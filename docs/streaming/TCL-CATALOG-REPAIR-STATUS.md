# TCL catalog repair — implementation and evidence

**Status:** building · **Updated:** 2026-10-02

Companion to [the reviewed RCA](TCL-CANDIDATE-CATALOG-RCA-AND-FIX.md).
This page records implementation, decisions and evidence separately from physical
playback acceptance. Work uses an isolated clone and the current Forgejo main
(`955e1551e`) as its current base; initial base was `bfdc4930b`. The original checkout is untouched.

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
| Review and fast lane | pending | No tests claimed yet |
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
compile successfully; tests have not executed. Database triggers cover raw SQL
import writes and same-timestamp updates. Final dispatch binds generation and source/probe identity, and revalidates the
selected descriptor without another ladder enumeration. Protocol 8 carries this
strict worker contract; older ingress and sessions need draining on rollout.
Successor responses retain their selection catalog.

Compiler checks pass on the current implementation. Boundary, legacy, binding
and expired-budget regressions are written and have not run. Apple compilation
first hit sandboxed macro/simulator services; an unsandboxed compile is underway.
No review or test acceptance is claimed.

Main integration, 2026-10-02: preserve main's SQLite v90 and replicated v68 DV
request provenance migrations, then append planning generation as SQLite v91
and replicated v69. The combined source compiled before review. Android app
and test sources and both Apple platforms compiled without running tests.

The work is one batched repair PR. The local effort branch retains the original
integration base; no independently merged task PRs are being created. Main's
fast lane includes its fast Rust unit/SQLite lane. It runs only after the
adversarial review, alongside focused feature-enabled generation evidence.
Full cluster qualification and device deployment remain separate acceptance.
