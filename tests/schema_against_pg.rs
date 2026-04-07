// Story 6.5 AC5: schema generator runs against a real PostgreSQL.
//
// Replaces the Story 6.4 stubs (`#[ignore]`'d, hand-rolled `setup_pool`)
// with the testcontainers harness from `tests/common/postgres.rs`.
//
// Two tests:
//
//   1. `schema_generator_produces_expected_columns_in_real_pg`
//      Walks all three bundled fixture IDLs (`simple_v030.json`,
//      `all_types.json`, `reserved_collision_v030.json`), introspects
//      `information_schema.columns`, and asserts the column type mapping
//      matches the IDL field types end-to-end.
//
//   2. `schema_generator_is_idempotent_in_real_pg`
//      Calls `generate_schema` twice with identical inputs, asserts the
//      second call is a no-op (sentinel row inserted between calls is
//      preserved). Calls `bootstrap_system_tables` twice as well.
//
// Both tests use `information_schema` (the portable PG catalog surface),
// not `pg_attribute` or `\d` — see Task 6 in the story spec.

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use anchor_lang_idl_spec::Idl;
use sqlx::PgPool;

use solarix::storage::bootstrap_system_tables;
use solarix::storage::schema::generate_schema;

mod common;
use common::postgres::with_postgres;

const SIMPLE_IDL: &str = include_str!("fixtures/idls/simple_v030.json");
const ALL_TYPES_IDL: &str = include_str!("fixtures/idls/all_types.json");
const RESERVED_IDL: &str = include_str!("fixtures/idls/reserved_collision_v030.json");

async fn fetch_columns(pool: &PgPool, schema: &str, table: &str) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT column_name, data_type FROM information_schema.columns
         WHERE table_schema = $1 AND table_name = $2
         ORDER BY ordinal_position",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .expect("information_schema.columns query should succeed")
}

fn assert_has_column(cols: &[(String, String)], name: &str, expected_type: &str) {
    let found = cols.iter().any(|(c, t)| c == name && t == expected_type);
    assert!(
        found,
        "expected column {name} {expected_type}; got: {cols:?}"
    );
}

#[tokio::test]
async fn schema_generator_produces_expected_columns_in_real_pg() {
    with_postgres(|pool| async move {
        // ----- Fixture 1: simple_v030.json -----
        let simple: Idl = serde_json::from_str(SIMPLE_IDL).expect("simple fixture parses");
        generate_schema(
            pool.clone(),
            simple,
            "ProgIdSimple1111111".to_string(),
            "test_schema_simple".to_string(),
        )
        .await
        .expect("generate_schema(simple) should succeed");

        // dataaccount: pubkey TEXT, slot_updated BIGINT, lamports BIGINT,
        //              data JSONB, is_closed BOOLEAN, updated_at TIMESTAMPTZ,
        //              promoted value BIGINT
        let cols = fetch_columns(&pool, "test_schema_simple", "dataaccount").await;
        assert_has_column(&cols, "pubkey", "text");
        assert_has_column(&cols, "slot_updated", "bigint");
        assert_has_column(&cols, "lamports", "bigint");
        assert_has_column(&cols, "data", "jsonb");
        assert_has_column(&cols, "is_closed", "boolean");
        assert_has_column(&cols, "updated_at", "timestamp with time zone");
        assert_has_column(&cols, "value", "bigint");

        // _instructions fixed columns.
        let cols = fetch_columns(&pool, "test_schema_simple", "_instructions").await;
        assert_has_column(&cols, "signature", "text");
        assert_has_column(&cols, "slot", "bigint");
        assert_has_column(&cols, "block_time", "bigint");
        assert_has_column(&cols, "instruction_name", "text");
        assert_has_column(&cols, "instruction_index", "smallint");
        assert_has_column(&cols, "is_inner_ix", "boolean");
        assert_has_column(&cols, "data", "jsonb");

        // ----- Fixture 2: all_types.json -----
        // Exercises every promoted-column type mapping (BIGINT for u64/i64,
        // SMALLINT for u8/u16/i8/i16, INTEGER for u32/i32, TEXT for
        // pubkey/string, REAL/DOUBLE PRECISION for f32/f64, NUMERIC(39,0)
        // for u128/i128, BYTEA for [u8;N]).
        let all_types: Idl = serde_json::from_str(ALL_TYPES_IDL).expect("all_types fixture parses");
        generate_schema(
            pool.clone(),
            all_types,
            "ProgIdAllTypes11111".to_string(),
            "test_schema_all_types".to_string(),
        )
        .await
        .expect("generate_schema(all_types) should succeed");

        let cols = fetch_columns(&pool, "test_schema_all_types", "allprimitivesaccount").await;
        assert_has_column(&cols, "authority", "text"); // Pubkey
        assert_has_column(&cols, "counter", "bigint"); // u64
        assert_has_column(&cols, "small_flag", "boolean"); // bool
        assert_has_column(&cols, "tiny_u", "smallint"); // u8
        assert_has_column(&cols, "tiny_i", "smallint"); // i8
        assert_has_column(&cols, "medium_u", "integer"); // u32
        assert_has_column(&cols, "medium_i", "integer"); // i32
        assert_has_column(&cols, "float32", "real"); // f32
        assert_has_column(&cols, "float64", "double precision"); // f64
        assert_has_column(&cols, "huge_unsigned", "numeric"); // u128 → NUMERIC(39,0)
        assert_has_column(&cols, "huge_signed", "numeric"); // i128
        assert_has_column(&cols, "label", "text"); // String
        assert_has_column(&cols, "blob", "bytea"); // Bytes
        assert_has_column(&cols, "fixed_key", "bytea"); // [u8;32]
        assert_has_column(&cols, "trailing_opt", "bigint"); // Option<u64>

        // ----- Fixture 3: reserved_collision_v030.json -----
        // Asserts (a) reserved column-name fields are NOT promoted,
        // (b) digit-prefix table name was sanitized to `_9digitprefixaccount`,
        // (c) SQL-reserved-word fields (`select`, `from`, `where`) are
        // properly quoted by `quote_ident` so the DDL was accepted by PG.
        let reserved: Idl = serde_json::from_str(RESERVED_IDL).expect("reserved fixture parses");
        generate_schema(
            pool.clone(),
            reserved,
            "ProgIdReserved11111".to_string(),
            "test_schema_reserved".to_string(),
        )
        .await
        .expect("generate_schema(reserved) should succeed");

        let cols = fetch_columns(&pool, "test_schema_reserved", "reservedcollisionaccount").await;
        // Real (non-reserved) field — must be promoted.
        assert_has_column(&cols, "real_column", "bigint");
        assert_has_column(&cols, "another_real", "text");
        // Reserved fields collide with the system columns and are NOT promoted
        // a second time. The system columns themselves still exist (they're
        // part of the fixed account-table preamble).
        assert_has_column(&cols, "data", "jsonb"); // system, not the IDL `data: string`
        assert_has_column(&cols, "pubkey", "text"); // system primary key

        // (b) Digit-prefix table is sanitized via `_` prefix.
        let cols_9 = fetch_columns(&pool, "test_schema_reserved", "_9digitprefixaccount").await;
        assert!(
            !cols_9.is_empty(),
            "_9digitprefixaccount table should exist after sanitization"
        );
        // The digit-prefix field becomes `_9start_field`. The `normal_field`
        // (string) is the only one with a clean column name; the digit-prefix
        // field becomes `_9start_field` after sanitization.
        assert_has_column(&cols_9, "normal_field", "text");
        assert_has_column(&cols_9, "_9start_field", "bigint");

        // (c) Reserved-word identifiers (`select`, `from`, `where`) — the
        // generator uses `quote_ident` for every column write, so the DDL
        // would have errored if any quoting was missing. The mere fact that
        // the table exists with all three columns is the live-DB proof.
        let cols_sel = fetch_columns(&pool, "test_schema_reserved", "selectaccount").await;
        assert_has_column(&cols_sel, "select", "bigint"); // u64
        assert_has_column(&cols_sel, "from", "text"); // string
        assert_has_column(&cols_sel, "where", "boolean"); // bool
    })
    .await;
}

#[tokio::test]
async fn schema_generator_is_idempotent_in_real_pg() {
    with_postgres(|pool| async move {
        // bootstrap_system_tables idempotency (complementary to
        // tests/bootstrap_test.rs::bootstrap_is_idempotent which uses a
        // local DB instead of a fresh container).
        bootstrap_system_tables(&pool)
            .await
            .expect("second bootstrap should be a no-op");
        bootstrap_system_tables(&pool)
            .await
            .expect("third bootstrap should still be a no-op");

        // generate_schema twice with identical inputs.
        let idl: Idl = serde_json::from_str(SIMPLE_IDL).expect("fixture parses");
        generate_schema(
            pool.clone(),
            idl.clone(),
            "ProgIdIdem111111".to_string(),
            "test_schema_idem".to_string(),
        )
        .await
        .expect("first generate_schema should succeed");

        // Insert a sentinel row that the second `generate_schema` call
        // must NOT overwrite or drop (proves the IF NOT EXISTS guards are
        // doing their job).
        sqlx::query(
            r#"INSERT INTO "test_schema_idem"."dataaccount"
                ("pubkey", "slot_updated", "lamports", "data", "value")
               VALUES ('SentinelPubkey', 42, 100, '{"value": 7}'::jsonb, 7)"#,
        )
        .execute(&pool)
        .await
        .expect("sentinel insert should succeed");

        // Second call — must be idempotent (no errors, no data loss).
        generate_schema(
            pool.clone(),
            idl,
            "ProgIdIdem111111".to_string(),
            "test_schema_idem".to_string(),
        )
        .await
        .expect("second generate_schema should be a no-op");

        let row: (i64, i64) = sqlx::query_as(
            r#"SELECT "slot_updated", "value" FROM "test_schema_idem"."dataaccount"
               WHERE "pubkey" = 'SentinelPubkey'"#,
        )
        .fetch_one(&pool)
        .await
        .expect("sentinel row should still exist after second generate_schema");
        assert_eq!(row.0, 42);
        assert_eq!(row.1, 7);
    })
    .await;
}
