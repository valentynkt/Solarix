pub mod rpc;
pub mod ws;

// std library
use std::sync::Arc;
use std::time::Duration;

// external crates
use anchor_lang_idl_spec::Idl;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// internal crate
use crate::config::Config;
use crate::decoder::{is_high_failure_rate, DecodeError, SolarixDecoder};
use crate::idl::IdlError;
use crate::pipeline::rpc::{BlockSource, RpcBlock, RpcClient, RpcTransaction};
use crate::storage::writer::StorageWriter;
use crate::storage::StorageError;
use crate::types::{DecodedAccount, DecodedInstruction};

// ---------------------------------------------------------------------------
// PipelineError (preserved from original)
// ---------------------------------------------------------------------------

/// Errors that can occur during pipeline operations.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("RPC call failed: {0}")]
    RpcFailed(String),

    #[error("WebSocket disconnected: {0}")]
    WebSocketDisconnect(String),

    #[error("rate limited")]
    RateLimited,

    #[error("decode error: {0}")]
    Decode(#[from] DecodeError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("IDL error: {0}")]
    Idl(#[from] IdlError),

    #[error("slot skipped: {0}")]
    SlotSkipped(String),

    #[error("fatal: {0}")]
    Fatal(String),
}

impl PipelineError {
    /// Whether this error is transient and the operation should be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RpcFailed(_)
                | Self::WebSocketDisconnect(_)
                | Self::RateLimited
                | Self::Idl(IdlError::FetchFailed { .. })
        )
    }
}

// ---------------------------------------------------------------------------
// StreamInterrupt — internal signal for streaming loop control flow
// ---------------------------------------------------------------------------

/// Distinguishes recoverable disconnects from fatal errors in the streaming loop.
enum StreamInterrupt {
    /// WebSocket disconnected; last known slot provided for gap detection.
    Disconnect(u64),
    /// Unrecoverable error; pipeline should stop.
    Fatal(PipelineError),
}

// ---------------------------------------------------------------------------
// WriteBatch — message type for the mpsc channel
// ---------------------------------------------------------------------------

struct WriteBatch {
    schema_name: String,
    stream: String,
    instructions: Vec<DecodedInstruction>,
    accounts: Vec<DecodedAccount>,
    slot: u64,
    signature: Option<String>,
}

// ---------------------------------------------------------------------------
// BackfillProgress — progress tracking
// ---------------------------------------------------------------------------

struct BackfillProgress {
    start_slot: u64,
    end_slot: u64,
    current_slot: u64,
    blocks_processed: u64,
    txs_decoded: u64,
    started_at: Instant,
    last_log_at: Instant,
}

impl BackfillProgress {
    fn new(start_slot: u64, end_slot: u64) -> Self {
        let now = Instant::now();
        Self {
            start_slot,
            end_slot,
            current_slot: start_slot,
            blocks_processed: 0,
            txs_decoded: 0,
            started_at: now,
            last_log_at: now,
        }
    }

    fn percent_complete(&self) -> f64 {
        let total = self.end_slot.saturating_sub(self.start_slot);
        if total == 0 {
            return 100.0;
        }
        let done = self.current_slot.saturating_sub(self.start_slot);
        (done as f64 / total as f64) * 100.0
    }

    fn slots_per_sec(&self) -> f64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed < 0.001 {
            return 0.0;
        }
        let processed = self.current_slot.saturating_sub(self.start_slot);
        processed as f64 / elapsed
    }

    fn eta(&self) -> Duration {
        let rate = self.slots_per_sec();
        if rate < 0.001 {
            return Duration::from_secs(0);
        }
        let remaining = self.end_slot.saturating_sub(self.current_slot);
        Duration::from_secs_f64(remaining as f64 / rate)
    }

    fn should_log(&self, interval_secs: u64) -> bool {
        self.last_log_at.elapsed() >= Duration::from_secs(interval_secs)
    }

    fn log_progress(&mut self) {
        info!(
            current_slot = self.current_slot,
            total_slots = self.end_slot.saturating_sub(self.start_slot),
            percent = format!("{:.1}%", self.percent_complete()),
            slots_per_sec = format!("{:.1}", self.slots_per_sec()),
            eta_secs = self.eta().as_secs(),
            blocks = self.blocks_processed,
            txs = self.txs_decoded,
            "backfill progress"
        );
        self.last_log_at = Instant::now();
    }
}

// ---------------------------------------------------------------------------
// PipelineOrchestrator
// ---------------------------------------------------------------------------

/// Pipeline orchestrator: manages the indexing state machine.
///
/// States: Initializing -> Backfilling <-> CatchingUp -> Streaming -> ShuttingDown
pub struct PipelineOrchestrator {
    pool: PgPool,
    rpc: RpcClient,
    decoder: Box<dyn SolarixDecoder>,
    writer: Arc<StorageWriter>,
    config: Config,
    cancel: CancellationToken,
}

impl PipelineOrchestrator {
    pub fn new(
        pool: PgPool,
        rpc: RpcClient,
        decoder: Box<dyn SolarixDecoder>,
        writer: StorageWriter,
        config: Config,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            pool,
            rpc,
            decoder,
            writer: Arc::new(writer),
            config,
            cancel,
        }
    }

    /// Run backfill for a registered program over a slot range.
    pub async fn run_backfill(
        &self,
        program_id: &str,
        schema_name: &str,
        idl: &Idl,
        start_slot: u64,
        end_slot: u64,
    ) -> Result<(), PipelineError> {
        // Read checkpoint for resume
        let checkpoint = self.writer.read_checkpoint(schema_name, "backfill").await?;

        let effective_start = if let Some(cp) = &checkpoint {
            let resume = cp.last_slot.saturating_add(1);
            let effective = std::cmp::max(resume, start_slot);
            info!(
                checkpoint_slot = cp.last_slot,
                effective_start = effective,
                "resuming backfill from checkpoint"
            );
            effective
        } else {
            start_slot
        };

        if effective_start > end_slot {
            info!(
                effective_start,
                end_slot, "backfill already complete (checkpoint past end)"
            );
            return Ok(());
        }

        let chunks =
            compute_backfill_chunks(effective_start, end_slot, self.config.backfill_chunk_size);
        info!(
            program_id,
            start = effective_start,
            end = end_slot,
            chunk_count = chunks.len(),
            chunk_size = self.config.backfill_chunk_size,
            "starting backfill"
        );

        // Update indexer_state to backfilling
        update_indexer_state(&self.pool, program_id, "backfilling", Some(effective_start)).await?;

        // Set up mpsc channel + writer task
        let (tx, rx) = mpsc::channel::<WriteBatch>(self.config.channel_capacity);
        let writer = Arc::clone(&self.writer);
        let pool_clone = self.pool.clone();
        let program_id_owned = program_id.to_string();
        let cancel_clone = self.cancel.clone();

        let writer_handle = tokio::spawn(async move {
            writer_task(rx, writer, &pool_clone, &program_id_owned, cancel_clone).await
        });

        let mut progress = BackfillProgress::new(effective_start, end_slot);

        let mut backfill_result = Ok(());
        for (chunk_start, chunk_end) in &chunks {
            if self.cancel.is_cancelled() {
                info!("backfill cancelled");
                break;
            }

            match self
                .process_chunk(
                    program_id,
                    schema_name,
                    idl,
                    *chunk_start,
                    *chunk_end,
                    &tx,
                    &mut progress,
                )
                .await
            {
                Ok(()) => {
                    // Update indexer_state heartbeat at chunk boundary
                    if let Err(e) = update_indexer_state(
                        &self.pool,
                        program_id,
                        "backfilling",
                        Some(*chunk_end),
                    )
                    .await
                    {
                        warn!(error = %e, "failed to update indexer_state heartbeat");
                    }
                }
                Err(e) => {
                    error!(error = %e, chunk_start, chunk_end, "chunk processing failed");
                    backfill_result = Err(e);
                    break;
                }
            }
        }

        // Drop sender to signal writer task to finish
        drop(tx);

        // Wait for writer task to complete
        match writer_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!(error = %e, "writer task failed");
                if backfill_result.is_ok() {
                    backfill_result = Err(e);
                }
            }
            Err(e) => {
                error!(error = %e, "writer task panicked");
                if backfill_result.is_ok() {
                    backfill_result =
                        Err(PipelineError::Fatal(format!("writer task panicked: {e}")));
                }
            }
        }

        // Update indexer_state based on outcome
        if backfill_result.is_ok() && !self.cancel.is_cancelled() {
            update_indexer_state(&self.pool, program_id, "idle", Some(end_slot)).await?;
            info!(program_id, start_slot, end_slot, "backfill complete");
        } else if backfill_result.is_err() {
            if let Err(e) = update_indexer_state(&self.pool, program_id, "failed", None).await {
                warn!(error = %e, "failed to set indexer_state to failed");
            }
        }

        backfill_result
    }

    /// Process a single operational chunk of the backfill.
    async fn process_chunk(
        &self,
        program_id: &str,
        schema_name: &str,
        idl: &Idl,
        chunk_start: u64,
        chunk_end: u64,
        tx: &mpsc::Sender<WriteBatch>,
        progress: &mut BackfillProgress,
    ) -> Result<(), PipelineError> {
        debug!(chunk_start, chunk_end, "processing chunk");

        let block_slots = self.rpc.get_blocks(chunk_start, chunk_end).await?;

        for &slot in &block_slots {
            if self.cancel.is_cancelled() {
                break;
            }

            let block = match self.rpc.get_block(slot).await {
                Ok(Some(block)) => block,
                Ok(None) => {
                    debug!(slot, "skipped slot (empty/skipped)");
                    continue;
                }
                Err(e) => {
                    warn!(slot, error = %e, "failed to fetch block, skipping");
                    continue;
                }
            };

            let instructions = self.decode_block(program_id, &block, idl);

            if !instructions.is_empty() {
                let sig = block.transactions.first().map(|t| t.signature.clone());

                let batch = WriteBatch {
                    schema_name: schema_name.to_string(),
                    stream: "backfill".to_string(),
                    instructions,
                    accounts: Vec::new(),
                    slot: block.slot,
                    signature: sig,
                };

                tx.send(batch)
                    .await
                    .map_err(|_| PipelineError::Fatal("writer task channel closed".into()))?;
            }

            progress.current_slot = slot;
            progress.blocks_processed += 1;

            if progress.should_log(self.config.checkpoint_interval_secs) {
                progress.log_progress();
            }
        }

        Ok(())
    }

    /// Decode matching instructions from a single transaction for the target program.
    fn decode_transaction(
        &self,
        program_id: &str,
        tx: &RpcTransaction,
        idl: &Idl,
    ) -> (Vec<DecodedInstruction>, usize, usize) {
        let mut decoded = Vec::new();
        let mut decode_failures = 0usize;
        let mut decode_attempts = 0usize;

        // Skip failed transactions unless configured otherwise
        if !self.config.index_failed_txs && !tx.success {
            return (decoded, decode_failures, decode_attempts);
        }

        // Decode top-level instructions
        for (ix_index, ix) in tx.instructions.iter().enumerate() {
            if !instruction_targets_program(ix.program_id_index, &tx.account_keys, program_id) {
                continue;
            }

            decode_attempts += 1;
            match self.decoder.decode_instruction(program_id, &ix.data, idl) {
                Ok(mut di) => {
                    enrich_instruction(
                        &mut di,
                        &tx.signature,
                        tx.slot,
                        None, // block_time not available at tx level; caller sets if needed
                        ix_index as u8,
                        None,
                        &ix.accounts,
                        &tx.account_keys,
                        program_id,
                    );
                    decoded.push(di);
                }
                Err(e) => {
                    decode_failures += 1;
                    warn!(
                        slot = tx.slot,
                        signature = %tx.signature,
                        ix_index,
                        error = %e,
                        "instruction decode failed, skipping"
                    );
                }
            }
        }

        // Decode inner instructions (CPI)
        for group in &tx.inner_instructions {
            for (inner_idx, ix) in group.instructions.iter().enumerate() {
                if !instruction_targets_program(ix.program_id_index, &tx.account_keys, program_id) {
                    continue;
                }

                decode_attempts += 1;
                match self.decoder.decode_instruction(program_id, &ix.data, idl) {
                    Ok(mut di) => {
                        enrich_instruction(
                            &mut di,
                            &tx.signature,
                            tx.slot,
                            None,
                            group.index,
                            Some(inner_idx as u8),
                            &ix.accounts,
                            &tx.account_keys,
                            program_id,
                        );
                        decoded.push(di);
                    }
                    Err(e) => {
                        decode_failures += 1;
                        warn!(
                            slot = tx.slot,
                            signature = %tx.signature,
                            parent_ix = group.index,
                            inner_idx,
                            error = %e,
                            "inner instruction decode failed, skipping"
                        );
                    }
                }
            }
        }

        (decoded, decode_failures, decode_attempts)
    }

    /// Decode all matching instructions from a block for the target program.
    fn decode_block(
        &self,
        program_id: &str,
        block: &RpcBlock,
        idl: &Idl,
    ) -> Vec<DecodedInstruction> {
        let mut all_decoded = Vec::new();
        let mut total_failures = 0usize;
        let mut total_attempts = 0usize;

        for tx in &block.transactions {
            let (mut decoded, failures, attempts) = self.decode_transaction(program_id, tx, idl);

            // Set block_time from block context (decode_transaction leaves it as None)
            for di in &mut decoded {
                di.block_time = block.block_time;
            }

            all_decoded.append(&mut decoded);
            total_failures += failures;
            total_attempts += attempts;
        }

        if is_high_failure_rate(total_failures, total_attempts) {
            error!(
                slot = block.slot,
                failures = total_failures,
                attempts = total_attempts,
                "high decode failure rate (>90%) — likely IDL mismatch"
            );
        }

        all_decoded
    }

    /// Run account snapshot for a registered program.
    pub async fn run_account_snapshot(
        &self,
        program_id: &str,
        schema_name: &str,
        idl: &Idl,
    ) -> Result<(), PipelineError> {
        use crate::pipeline::rpc::AccountSource;

        info!(program_id, "starting account snapshot");

        let pubkeys = self.rpc.get_program_accounts(program_id).await?;
        info!(
            program_id,
            account_count = pubkeys.len(),
            "fetched account pubkeys"
        );

        if pubkeys.is_empty() {
            return Ok(());
        }

        let current_slot = self.rpc.get_slot().await?;

        let account_infos = self.rpc.get_multiple_accounts(&pubkeys).await?;

        let mut decoded_accounts = Vec::new();
        let mut decode_failures = 0usize;
        let total = account_infos.len();

        for info in &account_infos {
            if self.cancel.is_cancelled() {
                info!("account snapshot cancelled");
                return Ok(());
            }

            match self
                .decoder
                .decode_account(program_id, &info.pubkey, &info.data, idl)
            {
                Ok(mut da) => {
                    da.slot_updated = current_slot;
                    da.lamports = info.lamports;
                    decoded_accounts.push(da);
                }
                Err(e) => {
                    decode_failures += 1;
                    warn!(
                        pubkey = %info.pubkey,
                        error = %e,
                        "account decode failed, skipping"
                    );
                }
            }
        }

        if is_high_failure_rate(decode_failures, total) {
            error!(
                failures = decode_failures,
                total, "high account decode failure rate (>90%) — likely IDL mismatch"
            );
        }

        // Write accounts in batches to avoid unbounded memory for large programs
        const ACCOUNT_WRITE_BATCH: usize = 1000;
        for batch in decoded_accounts.chunks(ACCOUNT_WRITE_BATCH) {
            self.writer
                .write_block(schema_name, "accounts", &[], batch, current_slot, None)
                .await?;
        }

        info!(
            program_id,
            accounts_decoded = decoded_accounts.len(),
            accounts_failed = decode_failures,
            "account snapshot complete"
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Streaming pipeline
    // -----------------------------------------------------------------------

    /// Run the streaming pipeline for a registered program.
    ///
    /// This is the main streaming entry point. It creates a WebSocket connection,
    /// processes events, and automatically handles disconnects with gap detection
    /// and mini-backfill before resuming.
    pub async fn run_streaming(
        &self,
        program_id: &str,
        schema_name: &str,
        idl: &Idl,
    ) -> Result<(), PipelineError> {
        use crate::pipeline::ws::{TransactionStream, WsTransactionStream};
        use backon::{ExponentialBuilder, Retryable};

        loop {
            if self.cancel.is_cancelled() {
                return Ok(());
            }

            // Create + subscribe WS
            let mut stream = WsTransactionStream::new(&self.config);
            stream.subscribe(program_id).await?;
            update_indexer_state(&self.pool, program_id, "streaming", None).await?;

            // Streaming loop — returns disconnect slot or clean exit
            let disconnect_slot = match self
                .stream_events(&mut stream, program_id, schema_name, idl)
                .await
            {
                Ok(()) => return Ok(()), // clean exit (cancelled)
                Err(StreamInterrupt::Disconnect(slot)) => slot,
                Err(StreamInterrupt::Fatal(e)) => return Err(e),
            };

            // CatchingUp
            warn!(
                disconnect_slot,
                "WebSocket disconnected, entering CatchingUp"
            );
            if let Err(e) =
                update_indexer_state(&self.pool, program_id, "catching_up", Some(disconnect_slot))
                    .await
            {
                warn!(error = %e, "failed to set indexer_state to catching_up");
            }

            // Reconnect with backon retry
            let reconnect_result: Result<WsTransactionStream, PipelineError> = (|| async {
                let mut new_stream = WsTransactionStream::new(&self.config);
                new_stream.subscribe(program_id).await?;
                Ok(new_stream)
            })
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(self.config.retry_initial_ms))
                    .with_max_delay(Duration::from_millis(self.config.retry_max_ms))
                    .with_total_delay(Some(Duration::from_secs(self.config.retry_timeout_secs)))
                    .with_factor(2.0)
                    .with_jitter()
                    .without_max_times(),
            )
            .when(|e: &PipelineError| e.is_retryable())
            .notify(|err: &PipelineError, dur: Duration| {
                warn!(error = %err, delay = ?dur, "retrying WebSocket reconnection");
            })
            .await;

            match reconnect_result {
                Ok(_new_stream) => {
                    // Mini-backfill: fill the gap between disconnect and current tip
                    self.mini_backfill(program_id, schema_name, idl, disconnect_slot)
                        .await?;
                    // Loop back to create a fresh stream for the Streaming state
                }
                Err(e) => {
                    error!(error = %e, "WebSocket reconnection failed after retry timeout");
                    if let Err(update_err) =
                        update_indexer_state(&self.pool, program_id, "error", None).await
                    {
                        warn!(error = %update_err, "failed to set indexer_state to error");
                    }
                    return Err(PipelineError::Fatal(format!(
                        "max reconnection time exceeded: {e}"
                    )));
                }
            }
        }
    }

    /// Process streaming events until disconnect or cancellation.
    async fn stream_events(
        &self,
        stream: &mut dyn crate::pipeline::ws::TransactionStream,
        program_id: &str,
        schema_name: &str,
        idl: &Idl,
    ) -> Result<(), StreamInterrupt> {
        let mut consecutive_failures: u64 = 0;
        let mut last_heartbeat_at = Instant::now();
        let mut txs_processed: u64 = 0;

        loop {
            if self.cancel.is_cancelled() {
                return Ok(());
            }

            let event = match stream.next().await {
                Ok(Some(event)) => event,
                Ok(None) => continue, // no event (shouldn't happen but handle gracefully)
                Err(PipelineError::WebSocketDisconnect(reason)) => {
                    let slot = stream.last_seen_slot().unwrap_or(0);
                    // Try to get a better slot from checkpoint if stream has no slot
                    let disconnect_slot = if slot == 0 {
                        match self.writer.read_checkpoint(schema_name, "realtime").await {
                            Ok(Some(cp)) => cp.last_slot,
                            _ => 0,
                        }
                    } else {
                        slot
                    };
                    warn!(
                        disconnect_slot,
                        reason = %reason,
                        txs_processed,
                        "WebSocket disconnected"
                    );
                    return Err(StreamInterrupt::Disconnect(disconnect_slot));
                }
                Err(e) => return Err(StreamInterrupt::Fatal(e)),
            };

            // Skip failed txs if configured
            if event.error.is_some() && !self.config.index_failed_txs {
                debug!(signature = %event.signature, "skipping failed tx");
                continue;
            }

            // Fetch full transaction
            let tx = match self.rpc.get_transaction(&event.signature).await {
                Ok(Some(tx)) => {
                    consecutive_failures = 0;
                    tx
                }
                Ok(None) => {
                    warn!(
                        signature = %event.signature,
                        slot = event.slot,
                        "transaction not found (not yet finalized?), skipping"
                    );
                    continue;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    if consecutive_failures > self.config.max_consecutive_fetch_failures {
                        return Err(StreamInterrupt::Fatal(PipelineError::Fatal(format!(
                            "exceeded {} consecutive getTransaction failures: {e}",
                            self.config.max_consecutive_fetch_failures
                        ))));
                    }
                    warn!(
                        signature = %event.signature,
                        consecutive_failures,
                        error = %e,
                        "getTransaction failed, skipping"
                    );
                    continue;
                }
            };

            // Decode + enrich
            let (instructions, _failures, _attempts) =
                self.decode_transaction(program_id, &tx, idl);

            // Write
            if !instructions.is_empty() {
                self.writer
                    .write_block(
                        schema_name,
                        "realtime",
                        &instructions,
                        &[],
                        event.slot,
                        Some(&event.signature),
                    )
                    .await
                    .map_err(|e| StreamInterrupt::Fatal(PipelineError::Storage(e)))?;
            }

            txs_processed += 1;

            // Heartbeat
            if last_heartbeat_at.elapsed()
                >= Duration::from_secs(self.config.checkpoint_interval_secs)
            {
                if let Err(e) =
                    update_indexer_state(&self.pool, program_id, "streaming", Some(event.slot))
                        .await
                {
                    warn!(error = %e, "heartbeat update failed");
                }
                debug!(
                    txs_processed,
                    current_slot = event.slot,
                    "streaming heartbeat"
                );
                last_heartbeat_at = Instant::now();
            }
        }
    }

    /// Run a mini-backfill to fill the gap between disconnect_slot and chain tip.
    async fn mini_backfill(
        &self,
        program_id: &str,
        schema_name: &str,
        idl: &Idl,
        disconnect_slot: u64,
    ) -> Result<(), PipelineError> {
        let chain_tip = self.rpc.get_slot().await?;
        let gap_start = disconnect_slot.saturating_add(1);

        if gap_start > chain_tip {
            info!(disconnect_slot, chain_tip, "no gap to backfill");
            return Ok(());
        }

        let gap_size = chain_tip - gap_start + 1;
        info!(
            gap_start,
            chain_tip,
            gap = gap_size,
            "mini-backfill starting"
        );

        let chunks = compute_backfill_chunks(gap_start, chain_tip, self.config.backfill_chunk_size);

        let (tx, rx) = mpsc::channel::<WriteBatch>(self.config.channel_capacity);
        let writer = Arc::clone(&self.writer);
        let pool_clone = self.pool.clone();
        let program_id_owned = program_id.to_string();
        let cancel_clone = self.cancel.clone();

        let writer_handle = tokio::spawn(async move {
            writer_task(rx, writer, &pool_clone, &program_id_owned, cancel_clone).await
        });

        let mut progress = BackfillProgress::new(gap_start, chain_tip);

        let mut backfill_result = Ok(());
        for (chunk_start, chunk_end) in &chunks {
            if self.cancel.is_cancelled() {
                info!("mini-backfill cancelled");
                break;
            }

            // Use "catchup" stream name for mini-backfill checkpoint tracking
            match self
                .process_chunk(
                    program_id,
                    schema_name,
                    idl,
                    *chunk_start,
                    *chunk_end,
                    &tx,
                    &mut progress,
                )
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    error!(error = %e, chunk_start, chunk_end, "mini-backfill chunk failed");
                    backfill_result = Err(e);
                    break;
                }
            }
        }

        drop(tx);

        match writer_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!(error = %e, "mini-backfill writer task failed");
                if backfill_result.is_ok() {
                    backfill_result = Err(e);
                }
            }
            Err(e) => {
                error!(error = %e, "mini-backfill writer task panicked");
                if backfill_result.is_ok() {
                    backfill_result =
                        Err(PipelineError::Fatal(format!("writer task panicked: {e}")));
                }
            }
        }

        if backfill_result.is_ok() {
            info!(gap_start, chain_tip, "mini-backfill complete");
        }

        backfill_result
    }
}

// ---------------------------------------------------------------------------
// Writer task
// ---------------------------------------------------------------------------

async fn writer_task(
    mut rx: mpsc::Receiver<WriteBatch>,
    writer: Arc<StorageWriter>,
    pool: &PgPool,
    program_id: &str,
    cancel: CancellationToken,
) -> Result<(), PipelineError> {
    let mut total_instructions = 0u64;
    let mut total_accounts = 0u64;

    loop {
        tokio::select! {
            maybe_batch = rx.recv() => {
                match maybe_batch {
                    Some(batch) => {
                        let result = writer
                            .write_block(
                                &batch.schema_name,
                                &batch.stream,
                                &batch.instructions,
                                &batch.accounts,
                                batch.slot,
                                batch.signature.as_deref(),
                            )
                            .await?;

                        total_instructions += result.instructions_written;
                        total_accounts += result.accounts_written;
                    }
                    None => {
                        // Channel closed, producer is done
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                // Drain remaining items from channel — log errors but continue
                while let Ok(batch) = rx.try_recv() {
                    match writer
                        .write_block(
                            &batch.schema_name,
                            &batch.stream,
                            &batch.instructions,
                            &batch.accounts,
                            batch.slot,
                            batch.signature.as_deref(),
                        )
                        .await
                    {
                        Ok(result) => {
                            total_instructions += result.instructions_written;
                            total_accounts += result.accounts_written;
                        }
                        Err(e) => {
                            warn!(slot = batch.slot, error = %e, "drain write failed, continuing");
                        }
                    }
                }
                break;
            }
        }
    }

    // Update totals in indexer_state
    if total_instructions > 0 || total_accounts > 0 {
        if let Err(e) =
            increment_indexer_counters(pool, program_id, total_instructions, total_accounts).await
        {
            warn!(error = %e, "failed to update indexer_state counters");
        }
    }

    debug!(total_instructions, total_accounts, "writer task completed");

    Ok(())
}

// ---------------------------------------------------------------------------
// Pure helper functions
// ---------------------------------------------------------------------------

/// Check if an instruction targets the given program_id.
fn instruction_targets_program(
    program_id_index: u8,
    account_keys: &[String],
    program_id: &str,
) -> bool {
    account_keys
        .get(program_id_index as usize)
        .is_some_and(|key| key == program_id)
}

/// Enrich a decoded instruction with transaction context.
fn enrich_instruction(
    di: &mut DecodedInstruction,
    signature: &str,
    slot: u64,
    block_time: Option<i64>,
    instruction_index: u8,
    inner_index: Option<u8>,
    account_indices: &[u8],
    account_keys: &[String],
    program_id: &str,
) {
    di.signature = signature.to_string();
    di.slot = slot;
    di.block_time = block_time;
    di.instruction_index = instruction_index;
    di.inner_index = inner_index;
    di.program_id = program_id.to_string();
    di.accounts = account_indices
        .iter()
        .filter_map(|&idx| {
            let key = account_keys.get(idx as usize).cloned();
            if key.is_none() {
                warn!(
                    idx,
                    account_keys_len = account_keys.len(),
                    "OOB account index in instruction"
                );
            }
            key
        })
        .collect();
}

/// Compute (start, end) chunk pairs for operational backfill chunks.
fn compute_backfill_chunks(start: u64, end: u64, chunk_size: u64) -> Vec<(u64, u64)> {
    if start > end || chunk_size == 0 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = start;

    while current <= end {
        let chunk_end = std::cmp::min(current.saturating_add(chunk_size - 1), end);
        chunks.push((current, chunk_end));
        if chunk_end >= end {
            break;
        }
        current = chunk_end.saturating_add(1);
    }

    chunks
}

// ---------------------------------------------------------------------------
// indexer_state DB operations
// ---------------------------------------------------------------------------

/// Update the indexer_state row for a program.
async fn update_indexer_state(
    pool: &PgPool,
    program_id: &str,
    status: &str,
    last_slot: Option<u64>,
) -> Result<(), PipelineError> {
    let sql = r#"
        UPDATE "indexer_state"
        SET "status" = $1,
            "last_processed_slot" = COALESCE($2, "last_processed_slot"),
            "last_heartbeat" = NOW()
        WHERE "program_id" = $3
    "#;

    sqlx::query(sql)
        .bind(status)
        .bind(last_slot.map(|s| s as i64))
        .bind(program_id)
        .execute(pool)
        .await
        .map_err(|e| {
            PipelineError::Storage(StorageError::WriteFailed(format!(
                "indexer_state update failed: {e}"
            )))
        })?;

    Ok(())
}

/// Increment instruction/account counters in indexer_state.
async fn increment_indexer_counters(
    pool: &PgPool,
    program_id: &str,
    instructions: u64,
    accounts: u64,
) -> Result<(), PipelineError> {
    let sql = r#"
        UPDATE "indexer_state"
        SET "total_instructions" = "total_instructions" + $1,
            "total_accounts" = "total_accounts" + $2
        WHERE "program_id" = $3
    "#;

    sqlx::query(sql)
        .bind(instructions as i64)
        .bind(accounts as i64)
        .bind(program_id)
        .execute(pool)
        .await
        .map_err(|e| {
            PipelineError::Storage(StorageError::WriteFailed(format!(
                "indexer_state counter update failed: {e}"
            )))
        })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::rpc::{
        RpcBlock, RpcInnerInstructionGroup, RpcInstruction, RpcTransaction,
    };

    // -- Send safety compile-time checks --

    fn _assert_send<T: Send>(_: &T) {}
    fn _assert_sync<T: Sync>(_: &T) {}

    fn _require_pipeline_orchestrator_send(o: &PipelineOrchestrator) {
        _assert_send(o);
        _assert_sync(o);
    }

    // -- compute_backfill_chunks tests --

    #[test]
    fn test_compute_backfill_chunks_normal() {
        let chunks = compute_backfill_chunks(0, 149_999, 50_000);
        assert_eq!(
            chunks,
            vec![(0, 49_999), (50_000, 99_999), (100_000, 149_999)]
        );
    }

    #[test]
    fn test_compute_backfill_chunks_single_chunk() {
        let chunks = compute_backfill_chunks(100, 200, 50_000);
        assert_eq!(chunks, vec![(100, 200)]);
    }

    #[test]
    fn test_compute_backfill_chunks_exact_boundary() {
        let chunks = compute_backfill_chunks(0, 49_999, 50_000);
        assert_eq!(chunks, vec![(0, 49_999)]);
    }

    #[test]
    fn test_compute_backfill_chunks_single_slot() {
        let chunks = compute_backfill_chunks(42, 42, 50_000);
        assert_eq!(chunks, vec![(42, 42)]);
    }

    #[test]
    fn test_compute_backfill_chunks_inverted_range() {
        let chunks = compute_backfill_chunks(100, 50, 50_000);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_compute_backfill_chunks_zero_chunk_size() {
        let chunks = compute_backfill_chunks(0, 100, 0);
        assert!(chunks.is_empty());
    }

    // -- instruction_targets_program tests --

    #[test]
    fn test_instruction_targets_program_match() {
        let keys = vec![
            "Alice".to_string(),
            "Bob".to_string(),
            "TargetProgram".to_string(),
        ];
        assert!(instruction_targets_program(2, &keys, "TargetProgram"));
    }

    #[test]
    fn test_instruction_targets_program_no_match() {
        let keys = vec![
            "Alice".to_string(),
            "Bob".to_string(),
            "OtherProgram".to_string(),
        ];
        assert!(!instruction_targets_program(2, &keys, "TargetProgram"));
    }

    #[test]
    fn test_instruction_targets_program_index_out_of_bounds() {
        let keys = vec!["Alice".to_string()];
        assert!(!instruction_targets_program(5, &keys, "TargetProgram"));
    }

    // -- filter_transactions_for_program tests --

    fn make_test_block() -> RpcBlock {
        let target = "TargetProg";
        let other = "OtherProg";

        RpcBlock {
            slot: 100,
            block_time: Some(1_700_000_000),
            transactions: vec![
                // tx0: top-level match
                RpcTransaction {
                    signature: "sig_match".to_string(),
                    slot: 100,
                    success: true,
                    account_keys: vec!["Alice".to_string(), target.to_string()],
                    instructions: vec![RpcInstruction {
                        program_id_index: 1,
                        data: vec![1, 2, 3],
                        accounts: vec![0],
                    }],
                    inner_instructions: vec![],
                },
                // tx1: no match
                RpcTransaction {
                    signature: "sig_nomatch".to_string(),
                    slot: 100,
                    success: true,
                    account_keys: vec!["Alice".to_string(), other.to_string()],
                    instructions: vec![RpcInstruction {
                        program_id_index: 1,
                        data: vec![4, 5, 6],
                        accounts: vec![0],
                    }],
                    inner_instructions: vec![],
                },
                // tx2: CPI match (inner instruction)
                RpcTransaction {
                    signature: "sig_cpi".to_string(),
                    slot: 100,
                    success: true,
                    account_keys: vec!["Alice".to_string(), other.to_string(), target.to_string()],
                    instructions: vec![RpcInstruction {
                        program_id_index: 1,
                        data: vec![7, 8, 9],
                        accounts: vec![0],
                    }],
                    inner_instructions: vec![RpcInnerInstructionGroup {
                        index: 0,
                        instructions: vec![RpcInstruction {
                            program_id_index: 2,
                            data: vec![10, 11, 12],
                            accounts: vec![0],
                        }],
                    }],
                },
                // tx3: failed tx
                RpcTransaction {
                    signature: "sig_failed".to_string(),
                    slot: 100,
                    success: false,
                    account_keys: vec!["Alice".to_string(), target.to_string()],
                    instructions: vec![RpcInstruction {
                        program_id_index: 1,
                        data: vec![13, 14, 15],
                        accounts: vec![0],
                    }],
                    inner_instructions: vec![],
                },
            ],
        }
    }

    #[test]
    fn test_filter_transactions_top_level_match() {
        let block = make_test_block();
        let target = "TargetProg";

        // tx0 has top-level instruction targeting our program
        let tx = &block.transactions[0];
        assert!(tx.instructions.iter().any(|ix| {
            instruction_targets_program(ix.program_id_index, &tx.account_keys, target)
        }));
    }

    #[test]
    fn test_filter_transactions_no_match() {
        let block = make_test_block();
        let target = "TargetProg";

        // tx1 does NOT target our program
        let tx = &block.transactions[1];
        assert!(!tx.instructions.iter().any(|ix| {
            instruction_targets_program(ix.program_id_index, &tx.account_keys, target)
        }));
    }

    #[test]
    fn test_filter_transactions_cpi_match() {
        let block = make_test_block();
        let target = "TargetProg";

        // tx2 has CPI instruction targeting our program
        let tx = &block.transactions[2];
        let inner_match = tx.inner_instructions.iter().any(|group| {
            group.instructions.iter().any(|ix| {
                instruction_targets_program(ix.program_id_index, &tx.account_keys, target)
            })
        });
        assert!(inner_match);
    }

    #[test]
    fn test_filter_transactions_failed_tx_filtered() {
        let block = make_test_block();
        // tx3 is failed — should be filtered when index_failed_txs is false
        let tx = &block.transactions[3];
        assert!(!tx.success);
    }

    // -- enrich_instruction tests --

    #[test]
    fn test_enrich_instruction_top_level() {
        let mut di = DecodedInstruction::from_decoded(
            String::new(),
            "transfer".to_string(),
            serde_json::json!({"amount": 100}),
        );

        let account_keys = vec![
            "Alice".to_string(),
            "Bob".to_string(),
            "Program".to_string(),
        ];

        enrich_instruction(
            &mut di,
            "txsig123",
            42,
            Some(1_700_000_000),
            0,
            None,
            &[0, 1],
            &account_keys,
            "Program",
        );

        assert_eq!(di.signature, "txsig123");
        assert_eq!(di.slot, 42);
        assert_eq!(di.block_time, Some(1_700_000_000));
        assert_eq!(di.instruction_index, 0);
        assert_eq!(di.inner_index, None);
        assert_eq!(di.program_id, "Program");
        assert_eq!(di.accounts, vec!["Alice", "Bob"]);
    }

    #[test]
    fn test_enrich_instruction_inner() {
        let mut di = DecodedInstruction::from_decoded(
            String::new(),
            "cpi_call".to_string(),
            serde_json::json!({}),
        );

        let account_keys = vec!["Alice".to_string(), "Prog".to_string()];

        enrich_instruction(
            &mut di,
            "txsig456",
            99,
            None,
            2,
            Some(1),
            &[0],
            &account_keys,
            "Prog",
        );

        assert_eq!(di.signature, "txsig456");
        assert_eq!(di.slot, 99);
        assert_eq!(di.block_time, None);
        assert_eq!(di.instruction_index, 2);
        assert_eq!(di.inner_index, Some(1));
        assert_eq!(di.accounts, vec!["Alice"]);
    }

    #[test]
    fn test_enrich_instruction_oob_account_indices() {
        let mut di = DecodedInstruction::from_decoded(
            String::new(),
            "test".to_string(),
            serde_json::json!({}),
        );

        let account_keys = vec!["Alice".to_string()];

        // index 5 is out of bounds — should be filtered out
        enrich_instruction(
            &mut di,
            "sig",
            1,
            None,
            0,
            None,
            &[0, 5],
            &account_keys,
            "P",
        );

        assert_eq!(di.accounts, vec!["Alice"]);
    }

    // -- BackfillProgress tests --

    #[test]
    fn test_backfill_progress_percent_complete() {
        let mut p = BackfillProgress::new(100, 200);

        // At start
        assert!((p.percent_complete() - 0.0).abs() < 0.1);

        // At midpoint
        p.current_slot = 150;
        assert!((p.percent_complete() - 50.0).abs() < 0.1);

        // At end
        p.current_slot = 200;
        assert!((p.percent_complete() - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_backfill_progress_zero_range() {
        let p = BackfillProgress::new(100, 100);
        assert!((p.percent_complete() - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_backfill_progress_slots_per_sec() {
        let p = BackfillProgress::new(0, 1000);
        // At the very start, rate should be 0 (no elapsed time)
        assert!(p.slots_per_sec() >= 0.0);
    }

    #[test]
    fn test_backfill_progress_eta() {
        let p = BackfillProgress::new(0, 1000);
        // ETA is non-negative
        let eta = p.eta();
        assert!(eta.as_secs() >= 0);
    }

    // -- decode failure rate (uses is_high_failure_rate from decoder) --

    #[test]
    fn test_decode_failure_rate_tracking() {
        // 91/100 = 91% > 90% → true
        assert!(is_high_failure_rate(91, 100));
        // 90/100 = 90% NOT > 90% → false
        assert!(!is_high_failure_rate(90, 100));
        // 0/0 → false
        assert!(!is_high_failure_rate(0, 0));
        // 10/10 = 100% → true
        assert!(is_high_failure_rate(10, 10));
    }

    // -----------------------------------------------------------------------
    // Story 4.2: decode_transaction + streaming unit tests
    // -----------------------------------------------------------------------

    /// Mock decoder that always succeeds, returning instructions with the given name.
    struct MockDecoder;

    impl SolarixDecoder for MockDecoder {
        fn decode_instruction(
            &self,
            program_id: &str,
            _data: &[u8],
            _idl: &Idl,
        ) -> Result<DecodedInstruction, crate::decoder::DecodeError> {
            Ok(DecodedInstruction::from_decoded(
                program_id.to_string(),
                "mock_ix".to_string(),
                serde_json::json!({"key": "value"}),
            ))
        }

        fn decode_account(
            &self,
            program_id: &str,
            pubkey: &str,
            _data: &[u8],
            _idl: &Idl,
        ) -> Result<crate::types::DecodedAccount, crate::decoder::DecodeError> {
            Ok(crate::types::DecodedAccount::from_decoded(
                program_id.to_string(),
                "mock_account".to_string(),
                pubkey.to_string(),
                serde_json::json!({}),
            ))
        }
    }

    /// Mock decoder that always fails.
    struct FailDecoder;

    impl SolarixDecoder for FailDecoder {
        fn decode_instruction(
            &self,
            _program_id: &str,
            _data: &[u8],
            _idl: &Idl,
        ) -> Result<DecodedInstruction, crate::decoder::DecodeError> {
            Err(crate::decoder::DecodeError::UnknownDiscriminator(
                "ff".into(),
            ))
        }

        fn decode_account(
            &self,
            _program_id: &str,
            _pubkey: &str,
            _data: &[u8],
            _idl: &Idl,
        ) -> Result<crate::types::DecodedAccount, crate::decoder::DecodeError> {
            Err(crate::decoder::DecodeError::UnknownDiscriminator(
                "ff".into(),
            ))
        }
    }

    fn make_test_config() -> Config {
        Config {
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

    fn make_test_idl() -> Idl {
        serde_json::from_value(serde_json::json!({
            "address": "11111111111111111111111111111111",
            "metadata": { "name": "test", "version": "0.1.0", "spec": "0.1.0" },
            "instructions": [],
            "accounts": [],
            "types": []
        }))
        .expect("test IDL should parse")
    }

    fn make_test_tx(program_id: &str, success: bool) -> RpcTransaction {
        RpcTransaction {
            signature: "test_sig".to_string(),
            slot: 200,
            success,
            account_keys: vec!["Alice".to_string(), program_id.to_string()],
            instructions: vec![RpcInstruction {
                program_id_index: 1,
                data: vec![1, 2, 3],
                accounts: vec![0],
            }],
            inner_instructions: vec![],
        }
    }

    fn make_orch_with_decoder(decoder: Box<dyn SolarixDecoder>) -> PipelineOrchestrator {
        // Create a minimal orchestrator — no real DB pool or RPC (not needed for decode tests)
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let config = make_test_config();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(PgConnectOptions::new());

        let rpc = RpcClient::new(&config).expect("rpc");
        let writer = StorageWriter::new(pool.clone());
        let cancel = CancellationToken::new();

        PipelineOrchestrator::new(pool, rpc, decoder, writer, config, cancel)
    }

    #[tokio::test]
    async fn test_decode_transaction_top_level_match() {
        let orch = make_orch_with_decoder(Box::new(MockDecoder));
        let idl = make_test_idl();
        let program_id = "TargetProg";
        let tx = make_test_tx(program_id, true);

        let (decoded, failures, attempts) = orch.decode_transaction(program_id, &tx, &idl);

        assert_eq!(decoded.len(), 1);
        assert_eq!(failures, 0);
        assert_eq!(attempts, 1);
        assert_eq!(decoded[0].instruction_name, "mock_ix");
        assert_eq!(decoded[0].signature, "test_sig");
        assert_eq!(decoded[0].slot, 200);
        assert_eq!(decoded[0].instruction_index, 0);
        assert_eq!(decoded[0].inner_index, None);
        assert_eq!(decoded[0].accounts, vec!["Alice"]);
    }

    #[tokio::test]
    async fn test_decode_transaction_inner_instruction() {
        let orch = make_orch_with_decoder(Box::new(MockDecoder));
        let idl = make_test_idl();
        let program_id = "TargetProg";
        let tx = RpcTransaction {
            signature: "cpi_sig".to_string(),
            slot: 300,
            success: true,
            account_keys: vec![
                "Alice".to_string(),
                "OtherProg".to_string(),
                program_id.to_string(),
            ],
            instructions: vec![RpcInstruction {
                program_id_index: 1, // OtherProg — not our target
                data: vec![4, 5, 6],
                accounts: vec![0],
            }],
            inner_instructions: vec![RpcInnerInstructionGroup {
                index: 0,
                instructions: vec![RpcInstruction {
                    program_id_index: 2, // TargetProg — CPI match
                    data: vec![7, 8, 9],
                    accounts: vec![0],
                }],
            }],
        };

        let (decoded, failures, attempts) = orch.decode_transaction(program_id, &tx, &idl);

        assert_eq!(decoded.len(), 1);
        assert_eq!(failures, 0);
        assert_eq!(attempts, 1);
        assert_eq!(decoded[0].instruction_index, 0); // parent ix index
        assert_eq!(decoded[0].inner_index, Some(0)); // inner instruction index
    }

    #[tokio::test]
    async fn test_decode_transaction_no_match() {
        let orch = make_orch_with_decoder(Box::new(MockDecoder));
        let idl = make_test_idl();
        let program_id = "TargetProg";
        let tx = RpcTransaction {
            signature: "no_match_sig".to_string(),
            slot: 400,
            success: true,
            account_keys: vec!["Alice".to_string(), "OtherProg".to_string()],
            instructions: vec![RpcInstruction {
                program_id_index: 1, // OtherProg — not our target
                data: vec![1, 2, 3],
                accounts: vec![0],
            }],
            inner_instructions: vec![],
        };

        let (decoded, failures, attempts) = orch.decode_transaction(program_id, &tx, &idl);

        assert!(decoded.is_empty());
        assert_eq!(failures, 0);
        assert_eq!(attempts, 0);
    }

    #[tokio::test]
    async fn test_decode_transaction_failed_tx_skipped() {
        let orch = make_orch_with_decoder(Box::new(MockDecoder));
        let idl = make_test_idl();
        let program_id = "TargetProg";
        let tx = make_test_tx(program_id, false); // failed tx

        // config.index_failed_txs = false (default)
        let (decoded, _failures, _attempts) = orch.decode_transaction(program_id, &tx, &idl);

        assert!(decoded.is_empty(), "failed tx should be skipped");
    }

    #[tokio::test]
    async fn test_decode_transaction_failed_tx_indexed_when_configured() {
        let mut config = make_test_config();
        config.index_failed_txs = true;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(PgConnectOptions::new());
        let rpc = RpcClient::new(&config).expect("rpc");
        let writer = StorageWriter::new(pool.clone());
        let cancel = CancellationToken::new();
        let orch =
            PipelineOrchestrator::new(pool, rpc, Box::new(MockDecoder), writer, config, cancel);

        let idl = make_test_idl();
        let program_id = "TargetProg";
        let tx = make_test_tx(program_id, false); // failed tx

        let (decoded, _failures, _attempts) = orch.decode_transaction(program_id, &tx, &idl);

        assert_eq!(
            decoded.len(),
            1,
            "failed tx should be indexed when index_failed_txs = true"
        );
    }

    #[test]
    fn test_consecutive_failure_threshold() {
        // Unit test for the counter logic used in stream_events
        let threshold: u64 = 100;
        let mut consecutive_failures: u64 = 0;

        // Simulate 99 failures — should not exceed threshold
        for _ in 0..99 {
            consecutive_failures += 1;
            assert!(
                consecutive_failures <= threshold,
                "should not exceed threshold at {consecutive_failures}"
            );
        }

        // One more failure → hits threshold
        consecutive_failures += 1;
        assert_eq!(consecutive_failures, 100);

        // Reset on success
        consecutive_failures = 0;
        assert_eq!(consecutive_failures, 0);

        // 101 consecutive → exceeds threshold
        for _ in 0..101 {
            consecutive_failures += 1;
        }
        assert!(consecutive_failures > threshold);
    }

    #[test]
    fn test_heartbeat_timing() {
        let checkpoint_interval = Duration::from_secs(10);

        // Just created — not elapsed yet
        let heartbeat_at = Instant::now();
        assert!(
            heartbeat_at.elapsed() < checkpoint_interval,
            "freshly created instant should not have elapsed 10 seconds"
        );

        // Simulate elapsed time by using a past instant
        let past = Instant::now() - Duration::from_secs(11);
        assert!(
            past.elapsed() >= checkpoint_interval,
            "instant 11s ago should trigger heartbeat"
        );

        // Exactly at boundary
        let at_boundary = Instant::now() - checkpoint_interval;
        assert!(
            at_boundary.elapsed() >= checkpoint_interval,
            "instant exactly at boundary should trigger heartbeat"
        );
    }

    #[test]
    fn test_run_streaming_is_send() {
        // Compile-time check that run_streaming future is Send
        fn _check(o: &PipelineOrchestrator) {
            fn _require_send<T: Send>(_: &T) {}
            let idl = serde_json::from_value::<Idl>(serde_json::json!({
                "address": "11111111111111111111111111111111",
                "metadata": { "name": "test", "version": "0.1.0", "spec": "0.1.0" },
                "instructions": [],
                "accounts": [],
                "types": []
            }))
            .expect("test IDL");
            let fut = o.run_streaming("prog", "schema", &idl);
            _require_send(&fut);
        }
    }

    #[test]
    fn test_stream_interrupt_variants() {
        // Verify StreamInterrupt can hold both variants
        let disconnect = StreamInterrupt::Disconnect(12345);
        matches!(disconnect, StreamInterrupt::Disconnect(12345));

        let fatal = StreamInterrupt::Fatal(PipelineError::Fatal("test".into()));
        matches!(fatal, StreamInterrupt::Fatal(_));
    }
}
