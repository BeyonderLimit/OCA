# OCA — Architecture & Roadmap Amendments (v2 proposal)

Resolves gaps G1–G15 from `OCA-build.md §16` and adds cross-cutting improvements found while fixing them.
**Status: APPLIED 2026-10-08.** D1–D8 (§5) were accepted as recommended and merged into `OCA-architechture.md` (Part II), `OCA-roadmap.md`, `OCA-implementation-plan.md` and `OCA-build.md`. This file remains as the rationale/ADR source.

Precedence rules from the implementation plan are unchanged (roadmap = order, architecture = stack, code wins once it exists).

---

## 1. Architecture amendments

### A1. Storage layout v2 — separate what can be rebuilt from what can't (fixes G1, G2)

Three classes of data, three locations:

| Class | Where | Rule |
| --- | --- | --- |
| **Creative content** (prose, lyrics, entities, versions, comments, audio) | Visible project dirs | Portable, human-readable, user-owned |
| **Irreplaceable state** (snapshots, AI proposal/generation history) | `.oca/state/` | Never auto-deleted; included in `oca backup` |
| **Derived cache** (index, FTS, vectors, thumbnails) | `.oca/cache/` | Always safe to delete; `oca reindex` rebuilds |

```
my-project/
├── project.yaml                  # includes project_id, format_version, language
├── manuscript/  characters/  world/  outline/
├── music/{songs,albums}/  music/artist.yaml
├── notes/  audio/  assets/
├── comments/                     # sidecar annotations, one file per document
├── dictionary.yaml               # user pronunciations/names (fantasy names, etc.)
└── .oca/
    ├── cache/   oca.db  thumbs/          # DELETABLE
    ├── state/   snapshots.git/  ai/*.jsonl  recovery/  lock   # PRECIOUS
    └── logs/
```

- AI generation history is written **append-only to `state/ai/*.jsonl` first**; the DB table `ai_generations` is just an index over it. This keeps "delete the DB → rebuild" true without losing history.
- `oca doctor` warns if `state/` is missing/corrupt; `oca reindex` never touches `state/`.
- `oca backup` / `oca restore`: one archive of project content + `state/` (excludes cache).
- Guardrail change: *"DB deletable + rebuildable"* becomes *"`.oca/cache/` deletable + rebuildable; `.oca/state/` is backed up and never auto-cleaned."*

### A2. Identity, links and external edits (fixes G3, G8, G15)

**IDs.** Every document/entity has an immutable `id` in frontmatter (ULID-style, e.g. `ch_01J9…`) generated at creation. `project.yaml` carries a `project_id`. Links reference IDs, never paths:

```
[[ch_01J9…]]  [[elena]] (resolves via id or alias)
oca://<project_id>/ch_01J9…        # fully-qualified form, reserved for future cross-project links
```

- Renaming/moving a file changes nothing. The index maps id → path.
- Files created externally with no `id`: indexed with a provisional path-hash id; `oca adopt` (or a TUI prompt) writes a real `id` after consent. OCA never edits a file it didn't need to.
- Duplicate ids (user copied a file) → `doctor` finds them and offers to reassign.

**External edits.** File watcher (`notify`) plus an mtime+hash check on every save:
- Disk changed, buffer clean → silently reload.
- Disk changed, buffer dirty → conflict view (keep mine / keep theirs / diff), with an auto-snapshot of both versions.

**Format versioning.** `format_version: 1` in `project.yaml`.
- Newer-than-supported → open read-only with a clear message.
- Older → `oca migrate --check` (dry-run), then migrate with an automatic snapshot first. Migrations are ordered `N → N+1` functions over files.
- **Unknown frontmatter keys and YAML comments must survive.** Re-serializing frontmatter with `serde_yaml` will reorder keys and drop comments, which breaks the Phase 1 "byte-identical round-trip" criterion. Rule: frontmatter is kept as raw text plus a parsed view; changes are applied as targeted patches to the original text. Verify a lossless-YAML approach in the Phase 1 spike; fall back to "canonical format + snapshot before first rewrite" if none exists.

**Scope of links in v1 (G8).** One project may be *mixed* (story + music in the same directory), so song↔chapter links are in-project and use ids. Cross-project links are **not supported in v1** but the `oca://<project_id>/…` form is reserved so adding them later is not a format break. `~/.oca/projects.toml` is a simple registry for `oca list`/recent projects. A shared Artist Bible across projects is a post-v1 "Universe" feature.

### A3. Write safety (fixes G4)

Data loss is the one unforgivable failure, so make it a first-class subsystem in `oca-storage`:

- **Atomic save:** write temp file in the same directory → `fsync` → rename over target (use the Windows-safe replace). Never write in place.
- **Recovery journal:** dirty editor buffers are journaled to `.oca/state/recovery/` every few seconds; on next open, offer to restore.
- **Single-writer lock:** `.oca/state/lock` (advisory, with PID + liveness check). A second instance opens read-only or tells the user.
- **Pre-flight snapshots** before any bulk edit, import, migration, restore or AI apply.
- **Crash test in CI:** kill the process at random points during save; the file is always either entirely old or entirely new.
- Windows/macOS filesystem care: case-insensitive collisions, reserved names, long paths, slug sanitization.

### A4. Annotations / comments (fixes G2)

Decision: **sidecar annotations in `comments/<document_id>.yaml`.** Visible and portable (travels with the project, not cache), yet never inside the manuscript text.

Anchoring uses the W3C text-quote pattern so edits don't orphan comments:

```yaml
- id: cm_01J…
  quote: "Something had moved behind her."
  prefix: "She stopped.\n\n"
  suffix: "\n\n## Scene"
  offset_hint: 412
  text: "Need stronger foreshadowing here."
  status: open
```

Re-anchor order: offset hint → exact quote → fuzzy prefix/suffix → **orphaned** (flagged in the UI, never deleted).

HTML comments a user types themselves (`<!-- … -->`) are preserved byte-for-byte as user content. Export strips them by default (configurable). This is a different mechanism from annotations, so the zero-leak guarantee covers both.

### A5. Core API as commands + events, with a Proposal model (fixes G10, supports G12)

Every mutation in `oca-core` is a **Command** that emits **Events**. Benefits: audit trail, undo, headless CLI/server mode, and a single choke point where "snapshot before apply" is enforced.

```
Frontend (TUI/CLI/GUI) ──Command──► oca-core ──► storage/versioning/ai
                         ◄──Event───
```

**Proposal** is a first-class object (stored in `state/ai/`):

```yaml
proposal:
  id, operation, prompt_version, model, created
  target: { document_id, range }
  base_hash: <hash of target text at generation time>
  context_manifest: [ids/ranges that were in the prompt]
  payload: <replacement text or structured result>
  status: pending | accepted | rejected | stale | edited
```

Apply rule: if `hash(current target) == base_hash` → apply (after snapshot). If the document changed but the target range is intact → rebase and apply. Otherwise mark `stale` and offer re-diff or regenerate. This lets the author keep typing while a generation runs.

### A6. Jobs and resource governor (new)

Indexing, embedding, generation and transcription are all long-running, cancellable and memory-hungry. Add a small job system in `oca-core`:

- Typed jobs with progress, cancel, and priority (user-interactive > background).
- **Model lifecycle manager:** by default only **one heavy model resident at a time** (LLM *or* embeddings *or* whisper), with explicit swap. Configurable for machines with spare RAM/VRAM.
- Background embedding yields to interactive generation.

### A7. Runtime packs and packaging reality (fixes G11)

- **Core binary** is self-contained: Rust app + bundled SQLite (FTS5). `sqlite-vec` should be statically linked and registered at startup so no loadable extension has to be shipped per platform (verify with the `sqlite-vec` crate in a Phase 5 spike).
- **Runtime Packs** are optional, per-platform/backend downloads: `llama-server` (CPU / Metal / CUDA / Vulkan builds) and whisper. Installed via `oca runtime install --from <file>`, so offline side-loading is first-class. Each pack and each model has a **SHA-256 manifest** verified by `doctor`.
- `llama-server` is launched bound to `127.0.0.1` on an ephemeral port with a random per-session API key, so other local processes can't use it.
- Signing and notarization (macOS notarization, Windows signing) are proven early, not at Phase 18: a **release dry-run** runs in CI from Phase 0 and a **runtime-pack spike** happens in Phase 7.
- Logs never contain manuscript text unless `-vv --log-content` is explicitly passed. No telemetry, ever.

### A8. AI reliability layer (fixes G9)

- **Hardware tiers, not model names** (model names churn): doctor detects RAM/VRAM and recommends a size class.

| Tier | Typical machine | Model size class |
| --- | --- | --- |
| Lite | 8 GB RAM, CPU only | ~3–4B params, 4-bit |
| Standard | 16 GB RAM or ~8 GB VRAM | ~7–9B, 4–5-bit |
| Comfortable | 32 GB+ RAM or ~24 GB VRAM | ~14–32B |

  Benchmark each tier on the fixtures below and publish measured tokens/sec and quality in `docs/`. Practical context is 8–32K tokens even if the model advertises more; retrieval, not stuffing, is the design.
- **Structured output via constrained decoding:** every operation with an output schema (continuity issues, tag suggestions, analysis) uses llama.cpp's JSON-schema/grammar support, with a parse → repair → retry fallback. Free-text operations (rewrite, continue) skip this.
- **Prompt versioning:** each operation has a `prompt_version`, recorded in every proposal, so results are comparable across changes.
- **Token budgeting:** use the server's tokenizer endpoint when a model is loaded; use a conservative character-based estimate when it isn't (golden prompt tests use the estimate).
- **`oca-eval` harness** (new crate, introduced Phase 9): fixture projects with *seeded* defects (contradictions, rhyme breaks, off-meter lines) and deterministic graders (schema validity, hook preserved?, syllable counts within tolerance, seeded issue found?). Run on demand/nightly against a pinned model; no LLM-as-judge in CI.
- **Imported/RAG text is untrusted data:** it is placed in clearly delimited context blocks and never treated as instructions. Because the AI can only create proposals, the blast radius is low, but this keeps imported documents from steering output.

### A9. Language and Unicode (fixes G13)

- `project.yaml: language: en` (default). Syllable/rhyme/typography rules are per language; for an unsupported language, the lyric tools say **"unsupported"** rather than showing wrong numbers.
- **`PhoneticProvider` trait:** English impl = offline pronunciation dictionary + rule-based grapheme-to-phoneme fallback for unknown words; others pluggable later.
- **`dictionary.yaml`** lets the author give pronunciations for invented names (`Blackwood`, `Elenor`), which matters for fantasy writers and songwriters.
- Editor text handling: rope buffer (`ropey`), grapheme clusters (`unicode-segmentation`), display width (`unicode-width`). IME works at the terminal level. RTL/bidi is a documented **v1 limitation**.
- Editor widget decision (open decision #4) becomes a **Phase 1 spike** with a written checklist: undo/redo, soft wrap, selection, clipboard, mouse, 50k-word file stays smooth, wide characters.

### A10. Performance budgets (new)

The roadmap says "500,000 words should remain usable." Make it measurable and test it in CI on a generated corpus:

| Operation | Budget (500k-word project, mid-range laptop) |
| --- | --- |
| Open project (cold, cache present) | < 1 s to interactive |
| Keystroke → screen | < 16 ms |
| Full-text search | < 100 ms |
| Reindex single saved file | < 200 ms |
| Full reindex | reported, regression-tracked |
| Snapshot create | < 2 s |

### A11. Domain-model decisions (fixes G5, G6, G7, G12)

**Character state (G5).** State is authored declaratively in the entity file, keyed by chapter id, with carry-forward semantics (the latest entry at or before chapter N applies):

```yaml
# characters/elena.md frontmatter
aliases: [Ellie, "the girl"]
states:
  - at: ch_001      # chapter id
    location: village
    emotional: isolated
  - at: ch_004
    location: blackwood
    emotional: frightened
```

Authoring aids: a TUI "set state at this chapter" form. Later phases let AI *propose* state entries from reading a chapter (`EXTRACT_CHARACTERS` / `UPDATE_STORY_STATE`), accepted by the author. Mention detection = word-boundary match on name + aliases (documented limit: pronouns aren't resolved). Timeline events distinguish **story time** from **narrative position** so flashbacks don't break "what happened before chapter 8?".

**Songs: one source of truth (G7).** `lyrics.md` headings are authoritative for order and content. `structure:` in `song.yaml` becomes an optional **plan**; `oca song check` reports drift between plan and actual. Repeated sections reference the original instead of copying text (exact syntax decided in Phase 3, e.g. a heading attribute like `## Chorus 2 {=chorus}`), so editing the chorus once updates every repeat.

**Chords (G6).** ChordPro-style inline chords in lyrics: `I [Dm]thought I [F]knew`. The parser extracts chord events with character offsets; the syllable engine strips them before counting. Phase 3 stores/displays/transposes; Phase 15 exports lead sheets from the same data.

**Versions vs snapshots (G12).** Two distinct concepts, one diff engine:

| | Snapshot | Version |
| --- | --- | --- |
| Purpose | Safety net | Author's named drafts |
| Scope | Whole project | One item (song, chapter) |
| Created | Automatically + manually | Only by the author ("Save as version") |
| Storage | `.oca/state/snapshots.git` | `versions/` files, visible |
| Verbs | create / list / diff / restore | save / compare / promote |

Promote replaces the working file only after an auto-snapshot. The versions mechanism is generic so chapters get "alternate drafts" too.

### A12. Scope fixes (fixes G14)

- v1 story forms: **novel and short story** (same hierarchy; a short story is a project with one chapter or a few scenes).
- **Screenplay is out of v1.** If wanted later, adopt Fountain format as a post-v1 phase. Remove it from diagrams.
- **Album-level analysis** (thematic/emotional progression, callbacks, opener/closer) gets an explicit home in Phase 10 (single-shot analysis) and Phase 14 (album review pipeline).

---

## 2. Gap resolution index

| Gap | Fix | Where |
| --- | --- | --- |
| G1 | `.oca/cache` vs `.oca/state`, AI history append-only in state | A1 |
| G2 | Sidecar `comments/` with text-quote anchoring; user HTML comments preserved | A4 |
| G3 | Immutable ids, id-based links, watcher + conflict view | A2 |
| G4 | Atomic save, recovery journal, lock, crash test | A3 |
| G5 | Declarative `states:` + aliases + story-time vs narrative position | A11 |
| G6 | ChordPro inline chords from Phase 3 | A11 |
| G7 | `lyrics.md` authoritative; `structure` is a plan; repeat references | A11 |
| G8 | Mixed projects in v1; reserved `oca://` form; registry; Universe post-v1 | A2 |
| G9 | Tiers, constrained decoding, prompt versions, `oca-eval` | A8 |
| G10 | Proposal model with `base_hash` and rebase/stale | A5 |
| G11 | Runtime packs, SHA manifests, loopback+key, early release dry-run | A7 |
| G12 | Snapshot vs version distinction | A11 |
| G13 | `PhoneticProvider`, `dictionary.yaml`, grapheme-aware editor | A9 |
| G14 | Novel + short story in v1; screenplay deferred | A12 |
| G15 | `format_version`, migrations, lossless frontmatter | A2 |

---

## 3. Revised roadmap

Phase order is unchanged except one new phase (7B) and one new post-v1 phase (20). Changes below are **additions** to each phase's work and exit criteria.

### Phase 0 — Foundation *(adds)*
- `docs/adr/` directory: one short ADR per locked decision (this file's A1–A12 become ADRs).
- CI **release dry-run** with `cargo-dist` on every tag; signing process documented even if unsigned initially (A7).
- **Fixture generator:** synthesizes a 500k-word project (many chapters, entities, songs) for perf tests (A10).
- Perf harness skeleton; license decision for the repo.
- *Exit adds:* a dry-run release artifact builds on all 3 OSes.

### Phase 1 — Core project & Markdown *(adds)*
- Layout v2 (A1), `project_id`, `format_version`, ids, id-based link resolution (A2).
- Safe-write, recovery journal, single-writer lock (A3).
- Command/Event core API skeleton (A5).
- Editor widget **spike** with checklist; Unicode handling (A9).
- Lossless frontmatter approach (A2).
- *Exit adds:* kill-during-save crash test passes; rename/move a file externally → links still resolve; unknown frontmatter keys + YAML comments survive an edit; open newer `format_version` → read-only.

### Phase 2 — Story engine *(adds)*
- `states:` + `aliases:` schema, carry-forward resolution, story-time vs narrative position (A11).
- TUI "set state at chapter" form.
- Short story as a supported form (A12).
- *Exit adds:* flashback fixture answers "what happened before chapter 8?" correctly in both time senses; alias fixture finds "Ellie" mentions.

### Phase 3 — Music engine *(adds)*
- `lyrics.md` authoritative + plan drift check; repeat-section reference syntax (A11).
- ChordPro chords: parse, display, transpose (A11).
- `PhoneticProvider` + `dictionary.yaml` + `language:` (A9).
- Albums model; `artist.yaml` schema.
- "Save as version" (copy + label) — compare/promote arrive in Phase 6.
- *Exit adds:* syllable count ignores chord markers; invented name with user pronunciation rhymes correctly; unsupported language shows "unsupported".

### Phase 4 — Audio *(adds)*
- Creative Note record (audio + optional text + tempo + note), with melodic fields reserved but unused (A: `OCA-build §10`).
- Audio files are **excluded from snapshot Git history** (stored by reference + checksum) to avoid repo bloat (see A1/§4 note below).

### Phase 5 — Search, tags, indexing *(adds)*
- File watcher + reconcile loop; `oca reindex` tests (cache only).
- `sqlite-vec` static-link spike (A7).
- Migration framework for DB *and* file format (A2).
- *Exit adds:* perf budgets in A10 pass in CI on the generated corpus.

### Phase 6 — Versioning & diff *(adds)*
- Snapshots in `.oca/state/snapshots.git`; audio tracked by checksum manifest, not blobs.
- Version compare/promote built on the same diff engine (A11).
- Conflict view for external-edit collisions (A2).

### Phase 7 — Runtime manager (was "llama.cpp integration") *(adds)*
- Runtime packs + SHA manifests + side-load install (A7); loopback + API key.
- Model lifecycle manager and job system (A6).
- Hardware detection → tier recommendation in `doctor` (A8).
- Constrained-decoding support in `LanguageModel::generate` (schema/grammar parameter).
- **Packaging spike:** a signed/notarized runtime pack runs on each OS.

### **Phase 7B — Dictation (new, pulled into v1)**
Why: it's one of the three pillars, and once Phase 7 gives you runtime packs and a job system it is cheap (whisper is another subprocess runtime).
- Mic → `cpal` → whisper transcription (works with **no LLM**) → raw transcript note.
- Options per capture: keep raw / clean transcription / turn into prose / add to notes / insert into chapter (cleanup options that need the LLM appear only when one is loaded).
- *Exit:* record → transcribe → insert works with only whisper installed; transcript never auto-inserts into the manuscript.
- If you'd rather keep v1 smaller, defer 7B to Phase 19 — nothing else depends on it.

### Phase 8 — Context engine *(adds)*
- Proposal model + store (A5) lands here so every AI phase uses it.
- Context manifests recorded on each proposal; token budgeting (A8); `prompt_version`.
- Untrusted-content delimiting for RAG/imported text (A8).

### Phase 9 — Story AI *(adds)*
- `oca-eval` introduced with seeded fixtures (A8); first graders: schema validity, summary length, rewrite preserves named entities.
- Stale-proposal tests: edit during generation → rebase or stale, never silent overwrite.

### Phase 10 — Music AI *(adds)*
- Deterministic graders for `PRESERVE_HOOK` / `PRESERVE_RHYME_SCHEME` (hook still present, scheme unchanged).
- Album analysis operation (A12).
- Optional **originality check**: n-gram overlap of proposed lyrics against the user's own reference corpus, to flag accidental near-copies.

### Phase 11 — RAG *(adds)*
- Embedding-model lifecycle within the one-heavy-model rule (A6).
- Reference library import marks source/provenance; "untrusted" flag (A8).

### Phase 12 — Continuity *(adds)*
- Requires Phase 2 `states:`; evaluation against seeded contradictions via `oca-eval`.

### Phase 13 — Song/Story relationships *(adds)*
- Links stored by id; `oca://` form reserved (A2).
- **Creative Timeline view** (story events + songs + album tracks on one axis) — the feature described in `OCA-music-build §13`, now with a home.

### Phase 14 — Pipelines *(adds)*
- Album review pipeline (A12); resumable pipeline state stored in `state/`.

### Phase 15 — Import/export *(adds)*
- Lead sheet export from ChordPro data (A11).
- EPUB metadata: title/author/language/cover, TOC, front/back matter; export settings stored in `project.yaml: export:`.
- User HTML comments stripped by default; annotations never exported (A4).

### Phase 16 — Zen & UX — unchanged (still no business logic in the TUI).
### Phase 17 — Advanced music — unchanged, post-v1.

### Phase 18 — Packaging & release *(now an assembly step)*
- Everything risky (signing, runtime packs, static sqlite-vec) was proven earlier; this phase wires final installers and runs the fresh-machine, no-network test.
- `oca backup` / `restore` verified.

### **Phase 20 — Screenplay (Fountain), post-v1, optional** (A12)

### Updated release map

| Release | Capability |
| --- | --- |
| 0.1 | Phases 0–1: TUI + Markdown + safe storage |
| 0.2 / 0.3 | Phase 2: story engine, characters, state |
| 0.4 | Phase 3: music, chords, lyric tools |
| 0.5 | Phase 4: audio + voice notes |
| 0.6 | Phase 5: search, tags, index, perf budgets |
| 0.7 | Phase 6: snapshots, versions, diff |
| 0.8 | Phase 7 + 7B: runtimes, llama.cpp, dictation |
| 0.9 | Phases 8–9: context engine, proposals, story AI |
| 0.10 | Phase 10: music AI |
| 0.11–0.16 | Phases 11–16 as in the roadmap |
| 1.0 | Phase 18 |

---

## 4. Additional notes

- **Audio and Git:** a Git-backed snapshot of WAV files will bloat quickly. Snapshots record audio by path + checksum (and the files themselves are never deleted by OCA without confirmation), instead of committing the blobs. Restore of a snapshot restores text/metadata and warns if referenced audio is missing.
- **New guardrails** to add to the checklist:
  - [ ] `.oca/state/` is never auto-deleted; only `.oca/cache/` is disposable.
  - [ ] Every file write is atomic; crash test green.
  - [ ] Every link uses ids, not paths.
  - [ ] Every AI proposal carries `base_hash` and `prompt_version`.
  - [ ] Logs contain no manuscript text by default.
  - [ ] Perf budgets pass in CI.
- **Risks added:** editor widget effort (Phase 1 spike), lossless frontmatter editing, per-OS runtime/GPU packaging, small-model output quality on the Lite tier (set expectations in docs; consider disabling some operations below a tier).

---

## 5. Decisions needed from you

| # | Decision | My recommendation |
| --- | --- | --- |
| D1 | Comments as sidecar `comments/*.yaml` (A4) | **Yes** |
| D2 | Pull dictation into v1 as Phase 7B vs defer to Phase 19 | **Pull in** (cheap after Phase 7) |
| D3 | One project may mix story + music; cross-project links deferred, `oca://` reserved | **Yes** |
| D4 | Songs: `lyrics.md` authoritative, `structure:` becomes a plan | **Yes** |
| D5 | ChordPro inline chords in lyrics from Phase 3 | **Yes** |
| D6 | Screenplay out of v1 (Fountain later) | **Yes** |
| D7 | Binary/product name: `oca` vs `creative-studio` | **`oca`** |
| D8 | Static-link `sqlite-vec`; runtime packs for llama/whisper | **Yes**, pending Phase 5 spike |

## 6. Doc edits to apply once decisions are confirmed

1. `OCA-architechture.md`: add §A1–A12 (layout v2, ids/links, write safety, annotations, command/proposal model, jobs, runtime packs, AI reliability, language, perf budgets); fix the table list in §17 to add `proposals`, `comments`, `versions`; add `oca-eval`.
2. `OCA-roadmap.md`: apply §3 deltas; add Phase 7B and Phase 20; replace `creative-studio`/`.studio/` with `oca`/`.oca/`.
3. `OCA-implementation-plan.md`: update Locked decisions (A1 layout, A2 ids, D1–D8), crate table (`oca-eval` in Phase 9), guardrail checklist (§4 above), risks.
4. `OCA-build.md`: replace §16 table with a pointer to this file; update §3 layout and §4.2/4.3 song format.
