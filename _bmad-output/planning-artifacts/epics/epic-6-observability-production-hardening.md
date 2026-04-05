# Epic 6: Observability & Production Hardening

Operator gets structured JSON logs with per-stage tracing spans, and the system handles edge cases gracefully -- signals senior engineering quality. Comprehensive tests prove correctness across decoder, schema, pipeline, and API.

## Story 6.1: Structured Tracing & Observability

As an operator,
I want structured JSON logs with per-pipeline-stage tracing spans,
So that I can monitor, debug, and understand system behavior in production.

**Acceptance Criteria:**

**Given** the tracing setup in `main.rs`
**When** the application starts
**Then** `tracing-subscriber` is configured with JSON formatting for production (or pretty-print for development, controlled by `SOLARIX_LOG_FORMAT` env var)
**And** the global log level is configurable via `SOLARIX_LOG_LEVEL` (default: `info`)
**And** each log line includes: timestamp, level, target module, span context, structured fields

**Given** pipeline stage functions
**When** they execute
**Then** they are annotated with `#[instrument(skip(self), fields(slot, program_id))]` where applicable
**And** pipeline state transitions log at `info!` level (e.g., "Pipeline state: Backfilling -> Streaming")
**And** per-block processing logs at `debug!` level with slot number
**And** wire-level data (raw bytes, full JSON responses) logs at `trace!` level

**Given** a decode failure on an individual transaction
**When** the pipeline processes it
**Then** it logs at `warn!` level with: signature, slot, program_id, error details
**And** continues processing without interruption

**Given** >90% decode failures within a single chunk
**When** the pipeline detects this threshold
**Then** it logs at `error!` level with: program_id, chunk slot range, failure count, total count, and a message suggesting IDL version mismatch

**Given** the `StorageError` enum in `storage/mod.rs`
**When** I inspect it
**Then** it includes variants: `ConnectionFailed`, `DdlFailed`, `WriteFailed`, `CheckpointFailed`, `QueryFailed`
**And** it derives `thiserror::Error`
**And** `impl From<StorageError> for PipelineError` exists
**And** `impl From<StorageError> for ApiError` exists

**Given** API request handling
**When** a request is processed
**Then** the tracing middleware logs: method, path, status code, response time in ms
**And** each request gets a unique span for correlation

## Story 6.2: Core Module Tests (Decoder & Schema)

As a developer,
I want property-based tests for the decoder and unit tests for schema generation,
So that I can prove correctness of the two most critical modules with high confidence.

**Acceptance Criteria:**

**Given** the `tests/fixtures/` directory structure
**When** I inspect it
**Then** it contains: `tests/fixtures/idls/` (test IDL files), `tests/fixtures/accounts/` (serialized account data), `tests/fixtures/transactions/` (serialized transaction data), `tests/fixtures/expected/` (expected decode outputs)
**And** an "all types" IDL fixture exists that exercises every supported Borsh type variant

**Given** proptest roundtrip tests in `tests/decode_roundtrip.rs`
**When** they run
**Then** for each supported type: a value is generated, Borsh-serialized, decoded via SolarixDecoder, and the resulting JSON is asserted to match the original value
**And** u64 values > 2^53 are verified to be serialized as JSON strings (not numbers)
**And** u128/i128 values are verified as JSON strings

**Given** fuzz tests for the decoder
**When** arbitrary byte sequences of length 0..1024 are fed to `decode_instruction()` and `decode_account()`
**Then** the decoder NEVER panics -- it always returns `Err(DecodeError)` for invalid input
**And** this is enforced in CI

**Given** schema generation unit tests in `tests/schema_generation.rs`
**When** they run against a test PostgreSQL instance
**Then** they verify: IDL -> DDL generation produces correct CREATE statements, all type mappings produce valid PostgreSQL types, sanitize*identifier handles edge cases (digit-starting, empty, unicode, 63-byte truncation), schema naming produces correct `{name}*{prefix}` format, IF NOT EXISTS makes DDL idempotent (run twice without error)

**Given** unit tests in `#[cfg(test)] mod tests` within source files
**When** they run via `cargo test`
**Then** each module (config, types, idl, decoder, storage/schema) has inline unit tests for core logic
**And** all tests pass with `cargo clippy` and `cargo fmt` clean

## Story 6.3: Integration & API Tests

As a developer,
I want integration tests that verify the full pipeline end-to-end and API tests that verify all endpoints,
So that I can prove the system works as a whole, not just in isolated units.

**Acceptance Criteria:**

**Given** pipeline integration tests in `tests/pipeline_integration.rs`
**When** they run with LiteSVM
**Then** they: deploy a test Anchor program to LiteSVM, send test transactions, run the indexing pipeline against the local validator, and verify that decoded data appears correctly in PostgreSQL
**And** `litesvm` is used (NOT `solana-program-test` which is deprecated since Solana v3.1.0)

**Given** API integration tests in `tests/api_integration.rs`
**When** they run
**Then** they use `axum-test` (v18.7+) to: register a program (POST), verify it appears in list (GET), query instructions with filters, query accounts by type and pubkey, verify pagination (cursor and offset), verify aggregation endpoint, verify stats endpoint, verify error responses (404, 400)
**And** test the full request-response cycle including JSON envelope format

**Given** integration tests that share a PostgreSQL instance
**When** they run in CI
**Then** they use `--test-threads=1` to avoid schema conflicts between concurrent tests
**And** each test creates a unique schema (or cleans up after itself) to prevent test pollution

**Given** the test database
**When** integration tests set up
**Then** they connect to a test PostgreSQL instance (CI provides a `postgres` service), bootstrap system tables, and seed test data as needed
**And** test teardown drops any created schemas

## Story 6.4: CI Pipeline

As a developer,
I want an automated CI pipeline that validates code quality, runs all tests, measures coverage, and verifies the Docker build,
So that every commit is verified against the project's quality standards.

**Acceptance Criteria:**

**Given** the GitHub Actions workflow in `.github/workflows/ci.yml`
**When** a push or PR is created
**Then** 5 jobs run:

**Job 1: Lint**
**Given** the lint job
**When** it runs
**Then** it executes `cargo fmt -- --check` and `cargo clippy -- -D warnings`
**And** the job fails if either produces output

**Job 2: Unit Tests**
**Given** the unit test job
**When** it runs
**Then** it executes `cargo test --lib` (unit tests only, no integration)
**And** all tests pass

**Job 3: Integration Tests**
**Given** the integration test job
**When** it runs
**Then** it starts a PostgreSQL 16 service container
**And** it executes `cargo test --tests -- --test-threads=1`
**And** `DATABASE_URL` is set to the CI PostgreSQL instance

**Job 4: Coverage**
**Given** the coverage job
**When** it runs
**Then** it installs the `llvm-tools-preview` rustup component and `cargo-llvm-cov`
**And** it sets `PROPTEST_CASES=1000` for higher fuzz coverage in CI
**And** it generates a coverage report for core modules (decoder, storage, pipeline)

**Job 5: Docker Smoke Test**
**Given** the Docker smoke test job
**When** it runs
**Then** it executes `docker compose up --build -d`, waits for health check, calls `GET /health`, and verifies HTTP 200
**And** it runs `docker compose down` for cleanup

---
