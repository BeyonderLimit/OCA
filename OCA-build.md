# OCA — Build Spec (Story + Music)

Merges `OCA-writer.md` and `OCA-music-build.md` into one build reference (those two files are now superseded by this one).

**v2 (amended 2026-10-08):** incorporates `OCA-amendments.md` (D1–D8 accepted). A-codes refer to `OCA-architechture.md` Part II.

**Precedence (unchanged from the implementation plan):**
- Sequencing → `OCA-roadmap.md` wins.
- Stack / crate structure → `OCA-architechture.md` wins.
- Executable source (code/config), once it exists, wins over all docs.

**Doc-sync applied in this file:** `.writer/` and `.studio/` → `.oca/`. Single SQLite DB (vectors live in `oca.db`, not a separate `vectors.db`).

---

## 1. Principles

> **The project files are the source of truth. The AI is an assistant that operates around them, not the owner of them.**

1. **Markdown/YAML on disk is canonical.** SQLite is an index/cache. A user can open the project in Vim, VS Code, Obsidian, etc.
2. **AI-optional.** Delete every model and the app is still a complete writing and songwriting studio.
3. **AI proposes, the author decides.** No silent writes, ever. Every apply path snapshots first.
4. **Deterministic beats LLM** for anything checkable: typography, syllable counts, rhyme, tags, dates, state queries. The LLM handles *meaning*.
5. **Stories and songs share infrastructure but have different creative models and workflows.**
6. **No business logic in the TUI.** The TUI, CLI and future GUI are clients of the OCA Core API.

---

## 2. Layered architecture

```
┌─────────────────────────────────────────────────┐
│                    AUTHOR                       │
└────────────────────────┬────────────────────────┘
┌────────────────────────▼────────────────────────┐
│  CREATIVE LAYER (files)                         │
│  Story: chapters / scenes / comments            │
│  Music: songs / sections / albums / demos       │
└────────────────────────┬────────────────────────┘
┌────────────────────────▼────────────────────────┐
│  INTELLIGENCE LAYER (deterministic state)       │
│  Story Bible · Artist Bible · Timeline · Links  │
│  Syllable/rhyme engine · State engine           │
└────────────────────────┬────────────────────────┘
┌────────────────────────▼────────────────────────┐
│  AI LAYER (optional)                            │
│  Context Engine · llama.cpp · RAG · Pipelines   │
└─────────────────────────────────────────────────┘
```

```
                    CREATIVE PROJECT
                           │
          ┌────────────────┼────────────────┐
        STORY            MUSIC            MIXED
     ┌────┴────┐      ┌────┴────┐      ┌────┴────┐
   Novel     World   Song      Album  Story     Soundtrack
   Chapter  Character Verse    Track  Character Song
   Scene    Timeline  Chorus   Demo   Chapter   Theme
```

Shared services under both modes:

```
┌─────────────────────────────────────────────────┐
│                 SHARED SERVICES                 │
├─────────────────────────────────────────────────┤
│ Markdown / filesystem   │ SQLite (FTS5 + vec)   │
│ Tags / search           │ Versioning / snapshots│
│ llama.cpp (subprocess)  │ RAG                   │
│ Audio recording         │ Speech recognition    │
│ AI context engine       │ Import / export       │
└─────────────────────────────────────────────────┘
```

---

## 3. Unified project structure

```
my-project/
├── project.yaml                   # project_id, format_version, language, kind
│
├── manuscript/                    # STORY
│   ├── 01-the-beginning/
│   │   ├── 001-opening.md
│   │   └── 002-the-call.md
│   └── 02-the-journey/
│
├── characters/
│   ├── elena.md
│   └── supporting/
├── world/
│   ├── locations/  cultures/  history/
│   └── rules.md
├── outline/
│   ├── structure.md  acts.md  timeline.md
│
├── music/                         # MUSIC
│   ├── songs/
│   │   └── ashes/
│   │       ├── song.yaml
│   │       ├── lyrics.md
│   │       ├── versions/   v01.md v02.md ...
│   │       ├── demos/      chorus-002.wav ...
│   │       ├── notes/
│   │       └── artwork/
│   ├── albums/
│   └── artist.yaml                # Artist Bible
│
├── notes/
│   ├── ideas/  research/  voice-memos/
├── audio/
├── assets/images/
├── comments/                      # sidecar annotations, <doc_id>.yaml (A4)
├── dictionary.yaml                # user pronunciations (invented names)
│
└── .oca/
    ├── cache/                     # DELETABLE: oca.db (FTS5 + sqlite-vec + metadata), thumbs
    ├── state/                     # PRECIOUS: snapshots.git, ai/*.jsonl, recovery/, lock
    └── logs/
```

Models live outside the project: `~/.oca/models/{llm,embeddings,whisper}`.

---

## 4. Document formats

### 4.1 Story — chapter file

```markdown
---
id: ch-004
title: The Forest
chapter: 4
tags: [forest, discovery, protagonist]
status: draft
pov: elena
---

# The Forest

Elena stepped beneath the trees.

## Scene: The Clearing

...
```

Import convention: `#` → chapter, `##` → scene.

**Ids (A2):** every document/entity has an immutable `id` in frontmatter, generated at creation (ULID-style). Links reference ids, never paths, so files can be renamed or moved freely. Files created externally get a provisional id until the author runs `oca adopt`. Unknown frontmatter keys and YAML comments must survive edits.

### 4.2 Music — `song.yaml`

```yaml
type: song
id: song_ashes
language: en
title: Ashes
artist: ""
status: draft

genre: [alternative, cinematic]
mood: [melancholic, defiant]
tempo: 92
key: D minor
time_signature: 4/4

themes: [loss, rebirth, revenge]

# optional target plan; lyrics.md headings are authoritative (`oca song check` reports drift)
plan: [intro, verse, pre_chorus, chorus, verse, chorus, bridge, final_chorus, outro]

links:                          # ids, in-project (cross-project links are post-v1)
  chapters: [ch_012, ch_018, ch_027]
  characters: [elena]
```

### 4.3 Music — `lyrics.md`

The AI must see structure, not a text blob. Each section is a heading:

```markdown
# Ashes

## Verse 1
...
## Pre-Chorus
...
## Chorus
...
## Bridge
...
## Final Chorus
...
```

Supported sections: Intro, Verse, Pre-Chorus, Chorus, Post-Chorus, Bridge, Breakdown, Outro, Interlude. `Final Chorus` is an alias of the Chorus kind.

**`lyrics.md` is the single source truth** for order and content (A11). Repeated sections reference the original instead of copying text — **syntax (decided in Phase 3): `## Chorus 2 {=chorus}`**, so editing the chorus once updates every repeat. The reference token is the canonical name of the target section (lowercased, non-alphanumerics → `_`, trailing number stripped). A reference resolves to the nearest *earlier* non-reference section with the same canonical token; metrics reuse the referent's lines and drift compares canonical tokens 1:1. An unresolved reference is a structure warning, never a rewrite.

**Chords:** ChordPro-style inline chords, e.g. `I [Dm]thought I [F]knew`. The parser extracts chord events with offsets; syllable counting strips them first. Accidentals count toward pitch; transposition renders sharps deterministically (`Bbmaj7` −2 → `G#maj7`). Phase 3 stores/displays/transposes; Phase 15 exports lead sheets from the same data.

**Song versions (Phase 3):** `oca song save-version` copies `lyrics.md` byte-for-byte to `music/songs/<slug>/versions/vNN.md` and records an optional human label in `versions/index.yaml`. Listing shows every version file with labels overlaid (unlabeled versions included). Compare/promote arrive with Phase 6.

**TUI (Phase 3):** while `lyrics.md` is open, `Alt-]`/`Alt-[` jump to the next/previous section heading (plain brackets stay typeable — chords are `[Dm]`) and the editor title shows live syllable metrics for the cursor's line and section. All analysis lives in `oca-music` (rule 8).

This enables instructions like *"make verse 2 develop verse 1's idea without repeating its imagery"* and *"rewrite the chorus but preserve the hook."*

### 4.4 Shared internal Document model

```
Document
├── Frontmatter
├── Blocks (Heading, Paragraph, Quote, Image, Table, Code, Section)
└── Metadata

Story: Chapter → Scene → Document
Music: Song → Section → Document
```

### 4.5 Tags (hierarchical)

```
character:elena     location:blackwood   theme:betrayal
plot:foreshadowing  emotion:fear         status:draft
song:ashes
```

AI may *suggest* tags; the author accepts. **Never silently modify user metadata.**

### 4.6 Timeline (`outline/timeline.md`)

Frontmatter `events:` — each event carries an in-world **story time** and,
optionally, the chapter it is *shown* in (its **narrative position**). The two
clocks are kept apart so a flashback in a late chapter still counts as "before"
on the story clock, and a flash-forward shown early does not (A11):

```yaml
events:
  - id: ev_letter            # required, unique
    title: The letter is found
    story_time: '1888-03-01' # required; sortable string (ISO date or
                             # zero-padded label — compared lexicographically)
    chapter: ch_002          # chapter id where the event is shown (optional)
    characters: [elena, marcus]
    summary: One line for context.
```

Entries without `id` or `story_time` can't be placed on either clock and are
skipped with a warning. "What happened before chapter N?" is answered in either
sense: narrative = events shown in chapters before N (book order); story =
events whose `story_time` precedes chapter N's frontmatter `story_time:` anchor
(the target chapter must carry one). `oca story before` defaults to narrative.

### 4.7 Audio takes (sidecar, Phase 4)

A **take** is 16-bit PCM WAV plus a same-stem YAML sidecar — files are
canonical, so no database row is required until Phase 5 indexes them. Takes
live in `audio/`, or in `music/songs/<slug>/demos/` for song demos. A bare
`.wav` without a sidecar is ignored (never auto-adopted: the immutable id is
the link anchor).

```yaml
# audio/chorus-idea-7.yaml — fields after take_id are losslessly patchable
take_id: take_01JB…          # immutable; renaming touches files only
title: Chorus Idea #7
created: 2026-10-15T09:00:00Z # RFC 3339
sample_rate: 44100
channels: 2
duration_ms: 4200
checksum: sha256:…            # A1 snapshot-by-reference contract
attached:                     # optional — targets resolve via owning engine
  kind: song                  # chapter | song | project
  id: song_ashes
note:                         # optional — Creative Note (§10)
  text: Needs to feel bigger on the last line.
  lyrics: "I thought I knew…"
  tempo: 92
  melody: {}                  # reserved for future AI; v1 preserves, never writes
```

Attachment is stored **only** in the sidecar — the chapter/song files are
never modified — and the id is validated through `oca-story`/`oca-music` at
read time, so `oca audio attach take song:no-such-song` fails loudly instead
of leaving a phantom link. `oca audio attach take chapter:8` resolves a
chapter by number/id/title/path like every other story command.

---

## 5. Knowledge layer

### 5.1 Story Bible

```
STORY
 ├── Characters  (relationships, motivations, secrets, status)
 ├── World       (locations, factions, history, rules)
 └── Plot        (events, conflicts, reveals, arcs)
        └── Timeline Engine → Chapter/Scene → Manuscript
```

Character state example:

```yaml
character: Elena
age: 27
current_state:
  location: Blackwood Forest
  emotional_state: frightened
  knows: [father disappeared]
  doesnt_know: [father is alive]
  relationship: { Marcus: distrustful }
arc:
  starting_state: isolated
  current_state: suspicious
  target_state: trusting
```

State is authored declaratively in the entity file, keyed by chapter id, with carry-forward semantics (the latest entry at or before chapter N applies; fields carry forward individually, so a later entry that sets only `location` keeps the earlier `emotional`). `name:` falls back to `title:`, then the filename stem. The TUI's "set state at this chapter" form (and `oca_story::set_state`) upserts an entry by chapter id, merging its fields, patching only the `states:` block — comments and unknown keys around it survive:

```yaml
name: Elena
aliases: [Ellie, "the girl"]
states:
  - at: ch_001
    location: village
    emotional: isolated
  - at: ch_004
    location: blackwood
    emotional: frightened
```

Mention detection = word-boundary match on name + aliases (pronouns are not resolved — documented limit). Timeline events carry both **story time** and **narrative position** so flashbacks don't break "what happened before chapter 8?". Later AI phases may *propose* state entries; the author accepts. State is the substrate for continuity checks, so the AI never has to re-read 300,000 words.

### 5.2 Artist Bible (`artist.yaml`)

```
Identity:         artist name, genres, influences, vocal style, lyrical themes, forbidden clichés
Musical vocab:    common tempos, preferred keys, instrumentation, song structures, production prefs
Lyrical identity: imagery, vocabulary, rhyme preferences, perspective, recurring motifs
```

### 5.3 Albums

```
Album
├── Concept / Narrative
└── Tracklist: 01 Intro · 02 Song · 03 Song · 04 Interlude · ... · 07 Outro
```

Album-level analysis (the music equivalent of chapter review): thematic progression, emotional progression, repeated imagery, lyrical callbacks, narrative continuity, pacing, opener/closer strength.

### 5.4 Story ↔ Music links and Creative Timeline

Links: `song ↔ character`, `song ↔ chapter`, `song ↔ event`, `song ↔ theme`, `song ↔ album`. Stored as relationships/tags in the DB, **authored in Markdown/YAML** (source of truth).

```
2026
├── Story: Elena's journey
├── Song: "Ashes"     → represents Chapter 18
├── Song: "Blackwood" → represents Chapter 24
└── Album: "The Long Night"
       ├── Track 1 → beginning
       ├── Track 4 → conflict
       └── Track 9 → resolution
```

Enables: *"Write a song for Elena's emotional state after chapter 18"* and *"Write lyrics that foreshadow the chapter 27 revelation."*

---

## 6. Deterministic engines (no LLM)

### 6.1 Syllable / meter engine

```
Lyrics → sentence segmentation → syllable estimation → stress estimation → meter analysis → (optional) LLM interpretation
```

Output example:

```
Line 1: 8 syllables
Line 2: 8 syllables
Line 3: 7 syllables ⚠
Line 4: 8 syllables
```

**Pronunciation (Phase 3, A9):** each word is pronounced by the first match of — the project's `dictionary.yaml` (invented names, fantasy words), the embedded English dictionary (`oca-music/src/data/en.dict`), then grapheme-to-phoneme rules. Syllables = vowel phonemes; chord markers never count.

`dictionary.yaml` sits at the project root — a flat word → ARPAbet map (stressed vowels carry `0`/`1`/`2`):

```yaml
Blackwood: B L AE1 K W UH2 D
Lyra: L AY1 ER0
Elenor: EH1 L AH0 N AO2 R
```

Meta keys (`language`, `type`, `format_version`) belong in `project.yaml` and are rejected here. Invalid phonemes fail loudly at load, never silently at lookup. An unsupported `song.yaml` language reports *unsupported* — never invented numbers.

### 6.2 Rhyme engine

Offline rhyme dictionary; distinguish **perfect, slant, assonance, consonance, internal, multisyllabic**.
AI use: *"20 rhymes for this line that aren't cliché"* — the dictionary supplies candidates, the AI ranks/filters.

**Implemented taxonomy (Phase 3):** the rhyme key runs from the last primary-stressed vowel (fallback: the last vowel) to the word end. Kinds are disjoint, checked in this order — **Multisyllabic** (keys equal, ≥2 vowels) → **Perfect** (keys equal) → **Assonance** (last vowel equal, codas differ) → **Consonance** (codas equal and non-empty, last vowels differ) → **Slant** (same-length keys, one phoneme different). Internal rhymes reuse the same classifier over within-line word pairs.

### 6.3 Other deterministic tools

- Repetition detection, section-structure validation, line length.
- **Typography cleanup** (configurable): smart quotes, em-dash (`--`→`—`), ellipsis, collapse spaces, preserve code blocks.
- Story state queries: *Where is Elena? Who is Marcus? Which chapters contain Elena? What happened before chapter 8?*

### 6.4 Search, tags, and backlinks (Phase 5)

All served by the cache DB (`.oca/cache/oca.db` — derived, rebuildable with
`oca reindex`; ADR-0023). No AI involved.

**Indexed units** — one FTS5 table spans every entity: chapters and world
docs (title + body), character/location/timeline files (frontmatter text),
song metadata + lyric sections (`kind: song`/`lyrics`), timeline events
(title + summary), and audio take sidecars (`kind: audio`). Author input is
never interpreted as FTS syntax: every term is quoted, the last is
prefix-matched, and `tag:` routes to a tag lookup.

- `oca search <words> [--limit N]` — cross-entity hits with kind, title,
  path, and a highlighted snippet. Deleting the DB loses nothing: the next
  open (or search) rebuilds from the files.
- `oca tag add|remove|list <doc> <ns:value>` — patches the `tags:` block
  losslessly (comments/unknown keys survive; removing the last tag drops
  the block). Tags are `namespace:value` or a bare word; `oca tags` lists
  them project-wide with counts.
- `oca links <id-or-title>` — a document's outgoing references and its
  backlinks, resolved by immutable id (never path).
- `oca reindex [--full]` — incremental reconcile (mtime walk) by default;
  `--full` rebuilds from scratch.
- `oca migrate` — the file-format + DB-schema migration entry point
  (no-op when current).

The TUI reaches the same engine through **Ctrl-F**: live query, ↑↓ to
select, Enter opens the hit. External edits made outside the session
surface within seconds (polling reconcile on the idle tick) and on
open/save.

---

## 7. AI layer

### 7.1 Context Engine (the most important AI component)

```
AI Feature → Intent detection → Context Engine → Retriever → Story/Music State → Prompt Builder → LanguageModel
```

Context types: `ManuscriptContext, ChapterContext, SceneContext, CharacterContext, WorldContext, SongContext, AlbumContext, ArtistContext`.

- Story context draws on: current chapter/scene, outline, character state, world rules, timeline, RAG chunks, previous AI decisions.
- `SongContext`: current lyrics, previous versions, Artist Bible, song metadata, related songs, themes, associated story material.

### 7.2 Explicit operations (not generic prompts)

Each operation has its own structured prompt **and output schema**.

| Domain | Operations |
| --- | --- |
| Story | `GENERATE_OUTLINE`, `DRAFT_SCENE`, `CONTINUE_SCENE`, `REWRITE_SELECTION`, `CHECK_CONTINUITY`, `CHECK_GRAMMAR`, `CHECK_PACING`, `REVIEW_CHAPTER`, `EXTRACT_CHARACTERS`, `UPDATE_STORY_STATE`, `GENERATE_SUMMARY`, `GENERATE_TAGS` |
| Music: create | `GENERATE_SONG_CONCEPT`, `GENERATE_SONG_STRUCTURE`, `GENERATE_LYRICS`, `GENERATE_VERSE`, `GENERATE_CHORUS`, `GENERATE_BRIDGE`, `GENERATE_HOOK` |
| Music: edit | `REWRITE_LYRICS`, `IMPROVE_RHYME`, `IMPROVE_METER`, `IMPROVE_SYLLABLE_FLOW`, `GENERATE_ALTERNATE_LINES`, `PRESERVE_HOOK`, `PRESERVE_RHYME_SCHEME` |
| Music: analyze | `ANALYZE_THEME/STRUCTURE/RHYME/SYLLABLES/METER/HOOK/REPETITION`, `CHECK_SINGABILITY` |

`CHECK_SINGABILITY` matters: something can be excellent poetry and terrible lyrics.

Example schema (`CHECK_CONTINUITY`):

```yaml
issues:
  - type: timeline
    severity: warning
    location: paragraph 17
    explanation: ...
  - type: character
    severity: error
    location: paragraph 32
    explanation: ...
```

### 7.3 Proposal UX (universal)

```
Current Draft ── AI Proposed Revision ── Diff ── [Accept] [Reject] [Edit] [Accept selection]
```

For lyrics, show the proposal **beside the original**, with deterministic rhyme/meter scores for both. Applying a proposal goes through the Core API after an auto-snapshot.

### 7.4 Rewrite → Refine → Review pipelines

```
STORY:  Chapter → Rewrite → Grammar/style → Continuity → Pacing → Review
MUSIC:  Lyrics  → Rewrite → Rhyme/meter  → Singability → Theme consistency → Review
```

Shared infrastructure, different evaluators. Each stage emits a proposal; the pipeline is resumable and cancellable; the final Review is a report, not an auto-apply.

### 7.5 `LanguageModel` abstraction

```rust
trait LanguageModel {                    // every method &self — interior state,
    fn info(&self) -> ModelInfo;         // so cancel() works from any thread
    fn is_loaded(&self) -> bool;
    fn load(&self) -> Result<()>;        // idempotent; config is per-instance
    fn generate(                         // streaming (Phase 7 form)
        &self,
        request: &GenerationRequest,     // prompt/system + optional schema
        sink: &mut dyn FnMut(&str),      // one token at a time
    ) -> Result<GenerationOutcome>;      // text + tokens + cancelled flag
    fn cancel(&self);                    // interrupt the stream, runtime stays up
    fn unload(&self) -> Result<()>;      // idempotent; releases the process
}
```

Impls: `LlamaCppSubprocess` (first — loopback server, per-session API key,
kill on unload, clean error on crash), mock (all tests), future providers.
Generation must be interruptible (`[ESC] Stop`). Runtime packs (`oca runtime
install --from <pack.tar.gz>`, SHA-256 verified) and the one-heavy-model
governor live in the same crate (ADR-0025).

---

## 8. RAG

Two retrieval layers, combined:

```
Query ─┬─► SQLite FTS5 (lexical)  ─┐
       └─► sqlite-vec (semantic)  ─┴─► rerank ─► context builder
```

Score = semantic similarity + keyword score + chapter relevance + character relevance + recency + explicit importance.

Separate embedding model (never the generative LLM). Knowledge domains:

```
KNOWLEDGE BASE
├── Narrative: characters, locations, events, timeline, themes
├── Music:     lyrics, songs, chords, structures, concepts
└── Reference: documents, books, notes, research
```

A music query might retrieve: current song, earlier chorus drafts, prior songs on the same theme, the Artist Bible, character emotional state, associated chapters.

---

## 9. Versioning

- Internal Git (`git2`) at `.oca/state/snapshots.git`; UX vocabulary is **Snapshots** only (no commit/HEAD/hash in any user-facing string). Audio is tracked by checksum manifest, not committed as blobs.
- Auto-snapshot before: AI proposal apply, restore, bulk edit, import.
- Diff granularity: chapter → scene → paragraph → sentence → word, with restore at each level. v0.7 ships the base of this: whole-file restore plus line-level unified diffs (finer-grained views are a rendering concern over the same engine).
- **Versions** are different from snapshots (A11): snapshots are an automatic project-wide safety net; versions are author-named drafts of one item (`versions/v01.md…`) with save / compare / promote. Promote snapshots first. The mechanism is generic, so chapters get alternate drafts too.

```
Snapshot #142 — Chapter 12 · 3,421 words · "Before AI rewrite" · +143 / −87
```

---

## 10. Audio

```
                 AUDIO INPUT
        ┌───────────┼───────────┐
    Dictation     Melody       Beat
        ▼           ▼           ▼
  transcription  pitch/MIDI  tempo/rhythm
        └───────────┼───────────┘
                    ▼
              CREATIVE NOTE
```

**Dictation must not auto-become polished prose.** Offer: `[Keep raw] [Clean transcription] [Turn into prose] [Add to notes] [Insert into chapter]`.

Pipeline: mic → `cpal` → WAV → `whisper.cpp` → raw transcript → optional LLM cleanup (preserve meaning, remove filler, detect paragraphs) → Markdown.

Voice notes attach to chapters, songs, or the project. A "Creative Note" record:

```
Chorus Idea #7
Audio: chorus_007.wav
Lyrics: "I thought I knew..."
Melodic data: [optional MIDI/pitch — future]
Tempo: ~92 BPM
Notes: "Needs to feel bigger on the last line."
```

Design the data model now so melody/MIDI fields can be added later without migration pain (v1 does **no** melody extraction).

**What v1 ships (Phase 4):** the take pipeline in §4.7 — record, pause/resume, save, list, play back, attach to chapter/song/project, rename, delete — with takes stored by WAV + sidecar and attachment validated through the owning engines. `note.text`/`note.lyrics`/`note.tempo` are editable; `note.melody` is preserved losslessly but writable only by future AI operations.

**What v1 ships (Phase 7B, dictation):** `oca dictate <take>` turns a take into a raw transcript through a side-loaded `whisper-cli` + `~/.oca/models/whisper` model — no LLM required — stored in the take's sidecar (`transcript:`) and searchable through the Phase 5 index. The five options above are all explicit author actions: keep raw is the default, `--into notes|chapter:<key>` / the TUI's `i`/`n` send it onward, and the two LLM cleanups are **previews only** (shown for review, never written; the CLI refuses `--clean` with `--into`). A transcript never auto-inserts into the manuscript. Melody extraction stays Phase 17.

---

## 11. Annotations / comments

Comments must never contaminate manuscript text. Stored as sidecar files `comments/<document_id>.yaml` (visible, portable, not cache), anchored by the W3C text-quote pattern (A4):

```
Annotation
├── id, status (open/resolved/orphaned)
├── quote, prefix, suffix, offset_hint
├── author, created, text
```

Re-anchor order: offset hint → exact quote → fuzzy prefix/suffix → **orphaned** (flagged, never deleted). HTML comments the user types themselves are preserved byte-for-byte and stripped on export by default.

Export ignores annotations entirely → **Zero-Leak Export Guarantee**.

---

## 12. Import / export

```
Markdown / EPUB / text ─► Parser ─► OCA Document AST ─► Project model
Project ─► clean (typography, strip annotations, strip internal metadata) ─► validate ─► Markdown / EPUB / PDF
Song ─► lead sheet / lyrics ─► PDF / Markdown / text      (audio stays attached, not embedded)
```

No EPUB-specific types may leak into the manuscript model.

---

## 13. TUI

Keyboard-first. Do not recreate a GUI.

```
┌──────────────────────────────────────────────────────────────┐
│ My Novel                          Chapter 04 · The Forest    │
├───────────────┬──────────────────────────────────────────────┤
│ PROJECT       │ Elena stepped beneath the trees.             │
│ > Manuscript  │                                              │
│   Characters  │ The light disappeared almost immediately.    │
│   World       │                                              │
│   Outline     │                                              │
│   Music       │                                              │
│   Audio       │                                              │
│   Notes       │                                              │
├───────────────┴──────────────────────────────────────────────┤
│ NORMAL │ 1,842 words │ Draft │ F2 AI │ F3 Outline │ F5 Zen   │
└──────────────────────────────────────────────────────────────┘
```

Song editor: section navigation, lyric metrics (syllables/rhyme) in the status bar, proposals beside original.

Proposed keybindings (configurable): `Ctrl-S` save · `Ctrl-P` command palette · `Ctrl-K` AI · `Ctrl-F` search · `Ctrl-G` go to chapter · `Ctrl-N` new · `Ctrl-T` tag · `Ctrl-R` rewrite selection · `Ctrl-I` AI insight · `F2` AI panel · `F3` outline · `F4` comments · `F5` Zen · `F6` diff · `F7` Story Bible · `F8` RAG search.

Command palette (heart of the TUI): `rewrite selection`, `continue scene`, `check continuity`, `summarize chapter`, `find mentions of Marcus`, `show Elena's current state`, `create snapshot`, `compare with snapshot 87`, `export epub`, `improve rhyme`, `check singability`, `analyze album`…

Zen mode: centered cursor, current paragraph bright, previous paragraphs dimmed (~35%), UI hidden.

---

## 14. Crate mapping

Replaces the old `writer/` module tree. Crates are introduced phase by phase (see implementation plan §2).

| Old writer module | OCA crate |
| --- | --- |
| `manuscript/`, parser, importer, exporter | `oca-markdown`, `oca-story`, `oca-import`, `oca-export` |
| `story/` (characters, world, timeline, outline) | `oca-story` |
| *(music, new)* songs, albums, lyric tools | `oca-music` |
| `ai/` (llama, prompts, operations, context, pipelines) | `oca-ai` |
| `rag/` | `oca-rag` |
| `audio/` (recording, transcription, cleanup) | `oca-audio` |
| `versioning/` | `oca-versioning` |
| `storage/` (sqlite, filesystem), `metadata/` | `oca-storage`, `oca-model` |
| *(new)* AI evaluation fixtures + graders | `oca-eval` |
| `ui/` (tui, editor, zen, diff, panels), `app/` | `oca-tui`, `oca-core`, `oca-cli` |

---

## 15. Feature → phase map (roadmap order is authoritative)

| Feature | Phase |
| --- | --- |
| Markdown, project tree, TUI editor | 1 |
| Story/chapter/scene, entity files, state engine, deterministic queries | 2 |
| Songs, sections, metadata, syllable/rhyme/repetition tools, artist.yaml, albums | 3 |
| Recording, voice notes, attach to song/chapter | 4 |
| FTS, tags, backlinks, `oca reindex` | 5 |
| Snapshots, diff, restore | 6 |
| Runtime packs, llama.cpp subprocess, model manager, job system | 7 |
| Context Engine, Proposal model | 8 |
| Story AI (continue, rewrite, summarize, outline, review) | 9 |
| Music AI (+ deterministic grading) | 10 |
| RAG | 11 |
| Continuity engine | 12 |
| Song ↔ Story links, Creative Timeline | 13 |
| Rewrite/Refine/Review pipelines | 14 |
| EPUB/PDF/Markdown import-export, typography cleanup, zero-leak export | 15 |
| Zen, palette, keybindings, themes | 16 |
| MIDI, melody, beat, chords, waveform | 17 (post-v1) |
| Packaging | 18 |
| Dictation (whisper.cpp) | 7B (v1) |
| Screenplay (Fountain) | 20 (post-v1, optional) |

---

## 16. Gap log — resolved

All gaps from the original review (G1–G15) are resolved in `OCA-amendments.md` and applied to the architecture, roadmap and implementation plan.

| ID | Resolution | Where |
| --- | --- | --- |
| G1 | `.oca/cache` (deletable) vs `.oca/state` (precious) | A1 |
| G2 | Sidecar `comments/` with text-quote anchoring | A4 |
| G3 | Immutable ids, id-based links, watcher + conflict view | A2 |
| G4 | Atomic save, recovery journal, lock, crash test | A3 |
| G5 | Declarative `states:` + aliases; story time vs narrative position | A11 |
| G6 | ChordPro inline chords from Phase 3 | A11 |
| G7 | `lyrics.md` authoritative; `structure` → optional plan | A11 |
| G8 | Mixed projects in v1; `oca://` reserved; Universe post-v1 | A2 |
| G9 | Hardware tiers, constrained decoding, prompt versions, `oca-eval` | A8 |
| G10 | Proposal model with `base_hash`, rebase/stale | A5 |
| G11 | Runtime packs, SHA manifests, loopback + key, early release dry-run | A7 |
| G12 | Snapshots vs versions | A11 |
| G13 | `PhoneticProvider`, `dictionary.yaml`, grapheme-aware editor | A9 |
| G14 | Novel + short story in v1; screenplay Phase 20 | A12 |
| G15 | `format_version`, migrations, lossless frontmatter | A2 |
