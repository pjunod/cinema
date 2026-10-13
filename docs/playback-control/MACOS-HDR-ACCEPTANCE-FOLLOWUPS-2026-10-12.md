# Mac HDR acceptance — playback follow-ups

**Status:** open — observed failures and bounded investigations; F1 repairs are built; new hardware qualification remains open · **Recorded:** 2026-10-12.

The [full acceptance report](../streaming/MACOS-METAL-HDR-ACCEPTANCE-2026-10-12.md)
closes the ordinary HDR10 Metal/CPU comparison. This document specifies the
seven remaining playback and fidelity follow-ups, including what happened,
what the evidence establishes, what remains unknown, and how to close each.
It is the detailed follow-up plan for backlog A02, A04 and A06, rather than a
new implementation project duplicating their existing work.

## 1. Baseline, evidence and scope

The accepted Mac correction landed in
[PR #1038](http://forge.lan:3000/noirr/plurx/pulls/1038), merge
`0da656d3a3e7d7f0c52387543571d2ff4a265340`. The physical Air comparison used
`94dee81daf5fa2c3c400f873c05ee98df32f28d4`; the laptop was checked on
`f01b8605d`. Those identities are observation baselines, not claims about
what is installed when a future investigation starts. Record the current
main, installed daemon, browser and pinned media-tool identities before
attempting a reproduction. Source-only requalification of PR #1038 does not
make a later source tree physically tested.

American Gangster, local catalog file 15, supplied the Air's ordinary HDR10
case: 3840×2160, MaxCLL 1179, requested 1080p SDR. Heretic, local file 398,
supplied the laptop case. Catalog IDs are local reproduction hints; retain
source identity and selected-stream facts so another catalog can identify
an equivalent sample. Heretic's source/negotiation path must be qualified
separately from the ordinary HDR10 Air case.

### 1.1 Where the retained records live

- [Evidence manifest](../streaming/evidence/macos-metal-hdr-20261012/evidence-manifest.json):
  189 retained files, original checksums and storage locations.
- [Committed text receipts](../streaming/evidence/macos-metal-hdr-20261012/text-receipts.tar.gz):
  logs, measurements, scripts, failed attempts and qualification receipts.
- [Private screenshot/evidence archive](http://forge.lan:3000/attachments/8e191b9e-898b-4525-8cfb-77897f99c559):
  original captures attached to PR #1038; archive identity and checksum are
  recorded in the manifest.

Evidence member names below refer to those archives. The text copies remove
credentials and use neutral infrastructure names; screenshots retain their
original pixels privately. The acceptance report and evidence were committed
and merged in [PR #1043](http://forge.lan:3000/noirr/plurx/pulls/1043), merge
`eef013b5e2818b4018b25a8a138e1fd41677fa87`. Preserve the negative records when
adding successful reruns. A screenshot without `5m-presented` is not accepted
proof of the exact five-minute frame.

Times below are October 12 UTC unless otherwise stated; the local acceptance
session used EDT, UTC−4. Startup latency, playback position, seek latency and
server authority outage are separate quantities.

### 1.2 What is already accepted

Corrected Metal started in 2.363 seconds and played through five minutes with
zero interruptions and zero dropped frames out of 7,810. CPU started in
4.027 seconds. Buffered paused seeks independently proved presentation at
300,000 ms in 39 ms and 36 ms. Actual producer commands identify the Metal
and CPU graphs; browser intrinsic dimensions identify the 1920×1080 delivery.
The parser is installed and FFprobe identity binds on both Macs.

These successes coexist with the failures below. They establish neither
cold-start reliability on all sources nor uninterrupted quality switching,
unbuffered paused seeks, calibrated grade parity or complete UI metadata.
The original network-volume mount and service-access problems were repaired
operationally; do not presume they explain a fresh failure after the repair.

## 2. Work order and ownership

Priority here is a proposed investigation order, not an assertion that every
observation is a confirmed code defect. No new builder is assigned by this
document. The existing workstream that accepts a package records its owner,
current baseline, reproduction and disposition in its ledger.

| ID | Follow-up | First deliverable | Existing workstream |
|---|---|---|---|
| F1 | Unbuffered paused seek and stale-picture evidence | Reproduce with actual frame settlement and locate the first missing lifecycle transition | A04 lifecycle; A09 web seek |
| F2 | HDR remux → 1080p SDR quality change loses alignment | Explain the failed alignment receipt and distinguish safe refusal from an applied switch | A02 continuous quality |
| F3 | Abandoned preparation retains capacity | Trace terminal ownership and reservations; distinguish legitimate retention from failed cleanup | A04 lifecycle; A02 preparation |
| F4 | Laptop cold/remux startup unreliable | Matched cold/warm stage timeline for Heretic | A04 startup/recovery; A09 client playback |
| F5 | Delivered-rendition metadata incomplete or misleading | Join planned, published and attached facts without inventing actual renderer data | A02 presentation; A04 observability |
| F6 | Transient replicated-authority self-fence and 503 | Attribute the incident and separate admission failure from existing-viewer continuity | A04 serving/recovery |
| F7 | Residual Metal darkness and weak boot parity fixture | A pinned grade comparison and a diagnostic that detects the old peak error | A06 codec/GPU and HDR fidelity |

Start with F1–F3 because they affect ordinary interaction and admission, and
can share a source-bound trace. F2's failed preparation may be related to F3,
but the evidence does not establish a common root cause. F4 needs a separate
cold/warm experiment. F5 can improve evidence for all of them. F6 is a
recovered incident that needs attribution before any policy change. F7's
narrow regression-detection work can proceed independently; broader grade
qualification follows a declared reference contract.

Reconcile against the [lifecycle status](PLAYBACK-LIFECYCLE-STATUS.md),
[coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md),
[buffer observability contract](PLAYBACK-BUFFER-OBSERVABILITY-STATUS.md),
[continuous-quality status](CONTINUOUS-QUALITY-STATUS.html) and
[quality matrix](CONTINUOUS-QUALITY-MATRIX.md) before selecting code changes.
Prepared transitions, cancellation, generation fences and recovery ownership
already exist. A reproduction should identify the failing rule in that code.

## 3. F1 — paused seek outside the local buffer stalls

### Observation and evidence

A paused seek outside buffered video stalled for roughly 20 seconds and
could reopen paused. A timeline showing 5:00 could still display an old
picture. The earlier `air-4e-metal-5m.jpg` and
`air-4e-metal-5m-info.jpg` are invalid exact-frame evidence: the displayed
picture remained from approximately 5:13 under a 5:00 timeline label.

Retained logs: `air-4e-final-playback.log`,
`air-4e-metal-startup-and-seek.log`, `air-94-metal-5m-proof.log` and
`air-94-cpu-5m-proof.log`. A resume-to-zero restoration also encountered an
unbuffered seek stall; the saved resume position was subsequently verified
as zero. Saving a position and presenting its picture are distinct outcomes.

The corrected buffered controls were:

| Path | Frame settlement time | Local buffered range before seek | Result |
|---|---|---|---|
| Metal | 22:11:52.543174, 18:11:52 EDT | 294,294–386,386 ms | `presented_frame`, landing 300,000 ms, 39 ms |
| CPU | 22:14:34.795471, 18:14:34 EDT | 298,795–382,382 ms | `presented_frame`, landing 300,000 ms, 36 ms |

Those controls prove buffered landing only. An empty interruption counter,
`currentTime` assignment or seekbar label does not prove frame presentation.

### Investigation

1. Repeat a paused, unbuffered seek on current source with one owned session.
   Record the accepted target, seek sequence, attachment generation, previous
   frame time, local ranges and server-ready range at the target.
2. Join the demand/control update, target segment request, producer readiness,
   response, append, decode callback and frame settlement on that sequence.
   Distinguish a genuinely missing segment from a distant buffered island or
   a stale snapshot. Find the first stage that fails to advance.
3. Check pause handling, demand issuance, loader state, attachment ownership
   and the existing recovery deadline. Determine whether recovery reopens
   the intended recipe and whether its callbacks belong to the new attempt.
4. Repeat with a superseding seek and a close during the pending seek. Old
   requests, callbacks and timeout actions must not settle the newer intent.
   Compare with the existing [web seek RCA](../streaming/WEB-SEEK-LATENCY-RCA-AND-FIX.md)
   before assuming this is a new defect.

### Acceptance and regression

Cover forward/backward, paused/playing, buffered/unbuffered and cold/warm
media rows. For each accepted seek, prove the presented target frame on the
right generation within the declared seek budget, or a truthful bounded
terminal failure. A successful paused seek remains paused at the new frame;
a superseded or closed operation cannot later reopen playback.

Declare target tolerance and latency budget from the current contract before
running the matrix. If the contract lacks a budget, propose and review one;
the observed 20-second wait is not an approved tolerance. Add a regression at
the identified demand/loader/settlement boundary, plus a real browser receipt
for frame presentation. A test that only updates the timeline is insufficient.

### Repair progress — local VOD repair merged; reopened-startup repair reviewed; HDR acceptance open

F1 is claimed by the paused-seek repair branch. On the October 12 main
baseline `6dc031387`, Pause suspends the established HLS loader with
`stopLoad()`. Local VOD seek execution then assigns `currentTime` without
restarting loading. An unbuffered paused destination therefore cannot arrive.
A later Play can also reject the old suspension because seeking advanced the
intent generation between Pause and Play.

The repair in `player/player.js` and `player/transport.js` grants target
loading to the current executed VOD seek on its exact media element and
attachment. It keeps the video paused, permits the owned playlist request,
and restores suspension after actual settlement. Supersession retires the
previous seek's loading and timer. The next Play can resume the newly held
loader; Play during loading leaves it running. Pause during a pending VOD
seek still permits that seek's target loading. A buffered target retains a fresh
suspension so Play before settlement can restart fetching. A paused deadline
with bytes but no target frame invokes the existing reopen; it cannot silently
retire recovery based only on coverage.
No Rust behavior, playback preference or feature switch changes.

The [repair receipt](evidence/paused-vod-seek-20261012.json) preserves both
negative and positive controls:

| Control | Observed result |
|---|---|
| Shipped seek regression before repair | 21 pass, one fails: Pause stops fetching and seek issues no target load |
| Focused seek/loader regressions after review | 43 pass; target-frame settlement, stale lease/attachment rejection, supersession, Play/Pause interleaving and deadline cleanup |
| Real Chrome, generated MPEG-TS, stopped-loader baseline | Target 35 s stays seeking, with no target frame or target buffer after 1,502 ms |
| Same browser/fixture with repaired loading | Unbuffered forward frame 35.000 s in 22 ms; unbuffered backward frame 24.080 s for target 24 s in 8 ms; both paused with seeking completed |
| Reviewed source, same Chrome fixture | Forward target frame in 19 ms, backward target frame in 5 ms; both remain paused and finish seeking |
| First browser harness | Preserved but invalid for final settlement: it accepted the callback before `seeked` and reused a buffered backward target |
| Fresh Air attempt on acceptance build `94dee81da` | Catalog reports analysis pending; original quality uses rolling remux. The rolling unbuffered reopen fails before media arrives. This does not qualify the repaired paused VOD path. |

Full `make web-unit-check`, web syntax and the unchanged TypeScript baseline
pass. The pinned Rust 1.97.1 baseline compiler check also passes. Reproduce the
interactive frame diagnostic with
`node tests/web/paused-vod-seek.fixture.cjs`, open its printed loopback URL and
press **Run diagnostic**. Set `PLURX_F1_RECEIPT` to retain its JSON outside the
temporary fixture directory. The diagnostic uses generated local media and
the shipped loading helper; the command-level regressions separately execute
the shipped seek path. Neither is a genuine HDR physical-display campaign.

Original HDR failures remain in the acceptance manifest/archive unchanged.
New Air failures and both browser harness versions are retained alongside the
repair. Astra found and verified the correction of a buffered-seek
Play-before-settlement race; three new regressions fail before that correction
and pass afterward. Remaining closure work is normal PR qualification,
and the source-bound HDR-on-Air paused VOD matrix from this section. Native
HLS and failed paused reopen/startup paths retain their separate qualification
needs; this local VOD loader repair does not claim to close them. In particular,
a forced reopen while still paused suppresses its fresh startup manifest. The
deadline test proves a bounded reopen attempt, not autonomous settlement or
terminal failure on that replacement.

The local VOD repair merged in [PR #1047](http://forge.lan:3000/noirr/plurx/pulls/1047)
as `b2adc5f789f981e189ad72bfbe48383355b5b272`; run 4935 passed the current
candidate's policy, web, Rust and Main promotion gates. The earlier statements
about qualification describe the retained pre-merge evidence.

The October 13 follow-up gives an executed paused hls.js replacement seek its
own startup loading lease. It can send the first playlist without Play and
complete startup on the actual target frame, then restore suspension. Its
original deadline remains active while paused; the real diagnosis raises
`startup_exhausted` with Close/Retry if no frame arrives. A suppressed or
interrupted unparsed playlist uses the existing single corrective reload.
Play carries a reserved reload to the current intent; Pause during the pending
seek preserves target loading. Supersession and cancellation fence old work.
Three initial regressions fail before the repair; 51 focused tests now pass.
Astra found and verified corrections to renewal, retry handoff and unparsed
supersession. Normal follow-up qualification and genuine HDR-on-Air acceptance
are pending. Native HLS remains unchanged and is not newly qualified. The
existing repair receipt retains the source identities and failed controls.

## 4. F2 — manual SDR quality change loses alignment

### Observation and evidence

On candidate `94dee81da`, Air Chrome in its default HDR-capable profile was
playing American Gangster as 2160p HDR remux. A requested 1080p SDR change
failed at 22:01:11.464582, 18:01:11 EDT. Preparation
`6bdd65ef-6dd6-450b-b997-e88e1f16599d` reported:

```text
lost-alignment: phase=incumbent-frame video_callbacks=4 bad_frames=0
last_frame_age_ms=34 drift_ms=51 ready_state=4 buffered=true
```

The receipt also reports 17.1 seconds of runway. The optional manual target
failed and the incumbent HDR remux remained attached. That is retained
playback after a refused change, not a successful 1080p switch. See
`air-94-metal-startup.log`, `air-94-metal-startup-error.log` and
`air-94-metal-5m-proof.log`.

### Investigation

1. Reproduce the precise remux HDR → encoded SDR transition on current main,
   recording incumbent and prepared generation, source timestamps, media
   origins, audio timeline, browser profile and requested/effective recipe.
2. Identify which callback supplied the 51 ms drift, which clock it was
   compared with and whether both observations refer to the same instant.
   Trace frame freshness through inactive/prepared elements and any timestamp
   transform. Four callbacks and `buffered=true` alone do not prove alignment.
3. Locate the existing alignment policy and explain whether refusal was
   correct, caused by stale observations, or caused by incorrect mapping.
   Do not widen the threshold just because this sample failed at 51 ms.
4. Exercise cancellation and supersession, then inspect preparation teardown
   for F3. Keep the failure trace even if the next switch succeeds.

### Acceptance and regression

Add this source/path pair to the existing continuous-quality matrix. Include
playing/paused, visible/background, cold/warm artifacts and a superseding
change. Successful application requires the requested SDR rendition, frame
and audio continuity, and truthful applied-quality UI. A legitimate refusal
must preserve the incumbent and report one bounded, comprehensible failure;
UI metadata must continue to describe the incumbent.

Keep a regression for the demonstrated alignment or timestamp defect, and
independent frame/audio evidence for the physical transition. Existing Chrome
campaign successes remain valid for their own rows. A full restart only
qualifies an explicitly requested restart operation; it cannot stand in for
a claimed continuous transition.

## 5. F3 — abandoned preparation occupies VOD capacity

### Observation and evidence

After failed preparation and browser relaunch/profile changes, the test's own
session remained in Activity, client unobserved, producer held ahead, with GPU
capacity shown as 1/2. Two new starts failed at 22:03:31.885455 and
22:04:35.815610, 18:03 and 18:04 EDT:

```text
vod_family_capacity: the continuous media group cannot be admitted together
```

Both were surfaced as HTTP 500/internal errors. Stopping only that test's
session via Activity cleared the operational obstruction. The subsequent
corrected Metal start at 22:06:24.910103 took 2.363 seconds and completed the
five-minute uninterrupted run. See `air-94-metal-startup-error.log`,
`air-94-metal-retry.log`, `air-94-capacity-error.jpg` and
`air-94-metal-5m-proof.log`.

This establishes that retiring the owned session allowed admission. It does
not yet establish a memory/resource leak, the intended retention lifetime,
or whether the retained reservation was still legitimately owned.

### Investigation

1. Inventory the failed preparation's session, family, reservations, pins,
   producer, viewer lease and terminal state before and after abandonment.
   Trace acquisition and release to their existing owners.
2. Distinguish explicit cancel, failed change, tab close, browser quit and
   missing heartbeat. Identify the intended TTL/grace for each and whether
   restart reconciliation applies. A disconnected browser is not by itself
   proof that all server resources should be immediately destroyed.
3. Compare actual reservation accounting with the compound family admission
   decision. Determine which retained charge caused refusal, including any
   shared producer or another valid viewer.
4. Reconcile with [scratch reservation work](../streaming/SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md)
   before building another ledger. Separately inspect whether expected
   capacity pressure should be classified differently from an internal 500.

### Acceptance and regression

Use owned sessions to cover failed preparation, cancel, supersession, browser
exit, lease expiry and restart reconciliation. Include same-family reuse,
another family and two legitimate viewers. Abandoned charges and pins return
to the intended baseline within the declared policy; active viewers retain
their resources. Admission after cleanup succeeds without Activity manually
stopping an abandoned test session.

Normal pressure must still refuse safely and explain the retry condition.
The repair must preserve compound admission and resource limits. Prove the
terminal release in a regression at the owning state transition and record
before/after accounting in the physical receipt. Never stop another viewer
to make the capacity test pass.

## 6. F4 — laptop cold/remux Heretic startup remains unreliable

### Observation and evidence

Earlier Heretic starts encountered placement/startup failures, stalls and
quality-unavailable/refusal outcomes. A warmed forced-1080p SDR attempt
ultimately played: browser intrinsic size 2583×1080, 2,268 frames, zero drops
and zero interruptions. The paused position was 94.358 seconds into the
movie. **94.358 seconds is not startup time.** Intrinsic display width also
is not proof of the encoder's stored raster without sample-aspect facts.

Retained records include `laptop-heretic-20261012-failure.txt`,
`laptop-heretic-20261012-session.log`, `laptop-heretic-final-failure.txt`,
`laptop-heretic-quality-unavailable.txt`, `laptop-heretic-warm-retry.txt`,
`laptop-heretic-source-read.txt`, `laptop-heretic-diagnostics.txt` and
`laptop-heretic-1080-started-20261012.txt`/`.jpg`.

The parser-install/identity failure is repaired, and source access was checked
separately. The surviving cold/remux symptoms need their own diagnosis; the
warmed success does not identify the earlier bottleneck.

### Investigation

1. Hold source, stream, node/build, client/profile, requested quality and
   preferences constant. Record exactly which state is cold: catalog probe,
   fragment index, rendition artifacts, producer or client attachment. A fresh
   browser tab with existing artifacts is not a cold server start.
2. For matched cold/warm trials, measure source open, probe, catalog lookup,
   index readiness, placement/admission, producer start, first published
   media, HTTP delivery, client append and first presented frame separately.
   Associate every stage with one request/attempt and owner epoch.
3. Separate remux, forced SDR transcode and quality refusal outcomes. Verify
   actual selected-stream HDR/DV facts and supported route before comparing
   with the Air's ordinary HDR10 case.
4. Establish whether time is spent reading, computing, queued for capacity,
   waiting for authority, or waiting for the client. Retry and independent
   file-descriptor diagnostics are evidence to interpret, not a root cause.

### Acceptance and regression

Declare startup budget and repeated cold/warm trial count before testing.
Use isolated artifacts or an owned test namespace to establish cold state;
do not erase a shared cache. Every trial ends in a proved first frame within
the budget or a truthful bounded refusal with an attributed stage. A warmed
retry cannot replace a failing cold row.

Add the smallest regression for the isolated bottleneck or incorrect deadline,
then repeat the original cold route and forced-SDR control. Publish latency
per stage, failure rate, route and source identity. Keep unsupported-source
refusal separate from supported playback reliability.

## 7. F5 — playback information does not fully describe delivery

### Observation and evidence

Cached VOD PlaybackInfo lacked renderer/pipeline and encoded-frame rows;
“Stream frame” was unavailable. Earlier laptop HDR badges disagreed with SDR
delivery. INFO-level VOD logs omitted FFmpeg argv. Acceptance therefore used
the actual running producer command plus browser intrinsic dimensions, not
the panel alone, to prove the Air graph and output.

A `vod` encoder/producer-held label describes delivery/production state; it
is not the actual tone-map renderer. See
`air-94-metal-5m-presented-info.jpg`,
`air-94-metal-producer-command.log`, `air-94-cpu-producer-command.log` and
`laptop-heretic-diagnostics.txt`.

### Investigation

1. Trace planned recipe, effective producer graph, published artifact facts
   and currently attached client rendition as separate evidence sources.
   Identify missing fields and where current code obtains each displayed row.
2. Bind actual facts to source identity, rendition/generation and cache
   provenance. A cached artifact can outlive a preference change or a producer;
   current node settings cannot reconstruct its actual renderer.
3. Distinguish encoded raster, sample/display aspect ratio and client intrinsic
   size. Likewise distinguish source HDR metadata, delivered transfer/color
   facts, negotiated capability and actual processing contribution.
4. Join diagnostics without exposing credentials, capability URLs or real
   infrastructure in mirrored receipts. Prefer structured provenance over
   inferring a graph from a template or changing global logging preferences.

### Acceptance and regression

Check fresh and cached artifacts, CPU/Metal, successful and failed quality
changes, stop/reopen and alternate-owner routing. UI/API facts must follow
the attached delivered rendition. After F2's refusal they still describe the
incumbent HDR remux; after a successful SDR change they describe SDR.

Show unknown when actual evidence is absent. Add focused identity/provenance
and presentation regressions at the discovered seam, including stale replies
and artifact reuse across preference changes. An actual graph receipt and
client delivery facts must remain independently inspectable.

## 8. F6 — serving authority briefly self-fences and startup returns 503

### Observation and evidence

During the earlier `4e` run around 20:46 UTC, 16:46 EDT, replicated state
application/quorum watermark operations timed out. A startup request returned
503 after approximately 4,216 ms while claiming its media-session request.
Authority recovered at 20:46:35.737490. The transcode recovery line reports:

```text
outage_ms=2468 outage_budget_spent_ms=2468 kept_sessions=0 grace_ms=5000
```

This proves a 2.468-second authority outage and recovery within that logged
grace. `kept_sessions=0` does not prove continuity for an active existing
viewer. Request latency, sampling outage and serving outage have different
anchors. See `air-4e-final-playback.log`.

### Investigation

1. Join required/applied watermark, term, owner epoch, transaction/apply times,
   authority loss/recovery and the specific rejected request. Determine the
   slow operation and compare with existing [quorum incident work](../streaming/HEATED-RIVALRY-S1E5-QUORUM-RCA-AND-FIX.md).
2. Separate admission fencing, an already-attached viewer's grace handling,
   owner migration and actual client frame interruption. Collect explicit
   active-session counts and frame evidence for any continuity claim.
3. Check recovery/retry classification and stale-owner rejection after authority
   returns. Preserve the authority guard while diagnosing its latency.

### Acceptance and regression

The recovered historical incident can be dispositioned with an attributed
cause and a current-source reproduction or explicit “not reproduced” result.
A controlled authority-delay case must preserve stale-owner fencing, recover
within its declared policy and give startup a truthful retryable result.
Existing-viewer continuity needs a nonzero owned session and independent
frame evidence. Exceeding grace needs a separate bounded terminal outcome.

Use an isolated fault-injection environment for deterministic regression.
This document does not request a live cluster partition, voter removal or
restart campaign. Disabling the guard to hide 503s does not qualify a repair.

## 9. F7 — residual grade difference and weak synthetic parity evidence

### Observation and evidence

The corrected peak serialization removes the old excess brightness and
highlight clipping, but Metal remains slightly darker than CPU in the real
five-minute comparison. Raw decoded-frame diagnostics report:

| Measurement at the matched 300 s frame | Old Metal | Corrected Metal |
|---|---|---|
| Mean luma | 57.7974 | 38.9083 |
| Luma mean absolute error against CPU | 13.7472 | 5.1532 |
| 95th-percentile absolute luma error | 23 | 9 |
| Maximum absolute luma error | 102 | 31 |

CPU mean luma was 44.0563. These are code-value diagnostics on a matched raw
frame, not display calibration or a perceptual grade score. See
`plurx-air-real-frame-check.log`/`.py` and
`plurx-metal-peak-check.log`/`.py`.

For this pinned FFmpeg build, Metal peak uses nits/10: 1179 becomes 117.9.
The CPU/CUDA/OpenCL terms use nits/100: 11.79. HLG is unchanged. A 1000-nit
fallback can mask the former serialization error; a non-round 1179-nit
neutral PQ diagnostic exposes it. The broad colored `testsrc2` boot probe
allows mean-channel difference 24 and accepts both variants: the diagnostic
record reports 8.209 for old and 8.341 for corrected. Passing that fixture
cannot establish detection of this real-source regression. See
`plurx-metal-boot-fixture-check.log`/`.py`.

### Investigation

1. Preserve absolute-unit and affected-cache-key regressions already landed.
   Build a deterministic diagnostic that fails the old serialization and
   passes the correction, with neutral PQ signals and non-round peaks.
2. Separate peak-unit regression detection from grade fidelity. Compare shadows,
   skin tones, neutral regions, saturated colors and unclipped highlights
   across multiple genuine HDR10 sources and timestamps, with matched frames,
   range, dimensions, transfer and tool identities.
3. Investigate Hable implementation, EOTF/gamma, gamut/range conversion and
   clipping before attributing residual darkness to one parameter. Keep HLG
   and Dolby Vision qualification in their own reference domains.
4. Declare the reference and thresholds independently before measuring. Follow
   [HDR reference scoring](../performance/HDR-REFERENCE-SCORING.md): its PQ
   pair scorer refuses SDR/HLG/DV inputs, while SDR VMAF requires a separate
   pinned SDR grade receipt. Its 1000-nit reference assumption does not by
   itself settle this sample's 1179-nit grade.

### Acceptance and regression

Close the narrow probe follow-up when the declared diagnostic demonstrably
rejects the old peak graph without rejecting qualified corrected controls.
Close broader fidelity only with a named corpus, independently pinned grade,
per-frame metrics, preserved failures and physical display evidence. Define
which differences are tolerable before seeing the final measurements.

Screen Sharing captures and forced-sRGB browser profiles are useful relative
observations, not calibrated physical-display measurements. Do not tune the
production curve to match one film frame or report SDR VMAF as HDR VMAF.
A parity-probe improvement and a calibrated grade qualification are separate
closure receipts.

## 10. Shared execution and closure contract

### 10.1 Minimum receipt for each reproduction

Record these fields in the existing owning ledger, with linked raw evidence:

| Group | Required facts |
|---|---|
| Identity | Main/patch commit, installed build, tool hashes, node role/epoch, browser/client version and profile |
| Source | Stable source identity, selected stream and HDR/DV/color facts, requested and delivered recipe |
| State | Explicit cold/warm definition, active/prepared generation, owned sessions, buffer/readiness ranges, capacity before/after |
| Operation | Attempt/control sequence, accepted target, timestamp anchors, supersession/cancellation and terminal result |
| Measurements | Startup/seek/switch stage durations, actual presented frame, dropped/interruption counts with their observation window |
| Outcome | Passed/failed/not run/not reproduced; confirmed mechanism versus remaining hypothesis; restoration and retained artifacts |

A useful failure taxonomy keeps source access, admission pressure, authority
fencing, preparation alignment, client delivery and presentation separate.
HTTP status or a final generic timeout alone is not a causal diagnosis.

### 10.2 What closes a package

Each package needs a current-baseline reproduction or documented inability to
reproduce, a justified disposition, a bounded repair where needed, the
smallest meaningful regression for that repair, and the physical acceptance
rows its contract requires. Source tests cannot replace frame/audio/device
proof. Physical success cannot replace a missing cancellation or accounting
regression. Record all failed attempts, even when a later retry succeeds.

If no defect is found, record the intended policy and the evidence showing
that it behaved correctly; identify any remaining UI or acceptance gap.
Do not silently close a historical observation because main moved.

Future behavior changes follow the repository's normal compiler, regression,
review and promotion workflow. This document itself is documentation only;
no playback rerun, code change, hardware action or new qualification was
performed to publish it. Update [the backlog](../features/PROJECT-BACKLOG.md),
the owning canonical ledger and this document together as F1–F7 progress.
The accepted HDR10 slice remains closed while those broader packages are open.
