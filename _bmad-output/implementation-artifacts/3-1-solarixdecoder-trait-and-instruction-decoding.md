# Story 3.1: SolarixDecoder Trait & Instruction Decoding

Status: done

## Story

As a developer,
I want a decoder abstraction that can deserialize Anchor instruction arguments from raw transaction data using an IDL,
so that instruction data is decoded into queryable JSON and the decoder implementation can be swapped without affecting other modules.

## Acceptance Criteria

1. **AC1: SolarixDecoder trait signature**
   - **Given** the `SolarixDecoder` trait in `decoder/mod.rs`
   - **When** I inspect it
   - **Then** it defines `decode_instruction(&self, program_id: &str, data: &[u8], idl: &Idl) -> Result<DecodedInstruction, DecodeError>` and `decode_account(&self, program_id: &str, data: &[u8], idl: &Idl) -> Result<DecodedAccount, DecodeError>`
   - **And** the trait remains `Send + Sync`

2. **AC2: Discriminator matching**
   - **Given** the decoder receives instruction data with a valid 8-byte discriminator
   - **When** it processes the data
   - **Then** it uses the pre-computed `discriminator` field from each `idl.instructions[]` entry to match
   - **And** it supports variable-length discriminators (compare only `min(data.len(), entry.discriminator.len())` bytes)
   - **And** for IDLs without pre-computed discriminators, it computes `SHA-256("global:<snake_case_name>")[0..8]`

3. **AC3: Borsh deserialization of instruction args**
   - **Given** a matched instruction with `args` field definitions
   - **When** the decoder deserializes the remaining bytes (after discriminator)
   - **Then** it recursively decodes each arg according to its `IdlType`
   - **And** the result is a `serde_json::Value` object with field names matching the IDL
   - **And** the decoded value is wrapped in a `DecodedInstruction` with `program_id`, `name`, and `args`

4. **AC4: Bytemuck/BytemuckUnsafe rejection**
   - **Given** a type definition with `serialization` field set to `Bytemuck` or `BytemuckUnsafe`
   - **When** the decoder encounters it during deserialization
   - **Then** it returns `DecodeError::UnsupportedType` with a message indicating non-Borsh serialization is not supported

5. **AC5: Large integer string representation**
   - **Given** a u128, i128, u256, or i256 value
   - **When** the decoder serializes it to JSON
   - **Then** the value is always represented as a JSON string to prevent precision loss in JavaScript consumers

6. **AC6: DecodeError enum**
   - **Given** the `DecodeError` enum
   - **When** I inspect it
   - **Then** it includes variants: `UnknownDiscriminator`, `DeserializationFailed`, `IdlNotLoaded`, `UnsupportedType`
   - **And** it derives `thiserror::Error`
   - **And** `impl From<DecodeError> for PipelineError` exists

7. **AC7: Unknown discriminator handling**
   - **Given** instruction data with an unrecognized discriminator
   - **When** `decode_instruction()` is called
   - **Then** it returns `DecodeError::UnknownDiscriminator` with the hex-encoded discriminator bytes

8. **AC8: decode_account stub**
   - **Given** this story focuses on instruction decoding
   - **When** `decode_account()` is called
   - **Then** it returns `DecodeError::UnsupportedType("account decoding not yet implemented")` as a placeholder (full implementation in Story 3.2)

## Tasks / Subtasks

- [x] Task 1: Add dependencies to Cargo.toml (AC: all)
  - [x] Uncomment `anchor-lang-idl-spec = "0.1.0"`
  - [x] Add `sha2 = "0.10"` for discriminator computation
  - [x] Add `bs58 = "0.5"` for Pubkey base58 encoding
  - [x] Verify `cargo build` compiles with new deps
- [x] Task 2: Update SolarixDecoder trait signature (AC: #1)
  - [x] Change `decode_instruction` to accept `idl: &anchor_lang_idl_spec::Idl` and return `Result<DecodedInstruction, DecodeError>`
  - [x] Change `decode_account` to accept `idl: &anchor_lang_idl_spec::Idl` and return `Result<DecodedAccount, DecodeError>`
  - [x] Fix any compilation errors in `pipeline/mod.rs` (From conversion still works)
- [x] Task 3: Implement TypeRegistry for IDL type lookup (AC: #3, #4)
  - [x] Create `TypeRegistry` struct: `HashMap<String, IdlTypeDef>` built from `idl.types`
  - [x] Check `serialization` field: if not Borsh, return `DecodeError::UnsupportedType`
  - [x] Handle type alias resolution (`IdlTypeDefTy::Type { alias }`)
- [x] Task 4: Implement core `decode_type()` recursive descent (AC: #3, #5)
  - [x] Primitives: bool, u8-u64, i8-i64, f32, f64 (direct JSON number)
  - [x] Large ints: u128, i128, u256, i256 -> JSON string (AC: #5)
  - [x] String: u32 length prefix + UTF-8 bytes
  - [x] Bytes: u32 length prefix + raw bytes -> JSON array of numbers
  - [x] Pubkey: 32 bytes -> base58 string
  - [x] Option: 1-byte tag, None -> null, Some -> decode inner
  - [x] Vec: u32 count + N elements
  - [x] Array: N elements (no length prefix), handle `IdlArrayLen::Value`
  - [x] Defined type: lookup in TypeRegistry, decode struct/enum recursively
  - [x] Struct (named fields): decode fields in order -> JSON object
  - [x] Enum: u8 variant index + variant payload (unit/tuple/named)
  - [x] Add depth limit (64) to prevent stack overflow on pathological inputs
  - [x] Return `(serde_json::Value, usize)` — value + bytes consumed
- [x] Task 5: Implement discriminator matching (AC: #2, #7)
  - [x] For each `idl.instructions[]`, compare `entry.discriminator` bytes against `data[0..disc_len]`
  - [x] If no `discriminator` field, compute `SHA-256("global:<instruction_name>")[0..8]`
  - [x] On no match, return `DecodeError::UnknownDiscriminator` with hex-encoded first 8 bytes
- [x] Task 6: Implement `ChainparserDecoder` struct (AC: #1, #3, #8)
  - [x] Create `pub struct ChainparserDecoder;` (stateless — all context comes via IDL param)
  - [x] Implement `SolarixDecoder` for `ChainparserDecoder`
  - [x] `decode_instruction`: match discriminator -> decode args -> wrap in `DecodedInstruction`
  - [x] `decode_account`: return `DecodeError::UnsupportedType` placeholder for now
- [x] Task 7: Unit tests (AC: all)
  - [x] Test: decode primitive instruction args (u8, u64, bool, string, pubkey)
  - [x] Test: decode nested struct in instruction args
  - [x] Test: decode enum variant in instruction args
  - [x] Test: u128/i128 values produce JSON strings
  - [x] Test: unknown discriminator returns correct error
  - [x] Test: Bytemuck type returns UnsupportedType error
  - [x] Test: empty args instruction (discriminator only)
  - [x] `cargo build`, `cargo clippy`, `cargo fmt -- --check` pass

## Dev Notes

### Current Codebase State (after Stories 1.1, 1.2, 1.3)

**`src/decoder/mod.rs`** contains:

```rust
pub trait SolarixDecoder: Send + Sync {
    fn decode_instruction(&self, program_id: &str, data: &[u8]) -> Result<serde_json::Value, DecodeError>;
    fn decode_account(&self, program_id: &str, data: &[u8]) -> Result<serde_json::Value, DecodeError>;
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unknown discriminator: {0}")]
    UnknownDiscriminator(String),
    #[error("deserialization failed: {0}")]
    DeserializationFailed(String),
    #[error("IDL not loaded for program: {0}")]
    IdlNotLoaded(String),
    #[error("unsupported type: {0}")]
    UnsupportedType(String),
}
```

**`src/types.rs`** contains:

```rust
pub struct DecodedInstruction {
    pub program_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

pub struct DecodedAccount {
    pub program_id: String,
    pub account_type: String,
    pub pubkey: String,
    pub data: serde_json::Value,
}
```

**`src/pipeline/mod.rs`** already has `Decode(#[from] DecodeError)` in `PipelineError`. This `From` conversion continues to work unchanged.

**`Cargo.toml`** has `anchor-lang-idl-spec` commented out. `serde` and `serde_json` are already dependencies.

### Implementation Approach: Custom Borsh Decoder

The `chainparser` fork (primary path in architecture doc) is high-risk: dormant repo, needs solana-sdk 1.18->3.x upgrade. **For this story, build the custom Borsh decoder directly.** The research report (agent-1e) provides a complete blueprint at ~560 LOC for core decoder + type registry. The `SolarixDecoder` trait abstraction means the implementation can be swapped later if chainparser fork materializes.

Name the implementation `ChainparserDecoder` per the architecture doc's naming convention (the trait is the stable API; the impl name is a detail).

### Dependency Changes

```toml
# Uncomment in Cargo.toml:
anchor-lang-idl-spec = "0.1.0"

# Add new:
sha2 = "0.10"    # SHA-256 for discriminator computation
bs58 = "0.5"     # Base58 encoding for Pubkey display
```

Do NOT uncomment `chainparser`, `solana-rpc-client-api`, or `solana-pubsub-client` yet — they are not needed for this story.

### Trait Signature Update

The current stub uses `serde_json::Value` returns. Update to use the typed `DecodedInstruction`/`DecodedAccount` from `types.rs` and accept `&Idl`:

```rust
use anchor_lang_idl_spec::Idl;
use crate::types::{DecodedInstruction, DecodedAccount};

pub trait SolarixDecoder: Send + Sync {
    fn decode_instruction(
        &self,
        program_id: &str,
        data: &[u8],
        idl: &Idl,
    ) -> Result<DecodedInstruction, DecodeError>;

    fn decode_account(
        &self,
        program_id: &str,
        data: &[u8],
        idl: &Idl,
    ) -> Result<DecodedAccount, DecodeError>;
}
```

### anchor-lang-idl-spec Key Types

The `Idl` struct from `anchor-lang-idl-spec` (v0.1.0) provides:

- `idl.instructions: Vec<IdlInstruction>` — each has `name`, `discriminator: Vec<u8>`, `args: Vec<IdlField>`
- `idl.accounts: Vec<IdlAccount>` — each has `name`, `discriminator: Vec<u8>`
- `idl.types: Vec<IdlTypeDef>` — user-defined types (structs, enums, aliases)
- `idl.metadata: IdlMetadata` — program name, version, spec version

`IdlField` has `name: String` and `ty: IdlType`.

`IdlType` enum variants (from the crate):

```
Bool, U8, I8, U16, I16, U32, I32, F32, U64, I64, F64, U128, I128, U256, I256,
Bytes, String, Pubkey,
Option(Box<IdlType>), Vec(Box<IdlType>),
Array(Box<IdlType>, IdlArrayLen),
Defined { name: String, generics: Vec<IdlGenericArg> },
Generic(String)
```

**CRITICAL:** The Rust spec does NOT include `HashMap`, `BTreeMap`, `HashSet`, `BTreeSet`, `COption`, or `Tuple` as direct IdlType variants. These appear only in TypeScript IDL types. For MVP, handle only the types in the Rust spec. If encountered as `Defined` references, they'll be looked up in the type registry.

`IdlTypeDef` has:

- `name: String`
- `ty: IdlTypeDefTy` — `Struct { fields }`, `Enum { variants }`, or `Type { alias }`
- `serialization: IdlSerialization` — default `Borsh`, also `Bytemuck`, `BytemuckUnsafe`, `Custom(String)`
- `generics: Vec<IdlTypeDefGeneric>`

`IdlDefinedFields` (for struct fields / enum variant fields):

- `Named(Vec<IdlField>)` — `[{"name": "x", "type": "u64"}]`
- `Tuple(Vec<IdlType>)` — `["u64", "u8"]`

### Core Decoder Architecture

Single file: `src/decoder/mod.rs`. Keep everything in one file — no sub-modules for a ~500 LOC implementation.

```
ChainparserDecoder::decode_instruction()
  1. Build TypeRegistry from idl.types (cache-friendly: do this once per call or accept pre-built)
  2. Match discriminator against idl.instructions[].discriminator
  3. For matched instruction, decode each arg via decode_type()
  4. Return DecodedInstruction { program_id, name, args: json_object }

decode_type(data, offset, idl_type, registry, depth) -> Result<(Value, usize), DecodeError>
  Match on IdlType variant:
    Primitives -> read N bytes LE, return JSON number (or string for u128+)
    String -> read u32 len + UTF-8 bytes
    Bytes -> read u32 len + raw bytes -> JSON array
    Pubkey -> 32 bytes -> bs58 encode -> JSON string
    Option -> 1-byte tag, recurse if Some
    Vec -> u32 count, recurse N times
    Array -> N elements (no prefix), recurse
    Defined -> lookup in TypeRegistry, recurse on struct/enum
    Generic -> error (should be resolved before decode)
```

### Borsh Wire Format Quick Reference

- All multi-byte integers: **little-endian**
- No padding, no alignment — packed format
- `bool`: 1 byte (0x00=false, 0x01=true, anything else=error)
- `String`: u32 LE byte length + UTF-8 bytes
- `Vec<T>`: u32 LE element count + N encoded elements
- `[T; N]`: N encoded elements, NO length prefix
- `Option<T>`: 0x00=None, 0x01=Some + encoded T
- `Enum`: u8 variant index + variant payload
- `Struct`: fields concatenated in declaration order
- Pubkey: 32 raw bytes (display as base58)

### Discriminator Matching

v0.30+ IDLs have pre-computed `discriminator: Vec<u8>` on each instruction entry. Use these directly:

```rust
fn find_instruction<'a>(data: &[u8], idl: &'a Idl) -> Option<&'a IdlInstruction> {
    for ix in &idl.instructions {
        let disc = &ix.discriminator;
        if data.len() >= disc.len() && data[..disc.len()] == disc[..] {
            return Some(ix);
        }
    }
    None
}
```

Fallback for IDLs without discriminators (rare, mostly legacy):

```rust
use sha2::{Sha256, Digest};

fn compute_instruction_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{name}").as_bytes());
    let hash = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}
```

### Helper Functions Pattern

```rust
fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, DecodeError> {
    ensure_bytes(data, offset, 2)?;
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

fn ensure_bytes(data: &[u8], offset: usize, needed: usize) -> Result<(), DecodeError> {
    if offset + needed > data.len() {
        return Err(DecodeError::DeserializationFailed(format!(
            "unexpected EOF: need {} bytes at offset {}, have {}",
            needed, offset, data.len()
        )));
    }
    Ok(())
}
```

### Enum Decoding

Three variant payload formats in IDL:

1. **Unit variant**: no `fields` key -> zero bytes after variant index
2. **Tuple variant**: `fields` is array of `IdlType` -> decode each in order
3. **Named variant**: `fields` is array of `IdlField` (objects with name+type) -> JSON object

JSON output format for enums:

- **Unit variant**: `{ "VariantName": {} }`
- **Named variant**: `{ "VariantName": { "field1": v1, "field2": v2 } }`
- **Tuple variant (single)**: `{ "VariantName": value }` (unwrap single-element tuple)
- **Tuple variant (multi)**: `{ "VariantName": [v1, v2] }` (JSON array)

Distinguish by checking `IdlDefinedFields::Named` vs `IdlDefinedFields::Tuple`.

### Generic Type Resolution

Generics are rare in practice. For MVP, implement basic single-level resolution:

1. When encountering `Defined { name, generics }`, look up the typedef
2. If typedef has `generics` definition, zip with the provided generic args
3. When encountering `Generic(name)` during decode, substitute from the bindings map
4. For `IdlArrayLen::Generic`, resolve from const generic bindings
5. If resolution fails, return `DecodeError::DeserializationFailed`

### Depth Limit

Cap recursion at 64 levels. Pass a `depth: u32` parameter to `decode_type` and increment on each recursive call. Return error if exceeded. In practice, Solana account data is flat or shallowly nested (2-3 levels typical).

### Trailing Bytes After Decoding

After decoding all instruction args, there may be unconsumed trailing bytes (padding, extra data, or malformed input). Do NOT error on trailing bytes — log at `debug!` level with the count of unconsumed bytes and continue. This is best-effort decoding for an indexer.

### Testing Strategy

Build test IDLs programmatically using `anchor_lang_idl_spec` types. No need for fixture files — construct `Idl` structs in test code with known instruction definitions and hand-craft the corresponding Borsh bytes.

**CRITICAL:** Before writing any test code, use the `context7` MCP server to fetch the current `anchor-lang-idl-spec` crate documentation. The `Idl`, `IdlMetadata`, `IdlInstruction`, and `IdlTypeDef` struct fields, optionality, and `Default` derivations MUST be verified against the actual crate API. Do NOT guess struct field names or assume `Default` is derived — construct each field explicitly based on verified docs. Create a `make_test_idl()` helper that builds an `Idl` with the minimum required fields.

### Files Modified by This Story

| File                 | Action | Purpose                                                    |
| -------------------- | ------ | ---------------------------------------------------------- |
| `Cargo.toml`         | Modify | Uncomment `anchor-lang-idl-spec`, add `sha2`, `bs58`       |
| `src/decoder/mod.rs` | Modify | Update trait, implement ChainparserDecoder + Borsh decoder |

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` in production code — use `?` with `map_err`
- NO `println!` — use `tracing` macros if logging is needed (warn on unknown types)
- NO separate `error.rs` — `DecodeError` stays in `decoder/mod.rs`
- NO modifications to `types.rs` — `DecodedInstruction` and `DecodedAccount` are already correct
- NO modifications to `pipeline/mod.rs` — `From<DecodeError>` already works
- NO modifications to `storage/`, `api/`, `idl/` modules — this story only touches decoder
- Do NOT import or depend on `chainparser` — build the decoder from scratch per the custom decoder architecture
- Do NOT handle `COption` in this story — it is not in the Rust `IdlType` spec; COption support will be added in Story 3.2 alongside account decoding if needed
- Do NOT decode accounts — `decode_account` returns a placeholder error; full implementation is Story 3.2

### Import Ordering for `src/decoder/mod.rs`

```rust
// std library
use std::collections::HashMap;

// external crates
use anchor_lang_idl_spec::{
    Idl, IdlArrayLen, IdlDefinedFields, IdlField, IdlGenericArg, IdlInstruction,
    IdlSerialization, IdlType, IdlTypeDef, IdlTypeDefTy,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// internal crate
use crate::types::{DecodedAccount, DecodedInstruction};
```

**IMPORTANT:** Verify exact import paths against the crate docs. The `anchor-lang-idl-spec` crate may use different module structure. Use `context7` MCP to check.

### Deferred Work from Previous Stories (Relevant)

- `litesvm` absent from dev-dependencies — will be added when pipeline integration tests are written (Epic 6)
- RPITIT traits not object-safe — not relevant to this story (decoder trait uses concrete return types)

### Project Structure Notes

All code goes in `src/decoder/mod.rs` per the architecture document. No new files created. The module layout remains 14 source files.

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-3-transaction-decoding-batch-indexing.md#Story 3.1]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Decoder Architecture]
- [Source: _bmad-output/planning-artifacts/architecture/implementation-patterns-consistency-rules.md#Error Handling Flow]
- [Source: _bmad-output/planning-artifacts/architecture/project-structure-boundaries.md#Architectural Boundaries]
- [Source: _bmad-output/planning-artifacts/research/agent-1e-custom-borsh-decoder-feasibility.md]
- [Source: _bmad-output/planning-artifacts/research/anchor-idl-type-spec-borsh-wire-format.md]
- [Source: _bmad-output/implementation-artifacts/1-1-project-scaffolding-and-configuration.md]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (claude-opus-4-6)

### Debug Log References

None — clean implementation with no debug issues.

### Completion Notes List

- Implemented custom Borsh decoder (~540 LOC in `src/decoder/mod.rs`) instead of chainparser fork
- Updated `SolarixDecoder` trait to accept `&Idl` and return typed `DecodedInstruction`/`DecodedAccount`
- Built `TypeRegistry` for resolving named IDL types with serialization validation (Bytemuck/BytemuckUnsafe rejection)
- Implemented full recursive `decode_type()` covering all `IdlType` variants: primitives, large ints (u128/i128 as strings, u256/i256 as hex strings), String, Bytes, Pubkey (base58), Option, Vec, Array, Defined types (struct/enum/alias), Generic resolution
- Discriminator matching: pre-computed from IDL with SHA-256 fallback for legacy IDLs
- Depth limit of 64 to prevent stack overflow
- `ChainparserDecoder` struct implements `SolarixDecoder` — stateless, all context via IDL param
- `decode_account` returns placeholder `UnsupportedType` error (full impl in Story 3.2)
- 12 unit tests covering all ACs: primitives, string+pubkey, nested struct, enum variants, u128/i128 strings, unknown discriminator, Bytemuck rejection, empty args, account stub, fallback discriminator, Vec+Option, Option None
- `pipeline/mod.rs` `From<DecodeError>` continues to work unchanged
- No modifications to `types.rs`, `pipeline/`, `storage/`, `api/`, or `idl/` modules
- All checks pass: `cargo build`, `cargo test` (12 pass), `cargo clippy` (clean), `cargo fmt --check` (clean)

### Change Log

- 2026-04-05: Implemented SolarixDecoder trait update, ChainparserDecoder with custom Borsh decoder, TypeRegistry, discriminator matching, and 12 unit tests (Story 3.1)

### File List

- `Cargo.toml` (modified) — uncommented anchor-lang-idl-spec, added sha2 and bs58
- `src/decoder/mod.rs` (modified) — full rewrite: trait update, TypeRegistry, decode_type recursive descent, ChainparserDecoder impl, 12 unit tests
