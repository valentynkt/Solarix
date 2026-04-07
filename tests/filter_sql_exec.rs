// Filter execution integration stub for Story 6.4 (AC7).
//
// TODO(story-6.5): replace `setup_pool` stub below with `with_postgres(|pool| …)`
//                  from `tests/common/postgres.rs` once the testcontainers
//                  harness lands. This test currently #[ignore]s itself; do
//                  not enable until 6.5 merges.
//
// AC7 scope: when the testcontainers harness ships, this file will:
//
//   1. Spawn a Postgres 16 container via `testcontainers`.
//   2. Call `bootstrap_system_tables(&pool)` so the `programs` / `indexer_state`
//      system tables exist.
//   3. Generate the schema for `tests/fixtures/idls/simple_v030.json` via
//      `generate_schema(pool, idl, "prog", "test_schema").await`.
//   4. Seed 10 rows with every promoted-column type (BIGINT, SMALLINT, TEXT,
//      BOOLEAN) plus JSONB field content that covers every `FilterOp` case
//      from `tests/filter_builder_contracts.rs`.
//   5. For each (op × column type) combination, build the query via
//      `build_query(&target, &resolved, 50, 0).build()` and execute it with
//      `sqlx::query_with`, asserting the rowset matches the expected set.
//   6. Include a specific regression case for the Sprint-4 `bigint > text`
//      bug: `slot_gt=100` must return only rows with slot > 100 and must
//      NOT panic / 500 with `operator does not exist`.
//
// This scaffold is deliberately minimal so the first 6.5 patch that wires
// the harness gets an immediate green signal: the function body compiles,
// has the right signature, and the `#[ignore]` attribute is the only thing
// keeping it out of `cargo test --tests` by default.
//
// To run manually after 6.5 lands:
//
//   cargo test --test filter_sql_exec -- --ignored
//
// Or via the `integration` feature gate (Story 6.7 CI wiring):
//
//   cargo test --features integration --test filter_sql_exec -- --ignored

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

// ---------------------------------------------------------------------------
// Stub scaffolding — replaced by tests/common/postgres.rs in Story 6.5
// ---------------------------------------------------------------------------

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Stubbed pool setup. Story 6.5 will replace this with a testcontainers-backed
/// `with_postgres(|pool| async { ... })` helper from `tests/common/postgres.rs`.
/// Until then we fall back to a `DATABASE_URL` read so the `#[ignore]`'d test
/// can still be executed manually against a developer's local PG if desired.
async fn setup_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://solarix:solarix@localhost:5432/solarix".to_string());
    PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect — Story 6.5 will swap this for a testcontainer harness")
}

// ---------------------------------------------------------------------------
// AC7: filter execution matrix against a real PostgreSQL
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Story 6.5 testcontainers harness"]
async fn filter_sql_exec_matrix_against_real_postgres() {
    let _pool = setup_pool().await;

    // TODO(story-6.5): the rest of AC7's requirements — bootstrap, schema
    // generation, row seeding, matrix execution — land here once the
    // testcontainers harness is wired up. Keeping this body minimal so a
    // future patch can extend it without first fighting the scaffolding.
    //
    // Until then this test is behind `#[ignore]` and `#[cfg(feature = "integration")]`
    // so `cargo test --tests` on a developer machine is not affected.

    eprintln!("filter_sql_exec stub — Story 6.5 will flesh this out");
}
