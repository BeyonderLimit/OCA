# ADR-0006: Job system and one-heavy-model governor

- Status: Accepted · Date: 2026-10-08 · Code: **A6**
- Source: `OCA-architechture.md` Part II §23 A6 · rationale: `OCA-amendments.md` §1 A6

## Decision

Indexing, embedding, generation, and transcription are long-running, cancellable, memory-hungry:

- Typed jobs in `oca-core` with progress, cancel, and priority (user-interactive > background).
- **Model lifecycle manager**: by default only **one heavy model resident at a time** (LLM *or* embeddings *or* whisper), with explicit swap; configurable on machines with spare RAM/VRAM.
- Background embedding yields to interactive generation.

## Consequences

- Jobs land with Phase 7 (runtime manager); all later long operations must go through them.
- `doctor`/config report the resident-model policy.
