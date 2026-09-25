# Playback information dimensions — implementation status

**Status:** local verification passed; fast lane pending · **Started:** 2026-09-25 · **Current base:** `60f3803d1` (refreshed from initial `415eb047f3`)

This page tracks the playback information repair in one isolated clone. The
implementation follows the supplied dimensions and aspect handoff. The
candidate branch is `codex/playback-info-dimensions`; [PR #526](http://192.168.4.7:3000/noirr/plurx/pulls/526)
targets `main`. Package boundaries are commits in one PR under the user's
2026-09-25 workflow instruction.

## Progress

| Step | State | Evidence |
|---|---|---|
| Fresh base and toolchains | Done | Fresh Forgejo clone at the base above; Node 26.8.1, Python 3.14.7, Xcode 27.0, JDK 21.0.11. |
| Shared web, Apple and Android field contract | Local checks passed | `148829768`; fixture generation, web checks, and focused native tests passed. |
| Web delivery facts and presentation | Local checks passed | `148829768`; three focused Node suites and `scripts/player-input-fence` passed. |
| Apple delivery facts and presentation | Local checks passed | `148829768`; iOS and tvOS simulator builds passed; 83 selected tests passed on each. |
| Android delivery facts and presentation | Local checks passed | `148829768`; `:app:assembleDebug` and 35 selected unit tests passed. |
| Adversarial implementation review | Findings addressed | One review found Apple Compact label drift, Android aspect basis, unattached web plan facts, and stale handoff ledger. Follow-up code and tests passed their focused checks. |
| History evidence | Passed locally | `make history-check`: 2,303 corrective commits, 281 client-fix anchors, eight post-boundary landing commits. One anchor per branch corrective client commit; immutable trailer omissions for earlier main PRs #519 and #522 recorded in `validation/merge-errata.toml`. |
| Fast lane | Pending | Mark PR #526 ready after pushing the final candidate; require current-head `Main promotion gate` success. |
| PR merge | Pending | Merge only after the current-head `Main promotion gate` passes. |
| Production and physical session evidence | Unavailable | No real Live TV Cozi session or physical phone/TV capture was available in this isolated checkout. The stream frame is planned on web/Apple and measured only from an eligible Android player sample. |

## How to read the states

`Local checks passed` means the focused regressions and affected client builds
passed in the isolated clone. `Pending` means no completion evidence exists.
A green fixture alone does not verify a real broadcast's output dimensions or
aspect.

The tracked pre-commit hook was installed in the isolated clone after its
initial absence. The first commit was amended through it; catalog lint, Rust
formatting, Clippy and embedded JavaScript syntax passed before rebasing
`3e970ec42` onto `60f3803d1`; the exact rebased tree awaits fast lane checks.
The post-review local pass included
`node tests/web/player-dom.test.js`, `node tests/playback/web-policy.test.js`,
`node tests/web/live-tv.test.js`, `scripts/player-input-fence`,
`make validation-lint`, the docs index unittest, Android `:app:assembleDebug`,
focused Android `:app:testDebugUnitTest`, iOS/tvOS simulator builds, and the
focused iOS/tvOS Xcode test selections (83 passed each). The initial Android
run exposed a test assumption about Media3's deprecated rotation constructor;
the test now checks the production eligibility helper. The initial iOS run
exposed an obsolete `observeFailure` call in a pre-existing test; it now
exercises the controller's current notification-error handler. Both affected
suites passed on rerun. The tracked hook passed on the reviewed commits; the
remaining fixes and status update still need a final hooked commit.

The history audit initially found three branch anchor errors and two earlier
`main` merges whose messages lacked the regression trailers already present
in their PR descriptions. The branch anchors now map each corrective client
commit once. The permanent errata names PRs #519 and #522, their landing
commits, and tests verified in those landing trees. The audit passed on this
corrected worktree; the fast lane must still validate the pushed candidate.

## Scope decisions

The user requested one larger PR, an adversarial review at the end, and a
single fast lane pass after review. This supersedes the handoff's separate
package PRs and per-package test timing. Normal commits still record each
reviewable package. No feature gate is planned: this repair changes the
information panel and does not enable a new playback route.
