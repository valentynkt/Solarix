# Epic 5: Query API & Filtering

User can query indexed data through REST endpoints with multi-parameter filters, pagination, aggregation, and program statistics -- the "it's actually useful" layer.

## Story 5.1: Program Management Endpoints

As a user,
I want to register, list, inspect, and deregister programs via REST API,
So that I can manage which programs are indexed without touching the database directly.

**Acceptance Criteria:**

**Given** the axum Router in `api/mod.rs`
**When** I inspect it
**Then** it defines routes using axum 0.8 `{param}` syntax (not `:param`)
**And** `AppState` contains: `PgPool`, `Arc<RwLock<ProgramRegistry>>`, `Config`
**And** state is shared via `State<Arc<AppState>>`

**Given** a `POST /api/programs` request with `{ "program_id": "<base58>" }`
**When** the handler processes it
**Then** it triggers IDL fetch (on-chain -> bundled -> error), registers the program in `ProgramRegistry`, generates the schema (Epic 2), and inserts into `programs` + `indexer_state` tables
**And** returns HTTP 202 Accepted with `{ "data": { "program_id": "...", "status": "registering", "idl_source": "on_chain" }, "meta": { "message": "Program registered. Indexing will begin shortly." } }`

**Given** a `POST /api/programs` request with an IDL JSON body (`{ "program_id": "...", "idl": {...} }`)
**When** the handler processes it
**Then** it uses the provided IDL (manual upload path) instead of fetching from chain
**And** returns HTTP 202 with `idl_source: "manual_upload"`

**Given** a `GET /api/programs` request
**When** the handler processes it
**Then** it returns a list of all registered programs with `program_id`, `program_name`, `status`, `created_at`
**And** the response uses the standard envelope: `{ "data": [...], "meta": { "total": N } }`

**Given** a `GET /api/programs/{id}` request
**When** the program exists
**Then** it returns program details including: `program_id`, `program_name`, `schema_name`, `idl_source`, `idl_hash`, `status` (from the state machine: registered -> schema_created -> indexing -> live -> error / stopped), `created_at`, `updated_at`, indexing stats from `indexer_state`
**And** when the program does not exist, returns HTTP 404 with `{ "error": { "code": "PROGRAM_NOT_FOUND", "message": "..." } }`

**Given** a `DELETE /api/programs/{id}` request without `drop_tables=true`
**When** the handler processes it
**Then** indexing stops for the program, status is set to `stopped`, but all data and schema are retained

**Given** a `DELETE /api/programs/{id}?drop_tables=true` request
**When** the handler processes it
**Then** it executes `DROP SCHEMA "{schema_name}" CASCADE` and removes entries from `programs` and `indexer_state`
**And** this is an irreversible operation

**Given** the `ApiError` enum
**When** I inspect it
**Then** it includes variants: `ProgramNotFound`, `ProgramAlreadyRegistered`, `InstructionNotFound`, `AccountTypeNotFound`, `AccountNotFound`, `InvalidFilter`, `InvalidOperator`, `InvalidValue`, `QueryFailed`, `IdlError(IdlError)`, `StorageError(StorageError)`
**And** it implements `axum::response::IntoResponse` mapping to appropriate HTTP status codes (404, 400, 409, 500)
**And** error responses include machine-readable `code` and human-readable `message`

## Story 5.2: Dynamic Query Builder & Filters

As a developer,
I want a dynamic SQL query builder that translates API filter parameters into safe, IDL-validated SQL queries,
So that users can filter indexed data by any field without risk of SQL injection.

**Acceptance Criteria:**

**Given** query parameters like `amount_gt=1000&signer_eq=ABC123`
**When** the filter parser in `api/filters.rs` processes them
**Then** it extracts field name and operator by splitting on the last `_` separator matching a known operator
**And** supported operators are: `_gt`, `_gte`, `_lt`, `_lte`, `_eq`, `_ne`, `_contains`, `_in`
**And** parameters are extracted via `Query<HashMap<String, String>>` (dynamic, not typed struct)

**Given** a filter field name
**When** the validator checks it against the IDL
**Then** promoted column fields are queried directly as SQL columns
**And** non-promoted fields (nested/complex) use JSONB `@>` containment queries (NOT `data->>'field'` which bypasses GIN indexes)
**And** unknown field names return HTTP 400 with `{ "error": { "code": "INVALID_FILTER", "message": "Unknown field 'foo'", "available_fields": ["amount", "authority", ...] } }`

**Given** the `QueryBuilder` in `storage/queries.rs`
**When** building a SQL query with filters
**Then** all user-provided values are bound via `QueryBuilder::push_bind()` (never string concatenation)
**And** table and column names are derived from the IDL (not from user input) and double-quoted
**And** operator mapping: `_gt` -> `>`, `_gte` -> `>=`, `_lt` -> `<`, `_lte` -> `<=`, `_eq` -> `=`, `_ne` -> `!=`, `_in` -> `= ANY($)`, `_contains` -> `@>`

**Given** the `_in` operator with value `val1,val2,val3`
**When** the query builder processes it
**Then** it splits on comma and binds as an array parameter

## Story 5.3: Instruction & Account Query Endpoints

As a user,
I want to query decoded instructions and account states by type with filters and pagination,
So that I can explore and analyze indexed on-chain data through the API.

**Acceptance Criteria:**

**Given** a `GET /api/programs/{id}/instructions` request
**When** the handler processes it
**Then** it returns a list of instruction type names available for the program (derived from the IDL)

**Given** a `GET /api/programs/{id}/instructions/{name}` request with filter params
**When** the handler processes it
**Then** it validates filters against the IDL instruction's `args` field types
**And** builds and executes a dynamic SQL query against the `_instructions` table
**And** returns decoded instructions matching the filters

**Given** instruction query results with cursor pagination
**When** the response is built
**Then** it uses keyset pagination on `(slot, signature)` with cursor encoded as `base64("{slot}_{signature}")`
**And** the response includes `{ "data": [...], "pagination": { "limit": N, "has_more": bool, "next_cursor": "..." }, "meta": { "program_id": "...", "query_time_ms": N } }`
**And** default limit is 50, max limit is 1000

**Given** a `GET /api/programs/{id}/accounts` request
**When** the handler processes it
**Then** it returns a list of account type names available for the program

**Given** a `GET /api/programs/{id}/accounts/{type}` request with filter params
**When** the handler processes it
**Then** it validates filters against the IDL account type's field definitions
**And** builds and executes a dynamic SQL query against the account type's table
**And** uses offset-based pagination with `{ "total": N, "limit": N, "offset": N, "has_more": bool }`

**Given** a `GET /api/programs/{id}/accounts/{type}/{pubkey}` request
**When** the account exists
**Then** it returns the single account record with all promoted columns and JSONB data
**And** when the account does not exist, returns HTTP 404

## Story 5.4: Aggregation, Statistics & Health Enhancement

As a user,
I want to see instruction call counts over time and program-level statistics,
So that I can understand program usage patterns and indexing progress.

**Acceptance Criteria:**

**Given** a `GET /api/programs/{id}/instructions/{name}/count` request with `interval` and optional `from`/`to` params
**When** the handler processes it
**Then** the `interval` parameter is validated against the whitelist: `["minute", "hour", "day", "week", "month"]` (raw user input is never passed to SQL)
**And** the SQL uses `date_trunc($1, to_timestamp(block_time))` since `block_time` is stored as BIGINT Unix seconds
**And** results are grouped by the truncated time bucket with count per bucket
**And** invalid interval values return HTTP 400 with `INVALID_VALUE` error code

**Given** a `GET /api/programs/{id}/stats` request
**When** the handler processes it
**Then** it returns program statistics: `total_instructions`, `total_accounts`, `unique_signers`, `first_seen_slot`, `last_seen_slot`, `instruction_counts` (breakdown by instruction name)
**And** statistics are read from the `indexer_state` table and per-program `_metadata` (pre-computed counters, not live COUNT(\*) queries)

**Given** the enhanced `GET /health` endpoint
**When** the system is healthy
**Then** it returns HTTP 200 with: `status: "healthy"`, `database: "connected"`, `pipeline` status per program (state, lag in slots, last_heartbeat), `uptime_seconds`, `version`

**Given** the system is unhealthy
**When** `GET /health` is called
**Then** it returns HTTP 503 (not 200 with error body) when: DB pool has no idle connections, OR pipeline lag > 120 seconds, OR any program in error state for > 5 minutes
**And** the response body still includes diagnostic details for debugging

---
