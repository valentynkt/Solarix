# Operating Solarix

This doc covers day-to-day operations: log formats, health probes, metrics,
graceful shutdown, and common `jq` recipes for debugging. It is aimed at
operators running Solarix in a containerized deployment.

## Structured Logging Conventions

Solarix emits JSON-structured logs (default format) with a stable field
convention introduced in Story 6.1. Every `info!`/`warn!`/`error!` event
carries enough context to reconstruct the path of a single request, decode,
or RPC call via `jq` without grepping by timestamp.

### Field glossary

Plain scalar fields use `snake_case`. Hierarchical fields use `dot.notation`:

| Field                                                         | Where                              | Meaning                                                                                                                     |
| ------------------------------------------------------------- | ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `program_id`                                                  | every pipeline + API + decoder log | The Solana program being indexed (base58 pubkey)                                                                            |
| `schema_name`                                                 | every pipeline + storage log       | Per-program DB schema (`{name}_{first_8_of_program_id}`)                                                                    |
| `slot`                                                        | per-block / per-tx logs            | Solana slot number                                                                                                          |
| `signature`                                                   | per-tx logs                        | Transaction signature                                                                                                       |
| `chunk_start`, `chunk_end`                                    | backfill chunk logs                | Inclusive slot range of the current chunk                                                                                   |
| `error.kind`                                                  | decode warn/error logs             | `DecodeError` variant (snake_case): `unknown_discriminator`, `deserialization_failed`, `idl_not_loaded`, `unsupported_type` |
| `pipeline.state.from`, `pipeline.state.to`                    | state transition events            | Snake_case state: `initializing`, `backfilling`, `catching_up`, `streaming`, `shutting_down`, `running`                     |
| `request.id`                                                  | every API request span             | UUIDv7 — sortable by time                                                                                                   |
| `http.method`, `http.target`, `http.route`, `http.user_agent` | API request spans                  | Request metadata                                                                                                            |
| `http.status_code`, `http.duration_ms`                        | API response events                | Response metadata                                                                                                           |

### Enforced rules

- **Every `warn!` / `error!` in `src/pipeline/mod.rs` and `src/pipeline/ws.rs` MUST carry `program_id`.** Enforced by `tests/log_levels.rs` Rule R1 (hard gate). Fail this rule and the build breaks.
- **No `info!` in per-block / per-tx hot paths in `src/pipeline/mod.rs`.** Per-block fields use `debug!`. Enforced by `tests/log_levels.rs` Rule R2 (hard gate).
- **Every `pub async fn` in the tracing scope (pipeline, api/handlers, idl, registry, storage/writer) carries `#[tracing::instrument]`.** Enforced by `tests/instrument_coverage.rs`.

### jq recipes

Filter all log lines for a specific request id:

```bash
docker compose logs solarix | jq 'select(.span.fields["request.id"] == "0193ae0a-…")'
```

Filter all logs for a specific program during a multi-program run:

```bash
docker compose logs solarix | jq 'select(.span.fields.program_id == "LBUZK…wxo")'
```

Find every decode failure for a specific program with its error variant:

```bash
docker compose logs solarix | jq 'select(.fields.program_id == "LBUZK…wxo" and .fields["error.kind"] != null)' | jq '.fields."error.kind"' | sort | uniq -c
```

Reconstruct the full pipeline state machine trace:

```bash
docker compose logs solarix | jq 'select(.fields.message == "pipeline state transition")'
```

### Log levels

- `error!` — fatal: pipeline will not proceed (e.g., DB down, exhausted retries)
- `warn!` — skip-and-log: operation skipped but pipeline continues (e.g., unknown discriminator, getTransaction failed)
- `info!` — state transitions, startup/shutdown, per-chunk summaries, API requests
- `debug!` — per-block / per-tx / per-RPC hot paths (filtered out at production level)
- `trace!` — wire data, only enabled for deep debugging
