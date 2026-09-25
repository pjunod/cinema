# Android double Back — root cause and proposed navigation fix

**Status:** focused Android validation passed; CI and merge state on PR #523;
release acceptance pending · **Written:** 2026-09-25 · **Updated:** 2026-09-25

Companion to [the Android client guide](../../clients/android/README.md)
and [the development pipeline](../DEVELOPMENT_PIPELINE.md). This document
explains the reported blank screen, the evidence for its cause, and the
proposed fix. Review the diagnosis and candidate patch together; local
test results do not constitute release approval.

Execution handoff: [Sol build and delivery plan](ANDROID-DOUBLE-BACK-IMPLEMENTATION-HANDOFF.md).
Live progress: [delivery status](ANDROID-DOUBLE-BACK-STATUS.md).

The initial investigation used `bafeb08766ce057634f3fab0850cdd9e03507a98`.
Following Fable's review on 2026-09-25, the candidate was reapplied to
`38f61dfe6` for the historical validation below. Its selected files were
then carried to `60f3803d1d5dc431a919235fab328ae6ea86d394`
and later rebased to `196d2a43e8015f142022d56f6c3a4b7c167e5bb1`
on `codex/android-double-back-20260925` in a separate agent clone. Validation
below describes the older tree until the final gate is recorded. Other workspace
changes are outside this proposal. No server, Rust, Apple, or web changes
are required.

## 1. Finding — the second callback can remove the page underneath

The movie page supplies an unconditional `nav.popBackStack()` to its Back
button. Each invocation operates on whichever destination is current. The
callback does not verify that the movie still owns the top entry.

With Home underneath the movie, the first invocation removes the movie and
the second removes Home. The controller then has no current destination,
leaving no destination content for the host to render.

This mechanism was reproduced using the real Compose navigation controller
and the movie page's actual Back button in an emulator test harness. It
matches the user's report. The physical touch sequence and animation timing
on the reporting device have not been captured.

## 2. Report and reproduction

**Reported behavior:** open a movie item page in Cinema for Android, tap
the back arrow twice quickly, and encounter a completely blank screen.
The user reports having to force-close and reopen the app to recover.

**Expected behavior:** return to the page that opened the movie. A repeated
callback from the departing movie must not remove that preceding page.

The destination stack below omits the navigation graph entry:

```text
Home → Movie detail
          │ first Back callback
          ▼
Home
          │ second callback from the same movie page
          ▼
No current destination → blank content
```

With a deeper stack, the same defect causes an extra back step:

```text
Home → Library → Movie detail
first callback  → Home → Library
second callback → Home              (Library was skipped)
```

[BackNavigationTest](../../clients/android/app/src/androidTest/java/tv/plurx/app/BackNavigationTest.kt)
covers both immediate duplicate delivery and delivery during the exit
animation. The immediate tests invoke the captured semantic click action
twice in one UI turn. The animated test freezes the Compose clock, clicks
Back, advances the clock by 100 ms, asserts that the outgoing arrow is still
displayed, then clicks that node again. Both use the real button action.
The harness uses simple Home, Library, and Player destinations; it does not
load a movie from a server or exercise the complete signed-in application.

## 3. Root cause — the callback has unrestricted stack authority

| Source | Role |
|---|---|
| [MainActivity.kt](../../clients/android/app/src/main/java/tv/plurx/app/MainActivity.kt), `MainNav` | Creates `detail/{id}` and supplies its Back callback. Before the patch, it unconditionally popped the controller's current entry. |
| [DetailScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/DetailScreen.kt), `DetailBackButton` | Passes the callback through for loading, error, and loaded content. |
| [SafeAreas.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/components/SafeAreas.kt), `SafeBackButton` | Connects the callback to the clickable arrow. It owns layout, not navigation policy. |

The outgoing screen's exit transition provides a reproduced delivery
window: the animated regression successfully clicks the still-displayed
arrow 100 ms after the first click. Without the guard, the second click
leaves `currentDestination == null`. This proves the transition window in
the emulator harness; it does not measure the reporting device's timing.

Two protections are missing:

1. **Entry ownership.** A callback should only pop the exact back-stack
   entry that created it. Route equality is insufficient: multiple detail
   entries share `detail/{id}` and can even refer to the same movie.
2. **Root preservation.** An in-app Back action should refuse to pop when
   no previous destination exists. The emulator reproduction demonstrated
   that an unconditional root pop leaves `currentDestination == null`.

No crash trace or playback error was needed to reproduce this navigation
failure. Recovery through system Back from an already-blank app was not
tested; the force-close recovery is user evidence.

## 4. Proposed fix — bind each pop to its originating entry

The candidate adds this helper in
[BackNavigation.kt](../../clients/android/app/src/main/java/tv/plurx/app/BackNavigation.kt):

```kotlin
internal fun NavController.popBackStackFrom(entry: NavBackStackEntry): Boolean {
    if (currentBackStackEntry?.id != entry.id || previousBackStackEntry == null) return false
    return popBackStack()
}
```

Each destination captures its own entry and supplies:

```kotlin
onBack = { nav.popBackStackFrom(entry) }
```

**Contract:** calls execute on the UI thread, as existing navigation
callbacks do. If the originating entry is current and has a predecessor,
pop once and return the controller's result. Otherwise return `false`
without changing the stack. The helper adds no dispatching or locking.

After the first pop, the departing entry's ID no longer matches the current
entry. Repeated callbacks become harmless. The preceding page retains its
own working Back action. A detail page covered by playback can use Back
again after playback returns to it; there is no permanent consumed flag.

### 4.1 Apply the rule at all equivalent call sites

The candidate replaces all 16 argumentless Back/Exit pops in `MainNav`:
library, search, detail, reader, photo, settings, Live TV, recordings,
recording detail, recording activity, library channels, developer,
downloads, offline book reader, offline player, and player.

These callbacks share the same ownership defect. Applying the helper
consistently also prevents a delayed Exit callback from popping a newer
destination. The reviewer should check that behavior at each caller.

The targeted return to `library-channels` with `inclusive = false` remains
unchanged. It expresses a separate, multi-entry navigation intent and
cannot remove the target itself. Forward navigation and Play Next are
outside this patch.

### 4.2 Alternatives considered

| Alternative | Limitation |
|---|---|
| Debounce taps for a fixed interval | Requires an arbitrary duration and may suppress intentional navigation. It does not establish ownership of a delayed callback. |
| Only require a previous entry | Protects Home, but duplicate callbacks can still skip Library in a deeper stack. |
| Compare routes | Two stacked detail entries may have identical routes; the regression covers this case. |
| Consume the button permanently | Adds state that must reset correctly when screens are retained or revisited. Entry identity follows the actual stack. |
| Require a particular lifecycle state | Adds transition-dependent rejection of actions. The required condition here is stack ownership. |

### 4.3 Scope boundaries

This proposal does not redesign system/predictive Back, change playback
cleanup, repair an already-empty restored stack, alter animations, or add a
global tap throttle. Those behaviors need separate evidence if found faulty.
The fix prevents the identified path from emptying the stack.

## 5. Evidence — regressions fail before the guard and pass after it

The local run used `plurx_pixel_10_pro_xl_api36`, an Android 16/API 36 Pixel
emulator, explicitly selected as `emulator-5580`. No physical phone was
used. Navigation Compose is pinned to `2.9.8` in
[libs.versions.toml](../../clients/android/gradle/libs.versions.toml).

For the failing run, the new helper delegated directly to `popBackStack()`
without either guard, preserving the old callback behavior. All five new
regressions failed. This was a controlled reproduction in the harness,
not a test run of the shipped APK.

| Regression | Unguarded result | Guarded result |
|---|---|---|
| Double Back from a movie opened on Home | Destination became `null` | Home remained visible |
| Double Back from a movie opened in Library | Skipped Library | Library remained visible |
| Double Back with identical detail routes | Removed the preceding detail too | Preserved that exact entry ID |
| Detail callback while Player covers it | Popped Player | Refused stale callback; valid Back worked after returning from Player |
| Back at root | Destination became `null` | Refused pop; Home remained visible |
| Second click 100 ms into the exit animation | Outgoing arrow remained displayed; second click left destination `null` | Home remained visible |

**Current-main rerun (2026-09-25):** the candidate on `38f61dfe6` passes all
six navigation regressions and the existing
[DetailBackButtonTest](../../clients/android/app/src/androidTest/java/tv/plurx/app/ui/DetailBackButtonTest.kt):
**7 tests, 0 failures**. The additional animation test was first run without
the guard on that same base and failed with expected `home`, actual `null`.
Restoring the guard makes it pass. Kotlin compilation and APK packaging
also pass. The existing button test checks system-inset clearance.

**Full unit suite:** `testDebugUnitTest` passes **763 tests across 107 suites,
0 failures, 0 errors, 0 skipped** on the rebased candidate. The four failures
in the original `bafeb087` run came from that outdated base missing main's
`d258d8fbc` fix, `fix(android): give the surface presenter the
system_interruption source`. Fable identified the missing fix; the rerun
confirms it resolves those failures. The earlier description of an ongoing
playback-fixture alignment problem is superseded.

**Historical lint result:** the original-base candidate passed `lintDebug`. On `38f61dfe6`,
`lintDebug` reports two `UnsafeOptInUsageError` errors at line 14 of
[PlaybackService.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackService.kt):
`setMediaNotificationProvider` and `DefaultMediaNotificationProvider` require
the Media3 `UnstableApi` opt-in. That file is unchanged by this patch.
A separate run with the navigation changes removed reproduced both errors
on unmodified current-main Android source; the patch was then restored. The combined
validation command therefore exits unsuccessfully at lint, despite green
unit and instrumented tests; the rebased lint result is not claimed green.

The delivery branch adds a file-level Media3 `UnstableApi` opt-in to
`PlaybackService.kt`, following the convention already used by
`PlayerScreen.kt`. The service behavior is unchanged. The focused final
validation on `855e46cf8` passed `lintDebug`, built both APKs, and passed all
seven selected API 36 emulator tests with zero failures, errors, or skips.
The [delivery status](ANDROID-DOUBLE-BACK-STATUS.md) has the exact command
and scope. The full Android JVM suite is deferred to the requested batch
process; the historical 763-test result above remains evidence for the
older candidate, not this branch.

The ready main fast lane required the Android source build counter to advance
from 125 to 126 and a client regression anchor for the navigation fix. It
also exposed missing `Regression-Test` trailers in the immutable landing
messages of earlier PRs #519 and #522; the delivery branch records those
specific omissions in `validation/merge-errata.toml`. These are CI policy
repairs, not changes to the navigation or playback-service behavior.

To repeat the historical proof, start a disposable emulator and substitute its serial
below. Run from the repository root:

```bash
cd clients/android
ANDROID_SERIAL=emulator-5580 ./gradlew --offline --no-daemon \
  :app:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=tv.plurx.app.BackNavigationTest,tv.plurx.app.ui.DetailBackButtonTest
./gradlew --offline --no-daemon :app:lintDebug
./gradlew --offline --no-daemon :app:testDebugUnitTest
```

The focused command must report seven passing tests and the unit command
must report no failures. Lint is run separately to expose the current-main
issue above; it requires a separate fix before claiming all validation is
green. `--offline` assumes cached dependencies.

### 5.1 CI includes the regression in the full instrumented gate

[ci.yml](../../.github/workflows/ci.yml) builds the app and test APKs and
runs the entire instrumented suite on a disposable Android TV API 36
emulator through `make android-instrumentation-run`. That
[Makefile target](../../Makefile) applies no test-class filter, so it includes
`BackNavigationTest` automatically. An instrumented-test failure fails the
job; the workflow retries only emulator startup failures before tests begin.
The regression is therefore a full-CI gate, not local-only evidence.

Trigger scope matters: full CI runs by manual dispatch or a release tag.
The ordinary [PR fast lane](../../.github/workflows/main-fast-lane.yml)
compiles the affected Android app but does not run this instrumented suite.
The existence of the gate does not claim that CI has executed this candidate.

## 6. Review questions and release acceptance

Review the three Android files together: the helper defines the rule,
`MainActivity.kt` applies it, and `BackNavigationTest.kt` proves the stack
behavior. This document and its [index row](../README.md) complete the
review package. Recheck the references if the base moves.

1. Does exact entry identity plus a predecessor check capture every intended
   Back/Exit use at the 16 changed sites, including delayed player exits?
2. Is ignoring callbacks from covered or departed screens correct for each
   caller, with no required cross-screen exit action lost?
3. Is the harness evidence sufficient for code review, with physical
   acceptance tracked separately?

Before release, complete the repository's ordinary PR review and
affected-surface gate, resolve the current-main lint error, and test the
candidate APK on a physical Android device. Verify rapid double taps from
Home and Library, nested detail navigation, Back after playback returns,
ordinary system Back, and Android TV D-pad double-press on the focused
Back arrow. Acceptance means the expected preceding page
remains usable, repeated callbacks from a departing page skip no page, and
no blank screen requires an app restart.

### 6.1 Fable review disposition (2026-09-25)

Fable approved the diagnosis and all 16 guarded call sites at the original
base. The review answered all three questions above affirmatively: exact
entry identity covers delayed exits; no required cross-screen exit is lost;
and the harness is sufficient for code review with physical acceptance
tracked separately.

The two required follow-ups are to validate on current main and correct the
broader-suite and CI evidence in §5. The candidate has been moved to current
main; its validation results are recorded there. The optional animated-exit
regression was added, and physical Android TV double-press acceptance is
included above. Fable reviewed the original five-test patch; the additional
animation test was added after that review.

An adversarial agent review of [PR #523](http://192.168.4.7:3000/noirr/plurx/pulls/523)
found no actionable issues in the recovered final patch, including the
animation regression and Media3 opt-in. Merge, published APK, and physical
device acceptance remain pending.
