# Full-suite parallel execution — preserve the universe, measure the host

**Status:** open — M3 implementation and focused controls in progress; no full
parallel run or adoption claimed · **Requested:** 2026-10-22 ·
**Owner:** Codex · **Branch:** `codex/full-pr-suite-m3`

Companion to the [canonical plan](FULL-SUITE-EVERY-PR-PLAN.md), especially
§6.4, and the [M2 setup ledger](FULL-SUITE-SETUP-PERFORMANCE-2026-10-21.md).
Work uses an independent clone. The user checkout is not edited or tested.

## 1. Start from the qualified M2 landing

The user authorized merging [PR #1258](http://forge.lan:3000/noirr/plurx/pulls/1258)
and proceeding with M3. It landed as
`9b427b07d79b438f4f9e9d4863d44b064f404142` into `effort/full-pr-suite`,
preserving qualified tree `cc4381fabf33d6316755d1a77661846d351d236f`
and all 21 regression references. Development gate 5762 and complete Rust
qualification 5765 passed on source `61801d85ae24bd79c336cbab0fc58e69590d88a5`.

The ordinary command took 36m23.741s: 6,971 native passes, zero failures,
50 preserved ignores, plus one actual doctest across seven selected targets.
All 36 binaries and every baseline identity were reconciled. Ten active
controls were added; none were removed. The 400 repair-cohort bodies,
20 writer bodies and both explicit serial cases also passed. M2's summary
SHA-256 is `a8eb0819aa560631b1c2a46578fca617d498fa6ca59db93e95a9e8eb49f55e94`.

| Native binary body | M1 seconds | M2 seconds |
|---|---:|---:|
| Daemon | 1,269.53 | 1,240.58 |
| Core | 971.32 | 497.32 |
| Ordinary Store contract | 413.60 | 416.40 |
| All 36 binaries | 2,677.98 | 2,177.79 |

Daemon is now 57% of native body time. Three whole-binary lanes would still
have a roughly 20m41s longest lane at those observed durations, before
overhead. That is a scheduling lead, not a parallel runtime prediction.
The complete diagnostic job improved only from 87m30s to 86m46s because
compile/static phases took longer. One full run per source does not establish
variance, a cause for those differences, or reliable every-PR latency.

## 2. Pilot the existing libtest process model first

`validation/rust_parallel_manifest.py` builds a source-bound deterministic
plan from completed compiler/listing discovery. The first pilot assigns the
daemon to lane 0, Core to lane 1 and all remaining binaries to lane 2.
Every empty binary, active case and ignore disposition remains owned.
Each binary keeps four native test threads, its original process lifetime
and ordinary stack. There are at most three concurrent binary processes.
No assertion retries or new ignores are allowed.

`validation/rust_parallel.py` reuses the existing bounded process observer,
hashes and native nested-output parser. Original exits, raw logs, outcomes
and process cleanup are retained. Missing, duplicate, wrong-binary,
wrong-feature, incomplete or unsafe work fails the aggregate. The producer
binds the plan to original discovery independently of worker receipts.
Cancellation stops new work and joins owned cleanup before releasing the
admission lease. Inherited leases keep cooperating pilots serialized while
native leaders remain alive; arbitrary detached grandchildren are not
thereby proved clean.

Doctests and the two explicit ignored real-FFmpeg regressions execute outside
fan-out. Their results remain separate from ordinary native passes. A fresh
legacy Cargo run supplies the comparison universe, including exact doctest
identities. Other features and platforms remain separate required cells.
Existing replicated Store shards and schema-lineage coverage are not replaced
by the ordinary SQLite/units pilot.

## 3. Compare nextest without accepting a smaller suite

The comparison pins official nextest 0.9.140. Nextest uses a process per test,
so process-local Source semaphores and endpoint registries cannot provide
cross-process admission. The initial configuration serializes each of the
daemon, Core and Store groups, with four global process slots. Finer groups
require their own isolation and capacity evidence. Retry count is zero and
leaks fail. Native child output is opaque to the outer result adapter.

A tiny fresh native workspace exposed an upstream plus-JSON incompatibility:
an active case beside an ignored case produced a premature suite footer,
duplicate suite start and no ignored outcome despite an exit of zero. The
strict adapter refused it. Those logs are retained; no parser fallback
accepts malformed JSON as a pass. A subsequent stable JUnit probe succeeded.
`validation/rust_nextest.py` therefore binds complete list discovery to
exact active XML outcomes, while ignored identities remain explicitly
unexecuted. A captured run UUID, fresh report, pinned tool/configuration,
original exit and qualified cleanup are all required. Doctests and explicit
ignored serial bodies remain owed separately.

Official Linux archive SHA-256:
`4ee9aaa0d0171a985a5d0eb735b87355894c1c455972e9674fb9fdbd1387c9a3`.
The macOS native probe used the official universal archive with SHA-256
`58e0a722f9444078fab447783f322acf15a2a771ba785b3fbbe8bacda31c3df9`.
Both published checksum files matched their immutable release asset digests.
These acquisition/probe observations do not qualify a Linux full-suite run.

## 4. Establish capacity before running the pilot

Runner services are not independent machines. `rust_host_admission.py`
requires a persistent shared Docker named-volume lock and verifies actual
CPU/memory/PID cgroup caps, non-voter eligibility and observed free memory.
The lock is never unlinked. An ACTIVE marker is persisted before work;
unexpected controller loss leaves admission fenced. Unqualified cleanup
adds quarantine. There is no automatic recovery: another pilot must wait
for independent teardown confirmation and explicit manual recovery.
The lock covers cooperating M3 jobs sharing one Docker
daemon, not other repositories, Android/general workflows or separate VMs.
Available memory is a point-in-time observation, not a reservation.

The manual `rust-parallel-pilot.yml` first records read-only current capacity.
No test execution is enabled by that observation. A named volume requires
runner allowlisting; a workflow option alone does not prove CPU limits.
Host budget and cleanup must be established before dispatching full fan-out.
Preserve Apple/Android capacity rather than inferring it from runner count.

## 5. Acceptance still owed

Record one fresh legacy execution and the nextest comparison on the same
candidate and compatible environment. Then qualify the selected runner with
three warm repetitions, one controlled cold build/run and one runner-loss
drill. Record queue/build/execution/critical path, runner-minutes, resource
bounds, source/binary/case identity, first outcomes and cleanup. Cold build
means a new owned target directory; it does not imply host cache flushing.

Every binary/case union and ignore disposition must match discovery, and all
doctest/serial obligations must match the fresh legacy reference. A runner
loss must produce incomplete/red evidence and qualified cleanup before
another pilot is admitted. Keep libtest if nextest cannot qualify or does
not improve the measured result. No automatic PR policy changes or final
effort promotion belong to this task.
