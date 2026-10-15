# ADR-0011: Domain-model decisions (state, songs, chords, versions)

- Status: Accepted · Date: 2026-10-08 · Code: **A11**
- Source: `OCA-architechture.md` Part II §23 A11 · rationale: `OCA-amendments.md` §1 A11

## Decision

- **Character state**: authored declaratively in the entity file as `states:` keyed by chapter id with carry-forward semantics (latest entry at or before chapter N applies), plus `aliases:`. Mention detection = word-boundary match on name + aliases (pronouns unresolved — documented limit). Timeline events distinguish **story time** from **narrative position** so flashbacks don't break "before chapter 8?".
- **Songs**: `lyrics.md` headings are authoritative for order/content; `plan:`/`structure:` in `song.yaml` is an optional plan (`oca song check` reports drift). Repeated sections reference the original instead of copying text.
- **Chords**: ChordPro-style inline (`I [Dm]thought I [F]knew`); parser extracts chord events with offsets; syllable engine strips them before counting.
- **Snapshots vs versions**: snapshots = automatic project-wide safety net (`.oca/state/snapshots.git`); versions = author-named drafts of one item (`versions/v01.md`) with save/compare/promote. One diff engine, two concepts. Promote snapshots first.
- **Implemented (Phase 2)**: engine in `crates/oca-story`; concrete formats — `states:` merge semantics + entity schema in `OCA-build.md` §5.1, timeline `events:` schema in §4.6.
- **Implemented (Phase 3)**: engine in `crates/oca-music`. Repeat syntax locked as `## Chorus 2 {=chorus}` — the token names the target's canonical section kind; a reference resolves to the nearest earlier non-reference section with the same token, metrics reuse the referent's lines, drift compares canonical tokens 1:1. Pronunciations: project `dictionary.yaml` → embedded `en.dict` → G2P rules; unsupported languages report *unsupported*, never invented numbers (A9). Formats: `dictionary.yaml` in `OCA-build.md` §6.1, repeat/version semantics in §4.3.

## Consequences

- State engine is deterministic — the substrate for continuity checks (Phase 12) without re-reading the manuscript.
- Repeat-section syntax is closed: `## Chorus 2 {=chorus}` (`OCA-build.md` §4.3); the TUI navigates sections with `Alt-]`/`Alt-[` because plain brackets are chord syntax.
