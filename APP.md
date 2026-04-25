# temper-git

Temper-native, byte-exact, GitHub-compatible version control. Installs
as an OS-app into any Temper kernel; exposes a git wire surface + a
GitHub REST v3 subset that tools like `git`, the GitHub CLI, `hub`,
and vanilla `libgit2` clients can talk to unmodified.

See [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
and [docs/rfc/0001-architecture.md](docs/rfc/0001-architecture.md) for
the full design.

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
`admin:platform` for `HttpEndpoint`.

## Surfaces

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

Route registration uses Temper's `HttpEndpoint` entity and
`http_call_streaming` WASM primitive.

## Byte-exact gate

Objects are SHA-1-addressed using the canonical git serialization
produced by [`canonical/`](canonical/). Integration tests round-trip
objects against the real `git` CLI; if any diverges, CI fails. No
bare-repo spill; no second content-addressable tier; git wire is the
authoritative protocol. See
[docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md).

## Installation

As a Temper OS-app. Drop the bundle into the kernel's
`TEMPER_OS_APPS_DIR` and set `TEMPER_AUTO_INSTALL_APPS=true`; Temper
discovers the app on boot, verifies specs through L0–L3, registers
the entity sets, and wires the HTTP routes.

For local testing, the [README](README.md) quickstart runs it all on
a single machine with an embedded database.
