# ADR-0006: Forks are metadata-only; lineage lives in a `Lineage` sidecar

## Status

Accepted — 2026-05-18. Pairs with
[ADR-0004](0004-registry-scope-absorption.md) and
[RFC-0003](../rfc/0003-genesis-app-registry.md).

## Context

The registry needs to record fork relationships: this app derived
from that app, with these changes. Two questions to settle:

1. **Where does the fork relationship live?** In the `Repository`
   entity itself (as `parent_ref`), in a separate `Lineage` entity, or
   inferred from git's commit graph?
2. **What does forking actually do mechanically?**

### What git itself provides

git has no fork concept. A "fork" in git is just *another repo* that
happens to share commits. From git's perspective, two repos with shared
history are just two repos. GitHub's "fork" button creates a new repo
and records the parent in GitHub's *database* — that's metadata, not
git.

So any "native fork" in temper-git would mean *temper-git inventing a
concept git itself doesn't have*. Given that temper-git's mission
([ADR-0001](0001-temper-git-mission.md), [ADR-0003](0003-byte-exact-git-compat.md))
holds 1:1 git compatibility as load-bearing, inventing a fork primitive
that diverges from git is a direct cost.

### What git's commit graph doesn't capture

Git's commit-parent graph cannot answer:

- **"What app is this app a fork of?"** Git tracks commits, not apps.
- **"This commit grafted bytes from another app's lineage — which one?"**
  Git just sees the diff; the cross-app provenance is invisible.
- **"Was this fork a code edit vs. a dep bump vs. an imported module?"**
  Git just sees the diff; the structured semantics are invisible.

The registry needs all three. They have to come from somewhere outside
git's data model.

## Decision

**Forks are metadata-only. The relationship lives in a `Lineage`
entity that sits alongside `Repository` entities. temper-git's git
layer is unchanged.**

### Sub-decision 1: No fork primitive in the git layer

There is no `Repository.Fork(parent)` action that modifies temper-git's
git-substrate model. The `Repository` entity does NOT carry a
`parent_ref` field. The git side stays at its current shape; 1:1 git
compatibility (ADR-0003) is preserved exactly.

### Sub-decision 2: A `Lineage` entity captures the relationship

```
entity Lineage {
  Id              : String      # "ln-..."
  child_repo      : Reference   # → Repository
  parent_repo     : Reference   # → Repository (nullable for genesis apps)
  parent_commit   : String      # SHA-1 of the commit the fork pointed at
  type            : String      # "fork" (v1); future: "merge"
                                #   "fork" = single-parent derivative
                                #   "merge" reserved for a future first-class
                                #     action representing reticulate evolution
                                #     (two diverged lineages converging back
                                #     into one descendant). Distinct from
                                #     "imported" mutations, which represent
                                #     bytes grafted from elsewhere within a
                                #     single-parent fork.
  created_by      : String      # agent or human principal
  created_at      : Timestamp
  mutations       : JSON        # structured array; see below
}
```

`mutations` is an append-only array of structured change records:

```
{ kind: "edit",     target: "specs/heal_rule.ioa.toml" }
{ kind: "imported", source: "paw-deploy-tracker@9c12...",
                    module: "deploy-ingestor" }
{ kind: "dep_bump", target: "temper-git",
                    from: "@b21c", to: "@d4e8" }
```

These capture what git's commit graph cannot:

- **`edit`**: ordinary code change in this app.
- **`imported`**: bytes grafted from another lineage. The v1 stand-in
  for "Hybridize." Multi-parent semantics rendered by the phylogeny
  projection without needing a first-class Hybridize action.
- **`dep_bump`**: a dependency hash changed. Lineage divergence in
  deps, not code.

### Sub-decision 3: Mechanics of `Apps.Fork`

```
action Apps.Fork(parent_app, parent_version)
{
  let parent_hash = resolve(parent_app, parent_version);
  let parent_repo = parent_app.repo;
  let parent_commit = parent_repo.refs[default_branch];

  // 1. Create new repository
  let child_repo = Repository.Create(
    Id: synthesize_id(caller_owner, parent_app.name),
    OwnerAccountId: caller_owner,
    Name: parent_app.name,
    DefaultBranch: parent_repo.DefaultBranch,
    Visibility: parent_repo.Visibility
  );

  // 2. Point child's main ref at parent's commit
  //    No byte copy: content-addressed objects are shared
  Refs.Create(
    Id: "rf-" + child_repo.Id + "-" + parent_repo.DefaultBranch,
    Repository: child_repo,
    Name: "refs/heads/" + parent_repo.DefaultBranch,
    Sha: parent_commit
  );

  // 3. Create the App row (registry entry)
  let child_app = Apps.Create(
    Name: parent_app.name,
    Owner: caller_owner,
    Repository: child_repo,
    LatestVersionHash: parent_hash  // initially same as parent
  );

  // 4. Create the Lineage row
  Lineage.Create(
    child_repo: child_repo,
    parent_repo: parent_repo,
    parent_commit: parent_commit,
    type: "fork",
    created_by: caller_principal,
    mutations: []
  );

  return child_app;
}
```

This is a composite action (see temper ADR-0040): one Cedar gate, four
sub-writes, one transaction, one event.

**No byte copy occurs.** Content-addressed objects (blobs, trees,
commits) are shared across repos via temper-git's existing entity
model. The fork diverges in storage only when the forking agent makes
its first new commit.

### Sub-decision 4: Hybridize is not first-class in v1

Multi-parent grafts (the HGT-analog from the design conversation) are
captured as `imported` mutations on single-parent forks. The DAG
phylogeny projection renders multi-parent edges by walking the
`imported` mutations across lineages.

If, in v2, the projection demands a distinct semantic (e.g., for Cedar
policies that gate "officially-blessed merges across lineages"),
Hybridize can be promoted to a first-class action. The data is already
captured.

### Sub-decision 5: Phylogeny projection over Lineage

A read endpoint exposes the DAG:

```
GET /api/lineage/{owner}/{app}
→ {
    node: { repo, hash, owner, name, version_label },
    parents: [Lineage{...}, ...],     // direct parent forks
    children: [Lineage{...}, ...],    // direct child forks
    imports: [{source, module}, ...], // multi-parent mutation edges
  }
```

OData query is the v1 baseline (`Lineages?$filter=parent_repo eq ...`);
the dedicated endpoint is server-side projection convenience.

## Consequences

### Positive

- **temper-git's git layer is unchanged.** ADR-0001/0003 mission and
  byte-exact compatibility hold exactly. No fork primitive divergence
  from git.
- **No byte copy on fork.** Content addressing makes forks ~3 row
  writes regardless of repo size.
- **Structured semantics for mutations.** "What kind of change was
  this fork?" is answerable from `mutations`, not inferred from diff.
- **Hybridize-data captured without Hybridize-action.** `imported`
  mutations cover the use case; we can promote to first-class later if
  needed.

### Negative

- **Phylogeny queries join through Lineage.** Walking the DAG requires
  joining `Lineage` rows; it's not a single-table read. Mitigated by
  the dedicated read endpoint and OData indexing.
- **Two places to look for fork relationships.** The git commit graph
  has parent commits; the Lineage table has parent repos. They overlap
  on "parent commit SHA" but diverge on everything else. Documentation
  must make this clear.

### Risks

- **Forks can be created without Lineage rows** if someone goes around
  the `Apps.Fork` composite action and creates a Repository directly.
  Mitigation: in commons mode, Cedar enforces invariants on Repository
  lifecycle. Concretely:

  - `Repository.Create` in commons mode is permitted **only** when
    called from inside one of two composite actions:
    `Apps.RegisterNewApp` (a genesis app — produces an `App` row with
    `parent = null` and no Lineage row), or `Apps.Fork` (produces an
    `App` row and a `Lineage` row in the same atomic transaction).
  - Direct `Repository.Create` from outside those composites is
    denied by Cedar policy in commons mode. Operator mode keeps the
    permissive default for single-tenant convenience.
  - This is enforced via Cedar's
    `principal.action_context == "composite:apps_fork"
      || principal.action_context == "composite:apps_register"`
    pattern, which uses the composite-action primitive's context
    propagation (see `nerdsane/temper` ADR-0040).

  The Cedar policy file `policies/repository.cedar` in commons mode
  carries this rule explicitly. Operator-mode policies don't include
  it; the operator can `Repository.Create` freely on their own kernel.

## Non-goals

- Promoting Hybridize to first-class (future work; see RFC-0003 §15).
- Designing the Genesis UI's lineage visualization (separate work
  in the implementation plan).
- Cedar-queryable lineage from inside policy evaluation (future).

## Alternatives considered

### Native fork primitive in `Repository` (rejected)

Add `parent_ref` to `Repository`; introduce a `Repository.Fork`
action in temper-git's git-substrate spec.

**Rejected because:** invents a concept git itself doesn't have;
breaks 1:1 git compatibility framing; couples lineage semantics into
the git layer where it doesn't belong.

### Lineage derived from git's commit graph (rejected)

Don't add a Lineage entity; compute fork relationships from shared
commits across repos.

**Rejected because:** can't capture cross-app provenance (`imported`),
can't capture structured mutation kinds, can't capture forks-of-forks
without walking O(N) git histories per query.

### `Lineage` rows live inside `Repository` (rejected variant)

Put the lineage fields as columns on `Repository`.

**Rejected because:** mutations are an array, forks can have multiple
imports, future merge semantics need multi-parent — embedding in
Repository forces a one-row-per-parent denormalization that's awkward
to query.

## References

- [RFC-0003](../rfc/0003-genesis-app-registry.md) §7
- [ADR-0001](0001-temper-git-mission.md)
- [ADR-0003](0003-byte-exact-git-compat.md)
- temper ADR-0040 (composite-action kernel primitive)
