use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Side { 
    Bid, 
    Ask 
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderbookSnapshot {
    pub symbol: String,
    pub bids: Vec<Level>,   
    pub asks: Vec<Level>,   
    pub sequence: Option<u64>,
    pub ts: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderbookDelta {
    pub symbol: String,
    pub bids: Vec<Level>, // change list (price, size)
    pub asks: Vec<Level>,
    pub prev_sequence: Option<u64>,
    pub sequence: Option<u64>,
    pub ts: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EstimatedFill {
    /// Weighted average price for the filled quantity
    pub avg_price: Decimal,
    /// Total quantity filled (should equal requested if no error)
    pub total_qty: Decimal,
    /// Total notional (sum price * qty)
    pub total_notional: Decimal,
}

impl EstimatedFill {
    pub fn new(avg_price: Decimal, total_qty: Decimal, total_notional: Decimal) -> Self {
        Self { avg_price, total_qty, total_notional }
    }
}