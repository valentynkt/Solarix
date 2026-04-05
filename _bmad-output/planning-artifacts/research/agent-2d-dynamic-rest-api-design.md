# Agent 2D: Dynamic REST API Design for Solarix

**Date:** 2026-04-05
**Context:** Superteam Ukraine Bounty -- Universal Solana Indexer (Rust, PostgreSQL, axum)
**Framework:** axum 0.8.x (latest stable, March 2026)
**Scope:** Research-only -- API layer design for dynamically-generated schemas

---

## 1. Executive Summary

Solarix needs a REST API layer that serves queries over database tables generated at runtime from Anchor IDLs. The core challenge is that neither the table names, column names, nor filterable fields are known at compile time. This document designs the complete API surface, the dynamic routing strategy, multi-parameter filter builder, aggregation queries, pagination, response formats, and the axum application architecture.

**Key Design Decisions:**

| Decision                   | Choice                                                          | Rationale                                                                                                                                |
| -------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Dynamic routing strategy   | Catch-all parametric routes                                     | axum `Router` is immutable after build; parametric routes with `{program_id}` and `{name}` handle all programs without router rebuilding |
| Query parameter extraction | `Query<HashMap<String, String>>`                                | Unknown filter fields at compile time; HashMap captures arbitrary key-value pairs                                                        |
| Filter operator convention | Underscore suffix (`_gt`, `_lt`, `_eq`)                         | Simple, URL-friendly, widely adopted; no ambiguity with field names validated against IDL                                                |
| SQL builder                | `sqlx::QueryBuilder` with `push_bind()`                         | Built-in parameterization prevents SQL injection; no extra dependencies                                                                  |
| Pagination                 | Hybrid: cursor-based primary, offset fallback                   | Cursor (keyset) for performance on large instruction tables; offset for simplicity on smaller account sets                               |
| Response format            | `Json<serde_json::Value>` with `json!` macro                    | Dynamic fields from JSONB; consistent envelope with `data`, `pagination`, `meta`                                                         |
| Error handling             | `thiserror` enum + `IntoResponse` impl                          | Production pattern; maps each error variant to HTTP status + JSON body                                                                   |
| State management           | `State<Arc<AppState>>` with inner `RwLock` for mutable registry | Immutable config (db pool, RPC client) + mutable program registry                                                                        |

---

## 2. Complete API Endpoint Table

### Program Management

| Method   | Path                         | Description                                                  | Status Code  |
| -------- | ---------------------------- | ------------------------------------------------------------ | ------------ |
| `POST`   | `/api/programs`              | Register a new program (by program_id; optional IDL upload)  | 202 Accepted |
| `GET`    | `/api/programs`              | List all registered programs                                 | 200          |
| `GET`    | `/api/programs/{program_id}` | Get program info (IDL summary, status, stats)                | 200 / 404    |
| `DELETE` | `/api/programs/{program_id}` | Deregister a program (stop indexing, optionally drop tables) | 200 / 404    |

### Instruction Queries

| Method | Path                                             | Description                                 | Status Code |
| ------ | ------------------------------------------------ | ------------------------------------------- | ----------- |
| `GET`  | `/api/programs/{program_id}/instructions`        | List all instruction types for this program | 200 / 404   |
| `GET`  | `/api/programs/{program_id}/instructions/{name}` | Query decoded instructions with filters     | 200 / 404   |

### Account State Queries

| Method | Path                                                  | Description                             | Status Code |
| ------ | ----------------------------------------------------- | --------------------------------------- | ----------- |
| `GET`  | `/api/programs/{program_id}/accounts`                 | List all account types for this program | 200 / 404   |
| `GET`  | `/api/programs/{program_id}/accounts/{type}`          | Query decoded accounts with filters     | 200 / 404   |
| `GET`  | `/api/programs/{program_id}/accounts/{type}/{pubkey}` | Get a specific account by pubkey        | 200 / 404   |

### Aggregation & Statistics

| Method | Path                                                   | Description                                 | Status Code |
| ------ | ------------------------------------------------------ | ------------------------------------------- | ----------- |
| `GET`  | `/api/programs/{program_id}/stats`                     | Basic program statistics (counts, activity) | 200 / 404   |
| `GET`  | `/api/programs/{program_id}/instructions/{name}/count` | Instruction call count over time            | 200 / 404   |

### Health

| Method | Path      | Description                                     | Status Code |
| ------ | --------- | ----------------------------------------------- | ----------- |
| `GET`  | `/health` | Pipeline status, DB health, last processed slot | 200 / 503   |

---

## 3. Dynamic Route Strategy

### The Problem

axum's `Router` is built once and is immutable after `.into_make_service()`. When a new program is registered, we cannot add new routes. The router must handle programs that do not exist yet.

### Research Findings

Three approaches were evaluated:

**Approach A: Catch-all parametric routes (RECOMMENDED)**
A single set of routes with `{program_id}`, `{name}`, `{type}`, and `{pubkey}` path parameters dispatches to generic handlers. The handler looks up the program's IDL from an in-memory cache to validate inputs and construct queries.

This is the idiomatic axum pattern. No runtime router rebuilding needed.

**Approach B: Dynamic router rebuilding**
axum maintainers explicitly state this is "quite a niche use case" and recommend against it. While technically possible by wrapping the router in `Arc<RwLock<Router>>` and swapping it, this adds complexity with no benefit over parametric routes. The service handlers still need to be defined at compile time.

**Approach C: Generic handler with schema validation middleware**
Valid but over-engineered. Middleware cannot easily inspect path parameters and perform IDL-specific validation without duplicating handler logic. Better to keep validation in the handler.

### Recommended Router Structure (axum 0.8)

```rust
use axum::{Router, routing::{get, post, delete}};

fn api_router(state: AppState) -> Router {
    let program_routes = Router::new()
        // Program management
        .route("/", post(handlers::register_program).get(handlers::list_programs))
        .route("/{program_id}", get(handlers::get_program).delete(handlers::delete_program))
        // Instruction queries
        .route("/{program_id}/instructions", get(handlers::list_instruction_types))
        .route("/{program_id}/instructions/{name}", get(handlers::query_instructions))
        .route("/{program_id}/instructions/{name}/count", get(handlers::instruction_count))
        // Account queries
        .route("/{program_id}/accounts", get(handlers::list_account_types))
        .route("/{program_id}/accounts/{type}", get(handlers::query_accounts))
        .route("/{program_id}/accounts/{type}/{pubkey}", get(handlers::get_account))
        // Statistics
        .route("/{program_id}/stats", get(handlers::program_stats));

    Router::new()
        .nest("/api/programs", program_routes)
        .route("/health", get(handlers::health))
        .with_state(Arc::new(state))
}
```

Note: axum 0.8 uses `{param}` syntax (not `:param`), aligning with OpenAPI standards.

### Handler Signature Pattern

```rust
use axum::extract::{Path, Query, State};
use std::collections::HashMap;
use std::sync::Arc;

async fn query_instructions(
    State(state): State<Arc<AppState>>,
    Path((program_id, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    // 1. Look up program in registry
    let program = state.registry.get(&program_id)
        .ok_or(ApiError::ProgramNotFound(program_id.clone()))?;

    // 2. Find instruction in IDL
    let instruction = program.idl.find_instruction(&name)
        .ok_or(ApiError::InstructionNotFound(name.clone()))?;

    // 3. Validate & build filters from query params against IDL schema
    let filters = FilterBuilder::from_params(&params, &instruction.args)?;

    // 4. Build and execute dynamic SQL query
    let rows = build_instruction_query(&state.db, &program.table_prefix, &name, &filters).await?;

    // 5. Return response envelope
    Ok(Json(json!({
        "data": rows,
        "pagination": { ... },
        "meta": { "program_id": program_id, "instruction": name }
    })))
}
```

### Path Parameter Extraction in axum 0.8

Single parameter:

```rust
async fn handler(Path(program_id): Path<String>) -> impl IntoResponse { ... }
```

Multiple parameters (tuple):

```rust
async fn handler(Path((program_id, name)): Path<(String, String)>) -> impl IntoResponse { ... }
```

Struct extraction (via serde):

```rust
#[derive(Deserialize)]
struct InstructionPath {
    program_id: String,
    name: String,
}
async fn handler(Path(path): Path<InstructionPath>) -> impl IntoResponse { ... }
```

---

## 4. Multi-Parameter Filter Builder Design

### Filter Operator Convention

Using the underscore suffix convention, which is widely adopted (DreamFactory, Strapi, many REST APIs):

| Suffix          | SQL Operator | Example Query Param         | SQL Output                       |
| --------------- | ------------ | --------------------------- | -------------------------------- | --- | --- | --- | ---- |
| (none) or `_eq` | `=`          | `recipient=3Kcg...`         | `data->>'recipient' = $1`        |
| `_ne`           | `!=`         | `status_ne=completed`       | `data->>'status' != $1`          |
| `_gt`           | `>`          | `amount_gt=1000`            | `(data->>'amount')::BIGINT > $1` |
| `_gte`          | `>=`         | `slot_gte=280000000`        | `slot >= $1`                     |
| `_lt`           | `<`          | `price_lt=50`               | `(data->>'price')::BIGINT < $1`  |
| `_lte`          | `<=`         | `block_time_lte=1700000000` | `block_time <= $1`               |
| `_contains`     | `LIKE`       | `memo_contains=hello`       | `data->>'memo' LIKE '%'          |     | $1  |     | '%'` |
| `_in`           | `IN`         | `status_in=active,pending`  | `data->>'status' = ANY($1)`      |

### Reserved Query Parameters (Not Filters)

These parameters control pagination and sorting, not filtering:

| Parameter | Description                      | Default |
| --------- | -------------------------------- | ------- |
| `limit`   | Max rows to return               | 100     |
| `offset`  | Rows to skip (offset pagination) | 0       |
| `cursor`  | Cursor for keyset pagination     | (none)  |
| `sort`    | Field to sort by                 | `slot`  |
| `order`   | Sort direction: `asc` or `desc`  | `desc`  |

### Filter Parsing Algorithm

```
Input: HashMap<String, String> from Query extractor
IDL schema: Vec<IdlField> with name and type_info

For each (key, value) in params:
  1. Skip if key is a reserved parameter (limit, offset, cursor, sort, order)
  2. Parse operator suffix:
     - Split key by last underscore
     - If suffix is a known operator (gt, gte, lt, lte, ne, eq, contains, in):
       field_name = key without suffix, operator = suffix
     - Else: field_name = key, operator = "eq"
  3. Determine if field_name is a common column or JSONB field:
     - Common columns: slot, signature, block_time, program_id, instruction_name (instructions)
     - Common columns: pubkey, account_type, slot_updated (accounts)
     - Everything else: JSONB field in `data` column
  4. Validate field_name against IDL schema (if JSONB field):
     - Look up field_name in instruction.args or account.fields
     - If not found: return ApiError::InvalidFilter with available fields
  5. Determine SQL cast from IDL type:
     - u8/u16/u32/i8/i16/i32 -> ::INT
     - u64/i64/u128/i128 -> ::BIGINT  (u128/i128 stored as text, compared as NUMERIC)
     - f32/f64 -> ::DOUBLE PRECISION
     - bool -> ::BOOLEAN
     - string/pubkey -> (no cast, text comparison)
  6. Build filter clause
```

### Nested Field Access

For nested struct fields, use dot notation in query params:

```
?config.max_amount_gt=1000
```

Maps to JSONB path access:

```sql
(data->'config'->>'max_amount')::BIGINT > $1
```

Parsing: split field_name on `.`, build JSONB path with `->` for intermediate keys and `->>` for the final key (text extraction).

### SQL Query Builder Implementation Pattern

```rust
use sqlx::QueryBuilder;
use sqlx::postgres::Postgres;

struct Filter {
    column_expr: String,    // e.g., "slot" or "(data->>'amount')::BIGINT"
    operator: SqlOp,        // Eq, Ne, Gt, Gte, Lt, Lte, Contains, In
    value: FilterValue,     // String, Int, Float, Bool, StringList
}

enum SqlOp { Eq, Ne, Gt, Gte, Lt, Lte, Contains, In }

fn build_instruction_query(
    table: &str,
    instruction_name: &str,
    filters: &[Filter],
    pagination: &Pagination,
) -> QueryBuilder<'_, Postgres> {
    // Table name is constructed from program_id, NOT from user input.
    // It is sanitized during program registration (alphanumeric + underscore only).
    let mut qb = QueryBuilder::new(format!(
        "SELECT slot, signature, block_time, instruction_name, data FROM {table}"
    ));

    // Always filter by instruction name
    qb.push(" WHERE instruction_name = ").push_bind(instruction_name);

    // Dynamic filters
    for filter in filters {
        match filter.operator {
            SqlOp::Eq => {
                // column_expr is NOT user input -- it's built from IDL field names
                // which are validated against the parsed IDL
                qb.push(format!(" AND {} = ", filter.column_expr));
                qb.push_bind(&filter.value);
            }
            SqlOp::Gt => {
                qb.push(format!(" AND {} > ", filter.column_expr));
                qb.push_bind(&filter.value);
            }
            SqlOp::Contains => {
                qb.push(format!(" AND {} LIKE '%' || ", filter.column_expr));
                qb.push_bind(&filter.value);
                qb.push(" || '%'");
            }
            SqlOp::In => {
                qb.push(format!(" AND {} = ANY(", filter.column_expr));
                qb.push_bind(&filter.value_list);
                qb.push(")");
            }
            // ... other operators
        }
    }

    // Sorting
    let sort_col = validate_sort_column(pagination.sort);  // whitelist check
    let order = if pagination.order == "asc" { "ASC" } else { "DESC" };
    qb.push(format!(" ORDER BY {sort_col} {order}"));

    // Pagination
    qb.push(" LIMIT ").push_bind(pagination.limit);
    if let Some(offset) = pagination.offset {
        qb.push(" OFFSET ").push_bind(offset);
    }

    qb
}
```

### SQL Injection Prevention Strategy

1. **Table names**: Constructed from `program_id` at registration time, sanitized to `[a-z0-9_]` only. Stored in the program registry, not taken from request params.
2. **Column expressions**: Built from IDL field names (validated at IDL parse time) + JSONB path operators. Never from raw user input.
3. **Values**: Always via `push_bind()` -- parameterized queries. Never interpolated.
4. **Sort columns**: Validated against a whitelist of known columns (`slot`, `signature`, `block_time`, plus IDL-derived field names).
5. **Operator suffixes**: Parsed from a fixed enum, not interpolated into SQL.

---

## 5. Aggregation Query Design

### 5.1 Instruction Count Over Time

**Endpoint:** `GET /api/programs/{program_id}/instructions/{name}/count`

**Query Parameters:**
| Param | Type | Default | Description |
|---|---|---|---|
| `from` | Unix timestamp (seconds) | 30 days ago | Start of time range |
| `to` | Unix timestamp (seconds) | now | End of time range |
| `interval` | `minute` / `hour` / `day` / `week` / `month` | `day` | Aggregation bucket size |

**Generated SQL:**

```sql
SELECT
    date_trunc($1, to_timestamp(block_time)) AS period,
    COUNT(*) AS count
FROM {table}
WHERE instruction_name = $2
  AND block_time >= $3
  AND block_time <= $4
GROUP BY period
ORDER BY period ASC
```

Notes:

- `$1` is the interval string (`'hour'`, `'day'`, etc.) -- validated against a whitelist before binding.
- `block_time` is stored as Unix timestamp (i64). `to_timestamp()` converts it for `date_trunc`.
- The interval parameter MUST be validated against a whitelist (`minute`, `hour`, `day`, `week`, `month`). Do NOT pass raw user input to `date_trunc`.

**Response:**

```json
{
  "data": [
    { "period": "2026-04-01T00:00:00Z", "count": 1523 },
    { "period": "2026-04-02T00:00:00Z", "count": 1891 },
    { "period": "2026-04-03T00:00:00Z", "count": 2104 }
  ],
  "meta": {
    "program_id": "JUP6...",
    "instruction": "route",
    "from": 1711929600,
    "to": 1712188800,
    "interval": "day",
    "total_count": 5518
  }
}
```

### 5.2 Program Statistics

**Endpoint:** `GET /api/programs/{program_id}/stats`

**Generated SQL (multiple queries composed into one response):**

```sql
-- Instruction counts by type
SELECT instruction_name, COUNT(*) AS count
FROM {instructions_table}
GROUP BY instruction_name
ORDER BY count DESC;

-- Account counts by type
SELECT account_type, COUNT(*) AS count
FROM {accounts_table}
GROUP BY account_type
ORDER BY count DESC;

-- Recent activity (last 30 days, daily)
SELECT
    date_trunc('day', to_timestamp(block_time)) AS day,
    COUNT(*) AS count
FROM {instructions_table}
WHERE block_time > extract(epoch FROM now() - interval '30 days')
GROUP BY day
ORDER BY day ASC;

-- Total counts
SELECT COUNT(*) FROM {instructions_table};
SELECT COUNT(*) FROM {accounts_table};

-- First and last indexed slot
SELECT MIN(slot) AS first_slot, MAX(slot) AS last_slot FROM {instructions_table};
```

**Response:**

```json
{
  "data": {
    "program_id": "JUP6LiNT...",
    "total_instructions": 1523456,
    "total_accounts": 89012,
    "first_indexed_slot": 275000000,
    "last_indexed_slot": 280000050,
    "instructions_by_type": [
      { "name": "route", "count": 1200000 },
      { "name": "sharedAccountsRoute", "count": 300000 },
      { "name": "setTokenLedger", "count": 23456 }
    ],
    "accounts_by_type": [
      { "type": "TokenLedger", "count": 50000 },
      { "type": "OpenOrders", "count": 39012 }
    ],
    "daily_activity": [
      { "day": "2026-03-06", "count": 45000 },
      { "day": "2026-03-07", "count": 52000 }
    ]
  }
}
```

---

## 6. Pagination Strategy

### Research Summary

| Approach                | Pros                                                | Cons                                                | Best For                                       |
| ----------------------- | --------------------------------------------------- | --------------------------------------------------- | ---------------------------------------------- |
| Offset-based            | Simple, intuitive, supports "jump to page N"        | O(offset) scan cost; breaks with concurrent inserts | Small result sets (<10K rows), account queries |
| Cursor-based (keyset)   | O(1) per page; stable under inserts/deletes         | Cannot jump to page N; requires stable sort key     | Large result sets, instruction queries         |
| Encoded cursor (opaque) | Hides internal keys from client; forward-compatible | Extra encode/decode step                            | Public APIs                                    |

### Recommendation: Hybrid Strategy

**For instruction queries:** Cursor-based (keyset) pagination using `(slot, signature)` as the composite cursor. Instructions can have millions of rows; offset becomes untenable past page 100.

**For account queries:** Offset-based pagination. Account tables are typically smaller (hundreds of thousands), and offset works well within this range.

**Both modes are available on all endpoints.** If `cursor` is provided, use keyset; otherwise fall back to `offset`.

### Keyset Pagination SQL

```sql
-- Forward pagination (next page)
SELECT slot, signature, block_time, instruction_name, data
FROM {table}
WHERE instruction_name = $1
  AND (slot, signature) < ($2, $3)  -- cursor values
  -- additional filters...
ORDER BY slot DESC, signature DESC
LIMIT $4
```

The cursor encodes the last row's `(slot, signature)` as a base64 string:

```
cursor = base64_encode(format!("{slot}_{signature}"))
```

Decoding:

```
(slot, signature) = cursor.base64_decode().split('_')
```

### Pagination Response

```json
{
  "pagination": {
    "limit": 100,
    "has_more": true,
    "next_cursor": "eyJzbG90IjoyODAwMDAwMDAsInNpZ25hdHVyZSI6IjVLall..."
  }
}
```

For offset mode:

```json
{
  "pagination": {
    "limit": 100,
    "offset": 200,
    "total": 15234,
    "has_more": true
  }
}
```

Note: `total` requires a `COUNT(*)` query. This is expensive on large tables. Consider:

- Including `total` only when `offset` pagination is used
- Caching counts per program/instruction (refresh periodically)
- Returning `total: null` for cursor pagination, with a separate `/count` endpoint

---

## 7. Response/Error Format Specification

### Success Response Envelope

All successful responses follow this envelope:

```json
{
  "data": <array or object>,
  "pagination": {
    "limit": 100,
    "offset": 0,
    "total": 15234,
    "has_more": true,
    "next_cursor": "..."
  },
  "meta": {
    "program_id": "JUP6LiNT...",
    "query_time_ms": 42
  }
}
```

- `data`: Always present. Array for list endpoints, object for single-item endpoints.
- `pagination`: Present only for list endpoints. Fields vary by pagination mode.
- `meta`: Always present. Contains contextual info and performance data.

### Single Item Response (no pagination)

```json
{
  "data": {
    "pubkey": "3Kcg...",
    "account_type": "TokenLedger",
    "slot_updated": 280000042,
    "data": {
      "token_a_balance": 1000000,
      "token_b_balance": 2500000
    }
  },
  "meta": {
    "program_id": "JUP6LiNT...",
    "query_time_ms": 3
  }
}
```

### Error Response Format

```json
{
  "error": {
    "code": "INVALID_FILTER",
    "message": "Field 'nonexistent' not found in instruction 'transfer' args",
    "details": {
      "available_fields": ["amount", "recipient", "authority"]
    }
  }
}
```

### Error Code Catalog

| Code                     | HTTP Status | Description                              |
| ------------------------ | ----------- | ---------------------------------------- |
| `PROGRAM_NOT_FOUND`      | 404         | Program ID not registered                |
| `INSTRUCTION_NOT_FOUND`  | 404         | Instruction name not in IDL              |
| `ACCOUNT_TYPE_NOT_FOUND` | 404         | Account type not in IDL                  |
| `ACCOUNT_NOT_FOUND`      | 404         | Specific account pubkey not found        |
| `INVALID_FILTER`         | 400         | Filter field not in IDL schema           |
| `INVALID_OPERATOR`       | 400         | Unknown filter operator suffix           |
| `INVALID_VALUE`          | 400         | Value cannot be parsed for field type    |
| `INVALID_PAGINATION`     | 400         | Invalid cursor, limit, or offset         |
| `INVALID_INTERVAL`       | 400         | Unknown aggregation interval             |
| `PROGRAM_ALREADY_EXISTS` | 409         | Program ID already registered            |
| `IDL_NOT_FOUND`          | 404         | IDL could not be fetched for program     |
| `IDL_PARSE_ERROR`        | 422         | IDL is malformed or unsupported version  |
| `INDEXING_IN_PROGRESS`   | 202         | Program registered but still backfilling |
| `INTERNAL_ERROR`         | 500         | Unexpected server error                  |
| `DATABASE_ERROR`         | 500         | Database query failed                    |
| `SERVICE_UNAVAILABLE`    | 503         | Pipeline or database unhealthy           |

### Error Type Implementation (thiserror + IntoResponse)

```rust
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Program '{0}' not found")]
    ProgramNotFound(String),

    #[error("Instruction '{0}' not found in program")]
    InstructionNotFound(String),

    #[error("Account type '{0}' not found in program")]
    AccountTypeNotFound(String),

    #[error("Account '{0}' not found")]
    AccountNotFound(String),

    #[error("Invalid filter: {message}")]
    InvalidFilter {
        message: String,
        available_fields: Vec<String>,
    },

    #[error("Invalid value for field '{field}': {message}")]
    InvalidValue { field: String, message: String },

    #[error("Program '{0}' already registered")]
    ProgramAlreadyExists(String),

    #[error("IDL not found for program '{0}'")]
    IdlNotFound(String),

    #[error("IDL parse error: {0}")]
    IdlParseError(String),

    #[error("Database error")]
    Database(#[from] sqlx::Error),

    #[error("Internal error")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, details) = match &self {
            ApiError::ProgramNotFound(id) => (
                StatusCode::NOT_FOUND,
                "PROGRAM_NOT_FOUND",
                format!("Program '{}' not found", id),
                None,
            ),
            ApiError::InvalidFilter { message, available_fields } => (
                StatusCode::BAD_REQUEST,
                "INVALID_FILTER",
                message.clone(),
                Some(json!({ "available_fields": available_fields })),
            ),
            ApiError::ProgramAlreadyExists(id) => (
                StatusCode::CONFLICT,
                "PROGRAM_ALREADY_EXISTS",
                format!("Program '{}' is already registered", id),
                None,
            ),
            ApiError::Database(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DATABASE_ERROR",
                "Database query failed".into(),
                None, // Don't expose internal DB errors
            ),
            // ... other variants
        };

        let body = if let Some(details) = details {
            json!({ "error": { "code": code, "message": message, "details": details }})
        } else {
            json!({ "error": { "code": code, "message": message }})
        };

        (status, Json(body)).into_response()
    }
}
```

This pattern enables using `?` operator throughout all handlers:

```rust
let program = state.registry.get(&program_id)
    .ok_or(ApiError::ProgramNotFound(program_id.clone()))?;
```

---

## 8. Program Registration Flow

### Endpoint: `POST /api/programs`

**Request Body:**

```json
{
  "program_id": "JUP6LiNT54oPHphKMB3FuZYezYZRjXNUkGMtEGHNiN"
}
```

Or with manual IDL upload:

```json
{
  "program_id": "CUSTOM_PROGRAM_ID",
  "idl": { ... }
}
```

### Flow Sequence

```
1. Parse request body
   ├── Extract program_id (required)
   └── Extract idl (optional)

2. Check if program already registered
   └── If yes: return 409 PROGRAM_ALREADY_EXISTS with current status

3. Acquire IDL (multi-tier cascade, if not provided)
   ├── Try on-chain fetch (legacy Anchor IDL account)
   ├── Try PMP fetch (Anchor v1.0+ Program Metadata Program)
   ├── Try bundled IDL registry (70+ pre-bundled IDLs)
   └── If all fail: return 404 IDL_NOT_FOUND
       └── Response includes: "Upload IDL manually via the 'idl' field"

4. Parse and validate IDL
   └── If malformed: return 422 IDL_PARSE_ERROR

5. Generate table names
   └── Sanitize program_id to table prefix: [a-z0-9_], max 40 chars
   └── Tables: {prefix}_instructions, {prefix}_accounts

6. Generate DDL from IDL
   ├── CREATE TABLE {prefix}_instructions (common columns + data JSONB)
   ├── CREATE TABLE {prefix}_accounts (common columns + data JSONB)
   ├── CREATE INDEX on instruction_name, slot, block_time
   ├── CREATE INDEX (GIN) on data column
   └── Execute DDL in a transaction

7. Register program in metadata
   ├── Insert into solarix_programs table (program_id, idl, status, created_at)
   └── Add to in-memory registry (Arc<RwLock<HashMap<String, ProgramInfo>>>)

8. Start indexing pipeline (async)
   ├── Spawn background task for backfill
   └── Set status = "indexing"

9. Return 202 Accepted
```

**Response (202 Accepted):**

```json
{
  "data": {
    "program_id": "JUP6LiNT...",
    "status": "indexing",
    "tables_created": ["jup6lint_instructions", "jup6lint_accounts"],
    "instructions": ["route", "sharedAccountsRoute", "setTokenLedger"],
    "account_types": ["TokenLedger", "OpenOrders"]
  },
  "meta": {
    "message": "Program registered. Indexing has started. Check GET /api/programs/{program_id} for status."
  }
}
```

### Why 202 Accepted?

Registration triggers an async backfill operation that may take minutes to hours. The 202 status code indicates "the request has been accepted for processing, but the processing has not been completed." The client polls `GET /api/programs/{program_id}` to check progress.

### Program Status State Machine

```
   registered (IDL parsed, tables created)
       │
       v
   indexing (backfill in progress)
       │
       v
   live (backfill complete, real-time streaming)
       │
       v
   error (pipeline failure, with error details)
       │
       v
   stopped (manually deregistered or paused)
```

### Deregistration: `DELETE /api/programs/{program_id}`

```json
{
  "data": {
    "program_id": "JUP6LiNT...",
    "status": "stopped",
    "tables_dropped": false,
    "message": "Indexing stopped. Tables retained. Use ?drop_tables=true to remove data."
  }
}
```

Optional query param `?drop_tables=true` to also DROP the program's tables. Default is to keep data.

---

## 9. Health Endpoint Design

### Endpoint: `GET /health`

**Response (200 OK when healthy):**

```json
{
  "status": "healthy",
  "version": "0.1.0",
  "uptime_seconds": 3600,
  "pipeline": {
    "mode": "streaming",
    "last_processed_slot": 280000000,
    "current_chain_slot": 280000050,
    "lag_slots": 50,
    "lag_seconds": 20
  },
  "programs": [
    {
      "program_id": "JUP6LiNT...",
      "status": "live",
      "instructions_indexed": 1523456,
      "accounts_tracked": 89012,
      "last_activity": "2026-04-05T12:34:56Z"
    },
    {
      "program_id": "whirL...",
      "status": "indexing",
      "instructions_indexed": 45000,
      "accounts_tracked": 1200,
      "backfill_progress": "72%"
    }
  ],
  "database": {
    "status": "connected",
    "pool_size": 10,
    "active_connections": 3,
    "idle_connections": 7
  }
}
```

**Response (503 Service Unavailable when unhealthy):**

```json
{
  "status": "unhealthy",
  "errors": [
    "Database connection pool exhausted",
    "Pipeline stalled: no new slots processed in 120 seconds"
  ],
  "database": {
    "status": "degraded",
    "pool_size": 10,
    "active_connections": 10,
    "idle_connections": 0
  }
}
```

### Health Check Logic

```
healthy IF:
  - Database pool has idle connections
  - Pipeline lag < 120 seconds (configurable)
  - No program in "error" state for > 5 minutes

unhealthy IF:
  - Database unreachable or pool exhausted
  - Pipeline stalled (no slot processed in 2 minutes)
  - All programs in error state
```

### Implementation Notes

- Use `sqlx::Pool::acquire()` with a short timeout to test DB connectivity.
- Track `last_processed_slot` in shared state, updated by the pipeline task.
- Fetch `current_chain_slot` from a cached RPC `getSlot()` call (refresh every 10s).
- Per-program counts can be cached and refreshed periodically (expensive COUNT queries).

---

## 10. axum Application Architecture

### AppState Design

```rust
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;

/// Immutable application configuration + shared resources
pub struct AppState {
    /// PostgreSQL connection pool
    pub db: PgPool,

    /// Solana RPC client
    pub rpc_client: solana_client::nonblocking::rpc_client::RpcClient,

    /// Program registry (mutable: programs added/removed at runtime)
    pub registry: Arc<RwLock<ProgramRegistry>>,

    /// Pipeline metrics (mutable: updated by indexing tasks)
    pub metrics: Arc<RwLock<PipelineMetrics>>,

    /// Application start time
    pub start_time: std::time::Instant,
}

pub struct ProgramRegistry {
    pub programs: HashMap<String, ProgramInfo>,
}

pub struct ProgramInfo {
    pub program_id: String,
    pub idl: ParsedIdl,
    pub table_prefix: String,
    pub status: ProgramStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct PipelineMetrics {
    pub last_processed_slot: u64,
    pub mode: PipelineMode,
}
```

### Why `Arc<AppState>` (not `Arc<RwLock<AppState>>`)?

The outer `AppState` is immutable after construction. Only the inner `registry` and `metrics` need mutability, and they have their own `RwLock`. This minimizes lock contention -- most requests only read the registry (shared read lock), and writes only happen during program registration (rare).

State is provided to the router with `.with_state(Arc::new(state))`:

```rust
Router::new()
    .nest("/api/programs", program_routes)
    .route("/health", get(handlers::health))
    .with_state(Arc::new(state))
```

Handlers extract it as `State(state): State<Arc<AppState>>`.

### Middleware Stack

```rust
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_http::compression::CompressionLayer;
use tower_http::timeout::TimeoutLayer;
use std::time::Duration;

fn build_app(state: AppState) -> Router {
    let api_routes = api_router();

    Router::new()
        .merge(api_routes)
        .route("/health", get(handlers::health))
        .with_state(Arc::new(state))
        // Middleware layers (outermost = last added)
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())  // tighten for production
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
}
```

Layer order matters. Layers wrap from bottom to top:

1. `TimeoutLayer` (outermost) -- enforces 30s request timeout
2. `CorsLayer` -- adds CORS headers
3. `CompressionLayer` -- compresses responses
4. `TraceLayer` (innermost) -- logs request/response details

### Module Structure

```
src/
  main.rs              -- Entry point, server setup, graceful shutdown
  api/
    mod.rs             -- Router construction
    handlers/
      mod.rs
      programs.rs      -- register, list, get, delete program handlers
      instructions.rs  -- query_instructions, list_instruction_types
      accounts.rs      -- query_accounts, get_account, list_account_types
      stats.rs         -- program_stats, instruction_count
      health.rs        -- health check handler
    filters.rs         -- FilterBuilder: query params -> Filter structs
    query_builder.rs   -- Filter structs -> sqlx QueryBuilder
    pagination.rs      -- Pagination parsing and cursor encode/decode
    response.rs        -- Response envelope helpers
    error.rs           -- ApiError enum + IntoResponse impl
  state.rs             -- AppState, ProgramRegistry, PipelineMetrics
```

### Server Setup and Graceful Shutdown

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "solarix=info,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    // Build state
    let db = PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    let state = AppState::new(db, rpc_client).await?;

    // Build app
    let app = build_app(state);

    // Bind and serve
    let addr = std::env::var("API_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("API server listening on {}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.expect("failed to listen for Ctrl+C");
    tracing::info!("Shutdown signal received");
}
```

---

## 11. Example curl Commands (for README)

These examples assume the API is running at `http://localhost:3000`.

### Register a Program

```bash
# Register Jupiter aggregator
curl -X POST http://localhost:3000/api/programs \
  -H "Content-Type: application/json" \
  -d '{"program_id": "JUP6LiNT54oPHphKMB3FuZYezYZRjXNUkGMtEGHNiN"}'
```

### Check Registration Status

```bash
curl http://localhost:3000/api/programs/JUP6LiNT54oPHphKMB3FuZYezYZRjXNUkGMtEGHNiN
```

### List All Programs

```bash
curl http://localhost:3000/api/programs
```

### Query Instructions with Filters

```bash
# Get all 'transfer' instructions with amount > 1000, sorted by slot desc
curl "http://localhost:3000/api/programs/TokenkegQ.../instructions/transfer?amount_gt=1000&sort=slot&order=desc&limit=50"

# Filter by recipient and time range
curl "http://localhost:3000/api/programs/TokenkegQ.../instructions/transfer?recipient=3Kcg...&block_time_gte=1711929600&block_time_lte=1712016000"

# Multiple filters combined (AND logic)
curl "http://localhost:3000/api/programs/JUP6.../instructions/route?amount_gt=1000000&slot_gte=280000000&limit=20"
```

### Query Accounts

```bash
# List all TokenLedger accounts
curl "http://localhost:3000/api/programs/JUP6.../accounts/TokenLedger?limit=50"

# Get a specific account
curl "http://localhost:3000/api/programs/JUP6.../accounts/TokenLedger/3Kcg..."
```

### Aggregation: Instruction Count Over Time

```bash
# Daily transfer count for the last 7 days
curl "http://localhost:3000/api/programs/TokenkegQ.../instructions/transfer/count?from=1711929600&to=1712534400&interval=day"

# Hourly route calls for the last 24 hours
curl "http://localhost:3000/api/programs/JUP6.../instructions/route/count?from=1712448000&to=1712534400&interval=hour"
```

### Program Statistics

```bash
curl http://localhost:3000/api/programs/JUP6.../stats
```

### Cursor-Based Pagination

```bash
# First page
curl "http://localhost:3000/api/programs/JUP6.../instructions/route?limit=100"

# Next page (using cursor from previous response)
curl "http://localhost:3000/api/programs/JUP6.../instructions/route?limit=100&cursor=eyJzbG90IjoyODAwMDAwMDAsInNpZyI6IjVLall..."
```

### Health Check

```bash
curl http://localhost:3000/health
```

### Deregister a Program

```bash
# Stop indexing, keep tables
curl -X DELETE http://localhost:3000/api/programs/JUP6LiNT54oPHphKMB3FuZYezYZRjXNUkGMtEGHNiN

# Stop indexing AND drop tables
curl -X DELETE "http://localhost:3000/api/programs/JUP6LiNT54oPHphKMB3FuZYezYZRjXNUkGMtEGHNiN?drop_tables=true"
```

---

## 12. Sources

### axum Framework

- [axum Router API docs (v0.8.8)](https://docs.rs/axum/latest/axum/routing/struct.Router.html)
- [axum Query extractor docs](https://docs.rs/axum/latest/axum/extract/struct.Query.html)
- [axum Path extractor docs](https://docs.rs/axum/latest/axum/extract/struct.Path.html)
- [axum State extractor docs](https://docs.rs/axum/latest/axum/extract/struct.State.html)
- [axum IntoResponse trait](https://docs.rs/axum/latest/axum/response/trait.IntoResponse.html)
- [axum Json response/extractor](https://docs.rs/axum/latest/axum/struct.Json.html)
- [Announcing axum 0.8.0 (Tokio blog)](https://tokio.rs/blog/2025-01-01-announcing-axum-0-8-0)
- [Rustfinity axum tutorial (2026)](https://www.rustfinity.com/blog/axum-rust-tutorial)
- [OneUpTime: Production-Ready REST APIs in Rust with Axum (2026)](https://oneuptime.com/blog/post/2026-01-07-rust-axum-rest-api/view)
- [Shuttle: Ultimate Guide to Axum](https://www.shuttle.dev/blog/2023/12/06/using-axum-rust)

### Dynamic Routes

- [GitHub Discussion #710: Adding Dynamic routes during runtime](https://github.com/tokio-rs/axum/discussions/710)
- [GitHub Discussion #2194: Dynamic routes](https://github.com/tokio-rs/axum/discussions/2194)
- [GitHub Discussion #1395: Add/update/remove router at runtime](https://github.com/tokio-rs/axum/discussions/1395)

### State Management

- [GitHub Discussion #1758: Multiple fields in AppState](https://github.com/tokio-rs/axum/discussions/1758)
- [GitHub Discussion #629: Mutate shared state in handlers](https://github.com/tokio-rs/axum/discussions/629)
- [Leapcell: Robust State Management in Axum](https://leapcell.io/blog/robust-state-management-in-actix-web-and-axum-applications)

### Error Handling

- [Leapcell: Elegant Error Handling in Axum with IntoResponse](https://leapcell.io/blog/elegant-error-handling-in-axum-actix-web-with-intoresponse)
- [StudyRaid: Error handling patterns in Axum handlers](https://app.studyraid.com/en/read/15308/530920/error-handling-patterns-in-axum-handlers)

### sqlx QueryBuilder

- [sqlx QueryBuilder API docs](https://docs.rs/sqlx/latest/sqlx/struct.QueryBuilder.html)
- [hostunibox: How to build safe dynamic query with sqlx](https://hostunibox.com/post/how-to-build-safe-dynamic-query-with-sqlx-in-rust)
- [GitHub: rust-query-builder for sqlx](https://github.com/wadtechab/rust-query-builder)
- [sqlx postgres types](https://docs.rs/sqlx/latest/sqlx/postgres/types/index.html)

### REST API Filter Conventions

- [Moesif: REST API Design -- Filtering, Sorting, Pagination](https://www.moesif.com/blog/technical/api-design/REST-API-Design-Filtering-Sorting-and-Pagination/)
- [Speakeasy: Filtering Responses Best Practices](https://www.speakeasy.com/api-design/filtering-responses)
- [DreamFactory: How to Filter Events in REST APIs](https://blog.dreamfactory.com/how-to-filter-events-in-rest-apis)
- [Strapi: REST API Filters](https://docs.strapi.io/cms/api/rest/filters)

### Pagination

- [paginator-axum crate](https://lib.rs/crates/paginator-axum)
- [paginator-rs on GitHub](https://github.com/maulanasdqn/paginator-rs)

### PostgreSQL

- [Neon: PostgreSQL DATE_TRUNC Function](https://neon.com/postgresql/postgresql-date-functions/postgresql-date_trunc)
- [Crunchy Data: 4 Ways to Create Date Bins in Postgres](https://www.crunchydata.com/blog/4-ways-to-create-date-bins-in-postgres-interval-date_trunc-extract-and-to_char)
- [PostgreSQL JSON Functions and Operators](https://www.postgresql.org/docs/current/functions-json.html)

### Middleware

- [DEV.to: API Development in Rust -- CORS, Tower Middleware, and Axum](https://dev.to/amaendeepm/api-development-in-rust-cors-tower-middleware-and-the-power-of-axum-397k)
- [DasRoot: High-Performance APIs with Axum and Rust (2026)](https://dasroot.net/posts/2026/04/building-high-performance-apis-axum-rust/)

### Solana Indexer Patterns

- [Helius: How to Index Solana Data](https://www.helius.dev/docs/rpc/how-to-index-solana-data)
- [Chainary: Analyzing Solana On-Chain Data with Custom Indexers](https://www.chainary.net/articles/analyzing-solana-on-chain-data-with-custom-indexers)
- [Solana Docs: Indexing](https://solana.com/docs/payments/accept-payments/indexing)
- [Bitquery: Solana Instructions API](https://docs.bitquery.io/docs/blockchain/Solana/solana-instructions/)
