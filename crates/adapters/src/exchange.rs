use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{error::AdapterError, types::OrderbookUpdate};

#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    /// Fetch a REST snapshot for a symbol (full depth or top N, depending on exchange).
    async fn get_snapshot(&self, symbol: &str) -> Result<OrderbookUpdate, AdapterError>;

    /// Open WebSocket stream for a list of symbols and return a receiver of normalized updates.
    ///
    /// Implementation detail:
    /// - Typically spawn one WS per symbol OR a combined stream (Binance supports multi-stream).
    /// - Inside those tasks, forward updates into this returned channel.
    async fn connect_ws(
        &self,
        symbols: Vec<String>,
    ) -> Result<mpsc::UnboundedReceiver<OrderbookUpdate>, AdapterError>;

    /// Clone into a boxed trait object (used in marketdata wiring).
    fn clone_box(&self) -> Box<dyn ExchangeAdapter>;
}

// allow Box<dyn ExchangeAdapter> to be cloned using clone_box.
impl Clone for Box<dyn ExchangeAdapter> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}