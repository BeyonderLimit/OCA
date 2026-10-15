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
# (crate → phase table, OCA-implementation.md §2). Phases 0–6 = these + CLI.
allowed="oca-audio oca-cli oca-core oca-markdown oca-model oca-music oca-storage oca-story oca-tui oca-versioning"
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

echo "== phase gate: zero AI crates in the dependency tree =="
# Phases 0–6 must build with no AI anywhere (AGENTS rule 3 — AI optional).
# oca-music (Phase 3) is deterministic and allowed; the rest arrive later.
deps=$(cargo tree --workspace -e normal,build --prefix none | grep -Eo '^[a-zA-Z0-9_-]+' | sort -u)
for bad in oca-ai oca-rag oca-eval oca-import oca-export llama whisper; do
    if printf '%s\n' "$deps" | grep -qx "$bad"; then
        echo "out-of-phase crate '$bad' found in the dependency tree"
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
