# Raft fault testing — a bounded extension of the existing cluster harness

**Status:** revised after independent adversarial review; not implemented ·
**Written / revised:** 2026-09-30 ·
**Source inspected:** `acfa667ecc929bf4790752451360c6b462798b23` ·
**Decider:** Paul after technical review

This document is both the proposal and the build contract. It answers what
to borrow from raft-rupee, what to build in Plurx, and where to stop. Read
§1–§3 for the decision, §4–§8 for implementation, and §9–§11 for delivery
and review. All new commands, files, tests, and artifact fields below are
**proposed**, unless explicitly identified as existing.

Companion to [the development pipeline](../DEVELOPMENT_PIPELINE.md),
[the WAL generation repair](WAL_GENERATION_REPAIR_PLAN.md), and
[the transport recovery contract](TRANSPORT-RECOVERY-CI-IMPLEMENTATION-HANDOFF.md).
The repository's [contributor instructions](../../AGENTS.md) govern execution.
Re-verify source anchors against the intended implementation base; the
authoring checkout is evidence of inspected code, not a claim about remote
main or the deployed fleet.

[The review record](RAFT-FAULT-TESTING-REVIEW.md) preserves the original
request-changes verdict and the author disposition of every finding. These
amendments have not received a second independent approval.

## 1. Decision — improve the tests without building a simulation platform

Keep OpenRaft, Hiqlite, and the production persistence and transport paths.
Add three repeatable fault scenarios to the existing separate-process
harness, a bounded generated WAL regression, and enough evidence to rerun a
failure. Deliver in **three implementation pull requests and one promotion**
on `effort/raft-fault-testing`. Do not create a new service, crate, simulator
runtime, or testing framework.

The initial effort is complete when those tests, failure reports, replay
command, and existing validation integration work. Full deterministic
simulation is outside this effort, not a final milestone waiting to expand it.

### 1.1 The user's constraints are implementation requirements

| Constraint | Required implementation |
|---|---|
| Keep it finite | Three process scenarios; one WAL generator; three task PRs. No generic fault language, automatic minimizer, dashboard, or seed farm. |
| No software feature gates | No new product flag, readiness gate, capability gate, Cargo feature, or runtime rollout switch. Existing product behavior remains active. |
| Settings are not an approval mechanism | This is a developer test executable, so add no Settings card. If a separately proposed product option is later needed, it belongs in Developer with advisory requirements and an unrestricted saved enable/disable choice. |
| Preserve development velocity | Do not add jobs, schedules, required statuses, or process campaigns to the ordinary fast lane. Do not remove its current tests either. |
| Maintain the quality bar | Focused local regressions, existing hooks, existing effort/main checks, exact-source evidence, and current qualification requirements remain mandatory. |

Fault injection runs only against disposable processes created by the test
executable. Reuse its existing validation controls. Do not add remotely
accessible fault endpoints or settings to `plurxd`. Test command arguments
select the test to execute; they are not product feature gates.

### 1.2 Why this is useful despite the tests already in the tree

The existing suite has deliberate, named failure sequences. The addition
varies bounded choices inside a few sequences, preserves what actually
happened, and checks acknowledged data independently of leader health.
The expected value is finding ordering and recovery regressions in Plurx's
integration, not independently proving the Raft algorithm.

| Option | Value | Cost and decision |
|---|---|---|
| Leave the suite unchanged | Existing incident regressions and substantial recovery coverage | Valid baseline; misses inexpensive variation and a shared replay record. |
| Bounded scenarios plus WAL generation | Exercises real production components with inspectable failures | **Selected.** Small extension using existing controls and checks. |
| Adopt raft-rupee | Readable pure-core example | Different consensus/storage implementation; its successes would not validate Plurx. Rejected. |
| Fully deterministic OpenRaft/Hiqlite runtime | Potentially reproduces internal interleavings exactly | Requires control over async scheduling, clocks, I/O and persistence. Separate future decision only. |

The recommendation comes from source inspection of
[raft-rupee at `87c5d33`][rupee]. Its simulator demonstrates seeded fault
selection, but schedules nodes through ordinary `HashMap` iteration and
ignores `Action::Apply`; its leader-append-only check is a stub, and the
server ignores snapshot persistence. These are reasons to borrow the idea
without importing its engine, harness, or correctness claims. Its tests
were not run for this proposal. See [simulator][rupee-sim],
[invariants][rupee-invariants], and [server][rupee-server].

## 2. Existing implementation — reuse these boundaries

| Existing source | Verified responsibility | Use in this effort |
|---|---|---|
| [Workspace manifest](../../Cargo.toml), [lockfile](../../Cargo.lock), [Hiqlite manifest](../../vendor/hiqlite/Cargo.toml) | Vendored Hiqlite 0.14.0 and resolved OpenRaft 0.9.25 | Keep dependency versions and engine unchanged. |
| [Harness library](../../crates/plurx-cluster-check/src/lib.rs) | `run`, `ClusterProcesses`, `NodeProcess`, `Request`, `Response` | Add a command and sibling module; reuse launch, control and cleanup. |
| [Harness integration tests](../../crates/plurx-cluster-check/tests/harness.rs) | One-voter tests for real child protocol and validators | Follow this split: cheap contracts in tests, quorum scenarios in explicit commands. |
| [Raft client](../../vendor/hiqlite/src/network/raft_client.rs), [Raft server](../../vendor/hiqlite/src/network/raft_server.rs) | Existing validation partition control on outbound/inbound Raft paths | Use whole-node isolation after confirming an observable partition; no new transport layer. |
| [Transport recovery](../../crates/plurx-cluster-check/src/transport_recovery.rs) | Linux processes, actual TLS transports, snapshot recovery and acknowledged-write evidence | Preserve existing full qualification and schemas; borrow narrow helpers where safe. |
| [Recovery diagnostics](../../crates/plurx-cluster-check/src/transport_recovery_diagnostics.rs) | Process ownership, failure classification and cleanup evidence | Reuse narrowly; avoid refactoring the existing campaign to obtain an abstraction. |
| [WAL implementation and tests](../../vendor/hiqlite-wal/src/wal.rs) | Real WAL files, mmap/memo refresh, purge and suffix rewrite regressions | Add bounded generated sequences alongside existing tests. |
| [Makefile](../../Makefile), [validation catalog](../../validation/points.toml) | Existing focused checks and full cluster surfaces | Register new checks in existing surfaces without expanding ordinary PR runtime work. |

The harness starts more copies of `plurx-cluster-check` with its `node`
subcommand. Those processes embed Hiqlite and production store code. They
are not complete `plurxd` daemons; existing daemon validation stays separate.

These interfaces already exist in the harness; preserve their contracts:

```rust
pub async fn request(&mut self, node_id: u64, request: Request)
    -> Result<Response>;
pub async fn kill(&mut self, node_id: u64) -> Result<()>;
pub async fn spawn_node(&mut self, executable: &Path, launch: NodeLaunch)
    -> Result<()>;
pub async fn wait_for_equal_dumps(&mut self) -> Result<()>;
```

Relevant existing request variants are `PutSetting { key, value }`,
`ReadLocalSetting { key }`, `Dump`, `RecoveryStatus`, `TriggerElection`, and
`SetRaftPartitioned { partitioned }`, plus `Bootstrap` and
`WriteWithoutQuorum`. The partition control keeps the node's control
and client API usable while isolating Raft. Do not assume its acknowledgement
proves all old work has drained; establish the scenario preconditions in §5.

## 3. Scope and evidence — say exactly what can be repeated

### 3.1 Three distinct claims

| Evidence | Guarantee | Explicit limit |
|---|---|---|
| Generated plan | Same generator version, scenario and seed produce identical canonical plan bytes | No control over the OS, elections or async scheduling. |
| Process replay | Re-executes the recorded sequence, values, target choices and predicates on fresh nodes | Internal timing and failure occurrence can differ; report that difference. |
| WAL sequence replay | Executes the same ordered calls against the actual WAL with the same generated bytes | Does not reproduce concurrent mmap races, torn sectors or power loss. |

Do not name process results `deterministic_simulation`, or infer one-leader-
per-term safety from periodic leader samples. Observation timestamps are
diagnostic; independent process clocks do not define a total causal order.
Controller sequence numbers and request/response links define the recorded
order. A passing run proves the selected scenario's observed contracts only.

### 3.2 Exclusions keep the project bounded

Do not implement disk-full injection, torn-write emulation, clock skew,
Byzantine faults, arbitrary RPC reordering, membership fuzzing, snapshot
interruption, arbitrary workload linearizability checking, or automated
trace shrinking. TCP/TLS socket manipulation is not a substitute for
message-level control. Do not copy production Raft behavior into a model.

Existing learner, serving-fence, bounded-read, snapshot and resource-leak
campaigns continue to own those contracts. This effort does not claim fresh
coverage for them merely because the new scenarios use Raft.

No production repair is bundled speculatively. If a new test exposes a
defect, retain the failing regression and make a separately reviewable
`fix(...)` task with its actual scope and required validation.

## 4. Implementation shape — one controller module and ordinary tests

Add `fault_campaign.rs` beside the existing harness modules. Keep the plan
types, generator, executor and artifact validator there initially. Split
only if the implementation becomes hard to review; no plugin interfaces
or general-purpose scheduler are needed for three scenarios.

```text
 scenario + seed ──▶ versioned plan ──▶ existing child-process harness
                           │                      │
                           │                 commands + responses
                           ▼                      ▼
                      plan.json             events.jsonl
                                                  │
                                  acknowledged-data / progress checks
                                                  │
                                                  ▼
                                             result.json

 WAL seed ──▶ ordered operations ──▶ real WalFileSet + independent byte map
```

### 4.1 Stable choices without a new random dependency

The seed is a `u64`, serialized as exactly 16 hexadecimal digits. Use a
small explicitly specified generator such as SplitMix64 with wrapping
arithmetic and golden output tests; this is test variation, not security.
Freeze its algorithm and choice order under `generator_version = 1`.
Hash-map iteration, wall time, random UUIDs and ephemeral ports must not
feed plan generation. Sort any set before using it to choose a target.

V1 chooses only payload bytes/lengths and one of the two eligible followers.
Fault cut points and write counts remain fixed in this first corpus. Require each
scenario to execute every mandatory phase regardless of those choices.
There is no probability of accidentally skipping the fault.

Persist the generated plan before starting children. It contains the
scenario, seed, generator version, topology, workload, fault selector,
readiness predicates and deadline values. Use typed structures with
ordered vectors and a canonical serializer; checksum the resulting bytes.
Keep ports, PIDs and timestamps in the event record, outside the plan hash.

### 4.2 Keep control available during a fault

Use the existing `ClusterProcesses` ownership and launch methods. Do not
hold its mutable request path across a write that prevents the controller
from injecting or healing the fault. V1 can sequence a completed write,
inject the fault, then perform bounded requests; it does not need a new
concurrent writer process or multiplexed child protocol.

The child protocol is an untagged request/response stream. Use existing
`WriteWithoutQuorum` for §5.3: its 17-second response timeout exceeds the
production write retry envelope of approximately 15.4 seconds, whereas
ordinary `PutSetting` receives only 12 seconds at this baseline. Record its
fixed key `no-quorum.write` and value `must-not-ack` as the scenario's one
explicit exception to the case-prefix namespace. Fresh databases make the
key unique to this run. Re-verify these budgets if production retries change.

If a controller timeout, cancellation during a request, partial read or
decode error leaves response framing uncertain, poison that channel. Fail
the active case, terminate/reap that owned child during cleanup, and do not
send another request on its stream. A fully decoded terminal store error
leaves the stream aligned and allows the planned recovery. Test that a
delayed first reply can never become the next command's success. This is
a narrow channel-state correction, not a new RPC protocol.

Always choose the fault target after observing a healthy baseline. Record
the resolved node ID, term and role. A replay uses that recorded node ID
and establishes its role precondition; if that cannot be established within
the existing deadline, record `inconclusive` with `replay_precondition`
instead of silently choosing a different node. `TriggerElection` can help
establish a leader but cannot be treated as proof that it won.

## 5. The three process scenarios

Each case starts a fresh three-voter cluster in an owned temporary directory,
uses unique immutable keys, and ends by healing faults, verifying data on
all three voters, and killing/reaping its children. Run cases serially so
resource contention is not an uncontrolled fourth fault.

Use the existing startup/bootstrap choreography to initialize every node's
store handle before data requests, including after restart. Register each
child for cleanup before awaiting readiness; §8 covers setup failures too.

The ordinary workload is **16 acknowledged baseline writes, 8 writes during
the fault phase where quorum exists, and 8 acknowledged writes after heal**.
Vary payloads from 1 to 4,096 bytes. Those are test parameters, not performance
claims. Require the exact expected operation count; an empty workload fails.

### 5.1 Leader crash and durable restart

1. Establish one observed leader and equal baseline keys on all voters.
2. Record its ID and kill/reap it through the existing process wrapper.
   Keep its state directory intact; this is a durable restart test.
3. Require a surviving voter to become leader and acknowledge eight new
   immutable writes through the normal store path.
4. Restart the old leader with its original launch identity and data.
5. After convergence, acknowledge eight more writes and compare the complete
   expected key set on every voter.

**Pass:** every acknowledged key/value survives; the majority progresses
while the old leader is down; the returning node catches up. Merely observing
a new leader does not pass. This does not claim a crash at a specific fsync
instruction or a lost-response window.

### 5.2 Follower isolation and catch-up

1. Establish the baseline and choose a follower from sorted eligible IDs.
2. Set its existing Raft partition control, keeping control/API access alive.
3. Write a unique witness through the healthy majority. Require its local
   absence on the follower throughout a sampled observation interval of at
   least 2 seconds: probe after every remaining fault-phase write, at
   intervals no greater than 100 ms while otherwise idle, and immediately
   before healing. The final probe must follow all eight acknowledged
   fault-phase writes. Bound each probe by the existing request deadline;
   a failed probe is not an observation of absence.
4. Heal the follower and require all acknowledged writes to appear locally.
5. Execute the post-heal writes and compare all nodes again.

**Pass:** the fault was observed, the majority made progress, and the
follower converged without lost acknowledged data. Require both the witness
and every other fault-phase key to remain absent at the final pre-heal
probe. If a key appears during the sampled interval, fail the fault-effect
check; do not turn this into a healthy-cluster pass. This proves observed
isolation over those samples, not absence at every internal instant. Add a
negative control with witness arrival during the interval. V1 does not
force snapshot compaction or claim snapshot-path coverage.

### 5.3 Majority absence and recovery

1. Establish the baseline and record the leader.
2. Kill/reap both other voters, preserving their state directories. The
   remaining process has no voting majority.
3. Send `WriteWithoutQuorum` to that process, using §4.2's existing deadline
   and recorded fixed key/value. A success response is a safety failure.
   Require a decoded terminal error recognized by the existing quorum-error
   contract before counting this phase as exercised. An unrelated SQL or
   protocol error fails the case. The operation remains potentially applied
   even after a quorum error; do not assert it can never appear after heal.
   A controller timeout records an unknown write and fails the case with
   cleanup under §4.2; it does not proceed on an uncertain response stream.
4. Restart the other voters with their original data and identities; require
   restored quorum and convergence.
5. Acknowledge eight post-heal writes. All baseline and post-heal writes
   must exist on every node. The unknown write may be absent everywhere or
   present identically everywhere after settlement; mixed results fail.

**Pass:** no fresh write is acknowledged without quorum; prior acknowledgements
survive; progress resumes. Fault-phase acknowledged-write minimum is zero
for this scenario alone. Do not delete data to make recovery succeed.

## 6. The oracle — acknowledgements are obligations

Use keys such as `fault-v1/<case>/<op-id>` and deterministic values; isolate
each case with a fresh database. Submit each logical write once. Retries
must not introduce different values under the same key or convert an
ambiguous earlier result into an asserted failure.

| Observation | Meaning | Final assertion |
|---|---|---|
| Store success received | Acknowledged durable obligation | Exact value exists on all recovered voters. |
| Refusal before dispatch | Operation was not issued | Record the reason; it cannot satisfy a required write count. |
| Timeout, disconnect, or error after dispatch | Outcome may be unknown | Presence is allowed; conflicting values and settled replica disagreement fail. |
| Node reports healthy/leader | Progress hint | Does not establish data durability or convergence. |

Stop the test writer before final comparison. Obtain each node's existing
`Request::Dump`, extract the complete `settings` key/value set for the case
prefix plus the explicitly registered no-quorum key, and compare exact
sorted sets under the existing convergence deadline. Reject unexpected
prefix keys as well as missing or changed expected values. Retain per-key
reads for partition witnesses. Derive result hashes from the returned,
filtered bytes; do not assume `Dump`'s separately queried whole-database
digest and dump describe an atomic cut while background writes continue.
Background settings outside the selected namespace do not enter this oracle.

Require all three launched voter generations to supply successful final
observations; missing nodes cannot be skipped. After healing, complete the
post-heal writes and allow convergence to settle before judging the unknown
write. The observed unknown-key presence/value must agree on all voters.

The oracle owns an independent expected map built from issued operations
and recorded responses, not from the final database. Test it with fabricated
missing data after an observed acknowledgement, changed values, unexpected
prefix keys, missing voters, divergent replicas, zero-work runs, and a
successful no-quorum write. Every such corruption must be rejected. A lost
response after dispatch is an ambiguous operation, not missing acknowledged
data.
This supplies negative controls without shipping deliberate product faults.

## 7. WAL generation — exercise the implementation that previously failed

Extend the unit tests in [wal.rs](../../vendor/hiqlite-wal/src/wal.rs).
Retain the existing named regressions:

- `full_purge_replaces_stale_mmap_and_memo_across_reused_wal_numbers`
- `reused_wal_number_rejects_a_memo_from_the_purged_generation`
- `suffix_rewrite_renews_the_incarnation_before_memo_reuse`

Add `generated_wal_sequences_match_reference_bytes`. Run **32 fixed seeds,
64 generated operations per seed**, with bounded payloads and unique temporary
directories. Every seed starts with a mandatory prefix that establishes a
reader mmap/memo, purges/recreates a WAL number, and rewrites a suffix with
different record sizes. Random variation supplements those transitions.

Use actual `WalFileSet` operations for append, rollover, front/full purge,
suffix rewrite, reader refresh, and valid range reads. Maintain a separate
ordered map of expected retained log IDs and bytes. After each mutation,
refresh the reader using the production method and compare complete retained
contents, then exercise a selected valid subrange. Reject missing, duplicated,
stale or changed entries. The model must not call WAL helpers to compute its
expected contents. Only generate operations whose WAL preconditions hold.

In the model, front purge removes IDs strictly below its cutoff, suffix
truncation removes IDs at or above its cutoff, and full purge uses a cutoff
beyond the retained maximum. Append follows the current legal tail; an
empty generation establishes its next base explicitly. Keep one reader and
its memo alive across mutations and refreshes, clearing them only as the
actual production refresh requires. Recreating them after each operation
would erase the stale-generation condition this test is meant to exercise.

On failure print the seed, generator version, operation number and full
bounded sequence. Support replaying one seed with a test-only environment
variable, `PLURX_WAL_TEST_SEED`, parsed strictly; malformed input fails.
Do not add a production environment control or dependency for this.

This is ordered property-style testing of real file operations. It does not
simulate kernel writeback or promise power-loss durability. If legal operation
semantics require substantial WAL refactoring to expose, keep the tests in
the existing internal test module rather than widening production APIs.

## 8. Evidence and replay — three small files per case

Write into an absent output directory claimed before launch. Reject an
existing result directory so stale success cannot be reused. Persist the
plan first, append and flush an event for every issue/response/fault/cleanup
transition, and publish the final result with a temporary file plus rename.
A missing or partial result is never success.

| File | Required content |
|---|---|
| `plan.json` | Schema/generator versions, scenario, seed, workload, topology, fault selection, predicates and deadlines; canonical plan hash. |
| `events.jsonl` | Contiguous controller sequence numbers; operation IDs; issue/response links; resolved targets; process generations; monotonic elapsed times; observed fault and recovery transitions. |
| `result.json` | Plan/event hashes, source identity, executable SHA-256, binary profile, toolchain, platform, verdict, first failure, phase timings, write counts, per-node observed values/digests, replay comparison, cleanup result. |

Use a closed version-1 JSON schema in the existing `benchmarks` schema
directory and a Rust offline validator. Do not redefine or claim compatibility
with existing transport-recovery qualification schemas. The validator checks
hashes, versions, required phases, exact required counts, response linkage,
oracle outcomes and cleanup. A JSON shape check alone is insufficient.

Bind `PLURX_BUILD_SHA` through the existing
[build script](../../crates/plurx-cluster-check/build.rs) before compilation.
Record the embedded identity for controller and children. Official evidence
requires a clean committed source tree and matching binaries; a caller-
supplied SHA alone is not attestation. In an archive build, record archive
digest and the originating committed SHA. Developer runs with dirty/unbound
source remain usable but explicitly unqualified. Never compare two source
revisions as if one replay proved the other.

Hash the actual controller executable and the binary used for every child
launch; record those digests with the existing embedded build identity.
Use the clean-source verification pattern in
[named_runner.rs](../../crates/plurx-cluster-check/src/named_runner.rs).
The authoritative promotion receipt remains the one produced by
[validation/qualification.py](../../validation/qualification.py) through
the existing workflow. Link that receipt and these supplemental bundles in
the promotion record. Do not create another signer, attestation service,
or qualification schema; a clean-source claim in a bundle alone cannot
replace the existing receipt.

**Verdicts:** `pass` · `fail` · `inconclusive` · `cancelled`. Only `pass`
returns success. Failed invariants and unmet required progress/fault effects
are `fail`. Unsupported platform, failure before a valid baseline, or unmet
replay role preconditions are `inconclusive`, with the original error retained.
Cancellation remains visible. None may be automatically rerun until green.

Replay reads the original plan and resolved targets, validates its hashes
and versions, starts fresh disposable nodes, and creates a new output
directory. It reruns the scenario checks and reports `failure_reproduced`,
`failure_not_reproduced`, `different_failure`, or `precondition_not_met`
when the original was a failure. Compare a stable failure code plus scenario
phase and operation ID, not timestamped error strings. Retain both runs.
A non-reproducing rerun does not erase the original failure.

The controller must heal/resume what it owns and kill/reap all children on
ordinary failure and cancellation. Give cleanup its own 60-second budget;
cleanup failure prevents `pass`. Record PID plus process start identity,
never kill unrelated processes by name, and never remove outside the owned
temporary root. Preserve failed-run diagnostics. Reuse existing ownership
and cleanup patterns; a hard controller kill cannot promise in-process
cleanup, so retain an ownership manifest and document manual cleanup.

M2 must close the existing helper gaps narrowly: `kill_all` currently drops
individual errors, and startup/dynamic spawn may fail before publishing a
child into the returned cluster. Register ownership immediately after spawn,
retain it on readiness failure, and collect each termination/reaping result.
Keep the first scenario error separate from cleanup errors. Add contracts
for readiness failure after spawn and a failed termination/reaping attempt,
as well as ordinary cancellation. `kill_on_drop` is a backstop, not proof
of successful cleanup. Changes to shared helpers require their existing
focused lifecycle tests too.

## 9. Build sequence — three tasks, with explicit acceptance

Work serially: tasks share the harness, Makefile and catalog. The ownership
table is for review and scope control, not permission for independent merges
to main. Every task updates this document's execution table and index status
only as evidence warrants.

| Task | Owned changes | Acceptance before its PR is ready |
|---|---|---|
| M1: plans, oracle and report | Proposed `crates/plurx-cluster-check/src/fault_campaign.rs`; small `lib.rs` registration; proposed `benchmarks/cluster-fault-campaign.schema.json` | Golden plan bytes match across fresh invocations; independent oracle rejects all negative controls; schema/semantic validator rejects missing phases, bad hashes and zero-work success. |
| M2: three scenarios and replay | Same module and narrow existing harness ownership/channel helpers; proposed `crates/plurx-cluster-check/tests/fault_campaign.rs` | Each scenario runs with seed 0; replay preserves plan/targets; delayed replies cannot satisfy later requests; setup failure and cleanup errors are retained; a controlled oracle failure is detected by replay; missing baseline is non-success. |
| M3: WAL generation and integration | Existing WAL test module; Makefile; catalog and focused validation contracts; final documentation | 32×64 WAL corpus passes; all three scenarios pass at seeds 0 and 1; new tests are selected by existing cluster validation; ordinary fast-lane scope remains unchanged; current-candidate qualification requirements are met before promotion. |

M1 includes a short source recheck and proof that the three existing fault
controls can support §5. Do not make this a standalone research milestone.
If M2 needs a production transport rewrite, a new scheduler, or a new RPC
protocol, stop that expansion and revise the proposal with the concrete
blocker. Do not quietly turn it into a fourth infrastructure task.

### 9.1 Compiler and focused commands

Before any Rust edit, establish the pinned compiler using
[the source-only compile loop](../ci/AGENT-COMPILE-LOOP.md). At this baseline
`rustc --version` must report 1.97.1. Archive committed source, transfer no
`.git` or credentials, keep the target directory warm, and rerun against the
exact intended branch after a base change. The documentation-only proposal
does not constitute compiler or runtime evidence.

Existing commands for implementation checks:

```bash
rustc --version                          # Verify the repository toolchain.
cargo fmt --all -- --check               # Check the exact source tree.
cargo check --locked -p plurx-cluster-check --all-targets
cargo clippy --locked -p plurx-cluster-check --all-targets -- -D warnings
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
make validation-lint                     # Verify governed file ownership.
```

Run the repository's affected vendored Clippy/format checks for the WAL
change too; workspace formatting alone does not establish that excluded
vendor sources were checked. Replicated `plurx-core` tests must use
`make unit-core` or `--features hiqlite-store` if any core code is touched.

The following are the proposed interface, available only after M1–M3:

```bash
# Fast contracts; no multi-voter processes in this filtered unit group.
cargo test --locked -p plurx-cluster-check fault_campaign::tests --lib

# One explicit Linux scenario; CASE is one of the three names below.
cargo run --locked -p plurx-cluster-check -- \
  fault-campaign --case leader-restart --seed 0000000000000000 \
  --output target/validation/fault-leader-0

# Other case names: follower-partition, majority-loss.
# Replay validates the original bundle and writes a separate result.
cargo run --locked -p plurx-cluster-check -- \
  fault-replay --input target/validation/fault-leader-0 \
  --output target/validation/fault-leader-0-replay

# Check a saved bundle without launching nodes.
cargo run --locked -p plurx-cluster-check -- \
  validate-fault-campaign --input target/validation/fault-leader-0

# The actual WAL implementation, using its internal test module.
cargo test --locked --manifest-path vendor/hiqlite-wal/Cargo.toml \
  wal::tests::generated_wal_sequences_match_reference_bytes --lib -- --exact
```

Wrap focused test commands with the existing `scripts/require-test-count`
when adding Make targets so a misspelled filter cannot return green with
zero tests. Reject unknown CLI fields/cases, malformed seeds, unsupported
versions and output collisions before launching children.

### 9.2 Finite runtime and complexity budgets

| Work | Initial budget | Interpretation |
|---|---|---|
| Pure plan/oracle/report tests | Target 5 seconds warm | Measurement target; no real cluster or sleeps. |
| WAL corpus | Target 30 seconds warm | Measure on the compiler host; print seed and sequence on failure. |
| One process scenario | 300-second outer deadline plus 60-second cleanup | Keep existing inner startup/request/convergence deadlines; outer timeout is a failure after valid baseline. |
| Routine local smoke | Three cases, seed 0, serial | At most 18 minutes including cleanup; expected faster, to be measured. |
| Effort acceptance corpus | Three cases × seeds 0 and 1, serial | At most 36 minutes including cleanup, separate from existing qualification. |

These are proposed budgets, not measurements. Do not borrow the existing
large-snapshot campaign's runtime or lower its requirements to meet them.
If the small corpus is slow, inspect orchestration first. Do not add nightly
seed exploration or expand the corpus in this effort. A newly discovered
failure earns one named regression, not an unbounded campaign.

## 10. CI/CD — use the process already paid for

The [development pipeline](../DEVELOPMENT_PIPELINE.md) has amendments dated
2026-09-10/13 and 2026-09-20. The current
[main fast lane](../../.github/workflows/main-fast-lane.yml) runs on ready
main-bound PRs, cancels superseded work, and includes Clippy and `make unit`.
Full CI is manual or release-triggered; routine runtime schedules were
removed. Preserve that arrangement. Do not resurrect older automatic
qualification/deployment behavior described later in the pipeline document.

In particular, `make unit` explicitly excludes `plurx-cluster-check`.
Do not assume these new harness tests run there or remove that exclusion.
M3 adds focused plan/oracle/report tests to the existing
`cluster-harness-check`, the WAL test to `cluster-wal-check`, and a proposed
`cluster-fault-smoke` Make target invoked from the existing `cluster-check`
on Linux. The initial process corpus supports **Linux only**. The existing
catalog also runs `cluster-check` on Darwin: preserve all its current checks
and print an explicit `cluster-fault-smoke: not applicable on Darwin`
for this added substep, without producing a passing fault-campaign artifact.
Direct invocation of the Linux-only command on Darwin remains non-success;
only the parent check's platform dispatch omits it. Official acceptance of
this effort requires the Linux corpus. Add a static integration contract
covering both branches so Darwin cannot accidentally lose its existing checks.

The smoke runs the three seed-0 cases with fresh output directories. Map
new source/schema paths to existing appropriate catalog points. Do not add
these process commands to a fast-lane job or a new recurring workflow.

For task PRs, run the focused local regression and compile checks, commit
normally with the tracked hook, and require `Effort development gate`.
For final promotion, freeze task merges, integrate current main, and use
the current required main/promotion checks and qualification receipt for
that exact candidate. The new corpus supplements the applicable existing
checks; it is not a replacement receipt or a new approval service.

Open the main-bound PR as draft, obtain the existing single adversarial
review, address it, and then mark ready. Record the commands, source SHA,
toolchain, runtime and evidence location. Keep user-observable corrections
under `fix(...)` or `perf(...)` and include real
`Regression-Test: <path>::<test name>` lines in both PR and landing message.
For API merges, preserve them through `MergeMessageField`. Test-only task
subjects should describe their actual scope; never use that label to hide
a production behavior correction.

No workflow YAML change is planned. If catalog integration unexpectedly
changes ordinary fast-lane selection, fix that mapping before proceeding;
do not silently widen the expensive lane. Recheck the actual pipeline at
build time if it has evolved since the inspected source.

## 11. Review questions, completion and handoff

Ask the reviewer to challenge these concrete points:

1. Do the three scenarios add useful variation over existing tests without
   duplicating the snapshot/learner campaign?
2. Does every success response become an independent durable obligation,
   and are ambiguous outcomes treated correctly after quorum returns?
3. Can each partition/crash be observed, healed, and cleaned up without the
   test control path deadlocking behind a write?
4. Does replay preserve recorded targets and report divergence honestly,
   without claiming deterministic execution?
5. Does the WAL model remain independent of the implementation and exercise
   retained mmap/memo state across real incarnation changes?
6. Can the new artifacts reject incomplete/stale/mismatched-source results,
   and do they retain the first failure when cleanup also fails?
7. Are the three task boundaries and unchanged fast lane sufficient to keep
   delivery short while preserving existing qualification?

**Done means:** the three cases pass the six-run acceptance corpus; the
32-seed WAL corpus passes; negative controls reject broken evidence; replay
and cancellation are verified; all owned children are accounted for; the
measured command costs and current-candidate evidence are recorded; existing
required checks pass; and the reviewed effort is promoted through the
current process. No further platform work is implied by completion.

Use this table as the execution record. Do not create a second status system.

| Task | State | PR / source SHA | Focused command and result | Measured runtime / evidence |
|---|---|---|---|---|
| M1 plans, oracle, report | Not started | — | — | — |
| M2 scenarios and replay | Not started | — | — | — |
| M3 WAL and integration | Not started | — | — | — |
| Current-candidate review and promotion | Not started | — | — | — |

[rupee]: https://github.com/wasif-exe/raft-rupee/tree/87c5d3383221926a41fb373398edf60038f5e44e
[rupee-sim]: https://github.com/wasif-exe/raft-rupee/blob/87c5d3383221926a41fb373398edf60038f5e44e/raft-sim/src/simulator.rs
[rupee-invariants]: https://github.com/wasif-exe/raft-rupee/blob/87c5d3383221926a41fb373398edf60038f5e44e/raft-sim/src/invariants.rs
[rupee-server]: https://github.com/wasif-exe/raft-rupee/blob/87c5d3383221926a41fb373398edf60038f5e44e/raft-server/src/main.rs
