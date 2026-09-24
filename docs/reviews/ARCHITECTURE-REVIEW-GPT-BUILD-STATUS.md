# Architecture review GPT build — execution status

**Status:** open · **Updated:** 2026-09-24 23:39 UTC · **Base:** `f600d2823`

The [workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) remains the
canonical plan ledger. This page shows the assigned build as one operating
queue, with the next action and evidence location visible while implementation
and fleet work run in parallel. A `done` entry requires a merged change and
the acceptance evidence named by its plan.

| Lane | Current state | Next action | Evidence |
|---|---|---|---|
| D-02 · Android lifecycle | M7 builder commit `78830ec99` on [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506); Kotlin compile green; M6 on main | Finish M1-M5 and M8-M9; review and unit lane when ready | Plan execution log and workboard row |
| D-03 · Android credentials and release | Reconnaissance; M2, M7 and M8 repo half are on main | Finish capability, release and device milestones | Plan execution log and workboard row |
| A-02 · Apple controller | Seek-fence commit `da5c6050a` on [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506); iOS/tvOS compile green | Continue 5.2-5.6; run review and fast lane when ready | Plan execution log and workboard row |
| A-03 · Native library paging | Reconnaissance; 5.1 is on main | Finish 5.2-5.5 | Plan execution log and workboard row |
| W-02 · Web player decomposition | 5.4-5.5 unstarted; current main's browser play fails before first frame with `WATCH_CLOSE_PROMISE` undefined | Diagnose the runtime failure, then split `attachHls` and `play()` after the type baseline | Plan execution log and workboard row |
| L-03 · Shared Live TV transport | Reconnaissance; #482 is on main | Finish M2 and M4 | Plan execution log and workboard row |
| A-04 · Adaptive quality traces | Chrome 8→1.5 Mb/s run failed before first frame on current main; Safari WebDriver timed out; Firefox absent | Repair the Chrome runtime failure, then repeat and complete the two-profile matrix | Raw reports under `/private/tmp/plurx-a04-d3-evidence-2026-09-24/` |
| Main deployment and fleet evidence | All four nodes running `f600d2823`; `nuc3` health and marker recheck in progress after a concurrent Compose deployment | Finish exact-image/readiness closeout, then collect bounded and long-window observations | Evidence-only docs PR and workboard rows |

## Current impediments

- The named handoff `claude/architecture-review-gpt-build-handoff-2026-09-24.md`
  is absent from the supplied checkout, the fresh Forgejo `main` clone and
  `~/code/plurx-agent`. Its location has been requested so its ownership and
  access rules can be applied before implementation.
- The user has been asked to resolve the test-timing and PR-boundary conflicts
  between the latest instructions and `AGENTS.md` / the workboard. Read-only
  reconnaissance continues while those answers are pending.

## Update rule

Update this page when a lane changes state, with the exact branch, PR, commit,
test or device observation that caused the change. Update its workboard row in
the same implementation or evidence PR. Record an unrun observation as owed;
never infer a pass from a build claim, an empty log window or an unavailable
device.
