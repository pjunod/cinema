# Playback information dimensions — implementation status

**Status:** built; promotion state is live on PR #526 · **Started:** 2026-09-25 · **Integrated main through:** `196d2a43e` (refreshed from initial `415eb047f3`)

This page tracks the playback information repair in one isolated clone. The
implementation follows the supplied dimensions and aspect handoff. The
candidate branch is `codex/playback-info-dimensions`; [PR #526](http://192.168.4.7:3000/noirr/plurx/pulls/526)
targets `main`. Package boundaries are commits in one PR under the user's
2026-09-25 workflow instruction.

## Progress

| Step | State | Evidence |
|---|---|---|
| Fresh base and toolchains | Done | Fresh Forgejo clone with current `main` merged; pinned Rust 1.97.1, Node 26.8.1, Python 3.14.7, Xcode 27.0, JDK 21.0.11. |
| Shared web, Apple and Android field contract | Local checks passed | `148829768`; fixture generation, web checks, and focused native tests passed. |
| Web delivery facts and presentation | Local checks passed | `148829768`; three focused Node suites and `scripts/player-input-fence` passed. |
| Apple delivery facts and presentation | Local checks passed | `148829768`; iOS and tvOS simulator builds passed; 83 selected tests passed on each. |
| Android delivery facts and presentation | Local checks passed | `148829768`; `:app:assembleDebug` and 35 selected unit tests passed. |
| Adversarial implementation review | Findings addressed | One review found Apple Compact label drift, Android aspect basis, unattached web plan facts, and stale handoff ledger. Follow-up code and tests passed their focused checks. |
| History evidence | Passed locally | `make history-check` on the current merged base found 2,303 corrective commits, 281 client-fix anchors, ten post-boundary landing commits. One anchor per branch corrective client commit; current main carries errata for earlier PRs #519 and #522. |
| Fast lane | [Live on PR #526](http://192.168.4.7:3000/noirr/plurx/pulls/526) | Run 3062 passed scope, mobile version, policy, web, Apple, Android and Windows. Rust stopped before compilation when the runner cache pruner could not meet its disk reserve. The runner's unused Docker build cache was reclaimed; use the PR's current-head `Main promotion gate` for the final result. |
| PR merge | [Live on PR #526](http://192.168.4.7:3000/noirr/plurx/pulls/526) | The PR is the authoritative merge record. Merge requires a successful current-head `Main promotion gate`. |
| Production and physical session evidence | Unavailable | No real Live TV Cozi session or physical phone/TV capture was available in this isolated checkout. The stream frame is planned on web/Apple and measured only from an eligible Android player sample. |

## How to read the states

`Local checks passed` means the focused regressions and affected client builds
passed in the isolated clone. The PR link shows the live gate and merge state.
A green fixture alone does not verify a real broadcast's output dimensions or
aspect.

The tracked pre-commit hook was installed in the isolated clone after its
initial absence. The first commit was amended through it; catalog lint, Rust
formatting, Clippy and embedded JavaScript syntax passed before rebasing
`3e970ec42` onto `60f3803d1`, then merged `main` at `c99a29090` and
`196d2a43e`;
the reviewed candidate passed the tracked hook on its final status commit.
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
suites passed on rerun.

The history audit initially found three branch anchor errors and two earlier
`main` merges whose messages lacked the regression trailers already present
in their PR descriptions. The branch anchors now map each corrective client
commit once. The permanent errata for PRs #519 and #522 arrived on `main` at
`c99a29090`; the merge kept those more complete records and removed the
branch duplicates. The audit passed again on the updated base.
The exact merged source also passed `cargo check --workspace --locked
--all-targets`, `cargo clippy --workspace --locked --all-targets -- -D
warnings`, and `cargo fmt --all -- --check` with pinned Rust 1.97.1 before
the new candidate push.

Run 3062 failed at `ci-cache-prune` on `gha-nynuc-general-01`, before Rust
compilation or unit tests. Its workspace had 37 GiB free against the 25 GiB
build minimum, but the cache pruner could not satisfy its separate reserve.
The host's Docker build cache had 17.45 GB unused; the documented operator
cleanup reclaimed it and left 64 GiB free on the host. No CI policy or
playback code was changed for this infrastructure failure. The next ready
head and any retry are reported on PR #526.

## Scope decisions

The user requested one larger PR, an adversarial review at the end, and a
consolidated fast lane after review. This supersedes the handoff's separate
package PRs and per-package test timing. Normal commits still record each
reviewable package. No feature gate is planned: this repair changes the
information panel and does not enable a new playback route.
