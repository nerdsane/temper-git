# ADR-0008: Commons mode is a public service; v1 ships cost-bounding guardrails

## Status

Accepted — 2026-05-18. Pairs with
[ADR-0004](0004-registry-scope-absorption.md) and
[RFC-0003](../rfc/0003-genesis-app-registry.md).

## Context

[ADR-0004](0004-registry-scope-absorption.md) decides that temper-git
absorbs an app registry. In commons mode, this means a public service:
anyone using Temper can publish, fork, and install apps via the
default commons kernel. The kernel is operated by one human (initially
Sesh), but the *load* comes from the whole ecosystem — like GitHub
Inc. operates github.com while serving the entire world's code.

This is operationally meaningful in ways that operator mode is not:

- **Noisy neighbors are real.** One bad actor or runaway agent can
  generate disproportionate load.
- **Storage scales with adoption.** Every published app's bytes are
  hosted by the commons operator. At public-registry scale, this
  becomes a real cost line.
- **Bandwidth (egress) scales faster than storage.** `git clone`
  pulls are the dominant ongoing cost, as Docker Hub's 2020 pull-rate
  limits demonstrated.
- **Abuse exists.** Squatters, malicious uploads, content takedown,
  copyright/license claims. All are operational realities.

If v1 ships without guardrails, the public commons is one bad agent
away from a $5k Railway bill or a sustained denial-of-service.

## Decision

**v1 ships four cost-bounding guardrails in commons mode. The
architecture supports them; the operational design doc specifies the
values.**

### Sub-decision 1: Per-owner storage caps

Each registered owner has a maximum aggregate storage allotment across
all their apps (e.g., 10 GB free; opt-in higher tiers).

Implementation:

- `Owner` entity carries a `storage_cap_bytes` field.
- Cedar policy on `Repository.Write` and `Blobs.IngestRaw` checks
  current usage against cap. Reject if exceeded.
- Usage tracked via a projection over `Blobs` entities scoped to
  repos owned by each owner.

**Storage attribution rule** (resolves the interaction with
[ADR-0006](0006-metadata-only-fork-via-lineage.md) on forks):

- A blob is attributed to the owner of the `Repository` that
  performed the *first* `Blobs.IngestRaw` introducing those bytes
  into the kernel. Content-addressed dedup is real (the kernel
  stores one copy), but quota accounting attributes to the
  introducing owner.
- When a fork inherits a parent's bytes by ref-pointing at the
  parent's commit (no new blob writes), **no quota is charged to
  the fork's owner**. The fork pays storage only for the bytes it
  introduces via its own subsequent pushes.
- This means a popular app's original publisher absorbs the cost
  of the bytes they shipped, regardless of how many fork
  references exist. The publisher accepted that cost when
  publishing; forks accepting the reference don't pay.
- A fork that diverges (its own commits introduce new blobs) is
  attributed those new blobs against its own owner's quota,
  exactly as if the fork were a fresh independent app.

This rule means the storage cap acts on "bytes you introduced,"
not on "bytes you reference." It avoids the perverse incentive
where popular apps' publishers would subsidize all forks; it also
avoids double-counting shared content.

### Sub-decision 2: Write rate limits (publishes/hour/owner)

A token bucket per owner limits write operations (publishes, forks,
metadata updates). Prevents one owner's runaway script from
overwhelming the kernel.

Implementation:

- Token bucket state lives in a `RateLimit` entity, keyed by
  `(owner_id, action_class)`.
- Middleware on the OData action dispatcher checks the bucket before
  Cedar evaluation. Returns 429 if exhausted.
- v1 buckets are simple per-owner; per-IP secondary buckets defer to
  v2.

### Sub-decision 3: Pull rate limits (per-IP, per-account)

Same token-bucket pattern, applied to read operations (`git clone`,
OData queries, install fetches). Prevents pull-based DoS.

Implementation:

- Per-IP bucket for unauthenticated requests.
- Per-account bucket for authenticated requests (higher limit).
- CDN in front of read endpoints (future work) absorbs cache hits;
  bucket only counts cache misses.

### Sub-decision 4: Content-addressed dedup (already structural)

Same bytes = one copy across all forks, owners, and repos.

No new work — this is how temper-git's entity model already operates.
Specifically called out because it's the single biggest free cost
reduction the architecture provides.

### Sub-decision 5: Specific numbers live in the operational doc, not this ADR

What this ADR locks: the four guardrails exist in v1.

What this ADR explicitly defers to the operational design doc:

- Specific storage cap per tier (10 GB? 100 GB? Tier names?)
- Specific rate-limit values (publishes per hour per owner?)
- Owner-name dispute / takedown policy
- Account verification flow choice (OAuth provider, email-magic-link)
- Abuse-reporting endpoint shape
- Verified-publisher badge eligibility

This separation is intentional: architecture is forward-stable;
operational policy will change as adoption grows. The architecture
admits arbitrary policy through Cedar; the policy is dialed
operationally.

## Consequences

### Positive

- **Worst-case cost is bounded.** A bad actor cannot generate
  unbounded Railway charges; they hit a cap or rate limit.
- **Architecture is forward-compatible with paid tiers.** If a paid
  tier ever happens, it's a different `storage_cap_bytes` for paying
  owners; no architectural change.
- **The cost-bounding design is the same as the abuse-bounding
  design.** Storage caps protect against accidental and malicious
  load equally.

### Negative

- **Real users may hit limits.** A legitimate app with a large
  binary may need a higher cap. Operational mechanism: contact the
  commons operator; cap adjusted per `Owner`.
- **Rate-limit middleware adds latency to every write.** Mitigated by
  in-process token-bucket state (no external store round-trip).

### Risks

- **Bucket exhaustion under legitimate load.** If publish rate-limits
  are set too tight, a productive operator hits 429s. Mitigated by
  observability (track rejection rate, adjust values per the
  operational doc's monitoring section) and by starting limits
  generously.
- **Owner-quota gaming.** A bad actor registers 100 owners, gets
  100× the free tier. Mitigated by account-verification flow
  requiring something with weak abuse-resistance (email + phone, or
  OAuth) — operational, not architectural.

### Forward-compatibility note

All of the future-work items below build on (not replace) the v1
guardrails:

- **Federation.** Each commons sets its own limits independently.
- **Cold/hot storage tiering.** Reduces storage cost behind the cap.
- **CDN for `git clone`.** Reduces bandwidth cost; per-IP bucket
  only counts cache misses.
- **Paid tier for private apps.** Different `storage_cap_bytes` for
  paying owners.
- **Verified-publisher badges.** Higher rate limits for verified
  accounts.

## Non-goals

- Specifying particular numbers for caps/limits (operational).
- Implementing federation or tiered storage (future).
- Designing the takedown UI flow (operational).
- Choosing the OAuth provider (operational).

## Alternatives considered

### No guardrails in v1 (rejected)

Ship the commons with no limits; rely on goodwill.

**Rejected because:** one runaway agent or one bad actor breaks the
service for everyone, and the operator absorbs the cost.

### Paid tier from day one (rejected v1)

Free tier + paid tier with billing infrastructure in v1.

**Rejected v1 because:** billing/legal/support complexity for unclear
benefit at v1 adoption levels. Defer until usage forces the question.
Architecture is forward-compatible.

### Cedar policies enforce limits, no separate `RateLimit` entity (rejected)

Use Cedar attributes alone for rate limiting.

**Rejected because:** Cedar evaluates per-action; token-bucket state
needs to persist across actions. A dedicated `RateLimit` entity gives
queryable state that Cedar can read but doesn't have to maintain.

## References

- [RFC-0003](../rfc/0003-genesis-app-registry.md) §12
- [ADR-0004](0004-registry-scope-absorption.md)
- [ADR-0005](0005-content-addressed-identity-and-owner-scoped-names.md)
- Operational design doc (separate; sees task #7 in project memory)
- Docker Hub pull-rate-limit history (cautionary): https://www.docker.com/blog/scaling-docker-to-serve-millions/
