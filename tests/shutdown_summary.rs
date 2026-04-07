//! Story 6.1 AC6 — shutdown summary event integration test.
//!
//! **STUB — currently `#[ignore]`.** This test needs a real PostgreSQL
//! instance to exercise the `read_shutdown_totals` helper plus the full
//! shutdown sequence end-to-end. Story 6.5 will land the testcontainers
//! harness at `tests/common/postgres.rs` and un-ignore this test.
//!
//! The assertion contract this test will enforce when un-ignored:
//!
//! 1. Start a fresh Solarix process (or equivalent in-process sequence)
//!    with the testcontainers Postgres URL injected.
//! 2. Register a program, let the pipeline tick long enough to index a
//!    handful of instructions and accounts.
//! 3. Send SIGTERM (or trigger `CancellationToken::cancel()`).
//! 4. Capture the JSON log output emitted during the shutdown sequence.
//! 5. Assert the `shutdown summary` event carries every required field:
//!    - `uptime_secs`
//!    - `total_instructions_indexed`
//!    - `total_accounts_indexed`
//!    - `total_rpc_retries`
//!    - `total_decode_failures`
//!    - `final_pipeline_state`
//!    - `outcome` == `"clean"` for the happy path
//! 6. Assert the message string is literally `"shutdown summary"` — NOT
//!    `"shutdown complete"` (the legacy message Story 6.1 replaced).

#![allow(clippy::expect_used, clippy::panic)]

#[tokio::test]
#[ignore = "requires testcontainers harness (Story 6.5)"]
async fn shutdown_summary_event_contains_all_required_fields() {
    // TODO(6.5): wire `tests/common/postgres.rs` harness here and implement
    // the full flow described in the module doc.
    //
    // Skeleton sketch:
    //
    // ```ignore
    // mod common;
    // let pool = common::postgres::with_postgres().await;
    // // register a test program, start pipeline, let it tick, then cancel
    // let captured_logs = capture_shutdown_logs(/* ... */).await;
    // let summary_line = captured_logs
    //     .lines()
    //     .find(|l| l.contains(r#""message":"shutdown summary""#))
    //     .expect("shutdown summary event not emitted");
    //
    // let json: serde_json::Value =
    //     serde_json::from_str(summary_line).expect("valid JSON");
    // let fields = json.get("fields").expect("fields object missing");
    // assert!(fields.get("uptime_secs").is_some());
    // assert!(fields.get("total_instructions_indexed").is_some());
    // assert!(fields.get("total_accounts_indexed").is_some());
    // assert!(fields.get("total_rpc_retries").is_some());
    // assert!(fields.get("total_decode_failures").is_some());
    // assert!(fields.get("final_pipeline_state").is_some());
    // assert_eq!(
    //     fields.get("outcome").and_then(|v| v.as_str()),
    //     Some("clean"),
    // );
    // ```
    panic!("stub — un-ignore when Story 6.5 testcontainers harness lands");
}
