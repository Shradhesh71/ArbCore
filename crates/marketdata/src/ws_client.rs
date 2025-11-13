// crates/marketdata/src/ws_client.rs
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, error, info};

/// Run a websocket URL with automatic reconnect and call `on_text` for each incoming text message.
/// `on_text` is an async closure that accepts the message payload string.
pub async fn run_ws_with_reconnect<F, Fut>(url: &str, mut on_text: F)
where
    F: FnMut(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let mut backoff_ms = 100u64;
    loop {
        match connect_async(url).await {
            Ok((ws_stream, _resp)) => {
                info!("ws connected: {}", url);
                backoff_ms = 100; // reset backoff on success
                let (mut write, mut read) = ws_stream.split();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(Message::Text(txt)) => {
                            on_text(txt.to_string()).await;
                        }
                        Ok(Message::Binary(_)) => {
                            // ignore binary by default, adapters can handle if needed
                        }
                        Ok(Message::Ping(payload)) => {
                            // reply with pong
                            let _ = write.send(Message::Pong(payload)).await;
                        }
                        Ok(Message::Pong(_)) => {
                            // keepalive
                        }
                        Ok(Message::Close(frame)) => {
                            debug!("ws closed: {:?} - reconnecting", frame);
                            break;
                        }
                        Err(e) => {
                            error!("ws read error: {:?} - reconnecting", e);
                            break;
                        }
                        _ => {}
                    }
                }
                // loop will attempt reconnect
            }
            Err(e) => {
                error!("ws connect error to {}: {:?}", url, e);
            }
        }

        // reconnect backoff with jitter
        let jitter: u64 = rand::random::<u64>() % 100;
        let wait = Duration::from_millis(backoff_ms + jitter);
        info!("reconnecting in {:?} ...", wait);
        tokio::time::sleep(wait).await;
        backoff_ms = (backoff_ms * 2).min(10_000);
    }
}
