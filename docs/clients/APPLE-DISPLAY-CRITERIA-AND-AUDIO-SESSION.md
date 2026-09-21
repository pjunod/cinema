# Apple display criteria and audio session — Match Content on tvOS, and a player that knows it was interrupted

**Status:** implementation complete; device evidence pending; draft PR #406
awaits one adversarial review · **Executes:** §2.8 / A1 / F-apple-1 and
§2.10 / A2 / F-apple-2 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [APPLE-CLIENT-PARITY.md](APPLE-CLIENT-PARITY.md) (what the
Apple client lacks, as a list) and
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (who may pause
or stop the player, and what a viewer pause means) — this is *the two
platform integrations the Apple player never had*, in three PRs plus two
device tests.

Read first: §2.8 and §2.10 of the review, the A1/A2 rows (§3.6), and the
assessment's 2.8, 2.10, A1, A2, F-apple-1, F-apple-2 rows in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md),
then the code in §2 in order. Milestone by milestone (§5); each is one
draft PR into `main` under the fast lane, and each shipped Swift change
takes `make apple-build-bump`.

The standing instruction: **if a step seems to require changing
`wantsPlayback`'s ownership (`setPlaybackRequested`), the stall detector's
constants, the recovery ladder, the prepared-commit sequence, or replacing
`AVPlayerLayer` with `AVPlayerViewController`, stop and flag it.** Both
integrations are observers and one property assignment each. Line numbers
are from `88a3957a`; re-verify by function name.

**Execution decisions (2026-09-20).** The current Xcode 27.0 / tvOS 27.0 SDK
exports `AVDisplayCriteria.initWithRefreshRate:formatDescription:` and does
not export the review's proposed dynamic-range initializer. The implementation
therefore follows the plan's asset-owned route and does not construct criteria.
The current work-board protocol supersedes this document's older three-PR
wording: M1–M3 stay in one draft PR, with milestone commits/log rows. No plurx
enablement setting was added because Match Content is the viewer's tvOS setting
and interruption correctness is unconditional.

**Correction to the review:** none for the code facts. Two things the
review states as expected behaviour, not observation, stay that way here:
the SDR/60 HDMI consequence of missing display criteria (§2.8) and the
phone-call → speaker-resume → "Playback stopped." sequence (§2.10). The
device tests in §6 are what turn either into a finding.

---

## 1. Objective

1. **§2.8 / A1.** On tvOS, when the viewer has enabled Match Content
   (frame rate and/or dynamic range) on the Apple TV, the active window's
   `avDisplayManager.preferredDisplayCriteria` is set from the *committed,
   visible* item's asset after each attach, guarded against a stale
   attachment, and reset to `nil` on stop — for the finite player and the
   Live TV player. The library-channel player, which is SwiftUI
   `VideoPlayer`, is assessed on its own (§3.3).
2. **§2.10 / A2.** All three AVPlayer stacks observe
   `AVAudioSession.interruptionNotification` and
   `routeChangeNotification`. A system interruption sets a `systemPaused`
   state that is distinct from a viewer pause and from background/PiP;
   the stall monitor does not run while it is set; playback resumes only on
   `.ended` with `.shouldResume` *and* `wantsPlayback` still true; an
   `.oldDeviceUnavailable` route change becomes a viewer pause
   (`wantsPlayback = false`); the session category is `.playback` with
   mode `.moviePlayback`; the existing iOS-activates / tvOS-does-not split
   is kept.

Done means: three PRs merged, `make apple-test` green, and the two GPT
device tests in §6 recorded (the Apple TV's actual HDMI mode; the iPhone
interruption matrix).

---

## 2. Contract today

Copied from `main` @ `88a3957a`; **re-verify at build time**.

### 2.1 The three player stacks

| Stack | Player | Surface | Audio session today | Failure/state observation today |
|---|---|---|---|---|
| Finite (`PlayerController`) | `let player: AVPlayer` ([`PlayerController.swift:1851`](../../clients/apple/Sources/PlayerController.swift)) | `AVPlayerLayer` in `PlayerSurfaceView` ([`PlayerSurface.swift:310-311`](../../clients/apple/Sources/PlayerSurface.swift)); `view.playerLayer.player = player` at `:270,280`, `= nil` at `:298`; chosen "to avoid reintroducing AVPlayerViewController's LIVE treatment" (`:17-19`) | `#if os(iOS)` `setCategory(.playback)` + `setActive(true)` at `:2610-2611` (open) and `:2681-2682` (offline); `setActive(false, options: .notifyOthersOnDeactivation)` in `stop()` at `:4079`; comment: "Activating it on tvOS was a regression: Apple TV owns the output route and AVPlayer can remain waiting" | `observeStatus` (`status == .failed`), `observeEnd`; periodic observer; 2 s recovery monitor |
| Live TV (`LiveTvController` in `LiveTvView.swift`) | `let player = AVPlayer()` (`:54`) | `PlayerSurface` | `activateAudioSession`/`deactivateAudioSession` closures, `#if os(iOS)` (`:66-79`), owned via `ownsAudioSession` (`:469-479`) | `timeControlStatus` KVO (`:188`); 5 s heartbeat watchdog (`:197+`) |
| Library channels (`LibraryChannelController` in `LibraryChannels.swift`) | `let player = AVPlayer()` (`:318`) | SwiftUI `VideoPlayer(player:)` at `:855` (pane) and `:901` (fullscreen) | **none** — no `AVAudioSession` call in the file | `status` KVO + `AVPlayerItemFailedToPlayToEndTime` (`:638-650`) |

No file under `clients/apple/Sources` contains `avDisplayManager`,
`AVDisplayCriteria`, `preferredDisplayCriteria`, `interruptionNotification`
or `routeChangeNotification` (grep, 2026-09-20).

### 2.2 Where the finite player attaches an item

`player.replaceCurrentItem(with: item)` at four sites, each followed or
preceded by `present(.attach(generation))` / `observeStatus(of:)`:

| Site | Line | What it is |
|---|---|---|
| `open(decision:at:intent:)` | `:4620` | the ordinary and reopen attach; readiness awaited at `:4677-4695` (`itemPreparation.ready(item)`, then `player.play()` if `wantsPlayback`) |
| offline load | `:2759` | local asset |
| node failover reattach | `:7179` | same session, other origin |
| `commitPreparedSuccessor` | `:9377` | the staged successor becomes visible; `sessionId`/`baseMs` move at `:9380-9382`; `startStatusPolling(); player.play()` at `:9406-9407` |

Before the commit, the successor lives in `preparedPlayer: AVPlayer?`
(`:2062`), "muted, no layer, priming" (`:2053`). It is never the surface's
player until `:9377` swaps its *item* into `player`.

### 2.3 The stall monitor and the intent owner

```swift
// PlayerController.swift:5120-5127
let shouldMonitor = self.started
    && self.wantsPlayback
    && !self.finished
    && !self.isPlaybackBlocked
    && !self.isChangingStream
    && self.seekState.allowsStallRecovery
    && self.player.currentItem != nil
```

`PlaybackStallDetector` (`:1317-1330`): `establishedNudgeChecks = 3`,
`establishedReopenChecks = 6`, `unestablishedReopenChecks = 15`, sampled
every 2 s; the nudge is `applyStallRecoveryNudge()` (`:5178`) which calls
`player.play()` through `Self.applyPlaybackCommand`; the reopen is
`retrySameDeliveryAfterStall` (`:5200`).

`wantsPlayback` (`:2314`) is `private(set)`, its `didSet` presents
`.playbackRequested`, and the only writer is `setPlaybackRequested(_:)`
(`:2842`), "the one owner for explicit Play and Pause, including
lock-screen input". Background is observed as `present(.hidden(true))`
(`:6305-6316`); PiP as `pictureInPictureIsActive` (`:2135`).

### 2.4 Toolchain

`clients/apple/project.yml`: `deploymentTarget iOS 17.0 / tvOS 17.0`,
`SWIFT_VERSION 5.9`, `CURRENT_PROJECT_VERSION 173`. CI: `apple_compile` on
the `xcode-26` macOS runner (`main-fast-lane.yml`); tests via `make
apple-test`. Pure-policy tests live in `clients/apple/Tests/AppleClientTests.swift`
and read the shared fixtures under `tests/playback/`.

---

## 3. Change

### 3.1 Display criteria on tvOS (A1)

One controller-side function on the finite player and its twin in Live TV,
`#if os(tvOS)`:

```swift
/// Ask the Apple TV to match the visible item's frame rate / dynamic range,
/// the way AVPlayerViewController would have. Only when the viewer turned
/// Match Content on (that is the switch — plurx adds none), only for the
/// item the surface is showing, and never from a continuation that has
/// been superseded.
private func applyDisplayCriteria(for item: AVPlayerItem, generation: Int) {
    guard !isSuperseded(generation), player.currentItem === item,
          let manager = Self.activeDisplayManager(),
          manager.isDisplayCriteriaMatchingEnabled else { return }
    manager.preferredDisplayCriteria = item.asset.preferredDisplayCriteria
}

nonisolated static func activeDisplayManager() -> AVDisplayManager? {
    UIApplication.shared.connectedScenes
        .compactMap { $0 as? UIWindowScene }
        .flatMap(\.windows)
        .first(where: \.isKeyWindow)?
        .avDisplayManager
}
```

- **Called** at the four attach sites of §2.2, after `replaceCurrentItem`
  and after readiness where readiness is awaited (`:4695`), because
  `preferredDisplayCriteria` on an unloaded asset is `nil` and setting
  `nil` clears a match the previous title had. For the prepared commit it
  is called at `:9377` and **not** when the successor is primed (`:9124`)
  — "a staged prepared successor must not change the display mode before
  it becomes visible" (§2.8 amendment, F-apple-1).
- **Reset** `preferredDisplayCriteria = nil` in `stop()` (beside `:4057`
  `replaceCurrentItem(with: nil)`), in Live TV's `stop()` (`LiveTvView.swift`
  around `:462`), and when the surface releases the layer
  (`PlayerSurface.swift:298`) — the last covers the view going away
  without `stop()`.
- **Stale-attachment guard:** the `generation` is `openGeneration` captured
  when the attach began; `isSuperseded(_:)` (`:4736`) is the existing check. A late
  `open()` continuation cannot re-apply criteria over a newer title.
- **Order with `play()`:** criteria before `player.play()` where possible
  (`:4695` sits before the `play()` at `:4700`), so the mode switch and
  first frame coincide rather than switching mid-picture.
- The explicit `AVDisplayCriteria(refreshRate:videoDynamicRange:)`
  initializer is **not used**. Apple's reference lists it for tvOS 17+;
  the assessor did not find it in the installed SDK. Nothing depends on
  it; 5.1's first step checks on `maca` and records the answer in the PR
  body either way (§5.1).

Verification of the *effect* is a device test (§6): the box's actual HDMI
mode, not the app's badge.

### 3.2 Audio-session interruptions and route changes (A2)

A small owned type, `PlaybackAudioSessionObserver`, created per stack,
that subscribes to both notifications on `AVAudioSession.sharedInstance()`
and calls back on the main actor with a typed event. Its pure policy is
`nonisolated static` so it is unit-testable without AVFoundation:

```swift
enum AudioInterruptionResponse: Equatable { case suspend, resume, stay }

nonisolated static func interruptionResponse(
    type: AVAudioSession.InterruptionType,
    options: AVAudioSession.InterruptionOptions,
    wantsPlayback: Bool
) -> AudioInterruptionResponse {
    switch type {
    case .began: return .suspend
    case .ended: return options.contains(.shouldResume) && wantsPlayback ? .resume : .stay
    @unknown default: return .stay
    }
}

nonisolated static func routeChangeRevokesIntent(
    reason: AVAudioSession.RouteChangeReason
) -> Bool { reason == .oldDeviceUnavailable }
```

Finite player wiring:

- New state `private(set) var systemPaused = false`, presented to the
  surface as its own event (`present(.systemPaused(Bool))`) so the
  overlay can say "Paused by a call" rather than raising a stall notice.
- `.suspend`: `systemPaused = true`; **do not** touch `wantsPlayback`;
  `player.pause()` is what the system already did — call it anyway so
  `isPlaying` and the transport agree; invalidate any in-flight resume
  attempt (`invalidateResumeAttempt(outcome: "interrupted")`, the same
  seam backgrounding uses at `:6314`).
- `.resume`: `systemPaused = false`; issue the play through the existing
  command seam (`Self.applyPlaybackCommand(to: player, preferredRate:
  preferredRate, immediately: …)`), **not** through
  `setPlaybackRequested(true)` — that would flip an intent the viewer
  never changed.
- `.stay`: `systemPaused = false` and nothing else; the viewer's own
  Play press (or the lock-screen command, which already routes through
  `setPlaybackRequested`) resumes.
- Route change `.oldDeviceUnavailable` (headphones pulled, AirPods case
  closed): `setPlaybackRequested(false)` — this **is** a viewer-pause
  semantically, and it is what keeps the film off the speaker. Every
  other reason (`.newDeviceAvailable`, `.categoryChange`, …) is logged and
  ignored.
- `shouldMonitor` (`:5120`) gains `&& !self.systemPaused`. This is the
  one-line fix for the review's narrative: the detector no longer reads a
  stationary clock during a call as a stall to nudge or reopen.
- Category and mode: `setCategory(.playback, mode: .moviePlayback)` at
  the two iOS activation sites (`:2610`, `:2681`). `.moviePlayback` is the
  mode Apple documents for video; it changes AirPlay/route defaults, not
  interruption semantics.
- **The iOS/tvOS split stays:** observers are installed on both
  platforms (tvOS receives interruptions too — Siri, another app taking
  audio), *activation* stays `#if os(iOS)` for the reason recorded at
  `:2606-2609`.
- Teardown: the observer is removed in `stop()` beside the remote-command
  removal (`:4073`), and `systemPaused` is reset to `false` there.

Live TV: same observer; `.suspend` sets a `systemPaused` that the 5 s
watchdog (`LiveTvPlaybackWatchdog`) and the `.waiting` debounce treat as
"not a stall"; `.resume` calls `player.play()` only if `playing` (its
intent flag) is still true; `.oldDeviceUnavailable` → `playing = false;
player.pause()`.

Library channels: add the iOS category set (the file has none, so the
silent switch mutes it today — a finding of this read, one line to fix),
the same observer, `.resume` only if `!paused`, `.oldDeviceUnavailable`
→ `paused = true`.

### 3.3 Library channels use SwiftUI `VideoPlayer` — assessed separately

`VideoPlayer` wraps `AVPlayerViewController`; on tvOS that controller
applies `asset.preferredDisplayCriteria` itself when Match Content is on.
So §3.1 is **not** applied to `LibraryChannelController`: setting the
window's criteria beside a view controller that also sets them is two
owners. The 5.1 PR records, from the GPT test, whether the channel pane
switches the HDMI mode on its own (expected: yes when fullscreen at
`:901`, unknown for the inline pane at `:855`, since an embedded
`VideoPlayer` may not be the "presenting" controller). If the inline pane
does not switch, the fix is to present the channel fullscreen through the
same path, not to add a second criteria writer.

### 3.4 What the surface shows

A `systemPaused` interruption is presented as a `hold`-class notice
("Paused — call in progress" / "Paused — audio interrupted") that is
retired by `.systemPaused(false)`, never by a timer: the contract's
`hold` is timed at 30 s, so this needs the contract fixture to gain a
`retired_by: ["system_resumed"]` entry for one new source,
`system_interruption`. That is a fixture + doc + presenter change in the
same PR and nothing else in the contract moves.

---

## 4. Guardrails (non-goals)

- **Use the asset's criteria through the active window; do not construct
  criteria by hand** (assessment 2.8, F-apple-1: "the proposed
  initializer is not the inspected SDK API"). §3.1 reads
  `item.asset.preferredDisplayCriteria`; the initializer is checked on
  `maca` and not depended on.
- **Bind to the committed visible asset** (§2.8 Astra amendment). Applied
  at the four `replaceCurrentItem` sites on `player`, never on
  `preparedPlayer`.
- **Guard against stale attachment; reset on teardown** (assessment 2.8).
  `!isSuperseded(generation)` + `player.currentItem === item`; `nil` in
  both `stop()`s and on layer release.
- **Do not infer the HDMI state from the API's absence** (assessment 2.8:
  "those require a display-mode observation on a named title/device").
  §6's GPT test is the evidence; the PR body carries it.
- **Do not replace `AVPlayerLayer` with `AVPlayerViewController`.** The
  LIVE-treatment reason at `PlayerSurface.swift:17-19` stands.
- **`VideoPlayer` is assessed separately** (§3.3); no criteria writer is
  added beside it.
- **Keep viewer intent separate from system suspension** (F-apple-2).
  `.suspend` never writes `wantsPlayback`; `.resume` never calls
  `setPlaybackRequested`; only `.oldDeviceUnavailable` writes intent, and
  it writes it *false*.
- **Resume only when the system permits and the viewer still wants it**
  (F-apple-2). `interruptionResponse` returns `.resume` only for
  `.shouldResume && wantsPlayback`.
- **Background and PiP keep their own policy** (F-apple-2). `.hidden` and
  `pictureInPictureIsActive` are untouched; an interruption during PiP
  follows §3.2 like any other.
- **Keep the iOS/tvOS activation split** (§2.10, assessment 2.10).
  Observers on both; `setActive` on iOS only.
- **No detector constant, ladder rung, or reopen budget changes.** The
  only detector edit is `&& !systemPaused` in `shouldMonitor`.
- **No plurx setting.** Match Content is the Apple TV's own switch and
  `isDisplayCriteriaMatchingEnabled` is read every time; an interruption
  policy has no reason to be optional.
- **No metric.** Client-log events `display_criteria_applied`
  (`refresh_rate`, `dynamic_range` as reported by the criteria object's
  description) and `audio_interruption {began|ended|route}` are the trace;
  the Apple client's existing `report` path carries them.

---

## 5. Milestones

Each PR: `make apple-build-bump`, fast Apple compile lane, `make
apple-test`, `WIP:`, review, full suite once, merge.

### 5.1 SDK check, then display criteria on the finite and Live TV players (A1)

Step 0 on `maca`:

```bash
sdk=$(xcrun --sdk appletvos --show-sdk-path)                 # installed tvOS SDK
grep -rn "refreshRate" "$sdk/System/Library/Frameworks/AVFoundation.framework/Headers/AVDisplayCriteria.h" \
  || echo "no explicit initializer in this SDK"             # informational only
```

Then §3.1: `applyDisplayCriteria`, `activeDisplayManager`, the four call
sites, the three resets, the client-log event. Unit test: a
`DisplayCriteriaDecision` pure function (`matchingEnabled`, `itemIsCurrent`,
`openIsCurrent`) → apply/skip, four cases.

**Acceptance:** `make apple-test` green; on the Apple TV in "4K SDR +
Match Content" the §6 GPT prompt reports the TV's HDMI mode changing to
24 Hz (23.976) and/or HDR for a 24p HDR10 title within 2 s of first frame
and back to the home-screen mode after Stop; `grep -c "preferredDisplayCriteria = nil"
clients/apple/Sources/*.swift` ≥ 3.

### 5.2 Audio-session observers on all three stacks (A2)

`PlaybackAudioSessionObserver`, the two pure functions, `systemPaused` on
each stack, `shouldMonitor` gate, `.moviePlayback` mode, Library-channel
category, contract fixture source `system_interruption`, presenter text.
Unit tests: `interruptionResponse` × {began, ended+shouldResume+wants,
ended+shouldResume+!wants, ended+!shouldResume}; `routeChangeRevokesIntent`
× {oldDeviceUnavailable, newDeviceAvailable, categoryChange};
`PlaybackStallDetector.sample(shouldMonitor:false)` already returns `.none`
— add a controller-level test that a `.began` followed by three 2 s
samples produces no nudge (the review's narrative, pinned).

**Acceptance:** `make apple-test` green including the new cases;
`node tests/playback/playback-surface-contract.test.js` green with the new
source; the §6 iPhone prompt reports: no speaker playback after pulling
headphones, resume after a declined call, no "Playback stopped." within a
one-minute call, and one server session in Activity throughout.

### 5.3 Library-channel `VideoPlayer` assessment (A1, separate)

No code unless the GPT test shows the inline pane does not switch; then
the fullscreen-presentation change of §3.3 as its own PR.

**Acceptance:** a dated paragraph in APPLE-CLIENT-PARITY.md recording the
observed HDMI behaviour for the inline pane and the fullscreen surface.

---

## 6. Verification and rollout

Fast lane: `make apple-build` (compile both platforms), `make apple-test`
(simulators). `cargo`: nothing server-side changes except the contract
fixture test, `node tests/playback/playback-surface-contract.test.js`.

What only hardware proves, and the prompts to give a session with device
access (fleet names: the bedroom Apple TV, the iPhone; server `media1`):

```
Apple TV (bedroom). In Settings → Video and Audio set Format to "4K SDR"
and turn ON both "Match Content → Match Dynamic Range" and "Match Frame
Rate". Open the TV's own info/status overlay (the panel that shows the
current input signal, e.g. "3840x2160 60Hz SDR"). Record it. In plurx play
"Harbor Lights" (24p HDR10) from the start. Record the TV's signal readout
at first frame, at 30 s, after a quality change from the player menu
(Auto → 1080p), and 5 s after pressing Stop. Repeat once with "Night Tide"
(25p SDR) and once with a Dolby Vision title. Report every readout with the
title and moment; the plurx playback-info badge is NOT the evidence.
Then open Library channels, play a channel inline and then Fullscreen, and
record the readout in each.
```

```
iPhone. Play "Harbor Lights" with wired or Bluetooth headphones. (1) Have
someone call you; decline after 20 s. Report: did playback pause, did it
resume by itself after the decline, and what did the player overlay say
during the call. (2) Repeat and answer the call for 90 s, then hang up.
Report the same, plus whether the film ended on a "Playback stopped."
screen. (3) While playing, pull the headphones out. Report: did the film
keep playing from the speaker (defect) or pause (expected), and did the
play button show paused. (4) Invoke Siri mid-film and dismiss it. (5)
Start PiP, then trigger a call. For every step also open Settings →
Activity on the web app at http://10.42.0.10:8080 and report how many
sessions the iPhone shows.
```

Rollout: the Apple build goes to TestFlight/the fleet through the normal
[CLIENT-DEPLOY-PROMPT.md](CLIENT-DEPLOY-PROMPT.md) path after
`apple-build-bump`; the contract fixture change deploys with the server.
Rollback is the previous build number.

---

## 7. Open questions

1. **Is `AVDisplayCriteria(refreshRate:videoDynamicRange:)` in the
   installed tvOS SDK on `maca`?** Informational; §5.1 step 0 answers it
   and nothing in the plan uses it either way.
2. **Does an embedded SwiftUI `VideoPlayer` apply display criteria when it
   is not the presenting controller?** Only the §6 Apple TV test answers
   it; §5.3 acts on the answer.
3. **Interruption with `.shouldResume` while the app is backgrounded
   without PiP.** iOS does not deliver video frames to a backgrounded
   `AVPlayerLayer`; today the app does not pause on background (the
   `.hidden` presentation only freezes timers). `.resume` will call
   `play()` in the background and audio continues, as it does today after
   the viewer backgrounds mid-film. Whether that is the desired behaviour
   is a product question outside this plan; the plan keeps today's.
4. **tvOS interruptions.** Which interruptions tvOS actually delivers to
   a `.playback` session that was never activated is not documented in
   the SDK headers. §5.2 installs the observer on tvOS and the Apple TV
   test (Siri invocation) records whether `.began` arrives.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | Commit / PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M1 | `b1709dbc` / #406 | Finite and Live TV apply asset-owned criteria only for the current committed item/open; teardown clears the active window at the finite controller, Live TV controller, and layer release. The four-case pure decision test and iOS/tvOS simulator compilation pass. Physical HDMI-mode evidence remains required. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M2 | `b1709dbc` / #406 | One owned observer serves all three player stacks; interruption state remains separate from viewer intent, stall/watchdog sampling is gated, old-route loss revokes intent, and iOS uses `.playback` / `.moviePlayback`. Six focused Swift tests and all 64 shared surface cases pass. The iPhone interruption matrix remains required. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M3 | `3bbe3ea3` / #406 | No second display writer was added beside SwiftUI `VideoPlayer`. APPLE-CLIENT-PARITY records the implementation and explicitly leaves inline/fullscreen HDMI behavior unobserved; needs the §6 Apple TV prompt before this plan can be `done`. |
