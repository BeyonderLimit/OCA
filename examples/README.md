# examples/

Committed sample projects used by integration tests and safe to open by hand
(`oca open examples/blackwood`). Unlike the perf fixture under `fixtures/`
(generated, never committed), these are small, hand-authored, and load-bearing:
the story-engine exit criteria run against them.

- `blackwood/` — a ten-chapter novel exercising the whole Phase 2 story
  engine: character entities with aliases and chapter-keyed `states:`,
  mention-detection traps, and a timeline whose story clock and narrative
  position disagree on purpose (flashback + flash-forward). See its README.

Tests always work on a copy in a temp directory — the samples here are never
written to.
