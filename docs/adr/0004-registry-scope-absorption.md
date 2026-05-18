# ADR-0004: temper-git absorbs an app registry as first-class scope

## Status

Accepted — 2026-05-18. Establishes that temper-git is now both the
git substrate and the app registry that operates on top of it. Pairs
with [RFC-0003](../rfc/0003-genesis-app-registry.md).

## Context

[ADR-0001](0001-temper-git-mission.md) defines temper-git's mission
as "byte-exact git wire over Temper kernel" for a single operator's
environment. [VISION.md](../../VISION.md) lists explicit non-goals,
including:

- A replacement for GitHub as a public collaboration platform
- "Fork" semantics as v1 features
- Hosted multi-tenant SaaS
- A browser-based code browsing UI

The TemperPaw ecosystem now has a problem the original scope cannot
address. Apps that run on Temper (`paw-heal`, `paw-harness`, `katagami`,
agent-evolved variants) live scattered across GitHub repositories with
no canonical home, no sharing story, and no lineage record. The
question "where do I find or publish a Temper app, and what's its
provenance" has no good answer.

Three options for placing the registry:

1. **A separate new repo (`temper-genesis`).** Cleanest in terms of
   scope discipline; honors temper-git's VISION as-is.
2. **A separate Temper app, depending on temper-git.** No new repo;
   the registry is an OS app at the same layer as `paw-heal`.
3. **Absorb the registry into temper-git itself.** temper-git's scope
   expands; VISION updates.

## Decision

**Adopt Option 3. temper-git absorbs the registry as first-class
scope.**

Rationale:

- The registry is metadata-over-git — the same shape as GitHub's
  fork-network + database metadata, or npm's package index over tarballs.
  It is not a separate kind of system; it's git's natural
  organizational layer.
- Splitting into a separate repo or app fragments the substrate. The
  registry's identity model (content hashes, owner-scoped names) and
  storage layer (git repos) are temper-git's. The metadata entities
  (App, Lineage, Closure) are small additions that don't justify a
  separate codebase.
- temper-git's stated non-goals were point-in-time scoping, not
  permanent principles. They served while temper-git was finding its
  shape. Now that the shape is known, expanding to absorb the registry
  is consistent with the underlying vision (entity-up, governed,
  agent-native).
- The "two PRs total" working agreement (one on `nerdsane/temper`, one
  on `nerdsane/temper-git`) confirms only two repos in scope.

## What changes

### Scope (added to VISION.md)

temper-git now also provides:

- **A multi-tenant `commons mode`** suitable for hosting a public
  registry. Owner-scoped names. Cross-publisher isolation.
- **App-level entities** (`App`, `Lineage`, `Closure`, `Owner`) for
  registering, forking, and pinning dependency closures.
- **A web UI (Genesis)** for browsing, lineage visualization, and
  account management.
- **Cost-bounding guardrails** (per-owner storage caps, rate limits)
  for safe public operation.

### Scope (preserved from previous ADRs)

Everything previously committed: byte-exact git wire protocol, refs as
CAS transitions, PRs/reviews/comments as same-substrate entities,
policy-as-code per transition, authoritative event log, scoped tokens,
single-operator deployability. **All of these remain valid in operator
mode and inform commons mode.**

### Two deployment modes

- **Operator mode** (existing) — single owner, no rate limits, no
  cross-owner enforcement. TemperPaw runs in this mode today.
- **Commons mode** (new) — multi-tenant, ownership enforced on every
  write, rate limits active, abuse handling enabled.

Same binary, different configuration. There is no separate
"temper-registry" binary.

### What stays out of scope (v1)

- Publisher signing beyond API auth at the commons gate.
- Multi-commons federation (architecture is forward-compatible but not
  shipped).
- Hybridize as a first-class action (multi-parent grafts are encoded
  as `imported` mutations on single-parent forks).
- Cross-ecosystem evolution signal (Evolution Agent stays per-operator).

## Consequences

### Positive

- **Single substrate for the whole agent-VCS picture.** No
  "registry vs. version control" split that GitHub has between
  database and git.
- **VISION matches reality.** No tension between stated non-goals and
  actual roadmap.
- **Forward-compatible.** Federation, cold storage, CDN, signing are
  all additive on top of v1.

### Negative

- **temper-git's surface grows.** More entities, more actions, more
  policies. Discipline required to keep operator mode lean.
- **Contribution barrier for outside contributors** rises: the project
  now spans more concepts. README and onboarding need updates.

### Risks

- **Scope creep.** Once "app registry" is in scope, the temptation to
  also ship features like Cedar-queryable lineage, federation, or
  paid tiers in v1 increases. Mitigation: this ADR explicitly defers
  those to future work; the RFC lists them as "considerations" not
  v1 items.
- **VISION non-goals were trust signals to early readers.** Removing
  them is a narrative shift. Mitigated by an explicit "we changed
  our minds and here's why" note in VISION.

## Non-goals

This ADR does not decide:

- Where exactly Cedar enforcement lives for App/Lineage/Closure
  (separate ADR).
- The owner-name dispute-resolution policy (operational design doc).
- Storage limits and rate-limit values (operational design doc).

## Alternatives considered

### Option 1: Separate `temper-genesis` repo (rejected)

A new repo for the registry, depending on temper-git as an upstream
substrate.

**Rejected because:** fragments the substrate; the registry's storage
and identity are git-shaped and tightly coupled to temper-git;
maintaining two repos doubles release coordination for changes that
naturally touch both layers; the "two PRs total" working agreement
explicitly confined the work to temper-git + temper.

### Option 2: Separate Temper app, no new repo (rejected)

A new app spec living in temper-git's directory but kept logically
separate from the git-substrate code.

**Rejected because:** logical separation without physical separation
doesn't actually decouple anything. We'd end up coordinating releases
between two apps that share a repo, with all the costs of separation
and none of the benefits.

### Option 3: Absorb into temper-git (chosen)

temper-git's scope expands to include the registry. One repo, one
release cadence, one substrate.

**Pros:** unified substrate, simpler dependency story, fewer moving
parts.
**Cons:** VISION updates required; contribution barrier rises;
discipline needed to keep operator mode lean.

## References

- [RFC-0003](../rfc/0003-genesis-app-registry.md) — the proposal this
  ADR locks in.
- [ADR-0001](0001-temper-git-mission.md) — original mission (superseded
  in scope, not in spirit).
- [VISION.md](../../VISION.md) — updated in this PR.
