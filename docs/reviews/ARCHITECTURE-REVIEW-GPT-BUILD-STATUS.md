# Architecture review GPT build — execution status

**Status:** PR #506 merged; node and Apple rollout in progress · **Updated:** 2026-09-25 04:42 UTC · **Main:** `44cdfccc7`

The [workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) remains the
canonical plan ledger. This page shows the assigned build as one operating
queue, with the next action and evidence location visible while implementation
and fleet work run in parallel. A `done` entry requires a merged change and
the acceptance evidence named by its plan.

| Lane | Current state | Next action | Evidence |
|---|---|---|---|
| D-02 · Android lifecycle | M1-M5 and M7-M9 integrated on [PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) through `5829810bc`; production and test Kotlin compilation passed. Notification Pause while already owner-paused needs Pixel validation; code merged at `44cdfccc7` | §5.6 physical device checks | Plan execution log and workboard row |
| D-03 · Android credentials and release | M1 grant server/revocation and M3 API prep integrated, with a typed grant-store request for pinned Rust Clippy on PR #506; [draft mobile role PR #23](https://github.com/pjunod/ansible/pull/23) uses signed release artifacts. Four attached Android devices still run debuggable version 124; no signing identity found; code merged at `44cdfccc7` | Finish external chooser if automatic approval permits, then sign and deploy release when identity is available; collect device acceptance | Plan execution log, workboard row and dated fleet evidence |
| A-02 · Apple controller | 5.1-5.6 code integrated on PR #506 through `fe204f789`; combined iOS/tvOS compilation and promotion gate passed; code merged at `44cdfccc7` | Physical remote-control acceptance | Plan execution log and workboard row |
| A-03 · Native library paging | 5.2-5.5 code integrated into PR #506 at `34e4f4364`; Apple iOS/tvOS and Android app/test-source compilation and promotion gate passed; code merged at `44cdfccc7` | Apple TV/Lenovo evidence | Plan execution log and workboard row |
| W-02 · Web player decomposition | 5.4 `8907b9cf5` and 5.5 `cb7042ea2`/`858c8bc01` splits on PR #506; playback-lab startup fix `2a6fbc855`; JS syntax and promotion gate green; code merged at `44cdfccc7` | Type baseline and browser acceptance remain open under their assigned owners | Plan execution log and workboard row |
| L-03 · Shared Live TV transport | M2 capacity offers and M4 per-build caption proof integrated on [PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) at `6710d15f4`; prompt A found CC1/SERVICE1 dialogue on three broadcasts and prompt C Chrome found an English hidden track without drawn text; code merged at `44cdfccc7` | Collect post-advertising native/device caption checks; caption master playback fix `b20982f60` committed | Plan execution log, workboard and dated fleet evidence |
| Developer adaptive Auto enablement | Existing `playback_auto_abr` switch moved to Settings → Developer on PR #506 at `4c82aeca0`; controller and dated trace/device evidence are advisory only; code merged at `44cdfccc7` | Refresh dated rows when traces change | F-1 in native adaptive quality build plan and A-05 workboard row |
| A-04 · Adaptive quality traces | Exact-main Chrome 8→1.5 Mb/s trace failed recovery: 24.974 s, one restart/downshift and 1.883 s maximum video gap. Earlier two-cliff Chrome attempts failed before first frame. An exact-candidate run reached the first 8→1.1 Mb/s cliff and failed recovery at 20.9 s with an 8.10 s transition gap; the 350 kb/s stage did not apply. Firefox could not create a profile; Safari automation unavailable. Apple TV asleep and iPhone/iPad locked refused native trace launch | Complete platform matrix when browser automation and devices are available; retain failed traces as baseline evidence | Raw and normalized reports in `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/`; design §5.3 and dated fleet evidence |
| Main deployment and fleet evidence | All four nodes and six Apple devices received former main `f600d2823`. At 03:28 UTC, `nynuc` and `nuc3` had advanced to new main `dafadf043`, while `m6` and `nuc4` still reported `f600d2823`; the first-hour collector has a 337 s gap and two `nuc3` connection refusals. Merged main `44cdfccc7` is rolling to the four nodes and reachable Apple devices now | Collect required device and long-window acceptance; Android release-signing inputs remain absent | [Dated fleet evidence](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md), [fleet/client baselines](ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md), [fleet readout](ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md) and deployment logs |

## Sole adversarial review and fast lane

The one adversarial review of PR #506 examined `e0164d784`. Its caption-master,
Android text-track and account-isolation, web capacity-offer, and Apple paging
findings were addressed in the same PR. D-03's external-reader bearer handoff
remains open under automatic approval rejection; no second review will be
requested for this PR.

Fast-lane #2933 stopped in history preflight. Ledger commits `19ecaa6`,
`2f2ff9d67`, and `fadbe098e` made focused `make history-check` green.
Fast-lane #2939 passed history but stopped on rolling-producer inventory
counts; `719ec563d` records the new owners. Local validation ran 242 tests
with one skip and passed, and all 63 Live TV web cases passed. API/build claim,
Apple attempt census/remote adapter, and moved evidence-workflow contracts were
corrected at `cf5a74a61`, `9301cae1c`, and `39e40d10d`, with their focused
checks green. New main `dafadf043` was merged into the PR at `47338f39d`;
its one conflict retained both client-anchor rows. The current-main merged tree passed pinned Rust check and Clippy, iOS/tvOS
application build, Android app/test-source compilation, and 95 focused
operations and ownership tests. Its final `make history-check` passed before fast-lane #2944. That gate passed all 505 Linux operations checks, then stopped on a web policy source assertion that still looked inside `play()` after the 5.5 split. Commit `9bf0425f9` follows the shipped `play()` → `presentPlayerChrome()` wiring; the focused web policy suite passed locally. Fast-lane #2951 passed policy, Apple, Android, Windows, and web jobs. Its Rust lane passed compilation and Clippy, then reported five file-grant migration/source inventory drifts and two unchanged timing-sensitive tests. `0a774e5ec` updates the schema, all named inventories and rollback fixtures, including the missing v45→v46 migration admission; six focused pinned Rust tests and formatting passed. `dfac3e62e` removes trailing Kotlin whitespace. The two timing-sensitive tests predate this PR and are assigned to the separate unit-failure batch. Fast-lane #2956 passed all policy, Rust, Windows, web, Apple, Android and aggregate promotion jobs on head `264f2a7d1` and base `dafadf043`. Both timing-sensitive Rust tests passed on that run. PR #506 merged as `44cdfccc7`; node and Apple deployment is in progress.

## Current impediments

- The named handoff `claude/architecture-review-gpt-build-handoff-2026-09-24.md`
  is absent from the supplied checkout, the fresh Forgejo `main` clone and
  `~/code/plurx-agent`. Its location has been requested so its ownership and
  access rules can be applied; assigned work proceeds using the available plans.
- The latest user instruction controls batching: proper commits on one consolidated
  draft PR, exactly one adversarial review at merge readiness, then the fast
  lane. This differs from the repository's per-task focused-regression and
  task-PR convention; compile checks continue before integration.
- Automatic approval review rejected editing either D-03 external-reader
  call site because it would hand a private book capability URL to an
  unspecified app. Explicit authorization was requested; both call sites stay
  unchanged. Release-signing values are unset on the Mac and all four nodes;
  only debug keystores were found, so no signed Android release can be made.
- The seven-day read-only fleet collector restarted after a 337 s gap. The first
  hour also spans a main deployment and two connection refusals on `nuc3`;
  preserve those discontinuities when assessing the long windows.

## Update rule

Update this page when a lane changes state, with the exact branch, PR, commit,
test or device observation that caused the change. Update its workboard row in
the same implementation or evidence PR. Record an unrun observation as owed;
never infer a pass from a build claim, an empty log window or an unavailable
device.
