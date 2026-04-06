// std library
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

// external crates
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info};

// internal crate
use super::PipelineError;
use crate::config::Config;

// ---------------------------------------------------------------------------
// StreamEvent
// ---------------------------------------------------------------------------

/// Event received from WebSocket transaction stream.
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
        Self {
            seen: HashSet::with_capacity(max_size),
            order: VecDeque::with_capacity(max_size),
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
}

// ---------------------------------------------------------------------------
// JSON-RPC deserialization types (private)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WsJsonRpcResponse {
    result: Option<serde_json::Value>,
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
        }
    }
}

#[async_trait]
impl TransactionStream for WsTransactionStream {
    async fn subscribe(&mut self, program_id: &str) -> Result<(), PipelineError> {
        info!(ws_url = %self.ws_url, program_id, "connecting to WebSocket");

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

        // Read subscription response
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    let response: WsJsonRpcResponse = serde_json::from_str(&text).map_err(|e| {
                        PipelineError::WebSocketDisconnect(format!(
                            "failed to parse subscribe response: {e}"
                        ))
                    })?;

                    if let Some(error) = response.error {
                        return Err(PipelineError::WebSocketDisconnect(format!(
                            "logsSubscribe failed: {} (code {})",
                            error.message, error.code
                        )));
                    }

                    if let Some(result) = response.result {
                        self.subscription_id = result.as_u64();
                        break;
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    ws.send(Message::Pong(data)).await.map_err(|e| {
                        PipelineError::WebSocketDisconnect(format!("pong send failed: {e}"))
                    })?;
                }
                Some(Ok(_)) => continue,
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

        self.last_message_time = Instant::now();

        info!(
            subscription_id = ?self.subscription_id,
            "subscribed to logsNotification"
        );

        Ok(())
    }

    async fn next(&mut self) -> Result<Option<StreamEvent>, PipelineError> {
        loop {
            let deadline = if self.pending_ping {
                self.last_ping_time + self.pong_timeout
            } else {
                self.last_message_time + self.ping_interval
            };

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
                Received::Message(Ok(msg)) => {
                    self.last_message_time = Instant::now();
                    self.pending_ping = false;

                    match msg {
                        Message::Text(text) => {
                            match parse_logs_notification(
                                &text,
                                &mut self.dedup,
                                &mut self.last_seen_slot,
                            ) {
                                Some(event) => return Ok(Some(event)),
                                None => continue,
                            }
                        }
                        Message::Pong(_) => continue,
                        Message::Ping(data) => {
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
                        _ => continue,
                    }
                }
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
        }

        self.ws_stream = None;
        self.subscription_id = None;

        info!("unsubscribed from logsNotification");

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

/// Derive WebSocket URL from an HTTP RPC URL.
fn derive_ws_url(rpc_url: &str) -> String {
    if rpc_url.starts_with("wss://") || rpc_url.starts_with("ws://") {
        return rpc_url.to_string();
    }
    rpc_url
        .replace("https://", "wss://")
        .replace("http://", "ws://")
}

/// Parse a JSON-RPC logsNotification into a StreamEvent, applying deduplication.
fn parse_logs_notification(
    text: &str,
    dedup: &mut DeduplicationSet,
    last_seen_slot: &mut Option<u64>,
) -> Option<StreamEvent> {
    let notification: LogsNotification = match serde_json::from_str(text) {
        Ok(n) => n,
        Err(_) => {
            debug!(
                text_len = text.len(),
                "non-notification WS message, skipping"
            );
            return None;
        }
    };

    let sig = notification.params.result.value.signature;
    let slot = notification.params.result.context.slot;
    let error = notification.params.result.value.err;

    if !dedup.insert(sig.clone()) {
        debug!(signature = %sig, "duplicate signature, skipping");
        return None;
    }

    if last_seen_slot.map_or(true, |s| slot > s) {
        *last_seen_slot = Some(slot);
    }

    Some(StreamEvent {
        signature: sig,
        slot,
        error,
    })
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

    // -- Send safety --

    #[test]
    fn test_ws_transaction_stream_is_send() {
        fn _assert_send<T: Send>() {}
        _assert_send::<WsTransactionStream>();
    }
}
