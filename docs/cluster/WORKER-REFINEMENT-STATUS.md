# Worker refinement — connecting Rust execution to two-worker progress

**Status:** built; qualification and landing receipt in PR #1251 · **Updated:** 2026-10-21 · **Owner:** Codex coordinator · **Branch:** `codex/worker-progress-refinement` · **PR:** [#1251](http://forge.lan:3000/noirr/plurx/pulls/1251); coordinator owns qualification and merge.

Companion to [the previous progress ledger](OPERATION-PROGRESS-STATUS.md).
This phase connects the selected library worker model to production Rust
and replaces the domain-only competitor with two complete worker pipelines.
No whole-cluster deadlock theorem or removal of existing tests is claimed.

## 1. Deliverables and ownership

| Deliverable | Owner | State |
|---|---|---|
| Source transitions, held resources and real conformance schedules | GPT-6 Luna | Committed seven real schedules and explicit gaps; pinned daemon/core compiles, Clippy and formatting pass |
| Model scope, assumptions and correspondence design | Fable 5.1 | Design delivered; authority split, real-worker schedules and correspondence obligations recorded |
| Two complete workers, temporal properties and controls | GPT-6.1 Sol | Committed two-worker model; duplicate cancellation witnesses consolidated after review |
| Closed package registry, strict receipts and existing CI wiring | GPT-6 Luna | Integrated strict 57-action/18-boundary checks; all three source mappings pass static validation |
| Integration, ledger, PR and exact-tree qualification | Codex coordinator | Owned clone established at `c2d5aa257` |
| Independent final adversarial review | Fresh Fable 5.1 session | Review delivered on `3eee032ad`; three findings addressed; finite corrections and execution receipts recorded below |

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
not claim that passing tests establishes formal Rust refinement. The fresh
independent final review and its findings are recorded below.

Scan and metadata refresh have different dedupe keys and can both be queued
for one library. Qualification corrected the initial claim that both could
hold the source reservation concurrently: normal same-root library work is
serialized by the `source_io` claim predicate. The model admits both claims
as an explicit abstraction. Its domain-overlap witness is model evidence;
a simulation and fairness argument remains necessary before transferring
its temporal result to Rust. Both complete pipelines remain in the model.

Local revocation cannot roll back an already submitted mutation. New local
dispatch must stop; durable guards govern subsequent application. Renewal
holds its publication writer through backend return. Release revokes and
joins renewal before exact-token retirement; no deadline is inferred.

Before Rust edits, Luna verified `rustup run 1.97.1 cargo check -p plurxd
--all-targets` with an owned warm target directory: exit zero in 2m 42s.
This was compile-only evidence before the qualification runs below. The default
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
backend schedules. The paired-worker daemon fixture is being corrected to observe real queue
contention and subsequent retry, rather than unreachable domain contention. Local queue-loss, domain-loss and admission-pressure schedules await
the real processing future before cleanup; their barrier is before scanner
dispatch, so they do not demonstrate joining a blocked filesystem walker.
The lost completion acknowledgement schedule uses the actual worker reread.
The backend captured-token example checks current durable-owner predicates;
it does not exercise local revocation or a prior facade dispatch. The release rejection is isolated; a replacement-owner schedule must retire
the first queue claim before obtaining the second. Its stale queue and
domain predicates will therefore both reject the old call.

Ambiguous renewal, repeated release failures, spawned heartbeat ordering,
durable queue expiry in the worker, actual blocked-child joins, arbitrary
cancellation sites, migration and general shutdown remain explicit gaps.
No existing tests are removed or declared redundant by these schedules.

The workspace check caught a test-only release hook referencing a daemon-only
module from shared lease source compiled by the cluster checker. The unused
hook was removed, rather than adding another conditional build mechanism.
Pinned cluster-check compilation and Clippy now pass. The integration hook
will check the exact candidate across the workspace before review.

## 8. Independent review and the bounded correction batch

Fresh Fable 5.1 task `t_7e8715bf` returned **CHANGES_REQUESTED** on
`3eee032ad42d15f7b1b34dcbb50c3d747e80abca`. It read the complete model,
configurations, Rust/tooling diffs, semantic correspondence, ledger and source
excerpts. It did not independently read the raw mapping, prior-pin diff,
outlines or index-only hunks; those limits accompany the report. No tests
were run by the reviewer. No reviewer approval is inferred from this verdict.

| Finding | Required correction | Owner / state |
|---|---|---|
| F1 · overstated action coverage | Mark unobserved work-fault, heartbeat, release retry/ambiguity and lost-settlement transitions unchecked with explicit gaps | Luna / corrected; all 18 flagged rows unchecked with gaps |
| F2 · duplicate cancellation configurations | Replace four identical model runs with one abstract cancellation witness; keep distinct Rust schedules | Sol and Luna / model, registry and mapping integrated |
| F3 · misleading local-revoke test history | Remove unused local fence/revoke and name the test for current durable-owner predicates | Luna / renamed current-durable-owner test and corrected ledger |

The corrected registry contains **14 outcomes**: two positives, five temporal
controls, one safety control and six reachability witnesses. This removes
three redundant model runs while preserving both complete worker pipelines.
Static SANY passed for the consolidation. The findings were addressed before affected execution began. Three daemon
cancellation checks passed; four fixture/assertion failures and a TLC timeout
remain under correction. Passing compile/link checks are separate evidence.

## 9. Nonblocking review comments have explicit dispositions

| Comment | Disposition |
|---|---|
| C1 · livelock name | README explains it removes scan-finish fairness while renewal may continue. |
| C2 · dead stale term | Retained as the documented by-construction ordinary apply guard; no unrelated rewrite. |
| C3 · cancellation after dispatched completion | Recorded ordering abstraction: model can settle then release, while Rust loss/pressure releases then yields. |
| C4 · Retry versus Yield | Recorded omitted settlement-kind and failed-attempt accounting; no claim about retry metrics. |
| C5 · model feasibility | Both bounded positive attempts timed out. After observer projection, twelve configurations run locally and CI owns the two smoke outcomes, avoiding duplicate successful runs; all fourteen remain mandatory. |
| C6 · 5.1-second delay | A real yield requires the production clock; the corrected queue-refusal fixture does not yield or incur its five-second delay. |
| C7 · renewal fault overlap | Retain named actions for separate source boundaries; overlap is documented and harmless. |
| C8 · selected cancellation | Contract explicitly limits the abstract signal to worker one and assumes no fair cancellation. |
| C9 · no additional gate | Corrected the data; no new validator policy or product gate. |
| C10 · replicated coverage | Focused core command explicitly enables `hiqlite-contract-tests`. |
| C11 · barrier placement | Module documentation warns never to pause inside selected loss/pressure arms. |
| C12 · hash verification | Strict mapping validation checks actual model/config/source/test hashes; review did not recompute them. |

The sole final review requested corrections; the coordinator has checked the
specified changes and preserves that verdict rather than relabelling it
reviewer approval. The next gate is affected qualification on the corrected
candidate. Existing successful static checks remain identified separately.

## 10. Qualification receipts and remaining obligations

The [PR receipt](http://forge.lan:3000/noirr/plurx/pulls/1251) records the
current candidate, owner, gate state, configuration counts and landing.
This ledger distinguishes delivered implementation from its acceptance;
source changes and model results are never interchangeable evidence.

All **seven Rust functions have passing evidence**. Three cancellation
schedules retain their original `2ffb56b86` results: the functions, eight
helpers and relevant production paths are byte-identical. The paired queue
admission and completion-acknowledgement cases passed on `eeecacddf`.
The current-owner backend case also passed there; the corrected yield and
replacement case passed on `5b212518`. Both backend cases invoke memory
SQLite, file SQLite and the real three-voter Hiqlite contract harness with
`hiqlite-contract-tests`. The integrated Rust file hashes match the final
tested source. Failed attempts and a zero-test filter are retained separately.

Qualification found concrete fixture errors, rather than a runtime defect:
the paired worker originally could not pass singleton physical admission or
the durable `source_io` guard; successful request state is `succeeded`, and
an explicitly failed result is `failed`. Cancelling a job also cancels its
waiter, so the replacement fixture uses the production five-second queue
yield to free reservations while retaining pending demand. S2 isolates
release with the queue token still current; S3 deliberately combines an
obsolete queue token and changed domain owner.

The unchanged model hit both bounded positive limits: 300 seconds at
678,415 distinct states / 51,612 queued, and 900 seconds at 4,162,006 /
151,968 queued. Neither is a pass or a counterexample. The second used
final-only liveness analysis and retained all properties.

Fable design `t_336dc863` supplies the state projection and both directions
of fair-execution lifting for unread `tick` and `item` bookkeeping. Sol
removed only those fields and their updates; all worker stages, 57 actions,
14 configuration bytes, fairness clauses, eligibility barriers and properties
remain. The configured model hash is
`abe9667c462b6275445f5c488456c208885e143e10bb0d017c20870cee63b3b0`.
The argument is documented in the verification contract; it is not a
machine-checked refinement theorem.

The coordinator rejected that advice's checker-disable recommendation:
inspection of the pinned TLC jar shows that an identical-state successor
counts before duplicate filtering. `TimeObserved` still emits that explicit
self-loop, so the existing global checker remains enabled and vacuous.
The temporal properties and negative controls carry the progress evidence.

The revised execution plan requires twelve local outcomes, then the two
existing CI smoke outcomes (`progress.cfg` and `witness-both-complete.cfg`)
and the current-candidate Main promotion gate. The PR records their actual
results; incomplete exploration never qualifies. CI owns those two outcomes
to avoid executing a successful local smoke twice. No additional test gate,
product switch, watchdog or test deletion was introduced.

Twelve local outcomes matched their registry: complete fault-free contention
exploration (398,166 distinct states, zero queued), five temporal controls,
one finite stale-commit control and five reachability witnesses. One stale
control receipt first rejected a mismatched trace-preamble entry; correcting
that single metadata string and rerunning only that case produced the
expected finite violation. The parser remained strict. Successful cases were
retained. The two CI-owned smoke outcomes and promotion/landing receipt are
recorded in the linked PR, alongside each run's actual source candidate.
