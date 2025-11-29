use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("ws error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid data: {0}")]
    InvalidData(String),

    #[error("other: {0}")]
    Other(String),
}
