# Playback surface contract — the overlay is a projection of the player, not a message

**Status:** v2, 2026-09-13 — ruled (§9), reviewed
([review](PLAYBACK-SURFACE-CONTRACT-REVIEW.md) ·
[response](PLAYBACK-SURFACE-CONTRACT-REVIEW-RESPONSE.md)), ready to build
([implementation](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)) ·
**Investigated at:** `main` @ `10f2afe6`, evidence re-checked at `a124876` ·
**Source of truth once built:** `tests/playback/playback-surface-contract.json` ·
**Kept honest by:** `tests/playback/playback-surface-contract.test.js` (web), a
fixture test per native client, and `scripts/playback-surface-fence` (§5)

Companion to [PLAYER-INPUT-CONTRACT.md](PLAYER-INPUT-CONTRACT.md) (what a
press does) — this is *what the viewer is shown when playback is not simply
playing, on every client, and when it goes away*. Same shape as that contract,
for the same reason: the fault surfaces diverged per client because every
call site answered the question for itself.

The one-sentence version: **recovery owns the player and the presenter owns
the pixels — a failure-shaped event becomes a typed fault, the fault's class
picks the surface, a blocking surface is rendered only over a player its
recovery owner has already stopped, and the player's own presentation
evidence retires any fault that claimed the picture wasn't moving.**

What changed in v2 (after the adversarial review): the presenter has **no
side effects**. v1 had it pause the player when raising a blocking surface,
which made the overlay a recovery actor and contradicted the thesis. Now the
recovery owner — the client's existing ladder — stops the player when it
declares exhaustion, and the presenter merely renders that. Faults carry two
identities (attached media, requested intent), evidence is each client's
existing presentation proof rather than a bare clock, and the source table has
a precedence rule.

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
| Raising a blocking surface is not tied to the player being stopped | overlay over a moving picture |
| Presentation evidence does not retire the surface | overlay stays after playback resumed |
| A fault carries no class, no identity, no generation | "not yet" answers, automatic quality changes and stale refusals all get the same full screen |

The fix is therefore not to patch the call sites — there are 35 on the web, 12
on Apple, 8 on Android — but to take the decision away from them, the way the
input contract took key handling away from the views.

---

## 2. What the code does today

Every claim below was checked against the source at `10f2afe6` and re-checked
by the adversarial review at `a124876`. Line numbers are from `10f2afe6` and
will drift; the function names will not. "Reachable" means the mechanism
exists and its guards can pass, not that every reported incident took it.

### 2.1 Web — `#ploading` is loading, buffering, failure and prompt in one element

- The only full-screen surface is `#ploading` (`index.html:3404`, CSS 535-537).
  `setLoading(on,text,sub,actionHtml)` (`index.html:10998`) paints text into
  it directly from **35 call sites**; severity exists only as an ad-hoc
  combination of "buttons present", a `failed` class (added at exactly four
  sites: 4076, 6994, 10962, 11611) and `PLAYER.stallPrompt`. No CSS rule
  targets `.ploading.failed` — the spinner keeps spinning on every failure.
- **A fatal hls.js error never pauses the video.** hls.js only calls
  `stopLoad()`; the generic and explained branches of the handler
  (`6984-6995`) paint "Still preparing this stream…" or "Playback failed to
  start (fragLoadError)" while the `<video>` plays out up to 30 s of forward
  buffer. Neither sets `stallPrompt`, so when the buffer drains `waiting`
  repaints "Buffering…" (which also strips `failed`, `11002`) and
  `persistentWait` may reopen 8 s later — the recovering-stall branch
  (`6933`) and `persistentWait`'s own guards (`3932-3936`) take other paths,
  so the sequence is reachable, not universal.
- **A failed stream change is sticky over a playing predecessor.**
  `executePlaybackMediaChange` deliberately keeps the predecessor playing
  until the create succeeds (`9513-9515`) and, on failure, retains
  `pendingMediaChange` (`9562-9564`). `playbackOwnsAttachedMedia()`
  (`10649`) is then false, so `playing` (11462), `canplay` (11515),
  `waiting` (11527) and the progress tick all return early: "Playback could
  not reconnect." sits over a stream that is still playing until Try again or
  Close. `failPreparation` (`9039`) has the same shape.
- **`STREAM_FAILURE` is time-keyed, not attempt-keyed.** Every ≥400 response
  with a legible `{code|error, message}` body that hls.js sees is recorded
  (`6793`; `parseStreamFailure` rejects malformed or empty bodies,
  `playback-policy.js:695-706`), including a segment 503 that hls.js then
  retries successfully. `attachHls` clears it on every attach (`6746-6748`)
  and `LEVEL_LOADED` clears only *retryable* codes (`clearStreamFailureFor`,
  `6680-6685`); a VOD playlist is never reloaded, a terminal 4xx is never
  cleared by loading, and `closePlayer` does not clear it. So within one
  attach, for 90 s (`FAILURE_FRESH_MS`, `playback-policy.js:727`), any
  unrelated fatal is explained by a refusal hls.js already recovered from;
  and a next title that takes the progressive path (no `attachHls`) can have
  its start watchdog quote the previous title's refusal.
- **`waiting` paints before it is debounced.** The comment at 3798-3812
  documents Safari firing `waiting` at every fMP4 boundary on healthy 4K
  HEVC and `STALL_MIN_MS=350` exists for exactly that — but the `waiting`
  handler (`11526-11535`) paints "Buffering…" full-screen in the same tick,
  before the wait has lasted anything; only the *counting* is debounced.
- **Session-opening changes dim the whole picture.** Direct-play and VOD
  seeks are native (`v.currentTime=`, `11768`) and paint nothing; every
  change that opens a session — a seek on a growing session, an Auto
  downshift, a decode rescue, a subtitle burn — goes through
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
- `fail(_:)` (`PlayerController.swift:4758-4766`) is the writer for decision,
  create and readiness failures. It never pauses the player, and it chooses
  the blocking surface with `failed = player.currentItem == nil || error is
  PlaybackPreparationError` — "is an item attached", not "is the player
  stopped". The other blocking writers — `handleItemFailure` (`5240`), the
  stall `.stop` (`4258`), the server `terminal` (`4389`), the repeated early
  end (`5032`) and the HDR rung (`5942`) — do `player.pause()` first: Apple
  already has the stop-before-block property on those paths, and lacks it
  only in `fail()`. Node-failover readiness errors are swallowed rather than
  surfaced (`5384` at `a124876`).
- **A readiness deadline can become a full-screen stop over a running
  player.** `awaitItemReady` (`6073-6086`) throws
  `PlaybackPreparationError.timedOut` after 15 s; `reopen()`'s catch
  (`3284-3292`) and `load()`'s (`3248-3256`) call `fail()`, which sets
  `failed = true` because the error is a `PlaybackPreparationError` — while
  `player.play()` (`3655`) stands. Scope: the deadline is on `seekWhenReady`
  (every resume or seek that is *not* a native in-item seek — native VOD seeks
  go through `player.seek` without it, `2673` at `a124876` — and every copy
  session whose keyframe lead-in needs a seek) and on
  `shouldBoundFreshStartReadiness` (`1625-1632`: a fresh VOD start from 0
  that resumes playback, transcodes included), where
  `retryAfterReadinessTimeout` walks the compatibility ladder *before*
  `fail()`. On the paths that reach `fail()`, an item that readies after the
  deadline plays behind a black view; the periodic observer keeps advancing
  `currentMs` and Now Playing; the recovery monitor is off (`!failed`,
  `4125`).
- **Nothing clears `playbackError` on progress.** `playbackError = nil` is
  written only by `start`/`startOffline` (2141, 2245),
  `restartInitialDecision` (3152), `retryAfterPlaybackFailure` (2447) and
  `open()` entry (3356); the post-attach sites (2368, 3718) clear `failed`
  only. The periodic observer (`4916-4988`) touches neither field. A
  session-create failure during a seek or quality change leaves "Server
  returned 503" in the banner over a stream `restoreAfterFailedChange`
  (`3766-3782`) has resumed — until the next `open()`, which may be never.
- **Refusal bodies are discarded, and AVFoundation never had them.**
  `PlurxAPI.check` (`PlurxAPI.swift:192-205`) decodes `{code,message}` only
  for 409 — that covers session *create*. Playlist and segment refusals are
  fetched by AVFoundation, which surfaces only the status code through the
  item's error log (`errorStatusCode`), never the body; `handleItemFailure`
  reads that log (`5141`). So a 503 `startup_timeout` or
  `media_owner_transition` — documented as "retry" in
  [`PLAYBACK.md`](../PLAYBACK.md) — reaches the viewer as `APIError.http(503)`
  ("Server returned 503") on create, or, after the item fails, as a transport
  class that tries failover and the established-HDR retry (`5300`) before
  becoming terminal with AVFoundation's generic text. 410 `media_owner_lost`
  is 4xx → fatal, its `film_position_ms` unread.
- The iOS lock-screen `playCommand` (`6898-6906`) calls `player.play()`
  directly and resumes audio under the failure view; `togglePlayPause` is
  routed and correctly `ignore`d in `.failed`.
- The PiP *persistent* error (`showPersistentErrorMessage`,
  `PlayerSurface.swift:176-180`) clears on a later PiP start, on
  `isPictureInPicturePossible` and on `toggle()` — PiP events, never
  playback ones.
- The pre-start black-frame ladder (`handleBlackFrameDecodeFailure`, `6092`
  at `a124876`) exhausts by *doing nothing*: audio keeps playing over a
  black picture with no surface at all. Progress in the clock is not
  progress in the picture there.

### 2.3 Android — `playFailure` is set eight ways and cleared none

- `playFailure` is `remember { mutableStateOf<String?>(null) }`
  (`PlayerScreen.kt:674`), written by `onError = { playFailure = it }`
  (`691`) from eight Controller sites, and **never set back to null** — the
  only dismissal is `PlayerContent` leaving composition (Retry → reload,
  or Exit). `onError` is `(String) -> Unit` (`Controller.kt:130`): no
  class, code, retryability or "player still running" bit crosses it.
- `PlaybackFailed` (`PlayerScreen.kt:621-645`) is a transparent
  `Column(fillMaxSize)` with no background: the video keeps rendering behind
  the text. It is blocking only by gating — chrome and tap surface removed,
  input state `Failed`, in which the contract routes play/pause to `ignore`
  — so the viewer cannot even pause the stream still playing beneath it.
- **`Fail` never pauses, and the stall watchdog can restart the stream under
  the overlay.** `onPlayerError` → `PlaybackErrorAction.Fail` → `onError`
  (`653-661`) leaves `playWhenReady` true; `stallWatchdogJob` (`816-846`) is
  cancelled only in `release()`. ExoPlayer sits in `STATE_IDLE` with a frozen
  position; if the app is foreground with no pending seek (`825-826`), the
  tracker fires after 8 s, `onStall` (`1428-1460`) checks intent and budget
  and calls `restartAt`/`reopenAfterStall`. Playback resumes; `playFailure`
  stays. Reachable, not inevitable — and the literal shape of "stopped and
  then resumed even though the overlay is still there".
- Paths B–G (session create failed `1281`, stall reopen failed `1544`,
  terminal verdict `1337`, sessionless second stall `1444`, budget exhausted
  `1457`, target deadline `2174`) never stop ExoPlayer at all; B in
  particular fires while the old item is still playing from buffer.
- **Refusal bodies are never read.** A playlist/segment 503 becomes
  `ERROR_CODE_IO_BAD_HTTP_STATUS` after ExoPlayer's default retries, is
  tried once on another ingress if the cluster has one
  (`retryMediaOnNextNode`, `553`), may take the established-HDR retry
  (`PlaybackPolicy.kt:72`), and then goes to `Fail` ("Playback stopped
  (ERROR_CODE_IO_BAD_HTTP_STATUS)."); the web shows "Still preparing this
  stream…" for the same answer. Session-create exceptions are not classified
  (`AppViewModel.kt:791-798`). `BEHIND_LIVE_WINDOW` (1002) also reaches
  `Fail` (`PlaybackPolicy.kt:77`).
- No Android test references `onError`, `playFailure`, `playbackNotice` or
  `PlaybackFailed` (Apple's `AppleClientTests` does pin `playbackNotice`).
- **Field evidence, 2026-09-13.** Paul's Android tablet, mid-film: "Playback
  stopped (ERROR_CODE_IO_BAD_HTTP_STATUS)." with Retry / Back, the picture
  visibly still playing behind the transparent overlay
  ([photo](../img/android-bad-http-status-overlay-20260913.jpg)). The
  overlay and its transparency are what the photo proves; the watchdog
  restart is the plausible reason the picture moves, not a logged one — the
  Surface ledger (§5) exists so the next photo comes with its cause.

### 2.4 The common shape

```
 TODAY                                     PROPOSED
 ─────                                     ────────
 event ──▶ call site decides ──▶ view      event ──▶ ADAPTER ──▶ PRESENTER ──▶ SURFACE
           text · full-screen?             (raw → fault    (pure: faults ×      #ploading / banner /
           buttons? failed flag?            + identities)   evidence → surface)  failureView / indicator
           (35 / 12 / 8 sites)                                   ▲               rendered FROM state
 player ──▶ progress callbacks exist,      RECOVERY OWNER ───────┘
            but no site is told the       (ladders, budgets, pause/stop —
            surface it painted is stale    unchanged; declares exhaustion)
```

Nothing in the left column is a *wrong* decision in isolation — each site was
written for one fault and is defensible for it. The defect is that the
decision is made at the site, so the sites disagree, and no site learns that
the surface it painted has gone stale.

---

## 3. The contract

### 3.0 Two owners, one rule each

**The recovery owner owns the player.** It is whatever code decides
reopens, fallbacks, failover and budgets today — `persistentWait` and the
hls.js/`<video>` error handlers on the web, `handleItemFailure` /
`retrySameDeliveryAfterStall` / the compatibility ladder on Apple,
`onPlayerError` / `onStall` / the target deadline on Android. Nothing about
it changes in M0–M4. It gains exactly one obligation: **when it has nothing
left to try, it stops the player and says so** (raises an `exhausted` fault)
— which five of its six Apple paths already do (§2.2).

**The presenter owns the pixels.** It is a pure function of (faults,
evidence, identities) → surface. It never pauses, resumes, reopens, cancels a
timer or reports to the control plane. Every side effect v1 gave it is gone:
that is the whole of the review's first and third blockers, and it is also
what makes the reducer testable row by row.

### 3.1 Faults — a closed set of classes

A **fault** is:

```
{ class, source, attached: <media generation>, intent: <request id>,
  raised_at, position_ms?, title?, detail?, actions: [...], expires? }
```

`class` is one of the rows below. `source` is the raw event that produced it
(§3.3), kept for the ledger. `attached` is the media generation the fault is
*about* (web `p._seekToken`/open attempt, Apple `openGeneration`, Android
`mediaMutationEpoch`); `intent` is the viewer's request it belongs to, when
it belongs to one (a seek, a quality change — web `controlSeek`, Apple
`viewerActionEpoch`, Android `PlaybackIntent.sequence`). A fault about a
*pending destination* has an `intent` and is retired only by that intent
settling or being superseded; a fault about the *attached media* has no
`intent` and is retired by that media's evidence. That distinction is the
review's second blocker: a predecessor moving does not forgive a replacement
that failed.

| Class | Severity | Surface | Who stopped the player | Retired by |
|---|---|---|---|---|
| `preparing` | progress | spinner + stage text — full-screen while the attached picture is not presenting, in-chrome indicator while it is | nobody | presenting evidence on `attached` · intent settled |
| `buffering` | progress | same rule; raised only once the wait has lasted `STALL_MIN_MS` (350 ms) | nobody | presenting evidence · the viewer no longer wanting media |
| `recovering` | progress | same rule, with the reason ("Reconnecting…", "Switching quality…") | nobody | presenting evidence on the *new* attached generation · owner reports success |
| `hold` | notice | banner, timed (30 s), server sentence | nobody | timer · presenting evidence |
| `degraded` | notice | banner, timed (5 s) | nobody | timer |
| `refused` | notice | banner, **untimed**, with actions: a viewer-requested change failed and the previous stream continues | nobody | intent superseded (a later change succeeds or the viewer asks again) · presenting evidence ≥ 10 s *of continuous playback* on `attached` |
| `exhausted` | prompt | full-screen: "Playback is stalled." + Keep waiting / Try again / Close (plus Force transcode where the owner offers it) | **the owner, before raising** | user action only |
| `stopped` | terminal | full-screen: title + sentence + Try again / Close (+ Sign in for 401/403) | **the owner, before raising** | user action only |

<!-- contract:surface-classes:begin -->

_Generated from [`tests/playback/playback-surface-contract.json`](../../tests/playback/playback-surface-contract.json) by `scripts/player-contract-table`; do not edit by hand._

| Class | Severity | Blocking | Timer | Requires the owner to have stopped the player | Retired by | Default actions |
|---|---|---|---|---|---|---|
| `preparing` | progress | while not presenting | none | no | `presenting` · `intent_settled` · `attached_retired` | – |
| `buffering` | progress | while not presenting | none | no | `presenting` · `playback_not_requested` · `attached_retired` | – |
| `recovering` | progress | while not presenting | none | no | `presenting_after_raise` · `owner_success` · `attached_retired` | – |
| `hold` | notice | never | 30000 ms | no | `timer` · `presenting_after_raise` | – |
| `degraded` | notice | never | 5000 ms (paused while it has actions) | no | `timer` · `presenting_continuous_ms` | – |
| `refused` | notice | never | none | no | `intent_superseded` · `presenting_continuous_ms` | `retry` |
| `exhausted` | prompt | always | none | yes | `user` | `keep_waiting` · `retry` · `close` |
| `stopped` | terminal | always | none | yes | `user` | `retry` · `close` |

| Severity | Rank | Meaning |
|---|---|---|
| notice | 1 | a banner beside the picture |
| progress | 2 | the player is working on it |
| prompt | 3 | the viewer has to answer |
| terminal | 4 | this attempt is over |

Rows are scanned in order and the first row whose id and context both match wins. The highest-severity drawable fault owns the surface; faults of equal severity are broken by recency, newest first.

**Surface kinds:** `none` — Nothing is drawn over the picture. `indicator` — In-chrome progress indicator; the picture is presenting behind it. `banner` — A notice strip with the fault's actions; the picture is untouched. `blocking` — The picture is covered. Only ever drawn over a player its recovery owner has already stopped, or over a picture that is not presenting.

**The input contract's `failed` state** is a `blocking` surface whose class is `exhausted` or `stopped`. The player input contract's `failed` state is a BLOCKING surface whose class is a prompt or a terminal — a fault with actions the viewer must answer. A full-screen `preparing`/`buffering`/`recovering` is blocking pixels, not routing: it keeps today's routing, per PLAYBACK-SURFACE-CONTRACT.md §4.

| Timing | Value |
|---|---|
| `buffering_min_ms` | 350 |
| `hold_notice_ms` | 30000 |
| `degraded_notice_ms` | 5000 |
| `refused_progress_ms` | 10000 |
| `disagreement_notice_ms` | 30000 |

- refused_progress_ms is CONTINUOUS presenting on the attached generation, not accumulated playback.
- buffering_min_ms debounces the surface, not the fault: a `media_waiting` fault exists from the moment it is raised and is simply not drawn until it has lasted this long.
- A hidden page freezes every timer as well as every evidence sample. The reducer does this by carrying the hidden interval forward: on becoming visible again every fault's raise time is shifted by however long the page was hidden, and the continuous-presenting clock is reset, because no sample bridged the gap.
- disagreement_notice_ms retires the "Playback recovered" banner a demotion leaves behind (§3.2). Its actions stay valid while the picture could still fail again; this much CONTINUOUS presenting is the point at which the offer is stale. Ruled by the implementer 2026-09-13 in Paul's absence — §3.1 says such a banner does not expire "while an action is still valid" and does not say when that ends; a banner with no end is the Android `playFailure` defect wearing a different hat.
- presenting_after_raise is the contract's "presenting evidence on the NEW attached generation": what makes the evidence count is that the run of presentation BEGAN at or after the fault was raised. An owner that reopens in place, on the same generation, produces exactly that, and a recovering indicator that only a new generation could retire would outlive every in-place recovery.

<!-- contract:surface-classes:end -->

`exhausted` replaces v1's `stalled`, and the rename is the point: it is raised
by the recovery owner *after* its ladder and budgets are spent and *after* it
has stopped the player, never by a deadline that still has a rung to try. On
Apple that is the site of today's `.stop(terminal)`; on Android the two
budget-exhausted `onError` calls (`1444`, `1457`) plus the target-deadline
terminal (`2174`); on the web `showStallRecoveryFailure` and the
`stallRecoveryAction === 'prompt'` branch. A readiness deadline that fires
with rungs left (`retryAfterReadinessTimeout`, the Android target-deadline
`recover`) raises `recovering`, not `exhausted`.

**Keep waiting** is an action the *owner* implements, and the fixture only
names it: web re-arms `armStall` and clears `recoveringStall`; Apple runs
`retryAfterPlaybackFailure` (which resets `recoveryReopenBudget` and only
that, `2447-2453`); Android resets `stallReopenBudget` and
`sessionlessStallRecoveryUsed` and sets `playWhenReady = true`. Each is one
more bounded attempt; a prompt that comes back is the ladder spent again, and
that is the honest answer.

**Actions are a property of the fault, not the class.** The vocabulary is
`keep_waiting · retry · close · force_transcode · sign_in`; the owner sets
them when it raises. A fault demoted by the agreement rule (§3.2) keeps its
actions in the banner, and the banner does **not** expire while an action is
still valid — the 5 s `degraded` timer applies to notices with no actions.

### 3.2 The agreement rule

> A blocking surface is rendered only over a player its recovery owner has
> stopped. If the attached media presents progress while a blocking surface
> is up, that is a **disagreement**, and it is resolved in favour of the
> picture.

The presenter checks, not enforces: `exhausted`/`stopped` faults carry
`player_stopped: true` from the owner, and the fixture refuses a blocking
class without it. If evidence nevertheless arrives on the same `attached`
generation — the iOS lock-screen `playCommand` (`6898`), a browser autoplay
policy, a bug — the presenter demotes the fault to `degraded` ("Playback
recovered"), keeps its actions and data (a `media_owner_lost` still carries
`film_position_ms` and its Try again, so when the buffer drains the viewer
gets the specific reopen), clears the blocking surface, and emits
`surface_disagreement {class, source, attached, intent, position_ms}` to the
client log and the ledger (§5). No timer is cancelled and nothing is
re-paused: the owner's detectors are what will catch the next stall, and they
are running because the owner never stopped them — the presenter has nothing
to release.

Ruled 2026-09-13: the picture wins. The alternative — re-pausing to make the
overlay true — is the one that fights the lock screen.

### 3.3 Sources — what each raw event becomes

The mapping lives in the fixture so all three clients agree. Rows are
evaluated in order and **the first matching row wins**; the two columns that
disambiguate are *context* (start · attached playback · pending change) and
*owner state* (rungs left · exhausted).

| # | Raw event (context) | Class | Notes |
|---|---|---|---|
| 1 | Any refusal or error while the owner reports **exhausted** and has stopped the player | `stopped` | title from context; sentence = armed `terminal` verdict message if one exists **and the failure is not transport-class** (the existing exception, `PlayerController.swift:5327` at `a124876`, `Controller.kt:657`), else the client sentence |
| 2 | Owner budget spent, picture frozen, owner stopped the player | `exhausted` | Keep waiting / Try again / Close; Force transcode on the web's diagnosed-stall path |
| 3 | 401 / 403 on any playback request | `stopped` | `sign_in` action; keeps the status-based auth match the app has (`AppModel.swift:288`) |
| 4 | 409 `vod_source_rescan_required`, 422 `vod_source_unsupported` / unsupported tracks, 501 `vod_transcode_unavailable` / `vod_subtitle_burn_unavailable` (start) | `stopped` | server sentence; no retry |
| 5 | 503 `vod_disabled` (start) | `stopped` | server sentence; no retry — service is off, not building |
| 6 | 503 `startup_timeout` / `media_owner_transition` / `vod_index_pending` / `vod_engine_unattested` on create (start) | `preparing` | "still building"; the owner retries the same request identity on 1 s · 2 s · 4 s inside an absolute 60 s deadline and then raises `exhausted` (M5, landed). **One exception, by call site:** the web's `startCopyHls` answers `vod_index_pending` with its existing progressive-remux fallback instead of retrying — that path already has a strictly better local answer, and waiting seven seconds for a stream it can play now would be a regression dressed as a recovery |
| 7 | Any create failure, decision timeout or cancelled request during a **pending change** (predecessor still attached) | `refused` | `intent` = the change; actions `retry` (re-issue the change); the predecessor keeps its own faults |
| 8 | Playlist/segment 503 with a "not yet" code, attached playback | `recovering` | hls.js keeps `stopLoad()`; the owner's existing reopen is the recovery; natives see only the status code (§2.2) and take failover/HDR retry first |
| 9 | 410 `media_owner_lost` | `recovering` while buffered media plays out, then `stopped` when the owner stops | D1's shape; `position_ms` from the body (web) or the last observed position (natives); Try again reopens there |
| 10 | Control-plane `terminal` verdict | *(not a source)* | ruling D1: arms wording, never tears down; consumed by row 1 |
| 11 | Control-plane `hold` / `retry_resource` | `hold` | unchanged |
| 12 | Media `waiting` / `waitingToPlayAtSpecifiedRate` / `STATE_BUFFERING` after start, lasting ≥ 350 ms | `buffering` | |
| 13 | Owner starts a reopen / rescue / fallback / node failover / compatibility step / `BEHIND_LIVE_WINDOW` recovery (M5) | `recovering` | reason text from the owner; Apple's "Dolby Vision did not start. Retrying…" is this class |
| 14 | Readiness deadline fires with rungs left (`retryAfterReadinessTimeout`, Android target-deadline `recover`) | `recovering` | never a prompt while a rung remains |
| 15 | Decoder failure after the ladder is spent; web probe verdicts ("looks blocked", "won't play"); Apple black-frame ladder spent | `stopped` (`exhausted` if the owner leaves audio running, so the viewer is told and offered Close) | closes the silent black-picture case (§2.2) |
| 16 | Repeated early end at the same position < 95 % | `stopped` | unchanged |
| 17 | Auto downshift, decode rescue, HDR-subtitle, PGS, PiP failure | `degraded` | notices, unchanged in copy |
| 18 | Prepared-successor abandonment, `session_gone` on a successor's first exchange, telemetry / reporter / stats failures | (none) | log only — the incumbent is untouched; the fence keeps it so |

<!-- contract:surface-sources:begin -->

_Generated from [`tests/playback/playback-surface-contract.json`](../../tests/playback/playback-surface-contract.json) by `scripts/player-contract-table`; do not edit by hand._

Rows are evaluated in order; the first row whose `context` matches wins.

| # | Source | Context | Class | Requires a stopped player | Actions | Retired by override | Notes |
|---|---|---|---|---|---|---|---|
| 1 | `owner_stopped` | any | `stopped` | yes | class default | – |  |
| 2 | `owner_exhausted` | any | `exhausted` | yes | class default | – |  |
| 3 | `startup_exhausted` | any | `exhausted` | yes | `close` · `retry` | – |  |
| 4 | `hls_init_invalid` | start | `stopped` | yes | class default | – |  |
| 5 | `hls_init_unsupported` | start | `stopped` | yes | class default | – |  |
| 6 | `auth_401_403` | any | `stopped` | yes | `sign_in` · `close` | – |  |
| 7 | `vod_source_rescan_required` | start | `stopped` | yes | class default | – |  |
| 8 | `vod_source_unsupported` | start | `stopped` | yes | class default | – |  |
| 9 | `vod_transcode_unavailable` | start | `stopped` | yes | class default | – |  |
| 10 | `vod_subtitle_burn_unavailable` | start | `stopped` | yes | class default | – |  |
| 11 | `vod_disabled` | start | `stopped` | yes | class default | – |  |
| 12 | `create_503_not_yet` | start | `preparing` | no | class default | – | codes: `startup_timeout` · `media_owner_transition` · `vod_index_pending` · `vod_engine_unattested`; retryable by the owner (M5) |
| 13 | `client_preparing` | any | `preparing` | no | class default | – |  |
| 14 | `change_failed` | change | `refused` | no | `retry` | – |  |
| 15 | `segment_503_not_yet` | attached | `recovering` | no | class default | – | codes: `startup_timeout` · `playlist_state_changed` · `segment_pending` · `segment_wait_busy` · `node_wait_capacity` · `media_owner_transition` · `vod_resurrection_unavailable` · `response_owner_transition` · `response_state_changed` · `response_owner_reclassification_unavailable` · `response_publication_timeout` · `response_completion_capacity` · `response_snapshot_capacity` · `node_maintenance` · `node_removal_fenced` · `learner_route_ineligible` |
| 16 | `media_owner_lost_410` | any | `recovering` | no | class default | – | re-classes to `stopped` when the owner stops; carries `position_ms` |
| 17 | `control_hold` | attached | `hold` | no | class default | – |  |
| 18 | `system_interruption` | attached | `hold` | no | class default | `system_resumed` |  |
| 19 | `media_waiting` | attached | `buffering` | no | class default | – |  |
| 20 | `owner_recovery_step` | any | `recovering` | no | class default | – |  |
| 21 | `readiness_deadline_rungs_left` | any | `recovering` | no | class default | – |  |
| 22 | `decoder_failed` | any | `stopped` | yes | class default | – |  |
| 23 | `black_frame_ladder_spent` | start | `exhausted` | yes | `close` · `retry` | – |  |
| 24 | `repeated_early_end` | attached | `stopped` | yes | class default | – |  |
| 25 | `degraded_notice` | any | `degraded` | no | class default | – |  |
| 26 | `log_only` | any | *(log only)* | no | class default | – |  |

| Fixture error | Meaning |
|---|---|
| `blocking_without_stop` | A source whose CLASS is blocking was raised with player_stopped false. The reducer logs the error and leaves the surface unchanged; it never renders the fault. Every source mapping to a blocking class carries `requires.player_stopped` so the table and the reducer cannot disagree. |
| `source_context_mismatch` | A source was raised in a context no row for that id declares. |
| `unknown_source` | A source id that is in no row was raised. |

- A fault about the ATTACHED media carries `attached` and no `intent`; it is retired by that media's presentation evidence.
- A fault about a PENDING DESTINATION carries an `intent`. Evidence never retires it, with one exception the class table states outright: `refused` also retires on `refused_progress_ms` of continuous presentation, because a viewer whose picture has been fine for that long has been told.
- A fault whose `attached` generation is retired stops being about anything and is dropped (`surface_cleared {by: attached_retired}`). That is identity, not one of the class's `retired_by` rules, so it applies to blocking faults too — otherwise a `stopped` prompt would outlive the attempt it described and sit over the next one.
- A retirement reason normally comes from the CLASS, but a source may declare an explicit `retired_by` override when its lifecycle is narrower. `system_interruption` uses that override because `system_resumed` retires that hold without retiring unrelated holds. `attached_retired` is an identity rule rather than either kind of retirement declaration, and the note above says why.
- `owner_success: G` is the recovery owner reporting that its recovery produced attached generation G. There is one recovery owner per player, so it retires every `recovering` fault, not only the ones about the generation it replaced.
- `playback_not_requested` is the viewer no longer wanting media. A `buffering` fault is about a player that WANTS it — the wait is only a wait while something is trying to play — so a pause makes the fault about nothing and it is retired. It is on `buffering` and on no other class: a `preparing` start has not been paused by a viewer who has not seen it yet, and a blocking prompt is answered by the viewer rather than by a transport change. Resuming raises nothing back; the raise sites decide what comes back, which is the same rule every other retirement follows.

<!-- contract:surface-sources:end -->

`client_preparing` was added to the fixture by M1 (`web/playback-surface`) and
is the one row above that the v2 table did not have. The staged loading overlay
every client paints while it is opening a stream — "Reading media…", "Starting
the transcoder…", "Preparing the stream…" — is a `preparing` fault with no
refusal behind it, and the only row that produced `preparing` was the create
503. It is the same class, so nothing about what is drawn changes; what changes
is that the ledger can name the ordinary case instead of borrowing
`owner_recovery_step` and calling a cold start a recovery.

Rows 6, 8 and the `BEHIND_LIVE_WINDOW` clause of 13 named recovery steps the
clients did not all perform; those are **M5**, their own PR (ruled), and it has
landed. Row 6 is now reachable on all three clients: the owner re-posts the same
create identity on 1 s · 2 s · 4 s inside an absolute 60 s deadline, releases a
success that arrives after it, and raises `exhausted` when the ladder or the
deadline is spent — in the `start` context only, ladder AND deadline alike,
because a create refused over a predecessor is row 7 and a prompt that stops
that player is §7 recipe (b)'s not-allowed outcome. **On the web the 60 s is not
the operative bound:** `beginPlaybackPreparation` gives every open a
pre-existing 20 s absolute deadline and the sequence runs inside it. **Ruled
2026-09-13 (implementation §4.6, ruling 2): the 20 s stands and the OUTCOME is
what changes.** When that deadline ends a sequence the server has already
answered "not yet" to, the sequence's own `exhausted` is what surfaces, with the
server's own sentence — so the web reaches `exhausted` either by spending the
ladder (~7 s) or on the preparation clock, and `owner_stopped` "Playback could
not prepare." is left to a create that refused nothing and was merely slow. Row
8 gains the web's one bounded `hls.startLoad(position)`
two seconds after the refusal, sharing a single per-attach budget with the
network-class fatal. Row 13's `BEHIND_LIVE_WINDOW` clause is Android's
`seekTo(lastRealPosition)` + `prepare()`, once per attach, on FINITE timelines
only — Media3's `seekToDefaultPosition()` is a live-edge policy and is not used,
and a live item keeps today's `Fail`.

### 3.4 Evidence — the player's own proof of presentation, not a clock

"Presenting" means the client's existing presentation proof says so; the
presenter adds no detector and never infers from an event:

- **Web:** `playbackProgressTick.moved` (`index.html:11216`) — clock advance
  *and*, where a frame counter exists, frame advance — on the attached
  element, not hidden, not seeking, `wantsPlayback` true. `playing` and
  `canplay` only prompt a look.
- **Apple:** the periodic observer's position delta (`4916`) while
  `!isChangingStream` and `rate > 0`, *and* for a not-yet-established item
  the first-frame proof the black-frame ladder uses — audio advancing over a
  black picture is not presenting.
- **Android:** `presentedVideoFrame` / `realPosition()` advance on the
  current `mediaMutationEpoch`, foreground, `playWhenReady` true.

A fault with an `intent` is never retired by evidence alone: the destination
must settle (`markPlaybackControlSeekExecuted` / seek generation settled /
`pendingSeek == null`) or be superseded. A fault about `attached` generation
N is dropped the moment N is retired, which is the existing
`playbackOwnsAttachedMedia` / `isSuperseded(generation)` / epoch test — that
replaces the 90 s `FAILURE_FRESH_MS` window. A refusal hls.js already
recovered from within the same attach is retired by the recovery
(`FRAG_LOADED` for the same fragment), not by the next fatal.

Remote and background presentation is declared, not guessed: AirPlay and
PiP report presenting through the platform's own flags
(`isExternalPlaybackActive`, `isPictureInPictureActive`, the web
`webkitCurrentPlaybackTargetIsWireless`) and a hidden page samples nothing —
a fault raised before the page was hidden is neither cleared nor promoted
until it is visible again.

### 3.5 The fixture

`tests/playback/playback-surface-contract.json`, in the shape of
`player-input-contract.json`, with cases that are **ordered event
sequences**, not snapshots — the review's seventh finding:

```json
{
  "schema_version": 1,
  "classes": {
    "exhausted": {"severity": "prompt", "blocking": true,
                  "requires_player_stopped": true,
                  "retired_by": ["user"], "title": "Playback is stalled.",
                  "default_actions": ["keep_waiting", "retry", "close"]},
    "refused":   {"severity": "notice", "blocking": false, "timed": false,
                  "retired_by": ["intent_superseded", "progress_10s"],
                  "default_actions": ["retry"]},
    "preparing": {"severity": "progress", "blocking": "while_not_presenting",
                  "retired_by": ["presenting", "intent_settled"]}
  },
  "sources": [
    {"id": "create_503_startup_timeout", "context": "start",
     "class": "preparing", "retryable": true},
    {"id": "create_failed_pending_change", "context": "change",
     "class": "refused", "actions": ["retry"]},
    {"id": "owner_exhausted", "context": "any", "class": "exhausted"}
  ],
  "actions": ["keep_waiting", "retry", "close", "force_transcode", "sign_in"],
  "timings": {"buffering_min_ms": 350, "hold_notice_ms": 30000,
              "degraded_notice_ms": 5000, "refused_progress_ms": 10000},
  "cases": [
    {"name": "a failed quality change never blocks the predecessor",
     "events": [
       {"t": 0,    "attach": "g1", "presenting": true},
       {"t": 1000, "intent": "i7", "raise": "create_failed_pending_change"},
       {"t": 2000, "presenting": true, "attached": "g1"},
       {"t": 30000, "presenting": true, "attached": "g1"}],
     "expect": [
       {"at": 1000, "surface": "banner", "class": "refused", "actions": ["retry"]},
       {"at": 30000, "surface": "banner", "class": "refused"}]},
    {"name": "exhausted without a stopped player is refused by the fixture",
     "events": [{"t": 0, "attach": "g1"},
                {"t": 500, "raise": "owner_exhausted", "player_stopped": false}],
     "expect": [{"at": 500, "error": "blocking_without_stop"}]},
    {"name": "stopped, then an outside play() — the picture wins",
     "events": [{"t": 0, "attach": "g1"},
                {"t": 100, "raise": "decoder_failed", "player_stopped": true,
                 "actions": ["retry", "close"]},
                {"t": 5000, "presenting": true, "attached": "g1"}],
     "expect": [{"at": 100, "surface": "blocking", "class": "stopped"},
                {"at": 5000, "surface": "banner", "class": "degraded",
                 "actions": ["retry", "close"], "log": "surface_disagreement"}]},
    {"name": "canplay on a stopped player is not presenting",
     "events": [{"t": 0, "attach": "g1"},
                {"t": 100, "raise": "decoder_failed", "player_stopped": true},
                {"t": 800, "event": "canplay"}],
     "expect": [{"at": 800, "surface": "blocking", "class": "stopped"}]}
  ]
}
```

`cases` is the table every client's presenter is run against, row for row.
The doc's §3 tables are rendered from the fixture by
`scripts/player-contract-table` once it learns a second fixture, so this page
cannot drift from the contract.

---

## 4. How a client implements it

Three layers, and the middle one is the only one allowed to decide *what is
shown*. It is the input contract's diagram with the nouns changed.

```
 raw event ─────▶ ADAPTER ─────▶ PRESENTER ─────────────▶ SURFACE
 (hls.js ERROR,   raw → fault    pure reducer:            #ploading / banner /
  item.status,    + identities   (faults, evidence,        failureView / indicator
  PlaybackExc.,   + actions      identities) → surface     rendered FROM state
  503 body/code)                 + log entries only
 evidence ───────────────────────────┘
 RECOVERY OWNER ── ladders · budgets · pause · stop ── raises exhausted/stopped
                   (unchanged; must stop before raising a blocking fault)
```

- **Adapter** — turns a raw outcome into a fault with identities via the
  fixture's `sources`. On the natives this is also where the create-call
  `{code,message}` body gets read: Apple `PlurxAPI.check` decodes it for
  every non-2xx into `APIError.refused(status:code:message:position_ms:)`
  **while keeping `.http(status)` for bodiless answers, so the status-based
  auth match (`isSessionExpired`) and every existing retry matcher keep
  working**; Android reads the create-call body in `createHlsSession` and
  `HttpDataSource.InvalidResponseCodeException.responseBody` for
  playlist/segment refusals. AVFoundation media refusals carry a status code
  only (§2.2); the Apple adapter classifies on the code plus the session
  status poll it already runs (`startStatusPolling`), and never claims a body
  it cannot have. The web's `parseStreamFailure` moves in and grows a
  `position_ms` field.
- **Presenter** — one pure reducer per client, no platform imports, no
  effects except log entries, signature-equivalent to
  `present(state, event) → (state, log[])`, where `event` is a fault, an
  evidence sample, a generation/intent change, a timer tick or a user
  action. Web: `PlaybackPolicy.presentSurface` next to `routeInput`; Apple: a
  `PlaybackSurfaceModel` value published as **one** field on
  `PlayerController` replacing `failed`, `playbackError`,
  `playbackFailureTitle` and `playbackNotice`; Android: a `PlaybackSurface`
  sealed class on a `StateFlow` replacing `playFailure`, `playbackNotice`
  and the `onError` string callback. Each runs every `cases` sequence.
- **Surface** — renders from the presenter's state and nothing else.
  `setLoading()` becomes the private render function of the web presenter's
  output; `failureView` and `PlaybackFailed` read the model; `PlaybackFailed`
  gets an opaque background because the player behind it is now stopped.
  The input contract's `failed` state is **the presenter's "a blocking
  surface with actions is up"**, not a DOM class — `exhausted` and `stopped`
  only; a full-screen `preparing` keeps today's routing, and the
  cross-product (surface × class × input) is pinned by a test so a change to
  which prompts enter `failed` is a visible diff.
- **Recovery owner** — unchanged code, one new obligation at each
  exhaustion site: stop the player, then raise. Web: `pausePlaybackInternally`
  + `stopPlayerTimers` at `showStallRecoveryFailure` and the prompt branch;
  Apple: already `player.pause()` at five of six sites, `fail()` gains it for
  the readiness-exhausted path only; Android: `playWhenReady = false` before
  the three exhausted `onError` calls, which also resets the stall tracker
  (`PlaybackTelemetry.kt:306-309`) — the restart-under-overlay path closes
  by construction.

The rule that keeps the moles from coming back: **no surface write outside
the presenter's render.** `scripts/playback-surface-fence` runs from `make
validate-staged` beside `player-input-fence` and fails on **writes** — web:
`setLoading(`, `classList.add("failed")`, `classList.remove(…"failed"…)`,
`stallPrompt=`; Apple: assignments to `failed`, `playbackError`,
`playbackFailureTitle`, `playbackNotice` on `PlayerController`; Android:
`onError(` calls and assignments to `playFailure`, `playbackNotice` — in the
finite player's shipped files, with the same allow-list-with-a-reason shape
as the input fence, positive **and** negative fixtures (a file that must
trip, a file that must not), and a scan that is spelling-complete for
assignment, `+=`, bulk `Object.assign`/`apply`, and class toggles. The fence
says nothing about *reading* raw errors: ladders, readiness code and
diagnostics keep every `errorCode`, `status` and `details` reference they
have. Scope is the finite player; `LibraryChannels.swift`,
`OfflinePlayerScreen.kt`, `OfflineDownloadManager.swift` and the web reader
are outside it (§7), and the web Library Channels path, which shares
`play(...)`, is inside it because it shares the renderer.

What does **not** change: the recovery ladders, the control-plane verdict
semantics (D1 included), the stall detectors and their thresholds, the
budgets, the input routing table. M5 is the only PR that changes what the
player does, and it is its own PR by ruling.

---

## 5. Attributability — the ledger, the log, the fence

Anything that dims the picture on Paul's Apple TV has to be answerable from
inside the product. The presenter keeps a bounded ring (last 16) of
`{class, source, attached, intent, session_id, attempt, raised_at,
cleared_at, cleared_by, player_at_raise: {rate, position_ms, presenting,
stopped_by_owner}}` and:

- the **Playback debug ledger** on all three clients gets a *SURFACE* section
  (joins `tests/playback/playback-info-fields.json` as a new section with
  `current_surface`, `current_fault`, `surface_history`) — "what was that
  overlay" has an answer without a log file;
- the client log gets `surface_raised`, `surface_cleared` and
  `surface_disagreement` events with the same fields, so the server-side
  session log joins on `session_id`/`attempt`, not on a client-local
  generation;
- `surface_disagreement` is the one to watch after the build lands: each
  occurrence is a place where something started the player after its owner
  stopped it, and each ends as either a fixed call site or a documented
  exception (the lock-screen command, for one).

Nothing here is gated. Per the standing ruling, if any part of this needs an
operator decision it goes in the Developer tab as an advisory enable section;
as proposed, nothing does.

---

## 6. What the migration closes, and two things it does not

Closed by construction (the presenter cannot express them, or the owner's
new obligation forbids them):

- **Web:** fatal-over-live-buffer painted as "failed to start"; sticky
  "could not reconnect" / refusal over a playing predecessor; stale
  `STREAM_FAILURE` across attempts and titles; "Preparing the stream…"
  full-screen for an automatic downshift; undebounced "Buffering…" flashes;
  `failed` routing reachable only from four sites.
- **Apple:** the readiness-exhausted stop over a running player; "Server
  returned 503" banner that only `open()` clears; create refusal bodies
  discarded; the PiP error that only PiP can clear; the lock-screen `play()`
  under a black view (now a logged disagreement that resolves to the
  picture); the silent black-picture exhaustion.
- **Android:** `playFailure` never nulled; the stall watchdog restarting the
  stream under the overlay; `Fail` without a stop at the exhausted sites;
  transparent failure view over a running player; refusal bodies unread; no
  test on any failure surface.

Two findings from the same investigation are **separate defects** and are
listed so they are not mistaken for this one:

1. **Android progressive-remux seeks may never "land".** Since
   `live_presentation_removed` (`transcode.rs:17115` at `a124876`) every HLS
   session is VOD and Android seeks those natively (`executeSeek`, `986`),
   so the copy-HLS variant of this finding is retired. The progressive remux
   path remains: a seek sets a new item with no start position
   (`Controller.kt:970-983`) and the server's `X-Plurx-Media-Origin-Ms` is
   the *preceding keyframe* (`stream.rs:2886-2896`) — unless the one-second
   origin probe times out, in which case the server reports the requested
   start (`stream.rs:55-70`) and the mismatch is hidden rather than absent.
   Nothing in the seek path seeks the item forward by the difference; the
   prepared-successor path does (`successorAttachPositionMs`, `2428`).
   `presentedVideoFrame` (`PlaybackIntent.kt:143-157`) demands
   |position − target| ≤ 250 ms at the first rendered frame, once. On a GOP
   longer than 250 ms that check fails, the 8 s target deadline fires
   `presentation-recovery`, and the second miss is the terminal "couldn't
   reach the requested position" overlay. **Device evidence so far is
   inconclusive**: the review's Google TV run (build 89, a 73 Mb/s remux)
   seeking 0:36 → 11:14 produced a paused transport and, after resume,
   "Playback stopped responding after retrying this stream." — the
   sessionless-second-stall sentence, not the target-deadline one — with no
   `playback_target_timeout` log captured. M6 is therefore gated on
   measuring the achieved origin and first-frame position on a device
   (§8), not on this reading.
2. **Web progress-watch on a stalled frame counter.** `playbackProgressTick`
   (`index.html:11216-11264`) requires the frame count to advance when a
   counter exists; under AirPlay or iOS video-only fullscreen the clock
   advances while the counter does not, which would make `persistentWait`
   fire every 8 s on a playing film. Structurally reachable, unverified in a
   browser; §3.4's "declared, not guessed" rule for remote presentation is
   the contract's answer, and the check belongs on the next AirPlay bench.

---

## 7. Non-goals

- **No new watchdogs, thresholds, budgets or recovery steps in M0–M4.** The
  recovery owner's one new obligation (stop before raising a blocking fault)
  is the only change to what the player does, and at five of the six Apple
  sites it is already true. The three recovery additions from v1 are M5,
  their own PR, with the constraints the review set: an absolute deadline
  and request identity on create retries, one shared hls.js retry budget
  between the fatal-network and "not yet" rows with position preserved, and
  a finite-timeline rule and retry limit for `BEHIND_LIVE_WINDOW` (Media3's
  default-position recovery is a live-edge policy).
- **No wording pass.** Copy moves into the fixture as it is today unless a
  sentence is wrong about the state (e.g. "failed to start" for a mid-film
  network fatal); a copy review is a later, separate change.
- **No gate.** Nothing is switchable.
- **Not the Live TV player.** `#live-tv-overlay` and the Live TV natives have
  their own contract page; they join this fixture when their overlay work
  resumes.
- **Not the Library Channels players, the Android offline player, the
  offline download manager, or the readers.** `LibraryChannels.swift`,
  `OfflinePlayerScreen.kt` and `OfflineDownloadManager.swift` keep their own
  `playbackError`/`failed`/`failure` fields until a later pass. Apple
  offline playback shares `PlayerController` and gets the model for free,
  with `exhausted` unreachable there (no server to keep waiting for) — its
  owner raises `stopped`. The web Library Channels path shares the finite
  player's renderer and is in scope.

---

## 8. Build plan

Six PRs; the [implementation doc](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)
carries the exact interfaces, ownership and acceptance commands. Fast lane
until each is together, opened as a draft, adversarial review, findings, one
full run, then merged — per
[`DEVELOPMENT_PIPELINE.md`](../DEVELOPMENT_PIPELINE.md). M1–M3 are
independent and can run in parallel; each owns its client's files and the
fixture rows tagged for it. M5 and M6 follow and are the only two that
change what the player does.

| # | Scope | Acceptance (summary — the implementation doc has the commands) |
|---|---|---|
| M0 | Fixture + doc rendered from it + `player-contract-table` second fixture + `playback-surface-fence` with a full allow list | reference reducer passes every `cases` sequence; index and fence green |
| M1 | Web adapter + `PlaybackPolicy.presentSurface` + render + owner stops at its two exhausted sites + ledger/log | every `cases` row against the shipped reducer via `shippedSource()`; a mutation that lets `exhausted` render without `player_stopped` fails; injected mid-film fatal with 20 s of buffer → indicator, picture continues, reopen on drain |
| M2 | Apple adapter (`PlurxAPI.check` `.refused` + `.http` kept) + `PlaybackSurfaceModel` + `fail()` stops on the readiness-exhausted path + lock-screen disagreement + PiP + black-frame exhaustion surfaced | `AppleClientTests` runs `cases`; `make apple-test` on `maca`; a fresh VOD transcode whose item never readies walks the ladder, then shows `exhausted` with `rate == 0`; lock-screen play under it → banner + `surface_disagreement` |
| M3 | Android adapter (bodies read) + `PlaybackSurface` `StateFlow` + `playWhenReady = false` at the three exhausted sites + opaque `PlaybackFailed` + ledger | `testDebugUnitTest` runs `cases`; a test pins that each exhausted `onError` site leaves `playWhenReady == false` and that a later `realPosition()` advance on the same epoch (not `onIsPlayingChanged`) yields `surface_disagreement` |
| M4 | Physical verification (Apple TV, iPhone, Android TV): the three recipes below, each with the ledger's `surface_history` captured | recorded in a `PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-<date>.md` beside this page; allowed outcomes are enumerated per recipe in the implementation doc, and a `surface_disagreement` is a pass only if its ledger row names a documented exception |
| M5 | The three recovery additions, one PR, with the §7 constraints — **landed** (`playback/recovery-additions`) | a `cases` row per addition; mutation removing any one retry fails; hls.js retry-once pinned |
| M6 | Android progressive-remux landing, **after** the device measurement in recipe (c) shows achieved origin ≠ requested by > 250 ms | `PlaybackIntentTest` keyframe-origin case; recipe (c) passes |

Reproduction recipes (today's behaviour first, then M4): (a) play a 4K remux,
pull the server's network for 5 s at minute 2 with ≥ 20 s buffered — today:
full-screen error over a moving picture; (b) seek during a stream change on
a cold NAS so the create 503s — today: sticky overlay over the predecessor;
(c) Android, a title Playback debug shows as `Remux` with no session id (the
progressive path), seek forward 10 min, and capture `playback_target_timeout`
plus the achieved `X-Plurx-Media-Origin-Ms` from the client log.

---

## 9. Rulings

Ruled by Paul, 2026-09-13:

1. **The classes and their surfaces (§3.1) — accepted.** A progress fault is
   full-screen only while the attached picture is not presenting; a
   readiness timeout with rungs left is `recovering`, and only a spent
   owner raises the prompt.
2. **The agreement rule (§3.2) — the picture wins.** A blocking surface
   under which the player has started demotes to a banner (keeping its
   actions) and is logged as `surface_disagreement`; nothing re-pauses.
3. **Android keyframe landing (§6.1) — its own PR, M6,** after the device
   measurement.
4. **The three recovery additions — their own PR, M5,** after the surface
   PRs, so M1–M3 are reviewable as behaviour-neutral.
