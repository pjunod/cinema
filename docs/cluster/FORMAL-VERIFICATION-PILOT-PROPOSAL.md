# Formal verification pilot — check durable-job authority before expanding proofs

**Status:** implemented; bounded results in the status page · **Revised:** 2026-10-20 ·
**Owner:** Codex coordinator (this chat); Sol implementation; Fable 5.1 advice/review ·
**Executes:** Paul's request to turn the formal-methods exploration into a
reviewable project · **Source inspected:** `9ab523c32e00e5b56c15bbc7a71ac2d3d48c0d5e`.

This proposal owns scope and acceptance; discovery status is in the
[backlog](../features/PROJECT-BACKLOG.md); current ownership and evidence are in the
[status page](FORMAL-VERIFICATION-PILOT-STATUS.md). Revalidate anchors against the
implementation source.

If matching the model requires changing production semantics, record the
discrepancy and propose a separate correction; do not alter semantics or
simplify away a counterexample.

## 1. Decision — one bounded cluster pilot with an implementation bridge

Adopt **TLA+ with the TLC model checker** for a finite model of durable-job
ownership and one library catalogue publication path. Establish safety first,
then check one narrowly conditional recovery property. Connect the model to
real SQLite and replicated Hiqlite behavior through named contract tests and
a reviewed transition map. Keep every existing test obligation.

The deliverable is a reproducible verification package, not a claim that the
whole cluster is proved correct. Finish in four implementation tasks and one
effort promotion. Expansion to resource deadlocks, more publication adapters,
or deductive proofs is a later decision based on this pilot's evidence.

### 1.1 The questions this project must answer

1. **Can a displaced worker publish?** Enumerate cancellation, expiry,
   renewal, takeover, reboot, request replay and delayed replies around one
   publication boundary. Reject publication by an invalidated attempt.
2. **Does the model describe the code?** Show where each abstract guard and
   atomic action comes from, and exercise the important distinctions against
   both real Store backends. Hashes identify evidence; they do not prove
   correspondence.
3. **Can a recoverable claim become permanently stuck?** Check eventual
   reacquisition after a stable recovery suffix under explicit assumptions.
   Do not equate that with eventual successful job execution.
4. **Is the investment useful?** Measure model runtime, maintenance cost,
   actionable counterexamples and overlap with tests before proposing any
   testing reduction.

### 1.2 Required non-goals

- No whole-repository proof, general cluster deadlock theorem, or proof of
  OpenRaft, Hiqlite, TLS, SQLite, the compiler, operating system or hardware.
  Those would be separate projects with much larger trust boundaries.
- No consensus replacement, deterministic simulation platform, runtime
  verifier, new service, production fault endpoint or product feature gate.
  This is development evidence; it needs no Settings or Developer card.
- No exactly-once physical execution claim. A partitioned worker can continue
  external work after another worker becomes authoritative. The property is
  about accepted catalogue mutations through the selected boundary.
- No removal, weakening or rescheduling of existing tests, hooks, CI jobs or
  release qualification. Tool success is not fleet acceptance.
- No automatic proof of Rust/SQL refinement. The pilot's implementation
  bridge is a reviewed mapping plus executable conformance evidence.
- No assumption that a model checker is cheaper than testing. Record actual
  costs; an impractical configuration remains incomplete.

## 2. Why start here — bounded authority before all deadlocks

The queue exposes a bounded ordering-sensitive protocol. Model assumptions
do not replace the real transport and backend evidence this plan retains.
Local permit deadlocks remain a separate later candidate.

### 2.1 Evidence terms are part of the contract

A regression covers selected executions. A completed TLC run checks the
listed finite configuration; conditional liveness additionally depends on
its declared environment/fairness. Backend trace agreement is conformance
evidence, not a refinement theorem. Deductive proof and arbitrary cluster
size remain outside this pilot. Never report “the cluster cannot deadlock.”

## 3. Alternatives — select protocol modeling first

Use TLA+/TLC. Loom, Kani, Verus and an unbounded theorem remain later
investments; the [review companion](FORMAL-VERIFICATION-PILOT-REVIEW.md#5-tool-choice-and-review-questions)
records their roles, primary references and design-review answers.

## 4. Current implementation — preserve both ownership domains

### 4.1 Source map and initial publication boundary

| Existing source | Responsibility to map |
|---|---|
| [Queue protocol](../../crates/plurx-core/src/store/background_jobs.rs) | `JobState`, `JobToken`, `ClaimJob`, `ResolveClaim`, `RenewJobs`, cancellation, claim SQL and shared Store implementation |
| [Domain binding](../../crates/plurx-core/src/store/background_jobs_domain.rs) | `BindLibraryJob`, `BIND_SQL`, stable queue/domain fence association and initial exact-token checks |
| [Binding lifetime](../../crates/plurx-core/src/store/background_jobs_domain.sql) | Retained binding survives queue expiry/compaction; deleting the domain lease removes it |
| [Library worker](../../crates/plurxd/src/library_work.rs) and [runtime fence](../../crates/plurxd/src/background_jobs.rs) | Successful `JobFence::bind_library` precedes `process_library_requests`; bind failure releases/yields rather than starting work |
| [Publication facade](../../crates/plurx-core/src/store/publication.rs) | `PublicationStore::insert_item`, lease succession and the branch that permits an unfenced call |
| [SQLite transaction](../../crates/plurx-core/src/store/sqlite/mod.rs) | `with_fenced_conn`: exact predecessor/domain check, current queue binding check, renewal and mutation in one transaction |
| [SQLite publication](../../crates/plurx-core/src/store/sqlite/publication.rs) | `insert_item_fenced` and observable catalogue insertion |
| [Replicated publication](../../crates/plurx-core/src/store/hiqlite_publication.rs) | `atomic_renewal_statement`, `atomic_publication`, statement-zero output binding and rollback on missing authority output |
| [Backend contracts](../../crates/plurx-core/tests/store_contract/background_jobs.rs) | `background_library_publication_requires_both_live_owners` and related resource contracts |
| [Queue regressions](../../crates/plurx-core/src/store/background_jobs_tests.rs) | Replay identity, cancellation versus renewal and takeover rejection |

Select the bound library-scan route to `insert_item_fenced` as the initial
catalogue mutation. Existing test helpers may exercise a different mutation;
M0 must record that distinction rather than calling all publication methods
covered. Other mutations and adapters remain outside the first result.

Only `library_scan` and `metadata_refresh` are bindable kinds. The
[caller classification](FORMAL-VERIFICATION-PILOT-REVIEW.md#3-caller-and-maintenance-inventory)
distinguishes bound worker, domain-only and unfenced routes, including the
daemon metadata/genre publishers traced in the amendment follow-up. M0 must
revalidate that inventory; a shared facade does not extend the selected result.

The existing dual-owner test exercises `put_setting_fenced`, not catalogue
insertion. It supplies useful prior scenarios but does not discharge M2's
insert-specific obligation. Item identity allocation and catalogue content
correctness are abstracted as an opaque mutation and remain covered by
ordinary tests rather than this protocol model.

### 4.2 Real interfaces — reverify before implementation

From the [queue protocol](../../crates/plurx-core/src/store/background_jobs.rs):

```rust
pub struct JobToken {
    pub job_id: String,
    pub node_id: String,
    pub boot_id: String,
    pub claim_id: String,
    pub fence: i64,
    pub revision: i64,
    pub lease_expires_ms: i64,
}
```

`node_id` alone is not authority: reboot identity, claim identity, fence,
revision and expiry matter. At the inspected base `JOB_LEASE_MS` is 30,000 ms
and `CLAIM_RESOLUTION_WINDOW_MS` is 120,000 ms. Model their ordering and exact
boundary comparisons; small abstract times do not demonstrate real timing.
Claim dispatch time survives retries. Attempt retention and pruning bound
replay resolution; do not model an infinite deduplication history.

From the [domain binding](../../crates/plurx-core/src/store/background_jobs_domain.rs):

```rust
pub struct BindLibraryJob {
    pub token: JobToken,
    pub lease: Lease,
    pub now_ms: i64,
}
```

Binding checks the exact current token and domain lease, required resource
reservations, expiry and removed-owner state. The stored association uses
stable fences and attempt identity: queue renewal revisions and domain
renewal revisions advance independently. A model that makes every old queue
revision invalidate an otherwise current binding would describe another
protocol.

The selected backend operation has this signature:

```rust
async fn insert_item_fenced(
    &self,
    item: &NewItem,
    lease: &Lease,
    replacement: &Lease,
) -> Result<i64, StoreError>;
```

### 4.3 Publication is a transaction boundary, not a prior permission check

```text
claim queue attempt ── acquire domain lease ── bind both authorities
                                                  │
                                                  ▼
                      publication transaction:
                      check current authority + renew domain + insert item
                                                  │
                                      commit or reject/roll back
```

Model successful publication at its authoritative transaction point. An
earlier successful bind does not authorize an arbitrarily delayed write.
Cancellation serialized after a successful publication does not erase that
past publication; cancellation serialized before it must prevent acceptance.

Both backend guards allow domain-only operation when no matching queue
binding exists. The facade also has an unfenced mode. M0 must trace production
callers from the selected library worker through binding to publication,
including failure branches, binding lifetime and cleanup. Represent unbound,
bound and obsolete-bound states. Never assume binding exists merely because
the property needs it. If reachable worker behavior bypasses the boundary,
report the gap and narrow the claim or propose a corrective task.

The stored binding survives queue expiry/compaction and ordinary domain-lease
release. Release expires the lease and advances its revision; it does not
delete the lease row. A new domain fence can temporarily have no matching
binding. Include that interval and a control that removes bind-before-work
ordering. Do not model release as erasing all evidence of the previous owner.

The replicated path samples Unix milliseconds before submitting its
transaction; SQLite samples inside its transaction closure. Preserve where
time is sampled and how long a request may wait. Do not describe both as an
oracle returning physical commit time. Safety after an authoritative fence
change and rejection after real wall-clock expiry are distinct claims.

One facade call can make up to four Hiqlite insertion submissions with the
same predecessor/successor; allocator candidates and backend time samples
are refreshed. Retries require the specific identity-conflict errors, not
every transport failure. Model the retry guard, committed-but-lost reply,
stale retry rejection and facade state clearing. See the
[source-qualified retry contract](FORMAL-VERIFICATION-PILOT-REVIEW.md#2-publication-and-time-boundaries).

Preserve three authority samples: facade pre-check, successor computation
and backend check (per retry). SQLite explicitly checks successor expiry
against its sample; Hiqlite checks successor > predecessor and predecessor >
sample. The latter implies successor > sample on acceptance, but validation
order and error outcomes differ. Do not invent an acceptance difference or
treat ordinary metadata time as an authority clock.

Queue operations instead consume caller-supplied `now_ms` through both Store
bridges. Runtime monotonic deadlines and local fence revocation are another
layer; identify which are included in the caller protocol and which are
explicit premises. `PublicationStore::fenced_call` can observe local
revocation after the backend commits and then return `FenceRejected`.
Consequently an error returned by the facade is not proof that no insertion
committed. Model commit, reply loss/delay and local revocation separately.

## 5. Specification — explicit state, actions and properties

### 5.1 Abstract state and failure model

Model queue state, current attempt identity, queue fence/revision/expiry,
domain owner/fence/revision/expiry, binding identity, required and held
reservations, pending requests, retained attempt records, cancellation and
removed-owner markers, worker-local tokens, and accepted publication events.
Keep precheck/successor/per-submission backend timestamps and facade attempt
count in model state; commit and acknowledgement are separate events.

Use two workers with two boot identities, delayed/lost/reordered replies,
duplicate requests, and a single serialized authoritative Store. A request
may fail to reach the Store; losing its reply does not undo its commit.
Crash loses worker-local state but preserves committed Store state. Reboot
does not turn an earlier boot's token into authority.

Treat consensus serialization and durable transaction behavior as assumptions,
backed by existing tests. This model does not reproduce Raft internals or
prove those assumptions. Represent unavailable quorum as unavailable authority
operations rather than allowing partitioned workers to edit independent copies
of the authoritative queue.

| Abstract action | Required correspondence |
|---|---|
| Enqueue / add demand | Distinguish job identity from request identity and valid pending interest |
| Send / apply claim / deliver reply | CAS revision, dispatch window, unique claim identity, eligibility and resource checks |
| Replay / resolve claim | Recover an existing result without silently allocating another attempt; include expired/pruned outcome |
| Renew / deliver renewal reply | Exact predecessor token, cancellation precedence, new token and reservation lifetime |
| Acquire / renew domain lease | Independent domain ownership and exact predecessor requirements |
| Bind | Current queue and domain authority plus complete required reservations |
| Cancel / expire / reclaim | Invalidated authority, retained stale tokens and possible delayed traffic |
| Revoke reservation / remove owner | Publication eligibility changes without trusting worker acknowledgement |
| Attempt publication | Current bound authority and selected catalogue mutation in one atomic transition |
| Crash / reboot / prune history | Persistence boundary and bounded replay-history semantics |

Document each split between send, apply and reply. Only group SQL statements
that production executes atomically. Preserve intermediate binding and lease
states; do not collapse the whole worker lifecycle into one atomic action.

### 5.2 Required safety properties

| ID | Property checked |
|---|---|
| S0 · Type and identity | State remains well typed; attempt identities are not reused and fence/revision abstraction never wraps |
| S1 · Exclusive current attempt | Each modeled job has at most one authoritative current attempt; workers may hold stale local copies |
| S2 · Publication authority | Every accepted bound-route insertion passed the exact predecessor/state/binding/resource/removal checks at its authoritative transaction point, with expiry evaluated against that operation's recorded sampled time; no physical commit-time expiry guarantee is implied |
| S3 · Displacement | An old attempt cannot cause a new accepted insertion after cancellation, takeover or owner removal invalidates it; a legitimately new attempt is a separate identity |
| S4 · Replay stability | Replaying a claim cannot advance its fence or transfer its boot identity; resolution cannot manufacture current authority from pruned history |
| S5 · Cancellation precedence | Renewal cannot return a cancelling/cancelled attempt to running; retry with new audited interest is modeled separately |
| S6 · Binding continuity | Valid independent renewals preserve the stable association; a different attempt cannot replace it under the same domain fence. Only a higher domain fence or the identical stable tuple can replace/reaffirm a binding; refusal yields rather than starting work |
| S7 · Transaction rejection preserves catalogue | A publication transaction rejected before commit adds no selected catalogue item; rollback retains no partial domain-renewal/catalogue mutation. A facade error or lost reply after commit is a separate outcome |

Express S2 against a publication event's recorded pre-state, not against the
current state after later cancellation. Record harmless diagnostic/audit
changes separately from catalogue state. Do not add an idempotent-insert or
exactly-once insertion property unless the selected API actually provides it.

Safety exploration must allow unavailable quorum, indefinitely delayed
workers, stale messages and clock anomalies. If a particular property needs
a clock premise, label that property and run a configuration without the
premise to expose the distinction. Never hide a safety counterexample by
adding fairness.

If a physical-time expiry property is proposed later, give it a separate ID
and explicit clock-skew/submission-delay premises plus implementation
evidence. Do not strengthen S2's wording after seeing a sampled-time pass.

## 6. Progress — one conditional recovery result, with deadlock claims deferred

The mandatory progress target is **L1: a recoverable, eligible queued or
expired job eventually obtains a fresh current claim**. It says nothing about
transcoding success, publication completion or response time.

Check L1 in a separate stable recovery suffix: demand remains valid, no new
cancellation/removal occurs, one capable worker survives, necessary resources
remain available, time advances enough to expire the old claim, a functioning
quorum remains reachable, and sent authority requests are eventually applied
or explicitly failed and retried. Model outstanding pre-recovery messages so
that an old response can still arrive after recovery.

Time must also advance past worker `not_before` delays: bind refusal or
unavailable domain lease yields for 5,000 ms, and failure uses
`retry_delay_ms`. Model yielded demand becoming eligible again; an expired
queue lease alone does not guarantee a worker will retry.

Fairness belongs on concrete worker polling, sending, application and reply
steps. Do not assume “a claim eventually succeeds,” which would restate L1.
Record why each fairness premise is plausible and which runtime component
must provide it. Include a negative configuration where permanent partition
or an unscheduled worker prevents progress; that outcome is expected and
must not contaminate the safety verdict.

A live protocol may have no useful enabled action during an allowed outage;
a valid terminal state may have none either. Define intentional quiescence
and terminal behavior explicitly. Keep TLC deadlock checking enabled in
configurations where a missing successor is an error. An intentional stutter
action must not mask protocol blockage; L1 still needs separate temporal
checking. Do not call absence of TLC deadlock a starvation or livelock proof.

Resource circular-wait freedom, scheduler starvation freedom, all-library
publication progress and whole-cluster recovery remain later work. Their
premises include fair resource admission, eventual release and additional
locks and queues that this pilot does not model.

## 7. Finite exploration — bounds and completion must remain visible

M0 freezes explicit configurations after a feasibility run. Start from these
proposed domains; changes require a written reason and a new receipt:

| Configuration | Initial proposed scope | Intended use |
|---|---|---|
| Smoke | 1 job, 2 workers, 2 boot identities per worker, 2 claim identities, 1 domain, 2 resource keys | Fast syntax, invariant and mutation checks |
| Safety baseline | 2 jobs, 2 workers, 2 boot identities per worker, 3 claim identities, 1 contested domain, 2 resource keys | Contention, stale authority and replay exploration |
| Recovery suffix | 1 target job, 2 workers, an old retained token and a fresh claim identity, both resource keys | L1 after stabilization with delayed old traffic |
| Adversarial controls | Named small configurations that violate one selected assumption or remove one guard | Demonstrate that checks can detect the intended failures |

Expiry abstraction must distinguish before, equal and after each relevant
deadline, including equality rejection. M0 records the relation between
queue/domain expiries, dispatch-window expiry, reservation expiry and sampled
request time. A single `expired: bool` is insufficient if the code compares
different timestamps. Include fast/slow/backward-clock cases where relevant;
reusing a request's earlier timestamp must remain distinguishable from a
fresh sample.

The [boundary comparison table](FORMAL-VERIFICATION-PILOT-REVIEW.md#2-publication-and-time-boundaries)
records exact source lines: queue/domain expiry `>`, reservation coverage
`>=`, resolution-window expiry `>=`, release `min(expires, now)`, and the
successor checks. M0 preserves these separately, including equality cases.

Use finite fresh identities without recycling, overflow or counter modulo.
For safety, state the maximum number of lifecycle generations explored and
label exhausted exploration budgets explicitly; do not report such a boundary
as a production terminal state or a general deadlock result. Exercise real
integer-overflow refusal separately in backend tests where appropriate.

For L1 use a finite abstraction that preserves the paths to first fresh claim
and admits an absorbing success state. Explain why bounded identities or
renewal revisions cannot disable progress before that state. If that cannot
be justified, L1 is incomplete; do not use a safety depth cap to pass it.

Any reduced history-retention thresholds must preserve production pruning
eligibility: elapsed resolution window, successor/newer-finished-attempt
conditions and global-pressure behavior. Map those predicates and exercise
their real parameter boundaries in backend tests; retaining exactly a fixed
number of attempts is not an adequate abstraction of the production policy.

Use the [maintenance inventory](FORMAL-VERIFICATION-PILOT-REVIEW.md#3-caller-and-maintenance-inventory):
enqueue admission pruning and the installed receipt-pressure maintenance
trigger delete terminal jobs; attempt pruning is a separate statement, and
job-deletion triggers retire details. `MAX_RETAINED_JOBS` alone is not
evidence of deletion. Model only the identified statements and their guards.

Run exhaustive reachable-state exploration for acceptance, not random
simulation. Record invariant/property selection, all constraints, overrides,
symmetry, reductions and fingerprint settings. No silent depth cutoff, state
constraint, omitted temporal property or success after timeout. Disable
symmetry for liveness configurations, as the
[TLC model-values documentation](https://nightly.tlapl.us/doc/model/model-values.html)
requires. Record residual model-checker/fingerprint trust assumptions.

Proposed evaluation budgets are 60 seconds for smoke and 15 minutes / 4 GiB
Java heap for each baseline configuration on a named reference host. These
are feasibility targets, not measured costs or correctness limits. Record
actual wall time and peak process RSS separately from the heap limit. An
over-budget run is incomplete. Simplify an independently justified abstraction
or explicitly revise the budget; never silently shrink the safety domain.

## 8. Implementation bridge — test the abstraction against both backends

### 8.1 A reviewed transition map is mandatory

For every abstract action, record its source symbol, SQL predicate or
transaction boundary, relevant input validation, omitted state, time source,
observable output, backend differences, and named conformance test. Include
unbound publication and error paths. Trace production library-worker callers,
not just tests that bind correctly by construction.

Map all three clock samples, backend validation order, insertion retries
and the second Hiqlite insert authority check using statement-zero output.
Classify matching `StmtIndex(0) does not have observable row output` as an
error-text compatibility assumption; add a pinned-version conformance test
that exercises real missing output and verifies rollback/classification.

M1's map is a reviewed table. Machine-readable source hashing/drift detection
in `mapping.json` belongs to M3; a hash is not required to author the model.
Classify each relationship as **reviewed mapping**, **executable example**,
**assumption**, or **unresolved gap**. None is called a refinement proof.
Source hashes catch drift only when the checked path set is complete; require
a deliberate mapping review when those paths change. Include queue adapters,
binding schema, lease implementation, worker/facade and both backend paths in
that set once M0 identifies them. Do not equate unchanged filenames with
unchanged semantics of their dependencies.

### 8.2 Conformance cases and controls

Retain the existing regressions named in §4 and add the smallest missing
trace cases for the selected insertion. Required examples are:

- Live bound insertion succeeds; cancellation or takeover before insertion
  rejects it; cancellation after commit leaves the prior insertion intact.
- Bind, then queue renewal and independent domain renewal, then insertion:
  distinguish legitimate association continuity from stale exact tokens.
- Queue expiry with a still-live domain lease rejects at the applicable
  sampled-time boundary; separate real elapsed-time claims from that result.
- Lost claim reply, replay and resolve preserve attempt identity; reboot,
  retention pruning and resolution-window expiry do not restore stale rights.
- Revoked reservations, owner removal, obsolete binding and missing binding
  exercise their actual code semantics rather than an idealized predicate.
- A transaction rejected before commit leaves no selected inserted item and
  no partial authority transition that the model treats as rolled back.
  A successful commit followed by local revocation or reply loss remains
  visible even if the facade returns an error; do not assert no write from
  that return value alone.
- Target lost-reply testing at the chosen Hiqlite insertion: retain a real
  commit, lose its acknowledgement, exercise stale-predecessor rejection and
  facade state clearing, and inspect the inserted item. Separately exercise
  the actual allocator-conflict retry guard. Do not assume arbitrary lost
  replies trigger the four-attempt loop; any combined trace must justify its
  concrete error path.

Use the existing Store contract framework to run the same observable cases
against SQLite and a real three-voter Hiqlite backend. Test retained database
state as well as return values. Seed recorded times where the API permits;
use controlled barriers for ordering and bounded polling for real authority
transitions. Do not rely on arbitrary sleeps to establish invalidation.

Existing names to retain include
`background_jobs_replayed_claim_never_advances_fence_or_changes_boot`,
`background_jobs_cancellation_wins_renewal_and_rejects_new_interest`, and
`background_library_publication_requires_both_live_owners`. M0 records exact
module paths, feature flags and executable commands; names alone are not proof
that replicated code ran. Focused replicated core tests must enable
`hiqlite-store`; Store contracts require `hiqlite-contract-tests` (which
implies `hiqlite-store`) and the `hiqlite-3-voter` harness result.

### 8.3 The verifier must catch a deliberate defect

Provide at least three model mutations: ignore queue binding at publication,
allow a replay to allocate a fresh fence, and let renewal revive cancellation.
Each must produce the expected named invariant violation and a retained trace.
A parser error, timeout, crash or unrelated failure is not a successful
negative control. For L1, a separate defective retry transition must produce
the expected temporal counterexample under the same recovery assumptions.

Replay selected counterexample schedules through the real Store contract
helpers as regression cases where they describe real operations. Do not
build a generic Rust/TLA interpreter or trace language for this pilot. Model
mutations remain test-only; never patch production guards to demonstrate them.

## 9. Proposed artifacts and developer interface

Keep a single pilot directory, `verification/durable-jobs/`, with the TLA+
specification, named TLC configurations, mutation controls, machine-readable
source/transition mapping and a short README explaining how to reproduce
results. The [status page](FORMAL-VERIFICATION-PILOT-STATUS.md) records current ownership and acceptance.

| Proposed artifact | Job |
|---|---|
| `verification/durable-jobs/DurableJobs.tla` | Protocol state, transitions, invariants and recovery property |
| `verification/durable-jobs/*.cfg` | Explicit smoke, safety, recovery and control configurations |
| `verification/durable-jobs/mapping.json` | Source symbols, assumptions, property coverage and conformance links |
| `verification/durable-jobs/toolchain.json` | Exact TLC release/artifact SHA-256, supported Java version and resolved invocation |
| `verification/durable-jobs/README.md` | Commands, verdict interpretation, bounds and trusted components |
| `scripts/check-formal` | Bounded runner, strict completion checks, controls and receipt emission |
| `tests/operations/test_formal_runner.py` | Meaningful runner controls: incomplete run, bad artifact, missing property, expected mutation and stale source evidence |

Keep downloaded tool binaries and generated logs out of tracked source. Pin
a supported published TLC artifact and compatible Java runtime in M0; never
use an unqualified `latest` download. Verify the artifact hash before
execution. Tool acquisition must be explicit and separate from verification;
offline use of an already verified artifact must work. No new secret or
repository credential is required.

Proposed commands after implementation:

```bash
scripts/check-formal smoke  # safety, recovery and reachability witnesses
scripts/check-formal full   # all 16 safety/recovery/control/witness configurations
```

The runner accepts an explicitly configured local TLC artifact/runtime and
prints the resolved identity. Exit zero requires every selected configuration
to finish with its expected verdict. Nonzero includes unexpected
counterexample, missing dependency/property, hash mismatch, timeout, memory
exhaustion, source mismatch or incomplete exploration. A control finding its
specific expected counterexample is an expected verdict, not a positive proof.

The model uses seeded starting states with one shared safety transition
relation. Smoke explores at most six modeled events and the two-job baseline eight;
a supplementary single-job lifecycle configuration explores eleven. The
reachability configurations use at most sixteen. These finite schedules do not
prove arbitrary-length protocol safety. Recovery uses a separate finite
suffix with explicit fairness and time-advance premises. Seven normal-model
reachability witnesses complement five deliberately defective controls. A
witness reaches a named state; it is not an exhaustive safety result.

Smoke has a 180-second aggregate model budget after tool resolution, with a
60-second safety cap and 30-second recovery/witness caps. Full baseline and lifecycle each have a 900-second cap. Timeout or nonempty state queue supplies no passing evidence.

Write logs, traces and receipts beneath ignored `target/verification/`.
Each receipt records source commit/tree and dirty state, hashes of relevant
source/model/config/tool artifacts, Java/TLC identity, invocation, bounds and
assumptions, selected properties, reductions, generated/distinct states where
available, completion state, expected versus actual verdict, runtime, memory
and artifact hashes. Record the checked commit/tree. Later documentation-only
bookkeeping can retain evidence only when every governed source, model,
configuration, tool and runner hash still matches; record both commits in the
PR. Changed checked inputs invalidate their affected results. Schema fields
that TLC cannot supply are explicitly unavailable, not zero.

**How to read it:** `complete / bounded-model-checked` means only the listed
safety properties on those configurations passed. `complete / conditional-
liveness-checked` carries its explicit recovery assumptions. `incomplete`
supplies no passing evidence. A backend result is reported separately from
either model verdict. Retain the checked commit and artifact digests in the
review/qualification record; local scratch files alone are not durable
acceptance evidence.

## 10. Delivery — four milestones in one batched PR

Use `codex/formal-verification-pilot` for one main-bound batched PR under
Paul's §12 execution direction. Preserve the serial M0–M3 acceptance
dependencies. Workers use isolated agent-owned clones and explicit file
ownership; coordinator integrates their commits.

| Task | Owned scope | Acceptance before the next task |
|---|---|---|
| M0 · Freeze semantics and tool feasibility | This ledger and index/backlog row; pilot README, toolchain/JDK pin and existing preflight provisioning; reviewed caller/maintenance table | One pinned TLC smoke example completes; exact model boundary and backend commands recorded; legacy/unbound path and time-sampling questions resolved or named as blockers |
| M1 · Safety model and defect controls | TLA+ model, safety configurations/mutations, reviewed transition table, runner core and existing preflight smoke steps | S0–S7 complete for the frozen safety baseline; three expected mutation violations retained; no unresolved abstraction gap hidden by an assumption |
| M2 · Backend correspondence | Existing queue/Store test modules, focused conformance helpers, source mapping and ledger | All named SQLite and three-voter Hiqlite cases run on the candidate source; selected insertion/error paths mapped; differences resolved or explicitly narrow the result |
| M3 · Recovery and reproducible evidence | Recovery config/mutation, machine-readable source hashes/drift checks, receipt controls, README, catalog integration and closeout | L1 completes under stated premises; retry defect gives expected temporal violation; baseline command rejects incomplete/stale evidence; costs and coverage reported |

M0 is a feasibility decision. If honest modeling requires a new simulation
platform, adapting the whole daemon to a verifier, or an unbounded proof just
to state the selected property, return a bounded findings document and revise
the project explicitly. Do not spend the remaining tasks hiding that cost.

**Planning estimate:** 8–15 engineer-days plus independent review, with the
largest uncertainty in backend correspondence and finite liveness abstraction.
This is an estimate, not a measured delivery promise. Review scope and cost
after M0 and after the first completed safety baseline. An unresolved safety
counterexample is a finding, never a reason to declare completion early.

### 10.1 Repository gates stay authoritative

Use the pinned Rust 1.97.1 [compile loop](../ci/AGENT-COMPILE-LOOP.md) and
normal commits. The single-PR execution in §12 governs test timing. Runtime
fixes require `fix(`/`perf(` scope and `Regression-Test:` fields in the PR and
landing message; model or test work must not conceal a production fix.

Existing preflights provision pinned Java/TLC, check source drift and run the
affected bounded smoke profile. Preserve effort `compile_only` coverage and
retain failure artifacts. The 900-second baseline/lifecycle checks are local
qualification evidence, outside the 10/15-minute preflight budget. Missing
tools or incomplete exploration fail the selected check. Add no workflow jobs
or suppression of existing checks; see the [CI disposition](FORMAL-VERIFICATION-PILOT-REVIEW.md#4-ci-provisioning-without-another-job).

Integrate current `main`, freeze the candidate, obtain review, fix findings,
and qualify the resulting source. Merge requires **Main promotion gate** and
the qualification receipt. Changed checked inputs invalidate affected results;
documentation-only bookkeeping follows §9's hash-equivalence rule. Production
deployment and physical acceptance are separate work.

## 11. Completion — stronger evidence before any test reduction

The pilot is complete only when all four milestones, independent design and
implementation reviews, exact-candidate gates, reproducible model receipts,
backend evidence and the result/cost report exist. State the selected route,
finite bounds, time premises, trusted components and unsupported properties
in the closeout. Finding no bug is a valid outcome; finding a bug requires a
regression and disposition, not a broader correctness claim.

Keep incident/ownership regressions, SQLite/replicated Store contracts,
process/transport recovery, performance and fleet evidence: they exercise
implementation and physical behavior absent from the model. Measure overlap
with repeated schedule-only examples; propose any reduction separately.

A later test-reduction proposal must map each candidate test to the exact
property, model bounds, implementation bridge, residual risk and replacement
command, and show before/after runtime. Neither this proposal nor pilot
acceptance authorizes deleting tests. The first measured success criterion is
more reproducible coverage of authority orderings at an understood cost.

## 12. Execution direction — one batch, review, then the fast lane

Fable's design verdict and source-qualified dispositions remain in the
[review record](FORMAL-VERIFICATION-PILOT-REVIEW.md). Paul's 2026-10-20 build
direction supersedes §10's task-PR and test timing: work in agent-owned
clones, make proper related commits, batch one substantial PR to main, obtain
one final adversarial Fable 5.1 review when the whole PR is ready, fix its
findings, then run the fast lane once. Preserve passing evidence on unchanged
source; rerun only failed or genuinely invalidated checks. Development model
checking and compile/static checks remain available while building. No
construction-time unit suites. Existing correctness requirements remain.

Paul clarified the model roles: Fable 5.1 advises on heavy decisions and
performs final review; lower models do the coding. Sol handles substantial
implementation and Luna handles bounded tasks, with explicit file ownership. The coordinator owns the
PR and [status](FORMAL-VERIFICATION-PILOT-STATUS.md). M0 semantics/tool
feasibility remains prerequisite to M1; implementation and acceptance are
reported separately.

### 12.1 M0 cost decision — separate short contention and full lifecycle

The two-job ten-event exploration was stopped incomplete after sustained
frontier growth. One positive-only state quotient reduced cost but did not
make that horizon credible within the declared budget. Fable's second
text-only design consultation recommended a two-job eight-event baseline
and an additional single-job eleven-event lifecycle check. Sol implements the
configurations; final review and qualified results are in the status page.

The identity domains, shared safety transition relation and enabled faults
stay explicit. The two-job baseline checks all seeds through eight events;
publication from an already-bound seed can still overlap contention. The
fresh-claim→acquire→bind→publication→reply composition is checked separately
with one job and two workers. These checks do not establish the longer
combined two-job lifecycle. Normal reachability witnesses and real Store
examples support their own narrower claims. Neither new positive check is
accepted until it completes. The original incomplete configuration and its
metrics remain negative feasibility evidence, outside the qualifying full
profile. If eleven single-job events exceed budget, a predeclared ten-event
fallback covers commit without delivery and must be reported accordingly.
