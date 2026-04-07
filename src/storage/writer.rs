// std library
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Mutex;

// external crates
use sqlx::{PgPool, Row};
use tracing::debug;

// internal crate
use crate::storage::schema::{quote_ident, sanitize_identifier};
use crate::storage::StorageError;
use crate::types::{DecodedAccount, DecodedInstruction};

/// Result of a write_block operation.
#[derive(Debug, Clone)]
pub struct WriteResult {
    pub instructions_written: u64,
    pub accounts_written: u64,
}

/// Checkpoint information for crash-safe restart.
#[derive(Debug, Clone)]
pub struct CheckpointInfo {
    pub last_slot: u64,
    pub last_signature: Option<String>,
}

/// Metadata about a promoted column discovered from information_schema.
#[derive(Debug, Clone, PartialEq)]
struct PromotedColumn {
    column_name: String,
    data_type: String,
}

/// System column names reserved for account tables — excluded from promoted column discovery.
const COMMON_ACCOUNT_COLUMNS: &[&str] = &[
    "pubkey",
    "slot_updated",
    "write_version",
    "lamports",
    "data",
    "is_closed",
    "updated_at",
];

/// Batch writer for inserting decoded data into PostgreSQL.
pub struct StorageWriter {
    pool: PgPool,
    promoted_cache: Mutex<HashMap<String, Vec<PromotedColumn>>>,
}

impl StorageWriter {
    /// Create a new StorageWriter.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            promoted_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Write a block's worth of data atomically within a single transaction.
    ///
    /// Writes instructions, upserts accounts, and updates the checkpoint.
    /// If any operation fails, the entire transaction rolls back.
    #[tracing::instrument(
        name = "storage.write_block",
        skip(self, instructions, accounts),
        fields(
            schema_name = schema_name,
            stream = stream,
            slot = slot,
            signature = ?signature,
            instructions_count = instructions.len(),
            accounts_count = accounts.len(),
        ),
        level = "debug",
        err(Display)
    )]
    pub async fn write_block(
        &self,
        schema_name: &str,
        stream: &str,
        instructions: &[DecodedInstruction],
        accounts: &[DecodedAccount],
        slot: u64,
        signature: Option<&str>,
    ) -> Result<WriteResult, StorageError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageError::WriteFailed(format!("transaction begin failed: {e}")))?;

        let instructions_written = write_instructions(&mut tx, schema_name, instructions).await?;
        let accounts_written = self
            .write_accounts_inner(&mut tx, schema_name, accounts)
            .await?;
        update_checkpoint(&mut tx, schema_name, stream, slot, signature).await?;

        tx.commit()
            .await
            .map_err(|e| StorageError::WriteFailed(format!("transaction commit failed: {e}")))?;

        debug!(
            %schema_name, slot, instructions_written, accounts_written,
            "block written"
        );

        Ok(WriteResult {
            instructions_written,
            accounts_written,
        })
    }

    /// Read the last checkpoint for a given stream.
    #[tracing::instrument(
        name = "storage.read_checkpoint",
        skip(self),
        fields(schema_name = schema_name, stream = stream),
        level = "debug",
        err(Display)
    )]
    pub async fn read_checkpoint(
        &self,
        schema_name: &str,
        stream: &str,
    ) -> Result<Option<CheckpointInfo>, StorageError> {
        let sql = format!(
            r#"SELECT "last_slot", "last_signature" FROM {}.{} WHERE "stream" = $1"#,
            quote_ident(schema_name),
            quote_ident("_checkpoints"),
        );

        let row = sqlx::query(&sql)
            .bind(stream)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::CheckpointFailed(format!("checkpoint read failed: {e}")))?;

        match row {
            Some(row) => {
                let last_slot: Option<i64> = row.try_get("last_slot").map_err(|e| {
                    StorageError::CheckpointFailed(format!("bad checkpoint data: {e}"))
                })?;
                let last_signature: Option<String> =
                    row.try_get("last_signature").map_err(|e| {
                        StorageError::CheckpointFailed(format!("bad checkpoint data: {e}"))
                    })?;
                Ok(last_slot.and_then(|slot| {
                    if slot < 0 {
                        tracing::warn!(
                            slot,
                            "negative slot in checkpoint — treating as no checkpoint"
                        );
                        return None;
                    }
                    Some(CheckpointInfo {
                        last_slot: slot as u64,
                        last_signature,
                    })
                }))
            }
            None => Ok(None),
        }
    }

    // --- Private: account writing with promoted column discovery ---

    #[tracing::instrument(
        name = "storage.write_accounts_inner",
        skip(self, conn, accounts),
        fields(schema_name = schema_name, account_count = accounts.len()),
        level = "debug",
        err(Display)
    )]
    async fn write_accounts_inner(
        &self,
        conn: &mut sqlx::PgConnection,
        schema_name: &str,
        accounts: &[DecodedAccount],
    ) -> Result<u64, StorageError> {
        if accounts.is_empty() {
            return Ok(0);
        }

        // Group accounts by account_type
        let mut grouped: HashMap<&str, Vec<&DecodedAccount>> = HashMap::new();
        for account in accounts {
            grouped
                .entry(account.account_type.as_str())
                .or_default()
                .push(account);
        }

        let mut total_written = 0u64;

        for (account_type, type_accounts) in &grouped {
            let table_name = sanitize_identifier(account_type);
            let cache_key = format!("{schema_name}.{table_name}");

            let promoted = self
                .get_or_discover_promoted(schema_name, &table_name, &cache_key)
                .await?;

            let rows = write_accounts_batch(
                &mut *conn,
                schema_name,
                &table_name,
                type_accounts,
                &promoted,
            )
            .await?;
            total_written += rows;
        }

        Ok(total_written)
    }

    #[tracing::instrument(
        name = "storage.get_or_discover_promoted",
        skip(self),
        fields(schema_name = schema_name, table_name = table_name),
        level = "debug",
        err(Display)
    )]
    async fn get_or_discover_promoted(
        &self,
        schema_name: &str,
        table_name: &str,
        cache_key: &str,
    ) -> Result<Vec<PromotedColumn>, StorageError> {
        // Check cache (brief lock, no await while held)
        match self.promoted_cache.lock() {
            Ok(cache) => {
                if let Some(cached) = cache.get(cache_key).cloned() {
                    return Ok(cached);
                }
            }
            Err(e) => {
                tracing::warn!(%cache_key, "promoted column cache mutex poisoned (read): {e}");
            }
        }

        let columns = discover_promoted_columns(&self.pool, schema_name, table_name).await?;

        // Cache result (brief lock, no await while held)
        match self.promoted_cache.lock() {
            Ok(mut cache) => {
                cache.insert(cache_key.to_string(), columns.clone());
            }
            Err(e) => {
                tracing::warn!(%cache_key, "promoted column cache mutex poisoned (write): {e}");
            }
        }

        Ok(columns)
    }
}

// --- DB operations as free functions (keep futures Send-friendly) ---

/// Batch INSERT...UNNEST for instructions with ON CONFLICT DO NOTHING dedup.
#[tracing::instrument(
    name = "storage.write_instructions",
    skip(conn, instructions),
    fields(schema_name = schema_name, count = instructions.len()),
    level = "debug",
    err(Display)
)]
async fn write_instructions(
    conn: &mut sqlx::PgConnection,
    schema_name: &str,
    instructions: &[DecodedInstruction],
) -> Result<u64, StorageError> {
    if instructions.is_empty() {
        return Ok(0);
    }

    let (
        signatures,
        slots,
        block_times,
        names,
        ix_indexes,
        inner_indexes,
        args,
        accounts,
        data,
        is_inner,
    ) = decompose_instructions(instructions);

    let sql = build_instruction_sql(schema_name);

    let result = sqlx::query(&sql)
        .bind(&signatures)
        .bind(&slots)
        .bind(&block_times)
        .bind(&names)
        .bind(&ix_indexes)
        .bind(&inner_indexes)
        .bind(&args)
        .bind(&accounts)
        .bind(&data)
        .bind(&is_inner)
        .execute(&mut *conn)
        .await
        .map_err(|e| StorageError::WriteFailed(format!("instruction insert failed: {e}")))?;

    Ok(result.rows_affected())
}

/// Write a batch of accounts of the same type via UNNEST upsert.
#[tracing::instrument(
    name = "storage.write_accounts_batch",
    skip(conn, accounts, promoted),
    fields(schema_name = schema_name, table_name = table_name, count = accounts.len()),
    level = "debug",
    err(Display)
)]
async fn write_accounts_batch(
    conn: &mut sqlx::PgConnection,
    schema_name: &str,
    table_name: &str,
    accounts: &[&DecodedAccount],
    promoted: &[PromotedColumn],
) -> Result<u64, StorageError> {
    let pubkeys: Vec<&str> = accounts.iter().map(|a| a.pubkey.as_str()).collect();
    // Safety: Solana slots (~300M current) and single-account lamports (max ~6e17 for
    // total supply) are well within i64::MAX (9.2e18). Overflow would require >9.2B SOL
    // in one account, which exceeds total supply. Values are always preserved in JSONB.
    let slots: Vec<i64> = accounts.iter().map(|a| a.slot_updated as i64).collect();
    let lamports: Vec<i64> = accounts.iter().map(|a| a.lamports as i64).collect();
    let data_values: Vec<sqlx::types::Json<serde_json::Value>> = accounts
        .iter()
        .map(|a| sqlx::types::Json(a.data.clone()))
        .collect();

    let sql = build_account_upsert_sql(schema_name, table_name, promoted);

    let result = sqlx::query(&sql)
        .bind(&pubkeys)
        .bind(&slots)
        .bind(&lamports)
        .bind(&data_values)
        .execute(&mut *conn)
        .await
        .map_err(|e| {
            StorageError::WriteFailed(format!("account upsert failed for {table_name}: {e}"))
        })?;

    Ok(result.rows_affected())
}

/// INSERT...ON CONFLICT for checkpoint upsert.
#[tracing::instrument(
    name = "storage.update_checkpoint",
    skip(conn, signature),
    fields(schema_name = schema_name, stream = stream, slot = slot),
    level = "debug",
    err(Display)
)]
async fn update_checkpoint(
    conn: &mut sqlx::PgConnection,
    schema_name: &str,
    stream: &str,
    slot: u64,
    signature: Option<&str>,
) -> Result<(), StorageError> {
    let sql = format!(
        r#"INSERT INTO {}.{} ("stream", "last_slot", "last_signature", "updated_at")
VALUES ($1, $2, $3, NOW())
ON CONFLICT ("stream") DO UPDATE SET
    "last_slot" = EXCLUDED."last_slot",
    "last_signature" = EXCLUDED."last_signature",
    "updated_at" = NOW()"#,
        quote_ident(schema_name),
        quote_ident("_checkpoints"),
    );

    sqlx::query(&sql)
        .bind(stream)
        .bind(slot as i64)
        .bind(signature)
        .execute(&mut *conn)
        .await
        .map_err(|e| StorageError::CheckpointFailed(format!("checkpoint update failed: {e}")))?;

    Ok(())
}

/// Discover promoted columns for a table from information_schema.
///
/// Returns columns that exist in the table but are not common system columns.
/// Uses the pool (separate connection) so it doesn't interfere with an open transaction.
#[tracing::instrument(
    name = "storage.discover_promoted_columns",
    skip(pool),
    fields(schema_name = schema_name, table_name = table_name),
    level = "debug",
    err(Display)
)]
async fn discover_promoted_columns(
    pool: &PgPool,
    schema_name: &str,
    table_name: &str,
) -> Result<Vec<PromotedColumn>, StorageError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name, data_type FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = $2 \
         AND NOT (column_name = ANY($3::TEXT[])) \
         ORDER BY ordinal_position",
    )
    .bind(schema_name)
    .bind(table_name)
    .bind(COMMON_ACCOUNT_COLUMNS)
    .fetch_all(pool)
    .await
    .map_err(|e| StorageError::WriteFailed(format!("promoted column discovery failed: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|(name, dt)| PromotedColumn {
            column_name: name,
            data_type: dt,
        })
        .collect())
}

// --- Pure SQL builders (unit-testable) ---

/// Build the INSERT...UNNEST SQL for the _instructions table.
fn build_instruction_sql(schema_name: &str) -> String {
    format!(
        r#"INSERT INTO {}.{}
    ("signature", "slot", "block_time", "instruction_name",
     "instruction_index", "inner_index", "args", "accounts", "data", "is_inner_ix")
SELECT * FROM UNNEST(
    $1::TEXT[], $2::BIGINT[], $3::BIGINT[], $4::TEXT[],
    $5::SMALLINT[], $6::SMALLINT[], $7::JSONB[], $8::JSONB[], $9::JSONB[], $10::BOOLEAN[]
)
ON CONFLICT ("signature", "instruction_index", COALESCE("inner_index", -1)) DO NOTHING"#,
        quote_ident(schema_name),
        quote_ident("_instructions"),
    )
}

/// Build the account upsert SQL with CTE for promoted column extraction.
fn build_account_upsert_sql(
    schema_name: &str,
    table_name: &str,
    promoted: &[PromotedColumn],
) -> String {
    let schema_q = quote_ident(schema_name);
    let table_q = quote_ident(table_name);
    let qualified = format!("{schema_q}.{table_q}");

    let mut promoted_insert_cols = String::new();
    let mut promoted_select_exprs = String::new();
    let mut promoted_update_exprs = String::new();

    for col in promoted {
        let col_q = quote_ident(&col.column_name);
        let extract = build_promoted_extract_expr(col);

        let _ = write!(promoted_insert_cols, ", {col_q}");
        let _ = write!(promoted_select_exprs, ",\n    {extract}");
        let _ = write!(promoted_update_exprs, ",\n    {col_q} = EXCLUDED.{col_q}");
    }

    format!(
        r#"WITH raw AS (
    SELECT * FROM UNNEST($1::TEXT[], $2::BIGINT[], $3::BIGINT[], $4::JSONB[])
    AS t(pubkey, slot_updated, lamports, data)
)
INSERT INTO {qualified}
    ("pubkey", "slot_updated", "lamports", "data", "updated_at"{promoted_insert_cols})
SELECT
    pubkey, slot_updated, lamports, data, NOW(){promoted_select_exprs}
FROM raw
ON CONFLICT ("pubkey") DO UPDATE SET
    "slot_updated" = EXCLUDED."slot_updated",
    "lamports" = EXCLUDED."lamports",
    "data" = EXCLUDED."data",
    "updated_at" = NOW(){promoted_update_exprs}
WHERE EXCLUDED."slot_updated" > {qualified}."slot_updated""#
    )
}

/// Build a SQL expression to extract a promoted column from JSONB data.
///
/// Handles u64 overflow for BIGINT columns (values > i64::MAX -> NULL).
fn build_promoted_extract_expr(col: &PromotedColumn) -> String {
    // Escape single quotes in column name for safe SQL embedding
    let safe_name = col.column_name.replace('\'', "''");
    let json_extract = format!("data->>'{safe_name}'");

    match col.data_type.as_str() {
        "bigint" => {
            // u64 overflow guard: values > i64::MAX stored as NULL in promoted column
            format!(
                "CASE WHEN ({json_extract})::NUMERIC > 9223372036854775807 \
                 THEN NULL ELSE ({json_extract})::BIGINT END"
            )
        }
        "integer" => format!("({json_extract})::INTEGER"),
        "smallint" => format!("({json_extract})::SMALLINT"),
        "boolean" => format!("({json_extract})::BOOLEAN"),
        "text" | "character varying" => json_extract,
        "double precision" => format!("({json_extract})::DOUBLE PRECISION"),
        "real" => format!("({json_extract})::REAL"),
        "numeric" => format!("({json_extract})::NUMERIC"),
        // BYTEA extraction from JSON is complex — leave NULL
        _ => "NULL".to_string(),
    }
}

// --- Pure helpers (unit-testable) ---

/// Convert u64 to i64, returning None for values that exceed i64::MAX.
///
/// Used for promoted u64 IDL fields: values exceeding i64::MAX are stored as
/// NULL in promoted columns but preserved in the JSONB `data` column.
/// Currently overflow is handled in SQL; this helper is available for Rust-side use.
#[allow(dead_code)]
fn safe_u64_to_i64(value: u64) -> Option<i64> {
    if value <= i64::MAX as u64 {
        Some(value as i64)
    } else {
        None
    }
}

/// Decompose instructions into column vectors for UNNEST binding.
#[allow(clippy::type_complexity)]
fn decompose_instructions(
    instructions: &[DecodedInstruction],
) -> (
    Vec<&str>,                                 // signatures
    Vec<i64>,                                  // slots
    Vec<Option<i64>>,                          // block_times
    Vec<&str>,                                 // instruction_names
    Vec<i16>,                                  // instruction_indexes
    Vec<Option<i16>>,                          // inner_indexes
    Vec<sqlx::types::Json<serde_json::Value>>, // args (JSONB)
    Vec<sqlx::types::Json<serde_json::Value>>, // accounts (JSONB)
    Vec<sqlx::types::Json<serde_json::Value>>, // data (JSONB)
    Vec<bool>,                                 // is_inner_ix
) {
    let len = instructions.len();

    let mut signatures = Vec::with_capacity(len);
    let mut slots = Vec::with_capacity(len);
    let mut block_times = Vec::with_capacity(len);
    let mut names = Vec::with_capacity(len);
    let mut ix_indexes = Vec::with_capacity(len);
    let mut inner_indexes = Vec::with_capacity(len);
    let mut args = Vec::with_capacity(len);
    let mut accounts = Vec::with_capacity(len);
    let mut data = Vec::with_capacity(len);
    let mut is_inner = Vec::with_capacity(len);

    for ix in instructions {
        signatures.push(ix.signature.as_str());
        slots.push(ix.slot as i64);
        block_times.push(ix.block_time);
        names.push(ix.instruction_name.as_str());
        ix_indexes.push(ix.instruction_index as i16);
        inner_indexes.push(ix.inner_index.map(|i| i as i16));
        args.push(sqlx::types::Json(ix.args.clone()));
        accounts.push(sqlx::types::Json(serde_json::json!(&ix.accounts)));
        // data column = same as args (DecodedInstruction has no separate data field)
        data.push(sqlx::types::Json(ix.args.clone()));
        is_inner.push(ix.inner_index.is_some());
    }

    (
        signatures,
        slots,
        block_times,
        names,
        ix_indexes,
        inner_indexes,
        args,
        accounts,
        data,
        is_inner,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // Send-safety compile-time checks (Story 6.4 AC9)
    //
    // The legacy `_assert_*` helpers below (pre-Story-6.4) were never
    // monomorphized because they were free functions inside `#[cfg(test)]`
    // never called from any `#[test]`. They are kept for historical context
    // but the actual Send check now lives in `test_write_block_future_is_send`
    // which uses the `fn _check` + `let _: fn = _check;` fn-pointer cast
    // pattern that forces monomorphization — see `src/idl/mod.rs` test module
    // doc comment and `_bmad-output/problem-solution-2026-04-06.md` for the
    // underlying !Send lesson.
    // -----------------------------------------------------------------------

    fn _assert_send<T: Send>(_: &T) {}

    // Kept (pre-6.4, tautological): StorageWriter itself must be Send.
    fn _assert_writer_send(w: &StorageWriter) {
        _assert_send(w);
    }

    // Kept (pre-6.4, never actually monomorphized — see module comment above).
    fn _assert_read_checkpoint_future_send(w: &StorageWriter) {
        let fut = w.read_checkpoint("s", "stream");
        _assert_send(&fut);
    }

    #[test]
    fn test_write_block_future_is_send() {
        fn _check(w: &StorageWriter) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = w.write_block("s", "stream", &[], &[], 0, None);
            _require_send(&fut);
        }
        let _: fn(&StorageWriter) = _check;
    }

    #[test]
    fn test_read_checkpoint_future_is_send() {
        fn _check(w: &StorageWriter) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = w.read_checkpoint("s", "stream");
            _require_send(&fut);
        }
        let _: fn(&StorageWriter) = _check;
    }

    // -- safe_u64_to_i64 tests --

    #[test]
    fn safe_u64_to_i64_zero() {
        assert_eq!(safe_u64_to_i64(0), Some(0));
    }

    #[test]
    fn safe_u64_to_i64_normal_value() {
        assert_eq!(safe_u64_to_i64(1_000_000), Some(1_000_000));
    }

    #[test]
    fn safe_u64_to_i64_at_max() {
        assert_eq!(safe_u64_to_i64(i64::MAX as u64), Some(i64::MAX));
    }

    #[test]
    fn safe_u64_to_i64_overflow() {
        assert_eq!(safe_u64_to_i64(i64::MAX as u64 + 1), None);
    }

    #[test]
    fn safe_u64_to_i64_u64_max() {
        assert_eq!(safe_u64_to_i64(u64::MAX), None);
    }

    // -- Column vector decomposition tests --

    fn sample_instruction(sig: &str, slot: u64, inner: Option<u8>) -> DecodedInstruction {
        DecodedInstruction {
            signature: sig.to_string(),
            slot,
            block_time: Some(1_700_000_000),
            instruction_name: "transfer".to_string(),
            args: json!({"amount": 1000}),
            program_id: "Prog1".to_string(),
            accounts: vec!["Acc1".to_string(), "Acc2".to_string()],
            instruction_index: 0,
            inner_index: inner,
        }
    }

    #[test]
    fn decompose_instructions_correct_lengths() {
        let ixs = vec![
            sample_instruction("sig1", 100, None),
            sample_instruction("sig2", 101, Some(2)),
        ];

        let (sigs, slots, bt, names, ixi, inner, args, accs, data, is_inner) =
            decompose_instructions(&ixs);

        assert_eq!(sigs.len(), 2);
        assert_eq!(slots.len(), 2);
        assert_eq!(bt.len(), 2);
        assert_eq!(names.len(), 2);
        assert_eq!(ixi.len(), 2);
        assert_eq!(inner.len(), 2);
        assert_eq!(args.len(), 2);
        assert_eq!(accs.len(), 2);
        assert_eq!(data.len(), 2);
        assert_eq!(is_inner.len(), 2);
    }

    #[test]
    fn decompose_instructions_correct_values() {
        let ixs = vec![
            sample_instruction("sig1", 100, None),
            sample_instruction("sig2", 101, Some(2)),
        ];

        let (sigs, slots, _bt, _names, _ixi, inner, _args, _accs, _data, is_inner) =
            decompose_instructions(&ixs);

        assert_eq!(sigs, vec!["sig1", "sig2"]);
        assert_eq!(slots, vec![100i64, 101i64]);
        assert_eq!(inner, vec![None, Some(2i16)]);
        assert_eq!(is_inner, vec![false, true]);
    }

    #[test]
    fn decompose_instructions_data_equals_args() {
        let ixs = vec![sample_instruction("sig1", 100, None)];
        let (_s, _sl, _bt, _n, _ix, _in, args, _accs, data, _is) = decompose_instructions(&ixs);
        assert_eq!(args[0].0, data[0].0);
    }

    #[test]
    fn decompose_instructions_accounts_as_json_array() {
        let ixs = vec![sample_instruction("sig1", 100, None)];
        let (_s, _sl, _bt, _n, _ix, _in, _args, accs, _data, _is) = decompose_instructions(&ixs);
        let expected = json!(["Acc1", "Acc2"]);
        assert_eq!(accs[0].0, expected);
    }

    // -- Account grouping tests --

    fn sample_account(pubkey: &str, account_type: &str) -> DecodedAccount {
        DecodedAccount {
            pubkey: pubkey.to_string(),
            slot_updated: 100,
            lamports: 1000,
            data: json!({"owner": "abc"}),
            account_type: account_type.to_string(),
            program_id: "prog".to_string(),
        }
    }

    #[test]
    fn account_grouping_by_type() {
        let accounts = vec![
            sample_account("pk1", "TokenAccount"),
            sample_account("pk2", "Mint"),
            sample_account("pk3", "TokenAccount"),
        ];

        let mut grouped: HashMap<&str, Vec<&DecodedAccount>> = HashMap::new();
        for account in &accounts {
            grouped
                .entry(account.account_type.as_str())
                .or_default()
                .push(account);
        }

        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped["TokenAccount"].len(), 2);
        assert_eq!(grouped["Mint"].len(), 1);
    }

    #[test]
    fn account_table_name_sanitized() {
        assert_eq!(sanitize_identifier("TokenAccount"), "tokenaccount");
        assert_eq!(sanitize_identifier("my_account"), "my_account");
        assert_eq!(sanitize_identifier("123Invalid"), "_123invalid");
    }

    // -- Promoted column extraction tests --

    #[test]
    fn promoted_extract_bigint_has_overflow_guard() {
        let col = PromotedColumn {
            column_name: "amount".to_string(),
            data_type: "bigint".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("9223372036854775807"), "missing i64::MAX");
        assert!(expr.contains("CASE WHEN"), "missing overflow guard");
        assert!(expr.contains("::BIGINT"), "missing cast");
        assert!(expr.contains("THEN NULL"), "missing NULL fallback");
    }

    #[test]
    fn promoted_extract_text_no_cast() {
        let col = PromotedColumn {
            column_name: "owner".to_string(),
            data_type: "text".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert_eq!(expr, "data->>'owner'");
    }

    #[test]
    fn promoted_extract_boolean() {
        let col = PromotedColumn {
            column_name: "is_frozen".to_string(),
            data_type: "boolean".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("::BOOLEAN"));
        assert!(expr.contains("data->>'is_frozen'"));
    }

    #[test]
    fn promoted_extract_integer() {
        let col = PromotedColumn {
            column_name: "count".to_string(),
            data_type: "integer".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("::INTEGER"));
    }

    #[test]
    fn promoted_extract_smallint() {
        let col = PromotedColumn {
            column_name: "decimals".to_string(),
            data_type: "smallint".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("::SMALLINT"));
    }

    #[test]
    fn promoted_extract_double_precision() {
        let col = PromotedColumn {
            column_name: "rate".to_string(),
            data_type: "double precision".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("::DOUBLE PRECISION"));
    }

    #[test]
    fn promoted_extract_numeric() {
        let col = PromotedColumn {
            column_name: "big_num".to_string(),
            data_type: "numeric".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("::NUMERIC"));
    }

    #[test]
    fn promoted_extract_unknown_type_returns_null() {
        let col = PromotedColumn {
            column_name: "weird".to_string(),
            data_type: "bytea".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert_eq!(expr, "NULL");
    }

    #[test]
    fn promoted_extract_escapes_single_quotes() {
        let col = PromotedColumn {
            column_name: "it's_name".to_string(),
            data_type: "text".to_string(),
        };
        let expr = build_promoted_extract_expr(&col);
        assert!(expr.contains("it''s_name"), "single quote not escaped");
    }

    // -- SQL generation tests --

    #[test]
    fn instruction_sql_has_correct_structure() {
        let sql = build_instruction_sql("test_schema");
        assert!(sql.contains(r#""test_schema"."_instructions""#));
        assert!(sql.contains("INSERT INTO"));
        assert!(sql.contains("UNNEST"));
        assert!(sql.contains("$1::TEXT[]"));
        assert!(sql.contains("$10::BOOLEAN[]"));
        assert!(sql.contains("ON CONFLICT"));
        assert!(sql.contains(r#"COALESCE("inner_index", -1)"#));
        assert!(sql.contains("DO NOTHING"));
    }

    #[test]
    fn account_upsert_sql_no_promoted() {
        let sql = build_account_upsert_sql("my_schema", "tokenaccount", &[]);
        assert!(sql.contains(r#""my_schema"."tokenaccount""#));
        assert!(sql.contains("WITH raw AS"));
        assert!(sql.contains("UNNEST($1::TEXT[], $2::BIGINT[], $3::BIGINT[], $4::JSONB[])"));
        assert!(sql.contains("NOW()"));
        assert!(sql.contains(r#"ON CONFLICT ("pubkey")"#));
        assert!(
            sql.contains(r#"EXCLUDED."slot_updated" > "my_schema"."tokenaccount"."slot_updated""#)
        );
        // No promoted columns
        assert!(!sql.contains("CASE WHEN"));
    }

    #[test]
    fn account_upsert_sql_with_promoted_columns() {
        let promoted = vec![
            PromotedColumn {
                column_name: "owner".to_string(),
                data_type: "text".to_string(),
            },
            PromotedColumn {
                column_name: "amount".to_string(),
                data_type: "bigint".to_string(),
            },
        ];
        let sql = build_account_upsert_sql("s", "token", &promoted);

        // INSERT columns include promoted
        assert!(sql.contains(r#""owner""#));
        assert!(sql.contains(r#""amount""#));
        // SELECT includes extraction expressions
        assert!(sql.contains("data->>'owner'"));
        assert!(sql.contains("CASE WHEN"));
        // UPDATE includes promoted columns
        assert!(sql.contains(r#""owner" = EXCLUDED."owner""#));
        assert!(sql.contains(r#""amount" = EXCLUDED."amount""#));
    }

    #[test]
    fn instruction_sql_schema_name_quoted() {
        let sql = build_instruction_sql("special-schema");
        assert!(sql.contains(r#""special-schema""#));
    }
}
