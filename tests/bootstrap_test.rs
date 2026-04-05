use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[tokio::test]
#[ignore] // requires running PostgreSQL
async fn bootstrap_is_idempotent() {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://solarix:solarix@localhost:5432/solarix".to_string());
    let pool = PgPoolOptions::new()
        .connect(&url)
        .await
        .expect("failed to connect to test database");

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
