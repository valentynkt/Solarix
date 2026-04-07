// `expect` is fine in integration test helpers; clippy's `allow-expect-in-tests`
// knob only exempts `#[test]` fns and `#[cfg(test)]` modules.
#![allow(clippy::expect_used)]

use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

use solarix::idl::IdlManager;
use solarix::registry::{ProgramRegistry, RegistrationError};
use solarix::storage::schema::derive_schema_name;

fn sample_idl_json() -> String {
    serde_json::json!({
        "address": "11111111111111111111111111111111",
        "metadata": {
            "name": "test_program",
            "version": "0.1.0",
            "spec": "0.1.0"
        },
        "instructions": [],
        "accounts": [],
        "types": []
    })
    .to_string()
}

async fn setup_pool() -> sqlx::PgPool {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://solarix:solarix@localhost:5432/solarix".to_string());
    let pool = PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect to test database");

    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .expect("bootstrap failed");

    pool
}

async fn cleanup(pool: &sqlx::PgPool, program_id: &str) {
    // Drop generated schema (ignore errors if it doesn't exist)
    let schema_name = derive_schema_name("test_program", program_id);
    let drop_ddl = format!(
        "DROP SCHEMA IF EXISTS \"{}\" CASCADE",
        schema_name.replace('"', "\"\"")
    );
    let _ = sqlx::raw_sql(&drop_ddl).execute(pool).await;

    // Clean up test data (ignore errors if rows don't exist)
    let _ = sqlx::query(r#"DELETE FROM "indexer_state" WHERE "program_id" = $1"#)
        .bind(program_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "programs" WHERE "program_id" = $1"#)
        .bind(program_id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn register_program_creates_db_rows() {
    let pool = setup_pool().await;
    // Valid base58 Solana pubkey for test isolation
    let program_id = "Testa1111111111111111111111111111111111111111";

    cleanup(&pool, program_id).await;

    let idl_manager = IdlManager::new("http://localhost:8899".to_string());
    let mut registry = ProgramRegistry::new(idl_manager);

    let idl_json = sample_idl_json();
    let data = registry
        .prepare_registration(program_id.to_string(), Some(idl_json.clone()))
        .expect("prepare should succeed");

    let info = ProgramRegistry::commit_registration(pool.clone(), data)
        .await
        .expect("commit should succeed");

    assert_eq!(info.program_id, program_id);
    assert_eq!(info.program_name, "test_program");
    assert_eq!(info.idl_source, "manual");
    assert_eq!(info.status, "schema_created");
    assert!(!info.idl_hash.is_empty());
    assert_eq!(info.schema_name, "test_program_testa111");

    // Verify programs row
    let row = sqlx::query(r#"SELECT "program_name", "schema_name", "status", "idl_source" FROM "programs" WHERE "program_id" = $1"#)
        .bind(program_id)
        .fetch_one(&pool)
        .await
        .expect("programs row should exist");

    let name: String = row.get("program_name");
    let status: String = row.get("status");
    let source: String = row.get("idl_source");
    assert_eq!(name, "test_program");
    assert_eq!(status, "schema_created");
    assert_eq!(source, "manual");

    // Verify indexer_state row
    let row = sqlx::query(r#"SELECT "status", "total_instructions", "total_accounts" FROM "indexer_state" WHERE "program_id" = $1"#)
        .bind(program_id)
        .fetch_one(&pool)
        .await
        .expect("indexer_state row should exist");

    let state_status: String = row.get("status");
    let total_ix: i64 = row.get("total_instructions");
    let total_acct: i64 = row.get("total_accounts");
    assert_eq!(state_status, "initializing");
    assert_eq!(total_ix, 0);
    assert_eq!(total_acct, 0);

    cleanup(&pool, program_id).await;
}

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn mark_program_error_transitions_status_and_records_message() {
    // Story 4.4 Task 6 (refined P15): when registry IDL cache seeding fails
    // for a program loaded from persistence, we drop it from the auto-start
    // list AND flip `programs.status = 'error'` so the API stays consistent.
    // This test exercises the helper that performs that transition.

    let pool = setup_pool().await;
    let program_id = "Testd1111111111111111111111111111111111111111";

    cleanup(&pool, program_id).await;

    // Seed a successful registration so the row + indexer_state row exist.
    let idl_manager = IdlManager::new("http://localhost:8899".to_string());
    let mut registry = ProgramRegistry::new(idl_manager);
    let data = registry
        .prepare_registration(program_id.to_string(), Some(sample_idl_json()))
        .expect("prepare should succeed");
    ProgramRegistry::commit_registration(pool.clone(), data)
        .await
        .expect("commit should succeed");

    // Pre-condition: status should not be 'error' yet.
    let pre_status: String =
        sqlx::query_scalar(r#"SELECT "status" FROM "programs" WHERE "program_id" = $1"#)
            .bind(program_id)
            .fetch_one(&pool)
            .await
            .expect("programs row should exist");
    assert_ne!(pre_status, "error");

    // Apply the helper.
    ProgramRegistry::mark_program_error(
        pool.clone(),
        program_id.to_string(),
        "registry IDL cache seeding failed at startup".to_string(),
    )
    .await
    .expect("mark_program_error should succeed");

    // Post-condition: programs.status flipped to 'error'.
    let status: String =
        sqlx::query_scalar(r#"SELECT "status" FROM "programs" WHERE "program_id" = $1"#)
            .bind(program_id)
            .fetch_one(&pool)
            .await
            .expect("programs row should still exist");
    assert_eq!(status, "error");

    // Post-condition: indexer_state.error_message captured the failure
    // reason and status flipped to 'error' too.
    let row = sqlx::query(
        r#"SELECT "status", "error_message" FROM "indexer_state" WHERE "program_id" = $1"#,
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .expect("indexer_state row should exist");
    let state_status: String = row.get("status");
    let error_message: Option<String> = row.get("error_message");
    assert_eq!(state_status, "error");
    assert_eq!(
        error_message.as_deref(),
        Some("registry IDL cache seeding failed at startup")
    );

    cleanup(&pool, program_id).await;
}

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn idl_json_persisted_bytes_are_byte_exact() {
    // Story 4.4 AC5: read back the persisted bytes from `programs.idl_json`
    // after a successful registration and confirm they are byte-identical to
    // the bytes the operator uploaded — not a re-serialization that drops
    // unmodeled fields or canonicalizes key order.
    let pool = setup_pool().await;
    let program_id = "Teste1111111111111111111111111111111111111111";

    cleanup(&pool, program_id).await;

    // Deliberately unusual whitespace + key order so a silent re-serialize
    // would produce different bytes.
    let raw_json = "{\n  \"address\": \"11111111111111111111111111111111\",\n  \"metadata\": {\n    \"version\": \"0.1.0\",\n    \"name\":    \"test_program\",\n    \"spec\":   \"0.1.0\"\n  },\n  \"instructions\": [],\n  \"accounts\": [],\n  \"types\": []\n}";

    let idl_manager = IdlManager::new("http://localhost:8899".to_string());
    let mut registry = ProgramRegistry::new(idl_manager);
    let data = registry
        .prepare_registration(program_id.to_string(), Some(raw_json.to_string()))
        .expect("prepare should succeed");
    let original_hash = data.idl_hash.clone();

    ProgramRegistry::commit_registration(pool.clone(), data)
        .await
        .expect("commit should succeed");

    // Read back the persisted bytes.
    let persisted: String =
        sqlx::query_scalar(r#"SELECT "idl_json" FROM "programs" WHERE "program_id" = $1"#)
            .bind(program_id)
            .fetch_one(&pool)
            .await
            .expect("programs row should exist");

    assert_eq!(
        persisted, raw_json,
        "persisted idl_json must be byte-exact to the original upload"
    );
    assert_eq!(
        solarix::idl::compute_idl_hash(&persisted),
        original_hash,
        "compute_idl_hash(persisted) must match the registration-time idl_hash"
    );

    cleanup(&pool, program_id).await;
}

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn register_duplicate_program_returns_error() {
    let pool = setup_pool().await;
    // Valid base58 Solana pubkey for test isolation
    let program_id = "Testb1111111111111111111111111111111111111111";

    cleanup(&pool, program_id).await;

    let idl_manager = IdlManager::new("http://localhost:8899".to_string());
    let mut registry = ProgramRegistry::new(idl_manager);

    // First registration succeeds
    let idl_json = sample_idl_json();
    let data = registry
        .prepare_registration(program_id.to_string(), Some(idl_json.clone()))
        .expect("first prepare should succeed");
    ProgramRegistry::commit_registration(pool.clone(), data)
        .await
        .expect("first commit should succeed");

    // Second registration — prepare succeeds (cache hit), commit fails
    let data2 = registry
        .prepare_registration(program_id.to_string(), Some(idl_json.clone()))
        .expect("second prepare should succeed");
    let err = ProgramRegistry::commit_registration(pool.clone(), data2)
        .await
        .expect_err("second commit should fail with AlreadyRegistered");

    assert!(
        matches!(&err, RegistrationError::AlreadyRegistered(id) if id == program_id),
        "expected AlreadyRegistered({program_id}), got: {err}"
    );

    cleanup(&pool, program_id).await;
}
