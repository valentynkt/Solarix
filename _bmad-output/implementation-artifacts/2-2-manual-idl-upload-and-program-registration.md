# Story 2.2: Manual IDL Upload & Program Registration

Status: review

## Story

As a user,
I want to upload an IDL manually and register a program for indexing,
so that I can index programs that don't have on-chain IDLs or use custom IDL modifications.

## Acceptance Criteria

1. **AC1: Manual IDL upload via IdlManager**
   - **Given** a valid IDL JSON provided via the IdlManager's manual upload path
   - **When** the IdlManager processes the upload
   - **Then** it parses and validates the IDL (same validation as on-chain fetch from story 2.1)
   - **And** it caches the parsed IDL keyed by the provided program ID
   - **And** the IDL source is recorded as `Manual` (vs `OnChain` or `Bundled`)

2. **AC2: ProgramRegistry struct**
   - **Given** the `ProgramRegistry` struct
   - **When** I inspect it
   - **Then** it wraps `IdlManager` + per-program schema metadata
   - **And** it is shared across pipeline and API via `Arc<RwLock<ProgramRegistry>>`
   - **And** a `register_program()` method orchestrates: IDL fetch/upload -> validate -> store metadata in DB -> return program info

3. **AC3: Database registration**
   - **Given** a program is registered
   - **When** the registration completes
   - **Then** the `programs` table is updated with: `program_id`, `program_name` (from IDL `metadata.name`), `schema_name` (derived via `sanitize_identifier`), `idl_hash` (SHA-256), `idl_source`, `status = 'registered'`
   - **And** the `indexer_state` table is populated with initial state for the program (`status = 'initializing'`, zeroed counters)

4. **AC4: Shared types in types.rs**
   - **Given** the `types.rs` shared types module
   - **When** I inspect it
   - **Then** `DecodedInstruction` includes fields: `signature`, `slot`, `block_time`, `instruction_name`, `args` (serde_json::Value), `program_id`, `accounts` (Vec<String>), `instruction_index` (u8), `inner_index` (Option<u8>)
   - **And** `DecodedAccount` includes fields: `pubkey`, `slot_updated`, `lamports`, `data` (serde_json::Value), `account_type`, `program_id`

5. **AC5: Duplicate registration rejection**
   - **Given** a program ID that is already registered
   - **When** the user attempts to register it again
   - **Then** the system returns an appropriate error indicating the program is already registered

6. **AC6: Schema name derivation**
   - **Given** a program ID and IDL name
   - **When** the schema name is derived
   - **Then** it uses the format `{sanitized_name}_{lowercase_first_8_of_base58_program_id}`
   - **And** the `sanitize_identifier()` function strips non-alphanumeric chars (except underscores), lowercases, prepends underscore if starts with digit, falls back to `_unnamed` if empty, truncates to 63 bytes

## Tasks / Subtasks

- [x] Task 1: Expand `types.rs` with full pipeline types (AC: #4)
  - [x] Update `DecodedInstruction` to include all fields: `signature`, `slot`, `block_time` (Option<i64>), `instruction_name`, `args` (Value), `program_id`, `accounts` (Vec<String>), `instruction_index` (u8), `inner_index` (Option<u8>)
  - [x] Update `DecodedAccount` to include all fields: `pubkey`, `slot_updated` (u64), `lamports` (u64), `data` (Value), `account_type`, `program_id`
  - [x] Verify `BlockData` and `TransactionData` structs are adequate or update them
- [x] Task 2: Add manual upload method to `IdlManager` in `src/idl/mod.rs` (AC: #1)
  - [x] Add `pub fn upload_idl(&mut self, program_id: &str, idl_json: &str) -> Result<&Idl, IdlError>`
  - [x] Reuse existing `validate_idl()` and `compute_idl_hash()` from story 2.1
  - [x] Cache with `IdlSource::Manual`
  - [x] Return reference to cached `Idl`
- [x] Task 3: Implement `sanitize_identifier()` utility (AC: #6)
  - [x] Add `pub fn sanitize_identifier(name: &str) -> String` (either in `src/idl/mod.rs` or `src/storage/schema.rs`)
  - [x] Strip non-alphanumeric chars except underscores
  - [x] Lowercase the result
  - [x] Prepend `_` if starts with digit
  - [x] Fall back to `_unnamed` if empty after sanitization
  - [x] Truncate to 63 bytes on byte boundaries
  - [x] Add `pub fn derive_schema_name(idl_name: &str, program_id: &str) -> String` — combines sanitized name + first 8 lowercase chars of program ID
- [x] Task 4: Implement `ProgramRegistry` struct (AC: #2, #3, #5)
  - [x] Define `ProgramRegistry` in a new file `src/registry.rs` (or in `src/idl/mod.rs` — decide based on size)
  - [x] Fields: `idl_manager: IdlManager`, `pool: PgPool`
  - [x] Implement `ProgramRegistry::new(idl_manager: IdlManager, pool: PgPool) -> Self`
  - [x] Implement `pub async fn register_program(&mut self, program_id: &str, idl_json: Option<&str>) -> Result<ProgramInfo, RegistrationError>`
    - If `idl_json` is `Some`, use `idl_manager.upload_idl()` (manual upload)
    - If `idl_json` is `None`, use `idl_manager.get_idl()` (on-chain/bundled cascade)
    - Check for duplicate: query `programs` table first, return error if exists
    - Derive `schema_name` via `derive_schema_name(idl.metadata.name, program_id)`
    - Insert into `programs` table with `status = 'registered'`
    - Insert into `indexer_state` table with initial state
    - Return `ProgramInfo` struct
  - [x] Define `ProgramInfo` struct: `program_id`, `program_name`, `schema_name`, `idl_hash`, `idl_source`, `status`
  - [x] Define `RegistrationError` enum with variants: `IdlError(IdlError)`, `AlreadyRegistered(String)`, `DatabaseError(String)`
  - [x] Implement `pub fn get_idl(&self, program_id: &str) -> Option<&Idl>` — delegate to IdlManager cache
  - [x] Implement `pub fn list_programs(&self) -> Vec<&str>` — return cached program IDs
- [x] Task 5: Integrate `ProgramRegistry` into `AppState` and `main.rs` (AC: #2)
  - [x] Update `AppState` in `src/api/mod.rs` to include `pub registry: Arc<RwLock<ProgramRegistry>>`
  - [x] Update `main.rs` to create `IdlManager`, then `ProgramRegistry`, wrap in `Arc<RwLock<>>`, pass to `AppState`
  - [x] Add `pub mod registry;` to `src/lib.rs` (if using separate file)
  - [x] Ensure router still compiles with updated `AppState`
- [x] Task 6: Add unit tests (AC: #1, #3, #5, #6)
  - [x] Test `sanitize_identifier` with normal input, digits-first, special chars, empty, unicode, 63-byte truncation
  - [x] Test `derive_schema_name` produces expected format
  - [x] Test `upload_idl` validates and caches correctly
  - [x] Test `upload_idl` rejects invalid IDL (no metadata.spec)
  - [x] Test duplicate registration detection (mock or in-memory)
- [x] Task 7: Add integration test for DB registration (AC: #3, #5)
  - [x] Create `tests/registration_test.rs` with `#[ignore]`
  - [x] Test: register program -> verify `programs` row exists with correct columns
  - [x] Test: register program -> verify `indexer_state` row exists
  - [x] Test: register same program twice -> returns `AlreadyRegistered` error
- [x] Task 8: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `cargo test` passes all unit tests

## Dev Notes

### Current Codebase State (Post Story 2.1)

Story 2.1 implements:

- `IdlManager` with `HashMap<String, CachedIdl>` cache, on-chain fetch, bundled fallback
- `CachedIdl` struct with `idl: Idl`, `hash: String`, `source: IdlSource`
- `IdlSource` enum: `OnChain`, `Bundled`, `Manual`
- `validate_idl()` and `compute_idl_hash()` functions
- `IdlError` enum with 5 variants
- `fetch_idl_from_chain()` and `fetch_idl_from_bundled()` in `src/idl/fetch.rs`

This story builds on those to add manual upload + ProgramRegistry + DB writes.

### Existing types.rs (Needs Expansion)

Current `types.rs` has minimal stubs:

```rust
pub struct DecodedInstruction {
    pub program_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

pub struct DecodedAccount {
    pub program_id: String,
    pub account_type: String,
    pub pubkey: String,
    pub data: serde_json::Value,
}
```

These need expansion per AC4. The `name` field in `DecodedInstruction` should be renamed to `instruction_name` for consistency with the `_instructions` table column name (story 2.3). Add the missing fields: `signature`, `slot`, `block_time`, `accounts`, `instruction_index`, `inner_index` for instructions; `slot_updated`, `lamports` for accounts.

### Existing AppState (Needs ProgramRegistry)

Current `AppState`:

```rust
pub struct AppState {
    pub pool: PgPool,
    pub start_time: Instant,
}
```

Add `pub registry: Arc<RwLock<ProgramRegistry>>` field. The `RwLock` should be `tokio::sync::RwLock` (async-aware) since registration involves async DB operations while holding the lock.

### ProgramRegistry Location Decision

The `ProgramRegistry` should live in its own `src/registry.rs` file (not in `src/idl/mod.rs`) because:

- It depends on both `idl` and `storage` modules (cross-cutting concern)
- It will be used by `api` handlers and `pipeline` orchestrator
- Keeps `idl/mod.rs` focused on IDL parsing/caching

Add `pub mod registry;` to `src/lib.rs`.

### Database Write Pattern for Registration

Use `sqlx::query()` with bind parameters (NOT `raw_sql` — that's for DDL):

```rust
sqlx::query(
    r#"INSERT INTO "programs" ("program_id", "program_name", "schema_name", "idl_hash", "idl_source", "status")
       VALUES ($1, $2, $3, $4, $5, 'registered')"#
)
.bind(program_id)
.bind(&idl.metadata.name)
.bind(&schema_name)
.bind(&idl_hash)
.bind(idl_source_str)
.execute(pool)
.await
.map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;
```

For `indexer_state`:

```rust
sqlx::query(
    r#"INSERT INTO "indexer_state" ("program_id", "status", "total_instructions", "total_accounts")
       VALUES ($1, 'initializing', 0, 0)"#
)
.bind(program_id)
.execute(pool)
.await
.map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;
```

### Duplicate Detection

Check before insert to provide a clear error message:

```rust
let exists = sqlx::query_scalar::<_, bool>(
    r#"SELECT EXISTS(SELECT 1 FROM "programs" WHERE "program_id" = $1)"#
)
.bind(program_id)
.fetch_one(pool)
.await
.map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

if exists {
    return Err(RegistrationError::AlreadyRegistered(program_id.to_string()));
}
```

### RegistrationError Enum

```rust
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("IDL error: {0}")]
    Idl(#[from] IdlError),

    #[error("program {0} is already registered")]
    AlreadyRegistered(String),

    #[error("database error: {0}")]
    DatabaseError(String),
}
```

This lives in `src/registry.rs` alongside `ProgramRegistry`.

### IdlSource to String Conversion

The `idl_source` column in `programs` table expects string values: `'onchain'`, `'file'`, `'bundled'`, `'manual'`. Map from `IdlSource` enum:

```rust
impl IdlSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            IdlSource::OnChain => "onchain",
            IdlSource::Bundled => "bundled",
            IdlSource::Manual => "manual",
        }
    }
}
```

### sanitize_identifier Implementation

Place in `src/storage/schema.rs` since it's used by schema generation (story 2.3) and registration (this story). It's a pure function with no dependencies.

```rust
pub fn sanitize_identifier(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_lowercase();

    let sanitized = if sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{sanitized}")
    } else {
        sanitized
    };

    let sanitized = if sanitized.is_empty() {
        "_unnamed".to_string()
    } else {
        sanitized
    };

    // Truncate to 63 bytes on byte boundary
    if sanitized.len() > 63 {
        let mut end = 63;
        while !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        sanitized[..end].to_string()
    } else {
        sanitized
    }
}

pub fn derive_schema_name(idl_name: &str, program_id: &str) -> String {
    let name_part = sanitize_identifier(idl_name);
    let id_prefix = program_id.chars().take(8).collect::<String>().to_lowercase();
    let full = format!("{name_part}_{id_prefix}");

    // Final truncation to 63 bytes
    if full.len() > 63 {
        let mut end = 63;
        while !full.is_char_boundary(end) {
            end -= 1;
        }
        full[..end].to_string()
    } else {
        full
    }
}
```

### Arc<RwLock<ProgramRegistry>> Pattern

Use `tokio::sync::RwLock` (NOT `std::sync::RwLock`) because:

- `register_program()` is async (DB queries)
- Holding a `std::sync::RwLock` across `.await` points would block the Tokio runtime

```rust
// In main.rs:
let idl_manager = IdlManager::new(config.rpc_url.clone());
let registry = ProgramRegistry::new(idl_manager, pool.clone());
let registry = Arc::new(RwLock::new(registry));

let state = Arc::new(AppState {
    pool: pool.clone(),
    start_time: Instant::now(),
    registry: registry.clone(),
});
```

### PipelineOrchestrator Integration (Not This Story)

`PipelineOrchestrator` will receive `Arc<RwLock<ProgramRegistry>>` in a future story (3.5). For now, just ensure the registry is constructed in `main.rs` and passed to `AppState`. The pipeline stub doesn't use it yet.

### What This Story Does NOT Do

- Does NOT implement API endpoint `POST /api/programs` (that's story 5.1)
- Does NOT generate PostgreSQL schema/DDL from IDL (that's story 2.3)
- Does NOT implement the pipeline orchestrator (that's story 3.5)
- Does NOT implement decoder integration (that's epic 3)
- Does NOT add `_checkpoints` or `_metadata` tables (that's story 2.3)

### Error Conversion Chain

```
IdlError -> RegistrationError (via #[from])
RegistrationError -> ApiError (story 5.1, not this story)
StorageError -> PipelineError (already exists)
```

### Import Ordering Convention

```rust
// std library
use std::sync::Arc;

// external crates
use anchor_lang_idl_spec::Idl;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{info, warn};

// internal crate
use crate::config::Config;
use crate::idl::{IdlError, IdlManager, IdlSource};
```

### Files Created/Modified by This Story

| File                         | Action | Purpose                                                               |
| ---------------------------- | ------ | --------------------------------------------------------------------- |
| `src/types.rs`               | Modify | Expand `DecodedInstruction` and `DecodedAccount` with full fields     |
| `src/registry.rs`            | Create | `ProgramRegistry`, `ProgramInfo`, `RegistrationError`                 |
| `src/idl/mod.rs`             | Modify | Add `upload_idl()` method, `IdlSource::as_str()`                      |
| `src/storage/schema.rs`      | Modify | Add `sanitize_identifier()` and `derive_schema_name()`                |
| `src/api/mod.rs`             | Modify | Add `registry` field to `AppState`                                    |
| `src/main.rs`                | Modify | Create `ProgramRegistry`, wrap in `Arc<RwLock<>>`, pass to `AppState` |
| `src/lib.rs`                 | Modify | Add `pub mod registry;`                                               |
| `tests/registration_test.rs` | Create | Integration test for DB registration                                  |

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests
- NO `println!` — use `tracing` macros
- NO `sqlx::raw_sql()` for DML (INSERT/SELECT) — use `sqlx::query()` with bind params
- NO SQL string concatenation — use bind parameters `$1, $2, ...`
- NO `std::sync::RwLock` — use `tokio::sync::RwLock` (async context)
- NO separate `error.rs` file — `RegistrationError` lives in `src/registry.rs`
- NO hardcoded program IDs
- NO creating `_checkpoints` or DDL tables (that's story 2.3)

### Project Structure Notes

- `src/registry.rs` is a new module — cross-cutting concern between `idl`, `storage`, and `api`
- `sanitize_identifier()` in `src/storage/schema.rs` — shared with story 2.3 DDL generation
- `types.rs` field expansion is backwards-compatible — existing code compiles if it only uses the fields it needs (struct fields are all `pub`)
- `AppState.registry` addition requires updating all places that construct `AppState` (only `main.rs`)

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-2-program-registration-idl-acquisition.md#Story 2.2]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md]
- [Source: _bmad-output/implementation-artifacts/2-1-idl-manager-and-on-chain-fetch.md]
- [Source: _bmad-output/implementation-artifacts/1-2-database-connection-and-system-table-bootstrap.md]

## Dev Agent Record

### Agent Model Used

claude-opus-4-6

### Debug Log References

None

### Completion Notes List

- Expanded `DecodedInstruction` with all pipeline fields (`signature`, `slot`, `block_time`, `instruction_name`, `accounts`, `instruction_index`, `inner_index`). Renamed `name` -> `instruction_name` for DB column consistency. Added `from_decoded()` factory for decoder output.
- Expanded `DecodedAccount` with `slot_updated`, `lamports` fields. Added `from_decoded()` factory for decoder output.
- Added `block_time` field to `BlockData`.
- Updated `ChainparserDecoder` to use `from_decoded()` factories, keeping all existing decoder tests passing.
- Replaced `insert_manual()` with `upload_idl()` that validates JSON, checks `metadata.spec`, computes hash, and caches with `Manual` source.
- Added `IdlSource::as_str()` for DB serialization and `cached_program_ids()` for listing.
- Implemented `sanitize_identifier()` and `derive_schema_name()` in `src/storage/schema.rs` with 10 unit tests.
- Created `ProgramRegistry` in `src/registry.rs` with `register_program()`, `get_idl()`, `list_programs()`. Includes `ProgramInfo` and `RegistrationError` types.
- Integrated `ProgramRegistry` into `AppState` via `Arc<RwLock<ProgramRegistry>>` (tokio::sync::RwLock for async safety).
- Added 2 integration tests in `tests/registration_test.rs` (with cleanup for idempotent test runs).
- All 74 unit tests pass, 0 clippy warnings, formatting clean.

### File List

- `src/types.rs` (modified) - Expanded DecodedInstruction/DecodedAccount with full fields, added from_decoded() factories, added block_time to BlockData
- `src/idl/mod.rs` (modified) - Replaced insert_manual with upload_idl, added IdlSource::as_str(), cached_program_ids()
- `src/decoder/mod.rs` (modified) - Updated to use from_decoded() factories and instruction_name field
- `src/storage/schema.rs` (modified) - Added sanitize_identifier() and derive_schema_name()
- `src/registry.rs` (created) - ProgramRegistry, ProgramInfo, RegistrationError
- `src/api/mod.rs` (modified) - Added registry field to AppState
- `src/main.rs` (modified) - Create IdlManager, ProgramRegistry, wrap in Arc<RwLock<>>
- `src/lib.rs` (modified) - Added pub mod registry
- `tests/registration_test.rs` (created) - Integration tests for DB registration

### Change Log

- 2026-04-05: Story 2.2 implementation complete - Manual IDL upload, ProgramRegistry, DB registration, shared types expansion
