# Requests for Comment

temper-git uses RFCs for design proposals ahead of implementation.

- Write an RFC when a non-trivial new feature, protocol, or entity is
  about to be built.
- RFC is the HOW; paired ADR (if needed) is the WHY.
- Format: `NNNN-short-title.md`, sequential numbering.

## Open for review

- [0001-temper-git-v1-architecture.md](0001-temper-git-v1-architecture.md) —
  v1 concrete architecture: entity model, WASM integrations, kernel
  deltas (HttpEndpoint + streaming WASM I/O), storage substrate
  (per-repo libSQL + GCS-backed WAL; see
  [ADR-0004](../adr/0004-per-repo-libsql-gcs.md)), deployment topology,
  and the four-phase delivery plan. Amended 2026-04-21 to land
  ADR-0004.

## Accepted

(none)

## Rejected

(none)
