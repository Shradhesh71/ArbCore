use orderbook::Level;
use rust_decimal::Decimal;
use serde::Deserialize;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::StreamExt;
use tracing::{debug, error, info};

use crate::{error::AdapterError, exchange::ExchangeAdapter, types::OrderbookUpdate};

#[derive(Debug, Clone)]
pub struct BinanceSpotAdapter {
    rest_base: String,
    ws_base: String,
    client: reqwest::Client,
}

impl BinanceSpotAdapter {
    pub fn new() -> Self { 
        Self { 
            rest_base: "https://api.binance.com".to_string(),
            ws_base: "wss://stream.binance.com:9443/stream".to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn depth_url(&self, symbol: &str, limit: u32) -> String {
        format!("{}/api/v3/depth?symbol={}&limit={}",self.rest_base,symbol.to_uppercase(),limit)
    }

    fn ws_depth_url(&self, symbol: &str) -> String{
        format!("{}?streams={}@depth@100ms", self.ws_base, symbol.to_lowercase())
    }
}

//  rest depth 
#[derive(Debug, Deserialize)]
struct BinanceDepthResponse {
    #[serde(rename = "lastUpdateId")]
    last_update_id: u64,
    bids: Vec<[String; 2]>, // [price, qty]
    asks: Vec<[String; 2]>,
}

/// WebSocket depth event (partial or diff)
#[derive(Debug, Deserialize)]
struct BinanceDepthEvent {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "u")]
    final_update_id: u64,
    #[serde(rename = "b")]
    bids: Vec<[String; 2]>,
    #[serde(rename = "a")]
    asks: Vec<[String; 2]>,
}

/// WebSocket combined stream wrapper
#[derive(Debug, Deserialize)]
struct BinanceStreamWrapper {
    #[allow(dead_code)]
    stream: String,
    data: BinanceDepthEvent,
}


fn parse_level(p: &str, q: &str) -> Result<Level, AdapterError> {
    let price = p.parse::<Decimal>()
        .map_err(|e| AdapterError::InvalidData(format!("bad price {}: {}", p, e)))?;
    let size = q.parse::<Decimal>()
        .map_err(|e| AdapterError::InvalidData(format!("bad qty {}: {}", q, e)))?;
    Ok(Level { price, size })
}

fn to_update_from_depth(symbol: &str, d: &BinanceDepthResponse) -> Result<OrderbookUpdate, AdapterError> {
    let bids = d.bids
        .iter()
        .map(|arr| parse_level(&arr[0], &arr[1]))
        .collect::<Result<Vec<_>, _>>()?;

    let asks = d.asks
        .iter()
        .map(|arr| parse_level(&arr[0], &arr[1]))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OrderbookUpdate {
        symbol: symbol.to_uppercase(),
        bids,
        asks,
        sequence: Some(d.last_update_id),
        is_snapshot: true,
    })
}


fn to_update_from_event(e: &BinanceDepthEvent) -> Result<OrderbookUpdate, AdapterError> {
    let bids = e.bids
        .iter()
        .map(|arr| parse_level(&arr[0], &arr[1]))
        .collect::<Result<Vec<_>, _>>()?;

    let asks = e.asks
        .iter()
        .map(|arr| parse_level(&arr[0], &arr[1]))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OrderbookUpdate {
        symbol: e.symbol.to_uppercase(),
        bids,
        asks,
        sequence: Some(e.final_update_id),
        is_snapshot: false,
    })
}

#[async_trait]
impl ExchangeAdapter for BinanceSpotAdapter {
    async fn get_snapshot(&self, symbol: &str) -> Result<OrderbookUpdate, AdapterError> {

        let url = self.depth_url(symbol, 100);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let body: BinanceDepthResponse = resp.json().await?;
        to_update_from_depth(symbol, &body)
    }

    async fn connect_ws(
        &self,
        symbols: Vec<String>
    ) -> Result<mpsc::UnboundedReceiver<OrderbookUpdate>, AdapterError>{
        // Central channel that marketdata manager will consume
        let (tx, rx) = mpsc::unbounded_channel::<OrderbookUpdate>();

        for sym in symbols {
            let url = self.ws_depth_url(&sym);
            let tx_clone = tx.clone();
            let sym_clone = sym.clone();

            tokio::spawn(async move {
                let mut backoff_ms = 100u64;

                loop {
                    match connect_async(&url).await {
                        Ok((ws, _)) => {
                            info!("Binance ws connected: {} for {}", url, sym_clone);
                            backoff_ms = 100;
                            let (_write, mut read) = ws.split();

                            while let Some(msg) = read.next().await {
                                match msg {
                                    Ok(Message::Text(txt)) => {
                                        // Try parsing as combined stream wrapper first
                                        match serde_json::from_str::<BinanceStreamWrapper>(&txt) {
                                            Ok(wrapper) => {
                                                match to_update_from_event(&wrapper.data) {
                                                    Ok(update) => {
                                                        if let Err(e) = tx_clone.send(update) {
                                                            error!("Binance ws send error for {}: {:?}", sym_clone, e);
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        debug!("Binance depth parse error {}: {:?}", sym_clone, e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                debug!("Binance ws json error {} (trying direct parse): {:?}", sym_clone, e);
                                                // Fallback: try direct depth event parse (single stream format)
                                                match serde_json::from_str::<BinanceDepthEvent>(&txt) {
                                                    Ok(evt) => {
                                                        match to_update_from_event(&evt) {
                                                            Ok(update) => {
                                                                if let Err(e) = tx_clone.send(update) {
                                                                    error!("Binance ws send error for {}: {:?}", sym_clone, e);
                                                                    break;
                                                                }
                                                            }
                                                            Err(e) => {
                                                                debug!("Binance depth parse error {}: {:?}", sym_clone, e);
                                                            }
                                                        }
                                                    }
                                                    Err(e2) => {
                                                        debug!("Binance ws parse failed both formats {}: {:?}", sym_clone, e2);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Ok(Message::Ping(_)) => {
                                        // ignore, tungstenite auto-pongs if needed
                                    }
                                    Ok(Message::Close(frame)) => {
                                        info!("Binance ws closed for {}: {:?}", sym_clone, frame);
                                        break;
                                    }
                                    Err(e) => {
                                        error!("Binance ws error for {}: {:?}", sym_clone, e);
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(e) => {
                            error!("Binance ws connect error {}: {:?}", url, e);
                        }
                    }

                    // backoff + reconnect
                    let jitter: u64 = rand::random::<u64>() % 100;
                    let wait = std::time::Duration::from_millis(backoff_ms + jitter);
                    tokio::time::sleep(wait).await;
                    backoff_ms = (backoff_ms * 2).min(10_000);
                    info!("Binance ws reconnecting {} ...", sym_clone);
                }
            });
        }

        Ok(rx)
    }

    fn clone_box(&self) -> Box<dyn ExchangeAdapter> {
        Box::new(self.clone())
    }
}
