pub mod rpc;
pub mod ws;

use crate::decoder::DecodeError;
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
}
