# Deferred Work

## Deferred from: code review of 1-1-project-scaffolding-and-configuration (2026-04-05)

- **Config cross-field validation missing** — `db_pool_min > db_pool_max`, `start_slot > end_slot`, `retry_initial_ms > retry_max_ms`, `api_default_page_size > api_max_page_size`, `rpc_rps = 0`, `backfill_chunk_size = 0` all accepted. Add validation when each field is first consumed.
- **`TransactionData.slot` duplicates `BlockData.slot`** — Redundancy invites inconsistency. Revisit when types are used in pipeline (Story 3.4+).
- **`ApiError` missing `IntoResponse` impl** — Required by architecture spec. Implement in Story 5.1 (API endpoints).
- **`subscribe()` returns `()` with no message channel** — Redesign in Story 4.1 (WebSocket streaming).
- **`fetch_block` on empty/skipped slot** — Returns `BlockData` not `Option<BlockData>`. Handle in Story 3.3 (RPC block source).
- **`get_program_account_keys` unbounded `Vec<String>`** — Large programs could OOM. Address in Story 3.3 (RPC client).
- **`log_level` and `tx_encoding` are free-form strings** — Add enum validation when fields are first consumed.
- **`litesvm` absent from dev-dependencies** — Add in Epic 3 when pipeline tests are written.
- **`chainparser` git dep unpinned (branch, not rev/tag)** — Pin when uncommented in Epic 2.
- **RPITIT traits not object-safe** — `BlockSource`, `AccountSource`, `TransactionStream` use `-> impl Future` breaking `dyn Trait`. Add object safety as AC to Stories 3.3 and 4.1.

## Deferred from: code review of 1-2-database-connection-and-system-table-bootstrap (2026-04-05)

- **`updated_at` column has no auto-update trigger** — `DEFAULT NOW()` only fires on INSERT. UPDATEs leave it stale. Address in story 3.4 (storage writer) with either a PG trigger or application-layer SET.
- **Bootstrap DDL not wrapped in explicit transaction** — Two CREATE TABLE statements without BEGIN/COMMIT. Partial failure leaves half-bootstrapped state. Low real-world risk with IF NOT EXISTS ordering. Future hardening item.
- **`status` columns are unconstrained TEXT** — No CHECK constraint for valid pipeline state values on `programs.status` and `indexer_state.status`. Add constraints in a schema hardening pass.
- **`db_pool_min > db_pool_max` not validated** — sqlx will error at runtime but no explicit validation. Config validation not yet planned (also noted in 1.1 review).
- **Duplicate error output on failure** — `map_err(|e| { error!(...); e })?` logs via tracing then Rust prints Err to stderr. Clean up with graceful shutdown (story 4.3).

## Deferred from: code review of 1-3-docker-compose-and-health-endpoint (2026-04-05)

- **`rust:latest` / `debian:bookworm-slim` unpinned** — Non-reproducible Docker builds. Pin to specific versions when CI pipeline is set up (Story 6.4).
- **No restart policy on compose services** — Crashed containers stay down. Add when pipeline orchestrator handles crash recovery (Story 4.3).
- **Hardcoded credentials in docker-compose.yml** — Dev-only convenience. Address when deployment docs are written (Story 7.1).
- **ApiError missing IntoResponse impl** — Explicitly scoped for Story 5.1 (also noted in 1.1 review).

## Deferred from: code review of 2-2-manual-idl-upload-and-program-registration (2026-04-05)

- **No `program_id` format validation** — `register_program` accepts empty strings and non-base58 values. The `upload_idl` path bypasses `Pubkey::parse`. Add validation at the API boundary in Story 5.1 (`POST /api/programs` handler).

## Findings from: deep architecture review of Epics 2 & 3 (2026-04-06)

These findings come from a comprehensive parallel subagent review of all implemented stories
in Epics 2 and 3. Items are grouped by priority. Stories marked "done" in sprint-status have
these as known technical debt; stories still in-progress have blocking items noted.

### P0 — Compile-blocking (must fix to build)

- **Missing `SchemaFailed` match arm in `From<RegistrationError> for ApiError`** — `src/api/mod.rs:101-108`. The `RegistrationError` enum has 4 variants but the `From` impl only handles 3, omitting `SchemaFailed`. Causes `error[E0004]`. **Story 2-2. Fix: add `RegistrationError::SchemaFailed(e) => ApiError::StorageError(e.to_string())`.**

### P1 — Must fix before stories can be marked done

- **Integration test asserts wrong status value** — `tests/registration_test.rs:69,84` assert `"registered"` but `register_program()` now returns `"schema_created"` after calling `generate_schema()`. **Story 2-2. Fix: change assertions to `"schema_created"`.**
- **`std::process::exit(1)` in tests instead of `panic!`/`assert!`** — Used in `tests/registration_test.rs:133` and ~8 match arms in `src/pipeline/rpc.rs` (lines 728, 744, 760, 776, 899, 923, 935, 990). Kills the entire test binary, skips cleanup, prevents other tests from running. **Stories 2-2, 3-3. Fix: replace with `assert!(matches!(...))`.**
- **Unused imports causing warnings** — `error` in `src/api/handlers.rs:11`, `delete` in `src/api/mod.rs:9`, `warn` in `src/storage/schema.rs:8`. **Story 2-2. Fix: remove.**

### P2 — Should fix (correctness/robustness)

- **No HTTP timeout on `reqwest::Client` in IdlManager** — `src/idl/mod.rs:54` uses `Client::new()` with no timeout. A hanging RPC blocks IDL fetch indefinitely. **Story 2-1. Fix: `Client::builder().timeout(Duration::from_secs(30)).build()`.**
- **Non-atomic duplicate check + insert (TOCTOU race)** — `src/registry.rs:59-119`. SELECT EXISTS and INSERT are not in a transaction. Concurrent registration of the same program_id can cause PK violation (returns `DatabaseError` instead of `AlreadyRegistered`). Also, crash between `programs` INSERT and `indexer_state` INSERT leaves orphaned row. **Story 2-2. Fix: wrap in `pool.begin()` / `tx.commit()`. Already partially noted in 2-3 deferred work.**
- **IDL cached before DB writes — ghost entries on failure** — `src/registry.rs:78-82`. `upload_idl()` or `get_idl()` caches before DB inserts. If DB fails, `list_programs()` returns ghost program IDs. **Story 2-2. Fix: cache after successful DB write, or add cleanup on error.**
- **Integration test doesn't clean up created PG schemas** — `tests/registration_test.rs:38-48`. Cleanup only deletes rows from `indexer_state` and `programs` but doesn't `DROP SCHEMA`. **Story 2-2. Fix: add `DROP SCHEMA IF EXISTS ... CASCADE` to cleanup.**
- **f32/f64 NaN/Infinity produce invalid JSON** — `src/decoder/mod.rs:288-301`. Borsh can encode NaN/Infinity but `serde_json` will error or panic on these values. Rare but possible on-chain. **Story 3-1. Fix: check `is_finite()`, represent non-finite as strings.**
- **No test for v0 transaction `loadedAddresses`** — `src/pipeline/rpc.rs` tests. The v0 loaded address logic (lines 517-523) is untested. Critical for mainnet indexing. **Story 3-3. Fix: add test fixture with `loadedAddresses` in block JSON.**

### P3 — Nice to have (quality/polish)

- **u256/i256 hex encoding is LE byte order** — `src/decoder/mod.rs:313-324`. Consumers typically expect BE hex. Document or reverse bytes. **Story 3-1.**
- **`TypeRegistry` rebuilt per-call** — `src/decoder/mod.rs:729,764`. `TypeRegistry::from_idl()` clones all type defs on every decode. Performance concern at scale. **Stories 3-1, 3-2. Consider caching per-IDL.**
- **No `tracing` usage in decoder module** — Story spec says log trailing bytes at `debug!`. No `tracing` import in entire file. **Stories 3-1, 3-2.**
- **Dead `source` variable in registry** — `src/registry.rs:72-93`. Computed and then explicitly discarded with `let _ = source`. **Story 2-2. Remove.**
- **`compute_idl_hash` not truly deterministic for reordered keys** — `src/idl/mod.rs:171-182`. Works today because `serde_json` without `preserve_order` uses `BTreeMap`, but fragile if feature flag changes. **Story 2-1. Add regression test.**
- **Stale test names** — `test_decode_account_stub` (`decoder/mod.rs:1147`) no longer tests a stub. Stale panic message at line 1158. **Story 3-2. Rename and fix message.**
- **Missing decoder test coverage** — No tests for: u16/i16/i32/i64/f32/f64 primitives, u256/i256, Bytes type, Array type, buffer underrun, invalid bool byte, tuple enums, generics, COption invalid tag, multiple account types in one IDL. **Stories 3-1, 3-2.**
- **`sanitize_identifier` passes Unicode alphanumerics** — `src/storage/schema.rs:21-23`. `char::is_alphanumeric()` is Unicode-aware. CJK/accented chars pass through. PG identifiers need quoting for non-ASCII. **Story 2-2. Already noted in story review findings.**
- **Long schema names lose collision-prevention suffix** — `src/storage/schema.rs:45-54`. 63-byte sanitized name + 9-byte suffix gets truncated, dropping `_{id_prefix}`. **Story 2-2. Already noted in story review findings.**
- **HTTP 4xx errors (except 429) classified as retryable** — `src/pipeline/rpc.rs:289-291`. HTTP 400/403/404 retried with backoff up to 300s. Should be Fatal. **Story 3-3.**
- **P10 comment is misleading** — `src/idl/mod.rs:61`. Says "single lookup" but code still does `contains_key` + index. **Story 2-1. Style nit.**

## Deferred from: code review of 2-3-dynamic-schema-generation (2026-04-06)

- **`DROP SCHEMA` in `delete_program` uses string interpolation instead of `quote_ident()`** — `src/api/handlers.rs:173` builds DDL with `format!()` instead of using the `quote_ident()` function from `schema.rs`. Low risk since `schema_name` comes from the DB (sanitized at creation), but should use `quote_ident()` for defense in depth. Fix in Story 5.1.
- **TOCTOU race in `write_registration` at default isolation level** — `src/registry.rs:186-209` uses SELECT EXISTS + INSERT inside a transaction with READ COMMITTED. Two concurrent transactions could both see `EXISTS = false`. Currently mitigated by `Arc<RwLock>`, but Story 5.1 plans to relax the lock scope — must add a UNIQUE constraint guard or serializable isolation when that happens.
- **Build error in `src/api/handlers.rs`** — The `register_program` handler stub has a `!Send` issue with `RwLockWriteGuard` across an `.await` point. Blocks `cargo test --lib`. Fix in Story 5.1 handler implementation.

## Deferred from: code review of 5-1-program-management-endpoints (2026-04-06)

- **TOCTOU race in `write_registration` duplicate check** — `src/registry.rs:267-277`. SELECT EXISTS + INSERT at READ COMMITTED isolation. Concurrent registrations could both see `exists = false`. PK constraint catches it but returns `DatabaseError` instead of `AlreadyRegistered`. Fix: use INSERT ON CONFLICT or SERIALIZABLE isolation.
- **`list_programs` has no pagination** — `src/api/handlers.rs:157`. `fetch_all` loads every row. Config already has `api_default_page_size` / `api_max_page_size`. Add pagination in story 5.2+ (query builder).
- **Excessive cloning in `commit_registration`** — `src/registry.rs:133-158`. `Idl` struct cloned 2x (for `generate_schema` and `seed_metadata`). Last usage can consume `data` by move. Performance optimization.
- **Integration test doesn't clean up created PG schemas** — `tests/registration_test.rs:38-48`. Cleanup only deletes DB rows, doesn't `DROP SCHEMA ... CASCADE`. Leaves orphaned schemas on repeated runs.
- **No request body size limit on IDL upload** — `src/api/mod.rs` router has no `DefaultBodyLimit`. A large POST body could exhaust server memory. Add body limit in hardening sprint (Epic 6).
- **Hard delete doesn't check for active indexing pipeline** — `src/api/handlers.rs:241`. No status guard before DROP SCHEMA. Pipeline (story 3.5) doesn't exist yet; add guard when pipeline is implemented.

## Deferred from: code review of 3-3-rpc-block-source-and-rate-limited-fetching (2026-04-06, second pass)

- **Unbounded `Vec` accumulation in `get_blocks` for huge ranges** — `src/pipeline/rpc.rs:347`. Calling `get_blocks(0, 300_000_000)` accumulates ~150M u64 entries (~1.2 GB). Pipeline orchestrator will enforce `backfill_chunk_size` when implemented (Story 3-5). Add a max-range guard or streaming mechanism.
- **`is_retryable()` includes `Idl(FetchFailed)` beyond AC8 spec** — `src/pipeline/mod.rs:49`. AC8 specifies exactly 3 retryable variants (RpcFailed, WebSocketDisconnect, RateLimited). Code also retries `IdlError::FetchFailed`. Reasonable behavior but not in spec. Added by story 2-1.
- **`tx_encoding` config field unused by RpcClient** — `src/config.rs:54-55`. Config defines `SOLARIX_TX_ENCODING` (default "base64") but RpcClient hardcodes `encoding: "json"`. Dead config field could mislead operators. Either remove or wire through.
