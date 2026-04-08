use clap::Parser;

fn parse_nonzero_usize(s: &str) -> Result<usize, String> {
    let val: usize = s.parse().map_err(|e| format!("{e}"))?;
    if val == 0 {
        return Err("value must be at least 1".to_string());
    }
    Ok(val)
}

fn parse_nonzero_u64(s: &str) -> Result<u64, String> {
    let val: u64 = s.parse().map_err(|e| format!("{e}"))?;
    if val == 0 {
        return Err("value must be at least 1".to_string());
    }
    Ok(val)
}

/// Solarix universal Solana indexer configuration.
#[derive(Parser, Debug, Clone)]
#[command(name = "solarix", about = "Universal Solana indexer")]
pub struct Config {
    // === Solana RPC ===
    #[arg(
        long,
        env = "SOLANA_RPC_URL",
        default_value = "https://api.mainnet-beta.solana.com"
    )]
    pub rpc_url: String,

    #[arg(long, env = "SOLANA_WS_URL")]
    pub ws_url: Option<String>,

    // === Database ===
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    #[arg(long, env = "SOLARIX_DB_POOL_MIN", default_value_t = 2)]
    pub db_pool_min: u32,

    #[arg(long, env = "SOLARIX_DB_POOL_MAX", default_value_t = 10)]
    pub db_pool_max: u32,

    // === Rate Limiting ===
    #[arg(long, env = "SOLARIX_RPC_RPS", default_value_t = 10)]
    pub rpc_rps: u32,

    // === Backfill ===
    #[arg(long, env = "SOLARIX_BACKFILL_CHUNK_SIZE", default_value_t = 50_000)]
    pub backfill_chunk_size: u64,

    #[arg(long, env = "SOLARIX_START_SLOT")]
    pub start_slot: Option<u64>,

    #[arg(long, env = "SOLARIX_END_SLOT")]
    pub end_slot: Option<u64>,

    // === Indexing ===
    #[arg(long, env = "SOLARIX_INDEX_FAILED_TXS", default_value_t = false)]
    pub index_failed_txs: bool,

    // === API ===
    #[arg(long, env = "SOLARIX_API_HOST", default_value = "0.0.0.0")]
    pub api_host: String,

    #[arg(long, env = "SOLARIX_API_PORT", default_value_t = 3000)]
    pub api_port: u16,

    #[arg(long, env = "SOLARIX_API_PAGE_SIZE", default_value_t = 50)]
    pub api_default_page_size: u32,

    #[arg(long, env = "SOLARIX_API_MAX_PAGE_SIZE", default_value_t = 1000)]
    pub api_max_page_size: u32,

    // === Pipeline ===
    #[arg(long, env = "SOLARIX_CHANNEL_CAPACITY", default_value_t = 256, value_parser = parse_nonzero_usize)]
    pub channel_capacity: usize,

    #[arg(long, env = "SOLARIX_CHECKPOINT_INTERVAL_SECS", default_value_t = 10)]
    pub checkpoint_interval_secs: u64,

    // === Retry ===
    #[arg(long, env = "SOLARIX_RETRY_INITIAL_MS", default_value_t = 500)]
    pub retry_initial_ms: u64,

    #[arg(long, env = "SOLARIX_RETRY_MAX_MS", default_value_t = 30_000)]
    pub retry_max_ms: u64,

    #[arg(long, env = "SOLARIX_RETRY_TIMEOUT_SECS", default_value_t = 300)]
    pub retry_timeout_secs: u64,

    // === Streaming ===
    #[arg(
        long,
        env = "SOLARIX_MAX_CONSECUTIVE_FETCH_FAILURES",
        default_value_t = 100,
        value_parser = parse_nonzero_u64
    )]
    pub max_consecutive_fetch_failures: u64,

    // === WebSocket ===
    #[arg(long, env = "SOLARIX_WS_PING_INTERVAL_SECS", default_value_t = 30, value_parser = parse_nonzero_u64)]
    pub ws_ping_interval_secs: u64,

    #[arg(long, env = "SOLARIX_WS_PONG_TIMEOUT_SECS", default_value_t = 10, value_parser = parse_nonzero_u64)]
    pub ws_pong_timeout_secs: u64,

    #[arg(long, env = "SOLARIX_DEDUP_CACHE_SIZE", default_value_t = 10_000, value_parser = parse_nonzero_usize)]
    pub dedup_cache_size: usize,

    // === Shutdown ===
    #[arg(long, env = "SOLARIX_SHUTDOWN_DRAIN_SECS", default_value_t = 15)]
    pub shutdown_drain_secs: u64,

    #[arg(long, env = "SOLARIX_SHUTDOWN_DB_FLUSH_SECS", default_value_t = 10)]
    pub shutdown_db_flush_secs: u64,

    // === Logging ===
    #[arg(long, env = "SOLARIX_LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    #[arg(long, env = "SOLARIX_LOG_FORMAT", default_value = "json")]
    pub log_format: String,
}

impl Config {
    /// Test-friendly default Config used by both unit tests (via `#[cfg(test)]`
    /// modules) and integration tests in `tests/*.rs`. Exposed unconditionally
    /// so `tests/common/api.rs::build_test_state` can call
    /// `solarix::config::Config::test_default()` when constructing `AppState`.
    pub fn test_default() -> Self {
        Self {
            rpc_url: String::new(),
            ws_url: None,
            database_url: String::new(),
            db_pool_min: 2,
            db_pool_max: 10,
            rpc_rps: 10,
            backfill_chunk_size: 50_000,
            start_slot: None,
            end_slot: None,
            index_failed_txs: false,
            api_host: String::new(),
            api_port: 3000,
            api_default_page_size: 50,
            api_max_page_size: 1000,
            channel_capacity: 256,
            checkpoint_interval_secs: 10,
            retry_initial_ms: 500,
            retry_max_ms: 30_000,
            retry_timeout_secs: 300,
            max_consecutive_fetch_failures: 100,
            ws_ping_interval_secs: 30,
            ws_pong_timeout_secs: 10,
            dedup_cache_size: 10_000,
            shutdown_drain_secs: 15,
            shutdown_db_flush_secs: 10,
            log_level: String::new(),
            log_format: String::new(),
        }
    }
}
