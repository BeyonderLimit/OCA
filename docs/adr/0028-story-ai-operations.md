# ADR-0028: Story AI operations, the universal proposal UX, and `oca-eval`

- Status: Accepted · Date: 2026-10-19 · Code: **A5, A8** · AGENTS rules 3, 4, 6, 8
- Source: `OCA-architechture.md` Part II §23 A5/A8 · plan: `OCA-implementation.md` §3 Phase 9 · spec: `OCA-build.md` §7.2–7.4

## Decision

- **Five operations, smallest first** — Continue scene → Rewrite selection → Summarize chapter → Generate outline → Review chapter — live in `oca-ai/src/operations.rs`. No new crate beyond `oca-eval` (crate→phase table): the operations need exactly what `oca-ai` already depends on (`oca-core`, `oca-story`).
- **Three-phase pipeline per operation**, so no phase of it can write on its own: `prepare_*` (main thread, `&Core`, fully deterministic — resolves the target range, gathers context, builds the prompt; the model is not consulted) → `complete` (job thread, no `Core` handle at all, streams via `Ai::generate_with`, cancellable through the job's cancel token) → `create_proposal` (main thread, `&mut Core`, `Command::ProposalCreate`). The model only ever fills in text; *where* text lands is decided before it runs and recorded after it finishes (rule 4).
- **Two output contracts.** Continue/rewrite/summarize are free text (summarize prepends `## <chapter title>` so the appended block is a valid markdown section). Outline/review use constrained decoding (A8 schemas) with exactly one repair step — strip markdown fences — before a parse fails for real; invalid model output is a clean error, never a mangled draft.
- **Review is a report, not a revision** (`OCA-build.md` §7.4): `DraftTarget::Report` — the rendered issue list is printed (CLI) or shown in the AI panel (TUI) and is never recorded as a proposal, because "nothing to fix" must not produce an editable artifact.
- **Append targets** (summarize → `notes/summaries.md`, outline → `outline/outline.md`) ensure the container document exists first — an *empty file only*; the AI text arrives solely through the accepted proposal.
- **Edit is audited**: `Command::ProposalRevise { id, payload }` + `Event::ProposalRevised`. Apply/reject/revise accept only undecided proposals (`pending` or `edited` — `ProposalStatus::is_undecided`); the TUI's payload editor (the same rope editor, boxed inside the panel) saves through this command, so an author-edited payload is itself a recorded event, and the subsequent apply still takes the auto-snapshot first.
- **Universal proposal UX**, one flow for this and every later AI phase: TUI `p` panel — list (status-colored) plus a side-by-side review, *Current Draft* vs *AI Proposed Revision* — with `a` accept, `r` reject, `e` edit; CLI `oca ai continue|rewrite|summarize|outline|review` and `oca proposal list|show|apply|reject`. Every decision is a Core command through the frontends (rule 8), and accept is undoable via snapshot restore (Phase 6), which is the Phase 9 exit criterion.
- **`[ai] mock = true`** (config, field-merged like every other `[ai]` key) → `BootstrapOptions.mock` → `bootstrap` returns `MockModel::story_defaults()`: canned answers matched on the operation's first prompt line (`Operation: <name>`). The whole surface — CLI, TUI, evals — can be exercised with no runtime pack, no model files, and zero stderr (rule 3; deleting `~/.oca` cannot break it).
- **`oca-eval`** (new crate, Phase 9): *seeded* fixtures (fixed bytes, no RNG — reproducible on any machine) plus deterministic graders: `SchemaValidity` (JSON object with required keys, via the shared `oca_ai::repair_json`), `SummaryLength` (word band), `PreservesNamedEntities` (explicit entity list from the story engine, not capitalization guessing), `NonEmpty`. Grading is a pure function of text — **no LLM-as-judge in CI** (A8); graders also have to fail, so each has a negative test.

## Consequences

- Phase 10 (music AI) reuses the identical pipeline and UX with `SongContext` and deterministic rhyme/meter graders — nothing else gets to invent an apply path.
- Stale-during-generation is covered at three levels: engine (`oca-ai/tests/operations.rs`), TUI (panel test), and CLI e2e — edit while streaming → rebase if the anchor survives once, `stale` otherwise, never a silent overwrite.
- The dictation cleanup path (ADR-0026) can adopt the same proposal UX later without new machinery; Phase 9 did not rework it.
