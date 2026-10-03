# Web seek failures — why ready media waits, and how seeks should complete

**Status:** root causes reproduced; proposed repair, not implemented;
agent and Opus findings addressed in proposal · **Written:** 2026-10-02 EDT ·
**Incident/source build:** `4fa50b79e4b3b196797a9b7a1a7a4abd2184ea43` ·
**Browser:** Safari 27.0.1 · **Bundled player:** hls.js 1.6.16.

Companion to [PLAYBACK.md](../PLAYBACK.md), the
[earlier VOD seek repair](../clients/WEB-VOD-SEEK-MISSING-MEDIA-RCA-AND-FIX.md),
and the [Safari seek build status](SAFARI-SEEK-STATUS.md). This document
explains the October 2 media1 incident, separates the initiating failures from
recovery defects, and proposes a repair with physical acceptance criteria.
The earlier documents remain history; this diagnosis supersedes their
incomplete explanation of this incident. Read the
[web source map](../clients/WEB-SHELL-LAYOUT.md) before implementation.

## 1. Findings — delivery, presentation and reachability

The reported 8–12 seconds for a small seek is real. VOD can deadlock before
requesting the destination. Native rolling HLS often cannot seek locally,
although the bytes are buffered, and pays for a full replacement. A separate
VOD presentation-evidence bug converts an unfinished seek into an ordinary
stall, which spends the generic recovery budget and eventually stops the
player.

| Finding | Evidence | Confidence |
|---|---|---|
| VOD's initiating failure is paused ManagedMediaSource buffering that does not resume on an out-of-buffer seek. | A controlled same-title trace has `seeking=true`, HLS `IDLE`, loading enabled but buffering disabled, old fragment/next-load position, and no target coverage. One `resumeBuffering()` call obtains target media and clears seeking within the next 600 ms sample, on the same session. The vendored library lacks the upstream fix for this exact WebKit failure. | Reproduced mechanism; original pre-failure browser trace was not retained. |
| The VOD seek fallback is cancelled using false target-presentation evidence. | Frozen player: `localVodPresented=true`, fallback disabled, `seeking=true`, presented frames **405**, dispatch frame floor **405**. Executing the deployed progress function reproduces that contradiction. | Confirmed application defect. |
| Paused seeks can discard their sole destination frame. | The deployed callback subscription retains its old epoch across a seek; execution increments the epoch, and the observer rejects that callback for settlement. | Confirmed source-level defect; no paused physical reproduction retained. |
| Native rolling HLS's reachability guard routes small seeks to full reopens. | Bad Boys: Ride or Die traces show destinations inside `buffered` but outside `seekable`; reopen takes 6.567–9.567 s in the captured set. One reachable local seek takes 201 ms. | Confirmed route/cost; exact contemporaneous published edge is absent. |
| Control communicates demand but does not close the browser loading/presentation loop. | The target is represented in control; server status at 19:58 reports 190.4 s ready and zero parked HTTP requests while the browser still holds only 17:22–17:57. The generic client recovery explicitly waits 8 s before asking for a stall action. | Confirmed responsibility gap; zero HTTP wait alone would not prove zero requests. |

**Repair priority:** update the bundled loader to the supported upstream
ManagedMediaSource repair, fix presentation evidence, and remove quality
rediscovery from the fast-seek critical path. Then qualify a bounded native
rolling successor improvement, then steady coverage. Do not call the first repair a fix
for native rolling seeks: native HLS does not run through hls.js.

**Non-goals:** a new player, another watchdog, unconditional VOD reopening,
blindly reducing all timeouts, removing authority fences, forcing lower video
quality, adding feature flags, or promising instant arbitrary distant seeks.
This is a proposal, not authorization to deploy an unqualified repair.

## 2. Evidence — original incident and controlled intervention

### 2.1 Source identity and retained records

The container image revision and the Playback Info build agreed on
`4fa50b79e`. Diagnosis used a `git archive` extraction of that commit, not the
older working branch. The live served transport file matched that source.
Safari's runtime also reported `Hls.version === '1.6.16'`.

Retained artifacts:

- [Sanitized incident and browser evidence](../evidence/web-seek-rca-2026-10-02.json):
  26 seek-route/outcome events, source hashes, runtime snapshots and evidence
  limits. It contains no authorization token, media URL, credential or source
  filesystem path.
- [Mechanism replay](../evidence/web-seek-mechanism-replay.cjs): runs the actual
  progress function and bundled hls.js controller against a small diagnostic
  harness. It asserts the incident behavior, so it is expected to stop
  passing after a correct repair. It is not a production regression test.

Times below are October 2 **EDT**. Server logs use October 3 UTC, four hours
ahead. The browser captures were collected through Safari Web Inspector;
snapshots are transcribed into the evidence file. The controlled baseline
sampled every 200 ms; this is sufficient to identify state transitions, not
a submillisecond timing claim.

### 2.2 The viewer's failures

| Time EDT | Event | Interpretation |
|---|---|---|
| 22:48:10.484 | Native rolling target 3301.154 s, reachable end 3302.939 s; local landing 201 ms later. | The fast path exists when Safari admits the destination. |
| 22:48:14.139 | Next native target 3313.223 s, browser buffer through 3350.942 s, seekable end still 3302.939 s; reopen takes 6908 ms. | An ordinary +10 skip crosses the native reachability edge despite loaded bytes. |
| 22:48:45.305 | Sahara VOD target 890.530 s inside buffer; local landing 151 ms. | Healthy buffered VOD control sample. |
| 22:48:47.992 | VOD target 921.792 s, +28.701 s from the sampled element, beyond buffer end 911.098 s. | Small-seek failure; local route is correct and destination should be fetched. |
| 22:48:56.523–59.517 | 8066 ms supply stall, automatic reopen, then seek resumed at 11510 ms. | Most latency is waiting for recovery, not transferring the target. |
| 22:49:05.027 | Later VOD target 1045.552 s lands in 335 ms. | VOD failure is state-dependent, not every distant seek. |
| 22:49:09.753–18.506 | Target 1198.106 s from element 1049.989 s: +148.117 s, not a single +10/+30. Then 8065 ms stall, abandonment and exhausted surface. | Larger jump, compatible with accumulated presses or scrub; the input sequence was not captured. A second failed seek before recovery rearming stops playback. |

The frozen runtime still had the old buffer `[1042.280, 1077.1355]` seconds,
`currentTime=1198.1055`, `seeking=true`, and `readyState=4`. Its
`localVodPresented` flag was true, but no frame newer than the dispatch floor
had been presented. This is a contradiction, not healthy playback.

The inspected server readiness sample was fresh when first read; later
samples became stale and the retired session returned 410. Those later 410s
are aftermath and are not the initiating failure. Likewise the loader's
post-exhaustion `STOPPED` state is caused by the client stopping it; the
controlled pre-exhaustion capture below establishes the initiating `IDLE`
state.

### 2.3 A fresh VOD seek reproduced the loading deadlock

Reopened Sahara and allowed real playback, then issued one ordinary
`nudge(30)` through the same command used by the button.

| Browser sample | Media time / buffered range | Loader and media source |
|---|---|---|
| Earlier baseline, 15.695 s before dispatch-state sample | 1256.743 s / 1232.480–1286.330 s | `IDLE`; loading enabled; buffering disabled; ManagedMediaSource `streaming=false` |
| Seek dispatched | 1301.119 s / 1250.200–1286.330 s | `seeking=true`; still `IDLE`, buffering disabled; fragment 134 and next-load position 1286.320 s unchanged |
| After generic recovery reopens | New attachment begins loading fragment 136 | `FRAG_LOADING`; buffering enabled; new media source `streaming=true` |
| Playback recovered | 1301.572 s / 1295.760–1333.135 s | Seeking cleared; media moving |

Matched server log: `a11:11`, target 1301119 ms, seek dispatch at
23:02:10.349, supply-persistent at 23:02:18.850, resumed at
23:02:21.751: **11401 ms dispatch-to-presentation**. This reproduces the
original shape on the same deployed source and title.
The route-time element was 1272.349 s, so the sampled jump was +28.770 s.
The older baseline is not the immediate pre-click position.

### 2.4 The causal intervention resumes the same attachment

A second single +30 seek reproduced `IDLE`, buffering disabled, and missing
target coverage. At 23:04:21.331, called **only**
`PLAYER.hls.resumeBuffering()`. This was a temporary browser experiment,
not a source patch or deployment.

| Observation | Before intervention | 600 ms later |
|---|---|---|
| Session | `ee4772ab-a06d-4ba8-84ba-df4c80cb2b32` | Same |
| Requested position | 1448.386 s | 1448.386 s |
| Seeking | true | false |
| Loader | `IDLE`, buffering disabled | `FRAG_LOADING`, buffering enabled |
| Buffered media | 1398.440–1437.220 s | 1443.960–1465.082 s |
| ManagedMediaSource streaming | false | Still false |

The server recorded `seek_resumed` at 23:04:22.060. A subsequent snapshot
showed playback at 1505.033 s on that same session, with no pending seek.
Thus a new producer/session was unnecessary. The intervention deliberately
waited 2500 ms after invoking `nudge(30)`. That input-to-intervention delay
starts before coalescing and quality selection; the logged **1813 ms** starts
later, at execution, so it includes only the post-dispatch portion of the
delay. The 600 ms sample proves coverage and cleared seeking, not the exact
first-frame instant. **None of these figures is a patched performance
benchmark.** The experiment proves the loader state is causal and the target
can arrive promptly once fetching is enabled.

After testing, the episode was returned near the original position and left
paused at approximately 20:00. Bounded sampling was stopped; no persistent
player or server setting changed.

## 3. VOD root cause — the browser's streaming hint becomes a deadlock

### 3.1 The vendored dependency lacks the upstream seek repair

In the bundled [hls.js](../../crates/plurxd/src/web/hls.min.js) 1.6.16:

1. `preferManagedMediaSource` defaults to true.
2. The buffer controller handles `endstreaming` by calling `pauseBuffering()`.
3. Only `startstreaming` calls `resumeBuffering()`; the buffer controller
   does not also resume on a media `seeking` event.
4. The stream controller can tick an out-of-buffer seek while `IDLE`, but
   its idle work refuses segment loading while `buffering` is false.

Safari in the reproduced state does not supply the restart event needed by
that loop. Assigning `currentTime` changes the requested clock while old
media remains buffered; the browser waits for target data and hls.js waits
for permission to fetch it.

```text
old range buffered -> MMS endstreaming -> HLS buffering disabled
                                             |
viewer seeks outside old range               |
    -> browser waits for target bytes <------+
    -> missing startstreaming -> no target fetch
    -> generic 8 s stall -> reopen or exhausted
```

The primary upstream record describes the same cycle:
[WebKit change 319330](https://github.com/WebKit/WebKit/commit/a3af5da09b5c80d0d49bedc0528d3bc6947f4690)
identifies pending-seek time handling and missing buffer monitoring that
prevent the streaming restart. This is strong corroboration of the live
mechanism; we did not inspect the installed Safari binary or establish its
exact included WebKit revision.

[hls.js PR #7984](https://github.com/video-dev/hls.js/pull/7984) and
[release 1.6.19](https://github.com/video-dev/hls.js/releases/tag/v1.6.19)
provide the supported workaround. The complete change prefers standard MSE
when available, resumes MMS buffering on seeking, and avoids pausing it on
`endstreaming` while seeking. It also removes the added listener on detach.
One unconditional `resumeBuffering()` call in application code is not an
adequate substitute for that complete lifecycle handling.

These are two distinct repairs: a **default flip** to standard MSE avoids
this MMS failure on macOS Safari; the seeking-event workaround protects
platforms that still instantiate MMS. A macOS pass after the upgrade does
not exercise that workaround. §6.1 makes the choice explicit and §7.3
requires separate MSE and MMS physical evidence.

### 3.2 False presentation evidence hands the seek to the wrong timer

In [transport.js](../../crates/plurxd/src/web/player/transport.js),
`playbackProgressTick()` combines the element's current clock with the global
presented-frame counter. Its progress watch can still have the pre-dispatch
frame baseline: the seek is published before a coalescing/quality await,
then `markPlaybackControlSeekExecuted()` updates the seek's floor without
resetting the watch's baseline.

A concrete reproducer has watch frames 404, dispatch floor 405 and current
frames 405. Assigning the requested time makes the clock jump forward. The
watch sees both clock and frames greater than its older sample and enters
`localVodPresented=true`, although **no frame newer than dispatch exists**.
It then calls `localVodSeekCleanup()` and disables the fallback.

This is a normal forward-seek sequence, not merely a rare race. The progress
sampler runs every 500 ms. After intent publication, old media keeps playing
through coalescing/catalog work; at 24 fps, about 12 callbacks can occur
between ticks. Execution updates `frameFloor` without changing the watch
key or baseline. The next tick sees the assigned forward clock and those
pre-execution frames and cancels protection. The September 23 protection is
therefore routinely bypassed for playing forward local VOD seeks under this
ordering. The traces all show the roughly eight-second failure, but do not
measure a population-wide failure frequency or prove literally every seek.

A backward missing-target seek does not satisfy `clock > watch.clock` while
stuck. It should retain the 20 s protection under the same ordering. This
directional prediction needs a regression; it was not physically timed in
this investigation. Test publication → old frames over at least one tick →
execution → destination tick, as well as the minimal 404/405 snapshot.

The stricter `settlePlaybackControlSeek()` still refuses settlement. The
false flag therefore does **not** prove the control intent cleared or that
`seek_resumed` was emitted. It removes the seek's protected missing-media
state, clears wait/recovery observations, and allows the ordinary eight-second
stall path to take over. That distinction explains the frozen runtime.

A frame count check alone is insufficient as the repair: a late old-timeline
frame can advance the count after a seek. Presentation proof needs the
callback's `mediaTime`, attachment, presentation epoch, and dispatch floor
from the same observation. See `armHitchDetector()` in
[decode-tiers.js](../../crates/plurxd/src/web/player/decode-tiers.js), which
already supplies timestamped callbacks to the stricter settlement path.

### 3.3 The eight-second wait is application policy, not server necessity

[measurements.js](../../crates/plurxd/src/web/player/measurements.js) defines
`PERSISTENT_STALL_MS=8000`. The progress watch can call `persistentWait()`
directly after that interval; `beginWait()` is not required to have armed it
and normally returns while the element is seeking. `persistentWait()` then
asks control for a verdict, using the existing absolute 20 s defer ceiling.

The VOD-specific fallback separately uses
`HLS_STARTUP.seek_deadline_ms=20000` in
[playback-policy.js](../../crates/plurxd/src/web/playback-policy.js). Correcting
false presentation without fixing the loader can therefore turn an 8 s
failure into a **20 s wait**. It is necessary correctness work, not the
latency fix by itself.

The deployed recovery allowance rearms after 30 s of observed healthy
presentation. The two original failed seeks were too close together for
that rearm. The final prompt is thus a policy consequence of repeated
unresolved seeks, not evidence that the file is unplayable. Do not fix it by
blindly increasing the number of reopens.

### 3.4 The paused single-frame hazard is already deployed

`queuePlaybackFrame()` captures the presentation epoch at registration and
deliberately retains an armed callback across ordinary seeks. Execution
increments the epoch; `armHitchDetector()` then skips settlement for that
old-epoch callback even if its media time is the destination. A paused seek
may present only that one frame, leaving the intent pending until Play
produces another callback. This is a source-level defect in `4fa50b79e`, not
just a risk introduced by the proposed stricter proof. The progress watch is
inactive while paused, so it does not provide an alternative completion.
The required regression must fail on this source and settle without Play
after the repair; physical paused-frame behavior remains to be qualified.

## 4. Native rolling HLS — small skips repeatedly fall outside reach

The September 29 guard in `seekRoute()` prevents Safari from clamping an
unreachable local target back to the old position. Removing the guard would
restore wrong landings. The live comparison is decisive:

| Trace | Target relative to element | Buffer ahead | Seekable ahead | Result |
|---|---:|---:|---:|---|
| `a7:5` | +8.675 s | 21.759 s | 10.460 s | Local, 201 ms |
| `a7:6` | +8.586 s | 46.305 s | −1.698 s | Reopen, 6908 ms |
| `a5:3` | +28.639 s | 43.269 s | 1.468 s | Reopen, 6861 ms |

Targets are sampled after coalescing and decision work, so a +10 button can
appear as only +8.6 s relative to the later element sample.

The chain is also a startup problem. At successive post-reopen presses,
seekable lead from the new origin was only 9.099 s (`a5:3`), 10.892 s
(`a6:4`) and 13.894 s (`a7:5`), while buffered lead was 50.900, 37.175 and
25.193 s. The first two +30 presses reopened again; the +10 in `a7:5`
landed locally, then the next +10 in `a7:6` reopened. Thus small startup
reach can repeatedly force replacements. These traces do **not** establish
that every +10 fails or that a fixed 30-second exclusion applies. A
steady-only reserve change cannot improve the initial successor window.

The [prior native fixture](../evidence/safari-seek-native-2026-09-29.json)
measured a roughly three-target-duration native exclusion. At 16 s segment
targets that consumes 48 s of published lead. Current production
[publication policy](../../crates/plurxd/src/transcode/rolling/publication.rs)
still uses a conservative 48 s startup/publication runway; the alternate
32 s startup is test-only. The live initial writer snapshots also report
`bootstrap_ms=48000`.

The exact per-click published edge/reload state is absent from the native
trace (`published_ms=null`). Do not claim the incident measured exactly 48 s
of holdback on every click. The established cause is the measured
buffered/seekable mismatch and resulting costly replacement; the prior
fixture and current publication policy explain why it recurs.

The previous 108 s steady-reserve candidate was **withdrawn**, not shipped:
three integrated publication/flow tests failed, including an observed
39.6 s producer overshoot. Its arithmetic needed 108 s for +10 at 1×;
+30 at 1× needed at least 128 s before rounding, against the prior 124 s
reserve ceiling. +10 at 2× needed 158 s. These are the prior plan's budgeted
bounds, not new performance guarantees. See
[the M4 disposition](SAFARI-SEEK-IMPLEMENTATION.md#11-execution-record--update-in-the-implementation-commits).

Reopen also pays capability/recipe selection, admission, producer startup,
manifest readiness and decoder attachment. Original logs show initial
publication alone taking roughly 1.8–3.8 s in several native attempts. A
server already producing bytes is not equivalent to a native timeline that
can accept the requested position.

## 5. Why this survived earlier repairs

| Change / boundary | What it did | What remained |
|---|---|---|
| Bundled hls.js 1.6.16 | Retained MMS-first selection and original streaming-event handling. | No upstream 1.6.19 repair for the identified Safari 27 regression family; machine upgrade date remains uncollected. |
| September 23 VOD repair, `528316c24` | Intended to protect missing-media seeks from generic 8 s recovery. | Forward seeks with intervening old frames routinely cancel that protection before any target frame; the initiating request/no-request distinction was left unmeasured. |
| September 29 native guard, `8f763feca` | Reopened targets Safari could not reach locally. | Correctness improved while reopen latency remained; the greater reserve proposal failed qualification. |
| Subsequent recovery repairs | Fixed lifecycle/rearm/terminal ownership cases. | Neither the dependency deadlock nor the false target-frame proof was removed. |
| Tests and acceptance | Exercised route/timer contracts and synthetic native clamp. | Mocked VOD tests allowed a clock-only target advance without real frames; real-server Safari timing acceptance remained open. |

The [1.6.19 release](https://github.com/video-dev/hls.js/releases/tag/v1.6.19)
identifies the macOS/iOS 27 regression family; WebKit names its introducing
change as `315668@main`. This makes the Safari 27 upgrade the likely trigger
for the newly visible MMS deadlock, rather than a newly introduced October 2
application change. The exact local upgrade date and binary mapping remain
unverified. Safari 26 should not reproduce this particular regression; that
is a control-test prediction, not a measured result. Chrome's standard-MSE
path does not enter the MMS event cycle. Later Safari builds may contain
WebKit's fix, so every MMS result must record the full browser/OS build.

Pure buffer-drain behavior on MMS remains unmeasured. The earlier baseline
and dispatch snapshot keep the same next-load position while forward
coverage decreases; they do not prove ordinary playback would eventually
stall. Add five minutes with no seeks on the MMS platform, because a
seek-only wake does not prove normal refill works.

## 6. Proposed repair — each transition has one existing owner

### 6.1 Repair the dependency at its owning layer

Update the checked-in full hls.js bundle from 1.6.16 to a pinned, verified
1.6.x release containing #7984; **1.6.19 is the minimum identified fixed
version**, not a claim that it is the newest suitable release. Prefer the
upstream build to a hand edit of minified code. Record its upstream tag,
commit, checksum and license provenance. Review intervening changes and
verify the worker path, custom loader API, quotas, media-error recovery,
prepared replacements and subtitles with that exact bundle.

The required behavior is all of: standard MSE preference when supported,
MMS fallback where necessary, resume buffering on the attached media's seek,
ignore MMS endstreaming during that seek, and listener removal on detach.
Do not add a second application timer or permanently force buffering on.
Preserve codec/HDR/DV/AirPlay capability selection; constructor availability
alone does not prove a format is supported. Native HLS remains native when
that transport is required.

**Proposed product decision:** deliberately accept `preferManagedMediaSource:
false`, rather than pinning MMS-first. Apply one explicit preference to main
playback, prepared replacement and Live TV Hls construction. On macOS Safari
with standard MSE this removes the MMS event cycle; on an MMS-only device
the complete upstream seeking workaround still applies. Record the actual
selected class rather than infer it from the device name.

Align `mseCanTake()` with that same selection rule; it currently probes
`ManagedMediaSource || MediaSource`. The
[upstream selector](https://github.com/video-dev/hls.js/blob/v1.6.19/src/utils/mediasource-helper.ts)
uses the explicit preference, with MMS fallback when standard MSE is absent.
Do not use static `Hls.getMediaSource()` as a proxy for configured selection:
the [static method](https://github.com/video-dev/hls.js/blob/v1.6.19/src/hls.ts)
calls the helper without an argument, whose default remains MMS-first.
Tests must give the two classes different codec support and verify that the
probe and all three constructors agree, including MMS-only fallback.

This choice needs product qualification: MMS memory-pressure eviction and
standard-MSE quota handling differ; `decode-margin.js`'s 144 MB budget is
Chrome-derived, not a measured Safari bound. Exercise high-bitrate Safari
MSE append/eviction behavior and memory pressure. Check AirPlay control and
actual remote playback where available: the library changes
`disableRemotePlayback` on its MMS path. Record power/CPU observations for
the new default without claiming an unmeasured efficiency win.

The independently diffed 1.6.16/1.6.19 npm source has four further affected
areas. Preserve this inventory when selecting the final pinned release:

| Dependency change | Required coverage |
|---|---|
| `mp4-remuxer.ts`: recompute a lower initial timestamp base at zero offset, bind initial PTS to the lowest DTS across tracks, and alter rollover threshold from offset-plus-timescale to offset. | The server's HLS transcode path emits MPEG-TS, which hls.js transmuxes. Verify transcode A/V offset and ±seeks on Chrome and Safari MSE, including nonzero starts, replacement/discontinuity and reordered video. Copy-HLS VOD alone is insufficient. |
| `base-playlist-controller.ts`: respect scheduled reload time. | Chrome Live TV start and continued playlist refresh; rolling hls.js playback and seeking. |
| `level-helper.ts`: LL-HLS part indexing and fragment-hint guard. | Retain playlist/part coverage; record whether a tested stream uses LL-HLS. |
| `base-stream-controller.ts`: key-loading refactor. | Confirm the tested recipes are unencrypted; do not claim encrypted-stream qualification from them. |

Source provenance: [1.6.16 npm source](https://registry.npmjs.org/hls.js/-/hls.js-1.6.16.tgz),
[1.6.19 npm source](https://registry.npmjs.org/hls.js/-/hls.js-1.6.19.tgz),
and [the revised remuxer](https://github.com/video-dev/hls.js/blob/v1.6.19/src/remux/mp4-remuxer.ts).

**Acceptance:** macOS Safari ready-target seeks must pass on confirmed MSE;
MMS-only Safari must independently pass seeks and ordinary buffer refill.
Both stay on the same session without a generic stall or console wake.
The MMS row is mandatory; unavailable hardware leaves qualification open.

### 6.2 Make target presentation one authoritative observation

Unify VOD completion and telemetry with the existing strict settlement
owner. Retire the weak `localVodPresented` branch or derive it only from a
validated target observation. The proposed observation carries:

```text
attachment identity + intent sequence + presentation epoch
+ presented frame sequence > dispatch floor
+ callback mediaTime converted to film time inside target landing window
```

On dispatch, reset the progress observation epoch/baseline atomically with
the seek's frame floor. With frame callbacks available, do not substitute
`video.currentTime` for the callback's time. Late callbacks from the prior
epoch or prior attachment cannot complete, cancel or credit the current
seek. Playback after a newly superseding seek belongs only to that seek.

The subscription lifecycle must change with the epoch. The current
`queuePlaybackFrame()` can retain an armed pre-seek callback; rejecting its
old epoch may discard the **only** destination frame of a paused seek.
Before assigning the new media time, atomically advance the epoch, cancel
the old subscription and arm the existing frame observer for the new intent
when registration is supported. Retain the current post-load/seek readiness
rearming for platforms that strand a pre-load registration, without
discarding a valid armed observer at `seeked`. Keep one subscription owner;
a callback already queued before cancellation remains generation-fenced.
Retain a matching target-frame observation if it precedes `seeked`, then
reconcile it when seeking clears. Neither callback order may require a
second frame or an implicit Play. Prove both orders on paused real media.

For playing audio-only media, use successive advancing audio-clock samples
at the target after seeking clears. For video without frame callbacks, retain a
separate conservative fallback requiring seek completion, target coverage
and advancing decoded-frame/clock evidence; record its weaker provenance.
Paused audio and paused video without presentation evidence must instead
report a distinct completed positioning outcome after seek completion and
target coverage, with current element time converted to film time inside the
existing landing tolerance for the current attachment and intent. A clamped
seek cannot count as positioned. They cannot require an advancing clock or
claim a rendered frame. Preserve that weaker evidence in telemetry, and require fresh
presentation proof when playback resumes. Unavailable evidence never means
presentation succeeded.

Buffer coverage ends the missing-delivery stage; it does not itself prove
presentation. Use the existing seek/recovery owner for the next stage, with
one bounded deadline and cancellation rules. Keep the ordinary 8 s sustained
playback-stall rule until a separately justified change needs it; it must
not delay a known corrective seek transition.

### 6.3 Use active control to explain and execute seek progress

The existing protocol already carries `seek_target_ms`, accepted sequence,
render state and target-anchored delivery readiness. Reuse it. A local loader
wake on a browser seek is immediate and does not need a network round trip.
Do not block the confirmed loader/frame repairs on a protocol redesign.

For the broader repair, make the existing client seek owner act on these
stage transitions and report them through the existing reporter/telemetry:

| Stage / evidence | Owning action |
|---|---|
| New committed seek | Publish the executed target promptly through the reporter after media assignment; preserve its existing coalescing and 250 ms exchange floor. |
| HLS loader suspended while target data is absent | Dependency's seek handler resumes target loading immediately, including the bounded data needed to position a paused seek. This does not call Play; retired attachments remain fenced. |
| Server ready, target request not dispatched | Client loader owns dispatch; server producer hold cannot justify idling the loader. |
| Target request is in flight | Existing request loader owns retry/abort; report request age and typed result. Do not create a duplicate GET from a polling callback. |
| Server is materializing an admitted target | Server's existing materialization owner reports progress/failure and a bounded revisit; client obeys that response within the same absolute seek attempt. |
| Target appended, no target frame | Presentation owner diagnoses using timestamped frame evidence; production readiness is not presentation success. |
| Target presents / newer seek / close | Complete, supersede or retire the current intent, cancel its owned work and ignore stale callbacks. |
| Viewer pauses during a pending seek | Preserve the chosen destination and finish its bounded positioning work while retaining paused intent; cancel automatic play/reopen work that contradicts that intent. Pause alone does not discard the current target. |

Before a server observation influences that state machine, validate its
session, owner epoch, accepted control/intent sequence, target anchor and
freshness. Convert all ranges to the same film-time coordinates. Readiness
for an earlier target cannot cancel delivery or complete a newer seek.

Paused ready-target positioning must not become an unbounded refill request.
VOD's current control owner already accepts non-End target demand, including
Hold. Native rolling production has different Hold rules: any missing-target
successor still follows existing admission, scratch and bounded production
rules. Preserve those limits and report a typed refusal/deferred positioning
outcome if they prevent completion; do not bypass Hold to satisfy the
paused-frame test. A ready, admitted target must finish while remaining paused.

Add optional diagnostic observations for loader buffering state, source
class, requested target/fragment, dispatch/first-byte/append timestamps and
last target-frame time. These describe existing work; they are not commands
to a second scheduler. Any wire additions need capability/version-compatible
handling and the protocol's existing size/rate/identity validation.

Current `playbackControlSnapshot()` reports decoder `ready` from
`p.started`, even while a new target has no bytes, and prioritizes `seeking`
over `stalled`. Preserve `seeking` as intent, but distinguish last attachment
readiness from target progress. Do not relabel a seek as generic `stalled`
and thereby drop `seek_target_ms`.

Inventory and reconcile `beginWait`, `playbackProgressTick`, the local seek
fallback, startup deadline and persistent recovery before editing them.
Exactly one owner may reopen/stop a given seek. Positive typed failures or
known missing loader dispatch should trigger the corresponding action
immediately; absolute ceilings remain safeguards for genuinely unknown or
non-progressing work. Increasing 8 s to 20 s is not this repair.

### 6.4 Remove avoidable work before the seek is executed

`seekTo()` currently awaits 100 ms coalescing and, in Auto quality,
`naturalBoundaryQualityCandidate()`, which can wait up to 1500 ms for a fresh
capability decision before selecting a local route. The original logs show
those decision requests around the seeks. This is additional overhead,
not the cause of the eight-second deadlock.

The boundary can also select a different candidate and call
`requestPlaybackMediaChange()`, turning an otherwise local seek into a full
replacement. Record catalog wait, candidate-change count and reason for
each replacement in physical runs. Separate Auto-quality replacement from
reachability replacement and recovery; an aggregate seek time hides them.

Use a still-valid negotiated candidate catalog at the seek boundary. If it
is stale, execute the seek on the current valid recipe and refresh outside
the critical path; apply any later automatic quality change only at a valid
subsequent boundary, fenced to current selection, display and attachment.
An explicit viewer quality change still owns its separate transaction.
Do not silently discard selection, subtitle, audio offset or HDR constraints.

Start the end-to-end metric at committed viewer input. Retain separate
coalescing/catalog/route, target request, append and presented-frame times.
Current `seek_resumed.ms` starts at dispatch and excludes pre-route work;
it cannot be advertised as click-to-picture latency.

### 6.5 Native rolling needs a qualified coverage or replacement improvement

Keep `seekable` admission and strict landing checks. Prefer an already-ready
immutable VOD recipe through the existing compatible selection path; never
force a transcode solely to claim fast seeks or switch transport invisibly.

For unavoidable native rolling, address successor startup before steady
coverage. The proposed server work has two parts:

1. **An efficient exact-target successor with initial reach.** Preserve
   newest-intent cancellation, request identity and retained recipe facts.
   Measure and remove repeated catalog/admission/initial-publication work
   where existing ownership permits reuse. Do not manufacture a new
   producer for each intermediate click or reuse stale authority. The
   successor must attach at the requested film time, not merely return 200.
   Its initial `seekable` end should already admit the next +30 target;
   with the prior measured 48 s exclusion, the provisional served-edge
   requirement is at least landing +30+48 s, plus measured movement/reload
   allowances and segment rounding. This is a resource/admission hypothesis
   to prove, not a new hard-coded constant. Measure served and seekable edges
   together at attachment and each subsequent press.
2. **One allowance for publication and production.** Derive the steady
   coverage requirement from measured native seekable lag, maximum segment
   duration, intended +10/+30 reach, playback rate, reload delay and producer
   refill time. Feed the same incremental allowance into publication and the
   producer flow controller across startup-to-steady cutover. Include
   in-flight segment rounding in scratch admission. Merely changing the
   runway constant repeats the failed M4 design.

Investigate reuse of already-produced compatible scratch before new
production. Browser coverage is evidence of past delivery, not proof that
the server still retains reusable files. Verify generation, recipe, timeline,
retention and authority first. At `a7:6`, the old buffer ended at 3350.942 s,
only 37.719 s beyond target 3313.223 s: this trace alone does **not** prove
enough retained runway for the proposed 78 s minimum. If ready coverage and
admission cannot meet the goal, demonstrate the bounded fast-successor
alternative or report this acceptance row open; do not delay first picture
without measuring that tradeoff or exceed scratch bounds.

A +30 promise does not fit the previously qualified budget arithmetic.
Implementation must either prove a larger admitted scratch/coverage bound
under the existing resource controller, reduce applicable segment/reload
costs, or meet latency via the successor path. Copy HLS cannot shorten a
long source GOP by changing a target-duration label. The decision is gated
by measured resource and presentation evidence, not an optional feature
switch. This document does not claim the required new budget is solved.

Capture served playlist and browser `seekable` together on the actual title,
at startup and at the low point of a full refill cycle. Existing native traces
lack the publication fields needed to choose a safe new bound. Keep that
measurement inside this repair's acceptance, rather than shipping an
unverified constant and marking the whole problem closed.

## 7. Validation — prove the mechanism and the user outcome

### 7.1 Diagnostic replay already run

```bash
# Use an extraction of the exact deployed commit, without .git or credentials.
node docs/evidence/web-seek-mechanism-replay.cjs /tmp/plurx-seek-rca-src
```

Observed: false VOD presentation with frames equal to dispatch floor;
fallback cancelled while still seeking; bundled 1.6.16 pauses buffering on
endstreaming during a seek and resumes on startstreaming. The browser
intervention in §2.4 supplies the real-media evidence this stub cannot.
The extended replay also executes the deployed dispatch function after old
frames advance over a sampling tick: forward cancels protection without a
new frame; backward retains it. This confirms the sequence/asymmetry, not
an end-to-end timer duration or a measured prevalence.

### 7.2 Regressions required from the implementation

| Boundary | Required counterexample / assertion |
|---|---|
| Bundled MMS lifecycle | Real bundled controller: endstreaming → seeking → late endstreaming resumes fetching and does not repause; detach/destroy removes listeners. With separate main/spare Hls instances, retiring the old attachment cannot remove the successor's seek listener. |
| MediaSource selection | Explicit MSE-first preference, MMS-only fallback and deliberately different codec support: the capability probe and main/prepared/Live TV constructors select the same class. Static `Hls.getMediaSource()` must not silently reintroduce MMS-first probing. |
| False presentation | Publish intent → keep old media playing across a 500 ms tick → execute with updated frame floor → assigned-target tick: no completion, fallback cancellation or false recovery health. Retain the minimal 404/405 case too. |
| Forward/backward asymmetry | On incident source the sequence above cancels forward protection but retains it for a backward missing-target seek. After repair both wait for real target evidence and promptly fetch; neither relies on an 8 s/20 s timeout as its normal path. |
| Late old frame | Callback 406 from old media time or epoch cannot complete the new target. A callback with current attachment/epoch and target media time can. |
| Supersession | Five +30 inputs during pending delivery leave only the final intended destination; stale GETs, callbacks and control replies cannot win. |
| Paused seek | A pre-seek callback is outstanding; exactly one target frame arrives, before or after `seeked`: current intent settles without a second frame or Play. An old queued callback cannot win. Explicit Pause survives a seek and later replies. Audio/no-frame-callback positioning is labelled separately; Resume requires fresh presentation proof and cannot revive a retired attachment. |
| Missing versus ready target | Ready target fetch begins promptly; real admitted materialization follows typed progress/retry and one absolute bound; permanent media failure terminates once with its cause. |
| Stale control readiness | Earlier target/session/epoch/sequence readiness cannot complete or cancel the current seek; fresh target-anchored ranges use the same film-time origin. |
| Paused resource ownership | Ready admitted target positioning finishes while paused; unavailable native targets retain Hold/admission/scratch bounds and produce an explicit bounded outcome rather than unrestricted production. |
| Timer race | Seek-specific and generic clocks cannot both recover or stop the same intent; correcting frame evidence does not merely restore a 20 s deadlock. |
| Auto catalog | Healthy local seek executes without awaiting a network catalog refresh; stale decision cannot change current selection. |
| Native rolling budget | Startup/steady transition, low-water refill, segment rounding, pause/resume, 1×/2×, concurrent streams and scratch pressure never overshoot the admitted allowance. |
| Native reach | A buffered but unreachable target still cannot take the local route; an actually reachable target lands correctly. |
| Native successor startup | Five spaced +30 presses immediately following successor landings: initial reach admits the next target or a qualified bounded successor meets latency; measure each attachment's served/seekable lead and retained-scratch reuse. Separately test five coalesced rapid presses. |

Extend the existing
[seek control](../../tests/playback/seek-control.test.js),
[web control](../../tests/playback/web-control.test.js), and
[seek telemetry](../../tests/web/seek-telemetry.test.js) harnesses. Rewrite the
test that credits clock-only VOD progress without frame evidence. Add a
bundled-library regression rather than stubbing out the very MMS state
machine being repaired.

### 7.3 Physical acceptance is required for both transports

Proposed acceptance targets below are requirements to prove, not measured
results or a claim about arbitrary unavailable media:

- At least 20 single seeks per ready-media route, mixing ±10 and ±30, with
  separate buffered and out-of-buffer cases. Report median, p95, maximum,
  landed-position error, session changes and failures. Count cancelled
  intents separately; do not erase abandoned seeks from the report.
- Ready VOD target: p95 committed-input-to-target-frame **under 1 s**, no
  recovery reopen, no eight-second plateau, and no terminal freeze.
- Native rolling within reported reach: the same under-1 s target. For
  ordinary +10/+30 outside reach, demonstrate either maintained reach or a
  successor p95 **under 2 s** on the reference fleet. If not achieved, report
  native seek latency still open rather than calling VOD repair complete
  for both transports.
- Five rapid +30 presses, backward seek, paused seek, and a seek during
  replacement all end at the winning target with correct playback intent.
  Also test five spaced native +30 presses, each within 3–8 s of the previous
  landing, so coalescing cannot hide a reopen chain. Record initial seekable
  lead and per-seek replacement reason, including Auto candidate changes.
- Ten minutes of native steady playback covering full refill cycles, with
  simultaneous served-edge/seekable observations and scratch/producer peak
  measurements. Repeat a constrained-resource case with an honest refusal.
- Required browser rows: macOS Safari 27.0.1 against media1/reference files
  on confirmed MSE; iPhone Safari on confirmed MMS; Chrome hls.js. Record
  full browser/OS build, bundle checksum and actual source class for each.
  An unavailable iPhone leaves qualification open; §7.5 supplies its handoff.
- On MMS, play for at least five minutes without seeking and verify repeated
  buffer drain/refill, then perform buffered/out-of-buffer ±10/±30 and paused
  seeks. A forced-MMS Mac test supplements, but cannot replace, this row.
- Chrome and Safari MSE: MPEG-TS transcode A/V sync and seek landing before
  and after nonzero starts/replacements. Chrome Live TV must start and keep
  refreshing its playlist. Report numerical A/V offset against the same-file
  baseline; no new persistent drift. Include Safari MSE high-bitrate quota,
  memory-pressure, AirPlay and power observations described in §6.1.
- Verify preserved codec, resolution, dynamic range, selected audio and
  subtitles. A downgrade is not a seek-performance success.

### 7.4 Delivery sequence and scope

First deliver the loader update, explicit source-class/probe alignment and
frame-proof correction with focused tests and required MSE/MMS, transcode
and Live TV evidence. Next complete existing-owner stage wiring and catalog
work. For native HLS, address successor startup before measured steady
coverage. The overall incident remains
open until both transports satisfy them. Add diagnostics/control stage
wiring needed to prove those rows through existing owners, not as a new
monitoring service.

If this becomes multiple implementation tasks, use one `effort/web-seek`
branch and task PRs into it per [AGENTS.md](../../AGENTS.md). Before any Rust
edit, establish the pinned 1.97.1 compiler loop in
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md); this investigation
changed no Rust and makes no compiler claim. Run appropriate focused
regressions before pushing; use `fix(` or `perf(` subjects and preserve
`Regression-Test:` fields. Qualify the final integrated tree on current main.

No production fix or deployment is included in this documentation request.

### 7.5 Required iPhone/MMS qualification handoff

Use this prompt when the implementation candidate is available and an
operator or agent has an iPhone. Hardware absence does not waive the row:

> Qualify the exact web-seek implementation candidate on a physical iPhone
> using Safari and media1. Record candidate revision, hls.js version/checksum,
> iPhone model, full iOS/Safari build, actual MediaSource class, recipe and
> codec facts. Confirm the app is using ManagedMediaSource. Play a ready VOD
> for five minutes without seeks and capture multiple buffer drain/refill
> cycles; report any missing startstreaming or empty-buffer stall. Then run
> at least 20 ±10/±30 seeks covering buffered and out-of-buffer targets, a
> paused sole-frame seek, five rapid +30 inputs and a prepared replacement.
> Capture input, execution, request, append and target-frame times plus
> session identity and buffering state. Require truthful target settlement,
> preserved pause/quality, no generic recovery reopen, and the §7.3 VOD
> latency target. Verify retiring the old Hls instance leaves the successor
> responsive. Report failures and unavailable evidence explicitly. Do not
> change global settings or claim a patched-WebKit pass proves the workaround
> was necessary; retain the bundled-controller regression for that mechanism.

## 8. Adversarial review — dispositions

An independent adversarial agent (`seek_adversary`) reviewed the deployed
source, retained evidence, diagnostic replay, upstream records and full
draft. The first disposition was **request changes**. Its findings and the
subsequent corrections are:

| Finding | Risk | Addressed disposition |
|---|---|---|
| R1 · P1 · Paused sole-frame callback lifecycle | Rejecting an old subscription's epoch can discard the only destination frame, leaving a paused seek pending forever. Clock-advance fallback is also impossible while paused. | §6.2 now defines cancel/rearm ordering, generation fencing, retention of a target frame arriving before `seeked`, and distinct paused positioning evidence. Positioning still requires current attachment/intent and film time within landing tolerance, so a clamp cannot count. §7.2 requires the sole-frame race in both event orders and separately labelled audio/no-callback behavior. |
| R2 · P2 · Intervention timeline | Saying an 1813 ms dispatch metric includes the entire 2500 ms input delay is inconsistent and overstates timing precision. | §2.4 separates input from later execution, explains only the post-dispatch portion is included, and distinguishes the 600 ms state sample from first-frame timing. None is presented as a patched benchmark. |
| R3 · P2 · Evidence precision | Incorrect bootstrap log field and a nonexistent M4 anchor weaken reproducibility. | §4 uses the observed `bootstrap_ms=48000` and links to the actual execution record. §3.1 now cites the immutable WebKit commit and retains the installed-binary attribution limit. |
| R4 · P1 · Pause and target-fill ownership | Treating Pause as seek cancellation or fencing all target loading can contradict paused landing; bypassing native Hold can create unbounded production. | §6.3 separates pause from supersession, permits bounded current-target positioning without Play, and retains native admission/Hold/scratch constraints with explicit bounded outcomes. §7.2 adds resource-ownership coverage. |
| Additional control invariant | Readiness for an old target can otherwise influence a newer seek. | §6.3 requires fresh matching session, owner epoch, accepted sequence and target anchor in common film coordinates; §7.2 adds the stale-readiness counterexample. |

The reviewer verified these revisions and accepted the RCA/proposed design,
subject to the implementation and physical qualification still required in
§7. This is **not** approval of a deployed fix or proof that the native
coverage budget is solved. The review supported the causal VOD finding and
the separate native diagnosis, while requiring original-incident evidence
to remain distinct from the controlled reproduction.

Documentation validation passed: all four docs-index contracts (also run
with these new deliverables included), all new document file links, evidence
JSON parsing and the deployed-source mechanism replay. Existing unrelated
working-tree documents and index additions were preserved. No production
source was changed.

## 9. Opus review — verified dispositions

[Opus's supplied review](../reviews/WEB-SEEK-LATENCY-OPUS-REVIEW.md) approved
the diagnosis and approved the repair plan with changes, including one P1.
This revision addresses every finding. The author independently inspected
the incident source, downloaded and diffed the two npm source releases,
checked the upstream release/selector, recalculated the native trace leads,
and extended/reran the mechanism replay. Opus has not re-reviewed this
revision; its initial conditional verdict is preserved in the review record.

| Finding | Disposition in this revision |
|---|---|
| B1 · P1 · Upgrade changes macOS source class | Accepted. §3.1 separates MSE avoidance from the MMS workaround. §6.1 explicitly proposes MSE-first across all Hls constructors, matching capability probing, static-helper trap coverage and quota/AirPlay/power qualification. §7.3 makes physical iPhone MMS required; §7.5 supplies the requested handoff prompt. |
| B2 · P2 · Onset and ordinary buffer drain | Accepted with attribution limits. §5 identifies the Safari 27 regression family and likely upgrade trigger, while keeping the local update date/binary mapping unverified. Safari 26 is a predicted control result, not a recorded pass. MMS no-seek refill and full browser-build recording are required. |
| B3 · P2 · Forward false presentation is common | Accepted mechanism; narrowed universal wording. §3.2 explains normal tick/old-frame/dispatch ordering and backward asymmetry. The extended deployed-source replay demonstrates both. The evidence supports routine bypass under this ordering, not a measured “every seek” rate. |
| B4 · P2 · Intervening dependency risk | Accepted. §6.1 names remux timestamp/rollover, playlist reload, part/hint and key-loader changes from the source diff. §7.3 requires TS-transcode A/V/seek checks on Chrome and Safari MSE plus Chrome Live TV refresh. |
| B5 · P2 · Paused callback defect predates repair | Accepted. §3.4 explicitly identifies the deployed epoch mismatch. §6.2 and §7.2 require one-frame paused completion without Play and a regression that fails against incident source. Physical reproduction is not claimed. |
| B6 · P2 · Successor startup causes chains | Accepted priority and acceptance; qualified the extrapolation. §4 records initial leads and the successful local +10 counterexample. §6.5 now puts successor initial reach first and measures the next +30 at attach. Old browser bytes do not prove retained scratch or 78 s of reusable runway; the cited trace contains only 37.719 s beyond the new target. Both admission and measured startup latency must hold. |
| N1/N2 · Jump size and snapshot age | Accepted. §2 labels the +148.117 s terminal jump separately from small-seek failures and gives the baseline's 15.695 s age plus route-time element position. |
| N3 · Auto can itself reopen | Accepted. §6.4 and §7.3 require candidate-change frequency, latency and replacement reasons so Auto changes cannot be misclassified as native reach or recovery. |
| N4 · Public host naming | Accepted. This RCA and retained review use `media1`; evidence contains no host. Existing title names are retained as reproducible media identifiers for this private investigation. |
| N5 · Per-attachment MMS listener | Accepted. §7.2 explicitly covers separate incumbent/spare Hls instances and retiring one without removing the other's listener. |

No implementation, browser upgrade or deployment occurred while addressing
this review. Required physical results remain open. The existing constraints
remain: no new watchdog, no timeout increase as the fix, and no incident
closure until both VOD and native rolling pass their acceptance rows.
