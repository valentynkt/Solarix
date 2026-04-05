# Story 5.2: Dynamic Query Builder & Filters

Status: ready-for-dev

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

- [ ] Task 1: Implement filter operator enum and parser in `src/api/filters.rs` (AC: #1)
  - [ ] Define `FilterOp` enum: `Eq`, `Ne`, `Gt`, `Gte`, `Lt`, `Lte`, `Contains`, `In`
  - [ ] Define `ParsedFilter` struct: `field: String`, `op: FilterOp`, `value: String`
  - [ ] Define `RESERVED_PARAMS` constant: `limit`, `offset`, `cursor`, `sort`, `order`
  - [ ] Implement `parse_filters(params: &HashMap<String, String>) -> Vec<ParsedFilter>` that skips reserved params and parses `field_op=value`
  - [ ] Parse by trying known operator suffixes from longest to shortest (`_contains` before `_in`), splitting on the last match; default to `Eq` if no operator suffix found
- [ ] Task 2: Implement IDL field resolution and validation in `src/api/filters.rs` (AC: #2)
  - [ ] Define `ResolvedFilter` struct: `column_expr: ColumnExpr`, `op: FilterOp`, `value: String`
  - [ ] Define `ColumnExpr` enum: `Promoted { column: String }`, `Jsonb { field: String }`
  - [ ] Implement `resolve_filters(parsed: &[ParsedFilter], fields: &[IdlField], types: &[IdlTypeDef]) -> Result<Vec<ResolvedFilter>, ApiError>`
  - [ ] For each filter: check if field name matches a top-level IDL field where `map_idl_type_to_pg` returns `Some` -> `Promoted`; else if field exists in IDL but not promotable -> `Jsonb`; else -> `ApiError::InvalidFilter` with available field names
  - [ ] Also accept common/fixed columns without IDL check (see Dev Notes below)
- [ ] Task 3: Implement `QueryBuilder` in `src/storage/queries.rs` (AC: #3, #4)
  - [ ] Define `QueryTarget` enum: `Instructions { schema: String }`, `Accounts { schema: String, table: String }`
  - [ ] Implement `pub fn build_query(target: &QueryTarget, filters: &[ResolvedFilter], limit: i64, offset: i64) -> sqlx::QueryBuilder<'_, sqlx::Postgres>`
  - [ ] Build SELECT with appropriate columns for instructions vs accounts
  - [ ] Append WHERE clauses per filter: promoted columns use direct comparison; JSONB fields use `@>` containment with `push_bind(serde_json::json!({ field: value }))`
  - [ ] For `_in` operator: split value on `,`, bind as `Vec<String>`, use `= ANY($)`
  - [ ] For `_contains` on JSONB: use `@>` containment (not LIKE)
  - [ ] Append ORDER BY, LIMIT, OFFSET
- [ ] Task 4: Add `ApiError::InvalidValue` variant (AC: #2)
  - [ ] Add `InvalidValue(String)` to `ApiError` in `src/api/mod.rs` mapping to 400 `INVALID_VALUE`
- [ ] Task 5: Unit tests (AC: all)
  - [ ] Test `parse_filters` with various operator suffixes, edge cases (field name containing `_gt` substring), reserved param skipping
  - [ ] Test `resolve_filters` with promoted fields, JSONB-only fields, unknown fields returning error with available_fields
  - [ ] Test `build_query` produces correct SQL structure for promoted vs JSONB filters, \_in arrays, \_contains
- [ ] Task 6: Verify (AC: all)
  - [ ] `cargo build` compiles
  - [ ] `cargo clippy` passes
  - [ ] `cargo fmt -- --check` passes
  - [ ] `cargo test` passes all unit tests

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

## Dev Agent Record

### Agent Model Used

{{agent_model_name_version}}

### Debug Log References

### Completion Notes List

### File List
