use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use solarix::config::Config;
use solarix::idl::IdlManager;
use solarix::pipeline::update_indexer_state;
use solarix::registry::ProgramRegistry;
use solarix::runtime_stats::RuntimeStats;
use solarix::storage::StorageError;

/// Top-level error type so `main` can return a `Result` and let the runtime
/// produce the right exit code (0 = clean, 1 = any error path). Spec forbids
/// `std::process::exit` (story 4.3 anti-patterns).
#[derive(Debug, thiserror::Error)]
enum SolarixError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pipeline: {0}")]
    Pipeline(#[from] solarix::pipeline::PipelineError),
    #[error("API server: {0}")]
    ApiServer(String),
    #[error("pipeline task panicked: {0}")]
    PipelineJoin(String),
    #[error("API task panicked: {0}")]
    ApiJoin(String),
}

#[tokio::main]
async fn main() -> Result<(), SolarixError> {
    dotenvy::dotenv().ok();

    let config = Config::parse();

    // Initialize tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| config.log_level.clone().into());

    if config.log_format.eq_ignore_ascii_case("json") {
        // Story 6.1 AC8: span context on close, immediate parent only
        // (no full ancestry → bounded log size), FmtSpan::CLOSE so each
        // instrumented function emits exactly one span-close event with
        // its fields + `err(Display)` payload. `with_current_span` /
        // `with_span_list` are JSON-formatter-specific, so they must come
        // after `.json()` in the builder chain.
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    let start_time = Instant::now();

    info!(
        rpc_url = %config.rpc_url,
        api_host = %config.api_host,
        api_port = config.api_port,
        "solarix starting"
    );

    // Process-wide counters for the shutdown summary event (Story 6.1 AC6)
    // and Story 6.2's Prometheus `/metrics` handler. Constructed once and
    // cloned into every component that increments counters.
    let stats = Arc::new(RuntimeStats::new());

    // Shared cancellation token — drives all graceful shutdown
    let cancel = CancellationToken::new();

    // Wire signal handler to cancel the token
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        cancel_signal.cancel();
    });

    info!("connecting to database");
    let pool = solarix::storage::init_pool(&config).await.map_err(|e| {
        error!(error = %e, "failed to initialize database pool");
        e
    })?;

    info!("bootstrapping system tables");
    solarix::storage::bootstrap_system_tables(&pool)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to bootstrap system tables");
            e
        })?;

    info!("database ready");

    let idl_manager = IdlManager::new(config.rpc_url.clone());
    let registry = ProgramRegistry::new(idl_manager);
    let registry = Arc::new(RwLock::new(registry));

    // Check for registered programs to auto-start pipeline. A DB error here
    // is fatal — we MUST NOT silently degrade into "API-only" mode if the
    // programs table is unreachable; supervisors would never see the failure
    // and indexing would silently die. (Story 4.3 review patch P8.)
    let programs = query_registered_programs(&pool).await?;

    // Seed the registry's IDL cache with persisted IDLs so API queries work.
    // This is the IDL persistence path tracked under story 4.4.
    //
    // If seeding fails for a program, drop it from the auto-start list AND
    // mark `programs.status = 'error'` (with the failure message stored in
    // `indexer_state.error_message`) so the API stays consistent — operators
    // see status=error instead of a 200 from /api/programs/{id} alongside
    // 404s from /api/programs/{id}/instructions/{name}. (Story 4.4 Task 6,
    // refined P15.)
    let seed_outcomes: Vec<bool> = {
        let mut reg = registry.write().await;
        let mut outcomes = Vec::with_capacity(programs.len());
        for p in &programs {
            match reg.idl_manager.insert_fetched_idl(
                &p.program_id,
                &p.idl_json,
                solarix::idl::IdlSource::OnChain,
            ) {
                Ok(_) => outcomes.push(true),
                Err(e) => {
                    warn!(program_id = %p.program_id, error = %e, "failed to seed registry cache; marking status=error");
                    outcomes.push(false);
                }
            }
        }
        outcomes
    };

    // Mark seeding failures as `status = 'error'` in the DB **after** the
    // write lock is released, so the registry isn't held across an await.
    let mut programs_to_start: Vec<StartupProgram> = Vec::with_capacity(programs.len());
    for (program, ok) in programs.into_iter().zip(seed_outcomes.iter().copied()) {
        if ok {
            programs_to_start.push(program);
        } else if let Err(e) = ProgramRegistry::mark_program_error(
            pool.clone(),
            program.program_id.clone(),
            "registry IDL cache seeding failed at startup".to_string(),
        )
        .await
        {
            warn!(
                program_id = %program.program_id,
                error = %e,
                "failed to persist status=error after cache seeding failure"
            );
        }
    }
    let programs = programs_to_start;

    // If a signal arrived during init, exit before spawning anything heavy.
    if cancel.is_cancelled() {
        info!("cancellation observed before startup; exiting");
        graceful_shutdown(&pool, &[], &config).await;
        return Ok(());
    }

    let addr = format!("{}:{}", config.api_host, config.api_port);

    let state = Arc::new(solarix::api::AppState {
        pool: pool.clone(),
        start_time,
        registry,
        config: config.clone(),
        stats: Arc::clone(&stats),
    });
    let app = solarix::api::router(state);
    let listener = TcpListener::bind(&addr).await.map_err(|e| {
        error!(error = %e, addr = %addr, "failed to bind listener");
        e
    })?;

    info!(addr = %addr, "listening");

    // Track which programs were actually started by this process so the
    // shutdown phase only resets their indexer_state row. (Patch P14.)
    let mut started_programs: Vec<String> = Vec::new();

    let run_result: Result<(), SolarixError> = if programs.is_empty() {
        info!("no registered programs with IDL, running API server only");
        info!("register a program via POST /api/programs to start indexing");

        let api_cancel = cancel.clone();
        match axum::serve(listener, app)
            .with_graceful_shutdown(api_cancel.cancelled_owned())
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                error!(error = %e, "server error");
                Err(SolarixError::ApiServer(e.to_string()))
            }
        }
    } else {
        // Pipeline mode: run API + pipeline concurrently.
        // Multi-program is tracked under deferred-work; we deterministically pick
        // the lexicographically-first program (the SELECT already orders by
        // program_id) so restarts are reproducible. (Patch P13.)
        if programs.len() > 1 {
            warn!(
                count = programs.len(),
                first_program = %programs[0].program_id,
                "multiple programs registered; only the first will be indexed in this version"
            );
        }
        let program = &programs[0];
        started_programs.push(program.program_id.clone());
        info!(
            program_id = %program.program_id,
            schema_name = %program.schema_name,
            "starting pipeline for registered program"
        );

        let api_cancel = cancel.clone();
        let api_handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(api_cancel.cancelled_owned())
                .await
        });

        let pipeline_cancel = cancel.clone();
        let pipeline_pool = pool.clone();
        let pipeline_config = config.clone();
        let pipeline_program_id = program.program_id.clone();
        let pipeline_schema_name = program.schema_name.clone();
        let pipeline_idl = program.idl.clone();
        let pipeline_stats = Arc::clone(&stats);

        let pipeline_handle = tokio::spawn(async move {
            use solarix::decoder::ChainparserDecoder;
            use solarix::pipeline::rpc::RpcClient;
            use solarix::pipeline::PipelineOrchestrator;
            use solarix::storage::writer::StorageWriter;

            let rpc = RpcClient::new(&pipeline_config, Arc::clone(&pipeline_stats))?;
            let decoder: Arc<dyn solarix::decoder::SolarixDecoder> =
                Arc::new(ChainparserDecoder::new());
            let writer = StorageWriter::new(pipeline_pool.clone());

            let orch = PipelineOrchestrator::new(
                pipeline_pool,
                rpc,
                decoder,
                writer,
                pipeline_config,
                pipeline_cancel,
                pipeline_stats,
            );

            orch.run(&pipeline_program_id, &pipeline_schema_name, &pipeline_idl)
                .await
        });

        // Coordinated shutdown: whichever finishes first signals the other via
        // the cancel token, then we await BOTH handles (with a drain timeout).
        // Just dropping `select!`'s loser would detach its task and let
        // `pool.close()` race in-flight queries. (Patch P6.)
        let mut pipeline_handle = pipeline_handle;
        let mut api_handle = api_handle;

        let initial_outcome: Result<(), SolarixError> = tokio::select! {
            result = &mut pipeline_handle => {
                cancel.cancel();
                match result {
                    Ok(Ok(())) => {
                        info!("pipeline exited cleanly");
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        error!(error = %e, "pipeline error");
                        Err(SolarixError::Pipeline(e))
                    }
                    Err(e) => {
                        error!(error = %e, "pipeline task panicked");
                        Err(SolarixError::PipelineJoin(e.to_string()))
                    }
                }
            }
            result = &mut api_handle => {
                cancel.cancel();
                match result {
                    Ok(Ok(())) => {
                        info!("API server exited");
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        error!(error = %e, "API server error");
                        Err(SolarixError::ApiServer(e.to_string()))
                    }
                    Err(e) => {
                        error!(error = %e, "API server task panicked");
                        Err(SolarixError::ApiJoin(e.to_string()))
                    }
                }
            }
        };

        // Drain phase: bound the wait so a stuck handler can't hang shutdown
        // forever. (Patch P2.)
        let drain = Duration::from_secs(config.shutdown_drain_secs);
        let drain_outcome = tokio::time::timeout(drain, async {
            let api_res = (&mut api_handle).await;
            let pipe_res = (&mut pipeline_handle).await;
            (api_res, pipe_res)
        })
        .await;

        match drain_outcome {
            Ok((api_res, pipe_res)) => {
                if let Err(e) = api_res {
                    if !e.is_cancelled() {
                        warn!(error = %e, "API task drain returned error");
                    }
                }
                if let Err(e) = pipe_res {
                    if !e.is_cancelled() {
                        warn!(error = %e, "pipeline task drain returned error");
                    }
                }
            }
            Err(_) => {
                warn!(
                    drain_secs = config.shutdown_drain_secs,
                    "shutdown drain timed out; aborting remaining tasks"
                );
                api_handle.abort();
                pipeline_handle.abort();
            }
        }

        initial_outcome
    };

    // Snapshot totals BEFORE `graceful_shutdown` closes the pool — the
    // helper runs after we've already flushed `indexer_state.status = stopped`
    // so the `total_instructions` / `total_accounts` columns are final.
    // The helper degrades to zeroes on DB errors so the summary always emits.
    let (total_instructions_indexed, total_accounts_indexed, final_pipeline_state) =
        read_shutdown_totals(&pool, &started_programs, &config).await;
    let total_rpc_retries = stats.rpc_retries();
    let total_decode_failures = stats.decode_failures();

    // Graceful shutdown: final indexer_state UPDATEs (with proper
    // last_processed_slot via update_indexer_state), then pool.close().
    graceful_shutdown(&pool, &started_programs, &config).await;

    let outcome = if run_result.is_ok() { "clean" } else { "error" };
    info!(
        uptime_secs = start_time.elapsed().as_secs(),
        total_instructions_indexed,
        total_accounts_indexed,
        total_rpc_retries,
        total_decode_failures,
        final_pipeline_state = %final_pipeline_state,
        outcome,
        "shutdown summary"
    );

    run_result
}

/// Read shutdown totals across started programs. On any DB error or
/// timeout, logs a `warn!` and returns zeroes — the shutdown summary
/// event MUST always emit. Story 6.1 AC6.
async fn read_shutdown_totals(
    pool: &PgPool,
    started_programs: &[String],
    config: &Config,
) -> (u64, u64, String) {
    if started_programs.is_empty() {
        return (0, 0, "unknown".to_string());
    }

    let sql = r#"SELECT
        COALESCE(SUM("total_instructions"), 0)::BIGINT AS total_instructions,
        COALESCE(SUM("total_accounts"), 0)::BIGINT AS total_accounts,
        COALESCE(MAX("status"), 'unknown') AS final_status
        FROM "indexer_state"
        WHERE "program_id" = ANY($1::TEXT[])"#;

    let fut = sqlx::query_as::<_, (i64, i64, String)>(sql)
        .bind(started_programs)
        .fetch_one(pool);

    match tokio::time::timeout(Duration::from_secs(config.shutdown_db_flush_secs), fut).await {
        Ok(Ok((ti, ta, status))) => {
            let ti_u = if ti < 0 { 0u64 } else { ti as u64 };
            let ta_u = if ta < 0 { 0u64 } else { ta as u64 };
            (ti_u, ta_u, status)
        }
        Ok(Err(e)) => {
            warn!(error = %e, "failed to read shutdown totals from indexer_state");
            (0, 0, "unknown".to_string())
        }
        Err(_) => {
            warn!(
                drain_secs = config.shutdown_db_flush_secs,
                "timed out reading shutdown totals from indexer_state"
            );
            (0, 0, "unknown".to_string())
        }
    }
}

/// Registered program info loaded from DB at startup.
struct StartupProgram {
    program_id: String,
    schema_name: String,
    idl: anchor_lang_idl_spec::Idl,
    /// Raw IDL JSON bytes as stored in `programs.idl_json`. Carried through
    /// to the cache seeding step so the in-memory cache holds the same bytes
    /// the hash was computed from. Story 4.4 AC5.
    idl_json: String,
}

/// Query the programs table for programs with persisted IDL JSON.
///
/// Returns programs with `status = 'schema_created'` and a non-null `idl_json` column,
/// parsing the IDL JSON into the `Idl` type for pipeline use.
///
/// A DB error is propagated as `StorageError::QueryFailed` so the supervisor
/// sees a non-zero exit instead of a silent "no programs" startup.
async fn query_registered_programs(pool: &PgPool) -> Result<Vec<StartupProgram>, StorageError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
        r#"SELECT "program_id", "schema_name", "idl_json" FROM "programs"
           WHERE "status" = 'schema_created'
           ORDER BY "program_id" ASC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| {
        error!(error = %e, "failed to query programs table");
        StorageError::QueryFailed(format!("programs lookup failed: {e}"))
    })?;

    let row_count = rows.len();
    if row_count == 0 {
        return Ok(Vec::new());
    }

    info!(count = row_count, "found registered program rows in DB");

    let mut programs = Vec::new();
    for (program_id, schema_name, idl_json) in rows {
        let Some(json) = idl_json else {
            warn!(program_id = %program_id, "program has no persisted idl_json, skipping pipeline auto-start");
            continue;
        };
        match serde_json::from_str::<anchor_lang_idl_spec::Idl>(&json) {
            Ok(idl) => {
                info!(program_id = %program_id, schema_name = %schema_name, "loaded persisted IDL");
                programs.push(StartupProgram {
                    program_id,
                    schema_name,
                    idl,
                    idl_json: json,
                });
            }
            Err(e) => {
                warn!(program_id = %program_id, error = %e, "failed to parse persisted IDL JSON");
            }
        }
    }

    if programs.is_empty() && row_count > 0 {
        error!(
            row_count,
            "all registered programs failed to load IDL JSON; pipeline will not auto-start"
        );
    } else {
        info!(
            loaded = programs.len(),
            row_count, "loaded persisted IDLs for pipeline auto-start"
        );
    }

    Ok(programs)
}

/// Final shutdown sequence: update indexer_state and close pool.
///
/// Only updates programs that this process actually started (`started_programs`),
/// not every row in the DB. Updates run concurrently with one global timeout
/// so N slow programs don't multiply the shutdown grace period. (Patch P14, P20.)
async fn graceful_shutdown(pool: &PgPool, started_programs: &[String], config: &Config) {
    if started_programs.is_empty() {
        pool.close().await;
        return;
    }

    let timeout_dur = Duration::from_secs(config.shutdown_db_flush_secs);

    let updates = started_programs.iter().map(|program_id| {
        let pool = pool.clone();
        let program_id = program_id.clone();
        async move {
            // Carry the writer's authoritative slot into the indexer_state row.
            // We read the highest known checkpoint at shutdown time so the row
            // reflects what's actually durable. (Patch P3.)
            let last_slot = read_max_checkpoint_slot(&pool, &program_id).await;
            let res = update_indexer_state(&pool, &program_id, "stopped", last_slot).await;
            (program_id, last_slot, res)
        }
    });

    match tokio::time::timeout(timeout_dur, futures_util::future::join_all(updates)).await {
        Ok(results) => {
            for (program_id, last_slot, res) in results {
                match res {
                    Ok(()) => info!(
                        program_id = %program_id,
                        last_processed_slot = last_slot.unwrap_or(0),
                        "indexer_state set to stopped"
                    ),
                    Err(e) => warn!(
                        program_id = %program_id,
                        error = %e,
                        "failed to update indexer_state on shutdown"
                    ),
                }
            }
        }
        Err(_) => {
            warn!(
                drain_secs = config.shutdown_db_flush_secs,
                programs = started_programs.len(),
                "indexer_state shutdown updates timed out"
            );
        }
    }

    pool.close().await;
}

/// Read the highest known checkpoint slot across all streams for a program.
///
/// Used at shutdown to record the durable cursor in `indexer_state.last_processed_slot`.
/// Looks up the schema name from the `programs` table, then takes the max across all
/// `_checkpoints` rows. Returns `None` on any error or when no checkpoint exists.
async fn read_max_checkpoint_slot(pool: &PgPool, program_id: &str) -> Option<u64> {
    let schema_name: Option<String> = match sqlx::query_scalar::<_, String>(
        r#"SELECT "schema_name" FROM "programs" WHERE "program_id" = $1"#,
    )
    .bind(program_id)
    .fetch_optional(pool)
    .await
    {
        Ok(s) => s,
        Err(e) => {
            warn!(program_id, error = %e, "failed to look up schema_name for shutdown checkpoint read");
            return None;
        }
    };

    let schema = schema_name?;
    let sql = format!(r#"SELECT MAX("last_slot") FROM "{schema}"."_checkpoints""#);
    match sqlx::query_scalar::<_, Option<i64>>(&sql)
        .fetch_one(pool)
        .await
    {
        Ok(Some(s)) if s >= 0 => Some(s as u64),
        Ok(_) => None,
        Err(e) => {
            warn!(program_id, schema, error = %e, "failed to read max checkpoint at shutdown");
            None
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    // NOTE: `pipeline.state.from` here is approximate — at the signal site
    // the orchestrator's current state (backfilling / catching_up / streaming)
    // is not accessible. Emitting "running" is a conscious placeholder so log
    // scrapers always see a transition event bracketing shutdown. Story 6.1 AC2.
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| tracing::warn!(error = %e, "failed to install SIGTERM handler"))
            .ok();

        tokio::select! {
            _ = ctrl_c => {
                info!(
                    pipeline.state.from = "running",
                    pipeline.state.to = "shutting_down",
                    last_processed_slot = 0u64,
                    reason = "sigint",
                    "pipeline state transition"
                );
            },
            _ = async {
                if let Some(ref mut s) = sigterm {
                    s.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                info!(
                    pipeline.state.from = "running",
                    pipeline.state.to = "shutting_down",
                    last_processed_slot = 0u64,
                    reason = "sigterm",
                    "pipeline state transition"
                );
            },
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        info!(
            pipeline.state.from = "running",
            pipeline.state.to = "shutting_down",
            last_processed_slot = 0u64,
            reason = "sigint",
            "pipeline state transition"
        );
    }
}
