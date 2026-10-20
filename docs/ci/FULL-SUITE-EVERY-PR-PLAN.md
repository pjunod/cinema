# Full suite on every PR — audit, execution contract, and implementation plan

**Status:** open — audit complete; M0 implementation and baseline in progress ·
**Requested:** 2026-10-20 · **Audit source:** `c1c344132` ·
**Evidence captured:** 2026-10-20 14:21:12 UTC

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (the rules
currently enforced), [VALIDATION.md](../VALIDATION.md) (behavior ownership),
and [RUNNER-DISK.md](RUNNER-DISK.md) (resource limits). This is the canonical
ledger for restoring complete automated test execution on every ready PR.
Read the measured findings in §2 before choosing optimizations, then build
§6 in order on one `effort/full-pr-suite` branch. A task that appears to
need fewer tests, weaker assertions, swallowed failures, or unqualified
hardware exclusions must flag the contract change before implementing it.

The October 20 follow-up authorizes M0 implementation and a complete diagnostic
baseline. Work is isolated in an independent clone on
`codex/full-pr-suite-m0`; the user checkout is not used for edits or tests.
Existing source fixes are identified separately from unimplemented work.
No passing complete run is claimed until its outcomes are recorded.
Current measurements are tracked in
[FULL-SUITE-BASELINE-2026-10-20.md](FULL-SUITE-BASELINE-2026-10-20.md).

## 1. Outcome — every landing has complete, fresh test evidence

Every ready PR, including PRs into an effort branch, automatically tests its
current merge candidate. Drafts retain inexpensive paperwork only. A
superseded head/base may cancel; it cannot merge using its old result.
The latest candidate must finish all required software test cells before
landing. Paths select early feedback and priority, not permission to omit
the full suite. Documentation-only PRs follow the same final requirement.

This target replaces the compile-only/proportional-runtime placement goals
in [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md),
[CI_EXECUTION_ACCELERATION_PLAN.md](CI_EXECUTION_ACCELERATION_PLAN.md), and
[RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) when the cutover
in §6.6 is qualified. Their valid inventory, cache, and resource safeguards
remain requirements. Their historical passes remain historical evidence.
Current triggers remain unchanged until implementation lands.

**Full means an enumerated universe.** It includes ordinary software units,
integration/contracts, browsers, supported simulators/emulators, and
automatable privileged checks. Physical-display, real-tuner, mastered-media,
and destructive live-cluster qualification need explicit environment cells
and separate evidence. They cannot become silent no-op passes or be
described as covered by this software gate. M0 must enumerate every such
exception and its owner before the software universe is approved.

**Fresh means execution for this candidate.** Reuse compiled artifacts with
valid input keys. Previous candidates' successful test outcomes do not
replace execution in the required manifest. Existing Python/Node journals
may retain diagnostics; their adoption paths and manual retained-unit proofs
must not satisfy the new fresh-execution gate. If reuse is later desired,
change this contract explicitly with its measured benefit and safeguards.

## 2. Audit — the count is large, but waits and missing execution dominate

The durable [audit data](FULL-SUITE-AUDIT-2026-10-20.json) contains source
inventory methods, run/job counts, six log samples, exact tested SHAs,
per-binary results, and log SHA-256 digests. Its
[metadata extract](FULL-SUITE-AUDIT-2026-10-20-EXTRACT.json) retains the exact
read-only query, parameters and compact run/job/merge rows so denominators
and duration summaries can be recomputed. Source and read-only Forgejo
records were inspected; no fresh full run or controlled resource experiment
was performed. The checkout advanced concurrently from `2ae66759b` to
`c1c344132`. Both committed inventories are retained; the audited CI and
heavyweight fixture files did not change between them. Reverify against the
implementation branch before acting.

### 2.1 There are more than 11,000 declarations, not one uniform unit suite

| Source group at `c1c344132` | Static declarations |
|---|---:|
| Rust under `crates/` | 7,252 |
| Python operations / validation / benchmark | 1,304 / 502 / 52 |
| Swift mobile / server tray | 1,246 / 75 |
| Android JVM / instrumentation | 1,171 / 85 |
| Principal groups, before JavaScript | **11,687** |
| Other Python tooling and tests | 247 |
| All non-JavaScript estimates | **11,934** |
| JavaScript lexical test/it callsites, counted separately | 1,419 |

These are declarations, not runtime-discovered cases. Macros, inheritance,
parameter expansion, platform/feature conditions, ignored cases, and plain
assertion scripts prevent an exact runnable total from source counting.
iOS/tvOS executions of one method are separate environment results, not
two unique source declarations. The source-only ignored inventory reported
55 Rust identities (41 ignored inventory, 14 opt-in fixtures) and zero
known-red entries; that does not prove those tests execute or pass.

Reproduce the committed-source estimate without importing or running tests:

```bash
scripts/audit-test-inventory c1c344132 > /tmp/plurx-source-inventory.json
python3 -m validation.known_red --no-cargo-list
```

**How to read it:** an estimate supports sizing only. The discovered
identity manifest and completed outcomes in §4 establish completeness.

### 2.2 Full execution is currently discretionary and has no green baseline

Window: after 2026-10-07 19:29:34 UTC, when full Rust units left the ready-PR
gate, through the evidence capture above. **170 PRs merged into `main`.**

| Forgejo evidence in that window | Outcome |
|---|---|
| `ci.yml` full workflow | 46 attempts: 28 failed, 18 cancelled, zero successful |
| Its Rust/SQLite `check` job | 13 failed, 8 cancelled, 25 skipped, zero successful |
| Coverage workflow | 66 attempts: 50 failed, 16 cancelled, zero successful |
| Last successful coverage job | Run 4211, ended 2026-10-07 02:13:57 UTC; 28m35s, outside this window |

These are attempts, not a ratio of PRs with verified complete evidence.
A skipped job can reflect scope, failed prerequisites, or retained evidence;
the counts alone do not classify each skip. Smaller lanes can pass while
the complete graph fails. There was no `validation-nightly.yml` run in the
captured window; its name does not imply a schedule.

[main-fast-lane.yml](../../.github/workflows/main-fast-lane.yml) compiles
Rust and runs Clippy, compiles Android, and compiles Apple plus a TV
navigation check; Python and Node units do execute. The full
[ci.yml](../../.github/workflows/ci.yml) is manual/release-tag triggered.
[Makefile](../../Makefile)'s `unit` excludes `plurx-cluster-check` and the
extra replicated/daemon test features. `test-full` includes those features,
but does not automatically execute all ignored or hardware cases. Restoring
`make unit` alone would not implement the requested complete suite.

### 2.3 The 28-minute and 89-minute runs measure different work

| Measurement | Coverage 4211 | Rust gate 5447 |
|---|---:|---:|
| Source | `cca4a09b997` on main | `4659625e8bb` on `effort/apple-auto-declared-costs` |
| Execution date, UTC | October 7 | October 18 |
| Completed tests, excluding ignored | 5,458 | 6,721 |
| Failures | 0 | 48 |
| Clippy stage | absent | 4m41s |
| Test compilation | 5m19s | 7m20s |
| Sum of completed binary test durations | **20m44s** | **75m42s** |
| Whole job, excluding queue | 28m35s | 88m51s |
| Tests reported running over 60 seconds | 5 | 315 |

The source trees, runner machines, FFmpeg environments, and workload differ.
This is not a measurement of instrumentation overhead or normal variation
on an unchanged suite. The older runner was in the general pool; today's
reserved coverage limits were introduced later. Run 5447 failed before the
two following serial FFmpeg restart checks, so its duration is not the
completed full-gate runtime.

| Dominant binary | Old tests / seconds | Later tests / seconds |
|---|---:|---:|
| `plurx_core` | 1,558 / 527.13 | 1,914 / 1,375.54 |
| `store_contract` | 234 / 256.10 | 269 / 868.00 |
| `plurxd` | 3,527 / 455.83 | 4,316 / 2,267.08 |

The extra minute counts are mostly execution, not compiler setup. Four
additional sampled failed gates lasted about 30, 30, 60, and 60 minutes;
some emitted only partial results before their deadline. Treat a missing
binary result as incomplete, never as a zero-cost or passing binary.

### 2.4 Other complete-suite lanes also constrain the critical path

Successful job durations in the same mixed-source window:

| Lane | Success sample size | Median minutes | Maximum minutes |
|---|---:|---:|---:|
| Replicated Store, legacy | 6 | **40.94** | **84.90** |
| Cluster daemon | 10 | 19.62 | 23.88 |
| Topology | 5 | 16.42 | 21.53 |
| Apple iOS/tvOS | 2 | 17.02 | 17.22 |
| Android device | 7 | 10.88 | 17.70 |
| Android JVM | 16 | 6.92 | 14.57 |

Rust units, WAL, web layout, and VOD browser acceptance had no successful
job in this window. This does not establish that every current failure is
still present; several repairs landed afterward. These sample medians are
not SLAs or normalized benchmarks. The Store lane can remain the longest
path even after ordinary Rust tests improve.

## 3. Ranked findings — fix measured costs and qualify existing repairs

### 3.1 Real Source fixtures repeatedly bootstrap databases and encode media

**Proven structure:**
[shared_source_playback.rs](../../crates/plurxd/src/http/shared_source_playback.rs)
builds actual replicated voters, constructs AppState through another SQLite
initialization, creates media with real FFmpeg, optionally muxes subtitles,
and probes it per fixture. Some modes generate 1080p Main10 with libx265.
The fixture family was absent from the older successful tree. The
[Source actor tests](../../crates/plurxd/src/transcode/tests/source_actor.rs)
repeat similar setup. Fixture-generation commands lack local deadlines
and explicit encoder thread limits. A manager's worker budget does not
automatically cap its external fixture-generation subprocesses.

**Current repair:** `4eee020e4` added a four-permit process-wide Source body
budget in
[source_actor.rs](../../crates/plurxd/src/transcode/source_actor.rs), retained
through normal fixture shutdown. It postdates run 5447. First qualify that
repair on the current candidate; do not replace it based on the old log.

**Next measurement and change:** time admission, bootstrap, migration,
generation, probe, body and teardown separately. Sweep admitted fixtures
1/2/4 with CPU/RSS/IO/child counts. If generation dominates, reuse immutable
recipe-and-tool-version keyed media and copy it into each test's private
directory. Tests of encoding, cancellation, identity changes, or media
mutation still run their actual behavior. Add bounded tool execution and
owned child cleanup; preserve stderr and original assertions.

### 3.2 Store fixtures repeat migrations and backfills

**Proven structure:**
[store_contract.rs](../../crates/plurx-core/tests/store_contract.rs) constructs
fresh memory and file stores for each ordinary contract.
[SqliteStore initialization](../../crates/plurx-core/src/store/sqlite/mod.rs)
executes migration/backfill. The historical migration maximum grew from
103 to 112; ordinary SQLite contract count grew only 15% while elapsed time
grew 3.39 times. The feature-gated Hiqlite contract mutex is not evidence
that the ordinary SQLite target waits on that lock.

**Next measurement and change:** benchmark fresh initialization and fixture
body separately under fixed resources. If setup dominates, evaluate a
test-only initialized template copied into independent databases. Preserve
true empty/upgrade/backfill migrations, WAL/sidecar correctness, database
identity, PRAGMAs and isolation. Never share a mutable Store between cases.
Consider directly building Source AppState with its intended Store to avoid
discarding another fully initialized store; require constructor-equivalence
tests before changing helpers.

### 3.3 Failed fixtures may release capacity before asynchronous cleanup

**Proven failure and source path:** 48 failures in run 5447 included port
collision, HTTP assertions, unresolved Source starts, and child lifecycle
failures. Assertion panic can bypass explicit async
`SelectedStore::shutdown` in Source actor/body fixtures. Normal Source
shutdown drains owners before releasing its permit, but unwinding does not
run that async method. A permit limiting successful bodies alone is not a
failure-cleanup guarantee.

**Hypothesis to test:** leaked or undrained work may amplify subsequent
waits/failures. Logs show writer response-receiver panics but do not prove
that these originally caused Source startup failure. The 315 slow warnings
are latency evidence, not a CPU profile or proof of lock contention.

**Required repair qualification:** inject an intentional assertion failure,
timeout and cancellation after each fixture acquisition phase. Prove voters,
listeners, subprocesses and background workers drain before replacement
work is admitted, while the original failure remains visible. Select an
owned process or test-owned async cleanup scope from this evidence; do not
silently retry failed assertions or extend every timeout.

### 3.4 Narrow endpoint and deterministic fixture repairs already exist

The core restore failure was a proven Raft `Address already in use`.
`a0c2c6730` adds owned child processes and bounded endpoint acquisition
retries in
[migration tests](../../crates/plurx-core/src/cluster/migration/tests.rs).
`dbb6f27f9` repairs subtitle route classification, caption/remote/startup
fixtures and child handling; `450fcd296` cleans test-only manager coupling.
The full affected cohort must qualify these repairs together. Map all 48
historical failures to a fix plus a current outcome or an open reproduction.
An empty known-red catalog is not closure of that list.

### 3.5 Runner limits and workflow semantics can defeat more parallel jobs

[RUNNER-DISK.md](RUNNER-DISK.md) records OOM loss and missing runner labels
during the October 11 outage. The committed
[roster](../../validation/runner-fleet.toml) defines three Store-eligible
physical hosts and shared Android/topology placement. Eligibility is not
proof of current health, available memory or spare capacity. General jobs
have tight CPU/memory/disk bounds; a daemon compiler has measured near 7 GiB.
Adding shards on the same saturated host may make each test slower.

The existing [Store shard workflow](../../.github/workflows/store-shards.yml)
already checks inventory and exact shard union. Preserve those guarantees
and measure its mode/health before building another scheduler. Introduce no
new heavy workload on production voters to meet a time target.

**Merge evidence gap:** the current main fast lane intentionally tests the
head alone and ignores advancing main. Its aggregate accepts `skipped`
downstream jobs. A new full-suite gate needs candidate binding and strict
expected-lane outcomes, not merely another successful workflow name.

## 4. Execution contract — discover, execute, and account for every case

Reverify these source interfaces before implementation; proposed artifacts
below are new contracts, not files already emitted today.

### 4.1 One manifest covers each supported environment

| Required cell | Existing entry point / discovery foundation | Completion evidence |
|---|---|---|
| Ordinary Rust and SQLite | `make ci-rust-gate`; libtest discovery | Per-binary identities and outcomes; both serial restart regressions |
| Replicated/daemon/transport/WAL/Store features | Existing full `ci.yml` jobs; `make test-full` for universe reconciliation; `validation.store_shard` | Full feature/role inventory and disjoint shard union; no excluded cluster package left unowned |
| Rust doctests | Cargo doctest discovery/execution for supported cells | Separate results; do not lose them during runner migration |
| Python including tooling | unittest discovery; `validation.python_unit_receipts` | Imported/generated/inherited cases accounted for; discovery error fails |
| Web, playback and desktop-remote JavaScript | Makefile lists, Node reporters, plain assertion scripts | Cases plus script identities; no double execution through transitive requires |
| Browser/accessibility/VOD | Existing full-CI jobs | Completed behavior results and applicable capture artifacts |
| Apple iOS/tvOS and macOS tray | `make apple-test`, `make desktop-macos-test` | Destination-bound discovered XCTest cases and xcresult outcomes |
| Android JVM/device | `make android-test`, `make android-instrumentation-run`; `validation.jvm_unit_discovery` | Compiled discovery, JUnit/device cases, explicit emulator identity |
| Automatable privileged tests | `make bind-mount-check`, `make storage-pressure-check` | Qualified disposable environment and actual assertions, not early returns |

M0 reconciles overlaps between default and feature-expanded inventories;
do not run both wholesale and count duplicates as extra coverage. Record
case identity as `(suite, binary/class, test name, platform, feature cell)`.
An intentional run on two platforms belongs to two cells. Any incompatible
feature combination must have explicit separate cells, not `--all-features`.

Each manifest records source/test identity, expected cases/scripts, toolchain,
profile, target, features, environment image, FFmpeg version, runner class,
shard assignment, required lanes, and every skip/ignore disposition.
The 55 ignored Rust identities must each have execution or an explicit
environment-specific exception. New unclassified cases fail discovery.

### 4.2 One candidate binding follows every result

The proposed manifest/receipt binding is:

```json
{
  "schema": 1,
  "repository": "noirr/plurx",
  "pr": 123,
  "base_sha": "<immutable base>",
  "head_sha": "<immutable PR head>",
  "candidate_tree": "<tree produced by the intended merge>",
  "workflow_digest": "<scheduler and test-policy inputs>",
  "inventory_digest": "<complete cell and case universe>",
  "environment_digest": "<toolchain, image, features and runtime inputs>",
  "attempt": "<Forgejo run and attempt identity>"
}
```

Prepare a candidate once; all jobs check out that exact candidate and report
its binding. Build artifacts also include target ABI and flags. Compiler
caches may miss; they may not import stale results. For sharding, require
the same byte-identical binaries and binary digests within a compatible
cell, with discovery from those exact binaries. Matching build inputs alone
do not establish binary identity. Different binaries require explicit
separate cells; never mix another tree's binary with this list.

**Gate rule:** each required lane must report authenticated success for the
binding; executed identities equal its expected identities and shard unions
are complete and disjoint. Failure, cancellation, unexpected skip, missing
output, discovery error, unclassified ignored case, stale binding, or an
unfinished binary fails the gate. Platform exceptions are explicit manifest
decisions, not arbitrary `success|skipped` acceptance. Store package/runtime
and non-test qualification obligations remain separately required.

Use the existing `Main promotion gate` for main-bound PRs and retain
`Effort development gate` where the target requires it; define one aggregate
verdict per target with these same evidence rules. Keep the check names
stable for contributors and tooling.

### 4.3 Serialize landing and invalidate an advancing base

Use a per-target candidate/landing queue with one merge owner. New head or
base invalidates the candidate; its replacement automatically reruns the
complete required suite. The final owner holds the queue lock through live
head/base verification and merge, checks that the landing tree equals the
tested tree, and preserves `Regression-Test:` landing lines. Expected head
alone does not protect against another contributor moving the base.

This account has no server branch protection. CI status cannot itself
prevent a manual/API bypass. All contributors and bots must use the merge
owner/convention; document bypass detection and leave the gate red on stale
or incomplete evidence. Do not describe client-side convention as an
atomic server-enforced transaction. Out-of-band base movement requires
requalification and an explicit incident record if an unqualified tree lands.

## 5. Scheduling and performance — preserve assertions while shortening waits

Build once per compatible environment and split independent execution into
balanced shards using observed test durations. Include transfer time and
cache storage in the comparison. Fixed-size hash buckets prove assignment;
they do not prove equal duration. Keep an exact-union receipt even if
duration-based assignments change.

`cargo-nextest` is a candidate, not an assumed speedup. Its process-per-test
model changes global-state lifetime. The four-permit Source semaphore and
process-local endpoint registries do not limit all nextest processes or
several jobs on a shared host. Resource groups must cap voters, media tools,
database writers and total worker processes at scheduler and host level.
Preserve doctests, ignored regressions, features, child cleanup, timing
semantics, and the complete inventory in a paired pilot before adoption.

Record queue readiness/wait, setup, compile, discovery, fixture admission,
fixture setup, test body, cleanup and reporting separately. Job creation to
start includes prerequisites and is not automatically runner queue time.
Retain per-case timings and host CPU/RSS/IO/swap/disk pressure sufficient to
distinguish useful work from admission waits or saturation. Record both
warm and cold caches with the exact source/environment.

Capacity planning uses measured eligible ready-PR arrival rate and
runner-minutes per class, with headroom for bursts, retries of infrastructure
failures and one runner outage. The 170 merges alone are not an arrival or
load estimate. Reserve Apple, Android, Store and general capacity; enforce
per-host budgets rather than multiplying daemons on one machine. Add
hardware only after this model identifies the bottleneck and a named fleet
change is authorized.

**Proposed engineering targets, not observed performance:** first decisive
feedback within 5 minutes p95; warm complete candidate within 45 minutes
p95 execution and 60 minutes p95 ready-to-result, initially. M0 must either
show a capacity path to these targets or record a revised budget with the
reason. They are not permission to cancel a valid suite at the target,
silently skip a cell, or call a partial run green. Target completion
reliability is 100% evidence coverage of merged candidates, with no hidden
assertion retries. Measure first-attempt failures separately from eventual
infrastructure recovery.

Coverage remains a separate diagnostic policy. Measure instrumentation
on/off on the same source, case universe, environment and cache state,
with repeated runs. If it fits the accepted cost budget, collect profiles
during the required Rust execution and report afterward, preserving its
binary/source binding. Do not schedule another duplicate full suite merely
to update a badge. Coverage percentage and test pass/completeness verdicts
remain distinct.

## 6. Delivery — qualify one milestone before claiming the next

Each task branches from the current effort, opens a PR back into it, runs
its focused regression and affected compile loop, and passes the manually
dispatched effort gate before merge. Shared source ownership below makes
the independent-main exception inappropriate. Establish pinned Rust 1.97.1
compilation before Rust edits, following
[AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md). No code or fleet work is
claimed by writing this plan.

### 6.1 M0 — discover the universe and establish a current baseline

**Files:** new inventory/timing support in `validation/`; existing discovery
helpers; reporting additions to workflows and Makefile; audit evidence.

Produce the per-cell manifest in §4, inventory all opt-in and tooling tests,
and collect one complete current candidate's diagnostic outcomes. Map all
48 historical failures to current fixes or reproductions. Retain unsuccessful
outcomes rather than waiting for a green result to start measurement. Run
controlled warm/cold baseline observations and collect per-phase/resource
metrics. First failure stops that lane's green verdict, not evidence capture
for unrelated lanes. Determine available runner capacity and dependency
critical path before promising the target budget.

**Acceptance:** every static group has a discovery owner; no discovery
errors or unexplained ignores/skips; expected-minus-observed identifies
incomplete binaries; exact candidate/environment is recorded. Tests adding
an undiscovered case, omitting a result, and failing discovery are rejected.
Record baseline performance and the list of unresolved reds.

#### M0 diagnostic implementation (2026-10-20, measurement in progress)

`ci.yml` accepts an explicit `diagnostic_baseline=true` with an exact
`expected_sha` on a non-main branch. It refuses retained results and promotion
inputs, forces the legacy execution path once, lets independent jobs collect
evidence after preflight failures, and disables publication and badges. Ordinary
triggers and required-gate policy are unchanged. Per-job artifacts retain source
binding, tool versions, timing, resource sampling and original failure logs.

The ordinary Rust cell first compiles and enumerates exact executables, recording
binary hashes and ignored identities; execution follows through the existing
recipe. Python adapters capture native unittest discovery and per-method outcomes;
script entrypoints retain their required `__main__` setup. Node records the exact
Makefile unit command universe and independent exit codes. Doctest log streams
have distinct identities. Apple destinations and the three declared VOD cases
collect independent command outcomes rather than stopping at the first failure.

Supplemental Python tooling has explicit fixture gaps: decoded-layer evidence,
timeline replay/native-driver inputs, parsed-RPU pixels/chroma and RPU reuse
runtime fixtures are absent from the slim source bundle. The workflow records
these identities and path owners as unqualified. Plain renderer controls, physical
hardware, destructive live-cluster cases, and the four privileged mount/ENOSPC
cases need qualified disposable environments; they are not implied passes.
Root/CAP_SYS_ADMIN mount checks must use a private mount namespace and temporary
paths, without a host Docker socket, device exposure or shared propagation.

Current-job cgroup sampling cannot observe sibling Docker test containers. Those
cells explicitly mark actual-test CPU, memory and I/O unavailable; wall timing
remains valid. Unsupported macOS cgroup metrics are likewise explicit. This first
run is observational, with existing runner cache state recorded; it cannot establish
warm/cold variance or a p95 from a single sample. M0 remains open until the run,
source inventory reconciliation, unresolved reds and environment gaps are recorded.

#### First measured priorities

The [baseline report](FULL-SUITE-BASELINE-2026-10-20.md) already separates
three substantial costs: Store's serial bodies take 75m36s, seven jobs occupy
the single eligible topology runner for 77m02s, and the recovery validator is
built independently three times for 14m24s. These observations make Store
fixture lifecycle/isolation and balanced shards, compatible build reuse, and
runner placement the first optimization investigations. iPhone and tvOS
add 20 minutes waiting for Xcode simulator diagnostics after native bundles
finish, including after a passing tvOS bundle. Investigate that reporting
phase while preserving native failures and necessary failure artifacts.
These observations do not establish an achieved full-suite duration.
The proposed 45-minute warm target needs
qualified Store parallelism or less repeated setup; cache improvements alone
cannot meet it with the observed serial Store body.

### 6.2 M1 — repair failure ownership and qualify existing fixes

**Files:** affected core migration/Store fixtures, HTTP/Source fixtures,
transcode tests, Apple/golden assertions and diagnostic collection controls,
and focused regressions. Run after M0; serialize overlapping
fixture edits with M2.

Qualify the endpoint, route, runtime, child and body-budget fixes already
present. Reproduce remaining OAuth/Source errors without treating them as
timing noise. Implement failure-safe fixture ownership and finite setup/body/
cleanup deadlines where measurements show missing bounds. Replace arbitrary
sleep with observable readiness where behavior permits; keep actual timeout,
pacing and cancellation tests on their intended clock.

**Acceptance:** intentional body panic, cancellation, tool timeout and bind
collision retain the original failure and leave no owned child/listener/
writer after the cleanup deadline. Ten repetitions of the affected cohort
under its declared concurrency produce identical inventories, successful
cleanup and no assertion retries. Full current Rust/SQLite/serial restart
lane passes; the complete manifest tracks other remaining lane failures.

### 6.3 M2 — reduce repeated setup while retaining real behavior

**Files:** Source/HTTP/media test helpers, SQLite fixture helpers and their
focused isolation/migration/media regressions. Depends on M1 ownership.

Use M0 phase measurements to choose media reuse, direct Store injection,
initialized DB cloning, or no change for each helper. Key immutable inputs
by recipe/toolchain; keep writable state per test. Measure cold preparation
and warm reuse, source mutation, process teardown, database clone isolation
and genuine migration/encoding tests. Publish before/after same-candidate
results; remove an optimization if assertions or test identity change.

**Acceptance:** full expected inventory and outcomes are preserved; mutation
of one fixture cannot affect another; real upgrade/encode/cancel assertions
still execute. Repeated controlled observations demonstrate which phase
improved and whether contention or memory changed. No speed claim based on
a failed or truncated run.

### 6.4 M3 — shard execution within measured host budgets

**Files:** Rust execution adapter/configuration, Store shard integration,
workflow matrices, runner-class scheduling and receipts. Depends on M0/M1;
rebase performance evidence after M2.

Compare current libtest scheduling with a qualified nextest pilot. Reuse
builds only across compatible ABI/environment cells. Generalize existing
Store exact-union checks; cap process-wide and host-wide heavyweight work.
Inspect the replicated Store critical path and serial controls independently
of ordinary Rust. Preserve Android/Apple resources during Linux fan-out.

**Acceptance:** missing/duplicate/wrong-binary/wrong-feature shards fail;
the union equals discovery. Legacy and proposed runners execute equivalent
case cells on the same candidate with no retries. Three warm repetitions,
one cold run and one runner-loss drill record critical path, runner-minutes,
memory/disk bounds, cleanup and all required outcomes. Adoption requires a
measured benefit within capacity; keep libtest if the pilot does not qualify.

### 6.5 M4 — automatically bind and gate every ready PR candidate

**Files:** `main-fast-lane.yml`, `effort-ci.yml`, shared/full runtime
workflow definitions, `validation` binding/gate code, merge-owner tooling,
and workflow behavioral tests. Depends on M0/M1; use M3 where qualified.

Refactor existing runtime jobs into one reusable source of truth. Start the
full required manifest automatically for ready `main` and effort-targeted
PRs, including docs-only changes. Retain cheap early feedback and draft
paperwork. Remove scope/retention bypasses from the fresh required verdict.
Implement the candidate queue and result binding in §4 before treating
individual green jobs as merge authority. Cancel superseded work only with
owned cleanup and always-published partial diagnostics.

**Acceptance:** negative controls for failed/missing/skipped/cancelled lanes,
wrong tree/platform/features, incomplete inventory, stale head, advancing
base, unrelated workflow and changed merge method all block landing.
A throwaway failing regression blocks the PR; correcting it permits a fresh
complete result. Head/base changes queue new candidates. Concurrent landing
attempts serialize. API landings preserve regression lines. Runtime jobs
never publish or deploy as a side effect of PR validation.

### 6.6 M5 — promote the policy only after complete pilot evidence

**Files:** pipeline/validation policy, old plan disposition headers,
contributor/merge tooling, workflow tests, this ledger, and backlog.

Observe at least ten distinct ready candidates with complete required
inventories and first-attempt outcomes, including Rust, client, docs-only,
rebased and concurrent-PR examples. Include at least two cold candidates and
the runner-loss drill. Resolve every failed required lane; a current green
ordinary Rust subset cannot qualify WAL/browser/client gaps. Report sample
size and all durations; ten observations are a pilot, not a stable p95 study.

**Acceptance:** every pilot landing has fresh complete evidence matching its
landing tree; zero unexplained skips, swallowed failures or stale receipts.
Demonstrate operation under the measured arrival load, then retain a rolling
30-day merged-PR denominator, valid-evidence numerator, first-attempt failure
rate and queue/runtime distributions. A missing receipt raises an actionable
failure. Update the conflicting policy prose and text contracts together.
Freeze task merges, incorporate current main, qualify that exact effort
tree, and pass `Main promotion gate` before promotion.

If pilot reliability fails, hold cutover and fix the cause. If a post-cutover
runner outage prevents evidence, block landing and report capacity failure;
do not silently restore compile-only success. A temporary policy exception
must be an explicit user decision recorded with the affected candidates.

## 7. Guardrails and execution ledger

This effort does not remove behavior assertions, blanket-ignore flaky tests,
make an empty known-red catalog a pass, add application Developer toggles,
increase all deadlines, move CI load onto production voters, provision hosts
implicitly, or promise that software tests prove physical media/display
acceptance. Build-cache reuse is permitted; stale test verdict adoption is
not part of the requested fresh-execution contract.

| Milestone | State | Evidence / remaining work |
|---|---|---|
| Audit and plan | Complete | Read-only source inventory and Forgejo sample in the linked JSON; no controlled current full pass |
| M0 | Instrumentation built; measurement and qualification open · Codex | Independent clone on `codex/full-pr-suite-m0`, draft PR #1237 into `effort/full-pr-suite`; frozen `86f13850` runtime observations in the linked baseline report, with unfinished executions and fixture gaps explicit |
| M1 | Unclaimed | Current repair qualification and failure-safe cleanup |
| M2 | Unclaimed | Measured fixture setup optimizations |
| M3 | Unclaimed | Qualified execution/sharding and host budgets |
| M4 | Unclaimed | Automatic full candidates, strict gate and serialized landing |
| M5 | Unclaimed | Pilot, policy cutover and complete effort promotion |

The ledger records owner, task branch/PR, tested SHA/tree, commands, counts,
resource/timing evidence and acceptance outcome as work is claimed.
M0 instrumentation is implemented and under measurement; M1–M5 remain unclaimed.
A diagnostic observation does not qualify the complete execution gate.
