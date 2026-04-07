pub mod filters;
pub mod handlers;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::http;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn, Span};
use uuid::Uuid;

use crate::config::Config;
use crate::registry::{ProgramRegistry, RegistrationError};
use crate::runtime_stats::RuntimeStats;

/// Shared application state passed to all handlers.
pub struct AppState {
    pub pool: PgPool,
    pub start_time: Instant,
    pub registry: Arc<RwLock<ProgramRegistry>>,
    pub config: Config,
    /// Process-wide counters. Story 6.2's `/metrics` handler will read these
    /// and emit Prometheus gauges without any refactor of the pipeline or
    /// RPC layers.
    pub stats: Arc<RuntimeStats>,
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

/// Custom `MakeRequestId` that produces sortable UUIDv7 identifiers.
///
/// `tower-http` ships `MakeRequestUuid` which uses UUID v4 — we intentionally
/// pick v7 so request IDs are monotonically time-sorted. An operator
/// correlating logs across multiple services can grep by prefix to find
/// requests from a given window without parsing timestamps.
#[derive(Clone, Default)]
struct SolarixRequestId;

impl MakeRequestId for SolarixRequestId {
    fn make_request_id<B>(&mut self, _request: &http::Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7().to_string();
        HeaderValue::from_str(&id).ok().map(RequestId::new)
    }
}

/// Build the tracing span for a single HTTP request. Story 6.1 AC4.
fn solarix_make_span(req: &Request) -> Span {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let method = req.method().clone();
    let target = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    // `info_span!` supports dotted field names via the macro; per AC4 we
    // pre-populate fields with string values and leave `http.status_code`
    // + `http.duration_ms` as `Empty` to be recorded on the response.
    tracing::info_span!(
        "http.request",
        request.id = %request_id,
        http.method = %method,
        http.target = %target,
        http.route = %route,
        http.user_agent = %user_agent,
        http.status_code = tracing::field::Empty,
        http.duration_ms = tracing::field::Empty,
    )
}

/// Record response fields onto the request span and emit a completion
/// event at a level chosen by the status class. Story 6.1 AC4.
fn solarix_on_response(
    response: &http::Response<axum::body::Body>,
    latency: Duration,
    span: &Span,
) {
    let status = response.status();
    let duration_ms = latency.as_millis() as u64;
    span.record("http.status_code", status.as_u16());
    span.record("http.duration_ms", duration_ms);

    if status.is_server_error() {
        error!(
            http.status_code = status.as_u16(),
            http.duration_ms = duration_ms,
            "http request completed"
        );
    } else if status.is_client_error() {
        warn!(
            http.status_code = status.as_u16(),
            http.duration_ms = duration_ms,
            "http request completed"
        );
    } else {
        info!(
            http.status_code = status.as_u16(),
            http.duration_ms = duration_ms,
            "http request completed"
        );
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

    // Layer order: axum applies `.layer()` calls in reverse order, so the
    // LAST `.layer()` call wraps the OUTERMOST behavior. We want the
    // request-id stamping to run first (so downstream layers see the
    // header), so `SetRequestIdLayer` is applied LAST. The propagate
    // layer runs first (innermost), copying the header onto the response.
    let header = HeaderName::from_static("x-request-id");
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(solarix_make_span)
        .on_response(solarix_on_response);

    Router::new()
        .nest("/api/programs", program_routes)
        .route("/health", get(handlers::health))
        .layer(PropagateRequestIdLayer::new(header.clone()))
        .layer(trace_layer)
        .layer(SetRequestIdLayer::new(header, SolarixRequestId))
        .with_state(state)
}
