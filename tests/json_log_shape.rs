//! Story 6.1 AC8 — JSON log shape contract test.
//!
//! Locks the `tracing-subscriber` JSON output shape so future upgrades cannot
//! silently change it. Mirrors the production config in `src/main.rs`:
//!
//! ```text
//! tracing_subscriber::fmt()
//!     .with_span_events(FmtSpan::CLOSE)
//!     .json()
//!     .with_current_span(true)
//!     .with_span_list(false)
//! ```
//!
//! The asserted contract is intentionally narrow:
//! - top-level `timestamp`, `level`, `target` fields exist;
//! - `with_current_span(true)` produces a `span` (singular) object with a
//!   `name` field;
//! - `with_span_list(false)` suppresses the `spans` (plural) ancestry array;
//! - the user fields and `message` attached to an `info!` event survive
//!   round-tripping into the `fields` object.
//!
//! Hermetic — no DB, no extra dependencies. Single-threaded tokio runtime
//! is required because `tracing::subscriber::set_default` installs the
//! subscriber on the current thread only; multi-threaded executors would
//! lose visibility when the future is parked on a different worker.

#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::MakeWriter;

/// In-memory `MakeWriter` that captures every byte the subscriber writes.
///
/// Cloning the writer hands out additional handles that share the same
/// backing buffer via `Arc<Mutex<Vec<u8>>>`, which is what
/// `tracing_subscriber::fmt::MakeWriter::make_writer` requires (it returns
/// a fresh writer per event).
#[derive(Clone, Default)]
struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturingWriter {
    type Writer = CapturingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Synthetic instrumented function used to drive the subscriber. The
/// `program_id` and `slot` fields stand in for the production correlation
/// fields the pipeline emits via `#[instrument]`.
#[tracing::instrument(skip_all, fields(program_id = "TestProg111", slot = 42u64))]
async fn synthetic_work() {
    tracing::info!(signature = "sigABC", "test event");
}

#[tokio::test(flavor = "current_thread")]
async fn json_event_shape_contains_required_fields() {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let writer = CapturingWriter(Arc::clone(&buf));

    // Mirror `src/main.rs` exactly: `.json()` then the JSON-formatter-specific
    // toggles. `with_ansi(false)` is a defensive default for the in-memory
    // writer; the JSON formatter ignores ANSI anyway.
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .finish();

    // `set_default` returns a per-thread `DefaultGuard` that unsets on drop.
    // Safe in an async fn here because the runtime is `current_thread`, so
    // `synthetic_work()` cannot migrate to another worker mid-await.
    let _guard = tracing::subscriber::set_default(subscriber);

    synthetic_work().await;

    drop(_guard);

    let captured =
        String::from_utf8(buf.lock().expect("buffer mutex poisoned").clone()).expect("utf-8");

    let lines: Vec<&str> = captured.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "no events captured; subscriber wiring is broken"
    );

    // Find the `info!("test event")` line. The span-close event from
    // `FmtSpan::CLOSE` is also in the buffer but we want the user-emitted
    // event because it carries both the instrument fields AND the event
    // fields, which exercises more of the shape at once.
    let event_line = lines
        .iter()
        .find(|l| l.contains(r#""message":"test event""#))
        .copied()
        .expect("test event line not captured; check subscriber config");

    let json: serde_json::Value =
        serde_json::from_str(event_line).expect("event line must be valid JSON");

    // --- top-level shape ---
    assert!(
        json.get("timestamp").is_some(),
        "timestamp field missing on event line: {event_line}"
    );
    let level = json
        .get("level")
        .and_then(|v| v.as_str())
        .expect("level field missing or not a string");
    assert_eq!(level, "INFO", "expected INFO level, got {level}");
    assert!(
        json.get("target").is_some(),
        "target field missing on event line"
    );

    // --- `with_current_span(true)` adds a singular `span` object ---
    let span = json.get("span").expect("span object missing");
    let span_name = span
        .get("name")
        .and_then(|v| v.as_str())
        .expect("span.name missing or not a string");
    assert_eq!(
        span_name, "synthetic_work",
        "span.name should be the instrumented fn name"
    );
    // Instrument fields land on the span object.
    assert_eq!(
        span.get("program_id").and_then(|v| v.as_str()),
        Some("TestProg111"),
        "span.program_id missing or wrong"
    );
    assert_eq!(
        span.get("slot").and_then(|v| v.as_u64()),
        Some(42),
        "span.slot missing or wrong"
    );

    // --- `with_span_list(false)` suppresses the plural ancestry array ---
    assert!(
        json.get("spans").is_none(),
        "`spans` (plural) should be suppressed by with_span_list(false); \
         found {:?}",
        json.get("spans")
    );

    // --- event-level fields land in `fields` ---
    let fields = json
        .get("fields")
        .expect("fields object missing on event line");
    assert_eq!(
        fields.get("message").and_then(|v| v.as_str()),
        Some("test event"),
        "fields.message missing or wrong"
    );
    assert_eq!(
        fields.get("signature").and_then(|v| v.as_str()),
        Some("sigABC"),
        "fields.signature missing or wrong"
    );
}
