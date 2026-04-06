# Story 4.1: WebSocket Transaction Stream

Status: ready-for-dev

## Story

As a system,
I want to subscribe to new transactions for a program via WebSocket and decode them in real time,
so that the index stays current with on-chain activity without manual re-triggering.

## Acceptance Criteria

1. **AC1: TransactionStream trait redesign**
   - **Given** the `TransactionStream` trait in `pipeline/ws.rs`
   - **When** I inspect it
   - **Then** it uses `#[async_trait]` for object safety (enabling `Box<dyn TransactionStream>`)
   - **And** it defines: `subscribe(program_id) -> Result<(), PipelineError>`, `next() -> Result<Option<StreamEvent>, PipelineError>`, `unsubscribe() -> Result<(), PipelineError>`, `last_seen_slot(&self) -> Option<u64>`
   - **And** `StreamEvent` is a struct containing `signature: String`, `slot: u64`, `error: Option<serde_json::Value>`

2. **AC2: WsTransactionStream implementation**
   - **Given** the `WsTransactionStream` struct
   - **When** it subscribes to a program
   - **Then** it opens a WebSocket connection to the configured `ws_url` (derived from `rpc_url` if not set)
   - **And** it sends `logsSubscribe` with `{"mentions": [program_id]}` and `{"commitment": "confirmed"}`
   - **And** for each `logsNotification` received, it yields a `StreamEvent` via `next()`
   - **And** `unsubscribe()` sends `logsUnsubscribe` and closes the connection

3. **AC3: Heartbeat monitoring**
   - **Given** the WebSocket connection is active
   - **When** 30 seconds pass without receiving a message
   - **Then** the client sends a WebSocket ping frame
   - **And** if no pong is received within 10 seconds, the connection is considered stale
   - **And** `next()` returns `Err(PipelineError::WebSocketDisconnect(...))` to signal reconnection is needed

4. **AC4: Signature deduplication**
   - **Given** the WebSocket receives a transaction signature
   - **When** the signature is checked against the in-memory dedup set
   - **Then** duplicates are discarded (next() skips them and continues to the next message)
   - **And** the dedup set is bounded at configurable size (default ~10,000 entries)
   - **And** uses `VecDeque` as eviction queue alongside a `HashSet`
   - **And** when the set exceeds the bound, the oldest entries are evicted first

5. **AC5: WS URL derivation**
   - **Given** `Config.ws_url` is `None`
   - **When** the WS client initializes
   - **Then** it derives the WS URL from `rpc_url` by replacing `https://` with `wss://` (or `http://` with `ws://`)
   - **And** if `Config.ws_url` is `Some(url)`, it uses that value directly

6. **AC6: Config additions**
   - **Given** `Config` in `config.rs`
   - **When** I inspect it
   - **Then** it has: `ws_ping_interval_secs: u64` (default 30), `ws_pong_timeout_secs: u64` (default 10), `dedup_cache_size: usize` (default 10_000)

7. **AC7: Unit tests**
   - **Given** the test module
   - **When** I run `cargo test`
   - **Then** tests verify: dedup set insert/eviction, WS URL derivation, StreamEvent creation, trait Send+Sync safety

## Tasks / Subtasks

- [ ] Task 1: Add `tokio-tungstenite` + `futures-util` dependencies (AC: #2)
  - [ ] Add `tokio-tungstenite = { version = "0.26", features = ["native-tls"] }` to `[dependencies]` in Cargo.toml
  - [ ] Add `futures-util = "0.3"` to `[dependencies]` (for `StreamExt`/`SinkExt` on WS stream)
  - [ ] Verify `cargo build` compiles with new deps

- [ ] Task 2: Add config fields (AC: #6)
  - [ ] Add `ws_ping_interval_secs: u64` (env `SOLARIX_WS_PING_INTERVAL_SECS`, default 30) to Config
  - [ ] Add `ws_pong_timeout_secs: u64` (env `SOLARIX_WS_PONG_TIMEOUT_SECS`, default 10) to Config
  - [ ] Add `dedup_cache_size: usize` (env `SOLARIX_DEDUP_CACHE_SIZE`, default 10_000) to Config

- [ ] Task 3: Implement `DeduplicationSet` (AC: #4)
  - [ ] Add `pub struct DeduplicationSet` with fields: `seen: HashSet<String>`, `order: VecDeque<String>`, `max_size: usize`
  - [ ] `pub fn new(max_size: usize) -> Self`
  - [ ] `pub fn insert(&mut self, sig: String) -> bool` — returns `true` if new, `false` if duplicate. Evicts oldest if full.
  - [ ] `pub fn contains(&self, sig: &str) -> bool`
  - [ ] `pub fn len(&self) -> usize`

- [ ] Task 4: Define `StreamEvent` and redesign `TransactionStream` trait (AC: #1)
  - [ ] Add `StreamEvent` struct: `signature: String`, `slot: u64`, `error: Option<serde_json::Value>`
  - [ ] Rewrite `TransactionStream` trait using `#[async_trait]`:
    ```rust
    #[async_trait]
    pub trait TransactionStream: Send + Sync {
        async fn subscribe(&mut self, program_id: &str) -> Result<(), PipelineError>;
        async fn next(&mut self) -> Result<Option<StreamEvent>, PipelineError>;
        async fn unsubscribe(&mut self) -> Result<(), PipelineError>;
        fn last_seen_slot(&self) -> Option<u64>;
    }
    ```

- [ ] Task 5: Implement `WsTransactionStream` core (AC: #2, #5)
  - [ ] Add `pub struct WsTransactionStream` with fields:
    - `ws_url: String` (resolved from config)
    - `ws_stream: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>` — active connection (types from `tokio_tungstenite`)
    - `subscription_id: Option<u64>` — from logsSubscribe response
    - `dedup: DeduplicationSet`
    - `last_seen_slot: Option<u64>`
    - `ping_interval: Duration`
    - `pong_timeout: Duration`
    - `last_message_time: Instant`
  - [ ] `pub fn new(config: &Config) -> Self` — resolve ws_url, init dedup set
  - [ ] Implement `fn derive_ws_url(rpc_url: &str) -> String`

- [ ] Task 6: Implement `subscribe()` (AC: #2)
  - [ ] Connect to `ws_url` via `tokio_tungstenite::connect_async()`
  - [ ] Send JSON-RPC 2.0 request: `logsSubscribe([{"mentions": [program_id]}, {"commitment": "confirmed"}])`
  - [ ] Parse response to extract `subscription_id`
  - [ ] Store connection and subscription_id

- [ ] Task 7: Implement `next()` with heartbeat (AC: #2, #3, #4)
  - [ ] Use `tokio::select!` to race:
    - WS message arrival
    - Ping timeout (time since last message > `ping_interval`)
  - [ ] On message: parse `logsNotification`, extract `signature`, `slot`, `err` from JSON
  - [ ] Check dedup set — skip if duplicate
  - [ ] Update `last_seen_slot` if new slot > current
  - [ ] Reset `last_message_time` on any received message
  - [ ] On ping timeout: send WebSocket Ping frame
  - [ ] If no Pong received within `pong_timeout`: return `Err(PipelineError::WebSocketDisconnect(...))`
  - [ ] Handle WS close/error frames: return `Err(PipelineError::WebSocketDisconnect(...))`

- [ ] Task 8: Implement `unsubscribe()` (AC: #2)
  - [ ] Send `logsUnsubscribe([subscription_id])` if subscription is active
  - [ ] Close WebSocket connection cleanly
  - [ ] Clear subscription_id

- [ ] Task 9: Unit tests (AC: #7)
  - [ ] `test_dedup_set_insert_and_contains` — insert returns true on first, false on duplicate
  - [ ] `test_dedup_set_eviction` — set at capacity evicts oldest entry when new one is inserted
  - [ ] `test_dedup_set_maintains_bounded_size` — insert 2x max_size entries, verify len == max_size
  - [ ] `test_derive_ws_url_https` — `https://api.mainnet-beta.solana.com` -> `wss://api.mainnet-beta.solana.com`
  - [ ] `test_derive_ws_url_http` — `http://localhost:8899` -> `ws://localhost:8899`
  - [ ] `test_derive_ws_url_already_wss` — `wss://already.good` -> `wss://already.good`
  - [ ] `test_stream_event_creation` — verify struct construction
  - [ ] `test_ws_transaction_stream_is_send` — compile-time check that `WsTransactionStream: Send`

- [ ] Task 10: Verify (AC: all)
  - [ ] `cargo build` compiles
  - [ ] `cargo clippy` passes
  - [ ] `cargo fmt -- --check` passes
  - [ ] `cargo test` — all tests pass (existing + new)

## Dev Notes

### Current Codebase State

`src/pipeline/ws.rs` currently contains a 8-line stub:

```rust
pub trait TransactionStream: Send + Sync {
    fn subscribe(
        &self,
        program_id: &str,
    ) -> impl std::future::Future<Output = Result<(), super::PipelineError>> + Send;
}
```

This trait has two problems (both from deferred-work.md):

1. Returns `()` with no message channel — redesign needed
2. Uses RPITIT (`-> impl Future`) which is NOT object-safe — cannot be used as `Box<dyn TransactionStream>`

This story replaces the entire stub with a working WebSocket implementation.

Note: `PipelineError::WebSocketDisconnect(String)` already exists in `pipeline/mod.rs:20`. No modification to `pipeline/mod.rs` is needed.

### Concrete WebSocket Type

`tokio-tungstenite` with `native-tls` feature produces:

```rust
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
```

`MaybeTlsStream<TcpStream>` is `Send` when using `native-tls` (not `rustls`). The `native-tls` feature is specified in Cargo.toml. This avoids the `!Send` issues seen in story 5-1.

### Dependency Decision: `tokio-tungstenite` vs `solana-pubsub-client`

Architecture doc specifies `solana-pubsub-client`, but it is currently commented out in Cargo.toml. Use `tokio-tungstenite` instead because:

1. **Thin deps** — `solana-pubsub-client` pulls in `solana-rpc-client-api`, `solana-sdk`, `crossbeam-channel`, and many transitive deps. `tokio-tungstenite` is minimal.
2. **`logsSubscribe` is simple** — Just JSON-RPC 2.0 over WebSocket. No special SDK types needed.
3. **Control** — Direct access to ping/pong frames, connection lifecycle, and raw message handling.
4. **Consistency** — Project already uses raw `reqwest` for HTTP RPC instead of `solana-rpc-client`. Same philosophy for WS.

If `tokio-tungstenite` causes any issues, `solana-pubsub-client` remains the fallback.

### WebSocket Message Format

`logsSubscribe` request:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "logsSubscribe",
  "params": [{ "mentions": ["<program_id>"] }, { "commitment": "confirmed" }]
}
```

Subscribe response:

```json
{ "jsonrpc": "2.0", "result": 42, "id": 1 }
```

Notification format:

```json
{
  "jsonrpc": "2.0",
  "method": "logsNotification",
  "params": {
    "result": {
      "value": {
        "signature": "5h6x...abc",
        "err": null,
        "logs": ["Program 11111... invoke [1]", "..."]
      },
      "context": { "slot": 290000000 }
    },
    "subscription": 42
  }
}
```

`logsUnsubscribe` request:

```json
{ "jsonrpc": "2.0", "id": 2, "method": "logsUnsubscribe", "params": [42] }
```

### JSON Deserialization Types (add to ws.rs)

```rust
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
    logs: Vec<String>,
}

#[derive(Deserialize)]
struct LogsContext {
    slot: u64,
}
```

### Ping/Pong Implementation Pattern

`tokio-tungstenite` uses `tungstenite::Message` enum:

- `Message::Ping(Vec<u8>)` — send to check connection health
- `Message::Pong(Vec<u8>)` — received in response to Ping
- `Message::Text(String)` — JSON-RPC messages
- `Message::Close(...)` — connection closing

The `next()` method should handle all message types in its main loop:

```rust
match msg {
    Message::Text(text) => { /* parse JSON notification */ }
    Message::Pong(_) => { /* reset pong_received flag */ }
    Message::Ping(data) => { /* auto-respond with Pong */ }
    Message::Close(_) => { /* return WebSocketDisconnect */ }
    _ => { /* ignore binary frames */ }
}
```

### DeduplicationSet Implementation

```rust
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
}
```

Memory footprint: ~10,000 signatures \* ~90 bytes each = ~900 KB. Negligible.

### WS URL Derivation

```rust
fn derive_ws_url(rpc_url: &str) -> String {
    if rpc_url.starts_with("wss://") || rpc_url.starts_with("ws://") {
        return rpc_url.to_string();
    }
    rpc_url
        .replace("https://", "wss://")
        .replace("http://", "ws://")
}
```

Config already has `ws_url: Option<String>`. Use it if present, otherwise derive from `rpc_url`.

### `next()` State Machine

The `next()` method manages heartbeat internally:

```
loop {
    select! {
        msg = ws.next() => {
            // reset last_message_time
            // parse message type
            // if Text -> parse notification -> check dedup -> yield StreamEvent
            // if Pong -> clear pending_ping flag
            // if Close -> return Err(WebSocketDisconnect)
        }
        _ = sleep_until(last_message_time + ping_interval) => {
            // send Ping
            // set pending_ping flag
            // if already pending -> pong_timeout elapsed -> return Err(WebSocketDisconnect)
        }
    }
}
```

### What This Story Does NOT Do

- Does NOT implement reconnection logic (story 4.2: `CatchingUp` state handles this)
- Does NOT implement gap detection (story 4.2: compare last streaming slot vs chain tip)
- Does NOT wire WsTransactionStream into PipelineOrchestrator (story 4.2: state machine integration)
- Does NOT call `get_transaction` or decode transactions (story 4.2: pipeline handles fetch+decode+store loop)
- Does NOT implement pipeline state transitions (story 4.2)
- Does NOT implement concurrent backfill+streaming (story 4.2: Option C dedup)
- Does NOT implement graceful shutdown sequence (story 4.3)
- Does NOT modify `main.rs` (story 4.3)
- Does NOT add `#[instrument]` tracing spans (story 6-1)

This story provides the **transport layer only**: subscribe, receive events, detect stale connections. Story 4.2 builds the processing pipeline on top.

### Dependencies Already Implemented

| Component            | Location            | Interface                                               |
| -------------------- | ------------------- | ------------------------------------------------------- |
| `RpcClient`          | `pipeline/rpc.rs`   | `get_transaction(sig) -> Option<RpcTransaction>`        |
| `ChainparserDecoder` | `decoder/mod.rs`    | `decode_instruction()`, `decode_account()`              |
| `StorageWriter`      | `storage/writer.rs` | `write_block(schema, stream, ixs, accs, slot, sig)`     |
| `Config`             | `config.rs`         | `ws_url: Option<String>`, `rpc_url`, `index_failed_txs` |

### Previous Story Learnings

**From story 3-5 (Pipeline Orchestrator spec):**

- Story 3-5 defined `PipelineOrchestrator` struct with `CancellationToken`, `Box<dyn SolarixDecoder>`, etc.
- Pipeline uses bounded `tokio::sync::mpsc` channel (capacity from `Config.channel_capacity`)
- `get_transaction` is already implemented on `RpcClient` (was added as part of story 3-5 prep)

**From story 3-3 (RPC):**

- All RPC calls pass through `governor` rate limiter + `backon` retry
- `get_transaction` uses `commitment: "finalized"` and `maxSupportedTransactionVersion: 0`
- Error classification is comprehensive (skipped slots, node behind, protocol errors)

**From story 5-1 (API - !Send blocker):**

- `async_trait` is already in Cargo.toml and used in `pipeline/rpc.rs` for `BlockSource`/`AccountSource`
- Pattern: use `#[async_trait]` on all traits that need object safety

### File Structure

| File                 | Action  | Purpose                                                                 |
| -------------------- | ------- | ----------------------------------------------------------------------- |
| `Cargo.toml`         | Modify  | Add `tokio-tungstenite`, `futures-util`                                 |
| `src/config.rs`      | Modify  | Add `ws_ping_interval_secs`, `ws_pong_timeout_secs`, `dedup_cache_size` |
| `src/pipeline/ws.rs` | Rewrite | Full WsTransactionStream implementation                                 |

**DO NOT modify:** `src/pipeline/mod.rs`, `src/pipeline/rpc.rs`, `src/storage/`, `src/decoder/`, `src/types.rs`, `src/api/`, `src/main.rs`, `src/registry.rs`

### Anti-Patterns to Avoid

- NO `unwrap()` or `expect()` outside tests -- use `?` with `map_err` to `PipelineError`
- NO `println!` -- use `tracing` macros (`info!`, `debug!`, `warn!`, `error!`)
- NO blocking calls on the Tokio runtime
- NO `solana-pubsub-client` dependency -- use `tokio-tungstenite` for thin deps
- NO processing/decoding in this story -- only transport + event delivery
- NO reconnection logic -- return `Err(WebSocketDisconnect)` and let caller handle
- DO use `#[async_trait]` for `TransactionStream` trait
- DO handle all `tungstenite::Message` variants (Text, Ping, Pong, Close, Binary)
- DO bound the dedup set to prevent unbounded memory growth
- DO make `WsTransactionStream` `Send + Sync`
- DO handle JSON parse errors gracefully (warn and skip malformed notifications)

### Testing Strategy

Unit tests in `#[cfg(test)] mod tests` at the bottom of `pipeline/ws.rs`:

1. **`test_dedup_set_insert_and_contains`** -- insert "sig1", verify contains, insert again returns false
2. **`test_dedup_set_eviction`** -- capacity 3, insert 4 entries, verify oldest is evicted
3. **`test_dedup_set_maintains_bounded_size`** -- capacity 100, insert 200, verify len == 100
4. **`test_derive_ws_url_https`** -- https -> wss
5. **`test_derive_ws_url_http`** -- http -> ws
6. **`test_derive_ws_url_already_ws`** -- wss:// and ws:// pass through unchanged
7. **`test_stream_event_creation`** -- verify struct fields
8. **`test_ws_transaction_stream_is_send`** -- compile-time `fn _assert_send<T: Send>() {}; _assert_send::<WsTransactionStream>();`

No integration tests (requiring actual WebSocket server) -- those go in Epic 6.

### JSON-RPC ID Management

Use a simple `AtomicU64` counter for JSON-RPC request IDs:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_rpc_id() -> u64 { NEXT_ID.fetch_add(1, Ordering::Relaxed) }
```

Or simpler: use `id: 1` for subscribe and `id: 2` for unsubscribe since only one outstanding request at a time.

### References

- [Source: _bmad-output/planning-artifacts/epics/epic-4-real-time-streaming-cold-start.md#Story 4.1]
- [Source: _bmad-output/planning-artifacts/architecture/core-architectural-decisions.md#Transport & Pipeline]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#6. Real-time Streaming Design]
- [Source: _bmad-output/planning-artifacts/research/agent-2c-backfill-pipeline-cold-start.md#6.3 Deduplication]
- [Source: _bmad-output/planning-artifacts/prd.md#Real-Time Indexing (FR18-FR21)]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md#subscribe() returns () with no message channel]
- [Source: _bmad-output/implementation-artifacts/deferred-work.md#RPITIT traits not object-safe]
- [Source: _bmad-output/implementation-artifacts/3-5-batch-indexing-pipeline-orchestrator.md]

## Dev Agent Record

### Agent Model Used

{{agent_model_name_version}}

### Debug Log References

### Completion Notes List

### File List
