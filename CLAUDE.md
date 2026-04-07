# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Solarix** is a universal Solana indexer that dynamically generates PostgreSQL schemas and decodes on-chain data from any Anchor IDL at runtime. Built in Rust for the Superteam Ukraine bounty (Middle level, 500 USDG first place). This is a greenfield project — no existing codebase to extend.

**Core differentiator:** Runtime dynamic schema generation from arbitrary Anchor IDLs. No codegen, no recompile, no redeploy. Give it a program ID -> it fetches the IDL, generates tables, indexes transactions and account states, exposes a query API.

## Solana Development

**Always use the `/solana-dev` skill** when working on Solana-specific code (Anchor programs, RPC interactions, account decoding, PDAs, Borsh serialization, testing with LiteSVM). The skill is installed and provides current documentation for the Solana ecosystem.

**Always use the `context7` MCP server** to fetch up-to-date docs for any library before writing code — especially for `solana-rpc-client-api`, `solana-pubsub-client`, `anchor-lang-idl-spec`, `sqlx`, `axum`, `tokio`, `chainparser`, and other dependencies. Training data may not reflect the latest API changes.

## Commands

```bash
# Build
cargo build
cargo build --release

# Run locally (requires .env with DATABASE_URL, SOLANA_RPC_URL)
cargo run
cargo watch -x run          # hot reload

# Tests
cargo test                  # unit tests
cargo test --tests          # integration tests (requires running PostgreSQL)

# Linting & formatting
cargo clippy
cargo fmt
cargo fmt -- --check        # CI check

# Docker (full stack)
docker compose up --build   # PostgreSQL + Solarix binary
```

## Architecture

Four-layer pipeline: **Read -> Decode -> Store -> Serve**, connected by bounded Tokio mpsc channels (capacity 256).

Pipeline state machine: `Initializing -> Backfilling <-> CatchingUp -> Streaming -> ShuttingDown`

### Module Layout (14 source files)

```
src/
  main.rs             -- clap parse, tokio::main, spawn pipeline + API server
  lib.rs              -- pub mod declarations
  config.rs           -- Config struct (#[derive(Parser)]), 22 env vars
  types.rs            -- shared types: DecodedInstruction, DecodedAccount, BlockData, TransactionData

  idl/
    mod.rs            -- IdlManager: cache, parse, version detect, type registry
    fetch.rs          -- fetch cascade: on-chain PDA -> bundled registry -> manual upload

  decoder/
    mod.rs            -- SolarixDecoder trait + ChainparserDecoder impl + DecodeError

  pipeline/
    mod.rs            -- PipelineOrchestrator: state machine (5 states), lifecycle
    rpc.rs            -- BlockSource + AccountSource traits + RpcBlockSource (HTTP JSON-RPC, rate-limited)
    ws.rs             -- TransactionStream trait + WsTransactionStream (logsSubscribe)

  storage/
    mod.rs            -- DB pool init, system table bootstrap
    schema.rs         -- DDL generator: IDL -> CREATE TABLE/INDEX, column promotion
    writer.rs         -- batch INSERT...UNNEST, account upsert, checkpoint
    queries.rs        -- dynamic QueryBuilder for API reads, filter -> SQL

  api/
    mod.rs            -- axum Router, AppState, middleware
    handlers.rs       -- 12 endpoint handlers
    filters.rs        -- query param parsing, operator validation against IDL
```

### Key Trait Boundaries (seams for testing)

| Trait               | Defined In        | Purpose                                            |
| ------------------- | ----------------- | -------------------------------------------------- |
| `SolarixDecoder`    | `decoder/mod.rs`  | `decode_instruction()` + `decode_account()`        |
| `BlockSource`       | `pipeline/rpc.rs` | Block fetching abstraction                         |
| `AccountSource`     | `pipeline/rpc.rs` | Account fetching (getProgramAccounts, getMultiple) |
| `TransactionStream` | `pipeline/ws.rs`  | WebSocket subscription abstraction                 |

### Data Architecture

- PostgreSQL 16 with hybrid typed columns + JSONB `data` payload
- Schema-per-program with disambiguated names: `{name}_{program_id_prefix}` (prevents collision when programs share IDL name)
- One table per account type (upsert on pubkey), single `_instructions` table per program (append-only)
- System tables: `programs` (registry + stats) + `indexer_state` (pipeline checkpoints) in `public` schema
- Per-program `_checkpoints` table for slot cursor tracking
- u64 → BIGINT with overflow guard: values > i64::MAX → NULL in promoted column, preserved in JSONB `data`
- All DDL uses `IF NOT EXISTS` — self-bootstrapping, no migration tooling
- GIN indexes with `jsonb_path_ops` for JSONB queries
- `INSERT...UNNEST` + `ON CONFLICT DO NOTHING` for dedup and atomic per-block writes
- JSONB array bindings require `sqlx::types::Json<T>` wrapper

### Cold Start Strategy

Concurrent backfill + streaming (Option C). Both paths write to the same tables with `INSERT ON CONFLICT DO NOTHING`. Crash-safe because both paths are independently idempotent.

## Code Conventions

### Error Handling

- `thiserror` everywhere — 5 module-level error enums: `IdlError`, `DecodeError`, `PipelineError`, `StorageError`, `ApiError`
- Error classification: retryable (429, timeout) / skip-and-log (unknown discriminator) / fatal (DB down)
- Gap detection is a pipeline state transition (Streaming → CatchingUp), not an error
- Error conversions: `impl From<DecodeError> for PipelineError`, `impl From<StorageError> for PipelineError`
- Each error enum lives in its module's `mod.rs` (not separate `error.rs` files)
- API errors implement `IntoResponse` for automatic HTTP status mapping
- Decode failures: log at `warn!`, skip, continue pipeline. If >90% fail in a chunk, log at `error!` (likely IDL mismatch)

### Lints (Cargo.toml)

```toml
[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
```

Note: `clippy::panic` only catches explicit `panic!()` calls, not `assert!()` or indexing. Add `clippy.toml` with `allow-expect-in-tests = true` to allow `expect()` in test code.

### Naming

- Rust: `snake_case` functions/variables/modules, `PascalCase` types/traits/enums, `SCREAMING_SNAKE_CASE` constants
- DB: `snake_case` columns/tables, internal tables prefixed with `_` (`_instructions`, `_metadata`), indexes `idx_{table}_{column}`
- DB schemas: `{sanitized_name}_{first_8_of_program_id}` for collision safety
- API: plural nouns, slash-separated resource paths, `snake_case` query params and JSON fields

### Import Ordering (by convention)

```rust
// std library
use std::collections::HashMap;

// external crates
use axum::Router;
use sqlx::PgPool;

// internal crate
use crate::decoder::SolarixDecoder;
use crate::types::DecodedInstruction;
```

Note: `group_imports` and `imports_granularity` are nightly-only rustfmt options and are NOT in rustfmt.toml. Import ordering is enforced by convention.

### Anti-Patterns

- `unwrap()` or `expect()` outside of tests
- `println!` for logging (use `tracing` macros: `info!`, `debug!`, `warn!`, `error!`)
- `anyhow` (use `thiserror` typed enums)
- Hardcoded connection strings or program addresses
- `CREATE TABLE` without `IF NOT EXISTS`
- SQL string concatenation (use `QueryBuilder::push_bind()`)
- `sqlx::query!()` compile-time macros (use runtime `sqlx::query()` for dynamic DDL)
- Direct `serde_json::Value` binding for JSONB arrays (use `sqlx::types::Json<T>`)
- Blocking calls on the Tokio runtime
- Separate `error.rs` files for small error enums

### Format Patterns

```toml
# rustfmt.toml
edition = "2021"
max_width = 100
```

### Testing

- Unit tests in `#[cfg(test)] mod tests` at bottom of each source file
- Integration tests in `tests/` with fixtures in `tests/fixtures/`
- `proptest` for decoder roundtrip verification (Borsh serialize -> decode -> assert JSON)
- `litesvm` for pipeline integration (deploy program, send txs, verify indexed data)
- `axum-test` for API endpoint testing
- New integration tests **must** reuse `tests/common/postgres.rs::with_postgres` as the canonical pool fixture instead of rolling their own setup. The harness spawns a per-test `postgres:16-alpine` testcontainer, calls `bootstrap_system_tables`, and tears the container down on return or panic. Gate test files behind `#![cfg(feature = "integration")]`.

### Tracing

- Span per pipeline stage: `#[instrument(skip(self), fields(slot, program_id))]`
- Levels: `error!` (fatal), `warn!` (skip-and-log), `info!` (state transitions), `debug!` (per-block), `trace!` (wire data)

## Key Dependencies

| Purpose    | Crate                                                      |
| ---------- | ---------------------------------------------------------- |
| Decode     | `chainparser` (forked) v0.3.0                              |
| IDL types  | `anchor-lang-idl-spec` 0.1.0                               |
| RPC (HTTP) | `solana-rpc-client-api` + `reqwest`                        |
| RPC (WS)   | `solana-pubsub-client`                                     |
| Storage    | `sqlx` 0.8.x (PostgreSQL)                                  |
| API        | `axum`                                                     |
| Async      | `tokio` + `tokio-util`                                     |
| Rate limit | `governor`                                                 |
| Retry      | `backon` (NOT `backoff` — unmaintained, RUSTSEC-2025-0012) |
| Errors     | `thiserror`                                                |
| Logging    | `tracing` + `tracing-subscriber`                           |
| Config     | `clap` + `dotenvy`                                         |

All Solana crates target v3.x ecosystem.

## Solana-Specific Constraints

- Always set `maxSupportedTransactionVersion: 0` on RPC calls or v0 transactions are silently dropped
- Instruction discriminator: `SHA-256("global:<snake_case>")[0..8]`
- Account discriminator: `SHA-256("account:<PascalCase>")[0..8]`
- `COption` uses 4-byte u32 tag (differs from Rust `Option` with 1-byte tag) — decoder must dispatch differently
- Target Anchor IDL v0.30+ format (uses `metadata.spec` field)
- Public RPC rate limit: ~10 RPS — all backfill design respects this
- WebSocket has no delivery/ordering/exactly-once guarantees — all reliability is application-layer
- `logsSubscribe` supports exactly 1 program filter, returns signature + logs (not full tx data)
- `getProgramAccounts` has no pagination — use `dataSlice: {offset: 0, length: 0}` for pubkey-only fetch, then batch `getMultipleAccounts` (max 100)

## Worktree Workflow

Sprint plan with parallel track assignments: `_bmad-output/implementation-artifacts/sprint-status.yaml`

### Parallel Development Model

- Each parallel track (A, B, C) runs in its own git worktree/branch simultaneously
- Stories within a track are sequential; tracks within a sprint are parallel
- Sprint N+1 begins only after all Sprint N worktree branches merge to main
- Track file ownership is declared in the sprint plan — respect it to avoid merge conflicts

### Worktree Commands

```bash
claude --worktree track-a-sprint-2    # start isolated session for a track
claude -w                              # auto-named worktree
/clean_gone                            # remove stale branches + worktrees after merge
```

### Subagent Isolation

When spawning agents for independent track work, use `isolation: "worktree"` to prevent file conflicts.

### Merge Protocol

1. All tracks in a sprint complete independently
2. Merge each track branch to main: `git merge worktree-<track-name>`
3. Verify on main: `cargo test && cargo clippy`
4. Clean up: `/clean_gone`
5. Only then start next sprint's stories

### .worktreeinclude

Create at repo root to copy gitignored files into new worktrees:

```
.env
.env.local
```

## Planning Artifacts

BMad framework outputs live in `_bmad-output/`:

- `bounty-requirements.md` — source of truth for bounty requirements and implicit expectations
- `planning-artifacts/prd.md` — full PRD with success criteria, user journeys, API surface (12 endpoints)
- `planning-artifacts/architecture.md` — complete architecture decisions, module layout, data flow, implementation patterns
- `planning-artifacts/research/` — 10 deep research reports (RPC capabilities, Borsh decoding, IDL-to-DDL mapping, storage architecture, backfill pipeline, REST API design, testing strategy)

Research docs in `docs/research/` and Solana skill references in `.agents/skills/solana-dev/references/`.

## Critical Implementation Risk

The **chainparser fork** is the highest-risk component. Repo is dormant (7 commits, last activity Sep 2024). Requires upgrading from solana-sdk 1.18 -> 3.x, adding instruction arg deserialization, fixing COption for Defined inner types. The `SolarixDecoder` trait abstracts this — implementation can be swapped to a custom decoder (~330 LOC covering top 10 types, ~95% of real programs) if the fork fails.
