# Contributing to temper-git

Read [CLAUDE.md](CLAUDE.md) first — it sets the discipline for agents and
humans working in this repo.

## Before you write code

1. Find or open an Issue. If there isn't one, the work is premature.
2. Check whether the decision needs an ADR. Rule of thumb: if a reviewer
   could reasonably pick a different design, write the ADR first.
3. Check whether the implementation needs an RFC. Rule of thumb: if the
   work takes more than one session, write the RFC first.
4. For anything touching protocol handlers (wire-protocol WASMs,
   REST-compat WASMs), get the compat-gate tests outlined *before*
   writing the implementation. Test-first for byte-exact compat.

## Code discipline

- TigerStyle inherited from Temper. See [CODING_GUIDELINES.md](CODING_GUIDELINES.md)
  for the deltas specific to temper-git.
- 70-line function cap, 500-line file cap.
- No `unwrap()`, no `unsafe`. `?`-propagation only.
- No comments that restate what code does; only `why` comments.

## Tests

Every PR that touches a protocol handler MUST include:

1. **Hash-byte-match test.** Inputs → canonical bytes → `git hash-object`
   output comparison.
2. **Round-trip test.** `git clone → commit → push → clone → diff` via
   a live temper-git dev server.
3. **GitHub API shape test** (for REST-touching PRs). Fixture from
   `gh api ...` vs temper-git response, asserting structural equality.

PRs that skip any of these for "small changes" don't merge.

## Commits

- Conventional-commit-ish but not religiously: `feat:`, `fix:`,
  `refactor:`, `docs:`, `test:`, `chore:`.
- Every commit that changes protocol behavior must reference the ADR
  or RFC that authorized the change (`Refs: ADR-0003`, `Refs: RFC-0001`).
- No `--amend` on published commits without `-F` sign-off from another
  reviewer.

## Reviews

- Two signoffs required on anything that touches
  `specs/`, `wasm-modules/git_*/`, or `wasm-modules/github_*/`.
- Docs-only PRs (`docs/`, `CLAUDE.md`, `README.md`) can land with one
  signoff.
- Kernel-level changes go into Temper first, then the submodule
  pointer here bumps — never patch `temper/` files on a temper-git
  branch.

## Getting started

See the [README quickstart](README.md#quickstart-local) for a
three-terminal local setup. To dig into design:

1. Read [VISION.md](VISION.md) and the ADRs.
2. Skim [docs/rfc/0001-architecture.md](docs/rfc/0001-architecture.md)
   and [docs/rfc/0002-push-and-clone.md](docs/rfc/0002-push-and-clone.md).
3. Open an issue or a draft PR.
