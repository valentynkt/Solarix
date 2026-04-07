// Async testcontainers harness for Solarix integration tests (Story 6.5).
//
// Canonical entry point: `with_postgres(|pool| async { ... }).await`.
//
// Each call spawns a fresh `postgres:16-alpine` container via the
// testcontainers-modules `AsyncRunner`, builds an `sqlx::PgPool` against the
// mapped port, calls `solarix::storage::bootstrap_system_tables(&pool)` so the
// closure starts from a clean, bootstrapped DB, then awaits the closure.
//
// We deliberately do **not** reuse `solarix::storage::init_pool` because it
// reads from the runtime `Config` struct, which is irrelevant for tests; the
// harness builds its own `PgPoolOptions` directly so the test surface stays
// independent of the production startup path.
//
// Per-test container model: every call to `with_postgres` spawns its own
// container. See "Per-test container vs shared container" in story 6.5's
// Dev Notes for the trade-off (chosen for total isolation + parallelism).
//
// `with_postgres_returning<T>` is the variant that lets a test return a value
// out of the closure for assertion *after* the container has been dropped.
//
// SyncRunner is **not** used because Solarix is fully async tokio + sqlx and
// the blocking runner would deadlock the executor.

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::future::Future;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::ImageExt;

const POSTGRES_TAG: &str = "16-alpine";
const POSTGRES_PORT: u16 = 5432;

/// Spawn a fresh `postgres:16-alpine` container, bootstrap system tables,
/// hand the pool to `f`, and tear everything down on return or panic.
///
/// The closure receives an **owned** `PgPool` so the future it produces is
/// `'static + Send`. The container handle is held alive for the entire
/// lifetime of the closure (testcontainers-rs cleans up on `Drop`).
///
/// `Output = ()` is intentional — see the "Closure ergonomics" section of
/// story 6.5's Dev Notes for why `.unwrap()` inside the closure is the
/// canonical pattern instead of bubbling `Result`s.
pub async fn with_postgres<F, Fut>(f: F)
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (container, pool) = start_container_and_pool().await;

    f(pool).await;

    // Explicit drop so the reader can see the lifetime contract.
    drop(container);
}

/// Same as [`with_postgres`] but lets the closure compute and return a value
/// out of the container's lifetime. Useful for tests that want to assert on
/// a row count *after* the container has gone away.
pub async fn with_postgres_returning<F, Fut, T>(f: F) -> T
where
    F: FnOnce(PgPool) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (container, pool) = start_container_and_pool().await;

    let result = f(pool).await;

    drop(container);
    result
}

async fn start_container_and_pool() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    PgPool,
) {
    let container = Postgres::default()
        .with_tag(POSTGRES_TAG)
        .start()
        .await
        .expect("failed to start postgres testcontainer (is docker running?)");

    let host = container
        .get_host()
        .await
        .expect("failed to read container host");
    let port = container
        .get_host_port_ipv4(POSTGRES_PORT)
        .await
        .expect("failed to read container port");

    // testcontainers-modules `Postgres::default()` ships with the canonical
    // `postgres/postgres/postgres` credentials. These differ from the
    // Solarix compose default but are fine — the harness only cares about
    // *some* working pool against the container.
    let conn_string = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&conn_string)
        .await
        .expect("failed to connect to postgres testcontainer");

    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .expect("failed to bootstrap system tables in postgres testcontainer");

    (container, pool)
}
