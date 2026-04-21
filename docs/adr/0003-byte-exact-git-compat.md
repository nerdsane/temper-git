# ADR-0003: Byte-exact git compatibility is a product guarantee, not a nice-to-have

## Status

Accepted — 2026-04-21. Establishes the testable compatibility contract for
temper-git's wire protocol and object serialization. Paired with
[ADR-0001](0001-temper-git-mission.md) (mission) and
[ADR-0002](0002-temper-native-scm.md) (Temper-native SCM).

## Context

ADR-0001 commits us to "full git client compatibility." ADR-0002 commits us
to "state in entities, protocol in WASM." This ADR pins down what
"compatibility" specifically means and how we enforce it.

There are three degrees of "compatibility" a git server can aim for:

1. **Semantic compatibility.** `git clone` works; `git push` works; the
   commit history arrives. But object hashes may differ, pack bytes may
   differ, wire-protocol formatting may differ. Third-party tools that
   *only* use high-level git commands work; tools that rely on exact
   hashes (mirror tools, hash-verified CI, signed commits) break.

2. **Structural compatibility.** Object hashes match. Commit SHAs round-trip.
   Mirrors produce identical repos. But pack bytes may be encoded
   differently (different delta choices, different object ordering);
   HTTP responses use different header shapes; HEAD pointer behavior
   may differ in edge cases. Most tools work; some esoteric ones
   don't.

3. **Byte-exact compatibility.** Every object hash matches. Every pack the
   server emits is bit-identical to what git-core would emit for the
   same repository state. HTTP smart-protocol responses are
   byte-for-byte identical. Every test-fixture github.com response is
   reproducible. Nothing distinguishable from an actual
   GitHub-Enterprise server except the hostname.

## Decision

**temper-git targets byte-exact compatibility (Degree 3) as a testable,
gating contract. CI blocks any change that breaks it.**

Specifically:

### Hash-byte-match (hard requirement)

For every Blob, Tree, Commit, and Tag object we emit, the canonical
serialization used to compute its SHA-1 hash MUST match byte-for-byte
what `git hash-object` produces for the same logical input. This is
non-negotiable because:

- Mirrors to github.com require identical hashes or `git push --mirror`
  creates "new" objects.
- Signed commits verify against the commit-object bytes; mismatched
  bytes = mismatched signature = rejected PR.
- Third-party hash-verified tools (e.g., Sigstore/Rekor attestations,
  `git rev-parse --verify`) silently break on hash divergence.

### Pack-byte-match (soft requirement, tested)

Pack files we emit for upload-pack MUST be parseable by `git-core` with
`git index-pack --stdin --fix-thin` (no errors, no warnings about
unknown features). They SHOULD be byte-identical to git-core's output
given the same working set. We test this via a `git diff-pack-streams`
harness helper.

The distinction (MUST parseable, SHOULD bit-identical) is because git-core
has freedom in delta choices: given the same inputs, two valid
git-packs can differ bytewise if they pick different delta bases. We
pick the same delta strategy as git-core (greedy window-based) so that
in *practice* our packs bit-match, but we don't assert exact bytes —
we assert semantic pack equivalence (same objects, same hashes, valid
pack-v2 with consistent header/trailer/CRCs).

### HTTP wire-response shape (hard requirement)

Every smart-HTTP response we emit must match git-core's advertised
shape:
- `Content-Type: application/x-git-upload-pack-advertisement` etc.
- pkt-line framing with correct 4-hex length prefix.
- Capability advertisement list matches git-core's conventions.
- Chunked transfer encoding on streaming responses.
- Side-band-64k channel multiplexing on multi-ack responses.

### GitHub REST response shape (hard requirement for shipped endpoints)

For every REST endpoint we ship, the response JSON body must have the
same field names, types, and required-field presence as github.com's
response for the same call. We test by:
- Recording a response from github.com (via `gh api` or `curl`) and
  storing it as a fixture.
- Calling temper-git with the same request.
- Asserting structural equality: every field in the github.com
  response is present in ours with matching type. Extra fields we
  emit are allowed (we can add our own metadata). Missing fields are
  a compat break.

Values are not required to match — our endpoint computes its own repo
state. Only the response *shape* matters for tooling compat.

## Enforcement

1. **Hash-byte-match harness tests.** For every object-serialization
   helper (`blob_to_bytes`, `tree_to_bytes`, `commit_to_bytes`,
   `tag_to_bytes`), a test with 10+ canonical fixtures:
   - Empty blob.
   - Blob with binary content.
   - Tree with mixed file/executable/symlink/subtree entries.
   - Tree with unicode filenames.
   - Commit with no parents (initial).
   - Commit with one parent (normal).
   - Commit with two parents (merge).
   - Commit with signed payload (GPG signature).
   - Annotated tag object.
   - Lightweight tag (ref only).
   Each asserts `our_hash(input) == git_hash_object(input)`.

2. **Round-trip protocol harness.** For every protocol handler, an
   integration test runs:
   ```
   temper-git-dev-server &
   git clone http://localhost:<port>/owner/repo.git clone1
   cd clone1 && echo "x" > f && git commit -am msg && git push
   cd .. && rm -rf clone1
   git clone http://localhost:<port>/owner/repo.git clone2
   diff -r clone1 clone2  # bit-identical working tree
   ```
   Plus `git fsck --full` on `clone2` must produce zero errors,
   zero warnings.

3. **Mirror-round-trip test.** A harness test:
   ```
   git clone --mirror https://github.com/temper-git-fixtures/small.git mirror-from-gh
   cd mirror-from-gh && git push --mirror temper-git://localhost/fixtures/small.git
   cd .. && git clone --mirror temper-git://localhost/fixtures/small.git mirror-back
   git -C mirror-back show-ref | sort > a.refs
   git -C mirror-from-gh show-ref | sort > b.refs
   diff a.refs b.refs  # zero differences
   ```
   Every ref hash in `b.refs` appears identically in `a.refs`.

4. **GitHub API shape tests.** For every REST endpoint listed in
   [RFC-0001 §"REST surface"](../rfc/0001-temper-git-v1-architecture.md),
   a fixture pair `(gh_fixture.json, temper_response.json)` with a
   `assert_compatible_shape()` helper that walks both and asserts
   every github-field appears in our response with the right type.

5. **CI gating.** All four harnesses run on every PR. Any break
   blocks merge. The harness output is archived (packfile diffs,
   response diffs) for debugging.

## Consequences

### Easier

- **Ecosystem trust.** Users can point any git tool at temper-git and
  expect it to work. "Works with GitHub Enterprise" is a checkbox
  many tools advertise; we match that standard.
- **Mirror-compatibility without code.** Standard mirror tools
  (`git-sync`, `gitsync`, `mirror`) work without any temper-git-specific
  configuration.
- **Signed-commit support is free.** Because we preserve exact
  commit-object bytes, existing GPG/SSH signatures on commits
  pushed to temper-git verify without re-signing.
- **Deterministic testing.** Byte-exact means reproducible; no flaky
  tests over "the pack ordering changed."

### Harder

- **Implementation discipline.** "Close enough" is never close enough.
  Every edge case — trailing newlines in commit messages, tree entry
  ordering, mode bit encoding (100644 vs 100664 handling), unicode
  filename normalization — must match git-core behavior.
- **Ongoing conformance.** When git-core ships a new feature (SHA-256
  transition, pack-v3, new reference types like `reftable`), we must
  either adopt or document the gap. We cannot sit still.
- **Test harness build surface.** The harness invokes `git` CLI, which
  means our CI images must include git. Small cost, documented.

### Risks

- **git-core behavior change.** If `git` releases a new version that
  changes an object serialization detail (has happened with caveats
  around Git 2.x transitions), our fixtures drift from reality. We
  mitigate by pinning the `git` version in the CI image and reviewing
  bumps.
- **Unicode normalization.** git-core uses NFC on macOS, raw bytes on
  Linux, and has its own `core.precomposeUnicode` flag. We follow
  git-core's behavior exactly; if an agent pushes filenames in NFD
  from a Linux context, we store them as NFD bytes — same as git-core.

## Options Considered

### Option 1: Semantic compat only (rejected)

High-level `git clone`/`git push` works, hashes may differ.
**Rejected because:** breaks mirrors, signed commits, and any tool that
relies on hash identity. That's most of the real ecosystem.

### Option 2: Structural compat (rejected)

Hashes match, pack bytes roughly match.
**Rejected because:** doesn't give us a crisp testable contract. "Roughly
matches" = flaky tests and eventual drift.

### Option 3: Byte-exact compat (chosen)

Hashes match exactly; pack streams pass `git fsck`; REST response
shapes match github.com structurally.
**Pros:** testable, predictable, ecosystem-wide compatibility.
**Cons:** implementation cost; ongoing conformance cost.

## References

- [ADR-0001](0001-temper-git-mission.md)
- [ADR-0002](0002-temper-native-scm.md)
- [ADR-0004](0004-per-repo-libsql-gcs.md) — per-repo libSQL storage; BLOB
  bytes are round-trip-tested for hash-byte-match against the same harness
  regardless of storage vendor.
- [RFC-0001](../rfc/0001-temper-git-v1-architecture.md)
- [CODING_GUIDELINES.md](../../CODING_GUIDELINES.md) — hash-integrity, canonical serialization rules
- Git object format: https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
- Git hash-object behavior: https://git-scm.com/docs/git-hash-object
- Git pack format: https://git-scm.com/docs/pack-format
- Git smart-HTTP protocol: https://git-scm.com/docs/http-protocol
- GitHub REST v3 reference: https://docs.github.com/en/rest
