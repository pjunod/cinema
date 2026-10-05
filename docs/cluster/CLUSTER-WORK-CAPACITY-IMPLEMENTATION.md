# Cluster work visibility and throughput

**Status:** implemented and reviewed; awaiting integration · **Written:** 2026-09-30 ·
**Integration:** `effort/cluster-throughput`

Read the [durable queue contract](DURABLE-WORK-QUEUE-IMPLEMENTATION.md) first.
Implement the milestones below in order, retaining the existing claim,
publication, process-reaping and playback-priority boundaries. If a change
requires weakening one of those boundaries, revise this design before coding.
The Activity changes and DVR repair precede the scheduling changes.

## 1. Objective and evidence

Cinema should explain who owns each job, what each node can run, why work is
waiting, and which execution stage has actually been observed. Watching stays
above two Activity tabs: Status and Jobs. Node and job-type selections must
query the entire queue, rather than filter only the current page.

On 2026-09-30, a short Docker sample across media1, lab6, lab4 and lab3 showed
roughly 1.3 CPU cores used out of 60 logical CPUs. That sample does not prove
sustained CPU starvation or establish a safe storage concurrency limit.
The observed upstream analysis backlog was 2,631 requests; these are not the
same population as the durable queue. Do not add the two counters together.

The code establishes more than the CPU sample:

| Constraint | Consequence | Source |
|---|---|---|
| One shared heavy permit per node | Several worker loops compete for one slot | [manager](../../crates/plurxd/src/transcode/manager/construct.rs) |
| Fragment admission reserves the software budget | Raising only the semaphore cannot create safe concurrency | [manager](../../crates/plurxd/src/transcode/manager/construct.rs) |
| Idle checks compare usage with one permit | Independently admitted heavy workers could make each other yield | [manager](../../crates/plurxd/src/transcode/manager/construct.rs) |
| Successful polling still waits 5–10 seconds | Short jobs leave avoidable gaps | [pacing](../../crates/plurxd/src/background_jobs.rs) |
| Some loops acquire heavy capacity before reading candidates | Empty or incompatible queues temporarily exclude useful work | [probe](../../crates/plurxd/src/source_probe.rs), [artwork](../../crates/plurxd/src/http/images_worker.rs), [embedding](../../crates/plurxd/src/library_search/semantic_work.rs) |
| Progress uses a cache key in fragment execution | A job UUID cannot safely identify its stage without explicit correlation | [progress](../../crates/plurxd/src/state.rs) |

## 2. Non-goals

No change to consensus membership, authority, durable lease durations, source
identity checks, storage-domain limits, provider limits, retention, or GPU
session limits. No automatic estimate of backlog completion from one sample.
No change to the rule that background work yields to playback. No unbounded
worker spawning or one worker per logical CPU.

This implementation does not raise heavy pipeline concurrency. It improves
admission efficiency and short-job throughput first. Any later concurrent
heavy pipelines need measured per-pipeline CPU and memory estimates plus
shared-storage admission; the present whole-budget estimate cannot justify
that change.

## 3. Activity contract

### 3.1 Layout and detail

Watching remains above Status and Jobs. Status shows the node matrix,
reported heavy capacity, reserved CPU threads, GPU reservations, and current
assignments. Recordings, analysis, processes, scans and sync use disclosures.
Jobs has job-type counts, state and node filters, and server-side keyset pages
(default 20 rows; selectable 10, 20 or 50), with First/Previous/Next navigation.
A native dialog shows Summary, Stages and History without lengthening either
tab. Preserve selected tab, dialog, focus and scroll across polling. Clear
protected state and close the dialog on logout.

Node selection includes the durable owner or explicit destination. Shared
unassigned work remains in the shared queue; it is not attributed to every
node. Expired owners are labelled previous owners. Counts remain global and
are labelled accordingly. Independent bounded active-job reads feed Status;
changing a Jobs filter must not hide an assignment from the node matrix.

### 3.2 Exact interfaces

Re-verify these against [HTTP jobs](../../crates/plurxd/src/http/background_jobs.rs)
and [store queries](../../crates/plurx-core/src/store/background_jobs.rs) at build
time:

```text
GET /api/v1/cluster/jobs?state=queued&kind=media_probe&node_id=<id>&limit=25
                         &cursor=<last job UUID>
JobQuery { state, kind, node_id: Option<String>, after_id, limit }
```

Apply owner/destination filtering in SQL before LIMIT. Limit is 1–100,
default 100 for existing API clients. Node identifiers use the store's
256-byte ASCII identifier grammar. Reject malformed input with HTTP 400.
Details return title/library labels, attempts and consumer kinds without
payloads, filesystem paths, claim IDs or boot tokens.

The admin-only activity `workers` map contains observed time, heavy limit,
occupied and available slots, serving/maintenance eligibility, hardware and
software usage/limits, and bounded child descriptions. Missing fields from
old peers mean unknown capacity. The 30-second freshness limit and answered
peer status apply both to totals and the node dialog. A fresh slot count is
an observation, not a promise that a particular queued job is compatible.

### 3.3 Execution-stage correlation

Extend `AnalysisProgress` with optional `durable_job_id` and `durable_fence`.
At fragment execution start and for each media probe, attach the exact fence token's job ID and
monotonic fence number to that progress guard. Keep the existing legacy key
for producer updates. Guard epoch checking prevents an older execution from
attaching its identity to a replacement row.

Expose the monotonic fence number in the admin job summary. It is not a
capability; omit the claim/boot tokens. The browser accepts progress only
when job ID, fence and owner match, the durable state is running, the lease
has not expired, and the observation is at most 30 seconds old. Reject
observations more than 30 seconds in the future. Old peers remain readable
but show “Not reported.” Terminal state always overrides cached progress.
Stage duration and byte progress are observations; queue age is not runtime.

## 4. Throughput changes

### 4.1 Productive polling

A new `IdlePoll::after_completion(published)` returns a jittered 100–250 ms
after acknowledged publication and resets backoff. Unproductive outcomes use
the existing 5–30 second capped jittered backoff. Probe, artwork and copy
workers use this explicit per-pass outcome. The legacy `delay(progressed)`
remains unchanged for preparation, fragments and library orchestration, whose
claim-based progress signals are insufficient evidence for fast polling. This
limits rapid bursts to at most ten passes per second per loop and keeps
empty, refused and failing queues quiet. A successful settlement is progress;
a refused claim is not. Audit every call site for this distinction. Existing
loops that bypass the delay after success must still reset backoff.

### 4.2 Candidate-first admission

Probe, artwork, transcode-copy and semantic workers read a bounded candidate page and check
payload compatibility before requesting the heavy permit. A pipeline digest
may launch a bounded capability child: keep that child admitted, or reuse
an already cached identity; never move subprocess work outside admission.
A refusal to acquire capacity must not advance past compatible unclaimed
work indefinitely. Preserve or reset the cursor so it is revisited.

After admission, recheck shutdown and execution authority before claiming.
Keep source identity revalidation, claim resolution, lease heartbeats and
fenced publication unchanged. Capacity remains owned until physical work
has stopped and the active attempt has finished.

### 4.3 Bounded probe batches

Execute up to two compatible media probes within one heavy
admission, only when the configured CPU budget is at least two. Each probe
has its own durable claim and heartbeat; both share ownership of the physical
admission. Force a one-thread probe invocation. Collect all children before
releasing capacity. Do not replenish a running batch; this bounds its hold
time and gives other job types a chance between batches.

Review confirmed that `background_jobs_resources.sql` assigns media probes
shared-domain reader reservations atomically at claim time. Current `main` reserves one of the two readers for live demand, so only one
background reader per storage domain can run. A scan parent owns that reader
and already probes inline under its own admission. The production callers
that dispatch leaf batches use unbound `repair:probe` coordinators. A running
scan blocks repair probes on its domain; a different domain can still run.
Two-probe batches therefore require two available storage domains, as well as
CPU capacity. Refused claims create no attempts. Keep the viewer reservation;
do not introduce a scan parent waiting for children that need its own slot.

## 5. Decisions and failure behavior

1. **Retain heavy exclusivity.** Existing estimates reserve all available CPU;
   concurrency without revised estimates would make the capacity UI dishonest.
2. **Make progress disposable and attempt-specific.** Stale progress must
   never revive a completed or re-owned job; durable state remains authority.
3. **Use bounded candidate and active reads.** A large backlog must not make
   the Activity response or the scheduler's per-pass work unbounded.
4. **Preserve physical lifetime accounting.** Shutdown, playback arrival,
   renewal failure and publication refusal must all reap children before the
   admission guard is released.
5. **Do not infer idle from silence.** A missing peer contributes neither free
   capacity nor a zero-worker count.

```text
candidate page → compatible candidate → physical admission → authority check
        │                 │                    │                   │
      empty          incompatible            refused             refused
        └─────────────────┴────────────────────┴───────────────────┘
                              idle backoff
                                                                   │ allowed
                                                                   ▼
claim + heartbeat → execute + observe → reap → fenced settlement → release
                               │
                 viewer / shutdown / lease loss → cancel and reap
```

## 6. Milestones, ownership and acceptance

All task branches integrate through `effort/cluster-throughput`. This effort
has overlapping files and does not use the independent-main exception.

| Milestone | Owned surfaces | Acceptance |
|---|---|---|
| A: Activity and DVR correction | core DVR/store queries; daemon activity/HTTP; web Activity/auth/CSS; API/operations | Focused DVR/store tests; Activity, asset load/order tests; pinned Rust check and Clippy |
| B: Plan and adversarial review | this document and docs index | Reviewer challenges correctness, capacity, source/process lifetime and regressions; dispositions recorded before C |
| C: Pacing and candidate admission | background_jobs, source_probe, images_worker, semantic_work | Pacing bounds/reset regression; candidates cannot claim without capacity/authority; no skipped work on refusal |
| D: Exact stage linkage | state progress, JobFence, HTTP summaries, Activity renderer | Old attempt, owner change, expired lease, terminal state and stale observation tests |
| E: Probe batch decision | source_probe and probe invocation only if storage contract is satisfied | Bounded concurrency, domain capacity and kill/reap tests, or explicit rejected proposal with reason |

Use Rust 1.97.1, `cargo check -p plurxd --all-targets --locked`,
`cargo clippy -p plurxd --all-targets --locked -- -D warnings`, formatting,
focused daemon tests and core tests with `--features hiqlite-store`.
Run the web Activity, asset load/order and docs-index checks. Each corrective
commit and task PR names its focused `Regression-Test` entries. Complete
required effort/promotion gates before merge. CI is not the compiler.

## 7. Rollout and how to read the result

The code changes do not themselves deploy a new fleet build. First verify the
new binary and fresh worker observations on every node. Compare matched
30-minute windows with the same job mix and no playback, then with playback.
Record completions/minute by kind, oldest queued age, yields and failures,
CPU usage, storage latency, and playback start/rebuffer latency. Faster
polling should reduce inter-job gaps; it cannot accelerate a single long
media pass. Do not report a backlog-clear ETA until completion rate and new
arrival rate are both measured.

**How to read it:** occupied heavy capacity with low CPU can indicate storage
or provider waits. Available capacity plus queued jobs can indicate payload,
source, destination or authority incompatibility. Unknown peers invalidate
fleet totals. More completions with stable failure/yield rates is useful;
more claims and yields without completions is churn.

Revert the scheduling commit if playback regression, storage latency or
claim churn rises materially against the matched baseline. The UI/DVR fix
can remain; the queue schema and payload versions do not change. Future
heavy parallelism requires a separate measured resource-class design.

## 8. Review and implementation record

The requested adversarial agent review completed on 2026-09-30 with six
findings. Implementation addresses the dispositions below.

| Finding | Disposition |
|---|---|
| P1: accepted claims are not successful work | Preserve legacy pacing where outcomes are ambiguous; add fast pacing only for acknowledged per-pass publication. Test Yield/Retry/refusal as unproductive. |
| P1: scan parents compete with probe children for two domain readers | Retain atomic claims and the newer viewer-reserved reader. Scans probe inline; repair batches span available domains and may contain one probe. Test scan contention, post-scan release and physical lifetime. |
| P1: moving admission can skip candidate pages | Keep the incoming cursor on admission/authority refusal; advance only after examination. Test compatible work survives contention. |
| P2: logout can strand a busy queue | Request-owned busy state resets at logout; old finally cannot release a new request. Regression implemented and passing. |
| P2: failed first detail fetch never retries | Poll from selected job identity, independent of existing detail data. Regression implemented and passing. |
| P2: progress can outlive the lease or exceed wire bounds | Require current lease freshness plus ID/fence/owner match; validate optional wire fields and guard epochs. |

Local evidence is recorded below; fleet qualification remains separate. No
fleet throughput improvement is claimed from these design decisions alone.

The post-integration review found two additional issues. A newer main change
reserves the second source reader for playback: the old test expectation of
a scan plus one repair probe was incorrect. Call-site review confirmed scans
already probe inline, so production reservation policy stays unchanged and
the regression now covers scan contention and subsequent probe admission.
Cancel/Retry also invalidated details opened during a pending mutation; the
completion now preserves the current selection and refetches it, with a
regression for both actions.

### Local validation record

The implementation was reconciled with `main` at `28964229c` before these
checks. Rust checks use `rustc 1.97.1 (8bab26f4f 2026-07-14)` from the pinned
local toolchain. The default host compiler is newer and was not used as
validation evidence.

| Surface | Command and result |
|---|---|
| Local binary | `cargo build -p plurxd --locked` — passed (macOS development build; not a deployed fleet image) |
| Workspace compilation | `cargo check --workspace --all-targets` — passed |
| Probe lifetime and candidate cursor | `cargo test -p plurxd --bin plurxd source_probe::tests -- --test-threads=1` — 2 passed |
| Pacing | `cargo test -p plurxd --bin plurxd publication_pacing_keeps_refusal_retry_and_yield_on_idle_backoff` — passed |
| Exact attempt attachment | `cargo test -p plurxd --bin plurxd durable_progress_attachment_cannot_relabel_a_replacement_execution` — passed |
| Signed Activity observations | `cargo test -p plurxd --bin plurxd http::internal_activity::tests -- --test-threads=1` — 20 passed |
| Artwork and copies | `cargo test -p plurxd --bin plurxd http::images::worker::tests` and `http::transcode_copies::tests` — 6 passed |
| DVR catalog purge | Core lib filters `purged_recordings_remove_only_their_catalog_entries` and `failed_catalog_purge_keeps_links_for_retry`, with `--features hiqlite-store` — passed |
| Empty recordings and filtered queue | Core lib filters `empty_recordings_library_is_normal_but_unexpected_losses_stay_protected` and `background_job_node_filter_precedes_pagination_and_excludes_unassigned_work`, with `--features hiqlite-store` — passed |
| Probe subprocess behavior | `cargo test -p plurx-core --features hiqlite-store --lib scan::probe::tests -- --test-threads=1` — 28 passed |
| Storage contract | `cargo test -p plurx-core --features hiqlite-store --test store_contract probe_batches_respect_scan_parent_and_shared_domain_reservations` — passed |
| Activity render and asynchronous state | `node tests/web/activity-node-names.test.js` — passed, including mutation/inspection, session reset, stale attempt and pagination regressions |
| Browser behavior | `node tests/web/durable-work.browser.cjs` with bundled Playwright — passed: 105-job pagination, focus, detail retry, stale responses, disclosures, desktop/mobile containment |
| Shell contracts | Asset load/order/layout, page-read-budget and `scripts/web-types` — passed; the type baseline decreases by one existing diagnostic |
| Documentation | `python3 -m unittest tests.operations.test_docs_index` — 4 passed |

Local process and socket tests ran outside the restrictive filesystem sandbox
where macOS denied the test harness's child-process or socket operations. The
browser uses fixture data; it does not assert that the live fleet is running
this build. No fleet qualification receipt or matched throughput measurement
is claimed here. Effort integration and release gates remain required.
