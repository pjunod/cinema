# Playback surface contract — implementation plan

**Status:** ready to build · **Executes:** contract v2,
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (§9 rulings of
2026-09-13) · **Written:** 2026-09-13 against `main` @ `a124876` ·
**Answers:** the acceptance gaps in
[PLAYBACK-SURFACE-CONTRACT-REVIEW.md](PLAYBACK-SURFACE-CONTRACT-REVIEW.md)
finding 10

Read the contract first — §3.0 (two owners), §3.1 (classes), §3.3 (source
precedence), §3.4 (evidence) — then this document top to bottom, then build
milestone by milestone in the order of §4. Each milestone is one PR and ends
with an acceptance check that is a command or an observable fact. Do not
start M1–M3 until M0 has merged; they are independent of each other after
that and may run in parallel sessions under the ownership rules in §8.

The standing instruction: **if a step seems to require changing a recovery
ladder, a budget, a threshold, a detector, or the control-plane verdict
semantics anywhere outside M5, stop and flag it instead of doing it.** The
surface PRs (M0–M4) are behaviour-neutral by ruling; the only thing they may
change about what the player *does* is the one obligation in §3.4 below.
Every line number here is from `a124876` and will have drifted — re-verify
each anchor by function name before editing.

The PR lifecycle, per [`DEVELOPMENT_PIPELINE.md`](../DEVELOPMENT_PIPELINE.md)
and the standing rulings: proper commits · the fast local lane only until the
PR is together (`make validate-staged`, `make web-check`, the client's unit
suite) · open as `WIP:` · get an adversarial review · implement the findings ·
run the full suite **once** · watch it and fix until green · un-WIP and merge
it yourself. Never open a PR early; a PR opened before it is together burns a
full CI fan-out for nothing.

---

## 1. Objective

Replace the three imperative error channels — web `setLoading()`, Apple
`failed`/`playbackError`/`playbackFailureTitle`/`playbackNotice`, Android
`onError`/`playFailure`/`playbackNotice` — with one fixture-driven presenter
per client that renders a surface from typed faults and the player's own
presentation evidence, such that:

1. a blocking surface (`exhausted`, `stopped`) is only ever rendered over a
   player its recovery owner has already stopped, and the fixture refuses
   any other blocking fault;
2. a fault about the attached media is retired by that media presenting,
   and a fault about a pending destination is retired only by that
   destination settling or being superseded;
3. every surface change is attributable from inside the product (ledger
   section + client-log events) and no shipped file outside the presenter's
   render can write a surface (fence).

Done means: all six milestones merged, the physical verification recorded,
and `surface_disagreement` either absent or explained per row.

---

## 2. Repository facts the build depends on

Re-verify at build time. Each row names the function so the anchor survives
drift.

| Fact | Where (`a124876`) |
|---|---|
| Web renders the overlay from `setLoading(on,text,sub,actionHtml)`; 35 call sites; `failed` class added at four; `playerInputState()` reads `classList.contains("failed")` | `crates/plurxd/src/web/index.html` `setLoading` ~11006, `playerInputState` ~11056 |
| Web recovery owner sites that are *exhausted*: `showStallRecoveryFailure` (buttons + `failed`), the `stallRecoveryAction === 'prompt'` branch of `persistentWait`, `stallDiagnose` verdicts | `showStallRecoveryFailure` ~4077, `persistentWait` ~3889, `stallDiagnose` ~10930 |
| Web progress evidence | `playbackProgressTick` ~11224 (`moved` = clock **and** frame advance when a counter exists); `samplePlaybackPresentationClock` |
| Web pause that keeps `wantsPlayback`: `pausePlaybackInternally(v)`; timer cancellation: `stopPlayerTimers` | ~10528, ~10565 |
| Web refusal parse: `PlaybackPolicy.parseStreamFailure({status, body}) → {status, code, message} \| null`; overlay text: `streamFailureOverlay(failure, now)` | `crates/plurxd/src/web/playback-policy.js` ~689, ~732 |
| Web policy module shape: IIFE, `module.exports = policy`, `root.PlurxPlaybackPolicy`; the input contract is embedded as generated constants (`INPUT_ROUTING`, `CONTRACT_TIMINGS`, …) by `scripts/player-contract-table` | `playback-policy.js:1-58` |
| Web test harness slices shipped functions by name from `index.html`: `sliceDeclaration(start)` in `tests/playback/web-policy.test.js` (`SHIPPED_UI`, `TERMINATORS`) | `web-policy.test.js:13-56` |
| Web client log: `clientLog(ev)` POSTs `{ua, method, title, file_id, vcodec, …ev, control}` to `/client-log` | `index.html:3574` |
| Apple fields: `@Published private(set) var failed / playbackError / playbackFailureTitle / playbackNotice / finished` | `PlayerController.swift:1760-1764` |
| Apple `fail(_ error: Error)` — the only blocking writer that does not pause | ~4830 |
| Apple blocking writers that pause first: `handleItemFailure`, stall `.stop`, server `terminal`, repeated early end, HDR rung | ~5309, ~4327, ~4458, ~5101, ~6011 |
| Apple readiness: `awaitItemReady` (15 s), `seekWhenReady`, `shouldBoundFreshStartReadiness(isVOD:startMs:seeksAfterAttach:resumesPlayback:)`, `retryAfterReadinessTimeout` | ~6147, ~6166, ~1625, ~6076 |
| Apple Keep-waiting primitive: `retryAfterPlaybackFailure()` resets `recoveryReopenBudget` only | ~2447 |
| Apple lock-screen play target calls `player.play()` directly | ~6969 |
| Apple periodic observer: `makePeriodicPlaybackObservation()` | ~4985 |
| Apple API: `PlurxAPI.check(_ resp: URLResponse, data: Data?)` decodes `{code,message}` only for 409 → `APIError.conflict(code:message:)`; else `APIError.http(status)`; `AppModel.isSessionExpired` matches `.http(401|403)` | `PlurxAPI.swift:192-205`, `AppModel.swift:288` |
| Apple notices: `showPlaybackNotice(_ message: String, duration: Duration = .seconds(5))` | ~2800 |
| Apple ledger types: `PlaybackLedgerRow {section,label,value,tone,placement}`, `PlaybackLedgerSection {id,title,column,placeholder,keepsNotesInBox}` | `PlayerView.swift:2542`, `:2581` |
| Apple test resources are wired in `clients/apple/project.yml` (`plurx-iOSTests` / tvOS twins): `../../tests/playback/*.json` with `buildPhase: resources`; decoded in `Tests/AppleClientTests.swift:53-128` | `project.yml:113-122` |
| Android: `class Controller(…, private val onError: (String) -> Unit = {})`; `var playbackNotice: String? by mutableStateOf(null)` | `Controller.kt:118-130`, `:209` |
| Android exhausted `onError` sites: sessionless second stall, budget exhausted, target-deadline terminal | `Controller.kt` ~1444, ~1457, ~2174 |
| Android other `onError` sites (not exhausted; become `stopped`/`refused` per §3.3): `onPlayerError` `Fail` ~653, create failed ~1281, terminal verdict ~1337, stall reopen create failed ~1544 | |
| Android screen state: `var playFailure by remember { mutableStateOf<String?>(null) }`; `PlaybackFailed(message, onRetry, onExit)` | `PlayerScreen.kt:674`, `:621` |
| Android stall tracker resets when `playbackRequested` is false (`playWhenReady = false`) | `PlaybackTelemetry.kt:300-312` |
| Android evidence: `PlaybackIntent.presentedVideoFrame(positionMs, sequence)`, `Controller.realPosition()`, `mediaMutationEpoch` | `PlaybackIntent.kt:143`, `Controller.kt:858` |
| Android telemetry: `PlaybackTelemetry.report(event, level, message, code?, detail?, ms?, attempt?)` | `PlaybackTelemetry.kt:170` |
| Android test resources: `build.gradle.kts` adds `../../../tests/contracts` and `../../../tests/playback` to the `test` source set; tests read via `javaClass.classLoader?.getResource("player-input-contract.json")` | `build.gradle.kts:80-84`, `PlayerInputPolicyTest.kt:15` |
| Fixture pattern: `tests/playback/player-input-contract.json` (`schema_version`, `surfaces`, `timings`, `states`, `outcomes`, `routing`, …) rendered into the doc by `scripts/player-contract-table` between `<!-- contract:routing:begin/end -->` markers; `player-input-contract.test.js` asserts doc ↔ fixture ↔ policy | `scripts/player-contract-table:20-37` |
| Fence pattern: `scripts/player-input-fence` (python3) with `WHOLE_FILE_ALLOWLIST`, `TOKEN_FILE_ALLOWLIST`, `INDEX_REGIONS` (begin/end comment anchors in `index.html`), test trees skipped; registered as a `[[checks]]` in `validation/points.toml` (`id = "player-input-fence"`, profiles `commit, ci, full, nightly`) and listed in the client points' `checks` | `validation/points.toml:79-86, 669-676, 804-814, 845-853` |
| Ledger fixture: `tests/playback/playback-info-fields.json` with `sections: ["PLAYBACK","SOURCE","NOW DECODING","BUFFERING / DELIVERY","NETWORK","SERVER"]` and `fields[] {id,label,section,modes,format,note,available_on?}` | |
| Docs index: every new doc needs a row in `docs/README.md` in the same commit; `tests/operations/test_docs_index.py` also refuses any `docs/…md` path in the repo that does not exist | |
| Mobile version gate: any shipped Swift/Kotlin change needs `make apple-build-bump` / the Android counter bump; comment-only changes are exempt (`validation/mobile_versions.py`) | |
| Server refusal codes and statuses for VOD session create | `crates/plurxd/src/http/hls.rs:3755-3785` |

---

## 3. Contract — exact interfaces

### 3.1 The fixture: `tests/playback/playback-surface-contract.json`

```json
{
  "schema_version": 1,
  "title": "Playback surface contract",
  "documented_in": "docs/clients/PLAYBACK-SURFACE-CONTRACT.md",
  "ruled": "2026-09-13",

  "classes": {
    "preparing":  {"severity": "progress", "blocking": "while_not_presenting",
                   "retired_by": ["presenting", "intent_settled", "attached_retired"]},
    "buffering":  {"severity": "progress", "blocking": "while_not_presenting",
                   "min_ms": 350, "retired_by": ["presenting", "attached_retired"]},
    "recovering": {"severity": "progress", "blocking": "while_not_presenting",
                   "retired_by": ["presenting_new_attached", "owner_success", "attached_retired"]},
    "hold":       {"severity": "notice", "timed_ms": 30000,
                   "retired_by": ["timer", "presenting"]},
    "degraded":   {"severity": "notice", "timed_ms": 5000,
                   "retired_by": ["timer"], "timer_paused_while_actions": true},
    "refused":    {"severity": "notice", "timed_ms": null,
                   "retired_by": ["intent_superseded", "presenting_continuous_ms"],
                   "default_actions": ["retry"]},
    "exhausted":  {"severity": "prompt", "blocking": true,
                   "requires_player_stopped": true, "retired_by": ["user"],
                   "title": "Playback is stalled.",
                   "default_actions": ["keep_waiting", "retry", "close"]},
    "stopped":    {"severity": "terminal", "blocking": true,
                   "requires_player_stopped": true, "retired_by": ["user"],
                   "default_actions": ["retry", "close"]}
  },

  "actions": ["keep_waiting", "retry", "close", "force_transcode", "sign_in"],

  "timings": {
    "buffering_min_ms": 350,
    "hold_notice_ms": 30000,
    "degraded_notice_ms": 5000,
    "refused_progress_ms": 10000,
    "notes": ["refused_progress_ms is CONTINUOUS presenting on the attached generation, not accumulated playback."]
  },

  "contexts": ["start", "attached", "change"],

  "sources": [
    {"id": "owner_stopped",                "context": "any",     "class": "stopped",
     "requires": {"player_stopped": true}},
    {"id": "owner_exhausted",              "context": "any",     "class": "exhausted",
     "requires": {"player_stopped": true}},
    {"id": "auth_401_403",                 "context": "any",     "class": "stopped",
     "actions": ["sign_in", "close"]},
    {"id": "vod_source_rescan_required",   "context": "start",   "class": "stopped"},
    {"id": "vod_source_unsupported",       "context": "start",   "class": "stopped"},
    {"id": "vod_transcode_unavailable",    "context": "start",   "class": "stopped"},
    {"id": "vod_subtitle_burn_unavailable","context": "start",   "class": "stopped"},
    {"id": "vod_disabled",                 "context": "start",   "class": "stopped"},
    {"id": "create_503_not_yet",           "context": "start",   "class": "preparing",
     "codes": ["startup_timeout", "media_owner_transition", "vod_index_pending", "vod_engine_unattested"],
     "retryable": true},
    {"id": "change_failed",                "context": "change",  "class": "refused",
     "actions": ["retry"]},
    {"id": "segment_503_not_yet",          "context": "attached","class": "recovering"},
    {"id": "media_owner_lost_410",         "context": "attached","class": "recovering",
     "then_when_stopped": "stopped", "carries": ["position_ms"]},
    {"id": "control_hold",                 "context": "attached","class": "hold"},
    {"id": "media_waiting",                "context": "attached","class": "buffering"},
    {"id": "owner_recovery_step",          "context": "any",     "class": "recovering"},
    {"id": "readiness_deadline_rungs_left","context": "any",     "class": "recovering"},
    {"id": "decoder_failed",               "context": "any",     "class": "stopped",
     "requires": {"player_stopped": true}},
    {"id": "black_frame_ladder_spent",     "context": "start",   "class": "exhausted",
     "requires": {"player_stopped": true}, "actions": ["close", "retry"]},
    {"id": "repeated_early_end",           "context": "attached","class": "stopped",
     "requires": {"player_stopped": true}},
    {"id": "degraded_notice",              "context": "any",     "class": "degraded"},
    {"id": "log_only",                     "context": "any",     "class": null}
  ],

  "cases": [ … see §3.2 … ]
}
```

Rules the fixture encodes and every client test must enforce:

- `sources` are evaluated **in order**; the first row whose `context`
  matches (`any` matches all) wins. `context` is supplied by the adapter:
  `start` before first presentation on this playback, `change` while a
  viewer-requested replacement is pending and a predecessor is attached,
  `attached` otherwise.
- A row with `requires.player_stopped: true` raised with
  `player_stopped: false` is a **fixture error** (`blocking_without_stop`),
  never a surface. The reducer returns the error in its log and leaves the
  surface unchanged.
- `then_when_stopped` (row `media_owner_lost_410`): the fault re-classes to
  `stopped` on the owner's `stopped` event carrying the same `attached`.
- `class: null` produces no surface and one `surface_log_only` log entry.

### 3.2 Cases — ordered event sequences

```json
{
  "name": "…",
  "events": [
    {"t": 0,     "attach": "g1"},
    {"t": 0,     "presenting": true,  "attached": "g1"},
    {"t": 1000,  "raise": "<source id>", "attached": "g1", "intent": "i7",
                 "player_stopped": true, "actions": ["retry"], "position_ms": 90000,
                 "context": "change"},
    {"t": 2000,  "presenting": false, "attached": "g1"},
    {"t": 2500,  "event": "canplay"},
    {"t": 3000,  "intent_settled": "i7"},
    {"t": 3000,  "intent_superseded": "i7"},
    {"t": 4000,  "retire": "g1"},
    {"t": 4000,  "owner_success": "g2"},
    {"t": 5000,  "user_action": "keep_waiting"},
    {"t": 6000,  "hidden": true},
    {"t": 9000,  "tick": true}
  ],
  "expect": [
    {"at": 1000, "surface": "banner|indicator|blocking|none", "class": "refused",
                 "actions": ["retry"], "log": ["surface_raised"]},
    {"at": 1000, "error": "blocking_without_stop"}
  ]
}
```

Event vocabulary (all timestamps in ms from the case start; events at the
same `t` apply in listed order):

| Event | Meaning for the reducer |
|---|---|
| `attach: G` | media generation G becomes attached; earlier attached generation is retired |
| `retire: G` | generation G retired (faults with `attached: G` are dropped, logged `surface_cleared {by: attached_retired}`) |
| `presenting: bool, attached: G` | an evidence sample from the client's presentation proof for G — this is the **only** event that may retire a `presenting`-retired class or trigger a disagreement |
| `event: canplay \| playing \| isPlayingChanged \| timeControlStatus` | an inert platform event; the reducer must not change the surface (the fixture asserts it) |
| `raise: <source>` (+ `attached`, `intent?`, `player_stopped?`, `actions?`, `position_ms?`, `context`) | a fault from the adapter/owner |
| `intent_settled: I` / `intent_superseded: I` | the destination I landed / was replaced |
| `owner_success: G` | the owner reports its recovery produced attached G (retires `recovering`) |
| `user_action: a` | the viewer pressed an action; the reducer clears the fault the action belonged to and logs `surface_cleared {by: user, action: a}` — the *effect* of the action is the owner's, outside the reducer |
| `hidden: bool` | the page/app is hidden; while hidden, `presenting` samples are ignored |
| `tick` | timers advance to `t` (timed notices expire) |

`expect` entries assert the surface **after** all events at `at` have been
applied. `surface` ∈ `none · indicator · banner · blocking`. `log` lists
event names emitted at that time, in order.

The M0 fixture ships with at least these cases, each named for the defect
it pins: `web_fatal_over_live_buffer`, `web_failed_change_sticky`,
`web_stale_refusal_across_attach`, `web_safari_boundary_waiting`,
`apple_readiness_rungs_left_is_recovering`, `apple_lock_screen_play_under_stop`,
`apple_black_frame_exhausted`, `android_watchdog_restart_under_stop`,
`android_fail_without_stop_is_fixture_error`, `refused_survives_predecessor_progress`,
`refused_retired_by_continuous_progress`, `degraded_with_actions_does_not_expire`,
`hidden_page_freezes_surface`, `media_owner_lost_plays_out_then_stops`,
`terminal_wording_transport_exception`, `canplay_is_inert`,
`isPlayingChanged_is_inert`. Every `cases` row is run by all three clients;
a client that cannot express an event (Android has no `hidden` page — it has
`presentationForeground`) maps it to its own equivalent and says so in a
comment beside the mapping, never by skipping the row.

### 3.3 Reducer signatures

**Web** — `crates/plurxd/src/web/playback-policy.js`, exported beside
`routeInput`:

```js
// state: { faults: Fault[], attached: string|null, hidden: boolean,
//          presentingSince: number|null, now: number }
// event: one of the fixture's event objects (§3.2) with `t` as `now`
// returns: { state, surface: {kind, class, title, detail, actions, fault}, log: [...] }
function presentSurface(state, event) { … }
function initialSurfaceState() { … }
```

`presentSurface` is pure: no DOM, no timers, no `PLAYER`. Timers are the
caller's `tick` events. The render function in `index.html`,
`renderPlaybackSurface(surface)`, is the **only** code that calls
`setLoading`, toggles the `failed` class, or writes `#ploadAct`; it lives
between `// playback-surface-render:begin` and
`// playback-surface-render:end` anchors so the fence can allow it.

**Apple** — `clients/apple/Sources/PlaybackSurfaceModel.swift`:

```swift
struct PlaybackFault: Equatable, Sendable {
    enum Class: String, Codable { case preparing, buffering, recovering, hold, degraded, refused, exhausted, stopped }
    enum Action: String, Codable { case keepWaiting = "keep_waiting", retry, close, forceTranscode = "force_transcode", signIn = "sign_in" }
    let cls: Class
    let source: String
    let attached: Int            // openGeneration
    let intent: Int?             // viewerActionEpoch / seek generation
    let raisedAt: ContinuousClock.Instant
    var positionMs: Int?
    var title: String?
    var detail: String?
    var actions: [Action]
    var playerStopped: Bool
}

struct PlaybackSurface: Equatable {
    enum Kind { case none, indicator, banner, blocking }
    let kind: Kind
    let fault: PlaybackFault?
}

struct PlaybackSurfaceModel: Equatable {
    private(set) var faults: [PlaybackFault]
    private(set) var attached: Int?
    private(set) var hidden: Bool
    private(set) var presentingSince: ContinuousClock.Instant?
    private(set) var surface: PlaybackSurface

    enum Event { case attach(Int), retire(Int), presenting(Bool, attached: Int),
                 inert(String), raise(PlaybackFault, context: Context),
                 intentSettled(Int), intentSuperseded(Int), ownerSuccess(Int),
                 userAction(PlaybackFault.Action), hidden(Bool), tick }
    enum Context: String { case start, attached, change }

    /// Pure. Returns the log entries the caller must emit (client log + ledger ring).
    mutating func apply(_ event: Event, now: ContinuousClock.Instant) -> [PlaybackSurfaceLog]
}
```

`PlayerController` publishes exactly one field for this:
`@Published private(set) var surface = PlaybackSurfaceModel()`; `failed`,
`playbackError`, `playbackFailureTitle` and `playbackNotice` are deleted
(not deprecated — the fence would allow a deprecated field to keep being
written). `PlayerView.failureView` and the banner read `controller.surface`.
`PlayerInputRouting`'s `.failed` state is `surface.surface.kind == .blocking`.

**Android** — `clients/android/app/src/main/java/tv/plurx/app/player/PlaybackSurface.kt`:

```kotlin
internal data class PlaybackFault(
    val cls: SurfaceClass, val source: String,
    val attached: Long,               // mediaMutationEpoch
    val intent: Long?,                // PlaybackIntent sequence
    val raisedAtMs: Long,
    val positionMs: Long? = null,
    val title: String? = null, val detail: String? = null,
    val actions: List<SurfaceAction> = emptyList(),
    val playerStopped: Boolean = false,
)
internal enum class SurfaceClass { Preparing, Buffering, Recovering, Hold, Degraded, Refused, Exhausted, Stopped }
internal enum class SurfaceAction { KeepWaiting, Retry, Close, ForceTranscode, SignIn }
internal sealed interface PlaybackSurface {
    data object None : PlaybackSurface
    data class Indicator(val fault: PlaybackFault) : PlaybackSurface
    data class Banner(val fault: PlaybackFault) : PlaybackSurface
    data class Blocking(val fault: PlaybackFault) : PlaybackSurface
}
internal sealed interface SurfaceEvent { /* mirrors §3.2 */ }

/** Pure. */
internal class PlaybackSurfaceReducer {
    fun apply(state: SurfaceState, event: SurfaceEvent, nowMs: Long): SurfaceStep
}
internal data class SurfaceStep(val state: SurfaceState, val surface: PlaybackSurface, val log: List<SurfaceLog>)
```

`Controller` exposes `val surface: StateFlow<PlaybackSurface>` and drops the
`onError: (String) -> Unit` constructor parameter and `playbackNotice`.
`PlayerScreen` collects `surface`; `playFailure` is deleted; `PlaybackFailed`
takes the `PlaybackFault` and gets `.background(Color.Black)`;
`PlayerInputState.Failed` is `surface is PlaybackSurface.Blocking`.

### 3.4 The recovery owner's one obligation — exact sites

At each site below, the owner **stops the player, then raises** a blocking
fault with `playerStopped = true`. Nothing else at the site changes.

| Client | Site | Stop with | Raise |
|---|---|---|---|
| Web | `showStallRecoveryFailure(detail)` | `pausePlaybackInternally(v); stopPlayerTimers(p)` (already exists; keeps `wantsPlayback`) | `exhausted`, actions `keep_waiting, retry, close` (+ `force_transcode` when the button is shown today) |
| Web | `persistentWait` — `stallRecoveryAction === 'prompt'` branch | same | `exhausted` |
| Web | `stallDiagnose` terminal verdicts ("won't play", "looks blocked", "couldn't build") | same | `stopped` |
| Web | hls.js fatal / `<video>` `error` with the ladder spent (`triedFallback` already true, `recoveringStall` failed) | same | `stopped` |
| Apple | `fail(_:)` when the error is `PlaybackPreparationError` *and* `retryAfterReadinessTimeout` has no rung left | `player.pause()` before setting the model | `exhausted` (item attached) / `stopped` (no item) |
| Apple | the five sites that already `player.pause()` first | unchanged | `stopped` / `exhausted` as today's semantics dictate (stall `.stop` → `exhausted`; the others `stopped`) |
| Apple | `handleBlackFrameDecodeFailure` with `fallback == .none` | `player.pause()` — **new**: today it does nothing | `exhausted`, actions `retry, close` (row 15) |
| Android | `onStall` sessionless second stall (`~1444`) | `player.playWhenReady = false` | `exhausted` |
| Android | `onStall` budget exhausted (`~1457`) | same | `exhausted` |
| Android | target-deadline `event.terminal` (`~2174`) | same | `exhausted` |
| Android | `onPlayerError` → `Fail` (`~653`) | same | `stopped` (with the transport-class wording exception kept) |
| Android | create failed at start (`~1281`, no predecessor) / stall reopen create failed (`~1544`) | same when no predecessor is **presenting**; when a predecessor is presenting this is context `change` → `refused`, no stop | `stopped` / `refused` |
| Android | terminal verdict `applyStallVerdict` (`~1337`) | `player.playWhenReady = false`, and the branch still returns `true` so no reopen starts and no budget is spent | `stopped`, with the verdict's message |

**Amended 2026-09-13, during M3, in Paul's absence.** Two of the Android rows
above were wrong about the code, and both were found by the adversarial review
of #279:

1. **The terminal verdict row said "no stop, no fault; wording consumed by the
   later `stopped`".** There is no later `stopped`. The `"terminal"` branch
   returns before `openStallTracker.reset()`, so the tracker stays latched with
   `fired = true` on a playhead that will never move; `playWhenReady` is still
   true, so the tracker's own `!playbackRequested` escape never fires;
   `sampleTargetPresentationDeadline` needs a `pendingSeek` that a mid-film
   stall does not have; and `onPlayerError` never comes, because ExoPlayer is
   buffering rather than failing. Followed literally the row produced a frozen
   picture with **no surface at all**, where `main` gave the viewer a
   full-screen overlay with Retry — a user-visible regression, not a latent
   gap. The owner has nothing left to try at that point, so it stops and says
   so in the server's words. D1 is untouched: the branch still returns `true`,
   which is what keeps the verdict from spending a budget or starting a reopen.
2. **The create-failure row said "when a predecessor *is* attached".** At the
   stall-reopen site an attached item is exactly what a stalled player still
   has — it holds its item and sits in `STATE_BUFFERING` — so "attached" made
   "recovery failed" a passive banner over a **frozen** picture, with
   `intent = null`, which `intent_superseded` can never match and which ten
   seconds of continuous presentation will never reach. The question the row
   means to ask is whether there is a *picture* behind the failure, and the
   client's own presentation evidence already answers it. At the start site the
   two readings agree, because nothing was ever set on the player.

Setting `playWhenReady = false` on Android resets the open-stall tracker
(`PlaybackTelemetry.kt:300-312`), which is what closes the
restart-under-overlay path; do not cancel `stallWatchdogJob` — it is the
owner's detector and Keep waiting needs it.

### 3.5 Adapters — reading refusals

**Web:** `PlaybackPolicy.parseStreamFailure({status, body})` returns
`{status, code, message, position_ms}` where `position_ms` is
`Number(parsed.film_position_ms)` when present and finite, else `null`. The
hls.js `xhrSetup` capture is unchanged; the adapter maps `{status, code}` to
a source id via the fixture (`create_503_not_yet` codes list,
`media_owner_lost_410` on 410, the VOD codes on 409/422/501/503, `auth_401_403`).

**Apple:** `PlurxAPI.check` becomes:

```swift
enum APIError: Error {
    case http(Int)                                  // bodiless or unparseable — KEEP; auth matching depends on it
    case conflict(code: String, message: String)    // KEEP for callers that match it
    case refused(status: Int, code: String, message: String, positionMs: Int?)
    case transport(String)
    …
}
static func check(_ resp: URLResponse, data: Data? = nil) throws {
    guard let http = resp as? HTTPURLResponse, !(200..<300).contains(http.statusCode) else { return }
    if http.statusCode == 401 || http.statusCode == 403 { throw APIError.http(http.statusCode) } // auth stays status-based
    if let data, data.count <= 16_384,
       let detail = try? JSONDecoder().decode(Refusal.self, from: data),
       !detail.code.isEmpty, !detail.message.isEmpty {
        if http.statusCode == 409 { throw APIError.conflict(code: …, message: …) } // existing matchers
        throw APIError.refused(status: http.statusCode, code: String(detail.code.prefix(80)),
                               message: String(detail.message.prefix(512)), positionMs: detail.film_position_ms)
    }
    throw APIError.http(http.statusCode)
}
```

with
`struct Refusal: Decodable { let code: String; let message: String; let film_position_ms: Int? }`.
`AppModel.isSessionExpired` is unchanged and a test pins that a 401 **with**
a JSON body still matches it. Media (playlist/segment) refusals reach the
Apple adapter only as `AVPlayerItemErrorLogEvent.errorStatusCode`; the
adapter classifies 503 → `segment_503_not_yet`, 410 → `media_owner_lost_410`
(position from `realPositionMs()`), 401/403 → `auth_401_403`, and consults
the last `startStatusPolling` snapshot for the code when one is present. It
must not invent a `message` for a status it only has a number for.

**Android:** `AppViewModel.createHlsSession` maps a non-2xx to a
`RefusalException(status, code, message, positionMs)` by decoding the body
with the existing JSON instance (fallback: `HttpException(status)`).
Playlist/segment refusals: in `onPlayerError`, unwrap
`error.cause as? HttpDataSource.InvalidResponseCodeException` and decode
`responseBody` the same way; classify as above. `PlaybackPolicy.playbackErrorAction`
is unchanged — the adapter runs *after* it, on the outcome.

### 3.6 Ledger and log

Add to `tests/playback/playback-info-fields.json`: section `"SURFACE"`, fields

```json
{ "id": "surface_kind",    "label": "Surface",       "section": "SURFACE", "modes": ["standard","debug"], "format": "text" },
{ "id": "surface_class",   "label": "Fault",         "section": "SURFACE", "modes": ["standard","debug"], "format": "text" },
{ "id": "surface_source",  "label": "Source",        "section": "SURFACE", "modes": ["debug"], "format": "text" },
{ "id": "surface_ids",     "label": "Attached/intent","section": "SURFACE", "modes": ["debug"], "format": "text" },
{ "id": "surface_history", "label": "History",       "section": "SURFACE", "modes": ["debug"], "format": "text",
  "note": "Last 16 faults: class · source · raised → cleared (by) · player at raise (rate, position, presenting, stopped_by_owner)." }
```

`scripts/player-contract-table` renders the info table; regenerate
`PLAYER-INPUT-CONTRACT.md`'s info block in the same commit.

Client-log events (all three clients, same field names):

```
surface_raised       {class, source, attached, intent, session_id, attempt, position_ms, actions, player_stopped, rate, presenting}
surface_cleared      {class, source, attached, intent, session_id, attempt, by: presenting|intent_settled|intent_superseded|attached_retired|timer|user|owner_success, action?}
surface_disagreement {class, source, attached, intent, session_id, attempt, position_ms}
surface_log_only     {source, attached, session_id, attempt}
```

`session_id` is the HLS session id when one exists, else `null`; `attempt` is
the client's playback attempt id (web `openAttempt`, Apple `openGeneration`,
Android `PlaybackAttempt`).

### 3.7 The fence: `scripts/playback-surface-fence`

Python 3, same skeleton as `scripts/player-input-fence`. Registered in
`validation/points.toml` as `id = "playback-surface-fence"`, profiles
`commit, ci, full, nightly`, and added to the `checks` of the same points
that list `player-input-fence`.

Scanned files (shipped only; test trees skipped): `crates/plurxd/src/web/index.html`,
`crates/plurxd/src/web/playback-policy.js`, `clients/apple/Sources/PlayerController.swift`,
`clients/apple/Sources/PlayerView.swift`, `clients/apple/Sources/PlayerSurface.swift`,
`clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt`,
`clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt`.
Explicitly **not** scanned: `LibraryChannels.swift`, `OfflinePlayerScreen.kt`,
`OfflineDownloadManager.swift`, `reader.js`, `offline-reader.js`, `live-tv.js`.

Write patterns (regex, per file kind):

| Kind | Trips on |
|---|---|
| web | `\bsetLoading\s*\(`, `classList\.(add\|remove\|toggle)\([^)]*["']failed["']`, `\bstallPrompt\s*=[^=]`, `ploadText\|ploadSub\|ploadAct` property writes (`\.(textContent\|innerHTML)\s*=`) |
| swift | `^\s*(self\.)?(failed\|playbackError\|playbackFailureTitle\|playbackNotice)\s*(=\|\+=)[^=]` |
| kotlin | `\bonError\s*\(`, `^\s*(playFailure\|playbackNotice)\s*(=\|\+=)[^=]` |

Allowed regions: the web render function between
`// playback-surface-render:begin` / `end`; Swift inside
`PlaybackSurfaceModel.swift` (whole-file allow, with reason) and the single
`surface = …` assignment in `PlayerController`; Kotlin inside
`PlaybackSurface.kt` and the single `_surface.value = …` in `Controller`.
Reads are never scanned. Fixtures: `tests/operations/fence-fixtures/surface/`
holds one file per kind that **must trip** and one that **must not** (a read
of `playFailure`, a `setLoading` inside the render anchors, an `errorCode`
comparison in a ladder), and `tests/operations/test_playback_surface_fence.py`
runs the script against both sets.

---

## 4. Milestones

Branch names are suggestions; the WIP title prefix is not. Each milestone's
PR description links this document and names its milestone.

### 4.1 M0 — fixture, reference reducer, doc rendering, fence (`docs/playback-surface-m0`)

1. Write `tests/playback/playback-surface-contract.json` per §3.1–3.2 with
   the named cases.
2. Write `tests/playback/playback-surface-contract.test.js`: a reference
   reducer in plain JS (it becomes the web reducer in M1 — write it in
   `playback-policy.js` now as `presentSurface`, unused by `index.html`
   until M1), run every case, assert every `expect`, and assert the fixture
   invariants (every source class exists; blocking classes require
   `player_stopped`; every action is in `actions`; case names unique).
3. Extend `scripts/player-contract-table` to render the classes and sources
   tables into `PLAYBACK-SURFACE-CONTRACT.md` between
   `<!-- contract:surface-classes:begin/end -->` and
   `<!-- contract:surface-sources:begin/end -->`, and to embed the fixture's
   `classes`/`sources`/`timings` into `playback-policy.js` as generated
   constants (`SURFACE_CLASSES`, `SURFACE_SOURCES`, `SURFACE_TIMINGS`) the
   way it embeds `INPUT_ROUTING`. Extend `player-input-contract.test.js`'s
   doc ↔ fixture ↔ policy check to the new blocks.
4. Add the SURFACE section to `playback-info-fields.json` (§3.6) and
   regenerate the info block.
5. Write `scripts/playback-surface-fence` (§3.7) with an allow list covering
   **every current write site** (with the reason "M1/M2/M3 remove this"),
   the `[[checks]]` registration, the fixtures and their test.
6. Hook `make web-check` to run the new test (follow how
   `player-input-contract.test.js` is invoked in the Makefile).

Acceptance:

```bash
node tests/playback/playback-surface-contract.test.js        # every case passes; prints the case count
node tests/playback/player-input-contract.test.js            # doc blocks match the fixtures
node tests/playback/web-policy.test.js
python3 -m unittest tests.operations.test_docs_index tests.operations.test_playback_surface_fence
scripts/playback-surface-fence                               # exit 0 with the M0 allow list
scripts/validate lint                                        # points.toml accepts the new check
```

### 4.2 M1 — web (`web/playback-surface`)

1. Adapter: `parseStreamFailure` gains `position_ms`; a new
   `PlaybackPolicy.classifyStreamFailure({status, code, context}) → source id`
   from the embedded `SURFACE_SOURCES`.
2. `presentSurface` state lives on `PLAYER` as `p.surfaceState`; every
   former `setLoading` site raises a fault instead (table in §3.4 for the
   blocking ones; `preparing`/`buffering`/`recovering`/`refused`/`degraded`
   for the rest — list each site's new class in the PR description).
3. Evidence: `playbackProgressTick` feeds `presenting` samples for the
   attached generation; `visibilitychange` feeds `hidden`; `attachHls` /
   `retirePlaybackPredecessor` feed `attach`/`retire`; `controlSeek`
   settle/supersede feed `intent_*`.
4. `renderPlaybackSurface` inside the fence anchors is the only caller of
   `setLoading`; `playerInputState()` reads `p.surfaceState.surface.kind`.
5. Owner obligation at the four web sites (§3.4).
6. Ledger section + client-log events (§3.6). `closePlayer` clears the model.
7. Remove the M1 entries from the fence allow list.

Acceptance:

```bash
node tests/playback/web-policy.test.js       # runs every fixture case against the SHIPPED presentSurface via sliceDeclaration
node tests/playback/playback-surface-contract.test.js
scripts/playback-surface-fence               # zero web allow-list entries outside the render anchors
make web-check
```

Plus three pinned tests in `web-policy.test.js`, each of which must fail
under the named mutation: (i) delete `pausePlaybackInternally` from
`showStallRecoveryFailure` → the fixture-error case `android_fail_without_stop_is_fixture_error`'s
web twin fails; (ii) make `renderPlaybackSurface` add `failed` for
`preparing` → the input cross-product test fails; (iii) let `canplay` call
the reducer with `presenting: true` → `canplay_is_inert` fails. Browser
smoke (Chrome + Safari, recorded in the PR): a 4K remux with ≥ 20 s buffered,
server network pulled for 5 s at minute 2 → indicator, picture continues,
existing reopen on drain, ledger shows `recovering → cleared(by: presenting)`.

### 4.3 M2 — Apple (`apple/playback-surface`)

1. `PlaybackSurfaceModel.swift` (§3.3) + `PlaybackSurfaceLog`.
2. `PlurxAPI.check` per §3.5; `APIError.refused`; test that
   `isSessionExpired` still matches a 401 with a JSON body and that a 409
   still yields `.conflict`.
3. Replace the four fields with `surface`; every writer raises. `fail()`
   gains `player.pause()` on the exhausted readiness path;
   `handleBlackFrameDecodeFailure` gains pause + `exhausted`.
4. Evidence from `makePeriodicPlaybackObservation` (gated
   `!isChangingStream && player.rate > 0`, plus the first-frame proof for an
   unestablished item); `openGeneration` changes feed `attach`/`retire`;
   `viewerActionEpoch`/`seekState` feed `intent_*`;
   `UIApplication.didEnterBackground`/`willEnterForeground` feed `hidden`;
   `isExternalPlaybackActive` / PiP `isActive` feed `presenting` as declared.
5. Lock-screen `playCommand`: unchanged call, and a test that a `presenting`
   sample after `stopped` yields the demoted banner + `surface_disagreement`.
6. PiP `errorMessage` becomes a `degraded` fault (PiP-sourced) in the same
   model.
7. Ledger SURFACE section in `PlayerView`'s ledger; client-log events via
   the existing reporter path.
8. `make apple-build-bump`; parity doc + README claims move with it.

Acceptance:

```bash
# on the macOS runner (mba, 192.168.5.115) — DEVELOPMENT_PIPELINE.md §4, compile before CI
make apple-test          # AppleClientTests runs every fixture case against PlaybackSurfaceModel
scripts/playback-surface-fence
python3 -m unittest discover -s tests/operations -p 'test_*.py'   # build claims + docs index
```

Pinned tests, each failing under the named mutation: (i) remove
`player.pause()` from the black-frame exhaustion → fixture error; (ii) make
`PlayerInputRouting` derive `.failed` from any blocking-or-progress surface →
cross-product test fails; (iii) feed `timeControlStatus == .playing` as
evidence → `isPlayingChanged_is_inert`'s Apple twin fails. Simulator run
recorded in the PR: a fresh VOD transcode whose item never readies
(`shouldBoundFreshStartReadiness` path) walks the compatibility ladder, then
shows `exhausted` with `player.rate == 0`; lock-screen play under it → banner
"Playback recovered" with Try again, `surface_disagreement` in the ledger.

### 4.4 M3 — Android (`android/playback-surface`)

1. `PlaybackSurface.kt` (§3.3); `Controller.surface: StateFlow`; delete
   `onError` and `playbackNotice`; `PlayerScreen` deletes `playFailure`.
2. Adapter per §3.5 (`RefusalException`, `InvalidResponseCodeException`
   body decode).
3. Owner obligation at the sites in §3.4; `PlaybackFailed` opaque and
   reading the fault (title, detail, actions).
4. Evidence from the stall watchdog's `realPosition()` samples on the
   current `mediaMutationEpoch` and `presentedVideoFrame`;
   `presentationForeground` feeds `hidden`; `mediaMutationEpoch` feeds
   `attach`/`retire`; `PlaybackIntent` sequence settle/supersede feed
   `intent_*`.
5. Ledger SURFACE section; `PlaybackTelemetry.report` for the log events.
6. Bump the Android build counter; parity doc + README claims move with it.

Acceptance:

```bash
cd clients/android && ./gradlew :app:testDebugUnitTest :app:lintDebug   # PlaybackSurfaceReducerTest runs every fixture case
scripts/playback-surface-fence
python3 -m unittest discover -s tests/operations -p 'test_*.py'
```

Pinned tests: (i) each of the three exhausted `onStall`/deadline sites
leaves `player.playWhenReady == false` before the fault is raised (a test
per site, using the existing fake player); (ii) a `realPosition()` advance
on the same epoch after `stopped` yields `surface_disagreement`, and an
`onIsPlayingChanged(true)` alone does **not**; (iii) `PlaybackPolicy.
playbackErrorAction` is untouched (its existing tests still pass unmodified).
Emulator run recorded in the PR: inject a 503 on a segment after 30 s of
playback → `recovering` indicator, picture continues from buffer, the
existing failover/reopen runs, no blocking surface unless the ladder is
spent — and if it is, `playWhenReady == false` under it.

### 4.5 M4 — physical verification (`docs/playback-surface-verification`)

Run the three recipes in §7 on the Apple TV, an iPhone and an Android TV
with the builds from M1–M3 installed (deploy is Paul's; the client install
prompt is `docs/clients/CLIENT-DEPLOY-PROMPT.md`). Record each run in
`docs/clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-<date>.md` with the
ledger's `surface_history` pasted per recipe, and add the index row.
Acceptance is §7's allowed-outcome table: every observed outcome is in the
allowed set for its recipe, and every `surface_disagreement` row names a
documented exception (today's list: the iOS lock-screen `playCommand`).

### 4.6 M5 — the three recovery additions (`playback/recovery-additions`)

Own PR by ruling; after M1–M3. Constraints from the review, all mandatory:

1. **Create 503 "not yet" retry** (all three clients): retry the *same*
   create request with the same request identity, backoff 1 s · 2 s · 4 s,
   bounded by an **absolute** deadline of 60 s from the first attempt, then
   the owner raises `exhausted`. A late success after the deadline is
   released (`releaseSession`) not attached. Cancelled by any newer intent.
2. **Web hls.js network fatal retry**: one `hls.startLoad(position)` after
   2 s, sharing a single per-attach budget with the `segment_503_not_yet`
   row (one retry total, whichever fires first), position preserved from
   `v.currentTime`; then the existing reopen path.
3. **Android `BEHIND_LIVE_WINDOW`**: finite timelines only — on 1002 with
   `player.isCurrentMediaItemLive == false`, `seekTo(lastRealPosition)` +
   `prepare()` once per attach; live items keep today's `Fail`. Media3's
   `seekToDefaultPosition()` is a live-edge policy and is **not** used.

Acceptance: a fixture case per addition (`m5_create_retry_deadline`,
`m5_hls_retry_once`, `m5_behind_live_window_finite`), each run on the
client it applies to; a mutation removing any one retry fails its case; the
web test pins `startLoad` called at most once per attach.

#### What landed, and the two readings it had to settle

**Landed** in `playback/recovery-additions`. Three decisions the text above
left open, recorded here rather than resolved silently:

1. **"Backoff 1 s · 2 s · 4 s" is a CLOSED list**, not a ramp that then holds at
   4 s. Three retries, four attempts; the ladder is spent after the third and
   the owner raises `exhausted` with the reason `ladder_spent`. The absolute
   deadline is the other termination, with the reason `deadline`, and it is
   what bounds a SLOW server rather than a refusing one — which is the case the
   review named. Read the other way the sequence would be about fourteen
   retries, and nothing in the review or the contract asks for that.
2. **The absolute deadline runs on a watchdog, not between attempts.** Checking
   elapsed time only at rung boundaries turns an absolute bound back into a
   per-attempt one the moment the server holds a create: Apple's create session
   has a 180 s request timeout, so a boundary-checked "60 s" could surface at
   three minutes. All three clients arm a timer at the first attempt; when it
   fires the owner stops the player and raises `exhausted` immediately, and the
   create still in flight is awaited only so its session can be RELEASED.
3. **On the web the 60 s deadline is not reachable at all, and three of M5's
   own branches are dead there.** `beginPlaybackPreparation` (`index.html`)
   gives every open a 20 s ABSOLUTE preparation deadline, measured from before
   the decision call, and the M5 sequence runs inside it. The consequence is not
   a shorter timer. When preparation wins it aborts the create's signal, so the
   sequence throws a plain `AbortError` carrying no `surfaceRaised`, and
   `failPreparation` raises **`owner_stopped` — "Playback could not prepare."**,
   not M5's `exhausted` sentence. So on the web:

   * the only reachable termination is `ladder_spent`, at about 7 s;
   * the `deadline` reason, the release-a-late-success branch and the
     `sequence.expired` guard are exercised by the test suite and by nothing on
     a shipped path;
   * "a late success is released, not attached" is a property of the tests, not
     of the browser.

   This is a **conflict between this plan and the code**, not a resolution.
   Closing it means moving or restructuring a pre-existing threshold, which §6
   forbids in this PR. The question for Paul is therefore not a timing number:
   it is **should the web raise `exhausted` for a still-building create at all,
   or is 20 s + `owner_stopped` the right answer for a client whose whole open
   is bounded at 20 s?** Apple (180 s request timeout) and Android (60 s read
   timeout) both reach the deadline on shipped paths, so whichever way it is
   ruled, the web is the client that diverges.

Scope notes worth keeping:

* The create retry runs in the `start` context only on every client — **the
  ladder and its deadline watchdog alike.** The first version of this work
  gated only the ladder on Android and armed the watchdog unconditionally,
  which put a sixty-second stop-the-player timer on every seek and quality
  change: §7 recipe (b)'s explicitly not-allowed outcome, raced against that
  client's own sixty-second read timeout. `startContext` is now a parameter of
  `createRetryingNotYet` rather than a predicate folded into `isNotYet`, which
  is what makes the gate one decision instead of two.
* `startCopyHls` keeps answering `vod_index_pending` with its existing
  progressive-remux fallback rather than waiting seven seconds for a stream it
  can already play. Both branches of a pending change on the web now go through
  the same wrapper, so which context a create is in is decided in one place.
* Apple's `refusalSurfaceOutcome` still declines to classify
  `create_503_not_yet` — that function feeds a raise that only happens after the
  owner's stop, where a progress class would be a spinner over a stopped
  player.
* "Attach" for the Android `BEHIND_LIVE_WINDOW` budget means the item on the
  screen changed. `attachRecipe` is not the only way that happens:
  `commitPreparedReplacement` swaps in a second `ExoPlayer` without going near
  it, so the budget is re-armed at both sites.

### 4.7 M6 — Android progressive-remux landing (`android/remux-seek-origin`)

Gated on a measurement, not a reading. First, on an Android TV with a
progressive-remux title (Playback debug: `Remux`, no session id): seek
forward 10 min and capture from the client log the achieved
`X-Plurx-Media-Origin-Ms` (`ProgressiveMediaOrigin.acceptResponse`) and the
first-frame `realPosition()`; also whether `bounded_progressive_media_origin`
timed out (server log line "using requested start"). Build M6 only if
achieved origin differs from the requested start by > 250 ms **and** the
first frame landed at the origin.

Then: in `executeSeek`'s remux branch and `restartAt`'s remux path, after
`acceptResponse` resolves the origin, `player.seekTo(requested − originMs)`
once, guarded by `mediaMutationEpoch` (the same shape as
`successorAttachPositionMs` for prepared successors). Commit/rollback: the
compensating seek belongs to the epoch that attached the item; a newer epoch
cancels it; a failed item (`onPlayerError`) before the seek lands leaves
`pendingSeek` to the target deadline as today. Acceptance: `PlaybackIntentTest`
case (target 90 000, origin 86 000 → lands after compensation);
`PlaybackTargetDeadlineTest` unchanged; recipe (c) passes on the device with
no `playback_target_timeout`.

---

## 5. Tests that must exist when the work is done

1. **Fixture runner per client** (M0 web reference, M1 shipped web, M2
   Apple, M3 Android) — every `cases` row, no skips; an unexpressible event
   is mapped, not dropped.
2. **Fixture invariants** (M0) — blocking requires stop; every source's
   class exists; every action listed; first-match precedence has no
   unreachable row (each row is hit by at least one case).
3. **Inert-event tests** — `canplay`, `playing`, `isPlayingChanged`,
   `timeControlStatus` never change a surface.
4. **Cross-product input test** (each client) — for every surface kind ×
   class × contract input, the routed outcome equals the fixture's; the
   `failed` state is entered only by `exhausted`/`stopped`.
5. **Fence fixtures** — must-trip and must-not-trip files per kind.
6. **Auth preservation** (Apple) — 401 with a body still `isSessionExpired`;
   409 still `.conflict`.
7. **Owner-stop tests** — one per exhausted site per client, each a
   mutation-killer.
8. **Ledger fields** — `playback-info-fields.json`'s SURFACE fields are
   rendered by all three ledgers (the existing info-contract tests extend to
   the new section).

---

## 6. Non-goals and guardrails

- Do not change any threshold, budget, detector or ladder in M0–M4. The
  only player-behaviour change is the stop-before-raise obligation (§3.4).
- Do not add a watchdog for evidence; use the samples the client already
  takes.
- Do not make the presenter call `pause`, `play`, `seek`, `prepare`,
  `startLoad`, `reopen`, cancel a timer, or report to the control plane.
- Do not keep the old fields as deprecated aliases; delete them so the fence
  can be exact.
- Do not gate anything. If an operator decision is discovered, add an
  advisory section to the Developer tab of settings and flag it in the PR.
- Do not touch Live TV, Library Channels (Swift), the offline players, the
  readers, or the download manager.
- Do not do a copy pass; move text as-is unless it lies about the state.
- Do not merge past a real test failure; infrastructure-only failures are
  noted and merged per the standing ruling.

---

## 7. Physical verification recipes and allowed outcomes (M4)

| Recipe | Steps | Allowed outcomes | Not allowed |
|---|---|---|---|
| (a) network drop mid-film | 4K remux, ≥ 20 s buffered, pull the server's network for 5 s at minute 2, restore | `recovering` indicator while the picture plays from buffer, then `cleared(by: presenting)` on the same or a new attached generation; **or** buffer drains → full-screen `recovering`/`buffering` → existing reopen → cleared; **or** ladder spent → `exhausted` with rate 0 and Keep waiting works after the network returns | any blocking surface with the picture moving; any surface that persists after 10 s of continuous presenting; any `surface_disagreement` |
| (b) failed change on a cold NAS | while playing, change quality so the create 503s (NAS spun down) | `refused` banner with Retry; predecessor keeps playing; banner retires when the change is re-issued and succeeds, or after 10 s continuous presenting if the viewer does nothing | full-screen anything; the predecessor's surface changing because of the failed change |
| (c) Android progressive-remux seek (M6 measurement) | `Remux` title, no session id, seek +10 min, wait 25 s | picture resumes near the target; **or** `recovering` (target-deadline `recover`) then resumes; capture achieved origin + first-frame position | terminal "couldn't reach the requested position" over a moving picture (that is the M6 defect — record it, then build M6) |
| (d) lock screen (iPhone) | force `stopped` (e.g. decoder failure on a known-bad file), lock, press play on the lock screen | banner "Playback recovered" with the fault's actions; `surface_disagreement` row naming `lock_screen_play` | blocking surface over audio; no ledger row |

---

## 8. File ownership for parallel sessions

| Session | Owns | Must not touch |
|---|---|---|
| M0 | `tests/playback/playback-surface-contract.json`, `…contract.test.js`, `scripts/player-contract-table`, `scripts/playback-surface-fence`, `tests/operations/test_playback_surface_fence.py`, `validation/points.toml` (the check), `playback-info-fields.json`, `playback-policy.js` (`presentSurface` + generated constants), the three docs' generated blocks | `index.html`, any client source |
| M1 | `crates/plurxd/src/web/index.html`, `playback-policy.js` (adapter functions only — not the reducer's contract), `tests/playback/web-policy.test.js`, the web allow-list entries of the fence | Swift, Kotlin, the fixture (propose fixture changes to M0's owner as a separate commit) |
| M2 | `clients/apple/**`, the Apple allow-list entries, `docs/clients/APPLE-CLIENT-PARITY.md`, build counter | web, Kotlin, fixture |
| M3 | `clients/android/**`, the Android allow-list entries, `docs/clients/ANDROID-CLIENT-PARITY.md`, build counter | web, Swift, fixture |
| M5 | one PR, all three clients' *owner* code and the fixture rows tagged `m5_*` | presenters, renders |
| M6 | `Controller.kt` remux paths, `PlaybackIntentTest`, `MediaOrigin.kt` if needed | everything else |

A fixture change discovered mid-M1/M2/M3 is a separate commit on that
milestone's branch, cherry-pickable, and is mentioned in the PR description
so the other two sessions rebase onto it.

---

## 9. PR mechanics

- Branch from `main`; `WIP:` title; body links this doc and the milestone.
- Each PR adds/updates its `docs/README.md` rows and a `STATUS.md` entry at
  the top (newest effort first) in the same commit as the work.
- Client PRs bump the build counter with the repo's tooling (`make
  apple-build-bump`; Android per `validation/mobile_versions.py`) and let
  `doc_versions.py` move the claims.
- Runtime commits may need a `validation/regressions.d/` row if
  `make validate-staged` asks for one; follow its message.
- Adversarial review before un-WIP; implement findings; one full run;
  merge.
- Deploy is Paul's. Say what wants deploying and stop.
