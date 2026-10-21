# Worker refinement — connecting Rust execution to two-worker progress

**Status:** building · **Updated:** 2026-10-21 · **Owner:** Codex coordinator · **Branch:** `codex/worker-progress-refinement` · **PR:** pending initial plan commit.

Companion to [the previous progress ledger](OPERATION-PROGRESS-STATUS.md).
This phase connects the selected library worker model to production Rust
and replaces the domain-only competitor with two complete worker pipelines.
No whole-cluster deadlock theorem or removal of existing tests is claimed.

## 1. Deliverables and ownership

| Deliverable | Owner | State |
|---|---|---|
| Source transitions, held resources and real conformance schedules | GPT-6 Luna | Tracing actual daemon/storage paths; pinned compiler available |
| Model scope, assumptions and correspondence design | Fable 5.1 | Advice requested; private-source access explicitly authorized |
| Two complete workers, temporal properties and controls | GPT-6.1 Sol | Architecture inspected; awaits design advice |
| Closed package registry, strict receipts and existing CI wiring | GPT-6 Luna | Extension inspected; implementation awaits package contract |
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
