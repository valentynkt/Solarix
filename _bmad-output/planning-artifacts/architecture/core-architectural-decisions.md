# Core Architectural Decisions

## Decision Priority Analysis

**Critical Decisions (Block Implementation):**

All resolved — technology stack, data architecture, pipeline design, API design, and deployment strategy confirmed through research + party mode consensus.

**Deferred Decisions (Post-MVP):**

- Geyser/gRPC data source (trait abstraction ready)
- GraphQL API layer
- Prometheus metrics endpoint
- Schema evolution on IDL changes
- Legacy v0.29 IDL format support
- Anchor v1.0 PMP IDL fetch

## Structural Decisions

| Decision            | Choice                    | Rationale                                                                                                           |
| ------------------- | ------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Project structure   | Single crate with modules | One binary, velocity first, clean `mod.rs` boundaries, mechanical to split later                                    |
| Error handling      | `thiserror` everywhere    | 5 bounded enums mapping to error classification (retryable/skip/fatal), signals code quality to judges, no `anyhow` |
| Database migrations | All runtime DDL           | One pattern, self-bootstrapping via `IF NOT EXISTS`, no migration tooling dependency                                |
| Async runtime       | Default `#[tokio::main]`  | Multi-threaded, worker threads = CPU cores, over-provisioned for bounty scale                                       |

## Data Architecture

| Decision              | Choice                                      | Rationale                                                                                                                                                |
| --------------------- | ------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Database              | PostgreSQL 16                               | Hybrid typed+JSONB, GIN indexes, schema-per-program isolation                                                                                            |
| Storage pattern       | Typed common columns + JSONB `data` payload | Filterable scalars + flexible JSONB safety net                                                                                                           |
| u64 handling          | BIGINT with overflow guard                  | Values > i64::MAX → NULL in promoted column, preserved as string in JSONB `data`. See research agent-2a §2.6 for analysis. Real-world overflow is rare.  |
| GIN index strategy    | `jsonb_path_ops`                            | 3-4x smaller than `jsonb_ops`, 5x lower write overhead, containment queries sufficient                                                                   |
| Write path            | `INSERT...UNNEST` + `ON CONFLICT`           | Per-block atomic writes, upsert for accounts, dedup for instructions. JSONB arrays require `sqlx::types::Json<T>` wrapper.                               |
| Account tables        | One table per account type                  | Upsert on pubkey for latest state, promoted scalar columns                                                                                               |
| Instruction table     | Single unified `_instructions` per program  | Append-only with JSONB args, simpler DDL than table-per-instruction                                                                                      |
| System tables         | Runtime DDL in `public` schema              | `programs` (registry + stats) + `indexer_state` (pipeline checkpoints) — two tables, created on startup                                                  |
| Per-program isolation | Schema-per-program with disambiguated names | `CREATE SCHEMA "{name}_{program_id_prefix}"` — prevents collision when programs share IDL `metadata.name`. See [Schema Naming](#schema-naming-strategy). |

### Schema Naming Strategy

Anchor IDL `metadata.name` comes from the program's `Cargo.toml` package name. **This is not unique** — forks commonly keep the same name but deploy under a different program ID. To prevent schema collisions and data corruption:

```
Schema name = {sanitized_idl_name}_{lowercase_first_8_chars_of_base58_program_id}
```

Examples:

- `token_swap` at `SwaPpA...` → schema `token_swap_swappall`
- `token_swap` at `9xQeW4...` → schema `token_swap_9xqew4rs`

An 8-character base58 prefix gives ~2.8 trillion possible values — accidental collision is astronomically unlikely. The human-readable program name remains the leading part for query ergonomics.

The `programs` system table uses `program_id TEXT PRIMARY KEY` (full base58 address) and stores both `program_name` (from IDL) and `schema_name` (derived composite).

### Checkpoint Architecture

Two-tier checkpoint design serving different purposes:

| Table           | Schema      | Purpose                                                                                | Updated                                                                        |
| --------------- | ----------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `indexer_state` | `public`    | Global pipeline status per program: status, heartbeat, error tracking, total counts    | Per-chunk during backfill, every 10s during streaming, final write on shutdown |
| `_checkpoints`  | Per-program | Per-program slot cursors: backfill position, realtime position, accounts sync position | Per-block during writes                                                        |

Both tables are needed. `indexer_state` answers "what is the pipeline doing?" while `_checkpoints` answers "where exactly has processing reached for each data stream?"

## Decoder Architecture

| Decision        | Choice                                   | Rationale                                                                             |
| --------------- | ---------------------------------------- | ------------------------------------------------------------------------------------- |
| Primary decoder | chainparser v0.3.0 fork                  | 26 type variants already implemented, 3 bounded gaps to fix                           |
| Abstraction     | `SolarixDecoder` trait                   | `decode_instruction()` + `decode_account()`, enables swap to custom decoder if needed |
| Shared engine   | Single `decode_type()` recursive descent | Both instruction and account paths use same type walker                               |
| Fallback        | Custom decoder (Plan B, ~960 LOC)        | Only if fork encounters unfixable issue during sdk v3 upgrade                         |
| IDL format      | v0.30+ only (MVP)                        | Current standard, legacy v0.29 conversion deferred                                    |

## Transport & Pipeline

| Decision          | Choice                                                     | Rationale                                                         |
| ----------------- | ---------------------------------------------------------- | ----------------------------------------------------------------- |
| HTTP RPC          | `solana-rpc-client-api` + `reqwest`                        | Thin deps, no vendor lock-in, configurable URL                    |
| WebSocket         | `solana-pubsub-client`                                     | `logsSubscribe` with program filter, reconnection + gap detection |
| Pipeline channels | Tokio bounded mpsc(256)                                    | Automatic backpressure, simple producer-consumer                  |
| State machine     | 5 states: Init/Backfill/CatchUp/Stream/Shutdown            | Covers all lifecycle transitions                                  |
| Handoff strategy  | Option C — concurrent backfill+stream with signature dedup | Zero-gap guarantee, crash-safe, `INSERT ON CONFLICT DO NOTHING`   |
| Rate limiting     | `governor` (GCRA)                                          | Async-native, jitter support, configurable RPS                    |
| Retry             | `backon` with exponential backoff                          | 500ms initial, 30s max, 5min total timeout                        |
| Graceful shutdown | `CancellationToken` from `tokio-util`                      | 4-phase: reader stop → pipeline drain → DB flush → cleanup        |

## API & Communication

| Decision        | Choice                                             | Rationale                                                     |
| --------------- | -------------------------------------------------- | ------------------------------------------------------------- |
| Framework       | `axum`                                             | Catch-all parametric routes, no runtime router rebuilding     |
| Routing         | `/{program_id}/...` parametric handlers            | Validated against IDL cache, 12 endpoints                     |
| Filters         | Query param operators (`_gt`, `_lt`, `_eq`, etc.)  | IDL-validated, SQL injection prevented via `push_bind()`      |
| Pagination      | Cursor-based for instructions, offset for accounts | Keyset on `(slot, signature)` for large result sets           |
| Response format | `{ data, pagination, meta }` JSON envelope         | Consistent, includes query timing                             |
| Security        | 5-layer SQL injection prevention                   | Table names sanitized at registration, values via bind params |

## Infrastructure & Deployment

| Decision  | Choice                                | Rationale                                                      |
| --------- | ------------------------------------- | -------------------------------------------------------------- |
| Container | Docker multi-stage build              | Build stage (rust:latest) + runtime stage (debian-slim)        |
| Compose   | postgres:16 + solarix binary          | Single `docker compose up`, self-bootstrapping                 |
| Config    | `clap` + `dotenvy`, 22 env vars       | CLI args > env vars > .env > defaults                          |
| Logging   | `tracing` + `tracing-subscriber` JSON | Spans per pipeline stage, structured output                    |
| Health    | `GET /health`                         | Pipeline status, lag, DB connectivity                          |
| CI        | 5 GitHub Actions jobs                 | Lint, unit, integration (PG + LiteSVM), coverage, Docker smoke |

## Error Handling Architecture

Five module-level error enums mapping to the cross-cutting error classification:

| Module     | Error Enum      | Key Variants                                                                       | Classification     |
| ---------- | --------------- | ---------------------------------------------------------------------------------- | ------------------ |
| `idl`      | `IdlError`      | `FetchFailed`, `ParseFailed`, `NotFound`, `UnsupportedFormat`                      | Fatal / Retryable  |
| `decoder`  | `DecodeError`   | `UnknownDiscriminator`, `DeserializationFailed`, `IdlNotLoaded`, `UnsupportedType` | Skip-and-log       |
| `pipeline` | `PipelineError` | `RpcFailed`, `WebSocketDisconnect`, `RateLimited`                                  | Retryable          |
| `storage`  | `StorageError`  | `ConnectionFailed`, `DdlFailed`, `WriteFailed`, `CheckpointFailed`                 | Fatal / Retryable  |
| `api`      | `ApiError`      | `InvalidFilter`, `ProgramNotFound`, `QueryFailed`                                  | User error / Fatal |

**Note:** Gap detection (WS disconnect → need to catch up) is a **pipeline state transition** (Streaming → CatchingUp), not an error. It is handled by the pipeline orchestrator's state machine, not the error classification system.

**Error conversions:** `PipelineError` wraps `DecodeError` and `StorageError` via `impl From<DecodeError> for PipelineError` and `impl From<StorageError> for PipelineError`. `ApiError` implements `IntoResponse` for automatic HTTP status mapping.

## Crate Dependencies (Final Stack)

| Layer         | Crate                                | Version      | Purpose                              |
| ------------- | ------------------------------------ | ------------ | ------------------------------------ |
| Decode        | `chainparser` (forked)               | 0.3.0        | Runtime IDL decode, 26 type variants |
| IDL Types     | `anchor-lang-idl-spec`               | 0.1.0        | Official Rust IDL type definitions   |
| RPC (HTTP)    | `solana-rpc-client-api` + `reqwest`  | 3.x / latest | Thin deps, type definitions only     |
| RPC (WS)      | `solana-pubsub-client`               | 3.x          | WebSocket subscriptions              |
| Storage       | `sqlx`                               | 0.8.x        | PostgreSQL with runtime queries      |
| API           | `axum`                               | latest       | Catch-all parametric routes          |
| Pipeline      | `tokio` + `tokio-util`               | latest       | Bounded mpsc, CancellationToken      |
| Rate Limit    | `governor`                           | latest       | GCRA, async-native                   |
| Retry         | `backon`                             | latest       | Exponential with jitter              |
| Errors        | `thiserror`                          | latest       | Typed error enums                    |
| Logging       | `tracing` + `tracing-subscriber`     | latest       | Structured JSON, spans               |
| Config        | `clap` + `dotenvy`                   | latest       | Env vars + CLI                       |
| Serialization | `serde` + `serde_json`               | latest       | JSON handling                        |
| Testing       | `proptest` + `litesvm` + `axum-test` | latest       | Property + integration + API         |

**Dependency notes:**

- `backoff` crate is **unmaintained** (RUSTSEC-2025-0012). Use `backon` instead — actively maintained, same API surface.
- `governor` v0.10.2 — docs.rs build failed; verify compilation in dependency tree before committing. Consider `tower_governor` for axum middleware integration.
- JSONB array bindings in sqlx require wrapping `serde_json::Value` in `sqlx::types::Json<T>`. Enable the `json` feature flag on sqlx.
