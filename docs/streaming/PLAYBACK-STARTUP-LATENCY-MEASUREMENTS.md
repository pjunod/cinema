# Playback startup measurements — separate readiness from continuity

**Status:** conservative implementation selected; final review addressed ·
**Written:** 2026-09-29 EDT · **Source:** diagnostics `b3ca3a0de`; current main `4cb902884` integrated; serving-settings correction in draft PR #627.

Companion to [the build contract](PLAYBACK-STARTUP-LATENCY-BUILD.md) — this
is retained evidence and its limits, not a proposed shipping threshold.

## Monotonic diagnostics — each elapsed value has one origin

The implementation adds bounded phase names to existing structured logs.
Session identity is hashed by the existing log helper. File/attempt identities
are log fields, never metric labels. The phases do not alter publication,
source validation, actor admission, scratch ownership or startup deadlines.

| Phase | Origin and endpoint | Interpretation |
|---|---|---|
| `vod_create` | Start/end of the existing immutable-VOD create attempt | Includes exact recipe/index lookup and preparation admission; success or refusal |
| `create` | After request claiming to completed route creation | Excludes decision and the earlier claim wait; includes VOD refusal plus rolling creation when used |
| `copy_writer_first_complete_segment` | Copy writer construction to first successful segment rename | A completed object, not permission to serve it |
| `copy_writer_gate_open` | Same writer origin to successful first non-final playlist write | The writer's 12-second gate; separate from server publication |
| `writer_inventory_to_first_snapshot` | First actual observation of a gated writer playlist to actor-admitted snapshot availability | Includes publication wait and admission work; the worker poll may delay the initial observation |
| `copy_init_validation` | Validation entry to result | Separates time spent validating from the timestamp of its success log |
| `first_response_admission` | Response authorization entry to the first admitted producer-media response | Exact owner and actor fences remain authoritative |

Failures or cancellation before a completion event leave that phase absent;
absence is not a zero-duration success. Browser click-to-frame remains the
browser's monotonic duration. Do not subtract it from server wall timestamps.
The short-source ENDLIST path retains its existing completion contract;
`copy_writer_gate_open` currently records non-final gate opening only.

## Incident replay — the sixth endpoint crosses the existing gate

`scripts/playback-startup-trace` replays the sanitized six durations and
7.965-second achieved-origin lead. It performs design arithmetic; it is not
a daemon or browser execution. The authored daemon reproduction is
`rolling_publication_budget_incident_resume_waits_for_sixth_endpoint` in
[chunk_03.rs](../../crates/plurxd/src/transcode/tests/chunk_03.rs).
It is **authored, not run**, per the build's deferred regression sequence.

| Candidate usable runway at 1× | Required endpoint | First incident endpoint crossing it |
|---|---|---|
| 12 s | 19.965 s | Segment 2, 23.231 s |
| 16 s | 23.965 s | Segment 3, 30.989 s |
| 24 s | 31.965 s | Segment 4, 39.664 s |
| 32 s | 39.965 s | Segment 5, 50.967 s |
| 48 s | 55.965 s | Segment 6, 58.558 s |

The recorded first/sixth completion timestamps differ by 5.990 seconds.
Intermediate completion timestamps are not supplied or interpolated.
The capacity prefilter covers the clamp edges and 0.25×–4× rates with
0.95×/1.05×/1.2×/2× production relative to consumption. Its transfer/append
and scheduling allowance is an explicit 2.5-second experiment assumption,
not a measured production bound. No candidate is qualified by that output.

## Exact preparation — contention and a different pipeline are observed

Read-only observations on `nynuc` at Unix milliseconds `1790735954244`,
`1790736051977`, `1790736082565` and `1790736212763` show:

- File 6456 has no local fragment index or terminal preparation outcome.
- Its exact fragment job remains queued at priority 3, with zero failed
  attempts and an elapsed age rising from 2303 to approximately 2561 seconds.
- Its requested pipeline digest begins `9e7e13ea`; the available peer
  artifact's pipeline begins `6386de24`. Their attested source digest is the
  same, but the artifact cannot satisfy a different exact pipeline.
- Both live `source_io` reservations are occupied by priority-0 subtitle
  extraction jobs. At the final observation their job ages exceed 258,000
  seconds; job creation age does not prove uninterrupted claim age.
- Discovery cursors and timestamps advance on three nodes during the window.
  A progressing discovery cursor is not proof that the exact build can claim
  the occupied shared resources.

This classifies the observed absence as queued foreground work under shared
source-I/O contention, with an incompatible remote artifact. It does not
prove permanent starvation or the cause of every client's slow startup.
[Safari seek PR #623](http://192.168.4.7:3000/noirr/plurx/pulls/623) already
implements durable viewer interest and a reserved source-reader lane.
Its final main result is now integrated into this branch. No duplicate scheduler
repair is added; no live claims or production queues were reset.

## Generated transport experiment — incomplete captures retained

`scripts/playback-startup-lab` serves generated H.264/AAC fMP4 with a fixed
16-second target. It uses the repository's shipped hls.js library or native
Safari's video element, without an accelerated client reload loop. Its
completed inventory grows at the specified production ratio; segment-bearing
snapshots are chosen on 8-second ticks and retain a 160-second window.
The initial burst is explicitly modeled at 8× with a 3-second setup cost.
It does not measure actual daemon source production, actor fences or scratch.

The retained comparison uses 32 seconds of post-position runway and
7.965 seconds of origin lead, so complete-segment rounding exposes 48
seconds total and 40.035 seconds after the requested position. At 1.05×,
Chrome hls.js reported first frame at 9.126 seconds and continued without a
reported stall through a captured 23-minute window. Safari native
reloads approximately every 16 seconds and shows moving generated video,
but its first-frame callback remains absent; TTFF is **unavailable**.
The original native observer counted `waiting` only after the missing frame
callback, so its zero counter is not valid stall evidence. The tracked lab
now counts waiting after sampled playback progress independently of TTFF.
The native window was subsequently closed, so that run is incomplete.
These results do not select a shipping policy or prove a 30-minute run.

Bulky generated media and evolving receipts are owned by this effort under
`/private/tmp/plurx-startup-evidence-20260929`. Retain a final sanitized
summary and hashes before cleanup. The Mac computer-use tool briefly reported
that the machine was locked; retry succeeded. This was an automation error,
not a confirmed physical-device blocker.

## One-hour timing model — useful rejection, no client qualification

The design replay now runs 1280 one-hour scenarios across the candidate
floors, rate clamp edges, 0.95×/1.05×/1.2×/2× production ratios, cold/resume
origin leads, reload alignment, and a two-second source pause. Changed reloads
wait 16 seconds and unchanged reloads wait 8 seconds. Server inventories
advance on eight-second ticks. The 2.5-second transfer/append allowance remains
an assumption.

At 1.05× production relative to consumption, modeled buffer deficits occur in
50/64 scenarios for the 12-second candidate, 48/64 for 16 seconds, and 14/64
for 24 seconds. The 32- and 48-second candidates have no modeled buffer
deficit in that subset. All candidates include illegal update-spacing cases
when production is too slow in absolute media time: sustainable consumption
alone cannot make a 16-second object appear within the 24-second protocol
limit. The model does not exercise daemon flow/scratch/actor constraints or
native initial selection, so zero modeled deficits is not qualification.

Receipt: `/private/tmp/plurx-startup-evidence-20260929/design-replay.json`,
SHA-256 `9c2633cf73d5a2afe993d29f72aa8ab20e33253db2812cf0268463695a62fea5`.
This is a bounded design experiment, not a unit-suite execution. Runtime
runway remains unchanged while actual transport qualification is incomplete.

## Preparation correction — one consistent admission-settings read

`TranscodeManager::vod_settings` previously issued six serial `get_setting`
requests on every VOD attempt, including an index miss followed by rolling
fallback. The Hiqlite backend makes each a separate consistent read. The
correction uses the existing bounded `get_settings` contract to read exactly
those six keys from one snapshot. That removes five Store round trips without
caching settings or changing their defaults, clamping, request caps, or explicit
maintenance refusal. SQLite also reads the tuple in one statement.

`vod_settings_snapshot_preserves_budgets_and_maintenance_refusal` is authored,
not run. It covers stored values, invalid values, server/request budget caps,
and the explicit refusal. The existing blocked-GET-cap regression remains.
Pinned all-target compilation passed for the correction. No measured TTFF
improvement is claimed until the changed daemon is measured.

## Physical-device discovery — addresses are known

Xcode discovered physical Apple TV **Bedroom**, AppleTV14,1, device
`00008110-001C19140299801E`. The effort built a signed debug client and installed
it successfully. The first launch was refused because the TV was asleep;
a later retry launched and produced real physical-device telemetry.

On September 29 EDT (receipt timestamps September 30 UTC), the current
production H.264-to-AAC remux cold start reported **11,259 ms to first frame**.
The run used the existing device harness at 100 Mb/s; its later 99 Mb/s stage
was scheduled beyond the observation window and never applied. This is a
production baseline, not a measurement of the new settings correction.

At 03:49:58.914 UTC the node logged serving-authority expiry and, at
03:49:59.186, self-fenced the exact transcode session after quorum loss.
The Apple client reported `503:serving_fenced`, then HTTP 410 and terminal
AVPlayer failure at 03:50:27.162. Authority recovered at 03:50:08.885, after
the session had been fenced. Thus this run is not a continuity pass and does
not authorize a lower startup runway. An earlier authority expiry/recovery
also occurred at 03:47:03; the instability is observed more than once.

Receipt: `/private/tmp/plurx-startup-evidence-20260929/apple-current-policy.json`,
SHA-256 `18524ebe57824454cf9eb7e8ccc36dda035e0597fe74e1abea36616c2f00a1c9`.
The harness restored the app to its normal launch after collecting the failure.
No production queue or serving-fence rule was changed.

DNS-SD discovered Google TV Streamer **Bedroom TV** at `192.168.4.108`.
The installed Android SDK reports no connected ADB devices or advertised ADB
mDNS services; connecting its usual port 5555 failed. Cast discovery proves
network presence, not a debugging connection. Wireless/network debugging or an
existing alternative ADB port is required for automated client measurements.

## Decisions — preserve contracts while evidence is incomplete

1. **Ship with the current runtime runway.** Earlier readiness does not
   prove that the next segment is discoverable before its buffer drains.
2. **Reuse the exact-preparation repair landed in #623.** It overlaps
   the demonstrated source-reader bottleneck; a second scheduler change
   would create competing ownership and integration work.
3. **Keep unavailable first-frame evidence explicit.** A moving time counter,
   decoded frame or `playing` event does not replace presentation timing.
4. **Defer unit execution.** Compiler, format and Clippy checks run during
   development; the authored replay executes only after the final review.

The final conservative decision follows the build contract's fallback: no
smaller fixed-target policy qualified, so retain 48 seconds and ship the
demonstrated settings-read reduction plus phase instrumentation, alongside
the integrated preparation repair from main. This completes the bounded
implementation decision; it does not establish a new fleet TTFF guarantee.

## Final implementation review — one finding addressed

The independent reviewer examined the complete diff against main `4cb902884`
at head `45057c5aa`. No production correctness findings: settings semantics
and all publication/decoder/attempt/scratch gates remain intact. The review
found concurrent harness receipt writes could corrupt or lose telemetry.
Per-run locks now cover mutation, snapshot and persistence; a temporary file
is atomically replaced, including playlist reload records. A local concurrent
check retained all 128 unique events plus a reload in valid JSON. Required
fast-lane validation remains the merge prerequisite; no new TTFF claim.


## October 2 amendment — candidate rejection and generated browser sweep

The September measurements above are retained history of the conservative
settings correction. Initial build base was `dea1a403e`; current main `b43d9cdb` is integrated; the current decision
receipt is [M2-20261002](PLAYBACK-STARTUP-LATENCY-M2-20261002.json).

The new replay uses the normal 16-second publication cadence, complete
8/16-second objects, 0.25–4× playback, 1.05× relative production, three origin
leads, three reload phases, a 0/2-second source pause and an assumed 2.5-second
transfer/append margin. `scripts/playback-startup-trace --output <owned-path>`
reproduces it. The candidate first-snapshot endpoint is capped by position plus 124 seconds;
when rounding up crosses that snapshot limit, the model uses the largest
complete eligible endpoint within the limit. The producer keeps its existing
paid steady allowance and complete-cut envelope (140 seconds at 4×); first
snapshot selection does not shrink that grant. The ceiling regression checks
both limits separately. These margins are experiment inputs, not measured
fleet bounds.

| 1× candidate / minimum | Buffer deficits / 180 rows |
|---|---:|
| 12 / 12 seconds | 130 |
| 16 / 16 seconds | 102 |
| 24 / 12 seconds | 68 |
| 24 / 24 seconds | 44 |
| 32 / 12 seconds | 18 |
| 32 / 32 seconds | 0 |
| 48 / 48 seconds | 0 |

Keep a 32-second candidate minimum rather than assuming the 12-second writer
gate provides enough runway at slower playback. Fifty-four rows per candidate
cannot produce a new complete object within 24 seconds at low absolute
production. This applies to the old policy too; those rows are not protocol
qualification and are not converted into a pass by additional buffering.

The generated Chrome/shipped-hls.js sweep observed first frames after 4.459,
4.428, 6.548, 8.639 and 12.868 seconds for 12/16/24/32/48-second thresholds.
Each short trial had no observed waiting event after the first frame in its
45-second window. The selected 32-second Chrome continuity run completed:
first frame 8.683 s, 1,869.208 s of advancing playback, and zero observed
post-first-frame waiting, native errors or fatal HLS errors. The full receipt
SHA-256 is `58b7a6a0e5c6676cb43df1cdd03ea3f9b7cb1e6fa8f17f61167a27929ef23448`.
Safari native completed its generated 30-minute window: composited frame
8.665 s, first `playing` 8.664 s, and 1,800.742 s of advancing playback.
There were zero waiting events after 0.5 s advancing progress within that
window, no native errors and no classification errors. An initial waiting
bounce at 8.673 s is retained. The full receipt continues beyond the window:
Safari waits at 1,895.913 s because the fixture caps its complete prefix at
237 × 8 s = 1,896 s without ENDLIST; this is recorded harness exhaustion,
not a daemon EOF qualification. The receipt SHA-256 is
`893fd4967cbf5c494511504281c66235de4e142b2b30f3d9c1113a1c257b5aa4`.
Record initial composited frame and advancing playback separately: Safari
can show the first frame while still buffering. Do not infer native/HEVC
qualification or production startup timing from these AVC trials.

The fixture exposes a complete 8-second object at 2.25 seconds, bursts at
3.8× until the selected bootstrap, then runs at 1.05×. It includes every new
completed endpoint on 16-second snapshot cycles. It exercises actual shipped
web startup/loading code and the HLS engine, with generated local media; it
does not exercise daemon actor, production flow or scratch. The authored
server regressions exercise publication seams separately and remain unrun
until final review, per the build contract. The continuity fixture supplies
the actor established-presentation fact through its existing test-only
marker; it checks AwaitingPresentation, ActiveLowReserve and a reachable
Steady phase, rather than claiming an HTTP response-commit or actor
presentation-progress proof.

Current main preserves PR #703's index reconciliation and the existing
durable playback-interest/source-reader admission. The incident census is a
dated review report; no fresh live request-cycle trace was obtained during
this build. There is no demonstrated new queue defect to justify a second
scheduler change, no digest fence is weakened, and no live requeue occurs.
Boot storage-probe/consensus contention remains a reported pre-creation cost;
this build does not claim that changing publication removes it. First complete
media at 2.25 seconds is already separated from the first served snapshot.

The interim candidate buffers 32 media seconds and has no few-seconds
startup guarantee. Native readiness and preparation deadline alignment repair
the failure attribution even when ordinary HLS still needs a larger runway.
Track S and physical Apple/Android acceptance remain open. Deployment and
live reference-film acceptance are separate and have not occurred.

## Conservative admission after final review

The 32-second candidate is restricted to test builds after the final review
identified insufficient source/rate qualification. All production transport
labels preserve the existing conservative runway before the first response.
The current shipped client/parser/authority helper ran a bounded generated
AVC/AAC Safari smoke with 48 seconds of readiness: first playing at 12.858 s,
first frame at 12.861 s, maximum position 88.732 s, with no waiting/errors
after advancing progress. It deliberately ended before 30 minutes; the page's
initial continuity label does not turn this capture into that qualification.
The M2 receipt retains its raw path/hash and last advancing sample time.

The previous ~8.7-second, 30-minute Chrome/Safari captures remain evidence for
the isolated 32-second experiment and their captured source hashes. They do
not describe the production buffer policy. Daemon/resource-path, HEVC/DV,
resume/GOP, rate/remote-transfer and physical device evidence remains absent;
those classes receive no smaller production gate. The couple-second target
is still unmet. No production deployment or live film acceptance occurred.

## Wicked on nynuc, 2026-10-02 — preparation blocked by its viewer

A fresh live capture on `v0.3.0-5476-g9a719fcb7` matches the reported Safari
web playback of Wicked (file 120), resumed at 3519.601 seconds. This attempt
used native HLS, copy-video HEVC/Dolby Vision and TrueHD-to-AAC audio.
The client reported **11,019 ms to first frame**. UTC phase evidence:

| Phase | Time | What it establishes |
|---|---|---|
| Play, inferred from first-frame telemetry | 19:16:21.276 | Start of the client interval |
| Decision | 19:16:21.684 | Initial route selection |
| Index miss | 19:16:23.499 | Rolling fallback, exact preparation requested |
| Producer spawn | 19:16:23.961 | 90-second initial burst, subsequent 2× read rate |
| First complete segment | 19:16:24.777 | 3.170-second segment, 546 ms writer elapsed |
| Writer's 12-second gate | 19:16:26.055 | 15.891 seconds produced |
| First served snapshot | 19:16:29.823 | 52.970 seconds produced, conservative 48-second bootstrap |
| Native attachment | 19:16:30.012 | Browser receives playable presentation |
| First frame | 19:16:32.295 | 9.4 seconds of client runway |

The snapshot required 3,574 ms after the gated inventory. The reported
5,158 ms init-inspection interval includes readiness waiting; actual init
validation was 11 ms. Treating that entire interval as parser cost would
misdiagnose the delay. An adjacent Avatar: Fire and Ash attempt used the
same conservative path and reported 9,688 ms to first frame.

The live Store snapshot supplies a new preparation defect beyond the earlier
census. Wicked's exact current-engine/recipe request remained queued with
zero charged attempts and `foreground_preempted`; its playback-analysis
waiter was live during the captured playback. Avatar showed the same
preemption. Both titles had indexes for older engine identities, which are
correctly ineligible. Playback had durably requested preparation, but the
daemon's unconditional idle check prevented its own busy node from attesting
the source. Store viewer admission alone could not make that worker run.

The correction admits fragment-index source attestation on a busy node only
while that exact request has an unexpired pending playback waiter. Candidate
selection and fenced claim both check interest. The source read retains its
existing bounded source-I/O reservation. While playback keeps the node busy,
it stops when viewer interest ends; lease loss and disabling indexing also
stop the read. Ordinary maintenance stays
idle-only. Artifact production still uses its separate encoder/resource
admission; this correction does not relax that admission or digest identity.

`busy_analysis_worker_claims_only_live_viewers_without_spending_maintenance_attempts`
passed against SQLite and three-voter Hiqlite using pinned Rust 1.97.1. It
covers maintenance deferral, compatible-engine filtering, viewer admission,
interest expiry and subsequent idle maintenance. The live source logs and
user-approved three-minute private sample remain in the local temporary
evidence directory, outside Git. This diagnosis does not establish a new
production first-frame result; the 48-second policy remains unchanged.

The daemon wake regression `playback_preparation_wakes_busy_analysis_only_for_its_live_viewer`
and the existing idle-worker wake regression pass. The new test observes the
claim fence, because an incompatible local media engine legitimately refunds
its charged attempt after admission, and verifies viewer cancellation ends
the busy-node source selector. The existing
`analysis_hash_stop_signal_observes_foreground_playback` test also passes.

### Local real-media baseline

With the user's approval, a stream-copy excerpt of the next 180 seconds at
Wicked's reported resume point was transferred to this Mac's temporary test
folder. An isolated daemon bound only to loopback scanned that sample; Safari
used the shipped web client and native HLS, preserving HEVC and converting
Dolby Vision P7 to P8.1 plus TrueHD to AAC. The lab enabled the same unverified
HEVC rolling fallback used by the reported attempt. No production setting
was changed.

The unchanged conservative policy reported **3,580 ms to first frame**,
306 ms to the first completed segment, 843 ms to the writer gate and 1,260 ms
from gated inventory to the first served snapshot. The create handler took
59 ms with 29 local reads and three mutations, versus 2,520 ms with 33 local
reads and six mutations on nynuc. Safari reached 3:00 of 3:00; no post-start
stall/error was reported in that capture. These are real daemon/client
measurements, not the earlier generated-media fixture.

This is a different machine, local source storage, FFmpeg 9.0.1 rather than
nynuc's pinned 8.1.3, and a remuxed excerpt whose start is the original resume
point. It does not reproduce full-file seeking costs or qualify fleet-wide
latency. The fresh lab initially refused unverified HEVC until its settings
were matched; that refusal is excluded from the successful baseline.
