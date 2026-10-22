# Full-suite M0 baseline — 2026-10-20

**Status:** measured — diagnostic attempts recorded; complete-suite qualification remains open

The first measured candidate is `86f1385005b27122e8a8d3a3d5bd9501eb2fdb16`
(tree `31501aa3d54a02e3bbb8288ba6104cceecd5f59c`), based on
`c1c344132463f0f4b7e9b98be82d23cad14aea24`. Work lives in an independent
clone on `codex/full-pr-suite-m0`; the user checkout is not used for edits or
tests. This report implements the measurement part of
[FULL-SUITE-EVERY-PR-PLAN.md](FULL-SUITE-EVERY-PR-PLAN.md), not its final
fresh-execution gate or a release qualification.

## The observations and their binding

| Observation | Forgejo display run / internal ID | Tested source | Workflow transport |
|---|---|---|---|
| Primary full diagnostic | #5620 / 5649 | `86f13850` | `86f13850` |
| Supplemental Python/Node continuation | #5624 / 5653 | `86f13850` | `bb3db2fa` |
| Disposable privileged continuation | #5625 / 5654 | `86f13850` | `bb3db2fa` |
| Privileged capability retry | #5629 / 5658 | `86f13850` | `7dc1869e` |
| Fresh JVM outcome capture repeat | #5627 / 5656 | `86f13850` | `15b22f36` |
| Seek/Cinema continuation | #5631 / 5660 | `86f13850` | `5721dd26` |
| Apple unit continuation | #5634 / 5663 | `86f13850` | `9968a63d` |
| Store schema-lineage continuation | #5636 / 5665 | `86f13850` | `9729f48c` |
| Ordinary Rust continuation | #5638 / 5667 | `86f13850` | `879bd652` |

The primary attempt ran from 15:35:13 until its final job stopped at
17:41:50 UTC: **2h06m37s**. Forgejo marks the run stopped on its first
failed job; this duration therefore uses the last terminal job, not the run
row's earlier failure timestamp. The separate continuations retain their
own scheduling, setup and cache state. Their durations are not added together
as though they were a single uninterrupted critical path. No test source is
patched during execution, retained test results are refused by the primary
diagnostic controls, and publication/badges are disabled.

The first API dispatch was rejected before a run existed: Forgejo requires a
literal `continue-on-error` boolean. The corrected controls retain strict
ordinary-run verdicts and final diagnostic failure verdicts. The reusable
supplemental children were skipped by the scheduler, so the standalone
continuation checks out the original source explicitly. Run #5622 stopped
before testing because the immutable supplemental image contains Python
3.11.2, rather than the initially assumed 3.12. These are recorded harness
attempts, not test assertion failures.

## Commands and timing boundaries

The ordinary Rust measurement uses the existing coverage-off command:

```sh
cargo test --workspace --locked --exclude plurx-cluster-check --no-fail-fast
```

Discovery first compiles that same cell with `--no-run --message-format=json`,
then lists the exact executables. Discovery and execution share a target
directory. Native execution reports a 0.44 s Cargo finish with no `Compiling`
lines, establishing reuse after the intervening static checks in this run.
The two ignored real-FFmpeg restart regressions run afterward with their exact
filters, `--ignored --exact --test-threads=1`.

Store uses `make cluster-store-check`: `plurx-core` with
`cluster-read-cost-validation,hiqlite-contract-tests`, an 8 MiB Rust test-thread
stack and `--test-threads=1`. Its first binary is `store_contract`, followed
by `schema_lineage_upgrade`. No LLVM coverage instrumentation was added to
these commands. Native body, command wall and whole-job wall measurements
answer different questions and are kept separate below.

## Results recorded so far

| Cell | Observed execution | Test execution time | Job wall time |
|---|---|---:|---:|
| Python operations | 1,357 passed; zero skips/errors | 124.35 s | Included in preflight |
| Python validation | 538 passed; zero skips/errors | 31.27 s | Included in preflight |
| Policy/contract preflight | Passed; history scan alone 148 s | 155.62 s Python bodies | 5m19s |
| Node software units | All 70 explicit command identities passed | 101.08 s | 1m55s |
| Supplemental Python tooling | 188 discovered, 187 method passes, one fixture configuration error; see below | Per-method artifacts retained | 15 s |
| Android JVM + lint, first observation | Job passed; XML was not retained and test-result caching was permitted | Unqualified until separately observed | 6m56s |
| Android device | Native summary `OK (85 tests)`; four conditional methods unresolved in raw status | Native runner reports 190.096 s | 17m33s |
| Replicated Store contracts | 371 passed, one failed, three ignored; subsequent schema-lineage binary not reached | 4,536.02 s bodies; 338 s build | 81m26s |
| Daemon activation/activity | 16 passed; one readiness failure, all 17 method entries completed | 810.50 s | 22m27s |
| Browser/layout | 222 structural-golden fact failures across 104 captures; seek/Cinema bodies missing | UI capture 687 s | 25m03s |
| VOD browser | All three native scenarios passed; wrapper cleanup unqualified | 350.00 s command wall including setup/cleanup | 11m21s |
| amd64 package/smoke | Failed downloading Dolby build input: temporary DNS failure | Not a unit-test duration | 25m11s |
| Ordinary Rust continuation | 6,970 exact cases independently reconciled: 6,896 passed, 25 failed, 49 ignored; original parser qualification remains false | 4,577.60 s native bodies; 4,585.024 s command; 389.440 s compile; 228.651 s static checks | Separate 87m47s job |
| Exact ignored FFmpeg regressions | VFR and bitmap-burn each pass one exact native body; cleanup and binding qualified | 3.17 s / 3.22 s bodies; 4.009 s / 3.667 s commands | Included in Rust continuation |
| Ordinary Rust, original attempt | Discovery reported 6,970 cases; exact lists/resource artifacts lost, no preserved completed unit outcomes | Discovery compile/list phase 101m04s; following combined phase 15m49s | 120m deadline + 64 s failed finalization |
| Rust compile gate | Passed; no unit execution in this lane | Not a unit-test duration | 19m22s |
| Apple | Desktop commands zero; iPhone exit65, iPad/tvOS zero; TV navigation hit job cap; native per-case artifacts unavailable | Mobile command phase 26m59s; no method counts retained | 60m01s, incomplete |
| Apple tvOS continuation | All 1,166 eligible methods passed; zero skips | 109.003 s native bundle; 743.169 s command including diagnostic timeout | Separate 12m48s job |
| Apple iPhone continuation | All 1,180 identities observed: 1,179 passed, one failed, zero skips | 104.948 s native bundle; 768.217 s command including diagnostic timeout | Separate 13m52s job |
| Apple iPad continuation | 1,179 passed, one runtime-capability skip; all 1,180 eligible identities observed | 104.486 s native bundle time | Separate 4m15s job |
| Apple desktop continuation | 75 passed across two XCTest bundles; exact native method set matches source | 1.779 s summed bundle times | Separate 41 s job |
| Schema-lineage continuation | Exact single native method passed; complete inventory and cleanup receipts | 123.36 s body; 85.381 s compile command | Separate 4m19s job |
| Fresh Android JVM repeat | 1,171 passed; zero skips/errors, 186 XML reports | 19.462 s summed case time | 1m05s |
| ARM64 package/smoke | Passed; a build/package obligation | Not a unit-test duration | 14m28s |
| Topology contracts/campaigns | 27 libtest pass invocations, one explicit benchmark ignore; separate campaigns passed | 11.96 s libtest + 25.80 s semantic + 357.44 s drills | 14m30s |
| Transport contracts | 77 libtest pass invocations | 0.80 s | 2m30s |
| WAL contracts | 265 passing invocation rows over 253 libtest runs | 23.55 s bodies; 1,127.89 s Cargo build/check reports | 19m41s |
| Voter recovery campaign | All 20 cycles passed | 198.348 s native campaign; 292 s build | 8m19s |
| Learner recovery campaign | All 20 cycles passed | 168.967 s native campaign; 290 s build | 7m47s |
| Recovery evidence validator | Current-run evidence accepted; no new role-cycle execution | 282 s build | 4m53s |
| Seek browser continuation | 1 passed; complete native method receipt | 4.56 s | Separate continuation |
| Cinema continuation | 17 passed, one live-fixture skip; original exit zero, wrapper cleanup unresolved | 9.20 s native scenarios | Separate continuation |

The Apple continuation uses an external, Git-blob-bound reviewed process
observer while preserving the frozen checkout. Desktop's 75 native identities
match all source methods across its UI (14) and core (61) bundles. iPad's
1,180 eligible identities all have native outcomes: 1,179 pass and one skips
because that simulator supports Picture in Picture, so the unsupported-PiP
regression body cannot run there. Twenty-two tvOS-only methods are compile
excluded from this destination; 44 UI methods belong to separate schemes,
not this unit command. Its 4m15s job includes 67.334 s simulator setup and
171.05 s combined native build/unit command wall; the XCTest bundle reports
104.486 s of bodies. The owned simulator shutdown/delete both succeed and
the tested source remains clean. macOS CPU/RSS/I/O counters are unavailable;
the frozen finish helper's later generic missing-state marker must not be
mistaken for a failed telemetry start when early artifacts explicitly say
that the Linux cgroup sampler is unsupported on this platform.

iPhone completes the same 1,180 eligible methods: 1,179 pass, one fails and
none skip. The unsupported-PiP regression runs successfully on this destination. The failure is
`MeridianTests.testStandardRestoresAdaptiveNativeTabsAndRetainsSettingsAcrossLayouts`,
whose assertion expects the original floating top tabs. Its XCTest bundle
finishes in 104.948 s. Xcode then reports a **600 s timeout collecting simulator
diagnostics**, inflating native command wall to 768.217 s and the job to
13m52s. This is retained first-observation failure handling cost, not another
ten minutes of test bodies; the source of the assertion needs M1 diagnosis.
The owned observer records exit65, no command timeout and completed cleanup.

tvOS accounts for all 1,166 eligible methods with passes and no skips; 36
iOS-only methods are compile excluded there. Its bundle finishes in 109.003 s,
but Xcode again waits **600 s for simulator diagnostics**, then reports test
success. Native command and job walls are 743.169 s and 12m48s. The four-destination Apple campaign finishes at 17:47:26 UTC,
31m42s after its first job starts; its earlier run stop records the iPhone
failure while tvOS continues. The two Apple
commands therefore contain 20 minutes of explicitly logged diagnostic waits,
including ten minutes after a passing bundle. This is a distinct reporting
cost to investigate with bounded artifact capture; the measurements do not
establish the underlying Xcode/runtime cause. Source purity, owned simulator
cleanup and observer process cleanup are qualified for all four destinations.

The fresh JVM repeat disables result caching/up-to-date shortcuts for Test
tasks only. All 1,171 observed method rows passed, matching the 1,171 static
JUnit declarations in count; independent compiled-method discovery remains a
separate gate obligation. The first JVM observation is retained as a different
cache/setup observation, rather than replacing its 6m56s with the repeat.

The device stream prints 81 completion dots against its native summary of 85.
Source and emulator configuration explain four conditional methods: three need
synthetic subtitle/remote fixtures that were not supplied, and one is phone-only
on an Android TV emulator. The stream has no per-method assumption records, so
81 passes and four skips is an inference, not a complete native status receipt.
Those four methods remain unqualified.

The primary seek-control browser collector refused the dirty checkout before
importing tests. The Cargo cache action creates an unignored cache directory in
the worktree; this is a source-supported cause candidate, because the primary
artifact did not retain the exact dirty paths. Cinema did not execute bodies:
its Xvfb wrapper lacked `xauth` after an implicit success condition skipped
provisioning following earlier failures. New workflow transport moves pure
browser discovery before cache mutation and makes diagnostic Cinema provisioning
independent. The separate exact-source continuation #5631 supplies their outcomes: seek
passed its one discovered method in 4.56 s, and Cinema completed 17 cases with
one live Router fixture skip in 9.20 s. Both source checks were clean. Cinema
returned zero but encountered the same unresolved frozen-wrapper cleanup
observation; seek completed with no cleanup error.
The playback-control browser exchanges (3), library rows (7) and subtitle
completion scenarios (3) passed. UI capture's 222 structural-golden failures are
actual assertions; its report contains no console or page errors.

All three VOD native reports identify build `86f1385`, pass 1/1 and contain no
fatal error. The frozen wrapper reports a surviving process group after each
zero command exit, but records no member states. It cannot distinguish live
orphans from zombies. Browser assertions are passed; cleanup qualification is
unresolved/failed under that observer, rather than a browser regression. The
original receipts are preserved while the collector gains state-aware evidence.

The source-declaration count is not the runtime count. Operations has 1,358
static method declarations, of which one belongs to a synthetic local test
fixture inside a discovered outer test. Validation has 548 declarations and
ten such local fixture methods. Excluding those local fixtures gives exactly
the native discovered identity sets: 1,357 and 538, with no missing top-level,
inherited, generated or overwritten methods.

The supplemental authoring failure is `tools/dv_processing/test_author.py`:
it requires `P81_HELPER`, `P81_ENCODED`, `P81_TIMING` and `P81_RPUS`, which the
portable cell does not provision. Its source owner and fixture requirement
are now explicitly unqualified. The combined checker (17 cases) and RPU
unpack checker (10 cases) completed every method successfully, but the old
collector interpreted native `SystemExit(False)` as a failure under Python
3.11. Their original red receipts remain. A new regression demonstrates the
collector defect and verifies integer normalization of both boolean exits;
the frozen receipts are not rewritten as green results.

## What already explains part of the cost

The replicated Store lane is dominated by test execution: **75m36s of bodies**
inside an 81m26s job, with only 5m38s compiling. Its serial binary completes
375 identities: 371 pass, one fails and three are ignored. The failure is
`scan_identity_repair_contract`, asserting `AlreadyApplied` where `Applied`
was expected; its assertion does not identify which backend failed. The Make
recipe stops there, so the following `schema_lineage_upgrade` binary did not
execute. That is a missing execution obligation, not a passing Store suite.

Completion intervals have an 11.105 s median; 310 of 372 executed cases fall
between 10 and 20 s. The ten longest intervals account for only 8.06% of the
native body timeline, so a handful of outliers cannot explain the total.
These are serial completion intervals, not instrumented setup/body/teardown
splits. Source maps 250 cases to a helper that visits in-memory SQLite,
fresh file SQLite and a fresh three-voter cluster; two more use its
strandable variant and 87 executed cases start clusters directly. Factory
cases account for 68.01% of the completion timeline. Each cluster creates
three subprocesses, fresh ports, temporary storage and TLS, boots/initializes,
then shuts down and reaps the children. Initialization explicitly waits five
500 ms heartbeats. There is no evidence yet dividing the remaining per-case
cost among admission, startup, body and cleanup.

The distinct schema-lineage continuation fills that unexecuted binary without
repeating the 375 Store cases. Its one inventoried native method,
`narrow_schema_lineage_bridge_preserves_and_refuses`, passes in 123.36 s;
compile and native command walls are 85.381 s and 123.857 s, inside a 4m19s
job. The original features, stack bound and serial thread flags are retained.
The reviewed observer records complete cleanup, exact executable/source
binding and a complete native outcome. This is a separate cache/scheduling
observation, and does not make the original Store job green.

Store optimization therefore needs isolated, balanced shards and measured
fixture lifecycle work, preserving every backend and assertion. Reusing one
mutable cluster across unrelated cases would change isolation and needs a
separate design and equivalence check. Compilation/cache changes alone cannot
make this observed 75-minute serial body meet a 45-minute full-suite target.

The scheduler assigns the fast Rust compile job to the single online
`ci-topology` runner. The topology, WAL and transport contracts require that
same class and queue behind compilation. The compile job occupied it for
19m22s before topology could begin. The sole registered `ci-store` runner
does not satisfy those labels. Required high-CPU jobs can also land on runners
advertising different bare-metal/Incus and OS classes. This is scheduler and
resource heterogeneity to measure, not evidence that every eligible runner
has identical performance.

The observed single-runner sequence has effectively no idle gap:

| Runner-class occupancy (UTC) | Job |
|---|---|
| 15:40:47–16:00:09 | Rust compile gate |
| 16:00:09–16:14:39 | Topology |
| 16:14:39–16:17:09 | Transport contracts |
| 16:17:09–16:36:50 | WAL |
| 16:36:51–16:45:10 | Voter recovery |
| 16:45:11–16:52:58 | Learner recovery |
| 16:53:01–16:57:54 | Recovery evidence validator |

Seven jobs occupy the runner for 4,622 s (77m02s); the first six occupy it
for 4,329 s (72m09s). The final validator is compilation and evidence work,
not another set of role cycles.

These are observed job windows, not a claim of pure scheduler queue time.
The recovery roles also have a real contracts dependency. A future capacity
change must verify equivalent capabilities before changing runner labels.

The first daemon failure is
`cluster_activity::a_follower_builds_the_portable_cluster_backup`: the follower
remained at `/readyz = 503 starting` until its 90-second readiness deadline.
Activation completed 9/9 cases in 228.89 s; activity completed 7/8 in 581.61 s.
That is an assertion failure, not a successful retry or an omitted test.

The daemon Cargo target cache resolved inside a disposable Forgejo job
workspace. Its paired workspace volumes are removed after the job, so that
observed cache cannot persist across runs. Compilation occupied 509.00 s and
6.21 s across its two commands, totaling 8m35s before/alongside 13m30s of test
bodies. Registry/download warnings occupied seconds, not those minutes.
Persistent bounded caches are therefore a measured optimization priority.
The ordinary Rust no-run inventory and following test command share a target
directory within one job. The continuation's execution reports a 0.44 s Cargo
finish and no `Compiling` lines: it reuses the compiled tests after static
checks. `cargo check` alone does not produce runnable test executables. The
original ordinary cell spent 101m04s in compile/list discovery, which completed
at 17:24:56 UTC in step metadata and printed 6,970 identities at 17:24:54.
The following combined command phase lasted 15m49s before the 17:40:46 job
deadline. Its static checks run first, but the inner command files and resource
samples were lost when evidence steps were canceled; no unit execution is
qualified from that phase. This is insufficient evidence to blame CPU, memory,
disk or assertions for the cost. A separate frozen-source continuation uses
bare-metal labels, owned per-phase deadlines and uploads before the next phase.
It retains the original failure and records its own cache/environment state.

Topology's 14m30s includes more than its 11.96 s of libtest summaries:
separate native artifacts record a 25.80 s semantic campaign and 357.44 s of
failure drills. Preserve these as runtime obligations when considering cache
savings. Transport's 77 passing invocation rows total 0.80 s inside a 2m30s
job. Invocation counts may overlap source identities across filters and
feature cells; these are not a new unique-suite count.

Voter, learner and final evidence validation each build the same recovery
validator with the `transport-recovery` profile in separate Cargo lanes.
Their reported build times total 864 s (14m24s); native role campaigns take
198.348 s and 168.967 s. A reuse design must retain exact source, profile,
features, ABI and runtime-path compatibility before treating one artifact as
valid in another lane. The final validator had no resource sampler in this
frozen run; its build/job timings are retained, resource usage is unavailable,
and the newer diagnostic workflow instruments it explicitly.

The ordinary Rust continuation completes compilation in 389.440 s and
inventory in 2.572 s: **6m32s for the corresponding discovery work**, compared
with 101m04s in the original job. It records 6,970 cases, 49 ignored identities
and 36 exact executable hashes. All 619 compiler-artifact records report
`fresh=false` at Cargo level. Static checks then pass in 228.651 s. The new runner
reports 22 logical/affinity CPUs. Both observations use the frozen source and
immutable image, but original per-job resource/cache metadata was lost; runner
class, cache/environment and build concurrency are not controlled together.
This large observed difference therefore warrants runner qualification and
resource investigation, rather than attributing it entirely to cache reuse.

The continuation then finishes the ordinary command in **76m25s**, with
complete process cleanup and native exit 101. Each of the 36 inventoried
binaries has one outer summary matching its full expected case count:
6,896 pass, 25 fail and 49 ignore, totaling 6,970. Their reported body times
sum to **76m17.60s**. This is actual test execution, rather than another long
compilation. Seven selected workspace doctest streams also finish; their
native summaries remain distinct from the ordinary inventory.

| Ordinary Rust binary | Native body time | Outer outcome |
|---|---:|---|
| `plurxd` | 36m41.83s | 4,362 pass, 15 fail, 40 ignore |
| `plurx_core` | 22m05.21s | 1,932 pass, 10 fail, five ignore |
| `store_contract` | 16m56.49s | 270 pass |

These three binaries consume **99.26%** of the ordinary binary body time.
The ordinary Store feature cell and the separate 75-minute replicated Store
cell are distinct environments with overlapping source identities; their
counts cannot be added as additional unique tests. Their durations describe
separate required feature cells.
The full continuation job takes **87m47s**, including compilation, inventory,
static checks, both exact ignored regressions, setup and evidence handling.
The two regressions each execute exactly one formerly ignored body and pass,
without changing the ordinary run's 49-ignore observation.

Samples wholly inside the unit phase cover 4,581.690 s, excluding 3.334 s at
its edges. Current-job cgroup CPU increases by 61,335.195 CPU-seconds,
averaging **13.39 CPU equivalents**; 85.39% is reported as system CPU.
Sampled unit-phase memory peaks at **8.41 GiB**, swap at **0.102 GiB**.
The observed cgroup records no throttle/high/max/OOM event increases and
advertises no own CPU/memory cap. Parent/host constraints remain unobserved.
This establishes substantial parallel CPU activity during the phase, not a
per-case wait fraction or the cause of the high system time. Profile kernel
work, process/fixture lifecycle and scheduling before assigning that cost.

The frozen timing parser cannot qualify this ordinary stream: child-test
diagnostic tails contain their own libtest banners, outcomes and summaries,
which it mistakes for outer suites. Its original 1,730 qualification gaps and
false verdict are retained. A separately reviewed, source-bound reconciliation
excludes eight explicitly labelled restore stdout replay blocks and two
filtered Tray child invocations. It then requires every exact inventoried
case once, matching ignored flags, header/footer counts and all 36 executable
identities. All 6,970 cases reconcile. Omission, duplication, changed-status
and footer-mutation negative controls reject corrupted evidence. This targeted
analysis is not a general replacement collector or a green gate verdict;
future collection must frame nested output or apply equally strict checks.

The 25 outer failures have exact source owners in the measurement extract:

| Assertion family | Cases |
|---|---:|
| Core source census, durable-table and migration expectations | 10 |
| Shared Source HTTP/actor acquisition and readiness | 9 |
| Daemon read authority, refusal codes and seam census | 3 |
| HLS processed-descriptor receipt counter | 1 |
| Rolling publication capacity admission | 1 |
| Serving-fence live-child negative control | 1 |

These are assertion locations and observed symptoms, not established root
causes. Twenty are absent from the historical 48-failure list; that alone does
not establish new regressions because source and case universes changed.

The preflight spends almost as long checking corrective history (148 s) as
executing 1,895 Python tests (155.62 s). The Android device runner reports
190.096 s inside a 1,053 s job; build, emulator and setup phases account for
the remaining wall time. The slowest Node command bodies are playback web
control (17.57 s), network shaping (15.56 s), web asset ordering (14.91 s),
cluster membership (13.06 s) and live TV (8.45 s). These are separate
optimization targets from the Rust test bodies.

## What the measurements prioritize next

| Work | Evidence and qualification needed |
|---|---|
| Close actual failures and collection gaps | Preserve and diagnose the 25 ordinary Rust failures, replicated Store repair assertion, daemon follower readiness, iPhone navigation assertion and structural-golden facts. Frame nested child logs so exact case reconciliation cannot mistake diagnostic text for outer results. |
| Profile and split the expensive ordinary binaries | Daemon, core and ordinary Store bodies consume 99.26% of ordinary Rust time. Unit-phase samples average 13.39 CPU equivalents with 85.39% system CPU; profile that work before attributing it. Qualify balanced scheduling with features, fixture isolation and every case preserved; compilation reuse alone leaves 76m18s of bodies. |
| Bound Apple reporting | Native bundles finish in about 105–109 s, followed by two explicit 600 s diagnostics waits. Qualify a bounded collection policy that keeps native outcomes and necessary failure artifacts. |
| Qualify Rust runner and cache placement | Discovery differs by 15.47× across these observations. Standardize measured capacity, observe parent/host constraints and demonstrate persistent cache mounts; repeat controlled same-source comparisons before claiming the cause or a steady-state budget. |
| Reduce replicated Store lifecycle cost | Median serial completion interval is 11.105 s and outliers explain little of the total. Instrument admission/startup/readiness/body/drain separately; preserve isolated writable state and every backend. Then qualify balanced shards under host voter/memory limits and sufficient registered capacity: only one runner currently matches `ci-store`. |
| Remove scarce-runner rebuild serialization | Seven lanes occupy 77m02s; three validator builds consume 14m24s. Qualify a compatible shared executable and move independent compile work off the scarce class only after capability equivalence is demonstrated. |
| Close environment and execution gaps | Supply owned synthetic/native fixtures and privileged disposable environments, and account for the original incomplete TV navigation cell. Explicit owners and missing outcomes remain part of the manifest. |

These priorities refine M1–M3 in the canonical plan. They are not implemented
performance improvements or a promised new full-suite duration. The existing
75-minute serial Store body alone exceeds the proposed 45-minute warm target;
all critical paths must fit before the automatic gate can be promoted.

## Evidence and remaining qualification

Per-job artifacts bind the candidate SHA/tree, toolchain, command identities,
original exits, runtime cases where supported and resource samples. The
ordinary Rust inventory also records exact executable hashes. Full original
logs and artifact ZIP hashes are retained outside tracked source. The
[compact measurement extract](FULL-SUITE-BASELINE-2026-10-20.json) records the
exact input snapshot, original failures, artifact/report hashes and missing
evidence. Its capture timestamp identifies the completed observations and
separates the original qualification failures from derived analyses.

Timing granularity follows the native runner. Ordinary Rust reports completed
binary durations and slow-test warnings; its redirected log does not provide
an elapsed duration for every case. Store's serial Forgejo completion
intervals include fixture and cleanup time. XCTest supplies native case/bundle
times, Python supplies method/subtest outcomes, and the 70 Node cells have
script-level wall times. These measurements are not interchangeable, and no
synthetic per-test Rust timings are inferred from concurrent completion order.

The preflight used Python 3.14.8; the immutable supplemental image used Python
3.11.2 and Node v22.23.2. Rust is pinned to 1.97.1. Current-job cgroup samples
do not see sibling Docker test containers. Such
cells mark actual-test CPU, memory and I/O unavailable. macOS cgroup metrics
and unsampled cgroup-v1 I/O remain unavailable, not zero. First-observation
cache state is uncontrolled; one candidate does not establish warm/cold
variance or a p95 budget.

The private mount/ENOSPC continuation ran no test bodies. Its first private
mount namespace probe failed; a retry retained the effective capability,
seccomp and AppArmor state before setup. The runner did not grant requested
`CAP_SYS_ADMIN`, retained seccomp mode 2 and `docker-default (enforce)`. The
four environment-controlled cases are unqualified, not assertion failures.
No host security settings were changed. The 55 governed crate `#[ignore]` identities
are separately dispositioned: 14 fixture-catalog entries are part of those
55, not 14 extra tests. Two exact serial restarts now execute and pass; three delegated
child cases require their parent receipts, one FFmpeg capability case has a
separate manual workflow, and 49 remain environment/fixture/campaign gaps.

The ordinary Linux inventory contains 49 of those 55 governed identities,
with every native ignore matched to its source owner. The six exclusions are
three Store entries requiring `hiqlite-contract-tests`, one rolling campaign
requiring `cluster-integration-tests`, and two macOS-only entries. They are
explained feature/platform exclusions, not lost native discovery records.

Two additional ignored identities sit outside those governed crates: the
Hiqlite M0 manual 10,000-write/RSS benchmark and a vendored Hiqlite ENOSPC
control requiring an owned filesystem of at most 32 MiB. The former was
observed ignored in the topology log; the latter was not provisioned. A
separate source-group ledger identifies 20 extra Rust groups outside the
principal 7,252 declarations, including tooling, fuzz support and vendored
suites. Its 619 raw static attributes are not an added runtime count. The
combined backend's 24 source attributes have no configured discovery lane;
its isolated test build references an absent `process/census` module, recorded
as a source/configuration gap rather than an observed compiler failure.

Apple illustrates why an outer job cap is insufficient evidence handling.
Its mobile helper recorded iPhone exit 65, iPad and tvOS exit zero; the desktop
build/test commands also returned zero. TV navigation then consumed the rest
of the 60-minute job budget. Forgejo terminated the job and its `always()`
evidence steps did not run. The full log preserves command statuses but no
per-destination native case counts or original iPhone assertions. These
outcomes are incomplete; a zero command exit is not a substitute for the
lost native receipts. Future observations must upload phase evidence before
longer phases and reserve time for cleanup and artifact persistence.

All 48 historical failures have source ownership: seven specific repair
candidates, 39 cases sharing the Source admission mitigation and two
previously unresolved cases (Store-result census and stale telemetry sink).
The independent exact-binary/case reconciliation observes **43 passing and
five failing** on the frozen candidate. The remaining five are two shared
Source HTTP cases and three Source actor cases; earlier repair labels do not
override those fresh failures. Original failed outcomes and the frozen
observer's qualification error remain unchanged. Native decoder/pixel/renderer
fixture gaps are retained individually. The diagnostic measurements are
recorded; qualification remains open for unresolved assertions, environments,
production-safe collection and controlled warm/cold comparisons.

## Measurement implementation checks

These are checks of the M0 branch, separate from frozen-candidate suite counts.
Python 3.12 local validation completed 547 tests with one skip;
operations completed 1,431 with two skips. The first operations
sweep exposed an ignored GitHub permissions declaration in the new Forgejo
privileged workflow; removing it made the repeated complete sweep pass.
The skipped landing-status audit passed after fetching the independent
clone's missing history. The remaining local skips require GNU Linux
linker/readelf and Linux pidfd lifecycle facilities; their original local
skip outcomes are retained rather than relabeling the full sweep.
Synthetic failures printed by negative-control tests are expected witnesses,
not failed outer suites. Rust 1.97.1 formatting and workspace/all-target Clippy
with warnings denied pass, as do syntax checks for all 95 served scripts.

Focused regressions cover omitted/wrong-binary cases, exact ignored-case
execution, corrupted native listings, workspace-only doctest selection,
original nonzero exits, source/observer binding, process-group ownership,
timeouts and unsafe-cleanup blocking. Normal hooks and paperwork remain in
use. These checks validate the measurement implementation; full software
qualification remains subject to the recorded runtime failures and gaps.
