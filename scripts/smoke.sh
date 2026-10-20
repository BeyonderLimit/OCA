#!/usr/bin/env bash
# Local build/package smoke test — the same gates CI runs, on one machine.
# Usage: scripts/smoke.sh
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== fmt =="
cargo fmt --all --check

echo "== clippy (-D warnings) =="
cargo clippy --workspace --all-targets -- -D warnings

echo "== phase gate: only this phase's crates exist =="
# No big-bang scaffolding: crates are created when their phase arrives
# (crate → phase table, OCA-implementation.md §2). Phases 0–12 = these +
# CLI (7B dictation and 12 continuity add no crate; 9 adds oca-eval;
# 11 adds oca-rag).
allowed="oca-ai oca-audio oca-cli oca-core oca-eval oca-markdown oca-model oca-music oca-rag oca-storage oca-story oca-tui oca-versioning"
for dir in crates/*/; do
    name=$(basename "$dir")
    case " $allowed " in
        *" $name "*) ;;
        *)
            echo "unexpected crate '$name' — create crates only when their phase arrives"
            exit 1
            ;;
    esac
done

echo "== phase gate: engines stay AI-free; later crates stay absent =="
# Rule 3 (AI optional): the deterministic engines from Phases 0–6 must not
# depend on oca-ai at all — deleting every model leaves them untouched.
# oca-cli/oca-tui are frontends: they hold the optional runtime behind the
# LanguageModel trait. oca-rag is a retrieval layer over the AI seam, so
# it depends on oca-ai — but no engine ever depends on it. Crates from
# later phases must not exist yet.
deps=$(cargo tree --workspace -e normal,build --prefix none | grep -Eo '^[a-zA-Z0-9_-]+' | sort -u)
for bad in oca-import oca-export llama whisper; do
    if printf '%s\n' "$deps" | grep -qx "$bad"; then
        echo "out-of-phase crate '$bad' found in the dependency tree"
        exit 1
    fi
done
for crate in oca-model oca-markdown oca-storage oca-story oca-music oca-audio oca-versioning oca-core; do
    if cargo tree -p "$crate" -e normal,build --prefix none \
        | grep -Eo '^[a-zA-Z0-9_-]+' | sort -u | grep -qx "oca-ai"; then
        echo "engine crate '$crate' depends on oca-ai — AI must stay optional (rule 3)"
        exit 1
    fi
done

echo "== test =="
cargo test --workspace

echo "== debug binary =="
cargo build
target/debug/oca --version
target/debug/oca --help > /dev/null
target/debug/oca doctor

echo "== release binary =="
cargo build --release
target/release/oca --version
target/release/oca doctor > /dev/null

echo "SMOKE OK"
