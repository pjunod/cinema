# Worker refinement — run two complete workers against one domain

**Status:** implementation candidate; static parsing passed. Independent final
review and all model-checking qualification remain pending. No TLC result is
claimed by this document.

This separate finite TLA+ package models two distinct library jobs, each with
its complete admission, claim, token mutex, publication lock, scan, item,
completion, heartbeat, retry, settlement and cleanup pipeline. Both contend
on one `scan:library:<id>` domain. Each job owns its own selected durable
request and waiter. Repeated demand and retries remain cyclic without an
event horizon, state constraint, `VIEW` or symmetry reduction.

Read [CONTRACT.md](CONTRACT.md) for exact properties and premises. The
source-owned `CORRESPONDENCE.md` and `correspondence.json` identify Rust
anchors, tested schedules and remaining gaps; they are maintained separately.
This package does not establish a mathematical refinement of Rust. The prior
[operation-progress](../operation-progress/README.md) and
[durable-jobs](../durable-jobs/README.md) executable packages are unchanged.

## Run the pinned package after final review

```bash
scripts/check-formal smoke --package worker-refinement --cache-dir target/formal-toolchain
scripts/check-formal full --package worker-refinement --cache-dir target/formal-toolchain
```

The shared toolchain pins TLC 1.7.4 and Temurin 21.0.12+8. Each configuration
has a 300-second hard cap and the qualification heap is 4 GiB. Smoke runs
`progress.cfg` and `witness-both-complete.cfg` under a 600-second aggregate
cap. Fourteen full configurations have a maximum combined cap of 70 minutes.
These are resource ceilings, not measured feasibility claims. A timeout
fails qualification; both complete pipelines remain mandatory.

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
| `witness-overlap-reacquire.cfg` | `w2` acquires between `w1` release and reacquisition; both complete and clean up |
| `witness-detached-before-release.cfg` | Detached completion applies after local revocation and before durable release |
| `witness-stale-after-release.cfg` | A detached completion is rejected after durable invalidation; request stays pending |
| `witness-cancel.cfg` | One abstract cancellation action reaches cleanup; distinct signals are covered only by Rust schedules |
| `witness-reconcile.cfg` | Lost completion acknowledgement, authoritative non-pending reread and cleanup |

The `renewal-livelock` control name denotes heartbeat activity continuing while
`w1` scan-finish scheduling is unfair; the mutation changes only that fairness
premise.

`TimeObserved(w)` remains enabled and toggles only that worker's time marker
while making its backoff/expiry barrier eligible. Global deadlock detection
cannot establish operation progress in this model. The temporal controls
check their own named properties and therefore do not use unrelated clock
activity as evidence of useful progress.
