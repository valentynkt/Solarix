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
PROGRAM_ID="JUP6LkMUje6dvM2FeAg8pUhfHayPdTHaFxVMLsXkICL"
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
        echo "  DRY: $*"
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
    local i=0
    printf "Waiting for health endpoint"
    until run curl -sf "$BASE_URL/health" | jq -e '.status == "ok"' >/dev/null 2>&1; do
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
    local i=0
    printf "Waiting for first indexed instructions"
    until run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/stats" |
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

header 2 "Register Jupiter V6"
run curl -sf -X POST "$BASE_URL/api/programs" \
    -H "Content-Type: application/json" \
    -d "{\"program_id\":\"$PROGRAM_ID\"}" | jq .

# Verify schema was created
if [ "$DRY_RUN" != "1" ]; then
    STATUS=$(curl -sf "$BASE_URL/api/programs/$PROGRAM_ID" | jq -r '.data.status')
    if [ "$STATUS" != "schema_created" ] && [ "$STATUS" != "indexing" ]; then
        echo "Unexpected registration status: $STATUS"
        exit 1
    fi
fi

header 3 "Inspect program details"
run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID" | jq .

header 4 "Wait for first indexed instructions"
poll_stats 40
run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/stats" | jq .

header 5 "Query decoded swap instructions"
run curl -sf \
    "$BASE_URL/api/programs/$PROGRAM_ID/instructions/route?limit=3" | jq .

header 6 "Filter: swaps with in_amount > 1 SOL"
run curl -sf \
    "$BASE_URL/api/programs/$PROGRAM_ID/instructions/route?filter=data.in_amount_gt=1000000000&limit=5" | jq .

header 7 "List account types"
run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/accounts" | jq .

header 8 "Time-series aggregation (swap count by hour)"
run curl -sf \
    "$BASE_URL/api/programs/$PROGRAM_ID/instructions/route/count?interval=hour" | jq .

header 9 "Restart Solarix and verify checkpoint recovery"
run docker compose stop solarix
run docker compose start solarix
poll_health 30
LAST_SLOT=$(run curl -sf "$BASE_URL/api/programs/$PROGRAM_ID/stats" |
    jq '.data.last_processed_slot')
if [ "$DRY_RUN" != "1" ] && [ "$LAST_SLOT" = "null" ] || [ "${LAST_SLOT:-0}" -le 0 ]; then
    echo "Checkpoint recovery check failed: last_processed_slot=$LAST_SLOT"
    exit 1
fi
echo "Checkpoint resumed at slot $LAST_SLOT"

header 10 "Final health check"
run curl -sf "$BASE_URL/health" | jq .

echo ""
success "Demo complete. All 10 steps passed."
