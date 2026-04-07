// Test-only file: allow panic/unwrap/expect so this file does not increase
// the `cargo clippy --all-targets` error count vs the pre-Story-6.4
// baseline (Story 6.4 AC10).
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

// Schema generator snapshot tests for Story 6.4 (AC5).
//
// This file captures the output of `build_ddl_statements` against three
// fixture IDLs so that any accidental change to DDL generation is caught
// at review time:
//
//   1. tests/fixtures/idls/simple_v030.json        — minimal v0.30 IDL
//   2. tests/fixtures/idls/all_types.json          — every primitive + container + enum + alias + COption
//   3. tests/fixtures/idls/reserved_collision_v030.json — reserved-column collision, digit-first,
//                                                         SQL reserved words
//
// Plus inline snapshots of `sanitize_identifier` edge cases.
//
// ## Review workflow
//
// When a DDL generator change is intentional, review the diff against the
// snapshot files in `tests/snapshots/` before accepting:
//
// ```
// cargo insta review
// ```
//
// Or, for a one-off accept without interactive review:
//
// ```
// INSTA_UPDATE=always cargo test --test schema_snapshots
// ```
//
// A reviewer MUST run `cargo insta review` before approving a PR that
// touches snapshot files — the snapshot diff is the contract.

use anchor_lang_idl_spec::Idl;

use solarix::storage::schema::{build_ddl_statements, sanitize_identifier};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_fixture(bytes: &str) -> Idl {
    serde_json::from_str(bytes).expect("fixture IDL must parse")
}

// ---------------------------------------------------------------------------
// Whole-IDL DDL snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_simple_v030_ddl() {
    let idl = parse_fixture(include_str!("fixtures/idls/simple_v030.json"));
    // Stable schema name — the snapshot would churn if we used a timestamp
    // or randomized suffix. The prefix `test_snapshot_` makes intent obvious.
    let schema = "test_snapshot_simple_v030";
    let stmts = build_ddl_statements(&idl, schema);
    let joined = stmts.join("\n\n");
    insta::assert_snapshot!("simple_v030_ddl", joined);
}

#[test]
fn snapshot_all_types_ddl() {
    // The `all_types.json` fixture exercises every primitive, every
    // container, nested Defined structs, enums with named + tuple payloads,
    // a `Type { alias }` node, a COption, byte arrays, and accounts with
    // mixed promoted / non-promoted fields. This is the most comprehensive
    // snapshot target in the suite.
    let idl = parse_fixture(include_str!("fixtures/idls/all_types.json"));
    let schema = "test_snapshot_all_types";
    let stmts = build_ddl_statements(&idl, schema);
    let joined = stmts.join("\n\n");
    insta::assert_snapshot!("all_types_ddl", joined);
}

#[test]
fn snapshot_reserved_collision_ddl() {
    // Locks in the reserved-column collision handling: fields named `pubkey`,
    // `data`, `slot_updated`, `lamports`, `is_closed`, `updated_at`, and
    // `write_version` MUST NOT appear as duplicate promoted columns.
    // Also captures how `sanitize_identifier` handles digit-first account
    // names and SQL-reserved field names.
    let idl = parse_fixture(include_str!("fixtures/idls/reserved_collision_v030.json"));
    let schema = "test_snapshot_reserved_collision";
    let stmts = build_ddl_statements(&idl, schema);
    let joined = stmts.join("\n\n");
    insta::assert_snapshot!("reserved_collision_ddl", joined);
}

// ---------------------------------------------------------------------------
// sanitize_identifier edge-case inline snapshots
// ---------------------------------------------------------------------------
//
// These overlap with the unit tests inside `src/storage/schema.rs` — that's
// intentional. The unit tests there assert specific output strings; these
// inline snapshots make the entire sanitization contract visible to a reviewer
// without requiring them to run `cargo test` locally. If `sanitize_identifier`
// changes, both sets fail and the reviewer immediately sees the behavior
// diff alongside the code change.

#[test]
fn inline_sanitize_digit_first() {
    insta::assert_snapshot!(sanitize_identifier("123program"), @"_123program");
}

#[test]
fn inline_sanitize_reserved_word_select() {
    // `select` is a SQL reserved word but NOT altered by sanitize_identifier
    // — quoting happens at a later layer via `quote_ident`. This snapshot
    // locks that boundary in place so a future "fix" that tries to rewrite
    // reserved words at sanitize time is caught.
    insta::assert_snapshot!(sanitize_identifier("select"), @"select");
}

#[test]
fn inline_sanitize_unicode_stripped() {
    insta::assert_snapshot!(sanitize_identifier("café"), @"caf");
}

#[test]
fn inline_sanitize_cjk_to_unnamed() {
    insta::assert_snapshot!(sanitize_identifier("程序"), @"_unnamed");
}

#[test]
fn inline_sanitize_empty_to_unnamed() {
    insta::assert_snapshot!(sanitize_identifier(""), @"_unnamed");
}

#[test]
fn inline_sanitize_truncates_to_63_bytes_on_char_boundary() {
    let long = "a".repeat(100);
    let result = sanitize_identifier(&long);
    assert_eq!(result.len(), 63);
    insta::assert_snapshot!(result, @"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
}

#[test]
fn inline_sanitize_strips_special_chars() {
    insta::assert_snapshot!(sanitize_identifier("hello-world!@#$%"), @"helloworld");
}

#[test]
fn inline_sanitize_mixedcase_to_lower() {
    insta::assert_snapshot!(sanitize_identifier("MyProgram"), @"myprogram");
}
