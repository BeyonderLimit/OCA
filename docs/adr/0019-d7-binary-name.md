# ADR-0019: Binary and product name is `oca` (D7)

- Status: Accepted (decision) · Date: 2026-10-08 · Code: **D7**
- Source: `OCA-amendments.md` §5 D7

## Decision

The binary and product name is **`oca`** (not `creative-studio`). Commands: `oca new`, `oca open`, `oca doctor`, `oca reindex`, `oca backup`, `oca migrate`, `oca adopt`, `oca runtime install`, `oca song check`. Hidden dir is `.oca/` (replacing the earlier `.writer/`/`.studio/` spellings).

## Consequences

All docs, clap config, installers, and user-facing strings use `oca`. Workspace package is `oca-cli` producing the `oca` binary.
