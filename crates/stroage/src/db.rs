use crate::errors::StorageError;
use crate::trade_repo::TradeRepo;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;

#[derive(Clone)]
pub struct Storage {
    pub pool: PgPool,
}

impl Storage {
    /// Create Storage from DATABASE_URL (Postgres)
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await?;

        Ok(Self { pool })
    }

    /// Get TradeRepo for working with trades/fills.
    pub fn trades(&self) -> TradeRepo {
        TradeRepo::new(self.pool.clone())
    }
}
