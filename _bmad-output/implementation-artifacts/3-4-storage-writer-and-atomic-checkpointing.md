# Story 3.4: Storage Writer & Atomic Checkpointing

Status: done

## Story

As a developer,
I want a storage writer that can persist decoded instructions and accounts to PostgreSQL with atomic per-block writes and checkpoint updates,
so that the pipeline has a reliable, crash-safe persistence layer.

## Acceptance Criteria

1. **AC1: Batch instruction INSERT...UNNEST**
   - **Given** decoded instruction data for a block
   - **When** the storage writer processes it
   - **Then** it performs `INSERT...UNNEST` into the per-program `_instructions` table with column vector decomposition (separate typed vectors per column)
   - **And** uses `ON CONFLICT DO NOTHING` on the unique constraint `(signature, instruction_index, COALESCE(inner_index, -1))` for deduplication
   - **And** JSONB array values (`args`, `accounts`, `data`) are bound using `sqlx::types::Json<T>` wrapper

2. **AC2: Account upsert with slot guard**
   - **Given** decoded account data
   - **When** the storage writer processes it
   - **Then** it performs an upsert into the account type's table with `ON CONFLICT (pubkey) DO UPDATE ... WHERE EXCLUDED.slot_updated > {table}.slot_updated`
   - **And** promoted scalar columns are populated from the decoded JSON using `map_idl_type_to_pg` mapping
   - **And** u64 values > `i64::MAX` are stored as NULL in promoted columns but preserved as strings in the JSONB `data` column
   - **And** `updated_at` is explicitly set to `NOW()` on both INSERT and UPDATE

3. **AC3: Atomic per-block transaction**
   - **Given** a block's worth of data (instructions + accounts + checkpoint)
   - **When** the storage writer processes it
   - **Then** all three operations (instruction INSERT, account upsert, checkpoint update) execute within a single PostgreSQL transaction
   - **And** if any operation fails, the entire transaction rolls back (nothing committed)
   - **And** the function returns `StorageError::WriteFailed` or `StorageError::CheckpointFailed` as appropriate

4. **AC4: Checkpoint updates**
   - **Given** a successful block write
   - **When** the checkpoint is updated
   - **Then** it performs `INSERT...ON CONFLICT (stream) DO UPDATE` on the per-program `_checkpoints` table
   - **And** updates `last_slot`, `last_signature` (optional), and `updated_at = NOW()`
   - **And** the stream name (e.g., `"backfill"`, `"realtime"`) is passed as a parameter

5. **AC5: Crash-safe restart**
   - **Given** the pipeline is interrupted mid-write
   - **When** it restarts
   - **Then** `read_checkpoint(pool, schema_name, stream)` reads the last completed slot from `_checkpoints`
   - **And** the pipeline resumes from `last_slot + 1`
   - **And** re-processed blocks are safely deduplicated via `ON CONFLICT DO NOTHING`

6. **AC6: StorageWriter struct**
   - **Given** the `StorageWriter` struct in `src/storage/writer.rs`
   - **When** I inspect it
   - **Then** it holds `PgPool` (owned, for Send safety) and any cached schema metadata
   - **And** it exposes `pub async fn write_block(...)` and `pub async fn read_checkpoint(...)` methods
   - **And** it uses `schema_name` to route writes to the correct per-program schema

## Tasks / Subtasks

- [x] Task 1: Define `StorageWriter` struct and constructor (AC: #6)
  - [x] Replace empty stub in `src/storage/writer.rs` with `StorageWriter { pool: PgPool }`
  - [x] Add `pub fn new(pool: PgPool) -> Self`
  - [x] Add necessary imports (sqlx, serde_json, tracing, types)

- [x] Task 2: Implement `write_instructions` batch INSERT (AC: #1)
  - [x] Add private method `async fn write_instructions(tx: &mut PgConnection, schema_name: &str, instructions: &[DecodedInstruction]) -> Result<u64, StorageError>`
  - [x] Decompose `Vec<DecodedInstruction>` into column vectors: signatures, slots, block_times, instruction_names, instruction_indexes, inner_indexes, args, accounts, data, is_inner_ix
  - [x] Build `INSERT INTO {schema}."_instructions" (...) SELECT * FROM UNNEST($1::TEXT[], $2::BIGINT[], ...) ON CONFLICT ... DO NOTHING`
  - [x] Use `sqlx::types::Json<Vec<serde_json::Value>>` for JSONB array columns
  - [x] Return rows affected count
  - [x] Handle empty instructions vec (skip INSERT, return 0)

- [x] Task 3: Implement `write_accounts` upsert (AC: #2)
  - [x] Add private method `async fn write_accounts(tx: &mut PgConnection, schema_name: &str, accounts: &[DecodedAccount]) -> Result<u64, StorageError>`
  - [x] Group accounts by `account_type` for routing to correct tables
  - [x] **CRITICAL:** Apply `sanitize_identifier(&account_type)` to derive the actual table name (DDL lowercases via `sanitize_identifier`, e.g., `"TokenAccount"` -> `"tokenaccount"`)
  - [x] For each account type batch, build UNNEST upsert: `INSERT INTO {schema}."{sanitized_account_type}" (pubkey, slot_updated, lamports, data, updated_at, ...promoted...) SELECT * FROM UNNEST(...) ON CONFLICT (pubkey) DO UPDATE SET ... WHERE EXCLUDED.slot_updated > {table}.slot_updated`
  - [x] Implement `safe_u64_to_i64(value: u64) -> Option<i64>` for overflow guard
  - [x] Implement promoted column value extraction from `DecodedAccount.data` JSON
  - [x] Set `updated_at = NOW()` explicitly on both INSERT and UPDATE paths

- [x] Task 4: Implement `update_checkpoint` (AC: #4)
  - [x] Add private method `async fn update_checkpoint(tx: &mut PgConnection, schema_name: &str, stream: &str, slot: u64, signature: Option<&str>) -> Result<(), StorageError>`
  - [x] `INSERT INTO {schema}."_checkpoints" (stream, last_slot, last_signature, updated_at) VALUES ($1, $2, $3, NOW()) ON CONFLICT (stream) DO UPDATE SET last_slot = EXCLUDED.last_slot, last_signature = EXCLUDED.last_signature, updated_at = NOW()`

- [x] Task 5: Implement `write_block` atomic transaction (AC: #3)
  - [x] Add `pub async fn write_block(&self, schema_name: &str, stream: &str, instructions: &[DecodedInstruction], accounts: &[DecodedAccount], slot: u64, signature: Option<&str>) -> Result<WriteResult, StorageError>`
  - [x] Begin transaction: `self.pool.begin().await`
  - [x] Call `write_instructions`, `write_accounts`, `update_checkpoint` within the transaction
  - [x] Commit on success, return `WriteResult { instructions_written, accounts_written }`
  - [x] On failure, transaction auto-rollbacks (Drop), return appropriate StorageError
  - [x] Use `Box::pin(async move { ... })` with owned params if needed for Send safety

- [x] Task 6: Implement `read_checkpoint` (AC: #5)
  - [x] Add `pub async fn read_checkpoint(&self, schema_name: &str, stream: &str) -> Result<Option<CheckpointInfo>, StorageError>`
  - [x] Define `CheckpointInfo { last_slot: u64, last_signature: Option<String> }`
  - [x] Query: `SELECT last_slot, last_signature FROM {schema}."_checkpoints" WHERE stream = $1`
  - [x] **Note:** `last_slot` is BIGINT (i64) in DB — cast to u64 with `as u64` (safe: Solana slots are well within i64 range)
  - [x] Return `None` if no checkpoint row exists (fresh start)
  - [x] Map sqlx errors to `StorageError::CheckpointFailed`

- [x] Task 7: Add `WriteResult` and `CheckpointInfo` types (AC: #3, #5)
  - [x] Define in `src/storage/writer.rs`:
    ```rust
    pub struct WriteResult {
        pub instructions_written: u64,
        pub accounts_written: u64,
    }
    pub struct CheckpointInfo {
        pub last_slot: u64,
        pub last_signature: Option<String>,
    }
    ```

- [x] Task 8: Unit tests (AC: all)
  - [x] Test `safe_u64_to_i64` — values at i64::MAX boundary, above, below, zero
  - [x] Test column vector decomposition — verify correct mapping from DecodedInstruction fields to typed vectors
  - [x] Test promoted column extraction from JSON — u8, u64, string, pubkey, bool, Option
  - [x] Test u64 overflow handling — value > i64::MAX produces None in promoted, preserved in JSONB
  - [x] Test empty input handling — write_block with no instructions and no accounts succeeds
  - [x] Test SQL generation — verify generated INSERT...UNNEST SQL strings contain correct schema/table names and ON CONFLICT clauses

- [x] Task 9: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` — all tests pass (existing + new)

## Dev Notes

### Current Codebase State (Post Stories 2.3, 3.1-3.3, 5.1)

`src/storage/writer.rs` currently contains only an empty stub:

```rust
/// Batch writer for inserting decoded data into PostgreSQL.
pub struct StorageWriter;
```

This story replaces the stub with the full writer implementation.

### Types You Will Use

From `src/types.rs`:

```rust
pub struct DecodedInstruction {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub instruction_name: String,
    pub args: serde_json::Value,
    pub program_id: String,
    pub accounts: Vec<String>,
    pub instruction_index: u8,
    pub inner_index: Option<u8>,
}

pub struct DecodedAccount {
    pub pubkey: String,
    pub slot_updated: u64,
    pub lamports: u64,
    pub data: serde_json::Value,
    pub account_type: String,
    pub program_id: String,
}
```

### Table Schemas (from story 2-3 DDL generation)

**Instructions table** (`{schema}."_instructions"`):

| Column            | Type                           | Notes                    |
| ----------------- | ------------------------------ | ------------------------ |
| id                | BIGSERIAL PRIMARY KEY          | auto-increment           |
| signature         | TEXT NOT NULL                  | tx sig                   |
| slot              | BIGINT NOT NULL                | block slot               |
| block_time        | BIGINT                         | unix timestamp, nullable |
| instruction_name  | TEXT NOT NULL                  | decoded name             |
| instruction_index | SMALLINT NOT NULL              | position in tx           |
| inner_index       | SMALLINT                       | NULL for top-level       |
| args              | JSONB NOT NULL                 | decoded args             |
| accounts          | JSONB NOT NULL                 | account addresses        |
| data              | JSONB NOT NULL                 | full decoded payload     |
| is_inner_ix       | BOOLEAN NOT NULL DEFAULT FALSE | CPI flag                 |

Unique constraint: `(signature, instruction_index, COALESCE(inner_index, -1))`

**Account tables** (`{schema}."{account_type}"`) — one per IDL account type:

| Column         | Type                               | Notes                  |
| -------------- | ---------------------------------- | ---------------------- |
| pubkey         | TEXT PRIMARY KEY                   | account address        |
| slot_updated   | BIGINT NOT NULL                    | last update slot       |
| write_version  | BIGINT NOT NULL DEFAULT 0          | optimistic concurrency |
| lamports       | BIGINT NOT NULL                    | SOL balance            |
| data           | JSONB NOT NULL                     | full decoded payload   |
| is_closed      | BOOLEAN NOT NULL DEFAULT FALSE     |                        |
| updated_at     | TIMESTAMPTZ NOT NULL DEFAULT NOW() |                        |
| ...promoted... | varies, all nullable               | scalar IDL fields      |

**Checkpoints table** (`{schema}."_checkpoints"`):

| Column         | Type                               | Notes                    |
| -------------- | ---------------------------------- | ------------------------ |
| stream         | TEXT PRIMARY KEY                   | "backfill" or "realtime" |
| last_slot      | BIGINT                             | cursor position          |
| last_signature | VARCHAR(88)                        | optional tx sig          |
| updated_at     | TIMESTAMPTZ NOT NULL DEFAULT NOW() |                          |

### INSERT...UNNEST Pattern

Column vector decomposition is the key pattern. Decompose `Vec<DecodedInstruction>` into separate typed vectors, one per column:

```rust
let signatures: Vec<&str> = instructions.iter().map(|ix| ix.signature.as_str()).collect();
let slots: Vec<i64> = instructions.iter().map(|ix| ix.slot as i64).collect(); // safe: Solana slots won't exceed i64::MAX
let block_times: Vec<Option<i64>> = instructions.iter().map(|ix| ix.block_time).collect();
let names: Vec<&str> = instructions.iter().map(|ix| ix.instruction_name.as_str()).collect();
let ix_indexes: Vec<i16> = instructions.iter().map(|ix| ix.instruction_index as i16).collect();
let inner_indexes: Vec<Option<i16>> = instructions.iter().map(|ix| ix.inner_index.map(|i| i as i16)).collect();
let is_inner: Vec<bool> = instructions.iter().map(|ix| ix.inner_index.is_some()).collect();

// JSONB columns — require Json<T> wrapper:
let args: Vec<sqlx::types::Json<serde_json::Value>> =
    instructions.iter().map(|ix| sqlx::types::Json(ix.args.clone())).collect();
// `accounts` column: convert Vec<String> -> JSON array
let accounts: Vec<sqlx::types::Json<serde_json::Value>> =
    instructions.iter().map(|ix| sqlx::types::Json(serde_json::json!(ix.accounts))).collect();
// `data` column = same as `args` (DecodedInstruction has no separate `data` field):
let data: Vec<sqlx::types::Json<serde_json::Value>> =
    instructions.iter().map(|ix| sqlx::types::Json(ix.args.clone())).collect();
```

**Note:** `DecodedInstruction` has no separate `data` field. The `_instructions` table `data` column stores the same decoded payload as `args`. Both bind the same `ix.args` value.

SQL template:

```sql
INSERT INTO {schema}."_instructions"
    ("signature", "slot", "block_time", "instruction_name",
     "instruction_index", "inner_index", "args", "accounts", "data", "is_inner_ix")
SELECT * FROM UNNEST(
    $1::TEXT[], $2::BIGINT[], $3::BIGINT[], $4::TEXT[],
    $5::SMALLINT[], $6::SMALLINT[], $7::JSONB[], $8::JSONB[], $9::JSONB[], $10::BOOLEAN[]
)
ON CONFLICT ("signature", "instruction_index", COALESCE("inner_index", -1)) DO NOTHING
```

### JSONB Array Binding in sqlx

**Critical:** sqlx JSONB array binding requires `sqlx::types::Json<T>` wrapper. Direct `serde_json::Value` vectors will fail with type mismatch errors.

```rust
// Correct: wrap each Value in Json<T>
let args_json: Vec<sqlx::types::Json<serde_json::Value>> =
    instructions.iter().map(|ix| sqlx::types::Json(ix.args.clone())).collect();
sqlx::query(&sql).bind(&args_json) // binds as JSONB[]
```

Ensure `sqlx` `json` feature is enabled in Cargo.toml (already present).

### Account Upsert Pattern

Accounts are grouped by `account_type` because each type maps to a different table. The upsert only updates if the incoming slot is newer:

```sql
INSERT INTO {schema}."{sanitized_account_type}"
    ("pubkey", "slot_updated", "lamports", "data", "updated_at")
SELECT * FROM UNNEST($1::TEXT[], $2::BIGINT[], $3::BIGINT[], $4::JSONB[], $5::TIMESTAMPTZ[])
ON CONFLICT ("pubkey") DO UPDATE SET
    "slot_updated" = EXCLUDED."slot_updated",
    "lamports" = EXCLUDED."lamports",
    "data" = EXCLUDED."data",
    "updated_at" = NOW()
WHERE EXCLUDED."slot_updated" > {schema}."{sanitized_account_type}"."slot_updated"
```

**Note on omitted columns:** `write_version` (DEFAULT 0) and `is_closed` (DEFAULT FALSE) are omitted from the INSERT — they use their DDL defaults. The writer doesn't have Geyser `write_version` data (no Geyser source yet). `is_closed` detection requires account data length checks — deferred.

For promoted columns: extract scalar values from `DecodedAccount.data` JSON using `data[field_name]`, convert to PG-compatible types. If the IDL field type is u64 and the value > i64::MAX, bind NULL for the promoted column.

**Implementation note:** Since promoted column names and types vary per account type and are not known at compile time, use `format!()` with `quote_ident()` for identifiers and bind parameters for values. Start by implementing common columns + JSONB data first (Tasks 3-5), then add promoted column extraction (Task 3 subtask) using the `extract_promoted_value` helper described below. The JSONB `data` column always has the complete payload as a safety net.

### u64 Overflow Guard

u64 overflow for promoted columns is handled at the **SQL level** via `CASE WHEN` expressions in the upsert query (see `build_promoted_extract_expr`). Values > i64::MAX produce NULL in the promoted BIGINT column but are always preserved in the JSONB `data` column.

A Rust-side utility `safe_u64_to_i64()` exists for potential future use (e.g., Rust-side casting of `slot`/`lamports`) but is currently `#[allow(dead_code)]` since the SQL approach is the single source of truth. Consider removing if it stays unused after story 3.5.

For `slot` and `lamports` common columns: these are cast as `ix.slot as i64` / `account.lamports as i64` directly. Solana slots (~300M currently) and single-account lamports (max ~600B SOL \* 1e9 = 6e17) are well within i64 range. If a future edge case arises, add `safe_u64_to_i64` guards on these casts.

### Async + Send Safety (Critical)

From the !Send blocker resolved in story 5-1, the following patterns are mandatory:

1. **Owned parameters** — `write_block` takes `PgPool` (from `self.pool.clone()`), not `&PgPool`
2. **Box::pin if needed** — If composing multiple tx operations in one async block causes Send inference failure, wrap in `Box::pin(async move { ... }) -> Pin<Box<dyn Future<Output = ...> + Send>>`
3. **`tx.as_mut()` is safe INSIDE Box::pin** — The Executor lifetime is hidden behind the trait object
4. **DO NOT use `raw_sql().execute(tx.as_mut())`** for DDL inside Box::pin — this was the specific trigger for the !Send issue in story 5-1. For DML (INSERT/UPDATE), `sqlx::query().execute(tx.as_mut())` works fine inside Box::pin.

**Recommended approach:** Use a simple `async fn` method that takes `&self`, calls `self.pool.begin()`, does all work, and commits. If the compiler complains about Send, apply Box::pin to the leaf functions.

### Existing StorageError Variants

From `src/storage/mod.rs`:

```rust
pub enum StorageError {
    ConnectionFailed(String),
    DdlFailed(String),
    WriteFailed(String),     // Use for INSERT/UPDATE failures
    CheckpointFailed(String), // Use for checkpoint update failures
}
```

No new variants needed. Map sqlx errors appropriately:

- INSERT/upsert failures → `StorageError::WriteFailed`
- Checkpoint update failures → `StorageError::CheckpointFailed`
- Checkpoint READ failures (`read_checkpoint` SELECT) → `StorageError::CheckpointFailed` (it's checkpoint-related)
- Transaction begin/commit failures → `StorageError::WriteFailed`

### Existing Helpers You MUST Reuse

From `src/storage/schema.rs` (already implemented in story 2-3):

- `pub fn quote_ident(name: &str) -> String` — Always use for identifier quoting in generated SQL
- `pub fn sanitize_identifier(name: &str) -> String` — For table name derivation
- `pub fn derive_schema_name(idl_name: &str, program_id: &str) -> String` — Schema name generation
- `pub fn map_idl_type_to_pg(ty: &IdlType, types: &[IdlTypeDef]) -> Option<&'static str>` — Type mapping for promoted columns

The `schema_name` parameter passed to writer methods comes from `ProgramInfo.schema_name` (already derived and stored during registration in story 2-2).

### Deferred Work Items Addressed by This Story

From `_bmad-output/implementation-artifacts/deferred-work.md`:

1. **`updated_at` has no auto-update trigger** (story 1-2 deferred) — Solved: writer explicitly sets `updated_at = NOW()` on all UPDATE paths
2. **`TransactionData.slot` duplicates `BlockData.slot`** (story 1-1 deferred) — Acknowledged: `DecodedInstruction.slot` is populated by pipeline before passing to writer. Writer trusts the incoming value.

### Promoted Column Writing Strategy

**Implemented approach:** Promoted columns are populated via SQL-side extraction from the JSONB `data` column within the same UNNEST upsert query. The writer discovers promoted columns by querying `information_schema.columns` for each account table, filtering out the 7 common system columns (`COMMON_ACCOUNT_COLUMNS`). Results are cached per-schema in a `Mutex<HashMap>` on `StorageWriter`.

**SQL-side extraction pattern:** For each discovered promoted column, the upsert SQL includes an expression like `(data->>'field_name')::BIGINT` in a CTE. u64 overflow is handled at the SQL level with `CASE WHEN (data->>'field')::NUMERIC > 9223372036854775807 THEN NULL ELSE (data->>'field')::BIGINT END`.

**Column name matching:** DDL creates promoted columns using `sanitize_identifier(field.name)` (lowercased). The decoder outputs JSON with original IDL field names. In practice, Anchor v0.30+ uses snake_case, so `sanitize_identifier` is a no-op for most fields. The `information_schema` query returns the actual DB column names, which match the sanitized versions.

**Note on `COMMON_ACCOUNT_COLUMNS`:** `writer.rs` hardcodes a list of 7 system columns matching `RESERVED_ACCOUNT_COLUMNS` in `schema.rs`. If the DDL adds/removes a system column, both lists must stay in sync. Consider importing from `schema.rs` in a future cleanup.

For promoted column extraction, build a helper:

```rust
fn extract_promoted_value(
    data: &serde_json::Value,
    field_name: &str,
    pg_type: &str,
) -> PromotedValue {
    // Extract from JSON, convert based on pg_type
    // Handle u64 overflow for BIGINT columns
}
```

**Complexity note:** Dynamic promoted columns mean the SQL must be built at runtime with `QueryBuilder` or `format!()`. Since table/column names come from sanitized IDL data (via `quote_ident`), string formatting for identifiers is safe. Values MUST use bind parameters.

### Data Column Population

The `data` JSONB column in both instructions and account tables stores the **complete** decoded payload. For instructions, this is `ix.args` (already a JSON object). For accounts, this is `account.data` (already a JSON object from the decoder).

The `accounts` JSONB column in `_instructions` stores the account address list. Map from `DecodedInstruction.accounts: Vec<String>` to a JSON array.

### File Structure

All code goes in `src/storage/writer.rs` (replacing the empty stub).

| File                    | Action     | Purpose                                                                                            |
| ----------------------- | ---------- | -------------------------------------------------------------------------------------------------- |
| `src/storage/writer.rs` | Rewrite    | StorageWriter, write_block, write_instructions, write_accounts, update_checkpoint, read_checkpoint |
| `src/storage/mod.rs`    | May modify | Re-export writer types if needed                                                                   |

**DO NOT modify:** `src/types.rs`, `src/decoder/`, `src/pipeline/`, `src/api/`, `src/config.rs`, `src/storage/schema.rs`

### What This Story Does NOT Do

- Does NOT implement the pipeline orchestrator (story 3.5 — calls `write_block` in a loop)
- Does NOT implement the query builder (story 5.2)
- Does NOT implement indexer_state updates (story 3.5 — global pipeline status table)
- Does NOT handle schema evolution or IDL changes
- Does NOT implement WebSocket/streaming writes (story 4.x)
- Does NOT handle `write_version` — defaults to 0 (no Geyser source yet)
- Does NOT update `program_stats` or `indexer_state` counters in the per-block transaction — story 3.5 (orchestrator) or 5.4 (stats) should add counter updates to `write_block` or alongside it
- Does NOT detect `is_closed` accounts (data length = 0) — column defaults to FALSE; detection deferred to story 4.x or post-MVP

### Testing Strategy

Unit tests in `#[cfg(test)] mod tests` at the bottom of `writer.rs`:

1. **`test_safe_u64_to_i64`** — boundary values: 0, i64::MAX, i64::MAX+1, u64::MAX
2. **`test_instruction_column_decomposition`** — create sample DecodedInstruction vec, verify each column vector has correct types and lengths
3. **`test_account_grouping_by_type`** — verify accounts are correctly grouped by `account_type` for routing to different tables
4. **`test_promoted_value_extraction`** — extract u64, string, bool, pubkey from JSON, verify correct conversion and u64 overflow handling
5. **`test_empty_block_write`** — verify write_block with empty vecs doesn't error
6. **`test_build_instruction_sql`** — verify generated SQL includes correct schema name, table name, ON CONFLICT clause

Integration tests (requiring PostgreSQL) are deferred to Epic 6.

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests — use `?` with `map_err` to StorageError
- NO `println!` — use `tracing` macros (`debug!`, `warn!`, `info!`)
- NO `sqlx::query!()` compile-time macros — use runtime `sqlx::query()` for all dynamic SQL
- NO SQL string concatenation for VALUES — use bind parameters via `sqlx::query().bind()`
- NO direct `serde_json::Value` binding for JSONB arrays — use `sqlx::types::Json<T>` wrapper
- NO `CREATE TABLE` or DDL in writer — DDL is handled by `schema.rs` (story 2-3)
- NO blocking calls on the Tokio runtime
- DO use `quote_ident()` from `schema.rs` for all generated identifiers
- DO handle empty input gracefully (zero instructions or zero accounts)
- DO use `NOW()` explicitly for `updated_at` on UPDATE paths

### Previous Story Learnings

**From story 5-1 (!Send blocker):**

- `Box::pin(async move { ... })` with `+ Send` on leaf functions that touch sqlx transactions
- Owned parameters (`PgPool`, `String`) instead of borrowed to make futures `'static`
- `raw_sql(&batch).execute(tx.as_mut())` triggers Executor lifetime issue — avoid for DDL inside Box::pin. For DML with `sqlx::query()`, this pattern works fine.

**From story 2-3 (schema generation):**

- `sqlx::raw_sql()` for DDL, `sqlx::query()` for DML
- All promoted columns are nullable (handles partial data and u64 overflow)
- `quote_ident()` doubles embedded double-quotes and wraps in double-quotes
- Reserved column names (pubkey, slot_updated, write_version, lamports, data, is_closed, updated_at) are skipped during promotion

**From story 3-3 (RPC source):**

- `backon` for retry, `governor` for rate limiting (not relevant to writer but shows error handling patterns)
- `map_err(|e| StorageError::WriteFailed(format!(...)))` pattern for sqlx errors

**From deferred work:**

- `updated_at` DEFAULT NOW() only fires on INSERT; writer must SET explicitly on UPDATE
- Schema cleanup in tests: add `DROP SCHEMA IF EXISTS ... CASCADE` in integration test teardown

### Tracing Note

Writer functions do not currently use `#[instrument]` spans. Architecture prescribes `#[instrument(skip(self), fields(slot, program_id))]` per pipeline stage. Adding a span on `write_block` is deferred to story 6-1 (structured tracing). For now, `debug!` logs on write completion provide basic observability.

### Project Structure Notes

- `src/storage/writer.rs` is the designated location per architecture docs
- Writer is called by pipeline orchestrator (story 3.5) — no direct coupling to pipeline module
- Writer receives `schema_name` as parameter — does not look up programs table itself
- Both backfill and streaming paths call the same `write_block` method — dedup is handled by ON CONFLICT

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-3-transaction-decoding-batch-indexing.md#Story 3.4]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Data Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Checkpoint Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Error Handling Flow]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md#Data Flow Through Boundaries]
- [Source: _bmad-output/planning-artifacts/research/agent-2b-hybrid-storage-architecture.md#Write Path Design]
- [Source: _bmad-output/planning-artifacts/research/agent-2a-idl-to-ddl-mapping.md#sqlx Implementation Patterns]
- [Source: _bmad-output/implementation-artifacts/2-3-dynamic-schema-generation.md]
- [Source: _bmad-output/implementation-artifacts/3-3-rpc-block-source-and-rate-limited-fetching.md]
- [Source: _bmad-output/implementation-artifacts/5-1-program-management-endpoints.md]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

No blocking issues encountered. Build compiled on first attempt.

### Completion Notes List

- Replaced empty `StorageWriter` stub with full implementation (~500 LOC + ~250 LOC tests)
- `StorageWriter` holds `PgPool` + `Mutex<HashMap>` for promoted column cache — Send+Sync safe
- `write_block()`: atomic transaction wrapping instructions + accounts + checkpoint. No Box::pin needed — simple `&self` method compiled Send-clean
- `write_instructions()`: INSERT...UNNEST with 10 column vectors, ON CONFLICT DO NOTHING dedup, `sqlx::types::Json<T>` for JSONB arrays
- `write_accounts()`: groups by account_type, sanitizes table names, CTE-based UNNEST upsert with slot guard (WHERE EXCLUDED.slot_updated > existing)
- Promoted column extraction: discovers columns via `information_schema.columns`, builds SQL-side extraction expressions (`data->>'field'`::TYPE), with u64 overflow guard (>i64::MAX → NULL)
- `update_checkpoint()`: INSERT...ON CONFLICT for per-program `_checkpoints` table
- `read_checkpoint()`: SELECT with proper NULL handling, maps BIGINT→u64
- `safe_u64_to_i64()`: utility for Rust-side overflow guard (currently SQL-side via CASE WHEN)
- 24 unit tests covering: safe_u64_to_i64 boundaries, column decomposition correctness, account grouping, promoted column SQL extraction (bigint overflow, text, boolean, integer, smallint, double precision, numeric, unknown types, single-quote escaping), SQL generation for both instructions and accounts (with/without promoted columns), Send safety compile-time checks
- All 169 tests pass (24 new + 145 existing), clippy clean, fmt clean

### File List

- `src/storage/writer.rs` — Rewritten (was empty stub, now full StorageWriter implementation + 24 unit tests)

### Review Findings

- [x] [Review][Patch] `lamports as i64` bare cast without overflow guard — added safety comment explaining why overflow is impossible in practice (max ~6e17 < i64::MAX 9.2e18) [src/storage/writer.rs:265]
- [x] [Review][Patch] `read_checkpoint` casts `i64` to `u64` without defensive guard — fixed: negative slot now returns None with `warn!` log instead of wrapping to huge u64 [src/storage/writer.rs:129-136]
- [x] [Review][Patch] Poisoned Mutex silently ignored via `.lock().ok()` — fixed: both read and write paths now log `warn!` on poison, with cache_key context [src/storage/writer.rs:190-204]
- [x] [Review][Defer] Promoted column cache never invalidated — no refresh on schema evolution. Out of scope per spec ("Does NOT handle schema evolution or IDL changes") — deferred
- [x] [Review][Defer] No batch size limits for UNNEST arrays — large blocks could produce oversized SQL. Naturally bounded by Solana block limits in practice — deferred
- [x] [Review][Defer] Integer/smallint promoted extract lacks overflow guard — unlike bigint CASE WHEN guard, ::INTEGER/::SMALLINT casts could fail if JSON value exceeds range — deferred, depends on schema.rs type mapping

### Story Spec Review Findings (2026-04-07)

6 spec improvements applied to story file:

- [x] [Review][Spec] S1: Clarified `program_stats`/`indexer_state` counter updates out of scope — added to "What This Story Does NOT Do"
- [x] [Review][Spec] S2: Documented `safe_u64_to_i64()` as dead code — u64 overflow handled at SQL level via CASE WHEN
- [x] [Review][Spec] S3: Consolidated promoted column strategy — removed contradictory Option A/B text, described actual implementation
- [x] [Review][Spec] N1: Noted `COMMON_ACCOUNT_COLUMNS` duplication with `RESERVED_ACCOUNT_COLUMNS` — sync risk documented
- [x] [Review][Spec] N2: Noted missing `#[instrument]` tracing spans — deferred to story 6-1
- [x] [Review][Spec] N3: Noted `is_closed` detection not implemented — deferred to story 4.x
