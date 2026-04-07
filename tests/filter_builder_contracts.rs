// Filter builder SQL contract matrix for Story 6.4 (AC6).
//
// This test file asserts the SQL shape produced by `build_query` for every
// (FilterOp × ColumnExpr × pg_type) combination that the production code
// path emits. It is a pure-text test against `sqlx::QueryBuilder::sql()`:
// no database connection is required, and no sqlx executor is invoked.
//
// Why this file exists:
//
//   The Sprint-4 e2e gate surfaced a production bug where `slot_gt=N` on a
//   promoted BIGINT column produced SQL that PostgreSQL rejected with
//   `operator does not exist: bigint > text`. The unit tests in
//   `src/storage/queries.rs` only asserted `sql.contains("WHERE")` and
//   never pinned the `::BIGINT` cast that makes the operator valid.
//
//   This file adds the operator × type matrix at the integration-test layer
//   so the cast is observable from outside the crate, and so adding a new
//   FilterOp variant without updating build_query immediately fails a visible
//   contract test.
//
// Related:
//   - `src/storage/queries.rs:118-128` — the `::BIGINT` cast (commit 243a0de)
//   - `_bmad-output/implementation-artifacts/e2e-test-results.md` — bug reproduction
//   - Companion file `tests/filter_sql_exec.rs` — `#[ignore]` stub for the
//     testcontainers-backed integration tests that will land in Story 6.5.

use anchor_lang_idl_spec::{IdlField, IdlType};

use solarix::api::filters::{
    parse_filters, resolve_filters, ColumnExpr, FilterContext, FilterOp, ParsedFilter,
    ResolvedFilter,
};
use solarix::api::ApiError;
use solarix::storage::queries::{build_query, QueryTarget};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn accounts_target() -> QueryTarget {
    QueryTarget::Accounts {
        schema: "s".to_string(),
        table: "t".to_string(),
    }
}

fn instructions_target() -> QueryTarget {
    QueryTarget::Instructions {
        schema: "s".to_string(),
    }
}

fn promoted_filter(column: &str, pg_type: &str, op: FilterOp, value: &str) -> ResolvedFilter {
    ResolvedFilter {
        column_expr: ColumnExpr::Promoted {
            column: column.to_string(),
            pg_type: Some(pg_type.to_string()),
        },
        op,
        value: value.to_string(),
    }
}

fn jsonb_filter(field: &str, op: FilterOp, value: &str) -> ResolvedFilter {
    ResolvedFilter {
        column_expr: ColumnExpr::Jsonb {
            field: field.to_string(),
        },
        op,
        value: value.to_string(),
    }
}

fn build_sql(target: &QueryTarget, filters: &[ResolvedFilter]) -> String {
    build_query(target, filters, 50, 0).sql().to_string()
}

// ---------------------------------------------------------------------------
// Promoted BIGINT × comparison operators (the `bigint > text` regression)
// ---------------------------------------------------------------------------

#[test]
fn promoted_bigint_gt_casts_bind_to_bigint() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter("slot", "BIGINT", FilterOp::Gt, "100")],
    );
    assert!(
        sql.contains(r#"WHERE "slot" > "#),
        "expected `WHERE \"slot\" > `, got: {sql}"
    );
    assert!(
        sql.contains("::BIGINT"),
        "missing ::BIGINT cast — this is the `bigint > text` regression: {sql}"
    );
}

#[test]
fn promoted_bigint_gte_casts_bind_to_bigint() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter("slot", "BIGINT", FilterOp::Gte, "100")],
    );
    assert!(sql.contains(r#"WHERE "slot" >= "#));
    assert!(sql.contains("::BIGINT"));
}

#[test]
fn promoted_bigint_lt_casts_bind_to_bigint() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter("slot", "BIGINT", FilterOp::Lt, "100")],
    );
    assert!(sql.contains(r#"WHERE "slot" < "#));
    assert!(sql.contains("::BIGINT"));
}

#[test]
fn promoted_bigint_lte_casts_bind_to_bigint() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter("slot", "BIGINT", FilterOp::Lte, "100")],
    );
    assert!(sql.contains(r#"WHERE "slot" <= "#));
    assert!(sql.contains("::BIGINT"));
}

#[test]
fn promoted_bigint_eq_casts_bind_to_bigint() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter("slot", "BIGINT", FilterOp::Eq, "100")],
    );
    assert!(sql.contains(r#"WHERE "slot" = "#));
    assert!(sql.contains("::BIGINT"));
}

#[test]
fn promoted_bigint_ne_casts_bind_to_bigint() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter("slot", "BIGINT", FilterOp::Ne, "100")],
    );
    assert!(sql.contains(r#"WHERE "slot" != "#));
    assert!(sql.contains("::BIGINT"));
}

// ---------------------------------------------------------------------------
// Promoted SMALLINT
// ---------------------------------------------------------------------------

#[test]
fn promoted_smallint_eq_casts_bind_to_smallint() {
    let t = instructions_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "instruction_index",
            "SMALLINT",
            FilterOp::Eq,
            "3",
        )],
    );
    assert!(sql.contains(r#""instruction_index" = "#));
    assert!(sql.contains("::SMALLINT"));
}

#[test]
fn promoted_smallint_gt_casts_bind_to_smallint() {
    let t = instructions_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "instruction_index",
            "SMALLINT",
            FilterOp::Gt,
            "0",
        )],
    );
    assert!(sql.contains("::SMALLINT"));
    assert!(sql.contains(r#""instruction_index" > "#));
}

// ---------------------------------------------------------------------------
// Promoted TEXT
// ---------------------------------------------------------------------------

#[test]
fn promoted_text_eq_casts_bind_to_text() {
    let t = instructions_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "instruction_name",
            "TEXT",
            FilterOp::Eq,
            "transfer",
        )],
    );
    assert!(sql.contains(r#""instruction_name" = "#));
    assert!(sql.contains("::TEXT"));
}

#[test]
fn promoted_text_in_with_values_uses_text_any() {
    let t = instructions_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "instruction_name",
            "TEXT",
            FilterOp::In,
            "a,b,c",
        )],
    );
    assert!(
        sql.contains(r#""instruction_name"::text = ANY("#),
        "expected text ANY(), got: {sql}"
    );
}

#[test]
fn promoted_text_in_empty_value_collapses_to_false() {
    let t = instructions_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "instruction_name",
            "TEXT",
            FilterOp::In,
            "",
        )],
    );
    assert!(
        sql.contains("FALSE"),
        "empty _in should collapse to FALSE, got: {sql}"
    );
}

// ---------------------------------------------------------------------------
// Promoted BOOLEAN
// ---------------------------------------------------------------------------

#[test]
fn promoted_boolean_eq_casts_bind_to_boolean() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "is_closed",
            "BOOLEAN",
            FilterOp::Eq,
            "true",
        )],
    );
    assert!(sql.contains(r#""is_closed" = "#));
    assert!(sql.contains("::BOOLEAN"));
}

#[test]
fn promoted_boolean_ne_casts_bind_to_boolean() {
    let t = accounts_target();
    let sql = build_sql(
        &t,
        &[promoted_filter(
            "is_closed",
            "BOOLEAN",
            FilterOp::Ne,
            "false",
        )],
    );
    assert!(sql.contains("::BOOLEAN"));
}

// ---------------------------------------------------------------------------
// JSONB field filters
// ---------------------------------------------------------------------------

#[test]
fn jsonb_eq_uses_containment() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("amount", FilterOp::Eq, "42")]);
    assert!(
        sql.contains(r#""data" @> "#),
        "expected @> containment, got: {sql}"
    );
}

#[test]
fn jsonb_contains_uses_containment() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("meta", FilterOp::Contains, "xyz")]);
    assert!(sql.contains(r#""data" @> "#));
}

#[test]
fn jsonb_gt_uses_text_extraction() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("score", FilterOp::Gt, "50")]);
    assert!(
        sql.contains(r#"("data"->>'score') > "#),
        "expected text extraction operator, got: {sql}"
    );
}

#[test]
fn jsonb_gte_uses_text_extraction() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("score", FilterOp::Gte, "50")]);
    assert!(sql.contains(r#"("data"->>'score') >= "#));
}

#[test]
fn jsonb_lt_uses_text_extraction() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("score", FilterOp::Lt, "50")]);
    assert!(sql.contains(r#"("data"->>'score') < "#));
}

#[test]
fn jsonb_lte_uses_text_extraction() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("score", FilterOp::Lte, "50")]);
    assert!(sql.contains(r#"("data"->>'score') <= "#));
}

#[test]
fn jsonb_ne_uses_text_extraction() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("score", FilterOp::Ne, "50")]);
    assert!(sql.contains(r#"("data"->>'score') != "#));
}

#[test]
fn jsonb_in_with_values_uses_any_extraction() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("tag", FilterOp::In, "a,b,c")]);
    assert!(
        sql.contains(r#""data"->>'tag' = ANY("#),
        "expected JSONB extraction + ANY, got: {sql}"
    );
}

#[test]
fn jsonb_in_empty_value_collapses_to_false() {
    let t = accounts_target();
    let sql = build_sql(&t, &[jsonb_filter("tag", FilterOp::In, "")]);
    assert!(
        sql.contains("FALSE"),
        "empty _in should collapse, got: {sql}"
    );
}

// ---------------------------------------------------------------------------
// Error cases: _contains on promoted column, unknown fields
// ---------------------------------------------------------------------------

fn sample_idl_fields() -> Vec<IdlField> {
    vec![
        IdlField {
            name: "amount".to_string(),
            docs: vec![],
            ty: IdlType::U64,
        },
        IdlField {
            name: "owner".to_string(),
            docs: vec![],
            ty: IdlType::Pubkey,
        },
        IdlField {
            name: "metadata_vec".to_string(),
            docs: vec![],
            ty: IdlType::Vec(Box::new(IdlType::U8)),
        },
    ]
}

#[test]
fn resolve_rejects_contains_on_promoted_idl_field() {
    let parsed = vec![ParsedFilter {
        field: "amount".to_string(),
        op: FilterOp::Contains,
        value: "foo".to_string(),
    }];
    let err = resolve_filters(&parsed, &sample_idl_fields(), &[], FilterContext::Accounts)
        .expect_err("_contains on a BIGINT promoted column must fail");
    match err {
        ApiError::InvalidFilter { message, .. } => {
            assert!(message.contains("_contains"), "{message}");
            assert!(message.contains("amount"), "{message}");
        }
        other => panic!("expected InvalidFilter, got {other:?}"),
    }
}

#[test]
fn resolve_rejects_contains_on_fixed_promoted_column() {
    let parsed = vec![ParsedFilter {
        field: "slot".to_string(),
        op: FilterOp::Contains,
        value: "foo".to_string(),
    }];
    let err = resolve_filters(&parsed, &[], &[], FilterContext::Instructions)
        .expect_err("_contains on a SMALLINT fixed column must fail");
    match err {
        ApiError::InvalidFilter { message, .. } => {
            assert!(message.contains("_contains"), "{message}");
        }
        other => panic!("expected InvalidFilter, got {other:?}"),
    }
}

#[test]
fn resolve_unknown_field_returns_invalid_filter_with_available_fields() {
    let parsed = vec![ParsedFilter {
        field: "nonexistent_field".to_string(),
        op: FilterOp::Eq,
        value: "x".to_string(),
    }];
    let err = resolve_filters(&parsed, &sample_idl_fields(), &[], FilterContext::Accounts)
        .expect_err("unknown field must error out");
    match err {
        ApiError::InvalidFilter {
            message,
            available_fields,
        } => {
            assert!(
                message.contains("nonexistent_field"),
                "message should name the offending field: {message}"
            );
            // Fixed account columns + every IDL field name — this is the
            // set that the handler surfaces to the API caller.
            assert!(available_fields.contains(&"pubkey".to_string()));
            assert!(available_fields.contains(&"slot_updated".to_string()));
            assert!(available_fields.contains(&"lamports".to_string()));
            assert!(available_fields.contains(&"is_closed".to_string()));
            assert!(available_fields.contains(&"amount".to_string()));
            assert!(available_fields.contains(&"owner".to_string()));
            assert!(available_fields.contains(&"metadata_vec".to_string()));
        }
        other => panic!("expected InvalidFilter, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Metadata: a hand-rolled parse path that exercises the full pipeline
// from query-string → ParsedFilter → ResolvedFilter → SQL text.
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_slot_gt_produces_bigint_cast_and_bound_value() {
    // Simulates `?slot_gt=100` on an instructions endpoint.
    let params: std::collections::HashMap<String, String> =
        [("slot_gt".to_string(), "100".to_string())]
            .into_iter()
            .collect();
    let parsed = parse_filters(&params);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].field, "slot");
    assert_eq!(parsed[0].op, FilterOp::Gt);

    let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions)
        .expect("fixed column should resolve");
    assert_eq!(resolved.len(), 1);

    let sql = build_sql(&instructions_target(), &resolved);
    assert!(sql.contains(r#""slot" > "#));
    // The cast is the whole point of AC6 — pinpoint the regression shape.
    assert!(
        sql.contains("::BIGINT"),
        "regression for `bigint > text` — expected ::BIGINT cast in: {sql}"
    );
    assert!(sql.contains("$1"));
}
