# ADR-0013: Comments as sidecar YAML (D1)

- Status: Accepted (decision) · Date: 2026-10-08 · Code: **D1**
- Source: `OCA-amendments.md` §5 D1

## Decision

Comments are sidecar files `comments/<document_id>.yaml` with W3C text-quote anchoring — visible and portable with the project, never inside manuscript text.

## Consequences

Implements ADR-0004 (A4). Zero-leak export guarantee covers annotations and user HTML comments alike.
