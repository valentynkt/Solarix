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
