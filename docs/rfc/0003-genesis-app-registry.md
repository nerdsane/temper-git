# RFC-0003: Genesis — temper-git absorbs an app registry

- Status: Draft
- Date: 2026-05-18
- Authors: Sesh, with design sessions captured in
  [project memory](https://example.invalid) (private)
- Related:
  - [RFC-0001](0001-architecture.md) (v1 architecture)
  - [RFC-0002](0002-push-and-clone.md) (push and clone)
  - [ADR-0001](../adr/0001-temper-git-mission.md) (mission)
  - [ADR-0003](../adr/0003-byte-exact-git-compat.md) (byte-exact git compat)
  - **New ADRs in this PR**:
    - [ADR-0004](../adr/0004-registry-scope-absorption.md)
    - [ADR-0005](../adr/0005-content-addressed-identity-and-owner-scoped-names.md)
    - [ADR-0006](../adr/0006-metadata-only-fork-via-lineage.md)
    - [ADR-0007](../adr/0007-content-addressed-dependency-closure.md)
    - [ADR-0008](../adr/0008-commons-as-public-service-and-v1-cost-guardrails.md)
  - **Companion ADRs in `nerdsane/temper`**:
    - ADR-0040 (composite-action kernel primitive)
    - ADR-0041 (in-process direct-dispatch)

## TL;DR

Apps that run on Temper (paw-heal, paw-harness, katagami, etc.) currently
live scattered across repositories with no canonical place, no sharing
story, and no record of who forked what from whom. This RFC proposes
expanding temper-git's scope from "byte-exact git wire over Temper kernel"
to "byte-exact git wire over Temper kernel **plus an app registry layered
on the same substrate**." A public commons kernel hosts the canonical
copies; each operator's private TemperPaw kernel runs the apps it cares
about and pulls from the commons on first boot. Lineage (who forked from
whom, who imported what from where) lives in a `Lineage` sidecar entity
beside the git repos. Publishing is `git push`. A web UI called Genesis
provides browsing, lineage visualization, and account management.

Two kernel primitives in temper proper — composite actions and in-process
direct-dispatch — are first-class v1 dependencies of this work, captured
in companion ADRs in `nerdsane/temper`.

---

## 1. Background

Reader who hasn't seen the rest of the project: skim this section.
Reader who has: skip to §2.

**Temper** is a kernel that runs governed application state as
state-machined entities with policy-as-code (Cedar) evaluated at every
transition. Lives at `nerdsane/temper`.

**temper-git** is a Temper app that exposes git objects (blobs, trees,
commits, refs) as entities in that kernel. It speaks byte-exact git wire
protocol — any standard `git` client works against it. Lives at
`nerdsane/temper-git` (this repo). Today, temper-git's stated scope is
"git for one operator's environment."

**TemperPaw** is an autonomous agent (formerly OpenPaw) plus a set of
`paw-*` apps (`paw-heal`, `paw-harness`, `paw-deploy-tracker`, …) that
operate on behalf of a human owner. Lives at `nerdsane/temperpaw`.
TemperPaw is the consumer of temper-git, not its host.

**The configuration today**: an operator (Sesh) runs a TemperPaw
deployment on Railway. Inside that deployment, the kernel runs
temper-git, the `paw-*` apps, and any user-facing apps the operator
cares about (e.g., katagami). Apps' source code is committed to GitHub
repos under various namespaces. There is no central registry, no
lineage record, no way for one operator's TemperPaw to pull an app
another operator built or evolved.

---

## 2. Problem

Three concrete pain points motivate this RFC.

**Apps are scattered with no canonical home.** `paw-heal` lives in
temperpaw's `os-apps/`; future agent-evolved variants have nowhere to
go; user apps like katagami live in their own GitHub repos. The
question "where do I find or publish a Temper app" has no good answer.

**No sharing across operators.** TemperPaw is designed for multiple
operators (Sesh and anyone else who builds on Temper). Today, one
operator cannot easily install an app another operator developed
without manually pointing GitHub clone URLs at a private repo. There
is no listing, no discovery, no version pinning.

**No lineage when an agent forks or imports.** A core Temper design
goal is that agents evolve apps autonomously: paw-heal v2 forked from
paw-heal v1, with three mutations and one module grafted from
paw-deploy-tracker. Git's commit graph captures commit ancestry, but
nothing in the system records *which app was forked from which*, what
the structural change was, or which modules were imported from where.
Without that record, agent evolution is invisible at the app level.

---

## 3. Goals and non-goals

### 3.1 Goals (v1)

1. **A canonical place to publish a Temper app**, content-addressed,
   with versions identified by hash.
2. **A canonical place to discover and install one**, both
   programmatically (so a TemperPaw bootstrap manifest can pull an app
   by hash) and visually (so a human can browse).
3. **An explicit lineage record** for every fork, with structured
   mutations (code edit, dep bump, imported module).
4. **A dependency model** that pins app closures by content hash, so
   two TemperPaw boxes with the same bootstrap manifest run bit-identical
   app stacks.
5. **Multi-tenant naming** in the commons kernel — `<owner>/<app>`
   pattern with real ownership verification.
6. **The git wire stays exactly as it is.** Byte-for-byte
   compatibility with `git` clients is non-negotiable; new app-level
   operations are entity actions, not git extensions.
7. **A web UI (Genesis)** for browse, lineage view, account
   management, install help.
8. **Cost-bounding guardrails** so the public commons can run without
   one bad actor running up Sesh's Railway bill.

### 3.2 Non-goals (v1)

- **Publisher signing** beyond the API auth gate at the commons.
- **Multi-commons federation.** v1 ships one default commons; the
  architecture is forward-compatible with federation but doesn't
  implement it.
- **Cross-ecosystem evolution signal** ("trending modules across all
  operators"). The Evolution Agent operates per-operator; the registry
  doesn't aggregate.
- **Hybridize as a first-class action.** Multi-parent grafts (HGT
  equivalents) are captured as `imported` mutations on single-parent
  forks; the DAG projection can already render them.
- **Bare-push p50 vs Gitea or huge-monorepo binary ops vs Sapling.**
  Scoping decisions, justified in §16.
- **Browser-based code browsing** beyond Genesis's app-level views.
  `git clone` covers source browsing; we don't ship a file viewer in v1.

---

## 4. Proposal overview

### 4.1 What temper-git becomes

temper-git absorbs the registry scope. New entity types live in its
spec bundle alongside the existing git objects:

| Entity (existing) | Entity (new) |
|---|---|
| `Blob`, `Tree`, `Commit`, `Tag` | `App` |
| `Ref` | `Lineage` |
| `Repository` | `Closure` |
| `PullRequest`, `Review`, `Comment` | (and a small `Owner` entity for accounts) |

The git side is unchanged in behavior — temper-git still speaks bog-standard
git wire, still byte-exact compatible. The new entities are *metadata
layered over* git repos; they don't modify or extend the git data model.

### 4.2 Two deployment modes

temper-git becomes deployable in two modes:

- **Operator mode** (the existing one): one operator, their apps and
  data. What TemperPaw runs today.
- **Commons mode** (new): public service, multi-tenant by owner, hosts
  many publishers' apps. The "registry."

These are the same code with different configuration: in commons mode,
ownership is enforced on every write, rate limits apply, and
abuse-handling endpoints are enabled. There is no separate "commons
binary."

### 4.3 The architecture, in one diagram

```
                ┌──────────────────────────────────────────────┐
                │  Commons kernel (public Temper deployment)   │
                │  ─ runs temper-git in commons mode           │
                │  ─ hosts canonical bytes + App/Lineage/      │
                │    Closure metadata                          │
                │  ─ multi-tenant: ownership scoped by         │
                │    authenticated identity                    │
                │  ─ serves git wire on /owner/app.git         │
                │  ─ Genesis web UI                            │
                └──────────────────────────────────────────────┘
                       ▲              ▲              ▲
                       │              │              │
            federation hop (publish + install only — never on the
                          operator's hot path)
                       │              │              │
                       │              │              │
         ┌─────────────┘              │              └──────────────┐
         │                            │                             │
  ┌────────────────┐         ┌────────────────┐            ┌────────────────┐
  │ Sesh's         │         │ Other op's     │            │ Yet another    │
  │ TemperPaw      │         │ TemperPaw      │            │ TemperPaw      │
  │ (Railway)      │         │                │            │                │
  │                │         │                │            │                │
  │ Runs temper-   │         │ Same           │            │ Same           │
  │ git in         │         │                │            │                │
  │ operator mode  │         │                │            │                │
  │                │         │                │            │                │
  │ Apps:          │         │ Apps:          │            │ Apps:          │
  │  • paw-heal    │         │  • paw-heal    │            │  • their own   │
  │  • paw-harness │         │  • their own   │            │  • paw-heal    │
  │  • katagami    │         │    forks       │            │    (fork)      │
  │                │         │                │            │                │
  │ Agent hot path │         │ Same           │            │ Same           │
  │ = regime A     │         │                │            │                │
  │ (~100µs)       │         │                │            │                │
  └────────────────┘         └────────────────┘            └────────────────┘
```

Each operator's TemperPaw is a private kernel running their own
temper-git. The commons kernel is one shared public Temper deployment.
The two communicate by git wire (for bytes) and authenticated OData
(for App / Lineage / Closure rows) on **publish** and **install** only;
the operator's agent does its actual work inside its own kernel, in
regime A (in-process, microsecond-scale).

---

## 5. Storage: where bytes and metadata live

| What | Where |
|---|---|
| App source bytes (specs, WASM, policies, manifest) | git repos in temper-git — both in the operator's local kernel (working copy) and in the commons kernel (canonical) |
| `App` row (name, owner, exports, manifest digest) | temper-git entity table in the kernel that owns it |
| `Lineage` row (parent, mutations) | same |
| `Closure` row (resolved dep graph by hash) | same |
| Running app instance data (e.g., katagami's actual posts) | private to the operator's kernel; never leaves |
| `Owner` row (account, public key, contact) | commons kernel only |

GitHub demoted to **optional backup mirror.** The commons kernel is
the canonical source. Operators can configure their TemperPaw to mirror
to GitHub for redundancy, but the registry doesn't depend on it.

---

## 6. Identity and naming

### 6.1 Hashes are truth

Every app version is identified by its content hash, computed
deterministically over `(app.toml + specs/ + wasm/ + policies/ + dep_hashes)`.
Two byte-identical apps produced by different agents = same hash =
same specimen.

### 6.2 Names are ergonomic sugar

Names take the form `<owner>/<app>[@<version-or-hash>]`. Examples:

- `nerdsane/paw-heal@1.4`
- `nerdsane/paw-heal@7a3f8e2c1d4b...`
- `evolution-7/paw-heal:from(nerdsane/paw-heal@1.4)`

The version label is human-friendly; the resolution to a hash is
unambiguous and happens at lock time. Owner-scoping is enforced
in the commons mode: only authenticated `nerdsane` can publish under
`nerdsane/...`.

See [ADR-0005](../adr/0005-content-addressed-identity-and-owner-scoped-names.md).

---

## 7. Forks and lineage

### 7.1 Forks are metadata-only

There is no `Fork` primitive in temper-git's git layer. Forking is:

```
1. CreateRepository(rp-evolution-7-paw-heal)         ← new repo, new id
2. Set its main ref to 7a3f                          ← point at parent's commit
                                                       (no byte copy: content-
                                                       addressed objects shared)
3. CreateLineage { child, parent, type: "fork" }     ← new metadata
```

No bytes are copied. Commits, trees, and blobs are shared across repos
via content addressing — the fork only diverges in storage when the
forking agent makes new commits.

See [ADR-0006](../adr/0006-metadata-only-fork-via-lineage.md).

### 7.2 The `Lineage` entity

```
Lineage {
  Id: "ln-...",
  child_repo: "rp-evolution-7-paw-heal",
  parent_repo: "rp-temper-genesis-paw-heal",
  parent_commit: "7a3f...",
  type: "fork",                            // "fork" | (future: "merge")
  created_by: "agent:evolution-7",
  created_at: 2026-05-18T18:42:00Z,
  mutations: [
    { kind: "edit",      target: "specs/heal_rule.ioa.toml" },
    { kind: "imported",  source: "paw-deploy-tracker@9c12...",
                         module: "deploy-ingestor" },
    { kind: "dep_bump",  target: "temper-git", from: "@b21c", to: "@d4e8" },
  ],
}
```

The `mutations` array captures structured semantics that git's commit
graph cannot:

- **`edit`** — a code change. References the path in the spec bundle.
- **`imported`** — a module grafted from another lineage. Cited by hash.
  This is the v1 stand-in for "Hybridize." The DAG projection can render
  this as a multi-parent edge without needing a first-class Hybridize
  action.
- **`dep_bump`** — a dependency hash changed.

The phylogenetic projection walks `Lineage` rows to produce the DAG.

---

## 8. Dependencies and closures

### 8.1 Apps as the dep unit

Each app declares dependencies on other apps by hash. The unit of
dependency is the app, not individual specs or modules. Inside an app,
everything versions together.

This is the granularity decision: "apps as the dep unit, with future
upgrade to content-addressed file dedup if storage bites." See the
sustained discussion captured in design memory.

### 8.2 `app.toml`

```toml
name = "paw-heal"
version = "1.4.0"            # human-readable label; identity is the hash

[deps]                       # pinned by hash — this is the lock
temper-git = "@b21c4f8a..."
user-app   = "@e44a7c2b..."

[deps.hints]                 # optional, used by the resolver pre-lock
temper-git = "^1.4"          # at lock time, resolves to a hash

[exports]
entities = ["HealRule", "HealAttempt", "HealOutcome"]
actions  = ["TriggerHeal", "RecordOutcome"]

[lineage]                    # populated by Fork; not hand-written
parent = "paw-heal@7a3f..."
mutations = [...]
```

### 8.3 Closures pin the entire dep graph

A `Closure` is the lockfile, but as a Temper entity:

```
Closure {
  Id: "cl-...",            // hash of (root + resolved + resolver_version)
  root: "paw-heal@7a3f...",
  resolved: {
    "paw-heal":   "@7a3f8e2c...",
    "temper-git": "@b21c4f8a...",
    "user-app":   "@e44a7c2b...",
  },
  resolver_version: "1.0",
  resolved_at: 2026-05-18T18:42:00Z,
  resolved_by: "agent:evolution-7",
}
```

Properties:

- **Immutable.** The ID is itself a hash; two identical resolutions
  produce the same Closure ID.
- **Bootstrap = one Closure ID.** `temperpaw/bootstrap.toml` is a
  single line: `closure = "cl-..."`. The closure pins everything below
  it. Two operators with the same bootstrap manifest run identical
  systems.
- **Phylogeny over closures is free.** The Lineage of a Closure = union
  of Lineage of its members. The Evolution Agent (per-operator) and
  the registry-level discovery layer (future) both query the same
  Lineage table.
- **Fork inherits closure.** When an agent forks an app, the new app's
  `[deps]` is copied verbatim. A later dep change is an explicit
  `dep_bump` mutation; absent that, lineage divergence is *code only*.

See [ADR-0007](../adr/0007-content-addressed-dependency-closure.md).

---

## 9. Publishing — git push, period

There is no `genesis publish` command. To publish a new version of an
app:

```
git push https://commons.temperpaw.dev/nerdsane/paw-heal.git main
```

That's bog-standard git. The commons kernel speaks git wire (because
it runs temper-git); `git push` works against it like any git host.
**1:1 git equivalence is preserved.**

The only Temper-specific overhead is the *first time* an app exists:
the `App` row needs to be created. Two ways:

- **Explicit (`genesis init <name>`):** creates Repository + App rows
  in the commons, after which `git push` works.
- **Push-to-create (in flight in temper-git's RFC-0002):** push to a
  non-existent commons URL auto-creates the Repository + a default App
  row.

Both are git-native. Neither introduces a "publish" verb.

The flow:

```
First-ever publish:
  1. Repository.Create   (rp-nerdsane-paw-heal)         [Temper action]
     OR auto-on-first-push (push-to-create)
  2. git push            (objects + refs land in the repo) [git wire]
  3. Apps.Create         ("this repo is an app named         [Temper action]
                          paw-heal, owned by nerdsane")

Every subsequent publish:
  1. git push            (advance the ref)               [git wire]
  (No more Temper-specific writes; the App row is already there.)
```

**Synchronous v1.** `git push` against the commons blocks until the
commons confirms (regime C network RTT — ~30–150ms cross-region,
single-digit ms same-region). Async publish (queue-and-push-background)
is a v2 optimization.

**Not the hot path.** The operator's agent works locally in their
private kernel at regime A (microsecond scale). Publishing happens
when the operator decides to share — not on every commit. RTT cost
is paid once per publish, not per work-cycle iteration.

---

## 10. Kernel changes — first-class v1

This RFC depends on two kernel primitives in temper proper. Both are
captured as ADRs in `nerdsane/temper` and merged together with this
RFC as a coordinated change.

### 10.1 Composite-action kernel primitive

ADR-0040 (`nerdsane/temper`). A composite action is *one* kernel
action that internally performs multiple sub-writes within a single
transaction, gated by a single Cedar evaluation, emitting a single
event in the log.

Existing examples in temper-git: `Repository.WriteFile`,
`Repository.BatchWriteFiles`, `Repository.MergePullRequest`. These
work today and demonstrate the pattern. What's missing is **a
first-class kernel feature** that any spec can declare a composite
action; today they're hand-rolled per app.

Why this RFC needs it:

- `Repository.IngestPack` (the missing piece in temper-git RFC-0002)
  is naturally a composite action: one Cedar gate at the repo level,
  N object inserts + 1 ref update in one transaction.
- `Apps.PublishNewVersion` similarly composes ref advance + optional
  App row update.

Without this primitive, the wire path stays at N OData round-trips and
keeps the latency problem the transmission log documents.

### 10.2 In-process direct-dispatch

ADR-0041 (`nerdsane/temper`). Today, a WASM module inside the kernel
that wants to call a Temper action does:

```
WASM → host_http_call → HTTP router → OData parser → action dispatcher
```

For an in-kernel caller, the HTTP and OData layers are overhead — the
caller already has typed arguments. Direct-dispatch is a new host
function:

```
host_call(action_id, typed_args)
```

that lands directly in the action handler. Same governance applies
(Cedar still gates; state machine still validates; event still
appends) — just without the parsing tax.

Why this RFC needs it:

- The agent's hot loop inside an operator's TemperPaw makes many
  in-kernel calls per work-cycle. With HTTP-and-OData, each is
  ~100µs–1ms. With direct-dispatch, each is ~10µs. This is the
  difference between "fast" and the regime-A "exceptional" the
  transmission log identifies.
- The commons kernel's own operations (install path, lineage queries
  during phylogeny rendering) similarly benefit.

### 10.3 What's NOT in scope for the kernel work

The transmission log lists six µs-floor primitives. v1 ships the two
above. The other four are documented as future work:

- Pre-compiled Cedar (decision tree at install time)
- Hot in-memory projections (durable log behind)
- Group-commit on the event log
- Reactive subscriptions inside the log

Each is a 10–50× latency improvement on a specific dimension. None is
required for correctness of the registry or the hot path; they're
optimizations.

---

## 11. The Genesis UI

A web frontend served by the commons kernel. v1 scope:

- **Browse apps.** Search by name, owner, exports. Owner pages.
- **App detail.** Description, current canonical version (latest hash),
  exports, dep graph, recent forks.
- **Lineage view.** The DAG visualized — descendants, ancestors,
  mutations. Like `git log --graph` but for apps.
- **Fork compare.** Diff between a fork and its parent. Both code
  diffs (via git) and mutation summaries (from the Lineage row).
- **Install help.** Copy the hash or Closure ID for an operator's
  bootstrap manifest.
- **Account sign-in.** Owner registration and management.

Deferred to v2:

- Cedar-policy-queryable lineage (used by governance rules)
- Dedicated phylogeny HTTP endpoint (OData baseline is enough for v1)
- Cross-commons federation browsing
- Browser-based source viewer (use `git clone`)

The UI is a thin frontend over the OData entity API plus the
commons-mode HTTP endpoints. No new substrate.

---

## 12. Cost-bounding guardrails (v1)

The commons is a public service Sesh operates. Bounded so a runaway
agent or bad actor can't generate a Railway bill while the operator is
asleep.

| Guardrail | Implementation |
|---|---|
| Per-owner storage cap | Cedar check on App/Repository writes |
| Write rate limits (publishes/hour/owner) | Token bucket per owner |
| Pull rate limits (per-IP, per-account) | Token bucket per principal |
| Content-addressed dedup | Already structural (same bytes → one copy) |

Specific numbers (e.g., 10GB per owner, 100 publishes/hour) live in
the operational design doc, not this RFC. The architecture supports
arbitrary policy; the policy is operational.

See [ADR-0008](../adr/0008-commons-as-public-service-and-v1-cost-guardrails.md).

---

## 13. Self-hosting and bootstrap

temper-git and temper-genesis (the registry surface in temper-git)
themselves run on the kernel as Temper apps. Their bytes are
git repos, hosted by temper-git, on the kernel that runs them. The
recursion resolves at first boot:

1. The TemperPaw binary ships with temper-git's bytes baked in
   (or in an `os-apps/` directory for the initial seed).
2. Once temper-git is running, it hosts its own repo. From that point,
   updates to temper-git flow through normal `git push` against the
   commons kernel.
3. Same for any other app shipped as part of the platform.

The bootstrap manifest reduces to one line:

```toml
# temperpaw/bootstrap.toml
closure = "cl-..."
```

That closure pins temper-git, temper-genesis (the registry features),
and any other v1-shipped apps by hash. Two operators with the same
bootstrap manifest get bit-identical systems.

---

## 14. Implementation phases

High-level. The detailed plan is produced by plan mode and lives in
the implementer-agent handoff.

### Phase 1 — Kernel primitives
- Composite-action primitive in temper kernel (ADR-0040)
- In-process direct-dispatch (ADR-0041)
- Tests against existing apps (paw-heal, paw-harness)

### Phase 2 — Registry entities
- `App`, `Lineage`, `Closure`, `Owner` specs in temper-git
- CRUD + composite actions (`Apps.PublishNewVersion`, `Apps.Fork`)
- Cedar policies for owner-scoped enforcement

### Phase 3 — Commons mode
- temper-git config switch for commons mode
- Multi-tenant ownership enforcement
- Rate-limit middleware
- Storage cap enforcement

### Phase 4 — Push-to-create + IngestPack
- `Repository.IngestPack` composite action (temper-git RFC-0002)
- Push-to-create for first-time app publication

### Phase 5 — Genesis UI
- React/Next frontend served by commons kernel
- Browse, app detail, lineage, fork-compare, install help
- Account sign-in

### Phase 6 — Operational
- Public commons deployment on a separate Railway box
- Account verification flow (OAuth)
- Abuse handling / takedown endpoints

---

## 15. Future work and forward-compatibility

Documented for direction; not implemented in v1.

- **Federation.** Anyone with Temper can run their own commons.
  Cross-commons references via `<owner>/<app>@<commons-host>` syntax.
  The architecture supports this — the commons is just a Temper
  deployment with the registry features enabled. Nothing structurally
  unique about it.
- **Cold/hot storage tiering.** Apps untouched for 30+ days move to
  S3 Glacier; re-warm on pull. 10×–50× storage cost reduction at
  scale.
- **CDN for `git clone`.** Cloudflare or similar in front of read
  endpoints. Caches public reads.
- **Read-replica pattern for popular apps.** Geographically replicate
  pulls for high-traffic apps.
- **Hybridize as a first-class action.** If the projection demands a
  distinct semantic from "imported" mutations, promote it.
- **Publisher signing.** Verified-publisher badges, key rotation,
  signature verification beyond the API auth gate.
- **Adoption signal mechanism.** Cross-ecosystem "trending modules"
  signal at the registry level (separate from per-operator Evolution
  Agent).
- **Cedar-queryable lineage.** Policies that reference lineage
  ("only forks of canonical paw-heal can be installed in production").
- **Async publish.** Queue + background push; useful if push sizes
  grow.
- **Paid tier for private apps / enterprise features.** When usage
  forces the question.
- **Sapling/Mononoke primitives.** Absorbed into temper-git for
  huge-monorepo workloads (out of v1 scope per transmission log
  positioning).

**Forward-compatibility statement:** The v1 architecture supports all
of the above as additive work. No v1 decision needs to be reversed to
enable any of these.

---

## 16. Scoping: what we explicitly don't try to beat

From the transmission log's competitive-position analysis:

- **Bare-push p50 vs Gitea.** Gitea wins at raw `git push` p50 (~5–10ms)
  because it does no governance. Real production "Gitea + branch
  protection + audit + webhook + secret-scan" deployments add ~200ms.
  temper-git's Cedar gate is paid once, atomically. Position: "the
  fastest *governed* push, not the fastest no-governance push." For
  the agent-resident path, `git push` doesn't exist as a hot operation
  — agents call `Repository.WriteFile` / `BatchWriteFiles` directly
  via direct-dispatch.

- **Huge-monorepo binary ops vs Sapling.** Sapling optimizes git CLI
  on 10M-file repos. Agents shouldn't run `git status`; they query
  indexed entity surfaces. v1 doesn't target Meta-scale; v2 absorbs
  Sapling primitives (Apache-licensed).

Both are deliberate scoping decisions, documented for honesty.

---

## 17. Open questions

1. **Account verification mechanism.** OAuth provider choice
   (GitHub OAuth? Email-magic-link? Both?). Lives in the operational
   design doc, but the choice affects v1 UI work.
2. **Owner-name dispute resolution.** First-come-first-served vs.
   trademark-style takedown vs. case-by-case. Operational policy
   decision.
3. **Initial commons deployment region.** Affects RTT for early
   adopters. Probably US-East to start.
4. **Bootstrap manifest format.** Confirmed as single `closure = "cl-..."`
   line, but the actual config file location in TemperPaw needs
   alignment with TemperPaw's existing config layout.
5. **Eviction policy for cold storage tiering** (future). When do
   apps go cold? When do they come back? Latency budget for warm-up?

---

## 18. References

- Design conversation memory: `project_temperpaw_registry_design.md`
  (private)
- temper-git latency findings: `project_temper_git_latency_regimes.md`
  (private; derived from `temper-git-transmission-log.html`)
- [VISION.md](../../VISION.md) (updated in this PR to reflect scope
  expansion)
- [temper repo](https://github.com/nerdsane/temper)
- [temperpaw repo](https://github.com/nerdsane/temperpaw)
- transmission log (private): `temper-git-transmission-log.html`
