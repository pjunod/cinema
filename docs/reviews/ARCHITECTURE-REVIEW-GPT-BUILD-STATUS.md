# Architecture review GPT build — execution status

**Status:** reviewed head compiled; fast lane pending · **Updated:** 2026-09-25 03:00 UTC · **Base:** `f600d2823`

The [workboard](ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) remains the
canonical plan ledger. This page shows the assigned build as one operating
queue, with the next action and evidence location visible while implementation
and fleet work run in parallel. A `done` entry requires a merged change and
the acceptance evidence named by its plan.

| Lane | Current state | Next action | Evidence |
|---|---|---|---|
| D-02 · Android lifecycle | M1-M5 and M7-M9 integrated on [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) through `5829810bc`; production and test Kotlin compilation passed. Notification Pause while already owner-paused needs Pixel validation | Address sole review findings, then fast lane and §5.6 device checks | Plan execution log and workboard row |
| D-03 · Android credentials and release | M1 grant server/revocation and M3 API prep integrated, with a typed grant-store request for pinned Rust Clippy on draft #506; [draft mobile role PR #23](https://github.com/pjunod/ansible/pull/23) uses signed release artifacts. Four attached Android devices still run debuggable version 124; no signing identity found | Finish external chooser if automatic approval permits, then sign and deploy release when identity is available; collect device acceptance | Plan execution log, workboard row and dated fleet evidence |
| A-02 · Apple controller | 5.1-5.6 code integrated on draft #506 through `fe204f789`; combined iOS/tvOS compilation passed | Address sole review findings, then fast lane; physical remote-control acceptance remains | Plan execution log and workboard row |
| A-03 · Native library paging | 5.2-5.5 code integrated into draft #506 at `34e4f4364`; Apple iOS/tvOS and Android app/test-source compilation passed | Address sole review findings, then fast lane; Apple TV/Lenovo evidence | Plan execution log and workboard row |
| W-02 · Web player decomposition | 5.4 `8907b9cf5` and 5.5 `cb7042ea2`/`858c8bc01` splits on draft #506; playback-lab startup fix `2a6fbc855`; JS syntax green | Finish type baseline/browser acceptance, then fast lane after review fixes | Plan execution log and workboard row |
| L-03 · Shared Live TV transport | M2 capacity offers and M4 per-build caption proof integrated on [draft PR #506](http://192.168.4.7:3000/noirr/plurx/pulls/506) at `6710d15f4`; prompt A found CC1/SERVICE1 dialogue on three broadcasts and prompt C Chrome found an English hidden track without drawn text | Collect post-advertising native/device caption checks; caption master playback fix `b20982f60` committed | Plan execution log, workboard and dated fleet evidence |
| Developer adaptive Auto enablement | Existing `playback_auto_abr` switch moved to Settings → Developer on draft #506 at `4c82aeca0`; controller and dated trace/device evidence are advisory only | Refresh dated rows when traces change; fast lane after review fixes | F-1 in native adaptive quality build plan and A-05 workboard row |
| A-04 · Adaptive quality traces | Exact-main Chrome 8→1.5 Mb/s trace failed recovery: 24.974 s, one restart/downshift and 1.883 s maximum video gap. Two-cliff Chrome attempts failed before first frame. Firefox could not create a profile; Safari automation unavailable. Apple TV asleep and iPhone/iPad locked refused native trace launch | Complete platform matrix when browser automation and devices are available; retain failed traces as baseline evidence | Raw and normalized reports in `/Users/pjunod/code/plurx-agent/codex-a04-evidence-20260925/`; design §5.3 and dated fleet evidence |
| Main deployment and fleet evidence | All four nodes run exact `f600d28230222005441cfc62301c306785c852ce`, healthy with `/readyz` 200; six physical Apple devices received that main build, with clean install exit 0; 42 owed rows updated; selected C-08/K-02/S-11 long-window collection started at 02:28:44 UTC | Collect required device and long-window acceptance; Android release-signing inputs remain absent | [Dated fleet evidence](ARCHITECTURE-REVIEW-FLEET-EVIDENCE-2026-09-24.md), [fleet/client baselines](ARCHITECTURE-REVIEW-FLEET-CLIENT-BASELINES-2026-09-25.md), [fleet readout](ARCHITECTURE-REVIEW-FLEET-READOUT-2026-09-25.md) and deployment logs |

## Sole adversarial review

The one review of draft PR #506 was performed against `e0164d784`. It found three
cross-client caption-master bypasses, Android text-track suppression, Android
pager account isolation, web capacity-offer loss, an Apple paging race, and the
pre-existing D-03 bearer handoff. Web playback and capacity fixes are committed
at `b20982f60`; Apple fixes are integrated at `dafb4bb5e` and `12331fb35`;
resume now returns the caption master at `676b62393`; Android fixes are
integrated at `dc26817db` and `22e9bf667`. The combined `bc5fee5d2` head passed pinned Rust check, pinned Clippy with
`-D warnings`, iOS/tvOS simulator build, Android app/test-source compilation,
Rust formatting, JavaScript syntax, and a clean merge-tree against current
`main`. The fast lane has not run. The D-03 milestone remains open
until the external-reader call sites use scoped grants. No second adversarial
review will be requested for this PR. The fast lane has not run.

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
- The seven-day read-only fleet collector restarted after an observed gap near
  02:44–02:50 UTC. Preserve that gap when assessing long-window evidence.

## Update rule

Update this page when a lane changes state, with the exact branch, PR, commit,
test or device observation that caused the change. Update its workboard row in
the same implementation or evidence PR. Record an unrun observation as owed;
never infer a pass from a build claim, an empty log window or an unavailable
device.
