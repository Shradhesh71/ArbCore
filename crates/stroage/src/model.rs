use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow};
use uuid::Uuid;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TradeFill {
    pub id: Uuid,
    pub plan_id: String,
    pub leg_idx: i32,
    pub symbol: String,
    pub side: String,
    pub qty: Decimal,
    pub price: Decimal,
    pub notional: Decimal,
    pub status: String,
    pub exchange: String,
    pub created_at: DateTime<Utc>
}