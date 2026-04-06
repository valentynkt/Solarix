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

```mermaid
graph TB
    subgraph External
        SOL[Solana RPC Node]
        CLIENT[API Clients]
    end

    subgraph Solarix
        subgraph "Read Layer"
            RPC[RpcClient<br/>HTTP JSON-RPC]
            WS[WsTransactionStream<br/>logsSubscribe]
        end

        subgraph "Decode Layer"
            IDL[IdlManager<br/>fetch + cache + validate]
            DEC[ChainparserDecoder<br/>Borsh type registry]
        end

        subgraph "Store Layer"
            SCH[SchemaGenerator<br/>IDL → DDL]
            WR[StorageWriter<br/>batch + upsert]
        end

        subgraph "Serve Layer"
            API[axum Router<br/>12 endpoints]
            QB[QueryBuilder<br/>dynamic SQL + filters]
        end

        PIPE[PipelineOrchestrator<br/>state machine]
        REG[ProgramRegistry<br/>registration lifecycle]
    end

    DB[(PostgreSQL 16)]

    SOL --> RPC
    SOL --> WS
    RPC --> PIPE
    WS --> PIPE
    PIPE --> DEC
    IDL --> DEC
    DEC --> WR
    WR --> DB
    SCH --> DB
    REG --> IDL
    REG --> SCH
    API --> QB
    QB --> DB
    CLIENT --> API
```

### Startup Sequence

```mermaid
sequenceDiagram
    participant M as main.rs
    participant DB as PostgreSQL
    participant API as axum Server
    participant P as PipelineOrchestrator

    M->>DB: init_pool() + bootstrap_system_tables()
    M->>DB: SELECT FROM programs WHERE status = 'schema_created'
    alt No registered programs
        M->>API: Start API-only mode
        Note right of API: Serves /health + POST /api/programs
    else Programs found
        M->>API: Spawn API server task
        M->>P: Spawn pipeline task
        par API handles requests
            API->>DB: Query endpoints
        and Pipeline indexes data
            P->>P: decide_initial_state()
            P->>DB: Read checkpoint
            P->>P: Run backfill + streaming
        end
    end
```

---

## Pipeline Architecture

### State Machine

The pipeline operates as a 5-state machine with well-defined transitions:

```mermaid
stateDiagram-v2
    [*] --> Initializing : startup

    Initializing --> Backfilling : checkpoint_slot < chain_tip
    Initializing --> Streaming : checkpoint_slot == chain_tip OR no checkpoint

    state "Concurrent Processing" as concurrent {
        Backfilling --> Streaming : backfill complete
        Streaming --> CatchingUp : gap detected (slot jump)
        CatchingUp --> Streaming : gap filled
    }

    Backfilling --> ShuttingDown : SIGTERM/SIGINT
    Streaming --> ShuttingDown : SIGTERM/SIGINT
    CatchingUp --> ShuttingDown : SIGTERM/SIGINT

    ShuttingDown --> [*] : drain + flush complete
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

```mermaid
graph LR
    subgraph "Backfill Path"
        B1[getBlocks<br/>slot range] --> B2[getBlock<br/>per slot] --> B3[Decode] --> B4[Write<br/>ON CONFLICT<br/>DO NOTHING]
    end

    subgraph "Streaming Path"
        S1[logsSubscribe<br/>WebSocket] --> S2[getTransaction<br/>per signature] --> S3[Decode] --> S4[Write<br/>ON CONFLICT<br/>DO NOTHING]
    end

    B4 --> DB[(Same tables)]
    S4 --> DB
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

```mermaid
erDiagram
    public_programs {
        varchar program_id PK
        text program_name
        text schema_name UK
        varchar idl_hash
        text idl_source
        text status
        timestamptz created_at
        timestamptz updated_at
    }

    public_indexer_state {
        varchar program_id FK
        text status
        bigint last_processed_slot
        timestamptz last_heartbeat
        text error_message
        bigint total_instructions
        bigint total_accounts
    }

    public_programs ||--o| public_indexer_state : "tracks"
```

Program-specific schemas follow the naming pattern `{sanitized_name}_{first_8_program_id}` to prevent collisions:

```sql
-- Example: registering Jupiter v6
CREATE SCHEMA IF NOT EXISTS "jupiter_v6_jup6lkmu";

-- Account tables (one per IDL account type)
CREATE TABLE IF NOT EXISTS "jupiter_v6_jup6lkmu"."pool" (
    pubkey          TEXT PRIMARY KEY,
    slot_updated    BIGINT NOT NULL,
    write_version   BIGINT NOT NULL DEFAULT 0,
    lamports        BIGINT NOT NULL,
    data            JSONB NOT NULL,
    is_closed       BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Promoted columns (simple scalars from IDL)
    token_a_mint    TEXT,
    token_b_mint    TEXT,
    fee_rate        INTEGER
);

-- GIN index for JSONB queries
CREATE INDEX IF NOT EXISTS idx_jupiter_v6_jup6lkmu_pool_data
    ON "jupiter_v6_jup6lkmu"."pool" USING GIN(data jsonb_path_ops);

-- Unified instructions table (append-only)
CREATE TABLE IF NOT EXISTS "jupiter_v6_jup6lkmu"."_instructions" (
    id                  BIGSERIAL PRIMARY KEY,
    signature           TEXT NOT NULL,
    slot                BIGINT NOT NULL,
    block_time          BIGINT,
    instruction_name    TEXT NOT NULL,
    instruction_index   SMALLINT NOT NULL,
    inner_index         SMALLINT,
    args                JSONB NOT NULL,
    accounts            JSONB NOT NULL,
    data                JSONB NOT NULL,
    is_inner_ix         BOOLEAN NOT NULL DEFAULT FALSE
);

-- Dedup index
CREATE UNIQUE INDEX IF NOT EXISTS idx_jupiter_v6_jup6lkmu__instructions_sig_ix
    ON "jupiter_v6_jup6lkmu"."_instructions"
    (signature, instruction_index, COALESCE(inner_index, -1));
```

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

```mermaid
graph TB
    subgraph "decoder/mod.rs"
        SD["trait SolarixDecoder<br/>+ Send + Sync"]
    end

    subgraph "pipeline/rpc.rs"
        BS["trait BlockSource<br/>+ Send + Sync"]
        AS["trait AccountSource<br/>+ Send + Sync"]
    end

    subgraph "pipeline/ws.rs"
        TS["trait TransactionStream<br/>+ Send + Sync"]
    end

    subgraph Implementations
        CD[ChainparserDecoder]
        RC[RpcClient]
        WTS[WsTransactionStream]
    end

    CD -.->|implements| SD
    RC -.->|implements| BS
    RC -.->|implements| AS
    WTS -.->|implements| TS
```

| Trait               | Methods                                             | Purpose                            |
| ------------------- | --------------------------------------------------- | ---------------------------------- |
| `SolarixDecoder`    | `decode_instruction()`, `decode_account()`          | Borsh bytes → typed JSON           |
| `BlockSource`       | `get_block()`, `get_slot()`                         | Block fetching abstraction         |
| `AccountSource`     | `get_program_accounts()`, `get_multiple_accounts()` | Account fetching abstraction       |
| `TransactionStream` | `connect()`                                         | WebSocket subscription abstraction |

### Dependency Graph

```mermaid
graph BT
    types[types.rs]
    config[config.rs]
    idl[idl/]
    decoder[decoder/]
    registry[registry.rs]
    pipeline[pipeline/]
    storage[storage/]
    api[api/]
    main[main.rs]

    idl --> types
    decoder --> types
    decoder --> idl
    registry --> idl
    registry --> storage
    pipeline --> decoder
    pipeline --> storage
    pipeline --> types
    storage --> types
    api --> storage
    api --> registry
    main --> api
    main --> pipeline
    main --> registry
    main --> config
```

No circular dependencies. The `types` module sits at the bottom with shared data structures. Modules only depend downward.

---

## IDL Processing

### Fetch Cascade

```mermaid
graph TD
    START[Register program_id] --> CACHE{IDL in cache?}
    CACHE -->|yes| DONE[Use cached IDL]
    CACHE -->|no| MANUAL{Manual upload<br/>provided?}
    MANUAL -->|yes| PARSE[Parse + validate]
    MANUAL -->|no| ONCHAIN[Fetch from on-chain PDA]

    ONCHAIN --> PDA["Derive PDA:<br/>seeds = ['anchor:idl', program_id]"]
    PDA --> RPC[getAccountInfo]
    RPC --> DECOMPRESS["Parse binary layout:<br/>[8B disc][32B authority][4B len][zlib]"]
    DECOMPRESS --> PARSE

    ONCHAIN -->|NotFound| BUNDLED["Check bundled IDLs:<br/>idls/{program_id}.json"]
    BUNDLED -->|found| PARSE
    BUNDLED -->|not found| ERROR[IdlError::NotFound]

    PARSE --> VALIDATE{"v0.30+ format?<br/>(metadata.spec exists)"}
    VALIDATE -->|yes| HASH[SHA-256 hash on<br/>canonical JSON]
    VALIDATE -->|no| REJECT[IdlError::UnsupportedFormat]
    HASH --> DONE
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

```mermaid
sequenceDiagram
    participant P as Pipeline
    participant W as StorageWriter
    participant DB as PostgreSQL

    P->>W: write_block(schema, instructions, accounts, slot)
    W->>DB: BEGIN
    W->>DB: INSERT INTO _instructions<br/>VALUES ($1...$N)<br/>ON CONFLICT DO NOTHING
    loop Each account type
        W->>W: Discover promoted columns<br/>(cached after first query)
        W->>W: Extract promoted values<br/>from JSONB data
        W->>DB: INSERT INTO {type}<br/>ON CONFLICT (pubkey)<br/>DO UPDATE SET ...
    end
    W->>DB: INSERT INTO _checkpoints<br/>ON CONFLICT DO UPDATE
    W->>DB: COMMIT
    W-->>P: WriteResult
```

### Instruction Batch Insert

Instructions use `INSERT ... VALUES` with multiple rows in a single statement. The dedup unique index on `(signature, instruction_index, COALESCE(inner_index, -1))` handles duplicates from concurrent backfill + streaming.

### Account Upsert

Accounts use `INSERT ... ON CONFLICT (pubkey) DO UPDATE` to maintain latest state. Promoted columns are dynamically bound based on the table's actual schema, discovered via `information_schema.columns` on first write per table (then cached).

---

## API Layer

### Request Flow

```mermaid
graph LR
    REQ[HTTP Request] --> ROUTER[axum Router]
    ROUTER --> HANDLER[Handler Function]
    HANDLER --> REGISTRY{Needs IDL?}
    REGISTRY -->|yes| REG[ProgramRegistry<br/>read lock]
    REG --> FILTER[Parse Filters<br/>validate vs IDL]
    REGISTRY -->|no| QUERY
    FILTER --> QUERY[QueryBuilder<br/>dynamic SQL]
    QUERY --> DB[(PostgreSQL)]
    DB --> SERIALIZE[JSON Response]
    SERIALIZE --> RES[HTTP Response]
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

```mermaid
graph TD
    PE[PipelineError] --> DE[DecodeError]
    PE --> SE[StorageError]
    PE --> IE[IdlError]

    AE[ApiError] --> RE[RegistrationError]
    RE --> IE2[IdlError]
    RE --> SE2[StorageError]
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
retryable       → exponential backoff, retry up to timeout
                  Examples: 429 rate limit, network timeout, WS disconnect

skip-and-log    → warn! level, continue processing
                  Examples: unknown discriminator, decode failure on single tx

fatal           → error! level, halt pipeline
                  Examples: DB unreachable, invalid config, checkpoint > chain tip
```

Decode failures are tracked per-batch. If >90% of transactions in a chunk fail to decode, it escalates to `error!` level (probable IDL mismatch rather than individual data issues).

---

## Concurrency Model

### Shared State

```mermaid
graph TD
    subgraph "Arc<RwLock<ProgramRegistry>>"
        REG[ProgramRegistry]
        IDL[IdlManager + caches]
    end

    API1[API Handler 1] -->|read lock| REG
    API2[API Handler 2] -->|read lock| REG
    REGISTER[Register Handler] -->|write lock<br/>rare| REG
    PIPELINE[Pipeline] -->|read via<br/>owned clone| IDL
```

- **Read lock**: All query handlers, IDL lookups — concurrent, no contention
- **Write lock**: Program registration only — rare operation, acceptable blocking
- **Pipeline**: Receives owned IDL clone at startup, no lock contention during indexing

### Channel Architecture

```
RpcClient ──→ [mpsc(256)] ──→ Decoder ──→ [mpsc(256)] ──→ StorageWriter
                                                              ↓
WsStream  ──→ [mpsc(256)] ──→ Decoder ──→ [mpsc(256)] ──→ StorageWriter
                                                              ↓
                                                         PostgreSQL
```

Bounded `tokio::sync::mpsc` channels (capacity 256) provide backpressure between stages. If the writer falls behind, senders block, naturally throttling the reader.

### Shutdown Protocol

```mermaid
sequenceDiagram
    participant SIG as Signal Handler
    participant CT as CancellationToken
    participant API as API Server
    participant PIPE as Pipeline
    participant DB as PostgreSQL

    SIG->>CT: cancel()
    par Graceful shutdown
        CT->>API: Stop accepting requests
        CT->>PIPE: Stop fetching new blocks
    end
    PIPE->>PIPE: Drain in-flight messages<br/>(15s timeout)
    PIPE->>DB: Flush pending writes<br/>(10s timeout)
    PIPE->>DB: Update indexer_state → 'stopped'
    API->>API: Finish in-flight requests
    DB->>DB: pool.close()
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
