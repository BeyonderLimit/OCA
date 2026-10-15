# ADR-0008: AI reliability layer

- Status: Accepted · Date: 2026-10-08 · Code: **A8**
- Source: `OCA-architechture.md` Part II §23 A8 · rationale: `OCA-amendments.md` §1 A8

## Decision

- **Hardware tiers, not model names** (model names churn): Lite (8 GB CPU, ~3–4B), Standard (16 GB / 8 GB VRAM, ~7–9B), Comfortable (32 GB+ / 24 GB VRAM, ~14–32B). `doctor` detects and recommends; benchmarks published in `docs/`.
- **Constrained decoding** for every operation with an output schema (JSON-schema/grammar in llama.cpp) with parse → repair → retry fallback; free-text ops skip it.
- **`prompt_version`** recorded on every proposal so results are comparable across prompt changes.
- **Token budgeting**: server tokenizer when a model is loaded; conservative character estimate otherwise (golden prompt tests use the estimate).
- **`oca-eval`**: fixture projects with seeded defects + deterministic graders; on-demand/nightly against a pinned model; **no LLM-as-judge in CI**.
- Imported/RAG text is untrusted data: delimited context blocks, never instructions.

## Consequences

- Practical context is 8–32K tokens; retrieval, not stuffing.
- `oca-eval` crate introduced at Phase 9.
