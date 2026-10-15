# ADR-0007: Runtime packs, early signing, locked-down llama-server

- Status: Accepted · Date: 2026-10-08 · Code: **A7** (decision D8)
- Source: `OCA-architechture.md` Part II §23 A7 · rationale: `OCA-amendments.md` §1 A7 · process: `docs/signing-and-release.md`

## Decision

- **Core binary is self-contained**: Rust app + bundled SQLite (FTS5); `sqlite-vec` statically linked and registered at startup (verify in a Phase 5 spike).
- **Runtime Packs** are optional, per-platform downloads (`llama-server` CPU/Metal/CUDA/Vulkan, whisper), installed via `oca runtime install --from <file>` — offline side-loading is first-class. Each pack/model has a SHA-256 manifest verified by `doctor`.
- `llama-server` binds `127.0.0.1` on an ephemeral port with a random per-session API key.
- Signing/notarization proven early: **release dry-run in CI from Phase 0** (cargo-dist), runtime-pack spike in Phase 7 — not first attempted at Phase 18.
- Logs never contain manuscript text unless `-vv --log-content`; **no telemetry, ever**.

## Consequences

- `.github/workflows/release.yml` + `dist-workspace.toml` exist from Phase 0.
- Every future log statement must respect the content gate.
