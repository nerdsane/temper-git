# ADR-0005: Content-addressed identity with owner-scoped names

## Status

Accepted — 2026-05-18. Pairs with
[ADR-0004](0004-registry-scope-absorption.md) and
[RFC-0003](../rfc/0003-genesis-app-registry.md).

## Context

The registry needs a way to refer to apps and versions. The model has
to satisfy four constraints:

1. **Identity is forgery-resistant.** Two agents claiming to have the
   same app version must produce the same identifier if and only if
   they have byte-identical bundles.
2. **Cross-operator references work.** An operator in one TemperPaw
   box must be able to refer to an app published by another operator
   in another box, unambiguously.
3. **Human-readable.** Agents and humans both read app references in
   commit messages, manifests, error logs. Pure-hash references are
   forgery-resistant but unreadable.
4. **Squatter-resistant.** The first person to register a popular name
   shouldn't be able to hold it hostage from the legitimate owner.

Three identity-model families to choose from:

1. **GitHub/npm-style `<owner>/<name>`.** Owners register, names are
   unique within owner, versions are semver tags. Forks are new
   identities; parent recorded as metadata.
2. **Pure content addressing.** Every version is its hash:
   `paw-heal@7a3f...`. Names are mutable pointers in a DNS-like layer.
   Two byte-identical apps from different authors share an identifier.
3. **DID/DNS-style** (`paw-heal.nerdsane.dev`). Ownership through
   domain control.

## Decision

**Use a hybrid: content hashes are truth; `<owner>/<name>@<version>`
is ergonomic sugar that resolves to hashes at lock time.**

### Sub-decision 1: Hashes are the substrate identity

Every app version is identified by its content hash, computed
deterministically over:

```
SHA-256(canonical(app.toml) ‖ canonical(specs/) ‖
        canonical(wasm/) ‖ canonical(policies/) ‖
        sorted(resolved_dep_hashes))
```

Two byte-identical bundles produced by different agents have the same
hash → are the same specimen. Constraint 1 (forgery-resistance) is
satisfied trivially.

The hash space is global; no central allocator is required for
content addressing. Constraint 2 (cross-operator references) is
satisfied: a hash refers to one specific bundle of bytes regardless of
which kernel originally produced it.

### Sub-decision 2: Names are mutable pointers, owner-scoped

A name is a tuple `(owner, app_name, version_label)` that resolves to
a hash. Examples:

- `nerdsane/paw-heal@1.4` → `paw-heal@7a3f8e2c1d4b...`
- `evolution-7/paw-heal@experiment-2` → `paw-heal@d5e9...`
- `nerdsane/paw-heal@latest` → whatever the latest published hash is

In commons mode:

- `owner` is bound to an authenticated identity (account on the
  commons). Only `nerdsane` (authenticated) can write under `nerdsane/...`.
- `app_name` is unique within an owner. `nerdsane/paw-heal` and
  `evolution-7/paw-heal` are distinct identifiers, not "the same app
  forked."
- `version_label` is a human-readable label (semver, "latest", "stable",
  arbitrary tag) that maps to a hash at lock time.

In operator mode (single owner), the `owner` segment defaults to a
local namespace; multi-tenant enforcement is off.

### Sub-decision 3: Fork creates a new identity, parent recorded in metadata

When `evolution-7` forks `nerdsane/paw-heal`, the result is
`evolution-7/paw-heal` — a new, independent identity in the registry.
The fork relationship is captured in a `Lineage` row (see
[ADR-0006](0006-metadata-only-fork-via-lineage.md)).

Constraint 4 (squatter resistance) is partially satisfied: a squatter
who claims a popular name can be reasoned about by checking lineage —
the legitimate continuation has the canonical lineage trace. Full
squatter-protection (dispute resolution, takedown, verified
publishers) is operational policy and lives in the operational design
doc.

### Sub-decision 4: Account verification at the commons gate

In commons mode, the commons kernel runs an account-verification flow
before allowing a writer to claim an `owner` segment. v1 uses
authenticated identity (specifically, an OAuth or email-magic-link
flow — the exact choice is operational). Once verified, the owner
controls all writes under their segment.

Signing of individual publishes is deferred (see
[RFC-0003](../rfc/0003-genesis-app-registry.md) §3.2). v1 trusts the
API auth gate to enforce ownership; v2 introduces signature
verification on publishes for higher trust ceilings.

## Consequences

### Positive

- **Forgery-resistant by construction.** Content hashes can't be
  faked; you either have the bytes or you don't.
- **Diamond dependencies fall out naturally.** If app A and app B
  both depend on `temper-git@b21c...`, they share storage. If they
  depend on different hashes, both are installed — they're different
  specimens. No SAT-solver required.
- **Human-readable layer is decoupled from substrate.** Names can be
  redesigned without breaking hash identity.

### Negative

- **Two layers to reason about.** "What hash does this name resolve
  to" is a real question and changes over time as labels move.
  Lockfiles (the `Closure` entity, [ADR-0007](0007-content-addressed-dependency-closure.md))
  freeze name resolution per install.
- **Bootstrapping account verification is operational work.** The
  commons needs an OAuth integration or similar before public launch.

### Risks

- **Trademark / squatter disputes.** Without verified publishers in
  v1, a bad actor could register `nerdsane/popular-app` before the
  real `nerdsane`. Mitigations: takedown policy in the operational
  doc; v2 adds verified-publisher badges.

## Non-goals

- Choosing the OAuth provider (operational).
- Choosing storage caps or rate-limit values (operational, see
  [ADR-0008](0008-commons-as-public-service-and-v1-cost-guardrails.md)).
- Designing the federation cross-commons reference syntax (future
  work; see RFC-0003 §15).

## Alternatives considered

### Pure content addressing (rejected)

Hashes only, no name layer.

**Rejected because:** humans and agents both need readable references
in commit messages, error logs, manifests. Names emerge naturally if
not designed; better to design them.

### DID/DNS-style names (rejected)

`paw-heal.nerdsane.dev` — ownership through domain control.

**Rejected because:** excludes anyone without a domain (most agents,
most casual contributors). Higher floor for participation than the
v1 ecosystem can absorb.

### First-come-first-served name allocator (rejected)

Whoever registers `paw-heal` first owns it, no owner-scoping.

**Rejected because:** invites squatters and races; no path to claim
back a name from a bad actor.

## References

- [RFC-0003](../rfc/0003-genesis-app-registry.md) §6
- [ADR-0004](0004-registry-scope-absorption.md)
- [ADR-0006](0006-metadata-only-fork-via-lineage.md)
- [ADR-0007](0007-content-addressed-dependency-closure.md)
