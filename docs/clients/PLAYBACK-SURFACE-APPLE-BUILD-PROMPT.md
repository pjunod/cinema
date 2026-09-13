# Apple build and test — compile the playback surface contract

**Status:** open · **For:** a session sitting at a Mac with Xcode ·
**Covers:** M2 (PR #280) and M5 (PR #282) of the [playback surface
contract](PLAYBACK-SURFACE-CONTRACT.md), both merged to `main`

Every line of Swift in those two PRs was written on a Linux machine with no
Xcode, no `swift` and no simulator. **Nothing has been compiled and no XCTest
case has ever executed.** That is not a caveat on the work; it is the work
that is left. This document is the hand-off for someone at the machine that
can do it.

Nothing here changes behaviour on purpose. If a step fails, the two honest
outcomes are (1) fix it there, when the failure is a compile error whose fix
is obvious and local — a wrong argument label, a missing `await`, an
exhaustiveness complaint — or (2) stop and report the compiler output
verbatim, when the failure is about what the code *means*. Do not delete a
test, loosen an assertion, or add a fixture skip to get green. A failing
assertion here is the first real evidence anyone has about this code.

## 0. What you need

- A Mac with Xcode installed and `xcodegen` on `PATH`
  (`brew install xcodegen`). No Apple Developer team and no signing identity
  is needed: every command below passes `CODE_SIGNING_ALLOWED=NO` and builds
  for simulators.
- A checkout of `plurx` at `main` (or later). Confirm the two PRs are in:

  ```bash
  cd ~/code/plurx && git fetch origin && git checkout main && git pull
  git log --oneline --grep='(M2)' --grep='(M5)' -i -5
  ls clients/apple/Sources/PlaybackSurfaceModel.swift   # M2's presenter
  ```

- Simulator runtimes for **both** iOS and tvOS. The default destinations are
  `iPhone 17 Pro` and `Apple TV 4K (3rd generation)`; override with
  `APPLE_IOS_SIM` / `APPLE_TVOS_SIM` if your Xcode has different names.

## 1. Generate the project

```bash
cd clients/apple && xcodegen generate
```

`project.yml` gained `tests/playback/playback-surface-contract.json` as a test
resource in **both** targets (iOS and tvOS). If `xcodegen` errors on that
entry, the path in `project.yml` is what to check — the fixture lives at the
repository root, not under `clients/apple/`.

## 2. Compile — this is the real unknown

```bash
cd ~/code/plurx && make apple-build
```

`apple-build` compiles **both** schemes (`plurx-iOS` and `plurx-tvOS`) for
generic simulator destinations without running anything. Build both: `PlayerView`
has `#if os(tvOS)` branches around `isPlaybackBlocked`, `PlayerControl.signIn`
and the new focus target, so an iOS-only green build proves less than half.

**Highest-risk constructs, in the order worth checking if it fails.** These
are the places the authors could not check, ordered most-likely-to-fail first.

From M2 (`PlaybackSurfaceModel.swift`, `PlayerController.swift`,
`PlayerView.swift`):

1. `ContinuousClock.Instant.duration(to:)` / `advanced(by:)` and `Duration`
   comparisons in `PlaybackSurfaceModel`.
2. `Result` plus `guard case .success`.
3. `fileprivate(set)` mutation of `PlaybackFault` from `PlaybackSurfaceModel`
   — `cls` and `raisedAt` are `var`, not `let`, because the agreement rule and
   the hidden-interval carry both change a fault in place.
4. The memberwise inits of `PlaybackSurfaceHistory.Entry` and
   `ApplePlaybackSurfaceLog`.
5. `nonisolated static` adapter funcs on `PlayerController`.
6. The tuple-destructuring `.map { name, hidden in }` over the notification
   list.
7. `#if os(tvOS)` inside the `@ViewBuilder` switch in `failureActionButton`.
8. Switch expressions with implicit returns in `rendersFailureAction`.

From M5 (`PlayerController.swift`, `PlaybackCreateRetry`):

9. The `Task { [weak self] in … }` watchdog and its
   `defer { watchdog.cancel(); if createRetryEpoch == epoch { … } }`
   (`createRetryingNotYet`, around `PlayerController.swift:5175`).
10. `catch let spent as PlaybackCreateRetryError` placed before the catch-all.
11. `Self.milliseconds(began.duration(to: ContinuousClock.now))`.
12. The two new constructor parameters — `waitCreateRetry` and
    `releaseHlsSession` — and **every existing `PlayerController(...)` call
    site in the test target**. They have defaults, so production call sites
    compile; a test that constructs one positionally will not.
13. Argument order on `raiseSurfaceNotice(source:context:title:detail:)` and
    `raiseOwnerFault(source:context:title:detail:actions:)`.
14. Exhaustiveness of `switch PlaybackCreateRetry.Step`.

## 3. Run the whole suite, on both destinations

```bash
make apple-test
```

That target generates the project, compiles iOS once with `build-for-testing`,
replays it with `test-without-building`, and then does the same for tvOS. Set
`APPLE_IPAD_SIM` if you also want the iPad destination.

**Run the whole suite, not just the new tests.** Both of M2's review blockers
were "the target does not build" or "an untouched file fails": a test still
read the deleted `PlaybackStallTerminalState.failed`, and `LiveTvTests`'
status loop asserted `.http` for `[400, 401, 403, 404, 503]` when three of
those now carry a refusal code. A green `AppleClientTests` alone would have
hidden both.

### What must be true when it passes

- **`testPlaybackSurfaceModelRunsEveryContractCase` passes on both the iOS
  and the tvOS destination.** It loads
  `tests/playback/playback-surface-contract.json` from the target's own
  resources and iterates every case in it; the fixture holds **60** today (it
  was 57 before M5 added `m5_create_retry_deadline`, `m5_hls_retry_once` and
  `m5_behind_live_window_finite`). The test asserts the fixture is non-empty
  — *"the fixture is the contract; an empty one proves nothing"* — so a
  missing resource fails loudly rather than passing on zero cases. What it
  cannot tell you is whether the resource is in **both** targets: check
  `project.yml` and confirm the test ran, and passed, under each destination.
- `testTheSwiftSurfaceTablesAreTheFixtureVerbatim` — the Swift class, source
  and timing tables equal the fixture, in order.
- `testABlockingSurfaceIsNeverDrawnOverAPlayerNobodyStopped`.
- `testEveryOwnerStopSiteStopsThePlayerBeforeItRaises` — one killer per site,
  eight sites.
- `testTheBlackFrameLadderStopsThePlayerBeforeItRaises`.
- `testAFailureOverAPictureThatIsNotPresentingIsBlockingWhateverIsAttached`
  (M2 review blocker B4).
- `testAPresentingSampleAfterAStopDemotesToABannerAndLogsTheDisagreement`
  and `testTheLockScreenRoutesThroughTheContract` — the documented
  `surface_disagreement` exception.
- M5's three in `PlayerOperationOwnershipTests`:
  `testACreateThatLandsAfterTheDeadlineIsReleasedAndNeverAttached`,
  `testTheCreateLadderReplaysOneIdentityAndThenRaisesExhausted`,
  `testAChangeContextCreateIsNotRetriedAndArmsNoDeadline`.
- M5's ladder arithmetic in `AppleClientTests`:
  `testCreateRetryLadderIsOneSecondTwoSecondsFourSecondsAndThenSpent` and
  `testCreateRetryDeadlineIsAbsoluteAndNeverSchedulesPastItself`.

## 4. Mutate and confirm — the tests have never killed anything

A test that has never run has never killed a mutation. For each mutation
below: apply it, run `make apple-test`, confirm **the named test fails**, then
`git checkout` the file. If a *different* test fails instead, that is fine —
report which one. What is not fine is the suite staying green: a mutation
nothing catches is a finding, and it is reported with the mutation and the
file rather than quietly fixed.

| # | Mutation | Must fail |
|---|---|---|
| A1 | Delete the `stopForBlockingSurface()` call from `handleBlackFrameDecodeFailure`'s ladder-spent branch (`PlayerController.swift:6943`) — that helper is the `player.pause()` §4.3 names (`PlayerController.swift:5151`) | `testTheBlackFrameLadderStopsThePlayerBeforeItRaises` |
| A2 | Make `PlaybackSurface.entersFailedRouting` (`PlaybackSurfaceModel.swift:132`) return `kind == .blocking` | `testPlaybackSurfaceModelRunsEveryContractCase` — the fixture carries `input_failed: false` on full-screen progress classes, which is exactly what this mutation gets wrong |
| A3 | Change `sampleSurfacePresentation` (`PlayerController.swift:5589`) to decide on `player.timeControlStatus == .playing` instead of the position delta | `testAPresentingSampleAfterAStopDemotesToABannerAndLogsTheDisagreement` |
| A4 | Delete the `await releaseHlsSession(model, id)` call on the late-create path (`PlayerController.swift:4053`) | `testACreateThatLandsAfterTheDeadlineIsReleasedAndNeverAttached` |
| A5 | Drop the third rung: `PlaybackCreateRetry.backoffMs = [1_000, 2_000]` (`PlayerController.swift:1538`) | `testCreateRetryLadderIsOneSecondTwoSecondsFourSecondsAndThenSpent`, and `node tests/playback/web-policy.test.js` on any machine |
| A6 | Make the `defer` in `createRetryingNotYet` retire the epoch unconditionally — drop the `if createRetryEpoch == epoch` guard (`PlayerController.swift:5214`) | `node tests/playback/web-policy.test.js` — *an abandoned Apple sequence must retire ONLY its own epoch*. **If no Swift test fails under this mutation, that is the finding**: B2's behavioural proof does not exist and wants writing. The field route is §6.6. |

A1–A3 are M2's three §4.3 mutation-killers. A4–A6 are M5's Apple properties
whose only current proof is a source-shape assertion in
`tests/playback/web-policy.test.js` (rows M17, M9 and M16 of that PR's
mutation table) — the behavioural proof is the Swift, and the Swift has never
run.

## 5. The fence bypasses — already verified, do not redo

`scripts/playback-surface-fence` is Python and runs on any machine. All seven
M2 bypass spellings were pasted into the real `PlayerController.swift`
**outside** the publish anchors and each one tripped the fence
(`docs/playback-surface-handoff`, 2026-09-13):

```
surface = PlaybackSurfaceModel()
self.surface.apply(.raise(fault, context: .attached), now: .now)
self[keyPath: \.surface] = PlaybackSurfaceModel()
_surface = Published(initialValue: PlaybackSurfaceModel())
setValue(nil, forKey: "surfaceHistory")
surfaceHistory = PlaybackSurfaceHistory()
surfaceHistory.record(entry, atMs: 0, player: snapshot)
```

Each also has a must-trip fixture line in
`tests/operations/fence-fixtures/surface/must_trip.swift`, checked by
`tests/operations/test_playback_surface_fence.py`. Nothing about them needs a
Mac. They are named here so nobody re-runs them believing they are owed.

## 6. Simulator and device runs

Run these after the suite is green. Each one is a behaviour the unit tests
cannot see.

1. **§4.3's run.** A fresh VOD transcode whose item never readies: the
   black-frame ladder walks, then `exhausted` is shown with `player.rate == 0`.
   Then press play on the **lock screen** underneath it — expect the "Playback
   recovered" banner with Try Again and a `surface_disagreement` row in the
   ledger.
2. **R1.** Force a 401 on a session create during a quality change. The picture
   must stop and the surface must read the server's own sentence with **Sign
   In** and **Close**. On `main` before M2, and on M2's first draft, this drew
   nothing at all.
3. **M5's create ladder, against a cold NAS.** Start a title whose create
   answers 503 `startup_timeout`. Expect: a `preparing` spinner carrying the
   **server's own sentence**; creates at roughly 0 / 1 / 3 / 7 s in the server
   log, **all under one `request_id`**; and if the server never finishes,
   `exhausted` with Try again and Close, `rate == 0`.
4. **The late release.** Same, but let the server answer *after* the 60 s
   deadline. The prompt must appear **at 60 s, not when the create returns**,
   and the server must see a `DELETE /hls/<session>` for the late session.
   A leaked transcode is what is on the other side of this property.
5. **Cancel on a newer intent.** Start a create, then seek or change quality
   while it is retrying: the sequence must stop posting and raise nothing.
6. **The B2 route.** Let the 60 s prompt appear, tap **Try again** while the
   first create is still in flight, and confirm the second sequence gets its
   own watchdog — its prompt must arrive 60 s after *its* first attempt, not
   never.
7. **Regression.** A bound stall reopen the server refuses with 400 must still
   take the unbound re-post, which now also goes through the retry sequence.
8. **The ledger.** Open Playback debug on tvOS and read the SURFACE section:
   five rows, the last a notes-strip History block of up to sixteen lines.

Capture Playback debug's SURFACE section for 1, 3 and 4.

## 7. The build claim

`clients/apple/README.md` claims build **149**, which is what carried M5; M2
was 147. If `main` has moved and another Apple change landed first, re-claim
above it:

```bash
make apple-build-bump
```

`tests/operations/test_apple_build_claims.py` is what enforces the claim.

## 8. What to report

1. Whether `make apple-build` compiled, on **both** schemes. If not: the
   compiler output verbatim, and which of §2's numbered risks it was (or that
   it was none of them).
2. Whether `make apple-test` passed, on **both** destinations, and that
   `testPlaybackSurfaceModelRunsEveryContractCase` ran and passed under each
   of them (not only iOS).
3. Every test that failed, with its assertion message. Do not summarise a
   failure as "a test failed".
4. For each of A1–A6: which test failed under the mutation, or that the suite
   stayed green (which is a finding).
5. For each numbered run in §6: what the surface actually was, and the SURFACE
   section where one was asked for.
6. Anything you fixed, as a diff — and if the fix changed what the player
   does rather than what it compiles to, say so loudly. §6 of
   [the implementation plan](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)
   forbids retuning a ladder, a budget, a threshold or a detector on this
   work.
