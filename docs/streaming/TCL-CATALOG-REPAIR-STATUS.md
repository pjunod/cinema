# TCL catalog repair — implementation and evidence

**Status:** post-review validation · **Updated:** 2026-10-02

Companion to [the reviewed RCA](TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md).
This page records implementation, decisions and evidence separately from physical
playback acceptance. Work uses an isolated clone and the current Forgejo main
(`d4bf682b7`) as its current base; initial base was `bfdc4930b`. The original checkout is untouched.

## 1. Delivery — commits batched for one main review

The user superseded per-task test runs: build with compile, Clippy and formatting
checks; obtain one adversarial agent review when the main PR is ready; address
findings, then run the fast lane once. Rerun failed checks only, unless a code
change invalidates a prior passing result. No deployment or saved-switch change
is authorized by implementation. Integration uses `effort/tcl-catalog-repair`.

| Milestone | State | Evidence |
|---|---|---|
| Preserve RCA and source replay | done | Copied only this effort's three untracked files |
| Pinned Rust loop | done | Rust 1.97.1; current-main all-target compile passed |
| Typed diagnostics and accounting | done | Diagnostic commit retained before bound repair |
| One catalog per create | done | Canonical caps and recipe/worker identities retained |
| Android compaction and shared contract | done | No legacy blanket 400 |
| Snapshot, generation and budgets | implemented | Coherent statement, transactional generation, enclosing deadline |
| Partial selection through existing owners | implemented | Validated local Auto rows; typed incomplete discovery |
| Review and fast lane | post-review tests running | Five findings fixed; focused Android, Apple and generation evidence passed |
| Physical TCL/Streamer and cluster timing | outstanding | Exact TCL body/count not captured |

## 2. Decisions — evidence required before dependent changes

Bound, negotiated contract, startup/reserve numbers and generation scope are
being resolved from current code and fixtures. No placeholder is acceptance.
RF1 consistent-read latency remains a separate investigation.

## 3. Acceptance — planner evidence does not close device playback

Retain the diagnostic commit for a gated diagnostic deployment. Confirm actual
TCL and Streamer wire counts on unchanged clients; a TCL count at or below 16
falsifies B1 attribution. Deployment, cold/warm Auto first-frame and sustained
playback, Safari fallback and settled-cluster p95/p99 remain outstanding until
measured against recorded client/server revisions.

Diagnostic milestone: `3bc3567a8`, compiled and Clippy-clean with Rust 1.97.1.
Its 16-entry gate remains unchanged for the physical confirmation run.

Concrete implementation decisions, 2026-10-02:

1. Shared client bound is 64 rows; node capability limits remain 16. The review
   registry fixture models 60 components/profile rows and compacts to 20. Sixty-four
   permits additional crossing envelopes without assuming every registry compacts
   to 20. Parsing retains the internal 64 KiB bound and never truncates rows.
2. `compact-v1` is an additive capability contract advertised by server identity.
   Android advertises it only after observing that server support and fitting the
   bound. Existing v2 alone is legacy. Old servers ignore additive DeviceCaps
   fields; control and worker envelope parsers are separately strict.
3. Server create allowance is 10,000 ms from handler entry through response.
   Catalog retains its 2,000 ms maximum within that allowance. Worker budgets
   reserve 250 ms for outbound transit and 250 ms for return/serialization.
   These are conservative engineering allocations, not measured p95 claims.
   Apple has a 30 s request/resource limit; Android/web create retry owners have
   a 60 s total sequence. Android's 30 s presentation stall clock starts at its
   first attached-player observation; web's 40 s cold HLS episode starts at attach.
   Neither is an enclosing create clock. Existing owners retain retries and
   first-frame handling. A shorter incoming remaining-duration allowance wins.
4. Settings generation covers all `playback.*` and `transcode.*` setting writes,
   including insert/update/delete through import and migration SQL, using database
   triggers in the same transaction. Job/history settings do not invalidate plans.
   File/probe/source facts and generation/settings are read in one statement.
   Rate-control evidence and engine/source bindings remain required at dispatch.
5. Initial Auto may select fully validated local partial rows. Explicit identity
   cannot become absent on incomplete discovery; no remote partial substitution.
   The existing sustainable-quality owner handles later upgrades.

Snapshot enumeration compiles with Rust 1.97.1. Android app and JVM test sources
compile successfully; focused results are recorded below. Database triggers cover raw SQL
import writes and same-timestamp updates. Final dispatch binds generation and source/probe identity, and revalidates the
selected descriptor without another ladder enumeration. Protocol 8 carries this
strict worker contract; older ingress and sessions need draining on rollout.
Successor responses retain their selection catalog.

Compiler checks pass on the current implementation, including both Apple
platforms. Boundary, legacy, binding and expired-budget regressions await the
main fast Rust lane. Physical acceptance remains outstanding.

Main integration, 2026-10-02: preserve main's SQLite v90 and replicated v68 DV
request provenance migrations, then append planning generation as SQLite v91
and replicated v69. The combined source compiled before review. Android app
and test sources and both Apple platforms compiled without running tests.

The work is one batched repair PR. The local effort branch retains the original
integration base; no independently merged task PRs are being created. Main's
fast lane includes its fast Rust unit/SQLite lane. It runs only after the
adversarial review, alongside focused feature-enabled generation evidence.
Full cluster qualification and device deployment remain separate acceptance.

Review candidate: `56e52c50e` integrates typed outcomes and deadlines;
`853487d70` integrates current main and the retained-source regression. All
commits ran the tracked lint/format/Clippy/JavaScript hook. The decoder snapshot
regression now tests overflow at 65, matching the negotiated shared bound.

Batched review: [Forgejo PR #718](http://192.168.4.7:3000/noirr/plurx/pulls/718),
reviewed head `30369ca0c`; post-review head `182f7f5a7`. The PR is ready after
addressing the adversarial review. The desktop attachment tool does not accept this Forgejo URL, so the
PR and status links remain explicit here. No changes to the original checkout.

## 4. Adversarial review — candidate `30369ca0c`

The final agent review requested changes. Five actionable findings were fixed:

- Apple: add the new code to the actual retry owner, preserving its existing
  identity, backoff and deadline. Its existing fixture-driven retry test applies.
- Admission: move planning-binding validation before the shared VOD/live split,
  after request replay recovery, so local copy and encoded VOD both revalidate.
- Legacy required discovery: Auto and 1440 retain typed validation. Oversized
  legacy direct/copy/manual paths keep their ordinary admission.
- Preparation: keep the one request-local catalog on SessionRequest, independent
  of selected recipe context, so manual/copy successors replace stale ladders.
- Accounting: propagate the Store counter and remaining allowance into the
  owned local worker task; unrelated background reads remain unscoped.

Regressions cover the legacy required/optional distinction, owned worker versus
background accounting, manual/copy catalog retention, generation invalidation
through the common producer entry, and audio-dependent follow-up identities.
These compiled successfully; individual test results follow.

Review findings commit: `3b13d6acf`, tracked hook passed. Main advanced only with
schema expectation fixes; the combined assertions now deliberately cover
SQLite 91 and replicated 69 before the final test pass.

## 5. Post-review validation — preserve passing results

- Android: `:app:testDebugUnitTest --tests tv.plurx.app.data.CapsPolicyTest`
  ran 16 tests: 15 passed, including the 60-row compaction and crossing-envelope
  fixtures. One existing transfer-curve assertion compared order; its correction
  compares the unchanged set. Only
  `CapsPolicyTest.capsDocumentKeepsDisplayPresentationAndDecoderClaimsSeparate`
  was rerun, and passed. An earlier rerun before the edit still failed.
- Apple: `xcodebuild ... -only-testing:plurx-iOSTests/AppleClientTests/testOnlyTheContractsNotYetCodesAreRetried test`
  passed one test, zero failures. The temporary simulator was deleted.
- Generation: Rust 1.97.1 `cargo test --locked -p plurx-core --features hiqlite-store --lib store::hiqlite::tests::playback_generation_covers_tied_updates_deletes_import_and_rollback -- --exact`
  passed one test. An earlier unqualified exact filter ran zero tests and is not evidence.
- Main fast lane at `182f7f5a7`: scope and mobile versions passed; preflight
  stopped at corrective-history audit because three client commits needed
  source-to-regression anchors. Those rows are added. Compiler/test jobs were
  skipped, so the fast Rust unit lane has not yet run.

Corrective-history audit passed after the anchor fix. Main subsequently added
only the HLS request-cleanup timing regression adjustment; it is integrated
before the next fast-lane push. Previously passing focused tests are unaffected.

Before the next push, regenerated the surface embed and documentation from
the shared fixture (`node scripts/player-contract-table --embed` and `--write`).
The web values are unchanged; the generated formatting and documented retry
code now match the fixture. Current-base all-target Clippy passed.

At `1142de82b`, history and regression-field audits passed. Validation ran 253
tests: 252 passed; the module-wide ownership census test failed three count
subtests. Reviewed the exact source delta: the production worker spawn moves
into an accounting wrapper, a joined background-read fixture adds one task,
awaited deadline bounds account for the net two timer sites, and the legacy
admission fixture adds Response::status(), not a process. The inventory records
those owners. Reran only that failing test; it passed. Rust and platform jobs
were skipped, not executed. The workflow's automatic full-preflight rerun on
each push conflicts with failed-only reruns; user reconciliation is pending.
