// std library
use std::future::Future;
use std::pin::Pin;

// external crates
use anchor_lang_idl_spec::Idl;
use sqlx::PgPool;
use tracing::{error, info, warn};

// internal crate
use crate::idl::{IdlError, IdlManager};
use crate::storage::schema::{derive_schema_name, generate_schema, seed_metadata};
use crate::storage::StorageError;

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

    #[error("schema generation failed: {0}")]
    SchemaFailed(#[from] StorageError),
}

/// Owned data extracted during the prepare phase of registration.
///
/// Holding this struct lets the caller drop the `RwLock` write guard on
/// `ProgramRegistry` before the expensive async commit phase, keeping the lock
/// window minimal and the resulting future `Send`.
#[derive(Debug, Clone)]
pub struct RegistrationData {
    pub program_id: String,
    pub program_name: String,
    pub schema_name: String,
    pub idl_hash: String,
    pub idl_source: String,
    pub idl: Idl,
    /// Whether the IDL was already in the cache before `prepare_registration`.
    /// Used by `rollback_cache` to decide whether to remove the entry on failure.
    pub was_cached: bool,
}

/// Registry of indexed programs, wrapping IdlManager for IDL cache access.
pub struct ProgramRegistry {
    pub idl_manager: IdlManager,
}

impl ProgramRegistry {
    pub fn new(idl_manager: IdlManager) -> Self {
        Self { idl_manager }
    }

    /// Phase 1: Resolve the IDL and prepare all data needed for registration.
    ///
    /// This is a **sync** method so the caller never holds the `RwLock` write
    /// guard across an `.await` point (which would make the handler future
    /// `!Send`).
    ///
    /// If `idl_json` is `Some`, does a manual upload into the cache.
    /// If `idl_json` is `None`, the IDL must already be cached (via a prior
    /// call to `IdlManager::get_idl()` outside the lock).
    ///
    /// Returns owned `RegistrationData` so the caller can drop the lock
    /// immediately and pass it to `commit_registration`.
    pub fn prepare_registration(
        &mut self,
        program_id: String,
        idl_json: Option<String>,
    ) -> Result<RegistrationData, RegistrationError> {
        let was_cached = self.idl_manager.get_cached(&program_id).is_some();

        // Manual upload: parse + cache (sync). Skip if already cached —
        // the DB duplicate check in commit_registration handles re-registration.
        if let Some(ref json) = idl_json {
            if !was_cached {
                self.idl_manager.upload_idl(&program_id, json)?;
            }
        } else if !was_cached {
            // Caller must pre-fetch the IDL before acquiring the write lock.
            // This should not happen if the handler logic is correct.
            return Err(RegistrationError::Idl(IdlError::NotFound(
                program_id.clone(),
            )));
        }

        let cached = self
            .idl_manager
            .get_cached_entry(&program_id)
            .ok_or_else(|| RegistrationError::Idl(IdlError::NotFound(program_id.clone())))?;

        let idl_hash = cached.hash.clone();
        let idl_source = cached.source.as_str().to_string();
        let program_name = cached.idl.metadata.name.clone();
        let schema_name = derive_schema_name(&program_name, &program_id);
        let idl = cached.idl.clone();

        Ok(RegistrationData {
            program_id,
            program_name,
            schema_name,
            idl_hash,
            idl_source,
            idl,
            was_cached,
        })
    }

    /// Phase 2: Commit registration to the database and generate the schema.
    ///
    /// Static method — does not require `&self` so the caller can drop the
    /// write-lock before calling. All parameters are owned so the returned
    /// future is `'static` + `Send`. Borrowed parameters create futures with
    /// specific lifetimes that fail Rust's async Send inference in composed
    /// state machines (compiler limitation, see rust#96865).
    pub async fn commit_registration(
        pool: PgPool,
        data: RegistrationData,
    ) -> Result<ProgramInfo, RegistrationError> {
        let RegistrationData {
            program_id,
            program_name,
            schema_name,
            idl_hash,
            idl_source,
            idl,
            was_cached: _,
        } = data;

        Self::write_registration(
            pool.clone(),
            program_id.clone(),
            program_name.clone(),
            schema_name.clone(),
            idl_hash.clone(),
            idl_source.clone(),
        )
        .await?;

        let idl_for_schema = idl.clone();
        let status = match generate_schema(
            pool.clone(),
            idl_for_schema,
            program_id.clone(),
            schema_name.clone(),
        )
        .await
        {
            Ok(()) => {
                // Consume idl by move — last usage
                if let Err(e) = seed_metadata(
                    pool.clone(),
                    idl,
                    program_id.clone(),
                    idl_hash.clone(),
                    schema_name.clone(),
                )
                .await
                {
                    warn!(
                        program_id = %program_id,
                        schema_name = %schema_name,
                        error = %e,
                        "metadata seeding failed (schema was created successfully)"
                    );
                }

                match Self::update_program_status(
                    pool.clone(),
                    program_id.clone(),
                    "schema_created".to_string(),
                )
                .await
                {
                    Ok(()) => "schema_created".to_string(),
                    Err(e) => {
                        warn!(
                            program_id = %program_id,
                            error = %e,
                            "failed to update status to schema_created, attempting error status"
                        );
                        if let Err(e2) = Self::update_program_status(
                            pool.clone(),
                            program_id.clone(),
                            "error".to_string(),
                        )
                        .await
                        {
                            error!(
                                program_id = %program_id,
                                error = %e2,
                                "failed to update program status to 'error'"
                            );
                        }
                        return Err(RegistrationError::DatabaseError(e.to_string()));
                    }
                }
            }
            Err(e) => {
                warn!(
                    program_id = %program_id,
                    schema_name = %schema_name,
                    error = %e,
                    "schema generation failed"
                );

                if let Err(update_err) = Self::update_program_status(
                    pool.clone(),
                    program_id.clone(),
                    "error".to_string(),
                )
                .await
                {
                    error!(
                        program_id = %program_id,
                        error = %update_err,
                        "failed to update program status to 'error'"
                    );
                }

                return Err(RegistrationError::SchemaFailed(e));
            }
        };

        info!(
            program_id = %program_id,
            program_name = %program_name,
            schema_name = %schema_name,
            idl_source = %idl_source,
            status = %status,
            "program registered with schema"
        );

        Ok(ProgramInfo {
            program_id,
            program_name,
            schema_name,
            idl_hash,
            idl_source,
            status,
        })
    }

    /// Update a program's status in the DB.
    fn update_program_status(
        pool: PgPool,
        program_id: String,
        status: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), RegistrationError>> + Send>> {
        Box::pin(async move {
            sqlx::query(
                r#"UPDATE "programs" SET "status" = $1, "updated_at" = NOW()
                   WHERE "program_id" = $2"#,
            )
            .bind(&status)
            .bind(&program_id)
            .execute(&pool)
            .await
            .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;
            Ok(())
        })
    }

    /// Roll back IDL cache entry added during a failed registration.
    pub fn rollback_cache(&mut self, program_id: &str) {
        self.idl_manager.remove_cached(program_id);
    }

    /// Execute registration DB writes in a single transaction.
    ///
    /// Returns a boxed `Send` future to hide the `Executor` lifetime from
    /// `tx.as_mut()`. Without boxing, the specific `&'1 mut PgConnection`
    /// lifetime propagates through the opaque return type, causing the
    /// "Executor not general enough" error in composed async state machines.
    fn write_registration(
        pool: PgPool,
        program_id: String,
        program_name: String,
        schema_name: String,
        idl_hash: String,
        idl_source: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), RegistrationError>> + Send>> {
        Box::pin(async move {
            let mut tx = pool
                .begin()
                .await
                .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            let result = sqlx::query(
                r#"INSERT INTO "programs" ("program_id", "program_name", "schema_name", "idl_hash", "idl_source", "status")
                   VALUES ($1, $2, $3, $4, $5, 'registered')
                   ON CONFLICT ("program_id") DO NOTHING"#,
            )
            .bind(&program_id)
            .bind(&program_name)
            .bind(&schema_name)
            .bind(&idl_hash)
            .bind(&idl_source)
            .execute(tx.as_mut())
            .await
            .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            if result.rows_affected() == 0 {
                return Err(RegistrationError::AlreadyRegistered(program_id));
            }

            sqlx::query(
                r#"INSERT INTO "indexer_state" ("program_id", "status", "total_instructions", "total_accounts")
                   VALUES ($1, 'initializing', 0, 0)"#,
            )
            .bind(&program_id)
            .execute(tx.as_mut())
            .await
            .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            Ok(())
        })
    }

    /// Remove a program from the in-memory IDL cache.
    pub fn remove_program(&mut self, program_id: &str) {
        self.idl_manager.remove_cached(program_id);
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
