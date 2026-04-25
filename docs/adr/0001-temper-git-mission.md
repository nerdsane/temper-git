# ADR-0001: Build a version-control experiment for Dark Factories

## Status

Accepted — this is the project's foundational decision. Implementation
is in progress; the claims here are intentionally bounded to what we've
had to decide, not what we've proven.

## Context

A Dark Factory, as we're using the term, is a software system where
autonomous agents produce code continuously, at machine rate, with
humans supervising through an evolution record (observation, problem,
analysis, decision, insight) rather than a ticket queue. We're building
one. Every cohort of agents in it — the ones producing tools, the ones
exercising them, the ones reviewing each other's work — needs to read
and write source code.

The standard answer is GitHub (or a self-hosted equivalent serving the
git wire protocol over a filesystem of bare repos). That answer works
for developer laptops and for most CI pipelines. Three things about it
felt awkward in the Dark Factory setting, enough to make us look at
alternatives:

1. **The primary writer is a program, not a human.** Agents in our
   setup run inside a sandbox without direct network sockets or a
   forkable git binary. They can reach out through a small set of
   capability-scoped host functions. Implementing the smart-HTTP pack
   upload protocol inside a sandbox is possible but large; giving the
   sandbox a git binary undoes the isolation; and neither option
   integrates with the rest of the system's authorization and audit
   machinery. This is solvable — every Dark Factory that uses GitHub
   today solves it — but the solutions are specific to git's
   client/server asymmetry rather than emerging naturally from the
   agent architecture.

2. **State for the repository lives in two places.** Git objects are
   on a filesystem; pull requests, reviews, and the conversations
   around them are in a database behind a separate API. They relate by
   reference. Every feature that touches both has to reconcile them:
   branch-protection rules against commit author identity, review
   state against merge eligibility, webhook delivery against repo
   visibility. Each reconciliation is correct in isolation; we found
   ourselves wondering what it would look like if they were all
   expressed in the same substrate.

3. **The audit record is partial.** Git's reflog is local; GitHub's
   activity feed is excellent for humans but was not designed to be
   queried as a primary operational surface. For a Dark Factory
   operator, the question "what did which agent do on which repository
   in the last hour" is the common case, not an edge case, and it's
   the kind of query that wants to run directly against the
   authoritative log.

None of these are criticisms of git or GitHub. They are both
extraordinary pieces of infrastructure; their conventions are a large
part of why software engineering moves as fast as it does, and our
project preserves those conventions wherever we can. The three points
above describe a mismatch between a general-purpose tool and a narrow
workload, not a defect in the tool.

We considered three approaches.

### Option A: Adapt a self-hosted git server with a write API

Stand up a standard git server and add a small REST layer that lets
the sandboxed agents mutate content indirectly: a "write file" endpoint
that clones, commits, and pushes on the agent's behalf. Lowest
upfront cost — a day or two of work.

We passed on this because the two-sources-of-truth problem
intensifies: the bare repo is authoritative for human clients, the
REST shim is authoritative for agents, and the pull-request /
review / branch-protection entities live in a third system. Every
future feature has to be reconciled across all three. The short-term
savings compound into long-term friction.

### Option B: Model content in an existing entity type

Use the kernel's generic "named blob with versions" entity as the
writable surface for agent-authored content, and keep a standard git
server as a read mirror maintained by a sync controller. Humans
`git clone` from the mirror; agents write via the entity's OData
surface.

We passed on this because the sync controller becomes a durability
bottleneck we don't fully trust — partial syncs can leave the system
in a state where "the code" means different things to agents and
humans. The generic entity is also designed for agent-remembered
state (playbooks, scratchpads) rather than content with git's
specific invariants around object identity, merge semantics, and
ref history; overloading it felt like a category error.

### Option C: Build a purpose-specific version control experiment

Model every git object — blob, tree, commit, tag, ref — as a
first-class entity with its own state machine, and serve the git
wire protocol from those entities directly. Keep byte-exact
compatibility with the real git binary so any existing tool works
unchanged. Keep the pull-request / review / comment surface in the
same entity model as the objects it points at.

This is the option we took.

## Decision

We're building **temper-git** as a stand-alone experiment in version
control designed for the Dark Factory workload. Every git object is an
entity with a state machine. Authorization is policy-as-code,
evaluated at every transition. The event log is authoritative. The
git wire protocol is preserved byte-for-byte.

### What the decision commits us to

- A stand-alone project with its own release cycle. The kernel it
  builds on ([Temper](https://github.com/nerdsane/temper)) is a
  submodule pinned to a specific upstream commit; we do not fork it,
  we extend it with an app bundle and with protocol handlers compiled
  to WASM.
- Byte-exact compatibility with real git — every object we emit must
  hash identically to what `git hash-object` produces, every
  advertisement and pack stream we produce must parse cleanly in the
  real `git` binary. We hold ourselves to this with integration tests
  that shell out to a real git, documented in
  [ADR-0003](0003-byte-exact-git-compat.md).
- A phased delivery we can walk back from if the core premise doesn't
  hold. The current phases are roughly: (1) ref advertisement and
  empty-repo clone, (2) `git push` landing objects as entities and
  `git clone` emitting packs from them, (3) GitHub-shaped REST
  endpoints for tooling compatibility, (4) hardening and operator
  workflows. We are currently between phase 1 and phase 2.

### What the decision does not commit us to

- A replacement for GitHub as a public collaboration platform. This
  is version control tailored for a specific operational pattern,
  not a product play against an established one.
- A browser-based code browsing UI. For now, humans who want to
  browse the code read it through git clients or through outbound
  read-only mirrors on github.com.
- Git LFS, submodule-first workflows, or "fork" semantics as a
  first-class feature. These might come later if the experiment goes
  well; they are not goals for v1.

## Consequences we expect

### What gets easier

- The agent's writing path doesn't have to understand git's
  transport protocol — it issues entity actions and the same
  authorization layer that handles every other action in the system
  handles these too. Our sandboxed agents can mutate source code
  with no new capability surface.
- The evolution chain terminates in code: an observation can point
  at an analysis, which can point at a pull request, which can
  point at a commit, all in one queryable substrate. An agent can
  walk from a production signal to the line of code responsible for
  it without leaving a single query protocol.
- The audit record is uniform and replayable. Every mutation is an
  event in the authoritative log; "what happened at 3am last
  Tuesday" is answerable by replaying the log, not by joining
  across systems.
- Mirror compatibility comes for free from byte-exactness. A
  `git push --mirror` to a regular git host produces identical
  hashes, so the same repository can be maintained as a read-only
  mirror on github.com with no special tooling.

### What gets harder

- We are now responsible for byte-exact serialization of git's
  object format and for the correctness of the wire protocol.
  The real `git` binary has absorbed two decades of edge cases
  that we have to re-encounter and match. Our discipline is to
  test against it rather than against ourselves.
- GitHub's REST API is a large, living surface. We will implement
  a subset; tools that hit endpoints we don't implement will
  fail. The subset we do implement has to be solid.
- Coupling to the kernel's upstream evolution. If the kernel's
  host functions or action dispatch change, we have to follow. We
  mitigate with a pinned submodule and deliberate upgrades, not by
  forking.

### What we don't know yet

- Whether the entity-model-for-git-objects premise holds at scale.
  Single-repo experiments look clean; we haven't stress-tested
  the pattern under the load of a repository with tens of
  thousands of commits or tens of concurrent pushing agents.
- Whether policy-as-code applied per transition is easier to
  reason about than GitHub's branch-protection + webhook + review
  integration model, or whether we're just relocating the
  complexity.
- Whether Dark Factories, as a pattern, is distinct enough from
  "a team of humans using GitHub well" to justify a
  purpose-specific tool. The whole pitch of this project assumes
  the answer is yes; we don't have a definitive answer yet.

We are taking the bet knowing these are open.

## References

- [VISION.md](../../VISION.md)
- [ADR-0002: Version control as an entity model](0002-temper-native-version-control.md)
- [ADR-0003: Byte-exact git compatibility](0003-byte-exact-git-compat.md)
- [RFC-0001: v1 architecture](../rfc/0001-architecture.md)
- [RFC-0002: push and clone roadmap](../rfc/0002-push-and-clone.md)
- Git wire protocol: https://git-scm.com/docs/http-protocol
- GitHub REST v3 reference: https://docs.github.com/en/rest
