//! Process-wide atomic counters for Prometheus metrics and the shutdown summary event.

// std library
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide counters for runtime events that are not persisted in the DB.
///
/// Story 6.1 introduces two counters (`rpc_retries`, `decode_failures`) read by
/// the shutdown summary event. Story 6.2 will add Prometheus gauges on top of
/// the same instance via `AppState` without any refactor of the pipeline or
/// RPC layers. The struct is intentionally lock-free (`AtomicU64`) — no
/// `parking_lot`/`dashmap` because the counters only ever increment.
#[derive(Debug, Default)]
pub struct RuntimeStats {
    /// Total number of RPC retry attempts since startup.
    pub rpc_retries: AtomicU64,
    /// Total number of Borsh decode failures since startup.
    pub decode_failures: AtomicU64,
}

impl RuntimeStats {
    /// Create a new `RuntimeStats` with all counters zeroed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the current RPC retry count.
    pub fn rpc_retries(&self) -> u64 {
        self.rpc_retries.load(Ordering::Relaxed)
    }

    /// Return the current decode failure count.
    pub fn decode_failures(&self) -> u64 {
        self.decode_failures.load(Ordering::Relaxed)
    }

    /// Increment the RPC retry counter by one.
    pub fn incr_rpc_retry(&self) {
        self.rpc_retries.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment the decode failure counter by one.
    pub fn incr_decode_failure(&self) {
        self.decode_failures.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_stats_starts_at_zero() {
        let stats = RuntimeStats::new();
        assert_eq!(stats.rpc_retries(), 0);
        assert_eq!(stats.decode_failures(), 0);
    }

    #[test]
    fn runtime_stats_increments() {
        let stats = RuntimeStats::new();
        stats.incr_rpc_retry();
        stats.incr_rpc_retry();
        stats.incr_decode_failure();
        assert_eq!(stats.rpc_retries(), 2);
        assert_eq!(stats.decode_failures(), 1);
    }

    #[test]
    fn runtime_stats_is_send_sync() {
        fn _assert<T: Send + Sync>() {}
        _assert::<RuntimeStats>();
    }
}
