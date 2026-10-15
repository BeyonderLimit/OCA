# ADR-0014: Dictation in v1 as Phase 7B (D2)

- Status: Accepted (decision) · Date: 2026-10-08 · Code: **D2**
- Source: `OCA-amendments.md` §5 D2

## Decision

Dictation (mic → `cpal` → whisper → raw transcript) is pulled into v1 as **Phase 7B**, after Phase 7 (runtime packs + job system make whisper cheap — another subprocess runtime). It works with **no LLM installed** and a transcript never auto-inserts into the manuscript.

## Consequences

- Nothing depends on 7B, so it can slip without blocking Phase 8+.
- Do **not** absorb dictation into Phase 4 (audio foundation) — scope-creep risk.
