# temper-git

**Self-hosted, Temper-native, GitHub-compatible source-control management.**

Git wire protocol. GitHub REST v3 + GraphQL v4. Every object — blobs, trees,
commits, refs, tags, pull requests, reviews — is a first-class Temper IOA
entity. No bare `.git` on disk. Byte-exact compatibility: any tool that
works against GitHub works against temper-git.

## Why

Agents running in WASM can't speak git wire protocol, so they need a git
server they can write to via OData. Humans still need `git clone`. Both
groups need the same repository, one source of truth. This is that.

## Status

Design phase. RFC-0001 out for review.

## Read first

1. [VISION.md](VISION.md)
2. [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
3. [docs/adr/0002-temper-native-scm.md](docs/adr/0002-temper-native-scm.md)
4. [docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md)
5. [docs/rfc/0001-temper-git-v1-architecture.md](docs/rfc/0001-temper-git-v1-architecture.md)
6. [CLAUDE.md](CLAUDE.md) — if you're an agent working on this repo
7. [CODING_GUIDELINES.md](CODING_GUIDELINES.md) — TigerStyle + temper-git additions

## Repo layout

```
temper-git/
├── VISION.md
├── CLAUDE.md                   # Agent operating guide
├── CODING_GUIDELINES.md        # TigerStyle + byte-exact additions
├── CONTRIBUTING.md
├── README.md                   # This file
├── docs/
│   ├── adr/                    # Architectural decisions (MADR)
│   ├── rfc/                    # Design proposals ahead of impl
│   ├── research/               # External investigations
│   └── design/                 # Working design notes
├── specs/                      # IOA TOML entity specs
├── policies/                   # Cedar policies per entity
├── wasm-modules/               # WASM protocol handlers
│   ├── git_upload_pack/
│   ├── git_receive_pack/
│   ├── github_rest_contents/
│   ├── github_rest_pulls/
│   ├── github_rest_refs/
│   ├── github_rest_commits/
│   └── github_rest_repos/
├── deploy/gke/base/            # K8s manifests
├── evolution/                  # O-P-A-D-I records per Temper convention
├── proofs/                     # TLA+/IOA proofs for invariants we own
└── temper/                     # Pinned Temper kernel submodule (NOT yet populated)
```

## Next step

RFC-0001 sign-off, then Phase 1 implementation (4-week target for v0.1.0).
See [docs/rfc/0001-temper-git-v1-architecture.md §Phase plan](docs/rfc/0001-temper-git-v1-architecture.md).

## Related

- Companion project: [../dark-helix/](../dark-helix/) — the factory that
  consumes temper-git as its source-control layer.
- Upstream: [github.com/nerdsane/temper](https://github.com/nerdsane/temper)
  — the Temper kernel we build on.

## License

TBD.
