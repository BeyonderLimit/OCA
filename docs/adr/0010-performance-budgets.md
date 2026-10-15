# ADR-0010: Performance budgets (500k words stays usable)

- Status: Accepted · Date: 2026-10-08 · Code: **A10**
- Source: `OCA-architechture.md` Part II §23 A10 · rationale: `OCA-amendments.md` §1 A10

## Decision

Measurable budgets, tested in CI on a generated corpus (`scripts/generate-fixture.py`, seeded):

| Operation | Budget (500k-word project, mid-range laptop) |
| --- | --- |
| Open project (cold, cache present) | < 1 s to interactive |
| Keystroke → screen | < 16 ms |
| Full-text search | < 100 ms |
| Reindex single saved file | < 200 ms |
| Full reindex | reported, regression-tracked |
| Snapshot create | < 2 s |

## Consequences

- Fixture generator + perf harness skeleton exist in Phase 0 (`scripts/`); budgets are enforced in CI from Phase 5.
- Features that would blow a budget need design review, not "faster later".
