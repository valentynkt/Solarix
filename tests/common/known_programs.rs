// Well-known mainnet program IDs reused across Solarix integration tests
// (Story 6.5 AC8).
//
// Centralizing these constants prevents drift across `tests/idl_address_vectors.rs`
// (Story 6.4), `tests/mainnet_smoke.rs` (Story 6.5), and any future regression
// test that needs a real mainnet program ID. Existing test files are not
// refactored to consume these constants in this story — that's a follow-up
// housekeeping task explicitly excluded from AC8.
//
// `#![allow(dead_code)]` is required because Cargo compiles every `tests/*.rs`
// crate independently and the per-binary dead-code lint fires when an
// integration test binary doesn't pull every constant.

#![allow(dead_code)]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

/// SPL Token program. Source: Solana Program Library `spl-token` v3 mainnet
/// deployment.
pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Associated Token Account program. Source: Solana Program Library
/// `spl-associated-token-account` mainnet deployment.
pub const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// Meteora DLMM (Dynamic Liquidity Market Maker) — used as the Sprint-4 e2e
/// gate target and as the default `mainnet_smoke.rs` program. Source: Meteora
/// DLMM mainnet deployment.
pub const METEORA_DLMM_PROGRAM_ID: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

/// Marinade Finance liquid staking program. Source: Marinade Finance mainnet
/// deployment.
pub const MARINADE_FINANCE_PROGRAM_ID: &str = "MarBmsSgKXdrN1egZf5sqe1TMThczhMLJhJTMS7xuGS";

/// Jupiter Aggregator v6. Source: Jupiter Aggregator mainnet deployment.
pub const JUPITER_V6_PROGRAM_ID: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
