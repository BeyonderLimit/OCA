# OCA — Implementation Plan

> **v2 (amended 2026-10-08):** incorporates `OCA-amendments.md` (decisions D1–D8 accepted). Architecture codes (A1–A12) refer to `OCA-architechture.md` Part II.

Merges `OCA-roadmap.md` (authoritative **order**) with `OCA-architechture.md` (authoritative **stack/structure**) into a single executable plan. When this plan and the source docs disagree on sequencing, the roadmap wins; on stack/structure, the architecture doc wins. Once code/config exists, the executable source wins over all docs.

---

## 1. Locked decisions

| Decision | Value | Source |
| --- | --- | --- |
| Language | Rust, Cargo workspace | architecture §1, §18 |
| Hidden project dir | **`.oca/`** with `cache/` (rebuildable `oca.db`) and `state/` (snapshots, AI history, recovery, lock — never auto-deleted) | user decision 2026-10-08; architecture A1 |
| Models dir | `~/.oca/models/{llm,embeddings,whisper}` | architecture §21 |
| Markdown on filesystem | source of truth; SQLite = index/metadata/cache only | architecture §6, AGENTS rule 2 |
| AI layer | optional acceleration; Phases 0–6 fully usable with zero AI | AGENTS rule 3 |
| AI writes | proposals only (accept/reject/edit), never silent writes | AGENTS rule 4 |
| llama.cpp | behind `LanguageModel` trait, **subprocess first** (not FFI) | architecture §9–10, AGENTS rule 6 |
| Vector store | embedded `sqlite-vec` in the project SQLite DB — no server/Docker | architecture §8 |
| Versioning | Git internally via `oca-versioning`, UX says "Snapshots" only | architecture §15, AGENTS rule 7 |
| TUI | Ratatui + Crossterm; **no business logic in TUI crate** | architecture §3, §19, AGENTS rule 8 |
| Binary / product name | **`oca`** (`oca new`, `oca open`, `oca doctor`, `oca reindex`, `oca backup`, `oca migrate`) | D7 |
| Identity & links | immutable ids in frontmatter; links by id; `oca://<project_id>/<id>` reserved for future cross-project links | architecture A2, D3 |
| Write safety | atomic save, recovery journal, single-writer lock, crash test in CI | architecture A3 |
| Comments | sidecar `comments/<doc_id>.yaml`, text-quote anchoring; zero-leak export | architecture A4, D1 |
| Core API | Commands + Events; `Proposal` objects with `base_hash` | architecture A5 |
| Heavy models | one resident at a time by default; job system | architecture A6 |
| Runtimes | optional Runtime Packs (llama-server, whisper) with SHA-256 manifests; llama-server loopback + API key; `sqlite-vec` statically linked (pending Phase 5 spike) | architecture A7, D8 |
| Songs | `lyrics.md` authoritative; `structure:` is an optional plan; ChordPro inline chords | architecture A11, D4, D5 |
| Snapshots vs versions | snapshots = automatic safety net (project-wide); versions = author-named drafts (per item) | architecture A11 |
| Scope | v1 = novel + short story; screenplay (Fountain) post-v1 Phase 20; dictation = Phase 7B in v1 | D2, D6 |
| Repo git gotcha | repo root is `/Users/sam`; stage explicit paths only (`git add OCA/...`) | AGENTS.md |

### Recommended stack picks (confirm, then lock)

| Layer | Recommendation | Notes |
| --- | --- | --- |
| SQLite binding | `rusqlite` + `bundled` feature (FTS5 enabled) | sync API fits a local app; `sqlx`/tokio not needed for SQLite |
| Vector ext | `sqlite-vec` loaded as an extension into the same DB | one DB per project |
| Markdown parser | `comrak` | GFM tables/task lists/HTML comments, AST with `sourcepos` for editor ranges; `pulldown-cmark` is the fallback pick |
| Frontmatter | `serde_yaml` (or `serde_yml`) + `serde` | |
| Snapshot backend | `git2` (vendored libgit2) | avoids requiring system `git` installed; behind a trait so it's swappable |
| CLI | `clap` (derive) | |
| Logging | `tracing` + `tracing-subscriber` | |
| Config | `serde` + TOML/YAML in project + `~/.oca/config` | |
| Audio | `cpal` (record) + `hound` (WAV) + `symphonia` (decode/playback) | |
| LLM runtime | `llama-server` subprocess, streaming over stdout/HTTP, cancellable | process isolation = crash isolation |
| Embeddings | separate small local embedding model via llama.cpp embedding mode | never the generative LLM (architecture §11) |
| Packaging | `cargo-dist` | Phase 18 |
| Future GUI | Tauri 2 + Svelte/TS — **explicitly out of v1** | |

### Decisions resolved by v2
1. **Binary name:** `oca`. (Roadmap examples updated.)
2. **Dictation:** Phase 7B, in v1.
3. **Cross-project links:** deferred; ids reserved.

### Open decisions (resolve at the phase that needs them)
1. **PDF generation approach:** HTML/CSS → PDF vs native renderer — decide in Phase 15.
2. **TUI editor widget:** `tui-textarea` vs custom rope-based buffer — **spike in Phase 1** against the checklist in architecture A9.
3. **Lossless frontmatter editing:** raw-text patching vs a lossless YAML library — decide in Phase 1 spike (A2).
4. **Repeat-section syntax in lyrics** — **resolved (Phase 3):** `## Chorus 2 {=chorus}` (`OCA-build.md` §4.3, `docs/adr/0011`).
5. **`sqlite-vec` static linking** — confirm in Phase 5 spike.

---

## 2. Workspace layout — created phase by phase

Do **not** scaffold all crates up front (AGENTS workflow rule). Each crate is introduced when its phase's foundation exists.

```
OCA/
├── Cargo.toml                  # workspace root          [Phase 0]
├── crates/
│   ├── oca-cli/                # thin CLI/TUI entry      [Phase 0]
│   ├── oca-core/               # Core API: commands/events/jobs [Phase 1]
│   ├── oca-model/              # shared domain types     [Phase 1]
│   ├── oca-storage/            # FS layout + SQLite      [Phase 1, DB in 5]
│   ├── oca-markdown/           # doc model + parser      [Phase 1]
│   ├── oca-tui/                # Ratatui frontend        [Phase 1]
│   ├── oca-story/              # story engine            [Phase 2]
│   ├── oca-music/              # music engine            [Phase 3]
│   ├── oca-audio/              # recording/playback; dictation [Phase 4, 7B]
│   ├── oca-versioning/         # snapshots/diff          [Phase 6]
│   ├── oca-ai/                 # LanguageModel + context [Phase 7–8]
│   ├── oca-rag/                # chunk/embed/vector/FTS  [Phase 11]
│   ├── oca-eval/               # seeded fixtures + graders [Phase 9]
│   ├── oca-import/             # EPUB/text import        [Phase 15]
│   └── oca-export/             # EPUB/PDF export         [Phase 15]
├── tests/                      # workspace integration tests
├── models/README.md            # model install docs (no weights in git)
├── docs/  examples/  scripts/
└── README.md
```

Dependency direction (enforce in CI with `cargo-deny` or similar):

```
oca-model
   ↑
oca-storage
   ↑
oca-core
   ↑
┌──┼──────────────┬─────────────┐
oca-story       oca-music     oca-ai ← oca-rag
   └───────┬─────────┴───────┬────────┘
      oca-audio        oca-versioning
           └────────┬────────┘
              oca-tui / oca-cli        (thin: zero business logic)
```

---

## 3. Phase-by-phase plan

Each phase: **goal → work → exit criteria**. A phase is not "done" until exit criteria pass in CI. Mapping to release numbers follows the roadmap's milestone table.

### Phase 0 — Development foundation → (included in 0.1)

**Goal:** build/test/package/run with zero AI.

- Workspace `Cargo.toml` with lint profile (`clippy -D warnings`, `rustfmt`).
- `oca-cli` skeleton: `--version`, `--help`, `doctor` (stubs returning ✓/⚠ per subsystem).
- Config system: layered (defaults → `~/.oca/config.toml` → project `.oca/config.toml`).
- `tracing` logging with `-v` flag and log file under `.oca/logs/`.
- CI: GitHub Actions matrix — Linux/macOS/Windows × (debug, release), running `fmt --check`, `clippy`, `cargo test`, plus build artifacts.
- `scripts/` for local build/package smoke tests.

**Exit criteria:** all Phase 0 deliverable checkboxes from the roadmap (build, unit tests, CI, 3 OS builds, debug+release binaries, logging, config) + `oca --version/--help/doctor` run on all three platforms.

**v2 amendments** (architecture Part II):

- `docs/adr/` directory: one short ADR per locked decision (this file's A1–A12 become ADRs).
- CI **release dry-run** with `cargo-dist` on every tag; signing process documented even if unsigned initially (A7).
- **Fixture generator:** synthesizes a 500k-word project (many chapters, entities, songs) for perf tests (A10).
- Perf harness skeleton; license decision for the repo.
- *Exit adds:* a dry-run release artifact builds on all 3 OSes.

### Phase 1 — Core project & Markdown engine → 0.1

**Goal:** a fully functional offline Markdown writing app, no AI.

- `oca-model`: `Project`, `Document`, `Section`, frontmatter types.
- `oca-storage`: project scaffolding — create/open/close/list; filesystem layout:
  ```
  my-novel/
  ├── project.yaml
  ├── manuscript/  characters/  world/  outline/  music/  notes/  audio/
  └── .oca/            # gitignored internals: cache/ (oca.db), state/ (snapshots, AI history), logs/
  ```
- `oca-markdown`: comrak-based parse → internal `Document` model (Frontmatter, Blocks: Heading/Paragraph/Quote/Image/Table/Code/Section, HTML-comment annotations) with source ranges for editing.
- `oca-core`: the **Core API** — define it before the TUI (architecture's "one decision to lock down"): project ops, document CRUD, save/load, event/callback surface. TUI and CLI are both clients.
- `oca-tui`: Project Tree (Manuscript/Characters/World/Outline/Music/Notes) + editor pane + status bar; save via Core API only.
- Decide the editor widget approach (open decision #4).

**Exit criteria:** `create project → create chapter → write → save → close → reopen → continue` works from both CLI and TUI; round-trip test proves Markdown + frontmatter survive byte-identically; zero AI crates in the dependency tree.

**v2 amendments** (architecture Part II):

- Layout v2 (A1), `project_id`, `format_version`, ids, id-based link resolution (A2).
- Safe-write, recovery journal, single-writer lock (A3).
- Command/Event core API skeleton (A5).
- Editor widget **spike** with checklist; Unicode handling (A9).
- Lossless frontmatter approach (A2).
- *Exit adds:* kill-during-save crash test passes; rename/move a file externally → links still resolve; unknown frontmatter keys + YAML comments survive an edit; open newer `format_version` → read-only.

### Phase 2 — Story engine → 0.2 + 0.3

**Goal:** novel-writing environment with deterministic state.

- `oca-story`: Story/Chapter/Scene hierarchy over `oca-markdown` docs; entity files for characters/locations/factions with YAML schema (id, name, role, state, relationships — per roadmap schema).
- Character **state engine**: chapter-indexed state transitions (`Chapter N → state → Chapter N+1`), computed deterministically from frontmatter/annotations — this is the substrate for later AI continuity.
- Timeline + outline structures.
- Query API answering: "Where is Elena?", "Who is Marcus?", "What chapters contain Elena?", "What happened before chapter 8?"

**Exit criteria:** all four example queries answered correctly in integration tests over an `examples/` sample novel; no AI involved.

**v2 amendments** (architecture Part II):

- `states:` + `aliases:` schema, carry-forward resolution, story-time vs narrative position (A11).
- TUI "set state at chapter" form.
- Short story as a supported form (A12).
- *Exit adds:* flashback fixture answers "what happened before chapter 8?" correctly in both time senses; alias fixture finds "Ellie" mentions.

### Phase 3 — Music engine → 0.4 *(can run parallel to Phase 2)*

**Goal:** Music Mode is first-class, before any AI.

- `oca-music`: `music/songs/<slug>/{song.yaml, lyrics.md, versions/, demos/, notes/}`, `albums/`, `artist.yaml`.
- Song section model: Intro/Verse/Pre-Chorus/Chorus/Post-Chorus/Bridge/Breakdown/Outro/Interlude.
- `song.yaml` schema: title, genre[], mood[], tempo, key, time_signature, themes[].
- **Deterministic lyric tools** (rule 5: deterministic beats LLM): syllable counting, line length, rhyme detection (CMU-phonetics-style dictionary, offline), repetition detection, section structure validation.
- TUI song editor with section navigation + lyric metrics in status bar.

**Exit criteria:** a complete song authored offline; syllable/rhyme tools covered by unit tests with known-answer fixtures.

**v2 amendments** (architecture Part II):

- `lyrics.md` authoritative + plan drift check; repeat-section reference syntax (A11).
- ChordPro chords: parse, display, transpose (A11).
- `PhoneticProvider` + `dictionary.yaml` + `language:` (A9).
- Albums model; `artist.yaml` schema.
- "Save as version" (copy + label) — compare/promote arrive in Phase 6.
- *Exit adds:* syllable count ignores chord markers; invented name with user pronunciation rhymes correctly; unsupported language shows "unsupported".

### Phase 4 — Audio foundation → 0.5

**Goal:** audio without AI.

- `oca-audio`: `cpal` capture → WAV (`hound`); record/pause/resume/stop/play/delete/rename; playback via `cpal` out (decode with `symphonia` as needed).
- Asset registry: audio files live in `audio/` or `song/demos/`; metadata rows link them to chapters/songs (DB comes in Phase 5 — until then, sidecar YAML or frontmatter refs).
- TUI: record control, take list, attach-to-chapter/attach-to-song.
- **No** melody extraction, no transcription.

**Exit criteria:** record → save → attach to song and to chapter → play back, on all 3 OSes (audio device handling is the main cross-platform risk).

**v2 amendments** (architecture Part II):

- Creative Note record (audio + optional text + tempo + note), with melodic fields reserved but unused (see `OCA-build.md §10`).
- Audio files are **excluded from snapshot Git history** (stored by reference + checksum) to avoid repo bloat (see architecture Part II, A1).

### Phase 5 — Search, tags & indexing → 0.6

**Goal:** infrastructure AI will depend on; usable at 500k words.

- `oca-storage` gains `.oca/cache/oca.db` via `rusqlite` (bundled, FTS5): schema from architecture §17 — `projects, documents, sections, characters, character_states, locations, relationships, timeline_events, songs, albums, song_sections, artist_profiles, tags, document_tags, annotations, snapshots, audio_assets, ai_operations, ai_generations` (+ FTS/embedding tables added in Phase 11).
- **Rebuildable index rule:** DB is derivable from the filesystem; ship `oca reindex` and test "delete DB → reindex → identical results".
- FTS5 full-text search across chapters/scenes/characters/world/songs/lyrics/notes/audio metadata; TUI `Ctrl-F`.
- Tags (`tag:character:elena` …), backlinks, references — manual/deterministic first.
- Migrate Phase 4 audio metadata sidecars into the DB.

**Exit criteria:** search returns correct cross-entity hits on the sample project; DB-loss recovery test passes; perf smoke test on a large generated corpus.

**v2 amendments** (architecture Part II):

- File watcher + reconcile loop; `oca reindex` tests (cache only).
- `sqlite-vec` static-link spike (A7).
- Migration framework for DB *and* file format (A2).
- *Exit adds:* perf budgets in A10 pass in CI on the generated corpus.

### Phase 6 — Versioning & diff → 0.7

**Goal:** every destructive operation reversible — hard architectural rule.

- `oca-versioning`: internal Git repo inside `.oca/` via `git2`; expose only `snapshot create / list / restore / diff`.
- **Vocabulary firewall** (rule 7): no "commit/HEAD/hash" in any user-facing string, TUI label, or error message — enforce with a test that scans TUI strings.
- Auto-snapshot hooks: before any AI proposal apply, restore-point, bulk edit, import.
- TUI: snapshot list, side-by-side/inline diff view.

**Exit criteria:** create → diff → restore round-trip; fuzz-style test that a restore reproduces exact prior filesystem state; vocabulary test green.

**v2 amendments** (architecture Part II):

- Snapshots in `.oca/state/snapshots.git`; audio tracked by checksum manifest, not blobs.
- Version compare/promote built on the same diff engine (A11).
- Conflict view for external-edit collisions (A2).

### Phase 7 — Runtime manager & llama.cpp → 0.8

**Goal:** local LLM behind an abstraction; nothing else changes.

- `oca-ai`: `trait LanguageModel { load, generate(streaming), cancel, unload }` + `ModelConfig` (context, temperature, top-p, max tokens, threads, GPU).
- First impl: **`LlamaCppSubprocess`** — model discovery in `~/.oca/models/llm`, spawn `llama-server`, stream tokens, cancel = kill/abort, crash isolation (TUI survives model crash).
- `doctor` now reports model runtime + installed models.
- TUI AI panel: model/context/GPU/threads, Load/Unload, streaming progress with `[ESC] Stop`.
- Mock `LanguageModel` for tests — all later phases test against the mock.

**Exit criteria:** streaming generation starts/stops cleanly 100/100 times; killing the model process does not crash OCA; `doctor` output matches roadmap mock.

**v2 amendments** (architecture Part II):

- Runtime packs + SHA manifests + side-load install (A7); loopback + API key.
- Model lifecycle manager and job system (A6).
- Hardware detection → tier recommendation in `doctor` (A8).
- Constrained-decoding support in `LanguageModel::generate` (schema/grammar parameter).
- **Packaging spike:** a signed/notarized runtime pack runs on each OS.


### Phase 7B — Dictation (new, v1) → 0.8

**Goal:** voice capture and transcription that works with no LLM installed.

- Mic → `cpal` → whisper transcription (works with **no LLM**) → raw transcript note.
- Options per capture: keep raw / clean transcription / turn into prose / add to notes / insert into chapter (cleanup options that need the LLM appear only when one is loaded).
- *Exit:* record → transcribe → insert works with only whisper installed; transcript never auto-inserts into the manuscript.
- Decision D2: dictation is in v1; nothing else depends on it, so it can slip without blocking Phase 8+.

### Phase 8 — AI context engine → (part of 0.9)

**Goal:** the phase deserving the most architectural effort — no feature talks to llama.cpp directly.

```
AI Feature → Context Engine → Retriever → Story/Music State → Prompt Builder → LanguageModel
```

- Explicit context types: `ManuscriptContext, ChapterContext, SceneContext, CharacterContext, WorldContext, SongContext, AlbumContext, ArtistContext`.
- Retriever pulls deterministic state (Phase 2/3 engines) + search results (Phase 5) into bounded token budgets; Prompt Builder assembles messages with provenance.
- Context objects are serializable and snapshot-testable (golden prompt tests).

**Exit criteria:** golden-file prompt tests for each context type; context size budget enforced with truncation policy; still only proposals — no writes.

**v2 amendments** (architecture Part II):

- Proposal model + store (A5) lands here so every AI phase uses it.
- Context manifests recorded on each proposal; token budgeting (A8); `prompt_version`.
- Untrusted-content delimiting for RAG/imported text (A8).

### Phase 9 — Story AI → 0.9

Operations, smallest first: **Continue scene → Rewrite selection → Summarize chapter → Generate outline → Review chapter.**
- Universal proposal UX: `AI Proposal | Accept / Reject / Edit`, side-by-side, applied only through Core API with an auto-snapshot (Phase 6) first.

**Exit criteria:** each of the 5 operations works end-to-end against the mock model in tests and the real model manually; accepting a proposal is undoable via snapshot restore.

**v2 amendments** (architecture Part II):

- `oca-eval` introduced with seeded fixtures (A8); first graders: schema validity, summary length, rewrite preserves named entities.
- Stale-proposal tests: edit during generation → rebase or stale, never silent overwrite.

### Phase 10 — Music AI → 0.10

- Operations: concept, structure, verse, chorus, bridge, rewrite, improve rhyme, improve meter, alternatives, analyze, singability — all reusing Phase 8 `SongContext`.
- Deterministic tools (Phase 3) validate/score AI output; AI proposes, deterministic tools grade.
- Same proposal UX beside the original.

**Exit criteria:** select-chorus → improve-rhyme flow works; proposals shown beside original; rhyme/meter scores computed deterministically for both.

**v2 amendments** (architecture Part II):

- Deterministic graders for `PRESERVE_HOOK` / `PRESERVE_RHYME_SCHEME` (hook still present, scheme unchanged).
- Album analysis operation (A12).
- Optional **originality check**: n-gram overlap of proposed lyrics against the user's own reference corpus, to flag accidental near-copies.

### Phase 11 — RAG → 0.11

- `oca-rag`: Import → Parse → Normalize → Chunk → Metadata → Embed → Vector index pipeline for Markdown/EPUB/text/project files/lyrics/notes.
- Add `documents_fts` (if not already) + `document_embeddings` tables; `sqlite-vec` loaded into the project DB.
- Separate embedding model (`~/.oca/models/embeddings`), incremental re-embed, embed-only-no-generative-model mode.
- Hybrid search: SQLite FTS ∥ vector search → reranker → context builder (feeds Phase 8 Retriever).

**Exit criteria:** import sample corpus → hybrid query returns ranked, deduped results; re-embed after edit is incremental; works with no LLM installed (embeddings-only).

**v2 amendments** (architecture Part II):

- Embedding-model lifecycle within the one-heavy-model rule (A6).
- Reference library import marks source/provenance; "untrusted" flag (A8).

### Phase 12 — Story continuity engine → 0.12

- `oca-story` + `oca-rag`: character/location/timeline/object/relationship/world-rule continuity.
- Issue report UX per roadmap: severity, chapter refs, `[View references] [Ignore] [Create note]` — deterministic state engine is the primary source, RAG the corroborator.

**Exit criteria:** seeded contradictions in a fixture novel are detected with correct severity and references; zero false "silent" fixes — reports only.

**v2 amendments** (architecture Part II):

- Requires Phase 2 `states:`; evaluation against seeded contradictions via `oca-eval`.

### Phase 13 — Song/Story relationship engine → 0.13

- Link model in `oca-model`: `song↔character, song↔chapter, song↔event, song↔theme, song↔album`, stored as relationships + tags in DB, human-editable in Markdown/YAML (source of truth).
- Context Engine composes cross-domain context (narrative + musical) for Phase 14.

**Exit criteria:** create/link/query a song↔chapter relationship from TUI; both a `SongContext` and cross-domain context render the linked material.

**v2 amendments** (architecture Part II):

- Links stored by id; `oca://` form reserved (A2).
- **Creative Timeline view** (story events + songs + album tracks on one axis) — the feature described in `OCA-music-build §13`, now with a home.

### Phase 14 — Full AI pipelines → 0.14

- REWRITE → REFINE → REVIEW pipelines sharing `oca-ai` infrastructure with per-domain evaluators (story: grammar/style, continuity, pacing; music: rhyme/meter, singability, theme consistency).
- Each stage emits a proposal; pipeline is resumable and cancellable; final review stage is a report, not an auto-apply.

**Exit criteria:** full chapter pipeline and full lyrics pipeline run stage-by-stage with human accept/reject at each stage; killing mid-pipeline loses nothing.

**v2 amendments** (architecture Part II):

- Album review pipeline (A12); resumable pipeline state stored in `state/`.

### Phase 15 — Import/export → 0.15

- `oca-import`: EPUB/text → parser → **OCA Document Model** → project (no EPUB-specific types leak into the manuscript model).
- `oca-export`: clean (typography, strip comments/internal metadata) → validate → Markdown / EPUB / PDF (decide PDF approach, open decision #3); lyric/lead-sheet export to PDF/MD/text; audio stays attached, not embedded.

**Exit criteria:** round-trip test: EPUB → project → EPUB is semantically identical; exported manuscript contains no annotations or internal metadata.

**v2 amendments** (architecture Part II):

- Lead sheet export from ChordPro data (A11).
- EPUB metadata: title/author/language/cover, TOC, front/back matter; export settings stored in `project.yaml: export:`.
- User HTML comments stripped by default; annotations never exported (A4).

### Phase 16 — Zen Mode & professional UX → 0.16

- Zen mode, centered cursor, focus dimming, Ghost HUD, dual-view comments, visual diff studio, command palette, configurable keybindings, themes, accessibility, autosave indicators — **all in `oca-tui`, calling Core API only.**

**Exit criteria:** command palette reaches every major operation; keybindings configurable; no business logic added to the TUI crate (enforced by crate dependency boundaries).

### Phase 17 — Advanced music (post-v1, optional)

MIDI import/export/playback, tempo map, chord progression, key/pitch detection, melody transcription, beat detection, waveform, lyrics↔timing. Explicitly **not v1**; `oca-audio` gains an analysis module without architecture changes.

### Phase 18 — Packaging & release → 1.0

- `cargo-dist`: Linux (AppImage, .deb, tar.gz), macOS (.app/.dmg), Windows (.exe/installer).
- Fully offline install; models are a separate optional package/download (`models/README.md` explains side-loading).
- `oca doctor` complete health report per roadmap.

**Exit criteria:** fresh machine, no network: install → create project → write → snapshot → (if models side-loaded) generate proposal. All green on 3 OSes.

**v2 amendments** (architecture Part II):

- Everything risky (signing, runtime packs, static sqlite-vec) was proven earlier; this phase wires final installers and runs the fresh-machine, no-network test.
- `oca backup` / `restore` verified.


### Phase 20 — Screenplay (Fountain) — post-v1, optional

Screenplay project form on Fountain format, reusing the story engine; format reserved via `project.yaml: kind`. Not in v1.

---

## 4. Sequencing & parallelization

Critical path: **0 → 1 → (2 ∥ 3) → 4 → 5 → 6 → 7 → (7B ∥ 8) → (9 ∥ 10) → 11 → 12 → 13 → 14 → 15 → 16 → 18**

- Phase 2 (story) and Phase 3 (music) are independent after Phase 1 — parallelize if two contributors.
- Phase 9/10 parallelize after Phase 8.
- Phase 15/16 can swap if needed; roadmap order stands as default.
- Nothing in Phases 7–16 may be started before Phase 6 exit criteria pass (reversibility before automation).

**Release mapping:** 0.1 = Phases 0–1 · 0.2/0.3 = Phase 2 · 0.4 = Phase 3 · 0.5 = Phase 4 · 0.6 = Phase 5 · 0.7 = Phase 6 · 0.8 = Phases 7 + 7B · 0.9 = Phases 8–9 · 0.10 = Phase 10 · 0.11 = Phase 11 · 0.12 = Phase 12 · 0.13 = Phase 13 · 0.14 = Phase 14 · 0.15 = Phase 15 · 0.16 = Phase 16 · 1.0 = Phase 18.

---

## 5. Guardrail checklist (re-verify at every phase review)

- [ ] No network required for any non-AI feature; no cloud dependency ever.
- [ ] Markdown/YAML filesystem remains the source of truth; `.oca/cache/` deletable + rebuildable; `.oca/state/` is backed up and never auto-cleaned.
- [ ] Phases 0–6 run with all model files deleted; deleting models breaks nothing.
- [ ] AI only ever produces proposals; every apply path goes through a snapshot first.
- [ ] Deterministic tools own formatting, syllables/rhyme, tags, dates; LLM only for meaning.
- [ ] `LanguageModel` is the only llama.cpp touchpoint; mock impl used in tests.
- [ ] No Git vocabulary in user-facing strings.
- [ ] `oca-tui` contains no business logic (dependency boundary enforced).
- [ ] No `git add -A` from `/Users/sam` — stage explicit `OCA/...` paths.
- [ ] Every file write is atomic; kill-during-save crash test is green.
- [ ] Every link uses ids, not paths; renames never break links.
- [ ] Every AI proposal carries `base_hash` and `prompt_version`; stale proposals never apply silently.
- [ ] Logs contain no manuscript text by default; no telemetry.
- [ ] Perf budgets (architecture A10) pass in CI on the generated corpus.
- [ ] Only one heavy model resident unless explicitly configured otherwise.
- [ ] Imported/RAG text is delimited as untrusted data in prompts.

---

## 6. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| Cross-platform audio (Phase 4) | abstract device selection early; CI can't test devices → manual checklist per OS; keep Phase 4 exit criteria honest |
| llama.cpp subprocess streaming/cancel flakiness (Phase 7) | process isolation + mock provider; cancellation test loop in CI (mock) |
| Scope creep from `OCA-writer.md`/`OCA-music-build.md` extra ideas | roadmap is authoritative for order; park extras in a backlog section of the roadmap, don't create phases |
| Dictation scope creep | lives in Phase 7B (needs runtime packs + jobs); do not absorb into Phase 4 |
| DB schema churn (Phase 5 → 11) | versioned migrations + rebuild-from-filesystem test from day one |
| Big-bang workspace scaffolding temptation | crate-per-phase table in §2; CI fails if a phase's crate appears early |
| TUI editor widget effort (Phase 1) | time-boxed spike against written checklist (A9); rope buffer + grapheme handling |
| Lossless frontmatter editing | raw-text patching; fallback = canonical format + snapshot before first rewrite (A2) |
| Per-OS runtime / GPU packaging and signing | release dry-run in CI from Phase 0; runtime-pack spike in Phase 7 (A7) |
| Small-model output quality on the Lite tier | tier table + `oca-eval` results in docs; gate heavy operations by tier (A8) |
| Audio bloating snapshot history | audio tracked by checksum manifest, not committed as blobs (A1) |

---

## 7. Immediate next steps

1. Doc sync is **done** (architecture Part II, roadmap deltas, this plan, `OCA-build.md`). `OCA-writer.md` and `OCA-music-build.md` are superseded by `OCA-build.md`; mark them as such or archive them.
2. Create `docs/adr/` and record A1–A12 + D1–D8 as ADRs (Phase 0).
3. Start **Phase 0**: workspace `Cargo.toml`, `oca-cli` skeleton with `--version/--help/doctor`, tracing + config, CI matrix with release dry-run, fixture generator, perf harness skeleton, lint profile. Stop when Phase 0 exit criteria pass.
