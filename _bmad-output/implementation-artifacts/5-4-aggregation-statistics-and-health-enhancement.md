# Story 5.4: Aggregation, Statistics & Health Enhancement

Status: review

## Story

As a user,
I want to see instruction call counts over time and program-level statistics,
so that I can understand program usage patterns and indexing progress.

## Acceptance Criteria

1. **AC1: Instruction count over time**
   - **Given** a `GET /api/programs/{id}/instructions/{name}/count` request with `interval` and optional `from`/`to` params
   - **When** the handler processes it
   - **Then** `interval` is validated against whitelist: `["minute", "hour", "day", "week", "month"]` (raw user input NEVER passed to SQL)
   - **And** SQL uses `date_trunc('interval', to_timestamp(block_time))` since `block_time` is stored as BIGINT Unix seconds
   - **And** results are grouped by truncated time bucket with count per bucket
   - **And** `from`/`to` are optional BIGINT Unix timestamps filtering `block_time`
   - **And** invalid interval returns HTTP 400 with `INVALID_VALUE` error code
   - **And** response: `{ "data": [{ "bucket": "2026-04-07T00:00:00Z", "count": 42 }, ...], "meta": { "program_id", "instruction", "interval", "query_time_ms" } }`

2. **AC2: Program statistics**
   - **Given** a `GET /api/programs/{id}/stats` request
   - **When** the handler processes it
   - **Then** it returns: `total_instructions`, `total_accounts` (from `indexer_state`), `first_seen_slot`, `last_seen_slot` (from `_instructions` table MIN/MAX), `instruction_counts` (per-name breakdown via GROUP BY)
   - **And** response: `{ "data": { "total_instructions": N, "total_accounts": N, "first_seen_slot": N, "last_seen_slot": N, "instruction_counts": { "swap": N, "transfer": N } }, "meta": { "program_id", "query_time_ms" } }`

3. **AC3: Enhanced health endpoint**
   - **Given** the `GET /health` endpoint
   - **When** the system is healthy
   - **Then** it returns HTTP 200 with: `status`, `database`, `uptime_seconds`, `version`, `programs` (array of per-program status objects from `indexer_state`)
   - **And** each program status includes: `program_id`, `status`, `last_processed_slot`, `last_heartbeat`, `total_instructions`, `total_accounts`
   - **And** when DB is unreachable, returns HTTP 503 with `status: "unhealthy"`

4. **AC4: Interval validation**
   - **Given** the instruction count endpoint
   - **When** `interval` is missing or not in `["minute", "hour", "day", "week", "month"]`
   - **Then** returns HTTP 400 with `{ "error": { "code": "INVALID_VALUE", "message": "..." } }`

## Tasks / Subtasks

- [x] Task 1: Add 2 new routes to axum Router (AC: #1, #2)
  - [x] Add `/{id}/instructions/{name}/count` route in `src/api/mod.rs`
  - [x] Add `/{id}/stats` route in `src/api/mod.rs`
- [x] Task 2: Implement `instruction_count` handler (AC: #1, #4)
  - [x] Validate program_id, look up IDL, validate instruction name exists
  - [x] Parse `interval` from query params, validate against whitelist
  - [x] Parse optional `from`/`to` as i64 Unix timestamps
  - [x] Build time-series aggregation query with `date_trunc` + GROUP BY
  - [x] Execute query, map rows to JSON array of `{ bucket, count }`
  - [x] Return response with timing metadata
- [x] Task 3: Implement `program_stats` handler (AC: #2)
  - [x] Validate program_id
  - [x] Query `indexer_state` for `total_instructions`, `total_accounts`
  - [x] Query `_instructions` table for `MIN(slot)`, `MAX(slot)`, `COUNT(*) GROUP BY instruction_name`
  - [x] Combine results into stats response
- [x] Task 4: Enhance `health` handler (AC: #3)
  - [x] Query `indexer_state` joined with `programs` for per-program status
  - [x] Build programs array with status, slot, heartbeat, counts
  - [x] Keep existing health fields (status, database, uptime_seconds, version)
- [x] Task 5: Unit tests (AC: all)
  - [x] Test interval validation (all 5 valid values, invalid values, missing)
  - [x] Test `from`/`to` parsing (valid i64, non-numeric, missing)
  - [x] Test response structure for all new endpoints
- [x] Task 6: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` all tests pass

## Dev Notes

### Current Codebase State (Post Story 5.3)

**`src/api/handlers.rs`** has 11 handlers: `health`, `register_program`, `list_programs`, `get_program`, `delete_program`, `list_instruction_types`, `query_instructions`, `list_account_types`, `query_accounts`, `get_account`, plus internal helpers. Add 2 new handlers + enhance `health`.

**`src/api/mod.rs`** has `ApiError` with 11 variants and `IntoResponse`. Router has 10 routes (4 program + 5 query + health).

**`src/storage/queries.rs`** has `build_query`, `build_query_base`, `append_order_and_limit`, `append_filter_clause`, `QueryTarget`.

**Total tests**: 214 pass, 3 ignored, clippy clean.

### Router Additions

Add to `src/api/mod.rs` inside the existing `router()` function, within the `program_routes` builder:

```rust
// Story 5.4 additions:
.route("/{id}/instructions/{name}/count", get(handlers::instruction_count))
.route("/{id}/stats", get(handlers::program_stats))
```

These go AFTER the existing `/{id}/instructions/{name}` route (axum matches most specific first).

### Handler: `instruction_count`

```rust
pub async fn instruction_count(
    State(state): State<Arc<AppState>>,
    Path((id, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> { ... }
```

**Interval validation** — whitelist, NEVER pass raw user input to SQL:

```rust
const VALID_INTERVALS: &[&str] = &["minute", "hour", "day", "week", "month"];

fn validate_interval(params: &HashMap<String, String>) -> Result<&'static str, ApiError> {
    let raw = params
        .get("interval")
        .ok_or_else(|| ApiError::InvalidValue("'interval' parameter is required".to_string()))?;
    VALID_INTERVALS
        .iter()
        .find(|&&v| v == raw)
        .copied()
        .ok_or_else(|| ApiError::InvalidValue(format!(
            "invalid interval '{}'. Must be one of: minute, hour, day, week, month",
            raw
        )))
}
```

**The interval string is selected from the whitelist** — the returned `&'static str` is safe to embed in SQL because it comes from compiled constants, NOT from user input.

**Time-series SQL** — `block_time` is stored as BIGINT Unix seconds:

```sql
SELECT
    date_trunc('day', to_timestamp("block_time")) AS bucket,
    COUNT(*) AS count
FROM {schema}."_instructions"
WHERE "instruction_name" = $1
  AND "block_time" IS NOT NULL
  [AND "block_time" >= $2]  -- optional from
  [AND "block_time" <= $3]  -- optional to
GROUP BY bucket
ORDER BY bucket ASC
```

Build with `QueryBuilder` to safely bind `instruction_name`, `from`, and `to` values. The `interval` is embedded as a string literal from the whitelist (NOT a bind parameter — `date_trunc` requires a string literal or a text expression, and bind parameters work fine here as text).

Actually, `date_trunc($1, ...)` with a bind parameter works in PostgreSQL — the first arg is `text`. So you CAN bind the interval:

```rust
qb.push("SELECT date_trunc(");
qb.push_bind(validated_interval.to_string()); // safe: from whitelist
qb.push(", to_timestamp(\"block_time\")) AS bucket, COUNT(*) AS count FROM ");
qb.push(format!("{}.{}", quote_ident(&schema_name), quote_ident("_instructions")));
qb.push(" WHERE \"instruction_name\" = ");
qb.push_bind(name.clone());
qb.push(" AND \"block_time\" IS NOT NULL");
```

**Optional from/to filtering**:

```rust
fn parse_optional_i64(params: &HashMap<String, String>, key: &str) -> Result<Option<i64>, ApiError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.parse::<i64>()
            .map(Some)
            .map_err(|_| ApiError::InvalidValue(format!("'{}' must be a Unix timestamp integer", key))),
    }
}
```

If `from` is present: `AND "block_time" >= $N`. If `to` is present: `AND "block_time" <= $N`.

**Row mapping**:

```rust
let bucket: chrono::DateTime<chrono::Utc> = row.get("bucket");
let count: i64 = row.get("count");
json!({ "bucket": bucket.to_rfc3339(), "count": count })
```

Note: `to_timestamp()` returns `TIMESTAMPTZ`, so `date_trunc()` returns `TIMESTAMPTZ` which sqlx maps to `chrono::DateTime<Utc>`.

### Handler: `program_stats`

```rust
pub async fn program_stats(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> { ... }
```

**Two queries in parallel** via `tokio::try_join!`:

Query 1 — from `indexer_state` (pre-computed counters):

```sql
SELECT "total_instructions", "total_accounts"
FROM "indexer_state"
WHERE "program_id" = $1
```

Query 2 — from `_instructions` table (aggregates):

```sql
SELECT
    MIN("slot") AS first_seen_slot,
    MAX("slot") AS last_seen_slot,
    "instruction_name",
    COUNT(*) AS count
FROM {schema}."_instructions"
GROUP BY "instruction_name"
```

Combine: `first_seen_slot` = MIN of all MINs, `last_seen_slot` = MAX of all MAXs, `instruction_counts` = name->count map.

If `indexer_state` row not found, the program isn't registered → `ProgramNotFound`. If `_instructions` has no rows, return zeroes.

**Schema lookup**: Reuse existing `get_schema_name()` helper.

### Handler: Enhanced `health`

Extend the existing `health` handler. Currently returns: `status`, `database`, `uptime_seconds`, `version`.

Add a `programs` array with per-program pipeline status:

```sql
SELECT p."program_id", p."status" AS program_status,
       i."status" AS pipeline_status, i."last_processed_slot",
       i."last_heartbeat", i."total_instructions", i."total_accounts"
FROM "programs" p
LEFT JOIN "indexer_state" i ON p."program_id" = i."program_id"
```

If DB query fails (connection issue), skip the programs array and return unhealthy status (existing behavior). Don't let the join query failure change the health endpoint's existing contract — wrap in a separate query with graceful fallback.

```rust
let programs_result = sqlx::query(...)
    .fetch_all(&state.pool)
    .await;

let programs = match programs_result {
    Ok(rows) => Some(rows.iter().map(|row| {
        json!({
            "program_id": row.get::<String, _>("program_id"),
            "status": row.get::<String, _>("pipeline_status"),
            "last_processed_slot": row.get::<Option<i64>, _>("last_processed_slot"),
            "last_heartbeat": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_heartbeat")
                .map(|t| t.to_rfc3339()),
            "total_instructions": row.get::<i64, _>("total_instructions"),
            "total_accounts": row.get::<i64, _>("total_accounts"),
        })
    }).collect::<Vec<_>>()),
    Err(_) => None,
};
```

Add `"programs": programs` to the existing health JSON response. When DB is down, `programs` will be `null` (not present).

### Existing Helpers to Reuse

| Helper                  | File              | Purpose                                       |
| ----------------------- | ----------------- | --------------------------------------------- |
| `validate_program_id()` | `handlers.rs:49`  | Validate base58 pubkey                        |
| `get_schema_name()`     | `handlers.rs:478` | Look up schema_name from programs table       |
| `clamp_limit()`         | `handlers.rs:417` | Not needed for this story (no pagination)     |
| `map_query_error()`     | `handlers.rs:464` | Map sqlx errors to ApiError (type cast → 400) |
| `quote_ident()`         | `schema.rs:71`    | Quote SQL identifiers                         |

### Database Tables Involved

**`indexer_state`** (public schema):

- `program_id` VARCHAR(44) PK
- `status` TEXT NOT NULL
- `last_processed_slot` BIGINT
- `last_heartbeat` TIMESTAMPTZ
- `error_message` TEXT
- `total_instructions` BIGINT NOT NULL DEFAULT 0
- `total_accounts` BIGINT NOT NULL DEFAULT 0

**`{schema}._instructions`** (per-program schema):

- `id` BIGSERIAL PK
- `signature` TEXT NOT NULL
- `slot` BIGINT NOT NULL
- `block_time` BIGINT (nullable — NULL if RPC doesn't return it)
- `instruction_name` TEXT NOT NULL
- `instruction_index` SMALLINT NOT NULL
- `inner_index` SMALLINT
- `args` JSONB NOT NULL
- `accounts` JSONB NOT NULL
- `data` JSONB NOT NULL
- `is_inner_ix` BOOLEAN NOT NULL DEFAULT FALSE

**Indexes on `_instructions`**: B-tree on `slot`, `signature`, `instruction_name`, `block_time`. The `block_time` index is critical for the time-series aggregation query.

**`programs`** (public schema):

- `program_id` VARCHAR(44) PK
- `program_name` TEXT NOT NULL
- `schema_name` TEXT NOT NULL UNIQUE
- `idl_hash` VARCHAR(64)
- `idl_source` TEXT
- `status` TEXT NOT NULL DEFAULT 'initializing'
- `created_at` TIMESTAMPTZ
- `updated_at` TIMESTAMPTZ

### What This Story Does NOT Do

- Does NOT implement `unique_signers` stat — would require a live `COUNT(DISTINCT ...)` across JSONB accounts data which is expensive and not indexed. Deferred.
- Does NOT add pre-computed counter tables — `indexer_state` already has `total_instructions`/`total_accounts` updated by the pipeline writer.
- Does NOT add pipeline lag calculation (would need current chain slot from RPC). The health endpoint exposes `last_processed_slot` and `last_heartbeat` — clients can compute lag.
- Does NOT implement 503 on "pipeline lag > 120s" — no mechanism to get current chain slot without an RPC call in the health endpoint (defeats fast health checks). Deferred to story 6.1 (observability).
- Does NOT add the epic's `_metadata` pre-computed stats — the current `_metadata` table only stores IDL-time data (program_id, idl_hash, account_types, instruction_types). Runtime stats are in `indexer_state` already.
- Does NOT modify the `_metadata` table schema or writer.

### Scope Simplifications vs Epic

The epic AC mentions reading stats from `_metadata` with pre-computed counters. In practice, `indexer_state` already has `total_instructions` and `total_accounts` (updated by `StorageWriter`). The per-instruction breakdown requires a live `GROUP BY` query on `_instructions` — this is fast with the existing `instruction_name` B-tree index for reasonable data volumes (bounty demo scale). No new pre-computed tables needed.

The enhanced health endpoint in the epic mentions checking pipeline lag > 120s and program error state > 5 minutes for 503 logic. These require either RPC calls (current chain slot) or time-based checks that belong in the observability story (6.1). This story adds per-program status data to health but keeps the 503 trigger as DB-connectivity-only.

### Anti-Patterns to Avoid

- **NEVER** embed raw `interval` param in SQL — always validate against whitelist first
- **NEVER** use `unwrap()` or `expect()` outside tests
- **NEVER** hold RwLock read guard across `.await` points (not needed for this story — handlers only touch DB, not registry)
- **NEVER** use string concatenation for user values — use `push_bind()`
- Table/schema names are derived from DB (sanitized at registration) — still use `quote_ident()` for defense in depth
- Use `map_query_error()` for all sqlx query results to get proper 400 on type cast failures

### Import Ordering Convention

```rust
// std library
use std::collections::HashMap;
use std::sync::Arc;

// external crates
use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;

// internal crate
use crate::storage::schema::quote_ident;
use super::{ApiError, AppState};
```

### Previous Story Intelligence

**From story 5.3:**

- `get_schema_name()` helper works and is tested — reuse for all per-program queries
- `map_query_error()` catches PostgreSQL type cast errors (22P02, 22003) → returns 400
- `clamp_limit()` / `clamp_offset()` pattern for param parsing — follow same pattern for `from`/`to`
- `instruction_row_to_json()` / `account_row_to_json()` — row mappers for reference
- `tokio::try_join!` for parallel queries (used in `query_accounts`) — reuse pattern
- RwLock read guard clone-then-drop pattern — use if accessing registry (only needed for instruction name validation)
- Cursor pagination NOT needed for this story (count returns aggregated buckets, stats returns single object)
- Total: 214 tests pass post 5.3

**From story 5.3 review — open patches (P1-P3):**

- P1: Cursor condition after ORDER BY — doesn't affect this story (no cursor pagination)
- P2: Type cast errors → 500 instead of 400 — FIXED by `map_query_error()` in last commit
- P3: Negative limit uses 1 instead of default — FIXED in last commit

**From story 5.1:**

- Handler pattern: `State(state): State<Arc<AppState>>` with `Path`/`Query` extractors
- Response envelope: `{ "data": ..., "meta": { ... } }` — be consistent
- `validate_program_id()` exists — reuse for all handlers

### Git Intelligence

Recent commits all compile cleanly (clippy/fmt). Last commit `eb988ce` applied review fixes for stories 4.1, 3.5, and 5.3. Codebase at 214 passing tests.

Files this story touches are stable on main — no pending merge conflicts expected.

### Dependencies

`chrono` is already in Cargo.toml (used by `get_program` handler for DateTime). No new crate dependencies needed.

### Files Created/Modified by This Story

| File                  | Action | Purpose                                                                        |
| --------------------- | ------ | ------------------------------------------------------------------------------ |
| `src/api/mod.rs`      | Modify | 2 new routes                                                                   |
| `src/api/handlers.rs` | Modify | 2 new handlers + enhanced health + interval validation + param parsing + tests |

Only 2 files touched. No new files. No storage module changes.

### Project Structure Notes

- All handlers live in `src/api/handlers.rs` — no per-endpoint files
- Helper functions (validation, parsing) are private to `handlers.rs`
- Config in `src/config.rs` — no new config fields needed
- `ApiError::InvalidValue` already exists for validation errors — reuse it

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-5-query-api-filtering.md#Story 5.4]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#API & Communication]
- [Source: _bmad-output/planning-artifacts/prd.md#API Surface — endpoints 10, 11, 12]
- [Source: _bmad-output/implementation-artifacts/5-3-instruction-and-account-query-endpoints.md]
- [Source: src/api/mod.rs — current ApiError enum, router, AppState]
- [Source: src/api/handlers.rs — existing handler patterns, health handler, helpers]
- [Source: src/storage/mod.rs — indexer_state table schema, programs table schema]
- [Source: src/storage/schema.rs — quote_ident, _instructions table structure]
- [Source: src/storage/queries.rs — build_query_base, QueryTarget, append_filter_clause]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

### Completion Notes List

- Implemented `instruction_count` handler with whitelist-validated interval, optional from/to Unix timestamp filtering, date_trunc time-series aggregation via QueryBuilder with push_bind (no raw SQL injection)
- Implemented `program_stats` handler with tokio::try_join! parallel queries to indexer_state + \_instructions, per-instruction breakdown via GROUP BY
- Enhanced `health` handler with per-program status array from programs LEFT JOIN indexer_state, graceful fallback to null when query fails
- Added `validate_interval()` helper returning &'static str from whitelist constant
- Added `parse_optional_i64()` helper for Unix timestamp params
- Added 18 new unit tests: interval validation (5 valid, missing, invalid, SQL injection), from/to parsing (missing, valid, negative, non-numeric, float), ApiError::InvalidValue response test
- All 232 tests pass (up from 214), 3 ignored, clippy clean, fmt clean
- Only 2 files modified: src/api/mod.rs (routes), src/api/handlers.rs (handlers + tests)
- No new dependencies, no new files, no storage module changes

### Change Log

- 2026-04-07: Implemented story 5.4 — 2 new handlers (instruction_count, program_stats), enhanced health endpoint, 18 new tests

### File List

- src/api/mod.rs (modified) — added 2 routes
- src/api/handlers.rs (modified) — added instruction_count, program_stats handlers, enhanced health, validate_interval, parse_optional_i64, 18 unit tests
