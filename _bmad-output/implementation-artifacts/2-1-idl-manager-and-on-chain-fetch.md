# Story 2.1: IDL Manager & On-Chain Fetch

Status: review

## Story

As a user,
I want the system to fetch and parse an Anchor IDL from on-chain PDA given a program ID,
so that I can index any Anchor program without manually providing its IDL.

## Acceptance Criteria

1. **AC1: On-chain IDL fetch via legacy PDA**
   - **Given** a valid Anchor program ID with an on-chain IDL (v0.30+ format)
   - **When** the IdlManager receives a fetch request for that program ID
   - **Then** it derives the IDL PDA address using `["anchor:idl", program_id]` seeds
   - **And** it fetches the account data via `getAccountInfo` RPC call
   - **And** it parses the account data layout: `[authority: 32 bytes][data_len: 4 bytes LE][zlib_compressed_json: N bytes]`, skipping the 8-byte discriminator prefix before reading authority
   - **And** it decompresses the zlib payload and parses as JSON
   - **And** it validates the IDL has a `metadata.spec` field confirming v0.30+ format
   - **And** it caches the parsed IDL in an internal `HashMap<String, Idl>` keyed by program ID
   - **And** it returns the parsed `anchor_lang_idl_spec::Idl` struct

2. **AC2: Bundled IDL fallback**
   - **Given** the on-chain IDL account does not exist for the program ID
   - **When** the IdlManager attempts to fetch
   - **Then** it falls back to the bundled `idls/` directory, searching for a matching IDL file by program ID in the filename
   - **And** if no bundled IDL is found, it returns `IdlError::NotFound` with a message suggesting manual upload

3. **AC3: Unsupported format rejection**
   - **Given** an IDL with an unsupported format (missing `metadata.spec` or legacy v0.29)
   - **When** the IdlManager parses it
   - **Then** it returns `IdlError::UnsupportedFormat` with a descriptive message

4. **AC4: IdlError enum**
   - **Given** the `IdlError` enum
   - **When** I inspect it
   - **Then** it includes variants: `FetchFailed`, `ParseFailed`, `NotFound`, `UnsupportedFormat`, `DecompressionFailed`
   - **And** it derives `thiserror::Error` with descriptive `#[error("...")]` messages

5. **AC5: Network failure handling**
   - **Given** a network failure during IDL fetch
   - **When** the RPC call fails
   - **Then** the error is classified as retryable and wrapped in `IdlError::FetchFailed` with the underlying error as source

6. **AC6: Cache behavior**
   - **Given** an IDL already cached for a program ID
   - **When** `get_idl()` is called again for the same program ID
   - **Then** the cached version is returned without an RPC call
   - **And** the cache is safe for concurrent readers (multiple pipeline tasks)

7. **AC7: IDL hash computation**
   - **Given** a parsed IDL
   - **When** the IdlManager stores it
   - **Then** it computes a SHA-256 hash of the deterministic JSON serialization (sorted keys)
   - **And** the hash is stored alongside the IDL for change detection

## Tasks / Subtasks

- [x] Task 1: Uncomment and configure Solana/IDL dependencies in Cargo.toml (AC: #1)
  - [x] Uncomment `anchor-lang-idl-spec = "0.1.0"`
  - [x] Add `flate2 = "1"` for zlib decompression
  - [x] Add `sha2 = "0.10"` for IDL hash computation
  - [x] Add `reqwest` JSON feature if not already enabled
  - [x] Add `base64 = "0.22"` for account data decoding
  - [x] Add `solana-pubkey = { version = "2", features = ["curve25519"] }` for `Pubkey` type and PDA derivation
  - [x] Verify `cargo build` compiles with new deps
- [x] Task 2: Expand `IdlError` enum in `src/idl/mod.rs` (AC: #4)
  - [x] Add `DecompressionFailed(String)` variant
  - [x] Add `#[error("IDL decompression failed: {0}")]` message
  - [x] Add `impl From<IdlError> for PipelineError` in `src/pipeline/mod.rs`
  - [x] Verify existing variants match AC4 spec
- [x] Task 3: Implement `IdlManager` struct in `src/idl/mod.rs` (AC: #1, #6, #7)
  - [x] Define `IdlManager` with fields: `cache: HashMap<String, CachedIdl>`, `rpc_url: String`, `http_client: reqwest::Client`, `bundled_idls_path: Option<PathBuf>`
  - [x] Define `CachedIdl` struct: `idl: Idl`, `hash: String`, `source: IdlSource`
  - [x] Define `IdlSource` enum: `OnChain`, `Bundled`, `Manual`
  - [x] Implement `IdlManager::new(rpc_url: String) -> Self`
  - [x] Implement `pub async fn get_idl(&mut self, program_id: &str) -> Result<&Idl, IdlError>` — cache-first, then fetch cascade
  - [x] Implement `fn validate_idl(value: &serde_json::Value) -> Result<(), IdlError>` — check `metadata.spec` field exists
  - [x] Implement `fn compute_idl_hash(idl_json: &str) -> String` — SHA-256 of sorted-key JSON
  - [x] Implement `pub fn get_cached(&self, program_id: &str) -> Option<&Idl>` — read-only cache access
- [x] Task 4: Implement on-chain fetch in `src/idl/fetch.rs` (AC: #1, #5)
  - [x] Implement `pub async fn fetch_idl_from_chain(client: &reqwest::Client, rpc_url: &str, program_id: &str) -> Result<String, IdlError>` returning raw IDL JSON
  - [x] Derive PDA: `Pubkey::find_program_address(&[b"anchor:idl"], &program_pubkey)`
  - [x] Build `getAccountInfo` JSON-RPC request body with `{"encoding": "base64", "commitment": "confirmed"}`
  - [x] Send via `reqwest` POST to `rpc_url`
  - [x] Parse JSON-RPC response, handle `result.value == null` (account doesn't exist) -> return `IdlError::NotFound`
  - [x] Base64-decode the account data
  - [x] Strip 8-byte discriminator, skip 32-byte authority, read 4-byte LE `data_len`
  - [x] Extract `data_len` bytes of zlib-compressed payload
  - [x] Decompress with `flate2::ZlibDecoder` -> return IDL JSON string
  - [x] On network failure, return `IdlError::FetchFailed` with reqwest error detail
- [x] Task 5: Implement bundled IDL fallback in `src/idl/fetch.rs` (AC: #2)
  - [x] Implement `pub fn fetch_idl_from_bundled(bundled_path: Option<&Path>, program_id: &str) -> Result<String, IdlError>`
  - [x] Search `idls/` directory at project root for files matching `{program_id}.json` pattern
  - [x] Read and return file contents as IDL JSON string
  - [x] If not found, return `IdlError::NotFound` with message: "IDL not found for program {program_id}. Upload manually via POST /api/programs"
  - [x] Create `idls/` directory at project root (initially empty, placeholder for bundled IDLs)
- [x] Task 6: Add unit tests in `src/idl/mod.rs` (AC: #3, #4, #6, #7)
  - [x] Test `validate_idl` with valid v0.30+ IDL JSON (has `metadata.spec`)
  - [x] Test `validate_idl` rejects IDL without `metadata.spec` (returns `UnsupportedFormat`)
  - [x] Test `compute_idl_hash` produces consistent output for same input
  - [x] Test `compute_idl_hash` produces different output for different inputs
  - [x] Test `IdlManager` cache: second `get_cached()` returns `Some` after first insert
  - [x] Add test fixture: `tests/fixtures/idls/simple_v030.json` — minimal valid v0.30+ IDL
- [x] Task 7: Add unit tests for fetch in `src/idl/fetch.rs` (AC: #1, #5)
  - [x] Test `decompress_idl_data` with known zlib-compressed IDL bytes
  - [x] Test PDA derivation produces expected address for known program ID
  - [x] Test response parsing with mock JSON-RPC response (null value = NotFound)
  - [x] Test response parsing with valid account data
- [x] Task 8: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` passes all unit tests (15 IDL tests + 12 other tests)

## Dev Notes

### Current Codebase State (Post Story 1.2)

Existing stubs to replace:

- `src/idl/mod.rs` — `IdlManager` empty struct + `IdlError` with 4 variants (add `DecompressionFailed`)
- `src/idl/fetch.rs` — `fetch_idl()` stub returning `NotFound`

Existing infrastructure to use:

- `src/config.rs` — `Config.rpc_url` for Solana RPC endpoint
- `src/storage/mod.rs` — `bootstrap_system_tables()` creates `programs` table (used later by story 2.2 for registration)
- `Cargo.toml` — Solana crates are commented out, ready to uncomment

### Solana Dependencies Note

The Cargo.toml has Solana crates commented out. For this story, you need:

- `anchor-lang-idl-spec = "0.1.0"` — the `Idl` struct type and all IDL type definitions
- `solana-sdk` or `solana-pubkey` — for `Pubkey` type and `find_program_address` (PDA derivation)
- Do NOT add `solana-rpc-client` — we use raw `reqwest` for RPC calls (simpler, avoids heavy dep tree)
- `reqwest` is already in Cargo.toml — ensure `json` feature is enabled

Check which exact Solana crate provides `Pubkey::find_program_address` in the v2/v3 ecosystem. The architecture spec says "Solana crates target v3.x ecosystem" but `anchor-lang-idl-spec` 0.1.0 may pin to v2.x. Resolve version compatibility before coding. Use `context7` MCP to check latest versions.

### On-Chain IDL Account Layout

The Anchor IDL PDA at `["anchor:idl", program_id]` has this binary layout:

```
[0..8]    - 8-byte discriminator (Anchor account discriminator)
[8..40]   - 32-byte authority pubkey
[40..44]  - 4-byte LE u32 data_len
[44..44+data_len] - zlib-compressed IDL JSON
```

The `getAccountInfo` RPC returns this as base64-encoded bytes. Decode steps:

1. Base64 decode the `result.value.data[0]` field (when encoding="base64")
2. Skip first 8 bytes (discriminator)
3. Skip next 32 bytes (authority)
4. Read 4 bytes as u32 LE = `data_len`
5. Read `data_len` bytes = compressed payload
6. Zlib inflate = IDL JSON string

### Version Detection

After parsing JSON, check format version:

```
if json["metadata"]["spec"].is_string() → v0.30+ (supported)
if json has "version" + "name" at top but no metadata.spec → v0.29 (unsupported, return error)
```

### IDL Hash Computation

For deterministic hashing:

1. Parse IDL JSON to `serde_json::Value`
2. Re-serialize with `serde_json::to_string()` (serde_json serializes object keys in insertion order, but `Idl` struct serialization via derive(Serialize) is deterministic by field order)
3. SHA-256 the bytes, hex-encode
4. Store as `idl_hash` (VARCHAR(64) in `programs` table, used by story 2.2)

### Error Handling Patterns (Follow Story 1.2)

```rust
// Module-level error enum in mod.rs (NOT separate error.rs)
#[derive(Debug, thiserror::Error)]
pub enum IdlError {
    #[error("failed to fetch IDL for {program_id}: {source}")]
    FetchFailed { program_id: String, source: String },

    #[error("failed to parse IDL: {0}")]
    ParseFailed(String),

    #[error("IDL not found for program {0}")]
    NotFound(String),

    #[error("unsupported IDL format: {0}")]
    UnsupportedFormat(String),

    #[error("IDL decompression failed: {0}")]
    DecompressionFailed(String),
}
```

Note: The existing `IdlError` uses `FetchFailed(String)` — you may restructure it to `FetchFailed { program_id: String, source: String }` for richer context, or keep the simple `(String)` form. Either is acceptable; just ensure `impl From<IdlError> for PipelineError` still works.

### RPC Request Format

Use raw `reqwest` HTTP POST (do NOT add `solana-rpc-client` dependency):

```rust
let body = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getAccountInfo",
    "params": [
        pda_address_base58,
        {
            "encoding": "base64",
            "commitment": "confirmed"
        }
    ]
});

let response = client.post(rpc_url)
    .json(&body)
    .send()
    .await
    .map_err(|e| IdlError::FetchFailed {
        program_id: program_id.to_string(),
        source: e.to_string(),
    })?;
```

Parse the response JSON:

- `result.value` is `null` → account doesn't exist → `IdlError::NotFound`
- `result.value.data` is `[base64_string, "base64"]` → decode and decompress
- `error` field present → `IdlError::FetchFailed`

### Bundled IDLs Directory

Create an `idls/` directory at project root. Initially empty. Files should be named `{program_id}.json` (base58 pubkey as filename). This allows the fallback to find IDLs by program ID without a registry file.

For the bounty demo, consider pre-bundling 1-2 IDLs (Jupiter v6, Raydium) to show the fallback path works. This is optional for this story.

### anchor-lang-idl-spec Crate Usage

```rust
use anchor_lang_idl_spec::Idl;

// Parse IDL JSON directly into the official Idl struct
let idl: Idl = serde_json::from_str(&idl_json)
    .map_err(|e| IdlError::ParseFailed(e.to_string()))?;

// Access fields
let program_name = &idl.metadata.name;
let spec_version = &idl.metadata.spec;
let instructions = &idl.instructions;
let accounts = &idl.accounts;
let types = &idl.types;
```

The `Idl` struct is the canonical type shared with decoder and schema generator. Do NOT create a custom `ParsedIdl` wrapper — use `Idl` directly.

### Import Ordering Convention

```rust
// std library
use std::collections::HashMap;
use std::path::PathBuf;

// external crates
use anchor_lang_idl_spec::Idl;
use reqwest::Client;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

// internal crate
use crate::config::Config;
```

### Files Created/Modified by This Story

| File                                   | Action | Purpose                                               |
| -------------------------------------- | ------ | ----------------------------------------------------- |
| `src/idl/mod.rs`                       | Modify | Full `IdlManager` implementation, expanded `IdlError` |
| `src/idl/fetch.rs`                     | Modify | On-chain fetch + bundled fallback implementations     |
| `src/pipeline/mod.rs`                  | Modify | Add `impl From<IdlError> for PipelineError`           |
| `Cargo.toml`                           | Modify | Uncomment/add Solana + compression deps               |
| `idls/`                                | Create | Empty directory for bundled IDL files                 |
| `tests/fixtures/idls/simple_v030.json` | Create | Test fixture: minimal valid v0.30+ IDL                |

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests
- NO `println!` — use `tracing` macros
- NO `solana-rpc-client` crate — use raw `reqwest` for `getAccountInfo`
- NO `anyhow` — use typed `IdlError`
- NO custom `ParsedIdl` wrapper struct — use `anchor_lang_idl_spec::Idl` directly
- NO separate `error.rs` file — `IdlError` stays in `src/idl/mod.rs`
- NO hardcoded program IDs in production code
- NO blocking calls on tokio runtime (all I/O is async)

### What This Story Does NOT Do

- Does NOT register programs in the `programs` DB table (that's story 2.2)
- Does NOT generate DDL/schema (that's story 2.3)
- Does NOT implement `ProgramRegistry` (that's story 2.2)
- Does NOT implement manual upload API endpoint (that's story 2.2)
- Does NOT decode instructions/accounts (that's epic 3)
- Does NOT implement Anchor v1.0 Program Metadata Program (PMP) fetch — deferred post-MVP

### Project Structure Notes

- `src/idl/mod.rs` owns `IdlManager` struct and `IdlError` enum
- `src/idl/fetch.rs` owns fetch functions (on-chain, bundled) — called by `IdlManager`
- Separation of concerns: `fetch.rs` acquires raw JSON, `mod.rs` parses/validates/caches
- `IdlManager` will later be wrapped by `ProgramRegistry` in story 2.2 (via `Arc<RwLock<ProgramRegistry>>`)

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-2-program-registration-idl-acquisition.md#Story 2.1]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md]
- [Source: _bmad-output/planning-artifacts/research/anchor-idl-type-spec-borsh-wire-format.md]
- [Source: _bmad-output/planning-artifacts/research/technical-solarix-universal-solana-indexer-research-2026-04-05.md]
- [Source: _bmad-output/implementation-artifacts/1-2-database-connection-and-system-table-bootstrap.md]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

- Used `solana-pubkey = { version = "2", features = ["curve25519"] }` instead of full `solana-sdk` — lightweight, provides `Pubkey::find_program_address`
- `thiserror` v2 treats field named `source` as `#[source]` — renamed to `reason` in `FetchFailed` variant
- `anchor-lang-idl-spec` 0.1.0 has no Solana SDK deps (only serde + anyhow), so `solana-pubkey` version is unconstrained
- Fixed parallel story 3-1's missing `find_account` function stub and test arity mismatch in `decoder/mod.rs`

### Completion Notes List

- Implemented full `IdlManager` with cache-first `get_idl()`, `validate_idl()`, `compute_idl_hash()`, and `insert_manual()` methods
- Implemented on-chain fetch via raw `reqwest` JSON-RPC (`getAccountInfo` with base64 encoding)
- Implemented `decompress_idl_data()` for the IDL account binary layout (8-byte disc + 32-byte authority + 4-byte LE len + zlib payload)
- Implemented bundled IDL fallback searching `idls/{program_id}.json`
- Created `IdlSource` enum (`OnChain`, `Bundled`, `Manual`) and `CachedIdl` struct
- Added `DecompressionFailed` variant to `IdlError`, added `From<IdlError> for PipelineError`
- 15 unit tests covering: validation, hashing, cache, decompression, PDA derivation, RPC response parsing, bundled fallback
- Created test fixture `tests/fixtures/idls/simple_v030.json`
- All checks pass: `cargo build`, `cargo clippy`, `cargo fmt -- --check`, `cargo test` (27 pass, 1 ignored)

### File List

- `Cargo.toml` — Modified: added `solana-pubkey`, `base64`, `flate2` deps
- `src/idl/mod.rs` — Modified: full `IdlManager`, `CachedIdl`, `IdlSource`, `validate_idl`, `compute_idl_hash`, expanded `IdlError` + unit tests
- `src/idl/fetch.rs` — Modified: `fetch_idl_from_chain`, `decompress_idl_data`, `fetch_idl_from_bundled` + unit tests
- `src/pipeline/mod.rs` — Modified: added `From<IdlError> for PipelineError`
- `src/decoder/mod.rs` — Modified: added missing `find_account` stub, fixed test arity (from parallel story 3-1)
- `idls/.gitkeep` — Created: empty bundled IDLs directory
- `tests/fixtures/idls/simple_v030.json` — Created: minimal v0.30+ IDL test fixture

### Review Findings

- [ ] [Review][Decision] D1: Cache not safe for concurrent readers (AC6) — `IdlManager` uses `HashMap` + `&mut self`, cannot be shared across async tasks. Add `Arc<RwLock<>>` here or defer to story 2.2?
- [ ] [Review][Decision] D2: Transient on-chain errors bypass bundled fallback — `FetchFailed` (timeout/429) skips bundled cascade, only `NotFound` triggers it
- [ ] [Review][Patch] P1: Unbounded zlib decompression (zip bomb) — no max output size [fetch.rs:145-149]
- [ ] [Review][Patch] P2: Path traversal in `fetch_idl_from_bundled` — `program_id` with `../` escapes `idls/` dir [fetch.rs:162]
- [ ] [Review][Patch] P3: `FetchFailed` not retryable in pipeline (AC5) — `is_retryable()` misses `Idl(FetchFailed{..})` [pipeline/mod.rs:40-45]
- [ ] [Review][Patch] P4: No HTTP status check before JSON parse — 429/503 yields opaque error [fetch.rs:44-53]
- [ ] [Review][Patch] P5: RPC `error: null` causes false FetchFailed — some RPCs set `"error": null` on success [fetch.rs:62-67]
- [ ] [Review][Patch] P6: Missing `result` key → `NotFound` instead of `FetchFailed` — masks malformed RPC [fetch.rs:70-76]
- [ ] [Review][Patch] P7: Dead code: unreachable `ok_or_else` after null guard [fetch.rs:78-81]
- [ ] [Review][Patch] P8: `bs58` unused dependency [Cargo.toml:46]
- [ ] [Review][Patch] P9: TOCTOU in `fetch_idl_from_bundled` — `exists()` then `read_to_string` race [fetch.rs:164-170]
- [ ] [Review][Patch] P10: `get_idl` double-lookup — `contains_key` then `get` is redundant [mod.rs:48-54]
- [x] [Review][Defer] W1: Relative path `"idls"` for bundled IDLs — CWD-dependent, breaks in Docker — deferred, config/deploy concern
- [x] [Review][Defer] W2: `solana-pubkey` v2 vs v3 ecosystem target — deferred, constrained by `anchor-lang-idl-spec` 0.1.0
- [x] [Review][Defer] W3: `FetchFailed` doesn't carry `#[source]` (AC5) — deferred, thiserror v2 reserves `source` field name
- [x] [Review][Defer] W4: `compute_idl_hash` key ordering depends on `serde_json` BTreeMap default — deferred, correct today
- [x] [Review][Defer] W5: `StorageError` retryability not classified — deferred, out of scope for this story

### Change Log

- 2026-04-05: Story 2.1 implemented — IDL Manager with on-chain fetch, bundled fallback, caching, validation, and hash computation

## Status

review
