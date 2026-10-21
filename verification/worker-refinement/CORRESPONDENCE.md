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
holding the shared domain while W2
hits `DomainUnavailable`, then confirms both distinct requests complete after
the production yield and retry delay. W6 observes completion reconciliation
after a committed completion reply is dropped, then reads the completed row.

The event stream is not a complete serialization of all TLA variables. The
tests assert these selected source/model ordering constraints; they do not
translate every observed event into a TLA action and check full trace
membership. The mapping and any unchecked transition remain visible in
`correspondence.json`.

## Seven schedules — what each one establishes

| ID | Exact Rust test | Observed behavior |
|---|---|---|
| W1/W2 | `library_worker_two_jobs_share_domain_and_both_complete` in `library_work.rs` | Distinct `library_scan` and `metadata_refresh` requests share one library domain. W2 yields while W1 holds it; after the production delay and retry, both requests complete. |
| W3 | `library_worker_domain_loss_joins_before_release_and_admission_drop` in `library_work.rs` | Domain-loss notification while processing is held at the pre-scan checkpoint; the processing future returns before release, retirement and admission drop. |
| W4 | `library_worker_queue_loss_joins_before_release_and_retirement` in `library_work.rs` | Local queue-loss notification while processing is held at the pre-scan checkpoint; the processing future returns before release and cleanup. The durable job is queued after retirement. |
| W5 | `library_worker_admission_pressure_joins_before_release_and_drop` in `library_work.rs` | Test-only pressure selection while processing is held at the pre-scan checkpoint; the processing future returns before release and admission drop. |
| W6/S4 | `library_worker_reconciles_lost_completion_acknowledgement` in `library_work.rs` | The real worker commits completion, loses its acknowledgement, rereads durable state and observes `completed`. |
| S1 | `formal_publication_library_late_completion_after_local_revoke_before_release_commits` in `formal_publication.rs` | A captured token is submitted directly to Store after local revocation and succeeds while durable ownership remains live. |
| S2/S3 | `formal_publication_library_completion_after_release_and_rebind_is_rejected` in `formal_publication.rs` | Store rejects the old captured token after release and after a distinct job binds a higher domain fence; the new owner can complete its own request. |

W1 uses two different durable jobs, matching the production dedupe identities.
Only one worker holds the domain at a time; W2's first attempt takes the
unavailable-domain yield path, then its later retry performs the second full
worker path. The test does not claim simultaneous scans.

`BeforeScan` fires inside `process_library_requests` immediately before the
call to `run_scan`. In W3-W5 the gate prevents that call from starting until
after the cancellation arm has run. `WorkJoined` therefore proves that the
held processing future returned before cleanup; it does not prove that a
filesystem walker or spawned resource child had started and joined. The
blocking-walker/child-join behavior is source-traced only (`PRM-fs`) and is
not a tested cancellation schedule.

S1 is a backend-authority schedule, not an in-flight dispatch schedule. It
captures the queue token and domain lease, locally revokes a
`PublicationFence`, then calls Store. It does not pause a production facade
write after dispatch and before revocation. That history remains unchecked.

## Unchecked paths — where the schedule stops

W4 injects only the local queue-loss notification. Production retirement
settles the still-owned job to `Queued`; the test does not invalidate durable
queue authority, wait for queue TTL expiry, or claim that local revocation
rolls back a detached write.

S5 (renewal committed with its acknowledgement lost), renewal deadlines and
heartbeat ordering are unchecked. The heartbeat runs in a spawned task and
does not inherit the task-local test barrier. Ambiguous release acknowledgement
and exact-token retry, the fifth release failure, and domain expiry are also
unchecked. The correspondence file marks these rows explicitly; storage-only
coverage is not worker coverage.

## Commands — compile now, execute schedules only after review

These pinned compile and static checks passed for source commit
`9adc50ca320244e2368803db33edbcdd17183545`:

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
