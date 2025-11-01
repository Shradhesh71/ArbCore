use thiserror::Error;

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("io error reading config: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("validation error: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, StrategyError>;