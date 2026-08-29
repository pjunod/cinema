# Cluster VOD index cache — build once, consume anywhere

**Status:** Revised after adversarial review; not yet implemented · **Scope:**
Fragment-index discovery, generation, publication, lookup, and cluster-safe
reuse · **Primary owners:** `plurx-core` storage and `plurxd` playback ·
**Last updated:** 2026-08-26

This plan replaces per-node, independently repeated VOD indexing with a durable
cluster work queue and a content-addressed, read-through index cache. Every
compatible node may build work, but a given immutable index identity is built
once and then consumed from a verified shared mount or a peer. The design keeps
index bytes out of Raft, fences stale workers, and preserves the exact byte
accounting required by landing-window matching.

This document extends the distributed pre-cache direction in
[PERF-PLAN.md](PERF-PLAN.md) and follows the clustering rule in
[CLUSTERING-PLAN.md](CLUSTERING-PLAN.md): replicate ownership and integrity
facts, not generated media bytes.

The independent adversarial findings and their dispositions are preserved in
[VOD-INDEX-CACHE-PLAN-REVIEW.md](VOD-INDEX-CACHE-PLAN-REVIEW.md).

## 1. Objective and current failure modes

### 1.1 Objective

Make fragment-index generation a bounded, horizontally scalable cluster job:

1. Each exact source-and-pipeline identity has one logical artifact.
2. Compatible voters claim different titles in parallel.
3. Any node can consume a completed artifact without rebuilding it.
4. A stale, incompatible, or partially written artifact is never accepted.
5. Index work yields quickly to foreground playback and offline transcodes.
6. The single-node path remains a supported degenerate cluster.

An "exact identity" is stronger than the existing file ID, size, modification
time, and copy-argument fingerprint. Section 4 defines it precisely.

### 1.2 Current behavior

The current index store is intentionally node-local. That decision is correct
for the current identity contract: fragment rows contain exact byte counts from
one local FFmpeg copy pipeline, and using another node's rows without proving
pipeline and source equivalence can select the wrong film position.

The surrounding scheduler has five independent cluster, liveness, and
efficiency problems:

1. Each node has its own wrapping, file-ID-ordered cursor. The cursor fixes the
   older prefix-starvation bug locally, but nodes coordinate neither discovery
   nor ownership and therefore repeat the same library work.
2. A complete FFmpeg run still has a total deadline. The current adaptive
   budget is `runtime / 8 + 30s`, clamped from 90 seconds to 30 minutes. A
   healthy input with missing/incorrect runtime or slower-than-assumed storage
   is killed and starts again from byte zero on a later pass.
3. `IndexOutcome::Unsupported` is logged but not persisted. The wrapping cursor
   retries terminally unsupported inputs on later generations.
4. `SourceIdentity::argv_fingerprint` hashes copy arguments, but not the actual
   FFmpeg executable or complete version/build output. An FFmpeg upgrade can
   therefore reuse an index produced by a materially different muxer.
5. Every voter builds its own complete local copy of the same index.

Work is sequential per node, starts on a coarse default 15-minute cadence, and
is capped at four attempts in a 120-second pass. The local cursor and adaptive
budget are useful mitigations; this plan preserves their lessons while moving
ownership and completion into a cluster contract.

The VOD-only path intentionally returns `503 vod_index_pending` when no usable
index exists. There is no live transcode fallback to hide these defects.

### 1.3 Success criteria

The implementation is complete only when all of these statements are tested:

- One exact key normally produces one full build across the cluster.
- Three compatible workers can claim three different titles concurrently.
- Every eligible library file is eventually discovered, even with more than
  200 files and repeated restarts.
- A healthy build lasting beyond the old calculated total budget completes.
- An unsupported key is not retried until an identity component changes.
- A worker that loses its lease cannot publish replicated completion.
- Nodes with different copy-engine digests never reuse each other's artifacts.
- Nodes whose full source-content digests differ never reuse artifacts.
- No index blob is stored in a Hiqlite command, WAL entry, or snapshot.
- Shared-mount loss falls back to peer hydration or a new build.
- Foreground pressure interrupts an index child within two seconds.
- Peer and shared-cache lookups have bounded time, body size, and memory use.
- A single-node installation follows the same state machine successfully.
- Every deployment class that enables v2 has passed the M0 byte, latency, and
  backlog gate; a measured no-go leaves the legacy path enabled.

### 1.4 Performance targets

These are initial service targets, not assumptions embedded in correctness
logic:

| Measure | Initial target | Reason |
| --- | ---: | --- |
| Local cache lookup | p95 under 10 ms | Session creation should stay cheap. |
| Verified shared lookup | p95 under 250 ms | Shared storage is the preferred remote tier. |
| Peer hydration | p95 under 1.5 s | A peer miss must not hang session creation. |
| Source digest | one full read, once per local object version | Exact cross-node identity. |
| Foreground preemption | under 2 s | Background indexing must not impair playback. |
| Default cluster builders | 2 | Adds useful parallelism without assuming NAS headroom. |
| Maximum configured builders | 8 | Prevents an accidental cluster-wide I/O stampede. |

The target builder count is cluster-wide. Per-node process concurrency remains
one in the first implementation.

## 2. Decisions and non-goals

### 2.1 Decisions

The first release makes these binding choices:

1. **Replicate metadata, store blobs outside Raft.** Hiqlite stores job,
   artifact, and location facts. The encoded index lives in the node-local
   sidecar or the already verified shared-cache root.
2. **Use a dedicated job store.** Fragment indexing copies the proven
   lease/fence/CAS behavior of `PretranscodeJobStore`, but does not overload
   pretranscode states or rows.
3. **Publish immutable, content-addressed blobs.** A key and blob digest never
   change in place. A conflicting digest for the same key is a correctness
   fault, not last-writer-wins behavior.
4. **Hydrate on read.** Consumers prefer local, then shared, then an authorized
   peer. A remote hit is verified and saved locally before use.
5. **Parallelize across files first.** Different nodes build different titles.
   This is simple, naturally abundant, and preserves the current one-pass
   FFmpeg semantics.
6. **Replace total runtime with progress supervision.** A build may run as long
   as it continues making bounded, measurable forward progress and retains its
   lease.
7. **Keep legacy indexes local during migration.** They may serve on their
   original node while v2 is backfilled, but are never advertised to peers.

### 2.2 Non-goals for the first release

- Splitting one title into parallel time ranges.
- Putting index blobs in replicated SQL, Raft snapshots, or peer membership
  messages.
- Exposing an index endpoint to normal clients.
- Reusing indexes across FFmpeg or Plurx copy-contract boundaries.
- Replicating rendition plans; those remain node-local and include the new
  index key in their identity.
- Indexing while `playback.vod_index_mins = 0`.
- Deleting the legacy `fragment_indexes` table in the first release.
- Enforcing a hard retention deadline while a current voter is unavailable or
  cannot attest its present key. Section 9.4 deliberately fails closed in that
  case; membership cleanup is the operator-controlled escape hatch.

### 2.3 Why one shared SQL blob table is rejected

A typical two-hour index is under 100 KiB, which makes a replicated blob table
look attractive. It is still the wrong boundary. Backfills can involve many
thousands of titles, causing blob bytes to be written to the leader log,
replicated to every voter, retained in WAL history, and copied into snapshots.
It also couples a regenerable cache to control-plane availability and growth.

The catalog rows are small and correctness-sensitive. The blobs are larger,
regenerable, and suited to shared storage or peer transfer. Keeping those
properties separate is the intended cluster architecture.

## 3. Architecture

### 3.1 Components

```
                 replicated Hiqlite metadata
       +-------------------------------------------+
       | fragment_index_jobs                      |
       | fragment_index_artifacts                 |
       | fragment_index_locations                 |
       | discovery leases and cursors             |
       +--------------------+----------------------+
                            |
                      lease / fence
                            |
            +---------------+----------------+
            |                                |
       +----v-----+                     +----v-----+
       | voter A  |                     | voter B  |
       | builder  |                     | builder  |
       +----+-----+                     +----+-----+
            |                                |
            +--------- distinct jobs --------+
                            |
                  deterministic blob
                            |
         +------------------+------------------+
         |                                     |
 +-------v---------+                   +-------v---------+
 | local sidecars  |                   | verified shared |
 | peer-readable   |                   | cache root      |
 +-------+---------+                   +-------+---------+
         |                                     |
         +---------- read-through -------------+
                            |
                     VOD index resolver
                            |
                     landing + serving
```

### 3.2 Control flow

1. A compatibility-group discoverer scans an ordered page of library files and
   advances a durable cursor.
2. It computes or loads the local full-content digest from a stable open file,
   derives the exact index key, and inserts one durable job row for that key.
3. Compatible nodes claim different due jobs with a lease and monotone fence.
4. The worker holds that attested file descriptor, passes the descriptor to
   FFmpeg, runs under progress and foreground supervision, encodes a
   deterministic blob, and validates the descriptor again.
5. It atomically installs the local blob and optionally the shared copy.
6. A fenced transaction publishes the artifact and valid locations and marks
   the job ready.
7. A VOD resolver computes the same key and reads local, shared, or peer bytes.
   Every remote result is size-, digest-, structure-, and identity-checked.

### 3.3 Data placement

| Data | Authority | Placement | Replicated |
| --- | --- | --- | --- |
| Job state, lease, fence | Hiqlite | Durable store | Yes |
| Artifact identity and digest | Hiqlite | Durable store | Yes |
| Location advertisements | Hiqlite | Durable store | Yes |
| Discovery cursor | Hiqlite | Coordination store | Yes |
| Index blob | Cache tier | Local sidecar/shared root | No |
| Legacy index row | Existing node | Local sidecar | No |
| Local full-digest memo | Existing node | Local sidecar | No |

The replicated artifact proves what bytes are valid. A location only says where
those bytes may be fetched; it never weakens the digest or key check.

### 3.4 Cache tier order

The resolver uses this order:

1. A complete v2 blob in the local sidecar.
2. A complete blob beneath a currently verified shared-cache root.
3. One currently reachable voter advertising a complete local copy.
4. A queued or reprioritized build, followed by `503 vod_index_pending`.

The shared tier is an optimization, not an availability dependency. Without a
shared mount, the builder's local sidecar is the initial holder and consumers
hydrate over the authenticated peer route. If that holder disappears before
hydration, a different compatible worker may rebuild the same immutable key.

## 4. Identity and blob contracts

### 4.1 `FragmentIndexKey`

The v2 key is the lowercase SHA-256 hex digest of a versioned,
length-prefixed binary encoding. Concatenating text with separators is not
allowed because escaping and ambiguity would become part of the contract.

```rust
pub struct FragmentIndexKeyInput {
    pub file_id: i64,
    pub source_size: u64,
    pub source_mtime: i64,
    pub source_digest_contract_version: u16,
    pub source_content_sha256: [u8; 32],
    pub indexer_contract_version: u32,
    pub segplan_version: u32,
    pub copy_engine_sha256: [u8; 32],
    pub file_pipeline_sha256: [u8; 32],
}

pub struct FragmentIndexKey(pub [u8; 32]);
```

The encoded field order is exactly the order above. Integers are big-endian.
Every variable-length field in a subordinate digest input is preceded by an
unsigned 64-bit big-endian byte length. The top-level encoding begins with
ASCII `plurx-fragment-index-key` and `u16(1)`.

`file_id` deliberately prevents cross-title aliasing even when two files have
the same content. Deduplicating identical media objects is a separate
storage feature and must not emerge accidentally from a cache key.

### 4.2 Copy-engine digest

`copy_engine_sha256` is established during daemon startup from a retained
`AttestedCopyEngine`:

1. ASCII `plurx-copy-engine` and `u16(1)`.
2. `INDEX_PIPE_CONTRACT_VERSION`, initially `1`.
3. SHA-256 of the resolved FFmpeg executable bytes.
4. A sorted, length-prefixed dependency manifest containing each ELF/Mach-O
   dynamic-library install name and SHA-256 of its resolved bytes.
5. The complete stdout and stderr from `ffmpeg -version`, with no line
   truncation and a hard 1 MiB combined limit.
6. Static Plurx arguments and muxer choices that affect emitted fragment
   boundaries or bytes.

```rust
pub struct AttestedCopyEngine {
    pub digest: [u8; 32],
    pub executable: std::fs::File,
    pub executable_version: LocalFileObjectVersion,
    pub dependencies: Vec<AttestedEngineDependency>,
}
```

Startup fails closed for v2 indexing if the executable or any declared dynamic
dependency cannot be resolved, read, or identified. The dependency walker
supports ELF `DT_NEEDED` on Linux and Mach-O load commands on macOS, resolves
loader/rpath tokens, hashes dependencies recursively, and rejects cycles only
after recording the already-visited identity. Process locale and timezone are
set to fixed values, and output-affecting environment variables are either
cleared or included as length-prefixed digest inputs. Existing non-index
playback startup behavior is not changed. The daemon exposes the digest in node
capabilities and aggregate diagnostics, never executable/library paths.

The daemon retains open handles and object-version tuples for the FFmpeg
executable and every dependency, including system libraries. Before **every**
index and VOD FFmpeg spawn, the launcher re-resolves all paths and requires an
exact tuple match. It also watches the engine/dependency directories; any
change withdraws the node's v2 capability, cancels active background work, and makes new v2
spawns fail `copy_engine_changed` until daemon restart. A package upgrade must
therefore drain and restart `plurxd`.

On Linux the launcher executes the retained executable handle with `execveat`
and `AT_EMPTY_PATH`, closing the executable pathname race. The dynamic loader
still opens dependencies by name, so v1 on **both** Linux and macOS requires an
operator-declared immutable/versioned runtime bundle or immutable container/OS
image containing FFmpeg and its entire dependency closure. The launcher
revalidates immediately before spawn and keeps all handles open. Concurrent
privileged mutation of that declared immutable runtime between final check and
loader mapping is explicitly outside the storage threat model. A deployment
whose runtime/dependency paths are writable by package upgrades while `plurxd`
runs is ineligible for v2 and remains on the local legacy path. Upgrades drain,
replace the bundle/image, and restart the daemon.

No v2 code path may call `Command::new(ffmpeg_bin())` directly. All such spawns
go through the attested launcher. Tests replace the live engine path after
startup: capability disappears, old-key work cannot publish, and restart
derives the new digest.

The internal full-digest helper likewise executes from a retained handle to the
current `plurxd` image on Linux, or the same declared immutable application
bundle on macOS. Its algorithm identity is `source_digest_contract_version`,
so replacing the installed daemon cannot silently change a running process's
source digest helper.

`INDEX_PIPE_CONTRACT_VERSION` must be incremented whenever Plurx logic could
change the copied video stream, initialization section, fragment boundaries,
timescale, byte count, or classification. The reason is to invalidate old
indexes even when the FFmpeg installation is unchanged.

`indexer_contract_version` starts at `1` and covers rules that need not affect
FFmpeg output but do affect whether a result is accepted or terminal: parser
behavior, blob envelope/caps, semantic validation, and unsupported
classification. It is incremented whenever one of those rules changes. This
prevents an old `unsupported` row from blocking a title after the software
learns how to accept it.

### 4.3 Per-file pipeline digest

`file_pipeline_sha256` includes all per-file, output-affecting choices in the
actual execution order. It excludes source path spelling because nodes can
mount the same library at different paths.

The initial input is:

```text
"plurx-fragment-index-file-pipeline"
u16(1)
length-prefixed copy_video_args, one UTF-8 argument at a time
length-prefixed selected video stream identity
length-prefixed generated movflags
length-prefixed fragment duration policy
```

The worker constructs the command and the digest from one typed recipe. It is
forbidden to separately assemble a command and then approximate its identity.
Unit tests assert that every output-affecting recipe field changes the digest
and that source-path-only changes do not.

### 4.4 Exact source identity

Size, modification time, and sparse sampling cannot prove that two nodes will
copy the same packet bytes. The correctness identity is therefore SHA-256 of
the complete source file. Sparse sampling may be used only as a cheap mismatch
prefilter; it is never part of an acceptance decision.

The full digest algorithm is:

1. Resolve the replicated path beneath the configured canonical library root.
   Version 1 requires the same canonical root/path on every participating node;
   explicit replicated-to-local mount mapping is deferred from this release.
2. Open the file read-only with the platform's no-follow protections and reject
   any resolved object outside the configured root.
3. Record `fstat` device, inode, size, nanosecond mtime, and nanosecond ctime.
4. Hash ASCII `plurx-source-content`, `u16(1)`, the unsigned 64-bit length, and
   every file byte in order.
5. Repeat `fstat` on the same descriptor and reject any tuple change.
6. Require the length and catalog-unit mtime to match the replicated `files`
   row, then return the descriptor and digest together.

```rust
pub struct AttestedMediaSource {
    pub file: std::fs::File,
    pub catalog: LibraryFileSnapshot,
    pub object_version: LocalFileObjectVersion,
    pub content_sha256: [u8; 32],
}

pub struct LocalFileObjectVersion {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_unix_ns: i128,
    pub ctime_unix_ns: i128,
}
```

A node-local memo maps the complete `LocalFileObjectVersion` plus digest
contract version to the full content digest. The memo is valid only while a new
`fstat` on the held descriptor exactly matches every tuple field. The contract
assumes the configured filesystem reports mutation through mtime or ctime;
privileged rollback of both timestamps is outside the storage threat model.

The first check of a new object version costs one full sequential read. It runs
as background work: a VOD request with no memo enqueues a high-priority digest
task and returns `vod_index_pending` rather than hashing a movie synchronously.
It does not enqueue when `playback.vod_index_mins = 0` or the cluster-cache
feature is disabled.
Discovery and artifact hydration prewarm these memos. Subsequent local lookups
only open, `fstat`, and compare the memo, which preserves the local latency
target.

The queue is the node-local, object-version-keyed
`source_content_digest_jobs` table in Section 5.9. Repeated misses for one file
upsert one row before an index key exists; they cannot create helper processes
or rows without bound.

The full read runs in a killable internal helper subprocess against inherited
descriptor 3, not an uncancellable in-process blocking/NFS read. The helper
emits bounded byte-progress frames and one final 32-byte digest, then exits zero.
The supervisor applies the same 90-second no-progress timeout and two-second
foreground termination bound as the index child. Digest publication requires a
valid final frame, EOF, zero exit, and unchanged parent descriptor tuple.

### 4.5 Stable-handle FFmpeg contract

Attesting one descriptor and later passing its pathname to FFmpeg is a TOCTOU
bug. Every v2 index build and VOD generation process consumes the exact held
descriptor from `AttestedMediaSource`:

1. Duplicate it to fixed child descriptor 3 immediately before `exec`.
2. Clear close-on-exec only on the child duplicate.
3. Seek the parent handle to byte zero before duplication. One VOD session may
   have only one active FFmpeg reader for that handle; separate sessions open
   independent handles.
4. Pass `/proc/self/fd/3` on Linux or `/dev/fd/3` on macOS as the input URL.
5. Fail v2 closed on a platform/demuxer for which the inherited regular-file
   descriptor is not seekable.
6. For an index build, recheck the parent descriptor's object tuple before and
   after the FFmpeg invocation. No build bytes are authoritative before that
   final check.
7. For VOD, buffer the already bounded initialization object or one complete
   planned segment, recheck the held descriptor tuple, and only then
   materialize/notify it. Repeat the check after every complete segment. A
   fragment is never streamed to a client directly from the live child pipe.

The typed command recipe takes an `AttestedMediaSource` handle rather than a
source pathname. Path spelling remains excluded from pipeline identity because
it cannot reach the v2 command. Tests rename and replace the original path
between attestation and spawn and prove that FFmpeg still consumes the held
object.

`RenditionSink::materialize` owns the VOD publication barrier. The existing
`vodgen` path already presents one complete `Vec<u8>` bounded by
`COPY_SEGMENT_MAX_BYTES`; the sink re-`fstat`s before the rendition-directory
commit and waiter notification. Initialization uses the same guard before
`write_init`. If size, mtime, or ctime changes, the sink refuses the bytes,
increments the generation epoch, kills the child, records typed
`vod_source_changed`, and leaves the old-key rendition isolated. A new session
must attest the new object version and resolve or build its new key. Mutation
after a check cannot alter the already buffered bytes; it is detected before
the next initialization object or segment is exposed. Privileged timestamp
rollback remains outside the explicit storage threat model in Section 4.4.

### 4.6 Deterministic v2 blob

The blob is immutable and independently verifiable:

```text
+----------------------+-----------------------------------+
| Field                | Encoding                          |
+----------------------+-----------------------------------+
| magic                | 8 bytes: PLRXFIDX                 |
| envelope version     | u16 big-endian: 1                 |
| header length        | u32 big-endian                    |
| header               | v1 fields below                   |
| row count            | u32 big-endian                    |
| rows                 | row count * 24 bytes              |
+----------------------+-----------------------------------+
```

The v1 header is the following exact byte sequence. All header integers are
big-endian. Digests are raw 32-byte values, not hexadecimal text.

| Header field | Width/encoding |
| --- | --- |
| file ID | signed 64-bit |
| source size | unsigned 64-bit |
| source mtime | signed 64-bit |
| source digest contract | unsigned 16-bit |
| source content SHA-256 | 32 bytes |
| indexer contract | unsigned 32-bit |
| segment-plan version | unsigned 32-bit |
| copy-engine SHA-256 | 32 bytes |
| file-pipeline SHA-256 | 32 bytes |
| index key | 32 bytes |
| row encoding version | unsigned 16-bit, value 1 |
| timescale | unsigned 32-bit, nonzero |
| initialization SHA-256 | 32 bytes |
| parameter sets constant | unsigned 8-bit, 0 or 1 |
| reserved | three zero bytes |
| parameter-set count | unsigned 32-bit |
| each parameter set | unsigned 32-bit length, then bytes |
| HDR10 SEI count | unsigned 32-bit |
| each HDR10 SEI | unsigned 32-bit length, then bytes |

The header length is exactly the number of bytes beginning at file ID and
ending after the final HDR10 SEI. Counts and lengths use checked arithmetic and
must fit the 64 KiB header cap. Empty vectors encode with count zero. Vector
order is the `PromotionInputs` discovery order and is never sorted or deduped.
The header contains no builder ID, hostname, path, timestamp, map iteration, or
other nondeterministic value.

Each v1 row is exactly the current 24-byte contract:

| Offset | Field | Encoding |
| ---: | --- | --- |
| 0 | DTS | unsigned 64-bit little-endian |
| 8 | duration | unsigned 32-bit little-endian |
| 12 | wire bytes | unsigned 32-bit little-endian |
| 16 | video bytes | unsigned 32-bit little-endian |
| 20 | cut class | `1=CleanIdr`, `2=CleanCra`, `3=Dirty`, `4=Unparseable` |
| 21 | reserved | three zero bytes |

Encoding fails if a duration does not fit `u32`; v2 never preserves the
current saturating conversion. Decoding rejects unknown classes or nonzero
padding. Packing and unpacking move into one `FragmentIndexBlob` codec in
`plurx-core` so SQLite, Hiqlite metadata, peer transfer, and VOD serving cannot
drift.

Decoder limits are binding:

- Maximum complete blob: 32 MiB.
- Maximum header: 64 KiB.
- Initialization and promoted-parameter data must fit within the header cap.
- `rows.len()` must equal `row_count * 24` using checked arithmetic.
- No trailing bytes are accepted.
- The header key must equal the requested key.
- The SHA-256 of the complete blob must equal the artifact catalog digest.

A larger legitimate index is reported as typed `index_blob_too_large` and is
not published. Raising the cap requires a contract review and
`INDEXER_CONTRACT_VERSION` bump because the peer endpoint, terminal-negative
cache, and session-creation memory bound depend on it.

## 5. Replicated schema and store contracts

### 5.1 Migration versions

The implementation adds:

- SQLite migration `v30` for the complete standalone job/artifact/location,
  current-key observation, publication/GC, discovery-cursor, local v2 blob,
  and source-digest memo/job schema.
- Hiqlite durable/auth schema version `12` for jobs, artifacts, canonical
  locations, both observation tables, publication/GC fencing, and discovery
  cursors.
- Node-local telemetry sidecar schema version `7` for v2 blobs and
  source-digest memos/jobs.

All migration paths must remain one-version-at-a-time and transactional.
Upgrade tests cover the current production version to the new version and a
fresh database. `hiqlite_import.rs`, bootstrap SQL, schema digest expectations,
and cluster growth fixtures must be updated in the same change as durable SQL.

### 5.2 Artifact table

```sql
CREATE TABLE fragment_index_artifacts (
    index_key TEXT PRIMARY KEY
        CHECK(length(index_key) = 64
          AND index_key NOT GLOB '*[^0-9a-f]*'),
    file_id INTEGER NOT NULL
        REFERENCES files(id) ON DELETE CASCADE,
    source_size INTEGER NOT NULL CHECK(source_size >= 0),
    source_mtime INTEGER NOT NULL CHECK(source_mtime >= 0),
    source_digest_contract_version INTEGER NOT NULL
        CHECK(source_digest_contract_version > 0),
    source_content_sha256 TEXT NOT NULL
        CHECK(length(source_content_sha256) = 64
          AND source_content_sha256 NOT GLOB '*[^0-9a-f]*'),
    indexer_contract_version INTEGER NOT NULL
        CHECK(indexer_contract_version > 0),
    segplan_version INTEGER NOT NULL CHECK(segplan_version > 0),
    copy_engine_sha256 TEXT NOT NULL
        CHECK(length(copy_engine_sha256) = 64
          AND copy_engine_sha256 NOT GLOB '*[^0-9a-f]*'),
    file_pipeline_sha256 TEXT NOT NULL
        CHECK(length(file_pipeline_sha256) = 64
          AND file_pipeline_sha256 NOT GLOB '*[^0-9a-f]*'),
    blob_sha256 TEXT NOT NULL
        CHECK(length(blob_sha256) = 64
          AND blob_sha256 NOT GLOB '*[^0-9a-f]*'),
    blob_bytes INTEGER NOT NULL
        CHECK(blob_bytes > 0 AND blob_bytes <= 33554432),
    fragment_count INTEGER NOT NULL CHECK(fragment_count > 0),
    timescale INTEGER NOT NULL CHECK(timescale > 0),
    first_builder_node_id TEXT NOT NULL
        CHECK(length(first_builder_node_id) BETWEEN 1 AND 256),
    built_at_ms INTEGER NOT NULL CHECK(built_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    superseded_at_ms INTEGER
        CHECK(superseded_at_ms IS NULL OR superseded_at_ms >= 0)
) STRICT;

CREATE INDEX fragment_index_artifacts_file
    ON fragment_index_artifacts(file_id, built_at_ms DESC);
```

`source_mtime` uses the exact unit and value already stored in `files.mtime`.
Changing the scanner's time precision is a separate catalog migration and would
naturally change every affected index key.

Immutable equality covers every column through `timescale` plus blob digest
and size. `first_builder_node_id`, `built_at_ms`, and `created_at_ms` are
first-publication provenance: an identical rebuild neither compares nor
updates them. `superseded_at_ms` is mutable retention state and is also excluded
from artifact equality.

### 5.3 Current-key observations

```sql
CREATE TABLE fragment_index_key_observations (
    file_id INTEGER NOT NULL
        REFERENCES files(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL CHECK(length(node_id) BETWEEN 1 AND 256),
    index_key TEXT NOT NULL
        CHECK(length(index_key) = 64
          AND index_key NOT GLOB '*[^0-9a-f]*'),
    source_content_sha256 TEXT NOT NULL
        CHECK(length(source_content_sha256) = 64
          AND source_content_sha256 NOT GLOB '*[^0-9a-f]*'),
    source_root_sha256 TEXT NOT NULL
        CHECK(length(source_root_sha256) = 64
          AND source_root_sha256 NOT GLOB '*[^0-9a-f]*'),
    key_changed_at_ms INTEGER NOT NULL CHECK(key_changed_at_ms >= 0),
    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
    PRIMARY KEY(file_id, node_id)
) STRICT;

CREATE INDEX fragment_index_key_observations_key
    ON fragment_index_key_observations(index_key, observed_at_ms);
```

Discovery and successful resolver identity checks upsert this node's current
key. `key_changed_at_ms` changes only when the key changes; identical refreshes
are coalesced to at most once per 24 hours. The row exists before an artifact
does and therefore deliberately has no artifact foreign key. It is the bounded
proof used to retire old source, engine, pipeline, segment-plan, and indexer
variants in Section 9.4.

### 5.4 Location table

```sql
CREATE TABLE fragment_index_locations (
    index_key TEXT NOT NULL
        REFERENCES fragment_index_artifacts(index_key) ON DELETE CASCADE,
    storage_class TEXT NOT NULL
        CHECK(storage_class IN ('node_local', 'shared')),
    storage_id TEXT NOT NULL CHECK(length(storage_id) BETWEEN 1 AND 256),
    relative_path TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 512),
    blob_sha256 TEXT NOT NULL
        CHECK(length(blob_sha256) = 64
          AND blob_sha256 NOT GLOB '*[^0-9a-f]*'),
    blob_bytes INTEGER NOT NULL
        CHECK(blob_bytes > 0 AND blob_bytes <= 33554432),
    state TEXT NOT NULL CHECK(state IN ('complete', 'retired')),
    verified_at_ms INTEGER NOT NULL CHECK(verified_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    PRIMARY KEY(index_key, storage_class, storage_id)
) STRICT;

CREATE INDEX fragment_index_locations_lookup
    ON fragment_index_locations(index_key, state, storage_class);

CREATE TABLE fragment_index_location_observations (
    index_key TEXT NOT NULL
        CHECK(length(index_key) = 64
          AND index_key NOT GLOB '*[^0-9a-f]*'),
    storage_class TEXT NOT NULL,
    storage_id TEXT NOT NULL,
    observer_node_id TEXT NOT NULL
        CHECK(length(observer_node_id) BETWEEN 1 AND 256),
    status TEXT NOT NULL CHECK(status IN (
        'healthy', 'missing', 'corrupt', 'unreachable'
    )),
    observed_blob_sha256 TEXT,
    first_unhealthy_at_ms INTEGER,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
        CHECK(consecutive_failures >= 0),
    shared_canary_verified_at_ms INTEGER
        CHECK(shared_canary_verified_at_ms IS NULL
           OR shared_canary_verified_at_ms >= 0),
    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
    PRIMARY KEY(index_key, storage_class, storage_id, observer_node_id),
    FOREIGN KEY(index_key, storage_class, storage_id)
        REFERENCES fragment_index_locations(
            index_key, storage_class, storage_id
        ) ON DELETE CASCADE,
    CHECK(observed_blob_sha256 IS NULL OR (
        length(observed_blob_sha256) = 64
        AND observed_blob_sha256 NOT GLOB '*[^0-9a-f]*'
    )),
    CHECK(
        (status = 'healthy' AND first_unhealthy_at_ms IS NULL
         AND consecutive_failures = 0)
        OR
        (status != 'healthy' AND first_unhealthy_at_ms IS NOT NULL
         AND consecutive_failures > 0)
    ),
    CHECK(
        storage_class != 'shared'
        OR status = 'unreachable'
        OR shared_canary_verified_at_ms IS NOT NULL
    )
) STRICT;
```

For local storage, `storage_id` is the stable node ID. For shared storage it is
the verified shared-cache domain ID, not a raw mount path. These fields form the
canonical location identity, so duplicate advertisements cannot hide a healthy
holder behind the resolver's bounded location limit. `relative_path` must equal
the path derived from the key; it is stored for diagnostics but never trusted
as arbitrary filesystem input.

The store accepts a `node_local` publication or location only when `storage_id`
equals the authenticated/current publication owner. Only that holder may claim
GC for the local row. A shared `storage_id` must equal a currently configured,
verified shared-cache domain.

A node records its own observation instead of globally poisoning a location.
A write is accepted only when `observer_node_id` is the caller's current exact
voter identity; observations from removed members are pruned.
A node-local location retires only when its holder confirms absence or cluster
membership has authoritatively removed the holder. A shared location retires
only after `missing` or `corrupt` observations from two current voters in that
shared domain, or from the sole voter when the domain has one member. Both bad
observations must be at most 60 seconds old, each must follow a locally verified
domain canary by at most 30 seconds, and no current voter may have a later
`healthy` observation for that location/digest. Historical failures therefore
cannot accumulate into retirement. `unreachable` never retires a location. A
healthy observation clears the observer's unhealthy start/count; repeated
unhealthy observations preserve the first timestamp and increment the count
with saturation.

Resolver queries return at most 16 canonical rows in deterministic order:
this node's local row, this node's verified shared domain, then node-local rows
whose holders are current exact voters ordered by most recent healthy
observation and `storage_id`. The query never returns duplicate storage IDs.
This bound therefore cannot be filled with attacker-chosen location aliases.

### 5.5 Job table

```sql
CREATE TABLE fragment_index_jobs (
    job_id TEXT PRIMARY KEY CHECK(length(job_id) BETWEEN 1 AND 256),
    index_key TEXT NOT NULL UNIQUE
        CHECK(length(index_key) = 64
          AND index_key NOT GLOB '*[^0-9a-f]*'),
    file_id INTEGER NOT NULL,
    source_size INTEGER NOT NULL CHECK(source_size >= 0),
    source_mtime INTEGER NOT NULL CHECK(source_mtime >= 0),
    source_digest_contract_version INTEGER NOT NULL
        CHECK(source_digest_contract_version > 0),
    source_content_sha256 TEXT NOT NULL
        CHECK(length(source_content_sha256) = 64
          AND source_content_sha256 NOT GLOB '*[^0-9a-f]*'),
    indexer_contract_version INTEGER NOT NULL
        CHECK(indexer_contract_version > 0),
    segplan_version INTEGER NOT NULL CHECK(segplan_version > 0),
    copy_engine_sha256 TEXT NOT NULL
        CHECK(length(copy_engine_sha256) = 64
          AND copy_engine_sha256 NOT GLOB '*[^0-9a-f]*'),
    file_pipeline_sha256 TEXT NOT NULL
        CHECK(length(file_pipeline_sha256) = 64
          AND file_pipeline_sha256 NOT GLOB '*[^0-9a-f]*'),
    source_root_sha256 TEXT NOT NULL
        CHECK(length(source_root_sha256) = 64
          AND source_root_sha256 NOT GLOB '*[^0-9a-f]*'),
    priority INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK(state IN (
        'queued', 'running', 'ready', 'unsupported', 'failed', 'cancelled'
    )),
    owner_node_id TEXT
        CHECK(owner_node_id IS NULL OR length(owner_node_id) BETWEEN 1 AND 256),
    fence INTEGER NOT NULL DEFAULT 0
        CHECK(fence >= 0 AND fence < 9223372036854775807),
    lease_expires_ms INTEGER CHECK(lease_expires_ms IS NULL OR lease_expires_ms >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    failure_attempts INTEGER NOT NULL DEFAULT 0 CHECK(failure_attempts >= 0),
    not_before_ms INTEGER NOT NULL CHECK(not_before_ms >= 0),
    error_code TEXT CHECK(error_code IS NULL OR length(error_code) <= 128),
    error_detail TEXT CHECK(error_detail IS NULL OR length(error_detail) <= 1024),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
    completed_at_ms INTEGER CHECK(completed_at_ms IS NULL OR completed_at_ms >= 0)
) STRICT;

CREATE INDEX fragment_index_jobs_due
    ON fragment_index_jobs(state, not_before_ms, priority DESC,
                           created_at_ms, job_id);

CREATE INDEX fragment_index_jobs_due_compat
    ON fragment_index_jobs(
        state, copy_engine_sha256, segplan_version, source_root_sha256,
        not_before_ms, priority DESC, created_at_ms, job_id
    );
```

One durable row per key collapses discovery, VOD-miss, retry, and rebuild races.
`unsupported` and exhausted `failed` rows remain tombstones for the exact key.
Operator retry is an explicit CAS from `failed` to `queued` that resets
`failure_attempts`; normal enqueue never creates a replacement row. A changed
source or contract produces a different key and therefore a different row.

A `ready` row may transition back to `queued` only through
`requeue_fragment_index_unavailable`. The same transaction must prove the
artifact exists and either:

1. it has zero `complete` locations; or
2. one current exact-voter requester has at least three consecutive unhealthy
   observations spanning 30 seconds for every complete location.

The second rule requests a safe redundant rebuild but does not retire any
location globally; one node's bad mount therefore cannot disable healthy peers.
The artifact and blob digest remain authoritative, so the rebuild must
reproduce that digest. This is the only automatic terminal-to-queued
transition.

### 5.6 Deletion trigger

```sql
CREATE TRIGGER fragment_index_jobs_source_delete
BEFORE DELETE ON files
BEGIN
    DELETE FROM fragment_index_jobs
     WHERE file_id = OLD.id
       AND state IN ('ready', 'unsupported', 'failed', 'cancelled');

    UPDATE fragment_index_jobs
       SET state = 'cancelled',
           owner_node_id = NULL,
           lease_expires_ms = NULL,
           error_code = 'source_deleted',
           error_detail = NULL
     WHERE file_id = OLD.id
       AND state IN ('queued', 'running');
END;
```

The trigger uses no wall-clock SQL because all voters must apply identical
state. It deliberately leaves prior timestamps unchanged. The job has no
cascading foreign key: a running claim must become terminal so its stale fence
cannot publish after deletion. The artifact foreign key cascades completed
metadata. Blob deletion is asynchronous because SQL transactions cannot safely
mutate cache files.

### 5.7 Publication reservations and GC tombstones

Physical installation and deletion need a replicated exclusion protocol. A
catalog lookup followed by filesystem mutation is not atomic and permits
finish-versus-GC races.

```sql
CREATE TABLE fragment_index_publication_leases (
    index_key TEXT NOT NULL,
    storage_class TEXT NOT NULL
        CHECK(storage_class IN ('node_local', 'shared')),
    storage_id TEXT NOT NULL CHECK(length(storage_id) BETWEEN 1 AND 256),
    publication_incarnation TEXT NOT NULL
        CHECK(length(publication_incarnation) = 32
          AND publication_incarnation NOT GLOB '*[^0-9a-f]*'),
    state TEXT NOT NULL CHECK(state IN ('idle', 'reserved')),
    publication_fence INTEGER NOT NULL
        CHECK(publication_fence >= 0
          AND publication_fence < 9223372036854775807),
    owner_node_id TEXT
        CHECK(owner_node_id IS NULL
           OR length(owner_node_id) BETWEEN 1 AND 256),
    authority_class TEXT
        CHECK(authority_class IS NULL
           OR authority_class IN ('job', 'artifact')),
    authority_id TEXT
        CHECK(authority_id IS NULL OR length(authority_id) BETWEEN 1 AND 256),
    authority_fence INTEGER
        CHECK(authority_fence IS NULL OR authority_fence >= 0),
    blob_sha256 TEXT
        CHECK(blob_sha256 IS NULL OR (
            length(blob_sha256) = 64
            AND blob_sha256 NOT GLOB '*[^0-9a-f]*'
        )),
    blob_bytes INTEGER
        CHECK(blob_bytes IS NULL OR
              (blob_bytes > 0 AND blob_bytes <= 33554432)),
    relative_path TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 512),
    expires_at_ms INTEGER CHECK(expires_at_ms IS NULL OR expires_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
    CHECK(
        (state = 'reserved' AND publication_fence > 0
         AND owner_node_id IS NOT NULL AND authority_class IS NOT NULL
         AND authority_id IS NOT NULL AND authority_fence IS NOT NULL
         AND blob_sha256 IS NOT NULL AND blob_bytes IS NOT NULL
         AND expires_at_ms IS NOT NULL)
        OR
        (state = 'idle' AND blob_sha256 IS NULL AND blob_bytes IS NULL
         AND expires_at_ms IS NULL AND owner_node_id IS NULL
         AND authority_class IS NULL AND authority_id IS NULL
         AND authority_fence IS NULL)
    ),
    CHECK(
        (authority_class = 'job' AND authority_fence > 0)
        OR
        (authority_class = 'artifact' AND authority_fence = 0)
        OR
        (authority_class IS NULL AND authority_fence IS NULL)
    ),
    PRIMARY KEY(index_key, storage_class, storage_id)
) STRICT;

CREATE TABLE fragment_index_gc_tombstones (
    index_key TEXT NOT NULL
        CHECK(length(index_key) = 64
          AND index_key NOT GLOB '*[^0-9a-f]*'),
    storage_class TEXT NOT NULL
        CHECK(storage_class IN ('node_local', 'shared')),
    storage_id TEXT NOT NULL CHECK(length(storage_id) BETWEEN 1 AND 256),
    gc_resource TEXT NOT NULL CHECK(length(gc_resource) BETWEEN 1 AND 512),
    gc_owner_node_id TEXT NOT NULL
        CHECK(length(gc_owner_node_id) BETWEEN 1 AND 256),
    gc_fence INTEGER NOT NULL
        CHECK(gc_fence > 0 AND gc_fence < 9223372036854775807),
    invalidated_publication_fence INTEGER NOT NULL
        CHECK(invalidated_publication_fence >= 0
          AND invalidated_publication_fence < 9223372036854775807),
    invalidated_publication_incarnation TEXT NOT NULL
        CHECK(length(invalidated_publication_incarnation) = 32
          AND invalidated_publication_incarnation NOT GLOB '*[^0-9a-f]*'),
    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    PRIMARY KEY(index_key, storage_class, storage_id)
) STRICT;
```

After a producer has an expected digest, it acquires the canonical location's
publication lease by presenting the row's exact `publication_incarnation` and
incrementing `publication_fence`. An absent row is inserted with a
caller-generated random 128-bit lowercase-hex incarnation; the value is part
of the replicated command so every voter applies the same bytes. In that same
transaction it rejects an active GC tombstone or deletes an expired tombstone;
deletion makes any delayed GC revalidation fail. A first builder uses
`authority_class = 'job'`; the transaction validates its exact live job fence
and enable settings. Hydration or repair uses `authority_class = 'artifact'`;
the transaction validates the existing artifact's exact digest and size. Both
paths validate the canonical path and absence of an unexpired GC tombstone.

Reservation does not make bytes serveable. Builder completion or replica
registration must present the exact `FragmentIndexPublicationLease`; the same
transaction requires `state = 'reserved'`, incarnation, owner, fence, expected
expiry, live job/artifact authority, and no GC tombstone. The lease value carries
the key, storage class/ID, incarnation, fence, owner, authority tuple, expected
digest/size/path, and expected expiry. The transaction then creates/verifies
the location and returns the lease row to `idle` without resetting the fence.

Renewal has the same predicates: exact owner/fence/expected expiry, no
tombstone, and still-live job or artifact authority. It is not a blind TTL
extension. Thus builder, resolver hydration, and repair all share one
create-versus-GC exclusion protocol.

GC claims under the exact existing GC `Lease`, and only when there is no
`complete` location for that storage candidate, running job, or unexpired
publication lease. In the claim transaction it creates an idle publication row
with a fresh incarnation when absent or CAS-invalidates an expired reservation
by incrementing `publication_fence`; it then inserts a tombstone carrying that
invalidated incarnation and fence. The artifact may remain as the expected
digest for a rebuild.

Physical deletion then branches by storage class:

- **Shared:** atomically rename the file into a same-filesystem quarantine,
  revalidate the GC lease, exact tombstone, and idle publication row whose fence
  and incarnation equal the tombstone, then unlink. Failed revalidation
  restores the canonical name when absent or leaves quarantine for the next
  safe sweep.
- **Node-local:** only the `storage_id` holder may proceed. It holds the same
  per-key local publication mutex used by install from before replicated GC
  claim through revalidation. It CAS-deletes the exact sidecar/SQLite row by
  key, stored digest, and trust state, then confirms the exact tombstone/fence.
  A crash after local delete leaves a harmless tombstone until expiry.

A publisher cannot reserve under an active tombstone; after tombstone expiry
its reserve transaction deletes the tombstone and increments the publication
fence, so a delayed GC cannot revalidate. This ordering closes fresh and
expiry-boundary create/delete races for both physical representations even when
callers present skewed timestamps.

The GC lease may compact an idle publication row after seven days only after
authoritative absence of its job, artifact, location, current-key observation,
physical blob, and tombstone. It CAS-deletes the exact incarnation and fence.
A later reservation creates a new random incarnation, so a delayed lease from
the deleted row cannot match even if its numeric fence is reused. When physical
absence cannot be proved, the implementation retains one fixed-size watermark
row for that historical location and reports it separately from live artifact
metadata.

Expired publication leases never authorize orphan adoption. They only delay GC
long enough for an ambiguous fenced completion to settle.

### 5.8 Core store interfaces

The names below are the contract. Implementations may add private helpers but
must not weaken fencing or make callers reproduce SQL invariants.

```rust
#[async_trait]
pub trait FragmentIndexWorkStore: Send + Sync {
    async fn enqueue_fragment_index(
        &self,
        request: NewFragmentIndexJob,
    ) -> Result<EnqueueFragmentIndexOutcome>;

    async fn claim_fragment_index(
        &self,
        voter: &ExactVoterContext,
        capabilities: &FragmentIndexWorkerCapabilities,
        after: Option<&FragmentIndexClaimCursor>,
        now_unix_ms: i64,
        lease_ms: i64,
    ) -> Result<Option<ClaimedFragmentIndexJob>>;

    async fn renew_fragment_index(
        &self,
        claim: &FragmentIndexClaim,
        now_unix_ms: i64,
        lease_ms: i64,
    ) -> Result<bool>;

    async fn yield_fragment_index(
        &self,
        claim: &FragmentIndexClaim,
        not_before_unix_ms: i64,
        reason: FragmentIndexYieldReason,
    ) -> Result<bool>;

    async fn finish_fragment_index(
        &self,
        claim: &FragmentIndexClaim,
        completion: FragmentIndexCompletion,
        now_unix_ms: i64,
    ) -> Result<FinishFragmentIndexOutcome>;

    async fn fail_fragment_index(
        &self,
        claim: &FragmentIndexClaim,
        failure: FragmentIndexFailure,
    ) -> Result<bool>;

    async fn reprioritize_fragment_index(
        &self,
        index_key: &FragmentIndexKey,
        priority: i64,
        now_unix_ms: i64,
    ) -> Result<()>;

    async fn retry_failed_fragment_index(
        &self,
        index_key: &FragmentIndexKey,
        now_unix_ms: i64,
    ) -> Result<bool>;

    async fn requeue_fragment_index_unavailable(
        &self,
        requester: &ExactVoterContext,
        index_key: &FragmentIndexKey,
        now_unix_ms: i64,
    ) -> Result<bool>;

    async fn reserve_fragment_index_builder_publication(
        &self,
        claim: &FragmentIndexClaim,
        publication: PendingFragmentIndexPublication,
        now_unix_ms: i64,
    ) -> Result<Option<FragmentIndexPublicationLease>>;

    async fn reserve_fragment_index_replica_publication(
        &self,
        owner: &ExactVoterContext,
        artifact: &FragmentIndexArtifact,
        publication: PendingFragmentIndexPublication,
        now_unix_ms: i64,
    ) -> Result<Option<FragmentIndexPublicationLease>>;

    async fn renew_fragment_index_publication(
        &self,
        lease: &FragmentIndexPublicationLease,
        now_unix_ms: i64,
    ) -> Result<Option<FragmentIndexPublicationLease>>;

    async fn fragment_index_queue_summary(
        &self,
        now_unix_ms: i64,
    ) -> Result<FragmentIndexQueueSummary>;
}
```

```rust
#[async_trait]
pub trait FragmentIndexArtifactStore: Send + Sync {
    async fn fragment_index_artifact(
        &self,
        key: &FragmentIndexKey,
    ) -> Result<Option<FragmentIndexArtifact>>;

    async fn fragment_index_locations(
        &self,
        key: &FragmentIndexKey,
    ) -> Result<Vec<FragmentIndexLocation>>;

    async fn observe_fragment_index_location(
        &self,
        observer: &ExactVoterContext,
        key: &FragmentIndexKey,
        storage_class: FragmentIndexStorageClass,
        storage_id: &str,
        status: FragmentIndexLocationObservation,
        observed_blob_sha256: Option<[u8; 32]>,
    ) -> Result<()>;

    async fn observe_current_fragment_index_key(
        &self,
        observer: &ExactVoterContext,
        file_id: i64,
        index_key: &FragmentIndexKey,
        source_content_sha256: [u8; 32],
        source_root_sha256: [u8; 32],
        observed_at_ms: i64,
    ) -> Result<()>;

    async fn register_fragment_index_location(
        &self,
        artifact: &FragmentIndexArtifact,
        location: NewFragmentIndexLocation,
        publication: &FragmentIndexPublicationLease,
        now_unix_ms: i64,
    ) -> Result<RegisterLocationOutcome>;

    async fn claim_fragment_index_gc(
        &self,
        lease: &Lease,
        candidate: FragmentIndexGcCandidate,
        now_unix_ms: i64,
    ) -> Result<Option<FragmentIndexGcTombstone>>;

    async fn confirm_fragment_index_gc(
        &self,
        lease: &Lease,
        tombstone: &FragmentIndexGcTombstone,
        now_unix_ms: i64,
    ) -> Result<bool>;

    async fn sweep_superseded_fragment_indexes(
        &self,
        lease: &Lease,
        after: Option<&FragmentIndexRetentionCursor>,
        membership: &CurrentVoterSnapshot,
        observation_fresh_after_ms: i64,
        retire_before_ms: i64,
        now_unix_ms: i64,
        limit: u16,
    ) -> Result<FragmentIndexRetentionPage>;
}
```

`ExactVoterContext` has private fields and is issued only by the store's
membership adapter after it binds the configured local node identity to a
current exact voter and capability epoch. The Hiqlite store owns that adapter;
callers cannot supply an arbitrary observer ID. The standalone backend issues
the sole-node equivalent. Claim, renew/requeue, observation, and
artifact-authorized replica publication derive `node_id` from this context and
revalidate its membership epoch in the write transaction. Claim also verifies
that the supplied capability digest equals the one bound into the context.
Replica reservation additionally requires a node-local `storage_id` to equal
that identity or a shared `storage_id` to be a verified domain in its
capability snapshot. A raw string is never an authority credential.

`finish_fragment_index` follows the existing pretranscode settlement pattern in
one replicated transaction. It first extends the exact claimed lease to a
short successor expiry, then publishes and settles against that exact successor
value. It succeeds only if:

1. The job is running under the supplied owner and fence.
2. The stored lease equals the claim's expected expiry and is later than the
   caller timestamp applied deterministically by every voter.
3. The live library row still matches file ID, size, and mtime.
4. The completion key equals the job key.
5. Every advertised location has the artifact's exact blob digest and size.
6. An existing artifact has identical immutable key inputs, blob digest, blob
   size, fragment count, and timescale; provenance is ignored.
7. Replicated settings still have cluster indexing enabled and
   `playback.vod_index_mins` nonzero.

If the same key already names a different blob digest, the transaction records
typed `index_nondeterministic`, leaves the old artifact untouched, and makes the
new bytes ineligible for serving. This is a page-worthy correctness alert.

Caller timestamps are required because nondeterministic SQL clock functions
cannot run in a replicated state machine. Fence/owner/expected-expiry CAS makes
clock skew a liveness concern rather than a stale-publication safety hole: once
another worker increments the fence, the old completion cannot match.

### 5.9 Local blob interfaces

SQLite v30 and sidecar v7 both add the same local tables. In standalone mode,
SQLite v30 also creates the replicated-logical tables from Sections 5.2–5.7
and 5.10 in the ordinary local database; only the Hiqlite backend splits
metadata from sidecar bytes.

```sql
CREATE TABLE fragment_index_blobs_v2 (
    index_key TEXT PRIMARY KEY
        CHECK(length(index_key) = 64
          AND index_key NOT GLOB '*[^0-9a-f]*'),
    blob_sha256 TEXT NOT NULL
        CHECK(length(blob_sha256) = 64
          AND blob_sha256 NOT GLOB '*[^0-9a-f]*'),
    blob_bytes INTEGER NOT NULL
        CHECK(blob_bytes > 0 AND blob_bytes <= 33554432),
    blob BLOB NOT NULL CHECK(length(blob) = blob_bytes),
    trust_state TEXT NOT NULL
        CHECK(trust_state IN ('verified', 'quarantined')),
    verified_at_ms INTEGER NOT NULL CHECK(verified_at_ms >= 0),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
) STRICT;

CREATE TABLE source_content_digest_memos (
    canonical_path TEXT NOT NULL CHECK(length(canonical_path) BETWEEN 1 AND 4096),
    device_decimal TEXT NOT NULL,
    inode_decimal TEXT NOT NULL,
    source_size INTEGER NOT NULL CHECK(source_size >= 0),
    mtime_unix_ns_decimal TEXT NOT NULL
        CHECK(length(mtime_unix_ns_decimal) BETWEEN 1 AND 40),
    ctime_unix_ns_decimal TEXT NOT NULL
        CHECK(length(ctime_unix_ns_decimal) BETWEEN 1 AND 40),
    digest_contract_version INTEGER NOT NULL CHECK(digest_contract_version > 0),
    content_sha256 TEXT NOT NULL
        CHECK(length(content_sha256) = 64
          AND content_sha256 NOT GLOB '*[^0-9a-f]*'),
    verified_at_ms INTEGER NOT NULL CHECK(verified_at_ms >= 0),
    PRIMARY KEY(
        canonical_path, device_decimal, inode_decimal, source_size,
        mtime_unix_ns_decimal, ctime_unix_ns_decimal,
        digest_contract_version
    )
) STRICT;

CREATE INDEX source_content_digest_memos_age
    ON source_content_digest_memos(verified_at_ms);

CREATE TABLE source_content_digest_jobs (
    digest_job_id TEXT PRIMARY KEY
        CHECK(length(digest_job_id) = 64
          AND digest_job_id NOT GLOB '*[^0-9a-f]*'),
    file_id INTEGER NOT NULL,
    canonical_path TEXT NOT NULL
        CHECK(length(canonical_path) BETWEEN 1 AND 4096),
    device_decimal TEXT NOT NULL,
    inode_decimal TEXT NOT NULL,
    source_size INTEGER NOT NULL CHECK(source_size >= 0),
    mtime_unix_ns_decimal TEXT NOT NULL
        CHECK(length(mtime_unix_ns_decimal) BETWEEN 1 AND 40),
    ctime_unix_ns_decimal TEXT NOT NULL
        CHECK(length(ctime_unix_ns_decimal) BETWEEN 1 AND 40),
    digest_contract_version INTEGER NOT NULL
        CHECK(digest_contract_version > 0),
    state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'failed')),
    priority INTEGER NOT NULL DEFAULT 0,
    run_generation INTEGER NOT NULL DEFAULT 0 CHECK(run_generation >= 0),
    failure_attempts INTEGER NOT NULL DEFAULT 0 CHECK(failure_attempts >= 0),
    not_before_ms INTEGER NOT NULL CHECK(not_before_ms >= 0),
    last_error_code TEXT
        CHECK(last_error_code IS NULL OR length(last_error_code) <= 128),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
    UNIQUE(
        canonical_path, device_decimal, inode_decimal, source_size,
        mtime_unix_ns_decimal, ctime_unix_ns_decimal,
        digest_contract_version
    )
) STRICT;

CREATE INDEX source_content_digest_jobs_due
    ON source_content_digest_jobs(
        state, not_before_ms, priority DESC, created_at_ms, digest_job_id
    );
```

Device and inode are canonical unsigned decimal text so complete platform
`u64` values round trip through SQLite. Nanosecond mtime and ctime are canonical
signed decimal text so Rust `i128` values do not silently narrow to SQLite's
signed 64-bit `INTEGER`; parse or range failure is fail-closed. The store
validates canonical decimal encoding before reads or writes. A conflicting blob
changes `trust_state` to `quarantined`; it does not require an unrepresentable
notion of retaining neither row.

Digest jobs are node-local because each node proves its own physical object.
`digest_job_id` is SHA-256 of the complete object-version tuple and contract
version, so requests can deduplicate before an index key exists. Upsert raises
priority on the one row. Startup changes `running` back to `queued`; a claim
increments `run_generation`, and late helper output must match it. Successful
completion atomically inserts the memo and deletes the job.

At most 4,096 queued/running digest jobs may exist locally. When full, request
traffic returns pending without inserting more; ordered discovery eventually
retries after the queue drains. Failed rows are one-per-object-version
tombstones after three failures and require operator retry or an object-version
change. Old memos/jobs for a file are swept when a current open/`fstat` proves a
different tuple, when the catalog size/mtime proves the recorded tuple is no
longer current, or seven days after authoritative catalog deletion. A missing
or unreadable object whose catalog row still matches is retained fail-closed.
The sweep is bounded to 200 rows behind a durable local cursor.

```rust
#[async_trait]
pub trait FragmentIndexBlobStore: Send + Sync {
    async fn load_verified(
        &self,
        artifact: &FragmentIndexArtifact,
    ) -> Result<Option<ResolvedFragmentIndex>>;

    async fn install(
        &self,
        artifact: &PendingFragmentIndexArtifact,
        blob: &[u8],
    ) -> Result<InstalledFragmentIndexBlob>;

    async fn remove_if_matches(
        &self,
        key: &FragmentIndexKey,
        blob_sha256: &[u8; 32],
    ) -> Result<bool>;
}
```

Both SQLite and Hiqlite deployments use this interface. SQLite stores blobs in
its local v2 table. Hiqlite uses the node-local telemetry sidecar, keeping the
durable replicated database free of blob bytes.

### 5.10 Discovery coordination

Discovery is scoped by node ID and `copy_engine_sha256`. Accessibility is a
node property: one node must never advance a shared cursor past a file that only
another node can open. Per-node discovery also prewarms the full-digest memo
that node needs before it may consume a remote artifact.

```sql
CREATE TABLE fragment_index_discovery_cursors (
    node_id TEXT NOT NULL CHECK(length(node_id) BETWEEN 1 AND 256),
    copy_engine_sha256 TEXT NOT NULL
        CHECK(length(copy_engine_sha256) = 64
          AND copy_engine_sha256 NOT GLOB '*[^0-9a-f]*'),
    after_file_id INTEGER,
    generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
    PRIMARY KEY(node_id, copy_engine_sha256)
) STRICT;
```

```rust
pub struct FragmentIndexDiscoveryCursor {
    pub node_id: String,
    pub copy_engine_sha256: [u8; 32],
    pub after_file_id: Option<i64>,
    pub generation: u64,
    pub updated_at_unix_ms: i64,
}

async fn library_file_page_after(
    &self,
    after_file_id: Option<i64>,
    limit: usize,
) -> Result<Vec<LibraryFileSnapshot>>;

async fn commit_fragment_index_discovery_disposition(
    &self,
    lease: &Lease,
    expected: &FragmentIndexDiscoveryCursor,
    next_file_id: i64,
    disposition: FragmentIndexDiscoveryDisposition,
    now_unix_ms: i64,
) -> Result<AdvanceDiscoveryOutcome>;
```

The SQL query must use `ORDER BY id ASC` and `WHERE id > ?`. At end of scan the
holder atomically increments `generation` and resets `after_file_id`. A generic
coordination lease named
`fragment-index-discovery:<node_id>:<copy_engine_sha256>` permits one active
loop for that node/domain. The existing `ActiveJobLease` heartbeat renews while
the full file digest runs.

Each file, not each 200-row page, is a publication boundary. The disposition is
either an enqueue/upsert request or a typed local skip. The store validates the
exact generic lease and expected cursor, applies the job upsert if present, and
advances the cursor in one transaction. A crash can therefore repeat one file
but cannot skip one. An unreadable local file advances only this node's cursor;
every other node has an independent chance to digest and enqueue it.

## 6. Job lifecycle and scheduling

### 6.1 State machine

```
                         source/key changed
                  +------------------------------+
                  |                              |
                  v                              |
discovery ---> queued --claim/fence--> running --+--> ready
                ^   |                    |  |          |
                |   |                    |  +-------> unsupported
                |   |                    |
                |   +--> cancelled       +----------> failed
                |                            retry       |
                +----------- yield/backoff -------------+
                ^                                       |
                +-- no complete location / operator ----+
```

State meanings are exact:

- `queued`: eligible after `not_before_unix_ms`.
- `running`: owned by one node, one monotone fence, and a live lease.
- `ready`: immutable artifact metadata committed successfully.
- `unsupported`: deterministic terminal result for this exact key.
- `failed`: exhausted operational failure retained as a same-key tombstone.
- `cancelled`: source deleted or administratively stopped.

Disabling indexing leaves queued work queued and yields running work; re-enable
does not require manufacturing replacement rows. `ready -> queued` is allowed
only after the zero-location or sustained all-unavailable proof in Section 5.5.
`failed -> queued` requires operator retry. `unsupported` has no same-key
automatic transition.

A claim changes `queued` to `running`, increments `fence`, increments
`attempts`, and sets owner and expiry in one compare-and-swap statement. Engine,
segment-plan version, and canonical-root digest are SQL predicates before
`LIMIT`; capability filtering may not happen only after a global batch is
loaded.

Within each `(engine, segplan, root)` group, a process-local
`FragmentIndexClaimCursor` records the last `(priority, created_at_ms, job_id)`
examined. The bounded query continues after that tuple and wraps at end. It
advances on claims and local refusals, so an arbitrarily large inaccessible
prefix cannot permanently hide later compatible work. Priority changes reset
that group's cursor. The job fence remains the only publication authority; the
cursor is a liveness optimization and need not be replicated.

### 6.2 Capabilities

```rust
pub struct FragmentIndexWorkerCapabilities {
    pub copy_engine_sha256: [u8; 32],
    pub segplan_versions: BTreeSet<u32>,
    pub canonical_source_roots: BTreeMap<[u8; 32], PathBuf>,
    pub shared_cache_domain_id: Option<String>,
}
```

The worker's node ID is derived from `ExactVoterContext`, not from this public
capability value. The context binds the canonical digest of engine, supported
segment-plan versions, source-root digests, and shared-cache domain; claim
rejects a capability value that does not match that bound digest.

Version 1 requires a configured canonical source root to equal the replicated
root/path; there is no implicit mount mapping in the current media pool.
The map key is SHA-256 of ASCII `plurx-source-root`, `u16(1)`, and the
length-prefixed canonical UTF-8 root. Discovery persists that digest on the job.
Capabilities are a claim prefilter only. After claim, the worker must still
open and fully attest the exact source because mounted, permissioned, and
healthy are runtime properties.

A failed local open is not immediately a job attempt failure. The worker yields
with `source_unavailable_on_node` and records a process-local exclusion for that
job/node pair for 60 seconds. This prevents one unreadable title at the front of
the queue from starving accessible work.

### 6.3 Lease and fence contract

Initial values:

- Lease duration: 30 seconds.
- Renewal interval: 10 seconds.
- Renewal failure grace: none for publication.
- Expired-job requeue scan: every 10 seconds, bounded to 128 rows.

The child process may continue briefly while a renewal request is uncertain,
but the worker must kill it once the lease is known lost or once one complete
renewal interval passes without confirmation. In all cases, publication still
requires the live fence transaction. No filesystem operation is considered a
substitute for that fence.

An expired claim returns to `queued` with its owner cleared and a bounded
backoff. `attempts` counts claims. The mandatory `failure_attempts` column
counts only failures classified retryable. Foreground pressure, local source
unavailability, lease loss, and permit loss do not increment it. This keeps
operational claim churn separate from retry exhaustion.

### 6.4 Retry classification

Errors are stored as bounded typed codes plus optional diagnostic detail. The
detail is limited to 1,024 UTF-8 bytes and is never used as identity or control
flow.

| Code | Class | Next action |
| --- | --- | --- |
| `foreground_preempted` | Yield | Queue after 60 s; no failure count. |
| `source_unavailable_on_node` | Yield | Try another compatible node. |
| `source_changed` | Cancel key | Discovery creates the new key. |
| `source_read_error` | Retryable | Exponential backoff, capped at 1 h. |
| `ffmpeg_spawn_failed` | Retryable | Backoff; surface node health. |
| `ffmpeg_stalled` | Retryable | Kill child and back off. |
| `ffmpeg_failed` | Retryable | Retry at most three failure attempts. |
| `unsupported_container` | Unsupported | Terminal for exact key. |
| `no_video_stream` | Unsupported | Terminal for exact key. |
| `invalid_fragment_output` | Unsupported | Terminal unless classified bug. |
| `index_blob_too_large` | Unsupported | Terminal for current contract. |
| `local_corrupt_orphan` | Yield | Prove corruption, quarantine/GC, retry. |
| `index_nondeterministic` | Correctness fault | Quarantine and alert. |

Retry backoff starts at 60 seconds, doubles with deterministic job-ID jitter,
and caps at one hour. After three counted failures, the one row for the key
remains `failed`. A later VOD miss observes that tombstone and cannot create a
fresh attempt. Only the explicit operator-retry CAS resets the same row, or an
identity/contract change creates a different key.

The exact `Unsupported` variants in `fragindex.rs` map to stable codes at the
worker boundary. Free-form FFmpeg text is diagnostic material, not a terminal
classification by itself.

### 6.5 Discovery loop

`playback.vod_index_mins` remains the operator kill switch and cold rescan
interval:

- `0`: cancel/yield current index work and do not discover or build.
- Positive: run a complete paginated generation at that interval.

The active queue is not polled only at that cadence. While indexing is enabled,
each node runs a lightweight worker loop that wakes on:

- local completion or yield;
- a bounded 2-second idle timer;
- a VOD miss notification;
- a replicated queue-change notification when available.

Each query page contains at most 200 files, but each file's enqueue/skip and
cursor advance commit atomically. The lease heartbeat runs throughout hashing
and cursor publication. The discoverer derives recipes and full source digests;
it does not wait for index builds. A cache-missing full hash must acquire one of
the same global media-pass permits as an FFmpeg index build. Memo hits need no
permit. Hashing uses at most one process-local task at a time.

### 6.6 VOD miss fast path

When session creation computes a key with no artifact or terminal negative, it
calls `enqueue_fragment_index` and then `reprioritize_fragment_index` with the
fixed `interactive_miss` priority. This signal wakes workers but still respects
`playback.vod_index_mins = 0`; disabling indexing is an operator decision, not
an invitation for request traffic to restart it.

The request does not wait for a build. It returns the existing structured
`503 vod_index_pending` response after the bounded resolver tiers have failed.
The response may include a server-provided retry delay, but not node IDs,
paths, keys, or internal failure detail.

### 6.7 Cluster concurrency

A new setting controls all full-source background passes, including first-time
content hashing and FFmpeg index generation:

```text
playback.vod_index_cluster_workers = 2
valid range: 1..=8
```

Workers acquire an existing generic fenced coordination permit before claiming
a job.
Permit slots are named `fragment-index-worker:0` through `N-1` and have the
existing 90-second generic lease. Lowering the setting stops new claims;
holders above the new limit finish or yield normally. The setting is read from
the replicated configuration so all nodes agree on the limit.

The permit uses the existing 90-second `ActiveJobLease` TTL and 30-second
heartbeat. Its loss token is selected alongside job-lease loss, foreground
pressure, child progress, and child exit. Permit loss terminates the child and
fenced-yields the job, so an expired slot cannot silently exceed the global I/O
bound.

Per-node concurrency is one for the first release. This makes the global value
the only parallelism knob and bounds child processes even if membership churns.
The fence on the job, not the permit, remains the publication authority.

### 6.8 Foreground and I/O pressure

Indexing shares source storage and CPU with playback even though stream copy is
not a full encode. A new `background_media_io_allowed()` decision wraps the
existing admission signals and is false when any of these is true:

- A live request is waiting for admission.
- An offline transcode is waiting or running locally.
- Software encoding capacity is in use.
- The local source/storage health monitor reports elevated latency or errors.
- The daemon is draining or shutting down.

The worker and full-digest task check this before acquiring a permit. While
running, they select on permit/lease renewal, progress, and a foreground-change
notification. The FFmpeg worker also selects on child exit and stall timeout.
Foreground pressure sends graceful termination and escalates to kill after two
seconds, then fenced-yields the job with `foreground_preempted`.

A preempted digest task closes its descriptor, releases its permit, and leaves
the per-node cursor unchanged. It restarts that file later; no partial SHA-256
state is persisted in v1.

### 6.9 Progress supervision

All duration-derived total deadlines are removed. The FFmpeg parser reports
progress only after either a complete fragment is accepted or at least 1 MiB
of additional syntactically valid parser input has arrived since the previous
progress event. A 90-second timer is reset on each such event. The source-digest
helper uses the same rule with 16 MiB of newly hashed source data as its
progress quantum. Bytes below a quantum accumulate, but a trickle cannot renew
the stall timer forever.

If the timer expires:

1. Capture bounded stderr diagnostics.
2. Terminate and then kill the child within two seconds.
3. Re-attest the source for diagnostic classification.
4. Mark retryable `ffmpeg_stalled`, unless the source changed.

A healthy multi-hour index is permitted. The existing 120-second maintenance
window becomes a "start no new work after" budget, not a deadline for a job
already making progress. This prevents periodic restart from destroying all
forward work.

Every child launcher keeps `kill_on_drop(true)` in addition to the explicit
terminate/kill path. Lease or permit loss, task cancellation, panic, and daemon
shutdown therefore cannot detach an unsupervised media reader. A deliberately
trickling child that never reaches a progress quantum is killed by the same
90-second stall path.

Normal publication requires all three independent completion signals: the
fragment parser reached a valid terminal boundary, stdout reached EOF, and
`wait()` returned a zero child exit status. A nonzero exit after an apparently
complete final fragment is `ffmpeg_failed` and its rows are discarded.

## 7. Build, publication, and resolution

### 7.1 Builder sequence

The worker performs the following sequence for a claimed job:

1. Confirm indexing remains enabled and a cluster permit is held.
2. Resolve the replicated path beneath an identical canonical library root.
3. Open or memo-attest the source. Require claimed size, mtime, and full digest.
4. Reconstruct the typed file recipe and require its pipeline digest and key.
5. If an authoritative artifact already exists, accept a local/shared blob only
   when its digest matches that artifact; uncataloged bytes are never adopted.
6. Run one supervised FFmpeg copy-index process against inherited descriptor 3.
7. Require parser completion, stdout EOF, and a successful child exit status.
8. Validate index semantics using the same checks required by VOD serving.
9. Recheck the still-open source object tuple and reject any change.
10. Encode the deterministic blob and compute its SHA-256.
11. Fenced-reserve the local publication and, when verified, the shared one.
12. Atomically install the local blob.
13. If the shared cache is verified, atomically publish the shared blob.
14. Commit artifact, locations, and job readiness through the live fence.

Step 5 allows recovery only when the replicated artifact supplies the trusted
expected digest. A local or shared file left before first fenced completion is
an unauthoritative orphan and must be rebuilt or garbage-collected, even when
its header key and structure look valid.

### 7.2 Local installation

The local cache key is the 64-character lowercase index key. SQLite uses a
`fragment_index_blobs_v2` table with `index_key`, `blob_sha256`, `blob`, and
local verification timestamps. The Hiqlite sidecar uses the same logical
schema.

Installation is one local transaction:

The caller first acquires the per-key local publication mutex, then the
replicated publication lease, and holds both through local commit and fenced
location registration. GC takes them in the same order, preventing lock-order
deadlock.

1. Decode and validate the blob before the write.
2. Insert if absent.
3. If present with the authoritative digest and bytes, report
   `AlreadyPresent`.
4. If an artifact exists and the incoming blob matches it, atomically replace
   any mismatching/quarantined local row and report repaired corruption.
5. If the incoming blob differs from an existing artifact, do not install it;
   report `index_nondeterministic` and retain the authoritative row if valid.
6. If no artifact exists and an older uncataloged row conflicts with the live
   first build, verify the old row against its own stored digest and the bounded
   decoder. If it is internally valid, two complete builds of one exact key
   differ: retain the old evidence, record both digest prefixes, and fail
   terminal `index_nondeterministic` before publishing either. If the old row
   fails its own digest or decode, corruption is proved; mark it `quarantined`,
   yield `local_corrupt_orphan`, and remove it under an immediate GC tombstone
   before retry.

The 32 MiB decoder cap applies before allocation and before SQL binding.
`quarantined` rows are never local hits or peer responses, and they cannot block
repair forever: known artifact authority may replace them immediately; proven
corrupt orphans bypass the seven-day grace and enter the next bounded local GC
page before the job's retry delay expires. A structurally valid conflicting
orphan is nondeterminism evidence and is never auto-deleted to make a preferred
answer win.

### 7.3 Shared publication

The shared path is derived, not caller-controlled:

```text
.fragment-index/v1/<first-two-key-chars>/<index-key>.idx
```

Publication reuses the shared-cache secure-directory and verification model:

1. Require a currently verified cache domain and canary.
2. Acquire the exact key, digest, size, storage ID, and derived path in
   `fragment_index_publication_leases`; reject an active GC tombstone.
3. Create a same-directory random temporary file with create-new semantics.
4. Stream bytes while hashing; never hold an additional unbounded copy.
5. `fsync` the file, verify length and digest, and rename without replacement.
6. If the destination exists, open it safely. Accept an exact reserved digest.
   With an existing artifact, its digest decides which copy is authoritative.
   Without an artifact, a different but internally digest/decoder-valid blob
   for the same key is terminal `index_nondeterministic`; it is retained as
   evidence and the new output is not published. Only a destination that fails
   its own bounded decode/header checks may be quarantined and replaced under
   the live publication lease.
7. `fsync` the parent directory where the platform supports it.
8. Return a complete location record only after all checks pass.

The artifact is content-addressed, so publication does not need a mutable
generation pointer. It does require the reservation/tombstone ordering in
Section 5.7. It also needs the current mount-loss behavior: a failed canary
disables the shared fast path immediately and cannot make local publication
fail.

Symlinks, `..`, unexpected file types, and paths outside the verified root are
rejected. A proven-corrupt conflicting destination may be quarantined and
replaced under the live publication lease. An internally valid conflicting
destination is retained as nondeterminism evidence and is never overwritten
automatically.

### 7.4 Fenced completion

The worker submits a completion containing the encoded artifact metadata and
one or more complete locations. The store transaction verifies Section 5.8 and
then:

1. Inserts or verifies the immutable artifact row.
2. Inserts or verifies each complete location row.
3. Clears prior unhealthy observations for each republished location and writes
   the completing node's `healthy` observation.
4. Returns every presented publication lease row to `idle` without resetting
   its monotone fence.
5. Marks the job `ready`, clears lease ownership, and sets completion time.

A worker that loses its lease can leave a local or shared orphan, but cannot
make it authoritative. It may be reused only if an artifact with the matching
digest already exists; otherwise a future claimant rebuilds. Garbage collection
removes unreferenced old blobs later, so lease loss never requires unsafe
rollback of another worker's filesystem operation.

### 7.5 Resolver contract

```rust
pub enum FragmentIndexResolveOutcome {
    Ready(ResolvedFragmentIndex),
    Pending { retry_after: Duration },
    Unsupported { code: FragmentIndexUnsupportedCode },
    Failed { code: FragmentIndexFailureCode },
}

pub async fn resolve_fragment_index(
    context: &FragmentIndexResolveContext,
    source: AttestedMediaSource,
    recipe: &CopyVideoRecipe,
) -> Result<FragmentIndexResolveOutcome>;
```

Before calling the resolver, session creation opens the source and validates a
full-digest memo for its current object version. If no memo exists, it schedules
digest work and returns pending. If the one local digest job is an exhausted
`failed` tombstone, it returns typed `vod_source_digest_failed` without
reenqueueing. The resolver owns that held file descriptor and transfers it to
the VOD generation context on success.

The resolver computes the key once, then:

1. Loads the authoritative artifact and bounded, healthy-first locations.
2. Loads a local v2 blob and verifies header, digest, and semantics against the
   artifact. Local bytes without an artifact are not v2 cache hits.
3. Tries the verified shared location with a 750 ms total timeout.
4. Tries one healthy local-holder peer with a 1.5 s total timeout.
5. Acquire an artifact-authorized local publication lease, install a valid
   remote blob, and synchronously register the location under that exact lease
   before returning it as a durable local hit.
6. Records a per-node corrupt, missing, or unreachable observation
   asynchronously; one node never globally poisons a shared location.
7. If an artifact has no complete location, fenced-requeue its existing ready
   job. If complete rows exist but none worked, schedule a bounded background
   probe across all canonical locations; after the Section 5.5 sustained
   all-unavailable proof, fenced-requeue without retiring those rows. Otherwise
   enqueue/reprioritize the one row, or return its durable
   `Unsupported`/`Failed` tombstone without mutating it. Return `Pending` only
   for work that is queued or running.

Remote tiers are attempted only after the local full source digest matches the
artifact key. A remote artifact cannot cause the node to skip proving that it
sees the same media bytes. `AttestedMediaSource` remains held through generated
VOD reads so a path replacement cannot swap the served source afterward.

The resolver never waits for multiple peers serially during session creation.
A later miss may select a different healthy holder. A background repair task
may probe unhealthy observations outside the request path.

### 7.6 Internal peer endpoint

Add a cluster-only route:

```http
GET /internal/v1/media/fragment-index/{index_key}
```

It uses the exact-voter HMAC authorization and replay protections in
`PeerTransport`; normal user authentication is not sufficient. The transport
API is extended from static paths to a bounded canonical path type and signs
the complete concrete path, including the lowercase index key. A signature for
key A is invalid for key B. The handler:

- accepts only 64 lowercase hexadecimal characters;
- requires that the local complete blob matches the replicated artifact;
- returns at most 32 MiB with a fixed content length;
- streams from the local blob store rather than collecting a second copy;
- sends `ETag: "<blob_sha256>"` and `Cache-Control: private, no-store`;
- returns `404` for absent or untrusted bytes;
- returns `409` if local bytes conflict with the catalog;
- exposes no path or source metadata.

The client sets connect, header, body-idle, total, and decoded-body limits. A
valid content length above 32 MiB is rejected before reading. Chunked bodies
are counted and aborted at the same cap.

### 7.7 Rendition-plan identity

New rendition keys include the 32-byte fragment index key. This prevents a
node-local plan created from one index from surviving a source, segment-plan,
FFmpeg, or Plurx copy-contract change.

Legacy rendition keys and plans remain readable only for already-established
legacy sessions during the rollout window. New sessions use v2 identity as soon
as a v2 artifact is available. The rollout must not reinterpret an old key as a
v2 key.

## 8. Failure model

### 8.1 Worker crashes

The lease expires and another compatible node claims the job with a higher
fence. A complete physical file is reusable only when an existing artifact has
the same digest. Uncataloged bytes are GC candidates, not evidence. Partial
local SQL transactions roll back; partial shared temporary files are never
cataloged.

### 8.2 Network partition

A minority-side worker may finish physical bytes but cannot complete the
replicated fence transaction. It must not advertise success locally to VOD
serving unless a matching authoritative artifact already exists. Once the
partition heals, bytes with an authoritative expected digest may be reused;
other orphans are removed.

### 8.3 Shared mount failure

Canary failure disables shared reads and writes. Local completion remains
valid, and the catalog advertises the builder as a node-local holder. Consumers
use peers. When the mount recovers and is reverified, a repair task may copy
authoritative local blobs into it.

### 8.4 Holder failure

Peer selection filters current exact-voter membership and recent health. If all
local holders are unavailable and no valid shared copy exists, the resolver
records per-node observations. A background probe covers every canonical
location; three failures spanning 30 seconds let that requester
fenced-requeue the one job row without retiring locations. At-least-one rebuild
is acceptable for availability; using unverified bytes is not.

### 8.5 Source changes during build

The full content digest is established before work, and the held descriptor's
object tuple is checked before and after each FFmpeg run. A change kills or
discards the attempt, cancels the old exact key, and lets discovery compute the
new full digest and key. No artifact or unsupported negative from the old source
attaches to the new source.

### 8.6 Mixed-version cluster

New schema rollout follows the repository's existing cluster upgrade contract.
Old daemons do not understand the new queue or peer route, so they neither
claim nor serve v2 work. New daemons advertise a fragment-index protocol
capability and select only capable holders.

Schema v12 is additive, but current binaries migrate automatically in
`open_or_migrate` and older binaries require exact schema compatibility at
restart. During the supported rolling window:

- Old nodes continue serving their node-local legacy indexes.
- New nodes can also serve a valid legacy local index as a temporary fallback.
- No legacy index is placed in the replicated catalog or sent to a peer.
- New v2 jobs are claimed only by nodes advertising protocol version `1`.
- The first upgraded voter performs the additive migration automatically.
- Already-running old voters may continue applying the additive schema, but
  they cannot be restarted on schema v12 with an old binary.
- The feature flag stays false, so no new-table workload begins until every
  voter and learner runs the capable binary.
- Learners are upgraded after migration before they are allowed to rejoin
  normal operation.
- Restart drills at every boundary prove the actual compatibility contract.

There is no manual "migrate after all upgrades" step and no old-binary rollback
after the first migration. Rollback from that point restores the pre-migration
database snapshot or keeps the new compatible binary with the feature disabled.

### 8.7 Nondeterminism

Two complete builds of one exact key are expected to have identical encoded
blob digests. A mismatch can indicate an incomplete pipeline identity,
nondeterministic FFmpeg output, a Plurx bug, or corruption.

On mismatch:

1. Preserve the first authoritative artifact.
2. Quarantine the new local/shared bytes from serving.
3. Mark the job failed with `index_nondeterministic`.
4. Emit an error event containing key prefix, both digest prefixes, engine
   digest prefix, and node IDs.
5. Stop automatic retries for that key.

The event uses short prefixes for diagnosis; metrics do not use them as labels.

## 9. Garbage collection and repair

### 9.1 Local sweep

Each node performs a bounded local sweep after startup and daily:

1. Read at most 200 local v2 rows after a durable local cursor.
2. Keep rows named by a current artifact or a running local claim.
3. Keep unreferenced rows younger than seven days for ambiguous completion.
4. Exception: proven `quarantined` corruption is eligible immediately after no
   matching live publication lease remains; valid nondeterminism evidence is
   retained for operator inspection.
5. Delete other older unreferenced rows in a local transaction.
6. Coordinate deletion with the local publication mutex and replicated
   tombstone when an advertisement or reservation could race.
7. Record local location observations for missing or corrupt rows.

The sweep never deletes legacy rows in the first release.

### 9.2 Shared sweep

One holder of the existing shared-cache GC lease walks
`.fragment-index/v1` in bounded pages. It may claim a GC tombstone only when:

- the filename and parent shard are canonical;
- the file is at least seven days old;
- an authoritative catalog lookup succeeds;
- no complete location references that exact storage class/ID;
- no running job for the key exists; and
- no live publication lease exists.

After the replicated claim, GC uses the quarantine, revalidation, and unlink
sequence in Section 5.7. Catalog timeout, lease loss, or unavailable
revalidation means keep/quarantine, never blind-delete. Temporary publish files
older than one day use the same canonical path/type checks and must not match a
live publication lease.

### 9.3 Repair

A low-priority repair loop examines bounded unhealthy location observations. It
may:

- verify and restore a shared location;
- hydrate a missing shared copy from a valid local holder;
- retire a node-local advertisement when its holder confirms absence;
- retire a shared location only after the independent-observation threshold;
  or
- requeue a key when no complete location remains.

Every repair copy acquires an artifact-authorized publication lease for its
destination and registers under that exact lease. It never calls the local blob
store or shared publisher outside the Section 5.7 exclusion protocol.

Repair obeys the same foreground pressure signal and cluster worker permits
when it requires media reconstruction. Copying an existing sub-100-KiB blob
does not consume a builder permit, but is still rate-limited.

### 9.4 Supersession and metadata retention

Updating `files` in place must not leave one artifact and job forever for every
historical source, engine, pipeline, segment-plan, or indexer-contract version.
The current-key observation table supplies bounded, positive evidence that an
old key is no longer current. Retention runs under the existing cluster GC
lease, scans at most 200 artifacts after a durable cursor, and uses the
`sweep_superseded_fragment_indexes` store transaction.

For each artifact, the transaction derives the eligible observer set from a
`CurrentVoterSnapshot` issued by the membership coordinator and the current
voters that advertise access to the artifact's canonical source root. The
snapshot contains the membership epoch, voter IDs, and capability digest; the
store revalidates that epoch and digest at commit and retries the page if
membership changed. Standalone mode supplies the one-node equivalent. An
observation is fresh when it is newer than
`max(2 * playback.vod_index_mins, 48 hours)`. The decision is deliberately
fail-closed:

1. If any eligible voter has a fresh observation equal to the artifact key,
   clear `superseded_at_ms`.
2. If an eligible voter is missing, stale, inaccessible, or no eligible voter
   exists, retain the artifact and emit a bounded retention reason.
3. Only complete fresh coverage in which no eligible voter reports the key may
   set `superseded_at_ms`, using the caller timestamp in a deterministic CAS.
4. If complete fresh coverage still excludes the key after a seven-day grace,
   the same transaction may retire it only when its job is `ready` or terminal,
   no build or publication lease is live, and no GC tombstone is active.
5. The fenced transaction deletes the old job and artifact; locations and
   location observations cascade. Physical bytes then become unreferenced
   candidates for Sections 9.1 and 9.2. The idle publication row is retained so
   its monotone fence is not immediately reset; Section 5.7 may later compact
   it safely through the incarnation-based ABA guard.

`fragment_index_key_observations` remains bounded at one row per `(file_id,
node_id)`. A key change updates `key_changed_at_ms`; an identical observation
refreshes `observed_at_ms` at most daily. Removed members' observations are
pruned. A stale observation from a node that remains a voter blocks retirement
and alerts after twice the freshness window; removing that voter through the
normal membership procedure is the explicit recovery path.

Mixed-version or storage-divergent observations also fail safely. If any
current voter still reports an old key, that artifact remains live. The
observation carries `source_content_sha256` separately from the composite key;
if current voters report different full source digests for what should be one
canonical file, every matching artifact is retained and a storage-divergence
alert is raised. A session that resolved an artifact before its metadata is retired
continues from its owned decoded index and stable source handle; new resolution
cannot discover retired metadata.

With healthy membership, live artifact/job/location metadata is therefore
bounded by the set of currently observed variants plus the seven-day
supersession grace. Publication watermarks compact after separately proven
physical absence; an unprovable orphan may retain one fixed-size watermark row.
A network partition or unavailable voter prefers leaked cache metadata over
deleting the last usable version.

## 10. Observability and operations

### 10.1 Metrics

Add fixed-cardinality metrics:

```text
plurx_fragment_index_jobs{state}
plurx_fragment_index_oldest_queued_seconds
plurx_fragment_index_builds_total{outcome}
plurx_fragment_index_build_seconds
plurx_fragment_index_source_bytes_total
plurx_fragment_index_cpu_seconds_total{component}
plurx_fragment_index_fragments_total
plurx_fragment_index_preemptions_total{reason}
plurx_fragment_index_resolves_total{tier,outcome}
plurx_fragment_index_hydration_bytes_total{tier}
plurx_fragment_index_location_faults_total{class}
plurx_fragment_index_nondeterminism_total
plurx_fragment_index_supersession_total{outcome}
plurx_fragment_index_active_workers
plurx_source_digest_jobs{state}
plurx_source_digest_seconds
plurx_source_digest_bytes_total{outcome}
```

`state`, `outcome`, `reason`, `tier`, `class`, and `component` are closed enums;
`component` is limited to `ffmpeg`, `digest_helper`, and `daemon`. File IDs,
paths, node IDs, keys, and digests are forbidden metric labels.

### 10.2 Structured events

Build lifecycle events include job ID, file ID, key prefix, owner, fence,
attempt, elapsed time, source bytes, fragment count, and typed result. Paths and
full digests are logged only at debug level. Peer failures include the peer node
and typed transport class, never HMAC or request headers.

### 10.3 System status

The existing system/settings surface reports:

- queued, running, ready, unsupported, and failed counts;
- this node's queued/running/failed source-digest counts;
- age of oldest queued job;
- current global permit limit and active permits;
- this node's engine digest prefix and compatibility-group size;
- this node's current job and progress;
- local/shared/peer resolver hit counts;
- verified shared-cache status; and
- most recent nondeterminism fault, if any.

The UI exposes the global worker setting with `1`, `2`, `4`, and `8` presets.
It keeps the existing `vod_index_mins` presets and explains that zero disables
miss-triggered indexing as well as scheduled discovery.

### 10.4 Operator controls

The initial implementation needs safe controls to:

- list aggregate job state;
- inspect one job by ID with typed failure detail;
- retry one failed job;
- retry one failed local source-digest job;
- cancel one queued or running job through the fence;
- force a new discovery generation; and
- verify one artifact's locations without downloading source media.

These may begin as authenticated system endpoints or CLI-backed store methods.
They must not expose blob contents publicly or allow arbitrary cache paths.

### 10.5 Alerts

Recommended operational alerts:

- any `index_nondeterministic` result: page;
- oldest queued job above one discovery interval: warn;
- no compatible worker for queued work for 15 minutes: warn;
- failed jobs above 1% of attempted jobs over one hour: warn;
- shared-cache location failures while the shared domain is verified: warn;
- repeated foreground preemption: informational capacity signal;
- stale current-key observations blocking retirement: warn; and
- divergent current source keys for one canonical file: page.

## 11. Security and resource bounds

### 11.1 Trust boundary

Fragment indexes determine exact byte accounting and film position. They are
trusted control data once accepted, even though they contain no media payload.
Accordingly, every remote byte crosses the same exact-voter authentication
boundary as internal media transport and remains subject to independent digest
and structural verification.

### 11.2 Required protections

- Exact-voter HMAC authentication, timestamp checking, and replay protection.
- Strict lowercase-hex key parsing before store or filesystem access.
- Derived paths only; no user-provided relative path is opened.
- No symlink traversal or non-regular shared-cache files.
- 32 MiB body cap enforced before and during streaming.
- Checked arithmetic for row counts and byte lengths.
- Bounded diagnostic text and stderr capture.
- Bounded claim scans, discovery pages, location lists, and GC pages.
- Per-node one-process worker limit and cluster-wide permit limit.
- Constant-time MAC comparison through the existing peer transport.
- In-place source mutation cannot expose an initialization object or segment
  under the old index key.
- `private, no-store` on the peer response.

### 11.3 Denial-of-service behavior

A client VOD miss may raise an existing job's priority but cannot select a node,
increase the cluster limit, bypass the disable switch, or create more than one
active index job for a key. Before the key is known, misses collapse into one
bounded local digest job for the exact open/fstat object version.

Source attestation and recipe construction are bounded before enqueue.
Artifact lookup
limits returned locations to a fixed maximum, initially 16; extra stale
locations are left for repair instead of expanding request work.

## 12. Rollout and compatibility

### 12.1 Feature gates

Introduce one temporary rollout flag:

```text
playback.vod_index_cluster_cache = false
```

The flag defaults false until schema migration, queue tests, and cluster
harness coverage ship together. It is replicated. When false, current local
indexing and serving continue unchanged. When true, all new writes use v2; a
valid legacy local row remains read-only fallback until the v2 key is ready.

The flag also remains false by default for standalone installations: exact v2
identity adds a full hash pass but cannot eliminate another node's build when
there is no other node. Standalone mode must still pass correctness tests and
may be enabled explicitly. Removing the flag requires a later review showing
net benefit for every automatically enabled topology, or a topology-aware
replacement gate. The existing `vod_index_mins` remains the permanent
enable/disable control.

### 12.2 Backfill sequence

1. Snapshot the database under the existing upgrade procedure and leave the
   feature flag false.
2. Upgrade one voter. Its `open_or_migrate` performs additive schema v12; verify
   schema digest parity while the remaining old voters stay running.
3. Upgrade remaining voters, then learners. Never restart an old binary after
   the schema bump.
4. Verify every member advertises fragment-index protocol version `1` and run
   the documented rolling-boundary restart checks.
5. Record an M0 `go` result for this node count and storage class. A `no-go`
   leaves the feature false without blocking the binary/schema rollout.
6. Enable the feature with one global worker.
7. Confirm local builds, fenced metadata, resolver tiers, and foreground yield.
8. Raise the worker limit to two and observe NAS latency and queue age.
9. Let paginated per-node discovery backfill all current library files.
10. Remove legacy fallback only in a later release after readiness and rollback
   data show it is safe.

Backfill priority is below an interactive miss. It is restart-safe because the
cursor, one-row-per-key uniqueness, and immutable artifact identity are durable.

### 12.3 Rollback

Disabling `playback.vod_index_cluster_cache` stops new v2 claims and restores
the legacy local read/write path while the compatible binary remains installed.
It does not drop schema or delete v2 blobs. In-flight v2 workers observe the
flag, kill/yield, and cannot complete without their live fence.

Rolling back to a binary that predates schema version 12 requires restoring the
pre-migration database snapshot; old binaries cannot open the newer exact
schema. Operators preserve that snapshot, the live database, and local
sidecars until the rollback decision is closed.

### 12.4 Legacy data

Legacy `fragment_indexes` rows cannot be safely promoted because they lack the
copy-engine/dependency digest and full source-content digest. They remain
node-local and may be
used only by that node's legacy resolver during transition. The backfill creates
new v2 rows from source; there is no synthetic metadata upgrade.

The first release does not dual-write legacy indexes. Dual writes would double
local state and imply rollback compatibility that the schema policy may not
support. If release engineering later requires dual write, it must be a
separate explicit compatibility decision with tests.

### 12.5 SQLite-to-Hiqlite activation

Cluster activation must not import authoritative v2 locations while creating
an empty telemetry sidecar. The activation sequence is:

1. Quiesce standalone indexing under the existing activation fence; convert any
   `running` job to `queued` with owner/lease cleared and discard expired
   publication reservations and GC tombstones.
2. Open the standalone backup read-only and validate every `verified` local v2
   blob against its artifact digest and bounded decoder.
3. Create a temporary sidecar at schema v7 and copy valid v2 blobs and
   full-digest memos into it. A memo is valid only when a current open/fstat
   still matches its complete object-version tuple. Invalid rows are recorded
   for reconciliation.
4. `fsync` and atomically install that sidecar at the exact telemetry path
   **before** constructing or calling `HiqliteAuthStore::bootstrap`. Bootstrap
   must open the installed v7 sidecar rather than create an empty one. Failure
   leaves the standalone source untouched.
5. Bootstrap the Hiqlite store, then import artifacts, jobs, locations,
   observations, and discovery cursors
   according to their replication class. Do not carry quiesced ephemeral
   reservations or tombstones into the new cluster.
6. Import a node-local location only when its validated blob was copied. Keep
   the artifact but atomically transition a `ready` job to `queued` when no
   valid imported location remains. Import a shared location only after the
   activation process verifies its cache domain canary, canonical path, size,
   digest, and bounded decode.
7. Continue excluding legacy `fragment_indexes`; they still lack v2 identity.

Sidecar-first ordering can leave safe unreferenced bytes if replicated import
fails. Metadata-first ordering could leave a ready artifact pointing at bytes
that do not exist and is forbidden.

## 13. Implementation milestones

Each milestone is independently reviewable and has runnable acceptance checks.
Later milestones must not weaken invariants established earlier.

### 13.1 M0 — Measurements and fault fixtures

Before changing architecture:

- Add timing/progress instrumentation to the current indexer.
- Capture index blob sizes, build rates, total source bytes read per title, NAS
  read throughput, VOD request latency, and aggregate user/system CPU without
  high-cardinality labels. CPU accounting includes every FFmpeg index child,
  every source-digest helper, and the daemon work attributable to discovery,
  parsing, encoding, verification, and publication.
- Add fixtures for a build beyond its adaptive total budget, more than 200
  library files, unsupported media, path replacement, in-place source mutation,
  and two simulated engine digests.
- Record current single-node VOD behavior for comparison.

M0 is a per-topology stop/go gate, not an informational exercise. Let `N` be
the number of nodes that attest and consume a title of size `S`. The clean
current baseline is approximately `N * S` source bytes; the conservative v2
model is `(N + 1) * S` because every consumer hashes once and one builder runs
FFmpeg once. The theoretical amplification is therefore `(N + 1) / N`: 2.0x
for one node, 1.5x for two, 1.33x for three, and it approaches 1.0x only as the
cluster grows. The plan must never conceal that cost behind a flat threshold.

On representative local and shared-NAS libraries, the prototype passes its
deployment class only when all of these are true:

- measured source bytes stay within 10% of the topology's `(N + 1) * S` model;
- p95 full-library readiness improves by at least 20% versus `N` independent
  indexers, or total copy-index CPU time falls by at least 30%;
- p95 new-session latency regresses by no more than 10%; and
- foreground-read latency remains within the existing storage-health
  threshold.

The one-node class is an expected rollout `no-go` unless measurement shows an
independent benefit; it keeps the legacy path by default while still exercising
v2 in correctness tests. A multi-node class that misses the readiness/CPU or
latency thresholds also keeps v2 disabled. M1 may proceed after at least one
target multi-node deployment class records `go`; otherwise return to design
review for a storage-authoritative content version, a trusted shared digest
service, or a single-pass exact indexer. Sparse sampling is never an acceptable
performance workaround.

Acceptance:

```bash
make unit
make check
```

The new tests must fail against current behavior for cross-node duplicate work,
total-budget restart, unsupported reattempt, and pathname TOCTOU. The current
ordered wrapping cursor is retained as a baseline, not described as broken.

### 13.2 M1 — Identity and deterministic codec

Implement:

- `FragmentIndexKeyInput` and canonical digest encoding.
- Full copy-engine digest and `INDEX_PIPE_CONTRACT_VERSION`.
- Typed recipe construction shared by command and digest.
- Full source-content digest memos and held-descriptor mutation checks.
- Bounded node-local, object-version-keyed digest jobs and killable helper.
- Inherited-descriptor FFmpeg input for indexing and VOD generation.
- `INDEXER_CONTRACT_VERSION` for parser, validation, and negative policy.
- Deterministic, bounded `FragmentIndexBlob` codec.
- New rendition-key inclusion.

Acceptance tests prove:

- Golden key and blob vectors are stable.
- Every output-affecting field changes the key.
- Path-only variation does not change pipeline identity.
- Engine bytes or full version output changes the key.
- Live executable/dependency replacement withdraws capability; old-digest work
  cannot spawn or publish.
- Any source byte, length, mtime, or digest-contract change changes the key.
- Parser/negative-contract changes invalidate terminal rows.
- Replacing the source path after attestation does not change FFmpeg's input.
- Mutating the held inode after init/segment buffering but before publication
  returns `vod_source_changed`, kills the child, and commits/notifies no bytes.
- Repeated pre-key VOD misses create one digest job and one helper at most.
- Oversized, truncated, trailing, overflowing, or mismatched blobs fail closed.
- Encode/decode round trips existing index semantics.

Commands:

```bash
make unit
make fmt-check
make lint
```

### 13.3 M2 — Durable queue and catalog

Implement migrations and both store backends for:

- jobs, artifacts, canonical locations, location/current-key observations, and
  exact-voter observation contexts;
- publication reservations, incarnation watermarks, and GC tombstones;
- ordered per-node discovery cursor and fenced disposition;
- exact capability-aware claim;
- renew, yield, retry, unsupported, cancel, and fenced completion;
- source-deletion and digest-memo/job cleanup;
- bounded supersession scans and watermark compaction; and
- aggregate status.

Acceptance tests prove:

- Concurrent enqueue produces one active job.
- Concurrent claim produces one owner and monotone fence.
- Expiry permits a new claim; the old fence cannot complete.
- Source change or deletion prevents completion.
- Conflicting blob digest cannot replace an artifact.
- Unsupported stays terminal only for the exact key.
- Upgrade and fresh-schema paths have the expected digest.
- Standalone SQLite v30 completes local publish, resolve, sole-voter
  supersession, fenced GC, and restart without Hiqlite.
- Hiqlite import/export and growth paths include metadata but not blobs.
- SQLite-to-Hiqlite activation copies valid blobs before importing locations.
- Ready work with all locations lost returns to the same queued job row.

Commands:

```bash
make unit
make cluster-store-check
make cluster-growth
```

### 13.4 M3 — Worker, pagination, and preemption

Implement:

- per-node/engine discovery lease and cursor;
- continuous cluster workers and global permits;
- local source exclusions;
- progress-based child supervision;
- foreground pressure cancellation; and
- typed retry/unsupported persistence.

Acceptance tests prove:

- A 501-file library discovers every file across restart boundaries.
- A node unable to read one file cannot prevent another node from enqueueing it.
- Three nodes claim three distinct compatible titles.
- Incompatible engine nodes do not claim each other's work.
- More than 128 higher-priority incompatible engine/root rows cannot hide one
  compatible due row from its worker.
- A healthy build beyond the old calculated total budget completes.
- A silent child stalls and is killed.
- A child emitting sub-quantum trickle bytes still stalls and is killed; task
  cancellation also kills the child through `kill_on_drop(true)`.
- A nonzero child exit after a complete fragment is never published.
- Foreground pressure yields within two seconds without failure exhaustion.
- An unreadable title on one node does not block later work there.
- Setting zero prevents discovery, miss enqueue, and active build continuation.

Commands:

```bash
make unit
make cluster-daemon-check
make cluster-harness-check
```

### 13.5 M4 — Shared publication and peer hydration

Implement:

- local v2 blob store for SQLite and Hiqlite sidecar;
- secure shared artifact path and atomic publication;
- exact-voter peer GET route and bounded streaming client;
- resolver tiering and local hydration;
- per-node location observations; and
- rendition plan binding to v2 key.

Acceptance tests prove:

- Node B serves VOD from node A's artifact without running FFmpeg.
- Shared mode creates one canonical blob and both nodes use it.
- No-shared mode hydrates B from A and registers B's local location.
- Shared canary loss falls back to peer.
- Holder loss causes bounded pending/rebuild behavior.
- Bad HMAC, replay, uppercase key, traversal, oversize, digest mismatch,
  truncation, and catalog conflict all fail closed.
- A signature for one dynamic key cannot authorize a different key.
- Mutating the held source after a segment is buffered but before sink commit
  exposes no segment and terminates the generation as `vod_source_changed`.
- Replicated WAL and snapshot fixtures contain no blob body.

Commands:

```bash
make unit
make cluster-check
scripts/playback-lab run --suite vod --browser chrome --json out/vod.json
```

### 13.6 M5 — Repair, GC, UI, and rollout docs

Implement:

- bounded local/shared orphan GC;
- bounded current-key observation and superseded metadata retirement;
- unhealthy-location observation and repair;
- settings and system status surfaces;
- fixed-cardinality metrics and structured events;
- feature-gated rollout and rollback behavior; and
- updates to VOD, operations, testing, status, and clustering documentation.

Acceptance tests prove:

- Referenced and running artifacts survive GC.
- Old unreferenced blobs are removed only after authoritative catalog success.
- Catalog outage makes shared GC keep bytes.
- Concurrent publication and GC serialize through reservation/tombstone state.
- A delayed publisher with a pre-expiry timestamp fails after GC increments the
  publication fence; a delayed GC fails after publisher removes an expired
  tombstone.
- A compacted publication row may be recreated, but an old lease with the same
  numeric fence fails because its incarnation differs.
- Repair restores a shared location from a peer holder.
- Fresh observations from every eligible voter mark an unused old key
  superseded and retire it only after seven days.
- Any current matching, missing, stale, or inaccessible voter observation keeps
  an old key; divergent current source keys alert and retain both artifacts.
- Retiring metadata makes local/shared bytes eligible for the existing fenced
  physical sweeps without interrupting an already resolved session.
- Dashboard counts match store state without key/file metric labels.
- Feature disable yields active work and preserves v2 data.

Commands:

```bash
make check
make test-full
make validate
```

## 14. Expected file changes

### 14.1 `plurx-core`

| File | Change |
| --- | --- |
| `crates/plurx-core/src/segplan.rs` | Expose the version used by key construction. |
| `crates/plurx-core/src/transcode/mod.rs` | Accept a stable inherited source handle for v2 copy recipes. |
| `crates/plurx-core/src/store/fragindex.rs` | Add key, artifact, codec, blob-store, and full-digest models. |
| `crates/plurx-core/src/store/mod.rs` | Add work/catalog traits and remove v2 reliance on node-local-only semantics. |
| `crates/plurx-core/src/store/sqlite/fragindex.rs` | Add complete standalone v30 metadata, local blob, attestation memo/job, and cleanup implementation. |
| `crates/plurx-core/src/store/sqlite/mod.rs` | Register SQLite migration v30. |
| `crates/plurx-core/src/store/hiqlite_fragindex.rs` | Add replicated job/catalog, both observation types, publication/GC fencing, cursors, and retention. |
| `crates/plurx-core/src/store/hiqlite.rs` | Register schema v12 and new store traits. |
| `crates/plurx-core/src/store/hiqlite_durable.rs` | Add bootstrap SQL and schema digest coverage. |
| `crates/plurx-core/src/store/hiqlite_import.rs` | Import replicated metadata; continue excluding blob tables. |
| `crates/plurx-core/src/store/telemetry.rs` | Add sidecar v7 local blobs and full-digest memos/jobs with bounded cleanup. |
| `crates/plurx-core/src/cluster/migration.rs` | Copy verified v2 blobs before importing locations. |
| Store backend test modules | Add parity tests for queue and artifact invariants. |
| `crates/plurx-cluster-check/src/storage_evidence.rs` | Verify v12 schema and no replicated blob columns. |

Backend parity tests should remain beside the current SQLite and Hiqlite store
tests; the requirement is shared invariants, not a new module for its own sake.

### 14.2 `plurxd`

| File | Change |
| --- | --- |
| `crates/plurxd/src/fragindex.rs` | Split builder, codec use, progress supervision, and typed outcomes. |
| `crates/plurxd/src/ffmpeg.rs` | Resolve/hash executable and full version; expose typed copy recipe. |
| `crates/plurxd/src/job_lease.rs` | Supervise discovery and global media-pass permit loss. |
| `crates/plurxd/src/state.rs` | Replace capped pass with discovery and worker loops. |
| `crates/plurxd/src/schedule.rs` | Keep cold cadence; wake continuous queue and repair work. |
| `crates/plurxd/src/shared_cache.rs` | Add derived index path and atomic verified publish/read helpers. |
| `crates/plurxd/src/http/internal_media.rs` | Add authenticated bounded index handler. |
| `crates/plurxd/src/http/peer_transport.rs` | Add bounded streaming fetch and protocol capability. |
| `crates/plurxd/src/http/mod.rs` | Register the internal route. |
| `crates/plurxd/src/vodserve.rs` | Use v2 resolver, bind rendition keys to index key, and gate init/segment publication on held-source re-attestation. |
| `crates/plurxd/src/http/system.rs` | Report queue, worker, tier, and fault status. |
| `crates/plurxd/src/main.rs` | Compute engine identity and start supervised loops. |
| `crates/plurxd/src/telemetry.rs` | Add fixed-cardinality index metrics. |

Prefer a new `fragindex_cache.rs` only if it keeps `fragindex.rs` focused on
FFmpeg generation. Do not create a thin wrapper that hides invariants without
reducing coupling.

### 14.3 Web and documentation

| File | Change |
| --- | --- |
| Settings UI source | Add feature flag, global-worker presets, and disable semantics. |
| [VOD-CUTOVER.md](VOD-CUTOVER.md) | Replace node-local-only operational assumptions. |
| [OPERATIONS.md](OPERATIONS.md) | Add rollout, status, retry, GC, and failure procedures. |
| [PLAYBACK-TESTING.md](PLAYBACK-TESTING.md) | Add shared and peer hydration test matrix. |
| [PERF-PLAN.md](PERF-PLAN.md) | Link implemented cross-title distribution to this plan. |
| `docs/STATUS.html` | Record implementation state only as milestones actually land. |

## 15. Validation matrix

### 15.1 Identity

| Scenario | Expected result |
| --- | --- |
| Same bytes but different canonical mount paths in v1 | Node is incompatible; no claim. |
| Same file ID/size/mtime but any changed byte | Different key. |
| Same source, different FFmpeg executable bytes | Different key. |
| Same first version line, different remaining version output | Different key. |
| Same executable, bumped Plurx contract | Different key. |
| Same source, output-affecting recipe change | Different key. |
| Different source path only | Same pipeline digest. |
| Held inode mutates after fragment buffering | No materialization/notification; child killed with `vod_source_changed`. |

### 15.2 Concurrency and fencing

| Scenario | Expected result |
| --- | --- |
| Two discoverers enqueue one key | One active job. |
| Two nodes race one claim | One fence owner. |
| Owner pauses beyond lease and resumes | Completion rejected. |
| Replacement sees uncataloged orphan | Rebuild; orphan never becomes authoritative. |
| Ready artifact loses every location | Same job fenced-requeues and reproduces digest. |
| Two builds differ for one key | Existing artifact kept; alert raised. |
| Global limit lowered during builds | No new excess claims. |
| Standalone v30 publish, restart, resolve, supersede, and GC | Same invariants with the sole-voter context and no Hiqlite. |

### 15.3 Cache tiers

| Local | Shared | Peer | Expected result |
| --- | --- | --- | --- |
| Valid | Any | Any | Serve local. |
| Missing | Valid | Any | Verify, hydrate, serve. |
| Missing | Disabled | Valid | Verify, hydrate, serve. |
| Corrupt | Valid | Any | Quarantine local, hydrate shared. |
| Missing | Corrupt | Valid | Record local observation, hydrate peer. |
| Missing | Missing | Missing | Enqueue/reprioritize, return pending. |
| Valid-looking bytes, absent artifact | Any | Any | Do not serve or adopt as v2. |

### 15.4 Failure injection

Tests kill the worker at each boundary:

1. After claim, before source open.
2. During full source hashing.
3. During FFmpeg generation.
4. After blob encode, before local install.
5. After local install, before shared publish.
6. After shared rename, before fenced completion.
7. During completion ambiguity.
8. After completion, before local success handling.

Also race path replacement between attestation and FFmpeg spawn; mutate the
held inode after init/segment buffering but before publication; disable the
feature between final rename and completion; and race a GC tombstone claim
against publication reservation. The mutation case must produce
`vod_source_changed`, terminate the child, and perform zero materialization or
waiter notification for the buffered object. For each boundary the invariant
is the same: at most one
authoritative artifact, no stale-fence publication, and eventual verified reuse,
rebuild, or safe GC of physical bytes.

### 15.5 Repository gates

The final implementation must pass the strongest relevant existing gates:

```bash
make fmt-check
make lint
make unit
make test-full
make cluster-store-check
make cluster-harness-check
make cluster-daemon-check
make cluster-check
make cluster-growth
make validate
scripts/playback-lab run --suite vod --browser chrome --json out/vod.json
```

If a gate is platform-specific, CI or the documented supported host runs it.
The handoff records exact commands, environment, and any skipped gate with a
reason; "not run" is not treated as passing.

## 16. Deferred same-file parallelism

Cross-title distribution should remove most wall-clock backlog because a media
library normally contains far more files than workers. Splitting one file is
deferred until metrics show that one or a few unusually large titles dominate
queue age after cluster distribution.

### 16.1 Why it is deferred

The current index is derived from one continuous FFmpeg copy stream. Independent
seeks can alter initialization data, DTS origin, fragment boundaries, promoted
parameter sets, and exact byte counts. Naively concatenating rows would violate
the landing matcher even if durations appear correct.

It also multiplies simultaneous reads of one source and may make NAS throughput
worse. Cross-file workers require none of the stitching proof and provide the
same cluster-level parallelism when backlog is broad.

### 16.2 Required future contract

If metrics justify a second phase, a sharded builder must define and prove:

- deterministic time ranges with overlap on both sides;
- a canonical rule for selecting the unique fragment sequence in overlap;
- affine DTS rebasing into one timeline;
- one authoritative initialization section or a proof of equivalence;
- deterministic promoted-parameter reduction;
- complete coverage with no gap, duplicate, or negative duration;
- exact row byte counts after stitching; and
- identical final blob digest to a single-pass reference fixture.

The acceptance bar is byte-for-byte equivalence on a broad codec/container
fixture set, not merely playable output. Until that exists, same-file sharding
must remain disabled.

### 16.3 Revisit trigger

Revisit only after production telemetry for at least one full backfill shows all
of the following:

1. Global workers spend significant time idle while one title is running.
2. p95 or maximum single-title build time materially drives readiness SLOs.
3. Shared-source storage has measured headroom for parallel range reads.
4. The deterministic reference suite covers the dominant containers/codecs.

## 17. Open rollout measurements

After M0 records the deployment class's stop/go result, these measurements tune
worker and rollout defaults without changing the correctness contract:

- NAS latency at one, two, four, and eight concurrent copy-index reads.
- Actual p50/p95/p99 blob sizes and fragment counts.
- Time spent in FFmpeg versus full source hashing and publication.
- Frequency of files exceeding the old calculated adaptive budget.
- Incidence of mismatched full source digests across voters.
- Peer hydration latency and failure rate without shared storage.
- Foreground preemption frequency during backfill.

If two workers materially degrade playback or NAS health, the default becomes
one. The queue, cache, and fencing design still provide cluster sharing and
correct restart behavior; performance tuning does not change identity.

## 18. Definition of done

The project is done when:

1. The success criteria in Section 1.3 have automated coverage.
2. New SQL exists in fresh, upgrade, import/export, and cluster-growth paths.
3. Replicated-state inspection proves no blob bytes enter Raft storage.
4. Shared and peer modes both pass the VOD playback lab.
5. A mixed-engine cluster demonstrates isolated compatibility groups.
6. Operational status can distinguish queued, unsupported, failed, corrupt,
   and unavailable-location cases.
7. Rollout and rollback are documented and rehearsed on a multi-node fixture.
8. The feature flag has an owner and removal milestone.
9. Legacy fallback removal is explicitly deferred to a later reviewed change.
10. An adversarial review has no unresolved correctness or liveness blocker.
11. At least one target multi-node storage class has a recorded M0 `go`, and
    every measured `no-go` class remains disabled by default.
