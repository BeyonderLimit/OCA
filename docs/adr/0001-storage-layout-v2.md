# ADR-0001: Storage layout v2 — rebuildable vs precious data

- Status: Accepted · Date: 2026-10-08 · Code: **A1**
- Source: `OCA-architechture.md` Part II §23 A1 · rationale: `OCA-amendments.md` §1 A1

## Decision

Three data classes, three locations:

| Class | Where | Rule |
| --- | --- | --- |
| Creative content (prose, lyrics, entities, versions, comments, audio) | Visible project dirs | Portable, human-readable, user-owned |
| Irreplaceable state (snapshots, AI proposal/generation history) | `.oca/state/` | Never auto-deleted; included in `oca backup` |
| Derived cache (index, FTS, vectors, thumbnails) | `.oca/cache/` | Always safe to delete; `oca reindex` rebuilds |

- AI history is written **append-only to `state/ai/*.jsonl` first**; the `ai_generations` DB table is only an index over it.
- `oca backup`/`restore` archives content + `state/`, excludes `cache/`.

## Consequences

- "Delete the DB → rebuild" stays true without ever losing history.
- `oca doctor` warns if `state/` is missing/corrupt; `oca reindex` never touches `state/`.
