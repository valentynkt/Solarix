use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::AppState;

pub async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let db_ok = sqlx::query("SELECT 1").fetch_one(&state.pool).await.is_ok();

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
