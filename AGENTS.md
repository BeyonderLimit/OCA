# AGENTS.md

## What this repository is

- Design specification + Rust implementation of **OCA (Offline Creative Assistant)** — a local-first workspace for story + music + audio with optional offline AI. Five `OCA-*.md` design docs; a Cargo workspace under `crates/`.
- **Phases 0–4 are implemented locally**: full workspace (`oca-model`/`oca-markdown`/`oca-storage`/`oca-core`/`oca-story`/`oca-music`/`oca-audio`/`oca-tui`/`oca-cli`) builds, tests, and lints green (see Build & Test). Remaining exit items need a GitHub run (3-OS CI matrix, tag dry-run) — workflows exist but nothing is pushed yet (repo has one local commit, no remote).
- Git repo with one local commit and no remote; files sit at repo root. Some docs claim the repo root is `/Users/sam` with `git add OCA/...` — in this checkout `git rev-parse --show-toplevel` is this directory itself. Either way, stage explicit paths; don't `git add -A`.
- Filename quirk: the architecture doc is spelled `OCA-architechture.md`.

## Build & Test

```sh
cargo build                                # debug → target/debug/oca
cargo build --release                      # release → target/release/oca
cargo test                                 # unit + CLI integration tests
cargo fmt --all --check                    # CI gate
cargo clippy --workspace --all-targets -- -D warnings   # CI gate (clippy -D warnings)
scripts/smoke.sh                           # all of the above + run both binaries
scripts/perf.sh                            # perf harness skeleton (A10 budgets)
python3 scripts/generate-fixture.py        # 500k-word perf fixture → target/fixtures/oca-500k
dist plan                                  # what a release tag would build (cargo-dist)
```

## Doc map & precedence (highest authority first)

1. **Executable source** (code/config), once it exists, wins over all docs.
2. **Sequencing / phase order** → `OCA-roadmap.md`.
3. **Stack / crate structure** → `OCA-architechture.md` (Part II §23, amendments A1–A12, supersedes Part I wherever they conflict).

- `OCA-implementation.md` — merged executable plan: locked decisions (§1), crate→phase layout (§2), per-phase goal/work/exit criteria (§3), **guardrail checklist (§5)**, risks (§6).
- `OCA-build.md` — feature + file-format reference (project layout, chapter/song formats, AI operations). Supersedes `OCA-writer.md` and `OCA-music-build.md`, which no longer exist — don't recreate them.
- `OCA-amendments.md` — rationale/ADR source for amendments A1–A12 and decisions D1–D8 (status: applied).

Cross-reference codes: `A#` = architecture amendment, `D#` = accepted decision, `G#` = resolved gap. Indexes: `OCA-amendments.md` §2, `OCA-build.md` §16. Some docs still cite old filenames (`OCA-implementation-plan.md`, `OCA-writer.md`, `OCA-music-build.md`) — those were renamed/merged; don't recreate them.

## Standing rules

`OCA-implementation.md` cites these as "AGENTS rule N" — keep them in force. (Original numbering preserved to match citations; rules 1 and 5 are not cited anywhere and are unknown.)

- **Rule 2 — files are canonical.** Markdown/YAML on disk is the source of truth; SQLite is only an index/cache. `.oca/cache/` (holds `oca.db`) is deletable/rebuildable via `oca reindex`; `.oca/state/` (snapshots, AI history, recovery, lock) is precious — never auto-delete it or write cache into it.
- **Rule 3 — AI optional.** Phases 0–6 must work with every model file deleted; deleting models breaks nothing.
- **Rule 4 — proposals only.** AI never writes to project files directly. Applies go through a `Proposal` (with `base_hash` + `prompt_version`) after an auto-snapshot; stale proposals never apply silently.
- **Rule 6 — one LLM touchpoint.** Only `oca-ai`'s `trait LanguageModel` talks to llama.cpp; first impl is a subprocess (`llama-server`), not FFI; tests use a mock impl.
- **Rule 7 — vocabulary firewall.** No Git vocabulary ("commit", "HEAD", "hash") in user-facing strings; UX says "Snapshots" (a string-scanning test is planned).
- **Rule 8 — thin TUI.** `oca-tui` holds zero business logic; TUI/CLI are clients of the Core API (commands + events), enforced via crate dependency boundaries.
- **Workflow rule — no big-bang scaffolding.** Create each crate when its phase arrives, per the crate→phase table (`OCA-implementation.md` §2); CI should fail if a phase's crate appears early.

## Locked decisions easy to get wrong

- Product/binary name is **`oca`** (`oca new/open/doctor/reindex/backup/migrate/adopt`).
- Hidden dir is **`.oca/`** — older `.writer/` / `.studio/` names are superseded.
- **One** SQLite DB per project: `.oca/cache/oca.db` (FTS5 + vectors via `sqlite-vec` in the same file). No vector server, no Docker, no second `vectors.db`.
- Models live outside the project at `~/.oca/models/{llm,embeddings,whisper}`; never commit weights.
- Links use immutable frontmatter ids, never file paths; `oca://<project_id>/<id>` is reserved for post-v1 cross-project links.
- v1 scope: novel + short story; dictation = Phase 7B (in v1); album analysis = Phases 10 and 14 (in v1); screenplay (Fountain) = post-v1 Phase 20; MIDI/melody = post-v1 Phase 17; cross-project links and Tauri GUI = post-v1.
- Sequencing hard rule: **nothing in Phases 7–16 may start before Phase 6 exit criteria pass** (reversibility before automation). Phase 2 ∥ 3 and 9 ∥ 10 may parallelize.
- A phase is "done" only when its exit criteria pass in CI; re-verify the guardrail checklist (`OCA-implementation.md` §5) at each phase review.

## Testing quirks

- Tests: unit tests beside code; CLI end-to-end tests in `crates/oca-cli/tests/cli.rs` run the real binary with isolated `HOME`/cwd; TUI tests drive `App`/`Editor` directly with constructed key/mouse events against a tempdir `Core` (no pty required). The Rule-7 Git-vocabulary scan (`crates/oca-cli/tests/vocabulary.rs`) walks all crate sources for string literals + doc comments; it self-checks so it can't pass vacuously.
- All AI-path tests (Phase 7+) run against the mock `LanguageModel`, never a real model.
- AI evals (`oca-eval`, from Phase 9): seeded fixtures + deterministic graders; **no LLM-as-judge in CI**; golden prompt tests use a character-based token estimate.
- Planned CI tests worth remembering: kill-during-save crash test ✅ (`oca-storage/tests/crash.rs`), no-Git-vocabulary string scan ✅ (`oca-cli/tests/vocabulary.rs`), "delete DB → reindex → identical results", restore-reproduces-exact-state, perf budgets on a generated 500k-word corpus (A10).
- Fixture content is generated (`scripts/generate-fixture.py`, seeded) — never commit fixtures; `/target` and `/fixtures/` are gitignored. The one exception: committed sample projects under `examples/` (hand-authored, load-bearing for story-engine exit tests — `oca-story/tests/queries.rs` + the CLI e2e copy one to a temp dir and never write it in place).

## Current phase status (keep in sync)

- **Phase 0 — done locally, CI unverified:** workspace + `oca-cli` (`--version`/`--help`/`doctor`), layered config (`~/.oca/config.toml` → project), tracing (stderr + daily file under `.oca/logs/`), lint profile, `docs/adr/` (A1–A12 + D1–D8), fixture generator, perf skeleton, license (MIT OR Apache-2.0), cargo-dist release dry-run (`dist-workspace.toml`, `.github/workflows/release.yml`).
- **Phase 1 — done locally, CI unverified:** `oca-model` (lossless YAML/frontmatter patches, ULID ids, `format_version` read-only gate), `oca-markdown` (comrak parse → blocks with sourcepos, byte-identical round-trip), `oca-storage` (A1 scaffold, atomic writes, recovery journal, single-writer lock, registry, scan/index, kill-during-save crash test), `oca-core` (Command/Event API — sole mutation path, read-only rejection, `save_body`), `oca-tui` (tree + custom rope editor per `docs/adr/0021` — `tui-textarea` rejected for having no soft wrap), CLI `new/list/open/doctor/chapter new|append|show`. Exit criteria covered locally: create→write→save→reopen from CLI and TUI with byte-identical frontmatter, external rename still resolves by id, unknown frontmatter keys/comments survive edits, newer `format_version` → read-only, zero AI crates in the dependency tree (`scripts/smoke.sh` phase gates).
- **Phase 2 — done locally, CI unverified:** `oca-story` (characters/ entities with `aliases:`/`relationships:`, chapter-keyed `states:` with carry-forward resolution, word-boundary mention detection, `outline/timeline.md` events with story-time vs narrative position), `LosslessYaml::set_block`/`Frontmatter::get_value` for structured frontmatter writes (only the patched block is rewritten; comments/unknown keys survive), CLI `oca story who|where|chapters-with|before [--sense narrative|story]`, TUI "set state at chapter" form (`S` in the tree on a character, targeting the open chapter; all logic in `oca_story::set_state`). Exit criteria covered locally: all four example queries answered in integration tests over the committed `examples/blackwood` sample novel, flashback fixture answers "before 8" in both time senses (flash-forward in ch.6 / flashback in ch.10), alias fixture finds "Ellie" + "the girl" and rejects "Elenara", `set_state` round-trips losslessly, short-story form works (A12), zero AI in the dependency tree.
- **Phase 3 — done locally, CI unverified:** `oca-music` (song model `song.yaml` + `lyrics.md` with `## Heading` sections and `## Chorus 2 {=chorus}` references, ChordPro chords with transpose, phonetics pipeline: project `dictionary.yaml` → embedded `en.dict` → G2P rules, syllable counting, rhyme classification (perfect/multisyllabic/assonance/consonance/slant/internal), `check` report: plan drift + structure warnings + meter + repetitions + end rhymes, versions as byte-copies with optional labels in `versions/index.yaml`, artist/albums YAML), CLI `oca song new|check|save-version`, TUI song editor (`Alt-]`/`Alt-[` section jumps — plain brackets are chord syntax — and live syllable metrics in the editor title; all logic in `oca_music::cursor_metrics`/`section_marks`, rule 8). Exit criteria covered locally: sample song `examples/blackwood/music/songs/ashes` checks clean (drift-free, repeats resolve), invented name "Lyra" (user dictionary) rhymes multisyllabically with "fire", chord markers never count as syllables, `language: fr` says "unsupported" instead of producing numbers (A9), an offline-authored song round-trips through CLI (`song new` → write → `check` → `save-version`), zero AI in the dependency tree.
- **Phase 4 — done locally, CI unverified:** `oca-audio` (WAV capture via cpal → hound; takes are `<stem>.wav` + `<stem>.yaml` sidecar with immutable `take_<ULID>`, sha256 checksum for the A1 snapshot-by-reference contract, `attached:`/`note:` blocks; attachment targets (`chapter:<key>`/`song:<key>`/`project`) are validated through `oca-story`/`oca-music` and only ever written to the sidecar; lossless sidecar patching via `LosslessYaml`, incl. the new `remove_key` in `oca-model`; `AudioHost` device abstraction — cpal impl + deterministic `MockHost`; on Linux, probes refuse to touch ALSA unless `/dev/snd` has PCM nodes and hint enumeration is never used, so `doctor`/`record`/`play` keep stderr silent even on device-less machines), CLI `oca audio list|record|play|rename|delete|attach|detach|note` (`record --attach` validates its target before opening a device), `doctor` shows `✓ Audio subsystem — input ✓ <rate> × <ch> …`, TUI audio panel (`A` in tree; r/p/s/Enter/a/d/Esc; Esc while recording saves first; song lyrics open → `music/songs/<slug>/demos/` + song context; all logic in `oca-audio`, rule 8). Exit criteria covered locally: on-device record → save → attach → play round trip with **zero stderr bytes** (Linux), engine + TUI mock-host tests (record→pause→resume→save, attach to chapter and to song by id, play, delete, song-without-id rejected, read-only refused, `melody:` preserved), CLI e2e take lifecycle + error paths, no melody/transcription anywhere, zero AI in the dependency tree. macOS/Windows device verification is the manual `docs/audio-device-checklist.md`.
- **Open Phase 0 item:** `repository =` in `Cargo.toml` is the placeholder `https://github.com/oca-offline/oca` — replace with the real URL and run `dist generate` (see `docs/signing-and-release.md`).
- **Not started:** Phase 5 (`oca-db`/index into `oca.db`) may start now that Phase 4 is done locally; then 6 (reversibility — hard gate for Phases 7–16). Create crates when their phase arrives, never up front.
