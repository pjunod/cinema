# Worker refinement — run two complete workers against one domain

**Status:** implementation built; current qualification and landing receipt:
[PR #1251](http://forge.lan:3000/noirr/plurx/pulls/1251). Before observation-state factoring, `progress.cfg` timed out at
300 seconds (678,415 distinct states; 51,612 queued) and again at 900 seconds
(4,162,006 distinct states; 151,968 queued). Neither run established a pass.
The factored model requires all fourteen configured expected outcomes.

This separate finite TLA+ package models two distinct library jobs, each with
its complete admission, claim, token mutex, publication lock, scan, item,
completion, heartbeat, retry, settlement and cleanup pipeline. Admission and
claim intentionally overapproximate production eligibility so both can
contend on one `scan:library:<id>` domain. The production same-root queue
gate instead refuses the second background library claim; the paired Rust
schedule covers that refusal and sequential completion. Domain overlap is
model-only evidence. Each job owns its own selected durable
request and waiter. Repeated demand and retries remain cyclic without an
event horizon, state constraint, `VIEW` or symmetry reduction.

Read [CONTRACT.md](CONTRACT.md) for exact properties and premises. The
source-owned `CORRESPONDENCE.md` and `correspondence.json` identify Rust
anchors, tested schedules and remaining gaps; they are maintained separately.
There is no proved state/action/fairness simulation from production to this
model, so model progress does not automatically transfer to Rust. The prior
[operation-progress](../operation-progress/README.md) and
[durable-jobs](../durable-jobs/README.md) executable packages are unchanged.

## Run the pinned package after final review

```bash
scripts/check-formal smoke --package worker-refinement --cache-dir target/formal-toolchain
scripts/check-formal full --package worker-refinement --cache-dir target/formal-toolchain
```

The shared toolchain pins TLC 1.7.4 and Temurin 21.0.12+8. `progress.cfg`
has a 900-second hard cap; each of the other thirteen configurations has a
300-second cap. The qualification heap remains 4 GiB. Smoke runs
`progress.cfg` and `witness-both-complete.cfg` under a 1,200-second aggregate
cap. Fourteen full configurations have a maximum combined cap of 80 minutes;
the CI preflight job has a 35-minute timeout.

This package uses TLC `-lncheck final`: all declared temporal properties
remain enabled and are checked after the reachable graph completes, rather
than repeatedly during exploration. One bounded retry follows the recorded
300-second timeout. These are resource ceilings and a scheduling choice,
not measured feasibility or a pass. A timeout or an incomplete final
liveness check fails qualification; both complete pipelines and their
properties remain mandatory.

**How to read results:** positives need complete exploration, zero queued
states and no errors. A temporal control needs its sole named property to
fail with exit 13 and a genuine lasso. A safety control or witness needs its
sole invariant to fail with exit 12 and a real finite trace. An unrelated
error, timeout, malformed receipt or source drift fails the run. Expected
violations characterize the selected model/control, not a production defect.

## Fourteen configurations have separate evidence jobs

All configurations contain both complete workers, one item milestone each,
retry limit 2, distinct queue jobs and the same exclusive domain. Fault budget
1 belongs only to `w1` and is never replenished; `w2` has zero faults. The
budget is therefore at most one across the entire repeated execution.

| Configuration | Intended result |
|---|---|
| `progress.cfg` | Recurrent global useful completion, cleanup and queue retirement under one finite fault |
| `contention.cfg` | Both workers' attempt cleanup and retirement under fault-free repeated contention |
| `control-lock-cycle.cfg` | `SelectedCleanup` fails after adding the renewal-to-token reverse edge |
| `control-renewal-livelock.cfg` | `SelectedCleanup` fails without `w1` scan-finish scheduling fairness |
| `control-external-stall.cfg` | `SelectedCleanup` fails without `w1` filesystem-return fairness |
| `control-lost-settlement.cfg` | `AttemptProgress` fails after non-applied settlement and disabled queue expiry |
| `control-no-useful-progress.cfg` | `UsefulProgress` fails without scan-finish fairness for either worker |
| `control-stale-commit.cfg` | `NoStaleCommit` fails on a pending request changed by an old detached completion |
| `witness-both-complete.cfg` | Both workers apply a durable request completion and reach cleanup |
| `witness-overlap-reacquire.cfg` | Model-only: `w2` acquires between `w1` release and reacquisition; both complete and clean up |
| `witness-detached-before-release.cfg` | Detached completion applies after local revocation and before durable release |
| `witness-stale-after-release.cfg` | A detached completion is rejected after durable invalidation; request stays pending |
| `witness-cancel.cfg` | One abstract cancellation action reaches cleanup; distinct signals are covered only by Rust schedules |
| `witness-reconcile.cfg` | Lost completion acknowledgement, authoritative non-pending reread and cleanup |

The `renewal-livelock` control name denotes heartbeat activity continuing while
`w1` scan-finish scheduling is unfair; the mutation changes only that fairness
premise.

The unused per-worker `tick` and `item` observation fields were removed
under the fair-execution projection argument in [CONTRACT.md](CONTRACT.md).
The item dispatch/application/return milestone, both complete pipelines,
all guards, fairness clauses, properties and fourteen configuration bytes
remain unchanged. This contingent design advice is not a review approval or
a runtime feasibility guarantee; the 900-second budget remains unchanged.

`TimeObserved(w)` now only makes that worker's backoff/expiry barrier
eligible. It generates an identical successor once the barrier is already
eligible. The pinned TLC checker counts generated successors, including
identical ones, so default deadlock checking remains vacuous and enabled.
The temporal properties carry the progress claims; neither clock activity
nor a self-loop is useful completion evidence.
