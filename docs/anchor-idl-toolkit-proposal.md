# Anchor IDL Toolkit — Ecosystem Extraction Proposal

**Status:** Draft · **Date:** 2026-04-08
**Related:** [Solarix README](../README.md) · [Architecture](./architecture.md) · [Runbook](./runbook.md)

---

## Abstract

Solarix is a universal Solana indexer that generates PostgreSQL schemas and decodes on-chain data from any Anchor IDL at runtime. During its development, several self-contained components were written to fill gaps in the existing Rust ecosystem — most notably a runtime Borsh decoder driven by `anchor-lang-idl-spec` 0.1.0, because no maintained library currently provides this capability.

This document proposes extracting those components into reusable crates under the working name **`anchor-idl-toolkit`**, with Solarix serving as the production reference implementation. It catalogs the ecosystem gaps, proposes concrete crate boundaries, outlines a staged extraction path, and explicitly notes which Solarix components are intentionally **not** proposed for extraction.

The intent is to fill the layer between _"raw IDL JSON on disk"_ and _"typed decoded data in memory"_ that currently requires every Anchor consumer — indexers, block explorers, wallets, debuggers, analytics pipelines — to reimplement locally.

---

## 1. Problem Statement

A healthy Anchor ecosystem needs a layer that answers the question:

> _"Given a program ID and an RPC endpoint, give me back decoded instructions, accounts, and events as typed data."_

Today, that layer does not exist as a maintained Rust library.

### 1.1 Concrete evidence of the gap

| Candidate              | State               | What it covers                                                                                                           | What it does not cover                                                                                                                                                                      |
| ---------------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `anchor-lang`          | Actively maintained | Framework for **writing** programs. Discriminators and type layouts are derived at compile time from `#[derive]` macros. | Runtime decoding of an IDL supplied at runtime — out of scope by design.                                                                                                                    |
| `anchor-syn`           | Actively maintained | Proc-macro helpers for the Anchor framework.                                                                             | Compile-time only. Cannot decode an arbitrary IDL supplied at runtime.                                                                                                                      |
| `anchor-cli`           | Actively maintained | The `anchor idl fetch` and `anchor idl parse` subcommands work as advertised.                                            | Only exposed as a binary. Internal decoding logic is not published as a library crate.                                                                                                      |
| `chainparser`          | **Dormant**         | An early attempt at runtime IDL decoding that the ecosystem has outgrown.                                                | Last commit September 2024. Seven commits total. Pinned to `solana-sdk` 1.18 (current is 3.x). Known correctness gaps: `u128`, `COption<Defined>` inner types, floating-point NaN handling. |
| `anchor-lang-idl-spec` | Actively maintained | Canonical `Idl`, `IdlInstruction`, `IdlAccount`, `IdlEvent`, `IdlType` type definitions.                                 | Types only — no decoder, no fetcher, no schema generator.                                                                                                                                   |
| `borsh`                | Actively maintained | Serialization primitives.                                                                                                | Knows nothing about Anchor discriminators, `idl.instructions[]`, `idl.accounts[]`, or `idl.events[]`.                                                                                       |

### 1.2 The downstream tax

Every team building on top of Anchor today must choose one of:

1. **Fork `chainparser`** and fix the known bugs locally. At least four projects are known to be doing this independently.
2. **Write a decoder from scratch.** Non-trivial — see §1.3 below for the long tail of correctness hazards.
3. **Skip runtime decoding entirely** and store raw bytes, surfacing opaque transaction blobs to end users. This is the most common choice.

The result is a visible productivity tax on every downstream consumer and a noticeably worse experience for end users who see `0xa1b2c3…` where they should see `route(amount_in: 16053520, min_amount_out: 0)`.

### 1.3 Why a correct runtime decoder is harder than it looks

Building a production-quality runtime IDL decoder requires solving a long tail of subtle problems:

- **Anchor v0.30 IDL format changes.** Explicit discriminator bytes replaced the legacy `SHA256("global:<name>")[..8]` derivation. A correct decoder must support both.
- **`COption<T>` tagging.** Anchor's `COption` uses a 4-byte `u32` tag, not Rust's 1-byte `Option` tag. A decoder that dispatches generically fails silently here.
- **Event-CPI self-calls.** Modern Anchor programs (Jupiter V6, Drift V2, Phoenix, parts of Marginfi v2, Kamino) emit events via `emit_cpi!()`, producing a self-CPI instruction whose data begins with the fixed `EVENT_IX_TAG_LE` constant (`0x1d9acb512ea545e4` little-endian). A decoder that only walks `idl.instructions[]` will report these as unknown discriminators and silently skip the majority of on-chain activity for these programs.
- **Reserved column names.** IDL field names like `data`, `slot`, `signature`, `id` collide with SQL reserved words and with Solarix-internal columns. A naïve schema generator produces broken DDL.
- **Recursive and generic type definitions.** Nested `Defined` references, generic type parameters, and self-referential structs require a bounded-depth recursive decoder with a shared type registry.
- **Floating-point NaN handling.** `f32` and `f64` fields with `NaN` values cannot be JSON-encoded without custom handling.
- **u64 → SQL BIGINT overflow.** PostgreSQL `BIGINT` is signed 64-bit; Anchor `u64` is unsigned. Values above `i64::MAX` must be preserved in JSONB and nulled in any promoted typed column, not truncated.

These are not theoretical. Each of the issues above is solved in Solarix today, with regression tests encoding the solution. Extracting that work removes the tax on every future consumer.

---

## 2. Proposed Crates

This proposal defines **three new crates** that together form a cohesive toolkit. They are designed to be adopted independently — a project can use only the decoder without pulling in the PostgreSQL layer, for example.

### 2.1 `anchor-idl-decoder` — flagship

A runtime Borsh decoder driven by `anchor-lang-idl-spec`.

**Public API sketch:**

```rust
pub struct IdlDecoder {
    // owns the parsed Idl
    // caches pre-indexed discriminator lookup tables
}

impl IdlDecoder {
    pub fn new(idl: Idl) -> Self;

    pub fn decode_instruction(&self, data: &[u8])
        -> Result<DecodedInstruction, DecodeError>;

    pub fn decode_account(&self, data: &[u8])
        -> Result<DecodedAccount, DecodeError>;

    pub fn decode_event_cpi(&self, data: &[u8])
        -> Result<DecodedEvent, DecodeError>;
}

pub struct DecodedInstruction {
    pub name: String,
    pub args: serde_json::Value,
}

pub struct DecodedAccount { /* similar shape */ }
pub struct DecodedEvent   { /* similar shape */ }
```

**Capabilities:**

- Decoding of the full surface of primitive and composite IDL types used by modern Anchor programs: scalars (`bool`, `u8..u128`, `i8..i128`, `f32`, `f64`), `Pubkey`, `String`, `Vec<T>`, `Option<T>`, `COption<T>`, fixed-size arrays, `struct`, `enum` (including C-style and tagged-union variants), and nested `Defined` type references.
- Constant-time discriminator lookup via pre-indexed tables built once at construction.
- SHA256-based fallback discriminator derivation for IDLs predating Anchor v0.30's explicit discriminator field.
- Event-CPI self-call recognition via the fixed `EVENT_IX_TAG_LE` marker, with transparent dispatch into `idl.events[]`.
- A `DecodeError` enum with retryable / permanent classification, suitable for use in streaming pipelines where skip-and-continue semantics are required.
- Forbids `unwrap`, `expect`, and `panic!` on all decode paths by lint config. The decoder never aborts the caller's process on a malformed input.
- Property-tested with `proptest` across the full type surface.
- Fuzz corpus seeded with historical edge cases.

**Scope boundaries:**

- Does **not** fetch IDLs. That is the concern of `anchor-idl-fetcher` (§2.2).
- Does **not** produce SQL. That is the concern of `anchor-idl-postgres` (§2.3).
- Does **not** run async. Purely synchronous, allocating from the provided `&[u8]` slice only.

### 2.2 `anchor-idl-fetcher` — complementary

A cascading fetcher that locates an Anchor IDL by program ID.

**Public API sketch:**

```rust
pub struct IdlFetcher {
    rpc: Arc<dyn RpcAdapter>, // minimal trait — BYO implementation
    bundled: BundledIdlRegistry,
}

#[async_trait]
pub trait RpcAdapter: Send + Sync {
    async fn get_account_data(&self, address: &Pubkey)
        -> Result<Option<Vec<u8>>, RpcError>;
}

impl IdlFetcher {
    pub async fn fetch(&self, program_id: &Pubkey)
        -> Result<Idl, FetchError>;
}
```

**Capabilities:**

- On-chain IDL PDA derivation using the Anchor v0.30 `create_with_seed` scheme (Solarix originally shipped the incorrect `find_program_address` variant; the corrected derivation is now captured as a regression test).
- zlib decompression of the on-chain payload.
- JSON deserialization into `anchor-lang-idl-spec::Idl`.
- Optional bundled-IDL fallback registry for programs without on-chain IDLs (e.g., pre-v0.30 Anchor programs).
- Version detection (v0.30+ vs. legacy shapes).

**Scope boundaries:**

- Does **not** decide which RPC client to use. It accepts a minimal trait so callers can plug in `solana-rpc-client`, `reqwest`, or a test mock without dictating async runtime or HTTP library choices.
- Does **not** decode anything. It returns raw `Idl` structs, ready to hand to `anchor-idl-decoder`.

### 2.3 `anchor-idl-postgres` — opinionated consumer

PostgreSQL schema generator and dynamic query builder driven by a decoded `Idl`.

**Public API sketch:**

```rust
pub struct SchemaGenerator { /* ... */ }

impl SchemaGenerator {
    pub fn generate_ddl(&self, idl: &Idl, schema_name: &str) -> String;
    pub fn promoted_columns(&self, type_def: &IdlTypeDef) -> Vec<PromotedColumn>;
    pub fn sanitize_schema_name(idl_name: &str, program_id: &Pubkey) -> String;
}

pub struct QueryBuilder { /* ... */ }

impl QueryBuilder {
    pub fn filter(&mut self, field: &str, op: FilterOp, value: Value) -> &mut Self;
    pub fn paginate(&mut self, cursor: Option<&str>, limit: u32) -> &mut Self;
    pub fn build(self) -> (String, Vec<SqlValue>);
}
```

**Capabilities:**

- IDL → `CREATE SCHEMA` / `CREATE TABLE` / `CREATE INDEX` DDL generation. All statements use `IF NOT EXISTS` — the output is idempotent and suitable for application-level bootstrap without a migration tool.
- **Hybrid promoted + JSONB storage.** Simple scalar fields (`u64`, `bool`, `Pubkey`, `String`) are promoted to native PostgreSQL columns with correct types for fast indexed queries. Complex nested types fall through to a JSONB `data` column with GIN indexes using `jsonb_path_ops`. Every field is preserved regardless of promotion — no data loss on schema generation.
- **Schema name disambiguation.** `{sanitized_idl_name}_{first_8_of_program_id}` prevents collision when multiple programs share the same IDL `metadata.name` field.
- **Reserved column collision handling.** IDL field names that collide with SQL reserved words or with Solarix-internal columns (`data`, `slot`, `signature`, `id`, etc.) are renamed deterministically.
- **8-operator dynamic SQL builder** (`_eq`, `_ne`, `_gt`, `_gte`, `_lt`, `_lte`, `_in`, `_contains`) with JSONB operator translation for non-promoted fields.
- **BIGINT casting on promoted columns** — prevents the `operator does not exist: bigint > text` runtime error that arises when JSONB-typed filters are mixed with typed column comparisons.
- **u64 → BIGINT overflow guard** — values above `i64::MAX` are preserved in the JSONB `data` column and nulled in the promoted typed column, surfacing a clean query contract.

**Scope boundaries:**

- **PostgreSQL-specific by design.** A generalization across database backends would dilute the promoted-column/JSONB hybrid that gives this its value. Other databases deserve their own opinionated crate.
- Does **not** open connections. It returns SQL strings and parameter bindings that a caller executes via their existing `sqlx`, `tokio-postgres`, or `diesel` setup.

### 2.4 Components intentionally NOT proposed for extraction

For transparency, the following Solarix modules were considered and rejected:

| Module                                                         | Rationale for not extracting                                                                    |
| -------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| RPC client wrapper (`governor` rate limiting + `backon` retry) | Commoditized. Many projects have written functional equivalents. Not a meaningful contribution. |
| WebSocket `logsSubscribe` with dedup + reconnect               | Same as above. Several competing implementations exist.                                         |
| Pipeline orchestrator (backfill / streaming state machine)     | Application code specific to the indexer use case. Not a library concern.                       |
| System table bootstrap (`programs`, `indexer_state`)           | Solarix-specific schema for a specific runtime product.                                         |

Stating these explicitly prevents scope creep and clarifies that this proposal covers the **three** crates in §2.1–2.3 only.

---

## 3. Relationship with Existing Work

This proposal is explicitly complementary to the existing Anchor ecosystem. It does not compete with any actively maintained tooling.

| Existing               | Relationship                                                                                                                                                                      |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `anchor-lang`          | **Complementary.** `anchor-idl-decoder` operates on IDLs produced by `anchor build`, at runtime, for programs the caller did not compile themselves.                              |
| `anchor-cli`           | **Complementary.** `anchor-idl-fetcher` provides as a library what `anchor idl fetch` does as a CLI subcommand.                                                                   |
| `anchor-lang-idl-spec` | **Depends on.** This proposal builds directly on top of the canonical IDL type definitions and does not duplicate them.                                                           |
| `chainparser`          | **Effectively replaces** the dormant fork for new adopters, without deprecating existing uses. The replacement is cleaner, better-tested, and tracks current Anchor IDL versions. |
| `borsh`                | **Used as a primitive.** The decoder traverses IDL types and reads primitives via `borsh`-compatible framing. No overlap.                                                         |

---

## 4. Validation: Solarix as Reference Implementation

Solarix provides a production validation surface that a greenfield crate extraction would lack. Publishing these crates with this test surface on day one differentiates the extraction from the typical "weekend library" pattern that the ecosystem has learned to distrust.

**Test coverage ready for migration:**

- **Proptest roundtrips:** 50 property-based tests generate random values for all supported types, Borsh-serialize them, decode through the candidate crate, and assert JSON output matches field-for-field.
- **Integration tests against real programs:** fixtures for Meteora DLMM, Marginfi, Jupiter V6, and an intentionally reserved-name collision IDL. The crate inherits this coverage on extraction.
- **Mainnet smoke verification:** a nightly CI job runs the full indexer end-to-end against `api.mainnet-beta.solana.com` on real programs. This exercises the decoder on live chain data with no fixtures.
- **Fuzz corpus:** seed inputs accumulated during Solarix development via `cargo-fuzz`, ready to carry over.
- **Regression tests from e2e findings:** three critical bugs surfaced during Sprint-4 integration testing — (1) incorrect IDL PDA derivation scheme, (2) missing IDL persistence across restarts, (3) BIGINT SQL cast omission — each has a pinned regression test. All three are relevant to the extracted crates and encode institutional learning that would otherwise be lost.

---

## 5. Sequencing

Extraction is proposed in three phases. Each phase is independently valuable. Phase 1 alone fills the most visible gap.

### Phase 1 — `anchor-idl-decoder` v0.1.0

1. Extract `src/decoder/` and its type registry into a standalone crate.
2. Port the proptest suite and fuzz corpus.
3. Publish to crates.io under the ecosystem-standard `MIT OR Apache-2.0` dual license.
4. Solarix migrates to depend on the published crate, dogfooding it in production.
5. **Acceptance gate:** the full Solarix test suite stays green on the published crate.

### Phase 2 — `anchor-idl-fetcher` v0.1.0

1. Extract `src/idl/` with the RPC client abstracted behind a minimal `RpcAdapter` trait so callers can plug in the runtime they already use.
2. Port bundled IDL fixtures.
3. Publish independently of the decoder — the two crates are coherent but intentionally decoupled.

### Phase 3 — `anchor-idl-postgres` v0.1.0

1. Extract `src/storage/schema.rs` and `src/storage/queries.rs`.
2. Port the schema generation test suite, including the reserved-column collision fixtures and the BIGINT cast regression tests.
3. Publish with optional `sqlx` and `tokio-postgres` feature flags so the caller can choose their preferred async executor without being forced into one.

---

## 6. Open Questions

### 6.1 Naming and namespace politics

The working name `anchor-idl-*` is descriptive but overlaps with the `anchor-*` crate namespace owned by the Anchor team. A neutral alternative such as `solana-idl-toolkit` or `idl-kit` avoids the overlap but is less discoverable via search.

This should be discussed with the Anchor maintainers before publication. The ideal outcome is either (a) publication under `anchor-*` with maintainer endorsement, or (b) publication under a clearly distinct namespace with a pointer from the Anchor documentation.

### 6.2 Maintenance commitment

A dormant crate is worse than no crate. `chainparser`'s fate is the cautionary tale. Before publication, the extraction requires:

- A clearly stated maintenance policy (issue triage cadence, PR review SLA).
- A versioning commitment (semver, with a stated pre-1.0 breaking-change policy).
- A public channel for downstream consumers to report bugs (GitHub issues at minimum).
- A minimum of one backup maintainer to avoid single-point-of-failure risk.

None of these are optional. They are the entry criteria for publication, not post-launch aspirations.

### 6.3 Relationship with `anchor-lang-idl-spec` versioning

The `anchor-lang-idl-spec` crate is currently at 0.1.0. When it bumps (0.2.0, 1.0.0, or beyond), the extracted crates will need coordinated updates. A CI job that tracks upstream releases would reduce drift risk.

### 6.4 Event-CPI decoding scope

Whether event-CPI decoding ships in `anchor-idl-decoder` v0.1.0 or in a follow-up v0.2.0 depends on the Solarix Story 7.4 rollout, which is the first implementation of the event-CPI path. Both sequences are compatible with this proposal.

### 6.5 License, CI, and release automation

Standard crates.io hygiene: `cargo-deny` audit, `cargo-release` for version bumps, `cargo-semver-checks` for API compatibility, and a GitHub Actions release workflow. None of this is novel; it is listed here to confirm it is not being skipped.

---

## 7. References

- `anchor-lang-idl-spec` 0.1.0 (`crates.io`) — canonical Anchor IDL type definitions. The foundation this proposal builds on.
- `anchor-lang` Anchor framework — source of the `emit_cpi!()` mechanism and the `EVENT_IX_TAG_LE` constant (`anchor-lang/src/event.rs`).
- `chainparser` — the dormant prior-art reference implementation that this proposal effectively replaces. History and extraction rationale are captured in Solarix's `CLAUDE.md`.
- [Solarix Architecture](./architecture.md) — the system that validates the extraction.
- [Solarix Runbook](./runbook.md) — operational reference for the production use case.

---

## 8. Status and Feedback

This document is a **draft proposal**. It is intentionally scoped as a discussion starter rather than a committed roadmap. Feedback, criticism, and counter-proposals are welcome via GitHub issues on the Solarix repository.

Three outcomes would indicate this proposal is worth pursuing:

1. At least one other project confirms they are currently maintaining a private fork of `chainparser` or a hand-rolled decoder that this extraction would replace.
2. The Anchor maintainers indicate they are open to — or at least not opposed to — a complementary crate in this namespace.
3. The Solarix test surface migrates cleanly into the extracted crate in Phase 1 without regression.

If none of the above materializes, the extraction should not happen, and this document becomes a record of an option that was considered and rejected. That outcome is also acceptable.
