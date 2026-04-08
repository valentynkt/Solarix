# Solarix

[![CI](https://github.com/valentynkt/Solarix/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/valentynkt/Solarix/actions/workflows/ci.yml)
[![MSRV: 1.88](https://img.shields.io/badge/MSRV-1.88-orange.svg)](rust-toolchain.toml)

Solarix is a universal Solana indexer built in Rust. Give it any Anchor program ID and it fetches the IDL directly from the blockchain, generates a typed PostgreSQL schema at runtime — no codegen, no recompile, no redeploy — then begins indexing transactions and account states through a concurrent backfill-plus-streaming pipeline. Decoded data is immediately queryable through a 13-endpoint REST API with typed filters, cursor pagination, and time-series aggregations. Built for the [Superteam Ukraine bounty](https://earn.superteam.fun/) (Middle level, 500 USDG).

```
POST /api/programs  { "program_id": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo" }

    Solarix:  fetch IDL on-chain  -->  CREATE SCHEMA + TABLES  -->  backfill + stream  -->  query API ready
```

---

## Table of Contents

- [Architecture](#architecture)
- [Technology Stack](#technology-stack)
- [Quick Start](#quick-start)
- [API Reference](#api-reference)
- [Configuration](#configuration)
- [Architectural Decisions](#architectural-decisions)
- [Testing](#testing)
- [Future Work](#future-work)
- [License](#license)

---

## Architecture

```mermaid
flowchart LR
    RPC["Solana RPC<br/>HTTP JSON-RPC"]
    WS["Solana WS<br/>logsSubscribe"]

    subgraph READ["Read Layer"]
        R1[RpcClient]
        R2[WsTransactionStream]
    end

    subgraph DECODE["Decode Layer"]
        D1["ChainparserDecoder<br/>Borsh to JSON"]
    end

    subgraph STORE["Store Layer"]
        S1["StorageWriter<br/>batch INSERT + upsert"]
    end

    subgraph SERVE["Serve Layer"]
        A1["axum Router<br/>13 endpoints"]
    end

    PG[("PostgreSQL 16<br/>typed + JSONB")]
    CLI["REST Clients"]

    RPC -->|getBlock| R1
    WS -->|logsSubscribe| R2
    R1 -->|"mpsc (256)"| D1
    R2 -->|"mpsc (256)"| D1
    D1 -->|"mpsc (256)"| S1
    S1 --> PG
    PG --> A1
    A1 --> CLI
```

The pipeline has four layers connected by bounded Tokio channels (capacity 256). Solana data enters through HTTP RPC (for historical blocks) and WebSocket (for live transactions), flows through the Borsh decoder, gets written to PostgreSQL, and is served by the axum REST API.

```mermaid
stateDiagram-v2
    [*] --> Initializing

    Initializing --> Backfilling : checkpoint behind chain tip
    Initializing --> Streaming : no gap detected

    Backfilling --> Streaming : caught up to tip

    Streaming --> CatchingUp : gap detected
    CatchingUp --> Streaming : gap filled

    Streaming --> ShuttingDown : SIGTERM or SIGINT
    ShuttingDown --> [*] : drain channels, flush DB
```

The pipeline starts in `Initializing`, transitions to `Backfilling` if there is a historical gap, then enters `Streaming`. A WebSocket slot gap triggers `CatchingUp` for a mini-backfill. `SIGTERM` begins a graceful shutdown: in-flight messages drain, final writes flush to PostgreSQL.

### Cold Start Strategy

On cold start, Solarix runs backfill and streaming **concurrently** (Option C). Both write to the same tables with `INSERT ON CONFLICT DO NOTHING`, so duplicate processing is harmless and crash recovery is automatic. Live data is available immediately while historical backfill catches up in the background.

### Database Layout

Each registered program gets its own PostgreSQL schema:

```
public/
  programs          -- program registry (program_id, schema_name, idl_hash, status)
  indexer_state     -- per-program pipeline state (last_slot, status, counters)

{program_name}_{program_id_prefix}/        -- e.g. lb_clmm_lbuzkhrx/
  lbpair            -- one table per account type, upsert on pubkey
  position          -- (promoted typed columns + JSONB data column)
  binarray
  ...
  _instructions     -- all decoded instructions (append-only)
  _checkpoints      -- slot cursor per stream (backfill, stream)
  _metadata         -- field names and types from IDL
```

Simple scalars (`u64`, `bool`, `String`, `Pubkey`) are promoted to native PostgreSQL columns for fast indexed queries. Complex nested types live in a JSONB `data` column with GIN indexes. Every field is preserved regardless of promotion — there is no data loss on schema generation.

<details>
<summary>Source module tree (20 modules, 14,109 lines)</summary>

```
src/
  main.rs              Entry point: signal handling, pipeline + API startup
  lib.rs               Public module declarations
  config.rs            22 env vars via clap, validation
  types.rs             DecodedInstruction, DecodedAccount, BlockData, TransactionData
  registry.rs          Two-phase program registration state machine
  runtime_stats.rs     Process-wide atomic counters for Prometheus metrics
  startup.rs           Router + AppState construction, extracted for testability

  idl/
    mod.rs             IdlManager: cache, parse, validate (v0.30+ only)
    fetch.rs           Fetch cascade: on-chain PDA -> bundled -> manual upload

  decoder/
    mod.rs             ChainparserDecoder: Borsh deserializer for 18+ IDL types

  pipeline/
    mod.rs             PipelineOrchestrator: 5-state machine, concurrent backfill+stream
    rpc.rs             RPC client with rate limiting (governor) and retry (backon)
    ws.rs              WebSocket logsSubscribe with dedup cache and heartbeat

  storage/
    mod.rs             DB pool init, system table bootstrap
    schema.rs          IDL -> CREATE TABLE/INDEX DDL, promoted column detection
    writer.rs          Batch INSERT...UNNEST, account upsert, checkpoint management
    queries.rs         Dynamic query builder for API filters

  api/
    mod.rs             axum Router, AppState, ApiError -> HTTP status mapping
    handlers.rs        13 endpoint handlers with pagination and cursor encoding
    filters.rs         Filter parsing, operator validation against IDL
```

</details>

For the full architecture deep-dive, see [docs/architecture.md](docs/architecture.md).

---

## Technology Stack

| Crate                                     | Version     | Purpose                                       |
| ----------------------------------------- | ----------- | --------------------------------------------- |
| `axum`                                    | 0.8         | HTTP framework and router                     |
| `sqlx`                                    | 0.8         | Async PostgreSQL driver                       |
| `tokio`                                   | 1 (latest)  | Async runtime                                 |
| `anchor-lang-idl-spec`                    | 0.1         | Anchor IDL type definitions                   |
| `tokio-tungstenite`                       | 0.26        | WebSocket client for `logsSubscribe`          |
| `governor`                                | 0.10        | Async GCRA rate limiting                      |
| `backon`                                  | 1           | Exponential backoff retry                     |
| `thiserror`                               | 2           | Typed error enums                             |
| `tracing` + `tracing-subscriber`          | 0.1         | Structured logging (JSON + pretty)            |
| `metrics` + `metrics-exporter-prometheus` | 0.24 / 0.17 | Prometheus metrics endpoint                   |
| `clap`                                    | 4           | CLI and environment variable configuration    |
| `sha2`                                    | 0.10        | Instruction/account discriminator computation |
| `flate2`                                  | 1           | IDL zlib decompression                        |
| `tower-http`                              | 0.6         | Request ID propagation and tracing middleware |

---

## Quick Start

### Docker Compose (Recommended)

1. Clone the repository and enter the directory:

   ```bash
   git clone https://github.com/valentynkit/solarix.git
   cd solarix
   ```

2. Start the full stack (PostgreSQL 16 + Solarix):

   ```bash
   docker compose up --build
   ```

   Verify the API is ready:

   ```bash
   curl -s http://localhost:3000/health | jq
   ```

3. Register a program — Solarix fetches the IDL on-chain, creates the schema, and starts indexing. The example below uses **Meteora DLMM** because it has rich on-chain activity and was verified end-to-end during the Sprint-4 e2e gate (199 swaps indexed in 5 minutes):

   ```bash
   curl -s -X POST http://localhost:3000/api/programs \
     -H "Content-Type: application/json" \
     -d '{"program_id":"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"}' | jq
   ```

   ```json
   {
     "data": {
       "program_id": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
       "status": "schema_created",
       "idl_source": "onchain"
     },
     "meta": {
       "message": "Program registered. Indexing will begin on next restart."
     }
   }
   ```

   The schema is generated synchronously during the request. `GET /api/programs/{id}` will return the same `"status": "schema_created"` immediately after, along with `"schema_name": "lb_clmm_lbuzkhrx"`.

   Restart the service so the pipeline auto-starts for the registered program:

   ```bash
   docker compose restart solarix
   ```

4. Query decoded data once indexing begins (allow a minute for the first slot to appear):

   ```bash
   # Decoded swap instructions (cursor-paginated)
   curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap?limit=5" | jq

   # Filter: swaps where amount_in > 0.001 SOL (1,000,000 lamports)
   curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap?filter=data.amount_in_gt=1000000&limit=10" | jq

   # Time-series swap count by hour
   curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap/count?interval=hour" | jq

   # Indexing statistics
   curl -s http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/stats | jq
   ```

5. Run the automated end-to-end demo:
   ```bash
   bash demo.sh
   ```

### Local Development (No Docker)

Start a local PostgreSQL instance:

```bash
docker run -d --name solarix-db \
  -e POSTGRES_DB=solarix \
  -e POSTGRES_USER=solarix \
  -e POSTGRES_PASSWORD=solarix \
  -p 5432:5432 \
  postgres:16
```

Configure environment variables and run:

```bash
export DATABASE_URL="postgres://solarix:solarix@localhost:5432/solarix"
export SOLANA_RPC_URL="https://api.devnet.solana.com"
cargo run
```

Build, test, and lint:

```bash
cargo build              # debug build
cargo build --release    # optimized build
cargo watch -x run       # hot-reload during development

cargo test               # 383 tests across all suites (MSRV 1.88)
cargo test --features integration   # integration tests (requires Docker)
cargo clippy             # strict lints (unwrap/expect/panic denied)
cargo fmt -- --check     # formatting check
```

---

## API Reference

Base URL: `http://localhost:3000`

### Programs

| Method   | Endpoint                              | Description                                                    |
| -------- | ------------------------------------- | -------------------------------------------------------------- |
| `POST`   | `/api/programs`                       | Register a program (auto-fetches IDL or accepts manual upload) |
| `GET`    | `/api/programs`                       | List all registered programs                                   |
| `GET`    | `/api/programs/{id}`                  | Get program details and schema info                            |
| `DELETE` | `/api/programs/{id}?drop_tables=true` | Deregister a program (optionally drop its schema)              |
| `GET`    | `/api/programs/{id}/stats`            | Indexing statistics (total instructions, accounts, last slot)  |

```bash
# Register a program
curl -s -X POST http://localhost:3000/api/programs \
  -H "Content-Type: application/json" \
  -d '{"program_id":"LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo"}' | jq
# {
#   "data": {
#     "program_id": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
#     "status": "schema_created",
#     "idl_source": "onchain"
#   },
#   "meta": { "message": "Program registered. Indexing will begin on next restart." }
# }

# Indexing statistics
curl -s http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/stats | jq
# {
#   "data": {
#     "program_id": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
#     "total_instructions": 142831,
#     "total_accounts": 9204,
#     "last_processed_slot": 318472910
#   }
# }
```

### Instructions & Accounts

| Method | Endpoint                                       | Description                                     |
| ------ | ---------------------------------------------- | ----------------------------------------------- |
| `GET`  | `/api/programs/{id}/instructions`              | List instruction types from IDL                 |
| `GET`  | `/api/programs/{id}/instructions/{name}`       | Query decoded instructions with filters         |
| `GET`  | `/api/programs/{id}/instructions/{name}/count` | Count instructions, optionally by time interval |
| `GET`  | `/api/programs/{id}/accounts`                  | List account types from IDL                     |
| `GET`  | `/api/programs/{id}/accounts/{type}`           | Query decoded accounts with filters             |
| `GET`  | `/api/programs/{id}/accounts/{type}/{pubkey}`  | Get a single account by pubkey                  |

```bash
# Query decoded swap instructions (cursor pagination)
curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap?limit=5" | jq

# Filter: swaps with amount_in > 0.001 SOL
curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap?filter=data.amount_in_gt=1000000&limit=10" | jq

# Time-series count by hour
curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap/count?interval=hour" | jq

# List account types
curl -s http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/accounts | jq

# Query accounts with offset pagination
curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/accounts/lbpair?limit=5&offset=0" | jq
```

### Observability

| Method | Endpoint   | Description                                                        |
| ------ | ---------- | ------------------------------------------------------------------ |
| `GET`  | `/health`  | System health with DB connectivity and per-program pipeline status |
| `GET`  | `/metrics` | Prometheus metrics (requires `SOLARIX_METRICS_ENABLED=true`)       |

```bash
# Health check
curl -s http://localhost:3000/health | jq
# {
#   "status": "healthy",
#   "database": "connected",
#   "programs": [
#     {
#       "program_id": "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
#       "status": "streaming",
#       "last_processed_slot": 318472910
#     }
#   ]
# }

# Prometheus metrics (enable with SOLARIX_METRICS_ENABLED=true)
curl -s http://localhost:3000/metrics | grep "^solarix_"
```

### Filter Syntax

Append `?filter=` to any instruction or account query endpoint:

```bash
?filter=data.amount_gt=1000000             # JSONB field, greater than
?filter=data.authority_eq=So11111111111111111111111111111111111111112   # equals (full address)
?filter=data.is_active_eq=true             # boolean
?filter=slot_gt=300000000                  # promoted BIGINT column (no "data." prefix)
```

Combine multiple filters with `&`:

```bash
?filter=data.amount_in_gt=1000000&filter=data.min_amount_out_lt=5000000000
```

| Operator    | Meaning                                                                        |
| ----------- | ------------------------------------------------------------------------------ |
| `_eq`       | Equals                                                                         |
| `_ne`       | Not equals                                                                     |
| `_gt`       | Greater than                                                                   |
| `_gte`      | Greater than or equal                                                          |
| `_lt`       | Less than                                                                      |
| `_lte`      | Less than or equal                                                             |
| `_in`       | In list (comma-separated)                                                      |
| `_contains` | JSONB containment (`@>`) — matches rows whose JSON value contains the argument |

### Pagination

```bash
?limit=50&offset=0          # Offset-based (accounts)
?limit=50&cursor=abc123     # Cursor-based (instructions, stable on append-only data)
```

### Error Responses

All errors return structured JSON:

```bash
curl -s "http://localhost:3000/api/programs/LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo/instructions/swap?filter=nonexistent_gt=1" | jq
```

```json
{
  "error": {
    "code": "INVALID_FILTER",
    "message": "Unknown field 'nonexistent'. Available: amount_in, min_amount_out, ...",
    "available_fields": ["amount_in", "min_amount_out"]
  }
}
```

| Code                         | HTTP Status | Meaning                                                         |
| ---------------------------- | ----------- | --------------------------------------------------------------- |
| `PROGRAM_NOT_FOUND`          | 404         | Program ID not registered                                       |
| `PROGRAM_ALREADY_REGISTERED` | 409         | Duplicate registration attempt                                  |
| `INVALID_FILTER`             | 400         | Bad filter syntax or unknown field (returns `available_fields`) |
| `INVALID_REQUEST`            | 400         | Malformed request body                                          |
| `IDL_ERROR`                  | 422         | IDL fetch/parse failure                                         |
| `STORAGE_ERROR`              | 500         | Database error                                                  |
| `QUERY_FAILED`               | 500         | Query execution error                                           |

---

## Configuration

All parameters are configured via environment variables (or CLI flags). Copy `.env.example` to `.env` and customize. Only `DATABASE_URL` is required.

### Core

| Variable         | Type   | Default                               | Description                            |
| ---------------- | ------ | ------------------------------------- | -------------------------------------- |
| `DATABASE_URL`   | String | _(required)_                          | PostgreSQL connection string           |
| `SOLANA_RPC_URL` | String | `https://api.mainnet-beta.solana.com` | Solana JSON-RPC endpoint               |
| `SOLANA_WS_URL`  | String | _(derived from RPC URL)_              | WebSocket endpoint for `logsSubscribe` |

### Database

| Variable              | Type | Default | Description                  |
| --------------------- | ---- | ------- | ---------------------------- |
| `SOLARIX_DB_POOL_MIN` | u64  | `2`     | Minimum connection pool size |
| `SOLARIX_DB_POOL_MAX` | u64  | `10`    | Maximum connection pool size |

### API

| Variable                    | Type   | Default   | Description               |
| --------------------------- | ------ | --------- | ------------------------- |
| `SOLARIX_API_HOST`          | String | `0.0.0.0` | Bind address              |
| `SOLARIX_API_PORT`          | u64    | `3000`    | Bind port                 |
| `SOLARIX_API_PAGE_SIZE`     | u64    | `50`      | Default page size         |
| `SOLARIX_API_MAX_PAGE_SIZE` | u64    | `1000`    | Maximum allowed page size |

### Pipeline

| Variable                           | Type    | Default  | Description                                  |
| ---------------------------------- | ------- | -------- | -------------------------------------------- |
| `SOLARIX_RPC_RPS`                  | u64     | `10`     | RPC rate limit (requests/second)             |
| `SOLARIX_BACKFILL_CHUNK_SIZE`      | u64     | `50000`  | Slots per backfill batch                     |
| `SOLARIX_START_SLOT`               | u64     | _(auto)_ | Override backfill start slot                 |
| `SOLARIX_END_SLOT`                 | u64     | _(auto)_ | Override backfill end slot                   |
| `SOLARIX_INDEX_FAILED_TXS`         | bool    | `false`  | Index failed transactions                    |
| `SOLARIX_CHANNEL_CAPACITY`         | u64     | `256`    | Bounded channel size between pipeline stages |
| `SOLARIX_CHECKPOINT_INTERVAL_SECS` | seconds | `10`     | Checkpoint persistence interval              |

### Retry & Resilience

| Variable                                 | Type    | Default | Description                                 |
| ---------------------------------------- | ------- | ------- | ------------------------------------------- |
| `SOLARIX_RETRY_INITIAL_MS`               | u64     | `500`   | Initial retry backoff (ms)                  |
| `SOLARIX_RETRY_MAX_MS`                   | u64     | `30000` | Maximum retry backoff (ms)                  |
| `SOLARIX_RETRY_TIMEOUT_SECS`             | seconds | `300`   | Total retry timeout before giving up        |
| `SOLARIX_MAX_CONSECUTIVE_FETCH_FAILURES` | u64     | `100`   | Max consecutive RPC failures before halt    |
| `SOLARIX_SHUTDOWN_DRAIN_SECS`            | seconds | `15`    | In-flight message drain timeout on shutdown |
| `SOLARIX_SHUTDOWN_DB_FLUSH_SECS`         | seconds | `10`    | Final DB write timeout on shutdown          |

### WebSocket

| Variable                        | Type    | Default | Description                    |
| ------------------------------- | ------- | ------- | ------------------------------ |
| `SOLARIX_WS_PING_INTERVAL_SECS` | seconds | `30`    | Heartbeat ping interval        |
| `SOLARIX_WS_PONG_TIMEOUT_SECS`  | seconds | `10`    | Pong response timeout          |
| `SOLARIX_DEDUP_CACHE_SIZE`      | u64     | `10000` | Signature dedup cache capacity |

### Logging

| Variable             | Type   | Default | Description                                                  |
| -------------------- | ------ | ------- | ------------------------------------------------------------ |
| `SOLARIX_LOG_LEVEL`  | String | `info`  | Log level (`trace`, `debug`, `info`, `warn`, `error`)        |
| `SOLARIX_LOG_FORMAT` | String | `json`  | Log format (`json` for production, `pretty` for development) |

### Observability

| Variable                  | Type   | Default    | Description                              |
| ------------------------- | ------ | ---------- | ---------------------------------------- |
| `SOLARIX_METRICS_ENABLED` | bool   | `false`    | Enable Prometheus `/metrics` endpoint    |
| `SOLARIX_METRICS_PATH`    | String | `/metrics` | Path for the Prometheus metrics endpoint |

---

## Architectural Decisions

**1. Runtime schema generation**

PostgreSQL DDL is generated directly from the Anchor IDL at program registration time — no code generation step required.

_Alternatives:_ compile-time codegen (anchor-gen, seahorse) requires a recompile and redeploy for every new program; predefined tables hardcode specific programs and break universality.

_Why:_ Zero additional configuration. The IDL is the only input. Any Anchor program works on the first `POST /api/programs`.

---

**2. Hybrid typed + JSONB storage**

Simple scalar fields (`u64`, `bool`, `Pubkey`, `String`) are promoted to native PostgreSQL columns with proper types. Complex types (structs, vecs, enums) live in a JSONB `data` column with a GIN index.

_Alternatives:_ all-JSONB is simple but makes range queries slow and loses type coercion; fully normalised relational storage creates schema explosion for deeply nested types.

_Why:_ Native columns give fast indexed queries on the most-filtered fields. JSONB fallback preserves every field regardless of type complexity, with no data loss on schema generation.

---

**3. Concurrent backfill + streaming (Option C)**

On cold start, historical backfill (HTTP RPC) and live streaming (WebSocket) run simultaneously. Both write to the same tables with `INSERT ON CONFLICT DO NOTHING`.

_Alternatives:_ sequential (backfill then stream) leaves a gap window and delays live data; streaming-only loses all historical data.

_Why:_ Live data is available immediately. Idempotent writes make overlap harmless. Crash recovery is automatic — both paths resume from their checkpoints independently.

---

**4. Typed `thiserror` error enums**

Five module-level error enums (`IdlError`, `DecodeError`, `StorageError`, `PipelineError`, `ApiError`) with explicit `is_retryable()` classification and `clippy::expect_used = "deny"`.

_Alternatives:_ `anyhow` erases the type needed for retry classification; `Box<dyn Error>` requires string matching to distinguish retryable from fatal errors.

_Why:_ The compiler enforces exhaustive error handling at every call site. Retry logic in the pipeline depends on typed classification. Zero `unwrap` in production paths.

---

**5. GCRA rate limiting via `governor`**

All outbound RPC calls pass through an async-native Generic Cell Rate Algorithm limiter set to 10 RPS by default.

_Alternatives:_ `sleep`-based throttle blocks the Tokio executor and wastes wall time; no rate limit causes 429 storms on public endpoints that corrupt backfill progress.

_Why:_ `governor` is fully async (no blocking), precise, and handles the ~10 RPS public Solana RPC limit with configurable burst tolerance.

---

## Testing

**Unit + Property Tests**

Run the full unit and property-based test suite:

```bash
cargo test
```

The decoder is verified with `proptest`: it generates random values for all 18+ supported Anchor IDL types, Borsh-serialises them, decodes via `ChainparserDecoder`, and asserts the JSON output matches field-for-field. The full suite runs 383 tests across 21 suites.

**Integration Tests (require Docker)**

Integration tests spin up a PostgreSQL 16 container per test via Testcontainers:

```bash
cargo test --features integration
```

Covers: schema generation against a real database, filter SQL execution (including the Sprint-4 BIGINT vs TEXT regression), the full register → decode → query roundtrip, and a LiteSVM end-to-end path that deploys a real Anchor program into an in-process Solana VM.

**Mainnet Smoke (nightly CI)**

```bash
cargo test --release --features mainnet-smoke -- mainnet_smoke
```

Run automatically by `.github/workflows/nightly.yml`. Not required for local development.

**CI**

Every push to `main` runs nine parallel jobs: lint, unit, integration, coverage, fuzz-smoke, security audit, docker smoke, MSRV check, and a toolchain matrix (stable + beta). See [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

---

## Future Work

### Planned

These features are in active development:

- **Multi-program concurrent orchestration** — index multiple programs simultaneously with a shared rate limiter and per-program `JoinSet` lifecycle management
- **Pipeline auto-start on API registration** — `POST /api/programs` immediately spawns the pipeline without requiring a service restart

### Post-Submission Ideas

- **Geyser/gRPC plugin transport** — replace HTTP RPC polling with a Geyser plugin connection for lower-latency block delivery
- **GraphQL query layer** — expose the same typed data through a GraphQL interface for richer client queries
- **Schema evolution** — `ALTER TABLE` on IDL update when new fields are added to an existing program
- **Grafana dashboard out-of-box** — bundle a preconfigured Grafana dashboard JSON for the Prometheus metrics
- **Legacy Anchor IDL v0.29 support** — extend the fetch cascade for programs that predate the `metadata.spec` field
- **Signature-list batch mode** — accept a list of transaction signatures for targeted backfill without slot-range scanning
- **WebSocket backpressure metrics** — expose queue depth and drop rate through the `/metrics` endpoint

---

## License

MIT — see [LICENSE](LICENSE).

---

_Built for the [Superteam Ukraine bounty](https://earn.superteam.fun/).
Read the [build story](docs/x-thread.md)._
