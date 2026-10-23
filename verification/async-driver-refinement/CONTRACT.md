# Contract — what the selected product can establish

Companion to [README.md](README.md), which gives commands and receipt paths.
**Status: implemented, unqualified.** The candidate has no new Rust test,
control, or model-checker result. The earlier unrestricted Store-D closure
hit its historical state cap; it remains a failed historical run, not an
acceptance result. This claim replaces that traversal with a complete finite
product for the selected two-worker protocol. The narrowing is part of the
claim, not a performance optimization. `OrdinarySettlementPayloadTransfer`
is explicitly `unclosed`; the finite protocol does not establish that
production Yield/Retry requests carry the represented deadlines, errors, or
application-time behavior.

## D is a concrete bounded input protocol

There are two selected jobs, each with one paired request and library waiter,
one shared root, two nodes with physical capacity one, and a foreign domain
holder. Each job has at most two successful claims; each claim has at most
one renewal. Each domain owner acquires at most once. Four successful attempt
identities and two reserved mismatch identities are distinct; allocator
exhaustion is outside D because the attempt limit is reached first. There
are no new arrivals or cyclic demand. The 20,000 cap counts distinct exact
Store identities across the union of all seven main contexts, including
capacity-only oracle successors. Reaching it aborts generation. There is no
product-node cap; the product bundle is bounded by 256 MiB and the total
artifact by 512 MiB. A byte/state resource failure cannot emit a complete
relation.

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

## The proof closes a selected product, not arbitrary Store D

The Rust test `store::async_driver_product::actual_sql_two_worker_product_is_closed`
builds `scope="selected_two_worker_product"` from the source-bound driver
graph and actual SQL Store oracle. Rust certifies the finite product frontier;
it does not claim closure of every Store-D input independently of that
product. The previous raw Store-closure test and its closure-driven C4/C9
wrappers are retired, not skipped or used as gates. Existing primitive,
retirement, refusal, and backend conformance tests remain in the exact
registry in [store-tests.json](store-tests.json).

The imported oracle receives only the safety checks warranted by its selected
product coverage. Rust, Python, and TLA independently derive structural
candidate choices, D/sample eligibility, apply and capacity demands before
looking up oracle edges. Every demanded eligible input must have exactly one
actual outcome; a missing edge is fatal and cannot make capacity false.
Python independently checks full successor equality and reachability. TLA
preserves the original `Next` and fairness formulas while checking
`DemandCovered`, `CapacityAgrees`, `CertifiedReachable`,
`CertificateNextAgreement`, `FairPairAgreement`, and
`FairEnablednessAgreement`. Fair memberships are the unions for each complete
state pair; stuttering edges do not establish enabledness. A coverage failure
is never a valid negative control.

The completeness argument rules out a first omitted step. The exact initial
valuation is certified. At each certified predecessor, complete structural
choices and eligible primitive outcomes supply every original next step;
exact successor equality certifies its target. Conversely, each certificate
transition must satisfy the original action and actual primitive result.
Equality of every fair action's state-pair relation preserves its
nonstuttering enabledness and occurrences, so the original WF/SF formulas
retain their meaning. This argument remains conditional on the three
implementations and their source correspondence being correct; source hashes
alone do not prove those obligations.

The seven main contexts are `base`, `stable`, `weak_admission`,
`no_claim_win`, `withheld_join`, `old_poll_policy`, and `drop_before_join`.
They configure nine main TLC runs: safety, cleanup, selected-success,
nonvacuity, and one control run for each of the five changed assumptions.
`boundary_local_revocation` and `boundary_unknown_rollback` are two additional
sparse boundary controls, for eleven configurations total. Their counterexamples
are existential witnesses, not closure of unrestricted Store use.

| Context | Configurations |
|---|---|
| `base` | `safety`, `cleanup` |
| `stable` | `selected-success`, `nonvacuity` |
| `weak_admission` | `control-weak-admission` |
| `no_claim_win` | `control-no-claim-win` |
| `withheld_join` | `control-withheld-join` |
| `old_poll_policy` | `control-old-poll-policy` |
| `drop_before_join` | `control-drop-before-join` |
| `boundary_local_revocation` | `control-local-revocation` |
| `boundary_unknown_rollback` | `control-unknown-as-rollback` |

The product bundle binds the exact driver-graph bytes, the Rust source
registry, compiled and installed schema identities, checker and product
schema, both TLA modules, and all eleven configurations. Its raw Store rows
and canonical full state identities preserve the Rust replay boundary. The
256 MiB bundle and 512 MiB artifact bounds are the existing limits; there is
no inferred product-state ceiling from the 20,000 Store-state cap.

## Ordinary settlement payload transfer remains unclosed

The model closes the ordinary Yield/Retry representatives declared in D. It
does not claim they are the requests the production adapter sends. Its ordinary
Yield uses `not_before_ms=application episode.now`, no error code and no
checkpoint; Retry uses that same application time with error code `transient`.
These values define the bounded product input, not a source-backed transfer
theorem.

The production lifecycle creates Yield deadlines from captured release time:
domain and bind refusals use saturating `+5000 ms`, while pressure/domain-loss
interruption uses `+1000 ms`. Retry stores the release sample plus a jittered
delay computed from raw job-ID bytes and the adapter's failed-attempt snapshot,
and sends `library_execution_failed`. The settlement method later
takes a fresh `unix_ms()` for Store application. These capture, payload, and
application times are distinct.

This difference affects durable rows and later behavior. SQL stores
`not_before_ms`, `last_error_code`, and error history; claim eligibility
requires `not_before_ms <= now_ms`. Store validation rejects an ordinary
Yield/Retry whose captured deadline is earlier than its application `now_ms`.
The nine-table state identity preserves these fields, so the difference cannot
be erased as a harmless tag or string normalization. `OrdinarySettlementPayloadTransfer`
therefore remains a source-bound `unclosed` boundary covering deadline/delay
construction, error text, capture timing, local dispatch, and Store
application timing. No noninterference premise is asserted.

The retained production T4 Kani target proves lifecycle `+5000`, `+1000`, and
retry-delay policy for its inputs. It does not prove daemon capture/mutex
scheduling, retry-delay construction for this adapter fixture, the selected
Store time transfer, or cross-layer simulation. The bounded product can close
its declared representative protocol while this source-transfer obligation
remains unclosed.

C9 replays the legal `Pressure → RevokeDomain/CancelWork → JoinFailed →
DropPublisher → ReleaseInterrupted → SettleYield` control order and its
trigger mutation under the declared representative product. Its deadline is
not payload-faithful to production's captured `+1000 ms` Yield. The C9 marker
must carry `scope="declared_representative_product"` and
`ordinary_settlement_payload_transfer="unclosed"`; this witness is not
actual-adapter payload evidence.

The product frontier retains exact finite choices and actual Store outcomes.
Rust records demands and branches; Python independently derives structural
candidates and D/sample/apply/capacity eligibility before edge lookup. TLA
checks demand completeness and capacity agreement against the original
operators. Missing eligible inputs, unchecked exclusions, missing successors,
or an incomplete frontier reject the bundle. Explicit attempt, renewal,
domain and no-future-boundary cutoffs remain visible and exclude only the
named input, not every transition from that state.

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
| C4 | `store::async_driver_product::product_prefix_completion_mutation_fails_last_request_atomic_retirement` replays the protocol-valid actual-SQL prefix with the existing trigger mutation and preserves the unmutated replay through the first `LastRequestAtomicRetirement` failure |
| C9 | `store::async_driver_product::product_prefix_reservation_mutation_fails_reservation_conservation` replays the protocol-valid actual-SQL prefix with the existing trigger mutation and preserves the unmutated replay through the first `ReservationConservation` failure |
| `control-local-revocation` | Separate Store-boundary model rejects the stronger false `NoCommitAfterLocalRevocation` while durable freshness holds |
| `control-unknown-as-rollback` | Separate actual-application boundary model fails `ResultAtomicity` when lost acknowledgement restores the preimage |
| Strict rejection controls | Source/schema drift, missing driver target, missing demanded oracle edge, forged exclusions, and incomplete certificate coverage reject by name |

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
The selected primitive tests retain full-row preimage checks for refused
operations, and `resolve_claim` checks unchanged durable rows. These source-
bound assertions support the imported oracle; they do not restore the retired
arbitrary Store-D closure claim.
