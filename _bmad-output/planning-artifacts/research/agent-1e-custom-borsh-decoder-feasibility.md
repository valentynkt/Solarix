## Agent 1E: Custom Borsh Decoder Feasibility Assessment

### Verdict: MODERATE

Building a custom runtime Borsh decoder is a well-bounded, tractable engineering problem. The Borsh wire format is deliberately simple: deterministic, tightly packed, no alignment, no padding, no self-description overhead. A recursive descent decoder mapping IDL type definitions to `serde_json::Value` output is approximately 500-800 lines of core logic plus 300-500 lines of supporting code (type registry, error handling). The entire effort is estimated at 3-5 days for one experienced Rust developer. This is a viable Plan B that fits comfortably within the 3-4 week project timeline.

The risk is further reduced by three factors: (1) `borsh-serde-adapter` already proves the pattern works at ~120 lines of core logic; (2) Anchor's TypeScript `BorshAccountsCoder` provides a complete reference implementation for IDL-driven decoding; (3) sol-chainsaw itself (2.2K SLoC total) is small enough to vendor and fork if licensing permits.

---

### Borsh Format Summary

**Encoding Properties:**

- Deterministic: bijective mapping between objects and binary representations
- Packed: no alignment, no padding bytes between fields
- Little-endian: all multi-byte integers stored LE
- Not self-describing: requires external schema (IDL) to decode
- Length-prefixed collections: all dynamic containers use u32 (4-byte LE) length prefix
- Enum discriminant: u8 (1 byte) variant index

**Complete Wire Format:**

| Type                   | Encoding                                               | Byte Size         |
| ---------------------- | ------------------------------------------------------ | ----------------- |
| `bool`                 | `0x00` or `0x01`                                       | 1                 |
| `u8` / `i8`            | raw byte                                               | 1                 |
| `u16` / `i16`          | 2 bytes LE                                             | 2                 |
| `u32` / `i32`          | 4 bytes LE                                             | 4                 |
| `f32`                  | IEEE 754 as u32 LE (NaN rejected)                      | 4                 |
| `u64` / `i64`          | 8 bytes LE                                             | 8                 |
| `f64`                  | IEEE 754 as u64 LE (NaN rejected)                      | 8                 |
| `u128` / `i128`        | 16 bytes LE                                            | 16                |
| `u256` / `i256`        | 32 bytes LE                                            | 32                |
| `()` (unit)            | nothing                                                | 0                 |
| `String`               | u32 byte-length + UTF-8 bytes                          | 4 + N             |
| `Vec<T>`               | u32 element-count + N encoded elements                 | 4 + sum(elements) |
| `[T; N]` (fixed array) | N encoded elements (NO length prefix)                  | sum(elements)     |
| `Option<T>`            | `0x00` if None; `0x01` + encoded T if Some             | 1 [+ T]           |
| `COption<T>`           | u32 tag (0 or 1) + T always allocated (zeroed if None) | 4 + sizeof(T)     |
| `HashMap<K,V>`         | u32 count + entries sorted by key                      | 4 + sum(entries)  |
| `HashSet<T>`           | u32 count + elements sorted                            | 4 + sum(elements) |
| `BTreeMap<K,V>`        | u32 count + entries in key order                       | 4 + sum(entries)  |
| `BTreeSet<T>`          | u32 count + elements in order                          | 4 + sum(elements) |
| Struct                 | fields concatenated in declaration order               | sum(fields)       |
| Enum                   | u8 variant index + variant payload                     | 1 + payload       |
| `Result<T,E>`          | `0x01` + T if Ok; `0x00` + E if Err                    | 1 + payload       |
| `Pubkey`               | 32 raw bytes (base58 on display)                       | 32                |
| `bytes` (`Vec<u8>`)    | u32 length + raw bytes                                 | 4 + N             |

**Complexity Assessment:** The format is simple enough for runtime decoding. There are no variable-length headers, no compression, no schema negotiation. Every type's byte layout is fully determined by the type definition alone. A recursive descent approach naturally mirrors the type tree.

---

### Existing Approaches Found

| Approach                               | Language   | Dynamic?                   | Maturity                | Notes                                                                                                                                                       |
| -------------------------------------- | ---------- | -------------------------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sol-chainsaw` v0.0.2                  | Rust       | Yes (IDL-driven)           | Early (0.0.2, Jan 2023) | Exactly our use case. 2.2K SLoC. Uses Anchor IDL, outputs JSON. Depends on borsh 0.9.3 (outdated).                                                          |
| `borsh-serde-adapter` v1.0.3           | Rust       | Yes (BorshSchemaContainer) | Moderate                | ~120 lines core decoder. Borsh bytes -> serde_json::Value via BorshSchemaContainer. Requires schema from compile-time BorshSchema trait, NOT from IDL.      |
| `borsh` crate `unstable__schema`       | Rust       | Partial                    | Stable (1.6.0)          | BorshSchemaContainer can describe types at runtime, but requires the Rust type to exist at compile time to generate the schema. Cannot work from IDL alone. |
| `@coral-xyz/anchor` BorshAccountsCoder | TypeScript | Yes (IDL-driven)           | Mature                  | Complete IDL-to-layout mapping. Uses buffer-layout + borsh-js. Best reference implementation for IDL type -> decoder logic.                                 |
| `borsh-construct`                      | Python     | Yes (schema-driven)        | Moderate                | Built on Python `construct` library. Dynamic schema definition via code. Good conceptual reference.                                                         |
| `borsh-python`                         | Python     | Yes (dict schemas)         | Moderate                | Dict-based schema definition at runtime. Simple API.                                                                                                        |
| Solana Explorer                        | Web        | Yes (IDL-fetched)          | Production              | Uses on-chain IDLs to decode account data. Closed implementation.                                                                                           |
| `borsh.m2.xyz`                         | Web        | Yes (manual struct def)    | Production              | UI tool for manual Borsh decoding. No API.                                                                                                                  |

**Key Insight:** `borsh-serde-adapter` proves the core pattern (recursive descent from schema to `serde_json::Value`) works in ~120 lines of Rust. However, it uses `BorshSchemaContainer` (requires compile-time type knowledge) rather than Anchor IDL (runtime JSON). Our decoder would replace `BorshSchemaContainer` with `IdlType` + a type registry built from the IDL's `types` array.

---

### Proposed Decoder Architecture

```rust
use serde_json::Value;
use std::collections::HashMap;

/// IDL type definitions (simplified from anchor-lang-idl-spec)
/// In practice, use or re-export from `anchor-lang-idl-spec` or `solana_idl`
pub enum IdlType {
    Bool,
    U8, I8, U16, I16, U32, I32, F32,
    U64, I64, F64, U128, I128, U256, I256,
    Bytes, String, Pubkey,
    Option(Box<IdlType>),
    COption(Box<IdlType>),
    Vec(Box<IdlType>),
    Array(Box<IdlType>, usize),
    Defined { name: String, generics: Vec<IdlGenericArg> },
    HashMap(Box<IdlType>, Box<IdlType>),
    BTreeMap(Box<IdlType>, Box<IdlType>),
    HashSet(Box<IdlType>),
    BTreeSet(Box<IdlType>),
    Tuple(Vec<IdlType>),
}

pub enum IdlTypeDefBody {
    Struct { fields: Vec<(String, IdlType)> },
    Enum { variants: Vec<IdlEnumVariant> },
}

pub struct IdlEnumVariant {
    pub name: String,
    pub fields: Option<IdlDefinedFields>,
}

pub enum IdlDefinedFields {
    Named(Vec<(String, IdlType)>),
    Tuple(Vec<IdlType>),
    Unit,
}

/// Registry of all user-defined types from the IDL
pub struct TypeRegistry {
    types: HashMap<String, IdlTypeDefBody>,
}

/// Core decoder error type
pub enum DecodeError {
    UnexpectedEof { expected: usize, remaining: usize },
    InvalidBool(u8),
    InvalidUtf8,
    InvalidFloat, // NaN
    UnknownType(String),
    VariantIndexOutOfBounds { index: u8, max: usize },
    InvalidOptionTag(u8),
    GenericResolutionFailed(String),
}

/// Core decode function
/// Returns (decoded_value, bytes_consumed)
pub fn decode(
    data: &[u8],
    offset: usize,
    idl_type: &IdlType,
    registry: &TypeRegistry,
) -> Result<(Value, usize), DecodeError> {
    // ... recursive descent implementation
}

/// Top-level: decode an entire account
pub fn decode_account(
    data: &[u8],
    account_type: &str,
    registry: &TypeRegistry,
    skip_discriminator: bool,
) -> Result<Value, DecodeError> {
    let start = if skip_discriminator { 8 } else { 0 };
    let (value, _consumed) = decode_struct(data, start, account_type, registry)?;
    Ok(value)
}
```

**Core `decode` function sketch (the heart of the decoder):**

```rust
fn decode(
    data: &[u8],
    offset: usize,
    ty: &IdlType,
    reg: &TypeRegistry,
) -> Result<(Value, usize), DecodeError> {
    match ty {
        // === Primitives (trivial, 2-5 lines each) ===
        IdlType::Bool => {
            ensure_bytes(data, offset, 1)?;
            match data[offset] {
                0 => Ok((Value::Bool(false), 1)),
                1 => Ok((Value::Bool(true), 1)),
                v => Err(DecodeError::InvalidBool(v)),
            }
        }
        IdlType::U8 => Ok((Value::from(data[offset]), 1)),
        IdlType::U16 => Ok((Value::from(read_u16_le(data, offset)?), 2)),
        IdlType::U32 => Ok((Value::from(read_u32_le(data, offset)?), 4)),
        IdlType::U64 => Ok((Value::from(read_u64_le(data, offset)?), 8)),
        IdlType::U128 => Ok((Value::String(read_u128_le(data, offset)?.to_string()), 16)),
        IdlType::U256 => Ok((Value::String(hex::encode(&data[offset..offset+32])), 32)),
        // ... i8-i128 mirror the unsigned versions
        IdlType::F32 => {
            let bits = read_u32_le(data, offset)?;
            let f = f32::from_bits(bits);
            if f.is_nan() { return Err(DecodeError::InvalidFloat); }
            Ok((json!(f), 4))
        }
        IdlType::F64 => { /* similar, 8 bytes */ }

        // === Strings & Bytes (simple, 5-8 lines each) ===
        IdlType::String => {
            let len = read_u32_le(data, offset)? as usize;
            let bytes = &data[offset + 4..offset + 4 + len];
            let s = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
            Ok((Value::String(s.to_string()), 4 + len))
        }
        IdlType::Bytes => {
            let len = read_u32_le(data, offset)? as usize;
            let bytes = &data[offset + 4..offset + 4 + len];
            Ok((json!(bytes), 4 + len)) // or base64 encode
        }
        IdlType::Pubkey => {
            let key_bytes = &data[offset..offset + 32];
            let pubkey = bs58::encode(key_bytes).into_string();
            Ok((Value::String(pubkey), 32))
        }

        // === Containers (5-15 lines each) ===
        IdlType::Option(inner) => {
            match data[offset] {
                0 => Ok((Value::Null, 1)),
                1 => {
                    let (val, consumed) = decode(data, offset + 1, inner, reg)?;
                    Ok((val, 1 + consumed))
                }
                v => Err(DecodeError::InvalidOptionTag(v)),
            }
        }
        IdlType::COption(inner) => {
            let tag = read_u32_le(data, offset)?;
            let inner_size = type_size(inner, reg)?;
            match tag {
                0 => Ok((Value::Null, 4 + inner_size)),
                1 => {
                    let (val, _) = decode(data, offset + 4, inner, reg)?;
                    Ok((val, 4 + inner_size))
                }
                _ => Err(DecodeError::InvalidOptionTag(tag as u8)),
            }
        }
        IdlType::Vec(inner) => {
            let count = read_u32_le(data, offset)? as usize;
            let mut pos = offset + 4;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                let (val, consumed) = decode(data, pos, inner, reg)?;
                arr.push(val);
                pos += consumed;
            }
            Ok((Value::Array(arr), pos - offset))
        }
        IdlType::Array(inner, len) => {
            let mut pos = offset;
            let mut arr = Vec::with_capacity(*len);
            for _ in 0..*len {
                let (val, consumed) = decode(data, pos, inner, reg)?;
                arr.push(val);
                pos += consumed;
            }
            Ok((Value::Array(arr), pos - offset))
        }

        // === Collections (10-20 lines each) ===
        IdlType::HashMap(key_ty, val_ty) | IdlType::BTreeMap(key_ty, val_ty) => {
            let count = read_u32_le(data, offset)? as usize;
            let mut pos = offset + 4;
            let mut map = serde_json::Map::new();
            for _ in 0..count {
                let (k, kc) = decode(data, pos, key_ty, reg)?;
                pos += kc;
                let (v, vc) = decode(data, pos, val_ty, reg)?;
                pos += vc;
                map.insert(value_to_string_key(&k), v);
            }
            Ok((Value::Object(map), pos - offset))
        }
        IdlType::HashSet(inner) | IdlType::BTreeSet(inner) => {
            let count = read_u32_le(data, offset)? as usize;
            let mut pos = offset + 4;
            let mut arr = Vec::with_capacity(count);
            for _ in 0..count {
                let (val, consumed) = decode(data, pos, inner, reg)?;
                arr.push(val);
                pos += consumed;
            }
            Ok((Value::Array(arr), pos - offset))
        }

        // === Compound types (15-30 lines) ===
        IdlType::Defined { name, generics } => {
            let typedef = reg.types.get(name)
                .ok_or_else(|| DecodeError::UnknownType(name.clone()))?;
            match typedef {
                IdlTypeDefBody::Struct { fields } => {
                    let mut pos = offset;
                    let mut map = serde_json::Map::new();
                    for (field_name, field_type) in fields {
                        let resolved = resolve_generics(field_type, generics)?;
                        let (val, consumed) = decode(data, pos, &resolved, reg)?;
                        map.insert(field_name.clone(), val);
                        pos += consumed;
                    }
                    Ok((Value::Object(map), pos - offset))
                }
                IdlTypeDefBody::Enum { variants } => {
                    let variant_idx = data[offset] as usize;
                    if variant_idx >= variants.len() {
                        return Err(DecodeError::VariantIndexOutOfBounds {
                            index: variant_idx as u8,
                            max: variants.len(),
                        });
                    }
                    let variant = &variants[variant_idx];
                    let mut pos = offset + 1;
                    let payload = match &variant.fields {
                        None | Some(IdlDefinedFields::Unit) => Value::Object(Default::default()),
                        Some(IdlDefinedFields::Named(fields)) => {
                            let mut map = serde_json::Map::new();
                            for (fname, ftype) in fields {
                                let (val, consumed) = decode(data, pos, ftype, reg)?;
                                map.insert(fname.clone(), val);
                                pos += consumed;
                            }
                            Value::Object(map)
                        }
                        Some(IdlDefinedFields::Tuple(types)) => {
                            let mut arr = Vec::new();
                            for t in types {
                                let (val, consumed) = decode(data, pos, t, reg)?;
                                arr.push(val);
                                pos += consumed;
                            }
                            if arr.len() == 1 { arr.remove(0) } else { Value::Array(arr) }
                        }
                    };
                    Ok((json!({ &variant.name: payload }), pos - offset))
                }
            }
        }
        IdlType::Tuple(types) => {
            let mut pos = offset;
            let mut arr = Vec::with_capacity(types.len());
            for t in types {
                let (val, consumed) = decode(data, pos, t, reg)?;
                arr.push(val);
                pos += consumed;
            }
            Ok((Value::Array(arr), pos - offset))
        }
    }
}
```

---

### Complexity Breakdown

| Type Category                 | Types Covered                           | Lines of Code | Difficulty      | Notes                                                                     |
| ----------------------------- | --------------------------------------- | ------------- | --------------- | ------------------------------------------------------------------------- |
| Primitives                    | bool, u8-u128, i8-i128, f32, f64        | ~80           | Trivial         | Each is 2-5 lines. 17 primitive types. Mechanical.                        |
| Strings/Bytes                 | string, bytes                           | ~15           | Simple          | Length-prefixed, UTF-8 validation for string.                             |
| Keys                          | pubkey                                  | ~5            | Simple          | 32 bytes, base58 encode.                                                  |
| Containers                    | option, coption, vec, array, tuple      | ~60           | Simple-Moderate | COption is the tricky one (fixed-size allocation).                        |
| Collections                   | hashMap, bTreeMap, hashSet, bTreeSet    | ~40           | Moderate        | Map key serialization to JSON string keys.                                |
| Compound/Defined              | struct, defined type lookup             | ~40           | Moderate        | Type registry lookup + recursive field decode.                            |
| Enums                         | unit/tuple/struct variants              | ~50           | Moderate-Hard   | Three variant payload formats. Variant index bounds check.                |
| Generic resolution            | Defined types with generics             | ~40           | Moderate-Hard   | Substitute type params with concrete types.                               |
| **Subtotal: Core decoder**    |                                         | **~330**      |                 |                                                                           |
| Type registry builder         | Parse IDL JSON -> TypeRegistry          | ~80           | Simple-Moderate | Map IDL types array to HashMap. Handle both v0.29 and v0.30+ IDL formats. |
| Helper functions              | read_u16_le, read_u32_le, etc.          | ~50           | Trivial         | Bounds-checked byte reading.                                              |
| Error types + Display         | DecodeError enum                        | ~40           | Simple          |                                                                           |
| Account-level decode          | Discriminator handling, top-level entry | ~30           | Simple          | Skip 8-byte discriminator, look up account type.                          |
| Serialization options         | pubkey_as_base58, u64_as_string, etc.   | ~30           | Simple          | Post-processing or parameterized output.                                  |
| **Subtotal: Supporting code** |                                         | **~230**      |                 |                                                                           |
| Unit tests                    | Per-type + integration                  | ~400          | Moderate        | Need test vectors for every type. Can extract from borsh-rs test suite.   |
| **TOTAL**                     |                                         | **~960**      |                 |                                                                           |

---

### Effort Estimate

| Component                   | Days         | Notes                                                                |
| --------------------------- | ------------ | -------------------------------------------------------------------- |
| Core decoder function       | 1.5          | All type arms + recursive descent structure                          |
| Type registry + IDL parsing | 0.5          | Leverage `anchor-lang-idl-spec` or `solana_idl` crate for type defs  |
| COption + edge cases        | 0.5          | COption fixed-size, u128/u256 as string, NaN rejection               |
| Generic type resolution     | 0.5          | Substitute type params, handle nested generics                       |
| Error handling + safety     | 0.5          | Bounds checking, malformed data recovery, clear error messages       |
| Testing                     | 1.0          | Unit tests per type, integration tests with real IDLs + account data |
| **Total**                   | **4.5 days** | For one experienced Rust developer                                   |

**With buffer for unknowns:** 5-6 days is a safe estimate.

---

### Key Technical Risks

**1. Generic Type Resolution**

- **Risk:** Anchor IDL v0.30+ supports generics with both type and const parameters (e.g., `GenericStruct<u16, 4>`). Resolving nested generics (a generic struct containing another generic struct) requires recursive substitution.
- **Mitigation:** Generics are rare in practice for Solana programs. Implement basic single-level resolution first. The Anchor TS client's `resolveGenericArgs()` is a direct reference for the algorithm. Can defer full support and log warnings for unresolved generics.

**2. COption Fixed-Size Layout**

- **Risk:** COption always allocates full inner type size even for None. Requires knowing the serialized size of the inner type statically. If the inner type is variable-size (e.g., `COption<String>`), this breaks the fixed-size assumption.
- **Mitigation:** In practice, COption is used almost exclusively with `Pubkey` (32 bytes) in SPL Token programs. COption with variable-size types is not seen in the wild. Implement for fixed-size inner types, error on variable-size.

**3. IDL Version Compatibility**

- **Risk:** Two IDL formats exist (pre-0.30 "legacy" and v0.30+). Field names differ (e.g., `defined` is a string in legacy, an object in v0.30+). Account discriminators may or may not be in the IDL.
- **Mitigation:** Use `anchor-lang-idl-spec` crate which handles v0.30+ format. For legacy IDLs, use the `anchor idl convert` CLI command or the `solana-idl-converter` crate. Pick ONE format and document the requirement.

**4. Recursive / Cyclic Type Definitions**

- **Risk:** Type A contains field of type B, which contains field of type A. Infinite recursion in decoder.
- **Mitigation:** Cycles are only possible through indirection (`Box<T>`, `Option<T>`, `Vec<T>`) in Rust, which in Borsh means length-prefixed or tagged. A depth limit (e.g., 64 levels) catches pathological cases. In practice, Solana account data is flat or shallowly nested.

**5. Performance Under High Throughput**

- **Risk:** Recursive descent with `serde_json::Value` allocation for every field may be slow for high-throughput indexing (thousands of accounts/sec).
- **Mitigation:** Borsh decoding is inherently fast (no parsing, just memcpy with offsets). The bottleneck will be JSON serialization and database writes, not the decoder. Benchmarks of similar recursive descent parsers in Rust show they handle millions of operations/sec. The decoder itself is unlikely to be a bottleneck. Can optimize later with arena allocation if needed.

**6. Unknown / Unsupported IDL Types**

- **Risk:** Some programs may use types not covered by the standard IDL type system (e.g., raw `[u8; N]` without proper IDL annotation, custom serialization).
- **Mitigation:** Return `DecodeError::UnknownType` with the type name. Log and skip. The universal indexer can store raw base64 data for accounts it cannot decode.

---

### Reference Implementations

| Implementation                               | Language   | URL                                                                                                                                             | Relevance                                                                                                                                                                                                              |
| -------------------------------------------- | ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `borsh-serde-adapter` deserialize_adapter.rs | Rust       | [github.com/wkennedy/borsh-schema-utils](https://github.com/wkennedy/borsh-schema-utils)                                                        | **HIGH** - Proves the pattern. ~120 lines of recursive descent from BorshSchemaContainer to serde_json::Value. Nearly identical architecture to what we need, but driven by BorshSchemaContainer instead of IDL types. |
| sol-chainsaw (full crate)                    | Rust       | [crates.io/crates/sol-chainsaw](https://crates.io/crates/sol-chainsaw)                                                                          | **HIGH** - Exactly our use case (IDL-driven Solana account decoding). 2.2K SLoC. MIT licensed. Could fork/vendor. But depends on borsh 0.9.3 and solana-sdk 1.14 (very outdated).                                      |
| Anchor TS BorshAccountsCoder                 | TypeScript | [github.com/coral-xyz/anchor/.../coder/borsh/](https://github.com/coral-xyz/anchor/tree/master/ts/packages/anchor/src/coder/borsh)              | **HIGH** - Production-grade IDL-to-layout mapping. IdlCoder.typeDefLayout() maps every IDL type to a borsh decoder. Best reference for the type mapping logic, especially generics resolution.                         |
| Anchor TS IdlCoder.fieldLayout               | TypeScript | [github.com/coral-xyz/anchor/.../coder/borsh/idl.ts](https://github.com/coral-xyz/anchor/blob/master/ts/packages/anchor/src/coder/borsh/idl.ts) | **HIGH** - The actual IDL type -> borsh layout translation. Handles all primitives, option, vec, array, defined, generics. Direct port target.                                                                         |
| borsh-construct (Python)                     | Python     | [github.com/near/borsh-construct-py](https://github.com/near/borsh-construct-py)                                                                | **MEDIUM** - Shows dynamic schema-driven Borsh decoding pattern in Python. Built on `construct` library. Good conceptual model but not directly portable to Rust.                                                      |
| borsh-rs (official crate)                    | Rust       | [github.com/near/borsh-rs](https://github.com/near/borsh-rs)                                                                                    | **MEDIUM** - Source of truth for Borsh encoding rules. The `schema` module and `BorshSchemaContainer` are useful references but cannot be used directly for IDL-driven decoding.                                       |
| `solana_idl` / `anchor-lang-idl-spec`        | Rust       | [docs.rs/anchor-lang-idl-spec](https://docs.rs/anchor-lang-idl-spec)                                                                            | **HIGH** - Ready-made Rust types for IDL parsing. Use these directly instead of defining our own IdlType enum. Saves significant boilerplate.                                                                          |

---

### Recommendation

**If sol-chainsaw fails evaluation, building our own decoder is the correct Plan B.** Specifically:

1. **Do not build from scratch.** Port the logic from `borsh-serde-adapter`'s `deserialize_to_serde_json` function, replacing `BorshSchemaContainer` lookup with `IdlType` matching + `TypeRegistry` lookup.

2. **Use `anchor-lang-idl-spec`** (or `solana_idl`) for IDL type definitions. Do not re-invent the IDL type system.

3. **Reference Anchor's TypeScript IdlCoder** for the exact type mapping, especially for generics resolution and enum variant handling.

4. **Start with the 80% case:** structs with primitives, pubkeys, strings, options, vecs, and defined types. This covers the vast majority of Solana programs. Add collections, COption, generics, and tuples as needed.

5. **Total investment: 5-6 days** including tests. This is ~15% of the 3-4 week timeline. Acceptable for an insurance policy.

**Alternative approaches if even Plan B feels too risky:**

- **Fork sol-chainsaw:** MIT licensed, 2.2K SLoC. Update dependencies (borsh 0.9 -> 1.x, solana-sdk 1.14 -> 2.x). Higher risk due to dependency churn but lower initial effort.
- **Use the Anchor TS client from Rust via wasm-bindgen or subprocess:** Ugly but proven. Not recommended for performance reasons.
- **Use `borsh-serde-adapter` + generate BorshSchemaContainer from IDL:** Write a converter from IdlType -> BorshSchemaContainer, then use the existing deserializer. Moderate complexity (~200 lines for the converter) but ties us to BorshSchemaContainer's representation which may not perfectly model all IDL types.

---

### Sources

- [Borsh Official Specification](https://borsh.io/)
- [borsh-rs (Rust implementation)](https://github.com/near/borsh-rs)
- [borsh crate schema module](https://docs.rs/borsh/latest/borsh/schema/index.html)
- [borsh-serde-adapter crate](https://docs.rs/borsh-serde-adapter/latest/borsh_serde_adapter/)
- [borsh-schema-utils (GitHub)](https://github.com/wkennedy/borsh-schema-utils)
- [Borsh to JSON blog post (BryteLands)](https://www.brytelands.xyz/borsh_to_json/)
- [sol-chainsaw crate](https://crates.io/crates/sol-chainsaw)
- [sol-chainsaw docs](https://docs.rs/sol-chainsaw/latest/sol_chainsaw/)
- [sol-chainsaw on lib.rs](https://lib.rs/crates/sol-chainsaw)
- [Anchor IDL documentation](https://solana.com/developers/guides/advanced/idls)
- [Anchor IDL spec (anchor-lang-idl-spec)](https://docs.rs/anchor-lang-idl-spec)
- [Anchor TS coder source](https://github.com/coral-xyz/anchor/tree/master/ts/packages/anchor/src/coder/borsh)
- [solana_idl crate](https://docs.rs/solana_idl/latest/solana_idl/)
- [Anchor IDL format documentation](https://www.anchor-lang.com/docs/basics/idl)
- [Decoding Solana data accounts (chalda blog)](https://blog.chalda.cz/posts/decoding-solana-data/)
- [borsh-construct-py](https://github.com/near/borsh-construct-py)
- [borsh-python](https://github.com/whdev1/borsh-python)
- [Solana Borsh Decoder UI](https://borsh.m2.xyz/)
- [QuickNode: How to Deserialize Account Data](https://www.quicknode.com/guides/solana-development/accounts-and-data/how-to-deserialize-account-data-on-solana)
- [Helius: Deserializing Account Data](https://www.helius.dev/blog/solana-dev-101-deserializing-account-data-on-solana)
- [RareSkills: Anchor IDL](https://rareskills.io/post/anchor-idl)
