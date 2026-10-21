# Worker refinement — connecting Rust execution to two-worker progress

**Status:** building · **Updated:** 2026-10-21 · **Owner:** Codex coordinator · **Branch:** `codex/worker-progress-refinement` · **PR:** pending initial plan commit.

Companion to [the previous progress ledger](OPERATION-PROGRESS-STATUS.md).
This phase connects the selected library worker model to production Rust
and replaces the domain-only competitor with two complete worker pipelines.
No whole-cluster deadlock theorem or removal of existing tests is claimed.

## 1. Deliverables and ownership

| Deliverable | Owner | State |
|---|---|---|
| Source transitions, held resources and real conformance schedules | GPT-6 Luna | Committed seven real schedules and explicit gaps; pinned daemon/core compiles, Clippy and formatting pass |
| Model scope, assumptions and correspondence design | Fable 5.1 | Design delivered; authority split, real-worker schedules and correspondence obligations recorded |
| Two complete workers, temporal properties and controls | GPT-6.1 Sol | Committed 596-line model and 17 configurations; static SANY passed |
| Closed package registry, strict receipts and existing CI wiring | GPT-6 Luna | Integrated strict 57-action/18-boundary checks; all three source mappings pass static validation |
| Integration, ledger, PR and exact-tree qualification | Codex coordinator | Owned clone established at `c2d5aa257` |
| Independent final adversarial review | Fresh Fable 5.1 session | Runs once the complete candidate is frozen |

## 2. Implementation correspondence is an explicit obligation

Map each relevant production transition to its model action, including lock
ownership across awaits and the task that can complete each wait. Record
source anchors, test schedules and omitted behavior in a machine-readable
contract. Conformance tests must exercise production paths; passing them
is evidence for selected schedules, not a proof of all Rust executions.

Separate local revocation from durable publication authority. A submitted
write may commit after local revocation and before durable release. Model
and check rejection after durable release or replacement by a new owner.
Trace ambiguous renewal/release and cancellation through joins and cleanup;
a caller timeout cannot establish lower-write rollback.

## 3. Two complete pipelines share the domain

Give both workers admission, claim, queue token mutex, publication lock,
renewal, backend submission/application/reply, cancellation and cleanup.
Verify that the selected source jobs can legitimately contend on one domain.
Check global useful progress and each worker's attempt cleanup under stated
dependency premises; do not assume per-job acquisition or starvation freedom.
Use reachability witnesses to demonstrate both pipelines execute and contend.
No finite event horizon may make a stuck execution disappear.

## 4. Review and qualification order

Build one substantial main-bound PR with related commits and current owner,
stage and remaining acceptance. Preserve previous proof packages except
explicitly reviewed source-pin changes if test additions affect mapped files.
Static/compile checks may run during implementation. Freeze the full candidate
for fresh Fable review, address findings, then execute the affected tests and
formal checks once. Retain successes on unchanged governed inputs and rerun
only failures or checks invalidated by subsequent edits. Merge only the
qualified tree after the required main promotion gate passes.

## 5. Assumptions and remaining limits

Filesystem and backend calls must eventually return, a capable worker must
survive, the runtime must schedule enabled tasks, and time/expiry barriers
must eventually be observed. Numeric deadlines, arbitrary failures, general
shutdown, Raft internals, arbitrary concurrency and complete Rust refinement
remain separate obligations unless this phase explicitly supplies evidence.
No runtime correction is planned without a concrete source defect. No product
watchdog, optional feature switch, broad testing framework or test deletion
belongs to this work. Final evidence will identify exactly which assumptions
were discharged and which remain.

## 6. Design decisions recorded on 2026-10-21

Fable 5.1 advice task `t_e1470e0f` separates finite model results,
implementation correspondence and selected conformance schedules. It does
not claim that passing tests establishes formal Rust refinement. A fresh
independent session will review the complete candidate later.

The source permits two real queue jobs for one library: scan and metadata
refresh have different dedupe keys and can occupy the two resource slots,
then contend on the same domain lease. Locks remain per worker; the domain
and durable backend authority are shared. The model will preserve both
complete pipelines. Removing a stage from the second worker to reduce
checking cost was considered in advice and declined by the coordinator.

Local revocation cannot roll back an already submitted mutation. New local
dispatch must stop; durable guards govern subsequent application. Renewal
holds its publication writer through backend return. Release revokes and
joins renewal before exact-token retirement; no deadline is inferred.

Before Rust edits, Luna verified `rustup run 1.97.1 cargo check -p plurxd
--all-targets` with an owned warm target directory: exit zero in 2m 42s.
No unit tests or model qualification have run in this phase. The default
shell compiler is newer and will not be used as qualification evidence.

The conformance implementation exposed another distinction: a local queue-loss
notification does not prove that the durable queue lease expired. Retirement
may resolve the still-valid attempt and settle it. Tests and model must
separate the local signal from durable authority; an expiry-only schedule
cannot stand in for an ambiguous notification. Worker lifecycle probes are
compiled only for tests, following the existing task-local fault-seam pattern.
Spawned-heartbeat scope limitations remain explicitly unchecked where relevant.

## 7. Candidate acceptance distinguishes observations from assumptions

Seven inventoried Rust tests cover five actual daemon schedules and two
backend schedules. The paired-worker daemon test includes contention and
retry. Local queue-loss, domain-loss and admission-pressure schedules await
the real processing future before cleanup; their barrier is before scanner
dispatch, so they do not demonstrate joining a blocked filesystem walker.
The lost completion acknowledgement schedule uses the actual worker reread.
The backend captured-token example checks durable guards after local revoke;
it does not observe a facade dispatch before that revoke. Rejection after
durable release and higher-fence rebinding is checked independently.

Ambiguous renewal, repeated release failures, spawned heartbeat ordering,
durable queue expiry in the worker, actual blocked-child joins, arbitrary
cancellation sites, migration and general shutdown remain explicit gaps.
No existing tests are removed or declared redundant by these schedules.

The workspace check caught a test-only release hook referencing a daemon-only
module from shared lease source compiled by the cluster checker. The unused
hook was removed, rather than adding another conditional build mechanism.
Pinned cluster-check compilation and Clippy now pass. The integration hook
will check the exact candidate across the workspace before review.
