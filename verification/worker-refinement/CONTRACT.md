# Worker refinement contract — qualify conditional progress and authority

**Status:** reviewed candidate with corrections; exhaustive qualification remains incomplete.
Companion to [README.md](README.md), which owns commands and the configuration
registry. The correspondence files own production anchors and schedule gaps.

## Two complete pipelines share one durable domain

Workers `w1` and `w2` denote two distinct selected library jobs with fixed
job/request pairing. `library_scan` and `metadata_refresh` have different
dedupe keys and may both be queued for one library. The model intentionally
overapproximates admission and claim by permitting both to run and contend
on `scan:library:<id>`; this is a domain-contention stress abstraction, not a
reachable paired production schedule.

Production admission has two earlier gates. Each node's background-heavy
physical semaphore has capacity one. Even with separate node-local pools,
the durable `CLAIM_SQL` source-reader gate rejects a second same-root
non-live background job while the first holds its reservation. The supported
`enqueue_library_work` API supplies a `library_work` waiter and does not
attach the qualifying live-demand waiter that would permit that second
reader. Different dedupe keys and a generic resource capacity of two do not
make both library claims eligible.

The real paired-worker schedule therefore covers two nodes, durable
claim refusal without domain yield, and sequential completion after the
first worker settles. It does not exercise the model's simultaneous claims
or domain-overlap/reacquisition witness. A separate single-worker source
schedule with a foreign domain holder can cover domain refusal, without
establishing this paired model history.

The production request query is filtered by claimed job ID; `request[w]`
and `waiter[w]` are never completed by another worker. Fixed pairing models
a selected request batch, not migration or arbitrary job scheduling. Both
modeled pipelines retain the same item and completion milestones.

Every worker has separate physical admission, local claim, durable queue
state, token mutex with FIFO intent queue, publication read/write conflict
with FIFO intent queue, domain heartbeat, queue heartbeat, backend caller
slot, detached lower slot, walker, cancellation state and retries. These
locks are local to each worker; the workers contend through durable domain
ownership, not through a shared in-process RwLock. One publication reader
and one writer make exclusive modeled ownership equivalent to their actual
conflict. Multiple readers and cancellation of queued lock acquisition are
outside this scope.

The ordinary pipeline is claim, domain acquisition, bound publication,
finite scan, item write, durable request completion, queue settlement,
domain heartbeat join/release, queue heartbeat join, local retirement and
admission drop. The branch ordering follows `afterSettle`: failed work
releases before settlement, successful completion settles before release.
The walker must return and join before physical admission drops.

## Source correspondence does not automatically transfer progress

The intended admission/claim overapproximation adds contention histories,
but no state/action simulation proof currently relates this entire model
to the production queue. Such a proof must account for physical refusal,
durable claim refusal without a queue mutation, reservation lifetime and
request assignment. Safety transfer requires an explicit state and step
mapping. Liveness transfer additionally requires showing how fair source
executions map to model executions satisfying the specific action-fairness
and stable-demand premises; removing source paths or adding modeled paths
alone supplies no such argument. The model's progress checks therefore stay
conditional model results. Source schedules are independent conformance
evidence for the observations they actually drive.

A source stale-write schedule that invalidates both old queue authority and
the old domain fence tests rejection under that conjunction; it does not
isolate the higher-fence predicate. Likewise a direct durable-token call
with current owners does not schedule a facade call across local revocation.
The correspondence ledger identifies which predicate and timing histories
each source schedule actually distinguishes.

## Local revocation and durable authority are different facts

`LocalCurrent` includes local revocation and gates new publication dispatch.
`DispatchRejected` releases caller locks and enters cleanup if the local
permission was lost before dispatch. `TrueAuthority` checks durable queue
liveness, binding and the domain owner. `StoreApply` uses durable authority,
so a local cancellation never retroactively cancels a dispatched write.
Request result and terminal waiter update in the same model transition.

A completion timeout moves its lower operation into a detached slot and
frees caller-held locks. The detached operation can apply after local revoke
while its captured durable authority remains current. Durable release or a
new claim invalidates its captured identity permanently. A later acquisition
by the same worker never revives an older slot. This relational old/current
identity avoids a wrapping finite epoch. `TrueDetachedAuthority` also checks
the current durable queue and domain. It does not inspect local revocation.

`NoStaleCommit` records an actual pending-to-done application with false
true authority; merely trying an old call is not a violation. The stale
mutation preserves the old captured bit across durable invalidation and
makes the mutated completion check that bit alone. It therefore permits a
real pending request update after release, rather than merely removing a
redundant guard while another guard still rejects the write. The positive
safety guard is by construction; real storage schedules provide separate
source conformance evidence. Late application after local revoke before
durable release remains model-only: the current Rust durable-predicate
schedule does not pause a previously dispatched facade call across local
revocation.

The same bounded detached slot represents an ambiguous release: lower
application, reply and caller observation are independent. A timeout before
application can leave an exact old release outstanding while a retry runs;
after release/new claim its identity cannot clear a new owner. A lost reply
after applied release leaves the domain released. With one global finite
fault the model cannot have detached completion and ambiguous release at
once. Renewal lost acknowledgement is represented after durable application:
local permission is revoked, while the in-flight heartbeat still owns its
publication writer until caller return. Renewal ambiguity before application
and simultaneous independent ambiguous calls remain unqualified.

Local queue-loss notification similarly revokes dispatch without asserting
that the durable queue lease expired. `QueueLoss` is a separate budgeted
durable loss. Settlement/resolve can requeue a still-valid row. Local
`FinishRetire` drops the claim after the modeled settlement attempt; if that
attempt did not apply, the durable row can remain running and `qlive` until
`QueueExpiry`. Cleanup and durable queue retirement are separate properties.
Exact resolve branching and queue-heartbeat failure causes remain in the
correspondence gap ledger.

## Completion recurrence does not assume per-job starvation freedom

`Arrival(w)` represents new stable selected demand after that worker's prior
request is done and its attempt cleaned up. It resets result/waiter to
pending and is weakly fair. It does not replenish faults. A selected pending
request remains assigned to the same job. The boolean `parity` flips only
when a durable application changes a pending request to done. Duplicate
completions and arrivals never flip it.

`UsefulProgress` requires both `[]<>parity` and `[]<>~parity`, conditional
on neither worker ever cancelling or exhausting retries. Therefore infinitely
many actual durable completions must occur; an old completed state cannot
satisfy recurrence forever. Either worker may supply those completions. No
formula assumes or establishes that a particular job wins the exclusive
domain. The dedicated no-useful-progress mutation removes both scan-finish
fairness premises while leaving cancellation/exhaustion absent and checks
this exact formula. It guards the useful-completion observation against
vacuity. The fault-free contention configuration checks cleanup and queue
retirement without claiming per-job completion.

| Property | Meaning |
|---|---|
| `EachWorkerCleanup` | Every admission of either worker eventually reaches no admission, local claim or local domain handle |
| `SelectedCleanup` | Same property for `w1`, selected by local controls |
| `AttemptProgress` | Each durable running queue row eventually becomes queued, including expiry after non-applied settlement |
| `UsefulProgress` | Infinitely many global pending-to-done applications under stable repeated demand and no cancellation/exhaustion |
| `ResultAtomicity` | Each done result and terminal waiter agree |
| `AdmissionUntilJoined` | A busy blocking walker retains admission |
| `NoFictitiousRelease` | Domain heartbeat owns publication write until its own return |
| `NoStaleCommit` | No pending request changes to done under false captured durable authority |

Cleanup for one worker does not assume that the other worker releases its
lease or completes its work: a contender that loses acquisition settles,
retires and releases admission. The global useful-progress argument does
require fair dependency completion for the incumbent worker. Witnesses for
both completion and overlap are model reachability evidence, not per-job
liveness. In particular, domain overlap is model-only; the corrected paired
Rust schedule refuses the second claim and then completes sequentially.
Monotone history markers are enabled only in witness scenarios; they do not
bound execution or prune paths.

## Each waiting dependency has its own premise

Weak fairness is attached to individual actions in `Fair`, not to an entire
worker or to unconditional release of held locks. FIFO grants cannot barge
past a queued intent. Backend application, backend reply, caller return and
heartbeat drain are separate scheduled dependencies. Optional finite faults,
cancellation and detached lower application are not forced to occur.

| Boundary | Premise and scope |
|---|---|
| `ProbeReturn`, `WalkReturn` | PRM-fs: selected filesystem operations eventually return; no local finite bound |
| `Admission`, successful modeled claim | PRM-admission/model abstraction: two independently admitted capable workers may hold claims; production node-local capacity and durable same-root eligibility are omitted, not established by this premise |
| Authority actions | PRM-authority: selected local/remote authority checks return |
| Store and heartbeat apply/reply actions | PRM-backend: stable quorum/transport permits remaining normal calls to apply and return |
| Individual lock, dispatch, poll, join, retire and drop actions | PRM-runtime: continuously enabled selected task steps eventually run |
| `TimeObserved`, expiry, retry and wake | PRM-time: each selected backoff/expiry barrier is eventually observed; no precision, skew or delay bound |
| `Arrival` | Abstraction/stable demand: selected new work continues after cleanup |

`RenewalDeadline` revokes locally and signals loss while retaining the
in-flight heartbeat writer and awaiting its return. Revocation is not a
cleanup deadline. No bounded cleanup latency is established. A filesystem
call or backend can stall indefinitely if its explicit return premise is
removed; the external-stall and lock/livelock controls demonstrate that limit.
`TimeObserved` is always enabled, so TLC global deadlock checking cannot
substitute for any operation-local temporal property.

## Cancellation observes work polls, not arbitrary await interruption

The model abstracts scan cancellation, queue-loss notification, domain-loss
notification and admission pressure into the same `Cancel` action, enabled
only for `w1` under `Scenario = "cancel"`. The single cancellation witness
does not distinguish those signals; the real Rust schedules cover their
distinct paths separately. The abstract action may signal while scanning or publishing. The caller observes cancellation at
modeled process-work polls: a busy blocking walk still must return before
`ScanFinish`, and an already dispatched store call still must apply/reply or
time out before its caller moves on. A queued lock grant must also resolve;
cancellation does not erase its queue place. Local signalling is distinct
from durable expiry and does not roll back a detached write. General daemon
shutdown, interruption inside `spawn_blocking` and every possible await
cancellation site remain outside the claim.

## Bounds exclude simultaneous faults without an event horizon

Only fault budgets 0/1 and retry limit 2 are supported. All faults belong to
`w1`; this is one selected fault in the two-worker execution, never reset by
arrival. Fault-caused retries therefore cannot exhaust limit 2 in a positive
run. Normal heartbeats, contention, retry/backoff, arrivals and cleanup can
repeat forever. There is no horizon counter, `VIEW`, symmetry quotient or
state constraint. Witness history is finite observational memory.

The model retains the source's five release-attempt branch and durable TTL
fallback, but five failures and `DomainExpiry` are unreachable with budget 1.
No evidence for that fallback is claimed. Claim-resolution ambiguity, boot
adoption, migration, numeric fence arithmetic, Raft internals, multiple
concurrent faults, item timeout enumeration and a pre-application ambiguous
renewal remain unqualified. Conformance schedules and hash-checked source
anchors support particular boundaries; they do not prove full Rust
refinement or arbitrary-cluster deadlock freedom.
