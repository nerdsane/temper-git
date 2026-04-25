# temper-git

An exploration of what version control could look like for Dark Factories.

A Dark Factory, as Sesh and I have been thinking about it, is the software
analogue of a lights-out manufacturing line — autonomous agents producing
code continuously, at machine rate, with humans supervising through an
evolution record rather than a ticket queue. The pattern is new enough
that it's still being shaped, and part of what we're doing here is finding
out what tools it actually needs.

Git and GitHub are extraordinary. Linus's design has survived two decades
of load that its authors couldn't have anticipated, GitHub built an
entire industry around making collaboration visible, and the tooling
ecosystem they enabled is one of the reasons software engineering moves
as fast as it does. None of that is in question. This project is not an
attempt to replace them or to argue they're doing anything wrong — I use
GitHub daily and most of this repo will live on it. What we wanted to
explore is a different question: if you were designing version control
today, specifically for a workload where the primary writer is a program
rather than a human, what might you reach for first?

The answer we're experimenting with is that the git objects themselves —
blobs, trees, commits, refs — could be versioned entities with their own
state machines and policy gates, evaluated by the same machinery that
handles every other piece of state in the system. Pull requests and
reviews sit in the same model as the objects they point at. Authorization
is policy-as-code, applied at every transition. The event log is
authoritative and replayable. The git wire protocol is preserved
byte-for-byte, because nothing about this experiment should require a
human to learn a new client.

Whether this is actually a better fit for Dark Factories than a
well-configured GitHub organization is something we don't know yet.
What we have so far is a running prototype where a real `git` client
can push commits and clone them back, with every blob, tree, commit,
and ref landing as a versioned entity in the same authoritative log
that holds every other piece of state in the system. The hard parts
— pack delta compression, the full GitHub REST surface, scale, policy
ergonomics — are still ahead. If the shape is interesting to you, the
quickstart below should get you from clone to a local end-to-end demo
in a few minutes, and the ADRs and RFCs walk through the decisions
we've made and haven't made.

## What we're exploring

A handful of properties we're trying out. Each one is a place where we've
chosen a different tradeoff than the usual git + GitHub deployment, and
most of those tradeoffs might turn out to be wrong. We're mostly curious
to see.

**Git objects as versioned entities.** In a standard deployment, a blob
is bytes on a filesystem inside `.git/`. Here, a blob is a row with a
state machine, created by an action that carries its canonical bytes and
content hash. The motivation: in a Dark Factory the interesting questions
tend to be "which commits did this agent produce in the last hour, and
what did the policy engine decide about each of them" — those answers
live more naturally next to the entities than in a walk of the
filesystem. The cost: we have to teach the system about git's byte-exact
serialization rules ourselves, and keep that discipline wherever the
objects get read or written. The canonical library does this and is
tested against real git output; so far it has held.

**Refs as compare-and-swap transitions.** A `git push` that races another
agent is, in the standard model, handled by `push --force-with-lease` on
the client side. Here, a ref is an entity whose Update action takes an
expected old SHA as a parameter; the action is rejected at the state
level if the current SHA doesn't match. The difference is small but it
matters at machine rate — two agents pushing to the same branch produce
one accepted update and one cleanly rejected one, not a silent last-write-
wins.

**Pull requests, reviews, comments in the same model.** GitHub stores
these in its own database alongside the git objects; they relate by
reference but live in separate systems, and the join happens at the UI
layer. Here they're entities in the same event log as the commits they
point at. No ID translation, no two-phase write when a review resolves a
comment on a commit. We suspect this matters for an agent querying "what
is the state of this PR right now", but we haven't stress-tested it.

**Policy as code, applied at every transition.** Rules like "only
maintainers can merge", "protected branches reject force-pushes", "a
review must exist before merge" are declarative files attached to each
entity, evaluated on every action. This is the part that most obviously
differs from the GitHub model, where the same rules are a mixture of
branch-protection settings, webhook hooks, and review app
integrations. We think moving them into a single uniform layer makes
them easier to reason about and easier for the system to verify — but we
also know that single uniform layers have a track record of being harder
to extend than they look.

**Authoritative event log.** Every state change is an event; the current
state is a projection over the log. This is already true of git locally
(a reflog is exactly this), but it's usually not true of the server
side. What we get by making the server's log authoritative: replay for
debugging, queryable history beyond the 90-day GitHub audit window, and
an exact answer to "what happened on this repo at 3am last Tuesday". The
cost is obvious: we're taking on log infrastructure that GitHub has
already solved at scale. For a single-tenant factory deployment this feels
manageable; at scale, less obviously so.

**Scoped, programmatic credentials.** A push-only token, scoped to one
repository, issued to one agent, revocable independently. GitHub has the
pieces for this too — fine-grained PATs, GitHub Apps, deploy keys — but
the defaults point toward longer-lived, broader-scoped credentials
because they're easier for humans. Here the narrow credential is the
default and the broader one requires an explicit decision. Again, this
might not matter; it might turn out that GitHub's current surface is
already sufficient for this.

**Git wire, unchanged.** None of the above touches what the `git` binary
sees on the wire. Advertisements, pack format, pkt-line framing — all
byte-for-byte parity with what git expects. This is non-negotiable: any
tool that speaks to a standard git server should speak to this one, and
if it doesn't, that's a bug.

## Status

Running locally end-to-end today:

- `git push` of one or more commits — every object lands as a
  versioned entity, with byte-exact SHA-1 verification and
  compare-and-swap on refs.
- `git clone` of populated repositories — refs advertise, the
  commit/tree/blob graph streams back to the client as a pack the
  real `git` binary accepts.
- `git ls-remote` against empty and populated repositories.
- Byte-exact serialization parity with the real `git` binary, tested
  against its output on every commit.

In flight (roadmap in
[`docs/rfc/0002-push-and-clone.md`](docs/rfc/0002-push-and-clone.md)):

- Pack delta compression. The current build sends one full object per
  pack entry, which is correct but ~5× larger on the wire than git's
  default.
- Push-to-create semantics for new repositories.
- Server-side hooks (pre-receive, post-receive).

## Quickstart (local)

No cloud, no containers. Three terminals, a few minutes.

```bash
# 1. Clone with the kernel submodule.
git clone --recurse-submodules https://github.com/nerdsane/temper-git.git
cd temper-git

# 2. Run the host-side tests (pure-Rust libraries — no kernel).
cargo test

# 3. Build the protocol handlers (sandboxed, deployed alongside the
#    kernel). They compile to wasm32-wasip1 because that's how Temper
#    runs sandboxed code; nothing about the wire protocol cares.
rustup target add wasm32-wasip1
cargo build -p git_upload_pack -p git_receive_pack \
  --target wasm32-wasip1 --release
mkdir -p wasm/git_upload_pack wasm/git_receive_pack
cp target/wasm32-wasip1/release/git_upload_pack.wasm wasm/git_upload_pack/
cp target/wasm32-wasip1/release/git_receive_pack.wasm wasm/git_receive_pack/
```

Then start the kernel. It runs with an embedded database and loads the
temper-git bundle from the current directory:

```bash
# Terminal A
TEMPER_OS_APPS_DIR="$PWD" cargo run \
  --manifest-path temper/Cargo.toml \
  --release --bin temper \
  -- serve --port 3000 --storage turso --skill temper-git
```

In another terminal, register the wire-protocol routes and create a
repository:

```bash
# Terminal B — register routes
for row in \
  '{"Id":"he-info-refs","PathPrefix":"/{owner}/{repo}.git/info/refs","Methods":"GET","IntegrationModule":"git_upload_pack","RequiresAuth":false,"TimeoutSecs":60}' \
  '{"Id":"he-upload-pack","PathPrefix":"/{owner}/{repo}.git/git-upload-pack","Methods":"POST","IntegrationModule":"git_upload_pack","RequiresAuth":false,"TimeoutSecs":300}' \
  '{"Id":"he-receive-pack","PathPrefix":"/{owner}/{repo}.git/git-receive-pack","Methods":"POST","IntegrationModule":"git_receive_pack","RequiresAuth":false,"TimeoutSecs":300}'
do
  curl -sX POST -H "Content-Type: application/json" -d "$row" \
    http://127.0.0.1:3000/tdata/HttpEndpoints
done

# Create a repository (the Id convention is rp-{owner}-{repo})
curl -sX POST -H "Content-Type: application/json" \
  -d '{"Id":"rp-acme-demo","OwnerAccountId":"acme","Name":"demo",
       "DefaultBranch":"main","Visibility":"private"}' \
  http://127.0.0.1:3000/tdata/Repositories

# Push and clone with a real git client
mkdir demo && cd demo && git init -b main
echo hi > README && git add README && git commit -m init
git push http://127.0.0.1:3000/acme/demo.git main
cd .. && git clone http://127.0.0.1:3000/acme/demo.git demo-clone
diff -r demo demo-clone   # bit-identical working tree
```

That round-trip — push, clone, compare — is the end-to-end happy path
running today.

## What we don't know yet

A lot. A few things we're actively uncertain about:

- Whether the event-log-as-authority story holds at scale, or whether
  operational concerns push us back toward a more conventional
  projection-first model.
- Whether policy-as-code applied per transition is actually easier to
  reason about than branch-protection checkboxes, or whether we're just
  moving the complexity somewhere different.
- Whether Dark Factories, as a pattern, is distinct enough from "a team
  of humans using GitHub well" to justify a separate tool. That question
  is the whole pitch of this project, and we don't have a definitive
  answer.

Feedback, counterexamples, and "you're missing something obvious" notes
are welcome. The easiest way to start a conversation is to open an issue.

## Repository layout

```
temper-git/
├── VISION.md
├── README.md
├── CLAUDE.md              # guidance for agents working on this repo
├── CODING_GUIDELINES.md
├── CONTRIBUTING.md
├── app.toml               # kernel app manifest
├── APP.md                 # user-facing app doc
├── docs/
│   ├── adr/               # architectural decision records
│   └── rfc/               # design proposals ahead of implementation
├── specs/                 # entity specifications + data model
├── policies/              # authorization policies per entity
├── canonical/             # byte-exact git-object serialization + SHA-1
├── wire/                  # pkt-line + advertisement + pack-v2 parser/emitter
├── wasm-modules/          # protocol handlers (sandboxed wasm32-wasip1)
│   ├── git_upload_pack/   # /info/refs + git-upload-pack (clone)
│   └── git_receive_pack/  # /info/refs + git-receive-pack (push)
└── temper/                # kernel submodule
```

## Read next

- [VISION.md](VISION.md) — a longer walk through where this came from and
  what we've been thinking about.
- [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
  — why we decided to build something new instead of adapting an
  existing tool.
- [docs/adr/0002-temper-native-version-control.md](docs/adr/0002-temper-native-version-control.md)
  — every object is an entity; protocol handlers are isolated programs.
- [docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md)
  — the parity bar we hold ourselves to.
- [docs/rfc/0001-architecture.md](docs/rfc/0001-architecture.md) — the
  v1 architecture.
- [docs/rfc/0002-push-and-clone.md](docs/rfc/0002-push-and-clone.md) —
  what we need to build for full push + clone against populated repos.

## License

MIT OR Apache-2.0, at your option. See
[`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE-2.0`](LICENSE-APACHE-2.0).

## Built on

[git](https://git-scm.com/), and the [Temper](https://github.com/nerdsane/temper)
kernel.
