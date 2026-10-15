# ADR-0024: Snapshot engine — vendored libgit2 store, checksum-manifest audio

- Status: Accepted · Date: 2026-10-15 · Code: (Phase 6)
- Source: `OCA-implementation.md` §3 Phase 6 · `OCA-build.md` §9 · `OCA-architechture.md` G12/A11 · AGENTS rules 7, 8

## Decision

- **`oca-versioning` crate** (per the crate→phase table) exposing only
  `SnapshotStore { create, list, restore, diff_working, diff_pair }`.
  Implementation lock: **`git2` with `default-features = false,
  features = ["vendored-libgit2"]`** — libgit2 compiles from source, so no
  system `git`/`libgit2`/network stack is required (the ssh/https features are
  off; snapshots never leave the machine).
- **Store**: a bare repo at **`.oca/state/snapshots.git`**, created lazily on
  the first snapshot. History is one append-only chain under an internal ref;
  user-facing numbers are 1-based positions (oldest = 1). Restoring first
  snapshots the current state, so history only grows — nothing is ever
  rewritten or dropped.
- **What a snapshot covers**: every file under the project root except
  `.oca/` (cache/logs/state are not authoring content), `.git/`, `target/`.
  Symlinks are not followed. User vocabulary is **Snapshots** only; the
  `oca-cli/tests/vocabulary.rs` scan enforces it across the crate.
- **Audio by checksum manifest**: `*.wav` is never stored as a blob. Each
  snapshot carries an in-tree manifest (`.oca/snapshot-manifest.json`, stored
  under the snapshot's own `.oca/` entry so restore never writes it out) with
  `files` count + `path → sha256`. Restore **never creates, overwrites, or
  deletes recorded bytes** — it only reports takes that differ, vanished, or
  are newer than the snapshot (`RestoreReport::audio_differing`).
- **Restore semantics**: files in the snapshot are written byte-exact via
  sibling-temp + rename; files the snapshot predates are removed (audio and
  `.oca/` internals exempt); identical files are skipped. The cache index is
  reconciled afterwards (best effort — it is derived state).
- **Auto-snapshot policy lives in Core** (A5 choke point): `SnapshotRestore`
  saves the current state first and **refuses the restore if that fails** —
  restoring is never itself a way to lose work. The same hook is what AI
  proposal applies (Phase 7+), bulk edits, and imports will call.
- **Core surface**: `Command::{SnapshotCreate, SnapshotRestore}` →
  `Event::{SnapshotCreated, SnapshotRestored}`; queries `snapshots()` /
  `snapshot_diff(n, against)`; types re-exported so frontends never depend on
  `oca-versioning` (rule 8). Mutations refuse read-only sessions.
- **Diff**: `similar`-generated unified patches over the snapshot tree vs the
  working files (or two snapshots); binary files and audio are one-line
  entries without patch text. Line-level hunks today; finer-grained
  (word-level) rendering is a TUI evolution, not a store change.

## Consequences

- `create → diff → restore` round-trips from CLI, Core, and TUI; the
  fuzz-style test restores random-churn states byte-for-byte (audio exempt by
  design and reported instead).
- Snapshot store build cost: libgit2 compiles once per platform in CI.
- `.oca/state/` now holds the safety net — backups include it, and nothing
  auto-cleans it (rule 2).
