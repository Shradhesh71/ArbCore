use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::manager::{MarketDataManager, SymbolMessage, SnapshotFn};
use crate::errors::MarketDataError;
use crate::types::{Level};

use adapters::ExchangeAdapter; // your adapters crate (trait)
use adapters::OrderbookUpdate; // adapter-normalized type (from your adapter template)
use orderbook::types::{OrderbookSnapshot as OBSnapshot, OrderbookDelta as OBDelta}; // canonical types

/// Generic wiring for any adapter that implements `ExchangeAdapter`.
///
/// - `manager` is your MarketDataManager
/// - `adapter` is a boxed adapter implementing ExchangeAdapter
/// - `symbols` are the pair symbols (e.g., ["btcusdt", "ethbtc", "ethusdt"]) adapted to the adapter's naming
///
/// This function:
/// 1. registers a SnapshotFn that calls adapter.get_snapshot(symbol)
/// 2. starts the adapter websocket feed (adapter.connect_ws) and forwards OrderbookUpdate messages into manager
pub async fn wire_adapter_to_manager<A>(
    manager: Arc<MarketDataManager>, 
    adapter: A,
    symbols: Vec<String>,
) -> Result<Vec<JoinHandle<()>>, MarketDataError>
where
    A: ExchangeAdapter + Send + 'static,
{
    // 1) Register symbols in manager and attach snapshot provider closures
    for sym in symbols.iter() {
        // build snapshot closure that calls adapter.get_snapshot
        let adapter_clone = adapter.clone_box();
        let snapshot_fn: SnapshotFn = Arc::new(move |s: String| {
            let a = adapter_clone.clone_box();
            let requested = s.clone();
            Box::pin(async move {
                // adapter.get_snapshot returns adapter-specific OrderbookUpdate or OrderbookSnapshot; map it to canonical OBSnapshot
                let snap = a.get_snapshot(&requested).await.map_err(|e| MarketDataError::Other(format!("adapter get_snapshot error: {:?}", e)))?;
                // convert adapter snapshot -> orderbook::types::OrderbookSnapshot
                map_adapter_snapshot_to_canonical(&snap).map_err(|e| MarketDataError::Other(e))
            })
        });

        manager.ensure_symbol_with_snapshot_provider(sym, Some(snapshot_fn)).await?;
    }

    // 2) Connect to the websocket combined stream for all symbols (adapter.connect_ws)
    // adapter.connect_ws returns mpsc::UnboundedReceiver<OrderbookUpdate>
    let mut ws_rx = adapter.connect_ws(symbols.clone()).await.map_err(|e| MarketDataError::Other(format!("ws connect err: {:?}", e)))?;

    // 3) spawn a forwarder task to read adapter updates and forward to per-symbol manager channel
    // Return the joinhandle so caller can await/shutdown if needed.
    let manager_clone = manager.clone();
    let forwarder = tokio::spawn(async move {
        let mut update_count = 0u64;
        while let Some(update) = ws_rx.recv().await {
            update_count += 1;
            if update_count % 100 == 0 {
                println!("📊 Received {} updates from adapter", update_count);
            }
            
            // adapter's OrderbookUpdate likely contains symbol, bids, asks, sequence
            // Map it into canonical OrderbookDelta or a Snapshot message if adapter flagged it
            match map_adapter_update_to_message(&update) {
                Ok((sym, msg)) => {
                    if let Some(tx) = manager_clone.get_sender(&sym).await {
                        // send the SymbolMessage
                        if let Err(e) = tx.send(msg).await {
                            tracing::warn!("failed to send to symbol worker {}: {:?}", sym, e);
                        }
                    } else {
                        tracing::warn!("no symbol sender for {}, dropping update", sym);
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to map adapter update: {:?}", e);
                }
            }
        }
        tracing::info!("adapter ws forwarder ended (ws_rx closed)");
    });

    Ok(vec![forwarder])
}

/// Map the adapter's snapshot-like structure to your canonical OrderbookSnapshot (orderbook::types::OrderbookSnapshot).
/// Implement this according to the adapter's update structure. This is a simple example.
fn map_adapter_snapshot_to_canonical(adapter_snap: &OrderbookUpdate) -> Result<OBSnapshot, String> {
    // Example adapter snapshot fields: symbol, bids: Vec<(price_str, qty_str)>, asks: Vec<(price_str, qty_str)>, sequence
    let symbol = adapter_snap.symbol.clone();
    let bids = adapter_snap
        .bids
        .iter()
        .map(|lvl| Level { price: lvl.price, size: lvl.size })
        .collect();
    let asks = adapter_snap
        .asks
        .iter()
        .map(|lvl| Level { price: lvl.price, size: lvl.size })
        .collect();

    Ok(OBSnapshot {
        symbol,
        bids,
        asks,
        sequence: adapter_snap.sequence,
        ts: None,
    })
}

/// Map adapter `OrderbookUpdate` into `(symbol, SymbolMessage)` where SymbolMessage is either Snapshot or Delta.
/// This checks the `is_snapshot` flag from the adapter.
fn map_adapter_update_to_message(update: &OrderbookUpdate) -> Result<(String, SymbolMessage), String> {
    let symbol = update.symbol.clone();

    if update.is_snapshot {
        // Convert to Snapshot
        let snapshot = OBSnapshot {
            symbol: symbol.clone(),
            bids: update.bids.clone(),
            asks: update.asks.clone(),
            sequence: update.sequence,
            ts: None,
        };
        Ok((symbol, SymbolMessage::Snapshot(snapshot)))
    } else {
        // Convert to Delta
        let bids = update.bids.clone();
        let asks = update.asks.clone();

        let ob_delta = OBDelta {
            symbol: symbol.clone(),
            prev_sequence: update.sequence.map(|s| s.saturating_sub(1)),
            sequence: update.sequence,
            bids,
            asks,
            ts: None,
        };

        Ok((symbol, SymbolMessage::Delta(ob_delta)))
    }
}

//////////////////////////////
// Mock wiring example
//////////////////////////////

// TODO: Implement MockExchange in adapters crate first
/*
/// Example: wire MockExchange (which exposes an in-memory map of snapshots and a channel)
pub async fn wire_mock_to_manager(
    manager: Arc<MarketDataManager>,
    mock: adapters::MockExchange,
    symbols: Vec<String>,
) -> Result<Vec<JoinHandle<()>>, MarketDataError> {
    // register snapshot provider that calls mock.get_snapshot
    for sym in symbols.iter() {
        let mock_clone = mock.clone();
        let s = sym.clone();
        let snapshot_fn: SnapshotFn = Arc::new(move |requested: String| {
            let m = mock_clone.clone();
            Box::pin(async move {
                // Mock exposes get_snapshot which returns OrderbookUpdate or similar
                match m.get_snapshot(&requested).await {
                    Ok(u) => map_adapter_snapshot_to_canonical(&u).map_err(|e| MarketDataError::Other(e)),
                    Err(e) => Err(MarketDataError::Other(format!("mock get_snapshot err: {:?}", e))),
                }
            })
        });
        manager.ensure_symbol_with_snapshot_provider(&s, Some(snapshot_fn)).await?;
    }

    // For MockExchange, you may have a receiver to receive updates; assume mock.connect_ws returns receiver
    let mut rx = mock.connect_ws(symbols.clone()).await.map_err(|e| MarketDataError::Other(format!("mock ws err: {:?}", e)))?;

    // forward updates same as generic adapter
    let manager_clone = manager.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            match map_adapter_update_to_message(&update) {
                Ok((sym, msg)) => {
                    if let Some(tx) = manager_clone.get_sender(&sym).await {
                        if let Err(e) = tx.send(msg).await {
                            tracing::warn!("failed to send to symbol worker {}: {:?}", sym, e);
                        }
                    } else {
                        tracing::warn!("no symbol sender for {}, dropping update", sym);
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to map mock update: {:?}", e);
                }
            }
        }
    });

    Ok(vec![forwarder])
}
*/
