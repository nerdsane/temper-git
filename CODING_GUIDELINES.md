# temper-git — Coding Guidelines

Inherit Temper's TigerStyle verbatim. These are deltas / additions specific
to temper-git's domain (git wire protocol, binary data, pack format, hash
integrity).

## Inherited from Temper (read [temper/CODING_GUIDELINES.md](temper/CODING_GUIDELINES.md) for full rules)

- 70-line function cap, 500-line file cap.
- Average 2 assertions per function.
- Explicit limits on every loop / queue / buffer.
- No `unwrap()` in non-test code. No `unsafe`. Only `?`-propagation for errors.
- No comments that restate what code does; only `why` comments.
- Files over 500 lines must be split into directory modules.
- All pub items must have doc comments.
- Edition 2024, rust-version 1.92+.
- `gen` is a reserved keyword; never use as variable name.
- `BTreeMap`/`BTreeSet`, not `HashMap`/`HashSet`, in simulation-visible
  crates.
- No `std::thread::spawn`, no `rayon`, no multi-threaded `tokio::spawn` in
  simulation-visible crates.
- No `chrono::Utc::now()`, no `std::thread::sleep()` in simulation-visible
  crates — use `sim_now()` / `sim_uuid()`.

## Additions for temper-git

### Byte-exact git compatibility

1. **Canonical object serialization is authoritative.** A Blob/Tree/Commit/Tag
   entity is serialized to the wire exactly as `git cat-file --batch`
   emits. If your emitter differs by one byte from git-core's output, you
   have a bug. Always.

2. **SHA-1 hash paths are reference-implementation-matched.** When computing
   a blob sha1, you MUST:
   - Emit the header `blob <content_len>\0`
   - Follow with raw content bytes
   - Hash the concatenation with SHA-1
   - The resulting hex must equal `git hash-object -w <content>` to the
     byte.

   Verify against `git hash-object` in a harness test for every object
   type you emit.

3. **Pack format: always v2.** Smart-HTTP requires pack-v2. Pack-v1 is
   deprecated; do not emit. Parsing: accept pack-v2. Pack-v3 is not
   standardized — do not emit until git-core ships it.

4. **Delta compression: OFS_DELTA preferred over REF_DELTA.** Non-thin packs
   only initially (no external base references). When we ship thin packs
   (Phase 2), document the transition behind a feature flag.

5. **zlib is authoritative.** Use `flate2::read::ZlibDecoder` / 
   `flate2::write::ZlibEncoder` with default-compression level 6, matching
   git-core. Do not invent your own deflation.

### Streaming everywhere

1. **Pack upload/download is streaming.** A clone of a non-trivial
   repo may be hundreds of MB. WASM must not buffer a full pack in
   memory. Consume input with chunked reads; emit output with chunked
   writes.

2. **HTTP responses for protocol handlers set `Transfer-Encoding: chunked`**
   on responses with unknown content-length. Smart-HTTP expects this.

3. **Object graph walks are bounded.** A malicious or buggy client could
   ask for a pack containing the entire history of a 10-year repo. Put a
   hard cap on per-request object count (default: 1M objects) and emit a
   graceful error, not OOM.

### Hash integrity

1. **Every entity read verifies its own hash.** When a `Commit` row is
   loaded from Temper OData, the in-WASM deserializer recomputes the
   commit's SHA-1 from its canonical serialization and asserts it matches
   the entity's `Id`. If a row is corrupted, we refuse to serve it.

2. **Pack parsing verifies every object.** An incoming pack contains
   objects with advertised hashes; we recompute every hash as we parse
   and reject the whole push if any hash mismatches.

3. **Ref updates are atomic.** A `Ref.Update` action that advances main
   from commit A to commit B must atomically also write the new `Commit`
   row if A is not an ancestor of B in the existing DAG. Half-applied
   state is not acceptable.

### Protocol safety

1. **Fail closed on unknown capabilities.** If a `git clone` request
   includes `multi_ack_detailed side-band-64k thin-pack ofs-delta
   no-done` capabilities but we only support a subset, negotiate down.
   Do not silently drop unknown capabilities without logging.

2. **Every user-controlled header and query param is validated.** Paths
   must be relative, must not contain `..`, must not exceed 256 bytes.
   Ref names must match `refs/heads/.+` or `refs/tags/.+`, no
   newlines, no spaces.

3. **Auth is bearer-only.** Basic auth is accepted over HTTPS only and
   must map to a `GitToken` entity. No fallback to anonymous for mutation
   actions. Reads may be anonymous if Cedar permits.

### Testing requirements

1. **Round-trip test for every protocol change.** For every PR that
   touches upload-pack, receive-pack, pack parsing, pack emission, or
   smart-HTTP routing, the harness runs:
   ```
   $ git clone https://<token>@localhost:<port>/owner/repo.git
   $ cd repo
   $ echo "change" >> file
   $ git commit -am "round-trip change"
   $ git push
   $ cd ..; rm -rf repo
   $ git clone https://<token>@localhost:<port>/owner/repo.git
   $ cat repo/file  # must include "change"
   ```

2. **Interoperability tests against github.com fixtures.** We maintain a
   small set of canonical repos (one per fixture shape: empty, single
   commit, merge commit, tag with signature, large binary blob) at
   known github.com URLs. Our test harness clones them from github.com,
   re-pushes to temper-git, re-clones, and bit-diffs the pack streams.

3. **Hash-byte-match tests.** For every serialization helper
   (`blob_to_bytes`, `tree_to_bytes`, `commit_to_bytes`), a test asserts
   output hex-matches `git hash-object` on the same input.

4. **GitHub REST response-shape tests.** For every REST endpoint we ship,
   a test compares response-shape structure against a recorded
   github.com response (field names, types, required presence). The
   comparison ignores values but asserts structural compatibility.

### Storage

1. **Storage goes through Temper's event-store abstraction.** Entity
   writes dispatch through the kernel; the physical backend (embedded
   SQLite, libSQL, Postgres) sits behind that abstraction. Don't
   reach around it to the backend directly.

2. **A push is a single transactional write.** A push that creates
   N blobs + M trees + one commit + advances a ref must either all
   land or none land. Use the batch semantics the kernel exposes;
   never split the write into multiple uncoordinated POSTs.

3. **Never hold a whole pack in memory.** Pack sizes can reach
   hundreds of MB. Stream on the incoming side; stream on the
   outgoing side. Don't materialize full packs on the WASM heap.

### No-goes

- **Do not optimize for "our internal use case" over compatibility.** If
  GitHub's REST returns a field in camelCase and we return snake_case
  "because it's prettier," ecosystem tools break.
- **Do not add SHA-256 git.** When git-core ships SHA-256 repos as default
  (in the future), we follow. Until then, SHA-1 is the compatibility
  contract.
- **Do not introduce a caching layer over TreeEntry/Blob content.**
  Caching is Temper's job (it already caches entity state). A second
  layer means cache invalidation becomes our problem and introduces
  divergence risk.
- **Do not introduce a second blob-storage tier.** Blob content lives
  in the entity row. If a specific use case needs chunking or a spill
  tier, write an ADR first.
- **Do not support git-annex, git-lfs, or sub-modules in v1.** Submodules
  are in scope for v2 (just another blob-like pointer).
- **Do not emit pack-v1.** Not even as fallback. Modern git clients all
  speak v2.
- **Do not couple to a specific storage backend in application code.**
  Everything goes through the kernel's event-store abstraction.
