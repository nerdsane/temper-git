# temper-git — Agent Guide

## What you're working on

**temper-git is a Temper-native, GitHub-compatible version-control
server.** Git wire protocol + GitHub REST v3 (+ GraphQL v4, later),
served out of IOA entities and WASM protocol handlers. Every git
object — blob, tree, commit, ref, tag, pull request, review — is a
first-class IOA entity.

Read [VISION.md](VISION.md) first. Then the ADRs. Then this file's
discipline section. Then the RFCs.

## Hard rules (non-negotiable)

1. **Temper discipline end to end.** Vendored Temper lives in the
   `temper/` submodule. Every convention applies: IOA TOML specs,
   Cedar policies, WASM tools, OData as the only API surface for
   internal state, TigerStyle for any Rust we write, ADRs/RFCs for
   every non-trivial decision. If your first instinct is "let's just
   add a Rust HTTP handler for this one case," stop and write an ADR.

2. **Byte-exact git compatibility is the product.** Every Blob / Tree /
   Commit / Tag object must hash identically to what `git hash-object`
   produces. Every pack we emit must pass `git fsck` on the client
   side. Every wire-protocol response must be byte-indistinguishable
   from `git-http-backend`'s output. If a third-party tool works
   against github.com, it works against temper-git.

3. **No bare repos on disk.** Blobs/Trees/Commits/Refs are IOA rows.
   When `git clone` arrives, a WASM integration walks the Commit chain
   and streams a pack out of entity state. When `git push` arrives, a
   WASM integration parses the incoming pack into entity writes. One
   source of truth.

4. **Every protocol handler is a WASM integration.** Not a host-side
   Rust extension, not a CGI script, not an nginx location-block trick.
   The handlers that serve `/info/refs`, `/git-upload-pack`,
   `/git-receive-pack`, `/api/v3/*`, and `/api/graphql` are WASM
   modules dispatched via Temper's integration machinery.

5. **Cedar gates every mutation.** `Repository.WriteFile`,
   `PullRequest.Merge`, `Ref.Update` — every action has a Cedar policy.
   A `GitToken` is a bearer credential that maps to a principal; its
   `Scopes` bound what actions pass Cedar.

6. **Wire-protocol compatibility is tested, not asserted.** The test
   harness invokes real `git` against a live temper-git and verifies
   round-trips (`git push` → `git clone` → `git log` shows the same
   commits with the same hashes). PRs that break any round-trip test
   do not merge.

7. **GitHub API compatibility is also tested.** For every REST endpoint
   we ship, there's a test that exercises it with `gh` or `curl`
   against github.com's documented response shape, then the same call
   against temper-git. Responses must match structurally (field names,
   types, required-field presence).

## Discipline

- **ADRs** in [docs/adr/](docs/adr/). MADR format. One per decision
  that is viable-to-alternate, costly to reverse, or crosses
  components.
- **RFCs** in [docs/rfc/](docs/rfc/) for design proposals ahead of
  implementation.
- **Research** in [docs/research/](docs/research/) for external
  investigations that feed decisions.
- **Proofs** in [proofs/](proofs/) — TLA+ / IOA specs we own + their
  verification status. The git object graph has real invariants
  (acyclic commit DAG, tree-hash integrity, ref-exists-if-named);
  these belong in TLA+.
- **Specs** in [specs/](specs/) — IOA TOML for every entity temper-git
  owns.

## The temper/ submodule

Temper is vendored at a pinned commit via git submodule at `temper/`.
The product is:
- `temper/` — the kernel this project builds on
- `specs/`, `policies/`, `wasm-modules/` — the temper-git app bundle

Do **not** patch `temper/` files in place on a feature branch. If a
kernel change is needed, it goes into Temper proper and the submodule
pointer bumps.

## Coding guidelines

Inherit Temper's TigerStyle verbatim, plus a few additions:

- 70-line function cap, 500-line file cap.
- Average 2 assertions per function.
- Explicit limits on every loop / queue / buffer.
- No `unwrap()` in non-test code. No `unsafe`.
- No comments that restate what code does; only `why` comments.

Additions specific to temper-git:

- **Canonical git object serialization is authoritative.** If you're
  tempted to write your own "close enough" version of a blob/tree/
  commit serializer, stop. Use `git cat-file --batch` in the test
  harness to verify byte equality, always.
- **SHA-1 is intentional.** Git uses SHA-1; we use SHA-1. Do not
  "upgrade" to SHA-256 in any code path that emits to the wire, even
  though SHA-1 is cryptographically broken for collision resistance.
  Git compatibility requires it. SHA-256 git exists; we adopt when
  git defaults change.
- **Protocol handlers respect timeouts.** `git clone` of a large repo
  may take minutes. WASM integration timeouts are configurable
  per-endpoint; default 300s, never less.
- **Streaming is mandatory for pack handlers.** Pack uploads/downloads
  can be hundreds of MB. The WASM integration must stream — we do not
  buffer a full pack in WASM memory.

See [CODING_GUIDELINES.md](CODING_GUIDELINES.md) for the full list.

## Workflow

- Before starting non-trivial work, read or write an RFC.
- Before making a decision that alternates viably, write an ADR.
- Before changing any IOA spec, run Temper's L0–L3 verification
  cascade locally.
- Every PR that touches protocol handlers must include a round-trip
  test against real `git`.

## Things not to do

- Do not shell out to `git` binaries at runtime. Everything is either
  IOA entity manipulation or in-WASM pack parsing/emission.
- Do not hand-write OpenAPI specs. The GitHub REST surface is declared
  as OData actions + a compatibility-shim WASM.
- Do not add Rust host extensions to Temper for temper-git-specific
  needs. If the kernel needs something, it's a generic primitive.
- Do not create read replicas of repo state (Memory entities,
  file-system caches, etc.). One source of truth.
- Do not skip the round-trip test when you edit a protocol handler.

## References

- [VISION.md](VISION.md) — why this exists.
- [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
- [docs/adr/0002-temper-native-version-control.md](docs/adr/0002-temper-native-version-control.md)
- [docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md)
- [docs/rfc/0001-architecture.md](docs/rfc/0001-architecture.md)
- [docs/rfc/0002-push-and-clone.md](docs/rfc/0002-push-and-clone.md)
- [temper/CLAUDE.md](temper/CLAUDE.md) — Temper kernel discipline we
  inherit.
- Git object format: https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
- Git smart-HTTP: https://git-scm.com/docs/http-protocol
- GitHub REST v3: https://docs.github.com/en/rest
- GitHub GraphQL v4: https://docs.github.com/en/graphql
