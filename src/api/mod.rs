pub mod filters;
pub mod handlers;

use std::sync::Arc;
use std::time::Instant;

use axum::{routing::get, Router};
use sqlx::PgPool;

/// Shared application state passed to all handlers.
pub struct AppState {
    pub pool: PgPool,
    pub start_time: Instant,
}

/// Errors that can occur in API request handling.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    #[error("program not found: {0}")]
    ProgramNotFound(String),

    #[error("query failed: {0}")]
    QueryFailed(String),
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handlers::health))
        .with_state(state)
}
