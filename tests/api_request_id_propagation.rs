//! Story 6.1 AC4 — request ID propagation.
//!
//! Asserts that:
//! 1. A request without `X-Request-Id` produces a response with a UUIDv7 ID.
//! 2. A request carrying a client-supplied `X-Request-Id` is echoed unchanged.
//! 3. 4xx responses (e.g. a handler returning `NOT_FOUND`) also carry the
//!    header.
//!
//! Uses a minimal router built directly from `tower-http`'s request-id layers
//! so the test is hermetic (no DB, no registry). Wiring the full production
//! router would require a PgPool — out of scope for AC4 unit-level checks.
//!
//! NOTE on the transport: we use `tower::ServiceExt::oneshot` instead of
//! `axum_test::TestServer` because `axum-test 16.x` targets `axum 0.7`
//! while the project is on `axum 0.8` — the `Router<S>` type from the
//! newer axum does not implement axum-test's `IntoTransportLayer`.

#![allow(clippy::expect_used)]

use std::convert::Infallible;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct SolarixRequestId;

impl MakeRequestId for SolarixRequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7().to_string();
        HeaderValue::from_str(&id).ok().map(RequestId::new)
    }
}

async fn ok_handler() -> Result<Response, Infallible> {
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::from("ok"))
        .expect("build response"))
}

async fn not_found_handler() -> Result<Response, Infallible> {
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("nope"))
        .expect("build response"))
}

fn test_router() -> Router {
    let header = HeaderName::from_static("x-request-id");
    Router::new()
        .route("/ok", get(ok_handler))
        .route("/missing", get(not_found_handler))
        .layer(PropagateRequestIdLayer::new(header.clone()))
        .layer(SetRequestIdLayer::new(header, SolarixRequestId))
}

async fn send(router: Router, req: Request<Body>) -> Response {
    router.oneshot(req).await.expect("oneshot dispatch")
}

#[tokio::test]
async fn missing_request_id_is_filled_with_uuid_v7() {
    let router = test_router();
    let req = Request::builder()
        .uri("/ok")
        .body(Body::empty())
        .expect("build request");

    let response = send(router, req).await;
    assert_eq!(response.status(), StatusCode::OK);

    let header = response
        .headers()
        .get("x-request-id")
        .expect("response must carry x-request-id");
    let id = header.to_str().expect("ascii");
    let uuid = Uuid::parse_str(id).expect("header must be a parseable UUID");
    // UUIDv7 version nibble is 7.
    assert_eq!(uuid.get_version_num(), 7, "expected UUIDv7, got {uuid}");
}

#[tokio::test]
async fn client_supplied_request_id_is_echoed_unchanged() {
    let router = test_router();
    // Use a UUIDv7 string so the value round-trips through header validation.
    let supplied = "0193ae0a-1111-7222-8333-444455556666";
    let req = Request::builder()
        .uri("/ok")
        .header("x-request-id", supplied)
        .body(Body::empty())
        .expect("build request");

    let response = send(router, req).await;
    assert_eq!(response.status(), StatusCode::OK);

    let header = response
        .headers()
        .get("x-request-id")
        .expect("header present");
    assert_eq!(header.to_str().expect("ascii"), supplied);
}

#[tokio::test]
async fn error_responses_also_carry_request_id() {
    let router = test_router();
    let req = Request::builder()
        .uri("/missing")
        .body(Body::empty())
        .expect("build request");

    let response = send(router, req).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let header = response
        .headers()
        .get("x-request-id")
        .expect("4xx responses must also carry x-request-id");
    let id = header.to_str().expect("ascii");
    Uuid::parse_str(id).expect("header must be a parseable UUID");
}
