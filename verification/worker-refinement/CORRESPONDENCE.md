# Worker correspondence — observed events and the Rust coverage boundary

Companion to [README.md](README.md) and [CONTRACT.md](CONTRACT.md). The
machine-readable [correspondence.json](correspondence.json) binds 57 TLA
`Next` actions and 18 source boundaries to Rust anchors and selected tests.
It is a source-drift and gap receipt, not a refinement proof.

The action inventory is `DispatchRejected`, `DetachedReleaseApply`,
`RenewalLostAck`, `ProbeReturn`, `Admission`, `ClaimSend`, `DomainSend`,
`BindSend`, `ItemSend`, `CompleteSend`, `ReleaseSend`, `SettleSend`,
`StoreApply`, `StoreReply`, `CompleteTimeout`, `DetachedApply`,
`DrainDetached`, `CallerReturn`, `WorkerTokenIntent`, `WorkerTokenGrant`,
`AuthorityReturn`, `WorkerPubIntent`, `WorkerPubGrant`, `ScanStart`,
`WalkReturn`, `ScanFinish`, `Reconcile`, `Cancel`, `WorkFault`, `QueueLoss`,
`DomainIntent`, `DomainGrant`, `ReverseTokenGrant`, `DomainDispatch`,
`DomainApply`, `DomainReturn`, `DomainStop`, `RenewalDeadline`,
`QueueIntent`, `QueueGrant`, `QueueAuthorityReturn`, `QueueDispatch`,
`QueueApply`, `QueueReturn`, `QueueStop`, `ReleaseJoin`, `ReleaseFailure`,
`ReleaseRetry`, `DomainExpiry`, `LostSettlement`, `FinishJoin`,
`FinishRetire`, `DropAdmission`, `QueueExpiry`, `Wake`, `TimeObserved`, and
`Arrival`.

The 18 source-boundary IDs are `select.queue_loss`, `select.domain_loss`,
`select.admission_pressure`, `select.work_returned`,
`lock.acquire.queue_token_mutex`, `lock.release.queue_token_mutex`,
`lock.acquire.domain_fence`, `lock.release.domain_fence`,
`heartbeat.queue.deadline`, `heartbeat.domain.deadline`,
`heartbeat.queue.stop_join`, `heartbeat.domain.stop_join`, `release.attempt`,
`release.retry`, `release.ambiguous_fallback`, `retire.claim`,
`drop.admission`, and `sql.complete_library_work.trigger`.

## Selected source binding refresh — 2026-10-22

The 16 `background_jobs.rs` bindings were compared from their exact prior
source hash at `a86448723ec67e24ec8c1739e0df209bf4519e9c` to
`ec8101dc62975b842e50e02e47a36709e5f6400e`. The sole source delta adds the
Linux `publish_dv_title` method outside the selected library path. Library
candidate probing/claim dispatch, queue heartbeat authority and renewal,
queue-token locking, expiry notification, heartbeat join and claim retirement
retain their prior implementations. Only these 16 source hashes and the
mapping's containing correspondence hash were refreshed; anchors, trust,
schedules, gap text, model and configurations are unchanged.

The queue-heartbeat, lock-boundary and durable-expiry gaps below remain open.
This binding disposition supplies no new schedule execution, qualification,
state/action/fairness simulation or title-publication/liveness claim. In
particular, local queue-loss notification still does not establish durable
queue expiry, and the paired production schedule still proves sequential
completion after durable claim refusal rather than simultaneous domain
contention.

## Event projection — selected orders, not complete traces

The test-only task-local observer in
[`library_work.rs`](../../crates/plurxd/src/library_work.rs) reports these
events from the real `work_library_queue` path:

`Claimed`, `DomainUnavailable`, `DomainAcquired`, `Bound`, `Processing`,
`BeforeScan`, `QueueLoss`, `DomainLoss`, `Pressure`, `WorkJoined`,
`BeforeCompletion`, `CompletionReconciled`, `ReleaseStarted`,
`ReleaseFinished`, `Finished`, and `AdmissionDropped`.

The tests project selected observations onto the worker lifecycle relation in
the model: a worker reaches a loss or pressure arm while request processing is
paused at `BeforeScan`; the worker cannot begin release before that processing
future resumes and returns; release finishes before retirement and admission
drop. W3, W4, and W5 assert that partial order directly. W1 observes W1
holding a source-I/O reservation while a second node's distinct job remains
queued and unclaimed; after W1 releases capacity, W2 retries and both complete.
It does not observe a domain-unavailable yield. W6 observes completion reconciliation
after a committed completion reply is dropped, then reads the completed row.

The event stream is not a complete serialization of all TLA variables. The
tests assert these selected source/model ordering constraints; they do not
translate every observed event into a TLA action and check full trace
membership. The mapping and any unchecked transition remain visible in
`correspondence.json`.

## Seven schedules — what each one establishes

| ID | Exact Rust test | Observed behavior |
|---|---|---|
| W1 | `library_worker_two_jobs_contend_at_the_durable_queue_and_both_complete` in `library_work.rs` | Two node-specific JobManagers share the real store. While W1 is paused, W2 cannot claim its distinct `metadata_refresh` job because the queue's `source_io` reservation is occupied. The test checks the request stays pending and the job stays queued with no claim, failed attempt, or changed eligibility time. W1 completes; W2 then retries immediately and both real pipelines complete serially. |
| W3 | `library_worker_domain_loss_joins_before_release_and_admission_drop` in `library_work.rs` | Domain-loss notification while processing is held at the pre-scan checkpoint; the processing future returns before release, retirement and admission drop. |
| W4 | `library_worker_queue_loss_joins_before_release_and_retirement` in `library_work.rs` | Local queue-loss notification while processing is held at the pre-scan checkpoint; the processing future returns before release and cleanup. The durable job is queued after retirement. |
| W5 | `library_worker_admission_pressure_joins_before_release_and_drop` in `library_work.rs` | Test-only pressure selection while processing is held at the pre-scan checkpoint; the processing future returns before release and admission drop. |
| W6/S4 | `library_worker_reconciles_lost_completion_acknowledgement` in `library_work.rs` | The real worker commits completion, loses its acknowledgement, rereads durable state and observes the public projection `succeeded`. |
| S1 | `formal_publication_library_captured_token_completes_while_durable_owners_current` in `formal_publication.rs` | A captured token is submitted directly to Store and succeeds while both durable owners remain live; the submitted `Failed` result is recorded as `failed`. |
| S2/S3 | `formal_publication_library_completion_after_release_and_rebind_is_rejected` in `formal_publication.rs` | S2 checks refusal after releasing the domain lease while the old queue token remains current. It then settles that attempt with the worker's five-second yield policy; the request stays pending and the queue reservation is released. A distinct metadata job claims at `now + 4`, before the old retry is eligible. S3 rejects the old completion after both queue yield and domain rebind; it does not isolate higher-domain-fence rejection. The new owner completes its request. |

W1 uses two different durable jobs and separate node-local managers, matching
the production dedupe identities and manager scope. W2's first attempt is
refused at durable queue admission while W1 holds the shared source-I/O
reservation; it never reaches domain acquisition, binding, or the unavailable-
domain yield path. Once W1 completes, W2 retries without a yield delay and
performs the second full worker path. The test does not claim simultaneous
claims or scans, nor does it isolate domain contention.
The test-only `JobManager::test_new_for_node` constructor in `state.rs` gives
each worker a distinct node identity; it is fixture setup, not a production
transition. The qualification source manifest binds this helper's bytes.

`BeforeScan` fires inside `process_library_requests` immediately before the
call to `run_scan`. In W3-W5 the gate prevents that call from starting until
after the cancellation arm has run. `WorkJoined` therefore proves that the
held processing future returned before cleanup; it does not prove that a
filesystem walker or spawned resource child had started and joined. The
blocking-walker/child-join behavior is source-traced only (`PRM-fs`) and is
not a tested cancellation schedule.

S1 is a backend-authority schedule, not an in-flight dispatch schedule. It
captures the queue token and domain lease, then calls Store while both durable
owners remain current. It does not locally revoke a fence or pause a production
facade write after dispatch and before revocation. That late-apply history
remains model-only and unchecked by Rust.

## Unchecked paths — where the schedule stops

W4 injects only the local queue-loss notification. Production retirement
settles the still-owned job to `Queued`; the test does not invalidate durable
queue authority, wait for queue TTL expiry, or claim that local revocation
rolls back a detached write.

S2/S3 does not isolate the higher-domain-fence predicate: S3 follows S2's
domain-only check, but then also yields the old queue attempt before claiming
the replacement. Thus S3 has both stale queue authority and a changed domain
owner. The yielded request stays pending for its retry; the test does not
retire it before checking the old completion.

S5 (renewal committed with its acknowledgement lost), renewal deadlines and
heartbeat ordering are unchecked. The heartbeat runs in a spawned task and
does not inherit the task-local test barrier. Ambiguous release acknowledgement
and exact-token retry, repeated release failure, domain expiry, detached
release application, injected work faults, and lost settlement are also
unchecked. The correspondence file marks these rows explicitly; storage-only
coverage is not worker coverage.

The model's single settlement transition does not distinguish retry from yield
or represent the production `failed_attempts` increment. Its cancel schedule
also abstracts over cancellation-versus-settlement timing: the worker tests
observe release before settlement on cancellation, while completion may have
been dispatched before cancellation. Those differences are recorded as model
abstractions, not Rust refinement claims.

## Commands — compile now, execute schedules only after review

The pinned daemon compile and Clippy checks passed for source commit
`9adc50ca320244e2368803db33edbcdd17183545`. The feature-gated core contract
compile and canonical formatting check also passed after the F1/F3 correspondence
update; no unit or model tests have been executed:

```bash
CARGO_TARGET_DIR=target-worker-refinement rustup run 1.97.1 cargo check -p plurxd --all-targets
CARGO_TARGET_DIR=target-worker-refinement rustup run 1.97.1 cargo clippy -p plurxd --all-targets -- -D warnings
CARGO_TARGET_DIR=target-worker-refinement rustup run 1.97.1 cargo check -p plurx-core --test store_contract --features hiqlite-contract-tests
rustup run 1.97.1 cargo fmt --all --check
```

After final review, the focused unit commands are:

```bash
rustup run 1.97.1 cargo test -p plurxd library_worker_ -- --nocapture
rustup run 1.97.1 cargo test --locked -p plurx-core --features hiqlite-contract-tests --test store_contract formal_publication_library_ -- --nocapture
```

The first filter selects the five `library_worker_*` tests. The second selects
the two new library publication schedules against the contract backends,
including the Hiqlite contract backend. Neither unit command nor any model
configuration has been run in this phase. The source tests and model configs
are conformance evidence for selected schedules and assumptions; they do not
establish mathematical refinement of Rust.

## Production lifecycle extraction

The driver now executes effects from the actual shared Rust controller; [its table](../worker-control/CORRESPONDENCE.md) records the local proof boundary and related existing actions. The current source mappings include that controller body. The older model/schedule receipts remain attributable to their actual candidates; a refreshed source hash is not a new execution of a historical schedule. New driver ordering qualification is recorded by the current [admission ledger](../../docs/cluster/RUST-ADMISSION-REFINEMENT-STATUS.md).
