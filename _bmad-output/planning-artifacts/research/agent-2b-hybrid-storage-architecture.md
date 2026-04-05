# Agent 2B: Hybrid Storage Architecture Research

**Date:** 2026-04-05
**Context:** Solarix -- Universal Solana Indexer (Superteam Ukraine Bounty)
**Scope:** PostgreSQL hybrid storage layer design (typed columns + JSONB + GIN indexes)

---

## 1. Executive Summary

This document specifies the complete storage architecture for Solarix, a universal Solana indexer that stores decoded blockchain data in PostgreSQL using a hybrid approach: typed common columns for frequently-queried metadata + JSONB for program-specific decoded payloads + strategic indexes (B-tree on typed columns, GIN on JSONB, expression indexes on hot paths).

**Key Design Decisions:**

| Decision             | Choice                                                        | Rationale                                                                                   |
| -------------------- | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| Schema approach      | Hybrid (typed + JSONB)                                        | Best of both: fast metadata queries + schema flexibility for arbitrary IDLs                 |
| JSONB operator class | `jsonb_path_ops` (default) + expression indexes on hot fields | 20-30% index size vs 60-80% with `jsonb_ops`; containment covers API filter patterns        |
| Batch insert method  | `INSERT...UNNEST` with `ON CONFLICT`                          | Best balance of performance + upsert support in sqlx; competitive with COPY at <=10K rows   |
| Transaction boundary | Per-block                                                     | Atomic: either the entire block is persisted or none of it is; enables clean crash recovery |
| Materialized views   | On-demand per account type                                    | Generated at IDL registration time; refreshed periodically or on API demand                 |
| TimescaleDB          | Deferred (not for bounty)                                     | Standard PostgreSQL with `date_trunc` aggregation is sufficient; avoids dependency          |

**Performance Expectations:**

- Metadata queries (by slot, signature, program_id): < 5ms with B-tree indexes
- JSONB containment queries (filter by decoded field): 15-60ms with GIN index
- Expression-indexed JSONB field queries: 5-15ms (approaches native column speed)
- Write throughput: 5,000-15,000 rows/second sustained with batched UNNEST (adequate for Solana's ~2.5 TPS per program)
- Storage overhead: JSONB adds ~2x vs fully normalized, acceptable for universal indexer flexibility

---

## 2. Hybrid Column Strategy

### 2.1 Design Principles

The hybrid approach stores data in two tiers:

**Tier 1 -- Typed Common Columns (always present, indexed):**

- Columns that exist for EVERY record regardless of the Anchor program
- Used for routing queries (which program? which instruction? which slot range?)
- B-tree indexed for fast equality and range lookups
- Data types chosen to match Solana's native representations

**Tier 2 -- JSONB Payload (program-specific, GIN-indexed):**

- The decoded instruction args or account state fields as a JSONB document
- Schema varies per program IDL -- this is what makes the indexer "universal"
- GIN-indexed for containment queries (API multi-parameter filtering)
- Expression-indexed on frequently-queried fields when access patterns emerge

### 2.2 Why Not Fully Normalized (Table-per-Account-Type)?

For a universal indexer that loads arbitrary IDLs at runtime:

- **Unknown table count at startup:** A program with 15 account types + 20 instructions = 35 dynamic tables. Multiple programs = hundreds of tables.
- **Schema migrations at runtime:** Adding a new IDL requires DDL (CREATE TABLE). Dynamic DDL is fragile, hard to test, and breaks connection pool assumptions.
- **JOIN complexity:** Querying across account types requires dynamic JOIN construction. JSONB with GIN avoids this entirely.
- **Storage savings are marginal:** The JSONB overhead (key repetition) is ~2x, but avoids nullable columns for sparse schemas.

The hybrid approach keeps the routing/metadata layer normalized and fast while accommodating arbitrary decoded payloads in JSONB.

### 2.3 Column Promotion Strategy

A JSONB field should be promoted to a typed column when:

1. **Query frequency threshold:** The field appears in >50% of API queries for a given program
2. **Aggregation target:** The field is used in GROUP BY, ORDER BY, or aggregate functions
3. **Storage threshold:** The field is present in >1/80th of rows (Heap.io heuristic -- at this point, a typed column saves space vs. repeated JSONB keys)
4. **Range query requirement:** The field needs `<`, `>`, `BETWEEN` operations (GIN cannot accelerate these)

For the bounty, no promotion is needed. The hybrid schema handles all required query patterns. Promotion is a post-launch optimization.

---

## 3. Table Schemas

### 3.1 Instructions Table

```sql
CREATE TABLE IF NOT EXISTS instructions (
    -- Tier 1: Typed common columns
    id              BIGSERIAL       PRIMARY KEY,
    signature       VARCHAR(88)     NOT NULL,          -- base58 tx signature (44 bytes encoded)
    slot            BIGINT          NOT NULL,          -- Solana slot number
    block_time      TIMESTAMPTZ,                       -- block timestamp (nullable: not all blocks have times)
    program_id      VARCHAR(44)     NOT NULL,          -- base58 program address
    instruction_name VARCHAR(128)   NOT NULL,          -- decoded instruction name from IDL
    is_inner_ix     BOOLEAN         NOT NULL DEFAULT FALSE, -- inner (CPI) instruction flag
    ix_index        SMALLINT        NOT NULL DEFAULT 0,     -- instruction index within transaction

    -- Tier 2: JSONB payload
    args            JSONB,                             -- decoded instruction arguments
    accounts        JSONB,                             -- named account list [{name, pubkey, is_signer, is_writable}]

    -- Metadata
    raw_data        BYTEA,                             -- original base64-decoded instruction data (for re-decode)
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),

    -- Constraints
    CONSTRAINT uq_instructions_sig_ix UNIQUE (signature, ix_index)
);
```

**Column Type Rationale:**

| Column             | Type           | Why                                                                                                      |
| ------------------ | -------------- | -------------------------------------------------------------------------------------------------------- |
| `id`               | `BIGSERIAL`    | Auto-incrementing surrogate key; 8 bytes vs 32 bytes for hash-based PK. Enables cursor pagination.       |
| `signature`        | `VARCHAR(88)`  | Base58-encoded Solana signature. Max 88 chars. Not BYTEA because API consumers expect base58.            |
| `slot`             | `BIGINT`       | Solana slots are u64. PostgreSQL has no unsigned types; BIGINT (i64) covers slots up to 9.2 quintillion. |
| `block_time`       | `TIMESTAMPTZ`  | Nullable because some blocks lack timestamps. TIMESTAMPTZ for timezone-safe aggregation.                 |
| `program_id`       | `VARCHAR(44)`  | Base58-encoded Pubkey. Fixed 44 chars.                                                                   |
| `instruction_name` | `VARCHAR(128)` | From IDL. Most are <32 chars. 128 handles edge cases.                                                    |
| `args`             | `JSONB`        | Decoded instruction arguments. Schema varies per program.                                                |
| `accounts`         | `JSONB`        | Named account list with metadata. Array of objects.                                                      |
| `raw_data`         | `BYTEA`        | Optional. Enables re-decoding if IDL changes. Can be omitted to save space.                              |

### 3.2 Account States Table

```sql
CREATE TABLE IF NOT EXISTS account_states (
    -- Tier 1: Typed common columns
    id              BIGSERIAL       PRIMARY KEY,
    pubkey          VARCHAR(44)     NOT NULL,          -- account public key (base58)
    account_type    VARCHAR(128)    NOT NULL,          -- decoded account type name from IDL
    program_id      VARCHAR(44)     NOT NULL,          -- owner program (base58)
    slot_updated    BIGINT          NOT NULL,          -- slot when this state was captured
    lamports        BIGINT          NOT NULL DEFAULT 0, -- account balance in lamports

    -- Tier 2: JSONB payload
    data            JSONB           NOT NULL,          -- decoded account state fields

    -- Metadata
    data_len        INTEGER         NOT NULL DEFAULT 0, -- raw data length in bytes
    is_executable   BOOLEAN         NOT NULL DEFAULT FALSE,
    rent_epoch      BIGINT          NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),

    -- Constraints: latest state per account
    CONSTRAINT uq_account_states_pubkey UNIQUE (pubkey)
);
```

**Design Choice -- Latest State Only:**

The `account_states` table stores the **current** state of each account (upsert on pubkey). This matches the bounty requirement ("current state of account X"). Historical state tracking would require a separate `account_states_history` table or TimescaleDB hypertable -- deferred for post-bounty.

### 3.3 Indexer State Table

```sql
CREATE TABLE IF NOT EXISTS indexer_state (
    id              SERIAL          PRIMARY KEY,
    program_id      VARCHAR(44)     NOT NULL UNIQUE,
    last_slot       BIGINT          NOT NULL DEFAULT 0,
    last_signature  VARCHAR(88),
    status          VARCHAR(20)     NOT NULL DEFAULT 'idle',  -- idle, backfilling, realtime, error
    idl_hash        VARCHAR(64),                               -- SHA256 of loaded IDL for change detection
    created_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ     NOT NULL DEFAULT NOW()
);
```

### 3.4 Program Statistics Table (Materialized or Cached)

```sql
CREATE TABLE IF NOT EXISTS program_stats (
    program_id      VARCHAR(44)     PRIMARY KEY,
    total_instructions  BIGINT      NOT NULL DEFAULT 0,
    total_accounts      BIGINT      NOT NULL DEFAULT 0,
    unique_signers      BIGINT      NOT NULL DEFAULT 0,
    first_seen_slot     BIGINT,
    last_seen_slot      BIGINT,
    instruction_counts  JSONB,      -- {"transfer": 1234, "initialize": 56, ...}
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

---

## 4. Index Design

### 4.1 B-Tree Indexes on Typed Columns

```sql
-- Instructions table
CREATE INDEX idx_instructions_slot ON instructions (slot);
CREATE INDEX idx_instructions_block_time ON instructions (block_time);
CREATE INDEX idx_instructions_program_id ON instructions (program_id);
CREATE INDEX idx_instructions_name ON instructions (program_id, instruction_name);
CREATE INDEX idx_instructions_signature ON instructions (signature);

-- Account states table
CREATE INDEX idx_account_states_program_id ON account_states (program_id);
CREATE INDEX idx_account_states_type ON account_states (program_id, account_type);
CREATE INDEX idx_account_states_slot ON account_states (slot_updated);
```

**Composite Index Rationale:**

- `(program_id, instruction_name)` -- most API queries filter by program first, then instruction. Composite index satisfies both in one lookup.
- `(program_id, account_type)` -- same pattern for account queries: "all TokenAccount states for program X".
- `slot` standalone -- needed for time-range queries and block-ordered scanning.

### 4.2 GIN Indexes on JSONB

```sql
-- GIN index on instruction args for multi-parameter filtering
CREATE INDEX idx_instructions_args_gin ON instructions
    USING GIN (args jsonb_path_ops);

-- GIN index on account state data
CREATE INDEX idx_account_states_data_gin ON account_states
    USING GIN (data jsonb_path_ops);
```

**Why `jsonb_path_ops` over `jsonb_ops`:**

| Factor                | `jsonb_ops`     | `jsonb_path_ops` | Winner           |
| --------------------- | --------------- | ---------------- | ---------------- |
| Index size            | 60-80% of table | 20-30% of table  | `jsonb_path_ops` |
| DML overhead          | +79%            | +16%             | `jsonb_path_ops` |
| Containment (`@>`)    | Supported       | Supported        | Tie              |
| Key existence (`?`)   | Supported       | Not supported    | `jsonb_ops`      |
| JSONPath (`@?`, `@@`) | Supported       | Supported        | Tie              |

**Solarix query patterns are containment-based:** "find instructions where args contain `{amount: 1000}`" maps to `args @> '{"amount": 1000}'`. Key existence checks (`?`) are not needed for the API.

The 3-4x smaller index size and 5x lower write overhead of `jsonb_path_ops` make it the clear winner for a write-heavy indexer.

### 4.3 Expression Indexes (for Hot Paths)

When specific JSONB fields are queried frequently (e.g., `amount` in token transfer programs), create B-tree expression indexes:

```sql
-- Example: expression index on a frequently-queried numeric field
CREATE INDEX idx_instructions_args_amount
    ON instructions (((args->>'amount')::BIGINT))
    WHERE instruction_name = 'transfer';

-- Example: expression index on a pubkey field
CREATE INDEX idx_instructions_args_recipient
    ON instructions ((args->>'recipient'))
    WHERE instruction_name = 'transfer';
```

**When to Use Expression Indexes vs. GIN:**

| Query Pattern                      | Best Index             | Why                                |
| ---------------------------------- | ---------------------- | ---------------------------------- |
| `args @> '{"field": "value"}'`     | GIN (`jsonb_path_ops`) | Containment is GIN's strength      |
| `(args->>'amount')::BIGINT > 1000` | Expression (B-tree)    | Range queries need B-tree          |
| `ORDER BY (args->>'timestamp')`    | Expression (B-tree)    | Sorting needs B-tree               |
| "does field X exist in args?"      | GIN (`jsonb_ops`)      | Key existence only in `jsonb_ops`  |
| Multi-field containment filter     | GIN (`jsonb_path_ops`) | Single index handles any key combo |

**For the bounty:** Start with GIN `jsonb_path_ops` only. Expression indexes are a post-launch optimization that can be added dynamically per-program without schema changes.

### 4.4 Partial Indexes

Partial indexes reduce index size by only indexing a subset of rows:

```sql
-- Only index non-CPI instructions (most API queries filter inner instructions out)
CREATE INDEX idx_instructions_args_gin_outer ON instructions
    USING GIN (args jsonb_path_ops)
    WHERE is_inner_ix = FALSE;
```

This can reduce index size by 60-80% for programs with heavy CPI usage (e.g., Jupiter routing through multiple DEXes).

---

## 5. Materialized View Strategy

### 5.1 Purpose

Materialized views provide typed projections over JSONB data, making account state queries feel like native relational tables. They serve the "Advanced API" requirement by enabling:

- SQL-standard column names for JSONB fields
- Typed columns for range queries and aggregation
- Pre-computed statistics

### 5.2 Auto-Generated Account Type Views

When an IDL is registered, Solarix generates a materialized view for each account type:

```sql
-- Auto-generated for a TokenAccount type from an IDL
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_token_account AS
SELECT
    pubkey,
    slot_updated,
    lamports,
    data->>'mint'           AS mint,
    data->>'owner'          AS owner,
    (data->>'amount')::BIGINT AS amount,
    (data->>'delegate')     AS delegate,
    (data->>'state')::INT   AS state,
    (data->>'is_native')::BOOLEAN AS is_native,
    (data->>'delegated_amount')::BIGINT AS delegated_amount,
    (data->>'close_authority') AS close_authority,
    updated_at
FROM account_states
WHERE program_id = '<program_id>' AND account_type = 'TokenAccount';

-- Required for REFRESH CONCURRENTLY
CREATE UNIQUE INDEX ON mv_token_account (pubkey);

-- Expression indexes on typed projections
CREATE INDEX ON mv_token_account (mint);
CREATE INDEX ON mv_token_account (owner);
CREATE INDEX ON mv_token_account (amount);
```

### 5.3 Time-Series Aggregation View

```sql
-- Instruction counts per hour (for aggregation API)
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_instruction_hourly AS
SELECT
    program_id,
    instruction_name,
    date_trunc('hour', block_time) AS hour,
    COUNT(*) AS instruction_count,
    COUNT(DISTINCT signature) AS tx_count
FROM instructions
WHERE block_time IS NOT NULL
GROUP BY program_id, instruction_name, date_trunc('hour', block_time);

CREATE UNIQUE INDEX ON mv_instruction_hourly (program_id, instruction_name, hour);
CREATE INDEX ON mv_instruction_hourly (hour);
```

### 5.4 Refresh Strategy

| View Type          | Refresh Method           | Frequency                         | Rationale                                                                              |
| ------------------ | ------------------------ | --------------------------------- | -------------------------------------------------------------------------------------- |
| Account type views | `REFRESH CONCURRENTLY`   | Every 60 seconds or on API demand | Account state changes are moderate frequency; concurrent refresh avoids blocking reads |
| Hourly aggregation | `REFRESH CONCURRENTLY`   | Every 5 minutes                   | Aggregation data is append-only; small diff each refresh                               |
| Program stats      | Application-level update | On each batch insert              | Counters can be updated atomically with `UPDATE ... SET count = count + N`             |

**REFRESH CONCURRENTLY Requirements:**

- Requires at least one UNIQUE index on the materialized view
- Cannot be run in parallel (only one refresh at a time per view)
- Performance: For small diffs (few changed rows), CONCURRENTLY is faster because it only updates differences. For large refreshes (>50% of data changed), non-concurrent is faster but blocks reads.
- Post-refresh: Run `VACUUM` to clean up dead tuples from the diff operation.

**Alternative: Skip Materialized Views for Bounty**

For the bounty submission, materialized views may be overkill. The direct JSONB queries with GIN indexes are sufficient for:

- Multi-parameter filtering: `SELECT * FROM instructions WHERE args @> '{"amount": 1000}' AND program_id = 'X'`
- Aggregation: `SELECT instruction_name, COUNT(*) FROM instructions WHERE block_time > NOW() - INTERVAL '24 hours' GROUP BY instruction_name`

Materialized views become valuable at scale (>10M rows) when the aggregation queries become too slow on raw data. The architecture should support adding them later without schema changes.

---

## 6. Query Pattern --> SQL --> Index Mapping

### 6.1 Multi-Parameter Filtering

**API Request:** "All transfer instructions where amount > 1000 AND recipient = AbC123..."

```sql
-- Option A: GIN containment (works immediately, moderate performance)
SELECT id, signature, slot, block_time, instruction_name, args
FROM instructions
WHERE program_id = $1
  AND instruction_name = 'transfer'
  AND args @> $2::jsonb    -- e.g., '{"recipient": "AbC123..."}'
  AND (args->>'amount')::BIGINT > $3
ORDER BY slot DESC
LIMIT $4 OFFSET $5;
```

**Indexes Used:**

- `idx_instructions_name` (B-tree) for `program_id + instruction_name` filtering
- `idx_instructions_args_gin` (GIN) for `args @> ...` containment
- Note: The `> $3` range filter on amount cannot use GIN. If this is a hot path, add an expression index.

**Expected Performance:**

- Without expression index: 50-200ms (GIN narrows to matching documents, then sequential filter on amount)
- With expression index on `(args->>'amount')::BIGINT`: 5-20ms

### 6.2 Time-Range Aggregation

**API Request:** "Count of transfer instructions per hour in the last 24 hours"

```sql
-- Direct query (sufficient for <10M rows)
SELECT
    date_trunc('hour', block_time) AS hour,
    COUNT(*) AS count
FROM instructions
WHERE program_id = $1
  AND instruction_name = $2
  AND block_time >= NOW() - INTERVAL '24 hours'
GROUP BY date_trunc('hour', block_time)
ORDER BY hour;
```

**Indexes Used:**

- `idx_instructions_name` (B-tree) for `program_id + instruction_name`
- `idx_instructions_block_time` (B-tree) for time range filter

**Expected Performance:**

- <1M instructions in 24h window: 20-100ms
- > 10M instructions: use materialized view `mv_instruction_hourly` for < 5ms

### 6.3 Program Statistics

**API Request:** "Basic stats for program X"

```sql
-- Fast path: read from pre-computed stats table
SELECT * FROM program_stats WHERE program_id = $1;

-- Slow path (fallback): compute on demand
SELECT
    COUNT(*) AS total_instructions,
    COUNT(DISTINCT signature) AS total_transactions,
    MIN(slot) AS first_seen_slot,
    MAX(slot) AS last_seen_slot,
    jsonb_object_agg(instruction_name, cnt) AS instruction_counts
FROM (
    SELECT instruction_name, COUNT(*) AS cnt
    FROM instructions
    WHERE program_id = $1
    GROUP BY instruction_name
) sub;
```

**Indexes Used:**

- `idx_instructions_program_id` (B-tree)
- Primary key scan on `program_stats`

**Expected Performance:**

- Pre-computed: < 1ms
- On-demand: 100-500ms for programs with <1M instructions

### 6.4 Account State Queries

**API Request:** "Current state of account X"

```sql
SELECT pubkey, account_type, program_id, slot_updated, lamports, data
FROM account_states
WHERE pubkey = $1;
```

**Index Used:** `uq_account_states_pubkey` (UNIQUE constraint = implicit B-tree)
**Expected Performance:** < 1ms (single row by unique key)

**API Request:** "All accounts of type TokenAccount for program X"

```sql
SELECT pubkey, account_type, slot_updated, lamports, data
FROM account_states
WHERE program_id = $1
  AND account_type = $2
ORDER BY slot_updated DESC
LIMIT $3 OFFSET $4;
```

**Index Used:** `idx_account_states_type` (composite B-tree on `program_id, account_type`)
**Expected Performance:** 5-20ms with pagination

### 6.5 Account State Filtering by JSONB Fields

**API Request:** "All TokenAccount states where owner = AbC123... and amount > 0"

```sql
SELECT pubkey, slot_updated, data
FROM account_states
WHERE program_id = $1
  AND account_type = 'TokenAccount'
  AND data @> '{"owner": "AbC123..."}'::jsonb
  AND (data->>'amount')::BIGINT > 0
ORDER BY slot_updated DESC
LIMIT $2;
```

**Indexes Used:**

- `idx_account_states_type` (B-tree) for `program_id + account_type`
- `idx_account_states_data_gin` (GIN) for `data @> ...`

**Expected Performance:** 15-60ms

---

## 7. Write Path Design

### 7.1 Data Flow

```
Solana RPC --> Fetch Block/Tx --> Decode (chainparser) --> JSON
  --> Batch Accumulator --> INSERT...UNNEST (per block) --> PostgreSQL
```

### 7.2 Batch Insert Strategy: UNNEST

**Why UNNEST over COPY:**

| Factor                   | `INSERT...UNNEST`                       | `COPY` (binary)                           |
| ------------------------ | --------------------------------------- | ----------------------------------------- |
| sqlx support             | Native (`sqlx::query` with array binds) | Requires `tokio-postgres` or raw protocol |
| Upsert support           | Full (`ON CONFLICT DO UPDATE`)          | None until PG17 (`ON_ERROR IGNORE` only)  |
| Performance (<=10K rows) | Competitive with COPY                   | Faster at >10K rows                       |
| JSONB support            | Native                                  | Requires custom binary encoding           |
| Parameter limit          | 1 per column (no limit issues)          | N/A                                       |
| Transactional            | Yes (part of normal SQL)                | Requires special handling                 |

For Solarix, batches are per-block. Solana blocks contain ~50-2000 transactions. After filtering for a single program, a typical batch is 0-100 instructions. UNNEST is optimal for this range.

**SQL Pattern:**

```sql
INSERT INTO instructions
    (signature, slot, block_time, program_id, instruction_name,
     is_inner_ix, ix_index, args, accounts)
SELECT * FROM UNNEST(
    $1::VARCHAR[], $2::BIGINT[], $3::TIMESTAMPTZ[], $4::VARCHAR[], $5::VARCHAR[],
    $6::BOOLEAN[], $7::SMALLINT[], $8::JSONB[], $9::JSONB[]
)
ON CONFLICT (signature, ix_index) DO NOTHING;
```

**Rust Pattern (sqlx):**

```rust
// Decompose Vec<Instruction> into column vectors
let signatures: Vec<String> = batch.iter().map(|ix| ix.signature.clone()).collect();
let slots: Vec<i64> = batch.iter().map(|ix| ix.slot as i64).collect();
let block_times: Vec<Option<DateTime<Utc>>> = batch.iter().map(|ix| ix.block_time).collect();
let program_ids: Vec<String> = batch.iter().map(|ix| ix.program_id.clone()).collect();
let names: Vec<String> = batch.iter().map(|ix| ix.instruction_name.clone()).collect();
let is_inner: Vec<bool> = batch.iter().map(|ix| ix.is_inner_ix).collect();
let ix_indexes: Vec<i16> = batch.iter().map(|ix| ix.ix_index).collect();
let args: Vec<serde_json::Value> = batch.iter().map(|ix| ix.args.clone()).collect();
let accounts: Vec<serde_json::Value> = batch.iter().map(|ix| ix.accounts.clone()).collect();

sqlx::query(r#"
    INSERT INTO instructions
        (signature, slot, block_time, program_id, instruction_name,
         is_inner_ix, ix_index, args, accounts)
    SELECT * FROM UNNEST($1, $2, $3, $4, $5, $6, $7, $8, $9)
    ON CONFLICT (signature, ix_index) DO NOTHING
"#)
.bind(&signatures)
.bind(&slots)
.bind(&block_times)
.bind(&program_ids)
.bind(&names)
.bind(&is_inner)
.bind(&ix_indexes)
.bind(&args)
.bind(&accounts)
.execute(&pool)
.await?;
```

### 7.3 Upsert for Account States

Account states use `ON CONFLICT DO UPDATE` to keep only the latest state:

```sql
INSERT INTO account_states
    (pubkey, account_type, program_id, slot_updated, lamports, data, data_len)
SELECT * FROM UNNEST($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (pubkey) DO UPDATE SET
    account_type = EXCLUDED.account_type,
    slot_updated = EXCLUDED.slot_updated,
    lamports = EXCLUDED.lamports,
    data = EXCLUDED.data,
    data_len = EXCLUDED.data_len,
    updated_at = NOW()
WHERE EXCLUDED.slot_updated > account_states.slot_updated;
```

The `WHERE EXCLUDED.slot_updated > account_states.slot_updated` clause prevents stale data from overwriting newer state during parallel processing or out-of-order block delivery.

### 7.4 Transaction Boundaries

**Per-block transactions:**

```rust
async fn persist_block(pool: &PgPool, block: DecodedBlock) -> Result<()> {
    let mut tx = pool.begin().await?;

    // 1. Insert instructions (batch UNNEST)
    insert_instructions_batch(&mut *tx, &block.instructions).await?;

    // 2. Upsert account states (batch UNNEST with ON CONFLICT)
    upsert_account_states_batch(&mut *tx, &block.account_states).await?;

    // 3. Update indexer cursor
    update_indexer_state(&mut *tx, &block.program_id, block.slot).await?;

    // 4. Update program stats counters
    update_program_stats(&mut *tx, &block.program_id, &block.stats_delta).await?;

    tx.commit().await?;
    Ok(())
}
```

**Why per-block:**

- **Atomicity:** If the indexer crashes mid-block, the transaction rolls back. On restart, the block is re-processed from scratch. No partial data.
- **Cursor consistency:** The `indexer_state.last_slot` is updated in the same transaction as the data. The cursor always points to a fully-persisted block.
- **Size:** A single program's data per block is typically 0-100 rows. This is well within PostgreSQL's optimal transaction size.
- **No long-running transactions:** Each block transaction completes in milliseconds, avoiding lock contention and WAL buildup.

### 7.5 Connection Pool Sizing

```rust
let pool = PgPoolOptions::new()
    .max_connections(20)          // <=80% of PG max_connections (default 100)
    .min_connections(5)           // Pre-warm 5 connections
    .max_lifetime(Duration::from_secs(1800))  // 30 min max lifetime
    .idle_timeout(Duration::from_secs(300))   // 5 min idle timeout
    .acquire_timeout(Duration::from_secs(10)) // 10s acquire timeout
    .connect(&database_url)
    .await?;
```

**Pool size rationale:**

- Solarix is a single-purpose indexer, not a multi-tenant web server
- Write path: 1-3 connections (block processing pipeline)
- Read path (API): 5-15 connections (concurrent API requests)
- Headroom: 2-5 connections for maintenance (VACUUM, index refresh)
- Total: 20 connections is conservative and leaves room for other tools (psql, monitoring)

### 7.6 Write-Ahead Considerations

PostgreSQL's WAL (Write-Ahead Log) provides crash safety by default:

- All changes are written to WAL before data files
- `synchronous_commit = on` (default) ensures durability
- For bulk backfill, consider `synchronous_commit = off` per-session to trade durability for 2-3x write speed improvement, then re-enable for real-time mode

```sql
-- Backfill mode: faster writes, small window of data loss risk on crash
SET LOCAL synchronous_commit = off;
```

**Index build during backfill:**

- For initial bulk load of historical data, consider:
  1. Drop GIN indexes
  2. Bulk insert all data
  3. Recreate GIN indexes (up to 3x faster than incremental maintenance)
  4. Switch to real-time mode with indexes active

---

## 8. Performance Projections

### 8.1 Storage Overhead

| Component                                              | Estimated Size per 1M Instructions  |
| ------------------------------------------------------ | ----------------------------------- |
| Typed columns only (signature, slot, program_id, etc.) | ~150-200 MB                         |
| JSONB `args` column (avg 200 bytes per document)       | ~200-300 MB                         |
| JSONB `accounts` column (avg 500 bytes per document)   | ~500-700 MB                         |
| B-tree indexes (5 indexes)                             | ~100-150 MB                         |
| GIN index (`jsonb_path_ops` on args)                   | ~60-120 MB (20-30% of JSONB column) |
| **Total per 1M instructions**                          | **~1.0-1.5 GB**                     |

For comparison, a fully normalized schema would be ~600-800 MB for the same data (40-50% less). The JSONB overhead is the cost of universal schema flexibility.

### 8.2 GIN Index Performance

| Table Size | GIN Build Time (estimate) | GIN Index Size  | Query Latency (containment) |
| ---------- | ------------------------- | --------------- | --------------------------- |
| 100K rows  | 2-5 seconds               | 10-30 MB        | 5-15 ms                     |
| 1M rows    | 20-60 seconds             | 60-120 MB       | 15-40 ms                    |
| 10M rows   | 3-10 minutes              | 600 MB - 1.2 GB | 30-80 ms                    |
| 100M rows  | 30-120 minutes            | 6-12 GB         | 50-200 ms                   |

**Notes:**

- Build times assume `maintenance_work_mem = 1GB`
- Query latency assumes `jsonb_path_ops` with containment operator
- GIN always uses Bitmap Index Scan (never Index Scan or Index Only Scan)
- `fastupdate = on` (default) defers index maintenance to reduce write latency

### 8.3 Write Performance

| Batch Size  | Method        | Estimated Throughput   |
| ----------- | ------------- | ---------------------- |
| 1 row       | Single INSERT | 500-1,000 rows/sec     |
| 100 rows    | UNNEST        | 5,000-10,000 rows/sec  |
| 1,000 rows  | UNNEST        | 10,000-20,000 rows/sec |
| 10,000 rows | UNNEST        | 15,000-30,000 rows/sec |
| 10,000 rows | COPY binary   | 30,000-60,000 rows/sec |

**Solarix context:** At Solana's 400ms block time, with 1-50 program-relevant transactions per block, the indexer needs ~25-125 rows/second sustained throughput. Even single-row inserts would suffice, but batched UNNEST provides a 10-20x safety margin.

### 8.4 Query Latency Summary

| Query Type                            | Expected Latency | Limiting Factor                 |
| ------------------------------------- | ---------------- | ------------------------------- |
| Single account by pubkey              | < 1 ms           | B-tree unique index             |
| Instructions by signature             | < 1 ms           | B-tree unique index             |
| Instructions by slot range            | 5-20 ms          | B-tree range scan               |
| Instructions by program + name        | 5-15 ms          | Composite B-tree                |
| JSONB containment filter              | 15-60 ms         | GIN bitmap scan                 |
| JSONB range filter (no expr index)    | 50-200 ms        | Sequential filter after GIN     |
| JSONB range filter (with expr index)  | 5-20 ms          | B-tree expression index         |
| Time-range aggregation (<1M rows)     | 20-100 ms        | B-tree + GroupAggregate         |
| Time-range aggregation (materialized) | < 5 ms           | Index scan on materialized view |
| Program stats (pre-computed)          | < 1 ms           | Primary key lookup              |

### 8.5 Connection Pool Impact

| Pool Size | Concurrent API Requests | Write Pipeline Connections | Risk                                             |
| --------- | ----------------------- | -------------------------- | ------------------------------------------------ |
| 5         | 3-4                     | 1-2                        | API bottleneck under load                        |
| 10        | 7-8                     | 2-3                        | Good for development/demo                        |
| 20        | 14-16                   | 3-4                        | Recommended for production                       |
| 50        | 40+                     | 5-10                       | Overkill for single-indexer; wastes PG resources |

---

## 9. Alternative Approaches Evaluation

### 9.1 Table-Per-Account-Type (Fully Normalized)

**Approach:** Create a dedicated table for each account type and instruction type defined in the IDL.

```sql
-- Example: dynamic table for TokenAccount
CREATE TABLE program_xyz_token_account (
    pubkey VARCHAR(44) PRIMARY KEY,
    mint VARCHAR(44) NOT NULL,
    owner VARCHAR(44) NOT NULL,
    amount BIGINT NOT NULL,
    delegate VARCHAR(44),
    state SMALLINT NOT NULL,
    ...
);
```

**Pros:**

- Maximum query performance (native B-tree on every column)
- ~40-50% less storage vs JSONB
- PostgreSQL planner has full statistics
- Type safety at the database level

**Cons:**

- **Dynamic DDL at runtime:** `CREATE TABLE` for each account type. Schema changes when IDL evolves. Migration headaches.
- **Explosion of tables:** A program with 15 types = 15 tables. 10 programs = 150 tables. Each needs indexes, vacuuming, monitoring.
- **Nested types lose structure:** Structs and enums must be flattened or require sub-tables. Vec/HashMap fields are especially problematic.
- **Inconsistent API:** Each program's tables have different schemas. The API layer must generate different SQL per program.
- **Column type mapping complexity:** IDL `u128` has no PostgreSQL equivalent. `Option<Vec<T>>` requires nullable array columns or separate tables.

**Verdict:** Rejected for the universal indexer. The complexity of dynamic DDL and type mapping negates the performance benefit. Would be appropriate for a single-program indexer where the schema is known at compile time.

### 9.2 Pure JSONB (Single Table, No Typed Columns)

**Approach:** Store everything in a single table with minimal typed columns and maximum JSONB.

```sql
CREATE TABLE events (
    id BIGSERIAL PRIMARY KEY,
    program_id VARCHAR(44),
    event_type VARCHAR(20),  -- 'instruction' or 'account_state'
    data JSONB NOT NULL
);
```

**Pros:**

- Simplest schema possible
- Zero schema changes for new programs
- Maximum flexibility

**Cons:**

- **Catastrophic query planning:** PostgreSQL cannot maintain statistics on JSONB field values. Heap.io measured 2,000x slower queries due to planner estimate errors.
- **No type safety:** `slot` is a string in JSONB, not a BIGINT. Range queries require casting on every row.
- **Storage explosion:** Every row stores `"slot"`, `"signature"`, `"program_id"` as JSONB keys instead of column offsets. 2x+ overhead.
- **GIN index covers everything:** Massive index covering both metadata and payload.

**Verdict:** Rejected. The query performance degradation is unacceptable. Common metadata queries (by slot, signature, program_id) should be native column lookups, not JSONB containment.

### 9.3 Column-Per-Field (Fully Flattened)

**Approach:** Dynamically add a column for every field in the IDL using `ALTER TABLE ADD COLUMN`.

**Pros:**

- Near-optimal query performance once columns exist
- PostgreSQL planner has full statistics

**Cons:**

- **Schema changes break everything:** Adding/removing columns requires `ALTER TABLE`, which takes an ACCESS EXCLUSIVE lock on large tables.
- **Nullable column explosion:** A table supporting 10 programs has columns for all 10 programs' fields. 95% are NULL.
- **Column limit:** PostgreSQL supports ~1600 columns. Complex programs can have 50+ fields. 32 programs = 1600 columns.
- **Type mapping:** Same issues as table-per-type but worse, because all types share one table.

**Verdict:** Rejected. Fragile, unmaintainable, and hits PostgreSQL limits quickly.

### 9.4 TimescaleDB for Time-Series Aggregation

**Approach:** Use TimescaleDB extension for the instructions table, with automatic time-based partitioning and continuous aggregates.

**Pros:**

- **Massive aggregation speedup:** 1.2x-14,000x faster for time-based queries
- **Continuous aggregates:** Incrementally maintained materialized views (unlike PostgreSQL's full-refresh-only approach)
- **Automatic partitioning:** By time (and optionally by program_id)
- **Compression:** 90%+ compression on historical data
- **Native PostgreSQL extension:** Full SQL compatibility, runs on same server

**Cons:**

- **Extra dependency:** Docker Compose needs TimescaleDB image instead of vanilla PostgreSQL
- **Licensing complexity:** TimescaleDB Community is open-source, but some features (multi-node, compression policies) require Timescale License
- **Overkill for bounty scale:** With <10M rows, standard PostgreSQL with date_trunc + GROUP BY is fast enough
- **Judge expectations:** Bounty says "PostgreSQL". TimescaleDB is a valid choice but adds a talking point about dependency justification

**Verdict:** Deferred. Not needed for the bounty submission. The standard PostgreSQL materialized view approach handles the aggregation requirement. TimescaleDB becomes valuable at >100M rows where time-based partitioning and continuous aggregates provide orders-of-magnitude improvement. Document as a "future optimization" in the README.

### 9.5 Separate Read Replicas

**Approach:** PostgreSQL streaming replication with a read-only replica for API queries.

**Pros:**

- Write path and read path never contend
- Can scale reads horizontally

**Cons:**

- **Docker Compose complexity:** Two PostgreSQL instances, replication configuration, connection routing
- **Replication lag:** Reads may be slightly stale (typically <1 second)
- **Overkill:** A single PostgreSQL instance easily handles the bounty's read/write ratio

**Verdict:** Rejected for bounty. A single PostgreSQL instance with connection pooling is sufficient. Read replicas are a production scaling concern, not a bounty submission concern.

---

## 10. Recommendations Summary

### For the Bounty Submission

1. **Use the hybrid schema** (Sections 3.1-3.4) with typed common columns + JSONB payload
2. **B-tree indexes** on all typed metadata columns (Section 4.1)
3. **Single GIN index** with `jsonb_path_ops` on `instructions.args` and `account_states.data` (Section 4.2)
4. **`INSERT...UNNEST`** with `ON CONFLICT` for batch writes, per-block transaction boundaries (Section 7.2-7.4)
5. **Pre-computed `program_stats`** table updated atomically in the block transaction (Section 3.4)
6. **Skip materialized views** initially -- add them if aggregation queries are too slow during testing
7. **Connection pool of 20** with sqlx defaults (Section 7.5)
8. **Standard PostgreSQL 16+** -- no TimescaleDB, no read replicas

### For Post-Bounty Production

1. **Add expression indexes** on frequently-queried JSONB fields per-program
2. **Add materialized views** for account type projections and hourly aggregations
3. **Evaluate TimescaleDB** when instruction table exceeds 100M rows
4. **Add partial GIN indexes** filtered by `is_inner_ix = FALSE` to reduce index size
5. **Consider read replicas** if API latency exceeds SLO under concurrent load
6. **Implement GIN index maintenance** -- `REINDEX CONCURRENTLY` on schedule to prevent bloat

### Schema Creation SQL (Complete)

The indexer should execute this DDL at startup when connecting to a fresh database:

```sql
-- Core tables
CREATE TABLE IF NOT EXISTS instructions (...);     -- Section 3.1
CREATE TABLE IF NOT EXISTS account_states (...);   -- Section 3.2
CREATE TABLE IF NOT EXISTS indexer_state (...);    -- Section 3.3
CREATE TABLE IF NOT EXISTS program_stats (...);    -- Section 3.4

-- B-tree indexes
CREATE INDEX IF NOT EXISTS idx_instructions_slot ...;
CREATE INDEX IF NOT EXISTS idx_instructions_block_time ...;
CREATE INDEX IF NOT EXISTS idx_instructions_program_id ...;
CREATE INDEX IF NOT EXISTS idx_instructions_name ...;
CREATE INDEX IF NOT EXISTS idx_instructions_signature ...;
CREATE INDEX IF NOT EXISTS idx_account_states_program_id ...;
CREATE INDEX IF NOT EXISTS idx_account_states_type ...;
CREATE INDEX IF NOT EXISTS idx_account_states_slot ...;

-- GIN indexes
CREATE INDEX IF NOT EXISTS idx_instructions_args_gin ...;
CREATE INDEX IF NOT EXISTS idx_account_states_data_gin ...;
```

All DDL uses `IF NOT EXISTS` for idempotent startup. No migration framework needed.

---

## 11. Sources

### PostgreSQL JSONB & GIN Performance

- [Indexing JSONB in Postgres - Crunchy Data](https://www.crunchydata.com/blog/indexing-jsonb-in-postgres)
- [Understanding Postgres GIN Indexes: The Good and the Bad - pganalyze](https://pganalyze.com/blog/gin-index)
- [GIN Indexes in PostgreSQL - SQLpassion (January 2026)](https://www.sqlpassion.at/archive/2026/01/12/gin-indexes-in-postgresql/)
- [PostgreSQL JSONB Performance Guide - SitePoint](https://www.sitepoint.com/postgresql-jsonb-query-performance-indexing/)
- [Postgres 2025: Advanced JSON Query Optimization - Markaicode](https://markaicode.com/postgres-json-optimization-techniques-2025/)
- [PostgreSQL Documentation: GIN Indexes](https://www.postgresql.org/docs/current/gin.html)
- [PostgreSQL Documentation: Built-in GIN Operator Classes](https://www.postgresql.org/docs/16/gin-builtin-opclasses.html)

### JSONB Operator Class Comparison

- [JSONB and GIN index operators - Gleb Otochkin (Google Cloud)](https://medium.com/google-cloud/jsonb-and-gin-index-operators-in-postgresql-cea096fbb373)
- [PostgreSQL JSONB Operator Classes - Josef Machytka](https://medium.com/@josef.machytka/postgresql-jsonb-operator-classes-of-gin-indexes-and-their-usage-0bf399073a4c)
- [Postgres JSONB Usage and Performance Analysis - Eresh Gorantla](https://medium.com/geekculture/postgres-jsonb-usage-and-performance-analysis-cdbd1242a018)

### Storage Overhead & Hybrid Architecture

- [When To Avoid JSONB In A PostgreSQL Schema - Heap.io](https://www.heap.io/blog/when-to-avoid-jsonb-in-a-postgresql-schema)
- [PostgreSQL JSONB - Powerful Storage for Semi-Structured Data - Architecture Weekly](https://www.architecture-weekly.com/p/postgresql-jsonb-powerful-storage)
- [JSONB: PostgreSQL's Secret Weapon - Rick Hightower](https://medium.com/@richardhightower/jsonb-postgresqls-secret-weapon-for-flexible-data-modeling-cf2f5087168f)
- [PostgreSQL as a JSON database - AWS](https://aws.amazon.com/blogs/database/postgresql-as-a-json-database-advanced-patterns-and-best-practices/)
- [5mins of Postgres: JSONB TOAST performance cliffs - pganalyze](https://pganalyze.com/blog/5mins-postgres-jsonb-toast)

### Batch Insert & Write Performance

- [A 10x faster batch job with PostgreSQL and Rust - Kerkour](https://kerkour.com/postgresql-batching)
- [Benchmarking PostgreSQL Batch Ingest - Tiger Data](https://www.tigerdata.com/blog/benchmarking-postgresql-batch-ingest)
- [Boosting Postgres INSERT Performance by 2x With UNNEST - Tiger Data](https://www.tigerdata.com/blog/boosting-postgres-insert-performance)
- [Rust bulk insert to PostgreSQL using sqlx - alxolr](https://www.alxolr.com/articles/rust-bulk-insert-to-postgre-sql-using-sqlx)
- [sqlx-batch crate](https://crates.io/crates/sqlx-batch)
- [sqlx QueryBuilder documentation](https://docs.rs/sqlx/latest/sqlx/struct.QueryBuilder.html)

### Materialized Views

- [PostgreSQL Documentation: REFRESH MATERIALIZED VIEW](https://www.postgresql.org/docs/current/sql-refreshmaterializedview.html)
- [Postgres REFRESH MATERIALIZED VIEW Guide - Epsio](https://www.epsio.io/blog/postgres-refresh-materialized-view-a-comprehensive-guide)
- [How to Use Materialized Views in PostgreSQL - OneUptime](https://oneuptime.com/blog/post/2026-01-25-use-materialized-views-postgresql/view)
- [Optimizing Materialized Views - Shiv Iyer](https://medium.com/@ShivIyer/optimizing-materialized-views-in-postgresql-best-practices-for-performance-and-efficiency-3e8169c00dc1)

### Connection Pooling

- [Pool in sqlx - Rust docs](https://docs.rs/sqlx/latest/sqlx/struct.Pool.html)
- [PoolOptions in sqlx - Rust docs](https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html)
- [Why Every Rust Microservice Is Doing Connection Pooling Wrong](https://medium.com/@theopinionatedev/why-every-rust-microservice-using-sqlx-is-doing-connection-pooling-wrong-cf809b5601b3)
- [How to Handle Database Connection Pooling in Rust](https://oneuptime.com/blog/post/2026-01-07-rust-database-connection-pooling/view)

### GIN Write Overhead

- [Debugging random slow writes in PostgreSQL - safts](https://iamsafts.com/posts/postgres-gin-performance/)
- [PostgreSQL GIN Tips and Tricks](https://www.postgresql.org/docs/16/gin-tips.html)

### TimescaleDB & Alternatives

- [TimescaleDB vs. PostgreSQL for time-series data](https://medium.com/timescale/timescaledb-vs-6a696248104e)
- [PostgreSQL + TimescaleDB: 1000x Faster Queries - Tiger Data](https://www.tigerdata.com/blog/postgresql-timescaledb-1000x-faster-queries-90-data-compression-and-much-more)
- [Managing Time-Series Data: Why TimescaleDB Beats PostgreSQL - Mad Devs](https://maddevs.io/writeups/time-series-data-management-with-timescaledb/)

### Blockchain Indexer Architecture

- [Scaling Transaction Indexing with PostgreSQL - CoinDesk Data](https://data.coindesk.com/blogs/on-chain-series-viii-scaling-transaction-indexing-with-postgresql-and-hybrid-storage-architecture)
- [How to Build a Custom Blockchain Indexer - Chainscore Labs](https://chainscorelabs.com/en/guides/developer-experience-dx-blockchain-tools-and-analytics/blockchain-data-engineering/how-to-build-a-custom-blockchain-indexer-from-scratch)
- [The Bitcoin Blockchain PostgreSQL Schema - Gregory Trubetskoy](https://grisha.org/blog/2017/12/15/blockchain-and-postgres/)
