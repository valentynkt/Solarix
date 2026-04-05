# Project Structure & Boundaries

## Complete Project Directory Structure

```
solarix/
├── Cargo.toml                  -- single crate, dependencies, [lints] section
├── Cargo.lock
├── rustfmt.toml                -- formatting config (max_width=100)
├── clippy.toml                 -- clippy config (allow-expect-in-tests)
├── Dockerfile                  -- multi-stage: rust:latest build + debian-slim runtime
├── docker-compose.yml          -- postgres:16 + solarix binary
├── .dockerignore               -- exclude target/, .git/, tests/, docs/
├── .env.example                -- all 22 env vars documented
├── .gitignore
├── .github/
│   └── workflows/
│       └── ci.yml              -- 5 jobs: lint, unit, integration, coverage, docker smoke
├── README.md                   -- architecture diagrams, setup, API examples, decisions
├── idls/                       -- bundled IDL registry (70+ from AllenHark + curated)
│   ├── jupiter_v6.json
│   ├── raydium_clmm.json
│   ├── marinade.json
│   └── ...
├── src/
│   ├── main.rs                 -- clap parse, tokio::main, spawn pipeline + API server
│   ├── lib.rs                  -- pub mod declarations, crate-level doc comment
│   ├── config.rs               -- Config struct (#[derive(Parser)]), 22 env vars
│   ├── types.rs                -- shared types: DecodedInstruction, DecodedAccount, BlockData, etc.
│   ├── idl/
│   │   ├── mod.rs              -- IdlManager: cache (HashMap<ProgramId, ParsedIdl>), parse, version detect
│   │   └── fetch.rs            -- fetch cascade: on-chain PDA -> bundled idls/ dir -> manual upload
│   ├── decoder/
│   │   └── mod.rs              -- SolarixDecoder trait + ChainparserDecoder impl + DecodeError
│   ├── pipeline/
│   │   ├── mod.rs              -- PipelineOrchestrator: state machine, lifecycle, spawn readers
│   │   ├── rpc.rs              -- BlockSource + AccountSource traits + RpcBlockSource impl
│   │   └── ws.rs               -- TransactionStream trait + WsTransactionStream (logsSubscribe, reconnect)
│   ├── storage/
│   │   ├── mod.rs              -- DB pool init, system table bootstrap (programs, indexer_state)
│   │   ├── schema.rs           -- DDL generator: IDL -> CREATE SCHEMA/TABLE/INDEX, column promotion
│   │   ├── writer.rs           -- batch INSERT...UNNEST, account upsert, checkpoint, per-block atomic txn
│   │   └── queries.rs          -- dynamic QueryBuilder for API reads, filter operators -> SQL
│   └── api/
│       ├── mod.rs              -- axum Router, AppState, middleware (tracing layer)
│       ├── handlers.rs         -- 12 endpoint handlers (programs CRUD, instructions, accounts, stats, health)
│       └── filters.rs          -- query param parsing, operator validation against IDL field types
├── tests/
│   ├── fixtures/
│   │   ├── idls/               -- test IDL files (simple + complex programs)
│   │   └── transactions/       -- serialized test transactions for decode tests
│   ├── decode_roundtrip.rs     -- proptest: generate struct -> borsh::to_vec -> decode -> assert JSON
│   ├── schema_generation.rs    -- IDL -> DDL -> execute -> verify table structure
│   ├── pipeline_integration.rs -- LiteSVM: deploy program, send txs, verify indexed data
│   └── api_integration.rs      -- axum-test: register program, query endpoints, verify responses
```

## Architectural Boundaries

**Module Boundary Contracts (trait interfaces):**

| Boundary      | Trait               | Defined In        | Implemented In                         |
| ------------- | ------------------- | ----------------- | -------------------------------------- |
| Decode        | `SolarixDecoder`    | `decoder/mod.rs`  | `decoder/mod.rs` (ChainparserDecoder)  |
| Block fetch   | `BlockSource`       | `pipeline/rpc.rs` | `pipeline/rpc.rs` (RpcBlockSource)     |
| Account fetch | `AccountSource`     | `pipeline/rpc.rs` | `pipeline/rpc.rs` (RpcAccountSource)   |
| Tx stream     | `TransactionStream` | `pipeline/ws.rs`  | `pipeline/ws.rs` (WsTransactionStream) |

`AccountSource` covers FR17 (`getProgramAccounts` + `getMultipleAccounts`). It lives in `rpc.rs` alongside `BlockSource` since both use HTTP JSON-RPC. For MVP, `RpcAccountSource` may be collapsed into `RpcBlockSource` as additional methods; the trait boundary exists for testability.

Traits are the seams for testing — mock implementations replace real network/DB calls.

**Data Flow Through Boundaries:**

```
main.rs
  ├── Config::parse()
  ├── storage::init_pool() -> PgPool
  ├── storage::bootstrap_system_tables(&pool)
  ├── PipelineOrchestrator::new(config, pool, program_registry, decoder)
  │   ├── RpcBlockSource::new(config) ──── HTTP ────> Solana RPC
  │   ├── RpcAccountSource::new(config) ── HTTP ────> Solana RPC
  │   ├── WsTransactionStream::new(config) ── WS ──> Solana RPC
  │   ├── decoder.decode_instruction(data) ──> DecodedInstruction
  │   ├── decoder.decode_account(data) ──> DecodedAccount
  │   ├── writer::write_block(&pool, decoded) ──> PostgreSQL
  │   └── writer::update_checkpoint(&pool, state) ──> PostgreSQL
  └── api::router(pool, program_registry)
      ├── handlers -> queries::build_query() ──> PostgreSQL
      └── filters -> validate against IDL field types (via ProgramRegistry)
```

**Dependency graph (no circular deps):**

```
types  ←── idl ←── decoder ←── pipeline ←── storage
  ↑                   ↑                        ↑
  └───────────────────┼────── api ─────────────┘
                      └─────── (filter validation needs IDL)
```

Note: `api` depends on both `storage` (queries) and `idl` (filter validation against IDL field types). This does not create a cycle since `idl` has no dependency on `api` or `storage`.

**External Integration Points:**

- Solana RPC (HTTP): configurable via `SOLANA_RPC_URL`
- Solana RPC (WS): derived from HTTP URL or `SOLANA_WS_URL`
- PostgreSQL: via `DATABASE_URL`
- No other external services

## Requirements to Structure Mapping

| FR Category                 | Primary Module           | Files                                                                          |
| --------------------------- | ------------------------ | ------------------------------------------------------------------------------ |
| IDL Acquisition (FR1-3)     | `idl/`                   | `mod.rs`, `fetch.rs`                                                           |
| Dynamic Schema (FR4-9)      | `storage/`               | `schema.rs`, `mod.rs`                                                          |
| Decoding (FR10-12)          | `decoder/`               | `mod.rs`                                                                       |
| Batch Acquisition (FR13-17) | `pipeline/`              | `rpc.rs`, `mod.rs`                                                             |
| Real-Time (FR18-21)         | `pipeline/`              | `ws.rs`, `mod.rs`                                                              |
| Cold Start (FR22-25)        | `pipeline/` + `storage/` | `mod.rs`, `writer.rs`                                                          |
| REST API (FR26-33)          | `api/`                   | `handlers.rs`, `filters.rs`, `queries.rs`                                      |
| Reliability (FR34-38)       | Cross-cutting            | `pipeline/rpc.rs` (retry), `pipeline/mod.rs` (shutdown), all `mod.rs` (errors) |
| Deployment (FR39-41)        | Root                     | `Dockerfile`, `docker-compose.yml`, `.env.example`                             |

## Development Workflow

- **Local dev:** `cargo watch -x run` with `.env` pointing to local postgres
- **Test:** `cargo test` runs unit tests; `cargo test --tests` runs integration (requires PG)
- **Docker:** `docker compose up --build` for full stack verification
- **CI:** Push triggers 5 parallel jobs, Docker smoke test runs last
