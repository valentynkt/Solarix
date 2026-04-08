//! Program registration lifecycle: two-phase state machine from `Pending` to `Active`.

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
    /// Raw IDL JSON bytes as fetched on-chain or uploaded by the operator.
    /// Persisted verbatim into `programs.idl_json` so that
    /// `compute_idl_hash(idl_json) == idl_hash` holds across the round trip.
    /// Story 4.4 AC5.
    pub idl_json: String,
    /// Whether the IDL was already in the cache before `prepare_registration`.
    /// Used by `rollback_cache` to decide whether to remove the entry on failure.
    pub was_cached: bool,
}

/// Registry of indexed programs, wrapping IdlManager for IDL cache access.
pub struct ProgramRegistry {
    pub idl_manager: IdlManager,
}

impl ProgramRegistry {
    /// Create a new `ProgramRegistry` wrapping the given `IdlManager`.
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
        // Raw bytes are what `idl_hash` was computed from. Persisting these
        // (instead of `serde_json::to_string(&idl)`) keeps the
        // `compute_idl_hash(persisted) == idl_hash` invariant. Story 4.4 AC5.
        let idl_json = cached.raw_json.clone();

        Ok(RegistrationData {
            program_id,
            program_name,
            schema_name,
            idl_hash,
            idl_source,
            idl,
            idl_json,
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
    #[tracing::instrument(
        name = "registry.commit_registration",
        skip(pool, data),
        fields(
            program_id = data.program_id.as_str(),
            schema_name = data.schema_name.as_str(),
        ),
        level = "info",
        err(Display)
    )]
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
            idl_json,
            was_cached: _,
        } = data;

        // Persist the **raw bytes** that idl_hash was computed from, NOT a
        // re-serialization of the parsed `Idl` struct. This is what keeps the
        // round trip `compute_idl_hash(read_back_bytes) == idl_hash` exact —
        // re-serializing through `serde_json::to_string(&idl)` would silently
        // drop fields not modeled by `anchor_lang_idl_spec::Idl`. Story 4.4 AC5.
        Self::write_registration(
            pool.clone(),
            program_id.clone(),
            program_name.clone(),
            schema_name.clone(),
            idl_hash.clone(),
            idl_source.clone(),
            idl_json,
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

    /// Mark a program as `status = 'error'` and stash a human-readable
    /// failure message in `indexer_state.error_message`. Used by the startup
    /// auto-start path when registry IDL cache seeding fails for a program
    /// that was successfully registered earlier — keeps the API consistent
    /// (operators see status=error instead of a 200 from `/api/programs/{id}`
    /// alongside 404s from the instructions handler). Story 4.4 Task 6
    /// (refined P15).
    ///
    /// Returns a boxed `Send` future for the same reason as
    /// `update_program_status` — the in-flight transaction holds an
    /// `Executor`-bound reference whose lifetime would otherwise propagate
    /// through the opaque return type and break Send inference at the
    /// caller's `await`.
    pub fn mark_program_error(
        pool: PgPool,
        program_id: String,
        error_message: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), RegistrationError>> + Send>> {
        Box::pin(async move {
            let mut tx = pool
                .begin()
                .await
                .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            sqlx::query(
                r#"UPDATE "programs" SET "status" = 'error', "updated_at" = NOW()
                   WHERE "program_id" = $1"#,
            )
            .bind(&program_id)
            .execute(tx.as_mut())
            .await
            .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            // The indexer_state row may not exist yet for some upgrade paths;
            // an UPDATE that affects 0 rows is not an error here, it just
            // means the operator will only see the failure on the programs
            // row. We deliberately do NOT INSERT a fallback row — that would
            // race with the pipeline's own initializer.
            sqlx::query(
                r#"UPDATE "indexer_state"
                   SET "status" = 'error', "error_message" = $2
                   WHERE "program_id" = $1"#,
            )
            .bind(&program_id)
            .bind(&error_message)
            .execute(tx.as_mut())
            .await
            .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            tx.commit()
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
        idl_json: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), RegistrationError>> + Send>> {
        Box::pin(async move {
            let mut tx = pool
                .begin()
                .await
                .map_err(|e| RegistrationError::DatabaseError(e.to_string()))?;

            let result = sqlx::query(
                r#"INSERT INTO "programs" ("program_id", "program_name", "schema_name", "idl_hash", "idl_source", "idl_json", "status")
                   VALUES ($1, $2, $3, $4, $5, $6, 'registered')
                   ON CONFLICT ("program_id") DO NOTHING"#,
            )
            .bind(&program_id)
            .bind(&program_name)
            .bind(&schema_name)
            .bind(&idl_hash)
            .bind(&idl_source)
            .bind(&idl_json)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::compute_idl_hash;

    // -----------------------------------------------------------------------
    // Send-safety compile-time checks (Story 6.4 AC9)
    //
    // See `src/idl/mod.rs` test module doc comment for rationale and
    // verification procedure. Short version: the `fn _check` + `let _: fn = _check;`
    // pattern forces monomorphization so the Send bound is checked even when
    // `cargo check --lib` would otherwise skip the test body.
    //
    // This module specifically pins `ProgramRegistry::commit_registration`
    // because it was the exact root-cause function in the Sprint-3 !Send
    // blocker (`_bmad-output/problem-solution-2026-04-06.md`). The
    // commit_registration future must be `'static + Send` so it can be
    // awaited from inside an axum handler spawned on a multi-thread runtime.
    // -----------------------------------------------------------------------

    #[test]
    fn test_commit_registration_future_is_send() {
        fn _check(pool: PgPool, data: RegistrationData) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = ProgramRegistry::commit_registration(pool, data);
            _require_send(&fut);
        }
        let _: fn(PgPool, RegistrationData) = _check;
    }

    #[test]
    fn test_mark_program_error_future_is_send() {
        fn _check(pool: PgPool, program_id: String, error_message: String) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = ProgramRegistry::mark_program_error(pool, program_id, error_message);
            _require_send(&fut);
        }
        let _: fn(PgPool, String, String) = _check;
    }

    // -----------------------------------------------------------------------
    // Story 4.4 AC5 — hash stability: persisted bytes match `idl_hash`.
    //
    // The contract: the bytes carried into `RegistrationData.idl_json`
    // (which `commit_registration` writes verbatim into `programs.idl_json`)
    // must hash to the same value as `RegistrationData.idl_hash`.
    //
    // We use deliberately unusual whitespace and key ordering in the input
    // JSON to prove that we're carrying the *original* bytes through, not a
    // re-serialization of the parsed `Idl` struct (which would silently
    // canonicalize and drop unmodeled fields).
    // -----------------------------------------------------------------------
    #[test]
    fn registration_data_idl_json_hashes_to_idl_hash() {
        let raw_json = "{\n  \"address\": \"11111111111111111111111111111111\",\n  \"metadata\": {\n    \"version\": \"0.1.0\",\n    \"name\":    \"test_program\",\n    \"spec\":   \"0.1.0\"\n  },\n  \"instructions\": [],\n  \"accounts\": [],\n  \"types\": []\n}";

        let idl_manager = IdlManager::new("http://localhost:8899".to_string());
        let mut registry = ProgramRegistry::new(idl_manager);

        let data = registry
            .prepare_registration(
                "Testc11111111111111111111111111111111111111".to_string(),
                Some(raw_json.to_string()),
            )
            .expect("prepare_registration should succeed");

        // The raw bytes are preserved verbatim in `RegistrationData.idl_json`.
        assert_eq!(data.idl_json, raw_json, "idl_json must be byte-exact");

        // And those bytes hash to the value stored as idl_hash. This is the
        // round-trip invariant the story exists to protect.
        assert_eq!(
            compute_idl_hash(&data.idl_json),
            data.idl_hash,
            "compute_idl_hash(persisted bytes) must equal idl_hash"
        );

        // Sanity check: re-serializing the parsed Idl produces *different*
        // bytes (whitespace canonicalization, key reordering), so the test
        // would fail if `commit_registration` silently re-serialized
        // through `serde_json::to_string(&idl)`.
        let reserialized = serde_json::to_string(&data.idl).expect("re-serialize Idl");
        assert_ne!(
            reserialized, raw_json,
            "re-serialized bytes should differ from raw — otherwise the test isn't actually proving byte preservation"
        );
    }
}
