# Project Context Analysis

## Requirements Overview

**Functional Requirements (41 total):**

| Category                       | FRs     | Architectural Impact                                    |
| ------------------------------ | ------- | ------------------------------------------------------- |
| IDL Acquisition & Management   | FR1-3   | Multi-tier fetch cascade, IDL storage/versioning        |
| Dynamic Schema Generation      | FR4-9   | Runtime DDL engine, type mapping system, index strategy |
| Transaction & Account Decoding | FR10-12 | Shared decode engine with dual dispatch paths           |
| Data Acquisition (Batch)       | FR13-17 | Chunked slot fetching, rate-limited RPC client          |
| Real-Time Indexing             | FR18-21 | WebSocket lifecycle, reconnection, gap detection, dedup |
| Cold Start & Checkpointing     | FR22-25 | Persistent state machine, concurrent backfill+stream    |
| REST API                       | FR26-33 | Dynamic routing, IDL-validated filters, query builder   |
| Reliability & Operations       | FR34-38 | Backoff/retry, graceful shutdown, tracing, health       |
| Deployment                     | FR39-41 | Docker Compose, self-bootstrapping, env var config      |

**Non-Functional Requirements:**

- Performance: <100ms single-entity lookups, <500ms filtered queries, streaming latency best-effort (~1-3s, constrained by RPC round-trip for logsSubscribe + getTransaction)
- Reliability: Zero data loss on crash (per-block atomic writes, checkpointing), WebSocket auto-reconnect with gap backfill
- Security: Parameterized queries only; table/column names from IDL not user input; no secrets in image
- Code Quality: proptest roundtrips for decoder, clippy::unwrap_used denied, >80% coverage on core modules (decoder, storage, pipeline)
- Deployment: Single `docker compose up`, self-bootstrapping DB schema, env var configuration

**Scale & Complexity:**

- Primary domain: Backend service / blockchain infrastructure
- Complexity level: High
- Estimated architectural components: 7 (IDL manager, decoder, schema generator, pipeline orchestrator, storage layer, API server, configuration/observability)

## Architectural Philosophy: Developer Velocity First

The architecture must optimize for **high developer velocity, ease of development, and extensibility** — applying 80/20 principles throughout:

- **Simplicity over cleverness**: Prefer straightforward patterns (bounded mpsc channels, trait objects) over complex abstractions (actor systems, event sourcing)
- **Flat module structure**: Minimize indirection layers. A developer should trace data flow from RPC to DB in one reading pass
- **Convention over configuration**: Sensible defaults everywhere, override via env vars only when needed
- **Crate leverage**: Use battle-tested crates for commodity concerns (HTTP, DB, logging), build only the unique parts (IDL-to-DDL, pipeline orchestration)
- **Additive-only evolution (MVP scope)**: New programs, new account types, new IDL fields — all additive. IDL type changes on existing fields are not supported in MVP (JSONB `data` column preserves full current payload regardless). Schema versioning deferred to post-MVP.
- **Test-friendly design**: Trait abstractions at boundaries (decoder, block source, transaction stream, account source) enable unit testing without network or DB

## Technical Constraints & Dependencies

- **Hard wall**: Standard Solana RPC only — no Geyser, no vendor-specific APIs
- **Hard wall**: Docker Compose single-command start with no pre-setup
- **Hard wall**: v0.30+ Anchor IDL format (legacy v0.29 deferred)
- **Critical dependency**: chainparser v0.3.0 fork — 3 known gaps, bounded fix effort. Repo is dormant (7 commits, last activity Sep 2024). Fork risk is high — custom decoder fallback is the insurance policy.
- **Rate limit ceiling**: Public RPC ~10 RPS shapes all backfill/batch design
- **WebSocket guarantees**: None (no ordering, no delivery, no exactly-once) — all reliability is application-layer

## Cross-Cutting Concerns Identified

- **Error classification**: Retryable (429, timeout) / skip-and-log (unknown discriminator) / fatal (DB down) — used by pipeline, RPC client, decoder, and storer
- **Decode failure detection**: Skip-and-log per-tx is correct, but if >90% of transactions in a chunk fail decode, log at `error!` level — likely indicates IDL version mismatch
- **Rate limiting & backpressure**: Affects batch reader, account fetcher, cold-start gap fill. Bounded mpsc(256) channels provide automatic backpressure across all pipeline stages.
- **Checkpoint persistence**: Touches pipeline orchestrator, storer, and cold start. Two-tier design: `indexer_state` (global pipeline status) + per-program `_checkpoints` (slot cursors). See [Checkpoint Architecture](#checkpoint-architecture).
- **Graceful shutdown**: CancellationToken propagation across all pipeline stages (reader, decoder, storer, API server)
- **Structured tracing**: Spans per pipeline stage, per-block context, per-request API timing
- **IDL context propagation**: IDL loaded once, shared via `ProgramRegistry` across decoder + schema generator + API filter validator. See [Shared State](#shared-state).
