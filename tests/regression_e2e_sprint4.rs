// Sprint-4 e2e gate regression tests (Story 6.6 AC5, AC6).
//
// GATES STORY 4.4: `test_idl_json_persisted_and_loaded_on_restart` is the
// sole remaining open item on Story 4.4 (Task 8, folded here on 2026-04-07).
// When this test is green, flip sprint-status.yaml `4-4-*` → done.
//
// Sprint-4 bug regression matrix:
//
// | Bug                                              | Regression                                         | Owner     |
// |--------------------------------------------------|----------------------------------------------------|-----------|
// | Wrong IDL PDA derivation                         | tests/idl_address_vectors.rs (5 vectors)           | Story 6.4 |
// | programs.idl_json missing → no auto-start        | test_idl_json_persisted_and_loaded_on_restart (HERE) | 6.6 AC6  |
// | slot_gt=N → bigint > text (builder/exec level)   | tests/filter_sql_exec.rs                           | Story 6.5 |
// | slot_gt=N → bigint > text (API level)            | test_promoted_column_filter_with_bigint_value (HERE) | 6.6 AC5  |
// | docker-compose.yml hardcoded 'pretty' log format | test_docker_compose_log_format_is_json_by_default (HERE) | 6.6 AC5 |
// | .env.example missing 6 vars                      | test_env_example_documents_every_config_field (HERE) | 6.6 AC5  |
// | Invalid program_id → 500 (historical)            | test_invalid_program_id_returns_400 (HERE)         | 6.6 AC5   |
// | bootstrap_system_tables idl_json upgrade path    | tests/bootstrap_test.rs::bootstrap_creates_idl_json_column_on_upgrade | Story 6.4 |

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use serde_json::{json, Value};

mod common;
use common::api::{build_test_router, oneshot_get, oneshot_post_json};
use common::postgres::with_postgres;

use solarix::idl::{IdlManager, IdlSource};
use solarix::registry::ProgramRegistry;
use solarix::startup::query_registered_programs;
use solarix::storage::schema::derive_schema_name;
use solarix::storage::writer::StorageWriter;
use solarix::types::DecodedInstruction;

const SIMPLE_IDL_JSON: &str = include_str!("fixtures/idls/simple_v030.json");

// ---------------------------------------------------------------------------
// AC6: GATES STORY 4.4
// test_idl_json_persisted_and_loaded_on_restart
// ---------------------------------------------------------------------------

/// GATES STORY 4.4.
///
/// Bug (Sprint-4 e2e gate, commit 797bf74): `programs.idl_json` column was
/// missing from the initial schema. After the fix the column is populated by
/// `write_registration`. This test confirms:
///
///   1. A registration via the API populates `idl_json` synchronously.
///   2. `query_registered_programs` reads and parses it after an in-memory
///      state reset (simulating a container restart).
///   3. The loaded IDL can be used to seed a new ProgramRegistry cache.
///   4. Rows with NULL or unparseable `idl_json` are silently skipped.
#[tokio::test]
async fn test_idl_json_persisted_and_loaded_on_restart() {
    // The test program ID and its expected schema name.
    const RESTART_PROGRAM_ID: &str = "Testd11111111111111111111111111111111111111";
    let idl_name = {
        let v: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
        v["metadata"]["name"].as_str().unwrap().to_string()
    };
    let expected_schema = derive_schema_name(&idl_name, RESTART_PROGRAM_ID);

    with_postgres(|pool| async move {
        // -----------------------------------------------------------------
        // Step 1: Register via the API handler (POST /api/programs).
        // Goes through the full register_program → do_register →
        // prepare_registration → commit_registration → write_registration
        // chain, which is the path that populates idl_json.
        // -----------------------------------------------------------------
        let router = build_test_router(pool.clone()).await;
        let idl_val: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
        let (status, _, body) = oneshot_post_json(
            router.clone(),
            "/api/programs",
            json!({ "program_id": RESTART_PROGRAM_ID, "idl": idl_val }),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "registration failed; body={body}");

        // -----------------------------------------------------------------
        // Step 2: Assert idl_json IS NOT NULL via raw SQL.
        // The handler awaits the spawned task, so this is synchronous.
        // -----------------------------------------------------------------
        let is_not_null: (bool,) = sqlx::query_as(
            r#"SELECT "idl_json" IS NOT NULL FROM "programs" WHERE "program_id" = $1"#,
        )
        .bind(RESTART_PROGRAM_ID)
        .fetch_one(&pool)
        .await
        .expect("failed to query idl_json IS NOT NULL");
        assert!(is_not_null.0, "idl_json must be non-NULL after registration via API");

        // -----------------------------------------------------------------
        // Step 3: Simulate container restart.
        // Drop the in-memory AppState (registry + stats) by letting `router`
        // go out of scope. The DB pool stays alive (testcontainer still up).
        // -----------------------------------------------------------------
        drop(router);

        // -----------------------------------------------------------------
        // Step 4: Call query_registered_programs to load programs from DB.
        // This is the exact code path that was broken before commit 797bf74.
        // -----------------------------------------------------------------
        let programs = query_registered_programs(&pool)
            .await
            .expect("query_registered_programs must succeed");

        assert_eq!(programs.len(), 1, "expected 1 program loaded after restart simulation");
        let p = &programs[0];
        assert_eq!(p.program_id, RESTART_PROGRAM_ID);
        assert_eq!(p.schema_name, expected_schema, "schema_name mismatch");

        // IDL must be parseable and contain the expected instruction + account type
        assert!(
            p.idl.instructions.iter().any(|i| i.name == "initialize"),
            "loaded IDL must contain 'initialize' instruction"
        );
        assert!(
            p.idl.accounts.iter().any(|a| a.name == "DataAccount"),
            "loaded IDL must contain 'DataAccount' account type"
        );
        assert_eq!(
            p.idl.metadata.name, "simple_test_program",
            "IDL name mismatch after reload"
        );

        // -----------------------------------------------------------------
        // Step 5: Seed a new ProgramRegistry from the loaded StartupProgram
        // and assert the IDL cache is populated.
        // -----------------------------------------------------------------
        let new_idl_manager = IdlManager::new("http://localhost:8899".to_string());
        let mut new_registry = ProgramRegistry::new(new_idl_manager);
        new_registry
            .idl_manager
            .insert_fetched_idl(&p.program_id, &p.idl_json, IdlSource::OnChain)
            .expect("insert_fetched_idl from persisted bytes must succeed");

        let cached = new_registry
            .idl_manager
            .get_cached(&p.program_id)
            .expect("IDL must be in cache after insert_fetched_idl");
        assert_eq!(cached.metadata.name, "simple_test_program");

        // -----------------------------------------------------------------
        // Step 6: Negative coverage — NULL idl_json row is skipped.
        // -----------------------------------------------------------------
        sqlx::query(
            r#"INSERT INTO "programs"
               ("program_id", "program_name", "schema_name", "idl_hash", "idl_source", "status", "idl_json")
               VALUES ($1, $2, $3, $4, $5, $6, NULL)"#,
        )
        .bind("NullIdlProgram11111111111111111111111111111")
        .bind("null_idl_prog")
        .bind("null_idl_prog_nullidlp")
        .bind("deadbeef")
        .bind("manual")
        .bind("schema_created")
        .execute(&pool)
        .await
        .expect("insert NULL-idl_json row failed");

        let after_null = query_registered_programs(&pool)
            .await
            .expect("query_registered_programs must succeed even with NULL row");
        assert_eq!(
            after_null.len(),
            1,
            "NULL idl_json row must be skipped; only the original program should load"
        );

        // -----------------------------------------------------------------
        // Step 7: Negative coverage — unparseable idl_json row is skipped.
        // -----------------------------------------------------------------
        sqlx::query(
            r#"INSERT INTO "programs"
               ("program_id", "program_name", "schema_name", "idl_hash", "idl_source", "status", "idl_json")
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind("BadIdlProgram111111111111111111111111111111")
        .bind("bad_idl_prog")
        .bind("bad_idl_prog_badidlpr")
        .bind("deadbeef2")
        .bind("manual")
        .bind("schema_created")
        .bind(r#"{"not": "a valid anchor idl"}"#)
        .execute(&pool)
        .await
        .expect("insert bad-idl_json row failed");

        let after_bad = query_registered_programs(&pool)
            .await
            .expect("query_registered_programs must succeed even with unparseable row");
        assert_eq!(
            after_bad.len(),
            1,
            "unparseable idl_json row must be skipped; only the original program should load"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// AC5 Test 2: API-level bigint > text regression
// Complementary to tests/filter_sql_exec.rs (builder/exec level).
// Bug fixed in commit 243a0de.
// ---------------------------------------------------------------------------

/// Sprint-4 bug: `slot_gt=100` on a BIGINT promoted column triggered PostgreSQL
/// error "operator does not exist: bigint > text" because the old query builder
/// emitted `"slot" > $1::TEXT` instead of `"slot"::BIGINT > $1::BIGINT`.
///
/// Fix: commit 243a0de patched `append_filter_clause` in `src/storage/queries.rs`
/// to emit `::BIGINT` casts for promoted numeric columns.
///
/// See also: `tests/filter_sql_exec.rs::test_bigint_filter_does_not_emit_text_cast`
/// (builder/exec level pin). This test is the API-layer pin — it guards the
/// handler's `resolve_filters` path, which a future refactor could bypass.
#[tokio::test]
async fn test_promoted_column_filter_with_bigint_value() {
    const PROG_ID: &str = "Testf11111111111111111111111111111111111111";

    with_postgres(|pool| async move {
        let router = build_test_router(pool.clone()).await;
        let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();

        // Register with a program ID whose schema name will use "testfilt" prefix
        let (s, _, _) = oneshot_post_json(
            router.clone(),
            "/api/programs",
            json!({ "program_id": PROG_ID, "idl": idl }),
        )
        .await;
        assert_eq!(s, axum::http::StatusCode::CREATED);

        let schema_name = {
            let v: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            let name = v["metadata"]["name"].as_str().unwrap().to_string();
            derive_schema_name(&name, PROG_ID)
        };

        // Seed rows at slots 50 and 200
        let writer = StorageWriter::new(pool.clone());
        for &slot in &[50u64, 200u64] {
            let ix = DecodedInstruction {
                signature: format!("bigint_sig_{slot}"),
                slot,
                block_time: Some(1_700_000_000),
                instruction_name: "initialize".to_string(),
                args: json!({ "value": slot }),
                program_id: PROG_ID.to_string(),
                accounts: vec![],
                instruction_index: 0,
                inner_index: None,
            };
            writer
                .write_block(&schema_name, "backfill", &[ix], &[], slot, None)
                .await
                .unwrap();
        }

        // Filter: slot_gt=100 — should return only the slot=200 row
        let (status, _, body) = oneshot_get(
            router,
            &format!("/api/programs/{PROG_ID}/instructions/initialize?slot_gt=100"),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "slot_gt=100 filter must return 200, not 500 (bigint > text regression); body={body}"
        );
        let data = body["data"].as_array().expect("data must be array");
        assert_eq!(
            data.len(),
            1,
            "slot_gt=100 should return exactly 1 row (slot=200); got: {data:?}"
        );
        let slot_val = data[0]["slot"].as_i64().expect("slot must be i64");
        assert_eq!(slot_val, 200, "returned row must be slot=200");

        assert!(
            schema_name.starts_with("simple_test_program_"),
            "schema_name pattern check; schema={schema_name}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// AC5 Test 3: docker-compose.yml log format default is json
// Bug: was hardcoded "pretty" before the Sprint-4 fix.
// ---------------------------------------------------------------------------

/// Sprint-4 bug: `docker-compose.yml` hardcoded `SOLARIX_LOG_FORMAT: "pretty"`
/// instead of defaulting to `json`. Fixed inline during the e2e gate.
///
/// This test pins the current value so a future edit can't regress it silently.
#[test]
fn test_docker_compose_log_format_is_json_by_default() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docker-compose.yml");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read docker-compose.yml: {e}"));

    assert!(
        content.contains(r#"SOLARIX_LOG_FORMAT: "${SOLARIX_LOG_FORMAT:-json}""#),
        "docker-compose.yml must have SOLARIX_LOG_FORMAT default set to json. \
         Regression: was previously hardcoded 'pretty'."
    );
}

// ---------------------------------------------------------------------------
// AC5 Test 4: .env.example documents every Config field
// Bug: Sprint-4 gate found 6 env vars missing from .env.example.
// ---------------------------------------------------------------------------

/// Sprint-4 bug: `.env.example` was missing 6 env var entries that Config
/// had defined, causing operators to miss required configuration.
///
/// This test does a bidirectional check:
///   Forward:  every Config env var must appear in .env.example
///   Reverse:  every SOLARIX_*/SOLANA_*/DATABASE_URL token in .env.example
///             must have a matching Config field
#[test]
fn test_env_example_documents_every_config_field() {
    use clap::CommandFactory;

    let cmd = solarix::config::Config::command();

    let env_names: Vec<String> = cmd
        .get_arguments()
        .filter_map(|arg| arg.get_env().and_then(|s| s.to_str()).map(String::from))
        .collect();

    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env.example");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read .env.example: {e}"));

    // Forward: every Config env var appears in .env.example
    for name in &env_names {
        assert!(
            content.contains(name.as_str()),
            ".env.example missing env var: {name}. \
             Every Config field with an `env = \"...\"` attribute must appear in .env.example \
             (commented out with `# VAR=` is fine for optional fields)."
        );
    }

    // Reverse: every SOLARIX_*/SOLANA_*/DATABASE_URL token in .env.example
    // has a matching Config get_env() name.
    for line in content.lines() {
        let trimmed = line.trim_start_matches('#').trim();
        if let Some((var, _)) = trimmed.split_once('=') {
            let var = var.trim();
            if var.starts_with("SOLARIX_") || var.starts_with("SOLANA_") || var == "DATABASE_URL" {
                assert!(
                    env_names.contains(&var.to_string()),
                    ".env.example has stale entry '{var}' that no Config field documents"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AC5 Test 5: invalid program_id returns 400, not 500
// ---------------------------------------------------------------------------

/// Locks the behavior: POST /api/programs with `program_id: "not-base58"`
/// must return 400 INVALID_REQUEST.
///
/// Pins the decision against a future "422 instead" refactor.
#[tokio::test]
async fn test_invalid_program_id_returns_400() {
    with_postgres(|pool| async move {
        let router = build_test_router(pool).await;
        let (status, _, body) = oneshot_post_json(
            router,
            "/api/programs",
            json!({ "program_id": "not-base58" }),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::BAD_REQUEST,
            "invalid program_id must return 400 INVALID_REQUEST; body={body}"
        );
        assert_eq!(
            body["error"]["code"], "INVALID_REQUEST",
            "error code must be INVALID_REQUEST; body={body}"
        );
        assert!(
            body["error"]["message"].is_string(),
            "error must have string message"
        );
        // Must NOT return 500
        assert_ne!(
            status,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "must not return 500 for bad program_id"
        );
    })
    .await;
}
