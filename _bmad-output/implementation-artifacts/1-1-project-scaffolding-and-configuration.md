# Story 1.1: Project Scaffolding & Configuration

Status: done

## Story

As a developer,
I want a properly initialized Rust project with all dependencies, lints, formatting config, and a typed configuration system,
so that all subsequent development starts from a solid, consistent foundation.

## Acceptance Criteria

1. **AC1: Project compiles with all dependencies**
   - **Given** a fresh checkout of the repository
   - **When** I run `cargo build`
   - **Then** the project compiles successfully with all dependencies resolved
   - **And** `Cargo.toml` includes all non-Solana production dependencies listed in [Dependencies](#dependencies-cargotml)
   - **And** Solana crates (`chainparser`, `anchor-lang-idl-spec`, `solana-rpc-client-api`, `solana-pubsub-client`) are present but commented out (deferred to Epics 2/3 to avoid long compile times and version resolution issues during scaffolding)
   - **And** `Cargo.toml` lints section enforces: `unsafe_code = "forbid"`, `unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"`
   - **And** `rustfmt.toml` contains `edition = "2021"` and `max_width = 100`
   - **And** `clippy.toml` contains `allow-expect-in-tests = true`

2. **AC2: Linting and formatting pass cleanly**
   - **Given** the project is built
   - **When** I run `cargo clippy` and `cargo fmt -- --check`
   - **Then** both pass with zero warnings or errors

3. **AC3: Config struct with 22 env vars**
   - **Given** the `Config` struct in `src/config.rs`
   - **When** I inspect its fields
   - **Then** it derives `clap::Parser` with all 22 env vars listed in [Config Fields](#config-fields-22-env-vars)
   - **And** each field has a sensible default value and `env` attribute
   - **And** `.env.example` documents every configurable variable with descriptions

4. **AC4: Module structure with stubs**
   - **Given** `src/lib.rs`
   - **When** I inspect it
   - **Then** it declares all module paths: `config`, `types`, `idl`, `decoder`, `pipeline`, `storage`, `api`
   - **And** non-config/non-types modules contain minimal stub implementations (empty structs/trait placeholders) sufficient to compile

5. **AC5: Main entry point**
   - **Given** `src/main.rs`
   - **When** I inspect it
   - **Then** it parses `Config` via clap, initializes tracing subscriber, and exits cleanly
   - **And** it has a placeholder `tokio::main` async entry point

## Tasks / Subtasks

- [x] Task 1: Initialize project (AC: #1)
  - [x] Run `cargo init --name solarix` in project root
  - [x] Create `.gitignore` with Rust defaults + `.env` (do NOT ignore `Cargo.lock` — must be committed for binary crates)
  - [x] Create `rustfmt.toml` with `edition = "2021"`, `max_width = 100`
  - [x] Create `clippy.toml` with `allow-expect-in-tests = true`
- [x] Task 2: Configure Cargo.toml (AC: #1)
  - [x] Add all production dependencies per [Dependencies](#dependencies-cargotml)
  - [x] Add dev-dependencies: `proptest`, `axum-test`, `tokio` (test-util feature)
  - [x] Add `[lints]` section per [Lints](#lints-cargotml)
- [x] Task 3: Create Config struct (AC: #3)
  - [x] Create `src/config.rs` with `Config` struct deriving `clap::Parser`
  - [x] Add all 22 env var fields per [Config Fields](#config-fields-22-env-vars)
  - [x] Create `.env.example` documenting all 22 variables per [.env.example Content](#env-example-content)
- [x] Task 4: Create shared types (AC: #4)
  - [x] Create `src/types.rs` with `DecodedInstruction`, `DecodedAccount`, `BlockData`, `TransactionData` placeholder structs (each must derive `Debug, Clone, Serialize, Deserialize`)
- [x] Task 5: Create module stubs (AC: #4)
  - [x] Create `src/idl/mod.rs` with `IdlManager` stub struct + `IdlError` enum
  - [x] Create `src/idl/fetch.rs` with placeholder fetch function
  - [x] Create `src/decoder/mod.rs` with `SolarixDecoder` trait + `DecodeError` enum
  - [x] Create `src/pipeline/mod.rs` with `PipelineOrchestrator` stub + `PipelineError` enum
  - [x] Create `src/pipeline/rpc.rs` with `BlockSource` + `AccountSource` trait stubs
  - [x] Create `src/pipeline/ws.rs` with `TransactionStream` trait stub
  - [x] Create `src/storage/mod.rs` with pool init placeholder + `StorageError` enum
  - [x] Create `src/storage/schema.rs` with DDL generator placeholder
  - [x] Create `src/storage/writer.rs` with writer placeholder
  - [x] Create `src/storage/queries.rs` with query builder placeholder
  - [x] Create `src/api/mod.rs` with router placeholder + `ApiError` enum
  - [x] Create `src/api/handlers.rs` with handler stubs
  - [x] Create `src/api/filters.rs` with filter parsing placeholder
- [x] Task 6: Create lib.rs and main.rs (AC: #4, #5)
  - [x] Create `src/lib.rs` with `pub mod` declarations for all modules
  - [x] Create `src/main.rs` with clap parse, tracing init, tokio::main
- [x] Task 7: Verify (AC: #1, #2)
  - [x] Run `cargo build` -- must compile
  - [x] Run `cargo clippy` -- zero warnings
  - [x] Run `cargo fmt -- --check` -- passes

## Dev Notes

### Project Initialization

```bash
cargo init --name solarix
```

Single crate, no workspace. Binary crate with internal library modules.

### Dependencies (Cargo.toml)

```toml
[package]
name = "solarix"
version = "0.1.0"
edition = "2021"

[dependencies]
# API
axum = "0.8"
# Async runtime
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
# Database
sqlx = { version = "0.8", features = ["runtime-tokio", "tls-native-tls", "postgres", "json", "chrono"] }
# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
# Config
clap = { version = "4", features = ["derive", "env"] }
dotenvy = "0.15"
# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
# Error handling
thiserror = "2"
# HTTP client
reqwest = { version = "0.12", features = ["json"] }
# Rate limiting & retry
governor = "0.10"
backon = "1"
# Solana (types only for now -- actual usage in later stories)
# chainparser = { git = "https://github.com/valentynkit/chainparser", branch = "solarix-v3" }
# anchor-lang-idl-spec = "0.1.0"
# solana-rpc-client-api = "2"
# solana-pubsub-client = "2"

[dev-dependencies]
proptest = "1"
axum-test = "16"
tokio = { version = "1", features = ["test-util"] }

[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
```

**CRITICAL NOTES on dependencies:**

- `clap` MUST have `features = ["derive", "env"]` -- the `env` feature enables `#[arg(env = "...")]`
- `sqlx` MUST have `features = ["runtime-tokio", "postgres", "json"]` -- runtime feature required or panics at runtime
- `backon` NOT `backoff` -- backoff is unmaintained (RUSTSEC-2025-0012)
- Solana crates (`chainparser`, `anchor-lang-idl-spec`, `solana-rpc-client-api`, `solana-pubsub-client`) are commented out for Story 1.1 -- they will be uncommented in Epic 2/3 when actually needed. This avoids long compile times and potential version resolution issues during scaffolding.
- `thiserror` v2 is current stable -- NOT v1

### Config Fields (22 env vars)

```rust
use clap::Parser;

/// Solarix universal Solana indexer configuration.
#[derive(Parser, Debug, Clone)]
#[command(name = "solarix", about = "Universal Solana indexer")]
pub struct Config {
    // === Solana RPC ===
    #[arg(long, env = "SOLANA_RPC_URL", default_value = "https://api.mainnet-beta.solana.com")]
    pub rpc_url: String,

    #[arg(long, env = "SOLANA_WS_URL")]
    pub ws_url: Option<String>,  // derived from rpc_url if not set

    // === Database ===
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,  // required, no default

    #[arg(long, env = "SOLARIX_DB_POOL_MIN", default_value_t = 2)]
    pub db_pool_min: u32,

    #[arg(long, env = "SOLARIX_DB_POOL_MAX", default_value_t = 10)]
    pub db_pool_max: u32,

    // === Rate Limiting ===
    #[arg(long, env = "SOLARIX_RPC_RPS", default_value_t = 10)]
    pub rpc_rps: u32,

    // === Backfill ===
    #[arg(long, env = "SOLARIX_BACKFILL_CHUNK_SIZE", default_value_t = 50_000)]
    pub backfill_chunk_size: u64,

    #[arg(long, env = "SOLARIX_START_SLOT")]
    pub start_slot: Option<u64>,

    #[arg(long, env = "SOLARIX_END_SLOT")]
    pub end_slot: Option<u64>,

    // === Indexing ===
    #[arg(long, env = "SOLARIX_INDEX_FAILED_TXS", default_value_t = false)]
    pub index_failed_txs: bool,

    #[arg(long, env = "SOLARIX_TX_ENCODING", default_value = "base64")]
    pub tx_encoding: String,

    // === API ===
    #[arg(long, env = "SOLARIX_API_HOST", default_value = "0.0.0.0")]
    pub api_host: String,

    #[arg(long, env = "SOLARIX_API_PORT", default_value_t = 3000)]
    pub api_port: u16,

    #[arg(long, env = "SOLARIX_API_PAGE_SIZE", default_value_t = 50)]
    pub api_default_page_size: u32,

    #[arg(long, env = "SOLARIX_API_MAX_PAGE_SIZE", default_value_t = 1000)]
    pub api_max_page_size: u32,

    // === Pipeline ===
    #[arg(long, env = "SOLARIX_CHANNEL_CAPACITY", default_value_t = 256)]
    pub channel_capacity: usize,

    #[arg(long, env = "SOLARIX_CHECKPOINT_INTERVAL_SECS", default_value_t = 10)]
    pub checkpoint_interval_secs: u64,

    // === Retry ===
    #[arg(long, env = "SOLARIX_RETRY_INITIAL_MS", default_value_t = 500)]
    pub retry_initial_ms: u64,

    #[arg(long, env = "SOLARIX_RETRY_MAX_MS", default_value_t = 30_000)]
    pub retry_max_ms: u64,

    #[arg(long, env = "SOLARIX_RETRY_TIMEOUT_SECS", default_value_t = 300)]
    pub retry_timeout_secs: u64,

    // === Logging ===
    #[arg(long, env = "SOLARIX_LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    #[arg(long, env = "SOLARIX_LOG_FORMAT", default_value = "json")]
    pub log_format: String,
}
```

**Count: 22 fields.** `DATABASE_URL` has no default (required). `ws_url` and `start_slot`/`end_slot` are `Option<T>`.

### .env.example Content

```bash
# === Solana RPC ===
SOLANA_RPC_URL=https://api.mainnet-beta.solana.com
# SOLANA_WS_URL=wss://api.mainnet-beta.solana.com  # Optional, derived from RPC URL if unset

# === Database (required) ===
DATABASE_URL=postgres://solarix:solarix@localhost:5432/solarix

# === Database Pool ===
SOLARIX_DB_POOL_MIN=2
SOLARIX_DB_POOL_MAX=10

# === Rate Limiting ===
SOLARIX_RPC_RPS=10

# === Backfill ===
SOLARIX_BACKFILL_CHUNK_SIZE=50000
# SOLARIX_START_SLOT=       # Optional: slot to begin backfill from
# SOLARIX_END_SLOT=         # Optional: slot to stop backfill at

# === Indexing ===
SOLARIX_INDEX_FAILED_TXS=false
SOLARIX_TX_ENCODING=base64

# === API ===
SOLARIX_API_HOST=0.0.0.0
SOLARIX_API_PORT=3000
SOLARIX_API_PAGE_SIZE=50
SOLARIX_API_MAX_PAGE_SIZE=1000

# === Pipeline ===
SOLARIX_CHANNEL_CAPACITY=256
SOLARIX_CHECKPOINT_INTERVAL_SECS=10

# === Retry ===
SOLARIX_RETRY_INITIAL_MS=500
SOLARIX_RETRY_MAX_MS=30000
SOLARIX_RETRY_TIMEOUT_SECS=300

# === Logging ===
SOLARIX_LOG_LEVEL=info
SOLARIX_LOG_FORMAT=json   # "json" or "pretty"
```

### Module Stubs Pattern

Each module stub must:

1. Define the module's error enum with `#[derive(Debug, thiserror::Error)]`
2. Define placeholder structs/traits with `///` doc comments
3. Compile cleanly under strict clippy lints (no `unwrap`, `expect`, `panic`)

Follow this pattern for all module stubs. Example for `decoder/mod.rs`: define `DecodeError` enum with variants from the table below, then define `SolarixDecoder` trait with `decode_instruction(&self, program_id: &str, data: &[u8]) -> Result<serde_json::Value, DecodeError>` and `decode_account` with the same signature. Trait must be `Send + Sync`.

**Note on Solana types:** Trait stubs use `&str` for `program_id` and `&[u8]` for data. These will change to `Pubkey` and richer types when Solana crates are added in Epics 2/3. Do not over-engineer stubs to anticipate this.

### Error Enum Variants (all 5 modules)

| Module            | Enum            | Variants                                                                                                           |
| ----------------- | --------------- | ------------------------------------------------------------------------------------------------------------------ |
| `idl/mod.rs`      | `IdlError`      | `FetchFailed(String)`, `ParseFailed(String)`, `NotFound(String)`, `UnsupportedFormat(String)`                      |
| `decoder/mod.rs`  | `DecodeError`   | `UnknownDiscriminator(String)`, `DeserializationFailed(String)`, `IdlNotLoaded(String)`, `UnsupportedType(String)` |
| `pipeline/mod.rs` | `PipelineError` | `RpcFailed(String)`, `WebSocketDisconnect(String)`, `RateLimited`, `Decode(DecodeError)`, `Storage(StorageError)`  |
| `storage/mod.rs`  | `StorageError`  | `ConnectionFailed(String)`, `DdlFailed(String)`, `WriteFailed(String)`, `CheckpointFailed(String)`                 |
| `api/mod.rs`      | `ApiError`      | `InvalidFilter(String)`, `ProgramNotFound(String)`, `QueryFailed(String)`                                          |

`PipelineError` must include `From` conversions:

```rust
#[error("decode error: {0}")]
Decode(#[from] crate::decoder::DecodeError),

#[error("storage error: {0}")]
Storage(#[from] crate::storage::StorageError),
```

### main.rs Structure

```rust
use clap::Parser;
use tracing::info;

use solarix::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok(); // load .env, ignore if missing

    let config = Config::parse();

    // Initialize tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| config.log_level.clone().into());

    if config.log_format == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .init();
    }

    info!(
        rpc_url = %config.rpc_url,
        api_host = %config.api_host,
        api_port = config.api_port,
        "solarix starting"
    );

    // Future stories will add: DB pool, system tables, pipeline, API server
    Ok(())
}
```

**WARNING:** `main.rs` uses `unwrap_or_else` which is allowed (it's not `unwrap()`). The `Box<dyn Error>` return is acceptable for the binary entry point. Later stories will add proper error handling.

### Import Ordering Convention

```rust
// 1. std library
use std::collections::HashMap;

// 2. external crates
use axum::Router;
use serde::{Deserialize, Serialize};

// 3. internal crate
use crate::config::Config;
use crate::types::DecodedInstruction;
```

### Files Created by This Story

| File                     | Purpose                                                                              |
| ------------------------ | ------------------------------------------------------------------------------------ |
| `Cargo.toml`             | Package config, dependencies, lints                                                  |
| `rustfmt.toml`           | `edition = "2021"`, `max_width = 100`                                                |
| `clippy.toml`            | `allow-expect-in-tests = true`                                                       |
| `.gitignore`             | Rust defaults + `.env`                                                               |
| `.env.example`           | All 22 env vars documented                                                           |
| `src/main.rs`            | Entry point: clap parse, tracing init                                                |
| `src/lib.rs`             | `pub mod` declarations                                                               |
| `src/config.rs`          | `Config` struct with 22 `#[arg(env)]` fields                                         |
| `src/types.rs`           | Shared types: `DecodedInstruction`, `DecodedAccount`, `BlockData`, `TransactionData` |
| `src/idl/mod.rs`         | `IdlManager` stub + `IdlError`                                                       |
| `src/idl/fetch.rs`       | Fetch placeholder                                                                    |
| `src/decoder/mod.rs`     | `SolarixDecoder` trait + `DecodeError`                                               |
| `src/pipeline/mod.rs`    | `PipelineOrchestrator` stub + `PipelineError`                                        |
| `src/pipeline/rpc.rs`    | `BlockSource` + `AccountSource` traits                                               |
| `src/pipeline/ws.rs`     | `TransactionStream` trait                                                            |
| `src/storage/mod.rs`     | Pool init placeholder + `StorageError`                                               |
| `src/storage/schema.rs`  | DDL generator placeholder                                                            |
| `src/storage/writer.rs`  | Writer placeholder                                                                   |
| `src/storage/queries.rs` | Query builder placeholder                                                            |
| `src/api/mod.rs`         | Router placeholder + `ApiError`                                                      |
| `src/api/handlers.rs`    | Handler stubs                                                                        |
| `src/api/filters.rs`     | Filter parsing placeholder                                                           |

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` in any production code
- NO `println!` -- use `tracing` macros only
- NO `anyhow` -- use `thiserror` typed enums
- NO separate `error.rs` files -- error enums go in each module's `mod.rs`
- NO `sqlx::query!()` compile-time macros -- use runtime `sqlx::query()`
- NO nightly-only rustfmt options (`group_imports`, `imports_granularity`)
- The `env` feature on clap is REQUIRED for `#[arg(env = "...")]` to work

### Project Structure Notes

All file paths match the architecture document exactly. 14 source files in `src/`. No deviations.

### References

- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md#Complete Project Directory Structure]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Module Layout]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Crate Dependencies]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Error Handling Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/starter-template-evaluation.md#Initialization Command]
- [Source: _bmad-output/planning-artifacts/epics/epic-1-project-foundation-first-boot.md#Story 1.1]

### Review Findings

#### Decision Needed

- [x] [Review][Decision] **Solana crate versions: v2 vs v3** — RESOLVED: Fix now. Update commented crates to v3. Zero-risk comment change.
- [x] [Review][Decision] **RPITIT traits not object-safe** — RESOLVED: Defer. Stubs will be rewritten in Stories 3.3/4.1. Add object safety to those stories' ACs.

#### Patch

- [x] [Review][Patch] **`init_pool` returns `()` instead of pool handle** — Already fixed in working tree (returns `PgPool`).
- [x] [Review][Patch] **`log_format` case-sensitive with no validation** — Fixed: `eq_ignore_ascii_case("json")`. [src/main.rs:16]
- [x] [Review][Patch] **`channel_capacity = 0` causes tokio panic** — Fixed: added `parse_nonzero_usize` value parser. [src/config.rs:63-64]
- [x] [Review][Patch] **`QueryBuilder` name collides with `sqlx::QueryBuilder`** — Fixed: renamed to `DynamicQueryBuilder`. [src/storage/queries.rs:2]

#### Deferred

- [x] [Review][Defer] **Config cross-field validation missing** — `db_pool_min > db_pool_max`, `start_slot > end_slot`, `retry_initial_ms > retry_max_ms`, `api_default_page_size > api_max_page_size`, `rpc_rps = 0`, `backfill_chunk_size = 0` — all accepted without validation. Deferred: validation belongs in the story that first uses each field.
- [x] [Review][Defer] **`TransactionData.slot` duplicates `BlockData.slot`** — Redundancy invites inconsistency. Deferred: types will evolve when the pipeline is implemented (Story 3.4+).
- [x] [Review][Defer] **`ApiError` missing `IntoResponse` impl** — Architecture spec requires it. Acceptable stub gap; belongs in Story 5.1 (API endpoints).
- [x] [Review][Defer] **`subscribe()` returns `()` with no message channel** — The trait signature provides no way to receive messages. Deferred: will be redesigned in Story 4.1 (WebSocket streaming).
- [x] [Review][Defer] **`fetch_block` on empty/skipped slot** — Returns `BlockData` not `Option<BlockData>`. Solana produces empty slots returning `null`. Deferred: Story 3.3 (RPC block source).
- [x] [Review][Defer] **`get_program_account_keys` unbounded `Vec<String>`** — Large programs could OOM. Deferred: Story 3.3 (RPC client).
- [x] [Review][Defer] **`log_level` and `tx_encoding` are free-form strings** — Invalid values not rejected at startup. Deferred: can add enums when the fields are first consumed.
- [x] [Review][Defer] **`litesvm` absent from dev-dependencies** — Architecture spec lists it for pipeline integration tests. Deferred: add in Epic 3 when pipeline tests are written.
- [x] [Review][Defer] **`chainparser` git dep unpinned** — Uses branch tip, not tag/rev. Deferred: will be addressed when uncommented in Epic 2.

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

- `cargo build` — 192 crates compiled, zero errors
- `cargo clippy` — zero warnings
- `cargo fmt -- --check` — passes clean

### Completion Notes List

- Initialized Rust binary crate with `cargo init --name solarix`
- Configured Cargo.toml with all 13 production deps (Solana crates commented out per AC1), 3 dev deps, and strict lints (unsafe_code forbid, unwrap/expect/panic deny)
- Created Config struct with all 22 env var fields matching architecture spec exactly
- Created .env.example with all 22 variables documented
- Created 4 shared types in types.rs (DecodedInstruction, DecodedAccount, BlockData, TransactionData)
- Created 13 module stub files across 5 modules (idl, decoder, pipeline, storage, api) with error enums, trait definitions, and placeholder structs
- PipelineError includes From conversions for DecodeError and StorageError
- Created lib.rs with pub mod declarations for all 7 modules
- Created main.rs with clap parse, dotenvy, tracing subscriber init (json/pretty), and tokio::main async entry point
- All code compiles cleanly under strict clippy lints with zero warnings

### Change Log

- 2026-04-05: Initial project scaffolding — all 23 files created, builds clean

### File List

- Cargo.toml (new)
- Cargo.lock (new)
- .gitignore (new)
- rustfmt.toml (new)
- clippy.toml (new)
- .env.example (new)
- src/main.rs (new)
- src/lib.rs (new)
- src/config.rs (new)
- src/types.rs (new)
- src/idl/mod.rs (new)
- src/idl/fetch.rs (new)
- src/decoder/mod.rs (new)
- src/pipeline/mod.rs (new)
- src/pipeline/rpc.rs (new)
- src/pipeline/ws.rs (new)
- src/storage/mod.rs (new)
- src/storage/schema.rs (new)
- src/storage/writer.rs (new)
- src/storage/queries.rs (new)
- src/api/mod.rs (new)
- src/api/handlers.rs (new)
- src/api/filters.rs (new)
