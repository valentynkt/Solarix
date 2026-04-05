pub mod rpc;
pub mod ws;

use crate::decoder::DecodeError;
use crate::idl::IdlError;
use crate::storage::StorageError;

/// Pipeline orchestrator: manages the indexing state machine.
///
/// States: Initializing -> Backfilling <-> CatchingUp -> Streaming -> ShuttingDown
pub struct PipelineOrchestrator;

/// Errors that can occur during pipeline operations.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("RPC call failed: {0}")]
    RpcFailed(String),

    #[error("WebSocket disconnected: {0}")]
    WebSocketDisconnect(String),

    #[error("rate limited")]
    RateLimited,

    #[error("decode error: {0}")]
    Decode(#[from] DecodeError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("IDL error: {0}")]
    Idl(#[from] IdlError),

    #[error("slot skipped: {0}")]
    SlotSkipped(String),

    #[error("fatal: {0}")]
    Fatal(String),
}

impl PipelineError {
    /// Whether this error is transient and the operation should be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RpcFailed(_)
                | Self::WebSocketDisconnect(_)
                | Self::RateLimited
                | Self::Idl(IdlError::FetchFailed { .. })
        )
    }
}
