# Model installation (no weights in git)

OCA is fully usable without any models (AGENTS rule 3). Models are **optional,
side-loaded, and never committed to this repository**.

Layout (created on demand):

```
~/.oca/models/
├── llm/           # GGUF models for llama-server (local generation)
├── embeddings/    # small embedding model for RAG (Phase 11)
└── whisper/       # whisper.cpp models for dictation (Phase 7B)
```

Rules (architecture A7/A8):

- Runtime packs (`llama-server`, whisper) install via `oca runtime install --from <file>`
  so offline side-loading is first-class (lands Phase 7).
- Every pack and model ships a **SHA-256 manifest**; `oca doctor` verifies it.
- One heavy model resident at a time by default (LLM *or* embeddings *or* whisper).
- `oca doctor` recommends a size class from hardware tiers (Lite / Standard /
  Comfortable) — pick by RAM/VRAM, not by model name.
- No telemetry, no downloads without an explicit user action.
