# ADR-0029: Music AI operations, deterministic lyric scores shared three ways

- Status: Accepted · Date: 2026-10-19 · Code: **A5, A8, A9** · AGENTS rules 3, 4, 6, 8
- Source: `OCA-architechture.md` Part II §23 A5/A8 · plan: `OCA-implementation.md` §3 Phase 10 · spec: `OCA-build.md` §7.2–7.4

## Decision

- **Eleven operations** live in `oca-ai/src/music_ops.rs`, reusing the Phase 9 pipeline unchanged (`prepare_*` → `complete` → `create_proposal`; `oca-ai` already depends on `oca-core`/`oca-music`/`oca-story` — no new crate, no Cargo change). Proposal ops: `song_structure`, `generate_section` (verse/chorus/bridge), `rewrite_lyrics`, `improve_rhyme`, `improve_meter`, `preserve_hook`, `preserve_rhyme_scheme`. Report ops (`DraftTarget::Report`, never proposals): `song_concept`, `alternatives`, `analyze_song`, `check_singability`.
- **Concept is a report, not a write** — `SongYaml` has no `concept:` field and identity calls (title, mood) stay in the author's hands; `song-concept` prints the description, title ideas, and themes, and the author copies what they like into `song.yaml` themselves. Same "nothing to fix must not produce an artifact" reasoning as review (§7.4).
- **The one metadata target**: `song_structure` lands in `song.yaml`'s `plan:` block through a proposal like everything else. A deterministic line-scan locator finds the top-level `plan:` line and extends over following indented/blank/comment lines (inline `plan: [a, b]` and block lists both); no plan yet → append at end of file. The rendered block keeps exactly one trailing newline so a middle-of-file replace never glues the payload onto the next key.
- **Deterministic scores, computed once and shared three ways** (`oca-music/src/scores.rs` — AGENTS: never reimplement syllable/rhyme math): `score_lines`/`rhyme_scheme`/`hook_line` feed (1) the prompts — `improve_meter` carries the per-line counts and the meter, `improve_rhyme`/`preserve_rhyme_scheme` carry the current scheme; (2) the TUI review, which shows both columns' scores beside a lyrics proposal; (3) the new `oca-eval` graders. The model never measures anything itself.
- **Honest refusals over fake numbers (A9)**: an unsupported language makes the scored operations fail with an actionable error ("reports unsupported rather than inventing numbers") instead of returning invented syllables. `preserve_rhyme_scheme` additionally refuses when no rhyme class repeats — an all-distinct scheme would be preserved vacuously — while `improve_rhyme` accepts both cases (strengthen the existing scheme, or introduce one where nothing rhymes yet).
- **The hook is `oca-music`'s hook**: the chord-stripped first line of the first chorus section. `preserve_hook` requires the selection to contain it; the `PreservesHook` grader compares chord-stripped, whitespace-collapsed, case-folded lines.
- **New graders** in `oca-eval`: `PreservesHook { hook }` and `PreservesRhymeScheme { scheme, language }` (via `score_lines`; both fail honestly on empty/vacuous inputs, both have negative tests). Fixtures gain `SONG_TWO_SECTIONS` + `song_project`.
- **`[ai] mock = true` now boots `MockModel::ai_defaults()`** — the story *and* music scripts, matched on the same `Operation: <name>` first prompt line. Each operation keeps its own prompt version (`improve-rhyme/1` …), so results stay comparable across prompt edits (A8).
- **Frontends stay thin (rule 8)**: CLI verbs are flat (`oca ai improve-rhyme <song> --start N --end N`, `… verse|chorus|bridge <song>`, reports as `oca ai analyze|singability|song-concept`); the TUI's AI panel switches its op keys on the open document kind — a song's `lyrics.md` gets the music keys, a chapter the story keys — and every music call goes through `oca-ai` prepare fns + Core commands.

## Consequences

- The Phase 10 exit flow (select chorus → improve rhyme → review scores both sides → accept → snapshot-restore undo) is identical machinery to Phase 9; Phase 11 (RAG) and later phases extend the same proposal UX rather than inventing apply paths.
- `Music::score_block` (resolve a song's language, score a text block) is the TUI's single scoring entry point; anything wanting rhyme/meter numbers calls `oca-music`.
- The mock's canned music outputs are line-count-sensitive: graders compare per-line schemes, so a canned rewrite must match the fixture's line count (the suite pins this).
