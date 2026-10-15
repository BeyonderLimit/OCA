# ADR-0003: Write safety is a first-class subsystem

- Status: Accepted · Date: 2026-10-08 · Code: **A3**
- Source: `OCA-architechture.md` Part II §23 A3 · rationale: `OCA-amendments.md` §1 A3

## Decision

Data loss is the one unforgivable failure. In `oca-storage`:

- **Atomic save**: temp file in the same directory → `fsync` → rename (Windows-safe replace). Never write in place.
- **Recovery journal**: dirty editor buffers journaled to `.oca/state/recovery/` every few seconds; offered back on next open.
- **Single-writer lock**: `.oca/state/lock` (advisory, PID + liveness); second instance opens read-only.
- **Pre-flight snapshots** before bulk edit, import, migration, restore, or AI apply.
- **Crash test in CI**: kill during save at random points; file is always entirely old or entirely new.
- Windows/macOS care: case-insensitive collisions, reserved names, long paths, slug sanitization.

## Consequences

- All Phase 1 file I/O goes through this one save path; the kill-during-save test is a Phase 1 exit gate.
