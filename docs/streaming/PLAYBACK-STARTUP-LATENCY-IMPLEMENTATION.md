# Playback startup latency — recover fast starts without moving the wait into a stall

**Status:** October 2 candidate built; M2 continuity/native qualification pending,
final implementation review not started · **Written:** 2026-09-29 EDT ·
**Executes:** the measured library-playback startup investigation below ·
**Runtime changes:** none in this document change.

> **Execution amendment, 2026-09-29 EDT:** The user has authorized Sol to
> build this work in a separate clone, with proper commits batched into one
> main-bound draft PR, one final implementation review, then fast-lane tests
> and merge when green. The [build handoff](PLAYBACK-STARTUP-LATENCY-BUILD.md)
> supersedes this plan’s earlier task-PR, per-task review and local-unit-run
> sequencing. Its workspace and test rules govern execution; the runtime
> contracts and acceptance requirements below remain in force.

Companion to [PLAYBACK.md](../PLAYBACK.md) (delivery and recovery contracts)
and the [Safari seek handoff](SAFARI-SEEK-IMPLEMENTATION.md) (native seek
ranges and preparation). This document owns click-to-first-frame latency for
finite library playback. Execute its milestones serially. Preserve the
actor's ownership, media validation and storage contracts; if faster startup
requires weakening one, record the failed candidate and revise the design.

The independent review and its dispositions belong in
[PLAYBACK-STARTUP-LATENCY-REVIEW.md](PLAYBACK-STARTUP-LATENCY-REVIEW.md).

> **Authorized amendment, 2026-10-02 EDT:** the user authorized the revised
> incident packet with “ok build it”. The appended contract is folded into
> this repository against build base `dea1a403e`. Track N and the publication
> candidate are in progress; measured qualification and deployment remain
> separate. See [the incident evidence](PLAYBACK-STARTUP-LATENCY-M0-20261002.md)
> and [the current build status](PLAYBACK-STARTUP-LATENCY-STATUS.html).

## 1. Evidence — one measured slow start, with a bounded causal claim

### 1.1 The observed attempt

The user reports a regression from roughly one second to ten seconds across
clients. The inspected example is Safari on `media1`, resuming Shameless
S05E05, file 6456, at requested film time 2000 seconds. The server ran
`38c917225` (`v0.3.0-5228-g38c917225`). The relevant publication sources are
identical between that runtime revision and the planning checkout
`b5fa758644d413ef73000ded00cedc15766eba02`.

These log timestamps are **UTC on 2026-09-30**, which is September 29 EDT.
The click origin is inferred from the client TTFF report; request receipt
times and filesystem timestamps are server observations, not a synchronized
browser/server performance trace.

| Event | UTC | Approximate elapsed from click |
|---|---|---|
| Client click, inferred | 02:00:47.916 | 0 s |
| Decision: H.264 copy, DTS to AAC, MKV remux | 02:00:48.434 | 0.52 s |
| VOD refusal `vod_index_pending`; rolling fallback selected | 02:00:50.418 | 2.50 s |
| FFmpeg spawn arguments logged | 02:00:50.901 | 2.98 s |
| Copy session registered; achieved origin 1992.035 s | 02:00:51.545 | 3.63 s |
| Browser manifest dispatch 1 received | 02:00:52.076 | 4.16 s |
| First completed segment mtime | 02:00:53.866 | 5.95 s |
| Sixth completed segment mtime | 02:00:59.860 | 11.94 s |
| Copy init validated before first media handoff | 02:00:59.953 | 12.04 s |
| Client first-frame report received: `ttff_ms=14210` | 02:01:02.127 | 14.21 s |

The first six segment durations were 10.427, 12.804, 7.758, 8.675, 11.303
and 7.591 seconds, totaling 58.558 seconds. The achieved-origin lead was
7.965 seconds, so the current requested-position-plus-48-second threshold
is 55.965 seconds from the achieved origin. The sixth segment crosses it;
the fifth, ending at 50.967 seconds, does not.

This alignment is strong evidence that the publication gate contributed
approximately six seconds after the first segment existed. It is not a
controlled before/after proof, and the init-validation log alone is not a
measurement of time spent validating. Approximately six seconds were already
spent before the first segment; removing the publication wait cannot by
itself restore a one-second start for this attempt. The final approximately
two seconds include manifest/media delivery and browser presentation.

### 1.2 What the index observation proves

A read-only query found no local `fragment_indexes` row for files 6456 or
6708; the sidecar held 2310 rows overall. The job metrics snapshot showed 60
queued fragment-index builds, zero running builds, and six running subtitle
extractions. These are point-in-time observations. They do **not** establish
starvation, disabled preparation, fleet-wide absence, or a broken scheduler.
Neither an index on another node nor a queued job proves this node can serve
the requested exact recipe. Inspect those states in M1.

Private raw captures remain at `/private/tmp/plurx-startup-media1.log` and
`/private/tmp/plurx-startup-history.log` on the investigating Mac. They contain
private media paths and are temporary diagnostics, not repository fixtures.
The sanitized event table and duration sequence above are the retained
incident record. M0 must create fresh, reproducible evidence.

### 1.3 The historical change and the evidence limits

Commit `52c1a0fab` on September 19 introduced the rolling publication clock
with an initial runway of three times the fixed 16-second presentation
target: 48 seconds at 1×. This sits above the copy writer's existing
12-second playlist gate. Later pacing changes made the publication allowance
relative to accepted demand and its timeline.

This identifies a plausible regression mechanism; it does not establish
that every reported slow start uses it. No pre-change one-second baseline,
Android trace, Apple-device trace or Live TV startup trace was captured in
this investigation. Keep direct play, immutable VOD, rolling copy and rolling
transcode separate throughout qualification.

## 2. Scope — two causes to investigate, one integrated delivery lane

**In scope:** finite library cold start and resume; time to create, first
complete media, first served playlist and first rendered frame; exact-index
availability and bounded playback-triggered preparation; rolling startup
publication and its transition to steady pacing; regressions on seek,
replacement, pause and end caused by those changes.

**Non-goals:** Live TV tuning; unrelated Safari seek redesign; changing codec
or HDR quality to hit a time target; low-latency HLS protocol introduction;
re-encoding copy-compatible video solely to shorten GOPs; library-wide forced
requeues; resetting production state; replacing the cluster scheduler.

Do not hide the regression by moving the TTFF origin, treating `playing` as
first frame, increasing timeouts, silently enabling a Developer setting, or
requiring a complete file scan on the Play request. Do not shorten copy
segments indiscriminately: open-GOP boundaries have an existing measured
frame-drop cost. Do not claim subsecond startup for a cold source whose
first decodable media takes several seconds to produce.

This is a multi-task effort with overlapping files. Use
`effort/playback-startup-latency`; task branches use `codex/startup-<milestone>`
and target the current effort. No file-disjoint-main exception applies.
Planning and review are the present authorization; this document does not
claim implementation, deployment or production repair has happened.

## 3. Source map — reverify these owners before implementation

| Owner | Current entry points and responsibility |
|---|---|
| [Rolling clock](../../crates/plurxd/src/transcode/rolling/publication.rs) | `RollingPublicationClock::publication_budget_at`, `rolling_explicit_publication_ends`, `ROLLING_INITIAL_RUNWAY_MS`; budget and timeline authority |
| [Rolling session](../../crates/plurxd/src/transcode/rolling/session.rs) | `publication_cycle_at`; reads writer output, selects an admitted prefix, commits actor publication and snapshot deadlines |
| [Flow and scratch](../../crates/plurxd/src/transcode/rolling/flow.rs) | `rolling_initial_runway_ms`, `rolling_startup_bytes`; production allowance, rate scaling and reservation sizing |
| [Segment accounting](../../crates/plurxd/src/transcode/rolling/segment_index.rs) | `SegmentIndex`, `server_ready`, served playlist rendering and first-playlist checks; achieved-origin-relative segment bounds |
| [Copy writer](../../crates/plurxd/src/copyseg.rs) | `Limits`, `publish_gate_secs`, temporary writes and publication; 12-second writer gate |
| [Media constants and arguments](../../crates/plurx-core/src/transcode/mod.rs) | 2-second first copy floor, 6-second usual floor, 15-second preferred ceiling, 16-second hard presentation target, 90-second initial read burst |
| [First-media validation](../../crates/plurxd/src/transcode/session_request.rs) | `validate_copy_init_before_publication`; complete segment/init, decoder configuration and attempt fencing |
| [Response admission](../../crates/plurxd/src/transcode/manager/publication.rs) | Exact response owner, actor first-media handoff, publication wait and terminal refusals |
| [Session routing](../../crates/plurxd/src/transcode/manager/create.rs) | Immutable VOD first; typed prerequisite refusal permits configured rolling recovery |
| [VOD creation](../../crates/plurxd/src/vod/serve/create.rs) | Exact local index/artifact lookup, preparation request and typed refusal |
| [Preparation workers](../../crates/plurxd/src/state.rs) | `enqueue_copy_preparation`, discovery, cluster index execution and local hydration |
| [Durable job lifetime](../../crates/plurxd/src/background_jobs.rs) | Claims, renewal, fenced publication and cleanup; queue work must use this owner |
| [Store contracts](../../crates/plurx-core/src/store/background_jobs_tests.rs) | Both backends' job lifetime/priority regression anchors |
| [Web creation and attach](../../crates/plurxd/src/web/player/decode-tiers.js) | `startCopyHls`, capability-driven native/MSE choice, cancellation and fallback |
| [Web timing](../../crates/plurxd/src/web/player/measurements.js) | Click-to-presentation measurements; keep their origin intact |
| [Control actor](../../crates/plurxd/src/playback_control.rs) | Accepted demand, attempt fencing, presentation-proven startup protection and terminal verdicts |

Current signatures, copied for orientation (do not introduce duplicates):

```rust
pub(super) fn rolling_initial_runway_ms(rate: f64) -> i64;
pub(super) fn rolling_startup_bytes(
    bitrate_bits_per_second: Option<f64>, playback_rate: f64,
) -> i64;
pub(super) fn rolling_explicit_publication_ends(
    demand: &PlaybackDemandSnapshot,
    observation_age: Duration,
    media_origin_ms: i64,
) -> RollingExplicitPublicationEnds;
```

## 4. Contract — separate startup readiness from steady reserve

### 4.1 One timeline and three distinct frontiers

Retain produced, served and client-presented frontiers as separate facts.
All policy math uses integer media milliseconds relative to the achieved
origin, with an explicit conversion from absolute requested position. A
resume cannot count media preceding the requested position as usable runway.
Use contiguous decodable coverage containing that position, not a byte count
or the final timestamp across a hole. Segment endpoints are measured from
actual media; never derive them as ordinal times target duration.

The candidate must keep:

```text
served_end <= completed_produced_end
served_end <= accepted publication allowance, including existing rounding rule
startup coverage contains the requested position and enough post-position media
protected history + reserve + guards <= served window and charged scratch
```

Validate init and first-media ownership before release. No successful
filesystem read or readiness counter substitutes for actor admission. A
predecessor's media, stale observation or expired owner cannot open the gate.

### 4.2 Proposed policy shape and the M2 selection gate

Introduce one internal policy value resolved when the attempt is created:

```rust
// Proposed interface, not present in the current tree.
struct RollingPublicationPolicy {
    startup_runway_at_one_x_ms: i64,
    active_low_reserve_at_one_x_ms: i64,
    steady_reserve_at_one_x_ms: i64,
    low_reserve_interval: Duration,
    steady_interval: Duration,
    publication_hard_budget: Duration,
}
```

Resolve it from the actual engine and a proven transport class; unknown or
legacy clients retain the currently qualified policy until their class is
tested. Do not infer a stronger client capability from a User-Agent string.
Do not add a new wire capability unless M2 proves it necessary and the
protocol/client compatibility contract is updated in the same task.
M2 candidate controls belong in the isolated harness. Do not ship an
unqualified hidden production switch; any unfinished user-selectable mode
must follow the repository's Developer-switch lifecycle.

The startup minimum, steady reserve, production allowance and scratch
reservation must no longer share a constant merely because their current
values coincide. A lower first-playlist threshold does not lower the storage
budget needed to survive the transition or authorize unbounded production.
Do not replace every use of `rolling_initial_runway_ms` mechanically.

**M2 must select actual values before M3 begins.** Sweep candidate first
runways of 12, 16, 24, 32 and 48 media seconds at 1×, scaled for consumption
at 0.25×, 0.5×, 1×, 1.5×, 2× and 4×. Test values just inside/outside the
existing 0.25×–4× clamp and the retention-derived reserve cap. These are
experiment values, not shipping defaults.
The existing writer gate remains 12 seconds in the initial experiment.
Record effective readiness after both gates, keyframe rounding and resume
lead. A policy unable to improve the measured route does not pass by making
only an indexed control faster.

For each candidate establish this bound with measured or enforced terms:

```text
usable startup media >= playback rate *
    (worst next discoverable-playlist delay
     + bounded next-media transfer/append delay + scheduling allowance)
```

The first term includes server publication, actual client reload timing and
unchanged-playlist behavior. Do not substitute the 250 ms worker poll for
native HLS reload latency. Account for slow producer completion separately:
a client reload cannot retrieve a segment that does not yet exist. If no
credible bound can be demonstrated for a transport/source class, retain the
safe gate for that class and report the limit. Do not fabricate a universal
subsecond promise by selecting an arbitrary smaller threshold.

Prefilter experiments by [RFC 8216](https://www.rfc-editor.org/rfc/rfc8216.html):
with target 16 s, non-final segment-bearing updates must be 8–24 s apart;
initial/changed reload waits are 16 s and unchanged reload waits 8 s.
Assert no prefix removal leaving less than 48 s in a non-final playlist.
Three-target initial-selection advice is a separate client behavior risk,
not a prohibition on short initial playlists. Test actual native selection
and the worst update/reload phase alignment, including unchanged reloads.
An accelerated non-shipped client poll is not qualification evidence.

### 4.3 Startup-to-steady transition

Separate readiness, first presentation and reserve growth. Proposed states
live in the existing publication clock, consume the actor's authoritative
presentation snapshot, and do not create a competing actor lifetime:

| State | Entry and permitted work | Exit / expiry |
|---|---|---|
| Prepublication | Current installed attempt; accumulate admitted complete media and validate it | First qualified snapshot enters AwaitingPresentation; existing prepublication failures/recovery and deadlines still apply |
| AwaitingPresentation | First snapshot fixes availability; qualified low-reserve cadence, existing actor startup protection | Accepted actor presentation proof enters ActiveLowReserve or Steady; existing 30 s presentation deadline ends a non-presenting attempt through its current typed recovery/retirement path |
| ActiveLowReserve | Playback proved; publish eligible prefixes on a cadence M2 proves sustainable without reserve growth | Promote only with enough contiguous reserve for a full steady cycle; no reserve-growth expiry and no presentation-startup exemption |
| Steady | Qualified reserve and current presentation proof | Rate/coverage changes may return to ActiveLowReserve under the selected policy; ordinary actor terminal and lifecycle conditions still apply |

At 1.05× production and 1× consumption, accumulating 24 additional media
seconds takes at least 480 wall seconds. Do not bind that work to the
30-second first-presentation deadline. ActiveLowReserve is a normal bounded
active operating mode: existing resource ceilings, demand leases, process
watchdogs, publication hard deadlines, pause grace and retirement still
apply. It may last for the remaining presentation; reaching 48 seconds is
not a new condition for successful playback. This state is admissible only
if M2 proves its long-run stability and legal publication cadence without
assuming production will become faster. Do not infer sustained capacity from
an initial burst or short speed sample.

M2 must record the pre-release classification rule using facts the request
actually carries or the server has validated. If no sustainable smaller-gate
policy exists for those known facts, retain the old policy **before** the
first response. Unknown transport and legacy clients retain the qualified
old policy throughout the attempt; learning a browser name later does not
opt them in. Slow production after admission uses the existing truthful
capacity/recovery contract, not a newly invented reserve-growth timeout.

Publish completed eligible prefixes on the selected cadence, keeping the
advertised target fixed. Reloads, retries, cadence transitions and accepted
rate changes never restart first availability or the actor's lifetime.
Fetched bytes alone do not mean playback started. Pause or stale demand
preserves existing bounded holding rules in every state.

Grow steady reserve through the cumulative demand allowance. Do not jump the
served live edge, reset media sequence, alter prior EXTINF values or evict
the starting segment at cutover. Rate changes, seek/replacement and origin
changes recompute effective bounds under existing attempt fences:

| Fact | Lifetime and permitted updates |
|---|---|
| Policy version, qualified transport/source class, fixed target, coefficients/cadences | Frozen for the installed attempt; settings or later hints do not rewrite it |
| Effective runway, consumption and allowed endpoint | Derived from current accepted rate/position, achieved origin and fixed policy; clamp/round with checked bounds |
| Rate change in same attempt | Re-evaluate coverage/state and acquire growth before new writes; no deadline reset or retroactive shrink of served snapshots/grants |
| Seek in same attempt | Respect actor timeline sequence and new anchor; preserve already admitted bytes/promises and original startup lifetime |
| New producer attempt or replacement | Reset staged/served attempt-local policy state through existing fencing; preserve presentation-level recovery budgets and predecessor object promises |

No transition may revoke advertised bytes, uncharge retained objects, or
authorize the successor with predecessor evidence. If a supported rate
cannot fit retention/scratch, keep the existing truthful capacity outcome;
do not silently clamp the viewer's requested rate beyond current semantics.
Source exhaustion releases a valid complete short source immediately; an
ENDLIST marker alone does not bypass producer completion or integrity proof.

### 4.4 Fixed target and producer constraints

The initial candidate retains the 16-second rolling target and current GOP
selection, decoder validation and audio routing. Faster server snapshots
alone may not improve a native client that reloads slowly or holds back
multiple targets. Test that explicitly in M2.

If the experiment demonstrates that native improvement needs a different
fixed target, write the concrete follow-on design before implementing it.
It must prove every segment and audio tail fits, account for unknown/long
GOPs and open-GOP frame drops, and freeze the target before the first
response. No mid-session target decrease, unsupported independent-segment
claim, silent video transcode or progressive fallback on an unproved client
is permitted as a shortcut. That expansion requires re-review of the changed
contract; it is not covered by approval of the initial candidate.

## 5. M0 — instrument and reproduce before choosing a threshold

**Owns:** server timing at the current creation/publication owners; fixture
and measurement harness; sanitized receipt. No publication behavior change.

Record monotonic durations for decision, create admission, exact-index
lookup/preparation request, producer spawn, first complete decodable segment,
writer gate open, first served snapshot, first response admitted and client
first presentation. Join by existing safe attempt/request identities. Never
put file, title, session or request IDs in metric labels. Use bounded route,
engine, transport and outcome values; make absent phases explicit.

Do not subtract browser and server wall clocks. Compare browser click-to-frame
with server monotonic phase durations, keeping network/unattributed time
separate. Report duplicate manifest requests and browser recovery separately.
Record current binary SHA, settings, node, media recipe, achieved/requested
origin, cache condition, segment sizes/durations and background load.

Build a generated fixture with variable segment durations and the sanitized
incident schedule: at minimum replay the six endpoints above and the
7.965-second origin lead. Add 1× and 2× paced writers, sparse clean keyframes,
short EOF and a cold storage-open delay. The old gate must wait through the
sixth incident segment; each phase must remain measurable when superseded.

**Acceptance:** repeatable old-tree reproduction; traces distinguish producer
time from policy hold and browser time; first-media validation time is
measured rather than inferred from its log line. Capture a comparable warm
indexed control and a forced missing-index test in an isolated environment.
Do not delete production indexes to manufacture the latter.

## 6. M1 — establish why exact indexes are unavailable

**Owns:** index preparation/hydration and the smallest demonstrated queue
defect, if any. Runs independently of M2's experiment but integrates serially.

Follow the exact identity for the affected recipe across local sidecar,
shared artifact catalog, source attestation, durable request, current worker
claim, eligibility, retry/backoff and local hydration. Sample across multiple
worker cycles; check settings and source availability first. Inspect the
already implemented queue rotation before proposing another scheduler.

Classify absence as never requested, queued and eligible, blocked resource,
disabled, backoff/terminal failure, unsupported source, stale identity,
remote-only artifact or failed local install. Provide the actual reason and
age; `vod_index_pending` alone is insufficient operator evidence.

On a missing exact index, retain a deduplicated bounded preparation request
and immediate eligible rolling fallback. Promote existing work only through
the current priority and lease APIs. Do not launch an extra whole-file read
on every Play, retry a terminal unsupported recipe forever, steal a live
claim, bypass resource limits or promise that enqueue means ready.

If a usable remote artifact exists, use existing verified hydration under
its deadline and resource owner; do not put indefinite remote fetch on the
critical path. If contention/priority is proved, repair fair admission with
tests for both index progress and continued subtitle progress. Otherwise
finish M1 with its diagnosis and explicitly no scheduler patch.

**Acceptance:** two repeated opens deduplicate the same recipe; successful
exact publication/hydration makes a later open take VOD; source/engine change,
lease loss, terminal failure and disabled preparation retain truthful
behavior. Queue progress is measured over an observation window with known
eligible work and resource availability, not inferred from queue depth.

## 7. M2–M4 — qualify a policy, build it, then remove remaining producer delay

### 7.1 M2: transport experiment and decision receipt

Use the same generated media, first-segment availability schedule and
bandwidth for old and candidate server snapshots. Exercise shipped hls.js,
Safari native HLS, AVPlayer on a physical Apple TV and ExoPlayer on a physical
Android/Google TV device. Use the shipped client route, and separately force
native/MSE only in the harness to isolate transport effects. The observed
Safari H.264 example used hls.js; Safari must not automatically be classified
as native HLS.

For each runway candidate capture first frame, reload requests, fetched and
buffered intervals, presentation progress, live-edge/seekable movement,
stalls through the first two minutes, dropped frames and resource peaks.
Test the writer at 1.05×, 1.2× and 2× consumption and one bounded 2-second I/O
pause, plus a below-consumption negative control. Exercise initial/changed
and unchanged reload phases, legal update intervals, and early retention.
Reject protocol-invalid candidates before comparing latency. A producer
slower than consumption must report that limit truthfully.

**Acceptance:** a checked-in decision receipt chooses the exact startup
minimum, cadence, hard budget, transition criterion and qualified transport
classes. It includes failing candidates and a source/readiness bound, plus
an executable trace for the selected policy. Its state table must distinguish
actual steady cutover from indefinitely sustainable ActiveLowReserve; run
the 1.05×/1.2× traces through cutover where reachable and verify stable
non-transition otherwise, including pause, rate increase and scratch refusal.
Physical continuity must cover the actual transition even beyond two minutes.
If none passes, revise §4;
M3 is blocked on technical evidence, not silently assigned guessed defaults.

### 7.2 M3: implement one policy across clock, flow and accounting

**Owns:** rolling publication/flow/session and corresponding actor seams.
Implement the selected M2 policy and its explicit publication states. Update scratch
sizing to cover the largest effective unconsumed requirement, complete
segment rounding and the write envelope. A smaller readiness target must
not deadlock because a ledger grant is below the writer's gate, or evade
limits by reserving less than the producer can write.

Preserve scratch refusal handling, prepublication recovery, exact response
ownership, monotonic served endpoints, failed-producer partial responses,
object removal grace and EOF drain. The publication cadence must remain
legal for the fixed target; do not ship an arbitrary aggressive poll loop.
Rename tests whose historical names no longer describe their assertions;
`first_live_transcode_playlist_waits_for_two_segments` currently asserts a
48-second runway, not two segments.

**Acceptance:** old-tree failing/new-tree passing incident and startup
transition regressions, plus existing pacing, scratch and retirement
contracts. No first-response release before required decodable coverage or
validation; release within one 250 ms publication poll plus measured bounded
admission work once the selected threshold is actually satisfied.

### 7.3 M4: address pre-segment costs on evidence

**Owns:** the specific measured creation or producer bottleneck, only after
M0 identifies it. The incident spent nearly six seconds before segment zero;
this milestone must account for that interval even if M3 removes the later
hold.

Check duplicate source opens/probes, serialized independent preparation,
queue joins, permit waits, cold I/O, exact-source checks, FFmpeg read burst,
audio conversion and first clean GOP. Reuse already authoritative immutable
facts only with source/recipe identity and invalidation intact. Parallel work
must share cancellation and release late resources when a newer intent wins.

Do not cache away tamper checks, strip HEVC configuration, omit required
audio/subtitle preparation, increase process priority beyond current classes,
or turn a bounded source open into an unbounded task. If the remaining cost
is source/GOP-bound, publish the measured lower bound and qualify warm-indexed
performance separately. There is no mandatory speculative code patch here.

**Acceptance:** phase-level old/new evidence accounts for the first segment
interval; applicable cancellation, stale-source and process-reap tests pass.
The chosen change improves the relevant phase without changing the requested
quality, audio channels, HDR or subtitle behavior.

## 8. M5 — acceptance matrix and performance targets

These are proposed release targets, not results. M2 records feasibility and
any explicit revision before implementation; do not relax them after an
unfavorable qualification run without recording why.

| Cohort | Target and interpretation |
|---|---|
| Warm local indexed/direct-compatible library start | p50 click-to-frame ≤1 s; p95 ≤2 s on the controlled LAN; separate by actual route |
| Warm source, missing index, short clean-GOP rolling copy | p95 ≤5 s and ≥25% improvement against matched old runs; no universal cold-source promise |
| Incident-shaped resume and long/open-GOP copy | ≥25% reduction in matched median TTFF; report first-media lower bound and remaining policy hold |
| Rolling video transcode and subtitle burn | No median/p95 regression >10% or 250 ms, whichever tolerance is larger; readiness bounded by measured encode capacity |
| All passing candidates | No added startup stalls in two-minute runs; no route/quality downgrade to satisfy latency |

Run at least 20 starts per client/primary cohort for median, p95 and maximum;
report the sample size and p95 method, and do not call this a p99 estimate.
Alternate old/new trials on the same source/recipe and comparable load.
Separate warm source cache, cold source, local index, remote hydration,
missing index and unsupported index. Record outliers and failed/cancelled
starts, not just successful TTFF. A warm run after an old cold run is not an
improvement measurement. Use isolated fixture copies for repeatable cold
conditions; do not flush production caches.

Required coverage:

| Dimension | Cases that must remain correct |
|---|---|
| Client | Chrome/hls.js, Safari MSE, Safari native, physical Apple TV/AVPlayer, physical Android or Google TV/ExoPlayer; smoke iPhone |
| Media | H.264/AAC; H.264/DTS→multichannel AAC; HEVC SDR/HDR; supported Dolby Vision; open-GOP; high bitrate; text subtitles and PGS burn |
| Timeline | Start at zero, nonzero resume with preceding keyframe, near EOF, source shorter than gate, discontinuity/hole, late first complete segment |
| Consumption | 0.25×–4× endpoints, clamp boundaries, 0.5×/1×/1.5×/2×, before/after-publication rate increases/decreases; barely sufficient and insufficient producers |
| Lifecycle | Pause before manifest, play, seek storm, replacement, tab close, expired owner, prepublication retry and late cancelled result |
| Pressure | Refused scratch growth, occupied worker slots, background subtitle work, slow source, bounded I/O pause, network shaping |
| Compatibility | Current and legacy demand, unknown transport, rolling fallback disabled, preparation disabled, stale exact index |

Use a reduced pairwise matrix for secondary combinations; run every primary
client/cohort cell and every deterministic lifecycle edge. Include a
30-minute continuity run per actual transport, extended when needed to cover
its actual state transition, and a one-hour deterministic pacing trace at
1×/2× plus endpoint traces at 0.25×/4×. Fast first frame followed by a stall, live-edge jump,
frame-drop increase or abandoned resource is a failure.

## 9. Build sequence, tests and delivery evidence

### 9.1 Task ownership and order

| Task | Files/surface | Depends on | Exit evidence |
|---|---|---|---|
| M0 timing and reproduction | Creation/publication timing, existing client telemetry seams, generated fixture harness | Compiler loop | Reproducible old trace and route census |
| M1 exact preparation | VOD creation, state workers, durable jobs/stores only if demonstrated | M0 identity trace | Cause classification and focused preparation regression or no-change receipt |
| M2 policy experiment | Harness and decision receipt in this subject folder | M0 | Exact tested policy or revised design |
| M3 bootstrap publication | Rolling clock/session/flow, actor and scratch seams | M2 | Failing-old/passing-new latency and correctness tests |
| M4 pre-segment repair | Only measured creation/producer owner | M0; integrate after M3 | Phase gain or evidenced lower bound |
| M5 qualification | Regression metadata, reference docs, retained sanitized receipts | M1–M4 | Client matrix and current-tree gate evidence |

M1 and M2 are conceptually independent, but do not merge concurrent edits to
shared runtime files. Coordinate with Safari seek work: its §8 preserves
existing startup behavior while exploring steady reserve. This plan owns a
separately qualified startup change; reconcile the common policy once on the
effort branch, then rerun both latency and forward-seek continuity tests.

### 9.2 Establish and retain the pinned compiler loop

The investigating Mac has Rust 1.97.1 installed through rustup, but default
PATH selected Homebrew Rust 1.98.0. Use the verified pin explicitly:

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check -p plurxd --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all --check
rustup run 1.97.1 cargo test -p plurxd --bin plurxd rolling_publication_budget
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls_schedule
rustup run 1.97.1 cargo test -p plurxd --bin plurxd scratch_charge_startup
```

Use the [source-only compile loop](../ci/AGENT-COMPILE-LOOP.md) if the pin
cannot run on the implementation host. Archive committed source, never
credentials or `.git`; preserve a warm target. Recheck the exact integrated
candidate after the base moves. Add affected core/Apple/Android compilation
when those surfaces change. Replicated store evidence requires
`make unit-core` or `--features hiqlite-store`.

Existing anchors are in
[chunk_03.rs](../../crates/plurxd/src/transcode/tests/chunk_03.rs),
[chunk_04.rs](../../crates/plurxd/src/transcode/tests/chunk_04.rs), and
[copyseg.rs](../../crates/plurxd/src/copyseg.rs). Reverify test filters still
select nonzero tests. Add tests for incident timing, rate-scaled bootstrap,
legacy cutover, writer-gate/scratch alignment, stale attempts, delayed actor
commit and exact EOF. Name each actual regression in the task PR:

```text
Regression-Test: <actual tracked test path>::<actual test name>
```

Do not paste placeholder names into a PR. Include the same lines in the
landing commit. Use normal commits and the tracked hook; run focused tests
and affected compile checks before push. Documentation verification is:

```bash
python3 -m unittest discover -s tests/operations -p test_docs_index.py
```

Add every new plan/review/receipt document to [the index](../README.md) in
the same commit. Update [PLAYBACK.md](../PLAYBACK.md) and
[OPERATIONS.md](../OPERATIONS.md) with the chosen behavior and diagnostic
meaning in the behavior-changing task, not with an unselected proposal.

### 9.3 Promotion, rollout and rollback

Follow [the development pipeline](../DEVELOPMENT_PIPELINE.md) and
[AGENTS.md](../../AGENTS.md): task PRs into the effort, adversarial review,
focused regressions and required effort gates. Freeze task merges, integrate
current main and qualify that exact tree before main promotion. If the
candidate moves, its old receipt is not sufficient.

No fleet deployment is part of this planning deliverable. Prepare a concrete
rollout candidate and evidence first. On an authorized rollout use the
established serial fleet procedure, exact image/source stamps and quorum
checks. Verify actual old/current client compatibility; changed runtime
policy applies to newly created attempts and must not mutate active attempts.

Rollback triggers: startup failures/stalls beyond the matched baseline,
new native live-edge jumps, invalid init/playlist responses, scratch overrun,
lost cleanup, or failed quorum/serving authority. Restore the prior qualified
binary/config through the normal rollout process; preserve valid exact-index
artifacts. Do not repair latency by clearing storage or resetting the queue.

## 10. Completion ledger

| Item | State |
|---|---|
| One production slow-start trace and source attribution | Recorded; not a controlled before/after experiment |
| Detailed implementation plan | Revised; ready for M0–M2 staged investigation |
| Independent adversarial review | Complete: one P1 and two P2 findings addressed; revision independently verified |
| Selected M2 runtime policy and physical transport evidence | Not started |
| Runtime implementation / regression runs | Not started |
| Deployment / user-observed improvement | Not started |

## 11. Authorized amendment — incident evidence for M0/M1 and a measured M2 choice

**Disposition:** fold the 2026-10-02 incident into M0/M1; keep M2's measured
selection gate and M3's integrated policy ownership. This amendment does
not dispatch the historical build authorization or create another effort.
[The review](PLAYBACK-STARTUP-LATENCY-REVIEW-20261002.md) approved the causal
diagnosis and requested these changes to the proposal.

### 11.1 M0 must separate warm startup from startup immediately after restart

Use [the retained incident trace](PLAYBACK-STARTUP-LATENCY-M0-20261002.md)
as evidence, not a newly reproduced M0 result. The reviewer reports daemon
start at 04:14:31.245 UTC, listening at 31.719 and playback decision at
40.805: about 9.56 s after daemon start. Concurrent storage probing,
20 caption-graph proof encodes and slow Raft applies were reported.

Measure warm-daemon and immediately-after-restart cases separately. The
latter is a qualification scenario, not authorization to restart lab6.
Retain source, engine, recipe, lease mode and resume anchor for each replay.
Record boot work and media/storage contention. Do not infer that the burst
rate here is representative sustained capacity or that boot load alone
caused the observed publication gate and timeout.

Decompose decision → create (8.36 s in this trace), the reported 2.714 s
creation phase, producer launch, first segment, first writer playlist,
first served snapshot, readiness completion and actual first frame. Keep
click-to-frame's real origin. A two-to-four-second overall claim is not
supported while pre-creation cost remains unaccounted for.

### 11.2 M1 begins from digest churn and the foreground-promotion gap

The reviewer read a copy of lab6's replicated database. Their snapshot
reports 1,523 ready / 1,037 queued files for previous digest `953a…`, versus
310 ready / 909 queued for `85e6…`, current since 2026-09-28. File 120 had
six ready artifacts under older digests, current-digest requests with zero
attempts, and `foreground_preempted` outcomes. No current-digest request
for this file had been claimed; the play did not promote it to foreground.

Treat those numbers as the review's dated census, not a new live query.
Reproduce the census by exact source/recipe/digest and retain the sanitized
query and receipt in M1. Explain queued versus eligible versus claimed,
priority promotion, preemption reason, worker availability and hydration;
counts alone do not identify a scheduler defect. Start from this evidence
instead of treating a missing local index as an unexplained single-title
condition. Indexed VOD is preferred when available, but cannot be assumed
to cover the majority of this keyspace.

The engine digest includes executable/dependency identity deliberately.
Preserve that correctness fence here. A separate digest-scope investigation
may evaluate compatibility and cache reuse if authorized; no weakening,
requeue or key migration belongs in this incident amendment. The review's
historical failed DV conversion is context, not permission to assume the
on-the-fly route is unavoidable forever.

### 11.3 M2 retains ordinary HLS and the existing readiness sweep

Keep low-latency HLS introduction outside this effort. Keep the existing
12/16/24/32/48 media-second sweep and the 12-second writer gate. The prior
four-second proposal is withdrawn as a runtime default for this geometry:
the first complete segment alone is 7.8 s, and ordinary native discovery
must be qualified against the fixed 16-second target.

After a snapshot at `a`, the deployed server normally schedules the next
publication at `a + 16 s`. Its early arm needs Rendering and low runway,
and cannot act before `a + 8 s`. A client's changed-playlist reload at
`a + δ + 16 s` overlaps the server schedule; do not add those two waits
as independent serial delays. Worst alignment, unchanged reloads, producer
completion and transfer/append cost still belong in §4.2's inequality.

The review estimates about 20 s usable coverage at 1× after allowances,
rounding to the third segment (about 24–25 s) on this source. At the observed
3.75–3.9× burst rate, that suggests about 6.5 s server readiness after
creation rather than the observed 15.11 s. This is an extrapolation, not a
measured first-frame time or approved shipping threshold. The exact
post-position threshold includes achieved-origin lead; source durations
and transfer margin must be measured again. `EXT-X-START:TIME-OFFSET=0`
and removal of `PLAYLIST-TYPE` already handle the initial snapshot's start
selection; preserve and test them rather than inventing another offset.

**Requirement disposition:** the user said a few seconds of media should
be buffered before playback, then filling should continue. Approximately
24 s of media does not meet that strict requirement. Ordinary HLS can be
proposed as an interim improvement only with that gap stated. Choosing
LL-HLS requires a separate scope/transport design and explicit authorization;
this amendment does not silently override the existing non-goal.

At rates below 1×, record bootstrap scaling explicitly. M2 tests the
existing consumption-scaled candidate values down to 0.25×, with effective
coverage constrained by the unchanged writer gate, complete cuts and actual
native discovery. The current steady runway clamps to at least 48,000 ms;
that clamp is not automatically inherited by a new bootstrap, and its
removal is not selected by this amendment. Choose the effective minimum in
the decision receipt, including down-scaling results and 4× capacity limits.

### 11.4 M3 remains integrated; one predicate is a candidate, not a proof

The smallest likely first-release seam is `publication_cycle_at`'s
`None => end_list || end_ms >= budget.desired_end_ms` arm. Replace initial
readiness there only with M2's qualified policy, and reconcile startup
scratch sizing. At readiness ≥16 s, leave the 12-second writer gate intact;
it is not the dominating gate. Preserve complete validated init/media,
attempt fences, media sequence and the unchanged steady resource ceiling.

Do not assume that restoring the existing 48-second budget on the next
cycle proves continuity. Retain §4.3's Prepublication, AwaitingPresentation,
ActiveLowReserve and Steady contract, legal cadence and actual client
discovery. Qualify sustained production at 1.05× as well as the fast burst;
reserve growth may take 480 wall seconds and cannot be a new startup gate
or expiry. Any smaller patch must demonstrate those invariants rather than
claiming that its size eliminates the need for them.

### 11.5 Master preparation alignment remains a separate decision

After Track N, propose aligning native master and legacy-native preparation
with the child-media path: wait under the existing absolute playlist wait
budget (55 s), then use the unchanged 5 s final response-admission budget,
clipped to the original outer request deadline. Replacement may reclassify
within that same deadline; it cannot renew the clock or use predecessor
init to authorize a successor.

This changes where waiting occurs even though the final five-second
publication budget stays fixed. Reconcile it explicitly with the native
contract's server-deadline non-goal before implementation. Track N's
application fetch preflight owns its shorter remaining attachment deadline
and aborts when exhausted; a server's 55 s cap is not a promise the browser
will wait that long. Native-element tolerance of a held request is unmeasured.

Retain the existing served-snapshot init fence. Withdraw the separately
fenced prepared-init reader: there is no evidence requiring another
publication authority. Keep byte bounds, response ownership, capability
checks and internal inspection/delivery accounting separate.

### 11.6 Integration order and acceptance receipts

1. Track N under its existing contract, plus the proposed compatible-route
   preflight; prove real Safari delayed readiness before claiming repair.
2. M0 warm and after-restart traces, plus M1's exact keyspace/foreground
   diagnosis; patch only an evidenced defect through existing job owners.
3. M2's original sweep and decision receipt, then M3's qualified readiness,
   scratch and startup-to-steady integration. Any interim improvement must
   retain the unmet strict-buffer requirement in its acceptance report.
4. Master/child preparation alignment only after its scope decision.

Run new/affected or failed regressions with nonzero selected test counts;
retain applicable passing evidence and do not repeat unrelated suites or
coverage. Exact candidate compile/lint/policy and affected client checks
still apply. Observe first frame and at least 30 minutes of continued
playback at resume 3273.778 s and zero, including worst-aligned reloads,
rate changes and slow reserve growth. Record produced, advertised, buffered
and presented frontiers separately. Required device qualification from
§7/§8 remains; a lab6 Safari improvement alone does not complete M5.
