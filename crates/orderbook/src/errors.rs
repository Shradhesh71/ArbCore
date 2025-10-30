use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrderBookError {
    #[error("sequence mismatch: expected {expected}, got {got}")]
    SequenceError {
        expected: u64,
        got: u64,
    },

    #[error("insufficient orderbook depth for requested size")]
    InsufficientDepth,

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("resync required before applying deltas")]
    ResyncRequired,

    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, OrderBookError>;
