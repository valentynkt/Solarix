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
