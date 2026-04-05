use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use tokio::net::TcpListener;
use tracing::{error, info};

use solarix::config::Config;

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

    info!(
        rpc_url = %config.rpc_url,
        api_host = %config.api_host,
        api_port = config.api_port,
        "solarix starting"
    );

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

    let start_time = Instant::now();
    let state = Arc::new(solarix::api::AppState { pool, start_time });
    let app = solarix::api::router(state);

    let addr = format!("{}:{}", config.api_host, config.api_port);
    let listener = TcpListener::bind(&addr).await.map_err(|e| {
        error!(error = %e, addr = %addr, "failed to bind listener");
        e
    })?;

    info!(addr = %addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| {
            error!(error = %e, "server error");
            e
        })?;

    info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

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
