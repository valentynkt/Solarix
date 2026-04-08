// Request-level test harness for Solarix API integration tests (Story 6.6 AC2).
//
// NOTE: We use `tower::ServiceExt::oneshot` instead of `axum_test::TestServer`
// because `axum-test 16.x` targets `axum 0.7` while the project is on `axum
// 0.8` — the `Router<S>` type from the newer axum does not implement
// axum-test's `IntoTransportLayer`.
//
// Pattern: call `build_test_router(pool)` per test to get a fresh router bound
// to that test's pool, then dispatch via `oneshot_get` / `oneshot_post_json` /
// `oneshot_delete`. Each helper reads the response body, asserts Content-Type,
// and returns `(StatusCode, HeaderMap, serde_json::Value)`.

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tower::ServiceExt;

use solarix::api::AppState;
use solarix::config::Config;
use solarix::idl::IdlManager;
use solarix::registry::ProgramRegistry;
use solarix::runtime_stats::RuntimeStats;

/// Build an `AppState` wired to `pool` with test-friendly defaults.
///
/// The `IdlManager` RPC URL is set to `http://localhost:8899` (unreachable) so
/// any code path that accidentally triggers an auto-fetch fails immediately with
/// a connection error rather than hitting a real RPC endpoint.
pub async fn build_test_state(pool: PgPool) -> Arc<AppState> {
    let idl_manager = IdlManager::new("http://localhost:8899".to_string());
    let registry = ProgramRegistry::new(idl_manager);
    let registry = Arc::new(RwLock::new(registry));
    let config = Config::test_default();
    let stats = Arc::new(RuntimeStats::new());

    Arc::new(AppState {
        pool,
        start_time: Instant::now(),
        registry,
        config,
        stats,
    })
}

/// Build the full production router against `pool`.
pub async fn build_test_router(pool: PgPool) -> Router {
    let state = build_test_state(pool).await;
    solarix::api::router(state)
}

/// GET `path`, return `(status, headers, body_json)`.
///
/// Asserts the `Content-Type` header is `application/json` before parsing.
pub async fn oneshot_get(router: Router, path: &str) -> (StatusCode, HeaderMap, Value) {
    let req = Request::get(path)
        .body(Body::empty())
        .expect("failed to build GET request");
    dispatch(router, req).await
}

/// POST `path` with a JSON body, return `(status, headers, body_json)`.
///
/// Asserts the `Content-Type` header is `application/json` before parsing.
pub async fn oneshot_post_json(
    router: Router,
    path: &str,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let body_bytes = serde_json::to_vec(&body).expect("failed to serialize POST body");
    let req = Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body_bytes))
        .expect("failed to build POST request");
    dispatch(router, req).await
}

/// DELETE `path`, return `(status, headers, body_json)`.
///
/// Asserts the `Content-Type` header is `application/json` before parsing.
pub async fn oneshot_delete(router: Router, path: &str) -> (StatusCode, HeaderMap, Value) {
    let req = Request::delete(path)
        .body(Body::empty())
        .expect("failed to build DELETE request");
    dispatch(router, req).await
}

/// Dispatch a `Request` via `router.oneshot`, collect the body, assert
/// `Content-Type: application/json`, and parse the body as JSON.
async fn dispatch(router: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router.oneshot(req).await.expect("router.oneshot failed");

    let status = response.status();
    let headers = response.headers().clone();

    // Assert Content-Type before parsing — catches handlers accidentally
    // returning HTML or plain text.
    let ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("application/json"),
        "expected Content-Type: application/json, got: {ct}"
    );

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to collect response body")
        .to_bytes();

    let body: Value = serde_json::from_slice(&bytes).expect("response body is not valid JSON");

    (status, headers, body)
}
