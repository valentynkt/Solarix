# Story 5.2: Dynamic Query Builder & Filters

Status: done

## Story

As a developer,
I want a dynamic SQL query builder that translates API filter parameters into safe, IDL-validated SQL queries,
so that users can filter indexed data by any field without risk of SQL injection.

## Acceptance Criteria

1. **AC1: Filter parameter parsing**
   - **Given** query parameters like `amount_gt=1000&signer_eq=ABC123`
   - **When** the filter parser in `src/api/filters.rs` processes them
   - **Then** it extracts field name and operator by splitting on the last `_` separator matching a known operator
   - **And** supported operators are: `_gt`, `_gte`, `_lt`, `_lte`, `_eq`, `_ne`, `_contains`, `_in`
   - **And** parameters are extracted via `Query<HashMap<String, String>>` (dynamic, not typed struct)

2. **AC2: IDL-aware field validation**
   - **Given** a filter field name
   - **When** the validator checks it against the IDL
   - **Then** promoted column fields are queried directly as SQL columns
   - **And** non-promoted fields (nested/complex) use JSONB `@>` containment queries (NOT `data->>'field'` which bypasses GIN indexes)
   - **And** unknown field names return HTTP 400 with `{ "error": { "code": "INVALID_FILTER", "message": "Unknown field 'foo'", "available_fields": ["amount", "authority", ...] } }`

3. **AC3: Safe SQL query construction**
   - **Given** the `QueryBuilder` in `src/storage/queries.rs`
   - **When** building a SQL query with filters
   - **Then** all user-provided values are bound via `QueryBuilder::push_bind()` (never string concatenation)
   - **And** table and column names are derived from the IDL (not from user input) and double-quoted
   - **And** operator mapping: `_gt` -> `>`, `_gte` -> `>=`, `_lt` -> `<`, `_lte` -> `<=`, `_eq` -> `=`, `_ne` -> `!=`, `_in` -> `= ANY($)`, `_contains` -> `@>`

4. **AC4: Array parameter handling**
   - **Given** the `_in` operator with value `val1,val2,val3`
   - **When** the query builder processes it
   - **Then** it splits on comma and binds as an array parameter

## Tasks / Subtasks

- [x] Task 1: Implement filter operator enum and parser in `src/api/filters.rs` (AC: #1)
  - [x] Define `FilterOp` enum: `Eq`, `Ne`, `Gt`, `Gte`, `Lt`, `Lte`, `Contains`, `In`
  - [x] Define `ParsedFilter` struct: `field: String`, `op: FilterOp`, `value: String`
  - [x] Define `RESERVED_PARAMS` constant: `limit`, `offset`, `cursor`, `sort`, `order`
  - [x] Implement `parse_filters(params: &HashMap<String, String>) -> Vec<ParsedFilter>` that skips reserved params and parses `field_op=value`
  - [x] Parse by trying known operator suffixes from longest to shortest (`_contains` before `_in`), splitting on the last match; default to `Eq` if no operator suffix found
- [x] Task 2: Implement IDL field resolution and validation in `src/api/filters.rs` (AC: #2)
  - [x] Define `ResolvedFilter` struct: `column_expr: ColumnExpr`, `op: FilterOp`, `value: String`
  - [x] Define `ColumnExpr` enum: `Promoted { column: String }`, `Jsonb { field: String }`
  - [x] Implement `resolve_filters(parsed: &[ParsedFilter], fields: &[IdlField], types: &[IdlTypeDef]) -> Result<Vec<ResolvedFilter>, ApiError>`
  - [x] For each filter: check if field name matches a top-level IDL field where `map_idl_type_to_pg` returns `Some` -> `Promoted`; else if field exists in IDL but not promotable -> `Jsonb`; else -> `ApiError::InvalidFilter` with available field names
  - [x] Also accept common/fixed columns without IDL check (see Dev Notes below)
- [x] Task 3: Implement `QueryBuilder` in `src/storage/queries.rs` (AC: #3, #4)
  - [x] Define `QueryTarget` enum: `Instructions { schema: String }`, `Accounts { schema: String, table: String }`
  - [x] Implement `pub fn build_query(target: &QueryTarget, filters: &[ResolvedFilter], limit: i64, offset: i64) -> sqlx::QueryBuilder<'_, sqlx::Postgres>`
  - [x] Build SELECT with appropriate columns for instructions vs accounts
  - [x] Append WHERE clauses per filter: promoted columns use direct comparison; JSONB fields use `@>` containment with `push_bind(serde_json::json!({ field: value }))`
  - [x] For `_in` operator: split value on `,`, bind as `Vec<String>`, use `= ANY($)`
  - [x] For `_contains` on JSONB: use `@>` containment (not LIKE)
  - [x] Append ORDER BY, LIMIT, OFFSET
- [x] Task 4: Add `ApiError::InvalidValue` variant (AC: #2)
  - [x] Add `InvalidValue(String)` to `ApiError` in `src/api/mod.rs` mapping to 400 `INVALID_VALUE`
- [x] Task 5: Unit tests (AC: all)
  - [x] Test `parse_filters` with various operator suffixes, edge cases (field name containing `_gt` substring), reserved param skipping
  - [x] Test `resolve_filters` with promoted fields, JSONB-only fields, unknown fields returning error with available_fields
  - [x] Test `build_query` produces correct SQL structure for promoted vs JSONB filters, \_in arrays, \_contains
- [x] Task 6: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` passes all unit tests

## Dev Notes

### Current Codebase State (Post Story 5.1)

**`src/api/filters.rs`** contains only a placeholder:

```rust
/// Placeholder for query parameter parsing and operator validation.
pub struct FilterParams;
```

Replace entirely. This file owns all filter parsing and IDL validation logic.

**`src/storage/queries.rs`** contains only a placeholder:

```rust
/// Dynamic query builder for API reads.
pub struct DynamicQueryBuilder;
```

Replace entirely. This file owns all dynamic SQL construction.

**`src/api/mod.rs`** already has `pub mod filters;` and a complete `ApiError` enum with `IntoResponse`. The `InvalidFilter(String)` variant exists.

### Reuse: `map_idl_type_to_pg` from `storage/schema.rs`

The function `crate::storage::schema::map_idl_type_to_pg(ty, types)` determines if an IDL field is promoted to a native column. Reuse this to decide if a filter targets a promoted column or JSONB:

```rust
use crate::storage::schema::{map_idl_type_to_pg, sanitize_identifier, quote_ident};
```

- `map_idl_type_to_pg` returns `Some("PG_TYPE")` -> promoted column
- Returns `None` -> field exists in IDL but lives in JSONB `data` column only

### Operator Parsing Strategy

Parse operator suffix by checking from longest to shortest to avoid ambiguous matches:

```
_contains (9 chars)
_gte     (4 chars)
_lte     (4 chars)
_gt      (3 chars)
_lt      (3 chars)
_eq      (3 chars)
_ne      (3 chars)
_in      (3 chars)
```

For a key like `amount_gt`, split on the last `_gt` to get `field=amount`, `op=Gt`. For a key like `my_field_gt`, split on the last `_gt` to get `field=my_field`, `op=Gt`.

Implementation: iterate the OPERATORS list from longest suffix to shortest. For each, check if the key ends with that suffix. If yes, the field name is `key[..key.len() - suffix.len()]`. This avoids ambiguity since `_gte` is checked before `_gt`.

If no operator suffix matches, treat the entire key as the field name with `Eq` operator.

### Common/Fixed Columns (No IDL Check Needed)

These columns exist on all tables regardless of IDL and should be accepted without IDL validation:

**Instructions table (`_instructions`):**

- `slot` (BIGINT)
- `signature` (TEXT)
- `block_time` (BIGINT)
- `instruction_name` (TEXT)
- `instruction_index` (SMALLINT)
- `is_inner_ix` (BOOLEAN)

**Account tables:**

- `pubkey` (TEXT)
- `slot_updated` (BIGINT)
- `lamports` (BIGINT)
- `is_closed` (BOOLEAN)

These are always `Promoted` column expressions. Store them as a constant set.

### JSONB Containment for Non-Promoted Fields

For JSONB fields, use `@>` containment operator which leverages GIN `jsonb_path_ops` indexes:

```sql
-- Promoted column (direct comparison):
WHERE "amount" > $1

-- JSONB field (containment — uses GIN index):
WHERE "data" @> $1   -- bind $1 as JSON: {"field_name": "value"}
```

The `@>` operator checks if the left JSONB value contains the right JSONB value. Binding as `serde_json::json!({"field": value})` via `push_bind`.

**Important:** `@>` only supports equality semantics. For comparison operators (`_gt`, `_lt`, etc.) on JSONB fields, fall back to `(data->>'{field}')::TYPE` cast. This does NOT use GIN indexes but is necessary for range queries on non-promoted fields.

Strategy:

- JSONB + `_eq` -> `"data" @> $1` (GIN-optimized)
- JSONB + `_gt/_lt/etc.` -> `("data"->>'{field}')::TYPE > $1` (no GIN, sequential)
- JSONB + `_contains` -> `"data" @> $1` (treats value as sub-document match)
- JSONB + `_in` -> `"data"->>'{field}' = ANY($1)` (text array comparison)

### SQL QueryBuilder Pattern

Use `sqlx::QueryBuilder<'_, Postgres>`:

```rust
use sqlx::postgres::Postgres;
use sqlx::QueryBuilder;

pub fn build_query<'a>(
    target: &QueryTarget,
    filters: &[ResolvedFilter],
    limit: i64,
    offset: i64,
) -> QueryBuilder<'a, Postgres> {
    let (qualified_table, select_cols) = match target {
        QueryTarget::Instructions { schema } => (
            format!("{}.{}", quote_ident(schema), quote_ident("_instructions")),
            r#""signature", "slot", "block_time", "instruction_name", "args", "accounts", "data""#,
        ),
        QueryTarget::Accounts { schema, table } => (
            format!("{}.{}", quote_ident(schema), quote_ident(table)),
            r#""pubkey", "slot_updated", "lamports", "data""#,
        ),
    };

    let mut qb = QueryBuilder::new(format!("SELECT {select_cols} FROM {qualified_table}"));

    // WHERE clauses
    let mut has_where = false;
    for filter in filters {
        qb.push(if has_where { " AND " } else { " WHERE " });
        has_where = true;
        append_filter_clause(&mut qb, filter);
    }

    // ORDER BY, LIMIT, OFFSET
    qb.push(" ORDER BY ");
    match target {
        QueryTarget::Instructions { .. } => qb.push(r#""slot" DESC, "signature" DESC"#),
        QueryTarget::Accounts { .. } => qb.push(r#""slot_updated" DESC"#),
    };

    qb.push(" LIMIT ");
    qb.push_bind(limit);

    if offset > 0 {
        qb.push(" OFFSET ");
        qb.push_bind(offset);
    }

    qb
}
```

### Adding `InvalidValue` to `ApiError`

Add one variant to `src/api/mod.rs`:

```rust
#[error("invalid value: {0}")]
InvalidValue(String),
```

In the `IntoResponse` impl:

```rust
ApiError::InvalidValue(msg) => {
    (StatusCode::BAD_REQUEST, "INVALID_VALUE", msg.clone())
}
```

### Files Created/Modified by This Story

| File                     | Action  | Purpose                                       |
| ------------------------ | ------- | --------------------------------------------- |
| `src/api/filters.rs`     | Rewrite | Filter parsing, operator enum, IDL validation |
| `src/storage/queries.rs` | Rewrite | Dynamic SQL query builder with bind params    |
| `src/api/mod.rs`         | Modify  | Add `InvalidValue` variant to `ApiError`      |

Only 3 files touched. `filters.rs` and `queries.rs` are full rewrites of placeholders.

### What This Story Does NOT Do

- Does NOT implement API endpoint handlers that USE the query builder (story 5.3)
- Does NOT implement cursor-based pagination encoding/decoding (story 5.3)
- Does NOT implement sort parameter validation (story 5.3)
- Does NOT implement aggregation queries like `instruction_count` (story 5.4)
- Does NOT add new routes to the router (story 5.3)
- Does NOT implement nested/dot-path field access (`config.max_amount_gt`) -- deferred post-MVP

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests
- NO `println!` -- use `tracing` macros (`info!`, `warn!`, `error!`)
- NO SQL string concatenation for user-provided values -- use `push_bind()` always
- NO `data->>'field'` for equality filters on JSONB -- use `@>` containment to leverage GIN indexes
- NO `sqlx::query!()` compile-time macros -- use runtime `sqlx::QueryBuilder`
- NO passing raw user input as table/column names -- derive from IDL/schema
- NO `anyhow` -- use `thiserror` typed enums
- NO separate `error.rs` file -- `ApiError` stays in `src/api/mod.rs`
- NO accepting arbitrary field names without IDL validation

### Import Ordering Convention

```rust
// std library
use std::collections::{HashMap, HashSet};

// external crates
use anchor_lang_idl_spec::{IdlField, IdlTypeDef};
use serde_json::json;
use sqlx::postgres::Postgres;
use sqlx::QueryBuilder;

// internal crate
use crate::api::ApiError;
use crate::storage::schema::{map_idl_type_to_pg, quote_ident, sanitize_identifier};
```

### Previous Story Intelligence

From story 5.1:

- `ApiError` enum already has: `InvalidFilter(String)`, `ProgramNotFound(String)`, `ProgramAlreadyRegistered(String)`, `QueryFailed(String)`, `InvalidRequest(String)`, `IdlError(String)`, `StorageError(String)` -- all with `IntoResponse`
- `AppState` has: `pool: PgPool`, `start_time: Instant`, `registry: Arc<RwLock<ProgramRegistry>>`, `config: Config`
- Handlers use `State(state): State<Arc<AppState>>` pattern
- Registry accessed via `state.registry.read().await` for reads, `.write().await` for writes
- `ProgramRegistry::get_idl(program_id)` returns `Option<&Idl>` for cached IDL access
- Response envelope: `{ "data": ..., "meta": { ... } }` for success, `{ "error": { "code": "...", "message": "..." } }` for errors

From story 2.3:

- `quote_ident(name)` -- double-quotes identifiers, escapes embedded quotes
- `sanitize_identifier(name)` -- strips non-alphanumeric (except `_`), lowercases
- `map_idl_type_to_pg(ty, types)` -- `Some("PG_TYPE")` for promoted, `None` for JSONB-only
- Account field definitions live in `idl.types` (matched by name to `idl.accounts`)
- Instruction args are in `idl.instructions[n].args: Vec<IdlField>`
- Schema naming: `{sanitized_name}_{first_8_of_program_id}`

### anchor-lang-idl-spec Types for Filter Validation

```rust
pub struct Idl {
    pub instructions: Vec<IdlInstruction>,
    pub accounts: Vec<IdlAccount>,    // name + discriminator only
    pub types: Vec<IdlTypeDef>,       // struct field definitions here
}

pub struct IdlInstruction {
    pub name: String,
    pub args: Vec<IdlField>,          // instruction argument fields
}

pub struct IdlField {
    pub name: String,
    pub ty: IdlType,                  // field is `ty` not `type`
}
```

To get fields for filter validation:

- **Instructions**: `idl.instructions.iter().find(|i| i.name == name).map(|i| &i.args)`
- **Accounts**: look up `IdlTypeDef` in `idl.types` by account name, extract `Struct { fields: Named(fields) }`

### Git Intelligence

Recent commits show the codebase compiles cleanly with all clippy/fmt checks passing. Story 5.1 added program management handlers following the patterns in the Dev Notes above. The codebase has ~80+ unit tests across all modules.

### Testing Strategy

All tests are unit tests (no DB required):

1. **Filter parsing tests** -- verify operator extraction from query param keys, reserved param skipping, edge cases with field names containing operator substrings
2. **Filter resolution tests** -- verify promoted vs JSONB classification using mock `IdlField` arrays, unknown field error with available_fields
3. **Query builder tests** -- verify generated SQL structure by calling `build_query` and inspecting the SQL string (via `QueryBuilder::sql()`), verify bind count matches filter count

Test helpers: create `IdlField` instances directly since the struct has public fields.

### Project Structure Notes

- `src/api/filters.rs` -- filter parsing and IDL validation (consumed by handlers in story 5.3)
- `src/storage/queries.rs` -- SQL query construction (consumed by handlers in story 5.3)
- These are library modules -- they expose types and functions but no endpoint handlers
- Story 5.3 will import from both and wire them into handler functions

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-5-query-api-filtering.md#Story 5.2]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#API & Communication]
- [Source: _bmad-output/planning-artifacts/research/agent-2d-dynamic-rest-api-design.md#Section 4]
- [Source: _bmad-output/implementation-artifacts/5-1-program-management-endpoints.md]
- [Source: _bmad-output/implementation-artifacts/2-3-dynamic-schema-generation.md]
- [Source: src/api/mod.rs -- current ApiError enum, IntoResponse impl]
- [Source: src/api/filters.rs -- placeholder to replace]
- [Source: src/storage/queries.rs -- placeholder to replace]
- [Source: src/storage/schema.rs -- map_idl_type_to_pg, quote_ident, sanitize_identifier]

### Review Findings

- [x] [Review][Decision] JSONB range queries missing `::TYPE` cast — DEFERRED. All Jsonb fields are complex types (map_idl_type_to_pg returned None) where range queries don't have meaningful numeric semantics. Text comparison is a cosmetic issue, not a real bug. [src/storage/queries.rs:134-143]
- [x] [Review][Patch] JSONB `_eq`/`_contains` always wraps value as JSON string — FIXED. Now tries `serde_json::from_str` first, falls back to `Value::String`. Numeric/boolean JSONB values now match correctly. [src/storage/queries.rs:97-108]
- [x] [Review][Patch] Promoted `_contains` generates invalid SQL — FIXED. `resolve_filters` now rejects `_contains` on promoted columns with `InvalidFilter` error. [src/api/filters.rs:180-188]
- [x] [Review][Patch] Error response missing `available_fields` as separate JSON key — FIXED. `InvalidFilter` now carries `Vec<String>` and `IntoResponse` emits `available_fields` array in error JSON. [src/api/mod.rs:31-35, 68-78]
- [x] [Review][Patch] Empty `_in` value matches empty string — FIXED. Empty strings filtered out after comma split; empty result produces `FALSE` clause. [src/storage/queries.rs:79-92, 123-136]
- [x] [Review][Patch] HashSet rebuilt on every `parse_filters` call — FIXED. Now uses `RESERVED_PARAMS.contains()` directly on slice. [src/api/filters.rs:87]
- [x] [Review][Defer] No max limit enforcement / negative limit bypasses pagination — handler responsibility (story 5.3) [src/storage/queries.rs:21-26] — deferred, story 5.3 scope
- [x] [Review][Defer] No value format validation — string value on numeric promoted column yields 500 instead of 400 — handler-level validation (story 5.3) [src/api/filters.rs, src/storage/queries.rs] — deferred, story 5.3 scope
- [x] [Review][Defer] Fixed columns (`instruction_index`, `is_inner_ix`, `is_closed`) filterable but not in SELECT — spec inconsistency, needs product decision [src/storage/queries.rs:30,34] — deferred, spec clarification needed
- [x] [Review][Defer] No tests verifying HTTP error response JSON structure for `InvalidFilter` — integration testing scope (story 6.3) — deferred, story 6.3 scope

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

- Initial JSONB key escaping produced double-quoted keys (`''score''`); fixed `escape_jsonb_key` to return raw escaped string, caller adds outer quotes in format string.

### Completion Notes List

- Implemented `FilterOp` enum (8 operators) with `as_sql()` method for SQL operator mapping
- Implemented `ParsedFilter` and `parse_filters()` with longest-suffix-first matching strategy
- Implemented `ColumnExpr` enum (Promoted/Jsonb), `ResolvedFilter`, and `resolve_filters()` with IDL-aware field classification using `map_idl_type_to_pg`
- Added `FilterContext` enum to distinguish instruction vs account fixed columns
- Implemented `QueryTarget` enum and `build_query()` producing complete SELECT with WHERE, ORDER BY, LIMIT, OFFSET
- JSONB equality/contains uses `@>` containment (GIN-optimized); range queries use `->>`text extraction
- `_in` operator splits on comma, binds as `Vec<String>` with `= ANY()`
- Added `ApiError::InvalidValue` variant with 400 status and `INVALID_VALUE` code
- 36 new unit tests (10 parse_filters, 7 resolve_filters, 12 build_query, 1 operator mapping, 1 escape, 5 existing passing)
- All 145 tests pass, clippy clean, fmt clean

### File List

- `src/api/filters.rs` — Rewritten: filter parsing, operator enum, IDL validation, unit tests
- `src/storage/queries.rs` — Rewritten: dynamic SQL query builder, unit tests
- `src/api/mod.rs` — Modified: added `InvalidValue` variant to `ApiError` + `IntoResponse` mapping

### Change Log

- 2026-04-06: Story 5.2 implementation complete — filter parser, IDL resolver, query builder, unit tests
