# Adaptive playback resources — learn capacity and use the whole cluster

**Status:** implementation active · qualification not run ·
**Written:** 2026-10-20 · **Approved design baseline:** `a06be68b90` ·
**Owner:** Codex planning session · **Review:** Fable 5.1 changes addressed;
final disposition in [the review ledger](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md).

Companion to [PLAYBACK.md](../PLAYBACK.md) (delivery behavior),
[the cluster performance contract](../cluster/CLUSTER-PERFORMANCE-PLAN.md)
(placement and authority), and [the performance handoff](PERF2-IMPLEMENTATION-HANDOFF.md)
(existing performance work). This is the canonical implementation and acceptance
ledger for automatic resource management across playback pipelines. The current
implementation and qualification state is tracked in the
[delivery log](ADAPTIVE-PLAYBACK-DELIVERY.md) and
[resource status](ADAPTIVE-PLAYBACK-RESOURCE-STATUS.md); qualification remains
open. Build in milestone order; re-verify source anchors against the intended
branch before editing. If adaptation appears to require weaker authorization,
an unowned producer, a false quality badge, or an unfenced replacement, amend
this design rather than weakening those contracts.

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

This source-backed map records known production seams, not an exhaustive proof
that every spawn or sink is covered. The selected ordinary software VOD path is
being integrated; each remaining adapter below is still open. Workload-specific
deadlines and recovery actions remain distinct; one generic “speed above 1×”
test is wrong.

| Work | Resources and service obligation |
|---|---|
| Direct play, byte ranges and cached segments | Read throughput, open handles, memory, egress; requested bytes by response deadline |
| Progressive/HLS copy and remux | Demux CPU, source I/O, audio work, packaging, scratch and egress |
| Software and hardware transcode | Decode/filter/encode engines, CPU time, RAM/VRAM, I/O; playable media by deadline |
| HDR/DV/HLG/tone mapping, scaling and deinterlacing | Whole graph, including helper children, transfers and authoring |
| Audio-only and subtitle work | CPU, decoder processes, extraction I/O and memory; preparation deadline or realtime output |
| Continuous/adaptive VOD | Audio/video rendition family, successor overlap and startup peaks |
| Live TV and timeshift | Tuner/source limits, paced input, decode/encode, ring buffers and transport latency |
| DVR recording | Committed capture and write deadlines, bounded queues, storage capacity |
| Background indexing, previews, pretranscode and probes | Accounted, bounded work that yields to foreground and capture |
| Cluster relay and remote source read | Worker, source node and ingress resource domains; network in both directions |

The [current production-owner map](ADAPTIVE-PLAYBACK-DELIVERY.md#production-owner-map--open-adapters)
records concrete constructors, terminal owners and missing runtime adapters.

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

Verified baseline anchors; recheck at implementation time:

| Anchor | Existing contract to preserve |
|---|---|
| [admission.rs](../../crates/plurxd/src/admission.rs): `try_admit_bundles_claiming` | Atomic all-or-none ownership, priority and family claims |
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

`ResourceReservation` is process-local authority retained by the existing
producer/reaper owner; it is never serialized. **Drop does not free counted
capacity after a spawn may have occurred.** It transfers custody to supervised
cleanup; panic, failed wait or missing cleanup ownership leaves an explicit
`UnknownHeld` ledger entry. Only a matching `ConfirmedProducerReap` plus writer/
descendant settlement, or an equivalent confirmed non-process close receipt,
releases it. A never-spawned reservation can release with proof that no child or
writer acquired it. Recovery must reconcile `UnknownHeld`; timeout cannot erase
it. A machine reboot invalidates process identity only after OS boot identity
and actual surviving-owner checks establish that the old workload cannot exist.
It supports atomic family acquisition and phase transitions. Moving from startup to steady state may
release only resources demonstrably no longer held; reacquisition is mandatory
before allocating them again. Queue residency and memory remain accounted while
paused. No scheduler mutex spans awaits or process creation.

Preserve the existing maximum of three roles per continuous family (video,
audio, companion), verified in `try_create_with_release_fence`; both current
family/bundle admission APIs bound inputs to three. A proposed adaptive overlap
transaction admits at most two such families, six distinct roles total, under
one pool lock. It retains and counts existing predecessor roles once, reserves
only missing successor roles, and validates both family identities. It must not
split a four-role audio/video overlap into independently visible reservations.
Reject a third family or seventh role without mutation. M2/M3 tests cover four
and six roles, shared retained roles, overflow and zero partial admission. The
legacy three-role helpers keep their bound; no unbounded vector widening.

Use typed refusals: `HardLimit`, `ExplicitCap`, `InsufficientEvidence`,
`TemporaryContention`, `DeadlineBudget`, `CapabilityUnavailable`,
`PeerUnavailable`, `SourceUnavailable`, `Maintenance`, `OwnershipConflict`.
Each carries the limiting domain, requested/available amount if known, evidence
age and whether another worker or later retry can help. Never collapse these
into “needs a GPU slot and threads.”

### 4.3 Compatibility and settings migration

Existing keys `transcode.max_hw_sessions` and
`transcode.software_pool_threads` keep distinct meanings: hardware zero disables
hardware; zero/undersized software budgets permit one saturating idle-pool job,
not a ban. Physical caps still bind. Family/bundle and approval details:
[review §6](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md). Add explicit
provenance to new writes, with four read states:

| State | Decision and transition |
|---|---|
| Absent | Automatic policy; new installation records automatic provenance without persisting resolved numeric defaults |
| Legacy-ambiguous | Enforce stored number as a ceiling; show “legacy value: confirm limit or switch to automatic”; only an explicit operator choice resolves it |
| Explicit | Enforce the saved ceiling; automatic learning cannot override it |
| Automatic | Numeric legacy cache is ignored for admission; effective policy and hardware limits are reported separately |

GET exposes stored provenance separately from effective values. Untouched
GET→PATCH form round-trips must preserve mode and omission, never convert a
resolved default into an explicit cap. Write a mode only for an explicit operator
choice or new-install initialization. Inventory `http/system.rs` GET/PATCH,
web/native settings forms, CLI/setup, imports and test fixtures in M0. A legacy
writer that changes a numeric limit becomes legacy-ambiguous and enforced; a
capability-unaware write must never silently enable expansion. Rollback readers
retain the stored conservative numeric ceiling until upgraded again.

Existing ambiguous installations may need a one-time choice of Automatic;
inferring intent would violate a real administrator cap. This is not per-box
benchmarking. The refusal text must expose this migration issue instead of
claiming hardware shortage. No gratuitous Developer enable switch: read-only
evidence may live there; permanent resource limits and Automatic belong in
normal settings. M2 acceptance includes untouched round-trip, explicit zero,
legacy writer, mode selection and rollback cases.

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
LRU eviction), with a schema version and atomic replacement. Use a node-instance
specific path under its data directory, protected by an exclusive lifetime lock.
A second daemon sharing that path cannot write or treat the first daemon's
records as its own; it reports the conflict and uses isolated volatile history.
Physical resources shared by separate daemons remain external contention unless
an existing common resource authority coordinates them. High-frequency
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
Fresh known-capacity worker FIRST -> reserve -> prepare -> activate
                 |
Unknown capacity -> bounded trial only after reserving peer-attempt time
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

Before a local unknown-capacity trial, rank any fresh known-capacity peer ahead
of it. Partition the original remaining deadline B: retain final activation/
fallback allowance A from the existing create contract, and reserve peer budget
R = max(3 seconds, the best peer's conservative prepare-latency estimate). A
local trial may spend at most min(5 seconds, B/2, B−A−R). Skip it unless that
allowance covers verified startup and cleanup bounds plus 2 seconds of useful
production; give the best eligible peer B−A instead. Unknown bounds prohibit
speculation beside incumbents. No history means the
3-second floor is a budget reservation, not evidence a peer can prepare in it.
Update the worker estimate from actual cold starts. A failure to fit any attempt
is `DeadlineBudget`, never cluster exhaustion. Do not spend A on exploration.

The coordinator may overlap at most two prepares only for distinct admitted
resource domains and with a separate provisional route/attempt identity for
each (§7.2); the first eligible prepared result is selected through one durable
winner CAS, and losers are fenced and physically cleaned up. Otherwise prepare
sequentially. Both share the original deadline, R and the §7.1 total attempt cap.
M5 must prove a local trial cannot consume the peer's reserved allowance and
that a fresh healthy peer wins before an unknown local trial.

A cold server uses existing conservative eligibility and a bounded first
preparation window. No full-library startup benchmark or required operator
calibration. Unknown classes remain discoverable through real work or admitted
idle-time probes; a previous refusal must not permanently blacklist a class.

### 6.2 Trial budget and acceptance

Initial policy; M7 must calibrate it:

- At most one additional speculative producer graph per node, also excluding
  conflicting trials on a shared device or source domain. Trials obey hard
  memory/device/admin bounds and the original request deadline.
- Trial runtime obeys the stricter §6.1 allocation. It cannot extend the start
  SLA. A production observation interval is a one-second monotonic sample of
  verified, serveable output progress, not an HLS segment. Require three samples
  spanning at least 2 seconds with actual output advancement. If the pipeline
  publishes only whole 4–6-second segments, it may produce insufficient evidence
  in this trial; use history/another worker or a later admitted idle trial. Never
  count unverified internal frames or increase the startup deadline to pass.
- Expansion beside VOD incumbents requires each affected incumbent to have
  trusted runway of at least `max(20 s, trial budget + cleanup bound + 2 segments)`.
  The process owner must have a tested cleanup bound; absent one means no trial
  beside incumbents. No extra lookahead is forced merely to qualify a trial.
- Paced Live TV/DVR and unbuffered copy cannot supply that protection. Do not
  explore unknown shared bottlenecks beside them. Use fresh previously observed
  concurrent capacity, another worker, or a trial while the affected domain is
  idle. Capture work must never be sacrificed to learn a VOD limit.
- For `producing` VOD, require at least 1.25× active production with adequate
  runway across its observation window. For `buffer-paced` incumbents, evaluate
  the protected runway floor and normal consumer drain, not 0× production: a
  buffer can decrease normally until its existing refill threshold. For
  `input-paced` work, evaluate queue/deadline slack, not a 1.25× speed target;
  unknown expansion beside such work remains barred by the preceding rule.
  Paused viewers impose no output-speed obligation but keep residency counted.
  A paced→producing transition restarts that producer's speed window and retains
  the runway guard; it cannot supply positive expansion proof until observed.
  Continue monitoring through scene/state changes; this is provisional evidence.
- Abort a trial immediately on hard-limit pressure, an incumbent deadline miss,
  or loss of the runway reserve. Two consecutive below-margin observations while
  actually producing, after its measurement window is established, also abort. Stop/reap the newcomer and confirm release
  before another attempt; keep the incumbent's recipe and ownership unchanged.

The policy must account for CPU needed by HTTP/control/cleanup, GPU allocation
peaks and shared-source effects even when device counters are unavailable. No
finite probe guarantees future movie complexity; runtime recovery is mandatory.

### 6.3 Ongoing scheduling and fairness

M0 inventories current Live/Background/Speculative gating, including the blanket
`Live if background_active` refusal. M3 must replace whole-budget fragment/
subtitle/index reservations with registered preemptible workloads, bounded
estimates and a tested yield/reap path. At foreground arrival, stop disposable
admitted work before classifying a capacity refusal. The yield wait consumes
the same B/A/R budgets; missed yield bounds leave resources held and try peers.
This is a mandatory M3 deliverable, not an optional follow-up. A background
registration without a proven yield path stays conservatively serialized and
visible as an explicit incomplete adapter; M3 cannot claim all-path completion.

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

A real margin/deadline/pressure failure starts a 60-second node/class cooldown;
intentional pacing, missing samples, protocol failure or insufficient evidence
do not record a capacity failure or trip that cooldown. Capacity growth needs
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

1. **Claim provisional owner first:** under the existing durable request claim,
   record the selected worker, principal, incarnation, source/settings digest,
   fixed deadline and monotonically fenced owner epoch before any remote spawn.
   The provisional route is not client-serveable. This is a state extension of
   the existing route lifecycle, not a bearer token replacing authorization.
   Retries resolve that same request/attempt; a different principal conflicts.
   M0 specifies schema migration and strict rolling-version refusal.
2. **Prepare under the epoch:** send an authenticated, versioned intent and
   expected owner epoch (and predecessor epoch for replacements). The worker
   rechecks the durable provisional assignment, serving/maintenance fences,
   source, graph and client before atomic admission and again before sealing
   output. It owns source handles, executable, credits and cleanup throughout.
3. **Prepared response:** return a scoped handle bound to boot/serving identity,
   principal, request/attempt, owner epoch, source/settings and recipe/output.
   Its expiry cannot exceed the original create deadline. It is a reference to
   held state, not a serialized reservation or a client-visible capability.
4. **Select and adopt:** commit one winning attempt through the durable request
   CAS, fence losers, then transition the matching provisional route to active
   under the same expected epoch after ordinary candidate/output validation.
   Duplicate adoption resolves the same owner. For two permitted parallel
   attempts, use separate provisional route IDs/epochs under the common request;
   only its recorded winner can activate. No unfenced pre-owner producer exists.
5. **Cancel/recover:** abandoned/expired/losing preparations remain counted and
   are owned cleanup work. Adoption/activation outcome unknown is reconciled
   through durable request/owner status before another conflicting preparation.
   A request timeout or expiry prohibits new adoption; it never proves release.
   Draining workers reject new Prepare/Adopt and retain cleanup obligations.

Reuse `RemotePrepareRequest` only under these existing owner/predecessor epoch
semantics. M0 defines exact wire fields and schema transitions; M4 implements
and tests the duplicate/crash/partition matrix for every arrow. No new message or
field is sent until a fresh authenticated capability advertisement proves that
peer implements this protocol; `deny_unknown_fields` peers never receive an
optimistic probe. An authenticated pre-handler deserialization refusal from an
advertising worker is a confirmed protocol rejection, not a load signal. A
transport failure without such a response remains outcome unknown. Older peers
are filtered before Prepare and retain only their supported ordinary routes.
Relax the remote-processed catalog restriction only after this full worker-owned
path is proven. A parsed description alone still grants no executable authority.

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
- “Remote attempt budget exhausted after local trial” identifies a budgeting
  failure, not full peers; the trace includes the reserved R and actual spend.
- “Legacy resource value: confirm limit or switch to automatic” distinguishes
  migrated settings from both measured shortage and an intentional new cap.
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
including for probes. Prepare and Adopt recheck the fence; prepared but
unadopted, provisional, expired and losing preparations are explicit drain
blockers until confirmed cleanup, even if no active client route exists.

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
| M0: inventory and contracts | Admission/process owners; spawn paths; settings writers; confirm durable request-claim/route lifecycle anchors; related plans | Checked coverage table maps each spawn/relay/sink to one owner, resource domains and phase; exact proposed wire/types and crash matrix reviewed; no route silently excluded |
| M1: observations and diagnostics | Portable observer module, process wrappers, production observers, metrics | Deterministic CPU/time/memory/progress tests; Linux/macOS/Windows observations or explicit Unknown; no decisions changed yet; bounded overhead evidence |
| M2: node policy and learning | `admission.rs`, bounded local history, settings migration | Atomic vectors, four-state settings migration, stale/unknown cases, real-failure cooldown, UnknownHeld/reap and bounded family-overlap tests |
| M3: all pipeline adapters | VOD finite/rolling/continuous; live/DVR; audio/subtitles/copy; background/census | Every M0 row uses service; family/share accounting; whole-budget background holder replacement with tested yield/reap; paced/producing trials and recovery |
| M4: worker-owned remote preparation | `media_pool.rs`, `media_sessions.rs`, HLS creation, worker admission | Same-quality remote preparation/adoption for ordinary and processed output; all transaction crash/duplicate cases; old/new peer compatibility; remote reservation never reconstructed from a description |
| M5: cluster policy and delivery domains | Offers/ranking, shared source claims, relay telemetry and maintenance hooks | Local-full/peer-idle and local-trial-ran cases preserve peer time and quality; NAS/relay saturation refuses misleading placement; simultaneous ingress, partitions, draining nodes and bounded search tests |
| M6: operational integration | Normal settings, Developer read-only facts, API/operations/reference docs, client diagnostics | Existing caps retain meaning; truthful fallback/placement explanation; no new feature toggle; drain blockers and client compatibility demonstrated |
| M7: qualification and promotion | Integration owner: exact final source, tests and receipts | Deterministic suites, real-process cleanup, platform matrix, mixed-load campaigns, independent review, current-main promotion gate and qualification receipt |

M1 is an instrumentation-only implementation phase, not a permanent shadow-mode
switch. M2/M3 replace automatic policy once their affected checks pass; supported
backend-specific behavior remains explicit. No production deployment is part of
this planning task. The owner controls deployment under existing workflow.

Follow [the development pipeline](../DEVELOPMENT_PIPELINE.md): normal commits,
focused regressions and Effort development gate per task; `fix(`/`perf(` for
behavior changes with real regression trailers. Freeze, integrate current main,
qualify that exact tree and pass Main promotion gate before promotion. Effort
compilation is not release qualification. Preserve evidence only while its
inputs remain unchanged.

## 11. Acceptance and commands

[The acceptance contract](ADAPTIVE-PLAYBACK-RESOURCE-ACCEPTANCE.md) contains the
required deterministic, process, cluster, platform and performance matrix,
exact baseline procedure and existing build/check commands. It is normative
for M0–M7, not optional supplementary evidence. Every implementation milestone
assigns real regression anchors; no skipped or zero-test result is a pass.

## 12. Planning validation

Initial draft: paperwork 14/14, index 6/6, infrastructure 14/14 and links passed.
These check documents, not runtime; re-run affected checks after revisions.

## 13. Review ledger and decisions still requiring evidence

The independent Fable 5.1 review requested F1–F9 and suggested O1–O4. All are
addressed in this revision; the same reviewer must verify closure. See
[the review ledger](ADAPTIVE-PLAYBACK-RESOURCE-REVIEW.md) for identities, reviewed
hashes, findings, changes and the final verdict. No implementation is claimed.

M7 still owes calibration of trial margin/duration, class cardinality, thermal
aging, observer overhead and backend memory bounds. This is qualification work,
not per-server operator benchmarking. Full-client evacuation remains separate
(§9). M0 must finish exact types/wire/schema and executable test anchors before
any affected implementation; plan approval alone does not waive that gate.
