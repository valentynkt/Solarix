// std library
use std::collections::{HashMap, HashSet};

// external crates
use anchor_lang_idl_spec::{IdlField, IdlTypeDef};

// internal crate
use crate::api::ApiError;
use crate::storage::schema::map_idl_type_to_pg;

/// Supported filter operators parsed from query parameter suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    In,
}

impl FilterOp {
    /// SQL operator string for direct column comparisons.
    pub fn as_sql(&self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "!=",
            FilterOp::Gt => ">",
            FilterOp::Gte => ">=",
            FilterOp::Lt => "<",
            FilterOp::Lte => "<=",
            FilterOp::Contains => "@>",
            FilterOp::In => "= ANY",
        }
    }
}

/// Operator suffixes ordered from longest to shortest to avoid ambiguous matches
/// (e.g. `_gte` must be checked before `_gt`).
const OPERATOR_SUFFIXES: &[(&str, FilterOp)] = &[
    ("_contains", FilterOp::Contains),
    ("_gte", FilterOp::Gte),
    ("_lte", FilterOp::Lte),
    ("_gt", FilterOp::Gt),
    ("_lt", FilterOp::Lt),
    ("_eq", FilterOp::Eq),
    ("_ne", FilterOp::Ne),
    ("_in", FilterOp::In),
];

/// Query parameters reserved for pagination/sorting — not treated as filters.
const RESERVED_PARAMS: &[&str] = &["limit", "offset", "cursor", "sort", "order"];

/// A filter extracted from a query parameter before IDL validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilter {
    pub field: String,
    pub op: FilterOp,
    pub value: String,
}

/// How a filter field maps to the database column structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnExpr {
    /// Field promoted to a native PostgreSQL column.
    Promoted { column: String },
    /// Field stored in the JSONB `data` column.
    Jsonb { field: String },
}

/// A filter after IDL validation and column resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFilter {
    pub column_expr: ColumnExpr,
    pub op: FilterOp,
    pub value: String,
}

/// Parse query parameters into filter descriptors.
///
/// Skips reserved params (limit, offset, cursor, sort, order).
/// For each remaining param, extracts the operator suffix (longest match first).
/// If no operator suffix matches, defaults to `Eq`.
pub fn parse_filters(params: &HashMap<String, String>) -> Vec<ParsedFilter> {
    let reserved: HashSet<&str> = RESERVED_PARAMS.iter().copied().collect();
    let mut filters = Vec::new();

    for (key, value) in params {
        if reserved.contains(key.as_str()) {
            continue;
        }

        let (field, op) = extract_field_and_op(key);
        if !field.is_empty() {
            filters.push(ParsedFilter {
                field,
                op,
                value: value.clone(),
            });
        }
    }

    filters
}

/// Extract field name and operator from a query parameter key.
fn extract_field_and_op(key: &str) -> (String, FilterOp) {
    for &(suffix, op) in OPERATOR_SUFFIXES {
        if let Some(field) = key.strip_suffix(suffix) {
            if !field.is_empty() {
                return (field.to_string(), op);
            }
        }
    }
    // No operator suffix found — treat entire key as field name with Eq
    (key.to_string(), FilterOp::Eq)
}

// --- Fixed/common columns that exist on all tables regardless of IDL ---

/// Common columns on the `_instructions` table.
const INSTRUCTION_FIXED_COLUMNS: &[&str] = &[
    "slot",
    "signature",
    "block_time",
    "instruction_name",
    "instruction_index",
    "is_inner_ix",
];

/// Common columns on account tables.
const ACCOUNT_FIXED_COLUMNS: &[&str] = &["pubkey", "slot_updated", "lamports", "is_closed"];

/// Whether we are resolving filters for instructions or accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterContext {
    Instructions,
    Accounts,
}

/// Resolve parsed filters against IDL field definitions.
///
/// - Fixed/common columns are always accepted as `Promoted`.
/// - IDL fields with a promotable PG type are `Promoted`.
/// - IDL fields without a promotable type are `Jsonb`.
/// - Unknown field names return `ApiError::InvalidFilter` with available fields.
pub fn resolve_filters(
    parsed: &[ParsedFilter],
    fields: &[IdlField],
    types: &[IdlTypeDef],
    context: FilterContext,
) -> Result<Vec<ResolvedFilter>, ApiError> {
    let fixed_columns: &[&str] = match context {
        FilterContext::Instructions => INSTRUCTION_FIXED_COLUMNS,
        FilterContext::Accounts => ACCOUNT_FIXED_COLUMNS,
    };

    let mut resolved = Vec::with_capacity(parsed.len());

    for filter in parsed {
        if fixed_columns.contains(&filter.field.as_str()) {
            resolved.push(ResolvedFilter {
                column_expr: ColumnExpr::Promoted {
                    column: filter.field.clone(),
                },
                op: filter.op,
                value: filter.value.clone(),
            });
            continue;
        }

        // Look up field in IDL
        let idl_field = fields.iter().find(|f| f.name == filter.field);

        match idl_field {
            Some(f) => {
                let column_expr = if map_idl_type_to_pg(&f.ty, types).is_some() {
                    ColumnExpr::Promoted {
                        column: filter.field.clone(),
                    }
                } else {
                    ColumnExpr::Jsonb {
                        field: filter.field.clone(),
                    }
                };
                resolved.push(ResolvedFilter {
                    column_expr,
                    op: filter.op,
                    value: filter.value.clone(),
                });
            }
            None => {
                let mut available: Vec<String> =
                    fixed_columns.iter().map(|s| (*s).to_string()).collect();
                available.extend(fields.iter().map(|f| f.name.clone()));
                return Err(ApiError::InvalidFilter(format!(
                    "Unknown field '{}'. Available fields: [{}]",
                    filter.field,
                    available.join(", ")
                )));
            }
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang_idl_spec::IdlType;

    // --- parse_filters tests ---

    #[test]
    fn parse_basic_operators() {
        let mut params = HashMap::new();
        params.insert("amount_gt".to_string(), "1000".to_string());
        params.insert("signer_eq".to_string(), "ABC123".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 2);

        let amount = filters.iter().find(|f| f.field == "amount").unwrap();
        assert_eq!(amount.op, FilterOp::Gt);
        assert_eq!(amount.value, "1000");

        let signer = filters.iter().find(|f| f.field == "signer").unwrap();
        assert_eq!(signer.op, FilterOp::Eq);
        assert_eq!(signer.value, "ABC123");
    }

    #[test]
    fn parse_gte_before_gt() {
        let mut params = HashMap::new();
        params.insert("amount_gte".to_string(), "500".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "amount");
        assert_eq!(filters[0].op, FilterOp::Gte);
    }

    #[test]
    fn parse_lte_before_lt() {
        let mut params = HashMap::new();
        params.insert("price_lte".to_string(), "99".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "price");
        assert_eq!(filters[0].op, FilterOp::Lte);
    }

    #[test]
    fn parse_contains_operator() {
        let mut params = HashMap::new();
        params.insert("name_contains".to_string(), "foo".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "name");
        assert_eq!(filters[0].op, FilterOp::Contains);
    }

    #[test]
    fn parse_in_operator() {
        let mut params = HashMap::new();
        params.insert("status_in".to_string(), "active,pending".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "status");
        assert_eq!(filters[0].op, FilterOp::In);
        assert_eq!(filters[0].value, "active,pending");
    }

    #[test]
    fn parse_no_operator_defaults_to_eq() {
        let mut params = HashMap::new();
        params.insert("pubkey".to_string(), "ABC".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "pubkey");
        assert_eq!(filters[0].op, FilterOp::Eq);
    }

    #[test]
    fn parse_skips_reserved_params() {
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "10".to_string());
        params.insert("offset".to_string(), "20".to_string());
        params.insert("cursor".to_string(), "abc".to_string());
        params.insert("sort".to_string(), "slot".to_string());
        params.insert("order".to_string(), "desc".to_string());
        params.insert("amount_gt".to_string(), "100".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "amount");
    }

    #[test]
    fn parse_field_with_underscore_containing_op_substring() {
        // Field name "my_field_gt" should parse as field="my_field", op=Gt
        let mut params = HashMap::new();
        params.insert("my_field_gt".to_string(), "42".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "my_field");
        assert_eq!(filters[0].op, FilterOp::Gt);
    }

    #[test]
    fn parse_field_named_like_operator() {
        // A param "gt" with no suffix operator -> field="gt", op=Eq
        let mut params = HashMap::new();
        params.insert("gt".to_string(), "value".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "gt");
        assert_eq!(filters[0].op, FilterOp::Eq);
    }

    #[test]
    fn parse_ne_operator() {
        let mut params = HashMap::new();
        params.insert("status_ne".to_string(), "closed".to_string());

        let filters = parse_filters(&params);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].field, "status");
        assert_eq!(filters[0].op, FilterOp::Ne);
    }

    #[test]
    fn parse_empty_params() {
        let params = HashMap::new();
        let filters = parse_filters(&params);
        assert!(filters.is_empty());
    }

    // --- resolve_filters tests ---

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
                name: "metadata".to_string(),
                docs: vec![],
                ty: IdlType::Vec(Box::new(IdlType::U8)),
            },
        ]
    }

    #[test]
    fn resolve_promoted_field() {
        let fields = sample_idl_fields();
        let parsed = vec![ParsedFilter {
            field: "amount".to_string(),
            op: FilterOp::Gt,
            value: "1000".to_string(),
        }];

        let resolved = resolve_filters(&parsed, &fields, &[], FilterContext::Accounts).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].column_expr,
            ColumnExpr::Promoted {
                column: "amount".to_string()
            }
        );
    }

    #[test]
    fn resolve_jsonb_field() {
        let fields = sample_idl_fields();
        let parsed = vec![ParsedFilter {
            field: "metadata".to_string(),
            op: FilterOp::Eq,
            value: "test".to_string(),
        }];

        let resolved = resolve_filters(&parsed, &fields, &[], FilterContext::Accounts).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].column_expr,
            ColumnExpr::Jsonb {
                field: "metadata".to_string()
            }
        );
    }

    #[test]
    fn resolve_unknown_field_returns_error() {
        let fields = sample_idl_fields();
        let parsed = vec![ParsedFilter {
            field: "nonexistent".to_string(),
            op: FilterOp::Eq,
            value: "x".to_string(),
        }];

        let err = resolve_filters(&parsed, &fields, &[], FilterContext::Accounts).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Unknown field 'nonexistent'"));
        assert!(msg.contains("amount"));
        assert!(msg.contains("owner"));
        assert!(msg.contains("pubkey"));
    }

    #[test]
    fn resolve_fixed_account_columns() {
        let parsed = vec![
            ParsedFilter {
                field: "pubkey".to_string(),
                op: FilterOp::Eq,
                value: "ABC".to_string(),
            },
            ParsedFilter {
                field: "slot_updated".to_string(),
                op: FilterOp::Gte,
                value: "100".to_string(),
            },
        ];

        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Accounts).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(matches!(
            &resolved[0].column_expr,
            ColumnExpr::Promoted { column } if column == "pubkey"
        ));
        assert!(matches!(
            &resolved[1].column_expr,
            ColumnExpr::Promoted { column } if column == "slot_updated"
        ));
    }

    #[test]
    fn resolve_fixed_instruction_columns() {
        let parsed = vec![
            ParsedFilter {
                field: "signature".to_string(),
                op: FilterOp::Eq,
                value: "SIG123".to_string(),
            },
            ParsedFilter {
                field: "instruction_name".to_string(),
                op: FilterOp::Eq,
                value: "transfer".to_string(),
            },
        ];

        let resolved = resolve_filters(&parsed, &[], &[], FilterContext::Instructions).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(matches!(
            &resolved[0].column_expr,
            ColumnExpr::Promoted { column } if column == "signature"
        ));
    }

    #[test]
    fn resolve_mixed_promoted_and_jsonb() {
        let fields = sample_idl_fields();
        let parsed = vec![
            ParsedFilter {
                field: "amount".to_string(),
                op: FilterOp::Gt,
                value: "1000".to_string(),
            },
            ParsedFilter {
                field: "metadata".to_string(),
                op: FilterOp::Contains,
                value: "test".to_string(),
            },
        ];

        let resolved = resolve_filters(&parsed, &fields, &[], FilterContext::Accounts).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(matches!(
            &resolved[0].column_expr,
            ColumnExpr::Promoted { .. }
        ));
        assert!(matches!(&resolved[1].column_expr, ColumnExpr::Jsonb { .. }));
    }

    // --- FilterOp::as_sql tests ---

    #[test]
    fn operator_sql_mapping() {
        assert_eq!(FilterOp::Eq.as_sql(), "=");
        assert_eq!(FilterOp::Ne.as_sql(), "!=");
        assert_eq!(FilterOp::Gt.as_sql(), ">");
        assert_eq!(FilterOp::Gte.as_sql(), ">=");
        assert_eq!(FilterOp::Lt.as_sql(), "<");
        assert_eq!(FilterOp::Lte.as_sql(), "<=");
        assert_eq!(FilterOp::Contains.as_sql(), "@>");
        assert_eq!(FilterOp::In.as_sql(), "= ANY");
    }
}
