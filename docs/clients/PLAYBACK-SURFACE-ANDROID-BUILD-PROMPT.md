# Android build and test — compile the playback surface contract

**Status:** open · **For:** a session with an Android toolchain (Docker, or a
local SDK) · **Covers:** M3 (PR #279) and M5 (PR #282) of the [playback
surface contract](PLAYBACK-SURFACE-CONTRACT.md), both merged to `main`

Every line of Kotlin in those two PRs was written on a Linux box with no
Android SDK and no disk for a Gradle build. **Nothing has been compiled and no
JVM unit test has ever executed.** The reducer was checked by transliterating
the shipped Kotlin into Python, running all 57 fixture cases of the day
against it and fuzzing 3,500 sequences differentially against the shipped JS
reducer with zero divergences — that is evidence about the *semantics*, and
none at all about the Kotlin compiling.

Nothing here changes behaviour on purpose. If a step fails, the two honest
outcomes are (1) fix it there, when it is a local compile error — a missing
import, an exhaustiveness complaint, a nullability mismatch — or (2) stop and
report the compiler output verbatim, when the failure is about what the code
means. Do not delete a test, loosen an assertion, or add a fixture skip to get
green.

## 0. What you need

Either route works.

- **Pinned image (preferred).** Docker, and `make android-test` from the
  repository root — it builds `clients/android`'s image (JDK 25 + SDK) and
  runs `./gradlew --no-daemon testDebugUnitTest lintDebug` inside it. This is
  what CI runs, so a green here is the green that counts.
- **Local SDK.** A JDK and an Android SDK with the platform
  `clients/android/app/build.gradle.kts` compiles against, then the raw
  Gradle invocations below.

A checkout of `plurx` at `main` or later, with both PRs in:

```bash
cd ~/code/plurx && git fetch origin && git checkout main && git pull
ls clients/android/app/src/main/java/tv/plurx/app/player/PlaybackSurface.kt        # M3's presenter
ls clients/android/app/src/main/java/tv/plurx/app/player/PlaybackSurfaceOwner.kt   # M3's owner arm
```

## 1. Compile — this is the real unknown

```bash
cd clients/android && ./gradlew :app:compileDebugKotlin
```

First, because everything below depends on it. The Kotlin was reviewed by
reading, for: `internal` visibility on `surface`, `surfaceHistory`,
`surfaceRevision` and `surfaceAction`; `when` exhaustiveness on the sealed
types; nullability; delegated-property smart casts in `PlayerContent`;
property-initialisation order against the `init` coroutines; and brace balance
by script. None of that is a compiler.

**Highest-risk constructs, in the order worth checking if it fails.**

From M3 (`PlaybackSurface.kt`, `PlaybackSurfaceOwner.kt`, `Controller.kt`,
`PlayerScreen.kt`):

1. `when` exhaustiveness over the sealed `PlaybackSurface` /
   `SurfaceClass` types, in the reducer and in both rewritten Composables.
2. Delegated-property smart casts in `PlayerContent` — a `by` delegate is not
   smart-cast the way a plain `val` is.
3. Property-initialisation order in `Controller` against the coroutines
   launched from `init`.
4. `internal` visibility of `surface`, `surfaceHistory`, `surfaceRevision`,
   `surfaceAction` against every call site in `PlayerScreen` and the tests.
5. Every pre-existing test that compiles against the changed
   `Controller` / `PlayerScreen` signatures — `onError: (String) -> Unit`,
   `playFailure` and `playbackNotice` are **deleted**, so anything that still
   names them fails to compile rather than fails an assertion.

From M5 (`StallReopen.kt`, `PlaybackPolicy.kt`, `BehindLiveWindowRecovery`):

6. The `coroutineScope { launch { … } … outcome }` shape of
   `createRetrySequence`, and its labelled `break@attempts` /
   `continue@attempts` out of a `catch`.
7. Exhaustiveness of `when (val step = createRetryStep(…))` over the sealed
   `CreateRetryStep`.
8. `data object Fail`.
9. `player.isCurrentMediaItemLive`.
10. `PlaybackException.ERROR_CODE_BEHIND_LIVE_WINDOW`.
11. The local `data class Transport` inside `MediaOriginTest`.
12. The `runTest` + `TestScope.currentTime` harness in `CreateRetryTest`,
    which imports `kotlinx.coroutines.test.currentTime`.

## 2. Unit tests, lint, and an APK

```bash
cd clients/android && ./gradlew :app:testDebugUnitTest :app:lintDebug :app:assembleDebug
```

or, in the pinned image, `make android-test` (unit tests + lint) from the
repository root.

`lintDebug` is there for unused imports and Compose lint on the two rewritten
renders. `assembleDebug` is there because a release-shaped build can fail
where a unit-test compile does not.

### What must be true when it passes

**`PlaybackSurfaceReducerTest`** — the part that must not drift:

- `everyFixtureCaseRuns` iterates every case of
  `tests/playback/playback-surface-contract.json`, which holds **60** today
  (it was 57 before M5 added `m5_create_retry_deadline`, `m5_hls_retry_once`
  and `m5_behind_live_window_finite`). It asserts the fixture is non-empty and
  that every case carries `events`, so a missing or truncated resource fails
  rather than passing on fewer cases.
- `classesAreTheFixtureVerbatim`, `sourcesAreTheFixtureVerbatimAndInOrder`,
  `timingsAndRanksAreTheFixtureVerbatim` — the Kotlin tables **are** the
  fixture, so a client that skipped a port fails here rather than drifting.
- `aBlockingSurfaceIsNeverDrawnOverAPlayerNobodyStopped`,
  `replayingACaseTwiceGivesTheSameAnswer`.

**`PlaybackSurfaceOwnerTest`** — one stop-before-raise test per site. Every
blocking site does its **own** stop, deliberately duplicated, so deleting one
stop fails exactly one test and the failure names the site:

`theSessionlessSecondStallStopsBeforeItRaises` ·
`theSpentReopenBudgetStopsBeforeItRaises` ·
`theTargetPresentationDeadlineStopsBeforeItRaises` ·
`onPlayerErrorFailStopsBeforeItRaises` ·
`aFailedSessionCreateWithNothingBehindItStopsBeforeItRaises` ·
`theControlPlaneTerminalVerdictStopsBeforeItRaises`

plus `anAuthRefusalKeepsItsSignInThroughEverySite`,
`aRefusedChangeCarriesItsRetryAndNeverStopsThePlayer`,
`aBlockingFaultRaisedWithoutTheStopIsRefusedOutright`, the disagreement pair
(`aFilmClockAdvanceOnTheSameEpochAfterAStopIsADisagreement`,
`anIsPlayingChangedAloneIsNotADisagreement`), `aHiddenAppSamplesNothing`,
`theLedgerRingIsBounded` and `nothingIsPublishedUntilTheAnswerChanges`.

**`PlaybackInfoContractTest`** — the five SURFACE rows in fixture order.

**`CreateRetryTest`** — ten cases:
`ladderIsOneSecondTwoSecondsFourSecondsAndThenSpent` ·
`deadlineIsAbsoluteAndNeverSchedulesARungPastItself` ·
`onlyANotYetAnswerIsRetriedAtAll` ·
`theSequenceReplaysOneIdentityOnTheContractsLadder` ·
`aSuccessOnARetryIsAttachedAndNothingIsReleased` ·
`aSuccessAfterTheAbsoluteDeadlineIsReleasedAndNeverAttached` ·
`aRefusalTheLadderDoesNotClaimIsRethrownUnchanged` ·
`aNewerIntentEndsTheSequenceWithoutRaisingAnything` ·
`aChangeContextCreateIsNotRetriedAndArmsNoWatchdogAtAll` ·
`aChangeContextRefusalIsRethrownRatherThanRetried`

**`BehindLiveWindowRecoveryTest`** — six cases:
`theFirstFiniteBehindLiveWindowSeeksBackOnceAndPreparesOnce` ·
`theSecondOneOnTheSameAttachDoesNothingAtAll` ·
`aNewItemOnTheScreenGetsItsOwnSingleRecovery` ·
`aLiveItemIsNeverSeekedAndNeverPrepared` ·
`noOtherErrorCodeTouchesThePlayer` ·
`theSeekTargetIsReadOnlyOnThePathThatSeeks`

### The `CreateRetryTest` note about `runTest` and virtual time

`CreateRetryTest` runs on `kotlinx.coroutines.test`'s `runTest` and virtual
time. The hand-rolled clock, the `yield()` and the `sleep` seam that the first
draft used are gone; **one `nowMs` seam remains on purpose**, because
`System.nanoTime()` does not move with virtual time — and that seam is what
makes "arms no clock at all" expressible.

If `aSuccessAfterTheAbsoluteDeadlineIsReleasedAndNeverAttached` or
`aChangeContextCreateIsNotRetriedAndArmsNoWatchdogAtAll` is flaky under
`advanceUntilIdle()`, **tighten the scheduler use rather than loosening the
assertion.** `currentTime == 0` in the change-context case is blocker B1's
whole proof: it says the 60 s stop-the-player watchdog is not merely late
outside the start context, it is never armed. An assertion changed from
`currentTime == 0` to "no surface was raised" is a strictly weaker test that
passes under the defect.

## 3. Mutate and confirm — the tests have never killed anything

For each mutation: apply it, run `./gradlew :app:testDebugUnitTest`, confirm
**the named test fails**, then `git checkout` the file. A mutation that leaves
the suite green is a finding — report it with the mutation and the file.

| # | Mutation | Must fail |
|---|---|---|
| K1 | Delete the `player.stop()` from any one blocking site in `PlaybackSurfaceOwner.kt` | **exactly one** `…StopsBeforeItRaises` case, and the failure must name that site. If more than one fails, the duplication that makes this diagnostic has been collapsed |
| K2 | Make `SessionCreateCoordinator.createRetryingNotYet` arm the deadline watchdog regardless of `startContext` (blocker B1) | `aChangeContextCreateIsNotRetriedAndArmsNoWatchdogAtAll`, on `currentTime == 0` |
| K3 | Attach the late session instead of releasing it | `aSuccessAfterTheAbsoluteDeadlineIsReleasedAndNeverAttached` |
| K4 | Drop the third rung: `PlaybackPolicy.CreateRetry.backoffMs = listOf(1_000, 2_000)` (`PlaybackPolicy.kt:135`) | `ladderIsOneSecondTwoSecondsFourSecondsAndThenSpent`, and `node tests/playback/web-policy.test.js` on any machine |
| K5 | Let `BehindLiveWindowRecovery` fire on a live timeline | `aLiveItemIsNeverSeekedAndNeverPrepared` |
| K6 | Let it fire more than once per attach | `theSecondOneOnTheSameAttachDoesNothingAtAll` |
| K7 | Delete the budget re-arm at `commitPreparedReplacement` (should-fix S1) | `aNewItemOnTheScreenGetsItsOwnSingleRecovery` |
| K8 | Replace `seekTo(target)` with `seekToDefaultPosition()` | `theFirstFiniteBehindLiveWindowSeeksBackOnceAndPreparesOnce` — Media3's default position is a **live-edge** policy and on a finite timeline it skips content, which is why it is not used |

## 4. The fence bypasses — already verified, do not redo

`scripts/playback-surface-fence` is Python and runs on any machine. All eight
Kotlin bypass spellings were pasted into the real `Controller.kt` and each one
tripped the fence (`docs/playback-surface-handoff`, 2026-09-13):

```
_surface.value = value
_surface.tryEmit(value)
_surface.update { value }
val blocking = PlaybackSurface.Blocking(fault)
val forged = PlaybackFault(cls = SurfaceClass.Stopped, source = "owner_stopped", …)
surfaceOwner.raise(source = "owner_exhausted", …)
raiseBlocking(source = "owner_exhausted", …)
var surfaceMessage by mutableStateOf<String?>(null)
```

Each also has a must-trip fixture line in
`tests/operations/fence-fixtures/surface/must_trip.kt` and
`must_trip.owner.kt`, checked by
`tests/operations/test_playback_surface_fence.py`. Nothing about them needs an
Android toolchain. They are named here so nobody re-runs them believing they
are owed.

## 5. Instrumentation and device runs

```bash
cd clients/android && ./gradlew :app:connectedDebugAndroidTest
```

Then, by hand — each of these is a behaviour no unit test can see.

1. **TV focus order.** `PlaybackFailed` focuses the *first* action button.
   Focus order across **Keep waiting / Retry / Back** is untested, and so are
   the new focusable buttons in the banner. The banner's buttons sit over the
   picture and can take focus: check they do not steal it from the transport.
2. **Emulator, §4.4's recipe.** Inject a 503 on a segment after 30 s of
   playback: expect a `recovering` indicator, the picture continuing from
   buffer, the existing failover/reopen running, and **no blocking surface**
   unless the ladder is spent — and if it is spent, `playWhenReady == false`
   under it.
3. **Device, a control-plane `terminal` verdict mid-film.** The viewer must
   get a full-screen `stopped` carrying the server's sentence, with **no
   reopen attempted and no budget spent** (ruling R2). This row of §3.4 was
   amended by M3: followed literally it produced a frozen picture with no
   surface at all.
4. **Media3's refusal bodies.** Confirm Media3 actually populates
   `HttpDataSource.InvalidResponseCodeException.responseBody` for playlist and
   segment refusals, and that the bounded 4-deep `cause` walk reaches it.
5. **Retrofit's refusal bodies.** Confirm the buffered `errorBody()` is still
   readable where `createHlsSession` reads it, and that a 400 still matches
   the create coordinator's `isBadRequest` through `refusalStatusOf`.
6. **A 401/403 mid-playback** shows **Sign in**, and it lands on the sign-in
   screen.
7. **Playback debug renders the SURFACE section**, and a clear that does not
   change the drawn surface still updates History (`surfaceRevision`).
8. **The opaque `PlaybackFailed` under PiP.**
9. **M5's create ladder, against a cold NAS.** A create answered 503
   `vod_index_pending` must show the spinner with the **server's own
   sentence**, post four creates under **one `request_id`**, and end in
   `exhausted`. Capture the SURFACE section of Playback debug.
10. **B1's field check.** Start a title successfully, then change quality
    against a cold NAS so its create takes longer than 60 s. The picture must
    keep playing **under a banner**; there must be **no full-screen prompt**,
    and the session the change eventually creates must **attach**, not be
    released. This is §7 recipe (b)'s explicitly not-allowed outcome, and it
    was reachable before `b4cf8e81`.
11. **`BEHIND_LIVE_WINDOW` on a finite title.** Every HLS session has been VOD
    since `live_presentation_removed`. Reproduce a 1002 and expect **one**
    seek-and-prepare with playback resuming at the same position — *not* a
    jump backwards to the window start — and only one recovery per attach.
    Capture `playback_behind_live_window` from the client log. **This is the
    addition with the least evidence:** its author could not construct a 1002
    without a device.
12. **Then commit a prepared quality successor and reproduce 1002 again on
    it.** The successor must get its own recovery (should-fix S1).

## 6. The build claim

`clients/android/app/build.gradle.kts` has `versionCode = 92`, which is what
carried M5; M3 was 90. Confirm **92 is what installs**, and that
[ANDROID-CLIENT-PARITY.md](ANDROID-CLIENT-PARITY.md) and
`clients/android/README.md` agree with `build.gradle.kts`.
`tests/operations/test_mobile_build_claims.py` is what enforces that.

## 7. What to report

1. Whether `:app:compileDebugKotlin` compiled. If not: the compiler output
   verbatim, and which of §1's numbered risks it was (or that it was none of
   them).
2. Whether `:app:testDebugUnitTest`, `:app:lintDebug` and `:app:assembleDebug`
   passed, and that `everyFixtureCaseRuns` ran and passed.
3. Every test that failed, with its assertion message.
4. For each of K1–K8: which test failed under the mutation, or that the suite
   stayed green (which is a finding). For K1, which site you deleted the stop
   from and whether exactly one test failed.
5. For each numbered run in §5: what the surface actually was, plus the
   SURFACE section and the client-log lines where one was asked for.
6. Anything you fixed, as a diff — and if the fix changed what the player does
   rather than what it compiles to, say so loudly. §6 of
   [the implementation plan](PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md)
   forbids retuning a ladder, a budget, a threshold or a detector on this
   work.
