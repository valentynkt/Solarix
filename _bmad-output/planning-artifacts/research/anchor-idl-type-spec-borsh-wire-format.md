# Agent 1C: Anchor IDL Type Specification + Borsh Wire Format

**Date:** 2026-04-05
**Status:** Complete
**Scope:** Authoritative reference for ALL Anchor IDL type variants and their Borsh binary encoding

---

## 1. IDL Format Versions

### Current: v0.30+ (Anchor v0.30.0 and later, including v0.31+)

- Spec version tracked in `idl.metadata.spec` (e.g., `"0.1.0"`)
- Canonical definition: `anchor-lang-idl-spec` crate
- Source of truth: `anchor-lang-idl-spec/src/lib.rs` (502 lines)

### Legacy: v0.29 and earlier

- No `spec` field in metadata
- Different field naming, different top-level structure

### Detection Method

```
if idl.has("metadata") && idl.metadata.has("spec"):
    → v0.30+ format
elif idl.has("version") && idl.has("name") at top level:
    → legacy (v0.29) format
```

Additional signals:

- v0.30+ has `"address"` at top level; legacy has `"metadata": { "address": "..." }` or no address
- v0.30+ uses `"writable"` / `"signer"` in accounts; legacy uses `"isMut"` / `"isSigner"`
- v0.30+ includes `"discriminator"` arrays in instructions/accounts/events; legacy does not
- v0.30+ uses `"pubkey"` for PublicKey type; legacy uses `"publicKey"`

---

## 2. Top-Level IDL Structure (v0.30+)

```json
{
  "address": "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
  "metadata": {
    "name": "my_program",
    "version": "0.1.0",
    "spec": "0.1.0",
    "description": "optional",
    "repository": "optional",
    "dependencies": [],
    "contact": "optional",
    "deployments": {
      "mainnet": null,
      "devnet": "...",
      "testnet": null,
      "localnet": null
    }
  },
  "docs": [],
  "instructions": [
    /* IdlInstruction[] */
  ],
  "accounts": [
    /* IdlAccount[] */
  ],
  "events": [
    /* IdlEvent[] */
  ],
  "errors": [
    /* IdlErrorCode[] */
  ],
  "types": [
    /* IdlTypeDef[] */
  ],
  "constants": [
    /* IdlConst[] */
  ]
}
```

Notes:

- All optional arrays (`accounts`, `events`, `errors`, `types`, `constants`, `docs`) default to `[]` and are omitted from JSON when empty.
- `instructions` is the only required array.

---

## 3. Complete IdlType Reference (v0.30+ Rust Spec)

This is the EXACT enum from `anchor-lang-idl-spec`:

```rust
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum IdlType {
    Bool,
    U8, I8,
    U16, I16,
    U32, I32,
    F32,
    U64, I64,
    F64,
    U128, I128,
    U256, I256,
    Bytes,
    String,
    Pubkey,
    Option(Box<IdlType>),
    Vec(Box<IdlType>),
    Array(Box<IdlType>, IdlArrayLen),
    Defined { name: String, generics: Vec<IdlGenericArg> },
    Generic(String),
}
```

**Critical:** The Rust spec does NOT include `HashMap`, `BTreeMap`, `HashSet`, `BTreeSet`, `Tuple`, or `COption` as direct IdlType variants. These exist in the TypeScript client types (`idl.ts`) but are NOT in the Rust spec crate. The TS types add `IdlTypeCOption` for legacy/SPL compatibility.

### 3.1 TypeScript IdlType (superset of Rust spec)

The TS client types (`ts/packages/anchor/src/idl.ts`) define:

```typescript
export type IdlType =
  | "bool"
  | "u8"
  | "i8"
  | "u16"
  | "i16"
  | "u32"
  | "i32"
  | "f32"
  | "u64"
  | "i64"
  | "f64"
  | "u128"
  | "i128"
  | "u256"
  | "i256"
  | "bytes"
  | "string"
  | "pubkey"
  | IdlTypeOption // { option: IdlType }
  | IdlTypeCOption // { coption: IdlType }
  | IdlTypeVec // { vec: IdlType }
  | IdlTypeArray // { array: [IdlType, IdlArrayLen] }
  | IdlTypeDefined // { defined: { name: string, generics?: IdlGenericArg[] } }
  | IdlTypeGeneric; // { generic: string }
```

**For the decoder/DDL generator, support both the Rust spec types AND the TS COption type.** Real-world IDLs (especially those involving SPL Token program interop) may include `coption`.

---

## 4. Complete Type Reference Table

### 4.1 Primitive Types

| #   | IDL Type | IDL JSON Value | Borsh Encoding                    | Size (bytes) | Fixed? | Notes                |
| --- | -------- | -------------- | --------------------------------- | :----------: | :----: | -------------------- |
| 1   | bool     | `"bool"`       | 1 byte: `0x00`=false, `0x01`=true |      1       |  Yes   |                      |
| 2   | u8       | `"u8"`         | 1 byte, unsigned                  |      1       |  Yes   |                      |
| 3   | i8       | `"i8"`         | 1 byte, two's complement          |      1       |  Yes   |                      |
| 4   | u16      | `"u16"`        | 2 bytes, little-endian            |      2       |  Yes   |                      |
| 5   | i16      | `"i16"`        | 2 bytes, LE, two's complement     |      2       |  Yes   |                      |
| 6   | u32      | `"u32"`        | 4 bytes, little-endian            |      4       |  Yes   |                      |
| 7   | i32      | `"i32"`        | 4 bytes, LE, two's complement     |      4       |  Yes   |                      |
| 8   | f32      | `"f32"`        | 4 bytes, IEEE 754, LE             |      4       |  Yes   | NaN = error in Borsh |
| 9   | u64      | `"u64"`        | 8 bytes, little-endian            |      8       |  Yes   |                      |
| 10  | i64      | `"i64"`        | 8 bytes, LE, two's complement     |      8       |  Yes   |                      |
| 11  | f64      | `"f64"`        | 8 bytes, IEEE 754, LE             |      8       |  Yes   | NaN = error in Borsh |
| 12  | u128     | `"u128"`       | 16 bytes, little-endian           |      16      |  Yes   |                      |
| 13  | i128     | `"i128"`       | 16 bytes, LE, two's complement    |      16      |  Yes   |                      |
| 14  | u256     | `"u256"`       | 32 bytes, little-endian           |      32      |  Yes   | New in v0.30+        |
| 15  | i256     | `"i256"`       | 32 bytes, LE, two's complement    |      32      |  Yes   | New in v0.30+        |

### 4.2 String / Byte Types

| #   | IDL Type | IDL JSON Value | Borsh Encoding                              | Size (bytes) | Fixed? | Notes                    |
| --- | -------- | -------------- | ------------------------------------------- | :----------: | :----: | ------------------------ |
| 16  | string   | `"string"`     | 4-byte u32 LE length prefix + N bytes UTF-8 |    4 + N     |   No   |                          |
| 17  | bytes    | `"bytes"`      | 4-byte u32 LE length prefix + N bytes raw   |    4 + N     |   No   | Alias for `Vec<u8>`      |
| 18  | pubkey   | `"pubkey"`     | 32 bytes raw (Ed25519 public key)           |      32      |  Yes   | v0.29 used `"publicKey"` |

### 4.3 Container Types

| #   | IDL Type | IDL JSON Example                    | Borsh Encoding                                                  |     Size Formula     |      Fixed?      | Notes                                                                     |
| --- | -------- | ----------------------------------- | --------------------------------------------------------------- | :------------------: | :--------------: | ------------------------------------------------------------------------- |
| 19  | option   | `{"option": "u64"}`                 | 1-byte tag (`0x00`=None, `0x01`=Some) + conditional inner value | 1 + (0 or sizeof(T)) |        No        | Tag is u8                                                                 |
| 20  | coption  | `{"coption": "pubkey"}`             | 4-byte u32 LE tag (`0`=None, `1`=Some) + ALWAYS sizeof(T)       |    4 + sizeof(T)     |       Yes        | Fixed size! SPL Token uses this. Not in Rust IdlType spec but in TS types |
| 21  | vec      | `{"vec": "u8"}`                     | 4-byte u32 LE count + N x sizeof(T)                             |   4 + N\*sizeof(T)   |        No        |                                                                           |
| 22  | array    | `{"array": ["u8", 32]}`             | N x sizeof(T), NO length prefix                                 |    N \* sizeof(T)    | Yes (if T fixed) | Length known at compile time                                              |
| 23  | defined  | `{"defined": {"name": "MyStruct"}}` | Depends on referenced type definition                           |        Varies        |      Varies      | Resolve from `types` array                                                |
| 24  | generic  | `{"generic": "T"}`                  | N/A (must be resolved before decoding)                          |         N/A          |       N/A        | Only in type defs with generics                                           |

### 4.4 Collection Types (in TS / sol-chainsaw; NOT in Rust IdlType spec)

These types appear in older IDLs and the `sol-chainsaw` crate. They are valid Borsh types but the official `anchor-lang-idl-spec` Rust enum does not include them as direct variants. They would appear as `Defined` types referencing standard library types, or in older IDL formats.

| #   | IDL Type | IDL JSON Example                  | Borsh Encoding                                      |         Size Formula         | Fixed? | Notes                          |
| --- | -------- | --------------------------------- | --------------------------------------------------- | :--------------------------: | :----: | ------------------------------ |
| 25  | hashMap  | `{"hashMap": ["string", "u64"]}`  | u32 count + entries sorted lexicographically by key | 4 + N\*(sizeof(K)+sizeof(V)) |   No   | Entries sorted for determinism |
| 26  | bTreeMap | `{"bTreeMap": ["string", "u64"]}` | u32 count + entries in key order                    | 4 + N\*(sizeof(K)+sizeof(V)) |   No   | Same wire format as HashMap    |
| 27  | hashSet  | `{"hashSet": "pubkey"}`           | u32 count + elements sorted lexicographically       |       4 + N\*sizeof(T)       |   No   | Elements sorted                |
| 28  | bTreeSet | `{"bTreeSet": "u64"}`             | u32 count + elements in order                       |       4 + N\*sizeof(T)       |   No   | Same wire format as HashSet    |

### 4.5 Compound Types (via IdlTypeDefTy)

These are not IdlType enum variants; they appear in the `types` array as `IdlTypeDef` entries:

| #   | Type Kind             | IDL JSON                                                            | Borsh Encoding                                     | Notes         |
| --- | --------------------- | ------------------------------------------------------------------- | -------------------------------------------------- | ------------- |
| 29  | struct (named fields) | `{"kind": "struct", "fields": [{"name": "x", "type": "u64"}, ...]}` | Fields serialized in declaration order, no padding |               |
| 30  | struct (tuple fields) | `{"kind": "struct", "fields": ["u8", "u64"]}`                       | Elements serialized in order, no padding           | Tuple struct  |
| 31  | struct (unit)         | `{"kind": "struct"}`                                                | Zero bytes                                         | No fields     |
| 32  | enum                  | `{"kind": "enum", "variants": [...]}`                               | u8 variant index + variant payload                 | See Section 6 |
| 33  | type alias            | `{"kind": "type", "alias": "u64"}`                                  | Same as aliased type                               | Transparent   |

---

## 5. IDL JSON Representation Examples

### 5.1 Primitive field

```json
{ "name": "count", "type": "u64" }
```

### 5.2 Option field

```json
{ "name": "maybe_owner", "type": { "option": "pubkey" } }
```

### 5.3 Vec field

```json
{ "name": "data", "type": { "vec": "u8" } }
```

### 5.4 Fixed array

```json
{ "name": "hash", "type": { "array": ["u8", 32] } }
```

### 5.5 Array with generic length

```json
{ "name": "items", "type": { "array": ["u64", { "generic": "N" }] } }
```

### 5.6 Nested option of vec

```json
{ "name": "tags", "type": { "option": { "vec": "string" } } }
```

### 5.7 Defined type reference (no generics)

```json
{ "name": "config", "type": { "defined": { "name": "Config" } } }
```

### 5.8 Defined type reference (with generics)

```json
{
  "name": "pool",
  "type": {
    "defined": {
      "name": "Pool",
      "generics": [
        { "kind": "type", "type": "pubkey" },
        { "kind": "const", "value": "32" }
      ]
    }
  }
}
```

### 5.9 COption (TS IDL only)

```json
{ "name": "freeze_authority", "type": { "coption": "pubkey" } }
```

### 5.10 Full struct type definition

```json
{
  "name": "GameAccount",
  "type": {
    "kind": "struct",
    "fields": [
      { "name": "player", "type": "pubkey" },
      { "name": "score", "type": "u64" },
      { "name": "level", "type": "u8" }
    ]
  }
}
```

### 5.11 Enum with all variant kinds

```json
{
  "name": "Action",
  "type": {
    "kind": "enum",
    "variants": [
      { "name": "Idle" },
      { "name": "Move", "fields": ["i32", "i32"] },
      {
        "name": "Attack",
        "fields": [
          { "name": "target", "type": "pubkey" },
          { "name": "damage", "type": "u64" }
        ]
      }
    ]
  }
}
```

### 5.12 Type with generics definition

```json
{
  "name": "Wrapper",
  "generics": [
    { "kind": "type", "name": "T" },
    { "kind": "const", "name": "N", "type": "usize" }
  ],
  "type": {
    "kind": "struct",
    "fields": [
      { "name": "data", "type": { "generic": "T" } },
      { "name": "items", "type": { "array": ["u8", { "generic": "N" }] } }
    ]
  }
}
```

### 5.13 Type with serialization annotation

```json
{
  "name": "ZeroCopyAccount",
  "serialization": "bytemuck",
  "repr": { "kind": "c" },
  "type": {
    "kind": "struct",
    "fields": [{ "name": "value", "type": "u64" }]
  }
}
```

---

## 6. Discriminator System

### 6.1 Discriminator Table

| Type        | Hash Input Format            | Hash Function |    Discriminator Size     | Location in Data                  |
| ----------- | ---------------------------- | :-----------: | :-----------------------: | --------------------------------- |
| Instruction | `"global:<snake_case_name>"` |    SHA-256    | 8 bytes (first 8 of hash) | First 8 bytes of instruction data |
| Account     | `"account:<PascalCaseName>"` |    SHA-256    | 8 bytes (first 8 of hash) | First 8 bytes of account data     |
| Event       | `"event:<PascalCaseName>"`   |    SHA-256    | 8 bytes (first 8 of hash) | First 8 bytes of event data       |

### 6.2 Discriminator Examples

```
Instruction "initialize":
  input:  "global:initialize"
  sha256: afaf6d1f0d989bed...
  disc:   [175, 175, 109, 31, 13, 152, 155, 237]

Account "Counter":
  input:  "account:Counter"
  sha256: ffb004f5bcfd7c19...
  disc:   [255, 176, 4, 245, 188, 253, 124, 25]
```

### 6.3 Discriminator in IDL JSON

In v0.30+ IDLs, discriminators are pre-computed and stored:

```json
{
  "instructions": [{
    "name": "initialize",
    "discriminator": [175, 175, 109, 31, 13, 152, 155, 237],
    ...
  }],
  "accounts": [{
    "name": "Counter",
    "discriminator": [255, 176, 4, 245, 188, 253, 124, 25]
  }],
  "events": [{
    "name": "TransferEvent",
    "discriminator": [12, 45, 67, 89, ...]
  }]
}
```

### 6.4 Discriminator Routing Logic

```
given raw_data (instruction data or account data):
  disc = raw_data[0..8]
  for each instruction/account/event in IDL:
    if entry.discriminator == disc:
      decode raw_data[8..] using entry's type definition
      break
```

### 6.5 No Other Discriminator Types

Anchor uses only these three discriminator categories (instruction, account, event). There are no separate discriminator types for errors, constants, or type definitions. Errors use numeric codes; constants are compile-time values; type definitions are referenced by name.

---

## 7. Borsh Wire Format Deep Dive

### 7.1 Core Encoding Rules

**Borsh is packed.** There is zero padding, zero alignment. Every byte carries data. The decoder must know the schema to parse the byte stream.

**Byte order:** ALL multi-byte values are little-endian.

**Deterministic:** Borsh produces a bijective mapping: each object has exactly one binary representation. This is critical for hash-based discriminators.

### 7.2 Encoding Pseudocode by Type

```
bool:       [val as u8]                              // 0x00 or 0x01
u8:         [val]                                     // 1 byte
u16:        little_endian(val, 2)                     // 2 bytes
u32:        little_endian(val, 4)                     // 4 bytes
u64:        little_endian(val, 8)                     // 8 bytes
u128:       little_endian(val, 16)                    // 16 bytes
u256:       little_endian(val, 32)                    // 32 bytes
i8-i256:    same sizes as unsigned, two's complement
f32:        assert(!NaN); little_endian(val as u32, 4)  // IEEE 754
f64:        assert(!NaN); little_endian(val as u64, 8)  // IEEE 754

string:     [u32_le(byte_count)] [utf8_bytes...]
bytes:      [u32_le(byte_count)] [raw_bytes...]       // Same as Vec<u8>
pubkey:     [32 raw bytes]                             // Ed25519, no prefix

Option<T>:  None  → [0x00]
            Some  → [0x01] [encode(value)]

COption<T>: None  → [u32_le(0)] [zero_bytes(sizeof(T))]   // ALWAYS full size
            Some  → [u32_le(1)] [encode(value)]

Vec<T>:     [u32_le(count)] [encode(elem_0)] [encode(elem_1)] ...
[T; N]:     [encode(elem_0)] [encode(elem_1)] ... [encode(elem_{N-1})]  // NO prefix

struct:     [encode(field_0)] [encode(field_1)] ...    // Declaration order
tuple:      [encode(elem_0)] [encode(elem_1)] ...     // Same as struct

enum:       [u8(variant_index)] [encode(variant_payload)]
  unit:     [u8(index)]                                // No payload
  tuple:    [u8(index)] [encode(elem_0)] [encode(elem_1)] ...
  struct:   [u8(index)] [encode(field_0)] [encode(field_1)] ...

HashMap:    [u32_le(count)] [encode(k_0)][encode(v_0)] [encode(k_1)][encode(v_1)] ...
            Entries sorted lexicographically by serialized key bytes
HashSet:    [u32_le(count)] [encode(elem_0)] [encode(elem_1)] ...
            Elements sorted lexicographically by serialized bytes
BTreeMap:   Same wire format as HashMap (already sorted)
BTreeSet:   Same wire format as HashSet (already sorted)
```

### 7.3 Size Formulas

```
fixed_size(bool)     = 1
fixed_size(u8)       = 1
fixed_size(u16)      = 2
fixed_size(u32)      = 4
fixed_size(u64)      = 8
fixed_size(u128)     = 16
fixed_size(u256)     = 32
fixed_size(i*)       = same as corresponding u*
fixed_size(f32)      = 4
fixed_size(f64)      = 8
fixed_size(pubkey)   = 32
fixed_size([T; N])   = N * fixed_size(T)          // only if T is fixed
fixed_size(COption<T>) = 4 + fixed_size(T)        // ALWAYS this size

variable_size(string)    = 4 + len(utf8_bytes)
variable_size(bytes)     = 4 + len(bytes)
variable_size(Vec<T>)    = 4 + count * size_of_each(T)
variable_size(Option<T>) = 1 + (if Some: size(value), else: 0)
variable_size(HashMap)   = 4 + sum(size(k_i) + size(v_i))
variable_size(HashSet)   = 4 + sum(size(elem_i))

size(struct)  = sum(size(field_i))
size(enum)    = 1 + size(active_variant_payload)
size(tuple)   = sum(size(elem_i))
```

---

## 8. Enum Encoding Detail

### 8.1 Variant Index

The variant index is a **u8** (0-255), assigned sequentially in declaration order. This limits enums to 256 variants maximum.

### 8.2 Unit Variant

```
Variant index only, no payload.
Rust:  enum Color { Red, Green, Blue }
Borsh: Red = [0x00], Green = [0x01], Blue = [0x02]
IDL:   { "name": "Red" }   (no "fields" key)
```

### 8.3 Tuple Variant

```
Variant index + each field serialized in order.
Rust:  enum Shape { Point, Circle(f64), Rect(f64, f64) }
Borsh: Point = [0x00]
       Circle(3.0) = [0x01] [little_endian(3.0 as u64)]
       Rect(1.0, 2.0) = [0x02] [le(1.0)] [le(2.0)]
IDL:   { "name": "Circle", "fields": ["f64"] }
       { "name": "Rect", "fields": ["f64", "f64"] }
```

### 8.4 Struct Variant

```
Variant index + named fields serialized in declaration order.
Rust:  enum Event { Transfer { from: Pubkey, to: Pubkey, amount: u64 } }
Borsh: [0x00] [32 bytes from] [32 bytes to] [8 bytes amount]
IDL:   { "name": "Transfer", "fields": [
          {"name": "from", "type": "pubkey"},
          {"name": "to", "type": "pubkey"},
          {"name": "amount", "type": "u64"}
       ]}
```

### 8.5 Distinguishing Named vs Tuple Fields in IDL JSON

```json
// Named fields (struct variant): array of objects with "name" and "type"
"fields": [{"name": "x", "type": "u32"}, {"name": "y", "type": "u32"}]

// Tuple fields: array of bare IdlType values
"fields": ["u32", "u32"]

// No fields: "fields" key is absent (unit variant)
```

The Rust spec uses `#[serde(untagged)]` for `IdlDefinedFields`:

- If JSON is an array of objects with `name`/`type` -> `Named(Vec<IdlField>)`
- If JSON is an array of strings/objects (IdlType) -> `Tuple(Vec<IdlType>)`

---

## 9. Nested / Defined Type Resolution

### 9.1 How "defined" References Work

When a field has type `{"defined": {"name": "MyStruct"}}`, the decoder must:

1. Search the IDL's `types` array for an entry where `name == "MyStruct"`
2. Read the `type` field of that entry (struct/enum/alias)
3. Recursively decode using that type's field definitions

### 9.2 Generic Resolution

When a defined type has generics:

```json
{
  "defined": {
    "name": "Pool",
    "generics": [{ "kind": "type", "type": "pubkey" }]
  }
}
```

The decoder must:

1. Find `Pool` in the `types` array
2. Check its `generics` definition: `[{"kind": "type", "name": "T"}]`
3. Substitute every `{"generic": "T"}` occurrence with `"pubkey"`
4. Then decode with the resolved types

### 9.3 Type Alias Resolution

When the type definition is `{"kind": "type", "alias": "u64"}`, the defined type is transparent -- decode it as the aliased type directly.

### 9.4 Resolution Algorithm

```
fn resolve_type(idl_type, types_map, generic_bindings):
  match idl_type:
    primitive/pubkey/bytes/string → return idl_type
    option(inner) → return option(resolve(inner))
    vec(inner)    → return vec(resolve(inner))
    array(inner, len) → return array(resolve(inner), resolve_len(len))
    generic(name) → return generic_bindings[name]
    defined(name, generics):
      type_def = types_map[name]
      resolved_generics = zip(type_def.generics, generics)
      new_bindings = merge(generic_bindings, resolved_generics)
      match type_def.ty:
        struct → decode fields with new_bindings
        enum   → decode variant index, then variant fields with new_bindings
        alias  → resolve(type_def.alias, new_bindings)
```

---

## 10. IdlTypeDef Full Structure

```rust
pub struct IdlTypeDef {
    pub name: String,
    pub docs: Vec<String>,                    // default: []
    pub serialization: IdlSerialization,       // default: Borsh
    pub repr: Option<IdlRepr>,                // None unless #[repr(...)]
    pub generics: Vec<IdlTypeDefGeneric>,     // default: []
    pub ty: IdlTypeDefTy,                     // struct | enum | type
}

pub enum IdlSerialization {
    Borsh,              // default
    Bytemuck,           // zero-copy
    BytemuckUnsafe,     // zero-copy unsafe
    Custom(String),     // custom serializer name
}

pub enum IdlRepr {
    Rust(IdlReprModifier),        // #[repr(Rust)]
    C(IdlReprModifier),           // #[repr(C)]
    Transparent,                  // #[repr(transparent)]
}

pub struct IdlReprModifier {
    pub packed: bool,     // #[repr(packed)]
    pub align: Option<usize>,  // #[repr(align(N))]
}

pub enum IdlTypeDefGeneric {
    Type { name: String },                      // e.g., T
    Const { name: String, ty: String },         // e.g., const N: usize
}
```

**Decoder implications:** If `serialization != Borsh`, the standard Borsh decoder cannot be used. The DDL generator should still work (type structure is the same), but the decoder must dispatch to the appropriate deserializer.

---

## 11. Supporting IDL Structures

### 11.1 IdlInstruction

```json
{
  "name": "transfer",
  "docs": ["Transfer tokens between accounts"],
  "discriminator": [163, 52, 200, 231, 140, 3, 69, 186],
  "accounts": [
    { "name": "from", "writable": true, "signer": true },
    { "name": "to", "writable": true },
    { "name": "authority", "signer": true },
    {
      "name": "token_program",
      "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
    }
  ],
  "args": [{ "name": "amount", "type": "u64" }],
  "returns": null
}
```

### 11.2 IdlInstructionAccountItem

Can be either:

- **Single account:** `{ name, docs, writable, signer, optional, address, pda, relations }`
- **Composite (nested group):** `{ name, accounts: [IdlInstructionAccountItem...] }`

Deserialization uses `#[serde(untagged)]` -- try Composite first, then Single.

### 11.3 IdlAccount / IdlEvent

```json
{ "name": "Counter", "discriminator": [255, 176, 4, 245, 188, 253, 124, 25] }
{ "name": "TransferEvent", "discriminator": [12, 45, 67, 89, 101, 23, 45, 67] }
```

### 11.4 IdlErrorCode

```json
{
  "code": 6000,
  "name": "InsufficientFunds",
  "msg": "Insufficient funds for transfer"
}
```

### 11.5 IdlConst

```json
{ "name": "MAX_SUPPLY", "type": "u64", "value": "1000000000" }
```

---

## 12. v0.29 to v0.30+ Migration Reference

| Aspect                          | v0.29 (Legacy)                       | v0.30+ (New)                                     |
| ------------------------------- | :----------------------------------- | :----------------------------------------------- |
| **Top-level address**           | Not present or in `metadata.address` | `"address": "..."` at root                       |
| **Metadata**                    | `"name"`, `"version"` at root        | `"metadata": { "name", "version", "spec" }`      |
| **Spec version**                | Not present                          | `"metadata.spec": "0.1.0"`                       |
| **Account mutability**          | `"isMut": true/false`                | `"writable": true` (omitted if false)            |
| **Account signer**              | `"isSigner": true/false`             | `"signer": true` (omitted if false)              |
| **PublicKey type**              | `"publicKey"`                        | `"pubkey"`                                       |
| **Account names**               | camelCase (`"systemProgram"`)        | snake_case (`"system_program"`)                  |
| **Enum variant fields**         | snake_case                           | camelCase                                        |
| **Discriminators**              | Not in IDL (computed at runtime)     | Pre-computed in IDL as byte arrays               |
| **Program address in accounts** | Name-based resolution                | Explicit `"address"` field                       |
| **Type definitions**            | `"kind": "struct"/"enum"` same       | Same structure, but additional features          |
| **Generics support**            | Not supported                        | Full support via `IdlTypeDefGeneric`             |
| **u256 / i256**                 | Not supported                        | Supported                                        |
| **Serialization field**         | Not present (assumed Borsh)          | `"serialization": "borsh"/"bytemuck"/...`        |
| **Repr field**                  | Not present                          | `"repr": { "kind": "c"/"rust"/"transparent" }`   |
| **Type alias**                  | Not supported                        | `{"kind": "type", "alias": IdlType}`             |
| **Optional fields**             | Always present                       | Omitted when default (serde skip_serializing_if) |
| **Conversion tool**             | N/A                                  | `anchor idl convert --program-id <ADDR>`         |
| **Auto-conversion**             | N/A                                  | v0.31+ auto-converts legacy IDLs in CLI commands |

---

## 13. COption Deep Dive

### 13.1 What is COption?

`COption<T>` is Solana's C-compatible option type from `solana_program::program_option::COption`. It is NOT a standard Borsh type -- it uses a different encoding than `Option<T>`.

### 13.2 Encoding Comparison

```
Option<Pubkey>:
  None:  [0x00]                        → 1 byte
  Some:  [0x01] [32 bytes pubkey]      → 33 bytes
  VARIABLE SIZE

COption<Pubkey>:
  None:  [0x00, 0x00, 0x00, 0x00] [0x00 x 32]  → 36 bytes
  Some:  [0x01, 0x00, 0x00, 0x00] [32 bytes]    → 36 bytes
  FIXED SIZE (always 4 + sizeof(T))
```

### 13.3 Why COption Exists

SPL Token program accounts (Mint, TokenAccount) use `COption<Pubkey>` for optional authorities because:

1. They need C-compatible layout (`#[repr(C)]`)
2. They use fixed-size fields for efficient random access
3. They predate Anchor and use bincode/Pack, not Borsh

### 13.4 COption in the IDL

- The **Rust** `anchor-lang-idl-spec` IdlType does NOT have a COption variant
- The **TypeScript** `idl.ts` DOES have `IdlTypeCOption = { coption: IdlType }`
- COption appears in IDLs generated by tools like `native-to-anchor` for SPL programs
- The decoder MUST support it even though it is not in the Rust spec

### 13.5 Decoder Implementation

```
fn decode_coption(data, inner_type):
  tag = read_u32_le(data[0..4])
  if tag == 0:
    advance(4 + fixed_size(inner_type))  // skip zero bytes
    return None
  elif tag == 1:
    value = decode(inner_type, data[4..])
    return Some(value)
  else:
    error("Invalid COption tag")
```

---

## 14. Edge Cases and Gotchas

### 14.1 No Padding/Alignment in Borsh

Borsh is packed. There is absolutely NO padding between fields, no alignment requirements. Every byte is meaningful data. This is different from C struct layout where fields may be padded.

**Exception:** Types with `serialization: "bytemuck"` and `repr: "c"` DO follow C alignment rules. The decoder must handle this differently.

### 14.2 Recursive/Nested Types

Anchor IDL allows nested type references (type A references type B), but actual recursive types (A contains A) are not supported in Borsh since there is no indirection (no pointers). The IDL spec uses `Box<IdlType>` in Rust for the enum representation, but this is implementation detail -- the serialized data itself cannot be recursive.

### 14.3 Enum Variant Limit

Borsh uses u8 for enum variant index, so enums are limited to 256 variants (0-255).

### 14.4 Vec/String Length Limit

The u32 length prefix means a maximum of 2^32 - 1 elements/bytes. In practice, Solana account data is limited to 10MB, so the actual limit is much lower.

### 14.5 Float NaN

Borsh spec requires NaN to produce a serialization error. When decoding, a NaN bit pattern in the data should be treated as an error or at least flagged.

### 14.6 Generic Type Resolution

Generic types (`{"generic": "T"}`) cannot be decoded directly -- they must be resolved against concrete type arguments before decoding. The decoder should never encounter an unresolved generic in actual account/instruction data.

### 14.7 IdlArrayLen::Generic

Array lengths can be generic constants (`{"generic": "N"}`). These must be resolved before the array size can be determined. In practice, concrete IDLs always have the generics resolved with actual values.

### 14.8 Empty Structs and Unit Types

Borsh encodes `()` (unit) as zero bytes. A struct with no fields also produces zero bytes. An enum unit variant is just the 1-byte index.

### 14.9 Multidimensional Arrays

Arrays can be nested: `[[u8; 16]; 32]` encodes as `{"array": [{"array": ["u8", 16]}, 32]}`. Wire format: 32 \* 16 = 512 bytes, no length prefixes at any level.

### 14.10 `#[non_exhaustive]` on IdlType

The IdlType enum is `#[non_exhaustive]`, meaning future Anchor versions may add new variants. The decoder should handle unknown types gracefully (log and skip, or error with context).

### 14.11 Defined Type Without Entry in Types Array

If a field references `{"defined": {"name": "ExternalType"}}` but there is no matching entry in the IDL's `types` array, this is an imported/external type. The decoder cannot decode it without additional information. This is a known limitation flagged in Anchor issue #1972.

### 14.12 Serialization != Borsh

When `serialization` is `"bytemuck"` or `"bytemuckUnsafe"`, the data is a direct memory-mapped C struct, not Borsh-encoded. The decoder must:

- Use C struct alignment rules
- Handle `repr(C)` padding
- Potentially handle `repr(packed)` which removes padding

### 14.13 CamelCase Conversion

Anchor's TS client converts all snake_case names to camelCase when loading IDLs. The Rust spec stores names as-is (snake_case for instructions/accounts, PascalCase for types). The decoder should normalize naming.

### 14.14 Discriminator Byte Order

Discriminators are stored as `Vec<u8>` (byte arrays), NOT as integers. `[175, 175, 109, 31, 13, 152, 155, 237]` means those exact bytes in that order.

### 14.15 Account Data Layout

For Anchor accounts: `[8-byte discriminator] [Borsh-encoded struct fields]`
For Anchor instruction data: `[8-byte discriminator] [Borsh-encoded args]`
For Anchor events: `[8-byte discriminator] [Borsh-encoded event fields]`

---

## 15. IdlArrayLen Variants

```rust
pub enum IdlArrayLen {
    Generic(String),   // const generic parameter name
    Value(usize),      // concrete numeric length
}
```

JSON representation:

- Concrete: `32` (bare number)
- Generic: `{"generic": "N"}` (object with "generic" key)

---

## 16. IdlGenericArg Variants

```rust
pub enum IdlGenericArg {
    Type { ty: IdlType },        // a type argument
    Const { value: String },     // a const value argument
}
```

JSON representation:

```json
{ "kind": "type", "type": "pubkey" }
{ "kind": "const", "value": "32" }
```

---

## 17. Quick Reference: Decoder Priority Types

For a Solana indexer, these are the types encountered in ~99% of real-world programs, ordered by frequency:

1. **u64** -- amounts, timestamps, counters
2. **pubkey** -- every account reference
3. **bool** -- flags
4. **u8** -- small enums, flags
5. **i64** -- signed amounts, timestamps
6. **string** -- names, URIs
7. **bytes** / `Vec<u8>` -- arbitrary data
8. **Option<T>** -- nullable fields
9. **Vec<T>** -- collections
10. **[T; N]** -- fixed arrays (common: `[u8; 32]`, `[u8; 64]`)
11. **Defined structs** -- nested account data
12. **Defined enums** -- state machines, variant configs
13. **u32** -- smaller counters
14. **u16** -- fees (basis points)
15. **u128** -- large amounts, sqrt price in DeFi
16. **COption<Pubkey>** -- SPL Token authorities

---

## Sources

- [anchor-lang-idl-spec source code (lib.rs)](https://docs.rs/anchor-lang-idl-spec/latest/src/anchor_lang_idl_spec/lib.rs.html)
- [anchor-lang-idl-spec API docs](https://docs.rs/anchor-lang-idl-spec/latest/anchor_lang_idl_spec/)
- [Anchor v0.30.0 Release Notes](https://www.anchor-lang.com/docs/updates/release-notes/0-30-0)
- [Anchor v0.30.1 Release Notes](https://www.anchor-lang.com/docs/updates/release-notes/0-30-1)
- [Anchor IDL Documentation](https://www.anchor-lang.com/docs/basics/idl)
- [Anchor TypeScript IDL types (v0.30.0)](https://github.com/coral-xyz/anchor/blob/v0.30.0/ts/packages/anchor/src/idl.ts)
- [Borsh Specification](https://borsh.io/)
- [Borsh GitHub Repository](https://github.com/near/borsh)
- [sol-chainsaw IdlType](https://docs.rs/sol-chainsaw/latest/sol_chainsaw/idl/enum.IdlType.html)
- [anchorpy-idl Rust source](https://github.com/kevinheavey/anchorpy-idl/blob/main/src/idl.rs)
- [Solana IDL Guide](https://solana.com/developers/guides/advanced/idls)
- [Anchor PR #2824 - IDL Rewrite](https://github.com/solana-foundation/anchor/pull/2824)
- [COption Issue #1581](https://github.com/coral-xyz/anchor/issues/1581)
- [Decoding Solana data accounts (chalda blog)](https://blog.chalda.cz/posts/decoding-solana-data/)
- [Anchor IDL and managing older versions (chalda blog)](https://blog.chalda.cz/posts/anchor-idl/)
