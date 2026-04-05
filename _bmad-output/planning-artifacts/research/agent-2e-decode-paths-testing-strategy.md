# Agent 2E: Decode Paths Architecture + Testing Strategy

**Date:** 2026-04-05
**Status:** Complete
**Scope:** Dual decode architecture (instruction args + account states) and comprehensive testing strategy for Solarix

---

## 1. Executive Summary

Solarix requires two distinct decode paths mandated by the bounty: **instruction argument decoding** (from transaction data) and **account state decoding** (from on-chain account data). Both paths share the same underlying Borsh deserialization engine but differ in how they locate type definitions in the IDL, how discriminators are computed, and how input data arrives.

This document designs:

1. A unified decoder abstraction with two decode paths sharing a common type-walking engine
2. Complete transaction and account processing pipelines
3. A comprehensive testing strategy covering unit tests, property-based tests (proptest), integration tests (litesvm), API tests (axum-test), and CI/CD

The testing strategy is designed to satisfy bounty judging criterion #5: "Code quality, architecture, and presence of tests."

---

## 2. Dual Decode Architecture

### 2.1 The Two Paths

Both paths follow the same high-level pattern but differ in where they get type information from the IDL:

```
Path 1: Instruction Argument Decoding
  Input: instruction data bytes (from transaction)
  [8-byte discriminator][Borsh-encoded args]
  Discriminator formula: SHA-256("global:<snake_case_name>")[0..8]
  Type source: idl.instructions[].args — an ordered list of (name, IdlType) pairs
  Decode: sequential field-by-field, each arg is a top-level field
  Output: { instruction_name, args: { field1: value1, field2: value2 } }

Path 2: Account State Decoding
  Input: account data bytes (from getAccountInfo/getProgramAccounts)
  [8-byte discriminator][Borsh-encoded struct fields]
  Discriminator formula: SHA-256("account:<PascalCaseName>")[0..8]
  Type source: idl.accounts[].name → look up in idl.types[] → decode as struct
  Decode: struct fields concatenated in declaration order
  Output: { account_type, data: { field1: value1, field2: value2 } }
```

### 2.2 Shared Type-Walking Engine

Both paths ultimately reduce to the same operation: **decode a sequence of typed fields from a byte cursor using IDL type definitions**. The difference is only in how those field definitions are obtained:

```
                    ┌─────────────────────────┐
                    │   IDL Type Registry     │
                    │  (types, accounts, ix)  │
                    └────────┬────────────────┘
                             │
              ┌──────────────┴──────────────┐
              │                             │
    ┌─────────▼─────────┐       ┌───────────▼──────────┐
    │ Instruction Path  │       │  Account Path        │
    │                   │       │                      │
    │ discriminator →   │       │ discriminator →      │
    │ find instruction  │       │ find account name →  │
    │ → get args list   │       │ look up in types →   │
    │ → decode fields   │       │ get struct fields →  │
    │                   │       │ decode fields        │
    └─────────┬─────────┘       └───────────┬──────────┘
              │                             │
              └──────────────┬──────────────┘
                             │
                    ┌────────▼────────────────┐
                    │  decode_fields()        │
                    │  Borsh cursor + field   │
                    │  definitions → JSON     │
                    │                         │
                    │  Uses decode_type()     │
                    │  for each field         │
                    └────────┬────────────────┘
                             │
                    ┌────────▼────────────────┐
                    │  decode_type()          │
                    │  Recursive descent:     │
                    │  IdlType → serde_json   │
                    │  ::Value                │
                    └─────────────────────────┘
```

### 2.3 Core Decoder Functions (Pseudo-Design)

```rust
/// The shared low-level decoder that both paths use
fn decode_type(
    cursor: &mut Cursor<&[u8]>,
    idl_type: &IdlType,
    type_registry: &TypeRegistry,
) -> Result<serde_json::Value, DecodeError>

/// Decode a list of named fields (used by both paths)
fn decode_fields(
    cursor: &mut Cursor<&[u8]>,
    fields: &[(String, IdlType)],
    type_registry: &TypeRegistry,
) -> Result<serde_json::Map<String, serde_json::Value>, DecodeError>

/// Path 1: Instruction decode entry point
fn decode_instruction_data(
    program_id: &str,
    data: &[u8],
    registry: &IdlRegistry,
) -> Result<DecodedInstruction, DecodeError> {
    // 1. Extract discriminator (first 8 bytes)
    // 2. Look up instruction by discriminator in registry
    // 3. Build field list from instruction.args
    // 4. Call decode_fields() on remaining bytes
}

/// Path 2: Account decode entry point
fn decode_account_data(
    program_id: &str,
    data: &[u8],
    registry: &IdlRegistry,
) -> Result<DecodedAccount, DecodeError> {
    // 1. Extract discriminator (first 8 bytes)
    // 2. Look up account name by discriminator in registry
    // 3. Find type definition in types array by name
    // 4. Build field list from type definition
    // 5. Call decode_fields() on remaining bytes
}
```

### 2.4 Discriminator Handling

The discriminator system differs between instruction and account paths:

```rust
use sha2::{Sha256, Digest};

/// Compute instruction discriminator
/// Formula: SHA-256("global:<snake_case_name>")[0..8]
fn instruction_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("global:{}", to_snake_case(name));
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

/// Compute account discriminator
/// Formula: SHA-256("account:<PascalCaseName>")[0..8]
fn account_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("account:{}", name); // PascalCase as-is
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}

/// Event discriminator (for future use)
/// Formula: SHA-256("event:<PascalCaseName>")[0..8]
fn event_discriminator(name: &str) -> [u8; 8] {
    let preimage = format!("event:{}", name);
    let hash = Sha256::digest(preimage.as_bytes());
    hash[..8].try_into().unwrap()
}
```

**Important (v0.30+ IDLs):** Discriminators are pre-computed and stored directly in the IDL JSON as `"discriminator": [124, 34, ...]`. Use the stored value when available; only compute when missing (legacy IDLs).

**Important (v0.31+):** Discriminator length is no longer fixed at 8 bytes. Custom discriminators of arbitrary length are supported. The decoder must handle variable-length discriminators.

### 2.5 Error Handling for Decode Paths

```rust
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Unknown discriminator {discriminator:?} for program {program_id}")]
    UnknownDiscriminator {
        program_id: String,
        discriminator: Vec<u8>,
    },

    #[error("No IDL loaded for program {program_id}")]
    NoIdlLoaded { program_id: String },

    #[error("Insufficient data: expected at least {expected} bytes, got {actual}")]
    InsufficientData { expected: usize, actual: usize },

    #[error("Type '{type_name}' referenced in IDL but not found in type registry")]
    UndefinedType { type_name: String },

    #[error("Borsh decode error at byte offset {offset}: {source}")]
    BorshDecode {
        offset: usize,
        #[source]
        source: std::io::Error,
    },

    #[error("Trailing bytes: {count} bytes remain after decoding")]
    TrailingBytes { count: usize },

    #[error("Invalid UTF-8 string at offset {offset}")]
    InvalidUtf8 { offset: usize },

    #[error("Enum variant index {index} out of range (max {max})")]
    InvalidEnumVariant { index: u8, max: usize },
}
```

### 2.6 CPI (Inner Instructions) Handling

Inner instructions (CPI calls) are available in `transaction.meta.innerInstructions`. Each `InnerInstructions` entry contains:

- `index: u8` — which top-level instruction triggered the CPI
- `instructions: Vec<InnerInstruction>` — the CPI instructions with `stack_height`

**Design decision:** Decode inner instructions that match the target program ID. They use the same instruction decode path. The decoded output should include a `depth` or `cpi_depth` field to distinguish top-level from inner instructions:

```rust
pub struct DecodedInstruction {
    pub name: String,
    pub args: serde_json::Map<String, serde_json::Value>,
    pub accounts: Vec<AccountMeta>,
    pub cpi_depth: u8,          // 0 = top-level, 1+ = CPI depth
    pub parent_index: Option<u8>, // index of parent instruction (for CPI)
}
```

This is important because a single transaction may contain both a top-level instruction to Program A and CPI calls from Program A to itself or other programs. The indexer should capture all of these.

---

## 3. Decoder Abstraction Trait Design

### 3.1 Core Trait

```rust
/// The primary decoder abstraction. Both chainparser-fork and custom decoder
/// implement this trait, enabling a clean swap if the fork approach fails.
pub trait SolarixDecoder: Send + Sync {
    /// Load an IDL for a program. Can be called multiple times for different programs.
    fn load_idl(&mut self, program_id: &str, idl_json: &str) -> Result<(), DecodeError>;

    /// Decode instruction data bytes into structured output.
    /// Returns Err(UnknownDiscriminator) if discriminator doesn't match any known instruction.
    fn decode_instruction(
        &self,
        program_id: &str,
        data: &[u8],
    ) -> Result<DecodedInstruction, DecodeError>;

    /// Decode account data bytes into structured output.
    /// Returns Err(UnknownDiscriminator) if discriminator doesn't match any known account.
    fn decode_account(
        &self,
        program_id: &str,
        data: &[u8],
    ) -> Result<DecodedAccount, DecodeError>;

    /// List all known instruction types for a program (for schema generation).
    fn known_instructions(&self, program_id: &str) -> Vec<InstructionInfo>;

    /// List all known account types for a program (for schema generation).
    fn known_accounts(&self, program_id: &str) -> Vec<AccountInfo>;

    /// Check if an IDL is loaded for a given program.
    fn has_idl(&self, program_id: &str) -> bool;
}
```

### 3.2 Output Types

```rust
/// Decoded instruction output
pub struct DecodedInstruction {
    /// Instruction name from IDL (e.g., "transfer", "initialize")
    pub name: String,
    /// Decoded arguments as JSON key-value pairs
    pub args: serde_json::Value, // Always a JSON object
    /// Discriminator bytes that were matched
    pub discriminator: Vec<u8>,
}

/// Decoded account output
pub struct DecodedAccount {
    /// Account type name from IDL (e.g., "Counter", "TokenAccount")
    pub account_type: String,
    /// Decoded fields as JSON key-value pairs
    pub data: serde_json::Value, // Always a JSON object
    /// Discriminator bytes that were matched
    pub discriminator: Vec<u8>,
}

/// Metadata about an instruction type (for schema generation and discovery)
pub struct InstructionInfo {
    pub name: String,
    pub discriminator: Vec<u8>,
    pub args: Vec<FieldInfo>,
}

/// Metadata about an account type (for schema generation and discovery)
pub struct AccountInfo {
    pub name: String,
    pub discriminator: Vec<u8>,
    pub fields: Vec<FieldInfo>,
}

/// Field metadata (shared between instructions and accounts)
pub struct FieldInfo {
    pub name: String,
    pub idl_type: String, // Human-readable type representation
}
```

### 3.3 Why This Trait Shape

1. **`load_idl` is `&mut self`:** IDL loading mutates internal state (discriminator maps, type registry). Runtime loading supports the "universal" requirement.

2. **`decode_*` methods are `&self`:** Decoding is read-only against the loaded state. Multiple threads can decode concurrently after IDL loading is complete (wrap in `RwLock` or load at startup).

3. **`Send + Sync` bound:** The decoder must be shareable across async tasks (tokio workers).

4. **`serde_json::Value` output:** Universal JSON representation avoids type-specific output structs. Aligns with the dynamic/universal nature — we cannot know field types at compile time.

5. **Separate `known_*` methods:** The schema generator needs to introspect the IDL to create database tables before any decoding happens.

### 3.4 IDL Registry (Internal State)

```rust
/// Internal state backing the SolarixDecoder trait
struct IdlRegistry {
    /// program_id → ProgramIdl
    programs: HashMap<String, ProgramIdl>,
}

struct ProgramIdl {
    /// discriminator bytes → instruction definition
    instruction_map: HashMap<Vec<u8>, IdlInstruction>,
    /// discriminator bytes → account name
    account_map: HashMap<Vec<u8>, String>,
    /// type name → type definition (structs, enums)
    type_registry: HashMap<String, IdlTypeDef>,
    /// Original IDL metadata
    metadata: IdlMetadata,
}
```

---

## 4. Transaction Processing Pipeline

### 4.1 Full Pipeline

```
getBlock(slot, { transactionDetails: "full", encoding: "json" })
  │
  ▼
For each transaction in block.transactions:
  │
  ├─ Extract structural data (no IDL needed):
  │   - slot, signature, block_time
  │   - fee, compute_units_consumed
  │   - success/failure status
  │   - all account keys (including lookup table resolved)
  │
  ├─ For each instruction in transaction.message.instructions:
  │   │
  │   ├─ Check: does programId match our target program?
  │   │   ├─ No → skip
  │   │   └─ Yes → continue
  │   │
  │   ├─ Extract instruction data (base58 or base64 → bytes)
  │   ├─ Extract account keys referenced by this instruction
  │   ├─ decoder.decode_instruction(program_id, data) →
  │   │   ├─ Ok(decoded) → build instruction record
  │   │   └─ Err(UnknownDiscriminator) → log warning, store raw
  │   │
  │   └─ Build record:
  │       (slot, signature, block_time, ix_index, ix_name,
  │        decoded_args_json, accounts, cpi_depth=0)
  │
  ├─ For each inner_instruction in transaction.meta.innerInstructions:
  │   │
  │   ├─ Filter: inner instructions where programId matches target
  │   ├─ Same decode flow as top-level but with cpi_depth > 0
  │   └─ Build record with parent_ix_index reference
  │
  └─ Batch insert all instruction records for this transaction

After all transactions in block:
  │
  ├─ Update checkpoint: last_processed_slot = current slot
  └─ Commit database transaction
```

### 4.2 Transaction Data Extraction (Rust)

The RPC response for `getBlock` with `encoding: "json"` provides:

```
transaction.message.accountKeys[]     → all account pubkeys
transaction.message.instructions[]    → top-level instructions
  .programIdIndex                     → index into accountKeys
  .accounts                           → indices into accountKeys
  .data                               → base58-encoded instruction data
transaction.meta.innerInstructions[]  → CPI instructions
  .index                              → parent instruction index
  .instructions[]                     → inner instruction objects
transaction.meta.err                  → null if success
transaction.meta.fee                  → lamports
transaction.meta.computeUnitsConsumed → CU used
transaction.meta.preBalances[]        → SOL balances before
transaction.meta.postBalances[]       → SOL balances after
```

### 4.3 Instruction Record Schema

```sql
CREATE TABLE {program}_instructions (
    id                BIGSERIAL PRIMARY KEY,
    signature         TEXT NOT NULL,
    slot              BIGINT NOT NULL,
    block_time        TIMESTAMPTZ,
    instruction_name  TEXT NOT NULL,
    instruction_index SMALLINT NOT NULL,
    cpi_depth         SMALLINT NOT NULL DEFAULT 0,
    parent_ix_index   SMALLINT,
    args              JSONB NOT NULL,          -- decoded args
    accounts          JSONB NOT NULL,          -- account keys for this ix
    is_successful     BOOLEAN NOT NULL,
    compute_units     INTEGER,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indices for common query patterns
CREATE INDEX idx_{program}_ix_name ON {program}_instructions(instruction_name);
CREATE INDEX idx_{program}_ix_slot ON {program}_instructions(slot);
CREATE INDEX idx_{program}_ix_time ON {program}_instructions(block_time);
CREATE INDEX idx_{program}_ix_sig  ON {program}_instructions(signature);
```

---

## 5. Account State Processing Pipeline

### 5.1 Full Pipeline

```
getProgramAccounts(program_id, { encoding: "base64" })
  OR
getAccountInfo(pubkey, { encoding: "base64" })
  │
  ▼
For each account:
  │
  ├─ Extract:
  │   - pubkey (account address)
  │   - lamports (SOL balance)
  │   - owner (should match program_id)
  │   - data (base64 → bytes)
  │   - rentEpoch
  │
  ├─ decoder.decode_account(program_id, data) →
  │   ├─ Ok(decoded) → build account record
  │   └─ Err(UnknownDiscriminator) → log warning, store raw
  │
  └─ Upsert record:
      (pubkey, account_type, decoded_data_json, slot, lamports, updated_at)

Checkpoint: store snapshot slot for incremental updates
```

### 5.2 Account Snapshot Strategies

**Full snapshot (initial load):**

- Use `getProgramAccounts` to fetch ALL accounts for the program
- Expensive RPC call; often rate-limited or slow
- Use `dataSlice` and `filters` to reduce payload when possible
- Helius offers `changedSinceSlot` for incremental updates (vendor-specific)

**Incremental updates (real-time):**

- Subscribe to `accountSubscribe` WebSocket notifications
- Or periodically re-fetch changed accounts via `getProgramAccounts` with `memcmp` filters
- Update only changed records (upsert pattern)

**On-demand (from transaction processing):**

- When processing a transaction, extract account keys
- Fetch current account state for accounts modified by the instruction
- Decode and upsert

### 5.3 Account Record Schema

```sql
CREATE TABLE {program}_accounts (
    pubkey            TEXT PRIMARY KEY,
    account_type      TEXT NOT NULL,
    data              JSONB NOT NULL,          -- decoded fields
    lamports          BIGINT NOT NULL,
    slot              BIGINT NOT NULL,         -- slot when last observed
    is_closed         BOOLEAN NOT NULL DEFAULT FALSE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indices for common query patterns
CREATE INDEX idx_{program}_acct_type ON {program}_accounts(account_type);
CREATE INDEX idx_{program}_acct_slot ON {program}_accounts(slot);
```

---

## 6. Testing Strategy Overview

The testing strategy is structured in four layers, from fastest/most isolated to slowest/most integrated:

```
Layer 1: Unit Tests (fastest, most numerous)
  ├─ Discriminator computation
  ├─ Individual type decoding (all 26+ IdlType variants)
  ├─ Field decoding sequences
  ├─ Error cases (truncated data, unknown discriminators, etc.)
  ├─ IDL parsing and registry construction
  └─ Schema generation (IDL → SQL DDL)

Layer 2: Property-Based Tests (proptest)
  ├─ Roundtrip: generate value → Borsh encode → decode → assert equal
  ├─ All primitive types
  ├─ Composite types (Option, Vec, structs, enums)
  └─ Fuzz: random bytes → decoder should not panic (graceful errors)

Layer 3: Integration Tests
  ├─ Real IDL fixtures (Jupiter, Marinade, etc.)
  ├─ Real account data fixtures (base64 from mainnet)
  ├─ Full transaction processing pipeline
  ├─ Database write/read roundtrip
  └─ LiteSVM-based program deployment + indexing

Layer 4: API Tests (axum-test)
  ├─ Endpoint tests (filter, aggregate, paginate)
  ├─ Program registration flow
  ├─ Error responses (invalid program, bad queries)
  └─ Multi-param filter combinations
```

**Target coverage:** Aim for >80% line coverage on core decoder and pipeline modules. Use `cargo-llvm-cov` for measurement.

---

## 7. Unit Test Plan (Per Component)

### 7.1 Discriminator Tests

```rust
#[cfg(test)]
mod discriminator_tests {
    #[test]
    fn test_instruction_discriminator_transfer() {
        // "global:transfer" → SHA-256 → first 8 bytes
        let disc = instruction_discriminator("transfer");
        assert_eq!(disc, [/* known bytes */]);
    }

    #[test]
    fn test_account_discriminator_counter() {
        // "account:Counter" → SHA-256 → first 8 bytes
        let disc = account_discriminator("Counter");
        assert_eq!(disc, [/* known bytes */]);
    }

    #[test]
    fn test_discriminator_from_idl_v030() {
        // v0.30+ IDLs include pre-computed discriminators
        // Verify our computation matches the IDL value
        let idl_disc = vec![124, 34, 56, 78, 90, 12, 34, 56];
        let computed = instruction_discriminator("initialize");
        assert_eq!(computed.to_vec(), idl_disc);
    }

    #[test]
    fn test_event_discriminator() {
        let disc = event_discriminator("TransferEvent");
        // "event:TransferEvent" → SHA-256 → first 8 bytes
        assert_eq!(disc.len(), 8);
    }
}
```

### 7.2 Type Decoding Tests (All Variants)

Each primitive and composite type needs its own test. The full list of types to test:

| Type Category    | Types                           | Test Count                                              |
| ---------------- | ------------------------------- | ------------------------------------------------------- |
| Boolean          | `bool`                          | 2 (true, false)                                         |
| Unsigned ints    | `u8, u16, u32, u64, u128, u256` | 6 x 3 (zero, mid, max)                                  |
| Signed ints      | `i8, i16, i32, i64, i128, i256` | 6 x 4 (min, neg, zero, max)                             |
| Floats           | `f32, f64`                      | 2 x 3 (zero, normal, edge)                              |
| String           | `String`                        | 3 (empty, ascii, unicode)                               |
| Bytes            | `bytes`                         | 2 (empty, non-empty)                                    |
| Pubkey           | `pubkey`                        | 2 (zero, valid)                                         |
| Option           | `Option<T>`                     | 2 x N (None, Some for each inner type)                  |
| COption          | `COption<T>`                    | 2 x N (None, Some — note: 4-byte tag, always allocates) |
| Vec              | `Vec<T>`                        | 3 (empty, single, multiple)                             |
| Array            | `[T; N]`                        | 2 (small, larger)                                       |
| HashMap/BTreeMap | `HashMap<K,V>`                  | 3 (empty, single, multiple)                             |
| HashSet/BTreeSet | `HashSet<T>`                    | 3 (empty, single, multiple)                             |
| Tuple            | `(T1, T2, ...)`                 | 2 (pair, triple)                                        |
| Struct (Defined) | named struct                    | 3 (simple, nested, with optional fields)                |
| Enum (Defined)   | enum variants                   | 4 (unit, tuple, struct, nested)                         |

**Total: ~80-100 individual type tests**

Example test pattern:

```rust
#[test]
fn test_decode_u64() {
    let value: u64 = 1_000_000;
    let bytes = value.to_le_bytes();
    let result = decode_type(
        &mut Cursor::new(&bytes),
        &IdlType::U64,
        &TypeRegistry::empty(),
    ).unwrap();
    assert_eq!(result, json!(1_000_000));
}

#[test]
fn test_decode_option_some_string() {
    let mut bytes = vec![1u8]; // Some tag
    let s = "hello";
    bytes.extend_from_slice(&(s.len() as u32).to_le_bytes()); // string length
    bytes.extend_from_slice(s.as_bytes());

    let result = decode_type(
        &mut Cursor::new(&bytes),
        &IdlType::Option(Box::new(IdlType::String)),
        &TypeRegistry::empty(),
    ).unwrap();
    assert_eq!(result, json!("hello"));
}

#[test]
fn test_decode_option_none() {
    let bytes = vec![0u8]; // None tag
    let result = decode_type(
        &mut Cursor::new(&bytes),
        &IdlType::Option(Box::new(IdlType::U64)),
        &TypeRegistry::empty(),
    ).unwrap();
    assert_eq!(result, json!(null));
}
```

### 7.3 Instruction Decode Tests

```rust
#[test]
fn test_decode_simple_instruction() {
    // Build a mock IDL with one instruction "transfer" having args [amount: u64, recipient: pubkey]
    let mut decoder = create_test_decoder();
    decoder.load_idl("prog1", TRANSFER_IDL_JSON).unwrap();

    // Build instruction data: discriminator + borsh-encoded args
    let mut data = Vec::new();
    data.extend_from_slice(&instruction_discriminator("transfer"));
    data.extend_from_slice(&1000u64.to_le_bytes()); // amount
    data.extend_from_slice(&[0u8; 32]);             // recipient pubkey

    let result = decoder.decode_instruction("prog1", &data).unwrap();
    assert_eq!(result.name, "transfer");
    assert_eq!(result.args["amount"], json!(1000));
}

#[test]
fn test_decode_instruction_unknown_discriminator() {
    let mut decoder = create_test_decoder();
    decoder.load_idl("prog1", SIMPLE_IDL_JSON).unwrap();

    let data = vec![0xFF; 16]; // garbage discriminator + some data
    let result = decoder.decode_instruction("prog1", &data);
    assert!(matches!(result, Err(DecodeError::UnknownDiscriminator { .. })));
}

#[test]
fn test_decode_instruction_no_idl_loaded() {
    let decoder = create_test_decoder();
    let data = vec![0u8; 16];
    let result = decoder.decode_instruction("unknown_program", &data);
    assert!(matches!(result, Err(DecodeError::NoIdlLoaded { .. })));
}

#[test]
fn test_decode_instruction_truncated_data() {
    let mut decoder = create_test_decoder();
    decoder.load_idl("prog1", TRANSFER_IDL_JSON).unwrap();

    // Only discriminator, no args
    let data = instruction_discriminator("transfer").to_vec();
    let result = decoder.decode_instruction("prog1", &data);
    assert!(matches!(result, Err(DecodeError::InsufficientData { .. })
                           | Err(DecodeError::BorshDecode { .. })));
}
```

### 7.4 Account Decode Tests

```rust
#[test]
fn test_decode_simple_account() {
    let mut decoder = create_test_decoder();
    decoder.load_idl("prog1", COUNTER_IDL_JSON).unwrap();

    let mut data = Vec::new();
    data.extend_from_slice(&account_discriminator("Counter"));
    data.extend_from_slice(&42u64.to_le_bytes());   // count field
    data.extend_from_slice(&[1u8; 32]);              // authority pubkey

    let result = decoder.decode_account("prog1", &data).unwrap();
    assert_eq!(result.account_type, "Counter");
    assert_eq!(result.data["count"], json!(42));
}
```

### 7.5 IDL Parsing Tests

```rust
#[test]
fn test_load_v030_idl() {
    let mut decoder = create_test_decoder();
    let result = decoder.load_idl("prog1", V030_IDL_JSON);
    assert!(result.is_ok());
    assert_eq!(decoder.known_instructions("prog1").len(), 3);
    assert_eq!(decoder.known_accounts("prog1").len(), 2);
}

#[test]
fn test_load_idl_with_complex_types() {
    // IDL with nested structs, enums, optional fields
    let mut decoder = create_test_decoder();
    decoder.load_idl("prog1", COMPLEX_IDL_JSON).unwrap();
    let accounts = decoder.known_accounts("prog1");
    assert!(accounts.iter().any(|a| a.name == "GameState"));
}

#[test]
fn test_load_invalid_idl_json() {
    let mut decoder = create_test_decoder();
    let result = decoder.load_idl("prog1", "not valid json");
    assert!(result.is_err());
}
```

### 7.6 Schema Generation Tests

```rust
#[test]
fn test_idl_to_ddl_simple() {
    let ddl = generate_ddl("counter", &COUNTER_IDL);
    assert!(ddl.contains("CREATE TABLE counter_instructions"));
    assert!(ddl.contains("CREATE TABLE counter_accounts"));
    assert!(ddl.contains("instruction_name TEXT"));
    assert!(ddl.contains("args JSONB"));
}

#[test]
fn test_idl_to_ddl_indices() {
    let ddl = generate_ddl("counter", &COUNTER_IDL);
    assert!(ddl.contains("CREATE INDEX"));
    assert!(ddl.contains("instruction_name"));
    assert!(ddl.contains("slot"));
}
```

---

## 8. Property-Based Testing with proptest

### 8.1 Why proptest for Borsh Decoding

Traditional unit tests cover known cases. Property-based testing proves that the decoder handles ANY valid Borsh-encoded data correctly. This is particularly important for a "universal" indexer that must handle arbitrary IDL types from unknown programs.

Key properties to test:

1. **Roundtrip:** `encode(value) → decode(encoded) == value` for all supported types
2. **No panics:** Arbitrary bytes should produce `Err`, never panic
3. **Deterministic:** Same input always produces same output

### 8.2 Primitive Type Roundtrips

```rust
use proptest::prelude::*;
use borsh::BorshSerialize;

proptest! {
    #[test]
    fn roundtrip_bool(val: bool) {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::Bool, &empty_registry()).unwrap();
        prop_assert_eq!(decoded, json!(val));
    }

    #[test]
    fn roundtrip_u8(val: u8) {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::U8, &empty_registry()).unwrap();
        prop_assert_eq!(decoded, json!(val));
    }

    #[test]
    fn roundtrip_u16(val: u16) {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::U16, &empty_registry()).unwrap();
        prop_assert_eq!(decoded, json!(val));
    }

    #[test]
    fn roundtrip_u32(val: u32) {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::U32, &empty_registry()).unwrap();
        prop_assert_eq!(decoded, json!(val));
    }

    #[test]
    fn roundtrip_u64(val: u64) {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::U64, &empty_registry()).unwrap();
        // Note: u64 may be encoded as string in JSON for precision
        let expected = if val > (1u64 << 53) {
            json!(val.to_string())
        } else {
            json!(val)
        };
        prop_assert_eq!(decoded, expected);
    }

    #[test]
    fn roundtrip_i64(val: i64) {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::I64, &empty_registry()).unwrap();
        prop_assert!(decoded.is_number() || decoded.is_string());
    }

    #[test]
    fn roundtrip_string(val in "[a-zA-Z0-9 ]{0,100}") {
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &IdlType::String, &empty_registry()).unwrap();
        prop_assert_eq!(decoded, json!(val));
    }
}
```

### 8.3 Composite Type Roundtrips

```rust
proptest! {
    #[test]
    fn roundtrip_option_u64(val: Option<u64>) {
        let bytes = borsh::to_vec(&val).unwrap();
        let idl_type = IdlType::Option(Box::new(IdlType::U64));
        let decoded = decode_type(&mut Cursor::new(&bytes), &idl_type, &empty_registry()).unwrap();
        match val {
            None => prop_assert_eq!(decoded, json!(null)),
            Some(v) => prop_assert!(decoded.as_u64().is_some() || decoded.as_str().is_some()),
        }
    }

    #[test]
    fn roundtrip_vec_u32(val: Vec<u32>) {
        prop_assume!(val.len() < 1000); // bound size for test performance
        let bytes = borsh::to_vec(&val).unwrap();
        let idl_type = IdlType::Vec(Box::new(IdlType::U32));
        let decoded = decode_type(&mut Cursor::new(&bytes), &idl_type, &empty_registry()).unwrap();
        let arr = decoded.as_array().unwrap();
        prop_assert_eq!(arr.len(), val.len());
    }

    #[test]
    fn roundtrip_fixed_array_u8(val in prop::array::uniform32(any::<u8>())) {
        let bytes = borsh::to_vec(&val).unwrap();
        let idl_type = IdlType::Array(Box::new(IdlType::U8), 32);
        let decoded = decode_type(&mut Cursor::new(&bytes), &idl_type, &empty_registry()).unwrap();
        let arr = decoded.as_array().unwrap();
        prop_assert_eq!(arr.len(), 32);
    }
}
```

### 8.4 Complex Struct Roundtrips

Testing struct decoding requires building both a Borsh-serializable Rust struct and a matching IDL type definition:

```rust
#[derive(Debug, BorshSerialize, PartialEq)]
struct TestStruct {
    count: u64,
    name: String,
    active: bool,
}

// Build matching IDL type definition
fn test_struct_idl_type() -> IdlTypeDef {
    IdlTypeDef {
        name: "TestStruct".to_string(),
        body: IdlTypeDefBody::Struct {
            fields: vec![
                ("count".to_string(), IdlType::U64),
                ("name".to_string(), IdlType::String),
                ("active".to_string(), IdlType::Bool),
            ],
        },
    }
}

proptest! {
    #[test]
    fn roundtrip_struct(
        count: u64,
        name in "[a-z]{0,50}",
        active: bool,
    ) {
        let val = TestStruct { count, name: name.clone(), active };
        let bytes = borsh::to_vec(&val).unwrap();

        let mut registry = TypeRegistry::new();
        registry.register(test_struct_idl_type());

        let idl_type = IdlType::Defined { name: "TestStruct".to_string(), generics: vec![] };
        let decoded = decode_type(&mut Cursor::new(&bytes), &idl_type, &registry).unwrap();

        prop_assert_eq!(decoded["count"], json!(count));
        prop_assert_eq!(decoded["name"], json!(name));
        prop_assert_eq!(decoded["active"], json!(active));
    }
}
```

### 8.5 Enum Roundtrips

```rust
#[derive(Debug, BorshSerialize)]
enum TestEnum {
    UnitVariant,
    TupleVariant(u32, String),
    StructVariant { x: i64, y: i64 },
}

fn test_enum_idl_type() -> IdlTypeDef {
    IdlTypeDef {
        name: "TestEnum".to_string(),
        body: IdlTypeDefBody::Enum {
            variants: vec![
                IdlEnumVariant { name: "UnitVariant".into(), fields: None },
                IdlEnumVariant {
                    name: "TupleVariant".into(),
                    fields: Some(IdlEnumFields::Tuple(vec![IdlType::U32, IdlType::String])),
                },
                IdlEnumVariant {
                    name: "StructVariant".into(),
                    fields: Some(IdlEnumFields::Named(vec![
                        ("x".into(), IdlType::I64),
                        ("y".into(), IdlType::I64),
                    ])),
                },
            ],
        },
    }
}

proptest! {
    #[test]
    fn roundtrip_enum_unit_variant(_dummy: bool) {
        let val = TestEnum::UnitVariant;
        let bytes = borsh::to_vec(&val).unwrap();
        // bytes should be [0x00] — variant index 0
        let decoded = decode_type(&mut Cursor::new(&bytes), &enum_idl_type(), &registry()).unwrap();
        prop_assert_eq!(decoded["variant"], json!("UnitVariant"));
    }

    #[test]
    fn roundtrip_enum_tuple_variant(x: u32, s in "[a-z]{0,20}") {
        let val = TestEnum::TupleVariant(x, s.clone());
        let bytes = borsh::to_vec(&val).unwrap();
        let decoded = decode_type(&mut Cursor::new(&bytes), &enum_idl_type(), &registry()).unwrap();
        prop_assert_eq!(decoded["variant"], json!("TupleVariant"));
    }
}
```

### 8.6 Fuzz Testing (No Panics on Random Input)

```rust
proptest! {
    #[test]
    fn fuzz_no_panic_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        // The decoder should never panic, only return errors
        for idl_type in all_primitive_types() {
            let _ = decode_type(&mut Cursor::new(&bytes), &idl_type, &empty_registry());
            // We don't assert Ok — just assert no panic
        }
    }

    #[test]
    fn fuzz_instruction_decode_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let decoder = create_loaded_decoder();
        let _ = decoder.decode_instruction("prog1", &bytes);
        // Must not panic
    }

    #[test]
    fn fuzz_account_decode_no_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let decoder = create_loaded_decoder();
        let _ = decoder.decode_account("prog1", &bytes);
        // Must not panic
    }
}
```

### 8.7 proptest Dependencies

```toml
[dev-dependencies]
proptest = "1.6"
# proptest-derive is optional but useful for automatic Arbitrary on simple structs
proptest-derive = "0.5"
borsh = { version = "1.6", features = ["derive"] }
```

---

## 9. Integration Test Design

### 9.1 Recommended Framework: LiteSVM

Based on research, **LiteSVM** is the recommended choice for Solarix integration tests:

| Framework               | Speed                  | Ecosystem Status           | Our Use Case Fit                                |
| ----------------------- | ---------------------- | -------------------------- | ----------------------------------------------- |
| `solana-test-validator` | Slow (process startup) | Official but heavyweight   | Overkill — we test decoding, not on-chain logic |
| `solana-program-test`   | Fast                   | Deprecated since v3.1.0    | Risky for new projects                          |
| **LiteSVM**             | Fastest                | Recommended by Solana docs | Best fit — in-process, ergonomic                |

LiteSVM advantages for Solarix:

- In-process: no external validator to start/stop
- Can load compiled `.so` programs from files
- Can set arbitrary account data (perfect for testing account decoding)
- Can send transactions and get results (perfect for testing instruction decoding)
- Supported by Anchor via `anchor-litesvm`

### 9.2 LiteSVM Integration Test Pattern

```rust
use litesvm::LiteSVM;
use solana_sdk::{
    signature::{Keypair, Signer},
    transaction::Transaction,
    instruction::{Instruction, AccountMeta},
};

#[test]
fn test_index_instruction_from_transaction() {
    // 1. Set up LiteSVM with a test program
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, "tests/fixtures/programs/counter.so");

    // 2. Create and send a transaction
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();

    let ix_data = build_initialize_ix_data(); // discriminator + args
    let ix = Instruction::new_with_bytes(
        program_id,
        &ix_data,
        vec![AccountMeta::new(payer.pubkey(), true)],
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let result = svm.send_transaction(tx).unwrap();

    // 3. Now test our decoder on the transaction data
    let mut decoder = SolarixDecoder::new();
    decoder.load_idl(&program_id.to_string(), COUNTER_IDL_JSON).unwrap();

    let decoded = decoder.decode_instruction(
        &program_id.to_string(),
        &ix_data,
    ).unwrap();
    assert_eq!(decoded.name, "initialize");

    // 4. Test account state after transaction
    let account = svm.get_account(&counter_pubkey).unwrap();
    let decoded_account = decoder.decode_account(
        &program_id.to_string(),
        &account.data,
    ).unwrap();
    assert_eq!(decoded_account.account_type, "Counter");
    assert_eq!(decoded_account.data["count"], json!(0));
}
```

### 9.3 Database Integration Tests

For testing the full pipeline including PostgreSQL writes:

```rust
use sqlx::PgPool;
use testcontainers::{clients, images::postgres::Postgres};

#[tokio::test]
async fn test_full_pipeline_instruction_to_db() {
    // 1. Start PostgreSQL via testcontainers (or use a test database)
    let docker = clients::Cli::default();
    let pg_container = docker.run(Postgres::default());
    let db_url = format!(
        "postgres://postgres:postgres@localhost:{}/postgres",
        pg_container.get_host_port_ipv4(5432)
    );
    let pool = PgPool::connect(&db_url).await.unwrap();

    // 2. Load IDL and generate schema
    let idl = load_test_idl("counter");
    generate_and_apply_schema(&pool, "counter", &idl).await.unwrap();

    // 3. Process a test transaction
    let decoded_ix = DecodedInstruction {
        name: "initialize".to_string(),
        args: json!({"count": 0}),
        discriminator: vec![/* ... */],
    };

    // 4. Write to database
    insert_instruction(&pool, "counter", &decoded_ix, &tx_meta).await.unwrap();

    // 5. Verify via query
    let row = sqlx::query("SELECT instruction_name, args FROM counter_instructions LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("instruction_name"), "initialize");
}
```

### 9.4 Real IDL Fixture Tests

Test against real-world IDLs to verify the decoder handles production complexity:

```rust
#[test]
fn test_decode_with_jupiter_idl() {
    let mut decoder = create_decoder();
    decoder.load_idl(JUPITER_PROGRAM_ID, include_str!("fixtures/idls/jupiter.json")).unwrap();

    // Verify IDL loaded correctly
    let instructions = decoder.known_instructions(JUPITER_PROGRAM_ID);
    assert!(instructions.iter().any(|i| i.name == "route"));
    assert!(instructions.iter().any(|i| i.name == "sharedAccountsRoute"));

    let accounts = decoder.known_accounts(JUPITER_PROGRAM_ID);
    assert!(!accounts.is_empty());
}

#[test]
fn test_decode_real_account_data() {
    // Account data captured from mainnet (base64 → bytes)
    let account_bytes = base64::decode(include_str!("fixtures/accounts/counter_account.b64")).unwrap();

    let mut decoder = create_decoder();
    decoder.load_idl(COUNTER_PROGRAM_ID, include_str!("fixtures/idls/counter.json")).unwrap();

    let decoded = decoder.decode_account(COUNTER_PROGRAM_ID, &account_bytes).unwrap();
    assert_eq!(decoded.account_type, "Counter");
    assert!(decoded.data["count"].is_number());
}
```

---

## 10. API Test Design

### 10.1 Framework: axum-test

The `axum-test` crate (v18.7+) provides `TestServer` for testing axum applications. It supports both mock transport (fastest) and real HTTP transport.

### 10.2 API Test Setup Pattern

```rust
use axum_test::TestServer;

async fn create_test_app() -> TestServer {
    // 1. Set up test database (testcontainers or test DB)
    let pool = setup_test_db().await;

    // 2. Seed with test data
    seed_test_data(&pool).await;

    // 3. Build app router
    let app = build_router(pool);

    // 4. Create test server
    TestServer::new(app).unwrap()
}

async fn seed_test_data(pool: &PgPool) {
    // Insert test IDL, instructions, accounts
    sqlx::query("INSERT INTO programs (program_id, idl, name) VALUES ($1, $2, $3)")
        .bind("CounterProgram111")
        .bind(COUNTER_IDL_JSON)
        .bind("counter")
        .execute(pool)
        .await
        .unwrap();

    // Insert some test instruction records
    for i in 0..50 {
        sqlx::query("INSERT INTO counter_instructions (signature, slot, instruction_name, args, ...) VALUES (...)")
            .execute(pool)
            .await
            .unwrap();
    }
}
```

### 10.3 API Endpoint Tests

```rust
#[tokio::test]
async fn test_list_instructions_with_filters() {
    let server = create_test_app().await;

    let response = server
        .get("/api/v1/programs/counter/instructions")
        .add_query_param("instruction_name", "initialize")
        .add_query_param("slot_from", "100")
        .add_query_param("slot_to", "200")
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["data"].as_array().unwrap().len() > 0);
    assert!(body["data"].as_array().unwrap().iter().all(|item| {
        item["instruction_name"] == "initialize"
    }));
}

#[tokio::test]
async fn test_pagination() {
    let server = create_test_app().await;

    let page1 = server
        .get("/api/v1/programs/counter/instructions")
        .add_query_param("limit", "10")
        .add_query_param("offset", "0")
        .await;

    let page2 = server
        .get("/api/v1/programs/counter/instructions")
        .add_query_param("limit", "10")
        .add_query_param("offset", "10")
        .await;

    page1.assert_status_ok();
    page2.assert_status_ok();

    let body1: serde_json::Value = page1.json();
    let body2: serde_json::Value = page2.json();

    // Pages should have different data
    assert_ne!(body1["data"], body2["data"]);
}

#[tokio::test]
async fn test_aggregation_instruction_count() {
    let server = create_test_app().await;

    let response = server
        .get("/api/v1/programs/counter/stats/instructions")
        .add_query_param("from", "2024-01-01T00:00:00Z")
        .add_query_param("to", "2024-12-31T23:59:59Z")
        .add_query_param("group_by", "instruction_name")
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["data"].as_array().unwrap().iter().any(|item| {
        item["instruction_name"] == "initialize" && item["count"].as_u64().unwrap() > 0
    }));
}

#[tokio::test]
async fn test_register_program() {
    let server = create_test_app().await;

    let response = server
        .post("/api/v1/programs")
        .json(&json!({
            "program_id": "NewProgram111111111111111111111111111111111",
            "idl": serde_json::from_str::<serde_json::Value>(COUNTER_IDL_JSON).unwrap(),
        }))
        .await;

    response.assert_status(201);
}

#[tokio::test]
async fn test_get_account_state() {
    let server = create_test_app().await;

    let response = server
        .get("/api/v1/programs/counter/accounts/SomeAccountPubkey111")
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["account_type"], "Counter");
    assert!(body["data"]["count"].is_number());
}

#[tokio::test]
async fn test_invalid_program_returns_404() {
    let server = create_test_app().await;

    let response = server
        .get("/api/v1/programs/nonexistent/instructions")
        .await;

    response.assert_status(404);
}

#[tokio::test]
async fn test_multi_param_filter() {
    let server = create_test_app().await;

    // Filter by instruction_name AND slot range AND success status
    let response = server
        .get("/api/v1/programs/counter/instructions")
        .add_query_param("instruction_name", "transfer")
        .add_query_param("slot_from", "1000")
        .add_query_param("slot_to", "2000")
        .add_query_param("is_successful", "true")
        .await;

    response.assert_status_ok();
}
```

---

## 11. Fixture Management Strategy

### 11.1 Directory Structure

```
tests/
├── fixtures/
│   ├── idls/
│   │   ├── counter.json          # Simple program (1-2 instructions)
│   │   ├── counter_legacy.json   # v0.29 format for legacy support
│   │   ├── jupiter.json          # Complex real-world IDL
│   │   ├── marinade.json         # Another real-world IDL
│   │   └── all_types.json        # Synthetic IDL with every type variant
│   ├── accounts/
│   │   ├── counter_account.b64   # Base64-encoded real account data
│   │   ├── counter_account.json  # Expected decoded output
│   │   └── README.md             # How these were captured
│   ├── transactions/
│   │   ├── counter_initialize.json  # Full getTransaction response
│   │   ├── counter_increment.json   # Another transaction
│   │   └── cpi_nested.json          # Transaction with CPI/inner instructions
│   └── expected/
│       ├── counter_initialize_decoded.json  # Expected decode output
│       └── counter_account_decoded.json     # Expected account decode output
├── common/
│   └── mod.rs                    # Shared test utilities
├── unit/
│   ├── decoder_test.rs
│   ├── discriminator_test.rs
│   └── schema_test.rs
├── property/
│   └── borsh_roundtrip_test.rs
├── integration/
│   ├── pipeline_test.rs
│   └── litesvm_test.rs
└── api/
    └── api_test.rs
```

### 11.2 Fixture Creation Process

**Real IDLs:**

1. Find deployed Anchor programs on mainnet/devnet
2. Fetch IDL via: `anchor idl fetch <program_id> --provider.cluster mainnet`
3. Or download from the program's GitHub repo
4. Save to `tests/fixtures/idls/`

**Real account data:**

1. Use `solana account <pubkey> --output json` to dump account data
2. Extract the `data` field (base64-encoded)
3. Save raw base64 to `tests/fixtures/accounts/`
4. Decode manually and save expected output to `tests/fixtures/expected/`

**Synthetic "all types" IDL:**
Create a synthetic IDL that exercises every type variant. This ensures decoder completeness:

```json
{
  "address": "AllTypes1111111111111111111111111111111111111",
  "metadata": { "name": "all_types", "version": "0.1.0", "spec": "0.1.0" },
  "instructions": [
    {
      "name": "test_all_primitives",
      "discriminator": [
        /* computed */
      ],
      "args": [
        { "name": "a_bool", "type": "bool" },
        { "name": "a_u8", "type": "u8" },
        { "name": "a_u16", "type": "u16" },
        { "name": "a_u32", "type": "u32" },
        { "name": "a_u64", "type": "u64" },
        { "name": "a_u128", "type": "u128" },
        { "name": "a_i8", "type": "i8" },
        { "name": "a_i64", "type": "i64" },
        { "name": "a_f32", "type": "f32" },
        { "name": "a_f64", "type": "f64" },
        { "name": "a_string", "type": "string" },
        { "name": "a_pubkey", "type": "pubkey" },
        { "name": "a_bytes", "type": "bytes" }
      ],
      "accounts": []
    },
    {
      "name": "test_composites",
      "discriminator": [
        /* computed */
      ],
      "args": [
        { "name": "opt_u64", "type": { "option": "u64" } },
        { "name": "vec_u32", "type": { "vec": "u32" } },
        { "name": "array_u8", "type": { "array": ["u8", 32] } },
        {
          "name": "defined_type",
          "type": { "defined": { "name": "MyStruct" } }
        }
      ],
      "accounts": []
    }
  ],
  "accounts": [
    {
      "name": "AllTypesAccount",
      "discriminator": [
        /* computed */
      ]
    }
  ],
  "types": [
    {
      "name": "MyStruct",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "x", "type": "u64" },
          { "name": "y", "type": "string" }
        ]
      }
    },
    {
      "name": "MyEnum",
      "type": {
        "kind": "enum",
        "variants": [
          { "name": "Unit" },
          { "name": "WithData", "fields": [{ "name": "value", "type": "u64" }] }
        ]
      }
    },
    {
      "name": "AllTypesAccount",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "count", "type": "u64" },
          { "name": "label", "type": "string" },
          { "name": "nested", "type": { "defined": { "name": "MyStruct" } } },
          { "name": "status", "type": { "defined": { "name": "MyEnum" } } }
        ]
      }
    }
  ]
}
```

### 11.3 Test Helper Module

```rust
// tests/common/mod.rs

use std::path::PathBuf;

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

pub fn load_idl(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join("idls").join(format!("{}.json", name))).unwrap()
}

pub fn load_account_data(name: &str) -> Vec<u8> {
    let b64 = std::fs::read_to_string(fixtures_dir().join("accounts").join(format!("{}.b64", name))).unwrap();
    base64::decode(b64.trim()).unwrap()
}

pub fn load_expected(name: &str) -> serde_json::Value {
    let json_str = std::fs::read_to_string(fixtures_dir().join("expected").join(format!("{}.json", name))).unwrap();
    serde_json::from_str(&json_str).unwrap()
}

pub fn create_test_decoder() -> impl SolarixDecoder {
    // Factory that creates the appropriate decoder implementation
    ChainparserDecoder::new(Default::default())
}

pub fn empty_registry() -> TypeRegistry {
    TypeRegistry::new()
}
```

---

## 12. Test Coverage Map (Requirement to Test)

| #   | Bounty Requirement         | Test Type          | Test Location                                   | Description                                                                         |
| --- | -------------------------- | ------------------ | ----------------------------------------------- | ----------------------------------------------------------------------------------- |
| 1   | Dynamic schema generation  | Unit               | `tests/unit/schema_test.rs`                     | IDL → DDL produces valid SQL; all type variants map correctly                       |
| 2   | Account state decoding     | Unit + Property    | `tests/unit/decoder_test.rs`, `tests/property/` | Decode all type variants; roundtrip all primitives and composites                   |
| 3   | Instruction arg decoding   | Unit + Property    | `tests/unit/decoder_test.rs`, `tests/property/` | Decode instruction args; handle unknown discriminators gracefully                   |
| 4   | Batch mode (slot range)    | Integration        | `tests/integration/pipeline_test.rs`            | Process slot range; verify all instructions indexed; checkpoint updated             |
| 5   | Batch mode (signatures)    | Integration        | `tests/integration/pipeline_test.rs`            | Fetch by signature list; decode and store correctly                                 |
| 6   | Real-time mode             | Integration        | `tests/integration/pipeline_test.rs`            | WebSocket subscription indexes new transactions                                     |
| 7   | Cold start / backfill      | Integration        | `tests/integration/pipeline_test.rs`            | Stop indexer, send transactions, restart, verify backfill then real-time            |
| 8   | Exponential backoff        | Unit               | `tests/unit/retry_test.rs`                      | Mock RPC failures; verify delay sequence (1s, 2s, 4s, 8s, ...); verify max retries  |
| 9   | Retry mechanism            | Unit               | `tests/unit/retry_test.rs`                      | Mock transient failures; verify successful retry; verify permanent failure handling |
| 10  | Graceful shutdown          | Integration        | `tests/integration/pipeline_test.rs`            | Send SIGTERM; verify checkpoint saved; no partial writes                            |
| 11  | Multi-param filter         | API                | `tests/api/api_test.rs`                         | Query with 2-3 simultaneous filters; verify result set                              |
| 12  | Aggregation                | API                | `tests/api/api_test.rs`                         | Count instructions by name over time period                                         |
| 13  | Program statistics         | API                | `tests/api/api_test.rs`                         | Total instructions, accounts, unique signers                                        |
| 14  | Docker Compose             | Manual/CI          | `.github/workflows/`                            | `docker compose up` starts everything; smoke test passes                            |
| 15  | Structured logging         | Unit               | `tests/unit/logging_test.rs`                    | Log output is valid JSON; contains required fields                                  |
| 16  | CPI / inner instructions   | Unit + Integration | `tests/unit/decoder_test.rs`                    | Inner instructions decoded with correct depth                                       |
| 17  | IDL loading (v0.30+)       | Unit               | `tests/unit/idl_test.rs`                        | Parse v0.30+ IDL; extract discriminators, types, instructions                       |
| 18  | Error handling (no panics) | Property (fuzz)    | `tests/property/borsh_roundtrip_test.rs`        | Random bytes never cause panics                                                     |

---

## 13. CI/CD Configuration

### 13.1 GitHub Actions Workflow

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  # ──────────────────────────────────────────────
  # Job 1: Lint and Format
  # ──────────────────────────────────────────────
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Check formatting
        run: cargo fmt --all -- --check
      - name: Run clippy
        run: cargo clippy --all-targets --all-features -- -D warnings

  # ──────────────────────────────────────────────
  # Job 2: Unit and Property Tests (no external deps)
  # ──────────────────────────────────────────────
  unit-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run unit tests
        run: cargo test --lib --all-features
      - name: Run property tests
        run: cargo test --test borsh_roundtrip_test --all-features
        env:
          PROPTEST_CASES: 1000 # More cases in CI

  # ──────────────────────────────────────────────
  # Job 3: Integration Tests (PostgreSQL required)
  # ──────────────────────────────────────────────
  integration-tests:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_USER: solarix
          POSTGRES_PASSWORD: solarix
          POSTGRES_DB: solarix_test
        ports:
          - 5432:5432
        options: >-
          --health-cmd pg_isready
          --health-interval 5s
          --health-timeout 5s
          --health-retries 5
          --tmpfs /var/lib/postgresql/data
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run integration tests
        run: cargo test --test '*' --all-features -- --test-threads=1
        env:
          DATABASE_URL: postgres://solarix:solarix@localhost:5432/solarix_test

  # ──────────────────────────────────────────────
  # Job 4: Code Coverage
  # ──────────────────────────────────────────────
  coverage:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_USER: solarix
          POSTGRES_PASSWORD: solarix
          POSTGRES_DB: solarix_test
        ports:
          - 5432:5432
        options: >-
          --health-cmd pg_isready
          --health-interval 5s
          --health-timeout 5s
          --health-retries 5
          --tmpfs /var/lib/postgresql/data
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: llvm-tools-preview
      - uses: taiki-e/install-action@v2
        with:
          tool: cargo-llvm-cov
      - uses: Swatinem/rust-cache@v2
      - name: Generate coverage
        run: cargo llvm-cov --all-features --lcov --output-path lcov.info
        env:
          DATABASE_URL: postgres://solarix:solarix@localhost:5432/solarix_test
      - name: Upload to Codecov
        uses: codecov/codecov-action@v4
        with:
          files: lcov.info
          fail_ci_if_error: false

  # ──────────────────────────────────────────────
  # Job 5: Docker Compose Smoke Test
  # ──────────────────────────────────────────────
  docker-smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build and start services
        run: docker compose up -d --build
      - name: Wait for services
        run: |
          for i in $(seq 1 30); do
            curl -sf http://localhost:8080/health && break
            sleep 2
          done
      - name: Smoke test API
        run: |
          curl -sf http://localhost:8080/health | jq .
          curl -sf http://localhost:8080/api/v1/programs | jq .
      - name: Tear down
        if: always()
        run: docker compose down -v
```

### 13.2 cargo-llvm-cov Details

`cargo-llvm-cov` uses LLVM's source-based instrumentation for precise coverage. Key points:

- Supports line, region, and branch coverage
- Integrates with `cargo test` and `cargo nextest`
- Output formats: LCOV (for Codecov), Cobertura XML (for GitLab), HTML (local viewing)
- Install: `cargo install cargo-llvm-cov` or via GitHub Action `taiki-e/install-action@v2`
- Local usage: `cargo llvm-cov --html --open` opens coverage report in browser

### 13.3 Test Parallelism Notes

- `cargo test` runs tests in parallel by default (one thread per test)
- Integration tests touching the same database must use `--test-threads=1` OR use separate databases per test
- Property tests can run in parallel (proptest handles this)
- API tests: use unique database names per test or wrap in transactions that roll back

### 13.4 Docker Compose for Integration Testing (Local)

```yaml
# docker-compose.test.yml
version: "3.8"
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_USER: solarix
      POSTGRES_PASSWORD: solarix
      POSTGRES_DB: solarix_test
    ports:
      - "5433:5432" # Different port to avoid conflicts
    tmpfs:
      - /var/lib/postgresql/data # RAM-backed for speed
    healthcheck:
      test: ["CMD", "pg_isready", "-U", "solarix"]
      interval: 5s
      timeout: 5s
      retries: 5
```

Run locally:

```bash
docker compose -f docker-compose.test.yml up -d
DATABASE_URL=postgres://solarix:solarix@localhost:5433/solarix_test cargo test
docker compose -f docker-compose.test.yml down -v
```

---

## 14. Sources

### Solana & Anchor

- [Anchor IDL Documentation](https://www.anchor-lang.com/docs/basics/idl)
- [Anchor Discriminator Trait](https://docs.rs/anchor-lang/latest/anchor_lang/trait.Discriminator.html)
- [Anchor v0.31 Release Notes — Custom Discriminators](https://www.anchor-lang.com/docs/updates/release-notes/0-31-0)
- [anchor-decoder crate](https://crates.io/crates/anchor-decoder)
- [Solana getBlock RPC Method](https://solana.com/docs/rpc/http/getblock)
- [Solana getTransaction RPC Method](https://solana.com/docs/rpc/http/gettransaction)
- [Solana getProgramAccounts RPC Method](https://solana.com/docs/rpc/http/getprogramaccounts)
- [Solana JSON Data Structures](https://solana.com/docs/rpc/json-structures)
- [IDLs (Interface Definition Language) — Solana Docs](https://solana.com/developers/guides/advanced/idls)
- [InnerInstructions struct — solana-transaction-status](https://docs.rs/solana-transaction-status/latest/solana_transaction_status/struct.InnerInstructions.html)
- [Helius — Guide to Testing Solana Programs](https://www.helius.dev/blog/a-guide-to-testing-solana-programs)
- [Helius — Faster getProgramAccounts](https://www.helius.dev/blog/faster-getprogramaccounts)

### chainparser

- [chainparser GitHub — thlorenz](https://github.com/thlorenz/chainparser)
- [solana-idl crate — thlorenz](https://github.com/thlorenz/solana-idl)

### LiteSVM

- [LiteSVM Documentation](https://docs.rs/litesvm/latest/litesvm/)
- [LiteSVM GitHub](https://github.com/LiteSVM/litesvm)
- [QuickNode — How to Test Solana Programs with LiteSVM](https://www.quicknode.com/guides/solana-development/tooling/litesvm)
- [Anchor LiteSVM Integration](https://www.anchor-lang.com/docs/testing/litesvm)
- [litesvm-testing crate](https://crates.io/crates/litesvm-testing)

### Borsh

- [Borsh Specification](https://borsh.io/)
- [borsh-rs GitHub](https://github.com/near/borsh-rs)
- [borsh crate](https://crates.io/crates/borsh)

### Testing Frameworks

- [proptest crate docs](https://docs.rs/proptest)
- [Proptest Book — Modifier Reference](https://altsysrq.github.io/proptest-book/proptest-derive/modifiers.html)
- [Property-based testing in Rust with Proptest — LogRocket](https://blog.logrocket.com/property-based-testing-in-rust-with-proptest/)
- [Proptest: Property Testing in Rust — Ivan Yurchenko](https://ivanyu.me/blog/2024/09/22/proptest-property-testing-in-rust/)
- [axum-test crate docs](https://docs.rs/axum-test)
- [axum-test TestServer](https://docs.rs/axum-test/latest/axum_test/struct.TestServer.html)
- [axum-test GitHub](https://github.com/JosephLenton/axum-test)

### Code Coverage & CI

- [cargo-llvm-cov GitHub](https://github.com/taiki-e/cargo-llvm-cov)
- [cargo-llvm-cov docs](https://docs.rs/crate/cargo-llvm-cov/latest)
- [Rust Testing Patterns for Reliable Releases (2026)](https://dasroot.net/posts/2026/03/rust-testing-patterns-reliable-releases/)
- [Rust Project Primer — Test Coverage](https://rustprojectprimer.com/measure/coverage.html)
- [cargo-nextest — Test Coverage Integration](https://nexte.st/docs/integrations/test-coverage/)

### Docker & CI

- [solana-test-validator-docker](https://github.com/tchambard/solana-test-validator-docker)
- [Docker Compose for Integration Testing — Alex Therrien](https://medium.com/@alexandre.therrien3/docker-compose-for-integration-testing-a-practical-guide-for-any-project-49b361a52f8c)

### solana-program-test (Deprecated)

- [solana-program-test crate](https://crates.io/crates/solana-program-test)
- [ProgramTest struct docs](https://docs.rs/solana-program-test/latest/solana_program_test/struct.ProgramTest.html)
- [Solana Developing Programs in Rust](https://solana.com/docs/programs/rust)

### Related Tooling

- [Carbon Framework — Solana Indexing](https://solanacompass.com/learn/accelerate-25/scale-or-die-at-accelerate-2025-indexing-solana-programs-with-carbon)
- [solana_toolbox_idl crate](https://crates.io/crates/solana_toolbox_idl)
- [solana-tx-parser — deBridge](https://github.com/debridge-finance/solana-tx-parser-public)
- [solana-snapshot-gpa — Offline Account Indexing](https://github.com/everlastingsong/solana-snapshot-gpa)
