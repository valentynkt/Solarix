//! Shared data types passed between pipeline stages.

use serde::{Deserialize, Serialize};

/// A decoded Solana instruction with its arguments as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedInstruction {
    /// Transaction signature (base58).
    pub signature: String,
    /// Slot this instruction was confirmed in.
    pub slot: u64,
    /// Unix timestamp of the block, if available.
    pub block_time: Option<i64>,
    /// IDL instruction name (e.g. `"route"`, `"swap"`).
    pub instruction_name: String,
    /// Decoded instruction arguments as a JSON object.
    pub args: serde_json::Value,
    /// Program ID that owns this instruction.
    pub program_id: String,
    /// Ordered list of account addresses referenced by the instruction.
    pub accounts: Vec<String>,
    /// Position of this instruction within the transaction (0-based).
    pub instruction_index: u8,
    /// Position within an inner instruction group; `None` for top-level.
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
    /// Account public key (base58).
    pub pubkey: String,
    /// Slot in which this account state was last observed.
    pub slot_updated: u64,
    /// Account balance in lamports.
    pub lamports: u64,
    /// Decoded account fields as a JSON object.
    pub data: serde_json::Value,
    /// IDL account type name (e.g. `"TokenLedger"`).
    pub account_type: String,
    /// Program ID that owns this account.
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
    /// Slot number for this block.
    pub slot: u64,
    /// Unix timestamp of the block, if available.
    pub block_time: Option<i64>,
    /// Transactions confirmed in this block.
    pub transactions: Vec<TransactionData>,
}

/// A single transaction within a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionData {
    /// Transaction signature (base58).
    pub signature: String,
    /// Slot this transaction was confirmed in.
    pub slot: u64,
    /// True if the transaction succeeded on-chain.
    pub success: bool,
    /// Decoded instructions contained in this transaction.
    pub instructions: Vec<DecodedInstruction>,
}
