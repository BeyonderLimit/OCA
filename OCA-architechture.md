# OCA - Full Architecture

> **v2 (amended 2026-10-08):** Part II (§23, amendments A1–A12) supersedes Part I wherever they conflict. Rationale and gap log: `OCA-amendments.md`.

**OCA: Offline Creative Assistant** 

It captures the broader scope without locking the project into "novel writing." It can naturally encompass **stories, songwriting, albums, audio notes, worldbuilding, research, and AI-assisted creation**.

## OCA — Offline Creative Assistant

### Product definition

> **OCA is a local-first creative workspace for writing stories, creating music, organizing ideas, and using private offline AI to develop, revise, and manage creative projects.**

### Core pillars

```
                         OCA
              Offline Creative Assistant
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
     📖 STORY           🎵 MUSIC           🎙 AUDIO
        │                  │                  │
     Novels              Songs             Dictation
     Chapters            Albums            Voice notes
     Scenes              Lyrics            Recordings
     Characters          Demos             Ideas
     Worldbuilding       Artist Bible
     Timelines           Rhythm/Rhyme
        │                  │                  │
        └──────────────────┼──────────────────┘
                           │
                    🧠 LOCAL AI
                           │
                 llama.cpp + RAG
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
      Draft              Refine             Review
      Outline            Rewrite            Continuity
      Brainstorm         Analyze            Suggestions
```

## Product architecture

I'd now officially organize OCA into five major subsystems:

```
OCA
│
├── Creative Projects
│   ├── Story
│   ├── Music
│   ├── Mixed
│   └── Notes
│
├── Authoring
│   ├── Markdown editor
│   ├── TUI
│   ├── Zen mode
│   ├── Comments
│   └── Versioning
│
├── Knowledge
│   ├── Characters
│   ├── Worldbuilding
│   ├── Artist Bible
│   ├── Timeline
│   ├── Tags
│   └── RAG
│
├── AI
│   ├── llama.cpp
│   ├── Context Engine
│   ├── Writing
│   ├── Songwriting
│   ├── Review
│   └── Continuity
│
└── Media
    ├── Audio recording
    ├── Voice notes
    ├── Music demos
    ├── Images
    └── Export
```

### The important distinction

**OCA is not an AI writing app with some extra features.**

It's:

> **A creative workspace that happens to contain a powerful local AI assistant.**

That distinction should guide the architecture.

The user's files remain theirs.

```
                 USER CREATIVE WORK
                        │
                        ▼
              ┌──────────────────┐
              │ OCA PROJECT FILES│
              └────────┬─────────┘
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
       Writing       Music         Audio
          │            │            │
          └────────────┼────────────┘
                       ▼
                 OCA Knowledge
                       │
                       ▼
                  Local AI
                       │
                       ▼
                 Suggestions
```

The AI **suggests**.

The author **decides**.

That's a very strong identity for OCA.

For **OCA — Offline Creative Assistant**, I would strongly recommend a **Rust-first native desktop/TUI stack**, with the AI and storage layers designed so they can evolve independently.

The key requirements are unusual enough that I would **not** build this as an Electron/web application.

## My recommended stack

| Layer | Recommendation | Why |
| --- | --- | --- |
| Core application | **Rust** | Native, fast, portable, excellent filesystem/CLI/audio ecosystem |
| TUI | **Ratatui** \+ Crossterm | Excellent terminal UI foundation |
| Desktop UI later | **Tauri 2 + Svelte/TypeScript** | Lightweight GUI without abandoning Rust core |
| Markdown | `pulldown-cmark` / `comrak` | Markdown parsing/rendering |
| YAML | `serde_yaml` | Project/frontmatter metadata |
| Database | **SQLite** | Portable, mature, offline |
| SQLite layer | **SQLx** or `rusqlite` | Rust integration |
| Full-text search | **SQLite FTS5** | Excellent local search |
| Vector search | **sqlite-vec** initially | Keeps deployment simple |
| Embeddings | Local embedding model | Fully offline RAG |
| LLM | **llama.cpp** | Your chosen local inference engine |
| llama.cpp integration | Rust FFI/bindings or subprocess initially | Flexible integration |
| Audio recording | **CPAL** | Cross-platform native audio |
| Speech-to-text | **whisper.cpp** | Local/offline transcription |
| Audio processing | **Symphonia** \+ FFmpeg where necessary | Broad media support |
| Versioning | **Git/libgit2** or native snapshot layer | Mature diff/restore infrastructure |
| EPUB | Rust EPUB parser/generator | Local import/export |
| PDF | HTML/CSS → PDF or native renderer | Publishing |
| Serialization | **Serde** | Rust ecosystem standard |
| Async | **Tokio** | Mature async runtime |
| CLI | **clap** | Excellent command-line interface |
| Logging | **tracing** | Structured logging |
| Testing | Rust built-in + integration tests | Strong foundation |
| Packaging | cargo-dist / platform installers | Distribution |

---

# 1\. Rust should be the foundation

I think Rust is particularly well suited to OCA.

Your application needs to simultaneously handle:

```
filesystem
SQLite
Markdown
TUI
Git
audio
speech recognition
LLM inference
embeddings
EPUB
PDF
large documents
streaming generation
```

That's exactly the sort of workload where a native systems language makes sense.

The architecture would be:

```
                    OCA
                     │
              ┌──────┴──────┐
              │ OCA Core    │
              │    Rust     │
              └──────┬──────┘
                     │
       ┌─────────────┼─────────────┐
       ▼             ▼             ▼
    Storage          AI          Media
       │             │             │
    SQLite       llama.cpp      CPAL
    Filesystem   Embeddings     whisper.cpp
    Git           RAG            Audio
```

The important thing is that **the TUI isn't the application**.

The TUI is just one frontend to an OCA core.

---

# 2\. Build the core as a library

I'd make this distinction from day one:

```
oca/
├── oca-core/
├── oca-cli/
├── oca-tui/
├── oca-ai/
├── oca-rag/
├── oca-audio/
└── oca-export/
```

Conceptually:

```
                 ┌──────────────┐
                 │   oca-tui    │
                 └──────┬───────┘
                        │
                 ┌──────▼───────┐
                 │   oca-core   │
                 └──────┬───────┘
                        │
       ┌────────────────┼────────────────┐
       ▼                ▼                ▼
   oca-storage        oca-ai          oca-media
```

Later:

```
                 ┌──────────────┐
                 │   Desktop    │
                 │ Tauri/Svelte │
                 └──────┬───────┘
                        │
                 ┌──────▼───────┐
                 │   oca-core   │
                 └──────────────┘
```

This prevents you from having to rewrite the application when you eventually want a graphical interface.

---

# 3\. TUI: Ratatui

For the terminal UI:

**Ratatui + Crossterm**

I'd choose this over trying to build the interface from scratch.

Your TUI becomes:

```
┌────────────────────────────────────────────────────┐
│ OCA                         My Novel / Chapter 04  │
├───────────────┬────────────────────────────────────┤
│ PROJECT       │                                    │
│               │ Elena stepped beneath the trees.  │
│ Manuscript    │                                    │
│ Characters    │ The forest was silent.            │
│ World         │                                    │
│ Outline       │ She stopped.                      │
│ Music         │                                    │
│ Audio         │                                    │
│               │                                    │
├───────────────┴────────────────────────────────────┤
│ NORMAL │ 1,843 words │ Draft │ F2 AI │ F5 Zen     │
└────────────────────────────────────────────────────┘
```

Ratatui is particularly appropriate because OCA is fundamentally keyboard-driven.

---

# 4\. Don't make the TUI the only interface

I'd design for:

```
                OCA Core
                   │
       ┌───────────┼───────────┐
       │           │           │
      TUI         CLI       Desktop
       │                       │
    terminal                 Tauri
```

Initially:

```
TUI + CLI
```

Later:

```
TUI + CLI + GUI
```

The GUI can be added without changing the underlying project format.

---

# 5\. Tauri for the eventual GUI

If you eventually want the beautiful features you described:

- visual diff
- margin comments
- waveform
- drag-and-drop EPUB
- split-pane editing
- focus mode
- rich manuscript navigation

I'd use **Tauri 2**.

Frontend:

**Svelte + TypeScript**

rather than React, unless you already strongly prefer React.

Architecture:

```
              Tauri
                │
       ┌────────┴────────┐
       │                 │
   Svelte UI          Rust Core
       │                 │
       └────────┬────────┘
                │
              OCA
```

That gives you a small native application instead of shipping a Chromium runtime as the entire application.

---

# 6\. SQLite should be your local database

This is an easy choice.

Use:

**SQLite**

for:

```
project metadata
documents
chapters
scenes
characters
relationships
tags
annotations
snapshots
AI operations
RAG metadata
audio metadata
```

But don't store your entire manuscript exclusively in SQLite.

Use:

```
Filesystem
    ↓
Markdown = source of truth

SQLite
    ↓
Index / metadata / relationships / cache
```

That gives you:

```
my-project/
├── manuscript/
│   ├── 001.md
│   └── 002.md
│
├── characters/
│   └── elena.md
│
└── .oca/
    ├── cache/oca.db      # rebuildable index (FTS5 + vectors)
    ├── state/            # snapshots, AI history, recovery (never auto-deleted)
    └── logs/
```

If the database becomes corrupted, the manuscript still exists.

---

# 7\. SQLite FTS5

For normal search:

**SQLite FTS5**

is enough.

You can search:

```
Elena
```

and get:

```
Chapter 2
Chapter 8
Chapter 13
Character profile
Song: Ashes
World note
```

This should coexist with semantic search rather than replacing it.

---

# 8\. Vector database: don't introduce a separate server

I would **not** deploy Qdrant, Weaviate, Milvus, Pinecone, etc. for OCA's initial architecture.

You're building an offline application.

You don't want:

```
OCA
 ↓
Docker
 ↓
Vector server
 ↓
Database
```

Instead:

```
OCA
 ↓
SQLite
 ├── metadata
 ├── FTS5
 └── vectors
```

I'd start with **sqlite-vec** or an equivalent embedded vector extension.

That keeps:

```
OCA project
     ↓
one database
```

rather than creating another service the user has to manage.

---

# 9\. llama.cpp

This should remain behind an abstraction.

Don't let the rest of OCA know about llama.cpp directly.

Instead:

```
AI Provider
    │
    ├── llama.cpp
    ├── future provider
    └── testing/mock provider
```

Interface conceptually:

```
trait LanguageModel {
    fn load(&self, model: ModelConfig);
    fn generate(&self, request: GenerationRequest);
    fn cancel(&self);
}
```

Then your AI operations don't care whether the model comes from:

```
llama.cpp
```

or something else later.

---

# 10\. Use llama.cpp initially as a subprocess

For the very first implementation, I would actually avoid deep FFI integration.

Start with:

```
OCA
 │
 └── launches llama.cpp
          │
          └── local model
```

Why?

It makes development substantially easier.

You can get:

```
streaming
cancellation
model selection
process isolation
crash isolation
```

without binding the entire OCA process directly to the inference engine.

Once everything works, you can integrate more deeply if performance requires it.

---

# 11\. Embeddings

For RAG, use a **separate embedding model**.

Don't use your generative LLM as the embedding system.

Architecture:

```
Document
   ↓
Chunker
   ↓
Embedding Model
   ↓
Vector
   ↓
SQLite
```

Then:

```
Query
 ↓
Embedding
 ↓
Vector search
 ↓
Reranking
 ↓
Context
 ↓
llama.cpp
```

This separation will make the system much easier to tune.

---

# 12\. Speech recognition: whisper.cpp

For your dictation requirement, **whisper.cpp** fits OCA extremely well.

Pipeline:

```
Microphone
    ↓
CPAL
    ↓
WAV/audio buffer
    ↓
whisper.cpp
    ↓
Transcript
    ↓
OCA editor
```

Then optionally:

```
Transcript
    ↓
Local LLM
    ↓
Cleaned prose
```

The distinction between those two stages is important.

---

# 13\. Audio: CPAL

For recording:

**CPAL**

gives you a cross-platform Rust audio foundation.

Then:

```
CPAL
 ↓
Audio buffer
 ↓
WAV
```

For more advanced music functionality later:

```
Audio
 ↓
FFmpeg / Symphonia
 ↓
decode
 ↓
analysis
```

Eventually you can add:

```
MIDI
pitch detection
tempo detection
waveform visualization
```

without changing your fundamental architecture.

---

# 14\. Markdown architecture

I would define an internal document model:

```
Document
│
├── Frontmatter
├── Blocks
│   ├── Heading
│   ├── Paragraph
│   ├── Quote
│   ├── Image
│   ├── Table
│   └── Section
└── Metadata
```

Then both story and music can use it.

```
Story
 └── Chapter
      └── Scene
           └── Markdown Document

Music
 └── Song
      └── Section
           └── Markdown Document
```

---

# 15\. Git for versioning

I would seriously consider using Git internally rather than creating an entire version-control system yourself.

Your users don't need to know they're using Git.

OCA can provide:

```
Snapshot

"Before AI rewrite"

[Compare]
[Restore]
```

while internally:

```
git commit
```

stores the state.

Then you get mature:

- diffs
- history
- branches
- restoration
- file tracking

for free.

However, **don't expose Git concepts in the primary UX**. Writers want "Snapshots," not "HEAD," "rebase," and "commit hashes."

---

# 16\. EPUB

EPUB should go through a normalized internal representation:

```
EPUB
 ↓
Parser
 ↓
OCA Document Model
 ↓
Project
```

And:

```
Project
 ↓
Publishing Engine
 ↓
EPUB
```

This prevents EPUB-specific assumptions from leaking into the manuscript model.

---

# 17\. The most important database design

I'd have roughly these tables:

```
projects
documents
sections
characters
character_states
locations
relationships
timeline_events

songs
albums
song_sections
artist_profiles

tags
document_tags

annotations      (index over comments/*.yaml)
snapshots        (index over .oca/state)
versions
proposals
schema_migrations

audio_assets
media_assets

ai_operations
ai_generations

documents_fts
document_embeddings
```

The exact schema will evolve, but this gives you the right conceptual separation.

---

# 18\. Suggested Rust workspace

I'd start the repository like this:

```
oca/
│
├── Cargo.toml
│
├── crates/
│   ├── oca-core/
│   ├── oca-model/
│   ├── oca-storage/
│   ├── oca-markdown/
│   ├── oca-story/
│   ├── oca-music/
│   ├── oca-ai/
│   ├── oca-rag/
│   ├── oca-eval/
│   ├── oca-audio/
│   ├── oca-versioning/
│   ├── oca-import/
│   ├── oca-export/
│   ├── oca-tui/
│   └── oca-cli/
│
├── tests/
│
├── models/
│   └── README.md
│
├── docs/
│
├── examples/
│
└── scripts/
```

That might look like over-engineering at first.

It isn't.

You're already building several distinct systems.

---

# 19\. Dependency philosophy

I'd keep the dependency graph roughly like this:

```
oca-model
   ↑
oca-storage
   ↑
oca-core
   ↑
 ┌─┼───────────────┐
 │ │               │
AI Story          Music
│  │               │
RAG               Audio
 │
TUI / CLI / GUI
```

**Don't let****`oca-tui`****become the place where business logic lives.**

This is one of the easiest ways a project like this becomes impossible to maintain.

---

# 20\. What I would NOT use

For this project, I'd avoid:

### ❌ Electron

Too heavyweight for your offline-first philosophy.

### ❌ Cloud database

Contradicts the local-first architecture.

### ❌ Cloud LLM API as a requirement

OCA should work with no network.

### ❌ Microservices

You don't need:

```
10 containers
7 APIs
3 databases
Kubernetes
```

You need:

```
one executable
one project directory
one SQLite database
local models
```

### ❌ Separate vector database server

Start embedded.

### ❌ AI-generated metadata as authoritative data

AI should suggest.

The project database should decide.

---

# 21\. Recommended deployment architecture

Ultimately, I'd want OCA to look like this to the user:

```
                 OCA
                  │
       ┌──────────┴──────────┐
       │                     │
    Project                Models
       │                     │
       ▼                     ▼
my-novel/              ~/.oca/models/
├── manuscript/        ├── llm/
├── characters/        ├── embeddings/
├── world/             └── whisper/
├── music/
├── audio/
├── notes/
└── .oca/
    ├── cache/oca.db      # rebuildable index (FTS5 + vectors)
    ├── state/            # snapshots, AI history, recovery (never auto-deleted)
    └── logs/
```

No server.

No account.

No cloud requirement.

No Internet requirement.

---

# 22\. My final stack recommendation

If I were starting OCA tomorrow, my **v1 stack** would be:

```
LANGUAGE
Rust

TUI
Ratatui
Crossterm

ASYNC
Tokio

DATABASE
SQLite
rusqlite
SQLite FTS5
sqlite-vec

DOCUMENTS
Markdown
pulldown-cmark/comrak
Serde
YAML frontmatter

AI
llama.cpp

EMBEDDINGS
Local embedding model

SPEECH
whisper.cpp

AUDIO
CPAL
Symphonia

VERSIONING
Git/libgit2

CLI
clap

LOGGING
tracing

TESTING
cargo test
integration tests

PACKAGING
cargo-dist

FUTURE GUI
Tauri 2
Svelte + TypeScript
```

And I'd make the **first production target**:

> **Linux + macOS + Windows, terminal-first, completely offline, Markdown-native, with SQLite and local AI.**

Then Tauri becomes the graphical layer rather than the foundation.

### The one architectural decision I'd lock down immediately

Define the **OCA Core API** before building the TUI:

```
                    OCA CORE API
                         │
       ┌─────────────────┼─────────────────┐
       │                 │                 │
     Story             Music             Media
       │                 │                 │
   chapters            songs            audio
   scenes              albums           voice
   characters          lyrics           recordings
   world               artist
       │                 │
       └─────────────────┼─────────────────┘
                         │
                    AI Context
                         │
                  llama.cpp / RAG
```

If that API is clean, you can eventually have **TUI OCA**, **Desktop OCA**, and potentially even a **headless OCA CLI/server mode** all operating on exactly the same creative projects.

---

# 23. Part II — v2 Amendments (A1–A12)

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
