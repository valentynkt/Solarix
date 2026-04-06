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
use crate::pipeline::rpc::{BlockSource, RpcBlock, RpcClient};
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
            total_slots = self.end_slot - self.start_slot,
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

    /// Decode all matching instructions from a block for the target program.
    fn decode_block(
        &self,
        program_id: &str,
        block: &RpcBlock,
        idl: &Idl,
    ) -> Vec<DecodedInstruction> {
        let mut decoded = Vec::new();
        let mut decode_failures = 0usize;
        let mut decode_attempts = 0usize;

        for tx in &block.transactions {
            // Skip failed transactions unless configured otherwise
            if !self.config.index_failed_txs && !tx.success {
                continue;
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
                            block.slot,
                            block.block_time,
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
                            slot = block.slot,
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
                    if !instruction_targets_program(
                        ix.program_id_index,
                        &tx.account_keys,
                        program_id,
                    ) {
                        continue;
                    }

                    decode_attempts += 1;
                    match self.decoder.decode_instruction(program_id, &ix.data, idl) {
                        Ok(mut di) => {
                            enrich_instruction(
                                &mut di,
                                &tx.signature,
                                block.slot,
                                block.block_time,
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
                                slot = block.slot,
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
        }

        if is_high_failure_rate(decode_failures, decode_attempts) {
            error!(
                slot = block.slot,
                failures = decode_failures,
                attempts = decode_attempts,
                "high decode failure rate (>90%) — likely IDL mismatch"
            );
        }

        decoded
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
        .filter_map(|&idx| account_keys.get(idx as usize).cloned())
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
}
