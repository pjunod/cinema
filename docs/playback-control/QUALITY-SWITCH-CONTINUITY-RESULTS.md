# Quality switch continuity — measured results

**Status:** instrumented, **not yet run** — every result row below is empty on
purpose · **Measures:** the bar in
[QUALITY-SWITCH-CONTINUITY-BUILD.md](QUALITY-SWITCH-CONTINUITY-BUILD.md) §1,
instrumented by its §8 · **Instruments:** built 2026-09-16 on
`agent/m3-measure` · **Fills this in:** a device run on the fleet build, per
§3's hardware lane — the numbers are not inferable from the simulator or the
unit suite and are not written here until a device has produced them.

This document is the acceptance record for the prepared quality handoff. It is
separate from the build plan for one reason: the plan says what to build and is
finished when it is built, and this says what the thing that was built did to a
viewer's picture and sound, which only hardware can say.

## 1. The bar

Twenty consecutive viewer-directed quality changes per platform, on a realistic
runway, on the fleet build:

| Property | Bar | Judged? |
|---|---|---|
| Dropped frames in the two seconds around the commit | **zero** | yes — a pass/fail |
| Audible seam at the commit | **none** | yes — a pass/fail |
| Tap to the new quality on screen | — | **no**, reported only |

A directed change must also end in exactly one of two ways — a committed
successor, or one ordinary reopen at the position the viewer has actually
reached (§1 of the build plan). A run in which any change ends in neither or
both fails regardless of the frame and audio numbers.

## 2. What each platform measures, and how

| Measure | Web | Apple | Android |
|---|---|---|---|
| Frames presented, ±2 s around commit | `getVideoPlaybackQuality().droppedVideoFrames` on the predecessor element for the two seconds before the commit plus the successor element for the two seconds after, sampled on the existing 500 ms progress tick | `AVPlayerItemAccessLog` `numberOfDroppedVideoFrames`, same two-sided window across `replaceCurrentItem`, sampled on the existing 2 s status poll plus one synchronous read either side of the swap | `AnalyticsListener.onDroppedVideoFrames` on both pipelines, keyed by pipeline so a second change is not measured against the first one's samples |
| Audio discontinuity | **not measured** — see §4 | `AVPlayerItemAccessLog` `numberOfStalls` over the same ±2 s window | `AnalyticsListener.onAudioUnderrun` events inside the ±2 s window |
| Tap → new quality on screen | `directedChange.tappedAt` to the successor's first frame | the viewer's tap in wall time to `first_frame_unix_ms` | `DirectedChange` tap time to `onRenderedFirstFrame` |

All three report through a `PREPARED SWITCH` group in the shared playback-info
ledger (`tests/playback/playback-info-fields.json`, Debug mode) and through the
advisory rows on each client's Developer surface. **Nothing is gated on any of
it**: every row is an observation, an unmeasured window says
`Not measured` rather than reporting a zero, and no reading can refuse a
quality change, disable the prepared handoff, or alter a capability.

The wording is identical on all three clients, so a fleet run can be read
without a per-platform key:

```
Frames at the switch   0 dropped · ±2.0 s · 4.0 s of 4.0 s sampled
Audio at the switch    0 access-log stalls · ±2.0 s · 4.0 s of 4.0 s sampled
Tap to new quality     812 ms
```

The coverage clause is load bearing. A sampler that observed only half the
window reports `2.0 s of 4.0 s sampled`, and a zero over half a window is not
the same claim as a zero over all of it. A window with fewer than two readings
on both sides reports `Not measured` and **does not count toward the twenty**.

## 3. Results — not yet run

Twenty consecutive directed changes per platform. Fill one row per platform
from one run; do not merge two runs into one row.

| Platform | Fleet build | Device | Changes | Committed | Reopened once | Neither or both | Max dropped frames (±2 s) | Windows fully sampled | Audible seam | Tap → new quality (min / median / max) | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Web | — | — | — | — | — | — | — | — | — | — | **not yet run** |
| Apple (tvOS) | — | — | — | — | — | — | — | — | — | — | **not yet run** |
| Apple (iOS) | — | — | — | — | — | — | — | — | — | — | **not yet run** |
| Android (TV) | — | — | — | — | — | — | — | — | — | — | **not yet run** |
| Android (phone) | — | — | — | — | — | — | — | — | — | — | **not yet run** |

Per-change detail, if a platform fails: one row per change, so a single bad
commit is not averaged away.

| Platform | # | Rung | Outcome | Dropped frames | Window sampled | Audio | Tap → new quality |
|---|---|---|---|---|---|---|---|
| — | — | — | — | — | — | — | — |

### How to run it

1. Deploy the fleet build to every node and install the client builds on the
   physical devices (`docs/PUBLISHING.md`).
2. On each device, open Settings → Developer and confirm the prepared handoff
   is on, and that the enable section's conditions read as expected. **The
   conditions are advisory; do not treat an unmet one as a reason to stop.**
3. Play a title with a realistic runway. Open the playback-info panel in Debug
   mode.
4. Change quality from the quality menu twenty times in a row, waiting for the
   picture to settle between changes. Record the three `PREPARED SWITCH` rows
   after each change, and whether the stream changed once or twice.
5. A `Not measured` row is not a pass. Repeat that change.

## 4. What is deliberately not measured, and what it would cost

**The web audible seam.** §8 of the build plan names an `AnalyserNode` on a
`MediaElementAudioSourceNode`. The detector itself is built and unit-tested —
`preparedSwitchSilenceRunMs` and `preparedSwitchSeam` in
`crates/plurxd/src/web/playback-control.js` scan a time-domain buffer for a
silent run and apply the 20 ms threshold inside 300 ms of the swap — but it is
**not wired to a live element**, and the ledger row says so rather than
reporting a zero.

The reason is that `createMediaElementSource` cannot be undone. It moves the
element's audio into the `AudioContext` graph for the rest of that element's
life, and the sound then reaches the speakers only while that graph is running
and connected. A context suspended by the autoplay policy, a cross-origin
tainted response, or a graph torn down with the player modal each end in
silence on a path the viewer is listening to. An instrument that can silence
the thing it is measuring does not belong on the audible path, so the
conditions are stated on the Developer tab as unmet and the probe is not built.
Cost of taking it anyway: a permanent capture of every playback element, and a
new class of "no sound at all" that no existing test would catch.

**Apple's `MTAudioProcessingTap` gap detector.** §8 offers it as a fallback if
the access log is silent. Installing one means setting the item's `audioMix`,
which puts a real-time callback of ours inside the audible path of every
session; a callback that overruns is a silence the viewer hears. The access
log's stall count is free and ships; the tap does not. The Developer tab states
the condition and says it is not met.

**Apple's `AVPlayerItemVideoOutput` frame count.** §8 names it, and the commit
path already installs one for first-frame proof — but counting every frame
across the window means polling `copyPixelBuffer` for two seconds after the
commit, which is per-frame pixel-buffer copies and a new poll loop on the
commit path. The access log's own `numberOfDroppedVideoFrames` is the same
quantity for free, so that is what is reported. **This is a deviation from §8's
letter**, taken under §8's own instruction that a measurement which can only be
had by disturbing the commit path is not to be taken.

## 5. Sampling cadence, and what it bounds

| Platform | Cadence | Readings expected per side of the window |
|---|---|---|
| Web | 500 ms progress tick, already running | ~4 |
| Apple | 2 s status poll, plus one synchronous access-log read either side of `replaceCurrentItem` | 2 |
| Android | event-driven (`onDroppedVideoFrames` fires as the renderer drops) plus the commit itself | varies; a switch with no drops on either side reports `Not measured` if the renderer said nothing, which the coverage clause makes visible |

Apple is the tightest. A counter delta needs two readings per side, and a 2 s
poll supplies one inside a 2 s half-window; the synchronous reads at the swap
are what make the second one certain. If a device run produces
`Not measured` on Apple more than occasionally, the answer is a denser sampler
on the poll — not a wider window, which would stop measuring the switch.

## 6. If a result fails

- **Apple drops frames at the item swap.** This result, and not the build plan,
  is what opens the second-layer cross-fade as a follow-up (build plan §8). D4
  keeps `replaceCurrentItem` until then.
- **A change ends in neither or both.** An ownership defect, not a measurement
  one: §5.3, §6.3 and §7.3 of the build plan own it, and the regression lanes
  named in §11 are where it is reproduced.
- **A seam with no dropped frames.** The audio and video paths are separately
  buffered; look at the underrun/stall count first and the runway second.
