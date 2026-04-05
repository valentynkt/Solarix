# Epic 1: Project Foundation & First Boot

Operator can `docker compose up`, the system starts, connects to PostgreSQL, creates system tables, and responds on the health endpoint -- proving the project is real and runnable.

## Story 1.1: Project Scaffolding & Configuration

As a developer,
I want a properly initialized Rust project with all dependencies, lints, formatting config, and a typed configuration system,
So that all subsequent development starts from a solid, consistent foundation.

**Acceptance Criteria:**

**Given** a fresh checkout of the repository
**When** I run `cargo build`
**Then** the project compiles successfully with all dependencies resolved
**And** `Cargo.toml` includes all production dependencies (axum, sqlx, tokio, tokio-util, tracing, tracing-subscriber, clap, dotenvy, serde, serde_json, thiserror, governor, backon, reqwest, chainparser fork, anchor-lang-idl-spec)
**And** `Cargo.toml` lints section enforces: `unsafe_code = "forbid"`, `unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"`
**And** `rustfmt.toml` contains `edition = "2021"` and `max_width = 100`
**And** `clippy.toml` contains `allow-expect-in-tests = true`

**Given** the project is built
**When** I run `cargo clippy` and `cargo fmt -- --check`
**Then** both pass with zero warnings or errors

**Given** the `Config` struct in `src/config.rs`
**When** I inspect its fields
**Then** it derives `clap::Parser` with all 22 env vars (including `SOLANA_RPC_URL`, `SOLANA_WS_URL`, `DATABASE_URL`, `SOLARIX_RPC_RPS`, `SOLARIX_BACKFILL_CHUNK_SIZE`, `SOLARIX_START_SLOT`, `SOLARIX_INDEX_FAILED_TXS`, API host/port, log level, etc.)
**And** each field has a sensible default value and `env` attribute
**And** `src/lib.rs` declares all module paths (config, types, idl, decoder, pipeline, storage, api) -- non-config/non-types modules contain minimal stub implementations (empty structs/trait placeholders) sufficient to compile
**And** `.env.example` documents every configurable variable with descriptions

## Story 1.2: Database Connection & System Table Bootstrap

As an operator,
I want the system to connect to PostgreSQL and auto-create its system tables on startup,
So that no manual database setup or migration tooling is required.

**Acceptance Criteria:**

**Given** a running PostgreSQL instance and a valid `DATABASE_URL`
**When** the application starts
**Then** it creates a connection pool via `sqlx::PgPool` with configurable pool size
**And** it creates the `programs` table in the `public` schema with `IF NOT EXISTS` (columns: `program_id TEXT PRIMARY KEY`, `program_name TEXT`, `schema_name TEXT`, `idl_hash TEXT`, `idl_source TEXT`, `status TEXT NOT NULL DEFAULT 'registered'`, `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`)
**And** it creates the `indexer_state` table in the `public` schema with `IF NOT EXISTS` (columns: `program_id TEXT PRIMARY KEY REFERENCES programs(program_id)`, `status TEXT NOT NULL`, `last_processed_slot BIGINT`, `last_heartbeat TIMESTAMPTZ`, `error_message TEXT`, `total_instructions BIGINT DEFAULT 0`, `total_accounts BIGINT DEFAULT 0`)
**And** DDL is executed via `sqlx::raw_sql()` (not compile-time macros)

**Given** an invalid or unreachable `DATABASE_URL`
**When** the application starts
**Then** it logs a fatal error with the connection details (excluding password) and exits with a non-zero status code

**Given** the system tables already exist from a previous run
**When** the application starts again
**Then** bootstrap completes without errors (`IF NOT EXISTS` is idempotent)

## Story 1.3: Docker Compose & Health Endpoint

As a bounty judge,
I want to run `docker compose up` and have the full stack start, then verify it is healthy via a health endpoint,
So that I can confirm the project is runnable with zero manual setup.

**Acceptance Criteria:**

**Given** Docker and Docker Compose are installed
**When** I run `docker compose up --build`
**Then** PostgreSQL 16 starts and becomes ready
**And** the Solarix binary starts, connects to PostgreSQL, and bootstraps system tables
**And** the entire stack is healthy within 60 seconds

**Given** the Dockerfile
**When** I inspect it
**Then** it uses a multi-stage build: `rust:latest` for build stage, `debian:bookworm-slim` for runtime
**And** the runtime image does not contain Rust toolchain, source code, or build artifacts
**And** `.dockerignore` excludes `target/`, `.git/`, `tests/`, `docs/`

**Given** the stack is running
**When** I call `GET /health`
**Then** the response returns HTTP 200 with JSON body containing: `status` ("healthy"/"unhealthy"), `database` (connection status), `uptime_seconds`, `version`
**And** if PostgreSQL is unreachable, the endpoint returns HTTP 503 with `status: "unhealthy"`

**Given** `main.rs`
**When** I inspect it
**Then** it parses `Config` via clap, initializes tracing subscriber (structured JSON), creates DB pool, bootstraps system tables, starts the axum server, and sets up signal handlers for SIGTERM/SIGINT
**And** structured logs are emitted for startup steps (config loaded, DB connected, tables bootstrapped, server listening)

---
