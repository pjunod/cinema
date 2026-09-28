# Android double Back — Sol build and delivery handoff

**Status:** focused Android validation passed; CI and merge state on PR #523 ·
**Written:** 2026-09-25 · **Updated:** 2026-09-25
**Executes:** Fable's approved diagnosis and navigation contract in the
[RCA and proposed fix](ANDROID-DOUBLE-BACK-RCA-AND-FIX.md), including the
required current-main rerun and corrected CI evidence.

Read the RCA first, then use the [delivery status](ANDROID-DOUBLE-BACK-STATUS.md)
for the active branch and milestone evidence. The uncommitted candidate
described below was recovered into a separate agent clone on a newer main.
The recovery instructions remain as historical context for that patch.

This is one Android bug fix with one supporting lint repair, not a
multi-task effort. No effort branch or parallel implementation is needed.
If delivery appears to require server, playback-policy, dependency, or CI
changes beyond the explicit scope below, identify that new issue separately.

## 1. Outcome and current state

Two rapid clicks on a movie's Back arrow must return to the preceding page
once. A second callback from the departing movie must not remove Home,
skip Library, or remove a preceding detail page. The user should never need
to restart the app because this action emptied navigation.

### 1.1 Where the candidate lives

These are local handoff locations, recorded on 2026-09-25:

| Item | Recorded value |
|---|---|
| Preferred working tree | `/private/tmp/plurx-agent-android-double-back-20260925` |
| Branch | `codex/android-double-back-20260925` |
| Current main base | `60f3803d1d5dc431a919235fab328ae6ea86d394` |
| Original checkout | `/Users/pjunod/code/plurx` |
| Original base | `bafeb08766ce057634f3fab0850cdd9e03507a98`; do not deliver from this base |
| Commit/push/PR state | See the delivery status; no APK publication is in scope. |
| Writable code already prepared | Navigation helper, all 16 call sites, six new instrumented tests |

The earlier candidate lived at
`/private/tmp/plurx-android-double-back-20260925` on `38f61dfe6`.
The original checkout contains unrelated user work. The agent clone now
carries the selected patch on current main; neither older tree is a delivery
base.

The earlier worktree was an uncommitted source for the selected files.
Review commits on the agent branch and the current status page for durable
delivery state. Do not copy an older checkout's entire tree or docs index
over current main.

### 1.2 What has already been proved

| Evidence on the recorded current-main candidate | Result |
|---|---|
| Five original navigation regressions with the guard removed | All failed on the original investigation tree |
| New animated-exit regression with the guard removed on `38f61dfe6` | Failed: expected Home, actual current destination `null` |
| Six navigation regressions plus existing Back-button inset test | 7 passed on Pixel API 36 emulator |
| Full Android JVM suite | 763 tests, 107 suites; zero failures, errors, or skips |
| Kotlin compilation and instrumented APK packaging | Passed |
| Android lint | Failed with two Media3 opt-in errors in `PlaybackService.kt` |
| Base-only lint, with this navigation patch removed | Same two errors; patch restored afterward |
| Docs index checks | Four passed |
| Physical phone / Android TV acceptance | Outstanding |

The original four `PlaybackSurfaceReducerTest` failures were caused by the
old base missing main's `d258d8fbc` system-interruption fix. That issue is
resolved by the rebase. Do not reintroduce the old RCA caveat or change the
playback fixture to make those obsolete failures disappear.

Fable approved the original five-test patch and all 16 call sites. The
animated-exit test was added afterward and proved to fail before/pass after
the guard. The lint annotation proposed below has not yet been applied or
reviewed as part of a final patch.

## 2. File ownership and scope

All repository-relative links here target the current-main candidate.

| File | Required work |
|---|---|
| [BackNavigation.kt](../../clients/android/app/src/main/java/tv/plurx/app/BackNavigation.kt) | Retain the reviewed entry guard. New file; explicitly stage it. |
| [MainActivity.kt](../../clients/android/app/src/main/java/tv/plurx/app/MainActivity.kt) | Retain all 16 guarded Back/Exit callbacks and capture each destination's entry. |
| [BackNavigationTest.kt](../../clients/android/app/src/androidTest/java/tv/plurx/app/BackNavigationTest.kt) | Retain the six behavior tests, including the animation test. New file; explicitly stage it. |
| [PlaybackService.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackService.kt) | If still needed on freshly fetched main, add only the required Media3 opt-in. |
| [RCA](ANDROID-DOUBLE-BACK-RCA-AND-FIX.md) | Update final base, validation, lint disposition, review, and delivery status. |
| [This handoff](ANDROID-DOUBLE-BACK-IMPLEMENTATION-HANDOFF.md) | Mark milestones and completion truthfully. |
| [Docs index](../README.md) | Keep both Android double-Back documents indexed in the same commit that adds them. |

Do not edit Rust, Cargo.lock, playback state machines, server APIs, web or
Apple clients, navigation dependencies, signing, or workflows for this fix.
The original scope excluded app versions, but the main fast lane requires a
`versionCode` increase when Android release inputs change; the execution
update in §9.1 records that bounded exception. No timing throttle, route-only guard, permanent consumed flag,
or new global navigation state is needed. Preserve the existing targeted
library-channel return and Play Next behavior.

Do not add source-text tests asserting the number of guard calls. The
behavioral regressions exercise the real navigation controller; counting
sites is a review aid, not a replacement for those tests.

## 3. Implementation contract

### 3.1 One helper owns the stack check

Reverify the candidate source before changing it. The package and signature
must remain compatible with `MainActivity.kt` and the instrumented tests:

```kotlin
package tv.plurx.app

import androidx.navigation.NavBackStackEntry
import androidx.navigation.NavController

internal fun NavController.popBackStackFrom(entry: NavBackStackEntry): Boolean {
    // An outgoing screen can still receive taps during its exit animation.
    // Bind Back to that exact entry, not its route (detail pages share a route),
    // so a repeated or delayed callback cannot pop the page underneath it.
    if (currentBackStackEntry?.id != entry.id || previousBackStackEntry == null) return false
    return popBackStack()
}
```

Calls execute on the UI thread. The callback may pop only when its original
entry is still current and a preceding destination exists. Rejected calls
return `false` without modifying navigation. Successful calls return the
controller's result.

Entry identity matters: two visits to the same movie can share a route and
arguments while representing different stack entries. The root check is a
safety net; the current Home destination exposes no such callback.

### 3.2 Capture the destination entry at every callback

Use the entry passed into the `composable` block, not a freshly read
`nav.currentBackStackEntry` inside the click callback. Reading the latter
would authorize the stale callback to remove the new current page.

```kotlin
composable("search") { entry ->
    SearchScreen(
        vm = vm,
        onOpenItem = { id -> nav.navigate("detail/$id") },
        onBack = { nav.popBackStackFrom(entry) },
    )
}
```

The same rule applies to `onExit`. Verify these 16 destinations against the
actual graph after updating the base:

| Destination | Callback |
|---|---|
| Library | `onBack` |
| Search | `onBack` |
| Detail | `onBack` |
| Reader | `onExit` |
| Photo | `onBack` |
| Settings | `onBack` |
| Live TV | `onBack` |
| Recordings | `onBack` |
| Recording detail | `onBack` |
| Recording activity | `onBack` |
| Library channels | `onBack` |
| Developer | `onBack` |
| Downloads | `onBack` |
| Offline book reader | `onExit` |
| Offline player | `onExit` |
| Player | `onExit` |

The reference graph has 16 `popBackStackFrom(entry)` calls and one unchanged
`popBackStack("library-channels", inclusive = false)`. If main adds routes,
review their intent instead of mechanically forcing the old count.

### 3.3 Preserve delayed-callback and system-Back behavior

A reader's late JavaScript completion must not pop a different page after
the reader has left. A previous player's late Exit after Play Next must
not remove the new player. An entry covered by playback must regain its
ordinary Back behavior when playback returns to it.

Do not replace NavHost's system/predictive Back handling. Screens that route
their own `BackHandler` through `onBack` or `onExit` receive the new guard
through those callbacks. System Back at Home retains platform behavior;
the root-safety test does not prescribe a new system-Back policy.

### 3.4 Resolve the narrow lint prerequisite

Fetch main first: another task may already have fixed this. If the error
remains, follow the existing convention at the top of
[PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt)
and add this file annotation before the package declaration in
`PlaybackService.kt`:

```kotlin
@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player
```

This acknowledges the Media3 API already used by
`setMediaNotificationProvider(DefaultMediaNotificationProvider(this))`.
It must not change service lifecycle or notification behavior. Do not use
a lint baseline, `abortOnError = false`, dependency changes, or broad warning
suppression to hide the error. The acceptance check is `:app:lintDebug`.

Keep the lint repair identifiable in the diff and PR description. It can
be a separate commit within this ordinary task branch. If fresh main
already contains an equivalent repair, omit it entirely.

## 4. M1 — recover the candidate and establish the intended base

In the preferred worktree, inspect before mutating:

```bash
cd /private/tmp/plurx-android-double-back-20260925
git status --short
git branch --show-current
git rev-parse HEAD
git diff -- clients/android/app/src/main/java/tv/plurx/app/MainActivity.kt
git fetch origin main
git rev-parse origin/main
```

Read the current [AGENTS.md](../../AGENTS.md) and pipeline rules. Preserve
the known untracked candidate files and docs before any base update. Never
reset, clean, or stash the original checkout wholesale: it contains other
work. Review and stage an explicit file list, not `git add .`.

If fetched main moved, preserve the candidate in a normal commit and rebase
onto that main, or carry the exact selected files/diff to a new clean
worktree. Resolve conflicts by retaining §3's behavior. Do not replace the
entire `MainActivity.kt` from the old checkout if upstream has changed it.
Run the final checks again on the resulting tree; the recorded results are
not proof for a different base.

Commit normally with the repository's tracked hook. If the hook requires a
Rust toolchain unavailable on this host, use the documented compile loop
in [AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md); do not disable the
hook or use CI to discover compile errors. This task itself needs no Rust
source changes.

**M1 acceptance:** a known current-main base, the complete selected patch,
both docs and index rows, and no unrelated user files in the intended diff.

## 5. M2 — finish the patch and keep the regression meaningful

Inspect the candidate against §3, then apply the lint opt-in only if still
needed. Retain all six functions in `BackNavigationTest`:

| Test | Required observation |
|---|---|
| `rapidBackFromMovieKeepsHomeVisible` | Two immediate callbacks preserve Home and its rendered content. |
| `rapidBackFromMovieKeepsItsLibraryVisible` | Two callbacks preserve Library; they do not skip to Home. |
| `rapidBackBetweenIdenticalDetailRoutesOnlyPopsOneEntry` | Two identical routes still preserve the exact preceding entry ID. |
| `coveredDetailCannotPopPlayerButCanGoBackAfterPlayback` | Covered detail cannot pop Player; detail Back works again after playback returns. |
| `backAtRootNeverEmptiesNavigation` | Root refusal preserves Home. |
| `secondBackDuringExitAnimationKeepsHomeVisible` | The outgoing arrow is displayed and clickable 100 ms after the first click; the second click preserves Home. |

The tests must use the real `NavHost`/controller and `DetailBackButton`.
Preserve the animation test's frozen Compose clock and displayed-node
assertion. A test that only calls a saved closure twice does not prove the
additional animation-window observation.

No need to repeat the guard-removal experiment for an unchanged test and
guard: it was already demonstrated. If either is substantially rewritten,
repeat the smallest relevant test with the guard removed in a disposable
copy, confirm failure for the expected reason, restore the guard, and prove
the final candidate passes. Never commit or publish the unguarded variant.

**M2 acceptance:** the scoped implementation matches §3, no unguarded
variant remains, and the tests still exercise behavior rather than copied
predicates.

## 6. M3 — validate the final source locally

### 6.1 Compile, JVM tests, and lint

Use the repository-pinned Gradle wrapper and dependencies. The recorded
local SDK is `/Users/pjunod/Library/Android/sdk`; local SDK settings remain
untracked. From the candidate's Android directory:

```bash
./gradlew --offline --no-daemon \
  :app:assembleDebug :app:assembleDebugAndroidTest \
  :app:testDebugUnitTest :app:lintDebug
```

`--offline` is appropriate only with populated caches. If dependencies are
missing, resolve the declared dependencies normally; do not downgrade
versions to fit the cache. The repository's Docker alternatives are
`make android-test` and `make android-instrumentation-build` from its root.

Expect successful APK packaging, no JVM test failures, and successful lint.
The previous 763-test count is evidence for the recorded base, not a fixed
assertion if main adds tests. Record the new count. If the opt-in leaves
other errors, report their actual scope rather than describing lint as green.

### 6.2 Focused emulator proof

Select an explicitly disposable emulator. The previous Pixel AVD was
`plurx_pixel_10_pro_xl_api36`; `emulator-5580` is an example serial, not a
guarantee that the device is running or is the intended target. Verify it
with `adb devices -l` before installing anything.

From the candidate's Android directory:

```bash
ANDROID_SERIAL=emulator-5580 ./gradlew --offline --no-daemon \
  :app:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=tv.plurx.app.BackNavigationTest,tv.plurx.app.ui.DetailBackButtonTest
```

Expect **seven passing tests**, including the existing system-inset test.
Use the XML reports under `app/build/outputs/androidTest-results` and
`app/build/test-results/testDebugUnitTest` to record failures, errors, and
skips. Do not infer success from a compiled test APK.

### 6.3 Documentation and evidence

From the repository root:

```bash
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

The index test enumerates tracked files, so confirm both new documents are
explicitly staged/indexed and validate their relative links as well. Update
the RCA with the exact base, commands, counts, lint repair, and review state.
The new clean lint run supersedes the old failure; retain enough history to
explain why `PlaybackService.kt` is in this navigation PR.

The temporary logs from the previous investigation are optional references:
`/private/tmp/plurx-double-back-animation-before.log`,
`/private/tmp/plurx-double-back-current-main.log`, and
`/private/tmp/plurx-double-back-base-lint.log`. The combined candidate log
ends in a lint failure despite passing unit and focused emulator tests.
Do not pass it off as an all-green build receipt. Capture fresh results for
the final tree rather than relying on those temporary files surviving.

**M3 acceptance:** app/test APKs build; all JVM tests, seven focused emulator
tests, lint, and docs checks pass on the delivery tree. Base changes require
fresh evidence.

## 7. M4 — physical acceptance and CI evidence

### 7.1 Physical device checks

On an explicitly selected test phone and Android TV device, record device,
OS, app build/commit, action, observed destination, and pass/fail:

| Scenario | Expected result |
|---|---|
| Home → movie; rapidly tap Back twice | Home stays visible and usable. |
| Library → movie; rapidly tap Back twice | Library remains the preceding page; no extra pop. |
| Stacked detail pages; repeat Back | Only the departing detail is removed by its repeated callback. |
| Detail → playback → exit; then Back | Playback returns to detail; detail can still go back normally. |
| Ordinary system Back, including at Home | Existing platform behavior; no blank navigation host. |
| Android TV: focus the detail arrow and double-press D-pad center | The same preceding page remains usable; no blank screen. |

The automated harness proves the stack mechanism, not physical touch or
D-pad acceptance. If hardware is unavailable, record these cases as
pending, not passed. Do not claim release acceptance without them.

### 7.2 Do not confuse the two CI lanes

[Full CI](../../.github/workflows/ci.yml) runs the entire instrumented suite
on a disposable Android TV API 36 emulator via the
[Makefile](../../Makefile) target `android-instrumentation-run`. There is no
class filter, so the new regressions are included automatically. Test
failures propagate; only a pre-test emulator startup failure may retry.

That workflow runs on manual dispatch or release tags. The ordinary
[main PR fast lane](../../.github/workflows/main-fast-lane.yml) compiles the
affected Android app and does not run this instrumented suite. The PR must
therefore name the focused local run explicitly. No workflow changes are
needed to register the new tests.

For an intentionally requested full local instrumented run, build the APKs
then use the target with a disposable device only:

```bash
PLURX_ANDROID_SERIAL=emulator-5580 make android-instrumentation-run
```

That target uninstalls the application and its test package before installing
the APKs. Do not use it on a daily-use device with data to preserve.

**M4 acceptance:** distinguish local tests, executed CI results, and physical
results. Leave explicit pending entries where no execution occurred.

## 8. M5 — prepare the PR and landing evidence

Use the repository's current remote and tooling. The recorded origin is
Forgejo; the `github` remote is not the push destination. Recheck remotes
rather than assuming GitHub PR commands will target the right service.

Suggested title: `fix(android): prevent repeated back actions from emptying navigation`.
This is a behavior fix and must retain the `fix(` prefix under the current
corrective-history rules.

The description should state the trigger and result, explain any included
Media3 annotation, link the RCA and this handoff, record the exact validated
base and commands, summarize Fable's approval and follow-ups, and distinguish
pending physical checks. Include these literal fields, one per line:

```text
Regression-Test: clients/android/app/src/androidTest/java/tv/plurx/app/BackNavigationTest.kt::rapidBackFromMovieKeepsHomeVisible
Regression-Test: clients/android/app/src/androidTest/java/tv/plurx/app/BackNavigationTest.kt::secondBackDuringExitAnimationKeepsHomeVisible
```

Validate a saved PR body from the repository root:

```bash
python3 -m validation.regression_field \
  --body-file /private/tmp/android-double-back-pr.md \
  --landing-lines
```

Create that body file first with the actual description. The command emits
the same regression lines required in the landing commit. A warning that
the PR preflight does not execute an Android instrumented test is expected;
it does not replace the focused local test or mean the test is absent from
full CI. Fix unresolved paths or names instead of dropping the fields.

Open the main-bound PR as draft according to the current pipeline. Carry
Fable's approval and the disposition of both required follow-ups into the
review record. The added animation test and any lint annotation are changes
since that review; have the final delta covered before marking ready. Use
the existing review process, not parallel duplicate reviews. Marking ready
starts the fast lane. A green run on an older base/head is not evidence for
a moved candidate.

The build handoff prepares code and evidence; it is not an instruction to
publish an APK, tag a release, deploy, or merge without the required gate
and delivery authorization. Ordinary independent changes use the applicable
main fast lane, not the large-effort promotion workflow.

**M5 acceptance:** final scoped commits, indexed docs, a reviewable PR with
resolving regression fields and accurate evidence, and the current candidate's
required gate. Report the PR link and remaining physical/release work.

## 9. Completion record Sol must return

Return a concise report containing branch, final base and head, commit/PR
links, changed-file scope, lint disposition, exact validation commands and
counts, review disposition, and physical results or pending cases. Separate
“implemented,” “tests passed,” “merged,” and “released”; report only the
states actually reached. Update this handoff and the RCA in the same task
so the next reader does not inherit the temporary uncommitted-candidate state.

### 9.1 Execution update — 2026-09-25

The selected navigation patch and Media3 opt-in were committed to the
separate agent branch, rebased to main `196d2a43e`. Ready [PR #523](http://192.168.4.7:3000/noirr/plurx/pulls/523)
received one adversarial agent review with no actionable findings. The
reviewed Android source at `855e46cf8` built app and test APKs, passed
`lintDebug`, and passed seven selected API 36 emulator tests with zero
failures, errors, or skips. Four docs index tests and the PR regression-field
check passed. Exact commands are on the [status page](ANDROID-DOUBLE-BACK-STATUS.md).

The user's 2026-09-25 delivery instruction defers the full Android JVM suite
to a later batch process and calls for one post-review fast lane run. Thus
the M3 full-suite acceptance above is explicitly deferred, not claimed
complete. Physical phone and Android TV acceptance is still pending. Merge
and release are not yet claimed.

The ready fast lane exposed two repository-wide delivery requirements absent
from the initial scope: Android release inputs need `versionCode` 126, and
`make history-check` needs a client regression anchor for the navigation
commit plus errata for immutable missing trailers in earlier merged PRs
#519 and #522. The errata were independently merged in #525, and the candidate
was rebased to retain those upstream rows. The Media3-only lint commit was
relabeled `chore` because it does not change user behavior. The Android
version and navigation anchor repairs are in PR #523;
the [PR](http://192.168.4.7:3000/noirr/plurx/pulls/523) remains the live
record for the final run and merge outcome.
