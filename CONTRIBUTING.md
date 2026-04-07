# Contributing to Solarix

Thanks for your interest! This document covers the things you need to know to
get a PR through the Solarix CI gate on the first try.

## Table of Contents

- [Local CI reproduction](#local-ci-reproduction)
- [Import ordering convention](#import-ordering-convention)
- [Filing a CI failure](#filing-a-ci-failure)

## Local CI reproduction

Solarix runs its CI on GitHub Actions (`.github/workflows/ci.yml`). Every job
in the pipeline has a local-cargo equivalent so you can verify before pushing.

| CI job             | Local equivalent                                                                                                                                    |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `lint`             | `cargo fmt -- --check && cargo clippy --release --all-targets -- -D warnings`                                                                       |
| `unit`             | `cargo test --release --lib`                                                                                                                        |
| `integration`      | `cargo test --release --tests` (or `cargo test --release --tests --features integration` after Story 6.5 ships the harness)                         |
| `coverage`         | `cargo llvm-cov --release --lib --summary-only`                                                                                                     |
| `fuzz-smoke`       | `cargo +nightly fuzz run decode_instruction -- -max_total_time=60` (requires Story 6.4 fuzz target)                                                 |
| `security`         | `cargo audit && cargo deny check && gitleaks detect --source . --config .gitleaks.toml --no-banner --redact`                                        |
| `docker-smoke`     | `docker compose down -v && docker compose up --build -d && curl --retry 12 --retry-delay 5 --retry-connrefused --fail http://localhost:3000/health` |
| `msrv`             | `cargo +1.88 build --release`                                                                                                                       |
| `toolchain-matrix` | `cargo +stable build --release` and `cargo +beta build --release`                                                                                   |

### Required tools

Most jobs only need `cargo`. A few jobs install extra tooling in CI that you may
want to pre-install locally:

```bash
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
