# ADR-0002: Immutable ids, id-based links, lossless frontmatter

- Status: Accepted · Date: 2026-10-08 · Code: **A2**
- Source: `OCA-architechture.md` Part II §23 A2 · rationale: `OCA-amendments.md` §1 A2

## Decision

- Every document/entity carries an immutable ULID-style `id` in frontmatter, generated at creation; `project.yaml` carries `project_id` and `format_version: 1`.
- **Links reference ids, never paths** (`[[ch_01J9…]]`, `[[elena]]` via id/alias). Renames/moves never break links; the index maps id → path.
- `oca://<project_id>/<id>` is reserved for post-v1 cross-project links (in-project only in v1).
- Files created externally get a provisional path-hash id until the author runs `oca adopt`; duplicate ids are found by `doctor`.
- External edits: file watcher + mtime+hash check → silent reload (clean buffer) or conflict view with auto-snapshot (dirty buffer).
- Format migrations are ordered `N → N+1` file functions, dry-run via `oca migrate --check`, snapshot first; newer `format_version` opens read-only.
- **Frontmatter edits must be lossless**: keep raw text + parsed view, apply targeted patches so unknown keys and YAML comments survive (breaks if re-serialized via `serde_yaml`).

## Consequences

- Phase 1 exit includes byte-identical round-trip and rename-keeps-links-working tests.
- Fallback if no lossless-YAML approach: canonical format + snapshot before first rewrite.
