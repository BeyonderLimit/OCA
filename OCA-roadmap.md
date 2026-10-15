# OCA Roadmap Build

 **phased deployment**, where every phase leaves you with a usable application rather than waiting until the entire AI/music stack is finished.

# OCA (Offline Creative Assistant) — Phased Build & Deployment Plan

> **v2 (amended 2026-10-08):** each phase has a **v2 amendments** block. Codes like (A7) refer to `OCA-architechture.md` Part II. Dictation is now Phase 7B; screenplay is post-v1 Phase 20. Rationale: `OCA-amendments.md`.

## Target architecture

```
┌─────────────────────────────────────────────────────────────┐
│              OCA — OFFLINE CREATIVE ASSISTANT               │
├──────────────────────────────┬──────────────────────────────┤
│ STORY MODE                   │ MUSIC MODE                   │
│                              │                              │
│ Novel                        │ Songs                        │
│ Chapters                     │ Lyrics                       │
│ Scenes                       │ Albums                       │
│ Characters                   │ Demos                        │
│ Worldbuilding                │ Artist Bible                 │
│ Timeline                     │ Rhythm / rhyme               │
│ Plot / outline               │ Song structure               │
├──────────────────────────────┴──────────────────────────────┤
│                    SHARED CREATIVE ENGINE                   │
├─────────────────────────────────────────────────────────────┤
│ Markdown │ Projects │ Tags │ Search │ Versioning │ Audio   │
├─────────────────────────────────────────────────────────────┤
│                     AI / CONTEXT ENGINE                     │
├─────────────────────────────────────────────────────────────┤
│ llama.cpp │ RAG │ Embeddings │ Story Bible │ Artist Bible  │
├─────────────────────────────────────────────────────────────┤
│                     LOCAL STORAGE                           │
├─────────────────────────────────────────────────────────────┤
│ Filesystem │ SQLite │ Vector Index │ Git/Snapshots         │
└─────────────────────────────────────────────────────────────┘
```

---

# Phase 0 — Development foundation

### Goal

Get the project building, testing, packaging, and running **without any AI**.

Do this first.

### Build

```
oca/
├── src/
├── tests/
├── resources/
├── examples/
├── docs/
├── scripts/
├── Cargo.toml / package.json / etc.
└── README.md
```

Pick one primary language/runtime and stick with it.

For a terminal-first application, I'd strongly favor:

**Rust**

because you eventually want:

- TUI
- filesystem access
- SQLite
- Git
- audio
- native binaries
- llama.cpp integration
- cross-platform packaging

without requiring a large runtime installation.

### Deliverables

```
[ ] Project builds
[ ] Unit testing works
[ ] CI works
[ ] Linux build
[ ] macOS build
[ ] Windows build
[ ] Debug executable
[ ] Release executable
[ ] Basic logging
[ ] Configuration system
```

### Deployment artifact

```
oca --version
oca --help
oca doctor
```

`doctor` should eventually report:

```
OCA Doctor

✓ Database
✓ Project directory
✓ Git
✓ Audio subsystem
✓ Model runtime
⚠ No embedding model installed
```


### v2 amendments

- `docs/adr/` directory: one short ADR per locked decision (this file's A1–A12 become ADRs).
- CI **release dry-run** with `cargo-dist` on every tag; signing process documented even if unsigned initially (A7).
- **Fixture generator:** synthesizes a 500k-word project (many chapters, entities, songs) for perf tests (A10).
- Perf harness skeleton; license decision for the repo.
- *Exit adds:* a dry-run release artifact builds on all 3 OSes.

---

# Phase 1 — Core project & Markdown engine

### Goal

Create a **completely functional offline writing application without AI**.

This is your first real milestone.

## Implement

### Project management

```
create project
open project
close project
list projects
```

Example:

```
oca new my-novel
oca open my-novel
```

### Filesystem model

```
project/
├── project.yaml
├── manuscript/
├── characters/
├── world/
├── outline/
├── music/
├── notes/
├── audio/
└── .oca/            # cache/ (rebuildable) + state/ (snapshots, AI history)
```

### Markdown

Implement:

- Markdown parser
- YAML frontmatter
- headings
- tables
- images
- links
- code blocks
- comments/annotations

### TUI

Build:

```
Project Tree
     │
     ├── Manuscript
     ├── Characters
     ├── World
     ├── Outline
     ├── Music
     └── Notes

Editor
```

### Deliverable

You should be able to:

```
create project
→ create chapter
→ write
→ save
→ close
→ reopen
→ continue writing
```

**No AI required.**


### v2 amendments

- Layout v2 (A1), `project_id`, `format_version`, ids, id-based link resolution (A2).
- Safe-write, recovery journal, single-writer lock (A3).
- Command/Event core API skeleton (A5).
- Editor widget **spike** with checklist; Unicode handling (A9).
- Lossless frontmatter approach (A2).
- *Exit adds:* kill-during-save crash test passes; rename/move a file externally → links still resolve; unknown frontmatter keys + YAML comments survive an edit; open newer `format_version` → read-only.

---

# Phase 2 — Story engine

### Goal

Turn the Markdown editor into a proper novel-writing environment.

Implement:

```
Story
├── manuscript
├── chapters
├── scenes
├── characters
├── locations
├── factions
├── timeline
└── outline
```

### Character schema

```
id: elena
name: Elena
role: protagonist

state:
  location: blackwood
  emotional: frightened
  goal: find_father

relationships:
  marcus:
    type: distrust
```

### Story state

Create a state engine:

```
Chapter 1
   ↓
Character State
   ↓
Chapter 2
   ↓
Character State
   ↓
Chapter 3
```

This becomes critical later for AI continuity.

### Deliverable

The application can answer basic deterministic questions:

```
Where is Elena?
Who is Marcus?
What chapters contain Elena?
What happened before chapter 8?
```


### v2 amendments

- `states:` + `aliases:` schema, carry-forward resolution, story-time vs narrative position (A11).
- TUI "set state at chapter" form.
- Short story as a supported form (A12).
- *Exit adds:* flashback fixture answers "what happened before chapter 8?" correctly in both time senses; alias fixture finds "Ellie" mentions.

---

# Phase 3 — Music engine

This should happen **before the AI**, not after it.

### Goal

Make Music Mode a first-class part of the application.

## Project structure

```
music/
├── songs/
│   ├── ashes/
│   │   ├── song.yaml
│   │   ├── lyrics.md
│   │   ├── versions/
│   │   ├── demos/
│   │   └── notes/
│   │
│   └── blackwood/
│
├── albums/
└── artist.yaml
```

### Song editor

Support:

```
Intro
Verse
Pre-Chorus
Chorus
Post-Chorus
Bridge
Breakdown
Outro
Interlude
```

### Song metadata

```
title: Ashes
genre:
  - alternative
mood:
  - dark
  - hopeful

tempo: 92
key: Dm
time_signature: 4/4

themes:
  - loss
  - rebirth
```

### Deterministic lyric tools

Implement:

```
syllable counting
line length
rhyme detection
repetition detection
section structure
```

### Deliverable

You can write a complete song offline without AI.


### v2 amendments

- `lyrics.md` authoritative + plan drift check; repeat-section reference syntax (A11).
- ChordPro chords: parse, display, transpose (A11).
- `PhoneticProvider` + `dictionary.yaml` + `language:` (A9).
- Albums model; `artist.yaml` schema.
- "Save as version" (copy + label) — compare/promote arrive in Phase 6.
- *Exit adds:* syllable count ignores chord markers; invented name with user pronunciation rhymes correctly; unsupported language shows "unsupported".

---

# Phase 4 — Audio foundation

### Goal

Introduce audio without introducing AI yet.

Implement:

```
record
pause
resume
stop
play
delete
rename
attach to project
attach to song
attach to chapter
```

Example:

```
song/
├── lyrics.md
├── song.yaml
└── demos/
    ├── idea-001.wav
    ├── chorus-002.wav
    └── guitar-003.wav
```

### Voice notes

For stories:

```
Record idea
     ↓
Save audio
     ↓
Attach to chapter
```

For music:

```
Record melody
     ↓
Save audio
     ↓
Attach to song
```

Don't attempt melody extraction yet.

### Deliverable

The application becomes an **offline writing + songwriting + voice-note application**.


### v2 amendments

- Creative Note record (audio + optional text + tempo + note), with melodic fields reserved but unused (see `OCA-build.md §10`).
- Audio files are **excluded from snapshot Git history** (stored by reference + checksum) to avoid repo bloat (see architecture Part II, A1).

---

# Phase 5 — Search, tags & indexing

### Goal

Build the infrastructure AI will later depend upon.

Use SQLite for metadata/indexing.

Implement:

```
full-text search
tags
metadata
relationships
backlinks
references
```

Example:

```
tag:character:elena
tag:theme:loss
tag:location:blackwood
tag:song:ashes
```

### Search

```
Ctrl-F
```

should search:

```
chapters
scenes
characters
world
songs
lyrics
notes
audio metadata
```

### Auto-tagging infrastructure

Initially deterministic/manual:

```
[tag]
```

Later AI can populate suggestions.

### Deliverable

A 500,000-word project should remain usable without AI.


### v2 amendments

- File watcher + reconcile loop; `oca reindex` tests (cache only).
- `sqlite-vec` static-link spike (A7).
- Migration framework for DB *and* file format (A2).
- *Exit adds:* perf budgets in A10 pass in CI on the generated corpus.

---

# Phase 6 — Versioning & diff system

### Goal

Protect the author's work before introducing automated rewriting.

Implement snapshots:

```
snapshot/
├── metadata
├── manuscript state
├── music state
└── project state
```

I'd use **Git internally where practical**, while presenting a writer-friendly abstraction.

Commands:

```
snapshot create
snapshot list
snapshot restore
diff
```

TUI:

```
CURRENT
   │
   ├── Snapshot 001
   ├── Snapshot 002
   ├── Snapshot 003
   └── Snapshot 004
```

Diff:

```
OLD                         NEW

Elena entered the forest.   Elena stepped beneath
                            the ancient trees.

                            + ancient
                            + stepped beneath
```

### Deliverable

Before touching AI:

> **Every destructive operation is reversible.**

This should be a hard architectural rule.


### v2 amendments

- Snapshots in `.oca/state/snapshots.git`; audio tracked by checksum manifest, not blobs.
- Version compare/promote built on the same diff engine (A11).
- Conflict view for external-edit collisions (A2).

---

# Phase 7 — Runtime manager & llama.cpp integration

### Goal

Introduce the local LLM.

Do **not** immediately implement every AI feature.

First create:

```
AI Runtime
     │
     ▼
Model Manager
     │
     ▼
llama.cpp
```

Support:

```
model discovery
model loading
model unloading
context configuration
temperature
top-p
max tokens
streaming
cancel generation
```

TUI:

```
AI Model

● llama model
  Context: 32K
  GPU: enabled
  Threads: 12

[Load]
[Unload]
```

### Critical requirement

AI generation must be **interruptible**.

```
Generating...
███████████████░░░

[ESC] Stop
```


### v2 amendments

- Runtime packs + SHA manifests + side-load install (A7); loopback + API key.
- Model lifecycle manager and job system (A6).
- Hardware detection → tier recommendation in `doctor` (A8).
- Constrained-decoding support in `LanguageModel::generate` (schema/grammar parameter).
- **Packaging spike:** a signed/notarized runtime pack runs on each OS.


---

# Phase 7B — Dictation

### Goal

Voice capture and transcription as a first-class feature, working with **no LLM installed**.

### Build

Why: it's one of the three pillars, and once Phase 7 gives you runtime packs and a job system it is cheap (whisper is another subprocess runtime).

- Mic → `cpal` → whisper transcription (works with **no LLM**) → raw transcript note.
- Options per capture: keep raw / clean transcription / turn into prose / add to notes / insert into chapter (cleanup options that need the LLM appear only when one is loaded).
- *Exit:* record → transcribe → insert works with only whisper installed; transcript never auto-inserts into the manuscript.
- Decision D2: dictation is in v1. Nothing else depends on it, so it can slip without blocking Phase 8+.

---

# Phase 8 — AI context engine

This is the phase I would spend the most architectural effort on.

Don't connect every feature directly to llama.cpp.

Instead:

```
AI Feature
    ↓
Context Engine
    ↓
Retriever
    ↓
Story/Music State
    ↓
Prompt Builder
    ↓
llama.cpp
```

Create explicit contexts:

```
ManuscriptContext
ChapterContext
SceneContext
CharacterContext
WorldContext
SongContext
AlbumContext
ArtistContext
```

Example:

```
SongContext

current lyrics
previous versions
artist bible
song metadata
related songs
themes
associated story material
```


### v2 amendments

- Proposal model + store (A5) lands here so every AI phase uses it.
- Context manifests recorded on each proposal; token budgeting (A8); `prompt_version`.
- Untrusted-content delimiting for RAG/imported text (A8).

---

# Phase 9 — Story AI

Now implement the first useful AI operations.

Start small.

### 1\. Continue

```
Continue scene
```

### 2\. Rewrite selection

```
Rewrite selected paragraph
```

### 3\. Summarize

```
Summarize chapter
```

### 4\. Outline

```
Generate chapter outline
```

### 5\. Review

```
Review chapter
```

Every AI operation produces a **proposal**, never an automatic overwrite.

```
AI Proposal
    │
    ├── Accept
    ├── Reject
    └── Edit
```


### v2 amendments

- `oca-eval` introduced with seeded fixtures (A8); first graders: schema validity, summary length, rewrite preserves named entities.
- Stale-proposal tests: edit during generation → rebase or stale, never silent overwrite.

---

# Phase 10 — Music AI

Now implement the equivalent music pipeline.

### Song operations

```
Generate concept
Generate structure
Generate verse
Generate chorus
Generate bridge
Rewrite lyrics
Improve rhyme
Improve meter
Generate alternatives
Analyze lyrics
Check singability
```

For example:

```
Select chorus
    ↓
Ctrl-K
    ↓
Improve rhyme
    ↓
AI Proposal
```

The result should be presented beside the original.

```
ORIGINAL                  PROPOSED

I walked into the night  I disappeared into night
                         where nobody knew my name
```

The user chooses.


### v2 amendments

- Deterministic graders for `PRESERVE_HOOK` / `PRESERVE_RHYME_SCHEME` (hook still present, scheme unchanged).
- Album analysis operation (A12).
- Optional **originality check**: n-gram overlap of proposed lyrics against the user's own reference corpus, to flag accidental near-copies.

---

# Phase 11 — RAG

Only now build the large knowledge base.

## Pipeline

```
Import
  ↓
Parse
  ↓
Normalize
  ↓
Chunk
  ↓
Metadata extraction
  ↓
Embedding
  ↓
Vector index
```

Support:

```
Markdown
EPUB
text
project files
character documents
worldbuilding
lyrics
notes
```

### Hybrid search

```
              Query
                │
       ┌────────┴────────┐
       ▼                 ▼
    SQLite FTS       Vector Search
       │                 │
       └────────┬────────┘
                ▼
           Reranker
                │
                ▼
          Context Builder
```


### v2 amendments

- Embedding-model lifecycle within the one-heavy-model rule (A6).
- Reference library import marks source/provenance; "untrusted" flag (A8).

---

# Phase 12 — Story continuity engine

Now use RAG + story state together.

Implement:

```
character continuity
location continuity
timeline continuity
object continuity
relationship continuity
world-rule continuity
```

Example:

```
Potential continuity issue

Chapter 14:
Elena says she has never seen Marcus.

Chapter 7:
Elena and Marcus previously met.

Severity: HIGH

[View references]
[Ignore]
[Create note]
```

This becomes one of the application's strongest features.


### v2 amendments

- Requires Phase 2 `states:`; evaluation against seeded contradictions via `oca-eval`.

---

# Phase 13 — Song/Story relationship engine

This is where your original concept becomes something special.

Allow:

```
song ↔ character
song ↔ chapter
song ↔ event
song ↔ theme
song ↔ album
```

Example:

```
Elena
 │
 ├── Chapter 18
 │      └── betrayal
 │
 └── Song: Ashes
        ├── theme: betrayal
        ├── theme: rebirth
        └── represents Chapter 18
```

Then AI can use both narrative and musical context.


### v2 amendments

- Links stored by id; `oca://` form reserved (A2).
- **Creative Timeline view** (story events + songs + album tracks on one axis) — the feature described in `OCA-music-build §13`, now with a home.

---

# Phase 14 — Full AI pipelines

Now implement your original:

```
REWRITE
   ↓
REFINE
   ↓
REVIEW
```

For stories:

```
Chapter
 ↓
Rewrite
 ↓
Grammar / style
 ↓
Continuity
 ↓
Pacing
 ↓
Review
```

For songs:

```
Lyrics
 ↓
Rewrite
 ↓
Rhyme / meter
 ↓
Singability
 ↓
Theme consistency
 ↓
Review
```

The two pipelines share infrastructure but use different evaluators.


### v2 amendments

- Album review pipeline (A12); resumable pipeline state stored in `state/`.

---

# Phase 15 — Import/export

Implement the publishing pipeline.

```
PROJECT
   │
   ├── Markdown
   ├── EPUB
   └── PDF
```

Before export:

```
Manuscript
   ↓
Typography cleanup
   ↓
Remove comments
   ↓
Remove internal metadata
   ↓
Validate
   ↓
Export
```

Music:

```
Song
   ↓
Lyrics
   ↓
Lead sheet / lyric export
   ↓
PDF / Markdown / text
```

Audio remains separately attached.


### v2 amendments

- Lead sheet export from ChordPro data (A11).
- EPUB metadata: title/author/language/cover, TOC, front/back matter; export settings stored in `project.yaml: export:`.
- User HTML comments stripped by default; annotations never exported (A4).

---

# Phase 16 — Zen Mode & professional UX

Only after the core system works.

Implement:

- Zen mode
- centered cursor
- focus dimming
- Ghost HUD
- dual-view comments
- visual diff studio
- command palette
- configurable keybindings
- themes
- accessibility
- autosave indicators

---

# Phase 17 — Advanced music capabilities

This is the **optional second-generation music layer**.

Possible additions:

```
MIDI import/export
MIDI playback
tempo map
chord progression
key detection
pitch detection
melody transcription
beat detection
audio waveform
lyrics ↔ timing
```

Eventually:

```
record melody
     ↓
pitch detection
     ↓
MIDI
     ↓
song structure
```

I would **not make this part of v1**.

---

# Phase 18 — Packaging & release

Once the system is stable, create platform packages.

```
Linux
├── AppImage
├── .deb
└── tar.gz

macOS
├── .app
└── .dmg

Windows
├── .exe
└── installer
```

The installer should **not require Internet access** for normal operation.

Model installation can be an optional separate package/download.


### v2 amendments

- Everything risky (signing, runtime packs, static sqlite-vec) was proven earlier; this phase wires final installers and runs the fresh-machine, no-network test.
- `oca backup` / `restore` verified.


---

# Phase 20 — Screenplay (Fountain) — post-v1, optional

Add a screenplay project form using the Fountain format, reusing the story engine (characters, timeline, continuity) with a screenplay-aware document model. Not part of v1; format reserved via `project.yaml: kind`.

---

# Recommended release milestones

| Release | Main capability |
| --- | --- |
| **0.1** | Phases 0–1: TUI + Markdown + safe storage |
| **0.2** | Projects + chapters + scenes |
| **0.3** | Characters + worldbuilding + story state |
| **0.4** | Music + lyrics + chords + songs |
| **0.5** | Audio recording + voice notes |
| **0.6** | Search + tags + index + perf budgets |
| **0.7** | Snapshots + versions + diff |
| **0.8** | Runtime packs + llama.cpp + dictation (Phases 7 + 7B) |
| **0.9** | Context engine + proposals + basic AI writing |
| **0.10** | AI songwriting + album analysis |
| **0.11** | RAG |
| **0.12** | Continuity engine |
| **0.13** | Story ↔ Music integration + Creative Timeline |
| **0.14** | Rewrite/refine/review pipelines |
| **0.15** | EPUB/PDF publishing + lead sheets |
| **0.16** | Zen mode + advanced UX |
| **1.0** | Stable OCA (Phase 18) |
| *post-1.0* | Phase 17 advanced music · Phase 20 screenplay |

---

# The development order I'd actually use

If you're building this yourself, I would follow this exact dependency chain:

```
             ┌──────────────┐
             │ Phase 0      │
             │ Foundation   │
             └──────┬───────┘
                    ↓
             ┌──────────────┐
             │ Phase 1      │
             │ Markdown/TUI │
             └──────┬───────┘
                    ↓
        ┌───────────┴───────────┐
        ↓                       ↓
┌───────────────┐       ┌────────────────┐
│ Phase 2       │       │ Phase 3        │
│ Story Engine  │       │ Music Engine   │
└───────┬───────┘       └───────┬────────┘
        │                       │
        └───────────┬───────────┘
                    ↓
             ┌──────────────┐
             │ Phase 4      │
             │ Audio        │
             └──────┬───────┘
                    ↓
             ┌──────────────┐
             │ Phase 5      │
             │ Search/RAG   │
             └──────┬───────┘
                    ↓
             ┌──────────────┐
             │ Phase 6      │
             │ Versioning   │
             └──────┬───────┘
                    ↓
             ┌──────────────┐
             │ Phase 7      │
             │ llama.cpp    │
             └──────┬───────┘
                    ↓
             ┌──────────────┐
             │ Phase 8      │
             │ Context      │
             └──────┬───────┘
                    ↓
        ┌───────────┴───────────┐
        ↓                       ↓
  Story AI                 Music AI
        │                       │
        └───────────┬───────────┘
                    ↓
             ┌──────────────┐
             │ RAG +        │
             │ Continuity   │
             └──────┬───────┘
                    ↓
             ┌──────────────┐
             │ Publishing   │
             └──────┬───────┘
                    ↓
                  v1.0
```

> **v2 note:** Phase 7B (Dictation) runs after Phase 7, in parallel with Phase 8. Phases 9 and 10 parallelize after Phase 8.

## One thing I would change from the original proposal

**Don't make "AI" Phase 1.**

Make the first six phases completely functional without it.

That gives you a very strong property:

> **If llama.cpp, the model, embeddings, or even the entire AI subsystem disappears, the application still works as a serious offline writing and songwriting studio.**

Then AI becomes a **replaceable acceleration layer**, rather than something the application fundamentally depends upon.

And the natural endpoint isn't merely an "AI Novel Writer." It's a **local creative operating environment for stories + music + audio**, where the same characters, themes, timelines, ideas, lyrics, recordings, and references can interact.
