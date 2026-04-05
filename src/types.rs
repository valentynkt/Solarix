use serde::{Deserialize, Serialize};

/// A decoded Solana instruction with its arguments as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedInstruction {
    pub program_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// A decoded Solana account state as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedAccount {
    pub program_id: String,
    pub account_type: String,
    pub pubkey: String,
    pub data: serde_json::Value,
}

/// A block of data fetched from the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockData {
    pub slot: u64,
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
