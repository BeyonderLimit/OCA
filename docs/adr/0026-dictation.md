# ADR-0026: Dictation — a `Transcriber` seam, one-shot whisper, preview-only cleanup

- Status: Accepted · Date: 2026-10-16 · Code: (Phase 7B; implements **D2**, A6 residency, rules **3, 4, 6, 8**)
- Source: `OCA-implementation.md` §3 Phase 7B · `OCA-build.md` §10 · ADR-0014 (D2), ADR-0022, ADR-0025 · AGENTS rules 3, 4, 6, 8

## Decision

- **No new crate** (crate→phase table, `OCA-implementation.md` §2). Dictation
  splits along the existing layering: `oca-audio` gains a `Transcriber`
  trait plus the rules for *where a transcript may go*, and `oca-ai` gains
  `Dictation` — whisper.cpp as a second one-shot subprocess runtime beside
  `LlamaCppSubprocess`. The frontends glue them: `FnTranscriber` closures
  adapt `oca_ai::Dictation` through `Audio::with_transcriber` (CLI) and
  `App::attach_transcriber` (TUI).
- **The seam keeps rule 3's dependency gate intact**: `oca-audio` never
  depends on `oca-ai` — it only knows `trait Transcriber`
  (`label()` + blocking `transcribe(wav, cancel)`). With no transcriber
  attached, transcription explains itself (`missing_runtime_hint()`) and
  everything else works. `scripts/smoke.sh`'s "engines stay AI-free" loop
  still includes `oca-audio`.
- **Whisper is a subprocess, never FFI** (the rule 6 discipline extends to
  the whisper half): one `whisper-cli` run per take with
  `-m <model> -f <wav> -nt -np -otxt -of <base>`; the transcript is read
  from `<base>.txt`, stdout/stderr go to `~/.oca/logs/whisper.log`, and the
  cancel token kills the child mid-run. The child also receives
  `OCA_TRANSCRIBE_OUT=<base>.txt` — the same contract real runs honor via
  `-of`, and test doubles honor via the variable, so CI never needs
  whisper.cpp. `whisper_args` (config `[ai]`) replaces the standard
  arguments entirely, mirroring `llama_server` — that's the seam for test
  doubles and nonstandard runtimes. Models live in
  `~/.oca/models/whisper` (`.gguf` + legacy `.bin`, largest first).
- **Bootstrap is independent of the LLM (rule 3)**:
  `bootstrap_dictation(opts, governor) -> Option<Dictation>` needs only a
  `whisper-cli` (configured path → packs → `PATH`) **and** a model file;
  `oca doctor` reports them as one discovery-only `Dictation` row that
  never spawns anything. `ModelManager` is now `Clone` (shared `Arc`
  inner): the TUI's language model and dictation run under one residency
  budget — `Dictation::transcribe` claims the Whisper slot (A6) for the
  run's duration, and a cleanup preview claims the LLM slot. With no LLM
  installed at all, dictation is complete on its own.
- **The transcript never writes itself forward** (exit criterion, §3 7B):
  transcription stores the raw text in the take's sidecar
  (`transcript: {text, created}`, losslessly patched like every other key)
  and the Phase 5 index makes it searchable — full stop. Every other
  destination is an explicit author action: `oca dictate --into
  notes|chapter:<key>` resolves through the owning engines (notes created
  on first use; chapters via the story engine, same keys as `attach`),
  and the TUI's `i` inserts into the open editor buffer (the author saves
  with Ctrl-S) while `n` appends to `notes/dictation.md`. A TUI
  transcription job touching nothing but the sidecar is asserted directly
  in tests.
- **Rule 4 stance until Phase 8**: `--clean` / `--prose` LLM cleanups are
  **previews only** — streamed to stdout (CLI) or into the panel (TUI) for
  review, and *never* written anywhere by themselves. The CLI refuses
  `--clean/--prose` combined with `--into` outright ("review the cleanup
  first"), and the cleanup keys appear in the TUI footer only when a
  language model is installed. There is no proposal machinery in 7B:
  applying cleaned text is still the author pressing `i`/`n` or typing
  `--into` themselves. *(Update, Phase 8: the proposal machinery now
  exists in `oca-core` — ADR-0027 — so the "apply the cleanup" path
  becomes a proposal with a pre-apply auto-snapshot (Phase 6 hook) as
  soon as the universal proposal UX lands in Phase 9. Until then the
  preview-only stance stands, unchanged.)* **Delivered, Phase 9
  (ADR-0028):** the universal proposal UX exists — `p` panel, `oca
  proposal apply`, Edit via `ProposalRevise` — so dictation cleanup
  *can* become a proposal when dictation next changes. Phase 9 itself
  did not rework dictation: cleanup output is still a preview, and
  applying it is still the author's explicit `i`/`n`/`--into` press.

## Consequences

- Exit criteria are enforced in four places, all against fakes/mocks:
  `oca-ai/tests/dictation.rs` (load → stream → cancel → clean error
  paths), `oca-audio/tests/dictation.rs` (record → transcribe → insert,
  "never auto-inserts"), TUI tests (keys, preview, cancel, no-runtime
  guidance), and `oca-cli/tests/cli.rs` (full `oca dictate` flow with the
  test binary acting as `whisper-cli` — child-only env, so a normal test
  run never misfires).
- Deleting the runtime pack or every whisper model disables transcription
  with one actionable sentence; nothing else in OCA changes (rule 3).
- Whisper logs are separate from the LLM log
  (`~/.oca/logs/whisper.log`); the transcript itself never lands in a log
  (A7 content gate) because the real runtime writes it to the named file,
  not stdout.
- **Supersedes ADR-0022's "symphonia formats arrive with Phase 7B
  dictation"**: the roadmap gives 7B no import or decode work — takes
  stay WAV-only through 7B, and audio format import belongs with
  import/export (Phase 15). Nothing in dictation decodes anything but
  the WAVs `oca-audio` itself recorded.
- Real-time streaming transcription, captions, timestamps (`-nt`), and
  speaker diarization are out of scope for v1; one take = one
  transcription run.
