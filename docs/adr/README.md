# Architectural Decision Records

temper-git uses MADR (Markdown Architectural Decision Records).

- Write an ADR when a decision is viable-to-alternate, costly to reverse,
  or crosses components.
- Don't write one for implementation details — use an RFC instead.
- Format: `NNNN-short-title.md`, where NNNN is sequential.

## Accepted

- [0001-temper-git-mission.md](0001-temper-git-mission.md) — temper-git is a
  self-contained, Temper-native, GitHub-compatible SCM. Supersedes the
  nginx+CGI git host in dark-helix.
- [0002-temper-native-scm.md](0002-temper-native-scm.md) — SCM state is IOA
  entities; protocol handlers are WASM integrations. No host-side Rust
  extensions.
- [0003-byte-exact-git-compat.md](0003-byte-exact-git-compat.md) — byte-exact
  git compatibility is a product guarantee, enforced by CI.

## Proposed

(none)

## Rejected

(none)

## Superseded

(none)
