//! Solarix — universal Solana indexer.
//!
//! Dynamically generates typed PostgreSQL schemas from Anchor IDLs at runtime,
//! then indexes transactions and account states through a four-layer pipeline:
//! Read → Decode → Store → Serve.

pub mod api;
pub mod config;
pub mod decoder;
pub mod idl;
pub mod pipeline;
pub mod registry;
pub mod runtime_stats;
pub mod startup;
pub mod storage;
pub mod types;

pub use runtime_stats::RuntimeStats;
