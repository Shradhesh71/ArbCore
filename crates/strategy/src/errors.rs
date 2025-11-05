use thiserror::Error;

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("io error reading config: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("quantity rounded to zero or below for leg {0}")]
    QtyRoundedZero(String),

    #[error("min_notional not satisfied for symbol {0}: notional {1} < min_notional {2}")]
    MinNotional(String, rust_decimal::Decimal, rust_decimal::Decimal),

    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, StrategyError>;