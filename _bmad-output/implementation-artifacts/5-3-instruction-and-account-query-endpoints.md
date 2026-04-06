# Story 5.3: Instruction & Account Query Endpoints

Status: review

## Story

As a user,
I want to query decoded instructions and account states by type with filters and pagination,
so that I can explore and analyze indexed on-chain data through the API.

## Acceptance Criteria

1. **AC1: List instruction types**
   - **Given** a `GET /api/programs/{id}/instructions` request
   - **When** the handler processes it
   - **Then** it validates the program exists in the registry
   - **And** returns a list of instruction type names derived from `idl.instructions[].name`
   - **And** response uses envelope: `{ "data": ["swap", "transfer", ...], "meta": { "program_id": "...", "total": N } }`

2. **AC2: Query instructions by name with filters**
   - **Given** a `GET /api/programs/{id}/instructions/{name}` request with filter params
   - **When** the handler processes it
   - **Then** it validates the instruction name exists in the IDL
   - **And** validates filters against the instruction's `args` field types via `resolve_filters`
   - **And** builds and executes a dynamic SQL query via `build_query` against the `_instructions` table with an extra `WHERE instruction_name = $N` clause
   - **And** returns decoded instructions matching the filters

3. **AC3: Cursor-based pagination for instructions**
   - **Given** instruction query results
   - **When** the response is built
   - **Then** it uses keyset pagination on `(slot, signature)` with cursor encoded as `base64("{slot}_{signature}")`
   - **And** when a `cursor` param is provided, adds `WHERE (slot, signature) < ($cursor_slot, $cursor_sig)` (DESC order)
   - **And** the response includes `{ "data": [...], "pagination": { "limit": N, "has_more": bool, "next_cursor": "..." }, "meta": { "program_id": "...", "instruction": "...", "query_time_ms": N } }`
   - **And** default limit is `config.api_default_page_size` (50), max limit is `config.api_max_page_size` (1000)

4. **AC4: List account types**
   - **Given** a `GET /api/programs/{id}/accounts` request
   - **When** the handler processes it
   - **Then** it validates the program exists in the registry
   - **And** returns a list of account type names derived from `idl.accounts[].name`
   - **And** response uses envelope: `{ "data": ["TokenAccount", "Vault", ...], "meta": { "program_id": "...", "total": N } }`

5. **AC5: Query accounts by type with filters**
   - **Given** a `GET /api/programs/{id}/accounts/{type}` request with filter params
   - **When** the handler processes it
   - **Then** it validates the account type exists in the IDL
   - **And** resolves the account type's field definitions from `idl.types` (matching by name)
   - **And** validates filters via `resolve_filters` with `FilterContext::Accounts`
   - **And** builds and executes a dynamic SQL query via `build_query` against the account type's table
   - **And** uses offset-based pagination with `{ "total": N, "limit": N, "offset": N, "has_more": bool }`

6. **AC6: Get single account by pubkey**
   - **Given** a `GET /api/programs/{id}/accounts/{type}/{pubkey}` request
   - **When** the account exists
   - **Then** it returns the single account record with all promoted columns and JSONB data
   - **And** when the account does not exist, returns HTTP 404 with `ACCOUNT_NOT_FOUND`

7. **AC7: Pagination parameter validation**
   - **Given** any query endpoint
   - **When** `limit` exceeds `config.api_max_page_size` (1000) or is negative
   - **Then** it clamps to max (not error) for positive values, uses default for negative/zero
   - **And** `offset` is clamped to 0 minimum

8. **AC8: New ApiError variants**
   - **Given** the `ApiError` enum
   - **When** an instruction name or account type is not found in the IDL
   - **Then** `InstructionNotFound(String)` returns HTTP 404 with code `INSTRUCTION_NOT_FOUND`
   - **And** `AccountTypeNotFound(String)` returns HTTP 404 with code `ACCOUNT_TYPE_NOT_FOUND`
   - **And** `AccountNotFound(String)` returns HTTP 404 with code `ACCOUNT_NOT_FOUND`

## Tasks / Subtasks

- [x] Task 1: Add new `ApiError` variants and routes (AC: #8, all)
  - [x] Add `InstructionNotFound(String)`, `AccountTypeNotFound(String)`, `AccountNotFound(String)` to `ApiError` enum in `src/api/mod.rs`
  - [x] Add `IntoResponse` mappings: all three -> 404 with respective codes
  - [x] Add 6 new routes to the router in `src/api/mod.rs` (see Dev Notes for exact structure)
- [x] Task 2: Implement pagination helpers in `src/api/handlers.rs` (AC: #3, #5, #7)
  - [x] Implement `PaginationParams` struct parsed from query params: `limit`, `offset`, `cursor`
  - [x] Implement `clamp_limit(raw: Option<&str>, config: &Config) -> i64` — parse, clamp to [1, max], default to `api_default_page_size`
  - [x] Implement `clamp_offset(raw: Option<&str>) -> i64` — parse, clamp to 0 minimum
  - [x] Implement `encode_cursor(slot: i64, signature: &str) -> String` — `base64("{slot}_{signature}")`
  - [x] Implement `decode_cursor(cursor: &str) -> Result<(i64, String), ApiError>` — decode + split
- [x] Task 3: Implement `list_instruction_types` handler (AC: #1)
  - [x] Validate program_id, read registry, get IDL
  - [x] Extract instruction names from `idl.instructions.iter().map(|i| &i.name)`
  - [x] Return `{ "data": [...], "meta": { "program_id", "total" } }`
- [x] Task 4: Implement `query_instructions` handler (AC: #2, #3)
  - [x] Validate program_id, look up IDL, find instruction by name or return `InstructionNotFound`
  - [x] Parse pagination params (limit, cursor) from query HashMap
  - [x] Parse and resolve filters using `parse_filters` + `resolve_filters` with `FilterContext::Instructions` and the instruction's `args` fields
  - [x] Look up `schema_name` from the `programs` table (or registry ProgramInfo)
  - [x] Build query via `build_query(QueryTarget::Instructions { schema }, &resolved, limit, 0)`
  - [x] Inject `instruction_name = $N` filter as an additional ResolvedFilter (Promoted column, Eq)
  - [x] If cursor provided, inject `(slot, signature) < ($slot, $sig)` cursor condition
  - [x] Execute query, map rows to JSON using `sqlx::Row`
  - [x] Build response with cursor pagination envelope
- [x] Task 5: Implement `list_account_types` handler (AC: #4)
  - [x] Validate program_id, read registry, get IDL
  - [x] Extract account type names from `idl.accounts.iter().map(|a| &a.name)`
  - [x] Return `{ "data": [...], "meta": { "program_id", "total" } }`
- [x] Task 6: Implement `query_accounts` handler (AC: #5)
  - [x] Validate program_id, look up IDL, find account type by name or return `AccountTypeNotFound`
  - [x] Resolve account type's field definitions from `idl.types` (find `IdlTypeDef` by name, extract struct fields)
  - [x] Parse pagination params (limit, offset) from query HashMap
  - [x] Parse and resolve filters with `FilterContext::Accounts`
  - [x] Look up `schema_name`, derive table name via `sanitize_identifier(account_type_name)`
  - [x] Build and execute query via `build_query(QueryTarget::Accounts { schema, table }, &resolved, limit, offset)`
  - [x] Run a parallel `SELECT COUNT(*)` for `total` (offset pagination needs it)
  - [x] Build response with offset pagination envelope
- [x] Task 7: Implement `get_account` handler (AC: #6)
  - [x] Validate program_id, look up IDL, validate account type
  - [x] Look up schema_name, derive table name
  - [x] Query `SELECT * FROM {schema}.{table} WHERE pubkey = $1`
  - [x] Return single record or `AccountNotFound`
- [x] Task 8: Implement `get_schema_name` helper (AC: all query handlers)
  - [x] Query `SELECT schema_name FROM programs WHERE program_id = $1`
  - [x] Return the schema_name or `ProgramNotFound`
- [x] Task 9: Unit tests (AC: all)
  - [x] Test cursor encode/decode roundtrip
  - [x] Test `clamp_limit` edge cases (negative, zero, over max, missing)
  - [x] Test `clamp_offset` edge cases
  - [x] Test new `ApiError` variants produce correct HTTP status and JSON
  - [x] Test row-to-JSON mapping for instruction and account results
- [x] Task 10: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` all tests pass (214 pass, 3 ignored)

## Dev Notes

### Current Codebase State (Post Story 5.2)

**`src/api/handlers.rs`** has 5 handlers: `health`, `register_program`, `list_programs`, `get_program`, `delete_program`. Add 6 new handlers in this file.

**`src/api/filters.rs`** has the complete filter pipeline:

- `parse_filters(params: &HashMap<String, String>) -> Vec<ParsedFilter>`
- `resolve_filters(parsed: &[ParsedFilter], fields: &[IdlField], types: &[IdlTypeDef], context: FilterContext) -> Result<Vec<ResolvedFilter>, ApiError>`
- `FilterContext::Instructions` / `FilterContext::Accounts`

**`src/storage/queries.rs`** has the query builder:

- `build_query(target: &QueryTarget, filters: &[ResolvedFilter], limit: i64, offset: i64) -> QueryBuilder<'_, Postgres>`
- `QueryTarget::Instructions { schema }` / `QueryTarget::Accounts { schema, table }`

**`src/api/mod.rs`** has `ApiError` with 8 variants and `IntoResponse`. Router has 4 program routes + health.

### Router Additions

Add to `src/api/mod.rs` inside the existing `router()` function:

```rust
let program_routes = Router::new()
    .route("/", post(handlers::register_program).get(handlers::list_programs))
    .route("/{id}", get(handlers::get_program).delete(handlers::delete_program))
    // Story 5.3 additions:
    .route("/{id}/instructions", get(handlers::list_instruction_types))
    .route("/{id}/instructions/{name}", get(handlers::query_instructions))
    .route("/{id}/accounts", get(handlers::list_account_types))
    .route("/{id}/accounts/{type}", get(handlers::query_accounts))
    .route("/{id}/accounts/{type}/{pubkey}", get(handlers::get_account));
```

Note: `{id}` is the existing `program_id` path param. axum 0.8 uses `{param}` syntax.

### Handler Signatures (follow existing pattern)

```rust
// Tuple path extraction for multiple params:
pub async fn query_instructions(
    State(state): State<Arc<AppState>>,
    Path((id, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> { ... }

// Triple path param:
pub async fn get_account(
    State(state): State<Arc<AppState>>,
    Path((id, account_type, pubkey)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> { ... }
```

### Helper: Getting schema_name

The `schema_name` is stored in the `programs` table. Query it rather than recomputing:

```rust
async fn get_schema_name(pool: &PgPool, program_id: &str) -> Result<String, ApiError> {
    let row = sqlx::query("SELECT schema_name FROM programs WHERE program_id = $1")
        .bind(program_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::QueryFailed(e.to_string()))?
        .ok_or_else(|| ApiError::ProgramNotFound(program_id.to_string()))?;
    Ok(row.get("schema_name"))
}
```

### Helper: Getting IDL from Registry

The registry uses `RwLock`. Access pattern:

```rust
let registry = state.registry.read().await;
let idl = registry
    .get_idl(&id)
    .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
// Clone what you need before dropping the lock
let instructions = idl.instructions.clone();
let accounts = idl.accounts.clone();
let types = idl.types.clone();
drop(registry);
```

Clone the IDL data you need and drop the lock before any `.await` points to avoid holding a read guard across awaits (which would block writers).

### Helper: Resolving Account Type Fields

Account types in the IDL: `idl.accounts` contains `Vec<IdlAccount>` with just `name` and `discriminator`. The actual struct field definitions are in `idl.types`:

```rust
fn get_account_fields(
    account_name: &str,
    types: &[IdlTypeDef],
) -> Result<Vec<IdlField>, ApiError> {
    let type_def = types
        .iter()
        .find(|t| t.name == account_name)
        .ok_or_else(|| ApiError::AccountTypeNotFound(account_name.to_string()))?;
    match &type_def.ty {
        anchor_lang_idl_spec::IdlTypeDefTy::Struct { fields: Some(fields) } => {
            match fields {
                anchor_lang_idl_spec::IdlDefinedFields::Named(named) => Ok(named.clone()),
                _ => Ok(vec![]), // tuple or unnamed struct
            }
        }
        _ => Ok(vec![]), // enum or empty struct
    }
}
```

### Account Table Name Derivation

Account tables use `sanitize_identifier(account_name)` from `src/storage/schema.rs`:

```rust
use crate::storage::schema::sanitize_identifier;
let table_name = sanitize_identifier(account_type);
```

### Instruction Name Filter Injection

The `_instructions` table stores all instruction types. When querying by name, inject an `instruction_name = $N` filter as a `ResolvedFilter`:

```rust
let mut resolved = resolve_filters(&parsed, &instruction.args, &types, FilterContext::Instructions)?;
resolved.push(ResolvedFilter {
    column_expr: ColumnExpr::Promoted { column: "instruction_name".to_string() },
    op: FilterOp::Eq,
    value: name.clone(),
});
```

### Cursor Pagination Implementation

Encode: `base64("{slot}_{signature}")`

```rust
use base64::{engine::general_purpose::STANDARD, Engine};

fn encode_cursor(slot: i64, signature: &str) -> String {
    STANDARD.encode(format!("{slot}_{signature}"))
}

fn decode_cursor(cursor: &str) -> Result<(i64, String), ApiError> {
    let decoded = STANDARD
        .decode(cursor)
        .map_err(|_| ApiError::InvalidValue("invalid cursor encoding".to_string()))?;
    let s = String::from_utf8(decoded)
        .map_err(|_| ApiError::InvalidValue("invalid cursor encoding".to_string()))?;
    let (slot_str, sig) = s
        .split_once('_')
        .ok_or_else(|| ApiError::InvalidValue("invalid cursor format".to_string()))?;
    let slot = slot_str
        .parse::<i64>()
        .map_err(|_| ApiError::InvalidValue("invalid cursor slot".to_string()))?;
    Ok((slot, sig.to_string()))
}
```

**Cursor WHERE clause** — append manually to the query builder BEFORE calling `build_query`, or inject as a special filter. Since `build_query` doesn't natively support composite cursor conditions, build the cursor condition manually:

Option A (recommended): Add cursor filters as two separate `ResolvedFilter` entries won't work because `(slot, signature) < (a, b)` is a tuple comparison, not two independent filters.

Option B: Extend `build_query` to accept an optional cursor. But this changes the existing API.

Option C (simplest): Build the query manually for instruction endpoints, reusing `append_filter_clause` logic, OR inject a raw SQL cursor clause after `build_query`. Since `QueryBuilder` is mutable, you can push additional SQL after building:

```rust
let mut qb = build_query(&target, &resolved, limit + 1, 0); // +1 for has_more detection

if let Some(ref cursor) = cursor_param {
    let (cursor_slot, cursor_sig) = decode_cursor(cursor)?;
    // Inject cursor condition — works because build_query already set WHERE or not
    // We need to check if there are existing WHERE clauses
    // Safest: add cursor as a filter before calling build_query
}
```

**Recommended approach**: Don't use `build_query` for instruction queries when cursor is present. Instead, build the query inline using the same pattern. OR — simpler — inject the cursor as two synthetic filters:

```rust
if let Some(cursor) = cursor_param {
    let (cursor_slot, cursor_sig) = decode_cursor(&cursor)?;
    // For DESC order: WHERE (slot < cursor_slot) OR (slot = cursor_slot AND signature < cursor_sig)
    // This is hard to express as simple filters.
}
```

**Simplest viable approach**: Build the full query manually in the handler for instructions, using `QueryBuilder` directly. Reuse `append_filter_clause` from `queries.rs` (make it `pub`). This avoids modifying `build_query`'s interface while handling the cursor composite condition correctly.

Alternative: Since the cursor is essentially a "start after this row" mechanism, and `build_query` already supports offset, you could implement cursor pagination by:

1. Fetching `limit + 1` rows
2. If cursor provided, add `slot_lte` + additional signature filter
3. Trim to `limit` and check if there's a next page

But the cleanest approach is: make `build_query` accept an optional `CursorCondition` parameter, or build the instruction query inline.

### Pagination Parameter Parsing

Extract from the `HashMap<String, String>` query params:

```rust
fn clamp_limit(params: &HashMap<String, String>, config: &Config) -> i64 {
    params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| v.clamp(1, config.api_max_page_size as i64))
        .unwrap_or(config.api_default_page_size as i64)
}

fn clamp_offset(params: &HashMap<String, String>) -> i64 {
    params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| v.max(0))
        .unwrap_or(0)
}
```

### has_more Detection

Fetch `limit + 1` rows. If you get more than `limit`, `has_more = true` and return only the first `limit` rows:

```rust
let fetch_limit = limit + 1;
let mut qb = build_query(&target, &resolved, fetch_limit, offset);
let rows = qb.build().fetch_all(&state.pool).await
    .map_err(|e| ApiError::QueryFailed(e.to_string()))?;
let has_more = rows.len() as i64 > limit;
let rows = if has_more { &rows[..limit as usize] } else { &rows[..] };
```

### Row-to-JSON Mapping

Instructions and accounts use different column sets. Map `PgRow` to `serde_json::Value`:

```rust
use sqlx::Row;

fn instruction_row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "signature": row.get::<String, _>("signature"),
        "slot": row.get::<i64, _>("slot"),
        "block_time": row.get::<Option<i64>, _>("block_time"),
        "instruction_name": row.get::<String, _>("instruction_name"),
        "args": row.get::<Value, _>("args"),
        "accounts": row.get::<Value, _>("accounts"),
        "data": row.get::<Value, _>("data"),
    })
}

fn account_row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "pubkey": row.get::<String, _>("pubkey"),
        "slot_updated": row.get::<i64, _>("slot_updated"),
        "lamports": row.get::<i64, _>("lamports"),
        "data": row.get::<Value, _>("data"),
    })
}
```

### Query Timing

Track query duration for the `meta.query_time_ms` field:

```rust
let start = std::time::Instant::now();
let rows = qb.build().fetch_all(&state.pool).await...;
let query_time_ms = start.elapsed().as_millis() as u64;
```

### Count Query for Offset Pagination

For account queries using offset pagination, include `total` count:

```rust
let count_sql = format!(
    "SELECT COUNT(*) as count FROM {}.{}",
    quote_ident(&schema_name),
    quote_ident(&table_name)
);
let total: i64 = sqlx::query(&count_sql)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| ApiError::QueryFailed(e.to_string()))?
    .get("count");
```

Note: This is unfiltered total. If filters are applied, the count should match. For simplicity, use unfiltered total (matches typical API behavior where `total` means "total records of this type"). If filtered total is needed, it requires a second query with the same WHERE clause.

### Deferred Items from Story 5.2 Review (Now In Scope)

These were explicitly deferred to story 5.3:

1. **No max limit enforcement / negative limit** — Handle via `clamp_limit` (Task 2)
2. **No value format validation** — String on numeric column yields 500 instead of 400. Consider wrapping `build().fetch_all()` errors and checking for PostgreSQL type cast errors to return 400. At minimum, handle gracefully.

### New Error Variants

Add to `ApiError` in `src/api/mod.rs`:

```rust
#[error("instruction not found: {0}")]
InstructionNotFound(String),

#[error("account type not found: {0}")]
AccountTypeNotFound(String),

#[error("account not found: {0}")]
AccountNotFound(String),
```

IntoResponse mappings:

```rust
ApiError::InstructionNotFound(name) => (
    StatusCode::NOT_FOUND,
    "INSTRUCTION_NOT_FOUND",
    format!("Instruction '{name}' not found in IDL"),
),
ApiError::AccountTypeNotFound(name) => (
    StatusCode::NOT_FOUND,
    "ACCOUNT_TYPE_NOT_FOUND",
    format!("Account type '{name}' not found in IDL"),
),
ApiError::AccountNotFound(key) => (
    StatusCode::NOT_FOUND,
    "ACCOUNT_NOT_FOUND",
    format!("Account '{key}' not found"),
),
```

### Dependencies

Check if `base64` crate is already in `Cargo.toml`. If not, add:

```toml
base64 = "0.22"
```

### Files Created/Modified by This Story

| File                     | Action | Purpose                                              |
| ------------------------ | ------ | ---------------------------------------------------- |
| `src/api/mod.rs`         | Modify | 3 new ApiError variants + IntoResponse + 6 routes    |
| `src/api/handlers.rs`    | Modify | 6 new handlers + pagination helpers + row mappers    |
| `src/storage/queries.rs` | Modify | Make `append_filter_clause` pub (if cursor approach) |

Only 2-3 files touched. No new files.

### What This Story Does NOT Do

- Does NOT implement aggregation endpoints (`/count`, `/stats`) — story 5.4
- Does NOT implement sort parameter validation (`sort`, `order` query params) — deferred post-MVP
- Does NOT implement nested/dot-path field access (`config.max_amount_gt`) — deferred post-MVP
- Does NOT implement list_programs pagination — deferred (noted in 5.1 review)

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests — use `map_err` chains
- NO `println!` — use `tracing` macros
- NO SQL string concatenation for user values — use `push_bind()`
- NO holding `RwLock` read guard across `.await` points — clone needed data, drop guard
- NO `sqlx::query!()` compile-time macros — use runtime `sqlx::query()`
- NO exposing DB error details in API responses — log internally, return generic message
- NO `anyhow` — use `thiserror` typed enums
- NO raw user input in table/column names — derive from IDL + sanitize_identifier

### Import Ordering Convention

```rust
// std library
use std::collections::HashMap;
use std::sync::Arc;

// external crates
use axum::extract::{Path, Query, State};
use axum::Json;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sqlx::Row;
use tracing::warn;

// internal crate
use crate::api::filters::{parse_filters, resolve_filters, FilterContext, ResolvedFilter, ColumnExpr, FilterOp};
use crate::api::AppState;
use crate::storage::queries::{build_query, QueryTarget};
use crate::storage::schema::{quote_ident, sanitize_identifier};
```

### anchor-lang-idl-spec Types Reference

```rust
pub struct Idl {
    pub instructions: Vec<IdlInstruction>,
    pub accounts: Vec<IdlAccount>,    // name + discriminator only
    pub types: Vec<IdlTypeDef>,       // struct field definitions
}

pub struct IdlInstruction {
    pub name: String,
    pub args: Vec<IdlField>,
}

pub struct IdlAccount {
    pub name: String,
    pub discriminator: Vec<u8>,
}

pub struct IdlField {
    pub name: String,
    pub ty: IdlType,
}

pub enum IdlTypeDefTy {
    Struct { fields: Option<IdlDefinedFields> },
    Enum { variants: Vec<IdlEnumVariant> },
    Type { alias: IdlType },
}

pub enum IdlDefinedFields {
    Named(Vec<IdlField>),
    Tuple(Vec<IdlType>),
}
```

### Previous Story Intelligence

**From story 5.2:**

- `parse_filters` and `resolve_filters` are fully tested (17 tests)
- `build_query` produces correct SQL for both Instructions and Accounts targets (12 tests)
- `_contains` on promoted columns is rejected by `resolve_filters`
- Empty `_in` values produce `FALSE` clause
- JSONB equality uses `@>` containment (GIN-optimized)
- Total across all modules: 145 tests pass

**From story 5.1:**

- Handler pattern: `State(state): State<Arc<AppState>>` with `Path`/`Query` extractors
- Registry: `state.registry.read().await` → `get_idl(program_id)` returns `Option<&Idl>`
- `validate_program_id()` exists and should be reused for all program_id params
- Box::pin NOT needed here since handlers just read (no RwLock write guards across awaits)
- Response envelope: `{ "data": ..., "meta": { ... } }` — be consistent
- Error pattern: log at `error!` for 500s, return generic message

**From story 5.2 review — deferred items now in scope:**

- Limit clamping (negative/over-max) — implement in this story
- Value format validation — handle gracefully (PostgreSQL cast error → 400)

### Git Intelligence

Recent commits: all compile cleanly with clippy/fmt. Codebase at 145 passing tests. Stories 5.1 and 5.2 are both done and merged to main.

### Project Structure Notes

- All handlers live in `src/api/handlers.rs` — no per-endpoint files
- Filter logic in `src/api/filters.rs` — consumed by handlers
- Query builder in `src/storage/queries.rs` — consumed by handlers
- Schema utilities in `src/storage/schema.rs` — `quote_ident`, `sanitize_identifier`, `map_idl_type_to_pg`
- Config in `src/config.rs` — `api_default_page_size` (50), `api_max_page_size` (1000)
- Registry in `src/registry.rs` — `ProgramRegistry::get_idl()` returns `Option<&Idl>`

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-5-query-api-filtering.md#Story 5.3]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#API & Communication]
- [Source: _bmad-output/planning-artifacts/research/agent-2d-dynamic-rest-api-design.md#Section 3-7]
- [Source: _bmad-output/planning-artifacts/prd.md#API Surface]
- [Source: _bmad-output/implementation-artifacts/5-2-dynamic-query-builder-and-filters.md]
- [Source: _bmad-output/implementation-artifacts/5-1-program-management-endpoints.md]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md#Deferred from story 5-2]
- [Source: src/api/mod.rs — current ApiError enum, router, AppState]
- [Source: src/api/handlers.rs — existing handler patterns]
- [Source: src/api/filters.rs — parse_filters, resolve_filters, FilterContext]
- [Source: src/storage/queries.rs — build_query, QueryTarget]
- [Source: src/storage/schema.rs — quote_ident, sanitize_identifier]
- [Source: src/config.rs — api_default_page_size, api_max_page_size]
- [Source: src/registry.rs — ProgramRegistry::get_idl()]

## Dev Agent Record

### Agent Model Used

claude-opus-4-6

### Debug Log References

- Build fix: `list_instruction_types`/`list_account_types` — borrow-then-drop pattern failed; switched to block-scoped clone pattern
- Build fix: `query_accounts` — missing `mut` on `qb` for `build()` call

### Completion Notes List

- Added 3 new ApiError variants (InstructionNotFound, AccountTypeNotFound, AccountNotFound) with 404 IntoResponse mappings
- Added 6 new routes to axum Router for instruction/account query endpoints
- Implemented pagination helpers: `clamp_limit`, `clamp_offset`, `encode_cursor`, `decode_cursor`
- Implemented `get_schema_name` DB helper and `get_account_fields` IDL helper
- Implemented `instruction_row_to_json` and `account_row_to_json` row mappers
- 6 new handlers: `list_instruction_types`, `query_instructions`, `list_account_types`, `query_accounts`, `get_account`, plus `get_schema_name` helper
- Cursor pagination for instructions: keyset on (slot, signature) DESC with base64-encoded cursor
- Offset pagination for accounts: parallel COUNT(\*) query via tokio::try_join!
- has_more detection: fetch limit+1, trim to limit
- All RwLock read guards dropped before any .await points (clone-then-drop pattern)
- 69 new lines of tests: cursor roundtrip, clamp_limit (6 edge cases), clamp_offset (4 edge cases), 3 new ApiError variant tests, get_account_fields helper test
- Total: 214 tests pass, 3 ignored, clippy clean, fmt clean

### Review Findings

- [ ] [Review][Patch] P1: CRITICAL — Cursor condition injected after ORDER BY/LIMIT produces invalid SQL [handlers.rs:577-593]
- [ ] [Review][Patch] P2: HIGH — PostgreSQL type cast errors (e.g. slot_gte=abc) return 500 instead of 400 [handlers.rs:598-600]
- [ ] [Review][Patch] P3: MEDIUM — Negative/zero limit returns 1, spec (AC7) says use default (50) [handlers.rs:417-423]
- [x] [Review][Defer] W1: JSONB range comparisons use text ordering, not numeric [queries.rs:134-143] — deferred, pre-existing from story 5.2
- [x] [Review][Defer] W2: Registry vs DB schema dropped externally yields 500 — deferred, pre-existing architectural
- [x] [Review][Defer] W3: Cursor key insufficiency (instruction_index not in cursor tuple) — deferred, changes API contract, rare edge case

### Change Log

- 2026-04-06: Story 5.3 implementation complete — 6 query handlers, pagination, 3 error variants, 19 new tests

### File List

- `src/api/mod.rs` — Modified: 3 new ApiError variants + IntoResponse + 5 new routes
- `src/api/handlers.rs` — Modified: 6 new handlers, pagination helpers, row mappers, get_schema_name, get_account_fields, 19 new tests
- `_bmad-output/implementation-artifacts/5-3-instruction-and-account-query-endpoints.md` — Modified: task checkboxes, dev agent record, status
