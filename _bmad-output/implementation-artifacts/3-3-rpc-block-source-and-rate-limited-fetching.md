# Story 3.3: RPC Block Source & Rate-Limited Fetching

Status: done

## Story

As a system,
I want to fetch block data from Solana RPC with rate limiting and retry logic,
so that batch indexing respects public RPC limits and recovers gracefully from transient failures.

## Acceptance Criteria

1. **AC1: BlockSource trait (object-safe)**
   - **Given** the `BlockSource` trait in `pipeline/rpc.rs`
   - **When** I inspect it
   - **Then** it defines async methods: `get_blocks(start_slot, end_slot) -> Result<Vec<u64>>`, `get_block(slot) -> Result<Option<RpcBlock>>`, `get_slot() -> Result<u64>`
   - **And** the trait is object-safe (`#[async_trait]`) to enable mocking for tests
   - **And** `get_block` returns `Option<RpcBlock>` (not bare `RpcBlock`) to handle skipped/empty slots

2. **AC2: AccountSource trait (object-safe)**
   - **Given** the `AccountSource` trait in `pipeline/rpc.rs`
   - **When** I inspect it
   - **Then** it defines async methods: `get_program_accounts(program_id) -> Result<Vec<String>>` (pubkeys only via `dataSlice` trick), `get_multiple_accounts(pubkeys) -> Result<Vec<RpcAccountInfo>>` (batches of max 100)
   - **And** the trait is object-safe (`#[async_trait]`)

3. **AC3: RpcClient implementation with rate limiting**
   - **Given** the `RpcClient` struct implementing both `BlockSource` and `AccountSource`
   - **When** it makes any RPC call
   - **Then** every request includes `maxSupportedTransactionVersion: 0`
   - **And** block data requests use `encoding: "base64"` for bandwidth efficiency
   - **And** all requests pass through a `governor` rate limiter (default 10 RPS, configurable via `SOLARIX_RPC_RPS`)

4. **AC4: Retry with exponential backoff**
   - **Given** any RPC call that fails with a retryable error
   - **When** the retry logic fires
   - **Then** it uses `backon` `ExponentialBuilder` with `min_delay: 500ms`, `max_delay: 30s`, jitter enabled, configurable via `SOLARIX_RETRY_*` env vars
   - **And** total retry duration does not exceed `SOLARIX_RETRY_TIMEOUT_SECS` (default 300s)

5. **AC5: getBlocks chunking**
   - **Given** a `get_blocks` call for a range exceeding 500,000 slots
   - **When** the RPC client processes it
   - **Then** it automatically chunks into multiple `getBlocks` calls of max 500K each and concatenates results

6. **AC6: Skipped slot handling**
   - **Given** a `get_block` call that returns JSON-RPC error `-32009` (skipped slot)
   - **When** the RPC client processes it
   - **Then** the error is classified as permanent (not retried) and `Ok(None)` is returned

7. **AC7: Failed transaction filtering**
   - **Given** `SOLARIX_INDEX_FAILED_TXS` is `false` (default)
   - **When** a block is fetched
   - **Then** transactions where `meta.err != null` are filtered out before returning

8. **AC8: PipelineError enhancements**
   - **Given** the `PipelineError` enum in `pipeline/mod.rs`
   - **When** I inspect it
   - **Then** it includes a `Fatal(String)` variant
   - **And** it has an `is_retryable(&self) -> bool` method returning `true` for `RpcFailed`, `WebSocketDisconnect`, `RateLimited`

9. **AC9: RPC response types**
   - **Given** the RPC response types in `pipeline/rpc.rs`
   - **When** I inspect them
   - **Then** `RpcBlock` contains: `slot`, `block_time`, `transactions: Vec<RpcTransaction>`
   - **And** `RpcTransaction` contains: `signature`, `success`, `account_keys`, `instructions` (with raw data bytes), `inner_instructions`, `slot`
   - **And** `RpcAccountInfo` contains: `pubkey`, `data` (raw bytes decoded from base64), `lamports`, `owner`
   - **And** all types derive `Debug, Clone`

## Tasks / Subtasks

- [x] Task 1: Add dependencies to Cargo.toml (AC: #1, #2, #3)
  - [x] Add `async-trait = "0.1"` for object-safe async traits
  - [x] Add `base64 = "0.22"` for decoding RPC response data (already present)
  - [x] Verify `governor = "0.10"`, `backon = "1"`, `reqwest = "0.12"` already present
- [x] Task 2: Add `Fatal` variant and `is_retryable()` to PipelineError (AC: #8)
  - [x] Add `Fatal(String)` variant to `PipelineError` in `src/pipeline/mod.rs`
  - [x] Add `pub fn is_retryable(&self) -> bool` method
- [x] Task 3: Define RPC response types in `pipeline/rpc.rs` (AC: #9)
  - [x] Define JSON-RPC envelope types for request/response
  - [x] Define `RpcBlock`, `RpcTransaction`, `RpcInstruction`, `RpcInnerInstructionGroup`
  - [x] Define `RpcAccountInfo`
  - [x] Implement `Deserialize` on all response types to parse RPC JSON
- [x] Task 4: Rewrite `BlockSource` trait with `#[async_trait]` (AC: #1)
  - [x] Replace RPITIT stubs with `#[async_trait]` methods
  - [x] `get_blocks(&self, start_slot: u64, end_slot: u64) -> Result<Vec<u64>, PipelineError>`
  - [x] `get_block(&self, slot: u64) -> Result<Option<RpcBlock>, PipelineError>`
  - [x] `get_slot(&self) -> Result<u64, PipelineError>`
- [x] Task 5: Rewrite `AccountSource` trait with `#[async_trait]` (AC: #2)
  - [x] `get_program_accounts(&self, program_id: &str) -> Result<Vec<String>, PipelineError>`
  - [x] `get_multiple_accounts(&self, pubkeys: &[String]) -> Result<Vec<RpcAccountInfo>, PipelineError>`
- [x] Task 6: Implement `RpcClient` struct (AC: #3, #4, #5, #6, #7)
  - [x] Struct fields: `http: reqwest::Client`, `rpc_url: String`, `rate_limiter: governor::RateLimiter<...>`, retry config fields, `index_failed_txs: bool`
  - [x] Constructor `new(config: &Config) -> Result<Self, PipelineError>`
  - [x] Private helper `rpc_request(&self, method, params) -> Result<Value>` with rate limiting + retry
  - [x] Implement `BlockSource` for `RpcClient`
  - [x] Implement `AccountSource` for `RpcClient`
- [x] Task 7: Unit tests (AC: all)
  - [x] Test `is_retryable()` on all PipelineError variants
  - [x] Test getBlocks chunking logic (pure function, no network)
  - [x] Test skipped slot error classification
  - [x] Test failed transaction filtering logic
  - [x] Test RPC response deserialization from fixture JSON
- [x] Task 8: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` — unit tests pass

### Review Findings

- [x] [Review][Patch] `compute_block_chunks` infinite loop at u64::MAX [src/pipeline/rpc.rs:600-606] — fixed: break when `chunk_end >= end_slot`
- [x] [Review][Patch] Skipped-slot detection via string prefix is brittle [src/pipeline/rpc.rs:370] — fixed: added `PipelineError::SlotSkipped` variant
- [x] [Review][Patch] v0 transactions: accountKeys misses address lookup table entries [src/pipeline/rpc.rs:126-129] — fixed: append `loadedAddresses.writable` + `readonly` from meta
- [x] [Review][Patch] Permanent RPC errors (-32007, -32010) classified as retryable [src/pipeline/rpc.rs:579-582] — fixed: map to `SlotSkipped`
- [x] [Review][Patch] `notify` closure captures borrowed `method` instead of owned `method_owned` [src/pipeline/rpc.rs:321] — fixed: use `method_for_log` owned clone
- [x] [Review][Defer] `get_multiple_accounts` silently drops None accounts [src/pipeline/rpc.rs:431-439] — deferred, acceptable for MVP
- [x] [Review][Defer] Empty signatures produce empty-string signature [src/pipeline/rpc.rs:478] — deferred, real blocks always have signatures
- [x] [Review][Defer] `RawInstruction.data` not `#[serde(default)]` [src/pipeline/rpc.rs:136] — deferred, standard Solana RPC always provides the field

## Dev Notes

### Codebase State (Stories 1.1 + 1.2 merged, 1.3 in progress)

Existing code relevant to this story:

- `src/pipeline/rpc.rs` — stub `BlockSource` and `AccountSource` traits using RPITIT (not object-safe). **This file is completely rewritten.**
- `src/pipeline/mod.rs` — `PipelineError` enum with 5 variants, `PipelineOrchestrator` stub. **Modified to add `Fatal` variant + `is_retryable()`.**
- `src/config.rs` — `Config` struct with relevant fields: `rpc_url`, `rpc_rps` (u32, default 10), `index_failed_txs` (bool, default false), `retry_initial_ms` (u64, default 500), `retry_max_ms` (u64, default 30000), `retry_timeout_secs` (u64, default 300), `tx_encoding` (String, default "base64"), `backfill_chunk_size` (u64, default 50000)
- `src/types.rs` — `BlockData`, `TransactionData`, `DecodedInstruction`, `DecodedAccount`. **DO NOT MODIFY** — owned by Track B. Story 3.3 defines separate RPC-layer types.
- `Cargo.toml` — `reqwest = "0.12"`, `governor = "0.10"`, `backon = "1"` already present

### Deferred Work Items Addressed by This Story

From `_bmad-output/implementation-artifacts/deferred-work.md`:

1. **`fetch_block` on empty/skipped slot** — Solved: trait returns `Option<RpcBlock>`, skipped slots return `Ok(None)`
2. **`get_program_account_keys` unbounded `Vec<String>`** — Addressed: use `dataSlice: {offset: 0, length: 0}` to fetch pubkeys only (minimal memory per key). Document that large programs (millions of accounts) may still be memory-intensive.
3. **RPITIT traits not object-safe** — Solved: use `#[async_trait]` on `BlockSource` and `AccountSource`

### Cargo.toml Changes

Add to `[dependencies]`:

```toml
# Async trait for object-safe trait boundaries
async-trait = "0.1"
# Base64 decoding for RPC response data
base64 = "0.22"
```

All other required crates (`reqwest`, `governor`, `backon`, `serde`, `serde_json`, `thiserror`, `tracing`) are already present.

**DO NOT** uncomment `solana-rpc-client-api` — this story uses `reqwest` directly for JSON-RPC. The Solana RPC API is a simple JSON-RPC 2.0 protocol; a thin reqwest wrapper is sufficient and avoids pulling in the heavy Solana SDK dependency tree.

### PipelineError Changes (`src/pipeline/mod.rs`)

Add `Fatal` variant and `is_retryable()` method:

```rust
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

    #[error("fatal: {0}")]
    Fatal(String),
}

impl PipelineError {
    /// Whether this error is transient and the operation should be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RpcFailed(_) | Self::WebSocketDisconnect(_) | Self::RateLimited)
    }
}
```

### RPC Response Types

Define these in `pipeline/rpc.rs`. They map directly to the Solana JSON-RPC response format.

**JSON-RPC envelope:**

```rust
#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}
```

**Block types:**

```rust
/// Raw block from Solana RPC (before decoding instruction data).
#[derive(Debug, Clone)]
pub struct RpcBlock {
    pub slot: u64,
    pub block_time: Option<i64>,
    pub transactions: Vec<RpcTransaction>,
}

/// Raw transaction from Solana RPC.
#[derive(Debug, Clone)]
pub struct RpcTransaction {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub account_keys: Vec<String>,
    pub instructions: Vec<RpcInstruction>,
    pub inner_instructions: Vec<RpcInnerInstructionGroup>,
}

/// A single instruction within a transaction.
#[derive(Debug, Clone)]
pub struct RpcInstruction {
    pub program_id_index: u8,
    pub data: Vec<u8>,
    pub accounts: Vec<u8>,
}

/// Group of inner instructions generated by CPI.
#[derive(Debug, Clone)]
pub struct RpcInnerInstructionGroup {
    pub index: u8,
    pub instructions: Vec<RpcInstruction>,
}

/// Account info returned by getMultipleAccounts.
#[derive(Debug, Clone)]
pub struct RpcAccountInfo {
    pub pubkey: String,
    pub data: Vec<u8>,
    pub lamports: u64,
    pub owner: String,
}
```

**Deserialization:** These types are NOT directly `#[derive(Deserialize)]` on the public structs because the RPC JSON format differs from our internal representation (e.g., base64-encoded data needs decoding, instruction data is base58-encoded string). Instead, define private `Raw*` serde structs and convert in parsing functions. Example:

```rust
#[derive(Deserialize)]
struct RawGetBlockResult {
    #[serde(rename = "blockTime")]
    block_time: Option<i64>,
    transactions: Option<Vec<RawBlockTransaction>>,
}

#[derive(Deserialize)]
struct RawBlockTransaction {
    transaction: RawTransactionPayload,
    meta: Option<RawTransactionMeta>,
}

#[derive(Deserialize)]
struct RawTransactionPayload {
    // When encoding: "base64", this is [base64_string, "base64"]
    // When encoding: "json", this is the full JSON structure
    // For base64: we only need account_keys from the message
    message: RawMessage,
    signatures: Vec<String>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(rename = "accountKeys")]
    account_keys: Vec<String>,
    instructions: Vec<RawInstruction>,
}

#[derive(Deserialize)]
struct RawInstruction {
    #[serde(rename = "programIdIndex")]
    program_id_index: u8,
    data: String, // base58-encoded
    accounts: Vec<u8>,
}

#[derive(Deserialize)]
struct RawTransactionMeta {
    err: Option<serde_json::Value>,
    #[serde(rename = "innerInstructions")]
    inner_instructions: Option<Vec<RawInnerInstructionGroup>>,
}

#[derive(Deserialize)]
struct RawInnerInstructionGroup {
    index: u8,
    instructions: Vec<RawInstruction>,
}
```

**IMPORTANT:** Request blocks with `encoding: "jsonParsed"` or `encoding: "json"` (NOT `"base64"` for the whole response) because we need the parsed message structure (account keys, instruction indices) for filtering. Use `"json"` encoding — it gives us the structured message with base58-encoded instruction data, which we can decode. The `"base64"` encoding returns the entire transaction as a single base64 blob which requires Solana SDK to parse.

**Correction from AC:** The AC says `encoding: "base64"` but this refers to efficiency. For MVP, use `encoding: "json"` to get the structured message. The instruction `data` field will be base58-encoded strings. Decode instruction data from base58 to bytes for the decoder. If we later switch to `"base64"` encoding for bandwidth, we'll need the Solana SDK's transaction parser.

### RpcClient Implementation

```rust
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use governor::{Quota, RateLimiter as GovRateLimiter};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use reqwest::Client;
use tracing::{debug, warn};

use crate::config::Config;

type RateLimiter = GovRateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct RpcClient {
    http: Client,
    rpc_url: String,
    rate_limiter: Arc<RateLimiter>,
    index_failed_txs: bool,
    retry_min_delay: Duration,
    retry_max_delay: Duration,
    retry_timeout: Duration,
}

impl RpcClient {
    pub fn new(config: &Config) -> Result<Self, super::PipelineError> {
        let rps = NonZeroU32::new(config.rpc_rps)
            .ok_or_else(|| super::PipelineError::Fatal("rpc_rps must be > 0".into()))?;

        let rate_limiter = Arc::new(
            GovRateLimiter::direct(Quota::per_second(rps))
        );

        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| super::PipelineError::Fatal(format!("HTTP client init failed: {e}")))?;

        Ok(Self {
            http,
            rpc_url: config.rpc_url.clone(),
            rate_limiter,
            index_failed_txs: config.index_failed_txs,
            retry_min_delay: Duration::from_millis(config.retry_initial_ms),
            retry_max_delay: Duration::from_millis(config.retry_max_ms),
            retry_timeout: Duration::from_secs(config.retry_timeout_secs),
        })
    }
}
```

### Rate Limiting Pattern

Every RPC call goes through the rate limiter before making the HTTP request:

```rust
// Before each RPC call:
self.rate_limiter.until_ready().await;
```

`governor::RateLimiter::until_ready()` is an async method that resolves when a permit is available under the GCRA algorithm. No jitter is needed here because `backon` handles jitter in the retry layer.

### Retry Pattern (backon API)

The `backon` crate v1.x uses a closure-based `.retry()` on the `Retryable` trait:

```rust
use backon::ExponentialBuilder;
use backon::Retryable;

let result = (|| async {
    self.rate_limiter.until_ready().await;
    self.send_rpc_request(method, params).await
})
    .retry(
        ExponentialBuilder::default()
            .with_min_delay(self.retry_min_delay)
            .with_max_delay(self.retry_max_delay)
            .with_total_delay(Some(self.retry_timeout))
            .with_factor(2.0)
            .with_jitter()
    )
    .when(|e| e.is_retryable())
    .notify(|err, dur| {
        warn!(error = %err, delay = ?dur, method, "retrying RPC call");
    })
    .await;
```

**CRITICAL backon API notes:**

- Use `ExponentialBuilder`, NOT `ExponentialBackoffBuilder` (that's the old `backoff` crate)
- Use `.with_min_delay()`, NOT `.with_initial_interval()`
- Use `.with_max_delay()`, NOT `.with_max_interval()`
- Use `.with_total_delay(Some(...))` for total timeout, NOT `.with_max_elapsed_time()`
- Use `.with_factor(2.0)`, NOT `.with_multiplier()`
- Use `.with_jitter()` to enable randomization (boolean toggle, not a factor)
- The `.when()` closure receives `&PipelineError` and returns `bool` — only retries when `true`
- The `.sleep()` method is NOT needed when using tokio (backon auto-detects)
- The `.notify()` closure receives `(&Error, Duration)` for logging retries

### JSON-RPC Request Helper

```rust
/// Send a JSON-RPC 2.0 request with rate limiting.
async fn send_rpc_request(
    &self,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, super::PipelineError> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let resp = self.http
        .post(&self.rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                super::PipelineError::RpcFailed(format!("timeout: {e}"))
            } else if e.is_connect() {
                super::PipelineError::RpcFailed(format!("connection failed: {e}"))
            } else {
                super::PipelineError::RpcFailed(format!("request failed: {e}"))
            }
        })?;

    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(super::PipelineError::RateLimited);
    }
    if !status.is_success() {
        return Err(super::PipelineError::RpcFailed(
            format!("HTTP {status}")
        ));
    }

    let rpc_resp: JsonRpcResponse<serde_json::Value> = resp.json().await
        .map_err(|e| super::PipelineError::RpcFailed(format!("response parse failed: {e}")))?;

    if let Some(err) = rpc_resp.error {
        return Err(classify_rpc_error(err));
    }

    rpc_resp.result.ok_or_else(|| {
        super::PipelineError::RpcFailed("null result without error".into())
    })
}
```

### Error Classification

```rust
/// Classify JSON-RPC errors into retryable vs permanent.
fn classify_rpc_error(err: JsonRpcError) -> super::PipelineError {
    match err.code {
        -32009 => {
            // Slot was skipped — permanent for this slot, handled as Ok(None) by get_block
            super::PipelineError::RpcFailed(format!("slot skipped: {}", err.message))
        }
        -32005 => {
            // Node is behind — transient
            super::PipelineError::RpcFailed(format!("node behind: {}", err.message))
        }
        -32600 | -32601 | -32602 => {
            // Invalid request / method not found / invalid params — permanent
            super::PipelineError::Fatal(format!("RPC protocol error {}: {}", err.code, err.message))
        }
        _ => {
            // Unknown errors treated as transient (conservative)
            super::PipelineError::RpcFailed(format!("RPC error {}: {}", err.code, err.message))
        }
    }
}
```

The `get_block` method wraps this to convert `-32009` to `Ok(None)`:

```rust
async fn get_block(&self, slot: u64) -> Result<Option<RpcBlock>, PipelineError> {
    match self.fetch_block_raw(slot).await {
        Ok(value) => Ok(Some(parse_block(slot, value, self.index_failed_txs)?)),
        Err(PipelineError::RpcFailed(msg)) if msg.starts_with("slot skipped") => {
            debug!(slot, "slot skipped");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
```

### getBlocks Chunking

The Solana `getBlocks` RPC method has a hard limit of 500,000 slots per call:

```rust
const MAX_GET_BLOCKS_RANGE: u64 = 500_000;

async fn get_blocks(&self, start_slot: u64, end_slot: u64) -> Result<Vec<u64>, PipelineError> {
    let mut all_slots = Vec::new();
    let mut current = start_slot;

    while current <= end_slot {
        let chunk_end = std::cmp::min(current + MAX_GET_BLOCKS_RANGE - 1, end_slot);

        let params = serde_json::json!([current, chunk_end, {"commitment": "finalized"}]);
        let slots: Vec<u64> = // rpc_request_with_retry("getBlocks", params) + deserialize
        all_slots.extend(slots);
        current = chunk_end + 1;
    }

    Ok(all_slots)
}
```

### getProgramAccounts (dataSlice trick)

The `dataSlice: {offset: 0, length: 0}` trick fetches pubkeys only without downloading account data:

```rust
async fn get_program_accounts(&self, program_id: &str) -> Result<Vec<String>, PipelineError> {
    let params = serde_json::json!([
        program_id,
        {
            "encoding": "base64",
            "dataSlice": { "offset": 0, "length": 0 },
            "commitment": "finalized"
        }
    ]);

    let result = // rpc_request_with_retry("getProgramAccounts", params)
    // Parse: result is array of { pubkey: String, account: { ... } }
    // Extract just the pubkeys
}
```

### getMultipleAccounts (batched, max 100)

```rust
async fn get_multiple_accounts(&self, pubkeys: &[String]) -> Result<Vec<RpcAccountInfo>, PipelineError> {
    let mut all_accounts = Vec::new();

    for chunk in pubkeys.chunks(100) {
        let params = serde_json::json!([
            chunk,
            { "encoding": "base64", "commitment": "finalized" }
        ]);

        let result = // rpc_request_with_retry("getMultipleAccounts", params)
        // Parse: result.value is array of nullable account objects
        // Decode base64 data field to Vec<u8>
        // Pair with pubkeys from the chunk
    }

    Ok(all_accounts)
}
```

### Solana RPC Constraints (MUST follow)

1. **ALWAYS include `maxSupportedTransactionVersion: 0`** in `getBlock` params — or v0 transactions are silently dropped
2. **Use `commitment: "finalized"`** for all calls — `"processed"` is NOT supported by `getBlock`/`getBlocks`
3. **`getBlocks` max range is 500,000 slots** — auto-chunk larger ranges
4. **`getMultipleAccounts` max 100 pubkeys per call** — auto-batch larger sets
5. **`getProgramAccounts` has no pagination** — use `dataSlice: {offset: 0, length: 0}` for pubkeys-only fetch
6. **Skipped slot error `-32009`** is permanent (not retried), return `Ok(None)`
7. **HTTP 429** from rate limiting must be retried with backoff

### File Structure

All code for this story goes in **two files only**:

| File                  | Action  | Purpose                                                 |
| --------------------- | ------- | ------------------------------------------------------- |
| `src/pipeline/rpc.rs` | Rewrite | Traits + RpcClient impl + RPC types + response parsing  |
| `src/pipeline/mod.rs` | Modify  | Add `Fatal` variant + `is_retryable()` to PipelineError |
| `Cargo.toml`          | Modify  | Add `async-trait`, `base64`                             |

**DO NOT** modify: `src/types.rs`, `src/config.rs`, `src/decoder/mod.rs`, `src/storage/`, `src/api/`, `src/main.rs`

### Import Ordering (pipeline/rpc.rs)

```rust
// std library
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

// external crates
use async_trait::async_trait;
use backon::ExponentialBuilder;
use backon::Retryable;
use governor::{Quota, RateLimiter as GovRateLimiter};
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, warn};

// internal crate
use crate::config::Config;
use super::PipelineError;
```

### Testing Strategy

Unit tests in `#[cfg(test)] mod tests` at the bottom of `pipeline/rpc.rs`:

1. **`test_is_retryable`** — verify each PipelineError variant's retryable classification
2. **`test_get_blocks_chunking`** — extract the chunking logic into a pure function `compute_block_chunks(start, end) -> Vec<(u64, u64)>` and test edge cases: exact 500K boundary, single slot, range smaller than 500K, range exactly at multiples
3. **`test_classify_rpc_error`** — verify error codes map to correct PipelineError variants
4. **`test_filter_failed_transactions`** — given a mock block with mixed success/failed txs, verify filtering behavior with `index_failed_txs = false` and `true`
5. **`test_parse_block_response`** — deserialize a fixture JSON blob (from `tests/fixtures/rpc_get_block_response.json`) into `RpcBlock`
6. **`test_parse_get_blocks_response`** — deserialize slot array response
7. **`test_batch_pubkeys`** — verify `get_multiple_accounts` correctly chunks pubkeys into groups of 100

Create fixture file `tests/fixtures/rpc_get_block_response.json` with a realistic Solana block response (can be obtained from any public RPC). Keep it small — just 2-3 transactions.

**No integration tests** requiring network in this story. Network tests are deferred to Epic 6 (integration testing).

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` — use `?` with `map_err` to PipelineError
- NO `println!` — use `tracing` macros (`debug!`, `warn!`, `info!`)
- NO `anyhow` — use `thiserror` typed `PipelineError`
- NO `backoff` crate (unmaintained RUSTSEC-2025-0012) — use `backon`
- NO hardcoded RPC URLs — use `config.rpc_url`
- NO `solana-rpc-client-api` dependency — use `reqwest` directly
- NO modifying `src/types.rs` — owned by Track B (decoder)
- NO `ExponentialBackoffBuilder` — that's the old `backoff` crate API. Use `ExponentialBuilder` from `backon`
- NO `retry_notify()` — that's the old `backoff` API. Use `.retry().when().notify()` chain from `backon`
- DO NOT use `encoding: "base64"` for getBlock — use `encoding: "json"` to get structured message data. Pure base64 encoding returns an opaque blob requiring the Solana SDK to parse.

### Previous Story Learnings

From Story 1.1 review:

- `clippy::expect_used = "deny"` is active — cannot use `expect()` in production code
- Import ordering: std → external → internal (enforced by convention)
- `thiserror` v2 is the current stable version

From Story 1.2 review:

- `map_err(|e| { error!(...); e })?` pattern for logging + propagation (used in main.rs)
- `sqlx::raw_sql()` for DDL — not relevant here, but shows the pattern of using raw APIs vs abstractions

From deferred work:

- `rpc_rps = 0` is not validated at config level. This story MUST handle it: `NonZeroU32::new(config.rpc_rps).ok_or_else(...)` in the constructor

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-3-transaction-decoding-batch-indexing.md#Story 3.3]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Transport & Pipeline]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Error Handling Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Error Handling Flow]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md#Module Boundary Contracts]
- [Source: _bmad-output/planning-artifacts/research/agent-1d-solana-rpc-capabilities.md#getBlock]
- [Source: _bmad-output/planning-artifacts/research/agent-1d-solana-rpc-capabilities.md#getBlocks]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#Rate Limiting]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#Error Handling Classification]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md]
- [Source: _bmad-output/implementation-artifacts/1-1-project-scaffolding-and-configuration.md#Review Findings]
- [Source: docs.rs/backon/1.6.0 — ExponentialBuilder API]
- [Source: docs.rs/governor — RateLimiter::direct, Quota::per_second]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

No issues encountered during implementation.

### Completion Notes List

- Added `async-trait` dependency; `base64` was already present in Cargo.toml
- Added `Fatal(String)` variant and `is_retryable()` method to `PipelineError`
- Completely rewrote `src/pipeline/rpc.rs` with:
  - Object-safe `BlockSource` and `AccountSource` traits using `#[async_trait]`
  - `RpcBlock`, `RpcTransaction`, `RpcInstruction`, `RpcInnerInstructionGroup`, `RpcAccountInfo` public types
  - Private `Raw*` serde types for JSON-RPC response deserialization
  - `RpcClient` struct with `governor` rate limiter + `backon` exponential retry
  - `getBlocks` auto-chunking at 500K slot boundary
  - `getMultipleAccounts` auto-batching at 100 pubkeys
  - `getProgramAccounts` with `dataSlice` trick for pubkey-only fetch
  - Skipped slot detection (-32009) returning `Ok(None)`
  - Failed transaction filtering when `index_failed_txs = false`
  - Error classification (skipped slot, node behind, protocol errors as Fatal)
  - All RPC calls include `maxSupportedTransactionVersion: 0` and `commitment: "finalized"`
  - Uses `encoding: "json"` for getBlock (structured message with base58 instruction data)
- 20 unit tests covering: is_retryable, block chunking, error classification, tx filtering, response parsing, pubkey batching, base64 decoding
- All 48 tests pass, clippy clean, fmt clean

### Implementation Plan

Followed red-green-refactor: defined types and traits first, implemented RpcClient with all helpers, added comprehensive unit tests for pure logic (no network tests in this story).

### File List

- `Cargo.toml` — added `async-trait = "0.1"`
- `src/pipeline/mod.rs` — added `Fatal(String)` variant + `is_retryable()` to `PipelineError`
- `src/pipeline/rpc.rs` — complete rewrite: traits, types, RpcClient, parsing, tests

## Change Log

- 2026-04-05: Implemented RPC block source with rate-limited fetching (Story 3.3)
