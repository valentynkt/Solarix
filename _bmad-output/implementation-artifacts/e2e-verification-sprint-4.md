# Story: E2E Verification — Sprint 4 Gate

Status: ready-for-dev

## Story

As a developer preparing for bounty submission,
I want to verify the entire Solarix system works end-to-end against a real Solana cluster,
so that I can confirm all bounty requirements are met and catch integration issues before writing documentation.

## Context

This is a **verification gate** after Sprint 4 (last feature sprint) and before Sprint 5 (tracing + docs). It is NOT about writing new features — it's about proving the existing system works as a whole and fixing any integration bugs discovered.

**Prerequisite:** ALL Sprint 4 stories must be done (4-1, 4-2, 4-3, 5-4) before this story starts.

**Bounty judging criteria this verifies (in order):**

1. Dynamic schema generation and account decoding
2. Real-time mode with cold start functionality
3. Reliability features (exponential backoff, retry, graceful shutdown)
4. Advanced API capabilities (multi-param filters, aggregation, statistics)
5. Infrastructure: Docker Compose, env vars, structured logging

## Acceptance Criteria

1. **AC1: Docker Compose cold start**
   - **Given** a clean environment (`docker compose down -v`)
   - **When** `docker compose up --build` is run
   - **Then** PostgreSQL starts, Solarix connects, system tables (`programs`, `indexer_state`) are auto-created, `/health` returns 200
   - **And** no manual steps are needed (no pre-setup, no migration scripts)
   - **And** startup completes in under 60 seconds

2. **AC2: Program registration with auto-fetch IDL**
   - **Given** a running Solarix instance pointed at devnet/mainnet
   - **When** `POST /api/programs {"program_id": "<ANCHOR_PROGRAM>"}` is called (no IDL body)
   - **Then** the IDL is auto-fetched from chain via PDA, a per-program schema is created with typed tables (one per account type + `_instructions` + `_checkpoints` + `_metadata`)
   - **And** `GET /api/programs` shows the program with status `schema_created`
   - **And** `GET /api/programs/{id}` returns program details including account type names and instruction names matching the IDL

3. **AC3: Manual IDL upload**
   - **Given** a running Solarix instance
   - **When** `POST /api/programs {"program_id": "...", "idl": <IDL_JSON>}` is called with the fixture IDL
   - **Then** the schema is generated from the provided IDL, and the program appears in the registry with `idl_source: "manual"`

4. **AC4: Batch indexing — slot range mode**
   - **Given** a registered program with schema and known on-chain transactions
   - **When** batch backfill runs for a slot range containing those transactions (triggered via registration flow or config env vars `SOLARIX_START_SLOT` / `SOLARIX_END_SLOT`)
   - **Then** decoded instructions appear in `{schema}._instructions` with correct `signature`, `slot`, `instruction_name`, `args` (JSONB), promoted columns
   - **And** decoded accounts appear in `{schema}.{account_type}` with correct `pubkey`, `slot_updated`, `data` (JSONB), promoted typed columns
   - **And** promoted columns contain correct typed values (e.g., BIGINT for u64, TEXT for Pubkey — not just JSONB)
   - **And** u64 values > i64::MAX are NULL in promoted column but preserved as string in JSONB `data`

5. **AC5: Batch indexing — signature list mode**
   - **Given** a list of known transaction signatures for the registered program
   - **When** signature-based batch indexing processes them
   - **Then** each transaction is fetched, decoded, and stored identically to slot-range mode
   - **And** results are queryable via the same API endpoints

6. **AC6: Checkpoint resume (crash safety)**
   - **Given** a backfill that has partially completed (checkpoint exists in `_checkpoints` table)
   - **When** the process is killed and restarted, backfill runs again for the same range
   - **Then** it resumes from `last_checkpoint_slot + 1` (visible in logs: "resuming from slot X")
   - **And** no duplicate rows are created (`ON CONFLICT DO NOTHING` — row count unchanged)
   - **And** the final checkpoint matches the end of the range

7. **AC7: Real-time streaming**
   - **Given** a registered program with active on-chain transactions
   - **When** the WebSocket `logsSubscribe` subscription is active
   - **Then** new transactions are captured, fetched via `getTransaction`, decoded, and stored in near-real-time
   - **And** the deduplication set prevents double-processing (bounded FIFO, configurable size)
   - **And** ping/pong keep-alive prevents idle disconnects

8. **AC8: Cold start (gap backfill + streaming transition)**
   - **Given** the indexer was stopped at slot N and new transactions occurred at slots N+5, N+10, etc.
   - **When** the indexer restarts
   - **Then** pipeline state transitions: `Initializing → Backfilling → CatchingUp → Streaming`
   - **And** the gap between slot N and current slot is backfilled
   - **And** WebSocket subscription starts concurrently with gap backfill
   - **And** no transactions are missed or duplicated during the transition (both paths write with `ON CONFLICT DO NOTHING`)

9. **AC9: Multi-parameter API queries**
   - **Given** indexed data in the database
   - **When** queries with multiple filters are sent (e.g., `?slot_gt=X&instruction_name_eq=Y&limit=10`)
   - **Then** correct filtered results are returned
   - **And** cursor-based pagination works for instructions (base64 `{slot}_{signature}`, no overlap between pages)
   - **And** offset-based pagination works for accounts
   - **And** promoted column filters use direct SQL comparison (not JSONB extraction)
   - **And** non-promoted field filters use JSONB `@>` containment (GIN-indexed)
   - **And** unknown filter fields return 400 with `{ "error": { "code": "INVALID_FILTER", "available_fields": [...] } }`

10. **AC10: Aggregation and statistics**
    - **Given** indexed data
    - **When** `GET /api/programs/{id}` is called
    - **Then** it returns instruction count and account count statistics per type
    - **And** `GET /api/programs/{id}/instructions` returns per-instruction-type counts
    - **And** aggregation endpoint returns call counts for a specific instruction over a time period (per bounty requirement)

11. **AC11: Second program isolation**
    - **Given** program A is already registered and indexed
    - **When** program B is registered
    - **Then** program B gets its own schema (`{name_b}_{prefix_b}`, different from A's)
    - **And** queries to program A return unchanged results
    - **And** queries to program B return only program B data
    - **And** `\dn` in psql shows both schemas

12. **AC12: Error handling**
    - `POST /api/programs` with invalid (non-base58) program ID → 422
    - `POST /api/programs` with valid ID but no on-chain IDL and no body → 422 with clear error
    - `GET /api/programs/{nonexistent}` → 404
    - Duplicate registration → 409
    - Invalid filter operator → 400 with descriptive error and available_fields
    - RPC 429 during backfill → exponential backoff visible in logs (not crash)

13. **AC13: Graceful shutdown**
    - **Given** an active backfill or streaming session
    - **When** SIGTERM is sent (`docker compose stop` or `kill -TERM`)
    - **Then** logs show ordered sequence: reader stop → pipeline drain → DB flush → checkpoint save
    - **And** the process exits cleanly (exit code 0)
    - **And** on restart, no data corruption (checkpoint is consistent with written data)

14. **AC14: Structured logging**
    - **Given** the indexer is running with `SOLARIX_LOG_FORMAT=json`
    - **When** log output is inspected
    - **Then** every log line is valid JSON with fields: `timestamp`, `level`, `target` (module), `message`
    - **And** pipeline logs include contextual fields: `slot`, `program_id`, `schema_name`
    - **And** API logs include request method, path, status code

15. **AC15: Environment configuration**
    - **Given** `.env.example` exists at project root
    - **Then** it documents all 22+ env vars with defaults and descriptions
    - **And** `docker-compose.yml` references env vars (not hardcoded values)
    - **And** invalid config combinations are handled: pool_min > pool_max, rpc_rps=0, chunk_size=0

## Tasks / Subtasks

Tasks are ordered to build on each other — complete sequentially.

- [ ] Task 1: Environment setup (AC: #1, #15)
  - [ ] Verify `.env.example` exists and documents all env vars from `src/config.rs`
  - [ ] Create/update `.env.example` if missing or incomplete
  - [ ] `docker compose down -v` → `docker compose up --build -d`
  - [ ] `curl http://localhost:3000/health` returns 200
  - [ ] Verify system tables exist: `psql -c "SELECT * FROM programs"` returns empty
  - [ ] Verify JSON log output: `docker compose logs solarix | head -5 | jq .`
  - [ ] Test with `SOLARIX_LOG_FORMAT=pretty` — verify non-JSON human-readable output
  - [ ] Document any Dockerfile or compose issues → fix immediately

- [ ] Task 2: Program registration (AC: #2, #3, #11)
  - [ ] Find a real Anchor program with on-chain IDL (see Dev Notes for discovery method)
  - [ ] `POST /api/programs {"program_id": "<FOUND_ID>"}` — verify auto-fetch + schema
  - [ ] `GET /api/programs` — verify program listed with `schema_created` status
  - [ ] `GET /api/programs/{id}` — verify account types and instruction names match IDL
  - [ ] Verify DB: `psql -c "\dn"` shows new schema, `\dt {schema}.*` shows tables
  - [ ] Verify promoted columns: `psql -c "\d {schema}.{account_table}"` shows typed columns
  - [ ] Register a second program — verify separate schema, no interference
  - [ ] Manual IDL upload: `POST` with `tests/fixtures/idls/simple_v030.json` as body
  - [ ] Record program IDs and schema names for subsequent tasks

- [ ] Task 3: Batch indexing — slot range (AC: #4, #6)
  - [ ] Find slot range with activity: use `getSignaturesForAddress` RPC to locate recent txs
  - [ ] Configure `SOLARIX_START_SLOT` and `SOLARIX_END_SLOT` (or trigger via registration flow)
  - [ ] Run backfill, watch logs for progress (slots/sec, checkpoint updates)
  - [ ] Query `{schema}._instructions`: verify rows with correct signature, slot, instruction_name, args
  - [ ] Query `{schema}.{account_type}`: verify decoded accounts with pubkey, promoted columns
  - [ ] Verify promoted column types in DB match IDL (BIGINT for u64, TEXT for pubkey, etc.)
  - [ ] Check `{schema}._checkpoints`: verify `last_slot` and `stream = "backfill"`
  - [ ] Kill process mid-backfill, restart — verify "resuming from slot X" in logs
  - [ ] Re-run same range — verify row count unchanged (dedup working)

- [ ] Task 4: Batch indexing — signature list (AC: #5)
  - [ ] Collect 5-10 known signatures from the test program
  - [ ] Trigger signature-based indexing
  - [ ] Verify each transaction decoded and stored correctly
  - [ ] Query via API — verify same results as slot-range mode

- [ ] Task 5: API queries and filters (AC: #9, #10, #12)
  - [ ] `GET /api/programs/{id}/instructions/{name}?limit=5` — verify 5 results
  - [ ] `GET /api/programs/{id}/instructions/{name}?slot_gt=X&limit=10` — verify filter works
  - [ ] Cursor pagination: fetch page 1 with `limit=3`, use returned cursor for page 2, verify no overlap
  - [ ] `GET /api/programs/{id}/accounts/{type}?limit=5&offset=0` then `offset=5` — verify offset pagination
  - [ ] `GET /api/programs/{id}/accounts/{type}/{pubkey}` — verify single account
  - [ ] Filter on promoted column (e.g., a pubkey or numeric field from the IDL)
  - [ ] Filter on JSONB field (non-promoted) — verify `@>` containment works
  - [ ] `?nonexistent_field_eq=x` → verify 400 with `available_fields` list
  - [ ] `?slot_gt=abc` (non-numeric for numeric field) → verify 400 with clear error
  - [ ] Verify aggregation: `GET /api/programs/{id}` shows instruction/account counts
  - [ ] Verify `GET /api/programs/{id}/instructions` shows per-type counts

- [ ] Task 6: Real-time streaming + cold start (AC: #7, #8)
  - [ ] Start indexer with streaming enabled for the test program
  - [ ] Verify WS connection established in logs (`logsSubscribe` confirmation)
  - [ ] Wait for new transaction (or use a program with regular activity)
  - [ ] Verify new tx appears in DB within seconds (query `_instructions` ordered by slot DESC)
  - [ ] Stop indexer, note last checkpoint slot
  - [ ] Wait 30+ seconds (allow new on-chain activity)
  - [ ] Restart indexer — verify logs show gap detection and backfill
  - [ ] Verify state transitions in logs: `Initializing → Backfilling → CatchingUp → Streaming`
  - [ ] Query DB — verify no gap in indexed slots between stop and restart

- [ ] Task 7: Reliability and error handling (AC: #12, #13)
  - [ ] `POST /api/programs {"program_id": "invalid!!!"}` → verify 422
  - [ ] `GET /api/programs/11111111111111111111111111111111` → verify 404
  - [ ] Duplicate registration → verify 409
  - [ ] `DELETE /api/programs/{id}?drop_tables=true` → verify schema dropped, program removed
  - [ ] Send SIGTERM during active backfill: `docker compose stop`
  - [ ] Verify logs show: reader stop → drain → checkpoint save → clean exit
  - [ ] Restart — verify checkpoint consistent, no corruption
  - [ ] Lower rate limit: `SOLARIX_RPC_RPS=2` — run backfill, observe backoff in logs

- [ ] Task 8: v0 transaction and edge cases
  - [ ] If testing on mainnet: verify v0 transactions are processed (not silently dropped)
  - [ ] Verify `maxSupportedTransactionVersion: 0` is set in RPC calls (check code or logs)
  - [ ] Test with a block containing 0 relevant transactions — verify no crash, no empty writes
  - [ ] Test with unknown discriminator (tx from wrong program version) — verify warn log, skip, continue

- [ ] Task 9: Fix discovered issues
  - [ ] Maintain a running list of all bugs/issues found during Tasks 1-8
  - [ ] Fix critical issues immediately (anything that would fail bounty judging)
  - [ ] Fix moderate issues that are quick wins (< 30 min each)
  - [ ] Log remaining non-critical issues in `deferred-work.md`

- [ ] Task 10: Verification sign-off
  - [ ] All ACs 1-15 pass
  - [ ] `cargo test` passes (no regressions from fixes)
  - [ ] `cargo clippy` clean
  - [ ] `cargo fmt -- --check` passes
  - [ ] Document: test program IDs, schema names, slot ranges used, curl examples that worked
  - [ ] Save these as `_bmad-output/implementation-artifacts/e2e-test-results.md` for README reuse

## Dev Notes

### Finding Test Programs

Don't guess program IDs — discover them:

```bash
# Option 1: Browse known Anchor programs with on-chain IDLs
# Check Anchor Program Registry or use solana-verify

# Option 2: Use a known active program
# Mainnet examples with Anchor IDLs:
#   - Marinade Finance: MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD
#   - Raydium AMM V4: 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8

# Option 3: Find any program with on-chain IDL
# Derive IDL PDA and check if account exists:
#   PDA = findProgramAddress(["anchor:idl"], program_id)
```

For devnet: programs may have less activity. Consider using mainnet with `SOLARIX_RPC_RPS=5` to stay within public limits.

### Batch Indexing Trigger Mechanism

Currently, backfill is triggered through `PipelineOrchestrator::run_backfill()` which takes `start_slot` and `end_slot`. Check how registration flow connects to pipeline:

- Registration creates schema, then pipeline starts automatically? Or is it manual?
- The `SOLARIX_START_SLOT` / `SOLARIX_END_SLOT` env vars may control this
- If pipeline doesn't auto-start on registration, this is a **critical gap** to fix in this story

### Key RPC Constraints

- Public RPC rate limit: ~10 RPS (configurable via `SOLARIX_RPC_RPS`)
- Always set `maxSupportedTransactionVersion: 0` or v0 transactions are silently dropped
- `getProgramAccounts` has no pagination — uses `dataSlice` for pubkey-only fetch, then batch `getMultipleAccounts` (max 100)
- `logsSubscribe` supports exactly 1 program filter per subscription

### Docker Environment

```bash
# Clean start
docker compose down -v && docker compose up --build -d

# Watch logs (JSON format)
docker compose logs -f solarix

# Verify health
curl -s http://localhost:3000/health | jq .

# Connect to PostgreSQL
docker compose exec postgres psql -U solarix -d solarix

# Override RPC URL
SOLANA_RPC_URL=https://api.devnet.solana.com docker compose up --build
```

### API Quick Reference

```bash
BASE=http://localhost:3000

# Registration
curl -X POST $BASE/api/programs -H "Content-Type: application/json" \
  -d '{"program_id": "<ID>"}'

# List programs
curl -s $BASE/api/programs | jq .

# Program details (includes stats)
curl -s $BASE/api/programs/<ID> | jq .

# Instruction types
curl -s $BASE/api/programs/<ID>/instructions | jq .

# Query instructions (with filters + cursor pagination)
curl -s "$BASE/api/programs/<ID>/instructions/<NAME>?limit=5&slot_gt=300000000" | jq .

# Account types
curl -s $BASE/api/programs/<ID>/accounts | jq .

# Query accounts (with filters + offset pagination)
curl -s "$BASE/api/programs/<ID>/accounts/<TYPE>?limit=10" | jq .

# Single account by pubkey
curl -s $BASE/api/programs/<ID>/accounts/<TYPE>/<PUBKEY> | jq .

# Delete (hard)
curl -X DELETE "$BASE/api/programs/<ID>?drop_tables=true"
```

### Known Issues to Watch For (from deferred-work.md)

| Issue                                                         | Severity | What to Check               |
| ------------------------------------------------------------- | -------- | --------------------------- |
| Integration test cleanup doesn't DROP SCHEMA                  | P1       | Orphan schemas after delete |
| Status assertions expect `"registered"` vs `"schema_created"` | P1       | Check actual status values  |
| f32/f64 NaN values in decoder                                 | P2       | JSON serialization panic    |
| CPI/inner instruction decoding untested                       | P2       | Nested CPI transactions     |
| v0 `loadedAddresses` decoding untested                        | P2       | Mainnet v0 transactions     |
| `process::exit(1)` in test code (rpc.rs)                      | P1       | Not e2e but should be fixed |
| TOCTOU race in registration                                   | P2       | Concurrent registration     |
| Config cross-field validation missing                         | P3       | `pool_min > pool_max`       |

### What This Story is NOT

- NOT about writing automated test suites (that's deferred stories 6-2, 6-3)
- NOT about adding new features or refactoring
- It IS about manually exercising every bounty-judged feature end-to-end
- It IS about fixing any integration bugs discovered
- It IS about documenting working curl examples for the README

### Success Criteria

The story is done when:

1. A judge could `docker compose up`, register a program, and query indexed data
2. Cold start + streaming works without manual intervention
3. Structured JSON logs contain timestamp, level, module, contextual fields
4. All critical bugs found are fixed
5. Non-critical issues are documented in `deferred-work.md`
6. Working test program IDs, slot ranges, and curl examples saved for README reuse

### References

- [Source: _bmad-output/bounty-requirements.md] — Judging criteria, implicit requirements, submission checklist
- [Source: _bmad-output/planning-artifacts/prd.md] — API surface (12 endpoints), user journeys, success criteria
- [Source: CLAUDE.md#Solana-Specific-Constraints] — RPC limits, discriminator format, WS guarantees, v0 txs
- [Source: _bmad-output/implementation-artifacts/deferred-work.md] — Known issues inventory
- [Source: _bmad-output/planning-artifacts/research/agent-2e-decode-paths-testing-strategy.md] — 4-layer testing strategy reference

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
