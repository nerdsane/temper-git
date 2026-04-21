# ADR-0001: temper-git is a self-contained, Temper-native, GitHub-compatible SCM

## Status

Accepted — 2026-04-21. Supersedes the temper-git pod in the dark-helix
factory (nginx + fcgiwrap + git-http-backend over a PVC of bare repos).

## Context

The dark-helix factory hosts three agent cohorts — operators, users, builders
— on Temper Managed Agents. Every cohort needs to read and write source code:
users maintain kafka client apps, builders turn operator `Observation`s into
pull requests against the factory's own specs, operators draft playbook
updates. The existing git host in the factory cluster is an nginx pod with
fcgiwrap wrapping `git-http-backend`, reading and writing bare repos on a
PersistentVolumeClaim.

That pod works for standard git clients — `git clone`, `git push`, `git pull`
from a developer laptop all succeed. But it has three fundamental problems
for the factory:

1. **Agents can't write to it.** Agents run in WASM. WASM cannot open TCP
   sockets, cannot run a git binary, cannot implement the smart-HTTP pack
   upload protocol without ~1000 lines of bit-level parsing code that
   doesn't belong in a WASM guest. The practical consequence: every one of
   our agent tools that tries to mutate code returns `{error: "unsupported"}`
   and the PR-flow loop is only theoretical.

2. **Two sources of truth.** If we bolt a write API onto nginx (a CGI
   shim, or a REST layer), agents can now write via the shim while humans
   write via git-smart-HTTP. The bare repo on the PVC is one representation;
   every "audit trail" we build on top (PR entities, review entities, commit
   entities) lives in Temper as a second, out-of-band representation. Merge
   semantics, history integrity, and Cedar policy enforcement all drift
   between the two.

3. **Evolution chain lives in Temper; code lives outside Temper.** Our
   Observation / Problem / Analysis / PullRequest chain is Temper-native.
   The PR references a branch in a repo — but the repo is opaque to
   Temper. An agent cannot express "read the file this PR wants to change"
   without leaving the Temper OData surface. That breaks the Temper-only
   control plane rule (dark-helix ADR-0002).

Three options were seriously considered:

### Option A: Patch the existing nginx pod with a write-capable REST shim

Add an nginx `location /_internal/write_file/...` + a CGI shell script
that clones, commits, pushes on behalf of the agent. Cheapest option in
elapsed time (a day of work). Keeps git-smart-HTTP for humans.

Rejected because: two sources of truth persist. The PVC bare repo is
authoritative for git clients; the REST shim is authoritative for agents;
Temper entities for PullRequest/Review etc. are a third view. Every
future feature (branch protection, merge queue, webhook delivery) has to
handle all three surfaces consistently. That's the road to nowhere.

### Option B: Temper's `Memory` entity as the substrate

Use Temper's existing `Memory` + `MemoryVersion` entities as the
writable surface for agent-authored content. Each Memory row has a
`Path`, `Content`, version chain. Agents mutate via plain OData. Later,
a sync controller mirrors Memory rows → temper-git bare repos.

Rejected because: humans can't `git clone` a Memory. The git protocol
is still served by the nginx pod from bare repos, which are now a
derived second system that eventual-consistency mirrors from Memory.
The sync controller is hard to make bulletproof; partial syncs leave
the factory in a state where "the code" is different depending on
whether you're asking an agent or a human. Also Memory was designed
for agent-remembered-state (playbooks, instructions), not source code;
overloading it conflates "what the agent is told" with "what the
agent authors."

### Option C: Build a full Temper-native SCM as its own product (chosen)

Stand up temper-git as a self-contained project. Bundle Temper's
kernel. Write a GitHub-compatibility OS app that models every git
concept — blobs, trees, commits, refs, tags, pull requests, reviews,
tokens — as first-class IOA entities. Implement the git smart-HTTP
wire protocol as WASM integration modules that parse/emit packs
directly from entity state. Implement the GitHub REST v3 API as
another set of WASM integrations translating REST calls into OData
actions on those entities. No bare repos on disk. No second source of
truth.

## Decision

**We build temper-git as a self-contained product, separate from
dark-helix, bundling the Temper kernel as a submodule and implementing a
full GitHub-compatible SCM on top.** Every git object — blob, tree,
commit, tag, ref — is an IOA entity. Wire protocol and REST are WASM
integrations. Byte-exact git compatibility is mandatory: every object
emitted must hash identically to what `git hash-object` produces.

### What this commits us to

- **A sibling project** at `~/Development/temper-git/`, not a subdirectory
  of dark-helix. Own Cargo workspace, own Dockerfile, own K8s deployment,
  own libSQL + GCS-backed storage substrate (see
  [ADR-0004](0004-per-repo-libsql-gcs.md)), own release cadence.
- **The temper/ submodule** pinned at a specific upstream Temper commit.
  We do not fork Temper's kernel; we extend it via OS app + WASM.
- **Per-repo libSQL database + GCS-backed WAL.** See
  [ADR-0004](0004-per-repo-libsql-gcs.md). Self-hosted libsql-server
  for air-gapped GCP sandbox operation; swap-path to Turso Cloud via
  env-var change with zero code diff.
- **Byte-exact compatibility** — every third-party git tool must work
  against temper-git without modification. GitHub REST v3 response shapes
  must match github.com structurally.
- **Phased delivery** (see ADR-0003 and RFC-0001 for the cuts):
  - Phase 1 — Foundation: entity model, pack v2 emitter/parser, full
    upload-pack + receive-pack, core REST (contents, pulls, refs,
    merges), GitToken auth.
  - Phase 2 — Production hardening: delta compression, thin packs,
    webhooks, remaining REST endpoints, branch protection.
  - Phase 3 — Modern surface: GraphQL v4, OAuth apps.
  - Phase 4 — Migration: importer for existing bare repos from the old
    temper-git pod; decommissioning the old pod.

### What this does NOT commit us to

- A public-internet GitHub alternative. temper-git is a cluster-internal
  service; external exposure is a user's deployment choice.
- A GitHub web UI replica. Humans browse via the Observe UI's
  (separately-shipped) code-view widget or set up outbound read-only
  mirrors to github.com / Gitea.
- Git LFS support.
- Git "fork" semantics. Repos can be duplicated via an OData action, but
  forks aren't a first-class feature.

## Consequences

### Easier

- Agents in WASM get a writeable source-control surface with a three-line
  tool: `POST /api/v3/repos/{o}/{r}/contents/{p}`. No pack protocol in
  WASM. No special case for "the git server is different from
  everything else."
- The evolution chain terminates in code: Observation → Problem →
  Analysis → PullRequest → merged Commit, all in one substrate. An
  agent can walk the chain via OData without leaving Temper.
- Every mutation is Cedar-gated and trajectory-emitted. The
  verification cascade governs git operations the same way it governs
  any IOA transition.
- Mirror-compatibility is free: `git push --mirror` against a
  byte-exact server produces identical hashes. Humans keep read-only
  mirrors on github.com/Gitea for browsing, and those mirrors are
  maintained by ordinary git tooling.

### Harder

- **Implementation cost.** A byte-exact git server is 4+ weeks of focused
  work. Pack-v2 parsing and emission, zlib framing, SHA-1 object
  serialization, smart-HTTP negotiation, delta encoding (Phase 2). Each
  piece must match git-core's behavior exactly; there's no room for
  "close enough."
- **Maintenance cost.** git-core ships new versions; GitHub ships new
  REST/GraphQL fields; mirror tools find edge cases we didn't test for.
  This is now a product we maintain.
- **Surface area.** GitHub's REST API is ~800 endpoints. GraphQL has
  thousands of field combinations. We implement a subset; we accept that
  some third-party tools will hit endpoints we don't implement and fail.
  The subset we *do* implement must be rock-solid.
- **Coupling to Temper upstream.** If Temper upstream changes OData
  semantics, WASM host API, or action dispatch, we need to follow. We
  mitigate by pinning the submodule and explicit upgrade reviews.

## Options Considered

### Option 1: Patch-nginx (rejected)

See Context above. Two sources of truth long-term. Short-term win, long-term
dead end.

### Option 2: Memory-entity substrate (rejected)

See Context above. Humans can't `git clone` a Memory row. Conflates
agent-remembered-state with source code.

### Option 3: Self-contained temper-git product (chosen)

**Pros:**
- One source of truth.
- Full git ecosystem compatibility for humans and third-party tools.
- Agent writes are first-class IOA transitions with Cedar + audit.
- Temper-only control plane rule is preserved.
- Mirror-friendly by construction (byte-exact hashes guarantee it).

**Cons:**
- Significant initial implementation effort (~4 weeks foundation).
- Ongoing maintenance of a git-protocol and GitHub-API-compatible
  surface. This is a product now, not a glue layer.
- Divergence risk if Temper upstream evolves; mitigated by submodule
  pinning.

## References

- [ADR-0004: Per-repo libSQL with GCS-backed WAL](0004-per-repo-libsql-gcs.md) —
  storage substrate decision that operationalizes this mission for an
  air-gapped GCP deployment.
- [VISION.md](../../VISION.md)
- [ADR-0002: Temper-native SCM substrate](0002-temper-native-scm.md)
- [ADR-0003: Byte-exact git compatibility](0003-byte-exact-git-compat.md)
- [RFC-0001: temper-git v1 architecture](../rfc/0001-temper-git-v1-architecture.md)
- Companion project: `dark-helix/CLAUDE.md` — the factory that consumes
  temper-git.
- Git wire protocol: https://git-scm.com/docs/http-protocol
- GitHub REST v3 reference: https://docs.github.com/en/rest
