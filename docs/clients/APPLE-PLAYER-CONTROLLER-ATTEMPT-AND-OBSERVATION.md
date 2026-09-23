# Apple PlayerController attempt and observation — nine epochs made explicit, one item observer, polls that keep their deadlines

**Status:** ready for review · **Executes:** A3, A4, A6, A7 and
F-apple-3, F-apple-4, F-apple-6, F-apple-7, F-apple-9, F-apple-10 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to
[APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md](APPLE-DISPLAY-CRITERIA-AND-AUDIO-SESSION.md)
(A1/A2 — land those first; the review's A3 row says "do after A1/A2" and
the observer in §3.2 is where their notifications will live) and
[playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md](../playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md)
(the prepared handoff whose sampling the status poll feeds) — this is *how
`PlayerController.swift` gets smaller without losing a fence*, in six PRs.

Read first: the A3-A7 rows (§3.6 of the review), the assessment's
"Corrections that must reach the implementation plan" item 7, its A3, A4,
A6, A7 rows and F-apple-3/4/6/7/9/10 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md),
then §2 below with the file open. Milestone by milestone; each is one
draft PR under the fast lane with `make apple-build-bump`.

The standing instruction: **if a step seems to require merging two of the
nine counters, removing a deadline, changing a detector constant or a
budget, or backing off the 2 s status cadence, stop and flag it.** Every
milestone here is a behaviour-preserving restructuring with a test that
pins the fence it touches. Line numbers are from `88a3957a`; re-verify by
function name.

**Correction to the review:** A7 says Now Playing is "rewritten at 2 Hz".
The periodic observer's interval is `CMTime(seconds: 1, preferredTimescale:
2)` (`PlayerController.swift:6734`), which is **one second**, and
`updateNowPlaying()` is also called from the seek-presentation monitor and
the end/finish paths. So: 1 Hz steady state plus event calls, and the
assessment's point that a setter is not proof of one XPC per call stands.
The remedy (write on state change, let the system extrapolate elapsed time
from rate) is unchanged.

---

## 1. Objective

1. **A3 / F-apple-3.** An immutable `Attempt` snapshot captured at the
   start of every continuation that crosses an `await`, with a
   **per-scope** `stillCurrent(_:scopes:)` comparison, so each fence names
   which of the nine epochs it depends on. The nine counters stay nine.
2. **A4 / F-apple-4.** One `AVPlayerItemObserver` that owns every KVO and
   notification on an item and emits a typed `AsyncStream<PlayerItemEvent>`
   carrying the item's identity; used by all three stacks; error taken
   from the fatal event first, `item.error` second, the error log last and
   only for the resource that failed.
3. **A6 / F-apple-6.** The four `Task.sleep` polls converted to
   observation-plus-deadline where an observation exists, every deadline
   kept; the 2 s status poll split into recovery evidence and panel
   telemetry **before** any cadence question is asked — and this plan asks
   none.
4. **A7 / F-apple-7 / F-apple-9.** Now Playing written on state change;
   remote commands on tvOS; `Session`'s `origin`/`token` behind its lock;
   `SWIFT_STRICT_CONCURRENCY` staged.
5. **F-apple-10.** Narrow the views' published dependencies before
   claiming any focus/jank win — measured with Instruments, not asserted.

Done means: six PRs merged, `make apple-test` green with the new
late-callback tests, and the controller's continuation fences enumerated
by one test that fails when a new `Task {}` reads an epoch without naming
its scope.

---

## 2. Contract today

Copied from `main` @ `88a3957a`; **re-verify at build time**.

### 2.1 The nine epochs, with their scopes

[`PlayerController.swift`](../../clients/apple/Sources/PlayerController.swift):

| # | Counter | Line | Advanced by | What it invalidates (its scope) |
|---|---|---|---|---|
| 1 | `lifecycleGeneration` | `:2137` | `stop()` (`:3990`) and a new title | "A title owns its decision and all subsequent asynchronous work" — everything |
| 2 | `openGeneration` | `:2294` | every `open()`; `stop()` (`:3998`) | the attach attempt: "an older attempt that wakes… must not replace the item, clear the transition state, or report its own failure over the newer one's (P2-6)" |
| 3 | `viewerActionEpoch` | `:2233` | `beginViewerAction()` on explicit Play/Pause/seek/track change | "recovery work that crossed an await. Session generation alone cannot see pause/resume or native seek" |
| 4 | `initialDecisionGeneration` | `:2031` | `stop()` (`:3993`) and each initial decision | the pre-play decision fetch and its deadline task |
| 5 | `createRetryEpoch` (+ `createRetryExpiredEpoch`) | `:2125-2126` | each create sequence | the create-retry watchdog: "a controller-wide `Bool` let the newer one clear the older one's expiry" |
| 6 | `preparedAlignmentGeneration` | `:2082` | each prepared-commit alignment seek | "stops an abandoned seek — one this commit gave up on — reporting into a later one" |
| 7 | `seekState.generation` | `PlayerSeekState:725`, field `:2215` | each new seek target | one pending seek; `markExecuted(generation:targetMs:)`, `presentedVideo(positionMs:generation:)` |
| 8 | `pgsOverlaySelectionGeneration` | `:2342` | each PGS overlay track selection | overlay prepare tasks for a superseded selection |
| 9 | `pgsOverlayItemGeneration` | `:2343` | each attached item (`:4616`, `:7176`, `:9374`) | overlay windows for a replaced item |

Beside them, not counters: `playbackAttemptId` (UUID per attached item,
telemetry only, `:2326`), `surfaceEpoch` (a clock, `:2100`), Live TV's
`serial` (`LiveTvView.swift:59`) and the channel controller's
`tuneSequence` (`LibraryChannels.swift:322`).

The comparison helpers: `isCurrentLifecycle(_:)` (`:4097`),
`isSuperseded(_:)` (`:4736`, `generation != openGeneration`). Everything
else is a hand-written conjunction at the continuation, e.g. the
seek-presentation fallback at `:6688-6696`:

```swift
guard let self,
      self.openGeneration == recoveryGeneration,
      self.viewerActionEpoch == recoveryActionEpoch,
      self.wantsPlayback, !self.isPlaybackBlocked, !self.finished,
      !(self.seekPresentationBackgrounded && hasVideo),
      self.seekState.pendingMs == targetMs,
      self.seekState.generation == generation
else { return }
```

The review counted 58 such conjunctions and 39 unstructured `Task {}`.

### 2.2 Item observation today

| Stack | Observes | Does not observe |
|---|---|---|
| Finite: `observeStatus(of:)` `:6967-6974` (`status == .failed` → `handleItemFailure`), `observeEnd(of:)` `:6908` (`AVPlayerItemDidPlayToEndTime`), periodic observer `:6733`, `timeControlStatus` read by the 2 s monitor | `AVPlayerItemFailedToPlayToEndTime`, `AVPlayerItemNewErrorLogEntry`, `AVPlayerItemPlaybackStalled`, `timeControlStatus` KVO |
| `handleItemFailure(_:)` `:6987`: `let event = item.errorLog()?.events.last` and classifies with `isCompatibilityPlaybackFailure(error:eventDomain:eventStatus:eventComment:)` (`:7613`) and `isTransportPlaybackFailure` (`:7561`) | — the last log entry is not matched to the failed resource |
| Live TV: `timeControlStatus` KVO (`LiveTvView.swift:188`), 5 s watchdog | item `status`, failed-to-end |
| Library channels: `status` KVO + `AVPlayerItemFailedToPlayToEndTime` (`LibraryChannels.swift:638-650`), also reads `errorLog()?.events.last` | `timeControlStatus`, new-error-log |

### 2.3 The polls

| Poll | Where | Cadence | Deadline it carries | What it feeds |
|---|---|---|---|---|
| `awaitItemReady(_:)` | `:7990-8002` | 100 ms | `itemReadinessDeadlineSeconds = 15` (`:1763`); comment: "The old unbounded `for await` consequently held the initial `play()` forever" | `open()` readiness |
| `beginSeekPresentationMonitor` | `:6630-6716` | 50 ms | `seekPresentationDeadlineSeconds = PlayerSeekState.presentationDeadlineSeconds` = 8 (`:737`) | seek settled evidence via `AVPlayerItemVideoOutput.copyPixelBuffer`, then the deadline reopen |
| `beginPreparedReadinessMonitor` | `:9180-9215` | 100 ms | `preparedReplacement.readinessBoundElapsed()` | prepared successor `.readyToPlay`, the owed seek |
| `startStatusPolling` | `:5014-5047` | 2 s | none (runs while `started`) | `sessionStatus` (panel), `sampleThePreparedSwitch()` (M3), `observeDeliveryStarvation(status)` (wedge recovery) — the comment: "A fired wedge recovery replaces the session… `observeDeliveryStarvation` spawns an unstructured task" |
| `PlaybackControlSession` ask loops | [`PlaybackControlSession.swift:445, 542`](../../clients/apple/Sources/PlaybackControlSession.swift) | `askPollNanoseconds = 25_000_000` | `deadline`/`hardDeadline` in the loop; owner-change and reporter-stopped exits | the control ask |

### 2.4 Now Playing, remote commands, `Session`, toolchain

- `updateNowPlaying()` `:8832-8846` rebuilds the whole dictionary; called
  from the periodic observer (`:6822`), seek settle (`:6678`), finish;
  `#else private func updateNowPlaying() {}` — nothing on tvOS.
  `installRemoteCommands()` `:8770` is under `#if os(iOS)` (`:2612`, `:2683`).
- [`Session.swift:8-13`](../../clients/apple/Sources/Session.swift):
  `final class Session: @unchecked Sendable { var origin: String = ""; var
  token: String?; private let nodeLock = NSLock() … }` — the lock guards
  only `mediaFailoverOrigins`/`mediaFailoverIndex`. `Session.shared.origin`
  / `.token` are read at 29 sites outside the file, including
  `AuthImage.swift:62, 222, 253` from non-isolated image-loading code.
- [`project.yml:14-18`](../../clients/apple/project.yml): `SWIFT_VERSION:
  "5.9"`, `CURRENT_PROJECT_VERSION: "173"`; no `SWIFT_STRICT_CONCURRENCY`.

---

## 3. Change

### 3.1 `Attempt` — a snapshot with named scopes (A3)

```swift
/// What a continuation captured before it awaited. Compared per scope: a
/// viewer action must not cancel an unrelated prepared replacement, and a
/// prepared alignment must not survive a new open. Nine counters, nine
/// scopes; nothing is merged.
struct Attempt: Equatable, Sendable {
    enum Scope: CaseIterable { case lifecycle, open, viewerAction, initialDecision,
        createRetry, preparedAlignment, seek, pgsSelection, pgsItem }
    let lifecycle: Int, open: Int, viewerAction: Int, initialDecision: Int
    let createRetry: Int, preparedAlignment: Int, seek: Int
    let pgsSelection: Int, pgsItem: Int
    let item: ObjectIdentifier?     // the AVPlayerItem attached at capture

    func stillCurrent(_ now: Attempt, scopes: Set<Scope>) -> Bool {
        for scope in scopes where value(scope) != now.value(scope) { return false }
        return true
    }
}
```

`snapshotAttempt()` on the controller reads the nine fields and
`ObjectIdentifier(player.currentItem)`. Every `guard` conjunction of the
form `self.openGeneration == g && self.viewerActionEpoch == e …` becomes
`guard attempt.stillCurrent(self.snapshotAttempt(), scopes: [.open,
.viewerAction]) …` with the *same* set of fields it compared before —
the migration is mechanical, one conjunction at a time, and the scope set
is copied from the conjunction, never widened. The non-epoch predicates
(`wantsPlayback`, `!isPlaybackBlocked`, `seekState.pendingMs == targetMs`)
stay beside it as they are.

Why not one counter (F-apple-3): the table in §2.1 shows nine different
invalidation scopes. A pause (`viewerAction`) during a prepared alignment
(`preparedAlignment`) is exactly the case where the two must be compared
independently — the commit path decides for itself at `:3855` ("A paused
viewer's change cannot be committed") rather than being cancelled by a
generic epoch bump.

Test: `AttemptScopesTest` builds two snapshots differing in one field at
a time and asserts `stillCurrent` is false only for scope sets containing
that field (81 cases, generated). A second test, `ContinuationScopeCensus`,
greps `PlayerController.swift` for `stillCurrent(` and for the legacy
`Generation ==`/`Epoch ==` comparisons and fails if a legacy comparison
remains outside an allow-list that shrinks per milestone — the census is
how "58 conjunctions" becomes zero without a big-bang edit.

### 3.2 `AVPlayerItemObserver` — one typed stream (A4)

```swift
enum PlayerItemEvent: Sendable {
    case status(AVPlayerItem.Status)
    case timeControl(AVPlayer.TimeControlStatus, AVPlayer.WaitingReason?)
    case playedToEnd
    case failedToPlayToEnd(NSError?)        // the fatal event's own error
    case newErrorLogEntry                   // a pointer, never an error
    case playbackStalled                    // evidence, never terminal
    case interruption(AudioInterruptionResponse)   // from A2's observer
    case routeLost                          // .oldDeviceUnavailable
}

/// Owns every KVO token and NotificationCenter observer for exactly one
/// item and its player. `events` finishes when `cancel()` runs or the
/// item is replaced; a finished stream is how a late callback learns it
/// is late — there is no per-callback identity check to forget.
@MainActor final class AVPlayerItemObserver {
    let item: AVPlayerItem
    let events: AsyncStream<PlayerItemEvent>
    init(item: AVPlayerItem, player: AVPlayer, audio: PlaybackAudioSessionObserver?)
    func cancel()   // removes observers, finishes the stream — idempotent
}
```

- The controller keeps **one** `itemObserver` and replaces it at the four
  `replaceCurrentItem` sites (`:2759`, `:4620`, `:7179`, `:9377`);
  the old one's `cancel()` runs before the new item is attached, so no
  event about the predecessor can arrive after the successor's first.
  The prepared successor gets its own observer while priming
  (`beginPreparedReadinessMonitor` becomes a `for await` on it).
- Consumers are `for await event in observer.events` loops that are
  per-stack policy: finite keeps `handleItemFailure`'s ladder, Live TV
  keeps its watchdog, channels keep theirs (assessment A4: "preserve the
  separate finite/live/channel policies").
- **Error precedence** (F-apple-4): `.failedToPlayToEnd(error)` → that
  error; else `item.error`; else the error log, and then only the entry
  whose `uri` matches the resource the failure named (the
  `AVPlayerItemErrorLogEvent.uri` against the item's asset URL or the
  segment URL in `item.error`'s `NSURLErrorFailingURLStringErrorKey`).
  A `.newErrorLogEntry` alone changes nothing but a diagnostic counter;
  `.playbackStalled` feeds the existing stall observation and never
  advances the ladder.
- `isCompatibilityPlaybackFailure` and `isTransportPlaybackFailure` keep
  their signatures; the new `PlayerItemFailure.classify(fatal:item:log:
  failedURI:)` pure function is what picks the arguments, and it is the
  unit under test.

### 3.3 Polls → observation with deadlines (A6)

Each conversion keeps the deadline as a first-class `Task.sleep` race,
installs the observation **before** reading state, and resumes its waiter
exactly once (F-apple-6):

| Poll | Becomes | Deadline kept as |
|---|---|---|
| `awaitItemReady` | `for await case .status(let s) in observer.events` after an initial `if item.status != .unknown` read *after* subscription | `withThrowingTaskGroup`: the stream task vs `Task.sleep(.seconds(15))` → `PlaybackPreparationError.timedOut`; first to finish wins, the other is cancelled |
| `beginPreparedReadinessMonitor` | `for await .status` on the successor's observer | `readinessBoundElapsed()` checked in the same group's timer branch |
| `beginSeekPresentationMonitor` | **stays a poll**. `AVPlayerItemOutputPullDelegate.outputMediaDataWillChange` is a wake-up, "not proof the sought frame has been presented" (F-apple-6); the plan uses it to *shorten* the sleep to the next frame, then still calls `hasNewPixelBuffer`/`copyPixelBuffer`. The 8 s deadline reopen is untouched |
| `PlaybackControlSession` asks | an `AsyncStream` of answer arrivals replaces the 25 ms sleep; `deadline`, `hardDeadline`, the one-time extension, owner-change and reporter-stopped exits stay as written | unchanged constants |

**The status poll is split, not slowed.** `startStatusPolling` becomes
`startRecoveryEvidencePoll` — same 2 s, same `openGeneration`/`sessionId`
fence, same call to `sampleThePreparedSwitch()` and
`observeDeliveryStarvation(status)` — and the panel's `sessionStatus` /
`diagnosticSessionStatus` are set from the **same response** by a
separate, named `publishPanelTelemetry(status)` so the two concerns are
two functions with two tests. No second request, no backoff, no
dependency on panel visibility (assessment correction 7). A future cadence
change would edit `publishPanelTelemetry`'s consumer, never the evidence
poll; this plan does not make it.

### 3.4 Now Playing, remote commands, `Session`, concurrency (A7, F-apple-9)

- `updateNowPlaying()` is called on: item attach, `wantsPlayback` change,
  rate change (`preferredRate` set), seek executed, finish, stop. Removed
  from the periodic observer. `MPNowPlayingInfoPropertyElapsedPlaybackTime`
  + `PlaybackRate` let the system extrapolate; the dictionary is rebuilt
  only when a field changed (a cached last value, compared). Cleared on
  teardown (already at `:4076`).
- tvOS: `installRemoteCommands()` and `updateNowPlaying()` leave
  `#if os(iOS)`; play/pause/togglePlayPause/skip/seek targets route to
  `setPlaybackRequested`/`seek(toMs:)` exactly as the iOS ones do. Live TV
  and channels register their own when active and remove them on stop —
  "coordinate remote ownership across all player stacks" (F-apple-7):
  a `RemoteCommandOwner` token, one at a time, released in each `stop()`.
- `Session`: `origin` and `token` become `private var` behind `nodeLock`
  with `var credentials: (origin: String, token: String?)` read under the
  lock and `func setCredentials(origin:token:)` as the one writer. The 29
  call sites read `credentials.origin`. **A lock, not an actor**
  (F-apple-9): the readers include non-isolated image code that needs a
  synchronous answer, and "an actor cannot safely expose a synchronous
  nonisolated read of mutable state merely by naming it snapshot()".
- `SWIFT_STRICT_CONCURRENCY` staged in `project.yml`: PR 5.6 sets
  `targeted`, records the warning count per file in the PR body; a
  follow-up sets `complete` once the count under `targeted` is zero;
  Swift 6 language mode is not this plan's. No warning is silenced with
  `@preconcurrency` or `nonisolated(unsafe)` without a comment naming the
  invariant.

### 3.5 View dependencies (F-apple-10)

Before any Observation-framework adoption: an Instruments SwiftUI trace on
the Apple TV during playback with the transport visible, recording body
evaluations per second per view. Then narrow `PlayerView`'s reads so the
1 Hz `currentMs` publish reaches only the time label and scrubber. Only if
the trace shows the focus tree rebuilding per tick is anything else done,
and that is a separate PR with the before/after trace attached.

---

## 4. Guardrails (non-goals)

- **Nine counters stay nine** (A3, F-apple-3). `Attempt` carries all nine;
  `stillCurrent` compares the named subset; no continuation's scope set is
  widened during migration.
- **No forced co-cancellation of unrelated work** (F-apple-3). A
  `viewerAction` bump never appears in a prepared-alignment fence unless
  today's conjunction already reads it.
- **Separate finite / live / channel policies** (assessment A4). The
  observer emits; each stack decides.
- **Error from the fatal event first; the log entry must match the
  resource** (F-apple-4). `classify` takes `failedURI` and ignores
  unmatched entries.
- **`PlaybackStalled` is not terminal and does not duplicate recovery**
  (F-apple-4). It feeds `stallObservation` only.
- **Every deadline survives its conversion** (assessment correction 7,
  F-apple-6): 15 s readiness, 8 s seek presentation, the prepared bound,
  the ask deadlines. §5's tests assert each fires.
- **Install observation before reading state; resume once** (F-apple-6).
  The subscription precedes the initial status read in every converted
  wait.
- **The 2 s evidence cadence is not backed off and not keyed on panel
  visibility** (A6, assessment correction 7). This plan splits functions;
  it changes no interval.
- **`Session` gets a lock, not a sync actor snapshot** (F-apple-9).
- **Strict concurrency is staged with real diagnostics** (F-apple-9): a
  count in the PR body, then `complete`; never "assume the warnings are
  benign".
- **No performance claim without a trace** (F-apple-10).
- **A1/A2 land first.** Their observers are consumed here; building the
  item observer before them would move the same code twice.
- **No settings key, no metric, no server change.** Client-log events:
  `attempt_stale {scope}` when a continuation is refused (bounded by the
  nine scope names), `item_event_ignored {kind}` for late events.

---

## 5. Milestones

Each PR: `make apple-build-bump`, fast Apple compile, `make apple-test`,
`WIP:`, review, full suite once, merge.

### 5.1 `Attempt` and the census, migrating the seek and stall fences

`Attempt`, `snapshotAttempt()`, `AttemptScopesTest` (81 cases),
`ContinuationScopeCensus` with the initial allow-list = every legacy
conjunction; migrate the seek-presentation fallback (`:6688`), the black
frame handler (`:6790`), the stall reopen guards, removing them from the
allow-list.

**Acceptance:** `make apple-test` green; the census test's allow-list is
at least 12 entries shorter than at PR open; a new test
`lateSeekContinuationIsRefusedAfterViewerPause` (bump `viewerActionEpoch`
between capture and check → refused) and
`preparedAlignmentSurvivesViewerPause` (bump `viewerActionEpoch`, scopes
`[.lifecycle, .preparedAlignment]` → still current) both pass.

### 5.2 `AVPlayerItemObserver` on the finite player

The observer, `PlayerItemFailure.classify`, the four attach sites, the
`for await` consumer holding today's `handleItemFailure` ladder,
`observeStatus`/`observeEnd` deleted. Tests: `classify` with (fatal
error, no log) → fatal; (no fatal, item error, log entry for another URI)
→ item error; (no fatal, no item error, matching log entry) → log; (only
an unmatched benign subtitle entry) → nil; `playbackStalled` → no ladder
step; observer `cancel()` twice → one finish.

**Acceptance:** `make apple-test` green; `grep -c "errorLog()?.events.last"
clients/apple/Sources/PlayerController.swift` prints `0`.

### 5.3 The observer on Live TV and library channels

Same type, each stack's own consumer; `LibraryChannels.swift:638-650` and
`LiveTvView.swift:188` replaced. Tests mirror 5.2 per stack.

**Acceptance:** `make apple-test` green; `grep -c "errorLog()?.events.last"
clients/apple/Sources/*.swift` prints `0`.

### 5.4 Polls converted; status poll split

§3.3. Tests: readiness never `.readyToPlay` → `timedOut` at 15 s (clock
injected); readiness arrives at 200 ms → returns at 200 ms; prepared bound
elapses → `.abandon(.failed)`; ask loop deadline and one-time extension
under the stream; `publishPanelTelemetry` and `startRecoveryEvidencePoll`
each with one test that the other's absence does not change its behaviour.

**Acceptance:** `make apple-test` green; `grep -c "Task.sleep(for:
.milliseconds(100))" clients/apple/Sources/PlayerController.swift` prints
`0`; the 50 ms seek poll remains (`grep -c "milliseconds(50)"` = 1) and
its deadline test still fires at 8 s.

### 5.5 Now Playing on state change; tvOS remote commands; `Session` lock

§3.4 first three bullets. Tests: `Session` credentials read under
concurrent write from two threads never observes a torn pair (origin from
one write, token from another); Now Playing dictionary rebuilt only when
a field changed (a counting stub on the info-center writer).

**Acceptance:** `make apple-test` green; `grep -c "updateNowPlaying()"
clients/apple/Sources/PlayerController.swift` is lower than today's and
none of the remaining calls is inside `makePeriodicPlaybackObservation`;
on the Apple TV the Siri Remote play/pause button toggles playback in
plurx from the TV app's Now Playing (GPT prompt, §6).

### 5.6 `SWIFT_STRICT_CONCURRENCY: targeted`, count recorded

`project.yml` `settings.base` gains the key; the PR body lists warnings
per file; the ones inside code this plan touched are fixed in the same PR.

**Acceptance:** `make apple-build` green with the key set; the PR body
carries the per-file count; no `nonisolated(unsafe)` added without a
comment.

---

## 6. Verification and rollout

Fast lane: `make apple-build`, `make apple-test`. Nothing server-side.
The shared fixtures under `tests/playback/` are unchanged by this plan.

The census test (5.1) is the guard that keeps the decomposition honest
across all six PRs: a new unstructured `Task {}` that compares an epoch by
hand fails it until the comparison names its scopes.

GPT prompts (what only hardware shows):

```
Apple TV. Play "Harbor Lights" in plurx, press the TV button to go Home,
open the TV app's Now Playing (or the Control Center audio card) and press
play/pause there twice. Report whether plurx paused and resumed, and
whether the elapsed time shown there advances while playing without
jumping. Then, from plurx, pause and unpause with the Siri Remote and
report whether the Now Playing card reflects each change within a second.
```

```
iPhone. Play "Night Tide" with the lock screen on. Press play/pause on the
lock screen twice, scrub the lock-screen slider forward one minute, then
unlock. Report whether the film position matches the slider and whether
the lock-screen state ever disagreed with the app for more than a second.
```

Rollout: TestFlight/fleet via [CLIENT-DEPLOY-PROMPT.md](CLIENT-DEPLOY-PROMPT.md)
after each `apple-build-bump`; rollback is the previous build number.

---

## 7. Open questions

1. **Does `AVPlayerItemErrorLogEvent.uri` reliably carry the segment URL
   for a failed fragment on tvOS 17?** If it is `nil` for the failing
   resource, `classify` falls back to `item.error` only and the log never
   decides — the conservative outcome. 5.2 records what the log carries
   for one injected 404.
2. **Scope names vs. field names.** `Attempt` uses the nine field names;
   if A1/A2 add `systemPaused` as an epoch-like state the census must not
   treat it as a tenth counter (it is a flag, compared as a predicate).
   Decide at 5.1.
3. **`AsyncStream` back-pressure.** `PlayerItemEvent` streams are
   unbounded by default; the item emits at most a few events per second.
   A `.bufferingNewest(16)` policy is proposed so a suspended consumer
   cannot accumulate stale `timeControl` events; confirm no consumer needs
   every `timeControl` transition (the stall observation samples, it does
   not count).
4. **tvOS Now Playing without an active audio session.** Whether
   `MPNowPlayingInfoCenter` on tvOS shows the app without `setActive(true)`
   (which A2 deliberately keeps off on tvOS) is answered by the §6 Apple TV
   prompt; if not, the answer is to register `MPRemoteCommandCenter`
   targets only, not to activate the session.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | 5.1, partial | [PR #464](http://192.168.4.7:3000/noirr/plurx/pulls/464) | `Attempt`, `Attempt.Scope` and `snapshotAttempt()` landed, and four multi-counter continuation fences were migrated to `stillCurrent(_:scopes:)`: the seek-presentation deadline reopen (`[.open, .viewerAction, .seek]`), the black-frame decode-failure handler (`[.lifecycle, .viewerAction]`), the same-delivery stall recovery after the control ask (`[.open, .viewerAction]`) and the item-failure ladder after its control ask (`[.open, .viewerAction]`). **The executing session had no Swift toolchain and no Xcode: none of the Swift was compiled and `make apple-test` was not run.** `AttemptScopesTests.swift` — the plan's 81 generated cases plus `testLateSeekContinuationIsRefusedAfterViewerPause` and `testPreparedAlignmentSurvivesViewerPause` — is written but unexecuted, so it is not evidence yet. What *is* evidence is `ContinuationScopeCensus`, which this session implemented as `validation/attempt_census.py` and `tests/operations/test_attempt_scope_census.py` so that it runs without a Swift toolchain: it counts every hand-written epoch comparison in `PlayerController.swift`, holds them against `validation/attempt-census.toml`, and fails on a difference in either direction. Reverting `PlayerController.swift` to `origin/main` under the new allow-list fails 8 of its 9 cases and names all eight restored conjunctions. **Acceptance not met:** §5.1 asks for the allow-list to shrink by at least 12 entries; it shrank by 8 comparisons across 4 fences, and the remaining 66 in 44 rows are inventoried rather than migrated. Apple build 180 is a generated version claim from `validation.apple_build`, not a built or tested binary. |
