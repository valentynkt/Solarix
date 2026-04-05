use crate::types::BlockData;

/// Trait for fetching blocks from Solana RPC.
pub trait BlockSource: Send + Sync {
    /// Fetch a block by slot number.
    fn fetch_block(
        &self,
        slot: u64,
    ) -> impl std::future::Future<Output = Result<BlockData, super::PipelineError>> + Send;
}

/// Trait for fetching account data from Solana RPC.
pub trait AccountSource: Send + Sync {
    /// Fetch all account pubkeys for a program.
    fn get_program_account_keys(
        &self,
        program_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<String>, super::PipelineError>> + Send;
}
