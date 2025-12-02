use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

use crate::{errors::StorageError, model::TradeFill};


#[derive(Clone)]
pub struct TradeRepo {
    pool: PgPool,
}

impl TradeRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_fill(
        &self,
        plan_id: &str,
        leg_idx: i32,
        symbol: &str,
        side: &str,          // "buy" or "sell"
        qty: Decimal,
        price: Decimal,
        status: &str,        // "new", "partial", "filled", "cancelled", "rejected", "error"
        exchange: &str,
    ) -> Result<TradeFill, StorageError> {
        let notional = qty * price;
        let id = Uuid::new_v4();
        let created_at: DateTime<Utc> = Utc::now();

        let row = sqlx::query_as::<_, TradeFill>(
            r#"
            INSERT INTO trade_fills
                (id, plan_id, leg_idx, symbol, side, qty, price, notional, status, exchange, created_at)
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING
                id, plan_id, leg_idx, symbol, side, qty, price, notional, status, exchange, created_at
            "#
        )
        .bind(id)
        .bind(plan_id)
        .bind(leg_idx)
        .bind(symbol)
        .bind(side)
        .bind(qty)
        .bind(price)
        .bind(notional)
        .bind(status)
        .bind(exchange)
        .bind(created_at)
        .fetch_one(&self.pool)
        .await?;

        debug!("inserted trade fill {} for plan {} leg {}", row.id, plan_id, leg_idx);
        Ok(row)
    }



    pub async fn list_fills_for_plan(&self, plan_id: &str) -> Result<Vec<TradeFill>, StorageError> {
        let rows = sqlx::query_as::<_, TradeFill>(
            r#"
            SELECT
                id, plan_id, leg_idx, symbol, side, qty, price, notional, status, exchange, created_at
            FROM trade_fills
            WHERE plan_id = $1
            ORDER BY created_at ASC
            "#
        )
        .bind(plan_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn latest_fills(&self, limit: i64) -> Result<Vec<TradeFill>, StorageError> {
        let rows = sqlx::query_as::<_, TradeFill>(
            r#"
            SELECT
                id, plan_id, leg_idx, symbol, side, qty, price, notional, status, exchange, created_at
            FROM trade_fills
            ORDER BY created_at DESC
            LIMIT $1
            "#
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }
}