# Decoder selection and recovery — an explicit plan for each producer

**Status:** implementation in progress; follow the live evidence and PR ledger
in [DECODER_SELECTION_RECOVERY_STATUS.md](DECODER_SELECTION_RECOVERY_STATUS.md)
· **Written:** 2026-09-05 · **Implementation baseline:** `main` at
`3d847b58b081dcb15a8d2e566d8d0ac1700882fd`

Companion to [AVI_VIDEOTOOLBOX_REVIEW_DECISION.md](AVI_VIDEOTOOLBOX_REVIEW_DECISION.md)
(the bounded MPEG-4 compatibility fix),
[PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md)
(the existing playback owner and replacement protocol), and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) (how to compile, review,
and qualify the work). This document specifies the broader decoder policy,
resource accounting, diagnostics, recovery, and cache changes. Execute the
milestones in §12 against the current intended base. Symbol names below are
source anchors; re-verify their definitions as the playback effort moves.

## 1. Decision — resolve decode independently and recover through one owner

Build a resolved transcode plan that explicitly selects the decoder,
renderer, and encoder. Use that same plan for FFmpeg arguments, output
identity, admission, diagnostics, and recovery. Add video decoder health as
an attempt-scoped input to the existing playback controller. Permit one
automatic producer recovery per playback recovery epoch, using a frozen
alternative and preserving the requested presentation contract.

For a hardware-decode failure, prefer software decode with the same hardware
encoder. If compatible rendering requires a different filter graph, select
and validate that graph before freezing the alternative. A decoder failure
must not automatically disable a working encoder.

Before media publication, install the alternative through the existing
fenced child replacement transaction. After publication, prepare a new
session and use the existing client replacement protocol. Never rewrite the
published timeline in place. A client without qualified replacement support
receives an explicit terminal failure instead of an uncontrolled reopen loop.

The final system must satisfy these invariants:

1. **One effective plan describes one attempt.** Command generation cannot
   re-read environment variables or infer a decoder from an encoder label.
2. **Compatibility precedes performance.** Required metadata, pixel formats,
   and surface ownership cannot be traded for startup speed.
3. **Published bytes keep their identity.** Recovery creates a new producer
   attempt or a new session according to the publication boundary.
4. **Progress does not erase decoder failure.** Segments appearing and a
   successful process exit cannot override a latched health fault.
5. **Recovery is bounded across replacement.** New attempts, sessions,
   request retries, and owner handoffs do not replenish the automatic budget.
6. **CPU decode consumes CPU admission.** Hardware encoding is not evidence
   that the whole pipeline avoids CPU work.
7. **Reusable output carries completion evidence.** Cache identity and
   health qualification survive beyond the lifetime of the child process.

The MPEG-4/VideoToolbox compatibility exclusion from the incident fix is the
first explicit rule in this policy. It remains container-independent.

### 1.1 Chosen approach and rejected alternatives

| Approach | Decision | Reason and trade-off |
|---|---|---|
| More encoder match guards | Replace with a resolved plan | Guards are useful incident mitigations but leave cache, admission, and recovery to infer different answers |
| One universal `heavy_source` threshold | Retain only as a temporary legacy preference | It is not a codec capability or a general CPU-cost model |
| Explicit plan plus typed health events | Adopt | Fits current FFmpeg subprocesses and playback ownership; adds types, evidence, and bounded state |
| Retry from the stderr task | Reject | Creates another lifecycle owner and races cancellation and publication |
| Global `-xerror` | Reject as the recovery mechanism | It does not isolate video decoder failure from unrelated audio errors |
| Replace the FFmpeg CLI with libavcodec now | Defer | Structured frame-level telemetry would be stronger, but this would replace a substantial process integration layer |
| Automatically ban a backend across the fleet after one failure | Defer | A stream-specific failure does not establish a node-wide hardware defect |

This design detects qualified decoder diagnostics. It does not prove that
every emitted pixel is correct or detect silent corruption without evidence.
Real-media and visual qualification remain necessary.

## 2. Current contracts — reuse the existing boundaries

The baseline contains the following relevant interfaces and behavior.

| Surface | Existing contract | Required change |
|---|---|---|
| [Core transcode](../crates/plurx-core/src/transcode/mod.rs) | `decode_setup(encoder, source)` reads `PLURX_HWDECODE`; `hls_args` applies renderer overrides | Replace implicit selection with a resolved plan |
| [Renderer selection](../crates/plurx-core/src/transcode/pipeline.rs) | `requires_software_decode`, `decode_args`, `for_session`, `fallback` describe parts of the contract | Expose requirements to one candidate validator |
| [Encoder inventory](../crates/plurx-core/src/transcode/encoder.rs) | `detect_video_decoders` inventories portable decoder names; encoder probes prove output encoding | Add independent input-backend evidence; do not reinterpret the existing list as hardware capability |
| [Media facts](../crates/plurx-core/src/domain.rs) | `MediaFile` stores codec, profile, dimensions, bit depth, HDR/DV facts | Obtain selected stream index, pixel format, and frame rate as additional bound probe facts |
| [Recipe identity](../crates/plurx-core/src/transcode/recipe.rs) | `Recipe::hash` includes encoder/build/filter choices | Include stable decode and health-qualification contracts |
| [Process execution](../crates/plurxd/src/transcode.rs) | `spawn_ffmpeg` separately drains progress and logs stderr | Return owned observer completion and feed typed health observations |
| [Retry recipe](../crates/plurxd/src/transcode.rs) | `PrepublicationTranscodeRetry::build` freezes one alternative | Freeze reason-specific alternatives that share one recovery budget |
| [Actor](../crates/plurxd/src/playback_control.rs) | `RollingProducerEvent` contains progress, exit, and flow observations | Add durable-in-ingress health fault and diagnostic completion events |
| [Resource admission](../crates/plurxd/src/admission.rs) | Hardware slots and software permits exist; `LiveAdmission` can hold both fields | Make joint ownership and transition explicit; stop using encoder family as the resource decision |
| [Durable replacement](../crates/plurx-core/src/store/mod.rs) | Preparation is keyed by `(user_id, playback_id)`; commit fences the expected predecessor | Carry the recovery reservation and frozen alternative through preparation and commit |
| [Replacement delivery](../crates/plurxd/src/http/hls.rs) | Prepares `PreparedSuccessorAction` for control exchanges | Connect decoder failure to preparation; qualify acknowledgment and commit paths on the implementation base |
| [Offline production](../crates/plurxd/src/transcode.rs) | Produces and resumes content-addressed parts | Require health evidence per retained part and assembled generation |

Two current facts matter for implementation. First, the retry executor uses
`SwPool::take_forced` and `demote_to_software` for its software alternative.
Neither operation is a correct model for retaining a hardware encoder while
adding CPU decode work. Second, `GenerationManifest` authenticates object
bytes; its current fields do not attest to decoder health.

The baseline's hardware startup budget is 12 seconds and software budget is
30 seconds. These are existing actor constants, not measurements showing
that a mixed pipeline fits one category. Add an explicit mixed-work budget
selection without silently changing deadlines for existing plans.

## 3. Data model — separate media facts, decisions, and execution state

### 3.1 Bind additional probe facts to the selected source

Introduce an immutable `DecodeFacts` snapshot in the daemon's preparation
path, backed by the existing held source identity. Obtain missing facts with
the configured FFprobe binary and the same bound-source discipline used by
production. Do not probe a mutable pathname after preparing a held source
and then assume both refer to the same bytes.

| Fact | Representation | Unknown or invalid handling |
|---|---|---|
| Video stream | Absolute input stream index and selection provenance | Do not attribute diagnostics to `0:0` by assumption; cover attached artwork and multiple video streams |
| Codec and profile | Canonical codec name and optional profile | Do not infer codec from container extension |
| Pixel layout | Optional pixel format and chroma sampling | An absent layout cannot prove a hardware candidate compatible |
| Geometry | Optional positive width and height | Zero, negative, or overflowed values are unknown |
| Frame rate | Optional reduced positive rational plus provenance | `0/0`, invalid values, and uncertain VFR estimates remain unknown; do not silently use 24 fps |
| Depth and HDR/DV | Existing typed source facts plus probed layout | Preserve existing metadata requirements and conservative ten-bit handling |
| Source identity | Existing bound source fingerprint plus facts digest | A changed source or selected stream invalidates the prepared plan |

Persisting new columns in `MediaFile` is not required for the first release.
Use a node-local bounded probe cache keyed by source identity and FFprobe
build, with existing source invalidation. This avoids an immediate scan/store
backfill. If later persisted, update both storage backends and scan contracts
as a distinct migration, preserving unknown values.

FFprobe supports selective stream entries and JSON output; use those to bound
the returned facts rather than parsing display text.
[FFprobe documentation](https://ffmpeg.org/ffprobe.html).

### 3.2 Proposed core planning interface

Create `crates/plurx-core/src/transcode/decode.rs` for pure planning types and
rules. The following signatures are proposed interfaces, not existing code
or a patch claimed to compile:

```rust
pub enum DecodeBackend {
    Software,
    VideoToolbox,
    Cuda,
    Qsv,
    Vaapi,
}

pub enum DecodeEvidence {
    Qualified,
    LegacyUnverified, // Migration mode only; never label this as proved.
}

pub struct ResolvedDecode {
    backend: DecodeBackend,
    software_decoder: Option<String>,
    input_video_stream: u32,
    surface: DecodeSurfaceContract,
    reason: DecodeReason,
    evidence: DecodeEvidence,
    policy_revision: u32,
}

pub struct ResolvedTranscode {
    decode: ResolvedDecode,
    encoder: Encoder,
    options: TranscodeMediaOptions,
    source_facts_digest: String,
    output_contract: PresentationContract,
}

pub fn resolve_transcode(
    request: &TranscodeRequest,
    facts: &DecodeFacts,
    capabilities: &DecodeCapabilities,
    policy: &DecodePolicySnapshot,
    restrictions: &AttemptRestrictions,
) -> Result<ResolvedTranscode, PlanError>;
```

Keep construction private to validation, expose read-only accessors, and do
not provide a `Default` that represents an unvalidated plan. The named
support types above must be introduced or adapted from existing contracts;
they are not assertions that such types already exist.

`DecodeSurfaceContract` describes software frames or a specific hardware
surface family, download/upload transitions, compatible pixel formats, and
required side data. `PresentationContract` includes output codec/profile,
dynamic range and color, geometry, selected tracks, subtitle rendering, and
A/V correction. Reuse the existing presentation fingerprint machinery
rather than introduce a competing output-grade definition.

`AttemptRestrictions` records a frozen backend exclusion after failure.
After successful recovery, the same restriction is retained durably for
ordinary playback continuation as specified in §8.2; it is not limited to
the immediate retry transaction.
`DecodeReason` is a stable bounded enum, including renderer requirement,
compatibility exclusion, operator software override, measured preference,
and legacy preference. Display prose is derived from the enum.

`ResolvedTranscode` describes media semantics. Separate execution parameters
hold paths/descriptors, pacing, start position, attempt ID, resource permits,
and actual thread caps. Retries at another position reuse the frozen
semantic alternative with a newly fenced execution context.

`TranscodeMediaOptions` is the proposed semantic subset of today's
`TranscodeOptions`. Split execution-only fields such as start position,
object start number, subtitle sidecar path, and thread allocation into
`TranscodeExecution`; preserve their current behavior while migrating callers.
Do not retain two independently mutable copies of renderer or track choices.

### 3.3 Change command construction so it cannot resolve policy again

Replace the current public builder shape:

```rust
pub fn hls_args(
    source: &MediaFile,
    encoder: Encoder,
    opts: &TranscodeOptions,
    pacing: Pacing,
    out_dir: &str,
) -> Vec<String>;
```

with a proposed resolved-plan builder:

```rust
pub fn hls_args(
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
) -> Vec<String>;
```

Migrate live, retry, speculative, offline, and test callers. A temporary
adapter may resolve the legacy policy during the neutral migration milestone;
remove it before enforcement is complete. Production cache and command paths
must not independently call that adapter.

An explicit software plan selects an inventoried software decoder and disables
video hardware acceleration. Put decoder selection and input thread options
before the relevant input, and encoder options after it. Do not confuse a
codec family such as AV1 with the actual installed decoder implementation.
Validate precise arguments against each qualified FFmpeg build. Requested
hardware decode is recorded as requested; observed backend remains unknown
unless the build supplies reliable evidence.

## 4. Selection algorithm — filter for correctness, then choose a preference

### 4.1 Snapshot policy once and enumerate valid complete pipelines

Read operator policy, node evidence, source facts, and requested output at
preparation time. Parse `PLURX_HWDECODE` into the snapshot; preserve the
existing `off`, `0`, `false`, and `no` spellings. Other values preserve the
current automatic behavior and produce bounded configuration diagnostics
where appropriate. No command builder reads environment state.

```text
 bound source + request + policy + capability snapshot
                         │
                         ▼
       enumerate decoder / renderer / encoder candidates
                         │
                         ▼
       remove incompatible metadata, layouts, surfaces
                         │
                         ▼
       apply compatibility exclusions and operator policy
                         │
                         ▼
       require candidate capability evidence for enforcement
                         │
                         ▼
       choose established preference; freeze valid alternative
                         │
                         ▼
       admit resources → execute the exact resolved plan
```

Renderer requirements and explicit software policy are simultaneous
constraints. If the preferred vendor graph requires hardware surfaces while
the operator requires software decode, choose a qualified renderer with the
same presentation contract that accepts software frames. If none exists,
return a typed incompatibility. Do not ignore the operator switch and do not
remove hardware input from a graph that still requires it.

For Dolby rendering, absence of a grade-preserving software-compatible
alternative is a terminal planning result, not permission to fall back to
an ordinary SDR graph. Device initialization needed by the encoder can remain
even when input decode is software; “no hardware decode” does not mean “no
hardware device anywhere.”

### 4.2 Capability evidence is independent of encoder detection

Maintain a node-local `DecodeCapabilities` snapshot keyed by actual FFmpeg
build, backend, device identity/driver environment where available, and
codec/profile/layout class. Represent availability and qualification
separately: `Unavailable`, `Advertised`, `Qualified`, and `Rejected` are
different facts. A successful synthetic probe qualifies the tested class,
not every file in the codec family.

Use the node's existing probe infrastructure and bounded startup/background
work. Fixtures must exercise decode and the required surface transfer,
correct dimensions/layout and metadata, and comparisons to a software
reference. A compiled decoder name or successful output encoder probe is
not this evidence. Keep absolute source paths and device handles out of
cluster advertisements.

Do not run an exhaustive probe suite synchronously on every play. Reuse
build/device-qualified evidence, refresh after identity changes, and use
bounded on-demand fact collection only for the input facts actually needed.
Unknown capability cannot silently become qualified. In enforced mode choose
a valid software alternative, or return a typed capability/capacity result.

### 4.3 Preserve existing preferences before making performance changes

During the neutral migration, encode existing policy explicitly: ordinary
VideoToolbox/CUDA choices, QSV/VAAPI's existing workload guard, renderer
ownership, and the MPEG-4 compatibility fix if it has landed. Mark any
unproved inherited candidate `LegacyUnverified`. Hard compatibility and
metadata constraints still apply.

Do not broaden the CPU-default rule as part of extraction. After qualification,
change preferences only for measured classes. Consider source pixel rate,
profile/depth/layout, renderer cost, and node capacity. A cropped 3840×1600
source is not classified as inexpensive merely because its height is below
2160. Unknown frame rate is not permission to underestimate work.

**Final-state rule:** enforced nodes never rely on `LegacyUnverified` as
their hardware qualification. Nodes awaiting qualification remain explicitly
in migration mode until their supported workload matrix is covered.

## 5. Admission — mixed pipelines own both resource reservations

Introduce a `TranscodeResourceEstimate` and a single `TranscodePermit` bundle
covering hardware capacity plus CPU decode, software filters, and software
encode work. Keep weights in the existing CPU pool's units; document that
they are conservative concurrency estimates, not hard CPU utilization caps.

For the first enforced implementation, use the existing
`Workload::software_threads()` estimate as a conservative **total** CPU
reservation for a mixed software-decode/hardware-encode pipeline. This
intentionally overestimates some light inputs and avoids inventing an
unmeasured decoder cost model. For all-software pipelines retain the current
total estimate; do not add another full encode estimate on top. Qualify and
reduce mixed weights by measured class in a later preference change. Include
known CPU filter work for hardware-decode plans in that resource inventory.

New or additional CPU reservations use bounded admission, including the
existing documented empty-pool exception. Do not extend unconditional
`take_forced` to every decode recovery. Waiting or refusal is an explicit
capacity result. Account for background yielding and use the existing
startup/install deadline instead of adding an unlimited recovery wait.

Joint acquisition must not hold one scarce permit while indefinitely waiting
for another. Try the full bundle atomically under coordinated admission or
release partial reservations before a bounded retry. Document one lock order;
never await while holding the admission mutex.

For an in-place recovery retaining the encoder, reap the old child, retain
the required hardware reservation, and acquire the CPU delta before spawn.
For a new postpublication session, transfer reservation ownership only after
the failed producer is reaped, or acquire a separate bundle within capacity.
One hardware slot must never authorize two live encoder processes. Serving
retained old segments needs no encoder permit.

Separate decoder thread caps from output encoder caps. A cap is passed only
where that decoder supports it and cannot be confused with the current
output `software_threads`. Do not claim a thread count caps all FFmpeg filter
or library threads. Measure contention under concurrent workload acceptance.

Select the existing 30-second software startup category for newly introduced
mixed software-decode recovery until a measured class justifies a smaller
budget. Preserve existing deadlines for unchanged initial plans. Resource
accounting, startup classification, and cache semantics are distinct fields.

## 6. Identity — plan semantics and health qualification survive caching

### 6.1 Extend the recipe with stable decoder semantics

Add a decode identity to the common `Recipe` constructor: actual software
decoder implementation or requested hardware backend, frame-domain and
metadata contract, semantic policy revision, and the health qualification
contract required for reuse. Continue hashing the existing encoder, renderer,
FFmpeg build, source identity, and output options.

Exclude attempt/session IDs, paths, thread reservations, timestamps, human
reason text, evidence timestamps, and physical device instance identifiers.
If a device-family distinction changes qualified output semantics, represent
that as a stable compatibility class rather than a per-node UUID.

Use a new recipe namespace/version for fully planned and health-qualified
production. Retain the targeted MPEG-4 revision during the incident release;
the broader project deliberately accepts a new cache namespace when the new
qualification contract turns on. No migration mode may label old artifacts
as health-qualified without reprocessing them. Exact version numbers must
be allocated against the current base to avoid concurrent version collisions.

A retry that changes decode or rendering has a different key. It must not
resume an old producer's prefix or publish under the failed plan's key.
Do not equate identical source and output codec with identical production.

### 6.2 Add authenticated producer-completion evidence

Add a bounded `ProducerHealthReceipt` associated with each retained part and
the assembled generation. Its proposed fields are:

| Field | Meaning |
|---|---|
| `receipt_version` | Version of the validation contract |
| `plan_digest` | Semantic producer plan used for these bytes |
| `diagnostic_contract` | Build-qualified parser/rules revision |
| `observation_complete` | Stderr was fully drained and no coverage loss occurred |
| `video_decode_error_records` | Count of qualified error records, not failed frames |
| `terminal_fault` | Optional bounded failure code |
| `exit_disposition` | Clean end, intentional scheduler yield, or failed termination |
| `qualification` | `qualified`, `unqualified`, or `rejected` |

Authenticate the receipt through the generation manifest's digest/object
inventory. An unauthenticated adjacent JSON file cannot authorize a cache
hit. Bind each part receipt to its part bytes/digest and the common plan;
validate every part again before assembly. Update both local and shared
cache publication/read paths, retained-prefix recovery, and offline packages.

For this release, reusable qualified output requires complete observation,
zero recognized selected-video decode error records, no terminal fault, and
the existing process/mux/duration/integrity checks. This is intentionally
stricter than live playback tolerance. A single recoverable video error may
allow live playback to continue while making that artifact unsuitable for
qualified cache reuse. Audio warnings alone do not fail this video receipt.

On failure, do not promote staged output. Keep any published objects needed
by existing delivery leases until ordinary retirement; prevent new reuse.
A restart during production leaves missing/incomplete receipts unqualified.
A late success exit never clears a previously latched error.

Intentional background preemption is distinct from decoder failure. A yielded
part may retain structurally complete immutable segments when its diagnostics
were fully drained, no selected-video error occurred, and the existing part
validation passes. Its receipt records intentional yield, not end-of-input
success. Incomplete trailing segments are discarded. Final assembly still
requires the existing whole-output completion contract. This preserves safe
resume without treating every interrupted part as a finished movie.

Qualified readers reject absent or unsupported receipt versions and fall
back through normal production/placement behavior. During mixed-version
rollout, old writers use old keys; new readers do not guess compatibility.
Rollback can disable automatic recovery while preserving the new identity
and rejection rules. Rolling back the whole binary also rolls back these
protections and requires an explicit operational decision.

## 7. Health observation — count qualified video errors, not log volume

### 7.1 Own the observer lifecycle with the child

Create `crates/plurxd/src/decoder_health.rs` for the diagnostic grammar,
bounded accumulator, and receipt construction. Change the subprocess wrapper
to return a proposed `ObservedFfmpeg` handle that owns the child, progress
reader, stderr reader, health accumulator, and final diagnostic completion.
The session/producer remains the owner of that handle. Reader task failure
must be observable; detached best-effort logging is insufficient for cache
qualification.

Use `-loglevel repeat+level+error` for builds qualified for this grammar and
disable terminal coloring for the child. The `repeat` flag prevents FFmpeg
from compressing repeated messages, while `level` supplies severity labels.
[FFmpeg logging documentation](https://ffmpeg.org/ffmpeg.html).
Qualify the arguments against the deployed Jellyfin builds before enabling
this parser; do not assume upstream documentation proves fork behavior.
Automatic action must name a versioned diagnostic contract bound to the exact
FFmpeg version, binary/build identity, codec/decoder family, log flags,
selected-stream contexts, severity position, and retained fixture. A tolerant
structural match without that external build receipt is observation only.

Keep raw diagnostic logging redacted through the existing logger. Rate-limit
displayed logs after classification so log suppression cannot erase health
evidence. Use a bounded byte reader with a proposed 16 KiB maximum retained
line and discard oversized-line tails while continuing to drain. Oversized
or malformed diagnostic input marks observation coverage incomplete for
cache purposes; it is not by itself proof of a decoder fault.

Retain counters and the small rolling failure window, not an unbounded event
history. Saturate counts safely. Publish at most one terminal health latch
per attempt plus bounded summaries and final completion; a log flood must
not fill the actor mailbox or block FFmpeg's stderr pipe.

### 7.2 Define a grammar with stream and stage attribution

Qualify a grammar for each supported FFmpeg diagnostic family using retained
real logs and controlled fixtures. The selected absolute input stream index
from `DecodeFacts` is authoritative. Match FFmpeg diagnostic structure and
stage, not arbitrary occurrences of words such as “error,” “green,” or a
codec name in a pathname.

| Diagnostic class | Treatment |
|---|---|
| Selected-video decoder packet/frame submission failure | Count one primary video error record |
| Subordinate `No frame decoded?` belonging to that failure sequence | Retain as supporting diagnosis; do not count it as an additional failed frame |
| Qualified hardware decoder initialization/device failure | Emit a typed backend fault when the grammar proves decode-stage attribution |
| Error on audio or an unselected stream | Log separately; do not consume the video recovery budget |
| Encoder, muxer, or I/O failure | Keep existing failure classification; do not pretend software decode will repair it |
| Unattributed or unknown diagnostic | Log as unclassified; preserve existing process/progress failure detection |
| Truncated, unreadable, or unsupported diagnostic stream | Mark observation incomplete; disallow a qualified cache receipt |

The sole M0 automatic-action family is the build-bound host FFmpeg 8 rawvideo
shape `[vist#…/rawvideo @ ADDRESS] [dec:rawvideo @ ADDRESS] [error] Error
submitting packet to decoder`. It qualifies the parser boundary, not a deployed
producer. The retained #913 FFmpeg 7.1.4 MPEG-4 records omit severity and remain
useful for observation only. The deployed FFmpeg 5.1 top-level stream error
does not name the selected stream and is likewise unqualified. Add other
families only with exact build/codec contracts and fixtures demonstrating
attribution. A bare `No frame decoded?` without reliable context is supporting
evidence, not an independent auto-retry trigger.

For legacy captures containing `Last message repeated N times`, including a
severity-prefixed form, attach the summary only to an unambiguous preceding
qualified record and label its timing/count provenance. Do not synthesize N
distinct failure timestamps or double-count an expanded capture. Any repeat
summary proves the stream is compressed and disqualifies the entire attempt
from automatic action, even when the repeated record was audio, unselected,
or subordinate. Live automatic enforcement requires the uncompressed grammar;
legacy compressed fixtures exercise observation and diagnosis but do not
pretend to supply precise window timing.

FFmpeg `-progress` frame/time values describe output progress, not a reliable
denominator of attempted input decodes. Do not report a decoder error
percentage by dividing diagnostic records by those counters.

### 7.3 Specify the first threshold and its qualification gate

Use the following **proposed initial policy constants**, subject to the
retained-log qualification milestone before enforcement:

| Constant | Proposed value | Meaning |
|---|---|---|
| `VIDEO_DECODE_ERROR_WINDOW` | 2 seconds | Rolling monotonic observation-time window |
| `VIDEO_DECODE_ERROR_LIMIT` | 5 records | Fifth primary selected-video error within the window latches a fault |
| `AUTOMATIC_PRODUCER_RECOVERY_LIMIT` | 1 | Shared across initial retry and later automatic replacement for one recovery epoch |
| `DIAGNOSTIC_DRAIN_BUDGET` | 2 seconds after process exit | Bounded wait for final stderr observation before completion qualification |
| `MAX_DIAGNOSTIC_LINE_BYTES` | 16 KiB | Memory bound for retained line parsing |

These are implementable starting values, not existing repository constants
or experimentally validated thresholds. M0 must replay #913-like failures
and tolerant control streams. If they discriminate poorly, change the
versioned policy and record the resulting numbers and evidence. Do not
enable enforcement with an unresolved threshold marked “TBD.”

The accumulator counts only primary errors within `(now - 2 seconds, now]`.
It prunes old timestamps before insertion and needs to retain only five
timestamps. One through four records indicate suspicion; the fifth latches
`VideoDecodeFailure`. A qualified fatal decode-backend error latches
`DecodeBackendUnavailable` immediately. Successful progress does not reset
either latch. A new attempt gets fresh counters; a pause does not clear a
fault that already latched. Raw wall-clock receipt timing is an approximation
of when FFmpeg failed, so tests must include pipe buffering and log bursts.

Isolated errors below the threshold do not restart live playback. They still
make the attempt's reusable output unqualified under §6.2. Unknown diagnostic
families cannot be advertised as fully monitored: enable qualified cache
production only for supported diagnostic contracts.

### 7.4 Make fault and completion observations non-lossy

Proposed observations added to `RollingProducerEvent` are:

```rust
pub struct ProducerDecodeFault {
    pub producer_attempt: u64,
    pub plan_digest: String,
    pub fault: DecodeFaultKind,
    pub input_video_stream: u32,
    pub primary_error_records: u64,
    pub diagnostic_contract: u32,
}

pub struct ProducerDiagnosticsComplete {
    pub producer_attempt: u64,
    pub plan_digest: String,
    pub receipt: ProducerHealthReceipt,
}
```

These are proposed type shapes. Add bounded serializers/status mappings as
needed; raw stderr and source paths do not enter the actor's operational
snapshot. The ingress assigns a sequence and monotonic publication time,
as it does for existing producer events.

Use an attempt-scoped sticky fault slot/barrier that cannot be overwritten
by progress compaction. Draining progress, authorizing first publication,
classifying exit, and authorizing reusable output must first observe all
preceding health barriers. A health event has precedence over later success
facts for the same attempt. The decision commits once; duplicate redelivery
returns the same decision or is acknowledged as already settled.

Keep diagnostic completion separate from process exit. Before accepting a
successful completed artifact, wait for stderr EOF and final accumulator
receipt. If the drain budget expires or a reader dies, reject qualification
and settle cleanup without hanging. A forced cancellation or failed attempt
does not wait for a success receipt to terminate the child.

Live publication cannot prove that FFmpeg has no future or buffered error;
this design prevents publication after an already-observed failure barrier.
A late fault after bytes were published takes the postpublication path.
Do not claim that stderr observation makes first-frame publication a visual
correctness guarantee.

## 8. Recovery — freeze alternatives and share one automatic budget

### 8.1 Classify the cause before choosing an alternative

Replace the single implicit fallback recipe with a bounded set of frozen
alternatives selected by typed failure class. The set contains at most a
decoder alternative and the existing general alternative; selection consumes
the same one-shot budget. It is not a chain that tries both in succession.

| Failure and current plan | Decision |
|---|---|
| Qualified video decode fault on hardware decode | Choose frozen software-decode plan with same encoder, if valid |
| Hardware decode plus vendor filters requiring hardware frames | Choose frozen software-compatible renderer with identical presentation contract and same encoder, if qualified |
| Video decode fault on software decode | No decoder alternative; fail with an actionable decode error |
| No grade-preserving renderer or usable software decoder | Fail with explicit incompatibility; do not change output grade |
| Encoder/process/progress failure without decode attribution | Preserve existing reason-specific general fallback, subject to the shared budget |
| CPU capacity unavailable or install deadline exceeded | Fail as recovery capacity/deadline failure; do not allocate unbounded forced permits |
| Budget already consumed | Surface terminal recovery exhaustion; do not restart under a fresh session ID |

Freeze semantic alternative selection before the initial producer is
admitted. Include its plan digest and presentation fingerprint in
`ValidatedRetryRecipe`. For prepublication execution, freeze concrete
arguments after the execution directory is known. Postpublication recovery
may bind a current fenced position and new descriptors to the same semantic
alternative; it may not rerun unconstrained policy and choose the failed
backend again.

Keep encoder rate control unchanged for a decode-only alternative. Only a
general alternative that actually changes encoder family resolves that
family's validated rate control. Verify geometry, grade/color, track selection,
subtitle rendering, and A/V correction as a complete contract rather than
compare the encoder label alone.

Add typed reasons to `ProducerDecisionReason` and every exhaustive mapping:
`VideoDecodeFailure`, `DecodeBackendUnavailable`, `DecoderRecoveryExhausted`,
and an appropriate recovery-capacity/observation-incomplete result. Preserve
existing permanent-source versus transient-failure meanings. A hardware
decoder fault is not automatically a permanently unsupported file; exhausted
automatic recovery is terminal for the current epoch without asserting that
the media can never play.

### 8.2 Make the budget survive attempts, sessions, and node handoff

Introduce a server-owned `recovery_epoch` associated with the logical
playback. A deliberate new user playback creates it; automatic reopen,
retry, seek, track change, and prepared successor inherit it. Changing a
request ID or presenting a new client session ID does not mint a new budget.
For clients lacking a durable logical-playback context, enable only the
existing in-session recovery scope until this identity can be preserved.

Use the existing `(user_id, playback_id)` authority plus the epoch to persist
the single reservation. Add the following proposed table through the common
SQLite/Hiqlite migration conventions:

```sql
CREATE TABLE media_session_producer_recovery (
    user_id                 INTEGER NOT NULL,
    playback_id             TEXT NOT NULL
        CHECK (length(playback_id) BETWEEN 1 AND 128),
    recovery_epoch          TEXT NOT NULL
        CHECK (length(recovery_epoch) BETWEEN 1 AND 128),
    failed_incarnation_id   TEXT NOT NULL,
    failed_producer_attempt INTEGER NOT NULL CHECK (failed_producer_attempt > 0),
    decision_sequence       INTEGER NOT NULL CHECK (decision_sequence > 0),
    failed_plan_digest      TEXT NOT NULL CHECK (length(failed_plan_digest) = 64),
    alternate_plan_digest   TEXT NOT NULL CHECK (length(alternate_plan_digest) = 64),
    decode_restriction      TEXT
        CHECK (decode_restriction IS NULL OR
               length(CAST(decode_restriction AS BLOB)) <= 4096),
    state                   TEXT NOT NULL
        CHECK (state IN ('reserved', 'installed', 'exhausted')),
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL,
    PRIMARY KEY (user_id, playback_id, recovery_epoch)
) STRICT;
```

This is proposed schema, not an allocated migration number. Persist the epoch
on the durable playback/incarnation/preparation records that need to carry it;
adapt domain types, store contracts, and both backends together. Bind the
frozen alternate's serialized semantic plan to the preparation using the
existing validated payload contract, with a digest and explicit size bound.
The table is the budget/decision ledger, not a second session lifecycle.

`decode_restriction` is a versioned, validated serialization of an optional
`ContinuationDecodeRestriction`: bound source revision digest, selected
absolute video stream index and codec, failed hardware backend, required
continuation backend (`Software` for decode recovery), and policy revision.
It is null for a general fallback without a decode restriction. Validate
field bounds, enum values, and source/stream binding on every read; malformed
restriction state must not silently restore automatic hardware selection.
Retain the restriction when the reservation becomes `installed` or `exhausted`.

Every ordinary continuation loads this restriction before resolving or
looking up a cached plan, including a seek after recovery has completed,
track/output changes, automatic reopen, and ownership handoff. A position-only
change reuses the recovered semantic plan with new execution coordinates.
Track, subtitle, or output changes revalidate the requested presentation under
the software requirement and retain the consumed budget. If no compatible
plan exists, return a typed refusal; do not retry the failed hardware backend.

The restriction applies to its exact source revision and selected video
stream. A continuation that selects another source revision or video stream
is rejected with an explicit source/stream-change result and requires the
new-start contract below; it cannot silently drop the restriction or inherit
stale metadata. Audio/subtitle changes do not create a new video identity.

**Logical start and continuation protocol:** extend the existing create-session
contract with two mutually exclusive typed intents, rather than infer intent
from a new HTTP request or session UUID:

| Intent | Proposed fields | Server contract |
|---|---|---|
| `logical_start` | `start_request_id` and `expected_current_incarnation_id` (explicit null means none) | Authorized deliberate new play; atomically create one epoch and start result under the expected-current fence |
| `continuation` | Server-issued `continuation_authority`, current predecessor/incarnation fence, and existing requested selection/position | Resolve within the existing epoch; load its budget and restriction before preparation |

The existing `playback_id` is stable per player instance in the baseline and
does not distinguish these intents. Update web/Apple/Android create callers
to send `logical_start` only for a deliberate new play and `continuation` for
seek, track changes, directed replacement, and automatic reconnect. Preserve
the existing replacement action/acknowledgment protocol; these are additive
creation/continuation fields, not a parallel replacement protocol.

`start_request_id` is an idempotency key scoped to the authenticated user and
player instance. Persist its request digest and result with epoch creation.
An exact replay returns the same result/epoch and never reactivates an old
playback; reusing the key with a different source or request is a conflict.
Concurrent starts against one expected-current value serialize through the
same durable fence: only one can become the new logical playback. A delayed
old start cannot replace a newer incarnation merely by arriving later.
Retain start-result replay records through the applicable request-replay
window, and keep rejected or completed old results from resetting recovery.

The returned continuation authority is an opaque server-issued capability
bound to authenticated user, player instance, recovery epoch, and source/
selected-video identity. Reuse the existing authenticated capability and
revocation conventions. Validate it atomically with the current predecessor
or staged-generation fence during creation/commit; a body field naming an
epoch is not authority. Carry the capability through ordinary create requests
and prepared successors, bind it to the owner-independent durable playback,
and revoke/expire it with that playback's authoritative lifecycle. Do not
log it or place it in semantic cache identity.

An epoch-aware client missing or presenting stale continuation authority gets
a typed continuation conflict; the server does not reinterpret it as a new
start. A deliberate stop followed by a new play uses a new start key and the
correct expected-current state. Legacy clients that cannot express these
intents retain in-session-only recovery; full postpublication recovery remains
disabled for them. The server cannot prove physical user intent from HTTP,
so the guarantee depends on authorized, conforming client intent semantics,
not guessing whether two otherwise identical create requests came from clicks.

Reserve by one fenced transaction checking the current owner/incarnation,
epoch, failed attempt, and existing reservation. An identical request is an
idempotent replay; another request for that epoch sees an already-consumed
budget. Use the same transaction/fence discipline as preparation. Do not
perform a read-then-insert race or assume application rollback is available
after a Hiqlite affected-row result.

The budget is consumed at reservation and never refunded by timeout,
cancellation, crash, or failed spawn. Exact replay may finish installing the
same reserved successor; it does not authorize another alternative or a
second spawn. `installed` records success; `exhausted` records a settled
failed/cancelled installation or failed recovered producer. State and row
presence both deny another reservation.

The actor's existing one-shot retry state and this durable reservation must
agree. The actor proposes and reserves its exact decision locally; the
executor obtains the durable reservation before any replacement spawn and
reports success/failure back. Store unavailability cannot produce a software
alternative without the required reservation. No distributed transaction
with a process is assumed: after a crash, reconcile the exact reservation,
owner fence, and spawned incarnation, or fail closed for automatic recovery.

Retain ledger rows for the lifetime of the playback/epoch and its permitted
replay window. Delete only with authoritative playback cleanup after no
current/staged session or replay capability can reuse the epoch. A short
timer must not restore the retry budget. Do not carry this as a global
per-file ban across unrelated user playbacks.

Use checked conversions for actor `u64` attempt/sequence values stored as
SQLite signed integers; overflow is a typed refusal. Preserve a bounded
frozen-plan payload (proposed maximum 64 KiB) and validate its digest and
version before reservation or execution. Never persist bearer URLs, open
descriptors, or node-local execution paths as portable recovery policy.

### 8.3 Prepublication recovery uses the existing replacement transaction

The required sequence is:

1. The actor consumes the health barrier, verifies attempt/plan identity,
   selects the frozen alternative, and commits one retry decision.
2. The executor validates `ValidatedRetryRecipe` against its frozen payload
   and reserves the durable budget when this playback supports durable scope.
3. The existing child transition guard fences cancellation and publication.
   Terminate and reap the exact failed child; confirm scratch cleanup.
4. Acquire/transfer the alternative's complete resource bundle within the
   actor's installation budget. A cancelled or retired session cannot spawn.
5. Admit the next attempt, start its owned observers, and install the child
   with the exact frozen plan and execution context.
6. Publish the installed plan/resource identity and acknowledge the actor
   decision. Settle all permits and the ledger on every failure path.

Publication and retry reservation race through the actor. If publication wins,
the in-place route is closed and the decoder failure takes §8.4. A filesystem
check or an empty playlist does not override the actor's publication fact.

Do not call `demote_to_software` when retaining a hardware encoder. Introduce
an explicit resource/plan transition that retains its hardware slot, adds the
CPU reservation, and updates the work classification. Update recovery tests
that currently equate any CPU reservation with encoder demotion.

### 8.4 Postpublication recovery prepares a new session

The current `replace_failed_producer` proposal is a useful control boundary;
it is not evidence that every step of automatic decoder replacement is wired.
Implement and qualify the complete route:

```text
 decoder fault after publication
              │
              ▼
 actor failure + exact replacement proposal
              │
              ▼
 durable budget reservation + frozen software-decode alternative
              │
              ▼
 prepare successor at current fenced viewer position
              │
              ▼
 Prepare action → client readiness/acknowledgment → fenced commit
              │
              ▼
 new session becomes current; old delivery retires through existing leases
```

Use authoritative settled playback demand to choose the recovery position,
preserving pause state and selected tracks. Producer output time is not the
viewer's position. A newer seek, explicit stop, track intent, or owner fence
supersedes a stale preparation according to existing precedence; recovery
must not commit a successor at an obsolete destination.

Integrate with `PreparedSuccessorAction`, action identity/replay, preparation
deadlines, and `commit_media_session_preparation`. The commit must verify the
expected predecessor, recovery epoch, reserved alternative digest, and current
selection contract. A moved pointer aborts the staged successor, never a
newer player generation. Duplicate acknowledgments commit at most once.

Automatic replacement is enabled only for a client/protocol combination that
has demonstrated prepare, readiness/acknowledgment, commit, and retirement on
the current implementation. If the base lacks any part, completion of that
part is a dependency of this milestone. Advertising an action vocabulary or
passing serializer tests alone is insufficient.

When a capable successor cannot be prepared, the deadline expires, or the
client cannot perform replacement, report a terminal actionable failure and
stop automatic retries. Preserve source-decode and budget-exhaustion reasons
in the control response so web/Apple/Android do not start a fresh unrestricted
session in response to generic `failed` status.

Already published objects retain immutable names and their existing serving
leases; no directory clear, sequence-number reuse, or mixing of repaired
frames into that timeline is allowed. The successor owns a different session
and recipe identity. Healthy buffered media may be served according to the
existing failure/retention contract while replacement proceeds; do not
promise previously delivered corrupt frames can be recalled.

### 8.5 Offline and speculative production have the same health contract

Wire the observer into every FFmpeg producer, including builds without
`live-hls-recovery`. Offline jobs use their existing job owner and claim fence,
not the live playback actor, to consume the same typed fault and choose at
most one frozen alternative for that job. Persist budget consumption with
the job's existing attempt/claim state so a worker restart does not retry
indefinitely. Do not reset it for every retained part.

If decode changes, abandon reuse of all old-plan parts, produce under the
alternate recipe, and atomically update the job/package's result reference
under its claim fence. Do not complete the original recipe's queue row with
different bytes without updating the typed result contract. A cancelled,
yielded, or superseded job cannot publish a late artifact.

## 9. Operational controls — visible enablement, qualified paths only

Put decoder controls in **Settings → Developer → Decoder selection and
recovery**. They are persisted settings, apply to newly prepared work without
a binary rebuild or daemon restart, and are never compile features or hidden
environment gates. The card shows current qualification, missing prerequisites,
the effect of each mode, and the last refusal. A saved value is the operator's
fleet-wide upper bound, not a claim that every node and client currently
qualify. The server computes a visible effective value for each node and
playback, and refuses unsupported work explicitly instead of silently
pretending the requested mode ran.

Introduce the following settings through the existing common settings/store
contracts. Names are new contracts to add, not existing configuration options:

| Setting | Values and initial default | Effect |
|---|---|---|
| `decoder.plan_policy` | `legacy` (initial), `enforce` | Both resolve explicit plans; legacy preserves marked inherited preferences, enforce requires qualified candidates |
| `decoder.recovery` | `observe` (initial), `prepublication`, `full` | Controls automatic action on new decoder-health faults; existing non-decoder failure handling remains |
| Existing `PLURX_HWDECODE` compatibility input | Existing values | Snapshot as an operator software requirement while migrating; it cannot enable this work or bypass the visible Developer controls |

The store accepts a syntactically valid requested value without using the node
that happened to receive the settings request as fleet authority. For newly
prepared work, each node derives its effective plan policy by intersecting the
requested upper bound with its current FFmpeg/build capability and diagnostic
grammar receipts. It derives `prepublication` recovery only when owned
observation, health receipts, mixed-resource admission, and the durable
one-shot reservation pass locally. Each playback derives `full` only when
those server checks and the actual client's advertised, implemented, and
qualified prepare/readiness/commit/retirement behavior pass. The Developer
card shows the requested value, every node's current effective value and
bounded prerequisite gaps, and the effective value for active sessions.

An unqualified node or client does not make the persisted operator request
invalid for qualified peers. It receives a typed refusal or terminal action at
the unsafe boundary, never silent downgrade after claiming automatic recovery.
This is runtime capability intersection, not feature gating: one shipping
binary contains the behavior, and the visible Developer controls set its upper
bound. Qualification receipts are first-class runtime status, not hidden
environment switches.

Recovery mode does not disable diagnostic collection or requalify a failed
artifact. The new qualified-cache namespace always enforces its receipt
contract. Observation-only rollout uses explicit unqualified/legacy output
identity until qualification is enabled; there is no shadow mode that writes
old-quality artifacts under new-qualified keys.

Authorize postpublication recovery only when the requested upper bound is
`full` and the session-effective capability intersection includes qualified
end-to-end client behavior. Apply the equivalent node-effective check to
offline job recovery.
Keep backend diagnostic grammar qualification separate from selection
qualification: a backend can decode a fixture correctly while its log format
is not yet suitable for automatic fault classification.

Expose bounded diagnostics through the existing producer-control/status
surfaces:

| Field | Interpretation |
|---|---|
| Requested decoder and observed decoder | `unknown` observation is honest; encoder label is not substituted |
| Renderer and encoder | Explain the complete running pipeline |
| Decision reason and evidence class | Distinguish compatibility, override, measurement, and legacy inheritance |
| Plan digest and policy revision | Correlate command, artifact, and recovery |
| Attempt and health state | Scope errors to the current child |
| Primary video errors and observation completeness | Counters are records, not decoded-frame failure percentages |
| Recovery state and remaining automatic budget | Explain pending, installed, refused, or exhausted behavior |
| Resource reservations | Show hardware capacity and CPU weight independently |

Add counters for plan selections by bounded backend/reason, decode faults,
recovery reservation/install/refusal/exhaustion, unqualified artifacts, and
diagnostic coverage loss. Do not label metrics with file names, session IDs,
plan hashes, or raw errors. Keep those correlations in redacted structured
logs. Log a fault transition once with summaries; avoid millions of lines
from a damaged stream.

**How to read it:** software decoder plus VideoToolbox encoder is an expected
mixed plan. Rising fault counts without recovery in `observe` mode are
expected observation results. Rising incomplete-observation counts mean the
system cannot qualify artifacts, not that the source is necessarily corrupt.
One successful recovery followed by exhaustion is the configured bound,
not evidence that the controller forgot to retry.

## 10. Verification — prove policy, ownership, and delivery separately

### 10.1 Retain the selector and command matrix

Use table-driven tests in a proposed core integration binary
`decoder_selection`. The matrix must include:

| Case | Required result |
|---|---|
| MPEG-4 Part 2 + VideoToolbox, AVI/MKV/MP4 | Software decoder with hardware encoder; identical compatibility reason |
| H.264 1080p and 4K | Legacy extraction preserves baseline; enforced preference follows explicit qualification |
| HEVC 3840×1600 SDR and 3840×2160 HDR | Source work is not inferred solely from a height threshold |
| High frame rate and VFR/unknown rate | Accurate rational handling or explicit unknown; no fabricated cost evidence |
| Main/Main10 and incompatible chroma/depth | Candidate rejected when its capability/surface contract does not cover input |
| Dolby renderer requiring side data | Software requirement wins; a metadata-losing alternative is unrepresentable |
| Operator software override with vendor renderer | Qualified compatible graph or typed refusal; no silent override bypass |
| Software, VT, CUDA, QSV, VAAPI | Encoder and decoder choices are independently represented |
| Missing facts or absent backend | Conservative typed outcome; legacy inheritance remains labeled |
| Attached cover image and multiple video streams | Probe, mapping, decoder choice, and diagnostic attribution name the same selected stream |
| Software decoder implementation variants | Only inventoried implementation names can enter the plan |
| Environment changes after resolution | Existing command and recipe remain unchanged |
| Source fingerprint changes before execution | Preparation rejected/rebound through existing source contract |

Compare parsed argument tokens and option scopes, not broad string searches.
Assert that command construction consumes the resolved plan without invoking
policy. Use snapshots of relevant baseline arguments for migration tests,
with paths and session-local values normalized only in the test fixture.

### 10.2 Test the diagnostic grammar and bounded stream reader

Use a proposed daemon module test prefix `decoder_health`. Retain sanitized
real diagnostic fixtures plus deterministic synthetic streams:

- Four primary errors in the window do not trigger; the fifth does.
- The lower time boundary is excluded; stale errors age out.
- Duplicate subordinate lines do not double-count primary errors.
- Qualified fatal backend failure triggers once without a five-error wait.
- Audio errors, unselected-video errors, encoder failures, filenames, and
  unrelated messages do not become selected-video faults.
- Advancing progress, process success, pause/resume, and a late frame do not
  clear a fault. A new attempt has new counters.
- Compressed legacy summaries preserve provenance without fabricated times.
- Partial reads, non-UTF-8, long lines, reader failure, and burst buffering
  stay bounded and produce the documented completeness result.
- A flood produces one fault barrier and bounded summaries while stderr
  continues draining. Memory use is independent of stream duration.
- Actual qualified FFmpeg output with the new log flags matches the fixture
  grammar. A fake process proves lifecycle behavior, not decoder semantics.

### 10.3 Exercise the actor and executor race matrix

Use paused Tokio time and explicit barriers, avoiding wall-clock sleeps for
state tests. Add a proposed test prefix `decoder_recovery` across controller,
executor, and admission tests.

| Race or transition | Assertion |
|---|---|
| Health fault with advancing segment progress | Fault reaches decision despite healthy-looking progress |
| Fault immediately before first publication | Earlier fault barrier prevents a new publication authorization |
| First publication wins | No in-place replacement or scratch clear occurs |
| Exit success before stderr drain completes | No final qualification until diagnostic completion |
| Fault arrives during exit classification | Failure wins; no successful cache promotion |
| Observer task dies or never reaches EOF | Drain deadline settles; no indefinite wait or qualified receipt |
| Duplicate fault and decision redelivery | One decision identity and one reserved alternative |
| General fallback then decoder fault | Shared budget is exhausted; no second automatic retry |
| Decoder recovery then general failure | Same shared bound |
| Stale predecessor diagnostics | New attempt's health and completion remain unchanged |
| Stop/retirement during reservation, reap, admission, spawn, or install | No successor is resurrected; every permit is released or transferred exactly once |
| CPU reservation unavailable | Bounded capacity failure; no hidden forced overcommit |
| Same hardware encoder retained | Hardware permit remains owned; CPU delta is accounted; encoder not relabeled software |
| Multi-permit contention | No circular waiting, orphan permit, or dual child authorized by one slot |
| Scratch cleanup fails | No successor is spawned into predecessor output |

### 10.4 Prove durable budget, preparation, and client behavior

Run equivalent store contracts on SQLite and Hiqlite, including:

- Competing nodes reserve the same epoch: exactly one identity wins.
- Exact request replay returns the existing reservation; changed digests or
  failed attempt cannot reuse it as a different action.
- Owner loss, store outage, and crash between reserve/spawn/install do not
  replenish the budget or authorize an unfenced producer.
- Preparation expiration, cancellation, and failed admission consume the
  one-shot budget and leave the intended predecessor/current pointer valid.
- A successor commit verifies epoch, alternate digest, predecessor, lease,
  and action identity. A late acknowledgment cannot replace a newer seek.
- Automatic reopen, track selection, and owner handoff inherit the epoch.
  Only a deliberate new logical playback can establish a new one.
- Successful software recovery followed by seek, audio/subtitle change,
  output change, reopen, or handoff cannot select the failed decoder again;
  cache lookup also uses the restricted plan. Source/video-stream changes
  require an explicit new start rather than dropping the restriction.
- Repeated initial-start requests return one epoch; changed-payload reuse of
  the same start key conflicts. Concurrent and delayed starts respect the
  expected-current fence. New request/session IDs on continuation do not reset
  the budget. Deliberate stop/new-play can establish a fresh epoch, while stale
  continuation authority and missing authority cannot do so.
- Epoch cleanup cannot run while an active/staged generation or valid
  replay window can still reference it.

On web, Apple, and Android, exercise a real fault after publication and
observe one prepared successor, one accepted replacement, position/pause/
track preservation, and retirement of the old session. Verify unsupported
clients terminate with a reason and do not loop through new sessions. Test
the actual HTTP prepare/acknowledgment/commit path, not only control enums.

### 10.5 Prove cache and offline semantics under failure

Retain an affected old-policy cache entry and prefix. They must miss the new
identity. New-plan healthy output must hit on a later equivalent request.
Changing reason prose, resource permits, or session IDs must not change the
semantic key; changing decoder, required surface/metadata semantics, or
policy revision must.

Test missing, malformed, tampered, unsupported, wrong-plan, incomplete, and
error-containing health receipts. Verify a late stderr fault prevents
completion in local cache, shared cache, and offline production. A receipt
for one part cannot authorize another part or hide a failed prefix in the
assembled generation. A crash between manifest write and completion cannot
create a qualified hit without the full fenced publication contract.

Verify no circular hash definition: the plan digest excludes receipt results;
part evidence binds media-object digests, excluding its own receipt and
manifest; the final manifest authenticates the receipt and media objects.

Test a decode alternative during an offline job: old parts are not mixed in,
the result reference names the alternate recipe, a worker restart keeps the
job's consumed recovery state, and cancellation prevents late publication.

### 10.6 Qualify the real workload and node matrix

Test the supported deployed FFmpeg build/backend combinations: Apple
VideoToolbox, NVIDIA CUDA/NVENC, QSV, VAAPI, and software where those nodes
exist. Record unavailable configurations as unqualified, never as passes.
Use MPEG-4, ordinary H.264, 4K H.264, cropped SDR HEVC, HDR10 Main10, and
supported Dolby paths, with both ordinary and high frame rates where
available. Include subtitle rendering and concurrent mixed pipelines.

For each run record source facts, binary/build and device class, requested
and observed backend, plan digest, CPU/hardware reservations, first-segment
and first-frame latency, sustained speed, rebuffering, health counts, recovery
latency, and final recipe/receipt. Inspect pixels and metadata against the
software reference or known-good playback; output checksums are not a
quality comparison between different encoders.

Acceptance requires no unintended output-grade change, no visual regression
in retained fixtures, startup within the applicable actor budget, and
sustained playback at the configured admitted concurrency. Fault injection
must show recovery while progress continues, one alternative at most, and
no rewrite of published media. Include the original #913 media when available;
synthetic fixtures do not close its original-file acceptance.

## 11. Files and contracts — where each change belongs

All new file names in this table are proposals. Existing links identify the
baseline code to adapt; do not assume this document has created the Rust files.

| Location | Change |
|---|---|
| New core `transcode/decode.rs` | Planning types, compatibility rules, surface requirements, stable decode identity |
| [Core transcode module](../crates/plurx-core/src/transcode/mod.rs) | Resolved-plan builder and caller migration; remove runtime policy reads |
| [pipeline.rs](../crates/plurx-core/src/transcode/pipeline.rs) | Candidate requirements, qualified alternatives, preserved grade/metadata |
| [encoder.rs](../crates/plurx-core/src/transcode/encoder.rs) | Keep encoder inventory separate; expose inventoried software decoder implementations |
| [scan/probe.rs](../crates/plurx-core/src/scan/probe.rs) and daemon bound-source preparation | Additional runtime `DecodeFacts` with selected stream and source identity |
| [recipe.rs](../crates/plurx-core/src/transcode/recipe.rs) | Stable decode namespace and qualification contract |
| [manifest.rs](../crates/plurx-core/src/transcode/manifest.rs) and cache readers | Authenticate health evidence and enforce receipt compatibility |
| New daemon `decoder_health.rs` | Grammar, accumulator, observer result, completion receipt |
| [Daemon transcode](../crates/plurxd/src/transcode.rs) | Owned subprocess observation, frozen alternatives, mixed permits, offline integration |
| [admission.rs](../crates/plurxd/src/admission.rs) | Combined resources, bounded acquisition/transfer, plan-aware classifications |
| [playback_control.rs](../crates/plurxd/src/playback_control.rs) | Non-lossy fault ingress, typed decisions, one-shot budget and publication ordering |
| [domain.rs](../crates/plurx-core/src/domain.rs), [store interface](../crates/plurx-core/src/store/mod.rs), SQLite/Hiqlite session implementations | Epoch propagation, durable reservation, fenced preparation result |
| [HTTP HLS](../crates/plurxd/src/http/hls.rs) | Failure proposal to qualified successor preparation and client action |
| [media_pool.rs](../crates/plurxd/src/media_pool.rs) | Versioned plan/qualification capability for ownership transfer; receiving worker validates frozen plan |
| Web/Apple/Android control consumers | Preserve epoch through directed replacement; explicit terminal behavior on exhausted/unsupported recovery |
| [System diagnostics](../crates/plurxd/src/http/system.rs) | Decoder/health/resource fields with bounded vocabulary |
| Validation registry and retained fixtures | Focused policy, health, store, cache, client, and real-media proof |

Cross-node placement must not interpret a portable codec name as support for
the frozen decoder/backend plan. Version the relevant private capability and
request contracts; an older worker cannot claim a new qualified recovery
plan it cannot represent. A qualified worker may bind its own execution paths
and permits but cannot silently change the plan digest. If no compatible
worker exists, return a bounded unavailable result rather than reroute to the
failed backend. Full automatic cross-node recovery remains gated until the
durable epoch and replacement contracts pass on that path.

## 12. Implementation milestones — reviewable slices with observable exits

Use one temporary `effort/decoder-selection-recovery` integration branch and
reviewable task branches based on its current head, following
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md). Branch names are a proposed
execution organization, not branches created by this document. Do not merge
an intermediate effort state into `main` as a finished feature.

Before any Rust edits, establish the pinned Rust 1.97.1 compile loop from
[AGENTS.md](../AGENTS.md) and [AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md).
Transfer committed source with `git archive` if using another compiler host;
transfer no repository credentials. Re-run evidence against each actual base.

### 12.1 M0 — capture baseline and freeze diagnostic qualification

Inventory production `hls_args` callers, process spawners, renderer constraints,
cache readers/writers, current playback replacement support, and configured
node FFmpeg builds. Capture baseline argument and real-media controls. Add
sanitized decoder-failure fixtures and validate the proposed grammar/window.
Document final constants, known grammar coverage, and node/client gaps.

**Exit:** source inventory covers every shipping caller; retained fixtures
discriminate primary video failures from tolerant controls; threshold values
and observed fault detection latency are recorded. There is no production
behavior change and no missing prerequisite disguised as an assumption.

### 12.2 M1 — extract explicit plans without broad routing changes

Implement pure types, bound fact collection, policy snapshot, surface
validation, and legacy candidate preferences. Encode the incident exclusion
if it is already in the intended base. Add the selector matrix and a
temporary adapter only where necessary to land the extraction coherently.

**Exit:** baseline command selections match for unaffected inputs; hard
renderer/override constraints have tests; every plan records whether its
backend evidence is qualified or inherited. No performance policy broadening.

### 12.3 M2 — make arguments and identity consume the same plan

Migrate all live, retry, speculative, offline, and cache recipe callers.
Introduce stable plan digest and versioned recipe identity. Remove hidden
builder policy reads and unsafe plan defaults. Wire new namespaces as
unqualified until receipt production exists; do not prematurely advertise
qualified cache entries.

**Exit:** a single prepared plan drives both arguments and identity; environment
mutation after preparation changes neither; old affected entries/prefixes
miss; relevant unchanged semantic inputs reuse keys. No unconverted shipping
caller can bypass planning.

### 12.4 M3 — own health observation and gate reusable completion

Implement bounded stderr parsing, owned reader completion, the actor's sticky
health barriers, and authenticated receipts. Integrate local/shared/offline
completion. Keep new automatic decoder actions in `observe` mode.

**Exit:** a fake FFmpeg that advances output, emits selected-video errors,
then exits successfully cannot publish a qualified artifact. A healthy real
fixture can publish and hit. Floods, reader loss, late diagnostics, and
completion races pass. Observation mode does not initiate a new decoder retry.

### 12.5 M4 — account for mixed pipeline resources

Add resource estimates, complete permit acquisition/transfer, decoder cap
binding, plan-aware speed classes, and explicit mixed startup classification.
Retain existing all-software accounting without double-counting encode work.

**Exit:** concurrent mixed plans cannot bypass CPU admission; capacity wait is
bounded; cancellation and resource transfer tests show no leaked permit or
double-authorized encoder. Real concurrent controls sustain the admitted load.

### 12.6 M5 — implement the shared budget and prepublication decode recovery

Add durable epoch/reservation contracts and store migrations. Freeze
reason-specific alternatives, add typed health failure decisions, and adapt
the existing child replacement transaction. Enable `prepublication` only on
qualified nodes while postpublication decoder faults remain explicit failures.
Persist the source/stream-bound recovered restriction and apply it to all
ordinary continuation planning and cache lookups.

**Exit:** ongoing segment progress cannot mask a decoder fault; an eligible
prepublication fault installs software decode with the same hardware encoder
exactly once; publication/cancellation/store/crash races retain the budget and
fences; both store backends pass the same reservation contract.

### 12.7 M6 — wire and qualify replacement after publication

Connect actor proposals to durable preparation with a frozen alternate and
current fenced position. Propagate recovery epoch through successor records,
control exchange, and owner transitions. Complete any missing existing
prepare/acknowledgment/commit dependency before enabling the path. Update
client terminal handling and qualify web, Apple, and Android independently.
Implement the idempotent logical-start and server-issued continuation-authority
contract, including atomic start fencing and start-result replay persistence.

**Exit:** each enabled client performs one directed replacement preserving
position, pause, tracks, and grade. A new seek wins over stale recovery;
duplicate action/acknowledgment is idempotent; unsupported clients do not
reopen-loop; published old media remains immutable.

### 12.8 M7 — finish offline, shared-cache, and handoff enforcement

Complete job-scoped budget persistence, alternate result references,
part/assembly receipt verification, versioned worker capabilities, and owner
handoff coverage. Remove any migration adapters that let these paths bypass
the resolved plan or qualify output without observation.

**Exit:** worker restart and mixed-version cache/placement tests cannot reset
recovery, mix prefixes, claim unsupported plans, or reuse missing/failed
receipts. Default and no-live-recovery builds both compile and exercise their
shipping producer paths.

### 12.9 M8 — qualify the fleet and promote the completed effort

Run the real workload matrix, classify false positives, verify ordinary
startup/concurrency, and measure recovered playback latency. Enable enforced
selection only on qualified backend classes; enable full recovery only for
qualified client/node combinations. Record feature settings and rollback
behavior in the operational docs.

Freeze task merges, merge current `main` into the effort, rerun qualification
on that exact tree, and follow the repository's Main promotion gate and
receipt requirements. A passing effort gate does not make the feature releasable.

**Exit:** all invariants in §1 and relevant matrices in §10 have evidence
against the final candidate; unsupported combinations remain explicitly
disabled rather than reported as complete. The release description names
enabled combinations and remaining limitations.

## 13. Commands and evidence — compile locally before using CI

The proposed new focused core binary is `decoder_selection`; create it in
M1 before using this command. The daemon prefixes below are proposed retained
test names. They must actually run matching tests and report nonzero counts.

```bash
rustc +1.97.1 --version
cargo +1.97.1 test -p plurx-core --test decoder_selection
cargo +1.97.1 test -p plurxd --bin plurxd decoder_health
cargo +1.97.1 test -p plurxd --bin plurxd decoder_recovery
```

**How to read it:** the compiler must report the repository pin, and each
focused command must report its expected test count with zero failures. A
filter matching zero tests is missing evidence. Add exact focused commands
for recipe/manifest and both backend store contracts to the task PR; do not
invent a test command that only exercises one implementation.

The shared compile/lint checks include:

```bash
cargo +1.97.1 fmt --all -- --check
cargo +1.97.1 check -p plurxd --all-targets
cargo +1.97.1 clippy -p plurxd --all-targets -- -D warnings
cargo +1.97.1 check -p plurxd --all-targets --no-default-features
cargo +1.97.1 clippy -p plurxd --all-targets --no-default-features -- -D warnings
git diff --check
```

Run the smallest meaningful regression for each task and the repository's
required affected-surface checks. At final promotion include the daemon
suite, core contracts, both storage backends, web controls, Apple/Android
checks for changed protocol handling, and the qualified hardware/media tests.
Use the actual validation commands from the current pipeline for client and
store configurations. Do not substitute an unpinned compiler or treat socket/
FFmpeg linkage failures as successful tests.

For every milestone record candidate commit/tree, compiler, FFmpeg build,
commands with counts, node/client combination, configuration mode, measured
startup/recovery results where applicable, and explicit missing evidence.
Use retained before/after regressions and [scripts/prove-fix](../scripts/prove-fix)
where appropriate. A mock proves orchestration; actual FFmpeg proves its
grammar and decode behavior; physical playback proves the client transition.

## 14. Rollout, rollback, and scope limits

Deploy in three observable stages: explicit legacy plans with health
observation; qualified selection and prepublication recovery; then full
recovery for qualified client/node pairs. Baseline startup, CPU pressure,
decode-fault incidence, unqualified-cache rate, and client reopens before
changing stages. Investigate any wrong-grade output, published timeline
mutation, duplicate successor, budget reset, or cache receipt bypass as a
release blocker.

If automated health actions cause false positives, return recovery to
`observe` while retaining diagnostics, plan identity, the MPEG-4 compatibility
exclusion, and cache qualification rules. If a selection class regresses,
roll back its preference revision with new output identity as needed.
Changing modes does not mutate arguments of already running attempts.

Do not turn off receipts or restore old cache eligibility to improve apparent
hit rates. A full binary rollback is broader: it may lose durable state or
receipt compatibility and must be checked against the migration strategy.
Use additive schema and backward-compatible old-row handling; require
explicit protocol/capability rejection where older owners cannot preserve
recovery epochs. Rehearse mixed versions and rollback before promotion.

This effort does not include media repair, a replacement of FFmpeg, a visual
quality classifier, a fleet-wide adaptive backend blacklist, or a new player
protocol alongside the existing preparation protocol. It does include any
missing integration necessary for decoder recovery to use that protocol
correctly. It must not claim full postpublication recovery while leaving that
dependency unwired.

The intended deliverable is a working, qualified selection and recovery
system with one source of plan truth, one lifecycle owner per producer,
bounded automatic alternatives, and durable output evidence. This document
specifies that work; no milestone has been implemented or tested by writing it.
