# Solarix Operator Runbook

This runbook covers six common operational scenarios. Each scenario lists the symptoms you may observe, a diagnostic command, and the recommended fix.

For the structured log field glossary, jq recipes, and full observability reference, see [docs/operating-solarix.md](operating-solarix.md).

---

## Scenario 1 — Healthy Startup Check

**Symptoms:** The stack just started and you want to confirm it is indexing correctly.

**Diagnosis:**

```bash
curl -s http://localhost:3000/health | jq
```

Healthy response (program registered and indexing):

```json
{
  "status": "ok",
  "database": "connected",
  "programs": [
    {
      "program_id": "JUP6LkMUje6dvM2FeAg8pUhfHayPdTHaFxVMLsXkICL",
      "pipeline_status": "indexing",
      "last_processed_slot": 318472910
    }
  ]
}
```

Not yet registered (programs array is empty):

```json
{
  "status": "ok",
  "database": "connected",
  "programs": []
}
```

**Fix:** If `programs` is empty, register a program with `POST /api/programs`. The pipeline status transitions: `schema_created` → `indexing` once the pipeline starts. If the database shows `"disconnected"`, check `DATABASE_URL` and whether PostgreSQL is reachable.

---

## Scenario 2 — RPC 429 / Rate Limit

**Symptoms:** Backfill stalls or makes very slow progress. Logs show `WARN`-level events with `rpc_failed` or HTTP status 429.

**Diagnosis:**

```bash
# Stream logs and filter for 429 events
docker compose logs solarix | jq -R 'fromjson? | select(.level == "WARN" and (.fields.status_code == 429 or (.fields.message // "" | test("429"))))'

# Check the retry counter if metrics are enabled (SOLARIX_METRICS_ENABLED=true)
curl -s http://localhost:3000/metrics | grep rpc_retries_total
```

**Fix:** Lower `SOLARIX_RPC_RPS` (default `10`, which matches the public mainnet-beta limit). Set it in `.env`:

```
SOLARIX_RPC_RPS=5
```

For sustained high-throughput backfill, switch to a paid RPC endpoint that allows higher request rates. Helius, QuickNode, and Triton all offer higher limits.

---

## Scenario 3 — IDL Fetch Failure

**Symptoms:** A program stays at `schema_created` and never transitions to `indexing`. Logs show an `IdlError::NotFound` or `error_kind = "not_found"`.

**Diagnosis:**

```bash
docker compose logs solarix | jq -R 'fromjson? | select(.fields.error_kind == "not_found")'
```

**Fix:** The on-chain IDL PDA does not exist for this program. You have two options:

1. Supply the IDL manually in the registration request:

   ```bash
   curl -s -X POST http://localhost:3000/api/programs \
     -H "Content-Type: application/json" \
     -d '{"program_id":"<PROGRAM_ID>","idl":{...}}' | jq
   ```

2. Bundle the IDL as a file at `idls/<PROGRAM_ID>.json` in the repository root before starting Solarix. The fetch cascade checks this location automatically.

---

## Scenario 4 — DB Connection Exhaustion

**Symptoms:** Logs show `StorageError::ConnectionFailed` or sqlx pool timeout errors. API requests return `500 STORAGE_ERROR`.

**Diagnosis:**

```bash
# Count active PostgreSQL connections
docker compose exec postgres psql -U solarix -c \
  "SELECT count(*) FROM pg_stat_activity WHERE datname = 'solarix';"

# Look for pool timeout messages in logs
docker compose logs solarix | jq -R 'fromjson? | select(.fields.message | test("pool timed out"; "i"))'

# Inspect long-running queries
docker compose exec postgres psql -U solarix -c \
  "SELECT pid, state, query_start, query FROM pg_stat_activity WHERE state != 'idle' ORDER BY query_start;"
```

**Fix:** Increase the pool ceiling in `.env`:

```
SOLARIX_DB_POOL_MAX=20
```

If long-running queries are holding connections, identify and terminate them with `SELECT pg_terminate_backend(pid)`. Common culprit: a query on the `_instructions` table without an index-backed filter on a large dataset.

---

## Scenario 5 — Filter Returns 400 or Unexpected 500

**Symptoms:** A filter query returns `{"error":{"code":"INVALID_FILTER",...}}` (400) or `{"error":{"code":"QUERY_FAILED",...}}` (500).

**Diagnosis:**

```bash
# Check which fields are available for this instruction/account type
curl -s "http://localhost:3000/api/programs/<ID>/instructions/<name>" | jq '.meta.available_fields'

# Try the query with verbose output
curl -sv "http://localhost:3000/api/programs/<ID>/instructions/<name>?filter=slot_gt=100"
```

**Fix:**

- **BIGINT promoted columns** (e.g. `slot`, `lamports`) are queried directly by column name: `slot_gt=100`. Do **not** prefix them with `data.` — that path uses lexicographic TEXT comparison inside JSONB, which produces incorrect results for numeric ranges.
- **JSONB fields** use the `data.` prefix: `data.in_amount_gt=1000000000`.
- Check the `INVALID_FILTER` error body for the `available_fields` list to find the correct field names.

See the filter syntax reference in [README.md § Filter Syntax](../README.md#filter-syntax).

---

## Scenario 6 — Checkpoint Reset (Force Full Re-Index)

**Symptoms:** You want to wipe all indexed data for a program and restart from scratch (e.g. after an IDL change or a data integrity issue).

**Diagnosis — check the current checkpoint:**

```bash
curl -s http://localhost:3000/api/programs/<ID>/stats | jq '.data.last_processed_slot'
```

**Fix:**

> **Warning:** The following commands permanently delete all indexed instructions and account states for the program.

```bash
# Step 1: Deregister the program and drop its schema
curl -s -X DELETE "http://localhost:3000/api/programs/<ID>?drop_tables=true" | jq

# Step 2: Re-register — Solarix will fetch the IDL and restart indexing from scratch
curl -s -X POST http://localhost:3000/api/programs \
  -H "Content-Type: application/json" \
  -d '{"program_id":"<ID>"}' | jq
```

After re-registration, backfill begins from the configured `SOLARIX_START_SLOT` (or chain genesis if not set). Monitor progress with `GET /api/programs/<ID>/stats`.

---

See [docs/operating-solarix.md](operating-solarix.md) for the structured log field glossary and jq recipes.
