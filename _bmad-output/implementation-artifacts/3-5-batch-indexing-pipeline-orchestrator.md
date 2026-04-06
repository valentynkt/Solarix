# Story 3.5: Batch Indexing Pipeline Orchestrator

Status: ready-for-dev

## Story

As a user,
I want to index historical transactions for a registered program by specifying a slot range,
so that past on-chain activity is captured in the database for querying.

## Acceptance Criteria

1. **AC1: PipelineOrchestrator struct and initialization**
   - **Given** the `PipelineOrchestrator` in `pipeline/mod.rs`
   - **When** I inspect it
   - **Then** it holds: `Config` (owned), `PgPool` (owned), `RpcClient` (owned), `ChainparserDecoder` (via `Box<dyn SolarixDecoder>`), `StorageWriter` (owned), `CancellationToken` (from `tokio-util`)
   - **And** `PipelineOrchestrator::new(...)` constructs it from these dependencies
   - **And** it replaces the current empty struct stub

2. **AC2: Slot-range backfill**
   - **Given** a registered program with schema created
   - **When** `run_backfill(program_id, schema_name, idl, start_slot, end_slot)` is called
   - **Then** it reads the checkpoint from `_checkpoints` (stream = `"backfill"`) and resumes from `max(checkpoint.last_slot + 1, start_slot)`
   - **And** it chunks the remaining range into operational chunks (default 50K slots, from `Config.backfill_chunk_size`)
   - **And** for each chunk: calls `get_blocks(start, end)` to get actual block slots, then fetches each block via `get_block(slot)`, filters transactions for the target `program_id`, decodes matching instructions via `SolarixDecoder.decode_instruction()`, and sends decoded data through a bounded `tokio::sync::mpsc` channel (capacity from `Config.channel_capacity`, default 256) to a writer task
   - **And** the writer task calls `StorageWriter.write_block()` for each block's decoded data

3. **AC3: Transaction filtering**
   - **Given** a fetched `RpcBlock` containing transactions
   - **When** the pipeline filters for the target program
   - **Then** it checks each transaction's instructions (top-level + inner/CPI) to see if any instruction's `program_id_index` resolves to the target program_id in the transaction's `account_keys`
   - **And** only matching instructions are decoded and sent to the writer
   - **And** transactions where `success == false` are filtered out unless `Config.index_failed_txs` is true

4. **AC4: Instruction enrichment**
   - **Given** a `DecodedInstruction` returned by the decoder
   - **When** the pipeline enriches it before sending to the writer
   - **Then** it sets `signature`, `slot`, `block_time`, `instruction_index`, `inner_index`, and `accounts` (resolved from `account_keys` using instruction's account indices)
   - **And** `is_inner_ix` is derived from `inner_index.is_some()`

5. **AC5: Account snapshot**
   - **Given** a registered program
   - **When** account snapshot is triggered (as part of backfill or explicit call)
   - **Then** it calls `get_program_accounts(program_id)` to get all account pubkeys (via dataSlice trick)
   - **And** batches `get_multiple_accounts(pubkeys)` in chunks of 100
   - **And** decodes each account via `SolarixDecoder.decode_account()`
   - **And** enriches `DecodedAccount` with `pubkey`, `slot_updated`, `lamports` from the RPC response
   - **And** upserts into account tables via `StorageWriter.write_block()` (accounts-only, empty instructions)

6. **AC6: Progress logging**
   - **Given** backfill is running
   - **When** 10 seconds have elapsed since last progress log (configurable via `Config.checkpoint_interval_secs`)
   - **Then** it logs at `info!` level: slots processed, total slots, percentage, slots/sec, and ETA

7. **AC7: Checkpoint resume**
   - **Given** the pipeline was interrupted mid-backfill
   - **When** it restarts
   - **Then** it reads `_checkpoints` for stream `"backfill"` to get `last_slot`
   - **And** resumes from `last_slot + 1`
   - **And** previously written data is safely deduplicated via `ON CONFLICT DO NOTHING`

8. **AC8: Cancellation support**
   - **Given** a `CancellationToken`
   - **When** the token is cancelled during backfill
   - **Then** the pipeline stops fetching new blocks after the current chunk
   - **And** drains in-flight items from the channel
   - **And** the writer task completes its current write and exits cleanly

9. **AC9: `get_transaction` RPC method**
   - **Given** the `RpcClient` in `pipeline/rpc.rs`
   - **When** I inspect it
   - **Then** it has a `get_transaction(signature)` method that fetches a single transaction by signature via `getTransaction` RPC call
   - **And** includes `maxSupportedTransactionVersion: 0` and `encoding: "json"` in params
   - **And** returns `Result<Option<RpcTransaction>, PipelineError>`
   - **And** passes through the existing rate limiter and retry logic

10. **AC10: indexer_state updates**
    - **Given** backfill is running for a program
    - **When** backfill starts
    - **Then** it updates `indexer_state SET status = 'backfilling', last_processed_slot = start_slot`
    - **When** each chunk completes
    - **Then** it updates `indexer_state SET last_processed_slot = chunk_end_slot, last_heartbeat = NOW()`
    - **When** backfill completes
    - **Then** it updates `indexer_state SET status = 'idle', last_processed_slot = end_slot`

## Tasks / Subtasks

- [ ] Task 1: Add `get_transaction` to `RpcClient` (AC: #9)
  - [ ] Add method `pub async fn get_transaction(&self, signature: &str) -> Result<Option<RpcTransaction>, PipelineError>`
  - [ ] Build JSON-RPC request for `getTransaction` with `maxSupportedTransactionVersion: 0`, `encoding: "json"`
  - [ ] Parse response into `RpcTransaction` using existing parsing logic from `get_block`
  - [ ] Handle null result (transaction not found → `Ok(None)`)
  - [ ] Uses existing rate limiter + retry (via `send_rpc_with_retry`)

- [ ] Task 2: Define `PipelineOrchestrator` struct (AC: #1)
  - [ ] Replace empty `pub struct PipelineOrchestrator;` stub
  - [ ] Fields: `pool: PgPool`, `rpc: RpcClient`, `decoder: Box<dyn SolarixDecoder>`, `writer: StorageWriter`, `config: Config`, `cancel: CancellationToken`
  - [ ] Add `pub fn new(pool: PgPool, rpc: RpcClient, decoder: Box<dyn SolarixDecoder>, writer: StorageWriter, config: Config, cancel: CancellationToken) -> Self`
  - [ ] Keep existing `PipelineError` enum and `is_retryable()` unchanged

- [ ] Task 3: Implement backfill chunking and block fetching (AC: #2, #7)
  - [ ] Add `pub async fn run_backfill(&self, program_id: &str, schema_name: &str, idl: &Idl, start_slot: u64, end_slot: u64) -> Result<(), PipelineError>`
  - [ ] Read checkpoint via `self.writer.read_checkpoint(schema_name, "backfill")` — resume from `max(checkpoint.last_slot + 1, start_slot)`
  - [ ] Compute chunks: `compute_chunks(effective_start, end_slot, self.config.backfill_chunk_size)` → `Vec<(u64, u64)>`
  - [ ] For each chunk: call `self.rpc.get_blocks(chunk_start, chunk_end)` to get block slot list
  - [ ] For each block slot: call `self.rpc.get_block(slot)` — skip `None` (empty/skipped slots)
  - [ ] Check `self.cancel.is_cancelled()` between chunks for graceful exit

- [ ] Task 4: Implement transaction filtering (AC: #3)
  - [ ] Add `fn filter_transactions_for_program(block: &RpcBlock, program_id: &str, index_failed: bool) -> Vec<&RpcTransaction>`
  - [ ] For each transaction: check if `success == true` (or `index_failed` flag)
  - [ ] Check top-level instructions: `account_keys[ix.program_id_index] == program_id`
  - [ ] Check inner instructions: same program_id check on CPI instructions
  - [ ] Return references to matching transactions

- [ ] Task 5: Implement instruction decoding and enrichment (AC: #4)
  - [ ] For each matching transaction + instruction pair, call `decoder.decode_instruction(program_id, &ix.data, idl)`
  - [ ] On `DecodeError::UnknownDiscriminator` → `warn!`, skip instruction, continue
  - [ ] On other `DecodeError` → `warn!`, skip instruction, continue
  - [ ] Track decode failure rate per chunk — if >90% fail, log at `error!` (likely IDL mismatch)
  - [ ] Enrich `DecodedInstruction`: set `signature`, `slot`, `block_time`, `instruction_index`, `inner_index`, resolve `accounts` from `account_keys` using instruction's account index list
  - [ ] Set `program_id` on the decoded instruction

- [ ] Task 6: Implement mpsc channel + writer task (AC: #2, #8)
  - [ ] Create bounded `tokio::sync::mpsc::channel::<WriteBatch>(capacity)` where `WriteBatch` holds `(schema_name, stream, instructions, accounts, slot, signature)`
  - [ ] Spawn writer task: loop receiving from channel, call `self.writer.write_block(...)` for each batch
  - [ ] Writer task exits when channel is closed (sender dropped) or cancellation
  - [ ] On writer error: log at `error!`, propagate via oneshot or shared error state

- [ ] Task 7: Implement account snapshot (AC: #5)
  - [ ] Add `pub async fn run_account_snapshot(&self, program_id: &str, schema_name: &str, idl: &Idl) -> Result<(), PipelineError>`
  - [ ] Call `self.rpc.get_program_accounts(program_id)` → Vec of pubkeys
  - [ ] Batch into chunks of 100, call `self.rpc.get_multiple_accounts(chunk)` for each
  - [ ] For each `RpcAccountInfo`: call `decoder.decode_account(program_id, &info.pubkey, &info.data, idl)`
  - [ ] Enrich `DecodedAccount`: set `pubkey`, `slot_updated` (from current slot), `lamports`
  - [ ] Write via `self.writer.write_block(schema_name, "accounts", &[], &decoded_accounts, slot, None)`

- [ ] Task 8: Implement progress tracking (AC: #6)
  - [ ] Add `BackfillProgress` struct: `start_slot`, `end_slot`, `current_slot`, `blocks_processed`, `txs_decoded`, `started_at: Instant`
  - [ ] Methods: `percent_complete()`, `slots_per_sec()`, `eta() -> Duration`
  - [ ] Log progress at `info!` level every `config.checkpoint_interval_secs` seconds (default 10)
  - [ ] Use `tokio::time::Instant` for timing

- [ ] Task 9: Implement indexer_state updates (AC: #10)
  - [ ] Add private method `async fn update_indexer_state(pool: &PgPool, program_id: &str, status: &str, last_slot: Option<u64>) -> Result<(), PipelineError>`
  - [ ] Uses `sqlx::query()` with bind params — NOT raw string interpolation
  - [ ] Updates `status`, `last_processed_slot`, `last_heartbeat = NOW()` on `indexer_state` WHERE `program_id = $1`
  - [ ] Called at: backfill start (`"backfilling"`), chunk completion, backfill end (`"idle"`)

- [ ] Task 10: Unit tests (AC: all)
  - [ ] `test_compute_chunks` — verify chunking logic: normal range, single chunk, exact boundary, zero-length range
  - [ ] `test_filter_transactions_for_program` — program match, no match, inner instruction match, failed tx filtering
  - [ ] `test_instruction_enrichment` — verify all fields set correctly from RpcTransaction context
  - [ ] `test_backfill_progress` — verify percent_complete, slots_per_sec, eta calculations
  - [ ] `test_pipeline_orchestrator_send` — compile-time Send safety check for PipelineOrchestrator
  - [ ] `test_decode_failure_rate_tracking` — verify >90% failure triggers error log

- [ ] Task 11: Verify (AC: all)
  - [ ] `cargo build` compiles
  - [ ] `cargo clippy` passes
  - [ ] `cargo fmt -- --check` passes
  - [ ] `cargo test` — all tests pass (existing + new)

## Dev Notes

### Current Codebase State

`src/pipeline/mod.rs` currently contains:

- Empty `pub struct PipelineOrchestrator;` stub
- `PipelineError` enum with 8 variants + `is_retryable()` method
- `pub mod rpc;` and `pub mod ws;` declarations

This story replaces the empty stub with the full batch indexing orchestrator. The `PipelineError` enum and `is_retryable()` must be preserved unchanged.

### Dependencies Already Implemented

| Component            | Location            | Interface                                                                                                                            |
| -------------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `RpcClient`          | `pipeline/rpc.rs`   | `BlockSource` trait: `get_blocks`, `get_block`, `get_slot`                                                                           |
| `RpcClient`          | `pipeline/rpc.rs`   | `AccountSource` trait: `get_program_accounts`, `get_multiple_accounts`                                                               |
| `ChainparserDecoder` | `decoder/mod.rs`    | `SolarixDecoder` trait: `decode_instruction`, `decode_account`                                                                       |
| `StorageWriter`      | `storage/writer.rs` | `write_block(schema, stream, instructions, accounts, slot, sig)`, `read_checkpoint(schema, stream)`                                  |
| `ProgramInfo`        | `registry.rs`       | `program_id`, `schema_name`, `status` fields                                                                                         |
| `Config`             | `config.rs`         | `backfill_chunk_size` (50K), `channel_capacity` (256), `checkpoint_interval_secs` (10), `index_failed_txs`, `start_slot`, `end_slot` |

### RPC Types You Will Use

From `pipeline/rpc.rs`:

```rust
pub struct RpcBlock {
    pub slot: u64,
    pub block_time: Option<i64>,
    pub transactions: Vec<RpcTransaction>,
}

pub struct RpcTransaction {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub account_keys: Vec<String>,
    pub instructions: Vec<RpcInstruction>,
    pub inner_instructions: Vec<RpcInnerInstructionGroup>,
}

pub struct RpcInstruction {
    pub program_id_index: u8,
    pub data: Vec<u8>,
    pub accounts: Vec<u8>,  // indices into account_keys
}

pub struct RpcInnerInstructionGroup {
    pub index: u8,
    pub instructions: Vec<RpcInstruction>,
}

pub struct RpcAccountInfo {
    pub pubkey: String,
    pub data: Vec<u8>,
    pub lamports: u64,
    pub owner: String,
}
```

### Instruction Decoding Flow

For each matching instruction in a transaction:

1. `decoder.decode_instruction(program_id, &ix.data, &idl)` → `DecodedInstruction` (with minimal fields)
2. Enrich the result:
   ```rust
   decoded.signature = tx.signature.clone();
   decoded.slot = block.slot;
   decoded.block_time = block.block_time;
   decoded.instruction_index = ix_index as u8;
   decoded.inner_index = inner_index; // None for top-level, Some(n) for CPI
   decoded.accounts = ix.accounts.iter()
       .filter_map(|&idx| tx.account_keys.get(idx as usize).cloned())
       .collect();
   ```

### Account Decoding Flow

For each `RpcAccountInfo`:

1. `decoder.decode_account(program_id, &info.pubkey, &info.data, &idl)` → `DecodedAccount`
2. Enrich: set `slot_updated` = current_slot, `lamports` = info.lamports (decoder already sets `pubkey`, `account_type`, `data`)

### Channel Architecture

```
Backfill loop (producer)          Writer task (consumer)
    |                                   |
    |-- decode instructions ---|        |
    |-- enrich fields ---------|        |
    |-- create WriteBatch -----|        |
    |       |                           |
    |       +----> mpsc(256) ------>----+
    |                                   |
    |                           write_block(...)
```

`WriteBatch` is a simple struct holding all args for `write_block`. One batch per block.

### Missing: `get_transaction` on RpcClient

`RpcClient` currently has no `get_transaction` method. Add it in Task 1 following the existing `get_block` pattern:

```rust
impl RpcClient {
    pub async fn get_transaction(
        &self,
        signature: &str,
    ) -> Result<Option<RpcTransaction>, PipelineError> {
        // JSON-RPC: getTransaction(signature, {encoding: "json", maxSupportedTransactionVersion: 0})
        // Parse response reusing same logic as get_block transaction parsing
        // Return None if result is null (tx not found / not yet confirmed)
    }
}
```

This method is needed for story 3.5 (signature-list mode) and story 4.1 (streaming — each WS notification yields a signature that must be fetched).

### Backfill Chunk Algorithm

```rust
fn compute_chunks(start: u64, end: u64, chunk_size: u64) -> Vec<(u64, u64)> {
    let mut chunks = Vec::new();
    let mut current = start;
    while current <= end {
        let chunk_end = std::cmp::min(current + chunk_size - 1, end);
        chunks.push((current, chunk_end));
        current = chunk_end + 1;
    }
    chunks
}
```

Note: `chunk_size` is the operational chunk (default 50K), NOT the RPC limit (500K). Each operational chunk fits within a single `getBlocks` call since 50K < 500K.

### Error Handling Strategy

- **Block fetch fails** → Retry via existing `backon` retry in `RpcClient`. If all retries exhausted, log `warn!` and skip the block. Increment skip counter.
- **Skipped slot** (`PipelineError::SlotSkipped`) → Expected behavior, skip and continue (already handled by `get_block` returning `None`)
- **Decode fails** → `warn!` log, skip instruction, continue. Track failure rate per chunk.
- **Write fails** → `StorageError::WriteFailed` → propagate as `PipelineError::Storage`, halt backfill (DB issues are serious)
- **CancellationToken** → Stop after current chunk, drain channel, clean exit

### indexer_state Table Schema

From `storage/mod.rs` bootstrap DDL:

```sql
CREATE TABLE IF NOT EXISTS "indexer_state" (
    "program_id"          VARCHAR(44) PRIMARY KEY REFERENCES "programs"("program_id"),
    "status"              TEXT NOT NULL,
    "last_processed_slot" BIGINT,
    "last_heartbeat"      TIMESTAMPTZ,
    "error_message"       TEXT,
    "total_instructions"  BIGINT NOT NULL DEFAULT 0,
    "total_accounts"      BIGINT NOT NULL DEFAULT 0
);
```

Update `status` and `last_processed_slot` at backfill start/chunk-end/completion. The `total_instructions` and `total_accounts` counters should be incremented by the actual `WriteResult` counts from each `write_block` call.

### What This Story Does NOT Do

- Does NOT implement streaming pipeline (story 4.1: WebSocket, story 4.2: streaming+gap detection)
- Does NOT implement concurrent backfill+streaming handoff (story 4.2: Option C dedup)
- Does NOT implement graceful shutdown sequence (story 4.3: phased drain with timeouts)
- Does NOT modify `main.rs` to wire up the orchestrator (story 4.3: cold start integration)
- Does NOT implement CatchingUp or ShuttingDown states (story 4.2, 4.3)
- Does NOT implement in-memory dedup set for signatures (story 4.2: streaming overlap)
- Does NOT implement Semaphore-based parallel block fetching — sequential within chunk for MVP simplicity. The rate limiter in `RpcClient` already gates throughput. Parallelism is a performance optimization for post-MVP.
- Does NOT add `#[instrument]` tracing spans (deferred to story 6-1)
- Does NOT implement signature-list mode as a separate public method — **do** implement it if adding `get_transaction` is straightforward, otherwise defer to story 4.1 when the method is needed for streaming too

### CancellationToken Setup

The `CancellationToken` is provided by the caller (will be wired to signal handlers in story 4.3). For now, the orchestrator accepts it as a constructor parameter and checks `is_cancelled()` between chunks:

```rust
for (chunk_start, chunk_end) in chunks {
    if self.cancel.is_cancelled() {
        info!("backfill cancelled");
        break;
    }
    // ... process chunk
}
```

### Deferred Work from Story 3-4 Relevant Here

- `safe_u64_to_i64()` is `#[allow(dead_code)]` in `writer.rs` — consider removing if still unused after this story
- `COMMON_ACCOUNT_COLUMNS` in `writer.rs` must stay in sync with `RESERVED_ACCOUNT_COLUMNS` in `schema.rs`
- Writer does NOT update `program_stats` or `indexer_state` counters — this story adds indexer_state updates alongside the pipeline, not inside the writer

### Deferred Work Relevant from Other Stories

- **Unbounded Vec in `get_blocks` for huge ranges** — This story's chunking (50K operational chunks) naturally bounds each `get_blocks` call. Document that `backfill_chunk_size` must be <= 500K (the RPC limit).
- **`is_retryable()` includes `Idl(FetchFailed)`** — Not relevant to backfill (IDL is pre-loaded), but don't break the existing behavior.
- **Hard delete doesn't check for active pipeline** — Story 5-1 deferred this guard until pipeline exists. This story creates the pipeline; the guard should be added in story 4.3 when main.rs integration happens.

### Previous Story Learnings

**From story 3-4 (StorageWriter):**

- `write_block` takes `&self` — no ownership issues, simple async call
- `read_checkpoint` returns `Option<CheckpointInfo>` — `None` means fresh start
- No Box::pin needed — writer methods compiled Send-clean
- JSONB arrays use `sqlx::types::Json<T>` wrapper (already handled in writer)
- `StorageError::WriteFailed` is the catch-all for INSERT/transaction failures

**From story 3-3 (RPC):**

- `get_block` returns `Option<RpcBlock>` — `None` for skipped/empty slots
- `get_blocks` auto-chunks ranges > 500K internally
- All RPC calls pass through `governor` rate limiter + `backon` retry
- `SlotSkipped` variant on `PipelineError` for `-32009` errors

**From story 3-1/3-2 (Decoder):**

- `decode_instruction` returns `DecodedInstruction::from_decoded()` with minimal fields (signature="", slot=0, etc.)
- `decode_account` returns `DecodedAccount::from_decoded()` with minimal fields
- Caller must enrich with RPC context (slot, signature, accounts, etc.)
- `DecodeError::UnknownDiscriminator` is expected for non-target instructions — skip and continue

### File Structure

| File                  | Action  | Purpose                                                                              |
| --------------------- | ------- | ------------------------------------------------------------------------------------ |
| `src/pipeline/mod.rs` | Rewrite | PipelineOrchestrator: struct, new(), run_backfill(), run_account_snapshot(), helpers |
| `src/pipeline/rpc.rs` | Modify  | Add `get_transaction()` method to `RpcClient`                                        |

**DO NOT modify:** `src/storage/writer.rs`, `src/storage/schema.rs`, `src/decoder/mod.rs`, `src/types.rs`, `src/api/`, `src/config.rs`, `src/main.rs`, `src/registry.rs`

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests — use `?` with `map_err` to PipelineError
- NO `println!` — use `tracing` macros (`debug!`, `warn!`, `info!`, `error!`)
- NO blocking calls on the Tokio runtime
- NO SQL string concatenation for VALUES — use bind parameters
- NO `sqlx::query!()` compile-time macros — use runtime `sqlx::query()`
- NO parallel block fetching with `tokio::spawn` per block — use sequential fetching within chunks (rate limiter is the bottleneck, not concurrency)
- DO check `cancel.is_cancelled()` between chunks
- DO handle `get_block` returning `None` (skipped slots) gracefully
- DO handle decode failures as skip-and-continue, not fatal errors
- DO update indexer_state at chunk boundaries, not per-block

### Testing Strategy

Unit tests in `#[cfg(test)] mod tests` at the bottom of `pipeline/mod.rs`:

1. **`test_compute_chunks`** — normal range (0..150K with 50K chunks = 3 chunks), single-chunk range, exact boundary, start == end, end < start (empty)
2. **`test_filter_transactions_for_program`** — build mock `RpcBlock` with transactions: one matching program, one non-matching, one CPI match, one failed tx
3. **`test_instruction_enrichment`** — verify enriched DecodedInstruction has correct signature, slot, block_time, instruction_index, inner_index, accounts
4. **`test_backfill_progress`** — verify percent_complete (0%, 50%, 100%), slots_per_sec, eta calculations
5. **`test_pipeline_orchestrator_is_send`** — compile-time check that `PipelineOrchestrator` is `Send + Sync`
6. **`test_decode_failure_rate`** — verify tracking logic flags >90% failures

Integration tests (requiring PostgreSQL + RPC) are deferred to Epic 6.

### Project Structure Notes

- `src/pipeline/mod.rs` is the designated location per architecture docs
- Pipeline orchestrator calls `StorageWriter` and `RpcClient` — no direct DB queries for data writes
- `indexer_state` updates ARE direct SQL via `sqlx::query()` (not through writer) since they update the global system table in `public` schema
- Both backfill and streaming (story 4.x) will use the same `StorageWriter.write_block()` — dedup handled by ON CONFLICT

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-3-transaction-decoding-batch-indexing.md#Story 3.5]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Transport & Pipeline]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Checkpoint Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Process Patterns]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md#Data Flow Through Boundaries]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#3. Backfill Strategy]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#5. Checkpoint Schema]
- [Source: _bmad-output/implementation-artifacts/3-4-storage-writer-and-atomic-checkpointing.md]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md]

## Dev Agent Record

### Agent Model Used

{{agent_model_name_version}}

### Debug Log References

### Completion Notes List

### File List
