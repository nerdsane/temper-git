# ADR-0007: Content-addressed dependency closures (Nix-flake-style)

## Status

Accepted — 2026-05-18. Pairs with
[ADR-0005](0005-content-addressed-identity-and-owner-scoped-names.md)
and [RFC-0003](../rfc/0003-genesis-app-registry.md).

## Context

Apps depend on other apps. `paw-heal` uses `temper-git`. `paw-harness`
uses `paw-heal`. User apps use various OS apps. Specs reference other
specs.

The dependency model has to answer:

1. **What's the unit of dependency?** Whole apps, individual specs,
   individual modules?
2. **How are dependencies declared?** By name + version range, or by
   hash?
3. **How is a complete install reproducible across operators?**
4. **What happens on diamond dependencies?**

### Why traditional semver-with-resolution doesn't fit

Cargo, npm, and Go modules use semver with version ranges, then resolve
to a lockfile. This is necessary because their source materials
(crates, packages) are not content-addressed at the substrate level —
the registry has to pick "the right" version from a range, with
SAT-solver-style negotiation for diamonds.

temper-git is content-addressed at the substrate (every blob, tree,
commit is its SHA-1). The "right" version question disappears: a hash
either resolves or it doesn't. No SAT solver needed.

The natural fit is **Nix-flake-style content-addressed DAG with
closure hashing**: every app's hash includes its dep hashes, so the
hash recursively pins the entire transitive graph.

## Decision

**Apps are the dependency unit. Dependencies are pinned by content
hash. A `Closure` entity captures the resolved transitive graph and
becomes the bootstrap-pinning unit.**

### Sub-decision 1: Apps are the dep unit (Option A granularity)

Each app is a coherent bundle: `app.toml` + `specs/` + `wasm/` +
`policies/`. The app's hash is computed deterministically over the
entire bundle plus the resolved dep hashes:

```
app_hash = SHA-256(
  canonical(app.toml) ‖
  canonical(specs/) ‖
  canonical(wasm/) ‖
  canonical(policies/) ‖
  sorted(resolved_dep_hashes)
)
```

If any byte inside paw-heal changes, paw-heal's hash changes, and
everything that depends on paw-heal sees a new hash. Docker-style
cache invalidation, but at the app boundary.

**Why apps and not finer:** simple, matches how Temper apps are already
laid out, matches "an app is what an agent installs." Finer
granularity (Option B: per-spec, per-module) gives more reuse but
explodes the dep graph with no obvious v1 benefit.

**Upgrade path:** content-addressed file dedup (Option C) underneath
the app-level dep model, transparently. If storage ever bites
(many apps carrying byte-identical helper modules), the kernel can
dedup at the file level without any app-author API change. This is
future work, not v1.

### Sub-decision 2: `app.toml` declares deps by hash

```toml
name = "paw-heal"
version = "1.4.0"          # human-readable label; identity is the hash

[deps]                     # pinned by hash — this is the lock
temper-git = "@b21c4f8a..."
user-app   = "@e44a7c2b..."

[deps.hints]               # optional, used by the resolver pre-lock
temper-git = "^1.4"        # resolved to a hash at lock time

[exports]                  # what this app provides for others to depend on
entities = ["HealRule", "HealAttempt", "HealOutcome"]
actions  = ["TriggerHeal", "RecordOutcome"]

[lineage]                  # populated by Fork action; not hand-written
parent = "paw-heal@7a3f..."
mutations = [
  { kind: "edit",     target: "specs/heal_rule.ioa.toml" },
  { kind: "imported", source: "paw-deploy-tracker@9c12...",
                      module: "deploy-ingestor" },
]
```

The `[deps]` table is the *locked* form. Hashes only. This is what
the kernel actually reads at install/run time.

The `[deps.hints]` table is the *unlocked* form, used at development
time by a resolver to pick fresh hashes. Resolution is a pre-publish
step, not a runtime concern.

### Sub-decision 3: `Closure` entity pins the whole graph

A `Closure` is the lockfile expressed as a Temper entity:

```
entity Closure {
  Id                : String   # itself a hash; see below
  root              : String   # "paw-heal@7a3f..."
  resolved          : JSON     # map: app_name → hash
  resolver_version  : String   # which resolver produced this
  resolved_at       : Timestamp
  resolved_by       : String   # agent or human principal
}
```

The `Id` is computed as:

```
closure_id = "cl-" + SHA-256(
  canonical(root) ‖
  canonical(sorted(resolved)) ‖
  canonical(resolver_version)
)
```

Properties:

- **Immutable.** Same root + same resolved set + same resolver version
  → same Closure ID. Two operators resolving the same root with the
  same resolver get the same Closure ID.
- **Bootstrap = one Closure ID.** `temperpaw/bootstrap.toml` is a
  single line: `closure = "cl-..."`. That ID pins the whole transitive
  graph.
- **Reproducible.** Two TemperPaw deployments with the same Closure ID
  run bit-identical app stacks, including the apps that run the
  registry features themselves.

### Sub-decision 4: Diamond dependencies share by hash

If app A and app B both depend on `temper-git@b21c...`, they share
one install (same hash = same specimen). If A wants `@b21c` and B
wants `@d4e8`, both install — they're different specimens, identified
by different hashes. No SAT-solver negotiation, no "which version
wins" question.

This is the cleanest property that falls out of content addressing:
the question of "which version is the right one" doesn't apply,
because hashes are exact.

### Sub-decision 5: Fork inherits the closure

When `Apps.Fork(parent_app)` runs (see
[ADR-0006](0006-metadata-only-fork-via-lineage.md)), the fork's
`[deps]` table is copied verbatim from the parent. The fork is, by
default, a *code* evolution, not a *dependency* evolution.

A fork can later bump a dep by editing its own `app.toml`. The bump
appears as a `dep_bump` mutation in the Lineage row:

```
{ kind: "dep_bump", target: "temper-git",
                    from: "@b21c", to: "@d4e8" }
```

This keeps the lineage signal clean: looking at `mutations` tells you
immediately whether divergence is code, deps, or both.

## Consequences

### Positive

- **Reproducibility falls out for free.** Same Closure ID → same
  system everywhere.
- **No SAT solver in the agent path.** Resolution is a pre-publish
  step; runtime is hash lookups.
- **Diamonds are real and tractable.** Same hash = shared install;
  different hash = different installs.
- **Cache invalidation is automatic.** Change any byte → hash changes
  → downstream closures invalidate. Docker-layer semantics.

### Negative

- **Resolvers are needed for `[deps.hints]` → `[deps]`.** At least
  one resolver implementation in v1. Picks the highest matching hash
  for a semver hint. Simple but real work.
- **Closure rows grow over time.** Every install computes a Closure;
  most are short-lived. Garbage collection is a future operational
  concern.

### Risks

- **Resolver bugs cascade.** A bad resolver picks wrong hashes; every
  closure produced is wrong. Mitigation: pin `resolver_version` in
  Closure rows so we can identify affected installs.
- **Hash drift if anything in canonicalization changes.** If the
  app-hash computation rules ever change between resolver versions,
  identical bundles get different hashes. Mitigation: treat
  canonicalization rules as part of `resolver_version`; document
  changes loudly.

### DST compliance

- All hashing uses deterministic canonicalization (sorted keys, fixed
  encoding, no embedded clock/random).
- `resolved_at` uses `sim_now()` in simulation-visible code paths.
- `Closure.Id` derivation is pure-functional given inputs.

## Non-goals

- Designing the resolver algorithm (separate work; v1 uses simple
  "highest matching hash" semver resolution).
- Closure garbage collection (operational; later).
- Cross-commons closure resolution in federated mode (future).

## Alternatives considered

### Pin by semver range only, no hashes in `[deps]` (rejected)

Cargo/npm-style: `[deps] temper-git = "^1.4"`.

**Rejected because:** breaks reproducibility unless paired with a
lockfile; lockfile is then in a separate format from the spec; doubles
the data model.

### Lockfile separate from `app.toml` (considered, rejected)

Two files: `app.toml` for hints; `Closure` is a separate file
committed alongside.

**Rejected because:** `Closure` as a Temper entity (this proposal)
gives queryability, immutability, and Lineage integration for free.
A flat file gives none of these.

### Finer granularity: specs/modules as the dep unit (rejected v1)

Track dependencies at the spec or WASM-module level, not the app level.

**Rejected v1 because:** explodes the dep graph; no obvious benefit
until storage bites; the upgrade path (content-addressed file dedup
under the app-level model) is non-breaking.

## References

- [RFC-0003](../rfc/0003-genesis-app-registry.md) §8
- [ADR-0005](0005-content-addressed-identity-and-owner-scoped-names.md)
- [ADR-0006](0006-metadata-only-fork-via-lineage.md)
- Nix flakes design: https://nixos.wiki/wiki/Flakes
