# ADR-0004: Per-repo libSQL with GCS-backed WAL, Turso-Cloud-swappable

## Status

Accepted — 2026-04-21. Supersedes the "Postgres + blob store spill" storage
design sketched in the first draft of
[RFC-0001](../rfc/0001-temper-git-v1-architecture.md) (before it was locked).
Paired with [ADR-0002](0002-temper-native-scm.md) (SCM as IOA entities) and
[ADR-0003](0003-byte-exact-git-compat.md) (byte-exact compat contract).

## Context

ADR-0002 committed us to modeling every git object — Blob, Tree, Commit, Tag,
Ref, PullRequest, Review, GitToken, HttpEndpoint — as first-class Temper IOA
entities. We've now decided the physical storage shape.

The initial sketch used Postgres for entity state with a two-tier blob-spill
strategy: small blobs inline, large blobs in a separate content-addressed
store accessed via a `BlobStoreRef` pointer. That design has real problems:

- **Two storage tiers = two failure domains = two backup stories.** Every
  Blob write that spans tiers is a distributed transaction with a failure
  window (blob-store write succeeded, entity row insert failed → orphaned
  blob). Garbage collection is another system to run.
- **Postgres is not a blob store.** TOAST handles large `bytea` values but
  performance degrades across the whole table as blob size grows. VACUUM
  cost, WAL size, replication lag all suffer.
- **Tenant isolation is soft.** A shared Postgres DB means cross-repo
  isolation is a Cedar-query-filter discipline; a buggy WHERE clause can
  leak. We want structural isolation aligned with ADR-0005's team-isolation
  rule in the dark-helix factory.
- **Scale ceiling is low.** 100 GB total entity state before Postgres
  starts hurting on common hardware. For a self-service SCM, that's a
  year of runway, not indefinitely.

The air-gapped-deployment constraint (this must run inside our GCP sandbox
with no external network reach) rules out any managed-service-only answer.
But the swappability requirement means the abstraction itself should not
couple to any specific vendor — if we later want to move to Turso Cloud,
that's a deployment-config change, not a code change.

## Decision

**Storage substrate: per-repository libSQL database, WAL frames shipped to
GCS. Self-hosted libsql-server running in the `temper-git` namespace.
Turso Cloud is a first-class alternative vendor of the same wire protocol;
swap is an environment-variable change.**

Specifically:

### Storage model

1. **One libSQL database per Repository entity.** A `Repository` in
   temper-git gets a dedicated database named by its repo slug
   (`/dark-helix`, `/darkhelix-users`, etc.). All entities belonging to
   that repo — Blobs, Trees, Commits, TreeEntries, Tags, Refs,
   PullRequests, Reviews, ReviewComments — live in that repo's database.

2. **One control-plane libSQL database.** Non-per-repo state — `GitToken`
   rows, `HttpEndpoint` rows, cross-repo audit, the list of repos
   themselves — lives in a shared `temper-git-control-plane` database.

3. **Blob content is inline in the Blob row as a SQLite BLOB column.**
   No spill tier, no threshold decision, no `BlobStoreRef`. A 100-byte
   source file and a 500 MiB binary both live as BLOB data in the same
   table. SQLite handles multi-GB BLOBs natively without the TOAST
   performance cliff Postgres has.

4. **Durable backing is GCS.** libsql-server's "bottomless" replication
   mode ships WAL frames to a GCS bucket via S3-compatible interop (GCS
   supports the S3 XML API when configured for interoperability with
   HMAC keys). The local SQLite page files on the libsql-server pod are
   a hot cache; the object store is the source of truth. On pod loss,
   a fresh pod replays WAL frames from GCS and continues.

5. **One Kubernetes StatefulSet** running libsql-server (single replica
   for v1; HA is Phase 2). PVC for the hot cache. Workload Identity
   binds the pod's ServiceAccount to a GCS IAM role on the WAL bucket.

### Vendor abstraction

The Temper kernel already has `temper-store-turso` with
`TenantStoreRouter`, which speaks libSQL wire protocol to a remote
endpoint defined by `TURSO_PLATFORM_URL + TURSO_PLATFORM_AUTH_TOKEN`.

Self-hosted mode:
```
TURSO_PLATFORM_URL=http://libsql-server.temper-git.svc.cluster.local:8080
TURSO_PLATFORM_AUTH_TOKEN=<issued-by-libsql-server-admin-key>
```

Turso Cloud mode (identical code path, different endpoint):
```
TURSO_PLATFORM_URL=https://<org>.turso.io
TURSO_PLATFORM_AUTH_TOKEN=<turso-api-token>
```

We write ZERO vendor-specific code. Every `TursoEventStore` call in
Temper goes through the libSQL wire protocol; self-hosted and Turso
Cloud serve the same protocol. The swap is one Secret change.

### Repo provisioning

When a `Repository.Create` action fires:
1. Temper dispatches through its normal pipeline to write a Repository
   row in the control-plane DB.
2. A `repository_provision` WASM integration runs on that action's
   effect:
   - Calls the libSQL control-plane API: `POST /v1/databases` with
     the new repo slug as the database name.
   - Awaits "ready" state.
   - Updates the Repository entity with `LibsqlDbName` (typically
     equal to the slug).
3. From that moment, `TenantStoreRouter` routes that tenant's (= that
   repo's) OData operations to the new DB.

This is the same path Temper already uses for multi-tenant OS apps;
we just piggyback on `TenantStoreRouter` with "tenant" meaning "repo."

### Air-gap guarantees

- libsql-server runs in-cluster; no external pull after the image is
  mirrored to Artifact Registry.
- GCS traffic goes out via Private Google Access (configured on the
  subnet) — never over the public internet.
- Workload Identity authenticates the pod to GCS without credentials
  on disk.
- No Turso Cloud dependency in the air-gapped deployment path.

### Swap to Turso Cloud

Steps to move off self-hosted to Turso Cloud (future, if/when):
1. Create the equivalent DBs on Turso Cloud (one per repo).
2. For each repo, copy the libSQL database file from self-hosted to
   Turso Cloud (Turso supports dump/restore).
3. Update temper-git's `TURSO_PLATFORM_URL` and
   `TURSO_PLATFORM_AUTH_TOKEN` secrets.
4. Roll the Temper pod. Hot.
5. Decommission the self-hosted libsql-server StatefulSet.

No schema change, no entity migration, no Cedar policy change, no WASM
rebuild.

## Consequences

### Easier

- **Single storage tier.** Blob content and Blob metadata are the same
  row. One transaction, one IO path, one backup story, one code path.
- **Per-repo isolation is structural.** Different repos live in
  different SQLite files. A buggy query cannot reach another repo's
  data because the connection isn't open to that DB.
- **Backup is free.** Object-store versioning IS the backup. GCS retains
  prior object versions for configurable periods; point-in-time recovery
  = roll the WAL to a target timestamp.
- **Migration cost per repo is trivial.** Adding a repo = creating a new
  DB (milliseconds). Removing a repo = dropping a DB (milliseconds).
  Renaming a repo = renaming the DB name + updating one Repository row.
- **Scale is predictable.** Each DB up to ~1 TB comfortably. Individual
  repo size is the bound; total service size is effectively unbounded
  (GCS is).
- **Vendor swappability is free.** `TenantStoreRouter` is the
  abstraction; we use it today, and switching self-hosted ↔ managed is
  an env-var change.

### Harder

- **libsql-server as additional infrastructure.** We run one more pod,
  one more image, one more set of metrics and dashboards. Not hard; not
  nothing. Tempered by: it's a single Rust binary, no JVM, no
  operator; just a StatefulSet with a PVC.
- **Cross-repo queries are application-side fanouts.** "List all PRs I
  opened across all repos" iterates my repo list, queries each DB.
  Acceptable at our scale (dozens of repos); at GitHub scale would need
  a cross-repo index DB. Flag this when we cross ~1000 repos.
- **Per-DB connection pool cost.** libsql-server holds an open
  connection per client per DB. At hundreds of repos and dozens of
  concurrent clients, connection count grows. Tunable; not an issue at
  v1 scale.
- **GCS write latency on commit.** WAL frames ship on commit. Every git
  push incurs an object-store round trip at transaction commit time.
  Typical latency in-region: tens of ms. Multi-region: hundreds of ms.
  This is the same cost Postgres synchronous replication has; not
  new, just visible here.

### Risks

- **libsql-server maturity.** The upstream project (turso-io/libsql) is
  actively developed but less battle-tested than Postgres. We mitigate
  by: pinning to a specific libsql-server version; running CI
  round-trip tests against it; having the swap-to-Turso-Cloud escape
  hatch if we hit a bug we can't fix fast.
- **WAL-shipping tuning.** If WAL frames accumulate locally faster than
  they ship to GCS, the hot cache grows. Tunable via libsql's
  `LIBSQL_BOTTOMLESS_MAX_WAL_SIZE` and similar knobs. Monitor via
  PVC fill metric.
- **GCS S3-interop edge cases.** libsql's bottomless uses S3 API;
  GCS's interop layer is S3-compatible but not 100% identical. We
  test against GCS explicitly during Phase 1 and document any gaps.
- **Single replica for v1 = Single point of failure for the data
  plane.** A pod loss requires WAL replay before accepting writes,
  taking seconds to minutes depending on WAL depth. For a sandbox
  deployment this is acceptable; production HA would add a
  leader+follower libsql setup, scoped in Phase 2.

## Options Considered

### Option 1: Postgres + spill (rejected)

The original sketch. See Context for why: two tiers, two failure
domains, Postgres cliff at scale, soft isolation. Rejected once the
alternative was on the table.

### Option 2: Managed Turso Cloud directly (rejected for v1, retained as swap target)

Fastest to ship for an internet-connected environment. Rejected because:
- Air-gapped GCP sandbox requires on-cluster services.
- SaaS dependency means outages we don't control.
Retained as a Phase 3+ option: the abstraction is swappable, so we can
move to Turso Cloud without code changes when network posture permits.

### Option 3: Self-hosted Postgres with external blob store (GCS) (rejected)

The hybrid. Postgres for entities, GCS for large blobs, application-level
coordination. Rejected because: two failure domains persist; isolation is
still soft; we added GCS complexity without gaining the per-repo model.

### Option 4: Self-hosted libSQL + GCS (chosen)

**Pros:**
- One storage tier, native BLOB handling, per-repo isolation is structural.
- GCS is our existing object store; Workload Identity is already configured.
- Temper natively supports libSQL via `temper-store-turso`; zero
  new code required in the kernel.
- Swap path to Turso Cloud = environment variable change.
- Backup + PIT recovery via GCS versioning is free.

**Cons:**
- New infra component (libsql-server StatefulSet) to run.
- libsql-server is less operationally mature than Postgres.
- WAL shipping to GCS adds commit latency (tens of ms in-region).

## Deployment shape (v1)

```
Namespace: temper-git
  ├─ StatefulSet: libsql-server           (1 replica)
  │    ├─ Pod: libsql-server-0
  │    │    ├─ container: libsql-server:vX.Y.Z
  │    │    ├─ volumeMount: /var/lib/libsql (PVC)
  │    │    ├─ env: LIBSQL_BOTTOMLESS_BUCKET=gs://temper-git-wal
  │    │    ├─ env: LIBSQL_BOTTOMLESS_ENDPOINT=https://storage.googleapis.com
  │    │    ├─ env: AWS_ACCESS_KEY_ID (HMAC for GCS interop)
  │    │    ├─ env: AWS_SECRET_ACCESS_KEY
  │    │    └─ serviceAccountName: libsql-server-ksa
  │    └─ PVC: libsql-hot-cache (100 GiB initial; autoscales)
  ├─ Deployment: temper-git                (1 replica)
  │    ├─ container: temper-git:vX.Y.Z
  │    ├─ env: TURSO_PLATFORM_URL=http://libsql-server.temper-git.svc.cluster.local:8080
  │    ├─ env: TURSO_PLATFORM_AUTH_TOKEN (from Secret)
  │    └─ command: temper serve --storage turso
  ├─ Service: libsql-server     (ClusterIP :8080, internal)
  ├─ Service: temper-git        (ClusterIP :80, internal; ingress later)
  └─ ServiceAccount: libsql-server-ksa → GCP IAM role on gs://temper-git-wal

GCS bucket: gs://temper-git-wal
  └─ Object versioning: enabled, 30-day retention
  └─ Access: restricted to libsql-server-ksa via Workload Identity
  └─ Network: Private Google Access only (no public route)
```

Credentials for GCS interop:
- HMAC key issued via `gcloud storage hmac create --project=... --service-account=...`
- HMAC access/secret stored in `libsql-bottomless-creds` Secret
- libsql-server picks them up via `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` env
  (libsql speaks S3 API; "AWS_" prefix is historical)

## Verification to do during Phase 1

Before locking this ADR to "implemented," Phase 1 Week 1 must verify:

1. **libsql-server bottomless to GCS interop works.** Boot
   libsql-server, create a DB, write rows, kill the pod, boot a new
   pod, verify rows come back from GCS WAL replay. Bit-exact.

2. **Temper's `temper-store-turso::TursoEventStore` works against
   self-hosted libsql-server** (not just Turso Cloud). Validate via
   `temper` integration test against a libsql-server in-cluster.

3. **BLOB handling at size.** Insert a 500 MiB BLOB; read it back;
   verify SHA-256 of bytes-in equals SHA-256 of bytes-out. Verify
   memory footprint during the insert (confirms streaming, not load-
   all-in-RAM).

4. **GCS egress via Private Google Access only.** Confirm libsql-server
   pod has NO public internet route and WAL shipping still succeeds
   (via PGA path).

5. **Per-DB provisioning path.** Create a new database via libsql admin
   API, verify it shows up via `TenantStoreRouter`, write to it, read
   back.

If any of these fail, this ADR goes back to "Proposed" and we revisit.

## References

- [ADR-0001](0001-temper-git-mission.md) — product mission
- [ADR-0002](0002-temper-native-scm.md) — IOA-entities + WASM integrations
- [ADR-0003](0003-byte-exact-git-compat.md) — byte-exact compat contract
- [RFC-0001](../rfc/0001-temper-git-v1-architecture.md) — v1 architecture
  (amended alongside this ADR to reference per-repo libSQL)
- libSQL server: https://github.com/tursodatabase/libsql
- libSQL bottomless replication:
  https://github.com/tursodatabase/libsql/tree/main/bottomless
- GCS S3 interop:
  https://cloud.google.com/storage/docs/interoperability
- Temper's `temper-store-turso` crate (in the pinned `temper/` submodule)
