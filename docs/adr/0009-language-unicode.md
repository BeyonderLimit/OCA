# ADR-0009: Language, Unicode, and phonetics

- Status: Accepted · Date: 2026-10-08 · Code: **A9**
- Source: `OCA-architechture.md` Part II §23 A9 · rationale: `OCA-amendments.md` §1 A9

## Decision

- `project.yaml: language: en` (default). Unsupported language → lyric tools say **"unsupported"** rather than showing wrong numbers.
- **`PhoneticProvider` trait**: English = offline pronunciation dictionary + rule-based grapheme-to-phoneme fallback; others pluggable later.
- **`dictionary.yaml`** lets the author define pronunciations for invented names.
- Editor text handling: rope buffer (`ropey`), grapheme clusters (`unicode-segmentation`), display width (`unicode-width`). RTL/bidi is a documented v1 limitation.
- Editor widget choice is a **Phase 1 spike** against a written checklist (undo/redo, soft wrap, selection, clipboard, mouse, 50k-word file stays smooth, wide characters).

## Consequences

- Syllable/rhyme/typography rules are per-language everywhere they appear.
- Phase 1 exit includes the widget spike decision recorded as an ADR update.
