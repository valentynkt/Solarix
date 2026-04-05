# Epic 3: Transaction Decoding & Batch Indexing

User can trigger batch indexing (slot range or signature list) and see decoded instruction args + account states land in the database -- the core "it actually works" moment for judges.

## Story 3.1: SolarixDecoder Trait & Instruction Decoding

As a developer,
I want a decoder abstraction that can deserialize Anchor instruction arguments from raw transaction data using an IDL,
So that instruction data is decoded into queryable JSON and the decoder implementation can be swapped without affecting other modules.

**Acceptance Criteria:**

**Given** the `SolarixDecoder` trait in `decoder/mod.rs`
**When** I inspect it
**Then** it defines `decode_instruction(&self, program_id: &str, data: &[u8], idl: &Idl) -> Result<DecodedInstruction, DecodeError>` and `decode_account(&self, program_id: &str, data: &[u8], idl: &Idl) -> Result<DecodedAccount, DecodeError>`

**Given** the `ChainparserDecoder` implementing `SolarixDecoder`
**When** it receives instruction data with a valid 8-byte discriminator
**Then** it computes `SHA-256("global:<snake_case_name>")[0..8]` for each instruction in the IDL
**And** it matches the data's first bytes against the IDL discriminators (using the `discriminator` field from the IDL entry, supporting variable-length discriminators for v0.31+)
**And** it deserializes the remaining bytes as Borsh according to the matched instruction's `args` field definitions
**And** the result is a `serde_json::Value` object with field names matching the IDL

**Given** a type definition with `serialization` field set to `Bytemuck` or `BytemuckUnsafe`
**When** the decoder encounters it during deserialization
**Then** it returns `DecodeError::UnsupportedType` with a message indicating non-Borsh serialization is not supported

**Given** a u128, i128, u256, or i256 value
**When** the decoder serializes it to JSON
**Then** the value is always represented as a JSON string (regardless of magnitude) to prevent precision loss in JavaScript consumers

**Given** the `DecodeError` enum
**When** I inspect it
**Then** it includes variants: `UnknownDiscriminator`, `DeserializationFailed`, `IdlNotLoaded`, `UnsupportedType`
**And** it derives `thiserror::Error`
**And** `impl From<DecodeError> for PipelineError` exists

## Story 3.2: Account State Decoding

As a user,
I want account state data decoded from on-chain accounts using the IDL,
So that current account states are stored as queryable typed data alongside instruction history.

**Acceptance Criteria:**

**Given** raw account data with a valid discriminator
**When** `decode_account()` is called
**Then** it matches the first bytes against `idl.accounts[]` entries using each entry's `discriminator` field (SHA-256("account:<PascalCase>")[0..8] for standard Anchor, variable-length for v0.31+)
**And** it looks up the matched account name in `idl.types[]` to get the struct definition
**And** it deserializes the remaining bytes as Borsh according to the struct's field definitions
**And** the result includes both the account type name and the decoded `serde_json::Value`

**Given** account data with an unrecognized discriminator
**When** `decode_account()` is called
**Then** it returns `DecodeError::UnknownDiscriminator` with the hex-encoded discriminator bytes
**And** the caller (pipeline) logs at `warn!` level and skips the account without crashing

**Given** a batch of transactions where >90% fail to decode
**When** the pipeline processes the chunk
**Then** it logs at `error!` level with a message indicating likely IDL version mismatch
**And** includes the program_id and chunk slot range in the log context

**Given** a type definition with nested structs, enums, Options, and Vecs
**When** the decoder processes it
**Then** it recursively descends through the type tree, handling: structs (named fields -> JSON object), enums (variant name + optional fields), Option (1-byte tag, None -> null), Vec (4-byte u32 length prefix), arrays (fixed-size, no length prefix), COption (4-byte u32 tag, fixed-size inner types only)
**And** recursive depth is capped at 64 levels to prevent stack overflow on pathological inputs

## Story 3.3: RPC Block Source & Rate-Limited Fetching

As a system,
I want to fetch block data from Solana RPC with rate limiting and retry logic,
So that batch indexing respects public RPC limits and recovers gracefully from transient failures.

**Acceptance Criteria:**

**Given** the `BlockSource` trait in `pipeline/rpc.rs`
**When** I inspect it
**Then** it defines async methods for: `get_blocks(start_slot, end_slot) -> Result<Vec<u64>>`, `get_block(slot) -> Result<BlockData>`, `get_slot() -> Result<u64>`

**Given** the `AccountSource` trait in `pipeline/rpc.rs`
**When** I inspect it
**Then** it defines async methods for: `get_program_accounts(program_id) -> Result<Vec<String>>` (pubkeys only via dataSlice trick), `get_multiple_accounts(pubkeys) -> Result<Vec<AccountData>>` (batches of max 100)

**Given** the `RpcBlockSource` implementation
**When** it makes any RPC call
**Then** every request includes `maxSupportedTransactionVersion: 0`
**And** block data requests use `encoding: "base64"` for bandwidth efficiency
**And** all requests pass through a `governor` rate limiter (default 10 RPS, configurable via `SOLARIX_RPC_RPS`)
**And** failed requests are retried via `backon` with exponential backoff (500ms initial, 30s max, 5min total timeout) and 50% randomization jitter

**Given** a `getBlocks` call for a range exceeding 500,000 slots
**When** the RPC source processes it
**Then** it automatically chunks into multiple `getBlocks` calls of max 500K each

**Given** a `getBlock` call that returns JSON-RPC error `-32009` (skipped slot)
**When** the RPC source processes it
**Then** the error is classified as permanent (not retried) and the slot is skipped

**Given** the `SOLARIX_INDEX_FAILED_TXS` config flag is `false` (default)
**When** a block is fetched
**Then** transactions where `meta.err != null` are filtered out before passing to the decode stage

**Given** the `PipelineError` enum
**When** I inspect it
**Then** it includes variants: `RpcFailed`, `WebSocketDisconnect`, `RateLimited`, `Decode(DecodeError)`, `Storage(StorageError)`, `Fatal(String)`
**And** it has an `is_retryable(&self) -> bool` method

## Story 3.4: Storage Writer & Atomic Checkpointing

As a developer,
I want a storage writer that can persist decoded instructions and accounts to PostgreSQL with atomic per-block writes and checkpoint updates,
So that the pipeline has a reliable, crash-safe persistence layer.

**Acceptance Criteria:**

**Given** decoded instruction data for a block
**When** the storage writer processes it
**Then** it performs `INSERT...UNNEST` into the `_instructions` table with column vector decomposition (separate typed vectors per column)
**And** uses `ON CONFLICT DO NOTHING` on the unique constraint for deduplication
**And** JSONB array values are bound using `sqlx::types::Json<T>` wrapper

**Given** decoded account data
**When** the storage writer processes it
**Then** it performs an upsert into the account type's table with `ON CONFLICT (pubkey) DO UPDATE ... WHERE EXCLUDED.slot_updated > {table}.slot_updated`
**And** u64 values > i64::MAX are stored as NULL in promoted columns but preserved as strings in the JSONB `data` column

**Given** a block's worth of data is written
**When** the transaction commits
**Then** both the data inserts and the checkpoint update (`_checkpoints` table, `last_slot` for 'backfill' stream) are in the same database transaction
**And** if the transaction fails, nothing is committed (atomic per-block writes)

**Given** the pipeline is interrupted mid-write
**When** it restarts
**Then** it reads the last checkpoint from `_checkpoints` and resumes from the next unprocessed slot
**And** at most one chunk of work is re-processed (idempotent due to `ON CONFLICT DO NOTHING`)

## Story 3.5: Batch Indexing Pipeline Orchestrator

As a user,
I want to index historical transactions for a registered program by specifying a slot range or signature list,
So that past on-chain activity is captured in the database for querying.

**Acceptance Criteria:**

**Given** a registered program with schema created
**When** the pipeline starts in batch mode with a slot range
**Then** the PipelineOrchestrator enters `Initializing` state, loads config and checkpoint, then transitions to `Backfilling`
**And** it chunks the slot range into operational chunks (default 50K slots, configurable via `SOLARIX_BACKFILL_CHUNK_SIZE`)
**And** for each chunk: fetches block slots via `getBlocks`, fetches each block via `getBlock`, filters transactions for the target program, decodes instructions via SolarixDecoder, sends decoded data through bounded mpsc(256) channel to the storage writer

**Given** a registered program
**When** the pipeline starts in batch mode with a list of signatures
**Then** it fetches each transaction via `getTransaction` (rate-limited), decodes, and writes to storage via the writer

**Given** current account states need to be fetched (FR17)
**When** the pipeline runs account snapshot
**Then** it calls `get_program_accounts` (pubkeys only via `dataSlice: {offset: 0, length: 0}`), then batches `get_multiple_accounts` (max 100 per call), decodes each account, and upserts into the appropriate account tables via the storage writer

**Given** backfill progress
**When** the pipeline is running
**Then** it logs progress at `info!` level every 10 seconds including slots processed, slots/sec, and ETA

---
