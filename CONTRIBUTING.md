# Contributing to Solarix

Thanks for your interest! This document covers the things you need to know to
get a PR through the Solarix CI gate on the first try.

## Table of Contents

- [Local CI reproduction](#local-ci-reproduction)
- [Integration tests](#integration-tests)
- [Import ordering convention](#import-ordering-convention)
- [Filing a CI failure](#filing-a-ci-failure)

## Local CI reproduction

Solarix runs its CI on GitHub Actions (`.github/workflows/ci.yml`). Every job
in the pipeline has a local-cargo equivalent so you can verify before pushing.

| CI job             | Local equivalent                                                                                                                                    |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `lint`             | `cargo fmt -- --check && cargo clippy --release --all-targets -- -D warnings`                                                                       |
| `unit`             | `cargo test --release --lib`                                                                                                                        |
| `integration`      | `cargo test --release --tests --features integration` (Story 6.5 harness — see [Integration tests](#integration-tests) below)                       |
| `coverage`         | `cargo llvm-cov --release --lib --summary-only`                                                                                                     |
| `fuzz-smoke`       | `cargo +nightly fuzz run decode_instruction -- -max_total_time=60` (requires Story 6.4 fuzz target)                                                 |
| `security`         | `cargo audit && cargo deny check advisories bans sources && gitleaks detect --source . --config .gitleaks.toml`                                     |
| `docker-smoke`     | `docker compose down -v && docker compose up --build -d && curl --retry 12 --retry-delay 5 --retry-connrefused --fail http://localhost:3000/health` |
| `msrv`             | `cargo +1.88 build --release`                                                                                                                       |
| `toolchain-matrix` | `cargo +stable build --release` and `cargo +beta build --release`                                                                                   |

### Required tools

Most jobs only need `cargo`. A few jobs install extra tooling in CI that you may
want to pre-install locally:

```bash
# MSRV toolchain (matches `rust-toolchain.toml`)
rustup toolchain install 1.88
rustup component add --toolchain 1.88 rustfmt clippy

# Coverage
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked

# Security
cargo install cargo-audit cargo-deny --locked
# gitleaks: `brew install gitleaks` (macOS) or see https://github.com/gitleaks/gitleaks

# Fuzz
cargo install cargo-fuzz --locked   # run fuzz targets with `+nightly`

# Docker smoke
# Requires Docker Desktop or docker engine with compose plugin v2.
```

### Which toolchain?

The repository pins MSRV via `rust-toolchain.toml`. When you `cd` into the repo
your `cargo` will use that channel automatically. If you want to test against
stable or beta (matching the `toolchain-matrix` job), use `cargo +stable` /
`cargo +beta` explicitly or set `RUSTUP_TOOLCHAIN=stable`.

## Integration tests

Solarix's integration tests boot real `postgres:16-alpine` containers via
[testcontainers-modules](https://crates.io/crates/testcontainers-modules) and
exercise the writer, schema generator, decoder, registration path, and filter
SQL builder against the live PostgreSQL catalog. They are gated behind the
`integration` cargo feature so the default `cargo test` stays fast and
docker-free.

**Local run:**

```bash
cargo test --release --tests --features integration
```

Requirements:

- Docker Desktop or docker engine running locally (the harness spawns one
  ephemeral container per test, ~2-3 s warm cache).
- The MSRV toolchain (1.88) — see the [Required tools](#required-tools)
  section.

The canonical pool fixture lives at `tests/common/postgres.rs::with_postgres`
and `with_postgres_returning`. New integration tests should reuse it instead
of rolling their own connection setup. The harness:

- Spawns a fresh container per call (per-test isolation, parallelism-safe).
- Calls `solarix::storage::bootstrap_system_tables` so every closure starts
  on a clean, bootstrapped DB.
- Drops the container automatically when the closure returns or panics.

The non-mainnet integration suite is budgeted to run end-to-end in **under
90 seconds** on a developer laptop with the docker image cached.

**Mainnet smoke test (nightly-only):**

`tests/mainnet_smoke.rs` is gated behind the separate `mainnet-smoke` feature
(declared in `Cargo.toml` as `mainnet-smoke = ["integration"]` so it pulls
in the testcontainers harness transitively). It is run only by
`.github/workflows/nightly.yml` against `https://api.mainnet-beta.solana.com`,
not by per-PR CI. Local invocation:

```bash
cargo test --release --features mainnet-smoke -- mainnet_smoke
```

The nightly workflow uses `continue-on-error: true` and posts a PR comment
on failure rather than turning the cron red.

## Import ordering convention

Rust imports in this repo are grouped by source, with a blank line between
groups:

```rust
// 1. std
use std::collections::HashMap;
use std::sync::Arc;

// 2. external crates
use axum::Router;
use sqlx::PgPool;

// 3. internal crate (use `crate::...`)
use crate::decoder::SolarixDecoder;
use crate::types::DecodedInstruction;
```

**This convention is NOT enforced by CI.** The nightly rustfmt options
(`group_imports = "StdExternalCrate"`, `imports_granularity = "Crate"`) that
would enforce it automatically require a nightly toolchain, and per
[ADR-0002 § D4](docs/adr/0002-ci-pipeline.md) we decided not to add nightly to
the `lint` job just for this one check. Reviewer discipline catches drift.

## Logging

Solarix uses `tracing` macros exclusively — never `println!` / `eprintln!`.
Every `pub async fn` in `src/pipeline/`, `src/api/handlers.rs`, `src/idl/`,
`src/registry.rs`, and `src/storage/writer.rs` carries `#[tracing::instrument]`
with required field names. Every `warn!` / `error!` in the pipeline modules
must carry `program_id` — enforced by `tests/log_levels.rs`.

See [docs/operating-solarix.md](docs/operating-solarix.md#structured-logging-conventions)
for the full field glossary, enforced rules, and `jq` recipes.

When adding a new async fn in the tracing scope: add an `#[tracing::instrument]`
attribute with an explicit `name = "..."` (dotted convention: `module.function`),
`skip(...)` for large structs, `level = "debug"` for hot paths or `"info"` for
lifecycle, and `err(Display)` on fallible functions. Run `cargo test --test
instrument_coverage` and `cargo test --test log_levels` before opening a PR.

## Filing a CI failure

If a CI job fails on your PR and the failure message isn't obvious:

1. Check the job log for the first `error:` line. Most clippy and compile
   failures are self-explanatory.
2. Two jobs upload artifacts on failure for offline inspection:
   - `docker-smoke-logs` (the `solarix` container's log output, 7-day retention)
   - `lcov-info` (always uploaded, 14-day retention; useful for spotting
     coverage drops even though we don't enforce a delta gate yet)
3. Reproduce the failing command locally using the table above.
4. Soft-gated jobs (`fuzz-smoke`, the `/ready` and `/metrics` checks inside
   `docker-smoke`, and the entire `nightly-mainnet-smoke` workflow) become
   hard gates when their dependency story lands. If a newly landed story
   unexpectedly reds one of these, the fix is usually to open an issue
   tracking it rather than to revert the CI change.
5. If the failure is transient (e.g., a flaky third-party service),
   re-run the failed job from the GitHub Actions UI. Do NOT paper over flake
   by adding `continue-on-error: true` — only `mainnet-smoke` and the `beta`
   toolchain matrix entry are allowed to flake, and those decisions are
   documented in ADR-0002.

Thanks for contributing!
