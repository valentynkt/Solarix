pub mod queries;
pub mod schema;
pub mod writer;

/// Errors that can occur during storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("DDL execution failed: {0}")]
    DdlFailed(String),

    #[error("write failed: {0}")]
    WriteFailed(String),

    #[error("checkpoint failed: {0}")]
    CheckpointFailed(String),
}

/// Initialize a database connection pool.
pub async fn init_pool(_database_url: &str) -> Result<(), StorageError> {
    Err(StorageError::ConnectionFailed(
        "pool init not yet implemented".to_string(),
    ))
}
