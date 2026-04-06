pub mod filters;
pub mod handlers;

use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::error;

use crate::config::Config;
use crate::registry::{ProgramRegistry, RegistrationError};

/// Shared application state passed to all handlers.
pub struct AppState {
    pub pool: PgPool,
    pub start_time: Instant,
    pub registry: Arc<RwLock<ProgramRegistry>>,
    pub config: Config,
}

/// Errors that can occur in API request handling.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("invalid filter: {message}")]
    InvalidFilter {
        message: String,
        available_fields: Vec<String>,
    },

    #[error("program not found: {0}")]
    ProgramNotFound(String),

    #[error("program already registered: {0}")]
    ProgramAlreadyRegistered(String),

    #[error("query failed: {0}")]
    QueryFailed(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("IDL error: {0}")]
    IdlError(String),

    #[error("invalid value: {0}")]
    InvalidValue(String),

    #[error("instruction not found: {0}")]
    InstructionNotFound(String),

    #[error("account type not found: {0}")]
    AccountTypeNotFound(String),

    #[error("account not found: {0}")]
    AccountNotFound(String),

    #[error("storage error: {0}")]
    StorageError(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            ApiError::ProgramNotFound(id) => (
                StatusCode::NOT_FOUND,
                "PROGRAM_NOT_FOUND",
                format!("Program '{id}' is not registered"),
            ),
            ApiError::ProgramAlreadyRegistered(id) => (
                StatusCode::CONFLICT,
                "PROGRAM_ALREADY_REGISTERED",
                format!("Program '{id}' is already registered"),
            ),
            ApiError::InvalidFilter {
                message,
                available_fields,
            } => {
                let body = json!({
                    "error": {
                        "code": "INVALID_FILTER",
                        "message": message,
                        "available_fields": available_fields,
                    }
                });
                return (StatusCode::BAD_REQUEST, Json(body)).into_response();
            }
            ApiError::InvalidRequest(msg) => {
                (StatusCode::BAD_REQUEST, "INVALID_REQUEST", msg.clone())
            }
            ApiError::InvalidValue(msg) => (StatusCode::BAD_REQUEST, "INVALID_VALUE", msg.clone()),
            ApiError::InstructionNotFound(name) => (
                StatusCode::NOT_FOUND,
                "INSTRUCTION_NOT_FOUND",
                format!("Instruction '{name}' not found in IDL"),
            ),
            ApiError::AccountTypeNotFound(name) => (
                StatusCode::NOT_FOUND,
                "ACCOUNT_TYPE_NOT_FOUND",
                format!("Account type '{name}' not found in IDL"),
            ),
            ApiError::AccountNotFound(key) => (
                StatusCode::NOT_FOUND,
                "ACCOUNT_NOT_FOUND",
                format!("Account '{key}' not found"),
            ),
            ApiError::IdlError(msg) => (StatusCode::UNPROCESSABLE_ENTITY, "IDL_ERROR", msg.clone()),
            ApiError::StorageError(msg) => {
                error!(error = %msg, "storage error in API handler");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "STORAGE_ERROR",
                    "Internal storage error".to_string(),
                )
            }
            ApiError::QueryFailed(msg) => {
                error!(error = %msg, "query failed in API handler");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "QUERY_FAILED",
                    "Query execution failed".to_string(),
                )
            }
        };

        let body = json!({
            "error": {
                "code": code,
                "message": message,
            }
        });

        (status, Json(body)).into_response()
    }
}

impl From<RegistrationError> for ApiError {
    fn from(err: RegistrationError) -> Self {
        match err {
            RegistrationError::AlreadyRegistered(id) => ApiError::ProgramAlreadyRegistered(id),
            RegistrationError::Idl(e) => ApiError::IdlError(e.to_string()),
            RegistrationError::DatabaseError(msg) => ApiError::StorageError(msg),
            RegistrationError::SchemaFailed(e) => ApiError::StorageError(e.to_string()),
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let program_routes = Router::new()
        .route(
            "/",
            post(handlers::register_program).get(handlers::list_programs),
        )
        .route(
            "/{id}",
            get(handlers::get_program).delete(handlers::delete_program),
        )
        .route("/{id}/instructions", get(handlers::list_instruction_types))
        .route(
            "/{id}/instructions/{name}",
            get(handlers::query_instructions),
        )
        .route(
            "/{id}/instructions/{name}/count",
            get(handlers::instruction_count),
        )
        .route("/{id}/stats", get(handlers::program_stats))
        .route("/{id}/accounts", get(handlers::list_account_types))
        .route("/{id}/accounts/{type}", get(handlers::query_accounts))
        .route("/{id}/accounts/{type}/{pubkey}", get(handlers::get_account));

    Router::new()
        .nest("/api/programs", program_routes)
        .route("/health", get(handlers::health))
        .with_state(state)
}
