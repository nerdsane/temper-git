# temper-git — Vision

## What this is

**A self-hosted, Temper-native, GitHub-compatible SCM.** Byte-exact git wire
protocol. GitHub REST v3 + GraphQL v4 surface for broad client compatibility.
Every object in the repository — blobs, trees, commits, refs, tags, pull
requests, reviews — is a first-class Temper IOA entity. No bare `.git`
directories on disk. No nginx. No CGI. Just Temper primitives + WASM protocol
handlers.

## The one-sentence pitch

**Agents hosted on Temper can't push code to GitHub — so GitHub moves into
Temper.**

## Frame

In the dark-helix factory (our sibling project), agent cohorts run inside
Temper. They file `Observation` entities, propose `PullRequest` entities, and
the only thing standing between an agent-authored change and production is a
human approval. When that happens, the change needs to land in real code —
and real code lives in git. But agents inside WASM can't speak git wire
protocol; they can only speak HTTP + OData.

The obvious answers:
- **"Use GitHub's API from WASM"** — coupling the whole factory to an external
  dependency controlled by a third party. Rate limits, outages, schema
  changes out of our control.
- **"Patch temper-git (the current nginx+CGI stack in dark-helix) to expose
  write endpoints"** — a CGI shell script writing to a bare repo. Works,
  but two sources of truth and git-wire protocol stays opaque to agents.
- **"Use a Temper `Memory` entity as the substrate"** — no wire protocol
  compat, humans can't `git clone`, splits "where the code lives" into two
  unrelated systems.

None of those treat git as what it actually is: **a specific data model
(blobs, trees, commits, refs) plus a specific wire protocol plus a specific
REST API for everything above the wire.** If we just *build that model
inside Temper*, agents get writeable git via OData, humans get `git clone` via
wire protocol, and everyone sees one source of truth.

## The two users

**Agents** (the primary user). Every write — `create_file`, `open_pr`,
`merge_pr`, `push_commit` — is an OData action on an IOA entity, Cedar-gated,
trajectory-emitted, verification-cascade-governed. An agent's WASM tool is
three lines: `ctx.http_call("POST", "/tdata/Repositories('foo')/Temper.WriteFile", ...)`.
No git binary in the WASM sandbox. No pack protocol parsing. No SHA-1
arithmetic. Agents don't even know git exists.

**Humans** (the secondary user). Every external git client — `git` CLI, JetBrains,
VS Code, `gh` CLI, Terraform providers, mirror tools (`git-sync`,
`mirror`) — must work against temper-git as if it were GitHub Enterprise. That
means byte-exact SHA-1 object hashes, canonical git object serialization,
pack-v2 with delta compression, full smart-HTTP protocol. A human should be
able to `git clone https://<token>@temper-git.internal/org/repo.git`,
push commits, open PRs via `gh pr create`, and not know they're not talking
to github.com.

## Hard rules

1. **Full Temper discipline.** This project is Temper, forked at commit level
   as `temper/` submodule, with a GitHub-compatibility OS app on top. Same
   TigerStyle, same IOA TOML specs, same Cedar policies, same verification
   cascade, same deterministic-simulation standards. No exceptions.

2. **Byte-exact git compatibility.** Every Blob/Tree/Commit/Tag object
   temper-git produces must hash identically to what `git hash-object`
   produces on the same inputs. Every pack emitted must pass `git fsck`.
   Every wire-protocol response must be indistinguishable from
   git-http-backend's output. If a third-party tool works against GitHub,
   it works against temper-git.

3. **No second source of truth.** The Blob/Tree/Commit/Ref entities *are*
   the repository. There is no "bare repo" on disk. If an agent writes a
   file via OData and a human then clones the repo, the human sees the
   agent's change, because it's all one dataset.

4. **Self-contained deployment.** temper-git is its own product: own image,
   own deployment, own Postgres, own cluster footprint. It consumes no
   dark-helix state. dark-helix consumes temper-git as a service.

5. **Everything above git is optional but compatible.** Webhooks, Actions-like
   CI, Issues, Discussions, Releases — if we ship them, they match GitHub's
   API surface. If we don't ship them, temper-git still works as a git
   host; those just return 404.

6. **Mirror-friendly.** `git push --mirror temper-git → github.com` must
   produce an identical repository. `git push --mirror github.com →
   temper-git` must produce an identical repository. Because byte-exact
   hashes and canonical serialization guarantee it.

## What we explicitly are NOT building

- **A competitor to GitHub for the public internet.** temper-git is
  cluster-internal; external exposure is a user's deployment choice, not our
  product surface.
- **A web UI replica.** Humans who want a browsable UI either use the
  dark-helix Observe UI's code-view widget (separate work in the dark-helix
  project) or set up a public read-only mirror on GitHub/Gitea/whatever.
- **Git LFS support.** Out of scope for v1. Possibly never — the TreeEntry
  model handles files up to blob-store-limit size (low-MB) inline and
  spills larger files to the blob store; that covers >99% of real repo
  content.
- **Forks.** Forks in GitHub's sense are a social-layer concept. If a project
  needs them, the `Repository` entity can be trivially duplicated, but we
  won't ship a first-class "fork" action until there's demand.
- **Private-repo billing / tier enforcement.** Every repo is private by
  default; Cedar controls who can read.

## References

- [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
- [docs/adr/0002-temper-native-scm.md](docs/adr/0002-temper-native-scm.md)
- [docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md)
- [docs/adr/0004-per-repo-libsql-gcs.md](docs/adr/0004-per-repo-libsql-gcs.md)
- [docs/rfc/0001-temper-git-v1-architecture.md](docs/rfc/0001-temper-git-v1-architecture.md)
