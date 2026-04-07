//! Story 6.1 AC7 — log-level discipline enforcement.
//!
//! R1 (HARD): warn!/error! in src/pipeline/{mod,ws}.rs must carry program_id,
//!            either as a macro arg or via the enclosing function's
//!            #[tracing::instrument(fields(...))] attribute.
//! R2 (HARD): no info! inside `for tx in &block.transactions` or
//!            `for &slot in &block_slots` loops in src/pipeline/mod.rs.
//! R3 (SOFT): error! on retryable-error variants prints a warning (not yet
//!            a hard gate; tightens once DecodeError::variant_name lands).
//!
//! Pure text walk — no `syn`, no `regex`, no `walkdir`. Pattern follows the
//! Story 6.4 text-walk precedent in `tests/idl_address_vectors.rs`.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// File loading + per-line "inside #[cfg(test)] mod" mask
// ---------------------------------------------------------------------------

struct FileScan {
    path: String,
    lines: Vec<String>,
    /// Parallel to `lines`: true iff the line is inside a `#[cfg(test)] mod` body.
    in_test_mod: Vec<bool>,
}

impl FileScan {
    fn load(path: &Path) -> Self {
        let raw = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let lines: Vec<String> = raw.lines().map(|s| s.to_string()).collect();
        let in_test_mod = compute_test_mod_mask(&lines);
        Self {
            path: path.display().to_string(),
            lines,
            in_test_mod,
        }
    }
}

/// Walk lines and mark every line that lives inside the body of a
/// `#[cfg(test)] mod ... { ... }` block. Brace-balanced; ignores braces
/// inside `//`-comments and string literals (best-effort — sufficient for
/// the Solarix codebase).
fn compute_test_mod_mask(lines: &[String]) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        // Look for a `#[cfg(test)]` attribute on this line.
        if line.contains("#[cfg(test)]") {
            // Search forward for the next `mod <name> {` (or `mod <name>` then `{`).
            // We accept any non-empty line beginning with `mod `.
            let mut j = i + 1;
            // Skip blank/attribute lines between cfg(test) and mod.
            while j < lines.len() {
                let trimmed = lines[j].trim_start();
                if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("#[") {
                    j += 1;
                    continue;
                }
                break;
            }
            if j < lines.len() && lines[j].trim_start().starts_with("mod ") {
                // Find the opening `{`. It could be on line `j` or a later line.
                let mut k = j;
                let mut found_open = false;
                while k < lines.len() {
                    if line_contains_unquoted(&lines[k], '{') {
                        found_open = true;
                        break;
                    }
                    k += 1;
                }
                if found_open {
                    // Brace-balance from line k onward.
                    let mut depth: i64 = 0;
                    let mut started = false;
                    let mut m = k;
                    while m < lines.len() {
                        let opens = count_unquoted(&lines[m], '{') as i64;
                        let closes = count_unquoted(&lines[m], '}') as i64;
                        depth += opens - closes;
                        if opens > 0 {
                            started = true;
                        }
                        // Mark this entire line as inside the test mod.
                        // (Including the opening `mod tests {` line itself —
                        // safe because no log macros live on that line.)
                        mask[m] = true;
                        if started && depth <= 0 {
                            // matched closing brace
                            i = m + 1;
                            break;
                        }
                        m += 1;
                    }
                    if m >= lines.len() {
                        i = lines.len();
                    }
                    continue;
                }
            }
        }
        i += 1;
    }
    mask
}

/// Count occurrences of `c` in `s`, ignoring characters inside `//` line
/// comments and basic `"..."` string literals. Best-effort — sufficient for
/// this codebase. Does NOT handle raw strings (`r#"..."#`); those are not
/// used in the pipeline source files we scan.
fn count_unquoted(s: &str, c: char) -> usize {
    let mut count = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '/' {
            if let Some(&'/') = chars.peek() {
                // Rest of line is a comment.
                break;
            }
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == c {
            count += 1;
        }
    }
    count
}

fn line_contains_unquoted(s: &str, c: char) -> bool {
    count_unquoted(s, c) > 0
}

// ---------------------------------------------------------------------------
// Macro call extraction
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct MacroCall {
    /// 1-based line number where the macro starts (the line containing `name!(`).
    start_line: usize,
    /// Concatenated macro args between the outermost `(` and matching `)`.
    args: String,
}

/// Find every `name!(...)` call in the file. Skips lines inside test mods.
/// Skips lines whose `name!` substring is preceded by a non-identifier char
/// other than whitespace/`.`/`{`/`(`/`!`/`,` (e.g. a comment marker is fine
/// because we already strip comments via the unquoted helpers when scanning).
fn find_macro_calls(scan: &FileScan, name: &str) -> Vec<MacroCall> {
    let needle = format!("{name}!(");
    let mut out = Vec::new();
    let mut i = 0;
    while i < scan.lines.len() {
        if scan.in_test_mod[i] {
            i += 1;
            continue;
        }
        let line = &scan.lines[i];
        // Strip line comments before searching, so `// warn!(...)` is ignored.
        let stripped = strip_line_comment(line);
        if let Some(idx) = stripped.find(&needle) {
            // Verify the char before `name` is not part of an identifier
            // (so we don't match `info!_other` or `tracing::info!`-style calls
            // — actually `tracing::info!(` should match too; the `:` before
            // `info` is fine).
            let preceding_ok = idx == 0 || {
                let prev = stripped[..idx].chars().last().unwrap_or(' ');
                !prev.is_ascii_alphanumeric() && prev != '_'
            };
            if preceding_ok {
                // Extract args by paren-balancing from the `(` after `name!`.
                let open_pos = idx + needle.len() - 1; // position of `(`
                let (args, _end_line) = extract_paren_args(&scan.lines, i, open_pos);
                out.push(MacroCall {
                    start_line: i + 1,
                    args,
                });
            }
        }
        i += 1;
    }
    out
}

/// Strip a `// ...` line comment from `s`, respecting basic string quoting.
fn strip_line_comment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '/' {
            if let Some(&'/') = chars.peek() {
                break;
            }
        }
        if ch == '"' {
            in_string = true;
        }
        out.push(ch);
    }
    out
}

/// Starting from `lines[start_line]` at byte position `open_pos` (which must
/// be a `(`), accumulate characters until the matching `)`. Returns
/// `(args_string, end_line_index)`.
fn extract_paren_args(lines: &[String], start_line: usize, open_pos: usize) -> (String, usize) {
    let mut args = String::new();
    let mut depth: i64 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut started = false;
    let mut line_idx = start_line;

    'outer: while line_idx < lines.len() {
        let line = &lines[line_idx];
        let start_byte = if line_idx == start_line { open_pos } else { 0 };
        // Iterate by char from start_byte.
        let slice = &line[start_byte..];
        for ch in slice.chars() {
            if in_string {
                args.push(ch);
                if escape {
                    escape = false;
                } else if ch == '\\' {
                    escape = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }
            if ch == '"' {
                in_string = true;
                args.push(ch);
                continue;
            }
            if ch == '(' {
                depth += 1;
                started = true;
                if depth == 1 {
                    // Don't push the outermost open paren.
                    continue;
                }
                args.push(ch);
                continue;
            }
            if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    break 'outer;
                }
                args.push(ch);
                continue;
            }
            if started {
                args.push(ch);
            }
        }
        // Newline between lines becomes a space so substring checks still work.
        args.push(' ');
        line_idx += 1;
    }
    (args, line_idx)
}

// ---------------------------------------------------------------------------
// Enclosing-function + #[instrument] lookup
// ---------------------------------------------------------------------------

/// Scan backwards from `from_line` (0-based) for the nearest preceding line
/// containing a `fn ` declaration (function or method). Returns the line
/// index of that declaration, or `None`.
fn find_enclosing_fn_line(lines: &[String], from_line: usize) -> Option<usize> {
    let mut i = from_line;
    loop {
        let line = strip_line_comment(&lines[i]);
        // Match `    fn name`, `pub fn name`, `pub(crate) fn`, `async fn`,
        // `pub async fn`, `pub(crate) async fn`, etc. We accept any line
        // where the first non-whitespace token chain ends in `fn `.
        let trimmed = line.trim_start();
        if trimmed.starts_with("fn ")
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub(crate) fn ")
            || trimmed.starts_with("pub(super) fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("pub(crate) async fn ")
            || trimmed.starts_with("pub(super) async fn ")
        {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Walk backwards from `fn_line - 1` over consecutive attribute lines and
/// return the concatenated text of every `#[instrument(...)]` /
/// `#[tracing::instrument(...)]` attribute attached to the function. The
/// attribute block ends at the first non-attribute, non-blank, non-doc-comment
/// line.
fn collect_instrument_attrs(lines: &[String], fn_line: usize) -> String {
    if fn_line == 0 {
        return String::new();
    }
    // Walk backwards to find the start of the attribute block.
    let mut start = fn_line;
    let mut i = fn_line;
    while i > 0 {
        i -= 1;
        let trimmed = lines[i].trim_start();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("#[")
            || trimmed.starts_with("#![")
            // continuation of a multi-line attribute — anything that doesn't
            // look like the start of a new statement
            || !is_statement_start(trimmed)
        {
            start = i;
            continue;
        }
        // Hit a non-attribute statement line — stop.
        break;
    }

    // Now find every `#[instrument(...)]` / `#[tracing::instrument(...)]` in
    // [start..fn_line) by paren-balancing from the `(`.
    let mut collected = String::new();
    let mut k = start;
    while k < fn_line {
        let line = &lines[k];
        let stripped = strip_line_comment(line);
        let mut search_from = 0;
        loop {
            let needle_a = "#[instrument(";
            let needle_b = "#[tracing::instrument(";
            let pos_a = stripped[search_from..]
                .find(needle_a)
                .map(|p| (p, needle_a));
            let pos_b = stripped[search_from..]
                .find(needle_b)
                .map(|p| (p, needle_b));
            let (rel_pos, needle) = match (pos_a, pos_b) {
                (Some(a), Some(b)) => {
                    if a.0 <= b.0 {
                        a
                    } else {
                        b
                    }
                }
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => break,
            };
            let abs_pos = search_from + rel_pos;
            let open_pos = abs_pos + needle.len() - 1;
            let (args, _end) = extract_paren_args(lines, k, open_pos);
            collected.push_str(&args);
            collected.push(' ');
            search_from = abs_pos + needle.len();
        }
        k += 1;
    }
    collected
}

/// Heuristic: a line is a "statement start" (rather than an attribute or
/// continuation) if it begins with a token that introduces an item / use /
/// statement. Anything else (closing braces, continuations) is treated as
/// allowed to live above an attribute block.
fn is_statement_start(trimmed: &str) -> bool {
    // The most common item starters that mark the *end* of a preceding attr
    // block walk. Anything we don't recognize is conservatively treated as
    // a continuation (returns false), which simply extends the attribute
    // window — harmless because we only collect `#[instrument(...)]` text
    // within it.
    const STARTERS: &[&str] = &[
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "pub(super) fn ",
        "async fn ",
        "pub async fn ",
        "pub(crate) async fn ",
        "pub(super) async fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "impl ",
        "use ",
        "pub use ",
        "mod ",
        "pub mod ",
        "trait ",
        "pub trait ",
        "const ",
        "pub const ",
        "static ",
        "pub static ",
        "type ",
        "pub type ",
        "let ",
        "}",
    ];
    STARTERS.iter().any(|s| trimmed.starts_with(s))
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// R1: every warn!/error! in pipeline/{mod,ws}.rs must carry program_id either
/// in its own args or via its enclosing function's #[instrument] fields.
fn rule_r1_pipeline_warns_carry_program_id(scan: &FileScan) -> Vec<String> {
    let mut violations = Vec::new();
    for level in ["warn", "error"] {
        for call in find_macro_calls(scan, level) {
            if call.args.contains("program_id") {
                continue;
            }
            // Look for an enclosing #[instrument] with program_id in fields.
            let line_idx = call.start_line - 1;
            let approved = match find_enclosing_fn_line(&scan.lines, line_idx) {
                Some(fn_line) => {
                    let attrs = collect_instrument_attrs(&scan.lines, fn_line);
                    attrs.contains("program_id")
                }
                None => false,
            };
            if !approved {
                violations.push(format!(
                    "{}:{}: {}!({}) is missing program_id (not in args, not in enclosing #[instrument] fields)",
                    scan.path,
                    call.start_line,
                    level,
                    truncate(&call.args, 80),
                ));
            }
        }
    }
    violations
}

/// R2: no info! inside per-tx / per-slot hot loops in pipeline/mod.rs.
fn rule_r2_no_info_in_hot_loops(scan: &FileScan) -> Vec<String> {
    let mut violations = Vec::new();
    // Find every for-loop start line that matches the per-tx / per-slot
    // shape, then brace-balance from the `{` at end of that line through
    // the matching `}`. Within that range, flag any `info!(` calls.
    let info_calls: Vec<MacroCall> = find_macro_calls(scan, "info");

    for (i, line) in scan.lines.iter().enumerate() {
        if scan.in_test_mod[i] {
            continue;
        }
        let stripped = strip_line_comment(line);
        let trimmed = stripped.trim_start();
        // Match `for ... in &block.transactions` or `for ... in &block_slots`.
        let is_tx_loop = trimmed.starts_with("for ")
            && trimmed.contains(" in ")
            && trimmed.contains("block.transactions");
        let is_slot_loop = trimmed.starts_with("for ")
            && trimmed.contains(" in ")
            && trimmed.contains("block_slots");
        if !(is_tx_loop || is_slot_loop) {
            continue;
        }
        // Find the matching closing brace by tracking depth from the first
        // `{` at-or-after this line.
        let (open_line_idx, open_col_in_line) = match find_first_brace(&scan.lines, i) {
            Some(p) => p,
            None => continue,
        };
        let close_line_idx =
            match find_matching_close_brace(&scan.lines, open_line_idx, open_col_in_line) {
                Some(p) => p,
                None => continue,
            };
        // Now flag any info! call whose start_line lies in
        // [open_line_idx+1 .. close_line_idx+1] (1-based start_line).
        // We do an inclusive comparison on 1-based line numbers.
        let lo = open_line_idx + 1; // 0-based -> 1-based
        let hi = close_line_idx + 1;
        for call in &info_calls {
            if call.start_line >= lo && call.start_line <= hi {
                violations.push(format!(
                    "{}:{}: info!({}) inside per-tx/per-slot hot loop starting at line {}",
                    scan.path,
                    call.start_line,
                    truncate(&call.args, 80),
                    i + 1,
                ));
            }
        }
    }
    violations
}

/// R3 (SOFT): error! calls whose args contain RetryableRpc / RateLimited /
/// 429 / Timeout — these should be warn!, not error!. Prints to stdout but
/// does NOT fail the test.
//
// TODO(6.1): tighten to hard gate when DecodeError variant_name lands
fn rule_r3_no_error_on_retryable(scan: &FileScan) -> Vec<String> {
    let mut warnings = Vec::new();
    for call in find_macro_calls(scan, "error") {
        let a = &call.args;
        if a.contains("RetryableRpc")
            || a.contains("RateLimited")
            || a.contains("429")
            || a.contains("Timeout")
        {
            warnings.push(format!(
                "{}:{}: error!({}) mentions a retryable error class — should be warn!",
                scan.path,
                call.start_line,
                truncate(a, 80),
            ));
        }
    }
    warnings
}

// ---------------------------------------------------------------------------
// Helpers: brace tracking for for-loop body extents
// ---------------------------------------------------------------------------

/// Starting at `start_line`, find the first `{` (line index, column index).
/// Searches at most 5 lines forward (for `for ... in ... \n {` patterns).
fn find_first_brace(lines: &[String], start_line: usize) -> Option<(usize, usize)> {
    let max = (start_line + 5).min(lines.len());
    for (i, line) in lines.iter().enumerate().take(max).skip(start_line) {
        let stripped = strip_line_comment(line);
        if let Some(col) = stripped.find('{') {
            return Some((i, col));
        }
    }
    None
}

/// Find the line index of the matching `}` for a `{` at `(open_line, open_col)`.
fn find_matching_close_brace(lines: &[String], open_line: usize, open_col: usize) -> Option<usize> {
    let mut depth: i64 = 0;
    for (i, line) in lines.iter().enumerate().skip(open_line) {
        let start = if i == open_line { open_col } else { 0 };
        let slice = &line[start..];
        let stripped = strip_line_comment(slice);
        depth += count_unquoted(&stripped, '{') as i64;
        depth -= count_unquoted(&stripped, '}') as i64;
        if depth <= 0 {
            return Some(i);
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= max {
        collapsed
    } else {
        format!("{}…", &collapsed[..max])
    }
}

// ---------------------------------------------------------------------------
// File walking
// ---------------------------------------------------------------------------

/// Walk every `*.rs` file under `src/`, returning absolute paths sorted for
/// deterministic test output.
fn walk_src_rs_files() -> Vec<PathBuf> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let src = PathBuf::from(&manifest_dir).join("src");
    let mut out = Vec::new();
    walk_rec(&src, &mut out);
    out.sort();
    out
}

fn walk_rec(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rec(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn pipeline_files() -> Vec<PathBuf> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    vec![
        PathBuf::from(&manifest_dir).join("src/pipeline/mod.rs"),
        PathBuf::from(&manifest_dir).join("src/pipeline/ws.rs"),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn log_level_discipline_pipeline_warns() {
    let mut all_violations: Vec<String> = Vec::new();
    for path in pipeline_files() {
        let scan = FileScan::load(&path);
        let v = rule_r1_pipeline_warns_carry_program_id(&scan);
        all_violations.extend(v);
    }
    if !all_violations.is_empty() {
        let joined = all_violations.join("\n");
        panic!(
            "R1 (pipeline warn!/error! must carry program_id) failed for {} call(s):\n{joined}\n\nFix: either add `program_id` to the macro args, or add it to the enclosing function's #[tracing::instrument(fields(...))] attribute.",
            all_violations.len()
        );
    }
}

#[test]
fn log_level_discipline_hot_loops() {
    let path = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo"),
    )
    .join("src/pipeline/mod.rs");
    let scan = FileScan::load(&path);
    let violations = rule_r2_no_info_in_hot_loops(&scan);
    if !violations.is_empty() {
        let joined = violations.join("\n");
        panic!(
            "R2 (no info! in per-tx/per-slot hot loops) failed for {} call(s):\n{joined}\n\nFix: downgrade these to debug! — per-block / per-tx hot paths must not log at info!.",
            violations.len()
        );
    }
}

#[test]
fn log_level_discipline_retryable_errors_soft_warning() {
    // SOFT rule — emit a stdout warning but never fail.
    // TODO(6.1): tighten to hard gate when DecodeError variant_name lands.
    let mut total = 0usize;
    for path in walk_src_rs_files() {
        let scan = FileScan::load(&path);
        let warnings = rule_r3_no_error_on_retryable(&scan);
        for w in &warnings {
            println!("[log_levels:R3 soft warning] {w}");
        }
        total += warnings.len();
    }
    if total > 0 {
        println!(
            "[log_levels:R3] {total} soft warning(s) above. R3 is currently advisory; tighten when DecodeError::variant_name lands."
        );
    }
}
