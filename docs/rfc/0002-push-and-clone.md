# RFC-0002: push and clone against populated repositories

- Status: Draft
- Date: 2026-04-24
- Related: [RFC-0001](0001-architecture.md) (v1 architecture),
  [ADR-0002](../adr/0002-temper-native-version-control.md),
  [ADR-0003](../adr/0003-byte-exact-git-compat.md) (byte-exact git
  compat).

## Goal

A standard `git` client pushes commits and clones repositories against
temper-git, with no bespoke client, no sidecar, no pre-serialized
objects. Same wire protocol GitHub speaks; same command lines.

## Status quo

Working locally end-to-end today:

- `git push` of one or more commits — pack parsed in WASM, every
  object's SHA-1 verified byte-exactly, blobs/trees/commits/tags
  persisted as IOA entities through OData, refs advanced with
  compare-and-swap.
- `git clone` of populated repositories — `/info/refs` advertises real
  refs from `/tdata/Refs`; `git-upload-pack` walks the
  commit/tree/blob DAG out of entity state and emits a pack-v2
  response with side-band-64k framing.
- `git ls-remote` against empty and populated repositories.
- Round-trip: push commits, clone them back, working tree
  bit-identical.

Available libraries:

- `tg-canonical` — byte-exact git-object serialization + SHA-1, plus
  minimal commit/tree parsers for DAG walks, parity-verified against
  real `git`.
- `tg-wire` — pkt-line framing, `/info/refs` advertisement, pack-v2
  parser **and** emitter, receive-pack command-list parser, all
  parity-verified against real `git`.
- Bidirectional streaming via the `HttpEndpoint` router +
  `http_call_streaming` host API.

## What ships next

The minimum viable round-trip:

```bash
# Create a repo
curl -X POST .../tdata/Repositories \
  -d '{"Id":"rp-1","OwnerAccountId":"me","Name":"foo",...}'

# Clone it (empty)
git clone http://localhost:.../me/foo.git   # ← works today

# Commit + push
cd foo && echo hi > README && git add README && git commit -m init
git push origin main                        # ← needs receive-pack

# Clone the pushed content back
git clone http://localhost:.../me/foo.git   # ← needs upload-pack POST
```

## Slices

### Slice A — receive-pack — **landed**

Scope (all in tree):

1. **Capabilities**: drop `thin-pack` + `ofs-delta` from
   `tg_wire::receive_pack_capabilities()` so clients won't emit
   delta-encoded objects. The parser in `wire/src/pack.rs` rejects
   deltas explicitly; dropping capabilities guarantees clients never
   send them.
2. **Command-list parser** (new module `tg_wire::commands`): pkt-line
   sequence of `<old-sha> <new-sha> <refname>\0<capabilities>\n`
   followed by pack bytes. First line carries the capability block;
   subsequent lines are bare commands; `0000` flush ends the list and
   starts the pack.
3. **`git_receive_pack` WASM module**:
   - GET `/info/refs?service=git-receive-pack` → reuse
     `advertise_info_refs(Service::ReceivePack, ...)` from `tg-wire`.
   - POST `/git-receive-pack`:
     - Read request body into a bounded buffer (v0: 64 MiB cap).
     - Parse command list.
     - Parse pack via `tg_wire::parse_pack`.
     - For each object, compute SHA-1 via `tg_canonical`
       (already does this byte-exactly), POST to the matching entity
       set (`/tdata/Blobs`, `/tdata/Trees`, `/tdata/Commits`,
       `/tdata/Tags`).
     - For each ref command, POST to `/tdata/Refs` (Create) or
       `/tdata/Refs('{id}')/Update` (CAS with old-sha).
     - Emit the pkt-line response: `000eunpack ok\n` + per-ref
       `ok <ref>\n` or `ng <ref> <reason>\n` + flush.
4. **Guest-side OData helper**: small wrapper over `host_http_call`
   that builds the temper-API bearer auth + tenant + principal
   headers from the invocation context.
5. **HttpEndpoint seed rows**: boot-time seed so the operator doesn't
   have to manually POST HttpEndpoint rows per deploy.

Deferred:

- Delta objects in the incoming pack — needs delta resolution
  (ofs-delta base offset walking, ref-delta base lookup). Not required
  for first-push workloads.
- Hooks (pre-receive / post-receive). Useful for CI triggers but
  orthogonal to the wire protocol.
- Shallow / partial clones. Phase 3.

### Slice B — upload-pack POST (pack emission) — **landed**

Scope (all in tree):

1. **Want/have negotiation parser** (new module
   `tg_wire::negotiation`): client emits `want <sha>\n` for each
   commit it wants, then `have <sha>\n` lines (first clone: none),
   then flush or `done`. Server responds with `NAK` (no common base)
   or `ACK <sha> common` when the client has one of our commits,
   ending with `ACK <sha>` on done.
2. **Reachable-object walker** (new `tg_canonical::walk`): starting
   from the wanted commit shas, BFS over parent refs + tree entries,
   collecting every commit/tree/blob SHA the client doesn't already
   have (exclusion via the `have` set).
3. **Pack-v2 emitter** (new `tg_wire::emit`): serialize the walked
   object set as `PACK\0\0\0\2<count>` + per-object (type + size
   header) + zlib-deflated body + 20-byte SHA-1 trailer. Non-delta
   entries only in v0.
4. **`git_upload_pack` module extension**: branch on method. GET
   /info/refs keeps the existing advertisement path; POST
   /git-upload-pack does the full negotiate + walk + emit flow,
   with sidebands:
   - sideband 1 (pack bytes): the pack we're emitting.
   - sideband 2 (progress): optional `Counting objects: N\n`,
     `Compressing: ...`, etc.
   - sideband 3 (error): propagate any failure.
5. **`git_upload_pack` advertisement refs**: the empty-repo
   advertisement we ship today is fine for cloning empty repos but
   won't let git clone populated ones. Need to query
   `/tdata/Refs?$filter=RepositoryId eq ...` from the guest when
   generating the advertisement.

Deferred:

- `include-tag` (auto-include annotated tags reachable from wants).
- `shallow` / depth-limited clones.
- Keepalive progress frames during long walks.

### Slice C — repository provisioning — **partially landed**

A client runs `git clone http://.../new-repo.git` on a repo that
doesn't exist yet. Two shapes:

1. **Explicit pre-create** (in tree): client POSTs `/tdata/Repositories`
   first using the convention `Id = rp-{owner}-{repo}`; receive-pack
   resolves owner/repo from the URL to that Id.
2. **Auto-provision on push** (deferred): receive-pack's first request
   against an unknown owner/repo creates the Repository row lazily.
   Simpler UX, matches GitHub's "push to create" behavior. Gated
   behind a Cedar policy when added.

## Readiness gates

Gate 1 — receive-pack — **green**:
- `git init && git commit -m hi && git push temper-git main` exits 0,
  advertises the new ref back on the next `git ls-remote`.
- `tg-wire` + `tg-canonical` tests green.

Gate 2 — upload-pack POST — **green**:
- After Gate 1, a fresh clone from a different working tree reproduces
  the file content bit-for-bit.
- Commit / tree / blob SHAs match what the source repo has.

Gate 3 — multi-commit roundtrip — **green**:
- Push multiple commits touching different files; clone; the commit
  history matches exactly (same SHAs in same order).

## Non-goals

- Bare repository hosting (ADR-0002: state is IOA entities, not on-disk
  packs).
- Protocol v2 (new `git-protocol: version=2` header). v1 is simpler and
  good enough for now; v2 is an optimization gated behind another RFC.
- Git LFS.
- SSH transport. HTTP-only because the primary writer (an agent) always
  has an HTTP client available, and SSH requires custom key management.

## Next

With Slice A + B green and Slice C explicit-only, the next work is on
RFC-0001's Phase 2 list: pack delta compression, hooks, branch
protection rules, the wider GitHub REST surface. Each of those gets
its own RFC when it comes into focus.
