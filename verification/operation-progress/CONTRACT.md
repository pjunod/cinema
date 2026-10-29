# Operation progress — what the finite model can establish

**Status:** independently approved; all nine configurations qualified. Delivery and the current main gate are recorded in the status page.

Companion to [README.md](README.md) (commands and configuration registry)
and [WAIT-MAP.md](WAIT-MAP.md) (source waits and external boundaries).
This contract defines the meaning of `OperationProgress.tla`; it does not
extend the original [safety contract](../durable-jobs/CONTRACT.md).

## Composed main/effort source correspondence — 2026-10-28

The resolved merge combines maintenance `96ab7f0e82de` and main
`66a33d723096`. The exact resolved source bytes are recorded in `mapping.json`;
this source-only review does not qualify the new merge or transfer either
parent's execution receipts. Earlier review and model qualification identities
below remain historical.

Main's caption-kind demand predicates, managed-v2 active resource view, exact
predecessor/transaction guards and strict rewind fixtures are retained. The
selected library kinds keep their source-domain/provider requirements. Caption
work can compete for existing resources; caption fairness and online schema
upgrade correctness remain outside these finite models. The progressive speech
path retains the effort's original restart admission through its capacity wait
and the admitted work; this adds no native-close or resource-refund witness.

The effort's `finish_original_controller` anchor, retained heartbeat/token mutex
and acknowledged/resolved retirement abstraction remain authoritative. The old
move-consuming `finish(mut self)` anchor is not restored. Failed joins,
ambiguous retirement, last-alias release, native closure and the separate title
requalification read/bind TOCTOU keep their recorded limits. Queue renewal and
resource-currentness SQL are byte-identical to both parents; the selected
fragment admission remains try-only. Conformance registration is unchanged. The
formal-publication module is identical to maintenance; its sole difference
from main relocates an Arc import into the replicated test module.

Six distinct mapped files require new whole-file pins: the shared queue,
replicated store, SQLite store, conformance parent, daemon state and transcode
manager constructor. Every mapped anchor remains unique. Both parents' source
reviews and exclusions, including main's subtitle/managed-v2 authority and the
effort's title/Linux-cfg review, are preserved. Models, configurations, bounds,
fairness premises, properties, action/check/test inventories and validators do
not change. No tests, compilers or models ran for this correspondence review;
no new acceptance or qualification is claimed.

## Subtitle resource admission — scoped refresh on 2026-10-28

Codex reviewed `c3d764a01` against `f2120bd9a`. Five mapped whole-file pins
change: the shared queue adds exact demand-bound caption admission, the
replicated store installs a guarded managed-resource v2 view, and the transcode
manager adds a bounded cooperative wait used only by demanded progressive
captions. The selected `admit_fragment` path remains try-only. Library worker
admission, claim, domain/token/publication lock order, completion, settlement,
heartbeat retirement and physical-admission lifetime are unchanged.

The v2 view is now a mapped input because it supplies current source-reader
requirements. `library_scan` and `metadata_refresh` keep their source-domain
and provider rows. Artifact-only text model setup loses its media-reader row;
qualified caption demand can use the existing reserved live slot without
changing the two-reader/one-speculative-reader limits. Competing caption work
can cause selected background work to yield; this package does not establish
per-job source scheduling fairness or caption progress. Its stable-demand and
external-return premises remain explicit.

Every existing mapped anchor remains unique and unchanged. Model, configuration,
fairness, property, bound and toolchain bytes are unchanged; no model delta is
needed for these out-of-scope caption paths. Online schema upgrades, caption
admission and playback require their own runtime evidence. This source mapping
review does not renew historical qualification or claim fresh TLC execution.

## Useful completion and cleanup are separate observations

`request = "done"` means a durable, non-pending library request result
(completed or failed) and its waiter's terminal update were applied atomically.
The model's `StoreApply` performs both updates. `ResultAtomicity` and
`NoFictitiousRelease` are model-consistency guards that hold by construction:
they pin the chosen update and lock-ownership shapes, not the correctness of
SQL atomicity or Rust lock handling. Source evidence belongs to the wait map
and mapped excerpts separately. Item insertion is an intermediate milestone.
A successful `process_library_requests` turn settles `Yield(now)` and requeues
the job; it does not put the job in a terminal successful state.

`Cleanup` means the selected worker holds no physical admission, domain
lease or local queue claim. Its durable queue row can still be running after
a lost/non-applied settlement. That row becomes eligible only after expiry.
Admission stays held until the blocking walker returns, work joins, domain
heartbeat drains and release returns, queue heartbeat drains and the local
claim retires. Cancellation can reach cleanup with a pending request. The
cancellation witness enables `Cancel` only during scanning; it does not cover
interruptions at every source await or a general shutdown path.

| Formula | Exact meaning | Configurations |
|---|---|---|
| `UsefulProgress` | If the execution never cancels and never exhausts retry budget, pending selected request eventually has a durable result | `progress.cfg` |
| `AttemptProgress` | Running selected queue row eventually becomes queued, including expiry after a non-applied settlement | Both positives; lost-settlement control |
| `AttemptCleanup` | Every state holding admission eventually reaches `Cleanup` | Both positives; other three controls |
| `AdmissionUntilJoined` | A busy blocking walker still has physical admission | Positives and witnesses |
| `NoFictitiousRelease` | A publication writer remains owned by the explicit domain-renewal task until its caller return | Positives and witnesses |
| `NoSuccessWitness` | Negated target: durable result, terminal waiter, completed cleanup | Success witness |
| `NoReconcileWitness` | Negated target: durable result, lost acknowledgement, authoritative non-pending reread, cleanup | Reconcile witness |
| `NoCancelCleanupWitness` | Negated target: selected cancellation, cleanup and still-pending durable request | Cancellation witness |

With fault budget 1 and retry limit 2, the normal positive cannot exhaust
its retry budget: each failing selected attempt consumes a fault. Exhaustion
has its own state and does not alter request/waiter state. The conditional
formula never equates exhaustion with durable completion. The contention
positive deliberately checks cleanup/retirement only: a competitor can
reacquire the domain indefinitely, and no source queue-order guarantee
justifies per-job starvation freedom. `TypeOK` bounds retries only for the
registered fault budgets 0/1 and retry limit 2. Larger fault budgets are
unsupported: retry increments are not capped, so widening the constants
requires review and an explicit retry bound/capping decision.

## Dependencies are scheduled individually

```text
probe -> admission -> claim -> domain -> token mutex -> publication read
                                                 |          |
                                                 |          v
                                                 |      bind backend
                                                 v
scan join -> item write -> completion -> result+waiter -> Yield(now)
                                                          |
                                                          v
drop admission <- retire token <- queue heartbeat join <- domain release
                                                           ^
                                                           |
                                               domain heartbeat join
                                               publication write
                                               backend caller return
```

The actual code takes token mutex before publication read for bind/complete.
The domain renewal holds publication write across its backend await and never
takes that token mutex. Queue renewal holds the token mutex across authority
and backend waits. Intent queues `tq` and `pq` contain at most one selected
worker intent and one heartbeat intent each. Grants inspect the queue head
and holder; neither acquisition nor release is fused with backend application.

Tokio 1.53.1 documents FIFO for
[`Mutex`](https://docs.rs/tokio/1.53.1/tokio/sync/struct.Mutex.html)
and a FIFO, write-preferring
[`RwLock`](https://docs.rs/tokio/1.53.1/tokio/sync/struct.RwLock.html).
The lockfile is a mapped source. The model never cancels an acquisition and
then preserves its old queue place. One selected reader and one writer make
exclusive ownership equivalent to the selected read/write conflict: concurrent
reader/reader behavior and other lock users are outside the model.

`Fair` lists weak fairness for each independently scheduled action. It never
asserts unconditional fair lock release, an eventually-free lock, whole-worker
execution or successful completion. Caller return must first have the
backend reply state; backend reply must first have application. A heartbeat
cannot release publication write before its own return. The lock-cycle
mutation adds the missing reverse wait, and fairness cannot rescue it.

| Boundary | Classification and premise |
|---|---|
| `ProbeReturn`, `WalkReturn` | **PRM-fs:** each selected filesystem step eventually returns; blocking filesystem syscalls have no local finite bound |
| `Admission` | **PRM-admission:** the selected capable worker has suitable physical capacity; production admission is try-only, not a waiting reservation |
| `AuthorityReturn`, `QueueAuthorityReturn` | **PRM-authority:** selected authority checks return; local metrics are examined, remote client construction is an external boundary |
| `StoreApply`, `StoreReply`, domain/queue backend steps | **PRM-backend:** after finite modeled faults, quorum, transport, rate limiting, forwarding and store scheduling permit normal calls to apply and return |
| Caller timeout | **Code fact for selected Hiqlite caller:** TimedClient wraps the whole transaction future with a 3-second deadline; acquiring the publication lock can precede that window |
| FIFO grants | **Code fact plus fair runtime scheduling:** pinned Tokio lock discipline prevents later heartbeat intents from barging past an existing selected intent |
| `ScanFinish`, individual send/return/join/retire/drop actions | **PRM-runtime:** enabled independently scheduled task steps eventually run |
| `Tick`, `QueueExpiry`, `DomainExpiry` | **PRM-time:** eligible backoff/expiry barriers are eventually observed; no clock precision, skew bound or bounded scheduling delay is inferred |
| Peer backend return and release application | **PRM-peer:** this finite selected competitor's external work and backend return; release is downstream of those actions, never unconditional fairness on a held lock |

Finite faults are nondeterministic and need not happen. Each modeled failure
consumes `s.faults`; therefore infinitely many faults are excluded without
requiring a fault to occur. Normal heartbeats, retry/reacquire, peer cycles
and ticks are not budgeted and can repeat forever. Stable demand, valid
selected payload, a capable surviving worker, no configuration removal,
and enough retry budget belong to the positive environment premise.

## Caller timeout does not roll back a replicated mutation

The selected worker has one ordinary backend slot and one detached completion
slot. Dispatch, application, reply and caller observation are separate.
`CompleteTimeout` releases caller-held locks and moves the lower request into
the detached slot. `DetachedApply` may still atomically publish request result
and terminal waiter afterward. It is permitted, not fair: a timed-out lower
request may never commit. Cleanup never waits for that detached slot.
`DrainDetached` only retires a slot whose lower application has already been
observed. This distinction avoids deriving lower-write cancellation from
the caller's deadline.

The model narrows one source behavior: `Current` includes local `~revoked`,
so local revocation immediately prevents modeled backend completion. The
source backend sees durable queue/domain rows, not that local flag; a detached
completion may still commit after local revoke and before durable release.
Those earlier-result traces are omitted. The durable stale-attempt guard is
represented separately by clearing `detachedCurrent` on release or fresh claim
application. Neither this guard nor the local simplification proves full
source refinement.

The authoritative reread can observe non-pending and set `reconciled`, or
observe pending and fail the attempt before a later retry. Reconciliation is
about the worker learning the outcome and cleaning up; durable usefulness
already occurred at the commit. Removing reconciliation would not undo it.
The lost-ack witness requires the non-pending reread, not merely a commit.

This slot abstracts a selected completion timeout, not arbitrary detached
claim, renewal, release or settlement mutations. Normal domain/queue renewal
steps model eventually successful caller returns in the stable suffix.
Their selected deadline action revokes and signals loss but still retains
the writer and waits for renewal return. The model makes no theorem that
all lower-layer writes have drained when release returns.

## Finite state classes preserve selected waits, not all production traces

1. **One selected request and finite work.** One blocking filesystem return,
   one representative fenced item insert and one completion stand for a
   selected finite request. Additional pages/items/children must independently
   satisfy the same finite-return premise. The model does not establish their
   bounds or scan throughput. The source bounds a turn to eight rounds of
   up to 256 pending rows each; that is not a lifetime event bound.
2. **One full worker and one optional competitor.** The peer has separate
   claimed/admitted, domain-acquired/backend, release and backoff stages.
   It represents domain contention only; its queue mutex/publication pipeline,
   request success, authority faults and admission details are outside scope.
   Peer acquisition cannot displace a current selected holder.
3. **Current versus superseded authority.** `relation = "current"` denotes
   the currently owned domain generation, while revocation/retirement makes
   publication invalid. `detachedCurrent` becomes false on release or a fresh
   queue claim, so a late old write cannot adopt a new attempt's identity.
   This is a relational abstraction of the selected owner, not modulo fence
   arithmetic. Source binding can replace an older binding with a higher
   domain fence; it rejects a lower superseded fence or a different attempt
   at the same domain fence. `Current` abstracts the selected valid owner
   after those source guards; equality alone does not preserve all fence
   ordering. Ordering among several stale fences, old-boot reply payloads and
   stale claim-resolution traffic remain in the original safety pilot or
   outside these progress claims.
4. **Time barriers never wrap.** `eligible` is reset on each new backoff and
   moves from waiting to reached on `Tick`. It is shared across the selected
   backoff/expiry class and does not encode TTL ratios or independent wall
   clocks. The separate tick bit permits real infinite activity even when
   eligibility is already reached. No maximum event count makes a stuck
   attempt disappear from the graph.
5. **Small failure budgets are explicit restrictions.** Progress uses fault
   budget 1 and retry limit 2; contention uses zero faults. Work failure,
   queue loss, renewal deadline, completion timeout, failed release and
   non-applied settlement are alternative uses of that one fault. They do not
   claim composition of many simultaneous failures. Release retains the
   modeled limit of five release attempts, with a backoff barrier and TTL
   fallback, corresponding to the source limit. Every registered fault budget
   is at most one: the fifth-failure branch and `DomainExpiry` are unreachable,
   so their coverage is not claimed. The model contains that branch for local
   cleanup followed by independent durable TTL recovery; larger budgets remain
   unsupported without the retry-bound review above. Item-insert timeout and
   its attempt failure/retry control flow are abstracted by `WorkFault`; any
   ambiguous intermediate catalogue effect is not asserted to roll back. This
   abstraction does not enumerate all item-timeout traces.
6. **Successful suffix and cleanup paths are distinct.** Work success settles
   before domain release; failure/cancellation joins work, releases domain,
   then settles. Queue loss prevents a successful settlement and needs queue
   expiry. An unavailable domain yields, cleans up and retries instead of
   assuming domain acquisition fairness. A non-pending result is absorbing;
   repeated pending attempts, renewals and peer cycles remain cyclic.

The claim path abstracts the successful durable claim protocol after the
filesystem probe and physical admission. It does not re-prove ambiguous
claim resolution, boot recovery, slot accounting or backend SQL safety.
The backend `StoreApply` is one serialized authority step with guards,
not an implementation of Raft. The result is finite conditional progress,
not a proof that these abstractions refine every execution of the daemon.

## Controls distinguish missing premises from source mutations

The lock control adds a token-mutex wait to renewal while renewal owns
publication write. Bind/complete can own token and wait for publication read,
forming a real cycle in the mutated wait graph. The source has no reverse
edge. `Tick` is always enabled, so global TLC deadlock detection cannot fire.
The temporal `AttemptCleanup` failure is the progress check for this cycle.

The renewal-livelock control removes only fairness of `ScanFinish`: external
work has returned, but worker scheduling can starve while renewal/ticks keep
running. It demonstrates why runtime scheduling is a premise, not a claim
that heartbeat activity proves useful work. The external-stall control removes
only `WalkReturn` fairness; cooperative cancellation/cleanup can then wait
forever on blocking I/O. The settlement control permits a settlement that
never applies, then removes queue expiry: local cleanup can finish, but the
durable running row never requeues. All four are expected temporal failures,
with exactly one property configured so TLC's generic temporal error is
unambiguously attributable.

There is no graceful-daemon-shutdown theorem: these worker loops are not
wired to the daemon background shutdown token. Abrupt process loss, arbitrary
boot adoption, per-job starvation freedom, unbounded in-flight writes,
numeric response time, remote authority internals and Raft liveness require
separate evidence. No timer or watchdog is proposed as a runtime fix.

## Subtitle completion construction correspondence

The 2026-10-26 mapping refresh compares the source with `origin/main` at
`087e836d369eb2b8f94ec1d0454ea6c157612df9`. Selected library claim, heartbeat,
bind/complete and retirement paths remain unchanged. Fragment admission
remains try-only. New speech work takes two bounded CPU threads through the
shared heavy gate; read attestation takes one speculative CPU slot without
producer authority. Those competitors do not establish admission fairness
for the selected library worker and are outside this model. New caption
schema installation preserves the mapped store timeout boundary.

Only source correspondence and hashes are refreshed. Model/configuration
bytes and fairness clauses are unchanged. Final adversarial review,
current-source runtime checks and TLC qualification remain pending.

## Subtitle producer recovery correspondence

The independent final review on 2026-10-26 inspected the frozen completion
and retargeting delta atop `be9ee45ae`. Shared queue changes add producer-node
and preparation-incarnation checks only for `subtitle_translation_prepare`
and `subtitle_translation_range`; legacy payloads remain inspectable but
cannot acquire new authority. The selected library enqueue/claim, binding,
renewal, cancellation, publication and cleanup paths retain their behavior.

The [exact-shape migration recognizer](../../crates/plurx-core/src/store/subtitle_translation_schema.rs)
and [replacement trigger](../../crates/plurx-core/src/store/subtitle_translation_retarget.sql)
were also inspected because migration installs effects on the shared job
table. They recognize the complete legacy caption shape, replace its trigger
transactionally, preserve domain data and refuse unknown shapes. Queue
updates select caption kinds for one operation; they do not cancel the
selected library jobs. New mapping dependencies pin these two boundaries.
SQLite v117, Hiqlite extension installation and added store-contract module
wiring leave the selected publication transaction and timeout unchanged.

This source review supersedes the earlier construction-only correspondence
status for these changes. It does not extend the finite model to caption
completion, producer recovery, remote-worker liveness or admission fairness.
Model/configuration bytes, fairness premises and historical receipts remain
unchanged. Offline mapping validation checks the reviewed inputs and anchors;
it is not Rust test execution, TLC qualification or a refinement proof.

## Moving-playback continuity correspondence

The independent bounded review on 2026-10-26 inspected the frozen delta atop
`352137f764d8cf87d530352222d5948a77275d4f`. Hiqlite changes only the
recognized caption extension chain; SQLite adds caption migration118. The
selected library transaction, publication and timeout paths are unchanged.

The exact original0/retarget1/continuity2 recognizer refuses unknown shapes.
The new [continuity triggers](../../crates/plurx-core/src/store/subtitle_translation_continuity.sql)
capture and retire grants only for `subtitle_translation_range`; demand
revocation deletes only matching captured caption grant rows. Comparing
the replacement mutation trigger with the previous retarget trigger confirms
that its shared-job effects retain their caption-kind/operation scope. New
dependency anchors pin these isolation boundaries. The expiry/UUID-reuse
review finding was fixed by atomic demand-trigger grant withdrawal.

The selected library enqueue, claim, renewal, binding, settlement and cleanup
semantics remain unchanged. This source correspondence does not extend the
finite model to caption continuity, playback deadlines or remote-worker
liveness. Model/configuration bytes, fairness premises and historical
receipts retain their identities. Offline mapping validation is distinct
from runtime tests, TLC qualification and a refinement proof.
