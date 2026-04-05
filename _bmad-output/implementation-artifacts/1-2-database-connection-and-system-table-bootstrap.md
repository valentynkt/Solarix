# Story 1.2: Database Connection & System Table Bootstrap

Status: ready-for-dev

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
   - **And** columns: `program_id TEXT PRIMARY KEY`, `program_name TEXT`, `schema_name TEXT`, `idl_hash TEXT`, `idl_source TEXT`, `status TEXT NOT NULL DEFAULT 'registered'`, `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`

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

- [ ] Task 1: Implement `init_pool()` in `src/storage/mod.rs` (AC: #1, #5)
  - [ ] Add `pub async fn init_pool(config: &Config) -> Result<PgPool, StorageError>`
  - [ ] Configure `PgPoolOptions` with min/max from config, acquire timeout 5s, idle timeout 300s, max lifetime 1800s
  - [ ] On connection failure, return `StorageError::ConnectionFailed` with sanitized URL (no password)
  - [ ] Log at `info!` level on successful connection with pool size details
- [ ] Task 2: Implement `bootstrap_system_tables()` in `src/storage/mod.rs` (AC: #2, #3, #4, #6)
  - [ ] Add `pub async fn bootstrap_system_tables(pool: &PgPool) -> Result<(), StorageError>`
  - [ ] Write DDL for `programs` table per AC2 columns, using `IF NOT EXISTS`
  - [ ] Write DDL for `indexer_state` table per AC3 columns, using `IF NOT EXISTS`
  - [ ] Execute both via `sqlx::raw_sql()` in sequence
  - [ ] On DDL failure, return `StorageError::DdlFailed` with context
  - [ ] Log at `info!` level on successful bootstrap
- [ ] Task 3: Update `main.rs` to call pool init and bootstrap (AC: #7)
  - [ ] After tracing init, call `storage::init_pool(&config).await`
  - [ ] On failure, log error and return early (non-zero exit)
  - [ ] Call `storage::bootstrap_system_tables(&pool).await`
  - [ ] On failure, log error and return early
  - [ ] Log success: "database connected, system tables bootstrapped"
- [ ] Task 4: Add unit test for DDL idempotency (AC: #6)
  - [ ] In `src/storage/mod.rs` `#[cfg(test)] mod tests`, add test that calls `bootstrap_system_tables` twice without error (requires running PostgreSQL)
- [ ] Task 5: Verify (AC: all)
  - [ ] `cargo build` compiles
  - [ ] `cargo clippy` passes
  - [ ] `cargo fmt -- --check` passes
  - [ ] With running PostgreSQL, `cargo run` connects and creates tables
  - [ ] Second `cargo run` succeeds (idempotent)

## Dev Notes

### Story 1.1 Dependency

Story 1.1 is being implemented in parallel. It creates:

- `src/storage/mod.rs` with `StorageError` enum stub and pool init placeholder
- `src/config.rs` with `Config` struct (including `database_url`, `db_pool_min`, `db_pool_max`)
- `src/main.rs` with clap parse and tracing init
- `src/lib.rs` with all module declarations

This story replaces the storage stub with real implementation. If 1.1 is not yet merged, the dev agent must either:

1. Wait for 1.1 to merge and start from that branch, OR
2. Create the same file structure independently (matching 1.1's patterns exactly)

The recommended approach: check if `src/storage/mod.rs` exists with the `StorageError` stub. If yes, extend it. If not, create it following the patterns from Story 1.1's dev notes.

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
CREATE TABLE IF NOT EXISTS programs (
    program_id TEXT PRIMARY KEY,
    program_name TEXT,
    schema_name TEXT,
    idl_hash TEXT,
    idl_source TEXT,
    status TEXT NOT NULL DEFAULT 'registered',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS indexer_state (
    program_id TEXT PRIMARY KEY REFERENCES programs(program_id),
    status TEXT NOT NULL,
    last_processed_slot BIGINT,
    last_heartbeat TIMESTAMPTZ,
    error_message TEXT,
    total_instructions BIGINT DEFAULT 0,
    total_accounts BIGINT DEFAULT 0
);
```

Execute via `sqlx::raw_sql()`:

```rust
pub async fn bootstrap_system_tables(pool: &PgPool) -> Result<(), StorageError> {
    let ddl = r#"
        CREATE TABLE IF NOT EXISTS programs (
            program_id TEXT PRIMARY KEY,
            program_name TEXT,
            schema_name TEXT,
            idl_hash TEXT,
            idl_source TEXT,
            status TEXT NOT NULL DEFAULT 'registered',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS indexer_state (
            program_id TEXT PRIMARY KEY REFERENCES programs(program_id),
            status TEXT NOT NULL,
            last_processed_slot BIGINT,
            last_heartbeat TIMESTAMPTZ,
            error_message TEXT,
            total_instructions BIGINT DEFAULT 0,
            total_accounts BIGINT DEFAULT 0
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

Story 1.1 creates `main.rs` with config parse and tracing init. This story adds DB pool + bootstrap after tracing init:

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

### Import Ordering Convention

```rust
// std library
use std::time::Duration;

// external crates
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::{error, info};

// internal crate
use crate::config::Config;
```

### Testing Notes

The bootstrap test requires a running PostgreSQL instance. Use `DATABASE_URL` from `.env` or environment. The test should:

1. Call `bootstrap_system_tables` once -- assert Ok
2. Call `bootstrap_system_tables` again -- assert Ok (idempotent)
3. Optionally verify tables exist via `information_schema.tables` query

Mark integration tests with `#[ignore]` if they require external services, so `cargo test` doesn't fail without PostgreSQL. Run with `cargo test -- --ignored` when PostgreSQL is available.

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-1-project-foundation-first-boot.md#Story 1.2]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#System Tables]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Error Handling]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#SQL Patterns]
- [Source: _bmad-output/planning-artifacts/research/agent-2b-hybrid-storage-architecture.md#System Tables]
- [Source: _bmad-output/implementation-artifacts/1-1-project-scaffolding-and-configuration.md]

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
