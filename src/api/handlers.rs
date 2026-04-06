use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::time::timeout;
use tracing::info;

use crate::idl::IdlManager;
use crate::registry::ProgramRegistry;
use crate::storage::schema::quote_ident;

use super::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Request / query types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterProgramRequest {
    pub program_id: String,
    pub idl: Option<Value>,
}

#[derive(Deserialize)]
pub struct DeleteProgramQuery {
    #[serde(default)]
    pub drop_tables: bool,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_program_id(program_id: &str) -> Result<(), ApiError> {
    program_id
        .parse::<solana_pubkey::Pubkey>()
        .map_err(|_| ApiError::InvalidRequest(format!("invalid program_id: '{program_id}'")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let db_ok = timeout(
        Duration::from_secs(2),
        sqlx::query("SELECT 1").fetch_one(&state.pool),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);

    let uptime = state.start_time.elapsed().as_secs();
    let version = env!("CARGO_PKG_VERSION");

    let status = if db_ok { "healthy" } else { "unhealthy" };
    let db_status = if db_ok { "connected" } else { "disconnected" };
    let http_status = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        http_status,
        Json(json!({
            "status": status,
            "database": db_status,
            "uptime_seconds": uptime,
            "version": version,
        })),
    )
}

pub async fn register_program(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterProgramRequest>,
) -> Result<Response, ApiError> {
    let registry = Arc::clone(&state.registry);
    let pool = state.pool.clone();
    tokio::spawn(do_register(registry, pool, body))
        .await
        .map_err(|e| ApiError::StorageError(format!("task failed: {e}")))?
}

async fn do_register(
    registry: Arc<tokio::sync::RwLock<ProgramRegistry>>,
    pool: sqlx::PgPool,
    body: RegisterProgramRequest,
) -> Result<Response, ApiError> {
    validate_program_id(&body.program_id)?;

    let idl_json = body
        .idl
        .map(|v| serde_json::to_string(&v))
        .transpose()
        .map_err(|e| ApiError::InvalidRequest(format!("invalid IDL JSON: {e}")))?;

    // Auto-fetch: if no IDL provided, fetch via on-chain -> bundled cascade
    if idl_json.is_none() {
        auto_fetch_idl(Arc::clone(&registry), body.program_id.clone()).await?;
    }

    let data = prepare_registration(Arc::clone(&registry), body.program_id, idl_json).await?;

    let was_cached = data.was_cached;
    let program_id_for_rollback = data.program_id.clone();
    let result = ProgramRegistry::commit_registration(pool, data).await;

    if result.is_err() && !was_cached {
        rollback_cache(registry, program_id_for_rollback).await;
    }

    let program_info = result?;

    info!(
        program_id = %program_info.program_id,
        idl_source = %program_info.idl_source,
        "program registered via API"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "data": {
                "program_id": program_info.program_id,
                "status": "registered",
                "idl_source": program_info.idl_source,
            },
            "meta": {
                "message": "Program registered. Indexing will begin shortly."
            }
        })),
    )
        .into_response())
}

/// Acquire write lock, run prepare_registration, drop lock.
///
/// Returns a boxed `Send` future to hide the specific-lifetime `&RwLock`
/// reference from `registry.write()`. Without boxing, this lifetime
/// propagates through the opaque `impl Future` return type, causing the
/// caller's composed state machine to fail Send inference.
fn prepare_registration(
    registry: Arc<tokio::sync::RwLock<ProgramRegistry>>,
    program_id: String,
    idl_json: Option<String>,
) -> Pin<Box<dyn Future<Output = Result<crate::registry::RegistrationData, ApiError>> + Send>> {
    Box::pin(async move {
        let mut guard = registry.write().await;
        let result = guard.prepare_registration(program_id, idl_json);
        drop(guard);
        Ok(result?)
    })
}

/// Roll back IDL cache on failed registration.
fn rollback_cache(
    registry: Arc<tokio::sync::RwLock<ProgramRegistry>>,
    program_id: String,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        registry.write().await.rollback_cache(&program_id);
    })
}

/// Fetch IDL via cascade (on-chain -> bundled) for auto-fetch registration.
///
/// Acquires read lock to check cache and get fetch params, drops it,
/// performs async fetch (no lock held), acquires write lock to cache
/// the result, drops it.
fn auto_fetch_idl(
    registry: Arc<tokio::sync::RwLock<ProgramRegistry>>,
    program_id: String,
) -> Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send>> {
    Box::pin(async move {
        // Check cache + get fetch params under one read lock
        let params = {
            let guard = registry.read().await;
            if guard.idl_manager.get_cached(&program_id).is_some() {
                return Ok(());
            }
            guard.idl_manager.fetch_params()
        };

        // Fetch IDL (async, no lock held — Send-safe)
        let (idl_json, source) = IdlManager::fetch_idl_standalone(&params, &program_id)
            .await
            .map_err(|e| ApiError::IdlError(e.to_string()))?;

        // Cache under write lock
        {
            let mut guard = registry.write().await;
            guard
                .idl_manager
                .insert_fetched_idl(&program_id, &idl_json, source)
                .map_err(|e| ApiError::IdlError(e.to_string()))?;
        }

        Ok(())
    })
}

pub async fn list_programs(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(
        r#"SELECT "program_id", "program_name", "status", "created_at"
           FROM "programs" ORDER BY "created_at" DESC"#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::QueryFailed(e.to_string()))?;

    let programs: Vec<Value> = rows
        .iter()
        .map(|row| {
            let program_id: String = row.get("program_id");
            let program_name: String = row.get("program_name");
            let status: String = row.get("status");
            let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
            json!({
                "program_id": program_id,
                "program_name": program_name,
                "status": status,
                "created_at": created_at.to_rfc3339(),
            })
        })
        .collect();

    let total = programs.len();
    Ok(Json(json!({
        "data": programs,
        "meta": { "total": total }
    })))
}

pub async fn get_program(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    let row = sqlx::query(
        r#"SELECT p."program_id", p."program_name", p."schema_name",
                p."idl_source", p."idl_hash", p."status",
                p."created_at", p."updated_at",
                i."total_instructions", i."total_accounts", i."last_processed_slot"
           FROM "programs" p
           LEFT JOIN "indexer_state" i ON p."program_id" = i."program_id"
           WHERE p."program_id" = $1"#,
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::QueryFailed(e.to_string()))?
    .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;

    let program_id: String = row.get("program_id");
    let program_name: String = row.get("program_name");
    let schema_name: String = row.get("schema_name");
    let idl_source: String = row.get("idl_source");
    let idl_hash: String = row.get("idl_hash");
    let status: String = row.get("status");
    let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
    let updated_at: Option<chrono::DateTime<chrono::Utc>> = row.get("updated_at");
    let total_instructions: Option<i64> = row.get("total_instructions");
    let total_accounts: Option<i64> = row.get("total_accounts");
    let last_processed_slot: Option<i64> = row.get("last_processed_slot");

    Ok(Json(json!({
        "data": {
            "program_id": program_id,
            "program_name": program_name,
            "schema_name": schema_name,
            "idl_source": idl_source,
            "idl_hash": idl_hash,
            "status": status,
            "created_at": created_at.to_rfc3339(),
            "updated_at": updated_at.map(|t| t.to_rfc3339()),
            "total_instructions": total_instructions.unwrap_or(0),
            "total_accounts": total_accounts.unwrap_or(0),
            "last_processed_slot": last_processed_slot,
        }
    })))
}

pub async fn delete_program(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DeleteProgramQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    if query.drop_tables {
        // Hard delete: drop schema, remove from DB and cache
        let schema_name: String =
            sqlx::query_scalar(r#"SELECT "schema_name" FROM "programs" WHERE "program_id" = $1"#)
                .bind(&id)
                .fetch_optional(&state.pool)
                .await
                .map_err(|e| ApiError::QueryFailed(e.to_string()))?
                .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;

        hard_delete(state.pool.clone(), schema_name, id.clone()).await?;

        let mut registry = state.registry.write().await;
        registry.remove_program(&id);

        info!(program_id = %id, "program hard-deleted (schema dropped)");

        Ok(Json(json!({
            "data": {
                "program_id": id,
                "action": "deleted",
                "drop_tables": true,
            }
        })))
    } else {
        // Soft delete: set status to stopped
        let result = sqlx::query(
            r#"UPDATE "programs" SET "status" = 'stopped', "updated_at" = NOW()
               WHERE "program_id" = $1"#,
        )
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::StorageError(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(ApiError::ProgramNotFound(id));
        }

        info!(program_id = %id, "program soft-deleted (stopped)");

        Ok(Json(json!({
            "data": {
                "program_id": id,
                "action": "stopped",
                "drop_tables": false,
            }
        })))
    }
}

/// Execute hard delete: DROP SCHEMA + DELETE rows in a transactional DML block.
///
/// DDL runs on pool directly (raw_sql + tx.as_mut() triggers !Send).
/// DML (DELETEs) runs in an explicit transaction for atomicity.
fn hard_delete(
    pool: sqlx::PgPool,
    schema_name: String,
    program_id: String,
) -> Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send>> {
    Box::pin(async move {
        let drop_ddl = format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            quote_ident(&schema_name)
        );
        sqlx::raw_sql(&drop_ddl)
            .execute(&pool)
            .await
            .map_err(|e| ApiError::StorageError(e.to_string()))?;

        let mut tx = pool
            .begin()
            .await
            .map_err(|e| ApiError::StorageError(e.to_string()))?;

        sqlx::query(r#"DELETE FROM "indexer_state" WHERE "program_id" = $1"#)
            .bind(&program_id)
            .execute(tx.as_mut())
            .await
            .map_err(|e| ApiError::StorageError(e.to_string()))?;

        sqlx::query(r#"DELETE FROM "programs" WHERE "program_id" = $1"#)
            .bind(&program_id)
            .execute(tx.as_mut())
            .await
            .map_err(|e| ApiError::StorageError(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| ApiError::StorageError(e.to_string()))?;

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Response;
    use http_body_util::BodyExt;

    use super::*;
    use crate::api::ApiError;

    async fn response_json(response: Response<Body>) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn register_request_deserializes_without_idl() {
        let json = r#"{"program_id": "abc123"}"#;
        let req: RegisterProgramRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.program_id, "abc123");
        assert!(req.idl.is_none());
    }

    #[test]
    fn register_request_deserializes_with_idl() {
        let json = r#"{"program_id": "abc123", "idl": {"metadata": {"name": "test"}}}"#;
        let req: RegisterProgramRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.program_id, "abc123");
        assert!(req.idl.is_some());
    }

    #[tokio::test]
    async fn api_error_program_not_found_returns_404() {
        let err = ApiError::ProgramNotFound("abc123".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "PROGRAM_NOT_FOUND");
    }

    #[tokio::test]
    async fn api_error_already_registered_returns_409() {
        let err = ApiError::ProgramAlreadyRegistered("abc123".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "PROGRAM_ALREADY_REGISTERED");
    }

    #[tokio::test]
    async fn api_error_invalid_filter_returns_400() {
        let err = ApiError::InvalidFilter("bad filter".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "INVALID_FILTER");
    }

    #[tokio::test]
    async fn api_error_invalid_request_returns_400() {
        let err = ApiError::InvalidRequest("bad request".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "INVALID_REQUEST");
    }

    #[tokio::test]
    async fn api_error_idl_error_returns_422() {
        let err = ApiError::IdlError("parse failed".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "IDL_ERROR");
    }

    #[tokio::test]
    async fn api_error_storage_error_returns_500_without_details() {
        let err = ApiError::StorageError("secret db info".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "STORAGE_ERROR");
        assert_eq!(body["error"]["message"], "Internal storage error");
    }

    #[tokio::test]
    async fn api_error_query_failed_returns_500_without_details() {
        let err = ApiError::QueryFailed("secret query info".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "QUERY_FAILED");
        assert_eq!(body["error"]["message"], "Query execution failed");
    }

    #[test]
    fn delete_query_defaults_drop_tables_to_false() {
        let json = r#"{}"#;
        let q: DeleteProgramQuery = serde_json::from_str(json).unwrap();
        assert!(!q.drop_tables);
    }

    #[test]
    fn delete_query_parses_drop_tables_true() {
        let json = r#"{"drop_tables": true}"#;
        let q: DeleteProgramQuery = serde_json::from_str(json).unwrap();
        assert!(q.drop_tables);
    }

    #[test]
    fn validate_program_id_accepts_valid_pubkey() {
        // System program
        assert!(validate_program_id("11111111111111111111111111111111").is_ok());
        // Token program
        assert!(validate_program_id("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").is_ok());
    }

    #[test]
    fn validate_program_id_rejects_empty() {
        assert!(validate_program_id("").is_err());
    }

    #[test]
    fn validate_program_id_rejects_non_base58() {
        // 'l' (lowercase L) and '0' (zero) are not in base58 alphabet
        assert!(validate_program_id("0000000000000000000000000000000000000000000").is_err());
        assert!(validate_program_id("llllllllllllllllllllllllllllllll").is_err());
    }

    #[test]
    fn validate_program_id_rejects_wrong_length() {
        // Too short — valid base58 but not 32 bytes
        assert!(validate_program_id("abc").is_err());
    }
}
