#!/usr/bin/env bash
# Perf harness skeleton (architecture A10 budgets, Phase 0 deliverable).
#
# Times the operations that exist today; rows for later phases print
# NOT IMPLEMENTED until their phase lands (Phase 5 for search/reindex,
# Phase 6 for snapshots, Phase 1 for open/editor). Budgets are for a
# 500k-word project on a mid-range laptop.
#
# Usage: scripts/perf.sh [fixture-dir]
set -euo pipefail
cd "$(dirname "$0")/.."

FIXTURE="${1:-target/fixtures/oca-500k}"
BIN="target/release/oca"

now_ms() {
    if date +%s%3N 2>/dev/null | grep -qE '^[0-9]+$'; then
        date +%s%3N
    else
        python3 -c 'import time; print(time.time_ns() // 10**6)'
    fi
}

if [[ ! -x "$BIN" ]]; then
    echo "building release binary..."
    cargo build --release
fi

if [[ ! -f "$FIXTURE/fixture-manifest.json" ]]; then
    echo "generating fixture at $FIXTURE ..."
    python3 scripts/generate-fixture.py --out "$FIXTURE"
fi

WORDS=$(python3 -c "import json;print(json.load(open('$FIXTURE/fixture-manifest.json'))['actual_words'])")
echo "fixture: $FIXTURE ($WORDS words)"
echo
printf '%-34s %10s %10s %s\n' "OPERATION" "MEASURED" "BUDGET" "STATUS"

row() { # row <name> <measured_ms|-> <budget_ms|-> <status>
    printf '%-34s %10s %10s %s\n' "$1" "$2" "$3" "$4"
}

measure() { # measure <cmd...> -> ms, or - on failure
    local start end
    start=$(now_ms)
    if "$@" > /dev/null 2>&1; then
        end=$(now_ms)
        echo $((end - start))
    else
        echo "-"
    fi
}

# --- implemented in Phase 0 --------------------------------------------------
row "oca --version (startup)" "$(measure "$BIN" --version)" "n/a" "measured"
row "oca doctor" "$(measure "$BIN" doctor)" "n/a" "measured"

# --- budgets from architecture A10 (implemented by later phases) -------------
row "open project (cold)"      "-" "< 1000 ms" "NOT IMPLEMENTED (Phase 1)"
row "keystroke -> screen"      "-" "< 16 ms"   "NOT IMPLEMENTED (Phase 1)"
row "full-text search"         "-" "< 100 ms"  "NOT IMPLEMENTED (Phase 5)"
row "reindex single file"      "-" "< 200 ms"  "NOT IMPLEMENTED (Phase 5)"
row "full reindex"             "-" "reported"  "NOT IMPLEMENTED (Phase 5)"
row "snapshot create"          "-" "< 2000 ms" "NOT IMPLEMENTED (Phase 6)"

echo
echo "A10 budgets are enforced in CI on the generated corpus starting Phase 5."
