# Starter Template Evaluation

## Primary Technology Domain

Rust backend service (CLI binary + REST API + PostgreSQL). No applicable starter template — single crate initialized from scratch with `cargo init`.

## Technical Preferences (Confirmed from Research)

**Language & Runtime:** Rust (stable), async via Tokio runtime

**Project Structure:** Single crate with logical module boundaries:

- `solarix` (binary crate) — CLI entry point, config, main orchestration
- Internal library modules for: IDL management, decoder, schema generator, pipeline, storage, API

**Build Tooling:** `cargo` with crate-level clippy/fmt config, Docker multi-stage build

**Testing Framework:** Built-in `cargo test` + `proptest` (property-based) + `litesvm` (integration) + `axum-test` (API)

**Code Organization:** Four-layer pipeline (Read -> Decode -> Store -> Serve), trait abstractions at layer boundaries

**Development Experience:** `cargo watch` for hot reload, `tracing` for structured logging, `.env` via `dotenvy`

## Initialization Command

```bash
cargo init --name solarix
```

## Starter Options Considered

| Option                         | Verdict      | Rationale                                                                                    |
| ------------------------------ | ------------ | -------------------------------------------------------------------------------------------- |
| Single crate from scratch      | **Selected** | Full control, no unnecessary abstractions, matches 80/20 philosophy                          |
| Carbon framework as base       | Rejected     | Compile-time codegen only, fights runtime dynamic differentiator                             |
| Generic Rust service templates | Rejected     | Add unnecessary opinions, Solarix's structure is unique enough to warrant custom scaffolding |

## Fork Management: chainparser

The chainparser fork is managed as a Git dependency in `Cargo.toml`:

```toml
[dependencies]
chainparser = { git = "https://github.com/valentynkit/chainparser", branch = "solarix-v3" }
```

The fork requires 3 changes: solana-sdk v3 upgrade, instruction arg deserialization, COption fix for Defined inner types. If the fork fails, swap to custom decoder behind the `SolarixDecoder` trait — no other code changes needed.
