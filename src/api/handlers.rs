use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anchor_lang_idl_spec::{IdlDefinedFields, IdlField, IdlTypeDef, IdlTypeDefTy};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::time::timeout;
use tracing::info;

use crate::api::filters::{
    parse_filters, resolve_filters, ColumnExpr, FilterContext, FilterOp, ResolvedFilter,
};
use crate::idl::IdlManager;
use crate::registry::ProgramRegistry;
use crate::storage::queries::{append_order_and_limit, build_query, build_query_base, QueryTarget};
use crate::storage::schema::{quote_ident, sanitize_identifier};

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

    // Per-program status — graceful fallback if query fails
    let programs = if db_ok {
        let result = sqlx::query(
            r#"SELECT p."program_id", p."status" AS program_status,
                      i."status" AS pipeline_status, i."last_processed_slot",
                      i."last_heartbeat", i."total_instructions", i."total_accounts"
               FROM "programs" p
               LEFT JOIN "indexer_state" i ON p."program_id" = i."program_id""#,
        )
        .fetch_all(&state.pool)
        .await;

        match result {
            Ok(rows) => Some(
                rows.iter()
                    .map(|row| {
                        let heartbeat: Option<chrono::DateTime<chrono::Utc>> =
                            row.get("last_heartbeat");
                        json!({
                            "program_id": row.get::<String, _>("program_id"),
                            "status": row.get::<Option<String>, _>("pipeline_status"),
                            "last_processed_slot": row.get::<Option<i64>, _>("last_processed_slot"),
                            "last_heartbeat": heartbeat.map(|t| t.to_rfc3339()),
                            "total_instructions": row.get::<Option<i64>, _>("total_instructions").unwrap_or(0),
                            "total_accounts": row.get::<Option<i64>, _>("total_accounts").unwrap_or(0),
                        })
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(_) => None,
        }
    } else {
        None
    };

    (
        http_status,
        Json(json!({
            "status": status,
            "database": db_status,
            "uptime_seconds": uptime,
            "version": version,
            "programs": programs,
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
    let idl_was_auto_fetched = if idl_json.is_none() {
        auto_fetch_idl(Arc::clone(&registry), body.program_id.clone()).await?
    } else {
        false
    };

    let data = prepare_registration(Arc::clone(&registry), body.program_id, idl_json).await?;

    let was_cached = data.was_cached;
    let program_id_for_rollback = data.program_id.clone();
    let result = ProgramRegistry::commit_registration(pool, data).await;

    // Rollback cache if we added the entry during this request:
    // - Manual upload that wasn't previously cached (!was_cached), OR
    // - Auto-fetched IDL (was_cached is true post-fetch, but we just added it)
    if result.is_err() && (!was_cached || idl_was_auto_fetched) {
        rollback_cache(registry, program_id_for_rollback).await;
    }

    let program_info = result?;

    info!(
        program_id = %program_info.program_id,
        idl_source = %program_info.idl_source,
        "program registered via API"
    );

    Ok((
        StatusCode::CREATED,
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
///
/// Returns `true` if a new IDL was fetched and cached, `false` if it was
/// already cached (no-op). Used by the caller to decide whether to rollback
/// the cache entry on registration failure.
fn auto_fetch_idl(
    registry: Arc<tokio::sync::RwLock<ProgramRegistry>>,
    program_id: String,
) -> Pin<Box<dyn Future<Output = Result<bool, ApiError>> + Send>> {
    Box::pin(async move {
        // Check cache + get fetch params under one read lock
        let params = {
            let guard = registry.read().await;
            if guard.idl_manager.get_cached(&program_id).is_some() {
                return Ok(false);
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

        Ok(true)
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
    let idl_source: Option<String> = row.get("idl_source");
    let idl_hash: Option<String> = row.get("idl_hash");
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

// ---------------------------------------------------------------------------
// Pagination helpers
// ---------------------------------------------------------------------------

fn clamp_limit(params: &HashMap<String, String>, config: &crate::config::Config) -> i64 {
    params
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|v| {
            if v <= 0 {
                None // treat zero/negative as missing → use default
            } else {
                Some(v.min(config.api_max_page_size as i64))
            }
        })
        .unwrap_or(config.api_default_page_size as i64)
}

fn clamp_offset(params: &HashMap<String, String>) -> i64 {
    params
        .get("offset")
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| v.max(0))
        .unwrap_or(0)
}

fn encode_cursor(slot: i64, signature: &str) -> String {
    STANDARD.encode(format!("{slot}_{signature}"))
}

fn decode_cursor(cursor: &str) -> Result<(i64, String), ApiError> {
    let decoded = STANDARD
        .decode(cursor)
        .map_err(|_| ApiError::InvalidValue("invalid cursor encoding".to_string()))?;
    let s = String::from_utf8(decoded)
        .map_err(|_| ApiError::InvalidValue("invalid cursor encoding".to_string()))?;
    let (slot_str, sig) = s
        .split_once('_')
        .ok_or_else(|| ApiError::InvalidValue("invalid cursor format".to_string()))?;
    let slot = slot_str
        .parse::<i64>()
        .map_err(|_| ApiError::InvalidValue("invalid cursor slot".to_string()))?;
    Ok((slot, sig.to_string()))
}

// ---------------------------------------------------------------------------
// Shared query helpers
// ---------------------------------------------------------------------------

/// Map sqlx errors to ApiError, detecting PostgreSQL type cast failures (22P02, 22003)
/// and returning 400 InvalidValue instead of 500 QueryFailed.
fn map_query_error(e: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(ref db_err) = e {
        if let Some(code) = db_err.code() {
            if code == "22P02" || code == "22003" {
                return ApiError::InvalidValue(format!(
                    "filter value type mismatch: {}",
                    db_err.message()
                ));
            }
        }
    }
    ApiError::QueryFailed(e.to_string())
}

async fn get_schema_name(pool: &sqlx::PgPool, program_id: &str) -> Result<String, ApiError> {
    let row = sqlx::query(r#"SELECT "schema_name" FROM "programs" WHERE "program_id" = $1"#)
        .bind(program_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::QueryFailed(e.to_string()))?
        .ok_or_else(|| ApiError::ProgramNotFound(program_id.to_string()))?;
    Ok(row.get("schema_name"))
}

fn get_account_fields(account_name: &str, types: &[IdlTypeDef]) -> Result<Vec<IdlField>, ApiError> {
    let type_def = types
        .iter()
        .find(|t| t.name == account_name)
        .ok_or_else(|| ApiError::AccountTypeNotFound(account_name.to_string()))?;
    match &type_def.ty {
        IdlTypeDefTy::Struct {
            fields: Some(fields),
        } => match fields {
            IdlDefinedFields::Named(named) => Ok(named.clone()),
            _ => Ok(vec![]),
        },
        _ => Ok(vec![]),
    }
}

fn instruction_row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "signature": row.get::<String, _>("signature"),
        "slot": row.get::<i64, _>("slot"),
        "block_time": row.get::<Option<i64>, _>("block_time"),
        "instruction_name": row.get::<String, _>("instruction_name"),
        "args": row.get::<Value, _>("args"),
        "accounts": row.get::<Value, _>("accounts"),
        "data": row.get::<Value, _>("data"),
    })
}

fn account_row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "pubkey": row.get::<String, _>("pubkey"),
        "slot_updated": row.get::<i64, _>("slot_updated"),
        "lamports": row.get::<i64, _>("lamports"),
        "data": row.get::<Value, _>("data"),
    })
}

// ---------------------------------------------------------------------------
// Instruction & Account query handlers
// ---------------------------------------------------------------------------

pub async fn list_instruction_types(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    let names: Vec<String> = {
        let registry = state.registry.read().await;
        let idl = registry
            .get_idl(&id)
            .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
        idl.instructions.iter().map(|i| i.name.clone()).collect()
    };
    let total = names.len();

    Ok(Json(json!({
        "data": names,
        "meta": { "program_id": id, "total": total }
    })))
}

pub async fn query_instructions(
    State(state): State<Arc<AppState>>,
    Path((id, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    // Get IDL data, drop lock before await
    let (instruction_args, types) = {
        let registry = state.registry.read().await;
        let idl = registry
            .get_idl(&id)
            .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
        let instruction = idl
            .instructions
            .iter()
            .find(|i| i.name == name)
            .ok_or_else(|| ApiError::InstructionNotFound(name.clone()))?;
        (instruction.args.clone(), idl.types.clone())
    };

    let limit = clamp_limit(&params, &state.config);
    let cursor_param = params.get("cursor").cloned();

    // Parse and resolve filters
    let parsed = parse_filters(&params);
    let mut resolved = resolve_filters(
        &parsed,
        &instruction_args,
        &types,
        FilterContext::Instructions,
    )?;

    // Inject instruction_name filter
    resolved.push(ResolvedFilter {
        column_expr: ColumnExpr::Promoted {
            column: "instruction_name".to_string(),
        },
        op: FilterOp::Eq,
        value: name.clone(),
    });

    let schema_name = get_schema_name(&state.pool, &id).await?;
    let target = QueryTarget::Instructions {
        schema: schema_name,
    };

    // Build query base (SELECT + FROM + WHERE), then inject cursor before ORDER BY/LIMIT
    let fetch_limit = limit + 1;
    let (mut qb, _) = build_query_base(&target, &resolved);

    // Inject cursor condition before ORDER BY/LIMIT
    if let Some(ref cursor) = cursor_param {
        let (cursor_slot, cursor_sig) = decode_cursor(cursor)?;
        // instruction_name filter ensures WHERE exists, so AND is safe
        qb.push(" AND (");
        qb.push(r#""slot" < "#);
        qb.push_bind(cursor_slot);
        qb.push(r#" OR ("slot" = "#);
        qb.push_bind(cursor_slot);
        qb.push(r#" AND "signature" < "#);
        qb.push_bind(cursor_sig);
        qb.push("))");
    }

    append_order_and_limit(&mut qb, &target, fetch_limit, 0);

    let start = std::time::Instant::now();
    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(map_query_error)?;
    let query_time_ms = start.elapsed().as_millis() as u64;

    let has_more = rows.len() as i64 > limit;
    let result_rows = if has_more {
        &rows[..limit as usize]
    } else {
        &rows[..]
    };

    let data: Vec<Value> = result_rows.iter().map(instruction_row_to_json).collect();

    let next_cursor = if has_more {
        let last = &result_rows[result_rows.len() - 1];
        let last_slot: i64 = last.get("slot");
        let last_sig: String = last.get("signature");
        Some(encode_cursor(last_slot, &last_sig))
    } else {
        None
    };

    Ok(Json(json!({
        "data": data,
        "pagination": {
            "limit": limit,
            "has_more": has_more,
            "next_cursor": next_cursor,
        },
        "meta": {
            "program_id": id,
            "instruction": name,
            "query_time_ms": query_time_ms,
        }
    })))
}

pub async fn list_account_types(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    let names: Vec<String> = {
        let registry = state.registry.read().await;
        let idl = registry
            .get_idl(&id)
            .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
        idl.accounts.iter().map(|a| a.name.clone()).collect()
    };
    let total = names.len();

    Ok(Json(json!({
        "data": names,
        "meta": { "program_id": id, "total": total }
    })))
}

pub async fn query_accounts(
    State(state): State<Arc<AppState>>,
    Path((id, account_type)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    // Get IDL data, drop lock before await
    let (account_fields, types) = {
        let registry = state.registry.read().await;
        let idl = registry
            .get_idl(&id)
            .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;

        // Validate account type exists in IDL
        if !idl.accounts.iter().any(|a| a.name == account_type) {
            return Err(ApiError::AccountTypeNotFound(account_type.clone()));
        }

        let types = idl.types.clone();
        let fields = get_account_fields(&account_type, &types)?;
        (fields, types)
    };

    let limit = clamp_limit(&params, &state.config);
    let offset = clamp_offset(&params);

    // Parse and resolve filters
    let parsed = parse_filters(&params);
    let resolved = resolve_filters(&parsed, &account_fields, &types, FilterContext::Accounts)?;

    let schema_name = get_schema_name(&state.pool, &id).await?;
    let table_name = sanitize_identifier(&account_type);
    let target = QueryTarget::Accounts {
        schema: schema_name.clone(),
        table: table_name.clone(),
    };

    // Fetch limit + 1 for has_more detection
    let fetch_limit = limit + 1;
    let mut qb = build_query(&target, &resolved, fetch_limit, offset);

    // Count query (unfiltered total for this account type)
    let count_sql = format!(
        "SELECT COUNT(*) as count FROM {}.{}",
        quote_ident(&schema_name),
        quote_ident(&table_name)
    );

    let start = std::time::Instant::now();
    let (rows, total) = tokio::try_join!(
        async {
            qb.build()
                .fetch_all(&state.pool)
                .await
                .map_err(map_query_error)
        },
        async {
            let row = sqlx::query(&count_sql)
                .fetch_one(&state.pool)
                .await
                .map_err(map_query_error)?;
            Ok::<i64, ApiError>(row.get("count"))
        }
    )?;
    let query_time_ms = start.elapsed().as_millis() as u64;

    let has_more = rows.len() as i64 > limit;
    let result_rows = if has_more {
        &rows[..limit as usize]
    } else {
        &rows[..]
    };

    let data: Vec<Value> = result_rows.iter().map(account_row_to_json).collect();

    Ok(Json(json!({
        "data": data,
        "pagination": {
            "total": total,
            "limit": limit,
            "offset": offset,
            "has_more": has_more,
        },
        "meta": {
            "program_id": id,
            "account_type": account_type,
            "query_time_ms": query_time_ms,
        }
    })))
}

pub async fn get_account(
    State(state): State<Arc<AppState>>,
    Path((id, account_type, pubkey)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    // Validate account type exists in IDL
    {
        let registry = state.registry.read().await;
        let idl = registry
            .get_idl(&id)
            .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
        if !idl.accounts.iter().any(|a| a.name == account_type) {
            return Err(ApiError::AccountTypeNotFound(account_type.clone()));
        }
    }

    let schema_name = get_schema_name(&state.pool, &id).await?;
    let table_name = sanitize_identifier(&account_type);

    let sql = format!(
        "SELECT * FROM {}.{} WHERE {} = $1",
        quote_ident(&schema_name),
        quote_ident(&table_name),
        quote_ident("pubkey"),
    );

    let row = sqlx::query(&sql)
        .bind(&pubkey)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_query_error)?
        .ok_or_else(|| ApiError::AccountNotFound(pubkey.clone()))?;

    let data = account_row_to_json(&row);
    Ok(Json(json!({ "data": data })))
}

// ---------------------------------------------------------------------------
// Aggregation & statistics handlers (Story 5.4)
// ---------------------------------------------------------------------------

const VALID_INTERVALS: &[&str] = &["minute", "hour", "day", "week", "month"];

fn validate_interval(params: &HashMap<String, String>) -> Result<&'static str, ApiError> {
    let raw = params
        .get("interval")
        .ok_or_else(|| ApiError::InvalidValue("'interval' parameter is required".to_string()))?;
    VALID_INTERVALS
        .iter()
        .find(|&&v| v == raw.as_str())
        .copied()
        .ok_or_else(|| {
            ApiError::InvalidValue(format!(
                "invalid interval '{}'. Must be one of: minute, hour, day, week, month",
                raw
            ))
        })
}

fn parse_optional_i64(
    params: &HashMap<String, String>,
    key: &str,
) -> Result<Option<i64>, ApiError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.parse::<i64>().map(Some).map_err(|_| {
            ApiError::InvalidValue(format!("'{}' must be a Unix timestamp integer", key))
        }),
    }
}

pub async fn instruction_count(
    State(state): State<Arc<AppState>>,
    Path((id, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    // Validate instruction name exists in IDL
    {
        let registry = state.registry.read().await;
        let idl = registry
            .get_idl(&id)
            .ok_or_else(|| ApiError::ProgramNotFound(id.clone()))?;
        if !idl.instructions.iter().any(|i| i.name == name) {
            return Err(ApiError::InstructionNotFound(name.clone()));
        }
    }

    let interval = validate_interval(&params)?;
    let from = parse_optional_i64(&params, "from")?;
    let to = parse_optional_i64(&params, "to")?;

    let schema_name = get_schema_name(&state.pool, &id).await?;

    let mut qb = sqlx::QueryBuilder::new("SELECT date_trunc(");
    qb.push_bind(interval.to_string());
    qb.push(", to_timestamp(\"block_time\")) AS bucket, COUNT(*) AS count FROM ");
    qb.push(format!(
        "{}.{}",
        quote_ident(&schema_name),
        quote_ident("_instructions")
    ));
    qb.push(" WHERE \"instruction_name\" = ");
    qb.push_bind(name.clone());
    qb.push(" AND \"block_time\" IS NOT NULL");

    if let Some(from_val) = from {
        qb.push(" AND \"block_time\" >= ");
        qb.push_bind(from_val);
    }
    if let Some(to_val) = to {
        qb.push(" AND \"block_time\" <= ");
        qb.push_bind(to_val);
    }

    qb.push(" GROUP BY bucket ORDER BY bucket ASC");

    let start = std::time::Instant::now();
    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(map_query_error)?;
    let query_time_ms = start.elapsed().as_millis() as u64;

    let data: Vec<Value> = rows
        .iter()
        .map(|row| {
            let bucket: chrono::DateTime<chrono::Utc> = row.get("bucket");
            let count: i64 = row.get("count");
            json!({ "bucket": bucket.to_rfc3339(), "count": count })
        })
        .collect();

    Ok(Json(json!({
        "data": data,
        "meta": {
            "program_id": id,
            "instruction": name,
            "interval": interval,
            "query_time_ms": query_time_ms,
        }
    })))
}

pub async fn program_stats(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    validate_program_id(&id)?;

    let schema_name = get_schema_name(&state.pool, &id).await?;

    let start = std::time::Instant::now();

    // Query indexer_state for pre-computed totals + _instructions for aggregates
    let (state_row, ix_rows) = tokio::try_join!(
        async {
            sqlx::query(
                r#"SELECT "total_instructions", "total_accounts"
                   FROM "indexer_state" WHERE "program_id" = $1"#,
            )
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_query_error)
        },
        async {
            let sql = format!(
                r#"SELECT MIN("slot") AS first_seen_slot, MAX("slot") AS last_seen_slot,
                          "instruction_name", COUNT(*) AS count
                   FROM {}.{} GROUP BY "instruction_name""#,
                quote_ident(&schema_name),
                quote_ident("_instructions")
            );
            sqlx::query(&sql)
                .fetch_all(&state.pool)
                .await
                .map_err(map_query_error)
        }
    )?;

    let query_time_ms = start.elapsed().as_millis() as u64;

    let (total_instructions, total_accounts) = match state_row {
        Some(row) => {
            let ti: i64 = row.get("total_instructions");
            let ta: i64 = row.get("total_accounts");
            (ti, ta)
        }
        None => return Err(ApiError::ProgramNotFound(id)),
    };

    let mut first_seen_slot: Option<i64> = None;
    let mut last_seen_slot: Option<i64> = None;
    let mut instruction_counts = serde_json::Map::new();

    for row in &ix_rows {
        let row_first: Option<i64> = row.get("first_seen_slot");
        let row_last: Option<i64> = row.get("last_seen_slot");
        let ix_name: String = row.get("instruction_name");
        let count: i64 = row.get("count");

        if let Some(f) = row_first {
            first_seen_slot = Some(first_seen_slot.map_or(f, |cur: i64| cur.min(f)));
        }
        if let Some(l) = row_last {
            last_seen_slot = Some(last_seen_slot.map_or(l, |cur: i64| cur.max(l)));
        }
        instruction_counts.insert(ix_name, Value::Number(count.into()));
    }

    Ok(Json(json!({
        "data": {
            "total_instructions": total_instructions,
            "total_accounts": total_accounts,
            "first_seen_slot": first_seen_slot,
            "last_seen_slot": last_seen_slot,
            "instruction_counts": Value::Object(instruction_counts),
        },
        "meta": {
            "program_id": id,
            "query_time_ms": query_time_ms,
        }
    })))
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
        let err = ApiError::InvalidFilter {
            message: "bad filter".to_string(),
            available_fields: vec!["amount".to_string(), "owner".to_string()],
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "INVALID_FILTER");
        assert_eq!(body["error"]["available_fields"][0], "amount");
        assert_eq!(body["error"]["available_fields"][1], "owner");
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

    // --- Story 5.3: Pagination helpers ---

    fn make_config() -> crate::config::Config {
        crate::config::Config {
            rpc_url: String::new(),
            ws_url: None,
            database_url: String::new(),
            db_pool_min: 2,
            db_pool_max: 10,
            rpc_rps: 10,
            backfill_chunk_size: 50_000,
            start_slot: None,
            end_slot: None,
            index_failed_txs: false,
            api_host: String::new(),
            api_port: 3000,
            api_default_page_size: 50,
            api_max_page_size: 1000,
            channel_capacity: 256,
            checkpoint_interval_secs: 10,
            retry_initial_ms: 500,
            retry_max_ms: 30_000,
            retry_timeout_secs: 300,
            max_consecutive_fetch_failures: 100,
            ws_ping_interval_secs: 30,
            ws_pong_timeout_secs: 10,
            dedup_cache_size: 10_000,
            log_level: String::new(),
            log_format: String::new(),
        }
    }

    #[test]
    fn clamp_limit_default() {
        let config = make_config();
        let params = HashMap::new();
        assert_eq!(clamp_limit(&params, &config), 50);
    }

    #[test]
    fn clamp_limit_valid_value() {
        let config = make_config();
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "25".to_string());
        assert_eq!(clamp_limit(&params, &config), 25);
    }

    #[test]
    fn clamp_limit_over_max_clamped() {
        let config = make_config();
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "5000".to_string());
        assert_eq!(clamp_limit(&params, &config), 1000);
    }

    #[test]
    fn clamp_limit_negative_uses_default() {
        let config = make_config();
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "-5".to_string());
        assert_eq!(clamp_limit(&params, &config), 50);
    }

    #[test]
    fn clamp_limit_zero_uses_default() {
        let config = make_config();
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "0".to_string());
        assert_eq!(clamp_limit(&params, &config), 50);
    }

    #[test]
    fn clamp_limit_non_numeric_uses_default() {
        let config = make_config();
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "abc".to_string());
        assert_eq!(clamp_limit(&params, &config), 50);
    }

    #[test]
    fn clamp_offset_default() {
        let params = HashMap::new();
        assert_eq!(clamp_offset(&params), 0);
    }

    #[test]
    fn clamp_offset_valid_value() {
        let mut params = HashMap::new();
        params.insert("offset".to_string(), "100".to_string());
        assert_eq!(clamp_offset(&params), 100);
    }

    #[test]
    fn clamp_offset_negative_clamped_to_zero() {
        let mut params = HashMap::new();
        params.insert("offset".to_string(), "-10".to_string());
        assert_eq!(clamp_offset(&params), 0);
    }

    #[test]
    fn clamp_offset_non_numeric_uses_default() {
        let mut params = HashMap::new();
        params.insert("offset".to_string(), "abc".to_string());
        assert_eq!(clamp_offset(&params), 0);
    }

    // --- Cursor encode/decode ---

    #[test]
    fn cursor_encode_decode_roundtrip() {
        let slot = 123456_i64;
        let sig = "5VERv8NMvzbJMEkV8xnrLkEaWRtSz9CosKDYjCJjBRnbJLgp8";
        let encoded = encode_cursor(slot, sig);
        let (decoded_slot, decoded_sig) = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded_slot, slot);
        assert_eq!(decoded_sig, sig);
    }

    #[test]
    fn cursor_decode_invalid_base64() {
        let result = decode_cursor("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn cursor_decode_missing_separator() {
        let encoded = STANDARD.encode("nounderscore");
        let result = decode_cursor(&encoded);
        assert!(result.is_err());
    }

    #[test]
    fn cursor_decode_non_numeric_slot() {
        let encoded = STANDARD.encode("abc_sig123");
        let result = decode_cursor(&encoded);
        assert!(result.is_err());
    }

    // --- New ApiError variants ---

    #[tokio::test]
    async fn api_error_instruction_not_found_returns_404() {
        let err = ApiError::InstructionNotFound("swap".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "INSTRUCTION_NOT_FOUND");
        assert!(body["error"]["message"].as_str().unwrap().contains("swap"));
    }

    #[tokio::test]
    async fn api_error_account_type_not_found_returns_404() {
        let err = ApiError::AccountTypeNotFound("Vault".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "ACCOUNT_TYPE_NOT_FOUND");
        assert!(body["error"]["message"].as_str().unwrap().contains("Vault"));
    }

    #[tokio::test]
    async fn api_error_account_not_found_returns_404() {
        let err = ApiError::AccountNotFound("ABC123pubkey".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "ACCOUNT_NOT_FOUND");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ABC123pubkey"));
    }

    // --- get_account_fields helper ---

    #[test]
    fn get_account_fields_extracts_named_struct_fields() {
        use anchor_lang_idl_spec::{IdlDefinedFields, IdlField, IdlType, IdlTypeDef, IdlTypeDefTy};

        let types = vec![IdlTypeDef {
            name: "TokenAccount".to_string(),
            docs: vec![],
            serialization: anchor_lang_idl_spec::IdlSerialization::default(),
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    IdlField {
                        name: "owner".to_string(),
                        docs: vec![],
                        ty: IdlType::Pubkey,
                    },
                    IdlField {
                        name: "amount".to_string(),
                        docs: vec![],
                        ty: IdlType::U64,
                    },
                ])),
            },
        }];

        let fields = get_account_fields("TokenAccount", &types).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "owner");
        assert_eq!(fields[1].name, "amount");
    }

    #[test]
    fn get_account_fields_returns_error_for_unknown_type() {
        let types = vec![];
        let result = get_account_fields("Nonexistent", &types);
        assert!(result.is_err());
    }

    // --- Story 5.4: Interval validation ---

    #[test]
    fn validate_interval_accepts_all_valid_values() {
        for &v in VALID_INTERVALS {
            let mut params = HashMap::new();
            params.insert("interval".to_string(), v.to_string());
            let result = validate_interval(&params);
            assert!(result.is_ok(), "expected '{}' to be valid", v);
            assert_eq!(result.unwrap(), v);
        }
    }

    #[test]
    fn validate_interval_rejects_missing() {
        let params = HashMap::new();
        let result = validate_interval(&params);
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::InvalidValue(msg) => assert!(msg.contains("required")),
            other => panic!("expected InvalidValue, got {:?}", other),
        }
    }

    #[test]
    fn validate_interval_rejects_invalid_value() {
        let mut params = HashMap::new();
        params.insert("interval".to_string(), "year".to_string());
        let result = validate_interval(&params);
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::InvalidValue(msg) => {
                assert!(msg.contains("year"));
                assert!(msg.contains("Must be one of"));
            }
            other => panic!("expected InvalidValue, got {:?}", other),
        }
    }

    #[test]
    fn validate_interval_rejects_sql_injection_attempt() {
        let mut params = HashMap::new();
        params.insert("interval".to_string(), "day'; DROP TABLE --".to_string());
        assert!(validate_interval(&params).is_err());
    }

    // --- Story 5.4: from/to parsing ---

    #[test]
    fn parse_optional_i64_returns_none_when_missing() {
        let params = HashMap::new();
        assert_eq!(parse_optional_i64(&params, "from").unwrap(), None);
    }

    #[test]
    fn parse_optional_i64_parses_valid_value() {
        let mut params = HashMap::new();
        params.insert("from".to_string(), "1712448000".to_string());
        assert_eq!(
            parse_optional_i64(&params, "from").unwrap(),
            Some(1712448000)
        );
    }

    #[test]
    fn parse_optional_i64_parses_negative_value() {
        let mut params = HashMap::new();
        params.insert("from".to_string(), "-100".to_string());
        assert_eq!(parse_optional_i64(&params, "from").unwrap(), Some(-100));
    }

    #[test]
    fn parse_optional_i64_rejects_non_numeric() {
        let mut params = HashMap::new();
        params.insert("from".to_string(), "abc".to_string());
        let result = parse_optional_i64(&params, "from");
        assert!(result.is_err());
        match result.unwrap_err() {
            ApiError::InvalidValue(msg) => {
                assert!(msg.contains("from"));
                assert!(msg.contains("Unix timestamp"));
            }
            other => panic!("expected InvalidValue, got {:?}", other),
        }
    }

    #[test]
    fn parse_optional_i64_rejects_float() {
        let mut params = HashMap::new();
        params.insert("to".to_string(), "1712448000.5".to_string());
        let result = parse_optional_i64(&params, "to");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn api_error_invalid_value_returns_400() {
        let err = ApiError::InvalidValue("bad interval".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "INVALID_VALUE");
    }
}
