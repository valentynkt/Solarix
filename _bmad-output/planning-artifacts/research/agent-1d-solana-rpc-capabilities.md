# Agent 1D: Solana RPC Indexing Capabilities & Provider Rate Limits

**Date:** 2026-04-05
**Research Type:** Technical — RPC API capabilities and provider constraints for Solarix universal indexer

---

## Core Methods for Indexing

### getBlock

**Parameters:**

- `slot` (u64, required): Slot number to retrieve
- `commitment` (string): `"confirmed"` or `"finalized"` (default). `"processed"` NOT accepted
- `encoding` (string): `"json"` (default), `"jsonParsed"`, `"base64"`, `"base58"`
- `transactionDetails` (string): `"full"` (default), `"accounts"`, `"signatures"`, `"none"`
- `maxSupportedTransactionVersion` (number): Set to `0` to get both legacy and v0 transactions. Omitting returns only legacy; blocks with v0 transactions will ERROR
- `rewards` (boolean): Include rewards array (default: true)

**Response schema (key fields):**

- `blockHeight` (u64|null), `blockTime` (i64|null), `blockhash` (string)
- `parentSlot` (u64), `previousBlockhash` (string)
- `transactions[]`: Each has `transaction` (encoded payload), `meta` (fee, err, pre/postBalances, pre/postTokenBalances, innerInstructions, logMessages, computeUnitsConsumed, loadedAddresses), `version` ("legacy" or 0)
- `rewards[]`: Validator rewards if requested

**Encoding comparison (single block example from Chainstack):**

| Encoding   | Uncompressed Size | Notes                                             |
| ---------- | ----------------- | ------------------------------------------------- |
| jsonParsed | ~16 MB            | Largest — auto-decodes known program instructions |
| json       | ~8 MB             | Default                                           |
| base58     | ~6.3 MB           | Slow to decode                                    |
| base64     | ~6.3 MB           | Best for raw binary parsing                       |

Gzip compression reduces payload by 70-90%, bringing most blocks down to a few hundred KB.

**Limitations:**

- `"processed"` commitment NOT supported
- Calling getBlock on a skipped slot returns error `-32009`: "Slot was skipped, or missing in long-term storage"
- Response payloads are large — serialization/deserialization is expensive, latency often hundreds of ms
- Must set `maxSupportedTransactionVersion: 0` or miss all v0 transactions

---

### getTransaction

**Parameters:**

- `signature` (string, required): Base-58 encoded transaction signature
- `encoding` (string): `"json"`, `"jsonParsed"`, `"base64"`, `"base58"`
- `commitment` (string): `"finalized"`, `"confirmed"`, or `"processed"`
- `maxSupportedTransactionVersion` (number): Set to `0`

**Response schema:**

- `slot` (u64): Slot containing this transaction
- `transaction`: Encoded payload (message with accountKeys, instructions, addressTableLookups)
- `meta`: Same as getBlock transaction meta (fee, err, balances, innerInstructions, logMessages, loadedAddresses)
- `blockTime` (i64|null), `version` ("legacy" or number)

**Notes:**

- Returns null if not found at requested commitment
- For indexing, prefer batch approaches over individual getTransaction calls

---

### getSignaturesForAddress

**Parameters:**

- `address` (string, required): Base-58 encoded account address
- `limit` (number): 1 to **1,000** (default: 1,000)
- `before` (string): Start searching backwards from this signature (cursor)
- `until` (string): Stop when this signature is reached
- `commitment` (string): `"confirmed"` or `"finalized"` (NOT `"processed"`)

**Pagination mechanism:**

- Results ordered **newest-first**
- To paginate backwards: take the last signature from current page, pass as `before` in next call
- `until` acts as a stop marker — if found before limit, stops there
- If `before` omitted, starts from highest confirmed block

**Response fields per entry:**

- `signature` (string), `slot` (u64), `blockTime` (i64|null)
- `err` (object|null), `memo` (string|null), `confirmationStatus` (string|null)

**Gotchas:**

- Max 1,000 results per call — for addresses with millions of transactions, requires many paginated calls
- Does NOT return full transaction data — need separate getTransaction call per signature
- Only returns transactions where address appears in `accountKeys` (top-level or loaded)
- `blockTime` and `confirmationStatus` may be null

---

### getBlocks (Gap Detection)

**Parameters:**

- `startSlot` (u64, required): Range start (inclusive)
- `endSlot` (u64, optional): Range end (inclusive)
- `commitment`: `"confirmed"` or `"finalized"` (NOT `"processed"`)

**Key constraint: max range of 500,000 slots**

**Response:** Array of u64 slot numbers where blocks were actually produced. Skipped/empty slots are ABSENT from the array.

**Usage for indexing:** Call getBlocks to discover which slots contain blocks, then only call getBlock on those slots. This avoids hitting `-32009` errors on skipped slots.

---

### getSlot / getBlockHeight

- `getSlot`: Returns the current slot height (~tip of chain)
- `getBlockHeight`: Returns the current block height (always less than slot height due to skipped slots)
- Difference between slot and block height = total historically skipped slots (~22M behind as of early 2025)
- Block time: ~400ms per slot, ~2.5 blocks/second, **~9,000 blocks/hour**

---

### getProgramAccounts

**Parameters:**

- `programId` (string, required): Program public key
- `encoding`: `"json"`, `"jsonParsed"`, `"base64"`, `"base64+zstd"`
- `filters` (array, max 4): `memcmp` (offset + bytes) and/or `dataSize`
- `dataSlice`: `{offset, length}` to limit returned data per account
- `withContext` (boolean): Include slot context

**Filters:**

- `memcmp`: Compare bytes at offset. Data limited to 128 decoded bytes. Encoding: base58 or base64
- `dataSize`: Match accounts with exact data length in bytes

**Limitations:**

- **No native pagination** — returns ALL matching accounts or times out
- Resource-intensive scan on RPC nodes — connection timeouts common for large programs
- Some providers impose additional rate limiting on this method
- Workaround: Use `dataSlice: {offset: 0, length: 0}` to get only pubkeys, then fetch data separately

**Helius getProgramAccountsV2 (extension):**

- Cursor-based pagination with `limit` (1-10,000) and `paginationKey`
- `changedSinceSlot`: Incremental updates — only accounts modified since a slot
- End of pagination indicated when no accounts returned (not when fewer than limit)
- Helius auto-indexes programs after first query
- Recommended batch size: 1,000-5,000 for best balance

---

### getMultipleAccounts

- **Max 100 accounts per call**
- Returns same data as getAccountInfo but batched
- Chunk pubkey arrays and parallelize with Promise.all/tokio::join
- For fewer than 12 accounts, parallel getAccountInfo may be faster

### getAccountInfo

- Single account read
- Supports `encoding` and `dataSlice` parameters
- Cheapest possible account query

---

## WebSocket Subscriptions

### Method Comparison

| Method               | Filter by Program?                             | Message Content                | Stability                              | Provider Support                              |
| -------------------- | ---------------------------------------------- | ------------------------------ | -------------------------------------- | --------------------------------------------- |
| `blockSubscribe`     | YES (`mentionsAccountOrProgram`)               | Full block data (configurable) | **UNSTABLE** — requires validator flag | Limited (NOT supported on Helius standard WS) |
| `logsSubscribe`      | YES (`mentions: [pubkey]`) — exactly 1 address | signature + logs + err         | Stable but unreliable under load       | Widely supported                              |
| `accountSubscribe`   | NO (single account only)                       | Account data on change         | Stable                                 | Widely supported                              |
| `programSubscribe`   | YES (all accounts owned by program)            | Account data on change         | Stable                                 | Widely supported                              |
| `signatureSubscribe` | NO (single signature)                          | Confirmation status            | Stable                                 | Widely supported                              |
| `slotSubscribe`      | NO                                             | Slot number updates            | Stable                                 | Widely supported                              |
| `rootSubscribe`      | NO                                             | New root slot                  | Stable                                 | Widely supported                              |

### blockSubscribe Details

- Filter: `"all"` or `{"mentionsAccountOrProgram": "<pubkey>"}`
- Supports encoding, transactionDetails, maxSupportedTransactionVersion, showRewards
- Marked **unstable** — validator must start with `--rpc-pubsub-enable-block-subscription`
- Known issues: connection errors (1006, 1009), skipped data on busy programs
- Official recommendation: Use Geyser plugins instead for production

### logsSubscribe Details

- Filter: `"all"`, `"allWithVotes"`, or `{"mentions": ["<pubkey>"]}`
- `mentions` currently supports **exactly 1 address** — multiple returns error
- Returns: signature, err, logs[] (program log messages)
- Supports `"processed"` commitment (unlike most HTTP methods)

### WebSocket Reliability (Critical)

- **No delivery guarantees**: Messages can be lost on disconnect
- **No ordering guarantees**: Must handle at application level
- **No exactly-once semantics**: Must deduplicate
- **Reconnection**: Must implement exponential backoff + automatic resubscription
- **Known issue**: 15+ second delays observed on finalized commitment
- **Stale connections**: Must implement heartbeat/ping-pong detection
- **Production verdict**: Standard WebSockets are insufficient for mission-critical indexing

### Enhanced Alternatives

- **Helius Enhanced WebSockets**: Powered by LaserStream infra, 1.5-2x faster, multi-node aggregation with automatic failover. Available on Business plan ($499/mo) and up. Starting April 7, 2026: `transactionSubscribe` on Developer plan ($49/mo) with up to 100 subscriptions/connection
- **Helius LaserStream (gRPC)**: Yellowstone-compatible, 24-hour historical replay on disconnect, automatic reconnects, up to 1.3 GB/s throughput. Professional plan ($999/mo) for mainnet
- **Triton Yellowstone gRPC (Dragon's Mouth)**: Direct Geyser plugin streaming via gRPC/Protobuf, bypasses JSON-RPC entirely, up to 400ms latency advantage over RPC. Dedicated nodes from $2,900/mo

---

## Rate Limits by Provider

### Summary Table

| Provider       | Free Tier        | Free RPS               | Paid Entry                 | Paid RPS    | Top RPS       | Batch JSON-RPC? | WebSocket?     | getBlock?   | Special Features                                                                        |
| -------------- | ---------------- | ---------------------- | -------------------------- | ----------- | ------------- | --------------- | -------------- | ----------- | --------------------------------------------------------------------------------------- |
| **Public RPC** | 100 req/10s      | ~10 overall, ~4/method | N/A                        | N/A         | N/A           | Yes (native)    | Yes            | Yes         | None — not for production                                                               |
| **Helius**     | 1M credits/mo    | 10                     | $49/mo (10M cr)            | 50          | 500 ($999/mo) | Yes             | Yes + Enhanced | Yes (10 cr) | gTFA, DAS API, gPAv2, LaserStream, auto-indexing                                        |
| **QuickNode**  | 10M credits/mo   | 15                     | $10/mo (25M cr)            | 40          | 500 ($999/mo) | Yes             | Yes            | Yes (30 cr) | Flat-rate RPS option, method-level limiting, marketplace add-ons                        |
| **Alchemy**    | 30M CU/mo        | ~300 (CU-based)        | $0.40/1M CU                | CU-based    | CU-based      | Yes             | Yes            | Yes         | 2x throughput claim, archival 20x faster, multi-chain                                   |
| **Triton One** | N/A (enterprise) | N/A                    | ~$2,900/mo dedicated       | Custom      | Custom        | Yes             | Yes            | Yes         | Yellowstone gRPC, Old Faithful (full history), Steamboat indexing, Vixen parsed streams |
| **Chainstack** | 3M req/mo        | Varies                 | $149/mo (25 RPS unlimited) | 25+         | Custom        | Yes             | Yes            | Yes         | Yellowstone gRPC, ShredStreams, dedicated nodes ~$3,577/mo                              |
| **Ankr**       | 200M credits/mo  | Varies                 | Credit-based               | Up to 1,500 | 1,500         | Yes             | Paid only      | Yes         | 80+ chains, per-method billing                                                          |
| **Syndica**    | Contact sales    | N/A                    | Custom                     | Custom      | Custom        | Yes             | Yes            | Yes         | Custom account indexing (10x gPA speed), ChainStream API                                |

### Credit/Cost Per Method (Helius)

| Method                                                               | Credits                                   |
| -------------------------------------------------------------------- | ----------------------------------------- |
| Standard RPC calls (getSlot, getBlockHeight, etc.)                   | 1                                         |
| Archival methods (getBlock, getTransaction, getSignaturesForAddress) | 10                                        |
| getTransactionsForAddress (Helius-exclusive)                         | 100                                       |
| DAS API methods                                                      | Separate rate limit (2-100 req/s by tier) |
| sendTransaction (Sender)                                             | 0 (tip in SOL)                            |

### Credit/Cost Per Method (QuickNode)

| Method Type                                 | Credits             |
| ------------------------------------------- | ------------------- |
| Standard methods                            | 30                  |
| Advanced APIs (getAsset, etc.)              | 60 (2x multiplier)  |
| Large calls (getLargestAccounts, getSupply) | 120 (4x multiplier) |

### Public RPC Details

- Endpoint: `https://api.mainnet-beta.solana.com`
- 100 requests per 10 seconds per IP (total)
- 40 requests per 10 seconds per IP (per single method)
- Returns HTTP 429 (rate limit) or 403 (blocked)
- **NOT suitable for indexing** — too slow, no SLA, will ban abusers

---

## Batch JSON-RPC Support

Solana **natively supports** batch JSON-RPC: send an array of request objects in a single HTTP POST, receive an array of responses.

**Caveats:**

- Provider splits batch into individual requests internally — adds processing overhead
- Error handling is complex: one request can fail while others succeed, but HTTP status is still 200
- Not always faster than parallel individual requests due to routing overhead
- Most providers support it, but may count each sub-request against rate limits individually

**Recommendation:** Use batch JSON-RPC for reducing TCP connection overhead, but don't expect it to bypass rate limits. Parallel individual requests with connection pooling may perform equally or better.

---

## Backfill Feasibility Analysis

### Block Production Rate

- ~2.5 blocks/second = ~9,000 blocks/hour = ~216,000 blocks/day
- Not every slot has a block — block height lags slot height by ~22M+

### Strategy 1: getBlock Scanning (Full Block Approach)

**When to use:** Target program appears in a high % of blocks (e.g., Token program, popular DeFi)

**Math for 100K slots:**

- Use getBlocks to find actual blocks in range (maybe ~90K blocks in 100K slots)
- At 50 RPS (Helius Developer): 90,000 / 50 = 1,800 seconds = **30 minutes**
- At 200 RPS (Helius Business): 90,000 / 200 = 450 seconds = **7.5 minutes**
- At 500 RPS (Helius Professional): 90,000 / 500 = 180 seconds = **3 minutes**

**Math for 1M slots:**

- ~900K actual blocks in 1M slots
- At 50 RPS: 900,000 / 50 = 18,000 seconds = **5 hours**
- At 200 RPS: 900,000 / 200 = 4,500 seconds = **75 minutes**
- At 500 RPS: 900,000 / 500 = 1,800 seconds = **30 minutes**

**Bandwidth (uncompressed):**

- 900K blocks x ~8 MB (json encoding) = ~7.2 TB uncompressed
- With gzip (~80% reduction): ~1.4 TB compressed
- With base64 encoding + gzip: ~900 GB compressed

**Credit cost (Helius):**

- getBlocks: 2 calls (500K range each) = 2 credits
- getBlock: 900K calls x 10 credits = 9M credits
- Helius Business (100M credits/mo) covers ~11 full-chain million-slot backfills/month
- Helius Developer (10M credits/mo) covers ~1.1 million-slot backfills/month

### Strategy 2: getSignaturesForAddress + getTransaction (Targeted Approach)

**When to use:** Target program has sparse activity across blocks

**Math for an address with 100K transactions:**

- getSignaturesForAddress: 100,000 / 1,000 per page = 100 calls
- getTransaction per signature: 100,000 calls
- Total: 100,100 calls
- At 50 RPS: ~33 minutes
- At 200 RPS: ~8 minutes

**Credit cost (Helius):**

- getSignaturesForAddress: 100 x 10 = 1,000 credits
- getTransaction: 100,000 x 10 = 1,000,000 credits
- Total: ~1M credits

### Strategy 3: getTransactionsForAddress (Helius-Exclusive, Recommended)

**When to use:** Best for targeted address backfill on Helius

**Math for 100K transactions:**

- 100K / 100 per page = 1,000 calls
- At 50 RPS: 20 seconds
- Credit cost: 1,000 x 100 = 100,000 credits

**Dramatic improvement:** 100,100 calls reduced to 1,000 calls. 33 minutes reduced to 20 seconds.

### Strategy 4: gRPC Streaming (Triton Old Faithful / Helius LaserStream)

**When to use:** Full historical replay or very large backfills

- Triton's Old Faithful provides complete verified Solana history via gRPC
- Faithful Streams replays the ledger sequentially at high throughput
- LaserStream offers 24-hour historical replay on reconnect
- Bypasses JSON-RPC entirely — Protobuf serialization is 5-10x more efficient
- Requires $999+/mo (Helius Professional) or $2,900+/mo (Triton dedicated)

### Backfill Cost Estimates

| Scenario              | Provider/Tier        | Time       | Monthly Cost | Credits Used      |
| --------------------- | -------------------- | ---------- | ------------ | ----------------- |
| 100K slots (getBlock) | Helius Dev ($49/mo)  | ~30 min    | $49          | ~900K             |
| 100K slots (getBlock) | Helius Biz ($499/mo) | ~7.5 min   | $499         | ~900K             |
| 1M slots (getBlock)   | Helius Dev ($49/mo)  | ~5 hours   | $49          | ~9M (over budget) |
| 1M slots (getBlock)   | Helius Biz ($499/mo) | ~75 min    | $499         | ~9M               |
| 100K txns (gTFA)      | Helius Dev ($49/mo)  | ~20 sec    | $49          | ~100K             |
| 1M txns (gTFA)        | Helius Dev ($49/mo)  | ~3.3 min   | $49          | ~1M               |
| Full replay           | Triton dedicated     | Hours-days | $2,900+      | N/A               |

---

## Pagination and Gap Handling

### getSignaturesForAddress Pagination

1. First call: omit `before` — starts from tip (newest)
2. Take last signature from results
3. Next call: pass that signature as `before`
4. Repeat until results are empty or `until` signature is reached
5. Results come **newest-first** — reverse if chronological order needed

### Empty/Skipped Slots

- Solana regularly has empty slots (no block produced)
- `getBlock` on a skipped slot returns error `-32009`
- Use `getBlocks(startSlot, endSlot)` to get only slots with actual blocks
- getBlocks max range: 500,000 slots
- For ranges > 500K slots: chunk into multiple getBlocks calls

### Gap Detection on Cold Start

1. Load `lastProcessedSlot` from database
2. Call `getSlot()` to get current tip
3. Gap = `currentSlot - lastProcessedSlot`
4. Use `getBlocks(lastProcessedSlot + 1, min(lastProcessedSlot + 500_000, currentSlot))` to find blocks in gap
5. Fetch each block with getBlock, filter for target program
6. Repeat in 500K-slot chunks until caught up
7. Switch to real-time streaming (WebSocket or gRPC)

---

## Data Format Considerations

### JSON vs Base64 Encoding

- **json**: Human-readable, ~8 MB per block, easy to filter/parse in application
- **jsonParsed**: Auto-decodes known programs (SPL Token, System, etc.), ~16 MB per block (largest), best for analysis
- **base64**: Compact (~6.3 MB), requires client-side deserialization, best for raw binary processing
- **Recommendation**: Use `base64` for bandwidth-sensitive backfill, `jsonParsed` for real-time processing where you need decoded instruction data

### Transaction Versions

- **Legacy**: Original format, all accounts inline in message
- **v0**: Supports Address Lookup Tables (ALTs) — accounts referenced by 1-byte index instead of 32-byte pubkey
- Must set `maxSupportedTransactionVersion: 0` everywhere or miss v0 transactions
- `loadedAddresses` in metadata shows accounts resolved from ALTs, split into `writable` and `readonly`

### Inner Instructions (CPI)

- `innerInstructions[].index`: Which top-level instruction triggered the CPI
- `innerInstructions[].instructions[]`: Array of CPI calls, each with `programIdIndex`, `accounts`, `data`
- `stackHeight`: CPI depth (1 = direct call, 2+ = nested)
- With `jsonParsed`: CPI instructions get `parsed` field with `program`, `type`, `info` when parser available

### Identifying Target Program Instructions

In a complex transaction with multiple CPI calls:

1. Check `transaction.message.instructions[]` — each has `programIdIndex` pointing to program's pubkey in `accountKeys`
2. Check `meta.innerInstructions[]` for CPI calls — match `programIdIndex` against your target program's pubkey
3. With `jsonParsed`, look for `program` field matching your program name
4. `logMessages` contain "Program <pubkey> invoke" lines that trace execution flow

---

## Recommended Read Layer Design

### 1. Primary Provider: Helius (Business tier at $499/mo for production)

**Why:**

- Best credit-per-method model for indexing (1 credit = 1 call, 10 for archival)
- `getTransactionsForAddress` reduces backfill calls by 100x
- `getProgramAccountsV2` with pagination + `changedSinceSlot` for account state
- Auto-indexing of programs improves gPA performance
- Enhanced WebSockets with LaserStream-backed reliability
- 200 RPS at Business tier covers most indexing workloads
- Clear upgrade path to Professional ($999/mo) for LaserStream gRPC mainnet

### 2. Backfill Strategy

**For targeted program backfill (preferred):**

- Use `getTransactionsForAddress` (Helius-exclusive) for the program address
- Paginate with `paginationToken`, use slot-based filters for ranges
- 100 full transactions per call, ~20 seconds for 100K transactions at 50 RPS
- Falls back gracefully to getSignaturesForAddress + getTransaction on non-Helius

**For full-block-scan backfill:**

1. `getBlocks(startSlot, endSlot)` — find actual blocks in range (max 500K slots per call)
2. Parallel `getBlock` with `encoding: "base64"`, `transactionDetails: "full"`, `maxSupportedTransactionVersion: 0`
3. Concurrency: 5-10 parallel requests with exponential backoff
4. Client-side filtering for target program transactions
5. Use gzip compression on HTTP to reduce bandwidth 70-90%

**For massive historical replay:**

- Consider Triton Old Faithful or Helius LaserStream historical replay
- gRPC streaming bypasses JSON-RPC overhead entirely

### 3. Real-Time Strategy

**Primary: Helius Enhanced WebSockets with `transactionSubscribe`**

- Available on Developer plan ($49/mo) starting April 7, 2026
- Up to 100 subscriptions per connection
- LaserStream-backed reliability with multi-node aggregation
- Filter by program ID

**Fallback: `logsSubscribe` with program filter**

- Filter: `{"mentions": ["<program_pubkey>"]}`
- Returns signature + logs — need follow-up getTransaction for full data
- Implement reconnection with exponential backoff
- Maintain last-seen slot for gap detection on reconnect

**Production upgrade: LaserStream gRPC (Professional plan, $999/mo)**

- 24-hour historical replay on disconnect — no gaps
- Automatic reconnection built into SDK
- Filter by program, account, or transaction attributes
- Yellowstone-compatible — can swap to Triton if needed

**Gap handling on reconnect:**

1. On disconnect, record `lastProcessedSlot`
2. On reconnect, LaserStream auto-replays from disconnect point (if within 24h)
3. For longer gaps, fall back to backfill strategy (getBlocks + getBlock)
4. Deduplicate by transaction signature

### 4. Account State Strategy

**Initial load:** `getProgramAccountsV2` (Helius) with pagination

- Limit: 1,000-5,000 per page
- Filters: `memcmp` + `dataSize` for specific account types
- Store all accounts with their slot number

**Incremental updates:** `getProgramAccountsV2` with `changedSinceSlot`

- Pass last-synced slot to get only modified accounts
- Vastly more efficient than full rescans

**Real-time:** `programSubscribe` WebSocket

- Notifies on any account change for the program
- Returns full account data on change

**Individual reads:** `getMultipleAccounts` (batch of up to 100) or `getAccountInfo` (single)

---

## Critical Constraints

### 1. getBlock Response Size Dominates Backfill Cost

A single block can be 8-16 MB uncompressed in JSON. At 1M slots, that's potentially 7+ TB of data transfer. Compression and base64 encoding are mandatory. This single factor determines whether backfill takes hours or days.

### 2. Rate Limits Define Architecture — Not RPC Capabilities

The difference between 10 RPS (public) and 500 RPS (Helius Pro) is the difference between 25 hours and 30 minutes for 1M slots. The backfill layer MUST be parameterized by rate limit and implement adaptive throttling.

### 3. WebSocket Subscriptions Are Unreliable Without Enhancement

Standard Solana WebSockets have no delivery, ordering, or exactly-once guarantees. 15+ second delays are documented. Any real-time indexer MUST implement: reconnection logic, subscription state management, gap detection on reconnect, and slot-based deduplication.

### 4. No Native Pagination for getProgramAccounts

The standard RPC has no pagination for getProgramAccounts — it either returns everything or times out. For programs with millions of accounts, Helius's `getProgramAccountsV2` is the only viable paginated alternative via standard RPC. This creates a provider dependency.

### 5. Archival Data Availability Is Limited

Public nodes keep only 2-3 epochs of history. For historical backfill, you need an archival RPC provider. Helius, Triton (Old Faithful), and dedicated nodes provide full history, but at significant cost. The indexer's "how far back" capability is directly limited by the RPC provider's archival depth.

---

## Sources

- [Solana Official RPC Docs — getBlock](https://solana.com/docs/rpc/http/getblock)
- [Solana Official RPC Docs — getSignaturesForAddress](https://solana.com/docs/rpc/http/getsignaturesforaddress)
- [Solana Official RPC Docs — getTransaction](https://solana.com/docs/rpc/http/gettransaction)
- [Solana Official RPC Docs — getBlocks](https://solana.com/docs/rpc/http/getblocks)
- [Solana Official RPC Docs — getProgramAccounts](https://solana.com/docs/rpc/http/getprogramaccounts)
- [Solana Official RPC Docs — getMultipleAccounts](https://solana.com/docs/rpc/http/getmultipleaccounts)
- [Solana Official RPC Docs — WebSocket Methods](https://solana.com/docs/rpc/websocket)
- [Solana Official RPC Docs — blockSubscribe](https://solana.com/docs/rpc/websocket/blocksubscribe)
- [Solana Official RPC Docs — logsSubscribe](https://solana.com/docs/rpc/websocket/logssubscribe)
- [Solana Official RPC Docs — JSON Structures](https://solana.com/docs/rpc/json-structures)
- [Solana Official RPC Docs — Clusters and Public Endpoints](https://solana.com/docs/references/clusters)
- [Solana Official — Versioned Transactions](https://solana.com/developers/guides/advanced/versions)
- [Solana Official — Understanding Slots, Blocks, Epochs (via Helius)](https://www.helius.dev/blog/solana-slots-blocks-and-epochs)
- [Chainstack — Optimize getBlock Performance](https://docs.chainstack.com/docs/solana-optimize-your-getblock-performance)
- [Chainstack — Understanding Slots vs Blocks](https://docs.chainstack.com/docs/understanding-the-difference-between-blocks-and-slots-on-solana)
- [Helius — Plans and Pricing](https://www.helius.dev/docs/billing/plans)
- [Helius — How to Index Solana Data](https://www.helius.dev/docs/rpc/how-to-index-solana-data)
- [Helius — getProgramAccountsV2](https://www.helius.dev/docs/api-reference/rpc/http/getprogramaccountsv2)
- [Helius — getTransactionsForAddress](https://www.helius.dev/blog/introducing-gettransactionsforaddress)
- [Helius — RPC Optimization Techniques](https://www.helius.dev/docs/rpc/optimization-techniques)
- [Helius — Enhanced WebSockets](https://www.helius.dev/docs/enhanced-websockets)
- [Helius — LaserStream Introduction](https://www.helius.dev/blog/introducing-laserstream)
- [Helius — LaserStream Powers All WebSockets](https://www.helius.dev/blog/laserstream-websockets)
- [Helius — How to Use getProgramAccounts](https://www.helius.dev/docs/rpc/guides/getprogramaccounts)
- [Helius — How to Use getBlock](https://www.helius.dev/docs/rpc/guides/getblock)
- [Helius — Historical Data for Indexing](https://www.helius.dev/historical-data)
- [QuickNode — Solana RPC Overview](https://www.quicknode.com/docs/solana)
- [QuickNode — Solana API Credits](https://www.quicknode.com/api-credits/sol)
- [QuickNode — Flat Rate RPS](https://www.quicknode.com/docs/platform/billing/flat-rate-rps)
- [QuickNode — Pricing](https://www.quicknode.com/pricing)
- [QuickNode — WebSocket Subscriptions Guide](https://www.quicknode.com/guides/solana-development/getting-started/how-to-create-websocket-subscriptions-to-solana-blockchain-using-typescript)
- [QuickNode — Subscription Strategies](https://www.quicknode.com/docs/solana/subscriptions)
- [Triton One — Yellowstone gRPC Guide 2026](https://blog.triton.one/complete-guide-to-solana-streaming-and-yellowstone-grpc/)
- [Triton One — gRPC Optimization 2026](https://blog.triton.one/solana-grpc-streaming-optimisation-and-troubleshooting-2026-guide/)
- [Triton One — Dragon's Mouth Docs](https://docs.triton.one/project-yellowstone/dragons-mouth-grpc-subscriptions)
- [Triton One — Enterprise RPC Infrastructure](https://blog.triton.one/practical-guide-to-enterprise-solana-rpc-infrastructure/)
- [Alchemy — Pricing](https://www.alchemy.com/pricing)
- [Alchemy — Solana RPC Overview](https://www.alchemy.com/overviews/solana-rpc)
- [Chainstack — Pricing](https://chainstack.com/pricing/)
- [Chainstack — Throughput Guidelines](https://docs.chainstack.com/docs/limits)
- [Ankr — Solana RPC Docs](https://www.ankr.com/docs/rpc-service/chains/chains-api/solana/)
- [Sanctum — Complete Guide to Solana RPC Providers 2026](https://sanctum.so/blog/complete-guide-solana-rpc-providers-2026)
- [Solana Cookbook — Get Program Accounts](https://solanacookbook.com/guides/get-program-accounts.html)
- [GitHub — Solana WebSocket Issues (#35489)](https://github.com/solana-labs/solana/issues/35489)
- [Chainary — Solana WebSocket Architecture](https://www.chainary.net/articles/solana-websocket-subscriptions-real-time-data-streaming-architecture)
- [Yellowstone gRPC GitHub](https://github.com/rpcpool/yellowstone-grpc)
