# Implementation Patterns & Consistency Rules

## Module Layout

```
src/
  main.rs             -- entry point, clap parse, tokio::main, spawn pipeline + API
  lib.rs              -- pub mod declarations, crate-level docs
  config.rs           -- Config struct (#[derive(Parser)]), all 22 env vars
  types.rs            -- shared types: DecodedInstruction, DecodedAccount, BlockData, TransactionData

  idl/
    mod.rs            -- IdlManager: cache, parse, version detect, type registry
    fetch.rs          -- fetch cascade: on-chain PDA -> bundled registry -> manual

  decoder/
    mod.rs            -- SolarixDecoder trait + ChainparserDecoder impl + DecodeError

  pipeline/
    mod.rs            -- PipelineOrchestrator: state machine (5 states), lifecycle
    rpc.rs            -- BlockSource + AccountSource traits + RpcBlockSource impl (HTTP, rate-limited)
    ws.rs             -- TransactionStream trait + WsTransactionStream (logsSubscribe)

  storage/
    mod.rs            -- DB pool init, system table bootstrap, connection management
    schema.rs         -- DDL generator: IDL -> CREATE TABLE/INDEX, column promotion
    writer.rs         -- batch insert (UNNEST), account upsert, checkpoint, per-block atomic
    queries.rs        -- dynamic QueryBuilder for API reads, filter -> SQL translation

  api/
    mod.rs            -- axum Router, AppState, middleware (tracing layer)
    handlers.rs       -- 12 endpoint handlers
    filters.rs        -- query param parsing, operator validation against IDL
```

**14 files total.** Flat enough to scan in 30 seconds, separated enough to show architectural thinking.

**Key layout decisions:**

- `types.rs` hosts cross-module data types (`DecodedInstruction`, `DecodedAccount`, `BlockData`, `TransactionData`) that flow between pipeline stages
- Error enums live in each module's `mod.rs` (4-8 variants each, not worth separate files)
- `schema.rs` in `storage/` — DDL generation is a storage initialization concern
- `writer.rs` in `storage/` — all DB writes in one place, not split across pipeline and storage
- `rpc.rs` and `ws.rs` split — different protocols, different lifecycles, different retry patterns
- `rpc.rs` hosts both `BlockSource` and `AccountSource` traits — both use HTTP JSON-RPC
- No `processor.rs` — pipeline orchestrator directly calls `decoder.decode()` then `writer.write()`
- `lib.rs` for module declarations — `main.rs` stays thin (parse config, spawn, await)

## Naming Patterns

**Rust Code:**

- All identifiers: `snake_case` (functions, variables, modules, files)
- Types/traits/enums: `PascalCase`
- Constants: `SCREAMING_SNAKE_CASE`
- Trait naming: domain noun for traits (`SolarixDecoder`, `BlockSource`, `TransactionStream`, `AccountSource`), descriptive prefix for implementations (`ChainparserDecoder`, `RpcBlockSource`, `WsTransactionStream`, `RpcAccountSource`)
- All `pub` items get `///` doc comments (structs, traits, enums, public functions)

**Import Ordering (by convention):**

```rust
// std library
use std::collections::HashMap;
use std::sync::Arc;

// external crates
use axum::Router;
use sqlx::PgPool;
use tokio::sync::RwLock;

// internal crate
use crate::decoder::SolarixDecoder;
use crate::storage::writer;
use crate::types::DecodedInstruction;
```

Import ordering is enforced by convention and code review. The `group_imports` and `imports_granularity` rustfmt options require nightly and are omitted from `rustfmt.toml` to keep stable toolchain compatibility.

**Database Naming:**

- Schema names: `{sanitized_name}_{first_8_of_program_id}` (see [Schema Naming Strategy](#schema-naming-strategy))
- Table names: `snake_case` matching IDL account type names
- Internal tables: underscore prefix (`_instructions`, `_metadata`, `_checkpoints`)
- Column names: `snake_case` matching IDL field names
- All identifiers double-quoted in generated DDL
- Index naming: `idx_{table}_{column}` for B-tree, `gin_{table}_data` for GIN

**API Naming:**

- Endpoints: `/api/programs/{id}/instructions/{name}` — plural nouns, slash-separated resource paths
- Query params: `snake_case` (`slot_from`, `amount_gt`)
- JSON response fields: `snake_case` (Rust serde default)

## Format Patterns

**API Response Envelope:**

```json
{
  "data": [...],
  "pagination": { "total": 100, "limit": 50, "has_more": true, "next_cursor": "..." },
  "meta": { "program_id": "...", "query_time_ms": 42 }
}
```

**Program Registration Response (202 Accepted):**

```json
{
  "data": {
    "program_id": "...",
    "status": "registering",
    "idl_source": "on_chain"
  },
  "meta": { "message": "Program registered. Indexing will begin shortly." }
}
```

**Error Response:**

```json
{
  "error": {
    "code": "PROGRAM_NOT_FOUND",
    "message": "Program JUP6... is not registered"
  }
}
```

- HTTP status codes: 200 (success), 201 (created), 202 (accepted, async), 400 (bad filter), 404 (not found), 429 (rate limited), 500 (internal)

**Date/Time:** ISO 8601 strings in JSON (`2026-04-05T12:00:00Z`), `TIMESTAMPTZ` in PostgreSQL

## Process Patterns

**Error Handling Flow:**

- Each module's error enum defined in `mod.rs` via `thiserror::Error`
- Pipeline errors classified at creation: `impl PipelineError { fn is_retryable(&self) -> bool }`
- Error conversions via `impl From<DecodeError> for PipelineError` (and StorageError)
- API errors implement `IntoResponse` for automatic HTTP status mapping
- Unknown/unexpected errors: log at `error!` level with full context, return 500
- Decode failures on individual transactions: log at `warn!`, skip, continue pipeline
- Systemic decode failures (>90% in a chunk): log at `error!` level — likely IDL version mismatch

### Shared State

- `AppState` struct with immutable fields (db pool, rpc client, config)
- `Arc<RwLock<ProgramRegistry>>` for mutable program state — `ProgramRegistry` wraps `IdlManager` + schema metadata + decoder instances. Shared across pipeline (for decoding) and API (for filter validation against IDL field types).
- Pass as `axum::extract::State<Arc<AppState>>`
- **Write lock contention:** Program registration takes a write lock on `ProgramRegistry`, briefly blocking API reads. Acceptable for bounty scale (registrations are rare).

**Tracing Conventions:**

- Span per pipeline stage: `#[instrument(skip(self), fields(slot, program_id))]`
- Log levels: `error!` (fatal), `warn!` (skip-and-log), `info!` (state transitions, startup), `debug!` (per-block/per-tx), `trace!` (wire data)
- All structured fields use `snake_case`

**Configuration Pattern:**

```rust
#[derive(Parser)]
struct Config {
    #[arg(long, env = "SOLANA_RPC_URL", default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,
    // ... all 22 env vars follow this pattern
}
```

## Tooling Configuration

**`rustfmt.toml`:**

```toml
edition = "2021"
max_width = 100
```

Note: `imports_granularity` and `group_imports` are nightly-only options (with a known non-idempotency bug, rustfmt#6195). They are omitted to keep stable toolchain compatibility. Import ordering is enforced by convention.

**`clippy.toml`:**

```toml
allow-expect-in-tests = true
```

This allows `expect()` in `#[test]` functions and `#[cfg(test)]` modules while keeping the crate-wide `expect_used = "deny"` lint for production code.

**`Cargo.toml` lints:**

```toml
[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
```

Note: `clippy::panic` only catches explicit `panic!()` macro invocations. It does NOT block `assert!()`, array indexing, or `unreachable!()`. This is the correct level — explicit panics should not exist in production code.

## Enforcement Guidelines

**All AI agents MUST:**

- Run `cargo clippy` and `cargo fmt` before considering code complete
- Place unit tests in `#[cfg(test)] mod tests` at bottom of each source file
- Place integration tests in `tests/` directory with fixtures in `tests/fixtures/`
- Use `thiserror` for all error types — no `anyhow`, no `unwrap()`, no `expect()` in non-test code
- Use `tracing` macros (`info!`, `debug!`, etc.) — never `println!` or `eprintln!`
- Use `sqlx::query()` (runtime) not `sqlx::query!()` (compile-time) for all dynamic SQL
- Use `sqlx::raw_sql()` for DDL execution (bypasses prepared statements)
- Wrap `serde_json::Value` in `sqlx::types::Json<T>` when binding JSONB arrays
- Add `///` doc comments to all `pub` items
- Follow import ordering by convention: std → external → crate

**Anti-Patterns:**

- `unwrap()` or `expect()` outside of tests
- `println!` for logging
- Hardcoded connection strings or program addresses
- `CREATE TABLE` without `IF NOT EXISTS`
- SQL string concatenation instead of `QueryBuilder::push_bind()`
- Direct `serde_json::Value` binding for JSONB arrays (use `sqlx::types::Json<T>`)
- Blocking calls on the Tokio runtime
- Separate `error.rs` files for small error enums (keep in `mod.rs`)
