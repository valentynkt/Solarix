# Solarix Architecture

Detailed technical architecture of the Solarix universal Solana indexer.

## Table of Contents

- [System Overview](#system-overview)
- [Pipeline Architecture](#pipeline-architecture)
- [Data Architecture](#data-architecture)
- [Module Boundaries](#module-boundaries)
- [IDL Processing](#idl-processing)
- [Borsh Decoding](#borsh-decoding)
- [Storage Layer](#storage-layer)
- [API Layer](#api-layer)
- [Error Handling](#error-handling)
- [Concurrency Model](#concurrency-model)
- [Reliability Features](#reliability-features)

---

## System Overview

Solarix is a four-layer pipeline that reads Solana blockchain data, decodes it using Anchor IDLs, stores it in dynamically-generated PostgreSQL schemas, and serves it through a REST API.

```
 ┌─────────────────────────────────────────────────────────────────────────────────┐
 │                                  SOLARIX                                       │
 │                                                                                │
 │  ┌─ Read ────────────┐  ┌─ Decode ──────────┐  ┌─ Store ──────────────────┐   │
 │  │                    │  │                    │  │                          │   │
 │  │  RpcClient         │  │  IdlManager        │  │  SchemaGenerator        │   │
 │  │  (HTTP JSON-RPC)  ─┼──┼─►                  │  │  (IDL → DDL)            │   │
 │  │                    │  │  ChainparserDecoder─┼──┼─► StorageWriter         │   │
 │  │  WsTransactionStream│ │  (Borsh registry)  │  │     (batch + upsert)    │   │
 │  │  (logsSubscribe)  ─┼──┼─►                  │  │                         │   │
 │  │                    │  │                    │  │                    ┌─────┤   │
 │  └────────────────────┘  └────────────────────┘  │                    │ DB  │   │
 │                                                   └────────────────────┤     │   │
 │  ┌─ Serve ───────────┐                                               │     │   │
 │  │                    │          PipelineOrchestrator                  │PG16 │   │
 │  │  axum Router      │           (state machine)                      │     │   │
 │  │  12 endpoints    ─┼───────────────────────────────────────────────►│     │   │
 │  │  QueryBuilder     │                                               │     │   │
 │  │  (dynamic SQL)    │          ProgramRegistry                       └─────┤   │
 │  └────────────────────┘          (registration lifecycle)                    │   │
 │                                                                                │
 └─────────────────────────────────────────────────────────────────────────────────┘
          ▲                                                        ▲
          │                                                        │
     API Clients                                            Solana RPC Node
```

### Startup Sequence

```
main.rs
  │
  ├─► init_pool() + bootstrap_system_tables()
  │
  ├─► SELECT FROM programs WHERE status = 'schema_created'
  │
  ├─► No programs found?
  │     └─► Start API-only mode (serves /health + POST /api/programs)
  │
  └─► Programs found?
        ├─► Spawn API server task ──────► handles requests ──► DB queries
        │
        └─► Spawn pipeline task
              ├─► decide_initial_state()
              ├─► Read checkpoint from DB
              └─► Run backfill + streaming concurrently
```

---

## Pipeline Architecture

### State Machine

The pipeline operates as a 5-state machine with well-defined transitions:

```
                                 checkpoint < tip
                ┌──────────────┐ ──────────────────► ┌──────────────┐
                │ Initializing │                     │ Backfilling  │
                └──────┬───────┘                     └──────┬───────┘
                       │ no gap                       caught up │
                       ▼                                        ▼
                ┌──────────────┐    gap detected     ┌──────────────┐
                │              │ ◄────────────────── │              │
                │  Streaming   │                     │  CatchingUp  │
                │              │ ──────────────────► │              │
                └──────┬───────┘    gap filled       └──────────────┘
                       │
                       │ SIGTERM / SIGINT
                       ▼
                ┌──────────────┐
                │ ShuttingDown │ ──► drain channels ──► flush DB ──► exit
                └──────────────┘
```

### Cold Start Decision

The `decide_initial_state()` function is a **pure function** (no I/O) that determines the initial pipeline state:

| Condition                               | Result                                  |
| --------------------------------------- | --------------------------------------- |
| No checkpoint + `start_slot` configured | Backfill from `start_slot` to chain tip |
| No checkpoint + no `start_slot`         | Stream from current tip                 |
| Checkpoint < chain tip                  | Backfill gap, then stream               |
| Checkpoint == chain tip                 | Stream immediately                      |
| Checkpoint > chain tip                  | Fatal error (wrong cluster?)            |

### Concurrent Backfill + Streaming (Option C)

During cold start, both the backfill and streaming paths run simultaneously:

```
  Backfill Path                                    Streaming Path

  getBlocks(range)                                 logsSubscribe (WebSocket)
       │                                                │
       ▼                                                ▼
  getBlock(slot)                                   getTransaction(sig)
       │                                                │
       ▼                                                ▼
  Decode (Borsh → JSON)                            Decode (Borsh → JSON)
       │                                                │
       ▼                                                ▼
  INSERT ... ON CONFLICT DO NOTHING ──────►  Same tables  ◄── INSERT ... ON CONFLICT DO NOTHING
                                                   │
                                            ┌──────┴──────┐
                                            │ PostgreSQL  │
                                            └─────────────┘
```

Both paths write to identical tables with `INSERT ... ON CONFLICT DO NOTHING`. Signature-based dedup ensures no duplicate data. If Solarix crashes and restarts, both paths resume from their respective checkpoints.

### Backfill Loop

```
for each chunk (50,000 slots):
    1. getBlocks(start, end)          # Get non-empty slot list
    2. for each slot in chunk:
        a. getBlock(slot)             # Full block with transactions
        b. Decode instructions        # Borsh → JSON via IDL
        c. Decode accounts            # getProgramAccounts
    3. Batch write (atomic tx)
    4. Update checkpoint
    5. Log progress (slots/sec, ETA)
```

### Gap Detection

While streaming, if the received slot jumps beyond `last_slot + 1`, the pipeline transitions to `CatchingUp`:

1. Spawns a mini-backfill for the missed slot range
2. Continues processing streaming transactions
3. Returns to `Streaming` once the gap is filled

---

## Data Architecture

### Schema-Per-Program Isolation

Each registered program gets its own PostgreSQL schema:

```
  ┌─────────────────────────────────────────────────────────────┐
  │ public schema                                               │
  │                                                             │
  │  programs                        indexer_state              │
  │  ┌─────────────────────┐         ┌────────────────────────┐ │
  │  │ program_id (PK)     │────────►│ program_id (FK)        │ │
  │  │ program_name        │         │ status                 │ │
  │  │ schema_name (UNIQ)  │         │ last_processed_slot    │ │
  │  │ idl_hash            │         │ last_heartbeat         │ │
  │  │ idl_source          │         │ total_instructions     │ │
  │  │ status              │         │ total_accounts         │ │
  │  │ created_at          │         │ error_message          │ │
  │  │ updated_at          │         └────────────────────────┘ │
  │  └─────────────────────┘                                    │
  └─────────────────────────────────────────────────────────────┘

  ┌─────────────────────────────────────────────────────────────┐
  │ jupiter_v6_jup6lkmu schema   (one schema per program)      │
  │                                                             │
  │  pool (account table)      token_ledger (account table)     │
  │  ┌──────────────────┐      ┌──────────────────┐            │
  │  │ pubkey (PK)      │      │ pubkey (PK)      │            │
  │  │ slot_updated     │      │ slot_updated     │            │
  │  │ lamports         │      │ lamports         │            │
  │  │ data (JSONB)     │      │ data (JSONB)     │            │
  │  │ token_a_mint     │      │ owner            │   ...      │
  │  │ fee_rate         │      │ balance          │            │
  │  └──────────────────┘      └──────────────────┘            │
  │                                                             │
  │  _instructions (append-only)    _checkpoints    _metadata   │
  │  ┌──────────────────────┐  ┌──────────────┐  ┌──────────┐ │
  │  │ id (PK, BIGSERIAL)   │  │ stream (PK)  │  │ ix_name  │ │
  │  │ signature             │  │ last_slot    │  │ field    │ │
  │  │ slot                  │  │ last_sig     │  │ type     │ │
  │  │ instruction_name      │  │ updated_at   │  │ is_acct  │ │
  │  │ args (JSONB)          │  └──────────────┘  └──────────┘ │
  │  │ accounts (JSONB)      │                                  │
  │  │ data (JSONB)          │                                  │
  │  └──────────────────────┘                                   │
  └─────────────────────────────────────────────────────────────┘
```

Schema naming pattern: `{sanitized_name}_{first_8_program_id}` prevents collisions when different programs share the same IDL name.

### Promoted Column Strategy

The schema generator inspects each IDL field type and decides whether to promote it to a native PostgreSQL column:

| IDL Type           | PostgreSQL Type        | Promoted? |
| ------------------ | ---------------------- | --------- |
| `u8`, `u16`        | `SMALLINT` / `INTEGER` | Yes       |
| `u32`              | `INTEGER`              | Yes       |
| `u64`, `i64`       | `BIGINT`               | Yes       |
| `u128`, `i128`     | `NUMERIC(39,0)`        | Yes       |
| `u256`, `i256`     | `NUMERIC(78,0)`        | Yes       |
| `bool`             | `BOOLEAN`              | Yes       |
| `String`, `Pubkey` | `TEXT`                 | Yes       |
| `f32`              | `REAL`                 | Yes       |
| `f64`              | `DOUBLE PRECISION`     | Yes       |
| `Option<T>`        | Type of T (nullable)   | Yes       |
| `Vec<T>`           | JSONB only             | No        |
| `Struct`, `Enum`   | JSONB only             | No        |
| `Array<u8, N>`     | `BYTEA`                | Yes       |

**Every field** is always present in the JSONB `data` column. Promoted columns are extracted at write time for fast native-type queries.

### u64 Overflow Guard

Solana uses `u64` extensively, but PostgreSQL `BIGINT` is signed (max `i64::MAX`). For values exceeding `i64::MAX`:

- Promoted column receives `NULL`
- Full value is preserved as a string in the JSONB `data` column
- Queries on the promoted column miss these rows; JSONB queries still work

### Checkpoint System

Two-tier checkpoint tracking:

```
public.indexer_state        -- global pipeline state per program
  program_id, status, last_processed_slot, last_heartbeat

{schema}._checkpoints       -- per-stream slot cursors
  stream ("backfill" | "stream"), last_slot, last_signature
```

Checkpoints are updated atomically within the same transaction as block writes, ensuring crash-safe progress tracking.

---

## Module Boundaries

### Trait Seams

Four trait interfaces define the architectural boundaries. Each is mockable for unit testing:

```
  Trait                    Defined In          Implemented By
  ─────────────────────    ─────────────────   ──────────────────────
  SolarixDecoder           decoder/mod.rs      ChainparserDecoder
    + Send + Sync
    decode_instruction()
    decode_account()

  BlockSource              pipeline/rpc.rs     RpcClient
    + Send + Sync
    get_block()
    get_slot()

  AccountSource            pipeline/rpc.rs     RpcClient
    + Send + Sync
    get_program_accounts()
    get_multiple_accounts()

  TransactionStream        pipeline/ws.rs      WsTransactionStream
    + Send + Sync
    connect()
```

### Dependency Graph

```
                    ┌──────────┐
                    │ main.rs  │
                    └────┬─────┘
                         │
            ┌────────────┼────────────┐
            ▼            ▼            ▼
       ┌─────────┐ ┌──────────┐ ┌──────────┐
       │  api/   │ │ pipeline/│ │ registry │
       └────┬────┘ └────┬─────┘ └────┬─────┘
            │           │             │
            │      ┌────┴────┐        │
            │      ▼         ▼        │
            │ ┌────────┐ ┌────────┐   │
            │ │decoder/│ │storage/│◄──┘
            │ └────┬───┘ └────┬───┘
            │      │          │
            └──────┼──────────┘
                   ▼
              ┌─────────┐     ┌──────────┐
              │ types.rs │     │ config.rs│
              └──────────┘     └──────────┘
```

No circular dependencies. The `types` module sits at the bottom with shared data structures. Modules only depend downward.

---

## IDL Processing

### Fetch Cascade

```
  Register program_id
         │
         ▼
    IDL in cache? ─── yes ──► Use cached IDL ──► Done
         │ no
         ▼
    Manual upload provided? ─── yes ──► Parse + validate ──┐
         │ no                                               │
         ▼                                                  │
    Fetch from on-chain PDA                                 │
    seeds = ["anchor:idl", program_id]                      │
         │                                                  │
         ├── getAccountInfo ──► parse binary ──► decompress ┤
         │                      [8B disc]                   │
         │                      [32B authority]             │
         │                      [4B len]                    │
         │                      [zlib payload]              │
         │                                                  │
         ├── NotFound ──► Check bundled: idls/{id}.json ──┤
         │                      │                           │
         │                  not found                       │
         │                      │                           │
         │                      ▼                           ▼
         │              IdlError::NotFound      Validate v0.30+ format
         │                                      (metadata.spec exists)
         │                                              │
         │                                   ┌──────────┼──────────┐
         │                                   │ yes                 │ no
         │                                   ▼                     ▼
         │                            SHA-256 hash          UnsupportedFormat
         │                            canonical JSON
         │                                   │
         └───────────────────────────────────►│
                                              ▼
                                          Cached + Done
```

### IDL Account Binary Layout

```
Offset  Size    Field
0       8       Discriminator
8       32      Authority (pubkey)
40      4       Data length (LE u32)
44      N       zlib-compressed IDL JSON (max 16 MiB decompressed)
```

---

## Borsh Decoding

The `ChainparserDecoder` recursively walks IDL type definitions to decode Borsh-serialized bytes:

### Discriminator Matching

```
Instruction discriminator = SHA-256("global:{snake_case_name}")[0..8]
Account discriminator     = SHA-256("account:{PascalCase_name}")[0..8]
```

The decoder first reads 8 bytes from the data, matches against pre-computed discriminators from the IDL, then decodes the remaining bytes according to the matched type definition.

### Type System Coverage

18+ IDL types supported with recursive descent:

- **Primitives**: u8-u256, i8-i256, bool, f32, f64, String, Bytes, Pubkey
- **Collections**: `Vec<T>`, `Option<T>`, `Array<T, N>`
- **Compounds**: Structs (named/tuple fields), Enums (discriminator-tagged variants)
- **Solana-specific**: `COption<T>` (4-byte fixed-size tag, differs from Rust's 1-byte `Option`)
- **Generics**: Resolved through IDL type parameters

### Special Cases

| Case                          | Handling                                                         |
| ----------------------------- | ---------------------------------------------------------------- |
| `u128`/`i128`/`u256`/`i256`   | Serialized as JSON strings to prevent precision loss             |
| `f32`/`f64` NaN/Infinity      | Converted to strings (`"NaN"`, `"Infinity"`) for JSON compliance |
| `Pubkey`                      | Base58-encoded string                                            |
| `COption<T>`                  | 4-byte u32 tag: 0 = null, 1 = Some(T). Fixed-size inner.         |
| Unknown discriminator         | Logged at `warn!`, skipped (not fatal)                           |
| >90% decode failures in batch | Logged at `error!` (likely IDL mismatch)                         |

---

## Storage Layer

### Write Path

```
  Pipeline                    StorageWriter                  PostgreSQL
     │                              │                             │
     │  write_block(schema,         │                             │
     │   instructions, accounts,    │                             │
     │   slot)                      │                             │
     │─────────────────────────────►│                             │
     │                              │  BEGIN                      │
     │                              │────────────────────────────►│
     │                              │                             │
     │                              │  INSERT INTO _instructions  │
     │                              │  VALUES ($1...$N)           │
     │                              │  ON CONFLICT DO NOTHING     │
     │                              │────────────────────────────►│
     │                              │                             │
     │                              │  ┌─ for each account type ─┐│
     │                              │  │ discover promoted cols  ││
     │                              │  │ (cached after 1st query)││
     │                              │  │ extract promoted values ││
     │                              │  │                         ││
     │                              │  │ INSERT INTO {type}      ││
     │                              │  │ ON CONFLICT (pubkey)    ││
     │                              │  │ DO UPDATE SET ...       ││
     │                              │  └─────────────────────────┘│
     │                              │────────────────────────────►│
     │                              │                             │
     │                              │  INSERT INTO _checkpoints   │
     │                              │  ON CONFLICT DO UPDATE      │
     │                              │────────────────────────────►│
     │                              │                             │
     │                              │  COMMIT                     │
     │                              │────────────────────────────►│
     │                              │                             │
     │◄─────────────────────────────│  WriteResult                │
```

### Instruction Batch Insert

Instructions use `INSERT ... VALUES` with multiple rows in a single statement. The dedup unique index on `(signature, instruction_index, COALESCE(inner_index, -1))` handles duplicates from concurrent backfill + streaming.

### Account Upsert

Accounts use `INSERT ... ON CONFLICT (pubkey) DO UPDATE` to maintain latest state. Promoted columns are dynamically bound based on the table's actual schema, discovered via `information_schema.columns` on first write per table (then cached).

---

## API Layer

### Request Flow

```
  HTTP Request
       │
       ▼
  axum Router ──► Handler Function
                       │
                       ├── Needs IDL? ── yes ──► ProgramRegistry (read lock)
                       │                              │
                       │                              ▼
                       │                         Parse Filters
                       │                         (validate vs IDL)
                       │                              │
                       ▼                              ▼
                  QueryBuilder (dynamic SQL)
                       │
                       ▼
                  PostgreSQL
                       │
                       ▼
                  JSON Response ──► HTTP Response
```

### Filter Resolution

Filters pass through a three-stage pipeline:

1. **Parse**: Tokenize `?filter=data.amount_gt=1000000` into `(column, operator, value)`
2. **Resolve**: Classify column as promoted (native SQL) or JSONB (path extraction), validate operator applicability
3. **Build SQL**: Generate parameterized WHERE clause

```sql
-- Promoted column (native type, indexed)
WHERE "balance" > $1

-- JSONB field (path extraction)
WHERE ("data"->>'nested_field')::BIGINT > $1

-- JSONB containment (for complex matches)
WHERE "data" @> $1::jsonb
```

### Pagination

- **Accounts**: Offset-based (`?limit=50&offset=100`)
- **Instructions**: Cursor-based (`?limit=50&cursor=<opaque>`) for stable ordering on append-only data

---

## Error Handling

### Error Enum Hierarchy

```
  PipelineError
    ├── DecodeError      (from decoder/)
    ├── StorageError     (from storage/)
    └── IdlError         (from idl/)

  ApiError
    └── RegistrationError
          ├── IdlError
          └── StorageError
```

Five module-level error enums, each with classification:

| Enum            | Module            | Variants                                                                   | Classification                               |
| --------------- | ----------------- | -------------------------------------------------------------------------- | -------------------------------------------- |
| `IdlError`      | `idl/mod.rs`      | FetchFailed, ParseFailed, NotFound, UnsupportedFormat, DecompressionFailed | retryable (fetch), fatal (parse)             |
| `DecodeError`   | `decoder/mod.rs`  | UnknownDiscriminator, DeserializationFailed, IdlNotLoaded, UnsupportedType | skip-and-log                                 |
| `StorageError`  | `storage/mod.rs`  | ConnectionFailed, DdlFailed, WriteFailed, CheckpointFailed                 | retryable (connection), fatal (DDL)          |
| `PipelineError` | `pipeline/mod.rs` | RpcFailed, WebSocketDisconnect, RateLimited, Decode, Storage, Idl, Fatal   | `is_retryable()` method                      |
| `ApiError`      | `api/mod.rs`      | 11 variants                                                                | Maps to HTTP status codes via `IntoResponse` |

### Error Classification Strategy

```
retryable       --> exponential backoff, retry up to timeout
                    Examples: 429 rate limit, network timeout, WS disconnect

skip-and-log    --> warn! level, continue processing
                    Examples: unknown discriminator, decode failure on single tx

fatal           --> error! level, halt pipeline
                    Examples: DB unreachable, invalid config, checkpoint > chain tip
```

Decode failures are tracked per-batch. If >90% of transactions in a chunk fail to decode, it escalates to `error!` level (probable IDL mismatch rather than individual data issues).

---

## Concurrency Model

### Shared State

```
  ┌──────────────────────────────────────────┐
  │       Arc<RwLock<ProgramRegistry>>       │
  │  ┌──────────────────────────────────┐    │
  │  │  ProgramRegistry                 │    │
  │  │  ├── IdlManager + caches         │    │
  │  │  └── program status tracking     │    │
  │  └──────────────────────────────────┘    │
  └────────────┬─────────────┬───────────────┘
               │             │
    ┌──────────┴──┐   ┌──────┴──────────┐
    │ read lock   │   │ write lock      │
    │ (concurrent)│   │ (rare)          │
    ▼             │   ▼                 │
  API Handler 1   │ Register Handler    │
  API Handler 2   │                     │
  API Handler N   │                     │
                  │                     │
                  │ Pipeline receives   │
                  │ owned IDL clone     │
                  │ at startup — no     │
                  │ lock contention     │
                  └─────────────────────┘
```

- **Read lock**: All query handlers, IDL lookups — concurrent, no contention
- **Write lock**: Program registration only — rare operation, acceptable blocking
- **Pipeline**: Receives owned IDL clone at startup, no lock contention during indexing

### Channel Architecture

```
  RpcClient ──► [mpsc(256)] ──► Decoder ──► [mpsc(256)] ──► StorageWriter ──► PostgreSQL
                                                                  ▲
  WsStream  ──► [mpsc(256)] ──► Decoder ──► [mpsc(256)] ─────────┘
```

Bounded `tokio::sync::mpsc` channels (capacity 256) provide backpressure between stages. If the writer falls behind, senders block, naturally throttling the reader.

### Shutdown Protocol

```
  SIGTERM / SIGINT
       │
       ▼
  CancellationToken.cancel()
       │
       ├──────────────────────────────────┐
       ▼                                  ▼
  API Server                         Pipeline
  stop accepting                     stop fetching
  new requests                       new blocks
                                          │
                                          ▼
                                     Drain in-flight
                                     messages (15s timeout)
                                          │
                                          ▼
                                     Flush pending writes
                                     to DB (10s timeout)
                                          │
                                          ▼
                                     UPDATE indexer_state
                                     SET status = 'stopped'
                                          │
                                          ▼
                                     pool.close()
```

The `CancellationToken` from `tokio-util` propagates shutdown across all tasks. Configurable timeouts prevent hung shutdown.

---

## Reliability Features

### Rate Limiting

`governor` crate with GCRA (Generic Cell Rate Algorithm):

- Default: 10 requests/second (matches public RPC limits)
- Async-native — waits for permit without busy-spinning
- Applied to all outbound RPC calls

### Retry with Backoff

`backon` crate with exponential backoff:

- Initial delay: 500ms
- Maximum delay: 30s
- Total timeout: 5 minutes
- Jitter: built-in randomization
- Only retries classified-retryable errors (429, timeouts, network errors)

### WebSocket Reliability

- **Heartbeat**: Ping every 30s, pong timeout 10s
- **Auto-reconnect**: On disconnect, reconnect with fresh subscription
- **Dedup cache**: LRU cache of 10,000 recent signatures prevents reprocessing on reconnect
- **Gap detection**: Slot jumps trigger mini-backfill to fill missed data

### Data Integrity

- All block writes are atomic (single PostgreSQL transaction)
- Checkpoint updated within the same transaction as data writes
- `INSERT ON CONFLICT DO NOTHING` makes all writes idempotent
- Account upserts use `ON CONFLICT (pubkey) DO UPDATE` for latest-state semantics
- DDL uses `IF NOT EXISTS` everywhere — self-bootstrapping, no migration tool needed

### Graceful Shutdown

1. **Signal catch**: SIGTERM/SIGINT via `tokio::signal`
2. **Drain**: Process in-flight channel messages (configurable timeout)
3. **Flush**: Final DB writes with timeout
4. **Status update**: Set `indexer_state.status = 'stopped'`
5. **Pool close**: Clean connection teardown
