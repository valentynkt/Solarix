pub mod filters;
pub mod handlers;

/// Errors that can occur in API request handling.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    #[error("program not found: {0}")]
    ProgramNotFound(String),

    #[error("query failed: {0}")]
    QueryFailed(String),
}
