# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**Read `AGENTS.md` first** — it is the authoritative project brief: phase ledger (what's implemented), standing rules 2–8, locked decisions, and doc precedence. This file covers commands and the architectural big picture; it does not repeat the phase status.

## What this is

OCA (Offline Creative Assistant) — a local-first writing/music/audio workspace with optional offline AI. Design docs (`OCA-*.md`) + Cargo workspace under `crates/`. Phases 0–11 are implemented locally; nothing is pushed to a remote yet. New crates are created only when their phase arrives (workflow rule) — the next phase is 12 (story continuity engine).

## Commands

```sh
cargo build                                            # debug -> target/debug/oca
cargo test                                             # unit + CLI integration tests
cargo fmt --all --check                                # CI gate
cargo clippy --workspace --all-targets -- -D warnings  # CI gate
scripts/smoke.sh                                       # full local gate: fmt + clippy + tests + binaries + phase gates
scripts/perf.sh                                        # perf harness (A10 budgets)
python3 scripts/generate-fixture.py                    # 500k-word perf fixture -> target/fixtures/ (never commit fixtures)
```

Run a single test file or test:

```sh
cargo test -p oca-story                     # one crate's tests
cargo test -p oca-cli --test cli            # one integration test file
cargo test -p oca-story resolve_state       # tests matching a name substring
```

Notes:
- CLI end-to-end tests (`crates/oca-cli/tests/cli.rs`) run the real binary with an isolated `HOME`/cwd; they copy `examples/blackwood` to a temp dir and never write to it in place.
- AI-path tests always use mock models (`MockModel`, `MockEmbedder`, `FnTranscriber`) — never a real llama.cpp/whisper. The fake `llama-server` in oca-ai tests is the test binary self-invoking through a child-only env seam.
- Golden prompt tests: `crates/oca-ai/tests/golden/`, regenerate with `OCA_UPDATE_GOLDEN=1` and review the diff.
- The vocabulary-scan test (`crates/oca-cli/tests/vocabulary.rs`, rule 7: no Git vocabulary in user-facing strings) walks all crate sources — it will catch violations in code you add.

## Architecture

### Crate layering (dependency boundaries are enforced, not conventional)

```
oca-model      lossless YAML/frontmatter patches, ULID ids, format_version gate
oca-markdown   comrak parse -> blocks with sourcepos, byte-identical round-trip
oca-storage    .oca/ scaffold, atomic writes, recovery journal, SQLite cache DB (FTS5 + sqlite-vec)
oca-core       Command/Event API — the SOLE mutation path; proposal store; job system
oca-story      characters/states/timeline (deterministic story engine)
oca-music      song model, ChordPro, phonetics, rhyme/meter analysis, lyric scores
oca-audio      WAV capture, take sidecars, Transcriber seam (no oca-ai dep)
oca-versioning snapshot store (.oca/state/snapshots.git via git2, vendored)
oca-ai         LanguageModel + Embedder + Transcriber impls, context engine, ops, proposals
oca-rag        chunking, import, hybrid search (FTS5 ∥ kNN)
oca-eval       seeded fixtures + deterministic graders (no LLM-as-judge)
oca-tui        thin client of Core (rule 8: zero business logic)
oca-cli        clap frontend, e2e tests
```

Key boundaries (violations fail `scripts/smoke.sh` phase gates):
- **Engine crates (`oca-story`, `oca-music`, `oca-audio`, `oca-versioning`) must never depend on `oca-ai`** (rule 3: AI optional). `oca-rag` may dep `oca-ai`.
- **TUI and CLI never dep `oca-versioning`** — snapshot types are re-exported through `oca-core`.
- **All mutations go through `oca-core`'s `Command`/`Event` API**; AI output lands only via `Command::ProposalCreate|Apply|Reject` (rule 4: proposals, never direct writes; auto-snapshot before apply).

### Patterns that span files

- **`Project::open` / `Core`** (oca-core) is the entry point surfaces share; `oca-tui` tests drive `App`/`Editor` against a tempdir `Core`, not a pty.
- **Subprocess + seam pattern**: every external process (llama-server, whisper-cli) is spawned behind a trait (`LanguageModel`, `Embedder`, `Transcriber`) with a mock impl; the test binary can self-invoke as the fake child via `server_args`/`server_env`/`cli_args` seams and child-only env vars.
- **Lossless file editing**: never rewrite whole files on mutation — use `oca-model`'s `LosslessYaml`/`Frontmatter` patching so comments, unknown frontmatter keys, and untouched blocks survive byte-identical.
- **Files are canonical** (rule 2): SQLite (`.oca/cache/oca.db`) is a rebuildable index/cache only; `.oca/state/` is precious and never auto-deleted. Snapshot-tracked audio WAVs live in-tree, referenced by sha256 in `.oca/snapshot-manifest.json` — restore never touches recorded bytes.
- **Deterministic scores are shared, never reimplemented**: syllable/rhyme math lives in `oca-music::scores` (`score_lines`/`rhyme_scheme`/`hook_line`); prompts, TUI review, and `oca-eval` graders all call it.
- **AI operation pipeline** (Phase 9/10): `prepare_*` (main thread, `&Core`, deterministic) → `complete` (job thread, no `Core` handle, cancellable streaming) → `create_proposal` (`&mut Core`). The model never decides where text lands.
- **Reports vs proposals**: review/analysis ops (`review_chapter`, `analyze_song`, etc.) are printed, never written; proposals apply only through the stale-safe anchor/rebase rule.

### Config & locations

Layered config `~/.oca/config.toml` → project; `[ai]` section (incl. `mock = true` for offline canned-model runs). Models at `~/.oca/models/{llm,embeddings,whisper}`, runtime packs at `~/.oca/runtime/` — never inside the project, never committed. Links between documents use immutable frontmatter ids, never paths.

## Gotchas

- The architecture doc is spelled `OCA-architechture.md` (not "architecture").
- `OCA-build.md` supersedes the deleted `OCA-writer.md`/`OCA-music-build.md`; don't recreate old filenames cited in some docs.
- Stage explicit git paths; don't `git add -A`.
- `repository =` in the root `Cargo.toml` is a placeholder until the real release URL exists.
