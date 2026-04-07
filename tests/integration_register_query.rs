// Story 6.5 AC3: registration → schema → query integration test.
//
// Drives the production registration → schema-generation → writer → query
// path against a real PostgreSQL container without going through axum.
// (axum routing is Story 6.6's job — see `axum-test` work landing there.)
//
// All assertions reference the bundled `tests/fixtures/idls/simple_v030.json`
// fixture so a future reader can map every step back to a concrete IDL.

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;
use sqlx::Row;

use solarix::api::filters::{parse_filters, resolve_filters, FilterContext};
use solarix::idl::IdlManager;
use solarix::registry::ProgramRegistry;
use solarix::storage::queries::{build_query, QueryTarget};
use solarix::storage::schema::quote_ident;
use solarix::storage::writer::StorageWriter;
use solarix::types::{DecodedAccount, DecodedInstruction};

mod common;
use common::postgres::with_postgres;

const SIMPLE_IDL_JSON: &str = include_str!("fixtures/idls/simple_v030.json");
const PROGRAM_ID: &str = "Testc11111111111111111111111111111111111111";

#[tokio::test]
async fn registration_to_query_round_trip() {
    with_postgres(|pool| async move {
        // Step 1: Construct registry against the container's pool.
        let idl_manager = IdlManager::new("http://localhost:8899".to_string());
        let mut registry = ProgramRegistry::new(idl_manager);

        // Step 2: prepare_registration with the bundled simple_v030.json IDL.
        let data = registry
            .prepare_registration(PROGRAM_ID.to_string(), Some(SIMPLE_IDL_JSON.to_string()))
            .expect("prepare_registration should succeed for tests/fixtures/idls/simple_v030.json");

        let schema_name = data.schema_name.clone();

        // Step 3: commit_registration creates the schema, the typed account
        // table, and the _instructions / _checkpoints / _metadata tables.
        let info = ProgramRegistry::commit_registration(pool.clone(), data)
            .await
            .expect("commit_registration should succeed");
        assert_eq!(info.status, "schema_created");
        assert_eq!(info.schema_name, schema_name);

        // Step 4: schema exists in information_schema.
        let row: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM information_schema.schemata WHERE schema_name = $1",
        )
        .bind(&schema_name)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, 1, "expected schema {schema_name} to exist");

        // Step 5: _instructions table exists with the promoted `value BIGINT` column.
        let cols: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name, data_type FROM information_schema.columns
             WHERE table_schema = $1 AND table_name = '_instructions'
             ORDER BY ordinal_position",
        )
        .bind(&schema_name)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            cols.iter().any(|(c, _)| c == "signature"),
            "_instructions missing signature column"
        );
        assert!(
            cols.iter().any(|(c, _)| c == "slot"),
            "_instructions missing slot column"
        );
        // simple_v030.json's `initialize` ix has a `value: u64` arg, but
        // promoted columns live on account tables, not _instructions. The
        // _instructions table only carries the fixed columns + JSONB args.
        // Verified by AC3 step 6 below where we assert on the typed account
        // table's promoted column instead.

        // Step 6: data_account table exists with the promoted `value BIGINT` column.
        let acct_cols: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name, data_type FROM information_schema.columns
             WHERE table_schema = $1 AND table_name = 'dataaccount'
             ORDER BY ordinal_position",
        )
        .bind(&schema_name)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            acct_cols.iter().any(|(c, t)| c == "value" && t == "bigint"),
            "dataaccount missing promoted `value BIGINT` column, got: {acct_cols:?}"
        );
        assert!(
            acct_cols.iter().any(|(c, _)| c == "pubkey"),
            "dataaccount missing pubkey column"
        );

        // Step 7: insert a synthetic decoded instruction via StorageWriter.
        let writer = StorageWriter::new(pool.clone());

        let ix = DecodedInstruction {
            signature: "test_sig".to_string(),
            slot: 100,
            block_time: Some(1_700_000_000),
            instruction_name: "initialize".to_string(),
            args: json!({ "value": 4_242_424_242u64 }),
            program_id: PROGRAM_ID.to_string(),
            accounts: vec!["payer".to_string(), "system_program".to_string()],
            instruction_index: 0,
            inner_index: None,
        };

        let result = writer
            .write_block(
                &schema_name,
                "backfill",
                &[ix.clone()],
                &[],
                100,
                Some("test_sig"),
            )
            .await
            .expect("write_block should insert the synthetic instruction");
        assert_eq!(result.instructions_written, 1);

        // Step 8: build a query for instruction_name = 'initialize' and execute.
        let mut params = std::collections::HashMap::new();
        params.insert("instruction_name".to_string(), "initialize".to_string());
        let parsed = parse_filters(&params);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions)
            .expect("resolve_filters should accept instruction_name");

        let target = QueryTarget::Instructions {
            schema: schema_name.clone(),
        };
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb
            .build()
            .fetch_all(&pool)
            .await
            .expect("query should execute");

        assert_eq!(rows.len(), 1, "expected exactly one instruction row");
        let row = &rows[0];
        assert_eq!(row.get::<i64, _>("slot"), 100i64);
        assert_eq!(row.get::<&str, _>("signature"), "test_sig");
        assert_eq!(
            row.get::<&str, _>("instruction_name"),
            "initialize",
            "instruction_name should round-trip from the writer"
        );

        // Step 9: insert a synthetic decoded account and assert upsert behaviour.
        let acct = DecodedAccount {
            pubkey: "AcctTesT11111111111111111111111111111111111".to_string(),
            slot_updated: 100,
            lamports: 2_039_280,
            data: json!({ "value": 7_777u64 }),
            account_type: "DataAccount".to_string(),
            program_id: PROGRAM_ID.to_string(),
        };

        writer
            .write_block(&schema_name, "backfill", &[], &[acct.clone()], 100, None)
            .await
            .expect("first account upsert should succeed");

        // Re-insert at higher slot — should override.
        let mut acct_v2 = acct.clone();
        acct_v2.slot_updated = 110;
        acct_v2.data = json!({ "value": 9_999u64 });
        writer
            .write_block(&schema_name, "backfill", &[], &[acct_v2], 110, None)
            .await
            .expect("higher-slot upsert should succeed");

        let after_higher: (i64, i64) = sqlx::query_as(&format!(
            r#"SELECT "slot_updated", "value" FROM {}.{} WHERE "pubkey" = $1"#,
            quote_ident(&schema_name),
            quote_ident("dataaccount"),
        ))
        .bind(&acct.pubkey)
        .fetch_one(&pool)
        .await
        .expect("dataaccount row should exist after upsert");
        assert_eq!(after_higher.0, 110, "slot_updated should advance");
        assert_eq!(
            after_higher.1, 9_999,
            "promoted value should reflect higher-slot upsert"
        );

        // Re-insert at LOWER slot — should be ignored.
        let mut acct_old = acct.clone();
        acct_old.slot_updated = 50;
        acct_old.data = json!({ "value": 1u64 });
        writer
            .write_block(&schema_name, "backfill", &[], &[acct_old], 50, None)
            .await
            .expect("lower-slot upsert call should still succeed (no error)");

        let after_lower: (i64, i64) = sqlx::query_as(&format!(
            r#"SELECT "slot_updated", "value" FROM {}.{} WHERE "pubkey" = $1"#,
            quote_ident(&schema_name),
            quote_ident("dataaccount"),
        ))
        .bind(&acct.pubkey)
        .fetch_one(&pool)
        .await
        .expect("dataaccount row should still exist");
        assert_eq!(
            after_lower.0, 110,
            "lower-slot re-insert must NOT overwrite (writer's monotonic upsert guarantee)"
        );
        assert_eq!(
            after_lower.1, 9_999,
            "lower-slot re-insert must NOT overwrite the promoted value"
        );

        // Step 10: drop the schema explicitly (defense in depth — testcontainers
        // will tear down the whole container anyway).
        let drop_ddl = format!(r#"DROP SCHEMA {} CASCADE"#, quote_ident(&schema_name));
        sqlx::raw_sql(&drop_ddl)
            .execute(&pool)
            .await
            .expect("explicit DROP SCHEMA should succeed");
    })
    .await;
}
