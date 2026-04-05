# Requirements Inventory

## Functional Requirements

FR1: System can fetch Anchor IDL from on-chain PDA given a program ID
FR2: User can upload IDL manually via file path or API endpoint
FR3: System can store IDL metadata (hash, source, program name) for reference
FR4: System can generate PostgreSQL schema from any v0.30+ Anchor IDL at runtime
FR5: System can create per-program database schema with namespace isolation
FR6: System can map IDL type variants (23 official + 6 unofficial) to appropriate PostgreSQL column types
FR7: System can promote top-level scalar fields to native typed columns
FR8: System can store full decoded payload in JSONB `data` column as safety net
FR9: System can create indexes (B-tree on common columns, GIN `jsonb_path_ops` on JSONB)
FR10: System can decode instruction arguments from transaction data using IDL
FR11: System can decode account state data using IDL and discriminator matching
FR12: System can identify unknown discriminators and log warning without crashing
FR13: User can index transactions within a specified slot range (batch mode)
FR14: User can index transactions from a list of signatures (batch mode)
FR15: System can chunk large slot ranges for rate-limit-safe processing
FR16: System can filter block transactions for target program
FR17: System can fetch current account states via `getProgramAccounts` and `getMultipleAccounts`
FR18: System can subscribe to new transactions for a program via WebSocket
FR19: System can automatically reconnect on WebSocket disconnect with backoff
FR20: System can detect gaps after reconnection and mini-backfill missed slots
FR21: System can deduplicate transactions across backfill and streaming paths
FR22: System can persist indexing checkpoint (last processed slot, status) to database
FR23: System can detect gap between last checkpoint and current chain tip on startup
FR24: System can resume backfill from last checkpoint without reprocessing
FR25: System can transition seamlessly from backfill to real-time streaming
FR26: User can register a program for indexing via API (program ID or IDL upload)
FR27: User can list registered programs and their indexing status
FR28: User can query instructions by type with multi-parameter filters
FR29: User can query account states by type with multi-parameter filters
FR30: User can retrieve a specific account by pubkey
FR31: User can get instruction call counts over a time period (aggregation)
FR32: User can get basic program statistics (total transactions, accounts, etc.)
FR33: User can paginate query results
FR34: System can retry failed RPC requests with exponential backoff and jitter
FR35: System can perform graceful shutdown preserving checkpoint state
FR36: System can emit structured JSON logs with per-stage tracing spans
FR37: Operator can check system health via health endpoint (pipeline status, lag, DB health)
FR38: Operator can configure operational parameters via environment variables
FR39: System can start with all dependencies via single `docker compose up` command
FR40: System can auto-create database schema on first start (self-bootstrapping)
FR41: Repository includes `.env.example` with all configurable variables documented

## NonFunctional Requirements

NFR1: API response time <100ms for single-entity lookups, <500ms for filtered queries
NFR2: Backfill throughput not bottlenecked by pipeline (RPC rate limit is the constraint at 10 RPS)
NFR3: Streaming mode indexes transactions within seconds of confirmation
NFR4: Zero data loss on crash -- per-block atomic writes, checkpoint updates after each chunk
NFR5: Exponential backoff with jitter on RPC failures
NFR6: WebSocket automatic reconnection with gap backfill before resuming
NFR7: Graceful shutdown: drain pipeline, flush to DB, persist checkpoint
NFR8: All dynamic SQL uses parameterized queries; table/column names derived from IDL, not user input
NFR9: No secrets in Docker image or repository (all via environment variables)
NFR10: Comprehensive tests on core modules: decoder (proptest roundtrip), schema generator (unit), API (axum-test)
NFR11: Integration tests against LiteSVM local validator for end-to-end verification
NFR12: CI pipeline: lint (clippy, fmt), test, Docker smoke test
NFR13: Clean Cargo workspace with logical module boundaries
NFR14: cargo clippy clean (including clippy::unwrap_used denial), cargo fmt enforced

## Additional Requirements

- Greenfield project: `cargo init --name solarix`, single crate with modules (no workspace)
- Starter template: None -- initialized from scratch
- chainparser v0.3.0 fork as Git dependency; SolarixDecoder trait abstracts decoder for swappability
- 5 module-level thiserror enums: IdlError, DecodeError, PipelineError, StorageError, ApiError
- Error classification: retryable (429, timeout) / skip-and-log (unknown discriminator) / fatal (DB down)
- Pipeline state machine: Initializing -> Backfilling <-> CatchingUp -> Streaming -> ShuttingDown
- Bounded Tokio mpsc(256) channels between pipeline stages for backpressure
- CancellationToken (tokio-util) for 4-phase graceful shutdown
- Schema-per-program with disambiguated names: `{sanitized_name}_{first_8_of_program_id}`
- Two-tier checkpoint: `indexer_state` (global pipeline status) + per-program `_checkpoints` (slot cursors)
- u64 -> BIGINT with overflow guard: values > i64::MAX -> NULL in promoted column, preserved in JSONB
- INSERT...UNNEST + ON CONFLICT for dedup and atomic per-block writes
- JSONB array bindings require sqlx::types::Json<T> wrapper
- DDL via sqlx::raw_sql() (bypasses prepared statements)
- Arc<RwLock<ProgramRegistry>> for shared mutable program state across pipeline + API
- backon (NOT backoff -- unmaintained, RUSTSEC-2025-0012) for retry
- governor (GCRA) for rate limiting, async-native
- Docker multi-stage build: rust:latest build + debian-slim runtime
- 5 GitHub Actions CI jobs: lint, unit, integration (PG + LiteSVM), coverage, Docker smoke
- rustfmt.toml (edition=2021, max_width=100) + clippy.toml (allow-expect-in-tests)
- Cargo.toml lints: unsafe_code=forbid, unwrap_used=deny, expect_used=deny, panic=deny
- All pub items get /// doc comments
- Bundled IDL registry in idls/ directory (70+ from AllenHark + curated)

## UX Design Requirements

N/A -- Solarix is a backend service with no UI. All interaction is via REST API and CLI.

## FR Coverage Map

FR1: Epic 2 - IDL fetch from on-chain PDA
FR2: Epic 2 - Manual IDL upload (file path or API)
FR3: Epic 2 - IDL metadata storage (hash, source, name)
FR4: Epic 2 - PostgreSQL schema generation from IDL
FR5: Epic 2 - Per-program schema with namespace isolation
FR6: Epic 2 - IDL type variant to PostgreSQL column mapping
FR7: Epic 2 - Top-level scalar field promotion to typed columns
FR8: Epic 2 - Full decoded payload in JSONB data column
FR9: Epic 2 - B-tree and GIN index creation
FR10: Epic 3 - Instruction argument decoding
FR11: Epic 3 - Account state decoding with discriminator matching
FR12: Epic 3 - Unknown discriminator handling (warn + skip)
FR13: Epic 3 - Batch indexing by slot range
FR14: Epic 3 - Batch indexing by signature list
FR15: Epic 3 - Slot range chunking for rate-limit safety
FR16: Epic 3 - Block transaction filtering for target program
FR17: Epic 3 - Account state fetching (getProgramAccounts + getMultipleAccounts)
FR18: Epic 4 - WebSocket subscription for new transactions
FR19: Epic 4 - Automatic reconnection with backoff
FR20: Epic 4 - Gap detection and mini-backfill after reconnect
FR21: Epic 3 - Transaction deduplication across paths
FR22: Epic 3 - Checkpoint persistence (last processed slot, status)
FR23: Epic 4 - Gap detection between checkpoint and chain tip
FR24: Epic 4 - Resume backfill from last checkpoint
FR25: Epic 4 - Seamless backfill-to-streaming transition
FR26: Epic 5 - Program registration via API
FR27: Epic 5 - List registered programs with status
FR28: Epic 5 - Query instructions with multi-parameter filters
FR29: Epic 5 - Query account states with multi-parameter filters
FR30: Epic 5 - Retrieve specific account by pubkey
FR31: Epic 5 - Instruction count aggregation over time
FR32: Epic 5 - Program statistics (totals, counts)
FR33: Epic 5 - Query result pagination
FR34: Epic 3 - RPC retry with exponential backoff and jitter
FR35: Epic 4 - Graceful shutdown preserving checkpoint
FR36: Epic 6 - Structured JSON logs with tracing spans
FR37: Epic 5 - Health endpoint (pipeline status, lag, DB health)
FR38: Epic 1 - Configuration via environment variables
FR39: Epic 1 - Docker Compose single-command start
FR40: Epic 1 - Self-bootstrapping database schema
FR41: Epic 1 - .env.example with documented variables
