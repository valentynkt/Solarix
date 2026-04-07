#!/usr/bin/env bash
#
# scripts/collect-fuzz-corpus.sh
#
# Populate fuzz/corpus/decode_instruction/ with real mainnet instruction blobs
# for three popular Anchor programs. These are used as seed inputs for the
# decoder fuzz target defined in Story 6-4.
#
# Prerequisites:
#   - jq
#   - base64 (GNU or BSD)
#   - curl
#   - python3 (for base58 decoding of Solana transaction data)
#
# Output layout:
#   fuzz/corpus/decode_instruction/
#     <program-slug>_<signature-prefix>_<ix-index>.bin
#
# Each .bin is the raw instruction data bytes (discriminator + borsh-encoded
# args) extracted from a real mainnet transaction. Story 6-4 requires at
# least 50 seed inputs across the three target programs.

set -euo pipefail

RPC_URL="${SOLANA_RPC_URL:-https://api.mainnet-beta.solana.com}"
CORPUS_DIR="fuzz/corpus/decode_instruction"
PER_PROGRAM="${PER_PROGRAM:-20}"

mkdir -p "$CORPUS_DIR"

# program_id,slug
PROGRAMS=(
    "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo,meteora"
    "MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD,marinade"
    "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4,jupiter"
)

decode_b58_to_bin() {
    python3 - "$1" <<'PY'
import sys
try:
    import base58
except ImportError:
    # Minimal base58 decoder so the script has zero pip deps.
    ALPHABET = b'123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
    def b58decode(s: bytes) -> bytes:
        n = 0
        for c in s:
            n = n * 58 + ALPHABET.index(c)
        full = n.to_bytes((n.bit_length() + 7) // 8, 'big')
        pad = 0
        for c in s:
            if c == ord('1'):
                pad += 1
            else:
                break
        return b'\x00' * pad + full
    class _B58:
        @staticmethod
        def b58decode(s):
            return b58decode(s.encode() if isinstance(s, str) else s)
    base58 = _B58()

sys.stdout.buffer.write(base58.b58decode(sys.argv[1]))
PY
}

fetch_program_signatures() {
    local program_id="$1"
    local limit="$2"
    curl -s "$RPC_URL" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getSignaturesForAddress\",\"params\":[\"$program_id\",{\"limit\":$limit}]}" |
        jq -r '.result[].signature'
}

fetch_transaction() {
    local sig="$1"
    curl -s "$RPC_URL" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$sig\",{\"encoding\":\"json\",\"maxSupportedTransactionVersion\":0}]}"
}

extract_program_ix_datas() {
    # stdin: full getTransaction JSON
    # stdout: one base58-encoded instruction data blob per line
    local program_id="$1"
    jq -r --arg pid "$program_id" '
        .result as $t
        | ($t.transaction.message.accountKeys // []) as $keys
        | ($t.transaction.message.instructions // []) as $top
        | (
            [$top[] | select(($keys[.programIdIndex] // "") == $pid) | .data]
            + [
                ($t.meta.innerInstructions // [])[].instructions[]
                | select(($keys[.programIdIndex] // "") == $pid)
                | .data
            ]
          )[]
    '
}

total=0
for entry in "${PROGRAMS[@]}"; do
    program_id="${entry%,*}"
    slug="${entry#*,}"

    echo ">>> fetching signatures for $slug ($program_id)"
    mapfile -t sigs < <(fetch_program_signatures "$program_id" "$PER_PROGRAM" || true)
    if [[ ${#sigs[@]} -eq 0 ]]; then
        echo "    (no signatures returned — RPC rate-limited or program inactive)"
        continue
    fi

    for sig in "${sigs[@]}"; do
        [[ -z "$sig" ]] && continue
        tx_json=$(fetch_transaction "$sig" || true)
        mapfile -t datas < <(printf '%s' "$tx_json" | extract_program_ix_datas "$program_id")
        ix_idx=0
        for d in "${datas[@]}"; do
            [[ -z "$d" ]] && continue
            out="$CORPUS_DIR/${slug}_${sig:0:16}_${ix_idx}.bin"
            decode_b58_to_bin "$d" >"$out" || continue
            total=$((total + 1))
            ix_idx=$((ix_idx + 1))
        done
        # courtesy throttle
        sleep 0.1
    done
done

echo
echo "Wrote $total seed inputs to $CORPUS_DIR"
if [[ $total -lt 50 ]]; then
    echo "Warning: Story 6-4 requires >= 50 seeds. Rerun with PER_PROGRAM=40 or retry if you hit rate limits."
    exit 1
fi
