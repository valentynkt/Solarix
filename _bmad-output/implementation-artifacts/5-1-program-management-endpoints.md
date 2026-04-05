# Story 5.1: Program Management Endpoints

Status: ready-for-dev

## Story

As a user,
I want to register, list, inspect, and deregister programs via REST API,
so that I can manage which programs are indexed without touching the database directly.

## Acceptance Criteria

1. **AC1: Router with program management routes**
   - **Given** the axum Router in `src/api/mod.rs`
   - **When** I inspect it
   - **Then** it defines routes using axum 0.8 `{param}` syntax (not `:param`)
   - **And** routes are nested under `/api/programs`
   - **And** `AppState` contains: `PgPool`, `Arc<RwLock<ProgramRegistry>>`, `Instant` (existing), plus `Config` (new)

2. **AC2: POST /api/programs (auto-fetch)**
   - **Given** a `POST /api/programs` request with `{ "program_id": "<base58>" }`
   - **When** the handler processes it
   - **Then** it triggers IDL fetch (on-chain -> bundled -> error) via `ProgramRegistry::register_program`
   - **And** returns HTTP 202 Accepted with `{ "data": { "program_id": "...", "status": "registered", "idl_source": "onchain" }, "meta": { "message": "Program registered. Indexing will begin shortly." } }`

3. **AC3: POST /api/programs (manual IDL upload)**
   - **Given** a `POST /api/programs` request with `{ "program_id": "...", "idl": {...} }`
   - **When** the handler processes it
   - **Then** it uses the provided IDL (manual upload path) via `ProgramRegistry::register_program(program_id, Some(idl_json))`
   - **And** returns HTTP 202 with `idl_source: "manual"`

4. **AC4: GET /api/programs**
   - **Given** a `GET /api/programs` request
   - **When** the handler processes it
   - **Then** it queries the `programs` table for all registered programs
   - **And** returns `{ "data": [...], "meta": { "total": N } }` with fields: `program_id`, `program_name`, `status`, `created_at`

5. **AC5: GET /api/programs/{id}**
   - **Given** a `GET /api/programs/{id}` request where the program exists
   - **When** the handler processes it
   - **Then** it returns program details: `program_id`, `program_name`, `schema_name`, `idl_source`, `idl_hash`, `status`, `created_at`, `updated_at`, plus indexing stats from `indexer_state` (`total_instructions`, `total_accounts`, `last_processed_slot`)
   - **And** when the program does not exist, returns HTTP 404 with `{ "error": { "code": "PROGRAM_NOT_FOUND", "message": "..." } }`

6. **AC6: DELETE /api/programs/{id} (soft)**
   - **Given** a `DELETE /api/programs/{id}` request without `drop_tables=true`
   - **When** the handler processes it
   - **Then** the program's status is updated to `stopped` in the `programs` table
   - **And** data and schema are retained

7. **AC7: DELETE /api/programs/{id}?drop_tables=true (hard)**
   - **Given** a `DELETE /api/programs/{id}?drop_tables=true`
   - **When** the handler processes it
   - **Then** it executes `DROP SCHEMA "{schema_name}" CASCADE`
   - **And** removes entries from `programs` and `indexer_state` tables
   - **And** removes the IDL from the in-memory cache

8. **AC8: ApiError with IntoResponse**
   - **Given** the `ApiError` enum in `src/api/mod.rs`
   - **When** I inspect it
   - **Then** it includes variants: `ProgramNotFound(String)`, `ProgramAlreadyRegistered(String)`, `InvalidFilter(String)`, `QueryFailed(String)`, `IdlError(String)`, `StorageError(String)`, `InvalidRequest(String)`
   - **And** it implements `axum::response::IntoResponse` mapping to HTTP status codes: 404, 409, 400, 500
   - **And** error responses use `{ "error": { "code": "MACHINE_CODE", "message": "human readable" } }` format

9. **AC9: Duplicate registration returns 409**
   - **Given** a `POST /api/programs` with an already-registered program ID
   - **When** the handler processes it
   - **Then** it returns HTTP 409 Conflict with `{ "error": { "code": "PROGRAM_ALREADY_REGISTERED", "message": "..." } }`

## Tasks / Subtasks

- [ ] Task 1: Expand `ApiError` enum and implement `IntoResponse` (AC: #8)
  - [ ] Add new variants: `ProgramNotFound`, `ProgramAlreadyRegistered`, `InvalidRequest`, `IdlError`, `StorageError` (keep existing `InvalidFilter`, `QueryFailed`)
  - [ ] Implement `IntoResponse` for `ApiError` mapping each variant to status + JSON error body
  - [ ] Implement `From<RegistrationError> for ApiError` to convert registry errors
- [ ] Task 2: Add `Config` to `AppState` (AC: #1)
  - [ ] Add `pub config: Config` field to `AppState`
  - [ ] Update `main.rs` to pass `config` to `AppState` (needs `Config` to implement `Clone` — it already does)
- [ ] Task 3: Define request/response types (AC: #2, #3, #4, #5)
  - [ ] `RegisterProgramRequest`: `program_id: String`, `idl: Option<serde_json::Value>`
  - [ ] Response structs or use `serde_json::json!` macro for dynamic responses (prefer `json!` for consistency with architecture)
- [ ] Task 4: Implement `register_program` handler (AC: #2, #3, #9)
  - [ ] Accept `Json<RegisterProgramRequest>`
  - [ ] If `idl` is `Some`, serialize to string, pass to `registry.register_program(id, Some(idl_str))`
  - [ ] If `idl` is `None`, call `registry.register_program(id, None)`
  - [ ] Convert `RegistrationError` to `ApiError`
  - [ ] Return HTTP 202 with standard envelope
- [ ] Task 5: Implement `list_programs` handler (AC: #4)
  - [ ] Query `SELECT program_id, program_name, status, created_at FROM programs ORDER BY created_at DESC`
  - [ ] Use `sqlx::query_as` or `sqlx::query().fetch_all()` with row mapping
  - [ ] Return `{ "data": [...], "meta": { "total": N } }`
- [ ] Task 6: Implement `get_program` handler (AC: #5)
  - [ ] Query `programs` JOIN `indexer_state` on `program_id`
  - [ ] Return full program details or 404
- [ ] Task 7: Implement `delete_program` handler (AC: #6, #7)
  - [ ] Parse `drop_tables` query param
  - [ ] If `drop_tables=true`: DROP SCHEMA CASCADE, DELETE from `indexer_state`, DELETE from `programs`, remove from IDL cache
  - [ ] If not: UPDATE `programs` SET `status = 'stopped'`
  - [ ] Return 200 with confirmation or 404
- [ ] Task 8: Update router with all program routes (AC: #1)
  - [ ] Nest program routes under `/api/programs`
  - [ ] Wire: `POST /` -> register_program, `GET /` -> list_programs, `GET /{id}` -> get_program, `DELETE /{id}` -> delete_program
  - [ ] Keep existing `/health` route
- [ ] Task 9: Add unit tests (AC: all)
  - [ ] Test `ApiError::IntoResponse` produces correct status codes and JSON structure
  - [ ] Test `RegisterProgramRequest` deserialization with and without `idl` field
- [ ] Task 10: Verify (AC: all)
  - [ ] `cargo build` compiles
  - [ ] `cargo clippy` passes
  - [ ] `cargo fmt -- --check` passes
  - [ ] `cargo test` passes all unit tests

## Dev Notes

### Current Codebase State

**AppState** (`src/api/mod.rs:14-18`):

```rust
pub struct AppState {
    pub pool: PgPool,
    pub start_time: Instant,
    pub registry: Arc<RwLock<ProgramRegistry>>,
}
```

Needs addition of `pub config: Config` — the epic AC says AppState contains Config. The `Config` struct already derives `Clone`.

**Current ApiError** (`src/api/mod.rs:21-31`):

```rust
pub enum ApiError {
    InvalidFilter(String),
    ProgramNotFound(String),
    QueryFailed(String),
}
```

Missing `IntoResponse` impl (explicitly deferred from stories 1.1 and 1.3). This story implements it.

**Current Router** (`src/api/mod.rs:33-37`):

```rust
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .with_state(state)
}
```

Needs program management routes nested under `/api/programs`.

**ProgramRegistry** (`src/registry.rs`):

- `register_program(&mut self, program_id, idl_json)` — already handles both auto-fetch and manual upload
- `get_idl(&self, program_id)` — returns cached IDL
- `list_programs(&self)` — returns cached program IDs (in-memory only, not from DB)
- **Important**: `register_program` takes `&mut self`, so the handler needs a **write lock** on `Arc<RwLock<ProgramRegistry>>`

**System Tables** (`src/storage/mod.rs:88-120`):

- `programs` table: `program_id` (PK), `program_name`, `schema_name`, `idl_hash`, `idl_source`, `status`, `created_at`, `updated_at`
- `indexer_state` table: `program_id` (PK, FK), `status`, `last_processed_slot`, `last_heartbeat`, `error_message`, `total_instructions`, `total_accounts`

### Router Structure (axum 0.8 syntax)

```rust
use axum::routing::{get, post, delete};

pub fn router(state: Arc<AppState>) -> Router {
    let program_routes = Router::new()
        .route("/", post(handlers::register_program).get(handlers::list_programs))
        .route("/{id}", get(handlers::get_program).delete(handlers::delete_program));

    Router::new()
        .nest("/api/programs", program_routes)
        .route("/health", get(handlers::health))
        .with_state(state)
}
```

axum 0.8 uses `{param}` syntax (not `:param`). The `id` path param extracts as `Path(id): Path<String>`.

### Handler Signatures

All handlers follow this pattern:

```rust
pub async fn register_program(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterProgramRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut registry = state.registry.write().await;
    // ...
}
```

For `GET`/`DELETE` with path params:

```rust
pub async fn get_program(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // ...
}
```

For `DELETE` with query params:

```rust
#[derive(Deserialize)]
pub struct DeleteProgramQuery {
    #[serde(default)]
    drop_tables: bool,
}

pub async fn delete_program(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DeleteProgramQuery>,
) -> Result<Json<Value>, ApiError> {
    // ...
}
```

### ApiError IntoResponse Implementation

```rust
use axum::response::{IntoResponse, Response};

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            ApiError::ProgramNotFound(id) => (
                StatusCode::NOT_FOUND,
                "PROGRAM_NOT_FOUND",
                format!("Program '{}' is not registered", id),
            ),
            ApiError::ProgramAlreadyRegistered(id) => (
                StatusCode::CONFLICT,
                "PROGRAM_ALREADY_REGISTERED",
                format!("Program '{}' is already registered", id),
            ),
            ApiError::InvalidFilter(msg) => (
                StatusCode::BAD_REQUEST,
                "INVALID_FILTER",
                msg.clone(),
            ),
            ApiError::InvalidRequest(msg) => (
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                msg.clone(),
            ),
            ApiError::IdlError(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "IDL_ERROR",
                msg.clone(),
            ),
            ApiError::StorageError(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "STORAGE_ERROR",
                "Internal storage error".to_string(),
            ),
            ApiError::QueryFailed(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "QUERY_FAILED",
                "Query execution failed".to_string(),
            ),
        };

        let body = json!({
            "error": {
                "code": code,
                "message": message,
            }
        });

        (status, Json(body)).into_response()
    }
}
```

**Important:** Don't expose internal DB error details in responses (security). Log them at `error!` level instead.

### RegistrationError -> ApiError Conversion

```rust
impl From<RegistrationError> for ApiError {
    fn from(err: RegistrationError) -> Self {
        match err {
            RegistrationError::AlreadyRegistered(id) => ApiError::ProgramAlreadyRegistered(id),
            RegistrationError::Idl(e) => ApiError::IdlError(e.to_string()),
            RegistrationError::DatabaseError(msg) => ApiError::StorageError(msg),
        }
    }
}
```

### Database Queries for Handlers

**List programs (GET /api/programs):**

```rust
let rows = sqlx::query(
    r#"SELECT "program_id", "program_name", "status", "created_at"
       FROM "programs" ORDER BY "created_at" DESC"#
)
.fetch_all(&state.pool)
.await
.map_err(|e| ApiError::QueryFailed(e.to_string()))?;
```

Map rows to JSON using `row.get::<String, _>("program_id")`, etc.

**Get program (GET /api/programs/{id}):**

```rust
let row = sqlx::query(
    r#"SELECT p."program_id", p."program_name", p."schema_name",
            p."idl_source", p."idl_hash", p."status",
            p."created_at", p."updated_at",
            i."total_instructions", i."total_accounts", i."last_processed_slot"
       FROM "programs" p
       LEFT JOIN "indexer_state" i ON p."program_id" = i."program_id"
       WHERE p."program_id" = $1"#
)
.bind(&id)
.fetch_optional(&state.pool)
.await
.map_err(|e| ApiError::QueryFailed(e.to_string()))?
.ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
```

**Delete program (hard - DROP SCHEMA CASCADE):**

```rust
// Get schema_name first
let schema_name: String = sqlx::query_scalar(
    r#"SELECT "schema_name" FROM "programs" WHERE "program_id" = $1"#
)
.bind(&id)
.fetch_optional(&state.pool)
.await
.map_err(|e| ApiError::QueryFailed(e.to_string()))?
.ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;

// Drop schema (DDL — use raw_sql with format!, schema_name is sanitized at registration)
let drop_ddl = format!(r#"DROP SCHEMA IF EXISTS "{schema_name}" CASCADE"#);
sqlx::raw_sql(&drop_ddl)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::StorageError(e.to_string()))?;

// Delete from indexer_state first (FK constraint), then programs
sqlx::query(r#"DELETE FROM "indexer_state" WHERE "program_id" = $1"#)
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::StorageError(e.to_string()))?;

sqlx::query(r#"DELETE FROM "programs" WHERE "program_id" = $1"#)
    .bind(&id)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::StorageError(e.to_string()))?;
```

**Note:** `schema_name` is safe to use in `format!` because it was sanitized via `sanitize_identifier()` at registration time (only `[a-z0-9_]` chars). It is NOT user input at this point — it's read from the DB where it was stored during registration.

**Delete program (soft):**

```rust
let result = sqlx::query(
    r#"UPDATE "programs" SET "status" = 'stopped' WHERE "program_id" = $1"#
)
.bind(&id)
.execute(&state.pool)
.await
.map_err(|e| ApiError::StorageError(e.to_string()))?;

if result.rows_affected() == 0 {
    return Err(ApiError::ProgramNotFound(id));
}
```

### Removing IDL from Cache on Hard Delete

The `ProgramRegistry` and `IdlManager` currently have no `remove` method. For hard delete, add:

```rust
// In IdlManager (src/idl/mod.rs):
pub fn remove_cached(&mut self, program_id: &str) {
    self.cache.remove(program_id);
}

// In ProgramRegistry (src/registry.rs):
pub fn remove_program(&mut self, program_id: &str) {
    self.idl_manager.remove_cached(program_id);
}
```

Then in the delete handler, after DB cleanup:

```rust
let mut registry = state.registry.write().await;
registry.remove_program(&id);
```

### RegisterProgramRequest Type

```rust
#[derive(serde::Deserialize)]
pub struct RegisterProgramRequest {
    pub program_id: String,
    pub idl: Option<serde_json::Value>,
}
```

Place in `src/api/handlers.rs` since it's only used there. When `idl` is provided, serialize it back to string for `ProgramRegistry::register_program`:

```rust
let idl_json = body.idl.map(|v| serde_json::to_string(&v))
    .transpose()
    .map_err(|e| ApiError::InvalidRequest(format!("invalid IDL JSON: {e}")))?;
```

### Response Envelope Convention

Follow the architecture pattern from `implementation-patterns-consistency-rules.md`:

```json
// Success list
{ "data": [...], "meta": { "total": N } }

// Success single
{ "data": { ... } }

// Registration (async)
{ "data": { "program_id": "...", "status": "registered", "idl_source": "onchain" }, "meta": { "message": "Program registered. Indexing will begin shortly." } }

// Error
{ "error": { "code": "PROGRAM_NOT_FOUND", "message": "..." } }
```

### sqlx Row Access Pattern

Since we use runtime `sqlx::query()` (not compile-time `sqlx::query!()`), row access is via `sqlx::Row` trait:

```rust
use sqlx::Row;

let program_id: String = row.get("program_id");
let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
```

For `TIMESTAMPTZ` columns, sqlx maps to `chrono::DateTime<Utc>` (with the `chrono` feature). Check if `chrono` is in Cargo.toml — the `sqlx` dependency likely has `chrono` feature enabled. If not, use `.to_string()` on the raw type or add the feature.

### Files Created/Modified by This Story

| File                  | Action | Purpose                                              |
| --------------------- | ------ | ---------------------------------------------------- |
| `src/api/mod.rs`      | Modify | Expand `ApiError`, add `IntoResponse`, update router |
| `src/api/handlers.rs` | Modify | Add register, list, get, delete handlers             |
| `src/registry.rs`     | Modify | Add `remove_program()` method                        |
| `src/idl/mod.rs`      | Modify | Add `remove_cached()` method                         |
| `src/main.rs`         | Modify | Pass `config` to `AppState`                          |

### What This Story Does NOT Do

- Does NOT implement instruction/account query endpoints (stories 5.2, 5.3)
- Does NOT implement the dynamic query builder (story 5.2)
- Does NOT generate PostgreSQL schema/DDL from IDL on registration (story 2.3 — schema generation happens separately)
- Does NOT start the indexing pipeline for a registered program (story 3.5)
- Does NOT implement filter parsing in `api/filters.rs` (story 5.2)
- Does NOT add stub handlers for instruction/account endpoints (keep handlers focused on this story)

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests
- NO `println!` — use `tracing` macros (`info!`, `warn!`, `error!`)
- NO exposing internal DB error messages in API responses (log them, return generic message)
- NO `sqlx::query!()` compile-time macros — use runtime `sqlx::query()`
- NO SQL string concatenation for user values — use `push_bind()` or `$N` params
- NO `std::sync::RwLock` — use `tokio::sync::RwLock` (async context)
- NO separate `error.rs` file — `ApiError` stays in `src/api/mod.rs`
- NO `anyhow` — use `thiserror` typed enums
- NO hardcoded program IDs or database URLs

### Import Ordering Convention

```rust
// std library
use std::sync::Arc;

// external crates
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use tracing::{error, info};

// internal crate
use super::AppState;
use crate::registry::RegistrationError;
```

### Previous Story Learnings

From story 2.2 (ProgramRegistry):

- `ProgramRegistry::register_program` returns `ProgramInfo` on success
- `RegistrationError` has 3 variants: `Idl(IdlError)`, `AlreadyRegistered(String)`, `DatabaseError(String)`
- `IdlSource::as_str()` returns lowercase strings: `"onchain"`, `"bundled"`, `"manual"`
- Registration writes to both `programs` and `indexer_state` tables atomically

From story 1.3 (health endpoint):

- Health handler uses `(StatusCode, Json<Value>)` return type
- Router uses `Arc<AppState>` as state type
- `with_state(state)` applies to all routes under the router

From deferred work (deferred-work.md):

- `ApiError` missing `IntoResponse` impl — **this story resolves this deferred item**
- `updated_at` has no auto-update trigger — be aware that UPDATE operations won't auto-update this column; explicitly SET it

### Git Intelligence

Recent commits show the pattern: feature commits with `feat(module):` prefix and story reference. The codebase compiles cleanly with all clippy/fmt checks passing. 74 unit tests pass as of story 2.2.

### Project Structure Notes

- All handlers go in `src/api/handlers.rs` (single file, matching architecture spec of "12 endpoint handlers")
- `ApiError` stays in `src/api/mod.rs` (error enums live in module's mod.rs per convention)
- Request types can be defined in `handlers.rs` since they're handler-specific
- No new files created — only modifications to existing files

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-5-query-api-filtering.md#Story 5.1]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md]
- [Source: _bmad-output/planning-artifacts/research/agent-2d-dynamic-rest-api-design.md]
- [Source: _bmad-output/planning-artifacts/prd.md#API Surface]
- [Source: _bmad-output/implementation-artifacts/2-2-manual-idl-upload-and-program-registration.md]
- [Source: _bmad-output/implementation-artifacts/1-3-docker-compose-and-health-endpoint.md]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md]

## BLOCKER: axum Handler `!Send` Future — Deep Investigation Log

### The Problem

`cargo build` fails with:

```
error[E0277]: the trait bound `fn(State<Arc<AppState>>, ...) -> ... {register_program}: Handler<_, _>` is not satisfied
```

The underlying cause is that the `register_program` handler's async future is `!Send`, which axum's `Handler` trait requires.

### Root Cause (Confirmed via Isolation Testing)

**The fundamental issue is `sqlx`'s `Executor` trait lifetime.** When any async function has:

1. Multiple `.await` points, AND
2. Any of those awaits involve `sqlx::query().execute(&pool).await` or `sqlx::raw_sql().execute(&mut *tx).await`

...the resulting future is `!Send` because the compiler error says:

```
implementation of `Executor` is not general enough
`Executor<'_>` would have to be implemented for `&'0 mut PgConnection` for any lifetime '0
...but `Executor<'1>` is actually implemented for `&'1 mut PgConnection` for some specific lifetime '1
```

This is a **known Rust compiler limitation** with sqlx futures and Send bounds. The compiler cannot prove that the `Executor` lifetime is "general enough" when the sqlx future is embedded in a larger async state machine with multiple suspend points.

### Isolation Test Results (Precise Binary Search)

All tests used the pattern: `register_program` (async fn handler) -> `do_register_program` (named async fn helper) -> `commit_registration` (named async fn with DB work).

| Test | What `commit_registration` contains                                          | Result   |
| ---- | ---------------------------------------------------------------------------- | -------- |
| 1    | Empty stub (return Ok)                                                       | COMPILES |
| 2    | `registry.write().await` + `prepare_registration()` (sync)                   | COMPILES |
| 3    | + `ProgramRegistry::commit_registration(&pool, &data).await` (original refs) | FAILS    |
| 4    | `commit_registration` with ONLY `write_registration` call                    | COMPILES |
| 5    | `commit_registration` with ONLY `generate_schema` call                       | FAILS    |
| 6    | `generate_schema` called directly in `do_register_program`                   | FAILS    |
| 7    | Inline `pool.begin() + for loop + execute(&mut *tx).await`                   | FAILS    |
| 8    | `generate_schema` changed to take owned `PgPool`                             | FAILS    |
| 9    | All inline sqlx removed, all delegated to leaf async fns                     | FAILS    |

**Key finding: `generate_schema` is confirmed `!Send`** via direct assertion:

```rust
fn _require_send<T: Send>(_t: T) {}
let fut = generate_schema(&pool, &idl, "test", "test");
_require_send(fut);  // ERROR: Executor not general enough
```

**`write_registration` IS Send** (compiles fine when it's the only call). Both functions use the same pattern (`pool.begin()`, `execute(&mut *tx).await`), but `write_registration` uses sequential explicit calls while `generate_schema` uses a `for` loop over dynamic statements.

### What Was Tried (All Failed)

1. **`async fn` -> `fn -> impl Future + Send` with `async move` block** — Failed. The async move block captures `Arc<RwLock<_>>` references with specific lifetimes, producing "Send is not general enough" for `&Idl`, `&RegistrationData`, `&RwLock`.

2. **Named `async fn do_register_program` helper** — Partially worked. The named fn isolation fixed the RwLock/Idl Send issues, but the sqlx Executor issue persists when `generate_schema` is called from anywhere in the handler chain.

3. **Split-lock pattern: `prepare_registration` (sync) + `commit_registration` (async static)** — The split itself works. The problem is inside `commit_registration` when it calls `generate_schema`.

4. **Owned `PgPool` instead of `&PgPool`** — Doesn't help. `pool.begin()` still borrows `&self`, creating `Transaction<'_, Postgres>` with a specific lifetime. The Executor issue is about the transaction reference, not the pool reference.

5. **All sqlx in leaf async fns, no inline sqlx** — Doesn't help. Even when `commit_registration` has zero inline sqlx and only calls named async fns (`write_registration`, `generate_schema`, `update_program_status`), the `!Send` from `generate_schema` poisons the entire call chain.

6. **`#[axum::debug_handler]`** — Doesn't improve the error message for this case. The macro validates extractors/return types but the error originates from the Handler trait bound check at the router level.

### Why `write_registration` Works But `generate_schema` Doesn't

Both are named async fns. Both use `pool.begin()` and `execute(&mut *tx).await`. The **suspected** difference:

- `write_registration`: sequential explicit `execute` calls (no loop)
- `generate_schema`: `for stmt in &statements { sqlx::raw_sql(stmt).execute(&mut *tx).await? }` (loop with dynamic iteration)

The for loop may create a state machine where the iterator state (borrowing `statements`) and the `&mut *tx` reborrow interact in a way the compiler can't prove Send. However, this has not been 100% confirmed — it could also be related to `generate_schema` being a public free function vs `write_registration` being a private associated function.

### Architecture Review Findings (from Parallel Agents)

1. **`ProgramRegistry::pool` field is dead** — `commit_registration` is static, all DB work uses pool from AppState. The field should be removed.

2. **`IdlManager::get_idl()` (async) is dead code** — Superseded by `fetch_idl_standalone()` + `insert_fetched_idl()`. Should be removed.

3. **Race condition in rollback_cache** — Between dropping write lock after prepare and committing, another request could register the same program. The DB catches duplicates, but `rollback_cache` could remove a legitimately cached IDL from a concurrent successful registration.

4. **`delete_program` hard delete not atomic** — DROP SCHEMA + DELETE indexer_state + DELETE programs are separate queries, not in a transaction. PostgreSQL supports transactional DDL.

5. **`delete_program` soft delete missing `updated_at = NOW()`** — The deferred work notes explicitly mention this.

6. **`delete_program` DROP SCHEMA uses `format!` without `quote_ident`** — Defense-in-depth: schema_name is sanitized at registration, but should still use `quote_ident()` at point of use.

7. **`axum-test` v16 depends on axum 0.7.9** — Incompatible with our axum 0.8.8. Should upgrade to `axum-test = "17"` or `"18"`.

8. **Integration tests don't compile** — `tests/registration_test.rs` calls `prepare_registration(program_id, Some(&idl_json)).await` but the function is sync and takes `Option<String>` not `Option<&String>`.

9. **HashMap indexing in IdlManager** — `self.cache[program_id]` panics if key missing. Not caught by `clippy::panic` lint (only catches explicit `panic!()`).

### Recommended Fix Approaches (Ordered by Preference)

#### Option A: Unroll `generate_schema`'s for loop (Minimal Change)

Replace the dynamic `for stmt in &statements` loop with a single `sqlx::raw_sql` call that concatenates all DDL statements separated by semicolons. Or execute them sequentially without the for loop by collecting into a single batch. This tests whether the for loop is the specific cause.

**Risk:** May not fix it if the root cause is something else about `generate_schema`.

#### Option B: `tokio::spawn` isolation (Pragmatic)

Spawn the `commit_registration` work in a `tokio::spawn` task. This forces Send at the spawn boundary and isolates the handler future from the sqlx future entirely:

```rust
async fn do_register_program(...) -> Result<Response, ApiError> {
    let data = { /* lock + prepare */ };
    let pool_clone = pool.clone();
    let result = tokio::spawn(async move {
        ProgramRegistry::commit_registration(pool_clone, data).await
    }).await.map_err(|e| ApiError::StorageError(e.to_string()))?;
    // ...
}
```

**Pro:** Guaranteed to work. **Con:** Adds JoinHandle overhead, error handling for JoinError, loses the task's tracing span context.

#### Option C: DashMap Architecture (Clean Redesign)

Replace `Arc<RwLock<ProgramRegistry>>` with a `Clone` registry using `DashMap` for the IDL cache. Remove the outer lock entirely. The handler becomes a plain `async fn` with no lock contention. `generate_schema` is called directly without any intermediate async fn nesting.

```rust
pub struct IdlManager {
    cache: Arc<DashMap<String, CachedIdl>>,
    rpc_url: Arc<str>,
    http_client: reqwest::Client,
    bundled_idls_path: Option<Arc<PathBuf>>,
}
// IdlManager is Clone + Send + Sync
```

**Pro:** Eliminates the entire Send problem, simplifies handler code, better for pipeline concurrency. **Con:** New dependency, larger refactor, touches multiple files.

#### Option D: `Box::pin` the future (Escape Hatch)

Use `Box::pin(async move { ... })` to erase the future type. This is the standard workaround for "Send is not general enough" in async Rust:

```rust
pub fn register_program(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterProgramRequest>,
) -> Pin<Box<dyn Future<Output = Result<Response, ApiError>> + Send>> {
    Box::pin(async move { ... })
}
```

**Pro:** Simple, well-known pattern. **Con:** Heap allocation per request, loses some type inference.

### Current File State (Partially Modified)

| File                         | State                       | Notes                                                                                           |
| ---------------------------- | --------------------------- | ----------------------------------------------------------------------------------------------- |
| `src/api/handlers.rs`        | Modified, doesn't compile   | Has `#[axum::debug_handler]`, named fn pattern, unused `IdlManager` import removed              |
| `src/api/mod.rs`             | Modified, correct           | ApiError expanded, IntoResponse impl, router with program routes, Config in AppState            |
| `src/registry.rs`            | Modified, partially correct | prepare/commit split, owned PgPool, update_program_status helper, dead `pool` field             |
| `src/idl/mod.rs`             | Modified, correct           | remove_cached, fetch_params, fetch_idl_standalone, insert_fetched_idl added                     |
| `src/storage/schema.rs`      | Modified                    | `generate_schema` changed to owned PgPool (may revert), `seed_metadata` changed to owned PgPool |
| `src/main.rs`                | Modified, correct           | Passes config to AppState                                                                       |
| `tests/registration_test.rs` | Modified, doesn't compile   | Uses old API signatures                                                                         |

### Next Steps

1. **Choose a fix approach** (A, B, C, or D above)
2. Apply the fix and verify `cargo build` passes
3. Fix integration tests to match current API
4. Run full verification: `cargo build && cargo clippy && cargo fmt -- --check && cargo test`
5. Address secondary findings (dead code, race condition, delete atomicity, etc.)

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

Isolation testing session (2026-04-06): Systematic binary search narrowing the !Send source from handler level down to `generate_schema` function.

### Completion Notes List

- Story is BLOCKED on the Send issue — no acceptance criteria can be verified until build passes
- All handler logic (list, get, delete) compiles fine — only `register_program` is affected
- The issue is structural (sqlx + async + Send) not a simple code bug

### File List

- `src/api/mod.rs` — ApiError, IntoResponse, router, AppState
- `src/api/handlers.rs` — All 5 handlers + request types + unit tests
- `src/registry.rs` — ProgramRegistry with prepare/commit/rollback split
- `src/idl/mod.rs` — IdlManager with standalone fetch pattern
- `src/storage/schema.rs` — generate_schema, seed_metadata (PgPool ownership changed)
- `src/main.rs` — Config passed to AppState
- `tests/registration_test.rs` — Integration tests (need API update)
