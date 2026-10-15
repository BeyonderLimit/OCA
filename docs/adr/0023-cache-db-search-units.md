# ADR-0023: The cache DB — one SQLite file, FTS5 over "units", mtime reconcile

- Status: Accepted · Date: 2026-10-15 · Code: Phase 5
- Source: `OCA-implementation.md` Phase 5; `OCA-architechture.md` §17; A2/A3

## Context

Phase 5 adds search, tags, and backlinks over every entity in a project.
Files stay canonical (rule 2): the index must be derived state that `oca
reindex` can rebuild from scratch, and a broken index must never block
editing. The DB must not become a second source of truth, and — since
Phase 6+ (reversibility, proposals, AI jobs) will grow it — it needs a
migration framework from day one.

## Decision

### The DB lives in `oca-storage`, not a new crate

Per the crate→phase table (`OCA-implementation.md` §2) the DB has always
been part of `oca-storage` ("FS layout + SQLite [Phase 1, DB in 5]"); the
AGENTS gloss "`oca-db`" was a misnomer. `Db` sits behind `Project`
(`crates/oca-storage/src/db.rs`), and frontends reach it only through
`Core` (rule 8).

### One file, WAL, bundled SQLite

`.oca/cache/oca.db` holds metadata + FTS5 + (later) vectors — never a
second `vectors.db`, no server, no Docker (D8). `rusqlite` with the
`bundled` feature is a workspace dep, so FTS5 is always available
regardless of system SQLite. Best-effort pragmas: `journal_mode=WAL`,
`synchronous=NORMAL`, `busy_timeout=2s`.

### Schema via migrations from v1 (A2)

`schema_migrations` records applied versions; migration v1 creates
`documents`, `search_units`, `search_text`, `search_fts`, `document_tags`,
`document_links`, `characters`, `character_states`, `relationships`,
`locations`, `timeline_events`, `songs`, `song_sections`, `audio_assets`,
`annotations`. Tables for snapshots/proposals (Phase 6/7), AI history
(Phase 7), embeddings (Phase 11), and albums (Phase 10/14) arrive in *their*
phases' migrations — the framework is the deliverable now.

### One FTS5 table over "units", external content

A *unit* is anything searchable: chapter/world/character/timeline
documents, song metadata + lyric sections, timeline events (title + summary
text), and audio take sidecars. `search_units` maps each FTS rowid to
`(kind, ref, rel, title)`; `search_text` holds the actual text;
`search_fts` is FTS5 **external content** over `search_text` — chosen over
contentless because `snippet()` must return stored text. Deletes go
fts-rowid → text → unit. `fts_query` quotes every term and only prefixes
the last, so author input can never be interpreted as FTS syntax;
`tag:ns:value` is routed to a plain tag lookup instead.

### Reconcile is a stat walk, not a watcher

External edits surface through `mtime_ns` + `size` comparison on project
open, after saves, on `oca reindex`, and on the TUI idle tick (5s,
skipped read-only). No `notify` dependency; no background threads. Song
directories (`song.yaml` + `lyrics.md`) index as one unit keyed by the
song id. A corrupt DB self-heals on open — delete and recreate (rule 2:
it is derived state).

### Best-effort, never blocks

`Project` holds `Option<Db>`, opened at `Project::open` (warm rebuild when
empty, reconcile otherwise). Open failures log a warning and degrade to
"search unavailable — run `oca reindex`"; every write path indexes
best-effort after the atomic file write. `.oca/state/` is untouched.

### sqlite-vec spike: passed (closes D8's pending item)

`crates/oca-storage/tests/spike.rs` registers `sqlite3_vec_init` via
`rusqlite::ffi::sqlite3_auto_extension` (transmuted fn pointer) and
creates/queries a `vec0` table. The runtime-embedding pattern for Phase 11
is validated; wiring stays deferred until then.

## Consequences

- Cross-entity search, tags, and backlinks are one SQLite file that `oca
  reindex` rebuilds; deleting `.oca/cache/` costs nothing (open rebuilds).
- Measured on the generated 513k-word corpus (A10, mid-range laptop):
  search ~41 ms, single-file reconcile ~58 ms, full reindex ~238 ms —
  budgets are 100/200 ms.
- Phase 6+ schema work is incremental migrations, never a rebuild.
