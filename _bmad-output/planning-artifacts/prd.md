---
stepsCompleted:
  - step-01-init
  - step-02-discovery
  - step-02b-vision
  - step-02c-executive-summary
  - step-03-success
  - step-04-journeys
  - step-05-domain
  - step-06-innovation
  - step-07-project-type
  - step-08-scoping
  - step-09-functional
  - step-10-nonfunctional
  - step-11-polish
  - step-12-complete
inputDocuments:
  - bounty-requirements.md
  - brainstorming-session-2026-04-05-rust.md
  - technical-solarix-universal-solana-indexer-research-2026-04-05.md
  - anchor-idl-type-spec-borsh-wire-format.md
  - agent-1d-solana-rpc-capabilities.md
  - agent-1e-custom-borsh-decoder-feasibility.md
  - agent-2a-idl-to-ddl-mapping.md
  - agent-2b-hybrid-storage-architecture.md
  - agent-2c-backfill-pipeline-cold-start.md
  - agent-2d-dynamic-rest-api-design.md
  - agent-2e-decode-paths-testing-strategy.md
  - on-chain-idl-availability.md
workflowType: "prd"
documentCounts:
  briefs: 0
  research: 10
  brainstorming: 1
  projectDocs: 0
classification:
  projectType: developer_tool
  domain: blockchain_infrastructure
  complexity: high
  projectContext: greenfield
---

# Product Requirements Document - Solarix

**Author:** Valentyn
**Date:** 2026-04-05

## Executive Summary

Solarix is a universal Solana indexer that dynamically generates database schemas and decodes on-chain data from any Anchor IDL at runtime. Built in Rust for the Superteam Ukraine bounty, it serves as a technical portfolio project demonstrating senior-level systems engineering for Solana Protocol and validator team roles.

The core problem: every existing open-source Solana indexer requires compile-time codegen or program-specific handlers to index new programs. Adding a new program means writing code, recompiling, and redeploying. Solarix eliminates this entirely. Give it a program ID, and it fetches the IDL from chain, generates PostgreSQL tables, starts indexing transactions and account states, and exposes a query API -- all at runtime, with zero code.

**Target users:** Bounty judges evaluating technical excellence, and secondarily Solana developers who need quick program indexing without custom code.

**Context:** This is not a startup product. It is a focused technical demonstration optimized for bounty judging criteria, leveraging 80/20 principles to maximize impact per effort. Every architectural decision is made to impress judges on code quality, clean architecture, testing depth, and README clarity.

### What Makes This Special

Solarix is the only open-source Solana indexer that performs runtime dynamic schema generation from arbitrary Anchor IDLs. The competitive landscape:

| Capability          | Typical Submission     | Carbon Framework     | Solarix                        |
| ------------------- | ---------------------- | -------------------- | ------------------------------ |
| IDL handling        | Hardcoded per program  | Compile-time codegen | Runtime dynamic                |
| Schema generation   | Manual migration files | Manual or codegen    | Auto-generated from IDL        |
| Add new program     | Recompile + redeploy   | Recompile + redeploy | API call -- zero downtime      |
| Dependency approach | Full solana-sdk        | Carbon + solana-sdk  | Thin: rpc-client-api + reqwest |

The core insight: `chainparser` v0.3.0 provides a runtime IDL-to-JSON deserialization engine covering 26 Borsh type variants. Forking it (3-5 days) to upgrade solana-sdk and add instruction arg decoding gives Solarix a battle-tested decode engine without building from scratch. Combined with PostgreSQL's hybrid typed+JSONB storage and axum's catch-all routing, the entire stack supports runtime dynamism end-to-end.

### Concurrent Backfill-Streaming Handoff

The cold start runs both backfill and streaming concurrently (Option C). Both paths write to the same tables with `INSERT ON CONFLICT DO NOTHING`. The ~1,500 duplicate inserts during the overlap window cost <100ms of DB overhead. No application-level buffer management. Crash-safe because both paths are independently idempotent.

## Project Classification

- **Project Type:** Developer tool / Backend service (Rust CLI binary + REST API)
- **Domain:** Blockchain infrastructure (Solana ecosystem)
- **Complexity:** High -- runtime Borsh decoding, dynamic DDL generation, WebSocket reliability, multi-mode pipeline orchestration
- **Project Context:** Greenfield -- new project, no existing codebase

## Success Criteria

### Bounty Success (Primary)

Success means winning 1st place (500 USDG) in the Superteam Ukraine bounty. The six judging criteria, in priority order:

1. **Dynamic schema generation and account decoding** -- Schema auto-generated from any Anchor IDL, both instruction args and account states decoded
2. **Real-time mode with cold start** -- WebSocket subscription with seamless backfill-to-streaming handoff, gap-aware restart
3. **Reliability features** -- Exponential backoff, retry with jitter, graceful shutdown
4. **Advanced API** -- Multi-parameter filtering, time-range aggregation, program statistics
5. **Code quality and architecture** -- Clean module boundaries (Cargo workspace), idiomatic Rust, comprehensive tests on core modules
6. **README completeness** -- Mermaid architecture diagrams, decision records with trade-off analysis, copy-pasteable setup, curl-able API examples

### Technical Excellence (Portfolio)

- Code reads as if written by a senior Rust engineer on a Solana validator team
- Architecture signals deep protocol understanding (thin deps, trait abstractions, standard RPC only)
- Property-based tests (proptest) for decoder prove correctness, not just coverage
- Proper error types with context via `thiserror`; `clippy::unwrap_used` denied in CI

### Measurable Outcomes

- `docker compose up` starts the full stack in under 60 seconds
- Indexing 1,000 slots completes in under 2 minutes on public RPC (10 RPS)
- API query latency under 100ms for single-entity lookups
- Cold start detects gap and resumes from last checkpoint without data loss
- All bounty core requirements demonstrably functional with a real Anchor program (e.g., Jupiter v6, Marinade)

## Deliverables

- Public GitHub repository with comprehensive README
- README includes: architectural overview (Mermaid diagram), setup instructions (copy-pasteable, verified in CI), API query examples (curl commands), explanation of architectural decisions and trade-offs
- `.env.example` with all configurable variables documented
- Docker Compose that starts the full stack with `docker compose up`
- Twitter thread detailing the build experience, technical challenges, and trade-offs
- All content in English

## Product Scope

### MVP - Bounty Submission (Phase 1)

Everything required to meet all six judging criteria:

- Dynamic schema generation from Anchor IDL (v0.30+ format)
- Dual decode paths: instruction args + account state
- Batch mode: slot range and signature list
- Real-time mode: WebSocket subscription via `logsSubscribe`
- Cold start with gap detection and seamless handoff
- Exponential backoff with jitter, retry mechanism, graceful shutdown
- REST API with multi-parameter filtering, aggregation, statistics
- Docker Compose (PostgreSQL + Solarix binary, single `docker compose up`)
- Configuration via environment variables with sensible defaults
- Structured JSON logging via `tracing`
- Comprehensive README (architecture diagrams, decision records, API examples)
- Unit tests, property-based tests (proptest), integration tests

### Post-MVP (README "Future Work")

- Anchor v1.0 PMP IDL fetch (on-chain via Program Metadata Program)
- Legacy v0.29 IDL format support
- CPI (inner) instruction decoding with depth tracking
- Geyser/Yellowstone gRPC data source (trait abstraction already in place)
- GraphQL API layer
- Prometheus metrics endpoint
- Schema evolution (additive ALTER TABLE on IDL changes)
- Nested JSONB field filtering via dot-path notation

## User Journeys

### Journey 1: Bounty Judge - First Evaluation

**Marcus**, a senior Solana developer and Superteam Ukraine bounty judge, opens the Solarix GitHub repo. He reads the README, scans the architecture diagram, and notices the "pluggable data source" trait abstraction. He runs `docker compose up`, waits for the health check to pass, then registers Jupiter v6 via `POST /api/programs`. The indexer fetches the IDL from chain, generates tables, and starts backfilling. Marcus queries `GET /api/programs/{id}/instructions/swap` with filters -- results come back in 42ms. He inspects the generated schema in PostgreSQL and sees cleanly mapped columns with JSONB payloads. He opens `src/` and sees a well-organized Cargo workspace with clear module boundaries. He runs `cargo test` -- all pass, including property-based decode tests. Marcus thinks: "This person understands Solana internals."

**Capabilities revealed:** Program registration, IDL auto-fetch, schema generation, batch indexing, API querying, test suite, README documentation, Docker setup.

### Journey 2: Judge - Edge Case Testing

**Marcus** tries to break Solarix. He registers a program with no on-chain IDL -- the indexer returns a clear error: "IDL not found. Provide via `--idl-path` or `POST /api/programs` with IDL body." He uploads a manual IDL JSON. He kills the process mid-backfill with `SIGTERM`, restarts it, and confirms the cold start detects the gap and resumes from the last checkpoint. He checks the graceful shutdown sequence in logs -- reader stop, pipeline drain, DB flush + checkpoint update. No data corruption.

**Capabilities revealed:** IDL fallback cascade, error messaging, manual IDL upload, graceful shutdown, cold start recovery, rate limiting, structured logging.

### Journey Requirements Summary

| Capability Area                              | Journeys | Priority |
| -------------------------------------------- | -------- | -------- |
| IDL acquisition (auto-fetch + manual upload) | 1, 2     | P0       |
| Dynamic schema generation                    | 1        | P0       |
| Dual decode (instructions + accounts)        | 1        | P0       |
| Batch mode (slot range + signatures)         | 1        | P0       |
| Real-time mode (WebSocket)                   | 1        | P0       |
| Cold start with gap detection                | 2        | P0       |
| REST API with filters + aggregation          | 1        | P0       |
| Graceful shutdown                            | 2        | P0       |
| Rate limiting + retry                        | 2        | P0       |
| Health endpoint + observability              | 1, 2     | P0       |
| Docker Compose deployment                    | 1        | P0       |
| Configuration via env vars                   | 2        | P0       |
| Error messaging and fallback UX              | 2        | P1       |

## Domain-Specific Requirements

### Solana Blockchain Constraints

- **Transaction versions:** Must set `maxSupportedTransactionVersion: 0` on all RPC calls or v0 transactions are silently dropped
- **Discriminator system:** Instructions use `SHA-256("global:<snake_case>")[0..8]`, accounts use `SHA-256("account:<PascalCase>")[0..8]` -- both 8-byte prefixes
- **COption vs Option:** COption uses 4-byte u32 tag with fixed-size allocation; Option uses 1-byte tag with conditional payload. Decoder must dispatch differently.
- **IDL format:** Solarix targets v0.30+ (current standard, uses `metadata.spec` field). Legacy v0.29 support deferred to post-MVP.
- **Anchor v1.0 (April 2, 2026):** Legacy IDL instructions removed, replaced by Program Metadata Program. Existing on-chain IDL accounts still readable. PMP support deferred to post-MVP.

### RPC Constraints

- **Public RPC rate limit:** ~10 RPS. All backfill and streaming must respect this. Design with adaptive rate limiting.
- **WebSocket reliability:** No delivery/ordering/exactly-once guarantees. Requires automatic reconnection, gap detection, and signature-based deduplication.
- **Block data:** 6-16 MB uncompressed JSON per block. Use `base64` encoding for bandwidth efficiency.
- **`getProgramAccounts`:** No pagination. Use `dataSlice: {offset: 0, length: 0}` for pubkey-only fetch, then batch with `getMultipleAccounts` (max 100).
- **`logsSubscribe`:** Supports exactly 1 program filter. Returns signature + logs, not full transaction data -- follow up with `getTransaction`.

### Data Integrity

- No data corruption on crash, network failure, or RPC timeout mid-batch
- Per-block atomic writes (entire block committed or nothing)
- Account upserts guard against stale overwrites: `WHERE EXCLUDED.slot_updated > table.slot_updated`
- Signature-based deduplication via `INSERT ON CONFLICT DO NOTHING`

## Backend Service Specific Requirements

### Architecture Overview

Four-layer pipeline: **Read -> Decode -> Store -> Serve**, connected by bounded Tokio mpsc channels (capacity 256).

```
Read Layer (HTTP + WebSocket)
    |  bounded mpsc(256)
    v
Decode Layer (chainparser fork, SolarixDecoder trait)
    |  bounded mpsc(256)
    v
Store Layer (sqlx -> PostgreSQL, hybrid typed+JSONB)
    |
    v
Serve Layer (axum REST API, catch-all parametric routes)
```

### Pipeline State Machine

```
Initializing -> Backfilling <-> CatchingUp -> Streaming -> ShuttingDown
```

- **Initializing:** DB connect, load checkpoint, compute gap
- **Backfilling:** Chunk by 50K slots, parallel `getBlock`, filter for target program
- **Streaming:** `logsSubscribe` -> `getTransaction` per signature
- **CatchingUp:** Mini-backfill on WebSocket disconnect, then resume streaming
- **ShuttingDown:** 4-phase drain (reader stop, pipeline drain, DB flush + checkpoint, cleanup)

### API Surface (12 Endpoints)

| Method   | Path                                           | Purpose                                |
| -------- | ---------------------------------------------- | -------------------------------------- |
| `POST`   | `/api/programs`                                | Register program (by ID or IDL upload) |
| `GET`    | `/api/programs`                                | List registered programs               |
| `GET`    | `/api/programs/{id}`                           | Program info + indexing status         |
| `DELETE` | `/api/programs/{id}`                           | Deregister program                     |
| `GET`    | `/api/programs/{id}/instructions`              | List instruction types                 |
| `GET`    | `/api/programs/{id}/instructions/{name}`       | Query instructions (with filters)      |
| `GET`    | `/api/programs/{id}/accounts`                  | List account types                     |
| `GET`    | `/api/programs/{id}/accounts/{type}`           | Query accounts by type                 |
| `GET`    | `/api/programs/{id}/accounts/{type}/{pubkey}`  | Get specific account                   |
| `GET`    | `/api/programs/{id}/stats`                     | Program statistics                     |
| `GET`    | `/api/programs/{id}/instructions/{name}/count` | Instruction count over time            |
| `GET`    | `/health`                                      | Pipeline status + lag                  |

**Filter operators:** `_gt`, `_gte`, `_lt`, `_lte`, `_eq`, `_ne`, `_contains`, `_in`

**Pagination:** Cursor-based (keyset on `(slot, signature)`) for instructions; offset for accounts.

### Technology Stack

| Layer      | Crate                                | Purpose                              |
| ---------- | ------------------------------------ | ------------------------------------ |
| Decode     | `chainparser` (forked v0.3.0)        | Runtime IDL decode, 26 type variants |
| IDL Types  | `anchor-lang-idl-spec`               | Official Rust IDL type definitions   |
| IDL Fetch  | `reqwest` + zlib                     | On-chain PDA fetch + manual upload   |
| RPC (HTTP) | `solana-rpc-client-api` + `reqwest`  | Thin deps, no vendor lock-in         |
| RPC (WS)   | `solana-pubsub-client`               | WebSocket subscriptions              |
| Storage    | `sqlx`                               | PostgreSQL with runtime queries      |
| API        | `axum`                               | Catch-all parametric routes          |
| Pipeline   | `tokio` + `tokio-util`               | Bounded mpsc, CancellationToken      |
| Rate Limit | `governor`                           | GCRA, async-native                   |
| Retry      | `backoff`                            | Exponential with jitter              |
| Logging    | `tracing` + `tracing-subscriber`     | Structured JSON, spans per stage     |
| Config     | `clap` + `dotenvy`                   | Env vars + CLI                       |
| Testing    | `proptest` + `litesvm` + `axum-test` | Property + integration + API tests   |

### Database Schema

```
solarix (database)
+-- public (system)
|   +-- programs          -- registered program registry
|   +-- indexer_state     -- per-program checkpoint + status
|
+-- "{program_name}" (per-program schema)
    +-- _metadata         -- IDL hash, program info
    +-- _instructions     -- unified instruction table (JSONB args)
    +-- {account_type_1}  -- per-type table (promoted cols + JSONB data)
    +-- {account_type_2}
    +-- ...
```

Schema-per-program isolation. No name collisions. Easy cleanup via `DROP SCHEMA CASCADE`. All dynamic SQL uses parameterized queries; table/column names derived from IDL, not user input.

## Risk Mitigation

- **chainparser fork breaks on solana-sdk v3:** Actual sdk usage is minimal (Pubkey, Account). Worst case: vendor 3 types. Plan B: custom decoder (4.5 days, ~960 LOC).
- **On-chain IDL not found for judge's test program:** On-chain fetch + manual upload fallback with clear error message. Document in README.
- **WebSocket drops messages:** Option C concurrent dedup + gap detection on reconnect + INSERT ON CONFLICT DO NOTHING.
- **Public RPC rate limits slow demo:** Adaptive `governor` rate limiter, small demo ranges, document free-tier providers in README.
- **Dynamic DDL edge cases:** Start with common types, JSONB safety net always has full decoded payload. Log warnings for unsupported types.

## Functional Requirements

### IDL Acquisition & Management

- FR1: System can fetch Anchor IDL from on-chain PDA given a program ID
- FR2: User can upload IDL manually via file path or API endpoint
- FR3: System can store IDL metadata (hash, source, program name) for reference

### Dynamic Schema Generation

- FR4: System can generate PostgreSQL schema from any v0.30+ Anchor IDL at runtime
- FR5: System can create per-program database schema with namespace isolation
- FR6: System can map IDL type variants (23 official + 6 unofficial) to appropriate PostgreSQL column types
- FR7: System can promote top-level scalar fields to native typed columns
- FR8: System can store full decoded payload in JSONB `data` column as safety net
- FR9: System can create indexes (B-tree on common columns, GIN `jsonb_path_ops` on JSONB)

### Transaction & Account Decoding

- FR10: System can decode instruction arguments from transaction data using IDL
- FR11: System can decode account state data using IDL and discriminator matching
- FR12: System can identify unknown discriminators and log warning without crashing

### Data Acquisition

- FR13: User can index transactions within a specified slot range (batch mode)
- FR14: User can index transactions from a list of signatures (batch mode)
- FR15: System can chunk large slot ranges for rate-limit-safe processing
- FR16: System can filter block transactions for target program
- FR17: System can fetch current account states via `getProgramAccounts` and `getMultipleAccounts`

### Real-Time Indexing

- FR18: System can subscribe to new transactions for a program via WebSocket
- FR19: System can automatically reconnect on WebSocket disconnect with backoff
- FR20: System can detect gaps after reconnection and mini-backfill missed slots
- FR21: System can deduplicate transactions across backfill and streaming paths

### Cold Start & Checkpointing

- FR22: System can persist indexing checkpoint (last processed slot, status) to database
- FR23: System can detect gap between last checkpoint and current chain tip on startup
- FR24: System can resume backfill from last checkpoint without reprocessing
- FR25: System can transition seamlessly from backfill to real-time streaming

### REST API

- FR26: User can register a program for indexing via API (program ID or IDL upload)
- FR27: User can list registered programs and their indexing status
- FR28: User can query instructions by type with multi-parameter filters
- FR29: User can query account states by type with multi-parameter filters
- FR30: User can retrieve a specific account by pubkey
- FR31: User can get instruction call counts over a time period (aggregation)
- FR32: User can get basic program statistics (total transactions, accounts, etc.)
- FR33: User can paginate query results

### Reliability & Operations

- FR34: System can retry failed RPC requests with exponential backoff and jitter
- FR35: System can perform graceful shutdown preserving checkpoint state
- FR36: System can emit structured JSON logs with per-stage tracing spans
- FR37: Operator can check system health via health endpoint (pipeline status, lag, DB health)
- FR38: Operator can configure operational parameters via environment variables

### Deployment

- FR39: System can start with all dependencies via single `docker compose up` command
- FR40: System can auto-create database schema on first start (self-bootstrapping)
- FR41: Repository includes `.env.example` with all configurable variables documented

## Non-Functional Requirements

### Performance

- API response time: <100ms for single-entity lookups, <500ms for filtered queries
- Backfill throughput not bottlenecked by pipeline (RPC rate limit is the constraint at 10 RPS)
- Streaming mode indexes transactions within seconds of confirmation

### Reliability

- Zero data loss on crash: per-block atomic writes, checkpoint updates after each chunk
- Exponential backoff with jitter on RPC failures
- WebSocket automatic reconnection with gap backfill before resuming
- Graceful shutdown: drain pipeline, flush to DB, persist checkpoint

### Security

- All dynamic SQL uses parameterized queries; table/column names derived from IDL, not user input
- No secrets in Docker image or repository (all via environment variables)

### Code Quality

- Comprehensive tests on core modules: decoder (proptest roundtrip), schema generator (unit), API (axum-test)
- Integration tests against LiteSVM local validator for end-to-end verification
- CI pipeline: lint (`clippy`, `fmt`), test, Docker smoke test
- Clean Cargo workspace with logical module boundaries
- `cargo clippy` clean (including `clippy::unwrap_used` denial), `cargo fmt` enforced
