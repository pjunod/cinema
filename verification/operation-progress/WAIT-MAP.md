# Operation progress wait map — durable library work from claim to reply

**Status:** source-traced for model design · **Source base:** `caf93319ce833f3d8f20f3a7bcbadcbae9c1c42a` · **Written:** 2026-10-21.

This map supports [Operation progress](../../docs/cluster/OPERATION-PROGRESS-IMPLEMENTATION.md). It follows one `library_scan` or `metadata_refresh` job through the worker and names what must progress before a durable library request has a useful result. [Source anchors](source-anchors.json) record exact strings and whole-file hashes for load-bearing paths.

## Main metadata integration — scoped reconciliation on 2026-10-29

Codex agent `coverage_review` reconciled repair parent `6eaa9c72d` with
main `85910425c`. Both parents' startup and repair audits below are retained.
Incoming main changes since `6c8c98c79` affect no Rust/Cargo or served-asset
bytes; current mapped source pins and exact anchors therefore stay unchanged.
This reconciliation changes no model, configuration, validator, fairness
premise or historical receipt and supplies no new runtime or TLC evidence.

## Sharing startup integration — scoped source review on 2026-10-29

Codex agent `coverage_review` compared main `66a33d723` with incoming main
`10f45ae5f2619ab3e807b5576bf6ffb39ade6e86` and the repair worktree.
[Startup](../../crates/plurxd/src/main.rs) adds a read-only Sharing host
capability command and separate host-reconciliation and invitation-network
loops using the existing background shutdown token. The mapped
`background_work_loop` call, selected library scheduler/worker bodies and
token/publication wait and lock order are unchanged. Sharing host setup and
invitation-network lifecycle, including their child work and shutdown, remain
outside this selected-worker model. This review does not prove their resource
contention or liveness.

[The translation schema test](../../crates/plurx-core/src/store/subtitle_translation_schema.rs)
changes only its upgraded-version expectation from 119 to 120. Its production
recognizer and upgrade guards are unchanged. Two whole-file row pins follow
the reviewed bytes; retained exact anchors remain unique.

This agent authored the mapping refresh; parent and independent Codex diagnose_core_failures metadata review found no issues.
Model/configuration/toolchain bytes, bounds, fairness premises, properties,
scope, validators and historical source receipts remain unchanged. This
review ran no builds, tests or TLC and supplies no new runtime, refinement,
liveness or qualification evidence.

## Combined-source integration — scoped author audit on 2026-10-29

The repair is combined with main `66a33d723096ee931ce2e05e860f51f8f3671513`.
Retain its reviewed resource-admission v2 schema and mapping inputs. The
historical rewind now validates schema 119 with managed v1, or schema 120
with managed v2, before any mutation; crossed pairs are refused atomically.
The conformance parent retains the selected `formal_publication` module and
combines the independently reviewed strict caption/continuity teardown with
main's exact managed shape/data planner. Relative to this main, `state.rs`
adds only the test-module declaration; mapped production bodies are unchanged.

Only affected whole-file pins follow the combined source; anchors, models,
configurations, properties, bounds and historical source receipts retain their
identities. Independent Codex agent `coverage_review` reviewed the merged source pins, both parents' anchors and the combined ownership census, finding no blockers. That source review ran no builds, tests or TLC. This is a
source correspondence audit and supplies no new runtime or TLC qualification.

## Unit-coverage repair — scoped author audit on 2026-10-28

Codex agent `/root/diagnose_core_failures` compared immutable base
`24d1bad72bed328b6de9a568a3537f375f9c206f` with runtime/test source
`d03c8abdf17dea7147e112a32fec274834151fca`. Previous pins match the base.
This agent authored the core SQL repairs; this audit is source-author
correspondence, not independent review of that implementation. Independent Codex agent `coverage_review` reviewed the implementation diff and this metadata disposition on 2026-10-28, finding no blockers. That review ran no builds, tests or TLC checks and supplies no new qualification claim.

[Membership](../../crates/plurx-core/src/cluster/membership.rs) corrects
Hiqlite parameter binding in maintenance lease consumption, heartbeat
acknowledgment and status capability/active-session projection. Exact
node/claim/lease, maintenance and active-session predicates remain; values
now reach their intended predicates in first-appearance order. Tables 31–32
retain byte-identical `ClusterJobAuthority` and `may_run_cluster_jobs`
bodies: local maintenance and absent committed-voter authority still refuse
selected library work. Membership lifecycle, status and drain behavior are
outside this selected-worker model; this audit does not prove them.

[State](../../crates/plurxd/src/state.rs) adds only a `cfg(test)` backfill
module declaration. Tables 42–43 retain the scheduler/worker bodies, selected
claim/bind/renew/settle/publication flow and token/publication wait and lock
ordering. Four whole-file row pins follow this source; exact anchors remain
unique. Model/configuration/toolchain bytes, bounds, fairness premises,
properties, scope, validators and historical source-anchor snapshots are
unchanged. This audit supplies no new runtime or TLC execution, refinement,
liveness, physical acceptance or qualification; earlier receipts retain only
their recorded source identities. Fresh affected execution is separate.

## Sharing host startup — scoped refresh on 2026-10-28

Compared the pinned `main.rs` from `2975dec487d2` with `10f45ae5f`.
The additions declare sharing host/network modules, expose a host-capability
probe and start independent sharing loops using the existing cancellation
token. Table 44's selected `background_work_loop` spawn and its argument are
unchanged. These sharing loops are outside this selected library-worker model;
this review adds no fairness, shutdown or liveness claim for them.

Refresh only Table 44's whole-file hash. The exact anchor, model, configurations,
bounds and historical source anchors stay unchanged. This records source
correspondence; it does not claim a new formal execution or qualification.

## Metadata-only title analysis — scoped refresh on 2026-10-28

Codex compared PR #1301 source `7d2d4a9c7` with main `77e985827`.
The two title-only trait/delegation additions preserve Table 30's selected
library/metadata authority arm. Stale-FEL selection is a read; the analysis
publisher uses title source/scanner/generation checks and the existing title
trigger with `job_id` NULL, matching no queue/waiter settlement rows.

`state.rs` adds fresh-title timestamps, local-index mode and an analysis-result
offer. The selected scheduler/worker, claim/bind/renew/settle/publication paths
and token/publication lock ordering are unchanged. Title scanning, local index
waiting, FEL requalification and analysis publication remain outside this
selected-worker model and add no admission fairness or liveness guarantee.

Three whole-file pins are refreshed; their exact anchors remain unique.
Models, configurations, bounds, fairness premises, toolchain and historical
source anchors retain their identities. This is source correspondence only;
fresh affected smoke receipts remain separate from historical qualification.

## VOD index recovery — scoped refresh on 2026-10-27

`construct.rs` adds `admit_fragment_queued`, a FIFO wait on the shared heavy
gate followed by the same `admit_source_work_with_heavy` refusal, used only by
the node-local fragment index pass, plus `fragment_must_yield` and
`background_yield_reason` predicates for that pass. The gate has one permit
and other heavy workers only try-acquire it, so a waiter takes the next turn
ahead of all of them: the pass waits once per pass, holds the gate for one
file at a time, and otherwise try-acquires like the rest (a queued speech pass
goes first). Like the speech queue, it is a competitor on the heavy gate,
outside this model, and establishes no admission fairness for the selected
library worker. `admit_fragment`
(Table 41) is byte-identical and still try-only. `state.rs` changes the local
index pass, the analysis-request resolver (learners resolve targeted
requests) and copy-preparation priority; `membership.rs` retires a removed
node's analysis requests in `finalize_node_removal`. None of them touches the
selected library claim, renewal, binding, wait/lock order, settlement or
fenced publication, `schedule_loop`, `background_work_loop` or the mapped
`may_run_cluster_jobs` authority arms. Hashes refreshed; anchors, model,
configurations and worker correspondence unchanged.

## HTTPS and Linux-player integration — 2026-10-26

The merge of HTTPS `67b09d29c` with main `167af4616` retains both scoped
source reviews below. The new FEL trait/delegation methods, off-boot VA-API
probe and independent handoff-pruning loop do not alter selected library
claim, renewal, binding, wait/lock, settlement or fenced-publication paths.
The Store parent retains strict historical fixtures and inventories; its
`formal_publication` module is unchanged. Reviewed whole-file pins follow
the composed source; models, configurations, anchors and scope do not change.
FEL, handoff lifecycle, identity repair, historical migrations and public TLS
remain outside these guarantees. This is source correspondence, not new
execution, physical acceptance or qualification. Other historical explanatory
anchors in `source-anchors.json` retain their recorded historical identity.
Row 41 also follows the Linux-only guard on `extend_fragment_admission`;
the selected `admit_fragment` body and try-only admission order are unchanged.

## Completion identity-result correspondence — 2026-10-25

Source `5eacfd4c6b83a6f036627a0bfb80c60d0d59cba9` changes identity-repair result classification
inside the shared publication file. Only its existing guarded annotation
set-copy permits zero-to-many rows; full-vector and all other exact-one checks
remain. Identity repair is outside the selected library-worker progress model
and **is not proved here**. `atomic_publication` renewal, authority binding,
wait/lock order, selected publication SQL/anchors and worker cancellation and
settlement are unchanged. Two whole-file pins track this reviewed correspondence.
No model, configuration, source-anchor, validator or scope changes are made;
this adds no liveness, runtime, theorem, physical or qualification guarantee.
Affected smoke receipts remain separate source-bound evidence.

## HTTPS integration — scoped refresh on 2026-10-25

Codex reviewed source `f18c39e66954b71fcedcf8f463914f112498a518` against
`9bcfa9ca1f792d214122762981c84c85ba5b3e64`; the parent coordinator approved
the scoped disposition. Four drifted whole-file inputs cover five table rows.

| Binding | Source-backed disposition |
|---|---|
| Library authority (30) | Fragment enqueue guards now precede receipt reuse, but ordinary library inputs omit both fragment/analysis fields. The selected authority arm, claim/bind/publish/renew/cancel/settle paths and worker wait order are unchanged. General fragment receipt admission remains outside this operation. |
| Daemon loops (42–43) | `AppState` gains only the HTTPS control field and initialization. Both mapped scheduler/worker bodies and their wait/lock/settlement order are unchanged. |
| Main worker owner (44) | Public TLS preparation, serving and drain gain a separate owner and may request the existing daemon cancellation. The mapped background worker spawn and shutdown consumers are unchanged. Public listener startup, handshake/drain latency and whole-daemon shutdown remain outside the selected operation-progress guarantee. |
| Tokio dependency | Only daemon dependency edges are added; Tokio remains 1.53.1 and Hiqlite remains 0.14.0. |

Exact anchors, executable model/configurations, toolchain, historical
source-anchor snapshot and worker correspondence are unchanged. Whole-file
pins remain deliberate and fail closed on future drift. This source review
adds no liveness guarantee or qualification result; fresh affected smoke
receipts are separate evidence.

## Read-only readiness binding — scoped refresh on 2026-10-23

The A2923b2c to be610d647 delta adds the advisory `dv_title_readiness`
trait method and delegation to the core background-job store. Table30's
`LibraryScan` / `MetadataRefresh` authority arm is byte-identical. Only that
whole-file mapping hash is refreshed; exact anchors, executable source-anchor
records, model/configurations and worker correspondence are unchanged. The
readiness API's completion and latency acquire no liveness or qualification claim.

## FEL qualification review fixes — scoped refresh on 2026-10-26 (2)

`construct.rs` now lets a background job extend its own `FragmentAdmission`
with a second bundle (`extend_fragment_admission`), and
`fragment_worker_idle` compares the node's hardware and software use with
what the admission claims, extension included. The selected library worker
never extends its admission, so for it `claimed()` is `(0, threads)` and the
predicate is exactly the old one (`in_use() <= 0` is `in_use() == 0`);
`admit_fragment` (Table 41) is byte-identical. `main.rs` warms the VA-API
HEVC Main10 probe in a spawned task instead of awaiting it during
`probe_system`; the Table 44 spawn line is byte-identical and the worker
loops start as before. Hashes refreshed; anchors, model, configurations and
worker correspondence unchanged.

## FEL qualification additions — scoped refresh on 2026-10-26

The `claude/lp-fel-p81` delta against `origin/effort/linux-player` adds three
DV-title store methods to the core background-job store (Table 30's
`LibraryScan` / `MetadataRefresh` arm is byte-identical), two
`TranscodeManager` field initializers in `construct.rs` (the live
reconstruction slot and a per-title re-qualification throttle;
`admit_fragment` is byte-identical), and a startup measurement of VA-API HEVC
Main10 decode in `main.rs`'s `probe_system`, before the worker loops are
spawned (Table 44's spawn line is byte-identical). None of it is on the
selected library worker's path. Only whole-file hashes are refreshed; anchors,
model, configurations and worker correspondence are unchanged.

## Physical admission correspondence refresh — 2026-10-25

The reviewed subtitle-setup change adds `admit_fragment_queued` for model
installation and verification. It waits cancellably in FIFO order for the
shared heavy permit, then uses the factored `admit_source_work_with_heavy`
helper to reserve CPU capacity and attempt bundle admission. The original
`admit_source_work` path retains its idle check and try-only heavy and bundle
admission; the selected `library_scan` / `metadata_refresh` callers still use
that original try-only path. The factored helper now rechecks worker idleness
after heavy-permit acquisition: newly observed foreground demand returns
`None` and drops the permit before the library worker can claim a job. The
queued method is outside the selected library model. Foreground admission
checks remain in force, and cancellation is observed before proceeding. This
correspondence adds no model liveness or qualification claim.

## Selected binding refresh — 2026-10-22

The five drifted source files were compared with the revisions whose contents
match their previous binding hashes, then traced at
`8c7b7398abb5361aff5c5f6a2d6bf3a4589994b1`. Tables 01–07, 21–23, 30, 34–38
and 41 retain the selected `library_scan` / `metadata_refresh` abstraction:

| Binding | Source-backed disposition |
|---|---|
| Queue admission, ambiguity, token locks and retirement (01–07) | The new `publish_dv_title` method is outside the selected library path. Claim selection, queue renewal, bind/complete lock order and heartbeat join before retirement are unchanged. The older explanatory anchor snapshot also predates the test-only completion acknowledgement fault hook; the production completion await is unchanged. |
| Timed transaction and deadline (21–23) | The new title-job schema installer does not change `TimedClient::txn`, the 3-second `STORE_TIMEOUT`, or `timeout_store`. The caller timeout still does not prove lower-write rollback. |
| Library authority (30) | New title payload/trait methods and title-specific demand guards do not change the `LibraryScan` / `MetadataRefresh` false arm. The added SQL conditions are conditional on `dv_title_scan`; the selected library jobs retain their voter-only authority path. |
| Scanner capacity, cancellation, send, join and per-file loop (34–38) | The sole source change is the `dv_title` module declaration. The selected library scanner still uses four pages, checks receiver closure/cancellation per entry, blocks on page send and awaits its walker before proceeding. Filesystem-return premises remain necessary. |
| Physical admission (41) | `admit_fragment` delegates to `admit_source_work`, passing the software budget clamped to at least one as its estimate. The helper retains the idle check, try-only heavy permit and try-only bundle admission, and returns their owned `FragmentAdmission`. Its additional settings read is an external Store wait; suitable capacity and returning calls remain premises. The model does not promise acceptance for every settings observation or establish title-scanner progress. |

Only these files' binding hashes were refreshed in `mapping.json` and
`source-anchors.json`; exact-string anchors and model/configuration inputs are
unchanged. Other historical explanatory anchors were not refreshed. This
source disposition adds no title-publication, whole-cluster liveness, shutdown
or qualification claim; the package's contract and remaining premises apply.

## Useful completion is the durable request reply

```text
readable candidate + physical admission
    -> queue claim -> scan:library:<id> domain lease -> bind both owners
    -> scan / enrich / publish catalogue rows
    -> complete request + pending waiter (fenced update)
    -> resolve a lost completion reply by reading the request
    -> release domain lease -> settle/retire queue claim -> release admission
```

Useful completion is the durable state change: the request has a non-null result and its waiter has moved out of `pending` (the completion trigger performs both atomically). A catalogue insert is intermediate progress: it does not reply to the requester. A scan can return and still fail to publish the request result. The worker's direct acknowledgement or follow-up read is knowledge of that state, not the state change itself. Cleanup follows the request attempt and is a separate progress obligation.

## Wait edges in the inspected path

| Edge and source | Resource held while awaiting | What completes the wait; reverse dependency seen | Bound and model treatment |
|---|---|---|---|
| Candidate scan and claim: [`claim_library`](../../crates/plurxd/src/background_jobs.rs#L2271) | No claim or domain lease yet; checks each candidate's library and root readability. | Store candidate/read calls; async `read_dir` completes per root. `admit_fragment` is try-only; no permit returns without claiming. Authority is checked again before the durable queue claim. No reverse edge found. | Hiqlite Store calls have per-call timeout; root filesystem readiness has no source deadline. Model readable root/available admission as premises, or refusal as idle. |
| Queue claim / ambiguity: [`claim_with_resolution_inner`](../../crates/plurxd/src/background_jobs.rs#L303) | No domain lease yet. | `claim_job` reply completes directly. On error, `resolve_claim` and possibly `background_job` repeat until a fixed claim deadline, with 250 ms sleeps. No reverse callback found. | Model claimed, refused, settled, and deadline outcomes; do not assume a claim succeeds. |
| Queue token heartbeat: [`JobFence::renew`](../../crates/plurxd/src/background_jobs.rs#L517) | Holds queue-token `Mutex` across authority, `renew_jobs`, and possible `resolve_claim` awaits. | Authority/Store replies. `bind_library`, `complete_library`, `settle`, and retirement also need this mutex. No backend callback taking it was found. | Store calls are bounded at the hiqlite boundary; this job kind uses the voter check below. Contention can delay renewal; fair lock acquisition must be an explicit premise. |
| Domain lease acquire / heartbeat: [`acquire_cluster_job_with_policy`](../../crates/plurxd/src/job_lease.rs#L281), [`start_with_policy`](../../crates/plurxd/src/job_lease.rs#L51) | Acquire holds no app lock. Renewal holds publication fence RwLock write guard over coordinator renewal. | Coordinator responds; fenced writes share that RwLock, so renewal and publication can delay each other. No reverse callback into the app was found. TTL 90 s, heartbeat 30 s. | On expiry the fence is revoked and loss notified, **then the already-dispatched renewal is still awaited before invalidation and heartbeat exit**. Expiry fences publication; it does not bound cleanup. Model eventual renewal-future return as a premise for joined cleanup. |
| Bind both owners: [`JobFence::bind_library`](../../crates/plurxd/src/background_jobs.rs#L649), [`bind_library_job`](../../crates/plurx-core/src/store/publication.rs#L227) | Queue-token mutex held; domain fence read guard held through backend bind. | Store returns exact binding result. Domain renewal needs fence write lock; queue renewal needs queue mutex. Observed order: queue mutex → domain read guard → Store. No reverse order in examined callbacks. | Hiqlite backend calls time out at 3 s; timed-out writes may have ambiguous commit. Model refusal, success and ambiguity. |
| Work and admission: [`work_library_queue`](../../crates/plurxd/src/library_work.rs#L205) | `FragmentAdmission` remains held from before claim through cleanup; domain lease remains active. | Queue loss, domain loss, or 250 ms pressure tick requests cancellation; caller then awaits work. Scanner/process must return before lease release and admission drop. | Cancellation is not completion. Model cancellation-to-join separately, with filesystem return as a premise. |
| Request batches: [`process_library_requests`](../../crates/plurxd/src/library_work.rs#L344) | Queue and domain ownership; sequential request reads, scans and result writes. | Store reads return; full/targeted scan returns; result write returns. At most 8 rounds × 256 pending rows per invocation, then Yield. | Row-count bound does not bound files in a full scan or provider calls per library. Model a finite request batch, not fixed total duration. |
| Walk, stat and media: [`scan_library_with_publication_and_prune_limit`](../../crates/plurx-core/src/scan/mod.rs#L762), [`spawn_walker_with_entry_hook`](../../crates/plurx-core/src/scan/mod.rs#L85), [`record_candidates`](../../crates/plurx-core/src/scan/mod.rs#L1437) | Domain lease and physical admission remain held. Walker runs in `spawn_blocking`; MPSC capacity is 4 pages × 256 entries; `blocking_send` waits for receiver capacity. Scanner drains pages and awaits walker join. | Receiver drains pages; walker advances through `WalkDir`; receiver closure/cancellation is checked per entry. Per-file stat, probe, placement and Store calls are sequential. | Checks do not interrupt a blocked filesystem syscall. WalkDir/stat/root readiness has no source-level wall-time bound; a stuck mount can delay join while the lease expires. Managed media children have wall timeout and kill/reap, which does not bound filesystem I/O. Model finite filesystem calls as a premise. |
| Fenced item insertion: [`insert_item`](../../crates/plurx-core/src/store/publication.rs#L474), [`insert_item_fenced`](../../crates/plurx-core/src/store/hiqlite_publication.rs#L1024), [`atomic_publication`](../../crates/plurx-core/src/store/hiqlite_publication.rs#L293) | `fenced_call` holds domain fence write guard across Store; scanner also holds queue claim, lease and admission. | ID allocation, then a replicated transaction atomically renews/checks lease and inserts item; at most 4 identity-conflict attempts. Vendor leader path awaits Raft `client_write`; follower sends stream request then awaits oneshot; SQLite writer handles transactions serially. No reverse app-lock callback found. | TimedClient wraps transaction in a 3 s timeout, including hiqlite rate-limit wait and forwarding. Timeout drops caller future; it does not prove an already-submitted transaction rolled back. Model timeout as ambiguous intermediate catalogue progress, not completion. |
| Enrichment/provider work: [`JobManager::enrich`](../../crates/plurxd/src/state.rs#L5346), [`metadata` bounds](../../crates/plurx-core/src/metadata/mod.rs#L43) | Queue claim, domain lease and physical admission; each fenced publication reacquires domain lock. | Provider replies or per-item deadline end individual work. | TMDB connect/read/total are 5/10/30 s, provider budget 60 s, item deadline 120 s. Item count is unbounded, so there is no whole-library completion bound. Model finite provider processing as a named premise. |
| Complete request / lost reply: [`complete_library_request`](../../crates/plurxd/src/library_work.rs#L486), [`complete`](../../crates/plurx-core/src/store/background_jobs_library.rs#L221), [`COMPLETE_SQL`](../../crates/plurx-core/src/store/background_jobs_library.rs#L241) | Queue-token mutex and domain fence read guard held through conditional update. | One `UPDATE ... RETURNING` records result only if request/waiter are pending and exact queue claim + bound live domain lease still match. The same transaction's `background_library_request_completed` trigger marks the waiter succeeded/failed and settles the parent job when no waiters remain. On false/error, worker reads by request ID; any non-pending waiter prevents rerun. The useful-completion predicate is narrower: result non-null plus waiter succeeded/failed. Pending propagates error for retry. | Mutation timeout 3 s; authoritative read has its own Store read policy. Model the durable terminal state change separately from the worker's observation: direct `Ok(true)` or the follow-up read can reveal the terminal disposition; only non-null result plus waiter succeeded/failed is useful completion. A still-pending/error outcome is not completion. |
| Release and retirement: [`ActiveJobLease::release`](../../crates/plurxd/src/job_lease.rs#L159), worker cleanup in [`work_library_queue`](../../crates/plurxd/src/library_work.rs#L454), [`ActiveBackgroundJob::finish`](../../crates/plurxd/src/background_jobs.rs#L473), [`retire`](../../crates/plurxd/src/background_jobs.rs#L1495) | Normal order: drop publisher; revoke/cancel and join domain heartbeat; release domain lease (up to 5 attempts, 100 ms delay); settle queue attempt if needed; cancel/join queue heartbeat; retire queue token; drop admission. Retirement holds queue mutex over settle/resolve. | In-flight renewal must finish before release samples token. Queue heartbeat joins before token retirement. These are sequential after work returns; no reverse lock nesting found in this normal path. | Store timeouts bound individual calls, not every await/join. Expiry branch still drains renewal. Model cleanup separately; no overall cleanup deadline is established. |
| Daemon shutdown: [`schedule_loop`](../../crates/plurxd/src/state.rs#L5987), [`background_work_loop`](../../crates/plurxd/src/state.rs#L6067), spawns in [`main`](../../crates/plurxd/src/main.rs#L3584) | These spawned loops receive no `background_shutdown` token and have no cancellation select. | Inspected daemon code has no explicit graceful cancel-and-join edge for these loops. Normal lease-loss path cancels and awaits work; task drop uses Drop code that revokes/cancels and aborts heartbeat handles, without RPC release or join. | No graceful-shutdown theorem. Runtime abort/process-exit semantics are outside this map; lease expiry is the durable fallback. |

## Authority and replicated-store boundaries

For `LibraryScan` and `MetadataRefresh`, `JobKind::permits_artifact_execution()` is false ([`background_jobs`](../../crates/plurx-core/src/store/background_jobs.rs#L724)). `MembershipManager::may_execute_job` therefore returns through voter-only `may_run_cluster_jobs` before learner artifact checks ([`membership`](../../crates/plurx-core/src/cluster/membership.rs#L1403)). That path reads an atomic maintenance flag and `local_node_is_committed_voter`; the latter awaits `metrics_db`, whose local-server path clones a local Raft metrics snapshot ([vendor mgmt](../../vendor/hiqlite/src/client/mgmt.rs#L541)). No app mutex was found in these authority methods.

`TimedClient::txn` applies `timeout_store` to `Client::txn` ([`hiqlite Store`](../../crates/plurx-core/src/store/hiqlite.rs#L1640), [`timeout_store`](../../crates/plurx-core/src/store/hiqlite.rs#L6755)). Vendor `Client::txn` first awaits rate limiting, then leader `raft.client_write(Transaction)` or follower stream send plus oneshot reply ([`transaction`](../../vendor/hiqlite/src/client/transaction.rs#L34)). At timeout, the app-side future is dropped. The Store contract says a timed-out consensus request may still commit ([`hiqlite retry contract`](../../crates/plurx-core/src/store/hiqlite.rs#L1240)); this map does not prove cancellation of an already-submitted Raft write.

## Model boundary and claims

Faithfully model one request's queue claim, domain lease, exact binding, finite abstract scan result, fenced publication, durable request completion, the worker's lost-ack observation, and sequential cleanup. Repeated attempts need reclaim, retry/yield, takeover and completion-ack loss. The progress predicate is the durable request result becoming non-null and its waiter becoming terminal. A refusal or yield is an attempt outcome, not useful completion; model its cleanup separately. Do not use “an item was inserted” or “scan returned” as the useful predicate.

For useful completion, require readable roots, available physical admission, authority, finite filesystem/provider calls, and an eventual Store/Raft reply or explicit timeout outcome. Model queue-token and publication-lock grants as atomic FIFO transitions with the actual holder and the dependency whose return releases it; do not assume that a lock holder or the cleanup it performs eventually finishes. Derive cleanup/join progress from those held resources and their external-return edges, rather than assuming unconditional eventual cleanup. A blocked filesystem syscall can outlive cancellation, and a lease heartbeat's expiry path still drains its renewal future. These remain explicit liveness premises.

No concrete lock cycle was found within this inspected application path. This is not a transitive deadlock proof: lower Raft/network/leader recovery, remote authority construction, kernel filesystem waits and process-runtime shutdown remain outside scope. Ripwire refused because `target` was inaccessible; claims were checked directly. The JSON file is a drift receipt, not a liveness proof.
