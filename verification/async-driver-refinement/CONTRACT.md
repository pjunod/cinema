# Contract — what qualification would establish

Companion to [README.md](README.md), which gives commands and receipt paths.
**Status: implemented, unqualified; independent review and execution pending.**
The finite-domain narrowing below is part of the claim, not a performance
optimization that may be omitted from a report.

## D is a concrete bounded input protocol

There are two selected jobs, each with one paired request and library waiter,
one shared root, two nodes with physical capacity one, and a foreign domain
holder. Each job has at most two successful claims; each claim has at most
one renewal. Each domain owner acquires at most once. Four successful attempt
identities and two reserved mismatch identities are distinct; allocator
exhaustion is outside D because the attempt limit is reached first. There
are no new arrivals or cyclic demand. The state cap is 20,000; reaching it
aborts generation rather than emitting a closed relation.

All signed and unsigned integer values remain concrete. Only opaque generated
job UUID strings are replaced by J0/J1. Clock steps use the next concrete
current/captured authority-expiry neighbour (-1, equality, +1), respecting
signed limits. There is no timestamp quotient, clipping or fixed horizon.
MAX arithmetic and excluded API-input partitions require separate named
conformance evidence.

The reduced ordinary input protocol uses the latest captured revision of
each claim, while preserving historical revisions in the state. Bind and
complete use leases owned by the queue-token node. Ordinary settlement inputs are
Yield, Retry and Cancel. Retirement Yield uses the distinct disposition 3
with `Some("worker_retired")`; it and exact-identity `resolve_claim` are
offered for every captured original token, including historical revisions. A node acquires the domain only while it has selected
durable running ownership, including expired retained ownership; the foreign
holder remains independent. Stale revisions, wrong-owner binding/completion,
Fail and Stop are separate conformance partitions, not closed graph inputs.

The importer independently enumerates this input domain for every state and
requires exactly one edge per input. It does not compute SQL result guards.
All operations must occur in the coverage inventory; known but unreachable
result classes are listed as unobserved. Explicit attempt, renewal, domain
and no-future-boundary cutoffs remain visible. A cutoff excludes an input,
not every transition from that state.

## Three claims have different premises

**Safety.** Every reachable product state satisfies physical admission through
join, join before local release, at most one currently
unexpired durable shared-root queue authority, completion monotonicity and
last-request atomic retirement.
All imported Store states independently satisfy retirement/reservation
assertions. Every accepted Store edge independently satisfies concrete
queue/domain freshness and monotone fence checks. Expired running owners can
have zero reservations after global pruning; only active owners require all
three, and retired owners require zero. The importer recomputes `active` from
`state == running` and concrete `expiry > now`, then independently checks
exclusivity and completed-request state/owner/reservation atomicity. Expired
Running rows remain represented, but no independent complete-residue
conservation guarantee covers the window before reclaim/prune. Each domain
owner acquires once; release/expiry never restores that budget, so same-owner
reacquisition cycles are excluded. The foreign holder is an independent third
domain owner, not a third worker node.

NoTwoSourceReaders counts durable records with state running and queue expiry after the sampled time; it does not bound physical readers while expired work awaits cancellation or join.

The local queue handle can remain held after durable last-request completion
has cleared ownership and reservations. Local domain release or admission
drop does not imply durable expiry. `AdmissionDropped` is local attempt cleanup;
durable quiescence L1d is not claimed.

`GuardBeforeRevoke` is a structural consistency check of this operation-boundary
model. It is true by construction: `readLock` is set only for in-flight
`PollWork`, cleared at `Reply`, and revocation can be marked only at a later
`DriverReturn`. It provides no independent evidence for the actual adapter's
publication-lock implementation; that remains a reviewed contract.

**Attempt cleanup.** For each actual claimed driver instance, identified by
node, selected job and immutable claim ID (preserved across renewal),
`[]WithinEpisodeCapacity => AttemptCleanup`, under `FairRuntime` and
`FairExternal`. `WithinEpisodeCapacity` requires a matching actual Store
edge for a sent SQL request. It is false at a domain/input exclusion; weak
fairness cannot manufacture an absent edge. Safety includes those states,
while this liveness implication does not claim cleanup beyond the declared
capacity. Each internal driver edge descends a rank of the actual control DAG;
effect-tail length alone is not the rank across phase changes.

`FairRuntime` assumes weak fairness of request dispatch, backend application,
reply and caller return, including token-mutex/guard service at the modeled
operation boundary. `FairExternal` separately assumes work and child/join
return and heartbeat drain. Retirement primitive service is weak fairness
of the union of actual application and an allowed error return; there is no
fairness premise forcing an eventually successful acknowledgement. These assumptions are not consequences of weak fairness on a Tokio
task. The production loss > work > tick poll policy structurally prevents a
ready pressure tick from masking a ready work poll. The old policy control
removes the modeled work-poll premise explicitly; it invents no tick action
inside the fixed driver.

**Both selected completions.** The stable-success configuration fixes the
local candidate pairing node-a→J0 and node-b→J1. It retains every real SQL
refusal but permits domain/bind application only when the actual success edge
is available. There is no clock advance, owner removal or foreign acquisition;
external supported work succeeds by actually completing its paired request.
Claims and domain release have reliable application; a completion
acknowledgement may be lost after application, and cleanup retains that
committed result.

The stable-success model permits a lost post-commit completion acknowledgement
to turn `WorkSucceeded` into `WorkFailed`. The driver can then request
`SettleRetry` after the paired request is already durably done. The actual
Store input enumerator retains the latest captured token for each claim after
COMPLETE, so this Retry still has a refused self-edge. Both `StoreApply` and
`WithinEpisodeCapacity` admit that matching input without requiring an
accepted settlement result. The reply flip therefore does not make cleanup
vacuous because of a missing Retry edge; all other capacity and return
premises still apply.

Physical availability recurs (weak fairness of making a denied
permit available), and local admission is strongly fair because that
availability can be intermittent; durable claim success has a separate strong-fairness
premise. This is an availability/success-before-budget premise, not a theorem
that arbitrary fair errors eventually become successful. It yields eventual
completion of these two fixed requests, never infinitely many fresh results.

A separate reachability witness must reach both durable requests freshly
done and both physical admissions dropped. No positive receipt is complete
without that witness. Removing claim-win and backend-claim application service
together is the missing-opportunity control; it does not claim SF is necessary
when opportunity is eventually continuous.

## Source transfer ends at named boundaries

The driver graph enumerates actual production `drive` over every reply tag
at every reachable control, including explicit rejection without mutation.
`next_control` takes no numeric payload. The universal numeric tag-independence
composition lemma is reviewed source reasoning, not a new all-i64 proof.
Representative integer regressions do not replace the unchanged lifecycle
arithmetic proof or its source gate. The graph exporter renders `Work` through Rust `Debug`. The importer validates
an exact six-string registry and maps it to stable model tags (`idle`,
`running`, `completed_failed`, `completed_succeeded`, `joined_failed`,
`joined_succeeded`). `JoinReturn` uses those tags. A formatting change outside
that registry rejects graph import; it cannot silently widen the allowed join
outcomes. This exporter/importer format contract remains source-bound.

The actual control graph is acyclic;
typed table edges alone do not establish fairness of adapter awaits.

The product represents physical admission before ClaimSend→Apply→Reply→Return;
a refused/unknown claim never enters the post-claim driver or emits Yield.
Each driver request then has dispatch/application/reply/return stages. Lost
replies retain durable application; an ambiguous local release can also have
no durable application in the cleanup configuration. Faults per attempt are
finite. There is no assumed rollback on timeout.

Supported work is an opaque operation ending at the actual paired COMPLETE
transaction; its publication read guard spans that modeled operation and
must be returned before revoke. Token-mutex exclusion and current-token
replacement across renewal are modeled adapter contracts. The selected
heartbeat envelope permits acknowledged renewal or ambiguous retention of
the old token before/drain; it does not close every renewal-resolution
classifier branch that can clear the local token. Inner publication
lock granularity, cancellation and child spawn/join behavior, catalogue item
inserts, retry classifiers, selection outside the fixed stable pairing,
SQLite atomicity/visibility, Raft/Hiqlite execution and Tokio/runtime internals
are reviewed or conformance contracts, not a full Rust simulation theorem.
The SQL graph claim action reads its expected revision immediately before
the validated call. Captured pre-claim read/application races and the
adapter error-to-reply-tag classifiers are conformance boundaries; their
absence from this finite relation is not a full adapter refinement proof.
Unknown-claim reconciliation is represented by abandoning local admission
without rolling back the Store; no unsupported success response is invented.

## Retirement composition and its source boundary

`localToken` represents the mutex-protected `Option<JobToken>`, separately
from the driver's held queue handle. Observed accepted ordinary Yield/Retry
clears that option. Refused/error/unknown settlement retains it, and COMPLETE
always retains it even when last-request completion clears durable ownership.
An unacknowledged actual renewal may leave the old local token; the heartbeat
reconciliation classifier remains a reviewed adapter boundary. A renewal may
finish before or during heartbeat drain, but not after retirement takes the
mutex-protected token. The token is taken before the first wall sample;
retirement ignores local lost/deadline/publication guards.

`RetireQueue` first drains the heartbeat, then takes the local token. None
finishes locally. Some samples the first wall time and calls actual retirement
Yield. The daemon can instead choose Cancel when its local cancelling flag
is set; that flag/environment branch is outside selected D and has separate
cancellation conformance. Each call has distinct send, application, reply and observation steps;
other worker/Store actions may interleave. Observed true finishes. False or
error samples a second wall time and calls actual `resolve_claim` with the
original job/node/boot/claim/fence and the first sample as `dispatched_at_ms`.
A successful resolve always takes the third wall sample, including Settled,
LostOwnership and ExpiredOrPruned. Running supplies the returned exact token
to one second retirement Yield; CancelRequested would supply a Cancel token,
but selected D has no queue-cancellation environment, so that branch needs
separate conformance. Other resolutions finish locally. A second true/false
finishes; error is logged by `finish`, which still returns. No retry-success
premise is needed for this bounded local sequence. The existing one-fault
per-attempt envelope applies to ambiguous calls and clock failures too;
multiple such fault episodes are excluded.

The SQL graph supplies separate actual settle and logical resolve primitives,
not an atomic retirement composite. Its primitive requests sample current
`episode.now`. The temporal model records three samples but admits first and
second settlement application only when the corresponding captured sample
still equals current Store time. Resolve requires its second sample
(`now_ms`) to equal current Store time at application. These are explicit
D/capacity restrictions; a clock advance between capture and application can
falsify `WithinEpisodeCapacity`. They do not close captured-wall-time races.
The first sample remains the daemon's conceptual `dispatched_at_ms`, while
the producer uses current time as that field's representative. A reviewed
source noninterference lemma permits this difference: with fixed
`Some(original.fence)`, fixed `now_ms` and the same authoritative read
outcomes, dispatch time cannot affect resolution control/results. Its sole
use in `resolve_claim` is in the `fence.is_none()` window guard, and neither
SQL query reads that JSON member. This is source reasoning, not literal
request equality or an all-Rust proof; first and second samples need not be
equal.

`resolve_claim` is a source-derived logical API edge. Its no-owner branch
contains two separate authoritative QueueSql reads. Internal interleavings
between those reads are a named conformance boundary, not graph closure or a
single-transaction claim. Resolution's returned state/result reference is
opaque because retirement ignores it. Python validates returned immutable
attempt identity, stable token indices and unchanged durable projections;
it does not independently reimplement SQL resolution guards.

Four exact daemon cases cover acknowledged settlement's durable invariance,
retirement after local revocation while durable authority is valid, resolving
an unobserved renewal revision, and resolving cancellation revision after
child join. They use actual finish/SQLite paths. They do not establish absence
of SQL calls, COMPLETE/error/successor branches, or either internal-read
schedule. Store primitive histories separately cover returned identity and
read-only resolution; source-bound conformance remains distinct from the
finite temporal claim.

Canonical state hashes bind the Rust full-row snapshot/replay check. Compact
JSON projections cannot independently reconstruct hidden rows. Compiled
migration and installed-schema hashes bind effective schema bytes; Python
checks the exported installed inventory digest but does not prove SQLite.

## Controls have named causal failures

| Control | Required evidence |
|---|---|
| Old source poll order | Isolated actual-source swap; exact leaf selector regression fails `SELECTOR_WORK_BEFORE_TICK`, exit 101 |
| `control-old-poll-policy` | Sole temporal property `ConditionalAttemptCleanup`; work-poll premise removed |
| `control-withheld-join` | Sole temporal property `ConditionalAttemptCleanup`; external join withheld |
| `control-weak-admission` | Sole temporal property `BothSelectedCompletion`; SF weakened to WF |
| `control-no-claim-win` | Sole temporal property `BothSelectedCompletion`; claim application/opportunity service removed |
| `control-drop-before-join` | Actual local permit dropped at CancelWork return; `AdmissionUntilJoined` fails |
| C4 / C9 | Actual SQL trigger mutants fail `LastRequestAtomicRetirement` / `ReservationConservation`, not table equality |
| `control-local-revocation` | Separate Store-boundary model rejects the stronger false `NoCommitAfterLocalRevocation` while durable freshness holds |
| `control-unknown-as-rollback` | Separate actual-application boundary model fails `ResultAtomicity` when lost acknowledgement restores the preimage |
| Strict rejection controls | Source hash drift, missing driver target and missing Store input are rejected by name |

Temporal counterexamples require a lasso trace and exact temporal exit code;
safety controls require their named invariant and exact safety exit code.
Causally related failures are allowed, compiler/time/resource failures are not
semantic counterexamples. TLC complete positive exploration and per-config
state counts are mandatory. These are finite-model results using the pinned
TLC fingerprint engine; neither graph-only inventory nor definitions of
checks constitute execution evidence.

The producer's nine-table full-row snapshot is the canonical state identity
for this selected protocol. It is not the whole SQLite database: compiled
migration/installed-schema hashes bind additional schema objects but do not
add their row contents to state identity. All selected queue mutations use
production QueueSql; no selected call routes through QueueTransaction. A
future routing/dependency change requires renewed review. Sequential same-
writer savepoints do not prove commit visibility, concurrent writers, engine
internals or replicated transactions. Any future selected primitive that uses
QueueTransaction must fail closed rather than silently inherit this harness.
The original BFS already compared full canonical preimages after refused
operations, and Python required refused self-edges; there was no prior
refusal-closure gap. The added direct-call mutation observer serves non-graph
primitive tests, while resolve independently checks unchanged durable rows.
