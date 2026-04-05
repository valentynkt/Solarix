# Story 1.3: Docker Compose & Health Endpoint

Status: done

## Story

As a bounty judge,
I want to run `docker compose up` and have the full stack start, then verify it is healthy via a health endpoint,
so that I can confirm the project is runnable with zero manual setup.

## Acceptance Criteria

1. **AC1: Docker Compose starts the full stack**
   - **Given** Docker and Docker Compose are installed
   - **When** I run `docker compose up --build`
   - **Then** PostgreSQL 16 starts and becomes ready
   - **And** the Solarix binary starts, connects to PostgreSQL, and bootstraps system tables
   - **And** the entire stack is healthy within 60 seconds

2. **AC2: Multi-stage Dockerfile**
   - **Given** the Dockerfile
   - **When** I inspect it
   - **Then** it uses a multi-stage build: `rust:latest` for build stage, `debian:bookworm-slim` for runtime
   - **And** the runtime image does not contain Rust toolchain, source code, or build artifacts
   - **And** `.dockerignore` excludes `target/`, `.git/`, `tests/`, `docs/`

3. **AC3: Health endpoint returns detailed status**
   - **Given** the stack is running
   - **When** I call `GET /health`
   - **Then** the response returns HTTP 200 with JSON body containing: `status` ("healthy"/"unhealthy"), `database` (connection status), `uptime_seconds`, `version`
   - **And** if PostgreSQL is unreachable, the endpoint returns HTTP 503 with `status: "unhealthy"`

4. **AC4: main.rs starts the axum server**
   - **Given** `main.rs`
   - **When** I inspect it
   - **Then** after DB pool + bootstrap, it creates an axum Router with the health endpoint, binds to `config.api_host:config.api_port`, and serves requests
   - **And** structured logs are emitted for startup steps (config loaded, DB connected, tables bootstrapped, server listening)

5. **AC5: Signal handling for graceful shutdown**
   - **Given** the application is running
   - **When** it receives SIGTERM or SIGINT
   - **Then** it initiates graceful shutdown (stops accepting new connections, drains in-flight requests)
   - **And** logs the shutdown event

## Tasks / Subtasks

- [x] Task 1: Create `.dockerignore` (AC: #2)
  - [x] Exclude `target/`, `.git/`, `tests/`, `docs/`, `_bmad-output/`, `_bmad/`, `.claude/`, `.agents/`, `*.md` (except Cargo-relevant), `.env`
- [x] Task 2: Create `Dockerfile` (AC: #2)
  - [x] Build stage: `rust:latest`, copy `Cargo.toml`, `Cargo.lock`, `src/`, build release binary
  - [x] Runtime stage: `debian:bookworm-slim`, install `libssl3` and `ca-certificates` (needed by `native-tls`), copy binary from build stage
  - [x] Set `ENTRYPOINT` to the solarix binary
- [x] Task 3: Create `docker-compose.yml` (AC: #1)
  - [x] Define `postgres` service: `postgres:16`, health check, env vars for db/user/pass
  - [x] Define `solarix` service: build from `.`, depends_on postgres (condition: service_healthy), pass `DATABASE_URL` and other env vars
  - [x] Solarix health check: `curl -f http://localhost:3000/health` with interval/timeout/retries
- [x] Task 4: Implement health endpoint handler (AC: #3)
  - [x] Replace stub in `src/api/handlers.rs` with real handler that takes `State<AppState>`
  - [x] Check DB connectivity via `sqlx::query("SELECT 1").fetch_one(&pool)`
  - [x] Return JSON: `{ "status": "healthy"|"unhealthy", "database": "connected"|"disconnected", "uptime_seconds": N, "version": "0.1.0" }`
  - [x] Return HTTP 200 when healthy, 503 when unhealthy
- [x] Task 5: Create `AppState` and axum router (AC: #4)
  - [x] Define `AppState` struct in `src/api/mod.rs` with `pool: PgPool`, `start_time: std::time::Instant`
  - [x] Create `pub fn router(state: AppState) -> Router` that mounts `GET /health`
- [x] Task 6: Update `main.rs` to start axum server (AC: #4, #5)
  - [x] After DB bootstrap, create `AppState`, build router
  - [x] Bind `TcpListener` to `config.api_host:config.api_port`
  - [x] Use `axum::serve` with `with_graceful_shutdown` using `tokio::signal::ctrl_c()` or `SIGTERM`
  - [x] Log: "listening on {host}:{port}"
- [x] Task 7: Verify (AC: all)
  - [x] `cargo build` compiles
  - [x] `cargo clippy` passes
  - [x] `cargo fmt -- --check` passes
  - [x] `docker compose up --build` starts the full stack
  - [x] `curl http://localhost:3000/health` returns 200 with expected JSON

## Dev Notes

### Stories 1.1 and 1.2 State (Completed)

The current codebase has:

- `src/main.rs` — clap parse, tracing init, DB pool init, bootstrap system tables, placeholder for pipeline/API server
- `src/config.rs` — `Config` struct with 22 env var fields including `api_host` (default `0.0.0.0`) and `api_port` (default `3000`)
- `src/storage/mod.rs` — `init_pool(&Config) -> Result<PgPool, StorageError>` + `bootstrap_system_tables(&PgPool) -> Result<(), StorageError>`
- `src/api/mod.rs` — `ApiError` enum + submodule declarations (handlers, filters)
- `src/api/handlers.rs` — stub `health()` returning `"ok"`
- `Cargo.toml` — `axum = "0.8"`, `sqlx = "0.8"`, `tokio = "1" (full)`, `serde = "1"`, `serde_json = "1"`, `tracing = "0.1"`
- No Dockerfile, docker-compose.yml, or .dockerignore exist yet

### Review Findings from Story 1.1 (Relevant to this story)

- `log_format` case-sensitive with no validation — `"JSON"` or `"Pretty"` silently falls through. Not blocking for 1.3 but be aware.
- `channel_capacity = 0` would panic — not relevant to this story.
- `ApiError` missing `IntoResponse` impl — this story does NOT need it yet. The health handler returns `axum::Json` or `(StatusCode, Json)` directly, not `Result<_, ApiError>`.

### Dockerfile Pattern

Multi-stage build. The `native-tls` feature in sqlx requires OpenSSL libs at runtime.

```dockerfile
# Build stage
FROM rust:latest AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/solarix /usr/local/bin/solarix
ENTRYPOINT ["solarix"]
```

Key points:

- `curl` is needed in the runtime image for Docker health checks
- `libssl3` is for native-tls (sqlx dependency)
- `ca-certificates` needed for HTTPS connections to Solana RPC
- Do NOT use Alpine — `native-tls` links against OpenSSL, not musl's tls. Debian slim is the safe choice.
- Consider a cargo-chef pattern for layer caching, but it's optional for MVP — a simple COPY + build is fine

### .dockerignore

```
target/
.git/
tests/
docs/
_bmad-output/
_bmad/
.claude/
.agents/
.env
.env.*
!.env.example
*.md
!README.md
```

### docker-compose.yml

```yaml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: solarix
      POSTGRES_USER: solarix
      POSTGRES_PASSWORD: solarix
    ports:
      - "5432:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U solarix -d solarix"]
      interval: 5s
      timeout: 5s
      retries: 5

  solarix:
    build: .
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      DATABASE_URL: postgres://solarix:solarix@postgres:5432/solarix
      SOLARIX_API_HOST: "0.0.0.0"
      SOLARIX_API_PORT: "3000"
      SOLARIX_LOG_FORMAT: "pretty"
    ports:
      - "3000:3000"
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 10s
      timeout: 5s
      retries: 5
      start_period: 30s
```

Key points:

- `depends_on` with `condition: service_healthy` ensures PostgreSQL is ready before Solarix starts
- `SOLARIX_LOG_FORMAT: "pretty"` for readable logs during Docker development
- `start_period: 30s` gives the Rust binary time to compile (only relevant during build, but provides buffer for cold starts)
- The postgres hostname is `postgres` (the service name) inside the compose network

### Health Endpoint Implementation

The health handler needs access to the DB pool (to check connectivity) and a start time (for uptime).

**AppState struct** in `src/api/mod.rs`:

```rust
use std::sync::Arc;
use std::time::Instant;

use sqlx::PgPool;

pub struct AppState {
    pub pool: PgPool,
    pub start_time: Instant,
}
```

**Router** in `src/api/mod.rs`:

```rust
use axum::{Router, routing::get};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .with_state(state)
}
```

**Health handler** in `src/api/handlers.rs`:

```rust
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::AppState;

pub async fn health(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    let db_ok = sqlx::query("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .is_ok();

    let uptime = state.start_time.elapsed().as_secs();
    let version = env!("CARGO_PKG_VERSION");

    let status = if db_ok { "healthy" } else { "unhealthy" };
    let db_status = if db_ok { "connected" } else { "disconnected" };
    let http_status = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        http_status,
        Json(json!({
            "status": status,
            "database": db_status,
            "uptime_seconds": uptime,
            "version": version,
        })),
    )
}
```

Key points:

- Use `Arc<AppState>` as the state type since `PgPool` is already `Clone`, but `Arc` is the standard pattern and allows adding non-Clone fields later
- `env!("CARGO_PKG_VERSION")` reads version from Cargo.toml at compile time — always accurate
- The handler returns a tuple `(StatusCode, Json<Value>)` — axum implements `IntoResponse` for this natively
- DB check uses `sqlx::query("SELECT 1")` — minimal overhead, proves connectivity
- No `unwrap()` or `expect()` — `is_ok()` handles the result

### main.rs Updates

After DB bootstrap, add the axum server:

```rust
use std::sync::Arc;
use std::time::Instant;

use tokio::net::TcpListener;

// ... existing code ...

let start_time = Instant::now();
let state = Arc::new(solarix::api::AppState {
    pool,
    start_time,
});
let app = solarix::api::router(state);

let addr = format!("{}:{}", config.api_host, config.api_port);
let listener = TcpListener::bind(&addr).await.map_err(|e| {
    error!(error = %e, addr = %addr, "failed to bind listener");
    e
})?;

info!(addr = %addr, "listening");

axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|e| {
        error!(error = %e, "server error");
        e
    })?;

info!("shutdown complete");
Ok(())
```

**Graceful shutdown signal:**

```rust
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .ok();
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .ok()
            .and_then(|mut s| {
                use futures_util::FutureExt;
                Some(s.recv().boxed())
            });
        // simplified: just wait for ctrl_c if unix signal setup fails
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { tracing::info!("received SIGINT, shutting down"); },
        _ = terminate => { tracing::info!("received SIGTERM, shutting down"); },
    }
}
```

**Simpler alternative for `shutdown_signal`** (preferred — avoids `futures_util` dep):

```rust
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate()
        ).ok();

        tokio::select! {
            _ = ctrl_c => { tracing::info!("received SIGINT, shutting down"); },
            _ = async {
                if let Some(ref mut s) = sigterm {
                    s.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => { tracing::info!("received SIGTERM, shutting down"); },
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        tracing::info!("received SIGINT, shutting down");
    }
}
```

The `shutdown_signal` function can be defined directly in `main.rs` since it's entry-point logic. No need to put it in a module.

**IMPORTANT:** `tokio::signal::unix` is only available on Unix platforms. Use `#[cfg(unix)]` to guard. Docker runs Linux, so SIGTERM support is essential for `docker compose down` to gracefully stop the container.

### Files Modified / Created by This Story

| File                  | Action | Purpose                                         |
| --------------------- | ------ | ----------------------------------------------- |
| `.dockerignore`       | Create | Exclude build artifacts and non-essential files |
| `Dockerfile`          | Create | Multi-stage build for minimal runtime image     |
| `docker-compose.yml`  | Create | PostgreSQL + Solarix full stack                 |
| `src/api/mod.rs`      | Modify | Add `AppState` struct + `router()` function     |
| `src/api/handlers.rs` | Modify | Replace health stub with real handler           |
| `src/main.rs`         | Modify | Add axum server startup + graceful shutdown     |

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` — use `?` with `map_err` or `.ok()` / `.is_ok()`
- NO `println!` — use `tracing` macros
- NO Alpine-based Docker images — `native-tls` needs glibc/OpenSSL
- NO hardcoded database URLs in Dockerfile or docker-compose.yml environment — use variables that match `.env.example`
- NO `EXPOSE` without matching `ports:` in compose (keep consistent)
- Do NOT add `IntoResponse` impl for `ApiError` yet — that belongs to Story 5.1
- Do NOT modify `StorageError`, `Config`, or any module stubs beyond what this story requires

### Required Imports for Modified Files

**`src/api/mod.rs`** — add:

```rust
use std::sync::Arc;
use std::time::Instant;

use axum::{Router, routing::get};
use sqlx::PgPool;
```

**`src/api/handlers.rs`** — replace stub with:

```rust
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::AppState;
```

**`src/main.rs`** — add:

```rust
use std::sync::Arc;
use std::time::Instant;

use tokio::net::TcpListener;
```

### Testing Notes

- No unit tests for the health endpoint in this story — the handler is simple enough that integration testing via Docker proves correctness
- Verification: `docker compose up --build` then `curl http://localhost:3000/health`
- The existing `tests/bootstrap_test.rs` continues to work as-is
- Future Story 6.3 will add `axum-test` based integration tests for all API endpoints

### Project Structure Notes

All file paths match the architecture document. New files (`Dockerfile`, `docker-compose.yml`, `.dockerignore`) are at the project root per the architecture spec in `project-structure-boundaries.md`.

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-1-project-foundation-first-boot.md#Story 1.3]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Infrastructure & Deployment]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md#Complete Project Directory Structure]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Shared State]
- [Source: _bmad-output/planning-artifacts/prd.md#Deployment]
- [Source: _bmad-output/implementation-artifacts/1-1-project-scaffolding-and-configuration.md]
- [Source: _bmad-output/implementation-artifacts/1-2-database-connection-and-system-table-bootstrap.md]

### Review Findings

- [x] [Review][Patch] Missing SOLANA_RPC_URL in docker-compose.yml — defaults to mainnet silently [docker-compose.yml]
- [x] [Review][Patch] No postgres volume — data lost on `docker compose down` [docker-compose.yml]
- [x] [Review][Patch] SIGTERM handler silently swallows registration failure [src/main.rs:92]
- [x] [Review][Patch] Uptime undercounts — start_time captured after DB bootstrap [src/main.rs:32]
- [x] [Review][Patch] Health check SELECT 1 has no timeout — false unhealthy under pool saturation [src/api/handlers.rs:11]
- [x] [Review][Defer] `rust:latest` / `debian:bookworm-slim` unpinned — non-reproducible builds [Dockerfile:2,9] — deferred, pre-existing pattern; pin when CI pipeline is set up (Story 6.4)
- [x] [Review][Defer] No restart policy on compose services [docker-compose.yml] — deferred, add when pipeline orchestrator handles crash recovery (Story 4.3)
- [x] [Review][Defer] Hardcoded credentials in docker-compose.yml [docker-compose.yml:5-7] — deferred, dev-only convenience; address when deployment docs are written (Story 7.1)
- [x] [Review][Defer] ApiError missing IntoResponse impl [src/api/mod.rs:17-27] — deferred, explicitly scoped for Story 5.1

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

### Completion Notes List

- All 7 tasks completed successfully
- Docker compose stack verified: `docker compose up --build` starts PostgreSQL 16 + Solarix, health endpoint returns 200 with `{"status":"healthy","database":"connected","uptime_seconds":N,"version":"0.1.0"}`
- Graceful shutdown via SIGINT/SIGTERM implemented using `tokio::select!` with `#[cfg(unix)]` guard
- `AppState` uses `Arc<AppState>` pattern with `PgPool` + `Instant` for uptime tracking
- All lint checks pass: `cargo build`, `cargo clippy`, `cargo fmt --check`

### Change Log

- 2026-04-05: Implemented Story 1.3 — Docker Compose & Health Endpoint

### File List

- `.dockerignore` (created)
- `Dockerfile` (created)
- `docker-compose.yml` (created)
- `src/api/mod.rs` (modified — added AppState, router())
- `src/api/handlers.rs` (modified — replaced stub with real health handler)
- `src/main.rs` (modified — added axum server startup + graceful shutdown)
