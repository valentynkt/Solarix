# Story 2.3: Dynamic Schema Generation (DDL Engine)

Status: in-progress

## Story

As a user,
I want the system to automatically generate a complete PostgreSQL schema from an Anchor IDL,
so that indexed data lands in properly typed, queryable tables without any manual database setup.

## Acceptance Criteria

1. **AC1: Schema creation**
   - **Given** a parsed Anchor IDL for a registered program
   - **When** the schema generator runs
   - **Then** it creates a PostgreSQL schema named `{sanitized_name}_{lowercase_first_8_of_base58_program_id}` using `CREATE SCHEMA IF NOT EXISTS`
   - **And** all identifiers are double-quoted in generated DDL

2. **AC2: Account tables with promoted columns**
   - **Given** the IDL defines account types
   - **When** the schema generator processes each account type
   - **Then** it creates one table per account type with common columns: `pubkey TEXT PRIMARY KEY`, `slot_updated BIGINT NOT NULL`, `write_version BIGINT NOT NULL DEFAULT 0`, `lamports BIGINT NOT NULL`, `data JSONB NOT NULL`, `is_closed BOOLEAN NOT NULL DEFAULT FALSE`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`
   - **And** top-level scalar IDL fields are promoted to native typed columns (always nullable, to handle u64 overflow and partial data)
   - **And** the type mapping covers: u8/i8 -> SMALLINT, u16 -> INTEGER, i16 -> SMALLINT, u32/i32 -> INTEGER, u64/i64 -> BIGINT, u128/i128 -> NUMERIC(39,0), f32 -> REAL, f64 -> DOUBLE PRECISION, bool -> BOOLEAN, string -> TEXT, pubkey -> TEXT, bytes/Vec<u8>/[u8;N] -> BYTEA, Option<T> -> nullable column of T's type, Vec<T>/arrays/structs/enums/tuples -> not promoted (JSONB only)
   - **And** `IF NOT EXISTS` is used for all CREATE statements

3. **AC3: Instructions table**
   - **Given** the IDL defines instructions
   - **When** the schema generator processes the instructions
   - **Then** it creates a single `_instructions` table with: `id BIGSERIAL PRIMARY KEY`, `signature TEXT NOT NULL`, `slot BIGINT NOT NULL`, `block_time BIGINT`, `instruction_name TEXT NOT NULL`, `instruction_index SMALLINT NOT NULL`, `inner_index SMALLINT`, `args JSONB NOT NULL`, `accounts JSONB NOT NULL`, `data JSONB NOT NULL`, `is_inner_ix BOOLEAN NOT NULL DEFAULT FALSE`
   - **And** a unique constraint on `(signature, instruction_index, COALESCE(inner_index, -1))`

4. **AC4: Indexes**
   - **Given** schema generation completes
   - **When** I inspect the created tables
   - **Then** B-tree indexes exist on: `slot` (all tables), `signature` (\_instructions), `instruction_name` (\_instructions), `block_time` (\_instructions)
   - **And** GIN indexes with `jsonb_path_ops` exist on `data` columns

5. **AC5: Metadata and checkpoint tables**
   - **Given** schema generation completes
   - **Then** a `_metadata` table exists with key-value pairs: `program_id`, `program_name`, `idl_hash`, `idl_version`, `schema_created_at`, `account_types` (JSON array), `instruction_types` (JSON array)
   - **And** a `_checkpoints` table exists with: `stream TEXT PRIMARY KEY`, `last_slot BIGINT`, `last_signature VARCHAR(88)`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`

6. **AC6: DDL execution atomicity**
   - **Given** DDL execution
   - **When** the schema generator runs all statements
   - **Then** they are executed via `sqlx::raw_sql()` within a transaction
   - **And** any statement failure rolls back all DDL for that program
   - **And** the `programs` table `status` is updated to `'schema_created'` on success or `'error'` on failure with `error_message`

7. **AC7: Integration with ProgramRegistry**
   - **Given** a program is registered via `ProgramRegistry::register_program()`
   - **When** registration succeeds (IDL fetched, DB row inserted)
   - **Then** `generate_schema()` is called automatically to create the per-program schema
   - **And** on success the `programs.status` transitions from `'registered'` to `'schema_created'`

## Tasks / Subtasks

- [ ] Task 1: Implement `quote_ident()` helper (AC: #1)
  - [ ] Add `fn quote_ident(name: &str) -> String` in `src/storage/schema.rs`
  - [ ] Escapes embedded double-quotes by doubling them, wraps in double-quotes

- [ ] Task 2: Implement IDL type to PostgreSQL type mapping (AC: #2)
  - [ ] Add `fn map_idl_type_to_pg(ty: &IdlType, types: &[IdlTypeDef]) -> Option<&'static str>`
  - [ ] Returns `Some("PG_TYPE")` for promotable scalars, `None` for complex types (JSONB only)
  - [ ] Handle `IdlType::Defined` by resolving through the type alias chain: if alias resolves to scalar -> promote; if struct/enum -> None
  - [ ] Handle `IdlType::Option(inner)` — promote inner type (column is always nullable anyway)
  - [ ] Handle `IdlType::Bytes` and `IdlType::Array(U8, _)` as `BYTEA`

- [ ] Task 3: Implement account table DDL generation (AC: #2)
  - [ ] Add `fn generate_account_table(schema: &str, account_name: &str, fields: &[IdlField], types: &[IdlTypeDef]) -> Vec<String>`
  - [ ] Common columns: `pubkey TEXT PRIMARY KEY`, `slot_updated BIGINT NOT NULL`, `write_version BIGINT NOT NULL DEFAULT 0`, `lamports BIGINT NOT NULL`, `data JSONB NOT NULL`, `is_closed BOOLEAN NOT NULL DEFAULT FALSE`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`
  - [ ] For each `IdlField` where `map_idl_type_to_pg` returns `Some(pg_type)`, add a nullable promoted column
  - [ ] Column names from `sanitize_identifier(&field.name)`

- [ ] Task 4: Implement instructions table DDL generation (AC: #3)
  - [ ] Add `fn generate_instructions_table(schema: &str) -> Vec<String>`
  - [ ] Fixed columns: `id BIGSERIAL PRIMARY KEY`, `signature TEXT NOT NULL`, `slot BIGINT NOT NULL`, `block_time BIGINT`, `instruction_name TEXT NOT NULL`, `instruction_index SMALLINT NOT NULL`, `inner_index SMALLINT`, `args JSONB NOT NULL`, `accounts JSONB NOT NULL`, `data JSONB NOT NULL`, `is_inner_ix BOOLEAN NOT NULL DEFAULT FALSE`
  - [ ] Add UNIQUE constraint on `(signature, instruction_index, COALESCE(inner_index, -1))`

- [ ] Task 5: Implement metadata and checkpoint tables (AC: #5)
  - [ ] Add `fn generate_metadata_table(schema: &str) -> String`
  - [ ] `_metadata` table: `key TEXT PRIMARY KEY`, `value JSONB NOT NULL`
  - [ ] Add `fn generate_checkpoints_table(schema: &str) -> String`
  - [ ] `_checkpoints` table: `stream TEXT PRIMARY KEY`, `last_slot BIGINT`, `last_signature VARCHAR(88)`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`

- [ ] Task 6: Implement index generation (AC: #4)
  - [ ] Add `fn generate_indexes(schema: &str, account_names: &[String]) -> Vec<String>`
  - [ ] B-tree: `slot` on each account table, `signature`/`instruction_name`/`block_time`/`slot` on `_instructions`
  - [ ] GIN `jsonb_path_ops`: `data` on each account table, `data` and `args` on `_instructions`
  - [ ] Index naming: `idx_{table}_{column}` for B-tree, `gin_{table}_{column}` for GIN

- [ ] Task 7: Implement top-level `generate_schema()` and `seed_metadata()` (AC: #1, #5, #6)
  - [ ] Add `pub async fn generate_schema(pool: &PgPool, idl: &Idl, program_id: &str, schema_name: &str) -> Result<(), StorageError>`
  - [ ] Build type lookup: `HashMap<String, &IdlTypeDef>` from `idl.types`
  - [ ] Collect all DDL statements: CREATE SCHEMA, \_metadata, \_checkpoints, account tables, instructions table, indexes
  - [ ] Execute in a transaction via `sqlx::raw_sql()` for each statement
  - [ ] On success: seed `_metadata` with program info
  - [ ] On failure: return `StorageError::DdlFailed`

- [ ] Task 8: Integrate into `ProgramRegistry::register_program()` (AC: #6, #7)
  - [ ] After DB insert, call `generate_schema()`
  - [ ] On success: UPDATE `programs` SET `status = 'schema_created'`
  - [ ] On failure: UPDATE `programs` SET `status = 'error'`, `error_message = ...` (add column if needed, or use `updated_at` + log)
  - [ ] Add `StorageError` variant to `RegistrationError` if needed

- [ ] Task 9: Add unit tests (AC: #2, #3, #4, #5)
  - [ ] Test `quote_ident` with normal names, names containing double-quotes, reserved words
  - [ ] Test `map_idl_type_to_pg` for all primitive types, Option wrapping, Defined resolution, complex types returning None
  - [ ] Test `generate_account_table` produces valid DDL with promoted columns
  - [ ] Test `generate_instructions_table` produces correct table structure
  - [ ] Test full `generate_schema` DDL output with a realistic IDL (use `tests/fixtures/idls/simple_v030.json`)

- [ ] Task 10: Verify (AC: all)
  - [ ] `cargo build` compiles
  - [ ] `cargo clippy` passes
  - [ ] `cargo fmt -- --check` passes
  - [ ] `cargo test` passes all unit tests

## Dev Notes

### Current Codebase State (Post Story 2.2)

`src/storage/schema.rs` currently contains only:

- `sanitize_identifier(name: &str) -> String` — strips non-alphanumeric chars, lowercases, truncates to 63 bytes
- `derive_schema_name(idl_name: &str, program_id: &str) -> String` — `{sanitized_name}_{first_8_of_program_id}`
- `truncate_to_bytes(s: &str, max: usize) -> String` — private helper
- 11 unit tests for the above

This story adds ALL DDL generation logic to the same file.

### anchor-lang-idl-spec Types You'll Use

The `anchor-lang-idl-spec` 0.1.0 crate provides these key types:

```rust
// Top-level IDL
pub struct Idl {
    pub address: String,
    pub metadata: IdlMetadata,     // .name, .version, .spec
    pub instructions: Vec<IdlInstruction>,
    pub accounts: Vec<IdlAccount>, // just name + discriminator
    pub types: Vec<IdlTypeDef>,    // account struct definitions live here
    // ...
}

// Account only has name and discriminator - struct fields are in `types`
pub struct IdlAccount {
    pub name: String,
    pub discriminator: Vec<u8>,
}

// Type definitions (account structs, enums, aliases)
pub struct IdlTypeDef {
    pub name: String,
    pub ty: IdlTypeDefTy,   // Struct { fields }, Enum { variants }, Type { alias }
    // ...
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

pub struct IdlField {
    pub name: String,
    pub ty: IdlType,  // NOTE: field is `ty` not `r#type`
}

pub enum IdlType {
    Bool, U8, I8, U16, I16, U32, I32, F32, U64, I64, F64,
    U128, I128, U256, I256, Bytes, String, Pubkey,
    Option(Box<IdlType>),
    Vec(Box<IdlType>),
    Array(Box<IdlType>, IdlArrayLen),
    Defined { name: String, generics: Vec<IdlGenericArg> },
    Generic(String),
}
```

**Critical: Account struct fields live in `idl.types`, NOT in `idl.accounts`.**

To get fields for an account type:

1. For each `IdlAccount` in `idl.accounts`, look up matching `IdlTypeDef` in `idl.types` by name
2. If the `IdlTypeDef.ty` is `Struct { fields: Some(Named(fields)) }`, those are the account fields to promote
3. If no matching type def is found, create account table with common columns only (no promoted columns)

### IDL Type -> PostgreSQL Type Mapping

```
IdlType::Bool      -> "BOOLEAN"
IdlType::U8        -> "SMALLINT"
IdlType::I8        -> "SMALLINT"
IdlType::U16       -> "INTEGER"      // max 65535 overflows SMALLINT
IdlType::I16       -> "SMALLINT"
IdlType::U32       -> "INTEGER"
IdlType::I32       -> "INTEGER"
IdlType::F32       -> "REAL"
IdlType::U64       -> "BIGINT"
IdlType::I64       -> "BIGINT"
IdlType::F64       -> "DOUBLE PRECISION"
IdlType::U128      -> "NUMERIC(39,0)"
IdlType::I128      -> "NUMERIC(39,0)"
IdlType::U256      -> "NUMERIC(78,0)"
IdlType::I256      -> "NUMERIC(78,0)"
IdlType::String    -> "TEXT"
IdlType::Pubkey    -> "TEXT"
IdlType::Bytes     -> "BYTEA"
IdlType::Option(inner) -> map inner type (column is nullable regardless)
IdlType::Array(U8, _)  -> "BYTEA"     // byte arrays special case

// NOT promoted (stay in JSONB `data` only):
IdlType::Vec(_)     -> None
IdlType::Array(_, _) -> None  (except [u8;N])
IdlType::Defined { struct/enum } -> None
IdlType::Generic(_) -> None
```

For `IdlType::Defined { name, .. }`:

- Look up in `idl.types` by name
- If `IdlTypeDefTy::Type { alias }` -> resolve alias recursively
- If `IdlTypeDefTy::Struct` or `Enum` -> not promoted (None)
- If not found -> not promoted (None)

### DDL Generation Pattern

All DDL uses `sqlx::raw_sql()` (bypasses prepared statements, required for DDL). Execute each statement within a transaction:

```rust
pub async fn generate_schema(
    pool: &PgPool,
    idl: &Idl,
    program_id: &str,
    schema_name: &str,
) -> Result<(), StorageError> {
    let statements = build_ddl_statements(idl, program_id, schema_name);
    let mut tx = pool.begin().await
        .map_err(|e| StorageError::DdlFailed(e.to_string()))?;
    for stmt in &statements {
        sqlx::raw_sql(stmt).execute(&mut *tx).await
            .map_err(|e| StorageError::DdlFailed(
                format!("DDL failed for {schema_name}: {e}")
            ))?;
    }
    tx.commit().await
        .map_err(|e| StorageError::DdlFailed(e.to_string()))?;
    Ok(())
}
```

`build_ddl_statements()` is a pure function returning `Vec<String>` — easy to unit test without a DB.

### quote_ident Implementation

Always double-quote all generated identifiers to avoid reserved word collisions:

```rust
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
```

### \_metadata Seeding

After DDL transaction succeeds, seed `_metadata` with program info. Use separate DML (not in DDL transaction):

```rust
let metadata_entries = vec![
    ("program_id", serde_json::json!(program_id)),
    ("program_name", serde_json::json!(&idl.metadata.name)),
    ("idl_hash", serde_json::json!(idl_hash)),
    ("idl_version", serde_json::json!(&idl.metadata.version)),
    ("schema_created_at", serde_json::json!(chrono::Utc::now().to_rfc3339())),
    ("account_types", serde_json::json!(account_type_names)),
    ("instruction_types", serde_json::json!(instruction_names)),
];
```

Use `INSERT ... ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value` for idempotency.

### Integration with ProgramRegistry

`ProgramRegistry::register_program()` in `src/registry.rs` currently:

1. Checks for duplicate
2. Fetches/uploads IDL
3. Inserts into `programs` table with `status = 'registered'`
4. Inserts into `indexer_state` table

Add after step 3: call `generate_schema()`. On success, update `programs.status` to `'schema_created'`. On failure, update to `'error'`.

Add to `RegistrationError`:

```rust
#[error("schema generation failed: {0}")]
SchemaFailed(#[from] StorageError),
```

Or handle `StorageError` explicitly in `register_program()` and map to existing `DatabaseError`.

### programs Table Status Transitions

```
'registered'      -> initial insert (story 2.2)
'schema_created'  -> after successful DDL generation (this story)
'error'           -> if DDL generation fails
```

The `programs` table already has `status TEXT NOT NULL DEFAULT 'initializing'` and `updated_at TIMESTAMPTZ`. Add `error_message TEXT` column if needed, or log the error and store just the status.

### Existing StorageError Enum

`src/storage/mod.rs` has:

```rust
pub enum StorageError {
    ConnectionFailed(String),
    DdlFailed(String),    // Use this for schema generation failures
    WriteFailed(String),
    CheckpointFailed(String),
}
```

`DdlFailed` is already the right variant — no new variants needed.

### Files Created/Modified by This Story

| File                    | Action | Purpose                                                                   |
| ----------------------- | ------ | ------------------------------------------------------------------------- |
| `src/storage/schema.rs` | Modify | Add `generate_schema()`, type mapping, DDL builders, `quote_ident`        |
| `src/registry.rs`       | Modify | Call `generate_schema()` after registration, handle errors, update status |

Only 2 files modified. The DDL engine is self-contained in `schema.rs`.

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests
- NO `println!` — use `tracing` macros
- NO `sqlx::query!()` compile-time macros — use runtime `sqlx::query()` for DML and `sqlx::raw_sql()` for DDL
- NO SQL string concatenation for VALUES — use bind parameters for DML (metadata seeding)
- NO `CREATE TABLE` without `IF NOT EXISTS`
- NO hardcoded program IDs
- DO double-quote ALL generated identifiers via `quote_ident()`
- DO make all promoted columns nullable (handles u64 overflow, partial data)
- DO use `sqlx::raw_sql()` for all DDL (CREATE SCHEMA/TABLE/INDEX)
- DO execute DDL in a transaction for atomicity

### What This Story Does NOT Do

- Does NOT implement the storage writer (`src/storage/writer.rs`) — that's story 3.4
- Does NOT implement the query builder (`src/storage/queries.rs`) — that's story 5.2
- Does NOT implement API endpoints — that's epic 5
- Does NOT implement schema evolution (ALTER TABLE) — deferred post-MVP
- Does NOT handle IDL changes or re-registration

### Testing Strategy

Unit tests can validate DDL output as strings without a DB:

- `build_ddl_statements()` returns `Vec<String>` — assert on content
- `map_idl_type_to_pg()` — exhaustive type mapping tests
- `generate_account_table()` — validate promoted column inclusion

For integration testing: the `generate_schema()` function runs against PostgreSQL. Add an `#[ignore]` integration test in `tests/` if desired, but unit tests on DDL string output are sufficient for this story.

### Project Structure Notes

- `src/storage/schema.rs` owns ALL DDL generation (consistent with architecture: "schema.rs — DDL generator: IDL -> CREATE TABLE/INDEX, column promotion")
- `src/registry.rs` orchestrates: IDL fetch -> DB insert -> schema generation
- `sanitize_identifier()` and `derive_schema_name()` already exist in `schema.rs` from story 2.2

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-2-program-registration-idl-acquisition.md#Story 2.3]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Data Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md]
- [Source: _bmad-output/planning-artifacts/research/agent-2a-idl-to-ddl-mapping.md]
- [Source: _bmad-output/planning-artifacts/research/agent-2b-hybrid-storage-architecture.md]
- [Source: _bmad-output/implementation-artifacts/2-2-manual-idl-upload-and-program-registration.md]
- [Source: anchor-lang-idl-spec 0.1.0 lib.rs — IdlType enum, Idl struct, IdlTypeDef]

## Review Findings

Code review performed 2026-04-06 using 3-layer adversarial review (Blind Hunter, Edge Case Hunter, Acceptance Auditor). Party mode consensus reached with Winston (Architect), Amelia (Dev), Quinn (QA).

### Applied Patches (6 fixes applied, testing blocked by unrelated issue)

- [x] [Review][Patch] P1: Integration test asserts stale status `"registered"` — updated to `"schema_created"` [`tests/registration_test.rs:69,84`]
- [x] [Review][Patch] P2: No cycle detection in type alias resolution — added depth counter (max 16) to `resolve_defined_type`/`map_idl_type_to_pg` + unit test for circular alias [`src/storage/schema.rs:77-127`]
- [x] [Review][Patch] P3: Promoted column name collision with system columns — added `RESERVED_ACCOUNT_COLUMNS` set, skip promotion for colliding names + unit test [`src/storage/schema.rs:129-146`]
- [x] [Review][Patch] P4: Log field `tables` mislabeled — renamed to `statements` [`src/storage/schema.rs:363`]
- [x] [Review][Patch] P5: Tuple-variant account struct fields silently dropped — added `warn!` for non-Named struct variants in `build_ddl_statements` [`src/storage/schema.rs:306-332`]
- [x] [Review][Patch] P6: Schema generation error path swallows status update failure — replaced `let _` with `if let Err` + `error!` log [`src/registry.rs:142-155`]

### Deferred Findings (pre-existing, not from story 2-3)

- [x] [Review][Defer] W1: `DROP SCHEMA` in `delete_program` uses string interpolation instead of `quote_ident()` [`src/api/handlers.rs:173`] — deferred, story 5-1 scope
- [x] [Review][Defer] W2: TOCTOU race in `write_registration` at default isolation level [`src/registry.rs:186-209`] — deferred, mitigated by `Arc<RwLock>`, noted for story 5-1 lock optimization
- [x] [Review][Defer] W3: Build error in `src/api/handlers.rs` — axum Handler trait not satisfied due to `!Send` RwLockWriteGuard — deferred, story 5-1 in-progress stub

### Dismissed (8 findings)

False positives or spec-aligned: compilation failure claim (handler is stubbed), PG index name collision (schema-scoped), U16→INTEGER (matches spec), HashMap usage (correctly used), program_id logging param (valid), metadata outside DDL tx (per spec design), byte-truncation boundary (mitigated by ASCII sanitization), COALESCE sentinel (-1 is spec-aligned).

### D1 Decision (U32 → INTEGER vs BIGINT)

User dismissed — to be addressed separately, not part of this story's scope.

### Remaining Work

1. **Testing blocked**: The pre-existing compile error in `src/api/handlers.rs` (story 5-1 `register_program` handler stub — `!Send` issue with `RwLockWriteGuard`) prevents `cargo test --lib` from running. All 6 review patches are applied but **unit tests have not been verified** against the patched code. The original code (pre-story-2.3) passes all 75 lib tests. The story 2-3 code added 32 new tests (107 total) which passed before the review patches were applied.
2. **To verify**: Once the handler compile error is fixed (story 5-1), run `cargo test --lib` and `cargo clippy` to confirm all review patches are clean.
3. **Story cannot be marked `done` until tests pass on the patched code.**

## Dev Agent Record

### Agent Model Used

claude-opus-4-6 (code review session 2026-04-06)

### Debug Log References

### Completion Notes List

- Implementation of all 10 tasks (quote_ident, type mapping, account table DDL, instructions table, metadata/checkpoint tables, indexes, build_ddl_statements, generate_schema, seed_metadata, ProgramRegistry integration) was completed prior to this review session.
- 6 review patches applied during this session (P1-P6).
- 2 new unit tests added: circular alias detection, reserved column name skipping.
- Build and test verification pending resolution of story 5-1 handler compile error.

### File List

| File                         | Changes                                                                                                                                                      |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `src/storage/schema.rs`      | Core DDL engine: type mapping, table/index generation, schema generation. Review patches: cycle detection, reserved columns, log fix, tuple variant warning. |
| `src/registry.rs`            | ProgramRegistry integration: generate_schema + seed_metadata calls, status transitions, error handling. Review patch: error path logging.                    |
| `tests/registration_test.rs` | Integration test status assertion updated to `"schema_created"`.                                                                                             |
