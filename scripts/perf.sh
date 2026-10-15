#!/usr/bin/env bash
# Perf harness (architecture A10 budgets).
#
# Times index + search against a generated 500k-word corpus and enforces
# the A10 budgets (Phase 5+ rows are real; the remaining NOT IMPLEMENTED
# rows light up when their phase lands — Phase 6 snapshots, interactive
# keystroke timing). Budgets default to A10's mid-range-laptop numbers;
# CI may raise them for slower runners via env.
#
# Usage: scripts/perf.sh [fixture-dir]
# Env:   BUDGET_OPEN_MS BUDGET_SEARCH_MS BUDGET_REINDEX1_MS
set -euo pipefail
cd "$(dirname "$0")/.."

FIXTURE="${1:-target/fixtures/oca-500k}"
BIN="target/release/oca"

# A10 budgets (ms).
BUDGET_OPEN_MS="${BUDGET_OPEN_MS:-1000}"
BUDGET_SEARCH_MS="${BUDGET_SEARCH_MS:-100}"
BUDGET_REINDEX1_MS="${BUDGET_REINDEX1_MS:-200}"

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

row() { # row <name> <measured_ms|-> <budget> <status>
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

failures=0
budget_row() { # budget_row <name> <budget_ms> <cmd...>
    local name="$1" budget="$2" ms status
    shift 2
    ms="$(measure "$@")"
    if [[ "$ms" == "-" ]]; then
        status="ERROR (command failed)"
        failures=$((failures + 1))
    elif (( ms <= budget )); then
        status="PASS"
    else
        status="FAIL ($ms ms)"
        failures=$((failures + 1))
    fi
    row "$name" "$ms" "< $budget ms" "$status"
}

# --- implemented in Phase 0 --------------------------------------------------
row "oca --version (startup)" "$(measure "$BIN" --version)" "n/a" "measured"
row "oca doctor" "$(measure "$BIN" doctor)" "n/a" "measured"

# --- Phase 5: build the index once so measurements aren't first-build cost ---
"$BIN" reindex --full --project "$FIXTURE" > /dev/null

# --- budgets from architecture A10 -------------------------------------------
# Cold open = project open + full mtime reconcile over the corpus + query.
budget_row "open project (cold, cache present)" "$BUDGET_OPEN_MS" \
    "$BIN" tags --project "$FIXTURE"
row "keystroke -> screen" "-" "< 16 ms" "NOT IMPLEMENTED (interactive)"
# One common term across the whole corpus (the fixture seeds it often).
budget_row "full-text search" "$BUDGET_SEARCH_MS" \
    "$BIN" search letter --limit 20 --project "$FIXTURE"
# Touch exactly one file: the next reconcile must reparse only that file.
one_file=$(find "$FIXTURE/manuscript" -name '*.md' -print -quit)
touch "$one_file"
budget_row "reindex single file" "$BUDGET_REINDEX1_MS" \
    "$BIN" reindex --project "$FIXTURE"
full_ms="$(measure "$BIN" reindex --full --project "$FIXTURE")"
row "full reindex" "$full_ms" "reported" "reported"
row "snapshot create" "-" "< 2000 ms" "NOT IMPLEMENTED (Phase 6)"

echo
if (( failures > 0 )); then
    echo "$failures A10 budget(s) exceeded or errored."
    exit 1
fi
echo "A10 budgets: all enforced rows pass on $WORDS words."
