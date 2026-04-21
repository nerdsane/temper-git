# RFC-0001: temper-git v1 architecture

## Status

Draft — 2026-04-21. Implementation design for the scope decided in
[ADR-0001](../adr/0001-temper-git-mission.md) /
[ADR-0002](../adr/0002-temper-native-scm.md) /
[ADR-0003](../adr/0003-byte-exact-git-compat.md). Seeking
review before the first Rust line gets written.

## Summary

Concrete architecture for temper-git v1:
- Which IOA entities exist and with what fields.
- Which WASM integrations serve which HTTP paths.
- What Temper-kernel deltas we need (HttpEndpoint entity, streaming WASM I/O).
- How storage scales (inline TreeEntry content vs blob store).
- What auth looks like end-to-end.
- How the product is deployed (K8s topology).
- Phase plan for delivery.

## Scope of v1

Per ADR-0001, v1 = Phase 1 = "Foundation." Concretely:

**Ship (v1):**
- Full git smart-HTTP wire protocol (upload-pack + receive-pack), pack-v2
  format, non-delta object emission.
- GitHub REST v3 subset: `/api/v3/repos[/{owner}/{repo}[/{contents,pulls,refs,branches,tags,commits,merges}]]`.
- GitToken bearer auth with scope enforcement.
- Repository, Ref, Commit, Tree, Blob, Tag, PullRequest, Review,
  ReviewComment, Webhook, GitToken as IOA entities.
- Byte-exact object hashing + canonical serialization (the gating
  contract of [ADR-0003](../adr/0003-byte-exact-git-compat.md)).
- Standalone K8s deployment in its own namespace (`temper-git`).
- First-class `HttpEndpoint` IOA entity for path-prefix routing.
- Streaming `http_call_streaming` WASM host API.

**Defer (v2+):**
- Pack delta compression (OFS_DELTA, REF_DELTA). v1 emits full objects.
  Bandwidth cost: ~5× for typical repos.
- Thin packs (reference objects not in the pack). v1 emits fat packs.
- Webhook delivery (the entity exists; the `trigger` side lands v2).
- Branch protection rules.
- GraphQL v4.
- OAuth apps, fine-grained PATs.
- Migration importer from existing bare repos.
- GitHub UI replica.

## Entity model

Each entity is declared as a `.ioa.toml` in `specs/` plus fields in
`model.csdl.xml`. Cedar policies in `policies/`. Every entity Id is a
UUIDv7 string EXCEPT where git requires a SHA-1 (Blob, Tree, Commit, Tag)
— those use the SHA-1 as `Id` so OData GET by id is the natural
git-object-by-hash lookup.

### `Repository`

States: `Provisioning` → `Active` → `Archived`.

Fields:
- `Id: Edm.String` (UUIDv7, e.g. `repo-019dac...`)
- `OwnerAccountId: Edm.String` (FK to an Account — may be on a different Temper tenant)
- `Name: Edm.String` (e.g. `darkhelix-users`)
- `Description: Edm.String?`
- `DefaultBranch: Edm.String` (e.g. `main`)
- `Visibility: Edm.String` (one of `private`, `public`) — v1 always
  `private`; visibility gating is Cedar's job.
- `Status: Edm.String` (state machine)
- `CreatedAt, UpdatedAt, ArchivedAt: Edm.DateTimeOffset`

Actions:
- `Create`, `Archive`, `SetDefaultBranch`
- `WriteFile(Ref, Path, Content, Mode, Message)` — convenience:
  reads the current Tree under Ref, builds a new Tree with the
  file changed, writes a new Commit, advances Ref. Emits
  `CommitCreated` + `RefUpdated` events.
- `DeleteFile(Ref, Path, Message)` — same, with deletion.
- `BatchWriteFiles(Ref, Changes[], Message)` — atomic multi-file commit.

Invariants:
- `Name` matches `^[a-z0-9][a-z0-9_-]{0,99}$`.
- `DefaultBranch` must be an existing Ref on this repo when Status=Active.
- `Visibility in {private, public}`.

### `Ref`

One git ref (branch or tag). States: `Active` → `Deleted` (terminal, kept
for audit but filter-out on list).

Fields:
- `Id: Edm.String` (UUIDv7)
- `RepositoryId: Edm.String`
- `Name: Edm.String` (e.g. `refs/heads/main`, `refs/tags/v0.1.0`)
- `TargetCommitSha: Edm.String` (SHA-1 hex of a Commit)
- `Kind: Edm.String` (`branch` or `tag`)
- `Status: Edm.String`
- `UpdatedAt: Edm.DateTimeOffset`

Actions:
- `Update(PreviousCommitSha, NewCommitSha)` — compare-and-set semantics
  so concurrent pushes reject. Optionally `ForceUpdate` with no
  precondition (requires `force:true` scope on the token).
- `Delete`.

Invariants:
- `Name` starts with `refs/heads/` or `refs/tags/`.
- `TargetCommitSha` must resolve to a Commit on the same
  RepositoryId (cross-invariant).
- For branches: `TargetCommitSha` must not create a cycle in the
  Commit DAG (cross-invariant checked at Update time).

### `Commit`

One git commit object. States: `Durable` (only terminal — commits
are immutable once hashed).

Fields:
- `Id: Edm.String` — SHA-1 hex of the canonical commit bytes.
- `RepositoryId: Edm.String`
- `TreeSha: Edm.String` — SHA-1 of the root Tree.
- `ParentShas: Collection(Edm.String)` — 0 for root, 1 for normal,
  2+ for merge commits.
- `Author: Edm.String` — formatted per git convention: `Name <email> <unix_secs> <tz>`.
- `Committer: Edm.String`
- `Message: Edm.String` — full commit message including trailer lines.
- `PgpSignature: Edm.String?` — if present, part of the canonical
  bytes.
- `CanonicalBytes: Edm.Binary` — the exact bytes used to compute the
  SHA. Stored for wire-protocol emission without re-serialization.

Invariants:
- `Id == sha1(CanonicalBytes)`. Asserted on every read (checksum
  integrity).
- Every `ParentShas[i]` must resolve to a Commit on the same
  RepositoryId.

### `Tree`

One git tree object. States: `Durable`.

Fields:
- `Id: Edm.String` — SHA-1 hex.
- `RepositoryId: Edm.String`
- `Entries: Collection(TreeEntry)` (child collection; see below).
- `CanonicalBytes: Edm.Binary` — exact tree-object bytes.

Invariants:
- `Id == sha1(CanonicalBytes)`.
- `Entries` sorted by `Path` per git's canonical ordering
  (compare-bytes, with directory entries treated as `path/`).

### `TreeEntry`

A child of Tree, one row per entry in the tree.

Fields:
- `Id: Edm.String` (UUIDv7, since entries can repeat across trees)
- `TreeId: Edm.String` (FK)
- `RepositoryId: Edm.String` (denormalized for shard key)
- `Path: Edm.String` — the path component (no slashes; git trees are
  non-recursive, nesting is via sub-trees).
- `Mode: Edm.String` — `100644` (file), `100755` (executable),
  `040000` (tree), `120000` (symlink), `160000` (submodule).
- `ObjectSha: Edm.String` — SHA-1 of the referenced object.
- `Kind: Edm.String` — `blob` or `tree`.

Invariants:
- `Mode in {100644, 100755, 040000, 120000, 160000}`.
- `Kind == blob` ⇒ `Mode in {100644, 100755, 120000, 160000}`.
- `Kind == tree` ⇒ `Mode == 040000`.

### `Blob`

One git blob. States: `Durable`.

Fields:
- `Id: Edm.String` — SHA-1 hex.
- `RepositoryId: Edm.String`
- `Size: Edm.Int64` — byte length.
- `Content: Edm.String?` — inline if `Size <= INLINE_BLOB_LIMIT` (default
  1 MiB). Stored as latin-1 string (OData constraint; bytes preserved).
- `BlobStoreRef: Edm.String?` — if `Size > INLINE_BLOB_LIMIT`,
  `/_internal/blobs/<repo>/<sha>` pointer into Temper's blob store.
- `CanonicalBytes: Edm.Binary?` — blob header bytes (`blob <size>\0`)
  stored alongside; the full serialized bytes are `CanonicalBytes +
  content`. Used to verify the hash without re-encoding.

Invariants:
- Exactly one of `Content` or `BlobStoreRef` is set.
- `sha1(canonical_bytes + resolve_content()) == Id`.

### `Tag`

Only annotated tags need a Tag row. Lightweight tags are just a Ref
with Kind=tag.

Fields:
- `Id: Edm.String` — SHA-1 of the tag object bytes.
- `RepositoryId: Edm.String`
- `TargetSha: Edm.String` — the commit (or other object) being tagged.
- `TargetType: Edm.String` — `commit`, `tree`, `blob`, or `tag`.
- `TagName: Edm.String`
- `Tagger: Edm.String`
- `Message: Edm.String`
- `PgpSignature: Edm.String?`
- `CanonicalBytes: Edm.Binary`

### `PullRequest`

States: `Draft` → `Open` → `UnderReview` → {`Approved`, `ChangesRequested`} → `Merged|Closed`.

Fields:
- `Id: Edm.String` (UUIDv7)
- `RepositoryId: Edm.String`
- `Number: Edm.Int64` — monotonic per-repo (GitHub-compat #n).
- `SourceRef: Edm.String` (e.g. `refs/heads/feat/xyz`)
- `TargetRef: Edm.String` (`refs/heads/main`)
- `HeadCommitSha: Edm.String` — advances as new commits are pushed.
- `BaseCommitSha: Edm.String` — merge base at PR open time; may
  advance if the PR is rebased.
- `Title: Edm.String`, `Body: Edm.String`
- `OpenedBy: Edm.String` (principal id)
- `State: Edm.String`
- `MergedCommitSha: Edm.String?` — the commit object written by merge.
- `MergedBy: Edm.String?`
- `OpenedAt, UpdatedAt, MergedAt, ClosedAt: Edm.DateTimeOffset?`

Actions:
- `Open(SourceRef, TargetRef, Title, Body, ClientRequestId)` — validates
  branches exist, BaseCommitSha is merge-base.
- `UpdateHead(NewHeadCommitSha)` — emitted by receive-pack when a
  push lands on SourceRef.
- `RequestReview(ReviewerPrincipal)` / `Approve(Body)` / `RequestChanges(Body)` /
  `DismissReview(ReviewId)`.
- `Merge(Strategy, Message, ClientRequestId)` — `Strategy` is one of
  `merge`, `squash`, `rebase`. Emits a new Commit, advances TargetRef,
  transitions state to Merged. v1 ships `merge` and `squash`.
- `Close` without merging.

### `Review` / `ReviewComment`

Standard GitHub-shaped review + line-level comments.

### `GitToken`

States: `Active` → `Revoked`.

Fields:
- `Id: Edm.String` (UUIDv7, e.g. `gt-019...`)
- `PrincipalId: Edm.String` (resolves to Agent or Customer or Admin)
- `HashedSecret: Edm.String` — SHA-256 of the secret (`ghp_...` format).
- `KeyPrefix: Edm.String` — first 8 chars for tracing in logs without
  leaking the secret.
- `Scopes: Collection(Edm.String)` — e.g. `repo:read`, `repo:write`,
  `pr:write`, `admin:repos`.
- `ExpiresAt: Edm.DateTimeOffset?`
- `LastUsedAt: Edm.DateTimeOffset?`
- `CreatedAt, RevokedAt: Edm.DateTimeOffset?`

Invariants:
- `HashedSecret` is unique.
- When presented as bearer, auth lookup computes SHA-256 of the bearer
  string and matches against `HashedSecret`.

### `Webhook`

States: `Active` → `Paused` → `Active` → `Deleted`.

Fields: `Id, RepositoryId, Url, Events, SecretHashed, Status, LastDeliveryAt, LastResponseCode, CreatedAt`.
Events: a subset of GitHub webhook event names (push, pull_request,
pull_request_review, etc.).

### `HttpEndpoint` (new kernel feature — see §"Kernel deltas")

States: `Active` → `Paused` → `Active`.

Fields:
- `Id: Edm.String`
- `PathPrefix: Edm.String` — e.g. `/{owner}/{repo}.git/info/refs`
- `Methods: Collection(Edm.String)` (`GET`, `POST`)
- `IntegrationModule: Edm.String` — WASM module name (e.g.
  `git_upload_pack`).
- `RequiresAuth: Edm.Boolean`
- `TimeoutSecs: Edm.Int32`

## WASM integrations (v1)

Each is a Rust crate compiled to `wasm32-wasip1`, using `temper-wasm-sdk`.

### `git_upload_pack`

Triggers: HttpEndpoint match on
- `GET /{owner}/{repo}.git/info/refs?service=git-upload-pack`
- `POST /{owner}/{repo}.git/git-upload-pack`

Flow for GET (advertisement):
1. Look up Repository by `{owner}/{repo}`.
2. Emit pkt-line header `# service=git-upload-pack`.
3. For every Ref on the repo, emit pkt-line with capabilities on the
   first, just `sha refs/heads/main` on subsequent.
4. Flush pkt.

Flow for POST (pack streaming):
1. Parse pkt-line body: `want <sha>` lines, `have <sha>` lines, `done`.
2. Compute the set of Commits reachable from `want` but not from `have`.
3. Recursively collect all Trees and Blobs reachable from those Commits.
4. For each object in the set, emit the pack entry: 1-byte type+length
   header, zlib-compressed canonical bytes.
5. Emit pack trailer: SHA-1 of all pack bytes so far.
6. Stream to client with side-band-64k framing for pack data on channel 1
   and progress updates on channel 2.

v1 emits full objects (no deltas). A pack is uncompressed-size +
~10% zlib overhead per object. For a 50 MB repo, that's ~60 MB on the
wire — acceptable for v1.

### `git_receive_pack`

Triggers:
- `GET /{owner}/{repo}.git/info/refs?service=git-receive-pack`
- `POST /{owner}/{repo}.git/git-receive-pack`

Flow for POST:
1. Parse initial pkt-lines: `old-sha new-sha refname` tuples (one per
   ref being updated) + capability list.
2. Stream-parse the incoming pack: for each entry, decompress zlib,
   compute SHA-1, construct a Blob/Tree/Commit/Tag entity.
3. Persist all new objects via OData POSTs.
4. For each ref update, atomically:
   - Verify old-sha matches current Ref.TargetCommitSha (or refspec
     is `force`).
   - Verify new-sha resolves to a Commit we just persisted (or
     already exists).
   - Verify new-sha is a fast-forward from old-sha (or refspec is
     `force`).
   - Dispatch `Ref.Update` action.
5. Emit per-ref status: `ok refname` or `ng refname <reason>`.

### `github_rest_contents`

Triggers: `/api/v3/repos/{owner}/{repo}/contents/{path}`

- `GET` → walk Tree from `ref` (query param or default branch), find
  entry for `path`, return JSON with
  `{name, path, sha, size, content (base64), encoding: "base64",
  _links}`.
- `PUT` (create or update) → parse body `{message, content (base64),
  sha?, branch?}`, decode content, call `Repository.WriteFile` action,
  return `{content: {...}, commit: {...}}` matching github.com shape.
- `DELETE` → parse body `{message, sha, branch?}`, call
  `Repository.DeleteFile`.

### `github_rest_pulls`

Triggers: `/api/v3/repos/{owner}/{repo}/pulls[/{number}[/merge|reviews|...]]`

- `GET` list — paginated `state=open|closed|all`, filter via OData.
- `GET {number}` — single PR. Compute `additions`, `deletions`,
  `changed_files` by diffing HeadCommitSha vs BaseCommitSha on-the-fly.
- `POST` create — validate refs, merge base, call `PullRequest.Open`.
- `PATCH {number}` — update title/body/state.
- `PUT {number}/merge` — call `PullRequest.Merge`.
- `GET {number}/reviews`, `POST {number}/reviews` — Reviews.
- `GET {number}/files` — per-file diff, computed from tree walks.

### `github_rest_refs`, `github_rest_branches`, `github_rest_tags`, `github_rest_commits`, `github_rest_merges`, `github_rest_repos`

Standard endpoints translating to OData queries + actions. Detailed
mapping table lands in a follow-up RFC if it gets unwieldy; most
endpoints are one-liners.

## Kernel deltas (temper/)

v1 requires two Temper-kernel additions. We propose upstream first; if
upstream accepts, we bump the submodule. If upstream rejects or
delays, we carry the patches on our submodule branch with explicit
rebase discipline. These are not optional — without them, the
architecture doesn't work.

### K-1: `HttpEndpoint` IOA entity + router integration

A new first-class IOA entity (declared in `temper-platform` specs).
Registered HttpEndpoints extend the router's match set: on request,
longest-prefix match against registered HttpEndpoints, extract path
params, dispatch to bound WASM integration.

Why it belongs upstream: other Temper users will want the same pattern
(generic reverse-proxy-into-entity-actions). Not temper-git-specific.

### K-2: `http_call_streaming` in WASM host

Current `ctx.http_call` is one-shot: WASM builds a complete request
body, host returns a complete response body. For pack upload
(receive-pack reading MBs of pack data) and download (upload-pack
streaming MBs of pack data), we need:

```rust
// Outgoing: WASM emits chunks as they're ready.
let stream = ctx.http_call_streaming_start("POST", url, headers)?;
for chunk in producer {
    stream.write(&chunk)?;
}
let resp = stream.finish()?;

// Incoming: WASM receives request body + emits response chunks.
let req_stream = ctx.request_body_stream();  // from host
for chunk in req_stream.chunks(64 * 1024) {
    parse(chunk)?;
}
ctx.response_body_start()?;
for out_chunk in producer {
    ctx.response_body_write(&out_chunk)?;
}
```

Proposed as an opt-in feature gate in temper-wasm-sdk so existing modules
don't break. Integration modules declare `streaming_io: true` in their
manifest; Temper sets up the streaming plumbing on dispatch.

## Storage sizing

### Blobs

- `INLINE_BLOB_LIMIT = 1 MiB` (configurable per-deployment).
- Larger blobs spill to blob store at `/_internal/blobs/{repo}/{sha}`.
- Typical source repo: 90%+ of blobs are <10 KiB; median blob <1 KiB.
- Pathological case (binary assets, vendored PDFs): handled by blob
  store; no impact on entity row size.

### Trees

- Tree entries average 10–50 per tree in real repos.
- `CanonicalBytes` length: ~35 bytes per entry + filename length.
- A typical 20-entry tree is ~1–2 KiB — fits comfortably inline.

### Commits

- `CanonicalBytes` length: ~200 bytes for small commits, up to ~5 KiB
  for annotated ones. Inline.

### Postgres footprint estimate

For the existing dark-helix repos (empty today, will grow):
- `darkhelix-users.git` projected @ 6 months: ~500 MB repo → ~50 MB
  entity state (blobs mostly inline, big binary assets via blob
  store).
- 1 GB Postgres per ~10 GB of repo state. Provision a 10 GB Postgres
  for v1 with room for ~100 GB of repo content.

## Auth flow

1. Client presents bearer (`Authorization: Bearer ghp_...` or HTTP Basic
   `<token>:x-oauth-basic`).
2. Temper's auth layer extracts the token, SHA-256s it, queries
   `/tdata/GitTokens?$filter=HashedSecret eq '<hash>'&$top=1`.
3. If found and `Status == Active` and `ExpiresAt > now`:
   - Set `Principal::{Kind}::<PrincipalId>` based on the token's
     `PrincipalId`.
   - Attach `Scopes` as context attrs for Cedar.
4. Update `GitToken.LastUsedAt`.
5. Cedar gates the downstream action (`Repository.WriteFile`, etc.) by
   combining principal + scopes + resource attrs.

Unauthenticated requests are rejected for any mutation. Reads may be
anonymous if Cedar's repo-level policy permits (v1: everything is
authenticated).

## Deployment topology

```
Namespace: temper-git (separate from darkhelix)
  ├─ Deployment: temper-git
  │    └─ 1 replica of the Temper pod with temper-git os-app baked in
  ├─ Service: temper-git (ClusterIP, port 80)
  ├─ PVC: temper-git-blob-store (for >1MB blob spills)
  ├─ Secret: temper-git-db-url (Cloud SQL Auth Proxy → dedicated Postgres)
  └─ Deployment: postgres-proxy-temper-git (Cloud SQL Auth Proxy instance)
```

Image: `us-central1-docker.pkg.dev/darkhelix-sandbox/temper-git/temper-git:vX.Y.Z`.
Built from `temper-git/Dockerfile` which FROMs the Temper upstream image
at the pinned submodule commit and COPYs `os-app/` + WASMs on top.

Service DNS: `temper-git.temper-git.svc.cluster.local`.

For the dark-helix factory consumption, the existing `temper-git.darkhelix.svc.cluster.local`
Service will be repointed (via `ExternalName` or an ingress reroute) to
the new service once the migration importer runs.

## Phase plan

### Phase 1 — Foundation (~4 weeks, target v0.1.0)

Weeks 1–2: **Entity model + canonical serialization**
- IOA specs + CSDL for all entities in §"Entity model".
- Cedar policies (read/write scoped by token scope).
- Rust helpers for canonical object serialization + SHA-1:
  - `blob_canonical_bytes(content)`
  - `tree_canonical_bytes(entries)`
  - `commit_canonical_bytes(tree, parents, author, committer, msg, sig?)`
  - `tag_canonical_bytes(...)`
- Hash-byte-match harness tests (10+ fixtures per helper).

Weeks 2–3: **Protocol handlers**
- `git_upload_pack` WASM with pkt-line + non-delta pack emission.
- `git_receive_pack` WASM with pack parsing + SHA-1 verification.
- `HttpEndpoint` routing (K-1).
- `http_call_streaming` (K-2).
- Round-trip integration test: clone → commit → push → clone → diff.

Week 4: **REST subset + auth**
- `github_rest_contents`, `_pulls`, `_refs`, `_branches`, `_commits`,
  `_merges`, `_repos`.
- GitToken auth resolution.
- GitHub API shape tests against fixtures.
- v0.1.0 release.

### Phase 2 — Production hardening (~3 weeks, target v0.2.0)

- Pack delta compression (OFS_DELTA, thin packs).
- Webhook delivery.
- Remaining REST endpoints (releases, tags w/ signatures, compare, search-within-repo).
- Branch protection rules.
- Concurrency / ref contention tests.

### Phase 3 — Modern surface (~3 weeks, target v0.3.0)

- GraphQL v4 implementation.
- OAuth apps / fine-grained PATs.
- Observe UI code-view widget (if we own it, or hand-off to dark-helix).

### Phase 4 — Migration + decommission (~1 week, target v0.4.0)

- Bare-repo importer: walks existing `darkhelix-users.git`,
  `darkhelix-operators.git`, `dark-helix.git`, `helix.git` bare repos
  from the old temper-git pod, ingests every object as Blob/Tree/Commit/Ref.
- Repoint `temper-git.darkhelix.svc.cluster.local` at the new service.
- Decommission the old pod.
- v1.0.0.

## Open questions

1. **SHA-256 git transition.** git-core is moving toward SHA-256 repos
   as an option. Our v1 is SHA-1-only. Do we design entities with
   `Id: Edm.String` assuming SHA-1 length (40 hex), or leave room
   for 64-hex SHA-256 ids? Proposal: use `Edm.String` no length limit
   now; when SHA-256 lands, add a `HashAlgo` field to Repository and
   select serialization accordingly.

2. **Inline vs blob store threshold.** v1 picks 1 MiB. Tunable per
   deployment. Do we tune per-repo? Probably not in v1; just a global.

3. **GraphQL in v1 vs v2.** REST alone covers 95% of agent use cases.
   GraphQL is better for humans and modern CLIs. We defer to v2 unless
   a specific consumer requires it.

4. **Pack delta in v1 vs v2.** Non-delta packs work but are ~5×
   larger. For the dark-helix factory's small-repo scale, that's fine;
   for production use with large repos, we'd want deltas. v1 ships
   non-delta; v2 adds deltas. Documented in release notes.

5. **Migration timing.** Phase 4 is optional for the v1 ship if we
   stand up new fresh repos and don't need to preserve history. For
   dark-helix today, repos are near-empty, so migration is cheap —
   recommend shipping migration with v1 anyway.

## Sign-off required

Review this RFC before Phase 1 starts. Areas most in need of scrutiny:

- Entity model completeness: are there git concepts we haven't modeled?
- Cedar policy surface: do the proposed scopes (`repo:read`,
  `repo:write`, etc.) match what GitHub PATs use?
- Kernel deltas: will upstream Temper accept HttpEndpoint + streaming
  WASM I/O?
- Deployment topology: own namespace + own Postgres is heavy — any
  lighter shape acceptable?

Once this RFC lands `Accepted`, Phase 1 work starts. New RFCs for Phase 2+
as those come into focus.

## References

- [ADR-0001](../adr/0001-temper-git-mission.md)
- [ADR-0002](../adr/0002-temper-native-scm.md)
- [ADR-0003](../adr/0003-byte-exact-git-compat.md)
- Git object format: https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
- Git pack format: https://git-scm.com/docs/pack-format
- Git smart-HTTP: https://git-scm.com/docs/http-protocol
- GitHub REST v3 reference: https://docs.github.com/en/rest
- GitHub GraphQL v4 reference: https://docs.github.com/en/graphql
