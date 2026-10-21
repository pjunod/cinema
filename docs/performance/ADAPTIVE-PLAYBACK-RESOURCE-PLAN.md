# Adaptive playback resources — learn capacity and use the whole cluster

**Status:** open · design only; implementation and qualification not started ·
**Written:** 2026-10-20 · **Source baseline:** `a06be68b90` ·
**Owner:** Codex planning session · **Review:** Fable 5.1 requested; pending.

Companion to [PLAYBACK.md](../PLAYBACK.md) (delivery behavior),
[the cluster performance contract](../cluster/CLUSTER-PERFORMANCE-PLAN.md)
(placement and authority), and [the performance handoff](PERF2-IMPLEMENTATION-HANDOFF.md)
(existing performance work). This is the canonical implementation and acceptance
ledger for automatic resource management across playback pipelines. Build in
milestone order; re-verify source anchors against the intended branch before
editing. If adaptation appears to require weaker authorization, an unowned
producer, a false quality badge, or an unfenced replacement, amend this design
rather than weakening those contracts.

## 1. Objective and evidence — capacity is a measured property

A server must determine automatically which concurrent workloads it can sustain
within their deadlines, then use eligible cluster workers before reducing
requested quality. Operators must not benchmark each installation or count
FFmpeg threads manually. This includes single-node installations, heterogeneous
clusters, containers and machines without GPU-utilization telemetry.

The motivating observation on October 20 was two P7/FEL plays. Whichever device
started first received FEL/RPU/DYN; the second received ordinary P8.1/DYN. The
server recorded that a GPU slot and 15 CPU threads were held by other work.
Source inspection found a conservative 15-thread reservation for FEL with
audio, not a measurement of 15 busy cores. On the observed 16-core server the
default CPU allowance is 15. No simultaneous-load measurement establishes that
two such streams would fail. The log alone does not distinguish CPU-budget from
GPU-slot contention; the new diagnostics must.

A separate cluster gap matters even if local admission becomes accurate:
[HLS creation](../../crates/plurxd/src/http/hls/create.rs) attempts processed
preparation locally before ordinary placement, while
[the media catalog](../../crates/plurxd/src/media_pool.rs) marks remote processed
candidates `dispatch_supported = false`. A remote description cannot carry the
local prepared executable or its one-shot credit. This restriction must be
replaced with worker-owned preparation, not removed in isolation.

**Required policy:** prefer a known eligible worker at the requested semantics
and quality; allow a bounded local learning trial when safe and useful; on local
capacity refusal try eligible peers at the same quality before any permitted
fallback. Do not move an existing successful stream merely to start another.
An incomplete or timed-out cluster search means capacity is unknown, not that
all nodes are full.

**Non-goals:** changing decoder compatibility, qualifying new HDR algorithms,
adding arbitrary HTTP redirects, weakening explicit administrator limits,
replacing player quality/control protocols, or promising uninterrupted service
on hardware that cannot meet demand. This proposal does not implement proactive
whole-client evacuation; §9 defines the boundary and required drain accounting.

## 2. Coverage — every physical workload has an owner

The same resource service governs all rows. Workload-specific deadlines and
recovery actions remain distinct; one generic “speed above 1×” test is wrong.

| Work | Resources and service obligation | Existing integration seam |
|---|---|---|
| Direct play, byte ranges and cached segments | Read throughput, open handles, memory, egress; requested bytes by response deadline | HTTP stream handlers; VOD source lifetime |
| Progressive/HLS copy and remux | Demux CPU, source I/O, audio work, packaging, scratch and egress | `EncodePermit::try_source_copy_under`; rolling/copy producers |
| Software and hardware transcode | Decode/filter/encode engines, CPU time, RAM/VRAM, I/O; playable media by deadline | `TranscodeResourceEstimate`, `admit_live_with`, frozen VOD bundles |
| HDR/DV/HLG/tone mapping, scaling and deinterlacing | Whole graph, including helper children, transfers and authoring | DV runtime, Mac processing, ordinary filtered encode |
| Audio-only and subtitle work | CPU, decoder processes, extraction I/O and memory; preparation deadline or realtime output | Live audio paths, `subtitle_work`, fragment extraction |
| Continuous/adaptive VOD | Audio/video rendition family, successor overlap and startup peaks | `VodServe::try_reserve_media_group`, retained credits |
| Live TV and timeshift | Tuner/source limits, paced input, decode/encode, ring buffers and transport latency | `LiveTvProcess`, live resource manager |
| DVR recording | Committed capture and write deadlines, bounded queues, storage capacity | DVR transport, sink and fanout |
| Background indexing, previews, pretranscode and probes | Accounted, bounded work that yields to foreground and capture | `admit_fragment`, pretranscode, census and benchmark admission |
| Cluster relay and remote source read | Worker, source node and ingress resource domains; network in both directions | `media_pool`, `media_sessions`, HLS relay |

Count a shared producer once, each actual sink separately, and retained memory
or devices even while the producer is paced. Cached output consumes delivery
resources, not historical encoding resources. Audio-only/copy Live TV currently
owns no video permit; register its real workload without inventing an encoder
slot. Fragment extraction currently reserves the entire CPU allowance; replace
that estimate only after its real lifecycle and measurements are integrated.

**Platform contract:** Linux, macOS and Windows implement the same portable
measurement and admission semantics. Hardware-specific counters are optional.
Unavailable metrics are explicit `Unknown`, never zero or a false idle reading.
Unsupported processing remains unsupported; automatic resource management does
not create backend capabilities.

## 3. Reuse existing authority and replace only capacity policy

These source anchors are verified at the baseline; signatures are existing,
not proposed replacements:

| Anchor | Existing contract to preserve |
|---|---|
| [admission.rs](../../crates/plurxd/src/admission.rs): `try_admit_bundles_claiming(&self, hardware_max: usize, software_budget: usize, estimates: &[TranscodeResourceEstimate], priority: Priority, claim: Option<u64>) -> Option<Vec<TranscodePermit>>` | Atomic all-or-none ownership, priority and family claims |
| [vodencode.rs](../../crates/plurxd/src/vodencode.rs): `try_admit_frozen_bundle`, `admit_frozen_preparation` | Frozen recipe and preparation credit are bound to admitted execution |
| Same file: `ActiveProductionWindow`, `CandidateProductionProofs` | Contiguous actual production; startup excluded; holds/retries reset evidence; exact recipe and live parent identity; bounded recent observations |
| [prodrun.rs](../../crates/plurxd/src/prodrun.rs): `ConfirmedProducerReap` | Admission survives until physical child/descendant cleanup and writer settlement |
| [VOD group creation](../../crates/plurxd/src/vod/serve/create.rs): `try_reserve_media_group` | Audio/video groups and overlap claims admit together |
| [media_pool.rs](../../crates/plurxd/src/media_pool.rs): `local_snapshot`, `compare_offer`, `compare_spare` | Advisory descriptions, fresh worker-side reservation; no capacity authority transferred by a snapshot |
| [media_sessions.rs](../../crates/plurxd/src/media_sessions.rs): `RemoteStartRequest`, `RemotePrepareRequest`, `StartedSessionGuard` | Authenticated remote lifecycle, incarnation/owner fencing and exact late-start cleanup |

The present spare-capacity score mixes hardware slots and CPU threads. Source
I/O and egress pressure also use session-count proxies in the current snapshot.
Replace these with dimensioned measurements and deadline feasibility; do not
rename the proxies as measured capacity.

Timeout, cancellation, lease expiry, owner-map removal, SIGSTOP and silence are
not proof of release. A borrowed reservation is not reusable spare capacity.
An output recipe, decoder plan or published playlist cannot mutate in place
because scheduling changes. Preserve generation-fenced replacement and the
existing distinction between unknown transport outcome and confirmed failure.

## 4. Resource contract — distinguish limits, estimates and observations

### 4.1 Separate physical limits from learned service cost

Maintain a resource vector per node and per device/storage/network domain:

- **Hard limits:** explicit admin caps, effective container/job constraints,
  verified device-session limits, tuner count, bounded process count, RAM/VRAM
  and scratch ceilings, disk reserve, file handles and transport queue bounds.
- **Service estimates:** CPU-seconds per media-second or per byte, device busy
  time where available, read/write bytes, allocation peaks, and stage latency.
  An estimate carries sample count, age, workload identity and uncertainty.
- **Observations:** actual completed playable output, deadlines, production
  backlog, process CPU time, resident memory, allocation failures and optional
  pressure/thermal/device signals. Record external contention as context.

Thread creation limits and expected CPU consumption are separate fields.
Fifteen worker threads are not fifteen continuously busy cores. Conversely,
low CPU use is not evidence that a saturated decoder or storage device is free.
Do not add unlike units into a single “spare” scalar.

### 4.2 Proposed interfaces and state ownership

The following is a proposed Rust-level seam, not existing code. M0 must bind
these types to current lifecycle owners before any policy replacement:

```rust
trait ResourceObserver {
    fn snapshot(&self, now: Instant) -> ResourceObservation;
}
fn assess(request: &WorkloadDemand, state: &ResourceState) -> AdmissionAssessment;
fn try_reserve(
    request: &WorkloadDemand,
    assessment: &AdmissionAssessment,
    owner: &ProducerIdentity,
) -> Result<ResourceReservation, AdmissionRefusal>;
fn observe(owner: &ProducerIdentity, sample: WorkSample);
```

`WorkloadDemand` includes recipe/source identity, operation graph, phase,
resource domains, output/source geometry and frame rate, deadline, priority,
known memory upper bound, sharing/family identity, explicit fallback policy,
and requested quality. `AdmissionAssessment` is advisory and versioned against
an observation epoch. `try_reserve` rechecks hard limits and conflicting live
claims atomically; stale assessment does not authorize execution.

`ResourceReservation` is process-local RAII authority, retained by the existing
producer/reaper owner. It is never serialized. It supports atomic family
acquisition and phase transitions. Moving from startup to steady state may
release only resources demonstrably no longer held; reacquisition is mandatory
before allocating them again. Queue residency and memory remain accounted while
paused. No new scheduler mutex spans awaits or process creation.

Use typed refusals: `HardLimit`, `ExplicitCap`, `InsufficientEvidence`,
`TemporaryContention`, `DeadlineBudget`, `CapabilityUnavailable`,
`PeerUnavailable`, `SourceUnavailable`, `Maintenance`, `OwnershipConflict`.
Each carries the limiting domain, requested/available amount if known, evidence
age and whether another worker or later retry can help. Never collapse these
into “needs a GPU slot and threads.”

### 4.3 Compatibility and settings migration

Existing keys `transcode.max_hw_sessions` and
`transcode.software_pool_threads` remain explicit ceilings when configured.
An absent value selects automatic policy; it no longer implies a permanent
capacity proof from the legacy default. M0 must audit every settings writer to
distinguish absent defaults from intentionally stored values. If provenance is
ambiguous, preserve the stored cap and label its origin as legacy; do not
silently relax it. Existing zero/disabled meanings remain intact.

Persist a versioned settings mode for future writes (`automatic` or `explicit`)
with migration/read compatibility, rather than guessing from equality to the
old default. Settings/UI changes are delivered with that contract. There is no
gratuitous Developer enable switch. Developer may expose read-only evidence;
permanent administrator limits belong in normal resource/playback settings.

## 5. Measurement — observe useful service, not just busy processes

### 5.1 Whole-graph and per-phase observations

Measure output that is verified, packaged and available to serve. FFmpeg's
`speed` and worker-thread counts alone are insufficient. Sum process-tree CPU
time, avoiding double-counted child totals; use monotonic elapsed time. Bound
sampling to once per second per resource domain, plus existing output events.
Include helper startup, shutdown and packaging in service latency; retain
separate steady-state production intervals to avoid mislearning startup cost.

Maintain separate states for preparing, producing, input-paced, buffer-paced,
client/network-backpressured, paused, retrying and retiring. Slow NAS reads and
slow clients must not train a fast encoder as slow. A full buffer intentionally
pausing a producer must not imply an idle reservation can be stolen. Thermal
throttling and external jobs should reduce short-term capacity confidence.

For VOD, track media seconds produced per active wall second and server/client
runway separately. Client buffer reports must be fresh, generation-bound and
clamped to the server's produced frontier; absent or untrusted reports cannot
authorize riskier trials. For paced Live TV/DVR, measure deadline slack, queue
occupancy and dropped/late input; its 1× input rate says nothing about spare
encoding capacity. For direct copy, measure byte-service deadlines and read/
egress pressure rather than fabricate a media-production speed.

### 5.2 Portable observations and memory safety

All platforms provide monotonic progress, process lifecycle, CPU time and memory
observations through their supported process APIs. Platform adapters optionally
add Linux pressure/cgroup constraints, macOS pressure/thermal signals, Windows
job/memory constraints and vendor engine counters. See the
[Linux PSI contract](https://docs.kernel.org/accounting/psi.html) for stall-time
semantics. Linux-specific counters are never prerequisites on other platforms.

Detect effective CPU quotas/affinity and memory limits rather than host totals.
Where a backend cannot bound peak memory, permit no speculative overcommit of
its memory domain: use the existing conservative serialized cold path and
report unknown capacity. A successful prior allocation is not a hard maximum.
Telemetry failure disables expansion, not existing ownership or cleanup.

### 5.3 Learning identity and persistence

Key history by node/device identity, executable and helper hashes, driver/API
identity, effective resource limits, graph stages, source and output raster,
frame rate, bit depth/HDR mode, codec, audio layout and subtitle/filter cost.
Separate startup and steady-state statistics. Exact recipe evidence can support
execution; workload-class history only predicts whether to attempt preparation.
Never grant an executable or compatibility proof from historical speed.

Store bounded local summaries (initial maximum 1,024 workload classes per node,
LRU eviction), with a schema version and atomic replacement. High-frequency
samples and reservations do not go through Raft. Start a new epoch after
relevant software/device/limit changes; older results are priors at most and
cannot authorize expansion. Restart loses active trials and claims. Recover
physical children through existing ownership recovery before declaring capacity
free. Long-term estimates expire after 7 days without relevant samples;
short-term expansion evidence expires after 15 seconds. These are initial
policy constants, subject to the M7 qualification criteria, not hardware facts.

## 6. Controller — bounded learning with predictable recovery

### 6.1 Admission algorithm

```text
Authenticate and freeze playback intent / source / deadline
                 |
Filter capabilities, hard limits, maintenance and source access
                 |
Rank eligible workers for this requested output and service deadline
                 |
Known capacity -> worker atomically reserves -> prepare -> activate
                 |
Unknown capacity -> one eligible bounded trial, if incumbents protected
                 |
Local refusal -> next eligible worker at SAME semantics/quality
                 |
No timely candidate -> permitted fallback, queue, or typed refusal
```

Reuse compatible completed/prepared output before creating duplicate producers.
For prediction, compare upper estimated resource demand with conservative
available capacity in each dimension. Do not extrapolate N concurrent streams
from one stream's speed or assume linear scaling. Capture observations under
actual mixed load, including existing viewers, and keep request admission
serialized per constrained device/domain so simultaneous starts cannot all
spend the same headroom.

A cold server uses existing conservative eligibility and a bounded first
preparation window. No full-library startup benchmark or required operator
calibration. Unknown classes remain discoverable through real work or admitted
idle-time probes; a previous refusal must not permanently blacklist a class.

### 6.2 Trial budget and acceptance

Initial trial policy, to be calibrated in M7:

- At most one additional speculative producer graph per node, also excluding
  conflicting trials on a shared device or source domain. Trials obey hard
  memory/device/admin bounds and the original request deadline.
- Trial runtime is at most 5 seconds and at most half the remaining start
  budget, whichever is less. It cannot extend an existing startup SLA. Measure
  startup separately; fewer than three complete production intervals or fewer
  than 2 seconds of production gives insufficient evidence, not success.
- Expansion beside VOD incumbents requires each affected incumbent to have
  trusted runway of at least `max(20 s, trial budget + cleanup bound + 2 segments)`.
  The process owner must have a tested cleanup bound; absent one means no trial
  beside incumbents. No extra lookahead is forced merely to qualify a trial.
- Paced Live TV/DVR and unbuffered copy cannot supply that protection. Do not
  explore unknown shared bottlenecks beside them. Use fresh previously observed
  concurrent capacity, another worker, or a trial while the affected domain is
  idle. Capture work must never be sacrificed to learn a VOD limit.
- For unpaced VOD, all participating producers must sustain at least 1.25×
  active production with non-shrinking runway across the observation window.
  This is preliminary admission evidence, not permanent approval. Continue
  monitoring throughout playback and across scene changes.
- Abort a trial immediately on hard-limit pressure, an incumbent deadline miss,
  or loss of the runway reserve. Two consecutive one-second observations below
  the production margin also abort. Stop/reap the newcomer and confirm release
  before another attempt; keep the incumbent's recipe and ownership unchanged.

The policy must account for CPU needed by HTTP/control/cleanup, GPU allocation
peaks and shared-source effects even when device counters are unavailable. No
finite probe guarantees future movie complexity; runtime recovery is mandatory.

### 6.3 Ongoing scheduling and fairness

Allocate service by deadlines and protected commitments: capture and already
playing foreground work first; startup/seek recovery next; optional upgrades
and background work last. Seek overlap belongs to its existing playback family,
not a second viewer. Pause/deactivate background processes cooperatively at safe
boundaries, then physically reap if yielding is required; suspension alone does
not surrender memory, surfaces or permits.

Use fair queued service among comparable foreground starts, with aging and a
bounded wait inside each request's deadline. A refusal includes retry guidance;
never create an unbounded hidden queue. On overload, first stop disposable work
and new trials, then attempt an eligible peer for the newcomer or an established
replacement protocol. Only Auto or otherwise explicitly permitted fallbacks may
change quality. An exact/manual request that cannot be met gets a named refusal
or the existing explicitly authorized recovery, not a silent cheaper recipe.

A failed expansion has a 60-second node/class cooldown; capacity growth needs
30 seconds of stable current observations before the next expansion. These
restrain oscillation. Real hard pressure triggers immediate action without
waiting for the cooldown. A node failure/capability change invalidates affected
predictions. Hysteresis never suppresses a user's new request or another node's
independent attempt.

## 7. Cluster placement — local pressure is not cluster exhaustion

### 7.1 Offers are hints; workers retain execution authority

Extend capability-negotiated offers with dimensioned estimates, confidence,
measurement age/epoch, effective limits, current commitments, source locality,
relay path and predicted startup deadline. No raw model weights or counts become
an authoritative remote permit. Rank candidates by output-policy compliance,
ability to meet the deadline, confidence and bottleneck headroom; use source/
cache locality and transfer cost as tie-breakers. Do not always prefer local
execution over a known healthy peer.

A worker must independently verify source identity/access, requested graph,
client compatibility, current settings, maintenance, serving generation and
atomic admission. Concurrent ingress requests may see the same idle offer;
only the worker's reservation decides who can run. Keep one common monotonic
create deadline; remote retries consume it rather than restarting it.

Attempt up to three distinct eligible workers, ordered by current evidence,
within that common deadline. Three is an initial bounded-search policy, not a
claim of global exhaustion on a larger fleet. Fallback diagnostics distinguish
all eligible workers refusing from budget/search-bound exhaustion. Do not
retry authentication, unsupported source or ownership errors as generic load.

### 7.2 Prepare enhanced and ordinary graphs on the selected worker

Add a versioned, capability-negotiated remote prepare/adopt transaction using
existing media-session transport and guards. Proposed logical phases:

1. **Prepare:** ingress sends authenticated principal, request/incarnation ID,
   source/planning digest, desired processing semantics, exact candidate when
   already known, client caps, output constraints and remaining deadline.
2. **Worker validation/reservation:** selected worker resolves its own concrete
   recipe and acquires its own resource bundle. It owns the source handles,
   executable, prepared output and cleanup guard. If a known candidate was
   selected, its identity must match; a newly resolved candidate is returned
   for existing candidate/admission checks before adoption.
3. **Prepared response:** return a scoped, unguessable handle plus recipe/output
   identity and evidence. Bind the handle to worker boot/serving generation,
   principal, incarnation, source/settings digest and fixed expiry. The handle
   is not the serialized resource reservation and is never client-visible.
4. **Adopt:** ingress validates output and commits the existing durable owner
   route; worker adopts idempotently under the same incarnation. Duplicate
   requests recover the same transaction, not duplicate producers. One live
   owner is selected; losing/late preparations remain guarded until reaped.
5. **Cancel/expiry:** cancellation and abandoned handles trigger owned cleanup.
   Expiry forbids adoption but never claims the process has already stopped.
   Outcome-unknown adoption is resolved by transaction status and durable owner
   fencing before preparing a conflicting successor.

M4 must specify exact wire fields, signed-request audience, replay/idempotency
rules, maximum handle lifetime and prepare/adopt crash matrix before coding the
protocol. Reuse existing `RemotePrepareRequest` where its semantics match;
do not add a competing authorization system. The present remote-processed
catalog restriction may be relaxed only for peers proving this full protocol,
not merely deserialization support. Older workers continue ordinary supported
routes and explicitly decline remote processed preparation.

### 7.3 Account for the entire delivery path

Current HLS normally keeps the client on its original ingress. A session URL is
a capability; the ingress resolves the durable owner and relays remote media
through authenticated cluster requests. See
[HLS relay](../../crates/plurxd/src/http/hls/relay.rs). Moving encoding to worker
B does not remove ingress A's egress or relay load.

Track worker processing/read load, source/storage-domain contention, inter-node
transfer, ingress relay CPU/memory/egress and client/network backpressure. Any
existing hard source-I/O/transport reservations remain required. Per-node CPU
learning cannot override a shared NAS bottleneck. Shared-domain exploration
needs one bounded cluster trial claim through existing durable authority; a
partition or unknown claim outcome refuses competing exploration. Do not put
per-frame accounting through consensus.

## 8. Operator and client behavior — explain the actual decision

Expose effective automatic/explicit limits, actual reservations, recent useful
throughput, confidence/age, limiting resource, protected workloads, and the
worker/ingress/source nodes. Avoid movie names, tokens, raw file paths and
unbounded workload IDs in metric labels. Keep detailed traces access-controlled
and bounded. Per-start explanations include attempted workers and reasons,
without promising a measurement when only a conservative estimate exists.

Example meanings:

- “Local capacity unproven; enhanced playback prepared on another node” means
  successful remote placement, not a local hardware failure.
- “CPU reservation cap reached (explicit setting)” means policy blocked work,
  not that observed CPU utilization reached 100%.
- “Enhancement unavailable within startup deadline; compatible P8.1 selected”
  means a permitted fallback; the trace distinguishes measured shortage,
  inaccessible peers, search bound and insufficient evidence.

Keep FEL/RPU/DYN, HDR10-E, codec and quality badges tied to actual delivered
output. Never light a badge because a preferred route was requested. Existing
clients remain compatible; richer diagnostics are additive. Fixing the Apple TV
Info focus defect is separate UI work and is not prerequisite to server logs.

## 9. Draining and failover — distinguish worker, ingress and client

Maintenance already fences new work and waits for existing owned work; it does
not imply live migration. See
[planned maintenance](../OPERATIONS.md#planned-node-maintenance--fence-drain-update-resume).
Adaptive scheduling must honor that fence and its late-admission barriers at
offer, reserve, prepare and adopt. A draining node advertises no new capacity,
including for probes; existing commitments remain counted until physical release.

Apple already has media-origin retry in
[Session.swift](../../clients/apple/Sources/Session.swift), but that deliberately
does not change the account/API origin. Failover is not a universal server-pushed
client redirect or proof that a draining ingress is empty. A new ingress can
still relay to the draining worker, and a new worker can still relay through
the draining ingress.

This effort delivers accurate drain blockers and capacity-aware new placement.
Proactive evacuation of existing clients is a separate contract: trusted client-
reachable origins, TLS/auth scope, capability negotiation, prepare/commit/rollback
of playback position and generation, API-origin handling, browser/native/third-
party clients, relay removal, direct/range downloads and non-migratable capture.
Until separately implemented and qualified, wait for those owners to finish or
use existing explicit termination procedures. Never declare a node reboot-ready
from model state, expired leases, successful processing relocation, or missing
client telemetry. Preserve direct local process/connection evidence.

## 10. Delivery sequence — integrate one effort, prove each seam

Use `effort/adaptive-playback-resources`. Task branches start from its current
head and target it. The disjoint-files exception does not apply: admission,
VOD and cluster lifecycle changes overlap. File ownership below names the
integration owner per milestone; independent exploration/adapter work may run in
isolated worktrees, never concurrent writers to the same files. Rust work must
establish the pinned compiler loop before editing.

| Milestone | Implementation scope / primary ownership | Completion evidence |
|---|---|---|
| M0: inventory and contracts | Admission/process owners; all producer-spawn paths; settings writers; reconcile related plans | Checked coverage table maps each spawn/relay/sink to one owner, resource domains and phase; exact proposed wire/types and crash matrix reviewed; no route silently excluded |
| M1: observations and diagnostics | Portable observer module, process wrappers, production observers, metrics | Deterministic CPU/time/memory/progress tests; Linux/macOS/Windows observations or explicit Unknown; no decisions changed yet; bounded overhead evidence |
| M2: node policy and learning | `admission.rs`, bounded local history, settings migration | Atomic vector reservations, explicit-cap preservation, stale/unknown cases, cooldown/fairness, process-reap invariants and model epoch tests |
| M3: all pipeline adapters | VOD finite/rolling/continuous; live/DVR; audio/subtitles/copy; background/census | Every M0 row uses service; family/share accounting tests; mixed workload trial and recovery; no thread-count-only hidden refusal on automatic paths |
| M4: worker-owned remote preparation | `media_pool.rs`, `media_sessions.rs`, HLS creation, worker admission | Same-quality remote preparation/adoption for ordinary and processed output; all transaction crash/duplicate cases; old/new peer compatibility; remote reservation never reconstructed from a description |
| M5: cluster policy and delivery domains | Offers/ranking, shared source claims, relay telemetry and maintenance hooks | Local-full/peer-idle new play stays enhanced; NAS/relay saturation refuses misleading placement; simultaneous ingress, partitions, draining nodes and bounded search tests |
| M6: operational integration | Normal settings, Developer read-only facts, API/operations/reference docs, client diagnostics | Existing caps retain meaning; truthful fallback/placement explanation; no new feature toggle; drain blockers and client compatibility demonstrated |
| M7: qualification and promotion | Integration owner: exact final source, tests and receipts | Deterministic suites, real-process cleanup, platform matrix, mixed-load campaigns, independent review, current-main promotion gate and qualification receipt |

M1 is an instrumentation-only implementation phase, not a permanent shadow-mode
switch. M2/M3 replace automatic policy once their affected checks pass; supported
backend-specific behavior remains explicit. No production deployment is part of
this planning task. The owner controls deployment under existing workflow.

For task merges, dispatch the Effort development gate, run the focused behavior
regressions locally, and include their real `Regression-Test:` anchors in PR and
landing messages. Behavior-changing work uses `fix(` or `perf(`. Freeze task
merges, integrate current main, qualify that exact tree and pass Main promotion
gate before promotion. Preserve passing evidence only while its inputs remain
unchanged. Do not treat effort compilation as full release qualification.

## 11. Acceptance — failure cases count as much as capacity gains

The following are required test specifications, not tests claimed to exist.
M0 assigns actual module/test names before implementation. No zero-test success
or skipped platform case counts as a pass.

| Test class | Required observations |
|---|---|
| Policy and replay | Fake clock; unknown/stale/corrupt samples; non-finite/overflow inputs; changes of recipe/backend/quota; duplicate observations; bounded history; explicit caps and zero semantics |
| Atomic ownership | Simultaneous starts; family/predecessor overlap; cancellation in every phase; panic, failed wait, child descendants; no permit release before reap; no double count of shared audio or DVR transport |
| Measurement | Startup/steady separation; paced live input; full-buffer holds; slow client vs slow source; cached responses; blocked output; sample loss; external contention; no falsely inferred spare capacity |
| Trial protection | Warm VOD incumbents and newcomer; no fresh runway; unknown memory; startup timeout; cleanup timeout; workload complexity spike; thermal slowdown; cooldown and recovery without oscillation |
| Full processing coverage | SDR transcode, hardware/software decode/encode, HDR/DV, subtitles, audio-only, copy/remux, shared continuous renditions, Live TV, timeshift and recording with background indexing |
| Cluster correctness | Local refusal with healthy peer; requested quality preserved; remote prepared output on cold peer; missing tools/source; all peers full; stale offers; concurrent ingresses; timeout/partition during each transaction phase; mixed versions |
| Delivery bottlenecks | Shared NAS load, saturated ingress egress, remote worker saturated read path, cold/warm cache; relocating processing never falsely clears relay/drain commitments |
| Service semantics | Manual/exact request vs Auto fallback; seek and resume; source mutation; capability/settings change; successful and cancelled replacement; generation-bound badges and truthful diagnostics |
| Maintenance | Drain during offer/reservation/prepare/adopt; new work fenced; active recording and relay remain blockers; no false reboot-ready state |

M7 uses licensed/public or synthetic reproducible sources, never requires private
movies. Run single-stream and mixed two/three-stream scenarios on Linux Intel,
AMD/NVIDIA where the backends are supported, macOS Apple Silicon, Windows with
its supported backends, CPU-only, and a resource-limited container. Physical
cases establish only the tested configurations; algorithm tests and portable
fallbacks support unknown installations. Retain an explicit unavailable-platform
list rather than claiming universal hardware validation.

Initial release criteria: no correctness/ownership regressions; no missed DVR
capture attributable to trials; no incumbent rebuffer attributable to a trial
in the controlled campaign; all induced overloads stop new expansion before
protected runway is consumed; same-quality peer placement passes when feasible.
Relative to the baseline, p95 start/seek latency must stay within the existing
route SLA and within 10% or 250 ms, whichever additional allowance is greater;
any exception needs a documented trade-off before promotion. Telemetry overhead
must remain below 1% of one CPU core and 16 MiB per node in the 100-workload
synthetic observer load test, with bounded queues and label cardinality.
These are acceptance targets to measure, not current performance claims.

Evidence includes source and binary hashes, platform/driver/effective limits,
input identities, full graph recipe, actual producer count, elapsed service and
runway traces, worker/ingress/source placement, refusal reasons, exit codes and
physical cleanup proof. Do not claim capacity improvements from raised limits
without sustained concurrent playback measurements.

## 12. Build-time commands and current planning validation

These are the existing workflow entry points; focused implementation tests must
be filled in by each milestone, never replaced by the generic docs checks:

```bash
scripts/quiet python3 -m unittest discover -s tests/operations -p test_docs_index.py
scripts/quiet python3 -m unittest discover -s tests/operations -p test_infra_names.py
make paperwork
```

Before Rust changes follow [the agent compile loop](../ci/AGENT-COMPILE-LOOP.md)
and [the development pipeline](../DEVELOPMENT_PIPELINE.md). Replicated storage
regressions require `make unit-core` or `--features hiqlite-store`. API/settings
and client changes update their reference docs, release counters and indexes in
the same implementation commits.

Planning validation and review dispositions will be recorded below. No runtime
behavior has been changed by this document.

## 13. Review ledger and decisions still requiring evidence

Fable 5.1 review is requested through the user-selected coordination chat.
Review the complete plan, source anchors and affected ownership seams. Require
an explicit verdict, numbered findings with counterexamples, and a recheck of
blocking fixes against the final document hash. Reviewer output and model/task
identity must be retained; absence of findings is not an approval receipt.

Open measurement questions for M7: trial margin/duration, class cardinality,
thermal aging, observer overhead and safe memory bounds for specific backends.
They are explicit qualification work, not reasons to require per-server operator
tuning. The full-client evacuation protocol remains a separate project (§9).
