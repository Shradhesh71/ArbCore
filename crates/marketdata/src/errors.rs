use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarketDataError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("ws error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("other: {0}")]
    Other(String),
}