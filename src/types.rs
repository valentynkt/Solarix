use serde::{Deserialize, Serialize};

/// A decoded Solana instruction with its arguments as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedInstruction {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub instruction_name: String,
    pub args: serde_json::Value,
    pub program_id: String,
    pub accounts: Vec<String>,
    pub instruction_index: u8,
    pub inner_index: Option<u8>,
}

impl DecodedInstruction {
    /// Create a minimal DecodedInstruction for decoder output (before pipeline enrichment).
    pub fn from_decoded(
        program_id: String,
        instruction_name: String,
        args: serde_json::Value,
    ) -> Self {
        Self {
            signature: String::new(),
            slot: 0,
            block_time: None,
            instruction_name,
            args,
            program_id,
            accounts: Vec::new(),
            instruction_index: 0,
            inner_index: None,
        }
    }
}

/// A decoded Solana account state as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedAccount {
    pub pubkey: String,
    pub slot_updated: u64,
    pub lamports: u64,
    pub data: serde_json::Value,
    pub account_type: String,
    pub program_id: String,
}

impl DecodedAccount {
    /// Create a minimal DecodedAccount for decoder output (before pipeline enrichment).
    pub fn from_decoded(
        program_id: String,
        account_type: String,
        pubkey: String,
        data: serde_json::Value,
    ) -> Self {
        Self {
            pubkey,
            slot_updated: 0,
            lamports: 0,
            data,
            account_type,
            program_id,
        }
    }
}

/// A block of data fetched from the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockData {
    pub slot: u64,
    pub block_time: Option<i64>,
    pub transactions: Vec<TransactionData>,
}

/// A single transaction within a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionData {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub instructions: Vec<DecodedInstruction>,
}
