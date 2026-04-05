pub mod queries;
pub mod schema;
pub mod writer;

// std library
use std::time::Duration;

// external crates
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::info;

// internal crate
use crate::config::Config;

/// Errors that can occur during storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("DDL execution failed: {0}")]
    DdlFailed(String),

    #[error("write failed: {0}")]
    WriteFailed(String),

    #[error("checkpoint failed: {0}")]
    CheckpointFailed(String),
}

/// Initialize a database connection pool.
pub async fn init_pool(config: &Config) -> Result<PgPool, StorageError> {
    let pool = PgPoolOptions::new()
        .min_connections(config.db_pool_min)
        .max_connections(config.db_pool_max)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .connect(&config.database_url)
        .await
        .map_err(|e| {
            StorageError::ConnectionFailed(format!("failed to connect to database: {e}"))
        })?;

    info!(
        min_connections = config.db_pool_min,
        max_connections = config.db_pool_max,
        "database connection pool created"
    );

    Ok(pool)
}

/// Create system tables (programs, indexer_state) if they don't already exist.
pub async fn bootstrap_system_tables(pool: &PgPool) -> Result<(), StorageError> {
    let ddl = r#"
        CREATE TABLE IF NOT EXISTS "programs" (
            "program_id"   VARCHAR(44) PRIMARY KEY,
            "program_name" TEXT NOT NULL,
            "schema_name"  TEXT NOT NULL UNIQUE,
            "idl_hash"     VARCHAR(64),
            "idl_source"   TEXT,
            "status"       TEXT NOT NULL DEFAULT 'initializing',
            "created_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            "updated_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS "indexer_state" (
            "program_id"          VARCHAR(44) PRIMARY KEY REFERENCES "programs"("program_id"),
            "status"              TEXT NOT NULL,
            "last_processed_slot" BIGINT,
            "last_heartbeat"      TIMESTAMPTZ,
            "error_message"       TEXT,
            "total_instructions"  BIGINT DEFAULT 0,
            "total_accounts"      BIGINT DEFAULT 0
        );
    "#;

    sqlx::raw_sql(ddl)
        .execute(pool)
        .await
        .map_err(|e| StorageError::DdlFailed(format!("system table bootstrap failed: {e}")))?;

    info!("system tables bootstrapped");

    Ok(())
}
