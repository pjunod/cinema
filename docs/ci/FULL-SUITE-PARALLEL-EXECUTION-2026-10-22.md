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

Source-only experiments use `validation/rust_source_bundle.py`. A clean
committed archive carries an external manifest of original Git blob/mode
identities and raw commit bytes. The isolated compiler reconstructs the
tree and commit hashes without receiving `.git` or repository credentials.
Unknown source files and changed bytes, modes or symlinks fail verification.
Archive transport checksums are separate from source identity; neither
asserts an authenticated CI result.

Doctests and the two explicit ignored real-FFmpeg regressions execute outside
fan-out. Their results remain separate from ordinary native passes. A fresh
legacy Cargo run supplies the comparison universe, including exact doctest
identities. Other features and platforms remain separate required cells.
Existing replicated Store shards and schema-lineage coverage are not replaced
by the ordinary SQLite/units pilot.

`validation/rust_parallel_rehearsal.py` holds one lease across compilation,
discovery and the six declared execution arms: cold parallel, fresh legacy,
three warm parallel repetitions and nextest. It requires a new private Cargo
target and separate artifact directory. Emptiness and target inode identity
are checked again after admission, so another waiting campaign cannot turn
a cold observation into warm reuse. Original native exits are retained;
independent arms can continue after an assertion failure only when cleanup
qualified. Cancellation and cleanup uncertainty block later arms.

Every phase has a resource sampler whose exact process must exit and whose
counter summary must be complete. The source, Cargo/configuration inputs,
compile environment, native executable bytes and stat identities, pre-run
manifest and source binding stay frozen across all arms. Doctests still
compile within their arms; native payload reuse does not prove that no
unobserved dependency compilation occurred. Results remain observations
with `adoption_ready: false` until the remaining acceptance is satisfied.

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

The agent-owned Linux experiment uses an isolated container on a non-voter
22-logical-CPU host with 30 GiB RAM. Its actual limits are 12 CPUs, 15 GiB
memory, no swap and 4,096 PIDs, with an 8 GiB observed host-memory reserve
required for admission. The container has the persistent admission volume,
no host Docker socket and no repository credentials. Its source-only
measurements remain distinct from Forgejo and promotion evidence.

The retained Linux image provides Rust 1.97.1, Python 3.11.2 and Jellyfin
FFmpeg 8.1.3. On source `ca55fd2c74597ab12e0d9bce22e2a1f917d234eb`, all
142 focused controller/accounting/source controls passed in 9.317 seconds,
including Linux process cancellation. An earlier Python 3.11 fixture
extraction incompatibility was fixed without changing the ten source-bundle
assertions. No application full-suite timing follows from those controls.

The manual `rust-parallel-pilot.yml` first records read-only current capacity.
No test execution is enabled by that observation. A named volume requires
runner allowlisting; a workflow option alone does not prove CPU limits.
Host budget and cleanup must be established before dispatching full fan-out.
Preserve Apple/Android capacity rather than inferring it from runner count.

### First application observations

The effort incorporated the qualified main snapshot as
`27b71d42a25201343b5a880ae8c2dcdf59395e1a`; M3 candidate
`94b5cebe9a0363818ff979b272baa1b77b21885f` includes that refresh.
Its Linux controller controls passed 171 tests in 9.404 seconds. Fresh
ordinary discovery found 36 binaries and 7,024 cases, with all 50 ignores
preserved. Every M2 case remains; three active cases were added by the
refresh. This is inventory retention, not execution evidence.

The first warm parallel attempt failed after 34.948 seconds because strict
source verification observed a live desktop-control fixture beneath the
source working directory. That fixture deliberately creates a temporary
replicated Store under its current directory. Another lane's source check
therefore correctly refused the changing source tree. Native processes were
canceled, their original exits retained and cleanup qualified. The attempt
is incomplete and supplies no full-suite runtime or speedup.

Direct native execution now needs a fresh private external working directory
per binary and explicit serial case, with protected real ancestors and no
source overlap. Cargo, doctests and nextest retain their existing directory
behavior. Exact case union alone does not establish equivalent execution
environments; a complete application run must qualify this directory change.
The source verifier remains strict, including refusal of unknown files.
The updated controller suite ran 180 controls in 13.366 seconds on macOS:
179 passed and one Linux-only cancellation control was skipped. The initial
sandboxed invocation could not inspect owned process groups and failed cleanup
controls; the retained successful invocation used normal process visibility.
Exact-candidate Linux controls and application execution remain owed.

A separate replicated Store discovery found 385 contract cases (three
ignored) and one active schema-lineage case. A focused repair contract then
failed during three-voter startup after 60.90 seconds, before its historical
repair-result assertion. Original failure and resource evidence are retained;
the exported child test-thread environment and readiness protocol are being
investigated. A standalone libtest reproduction confirmed that inherited
`RUST_TEST_THREADS=1` prefixes the child READY line, which the exact-line
protocol cannot recognize. Unsetting that environment variable while keeping
the parent's `--test-threads=1` matches Make's serialization behavior and
preserves a standalone child READY line. This does not prove the unrecorded
original children were ready. The corrected application probe was refused
before compile or native execution because observed free host memory did not
preserve admission's reserve. No test result follows from that refusal. These
observations neither complete the Store suite nor repair its historical
application failure.

The user declined the proposed six-hour exclusive host window. Shared CI
stays available; no runner services or optional runner configuration are
changed. Six hours was a conservative campaign allowance for reference,
cold/warm comparisons, nextest, replicated Store and cleanup checks, not a
measured parallel runtime or a proposed per-PR duration. The next useful
measurement is one complete parallel execution. Shared-host observations
cannot establish reserved capacity or reliable every-PR latency.

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
