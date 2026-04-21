# temper-git

Temper-native, byte-exact, GitHub-compatible SCM. Installs as an OS-app
into any Temper kernel; exposes a git wire surface + a GitHub REST v3
subset that tools like `git`, the GitHub CLI, `hub`, and vanilla
`libgit2` clients can talk to unmodified.

## Why

Agents running on Temper can't push code to GitHub — the blast radius
of a leaked agent credential that touches GitHub is unacceptable.
Rather than sandbox GitHub, we host the authoritative SCM ourselves.
`dark-helix` depends on a running `temper-git` instance; `temper-git`
is a standalone product that any other Temper deployment can use the
same way.

See [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
and [docs/rfc/0001-temper-git-v1-architecture.md](docs/rfc/0001-temper-git-v1-architecture.md)
for the full design.

## Entities

The app ships 13 IOA entities under the `Temper.Git` OData namespace:

| Family | Entities |
|---|---|
| Git objects (immutable, SHA-1-addressed) | `Blob`, `Tree`, `TreeEntry`, `Commit`, `Tag` |
| Repository container | `Repository` |
| Pointers | `Ref` |
| Social / review flow | `PullRequest`, `Review`, `ReviewComment` |
| Ops | `GitToken`, `Webhook`, `HttpEndpoint` |

Each entity has a matching Cedar policy under [`policies/`](policies/).
Scopes mirror GitHub fine-grained PATs: `repo:read`, `repo:write`,
`pr:write`, `pr:merge`, `admin:repos`, `admin:tokens`, `force`, plus
`admin:platform` for the `HttpEndpoint` kernel-delta entity.

## Surfaces (when K-1 + K-2 land upstream)

| Route | Purpose |
|---|---|
| `GET /{owner}/{repo}.git/info/refs` | Smart-HTTP advertisement |
| `POST /{owner}/{repo}.git/git-upload-pack` | Fetch/clone |
| `POST /{owner}/{repo}.git/git-receive-pack` | Push |
| `GET/PUT /api/v3/repos/{o}/{r}/contents/{path}` | GitHub REST content |
| `GET/POST/PATCH /api/v3/repos/{o}/{r}/pulls` | Pull request CRUD |
| `PUT /api/v3/repos/{o}/{r}/pulls/{n}/merge` | PR merge |
| `GET/POST/PATCH/DELETE /api/v3/repos/{o}/{r}/git/refs` | Ref CRUD |
| `GET/POST /api/v3/user/repos`, `GET/PATCH/DELETE /api/v3/repos/{o}/{r}` | Repo CRUD |

Route registration depends on temper's `HttpEndpoint` entity
(upstream ADR-0056) and `http_call_streaming` WASM primitive
(upstream ADR-0057). Until both land, the OData data surface works
but the git/REST surfaces 501.

## Byte-exact gate

Objects are SHA-1-addressed using the canonical git serialization
produced by [`canonical/`](canonical/) (internal Rust crate, shares
TigerStyle discipline with Temper). 11 integration tests round-trip
objects against the real `git` CLI — if any diverges, CI fails.
No bare-repo spill; no second content-addressable tier; git wire
is the authoritative protocol. See
[docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md).

## Storage

Per-repository libSQL database with GCS-backed WAL (self-hosted
libsql-server, swappable with Turso Cloud via env var). Blobs live in
the per-repo DB — no separate object store. See
[docs/adr/0004-per-repo-libsql-gcs.md](docs/adr/0004-per-repo-libsql-gcs.md).

## Installation

As a Temper OS-app. Drop the bundle into the Temper kernel's
`TEMPER_OS_APPS_DIR` and set `TEMPER_AUTO_INSTALL_APPS=true`; Temper
discovers the app on boot, verifies specs through L0–L3, registers
the entity sets, and (once K-1 lands) wires the HTTP routes.

For the dark-helix deployment, temper-git runs as its own Temper pod
in a separate Kubernetes namespace so the factory can depend on the
SCM without being able to mutate it.
