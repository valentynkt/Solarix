# Agent 2A: IDL to PostgreSQL DDL Mapping & Dynamic Schema Architecture

**Date:** 2026-04-05
**Status:** Complete (Enhanced)
**Scope:** Complete mapping from Anchor IDL types to PostgreSQL DDL, dynamic schema generation algorithm, table designs, naming conventions, index strategy, sqlx implementation patterns, performance projections, and schema evolution

---

## 1. Executive Summary

This document defines how Solarix transforms an arbitrary Anchor IDL into PostgreSQL CREATE TABLE statements at runtime. The mapping covers all 23 official IdlType variants plus 6 unofficial types (COption, HashMap, BTreeMap, HashSet, BTreeSet, Tuple). The design follows a **hybrid approach**: frequently-queried scalar fields become native PostgreSQL columns with proper types, while nested/complex structures are stored as JSONB with GIN indexes.

**Key decisions:**

| Decision                   | Choice                                            | Rationale                                                                                                       |
| -------------------------- | ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| Program isolation          | Schema-per-program                                | Clean namespace, no table name collisions, easy DROP SCHEMA CASCADE for cleanup                                 |
| Pubkey storage             | `VARCHAR(44)`                                     | Base58 string, human-readable, good enough for bounty; BYTEA is optimal for production scale                    |
| u64 handling               | `BIGINT` with application-layer overflow guard    | 99.9%+ of real-world u64 values fit in BIGINT; edge case handled by storing as string in JSONB `data` column    |
| Large integers (u128/u256) | `NUMERIC(39)` / `NUMERIC(78)`                     | Exact precision, native SQL math/aggregation; TEXT has no arithmetic                                            |
| Nested structs             | JSONB                                             | Recursive flattening is fragile; JSONB preserves structure, supports GIN queries                                |
| Enums                      | JSONB (not PG ENUM)                               | IDL enums have payloads (tuple/struct variants); PG ENUM only supports unit labels; also avoids ALTER TYPE pain |
| Vec/Array of primitives    | PostgreSQL native arrays                          | Compact storage, GIN-indexable, type-safe                                                                       |
| Vec/Array of complex types | JSONB                                             | Heterogeneous variant payloads cannot be PG arrays                                                              |
| Table-per-account-type     | Yes                                               | Each IDL account type gets its own table; enables typed columns and proper indexing                             |
| Table-per-instruction      | No (single table + JSONB args)                    | Instructions are high-cardinality events; one table with JSONB args is simpler and sufficient                   |
| DDL execution              | `sqlx::raw_sql()` for DDL, `QueryBuilder` for DML | `raw_sql` bypasses prepared statements (required for DDL); QueryBuilder for safe dynamic INSERTs                |
| Index strategy             | B-tree on scalar columns, GIN on JSONB payload    | B-tree for equality/range on common fields; GIN `jsonb_path_ops` for containment queries                        |

---

## 2. Complete Type Mapping Table

### 2.1 Primitive Types

| #   | IDL Type | PostgreSQL Type    | PG Size  | Rationale                                                                                                                                                                    |
| --- | -------- | ------------------ | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | `bool`   | `BOOLEAN`          | 1 byte   | Direct mapping, native PG type                                                                                                                                               |
| 2   | `u8`     | `SMALLINT`         | 2 bytes  | PG has no unsigned 1-byte int; SMALLINT (i16) is the smallest integer type. Range 0-255 fits.                                                                                |
| 3   | `i8`     | `SMALLINT`         | 2 bytes  | Same as u8; PG SMALLINT covers -128 to 127                                                                                                                                   |
| 4   | `u16`    | `INTEGER`          | 4 bytes  | SMALLINT max is 32,767 (signed); u16 goes to 65,535. Use INTEGER to avoid overflow.                                                                                          |
| 5   | `i16`    | `SMALLINT`         | 2 bytes  | SMALLINT covers -32,768 to 32,767, exact match                                                                                                                               |
| 6   | `u32`    | `BIGINT`           | 8 bytes  | INTEGER max is 2,147,483,647 (signed); u32 goes to 4,294,967,295. Use BIGINT.                                                                                                |
| 7   | `i32`    | `INTEGER`          | 4 bytes  | Direct mapping, exact range match                                                                                                                                            |
| 8   | `f32`    | `REAL`             | 4 bytes  | IEEE 754 single-precision, direct mapping                                                                                                                                    |
| 9   | `u64`    | `BIGINT`           | 8 bytes  | **See Section 2.6 for detailed analysis.** BIGINT covers 0 to 9.2e18; u64 max is 1.8e19. Values >i64::MAX are extremely rare in Solana (amounts, timestamps, slots all fit). |
| 10  | `i64`    | `BIGINT`           | 8 bytes  | Direct mapping, exact range match                                                                                                                                            |
| 11  | `f64`    | `DOUBLE PRECISION` | 8 bytes  | IEEE 754 double-precision, direct mapping                                                                                                                                    |
| 12  | `u128`   | `NUMERIC(39)`      | variable | PG has no native 128-bit integer. NUMERIC(39) stores up to 39 decimal digits (u128 max is 3.4e38 = 39 digits). Supports SUM/AVG/comparisons.                                 |
| 13  | `i128`   | `NUMERIC(39)`      | variable | Same as u128; signed range covered by NUMERIC                                                                                                                                |
| 14  | `u256`   | `NUMERIC(78)`      | variable | u256 max is 1.15e77 = 78 digits. NUMERIC(78) stores exactly. Standard blockchain practice (Ethereum uses same approach).                                                     |
| 15  | `i256`   | `NUMERIC(78)`      | variable | Same as u256; signed range covered                                                                                                                                           |

### 2.2 String / Byte / Key Types

| #   | IDL Type | PostgreSQL Type | Rationale                                                                                                                                                                                                      |
| --- | -------- | --------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 16  | `string` | `TEXT`          | Variable-length UTF-8; TEXT and VARCHAR are identical performance in PG                                                                                                                                        |
| 17  | `bytes`  | `BYTEA`         | Raw binary data; BYTEA is PG's native binary type                                                                                                                                                              |
| 18  | `pubkey` | `VARCHAR(44)`   | Base58-encoded Ed25519 key is 32-44 chars. VARCHAR(44) is human-readable for debugging/queries. For production at scale, BYTEA(32) is 27% more storage-efficient (as used by Solana's official Geyser plugin). |

**Pubkey rationale detail:** The official Solana AccountsDB Geyser plugin uses `BYTEA` for pubkeys. However, for the bounty/demo context, `VARCHAR(44)` is superior because: (a) base58 strings are directly readable in query results, logs, and API responses; (b) no encode/decode layer needed; (c) the decoder (chainparser) already outputs pubkeys as base58 strings. The 27% storage overhead is irrelevant at bounty scale.

### 2.3 Container Types

| #   | IDL Type             | PostgreSQL Type                  | Condition                              | Rationale                                                                                                                                                                          |
| --- | -------------------- | -------------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 19  | `Option<T>`          | `nullable column of T's PG type` | When T maps to a native PG type        | NULL semantics match Option::None perfectly. Column type follows inner T mapping.                                                                                                  |
| 19b | `Option<T>`          | `JSONB` (nullable)               | When T is complex (struct/enum/nested) | Inner type is JSONB anyway; nullable JSONB column                                                                                                                                  |
| 20  | `COption<T>`         | Same as `Option<T>`              | Always                                 | From DDL perspective, COption and Option are identical. The difference is only in Borsh wire format (fixed vs variable size). DDL cares about the logical value, not the encoding. |
| 21  | `Vec<T>`             | `T[]` (PG array)                 | When T is a primitive PG type          | PG native arrays are compact, GIN-indexable, and type-safe for homogeneous primitive lists                                                                                         |
| 21b | `Vec<T>`             | `JSONB`                          | When T is complex (struct/enum/nested) | PG arrays require homogeneous types; complex inner types have variable structure                                                                                                   |
| 22  | `[T; N]`             | Same as `Vec<T>`                 | Same conditions                        | Fixed-length arrays map identically from DDL perspective. Length is not enforced at DB level (could add CHECK constraint if needed).                                               |
| 23  | `Defined { struct }` | See Decision Tree (Section 8)    | Depends on depth/complexity            | Top-level struct fields can be flattened to columns; nested structs become JSONB                                                                                                   |
| 24  | `Defined { enum }`   | `JSONB`                          | Always                                 | Anchor enums can have variant payloads (tuple/struct); PG ENUM only supports labels. JSONB preserves the full `{"variant_name": {...payload}}` structure.                          |
| 25  | `Defined { alias }`  | Resolve to aliased type          | Transparent                            | Type alias is unwrapped; use the aliased type's PG mapping                                                                                                                         |
| 26  | `Generic(String)`    | N/A (resolve first)              | Must be resolved before DDL            | Generic types cannot appear in concrete account/instruction definitions. If encountered unresolved, skip with warning.                                                             |

### 2.4 Collection Types (Unofficial)

| #   | IDL Type        | PostgreSQL Type | Rationale                                    |
| --- | --------------- | --------------- | -------------------------------------------- |
| 27  | `HashMap<K,V>`  | `JSONB`         | Key-value maps are inherently JSON objects   |
| 28  | `BTreeMap<K,V>` | `JSONB`         | Same as HashMap from DDL perspective         |
| 29  | `HashSet<T>`    | `JSONB` (array) | Sets of values stored as JSON arrays         |
| 30  | `BTreeSet<T>`   | `JSONB` (array) | Same as HashSet from DDL perspective         |
| 31  | `Tuple`         | `JSONB` (array) | Unnamed ordered fields; JSON array of values |

### 2.5 Special Cases

| Case                            | PostgreSQL Handling                                                                                                                                                                                                       |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[u8; 32]` (hash/discriminator) | `BYTEA` -- recognized as a byte blob, not an array of integers. The decoder outputs this as a hex or base58 string; store as BYTEA via `decode(hex_string, 'hex')` or as `VARCHAR(64)` for hex, `VARCHAR(44)` for base58. |
| `[u8; 64]` (signature)          | `VARCHAR(88)` -- base58-encoded Ed25519 signature. Or `BYTEA` for raw storage.                                                                                                                                            |
| `Vec<u8>` (= `bytes`)           | `BYTEA` -- same as the `bytes` IDL type                                                                                                                                                                                   |
| `Vec<Pubkey>`                   | `VARCHAR(44)[]` -- PG array of base58 pubkey strings                                                                                                                                                                      |
| `Option<Pubkey>`                | `VARCHAR(44)` nullable -- NULL when None                                                                                                                                                                                  |
| `Vec<struct>`                   | `JSONB` -- array of objects                                                                                                                                                                                               |
| `Option<struct>`                | `JSONB` nullable                                                                                                                                                                                                          |
| Nested `Option<Option<T>>`      | `JSONB` -- multi-level optionality cannot map cleanly to SQL NULL                                                                                                                                                         |

### 2.6 The u64 BIGINT Question: Critical Analysis

This is the most debated mapping in the table and requires detailed justification.

**The problem:** PostgreSQL BIGINT is a signed 64-bit integer with range -9,223,372,036,854,775,808 to +9,223,372,036,854,775,807 (max ~9.2e18). Solana's u64 ranges from 0 to 18,446,744,073,709,551,615 (max ~1.8e19). Values above 9.2e18 will overflow BIGINT.

**Three options evaluated:**

| Approach                         | Pros                                                                             | Cons                                                                  |
| -------------------------------- | -------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| `BIGINT`                         | 50-70% faster than NUMERIC for aggregation; 8 bytes fixed; native CPU arithmetic | Values > i64::MAX overflow; need application-layer guard              |
| `NUMERIC(20)`                    | Covers full u64 range; exact arithmetic                                          | 50-70% slower at scale; variable-length overhead; ~12 bytes per value |
| Signed reinterpret (bitcast i64) | Same 8 bytes as BIGINT; no data loss                                             | Wrong values in DB; comparisons broken; confusing for users           |

**Decision: Use BIGINT with application-layer overflow guard.**

**Rationale:**

1. **Real-world Solana u64 values almost never exceed i64::MAX.** The common u64 fields in Solana programs are:
   - `lamports`: Max circulating supply ~5.6e17 lamports (fits in i64 by 16x)
   - `slot`: Current slot ~3e8, growing at ~2 slots/sec (will not exceed i64 for billions of years)
   - Token amounts: Even with 9 decimal places, 1 billion tokens = 1e18 (fits in i64)
   - Timestamps: Unix seconds ~1.7e9 (fits trivially)

2. **Performance difference is material at scale.** Benchmarks show NUMERIC is 50-70% slower for SUM/AVG aggregations (Xendit benchmark, 10M rows). Since the bounty requires aggregation/statistics queries, BIGINT gives significantly better performance.

3. **The JSONB `data` column is the safety net.** Even if a promoted u64 column is NULL due to overflow, the full decoded value is always available in the `data` JSONB column as a string. No data is lost.

**Implementation:**

```rust
fn safe_u64_to_pg(value: u64) -> PgValue {
    if value <= i64::MAX as u64 {
        PgValue::Bigint(value as i64)
    } else {
        // Overflow: don't promote to native column, log warning
        // The value is still preserved in the JSONB `data` column
        PgValue::Null
    }
}
```

**Alternative for production:** If the indexer is used with programs that routinely produce u64 values > i64::MAX, switch those specific columns to `NUMERIC(20)`. The DDL generator could accept a config override per field.

### 2.7 u128/u256 NUMERIC Sizing

**u128:** Maximum value = 340,282,366,920,938,463,463,374,607,431,768,211,455 (39 digits). Use `NUMERIC(39,0)`.

**u256:** Maximum value has 78 digits. Use `NUMERIC(78,0)`.

**Storage cost of NUMERIC:** 2 bytes per 4 decimal digits + 3-8 bytes overhead.

- NUMERIC(39): ~24 bytes per value
- NUMERIC(78): ~43 bytes per value

Compared to BIGINT at 8 bytes, this is 3-5x larger, but these types (u128/u256) are uncommon in Solana programs and there is no viable alternative. `postgres_web3` extension provides native uint256 but requires extension installation, which is not portable for bounty judging.

---

## 3. DDL Generation Algorithm

### 3.1 High-Level Flow

```
Input: Anchor IDL (v0.30+ format, already normalized)

Step 1: Create PostgreSQL schema for the program
  CREATE SCHEMA IF NOT EXISTS "{program_name}";

Step 2: Create metadata table (one per program)
  "{program_name}"._metadata

Step 3: Create indexing checkpoint table
  "{program_name}"._checkpoints

Step 4: For each account type in IDL:
  Generate CREATE TABLE for account state
  "{program_name}".{account_name}

Step 5: Create unified instructions table
  "{program_name}"._instructions

Step 6: Create indexes on all tables

Step 7: Store IDL hash in _metadata for change detection
```

### 3.2 Core Algorithm Pseudocode

```rust
fn generate_ddl(idl: &Idl) -> Vec<String> {
    let schema = sanitize_identifier(&idl.metadata.name);
    let mut statements = Vec::new();

    // Step 1: Schema
    statements.push(format!(
        "CREATE SCHEMA IF NOT EXISTS {}", quote_ident(&schema)
    ));

    // Step 2: Metadata table
    statements.push(format!(r#"
        CREATE TABLE IF NOT EXISTS {schema}._metadata (
            key         TEXT PRIMARY KEY,
            value       JSONB NOT NULL,
            updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
    "#));

    // Step 3: Checkpoints table
    statements.push(format!(r#"
        CREATE TABLE IF NOT EXISTS {schema}._checkpoints (
            checkpoint_type TEXT PRIMARY KEY,
            last_slot       BIGINT NOT NULL DEFAULT 0,
            last_signature  VARCHAR(88),
            details         JSONB,
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
    "#));

    // Step 4: Account tables
    let type_map = build_type_map(&idl.types);
    for account in &idl.accounts {
        let type_def = type_map.get(&account.name);
        let table_ddl = generate_account_table(
            &schema, &account, type_def, &type_map
        );
        statements.extend(table_ddl);
    }

    // Step 5: Instructions table
    statements.push(generate_instructions_table(&schema));

    // Step 6: Indexes
    statements.extend(generate_indexes(&schema, &idl));

    // Step 7: Store IDL hash
    let idl_hash = sha256(serde_json::to_string(&idl));
    statements.push(format!(r#"
        INSERT INTO {schema}._metadata (key, value)
        VALUES ('idl_hash', '"{idl_hash}"')
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
            updated_at = NOW()
    "#));

    statements
}

fn generate_account_table(
    schema: &str,
    account: &IdlAccount,
    type_def: Option<&IdlTypeDef>,
    type_map: &HashMap<String, IdlTypeDef>,
) -> Vec<String> {
    let table_name = to_snake_case(&account.name);
    let mut columns = Vec::new();

    // Common columns (every account table has these)
    columns.push("pubkey           VARCHAR(44) PRIMARY KEY".to_string());
    columns.push("slot             BIGINT NOT NULL".to_string());
    columns.push("write_version    BIGINT NOT NULL DEFAULT 0".to_string());
    columns.push("updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()".to_string());

    // Decoded payload (always present as JSONB for full data access)
    columns.push("data             JSONB NOT NULL".to_string());

    // Type-specific promoted columns
    if let Some(td) = type_def {
        if let IdlTypeDefTy::Struct { fields } = &td.ty {
            for field in fields {
                if let Some(pg_col) = try_promote_to_column(field, type_map) {
                    columns.push(pg_col);
                }
            }
        }
    }

    vec![format!(
        "CREATE TABLE IF NOT EXISTS {}.{} (\n  {}\n)",
        quote_ident(schema),
        quote_ident(&table_name),
        columns.join(",\n  ")
    )]
}

fn try_promote_to_column(
    field: &IdlField,
    type_map: &HashMap<String, IdlTypeDef>,
) -> Option<String> {
    let col_name = to_snake_case(&field.name);
    match map_idl_to_pg(&field.ty, type_map) {
        PgMapping::Native(pg_type, nullable) => {
            let null_str = if nullable { "" } else { " NOT NULL" };
            Some(format!("{} {}{}", quote_ident(&col_name), pg_type, null_str))
        }
        PgMapping::Jsonb => None, // Keep in JSONB payload only
    }
}
```

### 3.3 Type Mapping Function

```rust
enum PgMapping {
    Native(&'static str, bool), // (pg_type, is_nullable)
    Jsonb,
}

fn map_idl_to_pg(
    idl_type: &IdlType,
    type_map: &HashMap<String, IdlTypeDef>,
) -> PgMapping {
    match idl_type {
        IdlType::Bool => PgMapping::Native("BOOLEAN", false),
        IdlType::U8 | IdlType::I8 => PgMapping::Native("SMALLINT", false),
        IdlType::U16 => PgMapping::Native("INTEGER", false),
        IdlType::I16 => PgMapping::Native("SMALLINT", false),
        IdlType::U32 => PgMapping::Native("BIGINT", false),
        IdlType::I32 => PgMapping::Native("INTEGER", false),
        IdlType::F32 => PgMapping::Native("REAL", false),
        IdlType::U64 | IdlType::I64 => PgMapping::Native("BIGINT", false),
        IdlType::F64 => PgMapping::Native("DOUBLE PRECISION", false),
        IdlType::U128 | IdlType::I128 => PgMapping::Native("NUMERIC(39)", false),
        IdlType::U256 | IdlType::I256 => PgMapping::Native("NUMERIC(78)", false),
        IdlType::String => PgMapping::Native("TEXT", false),
        IdlType::Bytes => PgMapping::Native("BYTEA", false),
        IdlType::Pubkey => PgMapping::Native("VARCHAR(44)", false),

        IdlType::Option(inner) | IdlType::COption(inner) => {
            match map_idl_to_pg(inner, type_map) {
                PgMapping::Native(pg_type, _) => PgMapping::Native(pg_type, true),
                PgMapping::Jsonb => PgMapping::Jsonb,
            }
        }

        // Vec/Array of primitives -> PG array; complex -> JSONB
        IdlType::Vec(inner) | IdlType::Array(inner, _) => {
            match inner.as_ref() {
                // Special case: Vec<u8> and [u8; N] -> BYTEA
                IdlType::U8 => PgMapping::Native("BYTEA", false),
                _ => match map_idl_to_pg(inner, type_map) {
                    PgMapping::Native(pg_type, _) => {
                        // Return array type string
                        // Note: lifetime issue with format! - use a static lookup
                        pg_array_type(pg_type)
                    }
                    PgMapping::Jsonb => PgMapping::Jsonb,
                }
            }
        }

        // Defined types: resolve aliases, everything else -> JSONB
        IdlType::Defined { name, .. } => {
            if let Some(td) = type_map.get(name.as_str()) {
                match &td.ty {
                    IdlTypeDefTy::Alias(aliased) => map_idl_to_pg(aliased, type_map),
                    _ => PgMapping::Jsonb, // structs and enums -> JSONB
                }
            } else {
                PgMapping::Jsonb // unknown type -> JSONB fallback
            }
        }

        // Everything else -> JSONB
        _ => PgMapping::Jsonb,
    }
}

fn pg_array_type(scalar_type: &str) -> PgMapping {
    match scalar_type {
        "BOOLEAN" => PgMapping::Native("BOOLEAN[]", false),
        "SMALLINT" => PgMapping::Native("SMALLINT[]", false),
        "INTEGER" => PgMapping::Native("INTEGER[]", false),
        "BIGINT" => PgMapping::Native("BIGINT[]", false),
        "REAL" => PgMapping::Native("REAL[]", false),
        "DOUBLE PRECISION" => PgMapping::Native("DOUBLE PRECISION[]", false),
        "NUMERIC(39)" => PgMapping::Native("NUMERIC(39)[]", false),
        "NUMERIC(78)" => PgMapping::Native("NUMERIC(78)[]", false),
        "TEXT" => PgMapping::Native("TEXT[]", false),
        "VARCHAR(44)" => PgMapping::Native("VARCHAR(44)[]", false),
        _ => PgMapping::Jsonb, // Unknown scalar -> JSONB fallback
    }
}
```

### 3.4 Column Promotion Strategy

The algorithm promotes IDL fields to native PG columns only when they map to scalar types. This dual-storage approach (native columns + full JSONB) gives the best of both worlds:

**Native columns provide:**

- Type-safe queries (`WHERE amount > 1000`)
- Efficient B-tree indexes
- Proper sorting and aggregation
- Foreign key potential (pubkey references)

**JSONB `data` column provides:**

- Complete decoded data without loss
- GIN-indexed full-text search of nested fields
- No schema migration needed when fields are added
- API can return the full decoded object directly

**Example for a DeFi pool account:**

```
IDL struct Pool:
  owner: pubkey        -> PROMOTED to VARCHAR(44) column
  token_a_mint: pubkey -> PROMOTED to VARCHAR(44) column
  token_b_mint: pubkey -> PROMOTED to VARCHAR(44) column
  total_liquidity: u128 -> PROMOTED to NUMERIC(39) column
  fee_rate: u16        -> PROMOTED to INTEGER column
  is_active: bool      -> PROMOTED to BOOLEAN column
  config: Config       -> NOT promoted (struct -> JSONB only)
  rewards: Vec<Reward> -> NOT promoted (Vec<struct> -> JSONB only)
```

---

## 4. Table Schema Designs

### 4.1 Account State Table (per account type)

Each account type defined in the IDL gets its own table within the program's schema.

```sql
-- Example: for program "my_defi" with account type "Pool"
CREATE TABLE IF NOT EXISTS "my_defi"."pool" (
    -- === Common columns (every account table) ===
    pubkey              VARCHAR(44) PRIMARY KEY,
    slot                BIGINT NOT NULL,
    write_version       BIGINT NOT NULL DEFAULT 0,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- === Full decoded payload ===
    data                JSONB NOT NULL,

    -- === Promoted columns (from IDL struct fields) ===
    -- These are populated from the decoded JSON for fast queries.
    -- The `data` JSONB column still contains the full object.
    owner               VARCHAR(44),
    token_a_mint        VARCHAR(44),
    token_b_mint        VARCHAR(44),
    total_liquidity     NUMERIC(39),
    fee_rate            INTEGER,
    is_active           BOOLEAN
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_pool_slot
    ON "my_defi"."pool" (slot);
CREATE INDEX IF NOT EXISTS idx_pool_owner
    ON "my_defi"."pool" (owner);
CREATE INDEX IF NOT EXISTS idx_pool_token_a_mint
    ON "my_defi"."pool" (token_a_mint);
CREATE INDEX IF NOT EXISTS idx_pool_token_b_mint
    ON "my_defi"."pool" (token_b_mint);
CREATE INDEX IF NOT EXISTS idx_pool_data
    ON "my_defi"."pool" USING GIN (data jsonb_path_ops);
```

**Design rationale:**

- `pubkey` is PRIMARY KEY because each account has a unique on-chain address
- `slot` tracks when the account was last updated (used for ordering and gap detection)
- `write_version` enables optimistic concurrency (only update if version is newer)
- `data` JSONB holds the complete decoded struct as a JSON object
- Promoted columns are duplicated from `data` for efficient scalar queries
- The table stores only the LATEST state of each account (not history)

### 4.2 Instructions Table (single table per program)

```sql
CREATE TABLE IF NOT EXISTS "my_defi"."_instructions" (
    -- === Identity ===
    id                  BIGSERIAL PRIMARY KEY,
    signature           VARCHAR(88) NOT NULL,
    instruction_index   SMALLINT NOT NULL,
    inner_index         SMALLINT,  -- NULL for top-level, 0+ for CPI

    -- === Context ===
    slot                BIGINT NOT NULL,
    block_time          TIMESTAMPTZ,
    program_id          VARCHAR(44) NOT NULL,
    instruction_name    TEXT NOT NULL,

    -- === Decoded args ===
    args                JSONB,

    -- === Accounts involved ===
    accounts            JSONB NOT NULL,
    -- Format: [{"name": "from", "pubkey": "...", "writable": true, "signer": true}, ...]

    -- === Execution result ===
    success             BOOLEAN NOT NULL DEFAULT TRUE,
    error_message       TEXT,
    logs                TEXT[],

    -- === Metadata ===
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- === Uniqueness ===
    UNIQUE (signature, instruction_index, COALESCE(inner_index, -1))
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_instructions_slot
    ON "my_defi"."_instructions" (slot);
CREATE INDEX IF NOT EXISTS idx_instructions_name
    ON "my_defi"."_instructions" (instruction_name);
CREATE INDEX IF NOT EXISTS idx_instructions_block_time
    ON "my_defi"."_instructions" (block_time);
CREATE INDEX IF NOT EXISTS idx_instructions_signature
    ON "my_defi"."_instructions" (signature);
CREATE INDEX IF NOT EXISTS idx_instructions_args
    ON "my_defi"."_instructions" USING GIN (args jsonb_path_ops);
CREATE INDEX IF NOT EXISTS idx_instructions_accounts
    ON "my_defi"."_instructions" USING GIN (accounts jsonb_path_ops);
```

**Design rationale:**

- Single table for ALL instruction types (not table-per-instruction) because:
  - Instructions are append-only events (no updates)
  - Query patterns are typically "find all instructions of type X" or "find by signature"
  - A single table with `instruction_name` filter is simpler and sufficient
  - JSONB `args` column handles the variable structure across instruction types
  - The `accounts` JSONB preserves the labeled account list with writable/signer metadata
- `BIGSERIAL` id for fast sequential inserts
- Composite unique constraint prevents double-processing
- `inner_index` distinguishes top-level instructions from CPI (inner) instructions

**Why NOT table-per-instruction:**

- A program with 20 instruction types would generate 20 tables
- Each instruction is typically called rarely compared to account state updates
- Query performance on a single indexed table with JSONB args is adequate for the bounty
- Reduces DDL complexity significantly

### 4.3 Metadata Table

```sql
CREATE TABLE IF NOT EXISTS "my_defi"."_metadata" (
    key         TEXT PRIMARY KEY,
    value       JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed with program info
INSERT INTO "my_defi"."_metadata" (key, value) VALUES
    ('program_id', '"11111111111111111111111111111111"'),
    ('program_name', '"my_defi"'),
    ('idl_hash', '"abc123..."'),
    ('idl_version', '"0.1.0"'),
    ('idl_spec', '"0.1.0"'),
    ('schema_created_at', '"2026-04-05T12:00:00Z"'),
    ('account_types', '["Pool", "Position", "Config"]'),
    ('instruction_types', '["initialize", "swap", "add_liquidity"]')
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value, updated_at = NOW();
```

### 4.4 Checkpoints Table

```sql
CREATE TABLE IF NOT EXISTS "my_defi"."_checkpoints" (
    checkpoint_type TEXT PRIMARY KEY,       -- 'backfill', 'realtime', 'accounts'
    last_slot       BIGINT NOT NULL DEFAULT 0,
    last_signature  VARCHAR(88),
    details         JSONB,                  -- arbitrary checkpoint metadata
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

### 4.5 Global System Table (in public schema)

```sql
CREATE TABLE IF NOT EXISTS public._solarix_programs (
    program_id      VARCHAR(44) PRIMARY KEY,
    program_name    TEXT NOT NULL,
    schema_name     TEXT NOT NULL UNIQUE,
    idl_hash        TEXT NOT NULL,
    idl_source      TEXT NOT NULL,         -- 'onchain', 'file', 'bundled', 'manual'
    status          TEXT NOT NULL DEFAULT 'initializing',
                                            -- 'initializing', 'backfilling', 'realtime', 'paused', 'error'
    account_count   INTEGER DEFAULT 0,
    instruction_count INTEGER DEFAULT 0,
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

---

## 5. Naming Conventions

### 5.1 Schema Naming

```
Pattern: {sanitized_program_name}
Example: "my_defi_program" -> my_defi_program

Rules:
1. Take IDL's metadata.name field
2. Convert to snake_case (it usually already is)
3. Replace hyphens with underscores
4. Remove non-alphanumeric characters (except underscores)
5. Truncate to 63 characters (PG identifier limit)
6. If empty after sanitization, use "program_{first_8_of_program_id}"
```

### 5.2 Table Naming

```
Account tables:  {schema}.{account_type_name_snake_case}
Instructions:    {schema}._instructions
Metadata:        {schema}._metadata
Checkpoints:     {schema}._checkpoints

Examples:
  "my_defi"."pool"
  "my_defi"."user_position"
  "my_defi"."global_config"
  "my_defi"."_instructions"
  "my_defi"."_metadata"

Internal tables use underscore prefix (_instructions, _metadata, _checkpoints)
to visually distinguish them from account-type tables.
```

### 5.3 Column Naming

```
Pattern: {field_name_snake_case}

Rules:
1. IDL field names are already snake_case in v0.30+
2. If camelCase is encountered (legacy), convert to snake_case
3. Apply reserved word quoting (see 5.4)
```

### 5.4 Reserved Word and Collision Avoidance

PostgreSQL has ~130 reserved keywords (SELECT, TABLE, ORDER, etc.). IDL field names could collide.

**Strategy: Always double-quote all generated identifiers.**

This is the safest approach and has zero performance impact. PostgreSQL's `format()` function with `%I` specifier, or the application-layer equivalent, handles this automatically.

```sql
-- Always quote identifiers in generated DDL:
CREATE TABLE "my_program"."order" (       -- "order" is reserved
    "pubkey"    VARCHAR(44) PRIMARY KEY,
    "select"    BIGINT,                   -- "select" is reserved
    "from"      VARCHAR(44),              -- "from" is reserved
    "data"      JSONB NOT NULL
);
```

**Implementation in Rust:**

```rust
fn quote_ident(name: &str) -> String {
    // Escape any embedded double quotes, then wrap in double quotes
    format!("\"{}\"", name.replace('"', "\"\""))
}
```

### 5.5 Multiple Programs

Each program gets its own PostgreSQL schema. This provides complete isolation:

```
solarix database
  |-- "jupiter_v6" schema
  |     |-- pool
  |     |-- position
  |     |-- _instructions
  |     |-- _metadata
  |     |-- _checkpoints
  |
  |-- "raydium_clmm" schema
  |     |-- pool_state
  |     |-- position_state
  |     |-- _instructions
  |     |-- _metadata
  |     |-- _checkpoints
  |
  |-- "public" schema (Solarix system tables)
        |-- _solarix_programs (registry of indexed programs)
        |-- _solarix_config
```

**Benefits of schema-per-program:**

- No table name collisions between programs
- Easy cleanup: `DROP SCHEMA "jupiter_v6" CASCADE;`
- Clean `search_path` isolation per query context
- Programs can have identically-named account types without conflict
- PG schemas support up to thousands of schemas without performance issues

### 5.6 Index Naming

```
Pattern: idx_{table_short}_{column_name}

Rules:
1. Prefix all indexes with "idx_"
2. Use abbreviated table name (no schema prefix -- indexes are schema-scoped)
3. Truncate total identifier to 63 characters
4. For GIN indexes, suffix with "_gin" for clarity
5. For composite indexes, join column names with "_"

Examples:
  idx_pool_slot
  idx_pool_owner
  idx_pool_data_gin
  idx_ix_slot
  idx_ix_name
  idx_user_position_owner
```

---

## 6. GIN Index Strategy

### 6.1 Index Types and When to Use Them

| Index Type             | Use Case                                          | Columns                                                                    |
| ---------------------- | ------------------------------------------------- | -------------------------------------------------------------------------- |
| **B-tree** (default)   | Equality, range, ORDER BY on scalar columns       | `pubkey`, `slot`, `block_time`, `instruction_name`, promoted pubkey fields |
| **GIN jsonb_path_ops** | Containment queries (`@>`) on JSONB               | `data`, `args`, `accounts`                                                 |
| **GIN jsonb_ops**      | Existence queries (`?`, `?&`, `?\|`) on JSONB     | Only if needed for key-existence queries                                   |
| **Hash**               | Pure equality lookups (rarely better than B-tree) | Not recommended for this use case                                          |

### 6.2 Default Indexes per Table Type

**Account tables:**

```sql
-- Primary key (automatic B-tree)
PRIMARY KEY (pubkey)

-- Slot for ordering and gap detection
CREATE INDEX idx_{table}_slot ON {schema}.{table} (slot);

-- All promoted pubkey columns (for joins/lookups)
-- Generated dynamically for each VARCHAR(44) promoted column
CREATE INDEX idx_{table}_{col} ON {schema}.{table} ({col});

-- JSONB payload for containment queries
CREATE INDEX idx_{table}_data ON {schema}.{table}
    USING GIN (data jsonb_path_ops);
```

**Instructions table:**

```sql
-- Sequential scan avoidance
CREATE INDEX idx_ix_slot ON {schema}._instructions (slot);
CREATE INDEX idx_ix_name ON {schema}._instructions (instruction_name);
CREATE INDEX idx_ix_sig ON {schema}._instructions (signature);
CREATE INDEX idx_ix_time ON {schema}._instructions (block_time);

-- JSONB search
CREATE INDEX idx_ix_args ON {schema}._instructions
    USING GIN (args jsonb_path_ops);
CREATE INDEX idx_ix_accounts ON {schema}._instructions
    USING GIN (accounts jsonb_path_ops);
```

### 6.3 Why `jsonb_path_ops` over `jsonb_ops`

| Feature             | `jsonb_ops`                        | `jsonb_path_ops`          |
| ------------------- | ---------------------------------- | ------------------------- |
| Index size          | 60-80% of table                    | 20-30% of table           |
| Query performance   | Good                               | Better (for containment)  |
| Write overhead      | ~79% increase                      | ~16% increase             |
| Supported operators | `@>`, `?`, `?\|`, `?&`, `@?`, `@@` | `@>`, `@?`, `@@`          |
| Best for            | Exploratory key-existence queries  | Value-containment queries |

**Decision: Use `jsonb_path_ops` as the default.** Solarix queries are predominantly value-containment patterns (`WHERE data @> '{"owner": "..."}'`). Key-existence checks (`WHERE data ? 'some_field'`) are rare for indexed blockchain data.

If users need key-existence queries, they can be served via the `data` column's `->>` operator with B-tree expression indexes on specific paths.

### 6.4 Index Sizing Estimates

For 1 million rows with ~500 byte average JSONB documents:

| Index Type                    | Estimated Size |
| ----------------------------- | -------------- |
| B-tree on VARCHAR(44)         | ~35 MB         |
| GIN `jsonb_path_ops` on JSONB | ~2-10 MB       |
| GIN `jsonb_ops` on JSONB      | ~18-60 MB      |

At bounty demo scale (~10K-100K rows), index sizes are negligible. The `jsonb_path_ops` choice becomes important at production scale (10M+ rows) where 60MB vs 5MB index sizes affect memory pressure and cache hit rates.

### 6.5 Query Patterns and Index Usage

| Query Pattern                            | Uses Index?                  | Index Type               |
| ---------------------------------------- | ---------------------------- | ------------------------ |
| `WHERE pubkey = '...'`                   | Yes                          | B-tree (PK)              |
| `WHERE slot > X AND slot < Y`            | Yes                          | B-tree                   |
| `WHERE owner = '...'`                    | Yes                          | B-tree (promoted column) |
| `WHERE data @> '{"field": "value"}'`     | Yes                          | GIN jsonb_path_ops       |
| `WHERE data->>'field' = 'value'`         | No (unless expression index) | Seq scan                 |
| `WHERE (data->>'amount')::bigint > 1000` | No (unless expression index) | Seq scan                 |
| `WHERE instruction_name = 'swap'`        | Yes                          | B-tree                   |
| `WHERE args @> '{"direction": "AtoB"}'`  | Yes                          | GIN jsonb_path_ops       |

**Critical GIN misconception:** GIN indexes do NOT accelerate `->>` extraction queries. To use GIN, you must use the containment operator `@>`. This means the API layer must translate filter requests into `@>` containment queries, not `->>` extraction queries.

---

## 7. sqlx Implementation Patterns

### 7.1 DDL Execution with `raw_sql`

`sqlx::raw_sql()` is the correct API for DDL statements. It bypasses prepared statements entirely, which is required because `CREATE TABLE` and `CREATE INDEX` statements cannot be parameterized.

```rust
use sqlx::PgPool;

pub async fn execute_ddl(pool: &PgPool, statements: &[String]) -> Result<(), sqlx::Error> {
    // Wrap all DDL in a single transaction for atomicity.
    // If any statement fails, all roll back.
    let mut tx = pool.begin().await?;

    for statement in statements {
        sqlx::raw_sql(statement)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}
```

**Key properties of `raw_sql`:**

- No prepared statements created or cached
- No bind parameters supported (identifiers must be safe-quoted)
- Multiple semicolon-separated statements execute as implicit transaction
- Compatible with `Transaction`, `Pool`, and `Connection` as executor

### 7.2 Alternative: Batch DDL Execution

For large DDL batches, concatenating statements with semicolons and executing as one `raw_sql` call is more efficient:

```rust
pub async fn execute_ddl_batch(pool: &PgPool, statements: &[String]) -> Result<(), sqlx::Error> {
    let batch = statements.join(";\n");
    sqlx::raw_sql(&batch)
        .execute(pool)
        .await?;
    Ok(())
}
```

By default, when `raw_sql` executes a SQL string containing multiple statements separated by semicolons, the database server treats those statements as all executing within the same transaction block. If one statement triggers an error, the whole script aborts and rolls back.

### 7.3 Dynamic INSERT with QueryBuilder

For inserting decoded account data into dynamically-created tables, use `QueryBuilder` with bind parameters for safety:

```rust
use sqlx::{PgPool, QueryBuilder, Postgres};
use serde_json::Value as JsonValue;

pub async fn upsert_account(
    pool: &PgPool,
    schema: &str,
    table: &str,
    pubkey: &str,
    slot: i64,
    write_version: i64,
    decoded_data: &JsonValue,
    promoted_columns: &[(String, PgValue)], // (column_name, value)
) -> Result<(), sqlx::Error> {
    let full_table = format!("{}.{}", quote_ident(schema), quote_ident(table));

    // Build column list
    let mut col_names = vec![
        "\"pubkey\"", "\"slot\"", "\"write_version\"", "\"data\"", "\"updated_at\""
    ];
    for (col, _) in promoted_columns {
        col_names.push(col); // Already quoted
    }

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        format!("INSERT INTO {} ({}) VALUES (", full_table, col_names.join(", "))
    );

    // Bind common values
    let mut sep = qb.separated(", ");
    sep.push_bind(pubkey);
    sep.push_bind(slot);
    sep.push_bind(write_version);
    sep.push_bind(decoded_data); // sqlx handles JsonValue -> JSONB
    sep.push_unseparated(", NOW()");

    // Bind promoted column values
    for (_, value) in promoted_columns {
        match value {
            PgValue::Bigint(v) => { sep.push_bind(*v); }
            PgValue::Integer(v) => { sep.push_bind(*v); }
            PgValue::Smallint(v) => { sep.push_bind(*v); }
            PgValue::Boolean(v) => { sep.push_bind(*v); }
            PgValue::Text(v) => { sep.push_bind(v.as_str()); }
            PgValue::Numeric(v) => { sep.push_bind(v.clone()); }
            PgValue::Null => { sep.push("NULL"); }
        }
    }

    // UPSERT: update if newer write_version
    qb.push(format!(
        ") ON CONFLICT (\"pubkey\") DO UPDATE SET \
         \"slot\" = EXCLUDED.\"slot\", \
         \"write_version\" = EXCLUDED.\"write_version\", \
         \"data\" = EXCLUDED.\"data\", \
         \"updated_at\" = NOW()"
    ));

    // Add promoted column updates
    for (col, _) in promoted_columns {
        qb.push(format!(", {} = EXCLUDED.{}", col, col));
    }

    qb.push(" WHERE EXCLUDED.\"write_version\" > ");
    qb.push(format!("{}.\"write_version\"", full_table));

    qb.build().execute(pool).await?;
    Ok(())
}
```

### 7.4 Dynamic SELECT with QueryBuilder (for API)

```rust
pub async fn query_accounts(
    pool: &PgPool,
    schema: &str,
    table: &str,
    filters: &[(String, FilterOp, JsonValue)],
    limit: i64,
    offset: i64,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    let full_table = format!("{}.{}", quote_ident(schema), quote_ident(table));

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        format!("SELECT * FROM {} WHERE 1=1", full_table)
    );

    for (col, op, value) in filters {
        match op {
            FilterOp::Eq => {
                qb.push(format!(" AND {} = ", quote_ident(col)));
                qb.push_bind(value.clone());
            }
            FilterOp::Gt => {
                qb.push(format!(" AND {} > ", quote_ident(col)));
                qb.push_bind(value.clone());
            }
            FilterOp::Contains => {
                // JSONB containment for nested field queries
                qb.push(" AND \"data\" @> ");
                qb.push_bind(value.clone());
            }
            // ... other operators
        }
    }

    qb.push(" ORDER BY \"slot\" DESC");
    qb.push(" LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);

    qb.build()
        .fetch_all(pool)
        .await
}
```

### 7.5 Schema Existence Check

```rust
pub async fn schema_exists(pool: &PgPool, schema_name: &str) -> Result<bool, sqlx::Error> {
    let result: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.schemata
            WHERE schema_name = $1
        )"
    )
    .bind(schema_name)
    .fetch_one(pool)
    .await?;

    Ok(result.0)
}

pub async fn table_exists(
    pool: &PgPool,
    schema_name: &str,
    table_name: &str,
) -> Result<bool, sqlx::Error> {
    let result: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_schema = $1 AND table_name = $2
        )"
    )
    .bind(schema_name)
    .bind(table_name)
    .fetch_one(pool)
    .await?;

    Ok(result.0)
}
```

### 7.6 Connection Pool Configuration

```rust
use sqlx::postgres::PgPoolOptions;

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(20)            // Enough for concurrent indexing + API
        .min_connections(2)             // Keep warm connections
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .connect(database_url)
        .await
}
```

### 7.7 SQL Injection Prevention for Identifiers

Since DDL cannot use bind parameters for identifiers (table names, column names, schema names), all identifiers must be sanitized:

```rust
/// Sanitize and quote a PostgreSQL identifier.
/// This is the ONLY function that should produce identifier strings for DDL.
fn sanitize_identifier(name: &str) -> String {
    // 1. Convert to snake_case
    let snake = to_snake_case(name);

    // 2. Remove anything that isn't alphanumeric or underscore
    let clean: String = snake
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();

    // 3. Ensure it starts with a letter or underscore
    let safe = if clean.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{}", clean)
    } else if clean.is_empty() {
        "_unnamed".to_string()
    } else {
        clean
    };

    // 4. Truncate to 63 bytes (PG NAMEDATALEN - 1)
    truncate_to_bytes(&safe, 63)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
```

---

## 8. Typed Column Selection Decision Tree

```
Given an IDL field with type T:

Is T a primitive type?
  (bool, u8-u256, i8-i256, f32, f64, string, pubkey, bytes)
  |
  YES -> Map to native PG column type (see Section 2.1/2.2)
  |      -> PROMOTE to column + also include in JSONB data
  |
  NO -> Is T an Option<inner> or COption<inner>?
        |
        YES -> Is inner a primitive type?
        |      |
        |      YES -> Nullable native PG column
        |      NO  -> JSONB (nullable)
        |
        NO -> Is T a Vec<inner> or [inner; N]?
              |
              YES -> Is inner u8?
              |      |
              |      YES -> BYTEA (byte blob special case)
              |      NO  -> Is inner a primitive type?
              |             |
              |             YES -> PG native array (e.g., BIGINT[], TEXT[])
              |             NO  -> JSONB (array of objects)
              |
              NO -> Is T a Defined type?
                    |
                    YES -> Is it a type alias?
                    |      |
                    |      YES -> Resolve and re-evaluate
                    |      NO  -> JSONB (structs and enums always go to JSONB)
                    |
                    NO -> JSONB (HashMap, BTreeMap, HashSet, BTreeSet, Tuple, unknown)
```

**Simplification for MVP:** For the bounty, promote only top-level primitive fields to native columns. All structs, enums, and collections go to JSONB. This covers 90%+ of useful query patterns while keeping the DDL generator simple.

---

## 9. Schema Evolution Strategy

### 9.1 Change Detection

```
On program registration or periodic check:
1. Fetch current IDL (on-chain or from file)
2. Compute SHA-256 hash of normalized IDL JSON (sorted keys for determinism)
3. Compare with stored hash in {schema}._metadata WHERE key = 'idl_hash'
4. If different -> schema evolution needed
```

### 9.2 Evolution Strategy: Additive Only

For the bounty, use the simplest safe approach: **additive-only schema changes**.

```
When IDL changes:
1. Detect new fields in existing account types
   -> ALTER TABLE ADD COLUMN IF NOT EXISTS (safe, instant on PG 11+)

2. Detect new account types
   -> CREATE TABLE IF NOT EXISTS (new table, no impact on existing)

3. Detect new instruction types
   -> No schema change needed (instructions table uses JSONB args)

4. Detect removed fields
   -> Do NOT drop columns (data loss risk)
   -> Mark as deprecated in _metadata

5. Detect type changes in existing fields
   -> Do NOT alter column type (can break existing data)
   -> Log warning, continue using JSONB fallback
   -> The `data` JSONB column always has the full current payload

6. Detect removed account types
   -> Do NOT drop tables (data loss risk)
   -> Mark as deprecated in _metadata

7. Update IDL hash in _metadata
```

### 9.3 Safe ALTER TABLE Patterns

```sql
-- Adding a new column (safe, instant on PG 11+)
ALTER TABLE "my_defi"."pool"
    ADD COLUMN IF NOT EXISTS "new_field" BIGINT;

-- NEVER DO:
-- ALTER TABLE ... DROP COLUMN (data loss)
-- ALTER TABLE ... ALTER COLUMN ... TYPE (table rewrite + lock)
-- ALTER TABLE ... ADD COLUMN ... DEFAULT now() (table rewrite on PG < 11)
```

### 9.4 Idempotent DDL

All generated DDL uses `IF NOT EXISTS` clauses:

- `CREATE SCHEMA IF NOT EXISTS` -- safe on restart
- `CREATE TABLE IF NOT EXISTS` -- safe on restart
- `CREATE INDEX IF NOT EXISTS` -- safe on restart
- `INSERT ... ON CONFLICT DO UPDATE` -- safe for metadata seeding

This means the entire DDL script can be re-run at any time without error. On startup, Solarix always runs the full DDL script. If tables already exist, the statements are no-ops.

### 9.5 The JSONB Safety Net

The `data` JSONB column is the schema evolution safety net. Even if promoted columns lag behind IDL changes, the full decoded payload is always available in `data`. Queries can always fall back to JSONB path extraction:

```sql
-- Even if 'new_field' column doesn't exist yet:
SELECT data->>'new_field' AS new_field FROM "my_defi"."pool";
```

---

## 10. Performance Projections

### 10.1 JSONB vs Native Columns at Scale

Based on research (IEEE 2025 benchmark, Xendit Engineering, CyberTec):

| Metric                | Native Columns (B-tree)    | JSONB (GIN)                          | Difference                 |
| --------------------- | -------------------------- | ------------------------------------ | -------------------------- |
| Equality lookup       | ~0.1-0.5ms                 | ~0.2-1ms                             | 2x faster native           |
| Range query           | ~1-5ms (10K results)       | Not supported by GIN                 | N/A                        |
| Aggregation (SUM/AVG) | ~200ms (10M rows)          | ~400-600ms (extract + aggregate)     | 50-70% faster native       |
| Write throughput      | ~50K inserts/sec           | ~42K inserts/sec (GIN overhead ~16%) | Modest overhead            |
| Index size (1M rows)  | ~35MB per B-tree column    | ~2-10MB (jsonb_path_ops)             | GIN path_ops is smaller    |
| Storage per row       | Fixed (8 bytes for BIGINT) | Variable (~500 bytes avg JSONB)      | JSONB is larger but shared |

**Key insight:** The hybrid approach captures the best of both worlds. Promoted native columns handle the high-frequency query patterns (filter by owner, filter by amount, aggregate liquidity) at full speed. The JSONB `data` column handles the long-tail of ad-hoc nested queries at acceptable speed.

### 10.2 Expected Solarix Query Performance

At bounty demo scale (10K-100K accounts, 100K-1M instructions):

| Query Type                                      | Expected Latency | Index Used                       |
| ----------------------------------------------- | ---------------- | -------------------------------- |
| Get account by pubkey                           | <1ms             | PK B-tree                        |
| Filter accounts by owner pubkey                 | 1-5ms            | B-tree on promoted column        |
| Filter accounts by amount range                 | 1-10ms           | B-tree on promoted column        |
| Search nested JSONB field                       | 5-20ms           | GIN jsonb_path_ops               |
| Get instructions by signature                   | 1-5ms            | B-tree                           |
| Filter instructions by name + slot range        | 5-20ms           | B-tree composite                 |
| Aggregate (SUM, AVG, COUNT) on promoted columns | 10-50ms          | Seq scan or B-tree (small table) |
| Complex JSONB containment query                 | 10-50ms          | GIN jsonb_path_ops               |

At production scale (10M+ accounts, 100M+ instructions), the promoted columns become critical. Without them, every aggregation query would require JSONB extraction, adding 50-70% overhead.

### 10.3 Write Throughput Projections

For account state upserts (the main write pattern):

| Factor                              | Estimate                          |
| ----------------------------------- | --------------------------------- |
| PostgreSQL raw INSERT throughput    | ~50K rows/sec (single connection) |
| UPSERT with conflict resolution     | ~30K rows/sec                     |
| GIN index overhead (jsonb_path_ops) | ~16% reduction                    |
| Effective upsert rate               | ~25K rows/sec                     |
| With connection pooling (5 writers) | ~100K rows/sec                    |

For the bounty, this is orders of magnitude more than needed. Solana produces ~2 blocks/second, each with hundreds to low thousands of relevant account updates per program.

### 10.4 GIN Index Write Overhead

The `fastupdate` GIN feature (enabled by default) mitigates write overhead by batching index updates:

- New tuples go into a temporary pending list
- Pending list is flushed during VACUUM or when it exceeds `gin_pending_list_limit` (default 4MB)
- This converts many small random writes into fewer bulk writes
- Trade-off: slightly stale index during the pending period (acceptable for our use case)

---

## 11. Complete Worked Example

### 11.1 Sample IDL

A realistic token swap program with structs, enums, nested types, and various field types:

```json
{
  "address": "TokenSwap11111111111111111111111111111111111",
  "metadata": {
    "name": "token_swap",
    "version": "0.1.0",
    "spec": "0.1.0"
  },
  "instructions": [
    {
      "name": "initialize",
      "discriminator": [175, 175, 109, 31, 13, 152, 155, 237],
      "accounts": [
        { "name": "pool", "writable": true, "signer": true },
        { "name": "authority" },
        { "name": "token_a_mint" },
        { "name": "token_b_mint" },
        {
          "name": "system_program",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        { "name": "fee_rate", "type": "u16" },
        { "name": "initial_liquidity", "type": "u64" }
      ]
    },
    {
      "name": "swap",
      "discriminator": [248, 198, 158, 145, 225, 117, 135, 200],
      "accounts": [
        { "name": "pool", "writable": true },
        { "name": "user", "signer": true },
        { "name": "user_token_a", "writable": true },
        { "name": "user_token_b", "writable": true },
        { "name": "pool_token_a", "writable": true },
        { "name": "pool_token_b", "writable": true },
        {
          "name": "token_program",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        }
      ],
      "args": [
        { "name": "amount_in", "type": "u64" },
        { "name": "minimum_amount_out", "type": "u64" },
        {
          "name": "direction",
          "type": { "defined": { "name": "SwapDirection" } }
        }
      ]
    }
  ],
  "accounts": [
    { "name": "Pool", "discriminator": [255, 176, 4, 245, 188, 253, 124, 25] },
    {
      "name": "UserPosition",
      "discriminator": [100, 200, 50, 75, 150, 25, 175, 225]
    }
  ],
  "types": [
    {
      "name": "Pool",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "authority", "type": "pubkey" },
          { "name": "token_a_mint", "type": "pubkey" },
          { "name": "token_b_mint", "type": "pubkey" },
          { "name": "total_liquidity", "type": "u128" },
          { "name": "fee_rate", "type": "u16" },
          { "name": "is_active", "type": "bool" },
          { "name": "cumulative_volume", "type": "u128" },
          { "name": "last_update_slot", "type": "u64" },
          { "name": "config", "type": { "defined": { "name": "PoolConfig" } } },
          { "name": "reserved", "type": { "array": ["u8", 64] } }
        ]
      }
    },
    {
      "name": "UserPosition",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "owner", "type": "pubkey" },
          { "name": "pool", "type": "pubkey" },
          { "name": "liquidity_shares", "type": "u64" },
          { "name": "deposit_slot", "type": "u64" },
          { "name": "rewards_earned", "type": "u128" },
          { "name": "is_locked", "type": "bool" },
          { "name": "pending_rewards", "type": { "option": "u64" } }
        ]
      }
    },
    {
      "name": "PoolConfig",
      "type": {
        "kind": "struct",
        "fields": [
          { "name": "max_slippage_bps", "type": "u16" },
          { "name": "protocol_fee_bps", "type": "u16" },
          { "name": "admin", "type": { "option": "pubkey" } }
        ]
      }
    },
    {
      "name": "SwapDirection",
      "type": {
        "kind": "enum",
        "variants": [{ "name": "AtoB" }, { "name": "BtoA" }]
      }
    }
  ]
}
```

### 11.2 DDL Generation Trace

Walking through the algorithm step by step:

**Step 1: Build type_map from `idl.types`:**

```
type_map = {
    "Pool" -> struct with 10 fields,
    "UserPosition" -> struct with 7 fields,
    "PoolConfig" -> struct with 3 fields,
    "SwapDirection" -> enum with 2 unit variants,
}
```

**Step 2: Process Pool account struct fields:**

| Field               | IDL Type              | Decision                      | PG Type       |
| ------------------- | --------------------- | ----------------------------- | ------------- |
| `authority`         | `pubkey`              | Primitive -> PROMOTE          | `VARCHAR(44)` |
| `token_a_mint`      | `pubkey`              | Primitive -> PROMOTE          | `VARCHAR(44)` |
| `token_b_mint`      | `pubkey`              | Primitive -> PROMOTE          | `VARCHAR(44)` |
| `total_liquidity`   | `u128`                | Primitive -> PROMOTE          | `NUMERIC(39)` |
| `fee_rate`          | `u16`                 | Primitive -> PROMOTE          | `INTEGER`     |
| `is_active`         | `bool`                | Primitive -> PROMOTE          | `BOOLEAN`     |
| `cumulative_volume` | `u128`                | Primitive -> PROMOTE          | `NUMERIC(39)` |
| `last_update_slot`  | `u64`                 | Primitive -> PROMOTE          | `BIGINT`      |
| `config`            | `Defined{PoolConfig}` | Struct -> JSONB only          | Not promoted  |
| `reserved`          | `[u8; 64]`            | Vec<u8> special case -> BYTEA | `BYTEA`       |

**Step 3: Process UserPosition account struct fields:**

| Field              | IDL Type      | Decision                              | PG Type             |
| ------------------ | ------------- | ------------------------------------- | ------------------- |
| `owner`            | `pubkey`      | Primitive -> PROMOTE                  | `VARCHAR(44)`       |
| `pool`             | `pubkey`      | Primitive -> PROMOTE                  | `VARCHAR(44)`       |
| `liquidity_shares` | `u64`         | Primitive -> PROMOTE                  | `BIGINT`            |
| `deposit_slot`     | `u64`         | Primitive -> PROMOTE                  | `BIGINT`            |
| `rewards_earned`   | `u128`        | Primitive -> PROMOTE                  | `NUMERIC(39)`       |
| `is_locked`        | `bool`        | Primitive -> PROMOTE                  | `BOOLEAN`           |
| `pending_rewards`  | `Option<u64>` | Option<primitive> -> PROMOTE nullable | `BIGINT` (nullable) |

### 11.3 Generated DDL Output

```sql
-- =============================================================
-- DDL generated by Solarix for program: token_swap
-- Program ID: TokenSwap11111111111111111111111111111111111
-- IDL hash: a1b2c3d4e5f6...
-- =============================================================

-- Step 1: Create schema
CREATE SCHEMA IF NOT EXISTS "token_swap";

-- Step 2: Metadata table
CREATE TABLE IF NOT EXISTS "token_swap"."_metadata" (
    "key"           TEXT PRIMARY KEY,
    "value"         JSONB NOT NULL,
    "updated_at"    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO "token_swap"."_metadata" ("key", "value") VALUES
    ('program_id', '"TokenSwap11111111111111111111111111111111111"'),
    ('program_name', '"token_swap"'),
    ('idl_hash', '"a1b2c3d4e5f6..."'),
    ('idl_version', '"0.1.0"'),
    ('schema_created_at', '"2026-04-05T12:00:00Z"'),
    ('account_types', '["Pool", "UserPosition"]'),
    ('instruction_types', '["initialize", "swap"]')
ON CONFLICT ("key") DO UPDATE
    SET "value" = EXCLUDED."value", "updated_at" = NOW();

-- Step 3: Checkpoints table
CREATE TABLE IF NOT EXISTS "token_swap"."_checkpoints" (
    "checkpoint_type"   TEXT PRIMARY KEY,
    "last_slot"         BIGINT NOT NULL DEFAULT 0,
    "last_signature"    VARCHAR(88),
    "details"           JSONB,
    "updated_at"        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Step 4a: Account table for "Pool"
CREATE TABLE IF NOT EXISTS "token_swap"."pool" (
    -- Common columns
    "pubkey"                VARCHAR(44) PRIMARY KEY,
    "slot"                  BIGINT NOT NULL,
    "write_version"         BIGINT NOT NULL DEFAULT 0,
    "updated_at"            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Full decoded payload
    "data"                  JSONB NOT NULL,
    -- Promoted columns (scalar fields from Pool struct)
    "authority"             VARCHAR(44),
    "token_a_mint"          VARCHAR(44),
    "token_b_mint"          VARCHAR(44),
    "total_liquidity"       NUMERIC(39),
    "fee_rate"              INTEGER,
    "is_active"             BOOLEAN,
    "cumulative_volume"     NUMERIC(39),
    "last_update_slot"      BIGINT,
    "reserved"              BYTEA
    -- Note: "config" (PoolConfig struct) is NOT promoted -> lives in "data" JSONB only
);

-- Indexes for "pool"
CREATE INDEX IF NOT EXISTS "idx_pool_slot"
    ON "token_swap"."pool" ("slot");
CREATE INDEX IF NOT EXISTS "idx_pool_authority"
    ON "token_swap"."pool" ("authority");
CREATE INDEX IF NOT EXISTS "idx_pool_token_a_mint"
    ON "token_swap"."pool" ("token_a_mint");
CREATE INDEX IF NOT EXISTS "idx_pool_token_b_mint"
    ON "token_swap"."pool" ("token_b_mint");
CREATE INDEX IF NOT EXISTS "idx_pool_data"
    ON "token_swap"."pool" USING GIN ("data" jsonb_path_ops);

-- Step 4b: Account table for "UserPosition"
CREATE TABLE IF NOT EXISTS "token_swap"."user_position" (
    -- Common columns
    "pubkey"                VARCHAR(44) PRIMARY KEY,
    "slot"                  BIGINT NOT NULL,
    "write_version"         BIGINT NOT NULL DEFAULT 0,
    "updated_at"            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Full decoded payload
    "data"                  JSONB NOT NULL,
    -- Promoted columns (all scalar fields from UserPosition struct)
    "owner"                 VARCHAR(44),
    "pool"                  VARCHAR(44),
    "liquidity_shares"      BIGINT,
    "deposit_slot"          BIGINT,
    "rewards_earned"        NUMERIC(39),
    "is_locked"             BOOLEAN,
    "pending_rewards"       BIGINT       -- Option<u64> -> nullable BIGINT
);

-- Indexes for "user_position"
CREATE INDEX IF NOT EXISTS "idx_user_position_slot"
    ON "token_swap"."user_position" ("slot");
CREATE INDEX IF NOT EXISTS "idx_user_position_owner"
    ON "token_swap"."user_position" ("owner");
CREATE INDEX IF NOT EXISTS "idx_user_position_pool"
    ON "token_swap"."user_position" ("pool");
CREATE INDEX IF NOT EXISTS "idx_user_position_data"
    ON "token_swap"."user_position" USING GIN ("data" jsonb_path_ops);

-- Step 5: Instructions table (unified for all instruction types)
CREATE TABLE IF NOT EXISTS "token_swap"."_instructions" (
    "id"                    BIGSERIAL PRIMARY KEY,
    "signature"             VARCHAR(88) NOT NULL,
    "instruction_index"     SMALLINT NOT NULL,
    "inner_index"           SMALLINT,
    "slot"                  BIGINT NOT NULL,
    "block_time"            TIMESTAMPTZ,
    "program_id"            VARCHAR(44) NOT NULL,
    "instruction_name"      TEXT NOT NULL,
    "args"                  JSONB,
    "accounts"              JSONB NOT NULL,
    "success"               BOOLEAN NOT NULL DEFAULT TRUE,
    "error_message"         TEXT,
    "logs"                  TEXT[],
    "created_at"            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE ("signature", "instruction_index", COALESCE("inner_index", -1))
);

-- Indexes for "_instructions"
CREATE INDEX IF NOT EXISTS "idx_ix_slot"
    ON "token_swap"."_instructions" ("slot");
CREATE INDEX IF NOT EXISTS "idx_ix_name"
    ON "token_swap"."_instructions" ("instruction_name");
CREATE INDEX IF NOT EXISTS "idx_ix_block_time"
    ON "token_swap"."_instructions" ("block_time");
CREATE INDEX IF NOT EXISTS "idx_ix_signature"
    ON "token_swap"."_instructions" ("signature");
CREATE INDEX IF NOT EXISTS "idx_ix_args"
    ON "token_swap"."_instructions" USING GIN ("args" jsonb_path_ops);
CREATE INDEX IF NOT EXISTS "idx_ix_accounts"
    ON "token_swap"."_instructions" USING GIN ("accounts" jsonb_path_ops);
```

### 11.4 Example Queries Against This Schema

```sql
-- Find all active pools with more than 1M liquidity (uses promoted columns)
SELECT "pubkey", "authority", "total_liquidity", "fee_rate"
FROM "token_swap"."pool"
WHERE "is_active" = TRUE
  AND "total_liquidity" > 1000000
ORDER BY "total_liquidity" DESC;

-- Find user positions in a specific pool (uses promoted column index)
SELECT "pubkey", "owner", "liquidity_shares", "rewards_earned"
FROM "token_swap"."user_position"
WHERE "pool" = 'PoolPubkey123...'
ORDER BY "liquidity_shares" DESC;

-- Find pool config details (nested struct via JSONB containment -- uses GIN)
SELECT "pubkey", "data"->'config'->>'max_slippage_bps' AS max_slippage,
       "data"->'config'->>'admin' AS config_admin
FROM "token_swap"."pool"
WHERE "data" @> '{"config": {"protocol_fee_bps": 30}}';

-- Aggregate: total liquidity across all active pools (uses promoted column)
SELECT COUNT(*) AS pool_count,
       SUM("total_liquidity") AS total_tvl,
       AVG("fee_rate") AS avg_fee
FROM "token_swap"."pool"
WHERE "is_active" = TRUE;

-- Find all swap instructions in a slot range (uses B-tree indexes)
SELECT "signature", "block_time", "instruction_name", "args", "accounts"
FROM "token_swap"."_instructions"
WHERE "instruction_name" = 'swap'
  AND "slot" BETWEEN 250000000 AND 250010000
ORDER BY "slot", "instruction_index";

-- Find swaps by amount (JSONB containment for GIN, not ->> extraction)
SELECT "signature", "args"->>'amount_in' AS amount_in,
       "args"->>'direction' AS direction
FROM "token_swap"."_instructions"
WHERE "instruction_name" = 'swap'
  AND "args" @> '{"direction": {"AtoB": {}}}';

-- Find all instructions involving a specific account pubkey (GIN on accounts)
SELECT "signature", "instruction_name", "block_time"
FROM "token_swap"."_instructions"
WHERE "accounts" @> '[{"pubkey": "UserPubkey123..."}]';

-- Statistics: instruction count by type over time
SELECT "instruction_name",
       date_trunc('hour', "block_time") AS hour,
       COUNT(*) AS count
FROM "token_swap"."_instructions"
WHERE "block_time" > NOW() - INTERVAL '24 hours'
GROUP BY "instruction_name", hour
ORDER BY hour DESC, count DESC;
```

---

## 12. Sources

### PostgreSQL Performance and Patterns

- [PostgreSQL JSONB-based vs. Typed-column Indexing Benchmark (IEEE 2025)](https://ieee-dataport.org/documents/postgresql-jsonb-based-vs-typed-column-indexing-benchmark-read-queries) -- Rigorous academic comparison
- [PostgreSQL JSONB - Powerful Storage for Semi-Structured Data](https://www.architecture-weekly.com/p/postgresql-jsonb-powerful-storage) -- Hybrid approach analysis
- [Postgres 2025: Advanced JSON Query Optimization Techniques](https://markaicode.com/postgres-json-optimization-techniques-2025/) -- JSON Path 15-25% speedup, CROSS JOIN LATERAL 20-35% improvement
- [Postgres large JSON value query performance](https://www.evanjones.ca/postgres-large-json-performance.html) -- TOAST overhead for JSONB > 2KB
- [Postgres Planner Quirks: JSONB selectivity estimates](https://pganalyze.com/blog/5mins-postgres-planner-jsonb-selectivity) -- GIN index planner issues
- [Benchmarking Postgres Numeric and Integer (Xendit)](https://medium.com/xendit-engineering/benchmarking-pg-numeric-integer-9c593d7af67e) -- NUMERIC 50-70% slower than INTEGER (10M rows)
- [int4 vs int8 vs uuid vs numeric performance on bigger joins (CyberTec)](https://www.cybertec-postgresql.com/en/int4-vs-int8-vs-uuid-vs-numeric-performance-on-bigger-joins/) -- NUMERIC +34% penalty on joins

### GIN Index Deep Dive

- [Understanding Postgres GIN Indexes: The Good and the Bad (pganalyze)](https://pganalyze.com/blog/gin-index) -- GIN benchmarks (1M rows), jsonb_ops vs jsonb_path_ops sizing
- [How GIN Indexes Made Our JSONB Queries 100x Faster](https://medium.com/@sachin.backend.dev/how-gin-indexes-made-our-jsonb-queries-100x-faster-in-postgres-8022eedaf4ce) -- Real-world 4min -> 2sec improvement
- [PostgreSQL JSONB GIN Indexes: Why Your Queries Are Slow](https://dev.to/polliog/postgresql-jsonb-gin-indexes-why-your-queries-are-slow-and-how-to-fix-them-12a0) -- GIN does NOT accelerate ->> queries
- [JSONB Operator Classes of GIN Indexes](https://medium.com/@josef.machytka/postgresql-jsonb-operator-classes-of-gin-indexes-and-their-usage-0bf399073a4c) -- jsonb_ops index 60-80% table size; jsonb_path_ops 20-30%
- [Indexing JSONB in Postgres (Crunchy Data)](https://www.crunchydata.com/blog/indexing-jsonb-in-postgres) -- Best practices
- [PostgreSQL GIN Indexes Official Docs](https://www.postgresql.org/docs/current/gin.html) -- Fastupdate, pending list, vacuuming

### Large Integer Storage (Blockchain)

- [Storing u64 in PostgreSQL with SQLx](https://github.com/launchbadge/sqlx/discussions/2977) -- Community discussion on u64 overflow
- [Storing large Ethereum numbers in Postgres](https://www.turfemon.com/storing-large-ethereum-numbers-postgres) -- NUMERIC(78) for uint256
- [PostgreSQL extension: postgres_web3](https://github.com/Yen/postgres_web3) -- Native uint256 type (extension required)
- [PostgreSQL Numeric Types Official Docs](https://www.postgresql.org/docs/current/datatype-numeric.html) -- NUMERIC storage: 2 bytes per 4 decimal digits

### Dynamic DDL and sqlx

- [sqlx::raw_sql documentation](https://docs.rs/sqlx/latest/sqlx/fn.raw_sql.html) -- DDL execution without prepared statements
- [sqlx::QueryBuilder documentation](https://docs.rs/sqlx/latest/sqlx/struct.QueryBuilder.html) -- Dynamic DML with bind parameters
- [Dynamic SQL queries in sqlx](https://users.rust-lang.org/t/dynamic-sql-queries-in-sqlx/109244) -- Community patterns for runtime SQL
- [Raw SQL in Rust with SQLx (Shuttle)](https://www.shuttle.dev/blog/2023/10/04/sql-in-rust) -- Tutorial with examples

### PostgreSQL Identifiers and Naming

- [PostgreSQL Lexical Structure](https://www.postgresql.org/docs/current/sql-syntax-lexical.html) -- 63-byte identifier limit, reserved words
- [PostgreSQL and the 63-character limit](https://hamzatazeez.medium.com/postgresql-and-the-63-character-limit-c925fd6a3ae7) -- Truncation pitfalls
- [PostgreSQL SQL Review and Style Guide (Bytebase)](https://www.bytebase.com/blog/postgres-sql-review-guide/) -- Naming conventions best practices

### Solana Indexer Patterns

- [How to Index Solana Data (Helius)](https://www.helius.dev/docs/rpc/how-to-index-solana-data) -- Helius indexer architecture, DAS + Photon
- [Solana Token Transfer Indexer Design](https://www.niks3089.com/posts/token-history-indexer-design/) -- PostgreSQL schema for token transfer indexing
- [Solana AccountsDB Plugin Postgres](https://github.com/solana-labs/solana-accountsdb-plugin-postgres) -- Official Geyser plugin schema
- [Analyzing Solana On-Chain Data with Custom Indexers](https://www.chainary.net/articles/analyzing-solana-on-chain-data-with-custom-indexers) -- Indexer architecture patterns

### PostgreSQL Arrays vs JSONB

- [Postgres Arrays vs JSON Datatypes](https://www.netguru.com/blog/postgres-arrays-vs-json-datatypes-in-rails-5) -- Native arrays use less storage
- [Store Operations Optimization: Arrays, JSON, Tags (Alibaba)](https://www.alibabacloud.com/blog/store-operations-optimization-search-acceleration-over-postgresql-arrays-json-and-internal-tag-data_595796) -- Fixed-size array composite expression indexes
- [PostgreSQL JSON Types Official Docs](https://www.postgresql.org/docs/current/datatype-json.html) -- JSONB vs JSON, GIN indexing
