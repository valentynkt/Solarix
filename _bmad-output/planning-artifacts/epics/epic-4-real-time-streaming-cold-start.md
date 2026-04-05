# Epic 4: Real-Time Streaming & Cold Start

System streams new transactions via WebSocket, handles disconnects with automatic gap backfill, and on restart resumes from the last checkpoint -- demonstrating production reliability.

## Story 4.1: WebSocket Transaction Stream

As a system,
I want to subscribe to new transactions for a program via WebSocket and decode them in real time,
So that the index stays current with on-chain activity without manual re-triggering.

**Acceptance Criteria:**

**Given** the `TransactionStream` trait in `pipeline/ws.rs`
**When** I inspect it
**Then** it defines async methods for: `subscribe(program_id) -> Result<()>`, `next() -> Result<Option<TransactionData>>`, `unsubscribe() -> Result<()>`
**And** it provides access to the last seen slot for gap detection

**Given** the `WsTransactionStream` implementation
**When** it subscribes to a program
**Then** it calls `logsSubscribe` with exactly one program filter (mentions: [program_id]) and `commitment: "confirmed"`
**And** for each log notification received, it extracts the transaction signature
**And** it follows up with `getTransaction` (via the rate-limited RPC client) to fetch the full transaction data
**And** the full transaction is decoded via `SolarixDecoder` and passed to the writer

**Given** the WebSocket connection is active
**When** 30 seconds pass without receiving a message
**Then** the client sends a ping frame
**And** if no pong is received within 10 seconds, the connection is considered stale and reconnection is triggered

**Given** the WebSocket receives a transaction signature
**When** the signature is checked against the in-memory dedup set
**Then** duplicates are discarded without further processing
**And** the dedup set is bounded at ~10,000 entries using a `VecDeque` as eviction queue alongside a `HashSet`
**And** when the set exceeds the bound, the oldest entries are evicted first

**Given** logsSubscribe is called with multiple program IDs
**When** the subscription is attempted
**Then** it creates separate WebSocket subscriptions per program (logsSubscribe supports exactly 1 program filter)

## Story 4.2: Streaming Pipeline & Gap Detection

As an operator,
I want the system to detect gaps in indexed data after a WebSocket disconnect and automatically backfill missed slots,
So that no transactions are lost during network interruptions.

**Acceptance Criteria:**

**Given** the pipeline is in `Streaming` state
**When** the WebSocket disconnects unexpectedly
**Then** the PipelineOrchestrator transitions to `CatchingUp` state
**And** it records the last known streaming slot from the `_checkpoints` table
**And** it attempts reconnection with exponential backoff via `backon`

**Given** the pipeline is in `CatchingUp` state
**When** reconnection succeeds
**Then** it fetches the current chain tip slot
**And** it mini-backfills the gap (from last checkpoint slot to current tip) using the same batch logic as Epic 3
**And** gap data is written with `INSERT ON CONFLICT DO NOTHING` (dedup against any streaming data that arrived)
**And** after gap is filled, the pipeline transitions back to `Streaming`

**Given** the pipeline attempts reconnection
**When** 15 consecutive reconnection attempts fail OR 5 minutes of total retry time elapses
**Then** the pipeline transitions to `ShuttingDown` with a fatal error logged at `error!` level
**And** the `indexer_state` table is updated with `status = 'error'` and the error message

**Given** more than 100 consecutive block fetches fail (after individual retries)
**When** the pipeline detects this threshold
**Then** it transitions to `ShuttingDown` with a fatal error indicating possible RPC or network issues

**Given** the pipeline is in `Streaming` state
**When** it is actively processing
**Then** the `indexer_state.last_heartbeat` is updated every 10 seconds
**And** the `_checkpoints` 'realtime' stream `last_slot` is updated with each processed transaction's slot

## Story 4.3: Cold Start & Graceful Shutdown

As an operator,
I want the system to seamlessly resume indexing from where it left off after a restart, and shut down cleanly preserving all state,
So that no data is lost across restarts and the system is production-reliable.

**Acceptance Criteria:**

**Given** the system starts with an existing checkpoint in `_checkpoints`
**When** the PipelineOrchestrator initializes
**Then** it reads the last processed slot from `_checkpoints`
**And** it fetches the current chain tip via `get_slot()`
**And** if `last_processed_slot < current_tip`, it detects a gap and transitions to `Backfilling` to fill the gap
**And** after the gap is filled, it transitions to `Streaming`

**Given** `last_processed_slot > current_tip` (checkpoint ahead of chain)
**When** the pipeline initializes
**Then** it logs a fatal error indicating possible clock skew, wrong cluster, or misconfiguration
**And** it refuses to start and exits with a non-zero status code

**Given** the system starts with no prior checkpoint and `SOLARIX_START_SLOT` is not set
**When** the PipelineOrchestrator initializes
**Then** it defaults to the current chain tip slot and enters `Streaming` mode directly (no backfill from genesis)

**Given** the pipeline is running with Option C (concurrent backfill + streaming)
**When** both paths write to the same tables
**Then** `INSERT ON CONFLICT DO NOTHING` ensures no duplicate data
**And** backfill and streaming operate independently with their own checkpoint streams ('backfill' and 'realtime')
**And** the overlap window produces ~1,500 duplicate inserts that are silently deduplicated

**Given** the system receives SIGTERM or SIGINT
**When** the graceful shutdown sequence begins
**Then** the `CancellationToken` is triggered, propagating to all pipeline stages
**And** Phase 1: Reader/stream stops accepting new data
**And** Phase 2: Pipeline drains in-flight data through bounded channels
**And** Phase 3: Storage writer flushes remaining data to DB and updates checkpoints
**And** Phase 4: DB pool is closed and cleanup completes
**And** the process exits with status code 0

**Given** the system is killed with SIGKILL (non-graceful)
**When** it restarts
**Then** it resumes from the last committed checkpoint with at most one chunk of data loss (re-processable due to idempotent writes)

---
