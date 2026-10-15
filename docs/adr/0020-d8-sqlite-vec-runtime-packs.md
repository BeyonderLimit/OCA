# ADR-0020: Embedded sqlite-vec + runtime packs (D8)

- Status: Accepted (decision; static linking pending Phase 5 spike) · Date: 2026-10-08 · Code: **D8**
- Source: `OCA-amendments.md` §5 D8

## Decision

- `sqlite-vec` is statically linked into the core binary and registered at startup — no loadable extension shipped per platform (verify with the crate in a **Phase 5 spike**; fall back to extension loading if it fails).
- Runtime packs (llama-server, whisper) are optional per-platform downloads with SHA-256 manifests; side-loading via `oca runtime install --from <file>`.

## Consequences

- One SQLite DB per project holds metadata + FTS5 + vectors — no second `vectors.db`, no vector server, no Docker.
- Core binary stays self-contained and offline-installable.
