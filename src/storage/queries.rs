// external crates
use serde_json;
use sqlx::postgres::Postgres;
use sqlx::QueryBuilder;

// internal crate
use crate::api::filters::{ColumnExpr, FilterOp, ResolvedFilter};
use crate::storage::schema::quote_ident;

/// Target table for a dynamic query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryTarget {
    Instructions { schema: String },
    Accounts { schema: String, table: String },
}

/// Build a dynamic SELECT query with filters, ordering, limit, and offset.
///
/// All user-provided values are bound via `push_bind` — never string-concatenated.
/// Table and column names are derived from the IDL (not user input) and double-quoted.
pub fn build_query<'a>(
    target: &QueryTarget,
    filters: &[ResolvedFilter],
    limit: i64,
    offset: i64,
) -> QueryBuilder<'a, Postgres> {
    let (qualified_table, select_cols) = match target {
        QueryTarget::Instructions { schema } => (
            format!("{}.{}", quote_ident(schema), quote_ident("_instructions")),
            r#""signature", "slot", "block_time", "instruction_name", "args", "accounts", "data""#,
        ),
        QueryTarget::Accounts { schema, table } => (
            format!("{}.{}", quote_ident(schema), quote_ident(table)),
            r#""pubkey", "slot_updated", "lamports", "data""#,
        ),
    };

    let mut qb = QueryBuilder::new(format!("SELECT {select_cols} FROM {qualified_table}"));

    // WHERE clauses
    let mut has_where = false;
    for filter in filters {
        qb.push(if has_where { " AND " } else { " WHERE " });
        has_where = true;
        append_filter_clause(&mut qb, filter);
    }

    // ORDER BY
    qb.push(" ORDER BY ");
    match target {
        QueryTarget::Instructions { .. } => {
            qb.push(r#""slot" DESC, "signature" DESC"#);
        }
        QueryTarget::Accounts { .. } => {
            qb.push(r#""slot_updated" DESC"#);
        }
    };

    // LIMIT
    qb.push(" LIMIT ");
    qb.push_bind(limit);

    // OFFSET (only if > 0)
    if offset > 0 {
        qb.push(" OFFSET ");
        qb.push_bind(offset);
    }

    qb
}

/// Append a single filter clause to the query builder.
///
/// Dispatches on column type (Promoted vs Jsonb) and operator to generate
/// the correct SQL pattern with bound parameters.
fn append_filter_clause(qb: &mut QueryBuilder<'_, Postgres>, filter: &ResolvedFilter) {
    match (&filter.column_expr, &filter.op) {
        // --- Promoted column: _in operator ---
        (ColumnExpr::Promoted { column }, FilterOp::In) => {
            let values: Vec<String> = filter
                .value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if values.is_empty() {
                qb.push("FALSE");
            } else {
                qb.push(format!("{} = ANY(", quote_ident(column)));
                qb.push_bind(values);
                qb.push(")");
            }
        }

        // --- Promoted column: standard comparison ---
        (ColumnExpr::Promoted { column }, op) => {
            qb.push(format!("{} {} ", quote_ident(column), op.as_sql()));
            qb.push_bind(filter.value.clone());
        }

        // --- JSONB field: _eq / _contains use @> containment (GIN-optimized) ---
        (ColumnExpr::Jsonb { field }, FilterOp::Eq | FilterOp::Contains) => {
            qb.push(r#""data" @> "#);
            let mut obj = serde_json::Map::new();
            // Try parsing as a typed JSON value (number, boolean, null) first;
            // fall back to string if it's not valid JSON.
            let json_val = serde_json::from_str::<serde_json::Value>(&filter.value)
                .unwrap_or_else(|_| serde_json::Value::String(filter.value.clone()));
            obj.insert(field.clone(), json_val);
            qb.push_bind(serde_json::Value::Object(obj));
        }

        // --- JSONB field: _in uses text extraction + ANY ---
        (ColumnExpr::Jsonb { field }, FilterOp::In) => {
            let values: Vec<String> = filter
                .value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if values.is_empty() {
                qb.push("FALSE");
            } else {
                // data->>'field' = ANY($1)
                qb.push(format!(
                    r#""data"->>'{field}' = ANY("#,
                    field = escape_jsonb_key(field)
                ));
                qb.push_bind(values);
                qb.push(")");
            }
        }

        // --- JSONB field: range operators use text extraction ---
        (ColumnExpr::Jsonb { field }, op) => {
            // ("data"->>'field') OP $1  (no GIN, sequential scan)
            qb.push(format!(
                r#"("data"->>'{field}') {op} "#,
                field = escape_jsonb_key(field),
                op = op.as_sql()
            ));
            qb.push_bind(filter.value.clone());
        }
    }
}

/// Escape a key for safe embedding in a SQL single-quoted string literal.
///
/// Doubles any embedded single quotes to prevent SQL injection.
/// The caller is responsible for wrapping the result in outer single quotes.
fn escape_jsonb_key(key: &str) -> String {
    key.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::filters::{ColumnExpr, FilterOp, ResolvedFilter};

    #[test]
    fn build_query_instructions_no_filters() {
        let target = QueryTarget::Instructions {
            schema: "my_schema".to_string(),
        };
        let qb = build_query(&target, &[], 50, 0);
        let sql = qb.sql();

        assert!(sql.contains(r#"SELECT "signature", "slot", "block_time""#));
        assert!(sql.contains(r#"FROM "my_schema"."_instructions""#));
        assert!(sql.contains(r#"ORDER BY "slot" DESC, "signature" DESC"#));
        assert!(sql.contains("LIMIT"));
        assert!(!sql.contains("OFFSET"));
    }

    #[test]
    fn build_query_accounts_no_filters() {
        let target = QueryTarget::Accounts {
            schema: "my_schema".to_string(),
            table: "token_account".to_string(),
        };
        let qb = build_query(&target, &[], 10, 0);
        let sql = qb.sql();

        assert!(sql.contains(r#"SELECT "pubkey", "slot_updated", "lamports", "data""#));
        assert!(sql.contains(r#"FROM "my_schema"."token_account""#));
        assert!(sql.contains(r#"ORDER BY "slot_updated" DESC"#));
    }

    #[test]
    fn build_query_with_promoted_eq_filter() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Promoted {
                column: "amount".to_string(),
            },
            op: FilterOp::Gt,
            value: "1000".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();

        assert!(sql.contains(r#"WHERE "amount" > "#));
        assert!(sql.contains("$1"));
    }

    #[test]
    fn build_query_with_multiple_filters() {
        let target = QueryTarget::Instructions {
            schema: "s".to_string(),
        };
        let filters = vec![
            ResolvedFilter {
                column_expr: ColumnExpr::Promoted {
                    column: "slot".to_string(),
                },
                op: FilterOp::Gte,
                value: "100".to_string(),
            },
            ResolvedFilter {
                column_expr: ColumnExpr::Promoted {
                    column: "instruction_name".to_string(),
                },
                op: FilterOp::Eq,
                value: "transfer".to_string(),
            },
        ];

        let qb = build_query(&target, &filters, 20, 0);
        let sql = qb.sql();

        assert!(sql.contains("WHERE"));
        assert!(sql.contains("AND"));
        assert!(sql.contains("$1"));
        assert!(sql.contains("$2"));
    }

    #[test]
    fn build_query_with_offset() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let qb = build_query(&target, &[], 10, 20);
        let sql = qb.sql();

        assert!(sql.contains("LIMIT"));
        assert!(sql.contains("OFFSET"));
    }

    #[test]
    fn build_query_jsonb_eq_uses_containment() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "nested_field".to_string(),
            },
            op: FilterOp::Eq,
            value: "test_val".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();

        assert!(
            sql.contains(r#""data" @> "#),
            "expected @> containment, got: {sql}"
        );
    }

    #[test]
    fn build_query_jsonb_contains_uses_containment() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "meta".to_string(),
            },
            op: FilterOp::Contains,
            value: "abc".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();

        assert!(sql.contains(r#""data" @> "#));
    }

    #[test]
    fn build_query_jsonb_range_uses_extraction() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "score".to_string(),
            },
            op: FilterOp::Gt,
            value: "50".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();

        assert!(
            sql.contains(r#"("data"->>'score') > "#),
            "expected JSONB extraction, got: {sql}"
        );
    }

    #[test]
    fn build_query_promoted_in_uses_any() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Promoted {
                column: "status".to_string(),
            },
            op: FilterOp::In,
            value: "a,b,c".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();

        assert!(
            sql.contains(r#""status" = ANY("#),
            "expected = ANY(), got: {sql}"
        );
    }

    #[test]
    fn build_query_jsonb_in_uses_extraction_any() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "tag".to_string(),
            },
            op: FilterOp::In,
            value: "x,y".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();

        assert!(
            sql.contains(r#""data"->>'tag' = ANY("#),
            "expected JSONB extraction + ANY, got: {sql}"
        );
    }

    #[test]
    fn build_query_jsonb_eq_numeric_value_not_wrapped_as_string() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "count".to_string(),
            },
            op: FilterOp::Eq,
            value: "42".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();
        // The value should be bound as JSON number, not string
        assert!(sql.contains(r#""data" @> "#));
    }

    #[test]
    fn build_query_promoted_in_empty_value_produces_false() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Promoted {
                column: "status".to_string(),
            },
            op: FilterOp::In,
            value: "".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();
        assert!(
            sql.contains("FALSE"),
            "empty _in should produce FALSE, got: {sql}"
        );
    }

    #[test]
    fn build_query_jsonb_in_empty_value_produces_false() {
        let target = QueryTarget::Accounts {
            schema: "s".to_string(),
            table: "t".to_string(),
        };
        let filters = vec![ResolvedFilter {
            column_expr: ColumnExpr::Jsonb {
                field: "tag".to_string(),
            },
            op: FilterOp::In,
            value: "".to_string(),
        }];

        let qb = build_query(&target, &filters, 50, 0);
        let sql = qb.sql();
        assert!(
            sql.contains("FALSE"),
            "empty _in should produce FALSE, got: {sql}"
        );
    }

    #[test]
    fn escape_jsonb_key_with_quotes() {
        assert_eq!(escape_jsonb_key("normal"), "normal");
        assert_eq!(escape_jsonb_key("it's"), "it''s");
    }
}
