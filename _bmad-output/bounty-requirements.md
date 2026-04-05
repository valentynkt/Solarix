# Solarix — Bounty Requirements (Source of Truth)

**Source:** Superteam Ukraine Bounty
**Level:** Middle
**Tech Stack:** Rust

---

## Mission

Build a production-ready, universal Solana indexer that automatically adapts to any Anchor IDL.

---

## Core Requirements

### 1. Dynamic Schema and Decoding

- Automatically generate a database schema based on the provided IDL, without manual table descriptions.
- Decode not only instructions but also the program's account states.

### 2. Indexing Modes

- **Batch Mode:** Process data within a specified slot range or from a list of signatures.
- **Real-time Mode:** Subscribe to new transactions with a "cold start" capability. When launched, the indexer should first catch up on missed transactions (backfill from the last processed point) and then transition to real-time mode.

### 3. Reliability

- **Exponential backoff:** Implement increasing delays between RPC retries to avoid overwhelming the node.
- **Retry mechanism:** For failed requests.
- **Graceful shutdown:** Ensure correct termination without losing state or incomplete database writes.

### 4. Advanced API

- Filter by multiple parameters simultaneously.
- Aggregation, e.g., the number of calls for a specific instruction over a period.
- Basic program statistics.

### 5. Infrastructure

- Docker Compose with all dependencies (start with a single command).
- Configuration via environment variables.
- Structured logging.

---

## Submission Requirements

- A public GitHub repository with a comprehensive README, including:
  - Architectural overview.
  - Setup and running instructions.
  - Examples of API queries.
  - Explanation of key architectural decisions and trade-offs.
- A Twitter thread detailing your experience: what you built, how you tackled technical challenges, and the trade-offs you made and why.
- Must be in English.

---

## Judging Criteria (in listed order)

1. Dynamic schema generation and account decoding.
2. Real-time mode with cold start functionality.
3. Reliability features (exponential backoff, retry mechanisms, graceful shutdown).
4. Advanced API capabilities, including aggregation and statistics.
5. Code quality, architecture, and presence of tests.
6. Clarity and completeness of the README, including explanations of architectural decisions and trade-offs.

---

## Reward Structure

- 1st Place: 500 USDG
- 2nd Place: 450 USDG
- 3rd Place: 250 USDG

---

## Implicit Requirements (read between the lines)

### From "production-ready" in the mission statement

- Error handling must be robust — no panics, no unwraps in production paths. Proper error types with context.
- The indexer must not corrupt data on crash, network failure, or RPC timeout mid-batch.
- State must be persisted — restarting the indexer should resume from where it left off, not from scratch.
- Must handle edge cases: empty blocks, transactions with no matching IDL instructions, malformed data, RPC node returning errors.

### From "universal" and "any Anchor IDL"

- Must work with IDLs the judges have never seen before — not hardcoded to specific programs.
- Should handle both simple programs (1-2 instructions) and complex ones (many accounts, nested types, enums).
- The IDL is the ONLY input needed — judges will test by pointing the indexer at arbitrary Anchor programs.
- Implies support for Anchor IDL v0.30+ format (current standard). Supporting legacy v0.29 is a bonus.

### From "automatically generate a database schema"

- No migration files checked into the repo per-program. Schema creation must happen at runtime.
- Judges will look at the generated schema to verify it maps sensibly to the IDL types.
- Schema must handle IDL type diversity: structs, enums, nested types, arrays, options, public keys.

### From "without manual table descriptions"

- Zero configuration beyond providing the IDL (or program ID). No YAML/TOML mapping files, no manual column definitions.

### From "decode not only instructions but also account states"

- Two distinct decode paths are required, not just one. Instruction args AND account data deserialization.
- Account state decoding implies reading account data from the chain (via getAccountInfo or getProgramAccounts), not just parsing transaction instruction data.
- The discriminator (first 8 bytes) must be used to identify which account type is being decoded.

### From "batch mode" with "slot range or list of signatures"

- Two sub-modes within batch: slot-range-based AND signature-list-based.
- Slot range implies: fetch blocks in range, filter for transactions involving the target program.
- Signature list implies: accept a list of tx signatures, fetch and decode each.
- Both sub-modes must write to the same schema/tables as real-time mode.

### From "cold start" capability

- The indexer must persist its cursor (last processed slot/signature).
- On restart, it must detect the gap between last processed and current, backfill that gap, then switch to real-time.
- The transition from backfill to real-time must be seamless — no duplicate processing, no missed transactions.
- Implies a database table or metadata store for indexer state (last_processed_slot, indexer_status, etc.).

### From "Docker Compose with all dependencies"

- `docker compose up` must be the ONLY command needed. No pre-setup steps (no manual DB creation, no schema setup, no separate build step).
- All dependencies (PostgreSQL, the indexer binary, any init scripts) must be in the compose file.
- Environment variables must have sensible defaults or be documented in a `.env.example`.
- Implies the database schema is auto-created on first start (the indexer bootstraps itself).

### From "configuration via environment variables"

- All configurable values (RPC URL, database URL, program ID/IDL path, log level, batch size, etc.) must be env vars.
- No hardcoded connection strings or program addresses.

### From "structured logging"

- JSON-formatted log output (not just println). Machine-parseable.
- Should include: timestamp, log level, component/module, message, and contextual fields (slot number, tx signature, etc.).

### From judging criterion "code quality, architecture, and presence of tests"

- Tests are not optional. Judges explicitly look for them.
- Architecture must be visible in code structure — clean module boundaries, not a single main.rs monolith.
- Idiomatic Rust: proper error handling (Result, thiserror/anyhow), no unnecessary clones, clear ownership.
- Cargo workspace or well-organized crate structure signals architectural thinking.

### From judging criterion "clarity and completeness of the README"

- README is a first-class deliverable, not an afterthought. Judges read it before looking at code.
- "Architectural overview" implies a diagram (Mermaid or similar), not just prose.
- "Key architectural decisions and trade-offs" implies a decision log — not just what you chose, but what you considered and why you rejected alternatives.
- "Examples of API queries" implies curl commands or HTTP snippets that judges can actually run.
- Setup instructions must be copy-pasteable and work on first try.

### From the Twitter thread requirement

- Judges will read the thread. It's a soft evaluation of communication skills and technical depth.
- Should tell a story: what you built, what was hard, what surprised you, what trade-offs you made.
- Demonstrates you can explain technical decisions to a broader audience.

### From "participants can expand or simplify functionality"

- You CAN go beyond the requirements. Extra features (if polished) are a plus.
- You CAN simplify non-core features — but ALL core requirements must be met.
- Simplification should be documented and justified (not silent omission).

### From "Level: Middle"

- Expected skill level is intermediate. Overcomplicated solutions may be penalized if they don't work reliably.
- Working, clean, well-tested code beats ambitious but broken implementations.
- Judges expect solid fundamentals: error handling, clean architecture, tests, documentation.

---

## About Superteam Ukraine

Part of Superteam, a global community of Solana builders. They help onboard new talent, run bounties, and support the Solana ecosystem in Ukraine.
