# VOD index cache review — sound shape, unsafe first contract

**Status:** Two-pass adversarial review complete · **Reviewed:** original and
revised [VOD-INDEX-CACHE-PLAN.md](VOD-INDEX-CACHE-PLAN.md) · **Repository:**
working tree at `356a93f6` · **Written:** 2026-08-26 · **Outcome:** ready for
staged implementation; M1 is conditional on an M0 multi-node `go`

This is the independent adversarial review requested after the first complete
implementation draft. The reviewer made no file changes. It checked the draft
against the current fragment indexer, store contracts, replicated migrations,
pretranscode fence transaction, shared-cache coordinator, peer transport,
media-pool path behavior, and SQLite-to-Hiqlite activation path. The original
architecture was retained, but its identity, recovery, and migration contracts
were not safe enough to implement verbatim.

Line numbers in the original review referred to the pre-revision draft. This
companion records stable section references and the final disposition instead,
because the plan was then rewritten around the accepted findings.

## 1. Verdict

The metadata/blob split, cross-title parallelism, immutable artifact model, and
decision to defer same-file sharding were all judged sound. The original draft
was **not ready to implement** because four blockers could produce wrong byte
accounting or permanent unavailability:

1. A sparse source sample was described as an exact cross-node identity.
2. FFmpeg reopened a pathname after a different descriptor was attested.
3. An uncataloged orphan could be adopted without a trusted expected digest.
4. A permanent `ready` job had no legal rebuild transition after all physical
   locations disappeared.

The review also found twelve high-severity recovery, retry, migration,
shared-state, peer-authentication, and GC issues; six medium contract gaps; and
four lower-level hardening items. Every blocker and high-severity finding is
addressed in the revised plan. Sections 5 and 6 record the follow-up audit gate
and its final disposition.

## 2. Review basis

### 2.1 Code inspected

The reviewer verified the plan against these implementation boundaries:

- Node-local index rationale and store contract in
  `crates/plurx-core/src/store/mod.rs`.
- Current packed rows and local schema in
  `crates/plurx-core/src/store/fragindex.rs`.
- Current cursor, adaptive total deadline, and scheduler in
  `crates/plurxd/src/state.rs`.
- Current FFmpeg pathname recipe in
  `crates/plurx-core/src/transcode/mod.rs`.
- Current parser/child handling in `crates/plurxd/src/fragindex.rs`.
- Replicated queue fencing in
  `crates/plurx-core/src/store/hiqlite_pretranscode.rs`.
- Generic job leases and publication fencing in
  `crates/plurx-core/src/store/hiqlite_coordination.rs` and
  `crates/plurxd/src/job_lease.rs`.
- Shared-cache claim/publication and per-node observations in
  `crates/plurxd/src/shared_cache.rs`.
- Static-path signing in `crates/plurxd/src/http/peer_transport.rs`.
- Root fingerprint behavior in `crates/plurx-core/src/scan/mod.rs`.
- Automatic Hiqlite migration in `crates/plurx-core/src/store/hiqlite.rs`.
- Activation/import behavior in
  `crates/plurx-core/src/cluster/migration.rs` and
  `crates/plurx-core/src/store/hiqlite_import.rs`.

### 2.2 Severity

- **Blocker:** implementing the draft can produce incorrect serving state or a
  permanent liveness failure.
- **High:** a required recovery, migration, security, or operator contract is
  missing or contradicted by the code.
- **Medium:** the proposed contract is underspecified or the baseline is false.
- **Low:** hardening or validation that should ship with the implementation.

## 3. Findings and dispositions

### 3.1 Blockers

#### B1 — Sparse source identity cannot govern exact byte accounting

The original key sampled sixteen 64 KiB regions. Two same-size, same-mtime
copies can differ in an unsampled video packet, so the same key could name
different fragment byte counts. That violated the reason the current store is
node-local (`crates/plurx-core/src/store/mod.rs`).

**Disposition:** Accepted. Plan §4 now requires SHA-256 of every source byte.
Sparse reads may prefilter but can never authorize reuse. A node-local memo is
bound to device, inode, size, nanosecond mtime, and nanosecond ctime. A VOD
request without a valid memo returns pending while bounded background work
computes the full digest.

#### B2 — Attestation and FFmpeg input had a pathname TOCTOU

The draft held and rechecked one descriptor, but the existing typed recipe
passes `source.path` to FFmpeg. A rename or symlink swap could make FFmpeg index
file B while both checks examined file A.

**Disposition:** Accepted. Plan §4.5 introduces `AttestedMediaSource` and an
inherited descriptor contract: duplicate the held regular file to descriptor 3
and pass `/proc/self/fd/3` on Linux or `/dev/fd/3` on macOS. VOD generation uses
the same handle discipline. Path-replacement fault injection is required.

#### B3 — Uncataloged orphan adoption had no trust root

A header key only claims what produced bytes. Without an artifact row, there is
no authoritative blob digest against which to distinguish a valid artifact
from plausible corruption.

**Disposition:** Accepted. Plan §§5.7 and 7 permit reuse only when an existing
artifact supplies the expected digest. First-publication orphans are never
adopted. Fenced publication reservations exclude GC but do not make bytes
serveable.

#### B4 — `ready` blocked the promised rebuild

The original unique active-key index included `ready`, while neither the state
machine nor trait allowed `ready -> queued`. All-location loss therefore left a
permanent pending title. Artifact equality also included builder provenance,
making a valid rebuild on another node conflict.

**Disposition:** Accepted. Plan §5.5 now stores one durable row per key and
defines exactly one automatic terminal transition: a fenced transaction may
move `ready -> queued` only after proving the artifact has zero complete
locations. Rebuild retains the authoritative digest. Provenance and timestamps
are excluded from immutable equality.

### 3.2 High-severity findings

| ID | Finding | Disposition in revised plan |
| --- | --- | --- |
| H1 | One engine-scoped discoverer could skip a file only another node can open. | §5.10 uses per-node/engine cursors; every file disposition and cursor advance are one fenced transaction. |
| H2 | Different mount paths were claimed despite no path-mapping layer. | §4.4 makes identical canonical roots/paths a v1 compatibility requirement; mapping is deferred. |
| H3 | Retry exhaustion lacked a counter and `failed` misses could create fresh jobs forever. | §5.5 adds mandatory `failure_attempts`, one row per key, durable failed tombstones, and explicit operator retry. |
| H4 | Terminal negatives did not include parser/blob-policy versions. | §4 adds `indexer_contract_version` to the key and requires bumps for acceptance or negative-policy changes. |
| H5 | Parser completion could publish after a nonzero child exit. | §6.9 requires parser terminal state, stdout EOF, and zero `wait()` status independently. |
| H6 | The deletion trigger evaluated wall-clock SQL in replicated apply. | §5.6 removes clock functions and preserves prior timestamps. |
| H7 | The rollout assumed a manual post-upgrade migration, but `open_or_migrate` runs automatically. | §§8.6 and 12.2 specify first-voter automatic additive migration, no old-binary restart, then remaining voters and learners. |
| H8 | SQLite activation imported ready metadata but created an empty sidecar. | §12.5 copies and verifies blobs/memos sidecar-first, imports locations only for copied bytes, and requeues zero-location ready work. |
| H9 | One broken mount could globally mark a healthy shared object suspect. | §5.4 adds per-observer state and a two-current-voter retirement threshold, with a one-voter exception. |
| H10 | The dynamic key was not explicitly part of the peer HMAC. | §7.6 requires signing the complete canonical path and a key-A/key-B substitution test. |
| H11 | Shared GC had a lookup-to-unlink race with publication. | §5.7 adds fenced publication reservations, GC tombstones, atomic quarantine rename, and post-rename revalidation. |
| H12 | Feature disable was prose, not a completion predicate. | §5.8 requires replicated enable settings in fenced completion and a disable-between-rename-and-complete test. |

### 3.3 Medium-severity findings

| ID | Finding | Disposition in revised plan |
| --- | --- | --- |
| M1 | Baseline claimed no cursor and a fixed 90-second budget. | §1.2 now describes the current wrapping local cursor and adaptive 90-second-to-30-minute total budget accurately. |
| M2 | Blob header and rows were not independently implementable. | §4.6 specifies every width, endian, enum code, padding byte, count, and overflow rule. |
| M3 | Local v30/v7 schema and corruption state were missing. | §5.9 supplies strict SQL for blobs and full-digest memos, including `quarantined`. |
| M4 | Resolver called `load_verified` before loading its required artifact. | §7.5 loads authority first; uncataloged local bytes never count as v2 hits. |
| M5 | Arbitrary location IDs could crowd healthy holders out of a bounded query. | §5.4 keys locations by `(key, class, storage)` and defines deterministic healthy-first selection. |
| M6 | Global permit expiry did not stop a still-renewing job. | §6.7 selects on the existing `ActiveJobLease` loss token and yields immediately on permit loss. |

### 3.4 Low-severity hardening

The revised SQL uses `STRICT`, lowercase-hex checks, identifier/detail bounds,
nonnegative times, and fence exhaustion bounds. The copy-engine digest now
includes a recursively resolved ELF/Mach-O dynamic-library manifest and a
sanitized environment. The full-digest memo uses ctime as well as
device/inode/size/mtime. The validation matrix now includes standalone SQLite,
activation, all-location loss, inaccessible discovery roots, failed-miss
suppression, path-key substitution, nonzero child exit, and concurrent
publication/GC.

## 4. Decisions retained

The review did not challenge these architectural decisions:

1. Keep artifact bytes out of Raft, its WAL, and snapshots.
2. Replicate small job, integrity, and location facts.
3. Use immutable content-addressed artifacts.
4. Parallelize different titles across the cluster before splitting one title.
5. Prefer a verified shared mount and fall back to authenticated peer hydration.
6. Keep rendition plans local and bind their identity to the v2 index key.
7. Keep legacy indexes local during migration; never synthesize missing v2
   identity.
8. Use existing queue and lease/fence patterns rather than inventing advisory
   locks.

## 5. Resolution audit gate

The original verdict required another design pass before implementation. The
same adversarial reviewer must now inspect the revised plan and answer:

1. Does full-content identity plus stable-handle use close cross-node source
   equivalence and pathname races?
2. Can one-row-per-key state always recover from all-location loss without
   enabling failed-miss loops?
3. Do publication reservations and GC tombstones serialize every physical
   create/delete race?
4. Is the automatic migration and activation sequence faithful to current
   code?
5. Are peer authorization and per-node shared observations bounded and
   non-globalizing?
6. Does any remaining prose promise lack an atomic store or process mechanism?

No implementation milestone should start with an unresolved blocker or
high-severity finding from that audit.

## 6. Final resolution audit

The same adversarial reviewer inspected every revision, checked new mechanisms
against the current code again, and returned this final severity result:

- **Blocker/high:** none unresolved.
- **Medium:** none unresolved.
- **Verdict:** **ready for staged implementation**.

M0 may begin immediately. M1 remains deliberately conditional on at least one
target multi-node storage class meeting the topology-aware M0 byte, CPU,
readiness, and foreground-latency gate. Standalone v2 remains correctness-tested
but default-off unless measurement demonstrates a benefit.

### 6.1 Follow-up findings and dispositions

| ID | Follow-up finding | Final disposition |
| --- | --- | --- |
| R1 | Builder-only reservation left hydration and repair outside publication/GC exclusion. | §5.7 defines job- and artifact-authorized publication leases; §§7.2–7.4 require every install/register path to present the exact lease. |
| R2 | A `ready` artifact whose holders were alive but repeatedly unavailable could remain stuck forever. | §§5.4–5.5 and 7.5 add fresh per-observer probes and a fenced `ready -> queued` recovery after sustained all-location failure without globally poisoning a holder. |
| R3 | Repeated misses before a full digest existed had no deduplication key. | §§5.9 and 6.6 add one bounded local digest job per exact object-version tuple, retry tombstones, a killable helper, and pending behavior. |
| R4 | Valid-vs-valid local/shared conflicts could be deleted and silently choose a winner. | §§7.2–7.3 and 8.4 make them terminal nondeterminism evidence; only independently proven corruption may be auto-quarantined and replaced. |
| R5 | Historical shared-location failures could accumulate into retirement after conditions recovered. | §5.4 requires recent independent observations, a recent verified canary for each, and no later healthy result. |
| R6 | Activation ordering could construct Hiqlite before the copied sidecar existed. | §12.5 installs and verifies the exact sidecar path before constructing or calling `HiqliteAuthStore::bootstrap`. |
| R7 | Permit TTLs and progress renewal were inconsistent and could leave a detached or trickling child alive. | §§6.7 and 6.9 retain the existing generic-lease cadence, bind work to `ActiveJobLease` loss, require quantum progress, and keep `kill_on_drop(true)`. |
| R8 | Expired publication timestamps allowed delayed GC/publisher ABA races. | §5.7 CAS-invalidates the exact incarnation/fence, carries both in tombstones, and revalidates after quarantine before unlink. |
| R9 | Node-local SQL blobs were described with shared-filesystem rename semantics. | §§5.7 and 9.1 give node-local holders a publication-mutex plus exact SQL-row CAS-delete protocol; shared files keep same-filesystem quarantine. |
| R10 | Capability filtering after a global batch could starve compatible work behind an arbitrarily large incompatible prefix. | §§5.5, 6.1, and 6.2 predicate engine/segment-plan/root before `LIMIT` and use a wrapping per-capability claim cursor that advances on claims and refusals. |
| R11 | A live FFmpeg or dependency replacement could execute under an old engine digest. | §4.2 retains and revalidates executable/dependency handles, withdraws capability on change, and requires immutable runtime closure for v1 serving. |
| R12 | Historical source/engine/pipeline variants could grow forever because `files` updates in place. | §§5.3 and 9.4 add bounded current-key observations, unanimous fresh-voter supersession proof, seven-day grace, and fenced retirement. |
| R13 | Any output byte reset the stall timer, allowing an infinite trickle. | §6.9 requires complete-fragment or minimum-byte progress quanta and tests a sub-quantum trickle. |
| R14 | The first M0 byte gate was mathematically impossible for one- to three-node topologies. | §§12.1 and 13.1 now state the `N * S` versus `(N + 1) * S` model, account for all hash/FFmpeg/daemon CPU, gate each deployment class on measured benefit, and leave no-go classes disabled. |
| R15 | A stable descriptor did not prevent in-place mutation of the same inode while VOD bytes were being exposed. | §4.5 buffers one bounded init/segment and re-`fstat`s before materialization/notification; mutation kills the child with `vod_source_changed` and publishes no buffered bytes. |
| R16 | Observation, replica-publication, and claim APIs accepted forgeable node-ID strings. | §§5.8 and 6.2 use a private `ExactVoterContext`, bind its capability epoch/digest, and derive identity for claim, renew/requeue, observations, and replica publication. |
| R17 | Retaining a monotone publication row per historical key made the metadata-bound claim false. | §§5.7 and 9.4 add random incarnations and CAS compaction after proven physical absence, preventing delayed-lease ABA when a row is recreated. |
| R18 | Digest tombstones lacked deletion cleanup, standalone acceptance was implicit, and schema summaries omitted tables. | §§5.1 and 5.9 define bounded catalog-aware cleanup and complete v30/v12/v7 scope; §§13.3 and 15 require standalone publish/resolve/supersede/GC/restart coverage. |

### 6.2 Final low-level closure

The reviewer’s only final low-severity note was that Rust models nanosecond mtime
and ctime as `i128`, while SQLite `INTEGER` is signed 64-bit. Section 5.9 now
stores those values as validated canonical signed-decimal text, preserving the
full `i128` range and failing closed on parse errors.

The final review found no remaining correctness, liveness, migration, fencing,
or security issue that should block M0.
