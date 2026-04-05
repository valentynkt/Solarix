# Agent 2C: Backfill Pipeline, Cold Start & Handoff Design

**Date:** 2026-04-05
**Research Type:** Architecture design -- pipeline state machine, backfill, cold start, streaming, handoff
**Dependencies:** Agent 1D (RPC Capabilities), Phase 1 Technical Research

---

## 1. Executive Summary

This document specifies the complete pipeline lifecycle for Solarix: how the indexer starts, backfills historical data, transitions to real-time streaming, handles disconnections, and shuts down cleanly. Every design decision is grounded in Solana RPC constraints (500K slot getBlocks limit, no WebSocket delivery guarantees, 10-50 RPS rate limits) and Rust async patterns (tokio bounded mpsc, CancellationToken, backoff crate).

**Key design decisions:**

| Decision               | Choice                                     | Rationale                                                                         |
| ---------------------- | ------------------------------------------ | --------------------------------------------------------------------------------- |
| Handoff strategy       | Option C: Signature-based dedup            | Simplest, no buffer management, INSERT ON CONFLICT DO NOTHING handles overlap     |
| Rate limiting          | `governor` crate (GCRA) + adaptive backoff | Production-grade, async-native, jitter support, simpler than manual token bucket  |
| Concurrency control    | `tokio::Semaphore`                         | Configurable parallel RPC requests, fair scheduling, integrates with rate limiter |
| Checkpoint granularity | Per-program, per-slot                      | Enables multi-program indexing, fine-grained restart                              |
| Shutdown timeout       | 30 seconds total, phased                   | Reader stops immediately, pipeline drains for 15s, DB flush for 10s, cleanup 5s   |
| Error classification   | 3-tier (retryable / skip / fatal)          | Retryable = backoff, skip = log + continue, fatal = halt pipeline                 |

---

## 2. Pipeline State Machine

### 2.1 State Diagram

```
                    +--------------+
                    | Initializing |
                    +------+-------+
                           |
              DB connect, load checkpoint,
              fetch current slot, compute gap
                           |
                   +-------v--------+
              +----+  gap == 0?     +----+
              |    +----------------+    |
              |YES                       |NO
              |                          |
     +--------v--------+     +----------v----------+
     |    Streaming     |     |     Backfilling     |
     +--------+---------+     +----------+----------+
              |                          |
              |  WS disconnect           | backfill complete
              |  (gap detected)          | (current_slot reached)
              |                          |
     +--------v--------+     +----------v----------+
     |   CatchingUp    |     |     Streaming       |
     +--------+---------+     +----------+----------+
              |                          |
              | mini-backfill            | (same as left)
              | complete                 |
              |                          |
     +--------v--------+                |
     |    Streaming     |                |
     +---------+--------+                |
               |                         |
               |  SIGTERM / SIGINT       |
               |  or fatal error         |
               |                         |
      +--------v---------+              |
      |  ShuttingDown    |<-------------+
      +------------------+
```

### 2.2 State Definitions

#### Initializing

**Entry conditions:** Process starts.

**Work performed:**

1. Parse configuration (env vars, CLI args)
2. Connect to PostgreSQL, run migrations
3. Load `indexer_state` for target `program_id`
4. Call `getSlot()` to get chain tip
5. Compute gap: `current_slot - last_processed_slot`
6. Determine next state

**Exit conditions:**

- Gap == 0 AND previous status was "streaming" --> Streaming
- Gap > 0 --> Backfilling
- No prior state (fresh start) --> Backfilling (from configured start_slot or current_slot)
- Gap < 0 --> Fatal error (clock went backwards or misconfiguration)
- DB connection fails --> Fatal error

**Error handling:** All errors in this state are fatal. The indexer cannot operate without a valid DB connection and RPC endpoint.

**Logging:**

```
INFO  indexer.init program_id=<id> last_slot=<n> current_slot=<m> gap=<g>
INFO  indexer.init state_transition=Backfilling reason="gap of 50000 slots detected"
ERROR indexer.init msg="database connection failed" err=<e>
```

#### Backfilling

**Entry conditions:** Gap > 0, OR fresh start, OR CatchingUp (mini-backfill).

**Work performed:**

1. Chunk the gap into 500K-slot ranges
2. For each chunk: `getBlocks(start, end)` to find actual blocks
3. For each block: `getBlock(slot)` with concurrent requests (via Semaphore)
4. Filter transactions for target program_id
5. Send matching transactions through Decoder --> Storer pipeline
6. Update checkpoint after each chunk completes
7. Stream real-time events concurrently (Option C dedup)

**Exit conditions:**

- All chunks processed, current_slot reached --> Streaming
- CancellationToken cancelled --> ShuttingDown
- Fatal error (DB down) --> ShuttingDown

**Error handling:**

- getBlocks fails --> retry with backoff (transient)
- getBlock fails for specific slot --> retry with backoff, skip after max retries, log gap
- Rate limited (HTTP 429) --> backoff with `retry_after` hint
- Malformed block data --> log, skip block, continue

**Logging:**

```
INFO  indexer.backfill chunk=3/10 start_slot=1000000 end_slot=1500000 blocks=450000
INFO  indexer.backfill progress=45.2% slots_per_sec=120 eta="12m 30s"
WARN  indexer.backfill slot=1234567 msg="block fetch failed after 5 retries, skipping"
INFO  indexer.backfill msg="backfill complete" total_slots=5000000 total_txs=123456 duration="42m 15s"
```

#### Streaming

**Entry conditions:** Backfill complete, OR gap == 0 on startup, OR CatchingUp complete.

**Work performed:**

1. Subscribe via `logsSubscribe({"mentions": ["<program_id>"]})`
2. For each notification: extract signature, call `getTransaction(sig)`
3. Send transaction through Decoder --> Storer pipeline
4. Continuously update `last_processed_slot` checkpoint
5. Ping/pong heartbeat monitoring (detect stale connections)
6. Maintain bounded dedup set of recent signatures

**Exit conditions:**

- WebSocket disconnects --> CatchingUp
- CancellationToken cancelled --> ShuttingDown
- Fatal error --> ShuttingDown

**Error handling:**

- WebSocket disconnect --> transition to CatchingUp (not an error, expected behavior)
- getTransaction fails --> retry with backoff (signature is known, data will exist)
- Duplicate signature received --> skip (dedup set hit), no error

**Logging:**

```
INFO  indexer.stream msg="subscribed to logsSubscribe" program_id=<id> commitment=confirmed
INFO  indexer.stream txs_processed=1500 slot=290000000 lag_slots=2
WARN  indexer.stream msg="websocket disconnected" last_slot=290000000 reason=<r>
DEBUG indexer.stream msg="duplicate signature skipped" sig=<s>
```

#### CatchingUp

**Entry conditions:** WebSocket disconnected while in Streaming state.

**Work performed:**

1. Record `disconnect_slot` = last processed slot before disconnect
2. Reconnect WebSocket with exponential backoff
3. Once reconnected, call `getSlot()` for current tip
4. Compute mini-gap: `current_slot - disconnect_slot`
5. If mini-gap > 0: run mini-backfill (same logic as Backfilling but smaller range)
6. Start buffering new streaming events during mini-backfill
7. Dedup by signature (INSERT ON CONFLICT DO NOTHING)

**Exit conditions:**

- Mini-backfill complete AND WebSocket reconnected --> Streaming
- Too many reconnect failures (max 15 attempts or 5 minutes) --> ShuttingDown with error
- CancellationToken cancelled --> ShuttingDown

**Error handling:**

- Reconnection fails --> exponential backoff (1s, 2s, 4s, 8s... up to 60s)
- Mini-backfill uses same error handling as Backfilling
- If gap is too large (>1 hour of slots, ~9000 blocks) --> log warning, still process

**Logging:**

```
WARN  indexer.catchup msg="entering catch-up mode" disconnect_slot=290000000 current_slot=290000500 gap=500
INFO  indexer.catchup msg="mini-backfill in progress" slots_remaining=300
INFO  indexer.catchup msg="catch-up complete, resuming streaming" duration="15s"
ERROR indexer.catchup msg="max reconnection attempts exceeded, shutting down"
```

#### ShuttingDown

**Entry conditions:** SIGTERM/SIGINT received, OR fatal error, OR max reconnect failures.

**Work performed:**

1. Cancel CancellationToken (propagates to all tasks)
2. Reader task stops accepting new work
3. Wait for in-flight items to drain through pipeline (15s timeout)
4. Flush pending database writes (10s timeout)
5. Update checkpoint with final `last_processed_slot` and status
6. Close database connection pool
7. Exit with appropriate code

**Exit conditions:** Process exits.

**Error handling:**

- Drain timeout exceeded --> log warning, proceed to DB flush
- DB flush timeout exceeded --> log error, exit anyway (checkpoint may be stale)
- Checkpoint update fails --> log error, exit (next start will re-process some data, dedup handles it)

**Logging:**

```
INFO  indexer.shutdown msg="shutdown signal received" reason=SIGTERM
INFO  indexer.shutdown msg="draining pipeline" in_flight=42
INFO  indexer.shutdown msg="pipeline drained" remaining=0
INFO  indexer.shutdown msg="checkpoint saved" last_slot=290001000 status=stopped
INFO  indexer.shutdown msg="clean shutdown complete" uptime="4h 32m"
```

### 2.3 State Transition Table

| From         | To           | Trigger                 | Action                 |
| ------------ | ------------ | ----------------------- | ---------------------- |
| Initializing | Backfilling  | gap > 0                 | Begin chunk processing |
| Initializing | Streaming    | gap == 0                | Subscribe WebSocket    |
| Initializing | ShuttingDown | Fatal error             | Log and exit           |
| Backfilling  | Streaming    | All chunks processed    | Subscribe WebSocket    |
| Backfilling  | ShuttingDown | Cancel or fatal         | Drain + checkpoint     |
| Streaming    | CatchingUp   | WS disconnect           | Record disconnect_slot |
| Streaming    | ShuttingDown | Cancel or fatal         | Drain + checkpoint     |
| CatchingUp   | Streaming    | Reconnected + caught up | Resume subscription    |
| CatchingUp   | ShuttingDown | Max retries exceeded    | Log + checkpoint       |
| ShuttingDown | (exit)       | Drain complete          | Process exit           |

---

## 3. Backfill Strategy

### 3.1 Chunking

The `getBlocks` RPC method has a hard limit of 500,000 slots per call. The backfill chunker must subdivide large ranges accordingly.

**Algorithm:**

```
fn compute_chunks(start_slot: u64, end_slot: u64) -> Vec<(u64, u64)> {
    let chunk_size = 500_000;
    let mut chunks = Vec::new();
    let mut current = start_slot;

    while current <= end_slot {
        let chunk_end = min(current + chunk_size - 1, end_slot);
        chunks.push((current, chunk_end));
        current = chunk_end + 1;
    }

    chunks
}
```

**Operational chunk size:** While 500K is the API max, a smaller operational chunk (e.g., 50K slots) provides better progress reporting and more frequent checkpoints. The operational chunk size should be configurable via `SOLARIX_BACKFILL_CHUNK_SIZE` (default: 50,000).

**getBlocks calls per operational chunk:**

- 50K slots fits in a single getBlocks call (well under 500K limit)
- ~45K actual blocks returned (assuming ~90% block production rate)
- Each operational chunk takes ~45K / concurrency getBlock calls

### 3.2 Parallelism

**Concurrent block fetching via Semaphore:**

```
// Pseudocode for parallel block fetching within a chunk
let semaphore = Arc::new(Semaphore::new(concurrency)); // e.g., 5
let rate_limiter = governor::RateLimiter::direct(Quota::per_second(rps));

for slot in block_slots {
    let permit = semaphore.clone().acquire_owned().await?;
    let limiter = rate_limiter.clone();
    let token = cancel_token.clone();
    let tx = pipeline_sender.clone();

    tokio::spawn(async move {
        let _permit = permit; // held until task completes

        // Rate limit
        limiter.until_ready().await;

        // Check cancellation
        if token.is_cancelled() { return; }

        // Fetch with retry
        let block = backoff::future::retry(backoff, || async {
            rpc.get_block(slot).await.map_err(classify_error)
        }).await;

        // Send to pipeline (bounded channel provides backpressure)
        let _ = tx.send(block).await;
    });
}
```

**Concurrency levels by tier:**

| RPC Tier             | RPS Limit | Recommended Concurrency | Slots/sec (approx) |
| -------------------- | --------- | ----------------------- | ------------------ |
| Public (10 RPS)      | 10        | 3-5                     | 3-5                |
| Helius Dev (50 RPS)  | 50        | 10-20                   | 10-20              |
| Helius Biz (200 RPS) | 200       | 30-50                   | 30-50              |
| Local validator      | Unlimited | 20-50                   | 20-50              |

**Maintaining ordering:** Blocks fetched in parallel arrive out of order. Two options:

1. **Option A (recommended): Database handles ordering.** Transactions are inserted with their slot number. Queries order by slot. No ordering needed in the pipeline itself.
2. **Option B: In-pipeline reorder buffer.** Collect blocks into a BTreeMap keyed by slot, flush in order. Adds complexity, only needed if downstream requires strict ordering during processing.

Recommendation: Option A. The database is the ordering authority. The Storer writes slot numbers, and queries use `ORDER BY slot ASC`. During backfill, strict processing order is unnecessary because we are replaying historical data.

### 3.3 Filtering

After fetching a block, filter for target program transactions:

```
fn filter_block_for_program(block: &BlockData, program_id: &Pubkey) -> Vec<TransactionData> {
    block.transactions.iter().filter(|tx| {
        // Check top-level instructions
        let account_keys = &tx.transaction.message.account_keys;
        let has_top_level = tx.transaction.message.instructions.iter().any(|ix| {
            account_keys[ix.program_id_index as usize] == program_id
        });

        // Check inner instructions (CPI)
        let has_inner = tx.meta.inner_instructions.iter().any(|inner_group| {
            inner_group.instructions.iter().any(|ix| {
                account_keys[ix.program_id_index as usize] == program_id
            })
        });

        // Check loaded addresses (v0 transactions)
        let has_loaded = tx.meta.loaded_addresses.as_ref().map_or(false, |loaded| {
            loaded.writable.contains(program_id) || loaded.readonly.contains(program_id)
        });

        has_top_level || has_inner || has_loaded
    }).cloned().collect()
}
```

**Performance note:** Filtering happens in the Reader stage, BEFORE sending to the Decoder channel. This prevents the pipeline from being overwhelmed with irrelevant transactions. A typical Solana block has 1000-4000 transactions; a target program may appear in only 1-10% of them.

### 3.4 Rate Limiting

**Recommended crate: `governor`**

The `governor` crate implements the Generic Cell Rate Algorithm (GCRA), a sophisticated leaky bucket variant. It is the most widely used Rust rate limiter with native async support.

```
// Rate limiter setup
use governor::{Quota, RateLimiter, Jitter};
use std::num::NonZeroU32;
use std::time::Duration;

let rps = env::var("SOLARIX_RPC_RPS").unwrap_or("10".into()).parse::<u32>()?;
let quota = Quota::per_second(NonZeroU32::new(rps).unwrap());
let limiter = RateLimiter::direct(quota);

// Before each RPC call
limiter.until_ready_with_jitter(Jitter::up_to(Duration::from_millis(100))).await;
```

**Adaptive rate limiting:**

When a 429 (rate limit) response is received, the indexer should temporarily reduce its effective rate:

```
// Pseudocode for adaptive rate limiting
struct AdaptiveRateLimiter {
    base_limiter: RateLimiter,
    backoff_factor: AtomicU32, // 1 = normal, 2 = half speed, 4 = quarter speed
}

impl AdaptiveRateLimiter {
    async fn acquire(&self) {
        let factor = self.backoff_factor.load(Ordering::Relaxed);
        for _ in 0..factor {
            self.base_limiter.until_ready().await;
        }
    }

    fn on_rate_limited(&self) {
        // Double the backoff factor (up to 8x)
        self.backoff_factor.fetch_min(
            self.backoff_factor.load(Ordering::Relaxed) * 2,
            8
        );
    }

    fn on_success(&self) {
        // Gradually restore to normal (halve factor, min 1)
        self.backoff_factor.fetch_max(
            self.backoff_factor.load(Ordering::Relaxed) / 2,
            1
        );
    }
}
```

**Integration with `backoff` crate for retries:**

```
use backoff::ExponentialBackoffBuilder;
use backoff::future::retry_notify;

let backoff = ExponentialBackoffBuilder::new()
    .with_initial_interval(Duration::from_millis(500))
    .with_randomization_factor(0.5)
    .with_multiplier(2.0)
    .with_max_interval(Duration::from_secs(30))
    .with_max_elapsed_time(Some(Duration::from_secs(300))) // 5 min total timeout
    .build();

let result = retry_notify(backoff, || async {
    rate_limiter.acquire().await;
    rpc.get_block(slot).await.map_err(|e| classify_rpc_error(e))
}, |err, duration| {
    tracing::warn!(slot, ?err, ?duration, "retrying block fetch");
}).await;
```

### 3.5 Progress Tracking

```
struct BackfillProgress {
    start_slot: u64,
    end_slot: u64,
    current_slot: u64,
    blocks_processed: u64,
    txs_matched: u64,
    started_at: Instant,
}

impl BackfillProgress {
    fn percent_complete(&self) -> f64 {
        if self.end_slot == self.start_slot { return 100.0; }
        let processed = self.current_slot - self.start_slot;
        let total = self.end_slot - self.start_slot;
        (processed as f64 / total as f64) * 100.0
    }

    fn slots_per_sec(&self) -> f64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed == 0.0 { return 0.0; }
        (self.current_slot - self.start_slot) as f64 / elapsed
    }

    fn eta(&self) -> Duration {
        let rate = self.slots_per_sec();
        if rate == 0.0 { return Duration::MAX; }
        let remaining = self.end_slot - self.current_slot;
        Duration::from_secs_f64(remaining as f64 / rate)
    }
}
```

Progress is logged every N seconds (configurable, default 10s) and on each chunk completion.

---

## 4. Cold Start Algorithm

### 4.1 Decision Tree (Pseudocode)

```
async fn cold_start(config: &Config, db: &Pool) -> Result<InitialState> {
    // Step 1: Load checkpoint
    let checkpoint = db.query_opt(
        "SELECT program_id, last_processed_slot, last_processed_signature,
                status, backfill_start_slot, backfill_end_slot
         FROM indexer_state
         WHERE program_id = $1",
        &[&config.program_id]
    ).await?;

    // Step 2: Get current chain tip
    let current_slot = rpc.get_slot().await?;

    match checkpoint {
        // Case A: Fresh start (no prior state)
        None => {
            let start_slot = config.start_slot.unwrap_or(current_slot);
            tracing::info!(
                program_id = %config.program_id,
                start_slot,
                current_slot,
                "fresh start: no prior state found"
            );

            // Insert initial checkpoint
            db.execute(
                "INSERT INTO indexer_state (program_id, last_processed_slot, status, created_at)
                 VALUES ($1, $2, 'backfilling', NOW())",
                &[&config.program_id, &(start_slot as i64)]
            ).await?;

            if start_slot < current_slot {
                return Ok(InitialState::Backfill {
                    start_slot,
                    end_slot: current_slot,
                });
            } else {
                return Ok(InitialState::Stream);
            }
        }

        // Case B: Prior state exists
        Some(state) => {
            let last_slot = state.last_processed_slot as u64;
            let gap = current_slot.saturating_sub(last_slot);

            tracing::info!(
                program_id = %config.program_id,
                last_slot,
                current_slot,
                gap,
                previous_status = %state.status,
                "resuming from checkpoint"
            );

            match gap {
                // Case B1: Fully caught up
                0 => {
                    db.execute(
                        "UPDATE indexer_state SET status = 'streaming', updated_at = NOW()
                         WHERE program_id = $1",
                        &[&config.program_id]
                    ).await?;
                    Ok(InitialState::Stream)
                }

                // Case B2: Gap exists (normal restart scenario)
                1.. => {
                    db.execute(
                        "UPDATE indexer_state
                         SET status = 'backfilling',
                             backfill_start_slot = $2,
                             backfill_end_slot = $3,
                             updated_at = NOW()
                         WHERE program_id = $1",
                        &[&config.program_id,
                          &((last_slot + 1) as i64),
                          &(current_slot as i64)]
                    ).await?;
                    Ok(InitialState::Backfill {
                        start_slot: last_slot + 1,
                        end_slot: current_slot,
                    })
                }
            }
        }
    }
}

// Handle the edge case where last_processed_slot > current_slot
// This could happen if the RPC is behind or pointing to a different cluster.
// Log an error and refuse to start.
if last_slot > current_slot {
    return Err(anyhow!(
        "last_processed_slot ({}) > current_slot ({}). \
         Possible causes: RPC endpoint changed, clock skew, or wrong cluster.",
        last_slot, current_slot
    ));
}
```

### 4.2 Scenario Handling

#### First-ever start (no history)

1. No entry in `indexer_state` for this `program_id`
2. If `SOLARIX_START_SLOT` is set, use that as start_slot
3. If not set, default to current slot (start streaming from now, no historical backfill)
4. Create `indexer_state` entry with status = "backfilling" (or "streaming" if start == current)

**Rationale for defaulting to current slot:** A user who simply runs `docker compose up` without specifying a start slot likely wants to see real-time data immediately, not wait hours for a full historical backfill. The `SOLARIX_START_SLOT` env var provides explicit control for historical needs.

#### Restart after clean shutdown

1. `indexer_state` exists with status = "stopped" and valid `last_processed_slot`
2. Gap = current_slot - last_processed_slot
3. If gap is small (<100 slots, ~40 seconds of data), backfill is quick (<1 minute)
4. If gap is large, backfill takes longer but the algorithm is the same

#### Restart after crash

1. `indexer_state` exists but `last_processed_slot` may be behind the actual last write
2. This is safe because:
   - Checkpoint is updated per-chunk (not per-transaction)
   - Worst case: re-process one chunk of transactions
   - INSERT ON CONFLICT DO NOTHING handles any duplicates
3. Status may be "backfilling" or "streaming" (stale status from before crash)
4. Treat the same as a normal restart: compute gap, backfill if needed

#### Very large gaps (hours or days of downtime)

1. Gap of 9,000 slots (~1 hour) = ~8,100 blocks to fetch
   - At 10 RPS: ~13.5 minutes
   - At 50 RPS: ~2.7 minutes
2. Gap of 216,000 slots (~24 hours) = ~194,000 blocks
   - At 10 RPS: ~5.4 hours
   - At 50 RPS: ~65 minutes
3. Gap of 1,512,000 slots (~7 days) = ~1,360,000 blocks
   - At 10 RPS: ~37.8 hours (use a paid provider)
   - At 50 RPS: ~7.5 hours

For very large gaps (>24 hours), the indexer should log a clear warning with the estimated time and recommend using a higher RPS tier.

---

## 5. Checkpoint Schema

### 5.1 SQL Schema

```sql
-- Tracks indexer state per program
CREATE TABLE indexer_state (
    -- Primary identifier
    program_id          TEXT PRIMARY KEY,

    -- Position tracking
    last_processed_slot BIGINT NOT NULL DEFAULT 0,
    last_processed_sig  TEXT,              -- signature of last processed tx (optional, for finer dedup)

    -- Indexer status
    status              TEXT NOT NULL DEFAULT 'initializing'
                        CHECK (status IN (
                            'initializing',
                            'backfilling',
                            'streaming',
                            'catching_up',
                            'stopped',
                            'error'
                        )),

    -- Backfill progress tracking
    backfill_start_slot BIGINT,           -- start of current/last backfill range
    backfill_end_slot   BIGINT,           -- end of current/last backfill range
    backfill_current    BIGINT,           -- current position within backfill

    -- Error tracking
    error_count         INTEGER NOT NULL DEFAULT 0,
    last_error          TEXT,
    last_error_at       TIMESTAMPTZ,

    -- Statistics
    total_txs_processed BIGINT NOT NULL DEFAULT 0,
    total_blocks_scanned BIGINT NOT NULL DEFAULT 0,

    -- Timestamps
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_heartbeat      TIMESTAMPTZ        -- updated every N seconds during streaming
);

-- Index for monitoring queries
CREATE INDEX idx_indexer_state_status ON indexer_state(status);
```

### 5.2 Field Descriptions

| Field                  | Type        | Purpose                                                                                         |
| ---------------------- | ----------- | ----------------------------------------------------------------------------------------------- |
| `program_id`           | TEXT (PK)   | Base-58 encoded program public key                                                              |
| `last_processed_slot`  | BIGINT      | Slot of last fully processed block. Restart point.                                              |
| `last_processed_sig`   | TEXT        | Signature of the last transaction processed in that slot. For sub-slot dedup on crash recovery. |
| `status`               | TEXT        | Current indexer state. Matches state machine states.                                            |
| `backfill_start_slot`  | BIGINT      | Start of backfill range (for progress calculation)                                              |
| `backfill_end_slot`    | BIGINT      | Target end of backfill range                                                                    |
| `backfill_current`     | BIGINT      | Current position in backfill (for progress %)                                                   |
| `error_count`          | INTEGER     | Rolling count of errors since last clean state                                                  |
| `last_error`           | TEXT        | Most recent error message (truncated to 1000 chars)                                             |
| `last_error_at`        | TIMESTAMPTZ | Timestamp of most recent error                                                                  |
| `total_txs_processed`  | BIGINT      | Lifetime count of transactions processed                                                        |
| `total_blocks_scanned` | BIGINT      | Lifetime count of blocks scanned                                                                |
| `created_at`           | TIMESTAMPTZ | First creation timestamp                                                                        |
| `updated_at`           | TIMESTAMPTZ | Last update timestamp (any field change)                                                        |
| `last_heartbeat`       | TIMESTAMPTZ | Updated every 10s during streaming to detect stale indexers                                     |

### 5.3 Checkpoint Update Strategy

**During backfill:**

- Update `backfill_current` and `last_processed_slot` after each operational chunk (50K slots default)
- This means at most ~50K slots of re-work on crash
- Batch the update into the same DB transaction as the data writes for that chunk

**During streaming:**

- Update `last_processed_slot` every N transactions (default: 100) or every M seconds (default: 10s), whichever comes first
- Update `last_heartbeat` every 10 seconds (for stale indexer detection)

**On shutdown:**

- Final checkpoint update with `status = 'stopped'`
- This is the critical write; if it fails, next start re-processes from last successful checkpoint

### 5.4 Transaction Deduplication Table

```sql
-- Ensures no duplicate transaction processing across backfill/streaming overlap
-- Primary dedup happens via INSERT ON CONFLICT on the transactions table itself.
-- This is NOT a separate dedup table; the transactions table PK (signature) handles it.

CREATE TABLE transactions (
    signature           TEXT PRIMARY KEY,
    slot                BIGINT NOT NULL,
    block_time          BIGINT,
    program_id          TEXT NOT NULL,
    instruction_name    TEXT,
    accounts            JSONB,
    decoded_data        JSONB,
    raw_data            BYTEA,
    is_successful       BOOLEAN NOT NULL DEFAULT true,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_transactions_slot ON transactions(slot);
CREATE INDEX idx_transactions_program ON transactions(program_id);
CREATE INDEX idx_transactions_instruction ON transactions(program_id, instruction_name);
CREATE INDEX idx_transactions_block_time ON transactions(block_time);
-- GIN index for querying inside decoded JSONB
CREATE INDEX idx_transactions_decoded ON transactions USING GIN (decoded_data jsonb_path_ops);
```

The `signature` primary key provides natural deduplication. All inserts use:

```sql
INSERT INTO transactions (signature, slot, block_time, program_id, ...)
VALUES ($1, $2, $3, $4, ...)
ON CONFLICT (signature) DO NOTHING;
```

This handles all overlap scenarios (backfill + streaming running concurrently, crash recovery re-processing) with zero application-level dedup logic for persistence.

---

## 6. Real-time Streaming Design

### 6.1 Connection Management

**Initial connection + subscription:**

```
async fn connect_and_subscribe(
    ws_url: &str,
    program_id: &str,
    cancel: CancellationToken,
) -> Result<WebSocketStream> {
    // Connect with timeout
    let ws = tokio::time::timeout(
        Duration::from_secs(10),
        connect_websocket(ws_url)
    ).await??;

    // Subscribe to logs
    let sub_id = ws.send_request("logsSubscribe", json!([
        {"mentions": [program_id]},
        {"commitment": "confirmed"}
    ])).await?;

    tracing::info!(sub_id, program_id, "subscribed to logsSubscribe");
    Ok(ws)
}
```

**Heartbeat detection:**

Solana RPC WebSockets support standard WebSocket ping/pong frames. The client should:

1. Send a ping every 30 seconds
2. If no pong received within 10 seconds, consider the connection stale
3. Close and reconnect

```
// Heartbeat loop (runs concurrently with message processing)
async fn heartbeat_loop(ws: &WebSocket, cancel: CancellationToken) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if ws.send_ping().await.is_err() {
                    tracing::warn!("ping failed, connection may be stale");
                    break;
                }
                // Check pong within timeout
                match tokio::time::timeout(Duration::from_secs(10), ws.wait_pong()).await {
                    Ok(Ok(_)) => { /* healthy */ }
                    _ => {
                        tracing::warn!("pong timeout, closing stale connection");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
}
```

**Reconnection with exponential backoff:**

```
async fn reconnect_loop(
    ws_url: &str,
    program_id: &str,
    cancel: CancellationToken,
) -> Result<WebSocketStream> {
    let backoff = ExponentialBackoffBuilder::new()
        .with_initial_interval(Duration::from_secs(1))
        .with_multiplier(2.0)
        .with_max_interval(Duration::from_secs(60))
        .with_max_elapsed_time(Some(Duration::from_secs(300))) // give up after 5 min
        .build();

    retry_notify(backoff, || async {
        if cancel.is_cancelled() {
            return Err(backoff::Error::Permanent(anyhow!("shutdown requested")));
        }
        connect_and_subscribe(ws_url, program_id, cancel.clone())
            .await
            .map_err(|e| backoff::Error::transient(e))
    }, |err, duration| {
        tracing::warn!(?err, ?duration, "websocket reconnection attempt failed");
    }).await
}
```

### 6.2 Message Processing

**logsSubscribe notification format:**

```json
{
  "jsonrpc": "2.0",
  "method": "logsNotification",
  "params": {
    "result": {
      "value": {
        "signature": "5h6x...abc",
        "err": null,
        "logs": [
          "Program 11111... invoke [1]",
          "Program log: Instruction: Transfer",
          "Program 11111... success"
        ]
      },
      "context": {
        "slot": 290000000
      }
    },
    "subscription": 0
  }
}
```

**Processing pipeline for each notification:**

1. Extract `signature` and `slot` from notification
2. Check in-memory dedup set (bounded LRU). If seen, skip.
3. If `err` is not null, check if we still want to index failed txs (configurable)
4. Call `getTransaction(signature)` with retry to get full transaction data
5. Send full transaction to Decoder channel
6. Add signature to dedup set
7. Update `last_processed_slot` if slot > current last

**Batching getTransaction calls:**

For high-throughput programs, multiple logsSubscribe notifications may arrive in quick succession. Rather than calling getTransaction one at a time, batch them:

```
// Collect signatures for up to 50ms or 10 signatures, whichever comes first
let mut batch = Vec::new();
let batch_deadline = Instant::now() + Duration::from_millis(50);

loop {
    tokio::select! {
        msg = ws.next() => {
            if let Some(notification) = msg {
                batch.push(notification.signature);
                if batch.len() >= 10 { break; }
            }
        }
        _ = tokio::time::sleep_until(batch_deadline) => break,
        _ = cancel.cancelled() => break,
    }
}

// Fetch all transactions in parallel (within rate limit)
let futures = batch.iter().map(|sig| {
    let limiter = rate_limiter.clone();
    async move {
        limiter.until_ready().await;
        rpc.get_transaction(sig).await
    }
});
let results = futures::future::join_all(futures).await;
```

### 6.3 Deduplication

**In-memory bounded dedup set:**

Use a bounded LRU set to track recently seen signatures. This prevents re-processing duplicates from WebSocket (which has no exactly-once guarantees) without requiring a DB lookup for every notification.

```
// Using a simple HashSet with bounded size
struct DeduplicationSet {
    seen: HashSet<String>,
    order: VecDeque<String>,
    max_size: usize, // default: 10,000
}

impl DeduplicationSet {
    fn insert(&mut self, sig: String) -> bool {
        if self.seen.contains(&sig) {
            return false; // duplicate
        }
        if self.seen.len() >= self.max_size {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(sig.clone());
        self.order.push_back(sig);
        true // new entry
    }
}
```

**Database-level dedup:** Even if the in-memory set misses a duplicate (e.g., after restart), `INSERT ON CONFLICT (signature) DO NOTHING` ensures the database never stores duplicates.

**Two-layer dedup design:**

1. **Hot layer (in-memory):** Fast check, prevents redundant getTransaction calls. Bounded at 10,000 entries (~640KB memory).
2. **Cold layer (database):** INSERT ON CONFLICT DO NOTHING. Catches any duplicates that slip through the hot layer.

---

## 7. Handoff Strategy (Backfill to Streaming)

### 7.1 Analysis of Options

#### Option A: Sequential Handoff

```
Backfill completes at slot S
   |
   v
Start streaming from slot S
```

**Pros:** Simple. No concurrent state management. No dedup needed during transition.

**Cons:** Gap between backfill completion and stream subscription start. During the time it takes to subscribe to WebSocket (100ms-2s), new transactions may be missed. This gap is typically 1-5 slots (400ms-2s), which means 0-20 transactions could be lost.

**Mitigation attempt:** Subscribe to slot S-10 to overlap slightly. But logsSubscribe does not support a "start from slot" parameter -- it always starts from the current tip. So this mitigation is impossible.

**Verdict:** Unacceptable for a "zero gap" guarantee. Any missed transactions during the handoff window are permanently lost.

#### Option B: Overlapping Handoff (Buffer)

```
Start streaming at time T
   |
   +---> Buffer streaming events in memory
   |
   +---> Continue backfill concurrently
   |
   +---> Backfill reaches slot S (caught up)
   |
   +---> Drain buffer (process buffered events)
   |
   +---> Switch to live streaming
```

**Pros:** Zero gap guaranteed. All events captured from time T onward.

**Cons:**

- Memory pressure from buffer during backfill (if backfill takes hours, buffer grows unbounded)
- Complex state management (buffer + backfill + streaming)
- Must handle ordering between buffer drain and live events
- If backfill is slow, buffer can consume gigabytes of memory

**Mitigation:** Cap buffer size and trigger backfill speedup. But this introduces more complexity.

**Verdict:** Correct but overly complex. The buffer management adds significant implementation risk for minimal benefit over Option C.

#### Option C: Signature-based Dedup (Recommended)

```
Time T: Start BOTH backfill AND streaming concurrently
   |
   +---> Streaming writes to DB with INSERT ON CONFLICT DO NOTHING
   |
   +---> Backfill writes to DB with INSERT ON CONFLICT DO NOTHING
   |
   +---> When backfill catches up to stream, it naturally completes
   |
   +---> Only streaming continues
```

**Pros:**

- Simplest implementation. No buffer management. No ordering concerns.
- Zero gap guaranteed: streaming captures everything from subscription start.
- No duplicates: INSERT ON CONFLICT DO NOTHING handles overlap.
- Crash-safe: both paths are independently idempotent.
- Works for any gap size (minutes, hours, days).

**Cons:**

- Write amplification: transactions in the overlap window are fetched and inserted twice (once from backfill, once from streaming). The second insert is a no-op.
- Slightly higher RPC usage during overlap (getTransaction called for transactions that backfill will also process).
- DB sees conflict-resolution overhead for duplicate inserts.

**Quantifying write amplification:**

- Overlap window = time from stream start to backfill completion
- For a 10-minute backfill, overlap = ~10 minutes of transactions
- At ~2.5 blocks/sec with ~10 target transactions/block = ~1,500 duplicate inserts
- INSERT ON CONFLICT DO NOTHING for 1,500 rows: negligible overhead (<100ms total)
- Extra getTransaction calls: ~1,500 \* rate_limited = a few minutes of extra RPC usage

**Verdict: RECOMMENDED.** The write amplification is trivially small. The implementation simplicity, crash safety, and zero-gap guarantee make this the clear winner.

### 7.2 Implementation of Option C

```
async fn run_pipeline(config: Config, db: Pool, rpc: RpcClient) -> Result<()> {
    let cancel = CancellationToken::new();

    // Set up signal handler
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("shutdown signal received");
        cancel_clone.cancel();
    });

    // Determine initial state
    let initial = cold_start(&config, &db).await?;

    match initial {
        InitialState::Stream => {
            // No backfill needed, go straight to streaming
            run_streaming(&config, &db, &rpc, cancel).await
        }

        InitialState::Backfill { start_slot, end_slot } => {
            // Start BOTH concurrently
            let cancel_backfill = cancel.child_token();
            let cancel_stream = cancel.child_token();

            // Spawn streaming task (writes to same DB with ON CONFLICT DO NOTHING)
            let stream_handle = tokio::spawn({
                let config = config.clone();
                let db = db.clone();
                let rpc = rpc.clone();
                async move {
                    run_streaming(&config, &db, &rpc, cancel_stream).await
                }
            });

            // Run backfill in current task
            let backfill_result = run_backfill(
                &config, &db, &rpc, start_slot, end_slot, cancel_backfill
            ).await;

            match backfill_result {
                Ok(()) => {
                    tracing::info!("backfill complete, streaming continues");
                    // Backfill done; streaming task continues running
                    // Wait for streaming to be cancelled (by shutdown signal)
                    stream_handle.await??;
                }
                Err(e) => {
                    tracing::error!(?e, "backfill failed");
                    cancel.cancel(); // shut everything down
                    stream_handle.await.ok();
                    return Err(e);
                }
            }
            Ok(())
        }
    }
}
```

### 7.3 Overlap Window Diagram

```
Time ──────────────────────────────────────────────────────>

Slots:  100        200        300        400        500
         |          |          |          |          |
Backfill:[=========================================]
         start=100                           end=500
         (processes all blocks 100-500 sequentially)

Stream:                 [============================...
                        subscription starts at slot ~300
                        (processes all txs from ~300 onward)

Overlap:                [=============]
                        slots 300-500 processed by BOTH
                        INSERT ON CONFLICT DO NOTHING
                        deduplicates automatically

After backfill:                              [======...
                                             only streaming
                                             continues
```

---

## 8. Graceful Shutdown Sequence

### 8.1 Detailed Sequence

```
Phase 1: Signal Reception (immediate)
──────────────────────────────────────
1. SIGTERM or SIGINT received
2. Signal handler calls cancel_token.cancel()
3. All tasks see is_cancelled() == true via their token clones
4. Log: "shutdown signal received, beginning graceful shutdown"

Phase 2: Reader Stops (immediate, <100ms)
──────────────────────────────────────────
5. Backfill reader stops spawning new getBlock requests
6. Streaming reader stops processing new WebSocket messages
7. WebSocket subscription is unsubscribed
8. No new items enter the pipeline channels
9. Log: "reader stopped, no new items entering pipeline"

Phase 3: Pipeline Drain (up to 15 seconds)
──────────────────────────────────────────
10. In-flight getBlock/getTransaction requests complete (or timeout at 10s)
11. Decoder processes remaining items from Reader->Decoder channel
12. Storer processes remaining items from Decoder->Storer channel
13. Channels drain to empty
14. Log: "pipeline drained, N items processed during drain"

    If 15s timeout expires:
    - Log warning: "drain timeout, M items still in pipeline"
    - Drop remaining channel items (they will be re-processed on restart)
    - Proceed to Phase 4

Phase 4: Database Flush (up to 10 seconds)
──────────────────────────────────────────
15. Storer flushes any buffered/batched writes to PostgreSQL
16. Checkpoint update: last_processed_slot, status = 'stopped'
17. Log: "database flushed, checkpoint saved at slot N"

    If 10s timeout expires:
    - Log error: "database flush timeout, checkpoint may be stale"
    - Proceed to Phase 5 (next restart will re-process from last good checkpoint)

Phase 5: Cleanup (up to 5 seconds)
──────────────────────────────────
18. Close database connection pool (drain active connections)
19. Close WebSocket connection
20. Drop rate limiter, dedup set, and other resources
21. Log: "shutdown complete, exiting"

Phase 6: Exit
──────────────
22. Process exits with code 0 (clean) or 1 (error/timeout)
```

### 8.2 Implementation Pattern

```
async fn shutdown_with_timeout(
    cancel: CancellationToken,
    reader_handle: JoinHandle<()>,
    decoder_handle: JoinHandle<()>,
    storer_handle: JoinHandle<()>,
    db: Pool,
    last_slot: Arc<AtomicU64>,
    program_id: &str,
) -> Result<()> {
    // Phase 2: Reader stops (already signaled via cancel token)
    // Wait for reader to acknowledge and stop
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        reader_handle
    ).await;
    tracing::info!("reader stopped");

    // Phase 3: Pipeline drain
    match tokio::time::timeout(Duration::from_secs(15), async {
        // Decoder and Storer will finish processing channel items
        // and exit when their input channels are closed (reader dropped its sender)
        let _ = decoder_handle.await;
        let _ = storer_handle.await;
    }).await {
        Ok(_) => tracing::info!("pipeline drained successfully"),
        Err(_) => tracing::warn!("pipeline drain timeout, proceeding with shutdown"),
    }

    // Phase 4: Final checkpoint
    let final_slot = last_slot.load(Ordering::SeqCst);
    match tokio::time::timeout(Duration::from_secs(10), async {
        sqlx::query(
            "UPDATE indexer_state
             SET last_processed_slot = $2, status = 'stopped', updated_at = NOW()
             WHERE program_id = $1"
        )
        .bind(program_id)
        .bind(final_slot as i64)
        .execute(&db)
        .await
    }).await {
        Ok(Ok(_)) => tracing::info!(final_slot, "checkpoint saved"),
        Ok(Err(e)) => tracing::error!(?e, "checkpoint save failed"),
        Err(_) => tracing::error!("checkpoint save timeout"),
    }

    // Phase 5: Cleanup
    db.close().await;
    tracing::info!("shutdown complete");

    Ok(())
}
```

### 8.3 Timeout Rationale

| Phase          | Timeout  | Rationale                                                                                  |
| -------------- | -------- | ------------------------------------------------------------------------------------------ |
| Reader stop    | 2s       | Reader only needs to stop loop + unsubscribe WS                                            |
| Pipeline drain | 15s      | In-flight RPC requests may take up to 10s, plus decoder/storer processing                  |
| DB flush       | 10s      | Batch writes + checkpoint update. PG should respond in <1s normally.                       |
| Cleanup        | 5s       | Connection pool close + resource drop                                                      |
| **Total**      | **~32s** | Fits within typical container orchestrator grace period (default 30s in k8s; configurable) |

**Note:** For Kubernetes deployments, set `terminationGracePeriodSeconds: 45` to give the indexer sufficient time.

---

## 9. Error Handling Classification

### 9.1 Error Categories

#### Retryable Errors (use backoff, automatic recovery)

| Error                          | Source           | Backoff Strategy                                              | Max Retries                    |
| ------------------------------ | ---------------- | ------------------------------------------------------------- | ------------------------------ |
| HTTP 429 (rate limited)        | RPC provider     | Use `Retry-After` header if present, else exponential backoff | Unlimited (respect rate limit) |
| HTTP 503 (service unavailable) | RPC provider     | Exponential backoff, 1s initial, 30s max                      | 10                             |
| Connection timeout             | Network          | Exponential backoff, 500ms initial                            | 5                              |
| Connection refused             | Network          | Exponential backoff, 1s initial, 60s max                      | 15                             |
| WebSocket disconnect           | Network / server | Exponential backoff, 1s initial, 60s max                      | 15 (then ShuttingDown)         |
| RPC error -32005 (node behind) | RPC node         | Exponential backoff, 2s initial                               | 5                              |
| Database connection timeout    | PostgreSQL       | Exponential backoff, 1s initial                               | 10                             |
| Database deadlock              | PostgreSQL       | Immediate retry (jitter only)                                 | 3                              |

**Implementation:**

```
fn classify_rpc_error(err: &RpcError) -> backoff::Error<anyhow::Error> {
    match err {
        RpcError::HttpStatus(429) => {
            // Rate limited -- use retry_after if available
            backoff::Error::retry_after(
                anyhow!("rate limited"),
                err.retry_after().unwrap_or(Duration::from_secs(1))
            )
        }
        RpcError::HttpStatus(503) | RpcError::Timeout | RpcError::ConnectionRefused => {
            backoff::Error::transient(anyhow!("{}", err))
        }
        RpcError::JsonRpc { code: -32009, .. } => {
            // Slot was skipped -- permanent for this specific slot
            backoff::Error::permanent(anyhow!("slot skipped: {}", err))
        }
        RpcError::JsonRpc { code: -32005, .. } => {
            // Node is behind -- transient
            backoff::Error::transient(anyhow!("node behind: {}", err))
        }
        RpcError::HttpStatus(400) | RpcError::HttpStatus(401) |
        RpcError::JsonRpc { code: -32600, .. } | // invalid request
        RpcError::JsonRpc { code: -32601, .. } => { // method not found
            backoff::Error::permanent(anyhow!("permanent RPC error: {}", err))
        }
        _ => {
            // Unknown errors are treated as transient (conservative)
            backoff::Error::transient(anyhow!("unknown error: {}", err))
        }
    }
}
```

#### Skip-and-Log Errors (log warning, continue processing)

| Error                                 | Source       | Action                          | Logged Data                                |
| ------------------------------------- | ------------ | ------------------------------- | ------------------------------------------ |
| Malformed transaction data            | RPC response | Skip transaction, log signature | signature, slot, error details             |
| Unknown instruction discriminator     | Decoder      | Store raw data, log warning     | program_id, discriminator bytes, signature |
| Decoder type mismatch                 | Decoder      | Store raw data, log warning     | expected type, actual bytes, signature     |
| Skipped slot (-32009)                 | RPC          | Expected behavior, skip slot    | slot number                                |
| Transaction not found (null response) | RPC          | Skip, may not be finalized yet  | signature, commitment level                |

**Implementation:**

```
fn handle_decode_error(err: &DecodeError, tx: &TransactionData) {
    match err {
        DecodeError::UnknownDiscriminator(disc) => {
            tracing::warn!(
                signature = %tx.signature,
                slot = tx.slot,
                discriminator = ?disc,
                "unknown instruction discriminator, storing raw data"
            );
            // Store raw_data in transactions table, decoded_data = null
        }
        DecodeError::TypeMismatch { expected, actual_len } => {
            tracing::warn!(
                signature = %tx.signature,
                slot = tx.slot,
                expected,
                actual_len,
                "decoder type mismatch, storing raw data"
            );
        }
        DecodeError::MalformedData(msg) => {
            tracing::warn!(
                signature = %tx.signature,
                slot = tx.slot,
                msg,
                "malformed transaction data, skipping"
            );
            // Do not store anything for truly malformed data
        }
    }
}
```

#### Fatal Errors (halt pipeline, enter ShuttingDown state)

| Error                                    | Source      | Action          | Recovery                        |
| ---------------------------------------- | ----------- | --------------- | ------------------------------- |
| Database unreachable (after retries)     | PostgreSQL  | Halt pipeline   | Manual: fix DB, restart indexer |
| Invalid configuration                    | Startup     | Refuse to start | Fix config, restart             |
| IDL not found (no fallback)              | IDL cascade | Halt pipeline   | Provide IDL via --idl-path      |
| RPC endpoint unreachable (after retries) | Network     | Halt pipeline   | Fix network / RPC URL           |
| Disk full                                | OS          | Halt pipeline   | Free disk space, restart        |
| Max reconnection attempts exceeded       | WebSocket   | Halt pipeline   | Check RPC endpoint, restart     |

**Implementation:**

```
fn is_fatal(err: &PipelineError) -> bool {
    matches!(err,
        PipelineError::DatabaseUnreachable(_) |
        PipelineError::InvalidConfiguration(_) |
        PipelineError::IdlNotFound(_) |
        PipelineError::RpcUnreachable(_) |
        PipelineError::MaxReconnectsExceeded
    )
}
```

### 9.2 Error Flow Through Pipeline Stages

```
Reader Stage:
  RPC error --> classify_rpc_error()
    Transient --> retry with backoff
    Permanent (skipped slot) --> skip, continue to next slot
    After max retries --> log error, skip item, increment error_count
    If error_count > threshold (100 consecutive) --> fatal, ShuttingDown

Decoder Stage:
  Decode error --> handle_decode_error()
    Unknown discriminator --> store raw, continue
    Type mismatch --> store raw, continue
    Malformed data --> skip entirely, continue
    Panic in decoder --> catch_unwind, log, skip, continue

Storer Stage:
  DB error --> classify_db_error()
    Connection timeout --> retry with backoff (3 attempts)
    Deadlock --> immediate retry (1 attempt)
    Constraint violation --> skip (dedup), continue
    After max retries --> fatal, trigger ShuttingDown

WebSocket:
  Disconnect --> CatchingUp state
  Max reconnects --> fatal, ShuttingDown
```

### 9.3 Dead Letter Queue (Future Enhancement)

For skip-and-log errors, a future enhancement would add a dead letter queue table:

```sql
CREATE TABLE dead_letter_queue (
    id              BIGSERIAL PRIMARY KEY,
    signature       TEXT,
    slot            BIGINT,
    program_id      TEXT,
    error_type      TEXT,
    error_message   TEXT,
    raw_data        BYTEA,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved        BOOLEAN NOT NULL DEFAULT false,
    resolved_at     TIMESTAMPTZ
);
```

This allows operators to inspect and retry failed items without stopping the pipeline. Not required for the bounty MVP but a solid production feature.

---

## 10. Configuration Parameters

### 10.1 Environment Variables

| Variable                           | Default                               | Description                                                                                 |
| ---------------------------------- | ------------------------------------- | ------------------------------------------------------------------------------------------- |
| **Connection**                     |                                       |                                                                                             |
| `SOLANA_RPC_URL`                   | `https://api.mainnet-beta.solana.com` | HTTP JSON-RPC endpoint                                                                      |
| `SOLANA_WS_URL`                    | (derived from RPC URL)                | WebSocket endpoint. If not set, replace `https://` with `wss://` in RPC URL                 |
| `DATABASE_URL`                     | (required)                            | PostgreSQL connection string                                                                |
| **Program**                        |                                       |                                                                                             |
| `SOLARIX_PROGRAM_ID`               | (required)                            | Base-58 encoded program public key to index                                                 |
| `SOLARIX_IDL_PATH`                 | (none)                                | Path to IDL JSON file (manual fallback)                                                     |
| **Backfill**                       |                                       |                                                                                             |
| `SOLARIX_START_SLOT`               | (current slot)                        | Slot to start backfilling from. If unset, starts from current slot (no historical backfill) |
| `SOLARIX_BACKFILL_CHUNK_SIZE`      | `50000`                               | Number of slots per operational chunk                                                       |
| **Rate Limiting**                  |                                       |                                                                                             |
| `SOLARIX_RPC_RPS`                  | `10`                                  | Requests per second to RPC endpoint                                                         |
| `SOLARIX_RPC_CONCURRENCY`          | `5`                                   | Max concurrent in-flight RPC requests                                                       |
| **Pipeline**                       |                                       |                                                                                             |
| `SOLARIX_CHANNEL_CAPACITY`         | `256`                                 | Bounded channel capacity between pipeline stages                                            |
| `SOLARIX_DEDUP_CACHE_SIZE`         | `10000`                               | Max entries in the in-memory signature dedup set                                            |
| `SOLARIX_INDEX_FAILED_TXS`         | `false`                               | Whether to index failed (err != null) transactions                                          |
| **Checkpoint**                     |                                       |                                                                                             |
| `SOLARIX_CHECKPOINT_INTERVAL_SECS` | `10`                                  | Seconds between checkpoint updates during streaming                                         |
| `SOLARIX_CHECKPOINT_INTERVAL_TXS`  | `100`                                 | Transaction count between checkpoint updates                                                |
| **Shutdown**                       |                                       |                                                                                             |
| `SOLARIX_SHUTDOWN_DRAIN_SECS`      | `15`                                  | Max seconds to drain pipeline on shutdown                                                   |
| `SOLARIX_SHUTDOWN_DB_FLUSH_SECS`   | `10`                                  | Max seconds for final DB flush on shutdown                                                  |
| **Retry**                          |                                       |                                                                                             |
| `SOLARIX_RETRY_INITIAL_MS`         | `500`                                 | Initial retry interval in milliseconds                                                      |
| `SOLARIX_RETRY_MAX_INTERVAL_SECS`  | `30`                                  | Max retry interval in seconds                                                               |
| `SOLARIX_RETRY_MAX_ELAPSED_SECS`   | `300`                                 | Total retry timeout (5 minutes)                                                             |
| `SOLARIX_WS_MAX_RECONNECTS`        | `15`                                  | Max WebSocket reconnection attempts before fatal                                            |
| **Logging**                        |                                       |                                                                                             |
| `SOLARIX_LOG_LEVEL`                | `info`                                | Log level (trace, debug, info, warn, error)                                                 |
| `SOLARIX_LOG_FORMAT`               | `json`                                | Log format (json or pretty)                                                                 |
| **Heartbeat**                      |                                       |                                                                                             |
| `SOLARIX_WS_PING_INTERVAL_SECS`    | `30`                                  | WebSocket ping interval                                                                     |
| `SOLARIX_WS_PONG_TIMEOUT_SECS`     | `10`                                  | WebSocket pong timeout                                                                      |

### 10.2 Configuration Loading

```
// Priority: CLI args > env vars > .env file > defaults
fn load_config() -> Result<Config> {
    dotenvy::dotenv().ok(); // load .env if present

    let config = Config {
        rpc_url: env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into()),
        ws_url: env::var("SOLANA_WS_URL")
            .unwrap_or_else(|_| derive_ws_url(&config.rpc_url)),
        database_url: env::var("DATABASE_URL")
            .context("DATABASE_URL is required")?,
        program_id: env::var("SOLARIX_PROGRAM_ID")
            .context("SOLARIX_PROGRAM_ID is required")?,
        // ... etc
    };

    config.validate()?; // check program_id is valid base58, URLs are valid, etc.
    Ok(config)
}

fn derive_ws_url(rpc_url: &str) -> String {
    rpc_url
        .replace("https://", "wss://")
        .replace("http://", "ws://")
}
```

---

## 11. Sources

### Tokio & Async Patterns

- [Tokio Bounded MPSC Channels](https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.channel.html)
- [Tokio Channels Tutorial](https://tokio.rs/tokio/tutorial/channels)
- [Handling Backpressure in Rust Async Systems](https://www.slingacademy.com/article/handling-backpressure-in-rust-async-systems-with-bounded-channels/)
- [Tokio Graceful Shutdown](https://tokio.rs/tokio/topics/shutdown)
- [CancellationToken in tokio_util](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)
- [Rust Tokio Task Cancellation Patterns](https://cybernetist.com/2024/04/19/rust-tokio-task-cancellation-patterns/)
- [Building Graceful Shutdown Handler in Rust](https://oneuptime.com/blog/post/2026-01-07-rust-graceful-shutdown/view)
- [tokio-graceful-shutdown crate](https://crates.io/crates/tokio-graceful-shutdown)
- [Tokio Select Macro](https://docs.rs/tokio/latest/tokio/macro.select.html)
- [Tokio Semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html)
- [Rust Async Cancellation Safety](https://developerlife.com/2024/07/10/rust-async-cancellation-safety-tokio/)

### Rate Limiting

- [Governor Crate](https://github.com/boinkor-net/governor)
- [Leaky Bucket Crate](https://docs.rs/leaky-bucket)
- [Rate Limiting in Rust Without External Services](https://oneuptime.com/blog/post/2026-01-07-rust-rate-limiting/view)
- [API Rate Limiting in Rust (Shuttle)](https://www.shuttle.dev/blog/2024/02/22/api-rate-limiting-rust)

### Retry & Backoff

- [backoff Crate](https://github.com/ihrwein/backoff)
- [Exponential Backoff with Jitter in Rust](https://oneuptime.com/blog/post/2026-01-25-exponential-backoff-jitter-rust/view)
- [backon Crate Design](https://rustmagazine.org/issue-2/how-i-designed-the-api-for-backon-a-user-friendly-retry-crate/)
- [Retry Patterns: Backoff, Jitter, DLQ](https://dev.to/young_gao/retry-patterns-that-actually-work-exponential-backoff-jitter-and-dead-letter-queues-75)

### Solana Indexing

- [Solana logsSubscribe Docs](https://solana.com/docs/rpc/websocket/logssubscribe)
- [Helius: How to Index Solana Data](https://www.helius.dev/docs/rpc/how-to-index-solana-data)
- [Helius WebSocket Docs](https://www.helius.dev/docs/rpc/websocket)
- [Helius Enhanced WebSockets](https://www.helius.dev/blog/introducing-next-generation-enhanced-websockets)
- [Carbon Indexer Framework](https://github.com/sevenlabs-hq/carbon)
- [Carbon V1 Pipeline Architecture](https://solanacompass.com/learn/breakpoint-25/tech-talk-sevenlabs-carbon-data-pipeline)
- [Solana Indexer SDK](https://docs.rs/solana-indexer-sdk/latest/solana_indexer_sdk/)
- [Token Transfer Indexer Design](https://www.niks3089.com/posts/token-history-indexer-design/)
- [Substreams Solana Indexing](https://thegraph.com/blog/solana-indexing-pains/)
- [Solana Indexers in 2026](https://htwtech.medium.com/solana-indexers-in-2026-how-to-choose-the-right-data-infrastructure-213803e5c587)
- [WebSocket Reconnection Guide](https://websocket.org/guides/reconnection/)
- [Chainary: Solana WebSocket Architecture](https://www.chainary.net/articles/solana-websocket-subscriptions-real-time-data-streaming-architecture)

### PostgreSQL

- [PostgreSQL INSERT ON CONFLICT](https://www.postgresql.org/docs/current/sql-insert.html)
- [PostgreSQL Upsert Performance](https://archive.jlongster.com/how-one-word-postgresql-performance)
- [PostgreSQL Upsert Guide (DbVisualizer)](https://www.dbvis.com/thetable/postgresql-upsert-insert-on-conflict-guide/)
- [Postgres Upserts 300x Faster](https://www.tigerdata.com/blog/how-we-made-postgresql-upserts-300x-faster-on-compressed-data)

---

**Research Completion Date:** 2026-04-05
**Document Status:** Complete. Ready for implementation phase.
**Confidence Level:** HIGH -- all designs grounded in verified RPC constraints, existing Rust crate APIs, and production Solana indexer patterns.
