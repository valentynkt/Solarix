# Story 4.3: Cold Start & Graceful Shutdown

Status: review

## Story

As an operator,
I want the system to seamlessly resume indexing from where it left off after a restart, and shut down cleanly preserving all state,
so that no data is lost across restarts and the system is production-reliable.

## Acceptance Criteria

1. **AC1: Cold start from existing checkpoint**
   - **Given** the system starts with an existing checkpoint in `_checkpoints`
   - **When** the `PipelineOrchestrator` initializes
   - **Then** it reads the last processed slot from `_checkpoints`
   - **And** it fetches the current chain tip via `get_slot()`
   - **And** if `last_processed_slot < current_tip`, it detects a gap and transitions to `Backfilling` to fill the gap
   - **And** after the gap is filled, it transitions to `Streaming`

2. **AC2: Checkpoint ahead of chain (error)**
   - **Given** `last_processed_slot > current_tip` (checkpoint ahead of chain)
   - **When** the pipeline initializes
   - **Then** it logs a fatal error indicating possible clock skew, wrong cluster, or misconfiguration
   - **And** it refuses to start and exits with a non-zero status code

3. **AC3: Fresh start (no checkpoint)**
   - **Given** the system starts with no prior checkpoint and `SOLARIX_START_SLOT` is not set
   - **When** the `PipelineOrchestrator` initializes
   - **Then** it defaults to the current chain tip slot and enters `Streaming` mode directly (no backfill from genesis)

4. **AC4: Concurrent backfill + streaming (Option C)**
   - **Given** the pipeline is running with Option C (concurrent backfill + streaming)
   - **When** both paths write to the same tables
   - **Then** `INSERT ON CONFLICT DO NOTHING` ensures no duplicate data
   - **And** backfill and streaming operate independently with their own checkpoint streams (`'backfill'` and `'realtime'`)
   - **And** the overlap window produces duplicate inserts that are silently deduplicated

5. **AC5: Graceful shutdown on SIGTERM/SIGINT**
   - **Given** the system receives SIGTERM or SIGINT
   - **When** the graceful shutdown sequence begins
   - **Then** the `CancellationToken` is triggered, propagating to all pipeline stages
   - **And** Phase 1: Reader/stream stops accepting new data
   - **And** Phase 2: Pipeline drains in-flight data through bounded channels
   - **And** Phase 3: Storage writer flushes remaining data to DB and updates checkpoints
   - **And** Phase 4: DB pool is closed and cleanup completes
   - **And** the process exits with status code 0

6. **AC6: SIGKILL crash recovery**
   - **Given** the system is killed with SIGKILL (non-graceful)
   - **When** it restarts
   - **Then** it resumes from the last committed checkpoint with at most one chunk of data loss (re-processable due to idempotent writes)

7. **AC7: Config additions**
   - **Given** `Config` in `config.rs`
   - **When** I inspect it
   - **Then** it has: `shutdown_drain_secs: u64` (default 15, env `SOLARIX_SHUTDOWN_DRAIN_SECS`), `shutdown_db_flush_secs: u64` (default 10, env `SOLARIX_SHUTDOWN_DB_FLUSH_SECS`)

8. **AC8: Unit tests**
   - **Given** the test module
   - **When** I run `cargo test`
   - **Then** tests verify: cold start decision logic (gap, no gap, checkpoint ahead, fresh start), shutdown config defaults, pipeline orchestrator `run` method is Send

## Tasks / Subtasks

- [x] Task 1: Add config fields (AC: #7)
  - [x]Add `shutdown_drain_secs: u64` (env `SOLARIX_SHUTDOWN_DRAIN_SECS`, default 15) to Config
  - [x]Add `shutdown_db_flush_secs: u64` (env `SOLARIX_SHUTDOWN_DB_FLUSH_SECS`, default 10) to Config
  - [x]Update `make_test_config()` helpers in `pipeline/mod.rs` tests and `api/handlers.rs` tests

- [x] Task 2: Implement cold start logic on PipelineOrchestrator (AC: #1, #2, #3)
  - [x]Add `pub async fn determine_initial_state(&self, program_id: &str, schema_name: &str) -> Result<InitialState, PipelineError>` to `PipelineOrchestrator`
  - [x]Define `pub enum InitialState { Backfill { start_slot: u64, end_slot: u64 }, Stream, }` in `pipeline/mod.rs`
  - [x]Implementation:
    - Read checkpoint via `self.writer.read_checkpoint(schema_name, "backfill")` and `self.writer.read_checkpoint(schema_name, "realtime")`
    - Take the max `last_slot` across both checkpoint streams (backfill and realtime)
    - Fetch chain tip via `self.rpc.get_slot()`
    - If no checkpoint exists: use `self.config.start_slot` if set, else current chain tip → `InitialState::Stream` (no history)
    - If checkpoint exists and `last_slot < chain_tip`: `InitialState::Backfill { start_slot: last_slot + 1, end_slot: chain_tip }`
    - If checkpoint exists and `last_slot >= chain_tip` (within 1 slot tolerance): `InitialState::Stream`
    - If checkpoint exists and `last_slot > chain_tip + 1`: return `Err(PipelineError::Fatal("checkpoint ahead of chain tip..."))`
  - [x]Log the decision at `info!` level with all relevant slot numbers

- [x] Task 3: Implement `run` orchestrator entry point (AC: #1, #3, #4)
  - [x]Add `pub async fn run(&self, program_id: &str, schema_name: &str, idl: &Idl) -> Result<(), PipelineError>` to `PipelineOrchestrator`
  - [x]Call `determine_initial_state` to get `InitialState`
  - [x]Match on `InitialState`:
    - `Stream` → call `self.run_streaming(program_id, schema_name, idl).await`
    - `Backfill { start_slot, end_slot }` → spawn concurrent backfill + streaming (Option C):
      1. Clone the `CancellationToken` for both tasks
      2. Spawn streaming as a `tokio::spawn` task (requires owned values — clone pool, rpc, config, writer, decoder ref)
      3. Run backfill in the current task via `self.run_backfill(program_id, schema_name, idl, start_slot, end_slot).await`
      4. After backfill completes (or errors): streaming task continues independently
      5. If backfill errors: cancel everything, propagate error
      6. Await the streaming task's JoinHandle
  - [x]Important: `PipelineOrchestrator` fields are not all `Clone`. For the streaming spawn, build a second `PipelineOrchestrator` from shared components: `pool.clone()`, `rpc.clone()` (if Clone) or use `Arc` wrapping. Check if `RpcClient` is `Clone` — if not, wrap in `Arc` or restructure.

- [x] Task 4: Wire pipeline into main.rs (AC: #4, #5)
  - [x]After DB bootstrap + registry setup, create `PipelineOrchestrator` with `CancellationToken`
  - [x]Create a shared `CancellationToken` that is used by BOTH the pipeline AND the API server shutdown
  - [x]Signal handling: on SIGTERM/SIGINT, cancel the token → pipeline stages stop, API server shuts down
  - [x]Use `tokio::select!` to run pipeline + API server concurrently:
    ```rust
    tokio::select! {
        result = pipeline_task => { /* pipeline exited */ }
        result = api_server => { /* API server exited */ }
    }
    ```
  - [x]Pipeline task: for each registered program, call `orchestrator.run(program_id, schema_name, idl)`. For MVP, handle a single program (programs are registered via API first, pipeline is started manually or auto-discovers registered programs)
  - [x]Design decision: At startup, query `programs` table for registered programs. If none, only run API server (pipeline starts when a program is registered). If one or more exist, start pipeline for each.
  - [x]Refactor `shutdown_signal()` to cancel the `CancellationToken` instead of only being used by axum

- [x] Task 5: Implement graceful shutdown sequence (AC: #5, #6)
  - [x]Shutdown is driven by `CancellationToken` cancellation:
    1. Signal handler cancels the token
    2. Pipeline orchestrator's `run_streaming` / `run_backfill` checks `cancel.is_cancelled()` and exits loops
    3. Writer tasks drain remaining channel items (already implemented in `writer_task`)
    4. After pipeline tasks complete, update `indexer_state` status to `"stopped"` with `last_processed_slot`
    5. Close DB pool
  - [x]Add final checkpoint update: after pipeline exits, call `update_indexer_state(pool, program_id, "stopped", last_slot)` with timeout
  - [x]Use `tokio::time::timeout` with `config.shutdown_drain_secs` to wait for pipeline drain
  - [x]Use `tokio::time::timeout` with `config.shutdown_db_flush_secs` for final DB operations
  - [x]Exit code: 0 on clean shutdown, 1 on error

- [x] Task 6: Unit tests (AC: #8)
  - [x]`test_determine_initial_state_gap` — checkpoint at slot 100, chain tip at 200 → `Backfill { 101, 200 }`
  - [x]`test_determine_initial_state_no_gap` — checkpoint at slot 200, chain tip at 200 → `Stream`
  - [x]`test_determine_initial_state_fresh_start` — no checkpoint, no start_slot → `Stream`
  - [x]`test_determine_initial_state_fresh_start_with_start_slot` — no checkpoint, start_slot=100, chain_tip=200 → `Backfill { 100, 200 }`
  - [x]`test_determine_initial_state_checkpoint_ahead` — checkpoint at 300, chain tip at 200 → fatal error
  - [x]`test_shutdown_config_defaults` — verify default values for shutdown_drain_secs and shutdown_db_flush_secs
  - [x]`test_run_is_send` — compile-time check that `run()` future is Send

- [x] Task 7: Verify (AC: all)
  - [x]`cargo build` compiles (0 errors, 0 warnings)
  - [x]`cargo clippy` passes
  - [x]`cargo fmt -- --check` passes
  - [x]`cargo test` — all tests pass (existing + new)

## Dev Notes

### Current Codebase State

**`src/main.rs`** currently:

- Parses config, initializes tracing, connects DB, bootstraps system tables
- Creates `IdlManager`, `ProgramRegistry` (wrapped in `Arc<RwLock<>>`)
- Builds `AppState`, creates axum router
- Runs `axum::serve` with `with_graceful_shutdown(shutdown_signal())`
- `shutdown_signal()` handles SIGINT + SIGTERM (unix) — but only controls API server shutdown
- **No pipeline wiring** — does not create `PipelineOrchestrator` or run any indexing

**`src/pipeline/mod.rs`** currently has:

- `PipelineOrchestrator` with `run_backfill()`, `run_streaming()`, `run_account_snapshot()`, `mini_backfill()` — all fully implemented
- `stream_events()`, `decode_transaction()`, `decode_block()` — all working
- `WriteBatch`, `BackfillProgress`, `StreamInterrupt` — all defined
- `writer_task()` — drains channel items on cancellation
- `update_indexer_state()`, `increment_indexer_counters()` — free functions for DB updates
- `compute_backfill_chunks()`, `instruction_targets_program()`, `enrich_instruction()` — helpers
- **Missing:** `determine_initial_state()`, `run()` entry point, `InitialState` enum

**`src/config.rs`** currently has 25 fields — missing `shutdown_drain_secs` and `shutdown_db_flush_secs`

### Key Design: Cold Start Decision Tree

```rust
pub enum InitialState {
    /// Gap exists between last checkpoint and chain tip — backfill needed.
    Backfill { start_slot: u64, end_slot: u64 },
    /// Fully caught up or fresh start — go straight to streaming.
    Stream,
}

pub async fn determine_initial_state(
    &self,
    program_id: &str,
    schema_name: &str,
) -> Result<InitialState, PipelineError> {
    // Read both checkpoint streams
    let backfill_cp = self.writer.read_checkpoint(schema_name, "backfill").await?;
    let realtime_cp = self.writer.read_checkpoint(schema_name, "realtime").await?;

    let last_slot = [backfill_cp, realtime_cp]
        .iter()
        .filter_map(|cp| cp.as_ref().map(|c| c.last_slot))
        .max();

    let chain_tip = self.rpc.get_slot().await?;

    match last_slot {
        None => {
            // Fresh start
            match self.config.start_slot {
                Some(start) if start < chain_tip => {
                    info!(start_slot = start, chain_tip, "fresh start with backfill from configured start_slot");
                    Ok(InitialState::Backfill { start_slot: start, end_slot: chain_tip })
                }
                _ => {
                    info!(chain_tip, "fresh start, streaming from current tip");
                    Ok(InitialState::Stream)
                }
            }
        }
        Some(last) => {
            if last > chain_tip.saturating_add(1) {
                Err(PipelineError::Fatal(format!(
                    "checkpoint slot ({last}) ahead of chain tip ({chain_tip}). \
                     Possible causes: wrong cluster, RPC behind, or misconfiguration."
                )))
            } else if last < chain_tip {
                let gap = chain_tip - last;
                info!(last_checkpoint = last, chain_tip, gap, "resuming with backfill to close gap");
                Ok(InitialState::Backfill { start_slot: last + 1, end_slot: chain_tip })
            } else {
                info!(last_checkpoint = last, chain_tip, "fully caught up, entering streaming");
                Ok(InitialState::Stream)
            }
        }
    }
}
```

### Key Design: Option C Concurrent Backfill + Streaming

```rust
pub async fn run(
    &self,
    program_id: &str,
    schema_name: &str,
    idl: &Idl,
) -> Result<(), PipelineError> {
    let initial = self.determine_initial_state(program_id, schema_name).await?;

    match initial {
        InitialState::Stream => {
            self.run_streaming(program_id, schema_name, idl).await
        }
        InitialState::Backfill { start_slot, end_slot } => {
            // Option C: run backfill + streaming concurrently
            // Both write to the same tables; INSERT ON CONFLICT DO NOTHING handles dedup

            // Spawn streaming in a separate task
            let stream_cancel = self.cancel.child_token();
            let stream_handle = {
                // Build a second orchestrator for the streaming task
                // (needs owned values to be 'static + Send)
                let orch2 = PipelineOrchestrator::new(
                    self.pool.clone(),
                    self.rpc.clone(),     // RpcClient must be Clone or Arc-wrapped
                    self.decoder.clone(), // Box<dyn SolarixDecoder> — need Clone on trait or Arc
                    (*self.writer).clone(), // or Arc::clone
                    self.config.clone(),
                    stream_cancel.clone(),
                );
                let pid = program_id.to_string();
                let sn = schema_name.to_string();
                let idl_clone = idl.clone();
                tokio::spawn(async move {
                    orch2.run_streaming(&pid, &sn, &idl_clone).await
                })
            };

            // Run backfill in current task
            let backfill_result = self.run_backfill(
                program_id, schema_name, idl, start_slot, end_slot
            ).await;

            match backfill_result {
                Ok(()) => {
                    info!("backfill complete, streaming continues");
                    // Wait for streaming (exits on cancel or fatal error)
                    match stream_handle.await {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(PipelineError::Fatal(format!("streaming task panicked: {e}"))),
                    }
                }
                Err(e) => {
                    error!(error = %e, "backfill failed, cancelling streaming");
                    stream_cancel.cancel();
                    let _ = stream_handle.await;
                    Err(e)
                }
            }
        }
    }
}
```

**Critical consideration:** `PipelineOrchestrator` holds `Box<dyn SolarixDecoder>` which is not `Clone`. Options:

1. Change `decoder` field to `Arc<dyn SolarixDecoder>` — simplest, decoder is read-only
2. Implement `Clone` on `ChainparserDecoder` and add `clone_box()` to the trait

**Recommended:** Change `decoder: Box<dyn SolarixDecoder>` to `decoder: Arc<dyn SolarixDecoder>` across the codebase. This is a minor refactor — `decode_instruction` and `decode_account` take `&self`, so `Arc` works perfectly. Update `PipelineOrchestrator::new()` to accept `Arc<dyn SolarixDecoder>`.

Similarly, `RpcClient` may not be `Clone`. Check its fields:

- If it holds non-Clone resources (governor RateLimiter), wrap it in `Arc` or make the orchestrator hold `Arc<RpcClient>`

The `writer` field is already `Arc<StorageWriter>` — ready for sharing.

### Key Design: main.rs Wiring

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ... existing: parse config, init tracing, connect DB, bootstrap tables ...

    let cancel = CancellationToken::new();

    // Signal handler
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        cancel_signal.cancel();
    });

    // ... existing: create registry, AppState ...

    // Start API server
    let api_cancel = cancel.clone();
    let api_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(api_cancel.cancelled_owned())
            .await
    });

    // Query registered programs and start pipeline for each
    let programs = query_registered_programs(&pool).await?;

    if programs.is_empty() {
        info!("no registered programs, running API server only");
        api_handle.await??;
    } else {
        // For MVP: single program pipeline
        let program = &programs[0];
        let orch = PipelineOrchestrator::new(pool.clone(), rpc, decoder, writer, config.clone(), cancel.clone());

        let pipeline_handle = tokio::spawn(async move {
            orch.run(&program.program_id, &program.schema_name, &program.idl).await
        });

        // Wait for either to finish
        tokio::select! {
            result = pipeline_handle => {
                match result {
                    Ok(Ok(())) => info!("pipeline exited cleanly"),
                    Ok(Err(e)) => error!(error = %e, "pipeline error"),
                    Err(e) => error!(error = %e, "pipeline task panicked"),
                }
                cancel.cancel(); // Stop API server
            }
            result = api_handle => {
                // API server exited (shouldn't happen normally)
                // Keep pipeline running? Or cancel?
            }
        }
    }

    // Graceful shutdown: final DB updates
    // ... timeout-wrapped indexer_state update to "stopped" ...
    // ... pool.close() ...

    info!("shutdown complete");
    Ok(())
}
```

### Querying Registered Programs

Need a helper to load registered programs from the `programs` table at startup. Query:

```sql
SELECT p.program_id, p.schema_name, p.idl_json
FROM programs p
WHERE p.status = 'schema_created'
```

This function should live in `main.rs` or as a method on `ProgramRegistry`. The IDL JSON must be parsed into `Idl`. Programs with `status = 'registering'` (incomplete registration) should be skipped.

Alternatively, the pipeline could be started per-program via the API (POST to a `/api/pipeline/start` endpoint). For MVP simplicity, auto-discover at startup is sufficient.

**Important:** The `programs` table stores `idl_json TEXT` (raw JSON) and `schema_name TEXT`. The `ProgramRegistry` in-memory cache also has this data. At startup, either:

1. Load from DB directly (fresh data)
2. Use `registry.read().await` to get cached programs

Option 1 is safer — don't depend on in-memory state that hasn't been populated yet. But the registry IS populated during program registration. For startup, load from DB.

### Graceful Shutdown Sequence

The shutdown is inherently simple because `CancellationToken` propagation already handles most of it:

1. **Signal received** → `cancel.cancel()`
2. **Pipeline:** `run_backfill` / `run_streaming` check `cancel.is_cancelled()` in their loops and exit
3. **Writer tasks:** `writer_task` drains channel on cancellation (already implemented)
4. **API server:** `with_graceful_shutdown(cancel.cancelled_owned())` stops accepting new requests
5. **Final checkpoint:** Update `indexer_state` to `"stopped"` with timeout
6. **Pool close:** `pool.close().await`

The phased timeout design from the research doc (15s drain, 10s DB flush, 5s cleanup) maps to:

- Drain timeout: `tokio::time::timeout(Duration::from_secs(config.shutdown_drain_secs), pipeline_handle)`
- DB flush timeout: `tokio::time::timeout(Duration::from_secs(config.shutdown_db_flush_secs), final_checkpoint_update)`
- Cleanup: pool.close() (fast, no timeout needed)

### `RpcClient` Clone / Arc Analysis

Check `src/pipeline/rpc.rs` for `RpcClient` fields. It likely holds:

- `reqwest::Client` (Clone)
- `governor::RateLimiter<...>` (NOT Clone — wraps AtomicU64 internally)
- Config values (Clone)

If `RpcClient` is not `Clone`, change `PipelineOrchestrator.rpc` to `Arc<RpcClient>`. This is a non-breaking change since all methods take `&self`.

### Dependencies Already Implemented

| Component                   | Location            | Interface                                                        |
| --------------------------- | ------------------- | ---------------------------------------------------------------- |
| `PipelineOrchestrator`      | `pipeline/mod.rs`   | `run_backfill()`, `run_streaming()`, `run_account_snapshot()`    |
| `WsTransactionStream`       | `pipeline/ws.rs`    | `subscribe()`, `next()`, `unsubscribe()`, `last_seen_slot()`     |
| `RpcClient`                 | `pipeline/rpc.rs`   | `get_transaction()`, `get_blocks()`, `get_block()`, `get_slot()` |
| `StorageWriter`             | `storage/writer.rs` | `write_block()`, `read_checkpoint()`                             |
| `CancellationToken`         | `tokio-util`        | Already accepted by `PipelineOrchestrator::new()`                |
| `shutdown_signal()`         | `main.rs`           | Handles SIGINT + SIGTERM                                         |
| `update_indexer_state()`    | `pipeline/mod.rs`   | Free function for DB status update                               |
| `writer_task()`             | `pipeline/mod.rs`   | Drains channel on cancellation                                   |
| `compute_backfill_chunks()` | `pipeline/mod.rs`   | Chunking algorithm                                               |

### What This Story Does NOT Do

- Does NOT implement multi-program concurrent indexing (MVP: single program at startup, others added via API but not auto-started)
- Does NOT implement hot-reload of programs (requires restarting the pipeline)
- Does NOT implement health check integration for pipeline state (story 5-4 already enhanced health endpoint, but pipeline liveness reporting can be improved later)
- Does NOT add `#[instrument]` tracing spans (story 6-1)
- Does NOT implement Kubernetes-specific shutdown handling (terminationGracePeriodSeconds)
- Does NOT implement DLQ (dead letter queue) for failed items
- Does NOT add docker-compose restart policy (noted in deferred-work.md — address separately)

### Previous Story Learnings

**From story 4.2 (Streaming Pipeline):**

- `run_streaming` loops: subscribe → stream events → on disconnect: catch up → reconnect → loop back
- `stream_events` returns `StreamInterrupt::Disconnect(slot)` or `StreamInterrupt::Fatal(e)`
- `mini_backfill` reuses `process_chunk` + `writer_task` pattern
- `WsTransactionStream::new(&config)` doesn't connect — `subscribe()` opens the connection
- Writer task is spawned per-backfill/per-minibackfill — it's a scoped task, not a long-lived daemon
- Free functions for DB ops (`update_indexer_state`, etc.) for Send friendliness

**From story 4.1 (WebSocket):**

- `WsTransactionStream` is Send when using `native-tls` feature
- `TransactionStream` trait uses `#[async_trait]` for object safety
- Scoped borrow pattern in `next()` avoids `!Send` issues

**From story 3.5 (Pipeline Orchestrator):**

- `StorageWriter` wrapped in `Arc` for sharing with spawned writer tasks
- `CancellationToken` accepted in constructor, checked in all loops
- `Box<dyn SolarixDecoder>` — not Clone-able, needs Arc for sharing

**From story 5-1 (!Send blocker):**

- `Box::pin(async move { ... })` with `+ Send` on leaf functions
- Owned parameters for 'static futures when spawning tasks

### File Structure

| File                  | Action | Purpose                                                                              |
| --------------------- | ------ | ------------------------------------------------------------------------------------ |
| `src/config.rs`       | Modify | Add `shutdown_drain_secs`, `shutdown_db_flush_secs`                                  |
| `src/pipeline/mod.rs` | Modify | Add `InitialState` enum, `determine_initial_state()`, `run()` method                 |
| `src/main.rs`         | Modify | Wire pipeline + API server concurrently, shared CancellationToken, graceful shutdown |
| `src/api/handlers.rs` | Modify | Update `make_config()` test helper with new config fields                            |

**Potentially modified (if Arc refactor needed):**
| `src/pipeline/mod.rs` | Modify | Change `decoder` field from `Box<dyn SolarixDecoder>` to `Arc<dyn SolarixDecoder>` |
| `src/pipeline/rpc.rs` | Modify | May need to wrap `RpcClient` in Arc or add Clone |
| `src/decoder/mod.rs` | Modify | If decoder changes to Arc, update trait bounds |

**DO NOT modify:** `src/pipeline/ws.rs`, `src/storage/writer.rs`, `src/storage/schema.rs`, `src/storage/queries.rs`, `src/types.rs`, `src/registry.rs`, `src/idl/`

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests — use `?` with `map_err` to PipelineError
- NO `println!` — use `tracing` macros (`debug!`, `warn!`, `info!`, `error!`)
- NO blocking calls on the Tokio runtime
- NO `std::process::exit()` — return proper errors and let main handle exit codes
- NO `backoff` crate — use `backon` (RUSTSEC-2025-0012)
- NO hardcoded timeout values — use config fields
- DO use `CancellationToken` for all shutdown coordination (no ad-hoc AtomicBool flags)
- DO use `tokio::time::timeout` for shutdown phase timeouts
- DO update `indexer_state` to `"stopped"` on clean shutdown
- DO handle the case where no programs are registered (API-only mode)
- DO use `child_token()` for sub-tasks (allows cancelling children without parent)
- DO reuse existing `shutdown_signal()` from main.rs (just wire it to CancellationToken)
- DO check `self.cancel.is_cancelled()` at the top of `run()` before doing expensive initialization

### Testing Strategy

Unit tests in `#[cfg(test)] mod tests` at the bottom of `pipeline/mod.rs`:

1. **`test_initial_state_gap`** — mock checkpoint at slot 100, mock chain tip at 200 → verify returns `Backfill { 101, 200 }`
2. **`test_initial_state_no_gap`** — checkpoint == chain tip → `Stream`
3. **`test_initial_state_fresh`** — no checkpoint, no start_slot → `Stream`
4. **`test_initial_state_fresh_with_start_slot`** — no checkpoint, start_slot=100, chain_tip=200 → `Backfill { 100, 200 }`
5. **`test_initial_state_checkpoint_ahead`** — checkpoint > chain_tip + 1 → `Err(Fatal)`
6. **`test_shutdown_config_defaults`** — verify Config defaults for new fields
7. **`test_run_is_send`** — compile-time check that `run()` future is Send

**Note:** `determine_initial_state` requires real DB + RPC to test fully. The cold start logic can be tested by extracting the decision into a pure function that takes slot values as parameters. This avoids needing mock RPC/DB for unit tests.

```rust
fn decide_initial_state(
    last_checkpoint_slot: Option<u64>,
    chain_tip: u64,
    config_start_slot: Option<u64>,
) -> Result<InitialState, PipelineError> {
    // Pure logic — easy to unit test
}
```

Then `determine_initial_state` calls this after reading checkpoint and chain tip.

Integration tests (full cold start → backfill → streaming → shutdown cycle) are deferred to the e2e-verification-sprint-4 gate.

### Project Structure Notes

- `src/main.rs` orchestrates startup: config → DB → registry → pipeline + API
- Pipeline orchestrator owns its lifecycle; main.rs spawns it and monitors via JoinHandle
- `CancellationToken` is the single coordination primitive for shutdown
- `shutdown_signal()` already handles SIGINT + SIGTERM; just needs to trigger the shared token
- The `AppState` struct currently holds `config: Config` and `pool: PgPool` — pipeline does NOT share AppState (has its own copies)

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-4-real-time-streaming-cold-start.md#Story 4.3]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Transport & Pipeline]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Checkpoint Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Graceful shutdown]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#4. Cold Start Algorithm]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#7. Handoff Strategy]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#8. Graceful Shutdown Sequence]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#10. Configuration Parameters]
- [Source: _bmad-output/implementation-artifacts/4-2-streaming-pipeline-and-gap-detection.md]
- [Source: _bmad-output/implementation-artifacts/4-1-websocket-transaction-stream.md]
- [Source: _bmad-output/implementation-artifacts/3-5-batch-indexing-pipeline-orchestrator.md]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

None — clean build on first attempt.

### Completion Notes List

- Added `shutdown_drain_secs` (default 15) and `shutdown_db_flush_secs` (default 10) config fields
- Implemented `decide_initial_state()` pure function for cold start decision logic (testable without DB/RPC)
- Added `InitialState` enum: `Backfill { start_slot, end_slot }` or `Stream`
- Added `determine_initial_state()` async method on `PipelineOrchestrator` — reads both checkpoint streams, fetches chain tip, delegates to pure function
- Added `run()` entry point on `PipelineOrchestrator` implementing Option C concurrent backfill + streaming via `tokio::spawn`
- Changed `decoder` field from `Box<dyn SolarixDecoder>` to `Arc<dyn SolarixDecoder>` for sharing across concurrent tasks
- Added `#[derive(Clone)]` to `RpcClient` (all fields already Clone-able; shares rate limiter via Arc)
- Rewrote `main.rs` with shared `CancellationToken` for signal-driven shutdown across pipeline + API
- `shutdown_signal()` cancels the shared token on SIGTERM/SIGINT, propagating to all pipeline stages and API server
- `graceful_shutdown()` function: timeout-wrapped indexer_state update to "stopped" + pool.close()
- `query_registered_programs()` queries DB for `schema_created` programs (IDL persistence not yet implemented — logs warning, users re-register after restart)
- 10 new unit tests: cold start decision (gap, no gap, fresh, fresh+start_slot, checkpoint ahead, one-ahead tolerance, start_slot at tip), shutdown config defaults, `run()` is Send, InitialState Debug
- All 251 tests pass, clippy clean, fmt clean, zero warnings

### File List

- `src/config.rs` — added `shutdown_drain_secs`, `shutdown_db_flush_secs` fields
- `src/pipeline/mod.rs` — added `InitialState` enum, `decide_initial_state()`, `determine_initial_state()`, `run()` method; changed decoder to `Arc<dyn SolarixDecoder>`; 10 new unit tests
- `src/pipeline/rpc.rs` — added `#[derive(Clone)]` to `RpcClient`
- `src/main.rs` — complete rewrite: shared CancellationToken, signal handler wiring, concurrent pipeline+API via tokio::select!, graceful shutdown with timeouts, startup program discovery
- `src/api/handlers.rs` — updated `make_config()` test helper with new config fields

### Change Log

- 2026-04-07: Implemented cold start, Option C concurrent backfill+streaming, graceful shutdown, main.rs pipeline wiring (story 4.3)
