use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("db error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("invalid data: {0}")]
    Invalid(String),

    #[error("other: {0}")]
    Other(String),
}
