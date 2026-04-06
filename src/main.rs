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
use solarix::registry::ProgramRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let config = Config::parse();

    // Initialize tracing
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| config.log_level.clone().into());

    if config.log_format.eq_ignore_ascii_case("json") {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
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

    let addr = format!("{}:{}", config.api_host, config.api_port);

    let state = Arc::new(solarix::api::AppState {
        pool: pool.clone(),
        start_time,
        registry,
        config: config.clone(),
    });
    let app = solarix::api::router(state);
    let listener = TcpListener::bind(&addr).await.map_err(|e| {
        error!(error = %e, addr = %addr, "failed to bind listener");
        e
    })?;

    info!(addr = %addr, "listening");

    // Check for registered programs to auto-start pipeline
    let programs = query_registered_programs(&pool).await;

    let exit_code = if programs.is_empty() {
        info!("no registered programs with IDL, running API server only");
        info!("register a program via POST /api/programs to start indexing");

        // API-only mode: run until cancelled
        let api_cancel = cancel.clone();
        match axum::serve(listener, app)
            .with_graceful_shutdown(api_cancel.cancelled_owned())
            .await
        {
            Ok(()) => 0,
            Err(e) => {
                error!(error = %e, "server error");
                1
            }
        }
    } else {
        // Pipeline mode: run API + pipeline concurrently
        let program = &programs[0];
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

        let pipeline_handle = tokio::spawn(async move {
            use solarix::decoder::ChainparserDecoder;
            use solarix::pipeline::rpc::RpcClient;
            use solarix::pipeline::PipelineOrchestrator;
            use solarix::storage::writer::StorageWriter;

            let rpc = RpcClient::new(&pipeline_config)?;
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
            );

            orch.run(&pipeline_program_id, &pipeline_schema_name, &pipeline_idl)
                .await
        });

        // Wait for either to finish — the other is stopped via cancel token
        tokio::select! {
            result = pipeline_handle => {
                match result {
                    Ok(Ok(())) => info!("pipeline exited cleanly"),
                    Ok(Err(e)) => error!(error = %e, "pipeline error"),
                    Err(e) => error!(error = %e, "pipeline task panicked"),
                }
                cancel.cancel(); // Stop API server too
                0
            }
            result = api_handle => {
                match result {
                    Ok(Ok(())) => info!("API server exited"),
                    Ok(Err(e)) => error!(error = %e, "API server error"),
                    Err(e) => error!(error = %e, "API server task panicked"),
                }
                cancel.cancel(); // Stop pipeline too
                0
            }
        }
    };

    // Graceful shutdown: final DB updates with timeout
    graceful_shutdown(&pool, &programs, &config).await;

    info!(
        uptime_secs = start_time.elapsed().as_secs(),
        "shutdown complete"
    );

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Registered program info loaded from DB at startup.
struct StartupProgram {
    program_id: String,
    schema_name: String,
    idl: anchor_lang_idl_spec::Idl,
}

/// Query the programs table for programs with complete registration and cached IDL.
///
/// Since IDLs are stored in-memory only (not persisted to DB), this function
/// tries to re-fetch IDLs from on-chain for programs with `schema_created` status.
/// Programs whose IDL cannot be re-fetched are skipped.
async fn query_registered_programs(pool: &PgPool) -> Vec<StartupProgram> {
    let rows = match sqlx::query_as::<_, (String, String)>(
        r#"SELECT "program_id", "schema_name" FROM "programs" WHERE "status" = 'schema_created'"#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "failed to query programs table");
            return Vec::new();
        }
    };

    if rows.is_empty() {
        return Vec::new();
    }

    info!(count = rows.len(), "found registered programs in DB");

    // For now, IDLs are not persisted to DB. To auto-start the pipeline,
    // we would need to re-fetch IDLs from on-chain. This is deferred —
    // users should re-register programs after restart until IDL persistence
    // is added.
    //
    // TODO: Add idl_json column to programs table for IDL persistence,
    // then parse and return here.
    warn!(
        "IDL persistence not yet implemented — pipeline auto-start requires re-registration. \
         Register programs via POST /api/programs to start indexing."
    );
    Vec::new()
}

/// Final shutdown sequence: update indexer_state and close pool.
async fn graceful_shutdown(pool: &PgPool, programs: &[StartupProgram], config: &Config) {
    // Update indexer_state to "stopped" for all active programs
    for program in programs {
        let timeout_dur = Duration::from_secs(config.shutdown_db_flush_secs);
        match tokio::time::timeout(timeout_dur, async {
            sqlx::query(
                r#"UPDATE "indexer_state"
                   SET "status" = 'stopped', "last_heartbeat" = NOW()
                   WHERE "program_id" = $1"#,
            )
            .bind(&program.program_id)
            .execute(pool)
            .await
        })
        .await
        {
            Ok(Ok(_)) => {
                info!(
                    program_id = %program.program_id,
                    "indexer_state set to stopped"
                );
            }
            Ok(Err(e)) => {
                warn!(
                    program_id = %program.program_id,
                    error = %e,
                    "failed to update indexer_state on shutdown"
                );
            }
            Err(_) => {
                warn!(
                    program_id = %program.program_id,
                    "indexer_state update timed out on shutdown"
                );
            }
        }
    }

    pool.close().await;
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| tracing::warn!(error = %e, "failed to install SIGTERM handler"))
            .ok();

        tokio::select! {
            _ = ctrl_c => { tracing::info!("received SIGINT, shutting down"); },
            _ = async {
                if let Some(ref mut s) = sigterm {
                    s.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => { tracing::info!("received SIGTERM, shutting down"); },
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        tracing::info!("received SIGINT, shutting down");
    }
}
