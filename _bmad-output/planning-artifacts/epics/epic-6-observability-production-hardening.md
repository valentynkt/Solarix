# Epic 6: Observability, Production Hardening & Test Coverage

> **Brutal refinement after the Sprint-4 e2e gate (2026-04-07).** Three critical bugs slipped past 251 unit tests because the test pyramid had no integration layer, no metrics endpoint, no `/ready` probe, and no Prometheus surface to compare expected vs observed throughput. This epic exists to make sure the next class of bugs gets caught **before** a judge runs `docker compose up`, not three hours into a manual smoke test.

The goal is no longer "structured logs and some tests." The goal is: **a judge boots the container, points it at mainnet, opens `/metrics` in Grafana, and watches a Solarix dashboard come alive in real time** — while in parallel, every commit is gated by a CI pipeline that catches schema drift, decode regressions, and Send-inference breakage at PR time.

---

## Findings That Drive This Epic

These are not hypothetical. Every item below was observed during the Sprint-4 e2e verification against Meteora DLMM on mainnet.

### Critical bugs that 257 unit tests did not catch

| #   | Bug                                                                                                                                                     | Why unit tests missed it                                                                                                               | What test would have caught it                                                                                         |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| 1   | IDL account address derived as `find_program_address(&[b"anchor:idl"], pid)` instead of `create_with_seed(signer, "anchor:idl", pid)`                   | The unit test asserted determinism of a _wrong_ function and never compared against an Anchor reference vector                         | Snapshot test against a known-good Anchor IDL address fixture (e.g. Marinade's published IDL account)                  |
| 2   | `programs.idl_json` column did not exist; pipeline never auto-started after container restart because `query_registered_programs` returned `Vec::new()` | No integration test ever exercised "register → restart container → verify pipeline runs" because there was no integration layer at all | LiteSVM-backed integration test that registers a program, restarts the orchestrator, and asserts a checkpoint advances |
| 3   | `slot_gt=N` returned 500 with `operator does not exist: bigint > text` because the query builder bound every value as TEXT regardless of column type    | Unit tests asserted `sql.contains("WHERE \"amount\" > ")` but never executed the SQL against a real PostgreSQL                         | testcontainers-rs PostgreSQL harness in `cargo test --features integration`                                            |
| 4   | `docker-compose.yml` hardcoded `SOLARIX_LOG_FORMAT: "pretty"`, blocking AC14 (JSON logs by default)                                                     | No CI step actually parsed `docker compose logs` for valid JSON                                                                        | Docker smoke test that pipes a log line through `jq`                                                                   |
| 5   | `.env.example` was missing 6 of 27 env vars and had a stale entry that doesn't exist in `Config`                                                        | No test compared `.env.example` to the `Config` struct fields                                                                          | Code-gen check: derive macro emits `.env.example` from `Config`, fail CI if drift                                      |

### Observability gaps observed in real logs

- **No metrics endpoint at all.** The only way to know throughput was `docker compose exec postgres psql ... SELECT total_instructions FROM indexer_state`. A judge cannot grok performance from logs alone.
- **`BackfillProgress.txs_decoded` is never incremented.** Logs literally said `txs:0` while 786 instructions were being written to the database. Internal counters are demonstrably untrustworthy.
- **No correlation ID on API requests.** When `slot_gt` returned 500, finding the matching log line meant grepping by timestamp window — there was no `request_id` field to filter by.
- **No `program_id` field on the per-decode warning logs.** During Meteora streaming, we saw `unknown discriminator: e445a52e51cb9a1d` warnings with no way to tell which target program decoder they came from. (This is a multi-program correctness signal.)
- **State transition logs are inconsistent.** `cold start: backfill required` exists, but the matching `backfill complete, streaming continues` log is at info-level on a separate line, with no shared `pipeline_state` field. You cannot reconstruct the state machine from logs.
- **`subscribed to logsNotification` does not include `program_id`.** During the streaming test, the WS subscription log line was orphaned from the pipeline log line, making multi-program troubleshooting impossible.
- **No span around backfill chunks.** When we see `chunk processing failed`, there is no parent span correlating it to a specific `(start_slot, end_slot)` chunk and decode/write counts.
- **No structured shutdown summary.** We get `shutdown complete, uptime_secs=14` and nothing else. No "X instructions written, Y RPC retries, Z decode failures during this run."
- **`graceful_shutdown` in `main.rs` does not honor `shutdown_drain_secs`.** It only enforces `shutdown_db_flush_secs`. The drain budget is documented but not wired.

### Production hardening gaps observed

- **Cluster mismatch is silent and dangerous.** During testing I registered Meteora against mainnet, restarted with the devnet RPC URL, and the indexer happily computed a 42-million-slot "gap" from the saved mainnet checkpoint to the devnet tip. It did not crash. It would have happily wasted RPC quota.
- **Only `programs[0]` is indexed.** `main.rs` registers a warning if more than one program is registered, then silently ignores all but the first. AC11 (second-program isolation) was verified at the _schema_ level only.
- **Pipeline does not auto-start on API registration.** Workflow today: `POST /api/programs` → `docker compose restart solarix` → wait. That is a UX cliff for any judge.
- **No `/ready` probe distinct from `/health`.** Today `/health` returns 200 if the DB is up. It returns 200 even if the pipeline is `failed` or has never processed a slot. A judge's load balancer would route traffic to a broken pipeline.
- **No `/metrics`, no `/version`, no `/info`.** Standard production surfaces missing.
- **No request body size limit on the registration endpoint.** A 100MB IDL upload would pin the writer task and OOM the container.
- **No CORS, no auth, no rate limiting on the API.** Anyone with network access can `DELETE /api/programs/{id}?drop_tables=true`.
- **`Dockerfile` uses `rust:latest` and `debian:bookworm-slim` unpinned.** Reproducible builds: no.
- **Container runs as root.** No `USER solarix` line in `Dockerfile`.
- **`status` columns on `programs` and `indexer_state` are unconstrained `TEXT`.** A typo in code can write any string and we'll never notice until a query fails.
- **`panic = "abort"` is not set in `[profile.release]`.** A panic in a tokio task today silently kills the task and the orchestrator may continue with partial state.
- **No `cargo audit`, no `cargo deny`, no SBOM.** We discovered `backoff` was unmaintained (RUSTSEC-2025-0012) only by chance.
- **No retention or archival policy on indexed data.** A program with high volume will fill disk indefinitely.
- **Bootstrap DDL is not wrapped in an explicit transaction.** Two `CREATE TABLE` statements without `BEGIN/COMMIT`. Partial failure leaves half-bootstrapped state.

### Test coverage gaps

- **Zero integration tests against a real PostgreSQL.** We have 257 unit tests; the entire query-builder + writer surface is unit-tested via string `contains` assertions on the generated SQL, never against a running database.
- **Zero LiteSVM tests.** The story 6.3 acceptance criteria mentioned LiteSVM but no test exists.
- **Zero proptest roundtrips for the decoder.** The story 6.2 acceptance criteria mentioned proptest but `tests/decode_roundtrip.rs` does not exist. The `proptest` crate is in dev-deps but unused.
- **Zero fuzz tests.** No `cargo-fuzz` target. The decoder has never been fed random bytes.
- **Zero `axum-test` integration tests.** `axum-test` is in dev-deps but unused.
- **Zero snapshot tests for IDL → DDL output.** Schema regressions are detectable only by reading the diff manually.
- **Send-safety tests are inconsistent.** `_require_pipeline_orchestrator_send` exists in `src/pipeline/mod.rs:1330` but not for `WsTransactionStream`, `IdlManager`, `ProgramRegistry`, or any API handler future. The Sprint-3 `!Send` blocker took two days to resolve precisely because we lacked these compile-time checks at the leaf.
- **No chaos test.** RPC 429s, WS disconnects, and Postgres unavailability are not simulated in tests. The exponential backoff exists but its actual behavior under bursty failure has never been verified.
- **No regression test for any of the bugs fixed during the Sprint-4 e2e gate.**

---

## Story Numbering & Priority

The original Epic 6 had four stories. The refined epic has **eleven**, grouped by track and prioritized so the bounty submission ships with at least the **P0** items, with **P1** as fast-follow and **P2** as post-launch.

| ID   | Title                                                                                             | Priority | Track         |
| ---- | ------------------------------------------------------------------------------------------------- | -------- | ------------- |
| 6.1  | Structured Tracing & Span Propagation                                                             | P0       | Observability |
| 6.2  | Prometheus `/metrics` Endpoint                                                                    | P0       | Observability |
| 6.3  | Health, Readiness, Version Probes                                                                 | P0       | Observability |
| 6.4  | Core Module Tests (Decoder, Schema, Filter Builder)                                               | P0       | Test Coverage |
| 6.5  | Integration Tests with testcontainers + LiteSVM                                                   | P0       | Test Coverage |
| 6.6  | API Tests with `axum-test` + Regression Suite                                                     | P0       | Test Coverage |
| 6.7  | CI Pipeline (lint, test, coverage, audit, docker smoke)                                           | P0       | Test Coverage |
| 6.8  | Production Hardening (panic abort, CHECK constraints, image pinning, non-root, body limits, CORS) | P1       | Hardening     |
| 6.9  | Cluster Identity Guard (genesis hash validation)                                                  | P1       | Hardening     |
| 6.10 | Multi-Program Orchestration (one pipeline per program)                                            | P1       | Hardening     |
| 6.11 | Pipeline Auto-Start on API Registration                                                           | P1       | Hardening     |

---

## Story 6.1: Structured Tracing & Span Propagation

**As an** operator debugging a multi-program indexer,
**I want** every log line to carry enough span context to reconstruct the path of a single request, decode, or RPC call,
**So that** I can answer "what happened to _this_ signature?" in `jq` without grepping by timestamp.

### Findings driving this story

- During the e2e session, the `unknown discriminator` warn line had no `program_id` field — making it impossible to attribute the warning when more than one program is registered.
- The `subscribed to logsNotification` log line was orphaned from any parent span.
- The `BackfillProgress.txs_decoded` counter logged `0` while 786 rows were written. Internal counter discipline is broken.
- API request → decoded instruction → DB write is currently impossible to correlate from logs alone.

### Acceptance Criteria

**Span coverage (instrumentation pass)**

**Given** every public async function in `src/pipeline/`, `src/api/handlers.rs`, `src/idl/`, `src/registry.rs`, `src/storage/writer.rs`, and `src/storage/queries.rs`,
**Then** each is annotated with `#[tracing::instrument]` declaring the relevant fields (`program_id`, `slot`, `signature`, `chunk_start`, `chunk_end`, `request_id`),
**And** `skip(self, …)` is used on methods so that large structs do not bloat span metadata,
**And** `err(Display)` is used on functions returning `Result<_, _>` so errors are emitted as span events instead of being lost.

**Pipeline state transitions are first-class events**

**Given** the pipeline state machine `Initializing → Backfilling → CatchingUp → Streaming → ShuttingDown`,
**When** any transition occurs,
**Then** a single `info!` event is emitted with `pipeline.state.from`, `pipeline.state.to`, `program_id`, `schema_name`, and `last_processed_slot` fields,
**And** the field name is consistent across every emitter (no more "cold start: backfill required" vs "starting backfill" inconsistency),
**And** the event is also recorded as a Prometheus state-transition counter (see Story 6.2).

**Per-decode logs always carry program correlation**

**Given** any `warn!` or `error!` emitted by `decode_transaction` or `decode_block` in `src/pipeline/mod.rs`,
**Then** every line includes `program_id`, `signature`, `slot`, and `error.kind` (the `DecodeError` variant name),
**And** the `>90% decode failure rate` error event includes `chunk_start`, `chunk_end`, `failures`, `attempts`, and a single human-readable `hint` field suggesting "IDL version mismatch or wrong target program."

**API request correlation**

**Given** any request to `axum::Router`,
**When** it enters middleware,
**Then** the request is wrapped in a span with: `request.id` (UUIDv7), `http.method`, `http.target`, `http.route`, `http.user_agent`,
**And** the response is logged with `http.status_code` and `http.duration_ms`,
**And** the `request.id` is propagated as the `X-Request-Id` response header so a judge can quote it back in a bug report.

**Backfill chunk spans**

**Given** the `process_chunk` function in `src/pipeline/mod.rs`,
**When** it executes,
**Then** it opens a span `pipeline.backfill.chunk` with fields `chunk_start`, `chunk_end`, `program_id`, `schema_name`,
**And** at chunk completion the span records `blocks_processed`, `txs_decoded`, `decode_failures`, `chunk_duration_ms` as a single span event,
**And** the **`txs_decoded` counter is actually incremented** (this is a regression-fix AC: the counter has been broken since story 3.5).

**Shutdown summary event**

**Given** the application receives SIGTERM,
**When** it completes shutdown,
**Then** a final `info!` event is emitted with: `uptime_secs`, `total_instructions_indexed`, `total_accounts_indexed`, `total_rpc_retries`, `total_decode_failures`, `final_pipeline_state`,
**And** the existing `shutdown complete` log message is replaced by this richer event so a judge sees a one-line summary at process exit.

**Log level discipline test**

**Given** a unit test in `tests/log_levels.rs`,
**When** it runs,
**Then** it parses the source tree for `error!`, `warn!`, `info!`, `debug!`, `trace!` macros and asserts: no `info!` inside per-block hot loops, no `error!` for retryable errors, no `warn!` without `program_id` in pipeline modules. (Static check, AST-walk style.)

### Out of scope

- OpenTelemetry exporter (deferred to a future story; we use plain `tracing` JSON for now).
- Trace sampling.

---

## Story 6.2: Prometheus `/metrics` Endpoint

**As an** operator,
**I want** a `/metrics` endpoint exposing Prometheus-compatible counters, gauges, and histograms,
**So that** I can drop a Solarix Grafana dashboard in front of any deployment and watch real numbers instead of `tail -f`.

### Findings driving this story

- The only way to verify "is the pipeline actually working" during the e2e session was a SQL query against `indexer_state.total_instructions`. That is not acceptable for a judging environment.
- Decode failure rate, RPC latency, writer queue depth, WebSocket disconnect count — none of these are observable today.
- Throughput claims in the README cannot be substantiated without metrics.

### Acceptance Criteria

**Endpoint shape**

**Given** the application is running,
**When** `GET /metrics` is called,
**Then** it returns `200 OK` with `Content-Type: text/plain; version=0.0.4; charset=utf-8`,
**And** the body is in Prometheus text exposition format,
**And** every metric carries `program_id` and `schema_name` labels where applicable,
**And** the endpoint is added to the existing `axum::Router` in `src/api/mod.rs` (no separate listener).

**Required metrics**

| Metric                                     | Type             | Labels                           | Source                                                                   |
| ------------------------------------------ | ---------------- | -------------------------------- | ------------------------------------------------------------------------ |
| `solarix_pipeline_state`                   | gauge            | `program_id`, `state`            | one-of `{initializing,backfilling,catching_up,streaming,failed,stopped}` |
| `solarix_pipeline_state_transitions_total` | counter          | `program_id`, `from`, `to`       | every state change                                                       |
| `solarix_instructions_decoded_total`       | counter          | `program_id`, `instruction_name` | per successful decode                                                    |
| `solarix_instructions_written_total`       | counter          | `program_id`, `stream`           | `stream` ∈ `{backfill,realtime,catchup}`                                 |
| `solarix_accounts_decoded_total`           | counter          | `program_id`, `account_type`     | per successful decode                                                    |
| `solarix_decode_errors_total`              | counter          | `program_id`, `error_kind`       | one per `DecodeError` variant                                            |
| `solarix_rpc_calls_total`                  | counter          | `method`, `outcome`              | `outcome` ∈ `{ok,retry,error}`                                           |
| `solarix_rpc_call_duration_seconds`        | histogram        | `method`                         | per HTTP RPC call, buckets `[0.05,0.1,0.25,0.5,1,2.5,5,10]`              |
| `solarix_rpc_rate_limit_hits_total`        | counter          | `method`                         | per 429                                                                  |
| `solarix_ws_messages_received_total`       | counter          | `program_id`, `kind`             | `kind` ∈ `{logs_notification,ping,pong}`                                 |
| `solarix_ws_disconnects_total`             | counter          | `program_id`, `reason`           | per disconnect                                                           |
| `solarix_writer_queue_depth`               | gauge            | `program_id`                     | mpsc `len()` snapshot                                                    |
| `solarix_writer_batch_duration_seconds`    | histogram        | `schema_name`                    | per `write_block` call                                                   |
| `solarix_last_processed_slot`              | gauge            | `program_id`, `stream`           | from `_checkpoints`                                                      |
| `solarix_chain_tip_slot`                   | gauge            | (none)                           | `getSlot` cached value                                                   |
| `solarix_slot_lag`                         | gauge            | `program_id`                     | derived: `chain_tip - last_processed`                                    |
| `solarix_api_requests_total`               | counter          | `method`, `route`, `status`      | per request                                                              |
| `solarix_api_request_duration_seconds`     | histogram        | `method`, `route`                | per request                                                              |
| `solarix_idl_cache_size`                   | gauge            | (none)                           | `IdlManager.cache.len()`                                                 |
| `solarix_build_info`                       | gauge (always 1) | `version`, `git_sha`, `rustc`    | constant                                                                 |

**Wiring**

**Given** the existing `Arc<StorageWriter>` and `Arc<RpcClient>`,
**When** the pipeline runs,
**Then** the writer task increments counters via a shared `Metrics` struct held inside `AppState` (single source of truth, no duplicate registration),
**And** counters use `prometheus` v0.13 or `metrics` v0.24 with `metrics-exporter-prometheus`,
**And** the choice between the two is documented in an ADR added to `docs/adr/0001-metrics-library.md`.

**Performance budget**

**Given** the `/metrics` endpoint under load,
**Then** `GET /metrics` returns in under 10ms p99 even with 50 registered programs,
**And** scraping `/metrics` does not block the writer task or any RPC call.

**Cardinality discipline**

**Given** the metric label set,
**Then** no metric label takes a high-cardinality value such as `signature`, `pubkey`, or full block JSON,
**And** the unit test `tests/metrics_cardinality.rs` asserts that no metric exposes more than 1000 unique label combinations under a synthetic workload.

**Documentation**

**Given** the metrics surface,
**Then** `docs/metrics.md` documents every metric with type, labels, and a one-line "what does this tell me" description,
**And** a sample `dashboards/solarix.json` Grafana dashboard is committed alongside the README so a judge can `grafana-cli` import it.

### Non-goals

- Pushgateway support.
- StatsD/DogStatsD output.
- Custom histograms (use the documented bucket set).

---

## Story 6.3: Health, Readiness, Version Probes

**As an** operator running Solarix behind a load balancer or in Kubernetes,
**I want** distinct liveness, readiness, and version endpoints,
**So that** the LB does not route traffic to a process whose pipeline has failed and I can reliably identify which build I'm running.

### Findings driving this story

- During the e2e session, `/health` returned 200 even when the pipeline had never processed a slot. A judge's `kubectl get pods` would show "ready" while indexing is broken.
- There is no `/version` or `/info` endpoint. The build SHA is invisible to operators.

### Acceptance Criteria

**Liveness endpoint**

**Given** `GET /health`,
**Then** it returns 200 if the process is alive and the database connection is reachable within 2 seconds,
**And** it does NOT consider pipeline state,
**And** the existing fields (`status`, `database`, `uptime_seconds`, `version`, `programs`) are preserved for backwards-compatibility with the e2e test suite.

**Readiness endpoint**

**Given** `GET /ready`,
**Then** it returns 200 only when:

1. The database is reachable,
2. At least one program is registered AND its pipeline state is in `{Backfilling, CatchingUp, Streaming}`,
3. The last heartbeat is less than `2 × checkpoint_interval_secs` ago,
   **And** it returns 503 with a JSON body explaining which condition failed otherwise,
   **And** the response body shape is documented in the API reference.

**Version endpoint**

**Given** `GET /version`,
**Then** it returns:

```json
{
  "version": "0.1.0",
  "git_sha": "abcdef1",
  "git_branch": "main",
  "build_timestamp": "2026-04-07T12:00:00Z",
  "rustc_version": "1.86.0",
  "target_triple": "aarch64-apple-darwin"
}
```

**And** the values are baked in at compile time via `build.rs` + the `vergen` crate (or a hand-rolled equivalent — keep dep tree minimal),
**And** if the build is from a dirty working tree, `git_sha` is suffixed with `-dirty`.

**Info endpoint**

**Given** `GET /info`,
**Then** it returns the resolved (effective) configuration as JSON, with all secret-bearing fields (`DATABASE_URL`) redacted via the existing `sanitize_database_url`,
**And** every field name matches the `Config` struct field name so an operator can correlate with `.env`.

### Out of scope

- Kubernetes probe spec generation.
- Auth on these endpoints (covered in Story 6.8).

---

## Story 6.4: Core Module Tests (Decoder, Schema, Filter Builder)

**As a** developer adding a new IDL type or filter operator,
**I want** property-based and snapshot tests covering the decoder, schema generator, and filter builder,
**So that** I cannot accidentally regress the bugs the e2e session uncovered.

### Findings driving this story

- The decoder has never been tested with a Borsh roundtrip across all supported types.
- The schema generator has zero snapshot tests; a regression in DDL output is invisible.
- The query builder bug (`bigint > text`) was not caught by any unit test because tests asserted `sql.contains("WHERE…")` instead of running the SQL against a real database.
- The IDL PDA derivation had a unit test that asserted determinism of _the wrong function_.

### Acceptance Criteria

**Decoder property tests**

**Given** `tests/decode_roundtrip.rs`,
**When** it runs `cargo test --test decode_roundtrip`,
**Then** for every Borsh primitive type (`u8/i8`, `u16/i16`, `u32/i32`, `u64/i64`, `u128/i128`, `f32/f64`, `bool`, `String`, `Pubkey`, `Vec<T>`, `Option<T>`, `[T; N]`),

- a value is generated with `proptest::strategy`,
- Borsh-serialized,
- decoded via `SolarixDecoder::decode_instruction` and `decode_account`,
- the resulting JSON is asserted to round-trip back to the original,
  **And** `u64 > i64::MAX` is verified to land in JSONB as a string (not a truncated number),
  **And** `u128`/`i128` are verified to serialize as JSON strings,
  **And** `f32::NAN` and `f64::NAN` are verified to NOT cause a JSON serialize panic (regression for the deferred-work item from story 3.1),
  **And** the test runs at minimum 256 cases per type locally and 1024 cases in CI (`PROPTEST_CASES=1024`).

**Decoder fuzz target**

**Given** a `fuzz/` directory with `cargo-fuzz` configured,
**When** `cargo +nightly fuzz run decode_instruction` runs,
**Then** the decoder NEVER panics on any input of length 0..4096,
**And** the fuzz corpus is committed with at least 50 seed inputs derived from real Meteora/Marinade transactions.

**Anchor IDL PDA address regression test**

**Given** `tests/idl_address_vectors.rs`,
**When** it runs,
**Then** it asserts the derivation in `src/idl/fetch.rs` matches at least 5 known Anchor IDL account addresses for popular mainnet programs,
**And** the test would have caught the original bug where seeds were `&[b"anchor:idl"]`.

**Schema generator snapshot tests**

**Given** `tests/schema_snapshots.rs` using `insta` for snapshot testing,
**When** it runs against fixture IDLs (`tests/fixtures/idls/simple_v030.json`, plus the fetched Meteora and Marinade IDLs),
**Then** the generated DDL is compared against committed `.snap` files,
**And** any change to schema generation requires explicit `cargo insta review` approval,
**And** snapshots cover: column promotion rules, sanitize_identifier behavior on edge cases (digit-starting names, unicode, 63-byte truncation, reserved keywords), index generation, JSONB GIN setup.

**Schema generator runs against real PostgreSQL**

**Given** the schema generator,
**When** it runs against a test database (testcontainers, see Story 6.5),
**Then** every generated DDL statement is executed and the resulting tables are introspected,
**And** column types are asserted to match the IDL field types (BIGINT for u64, TEXT for pubkey, etc.),
**And** running `bootstrap_system_tables` twice in a row is idempotent (no errors, no data loss).

**Filter builder integration tests**

**Given** `tests/filter_sql.rs`,
**When** it runs against a seeded test schema,
**Then** every filter operator (`_eq`, `_ne`, `_gt`, `_gte`, `_lt`, `_lte`, `_contains`, `_in`) is exercised against every column type (BIGINT, SMALLINT, TEXT, BOOLEAN, JSONB),
**And** the test would have caught the `operator does not exist: bigint > text` bug,
**And** `_in` with empty value yields zero rows (not 500),
**And** unknown filter fields return 400 with `available_fields` list.

**Send-safety compile-time tests**

**Given** every public async function returning `impl Future`,
**When** the test module compiles,
**Then** a `_require_send` helper asserts the future is `Send + 'static`,
**And** this is enforced for `WsTransactionStream::next`, `WsTransactionStream::subscribe`, `IdlManager::get_idl`, `ProgramRegistry::commit_registration`, `PipelineOrchestrator::run`, `StorageWriter::write_block`, `do_register`, `query_instructions`, `query_accounts`,
**And** the project's CLAUDE.md `!Send` lessons section is referenced from the test module's doc comment.

### Out of scope

- Account snapshot decoding tests (deferred until a representative test program is selected).
- LiteSVM-based decoder tests (covered in Story 6.5).

---

## Story 6.5: Integration Tests with testcontainers + LiteSVM

**As a** developer,
**I want** end-to-end integration tests that boot real PostgreSQL and a real Solana validator inside CI,
**So that** the next class of bugs (the kind that hide in interactions, not in pure functions) gets caught at PR time.

### Findings driving this story

- The three critical bugs from the Sprint-4 gate were all interaction bugs (Anchor SDK ↔ our code, registration ↔ pipeline, query builder ↔ PostgreSQL).
- We have zero tests that would have caught any of them.
- LiteSVM is mentioned in `CLAUDE.md` and the original story 6.3 ACs but no actual test exists.
- `axum-test` is in `dev-dependencies` but unused.

### Acceptance Criteria

**testcontainers PostgreSQL harness**

**Given** `tests/common/postgres.rs`,
**When** any integration test calls `with_postgres(|pool| async { … })`,
**Then** a fresh PostgreSQL 16 container is spawned via `testcontainers-rs`,
**And** `bootstrap_system_tables` is called against the container's pool,
**And** the harness auto-cleans containers on test completion or panic,
**And** parallel test runs are supported (each test gets its own container).

**Registration → schema → query integration test**

**Given** `tests/integration_register_query.rs`,
**When** it runs,
**Then** it: starts a PostgreSQL container, calls `do_register` with a fixture IDL, asserts the schema and tables exist, inserts a synthetic decoded instruction directly into `_instructions`, calls `query_instructions` via the API layer, and asserts the row is returned with all promoted columns populated correctly.

**Filter execution integration test**

**Given** `tests/integration_filters.rs`,
**When** it runs,
**Then** it seeds a test schema with rows of every typed PG column (BIGINT, SMALLINT, TEXT, BOOLEAN, JSONB), constructs filter requests for every operator/type combination via the API handler, executes them against the testcontainer, and asserts the rows returned match the expected set,
**And** this test would have caught the `bigint > text` bug.

**LiteSVM decode pipeline test**

**Given** `tests/litesvm_pipeline.rs`,
**When** it runs,
**Then** it: deploys a minimal Anchor test program to LiteSVM, sends 10 known transactions, runs the indexing pipeline against LiteSVM as the RPC source (mocked via `BlockSource` trait), and asserts decoded instructions and accounts appear in the testcontainer database with correct values,
**And** the test asserts the `_checkpoints` table advances under `stream='backfill'`,
**And** kill-mid-test then restart asserts checkpoint resume works (regression test for the persisted-IDL fix).

**Mainnet smoke test (gated)**

**Given** `tests/mainnet_smoke.rs` gated behind `--features mainnet-smoke`,
**When** it runs,
**Then** it registers Meteora DLMM (`LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo`) against `https://api.mainnet-beta.solana.com`,
**And** asserts at least one decoded instruction lands in the database within 60 seconds,
**And** this test is run as a nightly CI job, not on every PR (cost / flakiness control),
**And** the test program ID is centralized in `tests/common/known_programs.rs`.

**Test isolation**

**Given** the integration test suite,
**When** it runs in CI,
**Then** tests do not share PostgreSQL databases (each test gets its own container or schema),
**And** `cargo test --tests` works without `--test-threads=1`,
**And** test execution time is under 90 seconds for the non-mainnet suite.

### Non-goals

- E2E tests against multiple Solana clusters in parallel.
- Tests that require deploying to devnet (cost, flakiness).

---

## Story 6.6: API Tests with `axum-test` + Regression Suite

**As a** developer,
**I want** request-level tests for every API endpoint with regression coverage for every fixed bug,
**So that** future refactors cannot quietly break public contracts.

### Findings driving this story

- Every API contract is currently verified only via unit tests on `IntoResponse` impl (which catches the _enum mapping_ but never the _router wiring_).
- Every bug fixed during the e2e session deserves a regression test.

### Acceptance Criteria

**`axum-test` integration**

**Given** `tests/api_endpoints.rs`,
**When** it runs,
**Then** it spins up the full `axum::Router` against a testcontainer Postgres,
**And** every endpoint listed in `src/api/mod.rs:router()` has at least one happy-path and one error-path test:

- `POST /api/programs` (manual upload)
- `POST /api/programs` (auto-fetch — mocked at the IDL fetch layer)
- `GET /api/programs`
- `GET /api/programs/{id}`
- `DELETE /api/programs/{id}` (soft + hard with `?drop_tables=true`)
- `GET /api/programs/{id}/instructions`
- `GET /api/programs/{id}/instructions/{name}` (with cursor pagination)
- `GET /api/programs/{id}/instructions/{name}/count?interval={…}`
- `GET /api/programs/{id}/stats`
- `GET /api/programs/{id}/accounts`
- `GET /api/programs/{id}/accounts/{type}` (with offset pagination)
- `GET /api/programs/{id}/accounts/{type}/{pubkey}`
- `GET /health`, `/ready`, `/version`, `/info`, `/metrics`

**JSON envelope contract test**

**Given** every successful API response,
**When** it is asserted in the test suite,
**Then** the `data`, `meta`, and `pagination` (where applicable) fields are present and correctly typed,
**And** every error response matches `{"error": {"code": "…", "message": "…"}}` (or `{"error": {"code": "INVALID_FILTER", "available_fields": […]}}` for filter errors),
**And** the test asserts `Content-Type: application/json` on every response.

**Regression test suite**

**Given** `tests/regression_e2e_sprint4.rs`,
**When** it runs,
**Then** it includes one test per bug fixed during the Sprint-4 e2e gate:

1. `test_idl_address_derivation_matches_anchor_v030` (regression for the wrong PDA derivation)
2. `test_idl_json_persisted_and_loaded_on_restart` (regression for the missing `idl_json` column)
3. `test_promoted_column_filter_with_bigint_value` (regression for `bigint > text`)
4. `test_docker_compose_log_format_is_json_by_default` (regression for the hardcoded `pretty`)
5. `test_env_example_documents_every_config_field` (regression for the stale `.env.example`)
6. `test_invalid_program_id_returns_documented_status` (lock down the 400 vs 422 decision once made)

**Cursor pagination invariant test**

**Given** the cursor pagination implementation,
**When** the test fetches every page of an instruction set,
**Then** no row appears in two pages,
**And** the union of all pages equals the total row count,
**And** the test runs against synthetic data of 1000 rows.

**Filter type coercion test**

**Given** filter inputs of every type the user might pass,
**When** the test sends them,
**Then** valid values return 200, invalid values (non-numeric for numeric column, malformed pubkey, etc.) return 400 with an `INVALID_VALUE` code,
**And** in NO case does the API return 500 because of a type mismatch (regression for `bigint > text`).

### Non-goals

- Load testing (separate story or post-launch).
- Penetration testing.

---

## Story 6.7: CI Pipeline (Lint, Test, Coverage, Audit, Docker Smoke)

**As a** maintainer,
**I want** every commit gated by an automated pipeline that catches everything Stories 6.1–6.6 add,
**So that** the bounty submission cannot be silently broken by a careless merge.

### Acceptance Criteria

**Workflow file**

**Given** `.github/workflows/ci.yml`,
**When** a push or PR is created against `main`,
**Then** the following jobs run **in parallel**, with the merge gate requiring all to pass:

**Job: lint**

- `cargo fmt -- --check`
- `cargo +nightly fmt -- --check --config group_imports=StdExternalCrate,imports_granularity=Crate` (the import-ordering convention from CLAUDE.md is enforced here, not just by reviewer discipline)
- `cargo clippy --release --all-targets -- -D warnings` (note: today there are 7 pre-existing warnings — those must be fixed or `#[allow]`'d explicitly with a justification comment)

**Job: unit**

- `cargo test --release --lib`
- Asserts all 257+ unit tests pass.

**Job: integration**

- Spins up a `postgres:16` service container.
- Runs `cargo test --release --tests --features integration`.
- Includes the testcontainer-based tests from Story 6.5 and the API tests from Story 6.6.

**Job: coverage**

- Installs `llvm-tools-preview` + `cargo-llvm-cov`.
- Runs `cargo llvm-cov --release --workspace --lcov --output-path lcov.info`.
- Uploads coverage report as a build artifact.
- Soft gate: prints coverage to PR comment, fails the job only if coverage drops more than 2 percentage points compared to `main`.

**Job: fuzz-smoke**

- `cargo +nightly fuzz run decode_instruction -- -max_total_time=60`
- Asserts no panic in the 60-second smoke run.
- Full fuzz runs are scheduled separately as a nightly job.

**Job: security**

- `cargo audit --deny warnings` (would have caught `backoff` RUSTSEC-2025-0012).
- `cargo deny check` against `deny.toml` (committed with this story).
- Secret scanning via `gitleaks` on the diff.

**Job: docker-smoke**

- `docker compose down -v && docker compose up --build -d`
- Polls `/health` until 200 with a 60-second timeout.
- Polls `/ready` until 200 with the same timeout.
- Pipes `docker compose logs solarix | head -10 | jq .` and asserts every line is valid JSON (regression for the pretty-format bug).
- Asserts `/metrics` returns 200 and the body contains `solarix_build_info`.
- `docker compose down -v` for cleanup.

**Job: msrv**

- Builds with `rust-toolchain.toml` pinned to the documented MSRV.
- A separate matrix job runs against `stable` and `beta` (warns only on `beta`).

**Nightly job: mainnet-smoke**

- Runs `cargo test --features mainnet-smoke -- mainnet_smoke`.
- Allowed to flake (continue-on-error) but failures alert via PR comment on the next merge.

**Concurrency & cache**

**Given** the CI workflow,
**Then** it uses `actions/cache` for the cargo registry and target directory,
**And** it uses `cancel-in-progress` so superseded PR runs are killed,
**And** the average end-to-end PR run is under 12 minutes.

### Non-goals

- Release automation (separate epic).
- Multi-arch builds (deferred until Solarix is published as a binary release).

---

## Story 6.8: Production Hardening

**As an** operator deploying Solarix to a real environment,
**I want** the boring hardening items that distinguish a "demo" from a "production-grade" project,
**So that** the bounty submission survives a hostile read.

### Findings driving this story

Multiple items observed during the Sprint-4 e2e session and called out in `deferred-work.md`. Each is small but together they signal seniority.

### Acceptance Criteria

**Panic discipline**

**Given** `Cargo.toml`,
**Then** `[profile.release]` includes `panic = "abort"` so panics in tokio tasks terminate the process instead of silently killing the task,
**And** the existing `clippy.toml` enforcement of `unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"` is documented in `CONTRIBUTING.md`.

**Database CHECK constraints**

**Given** `bootstrap_system_tables` in `src/storage/mod.rs`,
**When** it runs,
**Then** the `programs.status` column has a `CHECK` constraint accepting only `{registered, schema_created, error, stopped}`,
**And** the `indexer_state.status` column has a `CHECK` constraint accepting only `{initializing, backfilling, catching_up, streaming, idle, failed, stopped, error}`,
**And** the bootstrap DDL is wrapped in an explicit `BEGIN/COMMIT` transaction,
**And** an `updated_at` trigger is added to `programs` so updates correctly reflect the modification time (regression for the deferred-work item from story 1.2).

**Dockerfile hardening**

**Given** `Dockerfile`,
**Then** the build base is pinned: `FROM rust:1.86-bookworm AS builder` and `FROM debian:bookworm-20251130-slim`,
**And** the build uses `cargo chef` (or equivalent) to cache the dependency layer separately from source,
**And** the runtime stage adds a `solarix` system user (`RUN useradd -r -s /bin/false solarix && USER solarix`),
**And** `HEALTHCHECK` is declared in the Dockerfile (not just compose), so `docker run` works without compose,
**And** the resulting image size is documented in `docs/deployment.md`.

**API surface protection**

**Given** the `axum::Router` in `src/api/mod.rs`,
**When** the application starts,
**Then** the request body size limit is set to 1 MB (covers IDL upload, blocks the OOM vector),
**And** a CORS layer is added with an allowlist driven by `SOLARIX_API_CORS_ORIGINS` (default: same-origin only),
**And** an optional bearer-token auth layer is added, gated by `SOLARIX_API_AUTH_TOKEN` env var (if unset, auth is disabled and a `warn!` is logged at startup),
**And** the `DELETE /api/programs/{id}?drop_tables=true` endpoint requires the bearer token even when auth is otherwise disabled (small irreversible-action gate),
**And** the API has a per-IP rate limit (e.g. 60 requests per minute) implemented with a simple in-memory token bucket — documented as not a security boundary, but a courtesy throttle.

**Config validation**

**Given** the `Config::parse()` flow,
**When** the application starts,
**Then** invalid combinations are rejected with a clear error before the application binds the listener:

- `db_pool_min > db_pool_max`
- `start_slot > end_slot` (when both set)
- `retry_initial_ms > retry_max_ms`
- `api_default_page_size > api_max_page_size`
- `rpc_rps == 0`
- `backfill_chunk_size == 0`
- `log_format` not in `{json, pretty}`
- `log_level` not in `{trace, debug, info, warn, error}`
  **And** a unit test asserts every invalid combination produces a non-zero exit code with a stable error message,
  **And** valid edge values (e.g. `db_pool_min == db_pool_max`) are accepted.

**Graceful drain enforcement**

**Given** the SIGTERM handler,
**When** shutdown begins,
**Then** the orchestrator stops accepting new chunks,
**And** the writer task drains the mpsc channel,
**And** `shutdown_drain_secs` is honored as a hard deadline (currently it is documented but not enforced),
**And** the final shutdown event records whether drain completed cleanly or timed out.

**Sensitive log redaction**

**Given** `sanitize_database_url` in `src/storage/mod.rs`,
**When** any other secret-bearing field exists (e.g. `SOLARIX_API_AUTH_TOKEN`),
**Then** a single `Config::redact_for_logging()` method is added,
**And** any `info!("config = {:?}", config)` is replaced by structured emission via this method,
**And** a unit test asserts no secret value can leak to logs via `Debug`.

### Non-goals

- mTLS, OAuth, SAML.
- Vault / KMS integration.
- Audit logging of mutating API calls.

---

## Story 6.9: Cluster Identity Guard

**As an** operator,
**I want** Solarix to refuse to start if the saved checkpoint corresponds to a different Solana cluster than the configured RPC URL,
**So that** I cannot accidentally point a mainnet-derived database at devnet and waste 42 million slots of backfill.

### Findings driving this story

This was a real footgun observed during the Sprint-4 session. After registering Meteora against mainnet, restarting with the devnet RPC URL produced `cold start: backfill required, last_checkpoint: 411629292, chain_tip: 453886534` — a 42M-slot "gap" that would have hammered devnet RPC for hours before producing nothing useful.

### Acceptance Criteria

**Genesis hash capture on first run**

**Given** the application starts for the first time against a cluster,
**When** the bootstrap step runs,
**Then** it calls `getGenesisHash` against the configured RPC URL,
**And** stores the result in a new `cluster_identity` table with `(genesis_hash TEXT PRIMARY KEY, first_seen_at TIMESTAMPTZ, rpc_url TEXT)`,
**And** logs the genesis hash at `info!` level.

**Genesis hash validation on subsequent starts**

**Given** the application starts and the `cluster_identity` table is non-empty,
**When** bootstrap runs,
**Then** it calls `getGenesisHash` and compares the result against the stored value,
**And** if they differ, the application logs a fatal error with both hashes and the stored `rpc_url`, then exits with a non-zero code,
**And** the error message includes a remediation hint: "Either restore the matching cluster RPC URL or wipe the database with `docker compose down -v`.",
**And** an opt-out env var `SOLARIX_ALLOW_CLUSTER_MISMATCH=true` exists (for advanced users) but emits a `warn!` on every start when set.

**Per-program genesis hash binding**

**Given** the `programs` table,
**Then** a `cluster_genesis_hash TEXT NOT NULL` column is added (with backfill from `cluster_identity` for existing rows),
**And** the genesis hash is stored on registration so a future "import this checkpoint into a new database" path can verify cluster match.

**Test coverage**

**Given** `tests/cluster_identity.rs`,
**When** it runs,
**Then** it asserts: first start writes the row, matching restart succeeds, mismatching restart exits with the documented error, opt-out env var allows mismatch but warns,
**And** all four cases run against testcontainer Postgres.

### Non-goals

- Multi-cluster support (one process indexes one cluster).
- Cluster migration tooling.

---

## Story 6.10: Multi-Program Orchestration

**As an** operator,
**I want** to register multiple Anchor programs and have them all indexed concurrently,
**So that** Solarix lives up to its "universal indexer" promise.

### Findings driving this story

Today, `main.rs` spawns a pipeline only for `programs[0]` and warns about the rest. This was the most-viable architectural shortcut to ship the e2e gate, but it is inconsistent with the bounty positioning.

### Acceptance Criteria

**Per-program orchestrator**

**Given** N programs registered with `status = 'schema_created'`,
**When** the application starts,
**Then** it spawns one `PipelineOrchestrator::run` task per program inside a single `tokio::task::JoinSet`,
**And** each orchestrator shares the same `PgPool`, `Arc<dyn SolarixDecoder>`, `RpcClient`, and `StorageWriter`,
**And** each orchestrator owns its own `child_token` of the master cancellation token so the shutdown signal propagates correctly,
**And** if any single orchestrator panics, the failure is logged with `program_id` and the other orchestrators continue running.

**Concurrency control**

**Given** the shared `RpcClient`,
**Then** the existing `governor` rate limiter applies globally across all orchestrators (a single `SOLARIX_RPC_RPS` budget shared across N programs),
**And** the writer task channel is per-program (each orchestrator has its own bounded channel),
**And** the `SOLARIX_MAX_CONCURRENT_PROGRAMS` env var sets a hard cap (default: 16) to prevent unbounded spawn on huge program lists.

**Per-program metrics**

**Given** the metrics endpoint from Story 6.2,
**Then** every metric carries `program_id` as a label,
**And** an aggregated dashboard panel groups by program.

**Per-program lifecycle endpoints**

**Given** the API,
**When** an operator calls `POST /api/programs/{id}/pause`,
**Then** the orchestrator for that program receives a cancel signal and transitions to `paused`,
**And** `POST /api/programs/{id}/resume` re-spawns the orchestrator from the last checkpoint,
**And** `GET /api/programs/{id}/status` returns the current pipeline state plus health metrics for that program only.

**Test coverage**

**Given** `tests/multi_program.rs` (gated behind `--features integration`),
**When** it runs,
**Then** it registers two test programs against LiteSVM, asserts both pipelines run concurrently, asserts a panic in one does not kill the other, asserts metrics are correctly labeled, asserts pause/resume works.

### Non-goals

- Cross-program transaction joins.
- Cross-program scheduler / fairness.

---

## Story 6.11: Pipeline Auto-Start on API Registration

**As a** judge or new user,
**I want** `POST /api/programs` to immediately start indexing the registered program,
**So that** I do not have to `docker compose restart solarix` between registration and seeing data flow.

### Findings driving this story

Today's flow is: `POST /api/programs` → create schema → return 201 → user manually restarts container → pipeline picks up the persisted IDL on startup. This is a real UX cliff — a judge will hit it within 60 seconds of opening the README.

### Acceptance Criteria

**SpawnHandle in AppState**

**Given** the `AppState` struct in `src/api/mod.rs`,
**When** the application starts,
**Then** `AppState` carries a `pipeline_supervisor: Arc<PipelineSupervisor>` field,
**And** `PipelineSupervisor` is a new type wrapping the `JoinSet` from Story 6.10 plus the shared `RpcClient`/`Decoder`/`Writer`,
**And** the API handler can call `state.pipeline_supervisor.spawn(program)` to launch a new orchestrator without restarting the process.

**Registration handler spawns orchestrator**

**Given** a successful `POST /api/programs`,
**When** the handler returns,
**Then** it has called `state.pipeline_supervisor.spawn(program_info)` before returning,
**And** the response body's `meta.message` is updated from "Indexing will begin shortly" to "Indexing started" (or "Indexing queued (concurrency cap reached)" if applicable),
**And** the response includes the supervisor's task handle ID for correlation.

**Idempotent re-registration**

**Given** an already-registered program,
**When** the handler is called,
**Then** the existing orchestrator is left running,
**And** the response is 409 with the existing supervisor handle (not a new one).

**Test coverage**

**Given** `tests/integration_register_starts_pipeline.rs`,
**When** it runs,
**Then** it: starts the API against a testcontainer Postgres + LiteSVM, calls `POST /api/programs`, asserts the response is 201, then within 5 seconds asserts the pipeline state is `Backfilling` or `Streaming` via `GET /api/programs/{id}/status`.

### Non-goals

- Per-handler-call orchestrator hot-reload (an IDL update would still require explicit re-registration).

---

## Cross-Cutting Definition of Done

This epic is "done" when:

1. **Every story above is implemented** OR explicitly deferred to a follow-up epic with a justification logged in `deferred-work.md`.
2. **`/metrics` returns at least the metrics listed in Story 6.2**, and a screenshot of the Grafana dashboard is committed to `docs/screenshots/grafana-dashboard.png`.
3. **CI runs every job listed in Story 6.7** on every PR, and the badge is in the README.
4. **Every bug fixed during the Sprint-4 e2e gate has a corresponding regression test** in `tests/regression_e2e_sprint4.rs`.
5. **The decoder fuzzer has run for at least 24 hours cumulatively** without a panic, and the corpus is committed.
6. **The integration test suite executes in under 90 seconds** end-to-end on the standard CI runner.
7. **The `deferred-work.md` items from the e2e session are either fixed or moved to a "post-Epic-6" section** so the file accurately reflects what is and isn't covered.
8. **A "what observability gives me" two-page guide** is added to `docs/operating-solarix.md` so a judge can read it in 90 seconds and understand how to monitor a Solarix deployment.

---

## What This Epic Explicitly Defers

To keep the epic from sprawling, the following are NOT in scope and should be tracked as separate epics:

- OpenTelemetry / OTLP exporter
- Distributed tracing across services
- Multi-cluster indexing (one process indexes one Solana cluster)
- Sharded Postgres / read replicas
- Authentication beyond a single bearer token
- A web UI
- Schema migrations beyond `IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS`
- Time-series storage backends (TimescaleDB, ClickHouse)
- gRPC API surface
- Solana account snapshotting against high-volume programs (deferred until a representative test program is selected)

---

## Sequencing Recommendation

Do NOT implement these in numerical order. The dependency graph is:

```
6.4 (core tests) ─────┐
                      ├──→ 6.5 (integration + LiteSVM) ──┐
6.7 (CI pipeline) ────┘                                  │
                                                         ├──→ 6.6 (API tests + regression suite)
6.1 (tracing + spans) ──┐                                │
                        ├──→ 6.2 (metrics) ──→ 6.3 (probes)
6.8 (hardening) ────────┘
                                                         │
6.9 (cluster identity) ──┐                              │
                          ├──→ 6.10 (multi-program) ──→ 6.11 (auto-start on registration)
```

Suggested merge order for a one-week sprint:

1. **Day 1–2:** 6.4 + 6.7 in parallel (test foundations + CI) — unblocks every other story.
2. **Day 2–3:** 6.1 + 6.8 in parallel (tracing + hardening) — independent and small.
3. **Day 3–4:** 6.2 + 6.3 sequentially (metrics, then probes that depend on metrics).
4. **Day 4–5:** 6.5 + 6.6 sequentially (integration tests, then API regression suite that depends on testcontainer harness).
5. **Day 5:** 6.9 (cluster identity guard) — small and isolated.
6. **Day 5–6:** 6.10 (multi-program orchestration) — biggest behavioral change, do last.
7. **Day 6:** 6.11 (auto-start on registration) — depends on 6.10.

The P0 stories (6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 6.7) MUST land before the bounty submission. The P1 stories (6.8, 6.9, 6.10, 6.11) are fast-follow and should be completed before any external user is invited to deploy Solarix.
