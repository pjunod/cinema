# Transport recovery CI — make both roles observable and qualification finish

**Status:** ready to build · **Owner:** Sol · **Written:** 2026-09-08 ·
**Reviewed source:** `9fcd151c98485707d2dc28195adf9c0c1e4a6d4c`

This is the implementation handoff for improving the transport-recovery
campaign. Read [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) and
[AGENTS.md](../../AGENTS.md) first, then execute the milestones in §8 in
order. This document defines proposed behavior; it does not claim the new
commands, reports, or jobs already exist.

The immediate deliverable is independent voter and learner execution,
durable diagnostics, phase timings, and separate CI results behind the
existing promotion gate. Measure runtime before changing execution profiles
or resource measurements. Keep production transport changes outside this
effort unless a measured defect requires a separately reviewed correction.

## 1. The problem this work must solve

The slow check is a Linux qualification binary, not a Rust unit test. It
starts separate harness node processes, uses real Hiqlite Raft/API TLS
transports, runs an external writer, destroys node 4's local state, and
requires snapshot recovery without losing acknowledged writes.

In [Forgejo run #1034][run-1034], inspected on 2026-09-08:

| Work | Observed elapsed time |
|---|---:|
| The 39 unit tests present in that revision | 0.08 seconds |
| Compilation and preparatory tests before campaign start | About 123 seconds |
| Voter warmup and 20 measured recoveries | About 100 minutes |
| Entire failed job | 1 hour 42 minutes 46 seconds |

Most cycles took approximately 280 seconds. The campaign completed all 20
voter recoveries and then failed on the resource comparison:

```text
Error: voter campaign leaked resources: node 1 threads floor rose 14 -> 17 (allowance 2)
```

The original comparison included a cold warmup baseline in the opening floor.
The reviewed source excludes that baseline from the normal opening band.
Do not reintroduce it. This arithmetic correction is covered by unit tests;
it is not evidence that the full revised campaign has passed.

The learner campaign follows a fallible voter call using `?`, so any voter
failure prevents learner execution. The job previously allowed 120 minutes;
the reviewed source allows 240. There are 20 measured cycles **plus one
warmup per role**, so the full run performs 42 recoveries. Approximately
200 minutes is a planning estimate, not a measured learner duration.

The pasted investigation reported no learner cycles anywhere in its reviewed
history. This handoff corroborates the cited voter run and the control flow,
not an independent census of every historical run. The newer main run
[#1100][run-1100] was queued at inspection; its outcome must be checked again
when implementation begins.

### 1.1 Correct interpretations to preserve

1. **The node executable is the harness.** `harness_executable()` returns
   the current `plurx-cluster-check` executable, launched with its `node`
   subcommand. It uses production components; it is not four `plurxd`
   daemons. Retain separate daemon validation.
2. **Node 4 does not accumulate in-process tasks across restarts.** The
   harness kills and reaps that process before replacing it. Its resource
   series describes successive fresh processes. Nodes 1–3 remain alive and
   can accumulate resources across recoveries.
3. **Heartbeat tasks are not the entire owned-task census.**
   `StartHeartbeatLoop` spawns the membership loop in node 4. The recorded
   owned-task count comes from Hiqlite's instrumented transport/executor
   ownership, not every Tokio task. Do not claim this counter proves that
   arbitrary heartbeat tasks were released.
4. **A smoke is not full qualification.** The current envelope only becomes
   an asserted verdict when its closing window contains ten samples: at
   least 19 measured cycles. Even 19 cycles remain short of the required
   20-cycle role qualification.
5. **Cancellation is neither failure evidence nor success evidence.**
   Record it separately, and never turn an incomplete role into a pass.

For the rationale and known detection limits of the resource statistic, read
the [resource-contract decision at the reviewed revision][resource-decision]
and its [measurement record][resource-baseline]. They are on the reviewed
remote main, even if an older local checkout does not contain them.

## 2. Build from current main and establish the compiler first

The authoring checkout was 140 commits behind remote main and had unrelated
documentation changes. Those files were deliberately preserved. Do not build
the implementation on that stale checkout or include its unrelated changes.

Use one `effort/transport-recovery-ci` integration branch based on current
remote main. Base reviewable `codex/` milestone branches on the current
effort and target their pull requests back to it. Re-read the repository's
workflow instructions if they changed after the reviewed SHA.

Before editing Rust, establish the pinned compiler loop described in
[AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md). Verify the actual
compiler rather than relying on the command name:

```bash
rustup show active-toolchain             # Resolve the repository pin.
rustc --version                          # Must report Rust 1.97.1 at this baseline.
cargo --version                         # Verify the executable being used.
```

If the checkout host cannot run that toolchain, transfer committed source
with `git archive` to the established compiler environment. Transfer neither
`.git` nor repository credentials. Keep its target directory warm. A source
archive does not contain uncommitted edits: record the exact committed SHA
being tested, bind it through `PLURX_BUILD_SHA` at build time, and repeat the
loop after porting onto a moved base.

Use `PLURX_EFFORT_COMMIT=1 git commit ...` only for effort task commits, as
required by the repository. Run focused behavioral regressions locally
before pushing. CI must not be used to discover compilation failures.

## 3. The existing qualification contract remains the boundary

### 3.1 Retain these invariants

| Contract | Required behavior |
|---|---|
| Platform | Real Linux processes and `/proc` resource evidence |
| Voter image | At least 177,119,232 SQLite bytes |
| Learner image | At least 88,559,616 SQLite bytes |
| Full qualification | 20 measured cycles and one warmup for each role |
| Persistent sources | The same node 1–3 processes survive all cycles of a role |
| Target | Node 4 is killed, reaped, its disposable state deleted, and restarted |
| Transport | Real existing Raft TLS and external API TLS writer paths |
| Data evidence | Snapshot identity, size/hash, image digest, and acknowledged-write checks |
| Writer | Existing readiness, cadence, gap, end-coverage, and liveness checks |
| Resources | Existing envelope algorithm, allowances, and warmup exclusion |
| Source identity | Exact build SHA bound before a cluster starts |
| Canonical artifact | Existing closed version-2 schema and offline Rust validation |
| Promotion | Complete current-candidate evidence; workflow run attempt `1` |

At the reviewed source, the recovery deadline is 1,500 seconds, resource
cleanup horizon is 60 seconds, sampling interval is 3 seconds, and stability
requires two repeated samples. Thread envelope allowance is 2; socket and
owned-task allowances are 0. Leave these constants unchanged in the initial
implementation. They are assertions, not speed controls.

### 3.2 Explicit exclusions prevent accidental loss of coverage

- Do not lower full qualification to three cycles or substitute smoke output
  for a canonical artifact.
- Do not divide one role's cycles among fresh clusters or stitch together
  successful cycles from different attempts. That destroys accumulation
  evidence and selects away failures.
- Do not fix resource failures by increasing allowances, following the
  latest count with a new baseline, or retrying until the run passes.
- Do not shrink qualification images, bypass admission, remove the writer,
  mock TLS, or eliminate integrity checks to meet a runtime target.
- Do not assume a new task survives SIGKILL because its `JoinHandle` was
  dropped. Cross-process ownership and in-process ownership are different.
- Do not change heartbeat lifecycle or vendored transport code merely to
  simplify the harness. §10 defines the follow-up measurement work.
- Do not add a cross-workflow receipt lookup, latest-green fallback, or
  global receipt cache in this effort. Preserve one workflow's exact
  candidate evidence and the first-attempt rule.
- Do not downgrade ordinary main-bound PR requirements while adding a fast
  development command. The existing effort workflow already provides the
  development/qualification boundary; §7 uses it.

## 4. One role runner supplies every entry point

### 4.1 Current entry points and seams

Re-verify these names against current source before editing:

| Existing seam | Location and responsibility |
|---|---|
| CLI dispatch | [`lib.rs`](../../crates/plurx-cluster-check/src/lib.rs): `run` |
| Full orchestration | [`transport_recovery.rs`](../../crates/plurx-cluster-check/src/transport_recovery.rs): `run_transport_recovery_campaign` |
| Bounded voter path | Same file: `voter_smoke_plan`, `run_transport_recovery_voter_smoke` |
| Cluster lifecycle | Same file: `run_role_campaign`, `start_recovery_cluster`, `admit_learner` |
| Workload | Same file: `exercise_role_campaign` |
| Resource verdict | Same file: `resource_envelopes`, `envelope_is_asserted`, `resource_envelopes_over_allowance` |
| Offline validation | Same file: `validate_transport_recovery_artifact`, `validate_role_campaign` |
| Node kill/reap | [`lib.rs`](../../crates/plurx-cluster-check/src/lib.rs): `NodeProcess::kill`, `ClusterProcesses::kill` |
| Build identity | [`topology.rs`](../../crates/plurx-cluster-check/src/topology.rs): `resolve_build_sha` |

The existing role function accepts executable, root, role, minimum image
bytes, and required cycle count. Reuse its recovery body. Introduce a typed
plan and an evidence/diagnostic sink around it; do not maintain independent
copies of the voter, learner, smoke, and qualification algorithms.

### 4.2 Proposed CLI contract

Add these commands. The names and flags below are the implementation target:

```text
plurx-cluster-check transport-recovery-role
  --role <voter|learner>
  --cycles <1..20>
  --execution-id <id>
  --output <role-report.json>
  --diagnostics-dir <directory>
  [--max-runtime-seconds <positive-integer>]

plurx-cluster-check transport-recovery-assemble
  --execution-id <id>
  --voter <voter-role-report.json>
  --learner <learner-role-report.json>
  --output <cluster-transport-recovery.json>

plurx-cluster-check validate-transport-recovery-role-stdin
```

`--role` and `--cycles` are explicit so a diagnostic request cannot silently
start a 200-minute run. Image size is derived from the role; it is not a user
flag. `--execution-id` associates both role reports with one invocation of
the qualification. It is a correlation value, not proof of CI provenance.
Accept 1–128 ASCII letters, digits, dots, underscores, or hyphens; reject
empty values and path separators. CI supplies a value derived from its run
ID and attempt. A local paired invocation generates one ID and passes it
to both roles.

The optional runtime limit bounds the entire role invocation, including
setup and teardown initiation. It is an orchestration limit and cannot
extend a recovery's existing 1,500-second deadline. Map it once to a
monotonic deadline. CI must provide a limit computed from the job's
remaining budget; local callers may omit it and retain the existing phase
bounds. Record the selected limit in diagnostics, not as a new allowance
in the canonical evidence contract.

Reject unknown flags, duplicate flags, invalid counts, unsupported roles,
colliding output paths, and invalid output destinations before spawning a
cluster. Reject non-Linux execution and missing/mismatched source identity
at the same point. Apply the existing artifact-output preparation rules so
a stale success file cannot remain after a failed new invocation. Never
delete unrelated files when preparing an output path.

Keep these existing interfaces compatible:

| Existing command | Behavior after this change |
|---|---|
| `transport-recovery [output]` | Full 20+20 execution; common role runner; assemble existing canonical artifact |
| `transport-recovery-voter-smoke [n]` | Existing default of 4 and range 1–20; common role runner; never emits canonical qualification |
| `validate-transport-recovery-stdin` | Existing canonical schema/semantic validator |
| `transport-recovery-writer` | Internal writer interface; unchanged |

Add `transport-recovery-learner-smoke [n]` with the same argument semantics
as the voter alias. The new aliases may choose diagnostic paths internally,
but must print the selected execution ID and absolute output locations.

For the full local wrapper, retain voter-first ordering for continuity but
capture its result instead of returning immediately. Clean up the voter
cluster, then attempt the learner. Aggregate both errors if both fail. An
external cancellation stops the whole invocation; failure of one role alone
does not. Never start the second cluster while known children from the
first still survive an unsuccessful cleanup.

### 4.3 Share validation without broadening canonical acceptance

Extract reusable validation of a role's shape, contiguous cycles, source
and target transport evidence, write integrity, fixed baseline, resource
records, and recomputed envelope. The role report validator calls it with
the declared bounded cycle count. The canonical validator calls it with
the fixed count of 20 and continues to reject incomplete qualification.

A 1–18-cycle run reports `resource_envelope_asserted: false`. A 19-cycle
run can assert the existing envelope but still has scope `smoke`. Only a
20-cycle, completely validated role has scope `qualification_role`.
Recompute these properties from the cycle count; never trust their labels.
Smoke success means its recovery/integrity assertions passed. It does not
mean every resource assertion was exercised.

Do not let this refactor retain the current smoke shortcut of checking only
the returned role/count/order at its outer boundary. A new role report must
pass the shared semantic validation for all evidence its scope claims.

## 5. Separate passing reports from durable failure diagnostics

### 5.1 A closed role report is an input to assembly

Create a separate role-report schema under `benchmarks/`, with its own
version starting at `1`. It must not masquerade as the existing combined
artifact. Proposed top-level fields are:

| Field | Meaning and validation |
|---|---|
| `schema_version`, `kind` | `1`, `transport_recovery_role`; deny unknown fields |
| `execution_id` | The identifier in §4.2; identical across paired reports |
| `build_sha`, `platform` | Actual resolved source identity and `linux` |
| `build_profile` | Actual compiled profile descriptor; not an arbitrary runtime label |
| `started_at_unix_ms`, `finished_at_unix_ms` | Ordered wall-clock bounds |
| `scope` | Derived `smoke` or `qualification_role` |
| `resource_envelope_asserted` | Derived from the existing window rule |
| `snapshot_policy`, `transport` | Existing fixed contract fields |
| Resource allowances/horizon/stability | Existing contract fields and values |
| `campaign` | Existing `RecoveryRoleCampaign` shape and complete cycle evidence |

Publish a passing role report atomically only after all requested cycles,
shared validation, and orderly cluster shutdown succeed. A failed or
cancelled run has diagnostics, but no new passing role report. A prior
passing output at the designated destination must have been removed before
the attempt starts. Treat an evidence-write failure as a failed run.

The assembler reads each input once into immutable bytes, validates its
closed schema and semantics, and then checks:

1. There is exactly one voter and one learner, both qualified for 20 cycles.
2. Execution ID equals the expected invocation and matches across roles.
3. Build SHA equals the assembler's resolved candidate SHA and matches both
   inputs. Profile, transport, resource, and snapshot contracts also match.
4. Role timestamps and every cycle's evidence are internally valid.
5. The constructed canonical artifact passes the existing offline validator.

Use the earliest role start and latest role finish for combined wall-clock
bounds. Concurrent role windows are legitimate. Compute each role's worst
durations from its cycles as before. Write the existing canonical version-2
artifact at the existing path. Do not add diagnostic fields to that closed
schema to avoid creating a needless canonical migration.

Role reports and their execution IDs do not authenticate a CI run. In CI,
the aggregate downloads only the two named artifacts from its own workflow
run, verifies current job outcomes, and uses the existing exact-tree receipt
builder. §7 defines that trust boundary.

### 5.2 Diagnostics survive an early return

Place diagnostics outside the temporary cluster root. Use a new directory
per execution and role, with create-new semantics to avoid mixing attempts.
The role's temporary databases can still be deleted normally.

Required files are `events.jsonl`, `cycles.jsonl`, and an atomically replaced
`summary.json`. Give these diagnostic formats an explicit version and kind;
never pass them to the qualification validator. A complete record contains:

| Record | Required contents |
|---|---|
| Execution start | Execution ID, build SHA/profile, role, requested cycles, contract values, platform, runner label if supplied |
| Phase start/end | Phase, cycle including warmup `0`, wall time, monotonic elapsed milliseconds, outcome |
| Process lifecycle | Node ID, PID and process-start identity, source/target kind, start/kill/reap result |
| Cycle checkpoint | Full completed `RecoveryCycleEvidence`, source leader, phase durations, resource sample |
| Resource sample | Every completed sampling attempt, monotonic offset, busy nodes, counts, stability progress |
| Terminal summary | Passed/failed/cancelled, completed count, failing phase/cycle, failure class, paths, cleanup result |

Flush complete JSONL records after each phase transition and sampling
attempt. At completed-cycle boundaries, synchronize the checkpoint before
proceeding. Include the warmup resource sample even though it is excluded
from the normal envelope bands. Use bounded records and avoid recording
image payloads, writer values, join tokens, or entire protocol requests.
Errors should retain useful context through safe identifiers and sanitized
messages. Admission phases particularly need names instead of token dumps.

A reader may ignore one truncated final JSONL line after abrupt termination;
it must retain earlier complete records and report that termination was
unclean. Missing terminal summary never means success. If diagnostics cannot
be written, fail with an evidence I/O error and keep any files already
written. Record the primary failure before cleanup; a cleanup failure must
not overwrite the original cause.

### 5.3 Cancellation has a bounded cleanup path

Handle SIGTERM/SIGINT in the top-level orchestration without installing
competing handlers in every node. Stop the writer, kill/reap owned child
processes as needed, record cancellation and cleanup outcomes, and exit
nonzero. Use one bounded cleanup budget, initially 30 seconds; retain
`kill_on_drop` as a fallback, not the sole cleanup proof.

Do not start the other role after external cancellation. Do not continue a
role after timing out a partially completed request/response exchange; its
protocol stream may no longer be synchronized. A timed-out role is torn
down, then the independent role can run if the parent was not cancelled.

CI must retain logs, checkpoints, and summaries with `if: always()` before
propagating the result. A job timeout or host loss can prevent these steps
from executing. Promise retention of completed local records, not guaranteed
upload after SIGKILL or host failure. Test graceful cancellation explicitly
and describe abrupt interruption honestly.

## 6. Timing makes the next optimization a measured decision

### 6.1 Required phase names and boundaries

Use monotonic time for durations and wall time for correlation. Keep the
existing recovery interval used by write and deadline validation unchanged;
new diagnostic timing must not silently redefine it.

| Phase | Starts and ends |
|---|---|
| `cluster_start` | Initial launch through ready/bootstrap/member convergence |
| `learner_admission` | Protocol activation through finalized join and heartbeat start |
| Admission subphases | Activation, token issue/redeem, learner ready, member convergence, open, forced heartbeat, finalize, loop start |
| `image_seed` | Seed request through validated source image evidence |
| `baseline_convergence` | Target applied-index and initial image convergence |
| `writer_ready` | Writer launch through readiness acknowledgement |
| `target_reset` | Target kill through reaping and disposable state removal |
| `snapshot_trigger` | First trigger write through new source snapshot observation |
| `snapshot_purge` | New snapshot observation through required purge index |
| `source_snapshot_hash` | Source file opening through verified size/digest |
| `target_start_open` | Replacement process launch through ready and Open |
| `target_install_observed` | Waiting for the intended inbound install to complete |
| `source_completion_observed` | Waiting for the source's matching outbound completion |
| `installed_snapshot_verify` | Waiting for and verifying the target snapshot file |
| `writer_stop` | Stop signal through report collection/process exit |
| `write_digest_verify` | Target acknowledged-write digest convergence |
| `image_digest_verify` | Recovered image query/hash through equality check |
| `resource_settle` | First sampling attempt through accepted sample or timeout |
| `shutdown` | Role teardown through all children reaped |

Some phases overlap, especially installed-file observation and recovery
status observation. Record intervals and parent cycle duration; do not sum
overlapping intervals and call the result elapsed time. Preserve source
transport transfer/install timings as distinct from harness wait durations.

Print a concise cycle summary and one end-of-role timing summary with
sample count, median, maximum, and total wall time. Separate compilation,
runner queue time when CI exposes it, cluster setup, warmup, and measured
cycles. Name failures by role, cycle, and phase.

**How to read it:** a long snapshot trigger suggests workload/commit cost;
a long transfer suggests transport or host capacity; a long install suggests
storage/SQLite work; a long digest phase suggests harness verification cost;
a long resource settle requires checking busy owners and sample movement.
These are investigation directions, not automatic diagnoses.

### 6.2 Profile and hashing experiments come after instrumentation

Run an initial three-cycle smoke for each role on an identified Linux runner
class. Record CPU allocation, memory limit, storage type, competing jobs,
build profile, source SHA, and phase timings. Do not infer a cold-build
bottleneck from old documentation: run #1034 spent only about two minutes
before starting the campaign.

The first optimization experiment compares the existing development build
with a dedicated optimized harness profile. Prefer a profile that keeps
debug assertions and useful symbols; avoid global release/LTO changes just
for this lane. Record actual compiled profile metadata in role reports and
update the evidence-validator executable path if a profile changes it.

Compare three measured cycles per role in each profile after its ordinary
warmup, using the same runner class and workload. Record build cost
separately. Ship a profile change only if the repeated measurements show a
useful runtime improvement, integrity checks pass, and the resource signal
does not become less interpretable. There is no promised percentage speedup.

Inspect `snapshot_file_by_id` and `wait_for_installed_snapshot_file` in the
[recovery source](../../crates/plurx-cluster-check/src/transport_recovery.rs).
They synchronously read/hash files in an async polling loop. If timing shows
material cost or supervision delay, make verification asynchronous from the
orchestrator's perspective, bounded, and cancellation-aware:

- Prefer the matching installation event or cheap size/readiness checks
  before hashing a complete candidate file.
- Keep the intended snapshot identity pinned. Preserve the existing case
  where the current snapshot pointer rotates after threshold writes.
- Hash a completed immutable snapshot once rather than rehashing unchanged
  mismatches every poll. A stable wrong digest must still fail.
- Bound blocking worker ownership and wait for termination. Dropping a
  `spawn_blocking` handle does not prove the worker stopped.
- Preserve writer supervision and the absolute recovery deadline while
  hashing. Do not introduce untracked background work into the measurement.

Keep optimizations as separate commits with before/after evidence. Do not
reduce sample intervals, force fake clock advancement through production
network code, or shorten integrity verification to obtain a faster number.

## 7. CI runs both roles behind the same promotion contract

### 7.1 Make targets distinguish contracts, smoke, and qualification

Refactor the existing [Makefile](../../Makefile) target into these surfaces:

| Target | Purpose |
|---|---|
| `cluster-transport-recovery-contracts` | Existing validator and vendored focused tests, including required test-count enforcement |
| `cluster-transport-recovery-voter-check` | One full 20-cycle voter role report plus diagnostics |
| `cluster-transport-recovery-learner-check` | One full 20-cycle learner role report plus diagnostics |
| `cluster-transport-recovery-smoke` | Three measured cycles per role by default; attempts both independently |
| `cluster-transport-recovery-check` | Backward-compatible contracts plus full local 20+20 and canonical artifact |

Define `RECOVERY_EXECUTION_ID`, `RECOVERY_OUTPUT_DIR`, and
`RECOVERY_SMOKE_CYCLES` as task-specific Make variables. Add optional
`RECOVERY_MAX_RUN_SECONDS` to pass the orchestration limit. The paired smoke
target may use a small orchestration helper, but must not be two shell
commands joined by `&&`: a voter failure must not skip learner diagnostics.
It must return failure if either role fails. Full role targets always require
20 cycles and cannot be shortened with the smoke variable.

Update the hard-pinned count in both Makefile and the corresponding
[operations contract](../../tests/operations/test_contracts.py) after adding
tests. It was 40 at the reviewed main, not the 31 in the older authoring
checkout and not the 39 in run #1034. Preserve named historical regressions
or update their mappings when a refactor changes their location.

### 7.2 Separate execution jobs and retain the aggregate job ID

Change the recovery portion of [ci.yml](../../.github/workflows/ci.yml) to
this dependency graph:

```text
 scope + preflight
         │
         ▼
 recovery contracts
         ├─────────────▶ voter role (20 cycles) ────┐
         └─────────────▶ learner role (20 cycles) ──┤
                                                   ▼
                                  cluster_transport_recovery
                                  validate + assemble + receipt
                                                   │
                                                   ▼
                                         Main promotion gate
```

Use explicit jobs `cluster_transport_recovery_contracts`,
`cluster_transport_recovery_voter`, and
`cluster_transport_recovery_learner`. Keep `cluster_transport_recovery` as
the aggregate so existing qualification consumers have a stable job name.
Neither role may depend on the other. A shared contract/build failure may
block both, but must be reported as that prerequisite failure. If a matrix
is used instead, set `fail-fast: false` and provide equally strict individual
role provenance; explicit jobs are the preferred implementation.

All selected jobs use the existing cluster-auth scope decision. The
aggregate runs under `always()` when that scope is selected, even if a role
failed, and succeeds only if contracts and both roles succeeded. Distinguish
a genuinely unselected surface from a selected job unexpectedly skipped.
The promotion gate must reject the latter.

Upload separately named voter/learner artifacts with reports, diagnostics,
and logs. The aggregate must download from the **current workflow run only**,
require both role artifacts, assemble and validate the canonical JSON, then
call [ci_lane_receipt.py](../../validation/ci_lane_receipt.py) using the
existing `cluster-transport-recovery` lane. Preserve:

- The canonical artifact path and existing aggregate receipt path.
- The validator consuming the same immutable bytes whose hash is recorded.
- Candidate SHA/tree equality and run-attempt `1` for successful evidence.
- Failure receipts when canonical evidence is absent.
- Fourteen-day retention for logs, diagnostics, role reports, and receipts.
- Final nonzero propagation after diagnostic upload on failures.

Keep [qualification.py](../../validation/qualification.py)'s required
aggregate job unless a necessary schema change is explicitly documented.
If new child results are added to its receipt, test that missing, cancelled,
failed, duplicated-role, and skipped selected children cannot qualify.
Do not make job splitting an opportunity to relax receipt semantics.

Each worker uses the pinned toolchain, same candidate SHA, profile, and
contract. Use existing cache helpers with role-specific writable cache lanes
when workers can share a host. Do not introduce shared mutable target state
or a writer lock that serializes the entire campaign. A compiler artifact
handoff can be added later if build measurements justify its complexity.

Role concurrency requires actual available capacity. Keep the current runner
labels until an inventory proves that equivalent isolated slots exist. Two
jobs on the same saturated two-core host may run slower and change drain
timing. Without capacity, independent jobs can queue and run sequentially;
the learner must still get its own result. Do not change runner allocation,
capacity, or live service configuration as an incidental code edit.

### 7.3 Give frozen qualification a chance to finish

Use the existing effort development model rather than inventing a second
receipt authority. Ordinary development PRs target an effort branch and
retain its fast compile/policy gate; run both-role smoke as focused local
evidence when recovery changes. Do not add a 30-minute smoke to every effort
PR's otherwise compile-only gate. A final effort-to-main PR remains full
qualification. Ordinary affected PRs directly targeting main retain full
recovery qualification in this implementation.

At workflow level, preserve automatic cancellation for superseded effort
development and ordinary PR runs, and for superseded main pushes. Disable
automatic superseding cancellation for final qualification PRs recognized
by the existing rule: base `main`, head `effort/**` or
`integration/*-into-main`. Preserve immutable tag evidence. The concurrency
expression must use event context; scope-job outputs are not available when
workflow concurrency is evaluated. A child job's setting cannot rescue it
from cancellation of its parent workflow.

| Event | Cancel superseded execution? | Full recovery requirement |
|---|---|---|
| Task PR into `effort/**` | Yes | Focused smoke locally; effort CI remains compile/policy |
| Ordinary affected PR into `main` | Yes | Both full roles before merge |
| `effort/**` PR into `main` | No | Both full roles and complete promotion receipt |
| `integration/*-into-main` PR into `main` | No | Both full roles and complete promotion receipt |
| Push to `main` | Yes | Existing exhaustive post-merge workflow |
| Immutable release tag | No | Preserve existing tag validation/release requirements |

Keep the group tied to the PR/ref and explicitly set `cancel-in-progress`
for qualification; merely omitting it is insufficient in Forgejo. Verify
behavior on the installed Forgejo version using the [official concurrency
reference][forgejo-concurrency] and an isolated validation workflow/test
case. Do not cancel real user runs to demonstrate it.

Freeze effort task merges before qualification and reserve an integration
window in which main can remain stable. This is an operating convention,
not a claim that YAML enforces a merge queue. If main or the effort moves,
the older run may finish for diagnosis, but its result is stale for
promotion. Check current source/base refs again when emitting promotion
evidence and immediately before merging, consistent with the existing
exact-tree policy. A stale run must not issue a current-candidate success
receipt. Test the moved-base and moved-head cases.

Do not indiscriminately set cancellation off for every workflow; queued
obsolete work could starve the candidate being qualified. Do not add an
automatic rerun-until-green loop. Periodic full campaigns can be added as a
separate follow-up; they would provide diagnostic evidence, not qualify a
different tree for promotion.

### 7.4 Budgets include evidence collection and runner queues

Initially retain 240 minutes for each full role job; do not lower the
learner's limit from a voter-only estimate. Give the aggregate a separate
bounded budget that includes artifact download, validation, and any build
needed for its validator. Document it from the measured warm and cold path.

Leave a reserved interval before the job-level timeout for cancellation,
cleanup, receipt writing, and upload. Implement a parent role-run budget
shorter than the job timeout rather than relying solely on a hard CI kill;
under a 240-minute job budget, reserve ten minutes after role execution.
Account for checkout, toolchain, build, and setup time already consumed.
The role's `--max-runtime-seconds` must therefore be no greater than the
remaining job time minus that reserve, with 230 minutes as an upper bound,
not a fresh 230-minute allowance after a long build. Refuse to start if no
positive execution budget remains. It must report an orchestration timeout
with the active phase, not claim a particular transport deadline failed.
Keep per-recovery deadlines unchanged.

After complete voter and learner measurements, tighten job budgets with
measured headroom. Report execution time separately from queue time; a
100-minute campaign waiting two hours for a runner is still a four-hour
feedback path.

## 8. Milestones and the evidence required to close each one

### 8.1 M0 — establish the base and preserve a baseline record

**Build:** Record current remote SHA and the latest completed campaign
outcome. Establish the compiler loop. Create the effort branch and source
map. Use existing retained logs as the old-behavior timing baseline; do not
start another 200-minute run merely to reproduce known orchestration.

**Acceptance:** pinned compiler version recorded; package check/Clippy/fmt
work; no unrelated checkout edits included; reviewed source differences
from this handoff understood.

### 8.2 M1 — independently execute and validate both roles

**Build:** Typed role plan, generic role CLI, compatible aliases, shared
semantic validation, closed role reports, and canonical assembly. Change
the full local wrapper to collect both independent role results.

**Regression evidence:** parser and scope boundaries; invalid input starts
no children; voter failure still invokes learner in an orchestration test;
the opposite failure remains visible; both failures are returned; smoke
reports cannot assemble; wrong roles, SHA, execution ID, profile, counts,
contract, or cycle evidence are refused; canonical v2 fixtures retain their
existing acceptance/rejection behavior.

**Acceptance:** one actual Linux recovery per role, each preceded by its
ordinary warmup, passes recovery and integrity assertions. Learner output
shows admission completion and cycle 1 completion. These runs are smoke
evidence, not leak qualification.

### 8.3 M2 — retain diagnostics and expose where time is spent

**Build:** Journals, per-cycle checkpoints, phase/subphase timing, process
identity, failure classification, safe errors, and bounded cancellation.
No resource-statistic or production-transport change in this milestone.

**Regression evidence:** inject an early admission failure, a recovery
failure after a completed checkpoint, a resource failure, and an evidence
write failure. Earlier records remain readable; no passing report or
canonical artifact survives. Test SIGTERM with real owned child processes
and verify reaping; do not only assert that a cleanup function was called.
Test that a truncated final journal line is reported without discarding
earlier complete records. Test that cancellation prevents the next role.

**Acceptance:** three-cycle voter and learner smokes produce phase timing,
resource series, complete checkpoints, and an explicit statement that the
resource envelope was not asserted. Demonstrate one bounded failure with
durable diagnostics and no remaining owned child processes.

### 8.4 M3 — split CI execution while retaining exact qualification

**Build:** Make target split, independent role jobs, aggregate assembly,
current-run artifact download, existing receipt integration, and updates to
the workflow/operations tests. Preserve the aggregate job ID.

**Regression evidence:** selected voter failure does not prevent learner
scheduling; selected learner skip/failure/cancellation prevents aggregate
success; missing reports fail; smoke reports fail; wrong-run/wrong-SHA
inputs fail; a rerun cannot become successful qualification evidence;
unselected cluster scope retains the intended skip behavior.

**Acceptance:** static workflow tests pass and a bounded CI orchestration
exercise demonstrates both outcomes being collected. Any synthetic or smoke
exercise is explicitly nonqualifying and cannot publish a success receipt
for the full lane. Full real qualification is required in M6.

### 8.5 M4 — preserve frozen candidate runs and reject stale evidence

**Build:** The conditional workflow cancellation policy from §7.3, current
candidate checks at qualification completion, and the operator procedure.
Keep ordinary main-bound qualification and effort development semantics.

**Regression evidence:** event table covers effort task PR, ordinary main
PR, effort promotion PR, integration promotion PR, main push, and tag.
Moved head/base after execution starts cannot produce a current promotion
receipt. Verify the installed Forgejo's actual cancellation behavior on an
isolated test case without disrupting real work.

**Acceptance:** a final qualification candidate can finish despite an
unrelated superseding workflow event; stale completion is retained for
diagnosis and refused for promotion. Document capacity and the required
integration freeze honestly.

### 8.6 M5 — measure optimization candidates and retain justified changes

**Build:** Perform the baseline/profile comparison in §6.2. Address hashing
only if it consumes material time or interferes with supervision. Record
the dominant phases for each role whether or not an optimization is kept.

**Regression evidence:** before/after results use equivalent runner class,
input sizes, cycle counts, and fixed contract. Any hashing change passes
snapshot rotation, digest mismatch, cancellation, and writer-liveness
regressions. Build-profile changes do not silently remove assertions or
point the receipt writer at a different validator binary.

**Acceptance:** an evidence table explains where the minutes go and the
measured effect of each retained change. A finding that no safe speedup was
demonstrated is acceptable; an unmeasured speedup claim is not.

### 8.7 M6 — qualify the complete current candidate

**Build:** Update maintained documentation, freeze milestone merges, bring
current main into the effort, and run the exact candidate through the
compiler/focused checks again. Open the final effort-to-main qualification
according to repository convention.

**Acceptance:** one first-attempt workflow on that current candidate yields
20 validated voter cycles, 20 validated learner cycles, both warmups,
complete diagnostics, canonical version-2 evidence accepted by the Rust
validator, the transport lane receipt, and the full promotion receipt.
Confirm persistent source process identity and target replacement from the
diagnostic records. All required promotion jobs must pass.

If this fails on real recovery or resource growth, retain the evidence,
identify the failing phase/owner, fix the cause, and qualify a new candidate.
Do not label the effort complete on the strength of M1–M5 or an older tree.

## 9. Validation commands and source touchpoints

### 9.1 Compiler and focused checks

Run these from the exact intended source snapshot with the pinned toolchain:

```bash
cargo fmt --all --check
cargo check --locked -p plurx-cluster-check --all-targets
cargo clippy --locked -p plurx-cluster-check --all-targets -- -D warnings
cargo test --locked -p plurx-cluster-check --lib transport_recovery::tests
python3 -m unittest discover -s tests/validation -p 'test_ci_lane_receipt.py'
python3 -m unittest discover -s tests/validation -p 'test_qualification.py'
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
make operations-check
make validation-lint
```

Run additional CLI/schema/orchestration test modules introduced by this
effort and the existing vendored focused tests exposed by the new contracts
target. The workspace `make ci-rust-gate` excludes `plurx-cluster-check`
from its unit-test invocation, so it does not replace the explicit package
test above. Run required workspace checks before final qualification.

Every corrective commit needs its focused command recorded and any required
regression mapping under `validation/` updated. Do not weaken contracts to
avoid updating their pinned expectations.

### 9.2 Proposed operator smoke commands

These examples become valid after M1/M2. Build SHA binding occurs while
compiling; exporting a different SHA only when running an old executable
does not rebind its compiled identity.

```bash
PLURX_BUILD_SHA="$(git rev-parse HEAD)" \
  cargo build --locked -p plurx-cluster-check

# On a source-only compiler host, use the archived SHA recorded by the caller
# instead of git rev-parse. Resolve the actual binary path if CARGO_TARGET_DIR
# is set by the runner/cache helpers.

./target/debug/plurx-cluster-check transport-recovery-role \
  --role learner --cycles 3 --execution-id local-learner-001 \
  --output target/validation/local-learner-001/learner-role.json \
  --diagnostics-dir target/validation/local-learner-001/learner-diagnostics

PLURX_BUILD_SHA="$(git rev-parse HEAD)" make cluster-transport-recovery-smoke \
  RECOVERY_EXECUTION_ID=local-both-001 \
  RECOVERY_OUTPUT_DIR=target/validation/local-both-001 \
  RECOVERY_SMOKE_CYCLES=3
```

**How to read it:** successful role reports contain three completed measured
cycles and scope `smoke`, with the envelope explicitly not asserted. A
failed invocation exits nonzero, leaves diagnostics, and has no passing
report at its designated output. Reusing the same diagnostic directory
must fail safely rather than append a second attempt to the first.

Use the generic role command with `--cycles 20` and one shared execution ID
for separate full-role execution, or the existing full Make target for a
local paired run. The CI aggregate alone issues the CI lane receipt.

### 9.3 Files that need coordinated review

| File or area | Expected responsibility |
|---|---|
| [`transport_recovery.rs`](../../crates/plurx-cluster-check/src/transport_recovery.rs) | Common plan/runner, shared semantic validation, report/assembly, diagnostics, timings |
| [`lib.rs`](../../crates/plurx-cluster-check/src/lib.rs) | CLI dispatch and narrowly needed process-lifecycle plumbing |
| [`main.rs`](../../crates/plurx-cluster-check/src/main.rs) | Top-level cancellation integration only if necessary |
| [`Makefile`](../../Makefile) | Distinct contracts, smoke, role, and compatibility targets |
| [`ci.yml`](../../.github/workflows/ci.yml) | Independent jobs, aggregate, retention, frozen qualification concurrency |
| [`ci_lane_receipt.py`](../../validation/ci_lane_receipt.py) | Preserve immutable-byte validation and exact-tree/first-attempt requirements |
| [`qualification.py`](../../validation/qualification.py) | Preserve required aggregate; current-candidate verification as needed |
| [`test_ci_lane_receipt.py`](../../tests/validation/test_ci_lane_receipt.py) | Refuse partial, stale, wrong-role/run, and rerun evidence |
| [`test_qualification.py`](../../tests/validation/test_qualification.py) | Required-job and candidate freshness behavior |
| [`test_contracts.py`](../../tests/operations/test_contracts.py) | Make/CI structure, count pins, scheduling and result propagation |
| [`cluster-transport-recovery.schema.json`](../../benchmarks/cluster-transport-recovery.schema.json) | Canonical v2 remains unchanged; new role schema sits alongside it |
| [`Cargo.toml`](../../Cargo.toml) | A measured dedicated profile only in M5 |
| [`VALIDATION.md`](../VALIDATION.md), [`DEVELOPMENT_PIPELINE.md`](../DEVELOPMENT_PIPELINE.md), [`CHEATSHEET.md`](../CHEATSHEET.md) | Actual commands, job meanings, receipt and freeze procedure |
| [`docs index`](../README.md) | This handoff and any companion records in the same commit |

New filenames beyond this table are implementation choices, but keep each
module's responsibility narrow. Do not move the entire cluster harness or
rewrite unrelated validation infrastructure to introduce the new runner.

## 10. Follow-up ownership work needs a separate measured contract

The initial effort preserves the envelope algorithm. It improves coverage,
diagnosis, scheduling, and measurement; it does not make `operation_owns_work`
mean that every underlying connection has been released.

If the resulting evidence warrants production lifecycle work, prepare a
separate change with these requirements:

1. Distinguish logical recovery completion from release of resources owned
   by that recovery, including peer/attempt identity.
2. Distinguish intentionally persistent reusable connections from obsolete
   connections; a pooled connection being alive is not itself a leak.
3. Account for leader movement and role changes when comparing persistent
   process resources. Do not solve this by resetting baselines every cycle.
4. Keep thread-pool variation separate from task/socket ownership evidence.
5. Use an actual retained transport/task ownership guard or equivalent
   injected defect to demonstrate detection through the measurement path,
   alongside synthetic validator tests. Ordinary heartbeat spawns are not
   a valid negative control for Hiqlite's owned-task counter.
6. Record the smallest detectable leak and known late/slow-leak misses.
   Zero allowances alone do not eliminate the statistic's blind spots.
7. Only then propose fewer cycles or tighter resource assertions, with
   detection evidence on both roles and representative runner conditions.

Do not advertise a three-cycle leak guarantee as a deliverable of this
handoff. The required deliverable is a full campaign that reaches both roles,
can complete on a stable candidate, retains useful failure evidence, and
reports enough timing to justify its next optimization.

## 11. Delivery checklist for the implementing agent

- [ ] M0: current base and working pinned compiler loop recorded.
- [ ] M1: independent voter and learner commands and strict role assembly.
- [ ] M2: durable checkpoints, phase timing, classified failures, cleanup proof.
- [ ] M3: independent CI jobs and unchanged aggregate qualification boundary.
- [ ] M4: frozen candidate cancellation policy and stale-candidate rejection.
- [ ] M5: measured runtime decomposition and justified optimization decisions.
- [ ] M6: full current-tree voter/learner campaign and promotion receipts.
- [ ] Maintained docs and index match implemented behavior.

The completion report must link the final candidate SHA, each milestone PR,
focused validation commands/results, both role reports, canonical artifact,
transport and promotion receipts, and before/after timing table. State any
remaining infrastructure limits or resource-detection blind spots. If full
qualification has not completed, list the exact missing evidence and leave
M6 open.

[run-1034]: http://192.168.4.7:3000/noirr/plurx/actions/runs/1034/jobs/9/attempt/1
[run-1100]: http://192.168.4.7:3000/noirr/plurx/actions/runs/1100/jobs/9
[resource-decision]: http://192.168.4.7:3000/noirr/plurx/src/commit/9fcd151c98485707d2dc28195adf9c0c1e4a6d4c/docs/cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md
[resource-baseline]: http://192.168.4.7:3000/noirr/plurx/src/commit/9fcd151c98485707d2dc28195adf9c0c1e4a6d4c/docs/cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md
[forgejo-concurrency]: https://forgejo.org/docs/latest/user/actions/reference/#concurrency
