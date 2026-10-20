# Durable-job contract — connect finite authority transitions to source

**Status:** implementation contract; development checking only; independent
review and final qualification pending · **Source:** `3808640c7` plus the
publication conformance additions · **Scope:** bound `library_scan` and
`metadata_refresh` workers calling `insert_item_fenced`.

Companion to [README.md](README.md), which gives the reproducible commands,
and the [pilot proposal](../../docs/cluster/FORMAL-VERIFICATION-PILOT-PROPOSAL.md),
which owns acceptance. The transition correspondence below is a **reviewed
mapping candidate**, not an automatic refinement proof. Source hashes in
`mapping.json` detect drift in the reviewed dependency set; they cannot prove
that set complete or that the abstractions are correct.

## Publication checks two independent owners in one transaction

The queue tuple is job/node/boot/claim/fence/revision/expiry. The domain tuple
is resource/owner/fence/revision/expiry. Binding stores the stable queue
identity (including fence, boot and claim), plus the domain fence. Queue and
domain renewal revisions deliberately do not appear in the stored binding.
A worker can retain stale local tuples; only the authoritative Store guards
decide whether a transaction succeeds.

Both backend publication guards permit domain-only operation if no binding
matches the current domain fence. This is real behavior, not a universal
queue-bound invariant. The selected worker must bind successfully before
starting work. An obsolete binding survives queue expiry, compaction and
ordinary domain release; only deleting the domain row deletes its binding.
A newly acquired higher domain fence may temporarily have no matching binding.

| Transition / boundary | Source and guard | Effect / observable result | Omitted state or premise |
|---|---|---|---|
| Enqueue / demand | `store/background_jobs.rs::enqueue_job`, `ENQUEUE_SQL`, `background_jobs_retention.sql::background_job_enqueue_command` | Accepted demand creates or joins a queued job; request identity differs from job identity | Library id, kind and payload-v1 validation are fixed valid; waiter priorities and payload data are abstracted |
| Claim read, then CAS | `background_jobs.rs::claim_job`, `CLAIM_SQL` | First read can return the same live claim; otherwise exact observed revision CAS claims eligible queued/expired work, increments fence/revision, installs attempt and both required reservations atomically | Read and CAS are separate transitions; capability/kind predicates fixed to selected kinds; no Raft implementation |
| Claim eligibility | `CLAIM_SQL` and resource triggers in `background_jobs_resources.sql` | Valid demand, not-before reached, retry budget, owner not removed, dispatch window, fresh identity, reservation slots available; live reservations of other jobs compete | Finite identities never recycle; counter exhaustion is an exploration boundary, not a production terminal state |
| Replay / resolution | `background_jobs.rs::claim_job` and `resolve_claim` | Same identity returns existing current token only with matching node/boot, running state, live expiry and resource coverage; absent history gives expired/pruned, never a new attempt | Resolve without fence additionally checks dispatch window; per-operation caller `now_ms` is retained |
| Queue renewal | `RENEW_SQL`, `background_jobs_schema.sql::background_job_renew_command` | Exact predecessor, running state, live lease, no removed owner, complete reservations; increments revision and updates reservation expiry | Cancellation changes revision as well as state; replay is not a new renewal |
| Cancellation / settlement | `cancel_job`, `SETTLE_SQL`, cancellation/settlement triggers | Queued→cancelled, running→cancelling; exact live settlement clears owner and reservations; yield preserves demand and installs not-before; failure consumes retry budget | S5 applies to cancellation of existing interest; a separately audited new request is outside that monotone fragment |
| Domain acquire / renew / release | `store/sqlite/coordination.rs`; `store/hiqlite_coordination.rs`; `cluster/coordination.rs::Lease` | Acquire expired row with higher fence/revision; exact renew; release clamps expiry and advances revision without deleting row | SQLite acquire/renew do not have Hiqlite's explicit removed-owner filter; publication still checks its bound job |
| Bind | `background_jobs_domain.rs::BIND_SQL` | Selected kind and scan resource; exact live queue/domain tuples; reservations; owner not removed; stable tuple installed | Same domain fence accepts only identical stable tuple; different attempt needs a higher domain fence |
| Bind refusal | `plurxd/library_work.rs` around `bind_library` | Release domain lease, yield job until `now+5000`; work never starts | Poll/backoff runtime scheduling is a recovery fairness premise |
| Facade precheck | `store/publication.rs::PublicationStore::fenced_call` | Under write lock, reject revoked/missing/expired local lease; sample precheck timestamp; compute successor; call backend | Ordinary metadata time is not an authority clock |
| Successor | `cluster/coordination.rs::Lease::publication_successor` | Independent time sample; revision+1; expiry=max(sample+90000,pred.expiry+1); checked counter/minimum overflow | Finite counters never wrap; production i64 saturation/overflow remains ordinary test evidence |
| SQLite insertion | `store/sqlite/mod.rs::with_fenced_conn`; `sqlite/publication.rs::insert_item_fenced` | Sample inside transaction; validate successor; exact predecessor UPDATE plus binding guard; insert; commit, or rollback both changes | SQLite connection transaction serialization assumed |
| Hiqlite insertion | `hiqlite_publication.rs::atomic_renewal_statement`, `atomic_publication`, `insert_item_fenced`, `bind_atomic_authority` | Sample before await for each submission; statement zero renews predecessor with binding guard; insert rechecks the successor tuple returned by statement zero; transaction commits or rolls back | Serializable authoritative transaction/rollback is assumed, with real backend examples below |
| Retry | `hiqlite_publication.rs::insert_item_fenced` loop `0..4` | Same predecessor and successor, fresh candidate and backend sample; only allocated mode plus identity-error branch retries; up to four submissions | Identity candidate and error text are opaque; no retry on arbitrary lost reply/transport failure |
| Reply / facade state | `PublicationStore::fenced_call` | Backend error clears local lease; success records successor; local revocation after commit can return FenceRejected while insertion remains | Commit and acknowledgement are separate transitions; S7 is not a claim that every facade error means no item |
| Crash / reboot / removal | `plurxd/background_jobs.rs::JobFence`; `plurxd/job_lease.rs`; `membership.rs` owner removal | Crash discards locals and can leave requests in flight; reboot gets a distinct boot identity; removal marker rejects bound authority and renames/expires owned domain rows | Monotonic local deadlines may revoke authority independently of wall-clock expiry; transport and consensus internals omitted |

The shared queue operations are carried by the
[replicated `QueueSql` adapter](../../crates/plurx-core/src/store/hiqlite_background_jobs.rs#L206)
(`impl QueueSql for HiqliteAuthStore`) and the
[SQLite `QueueSql` adapter](../../crates/plurx-core/src/store/sqlite/background_jobs.rs#L10)
(`impl QueueSql for SqliteStore`). `queue_sql` forwards encoded caller
`now_ms` unchanged: Hiqlite uses `execute_returning_map` for mutations and
`query_consistent_map` for authoritative reads; SQLite executes the prepared
statement through `with_conn`. Claim reconciliation read and later claim CAS
are separate calls, not one transaction. `queue_transaction` groups its
provided statements in a Hiqlite transaction or SQLite connection transaction.
The shared `claim_job` / `resolve_claim` / renewal / settlement methods and
installed SQL triggers supply their guards; the bridges do not replace those
caller timestamps with backend publication time. These adapters are reviewed
mapping dependencies, not extra modeled consensus algorithms.

Paths in this table are relative to `crates/plurx-core/src/` except explicitly
prefixed `plurxd/`, which means `crates/plurxd/src/`. The dependency inventory
must include shared queue validation, schema/resource triggers, binding schema,
worker/facade, both backend bridges and coordination helpers; naming only the
publication files would miss guards they depend on.

## Boundary comparisons preserve equality and sample location

| Predicate | Exact source comparison | Consequence at equality | Source anchor |
|---|---|---|---|
| Current queue/domain authority | stored expiry `> sample` | Rejected | [queue renewal:630](../../crates/plurx-core/src/store/background_jobs.rs#L630); [Hiqlite domain/bound checks:49](../../crates/plurx-core/src/store/hiqlite_publication.rs#L49) |
| Expired queued takeover | stored queue expiry `<= caller now_ms` | Eligible subject to all other guards | [claim:532](../../crates/plurx-core/src/store/background_jobs.rs#L532) |
| Reservation coverage | held expiry `>= job lease expiry` with same fence/key | Sufficient | [queue renewal:636](../../crates/plurx-core/src/store/background_jobs.rs#L636); [publication:61](../../crates/plurx-core/src/store/hiqlite_publication.rs#L61) |
| Reservation occupancy | held expiry `> caller now_ms` | No longer occupies a slot | [claim:552](../../crates/plurx-core/src/store/background_jobs.rs#L552) |
| Not-before | `not_before_ms <= caller now_ms` | Eligible | [claim:535](../../crates/plurx-core/src/store/background_jobs.rs#L535) |
| Dispatch/resolution window | `now < dispatched` or `now-dispatched >= 120000` | Expired/pruned at upper boundary | [resolve:3465](../../crates/plurx-core/src/store/background_jobs.rs#L3465) |
| Domain release | `min(stored expiry, caller now_ms)` | Does not extend lease | [SQLite:125](../../crates/plurx-core/src/store/sqlite/coordination.rs#L125); [Hiqlite:162](../../crates/plurx-core/src/store/hiqlite_coordination.rs#L162) |
| Successor | same resource/owner/fence, revision+1, successor expiry `> predecessor expiry` | Equal successor expiry invalid | [successor construction:52](../../crates/plurx-core/src/cluster/coordination.rs#L52); [Hiqlite validation:34](../../crates/plurx-core/src/store/hiqlite_publication.rs#L34) |
| SQLite replacement | additionally successor expiry `> backend sample` | Equal successor expiry invalid | [inside-transaction validation:1985](../../crates/plurx-core/src/store/sqlite/mod.rs#L1985) |
| Hiqlite acceptance | predecessor expiry `> backend sample` and successor expiry `> predecessor expiry` | Implies successor expiry `> sample`; no accepted-publication strength difference | [replacement validation:38](../../crates/plurx-core/src/store/hiqlite_publication.rs#L38); [predecessor UPDATE:49](../../crates/plurx-core/src/store/hiqlite_publication.rs#L49) |

Facade precheck, successor-computation and each backend submission retain
separate timestamps. SQLite's backend sample is at the transaction action;
Hiqlite's sampled request may wait before its transaction action. Clock
anomalies and delayed samples are allowed in safety. The accepted authority
claim is relative to that operation's sample; physical commit-time expiry
is not proved. Equality, before and after classes must remain visible in the
finite model; expired Booleans alone cannot express the reservation or
resolution-window boundaries.

## Retention uses installed statements and preserves binding evidence

The installed maintenance trigger is the later replacement in
`background_jobs_receipt_pressure.sql`, included after
`background_jobs_retention.sql` and installed by both migrations. These are
separate transitions; a declared Rust `MAX_RETAINED_JOBS` is not their source.

| Action | Exact production eligibility and effect | Finite abstraction obligation |
|---|---|---|
| `PruneAttempts` | Finished AND resolve_until<=now AND (at least16 newer finished attempts for same job OR global attempts>=39872 AND any newer attempt); oldest-first LIMIT128 | Reduce newer-finished threshold16→1 and pressure39872→2; compute both branches and finished/window/newer predicates from modeled rows; never a fixed-size FIFO |
| `CompactInternalReceipts` | Terminal, retain_identity=0, receipt expired; pressure alternative count>=15360, internal scope, no active job; LIMIT128 | Receipt/demand eligibility classes, not all waiter fields; compaction cannot erase active demand |
| `RetireTerminalJob` | Terminal AND (updated<=now-604800000 OR global jobs>=9000), no pending/awaiting-hydration waiter, no legacy awaiting-import fragment pin; LIMIT128 | Explicit eligibility classes before allowing deletion |
| `EnqueueRetirement` | On accepted enqueue, count>=10000; terminal, no waiter/legacy pin, excluding admitted job id; LIMIT128 | Mapped source operation outside finite admission Next; no claim that this branch is explored |
| Detail deletion | `background_jobs_schema.sql::background_job_retire_details` removes attempts/reservations before job deletion | Domain binding remains; absent job rejects matching bound publication |
| Domain-row deletion | `background_jobs_domain.sql::background_job_domain_lease_removed` | Deletes binding only with domain lease row; ordinary release does not |

Ordering, pagination and payload-specific legacy state are outside the pilot;
their eligibility is represented explicitly rather than claiming all terminal
jobs may disappear. Stable recovery preserves valid demand and disables new
compaction/removal/cancellation interference.

## Caller scope is narrower than the facade

| Production route | Classification |
|---|---|
| `plurxd/library_work.rs`: acquire, publisher, bind, `process_library_requests` | Selected bound route; bind refusal releases/yields before work |
| Worker scan/enrichment and targeted scan, `plurxd/state.rs` passed publisher | Bound only when reached from that worker |
| Direct artwork repair/retry, `state.rs` `provider:artwork` | Domain-only, outside S2's bound-route claim |
| Genre backfill, `state.rs` `provider:genres` | Domain-only |
| Transcode `manager/produce.rs` with publication_fence | Domain-only example, not a bindable library job kind |
| `scan/mod.rs`, `metadata/mod.rs`, `metadata/{book,local,genres}.rs` convenience wrappers | Explicitly unfenced; outside the selected model |

A control that ignores binding must be able to accept an insertion after the
bound queue authority is invalidated. A control that bypasses bind-before-work
must expose the domain-only interval; that is a caller-premise failure, not a
claim that the backend universally requires a binding.

## Executable examples support correspondence; they do not prove refinement

The integrated `tests/store_contract/formal_publication.rs` contains these
named examples. Their existence is not a claim they have run; qualification
records live in the pilot status/evidence receipts after independent review.

| Example name (prefix `formal_publication_`) | Boundary exercised |
|---|---|
| `live_bound_insertion_commits_and_cancellation_orders_around_commit` | Actual insertion before/after cancellation; retained committed row |
| `binding_survives_independent_renewals_and_rejects_exact_stale_tokens` | Stable binding versus exact renewable tuples |
| `queue_expiry_with_live_domain_rejects_at_backend_sample` | Both-owner requirement with unequal expiries |
| `reservations_owner_removal_and_binding_lifetime_follow_code_semantics` | Resource/removal guards and retained binding |
| `resolved_claim_keeps_attempt_identity_through_insertion` | Replay/resolution identity through selected mutation |
| `hiqlite_statement_zero_missing_output_is_fence_rejected_and_rolled_back` | Actual pinned-vendor missing-output error and transaction rollback |
| `hiqlite_lost_acknowledgement_retains_commit_and_clears_facade` | Real commit with ambiguous acknowledgement; retained item and cleared facade |
| `hiqlite_allocator_conflict_retries_same_authority_fresh_candidate` | Actual allocator-conflict retry branch; four-submission authority contract |

Existing dual-owner regressions use `put_setting_fenced`; they remain useful
but cannot stand in for insertion conformance. The Hiqlite classifier's
substring `StmtIndex(0) does not have observable row output` is an explicit
**compatibility assumption**; the actual pinned vendor text also ends
`, or index out of bounds`. The allocated retry classifier matches
`item_identity_reuse` or `UNIQUE constraint failed: items.id`; the installed
schema's identity guard makes the latter branch unreachable in the integrated
allocator scenario, so the example exercises real identity reuse instead.
No artificial transport error is relabeled a retryable identity conflict.

## Trust and omissions remain explicit

One serialized durable Store, atomic rollback, correct statement-zero output
binding, finite model-checker/fingerprint behavior and the reviewed mapping
are assumptions. The pilot proves no Raft, quorum implementation, arbitrary
cluster size, physical exactly-once work, universal catalogue correctness,
resource deadlock freedom or production refinement. Existing tests and release
gates remain required. Recovery fairness and its narrower suffix are specified
in README and the model; finite safety exhaustion is never liveness evidence.

## Operator index — every transition has a source or abstraction class

| Model operators | Source correspondence / class |
|---|---|
| `InitFresh`, `InitRecovery`, `Next`, `ModelView`, `Symm` | Constructed reachable-shape seeds and explicit finite exploration machinery; not production operations |
| `Cancel`, `Remove`, `LoseResource`, `Takeover`, `Outage`, `Restart` | Queue cancellation; membership removal; reservation loss; expired `CLAIM_SQL` plus triggers; unreachable authority; crash/reboot transport abstraction |
| `SendRenew`, `ApplyRenew`, `DeliverRenew` | Caller queue heartbeat; atomic `RENEW_SQL`/renew trigger; worker adoption of returned exact token |
| `DomainChange`, `AcquireDomain` | Exact domain renewal/release and expired-lease acquisition in both coordination backends |
| `BindAttempt`, `RefusalRelease`, `RefusalSettle` | Binding transaction followed by distinct domain release and exact queue yield; caller not-before is now+5000 (model2) |
| `BypassBind` | Deliberate control violating the library worker's bind-before-work premise |
| `FacadePrecheck`, `ComputeSuccessor`, `Submit` | Facade precheck and independent successor sample; fresh Hiqlite sample per submission; SQLite sample is replaced by the transaction's own sample |
| `ApplyPublication`, `DeliverPublication` | Atomic domain renewal+insert or rollback, then separately delivered backend reply/facade outcome |
| `SendClaim`, `ReadClaim`, `ApplyClaim`, `DeliverClaim` | Claim dispatch with observed revision; initial read; CAS plus attempt/reservation triggers; optionally lost acknowledgement |
| `ReplayClaim`, `ResolveClaim` | Same dispatch/claim identity, local window checks, current stable tuple and retained-attempt lookup; current token revision can advance independently |
| `PrepareRetention` | Abstract maintenance-input classes and terminal settlement seed; flags explicitly summarize age, global pressure, demand, retain-identity, user scope and legacy pin |
| `PruneAttempts`, `CompactInternalReceipts`, `RetireTerminalJob` | Installed receipt-pressure maintenance statements and before-delete detail trigger |
| `RecoveryTick`, `RecoveryPoll`, `RecoveryRetry`, `RecoverySend`, `RecoveryApply`, `RecoveryAck`, `DeliverOldReply` | Stable suffix of worker polling/backoff, authority dispatch/application/reply and boot-mismatched delayed reply; concrete fairness premises, not a scheduler theorem |

The selected model fixes installed allocator mode for the identity-conflict
branch. It does not model every input-validation failure, UUID syntax,
payload-specific queue adapter, retry-deadline class, priority, waiter table,
full maintenance pagination, real i64 boundary or duration ratio. Selected
seeds assume sufficient remaining attempt budget. Both backends accept under
the same sampled-time implications, while SQLite validates its successor
against its inside-transaction sample. Only Hiqlite retries the selected
identity-conflict submissions; generic lost replies never enter that branch.

`Restart` intentionally over-approximates pending old-boot continuations:
local held authority is cleared but an old request stage can still complete.
Adoption checks boot identity, and the Store rechecks durable authority. This
is not a statement that a reboot preserves runtime Rust futures. A delayed
backend request may commit after its initiating process has gone away; an
old facade result is not new-boot authority.

The integer queue duration1 cannot produce a forward expiry extension under
its strict live-renewal guard: sample+1 is at most the old expiry. Queue
revision/binding continuity and backward-clock updates are modeled; ordinary
forward expiry extension remains executable conformance evidence rather than
a claim of this finite exploration.

Every authority operation uses the same shared safety relation from all seeds.
Only retention preparation is selected by its maintenance seed. Publication
records its entire relevant pre-state for S2/S3; later cancellation never
retroactively invalidates a committed event. Exact event horizons and the
proof obligation for the state view are in README. These limits
are part of every result, including a completed baseline.

## Measured coverage split preserves the shared action set

The required safety checks now use two jobs/eight events (`baseline.cfg`)
and one job/eleven events from claim seeds (`lifecycle.cfg`), both with two
workers, two boot identities per worker and three claim identities. The
shared relation and all failure actions are identical. The former covers
seeded bound publication under contention; exhaustive fresh claim through
commit/delivery composition is checked only in the latter single-job scope.
The abandoned two-job ten-event run is incomplete cost evidence, kept outside
the required registry. README gives its observed counts and the precise
eleven-action path. A predeclared single-job ten-event fallback would cover
commit only and must be labeled explicitly if used; it is not yet selected.
