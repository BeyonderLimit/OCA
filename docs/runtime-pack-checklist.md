# Runtime pack checklist (manual, per OS)

CI has no llama.cpp and no signing identity, so the Phase 7 *logic* (pack
verification, loopback spawn, streaming, cancel, crash isolation) is proven
by the engine's fake-server tests and the CLI e2e tests; what only a real
`llama-server` and a real OS can prove is checked by hand on each platform
before the phase is called done there — including the **packaging spike**
from the Phase 7 v2 amendments (a signed/notarized pack runs on each OS).

Run on **Linux**, **macOS**, and **Windows**:

1. **Build a pack** containing a real `llama-server` for the platform:
   ```
   pack/
     bin/llama-server        (+ .exe on Windows)
     manifest.json           name, version, platform ("<os>-<arch>"), files (sha256)
   tar czf llama-pack-1.0.0.tar.gz -C pack .
   ```
   `manifest.json` lists every file's SHA-256. (Until release tooling ships,
   produce it by hand; `oca_ai::pack_writer` is the reference layout.)
2. **Side-load it offline.** Disconnect the network, then
   `oca runtime install --from llama-pack-1.0.0.tar.gz` →
   `installed llama-pack 1.0.0 — N file(s) verified`. On unix the `bin/`
   contents are executable (0755).
3. **Doctor agrees.** `oca doctor` → `✓ Model runtime — <path> · one heavy
   model at a time`, `✓ Models — …` (a `.gguf` in `~/.oca/models/llm`),
   `Hardware — … → <tier>`, stderr empty.
4. **Tamper detection.** Edit one byte of the installed binary, then
   `oca runtime list` → `⚠ llama-pack 1.0.0 — <file> changed since install`
   and exit code 1. Restore the file → `✓` and exit 0.
5. **Wrong platform is refused.** Rebuild the pack with
   `"platform": "something-else"` → install refuses with both platforms named
   in the message.
6. **TUI end-to-end.** `oca open` → `I`: model, Context, GPU, Threads, and
   the governor policy render; `l` loads (runtime starts, panel reaches
   ready); `t` streams a probe with a live bar; `[ESC] Stop` interrupts
   within a second and the model stays loaded; `u` frees the memory.
7. **Crash isolation.** Kill the `llama-server` process externally (Task
   Manager on Windows) while the panel is loaded: the next probe reports
   `model runtime exited unexpectedly…` in the panel — OCA keeps running,
   and `l` starts a fresh runtime.
8. **GPU offload (if hardware allows).** Set `gpu_layers` in
   `~/.oca/config.toml` `[ai]`, reload: the panel shows `GPU: enabled (N
   layers)` and generation is faster; `doctor`'s tier recommendation is
   unchanged (it is RAM-based by design, ADR-0008).
9. **No AI at all.** Move `~/.oca/runtime` and `~/.oca/models` away and
   restart: every OCA feature still works; the `I` panel explains how to
   enable AI (rule 3).

Signing/notarization of packs (macOS notarization, Windows Authenticode)
stays open until a signing identity exists; `docs/signing-and-release.md`
tracks that thread.
