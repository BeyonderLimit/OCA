# Audio device checklist (manual, per OS)

CI cannot test microphones or speakers. Phase 4's *logic* (WAV encode/decode,
sidecar lifecycle, id-based attachment, read-only refusal, quiet failure on
device-less machines) is proven by the engine's mock-host tests and the CLI
e2e tests; what only hardware can prove is checked by hand on each platform
before the phase is called done there.

Run on **Linux**, **macOS**, and **Windows**, each on a machine with real
audio hardware, using a scratch copy of a sample project:

1. **Doctor is quiet and honest.** `oca doctor` shows
   `✓ Audio subsystem — input ✓ <rate> Hz × <n> ch · output ✓ …`, and stderr
   is empty at the default log level. (On a machine with no sound card, the
   line must be `✗` for the missing direction — still empty stderr.)
2. **Record from the CLI.** `oca audio record --title "device check"` →
   speak, press Enter → `saved device-check (take_…, ~Ns)` with N > 0.
   `oca audio list` shows the take, unattached.
3. **Play it back.** `oca audio play device-check` — you hear the take; the
   command returns when playback has finished (not instantly, not late).
4. **TUI round trip.** Open a chapter, press `A` (panel), `r`, speak, `s` →
   take listed; `Enter` plays it; `a` attaches it to the chapter.
5. **Song demos.** Open a song's `lyrics.md`, `A`, record, `s` → the take
   lands in `music/songs/<slug>/demos/` and `a` attaches it to the song.
6. **Pause/resume is gapless.** TUI: `r`, speak, `p`, wait ~5 s, `p`, speak,
   `s` → the saved take plays back as two consecutive segments with no
   captured-time gap during the pause (duration ≈ total speaking time).

Delete the test recordings afterwards (`oca audio delete <stem>`).
