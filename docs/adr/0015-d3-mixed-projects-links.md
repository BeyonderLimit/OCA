# ADR-0015: Mixed projects in v1; cross-project links deferred (D3)

- Status: Accepted (decision) · Date: 2026-10-08 · Code: **D3**
- Source: `OCA-amendments.md` §5 D3

## Decision

One project may be *mixed* (story + music in the same directory), so song↔chapter links are in-project and use ids. Cross-project links are **not supported in v1**; the `oca://<project_id>/…` form is reserved so adding them later is not a format break. `~/.oca/projects.toml` is a simple registry for `oca list`/recent projects.

## Consequences

- A shared Artist Bible across projects ("Universe") is post-v1.
- No cross-project resolution code in v1 — links with foreign ids are invalid.
