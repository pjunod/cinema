# Android display-mode matching and buffer budget — implementation plan

**Status:** open — M1–M3 and M4 actual-heap containment on `main` since 2026-10-04 (#793); M0 measured on the Google TV Streamer 2026-10-02; M4 larger allocation decided *not justified* (no-build on buffer roles, coordinator decision, awaits Paul's ratification, see the 2026-10-04 relevance pass §2.15); the §5.6 matcher-timeout reset is built in the 2026-10-04 close-out PR (Android 146); M0 on the other televisions and the M5 HDMI matrix open · **Executes:** §2.9 / D1 / F-android-1 /
F-android-2 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [ANDROID-CLIENT-PARITY.md](ANDROID-CLIENT-PARITY.md) (what the
Android client lacks) and
[ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md](ANDROID-LIFECYCLE-PLAYER-BUILDER-AND-ERROR-CLASSIFICATION.md)
(the shared `PlurxPlayerBuilder` this plan's load control plugs into). Read
review §2.9 and §0's platform-API paragraph first, then the assessment rows
`2.9`, `D1`, `F-android-1`, `F-android-2`, then this document top to bottom.
Work milestone by milestone; M0 is a measurement that gates M4, and M1 is a
server change that gates M3. Every line number below is from `88a3957a` and
will drift — re-verify each anchor by function name before editing.

The standing instruction: **if a step seems to require changing the stall
tracker thresholds (`PlaybackTelemetry.kt` `OpenPlaybackStallTracker`), the
compatibility ladder, the prepared-replacement commit sequence, or the
`PlaybackLoadControlTest` assertions, stop and flag it instead of doing it.**
Those are contracts other efforts own; this plan sizes a buffer and selects a
display mode, nothing else.

**Correction to the review:** §2.9 says to select the mode "from the plan's
source fps". The Android plan carries no frame rate today.
`DecisionResponse.source` is `SourceSummary` (`crates/plurxd/src/http/stream.rs:420-449`)
— container, codec, profile, width, height, bit depth, HDR, DV, bitrate,
duration — and `PlanLike` (`Controller.kt:3921-3960`) has no fps field. The
server does know it: `frozen_video_frame_rate(probe_json)`
(`crates/plurxd/src/transcode.rs:5238`) feeds `HlsContext.frame_rate`
(`:9123`) into the master playlist's `FRAME-RATE` attribute
(`http/hls.rs:12422-12427`), and Live TV's `LiveTvDeliveryOutput.frame_rate`
(`LiveTvApi.kt:257`) is already a rational. So the finite-media path needs a
server addition (M1) before the client can decide before `prepare()`; Media3's
`Format.frameRate` is only known after the first track selection, which is
after `prepare()` and too late for a switch that does not glitch the first
seconds.

---

## 1. Objective

1. On a television, when the server's plan names a source frame rate, select
   a `Display.Mode` whose refresh rate is that rate or an integer multiple of
   it — never a different resolution — through
   `WindowManager.LayoutParams.preferredDisplayModeId`, before `prepare()`,
   and wait for `DisplayManager.DisplayListener.onDisplayChanged` for at most
   2 s before continuing regardless. Behind a replicated setting. Applied to
   finite playback, to the prepared successor only at commit, and to
   progressive Live TV output.
2. Size the incumbent player's byte budget against `largeMemoryClass` with
   measured headroom for the image cache and a primed successor, and shrink
   only the successor while it is primed — after measuring, on the three
   fleet televisions, what the heap actually holds.

Done means: M0's numbers are in this document, M1–M5 are merged, the two
device verifications in §6 are recorded, and no `PlaybackLoadControlTest`
assertion changed.

---

## 2. Contract today

Re-verify at build time. Each row names the function so the anchor survives
drift.

### 2.1 The byte budget

`clients/android/app/src/main/java/tv/plurx/app/player/PlaybackLoadControl.kt:15-33`:

```kotlin
internal fun playbackBufferTargetBytes(memoryClassMb: Int): Int =
    (memoryClassMb.coerceAtLeast(16) / 8).coerceAtMost(64) * 1024 * 1024

internal fun playbackLoadControl(context: Context, live: Boolean = false): DefaultLoadControl {
    val memoryClass = (context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager)
        ?.memoryClass ?: 128
    return DefaultLoadControl.Builder()
        .setTargetBufferBytes(playbackBufferTargetBytes(memoryClass))
        .setPrioritizeTimeOverSizeThresholdsForStreaming(false)
        .setPrioritizeTimeOverSizeThresholdsForLocalPlayback(false)
        .apply { if (live) setBufferDurationsMsForStreaming(4_000, 12_000, 1_000, 2_000) }
        .build()
}
```

What the literals mean: `memoryClass/8` capped at 64 MiB — 16 MiB on a
128 MB class device, 32 MiB on the Lenovo's 256, 64 MiB from 512 up.
`prioritizeTime… = false` on both modes means loading stops at the byte
target even when the time target (Media3 default 50 s) is not reached. The
file's own comment records why: two primed pipelines on the Lenovo ran out
of heap; each pipeline is budgeted independently so two leave room for
extractors, artwork and the successor. This is an allocator target, not a
cap on decoder memory.

Every player uses it: `Controller.kt:4030` (`buildPipeline`, incumbent and
successor), `LiveTvPlayer.kt:166` (`live = true`),
`LibraryChannelPlayer.kt:66`, `OfflineDownloads.kt:392`.

### 2.2 The tests that pin it

`androidTest/.../PlaybackLoadControlTest.kt` — instrumented, runs against the
real allocator: for a network and a local URI, live and not, it asserts
`shouldContinueLoading` is true at zero bytes, false once
`totalBytesAllocated ≥ target`, and `shouldStartPlayback` is **true** with a
full byte buffer and 750 ms buffered. That last assertion is the assessment's
point (F-android-1): a full byte target *does* start playback, so a small
budget is a rebuffer-resilience constraint, not by itself an 8 s stall
against `OpenPlaybackStallTracker(establishedThresholdMs = 8_000)`
(`PlaybackTelemetry.kt:281`). **These semantics do not change.**

`test/.../PlaybackBufferBudgetTest.kt` — JVM: `playbackBufferTargetBytes(256)
== 32 MiB` and, for every heap class, `2 × target ≤ heap / 4`. That 25 %
invariant is the one this plan revises (§3.2); it is a sizing rule, not a
player behaviour.

### 2.3 What the stall tracker needs from the buffer

`OpenPlaybackStallTracker` fires an advisory at 8 s of no playhead progress,
a non-deferrable event at 20 s, startup deadline 30 s. At 80 Mb/s, 16 MiB is
1.68 s of media, 32 MiB is 3.4 s, 64 MiB is 6.7 s. A Wi-Fi hiccup longer than
the buffer's duration starves the renderer; whether that reaches 8 s depends
on the network, not the buffer alone — LIKELY on high-bitrate remuxes over
Wi-Fi, unmeasured (review §2.9).

#### Measurement status — 2026-09-21

No Lenovo, Google TV, or Shield ADB session was available to the executing
session. None of M0's memory, PSS, delivered-bitrate, stall, or supported-mode
columns has therefore been observed. M4 remains deliberately unimplemented:
choosing `INCUMBENT_SHARE`, an image-cache allowance, or a successor reserve
without those readings would repeat the unmeasured 48 MiB-floor error this
plan was written to prevent. The existing buffer formula and every
`PlaybackLoadControlTest` assertion remain unchanged.

The conservative implementation decision was to complete the independently
safe cadence work (M1–M3), including advisory-only enablement, while leaving
the measurement-dependent allocation change out of the branch. The Developer
switch always remains operable; its seven-day matched-switch observation is
status, not a gate.

#### Measurement status — 2026-10-02 (M0, Google TV Streamer)

First production-process M0 row. Collected over adb by
claude-opus-5-5 (session
https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7), 2026-10-02
08:04–08:49 UTC, on the Google TV Streamer (API 34, 32-bit
`armeabi-v7a` process) running installed release build 142. The raw
`dumpsys meminfo`, display and logcat captures are held outside the repo
with the collector's receipt; the numbers below are copied from it.

**Build caveat.** Build 142 predates `1ffaae2a4` (the granted-heap clamp
and its `PlurxBufferBudget` log line), so the active formula was the legacy
`(memoryClass/8).coerceAtMost(64)` = **48 MiB per pipeline**. On this device
the clamp gives the same value (`Runtime.maxMemory()` = 384 MiB, /8 = 48 MiB,
measured 2026-09-30), so the reading carries over to the effort branch, on
`main` since 2026-10-04 (#793).

| Column | Google TV Streamer |
|---|---|
| memoryClass / largeMemoryClass | **384 / 512 MB** (`dalvik.vm.heapgrowthlimit` / `heapsize`); no `largeHeap` request |
| Granted heap | 384 MiB (`Runtime.maxMemory()`, 2026-09-30 isolated probe on the same device; 142 does not log it) |
| Buffer target | **48 MiB** per pipeline — about 5.5 s at 73 Mb/s, about 47 s at 8.2 Mb/s |
| TOTAL PSS idle (Home) | 63–70 MB (Java ~6.9 MB allocated, graphics 27 MB) |
| TOTAL PSS playing | 4K remux 132 MB, **158 MB peak while stalled** (Java ~59 MB, graphics 54 MB); 1080p 91–142 MB as the buffer filled; 720p steady 111–126 MB |
| TOTAL PSS primed | 137–**146 MB** with a successor alive — **not** a valid primed reading, see below |
| Crash / OOM | no crash or OOM entries in the crash buffer during the run (`logcat -b crash`; its last entry is from 2026-09-25). Low-memory-killer and `ApplicationExitInfo` history were not captured |
| Network | **2.4 GHz Wi-Fi** (802.11n, 2412 MHz, link 130–144 Mb/s; Ethernet unused). Measured delivery about 20 Mb/s (`wlan0` rx 19–22 Mb/s in 10 s windows; `adb push` about 18 Mb/s) |
| Stalls | 73 Mb/s DV Profile 7 remux (played as HDR10, TrueHD so remux, not direct play): three "Playback is stalled" dialogs in about 16 min, panel counted **3 supply / 0 decode**. 720p (4.2 Mb/s): zero buffering transitions in 5 min, 50 s buffered on device |
| Supported / active modes | 19 modes: 720p 50/59.94/60; 1080p 23.976/24/25/29.97/30/50/59.94/60; 2160p 23.976/24/25/29.97/30/50/59.94/60. Active 2160p60 throughout (HDR10 only advertised) |

Peak PSS was 158 MB against a 384 MiB Java limit, and Java allocation never
exceeded about 68 MB. **The stalls were supply, not buffer size**: the link
delivered about 20 Mb/s against a 73 Mb/s stream, a sustained deficit of about
53 Mb/s that empties any finite buffer — at 48 MiB in about 7.6 s, at 128 MiB in
about 20 s. A bigger buffer only moves the first stall later; it cannot remove
it. At a rendition the link can carry (720p, and 1080p at 8.2 Mb/s) the 48 MiB
target held 47–50 s of media and nothing stalled.

**Why the primed row is not valid.** On build 142 the staged successor
requested audio focus at `playWhenReady = true` and paused the incumbent
within 3 ms; the incumbent sat frozen about 20 s, both players were released
and a cold start followed. So the 146 MB "primed" PSS is one paused incumbent
plus a successor that only created an audio decoder — not two healthy
pipelines. That defect is already corrected on the effort branch, on `main`
since 2026-10-04 (#793)
(`320535286`, pinned by `f646b9bdb`: `handlesAudioFocus(role)` is false for
`PlayerRole.Successor` and focus moves with `handOverAudioFocus` at commit),
but no installed build carries it yet. A valid primed reading needs the
first build that does.

**Not measured:** the TCL 9445X and the Lenovo TB322FC were not visible to adb
during the run. The Lenovo is a tablet (`isTelevision()` is false there), and no
Shield is reachable; the plan's three-television list needs Paul's device
substitution ruling (proposed: Google TV Streamer for the HDMI/Shield rows, the
TCL for a television panel, the Lenovo for tablet memory only).

### 2.4 Where the frame rate lives

| Source | Field | Shape | Available before `prepare()` |
|---|---|---|---|
| Finite `/decision` | none | — | no (M1 adds `source.frame_rate`) |
| HLS master playlist | `FRAME-RATE=23.976` | decimal, 3 places | no — Media3 parses it during `prepare()` |
| Live TV `LiveTvDelivery.output.frame_rate` | `LiveTvRational?` | `num/den` | yes (`LiveTvApi.kt:257`) |
| Media3 `Format.frameRate` | float | after track selection | no |

### 2.5 Television detection and the successor

`isTelevision(context)` (`Controller.kt:3915`) reads
`UI_MODE_TYPE_TELEVISION`; it already gates tunneling. The successor is built
by `buildSuccessorPlayer` (`:3991`) with `setVideoSurface(null)` and volume 0
and becomes visible at the commit in `commitPreparedReplacement`
(`:3555-3572`, `player = successor; mediaSession.setPlayer(successor); …
attachRecipe(…)`). Astra's constraint (review §0): a staged successor must
not change the display before it is visible.

### 2.6 Settings surface

Replicated settings are `keys::*` constants in
`crates/plurx-core/src/store/mod.rs` (e.g. `PLAYBACK_AUTO_ABR =
"playback.auto_abr"`, `:1594`), read with `store.get_setting`, surfaced to
clients on `GET /api/v1/server` (`http/system.rs:68-72`) and shown in
Settings → Developer as a `DeveloperEnableItem` with advisory
`DeveloperRequirement` rows (`http/developer.rs:1377-1385` is the shape).

---

## 3. Change

### 3.1 Display-mode selection

```
 plan arrives (decision / live start)
        │ source frame rate known?  ── no ──▶ no change, log outcome=no_source_rate
        ▼ yes
 television && setting on?           ── no ──▶ no change, outcome=disabled
        ▼ yes
 DisplayModeMatcher.choose(supported, current, fps)
        │ null (already matched / no candidate) ──▶ outcome=unchanged
        ▼ mode id
 window.attributes.preferredDisplayModeId = id
 wait onDisplayChanged(displayId) ≤ 2 s ──▶ outcome=matched | timeout
        ▼
 player.prepare()
```

**The pure policy**, JVM-testable, no Android types:

```kotlin
internal data class DisplayModeCandidate(val id: Int, val width: Int, val height: Int, val refreshHz: Float)

/** The mode to request, or null when the current one is already right or nothing fits. */
internal fun chooseDisplayMode(
    supported: List<DisplayModeCandidate>,
    current: DisplayModeCandidate,
    sourceFps: Double,
): Int?
```

Rules, each with its reason:

- Only modes with `width == current.width && height == current.height`.
  A resolution change re-negotiates HDMI and can drop HDR signalling; the
  gain is cadence, not pixels.
- Refresh matches when `|refresh − k × fps| ≤ 0.01 Hz` for integer `k ≥ 1`;
  prefer the smallest `k`, then the exact fractional family (23.976 over 24
  for a 23.976 source, 59.94 over 60 for 29.97). 0.01 Hz separates
  23.976 from 24.000 (0.024 apart) and 59.94 from 60 (0.06 apart) on every
  panel that reports them; a looser tolerance would pick 24 for 23.976 and
  keep the judder the change exists to remove.
- If the current mode already satisfies the rule, return null: requesting the
  same id is harmless but the 2 s wait is not free.
- No candidate → null. Never fall back to "closest": 50 Hz for a 24 fps
  source is worse than the panel's 60 Hz 3:2.

**Wiring** (`Controller.kt`, a new `DisplayModeMatcher` class holding the
`Activity` window reference the screen passes in):

- Finite playback: called once per plan, before the incumbent's `prepare()`
  in the attach path that follows `sessionCreateCoordinator.attachIfCurrent`
  (`:1732`) and the direct-play attach; the fps comes from
  `plan.sourceFrameRate` (M1). Not on quality changes within the same source.
- Successor: `buildSuccessorPlayer` does **not** call it. `commitPreparedReplacement`
  calls it after `player = successor` only if the successor's source rate
  differs from the incumbent's — it will not, for a rung change of the same
  file, so in practice the mode is untouched at commit. This is the Astra
  constraint made concrete.
- Live TV (`LiveTvPlayer.attach`, `:156`): called before `output.prepare()`
  with `started.delivery.output.frame_rate` when that output is progressive
  (the server's planner has already doubled the cadence for `bwdif
  send_field`, so the delivered rate is the right input — review Q9). A
  channel change re-evaluates; a compatibility retry does not.
- Reset: `preferredDisplayModeId = 0` in `Controller.release()` and
  `LiveTvPlayer.stop`, so the launcher gets its mode back. Android restores
  the mode when the window goes away anyway; resetting explicitly makes the
  ledger row truthful.
- Wait: register a `DisplayManager.DisplayListener` before setting the
  attribute, resolve on `onDisplayChanged(displayId)` where
  `display.mode.modeId == requested`, or after 2 s. A switch that never
  arrives (panel refuses) proceeds with `outcome=timeout`; it is not an
  error. 2 s is the jellyfin-androidtv figure and covers every HDMI
  re-sync observed there; it is a guess for our panels until M5 measures it.
- Telemetry: one `playbackTelemetry.report(event = "playback_display_mode",
  level = "info", detail = "outcome=<matched|unchanged|timeout|no_source_rate|disabled|unsupported> source_fps=<f> from_hz=<f> to_hz=<f> wait_ms=<n>")`
  per decision. Bounded outcome vocabulary, no content identifiers.

**Setting:** `keys::PLAYBACK_DISPLAY_MODE_MATCH = "playback.display_mode_match"`,
`"1"` on, missing off. Surfaced on `GET /api/v1/server` as
`display_mode_match: bool` beside `playback_auto_abr` (same `is_some_and(==
"1")` read) and in Settings → Developer as `DeveloperEnableItem{id:
"android_display_mode_match"}` with advisory requirements: "a television
client has reported a matched switch" (from `playback_display_mode`
telemetry, `outcome=matched`, this node, last 7 days) — advisory only,
enabling proceeds either way (`developer.rs:3236` precedent). Not a per-device
preference in this plan (§7 Q1).

### 3.2 Buffer budget

**Execution clarification, 2026-09-30:** the original larger incumbent
allocation below still requires M0's three-device idle/playing/primed
measurements. A side-by-side instrumented process cannot qualify the
installed production process or supply that matrix. While collecting honest
supplemental evidence, the continuation implements the independently sound
containment: `min(existing memoryClass/8 ceiling, Runtime.maxMemory()/8)`
for every explicit `BufferRole`. No role's target increases, no `largeHeap`
request is added, and no unmeasured incumbent share, image-cache allowance or
successor reserve is chosen. A lower actual grant reduces the legacy target;
the historical floor is not allowed to defeat the real process grant.

`PlaybackBufferBudget.kt` owns the pure policy. `PlurxPlayerBuilder` maps
staged successors to `Successor`, Live TV to `Live`, and other roles to
`Incumbent`; a committed successor retains its original load control.
`PlaybackLoadControl` records the actual class, hypothetical large class,
granted heap, configured Coil cache bound and chosen target locally. Cache
bytes are observations, not a new guessed reserve. This target remains an
allocator threshold, not a decoder/native-memory cap. Byte priority for local
and network media, full-buffer startup and all Live TV durations are unchanged.

`PlaybackBufferBudgetTest` rejects a declared 512 MiB class being treated as
a granted 128 MiB heap and proves every role stays within its existing
ceiling and the existing two-buffer quarter-heap containment. The real
`PlaybackLoadControlTest` retains every startup/loading assertion and loops
over all roles. Its provenance sample names package, version, SDK, ABI,
debuggable/largeHeap flags, actual grant, cache bound and targets. Physical
receipts below must distinguish a test process from production playback.

Two budgets instead of one:

```kotlin
internal enum class BufferRole { Incumbent, Successor, Live }

internal fun playbackBufferTargetBytes(
    memoryClassMb: Int,
    largeMemoryClassMb: Int,
    role: BufferRole,
): Int
```

- `Successor`: today's formula unchanged — `min(memoryClass/8, 64) MiB`. The
  successor is the second pipeline the Lenovo OOM was about; it stays small
  while it is primed. At commit the successor becomes the incumbent, and its
  load control cannot be swapped on a live `ExoPlayer` — so it keeps the
  small budget until the next replacement. Acceptable: a committed successor
  is a quality change, not a cold start, and it inherits a warm network.
- `Live`: unchanged (`live = true` durations, successor-size bytes).
- `Incumbent`: `min(HEAP × INCUMBENT_SHARE − IMAGE_CACHE − SUCCESSOR_RESERVE,
  128 MiB)` where `HEAP` is `largeMemoryClass` **only if the manifest requests
  `largeHeap` and M0 shows the request is honoured on that device class**,
  else `memoryClass`; `IMAGE_CACHE` is Coil's configured memory-cache bound;
  `SUCCESSOR_RESERVE` is the successor budget above; `INCUMBENT_SHARE` is a
  constant chosen from M0 (starting proposal 0.5). Floor: never below
  today's value. Every constant is named in the file with its M0 reading
  beside it, because "48 MiB floor" without the measurement is exactly what
  the assessment refused.

`largeHeap` is a request (assessment correction 8): `ActivityManager.
largeMemoryClass` is what the platform *would* grant, and some launchers and
low-RAM devices grant nothing. So the runtime reads `Runtime.maxMemory()`
after start and uses it, not the class, as `HEAP` — that is the granted
number.

The JVM `PlaybackBufferBudgetTest` invariant changes from `2 × target ≤
heap/4` to `incumbent + successor + IMAGE_CACHE ≤ HEAP × 0.75` for every
class in its table — stated as the new sizing rule with M0's numbers in the
test's comment. The instrumented `PlaybackLoadControlTest` runs for both
roles and keeps every assertion (byte target wins over time; a full buffer
starts playback).

---

## 4. Guardrails (non-goals)

- **Do not use `Surface.setFrameRate` or any Media3 "frame rate strategy"
  as the mechanism.** Media3 has `VIDEO_CHANGE_FRAME_RATE_STRATEGY_OFF` and
  `_ONLY_IF_SEAMLESS`; there is no `_ALWAYS` (that constant belongs to
  `Surface.setFrameRate`), and `ONLY_IF_SEAMLESS` is what already fails to
  switch an HDMI TV. `preferredDisplayModeId` is the mechanism; the review
  withdrew the other (§0).
- **Phones and tablets are out.** `isTelevision` gates it. A phone's panel
  has no HDMI mode to negotiate and some handset displays flicker on a mode
  request.
- **Never change resolution**, only refresh (§3.1). HDR signalling and
  overscan settings ride on the resolution mode.
- **Do not switch on the staged successor.** Only at commit, only if the
  source rate changed (§3.1). Otherwise the picture the viewer is watching
  glitches for a switch that has not happened.
- **Do not exclude Live TV wholesale, and do not match interlaced output.**
  F-android-2: progressive 24/25 fps and deinterlaced 50/59.94 output need
  matching; the delivered output rate is the input, never the source's
  interlaced field rate.
- **Do not raise the successor's budget**, do not remove
  `setPrioritizeTimeOverSizeThresholds…(false)`, do not touch the live
  durations. Each is a recorded decision (§2.1); the incumbent budget is the
  only number M4 moves.
- **Do not add `android:largeHeap="true"` before M0** and do not treat it as
  the fix. It is a request; the granted heap is `Runtime.maxMemory()`.
- **Do not change `OpenPlaybackStallTracker` thresholds** to make a small
  buffer look better. The tracker's 8 s is a recovery contract
  (PLAYBACK-SURFACE-CONTRACT); the buffer is sized to it, not the reverse.
- **No in-code feature flag.** The switch is the replicated setting in §3.1,
  read from `/api/v1/server`; a Kotlin `const val ENABLE_…` is refused.
- **Do not decide from `Format.frameRate` after `prepare()`** as the primary
  path. It is allowed only as the M3 fallback when the plan has no rate and
  the setting is on, and then it logs `outcome=late`.

---

## 5. Milestones

**Integration amendment, 2026-09-30:** the remaining continuation branches
from current `effort/architecture-review-2026-09-20` and targets that effort,
with one independent review, the tracked hook and the smallest focused
Android regressions. `Effort development gate` blocks integration; full
exact-tree qualification and `Main promotion gate` separately block final
promotion. The original `main` fast-lane instruction is retained as history,
not the continuation's rollout path.

Originally, one draft PR per milestone into `main` under the fast lane. Every Kotlin
change needs the Android build-counter bump (`validation/mobile_versions.py`,
`versionCode` in `clients/android/app/build.gradle.kts:48`).

### 5.1 M0 — measure the three televisions (no code)

GPT prompt, to run with device access:

```text
On the Lenovo (Android TV), the Google TV and the Shield, with the current
plurx APK installed and the server reachable:
1. `adb shell dumpsys meminfo tv.plurx.app` idle, then during 4K HDR direct
   play of "Harbor Lights" (a ≥60 Mb/s remux), then with a prepared successor
   primed (change quality once during playback and capture within 5 s).
   Record: Java heap used/max, native heap, graphics, TOTAL PSS each time.
2. `adb shell dumpsys activity processes | grep -A2 tv.plurx.app` and
   `adb shell getprop dalvik.vm.heapgrowthlimit dalvik.vm.heapsize` — these
   are memoryClass and largeMemoryClass on that device.
3. In the playback info panel, note the delivered bitrate and whether any
   stall/advisory appeared during a 5-minute window; repeat once on Wi-Fi if
   the device has Ethernet.
4. `adb shell dumpsys display | grep -A3 'mSupportedModes\|mActiveMode'` for
   each device with its TV set to Match Content OFF (the default): list every
   mode id, resolution and refresh.
5. Report one table per device: memoryClass, largeMemoryClass, granted
   maxMemory (from a logcat line the app will print in M4; for M0 read
   `dumpsys meminfo` "Java Heap" max), PSS idle/playing/primed, and the mode
   list. Do not change any setting on the device.
```

Acceptance: the five columns per device are pasted into §2.3 of this
document under a dated heading, with the adb output attached to the PR.

### 5.2 M1 — server: `source.frame_rate` on `/decision`

Add `pub frame_rate: Option<String>` (`"24000/1001"` form, `avg_frame_rate`
preferred, `r_frame_rate` fallback, exactly the rule `frozen_video_frame_rate`
applies) to `SourceSummary` (`stream.rs:420`), `#[serde(skip_serializing_if =
"Option::is_none")]` so older clients see no change; populate it where
`SourceSummary` is built from the `files` row's `probe_json`. A rational
string, not a float: the client must tell 23.976 from 24, and `FRAME-RATE`'s
three decimals already lose that (`23.976` vs `23.976023…`). Android
`SourceSummary` model (`Models.kt:540-549`) gains `val frame_rate: String? =
null`; `PlanLike` gains `val sourceFrameRate: Double?` parsed from it. Also
add it to `docs/API.md`'s `/decision` response table in the same PR.

Acceptance: `cargo test -p plurxd decision` includes a new test asserting a
probe with `avg_frame_rate: "24000/1001"` yields `"24000/1001"` and one with
neither field yields `null`; `cargo test -p plurxd --bin plurxd
video_frame_rate` still passes; `python3 -m unittest
tests.operations.test_api_doc_routes` passes.

### 5.3 M2 — `chooseDisplayMode` policy and its JVM test

`DisplayModePolicy.kt` with the function in §3.1 and
`DisplayModePolicyTest.kt` covering: 23.976 source against {60, 59.94, 24,
23.976} picks 23.976; against {60, 50, 24} picks 24; against {60, 50}
returns null; 25 against {50, 60} picks 50; 29.97 against {59.94, 60} picks
59.94; a 1080p mode is never chosen for a 2160p current mode; current
already matching returns null.

Acceptance: `make android-test` green with the new test class listed in
`clients/android/app/build/test-results/`.

### 5.4 M3 — wiring, setting, telemetry

The `DisplayModeMatcher` wiring in §3.1 for finite, commit and Live TV; the
`playback.display_mode_match` key, `/api/v1/server` field, Developer item;
the `playback_display_mode` telemetry event; `late` fallback from
`Format.frameRate` when the plan has no rate. The setting read is one field
on the `ServerInfo` the app already fetches at connect, so no new request.

Acceptance: `cargo test -p plurxd developer` and `cargo test -p plurxd
server_info` cover the new field; `make android-test`; on the Shield with
the setting on, `adb logcat -s PlurxTelemetry | grep playback_display_mode`
shows `outcome=matched` for a 23.976 title and `outcome=unchanged` for a
second play of the same title; with the setting off, `outcome=disabled`.

### 5.5 M4 — role-based budget from M0's numbers

The September 30 actual-heap clamp is complete containment, not acceptance
of this milestone's proposed larger incumbent budget. The remaining share,
cache headroom and prepared-successor PSS still require the original M0
matrix. Supplementary no-activity instrumentation uses the existing isolated
`capabilityProbe` identity by passing `-PplurxIsolatedBudgetProbe=true`; it
must verify the generated target package before installing and cannot replace
`tv.plurx.app`. Run only `PlaybackLoadControlTest`, without launching an
Activity, waking a display, signing in, starting playback or changing settings.
Never run the generic instrumentation install target against physical
production hardware: that target uninstalls `tv.plurx.app`.

`playbackBufferTargetBytes(memoryClassMb, largeMemoryClassMb, role)`, the
constants filled from M0 with their readings in comments, `Runtime.
maxMemory()` as the granted heap, one logcat line at player build naming
role, class values, granted heap and the chosen target. `largeHeap` in the
manifest only if M0 shows every target device grants more than
`memoryClass`; otherwise it is left out and this plan says why. Update
`PlaybackBufferBudgetTest` to the new invariant; `PlaybackLoadControlTest`
loops over roles unchanged.

Acceptance: `make android-test`; `make android-instrumentation` on the
disposable emulator passes `PlaybackLoadControlTest` for both roles; the M0
PSS "primed" measurement re-run on the Lenovo stays below the device's
low-memory kill threshold with the new incumbent budget (GPT prompt in §6).

**M4 decision, 2026-10-02 — recorded by the coordinator for Paul's review.**
A larger incumbent allocation is **not justified by the evidence**. The one
production M0 row (Google TV Streamer, §2.3, 2026-10-02) shows peak PSS 158 MB
against a 384 MiB grant, a 48 MiB target that held 47–50 s of any rendition the
link could carry, and every stall attributed to supply (about 20 Mb/s delivered
on 2.4 GHz Wi-Fi against a 73 Mb/s remux), which no buffer size fixes. So:

- the September 30 granted-heap containment stays as the shipped M4;
  `INCUMBENT_SHARE`, the image-cache allowance and the successor reserve are
  **not** introduced, and `largeHeap` stays out of the manifest;
- the role parameter remains deliberately inert (`when (role)` maps every role
  to the same ceiling) — that is the decision, not an omission;
- §7 question 3 (successor size after commit) is moot while the roles share one
  size;
- the instrumented `PlaybackLoadControlTest` and `PlaybackBufferBudgetTest`
  invariants are unchanged.

What would reopen it: a television whose stalls are counted as decode- or
buffer-side while delivery meets the stream's bitrate, or a valid primed-successor
reading (on a build carrying `320535286`) whose Dalvik allocation — the
`dumpsys meminfo` Dalvik Heap "Alloc" column, at most about 68 MB on 142 — exceeds
half of the granted `Runtime.maxMemory()` (192 MiB on the Google TV). Total PSS
is not the comparison: it includes native and graphics memory that the Java heap
limit does not govern. The supply-side remedy the evidence does point at — Auto quality stepping
down when delivery is far below the stream (on 142 Auto stayed on the 73 Mb/s
original through three stall dialogs) — belongs to the native adaptive-quality
work, not to this plan.

### 5.6 M5 — device verification and the doc row

Run §6's two prompts, record results under a dated heading here, add the
`docs/README.md` index row (returned to the coordinator, not edited by this
plan).

Acceptance: both verification tables filled; `python3 -m unittest
tests.operations.test_docs_index` passes.


**M5 precondition, 2026-10-02.** The matched path needs the replicated setting
`playback.display_mode_match` turned **on** (Settings → Developer → **Android TV display-mode
matching**). It is **currently off** on the fleet: `/api/v1/server` returned
`display_mode_match: false`, and every 2026-10-02 play logged
`playback_display_mode outcome=disabled`. With it off the Google TV stayed at
2160p60 before, during and after playback (the setting-OFF row of §6 is
therefore observed on that device), and 23.976 content on the 60 Hz output
produced about 20,900 `VideoRenderQualityTracker` frame-timing warnings in
about 42 minutes — the cost this plan exists to remove. The system's own
`match_content_frame_rate` was 1 and did not switch the mode either, so the app
request is the only path.

**Matcher timeout leaves the request set (checked 2026-10-02 against
`DisplayModeMatcher.match`).** On the 2 s timeout `withTimeoutOrNull` returns
null and `match` reports `outcome=timeout`, but nothing clears
`window.attributes.preferredDisplayModeId`; only `reset(owner)` does, and the
callers (`Controller` start path, `LiveTvPlayer` tune) call it on stop or
release, not on timeout. Playback then starts at the current mode, and if the
display completes the switch later the HDMI resync lands seconds into playback
with no telemetry recording when. The same is true of the owner-changed return
after the wait. This is a real gap, not yet observed: it can only happen with the
setting on. M5 must look for it explicitly — note any mode change after first
frame and its time. The recommended correction is to clear
`preferredDisplayModeId` (back to 0, as `reset` does) when the wait times out
while this owner is still current: that removes the late switch at its root.
Merely logging a late switch would need a *new* listener registered at the
timeout and held until `reset(owner)` — the wait's own listener cannot do it,
because `withTimeoutOrNull` cancels the wait and `invokeOnCancellation`
unregisters that listener (`DisplayModeMatcher.kt`, the `match` wait). On the
owner-changed return the request must not be cleared blindly, since the new
owner may have set its own. The `late` path (`onTracksChanged`, no source rate)
switches mid-play by design and is not this gap.

**Built 2026-10-04 (Android build 146).** `match` now withdraws the request —
`preferredDisplayModeId = 0` — when a prepare-time wait times out and this owner
is still current, and leaves it on the owner-changed return. A `late` wait
(the `onTracksChanged` fallback above) never withdraws, matched or not: it is
already mid-play by design, and a display that completes that switch after the
2 s bound is doing what the late request asked for. The decision is
`clearsDisplayModeRequestAfterWait(matched, stillOwner, late)`, pinned by
`DisplayModeMatcherTest.timedOutPrepareWaitWithdrawsTheRequestOnlyWhileItStillOwnsTheWindow`
and `DisplayModeMatcherTest.lateWaitNeverWithdrawsTheRequest`. The
`playback_display_mode` detail now ends `withdrawn=true|false`, so telemetry
records which waits cleared the request. M5 still has to observe it: with the
setting on, a timed-out prepare-time switch should leave the mode unchanged
through playback rather than change it after first frame.

---

## 6. Verification and rollout

Fast lane per PR: `make android-test` (JVM + lint) and, for M1, `make unit`
plus the focused `cargo test -p plurxd decision`. Instrumented:
`make android-instrumentation` needs `PLURX_ANDROID_SERIAL`. Only a
television with a real HDMI sink proves the switch, so:

**GPT prompt — display mode:**

```text
Shield and Google TV, plurx setting "Android TV display-mode matching" ON in Settings →
Developer, TV's own Match Content OFF. Play "Harbor Lights" (23.976, HDR10)
from the start; within 10 s run `adb shell dumpsys display | grep
mActiveMode` and read the TV's info panel (the TV's own HDMI-mode overlay, not
plurx's badge). Expect 23.976 or 24 Hz at the same resolution and the HDR
badge on the TV still lit. Change quality once (prepared successor): the TV's
mode must not change during the switch. Press Back: the launcher's mode
returns (dumpsys shows the original id). Then a Live TV channel that plurx
labels progressive 59.94 and one it deinterlaces to 59.94: expect 59.94 or
60, no black-screen longer than 2 s at start. Repeat with the setting OFF:
mode never changes. Report the dumpsys line and the TV overlay for each step;
note any black frame longer than ~1 s with its duration.
```

**GPT prompt — memory after M4:** the M0 prompt again on the Lenovo with the
M4 build, plus `adb shell dumpsys meminfo tv.plurx.app | grep -E 'Java
Heap|TOTAL'` during a quality change, and `adb logcat -b crash -d` to
confirm no OOM; report the buffer line the app logs at player build.

Rollout: the setting ships off; Paul turns it on per fleet node after the
Shield verification; the budget change has no switch (it is a sizing rule
with a floor at today's value), so it ships on the M4 APK to every device via
the mobile release role.

---

## 7. Open questions

1. **Per-device opt-out.** A panel that flickers on every mode switch wants
   this off for that TV only; the replicated setting is server-wide. Paul's
   rule against in-code gates does not forbid a viewer preference in
   `ViewerPreferences`; whether it is worth one is his call after M5.
2. **HDR mode switching.** `preferredDisplayModeId` covers refresh; matching
   HDR/SDR output (Android 14's `Display.getHdrSdrRatio` era) is a separate
   API and a separate plan.
3. **Successor budget after commit.** §3.2 accepts that a committed successor
   keeps the small budget until the next replacement. If M0 shows the Lenovo
   can hold two incumbent-sized buffers, the successor could be built large
   from the start and this asymmetry disappears — decide from the numbers.
4. **Direct play without a `/decision`.** Every Android play goes through
   `/decision` today; if a path is found that does not, it gets
   `outcome=no_source_rate` and the `late` fallback, and this document should
   name it.

---

## Execution log

**2026-09-30 continuation:** original M0/M4/M5 gaps remain explicit. The
Google TV Streamer's passive observation found production build 139 without
a running process and display OFF; heap properties 384m/512m were not
relabeled granted heap. Supplemental isolated instrumentation and source
checks are attributed to their exact build and package, not to production 139.

**Supplemental Google TV Streamer run, 2026-09-30 23:52–23:56 UTC:** wireless
serial `61171HFAG1GG00`, API 34; isolated debug package
`tv.plurx.app.capabilityprobe`, version 141, instrumented target verified as
that package. Both `PlaybackLoadControlTest` cases passed in 1.027 s without
an Activity, player start, media request, display wake or setting change.
The process reported `Runtime.maxMemory() = 402653184` bytes (384 MiB),
`memoryClass = 384`, hypothetical `largeMemoryClass = 512`, no largeHeap
request, configured Coil bound 80530636 bytes (about 76.8 MiB), and target
50331648 bytes (48 MiB) for each role. Reported device ABI list was
`armeabi-v7a,armeabi`; it is not a claim about a production process.

App APK SHA256
`8832de19a95a2267794bb9969e56c411aae95ee689a595078441ec38f2d45026`;
test APK SHA256
`56d8a01fffba601d27e0f1c90ca5b6357e38b70c7311e54e3a4b715510c6192b`.
Both debug signatures were verified, certificate SHA256
`5cefd0c7db3f0a8d6fd818937425b7647b3222f9ed1419d79883a12c3e168bce`.
The build used verified Temurin 25.0.4.1+1 / AGP 9.3.2 / SDK 37.0, not the
earlier JBR 21 baseline. Both previously absent probe packages were uninstalled;
production 139 APK hash, install/update times and absence of a running
production process were unchanged. Display stayed OFF with no app mode
request. No Java/native/graphics PSS under playback, primed successor, OOM
threshold, delivered bitrate or HDMI matrix is established by these tests.

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M0 | [#409](http://forge.lan:3000/noirr/plurx/pulls/409) | needs: run §5.1's ADB measurement prompt on Lenovo, Google TV, and Shield; no device values were inferred. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | `009b9068` / [#409](http://forge.lan:3000/noirr/plurx/pulls/409) | `/decision` now preserves the exact source rational and Android parses it before prepare. Pinned Rust 1.97.1 focused regression and Android compile/`FrameRateTest` pass. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | `8b44b5d8` / [#409](http://forge.lan:3000/noirr/plurx/pulls/409) | Pure same-resolution cadence policy plus fractional/nominal/resolution/current-mode JVM cases pass. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 | `e4622cee` / [#409](http://forge.lan:3000/noirr/plurx/pulls/409) | Finite and progressive/deinterlaced Live TV match before prepare with a 2 s bound; prepared successors do not switch while staged; release/stop resets; late Media3 fallback, bounded telemetry, replicated switch, and advisory Developer status are wired. Focused Rust setting/readiness/live-cadence regressions and Android compile/policy/settings tests pass. Physical HDMI outcomes remain unobserved. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 review fix | [#409 comment #3199](http://forge.lan:3000/noirr/plurx/pulls/409#issuecomment-3199) | A post-wait channel/window ownership fence and exact-session cleanup prevent delayed Live TV display matching from resurrecting or releasing the wrong tune. The advisory display-mode checkbox now uses the server's ordinary one-key settings shape rather than the unrelated Live TV generation CAS. Deterministic coroutine/lease regressions and the real server save regression pass. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M4 | [#409](http://forge.lan:3000/noirr/plurx/pulls/409) | blocked by M0: no role-based allocation or `largeHeap` request was guessed; current sizing and instrumented behavior remain intact. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M5 | [#409](http://forge.lan:3000/noirr/plurx/pulls/409) | needs: run both §6 physical-device prompts after M0/M4; no display, HDR, black-frame, PSS, or OOM result is claimed. |
| 2026-09-30 | gpt-6.1-sol | agent:/root/k06_runtime_sol61 | M4 containment and supplementary measurement | `codex/d01-android-buffer-budget` into the architecture effort | Actual-granted-heap no-increase policy and role wiring; focused JVM4, Android lint/application/test compile, and isolated wireless Google TV allocator/provenance2 pass. Original larger incumbent allocation and three-TV M0/M5 remain open. |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M0 (Google TV Streamer) | `opus/client-evidence` into the architecture effort | Production build 142 measured over adb: memoryClass 384 / large 512, PSS 63–158 MB, 48 MiB buffer, stalls 3 supply / 0 decode on about 20 Mb/s 2.4 GHz Wi-Fi; dated table in §2.3. Primed row invalid (successor audio-focus defect, corrected on the effort by `320535286`). TCL and Lenovo not reachable; the device-substitution ruling is Paul's. |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M4 decision | `opus/client-evidence` | Coordinator decision for Paul's review: larger incumbent allocation not justified; granted-heap containment stays as M4 (§5.5). |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M5 precondition | `opus/client-evidence` | `display_mode_match` is off on the fleet, so only the setting-OFF row is observed (Google TV stayed 2160p60). Matcher timeout leaves `preferredDisplayModeId` set (§5.6 note); M5 must watch for a late switch. |
| 2026-10-04 | claude-opus-5-5 | https://claude.ai/code/session_01ENdV5pjk5WztKEXnKHy8YT | §5.6 timeout correction | `0acff727`, Android build 146 | On timeout with the owner still current the matcher sets `preferredDisplayModeId` back to 0; the owner-changed return does not clear. JVM regression `timedOutWaitWithdrawsTheRequestOnlyWhileItStillOwnsTheWindow` (four cases). Not compiled or run here (no Android SDK in this session); no device observation. |
| 2026-10-04 | claude-opus-5-5 | https://claude.ai/code/session_01ENdV5pjk5WztKEXnKHy8YT | §5.6 review fix | close-out PR, Android build 146 | Adversarial review: the withdrawal also fired on the `late` (`onTracksChanged`) wait, which switches mid-play by design. The helper now takes `late` and never withdraws on that path; the test was renamed `timedOutPrepareWaitWithdrawsTheRequestOnlyWhileItStillOwnsTheWindow` and `lateWaitNeverWithdrawsTheRequest` added; telemetry detail gains `withdrawn=`. Not compiled or run here (no Android SDK in this session). |
