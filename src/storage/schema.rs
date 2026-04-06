// std library
use std::collections::HashMap;
use std::fmt::Write;
use std::future::Future;
use std::pin::Pin;

// external crates
use anchor_lang_idl_spec::{Idl, IdlDefinedFields, IdlField, IdlType, IdlTypeDef, IdlTypeDefTy};
use sqlx::PgPool;
use tracing::{info, warn};

// internal crate
use crate::storage::StorageError;

/// Sanitize a name for use as a PostgreSQL identifier.
///
/// - Strips non-alphanumeric chars except underscores
/// - Lowercases the result
/// - Prepends `_` if starts with digit
/// - Falls back to `_unnamed` if empty after sanitization
/// - Truncates to 63 bytes on byte boundaries
pub fn sanitize_identifier(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_lowercase();

    let sanitized = if sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{sanitized}")
    } else {
        sanitized
    };

    let sanitized = if sanitized.is_empty() {
        "_unnamed".to_string()
    } else {
        sanitized
    };

    truncate_to_bytes(&sanitized, 63)
}

/// Derive a schema name from IDL name and program ID.
///
/// Format: `{sanitized_name}_{lowercase_first_8_of_base58_program_id}`
pub fn derive_schema_name(idl_name: &str, program_id: &str) -> String {
    let id_prefix: String = program_id
        .chars()
        .take(8)
        .collect::<String>()
        .to_lowercase();
    // Cap name_part so _{id_prefix} suffix always fits within 63-byte PG limit
    let max_name_len = 63 - 1 - id_prefix.len();
    let name_part = truncate_to_bytes(&sanitize_identifier(idl_name), max_name_len);
    format!("{name_part}_{id_prefix}")
}

fn truncate_to_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Double-quote a PostgreSQL identifier, escaping embedded double-quotes.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Map an IDL type to a PostgreSQL column type for promoted columns.
///
/// Returns `Some("PG_TYPE")` for promotable scalar types, `None` for complex
/// types that should remain in the JSONB `data` column only.
pub fn map_idl_type_to_pg(ty: &IdlType, types: &[IdlTypeDef]) -> Option<&'static str> {
    map_idl_type_to_pg_inner(ty, types, 0)
}

const MAX_TYPE_RESOLUTION_DEPTH: u8 = 16;

fn map_idl_type_to_pg_inner(ty: &IdlType, types: &[IdlTypeDef], depth: u8) -> Option<&'static str> {
    if depth > MAX_TYPE_RESOLUTION_DEPTH {
        return None;
    }
    match ty {
        IdlType::Bool => Some("BOOLEAN"),
        IdlType::U8 | IdlType::I8 => Some("SMALLINT"),
        IdlType::U16 => Some("INTEGER"),
        IdlType::I16 => Some("SMALLINT"),
        IdlType::U32 | IdlType::I32 => Some("INTEGER"),
        IdlType::F32 => Some("REAL"),
        IdlType::U64 | IdlType::I64 => Some("BIGINT"),
        IdlType::F64 => Some("DOUBLE PRECISION"),
        IdlType::U128 | IdlType::I128 => Some("NUMERIC(39,0)"),
        IdlType::U256 | IdlType::I256 => Some("NUMERIC(78,0)"),
        IdlType::String => Some("TEXT"),
        IdlType::Pubkey => Some("TEXT"),
        IdlType::Bytes => Some("BYTEA"),
        IdlType::Option(inner) => map_idl_type_to_pg_inner(inner, types, depth + 1),
        IdlType::Array(inner, _) => {
            // byte arrays [u8; N] -> BYTEA, everything else not promoted
            if matches!(inner.as_ref(), IdlType::U8) {
                Some("BYTEA")
            } else {
                None
            }
        }
        IdlType::Vec(_) => None,
        IdlType::Defined { name, .. } => resolve_defined_type(name, types, depth + 1),
        IdlType::Generic(_) => None,
        _ => None,
    }
}

/// Resolve a `Defined` type through the alias chain to determine if it's promotable.
fn resolve_defined_type(name: &str, types: &[IdlTypeDef], depth: u8) -> Option<&'static str> {
    if depth > MAX_TYPE_RESOLUTION_DEPTH {
        return None;
    }
    let type_def = types.iter().find(|t| t.name == name)?;
    match &type_def.ty {
        IdlTypeDefTy::Type { alias } => map_idl_type_to_pg_inner(alias, types, depth + 1),
        IdlTypeDefTy::Struct { .. } | IdlTypeDefTy::Enum { .. } => None,
    }
}

/// System column names reserved for account tables. IDL fields with these names
/// are not promoted to native columns (they remain accessible via the JSONB `data` column).
const RESERVED_ACCOUNT_COLUMNS: &[&str] = &[
    "pubkey",
    "slot_updated",
    "write_version",
    "lamports",
    "data",
    "is_closed",
    "updated_at",
];

/// Generate DDL for an account table with promoted columns.
pub fn generate_account_table(
    schema: &str,
    account_name: &str,
    fields: &[IdlField],
    types: &[IdlTypeDef],
) -> Vec<String> {
    let table = sanitize_identifier(account_name);
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(&table));

    let mut columns = String::new();
    let _ = write!(
        columns,
        r#"    "pubkey" TEXT PRIMARY KEY,
    "slot_updated" BIGINT NOT NULL,
    "write_version" BIGINT NOT NULL DEFAULT 0,
    "lamports" BIGINT NOT NULL,
    "data" JSONB NOT NULL,
    "is_closed" BOOLEAN NOT NULL DEFAULT FALSE,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT NOW()"#
    );

    for field in fields {
        if let Some(pg_type) = map_idl_type_to_pg(&field.ty, types) {
            let col_name = sanitize_identifier(&field.name);
            if RESERVED_ACCOUNT_COLUMNS.contains(&col_name.as_str()) {
                continue;
            }
            let _ = write!(columns, ",\n    {} {pg_type}", quote_ident(&col_name));
        }
    }

    vec![format!(
        "CREATE TABLE IF NOT EXISTS {qualified} (\n{columns}\n);"
    )]
}

/// Generate DDL for the single `_instructions` table.
pub fn generate_instructions_table(schema: &str) -> Vec<String> {
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident("_instructions"));

    let create = format!(
        r#"CREATE TABLE IF NOT EXISTS {qualified} (
    "id" BIGSERIAL PRIMARY KEY,
    "signature" TEXT NOT NULL,
    "slot" BIGINT NOT NULL,
    "block_time" BIGINT,
    "instruction_name" TEXT NOT NULL,
    "instruction_index" SMALLINT NOT NULL,
    "inner_index" SMALLINT,
    "args" JSONB NOT NULL,
    "accounts" JSONB NOT NULL,
    "data" JSONB NOT NULL,
    "is_inner_ix" BOOLEAN NOT NULL DEFAULT FALSE
);"#
    );

    let unique = format!(
        r#"CREATE UNIQUE INDEX IF NOT EXISTS "uq__instructions_sig_idx" ON {qualified} ("signature", "instruction_index", COALESCE("inner_index", -1));"#
    );

    vec![create, unique]
}

/// Generate DDL for the `_metadata` key-value table.
pub fn generate_metadata_table(schema: &str) -> String {
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident("_metadata"));

    format!(
        r#"CREATE TABLE IF NOT EXISTS {qualified} (
    "key" TEXT PRIMARY KEY,
    "value" JSONB NOT NULL
);"#
    )
}

/// Generate DDL for the `_checkpoints` table.
pub fn generate_checkpoints_table(schema: &str) -> String {
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident("_checkpoints"));

    format!(
        r#"CREATE TABLE IF NOT EXISTS {qualified} (
    "stream" TEXT PRIMARY KEY,
    "last_slot" BIGINT,
    "last_signature" VARCHAR(88),
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT NOW()
);"#
    )
}

/// Generate B-tree and GIN indexes for all program tables.
pub fn generate_indexes(schema: &str, account_names: &[String]) -> Vec<String> {
    let mut stmts = Vec::new();
    let schema_q = quote_ident(schema);

    // Account table indexes
    for name in account_names {
        let table = sanitize_identifier(name);
        let table_q = quote_ident(&table);
        let qualified = format!("{schema_q}.{table_q}");

        // B-tree on slot
        stmts.push(format!(
            r#"CREATE INDEX IF NOT EXISTS {} ON {qualified} ("slot_updated");"#,
            quote_ident(&format!("idx_{table}_slot_updated"))
        ));

        // GIN on data
        stmts.push(format!(
            r#"CREATE INDEX IF NOT EXISTS {} ON {qualified} USING gin ("data" jsonb_path_ops);"#,
            quote_ident(&format!("gin_{table}_data"))
        ));
    }

    // Instructions table indexes
    let ix_qualified = format!("{schema_q}.{}", quote_ident("_instructions"));

    stmts.push(format!(
        r#"CREATE INDEX IF NOT EXISTS {} ON {ix_qualified} ("slot");"#,
        quote_ident("idx__instructions_slot")
    ));
    stmts.push(format!(
        r#"CREATE INDEX IF NOT EXISTS {} ON {ix_qualified} ("signature");"#,
        quote_ident("idx__instructions_signature")
    ));
    stmts.push(format!(
        r#"CREATE INDEX IF NOT EXISTS {} ON {ix_qualified} ("instruction_name");"#,
        quote_ident("idx__instructions_instruction_name")
    ));
    stmts.push(format!(
        r#"CREATE INDEX IF NOT EXISTS {} ON {ix_qualified} ("block_time");"#,
        quote_ident("idx__instructions_block_time")
    ));
    stmts.push(format!(
        r#"CREATE INDEX IF NOT EXISTS {} ON {ix_qualified} USING gin ("data" jsonb_path_ops);"#,
        quote_ident("gin__instructions_data")
    ));
    stmts.push(format!(
        r#"CREATE INDEX IF NOT EXISTS {} ON {ix_qualified} USING gin ("args" jsonb_path_ops);"#,
        quote_ident("gin__instructions_args")
    ));

    stmts
}

/// Build all DDL statements for a program schema (pure function, no DB).
pub fn build_ddl_statements(idl: &Idl, schema_name: &str) -> Vec<String> {
    let mut stmts = Vec::new();

    // 1. CREATE SCHEMA
    stmts.push(format!(
        "CREATE SCHEMA IF NOT EXISTS {};",
        quote_ident(schema_name)
    ));

    // 2. Metadata table
    stmts.push(generate_metadata_table(schema_name));

    // 3. Checkpoints table
    stmts.push(generate_checkpoints_table(schema_name));

    // Build type lookup for resolving Defined types
    let type_map: HashMap<&str, &IdlTypeDef> =
        idl.types.iter().map(|t| (t.name.as_str(), t)).collect();

    // 4. Account tables
    let mut account_names = Vec::new();
    for account in &idl.accounts {
        let type_def = type_map.get(account.name.as_str());
        let fields = type_def
            .and_then(|td| match &td.ty {
                IdlTypeDefTy::Struct {
                    fields: Some(IdlDefinedFields::Named(fields)),
                } => Some(fields.as_slice()),
                _ => None,
            })
            .unwrap_or(&[]);

        if fields.is_empty() {
            if let Some(td) = type_def {
                match &td.ty {
                    IdlTypeDefTy::Struct {
                        fields: Some(IdlDefinedFields::Named(_)),
                    }
                    | IdlTypeDefTy::Struct { fields: None } => {}
                    _ => {
                        warn!(
                            account = %account.name,
                            "account type has non-named fields; no columns promoted"
                        );
                    }
                }
            }
        }

        let table_stmts = generate_account_table(schema_name, &account.name, fields, &idl.types);
        stmts.extend(table_stmts);
        account_names.push(account.name.clone());
    }

    // 5. Instructions table
    stmts.extend(generate_instructions_table(schema_name));

    // 6. Indexes
    stmts.extend(generate_indexes(schema_name, &account_names));

    stmts
}

/// Generate the full PostgreSQL schema for a program from its IDL.
///
/// Executes all DDL in a transaction. On failure, all changes are rolled back.
///
/// All parameters are owned so the returned future is `'static` + `Send`.
/// Borrowed parameters create futures with specific lifetimes that Rust's async
/// Send inference cannot prove "general enough" when composed in larger state
/// machines (known compiler limitation, see rust#96865).
pub fn generate_schema(
    pool: PgPool,
    idl: Idl,
    program_id: String,
    schema_name: String,
) -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send>> {
    Box::pin(async move {
        let statements = build_ddl_statements(&idl, &schema_name);
        let batch = statements.join("\n");

        // Execute DDL directly on the pool (no explicit transaction).
        // All DDL uses IF NOT EXISTS, making each statement idempotent.
        // PostgreSQL wraps multi-statement raw_sql in an implicit transaction.
        // This avoids the `tx.as_mut()` reference that triggers the
        // "Executor not general enough" compiler inference failure with Box::pin.
        sqlx::raw_sql(&batch)
            .execute(&pool)
            .await
            .map_err(|e| StorageError::DdlFailed(format!("DDL failed for {schema_name}: {e}")))?;

        info!(
            %program_id,
            %schema_name,
            statements = statements.len(),
            "schema generated"
        );

        Ok(())
    })
}

/// Seed the `_metadata` table with program information.
///
/// Uses INSERT ... ON CONFLICT DO UPDATE for idempotency.
pub fn seed_metadata(
    pool: PgPool,
    idl: Idl,
    program_id: String,
    idl_hash: String,
    schema_name: String,
) -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send>> {
    Box::pin(async move {
        let account_type_names: Vec<&str> = idl.accounts.iter().map(|a| a.name.as_str()).collect();
        let instruction_names: Vec<&str> =
            idl.instructions.iter().map(|i| i.name.as_str()).collect();

        let metadata_entries = vec![
            ("program_id", serde_json::json!(&program_id)),
            ("program_name", serde_json::json!(&idl.metadata.name)),
            ("idl_hash", serde_json::json!(&idl_hash)),
            ("idl_version", serde_json::json!(&idl.metadata.version)),
            ("account_types", serde_json::json!(account_type_names)),
            ("instruction_types", serde_json::json!(instruction_names)),
        ];

        let qualified = format!("{}.{}", quote_ident(&schema_name), quote_ident("_metadata"));

        let mut batch = String::new();
        for (key, value) in &metadata_entries {
            let json_str = serde_json::to_string(value).unwrap_or_default();
            let escaped = json_str.replace('\'', "''");
            let _ = write!(
                batch,
                r#"INSERT INTO {qualified} ("key", "value") VALUES ('{key}', '{escaped}'::jsonb)
                   ON CONFLICT ("key") DO UPDATE SET "value" = EXCLUDED."value";
"#
            );
        }
        let _ = write!(
            batch,
            r#"INSERT INTO {qualified} ("key", "value") VALUES ('schema_created_at', to_jsonb(NOW()::text))
               ON CONFLICT ("key") DO UPDATE SET "value" = EXCLUDED."value";"#
        );

        sqlx::raw_sql(&batch).execute(&pool).await.map_err(|e| {
            StorageError::DdlFailed(format!("metadata seed failed for {schema_name}: {e}"))
        })?;

        info!(%schema_name, "metadata seeded");

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normal_input() {
        assert_eq!(sanitize_identifier("MyProgram"), "myprogram");
    }

    #[test]
    fn sanitize_with_underscores() {
        assert_eq!(sanitize_identifier("my_cool_program"), "my_cool_program");
    }

    #[test]
    fn sanitize_strips_special_chars() {
        assert_eq!(sanitize_identifier("hello-world!@#$%"), "helloworld");
    }

    #[test]
    fn sanitize_digit_first() {
        assert_eq!(sanitize_identifier("123program"), "_123program");
    }

    #[test]
    fn sanitize_empty_input() {
        assert_eq!(sanitize_identifier(""), "_unnamed");
    }

    #[test]
    fn sanitize_all_special_chars() {
        assert_eq!(sanitize_identifier("!@#$%^&*"), "_unnamed");
    }

    #[test]
    fn sanitize_unicode_stripped() {
        // Non-ASCII alphanumerics are stripped (ASCII-only for PG identifiers)
        assert_eq!(sanitize_identifier("café"), "caf");
    }

    #[test]
    fn sanitize_cjk_produces_unnamed() {
        assert_eq!(sanitize_identifier("程序"), "_unnamed");
    }

    #[test]
    fn sanitize_truncate_63_bytes() {
        let long = "a".repeat(100);
        let result = sanitize_identifier(&long);
        assert_eq!(result.len(), 63);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn derive_schema_name_basic() {
        let name = derive_schema_name(
            "token_program",
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        );
        assert_eq!(name, "token_program_tokenkeg");
    }

    #[test]
    fn derive_schema_name_short_program_id() {
        let name = derive_schema_name("test", "ABC");
        assert_eq!(name, "test_abc");
    }

    #[test]
    fn derive_schema_name_truncates_to_63() {
        let long_name = "a".repeat(60);
        let name = derive_schema_name(&long_name, "12345678ABCDEF");
        assert!(name.len() <= 63);
        assert!(name.is_char_boundary(name.len()));
    }

    #[test]
    fn derive_schema_name_preserves_suffix_for_long_name() {
        let long_name = "a".repeat(60);
        let name = derive_schema_name(&long_name, "12345678ABCDEF");
        assert!(name.ends_with("_12345678"), "suffix lost: {name}");
        assert!(name.len() <= 63);
    }

    // ---- quote_ident tests ----

    #[test]
    fn quote_ident_normal() {
        assert_eq!(quote_ident("my_table"), r#""my_table""#);
    }

    #[test]
    fn quote_ident_with_embedded_quotes() {
        assert_eq!(quote_ident(r#"my"table"#), r#""my""table""#);
    }

    #[test]
    fn quote_ident_reserved_word() {
        assert_eq!(quote_ident("select"), r#""select""#);
    }

    #[test]
    fn quote_ident_empty() {
        assert_eq!(quote_ident(""), r#""""#);
    }

    // ---- map_idl_type_to_pg tests ----

    #[test]
    fn map_primitive_types() {
        let types = vec![];
        assert_eq!(map_idl_type_to_pg(&IdlType::Bool, &types), Some("BOOLEAN"));
        assert_eq!(map_idl_type_to_pg(&IdlType::U8, &types), Some("SMALLINT"));
        assert_eq!(map_idl_type_to_pg(&IdlType::I8, &types), Some("SMALLINT"));
        assert_eq!(map_idl_type_to_pg(&IdlType::U16, &types), Some("INTEGER"));
        assert_eq!(map_idl_type_to_pg(&IdlType::I16, &types), Some("SMALLINT"));
        assert_eq!(map_idl_type_to_pg(&IdlType::U32, &types), Some("INTEGER"));
        assert_eq!(map_idl_type_to_pg(&IdlType::I32, &types), Some("INTEGER"));
        assert_eq!(map_idl_type_to_pg(&IdlType::F32, &types), Some("REAL"));
        assert_eq!(map_idl_type_to_pg(&IdlType::U64, &types), Some("BIGINT"));
        assert_eq!(map_idl_type_to_pg(&IdlType::I64, &types), Some("BIGINT"));
        assert_eq!(
            map_idl_type_to_pg(&IdlType::F64, &types),
            Some("DOUBLE PRECISION")
        );
        assert_eq!(
            map_idl_type_to_pg(&IdlType::U128, &types),
            Some("NUMERIC(39,0)")
        );
        assert_eq!(
            map_idl_type_to_pg(&IdlType::I128, &types),
            Some("NUMERIC(39,0)")
        );
        assert_eq!(
            map_idl_type_to_pg(&IdlType::U256, &types),
            Some("NUMERIC(78,0)")
        );
        assert_eq!(
            map_idl_type_to_pg(&IdlType::I256, &types),
            Some("NUMERIC(78,0)")
        );
        assert_eq!(map_idl_type_to_pg(&IdlType::String, &types), Some("TEXT"));
        assert_eq!(map_idl_type_to_pg(&IdlType::Pubkey, &types), Some("TEXT"));
        assert_eq!(map_idl_type_to_pg(&IdlType::Bytes, &types), Some("BYTEA"));
    }

    #[test]
    fn map_option_wrapping() {
        let types = vec![];
        let opt_u64 = IdlType::Option(Box::new(IdlType::U64));
        assert_eq!(map_idl_type_to_pg(&opt_u64, &types), Some("BIGINT"));

        let opt_vec = IdlType::Option(Box::new(IdlType::Vec(Box::new(IdlType::U8))));
        assert_eq!(map_idl_type_to_pg(&opt_vec, &types), None);
    }

    #[test]
    fn map_byte_array() {
        let types = vec![];
        let u8_arr = IdlType::Array(
            Box::new(IdlType::U8),
            anchor_lang_idl_spec::IdlArrayLen::Value(32),
        );
        assert_eq!(map_idl_type_to_pg(&u8_arr, &types), Some("BYTEA"));

        let u32_arr = IdlType::Array(
            Box::new(IdlType::U32),
            anchor_lang_idl_spec::IdlArrayLen::Value(4),
        );
        assert_eq!(map_idl_type_to_pg(&u32_arr, &types), None);
    }

    #[test]
    fn map_vec_not_promoted() {
        let types = vec![];
        let vec_u64 = IdlType::Vec(Box::new(IdlType::U64));
        assert_eq!(map_idl_type_to_pg(&vec_u64, &types), None);
    }

    #[test]
    fn map_defined_type_alias() {
        let types = vec![IdlTypeDef {
            name: "MyU64".to_string(),
            docs: vec![],
            serialization: Default::default(),
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Type {
                alias: IdlType::U64,
            },
        }];
        let defined = IdlType::Defined {
            name: "MyU64".to_string(),
            generics: vec![],
        };
        assert_eq!(map_idl_type_to_pg(&defined, &types), Some("BIGINT"));
    }

    #[test]
    fn map_defined_struct_not_promoted() {
        let types = vec![IdlTypeDef {
            name: "MyStruct".to_string(),
            docs: vec![],
            serialization: Default::default(),
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![IdlField {
                    name: "x".to_string(),
                    docs: vec![],
                    ty: IdlType::U64,
                }])),
            },
        }];
        let defined = IdlType::Defined {
            name: "MyStruct".to_string(),
            generics: vec![],
        };
        assert_eq!(map_idl_type_to_pg(&defined, &types), None);
    }

    #[test]
    fn map_defined_not_found() {
        let types = vec![];
        let defined = IdlType::Defined {
            name: "Unknown".to_string(),
            generics: vec![],
        };
        assert_eq!(map_idl_type_to_pg(&defined, &types), None);
    }

    #[test]
    fn map_generic_not_promoted() {
        let types = vec![];
        assert_eq!(
            map_idl_type_to_pg(&IdlType::Generic("T".to_string()), &types),
            None
        );
    }

    #[test]
    fn map_defined_circular_alias_returns_none() {
        let types = vec![
            IdlTypeDef {
                name: "AliasA".to_string(),
                docs: vec![],
                serialization: Default::default(),
                repr: None,
                generics: vec![],
                ty: IdlTypeDefTy::Type {
                    alias: IdlType::Defined {
                        name: "AliasB".to_string(),
                        generics: vec![],
                    },
                },
            },
            IdlTypeDef {
                name: "AliasB".to_string(),
                docs: vec![],
                serialization: Default::default(),
                repr: None,
                generics: vec![],
                ty: IdlTypeDefTy::Type {
                    alias: IdlType::Defined {
                        name: "AliasA".to_string(),
                        generics: vec![],
                    },
                },
            },
        ];
        let defined = IdlType::Defined {
            name: "AliasA".to_string(),
            generics: vec![],
        };
        // Must return None instead of stack overflow
        assert_eq!(map_idl_type_to_pg(&defined, &types), None);
    }

    // ---- DDL generation tests ----

    #[test]
    fn generate_account_table_basic() {
        let fields = vec![
            IdlField {
                name: "owner".to_string(),
                docs: vec![],
                ty: IdlType::Pubkey,
            },
            IdlField {
                name: "amount".to_string(),
                docs: vec![],
                ty: IdlType::U64,
            },
            IdlField {
                name: "data_vec".to_string(),
                docs: vec![],
                ty: IdlType::Vec(Box::new(IdlType::U8)),
            },
        ];

        let stmts = generate_account_table("test_schema", "TokenAccount", &fields, &[]);
        assert_eq!(stmts.len(), 1);

        let ddl = &stmts[0];
        assert!(ddl.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(ddl.contains(r#""test_schema"."tokenaccount""#));
        assert!(ddl.contains(r#""pubkey" TEXT PRIMARY KEY"#));
        assert!(ddl.contains(r#""slot_updated" BIGINT NOT NULL"#));
        assert!(ddl.contains(r#""data" JSONB NOT NULL"#));
        // Promoted columns
        assert!(ddl.contains(r#""owner" TEXT"#));
        assert!(ddl.contains(r#""amount" BIGINT"#));
        // Vec should NOT be promoted
        assert!(!ddl.contains("data_vec"));
    }

    #[test]
    fn generate_account_table_no_promotable_fields() {
        let fields = vec![IdlField {
            name: "complex".to_string(),
            docs: vec![],
            ty: IdlType::Vec(Box::new(IdlType::U32)),
        }];

        let stmts = generate_account_table("s", "Acc", &fields, &[]);
        let ddl = &stmts[0];
        // Should still have common columns
        assert!(ddl.contains(r#""pubkey" TEXT PRIMARY KEY"#));
        // No extra promoted column
        assert!(!ddl.contains("complex"));
    }

    #[test]
    fn generate_account_table_skips_reserved_column_names() {
        let fields = vec![
            IdlField {
                name: "data".to_string(),
                docs: vec![],
                ty: IdlType::String,
            },
            IdlField {
                name: "pubkey".to_string(),
                docs: vec![],
                ty: IdlType::Pubkey,
            },
            IdlField {
                name: "amount".to_string(),
                docs: vec![],
                ty: IdlType::U64,
            },
        ];

        let stmts = generate_account_table("s", "Acc", &fields, &[]);
        let ddl = &stmts[0];
        // "amount" should be promoted (not reserved)
        assert!(ddl.contains(r#""amount" BIGINT"#));
        // "data" and "pubkey" are reserved — should NOT appear as duplicate promoted columns
        // Count occurrences of "data" — should appear exactly once (the system JSONB column)
        let data_count = ddl.matches(r#""data""#).count();
        assert_eq!(data_count, 1, "expected 1 'data' column, got {data_count}");
    }

    #[test]
    fn generate_instructions_table_structure() {
        let stmts = generate_instructions_table("my_schema");
        assert_eq!(stmts.len(), 2);

        let create = &stmts[0];
        assert!(create.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(create.contains(r#""my_schema"."_instructions""#));
        assert!(create.contains(r#""id" BIGSERIAL PRIMARY KEY"#));
        assert!(create.contains(r#""signature" TEXT NOT NULL"#));
        assert!(create.contains(r#""instruction_name" TEXT NOT NULL"#));
        assert!(create.contains(r#""is_inner_ix" BOOLEAN NOT NULL DEFAULT FALSE"#));

        let unique = &stmts[1];
        assert!(unique.contains("CREATE UNIQUE INDEX IF NOT EXISTS"));
        assert!(unique.contains(r#"COALESCE("inner_index", -1)"#));
    }

    #[test]
    fn generate_metadata_table_structure() {
        let ddl = generate_metadata_table("test");
        assert!(ddl.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(ddl.contains(r#""test"."_metadata""#));
        assert!(ddl.contains(r#""key" TEXT PRIMARY KEY"#));
        assert!(ddl.contains(r#""value" JSONB NOT NULL"#));
    }

    #[test]
    fn generate_checkpoints_table_structure() {
        let ddl = generate_checkpoints_table("test");
        assert!(ddl.contains("CREATE TABLE IF NOT EXISTS"));
        assert!(ddl.contains(r#""test"."_checkpoints""#));
        assert!(ddl.contains(r#""stream" TEXT PRIMARY KEY"#));
        assert!(ddl.contains(r#""last_slot" BIGINT"#));
        assert!(ddl.contains(r#""last_signature" VARCHAR(88)"#));
    }

    #[test]
    fn generate_indexes_produces_expected() {
        let account_names = vec!["TokenAccount".to_string()];
        let stmts = generate_indexes("s", &account_names);

        // Account: 1 B-tree (slot_updated) + 1 GIN (data) = 2
        // Instructions: 4 B-tree (slot, signature, instruction_name, block_time) + 2 GIN (data, args) = 6
        assert_eq!(stmts.len(), 8);

        let joined = stmts.join("\n");
        assert!(joined.contains("idx_tokenaccount_slot_updated"));
        assert!(joined.contains("gin_tokenaccount_data"));
        assert!(joined.contains("idx__instructions_slot"));
        assert!(joined.contains("idx__instructions_signature"));
        assert!(joined.contains("idx__instructions_instruction_name"));
        assert!(joined.contains("idx__instructions_block_time"));
        assert!(joined.contains("gin__instructions_data"));
        assert!(joined.contains("gin__instructions_args"));
    }

    // ---- Full build_ddl_statements test ----

    #[test]
    fn build_ddl_from_fixture_idl() {
        let json = include_str!("../../tests/fixtures/idls/simple_v030.json");
        let idl: Idl = serde_json::from_str(json).expect("fixture IDL should parse");

        let schema = "simple_test_program_11111111";
        let stmts = build_ddl_statements(&idl, schema);

        // CREATE SCHEMA + _metadata + _checkpoints + 1 account table + _instructions + unique index + indexes
        assert!(
            stmts.len() >= 7,
            "expected at least 7 DDL statements, got {}",
            stmts.len()
        );

        let joined = stmts.join("\n");

        // Schema creation
        assert!(joined.contains(&format!(
            "CREATE SCHEMA IF NOT EXISTS {}",
            quote_ident(schema)
        )));

        // Account table with promoted column
        assert!(joined.contains(r#""value" BIGINT"#));

        // Instructions table
        assert!(joined.contains(r#""_instructions""#));

        // All statements use IF NOT EXISTS or IF NOT EXISTS
        for stmt in &stmts {
            assert!(
                stmt.contains("IF NOT EXISTS"),
                "statement missing IF NOT EXISTS: {stmt}"
            );
        }
    }
}
