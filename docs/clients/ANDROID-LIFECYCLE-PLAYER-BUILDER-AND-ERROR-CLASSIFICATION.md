# Android lifecycle, one player builder, and error classification — implementation plan

**Status:** ready for review · **Executes:** D2 / D3 / D4 / D7 /
F-android-3 / F-android-4 / F-android-5 / F-android-9 / F-android-13 /
F-android-14 / F-android-15 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to
[ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md](ANDROID-DISPLAY-MODE-AND-BUFFER-BUDGET.md)
(the load control M7's builder takes by role) and
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (the recovery
ownership every error change below must stay inside). Read review §3.7 rows
D2–D4 and D7, then the assessment rows `D2`–`D4`, `D7` and
`F-android-3/4/5/9/13/14/15`, then this document. Nine milestones, one PR
each; M1–M3 are one lifecycle contract and land in order, M4–M6 are
independent, M7 depends on nothing but should land before M4–M6 touch the
Live TV builder, M8 and M9 are independent. Line numbers are from
`88a3957a`; re-verify each anchor by function name.

The standing instruction: **if a step seems to require changing a recovery
budget, the compatibility ladder's order (`playbackErrorAction`), the
finite-timeline `BEHIND_LIVE_WINDOW` rule, the prepared-replacement commit,
or the surface presenter, stop and flag it.** Every change here adds a
classified branch *beside* those; none re-orders them.

**Correction to the review:** none of substance. Two precisions. (1) D2
cites `Controller.kt:2640-2647` as "does not pause there" — correct;
`setPresentationForeground` (`:2640`) only invalidates the stall observation
and marks the surface hidden, and `PlayerScreen.kt:1089-1097` drives it from
`Lifecycle.State.STARTED`, which is also true in visible PiP — so the hook
this plan needs already fires at the right moments and only lacks the pause.
(2) D3's "the comment describes code that does not exist" is
`PlaybackPolicy.kt:105-106` (`2004 … a 4xx capability answer is terminal and
is filtered at the call site`); `retryMediaOnNextNode` (`:2280`) filters
nothing by status. Confirmed.

---

## 1. Objective

1. **Lifecycle:** leaving the activity (ON_STOP, not in PiP) pauses video as
   an owner-initiated pause that survives and never overrides an explicit
   viewer pause; audio-only playback moves into a `MediaSessionService` with
   a notification, `WAKE_MODE_NETWORK` and the media-playback foreground
   permission, as one contract; the screen stays on for video and is
   released while paused.
2. **Errors:** Live TV recovers once per attach from `BEHIND_LIVE_WINDOW`
   after validating its session; AudioTrack failures 5001/5002/5004 are
   classified explicitly with sink diagnostics and spend the existing
   compatibility budget instead of falling into "unknown"; node failover is
   gated on the HTTP status the exception carries.
3. **Construction:** one `PlurxPlayerBuilder(context, role)` builds all
   five players with the safe defaults shared and the role differences
   explicit.
4. **Startup and polling:** `OfflineDownloads` recovery leaves the main
   looper without losing process-wide readiness; the 2 s session-status poll
   backs off when nothing reads it, with a bounded detection delay.

Done means: nine PRs merged, the device checks in §6 recorded, and the
`web-policy.test.js` Android inventory assertions unchanged except where §5
names them.

---

## 2. Contract today

Re-verify at build time.

### 2.1 Lifecycle and screen

| Fact | Where |
|---|---|
| Lifecycle observer calls `controller.setPresentationForeground(lifecycle.currentState.isAtLeast(STARTED))` | `PlayerScreen.kt:1089-1097` |
| `setPresentationForeground(foreground)` → `stallGuard.invalidateObservation()`, `surfaceOwner.hidden(!foreground)`, `sampleTargetPresentationDeadline()`; **no pause** | `Controller.kt:2640-2647` |
| `PlayerView(...).apply { keepScreenOn = true }` unconditional | `PlayerScreen.kt:1314` |
| Viewer pause: `playPause()` flips `playbackIntent.playbackRequested` through `stallGuard.setPlaybackRequested` and sets `player.playWhenReady` | `Controller.kt:1414-1421`; `PlaybackIntent.setPlaybackRequested` `:112-115` |
| Every attach re-applies `player.playWhenReady = playbackIntent.playbackRequested` | `Controller.kt:1016, 1226, 1614, 1630, 1760, 2051, 2320` |
| Stall tracker resets when `playbackRequested` is false | `PlaybackTelemetry.kt:305-308` |
| PiP: `isInPictureInPicture(activity)`; auto-enter on S+, leave-hint on 8–11 | `PlayerScreen.kt:439-440, 1180-1198` |
| `MediaSession.Builder(context, player).build()` inside the controller; no service | `Controller.kt:477` |
| Manifest: `WAKE_LOCK`, `POST_NOTIFICATIONS`, `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_DATA_SYNC`; services: `PlurxDownloadService` (`dataSync`), `OfflineTransferJobService`, `PlatformSchedulerService`; **no** `FOREGROUND_SERVICE_MEDIA_PLAYBACK`, no `mediaPlayback` service | `AndroidManifest.xml` |
| Audio-only detection available to the screen: `detail.item.isAudiobook` | `PlayerScreen.kt:270-272` |

### 2.2 Error paths

`Controller.onPlayerError` (`:698-748`), in order: transport failover
(`isTransportPlaybackError && retryMediaOnNextNode`) → finite
`behindLiveWindow.recover(...)` → `playbackErrorAction(...)` ladder.

```kotlin
// PlaybackPolicy.kt:101-109
internal fun isTransportPlaybackError(errorCode: Int): Boolean = errorCode in setOf(
    2000, 2001, 2002, 2003,
    2004, // ERROR_CODE_IO_BAD_HTTP_STATUS — only 5xx reaches this in practice;
          // a 4xx capability answer is terminal and is filtered at the call site
    2007, 2008,
)
// PlaybackPolicy.kt:111-119
internal fun isCompatibilityPlaybackError(errorCode: Int): Boolean = errorCode in setOf(
    3001, 3003, 4001, 4002, 4003, 4004, 4005,
)
```

5001–5004 (`ERROR_CODE_AUDIO_TRACK_INIT_FAILED`, `_WRITE_FAILED`,
`_OFFLOAD_WRITE_FAILED`, `_OFFLOAD_INIT_FAILED`) are in neither set, so
`mediaCompatibilityFailure` is false and the ladder answers `Fail`
(`PlaybackPolicy.kt:77`). `clientErrorCode` maps 4000–5999 to `DECODER`
(`Controller.kt:3905`).

`retryMediaOnNextNode` (`:2280-2321`): `Session.nextMediaFailoverUrl(path)`
(`Session.kt:58-67`) walks `mediaFailoverOrigins` once per session, then
`abandonPreparedReplacement`, `setMediaItem`, `attachRecipe`, `prepare()`.
No status inspection anywhere in it.

`BehindLiveWindowRecovery` (`PlaybackPolicy.kt:230-258`): `attached()`
resets `used`; `recover(errorCode, live, seekTargetMs, seekTo, prepare)`
runs only when `behindLiveWindowRecovers(errorCode, live, used)` —
finite-only (`!live`), once per attach. `attached()` is called from
`attachRecipe` (`Controller.kt:373-382`), through which every
`setMediaItem + prepare` and the prepared commit (`:3565`) pass.
`tests/playback/web-policy.test.js:5989-6015` pins the Kotlin predicate text
`errorCode == ERROR_CODE_BEHIND_LIVE_WINDOW && !live && used < 1` and the
recovery call shape; its call count at `:6007` is the stale assertion
[RUST-TEST-EXECUTION-POLICY.md](../ci/RUST-TEST-EXECUTION-POLICY.md) §5
replaces.

Live TV: `liveTvPlaybackErrorCode` (`LiveTvPlayer.kt:481-488`) maps the
five decoder codes to `codec_unsupported`, everything else — including 1002
— to `stream_failed`, which `attach`'s listener (`:174-186`) turns into
`stopWithMessage`. The player is rebuilt per `attach` with a `mine == serial`
fence and a `LiveTvLease`.

### 2.3 The four builders

| Site | Load control | Data source | Focus / noisy | Decoder fallback | Tunneling |
|---|---|---|---|---|---|
| `Controller.buildPipeline` `:4002-4046` | `playbackLoadControl(context)` | `Net.dataSourceFactory()` (bearer) + `ProgressiveMediaOrigin` | yes / yes | yes | `isTelevision` |
| `LiveTvPlayer.attach` `:164-167` | `live = true` | `OkHttpDataSource.Factory(api.mediaClient)` | no / no | no | no |
| `LibraryChannelPlayer` `:65-68` | default | `Net.capabilityClient` (no bearer) | no / no | no | no |
| `OfflineDownloads.cacheOnlyPlayer` `:387-395` | default | `CacheDataSource` over `PlaceholderDataSource.FACTORY` | no / no | no | no |

The successor (`buildSuccessorPlayer` `:3991`) is `buildPipeline` with
volume 0 and no surface; its tunneling inherits the incumbent's on purpose
(comment at `:3975-3989`).

### 2.4 Startup and polling

`OfflineDownloads.initialize(context)` (`:112-150`): asserts main looper,
then two `runBlocking(Dispatchers.IO)` — the legacy `offlineNetwork`
preference read and the intent-recovery loop over
`OfflineRecoveryStore.intents()` — before building `SimpleCache` and the
`DownloadManager`. Called from `PlurxApp.onCreate` (`PlurxApp.kt:18`) and
`OfflineBootReceiver` (`:16`). It initialises offline **media** (the
`downloads`/`offline/media` cache), not only books (assessment D7).

`startStatusPolling` (`Controller.kt:2225-2257`): every 2 000 ms,
`vm.hlsSessionStatus(sessionId)` (capability-authenticated — `/hls/{session}/status`
takes the session capability, not the bearer), keeps the last sample on
failure, logs once. Readers of `sessionStatus`: the playback-info panel
(`PlayerScreen.kt:2099, 2277, 2609`), the wait overlay's
`http_wait_count` (`:1367`), the quality label's `target_height` (`:2044`),
and the prepared-predecessor rollback snapshot (`Controller.kt:3161, 3528,
3684`). **No** starvation detector reads it (assessment D7, unlike Apple).

---

## 3. Change

### 3.1 The lifecycle contract (M1–M3)

```
                 ON_STOP, not PiP        ON_START / PiP exit
 video  ─────────▶ owner pause ──────────────▶ resume iff intent still play
        (keepScreenOn off)                    (keepScreenOn on)
 audio  ─────────▶ keeps playing in PlaybackService (notification, wake lock)
 viewer pause ────▶ intent = false; nothing above resumes it
```

**Owner pause** (M1). A new `lifecyclePaused: Boolean` on the controller.
`setPresentationForeground(false)` when the plan is video, the activity is
not in PiP and `playbackIntent.playbackRequested` is true: set
`lifecyclePaused = true`, `player.playWhenReady = false`, report
`playback_lifecycle_pause`. `setPresentationForeground(true)`: if
`lifecyclePaused && playbackIntent.playbackRequested`, `player.playWhenReady
= true`, `lifecyclePaused = false`. The intent is never written — that is
what preserves an explicit pause: a viewer who paused from the notification
or the media session while backgrounded has `playbackRequested == false` and
nothing resumes them (F-android-3: "do not resume unconditionally on
foreground entry"). PiP: STARTED still holds in visible PiP
(`PlayerScreen.kt:1089` comment), so the observer never reports background
there. The screen passes `isInPictureInPicture(activity)` into the call so
the controller does not hold an Activity. Every attach path that re-applies
`playWhenReady = playbackIntent.playbackRequested` must AND it with
`!lifecyclePaused`; a helper `effectivePlayWhenReady()` replaces the seven
sites so none is missed.

**Audio-only service** (M2), one contract:

- `PlaybackService : MediaSessionService` in `player/`, `onGetSession`
  returns the controller's existing `MediaSession` (`:477` moves to be owned
  by the service while an audio plan is attached; video keeps the in-process
  session as today).
- Manifest: `<uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PLAYBACK"/>`,
  `<service android:name=".player.PlaybackService" android:exported="true"
  android:foregroundServiceType="mediaPlayback"><intent-filter><action
  android:name="androidx.media3.session.MediaSessionService"/></intent-filter></service>`.
  API 34+ refuses `startForeground` without the typed permission; `exported`
  is what lets the system's media controls bind.
- `DefaultMediaNotificationProvider` for the notification; `POST_NOTIFICATIONS`
  is already declared; Android 13+ needs the runtime grant, requested when
  the first audio plan starts, denied → the service still runs (Media3
  handles a missing notification permission by a silent foreground on
  U+, and on T it must still post — verify per API level in §6).
- `player.setWakeMode(C.WAKE_MODE_NETWORK)` for the audio role only: CPU +
  Wi-Fi lock while playing. It does not keep the display on (F-android-3),
  which is why M3 is separate.
- Audio-only plans skip M1's owner pause.
- Plan classification: `plan.isAudioOnly` derived from `detail.item.isAudiobook
  || item kind is music`, passed into the controller at construction, never
  inferred from tracks after `prepare()`.
- Teardown: `Controller.release()` clears the session from the service and
  `stopSelf()`; a service left running after the last player is a wake lock
  leak.

**`keepScreenOn`** (M3). Replace `keepScreenOn = true` with a listener:
`keepScreenOn = isVideo && (player.isPlaying || (player.playWhenReady &&
player.playbackState == STATE_BUFFERING))`, updated on
`onIsPlayingChanged`, `onPlaybackStateChanged`, `onPlayWhenReadyChanged`.
Paused → off within one callback; buffering while wanting playback stays on
(a stall must not dim the screen mid-recovery); audio-only never holds it
(the service's wake lock is the right primitive there).

### 3.2 Live TV `BEHIND_LIVE_WINDOW` (M4)

In `LiveTvPlayer`, a `LiveEdgeRecovery` with the same `attached()/spent`
shape as `BehindLiveWindowRecovery` but the opposite policy: live only,
`player.seekToDefaultPosition(); player.prepare()`, once per `attach`.
Before running it: `mine == serial` (the attach is current), `lease` is
still held (not expired, not drained — the heartbeat's own check), and the
session's playlist URL still answers `200` to a HEAD (a stopped or expired
session under the old capability is not the same stream — F-android-4). If
any check fails, fall through to today's `stopWithMessage`. Observer
ownership: the same `output` player, same listener — never a second
`ExoPlayer` (F-android-4). Telemetry: `live_tv_behind_live_window`
`{outcome: recovered | spent | session_gone | lease_lost}`.

The finite controller's rule is untouched; `web-policy.test.js` keeps its
`!live && used < 1` assertion on `PlaybackPolicy.kt`.

### 3.3 AudioTrack failure classification (M5)

New in `PlaybackPolicy.kt`:

```kotlin
internal enum class AudioSinkFailure { InitFailed, WriteFailed, OffloadInitFailed, None }
internal fun audioSinkFailure(errorCode: Int): AudioSinkFailure   // 5001 / 5002 / 5004; 5003 → WriteFailed
internal fun audioSinkAction(
    failure: AudioSinkFailure,
    outputDevicePresent: Boolean,      // AudioManager.getDevices(GET_DEVICES_OUTPUTS) non-empty after the error
    routeChangedSinceSnapshot: Boolean,// caps snapshot route id != current route id
    sinkRetryUsed: Boolean,
    transcodeRescueAlreadyUsed: Boolean,
): AudioSinkRecovery  // ReSnapshotAndRetry | TranscodeRescue | FailDisconnected | Fail
```

Policy, each rule with its reason:

- Capture diagnostics first, always: `error.cause` class and message,
  `AudioSink.InitializationException.audioTrackState` /
  `isRecoverable` when present, the format that failed (encoding, channels,
  sample rate), current route (`AudioDeviceInfo.type`), and whether the
  format was passthrough. Logged as `playback_audio_sink_failure` detail;
  bounded vocabulary, no free text beyond the exception message truncated to
  200 chars. "Some are route/device state, not media incompatibility"
  (F-android-5) — the diagnostics are what tells them apart after the fact.
- `!outputDevicePresent` → `FailDisconnected`: surface "Audio output
  disconnected" (a `stopped` fault per the surface contract), no transcode,
  no reopen. Transcoding for a receiver that is off is the loop the
  assessment forbids.
- `routeChangedSinceSnapshot && !sinkRetryUsed` → `ReSnapshotAndRetry`:
  `Caps.snapshot(context)` again (`Caps.kt:64`, the route-aware probe), a
  new `/decision` with the new caps, re-attach at the current position. One
  per attach — the receiver that accepted E-AC-3 is now a TV speaker that
  does not, and the server can re-plan.
- Otherwise `InitFailed` / `OffloadInitFailed` with `!transcodeRescueAlreadyUsed`
  → `TranscodeRescue`: the existing `compatibilityTranscodeUsed` budget, the
  existing path. `WriteFailed` after playback was established with the route
  unchanged → `Fail` (a dead AudioTrack mid-stream is not fixed by a new
  encode).
- Budget: `sinkRetryUsed` is per attach and reset in `attachRecipe`; the
  transcode rescue is the same one-shot flag the ladder already uses, so the
  total spend for an audio failure is at most one re-snapshot plus one
  rescue plus Fail.

Wiring: in `onPlayerError`, after failover and before `playbackErrorAction`,
`if (audioSinkFailure(code) != None)` route through `audioSinkAction`;
`ReSnapshotAndRetry` and `TranscodeRescue` call the existing helpers the
ladder's actions call, so the surface adapter sees the same fault classes.

### 3.4 Failover gated on the response code (M6)

```kotlin
/** The HTTP status Media3 attached to this error, walking the cause chain. */
internal fun httpResponseCode(error: PlaybackException): Int? =
    generateSequence(error.cause) { it.cause }
        .filterIsInstance<HttpDataSource.InvalidResponseCodeException>()
        .firstOrNull()?.responseCode

internal fun nodeFailoverEligible(errorCode: Int, responseCode: Int?): Boolean = when {
    !isTransportPlaybackError(errorCode) -> false
    responseCode == null -> true              // connection-level: another ingress may answer
    responseCode in 500..599 -> responseCode != 501 && responseCode != 505
    else -> false                              // 401/403/404/409/410/416: same answer on every node
}
```

`retryMediaOnNextNode` takes the predicate's answer; the stale comment at
`PlaybackPolicy.kt:105-106` is rewritten to name this function. 404 and 410
on a session are "session gone" and belong to the ladder's existing
handling; 401/403 are credential problems no peer fixes; 409 is a control
conflict. Trying peers for those amplifies load and hides the cause
(F-android-15). Telemetry detail on `playback_transport_failover` gains
`http_status=<n|none>`.

### 3.5 One builder, explicit roles (M7)

```kotlin
internal enum class PlayerRole { Finite, Successor, LiveTv, LibraryChannel, Offline, Audio }

@UnstableApi
internal class PlurxPlayerBuilder(private val context: Context, private val role: PlayerRole) {
    fun build(
        dataSource: DataSource.Factory,          // caller supplies: bearer / capability / profile / cache-only
        audioLanguage: String? = null,
        transferListener: TransferListener? = null,
    ): ExoPlayer
}
```

| Role | Load control | Focus + noisy | Decoder fallback | Tunneling | Wake mode | Text disabled by default |
|---|---|---|---|---|---|---|
| Finite | Incumbent | yes | yes | TV only | none | yes |
| Successor | Successor | yes | yes | TV only (inherits — comment at `:3975`) | none | yes |
| LiveTv | Live | yes | yes | **no** | none | yes |
| LibraryChannel | Incumbent | yes | yes | no | none | yes |
| Offline | Incumbent | yes | yes | no | none | yes |
| Audio | Incumbent | yes | yes | no | `WAKE_MODE_NETWORK` | yes |

Focus, becoming-noisy and decoder fallback are the shared safe defaults
(F-android-9: "centralize shared focus/noisy/decoder policy"). Tunneling
stays Finite/Successor-on-TV only: it is not safe for every subtitle overlay
and device mode, and the Live TV and channel players never asked for it.
The data source is a parameter, never chosen by role: offline **must** stay
`CacheDataSource` over `PlaceholderDataSource` with no upstream (assessment
D4 — no account-bearing upstream), library channels stay on
`Net.capabilityClient`, Live TV on `api.mediaClient`. The builder refuses
(`require`) a `PlayerRole.Offline` whose factory is not a `CacheDataSource.Factory`.

A JVM test enumerates the four construction sites by reflection-free grep
(`tests/operations` style, or a Kotlin test over source text) asserting
`ExoPlayer.Builder(` appears only inside `PlurxPlayerBuilder` — the "count
and test all construction paths" disposition.

### 3.6 Offline initialisation off the main thread (M8)

`initialize` keeps constructing `OfflineCatalog`, `OfflineRecoveryStore`,
`StandaloneDatabaseProvider`, `SimpleCache` and `DownloadManager`
synchronously (cheap, and `PlurxDownloadService` needs `manager` at bind).
The two `runBlocking` blocks become one `scope.launch(Dispatchers.IO)` whose
completion is exposed as `val recovered: Deferred<Unit>`. `withManager`,
the UI's records flow consumers and `OfflineBootReceiver`'s
`sendResumeDownloads` await `recovered` first, so a pending transfer is
never resumed against a catalogue that has not absorbed its intent
(F-android-13: "initializing only when a downloads screen opens can strand
pending downloads" — this does not do that; readiness is still process-wide,
only the wait moved). TV is not special-cased: the assessment rejected the
"TV never uses it" premise.

### 3.7 Status poll backoff with a bound (M9)

`startStatusPolling` takes a `visible: () -> Boolean` (panel open **or**
wait overlay showing **or** a prepared replacement in flight). Interval: 2 s
while visible, 10 s otherwise. The bound: a session that has been replaced
or ended is noticed at most 10 s later than today, and only the panel,
quality label and wait overlay read the result — none of them drives
recovery (§2.4 audit). The rollback snapshot keeps the last sample, whatever
its age, as today. If a future reader turns the status into recovery
evidence, it must pass `visible = true`; the function's doc comment says so.

---

## 4. Guardrails (non-goals)

- **Do not write `playbackIntent.playbackRequested` from the lifecycle.** The
  owner pause is `playWhenReady` plus `lifecyclePaused`; the intent is the
  viewer's. Writing it would turn a backgrounded app into a "user paused"
  state the stall tracker and control protocol treat differently.
- **Do not pause in PiP** and do not pause audio-only plans. PiP is visible
  playback; audio in the background is the feature M2 adds.
- **Do not replace `keepScreenOn` with the wake lock.** `WAKE_MODE_NETWORK`
  holds CPU and Wi-Fi; the display is a separate flag (F-android-3).
- **Do not change the finite `BEHIND_LIVE_WINDOW` rule** or move Live TV's
  recovery into `Controller`. The contract forbids `seekToDefaultPosition`
  on a finite timeline; Live TV has its own player and lease.
- **Do not classify 5xxx as codec incompatibility wholesale** (assessment
  D3). 5001/5002/5004 get §3.3's classification; 5003 rides with 5002;
  nothing else in 5xxx changes.
- **Do not reopen or transcode for a disconnected output** — `FailDisconnected`
  is terminal until the viewer acts.
- **Do not fail over on 4xx** and do not widen `isTransportPlaybackError`.
  The allowlist's reasoning (`PlaybackPolicy.kt:93-100`) stands; §3.4 only
  narrows 2004.
- **Do not enable tunneling, offline upstream access or a bearer data source
  by role default.** Data sources are caller-supplied; the builder's only
  data-source rule is the Offline `require`.
- **Do not lazy-initialise `OfflineDownloads` on screen open.** Readiness
  stays process-wide (§3.6).
- **Do not back the status poll off while the wait overlay or a prepared
  replacement is showing**, and do not raise the idle interval above 10 s
  without re-auditing readers.
- **No in-code feature gates.** The only switch in this plan is none: the
  lifecycle contract and the classifications are behaviour fixes. If Paul
  wants the audio service staged, it is a replicated setting surfaced in
  Settings → Developer with an advisory readiness list, per his rule.

---

## 5. Milestones

Each is a draft PR into `main` under the fast lane, with the Android
build-counter bump. JVM tests run in `make android-test`; instrumented in
`make android-instrumentation`.

### 5.1 M1 — owner pause on ON_STOP

`lifecyclePaused`, `effectivePlayWhenReady()` replacing the seven
`playWhenReady = playbackIntent.playbackRequested` sites, PiP passed in from
the screen. JVM test `PlaybackLifecycleTest`: background with intent play →
`playWhenReady` false and intent still true; foreground → true; viewer
pause while backgrounded → foreground leaves it false; PiP background → no
pause; audio-only → no pause.

Acceptance: `make android-test`; on the Pixel, play "Night Tide", press
Home: `adb shell dumpsys media_session | grep -A3 plurx` shows state PAUSED
within 1 s; return: PLAYING; pause from the notification, Home, return:
still PAUSED.

### 5.2 M2 — audio-only `MediaSessionService`

Service, manifest, notification provider, `WAKE_MODE_NETWORK` for the Audio
role, `plan.isAudioOnly`, teardown. JVM test asserts the manifest declares
the permission and the `mediaPlayback` service (`tests/operations`-style
text check is acceptable here because the manifest is the contract).

Acceptance: `make android-test`; on the Pixel (API 34+) start an audiobook,
press Home, screen off: audio continues for 5 min, `adb shell dumpsys
activity services tv.plurx.app` lists `PlaybackService` foreground with type
`mediaPlayback`; lock-screen pause/play work; stopping playback removes the
notification and the service exits.

### 5.3 M3 — `keepScreenOn` follows playback

Listener-driven flag in `PlayerScreen`. Instrumented test asserts
`playerView.keepScreenOn` is false after `player.pause()` and true after
`play()` on a video plan.

Acceptance: `make android-instrumentation`; on the Lenovo, pause a film and
wait past the device's screen timeout: the screen dims; resume: stays on.

### 5.4 M4 — Live TV live-edge recovery

`LiveEdgeRecovery`, the three validity checks, telemetry. JVM test covers
once-per-attach, `session_gone` and `lease_lost` outcomes with fakes for the
lease and the HEAD.

Acceptance: `make android-test`; on the Shield, tune a channel, suspend the
device for 2 min (past the 24-segment window), wake: the stream re-joins at
the live edge once; a second suspend beyond the lease shows the existing
"unavailable, press Watch" message, not a loop.

### 5.5 M5 — AudioTrack classification

`audioSinkFailure`, `audioSinkAction`, diagnostics event, wiring. JVM test
`AudioSinkPolicyTest` covers all four recoveries and the budget exhaustion.

Acceptance: `make android-test`; on the Shield with an AVR: start a
TrueHD/E-AC-3 title, power the AVR off mid-play → `FailDisconnected`
surface, no new session in the server's Activity page; power it on, press
Play → normal start. Unplug HDMI audio route to TV speakers while playing
E-AC-3 passthrough → one re-snapshot, one `/decision`, playback continues
with a compatible audio plan; `adb logcat -s PlurxTelemetry | grep
audio_sink` shows exactly one `ReSnapshotAndRetry`.

### 5.6 M6 — failover on the response code

`httpResponseCode`, `nodeFailoverEligible`, comment rewrite, telemetry
field. JVM test: 2004 + 503 → eligible; 2004 + 404 → not; 2001 + null →
eligible; 3001 + 500 → not.

Acceptance: `make android-test`; on the lab cluster, stop plurxd on the
ingress node mid-play (connection failure) → failover to the next origin;
release the session server-side (`DELETE /hls/{session}`) → 404 → no
failover, existing session-gone handling.

### 5.7 M7 — `PlurxPlayerBuilder`

The class, the role table, the four sites migrated, the construction-site
test. Byte-identical behaviour for `Finite`/`Successor`; the three other
roles gain focus, noisy and decoder fallback and nothing else.

Acceptance: `make android-test`; `grep -rn "ExoPlayer.Builder(" clients/android/app/src/main`
returns one line; on the Google TV, Live TV and a library channel pause
when another app takes audio focus and stop when headphones are unplugged
(phone).

### 5.8 M8 — offline recovery off the main looper

`recovered: Deferred`, awaits at the three consumers, boot receiver.

Acceptance: `make android-test` plus a JVM test that `initialize` returns
before `recovered` completes with a slow fake store; on the Pixel with three
queued downloads, force-stop, relaunch: all three resume (Downloads screen
shows progress), and `adb shell am start -W` reports a `TotalTime` at least
100 ms lower than the pre-M8 build on the same device (measured three
times each).

### 5.9 M9 — status poll backoff

`visible` predicate, 2 s / 10 s, doc comment naming the readers.

Acceptance: `make android-test` with a JVM test of the interval choice; on
the Pixel, `adb shell dumpsys netstats detail | grep tv.plurx` (or the
server's request log for `/hls/{session}/status`) shows ~6 polls/min with
the panel closed and ~30/min with it open.

---

## 6. Verification and rollout

Fast lane: `make android-test` on every PR; `make android-instrumentation`
for M3. Device work is per-milestone above; the consolidated GPT prompt:

```text
Devices: Pixel (phone), Lenovo TV, Google TV, Shield with AVR. Server: the
lab cluster. For each numbered check, capture the adb command output named
in the milestone and one line of what you saw. M1: Home/return with and
without a notification pause. M2: audiobook in background 5 min, screen off,
lock-screen controls; confirm the notification disappears when playback
stops and `dumpsys activity services` no longer lists PlaybackService. M3:
pause past screen timeout. M4: Live TV suspend/resume twice. M5: AVR off
mid-play, then route change to TV speakers. M6: stop the ingress node
mid-play, then release the session from the server's Activity page. M7:
audio focus from another app during Live TV and a library channel. M8: three
queued downloads across a force-stop. M9: status request rate with the panel
open vs closed. Report refusals as refusals; do not work around one.
```

Rollout: no switches; each milestone ships on the next mobile release to
every device. M2 is the one users notice (a notification appears); the
release note says so.

---

## 7. Open questions

1. **Music as audio-only.** `isAudiobook` exists; whether the music library
   kind reaches the same player and should take the service path needs
   confirming in `PlayerScreen`'s plan construction before M2 lands.
2. **Notification permission denied on Android 13.** Media3's behaviour
   without `POST_NOTIFICATIONS` differs by API level; M2's device check
   decides whether the service still qualifies as foreground on the Pixel's
   OS version, and whether a denied grant should refuse background audio
   with a surface message instead.
3. **Interval for M9.** 10 s is a proposal; if the panel's "age" row reads
   badly at 10 s, 5 s is the alternative and the audit still holds.
4. **Audio sink diagnostics retention.** Whether `playback_audio_sink_failure`
   detail should also reach the server's telemetry table (bounded columns)
   or stay in the client log is a telemetry-schema decision outside this
   plan.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M6 | (this branch) | Complete. `nodeFailoverEligible(errorCode, responseCode)` added to `PlaybackPolicy.kt` exactly as §3.4 specifies it; `invalidResponse` extracted in `Controller.kt` so the bounded (depth < 4) cause-chain walk `mediaRefusal` already did is shared rather than duplicated, with `httpResponseCode` reading the status off it; the `onPlayerError` gate changed from `isTransportPlaybackError(error.errorCode)` to `nodeFailoverEligible(error.errorCode, httpResponseCode(error))`; `playback_transport_failover` gained `http_status=<n\|none>`. The stale comment on 2004 — D3's "the comment describes code that does not exist" — now describes `nodeFailoverEligible`, which exists. Five JVM cases added to `PlaybackPolicyTest.kt`; **they have never been run** (no Android SDK in this session). What *was* run: three cases in `tests/playback/web-policy.test.js` pinning the predicate's four branches and their order, the call-site shape, the shared bounded walk, the telemetry field, and `PlaybackPolicy.kt`'s freedom from `androidx`. Revert proofs, each restoring a plausible-looking regression: reverting the call site to `isTransportPlaybackError(error.errorCode) && retryMediaOnNextNode` fails "onPlayerError must gate the failover on the predicate AND pass it the status"; flipping `else -> false` to `else -> true` (which restores the old behaviour while still *looking* gated) fails "anything not named above must be terminal, not a failover"; moving the allowlist branch below the status branches fails "the transport allowlist must remain the first question". Device acceptance (stop the ingress node mid-play; `DELETE /hls/{session}` then confirm no failover) is **not** done. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M6 | (this branch) | **Deviation from §3.4, flagged rather than taken silently.** The plan puts `httpResponseCode` in `PlaybackPolicy.kt`. That file's header states its functions "stay free of ExoPlayer and Android so the tables are one screen of code each, unit-testable on the JVM"; `HttpDataSource.InvalidResponseCodeException` and `PlaybackException` would end that. The split is: the decision (`nodeFailoverEligible`, pure, JVM-testable) stays in the policy module, the Media3-typed extraction lives beside the other Media3 code in `Controller.kt`. `web-policy.test.js` now asserts `PlaybackPolicy.kt` contains no `androidx.` reference, so the shortcut cannot be taken later without failing a test that runs. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1, M2, M3, M4, M5, M7, M8, M9 | — | **Not started.** Every one of them is multi-file Kotlin whose blast radius is live playback — M1 alone rewrites the seven sites that set `playWhenReady`, M7 moves all four `ExoPlayer.Builder` call sites, M2 adds a foreground service and manifest entries. This session had no Android SDK and no Gradle toolchain: none of it could be compiled, `make android-test` could not be run, and the source-text pins that make M6 defensible cannot tell a working service or lifecycle from a broken one. Landing unverifiable surgery of that size would have been worse than leaving it, so it is left. M6 was chosen because it is the one milestone that is a real correctness fix, is completable end to end without a toolchain, and is pinnable by tests that do run. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M6 | [PR #470](http://192.168.4.7:3000/noirr/plurx/pulls/470) | **Correction to the first M6 row: the Kotlin has now been compiled and run.** The sole adversarial review (PR #470 comment 4098) ran `make android-test` and `lintDebug` on nuc3; after merging main (`99d4abf8c`) it was run again: 737 tests, all 12 `LadderVerdictTest` cases pass including the five M6 cases, and the only 4 failures are the `PlaybackSurfaceReducerTest` cases that fail identically at the merge-base `fad591a46` and are not this branch. The "they have never been run" and "no Android SDK" statements above describe the first session only. The review executed a Kotlin revert proof (`else -> false` flipped to `else -> true` in `PlaybackPolicy.kt` turns `aClientErrorIsTerminalAndNeverWalksTheIngressList` and `a2xxOr3xxStatusIsNotAFailoverEither` red), so the predicate is now proved by execution, not only by source pins. The `onPlayerError` call-site gate is still pinned only by the source assertion in `web-policy.test.js` (no JVM test reaches `Controller.kt`), which the review accepted for a one-line gate. The review's one finding (P2: the M6 block in `web-policy.test.js` had split the async-drain comment from the drain) is fixed. Device acceptance (§5.6) remains post-merge work. |
| 2026-09-24 | gpt-6-astra | agent:/root/client_recon | M7 | [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) · `78830ec99` | The six-role `PlurxPlayerBuilder` now owns construction at the four existing player sites. Finite and successor keep their source, transfer listener and TV tunneling; Live TV keeps its live load control and media client; the library channel keeps the capability client; offline keeps its cache-only placeholder upstream. Focus, becoming-noisy and decoder fallback follow §3.5's role table. `:app:compileDebugKotlin` passed in the pinned Android build image. The construction-site contract was added but the unit lane and device acceptance wait for the ready PR. M1-M5 and M8-M9 remain open. |
| 2026-09-24 | gpt-6-astra | agent:/root/client_recon | M5 AudioTrack classification | [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) · `38ece806e` | Audio-sink failures now use a bounded cause-chain policy and route snapshot to distinguish a changed output route from a persistent decoder or transport failure; controller recovery and diagnostics follow that classification. Production and test Kotlin compilation passed in the pinned Android image. Unit tests, the one adversarial review and device acceptance remain for the ready PR. M1 reached the plan's prepared-commit stop-and-flag boundary and is paused pending the narrow lifecycle-intent decision. |
