# OCA — Offline Creative Assistant

Local-first creative workspace for stories, music, and audio, with optional
offline AI. Everything lives in plain Markdown and YAML on your disk; OCA
indexes, analyzes, and safety-nets it — it never sends a byte anywhere.

**Status: Phases 0–11 implemented locally** (workspace, story engine, song
analysis, audio takes, search, snapshots, offline AI with proposals, dictation,
RAG/hybrid search). CI verification pending. See `AGENTS.md` for the phase
ledger and `OCA-roadmap.md` for what's next.

Three rules shape everything:

- **Files are canonical.** Markdown/YAML on disk is the source of truth; the
  SQLite cache in `.oca/cache/` is an index you can delete and rebuild
  (`oca reindex`). `.oca/state/` (snapshots, proposals, recovery) is precious.
- **AI is optional.** Every feature works with every model file deleted.
  Nothing AI-flavored ever writes to your files: it produces *proposals* you
  accept or reject, snapshotted first, undoable.
- **No network.** No cloud, no telemetry, no downloads at runtime. Model
  runtimes and model files are side-loaded by you, or absent.

## Build

```sh
cargo build --release          # -> target/release/oca
scripts/smoke.sh               # the full local gate: fmt + clippy + tests + binaries
```

Requirements: a recent stable Rust toolchain. Linux, macOS, and Windows are
supported; audio capture needs a working input device, everything else does
not.

## Quick start

```sh
oca new "Tideways"             # create + register a project
cd Tideways
oca doctor                     # health of every subsystem (warnings are fine)
```

Write:

```sh
oca chapter new "The Letter"                 # -> manuscript/001-the-letter.md
oca chapter append 001-the-letter "The tide came in all that night…"
oca chapter show 001-the-letter              # exact bytes on stdout
oca open                                     # the TUI editor (tree + editor + panels)
```

Documents resolve **by id, filename stem, or title** — whatever you saw
printed. Ambiguous stems refuse and name the candidates.

Ask the story engine (deterministic, no AI involved):

```sh
oca story who Elena            # identity, relationships, resolved current state
oca story where Ellie          # carry-forward state; aliases count
oca story chapters-with "the girl"
oca story before 008-the-trial --sense story   # or: --sense narrative
```

Characters live in `characters/*.md` with `id:`/`name:`/`aliases:` and
chapter-keyed `states:`; events live in `outline/timeline.md`.

Analyze a song (structure, rhyme scheme, syllable counts — offline):

```sh
oca song new "Lyrics for the tide"
oca song check lyrics-for-the-tide
oca song save-version lyrics-for-the-tide --label "first pass"
```

Record a voice note and attach it (needs a microphone; mock-free and offline):

```sh
oca audio record --attach chapter:001-the-letter
oca audio list
```

Never lose work:

```sh
oca snapshot create "before edits"
oca snapshot list
oca snapshot diff 1            # line-level patch against the working files
oca snapshot restore 1         # exact bytes back; audio takes are never touched
```

Find things — keywords always, vectors too when an embedding model is
installed (`· hybrid` marks fused results, `[reference]` marks imported,
untrusted material):

```sh
oca search tide lamplight      # words AND together; the last word matches as a prefix
oca search tag:character:elena # tag lookup
oca import ~/research/harbor-code.md   # Markdown / text / EPUB -> library/ with provenance
oca reindex                    # rebuild the cache; also syncs embeddings when installed
```

## Optional AI (all offline)

AI features need a side-loaded llama.cpp runtime and model files — or nothing
at all, via the built-in scripted stand-in:

```sh
[ai]
mock = true        # in ~/.oca/config.toml — canned deterministic output, no runtime
```

With a real runtime (`oca runtime install --from <pack.tar.gz>`, models in
`~/.oca/models/{llm,embeddings,whisper}`):

```sh
oca ai continue 002-the-harbor         # records a proposal — files stay untouched
oca ai rewrite 001-the-letter --start 40 --end 120
oca ai review 002-the-harbor           # a report, printed, never written
oca proposal list                      # pending / edited / accepted / rejected / stale
oca proposal apply prop_…              # auto-snapshot first; exact undo via snapshot restore
oca dictate take_…                     # whisper transcription with no LLM installed
```

One heavy model resident at a time (RAM is scarce); embeddings run their own
slot and degrade to keyword search when absent or busy. AI output arriving
after your edits goes `stale` — it never clobbers what you wrote.

## Where things live

```
Tideways/
├── project.yaml            # project id, title, format version
├── manuscript/             # NNN-slug.md chapters
├── characters/  world/  outline/   # story bible + timeline
├── music/songs/<slug>/     # song.yaml + lyrics.md + versions/
├── library/                # imported references (untrusted context, A8)
├── notes/
├── audio/                  # takes: <stem>.wav + provenance sidecar
└── .oca/
    ├── cache/oca.db        # derived index — safe to delete, `oca reindex` rebuilds
    ├── state/              # snapshots, proposals, recovery journal — precious
    └── logs/
```

Links between documents use immutable frontmatter ids, never file paths —
rename freely; external moves still resolve.

## Development

```sh
cargo build                                # debug -> target/debug/oca
cargo test                                 # unit + CLI integration tests
cargo fmt --all --check                    # CI gate
cargo clippy --workspace --all-targets -- -D warnings   # CI gate
scripts/smoke.sh                           # all of the above + both binaries
scripts/perf.sh                            # perf harness (A10 budgets)
python3 scripts/generate-fixture.py        # 500k-word perf fixture (never committed)
```

- Tests live beside the code; `crates/oca-cli/tests/` runs the real binary in
  an isolated HOME. All AI-path tests use mock models.
- Design docs (authoritative, precedence in `AGENTS.md`): `OCA-architechture.md`
  (stack; note the filename quirk), `OCA-roadmap.md` (phase order),
  `OCA-implementation.md` (executable plan), `OCA-build.md` (features and file
  formats), `OCA-amendments.md` (ADRs source). Short ADRs in `docs/adr/`.
- Extras: `docs/signing-and-release.md` (release process),
  `docs/audio-device-checklist.md` and `docs/runtime-pack-checklist.md`
  (manual verification), `examples/blackwood` (the committed sample novel the
  tests run against).

## License

MIT OR Apache-2.0 — see `LICENSE-MIT` and `LICENSE-APACHE`.
