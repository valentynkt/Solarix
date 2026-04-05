# Story 3.2: Account State Decoding

Status: done

## Story

As a user,
I want account state data decoded from on-chain accounts using the IDL,
so that current account states are stored as queryable typed data alongside instruction history.

## Acceptance Criteria

1. **AC1: Account discriminator matching**
   - **Given** raw account data with a valid discriminator
   - **When** `decode_account()` is called
   - **Then** it matches the first bytes against `idl.accounts[]` entries using each entry's `discriminator` field
   - **And** for IDLs without pre-computed discriminators, it computes `SHA-256("account:<PascalCaseName>")[0..8]`
   - **And** it supports variable-length discriminators (same logic as instruction discriminators from Story 3.1)

2. **AC2: Account struct resolution and decoding**
   - **Given** a matched account name (e.g., "Counter")
   - **When** the decoder processes it
   - **Then** it looks up the matched account name in `idl.types[]` to get the struct definition
   - **And** it deserializes the remaining bytes (after discriminator) as Borsh according to the struct's field definitions
   - **And** the result is a `DecodedAccount` with `program_id`, `account_type` (the name), `pubkey`, and decoded `data` as `serde_json::Value`

3. **AC3: Unknown discriminator handling**
   - **Given** account data with an unrecognized discriminator
   - **When** `decode_account()` is called
   - **Then** it returns `DecodeError::UnknownDiscriminator` with the hex-encoded discriminator bytes
   - **And** the caller (pipeline) logs at `warn!` level and skips the account without crashing

4. **AC4: COption support**
   - **Given** a type definition containing `COption<T>` (appears as `{"coption": T}` in IDL JSON)
   - **When** the decoder encounters it
   - **Then** it reads a 4-byte u32 LE tag (0=None, 1=Some)
   - **And** for None: advances past the tag PLUS `sizeof(T)` bytes (always fixed-size allocation, zeroed)
   - **And** for Some: decodes the inner value after the 4-byte tag
   - **And** the total consumed bytes is always `4 + fixed_size(T)` regardless of None/Some
   - **And** if `T` is not fixed-size, returns `DecodeError::UnsupportedType("COption with variable-size inner type")`

5. **AC5: Nested type handling**
   - **Given** a type definition with nested structs, enums, Options, and Vecs
   - **When** the decoder processes it
   - **Then** it recursively descends through the type tree (reusing the `decode_type()` from Story 3.1)
   - **And** recursive depth is capped at 64 levels to prevent stack overflow

6. **AC6: Batch failure detection helper**
   - **Given** the decoder module
   - **When** I inspect it
   - **Then** it exports a `pub fn is_high_failure_rate(failures: usize, total: usize) -> bool` helper that returns `true` when `failures * 100 / total > 90` (for total > 0)
   - **And** the pipeline (future story 3.5) uses this to log at `error!` level indicating likely IDL version mismatch

7. **AC7: pubkey parameter added to decode_account**
   - **Given** the `SolarixDecoder` trait
   - **When** I inspect `decode_account`
   - **Then** it accepts `pubkey: &str` as a parameter so the returned `DecodedAccount` includes the account's public key

## Tasks / Subtasks

- [x] Task 1: Update `decode_account` trait signature to include `pubkey` (AC: #7)
  - [x] Add `pubkey: &str` parameter to `SolarixDecoder::decode_account`
  - [x] Update `ChainparserDecoder` impl signature
  - [x] Update any existing tests that call `decode_account` (e.g., `test_decode_account_stub`) to pass the new `pubkey` argument
  - [x] Fix any compilation errors
- [x] Task 2: Implement account discriminator matching (AC: #1, #3)
  - [x] Create `find_account()` function matching against `idl.accounts[].discriminator`
  - [x] Add SHA-256 fallback: `SHA-256("account:<PascalCaseName>")[0..8]` for IDLs without discriminators
  - [x] On no match, return `DecodeError::UnknownDiscriminator` with hex-encoded bytes
- [x] Task 3: Implement `decode_account` body (AC: #2)
  - [x] Match discriminator via `find_account()`
  - [x] Look up account name in `TypeRegistry` (built from `idl.types[]`)
  - [x] Verify the type is a Borsh-serialized struct (check `serialization` field)
  - [x] Decode struct fields using existing `decode_type()` from Story 3.1
  - [x] Wrap result in `DecodedAccount { program_id, account_type, pubkey, data }`
- [x] Task 4: Add COption support to `decode_type()` (AC: #4)
  - [x] Detect COption: it appears as a `Defined` type with name matching a COption pattern, OR handle it as a special case if the IDL uses `{"coption": T}` JSON format
  - [x] Implement `fixed_size()` helper for computing static size of an `IdlType`
  - [x] Decode COption: read u32 tag, if None skip `fixed_size(T)` bytes, if Some decode inner
  - [x] Error on variable-size inner types
- [x] Task 5: Add batch failure detection helper (AC: #6)
  - [x] Add `pub fn is_high_failure_rate(failures: usize, total: usize) -> bool`
  - [x] Returns `true` when failures > 90% of total (and total > 0)
- [x] Task 6: Unit tests (AC: all)
  - [x] Test: decode simple account struct (pubkey, u64, bool fields)
  - [x] Test: decode account with nested struct
  - [x] Test: decode account with enum field
  - [x] Test: decode account with Option and Vec fields
  - [x] Test: COption<Pubkey> with Some value
  - [x] Test: COption<Pubkey> with None value (verify fixed-size skip)
  - [x] Test: unknown account discriminator returns correct error
  - [x] Test: account name not found in types[] returns error
  - [x] Test: `is_high_failure_rate` threshold logic
  - [x] `cargo build`, `cargo clippy`, `cargo fmt -- --check` pass

### Review Findings

- [x] [Review][Patch] `is_high_failure_rate` integer overflow — `failures * 100` can overflow usize [src/decoder/mod.rs:188] — fixed: use `checked_mul`
- [x] [Review][Patch] `fixed_size` Array branch unchecked multiplication — `s * n` overflow [src/decoder/mod.rs:494] — fixed: use `checked_mul`
- [x] [Review][Patch] `fixed_size` infinite recursion on cyclic IDL types — no depth guard [src/decoder/mod.rs:497-511] — fixed: added depth limit via `fixed_size_inner`
- [x] [Review][Patch] `fixed_size` returns None for type aliases and tuple-field structs [src/decoder/mod.rs:497-511] — fixed: added `Type{alias}` and `Tuple` arms
- [x] [Review][Defer] Vec allocation unbounded by count [src/decoder/mod.rs:370] — deferred, pre-existing from Story 3.1
- [x] [Review][Defer] Prefix discriminator ambiguity [src/decoder/mod.rs:83-107] — deferred, pre-existing from Story 3.1, theoretical with real Anchor IDLs

## Dev Notes

### Story 3.1 State (Prerequisite — Must Be Completed)

Story 3.1 implements the core decoder infrastructure. After 3.1 is complete, `src/decoder/mod.rs` contains:

- **`SolarixDecoder` trait** — with `decode_instruction()` (working) and `decode_account()` (placeholder returning `UnsupportedType`)
- **`ChainparserDecoder` struct** — stateless impl of `SolarixDecoder`
- **`TypeRegistry`** — `HashMap<String, IdlTypeDef>` built from `idl.types`
- **`decode_type()`** — recursive descent Borsh decoder handling all `IdlType` variants (primitives, strings, pubkeys, options, vecs, arrays, defined types, enums, generics)
- **`find_instruction()`** — discriminator matching for instructions
- **Helper functions** — `ensure_bytes()`, `read_u16_le()`, `read_u32_le()`, etc.
- **`DecodeError` enum** — 4 variants, unchanged
- **Dependencies** — `anchor-lang-idl-spec`, `sha2`, `bs58` already in Cargo.toml

This story ONLY needs to:

1. Add `pubkey` param to `decode_account` trait method
2. Replace the placeholder `decode_account` with real implementation
3. Add COption support to `decode_type()`
4. Add `fixed_size()` helper
5. Add `is_high_failure_rate()` helper

### Account Discriminator Matching

Account discriminators use `SHA-256("account:<PascalCaseName>")` (note: PascalCase, not snake_case like instructions):

```rust
fn compute_account_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("account:{name}").as_bytes());
    let hash = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

fn find_account<'a>(data: &[u8], idl: &'a Idl) -> Option<&'a IdlAccount> {
    for acct in &idl.accounts {
        let disc = &acct.discriminator;
        if !disc.is_empty() && data.len() >= disc.len() && data[..disc.len()] == disc[..] {
            return Some(acct);
        }
    }
    // Fallback: compute discriminators if IDL entries have empty discriminator fields
    for acct in &idl.accounts {
        if acct.discriminator.is_empty() {
            let computed = compute_account_discriminator(&acct.name);
            if data.len() >= 8 && data[..8] == computed {
                return Some(acct);
            }
        }
    }
    None
}
```

### Account Name to Type Lookup

After matching the account discriminator, the account name (e.g., "Counter") is used to find the struct definition in `idl.types[]`:

```rust
// In decode_account implementation:
let account = find_account(data, idl)
    .ok_or_else(|| DecodeError::UnknownDiscriminator(hex::encode(&data[..8.min(data.len())])))?;

let registry = TypeRegistry::from_idl(idl);
let type_def = registry.get(&account.name)
    .ok_or_else(|| DecodeError::DeserializationFailed(
        format!("account type '{}' not found in IDL types", account.name)
    ))?;
```

**IMPORTANT:** The `hex` crate is NOT a dependency. Use a manual hex encoding or format bytes as `{:02x}`. Story 3.1 already handles this for instruction discriminators — follow the same pattern.

### COption Implementation

COption is Solana's C-compatible option type (`solana_program::program_option::COption`). It differs from Borsh `Option`:

| Property      | Option<T>     | COption<T>       |
| ------------- | ------------- | ---------------- |
| Tag           | 1 byte (u8)   | 4 bytes (u32 LE) |
| Size for None | 1 byte        | 4 + sizeof(T)    |
| Size for Some | 1 + sizeof(T) | 4 + sizeof(T)    |
| Layout        | Variable      | Fixed            |

COption appears in IDLs generated for SPL Token programs (e.g., `freeze_authority: COption<Pubkey>`). In the IDL JSON, it appears as `{"coption": "pubkey"}`.

**Detection:** COption is NOT in the `anchor-lang-idl-spec` Rust `IdlType` enum. It appears only in the TypeScript IDL types. In practice, it may appear in IDL JSON as `{"coption": T}`. When deserializing an IDL that contains COption, `anchor-lang-idl-spec` may either:

1. Parse it into an unknown/catch-all variant (if `#[non_exhaustive]` + `#[serde(other)]`)
2. Fail to parse entirely
3. Parse it as a `Defined` type with a special name

**Strategy:** Use the `context7` MCP server to verify how `anchor-lang-idl-spec` handles `{"coption": ...}` in IDL JSON. If it's not supported natively, implement COption handling by:

1. Pre-processing the raw IDL JSON before parsing with `anchor-lang-idl-spec`
2. OR detecting COption as a named type convention in the type registry

**`fixed_size()` helper** — needed for COption to know how many bytes to skip for None:

```rust
fn fixed_size(ty: &IdlType, registry: &TypeRegistry) -> Option<usize> {
    match ty {
        IdlType::Bool => Some(1),
        IdlType::U8 | IdlType::I8 => Some(1),
        IdlType::U16 | IdlType::I16 => Some(2),
        IdlType::U32 | IdlType::I32 | IdlType::F32 => Some(4),
        IdlType::U64 | IdlType::I64 | IdlType::F64 => Some(8),
        IdlType::U128 | IdlType::I128 => Some(16),
        IdlType::U256 | IdlType::I256 => Some(32),
        IdlType::Pubkey => Some(32),
        IdlType::Array(inner, IdlArrayLen::Value(n)) => {
            fixed_size(inner, registry).map(|s| s * n)
        }
        // String, Bytes, Vec, Option are variable-size
        IdlType::String | IdlType::Bytes => None,
        IdlType::Vec(_) | IdlType::Option(_) => None,
        // Defined types: look up and check if all fields are fixed-size
        IdlType::Defined { name, .. } => {
            let typedef = registry.get(name)?;
            // Only structs with all fixed-size named fields
            // Enums are variable-size (variant payloads differ)
            match &typedef.ty {
                IdlTypeDefTy::Struct { fields: Some(IdlDefinedFields::Named(fields)) } => {
                    let mut total = 0;
                    for f in fields {
                        total += fixed_size(&f.ty, registry)?;
                    }
                    Some(total)
                }
                _ => None,
            }
        }
        _ => None,
    }
}
```

### decode_account Implementation Sketch

```rust
fn decode_account(
    &self,
    program_id: &str,
    pubkey: &str,
    data: &[u8],
    idl: &Idl,
) -> Result<DecodedAccount, DecodeError> {
    let account = find_account(data, idl)
        .ok_or_else(|| {
            let disc_hex = data.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>();
            DecodeError::UnknownDiscriminator(disc_hex)
        })?;

    let registry = TypeRegistry::from_idl(idl);
    let type_def = registry.get(&account.name)
        .ok_or_else(|| DecodeError::DeserializationFailed(
            format!("account type '{}' not found in IDL types", account.name)
        ))?;

    // Check serialization
    if !matches!(type_def.serialization, IdlSerialization::Borsh) {
        return Err(DecodeError::UnsupportedType(
            format!("account '{}' uses non-Borsh serialization", account.name)
        ));
    }

    let disc_len = if account.discriminator.is_empty() { 8 } else { account.discriminator.len() };
    // Use the existing decode helper for Defined types from Story 3.1.
    // The exact function name depends on 3.1's implementation — look for the function
    // that decodes an IdlTypeDef (struct/enum) given a TypeRegistry and depth counter.
    // Pass empty generics HashMap and depth=0 for the top-level call.
    let (value, _consumed) = decode_typedef_fields(&data[disc_len..], 0, type_def, &registry, &HashMap::new(), 0)?;

    Ok(DecodedAccount {
        program_id: program_id.to_string(),
        account_type: account.name.clone(),
        pubkey: pubkey.to_string(),
        data: value,
    })
}
```

### Trait Signature Change

The current trait has:

```rust
fn decode_account(&self, program_id: &str, data: &[u8], idl: &Idl) -> Result<DecodedAccount, DecodeError>;
```

Add `pubkey` parameter:

```rust
fn decode_account(&self, program_id: &str, pubkey: &str, data: &[u8], idl: &Idl) -> Result<DecodedAccount, DecodeError>;
```

This is needed because `DecodedAccount` includes `pubkey: String` — the caller (pipeline) provides the pubkey from the RPC response, the decoder includes it in the result.

**Note:** Story 3.1's placeholder `decode_account` doesn't use `pubkey` (it just returns an error), so adding the parameter to the trait is a signature-only change that requires updating the placeholder impl.

### Batch Failure Detection

Simple helper function — the pipeline orchestrator (Story 3.5) will use this:

```rust
/// Returns true if the failure rate exceeds 90%, indicating likely IDL mismatch.
pub fn is_high_failure_rate(failures: usize, total: usize) -> bool {
    total > 0 && failures * 100 / total > 90
}
```

This is a pure function, no state. Exported from `decoder/mod.rs` for the pipeline to use.

### Files Modified by This Story

| File                 | Action | Purpose                                                                                             |
| -------------------- | ------ | --------------------------------------------------------------------------------------------------- |
| `src/decoder/mod.rs` | Modify | Add `pubkey` param, implement decode_account, add COption, add fixed_size, add is_high_failure_rate |

No other files modified. No new dependencies needed — `sha2`, `bs58`, `anchor-lang-idl-spec` already added in Story 3.1.

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` in production code
- NO `println!` — use `tracing` macros
- NO modifications to `types.rs`, `pipeline/mod.rs`, `storage/`, `api/`, `idl/`
- NO modifications to `Cargo.toml` — all deps already present from Story 3.1
- Do NOT add a `hex` crate dependency — format discriminator bytes manually with `format!("{:02x}")`
- Do NOT duplicate `decode_type()` logic — reuse the existing recursive descent decoder from Story 3.1
- Do NOT add `decode_event()` — event decoding is post-MVP

### Testing Strategy

Same approach as Story 3.1: build test IDLs programmatically. For account tests, construct `Idl` structs with `accounts` entries (name + discriminator) and matching `types` entries.

**CRITICAL:** Use `context7` MCP server to verify `anchor-lang-idl-spec` struct fields before writing test code. The `IdlAccount` struct fields (name, discriminator, etc.) must be verified against actual crate API. Follow the same test helper pattern established in Story 3.1.

**COption test data construction:**

- `COption<Pubkey>` Some: `[0x01, 0x00, 0x00, 0x00]` + 32 bytes pubkey = 36 bytes total
- `COption<Pubkey>` None: `[0x00, 0x00, 0x00, 0x00]` + 32 zero bytes = 36 bytes total
- `COption<u64>` Some: `[0x01, 0x00, 0x00, 0x00]` + 8 bytes LE value = 12 bytes total
- `COption<u64>` None: `[0x00, 0x00, 0x00, 0x00]` + 8 zero bytes = 12 bytes total

### Project Structure Notes

All code stays in `src/decoder/mod.rs`. No new files. Module layout remains 14 source files.

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-3-transaction-decoding-batch-indexing.md#Story 3.2]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Decoder Architecture]
- [Source: _bmad-output/planning-artifacts/research/anchor-idl-type-spec-borsh-wire-format.md#Section 13 COption Deep Dive]
- [Source: _bmad-output/planning-artifacts/research/agent-1e-custom-borsh-decoder-feasibility.md#COption Fixed-Size Layout]
- [Source: _bmad-output/implementation-artifacts/3-1-solarixdecoder-trait-and-instruction-decoding.md]

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6 (1M context)

### Debug Log References

None — clean implementation, no blocking issues encountered.

### Completion Notes List

- Task 1 (pubkey param): Already implemented in Story 3.1's work. Fixed `test_decode_account_stub` to use 4-arg signature and test `UnknownDiscriminator` instead of old placeholder `UnsupportedType`.
- Task 2 (discriminator matching): Added `compute_account_discriminator()` using `SHA-256("account:<PascalCaseName>")` and `find_account_with_fallback()` mirroring `find_instruction_with_fallback()`. Updated `decode_account` to use the fallback version.
- Task 3 (decode_account body): Already implemented in Story 3.1's work — uses `find_account_with_fallback`, `TypeRegistry::resolve`, `check_serialization`, `decode_typedef`. Returns `DecodedAccount` with all required fields.
- Task 4 (COption): Detected as `Defined { name: "COption", generics: [Type { ty }] }` in `decode_type()`. Added `decode_coption()` reading u32 LE tag (0=None, 1=Some), skipping `fixed_size(T)` bytes for None. Added `fixed_size()` helper recursively computing static byte size for all `IdlType` variants. Errors on variable-size inner types.
- Task 5 (is_high_failure_rate): Added `pub fn is_high_failure_rate(failures, total) -> bool` — returns true when `failures * 100 / total > 90` for `total > 0`.
- Task 6 (tests): Added 11 new tests covering all ACs: simple account struct, nested struct, enum field, Option/Vec fields, COption<Pubkey> Some/None, unknown discriminator, missing type, `is_high_failure_rate` threshold logic, discriminator SHA-256 fallback, COption variable-size inner type error. All 59 tests pass (23 decoder, 36 other modules). `cargo clippy` and `cargo fmt -- --check` clean.

### Change Log

- 2026-04-05: Implemented Story 3.2 — account state decoding with COption support, discriminator fallback, batch failure detection, 11 new unit tests.

### File List

- `src/decoder/mod.rs` — Modified: added `compute_account_discriminator`, `find_account_with_fallback`, `fixed_size`, `decode_coption`, `is_high_failure_rate`; updated `decode_account` to use fallback; added COption intercept in `decode_type` Defined arm; added 11 new tests
