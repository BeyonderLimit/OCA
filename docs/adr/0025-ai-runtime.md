# ADR-0025: AI runtime — one trait, subprocess llama-server, offline packs

- Status: Accepted · Date: 2026-10-15 · Code: (Phase 7; implements **A6**, **A7**, **A8**)
- Source: `OCA-implementation.md` §3 Phase 7 · `OCA-build.md` §7.5 · ADR-0006/0007/0008 · AGENTS rules 3, 6, 8

## Decision

- **`oca-ai` crate** (per the crate→phase table) with **`trait LanguageModel`**
  as the only llama.cpp touchpoint (rule 6): `info`, `is_loaded`, `load`,
  `generate` (streaming into a sink), `cancel`, `unload`. Every method is
  `&self` — implementations carry interior state so `cancel` can arrive from
  any thread mid-stream. `ModelConfig` (context, temperature, top-p, max
  tokens, threads, GPU layers, server binary, startup timeout) is fixed per
  instance; `load` is idempotent spawn-or-attach. First impl:
  **`LlamaCppSubprocess`**; **`MockModel`** backs every test — no test ever
  touches a real model.
- **Subprocess, not FFI; loopback with a per-session key (A7)**:
  `llama-server` is spawned on `127.0.0.1:<ephemeral>` with a random 32-byte
  API key, readiness polled via `/health`, and tokens read from the
  OpenAI-compatible API as SSE by a hand-rolled HTTP/1.1 client over
  `TcpStream` (loopback needs no TLS, so no HTTP-client dependency). Cancel
  = socket shutdown (interrupt the stream, runtime stays up); unload = kill
  + reap. A dead runtime is a **clean error** (`model runtime exited
  unexpectedly…`), never a crash of OCA; a stream that ends without `[DONE]`
  is an error, never a silently truncated "success". Runtime logs go to
  `~/.oca/logs/llama-server.log`; llama-server does not echo prompts into
  them, so the content gate (A7) holds by default.
- **Constrained decoding (A8)**: `GenerationRequest.schema` passes a
  JSON-schema through as the runtime's `response_format`; ops with output
  schemas use it from Phase 8 on (parse → repair → retry lives with the
  operations). Free-text probes skip it.
- **Model governor + jobs (A6)**: `ModelManager` enforces *one heavy model
  resident* (LLM | Embeddings | Whisper slots; swaps are explicit — a second
  kind errors with "unload it first"; capacity configurable for big-RAM
  machines). `oca-core` gains the job system: labeled threads with priority
  (interactive > background), cooperative cancel tokens, progress messages,
  panic-isolation (`Failed("job panicked")`), and bounded tracking. The TUI's
  Load/probe run as jobs; every later long operation goes through them.
- **Hardware tiers, not model names (A8)**: RAM via `sysconf`
  (unix) / `GlobalMemoryStatusEx` (windows), cores via the stdlib;
  `doctor` reports `Hardware — N GB RAM × N cores → Lite/Standard/Comfortable`.
- **Runtime packs (A7/D8)**: a pack is a `.tar.gz` with `manifest.json`
  (`name`, `version`, `platform` = `<os>-<arch>` or `any`, `files:
  path → sha256`). `oca runtime install --from <file>` verifies **every**
  file against the manifest into a staging dir, then renames into
  `~/.oca/runtime/<name>-<version>` — a tampered, wrong-platform, symlinked,
  or tar-slipped pack never lands. `oca runtime list` re-verifies installed
  packs and exits non-zero when one was modified after install.
  `find_llama_server` searches packs first, then `PATH`.
- **Bootstrap is all-or-nothing (rule 3)**: AI is enabled only when a
  runtime *and* a model are present; otherwise `bootstrap` returns `None`
  and the TUI's `I` panel explains how to enable AI. Deleting every model
  file (or uninstalling packs) breaks nothing anywhere.
- **Test strategy**: the exit criteria are proven against a *real spawned
  process* — the `oca-ai` test binary re-invokes itself as a fake
  `llama-server` (via the `server_args`/`server_env` config overrides,
  delivered as child-only environment so a normal test run can never
  misfire), speaking `/health` + streaming `/v1/chat/completions` through
  `oca_ai::fake_server`. Proven: load → stream → unload **100/100**, cancel
  interrupts **100/100** streams, an externally SIGKILLed runtime is a clean
  error mid-stream and a fresh load recovers. CI never needs llama.cpp.
- **Packaging spike (signed/notarized pack per OS)**: deferred — no signing
  identity exists in this environment. Manual verification lives in
  `docs/runtime-pack-checklist.md` (same pattern as the audio checklist).

## Consequences

- Nothing AI-side touches project files. When proposals arrive (Rule 4,
  Phase 8/9), apply goes through `Proposal` + auto-snapshot — the same
  strict "snapshot first or refuse" policy `SnapshotRestore` uses today.
- Only `oca-cli`/`oca-tui` may depend on `oca-ai`; the deterministic engines
  must not (smoke.sh gate, rule 3).
- `ModelConfig::server_args`/`server_env` are the documented seam for
  nonstandard runtimes (custom flags, `CUDA_VISIBLE_DEVICES`) and for the
  test double; the standard args are the llama-server flag set.
- `~/.oca/models/llm` (GGUF, largest-first discovery) and `~/.oca/runtime`
  stay outside every project — nothing AI-related is ever committed.
