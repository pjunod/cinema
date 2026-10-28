# Operation progress — repeat selected worker attempts without a horizon

**Status:** independently approved; all nine configurations qualified. Delivery and the current main gate are recorded in the status page.

This separate TLA+/TLC package checks a finite abstraction of one bound
`library_scan` / `metadata_refresh` request: claim, domain lease, binding,
finite scanning, item insertion, durable request completion, domain release,
queue settlement, heartbeat retirement and physical admission release.
Renewal, retry, backoff and reacquisition can repeat indefinitely. There is
no event horizon, lifetime counter, symmetry, `VIEW` or state constraint.
The original [durable-jobs package](../durable-jobs/README.md) is unchanged.

Read [CONTRACT.md](CONTRACT.md) for properties, premises and abstraction
limits, and [WAIT-MAP.md](WAIT-MAP.md) for the source waits and unknowns.
The [implementation plan](../../docs/cluster/OPERATION-PROGRESS-IMPLEMENTATION.md)
owns the delivery scope; its
[status page](../../docs/cluster/OPERATION-PROGRESS-STATUS.md) owns review,
qualification and merge evidence. `mapping.json` pins model/config/source
hashes, expected errors and limits; an edited input invalidates its receipt.
Whole-file source hashes are intentional: any `Cargo.lock` dependency bump
requires a reviewed mapping refresh, even when unrelated to Tokio. Mapping
drift is not itself a model regression.

For this package, formal governed inputs are exactly the executable model,
registered configurations, mapped source files, `mapping.json`,
`scripts/check-formal`, `scripts/bootstrap-formal-toolchain` and the shared
`verification/durable-jobs/toolchain.json`. Receipts also show full repository
dirty/untracked paths, but package docs, tests and `validation/points.toml`
are not in this package's governed dirty subset. That subset is narrower
than the original pilot's; a clean governed subset does not certify those
other files.

## Run the separate package with the existing pinned toolchain

```bash
scripts/check-formal smoke --package operation-progress --cache-dir target/formal-toolchain
scripts/check-formal full --package operation-progress --cache-dir target/formal-toolchain
```

Smoke selects `progress.cfg` and `witness-success.cfg` without changing their
constants; its aggregate wall budget is 180 seconds. Each of the nine
configurations has a 120-second cap. Use the runner's `--help` for single
configuration selection. The existing durable-jobs pins supply TLC 1.7.4
(internal banner TLC2 2.19) and Temurin 21.0.12+8. Qualification uses a 4 GiB
Java heap and `-workers auto`, with durable logs, traces and receipt hashes.
The separate development calibration below explicitly used four workers.

**How to read results:** positives require complete exploration, no errors
and zero queued states. A control requires its sole named temporal property,
exit 13 and a real lasso; a witness requires its named invariant, exit 12
and a real path. A timeout, malformed receipt, source drift, checker crash
or unrelated error is failure. An expected violation is evidence about the
model mutation or omitted premise, never a production-defect claim.

## Nine configurations have distinct jobs

All configurations use one selected request, one selected full worker
pipeline, retry limit 2 and one domain. The contention configuration adds
one explicitly abstracted competing worker, whose claim/admission, domain
acquire, backend return, release application and backoff are separate steps.
It does not model a second complete request pipeline.

| Configuration | Fault budget / peer | Checked outcome |
|---|---|---|
| `progress.cfg` | 1 / absent | Atomic result/waiter update, admission until scan join, renewal lock retention, useful request completion, attempt retirement and cleanup |
| `contention.cfg` | 0 / present | Same invariants; attempt retirement and cleanup, without per-job completion |
| `control-lock-cycle.cfg` | 0 / absent | `AttemptCleanup` fails after adding a renewal wait on the token mutex |
| `control-renewal-livelock.cfg` | 0 / absent | `AttemptCleanup` fails when worker scan-finish scheduling is unfair while heartbeats can continue |
| `control-external-stall.cfg` | 0 / absent | `AttemptCleanup` fails without filesystem-return fairness |
| `control-lost-settlement.cfg` | 1 / absent | `AttemptProgress` fails after a non-applied settlement and removal of queue expiry |
| `witness-success.cfg` | 0 / absent | Reach durable request result, terminal waiter and cleanup |
| `witness-reconcile.cfg` | 1 / absent | Reach lost completion acknowledgement, authoritative non-pending reread and cleanup |
| `witness-cancel-cleanup.cfg` | 0 / absent | Reach cooperative cancellation and cleanup while durable request remains pending |

Deadlock checking stays on, but `Tick` is always enabled, so TLC global
deadlock detection cannot fire in any registered configuration.
The four temporal controls check the local operation's failure to progress
while other transitions remain possible. In particular the lock-cycle
control is a deliberate reverse-edge mutation: the examined source has no
renewal-to-token-mutex dependency.

## Development evidence is separate from qualification

One pre-review development calibration of `progress.cfg` used the pinned
runtime, `-Xmx4g`, four workers and a 60-second external timeout. It completed
in 2.778 seconds: 51,143 generated states, 13,514 distinct states, zero
queued, depth 122, exit 0 and measured child peak RSS 804,913,152 bytes.
Its exact model SHA-256 was
`ca5b8038dd78ccf5eea33b69060c3a03a7b35fb46414ae41bfe380133ae1cdb3`;
its config SHA-256 was
`f0d67cfeff477420a154232b1b42c6fcf7827154c4d9e70103c30f5d71380f44`.
The isolated development workspace retained
`target/verification/operation-progress/development/progress.log`,
`progress-calibration.json` and the TLC metadir. This is not a qualification
receipt. The final source then corrected the unreachable five-failed-release
fallback to drop the local lease without waiting for the durable TTL; that
final model was statically parsed, not recalibrated. The calibration hashes
above identify the earlier development input, not the final candidate.
No controls, witnesses, unit suites or full formal qualification were run
before final independent review. The first post-review `progress.cfg` run
must confirm TLC accepts the temporal `IF` expressions in `Fair`; SANY does
not check the liveness translator. Translation failure fails qualification
and would require a reviewed equivalent rewrite, not a success receipt.

## Limits belong to every result

This is a finite protocol argument under the premises in the contract,
not refinement, unbounded concurrency or whole-cluster deadlock analysis.
There is no per-job starvation-freedom claim under contention, no numeric
response-time bound, no Raft-liveness theorem and no lower-write rollback
promise after caller timeout. Daemon shutdown and arbitrary old-boot reply
adoption are excluded. Cancellation checks joining and resource cleanup;
it does not pretend that a pending durable request completed.
