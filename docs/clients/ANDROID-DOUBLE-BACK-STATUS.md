# Android double Back — delivery status

**Status:** focused Android validation passed; delivery outcome tracked on PR #523 ·
**Updated:** 2026-09-25

Companion to the [root cause analysis](ANDROID-DOUBLE-BACK-RCA-AND-FIX.md)
and [implementation handoff](ANDROID-DOUBLE-BACK-IMPLEMENTATION-HANDOFF.md).
This page records the state of the delivery branch, including work that has
not yet been proved. Read the RCA for the stack failure and the handoff for
the navigation contract.

## Candidate

| Item | Value |
|---|---|
| Branch | `codex/android-double-back-20260925` |
| Base | Forgejo `main` at `196d2a43e8015f142022d56f6c3a4b7c167e5bb1` |
| Worktree | Separate agent clone at `/private/tmp/plurx-agent-android-double-back-20260925` |
| Change | Bind Back and Exit callbacks to their originating navigation entry at 16 destinations; preserve Home and intermediate pages after repeated taps. |
| Lint repair | Opt in to Media3's existing `UnstableApi` usage in `PlaybackService.kt`. |
| Gate repairs | Advance Android source build from 125 to 126 and add the navigation regression anchor. Main now carries the independently merged errata for earlier PRs #519 and #522. |
| PR | [#523](http://192.168.4.7:3000/noirr/plurx/pulls/523) is the authoritative live record for CI and merge state. |

## Milestones

| Milestone | State | Evidence or next action |
|---|---|---|
| Recover candidate on current main | Done | The Android navigation files had no upstream changes since the candidate base; the branch was rebased again after an unrelated streaming PR reached main. |
| Review scoped implementation | Done | One adversarial agent review of PR #523 found no actionable issues. Six instrumented regressions use the real navigation controller and Back button. |
| Validate final tree | Local pass | App and test APKs built; `lintDebug` passed; seven focused API 36 emulator tests passed with zero failures, errors, or skips; four docs index tests passed. See PR #523 for the exact-head main fast lane result. |
| Physical phone and Android TV | Pending | No physical device result is claimed. |
| Merge to main | See PR #523 | Merge requires resolved review and a green current-head fast lane. The linked PR shows the final outcome. |

## How to read this page

"Prepared" means source is present in the branch, not that validation passed.
"Pending" is unexecuted work. A green build or CI run for an older commit
does not qualify a newer candidate. The previous candidate's emulator and JVM
results are historical evidence in the RCA, not validation of this branch.

## Final validation run

The reviewed Android source was at `855e46cf8e3748747931117bfb605fd3da18ee49`
on base `60f3803d1d5dc431a919235fab328ae6ea86d394`. On the disposable
`plurx_pixel_10_pro_xl_api36` emulator, explicitly selected as
`emulator-5580`, this command passed:

```bash
cd clients/android
ANDROID_SERIAL=emulator-5580 ANDROID_HOME=/Users/pjunod/Library/Android/sdk \
  ./gradlew --offline --no-daemon \
  :app:assembleDebug :app:assembleDebugAndroidTest :app:lintDebug \
  :app:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=tv.plurx.app.BackNavigationTest,tv.plurx.app.ui.DetailBackButtonTest
```

The instrumented XML reports **7 tests, 0 failures, 0 errors, 0 skipped**:
six in `BackNavigationTest` and one in `DetailBackButtonTest`. The Gradle
build finished successfully in 1m 35s. Lint produced reports without a
blocking issue. The docs index command
`python3 -m unittest discover -s tests/operations -p test_docs_index.py`
passed four tests, and `git diff --check` passed.

The full Android JVM suite was deferred to the later batch process requested
for this delivery. Physical touch and D-pad acceptance have not been run.

## Scope

This fix changes Android navigation callbacks, adds their regressions, and
acknowledges an existing Media3 API use. The main fast lane also required
Android `versionCode` 126 and the navigation history anchor before it could pass.
The unrelated history errata for PRs #519 and #522 were merged separately into main.
It introduces no optional feature or enable switch. It does not alter
playback policy, server behavior, release signing, or CI workflows.
