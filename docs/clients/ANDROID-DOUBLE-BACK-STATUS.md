# Android double Back — delivery status

**Status:** implementation prepared, review and validation pending · **Updated:** 2026-09-25

Companion to the [root cause analysis](ANDROID-DOUBLE-BACK-RCA-AND-FIX.md)
and [implementation handoff](ANDROID-DOUBLE-BACK-IMPLEMENTATION-HANDOFF.md).
This page records the state of the delivery branch, including work that has
not yet been proved. Read the RCA for the stack failure and the handoff for
the navigation contract.

## Candidate

| Item | Value |
|---|---|
| Branch | `codex/android-double-back-20260925` |
| Base | Forgejo `main` at `60f3803d1d5dc431a919235fab328ae6ea86d394` |
| Worktree | Separate agent clone at `/private/tmp/plurx-agent-android-double-back-20260925` |
| Change | Bind Back and Exit callbacks to their originating navigation entry at 16 destinations; preserve Home and intermediate pages after repeated taps. |
| Lint repair | Opt in to Media3's existing `UnstableApi` usage in `PlaybackService.kt`. |
| PR | Pending |

## Milestones

| Milestone | State | Evidence or next action |
|---|---|---|
| Recover candidate on current main | Done | The Android navigation files had no upstream changes since the candidate base; the branch was rebased again after an unrelated streaming PR reached main. |
| Review scoped implementation | Prepared | Six instrumented regressions use the real navigation controller and Back button. Final adversarial review remains pending. |
| Validate final tree | Pending | Run the affected fast lane once after review and review fixes, as requested for this delivery. |
| Physical phone and Android TV | Pending | No physical device result is claimed. |
| Merge to main | Pending | Requires resolved review and green current-head fast lane. |

## How to read this page

"Prepared" means source is present in the branch, not that validation passed.
"Pending" is unexecuted work. A green build or CI run for an older commit
does not qualify a newer candidate. The previous candidate's emulator and JVM
results are historical evidence in the RCA, not validation of this branch.

## Scope

This fix changes Android navigation callbacks, adds their regressions, and
acknowledges an existing Media3 API use. It introduces no optional feature or
enable switch. It does not alter playback policy, server behavior, app
versions, release signing, or CI workflows.
