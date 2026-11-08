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

## Pipeline v2 targeted job (T2) — scoped correspondence review on 2026-11-08

Claude (`claude-opus-5-5`, the pipeline-v2 builder) reviewed PR #1405
(`claude/pipeline-v2-m5-t2-targeted-job`, T2 of `effort/pipeline-v2`) against
its parent T1 `dd22ccd21` and main `1d11f0cdf`. This is an author review
awaiting an independent check; it is not execution evidence or a refinement
theorem. Contract v2 §5.1 adds a targeted library request as its own job:
kind `library_scan`, generation `library-targeted-v1`, dedupe
`library:targeted:{id}`, priority 3. It claims with a probe-only admission
and is serialised against a running full scan of its library.

| T2 behaviour (source) | Model action or property | Still holds? | Model change |
|---|---|---|---|
| Targeted enqueue: new generation, priority 3, own dedupe key, optional sidecar in the command body ([`background_jobs_library.rs`](../../crates/plurx-core/src/store/background_jobs_library.rs)) | Durable jobs `SendClaim`…`DeliverClaim`, S1–S8; `ENQUEUE_SQL` and `CLAIM_SQL` unchanged. The model does not represent kind, generation or priority | Yes: claim CAS, attempt and reservation triggers, binding, renewal and settlement are the existing ones for `library_scan` | None |
| `claim_library` accepts the targeted generation beside `library-work-v1`, through main's dispatch-turn readiness ([`background_jobs.rs`](../../crates/plurxd/src/background_jobs.rs)) | Operation progress `ProbeReturn`, `Admission`, `ClaimSend`; worker refinement `Admission`/`ClaimSend` (eligibility over-approximated) | Yes: same `claim_with_resolution`, `ActiveBackgroundJob` and shared library driver; an unknown generation is still skipped | None |
| Probe-only admission: no heavy permit and no CPU/GPU bundle; `fragment_worker_idle` is always true for it, so heavy-lane occupancy never raises pressure ([`construct.rs`](../../crates/plurxd/src/transcode/manager/construct.rs)) | Operation progress `Admission`, `DropAdmission`, `AdmissionUntilJoined`, `AttemptCleanup`; durable jobs `PauseSource`/`ResumeSource`; worker refinement boundary `select.admission_pressure` | Yes: the admission is still one owned object, held in the environment's `Arc` through `JoinWork` and dropped after release. Removing heavy-lane pressure only removes optional pause and pressure schedules; a subset of the modeled schedules keeps S8 and the safety results. Foreground source pressure still reaches the Linux source driver. Admission availability was never modeled | None |
| A claim the Store refuses is followed by a read of the existing source-reader observation and an advisory `waiting_on` write (`full_scan:<library>` or `heavy_lane:<kind>`), only when the reason changes ([`background_jobs.rs`](../../crates/plurxd/src/background_jobs.rs) `targeted_claim_blocker`) | No modeled transition. Claim eligibility contention is outside all three models (operation progress already records the extra eligibility states as unmodeled; worker refinement over-approximates the production same-root claim refusal) | Yes: the refused job holds no claim, domain or binding; the probe-only admission is dropped at `continue`. The reads and the sidecar write change no queue, domain, binding or publication state, and no lock is held across them | None. The refusal is the existing `CLAIM_SQL` rule that keeps a source's second reader for live demand |
| Fence refusal of a targeted job: advisory `waiting_on = full_scan:<library>`, yield with not-before `min(driver, now+250 ms)`; acquired fence clears `waiting_on`; a non-targeted release wakes the local library loop ([`env.rs`](../../crates/plurxd/src/library_work/env.rs)) | Durable jobs `AcquireDomain`, `RefusalRelease`, `RefusalSettle` (`notBefore = sample + YieldMs`); operation progress `PeerAcquire`, `CallerReturn` (domain refused), `Wake`; worker refinement `DomainSend`/`DomainApply`, `Wake` | Yes: the refusal still releases nothing it does not own and yields with a positive not-before. `YieldMs` (2) is an abstract positive delay; the model keeps no duration ratio (its `LeaseMs` is 1). The concrete 5 s and 250 ms both map to it. The local wake only shortens polling, a fairness premise | None; the bind-refusal row now names both delays |
| Targeted scan with `expected_files`: forced probe of the named files and per-file placed reports written to the sidecar ([`library_work.rs`](../../crates/plurxd/src/library_work.rs), [`scan/mod.rs`](../../crates/plurx-core/src/scan/mod.rs) `scan_path_targeted`) | Durable jobs `BindAttempt` → `FacadePrecheck`/`Submit`/`ApplyPublication`; operation progress `ScanStart`…`ScanFinish`, `ItemSend`, `CompleteSend`; worker refinement `ScanStart`, `ItemSend` | Yes: it publishes through the same bound `PublicationStore` and `insert_item_fenced` after binding (bind-before-work unchanged). The report write is advisory sidecar state | None |
| Local completion signals for `wait_ms` and the targeted scan returning placed reports ([`state.rs`](../../crates/plurxd/src/state.rs)) | None; caller-side waiting outside the worker | Yes: node-local `Notify`; the durable request record stays the authority | None |
| At the start of a targeted pass, `process_library_requests` clears the requests' `waiting_on` ([`library_work.rs`](../../crates/plurxd/src/library_work.rs)) | None; inside `ScanStart`'s bound work, before any publication | Yes: a plain sidecar write through the Store, not the publisher; it holds no queue-token or publication lock across the await and changes no authority state | None |

**Isolation of the refusal.** While another worker runs the full scan of the
same library, the targeted job is never claimed. The refusing predicate is
`CLAIM_SQL`'s "a second source reader is reserved for live demand" clause.
Another non-live holder of a conflicting `source_io` reservation refuses a
background claim, and a running scan always holds its library's reader.
`library_scan` also requires `provider:tmdb` and `provider:anilist`, but each
has two reservation slots (`background_job_claim_effects`), so a single
running scan leaves one free and they do not refuse it. The regression
asserts that the refused claim coincides with the full scan's
`source_io` reservation (`source_io_holders`). The per-library mutation
fence remains the serialiser of record whenever the reader does not
conflict.

**Limits.** No model or configuration byte changes, and no TLC run is
claimed for these sources. Only whole-file pins are refreshed. Progress for
a targeted request behind a running scan rests on that scan finishing, the
same premise the models state for a domain peer; it is not proved.

## Scan stack and canonical bootstrap — scoped source audit on 2026-11-04

Codex agent `/root/recovery_design_audit` reviewed the frozen
[library environment](../../crates/plurxd/src/library_work/env.rs) correction
against `460d8e6d3ec494f2335ed4bbdfa6601f54bfa380`. All 13 prior environment
fingerprints match the base, and retained anchors remain unique. Refresh those
13 existing action/boundary source pins and the containing correspondence
hash; retain all 57 actions, 18 boundaries, trust classifications and seven
selected schedules. This is an authored audit, not independent review or an
execution receipt.

The extracted `probe_work` factory heap-pins `process_library_requests` before
the generic scopes receive it. Direct await preserves task-local probe input,
budget, cancellation and pause bindings. The existing outer work future still
owns its publisher and domain reference; admission stays retained until joined
work releases it. `CancelWork`, `JoinWork`, publisher drop, domain release,
queue retirement, environment field order and shared driver bodies are
unchanged. Heap allocation supplies no new authority transition or fairness.

The related [canonical version-4 bootstrap correction](../durable-jobs/CONTRACT.md#scan-stack-and-canonical-bootstrap--scoped-source-audit-on-2026-11-04)
preserves the existing exact DDL/marker validation and resource view. Startup
and schema installation remain outside this closed worker correspondence.
Neither native stack exhaustion nor heap allocation is modeled. The PR's
future-size check and ordinary-stack empty scan are separate runtime evidence;
they do not prove a full UI/media scan, whole-daemon progress, or source reboot
recovery. Model, configuration, bounds, properties, fairness and historical
receipts are unchanged; fresh static/TLC results remain separate evidence.

## Verified host-reboot recovery — scoped source audit on 2026-11-04

Codex agent `/root/recovery_design_audit` compared source `88fa37663` in
[PR #1378](http://forge.lan:3000/noirr/plurx/pulls/1378) with
`edf6d08faf549554b9892dee582406ff79af1c97`. The agent contributed recovery
implementation/tests; this is an authored source audit, not independent
review or execution. All 18 affected action/boundary source fingerprints
match that base before refresh, and every retained anchor remains unique.

The only selected-worker file delta is
[queue ownership](../../crates/plurxd/src/background_jobs.rs) passing its
exact Store to source-journal cleanup. Acquisition, claim/renewal, binding,
token/publication locks, queue heartbeat join and retirement retain their
implementations. The 57 actions, 18 boundaries, trust classifications and
seven selected schedule definitions remain. `FinishRetire`'s gap now explicitly
retains post-retirement physical settlement and journal cleanup outside the
modeled transition and its liveness result.

[Custody integration](../../crates/plurxd/src/source_custody.rs) and
[recovery](../../crates/plurxd/src/source_custody/recovery.rs) additionally
persist active-reader evidence, reconcile aborts against the exact Store,
and settle only after independently verified host reboot. They are external
source dependencies, not extra actions in this closed correspondence inventory.
The [durable contract](../durable-jobs/CONTRACT.md#verified-host-reboot-recovery--scoped-source-audit-on-2026-11-04)
and [operation wait map](../operation-progress/WAIT-MAP.md#verified-host-reboot-recovery--scoped-source-audit-on-2026-11-04)
record the broader source audit and explicit dependency pins. Changed-boot
proof, descendant termination, durable directory publication, fallback-Store
isolation, legacy preparation and successful physical recovery are not proved
by this model or its existing paired-worker schedule.

Startup/scheduler recovery can wait on filesystem and Store work before a
library worker is dispatched. An unchanged boot or missing evidence can keep
source custody unavailable indefinitely. The model's external-return and
fairness premises neither force a reboot nor prove those new paths terminate.
The new crash/replay tests are runtime regressions, not additions to the seven
mapped worker schedules or a refinement argument.

Refresh only the 18 whole-file source pins and containing correspondence hash,
plus the explicit `FinishRetire` gap. Model, configuration, bounds, properties,
fairness, checker and historical receipt identities remain unchanged. Fresh
static/TLC smoke and independent Astra review are separate evidence.

## Playback startup custody — scoped source review on 2026-11-03

Codex agent `/root/other_formal_correspondence` compared base
`bd213c81942175f591912e4bb0a2cc10f2a7a195` with source candidate
`594210a783986d4da5ec7184f0895433ffe98b90`. Every previous mapped source
fingerprint matches the base. The detailed source audit and external waits
are in the [operation-progress wait map](../operation-progress/WAIT-MAP.md#playback-startup-custody--scoped-source-review-on-2026-11-03).

[Queue ownership](../../crates/plurxd/src/background_jobs.rs) adds custody
registration/activation before heartbeat startup and joined-source settlement
after heartbeat join and token retirement. Settlement awaits durable local
journal persistence, replicated proof and exact custody deletion, retrying
without a local completion bound. Thus `FinishRetire` retains its existing
queue meaning; its source anchor also contains an additional unmodeled wait
before `DropAdmission`. A model cleanup result does not prove this wait ends.

[The environment](../../crates/plurxd/src/library_work/env.rs) retains the
physical admission `Arc` in the awaited work future. Work join still precedes
domain release, queue retirement and physical admission drop. Linux source
pressure now uses verified pause and durable source-slot reacquisition inside
that future; other-platform source pressure selects cancellation. These source
slots are distinct from modeled physical admission. Custody, pause/resume,
byte pacing, private input, process-group quiescence and settlement replay
have no state/action/fairness mapping or schedule coverage in this package.
Additional core claim eligibility and paused-reservation renewal states also
remain outside the model. Neither selected lock facts nor existing filesystem,
backend and runtime premises establish progress through these new mechanisms.

[The library entry file](../../crates/plurxd/src/library_work.rs) only adds
the probe-input module declaration; its selected bodies and test schedules
are unchanged. [The storage schedules](../../crates/plurx-core/tests/store_contract/formal_publication.rs)
only gate an `Arc` import by their existing feature. These source edits add
no schedule execution evidence. Retain every action/boundary, exact anchor,
trust classification, schedule and gap; refresh only their reviewed source
fingerprints and the containing correspondence fingerprint. Model/configuration
bytes and historical qualification identities remain unchanged. This authored
audit supplies no independent review, new execution, simulation proof or
custody/pause liveness guarantee.

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
