# ADR-0002: SCM state is IOA entities; protocol handlers are WASM integrations

## Status

Accepted — 2026-04-21. Establishes the core architectural contract for
temper-git's internals. Paired with [ADR-0001](0001-temper-git-mission.md)
(mission) and [ADR-0003](0003-byte-exact-git-compat.md) (compat gate).

## Context

We've decided (ADR-0001) that temper-git is a Temper-native product with
GitHub-compatible surface. This ADR fixes the next-level question: *what
lives in IOA entities, what lives in WASM, and how do they compose?*

The temptation is to cut corners. Three shapes were considered:

1. **Temper host extensions.** Add a `git_` family of Rust crates inside the
   Temper kernel that know how to parse packs, emit objects, walk commit
   graphs. Fast (native code), shared process with the OData server.
2. **Hybrid: state in IOA entities, protocol handlers in Rust host.** Blob /
   Tree / Commit / Ref as entities; a Rust-side `temper-scm` crate serves
   `/*.git/*` and `/api/v3/*` paths.
3. **Pure Temper-native.** State in IOA entities AND protocol handlers in
   WASM integrations dispatched via Temper's existing integration
   machinery. No Rust host extensions; no separate protocol process.

## Decision

**Option 3: pure Temper-native.** Every git concept is an IOA entity.
Every protocol handler — smart-HTTP wire, REST v3, GraphQL v4 — is a WASM
integration bound to a `HttpEndpoint` entity (new; see RFC-0001). Temper
dispatches the inbound HTTP request to the matching WASM module the same
way it already dispatches entity-action integrations.

### Entity graph (IOA specs)

```
Repository
  ├─ Ref (name, target_commit_sha, type)
  │    └─ Commit ─┐
  │               ├─ Tree
  │               │    └─ TreeEntry (path, mode, sha, kind)
  │               │         └─ Blob (sha, content|blob_store_ref, size)
  │               ├─ Parent Commits (DAG)
  │               └─ Author, Committer, Message, Timestamp
  ├─ Tag (optional annotated tag metadata; lightweight tags are just Refs)
  ├─ PullRequest (source_ref, target_ref, head_commit_sha, state, title, body)
  │    ├─ Review (pr_id, reviewer, decision, body)
  │    └─ ReviewComment (review_id, path, line, body)
  ├─ Webhook (events[], endpoint_url, secret_hashed)
  └─ GitToken (hashed_secret, scopes[], expires_at, principal_id)
```

Each entity has a Cedar policy, a TLA+ invariant where meaningful
(Commit DAG is acyclic; Ref.target_commit_sha must reference an existing
Commit; etc.), and a set of IOA actions (Create, Archive, …). Mutations
flow through the normal Temper dispatch pipeline: OData POST → action
→ effect → verification → persistence → event emission.

### HTTP protocol handlers (WASM integrations)

The Temper kernel today routes HTTP into OData (`/tdata/*`), observe
endpoints (`/observe/*`), webhook receivers (`/webhooks/*`). It does not
route arbitrary prefixes. So temper-git introduces a new first-class
IOA entity:

```
HttpEndpoint
  ├─ PathPrefix (e.g., "/{owner}/{repo}.git")
  ├─ Methods ([GET, POST])
  ├─ IntegrationModule (WASM module name)
  ├─ RequiresAuth (bool)
  └─ TimeoutSecs
```

When a request arrives, the kernel matches against registered
HttpEndpoints longest-prefix-first, extracts path parameters, and
dispatches to the bound WASM with:
- Method, Path, Headers, Body (streaming)
- Matched path parameters as `ctx.params`
- Principal from auth (resolved before dispatch)

The WASM integration receives the streaming body, does whatever it
needs (parse a pack, read entity state, emit a response), and streams
its response back.

**Implementations (initial set):**
- `git_upload_pack` — handles `GET /{owner}/{repo}.git/info/refs?service=git-upload-pack`
  and `POST /{owner}/{repo}.git/git-upload-pack`.
- `git_receive_pack` — handles `/info/refs?service=git-receive-pack` and
  `POST /{owner}/{repo}.git/git-receive-pack`.
- `github_rest_contents` — `GET/PUT/DELETE /api/v3/repos/{owner}/{repo}/contents/{path}`.
- `github_rest_pulls` — `/api/v3/repos/{owner}/{repo}/pulls[/...]`.
- `github_rest_refs` — `/api/v3/repos/{owner}/{repo}/{git,branches,tags}/...`.
- `github_rest_repos` — `/api/v3/repos[/{owner}/{repo}]`.
- `github_graphql` — `POST /api/graphql` (Phase 2).

### Separation of concerns

- **IOA entities own state.** They are the durable, canonical
  representation. Nothing else is. Physical storage is a per-repo
  libSQL database with WAL frames shipped to GCS; see
  [ADR-0004](0004-per-repo-libsql-gcs.md). The separation contract
  holds regardless of substrate — if we later swap libSQL for Turso
  Cloud (same wire protocol, different vendor), the entity model and
  WASM integrations are unchanged.
- **WASM integrations own protocol.** They are stateless (aside from
  in-request working memory). They translate between
  smart-HTTP/REST/GraphQL and OData actions.
- **Temper kernel owns dispatch.** Routes requests, enforces Cedar,
  manages actor lifecycle, persists state via the storage backend.

A `git push` looks like:
1. Client: `POST /darkhelix-users.git/git-receive-pack` with pack body.
2. Kernel routes to `git_receive_pack` WASM.
3. WASM parses the pack, computes SHA-1 for each object, emits
   `POST /tdata/Blobs`, `POST /tdata/Trees`, `POST /tdata/Commits` to
   persist.
4. WASM emits `POST /tdata/Refs('refs/heads/main')/Temper.Update` to
   advance the ref.
5. Temper's action-dispatch machinery applies Cedar gates, writes events,
   triggers any downstream `[[integration]]` (e.g., a webhook delivery
   integration fires on Ref.Update).
6. WASM streams the status report response back to the client.

No bare repo on disk at any step. No second source of truth.

## Consequences

### Easier

- **One Cedar surface.** Cedar policies on Repository, PullRequest,
  GitToken, HttpEndpoint, etc. compose naturally. No parallel access
  control in nginx or a separate auth proxy.
- **One audit trail.** Every action — including raw wire-protocol pushes —
  emits a trajectory entry. The existing Datadog wiring on Temper gets
  a unified "git activity" view for free.
- **One verification cascade.** If a new Repository spec adds a
  cross-invariant ("every Commit's parent must exist in the same
  repo"), Temper's L0–L3 cascade runs it.
- **Agents are symmetric with humans.** A WASM tool on an agent and
  a `curl` from a laptop both land in the same entity graph via the
  same OData surface. No "one path for agents, another for humans."
- **No shell, no CGI, no PID 1 complexity.** The Temper pod already
  handles process management; protocol handlers are just more WASM.

### Harder

- **WASM is an unusual place to implement git pack format.** The
  integration needs SHA-1, zlib, and careful streaming. We can't just
  link `libgit2`; we implement the relevant subset in Rust, compile to
  WASM, and hope. Testable but labor-intensive (see
  [ADR-0003](0003-byte-exact-git-compat.md) for the compat guard).
- **HttpEndpoint is a new kernel feature.** Temper upstream doesn't
  ship it today. We either: (a) propose it upstream and wait, or (b)
  carry the addition on our submodule branch and keep it in sync. See
  RFC-0001 §"Kernel deltas".
- **Streaming WASM I/O requires capable host APIs.** `ctx.http_call` in
  Temper's WASM SDK is a one-shot request/response. For pack uploads
  (receive-pack) and pack downloads (upload-pack), we need streaming
  request bodies and streaming response bodies. Adding
  `ctx.http_stream_*` to the SDK is part of the kernel delta.

### Risks

- **Kernel delta approval.** HttpEndpoint and streaming WASM I/O are
  non-trivial temper-kernel changes. If upstream Temper rejects them,
  we carry a fork of temper/, which we explicitly said we won't do.
  Mitigation: engage temper maintainers early, ship upstream PRs in
  parallel with our feature work.
- **WASM execution budgets.** A full repo pack can stream MBs. The
  existing WASM invocation timeouts are tuned for sub-second actions.
  Pack handlers need 300s+ budgets, which Temper's dispatch machinery
  accepts but isn't well exercised under. We'll stress-test with
  synthetic 100MB+ pack fixtures.

## Options Considered

### Option 1: Host-side Rust extensions (rejected)

**Pros:** native code speed; easy access to SHA-1 / zlib crates.
**Cons:** violates "no code outside Temper's primitives." Every new
feature requires a Temper binary rebuild. Fragments the Cedar policy
surface. Fails the compat check for the temper-git OS-app layout.

### Option 2: Hybrid — entity state, Rust protocol handlers (rejected)

Mostly the pragmatic middle ground. Rejected because "Rust-host
protocol handlers" are exactly the thing ADR-0001 and Temper's
Temper-only rule preclude. Also means two languages in the project
(Rust kernel, Rust WASM, plus a Rust-in-host component) with two
build paths.

### Option 3: Pure Temper-native (chosen)

See Decision. The cost is real (WASM implementation of pack codec,
streaming APIs). The payoff is architectural coherence: Cedar, audit,
verification, everything is symmetric.

## References

- [ADR-0001](0001-temper-git-mission.md) — mission
- [ADR-0003](0003-byte-exact-git-compat.md) — compat gate
- [ADR-0004](0004-per-repo-libsql-gcs.md) — per-repo libSQL storage
- [RFC-0001](../rfc/0001-temper-git-v1-architecture.md) — v1 design
- [temper/CLAUDE.md](../../temper/CLAUDE.md) — kernel discipline
- Temper ADR-0002 (dark-helix): "Temper-only control plane"
- Git object format: https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
- Git smart-HTTP: https://git-scm.com/docs/http-protocol
