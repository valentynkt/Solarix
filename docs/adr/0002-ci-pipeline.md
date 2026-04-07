# ADR 0002: CI Pipeline

- **Status:** Accepted
- **Date:** 2026-04-07
- **Story:** [6.7 – CI Pipeline (Lint, Test, Coverage, Audit, Docker Smoke)](../../_bmad-output/implementation-artifacts/6-7-ci-pipeline-lint-test-coverage-audit-docker.md)

## Context

Solarix had no automated CI until Sprint 5. Every check was a manual
`cargo test` on a developer laptop, and the Sprint-4 end-to-end gate surfaced
three production bugs (an IDL PDA derivation mismatch, a missing `idl_json`
column, and a `bigint > text` filter bind error) that 257 lib-level unit tests
had silently missed. It also surfaced a `backoff` unmaintained advisory
(RUSTSEC-2025-0012) that nobody had caught because `cargo audit` was never
scheduled, and a `pretty`-format log regression because nobody had ever piped
a log line through `jq` after `docker compose up`.

The bounty submission cannot be silently broken by a careless merge. Story 6.7
introduces the first CI pipeline, and this ADR records the architecture
decisions so future contributors do not relitigate them.

## Decisions

### D1. Soft-gates for jobs that depend on un-landed stories

Several CI jobs in Story 6.7 depend on artifacts that other Epic-6 stories
produce (`/ready` from 6.3, `/metrics` from 6.2, the testcontainer harness
from 6.5, the `mainnet-smoke` feature and test file from 6.5). Each affected
job uses a `if: <guard>` step or a conditional `curl` step that degrades to a
warning. The SAME PR that lands the dependency removes the guard and turns
the soft check into a hard gate. This keeps 6.7 a real merge candidate on
Day 1 without painting the CI badge red.

Soft-gated today:

| Job                                   | Guard                                                                                              | Removed by          |
| ------------------------------------- | -------------------------------------------------------------------------------------------------- | ------------------- |
| `docker-smoke` step "wait for /ready" | warning-only curl                                                                                  | Story 6.3           |
| `docker-smoke` step "metrics check"   | warning-only curl                                                                                  | Story 6.2           |
| `nightly.yml` entire workflow         | `if: hashFiles('tests/mainnet_smoke.rs') != ''` AND a `grep` for the `mainnet-smoke` cargo feature | Story 6.5           |
| `integration` job                     | branches on `grep -q '^integration\b' Cargo.toml`                                                  | Story 6.5 (harness) |

`fuzz-smoke` was originally on this list with a guard
`if: hashFiles('fuzz/Cargo.toml') != ''`. That guard was always-true the
moment Story 6-7 merged, because Story 6.4 had already landed
`fuzz/Cargo.toml` and `fuzz/fuzz_targets/decode_instruction.rs` first. The
guard was removed in the post-merge code review (see Story 6-7 review
findings) and `fuzz-smoke` became a hard gate from Day 1.

Hard-gated from Day 1: `lint`, `unit`, `coverage`, `security`, `msrv`,
`toolchain-stable`, `toolchain-beta`, `fuzz-smoke`, and the always-on parts
of `docker-smoke` (reset, build, `/health` wait, JSON log check, cleanup).

### D2. Nightly mainnet smoke as a separate workflow file

`mainnet-smoke` is a fundamentally different beast: it touches an external
service (`api.mainnet-beta.solana.com`), it costs RPC quota, and it is allowed
to flake. Mixing it into the per-PR `ci.yml` would either slow every PR or
train developers to ignore CI failures. A separate `nightly.yml` triggered by
a cron (`0 6 * * *`) plus `workflow_dispatch` keeps the noisy external
dependency out of the critical path. On failure the workflow walks `git log`
for the most recent merged PR and posts a comment via `actions/github-script`
so humans notice the breakage.

### D3. Coverage ships in artifact-only mode (delta gate descoped)

Absolute coverage thresholds become a coverage-theater incentive (write tests
that cover lines, not behaviors). A delta gate that fails the job on a drop of
more than 2 percentage points versus the latest `main` baseline is the right
end-state, but specifying it precisely — which artifact, what retention, what
comparison script, how to bootstrap the first `main` run with no baseline — is
more work than 6.7 has room for. Story 6.7 ships the `coverage` job in
**artifact-only mode**: it produces `lcov.info`, prints a summary to the job
log, and uploads the artifact with 14-day retention. The delta gate is split
out as a follow-up story tracked in `_bmad-output/implementation-artifacts/deferred-work.md`.
Rationale: ship the data first, gate it after, avoid inventing a brittle
30-line bash baseline-fetch script that the next reviewer has to throw away.

### D4. No nightly toolchain in `lint` for import ordering

`CLAUDE.md` documents an import-ordering convention that would be enforced
automatically by `cargo +nightly fmt -- --check --config group_imports=StdExternalCrate,imports_granularity=Crate`.
Adding a nightly toolchain to the `lint` job for one config flag triples the
cache footprint and adds 30 seconds of toolchain install on every run. The
convention stays manual — reviewer discipline plus a paragraph in
`CONTRIBUTING.md`. A future story can add a dedicated `fmt-nightly` job if it
proves worth the cost.

### D5. `cargo deny` license check is fail-soft for Sprint 5

The Solana crate ecosystem includes crates with unusual license metadata
(`solana-frozen-abi-macro` and friends). A hard `deny.toml` license gate would
red the security job on Day 1 with no actionable fix. The `[licenses]` section
in `deny.toml` is fail-soft: it emits warnings on unlicensed or uncommon
licenses but never fails the job. `[advisories]`, `[bans]`, and `[sources]`
are hard gates. The license posture is revisited post-bounty.

### D6. Coverage job depends on `unit` (not on `integration`)

Including integration tests in `cargo llvm-cov` would push the coverage job
well over 15 minutes and double the testcontainer footprint. The unit-test
surface is the right coverage signal because it is deterministic, fast, and
tracks the business logic where regressions actually show up. Integration
tests are exercised by their own job and should not double-count toward
coverage.

### D7. `Swatinem/rust-cache@v2`, not `actions/cache@v4`

`actions/cache` requires hand-rolling cache keys for `~/.cargo/registry`,
`~/.cargo/git`, and `target/`, and gets the eviction logic wrong on
macOS/Windows hosts. `Swatinem/rust-cache@v2` is the de-facto Rust community
cache action, handles all four directories, and is pinned by tag. All jobs
share `shared-key: "solarix-ci"` so cache hits cross job boundaries. The
`nightly.yml` workflow uses a separate `shared-key: "solarix-nightly"` so its
cron runs do not pollute the per-PR cache.

### D8. MSRV pinning strategy (1.88)

`rust-toolchain.toml` pins the workspace toolchain at repo root. The
`dtolnay/rust-toolchain` action then installs that exact version in CI.
Without this mechanism, CI silently uses whatever `ubuntu-latest` happens to
ship and the `msrv` job becomes meaningless.

Story 6.7 Task 1 walked the MSRV forward from 1.86 against the current
`Cargo.lock`:

| Attempt | Result                                      |
| ------- | ------------------------------------------- |
| 1.86    | blocked — `home@0.5.12` requires rustc 1.88 |
| 1.88    | builds cleanly — **pinned**                 |

The lowest version that actually compiles the current `Cargo.lock` is 1.88,
so that is the MSRV. The `msrv` job installs 1.88 and runs
`cargo build --release`. The `toolchain-matrix` job exercises `stable` and
`beta` against the same lockfile to surface upcoming breakage early; `beta`
is allowed to fail.

One caveat: clippy 1.88 promotes `uninlined_format_args` to the default
`style` group, where clippy 1.90 (the dev-box version when the story was
written) has it moved to `pedantic`. Pinning 1.88 therefore surfaces a handful
of extra baseline warnings. Story 6.7 Task 2 absorbs these: the additional
uninlined-format-args fixes, the `FailDecoder` `#[allow(dead_code)]`, and the
removed-absurd-comparison assertion are all part of the clippy baseline clean.

### D9. Story 6.7 does NOT modify application source (except the clippy baseline)

Hard rule. If a CI job exposes a real bug (clippy warning, audit advisory,
panic in fuzz), the bug fix lands in a separate PR. 6.7 is infrastructure-only;
mixing in code fixes makes the diff impossible to review and conflates "did we
ship CI?" with "did we ship every CI fix?"

The only `src/` edits Story 6.7 includes are:

- `clippy.toml` — add `allow-unwrap-in-tests` and `allow-panic-in-tests` so
  the existing `[lints.clippy]` stack doesn't deny test-side `unwrap!`.
- The six (1.90-baseline) lib warnings fixed inline, plus the three extra
  `uninlined_format_args` warnings that clippy 1.88 surfaces.
- `#[allow(clippy::too_many_arguments)]` on `process_chunk` and
  `enrich_instruction` (refactoring 9-arg functions to take a struct is out of
  scope for a CI story).
- `#[allow(dead_code)]` on the `FailDecoder` test scaffold kept for later
  decode-failure tests.

Everything else Story 6.7 touches is infrastructure: the two workflow files,
`deny.toml`, `.gitleaks.toml`, `rust-toolchain.toml`, this ADR,
`CONTRIBUTING.md`, and the README badge.

## Consequences

**Good:**

- Every push and PR now runs lint, unit, integration-compile, coverage,
  security, docker-smoke, and MSRV in parallel.
- The Sprint-4 bug classes (`pretty` log regression, unmaintained advisory,
  missing type-bind coverage, docker-compose startup failures) each have a
  dedicated CI job that would catch them.
- The soft-gate strategy lets 6.7 land before 6.2/6.3/6.4/6.5/6.6 without
  painting the CI badge red.
- MSRV drift is visible: the `msrv` and `toolchain-matrix` jobs both hit the
  same `Cargo.lock`, so upstream MSRV bumps show up in CI before a developer
  trips over them locally.

**Bad / known trade-offs:**

- The `integration` job is effectively a compile-time gate today — the test
  functions are `#[ignore]` until Story 6.5 ships the testcontainer harness.
  The postgres services block in the workflow is wasted compute until then,
  but leaving it in place costs nothing and makes 6.5 a smaller diff.
- Clippy is pinned to 1.88 via `rust-toolchain.toml`, so newer clippy lints
  won't be enforced until we bump MSRV. This is an accepted trade-off: the
  alternative (run `lint` on stable while `msrv` builds on 1.88) creates the
  "works locally, red in CI" asymmetry that pinning was supposed to prevent.
- Coverage ships without a delta gate, so a PR can silently drop coverage.
  Mitigated by the fact that `lcov.info` is uploaded on every run and any
  reviewer can compare against the previous `main` run.

## Alternatives Considered

### Run `lint` on stable instead of pinned MSRV

Gives us the latest clippy lints for free and matches most other Rust projects.
Rejected because it creates the same "works locally, red in CI" asymmetry that
`rust-toolchain.toml` exists to prevent: a developer with stable installed
would see different warnings than CI running MSRV. Pinning is more internally
consistent even though it slows down lint discovery.

### Use `actions/cache@v4` directly

Gives finer-grained control over cache keys. Rejected because
`Swatinem/rust-cache@v2` has become the de-facto Rust community standard, is
actively maintained, and handles the four cargo directories plus the stale
eviction logic without per-project bikeshedding.

### Nightly mainnet smoke inside `ci.yml`

Simpler single-workflow setup. Rejected because it either slows every PR or
trains developers to ignore CI failures whenever the external RPC is flaky.
See D2.

### Ship coverage with a hard absolute threshold

Immediate gate, no bootstrap problem. Rejected because absolute thresholds
incentivize "drive-by" tests that hit uncovered lines without meaningfully
validating behavior. A delta gate (drop > 2pp fails) is better but requires a
baseline-fetch script we don't have time to specify precisely. See D3.

### Pin `chainparser` back into `Cargo.toml` to make `allow-git` meaningful

Would give the `deny.toml` `[sources]` section real work to do. Rejected
because `chainparser` is still dormant and Solarix uses the built-in
Borsh decoder today. When a future story reintroduces a git dep, that story
also extends `allow-git` with the specific repo URL.

## References

- Source story: [6.7 CI Pipeline](../../_bmad-output/implementation-artifacts/6-7-ci-pipeline-lint-test-coverage-audit-docker.md)
- Epic 6 (observability + hardening): [epic-6](../../_bmad-output/planning-artifacts/epics/epic-6-observability-production-hardening.md)
- Sprint-4 gate that motivated this story: [e2e-verification-sprint-4](../../_bmad-output/implementation-artifacts/e2e-verification-sprint-4.md)
- CLAUDE.md lints stack (`unwrap_used = "deny"` etc.): [`CLAUDE.md`](../../CLAUDE.md#code-conventions)
