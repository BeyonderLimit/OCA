# ADR-0022: Audio takes — WAV + sidecar files, id-based attachment, quiet devices

- Status: Accepted · Date: 2026-10-15 · Code: Phase 4
- Source: `OCA-implementation.md` Phase 4; `OCA-build.md` §10; roadmap v2
  amendments (Creative Note record)

## Context

Phase 4 adds *takes*: short audio recorded against a chapter, a song, or the
project. The standing rules apply and one new problem appears. Files are
canonical (no database until Phase 5), AI is optional (v1 ships no melody or
dictation work), the TUI holds zero business logic — and unlike every
subsystem so far, audio touches **hardware**, which cannot run in CI and on
some machines prints directly to stderr.

## Decision

### Take = WAV + sidecar YAML, both plain files

A take is `<stem>.wav` (16-bit PCM; `hound` writes, cpal captures) plus
`<stem>.yaml` carrying `take_id: take_<ULID>` (immutable — rename moves files
only), title, created (RFC 3339), sample_rate/channels/duration_ms,
`checksum: sha256:…` (the A1 snapshot-by-reference contract), and optional
`attached:` / `note:` blocks. Takes live in `audio/`, or for song demos in
`music/songs/<slug>/demos/`. Bare WAVs without sidecars are ignored — the id
is the link anchor and must never be invented by heuristics.

### Attachment is a stored id, resolved through the owning engine

`attached:` holds an internally-tagged block (`kind: chapter|song|project` +
`id:`). The target's files are never modified — attaching is a sidecar write —
and the id is validated through `oca_story::Story` / `oca_music::Music` at
read time, so a stale target is a reported error, never a silent phantom.
CLI and TUI targets: `chapter:<key>`, `song:<key>`, `project`. A song without
`id:` in its `song.yaml` is rejected for attachment (the id is the anchor).

### Creative Note fields live in the sidecar

`note:` carries text/lyrics/tempo (editable in v1) and `melody:` (reserved —
only future AI operations may write it; v1 preserves it losslessly through
the `remove_key`-based patching added to `oca_model::LosslessYaml`).

### Device access goes through one trait; probes never enumerate

`AudioHost` (default formats + capture + playback) has the cpal
implementation and a deterministic mock used by every test — the
AI-optional/testability pattern extended to hardware. `doctor` probes only
the **default** devices' formats. Hint *enumeration* and any probe on a
device-less machine make libasound print `ALSA lib …` lines straight to
stderr, breaking doctor's quiet contract — so on Linux the host refuses to
touch ALSA unless `/dev/snd` contains PCM nodes (those machines fail
anyway). Verified on real hardware: record → save → attach → play with
**zero stderr bytes**.

## Consequences

- `oca audio record/play` need no AI, no network, no database; the CLI
  lifecycle (list/attach/note/rename/detach/delete) is covered by e2e tests
  without touching a device.
- Deleting `.oca/cache` never touches takes; a take's links survive renames.
- WAV-only in v1; symphonia formats arrive with Phase 7B dictation.
- macOS/Windows (and any Linux quirk beyond the `/dev/snd` gate) device
  verification is the manual checklist in `docs/audio-device-checklist.md`
  — CI cannot test hardware.
