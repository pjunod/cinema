# Contract — what the selected product can establish

Companion to [README.md](README.md), which gives commands and receipt paths.
**Status: schema 4 wire implemented; feasibility and qualification pending.**
No completed schema 4 product or TLC receipt exists. The schema 3 run 5911
stopped at its conservative serialized-output estimate; it is incomplete
historical evidence, not a semantic counterexample or acceptance result. This
claim replaces the unrestricted Store-D traversal with a complete finite
product for the selected two-worker protocol. The narrowing is part of the
claim, not a performance optimization. Ordinary settlement payload and time
transfer is represented from the selected production adapter and Store path.
Its bundle boundary remains `pending`; a completed accepted receipt may mark
it `closed_bounded` only for the selected normal-clock product. No full
Rust/Tokio/cluster trace theorem is claimed.

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
product-node cap; the schema 4 compact product bundle and generated
`SourceTables.tla` member are each bounded by 256 MiB, and the total retained
and extracted artifact remains bounded by 512 MiB. A byte or Store-state
resource failure cannot emit a complete relation. Feasibility under these
unchanged limits remains unknown.

All source integer values and raw generated job IDs remain concrete. Clock
steps use exact retained wall/monotonic authority, payload, eligibility and
recovery-wake neighbours (-1, equality, +1). There is no timestamp quotient,
clipping or fixed horizon. The pinned TLC evaluator uses signed 32-bit
integers. Independent arbitrary-precision Python must certify every consumed
number and candidate arithmetic intermediate, including disabled candidates,
and equality to source checked/saturating arithmetic before TLC runs.
`UnsupportedTlcNumericDomain` fails qualification globally; it never removes
a D input or makes capacity false. All-i64/MAX policy and excluded-input
obligations retain their separate source/tool applicability.

The `ordinary_settle` input uses a saved captured claim token, including a
historical revision actually copied under the local mutex; SQL determines
whether the token is refused. Its payload is Yield or Retry. Bind and complete
use leases owned by the queue-token node. Cancel remains a separate driver
retirement input. Retirement Yield uses the distinct disposition 3
with `Some("worker_retired")`; it and exact-identity `resolve_claim` are
offered for every captured original token, including historical revisions. A node acquires the domain only while it has selected
durable running ownership, including expired retained ownership; the foreign
holder remains independent. Stale revisions, wrong-owner binding/completion,
Fail and Stop are separate conformance partitions, not closed graph inputs.

## The proof closes a selected product, not arbitrary Store D

The Rust test `store::async_driver_product::actual_sql_two_worker_product_is_closed`
builds `scope="selected_two_worker_product"` from the source-bound driver
graph and actual SQL Store oracle. Rust emits the finite product frontier certificate;
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
amends `Next` and fairness for ordinary settlement and clock service while checking
`DemandCovered`, `CapacityAgrees`, `CertifiedReachable`,
`CertificateNextAgreement`, `FairPairAgreement`, and
`FairEnablednessAgreement`. Fair memberships are the unions for each complete
state pair; stuttering edges do not establish enabledness. A coverage failure
is never a valid negative control.

The completeness argument rules out a first omitted step. The exact initial
valuation is certified. At each certified predecessor, complete structural
choices and eligible primitive outcomes supply every declared next step;
exact successor equality certifies its target. Conversely, each certificate
transition must satisfy the original action and actual primitive result.
Equality of every fair action's state-pair relation preserves its
nonstuttering enabledness and occurrences, so the declared WF/SF formulas
apply to those independently checked relations. This argument remains conditional on the three
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
and canonical full state identities preserve the Rust replay boundary. There
is no inferred product-state ceiling from the 20,000 Store-state cap.

## Schema 4 shares wire data without changing the logical product

Schema 4 stores repeated exact JSON values in `value_pool` and fixed-width,
37-column Worker rows in `worker_pool`. The six Worker references are `claim_snapshot`,
`claim_request`, `claim_recovery`, `driver_settlement`, `adapter_reply`, and
`ordinary`; the value is still present when it is JSON `null`. Product-state,
edge, demand, and enabledness rows use the columns and reference targets
declared in `product-schema.json`. That registry defines the exact row order;
it is checked against the reader's closed contract rather than used to alter
semantics.

Every reference is a zero-based integer of the required type and in range;
booleans do not count as integers. The reader rejects wrong row widths or
types, dangling references, duplicate pool values, and changed canonical
bytes or hashes. Pool indexes are wire positions, not semantic identities.
Interning compares complete canonical values, including JSON types and array
order, and resolves hash collisions by comparing the full values. Rust keeps
pools immutable and append-stable, uses compact references only for an exact
state key, and retains the original `real_drivers` payload.
`canonical_sha256` remains the hash of the old full logical product payload;
Store identity still hashes its original `canonical_json` bytes.

The Python reader resolves shared entries lazily as independent checks consume
the logical values. Candidate generation, exact successor equality, demands,
reachability, pair fairness, enabledness, and witness checks still operate on
the complete decoded product. Generated `SourceTables.tla` reconstructs the
same logical `ProductStates`, `ProductIndex`, `ProductEdges`, `ProductOutgoing`,
and `ProductEnabled` values. It builds exact index keys from decoded states
and shares repeated expressions; the schema change does not alter logical
constants, `Next`, fairness, or the independent transition relation.

The 256 MiB product-bundle limit counts exact compact JSON bytes, including
pools, references, framing, and metadata. The `SourceTables.tla` member has
its own unchanged 256 MiB limit, and retained/extracted artifact bytes remain
bounded by 512 MiB. Existing host-memory, JVM, and timeout limits also remain
unchanged. Conservative serialized-component accounting is a byte estimate,
not a claimed RSS or heap bound; a bounded exact serialization check decides
whether the output fits. Run 5911's schema 3 estimate was 268,436,757 bytes
against a 268,435,456-byte limit, with 74,293 product states, 26,397 expanded,
and 47,895 pending. That cutoff is historical and incomplete, not a
counterexample. Schema 4's fit and the proof remain pending.

## Initial claim recovery and timer service

A lost claim reply retains its actual original request and dispatch sample.
Recovery uses real fence=None resolution, a real committed job reread for
Running, and real Cancel settlement for CancelRequested. LostOwnership and
Settled can return None; inconclusive outcomes retry after the source 250 ms
sleep until the original 30,000 ms monotonic deadline. The Store resolution
window is separately 120,000 ms. Internal queries in each iteration form an
explicit zero-delay summary; their inner interleavings remain outside scope.
No second stochastic failure is available after the lost reply used its fault.

Persistent `(job,node,selected claim_id)` loss keys survive Idle/recycle.
Four selected identities and two nodes permit at most eight recovery episodes
in this finite input domain. Refused RPCs can repeat without a count cutoff,
but create a retained deadline anchor only when a previously unused loss key
is consumed. Source retry wakes become exact clock anchors. `ClaimRecover`
service and `ClaimRecoveryWake/global` timer progress are explicit fairness
premises. This bounds the experiment's fault histories; production has no
corresponding failure limit.

## Ordinary settlement transfer and its bounded evidence

The product carries the actual adapter payload and its separate time samples.
Domain-refusal Yield uses the wall sample taken after the refusal; bind-refusal
Yield uses the release sample. Both add saturating 5000 ms. Interrupted Yield
uses captured release time plus saturating 1000 ms. Retry
uses captured release time plus the source retry delay, computed from raw job
ID bytes and the immutable failed-attempt snapshot, with error code
`library_execution_failed`. The driver forwards the settlement unchanged.
The ordinary adapter then takes the token mutex, checks queue-loss, monotonic
deadline, and token presence in that order, samples request `unix_ms()`, and
calls Store while holding the mutex. Only observed `Ok(true)` clears the local
token. Store validates the saved deadline against request time, distinguishes
invalid Yield/Retry times from a refused CAS, and claim eligibility follows
the durable `not_before_ms`. The later wall clock does not rewrite the saved
SQL predicate.

The model records pre-call local exits, clock and transport errors, actual API
application/refusal/validation outcomes, possible lost replies, and observed
results. Historical tokens remain eligible after capture; the actual Store
outcome decides whether they are accepted. All exact integer and raw identity
fields remain in product identity. Three source-bound Rust cases cover
payload/time conformance, local guard/response outcomes, and queued eligibility
progress. They are requirements, not results in this candidate:

- `actual_ordinary_settlement_payload_and_time_conformance`
- `actual_ordinary_local_guard_and_response_conformance`
- `actual_queued_eligibility_progress_witness`

The temporal relation includes ordinary adapter service and wall and monotonic
time movement. In stable-success contexts, steps stay within live lease and
deadline windows. `EligibilityAdvance` is weakly fair when a safe next-boundary
step toward queued eligibility is enabled. This is an explicit timely-service
premise, not a claim that a real scheduler can freeze time. Seven main
contexts, two sparse boundary contexts, and eleven configurations remain; so
do the 20,000 exact Store-state cap and existing 256 MiB bundle/512 MiB artifact
limits. No product-node cap is added.

The clean stable contexts may never reach delayed incomplete queued work, so
`EligibilityAdvance` there can be unreachable. A separately reported
`eligibility_mechanics_witness` must independently follow the complete base
certificate from an actual accepted ordinary Yield/Retry to the same incomplete
queued job becoming eligible, preserving its deadline and remaining claim
attempt. Every wall step in its suffix must satisfy the exact next-boundary
and all-worker stable safe-window checks. This existential witness establishes
eligibility mechanics; it does not establish stable fairness nonvacuity or
universal recovery from failed attempts.

Qualification inventories and partial receipts retain
`ordinary_settlement_payload_transfer="pending"`. A complete accepted
receipt may derive `closed_bounded` only after product validation, every
source-bound actual-settlement and causal-control case, and all eleven
configurations pass. It binds the product bundle, raw driver graph, source
registry, commands, logs, toolchain, models, and configurations. This boundary
is not a full Rust/Tokio/cluster trace theorem; the separate T4 Kani policy
obligation remains distinct. Existing source/tool guards determine whether
its earlier results remain applicable.

C4/C9 are paired source-trigger controls with an unmutated prefix replay and
the named mutation failure. C9's ordinary Yield carries its captured +1000 ms
deadline and independently sampled request time. These controls do not replace
the positive payload, guard, and eligibility cases. Renewal uses a zero-delay
logical matching renewal plus actual resolution summary with both demanded
outcomes. Inner renewal-resolution interleavings remain an explicit source
boundary, and no full resolution loop is claimed.

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
is available. Wall and monotonic advances are permitted within the explicit
live lease and deadline windows; eligibility progress has its own fair
relation. Owner removal and foreign acquisition remain excluded from this
selected-success scenario; external supported work succeeds by actually
completing its paired request.
Claims and domain release have reliable application; a completion
acknowledgement may be lost after application, and cleanup retains that
committed result.

The stable-success model permits a lost post-commit completion acknowledgement
to turn `WorkSucceeded` into `WorkFailed`, after which the driver can request
Retry for a paired request that is already durably done. The product carries
the captured token and exact Retry payload into the actual Store oracle; the
source-derived Store result, not a synthetic successful response, determines
the successor. Demand and capacity checks require the matching outcome edge.

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
Representative integer regressions do not replace the retained lifecycle
arithmetic policy proof or its source gate. The graph exporter renders `Work` through Rust `Debug`. The importer validates
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
heartbeat envelope includes a zero-delay logical matching renewal followed by
an actual resolution summary dependent on the renewed-or-retained result; both
actual queries and their outcomes are demanded. This omits interleavings
inside renewal and resolution and is not a full resolution loop. Inner publication
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
