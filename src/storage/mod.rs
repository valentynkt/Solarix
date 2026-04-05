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

/// Strip password from a database URL for safe logging.
///
/// Replaces `://user:password@` with `://user:***@`.
fn sanitize_database_url(url: &str) -> String {
    // Pattern: scheme://user:password@host...
    // Find the credentials section between :// and @
    let Some(scheme_end) = url.find("://") else {
        return "<invalid-url>".to_string();
    };
    let after_scheme = scheme_end + 3;
    let Some(at_pos) = url[after_scheme..].find('@') else {
        // No credentials in URL
        return url.to_string();
    };
    let credentials = &url[after_scheme..after_scheme + at_pos];
    if let Some(colon) = credentials.find(':') {
        let user = &credentials[..colon];
        format!(
            "{}://{}:***@{}",
            &url[..scheme_end],
            user,
            &url[after_scheme + at_pos + 1..]
        )
    } else {
        // No password in credentials
        url.to_string()
    }
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
            let sanitized = sanitize_database_url(&config.database_url);
            StorageError::ConnectionFailed(format!(
                "failed to connect to database ({sanitized}): {e}"
            ))
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
            "total_instructions"  BIGINT NOT NULL DEFAULT 0,
            "total_accounts"      BIGINT NOT NULL DEFAULT 0
        );
    "#;

    sqlx::raw_sql(ddl)
        .execute(pool)
        .await
        .map_err(|e| StorageError::DdlFailed(format!("system table bootstrap failed: {e}")))?;

    info!("system tables bootstrapped");

    Ok(())
}
