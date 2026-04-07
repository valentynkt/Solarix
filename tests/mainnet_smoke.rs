// Story 6.5 AC7: gated nightly mainnet smoke test.
//
// **Compile gate**: `#[cfg(feature = "mainnet-smoke")]`. The
// `mainnet-smoke` feature transitively enables `integration` (declared in
// Cargo.toml as `mainnet-smoke = ["integration"]`), so this file can
// reach `tests/common/postgres.rs::with_postgres` even though that helper
// is gated by `#[cfg(feature = "integration")]`. To compile this file
// locally you must pass either `--features mainnet-smoke` or
// `--features integration,mainnet-smoke` (the former is sufficient).
//
// **No `#[ignore]`**. The nightly workflow at `.github/workflows/nightly.yml:87`
// runs `cargo test --release --features mainnet-smoke -- mainnet_smoke`
// and does NOT pass `--ignored`. Adding `#[ignore]` here would silently
// turn the cron into a no-op forever. The cfg gate is the single gate.
//
// **Forward-looking contract**: Step 5 polls `SELECT COUNT(*) FROM
// "{schema}"."_instructions"` for up to 60 seconds and asserts the count
// is > 0. Today the indexer pipeline does NOT auto-start when a program
// is registered via `ProgramRegistry::commit_registration` — that's
// Story 6.11's job. So this test is **expected to fail nightly** until
// Story 6.11 lands. The nightly workflow runs the step with
// `continue-on-error: true` and posts a PR comment on failure
// (`nightly.yml:88-125`) precisely so this expected-failure surface is
// visible without going red on `main`. Once 6.11 lands, the polling
// branch starts seeing real rows and the test flips to green.

#![cfg(feature = "mainnet-smoke")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use solarix::idl::IdlManager;
use solarix::registry::ProgramRegistry;
use solarix::storage::schema::quote_ident;

mod common;
use common::known_programs::METEORA_DLMM_PROGRAM_ID;
use common::postgres::with_postgres;

const POLL_BUDGET: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn mainnet_smoke_registers_and_polls_for_instructions() {
    // Hard ceiling so a hung RPC cannot wedge the nightly job indefinitely.
    tokio::time::timeout(TEST_TIMEOUT, run_smoke())
        .await
        .expect("mainnet smoke must finish within 120s — RPC hung?");
}

async fn run_smoke() {
    // `SOLANA_RPC_URL` matches the env var name in `nightly.yml:86`. The
    // `MAINNET_SMOKE_` prefix on the program ID is fine because the workflow
    // does NOT export it — the default below is what runs nightly. A
    // developer running the test locally can override either via env.
    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());
    let program_id = std::env::var("MAINNET_SMOKE_PROGRAM_ID")
        .unwrap_or_else(|_| METEORA_DLMM_PROGRAM_ID.to_string());

    eprintln!("[mainnet_smoke] rpc_url   = {rpc_url}");
    eprintln!("[mainnet_smoke] program_id = {program_id}");

    with_postgres(move |pool| {
        let rpc_url = rpc_url.clone();
        let program_id = program_id.clone();
        async move {
            // 1. Build an IdlManager pointing at the chosen RPC and pre-fetch
            //    the IDL into the cache. `prepare_registration` with
            //    `idl_json = None` requires the IDL to already be cached
            //    (so the registry write-lock isn't held across an await).
            let mut idl_manager = IdlManager::new(rpc_url.clone());
            idl_manager
                .get_idl(&program_id)
                .await
                .expect("auto-fetch cascade (on-chain PDA → bundled) must yield an IDL");

            // 2. Register the program. With the IDL already cached the
            //    `idl_json = None` path takes the auto-fetch branch and
            //    succeeds immediately.
            let mut registry = ProgramRegistry::new(idl_manager);
            let data = registry
                .prepare_registration(program_id.clone(), None)
                .expect("prepare_registration with cached IDL should succeed");
            let schema_name = data.schema_name.clone();
            let info = ProgramRegistry::commit_registration(pool.clone(), data)
                .await
                .expect("commit_registration should succeed against the mainnet IDL");
            assert_eq!(info.status, "schema_created");
            eprintln!("[mainnet_smoke] schema = {schema_name}");

            // 3. Poll _instructions for up to 60 seconds. See the file-level
            //    forward-looking-contract note: this branch will fail
            //    nightly until Story 6.11 wires pipeline auto-start. The
            //    nightly workflow's `continue-on-error: true` posts a PR
            //    comment on failure rather than turning the cron red.
            let count_sql = format!(
                r#"SELECT COUNT(*) FROM {}.{}"#,
                quote_ident(&schema_name),
                quote_ident("_instructions"),
            );

            let started = std::time::Instant::now();
            let mut last_count: i64 = 0;
            while started.elapsed() < POLL_BUDGET {
                let row: (i64,) = sqlx::query_as(&count_sql)
                    .fetch_one(&pool)
                    .await
                    .expect("instructions count query should succeed");
                last_count = row.0;
                if last_count > 0 {
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }

            eprintln!(
                "[mainnet_smoke] _instructions count after {:?}: {last_count}",
                started.elapsed()
            );
            assert!(
                last_count > 0,
                "expected at least one decoded instruction within {POLL_BUDGET:?} — \
                 EXPECTED to fail nightly until Story 6.11 (pipeline auto-start) lands"
            );
        }
    })
    .await;
}
