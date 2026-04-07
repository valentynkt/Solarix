// Story 6.5 AC4: filter execution matrix against a real PostgreSQL.
//
// Drives `solarix::storage::queries::build_query` against a fresh
// `postgres:16-alpine` container via `tests/common/postgres.rs::with_postgres`.
// Asserts every (FilterOp × column-type) combination from
// `tests/filter_builder_contracts.rs` (Story 6.4 AC6) returns the expected
// rowset, including:
//
//   - Sprint-4 regression: `slot_gt=100` against a BIGINT column without the
//     production fix would emit `"slot" > $1` (text bind), triggering
//     PostgreSQL's `operator does not exist: bigint > text` error. The
//     production fix in commit 243a0de added the per-column SQL cast in
//     `src/storage/queries.rs::append_filter_clause`. This test is the
//     regression pin.
//   - Empty `_in` value → exactly 0 rows (not a 500).
//   - JSONB `_eq` / `_contains` / `_in` cases via the `args` payload.
//
// REGRESSION: bigint > text — Sprint-4 e2e gate, fixed in commit 243a0de.
// See `_bmad-output/implementation-artifacts/deferred-work.md` →
// "Findings from: e2e-verification-sprint-4 (2026-04-07)".

#![cfg(feature = "integration")]
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;

use serde_json::json;
use sqlx::Row;

use solarix::api::filters::{
    parse_filters, resolve_filters, ColumnExpr, FilterContext, FilterOp, ResolvedFilter,
};
use solarix::storage::queries::{build_query, QueryTarget};
use solarix::storage::schema::{generate_schema, quote_ident};

mod common;
use common::postgres::with_postgres;

const SIMPLE_IDL_JSON: &str = include_str!("fixtures/idls/simple_v030.json");
const PROGRAM_ID: &str = "ProgID11111111111111111111111111111111111111";
const TEST_SCHEMA: &str = "test_filter_exec_simple";

#[tokio::test]
async fn filter_sql_exec_matrix_against_real_postgres() {
    with_postgres(|pool| async move {
        // Sanity: bootstrap_system_tables ran inside `with_postgres` already.
        // Spot-check that `programs` exists.
        let row: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = 'programs'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, 1, "system bootstrap should have created `programs`");

        // Generate the schema for the simple_v030.json fixture.
        let idl: anchor_lang_idl_spec::Idl =
            serde_json::from_str(SIMPLE_IDL_JSON).expect("simple_v030 fixture should parse");
        generate_schema(
            pool.clone(),
            idl.clone(),
            PROGRAM_ID.to_string(),
            TEST_SCHEMA.to_string(),
        )
        .await
        .expect("generate_schema should succeed");

        // Seed 10 synthetic rows directly into _instructions covering every
        // promoted-column type. Slot magnitudes include i64::MAX - 1 to lock
        // the BIGINT cast path that the Sprint-4 bug originally tripped.
        let slots: [i64; 10] = [1, 5, 10, 50, 100, 500, 1_000, 5_000, 10_000, i64::MAX - 1];
        let ix_indexes: [i16; 10] = [0, 1, 2, 0, 1, 2, 0, 1, 2, 0];
        let names: [&str; 10] = [
            "initialize",
            "transfer",
            "initialize",
            "transfer",
            "initialize",
            "transfer",
            "initialize",
            "transfer",
            "initialize",
            "transfer",
        ];
        // JSONB args used by the JSONB matrix below.
        let labels: [&str; 10] = [
            "alpha", "beta", "gamma", "alpha", "beta", "alpha", "delta", "alpha", "epsilon",
            "alpha",
        ];

        let qualified = format!(
            "{}.{}",
            quote_ident(TEST_SCHEMA),
            quote_ident("_instructions")
        );

        for i in 0..10 {
            let signature = format!("sig_{i:02}");
            let is_inner = i % 2 == 1;
            let value: i64 = (i as i64 + 1) * 1_000;
            // Use a value > i64::MAX/2 for the last row to exercise the
            // BIGINT promotion guard via JSONB extraction.
            let value = if i == 9 { i64::MAX / 2 + 1 } else { value };
            let args = json!({ "value": value, "label": labels[i] });

            let sql = format!(
                r#"INSERT INTO {qualified}
                   ("signature", "slot", "block_time", "instruction_name",
                    "instruction_index", "inner_index", "args", "accounts", "data", "is_inner_ix")
                   VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8::jsonb, $9::jsonb, $10)"#
            );

            sqlx::query(&sql)
                .bind(&signature)
                .bind(slots[i])
                .bind(Some(1_700_000_000i64 + i as i64))
                .bind(names[i])
                .bind(ix_indexes[i])
                .bind::<Option<i16>>(None)
                .bind(serde_json::to_string(&args).unwrap())
                .bind("[]")
                .bind(serde_json::to_string(&args).unwrap())
                .bind(is_inner)
                .execute(&pool)
                .await
                .expect("seed insert should succeed");
        }

        let target = QueryTarget::Instructions {
            schema: TEST_SCHEMA.to_string(),
        };

        // -----------------------------------------------------------------
        // (1) Sprint-4 regression: slot_gt=100 against a BIGINT column.
        //
        // Without the production fix in `append_filter_clause`, the bound
        // value would be cast to text and PostgreSQL would error with
        // `operator does not exist: bigint > text`. The fix appends
        // `::BIGINT` to the bind. This test is the regression pin.
        // -----------------------------------------------------------------
        let mut params = HashMap::new();
        params.insert("slot_gt".to_string(), "100".to_string());
        let parsed = parse_filters(&params);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions)
            .expect("slot_gt should resolve as a fixed-column promoted filter");
        // Sanity: confirm the resolver returned a Promoted{BIGINT} entry —
        // anything else means the regression matrix has drifted.
        assert!(
            matches!(
                &resolved[0].column_expr,
                ColumnExpr::Promoted { column, pg_type }
                    if column == "slot" && pg_type.as_deref() == Some("BIGINT")
            ),
            "slot_gt must resolve to Promoted{{BIGINT}}, got: {:?}",
            resolved[0].column_expr
        );

        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb
            .build()
            .fetch_all(&pool)
            .await
            .expect("slot_gt=100 must NOT trigger `operator does not exist: bigint > text`");
        // Slots > 100 in the seed: 500, 1000, 5000, 10000, i64::MAX-1.
        assert_eq!(rows.len(), 5, "slot > 100 should return 5 rows");
        for row in &rows {
            let slot: i64 = row.get("slot");
            assert!(slot > 100, "leaked row with slot={slot}");
        }

        // -----------------------------------------------------------------
        // (2) BIGINT range coverage: slot_gte / slot_lt / slot_lte / slot_eq.
        // -----------------------------------------------------------------
        for (op, value, expected) in [
            ("slot_gte", "500", 5), // 500, 1000, 5000, 10000, MAX-1
            ("slot_lt", "100", 4),  // 1, 5, 10, 50
            ("slot_lte", "100", 5), // 1, 5, 10, 50, 100
            ("slot_eq", "100", 1),  // 100
            ("slot_ne", "100", 9),  // everything except 100
        ] {
            let mut p = HashMap::new();
            p.insert(op.to_string(), value.to_string());
            let parsed = parse_filters(&p);
            let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
            let mut qb = build_query(&target, &resolved, 50, 0);
            let rows = qb.build().fetch_all(&pool).await.unwrap_or_else(|e| {
                panic!("{op}={value} failed (regression in BIGINT cast path?): {e}")
            });
            assert_eq!(
                rows.len(),
                expected,
                "{op}={value} expected {expected} rows"
            );
        }

        // -----------------------------------------------------------------
        // (3) SMALLINT promoted column: instruction_index.
        // -----------------------------------------------------------------
        let mut p = HashMap::new();
        p.insert("instruction_index_eq".to_string(), "1".to_string());
        let parsed = parse_filters(&p);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
        // Sanity: SMALLINT cast must be present in the resolved column expr.
        assert!(matches!(
            &resolved[0].column_expr,
            ColumnExpr::Promoted { pg_type, .. } if pg_type.as_deref() == Some("SMALLINT")
        ));
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap_or_else(|e| {
            panic!("instruction_index_eq=1 should not error against SMALLINT: {e}")
        });
        // ix_indexes pattern [0,1,2,0,1,2,0,1,2,0] → three 1's.
        assert_eq!(rows.len(), 3, "instruction_index=1 should return 3 rows");

        // -----------------------------------------------------------------
        // (4) TEXT promoted column: instruction_name.
        // -----------------------------------------------------------------
        let mut p = HashMap::new();
        p.insert("instruction_name_eq".to_string(), "initialize".to_string());
        let parsed = parse_filters(&p);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap();
        // names pattern: alternating, 5 of each.
        assert_eq!(
            rows.len(),
            5,
            "instruction_name=initialize should return 5 rows"
        );

        // -----------------------------------------------------------------
        // (5) BOOLEAN promoted column: is_inner_ix.
        // -----------------------------------------------------------------
        let mut p = HashMap::new();
        p.insert("is_inner_ix_eq".to_string(), "true".to_string());
        let parsed = parse_filters(&p);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap_or_else(|e| {
            panic!("is_inner_ix_eq=true should not error against BOOLEAN: {e}")
        });
        // Pattern i % 2 == 1 → 5 rows.
        assert_eq!(rows.len(), 5, "is_inner_ix=true should return 5 rows");

        // -----------------------------------------------------------------
        // (6) IN operator on a TEXT column with multiple values.
        // -----------------------------------------------------------------
        let mut p = HashMap::new();
        p.insert(
            "instruction_name_in".to_string(),
            "initialize,transfer".to_string(),
        );
        let parsed = parse_filters(&p);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 10, "instruction_name_in covers all 10 rows");

        // -----------------------------------------------------------------
        // (7) Empty `_in` regression: must produce 0 rows, not a 500.
        // -----------------------------------------------------------------
        let mut p = HashMap::new();
        p.insert("instruction_name_in".to_string(), "".to_string());
        let parsed = parse_filters(&p);
        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb
            .build()
            .fetch_all(&pool)
            .await
            .unwrap_or_else(|e| panic!("empty _in should yield FALSE, not error: {e}"));
        assert_eq!(rows.len(), 0, "empty _in must return zero rows");

        // -----------------------------------------------------------------
        // (8) JSONB `_eq` against the `args` payload via direct ResolvedFilter
        // construction. We bypass parse_filters here because the resolver
        // routes IDL fields through `Promoted` when the IDL declares them
        // with a promotable type (the simple_v030 IDL has `value: u64`),
        // and the test wants to exercise the JSONB containment branch
        // explicitly. This is the same surface the API handler reaches when
        // an IDL field is non-promotable.
        //
        // Note: the JSONB filter currently targets the `data` column (via
        // the `@>` operator), which `decompose_instructions` populates with
        // the same payload as `args`. The seed loop above mirrors that
        // contract.
        // -----------------------------------------------------------------
        let resolved = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "label".to_string(),
            },
            op: FilterOp::Eq,
            value: "alpha".to_string(),
        }];
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap();
        // labels pattern has "alpha" at indices 0,3,5,7,9 → 5 rows.
        assert_eq!(
            rows.len(),
            5,
            "JSONB label@>alpha should match the 5 'alpha' rows"
        );

        // (8b) JSONB `_contains` — same containment branch, distinct intent.
        let resolved = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "label".to_string(),
            },
            op: FilterOp::Contains,
            value: "beta".to_string(),
        }];
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap();
        // "beta" at indices 1, 4 → 2 rows.
        assert_eq!(rows.len(), 2, "JSONB label@>beta should match 2 rows");

        // (8c) JSONB `_in` — text-extraction + ANY branch.
        let resolved = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "label".to_string(),
            },
            op: FilterOp::In,
            value: "delta,epsilon".to_string(),
        }];
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb.build().fetch_all(&pool).await.unwrap();
        // "delta" at index 6, "epsilon" at index 8 → 2 rows.
        assert_eq!(
            rows.len(),
            2,
            "JSONB label _in (delta,epsilon) should match 2 rows"
        );

        // (8d) JSONB range operator — text extraction + comparison.
        //
        // NOTE: JSONB range filters use `("data"->>'field') OP $1`, which is
        // a TEXT comparison — lexicographic, NOT numeric. The huge i64::MAX/2+1
        // value (string `"4611686018427387904"`) is lexicographically *less
        // than* `"5000"` because `'4' < '5'`. So the matching rows are only
        // `6000, 7000, 8000, 9000` = 4 rows. This is intentional behaviour
        // of the JSONB range branch (callers needing numeric comparison
        // should add the field to the IDL so it gets promoted to a typed
        // column instead). Locking the count here pins the contract.
        let resolved = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "value".to_string(),
            },
            op: FilterOp::Gt,
            value: "5000".to_string(),
        }];
        let mut qb = build_query(&target, &resolved, 50, 0);
        let rows = qb
            .build()
            .fetch_all(&pool)
            .await
            .expect("JSONB range comparison must execute");
        assert_eq!(
            rows.len(),
            4,
            "JSONB value > 5000 (lexicographic text compare) should match 4 rows"
        );
    })
    .await;
}
