# ADR-0005: Core API as commands + events; AI produces proposals only

- Status: Accepted · Date: 2026-10-08 · Code: **A5** · AGENTS rules 4
- Source: `OCA-architechture.md` Part II §23 A5 · rationale: `OCA-amendments.md` §1 A5

## Decision

- Every mutation in `oca-core` is a **Command** that emits **Events** (audit trail, undo, headless mode, one choke point where "snapshot before apply" is enforced).
- `Proposal` is a first-class object stored in `state/ai/`, carrying `base_hash` (hash of target text at generation time), `prompt_version`, `context_manifest`, `status: pending|accepted|rejected|stale|edited`.
- Apply rule: current hash == `base_hash` → apply (after snapshot); target range intact but doc changed → rebase and apply; otherwise `stale` — offer re-diff or regenerate, never silent overwrite.

## Consequences

- The author can keep typing while a generation runs; stale proposals can't clobber edits.
- TUI/CLI/GUI are all clients of this API (rule 8); no direct storage writes from frontends.
