# Tech Debt Sweep: Sprint 2-3 Cleanup

Status: review

## Story

As a developer,
I want to resolve all verified P1 and P2 technical debt from completed stories (Epics 1-3, 5-1),
so that the codebase is correct, robust, and safe to build Sprint 3-4 features on top of.

## Context

After completing stories through 5-1, a comprehensive 5-agent verification sweep identified 14 false positives (already fixed) and 16 real open items. This story addresses the 9 highest-priority items (P1 + P2) that represent real bugs, correctness issues, or safety gaps in done stories. P3 items are included as stretch goals.

## Acceptance Criteria

### P1 — Must Fix

1. **AC1: Replace `std::process::exit(1)` in pipeline tests**
   - **Given** `src/pipeline/rpc.rs` has 7 sites using `process::exit(1)` in test code (lines ~736, 752, 768, 784, 907, 931, 943)
   - **When** I inspect the test code
   - **Then** all `unwrap_or_else(|e| { eprintln!(...); std::process::exit(1); })` patterns are replaced with `.expect()` or `assert!(matches!(...))`
   - **And** `cargo test` still passes with no `process::exit` calls in tests

2. **AC2: Fix `get_program` NULL column panic**
   - **Given** `src/api/handlers.rs` `get_program` handler reads `idl_hash` and `idl_source` as `String`
   - **When** the DB columns are NULL (they are nullable in schema)
   - **Then** the handler uses `Option<String>` for both fields
   - **And** the JSON response serializes missing values as `null`

### P2 — Correctness / Robustness

3. **AC3: Add HTTP timeout to IdlManager reqwest Client**
   - **Given** `src/idl/mod.rs` creates `reqwest::Client::new()` with no timeout
   - **When** I inspect the client construction
   - **Then** it uses `Client::builder().timeout(Duration::from_secs(30)).build()`

4. **AC4: Fix TOCTOU race in `write_registration` with INSERT ON CONFLICT**
   - **Given** `src/registry.rs` `write_registration` uses SELECT EXISTS + INSERT inside a transaction
   - **When** concurrent registrations occur
   - **Then** the INSERT uses `ON CONFLICT (program_id) DO NOTHING` (or equivalent)
   - **And** the function checks rows_affected to detect duplicates instead of SELECT EXISTS
   - **And** the separate `AlreadyRegistered` PK-violation catch can be removed or kept as defense-in-depth

5. **AC5: Fix IDL cache-before-DB ghost entry bug**
   - **Given** `src/registry.rs` `prepare_registration` caches IDL via `upload_idl()` before DB writes
   - **When** the subsequent `commit_registration` DB write fails
   - **Then** the IDL cache entry is removed (rollback)
   - **And** `list_programs` never returns program IDs that aren't in the DB

6. **AC6: Handle f32/f64 NaN/Infinity in decoder**
   - **Given** `src/decoder/mod.rs` f32/f64 decode paths (lines ~287-301)
   - **When** Borsh data decodes to NaN, Infinity, or -Infinity
   - **Then** non-finite values are represented as JSON strings (`"NaN"`, `"Infinity"`, `"-Infinity"`) instead of invalid JSON numbers
   - **And** a unit test covers each non-finite case

7. **AC7: Add v0 `loadedAddresses` test**
   - **Given** `src/pipeline/rpc.rs` has logic to merge `loadedAddresses` into account keys (lines ~527-530)
   - **When** I inspect the test suite
   - **Then** a test exists with a block JSON fixture containing `loadedAddresses: { writable: [...], readonly: [...] }`
   - **And** the test verifies merged account keys include loaded addresses

8. **AC8: Change `register_program` response to 201 Created**
   - **Given** `src/api/handlers.rs` `register_program` returns HTTP 202 Accepted
   - **When** the registration completes synchronously before responding
   - **Then** the handler returns `StatusCode::CREATED` (201)

9. **AC9: Fix partial commit status stuck at `registered`**
   - **Given** `src/registry.rs` `commit_registration` can leave status as `registered` if `update_program_status` fails after `generate_schema` succeeds
   - **When** any step after `write_registration` fails
   - **Then** the program status is updated to `error` with an error message
   - **And** if the status update itself fails, the error is logged and the original error is still returned (best-effort)

### P3 — Stretch Goals (if time allows)

10. **AC10: Fix HTTP 4xx error classification**
    - **Given** `src/pipeline/rpc.rs` classifies all non-429 HTTP errors as retryable
    - **When** a 400/401/403/404 response is received
    - **Then** it is classified as Fatal (not retried)
    - **And** only 429 and 5xx are retried

11. **AC11: Cache TypeRegistry per-IDL**
    - **Given** `src/decoder/mod.rs` rebuilds `TypeRegistry::from_idl()` on every decode call
    - **When** decoding multiple instructions/accounts for the same program
    - **Then** the TypeRegistry is cached (keyed by IDL hash or program_id) and reused

12. **AC12: Add tracing to decoder module**
    - **Given** `src/decoder/mod.rs` has no tracing imports or log statements
    - **When** decoding encounters trailing bytes, unknown discriminators, or skipped fields
    - **Then** appropriate `debug!`/`warn!` statements are emitted

13. **AC13: Fix stale test name**
    - **Given** `src/decoder/mod.rs` has `test_decode_account_stub` (line ~1147)
    - **When** I inspect the test
    - **Then** the test name reflects what it actually tests
    - **And** the panic message at line ~1158 is corrected

14. **AC14: Remove dead `tx_encoding` config field**
    - **Given** `src/config.rs` defines `tx_encoding` (line ~54-55) but it's never read by any code
    - **When** I inspect the RPC client
    - **Then** the dead config field and its env var `SOLARIX_TX_ENCODING` are removed
    - **And** a comment in `rpc.rs` documents the hardcoded encoding choices

15. **AC15: Reduce cloning in `commit_registration`**
    - **Given** `src/registry.rs` `commit_registration` clones `Idl` 2x and several strings multiple times (~11 clones)
    - **When** I refactor the function
    - **Then** the last `Idl` usage consumes by move instead of cloning
    - **And** string fields are borrowed or moved where possible

## Tasks / Subtasks

- [x] P1: Replace `process::exit(1)` with `expect()`/`assert!` in rpc.rs tests (AC1)
- [x] P1: Fix `get_program` NULL column handling (AC2)
- [x] P2: Add reqwest timeout to IdlManager (AC3)
- [x] P2: Replace SELECT EXISTS + INSERT with INSERT ON CONFLICT (AC4)
- [x] P2: Fix IDL cache ghost entry on failed registration (AC5)
- [x] P2: Handle f32/f64 non-finite values in decoder (AC6)
- [x] P2: Add v0 loadedAddresses test fixture (AC7)
- [x] P2: Change register_program to 201 Created (AC8)
- [x] P2: Fix partial commit status-stuck bug (AC9)
- [x] P3: Fix HTTP 4xx error classification (AC10)
- [x] P3: Cache TypeRegistry per-IDL (AC11)
- [x] P3: Cache TypeRegistry per-IDL (AC11)
- [x] P3: Add tracing to decoder module (AC12)
- [x] P3: Rename stale test + fix message (AC13)
- [x] P3: Remove dead tx_encoding config field (AC14)
- [x] P3: Reduce cloning in commit_registration (AC15)

## Dev Notes

- All items are in "done" stories — this sweep touches code that already works but has known gaps.
- P1+P2 items (AC1-AC9) are the core scope. P3 (AC10-AC15) are stretch.
- Each AC is independent — can be committed separately or batched.
- After completing P1+P2, run `cargo test && cargo clippy` to verify no regressions.
- Update `deferred-work.md` to mark resolved items after this story is done.

## File List

- `src/pipeline/rpc.rs` — AC1 (process::exit → expect/assert), AC7 (loadedAddresses test), AC10 (4xx Fatal classification)
- `src/api/handlers.rs` — AC2 (Option<String> for nullable cols), AC5 (auto_fetch rollback fix), AC8 (201 Created)
- `src/idl/mod.rs` — AC3 (30s timeout on reqwest Client)
- `src/registry.rs` — AC4 (INSERT ON CONFLICT), AC9 (status-stuck fix), AC15 (reduced cloning)
- `src/decoder/mod.rs` — AC6 (NaN/Infinity as strings + tests), AC11 (TypeRegistry cache), AC12 (tracing), AC13 (test rename)
- `src/config.rs` — AC14 (removed dead tx_encoding field)

## Change Log

- 2026-04-06: Implemented all 15 ACs (P1+P2+P3). 116 tests pass, clippy clean, fmt clean.

## Dev Agent Record

### Implementation Plan

All 15 acceptance criteria implemented in a single pass. Each change was independently verified at the module level before the final full regression run.

### Completion Notes

**P1 (2 items):**

- AC1: Replaced 7 `std::process::exit(1)` in rpc.rs tests with `.expect()` or `assert!(matches!(...))`.
- AC2: Changed `idl_hash` and `idl_source` from `String` to `Option<String>` in `get_program` handler.

**P2 (7 items):**

- AC3: Added 30-second timeout to `IdlManager::new()` reqwest client builder.
- AC4: Replaced SELECT EXISTS + INSERT with INSERT...ON CONFLICT DO NOTHING + rows_affected check, eliminating TOCTOU race.
- AC5: Modified `auto_fetch_idl` to return `bool` (whether new IDL was fetched), used in `do_register` rollback logic to cover the auto-fetch ghost entry window.
- AC6: Added `format_non_finite_f32/f64` helpers; f32/f64 NaN/Infinity/-Infinity now serialize as JSON strings. Two tests added covering all 6 cases.
- AC7: Added `test_parse_block_with_loaded_addresses` verifying writable+readonly loaded addresses are appended to account_keys.
- AC8: Changed `StatusCode::ACCEPTED` → `StatusCode::CREATED` in `do_register`.
- AC9: Changed `update_program_status("schema_created").await?` to a match that attempts best-effort error-status update on failure.

**P3 (6 items):**

- AC10: HTTP 4xx (except 429) now classified as `PipelineError::Fatal`, only 5xx and transport errors are retried.
- AC11: `ChainparserDecoder` now caches `TypeRegistry` per IDL address in a `Mutex<HashMap>`.
- AC12: Added `tracing::{debug, warn}` to decoder — trailing bytes, unknown discriminators, SHA-256 fallback matches.
- AC13: Renamed `test_decode_account_stub` → `test_decode_account_no_accounts_in_idl`, fixed panic message.
- AC14: Removed dead `tx_encoding` field from Config struct.
- AC15: Destructured `RegistrationData` in `commit_registration`, reduced Idl clones from 2 to 1 (last usage consumes by move).

## Dependencies

- None (all code already exists and compiles)
- Should be completed before story 5-2 to avoid building on top of known bugs
