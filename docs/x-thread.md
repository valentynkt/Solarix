# X Thread: Solarix

---

## 1/5

I built a universal Solana indexer in Rust that indexes ANY Anchor program — no codegen, no recompile, no redeploy.

Give it a program ID. It fetches the IDL, generates PostgreSQL tables, backfills history, streams live, and serves a query API.

All at runtime.

---

## 2/5

Every Solana indexer I've seen makes you write custom parsers per program, maintain schemas by hand, and redeploy for every new IDL.

Solarix: POST a program ID → IDL fetched on-chain → typed PG tables generated → indexing starts in seconds.

One binary. Zero config. Zero downtime.

---

## 3/5

4-layer pipeline: Read → Decode → Store → Serve

Concurrent backfill + live WebSocket streaming. Custom Borsh decoder for 18+ IDL types. Hybrid storage — native PG columns for fast queries, JSONB for complex types. 12-endpoint REST API with IDL-validated filters.

Crash-safe: both paths write with INSERT ON CONFLICT DO NOTHING. Restart from checkpoint, no data loss.

---

## 4/5

12,850 lines of Rust. 251 tests. Zero unsafe. Clippy denies unwrap/expect/panic.

Every error is a typed enum: retryable, skip-and-log, or fatal. Rate limiting, exponential backoff, graceful shutdown, WebSocket dedup — production stuff, not demo stuff.

docker compose up --build and you're running.

---

## 5/5

Built for the @SuperteamDAO Ukraine bounty. Fully open source.

github.com/valentynkit/solarix

Star it if useful. DMs open.
