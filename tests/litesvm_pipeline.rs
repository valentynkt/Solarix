// Story 6.5 AC6: Decoder → Writer → Checkpoint seam test.
//
// PATH SELECTION (mandatory module-doc preamble — see story 6.5 AC6):
//
//   This test takes **Path A** (REQUIRED unless reviewer-escalated to
//   Path B). It does NOT deploy a real BPF program inside LiteSVM, does
//   NOT commit a `.so` binary, and does NOT send any transactions through
//   `LiteSVM::send_transaction`. Instead, it constructs synthetic
//   Borsh-encoded instruction data using the helpers in
//   `tests/common/decoder_fixtures.rs` (committed in Story 6.4), passes
//   those bytes through the production `ChainparserDecoder::decode_instruction`,
//   and writes the resulting `DecodedInstruction` through `StorageWriter`
//   exactly the way `process_chunk` does in production.
//
//   Path A exercises the *seam between* the decoder, the writer, and the
//   checkpoint cursor — which is precisely the gap the Sprint-4 e2e gate
//   surfaced (the three critical bugs all hid in interaction seams that
//   268 unit tests never touched). It does NOT depend on a real BPF
//   binary, which is what keeps the repo's git hygiene clean (Path B
//   would commit an opaque `.so` and require reviewer sign-off).
//
//   `litesvm` ends up unused in this test body — only the import is
//   pulled in. The dependency stays in `[dev-dependencies]` because a
//   future story (orchestrator-against-LiteSVM, filed in deferred-work)
//   will need it. We instantiate `LiteSVM::new()` once at the bottom as
//   a smoke check that the dep tree resolved cleanly. If the dev needs
//   to remove the unused-import here in a future cleanup, that's fine.

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;

use solarix::decoder::{ChainparserDecoder, SolarixDecoder};
use solarix::idl::IdlManager;
use solarix::registry::ProgramRegistry;
use solarix::storage::writer::StorageWriter;
use solarix::types::DecodedAccount;

mod common;
use common::decoder_fixtures::{borsh_u64, compute_instruction_discriminator};
use common::postgres::with_postgres;

const SIMPLE_IDL_JSON: &str = include_str!("fixtures/idls/simple_v030.json");
const PROGRAM_ID: &str = "LiteSvm111111111111111111111111111111111111";

#[tokio::test]
async fn decoder_writer_checkpoint_seam_path_a() {
    with_postgres(|pool| async move {
        // Smoke-instantiate LiteSVM so the dep tree is exercised at runtime.
        // The seam test does not actually push transactions through it
        // (Path A), but instantiating LiteSVM proves the dep resolves
        // and validates the future-orchestrator-against-LiteSVM story can
        // build on this.
        let _svm = litesvm::LiteSVM::new();

        // Register the simple_v030 IDL so the DB schema exists.
        let idl_manager = IdlManager::new("http://localhost:8899".to_string());
        let mut registry = ProgramRegistry::new(idl_manager);
        let data = registry
            .prepare_registration(PROGRAM_ID.to_string(), Some(SIMPLE_IDL_JSON.to_string()))
            .expect("prepare_registration");
        let schema_name = data.schema_name.clone();
        let info = ProgramRegistry::commit_registration(pool.clone(), data)
            .await
            .expect("commit_registration");
        assert_eq!(info.status, "schema_created");

        // Synthesize an `initialize` instruction payload via the production
        // discriminator + Borsh encoder. The simple_v030 IDL declares
        // `initialize(value: u64)`, so the wire format is
        // `[8-byte discriminator][8-byte little-endian u64]`.
        let disc = compute_instruction_discriminator("initialize");
        let mut wire = disc.to_vec();
        wire.extend_from_slice(&borsh_u64(424_242));

        // Parse the IDL once for the decoder. We deliberately re-use the
        // *same* JSON string the registry was fed so any drift between
        // `IdlManager::upload_idl`'s parser and the decoder's expectations
        // would surface here.
        let idl: anchor_lang_idl_spec::Idl =
            serde_json::from_str(SIMPLE_IDL_JSON).expect("simple_v030 fixture parses");

        let decoder = ChainparserDecoder::new();
        let writer = StorageWriter::new(pool.clone());

        // Decode → write loop ×10 advancing the slot cursor each round.
        for slot in 1u64..=10 {
            // Production decoder produces a minimal DecodedInstruction —
            // we patch in the slot/signature exactly the way `process_chunk`
            // does after enrichment from the block envelope.
            let mut decoded = decoder
                .decode_instruction(PROGRAM_ID, &wire, &idl)
                .expect("decode_instruction should round-trip the synthetic payload");
            decoded.slot = slot;
            decoded.signature = format!("svm_sig_{slot:02}");
            decoded.block_time = Some(1_700_000_000 + slot as i64);

            assert_eq!(decoded.instruction_name, "initialize");
            assert_eq!(decoded.args["value"], 424_242);

            let result = writer
                .write_block(
                    &schema_name,
                    "backfill",
                    &[decoded],
                    &[],
                    slot,
                    Some(&format!("svm_sig_{slot:02}")),
                )
                .await
                .expect("writer.write_block should accept the decoded instruction");
            assert_eq!(result.instructions_written, 1, "round {slot}");
        }

        // Checkpoint should have advanced to slot 10. read_checkpoint
        // returns Result<Option<CheckpointInfo>, _> — None means "no
        // checkpoint row", which would be a bug here.
        let cp = writer
            .read_checkpoint(&schema_name, "backfill")
            .await
            .expect("read_checkpoint should not error");
        let cp = cp.expect("checkpoint row must exist after 10 writes");
        assert_eq!(cp.last_slot, 10, "checkpoint should advance to last write");

        // Kill-restart sub-step: drop the in-memory writer, recreate from a
        // fresh `StorageWriter::new(pool.clone())`, read the checkpoint again
        // — must return the SAME persisted slot. This pins the contract that
        // the checkpoint is durable across writer restarts (the persistence
        // layer is what `process_chunk` relies on after a pipeline crash).
        drop(writer);
        let writer2 = StorageWriter::new(pool.clone());
        let cp2 = writer2
            .read_checkpoint(&schema_name, "backfill")
            .await
            .expect("read_checkpoint after restart");
        let cp2 = cp2.expect("checkpoint must persist across writer restarts");
        assert_eq!(
            cp2.last_slot, 10,
            "kill-restart must read the same persisted slot"
        );

        // Optional account-upsert sub-test (encouraged by AC6).
        // Insert at slot 11, re-insert at slot 12 (higher → must overwrite),
        // re-insert at slot 10 (lower → must NOT overwrite).
        let acct = DecodedAccount {
            pubkey: "SvmAcct1111111111111111111111111111111111111".to_string(),
            slot_updated: 11,
            lamports: 1_500_000,
            data: json!({ "value": 100u64 }),
            account_type: "DataAccount".to_string(),
            program_id: PROGRAM_ID.to_string(),
        };
        writer2
            .write_block(&schema_name, "backfill", &[], &[acct.clone()], 11, None)
            .await
            .expect("first account upsert");

        let mut acct12 = acct.clone();
        acct12.slot_updated = 12;
        acct12.data = json!({ "value": 200u64 });
        writer2
            .write_block(&schema_name, "backfill", &[], &[acct12], 12, None)
            .await
            .expect("higher-slot account upsert");

        let mut acct_old = acct.clone();
        acct_old.slot_updated = 5;
        acct_old.data = json!({ "value": 999u64 });
        writer2
            .write_block(&schema_name, "backfill", &[], &[acct_old], 5, None)
            .await
            .expect("older-slot upsert call returns Ok (no error)");

        let row: (i64, i64) = sqlx::query_as(&format!(
            r#"SELECT "slot_updated", "value" FROM "{schema_name}"."dataaccount"
               WHERE "pubkey" = $1"#
        ))
        .bind(&acct.pubkey)
        .fetch_one(&pool)
        .await
        .expect("dataaccount row should exist");
        assert_eq!(row.0, 12, "monotonic upsert: latest slot wins");
        assert_eq!(row.1, 200, "monotonic upsert: latest value wins");
    })
    .await;
}
