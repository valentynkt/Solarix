//! Story 6.1 AC1 — every public/module-crossing `async fn` in the tracing
//! scope must carry `#[tracing::instrument]`. Pure text walk — no `syn`, no
//! `regex`, no `walkdir`. Follows the Story 6.4 text-walk precedent.
//!
//! Why a text walk and not `syn`?  Story 6.4 deliberately avoided pulling
//! procedural-macro dependencies into the integration test surface to keep
//! `cargo test` cold-build time low and the dep tree honest. This test
//! follows the same convention. The trade-off is that the scanner is
//! line-oriented and slightly conservative, but the rules below are tight
//! enough to catch the only failure mode that matters for AC1: a newly
//! added `async fn` that ships without `#[tracing::instrument]`.
//!
//! `src/storage/queries.rs` is intentionally absent from `FILES_TO_SCAN` —
//! it contains zero `async fn` declarations (the `QueryBuilder` there is
//! pure synchronous SQL assembly), so instrumenting it would be vacuous.
//! If a future change introduces an `async fn` in that file, add the path
//! back to `FILES_TO_SCAN` rather than re-deriving the exemption.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

/// Files in the Story 6.1 instrumentation scope. Every non-test
/// `async fn` declared in any of these files (excluding bare trait method
/// declarations, which carry the attribute on their impl) must have a
/// `#[tracing::instrument]` (or `#[instrument]`) attribute attached
/// directly to it.
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

/// One uninstrumented finding from the scan.
#[derive(Debug)]
struct Finding {
    file: String,
    line_no: usize,
    fn_name: String,
}

/// Locate the repository root by walking up from `CARGO_MANIFEST_DIR`.
///
/// `cargo test` always sets `CARGO_MANIFEST_DIR` to the package root, which
/// for a single-crate workspace IS the repo root. Returned as a `PathBuf`
/// so callers can join the relative `FILES_TO_SCAN` paths against it.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Strip a leading `pub` / `pub(crate)` / `pub(super)` visibility modifier
/// from `s` and return the rest. Returns the original `s` if no visibility
/// modifier is present. Used to normalize fn declaration lines before
/// matching `async fn`.
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

/// Returns `Some(fn_name)` if the line declares an `async fn`, otherwise
/// `None`. Matches lines like:
///
/// ```text
///     pub async fn write_block(
///         async fn process_chunk(
///     pub(crate) async fn foo<'a>(
/// ```
///
/// Does NOT distinguish trait declarations from impl methods — that's the
/// caller's job (look-ahead for `;` vs `{`).
fn parse_async_fn_decl(line: &str) -> Option<String> {
    let stripped = strip_visibility(line);
    let after_async = stripped.strip_prefix("async fn ")?;
    // The function name runs until the first `<` (generic), `(` (params),
    // or whitespace.
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

/// Walk forward from `start_idx` (inclusive) through `lines` looking for
/// the terminator that decides whether the `async fn` at `start_idx` is a
/// trait method declaration (`;`) or a real definition (`{`). Returns
/// `true` if it's a trait declaration that should be skipped.
///
/// Trait method bodies in this codebase are always single-line OR end with
/// `;` within ~10 lines (no trait method has 20+ lines of arg list), so a
/// 25-line lookahead is plenty.
fn is_trait_decl(lines: &[&str], start_idx: usize) -> bool {
    let end = (start_idx + 25).min(lines.len());
    for line in &lines[start_idx..end] {
        // Strip trailing whitespace and trailing line comments before
        // checking for the terminator.
        let trimmed = line.split("//").next().unwrap_or(line).trim_end();
        if trimmed.ends_with('{') {
            return false;
        }
        if trimmed.ends_with(';') {
            return true;
        }
    }
    // Defensive: if we can't find a terminator within 25 lines, assume it's
    // a real fn (so a missing instrument attribute still flags). This is
    // safer than silently skipping a runaway match.
    false
}

/// Walk backwards up to 25 non-blank lines from `idx` and look for an
/// `#[tracing::instrument` or `#[instrument` attribute. Stop walking when
/// we hit a line that is clearly NOT part of the attribute block for this
/// fn (a closing brace `}`, an `impl ` / `fn ` / `mod ` / `struct ` /
/// `trait ` / `enum ` line, or a previous `async fn` declaration).
///
/// Returns `true` if an instrument attribute is found.
fn has_instrument_attribute(lines: &[&str], idx: usize) -> bool {
    let lookback = 25usize;
    let start = idx.saturating_sub(lookback);
    // Walk from `idx - 1` back down to `start`.
    for i in (start..idx).rev() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            // Doc comments belong to the fn — keep walking past them.
            continue;
        }
        if trimmed.starts_with("//") {
            // Plain line comments are also fine to walk past.
            continue;
        }
        if trimmed.starts_with("#[tracing::instrument") || trimmed.starts_with("#[instrument") {
            return true;
        }
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            // Some OTHER attribute (e.g. `#[allow(...)]`, `#[async_trait]`).
            // Keep walking — instrument may live above it.
            continue;
        }
        // Multi-line attribute continuation lines (e.g. the inside of
        // `#[tracing::instrument(\n    name = "...",\n)]`) — these don't
        // start with `#[` but ARE part of the attribute block. We treat
        // them as transparent and keep walking, because we will eventually
        // reach the `#[tracing::instrument(` opener if it exists.
        if trimmed.starts_with(')') || trimmed == "]" || trimmed.ends_with(',') {
            continue;
        }
        if trimmed.starts_with("name =")
            || trimmed.starts_with("skip")
            || trimmed.starts_with("fields")
            || trimmed.starts_with("level =")
            || trimmed.starts_with("err(")
            || trimmed.starts_with("ret")
        {
            continue;
        }
        // Anything else means we've left the attribute block for THIS fn.
        return false;
    }
    false
}

/// Returns the line index (0-based) of the start of the `#[cfg(test)]`
/// region in `lines`, if any. Everything from that line forward is treated
/// as test code and excluded from the scan.
fn cfg_test_boundary(lines: &[&str]) -> Option<usize> {
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            return Some(i);
        }
    }
    None
}

/// Scan a single file and return all uninstrumented `async fn`
/// declarations.
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
        if !has_instrument_attribute(&lines, idx) {
            findings.push(Finding {
                file: rel_path.to_string(),
                line_no: idx + 1,
                fn_name: name,
            });
        }
    }
    findings
}

#[test]
fn every_async_fn_in_scope_has_instrument_attribute() {
    let mut missing: Vec<String> = Vec::new();
    for file in FILES_TO_SCAN {
        for f in scan_file(file) {
            missing.push(format!("{}:{} — async fn {}", f.file, f.line_no, f.fn_name));
        }
    }
    assert!(
        missing.is_empty(),
        "uninstrumented async fns found:\n{}",
        missing.join("\n")
    );
}

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
fn has_instrument_attribute_detects_attribute_directly_above_fn() {
    let lines = vec![
        "    /// doc comment",
        "    #[tracing::instrument(",
        "        name = \"storage.write_block\",",
        "        skip(self),",
        "        level = \"debug\",",
        "        err(Display)",
        "    )]",
        "    pub async fn write_block(",
    ];
    assert!(has_instrument_attribute(&lines, 7));

    let no_attr = vec!["    /// doc comment", "    pub async fn write_block("];
    assert!(!has_instrument_attribute(&no_attr, 1));
}
