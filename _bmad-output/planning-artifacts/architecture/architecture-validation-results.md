# Architecture Validation Results

## Coherence Validation

**Decision Compatibility:** PASS

- All crate versions target Solana SDK v3.x ecosystem
- `sqlx` 0.8.x + `axum` + `tokio` fully compatible (all tower-based)
- `thiserror` everywhere — no mixed error philosophy
- Single crate structure eliminates inter-crate version conflicts
- `backon` replaces unmaintained `backoff` (RUSTSEC-2025-0012)

**Pattern Consistency:** PASS

- `snake_case` everywhere: Rust code, DB columns, JSON fields, API query params
- All DB access via `sqlx::query()` (runtime) — no compile-time macros fighting dynamic DDL
- DDL via `sqlx::raw_sql()` — bypasses prepared statements as required
- Trait boundaries align with module boundaries

**Structure Alignment:** PASS

- 14 source files map cleanly to four-layer pipeline + IDL module + shared types
- Dependency graph is acyclic: `types` ← `idl` ← `decoder` ← `pipeline` ← `storage`, `api` reads from `storage` + `idl`

## Requirements Coverage

All 41 functional requirements mapped to specific source files. All non-functional requirements (performance, reliability, security, code quality, deployment) architecturally supported.

## Gap Analysis

**Resolved gaps (from adversarial review):**

- Schema naming collision: disambiguated with program_id prefix
- Workspace/crate terminology: unified to "single crate with modules"
- `backoff` unmaintained: replaced with `backon`
- Nightly rustfmt options: removed, enforced by convention
- AccountSource trait: added as 4th trait boundary
- Shared types module: `types.rs` added
- Dual checkpoint: both tables documented with clear purposes
- u64 overflow: guard documented in Data Architecture
- GapDetected: reclassified from error to state transition
- System tables: canonicalized to `programs` + `indexer_state`

**Known limitations (acceptable for bounty):**

- `ProgramRegistry` write lock during registration briefly blocks API reads
- proptest strategy for decoder testing requires hand-crafted fixtures for known IDLs (generating arbitrary type-conformant byte sequences is complex)
- WS reconnection and concurrent dedup (Option C) have no dedicated integration test files — tested implicitly through pipeline integration
- `governor` v0.10.2 needs compilation verification (docs.rs build failed)

## Architecture Completeness Checklist

- [x] Project context analyzed, scale and complexity assessed
- [x] 6 hard technical constraints identified
- [x] 7 cross-cutting concerns mapped (including backpressure, decode failure detection)
- [x] 30+ architectural decisions documented with rationale across 7 categories
- [x] 15 crate dependencies specified with version notes
- [x] 5 error enums with classification (retryable/skip/fatal) + error conversion pattern
- [x] Implementation patterns: naming, structure, format, process, tooling
- [x] Complete directory structure: 14 source files + tests + config
- [x] 4 trait interfaces as module boundary contracts
- [x] 41 FRs mapped to specific files
- [x] Development workflow documented (local, test, docker, CI)
- [x] Schema naming collision resolved
- [x] Checkpoint architecture clarified (two-tier)
- [x] Shared state ownership documented (ProgramRegistry)

## Readiness Assessment

**Status:** READY FOR IMPLEMENTATION
**Confidence:** HIGH — grounded in 7,200 lines of research across 10 agent reports, then adversarially reviewed and externally verified

**Key Strengths:**

- Runtime dynamism end-to-end (decode, schema, API)
- Developer velocity optimized: single crate, flat modules, 80/20 crate leverage
- Trait boundaries enable testing without network/DB
- Research-backed decisions with quantified risk
- All P0/P1 issues from adversarial review resolved

**Post-MVP Enhancement Areas:**

- Geyser/gRPC data source (trait ready)
- Schema evolution (ALTER TABLE on IDL changes)
- GraphQL API, Prometheus metrics
- Historical account state tracking

## Critical Path Risk: Decoder Strategy

**The chainparser fork is the highest-risk, least-predictable component in the entire architecture.** It requires upgrading someone else's 2,400 LOC Rust crate from solana-sdk 1.18 -> 3.x, adding instruction argument deserialization, and fixing COption for Defined inner types. The repo has only 7 commits and appears dormant. This is exploratory work with unknown unknowns.

**This needs dedicated deep research before implementation begins.** Specifically:

1. **Can the fork actually be done?** Clone chainparser, attempt `solana-sdk` v3 upgrade, measure actual breakage. The research estimated "minimal usage" but this is unverified.
2. **Are there alternative crates?** The ecosystem moves fast. New IDL decoders may have appeared since the research was conducted (April 2026). Search for alternatives.
3. **Is a minimal custom decoder faster?** A custom decoder covering the top 10 types (~330 LOC, ~95% of real programs) avoids the fork entirely. This may be the lower-risk path.
4. **Hybrid approach?** Start with custom decoder for MVP, attempt fork in parallel. Use whichever succeeds first behind the `SolarixDecoder` trait.

**The `SolarixDecoder` trait abstraction exists precisely for this risk** — the implementation can be swapped without touching any other module. This is the architectural insurance policy.

**Recommendation:** Before committing to a decoder strategy, run a focused technical spike (research + prototype). The rest of the architecture is not blocked — scaffolding, config, storage, API can all proceed in parallel.

## Implementation Handoff

**First implementation priorities:**

1. `cargo init` + Cargo.toml (all deps including `backon` not `backoff`) + rustfmt.toml + clippy.toml + lints
2. `src/types.rs` — shared data types for pipeline stages
3. `config.rs` — Config struct with 22 env vars
4. Docker Compose + Dockerfile + .dockerignore skeleton
5. `storage/mod.rs` — DB pool + system table bootstrap (`programs`, `indexer_state`)
6. `storage/schema.rs` — DDL generator with disambiguated schema naming
7. **Parallel: decoder spike** — attempt chainparser fork OR prototype custom decoder
