//  use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub use orderbook::types::{
    Level, OrderbookSnapshot
};

/// Normalized orderbook update from an adapter.
/// This is intentionally generic and then mapped into `orderbook::types::*` in marketdata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookUpdate {
    pub symbol: String,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    /// Sequence id if exchange provides it (e.g., lastUpdateId / u)
    pub sequence: Option<u64>,
    /// Whether this update represents a *full snapshot* (REST / large reset)
    pub is_snapshot: bool,
}