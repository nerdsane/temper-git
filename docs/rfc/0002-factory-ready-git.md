# RFC-0002: Factory-ready git push + clone

- Status: Draft
- Date: 2026-04-24
- Related: RFC-0001 (v1 architecture), ADR-0056 (HttpEndpoint),
  ADR-0057 (http_call_streaming), ADR-0003 (byte-exact git compat).

## Goal

Helix agents and the factory control plane use a real `git` binary
to push commits and clone repositories against temper-git, with no
bespoke client, no sidecar, no pre-serialized objects. Same wire
protocol GitHub speaks; same command lines the agents already use.

## Status quo (end of phase2f)

Live in-cluster on `dh-dev`:

- `git ls-remote http://temper-git.../<owner>/<repo>.git` → works.
- `git clone http://temper-git.../<owner>/<repo>.git` → works for
  **empty repos** (git short-circuits on zero refs).
- POST `/{owner}/{repo}.git/git-upload-pack` → 501 stub.
- POST `/{owner}/{repo}.git/git-receive-pack` → 404 (no route).

Available libraries:

- `tg-canonical` — byte-exact git-object serialization + SHA-1
  (38 tests, parity-verified against real git).
- `tg-wire` — pkt-line framing, `/info/refs` advertisement, pack-v2
  parser (32 tests, parity-verified against real git pack-objects).
- ADR-0057 inbound streaming works end-to-end (proven by
  `git ls-remote` exit 0).

## What ships in "factory ready"

The minimum viable factory flow:

```
# Agent creates repo
curl -X POST .../tdata/Repositories -d '{"Id":"rp-1","OwnerAccountId":"me","Name":"foo",...}'

# Agent clones it (empty)
git clone http://temper-git.../me/foo.git   # ← works today

# Agent commits + pushes
cd foo && echo hi > README && git add README && git commit -m init
git push origin main                         # ← needs receive-pack

# Another agent / dark-helix clones the pushed content
git clone http://temper-git.../me/foo.git   # ← needs upload-pack POST
```

## Remaining slices

### Slice A — Receive-pack

Scope:

1. **Capabilities**: drop `thin-pack` + `ofs-delta` from
   `tg_wire::receive_pack_capabilities()` so clients won't emit
   delta-encoded objects. The parser in `wire/src/pack.rs`
   rejects deltas explicitly; dropping capabilities guarantees
   clients never send them.
2. **Command-list parser** (new module `tg_wire::commands`):
   pkt-line sequence of `<old-sha> <new-sha> <refname>\0<capabilities>\n`
   followed by pack bytes. First line carries the capability
   block; subsequent lines are bare commands; `0000` flush
   ends the list and starts the pack.
3. **`git_receive_pack` WASM module**:
   - GET `/info/refs?service=git-receive-pack` → reuse
     `advertise_info_refs(Service::ReceivePack, ...)` from
     `tg-wire`.
   - POST `/git-receive-pack`:
     - Read request body into a bounded buffer (v0: 64 MiB cap).
     - Parse command list.
     - Parse pack via `tg_wire::parse_pack`.
     - For each object, compute its SHA-1 via `tg_canonical`
       (already does this byte-exactly), POST to the matching
       entity set (`/tdata/Blobs`, `/tdata/Trees`, `/tdata/Commits`,
       `/tdata/Tags`).
     - For each ref command, POST to `/tdata/Refs` (Create) or
       `/tdata/Refs('{id}')/Update` (CAS with old-sha).
     - Emit the pkt-line response: `000eunpack ok\n` + per-ref
       `ok <ref>\n` or `ng <ref> <reason>\n` + flush.
4. **Guest-side OData helper**: small wrapper over
   `host_http_call` that builds the temper-API bearer auth +
   X-Tenant-Id + X-Temper-Principal-* headers from the
   invocation context. The `integration_config` has
   `temper_api_url`; temper already auto-injects those headers
   when the URL matches (see
   `crates/temper-wasm/src/host_trait.rs` line 474).
5. **HttpEndpoint seed rows**: boot-time seed (via
   `seed-data/` TOML or a reconciler hook) so the operator
   doesn't have to manually POST two HttpEndpoint rows per
   deploy.

Scope boundaries (deferred):

- Delta objects in the incoming pack — needs delta resolution
  (ofs-delta base offset walking, ref-delta base lookup). Not
  required for first-push workloads.
- Hooks (pre-receive / post-receive). Useful for CI triggers
  but orthogonal to the wire protocol.
- Shallow clones / partial clones. Phase 3.

### Slice B — Upload-pack POST (pack emission)

Scope:

1. **Want/have negotiation parser** (new module
   `tg_wire::negotiation`): client emits `want <sha>\n` for
   each commit it wants, then `have <sha>\n` lines (first clone:
   none), then flush or `done`. Server responds with `NAK` (no
   common base) or `ACK <sha> common` when the client has one
   of our commits, ending with `ACK <sha>` on done.
2. **Reachable-object walker** (new `tg_canonical::walk`):
   starting from the wanted commit shas, BFS over parent refs +
   tree entries, collecting every commit/tree/blob SHA that
   the client doesn't already have (delta-based exclusion via
   the `have` set).
3. **Pack-v2 emitter** (new `tg_wire::emit`): serialise the
   walked object set as `PACK\0\0\0\2<count>` + per-object
   (type + size header) + zlib-deflated body + 20-byte SHA-1
   trailer. Non-delta entries only in v0.
4. **`git_upload_pack` module extension**: branch on method.
   GET /info/refs keeps the existing advertisement path; POST
   /git-upload-pack does the full negotiate + walk + emit
   flow, with sidebands:
   - sideband 1 (pack bytes): the pack we're emitting.
   - sideband 2 (progress): optional `Counting objects: N\n`,
     `Compressing: ...`, etc.
   - sideband 3 (error): propagate any failure.
5. **`git_upload_pack` advertisement refs** (small prerequisite
   of its own): the empty-repo advertisement we ship today is
   fine for cloning empty repos but won't let git clone
   populated ones. Need to query `/tdata/Refs?$filter=RepositoryId eq ...`
   from the guest when generating the advertisement.

Scope boundaries (deferred):

- `include-tag` (auto-include annotated tags reachable from
  wants). Default `git clone` sets this; we can add later.
- `shallow` / depth-limited clones.
- Keepalive progress frames during long walks.

### Slice C — Repository provisioning

A Helix agent runs `git clone http://.../new-repo.git` on a repo
that doesn't exist yet. Options:

1. **Explicit pre-create**: agent POSTs `/tdata/Repositories`
   first. This is what RFC-0001 assumed.
2. **Auto-provision on push**: receive-pack's first request
   against an unknown owner/repo creates the Repository row
   lazily before writing objects. Simpler UX, matches GitHub
   behavior of "push to create" when enabled.

Recommend starting with (1) — explicit is clearer and the agent
workflow already has a "create repo" step in the factory spec.
(2) is a future enhancement gated behind a Cedar policy.

## Readiness gates

Gate 1 — receive-pack:
- `git init && git commit -m hi && git push temper-git main`
  exits 0, advertises the new ref back on the next
  `git ls-remote`.
- `tg-wire` tests still green; new `git_push_parity.rs`
  integration test green.

Gate 2 — upload-pack POST:
- After Gate 1, a fresh clone from a different working tree
  reproduces the file content bit-for-bit.
- Commit / tree / blob SHAs match what the source repo has.

Gate 3 — multi-commit roundtrip:
- Push 10 commits touching different files; clone; the commit
  history matches exactly (same SHAs in same order).

## Non-goals

- Bare repository hosting (ADR-0002: state is IOA entities,
  not on-disk packs).
- Protocol v2 (new `git-protocol: version=2` header). v1 is
  simpler and good enough for the factory; v2 is an optimization
  gated behind another RFC.
- Git LFS.
- SSH transport. HTTP-only because agents always have an HTTP
  client available and SSH requires custom key management.

## Sequencing

1. Slice A (receive-pack) — unblocks the "agent pushes a commit"
   flow. Validates that objects round-trip through IOA entities.
2. Slice B (upload-pack POST) — unblocks cloning back what was
   pushed. Validates the reverse path.
3. Slice C (auto-provision) — optional polish once A + B work.
