use clap::Parser;

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

    #[arg(long, env = "SOLARIX_TX_ENCODING", default_value = "base64")]
    pub tx_encoding: String,

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
    #[arg(long, env = "SOLARIX_CHANNEL_CAPACITY", default_value_t = 256)]
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

    // === Logging ===
    #[arg(long, env = "SOLARIX_LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    #[arg(long, env = "SOLARIX_LOG_FORMAT", default_value = "json")]
    pub log_format: String,
}
