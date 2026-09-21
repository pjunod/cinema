# Android display-mode matching and buffer budget — implementation plan

**Status:** implementation complete through M3; M0/M4/M5 pending physical-device evidence · **Executes:** §2.9 / D1 / F-android-1 /
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

One draft PR per milestone into `main` under the fast lane. Every Kotlin
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

### 5.6 M5 — device verification and the doc row

Run §6's two prompts, record results under a dated heading here, add the
`docs/README.md` index row (returned to the coordinator, not edited by this
plan).

Acceptance: both verification tables filled; `python3 -m unittest
tests.operations.test_docs_index` passes.

---

## 6. Verification and rollout

Fast lane per PR: `make android-test` (JVM + lint) and, for M1, `make unit`
plus the focused `cargo test -p plurxd decision`. Instrumented:
`make android-instrumentation` needs `PLURX_ANDROID_SERIAL`. Only a
television with a real HDMI sink proves the switch, so:

**GPT prompt — display mode:**

```text
Shield and Google TV, plurx setting "Match display mode" ON in Settings →
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

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M0 | [#409](http://192.168.4.7:3000/noirr/plurx/pulls/409) | needs: run §5.1's ADB measurement prompt on Lenovo, Google TV, and Shield; no device values were inferred. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | `009b9068` / [#409](http://192.168.4.7:3000/noirr/plurx/pulls/409) | `/decision` now preserves the exact source rational and Android parses it before prepare. Pinned Rust 1.97.1 focused regression and Android compile/`FrameRateTest` pass. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | `8b44b5d8` / [#409](http://192.168.4.7:3000/noirr/plurx/pulls/409) | Pure same-resolution cadence policy plus fractional/nominal/resolution/current-mode JVM cases pass. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 | `e4622cee` / [#409](http://192.168.4.7:3000/noirr/plurx/pulls/409) | Finite and progressive/deinterlaced Live TV match before prepare with a 2 s bound; prepared successors do not switch while staged; release/stop resets; late Media3 fallback, bounded telemetry, replicated switch, and advisory Developer status are wired. Focused Rust setting/readiness/live-cadence regressions and Android compile/policy/settings tests pass. Physical HDMI outcomes remain unobserved. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 review fix | [#409 comment #3199](http://192.168.4.7:3000/noirr/plurx/pulls/409#issuecomment-3199) | A post-wait channel/window ownership fence and exact-session cleanup prevent delayed Live TV display matching from resurrecting or releasing the wrong tune. The advisory display-mode checkbox now uses the server's ordinary one-key settings shape rather than the unrelated Live TV generation CAS. Deterministic coroutine/lease regressions and the real server save regression pass. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M4 | [#409](http://192.168.4.7:3000/noirr/plurx/pulls/409) | blocked by M0: no role-based allocation or `largeHeap` request was guessed; current sizing and instrumented behavior remain intact. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M5 | [#409](http://192.168.4.7:3000/noirr/plurx/pulls/409) | needs: run both §6 physical-device prompts after M0/M4; no display, HDR, black-frame, PSS, or OOM result is claimed. |
