// std library
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

// external crates
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};

// internal crate
use super::PipelineError;
use crate::config::Config;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Total budget for `connect_async` + subscribe handshake response. Protects
/// against stalled DNS, TLS handshake blackholes, and unresponsive servers.
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard cap on the base58 signature length we accept from a notification.
/// Solana signatures are 88 chars; 128 leaves headroom without permitting
/// unbounded heap growth from a buggy/malicious upstream.
const MAX_SIGNATURE_LEN: usize = 128;

/// Upper bound applied to `dedup_cache_size` capacity hints so a pathological
/// config value cannot pre-allocate hundreds of GB at startup.
const DEDUP_CAPACITY_HINT_CAP: usize = 1 << 20;

/// Fallback deadline duration if `Instant + Duration` would overflow.
const FALLBACK_DEADLINE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// StreamEvent
// ---------------------------------------------------------------------------

/// Event received from WebSocket transaction stream.
#[derive(Debug)]
pub struct StreamEvent {
    pub signature: String,
    pub slot: u64,
    pub error: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// DeduplicationSet
// ---------------------------------------------------------------------------

/// Bounded deduplication set using HashSet + VecDeque for FIFO eviction.
pub struct DeduplicationSet {
    seen: HashSet<String>,
    order: VecDeque<String>,
    max_size: usize,
}

impl DeduplicationSet {
    pub fn new(max_size: usize) -> Self {
        let hint = max_size.min(DEDUP_CAPACITY_HINT_CAP);
        Self {
            seen: HashSet::with_capacity(hint),
            order: VecDeque::with_capacity(hint),
            max_size,
        }
    }

    /// Insert a signature. Returns `true` if new, `false` if duplicate.
    pub fn insert(&mut self, sig: String) -> bool {
        if self.seen.contains(&sig) {
            return false;
        }
        if self.seen.len() >= self.max_size {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(sig.clone());
        self.order.push_back(sig);
        true
    }

    pub fn contains(&self, sig: &str) -> bool {
        self.seen.contains(sig)
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC deserialization types (private)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WsJsonRpcEnvelope {
    /// Present on server-push notifications (e.g. "logsNotification").
    method: Option<String>,
    /// Present on JSON-RPC responses to client requests.
    result: Option<serde_json::Value>,
    /// Present on JSON-RPC errors (either responses or server-initiated).
    error: Option<WsJsonRpcError>,
}

#[derive(Deserialize)]
struct WsJsonRpcError {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct LogsNotification {
    params: LogsNotificationParams,
}

#[derive(Deserialize)]
struct LogsNotificationParams {
    result: LogsNotificationResult,
    #[allow(dead_code)]
    subscription: u64,
}

#[derive(Deserialize)]
struct LogsNotificationResult {
    value: LogsNotificationValue,
    context: LogsContext,
}

#[derive(Deserialize)]
struct LogsNotificationValue {
    signature: String,
    err: Option<serde_json::Value>,
    #[allow(dead_code)]
    logs: Vec<String>,
}

#[derive(Deserialize)]
struct LogsContext {
    slot: u64,
}

// ---------------------------------------------------------------------------
// TransactionStream trait
// ---------------------------------------------------------------------------

/// Trait for receiving real-time transaction notifications via WebSocket.
#[async_trait]
pub trait TransactionStream: Send + Sync {
    /// Subscribe to transaction logs for a program.
    async fn subscribe(&mut self, program_id: &str) -> Result<(), PipelineError>;

    /// Get the next stream event. Returns error on disconnect.
    async fn next(&mut self) -> Result<Option<StreamEvent>, PipelineError>;

    /// Unsubscribe and close the connection.
    async fn unsubscribe(&mut self) -> Result<(), PipelineError>;

    /// Last observed slot number.
    fn last_seen_slot(&self) -> Option<u64>;
}

// ---------------------------------------------------------------------------
// WsTransactionStream
// ---------------------------------------------------------------------------

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// WebSocket-based transaction stream using `logsSubscribe`.
pub struct WsTransactionStream {
    ws_url: String,
    ws_stream: Option<WsStream>,
    subscription_id: Option<u64>,
    dedup: DeduplicationSet,
    last_seen_slot: Option<u64>,
    ping_interval: Duration,
    pong_timeout: Duration,
    last_message_time: Instant,
    pending_ping: bool,
    last_ping_time: Instant,
    /// Raw text frames received during `subscribe()` before the subscribe
    /// response arrived. Drained by `next()` so handshake-ordering races
    /// never drop data.
    pending_frames: VecDeque<String>,
}

impl WsTransactionStream {
    pub fn new(config: &Config) -> Self {
        let ws_url = match &config.ws_url {
            Some(url) => url.clone(),
            None => derive_ws_url(&config.rpc_url),
        };

        let now = Instant::now();

        Self {
            ws_url,
            ws_stream: None,
            subscription_id: None,
            dedup: DeduplicationSet::new(config.dedup_cache_size),
            last_seen_slot: None,
            ping_interval: Duration::from_secs(config.ws_ping_interval_secs),
            pong_timeout: Duration::from_secs(config.ws_pong_timeout_secs),
            last_message_time: now,
            pending_ping: false,
            last_ping_time: now,
            pending_frames: VecDeque::new(),
        }
    }

    /// Reset all heartbeat / session state. Called at the start of `subscribe()`
    /// to guarantee a resubscribe on the same instance begins with a clean slate.
    fn reset_session_state(&mut self) {
        let now = Instant::now();
        self.last_message_time = now;
        self.last_ping_time = now;
        self.pending_ping = false;
        self.pending_frames.clear();
    }

    /// Compute the next deadline for `tokio::time::sleep_until`, guarding
    /// against `Instant + Duration` overflow (project forbids panics).
    fn next_deadline(&self) -> Instant {
        let (base, delta) = if self.pending_ping {
            (self.last_ping_time, self.pong_timeout)
        } else {
            (self.last_message_time, self.ping_interval)
        };
        base.checked_add(delta)
            .unwrap_or_else(|| Instant::now() + FALLBACK_DEADLINE)
    }

    /// Connect, send `logsSubscribe`, and block until the subscribe response
    /// arrives. Any `logsNotification` frames that race the response are
    /// buffered into `self.pending_frames` so `next()` can replay them.
    /// Split out from `subscribe()` so a single `timeout()` can cover the
    /// whole handshake without needing `async move` (which would consume
    /// `&mut self` for the duration of the await).
    #[tracing::instrument(
        name = "ws.do_handshake",
        skip(self),
        fields(program_id = %program_id, subscription_id = tracing::field::Empty),
        level = "info",
        err(Display)
    )]
    async fn do_handshake(&mut self, program_id: String) -> Result<(), PipelineError> {
        let (ws_stream, _response) = connect_async(&self.ws_url)
            .await
            .map_err(|e| PipelineError::WebSocketDisconnect(format!("connect failed: {e}")))?;

        self.ws_stream = Some(ws_stream);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                {"mentions": [program_id]},
                {"commitment": "confirmed"}
            ]
        });

        let ws = self
            .ws_stream
            .as_mut()
            .ok_or_else(|| PipelineError::WebSocketDisconnect("not connected".into()))?;

        ws.send(Message::Text(request.to_string().into()))
            .await
            .map_err(|e| {
                PipelineError::WebSocketDisconnect(format!("subscribe send failed: {e}"))
            })?;

        // Read subscription response. logsNotification frames that arrive
        // before the response are buffered into `self.pending_frames` so
        // `next()` can replay them.
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let envelope: WsJsonRpcEnvelope = serde_json::from_str(&text).map_err(|e| {
                        PipelineError::WebSocketDisconnect(format!(
                            "failed to parse subscribe response: {e}"
                        ))
                    })?;

                    // A server-push notification raced the subscribe ack —
                    // buffer it so `next()` can replay it.
                    if envelope.method.as_deref() == Some("logsNotification") {
                        self.pending_frames.push_back(text.to_string());
                        continue;
                    }

                    if let Some(error) = envelope.error {
                        return Err(PipelineError::WebSocketDisconnect(format!(
                            "logsSubscribe failed: {} (code {})",
                            error.message, error.code
                        )));
                    }

                    if let Some(result) = envelope.result {
                        let sub_id = result.as_u64().ok_or_else(|| {
                            PipelineError::WebSocketDisconnect(format!(
                                "logsSubscribe returned non-u64 subscription ID: {result}"
                            ))
                        })?;
                        self.subscription_id = Some(sub_id);
                        // Record subscription_id onto the parent span so
                        // AC1 + AC9 both see the field without double-
                        // logging. The parent span (`do_handshake`) is
                        // instrumented with `fields(subscription_id)`.
                        tracing::Span::current().record("subscription_id", sub_id);
                        return Ok(());
                    }
                    // No method, no result, no error — ignore and keep
                    // reading. The timeout wrapper protects us from a
                    // misbehaving server that spams these forever.
                }
                Some(Ok(Message::Ping(data))) => {
                    ws.send(Message::Pong(data)).await.map_err(|e| {
                        PipelineError::WebSocketDisconnect(format!("pong send failed: {e}"))
                    })?;
                }
                Some(Ok(Message::Close(_))) => {
                    return Err(PipelineError::WebSocketDisconnect(
                        "server closed connection during subscribe".into(),
                    ));
                }
                Some(Ok(other)) => {
                    debug!(?other, "ignoring unexpected WS frame during subscribe");
                }
                Some(Err(e)) => {
                    return Err(PipelineError::WebSocketDisconnect(format!(
                        "error during subscribe: {e}"
                    )));
                }
                None => {
                    return Err(PipelineError::WebSocketDisconnect(
                        "stream ended before subscription response".into(),
                    ));
                }
            }
        }
    }
}

#[async_trait]
impl TransactionStream for WsTransactionStream {
    #[tracing::instrument(
        name = "ws.subscribe",
        skip(self),
        fields(program_id = program_id),
        level = "info",
        err(Display)
    )]
    async fn subscribe(&mut self, program_id: &str) -> Result<(), PipelineError> {
        // Clean up any previous session so a resubscribe on the same instance
        // does not leak the prior server-side subscription or carry over stale
        // heartbeat state. `unsubscribe()` is idempotent on an empty session.
        let _ = self.unsubscribe().await;
        self.reset_session_state();

        info!(ws_url = %self.ws_url, program_id, "connecting to WebSocket");

        // Whole handshake (connect + subscribe + read response) shares a
        // single budget. Protects against stalled DNS, TLS handshake
        // blackholes, and servers that accept the connection but never reply.
        let program_id_owned = program_id.to_string();
        timeout(WS_HANDSHAKE_TIMEOUT, self.do_handshake(program_id_owned))
            .await
            .map_err(|_| {
                PipelineError::WebSocketDisconnect(format!(
                    "WebSocket handshake timed out after {}s",
                    WS_HANDSHAKE_TIMEOUT.as_secs()
                ))
            })??;

        self.last_message_time = Instant::now();

        info!(
            program_id,
            subscription_id = ?self.subscription_id,
            "ws logs subscription established"
        );

        Ok(())
    }

    #[tracing::instrument(name = "ws.next", skip(self), level = "debug", err(Display))]
    async fn next(&mut self) -> Result<Option<StreamEvent>, PipelineError> {
        loop {
            // Replay any frames buffered during `subscribe()` first.
            if let Some(text) = self.pending_frames.pop_front() {
                match parse_logs_notification(&text, &mut self.dedup, &mut self.last_seen_slot)? {
                    Some(event) => return Ok(Some(event)),
                    None => continue,
                }
            }

            let deadline = self.next_deadline();

            // Scope the ws borrow so it's released before processing
            let received = {
                let ws = self
                    .ws_stream
                    .as_mut()
                    .ok_or_else(|| PipelineError::WebSocketDisconnect("not connected".into()))?;

                tokio::select! {
                    biased;
                    msg = ws.next() => match msg {
                        Some(msg) => Received::Message(msg),
                        None => Received::StreamEnded,
                    },
                    _ = tokio::time::sleep_until(deadline) => Received::Timeout,
                }
            };

            match received {
                Received::Message(Ok(msg)) => match msg {
                    Message::Text(text) => {
                        self.last_message_time = Instant::now();
                        match parse_logs_notification(
                            &text,
                            &mut self.dedup,
                            &mut self.last_seen_slot,
                        )? {
                            Some(event) => return Ok(Some(event)),
                            None => continue,
                        }
                    }
                    Message::Pong(_) => {
                        self.last_message_time = Instant::now();
                        self.pending_ping = false;
                        continue;
                    }
                    Message::Ping(data) => {
                        self.last_message_time = Instant::now();
                        let ws = self.ws_stream.as_mut().ok_or_else(|| {
                            PipelineError::WebSocketDisconnect("not connected".into())
                        })?;
                        ws.send(Message::Pong(data)).await.map_err(|e| {
                            PipelineError::WebSocketDisconnect(format!("pong send failed: {e}"))
                        })?;
                        continue;
                    }
                    Message::Close(_) => {
                        return Err(PipelineError::WebSocketDisconnect(
                            "server closed connection".into(),
                        ));
                    }
                    other => {
                        debug!(?other, "ignoring unexpected WS frame");
                        continue;
                    }
                },
                Received::Message(Err(e)) => {
                    return Err(PipelineError::WebSocketDisconnect(e.to_string()));
                }
                Received::StreamEnded => {
                    return Err(PipelineError::WebSocketDisconnect("stream ended".into()));
                }
                Received::Timeout => {
                    if self.pending_ping {
                        return Err(PipelineError::WebSocketDisconnect("pong timeout".into()));
                    }
                    let ws = self.ws_stream.as_mut().ok_or_else(|| {
                        PipelineError::WebSocketDisconnect("not connected".into())
                    })?;
                    ws.send(Message::Ping(Default::default()))
                        .await
                        .map_err(|e| {
                            PipelineError::WebSocketDisconnect(format!("ping send failed: {e}"))
                        })?;
                    self.pending_ping = true;
                    self.last_ping_time = Instant::now();
                }
            }
        }
    }

    #[tracing::instrument(name = "ws.unsubscribe", skip(self), level = "debug", err(Display))]
    async fn unsubscribe(&mut self) -> Result<(), PipelineError> {
        if let (Some(ws), Some(sub_id)) = (self.ws_stream.as_mut(), self.subscription_id) {
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "logsUnsubscribe",
                "params": [sub_id]
            });

            let _ = ws.send(Message::Text(request.to_string().into())).await;
            let _ = ws.send(Message::Close(None)).await;
            let _ = ws.flush().await;

            info!(
                subscription_id = sub_id,
                "unsubscribed from logsNotification"
            );
        }

        self.ws_stream = None;
        self.subscription_id = None;

        Ok(())
    }

    fn last_seen_slot(&self) -> Option<u64> {
        self.last_seen_slot
    }
}

// ---------------------------------------------------------------------------
// Helper types and functions
// ---------------------------------------------------------------------------

/// Internal enum to transfer select! results out of the borrow scope.
enum Received {
    Message(Result<Message, tokio_tungstenite::tungstenite::Error>),
    StreamEnded,
    Timeout,
}

/// Derive WebSocket URL from an HTTP RPC URL. Handles trailing whitespace and
/// mixed-case schemes in addition to the common `http(s)://` prefixes.
fn derive_ws_url(rpc_url: &str) -> String {
    let trimmed = rpc_url.trim();
    let lowered = trimmed.to_ascii_lowercase();

    if lowered.starts_with("wss://") || lowered.starts_with("ws://") {
        return trimmed.to_string();
    }
    if let Some(rest) = trimmed.get("https://".len()..) {
        if lowered.starts_with("https://") {
            return format!("wss://{rest}");
        }
    }
    if let Some(rest) = trimmed.get("http://".len()..) {
        if lowered.starts_with("http://") {
            return format!("ws://{rest}");
        }
    }
    trimmed.to_string()
}

/// Parse a JSON-RPC frame. Returns:
/// - `Ok(Some(event))` on a fresh, non-duplicate logsNotification
/// - `Ok(None)` on a duplicate notification or non-notification frame that
///   should be silently skipped
/// - `Err(...)` on a server-pushed JSON-RPC error (propagated so the pipeline
///   can reconnect instead of silently ignoring a killed subscription)
fn parse_logs_notification(
    text: &str,
    dedup: &mut DeduplicationSet,
    last_seen_slot: &mut Option<u64>,
) -> Result<Option<StreamEvent>, PipelineError> {
    // Try generic envelope first to catch server-pushed errors.
    if let Ok(envelope) = serde_json::from_str::<WsJsonRpcEnvelope>(text) {
        if let Some(error) = envelope.error {
            return Err(PipelineError::WebSocketDisconnect(format!(
                "server-pushed error: {} (code {})",
                error.message, error.code
            )));
        }
        // If it's neither a notification nor carries useful data, skip.
        if envelope.method.as_deref() != Some("logsNotification") {
            debug!(text_preview = %preview(text), "non-notification WS message, skipping");
            return Ok(None);
        }
    }

    let notification: LogsNotification = match serde_json::from_str(text) {
        Ok(n) => n,
        Err(_) => {
            warn!(text_preview = %preview(text), "malformed logsNotification, skipping");
            return Ok(None);
        }
    };

    let sig = notification.params.result.value.signature;
    let slot = notification.params.result.context.slot;
    let error = notification.params.result.value.err;

    // Hard cap on signature length — Solana sigs are 88 base58 chars, so any
    // absurdly long value is either corruption or a malicious payload.
    if sig.len() > MAX_SIGNATURE_LEN {
        warn!(
            sig_len = sig.len(),
            "signature exceeds max length, skipping"
        );
        return Ok(None);
    }

    // Update slot cursor BEFORE dedup so duplicate-only windows still advance
    // the cursor — prevents downstream gap detection from over-triggering.
    if last_seen_slot.is_none_or(|s| slot > s) {
        *last_seen_slot = Some(slot);
    }

    if !dedup.insert(sig.clone()) {
        debug!(signature = %sig, "duplicate signature, skipping");
        return Ok(None);
    }

    Ok(Some(StreamEvent {
        signature: sig,
        slot,
        error,
    }))
}

/// First 256 chars of a string, for diagnostic logging without blowing up log volume.
fn preview(text: &str) -> String {
    text.chars().take(256).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- DeduplicationSet tests --

    #[test]
    fn test_dedup_set_insert_and_contains() {
        let mut ds = DeduplicationSet::new(100);
        assert!(ds.insert("sig1".to_string()));
        assert!(ds.contains("sig1"));
        assert!(!ds.insert("sig1".to_string()));
    }

    #[test]
    fn test_dedup_set_eviction() {
        let mut ds = DeduplicationSet::new(3);
        assert!(ds.insert("a".to_string()));
        assert!(ds.insert("b".to_string()));
        assert!(ds.insert("c".to_string()));
        assert_eq!(ds.len(), 3);

        assert!(ds.insert("d".to_string()));
        assert_eq!(ds.len(), 3);
        assert!(!ds.contains("a"));
        assert!(ds.contains("b"));
        assert!(ds.contains("c"));
        assert!(ds.contains("d"));
    }

    #[test]
    fn test_dedup_set_maintains_bounded_size() {
        let max = 100;
        let mut ds = DeduplicationSet::new(max);
        for i in 0..200 {
            ds.insert(format!("sig_{i}"));
        }
        assert_eq!(ds.len(), max);
    }

    // -- derive_ws_url tests --

    #[test]
    fn test_derive_ws_url_https() {
        assert_eq!(
            derive_ws_url("https://api.mainnet-beta.solana.com"),
            "wss://api.mainnet-beta.solana.com"
        );
    }

    #[test]
    fn test_derive_ws_url_http() {
        assert_eq!(
            derive_ws_url("http://localhost:8899"),
            "ws://localhost:8899"
        );
    }

    #[test]
    fn test_derive_ws_url_already_wss() {
        assert_eq!(derive_ws_url("wss://already.good"), "wss://already.good");
        assert_eq!(derive_ws_url("ws://already.good"), "ws://already.good");
    }

    #[test]
    fn test_derive_ws_url_trims_whitespace() {
        assert_eq!(
            derive_ws_url("  https://api.solana.com\n"),
            "wss://api.solana.com"
        );
    }

    #[test]
    fn test_derive_ws_url_mixed_case() {
        // Scheme matching is case-insensitive per RFC 3986, but we preserve
        // host-part casing from the trimmed input.
        assert_eq!(
            derive_ws_url("HTTPS://api.solana.com"),
            "wss://api.solana.com"
        );
    }

    // -- StreamEvent tests --

    #[test]
    fn test_stream_event_creation() {
        let event = StreamEvent {
            signature: "5h6x...abc".to_string(),
            slot: 290_000_000,
            error: None,
        };
        assert_eq!(event.signature, "5h6x...abc");
        assert_eq!(event.slot, 290_000_000);
        assert!(event.error.is_none());

        let event_with_err = StreamEvent {
            signature: "sig2".to_string(),
            slot: 100,
            error: Some(serde_json::json!({"InstructionError": [0, "Custom"]})),
        };
        assert!(event_with_err.error.is_some());
    }

    // -- parse_logs_notification tests --

    #[test]
    fn test_parse_logs_notification_happy_path() {
        let mut dedup = DeduplicationSet::new(10);
        let mut last_slot = None;
        let frame = r#"{
            "jsonrpc":"2.0",
            "method":"logsNotification",
            "params":{
                "result":{
                    "context":{"slot":12345},
                    "value":{"signature":"sigA","err":null,"logs":[]}
                },
                "subscription":1
            }
        }"#;
        let ev = parse_logs_notification(frame, &mut dedup, &mut last_slot)
            .expect("should parse")
            .expect("should yield event");
        assert_eq!(ev.signature, "sigA");
        assert_eq!(ev.slot, 12345);
        assert_eq!(last_slot, Some(12345));
    }

    #[test]
    fn test_parse_logs_notification_propagates_server_error() {
        let mut dedup = DeduplicationSet::new(10);
        let mut last_slot = None;
        let frame =
            r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"subscription cancelled"}}"#;
        let err = parse_logs_notification(frame, &mut dedup, &mut last_slot)
            .expect_err("server error should propagate");
        assert!(matches!(err, PipelineError::WebSocketDisconnect(_)));
    }

    #[test]
    fn test_parse_logs_notification_rejects_oversized_signature() {
        let mut dedup = DeduplicationSet::new(10);
        let mut last_slot = None;
        let huge_sig = "a".repeat(MAX_SIGNATURE_LEN + 1);
        let frame = format!(
            r#"{{
                "jsonrpc":"2.0",
                "method":"logsNotification",
                "params":{{
                    "result":{{
                        "context":{{"slot":1}},
                        "value":{{"signature":"{huge_sig}","err":null,"logs":[]}}
                    }},
                    "subscription":1
                }}
            }}"#
        );
        let result = parse_logs_notification(&frame, &mut dedup, &mut last_slot)
            .expect("parse should succeed");
        assert!(result.is_none(), "oversized sig should be skipped");
        assert_eq!(dedup.len(), 0);
    }

    #[test]
    fn test_parse_logs_notification_updates_slot_on_duplicate() {
        let mut dedup = DeduplicationSet::new(10);
        let mut last_slot = None;

        let frame1 = r#"{"jsonrpc":"2.0","method":"logsNotification","params":{"result":{"context":{"slot":100},"value":{"signature":"sigX","err":null,"logs":[]}},"subscription":1}}"#;
        parse_logs_notification(frame1, &mut dedup, &mut last_slot)
            .expect("first parse should succeed");
        assert_eq!(last_slot, Some(100));

        // Same sig, later slot — should still advance the slot cursor
        let frame2 = r#"{"jsonrpc":"2.0","method":"logsNotification","params":{"result":{"context":{"slot":200},"value":{"signature":"sigX","err":null,"logs":[]}},"subscription":1}}"#;
        let result = parse_logs_notification(frame2, &mut dedup, &mut last_slot)
            .expect("second parse should succeed");
        assert!(result.is_none(), "duplicate sig should be skipped");
        assert_eq!(
            last_slot,
            Some(200),
            "slot cursor must advance even on duplicate"
        );
    }

    // -----------------------------------------------------------------------
    // Send-safety compile-time checks (Story 6.4 AC9)
    //
    // See `src/idl/mod.rs` test module doc comment for rationale and
    // verification procedure. Short version: `fn _check` + `let _: fn = _check;`
    // forces monomorphization so the `T: Send` bound is actually checked.
    //
    // NOTE: `TransactionStream::{next, subscribe}` are `async_trait`-wrapped,
    // which emits `Pin<Box<dyn Future + Send + '_>>` internally — so these
    // tests mostly pin the macro contract in place. If someone switches the
    // trait to `#[async_trait(?Send)]`, the test will fail to compile, which
    // is exactly the regression we want to catch.
    // -----------------------------------------------------------------------

    #[test]
    fn test_ws_transaction_stream_is_send() {
        fn _assert_send<T: Send>() {}
        _assert_send::<WsTransactionStream>();
    }

    #[test]
    fn test_ws_next_future_is_send() {
        fn _check(s: &mut WsTransactionStream) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = s.next();
            _require_send(&fut);
        }
        let _: fn(&mut WsTransactionStream) = _check;
    }

    #[test]
    fn test_ws_subscribe_future_is_send() {
        fn _check(s: &mut WsTransactionStream) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = s.subscribe("prog");
            _require_send(&fut);
        }
        let _: fn(&mut WsTransactionStream) = _check;
    }
}
