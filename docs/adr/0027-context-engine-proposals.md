# ADR-0027: AI context engine and the proposal store

- Status: Accepted · Date: 2026-10-19 · Code: **A5, A8** · AGENTS rules 4, 6, 8
- Source: `OCA-architechture.md` Part II §23 A5/A8 · plan: `OCA-implementation.md` §3 Phase 8

## Decision

- The **context engine** lives in `oca-ai` (crate→phase table: "LanguageModel + context [Phase 7–8]"). It depends on `oca-core`, `oca-story`, and `oca-music` to pull deterministic state; the firewall stays one-directional — engine crates never depend on `oca-ai` (rule 3, enforced by `scripts/smoke.sh`).
- Eight explicit, serializable context types: `ManuscriptContext`, `ChapterContext`, `SceneContext`, `CharacterContext`, `WorldContext`, `SongContext`, `AlbumContext`, `ArtistContext`. Each renders to `ContextItem` blocks carrying provenance (`kind` + `id`), a priority, and a trust flag. The `Retriever` (a read-only view over `Core`) builds them from Phases 2/3 engine state and Phase 5 search results.
- **Golden prompt tests** (`crates/oca-ai/tests/golden/`) pin the byte-exact system + user renders, one file per context type, over the committed sample project plus a small in-test addition (world/artist/album/scenes). Regenerate deliberately with `OCA_UPDATE_GOLDEN=1 cargo test -p oca-ai` and review the diff — the files are artifacts, not disposable output.
- **Token budgeting (A8):** a conservative character-based estimate (4 characters per token, rounding up) is the single source of truth — with or without a loaded model — so budgets are reproducible in tests. Truncation policy: blocks fill highest-priority first (ties in given order); a block that would overflow is truncated to fit and marked `… [truncated]`; once the remainder can't hold a marked stub, the rest are dropped. Task instructions are never truncated (no task, no prompt).
- **Untrusted-content delimiting (A8):** imported/searched material renders inside `<untrusted-source …>` tags, and the fixed system preamble frames every quoted block (`<context>`, `<untrusted-source>`) as data, never instructions.
- The **proposal store (A5)** lands in `oca-core` so every AI phase shares it: `Command::ProposalCreate | ProposalApply | ProposalReject` plus events, with files under `.oca/state/ai/<id>.yaml` (precious state, rule 2). A proposal carries `operation`, `prompt_version`, `model`, the target range + anchor text, `base_checksum` (sha256 of the anchor — rule 7 names; the earlier sketch's Git-flavored field name is superseded), the `context_manifest`, and the payload.
- **Apply rule:** the recorded range still holds the anchor → apply in place; the document changed but the anchor survives exactly once elsewhere → rebase; otherwise → `stale`, never a silent overwrite. Empty-anchor (insertion) proposals apply only at their recorded point — an empty anchor matches everywhere, so there is nothing to search for. An auto-snapshot ("Before applying AI proposal (…)") is taken first, reusing the Phase 6 hook; if it fails, the apply is refused.
- Phase 8 ships **no user-facing operation**: the engine, builder, and proposal lifecycle are exercised by engine-level tests. The first operations and the universal proposal UX (`AI Proposal | Accept / Reject / Edit`) arrive in Phase 9. *(Delivered — see [ADR-0028](0028-story-ai-operations.md); Edit is audited by `Command::ProposalRevise`, which moves a proposal `pending → edited` and remains applicable.)*

## Consequences

- Every later AI phase builds context with the same Retriever and applies through the same proposal path — no feature can talk to llama.cpp directly (rule 6) or write project files (rule 4).
- A prompt change shows up as a reviewable diff in `tests/golden/`.
- Stale proposals can't clobber edits made while a generation streamed; ambiguous anchors are never guessed.
