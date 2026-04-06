# Solarix

**Universal Solana indexer** that dynamically generates PostgreSQL schemas from any Anchor IDL at runtime.

No codegen. No recompile. No redeploy. Give it a program ID and it fetches the IDL, creates typed tables, indexes transactions and account states, and exposes a REST API for queries.

```
POST /api/programs  { "program_id": "TokenkegQ..." }

    Solarix:  fetch IDL on-chain  -->  CREATE SCHEMA + TABLES  -->  backfill + stream  -->  query API ready
```

Built in Rust for the [Superteam Ukraine bounty](https://earn.superteam.fun/) (Middle level, 500 USDG).

---

## Features

| Category                   | What it does                                                                                                  |
| -------------------------- | ------------------------------------------------------------------------------------------------------------- |
| **Dynamic schema**         | Parses any Anchor IDL (v0.30+) at runtime and generates PostgreSQL tables with typed columns + JSONB fallback |
| **Zero-config indexing**   | Auto-fetches IDL from on-chain PDA, falls back to bundled registry, or accepts manual upload                  |
| **Batch + real-time**      | Concurrent historical backfill (HTTP RPC) and live streaming (WebSocket `logsSubscribe`)                      |
| **Cold start recovery**    | Checkpoint-based crash recovery with automatic gap detection and mini-backfill                                |
| **12-endpoint REST API**   | Program management, instruction/account queries, filters, aggregations, stats                                 |
| **Production reliability** | Rate limiting, exponential backoff with jitter, graceful shutdown (SIGTERM/SIGINT), signature dedup           |
| **Strict Rust**            | `unsafe` forbidden, `unwrap`/`expect`/`panic` denied by clippy, `thiserror` enums everywhere                  |

---

## Quick Start

### Docker Compose (recommended)

```bash
git clone https://github.com/valentynkit/solarix.git
cd solarix
docker compose up --build
```

This starts PostgreSQL 16 + Solarix. The API is available at `http://localhost:3000` once the health check passes.

### Register a Program

```bash
# Register any Anchor program by its program ID
curl -s -X POST http://localhost:3000/api/programs \
  -H "Content-Type: application/json" \
  -d '{"program_id": "JUP6LkMUje6dvM2FeAg8pUhfHayPdTHaFxVMLsXkICL"}' | jq
```

```json
{
  "data": {
    "program_id": "JUP6LkMUje6dvM2FeAg8pUhfHayPdTHaFxVMLsXkICL",
    "status": "schema_created",
    "schema_name": "jupiter_v6_jup6lkmu"
  }
}
```

Solarix fetches the IDL from on-chain, generates a PostgreSQL schema with typed tables for each account type and a unified instructions table, then begins indexing.

### Query Indexed Data

```bash
# List all account types for a program
curl -s http://localhost:3000/api/programs/JUP6LkMUje6dvM2FeAg8pUhfHayPdTHaFxVMLsXkICL/accounts | jq

# Query accounts with filters
curl -s "http://localhost:3000/api/programs/JUP6.../accounts/Pool?filter=data.token_a_amount_gt=1000000&limit=10" | jq

# Query instructions
curl -s "http://localhost:3000/api/programs/JUP6.../instructions/swap?limit=5" | jq

# Program statistics
curl -s http://localhost:3000/api/programs/JUP6.../stats | jq

# Health check (includes per-program indexing status)
curl -s http://localhost:3000/health | jq
```

---

## Architecture

Four-layer pipeline connected by bounded Tokio channels:

```
                        READ                    DECODE                  STORE                SERVE
               ┌───────────────────┐   ┌───────────────────┐   ┌──────────────────┐   ┌──────────────┐
               │  HTTP RPC         │   │                   │   │  StorageWriter   │   │  REST API    │
Solana ──────► │  getBlock         ├──►│  Borsh Decoder    ├──►│  batch INSERT    ├──►│  axum        │ ◄── Clients
  RPC          │  getProgramAccts  │   │  IDL type         │   │  account upsert  │   │  12 endpoints│
               ├───────────────────┤   │  registry         │   │                  │   │  filters     │
               │  WebSocket        │   │                   │   │                  │   │  pagination  │
Solana ──────► │  logsSubscribe    ├──►│                   ├──►│                  │   │              │
  WS           └───────────────────┘   └───────────────────┘   └────────┬─────────┘   └──────┬───────┘
                                                                        │                     │
                                                                        ▼                     │
                                                                ┌──────────────────┐          │
                                                                │   PostgreSQL 16  │ ◄────────┘
                                                                │   typed + JSONB  │
                                                                └──────────────────┘
```

### Pipeline State Machine

```
                              checkpoint < tip
              ┌──────────────┐ ──────────────► ┌──────────────┐
              │ Initializing │                 │ Backfilling  │
              └──────┬───────┘                 └──────┬───────┘
                     │ no gap                   caught up │
                     ▼                                    ▼
              ┌──────────────┐   gap detected   ┌──────────────┐
              │              │ ◄──────────────── │              │
              │  Streaming   │                   │  CatchingUp  │
              │              │ ──────────────► │              │
              └──────┬───────┘   gap filled     └──────────────┘
                     │ SIGTERM
                     ▼
              ┌──────────────┐
              │ ShuttingDown │
              └──────────────┘
```

During cold start, Solarix runs backfill and streaming **concurrently** (Option C). Both write to the same tables with `INSERT ON CONFLICT DO NOTHING`, so duplicate processing is harmless and crash recovery is automatic.

### Database Layout

Each registered program gets its own PostgreSQL schema:

```
public/
  programs          -- program registry (program_id, schema_name, idl_hash, status)
  indexer_state     -- per-program pipeline state (last_slot, status, counters)

{program_name}_{program_id_prefix}/        -- e.g. jupiter_v6_jup6lkmu/
  pool              -- one table per account type, upsert on pubkey
  token_ledger      -- (promoted typed columns + JSONB data column)
  ...
  _instructions     -- all decoded instructions (append-only)
  _checkpoints      -- slot cursor per stream (backfill, stream)
  _metadata         -- field names and types from IDL
```

**Hybrid column strategy**: simple scalars (u64, bool, String, Pubkey) are promoted to native PostgreSQL columns for fast filtering. Complex types (structs, vecs) live in the JSONB `data` column with GIN indexes.

For the full architecture deep-dive, see [docs/architecture.md](docs/architecture.md).

---

## API Reference

Base URL: `http://localhost:3000`

### Programs

| Method   | Endpoint                              | Description                                                    |
| -------- | ------------------------------------- | -------------------------------------------------------------- |
| `POST`   | `/api/programs`                       | Register a program (auto-fetches IDL or accepts manual upload) |
| `GET`    | `/api/programs`                       | List all registered programs                                   |
| `GET`    | `/api/programs/{id}`                  | Get program details and schema info                            |
| `DELETE` | `/api/programs/{id}?drop_tables=true` | Deregister program (optionally drop schema)                    |
| `GET`    | `/api/programs/{id}/stats`            | Indexing statistics (total instructions, accounts, last slot)  |

### Instructions

| Method | Endpoint                                       | Description                             |
| ------ | ---------------------------------------------- | --------------------------------------- |
| `GET`  | `/api/programs/{id}/instructions`              | List instruction types from IDL         |
| `GET`  | `/api/programs/{id}/instructions/{name}`       | Query decoded instructions with filters |
| `GET`  | `/api/programs/{id}/instructions/{name}/count` | Count instructions matching filters     |

### Accounts

| Method | Endpoint                                      | Description                         |
| ------ | --------------------------------------------- | ----------------------------------- |
| `GET`  | `/api/programs/{id}/accounts`                 | List account types from IDL         |
| `GET`  | `/api/programs/{id}/accounts/{type}`          | Query decoded accounts with filters |
| `GET`  | `/api/programs/{id}/accounts/{type}/{pubkey}` | Get a single account by pubkey      |

### Health

| Method | Endpoint  | Description                                                        |
| ------ | --------- | ------------------------------------------------------------------ |
| `GET`  | `/health` | System health with DB connectivity and per-program pipeline status |

### Filter Syntax

Append `?filter=` to instruction and account query endpoints:

```
# Comparison operators
?filter=data.amount_gt=1000000
?filter=data.authority_eq=So11111111111111111111111111111111111111112
?filter=data.is_active_eq=true

# Supported operators: _eq, _neq, _gt, _gte, _lt, _lte, _in, _like
# Combine with AND: &filter=data.amount_gt=100&filter=data.owner_eq=...
```

### Pagination

```
?limit=50&offset=0          # Offset-based (accounts)
?limit=50&cursor=abc123     # Cursor-based (instructions)
```

### Error Responses

All errors return structured JSON:

```json
{
  "error": {
    "code": "PROGRAM_NOT_FOUND",
    "message": "Program 'abc...' is not registered"
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

All parameters are configured via environment variables (or CLI flags). Only `DATABASE_URL` is required.

### Core

| Variable         | Default                               | Description                            |
| ---------------- | ------------------------------------- | -------------------------------------- |
| `DATABASE_URL`   | _(required)_                          | PostgreSQL connection string           |
| `SOLANA_RPC_URL` | `https://api.mainnet-beta.solana.com` | Solana JSON-RPC endpoint               |
| `SOLANA_WS_URL`  | _(derived from RPC URL)_              | WebSocket endpoint for `logsSubscribe` |

### Database

| Variable              | Default | Description                  |
| --------------------- | ------- | ---------------------------- |
| `SOLARIX_DB_POOL_MIN` | `2`     | Minimum connection pool size |
| `SOLARIX_DB_POOL_MAX` | `10`    | Maximum connection pool size |

### API

| Variable                    | Default   | Description               |
| --------------------------- | --------- | ------------------------- |
| `SOLARIX_API_HOST`          | `0.0.0.0` | Bind address              |
| `SOLARIX_API_PORT`          | `3000`    | Bind port                 |
| `SOLARIX_API_PAGE_SIZE`     | `50`      | Default page size         |
| `SOLARIX_API_MAX_PAGE_SIZE` | `1000`    | Maximum allowed page size |

### Pipeline

| Variable                           | Default  | Description                                  |
| ---------------------------------- | -------- | -------------------------------------------- |
| `SOLARIX_RPC_RPS`                  | `10`     | RPC rate limit (requests/second)             |
| `SOLARIX_BACKFILL_CHUNK_SIZE`      | `50000`  | Slots per backfill batch                     |
| `SOLARIX_START_SLOT`               | _(auto)_ | Override backfill start slot                 |
| `SOLARIX_END_SLOT`                 | _(auto)_ | Override backfill end slot                   |
| `SOLARIX_INDEX_FAILED_TXS`         | `false`  | Index failed transactions                    |
| `SOLARIX_CHANNEL_CAPACITY`         | `256`    | Bounded channel size between pipeline stages |
| `SOLARIX_CHECKPOINT_INTERVAL_SECS` | `10`     | Checkpoint persistence interval              |

### Retry and Resilience

| Variable                                 | Default | Description                                 |
| ---------------------------------------- | ------- | ------------------------------------------- |
| `SOLARIX_RETRY_INITIAL_MS`               | `500`   | Initial retry backoff                       |
| `SOLARIX_RETRY_MAX_MS`                   | `30000` | Maximum retry backoff                       |
| `SOLARIX_RETRY_TIMEOUT_SECS`             | `300`   | Total retry timeout                         |
| `SOLARIX_MAX_CONSECUTIVE_FETCH_FAILURES` | `100`   | Max consecutive RPC failures before halt    |
| `SOLARIX_SHUTDOWN_DRAIN_SECS`            | `15`    | In-flight message drain timeout on shutdown |
| `SOLARIX_SHUTDOWN_DB_FLUSH_SECS`         | `10`    | Final DB write timeout on shutdown          |

### WebSocket

| Variable                        | Default | Description                    |
| ------------------------------- | ------- | ------------------------------ |
| `SOLARIX_WS_PING_INTERVAL_SECS` | `30`    | Heartbeat ping interval        |
| `SOLARIX_WS_PONG_TIMEOUT_SECS`  | `10`    | Pong response timeout          |
| `SOLARIX_DEDUP_CACHE_SIZE`      | `10000` | Signature dedup cache capacity |

### Logging

| Variable             | Default | Description                                                  |
| -------------------- | ------- | ------------------------------------------------------------ |
| `SOLARIX_LOG_LEVEL`  | `info`  | Log level (trace, debug, info, warn, error)                  |
| `SOLARIX_LOG_FORMAT` | `json`  | Log format (`json` for production, `pretty` for development) |

---

## Development

### Prerequisites

- Rust 1.75+ (2021 edition)
- PostgreSQL 16
- Docker (optional, for containerized setup)

### Build

```bash
cargo build              # debug build
cargo build --release    # optimized release build
cargo watch -x run       # hot-reload during development
```

### Test

```bash
cargo test               # 251 tests across 5 suites
cargo clippy             # strict lints (unwrap/expect/panic denied)
cargo fmt -- --check     # formatting check
```

### Local Setup (without Docker)

```bash
# Start PostgreSQL
docker run -d --name solarix-db \
  -e POSTGRES_DB=solarix \
  -e POSTGRES_USER=solarix \
  -e POSTGRES_PASSWORD=solarix \
  -p 5432:5432 \
  postgres:16

# Set environment
export DATABASE_URL="postgres://solarix:solarix@localhost:5432/solarix"
export SOLANA_RPC_URL="https://api.devnet.solana.com"

# Run
cargo run
```

---

## Project Structure

```
src/
  main.rs               Entry point, signal handling, pipeline + API startup
  lib.rs                Public module declarations
  config.rs             22 env vars via clap, validation
  types.rs              DecodedInstruction, DecodedAccount, BlockData, TransactionData
  registry.rs           Two-phase program registration state machine

  idl/
    mod.rs              IdlManager: cache, parse, validate (v0.30+ only)
    fetch.rs            Fetch cascade: on-chain PDA -> bundled -> manual upload

  decoder/
    mod.rs              ChainparserDecoder: Borsh deserializer for 18+ IDL types

  pipeline/
    mod.rs              PipelineOrchestrator: 5-state machine, concurrent backfill+stream
    rpc.rs              RPC client with rate limiting (governor) and retry (backon)
    ws.rs               WebSocket logsSubscribe with dedup cache and heartbeat

  storage/
    mod.rs              DB pool init, system table bootstrap
    schema.rs           IDL -> CREATE TABLE/INDEX DDL, promoted column detection
    writer.rs           Batch INSERT...UNNEST, account upsert, checkpoint management
    queries.rs          Dynamic query builder for API filters

  api/
    mod.rs              axum Router, AppState, ApiError -> HTTP status mapping
    handlers.rs         12 endpoint handlers with pagination and cursor encoding
    filters.rs          Filter parsing, operator validation against IDL
```

**~12,850 lines of Rust** | **251 tests** | **15 source modules**

---

## Design Decisions

### Why runtime schema generation?

Competing indexers require codegen or predefined schemas. Solarix generates PostgreSQL DDL from Anchor IDLs at runtime, meaning you can add any program without touching code or restarting the service.

### Why hybrid typed + JSONB columns?

Simple scalar fields (u64, bool, Pubkey) are promoted to native PostgreSQL columns for fast indexed queries. Complex nested types go into a JSONB `data` column with GIN indexes. This balances query performance with schema flexibility.

### Why concurrent backfill + streaming?

On cold start, running backfill and streaming in parallel (Option C) means the indexer starts serving live data immediately while catching up on historical data. Both paths are idempotent (`INSERT ON CONFLICT DO NOTHING`), so duplicates are harmless.

### Why `thiserror` everywhere?

Five typed error enums (`IdlError`, `DecodeError`, `StorageError`, `PipelineError`, `ApiError`) with explicit classification (retryable / skip-and-log / fatal). No `anyhow`, no `unwrap`. The compiler enforces error handling at every boundary.

### Why rate limiting in the client?

Public Solana RPC endpoints are rate-limited to ~10 RPS. The `governor` crate provides async-native GCRA rate limiting, and `backon` handles exponential backoff with jitter. This prevents 429 errors and makes backfill reliable on free RPC endpoints.

---

## Key Dependencies

| Crate                  | Purpose                            |
| ---------------------- | ---------------------------------- |
| `axum` 0.8             | HTTP framework                     |
| `sqlx` 0.8             | Async PostgreSQL driver            |
| `tokio`                | Async runtime                      |
| `anchor-lang-idl-spec` | Anchor IDL type definitions        |
| `tokio-tungstenite`    | WebSocket client                   |
| `governor`             | RPC rate limiting                  |
| `backon`               | Retry with exponential backoff     |
| `thiserror`            | Typed error enums                  |
| `tracing`              | Structured logging (JSON + pretty) |
| `clap`                 | CLI/env configuration              |
| `sha2`                 | Discriminator computation          |
| `flate2`               | IDL zlib decompression             |

---

## License

MIT
