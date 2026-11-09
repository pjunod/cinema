# Playback start — A1/B1 implementation and publication contract

**Status:** accepted implementation contract — owner accepted the mechanism
2026-11-09; implementation and hardware qualification remain unbuilt. **Executes:** A1 and B1 from
[plan revision 3](PLAYBACK-START-PLAN.md), preserving A1-N and the A2 handoff.
**Written:** 2026-11-09 · **Source baseline:** `30f4343baded7c12a34bd71ba215d1802a0b15f8`.
**Owner:** Codex/root, chat `01a1c166-b5b2-7033-bd74-77816ba09733`.

The [plan](PLAYBACK-START-PLAN.md#6-lane-status) governs rulings;
the [continuation ledger](PLAYBACK-START-STATUS.md) owns execution evidence.
The [Phase 1 contract](PLAYBACK-START-BUILD.md) supplies prior context.
Re-anchor symbols before editing. Grant isolation, v2 meaning and complete-only
publication are fixed boundaries; implementation cannot waive them.

Use normal commits in one PR; adversarial review happens when implementation
is ready to merge, then final checks. Retain passing regressions across rebases;
rerun failures or genuinely changed coverage. Hardware acceptance is separate.

## 1. Objective and guardrails

One copy engine cuts servable media and canonical index observations on first
play. Indexed and cold starts use that engine; background work fills sparse
gaps under the same file-level fenced computation. Cold resume at 90 minutes
input-seeks to a bounded, verified sync point and never reads the first
90 minutes sequentially just to start.

Create takes one planning snapshot, makes a pure decision and performs at most
two durable mutations. It schedules preparation under the existing durable
start identity and answers without awaiting a new Analysis enqueue/join write.
Claim attachment is worker-side after that response; pending readiness uses
the existing typed retry/result route. Lost replies and abandonment remain owned
by that identity. Attestation, advisory work and checkpoint I/O cannot become
pre-answer dependencies. This deliberately accepts pending readiness rather
than adding a third critical-path write.

**Non-goals:** no new claim service, third copy engine, Live TV change, feature
gate/watchdog, processed-DV identity migration or global target replacement.
These duplicate ownership or alter unrelated contracts. A2 shares admission.

## 2. Source facts that constrain the mechanism

| Existing boundary | Source-backed behavior | Consequence |
|---|---|---|
| Provisional analysis | [state.rs](../../crates/plurxd/src/state.rs), `enqueue_copy_preparation_for_object_with_viewer` around 3307; `resolve_analysis_request` around 10348 | Enqueue precedes hashing. Analysis rows already own lease, fence, heartbeat and physical custody. Reuse them. |
| Analysis uniqueness | [fragment_index_cluster.rs](../../crates/plurx-core/src/store/fragment_index_cluster.rs), analysis schema around 858 and `AnalysisRequest` around 1548 | Target, requested generation, video identity and pipeline participate in current uniqueness. Different ingresses can create duplicate pre-hash computations. A lookup-only change cannot fix this. |
| Strong dedupe | [background_jobs_fragment_admission.rs](../../crates/plurx-core/src/store/background_jobs_fragment_admission.rs), `prepare` and `fragment:{cache_key}` | Post-hash jobs already coalesce across targets. Keep that computation and its fenced publication. |
| Ownership handoff | [state.rs](../../crates/plurxd/src/state.rs), `submit_fragment_index_analysis` around 10831; [SQLite twin](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs), submit around 1149; [Hiqlite twin](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs), around 2539 | Submission retires Analysis authority and makes JobToken responsible for future work. Existing submission does not adopt a live producer's checkpoint. |
| Source catalog authority | [create.rs](../../crates/plurxd/src/http/hls/create.rs), `prepare_source_playback` around 6493 | The prepared file is the origin Source installation's actual catalog file, after exact grant/witness checks. Receiver owns relay, not remux or a same-numbered receiver file. |
| Source execution | [source_actor.rs](../../crates/plurxd/src/transcode/source_actor.rs), `prepare_source_cold_index` around 1088 and call around 1438 | Source actor currently blocks on a separate complete index scan. Its authority is needed, its duplicate scanner is removed. |
| Serving isolation | [plan.rs](../../crates/plurxd/src/vod/plan.rs), `source_rendition_key`; [shared.rs](../../crates/plurxd/src/vod/shared.rs), `attach_rendition_owned`; [serve/create.rs](../../crates/plurxd/src/vod/serve/create.rs), around 2315 and 2668 | Source rendition keys include grant-side source tuple/revision; Source sets local viewer to None and bypasses cluster index lookup. Share computation while preserving these distinct serving identities. |
| Local viewer authority | [state.rs](../../crates/plurxd/src/state.rs), `PlaybackViewerDemand::require_local_authority` around 3290 | Sharing principals cannot be converted into local viewer demand. Introduce a typed Source interest. |
| Row extraction | [segplan.rs](../../crates/plurx-core/src/segplan.rs), `IndexRow`/`from_fragment` around 567/602 | Wire bytes mean the entire video-only fragment; video payload bytes are a different field. Multiplexed AV wire length cannot substitute. |
| Current verifier | [vodgen.rs](../../crates/plurxd/src/vodgen.rs), `GenerationRun::feed_landed` around 825 | It compares only video payload length and stops after first segment publication. B1 must extend both the measurements and the lifetime. |
| Existing seek probe | [generation.rs](../../crates/plurxd/src/vod/generation.rs), `measure_landing`/`first_packet_time` around 1166/1235 | Framecrc field 2 is PTS; landing uses floating-point PTS differences. It is not an exact absolute DTS witness. |
| Complete publication | [background_jobs_fragment.rs](../../crates/plurx-core/src/store/background_jobs_fragment.rs), `PublishFragmentJob`; [cluster index formats](../../crates/plurx-core/src/store/fragment_index_cluster.rs), v2 encode/decode around 2776/2853 | Source digest, immutable blob digest, exact job/fence/revision/lease and generation remain mandatory. Sparse ranges cannot pass this boundary. |

Engineering knowledge declares no reviewed plan coverage: unknown, not absent
obligations. Navigation and imported check metadata are not test evidence.

## 3. One Analysis computation, typed interests, separate physical readers

### 3.1 The analysis worker executes the common engine

**Decision:** the existing analysis worker owns canonical metadata computation.
Source actors and local VOD actors attach typed interests and request ranges;
they do not claim separate index jobs. This chooses a single executor bridge
instead of making every grant-bearing Source actor a second kind of claimant.
The analysis worker gains a foreground coordination mode before attestation.
It delegates range production under its Analysis fence to the existing local
or Source serving actor, which invokes the common engine and owns its physical
child lifetime. The canonical executor validates/merges contributions; it does
not steal Source actor grant or cancellation ownership. Both run on an eligible
origin node able to open the held source; the receiver remains a relay.

Change the analysis entrypoint: today's `resolve_analysis_request` always
registers background custody and `SourceReadBudget` before hashing. Foreground
coordination skips that physical-read admission; the serving actor already
owns the actual input. Background gap fill still uses the existing budget.

All signatures and types in code fences are **proposed**, not existing APIs.

```rust
struct FileIndexComputation {
    catalog_namespace: CatalogNamespace,
    file_id: i64,
    source_generation: SourceGeneration,
    selected_video: VideoTrackIdentity,
    output_contract: FragmentIndexContractId,
}
enum IndexInterest {
    Local(PlaybackViewerDemand),
    Source(ValidatedSourceIndexInterest),
    Background(BackgroundIndexDemand),
}
struct AnalysisRangeToken {
    request_id: String,
    analysis_fence: i64, // existing validated Store fence; preserve its meaning
    range_id: RangeId,
    range_epoch: u64,
}
async fn join_file_index(
    computation: FileIndexComputation, interest: IndexInterest,
) -> Result<IndexRendezvous, IndexJoinError>;
async fn request_copy_range(
    rendezvous: IndexRendezvous, origin: VerifiedSyncOrigin,
    serving: ServingNamespace, authority: RangeReadAuthority,
) -> Result<RangeAttachment, RangePending>;
```

Adapt existing analysis-row claim/enqueue methods and both stores to this
identity. Normalize target node, ingress, engine provenance and requested
publication generation into interests/constraints, not duplicate computation
keys. Selected video and output recipe remain in the contract identity when
they change observations; equal file IDs alone are insufficient. Other
analysis components retain their existing identity rules. Preserve waiters,
capacity headroom, retries, predecessor fences and existing reconciliation
when migrating current rows; join only proven equivalent source generations.

`SourceGeneration` is an origin catalog namespace and observed revision plus
a held-object identity token. Size and mtime are diagnostics, not its complete
identity. A remote node's opaque object_version is not proof of equivalence to
another node's object_version. Such interests remain unmerged until content
attestation or the same origin's validated object witness proves equivalence.
This conservatively permits distinct unproven generations rather than mixing
ranges. Once attested, equivalent computations converge on the existing strong
job; their partial namespace-local bytes are not silently exchanged.

`ValidatedSourceIndexInterest` can only be created after Source grant/current
preparation validation and contains origin tuple/revision, interest lifetime
and cancellation. Renew/recheck those facts before each read/serve boundary.
It is neither a serialized bearer grant nor an unchecked request field.
Revocation cancels that interest and serving namespace, even if another local
interest keeps the shared index computation alive. Never construct the local
viewer enum arm from a Sharing principal.

### 3.2 Metadata authority and physical I/O have different ownership

The Analysis owner/fence grants canonical row mutation. Each physical range
producer also holds an existing source-custody receipt in its actual purpose:
local playback or grant-scoped Source playback while demanded, background
Analysis only when filling unrequested gaps. A range token authorizes a writer
under the Analysis lease; it does not grant permission to open source bytes.

| Activity | Canonical mutation owner | Physical custody and charge |
|---|---|---|
| First-play range producing AV and measurement output | Current Analysis fence + range epoch | Existing foreground serving owner; one charge for the one input, not an extra occupied background slot |
| Another viewer observing the same range | Same computation; no new writer | Observer has no physical-reader slot and cannot hold source I/O |
| Another position needing input reads | Same Analysis fence, distinct range epoch | Independently positioned foreground read with its own custody; domain capacity still applies |
| Background sparse-gap fill or full attestation | Same Analysis fence until sealing | Analysis-owned `SourceReadBudget`, admitted and paced by current capacity |
| Complete artifact installation/delivery | JobToken after sealing | Existing job custody for any remaining physical work; observers release unused custody |

[SourceInput](../../crates/plurxd/src/source_input.rs), source custody and
[background resources](../../crates/plurx-core/src/store/background_jobs_resources.rs)
remain the sole physical admission system. Extend their owner/purpose bridge
where needed, rather than adding a parallel budget. Claim scheduling must not
exclude this computation because its own foreground read already occupies the
domain, nor treat an unrelated occupied domain as free. Mode changes are
fenced: cancel/join child readers, settle old physical receipt, admit the new
purpose, then expose its input. Do not hold both receipts during handoff or
release a receipt while its child is still reading.

Full-source attestation is a second physical reader, admitted under background
budget after first frame and allowed to pause while playback owns capacity.
[SourceFence](../../crates/plurxd/src/fragment_index_cluster.rs) `clone_held`
around 200 uses `try_clone` only for positioned AVIO reads; `reopen` around 232
requires an independent description and exact held-object identity. The
current `attest_source_mode` around 1160 opens a separate path and seeks it,
but its size/mtime scanner check alone does not bind it to our held generation.
Extend it to require the expected full object version before/after hashing,
or use verified `SourceFence::reopen`. Never run seek+read hashing on a cloned
FFmpeg descriptor: clones share offsets. Reuse SourceInput positioned `read_at`
for bridge reads. Concurrent hashing is permitted only with this independent
cursor, matching source witness and its own charged custody; otherwise defer.

Last viewer leaving cancels and joins its range producer. Background
continuation is a separately admitted use of the same computation; it may
pause with durable sparse coverage and no source handle. Observers reserve no I/O. Demanded ranges preempt/yield background gap work
through the existing capacity policy.

### 3.3 Completion transfers ownership rather than reviving an old fence

```text
Queued Analysis
   │ claim owner/fence + demanded ranges
   ▼
Growing sparse computation ◀── range resume / background gap fill
   │ all ranges covered + full source attestation + canonical digest
   ▼
Sealing (range writers stopped and joined; checkpoint manifest immutable)
   │ atomic submit/adopt into existing strong job
   ▼
JobToken owns sealed complete candidate ──▶ fenced artifact publication
   │
   └─ Analysis fence retired; old range writes always rejected
```

Extend `submit_fragment_index_analysis` with a sealed complete-checkpoint
adoption operation. One transaction checks current Analysis fence, source,
coverage, contract, predecessor generation and manifest digest; creates/joins
the existing `fragment:{cache_key}` job; records the adopted manifest and
retires Analysis mutation authority. It does not start another full scan.
The job owner verifies the transferred complete candidate and uses existing
publication validation. If an identical strong job already won, join its
result and discard only redundant unreferenced local material.

A crash before transfer leaves Analysis recovery responsible; a crash after
transfer leaves JobToken recovery responsible. An ambiguous acknowledgment is
resolved by persisted ownership/result lookup, never by allowing both tokens.
No post-submit write may carry the retired Analysis fence. Leases, renewal,
cancellation and takeover remain existing claim semantics with the new range
epoch/manifest checks, not an independent heartbeat framework.

## 4. Canonical rows from one input and bounded sparse seeking

### 4.1 One demux input, two measurement domains

Use one FFmpeg input for a range. Fan its selected video stream into the
production AV output and a canonical video-only measurement output; audio
exists only in AV output. Both video streams use the identical copy/sample
conversion contract. Parse both outputs with the existing `FragmentReader`
and use the existing production `Segmenter`. Apply DV conversion consistently
before row observation and use `IndexRow::from_fragment` on video-only
fragments. Wire bytes always come from that video-only output.

Extract the canonical video-only output options from today's background
builder into one shared recipe. Fix video track selection, timescale, mux
fragment boundary options, timestamp normalization, parameter retention,
promotion inputs and DV conversion revision in `FRAGMENT_INDEX_CONTRACT`.
Pair AV/video-only observations by source DTS/sample sequence, not arrival
order. Audio cannot alter canonical video fragments; mismatch stops commit.
AV `fragment.len()` never substitutes for row bytes.

This proposal has a concrete proof obligation: the range output and whole-file
background output produce exactly equal row fields and canonical init/promotion
facts for overlapping source samples, including nonzero input seek. If the
muxer's fragmentation, sequence numbering or seek normalization prevents that,
change the shared measurement recipe before enabling it. Do not relax equality
to payload length alone. The background builder uses the same engine in
measurement-only mode and fills missing covered ranges; it does not install a
second row extractor.

Reject overflows before persistence: current row extraction saturates byte
counts to `u32::MAX`; a canonical observation cannot treat saturation as exact
measurement. Checked narrowing is part of the new shared observation API.

### 4.2 A verified source DTS origin is required for every range

```rust
struct VerifiedSyncOrigin {
    source_generation: SourceGeneration,
    video_track: VideoTrackIdentity,
    source_dts: i64,
    source_pts: i64,
    time_base: RationalTimeBase,
    normalization: CanonicalTimelineOrigin,
    sync_class: CutClass,
}
fn canonical_row(
    observed: VideoOnlyObservation, origin: &VerifiedSyncOrigin,
) -> Result<IndexRow, CanonicalTimingError>;
```

Use bounded demuxer/index-assisted input seek before `-i` to locate a clean
sync point at or before the requested position. Bind signed packet DTS, PTS,
duration, rational time base and selected track to the held source; verify
that the generated range actually landed at that point. Define the canonical
whole-title timestamp origin once from a bounded beginning-of-file observation,
including the mux normalization used by the shared measurement recipe.
Rescale with checked rational arithmetic to index ticks. Reject missing DTS,
nonintegral normalization without a specified rounding rule, overflow and
ambiguous sync evidence; do not invent absolute DTS from zero-rebased output.

The current PTS float helper remains a legacy indexed landing heuristic until
separately replaced; its result cannot construct this new witness. Negative
initial DTS, B-frame reordering, nonzero initial timestamps, fractional time
bases and a demuxer landing at a neighboring GOP are mandatory cases.

A cold resume needs bounded reads near the beginning and requested sync point,
not full-source hashing. Missing trustworthy seek metadata gives a typed
per-title `copy_sync_unverified` preparation outcome (proposed code), within
the existing start budget. An independently supported encoded route may be
selected explicitly; do not hide sequential copy-from-zero behind Resume.
Eligibility uses census-level copy checks; full-source HEVC proof is deferred
to complete shared artifact publication. Observed incompatible headers still
refuse output immediately.

### 4.3 Sparse coverage commits with completed segments

```rust
struct RangeCheckpoint {
    source_generation: SourceGeneration,
    contract: FragmentIndexContractId,
    token: AnalysisRangeToken,
    origin: VerifiedSyncOrigin,
    through_source_tick: u64,
    canonical_rows: Vec<IndexRow>,
    completed_segments: Vec<VerifiedSegmentRecord>,
    next_source_sync: Option<VerifiedSyncOrigin>,
}
async fn commit_completed_segment(
    token: &AnalysisRangeToken, segment: StagedSegment,
    rows: &[VerifiedFragment], checkpoint: RangeCheckpoint,
) -> Result<CommittedRangeSnapshot, CommitError>;
```

The commit method is the only path that exposes new bytes. Write a complete
segment into a generation/range-owned staging name; flush and atomically
install immutable content; atomically publish a small manifest containing the
segment reference, verified observations and next checkpoint. Readers see
only the committed manifest snapshot. A crash between content install and
manifest leaves an orphan, never a visible half-segment. Recovery checks the
same source and contract, reads only complete manifests, drops unfinished
staging, and resumes at a verified sync point. Maintain a recoverable local fence/epoch ledger; stale async producers cannot
install manifests after takeover. Fail closed at the locally tracked Analysis
lease expiry or cancellation, even before a successor appears; revalidate on
renewal. Durable canonical adoption also checks the current Analysis fence.
No per-segment replicated write is required.

Ranges have independent source positions, segment namespaces and cancellation.
Overlapping rows merge only after exact equality under the same source/contract;
conflict enters B1 quarantine. Gap filling schedules uncovered source intervals,
yields against foreground intervals, and marks complete only with proven
beginning-to-EOF coverage and compatible init history. A sparse prefix or an
EOF seen from a 90-minute seek cannot masquerade as a complete title.

## 5. Publication preserves A1-N and held client views

### 5.1 One coherent snapshot backs plans, bytes and playlists

Replace immutable-only rendition assumptions with a proposed
`CopyPlanState::Growing(CommittedRangeSnapshot) | Frozen(CompletePlan)` in
[session.rs](../../crates/plurxd/src/vod/session.rs), shared attachment and
manifest generation. Source generation, contract, init identity, committed
segments, coverage, fixed target and completion are observed together.
`Frozen` retains put-if-absent; rendition and final v3 blob keys stay distinct.

A held EVENT URI only appends complete verified segments and eventually
`ENDLIST`. A whole-title immutable VOD playlist receives a separate URI after
completion. Range views preserve their original origin, segment numbering,
init and discontinuity sequence; sealing the complete index cannot renumber
them. Attach/reuse includes serving namespace and range view identity, even
when canonical file-level observations are shared.

### 5.2 The target is fixed before publication, even for unknown later GOPs

Keep target 16 seconds. Commit only target-valid segments; GOP count alone
does not establish admission.

An unseen GOP may make copy incapable of honoring that fixed target. This
has an explicit per-title outcome: before any invalid segment is advertised,
stop its range producer with proposed `copy_target_exceeded`, preserve the
verified committed prefix and report replacement required. Never raise the
target on the existing URI or append a knowingly invalid segment. If supported,
a new presentation is prepared at the held source position, preserving the
requested resolution and capabilities, with a new URI and its own admission;
otherwise return the typed terminal outcome.
Do not append ENDLIST as if this truncated prefix were a completed title.
The supported cold-copy acceptance denominator must record these outcomes;
a fixed target is not a promise that arbitrary unseen GOPs are suitable.

Late SPS/PPS or codec configuration changes likewise cannot use an old init
silently. This implementation chooses a typed per-title
`copy_init_contract_changed` outcome and prepared replacement, preserving the
valid prefix. Multi-init complete-index/discontinuity support can be added
only under an explicit output contract revision; it is not inferred from a
successful first GOP.

A1-N is authoritative: only exact `hlsjs` gets the short-reload rule. Native,
missing and unknown transport require three advertised target durations,
accounting for requested seek origin and playback rate, using the existing
admission implementation as the single rule. At target 16 and rate 1 this is
48 seconds of usable forward media, not three segments. Completed ENDLIST
permits a truly shorter title. Publishing an EVENT label proves none of this.
The planner may need more media before first frame while remaining inside
bounded startup work; do not change the rule to improve a timing metric.

### 5.3 Existing presentation descriptors remain meaningful on clients

The existing web [session](../../crates/plurxd/src/web/player/session.js)
`playbackControlBufferedRange` adds `p.offset` to local media time; the
[transport](../../crates/plurxd/src/web/player/transport.js) computes position
from `offset + currentTime` and has special handling for native pending
positions. These are concrete dependencies of a range-origin presentation,
not evidence that the proposed EVENT descriptor already works.

Trace descriptors through web, Apple and Android attachment, seek/control,
reload and retained replacement. Nonzero-origin EVENT views preserve title
position through seek and freeze; reload cannot reinterpret the old URI as
whole-title VOD or clamp a 90-minute position to its growing local window.
Unknown fields obey H2 negotiation. Server-state tests cannot establish this
client compatibility.
A2 reuses this committed snapshot/admission API for encoded output and retains
its independent actor budgets and short-ENDLIST semantics.

## 6. B1 identity, planning verification and compatibility

### 6.1 V3 identifies planning observations; engine provenance is separate

Define `FRAGMENT_INDEX_CONTRACT` as a versioned **index-planning observation
contract**: selected video/track semantics, timescale and checked source-DTS
normalization; canonical video-only fragment boundaries and wire byte count;
video payload count; duration and clean-cut class; init/promotion constraints;
parameter-set retention and DV conversion recipe. Changes to these quantities
require a new contract ID. A full source digest attests input content; an
index-table digest attests canonical planning measurements. Neither claims
cryptographic equality of output media samples.

```rust
struct FragmentIndexOutputIdentity {
    contract_id: FragmentIndexContractId,
    keyframe_table_sha256: [u8; 32],
}
struct AttestedIndexSource {
    catalog_namespace: CatalogNamespace,
    file_id: i64,
    source_sha256: [u8; 32],
}
enum IndexProvenance {
    LegacyV2 { pipeline_sha256: [u8; 32] },
    OutputV3 { identity: FragmentIndexOutputIdentity,
               engine: EngineProvenance },
}
fn cluster_fragment_index_v3_key(
    source: &AttestedIndexSource, output: &FragmentIndexOutputIdentity,
) -> V3ArtifactKey;
```

Use a domain-separated deterministic encoding of complete ordered canonical
rows, timescale, init/promotion facts and contract version to compute the
keyframe-table digest. Include all planning fields, not merely clean-keyframe
DTS; specify field widths/order and cut labels, with no JSON-map-order hash.
V3 keys contain file identity, full-source digest and output identity, without
engine SHA. Size/mtime remain source diagnostics. Engine/argv/library details
remain provenance and capability checks, not universal cache retirement.
Processed-DV roots remain recipe-keyed because their generated bytes have a
separate output contract.

No new cryptographic sample hash is required: these indexes plan cuts and
byte ceilings, and existing rows do not contain sample hashes. Do not describe
matching lengths as byte-equivalence proof. If a later consumer needs stronger
sample equivalence, it needs a separately versioned contract and actual missing
evidence; that requirement cannot be retroactively assigned to legacy rows or
made a first-frame prerequisite.

### 6.2 Verify every fragment before its segment can become servable

```rust
fn verify_fragment(
    expected: &IndexRow, observed: &CanonicalObservation,
    origin: &VerifiedSyncOrigin, init: &InitContract,
) -> Result<VerifiedFragment, IndexMismatch>;
struct VerifiedSegmentRecord {
    source_generation: SourceGeneration,
    serving_namespace: ServingNamespace,
    contract: FragmentIndexContractId,
    artifact_generation: ArtifactGeneration,
    range_id: RangeId,
    first_row: u64,
    through_row: u64,
    segment_content_id: SegmentContentId,
}
```

Compare normalized source DTS, duration, canonical video-only wire length,
video payload count, cut class and init/promotion constraints. The AV companion
must agree on selected video samples/timing/retention; audio and AV container
length are serving-recipe measurements and do not equal canonical row bytes.
A cold range creates observations under the same rules; indexed/adopted ranges
compare to candidate rows before every commit. `feed_landed` must not clear
verification after first segment publication. Discarded seek fragments remain
part of landing validation; direct seek to segment 80 establishes its own
origin and verifies its own rows. Segment 0 grants no credit there.

Direct cached delivery requires the corresponding committed verification
record, matching immutable segment content ID, exact serving recipe and
current held source binding. A v3 index hit alone proves no cached AV bytes.
A Source witness or local held object token may bind newly produced local
bytes without a full hash; it cannot bless cached bytes from an older object.
On a same-size/mtime replacement, inode/object identity or catalog revision
changes invalidate that local binding. If current identity is ambiguous,
regenerate the demanded range from the held current source or await full
attestation; never return old AV bytes speculatively. Cross-node cache adoption
requires matching full content attestation or an origin-authorized equivalent
object witness, not equality of opaque per-node object-version strings.

A legacy candidate may be used to plan after source/contract compatibility
checks, but missing observations are obtained from the demanded range's common
measurement output before serving. Thus candidate validation/adoption need
not block cold first frame on a whole-file scan, and cannot silently promote a
metadata-only candidate into a fully validated complete artifact.

### 6.3 V2 stays V2; adoption is explicit

Keep `PLRXIDX2`, v2 key helpers and `pipeline_sha256` meanings unchanged.
V2 decode intentionally neutralizes `SourceIdentity` to `(0,0,pipelineSHA)`;
that is portable provenance, never evidence for the current catalog object.
Do not store a contract ID in the old argv/pipeline fingerprint fields.

Add separate v3 header/encode/decode and a dispatcher returning index plus
explicit provenance. Add additive v3 artifact/provenance/candidate metadata
and Store methods in both backends. A validated legacy-blob mapping records
its immutable digest, original pipeline provenance, source attestation,
proposed contract and validation coverage. Lookup legacy candidates by attested
file/source, not only the currently installed engine digest. Never rewrite
legacy blob contents under their old keys.

V3 complete publication requires full coverage under the contract or complete
legacy observations validated against that same contract and attested source.
Pending adoption coverage is not a COMPLETE artifact. Remove unconditional
engine-current invalidation only from paths replaced by source/contract-aware
validation. Preserve engine capability admission for executing a recipe.

Readers ship before writers. Inventory strict internal envelopes; capability
negotiate v3/typed-interest messages, retain enough old/new eligible owners and
avoid sending unknown fields to old decoders. SQLite and Hiqlite receive
additive migrations, startup compatibility cases and explicit writer activation
order. Existing readers may refuse a newer schema even when tables are additive;
rollback uses preserved pre-upgrade state or a proven compatible intermediate
reader, not an assumed binary downgrade. V2 serving remains available in its
compatible cohort during rollout. No user-facing feature toggle is introduced.

### 6.4 Mismatch fences one generation and coalesces one rebuild

In one store transaction: compare expected title/contract head generation;
quarantine that generation; revoke its publisher authority; enqueue/join one
successor Analysis computation preserving live interests. Two late mismatches
racing at segment 80 produce one successor. A stale JobToken cannot publish
the quarantined generation after the transaction. Existing missing-holder
repair of the same immutable artifact is not semantic mismatch recovery.

Fence every affected range sink before additional visibility. Preserve already
verified immutable playable media when its source and serving contract still
hold, or attach a newly verified A1 range at the requested position. A suspect
segment cannot be the fallback. Unrelated titles and other contract generations
keep their cache and serving state; no global engine-digest flush occurs.

## 7. File ownership and implementation order

One owner integrates these steps in one effort PR under the existing gate.
New files follow their module conventions; do not add a second index service.

| Step | Owned implementation surface | Completion property |
|---|---|---|
| 1. Contract and readers | Core `segplan`, `store/fragment_index_cluster`, local `store/fragindex`, v3 decoder/provenance and both store migrations | V2 decoding/key meaning unchanged; V3 deterministic observations; old/new startup and rollback defined before writer activation |
| 2. One computation | Core analysis Store types, SQLite/Hiqlite analysis twins, daemon `state`, background admission/resources, `source_input`/custody | Equivalent pre-hash requests join; typed Source authority survives; physical reads charged once; observers hold no slot |
| 3. Common measurement and range sink | Daemon `fragindex`, `fragment_index_cluster`, `vodgen`, `vod/generation`, `renditiondir`; core `fmp4`/`segplan` where shared extraction requires it | Seeked and sequential canonical rows equal; every segment has a source-bound verified checkpoint; stale writer cannot commit |
| 4. Growing attachment and admission | `vod/session`, `vod/shared`, `vod/serve/create`, manifest/retained paths and existing A1-N publication helper | One coherent view; 48-second native rate-1 case; fixed target; explicit oversized-GOP/init outcomes; held EVENT views stable |
| 5. Source and client integration | `transcode/source_actor`, Source create path, receiver relay, shared descriptors and relevant web/Apple/Android attachment/seek clients | Source scanner removed; one engine and file computation; grants/namespaces isolated; correct nonzero-origin time and first reload |
| 6. Seal, adoption and quarantine | Analysis→Job store submission twins, fenced publication, cluster hydrate, per-fragment verifier and cached delivery | No retired-fence writes; complete-only publication; legacy adoption without engine-key retirement; one scoped rebuild |
| 7. Reconciliation and A2 handoff | Existing start plan/status docs, producer ownership inventory, control-wire corpus, encoded publication consumer | Exact final behavior/metrics/regressions recorded; encoded caller shares admission without losing actor budget |

Complete each boundary before activating dependent writers. Preserve unrelated
active work; partial wiring is not implementation completion.

## 8. Regression properties and fault cases

These properties are **not executed tests**. Add regressions at real callers
and stores, recording actual `Regression-Test:` selectors in the PR. This
design task runs no tests/builds.

| Surface | Required regression and counterfactual |
|---|---|
| Both Store backends | Two ingresses/targets and matching source/contract join one Analysis owner; legacy identities survive migration; ambiguous source versions never merge. Old target-keyed uniqueness creates two and must fail. |
| Authority and capacity | Sharing principal cannot become local demand; grant revocation cancels only its namespace; receiver never indexes an origin ID locally; one foreground input is not double-charged; observer opens no custody; independent range positions obey capacity. |
| Lifecycle | Lost heartbeat, source-reader loss, last-viewer departure, takeover and ambiguous submit acknowledgment join/settle children correctly; Analysis→Job transfer admits exactly one future writer; old Analysis and stale Job tokens fail publication. |
| Sparse resume | Cold seek at 90 minutes performs bounded sync work, no sequential prefix scan/hash; negative DTS, B-frames, nonzero start, fractional time base and wrong-GOP landing verify correctly; restart after segment rename or partial manifest exposes only complete segments. |
| Canonical rows | Background whole-title, first-play and nonzero seek ranges have exact equal canonical row encodings on overlapping samples, post-DV conversion; AV wire length differs without changing canonical bytes; overflow and AV/measurement disagreement refuse commit. |
| Publication | Exact hlsjs short window; native/missing/unknown transport needs three target durations from seek/rate-adjusted usable media; 21.897 seconds with target 16 remains insufficient; short ENDLIST passes; later long GOP never mutates target or exposes invalid segment. |
| Client views | Held EVENT range remains append-only through freeze and reload; second-position view remains independent; web and native descriptors report title position on attach, seek, replacement and ENDLIST; old/new decoder paths work. |
| Identity and verification | V2 neutral identity unchanged; old-engine candidate adopted by source/contract; contract bump misses; valid segment 0 then direct seek 80 mismatch is blocked before serving; cached old AV rejected after same-size/mtime replacement; no cross-node object-version assumption. |
| Quarantine | Simultaneous late mismatches fence publishers and join one rebuild; unrelated warm title survives; verified playable fallback remains; suspect range cannot be fallback. |
| Create ledger | One planning snapshot/pure decision; ≤2 durable mutations, no pre-answer full hash/advisory work; cross-ingress pending replay retains stable attempt; no second copy engine on index-pending. |

Review the diff and touched outlines. Update affected entries in
[producer ownership](../../tests/playback/rolling-producer-owners.toml);
new outcomes extend [wire conformance](../../tests/validation/test_control_wire_conformance.py)
with old/new decode coverage. Exercise [store contracts](../../crates/plurx-core/tests/store_contract.rs)
on both backends and include `hiqlite-store` for replicated-core regressions.
Use the established [compiler and final-check loop](../DEVELOPMENT_PIPELINE.md).
Retain valid passing results across rebases; rerun failures/changed coverage.

## 9. Implementation and hardware acceptance remain owed

The owner accepted the Analysis coordination/serving-actor range bridge,
one-input measurement recipe, fixed target 16 and typed target/init outcomes
on 2026-11-09. The critical implementation proof remains §4.1 exact canonical
row equality under nonzero input seek; failure requires changing the recipe,
not weakening the contract. Client EVENT compatibility is required, not proven.

After implementation checks and deployment, record matched cold/warm title,
source revision, client/transport, seek position, rate, contract, artifact
digest, ingress/owner, attempts and outcome in the continuation ledger.
Measure these as separate evidence:

| Evidence | Required observation |
|---|---|
| Startup | Eligible cold starts ≤5 s share, successful p90 and failures; cold/warm and native/hlsjs separate; actual first frame/first reload, 90-minute Resume and title timeline on web/Apple/Android. |
| Reuse | First-play complete artifacts attested and reused; same title/artifact across engine deploy with unchanged contract; hits/adoptions/verifications/rebuilds counted. |
| Isolation | Injected late mismatch gives one rebuild, unrelated warm title survives; no duplicate computation/observer input; Source grant revocation, exact-object replacement and relay cancellation observed. |

Before/after `/metrics` cohorts and hardware receipts remain owed.
