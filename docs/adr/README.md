# ADR index

One short ADR per locked decision (Phase 0 set; later phases add spike
results such as ADR-0021). Full rationale lives in
`OCA-amendments.md`; authoritative restatements in `OCA-architechture.md` Part II.

| # | Code | Decision |
| --- | --- | --- |
| [0001](0001-storage-layout-v2.md) | A1 | Storage layout: cache vs state |
| [0002](0002-ids-links-lossless-frontmatter.md) | A2 | Immutable ids, id links, lossless frontmatter |
| [0003](0003-write-safety.md) | A3 | Atomic save, recovery journal, lock, crash test |
| [0004](0004-annotations-sidecars.md) | A4 | Sidecar annotations, text-quote anchoring |
| [0005](0005-commands-events-proposals.md) | A5 | Commands + events; proposals with `base_hash` |
| [0006](0006-jobs-model-governor.md) | A6 | Jobs; one heavy model resident |
| [0007](0007-runtime-packs-packaging.md) | A7 | Runtime packs, early signing, llama-server lockdown |
| [0008](0008-ai-reliability.md) | A8 | Hardware tiers, constrained decoding, eval harness |
| [0009](0009-language-unicode.md) | A9 | Language, Unicode, phonetics |
| [0010](0010-performance-budgets.md) | A10 | Performance budgets (500k words) |
| [0011](0011-domain-model-decisions.md) | A11 | Character state, songs, chords, versions |
| [0012](0012-scope-fixes.md) | A12 | v1 scope: novel + short story; no screenplay |
| [0013](0013-d1-comments-sidecar.md) | D1 | Comments as sidecar YAML |
| [0014](0014-d2-dictation-phase-7b.md) | D2 | Dictation in v1 as Phase 7B |
| [0015](0015-d3-mixed-projects-links.md) | D3 | Mixed projects; links by id, `oca://` reserved |
| [0016](0016-d4-lyrics-authoritative.md) | D4 | `lyrics.md` authoritative |
| [0017](0017-d5-chordpro-chords.md) | D5 | ChordPro chords from Phase 3 |
| [0018](0018-d6-screenplay-deferred.md) | D6 | Screenplay out of v1 |
| [0019](0019-d7-binary-name.md) | D7 | Binary name `oca` |
| [0020](0020-d8-sqlite-vec-runtime-packs.md) | D8 | Embedded sqlite-vec + runtime packs |
| [0021](0021-custom-rope-editor.md) | A9 spike | Custom rope editor (`tui-textarea` rejected) |
| [0022](0022-audio-takes.md) | Phase 4 | Audio takes: WAV + sidecar, quiet devices |
| [0023](0023-cache-db-search-units.md) | Phase 5 | Cache DB: one SQLite file, FTS5 units, mtime reconcile |
| [0024](0024-snapshot-engine.md) | Phase 6 | Snapshot engine: vendored libgit2, checksum-manifest audio |
| [0025](0025-ai-runtime.md) | Phase 7 | AI runtime: one trait, subprocess llama-server, offline packs |
| [0026](0026-dictation.md) | Phase 7B | Dictation: `Transcriber` seam, one-shot whisper, preview-only cleanup |
| [0027](0027-context-engine-proposals.md) | Phase 8 | Context engine, token budgets, and the proposal store |
| [0028](0028-story-ai-operations.md) | Phase 9 | Story AI operations, universal proposal UX, `oca-eval` graders |
| [0029](0029-music-ai-operations.md) | Phase 10 | Music AI operations, deterministic lyric scores shared three ways |
| [0030](0030-rag-hybrid-search.md) | Phase 11 | RAG — embedding seam, vector index in the cache DB, hybrid search |
