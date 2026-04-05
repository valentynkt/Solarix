---
stepsCompleted: [1, 2, 3, 4]
inputDocuments: [brainstorming-session-2026-04-04-01.md]
session_topic: "Solarix - Universal Solana Indexer in Rust with architectural excellence as differentiator"
session_goals: "Rethink architecture decisions with Rust ecosystem leverage, find new differentiators beyond language choice, compress timeline via crate reuse, identify what makes this submission win among other Rust submissions"
selected_approach: "ai-recommended"
techniques_used:
  [
    "first-principles-thinking",
    "morphological-analysis",
    "constraint-mapping",
    "ecosystem-research",
  ]
ideas_generated:
  [
    RPC-thin,
    decode-sol-chainsaw,
    decode-fallback,
    schema-dynamic-ddl,
    pipeline-tokio-channels,
    api-axum,
    idl-fetch-from-chain,
    differentiator-runtime-dynamic,
    differentiator-zero-code,
    differentiator-thin-deps,
    differentiator-testing-depth,
    differentiator-readme-quality,
  ]
session_active: false
workflow_completed: true
context_file: "brainstorming-session-2026-04-04-01.md"
---

# Brainstorming Session Results — Rust Pivot

**Facilitator:** Valentyn
**Date:** 2026-04-05
**Predecessor:** brainstorming-session-2026-04-04-01.md (Zig session — architectural foundations carry forward)

## Session Overview

**Topic:** Architectural strategy for Solarix — Universal Solana Indexer in Rust, focused on winning through architectural elegance and code quality
**Goals:** Leverage Rust ecosystem to compress timeline, find differentiators that aren't language choice, make architectural decisions that beat other Rust submissions
**Portfolio Angle:** Solana Protocol / validator team roles — Rust is the native language, code quality IS the signal
**Bounty:** Superteam Ukraine - 1st: 500 USDG, 2nd: 450 USDG, 3rd: 250 USDG

### What Carries Forward (from Zig session)

- **Four-layer decomposition:** Read, Decode, Store, Serve — validated and complete
- **Read layer:** JSON-RPC over HTTP/WS, no Geyser, Geyser-aware abstraction
- **Decode layer:** Two-phase pipeline (structural + semantic), dynamic type system as critical path
- **Cold start:** Batch backfill until caught up, then real-time handoff
- **80/20 philosophy:** Build vs Defer split, judging criteria prioritization
- **Constraint map:** Hard walls, soft preferences, build path phases

### What Changes (Rust pivot)

- Differentiator strategy — language is no longer the hook
- Ecosystem leverage — massive crate ecosystem changes build-vs-build-from-scratch calculus
- Timeline — dramatically compressed, allowing more polish and testing
- Competition landscape — other submissions will also use Rust crates, must stand out differently

## Ecosystem Research (5 Parallel Research Agents)

### Agent 1: Solana Rust SDK Ecosystem

**Crate structure (current as of April 2026):**

- `solana-sdk` v3.0.0 — foundational types (Pubkey/Address, Transaction). Heavy dependency tree (50+ transitive deps).
- `solana-client` v3.1.8 — full-featured client. Very heavy.
- `solana-rpc-client` v3.1.6 — lighter RPC-only client.
- `solana-rpc-client-api` v3.0.1 — type definitions only, minimal deps (~10 transitive). Best for thin client approach.
- `solana-pubsub-client` v3.1.8 — WebSocket subscriptions. Actively maintained (weekly releases).
- `solana-transaction-status` — instruction parsing and decoding. 567+ dependents.

**Borsh crate (v1.5.7-1.6.0):**

- Static deserialization ONLY (derive macros for compile-time known types)
- NO dynamic/runtime deserialization support
- Confirms: dynamic decoder must be built or sourced elsewhere

**Anchor v1.0.0 (April 2026):**

- First stable release. Runtime IDL support via CLI.
- Client-centric, not indexer-centric. IDL parsing utilities not exposed programmatically.

**Key insight:** `solana-rpc-client-api` + custom `reqwest` client avoids bloated dependency tree. Signals deep understanding to validator team reviewers.

### Agent 2: Existing Solana Indexers

**Carbon framework (sevenlabs-hq/carbon):**

- Rust, modular, 3-layer pipeline: Datasources → Decoders → Processors
- 75+ pre-built decoders, CLI generates from Anchor/Codama IDLs
- **Compile-time codegen only** — no runtime IDL parsing
- Pre-V1, 63K downloads, actively maintained
- SPL token indexer in <150 lines

**sol-indexer (Jayant818/sol-indexer):**

- 4-stage pipeline: ingest → parse → buffer → persist/notify
- gRPC + RPC + file replay data sources
- PostgreSQL with sqlx, tokio async
- Dead letter queue error handling

**Key competitive insight:** No existing open-source Solana indexer does **runtime dynamic schema generation from arbitrary IDLs**. Carbon does IDL→static Rust types. Shyft does IDL→GraphQL but is proprietary. The bounty's core requirement is unserved in open source.

**Architectural patterns to adopt:**

- Gap-aware backfill with smart checkpointing
- Two-tier commitment (confirmed for streaming, finalized for trailing re-pulls)
- Observable by default (metrics + structured logging from day one)

### Agent 3: Anchor IDL Format Specification

**IDL Structure (v0.30+):**

- Top-level: `address`, `metadata`, `instructions`, `accounts`, `types`, `events`, `errors`
- 23+ type variants: primitives, signed/unsigned ints, vectors, arrays, options, coptions, tuples, hashmaps, btreemaps, sets, defined types with generics

**Discriminator system:**

- Instructions: `SHA256("global:<name>")[0..8]`
- Accounts: `SHA256("account:<name>")[0..8]`

**Edge cases for dynamic parser:**

- Generic types with type arguments
- Recursive/nested type resolution with cycle detection
- Enum variants (unit, tuple, struct)
- Tuple structs (unnamed fields)
- Events referencing account types instead of type definitions
- Legacy v0.29 vs v0.30+ format differences
- `option` vs `coption` (C-style)
- PDA seed constraints may be missing in some accounts

**Parser implementation checklist:** 14 items covering all type variants and edge cases.

### Agent 4: Rust Async Pipeline Patterns

**Pipeline architecture — SETTLED:**

- Tokio bounded mpsc channels between stages = automatic backpressure
- `tokio::spawn` for worker tasks — no actor framework needed
- Start with channel capacity 256, tune based on metrics

**Graceful shutdown — SETTLED:**

- `CancellationToken` from `tokio-util` + `tokio::select!` in each worker
- `tokio::signal::ctrl_c()` triggers cancellation
- Drain pending items before exit

**Resilience — SETTLED:**

- `backoff` crate for exponential backoff
- Tower ecosystem for retry/circuit breaker layers (optional, for DEFER)
- Error handling: each pipeline stage returns `Result`, logs with tracing, decides retry/skip/halt

**Structured logging — SETTLED:**

- `tracing` + `tracing-subscriber` with JSON formatter
- Spans for pipeline stage timing and context propagation

**Configuration — SETTLED:**

- `clap` v4 with `#[arg(env)]` for env var support
- `dotenvy` for .env file loading
- Hierarchy: CLI args > env vars > .env > defaults

### Agent 5: Dynamic DB Schema in Rust

**Database approach — SETTLED:**

- `sqlx` v0.8.6 with runtime queries (`sqlx::query()` not `sqlx::query!()`)
- PostgreSQL with hybrid schema: typed common columns + JSONB decoded payload + GIN indexes
- `QueryBuilder` for dynamic DML, raw SQL strings for DDL (CREATE TABLE)
- Refinery for migration tracking (optional — may be overkill for runtime DDL)

**JSONB performance:**

- GIN-indexed queries ~18ms vs 60ms without (3x speedup)
- `jsonb_ops` (default) or `jsonb_path_ops` (better perf, fewer operators)
- PostgreSQL 16+ has jsonpath-based indexing (20-35% faster)

**Connection pooling:** sqlx built-in pool or deadpool-postgres.

**Materialized views:** Viable for typed projections over JSONB. Auto-generate views per IDL type for typed query access.

### Agent 6: Carbon Deep Dive & Build-vs-Reuse Analysis

**Carbon as foundation — REJECTED:**

- Carbon's decoder system is compile-time only. Its Processor trait expects static InputType.
- Forcing runtime dynamic data through Carbon fights the framework.
- Carbon's datasources are mostly Yellowstone gRPC — overkill for bounty (Docker Compose required).
- Using Carbon as a dependency would add complexity without matching our "runtime dynamic" differentiator.

**sol-chainsaw (v0.0.2) — KEY DISCOVERY:**

- Runtime IDL parsing + dynamic Borsh deserialization
- `add_idl_json()` — load any IDL at runtime
- `JsonAccountsDeserializer` — discriminator → deserializer mapping
- Outputs to JSON dynamically
- Handles all 23 IDL type variants
- **Risk:** v0.0.2 is very early. May be buggy or incomplete. Abstract behind our own trait.

**Other useful crates:**

- `solana_toolbox_idl` — fetch IDL from chain by program address
- `solana_idl` — serde-compatible IDL type definitions

## Resolved Architecture Decisions

### All Layers — Final Stack

| Layer           | Decision                                         | Crates                                                     | Build ourselves                             |
| --------------- | ------------------------------------------------ | ---------------------------------------------------------- | ------------------------------------------- |
| **Read**        | Thin RPC client + WebSocket subs                 | `solana-rpc-client-api`, `reqwest`, `solana-pubsub-client` | RPC wrapper, subscription manager           |
| **Decode**      | Runtime dynamic via sol-chainsaw (with fallback) | `sol-chainsaw`, `solana-transaction-status`                | Value type wrapper, IDL edge case handling  |
| **Store**       | PostgreSQL hybrid (typed cols + JSONB + GIN)     | `sqlx`                                                     | Dynamic DDL generator (IDL → tables)        |
| **Serve**       | REST API with filters + aggregation              | `axum`                                                     | Routes, query builder, aggregation          |
| **Pipeline**    | Tokio bounded mpsc channels                      | `tokio`, `tokio-util`                                      | Pipeline orchestrator, backfill, cold start |
| **Reliability** | Backoff + retry + graceful shutdown              | `backoff`, `tokio-util` (CancellationToken)                | Drain logic, error handling                 |
| **Logging**     | Structured JSON                                  | `tracing`, `tracing-subscriber`                            | Span instrumentation                        |
| **Config**      | Env vars + CLI                                   | `clap`, `dotenvy`                                          | Config struct                               |
| **Deploy**      | Docker Compose                                   | —                                                          | Dockerfile, compose.yaml                    |

### Pipeline Architecture — Tokio Channels

```
                    ┌──────────┐
                    │  Config   │
                    │  + IDL    │
                    └────┬─────┘
                         │
   ┌─────────────────────▼─────────────────────┐
   │              Pipeline Orchestrator          │
   │  (backfill coordinator + mode switching)    │
   └──┬──────────────┬───────────────┬──────────┘
      │              │               │
      ▼              ▼               ▼
  ┌────────┐   ┌──────────┐   ┌──────────┐
  │ Reader │──▶│ Decoder  │──▶│  Storer  │
  │(RPC/WS)│   │(sol-chain│   │(sqlx/PG) │
  └────────┘   │ saw)     │   └──────────┘
   bounded     └──────────┘    bounded
   mpsc(256)    bounded        mpsc(256)
                mpsc(256)
```

Each stage runs in its own `tokio::spawn`. CancellationToken propagates shutdown.

### Differentiator Strategy

**Primary differentiator: "Give Solarix a program ID. It does everything else."**

- Fetch IDL from chain automatically
- Generate database schema at runtime
- Start indexing — no codegen, no recompile, no custom handlers
- REST API immediately available for queries

**This is what NO existing open-source indexer does.** Carbon needs compile-time codegen. Shyft is proprietary.

**Secondary differentiators:**

1. **Thin dependency philosophy** — `solana-rpc-client-api` + `reqwest` instead of full `solana-client`. Shows protocol understanding.
2. **Testing depth** — Property-based tests with `proptest` for Borsh decoder, integration tests against local validator. Most submissions will have minimal tests.
3. **README as architecture document** — Mermaid diagrams, decision records with trade-off analysis, benchmark results. This is judge-facing and explicitly scored.
4. **Operational polish** — Health endpoint with pipeline status/lag, structured JSON logging, graceful shutdown. Production-ready feel.
5. **Two-phase decode** — Structural indexing for any program + semantic indexing with IDL. Works even without IDL.

### Competition Differentiation Matrix

| Feature           | Typical submission     | Carbon-based             | Solarix                                   |
| ----------------- | ---------------------- | ------------------------ | ----------------------------------------- |
| IDL handling      | Hardcoded per program  | Compile-time codegen     | **Runtime dynamic**                       |
| Schema            | Manual migration files | Manual or codegen        | **Auto-generated from IDL**               |
| Add new program   | Recompile + redeploy   | Recompile + redeploy     | **API call — zero downtime**              |
| API               | Basic REST routes      | Build your own Processor | **Auto-discovery of registered programs** |
| Dependency weight | Full solana-sdk        | Carbon + solana-sdk      | **Thin: rpc-client-api + reqwest**        |
| Tests             | Minimal or none        | Framework tests          | **Property-based + integration**          |

## Build vs Reuse — Final Boundary

### REUSE (existing crates)

| Crate                            | Purpose                                    | Saves                 |
| -------------------------------- | ------------------------------------------ | --------------------- |
| `sol-chainsaw`                   | Runtime IDL loading + dynamic Borsh decode | Weeks of decoder work |
| `solana_toolbox_idl`             | Fetch IDL from chain by program address    | IDL discovery logic   |
| `solana-rpc-client-api`          | RPC type definitions (lightweight)         | Type definitions      |
| `solana-pubsub-client`           | WebSocket subscriptions                    | WS client             |
| `solana-transaction-status`      | Transaction/instruction parsing            | Tx decoding           |
| `reqwest`                        | HTTP client for RPC calls                  | HTTP plumbing         |
| `sqlx`                           | Async PostgreSQL with runtime queries      | DB driver + pool      |
| `axum`                           | HTTP server for API                        | Web framework         |
| `tracing` + `tracing-subscriber` | Structured JSON logging                    | Logging infra         |
| `clap` + `dotenvy`               | Config from env/CLI/files                  | Config system         |
| `tokio` + `tokio-util`           | Async runtime + CancellationToken          | Pipeline + shutdown   |
| `backoff`                        | Exponential backoff for RPC retries        | Retry logic           |
| `serde` + `serde_json`           | Serialization                              | JSON handling         |

### BUILD (our unique code)

1. **Dynamic schema generator** — IDL types → PostgreSQL DDL (CREATE TABLE with appropriate column types, GIN indexes on JSONB, optional materialized views). This is the core differentiator.
2. **Pipeline orchestrator** — Batch backfill (slot range chunking) → progress tracking → catch-up detection → seamless handoff to real-time WebSocket subscriber. Gap-aware with smart checkpointing.
3. **API layer** — axum routes for: program registration (POST IDL or program ID), multi-param filtering, time-range aggregation (instruction call counts), basic program statistics, health endpoint.
4. **Program registration flow** — Accept program ID → fetch IDL from chain via `solana_toolbox_idl` → parse with `sol-chainsaw` → generate schema → start indexing pipeline. Zero-code onboarding.
5. **sol-chainsaw abstraction** — Wrap behind our own trait so we can replace if the crate is too immature. Fallback: build our own dynamic Borsh decoder using the IDL spec mapped in research.
6. **Two-phase decode** — Phase 1: structural (slot, signature, program_id, accounts — works without IDL). Phase 2: semantic (instruction args, account state — requires IDL).

### DEFER (mention in README as future work)

- Geyser plugin support (with architecture note showing the abstraction is ready)
- GraphQL API
- Circuit breaker pattern (Tower layers)
- Event sourcing / re-decode on IDL change
- Parallel backfill workers
- Prometheus metrics endpoint
- Auto-generated API endpoints per IDL instruction
- WebSocket push notifications for real-time subscribers

## Remaining Research Spikes

| Priority | Spike                                                                                            | Effort    |
| -------- | ------------------------------------------------------------------------------------------------ | --------- |
| P0       | **sol-chainsaw evaluation** — clone, test against real Anchor IDLs, check edge cases             | Half day  |
| P1       | **IDL → DDL mapping** — design the type mapping (IDL types → PostgreSQL column types)            | 1 session |
| P2       | **Backfill strategy benchmarking** — how fast can we pull historical slots via RPC? Rate limits? | 1 session |

All other original research spikes are now **resolved** by ecosystem research.

## Recommended Build Path

**Phase 1 — Foundation (Week 1):**

- Project scaffolding (Cargo workspace, Docker setup)
- Config system (clap + dotenvy)
- sol-chainsaw evaluation + abstraction trait
- IDL parser integration (load IDL, parse types)
- Dynamic DDL generator prototype (IDL → CREATE TABLE)

**Phase 2 — Ingestion (Week 1-2):**

- Thin RPC client wrapper over `reqwest` + `solana-rpc-client-api`
- Batch mode indexer (fetch blocks in slot range)
- Pipeline: Reader → Decoder → Storer with tokio channels
- PostgreSQL write path (insert decoded data into dynamic tables)

**Phase 3 — Real-time (Week 2):**

- WebSocket subscriber via `solana-pubsub-client`
- Cold start: detect last processed slot → backfill gap → switch to real-time
- Seamless handoff logic

**Phase 4 — API (Week 2-3):**

- axum HTTP server
- Program registration endpoint (POST program ID → auto-setup)
- Query endpoints with multi-param filters
- Aggregation (instruction counts over time period)
- Basic statistics per program
- Health endpoint

**Phase 5 — Hardening (Week 3):**

- Exponential backoff + retry on RPC failures
- Graceful shutdown (CancellationToken → drain → flush → exit)
- Structured JSON logging with tracing spans per pipeline stage
- Error handling audit

**Phase 6 — Testing (Week 3):**

- Unit tests for DDL generator
- Property-based tests for Borsh decode path (if custom decoder)
- Integration test against solana-test-validator
- API endpoint tests

**Phase 7 — Polish (Week 3-4):**

- README with: architectural overview (Mermaid diagrams), setup instructions, API query examples, decision records with trade-offs
- Docker Compose (PostgreSQL + Solarix binary)
- Example API queries in README
- Twitter thread drafting (build narrative along the way)

## Session Summary

### Key Achievements

- Pivoted from Zig to Rust based on bounty rule analysis (disqualification risk)
- Conducted 5 parallel ecosystem research spikes with current data (not training cutoff)
- Discovered `sol-chainsaw` — runtime dynamic Borsh decoder that could save weeks
- Evaluated and rejected Carbon as foundation (compile-time only, fights our differentiator)
- Resolved ALL original research spikes from Zig session
- Defined complete crate stack (13 dependencies, each chosen with rationale)
- Identified clear differentiator: "Give it a program ID, it does everything else — no codegen, no recompile"
- Compressed build path to ~3-4 weeks with 80/20 scope

### Differentiator Summary

**What makes Solarix win:** It's the only open-source Solana indexer that dynamically generates database schemas and starts indexing from an arbitrary Anchor IDL at runtime with zero code. Every other solution requires compile-time codegen or is proprietary.

### Risk Register

| Risk                                     | Mitigation                                                                |
| ---------------------------------------- | ------------------------------------------------------------------------- |
| sol-chainsaw too immature (v0.0.2)       | Abstract behind trait, build own decoder as fallback                      |
| RPC rate limits slow backfill            | Implement smart batching, respect rate limits, document in README         |
| Dynamic DDL generation edge cases        | Start with common types, document unsupported cases                       |
| Scope creep beyond 80/20                 | DEFER list is explicit, stick to BUILD list                               |
| Other submissions also do dynamic schema | Unlikely — no existing OSS does this. If so, win on code quality + README |

### Next Steps

1. **Evaluate sol-chainsaw** — clone, test against pump.fun IDL and other real programs
2. **Start `/bmad-product-brief`** or skip to **`/bmad-create-prd`** — we have enough context
3. **Start `/bmad-technical-research`** for the IDL→DDL type mapping design
4. Begin project scaffolding (Cargo workspace)
