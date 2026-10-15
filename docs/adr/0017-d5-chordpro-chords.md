# ADR-0017: ChordPro inline chords from Phase 3 (D5)

- Status: Accepted (decision) · Date: 2026-10-08 · Code: **D5**
- Source: `OCA-amendments.md` §5 D5

## Decision

Chords are written inline in lyrics, ChordPro-style (`I [Dm]thought I [F]knew`). The parser extracts chord events with character offsets.

## Consequences

- Phase 3 stores/displays/transposes; Phase 15 exports lead sheets from the same data.
- Syllable counting strips chord markers first (Phase 3 exit test).
