# temper-git — Vision

## What this is

A version-control experiment for the setup I've been calling a Dark
Factory — software systems where autonomous agents produce code
continuously, at machine rate, with humans supervising through an
evolution record rather than a ticket queue. Byte-exact git wire
protocol, so any standard client works unchanged. Every git object
(blob, tree, commit, ref, tag) is a versioned entity with its own
state machine. Pull requests, reviews, and comments sit in the same
model as the objects they point at. Authorization is policy-as-code,
evaluated at every transition. The event log is authoritative and
replayable.

Whether this is actually a better fit than a well-configured GitHub
organization is something I don't know yet. A real `git` client can
push commits and clone them back today, with every object landing as
a versioned entity in the same authoritative log that holds every
other piece of state in the system. The hard parts — pack delta
compression, the full GitHub REST surface, scale, policy ergonomics —
are still ahead.

## Why bother

Git is remarkable. Linus's design has absorbed two decades of load its
authors couldn't have anticipated, and GitHub built an entire industry
on top of it. None of that is in question. I use GitHub daily and most
of this repo will live on it.

The question I wanted to explore is narrower: *if you were designing
version control today, specifically for a workload where the primary
writer is a program rather than a human, what might you reach for
first?*

Three things about the standard setup felt awkward, enough to try
something different. Not defects — mismatches between a
general-purpose tool and a narrow workload.

**The primary writer is a program, not a human.** In the setup I've
been building, agents run inside a sandbox without direct network
sockets or a forkable git binary. They reach out through a small set
of capability-scoped host functions. Implementing git's smart-HTTP pack
protocol inside that sandbox is possible but large; giving the
sandbox a git binary undoes the isolation; and neither option
integrates naturally with the rest of the system's authorization and
audit machinery. Every Dark Factory that uses GitHub today solves this
somehow — but the solutions are specific to git's client/server
asymmetry, not emerging naturally from the agent architecture.

**Repository state lives in two places.** Git objects are on a
filesystem; pull requests, reviews, and comments are in a database
behind a separate API. They relate by reference. Every feature that
touches both has to reconcile them: branch-protection rules against
commit author identity, review state against merge eligibility,
webhook delivery against repo visibility. Each reconciliation is
correct in isolation; I kept wondering what it would look like if they
were all in the same substrate.

**The audit record is partial.** Git's reflog is local; GitHub's
activity feed is excellent for humans but was not designed to be
queried as a primary operational surface. For a Dark Factory operator,
the question "what did which agent do on which repository in the last
hour" is the common case, not an edge case. It's the kind of query
that wants to run directly against the authoritative log, with the
same filters and indexes as every other query in the system.

## The shape I'm trying

A handful of choices, each a place where I've picked a different
tradeoff than the standard git + GitHub deployment. Most of these
tradeoffs might turn out to be wrong.

**Git objects as versioned entities with state machines.** A blob is a
row, created by an action that carries canonical bytes and a content
hash. A commit is a row with a state. The question "which commits did
this agent produce in the last hour, and what did the policy engine
decide about each of them" lives naturally next to the entity, rather
than as a walk of a filesystem plus a join into a separate audit
system. The cost is that byte-exact git serialization is now *my*
responsibility, tested against the real `git` binary. So far it holds.

**Refs as compare-and-swap transitions.** At machine rate, two agents
pushing to the same branch should produce one accepted update and one
cleanly rejected one, not a silent last-write-wins that the losing
agent discovers on the next pull. Ref updates here take an expected
old SHA and reject at the state level if the current SHA doesn't
match. Small change; matters more as concurrency rises.

**Pull requests, reviews, and comments in the same model.** GitHub
stores these in its own database alongside the git objects; the join
happens at the UI layer. Here they're entities in the same event log
as the commits they point at. No ID translation, no two-phase write
when a review resolves a comment on a commit.

**Policy as code, applied at every transition.** Rules like "only
maintainers can merge", "protected branches reject force-pushes", "a
review must exist before merge" are declarative files attached to each
entity, evaluated on every action. This is the part that most
obviously differs from the GitHub model, where the same rules are a
mixture of branch-protection settings, webhook hooks, and review app
integrations. I think moving them into a single uniform layer makes
them easier to reason about and easier for the system to verify — but
single uniform layers have a track record of being harder to extend
than they look.

**Authoritative event log.** Every state change is an event; the
current state is a projection. This is already true of git locally
(the reflog is exactly this), but it's usually not true of the server.
What you get from making the server's log authoritative: replay for
debugging, queryable history beyond any audit window, and an exact
answer to "what happened on this repo at 3am last Tuesday". The cost
is obvious — log infrastructure is work GitHub has already solved at
scale.

**Scoped, programmatic credentials.** A push-only token, scoped to
one repository, issued to one agent, revocable independently. GitHub
has the pieces for this too — fine-grained PATs, GitHub Apps, deploy
keys — but the defaults point toward longer-lived, broader-scoped
credentials because they're easier for humans. Here the narrow
credential is the default; the broader one requires an explicit
decision.

**Git wire, unchanged.** None of this touches what the `git` binary
sees on the wire. Advertisements, pack format, pkt-line framing —
byte-for-byte parity with what git expects. Any tool that speaks to a
standard git server should speak to this one, and if it doesn't,
that's a bug.

## What I'm NOT trying to build

- **A replacement for GitHub as a public collaboration platform.** This
  is version control tailored for a specific operational pattern, not
  a product play against an established one.
- **A browser-based code browsing UI.** For now, humans who want to
  browse read through git clients or through outbound read-only
  mirrors.
- **Git LFS, submodule-first workflows, or "fork" semantics as v1
  features.** Possibly later; not goals now.
- **Hosted multi-tenant SaaS.** The project is meant to be run inside
  one operator's environment. Public hosting is somebody else's
  problem.

## What I don't know yet

Quite a lot.

- Whether the entity-model-for-git-objects premise holds at scale.
  Single-repo experiments look clean; I haven't stress-tested the
  pattern under the load of a repository with tens of thousands of
  commits or tens of concurrent pushing agents.
- Whether policy-as-code applied per transition is actually easier to
  reason about than GitHub's branch-protection + webhook + review app
  integration model, or whether I'm just relocating the complexity.
- Whether the Dark Factory pattern is distinct enough from "a team of
  humans using GitHub well" to justify a purpose-specific tool. The
  whole pitch of this project assumes the answer is yes; I don't have
  a definitive answer.

Feedback, counterexamples, and "you're missing something obvious"
notes are welcome. The easiest way to start a conversation is to open
an issue.

## References

- [docs/adr/0001-temper-git-mission.md](docs/adr/0001-temper-git-mission.md)
- [docs/adr/0002-temper-native-version-control.md](docs/adr/0002-temper-native-version-control.md)
- [docs/adr/0003-byte-exact-git-compat.md](docs/adr/0003-byte-exact-git-compat.md)
- [docs/rfc/0001-architecture.md](docs/rfc/0001-architecture.md)
- [docs/rfc/0002-push-and-clone.md](docs/rfc/0002-push-and-clone.md)
