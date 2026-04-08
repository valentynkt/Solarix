// API endpoint integration tests for Solarix (Story 6.6 AC3, AC4, AC7, AC8).
//
// NOTE: We use `tower::ServiceExt::oneshot` instead of `axum_test::TestServer`
// because `axum-test 16.x` targets `axum 0.7` while the project is on `axum
// 0.8` — see `tests/common/api.rs` for details.
//
// Endpoints covered:
//   POST   /api/programs               (register — manual upload + auto-fetch negative + bad body)
//   GET    /api/programs               (list)
//   GET    /api/programs/{id}          (get)
//   DELETE /api/programs/{id}          (soft + hard)
//   GET    /api/programs/{id}/instructions
//   GET    /api/programs/{id}/instructions/{name}
//   GET    /api/programs/{id}/instructions/{name}/count
//   GET    /api/programs/{id}/stats
//   GET    /api/programs/{id}/accounts
//   GET    /api/programs/{id}/accounts/{type}
//   GET    /api/programs/{id}/accounts/{type}/{pubkey}
//   GET    /health
//
// Envelope contract (PRD §4):
//   Success: { "data": ..., "meta": {...} }   (pagination optional)
//   Error:   { "error": { "code": "...", "message": "..." } }
//   Filter error: error + "available_fields": [...]

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use serde_json::{json, Value};

use solarix::storage::writer::StorageWriter;
use solarix::types::{DecodedAccount, DecodedInstruction};

mod common;
use common::api::{build_test_router, oneshot_delete, oneshot_get, oneshot_post_json};
use common::postgres::with_postgres;

// ---------------------------------------------------------------------------
// Fixture constants
// ---------------------------------------------------------------------------

const SIMPLE_IDL_JSON: &str = include_str!("fixtures/idls/simple_v030.json");
const PROGRAM_ID: &str = "Testc11111111111111111111111111111111111111";
const SCHEMA_NAME: &str = "simple_test_program_testc111";

// A second program ID for "two-programs" tests (used in list_programs module).
const PROGRAM_ID_2: &str = "Test2c1111111111111111111111111111111111111";

// ---------------------------------------------------------------------------
// Envelope assertion helpers (AC4)
// ---------------------------------------------------------------------------

fn assert_success_envelope(body: &Value) {
    assert!(
        body.get("data").is_some(),
        "success envelope missing 'data' field; body={body}"
    );
    assert!(
        body.get("error").is_none(),
        "success envelope must not have 'error' field; body={body}"
    );
    // Note: `meta` is present on collection endpoints (list, query) but omitted
    // on single-resource endpoints (get_program, delete_program, get_account).
    // The PRD lists pagination as optional; meta presence is checked per-test
    // where the handler includes it.
}

fn assert_error_envelope(body: &Value, expected_code: &str) {
    assert!(
        body.get("data").is_none(),
        "error envelope must not have 'data' field; body={body}"
    );
    assert!(
        body["error"]["code"].as_str() == Some(expected_code),
        "expected error code '{expected_code}', got: {}; body={body}",
        body["error"]["code"]
    );
    assert!(
        body["error"]["message"].is_string(),
        "error envelope missing string 'message'; body={body}"
    );
}

fn assert_filter_error_envelope(body: &Value, expected_field: &str) {
    assert!(
        body["error"]["code"].as_str() == Some("INVALID_FILTER"),
        "expected INVALID_FILTER error; body={body}"
    );
    let fields = body["error"]["available_fields"]
        .as_array()
        .expect("available_fields must be array");
    assert!(!fields.is_empty(), "available_fields must be non-empty");
    let contains = fields.iter().any(|v| v.as_str() == Some(expected_field));
    assert!(
        contains,
        "available_fields should contain '{expected_field}'; got: {fields:?}"
    );
}

/// Parse `s` as a UUIDv7 — version nibble must be 7.
fn assert_uuidv7(s: &str) {
    // UUID format: xxxxxxxx-xxxx-7xxx-xxxx-xxxxxxxxxxxx
    assert_eq!(s.len(), 36, "UUID string wrong length: {s}");
    let parts: Vec<&str> = s.split('-').collect();
    assert_eq!(parts.len(), 5, "UUID wrong segment count: {s}");
    // Version nibble is the first char of the 3rd segment.
    assert_eq!(
        parts[2].chars().next(),
        Some('7'),
        "expected UUIDv7 version nibble '7', got: {s}"
    );
}

// ---------------------------------------------------------------------------
// POST /api/programs — register
// ---------------------------------------------------------------------------

mod register_program {
    use super::*;

    #[tokio::test]
    async fn happy_manual_upload_returns_201() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            let (status, headers, body) = oneshot_post_json(
                router,
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            assert_eq!(status, axum::http::StatusCode::CREATED);
            assert_success_envelope(&body);
            assert_eq!(body["data"]["program_id"], PROGRAM_ID);
            // Contract pin: the register response must match the DB value.
            // `commit_registration` runs synchronously so by the time 201 is
            // returned, `programs.status` is already `schema_created`.
            assert_eq!(body["data"]["status"], "schema_created");
            assert!(body["data"]["idl_source"].is_string());
            assert!(body["meta"]["message"].is_string());

            // Pin for Story 6.1: X-Request-Id header is a parseable UUIDv7
            let request_id = headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .expect("x-request-id header missing");
            assert_uuidv7(request_id);
        })
        .await;
    }

    #[tokio::test]
    async fn duplicate_registration_returns_409() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();

            // First registration
            let (s1, _, _) = oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl.clone() }),
            )
            .await;
            assert_eq!(s1, axum::http::StatusCode::CREATED);

            // Second registration — same program_id
            let (s2, _, body) = oneshot_post_json(
                router,
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;
            assert_eq!(s2, axum::http::StatusCode::CONFLICT);
            assert_error_envelope(&body, "PROGRAM_ALREADY_REGISTERED");
        })
        .await;
    }

    #[tokio::test]
    async fn auto_fetch_unreachable_rpc_returns_422() {
        // The test IdlManager points at localhost:8899 (unreachable).
        // Omitting `idl` triggers the auto-fetch path which should fail.
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            // System program address — valid base58 pubkey
            let (status, _, body) = oneshot_post_json(
                router,
                "/api/programs",
                json!({ "program_id": "11111111111111111111111111111111" }),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
            assert_error_envelope(&body, "IDL_ERROR");
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_program_id_returns_400() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) = oneshot_post_json(
                router,
                "/api/programs",
                json!({ "program_id": "not-base58" }),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
            assert_error_envelope(&body, "INVALID_REQUEST");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs — list
// ---------------------------------------------------------------------------

mod list_programs {
    use super::*;

    #[tokio::test]
    async fn empty_returns_empty_array() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) = oneshot_get(router, "/api/programs").await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let data = body["data"].as_array().unwrap();
            assert_eq!(data.len(), 0);
            assert_eq!(body["meta"]["total"], 0);
        })
        .await;
    }

    #[tokio::test]
    async fn two_programs_returned() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();

            // Register first
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl.clone() }),
            )
            .await;

            // Register second with the second program ID
            let mut idl2 = idl.clone();
            idl2["metadata"]["name"] = json!("simple_test_program_2");
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID_2, "idl": idl2 }),
            )
            .await;

            let (status, _, body) = oneshot_get(router, "/api/programs").await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let data = body["data"].as_array().unwrap();
            assert_eq!(data.len(), 2);
            assert_eq!(body["meta"]["total"], 2);
            // Each element carries required fields
            for item in data {
                assert!(item["program_id"].is_string());
                assert!(item["program_name"].is_string());
                assert!(item["status"].is_string());
                assert!(item["created_at"].is_string());
            }
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}
// ---------------------------------------------------------------------------

mod get_program {
    use super::*;

    #[tokio::test]
    async fn happy_returns_all_columns() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}")).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let d = &body["data"];
            assert_eq!(d["program_id"], PROGRAM_ID);
            assert!(d["program_name"].is_string());
            assert!(d["schema_name"].is_string());
            assert!(d["status"].is_string());
            assert!(d["created_at"].is_string());
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_program_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}")).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "PROGRAM_NOT_FOUND");
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_program_id_returns_400() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) = oneshot_get(router, "/api/programs/not-a-valid-pubkey").await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
            assert_error_envelope(&body, "INVALID_REQUEST");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// DELETE /api/programs/{id}
// ---------------------------------------------------------------------------

mod delete_program {
    use super::*;

    #[tokio::test]
    async fn soft_delete_sets_status_stopped() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) =
                oneshot_delete(router.clone(), &format!("/api/programs/{PROGRAM_ID}")).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            assert_eq!(body["data"]["action"], "stopped");
            assert_eq!(body["data"]["drop_tables"], false);

            // Check status in DB via list endpoint
            let (_, _, list_body) = oneshot_get(router, "/api/programs").await;
            let programs = list_body["data"].as_array().unwrap();
            let p = programs
                .iter()
                .find(|p| p["program_id"] == PROGRAM_ID)
                .unwrap();
            assert_eq!(p["status"], "stopped");
        })
        .await;
    }

    #[tokio::test]
    async fn hard_delete_drops_schema() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool.clone()).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_delete(
                router.clone(),
                &format!("/api/programs/{PROGRAM_ID}?drop_tables=true"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            assert_eq!(body["data"]["action"], "deleted");
            assert_eq!(body["data"]["drop_tables"], true);

            // Schema should be gone from information_schema
            let count: (i64,) = sqlx::query_as(
                "SELECT count(*) FROM information_schema.schemata WHERE schema_name = $1",
            )
            .bind(SCHEMA_NAME)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count.0, 0, "schema should have been dropped");
        })
        .await;
    }

    #[tokio::test]
    async fn delete_unknown_program_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) =
                oneshot_delete(router, &format!("/api/programs/{PROGRAM_ID}")).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "PROGRAM_NOT_FOUND");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/instructions
// ---------------------------------------------------------------------------

mod list_instruction_types {
    use super::*;

    #[tokio::test]
    async fn happy_returns_initialize() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}/instructions")).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let data = body["data"].as_array().unwrap();
            assert_eq!(data.len(), 1);
            assert_eq!(data[0], "initialize");
            assert_eq!(body["meta"]["total"], 1);
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_program_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}/instructions")).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "PROGRAM_NOT_FOUND");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/instructions/{name}
// ---------------------------------------------------------------------------

mod query_instructions {
    use super::*;

    async fn seed_instructions(pool: sqlx::PgPool, count: usize) {
        let writer = StorageWriter::new(pool.clone());
        for i in 0..count {
            let ix = DecodedInstruction {
                signature: format!("sig_{i:04}"),
                slot: i as u64,
                block_time: Some(1_700_000_000 + i as i64),
                instruction_name: "initialize".to_string(),
                args: serde_json::json!({ "value": i as u64 }),
                program_id: PROGRAM_ID.to_string(),
                accounts: vec!["payer".to_string()],
                instruction_index: 0,
                inner_index: None,
            };
            writer
                .write_block(
                    SCHEMA_NAME,
                    "backfill",
                    &[ix],
                    &[],
                    i as u64,
                    Some(&format!("sig_{i:04}")),
                )
                .await
                .expect("write_block failed");
        }
    }

    #[tokio::test]
    async fn happy_returns_3_seeded_rows() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool.clone()).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            seed_instructions(pool, 3).await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/initialize"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(body.get("data").is_some());
            assert!(body.get("pagination").is_some());
            let data = body["data"].as_array().unwrap();
            assert_eq!(data.len(), 3);
            assert_eq!(body["pagination"]["limit"], 50);
            assert_eq!(body["pagination"]["has_more"], false);
            assert!(body["pagination"]["next_cursor"].is_null());
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_instruction_name_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/nonexistent"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "INSTRUCTION_NOT_FOUND");
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_program_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/initialize"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "PROGRAM_NOT_FOUND");
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_filter_field_returns_400_with_available_fields() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/initialize?bogus_field=42"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
            assert_filter_error_envelope(&body, "value");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/instructions/{name}/count
// ---------------------------------------------------------------------------

mod instruction_count {
    use super::*;

    #[tokio::test]
    async fn happy_returns_buckets() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool.clone()).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            // Seed 3 rows with varying block_time
            let writer = StorageWriter::new(pool.clone());
            for i in 0..3_u64 {
                let ix = DecodedInstruction {
                    signature: format!("cnt_sig_{i}"),
                    slot: i,
                    block_time: Some(1_700_000_000 + i as i64 * 3600),
                    instruction_name: "initialize".to_string(),
                    args: serde_json::json!({ "value": i }),
                    program_id: PROGRAM_ID.to_string(),
                    accounts: vec![],
                    instruction_index: 0,
                    inner_index: None,
                };
                writer
                    .write_block(SCHEMA_NAME, "backfill", &[ix], &[], i, None)
                    .await
                    .unwrap();
            }

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/initialize/count?interval=hour"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let data = body["data"].as_array().unwrap();
            // 3 rows across 3 different hours → 3 buckets
            assert!(!data.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn missing_interval_returns_400() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/initialize/count"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
            assert_error_envelope(&body, "INVALID_VALUE");
            assert!(
                body["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("required"),
                "message should mention 'required'"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn invalid_interval_returns_400() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/instructions/initialize/count?interval=year"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
            assert_error_envelope(&body, "INVALID_VALUE");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/stats
// ---------------------------------------------------------------------------

mod program_stats {
    use super::*;

    #[tokio::test]
    async fn happy_returns_stats() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool.clone()).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            // Seed 3 instructions
            let writer = StorageWriter::new(pool.clone());
            for i in 0..3_u64 {
                let ix = DecodedInstruction {
                    signature: format!("stats_sig_{i}"),
                    slot: i,
                    block_time: Some(1_700_000_000),
                    instruction_name: "initialize".to_string(),
                    args: serde_json::json!({ "value": i }),
                    program_id: PROGRAM_ID.to_string(),
                    accounts: vec![],
                    instruction_index: 0,
                    inner_index: None,
                };
                writer
                    .write_block(SCHEMA_NAME, "backfill", &[ix], &[], i, None)
                    .await
                    .unwrap();
            }

            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}/stats")).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let d = &body["data"];
            assert!(d["first_seen_slot"].is_number());
            assert!(d["last_seen_slot"].is_number());
            assert!(d["instruction_counts"]["initialize"].is_number());
            assert_eq!(d["instruction_counts"]["initialize"], 3);
        })
        .await;
    }

    #[tokio::test]
    async fn empty_program_returns_zero_stats() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}/stats")).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            assert_eq!(body["data"]["total_instructions"], 0);
            assert_eq!(body["data"]["total_accounts"], 0);
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/accounts
// ---------------------------------------------------------------------------

mod list_account_types {
    use super::*;

    #[tokio::test]
    async fn happy_returns_data_account() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}/accounts")).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            let data = body["data"].as_array().unwrap();
            assert_eq!(data.len(), 1);
            assert_eq!(data[0], "DataAccount");
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_program_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) =
                oneshot_get(router, &format!("/api/programs/{PROGRAM_ID}/accounts")).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "PROGRAM_NOT_FOUND");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/accounts/{type}
// ---------------------------------------------------------------------------

mod query_accounts {
    use super::*;

    async fn seed_accounts(pool: sqlx::PgPool, count: usize) {
        let writer = StorageWriter::new(pool.clone());
        for i in 0..count {
            let pubkey = format!("AcctTest{i:036}");
            let acct = DecodedAccount {
                pubkey: pubkey.clone(),
                slot_updated: i as u64,
                lamports: 2_000_000,
                data: serde_json::json!({ "value": i as u64 }),
                account_type: "DataAccount".to_string(),
                program_id: PROGRAM_ID.to_string(),
            };
            writer
                .write_block(SCHEMA_NAME, "backfill", &[], &[acct], i as u64, None)
                .await
                .expect("write_block for account failed");
        }
    }

    #[tokio::test]
    async fn happy_returns_2_seeded_accounts() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool.clone()).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            seed_accounts(pool, 2).await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/accounts/DataAccount"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(body.get("data").is_some());
            assert!(body.get("pagination").is_some());
            let data = body["data"].as_array().unwrap();
            assert_eq!(data.len(), 2);
            assert_eq!(body["pagination"]["total"], 2);
            assert_eq!(body["pagination"]["offset"], 0);
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_account_type_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/accounts/NonExistentType"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "ACCOUNT_TYPE_NOT_FOUND");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /api/programs/{id}/accounts/{type}/{pubkey}
// ---------------------------------------------------------------------------

mod get_account {
    use super::*;

    const PUBKEY: &str = "AcctGetTest1111111111111111111111111111111";

    #[tokio::test]
    async fn happy_returns_single_account() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool.clone()).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let writer = StorageWriter::new(pool);
            let acct = DecodedAccount {
                pubkey: PUBKEY.to_string(),
                slot_updated: 42,
                lamports: 1_000_000,
                data: serde_json::json!({ "value": 99u64 }),
                account_type: "DataAccount".to_string(),
                program_id: PROGRAM_ID.to_string(),
            };
            writer
                .write_block(SCHEMA_NAME, "backfill", &[], &[acct], 42, None)
                .await
                .unwrap();

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/accounts/DataAccount/{PUBKEY}"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_success_envelope(&body);
            assert_eq!(body["data"]["pubkey"], PUBKEY);
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_pubkey_returns_404() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
            oneshot_post_json(
                router.clone(),
                "/api/programs",
                json!({ "program_id": PROGRAM_ID, "idl": idl }),
            )
            .await;

            let (status, _, body) = oneshot_get(
                router,
                &format!("/api/programs/{PROGRAM_ID}/accounts/DataAccount/unknownpubkey99"),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert_error_envelope(&body, "ACCOUNT_NOT_FOUND");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /health
// ---------------------------------------------------------------------------

mod health {
    use super::*;

    #[tokio::test]
    async fn returns_healthy_with_real_db() {
        with_postgres(|pool| async move {
            let router = build_test_router(pool).await;
            let (status, _, body) = oneshot_get(router, "/health").await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert_eq!(body["status"], "healthy");
            assert_eq!(body["database"], "connected");
            assert!(body["version"].is_string());
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// AC7: Cursor pagination invariant test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cursor_pagination_invariants() {
    with_postgres(|pool| async move {
        let router = build_test_router(pool.clone()).await;
        let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
        oneshot_post_json(
            router.clone(),
            "/api/programs",
            json!({ "program_id": PROGRAM_ID, "idl": idl }),
        )
        .await;

        // Seed exactly 1000 rows with unique (slot, signature) pairs
        let writer = StorageWriter::new(pool.clone());
        for i in 0..1000_u64 {
            let sig = format!("sig_{i:04}");
            let ix = DecodedInstruction {
                signature: sig.clone(),
                slot: i,
                block_time: Some(1_700_000_000),
                instruction_name: "initialize".to_string(),
                args: serde_json::json!({ "value": i }),
                program_id: PROGRAM_ID.to_string(),
                accounts: vec![],
                instruction_index: 0,
                inner_index: None,
            };
            writer
                .write_block(SCHEMA_NAME, "backfill", &[ix], &[], i, Some(&sig))
                .await
                .expect("write_block failed during cursor pagination seed");
        }

        // Walk all pages with limit=100
        let mut all_sigs: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut page_count = 0usize;
        let mut cursor: Option<String> = None;
        let mut prev_last_slot: Option<i64> = None;
        let mut prev_last_sig: Option<String> = None;

        loop {
            let url = match &cursor {
                None => format!("/api/programs/{PROGRAM_ID}/instructions/initialize?limit=100"),
                Some(c) => format!(
                    "/api/programs/{PROGRAM_ID}/instructions/initialize?limit=100&cursor={c}"
                ),
            };
            let (status, _, body) = oneshot_get(router.clone(), &url).await;
            assert_eq!(
                status,
                axum::http::StatusCode::OK,
                "page {page_count} returned non-200"
            );

            let data = body["data"].as_array().expect("data must be array");
            assert!(!data.is_empty(), "page {page_count} returned empty data");

            for row in data {
                let sig = row["signature"].as_str().expect("signature must be string");
                let inserted = all_sigs.insert(sig.to_string());
                assert!(inserted, "duplicate signature on page {page_count}: {sig}");

                // Assert descending order (slot DESC, signature DESC)
                let slot = row["slot"].as_i64().expect("slot must be i64");
                if let (Some(ps), Some(psg)) = (prev_last_slot, &prev_last_sig) {
                    let order_ok = slot < ps || (slot == ps && sig < psg.as_str());
                    assert!(
                        order_ok,
                        "ordering violation: prev ({ps}, {psg}) -> current ({slot}, {sig})"
                    );
                }
                prev_last_slot = Some(slot);
                prev_last_sig = Some(sig.to_string());
            }

            page_count += 1;

            let has_more = body["pagination"]["has_more"].as_bool().unwrap_or(false);
            if !has_more {
                break;
            }
            cursor = body["pagination"]["next_cursor"]
                .as_str()
                .map(ToString::to_string);
            assert!(
                cursor.is_some(),
                "has_more=true but next_cursor is null on page {page_count}"
            );
        }

        assert_eq!(page_count, 10, "expected 10 pages, got {page_count}");
        assert_eq!(
            all_sigs.len(),
            1000,
            "expected 1000 unique signatures, got {}",
            all_sigs.len()
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// AC8: Filter type coercion test — 400, not 500
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filter_type_coercion_returns_400_not_500() {
    with_postgres(|pool| async move {
        let router = build_test_router(pool.clone()).await;
        let idl: Value = serde_json::from_str(SIMPLE_IDL_JSON).unwrap();
        oneshot_post_json(
            router.clone(),
            "/api/programs",
            json!({ "program_id": PROGRAM_ID, "idl": idl }),
        )
        .await;

        // Seed at least one row so the SQL path is actually executed
        let writer = StorageWriter::new(pool.clone());
        let ix = DecodedInstruction {
            signature: "coerce_test_sig".to_string(),
            slot: 50,
            block_time: Some(1_700_000_000),
            instruction_name: "initialize".to_string(),
            args: serde_json::json!({ "value": 42u64 }),
            program_id: PROGRAM_ID.to_string(),
            accounts: vec![],
            instruction_index: 0,
            inner_index: None,
        };
        writer
            .write_block(SCHEMA_NAME, "backfill", &[ix], &[], 50, Some("coerce_test_sig"))
            .await
            .unwrap();

        // Case 1: non-numeric slot_gt → filtered out at builder level → INVALID_VALUE
        // (resolve_filters rejects non-integer strings for promoted BIGINT columns)
        let (status, _, body) = oneshot_get(
            router.clone(),
            &format!("/api/programs/{PROGRAM_ID}/instructions/initialize?slot_gt=not-a-number"),
        )
        .await;
        assert!(
            status == axum::http::StatusCode::BAD_REQUEST,
            "slot_gt=not-a-number should return 400, got {status}; body={body}"
        );
        // Can be INVALID_VALUE or INVALID_FILTER depending on where parser rejects
        assert!(
            body["error"]["code"] == "INVALID_VALUE" || body["error"]["code"] == "INVALID_FILTER",
            "expected INVALID_VALUE or INVALID_FILTER, got: {}",
            body["error"]["code"]
        );

        // Case 2: i64 overflow → rejected by builder-level parse
        let (status2, _, body2) = oneshot_get(
            router.clone(),
            &format!(
                "/api/programs/{PROGRAM_ID}/instructions/initialize?slot_gt=999999999999999999999999"
            ),
        )
        .await;
        assert!(
            status2 == axum::http::StatusCode::BAD_REQUEST,
            "i64 overflow should return 400, got {status2}; body={body2}"
        );

        // Case 3: unknown field → INVALID_FILTER with available_fields
        let (status3, _, body3) = oneshot_get(
            router.clone(),
            &format!("/api/programs/{PROGRAM_ID}/instructions/initialize?bogus_xyz=42"),
        )
        .await;
        assert_eq!(status3, axum::http::StatusCode::BAD_REQUEST);
        assert_filter_error_envelope(&body3, "value");

        // Verify: none of the above returned 500
        // (already enforced by the status assertions above)
    })
    .await;
}
