# Epic List

## Epic 1: Project Foundation & First Boot

Operator can `docker compose up`, the system starts, connects to PostgreSQL, creates system tables, and responds on the health endpoint -- proving the project is real and runnable.
**FRs covered:** FR38, FR39, FR40, FR41
**NFRs touched:** NFR9, NFR12, NFR13, NFR14

## Epic 2: Program Registration & IDL Acquisition

User can register any Anchor program by ID (auto-fetches IDL from chain) or upload an IDL manually. The system generates a full PostgreSQL schema with typed columns, indexes, and JSONB safety net -- visible proof of runtime dynamism.
**FRs covered:** FR1, FR2, FR3, FR4, FR5, FR6, FR7, FR8, FR9
**NFRs touched:** NFR8

## Epic 3: Transaction Decoding & Batch Indexing

User can trigger batch indexing (slot range or signature list) and see decoded instruction args + account states land in the database -- the core "it actually works" moment for judges.
**FRs covered:** FR10, FR11, FR12, FR13, FR14, FR15, FR16, FR17, FR21, FR22, FR34
**NFRs touched:** NFR2, NFR4, NFR5

## Epic 4: Real-Time Streaming & Cold Start

System streams new transactions via WebSocket, handles disconnects with automatic gap backfill, and on restart resumes from the last checkpoint -- demonstrating production reliability.
**FRs covered:** FR18, FR19, FR20, FR23, FR24, FR25, FR35
**NFRs touched:** NFR3, NFR4, NFR6, NFR7

## Epic 5: Query API & Filtering

User can query indexed data through REST endpoints with multi-parameter filters, pagination, aggregation, and program statistics -- the "it's actually useful" layer.
**FRs covered:** FR26, FR27, FR28, FR29, FR30, FR31, FR32, FR33, FR37
**NFRs touched:** NFR1

## Epic 6: Observability & Production Hardening

Operator gets structured JSON logs with per-stage tracing spans, and the system handles edge cases gracefully (unknown discriminators, rate limits, >90% decode failures) -- signals senior engineering quality.
**FRs covered:** FR36
**NFRs touched:** NFR10, NFR11, NFR12, NFR14

## Epic 7: Documentation & Demo Readiness

Judge opens the repo and finds a polished README with Mermaid architecture diagrams, copy-pasteable setup, curl-able API examples, and a compelling demo flow -- the bounty submission wrapper.
**FRs covered:** Deliverables (not numbered FRs)
**NFRs touched:** NFR13

---
