use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

async fn connect_test_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://solarix:solarix@localhost:5432/solarix".to_string());
    PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect to test database")
}

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn bootstrap_is_idempotent() {
    let pool = connect_test_pool().await;

    // First call succeeds
    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .expect("first bootstrap call failed");
    // Second call also succeeds (idempotent)
    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .expect("second bootstrap call failed");

    // Verify tables exist via information_schema
    let row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name IN ('programs', 'indexer_state')",
    )
    .fetch_one(&pool)
    .await
    .expect("information_schema query failed");

    let count: i64 = row.get("cnt");
    assert_eq!(count, 2, "both system tables should exist");
}

/// Story 6.4 AC8 + Story 4.4 regression pin.
///
/// The Story 4.4 persistence path added an `idl_json` column via
/// `ALTER TABLE "programs" ADD COLUMN IF NOT EXISTS "idl_json" TEXT`. This
/// test asserts that running `bootstrap_system_tables` against an
/// older-shaped `programs` table (without `idl_json`) transparently upgrades
/// it without data loss. If someone ever removes the `ALTER TABLE` statement
/// from `bootstrap_system_tables`, this test catches the regression on the
/// next CI run.
///
/// Matches the existing-test convention: `#[ignore]` + `DATABASE_URL` env.
#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn bootstrap_creates_idl_json_column_on_upgrade() {
    let pool = connect_test_pool().await;

    // 1. Drop `programs` to simulate a totally fresh DB, then rebuild the
    //    OLD-shape table (no idl_json column). Use DROP CASCADE so any
    //    indexer_state rows that FK to programs get wiped too.
    sqlx::raw_sql("DROP TABLE IF EXISTS \"indexer_state\" CASCADE;")
        .execute(&pool)
        .await
        .expect("drop indexer_state");
    sqlx::raw_sql("DROP TABLE IF EXISTS \"programs\" CASCADE;")
        .execute(&pool)
        .await
        .expect("drop programs");

    // Recreate the OLD shape (missing idl_json)
    sqlx::raw_sql(
        r#"CREATE TABLE "programs" (
            "program_id"   VARCHAR(44) PRIMARY KEY,
            "program_name" TEXT NOT NULL,
            "schema_name"  TEXT NOT NULL UNIQUE,
            "idl_hash"     VARCHAR(64),
            "idl_source"   TEXT,
            "status"       TEXT NOT NULL DEFAULT 'initializing',
            "created_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            "updated_at"   TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );"#,
    )
    .execute(&pool)
    .await
    .expect("recreate old-shape programs table");

    // Sanity: `idl_json` must NOT exist yet.
    let pre_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns \
         WHERE table_schema = 'public' \
           AND table_name = 'programs' \
           AND column_name = 'idl_json'",
    )
    .fetch_one(&pool)
    .await
    .expect("pre-check information_schema query");
    assert_eq!(
        pre_count, 0,
        "old-shape table must not already have idl_json"
    );

    // 2. Run bootstrap. The ALTER TABLE ADD COLUMN IF NOT EXISTS clause
    //    inside bootstrap_system_tables should upgrade the table in place.
    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .expect("bootstrap must succeed on older-shape programs table");

    // 3. Verify idl_json now exists, with the expected type.
    let row = sqlx::query(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_schema = 'public' \
           AND table_name = 'programs' \
           AND column_name = 'idl_json'",
    )
    .fetch_optional(&pool)
    .await
    .expect("post-check information_schema query");

    let row = row.expect("idl_json column must exist after bootstrap upgrade");
    let data_type: String = row.get("data_type");
    assert_eq!(data_type, "text", "idl_json must be TEXT (got {data_type})");

    // 4. And a second bootstrap call is still a no-op (idempotency carries).
    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .expect("repeat bootstrap must still succeed");
}
