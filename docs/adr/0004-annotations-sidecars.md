# ADR-0004: Annotations as sidecar files with text-quote anchoring

- Status: Accepted · Date: 2026-10-08 · Code: **A4** (decision D1)
- Source: `OCA-architechture.md` Part II §23 A4 · rationale: `OCA-amendments.md` §1 A4, D1

## Decision

- Comments live in **`comments/<document_id>.yaml`** — visible, portable, never inside manuscript text, never in cache.
- Anchoring uses the W3C text-quote pattern (`quote`, `prefix`, `suffix`, `offset_hint`). Re-anchor order: offset hint → exact quote → fuzzy prefix/suffix → **orphaned** (flagged in UI, never deleted).
- HTML comments the user typed (`<!-- … -->`) are preserved byte-for-byte as user content; export strips them by default (configurable).

## Consequences

- Zero-Leak Export Guarantee covers both annotations and user HTML comments.
- Works offline with no index: comments are plain YAML next to the project.
