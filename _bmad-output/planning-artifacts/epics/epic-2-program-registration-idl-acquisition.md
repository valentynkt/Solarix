# Epic 2: Program Registration & IDL Acquisition

User can register any Anchor program by ID (auto-fetches IDL from chain) or upload an IDL manually. The system generates a full PostgreSQL schema with typed columns, indexes, and JSONB safety net -- visible proof of runtime dynamism.

## Story 2.1: IDL Manager & On-Chain Fetch

As a user,
I want the system to fetch and parse an Anchor IDL from on-chain PDA given a program ID,
So that I can index any Anchor program without manually providing its IDL.

**Acceptance Criteria:**

**Given** a valid Anchor program ID with an on-chain IDL (v0.30+ format)
**When** the IdlManager receives a fetch request for that program ID
**Then** it derives the IDL PDA address using `["anchor:idl", program_id]` seeds
**And** it fetches the account data via `getAccountInfo` RPC call
**And** it parses the account data layout: `[authority: 32 bytes][data_len: 4 bytes LE][zlib_compressed_json: N bytes]`, skipping the authority prefix before decompressing the zlib payload and parsing as JSON
**And** it validates the IDL has a `metadata.spec` field confirming v0.30+ format
**And** it caches the parsed IDL in an internal `HashMap<String, Idl>` keyed by program ID
**And** it returns the parsed `anchor_lang_idl_spec::Idl` struct

**Given** the on-chain IDL account does not exist for the program ID
**When** the IdlManager attempts to fetch
**Then** it falls back to the bundled `idls/` directory, searching for a matching IDL file by program ID
**And** if no bundled IDL is found, it returns `IdlError::NotFound` with a message suggesting manual upload

**Given** an IDL with an unsupported format (missing `metadata.spec` or legacy v0.29)
**When** the IdlManager parses it
**Then** it returns `IdlError::UnsupportedFormat` with a descriptive message

**Given** the `IdlError` enum
**When** I inspect it
**Then** it includes variants: `FetchFailed`, `ParseFailed`, `NotFound`, `UnsupportedFormat`, `DecompressionFailed`
**And** it derives `thiserror::Error` with descriptive `#[error("...")]` messages

**Given** a network failure during IDL fetch
**When** the RPC call fails
**Then** the error is classified as retryable and wrapped in `IdlError::FetchFailed` with the underlying error as source

## Story 2.2: Manual IDL Upload & Program Registration

As a user,
I want to upload an IDL manually and register a program for indexing,
So that I can index programs that don't have on-chain IDLs or use custom IDL modifications.

**Acceptance Criteria:**

**Given** a valid IDL JSON provided via the IdlManager's manual upload path
**When** the IdlManager processes the upload
**Then** it parses and validates the IDL (same validation as on-chain fetch)
**And** it caches the parsed IDL keyed by the provided program ID
**And** the IDL source is recorded as "manual_upload" (vs "on_chain" or "bundled")

**Given** the `ProgramRegistry` struct
**When** I inspect it
**Then** it wraps `IdlManager` + per-program schema metadata + decoder instances
**And** it is shared across pipeline and API via `Arc<RwLock<ProgramRegistry>>`
**And** a `register_program()` method orchestrates: IDL fetch/upload -> validate -> store metadata -> return program info

**Given** a program is registered
**When** the registration completes
**Then** the `programs` table is updated with: `program_id`, `program_name` (from IDL `metadata.name`), `schema_name` (derived), `idl_hash` (SHA-256 of deterministic JSON serialization with sorted keys), `idl_source`, `status = 'registered'`
**And** the `indexer_state` table is populated with initial state for the program

**Given** the `types.rs` shared types module
**When** I inspect it
**Then** it contains `DecodedInstruction`, `DecodedAccount`, `BlockData`, `TransactionData` structs
**And** `DecodedInstruction` includes fields: `signature`, `slot`, `block_time`, `instruction_name`, `args` (serde_json::Value), `program_id`, `accounts` (Vec), `instruction_index` (u8), `inner_index` (Option<u8>)
**And** `DecodedAccount` includes fields: `pubkey`, `slot_updated`, `lamports`, `data` (serde_json::Value), `account_type`, `program_id`

**Given** a program ID that is already registered
**When** the user attempts to register it again
**Then** the system returns an appropriate error indicating the program is already registered

## Story 2.3: Dynamic Schema Generation (DDL Engine)

As a user,
I want the system to automatically generate a complete PostgreSQL schema from an Anchor IDL,
So that indexed data lands in properly typed, queryable tables without any manual database setup.

**Acceptance Criteria:**

**Given** a parsed Anchor IDL for a registered program
**When** the schema generator runs
**Then** it creates a PostgreSQL schema named `{sanitized_name}_{lowercase_first_8_of_base58_program_id}` using `CREATE SCHEMA IF NOT EXISTS`
**And** all identifiers are double-quoted in generated DDL

**Given** the IDL defines account types
**When** the schema generator processes each account type
**Then** it creates one table per account type with: `pubkey TEXT PRIMARY KEY`, `slot_updated BIGINT NOT NULL`, `write_version BIGINT NOT NULL DEFAULT 0`, `lamports BIGINT NOT NULL`, `data JSONB NOT NULL` (full decoded payload), `is_closed BOOLEAN NOT NULL DEFAULT FALSE`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`
**And** top-level scalar IDL fields are promoted to native typed columns (nullable, even for non-optional IDL fields, to handle u64 overflow)
**And** the type mapping covers: u8/i8 -> SMALLINT, u16 -> INTEGER (max 65535 overflows SMALLINT), i16 -> SMALLINT, u32/i32 -> INTEGER, u64/i64 -> BIGINT, u128/i128 -> NUMERIC(39,0), f32 -> REAL, f64 -> DOUBLE PRECISION, bool -> BOOLEAN, string -> TEXT, pubkey -> TEXT, bytes/Vec<u8>/[u8;N] -> BYTEA, Option<T> -> nullable column of T's type, Vec<T>/arrays/structs/enums/tuples -> not promoted (JSONB only)
**And** `IF NOT EXISTS` is used for all CREATE statements

**Given** the IDL defines instructions
**When** the schema generator processes the instructions
**Then** it creates a single `_instructions` table with: `id BIGSERIAL PRIMARY KEY`, `signature TEXT NOT NULL`, `slot BIGINT NOT NULL`, `block_time BIGINT`, `instruction_name TEXT NOT NULL`, `instruction_index SMALLINT NOT NULL`, `inner_index SMALLINT`, `args JSONB NOT NULL`, `accounts JSONB NOT NULL`, `data JSONB NOT NULL`, `is_inner_ix BOOLEAN NOT NULL DEFAULT FALSE`
**And** a unique constraint on `(signature, instruction_index, COALESCE(inner_index, -1))`

**Given** schema generation completes
**When** I inspect the created tables
**Then** B-tree indexes exist on: `slot` (all tables), `signature` (\_instructions), `instruction_name` (\_instructions), `block_time` (\_instructions)
**And** GIN indexes with `jsonb_path_ops` exist on `data` columns
**And** a `_metadata` table exists with key-value pairs: `program_id`, `program_name`, `idl_hash`, `idl_version`, `schema_created_at`, `account_types` (JSON array), `instruction_types` (JSON array)
**And** a `_checkpoints` table exists with: `stream TEXT PRIMARY KEY` (e.g., 'backfill', 'realtime', 'accounts'), `last_slot BIGINT`, `last_signature VARCHAR(88)`, `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`

**Given** the `sanitize_identifier` function
**When** processing IDL names
**Then** it strips non-alphanumeric characters (except underscores), lowercases, prepends underscore if starting with digit, falls back to `_unnamed` if empty after sanitization
**And** truncates to 63 bytes (not characters) on byte boundaries

**Given** DDL execution
**When** the schema generator runs all statements
**Then** they are executed via `sqlx::raw_sql()` within an implicit transaction (semicolon-concatenated)
**And** any statement failure rolls back all DDL for that program
**And** the `programs` table `status` is updated to `'schema_created'` on success or `'error'` on failure with `error_message`

---
