# ADR-0016: lyrics.md is authoritative (D4)

- Status: Accepted (decision) · Date: 2026-10-08 · Code: **D4**
- Source: `OCA-amendments.md` §5 D4

## Decision

`lyrics.md` headings are the single source of truth for section order and content. `structure:`/`plan:` in `song.yaml` is an optional target plan; `oca song check` reports drift between plan and actual.

## Consequences

Implements ADR-0011's song rule (G7). Editing a section in `lyrics.md` can never be contradicted by metadata — worst case is a reported drift warning.
