# Epic 7: Documentation & Demo Readiness

Judge opens the repo and finds a polished README with Mermaid architecture diagrams, copy-pasteable setup, curl-able API examples, and a compelling demo flow -- the bounty submission wrapper.

## Story 7.1: README & Architecture Documentation

As a bounty judge,
I want a comprehensive README that explains the project's architecture, setup, and API with clear examples,
So that I can evaluate the project's quality and run it within minutes of opening the repo.

**Acceptance Criteria:**

**Given** the `README.md` file
**When** I read it
**Then** it contains the following sections in order:

1. **Project Overview**: One-paragraph summary of what Solarix does and why it's unique (runtime dynamic schema generation vs compile-time codegen)
2. **Architecture**: Mermaid diagram showing the 4-layer pipeline (Read -> Decode -> Store -> Serve) with data flow arrows, channel boundaries, and external dependencies (Solana RPC, PostgreSQL)
3. **Technology Stack**: Table of key crates with version and purpose
4. **Quick Start**: Copy-pasteable commands: `git clone`, `docker compose up`, register a program, query data -- verified to work end-to-end
5. **API Reference**: All 12 endpoints with method, path, description, and curl examples showing real request/response bodies
6. **Configuration**: Table of all env vars with description, type, and default value (mirrors `.env.example`)
7. **Architectural Decisions**: 3-5 key decisions with trade-off analysis (e.g., runtime IDL decode vs codegen, hybrid typed+JSONB storage, Option C concurrent backfill, chainparser fork vs custom decoder)
8. **Testing**: How to run tests (`cargo test`, integration requirements, proptest)
9. **Future Work**: Bullet list of post-MVP enhancements (Geyser/gRPC, GraphQL, schema evolution, etc.)

**Given** the Mermaid architecture diagram
**When** it renders on GitHub
**Then** it shows: HTTP RPC and WebSocket inputs, bounded mpsc channels between stages, PostgreSQL output, axum API server, and the pipeline state machine

**Given** the Quick Start section
**When** a judge follows the commands exactly
**Then** they can go from `git clone` to querying indexed data in under 5 minutes (excluding Docker build time)

**Given** all documentation
**When** I review it
**Then** all content is in English, uses consistent formatting, and contains no placeholder text or TODO markers

## Story 7.2: Demo Script & Bounty Submission Polish

As a bounty judge,
I want a scripted demo that showcases all core capabilities and a polished codebase ready for evaluation,
So that I can verify all bounty criteria are met without guesswork.

**Acceptance Criteria:**

**Given** a demo script (documented in README or as `demo.sh`)
**When** executed step by step
**Then** it demonstrates:

1. `docker compose up` -- stack starts, health check passes
2. `POST /api/programs` with a real program ID (e.g., Jupiter v6) -- IDL fetched, schema generated
3. `GET /api/programs/{id}` -- shows status transition to `indexing`
4. Batch indexing of a small slot range -- data lands in DB
5. `GET /api/programs/{id}/instructions/{name}` with filters -- returns decoded results
6. `GET /api/programs/{id}/accounts/{type}` -- returns account states
7. `GET /api/programs/{id}/stats` -- shows statistics
8. `GET /api/programs/{id}/instructions/{name}/count?interval=hour` -- shows time series
9. Kill and restart -- cold start resumes from checkpoint
10. `GET /health` -- confirms healthy after restart

**Given** the codebase
**When** I review source files
**Then** all `pub` items have `///` doc comments
**And** each module's `mod.rs` has a module-level `//!` doc comment explaining its purpose
**And** `lib.rs` has a crate-level doc comment with project overview

**Given** the bounty submission checklist
**When** verified
**Then** all deliverables are present: public GitHub repo, comprehensive README, `.env.example`, Docker Compose, working demo with real Anchor program
**And** all six judging criteria are demonstrably met: (1) dynamic schema generation, (2) real-time mode with cold start, (3) reliability features, (4) advanced API, (5) code quality, (6) README completeness
**And** a Twitter thread documenting the build experience, technical challenges, and trade-offs is published and linked from the README
