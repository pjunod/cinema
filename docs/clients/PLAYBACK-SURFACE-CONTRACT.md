# Playback surface contract — the overlay is a projection of the player, not a message

**Status:** proposed 2026-09-13, awaiting Paul's ruling · **Investigated at:**
`main` @ `10f2afe6` · **Source of truth once built:**
`tests/playback/playback-surface-contract.json` · **Kept honest by:**
`tests/playback/playback-surface-contract.test.js` (web), a fixture test per
native client, and `scripts/playback-surface-fence` (see §5)

Companion to [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (what a
press does) — this is *what the viewer is shown when playback is not simply
playing, on every client, and when it goes away*. Same shape as that contract,
for the same reason: the fault surfaces diverged per client because every
call site answered the question for itself.

The one-sentence version: **a failure-shaped event becomes a typed fault, the
fault's severity picks the surface, a blocking surface is only ever shown over
a player the presenter itself has stopped, and evidence that the picture is
moving retires any fault that claimed it wasn't.**

---

## 1. What was reported, and what it is

Reported: a full-screen error overlay appears while the picture keeps playing,
or stays up after playback stopped and came back on its own; the text varies
("a bunch of various errors"); often a full-screen surface is not warranted,
and sometimes nothing is.

This is one defect with three spellings, not a collection of wording bugs. On
every client the blocking overlay is an **imperative message channel** — a
string pushed at the view from many call sites — rather than a **projection of
player state**. Three properties follow from that, and each one produces a
symptom Paul described:

| Missing property | Symptom |
|---|---|
| Raising a blocking surface does not stop the player | overlay over a moving picture |
| Progress evidence does not retire the surface | overlay stays after playback resumed |
| A fault carries no severity or generation | "not yet" answers, automatic quality changes and stale refusals all get the same full screen |

The fix is therefore not to patch the call sites — there are 40 on the web, 12
on Apple, 9 on Android — but to take the decision away from them, the way the
input contract took key handling away from the views.

---

## 2. What the code does today

Every claim below was checked against the source at `10f2afe6`. Line numbers
will drift; the function names will not.

### 2.1 Web — `#ploading` is loading, buffering, failure and prompt in one element

- The only full-screen surface is `#ploading` (`index.html:3404`, CSS 535-537).
  `setLoading(on,text,sub,actionHtml)` (`index.html:10998`) paints text into
  it directly from **40 call sites**; severity exists only as an ad-hoc
  combination of "buttons present", a `failed` class (added at exactly four
  sites: 4076, 6994, 10962, 11611) and `PLAYER.stallPrompt`. No CSS rule
  targets `.ploading.failed` — the spinner keeps spinning on every failure.
- **A fatal hls.js error never pauses the video.** hls.js only calls
  `stopLoad()`; the handler (`6984-6995`) paints "Still preparing this
  stream…" or "Playback failed to start (fragLoadError)" while the `<video>`
  plays out up to 30 s of forward buffer. No `stallPrompt` is set, so when the
  buffer drains `waiting` repaints "Buffering…" (which also strips `failed`,
  `11002`), `persistentWait` reopens 8 s later and `playing` clears
  everything. Net: error over a moving picture → silently becomes
  Buffering → resumes.
- **A failed stream change is sticky over a playing predecessor.**
  `executePlaybackMediaChange` deliberately keeps the predecessor playing
  until the create succeeds (`9513-9515`) and, on failure, retains
  `pendingMediaChange` (`9562-9564`). `playbackOwnsAttachedMedia()`
  (`10649`) is then false, so `playing` (11462), `canplay` (11515),
  `waiting` (11527) and the progress tick all return early: "Playback could
  not reconnect." sits over a stream that is still playing until Try again or
  Close. `failPreparation` (`9039`) has the same shape.
- **`STREAM_FAILURE` is not generation-keyed.** Every ≥400 body hls.js sees
  is recorded (`6793`), including a segment 503 that hls.js then retries
  successfully; only `LEVEL_LOADED` clears it, a VOD playlist is never
  reloaded, and `closePlayer` does not clear it. For 90 s
  (`FAILURE_FRESH_MS`, `playback-policy.js:727`) any unrelated fatal is
  explained by that stale body, and the next title's start watchdog can quote
  the previous title's refusal.
- **`waiting` paints before it is debounced.** The comment at 3798-3812
  documents Safari firing `waiting` at every fMP4 boundary on healthy 4K
  HEVC and `STALL_MIN_MS=350` exists for exactly that — but the handler
  paints "Buffering…" full-screen *before* calling `beginWait` (`11532`).
- **Automatic quality changes dim the whole picture.** Every non-direct seek,
  Auto downshift, decode rescue and subtitle burn goes through
  `executePlaybackMediaChange`, which paints "Preparing the stream…" over the
  still-playing predecessor (`9515`).
- The `failed` input state is read back *from the DOM class*
  (`playerInputState()` `11048`), and most prompts never set it, so arrows
  still seek while a prompt is up.

### 2.2 Apple — `fail()` decides "full screen" by whether an item is attached

- Surfaces are driven by five independent `@Published` fields
  (`PlayerController.swift:1691-1695`): `failed`, `playbackError`,
  `playbackFailureTitle`, `playbackNotice`, `finished`. The full-screen
  `failureView` (`PlayerView.swift:1447-1477`) shows on `failed`; the red
  banner shows `playbackNotice ?? playbackError ?? pip.errorMessage`
  (`PlayerView.swift:1038`) whenever `!failed`.
- `fail(_:)` (`PlayerController.swift:4758-4766`) is the canonical writer.
  It never pauses the player, and it chooses the blocking surface with
  `failed = player.currentItem == nil || error is PlaybackPreparationError`
  — "is an item attached", not "is the player stopped".
- **The 15 s readiness deadline is a full-screen stop over a running
  player.** `awaitItemReady` (`6073-6086`) throws
  `PlaybackPreparationError.timedOut`; `reopen()`'s catch (`3284-3292`) and
  `load()`'s (`3248-3256`) call `fail()`, which sets `failed = true` because
  the error is a `PlaybackPreparationError` — while `player.play()` (`3655`)
  stands. A slow cold transcode or a seek into unencoded film readies later
  and plays behind a black view; the periodic observer keeps advancing
  `currentMs` and Now Playing; the recovery monitor is off (`!failed`,
  `4125`).
- **Nothing clears `playbackError` on progress.** The only clear sites are
  `start`/`startOffline` (2141, 2245), `restartInitialDecision` (3152),
  `retryAfterPlaybackFailure` (2447), `open()` entry (3356) and post-attach
  (2368, 3718). The periodic observer (`4916-4988`) touches neither field. A
  session-create failure during a seek or quality change leaves "Server
  returned 503" in the banner over a stream `restoreAfterFailedChange`
  (`3766-3782`) has resumed — until the next `open()`, which may be never.
- **Refusal bodies are discarded.** `PlurxAPI.check` (`PlurxAPI.swift:192-205`)
  decodes `{code,message}` only for 409. A 503 `startup_timeout` or
  `media_owner_transition` — documented as "retry" in
  [`PLAYBACK.md`](../PLAYBACK.md) — reaches the viewer as `APIError.http(503)`
  ("Server returned 503") on create, or as AVFoundation's generic
  `item.error` text after the item fails; 410 `media_owner_lost` is 4xx →
  straight to fatal with its `film_position_ms` unread.
- The iOS lock-screen `playCommand` (`6898-6906`) calls `player.play()`
  directly and resumes audio under the failure view; `togglePlayPause` is
  routed and correctly `ignore`d in `.failed`.
- The PiP error message clears only on a later successful PiP start
  (`PlayerSurface.swift:214-220`).

### 2.3 Android — `playFailure` is set nine ways and cleared none

- `playFailure` is `remember { mutableStateOf<String?>(null) }`
  (`PlayerScreen.kt:674`), written by `onError = { playFailure = it }`
  (`691`) from nine Controller sites, and **never set back to null** — the
  only dismissal is `PlayerContent` leaving composition (Retry → reload,
  or Exit). `onError` is `(String) -> Unit` (`Controller.kt:130`): no
  severity, code, retryability or "player still running" bit crosses it.
- `PlaybackFailed` (`PlayerScreen.kt:621-645`) is a transparent
  `Column(fillMaxSize)` with no background: the video keeps rendering behind
  the text. It is blocking only by gating — chrome and tap surface removed,
  input state `Failed`, in which the contract routes play/pause to `ignore`
  — so the viewer cannot even pause the stream still playing beneath it.
- **`Fail` never pauses, and the stall watchdog restarts the stream under
  the overlay.** `onPlayerError` → `PlaybackErrorAction.Fail` → `onError`
  (`653-661`) leaves `playWhenReady` true; `stallWatchdogJob` (`816-846`) is
  cancelled only in `release()`. ExoPlayer sits in `STATE_IDLE` with a frozen
  position, `openStallTracker` fires after 8 s, `onStall` (`1428-1460`)
  passes every guard and calls `restartAt`/`reopenAfterStall`. Playback
  resumes; `playFailure` stays. This is the literal "stopped and then
  resumed even though the overlay is still there".
- Paths B–G (session create failed `1281`, stall reopen failed `1544`,
  terminal verdict `1337`, sessionless second stall `1444`, budget exhausted
  `1457`, target deadline `2174`) never stop ExoPlayer at all; B in
  particular fires while the old item is still playing from buffer.
- **Refusal bodies are never read.** A playlist/segment 503 becomes
  `ERROR_CODE_IO_BAD_HTTP_STATUS` after ExoPlayer's default retries and goes
  to `Fail` ("Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS)."); the web
  shows "Still preparing this stream…" for the same answer. Session-create
  exceptions are not classified (`AppViewModel.kt:791-798`).
  `BEHIND_LIVE_WINDOW` (1002) also goes to `Fail` (`PlaybackPolicy.kt:77`).
- No test references `onError`, `playFailure`, `playbackNotice` or
  `PlaybackFailed`.

### 2.4 The common shape

```
 TODAY                                     PROPOSED
 ─────                                     ────────
 event ──▶ call site decides ──▶ view      event ──▶ ADAPTER ──▶ PRESENTER ──▶ SURFACE
           text · full-screen?             (raw → fault)  (fault × player evidence
           buttons? failed flag?                          → surface; pauses player
           (40 / 12 / 9 sites)                            for blocking; clears on
                                                          progress; generation-keyed)
 player ──▶ nothing ties the two            player ──▶ evidence ───┘
```

Nothing in the left column is a *wrong* decision in isolation — each site was
written for one fault and is defensible for it. The defect is that the
decision is made at the site, so the sites disagree, and none of them is
told what the player did next.

---

## 3. The contract

### 3.1 Faults — a closed set of classes, each with one severity

A **fault** is `{class, source, generation, at, title?, detail?, retryable?,
position_ms?}`. `class` is one of the rows below; `source` is the raw event
that produced it (§3.3), kept for the ledger; `generation` is the open
generation / media-change epoch it was observed on. Severity is a property of
the class, never chosen at the call site.

| Class | Severity | Surface | Pauses player? | Cleared by |
|---|---|---|---|---|
| `preparing` | progress | spinner + stage text — full-screen only while no frame has been presented on this generation; otherwise the small in-chrome indicator | no | first frame / progress on this generation |
| `buffering` | progress | same as `preparing`, and only after `STALL_MIN_MS` (350 ms) — the wait must have *lasted* | no | progress |
| `recovering` | progress | in-chrome indicator with the reason ("Reconnecting…", "Switching quality…") over the still-playing predecessor | no | reopen success / progress on the new generation |
| `hold` | notice | banner, timed (30 s), server sentence | no | timer · progress |
| `degraded` | notice | banner, timed (5 s): HDR-subtitle, PGS, PiP, Auto downshift, subtitle fell back | no | timer |
| `refused` | notice | banner, untimed: a change the viewer asked for could not be made and the previous stream continues ("Couldn't switch quality — still playing 1080p", with Retry in the transport row) | no | next successful change · progress ≥ 10 s |
| `stalled` | prompt | full-screen: "Playback is stalled." + Keep waiting / Try again / Close | **yes** | user action · (progress ⇒ disagreement, §3.2) |
| `stopped` | terminal | full-screen: title + server or client sentence + Try again / Close | **yes** | user action |

Eight classes, and the table is exhaustive on purpose: a call site that
cannot name its class has not understood its fault, and the fixture test will
ask it to.

Two things the table fixes by construction. A `progress` fault can never be
full-screen over a presented picture — the surface degrades to the in-chrome
indicator the moment a frame has been shown on this generation, which is what
retires "Preparing the stream…" over a playing predecessor and "Buffering…" at
Safari segment boundaries. And the only two classes that block are the two
that stop the player, so the overlay and the picture cannot disagree unless
something outside the presenter moves the player — which is the case §3.2
handles.

### 3.2 The agreement rule

> A blocking surface is shown only over a player the presenter has stopped.
> Progress under a blocking surface is a **disagreement**, and a disagreement
> is resolved in favour of the picture.

Concretely: raising `stalled` or `stopped` pauses the player (web
`pausePlaybackInternally`, Apple `player.pause()`, Android
`playWhenReady = false`) *before* the view repaints. If the presenter
nevertheless receives progress evidence for the current generation while a
blocking surface is up — the iOS lock-screen `playCommand`, the Android stall
watchdog, a browser autoplay policy, a bug — it demotes the fault to a
`degraded` notice reading "Playback recovered", clears the blocking surface,
and logs `surface_disagreement {class, source, generation, position_ms}` to
the client log and the ledger (§5). The viewer sees the picture they are
already watching, with a banner, and the disagreement is attributable rather
than mysterious.

The rule deliberately does not try to *prevent* every outside `play()`. It
makes the outcome correct whichever side moves first, which is the property
none of the three clients has today.

### 3.3 Sources — what each raw event becomes

The mapping lives in the fixture so all three clients agree. The table is the
intent; the fixture is the contract.

| Raw event | Class | Notes |
|---|---|---|
| Session create 503 `startup_timeout` / `media_owner_transition` | `preparing` (retryable) | server is still building; retry with backoff for the startup budget, then `stalled` |
| Playlist/segment 503 with the same codes | `preparing` | hls.js: keep the media playing, `startLoad()` after backoff instead of fatal; ExoPlayer/AVPlayer: read the body from the error, reopen on the same session |
| 410 `media_owner_lost` | `stopped` | body carries `film_position_ms`; Try again reopens there |
| Control-plane `terminal` verdict | `stopped` | server sentence wins, as today (ruling D1) |
| Control-plane `hold` / `retry_resource` | `hold` | unchanged: notice, never blocking |
| Session create 4xx/5xx during a seek, quality, audio or subtitle change (predecessor still playing) | `refused` | today: sticky "could not reconnect" (web) / "Server returned 503" (Apple) / "couldn't start this stream" (Android) |
| Media `waiting` / `waitingToPlayAtSpecifiedRate` / `STATE_BUFFERING` after start | `buffering` | after 350 ms only |
| Watchdog: persistent wait → automatic reopen / rescue / fallback | `recovering` | the picture stays if it is moving |
| Watchdog: recovery budget exhausted, picture frozen | `stalled` | pauses; prompt |
| hls.js fatal **network** on a playing stream | `recovering` | never "failed to start"; one bounded `startLoad()` retry, then reopen, then `stalled` |
| hls.js fatal **media** on a copy path (first time) | `recovering` | the transcode rescue, unchanged, but as a progress surface |
| `<video>` `error` / `AVPlayerItem.status == .failed` / `onPlayerError` after the client's ladder is spent | `stopped` | decoder refused or nothing left to try |
| Readiness deadline (Apple 15 s, web 20/40 s watchdog) with no frame yet | `stalled` | pauses, prompts — never `stopped` while the server may still publish |
| `BEHIND_LIVE_WINDOW` (Android 1002) | `recovering` | `seekToDefaultPosition()` + `prepare()`, not `Fail` |
| Repeated early end at the same position < 95 % | `stopped` | unchanged |
| Auto downshift, decode rescue, HDR-subtitle, PGS, PiP failure | `degraded` | notices, unchanged in copy, no longer able to reach the overlay |
| Telemetry / reporter / stats fetch failures | (none) | log only — already true on all three clients; the fence keeps it so |

### 3.4 Clearing — evidence, not hope

The presenter consumes the progress evidence each client already produces
and clears faults with it; it does not add a watchdog:

- **Web:** `samplePlaybackPresentationClock` (`index.html:11537`), the
  `playing`/`canplay` handlers and the progress tick. The gate
  `playbackOwnsAttachedMedia()` stays for *automatic work*; it stops gating
  *dismissal*, because progress on the attached generation is progress
  whoever requested the next one.
- **Apple:** the periodic time observer (`PlayerController.swift:4916`) and
  `onRenderedFirstFrame`-equivalent readiness. A fault observed on
  generation N is dropped when N is superseded, which is the existing
  `isSuperseded(generation)` test.
- **Android:** `onRenderedFirstFrame`, `onIsPlayingChanged` and the stall
  tracker's own `realPosition()` samples, keyed by `mediaMutationEpoch`.

Generation keying replaces the 90 s `FAILURE_FRESH_MS` window: a refusal
explains the generation it arrived on and nothing else. `closePlayer` clears
every fault.

### 3.5 The fixture

`tests/playback/playback-surface-contract.json`, in the shape of
`player-input-contract.json`:

```json
{
  "schema": 1,
  "classes": {
    "stalled": {"severity": "prompt", "blocking": true, "pauses_player": true,
                "clears_on": ["user"], "title": "Playback is stalled.",
                "actions": ["keep_waiting", "retry", "close"]},
    "preparing": {"severity": "progress", "blocking": "until_first_frame",
                  "pauses_player": false, "clears_on": ["progress"]}
  },
  "sources": {
    "session_create_503_startup_timeout": {"class": "preparing", "retryable": true},
    "hls_fatal_network_established":     {"class": "recovering"},
    "behind_live_window":                {"class": "recovering"}
  },
  "timings": {"buffering_min_ms": 350, "hold_notice_ms": 30000,
              "degraded_notice_ms": 5000, "refused_clear_progress_ms": 10000},
  "cases": [
    {"name": "fatal network over a playing picture",
     "given": {"fault": "hls_fatal_network_established", "presenting": true},
     "expect": {"surface": "indicator", "paused": false}},
    {"name": "stopped, then an outside play()",
     "given": {"fault": "decoder_failed", "presenting": false,
               "then": "progress"},
     "expect": {"surface": "banner", "class": "degraded",
                "log": "surface_disagreement"}}
  ]
}
```

`cases` is the table every client's presenter is run against, row for row —
the same discipline as the routing table. The doc's §3 tables are rendered
from it by `scripts/player-contract-table` once that script learns a second
fixture, so this page cannot drift from the contract.

---

## 4. How a client implements it

Three layers, and the middle one is the only one allowed to decide. It is
the input contract's diagram with the nouns changed.

```
 raw event ─────▶ ADAPTER ─────▶ PRESENTER ─────────────▶ SURFACE
 (hls.js ERROR,   raw → fault    pure reducer:            #ploading / banner /
  item.status,    with source    (faults, evidence,        failureView / indicator
  PlaybackExc.,   + generation   generation) → surface     rendered FROM state
  503 body)                      + side effects (pause)
 player evidence ────────────────────┘
```

- **Adapter** — the only code per client allowed to mention an hls.js
  `details`, an `AVPlayerItem.status`, a `PlaybackException.errorCode`, an
  HTTP status, or a refusal `code`. It turns them into a fault via the
  fixture's `sources` table. On the natives this is also where the `{code,
  message}` body finally gets read: Apple `PlurxAPI.check` decodes it for
  every non-2xx (not only 409) into `APIError.refused(status, code,
  message)`; Android reads `HttpDataSource.InvalidResponseCodeException.
  responseBody` and the create-call body. The web already has
  `parseStreamFailure`; it moves into the adapter unchanged.
- **Presenter** — one pure reducer per client, no platform imports,
  signature-equivalent to `present(state, event) → (state, effects)` where
  `event` is a fault, progress evidence, a generation change or a user
  action, and `effects` is at most `pause`, `log`, `clear`. Web: a new
  `PlaybackPolicy.presentSurface` next to `routeInput`; Apple: a
  `PlaybackSurfaceModel` value type published as **one** field on
  `PlayerController` that replaces `failed`, `playbackError`,
  `playbackFailureTitle` and `playbackNotice`; Android: a `PlaybackSurface`
  sealed class on a `StateFlow` that replaces `playFailure`,
  `playbackNotice` and the `onError` string callback. Each is tested against
  `cases`, row for row.
- **Surface** — renders from the presenter's state and nothing else.
  `setLoading()` becomes a private render function called from one place;
  `failureView` and `PlaybackFailed` read the model; the input contract's
  `failed` state is derived from the model (`surface.blocking`), not from a
  DOM class. `PlaybackFailed` gets the black background it was always meant
  to have, because the player behind it is now actually stopped.

The rule that keeps the moles from coming back: **no fault text and no
overlay mutation outside the presenter.** `scripts/playback-surface-fence`
runs from `make validate-staged` beside `player-input-fence` and fails on
`setLoading(` / `classList.add("failed")` / `stallPrompt=` outside the web
render function, `failed =` / `playbackError =` / `playbackNotice =` outside
`PlaybackSurfaceModel`, and `onError(` / `playFailure` outside the Android
presenter, in any shipped client file. Allowed regions are named with a
reason beside their anchors, as the input fence does.

What does **not** change: the recovery ladders themselves (same-delivery
reopen, node failover, compatibility fallback, Auto), the control-plane
verdict semantics, the stall detectors and their thresholds, the input
routing table. Those decide *what the player does*; this contract decides
only *what the viewer is told and when it goes away*.

---

## 5. Attributability — the ledger, the log, the fence

Anything that dims the picture on Paul's Apple TV has to be answerable from
inside the product. The presenter keeps a bounded ring (last 16) of
`{class, source, generation, raised_at, cleared_at, cleared_by,
player_at_raise: {rate, position_ms, presenting}}` and:

- the **Playback debug ledger** on all three clients gets a *Surface*
  section showing the current fault and the ring — so "what was that
  overlay" has an answer without a log file;
- the client log gets `surface_raised`, `surface_cleared` and
  `surface_disagreement` events with the same fields, so the server-side
  session log can be joined to what the viewer saw;
- `surface_disagreement` is the one to watch after the build lands: every
  occurrence is a place where something outside the presenter moved the
  player, and each should end as either a fixed call site or a documented
  exception (the lock-screen command, for one).

Nothing here is gated. Per the standing ruling, if any part of this needs an
operator decision it goes in the Developer tab as an advisory enable section;
as proposed, nothing does.

---

## 6. What the migration closes, and two things it does not

Closed by construction (the presenter cannot express them):

- **Web:** fatal-over-live-buffer (§2.1 second bullet); sticky "could not
  reconnect" / refusal over a playing predecessor; stale `STREAM_FAILURE`
  across generations and titles; "Preparing the stream…" full-screen for an
  automatic downshift; undebounced "Buffering…" flashes; `failed` routing
  reachable only from four sites; a 503 on create painted with no retry.
- **Apple:** the 15 s readiness stop over a running player; "Server
  returned 503" banner that only `open()` clears; `media_owner_lost` and
  `startup_timeout` bodies discarded; the PiP error that only PiP can clear;
  the lock-screen `play()` under a black view (now a logged disagreement
  that resolves to the picture).
- **Android:** `playFailure` never nulled; the stall watchdog restarting the
  stream under the overlay; `Fail` without pause; transparent failure view
  over a running player; refusal bodies unread; `BEHIND_LIVE_WINDOW` as
  fatal; no test on any failure surface.

Two findings from the same investigation are **separate defects** and are
listed so they are not mistaken for this one:

1. **Android copy-HLS seeks may never "land".** A non-VOD session attaches
   at local 0 (`MediaOrigin.kt:37-44`, `attachPositionMs = 0`) with no
   forward seek from the keyframe origin — Apple seeks forward
   ([`PLAYBACK.md`](../PLAYBACK.md) "Resume & progress"); Android does not —
   while `presentedVideoFrame` (`PlaybackIntent.kt:143-157`) demands
   |position − target| ≤ 250 ms at the first rendered frame. A copy session
   begins at the preceding keyframe (1–6 s early on 4K GOPs), so the landing
   check fails, the 8 s target deadline fires `presentation-recovery`, the
   restart lands the same way, and the terminal "couldn't reach the requested
   position" overlay goes up over a playing stream. Structurally reachable on
   every seek and track switch of a copy session; **not yet confirmed on a
   device** — §8 has the verification prompt. The fix is the Apple one: seek
   the attached item forward by `requested − media_origin_ms`.
2. **Web progress-watch on a stalled frame counter.** `playbackProgressTick`
   (`index.html:11216-11264`) requires the frame count to advance when a
   counter exists; under AirPlay or iOS video-only fullscreen the clock
   advances while the counter does not, which would make `persistentWait`
   fire every 8 s on a playing film. Structurally reachable, unverified in a
   browser; worth a check when the AirPlay path is next on a bench.

---

## 7. Non-goals

- **No new watchdogs, thresholds or recovery steps.** Every detector and
  budget stays where it is with the numbers it has. Changing what the player
  *does* under this contract's name would make the review of either change
  impossible.
- **No wording pass.** Copy moves into the fixture as it is today unless a
  sentence is wrong about the state (e.g. "failed to start" for a mid-film
  network fatal); a copy review is a later, separate change.
- **No gate.** Nothing is switchable; per the standing ruling, a Developer
  tab enable section would only be added if something needed one.
- **Not the Live TV player.** `#live-tv-overlay` and the Live TV natives
  have their own contract page; they join this fixture when their overlay
  work resumes, not before.

---

## 8. Build plan

Four PRs, fast lane until each is together, opened as drafts, adversarial
review, findings, one full run, then merged — per
[`DEVELOPMENT_PIPELINE.md`](../DEVELOPMENT_PIPELINE.md). Client PRs are
independent of each other and can run in parallel across sessions; each owns
its client's files and `tests/playback/` rows tagged for it, nothing else.

| # | Scope | Acceptance |
|---|---|---|
| M0 | Fixture + this doc rendered from it + `scripts/player-contract-table` second fixture + `playback-surface-fence` (allow-listing every current site, so the fence lands red-free and the client PRs shrink its allow list to zero) | `make web-check` runs the fixture's `cases` against a reference reducer; `test_docs_index` green; fence green with its allow list |
| M1 | Web: adapter (`parseStreamFailure` moves in, hls.js fatal network → `recovering` with one `startLoad()` retry), `PlaybackPolicy.presentSurface`, `setLoading` demoted to render, input `failed` from the model, ledger section, log events | `web-policy.test.js` runs every `cases` row against the shipped `presentSurface` via `shippedSource()`; a mutation that removes the pause on `stopped` fails a test; Chrome + Safari smoke on a 4K remux with a mid-film network drop shows an indicator, not a black screen |
| M2 | Apple: `PlurxAPI.check` decodes `{code,message}` for every non-2xx; `PlaybackSurfaceModel` replaces the four fields; `fail()` and the 12 writers raise faults; lock-screen command routed; PiP error joins the model | `AppleClientTests` runs `cases`; `make apple-test` on `mba` green; a 15 s readiness timeout on a cold transcode shows `stalled` with a paused player and resumes on Keep waiting when the server publishes |
| M3 | Android: `PlaybackSurface` on a `StateFlow` replaces `playFailure`/`onError`; `Fail` pauses; the stall watchdog is told about the surface (it may raise `recovering`, never restart under `stopped`); refusal bodies read; `BEHIND_LIVE_WINDOW` recovers; `PlaybackFailed` opaque | `:app:testDebugUnitTest` runs `cases`; a test pins that `Fail` leaves `playWhenReady == false` and that a later `onIsPlayingChanged(true)` produces `surface_disagreement`, not a silent overlay |
| M4 | Physical verification on the Apple TV, an iPhone and an Android TV: the three reproduction recipes below | Recorded in a `PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-<date>.md` beside this page; `surface_disagreement` count over a 30 min session is zero or every instance is explained |

Reproduction recipes for M4 (and for confirming today's behaviour before
M1–M3 land): (a) play a 4K remux, pull the server's network for 5 s at
minute 2 — today: full-screen error over a moving picture; (b) seek during a
stream change on a cold NAS so the create 503s — today: sticky overlay over
the predecessor; (c) Android, a title with a sidecar subtitle (copy HLS),
seek forward 10 min — expected today if §6.1 is right: a restart at ~8 s, a
terminal overlay at ~16 s, picture still moving.

Prompt for the GPT session that has device access, for (c):

> On the Android TV plurx client, current build, open a film that has a
> sidecar `.srt` (so playback is a copy HLS session — Playback debug shows
> `method: remux`/`copy` and a session id). Let it play 30 s, then seek
> forward 10 minutes with one press. Watch for 25 s without touching
> anything. Report: did the picture come back and where (position shown vs
> requested); did it restart again around 8 s later; did an overlay reading
> "Playback couldn't reach the requested position after retrying" appear
> while the picture was moving. Then open Playback debug and copy the
> `target_ms` / `playback_target_timeout` lines from the client log. Repeat
> once with a transcode-quality selection (Auto → 1080p) to confirm it does
> not happen on a transcode session.

---

## 9. What needs a ruling

1. The eight classes and their surfaces (§3.1) — in particular that
   `preparing` and `recovering` **never** go full-screen once a frame has
   been presented on the generation, and that a readiness timeout is
   `stalled` (prompt, paused) rather than `stopped`.
2. The agreement rule's resolution (§3.2): the picture wins and the fault
   demotes to a banner. The alternative — re-pausing to make the overlay
   true — is defensible but it is the one that fights the lock screen.
3. Whether §6.1 (Android keyframe landing) is built inside M3 or as its own
   PR once the device check confirms it. My recommendation: its own PR;
   it is a timeline bug, not a surface bug, and M3 should stay reviewable.
