# Story 1.2: Database Connection & System Table Bootstrap

Status: review

## Story

As an operator,
I want the system to connect to PostgreSQL and auto-create its system tables on startup,
so that no manual database setup or migration tooling is required.

## Acceptance Criteria

1. **AC1: Connection pool creation**
   - **Given** a running PostgreSQL instance and a valid `DATABASE_URL`
   - **When** the application starts
   - **Then** it creates a connection pool via `sqlx::PgPool` using `PgPoolOptions`
   - **And** pool size is configured from `Config.db_pool_min` / `Config.db_pool_max`
   - **And** the pool includes acquire timeout (5s), idle timeout (300s), max lifetime (1800s)

2. **AC2: System tables created (programs)**
   - **Given** a connected pool
   - **When** bootstrap runs
   - **Then** it creates the `programs` table in `public` schema with `IF NOT EXISTS`
   - **And** columns: `program_id VARCHAR(44) PRIMARY KEY`, `program_name TEXT NOT NULL`, `schema_name TEXT NOT NULL UNIQUE`, `idl_hash VARCHAR(64)`, `idl_source TEXT` (valid values: `'onchain'`, `'file'`, `'bundled'`, `'manual'`), `status TEXT NOT NULL DEFAULT 'initializing'` (valid values: `'initializing'`, `'backfilling'`, `'realtime'`, `'paused'`, `'error'`), `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`

3. **AC3: System tables created (indexer_state)**
   - **Given** a connected pool
   - **When** bootstrap runs
   - **Then** it creates the `indexer_state` table in `public` schema with `IF NOT EXISTS`
   - **And** columns: `program_id TEXT PRIMARY KEY REFERENCES programs(program_id)`, `status TEXT NOT NULL`, `last_processed_slot BIGINT`, `last_heartbeat TIMESTAMPTZ`, `error_message TEXT`, `total_instructions BIGINT DEFAULT 0`, `total_accounts BIGINT DEFAULT 0`

4. **AC4: DDL execution via raw_sql**
   - **Given** the DDL statements
   - **When** they are executed
   - **Then** they use `sqlx::raw_sql()` (not compile-time macros, not prepared statements)

5. **AC5: Invalid DATABASE_URL handling**
   - **Given** an invalid or unreachable `DATABASE_URL`
   - **When** the application starts
   - **Then** it logs a fatal error with connection details (excluding password) and exits with non-zero status

6. **AC6: Idempotent bootstrap**
   - **Given** system tables already exist from a previous run
   - **When** the application starts again
   - **Then** bootstrap completes without errors (`IF NOT EXISTS` is idempotent)

7. **AC7: main.rs integration**
   - **Given** `main.rs`
   - **When** it runs
   - **Then** after tracing init and config parse, it creates the DB pool, calls bootstrap, and logs success
   - **And** on bootstrap failure, it logs the error and exits

## Tasks / Subtasks

- [x] Task 1: Replace `init_pool()` stub in `src/storage/mod.rs` (AC: #1, #5)
  - [x] Change signature from `pub async fn init_pool(_database_url: &str) -> Result<(), StorageError>` to `pub async fn init_pool(config: &Config) -> Result<PgPool, StorageError>`
  - [x] Add imports: `sqlx::PgPool`, `sqlx::postgres::PgPoolOptions`, `std::time::Duration`, `tracing::info`, `crate::config::Config`
  - [x] Configure `PgPoolOptions` with `min_connections(config.db_pool_min)` (default 2), `max_connections(config.db_pool_max)` (default 10), acquire timeout 5s, idle timeout 300s, max lifetime 1800s
  - [x] On connection failure, return `StorageError::ConnectionFailed` with sanitized URL (no password)
  - [x] Log at `info!` level on successful connection with pool size details
- [x] Task 2: Implement `bootstrap_system_tables()` in `src/storage/mod.rs` (AC: #2, #3, #4, #6)
  - [x] Add `pub async fn bootstrap_system_tables(pool: &PgPool) -> Result<(), StorageError>`
  - [x] Write DDL for `programs` table per AC2 columns, using `IF NOT EXISTS`
  - [x] Write DDL for `indexer_state` table per AC3 columns, using `IF NOT EXISTS`
  - [x] Execute both via `sqlx::raw_sql()` in sequence
  - [x] On DDL failure, return `StorageError::DdlFailed` with context
  - [x] Log at `info!` level on successful bootstrap
- [x] Task 3: Update `main.rs` to call pool init and bootstrap (AC: #7)
  - [x] After tracing init, call `storage::init_pool(&config).await`
  - [x] On failure, log error and return early (non-zero exit)
  - [x] Call `storage::bootstrap_system_tables(&pool).await`
  - [x] On failure, log error and return early
  - [x] Log success: "database connected, system tables bootstrapped"
- [x] Task 4: Add integration test for DDL idempotency (AC: #6)
  - [x] Create `tests/bootstrap_test.rs` with `#[ignore]` (requires running PostgreSQL)
  - [x] Test calls `bootstrap_system_tables` twice and asserts both succeed
  - [x] Optionally verify tables exist via `information_schema.tables` query
  - [x] Run with `cargo test -- --ignored` when PostgreSQL is available
- [x] Task 5: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [ ] With running PostgreSQL, `cargo run` connects and creates tables
  - [ ] Second `cargo run` succeeds (idempotent)

## Dev Notes

### Story 1.1 State (Completed)

Story 1.1 is merged. The current codebase has:

- `src/storage/mod.rs` — `StorageError` enum (4 variants) + `init_pool` stub returning `Result<(), StorageError>`
- `src/config.rs` — `Config` struct with `database_url: String`, `db_pool_min: u32` (default 2), `db_pool_max: u32` (default 10)
- `src/main.rs` — clap parse, tracing init, placeholder comment for DB pool
- `src/lib.rs` — all 7 module declarations

This story replaces the `init_pool` stub with a real implementation. Key change: the stub signature `init_pool(_database_url: &str) -> Result<(), StorageError>` must change to `init_pool(config: &Config) -> Result<PgPool, StorageError>` (takes full Config, returns pool).

### StorageError Enum (from Story 1.1)

The `StorageError` enum is defined in Story 1.1 with these variants:

```rust
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("DDL execution failed: {0}")]
    DdlFailed(String),

    #[error("write failed: {0}")]
    WriteFailed(String),

    #[error("checkpoint failed: {0}")]
    CheckpointFailed(String),
}
```

Do NOT redefine or modify this enum. Use `ConnectionFailed` for pool init failures and `DdlFailed` for bootstrap failures.

### Pool Initialization Pattern

```rust
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

pub async fn init_pool(config: &Config) -> Result<PgPool, StorageError> {
    PgPoolOptions::new()
        .min_connections(config.db_pool_min)
        .max_connections(config.db_pool_max)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .connect(&config.database_url)
        .await
        .map_err(|e| StorageError::ConnectionFailed(
            format!("failed to connect to database: {e}")
        ))
}
```

**Critical:** Do NOT log the raw `DATABASE_URL` on failure -- it contains the password. Sanitize by removing the password portion or use a generic message with the sqlx error (which does not include the connection string).

### System Table DDL

```sql
CREATE TABLE IF NOT EXISTS "programs" (
    "program_id"   VARCHAR(44) PRIMARY KEY,
    "program_name" TEXT NOT NULL,
    "schema_name"  TEXT NOT NULL UNIQUE,
    "idl_hash"     VARCHAR(64),
    "idl_source"   TEXT,
    "status"       TEXT NOT NULL DEFAULT 'initializing',
    "created_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    "updated_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS "indexer_state" (
    "program_id"          VARCHAR(44) PRIMARY KEY REFERENCES "programs"("program_id"),
    "status"              TEXT NOT NULL,
    "last_processed_slot" BIGINT,
    "last_heartbeat"      TIMESTAMPTZ,
    "error_message"       TEXT,
    "total_instructions"  BIGINT DEFAULT 0,
    "total_accounts"      BIGINT DEFAULT 0
);
```

Column notes:

- `program_id` — Solana base58 pubkey, max 44 chars
- `schema_name` — `UNIQUE` prevents per-program schema collisions
- `idl_hash` — SHA-256 hex digest, always 64 chars
- `idl_source` — valid values: `'onchain'`, `'file'`, `'bundled'`, `'manual'`
- `programs.status` — valid values: `'initializing'`, `'backfilling'`, `'realtime'`, `'paused'`, `'error'`

Execute via `sqlx::raw_sql()`:

```rust
pub async fn bootstrap_system_tables(pool: &PgPool) -> Result<(), StorageError> {
    let ddl = r#"
        CREATE TABLE IF NOT EXISTS "programs" (
            "program_id"   VARCHAR(44) PRIMARY KEY,
            "program_name" TEXT NOT NULL,
            "schema_name"  TEXT NOT NULL UNIQUE,
            "idl_hash"     VARCHAR(64),
            "idl_source"   TEXT,
            "status"       TEXT NOT NULL DEFAULT 'initializing',
            "created_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            "updated_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS "indexer_state" (
            "program_id"          VARCHAR(44) PRIMARY KEY REFERENCES "programs"("program_id"),
            "status"              TEXT NOT NULL,
            "last_processed_slot" BIGINT,
            "last_heartbeat"      TIMESTAMPTZ,
            "error_message"       TEXT,
            "total_instructions"  BIGINT DEFAULT 0,
            "total_accounts"      BIGINT DEFAULT 0
        );
    "#;

    sqlx::raw_sql(ddl)
        .execute(pool)
        .await
        .map_err(|e| StorageError::DdlFailed(format!("system table bootstrap failed: {e}")))?;

    Ok(())
}
```

**Key points:**

- Use `sqlx::raw_sql()` NOT `sqlx::query()` -- raw_sql bypasses prepared statements, which is required for DDL
- Both statements in one `raw_sql` call is fine -- PostgreSQL executes them sequentially
- `IF NOT EXISTS` makes this idempotent -- safe to call on every startup
- The `REFERENCES programs(program_id)` FK means `programs` MUST be created first (order matters in DDL)

### main.rs Integration

Story 1.1 created `main.rs` with config parse and tracing init. This story adds DB pool + bootstrap after tracing init. Add `use tracing::error;` to main.rs imports alongside existing `use tracing::info;`:

```rust
// After tracing init (from Story 1.1):

info!("connecting to database");
let pool = solarix::storage::init_pool(&config).await.map_err(|e| {
    error!(error = %e, "failed to initialize database pool");
    e
})?;

info!("bootstrapping system tables");
solarix::storage::bootstrap_system_tables(&pool).await.map_err(|e| {
    error!(error = %e, "failed to bootstrap system tables");
    e
})?;

info!("database ready");

// Future stories: pipeline, API server
```

The `?` operator works because `main` returns `Result<(), Box<dyn std::error::Error>>` and `StorageError` implements `std::error::Error` via `thiserror`.

### Files Modified by This Story

| File                 | Action | Purpose                                                        |
| -------------------- | ------ | -------------------------------------------------------------- |
| `src/storage/mod.rs` | Modify | Replace stubs with `init_pool()` + `bootstrap_system_tables()` |
| `src/main.rs`        | Modify | Add pool init + bootstrap calls after tracing init             |

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` -- use `?` with `map_err` to `StorageError`
- NO `println!` -- use `tracing::info!`, `tracing::error!`
- NO `sqlx::query!()` compile-time macros -- use `sqlx::raw_sql()` for DDL
- NO `CREATE TABLE` without `IF NOT EXISTS`
- NO logging the raw DATABASE_URL (contains password)
- NO separate error.rs file -- `StorageError` stays in `storage/mod.rs`

### Required Imports for `src/storage/mod.rs`

```rust
// std library
use std::time::Duration;

// external crates
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::info;

// internal crate
use crate::config::Config;
```

For `main.rs`, add `use tracing::error;` to existing imports.

### Testing Notes

The bootstrap test is an **integration test** (requires running PostgreSQL). Place in `tests/bootstrap_test.rs`, not in `#[cfg(test)] mod tests`.

```rust
// tests/bootstrap_test.rs
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn bootstrap_is_idempotent() {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://solarix:solarix@localhost:5432/solarix".to_string());
    let pool = PgPoolOptions::new().connect(&url).await.unwrap();

    // First call succeeds
    solarix::storage::bootstrap_system_tables(&pool).await.unwrap();
    // Second call also succeeds (idempotent)
    solarix::storage::bootstrap_system_tables(&pool).await.unwrap();
}
```

Run with `cargo test -- --ignored` when PostgreSQL is available. `cargo test` skips these by default.

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-1-project-foundation-first-boot.md#Story 1.2]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#System Tables]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Error Handling]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#SQL Patterns]
- [Source: _bmad-output/planning-artifacts/research/agent-2b-hybrid-storage-architecture.md#System Tables]
- [Source: _bmad-output/implementation-artifacts/1-1-project-scaffolding-and-configuration.md]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

None — clean implementation with no blockers.

### Completion Notes List

- Replaced `init_pool()` stub with real `PgPoolOptions`-based implementation accepting `&Config`, returning `PgPool`
- Pool configured with min/max connections, acquire timeout (5s), idle timeout (300s), max lifetime (1800s)
- Connection errors return `StorageError::ConnectionFailed` with sqlx error message (no raw URL leaked)
- Implemented `bootstrap_system_tables()` with DDL for `programs` and `indexer_state` tables via `sqlx::raw_sql()`
- All DDL uses `IF NOT EXISTS` for idempotent startup
- Updated `main.rs` to call `init_pool` and `bootstrap_system_tables` after tracing init, with error logging on failure
- Created integration test `tests/bootstrap_test.rs` with `#[ignore]` — tests idempotency and verifies tables via `information_schema`
- `cargo build`, `cargo clippy`, `cargo fmt -- --check` all pass
- Task 5 subtasks for running PostgreSQL left unchecked (requires running DB instance; test is `#[ignore]`)

### Change Log

- 2026-04-05: Story 1.2 implemented — database connection pool + system table bootstrap

### File List

- `src/storage/mod.rs` (modified) — `init_pool()` + `bootstrap_system_tables()` implementations
- `src/main.rs` (modified) — DB pool init + bootstrap calls after tracing init
- `tests/bootstrap_test.rs` (new) — integration test for DDL idempotency
