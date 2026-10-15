# OCA — Offline Creative Assistant

Local-first creative workspace for stories, music, and audio, with optional offline AI.
**Status: Phase 0 (development foundation).** No AI, no network, and no cloud dependencies — ever.

Design docs (authoritative, see `AGENTS.md` for precedence):

- `OCA-architechture.md` — stack & structure (Part II supersedes Part I)
- `OCA-roadmap.md` — phase order
- `OCA-implementation.md` — executable plan, locked decisions, guardrails
- `OCA-build.md` — feature & file-format reference
- `OCA-amendments.md` — rationale / ADR source

## Development

```sh
cargo build                # debug binary -> target/debug/oca
cargo build --release      # release binary -> target/release/oca
cargo test                 # unit + integration tests
cargo fmt --all --check    # formatting (CI gate)
cargo clippy --workspace --all-targets -- -D warnings   # lints (CI gate)
scripts/smoke.sh           # full local gate: fmt + clippy + test + both binaries
```

```sh
oca --version
oca --help
oca doctor                 # subsystem health report
```

## Extras

- `scripts/generate-fixture.py` — synthesizes a ~500k-word project for perf tests (A10)
- `scripts/perf.sh` — perf harness skeleton (budgets in architecture A10)
- `docs/adr/` — one short ADR per locked decision (A1–A12, D1–D8)
- `docs/signing-and-release.md` — release dry-run & signing process
