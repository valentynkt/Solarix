use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use tokio::time::timeout;

use super::AppState;

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
