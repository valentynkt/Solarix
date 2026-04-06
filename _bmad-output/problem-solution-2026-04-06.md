# Problem Solving Session: axum Handler !Send Future

**Date:** 2026-04-06
**Problem Solver:** Valentyn
**Problem Category:** Rust async type system / compiler inference limitation

---

## PROBLEM DEFINITION

### Initial Problem Statement

`register_program` handler doesn't compile because axum requires handler futures to be `Send`, but the handler's async state machine is `!Send`. 9+ approaches tried over multiple sessions without resolution.

### Refined Problem Statement

The Rust compiler's async Send inference fails when a handler's async state machine combines:

1. `RwLockWriteGuard` references (from `tokio::sync::RwLock::write()`)
2. sqlx `Executor` references (from `tx.as_mut()` and `raw_sql().execute()`)

All underlying types ARE `Send` — the compiler simply cannot prove it for the composed state machine. This is a known Rust compiler limitation (rust#96865, sqlx#1636, sqlx#2567).

### Success Criteria

- `cargo build` compiles with zero errors
- `cargo clippy` passes
- `cargo fmt -- --check` passes
- `cargo test` passes all tests
- No `Box::pin` or `tokio::spawn` hacks at the handler level

---

## DIAGNOSIS AND ROOT CAUSE ANALYSIS

### Root Cause: "Not General Enough" Inference Failure

The error manifests as three distinct "not general enough" messages:

1. `Executor<'_>` for `&'0 mut PgConnection` — from sqlx transactions (`tx.as_mut()`)
2. `Send` for `&Idl` — from borrowing owned data inside async fns
3. `Send` for `&tokio::sync::RwLock<ProgramRegistry>` — from `registry.write()`

These references have SPECIFIC lifetimes tied to the async state machine. The `+ Send` bound on `Handler` / `tokio::spawn` / `Box::pin` requires Send for ALL lifetimes (higher-ranked). The compiler can only prove Send for the specific lifetime, not universally.

### What Was Eliminated

| Hypothesis                              | Status     | Evidence                                                                       |
| --------------------------------------- | ---------- | ------------------------------------------------------------------------------ |
| `Idl: !Sync` (making `&Idl: !Send`)     | ELIMINATED | Standalone binary test: `Idl` is `Send + Sync`                                 |
| `raw_sql` vs `query` difference         | ELIMINATED | Isolated test: `raw_sql` future IS Send                                        |
| For-loop + await + transaction          | ELIMINATED | Batch DDL committed, still failed                                              |
| `RwLockWriteGuard` held across await    | ELIMINATED | Guard explicitly dropped before await                                          |
| `generate_schema` function in isolation | ELIMINATED | `_require_send` passes (note: test was in `cfg(test)`, unreliable — see below) |

### Critical Methodology Insight

`_require_send` tests placed in `#[cfg(test)]` blocks are **NOT compiled by `cargo check --lib`** when the main crate has compile errors. Three agents reported "compiles fine" for `commit_registration` and `generate_schema` futures, but those tests were never actually type-checked. The `cargo check` success was just the pre-existing error being the only error.

### Binary Search Results (Definitive)

| Test                   | Code in Handler                                     | Result                 |
| ---------------------- | --------------------------------------------------- | ---------------------- |
| Stub handler           | `return Ok(stub)`                                   | COMPILES               |
| Phase 1 only           | `prepare_registration().await`                      | COMPILES               |
| Phase 2 only           | `commit_registration(pool, todo!()).await`          | COMPILES (unreachable) |
| Data held across yield | `prepare_reg().await; yield_now().await; use data;` | COMPILES               |
| Phase 1 + Phase 2      | `prepare_reg().await; commit_reg(pool, data).await` | FAILS                  |

**The combination of two different async function calls in the same state machine triggers the inference failure, even when each is Send individually.**

---

## SOLUTION

### Architecture: Box::pin Leaf Functions + Owned Parameters

The fix has two parts:

**Part 1: Owned parameters on async function boundaries**

Changed `generate_schema`, `seed_metadata`, `write_registration`, and `update_program_status` to take all parameters by value (`Idl`, `String`, `PgPool`) instead of by reference (`&Idl`, `&str`, `&PgPool`). This makes their returned futures `'static`, removing the specific-lifetime references that fail the "general enough" check.

**Part 2: Box::pin functions that create internal references**

Functions with internal `tx.as_mut()` or `registry.write()` create specific-lifetime references INSIDE the async block. Even with owned parameters, these internal references propagate through `impl Future` return types. `Box::pin(async move { ... })` with `+ Send` hides these internal lifetimes behind a trait object.

Applied to:

- `generate_schema` → `fn ... -> Pin<Box<dyn Future + Send>>`
- `seed_metadata` → `fn ... -> Pin<Box<dyn Future + Send>>`
- `write_registration` → `fn ... -> Pin<Box<dyn Future + Send>>`
- `update_program_status` → `fn ... -> Pin<Box<dyn Future + Send>>`
- `prepare_registration` (handler helper) → `fn ... -> Pin<Box<dyn Future + Send>>`
- `rollback_cache` (handler helper) → `fn ... -> Pin<Box<dyn Future + Send>>`

**Part 3: Remove explicit transaction from `generate_schema`**

`raw_sql(&batch).execute(tx.as_mut())` triggers the Executor lifetime issue even inside `Box::pin`. Changed to `raw_sql(&batch).execute(&pool)` — PostgreSQL wraps multi-statement raw SQL in an implicit transaction, and all DDL uses `IF NOT EXISTS` (idempotent).

### Files Modified

| File                    | Changes                                                                                  |
| ----------------------- | ---------------------------------------------------------------------------------------- |
| `src/storage/schema.rs` | `generate_schema` + `seed_metadata`: owned params, `Box::pin`, pool-direct execution     |
| `src/registry.rs`       | `write_registration` + `update_program_status`: owned params, `Box::pin`                 |
| `src/api/handlers.rs`   | `prepare_registration` + `rollback_cache`: `Box::pin` with owned Arc; handler cleaned up |

### Verification

- `cargo build` — 0 errors, 0 warnings
- `cargo clippy` — no issues
- `cargo fmt -- --check` — formatted
- `cargo test` — 109 passed, 3 ignored

---

## KEY LEARNINGS

### The Rule

In Rust async code composed with axum/sqlx, if an async function contains `tx.as_mut()`, `raw_sql().execute()`, or `RwLock::write()`, its `impl Future` return type propagates specific-lifetime references. When composed in a larger state machine, the compiler cannot prove "general enough" Send.

### The Pattern

```rust
// BEFORE (fails Send inference when composed):
async fn do_db_work(pool: PgPool, data: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("...").bind(data).execute(tx.as_mut()).await?;
    tx.commit().await?;
    Ok(())
}

// AFTER (Send-safe for composition):
fn do_db_work(pool: PgPool, data: String)
    -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
{
    Box::pin(async move {
        let mut tx = pool.begin().await?;
        sqlx::query("...").bind(&data).execute(tx.as_mut()).await?;
        tx.commit().await?;
        Ok(())
    })
}
```

### What to Avoid

- Don't trust `_require_send` tests in `#[cfg(test)]` when the crate doesn't compile
- Don't assume `Box::pin` at the OUTER level fixes issues — it must be at the LEAF level where the specific-lifetime references originate
- Don't assume "types are Send" means "the future is Send" — the compiler's inference is about lifetime generality, not type capability

_Generated using BMAD Creative Intelligence Suite - Problem Solving Workflow_
