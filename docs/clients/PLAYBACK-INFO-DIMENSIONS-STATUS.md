# Playback information dimensions — implementation status

**Status:** PR ready; fast lane pending · **Started:** 2026-09-25 · **Current base:** `c99a29090` (refreshed from initial `415eb047f3`)

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
| History evidence | Passed locally | `make history-check`: 2,303 corrective commits, 281 client-fix anchors, eight post-boundary landing commits. One anchor per branch corrective client commit; the current main already carries immutable trailer errata for earlier PRs #519 and #522. |
| Fast lane | Pending | PR #526 is ready. Forgejo's WIP-prefix removal emitted an edit without a lane run; this status commit will synchronize the ready head and start the lane. Require current-head `Main promotion gate` success. |
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
`3e970ec42` onto `60f3803d1`, then merged current `main` at `c99a29090`;
the exact candidate tree awaits fast lane checks.
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
commit once. The permanent errata for PRs #519 and #522 arrived on `main` at
`c99a29090`; the merge kept those more complete records and removed the
branch duplicates. The audit passed before this base update; the fast lane
must validate the pushed candidate.

## Scope decisions

The user requested one larger PR, an adversarial review at the end, and a
single fast lane pass after review. This supersedes the handoff's separate
package PRs and per-package test timing. Normal commits still record each
reviewable package. No feature gate is planned: this repair changes the
information panel and does not enable a new playback route.
