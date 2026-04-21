# temper-git — Agent Guide

## What you're working on

**temper-git is a self-hosted, Temper-native, GitHub-compatible source-control
management server.** Git wire protocol + GitHub REST v3 + GraphQL v4, all
served out of IOA entities and WASM protocol handlers. It's a *self-contained
product* — its own repo, own image, own deployment. Every bit of git — blob,
tree, commit, ref, tag, pull request, review — is a first-class Temper IOA
entity.

Read [VISION.md](VISION.md) first. Then the ADRs. Then this file's discipline
section. Then the RFCs. Do not start code until you've read all of that.

## Hard rules (non-negotiable)

1. **Temper discipline end to end.** This project vendors Temper as the
   `temper/` submodule and follows every Temper convention: IOA TOML specs,
   Cedar policies, WASM tools, OData as the only API surface for internal
   state, TigerStyle for any Rust we write, ADRs/RFCs for every
   non-trivial decision. There is no escape hatch. If your first instinct
   is "let's just add a Rust HTTP handler for this one case," stop and
   write an ADR.

2. **Byte-exact git compatibility is the product.** Every Blob / Tree /
   Commit / Tag object must hash identically to what `git hash-object`
   produces. Every pack we emit must pass `git fsck` on the client side.
   Every wire-protocol response must be byte-indistinguishable from
   git-http-backend's output. If a third-party tool works against
   github.com, it works against temper-git.

3. **No bare repos on disk.** Blobs/Trees/Commits/Refs are IOA rows. When
   `git clone` arrives, a WASM integration walks the Commit chain and
   streams a pack out of entity state. When `git push` arrives, a WASM
   integration parses the incoming pack into entity writes. One source of
   truth: Temper.

4. **Every protocol handler is a WASM integration.** Not a host-side Rust
   extension, not a CGI script, not an nginx location-block trick. The
   handlers that serve `/info/refs`, `/git-upload-pack`,
   `/git-receive-pack`, `/api/v3/*`, and `/api/graphql` are WASM modules
   dispatched via Temper's integration machinery.

5. **Cedar gates every mutation.** `Repository.WriteFile`, `PullRequest.Merge`,
   `Ref.Update`, every action has a Cedar policy. A `GitToken` is a
   bearer credential that maps to a `Principal::Customer` or
   `Principal::Agent`; its Scope bounds what actions pass Cedar.

6. **Wire-protocol compatibility is tested, not asserted.** The test harness
   includes real `git` CLI against a live temper-git and verifies
   round-trips (`git push` → `git clone` → `git log` shows the same
   commits with the same hashes). PRs that break any round-trip test do
   not merge.

7. **GitHub API compatibility is also tested.** For every REST endpoint we
   ship, there's a test that exercises it with `gh` CLI or `curl` against
   github.com's documented response shape, and then the same call
   against temper-git. Responses must match in structure (field names,
   types, and required fields) so ecosystem tools don't crash.

## Discipline

Mirror Temper's discipline exactly:

- **ADRs** in [docs/adr/](docs/adr/). MADR format. Write one when a decision
  is viable-to-alternate, costly to reverse, or crosses components.
- **RFCs** in [docs/rfc/](docs/rfc/) for design proposals ahead of
  implementation. Every major new protocol/endpoint/entity gets an RFC.
- **Research** in [docs/research/](docs/research/) for external
  investigations (git internals, GitHub API behavior, pack format
  quirks) that feed decisions.
- **Evolution chain** in [evolution/](evolution/) — O-P-A-D-I records for
  runtime observations we act on.
- **Proofs** in [proofs/](proofs/) — TLA+ / IOA specs we own + their
  verification status. The git object graph has real invariants
  (acyclic commit DAG, tree-hash integrity, ref-exists-if-named); these
  belong in TLA+.
- **Specs** in [specs/](specs/) — IOA TOML for every entity temper-git
  owns.

## The temper/ submodule

We do not fork Temper's kernel. We vendor it at a pinned commit via git
submodule at `temper/`. Our product is:
- `temper/` — the machine tool, pinned upstream
- `specs/`, `policies/`, `wasm-modules/`, `deploy/` — the temper-git OS app

When Temper upstream ships a feature we need (e.g., first-class `HttpEndpoint`
IOA entity for path-prefix routing), we either:
a) cherry-pick / propose upstream the feature and bump the submodule, or
b) file a PR against upstream and wait.

We do **not** patch temper/ files in place on our branch. If temper/ ever
diverges from upstream, that's an outage waiting to happen.

## Coding guidelines

Inherit Temper's TigerStyle verbatim, plus a few additions:

- 70-line function cap, 500-line file cap.
- Average 2 assertions per function.
- Explicit limits on every loop / queue / buffer.
- No `unwrap()` in non-test code. No `unsafe`.
- No comments that restate what code does; only `why` comments.

Additions specific to temper-git:

- **Canonical git object serialization is authoritative.** If you're tempted
  to write your own "close enough" version of a git blob/tree/commit
  serializer, stop. Use `git cat-file --batch` in the test harness to
  verify byte equality, always.
- **SHA-1 is intentional.** Git uses SHA-1; we use SHA-1. Do not "upgrade"
  to SHA-256 in any code path that emits to the wire, even though SHA-1 is
  cryptographically broken for collision resistance. Git compatibility
  requires it. SHA-256 git exists upstream; we'll adopt when git defaults
  change.
- **Protocol handlers respect timeouts.** `git clone` of a large repo may
  take minutes. WASM integration timeouts must be configurable per-endpoint;
  default 300s, never less.
- **Streaming is mandatory for pack handlers.** Pack uploads/downloads can be
  hundreds of MB. The WASM integration must stream — we do not buffer a
  full pack in WASM memory. Use chunked transfer encoding and the host's
  streaming http_call surface.

See [CODING_GUIDELINES.md](CODING_GUIDELINES.md) for the full list.

## Workflow

- Before starting non-trivial work, read or write an RFC.
- Before making a decision that alternates viably, write an ADR.
- Before changing any IOA spec, run Temper's L0–L3 verification cascade
  locally.
- Every PR against temper-git must include round-trip tests against real
  `git` CLI if it touches protocol handlers.
- Merges to `main` require a green harness result.
- When you encounter something surprising in production telemetry, open an
  `Observation` record.

## Things not to do

- Do not shell out to `git` binaries at runtime. Everything is either IOA
  entity manipulation or in-WASM pack parsing/emission.
- Do not hand-write OpenAPI specs. The GitHub REST surface is declared as
  OData actions + a compatibility-shim WASM.
- Do not add Rust host extensions to Temper. If temper/ needs something,
  propose it upstream.
- Do not create read replicas of repo state (Memory entities, file-system
  caches, etc.). One source of truth.
- Do not skip the round-trip test when you edit a protocol handler.

## References

- [VISION.md](VISION.md) — why this exists.
- [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
- [docs/adr/0002-temper-native-scm.md](docs/adr/0002-temper-native-scm.md)
- [docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md)
- [docs/adr/0004-per-repo-libsql-gcs.md](docs/adr/0004-per-repo-libsql-gcs.md)
- [docs/rfc/0001-temper-git-v1-architecture.md](docs/rfc/0001-temper-git-v1-architecture.md)
- [temper/CLAUDE.md](temper/CLAUDE.md) — Temper kernel discipline we inherit.
- Git object format reference:
  https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
- Git smart-HTTP wire protocol:
  https://git-scm.com/docs/http-protocol
- GitHub REST v3 reference:
  https://docs.github.com/en/rest
- GitHub GraphQL v4 reference:
  https://docs.github.com/en/graphql
