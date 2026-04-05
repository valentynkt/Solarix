pub mod fetch;

/// IDL manager: caches, parses, and provides IDL data for programs.
pub struct IdlManager;

/// Errors that can occur during IDL operations.
#[derive(Debug, thiserror::Error)]
pub enum IdlError {
    #[error("failed to fetch IDL: {0}")]
    FetchFailed(String),

    #[error("failed to parse IDL: {0}")]
    ParseFailed(String),

    #[error("IDL not found: {0}")]
    NotFound(String),

    #[error("unsupported IDL format: {0}")]
    UnsupportedFormat(String),
}
