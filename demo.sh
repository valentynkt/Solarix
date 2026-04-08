#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# Solarix end-to-end demo — 10 steps
#
# Prerequisites: curl, jq, docker
# Usage:         bash demo.sh
# Dry run:       DEMO_DRY_RUN=1 bash demo.sh
# Custom URL:    BASE_URL=http://my-host:3000 bash demo.sh
# =============================================================================

BASE_URL="${BASE_URL:-http://localhost:3000}"
# Meteora DLMM — verified end-to-end in the Sprint-4 e2e gate (199 swaps in 5 min).
# `swap` is the highest-volume instruction; `lbpair` is the primary account type.
# Override with PROGRAM_ID=... INSTRUCTION_NAME=... ACCOUNT_TYPE=... if needed.
PROGRAM_ID="${PROGRAM_ID:-LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo}"
INSTRUCTION_NAME="${INSTRUCTION_NAME:-swap}"
ACCOUNT_TYPE="${ACCOUNT_TYPE:-lbpair}"
DRY_RUN="${DEMO_DRY_RUN:-0}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

header() {
    printf "\n\033[1;34m━━━ [Step %s] %s ━━━\033[0m\n" "$1" "$2"
}

success() {
    printf "\033[1;32m%s\033[0m\n" "$1"
}

run() {
    if [ "$DRY_RUN" = "1" ]; then
        # Print to stderr so the dry message does not pollute downstream pipes
        # (e.g. `run curl ... | jq .` would otherwise feed the echo string to jq).
        echo "  DRY: $*" >&2
    else
        "$@"
    fi
}

check_dep() {
    command -v "$1" &>/dev/null || {
        echo "Required tool not found: $1"
        exit 1
    }
}

poll_health() {
    local max=$1
    if [ "$DRY_RUN" = "1" ]; then
        echo "  DRY: poll /health until status==healthy (max ${max} attempts)"
        return 0
    fi
    local i=0
    printf "Waiting for health endpoint"
    until curl -sf "$BASE_URL/health" | jq -e '.status == "healthy"' >/dev/null 2>&1; do
        i=$((i + 1))
        if [ "$i" -gt "$max" ]; then
            echo ""
            echo "Health check timed out after $((max * 2))s"
            exit 1
        fi
        printf "."
        sleep 2
    done
    echo ""
}

poll_stats() {
    local max=$1
    if [ "$DRY_RUN" = "1" ]; then
        echo "  DRY: poll /stats until total_instructions>0 (max ${max} attempts)"
        return 0
    fi
    local i=0
    printf "Waiting for first indexed instructions"
    until curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/stats" |
        jq -e '.data.total_instructions > 0' >/dev/null 2>&1; do
        i=$((i + 1))
        if [ "$i" -gt "$max" ]; then
            echo ""
            echo "Stats poll timed out after $((max * 3))s — backfill may still be in progress"
            exit 1
        fi
        printf "."
        sleep 3
    done
    echo ""
}

# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------

check_dep curl
check_dep jq
check_dep docker

# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------

header 1 "Start the stack"
run docker compose up --build -d
poll_health 60

header 2 "Register Meteora DLMM"
run curl -sf -X POST "$BASE_URL/api/programs" \
    -H "Content-Type: application/json" \
    -d "{\"program_id\":\"$PROGRAM_ID\"}" | jq .

# Verify schema was created
if [ "$DRY_RUN" != "1" ]; then
    STATUS=$(curl -sf "$BASE_URL/api/programs/$PROGRAM_ID" | jq -r '.data.status')
    if [ "$STATUS" != "schema_created" ]; then
        echo "Unexpected registration status: $STATUS"
        exit 1
    fi
fi

header 3 "Restart to start the indexing pipeline"
# The pipeline auto-starts for registered programs on restart.
# Registration saves the IDL to the DB; restart picks it up and begins indexing.
run docker compose restart solarix
poll_health 30
run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID" | jq .

header 4 "Wait for first indexed instructions"
poll_stats 40
run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/stats" | jq .

header 5 "Query decoded swap instructions"
run curl -sf \
    "$BASE_URL/api/programs/$PROGRAM_ID/instructions/$INSTRUCTION_NAME?limit=3" | jq .

header 6 "Filter: swaps with amount_in > 0.001 SOL (1,000,000 lamports)"
run curl -sf \
    "$BASE_URL/api/programs/$PROGRAM_ID/instructions/$INSTRUCTION_NAME?filter=data.amount_in_gt=1000000&limit=5" | jq .

header 7 "List account types in IDL"
run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/accounts" | jq .

header 8 "Time-series aggregation (swap count by hour)"
run curl -sf \
    "$BASE_URL/api/programs/$PROGRAM_ID/instructions/$INSTRUCTION_NAME/count?interval=hour" | jq .

header 9 "Restart Solarix and verify checkpoint recovery"
run docker compose stop solarix
run docker compose start solarix
poll_health 30
if [ "$DRY_RUN" = "1" ]; then
    echo "  DRY: curl /stats and assert last_processed_slot > 0"
else
    LAST_SLOT=$(curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/stats" |
        jq '.data.last_processed_slot')
    if [ "$LAST_SLOT" = "null" ] || [ "${LAST_SLOT:-0}" -le 0 ]; then
        echo "Checkpoint recovery check failed: last_processed_slot=$LAST_SLOT"
        exit 1
    fi
    echo "Checkpoint resumed at slot $LAST_SLOT"
fi

header 10 "Final health check"
run curl -sf "$BASE_URL/health" | jq .

echo ""
success "Demo complete. All 10 steps passed."
