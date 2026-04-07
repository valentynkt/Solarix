// Schema generator integration stub for Story 6.4 (AC7 complementary).
//
// TODO(story-6.5): replace `setup_pool` stub below with `with_postgres(|pool| …)`
//                  from `tests/common/postgres.rs` once the testcontainers
//                  harness lands. This test currently #[ignore]s itself; do
//                  not enable until 6.5 merges.
//
// When the testcontainers harness ships, this file will:
//
//   1. Spawn a Postgres 16 container.
//   2. Call `bootstrap_system_tables(&pool)`.
//   3. For the bundled `tests/fixtures/idls/simple_v030.json` fixture, call
//      `generate_schema(pool, idl, "prog", "test_schema").await`.
//   4. Introspect `information_schema.columns` for the generated tables and
//      assert that column types match the IDL field types:
//          IdlType::U64    → data_type = 'bigint'
//          IdlType::Pubkey → data_type = 'text'
//          IdlType::Bool   → data_type = 'boolean'
//          JSONB 'data'    → data_type = 'jsonb'
//   5. Run `bootstrap_system_tables` + `generate_schema` a second time and
//      assert that the second call is a no-op (idempotency pin).
//   6. Cover `all_types.json` similarly to exercise every promoted column
//      type mapping end-to-end.
//
// This file is a scaffold; the real assertions land with Story 6.5. Once
// the testcontainer harness is in place, lift the `#[ignore]` and wire the
// body.

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn setup_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://solarix:solarix@localhost:5432/solarix".to_string());
    PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect — Story 6.5 will swap this for a testcontainer harness")
}

#[tokio::test]
#[ignore = "requires Story 6.5 testcontainers harness"]
async fn schema_generator_produces_expected_columns_in_real_pg() {
    let _pool = setup_pool().await;
    // TODO(story-6.5): see file-level comment for the full assertion plan.
    eprintln!("schema_against_pg stub — Story 6.5 will flesh this out");
}

#[tokio::test]
#[ignore = "requires Story 6.5 testcontainers harness"]
async fn schema_generator_is_idempotent_in_real_pg() {
    let _pool = setup_pool().await;
    // TODO(story-6.5): call bootstrap + generate_schema twice, assert no-op.
    eprintln!("schema_against_pg idempotency stub — Story 6.5 will flesh this out");
}
