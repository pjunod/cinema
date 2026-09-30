# Safari seeking — Sol build handoff

**Status:** ready for implementation; no implementation claimed ·
**Written:** 2026-09-29 · **Investigated base:** `1869871ce` (PR #613) ·
**Owner:** Sol · **Suggested effort:** `effort/safari-seek`

Read [AGENTS.md](../../AGENTS.md), the dated amendments at the top of
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md), and this document before
editing. Recheck the named symbols against current `main`; line numbers and
older plans are not authoritative. Work through M0–M5 in order, keeping the
execution record in §11 current. Establish the pinned Rust compiler loop
before writing Rust. This handoff is self-contained and does not depend on
the original pasted proposal or a local investigation directory.

The build restores useful access to the VOD system we already have. It also
measures and, where the existing resource bounds permit, improves the rolling
fallback's seek window. It does not build another VOD engine. If a step seems
to require weakening source identity, publication fencing, or live-playback
priority, record the specific conflict and continue independent work instead
of bypassing the contract.

## 1. Outcome — fast paths are reachable and failures are truthful

Deliver these observable behaviors:

1. A seek outside the attached element's actual seekable ranges cannot be
   reported as successful merely because `seeked` fired. Preserve PR #613.
2. Playing a copy-compatible title with missing preparation creates or joins
   urgent interest in that exact source and recipe. This urgency survives
   analysis, artifact building, and delivery to the requesting node.
3. Old-engine requests and ordinary library backfill cannot keep eligible
   viewer preparation at the back of the queue. Live playback retains first
   call on physical encoder and CPU capacity.
4. Two ordinary maintenance readers cannot continually occupy both shared
   source-I/O slots and prevent new viewer preparation from being admitted.
5. Rebuilding the same release for the same architecture uses the same media
   runtime inputs. A deliberate engine change is visible as an identity
   change, with a bounded preparation plan.
6. A well-supplied rolling session gains useful forward seek coverage only
   if the measured native-client behavior, publication cadence, retention and
   scratch bounds support it. Startup does not wait for a larger steady-state
   seek reserve.

Queue admission, completed indexing, VOD availability, and first presented
frame are separate milestones. A 46.8 GB source still takes time to index.
Do not promise subsecond cold-title seeking because a priority field changed.

### 1.1 Non-goals

- No new playback engine, durable queue, scheduler service or index format.
  The existing implementations already provide these capabilities.
- No engine-key weakening, cross-engine artifact reuse, forced transcoding,
  HDR downgrade or approximate fragment plan. Byte compatibility is unproved.
- No fake `ENDLIST`, understated target duration, blanket `EVENT` change or
  `currentTime` retry loop to defeat native HLS behavior.
- No automatic mid-session switch to VOD. Resolve VOD through the existing
  create/reopen path when a new request occurs; preserve the incumbent until
  its replacement is usable.
- No global reindex, queue deletion, cache flush, production package install,
  restart or deployment as part of building this handoff. Production seek
  records were authorized for reading during investigation; that is not
  deployment authorization.
- No new product feature gate or fleet certification flag. Correctness and
  admission checks are operation-level constraints, not hidden enablement.

## 2. Evidence — what was measured and what remains uncertain

All production observations below are snapshots from 2026-09-29, in UTC.
They describe that investigation, not the fleet at implementation time.

### 2.1 Native Safari clamp was reproduced

A local synthetic 120-second H.264/fMP4 presentation used six-second media
segments. Safari reported all 120 seconds buffered. Assigning `currentTime =
100` produced these results with the same media:

| Served playlist | Target duration | Seekable range | Position at `seeked` |
|---|---:|---:|---:|
| Unfinished, no playlist type | 16 s | 0–72 s | 72 s |
| Unfinished, `EVENT` | 16 s | 0–72 s | 72 s |
| Finished, `VOD` + `ENDLIST` | 16 s | 0–120 s | 100 s |
| Unfinished, no playlist type | 6 s | 0–102 s | 100 s |

The browser identified itself as Safari 27.0.1 / AppleWebKit 605.1.15 on
macOS. This establishes the native mechanism on that browser: buffered bytes
outside `seekable` did not make a local seek possible. It does not establish
the exact behavior of every Safari release, a growing server playlist, or
the original film. Preserve a repeatable browser fixture in M0.

The observed 48-second exclusion is three 16-second target durations.
[RFC 8216 §6.3.3](https://www.rfc-editor.org/rfc/rfc8216#section-6.3.3)
contains a live-start recommendation; it is not itself proof of Safari's
seek algorithm. The browser experiment is the evidence for that algorithm.
Do not generalize that hls.js is unaffected: test its actual configuration
and the element's ranges too.

### 2.2 Server publication makes the reproduced mechanism relevant

The rolling publication target is 16 seconds and its initial runway is
48 seconds. Explicit publication computes approximately:

```text
session_consumed = max(0, estimated_film_position - media_origin)
desired_end      = session_consumed + rate_scaled_initial_runway
allowed_end      = desired_end + maximum_segment_duration

observed native seekable end ≈ published_end - 48 seconds
```

The current policy publishes a selected completed prefix. It does not expose
every segment FFmpeg has produced. `-readrate` alone therefore cannot explain
or fix the published edge. The internal writer can use `EVENT`, while the
served rolling playlist strips the playlist-type tag.

At 17:38:33, the retained file-9 seek record showed position 18.750 seconds,
published end 66.750 seconds, produced end 83.125 seconds, and target duration
16 seconds. `66.750 - 48 = 18.750`: the observed publication shape is exactly
consistent with a seekable end at the playhead. Another record at 17:38:43
showed position 25.140, published end 75, and produced end 83.125 seconds.

The original incident did not capture browser ranges, and the original
file-9 `vod_index_pending` log was no longer retained. Treat the incident
correlation as strong supporting evidence, not a complete event replay.
Later reopened attempts have nonzero media origins: never subtract a
session-relative endpoint from a film position without converting them.

### 2.3 Preparation is blocked at more than one stage

| Observation | Consequence for the build |
|---|---|
| File 9's new-engine analysis request remained queued with zero attempts since 17:38 | Fix analysis admission, not just artifact priority |
| The missing-artifact path already enqueues a foreground artifact job after source attestation exists | Do not claim foreground artifact priority is new |
| The play-time analysis helper uses `normal` / `background` | Add supported viewer urgency at this earlier stage |
| Analysis validators allow `foreground` and `playback` only for `subtitle_source`; `foreground` is not an analysis trigger | The original proposal's string substitution is invalid |
| At 21:17, nynuc had 157 queued normal new-engine requests and 164 queued forced old-engine requests | Filter worker compatibility before spending an execution attempt |
| A stale forced request failed `pipeline_version_unavailable` at 21:15:38 | Old work was still consuming analysis dispatch opportunities |
| At approximately 21:12, fragment jobs had 561 claim writes, 561 refusals and zero accepted claims since restart | Distinguish local capacity, shared-resource refusal and successful execution |
| At 21:14 and 21:17, both shared `source_io` reservations belonged to subtitle extraction, started at 20:54:33 and 21:00:09 | Queue ordering cannot free a slot held by a running reader |

The two holder snapshots establish the resource blocker at those times.
They do not prove those same jobs caused all 561 refusals, or that the readers
were hung rather than making slow progress. Subtitle extraction has a long
timeout; renewed ownership is not evidence of useful work.

The statement “no indexes built after 16:43” was not correct when checked
against the artifact ledger: files 779 and 777 completed around 19:43 under
the relevant pipeline. Analysis-request status alone is an incomplete
completion ledger. A cluster-wide healthy badge or 24-hour ready count does
not answer whether this viewer can obtain this recipe on this node.

### 2.4 Runtime drift is real; pinning one package is insufficient

The sampled nodes reported the same application release
`v0.3.0-5216-g1869871ce` but different engines:

| Node | Jellyfin FFmpeg package | Engine digest prefix |
|---|---|---|
| nynuc / media1 | `8.1.3-1-bookworm` | `85e6fa5e` |
| nuc4 / lab4 | `8.1.2-5` | `953a7b12` |
| m6 / lab6 | `8.1.2-5` | `953a7b12` |

The engine digest includes version output, executable bytes and loaded
dependency objects. Equal top-level package versions do not prove equal
digests. Architecture differences can legitimately produce distinct engines.
Rebuilding without cache is not a convergence strategy. Keep the conservative
identity and make the shipped runtime reproducible instead.

### 2.5 Existing mitigation is not a measured latency win

PR #613 checks `seekable` and falls back when a local seek lands away from its
target. Its 23 seek-control tests passed during investigation. The available
production sample did not establish post-deployment seek latency.

Four retained remux reopen measurements were 8.632–12.068 seconds. Three
transcode measurements were 514–648 ms, but used a different title and Intel
Quick Sync. They are not an A/B comparison and cannot justify transcoding
this title or claiming a one-second fix.

## 3. Existing owners — re-verify these interfaces before editing

| Area | Source and symbols |
|---|---|
| Browser routing and landing | [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js), `seekRoute`; [transport.js](../../crates/plurxd/src/web/player/transport.js), `seekTo`, `playbackSeekSeekableRangesMs`, seek telemetry |
| VOD creation and exact repair | [create.rs](../../crates/plurxd/src/vod/serve/create.rs); [construct.rs](../../crates/plurxd/src/vod/serve/construct.rs), `try_cluster_fragment_index` |
| Analysis enqueue and dispatch | [state.rs](../../crates/plurxd/src/state.rs), `enqueue_copy_preparation`, `enqueue_copy_preparation_for_object`, `resolve_analysis_requests`, `work_cluster_fragment_index_queue`, `background_work_loop` |
| Analysis store contract | [fragment_index_cluster.rs](../../crates/plurx-core/src/store/fragment_index_cluster.rs); [Hiqlite implementation](../../crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs) and [SQLite implementation](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs), `valid_request`, `claim_analysis_request` |
| Durable admission and I/O | [background_jobs.rs](../../crates/plurx-core/src/store/background_jobs.rs), `CLAIM_SQL`; [schema](../../crates/plurx-core/src/store/background_jobs_schema.sql), required resources and reservations; [fragment adapter](../../crates/plurx-core/src/store/background_jobs_fragment_admission.rs) |
| Worker ownership and delivery | [daemon background jobs](../../crates/plurxd/src/background_jobs.rs); [local admission](../../crates/plurxd/src/transcode/manager/construct.rs), `admit_fragment`, `pretranscode_worker_idle`, `fragment_worker_may_start` |
| Engine and recipe identity | [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs), `fragment_index_engine_digest`; [cluster artifact code](../../crates/plurxd/src/fragment_index_cluster.rs), `pipeline_digest`; [Dockerfile](../../Dockerfile), `runtime-assets` |
| Rolling publication and startup | [publication.rs](../../crates/plurxd/src/transcode/rolling/publication.rs), `RollingPublicationClock`, `rolling_explicit_publication_ends`; [flow.rs](../../crates/plurxd/src/transcode/rolling/flow.rs), `rolling_startup_bytes`; [session.rs](../../crates/plurxd/src/transcode/rolling/session.rs), `publish`; [segment_index.rs](../../crates/plurxd/src/transcode/rolling/segment_index.rs), served playlist rendering |

Two existing signatures illustrate why the repair crosses layers:

```rust
pub(crate) async fn enqueue_copy_preparation_for_object(
    store: &dyn Store,
    node_id: &str,
    file: &MediaFile,
    video: plurx_core::transcode::CopyVideoOptions,
    object_version: Option<&str>,
) -> Result<AnalysisRequest, StoreError>;

pub(super) fn rolling_explicit_publication_ends(
    demand: &crate::playback_control::PlaybackDemandSnapshot,
    observation_age: Duration,
    media_origin_ms: i64,
) -> RollingExplicitPublicationEnds;
```

These are current signatures, not prescribed new APIs. Extend the first
through a typed demand context rather than more unrelated string arguments.
Keep an ordinary maintenance call path. The second already receives accepted
client demand and media origin; preserve that authority and coordinate space.

Read the related [durable-work plan](../cluster/DURABLE-WORK-QUEUE-IMPLEMENTATION.md),
[content-analysis repair](CONTENT-ANALYSIS-FAILURES-IMPLEMENTATION.md),
[shared-index handoff](STREAMING-SHARED-INDEX-HANDOFF.md),
[seek-scratch implementation](SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md), and
[Apple TV publication record](APPLE-TV-FORWARD-SKIPS-STATUS.md). Reuse their
landed contracts. Do not import a historical, task-specific workflow exception
into this effort or overwrite concurrent work in these files.

## 4. M0 — preserve the browser reproduction and expose each wait

**Build:** extend the existing playback test/lab surfaces rather than adding
a second harness. Keep four playlist variants from §2.1 over identical media.
Add a growing/sliding variant using the real server publication path, and
record the served manifest rather than the writer's private manifest.

A reproducible synthetic media generator is:

```bash
mkdir -p /tmp/plurx-safari-seek/hls
ffmpeg -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=320x180:rate=24 -t 120 -an \
  -c:v libx264 -preset ultrafast -g 144 -keyint_min 144 -sc_threshold 0 \
  -f hls -hls_time 6 -hls_list_size 0 -hls_segment_type fmp4 \
  -hls_playlist_type vod \
  -hls_segment_filename /tmp/plurx-safari-seek/hls/seg%03d.m4s \
  /tmp/plurx-safari-seek/hls/source.m3u8
```

Record the generator's FFmpeg identity. Derive variants by changing only the
target duration and playlist-type/end tags; retain valid URIs and durations.
Use a user-started playback action and wait for the required buffered coverage
before assigning 100 seconds. Capture ranges before assignment, at `seeked`,
and one second later. A JavaScript mock proves routing, not native clamping.

Extend existing seek telemetry, without adding a heartbeat log stream:

- At route choice: seek ID, attempt/session ID, target, element position,
  media origin, native HLS/hls.js/direct path, VOD/rolling mode, bounded
  buffered and seekable ranges, route and reason.
- At settlement: landing, first presented frame or supported audio progress,
  elapsed monotonic time, success/fallback/abandoned outcome. A stale callback
  from a superseded attempt cannot finish the current seek.
- Correlate server produced and published endpoints, origin, playlist target,
  accepted demand sequence and age when available. Unknown is null, not zero.
- Expose preparation stages using existing diagnostics: awaiting attestation,
  analysis eligibility, local capacity, shared I/O, building, artifact ready,
  hydration queued/running, local availability. Include bounded reason codes,
  timestamps and request/job linkage; do not emit media paths or credentials.

Report local-admission refusals separately from Store claim refusals. Include
shared-resource holders' kind, priority, age and last useful progress where
available. A worker that never reached the claim API must remain visible.
Measure whether analysis dispatch is delayed by the scheduler cadence or
`cluster_index_working`; the independent artifact consumer does not establish
that the analysis stage is getting service.

**Acceptance:** the native four-variant result can be rerun, or the precise
missing physical evidence is recorded. The real-server trace identifies each
wait stage without inferring it from a generic “queued” state. Existing
[seek-control](../../tests/playback/seek-control.test.js) and
[seek-telemetry](../../tests/web/seek-telemetry.test.js) tests pass. Keep the
pre-fix trace as evidence; do not encode the bug as the desired regression.

## 5. M1 — carry viewer urgency through preparation

### 5.1 Request semantics and lifetime

Use `priority = "foreground"`, `trigger = "playback"`, and
`force_rebuild = false` for viewer-originated **analysis** requests. These are
newly supported fragment-analysis semantics; they are not accepted on the
investigated base. Update both Store validators, SQL constraints/triggers,
migrations and contract tests together. The artifact queue's separate
`foreground` trigger vocabulary remains separate.

Urgency belongs to a live consumer interest, not to the artifact forever.
Reuse the durable waiter mechanism and carry a stable, server-owned consumer
identity through the stages. Use a renewable 120-second demand deadline,
refreshed at most once per 30 seconds while the owning playback session is
active; closing/cancelling it removes its urgency promptly. These numbers are
new design defaults, not measurements. Reuse existing equivalent lease
machinery where it supplies these semantics; do not persist a second session
registry. Internal retries must not manufacture new consumers.

Stopping a viewer removes only that viewer's interest. Other viewers and
background interest survive. Expiry drops the effective priority; it does not
delete an otherwise useful artifact or reset failure history. Document how
an initial create with no attached session transfers ownership to the session,
or expires if creation fails.

Join by the existing exact source generation, object version where present,
engine and video recipe. Joining lower-priority queued work promotes its
effective priority atomically. Joining running work preserves its owner,
fence and renewal token. A new priority cannot reset attempts, retry deadlines,
forced-generation semantics or terminal failures. A new source or recipe
continues to require its own identity.

### 5.2 Eligibility, dispatch and delivery

Pass the executing worker's supported pipeline identity into candidate
selection and enforce it again in the claim transaction. A worker must not
claim old-engine work merely to report `pipeline_version_unavailable` later.
Leave it available to a compatible worker or report it as waiting for one;
do not rewrite its digest or cancel the fleet's historical requests.

Select due, compatible viewer interests ahead of forced administrative and
normal work. Preserve existing retry/backoff eligibility. Apply this to
analysis, exact artifact construction and target-node hydration. Urgency on
an artifact that cannot reach the requesting node is not a completed repair.

Wake the existing consumer on newly admitted viewer demand and give analysis
a bounded dispatch opportunity. Do not hold the shared queue guard across a
long source read or artifact build. Reuse the existing single-consumer/fence
mechanisms rather than allowing two resolvers to race. In an idle test with
compatible source and free resources, the first analysis admission should
occur within two seconds of enqueue; this is a scheduling target, not an
index-build deadline.

Keep local physical admission conservative. `admit_fragment` currently
requires idle capacity and reserves the software budget at background
priority. Viewer preparation does not become a live encode by changing its
queue label. If playback owns that node, report the capacity wait and use an
already-supported eligible worker only where source attestation and delivery
contracts permit it. Do not add unchecked cross-node source assumptions.

**Acceptance:** both Store backends prove dedupe/promotion, demand expiry,
two viewers sharing one build, renewal after promotion, exact recipe/object
isolation, terminal retry limits and old-engine exclusion before claim.
An idle fixture with old forced requests ahead of the viewer starts current
work within the scheduling target. A ready remote artifact hydrates to the
consumer without rebuilding. A live waiter wins local physical admission.

## 6. M2 — keep a shared I/O opportunity for demand work

### 6.1 Admission policy

Retain the global limit of **two** `source_io` reservations. Implement a
conservative first version: at most **one** reservation may be held by work
with no live playback demand. Either slot can serve demand work. Ordinary
maintenance cannot borrow the second slot in this version.

This intentionally trades peak maintenance concurrency for predictable
admission of a newly arriving viewer. It avoids adding distributed preemption
or interrupting valid readers. Measure the maintenance throughput impact in
M5 and report it. Borrowing with cooperative cancellation can be a later,
separately justified optimization; it is not needed to complete this repair.

Classify demand from unexpired authoritative consumer interests, including
existing playback subtitle interests, rather than trusting a caller-supplied
numeric priority. An operator's forced backfill is not a playing viewer.
Preserve FIFO/priority fairness among eligible demand jobs. If both slots
already serve demand, a third viewer waits truthfully; the limit still holds.

Enforce both limits atomically in the replicated claim/reservation transaction
and in the SQLite backend. Candidate filtering alone is insufficient: two
nodes can see the same free capacity. Cover every adapter claiming
`source_io`, including hydration where it declares that resource. Audit source
attestation reads too; do not leave an unaccounted full-source read on the
other side of the new admission policy.

### 6.2 Upgrade, expiry and progress

Do not revoke existing reservations during migration. Existing two-reader
occupancy drains under the old owners; new background claims wait until the
new limit permits them. If demand expires on a running reader, account for
its resulting maintenance occupancy without stealing its lease. Temporary
grandfathered occupancy cannot authorize another claim.

This policy does not instantly remove the two readers observed in §2.3.
Expose that transition and its wait. Inspect useful progress and the current
timeout/cancellation path before labelling either long reader stalled.
Use existing bounded timeout and joined-worker cleanup for a stuck reader.
Never release a reservation while its worker can still read or publish.
Renewal, cancellation, restart and takeover must preserve the current fence.

**Acceptance:** in a multi-node Store test, many normal readers acquire only
one shared slot; a subsequent live-demand reader acquires the other; a third
reader cannot exceed two. Concurrent claims, expired demands, active-owner
renewal, worker death, migration from two occupied slots, and stale-owner
publication are tested on both backends. The real worker path demonstrates
this admission behavior; an isolated SQL count is insufficient.

## 7. M3 — make runtime identity a reproducible release input

Build and publish a reviewed, immutable media-runtime image per supported
architecture, then consume its recorded digest from the application build.
Use the repository's existing image/release tooling; preserve its recognized
binary-copy and runtime-stage conventions. Pin the media runtime's package
inputs and repository snapshots or verified package artifacts so that its
next intentional rebuild is reviewable too. Do not rely on a Docker cache.

Include FFmpeg/FFprobe and their loaded libraries, the supported media-driver
stack and output-affecting tools. Keep the existing AC-4, `dovi_rpu`,
`tonemapx`, mkvtoolnix and dovi_tool assertions. Choose an available, supported
package set during implementation; this document does not choose a fleet
downgrade to 8.1.2 or declare 8.1.3 qualified.

Emit a compact release record: architecture, runtime image digest, package
manifest, application SHA and the actual engine digest calculated by the
daemon's identity code. Add a read-only diagnostic command or extend an
existing diagnostic surface if needed; do not invent a parallel hash that
only resembles the index key. Verify observed identity during the ordinary
rollout. A mismatch is reported, not hidden by accepting incompatible bytes.

For an intentional engine bump, preserve old artifacts and queued identities.
Describe which workers can serve/build each keyspace, which reference titles
will be prepared first, and how current-engine demand bypasses old backlog.
No automatic whole-library rebuild. Per-architecture engine differences are
allowed; reuse requires the exact compatible identity, not a shared app tag.

**Acceptance:** two clean application-image builds using the same runtime
input on the same architecture report the same engine digest. Supported
architectures retain media capability checks. An intentional runtime change
that changes output identity causes an explicit key change; the old key is
not silently accepted. The release workflow carries and checks the actual
runtime record without adding a product gate.

## 8. M4 — improve rolling coverage within its existing safety bounds

This milestone follows measurement. Preparation improvements do not make a
cold title instant, so measure whether useful local seeks can be retained
while indexing runs. A larger reserve is a candidate optimization, not a
pre-established root-cause fix.

### 8.1 Model startup and steady state separately

Do not increase `ROLLING_INITIAL_RUNWAY_MS` globally. It participates in
startup readiness and scratch budgeting as well as publication. Preserve
startup's existing readiness behavior and introduce a distinct steady-state
publication target only if the following proof succeeds:

```text
needed steady lead >= measured native holdback
                    + desired forward-seek distance
                    + consumption during publication/reload delay
                    + bounded observation and rounding allowance

needed lead + protected history + guards <= served-window/resource bounds
published end <= completed produced end
```

Use media seconds consistently; playback rate affects consumption during a
wall-clock delay. Do not treat 48 seconds as universal across target durations
or native clients. Derive the allowance from the actual publication deadline,
client reload evidence and accepted-demand freshness rules.

Evaluate +10-second and +30-second seeks at 1× and 2×. First target useful
+10-second coverage across a full update cycle in the well-supplied 1× case;
report +30-second and 2× limits explicitly. A snapshot immediately after a
playlist refresh is insufficient evidence.

If the bound fits, acquire any additional scratch through the existing ledger
before producing bytes, and accumulate the steady reserve without delaying
the initial playlist. Grow it through the cumulative publication clock; do
not release an arbitrary completed tail or jump the live edge at cutover.
Preserve accepted-demand/attempt fences, monotonic endpoints, paused/stale
demand behavior, protected back-buffer segments, object grace and EOF drain.

The candidate must pass the existing Apple TV forward-discontinuity
regressions and a physical native continuity run before being called verified.
Retain the actual element-range gate even when the server expects ample lead.

### 8.2 A valid negative result is explicit

If the required lead cannot fit the current retention/scratch/cadence bounds,
record the measured inequality and finish M4 without widening those limits.
Ship M0–M3 with truthful reopen behavior and the demonstrated preparation
improvement. Do not claim rolling forward-seek performance is solved. A
larger retention or scheduling redesign needs its own concrete proposal;
it must not quietly enter this build under a constant change.

**Acceptance:** deterministic-clock tests cover startup/steady transition,
variable segment durations, fast and slow writers, pause/resume, stale demand,
nonzero origin, rate changes, EOF and scratch refusal. Real native range
traces cover the full publication cycle. Existing `rolling_publication_budget`
and `mkv_hls_schedule` regressions remain green. The execution record contains
either a verified bounded improvement or the exact reason it could not fit.

## 9. M5 — demonstrate the whole path and report its limits

Use the same source, selected recipe, client, engine and cache conditions for
before/after comparisons. Separate cold attestation, missing artifact, remote
artifact hydration and locally ready VOD. Do not compare one remux title to a
different hardware-transcoded title.

| Case | Required observable result |
|---|---|
| Native rolling target buffered but outside `seekable` | Reopen directly; no false local success |
| Native or hls.js local target inside valid coverage | Lands at the requested target, no unnecessary replacement |
| Uncached immutable VOD target | Remains on its existing VOD fetch/seek path with the existing deadline |
| New viewer, no source attestation | Exact request is urgent, compatible analysis is dispatched, stage waits are visible |
| Attested source, missing exact artifact | Existing repair job is joined/promoted and built once |
| Artifact ready on peer only | Verified targeted hydration, no duplicate source scan |
| Old forced requests plus new-engine viewer | Old rows do not consume the viewer's dispatch attempts |
| Two simultaneous viewers of one recipe | One computation, independently expiring interests |
| Viewer leaves during preparation | Urgency expires; retained maintenance work and other viewers survive |
| One maintenance reader plus a viewer request | Viewer can claim the second shared slot when local capacity permits |
| Live encode starts while preparation waits/runs | Existing live resource priority and safe cancellation remain intact |
| Seek from a reopened nonzero origin | Target, buffer, seekable and telemetry use correct film coordinates |
| Repeated seeks / refused replacement | Latest intent wins once; incumbent and scratch reservations remain correct |
| Produced tail far beyond published edge | Publication remains bounded; no native forced forward jump |

Record click-to-first-presented-frame, chosen route, landing error, producer
restarts, initial first-frame time, analysis queue delay, source-read duration,
artifact build duration, hydration duration, and peak scratch/I/O occupancy.
Use explicit unavailable fields where instrumentation cannot measure a stage.
Include a ten-minute native continuity run for any publication change.

For controlled buffered seeks, use a proposed acceptance target of p95 under
one second with no producer restart and no wrong-target settlement; run at
least 20 seeks per compared case and publish sample count and failures.
This is a lab target, not a claimed current production result. For reopened
seeks, report measured latency and its dominant stage rather than inventing
a universal bound. Preserve existing landing tolerances unless a separately
tested change is justified; the five-second fallback guard is not a quality
target for accurate seeks.

**How to read the result:** lower queue delay with unchanged scan duration
means scheduling improved. A ready artifact with long hydration still leaves
the viewer waiting. More local seeks accompanied by longer startup or forward
discontinuities fail the rolling optimization. A free global I/O slot with
local CPU admission blocked is not a queue fault. Good fixture numbers do
not establish NAS cold-cache or physical-device performance.

**Acceptance:** fill every applicable row with evidence or an explicit
unavailable result. Do not claim missing device evidence passed. Production
playback tests, backfills and deployment remain a separate operational step;
complete fixture and read-only work before requesting that step.

## 10. Delivery — serial ownership and exact-candidate proof

Use one effort branch because the packages overlap. Each task branch uses
the `codex/` prefix, starts from the current effort head and targets that
effort. Do not build these as independent concurrent changes to `main`.
Sol can execute the tasks serially; this document does not require extra
agents or separate chats.

| Package | Owned surfaces | Depends on |
|---|---|---|
| M0 evidence and diagnostics | Existing playback harness, web transport/measurements, diagnostic projections | Current main baseline |
| M1 demand preparation | VOD creation, state dispatch, analysis stores, waiter/fragment adapters | M0 |
| M2 I/O admission | Shared resource SQL, both stores, worker cleanup, adapter tests | M1 demand lifetime |
| M3 deterministic runtime | Dockerfile, existing image/release tooling and diagnostics | M0 identity baseline; integrate after M2 |
| M4 bounded rolling optimization | Publication/flow/session/retention, scratch integration, native tests | M0 evidence; integrate after M3 |
| M5 integration and promotion | Combined fixtures, evidence and this execution record | M1–M4 dispositions |

Read [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) before web edits.
Prefer existing scripts; if adding one, update the asset registry, shell tag
and layout table together. These scripts share a global scope, without ES
module imports or exports.

Use [the compile-loop instructions](../ci/AGENT-COMPILE-LOOP.md). Example
commands below are existing entry points, not a substitute for identifying
the smallest actual regression for each package:

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check -p plurxd --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
rustup run 1.97.1 cargo fmt --all -- --check
node --test tests/playback/seek-control.test.js tests/web/seek-telemetry.test.js
rustup run 1.97.1 cargo test -p plurxd --bin plurxd rolling_publication_budget
rustup run 1.97.1 cargo test -p plurxd --bin plurxd mkv_hls_schedule
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

Core Store tests must enable `hiqlite-store` (or use `make unit-core`). Select
the appropriate library or integration-test target and confirm the filter
selects tests; zero tests is not a pass. Check/lint affected core targets as
well as the daemon. Add regressions for the behavioral contracts above, not
tests that only assert a new constant or SQL spelling.

If using a cloud compiler, transfer `git archive` source only, keep the
compiler cache warm and transfer no repository credentials or `.git`.
Compile/lint and run focused evidence before pushing. After integrating a
new base, validate that exact tree again. Follow normal hooks and current
validation catalogs; never use CI as the compiler.

Observable behavior commits use `fix(` or `perf(`. Put exact
`Regression-Test: <path>::<test name>` lines in PR descriptions and preserve
them in landing messages, including `MergeMessageField` for API merges.
Do not copy a declaration for a test that does not exist on the candidate.

Follow the current repository effort and promotion requirements. Freeze task
integration, merge current `main` into the effort and open the final PR as
draft. Obtain the one adversarial review required by the current development
pipeline, address it, and run the applicable ready fast lane/manual evidence
requirements on the final candidate. Record the required promotion evidence;
do not rely on older prose claiming automatic full CI or deployment. A moved
base invalidates evidence tied to the previous tree. Merging and deploying
are separate actions.

## 11. Execution record — update in the implementation commits

Keep this section as the single build ledger. Add supporting evidence under
the existing evidence area and link it here. Update [the docs index](../README.md)
if adding or moving another document.

| Item | Result |
|---|---|
| Implementation base / effort / task branches | Not started |
| Compiler version and baseline check | Not run for this documentation-only handoff |
| M0 browser fixture and stage diagnostics | Not started; historical observations in §2 |
| M1 demand lifetime, promotion and engine eligibility | Not started |
| M2 shared I/O policy and throughput trade-off | Not started |
| M3 selected runtime inputs and per-architecture identities | Not started |
| M4 lead calculation, implementation or bounded negative result | Not started |
| M5 same-source measurements and unavailable evidence | Not started |
| Focused commands, selected counts, exit status and candidate SHA | Not started |
| Review findings and resolution | Not started |
| PRs, final promotion evidence and merge | Not started |
| Production authorization and deployment | Not requested by this handoff |

Return the implementation commit/PR links, changed behavior, exact test
evidence, measured latency breakdown, remaining limitations and operational
steps still needed. Do not report the whole effort as “seek fixed” if only
queue admission improved or the physical rolling result remains unverified.
