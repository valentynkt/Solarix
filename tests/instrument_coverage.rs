//! Story 6.1 instrument coverage — enforces CONTRIBUTING.md §Logging.
//!
//! Rules:
//!
//!   R1. Every non-trait, non-test `async fn` in the instrumented scope
//!       (`FILES_TO_SCAN`) must carry a `#[tracing::instrument]` attribute
//!       directly above it.
//!   R2. The instrument attribute must have an explicit `name = "..."`
//!       argument (bare `#[tracing::instrument]` is not allowed — we want
//!       to pin the metric/log name under version control).
//!   R3. The `name` value must follow the dotted `module.function`
//!       convention (contain at least one `.`).
//!   R4. `async fn`s whose return type is `Result<..>` must declare an
//!       `err(Display)` / `err(..)` / bare `err` directive so errors
//!       surface on the span.
//!   R5. No file under `src/` may use `println!` / `eprintln!` /
//!       `print!` / `eprint!` / `dbg!` outside `#[cfg(test)]` regions —
//!       the project uses `tracing` macros exclusively.
//!
//! Implementation notes:
//!
//! - Text walk only. No `syn`, no `regex`, no `walkdir`. Follows the
//!   Story 6.4 precedent of keeping cold-build time and the dep tree
//!   honest for integration tests.
//! - `src/storage/queries.rs` is intentionally absent from
//!   `FILES_TO_SCAN` — it contains zero `async fn` declarations (the
//!   `QueryBuilder` is pure synchronous SQL assembly), so instrumenting
//!   it would be vacuous. If a future change introduces an `async fn`
//!   in that file, add the path back to `FILES_TO_SCAN`.
//! - The scanner is line-oriented and slightly conservative, but the
//!   rules below are tight enough to catch the failure modes that
//!   matter: a newly added async fn that ships without instrument,
//!   an instrument attribute without an explicit dotted name, a
//!   fallible fn that forgets `err(..)`, or a stray `println!`.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// Files in the instrumented scope (R1–R4). Matches the list in
/// CONTRIBUTING.md §Logging plus `src/storage/mod.rs` whose two
/// bootstrap-time `pub async fn`s (`init_pool`, `bootstrap_system_tables`)
/// are also instrumented and must stay that way.
const FILES_TO_SCAN: &[&str] = &[
    "src/pipeline/mod.rs",
    "src/pipeline/rpc.rs",
    "src/pipeline/ws.rs",
    "src/api/handlers.rs",
    "src/idl/mod.rs",
    "src/idl/fetch.rs",
    "src/registry.rs",
    "src/storage/writer.rs",
    "src/storage/mod.rs",
];

/// Macros that emit to stdout/stderr directly. Solarix routes everything
/// through `tracing` so none of these may appear in non-test `src/` code.
const FORBIDDEN_MACROS: &[&str] = &["println!", "eprintln!", "print!", "eprint!", "dbg!"];

/// One scanner finding. `rule` is the R-number from the module doc.
#[derive(Debug)]
struct Finding {
    file: String,
    line_no: usize,
    fn_name: String,
    rule: &'static str,
    detail: String,
}

impl Finding {
    fn format(&self) -> String {
        format!(
            "{} — {}:{} async fn `{}` — {}",
            self.rule, self.file, self.line_no, self.fn_name, self.detail
        )
    }
}

/// Repo root via `CARGO_MANIFEST_DIR` (single-crate workspace).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Strip a leading visibility modifier (`pub`, `pub(crate)`, etc.) from
/// `s`. Returns the original trimmed `s` if no modifier is present.
fn strip_visibility(s: &str) -> &str {
    let s = s.trim_start();
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub(self) ",
        "pub(in crate) ",
        "pub ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    s
}

/// If `line` declares an `async fn`, returns the fn name; otherwise
/// `None`. Matches `pub async fn foo(`, `async fn bar<'a>(`, etc.
fn parse_async_fn_decl(line: &str) -> Option<String> {
    let stripped = strip_visibility(line);
    let after_async = stripped.strip_prefix("async fn ")?;
    let end = after_async
        .find(|c: char| c == '<' || c == '(' || c.is_whitespace())
        .unwrap_or(after_async.len());
    let name = &after_async[..end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Look ahead up to 25 lines from `start_idx` for the signature
/// terminator. `;` means trait method declaration (skip); `{` means
/// real definition.
fn is_trait_decl(lines: &[&str], start_idx: usize) -> bool {
    let end = (start_idx + 25).min(lines.len());
    for line in &lines[start_idx..end] {
        let trimmed = line.split("//").next().unwrap_or(line).trim_end();
        if trimmed.ends_with('{') {
            return false;
        }
        if trimmed.ends_with(';') {
            return true;
        }
    }
    false
}

/// Naive `//` line-comment strip. Does not understand string literals,
/// which is fine for fn signatures and attribute bodies in this codebase.
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(p) => &line[..p],
        None => line,
    }
}

/// Collect the full fn signature starting at `start_idx` into one flat
/// string. Walks forward up to 50 lines until a `{` or `;` terminator
/// appears at end-of-line, so multi-line signatures collapse into a
/// single searchable string.
fn collect_signature(lines: &[&str], start_idx: usize) -> String {
    let mut sig = String::new();
    let end = (start_idx + 50).min(lines.len());
    for line in &lines[start_idx..end] {
        let code = strip_line_comment(line);
        sig.push_str(code);
        sig.push(' ');
        let trimmed = code.trim_end();
        if trimmed.ends_with('{') || trimmed.ends_with(';') {
            break;
        }
    }
    sig
}

/// Returns true if `sig` declares a `Result<..>` return type. Uses the
/// last `) -> ` occurrence (so a `fn() -> Result<..>` parameter inside
/// the argument list doesn't false-positive) and searches the slice up
/// to the opening brace or `where` clause.
fn signature_returns_result(sig: &str) -> bool {
    let Some(arrow_pos) = sig.rfind(") -> ") else {
        return false;
    };
    let after = &sig[arrow_pos + 5..];
    let upto_brace = after.find('{').unwrap_or(after.len());
    let upto_where = after.find(" where ").unwrap_or(usize::MAX);
    let end = upto_brace.min(upto_where).min(after.len());
    after[..end].contains("Result<")
}

/// Walk backwards from `idx` (the `async fn` line) looking for the line
/// that opens an `#[tracing::instrument(..)]` or `#[instrument(..)]`
/// attribute block attached to this fn. Returns the 0-based line index
/// of the opener, or `None` if no such attribute is attached.
///
/// The walk tolerates interleaved doc comments, blank lines, other
/// attribute macros (e.g. `#[allow(..)]`, `#[async_trait]`), and
/// multi-line attribute-body continuation lines.
fn find_instrument_opener(lines: &[&str], idx: usize) -> Option<usize> {
    let lookback = 25usize;
    let start = idx.saturating_sub(lookback);
    for i in (start..idx).rev() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty()
            || trimmed.starts_with("///")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("//")
        {
            continue;
        }
        if trimmed.starts_with("#[tracing::instrument") || trimmed.starts_with("#[instrument") {
            return Some(i);
        }
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            continue;
        }
        if trimmed.starts_with(')')
            || trimmed == "]"
            || trimmed.ends_with(',')
            || trimmed.starts_with("name =")
            || trimmed.starts_with("skip")
            || trimmed.starts_with("fields")
            || trimmed.starts_with("level =")
            || trimmed.starts_with("err(")
            || trimmed.starts_with("ret")
        {
            continue;
        }
        return None;
    }
    None
}

/// Given the line index of an instrument opener and the fn line below
/// it, extract the raw text between the outer `(` and `)]` of the
/// attribute. Returns `None` if the attribute is bare (no parens) or if
/// the closing `)]` cannot be found.
fn extract_instrument_body(lines: &[&str], opener_idx: usize, fn_idx: usize) -> Option<String> {
    let mut buf = String::new();
    for i in opener_idx..=fn_idx {
        if i >= lines.len() {
            break;
        }
        buf.push_str(lines[i]);
        buf.push('\n');
        if lines[i].trim_end().ends_with(")]") {
            break;
        }
    }
    let after_open = if let Some(p) = buf.find("#[tracing::instrument(") {
        &buf[p + "#[tracing::instrument(".len()..]
    } else if let Some(p) = buf.find("#[instrument(") {
        &buf[p + "#[instrument(".len()..]
    } else {
        return None;
    };
    let end = after_open.rfind(")]")?;
    Some(after_open[..end].to_string())
}

/// Split an instrument-attribute body on top-level commas, ignoring
/// commas inside nested parens and inside `"`-delimited string
/// literals.
fn split_top_level(body: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for ch in body.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if in_string && ch == '\\' {
            escape = true;
            current.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            current.push(ch);
            continue;
        }
        if in_string {
            current.push(ch);
            continue;
        }
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    parts
}

/// Extract the string literal value of `name = "..."` from the parts
/// of a split instrument-attribute body, if present.
fn extract_name(parts: &[String]) -> Option<String> {
    for p in parts {
        let Some(rest) = p.strip_prefix("name") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Returns true if any top-level part is `err`, `err(..)`, or starts
/// with `err(`.
fn has_err_directive(parts: &[String]) -> bool {
    parts.iter().any(|p| p == "err" || p.starts_with("err("))
}

/// Line index of the first `#[cfg(test)]` region start, if any.
/// Everything from that line forward is treated as test code and
/// excluded from the scan.
fn cfg_test_boundary(lines: &[&str]) -> Option<usize> {
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            return Some(i);
        }
    }
    None
}

/// Scan one in-scope source file for R1–R4 violations.
fn scan_file(rel_path: &str) -> Vec<Finding> {
    let abs_path = repo_root().join(rel_path);
    let content = fs::read_to_string(&abs_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", abs_path.display()));
    let lines: Vec<&str> = content.lines().collect();
    let test_boundary = cfg_test_boundary(&lines).unwrap_or(lines.len());

    let mut findings = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx >= test_boundary {
            break;
        }
        let Some(name) = parse_async_fn_decl(line) else {
            continue;
        };
        if is_trait_decl(&lines, idx) {
            continue;
        }

        let opener_idx = match find_instrument_opener(&lines, idx) {
            Some(i) => i,
            None => {
                findings.push(Finding {
                    file: rel_path.to_string(),
                    line_no: idx + 1,
                    fn_name: name,
                    rule: "R1",
                    detail: "missing #[tracing::instrument] attribute".to_string(),
                });
                continue;
            }
        };

        let parts = match extract_instrument_body(&lines, opener_idx, idx) {
            Some(body) => split_top_level(&body),
            None => {
                findings.push(Finding {
                    file: rel_path.to_string(),
                    line_no: idx + 1,
                    fn_name: name,
                    rule: "R2",
                    detail: "bare `#[tracing::instrument]` has no `name = \"...\"` (parenthesised form required)"
                        .to_string(),
                });
                continue;
            }
        };

        match extract_name(&parts) {
            None => findings.push(Finding {
                file: rel_path.to_string(),
                line_no: idx + 1,
                fn_name: name.clone(),
                rule: "R2",
                detail: "instrument attribute missing explicit `name = \"...\"`".to_string(),
            }),
            Some(n) if !n.contains('.') => findings.push(Finding {
                file: rel_path.to_string(),
                line_no: idx + 1,
                fn_name: name.clone(),
                rule: "R3",
                detail: format!(
                    "instrument `name = \"{n}\"` does not follow dotted `module.function` convention"
                ),
            }),
            _ => {}
        }

        let signature = collect_signature(&lines, idx);
        if signature_returns_result(&signature) && !has_err_directive(&parts) {
            findings.push(Finding {
                file: rel_path.to_string(),
                line_no: idx + 1,
                fn_name: name,
                rule: "R4",
                detail: "Result-returning fn missing `err(Display)` / `err(..)` directive"
                    .to_string(),
            });
        }
    }
    findings
}

/// Recursively collect every `.rs` file under `dir`. Stable-order so
/// failure output is deterministic.
fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_rs_files(&path));
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

// =============================================================================
// Enforcement tests
// =============================================================================

#[test]
fn r1_to_r4_every_instrumented_async_fn_follows_convention() {
    let mut findings: Vec<String> = Vec::new();
    for file in FILES_TO_SCAN {
        for f in scan_file(file) {
            findings.push(f.format());
        }
    }
    assert!(
        findings.is_empty(),
        "instrument-coverage violations (see CONTRIBUTING.md §Logging):\n{}",
        findings.join("\n")
    );
}

#[test]
fn r5_no_print_or_dbg_macros_in_non_test_src() {
    let src_root = repo_root().join("src");
    let mut findings: Vec<String> = Vec::new();
    for file in collect_rs_files(&src_root) {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        let rel = file.strip_prefix(repo_root()).unwrap_or(&file);
        let lines: Vec<&str> = content.lines().collect();
        let test_boundary = cfg_test_boundary(&lines).unwrap_or(lines.len());
        for (i, line) in lines.iter().enumerate() {
            if i >= test_boundary {
                break;
            }
            let code = strip_line_comment(line);
            for forbidden in FORBIDDEN_MACROS {
                if code.contains(forbidden) {
                    findings.push(format!(
                        "R5 — {}:{} uses forbidden macro `{}` (use tracing macros instead)",
                        rel.display(),
                        i + 1,
                        forbidden
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "forbidden logging macros in non-test src/:\n{}",
        findings.join("\n")
    );
}

// =============================================================================
// Unit tests for the text-walk helpers
// =============================================================================

#[test]
fn parse_async_fn_decl_recognizes_visibility_variants() {
    assert_eq!(
        parse_async_fn_decl("    pub async fn write_block(").as_deref(),
        Some("write_block")
    );
    assert_eq!(
        parse_async_fn_decl("async fn process_chunk(").as_deref(),
        Some("process_chunk")
    );
    assert_eq!(
        parse_async_fn_decl("    pub(crate) async fn foo<'a>(").as_deref(),
        Some("foo")
    );
    assert_eq!(parse_async_fn_decl("fn not_async() {}"), None);
    assert_eq!(parse_async_fn_decl("// async fn commented_out("), None);
}

#[test]
fn is_trait_decl_distinguishes_decl_from_impl() {
    let decl_lines = vec![
        "    async fn get_blocks(&self, start_slot: u64, end_slot: u64) -> Result<Vec<u64>, PipelineError>;",
    ];
    assert!(is_trait_decl(&decl_lines, 0));

    let impl_lines = vec![
        "    async fn get_blocks(&self, start_slot: u64, end_slot: u64) -> Result<Vec<u64>, PipelineError> {",
        "        // body",
        "    }",
    ];
    assert!(!is_trait_decl(&impl_lines, 0));

    let multiline_decl = vec![
        "    async fn get_multiple_accounts(",
        "        &self,",
        "        pubkeys: &[String],",
        "    ) -> Result<Vec<RpcAccountInfo>, PipelineError>;",
    ];
    assert!(is_trait_decl(&multiline_decl, 0));
}

#[test]
fn signature_returns_result_detects_result_types() {
    assert!(signature_returns_result(
        "pub async fn foo(&self) -> Result<u64, PipelineError> { "
    ));
    assert!(!signature_returns_result(
        "    pub async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) { "
    ));
    assert!(!signature_returns_result("pub async fn noop(&self) { "));
    // Multi-line signature collapsed with `where` clause.
    assert!(signature_returns_result(
        "pub async fn generic<T>(x: T) -> Result<T, Error> where T: Send { "
    ));
    // Function-pointer param that itself returns `Result` must not
    // confuse the outer return-type check.
    assert!(!signature_returns_result(
        "pub async fn with_cb(cb: fn() -> Result<u64, Error>) -> () { "
    ));
    assert!(signature_returns_result(
        "pub async fn with_cb(cb: fn() -> u64) -> Result<u64, Error> { "
    ));
}

#[test]
fn find_instrument_opener_matches_single_and_multi_line() {
    // Multi-line attribute.
    let lines = vec![
        "    /// doc comment",
        "    #[tracing::instrument(",
        "        name = \"storage.write_block\",",
        "        skip(self),",
        "        err(Display)",
        "    )]",
        "    pub async fn write_block(",
    ];
    assert_eq!(find_instrument_opener(&lines, 6), Some(1));

    // Single-line attribute.
    let lines = vec![
        "#[tracing::instrument(name = \"api.health\", skip(state), level = \"info\")]",
        "pub async fn health(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {",
    ];
    assert_eq!(find_instrument_opener(&lines, 1), Some(0));

    // No attribute at all.
    let lines = vec!["    /// doc comment", "    pub async fn write_block("];
    assert_eq!(find_instrument_opener(&lines, 1), None);

    // Another attribute macro sandwiched between instrument and fn.
    let lines = vec![
        "    #[tracing::instrument(name = \"x.y\", skip(self))]",
        "    #[allow(clippy::too_many_arguments)]",
        "    pub async fn foo(",
    ];
    assert_eq!(find_instrument_opener(&lines, 2), Some(0));
}

#[test]
fn extract_instrument_body_handles_single_and_multi_line_forms() {
    let lines = vec![
        "#[tracing::instrument(name = \"api.health\", skip(state))]",
        "pub async fn health() -> () {",
    ];
    let body = extract_instrument_body(&lines, 0, 1).expect("body present");
    assert!(body.contains("name = \"api.health\""));
    assert!(body.contains("skip(state)"));

    let lines = vec![
        "#[tracing::instrument(",
        "    name = \"storage.write_block\",",
        "    skip(self),",
        "    err(Display)",
        ")]",
        "pub async fn write_block() -> Result<(), ()> {",
    ];
    let body = extract_instrument_body(&lines, 0, 5).expect("body present");
    assert!(body.contains("name = \"storage.write_block\""));
    assert!(body.contains("err(Display)"));
}

#[test]
fn split_top_level_respects_parens_and_strings() {
    let body = r#" name = "x.y", skip(self, foo), fields(a = 1, b = 2), err(Display) "#;
    assert_eq!(
        split_top_level(body),
        vec![
            "name = \"x.y\"".to_string(),
            "skip(self, foo)".to_string(),
            "fields(a = 1, b = 2)".to_string(),
            "err(Display)".to_string(),
        ]
    );

    // Comma inside a string literal must not split.
    let body = r#" name = "x,y", err(Display) "#;
    assert_eq!(
        split_top_level(body),
        vec!["name = \"x,y\"".to_string(), "err(Display)".to_string(),]
    );
}

#[test]
fn extract_name_finds_string_literal_name() {
    let parts = vec![
        "name = \"storage.write_block\"".to_string(),
        "skip(self)".to_string(),
        "err(Display)".to_string(),
    ];
    assert_eq!(extract_name(&parts).as_deref(), Some("storage.write_block"));

    let parts = vec!["skip(self)".to_string()];
    assert_eq!(extract_name(&parts), None);
}

#[test]
fn has_err_directive_matches_common_forms() {
    assert!(has_err_directive(&["err(Display)".to_string()]));
    assert!(has_err_directive(&["err".to_string()]));
    assert!(has_err_directive(&["err(Debug)".to_string()]));
    assert!(!has_err_directive(&["fields(error = 1)".to_string()]));
    assert!(!has_err_directive(&["ret".to_string()]));
    assert!(!has_err_directive(&[
        "name = \"x.y\"".to_string(),
        "skip(self)".to_string(),
    ]));
}
