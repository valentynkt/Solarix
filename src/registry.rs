// external crates
use anchor_lang_idl_spec::Idl;
use sqlx::PgPool;
use tracing::info;

// internal crate
use crate::idl::{IdlError, IdlManager, IdlSource};
use crate::storage::schema::derive_schema_name;

/// Information about a registered program.
#[derive(Debug, Clone)]
pub struct ProgramInfo {
    pub program_id: String,
    pub program_name: String,
    pub schema_name: String,
    pub idl_hash: String,
    pub idl_source: String,
    pub status: String,
}

/// Errors that can occur during program registration.
#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("IDL error: {0}")]
    Idl(#[from] IdlError),

    #[error("program {0} is already registered")]
    AlreadyRegistered(String),

    #[error("database error: {0}")]
    DatabaseError(String),
}

/// Registry of indexed programs, wrapping IdlManager and DB access.
pub struct ProgramRegistry {
    pub idl_manager: IdlManager,
    pool: PgPool,
}

impl ProgramRegistry {
    pub fn new(idl_manager: IdlManager, pool: PgPool) -> Self {
        Self { idl_manager, pool }
    }

    /// Register a program for indexing.
    ///
    /// If `idl_json` is `Some`, uses manual upload. Otherwise fetches via cascade.
    /// Checks for duplicates, derives schema name, writes to `programs` and `indexer_state`.
    pub async fn register_program(
        &mut self,
        program_id: &str,
        idl_json: Option<&str>,
    ) -> Result<ProgramInfo, RegistrationError> {
        // Check for duplicate
        let exists: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "programs" WHERE "program_id" = $1)"#,
        )
        .bind(program_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

        if exists {
            return Err(RegistrationError::AlreadyRegistered(program_id.to_string()));
        }

        // Get IDL (manual upload or fetch cascade)
        let source = if idl_json.is_some() {
            IdlSource::Manual
        } else {
            IdlSource::OnChain // will be determined by get_idl cascade
        };

        if let Some(json) = idl_json {
            self.idl_manager.upload_idl(program_id, json)?;
        } else {
            self.idl_manager.get_idl(program_id).await?;
        }

        // Get cached entry for hash and actual source
        let cached = self
            .idl_manager
            .get_cached_entry(program_id)
            .ok_or_else(|| RegistrationError::Idl(IdlError::NotFound(program_id.to_string())))?;

        let idl_hash = cached.hash.clone();
        let actual_source = cached.source.as_str().to_string();
        let program_name = cached.idl.metadata.name.clone();
        let _ = source; // actual source comes from cached entry

        let schema_name = derive_schema_name(&program_name, program_id);

        // Insert into programs table
        sqlx::query(
            r#"INSERT INTO "programs" ("program_id", "program_name", "schema_name", "idl_hash", "idl_source", "status")
               VALUES ($1, $2, $3, $4, $5, 'registered')"#,
        )
        .bind(program_id)
        .bind(&program_name)
        .bind(&schema_name)
        .bind(&idl_hash)
        .bind(&actual_source)
        .execute(&self.pool)
        .await
        .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

        // Insert into indexer_state table
        sqlx::query(
            r#"INSERT INTO "indexer_state" ("program_id", "status", "total_instructions", "total_accounts")
               VALUES ($1, 'initializing', 0, 0)"#,
        )
        .bind(program_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

        info!(
            program_id,
            program_name = %program_name,
            schema_name = %schema_name,
            idl_source = %actual_source,
            "program registered"
        );

        Ok(ProgramInfo {
            program_id: program_id.to_string(),
            program_name,
            schema_name,
            idl_hash,
            idl_source: actual_source,
            status: "registered".to_string(),
        })
    }

    /// Get a cached IDL for a program.
    pub fn get_idl(&self, program_id: &str) -> Option<&Idl> {
        self.idl_manager.get_cached(program_id)
    }

    /// List all cached program IDs.
    pub fn list_programs(&self) -> Vec<&str> {
        self.idl_manager.cached_program_ids()
    }
}
