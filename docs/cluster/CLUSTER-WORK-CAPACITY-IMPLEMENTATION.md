# Cluster work visibility and throughput

**Status:** reviewed; implementation in progress · **Written:** 2026-09-30 ·
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

On 2026-09-30, a short Docker sample across nynuc, m6, nuc4 and nuc3 showed
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
Jobs has job-type counts, state and node filters, and 25-row keyset pages.
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
At fragment execution start, attach the exact fence token's job ID and
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

Probe, artwork and semantic workers read a bounded candidate page and check
payload compatibility before requesting the heavy permit. A pipeline digest
may launch a bounded capability child: keep that child admitted, or reuse
an already cached identity; never move subprocess work outside admission.
A refusal to acquire capacity must not advance past compatible unclaimed
work indefinitely. Preserve or reset the cursor so it is revisited.

After admission, recheck shutdown and execution authority before claiming.
Keep source identity revalidation, claim resolution, lease heartbeats and
fenced publication unchanged. Capacity remains owned until physical work
has stopped and the active attempt has finished.

### 4.3 Bounded probe batches — review decision required

Proposal: execute up to two compatible media probes within one heavy
admission, only when the configured CPU budget is at least two. Each probe
has its own durable claim and heartbeat; both share ownership of the physical
admission. Force a one-thread probe invocation. Collect all children before
releasing capacity. Do not replenish a running batch; this bounds its hold
time and gives other job types a chance between batches.

Review confirmed that `background_jobs_resources.sql` assigns media probes
shared-domain reader reservations atomically at claim time. A scan parent
also holds a domain reader, so one parent often leaves room for only one
child. A refused second claim must produce a one-probe batch, with no attempt
created for the refusal. Test this explicitly; never promise two active
probes on every node regardless of domain contention.

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
findings. Implementation must satisfy the dispositions below.

| Finding | Disposition |
|---|---|
| P1: accepted claims are not successful work | Preserve legacy pacing where outcomes are ambiguous; add fast pacing only for acknowledged per-pass publication. Test Yield/Retry/refusal as unproductive. |
| P1: scan parents compete with probe children for two domain readers | Retain atomic claims; bound batches to two and accept partial batches. Add parent/child capacity and physical-lifetime regressions. |
| P1: moving admission can skip candidate pages | Keep the incoming cursor on admission/authority refusal; advance only after examination. Test compatible work survives contention. |
| P2: logout can strand a busy queue | Request-owned busy state resets at logout; old finally cannot release a new request. Regression implemented and passing. |
| P2: failed first detail fetch never retries | Poll from selected job identity, independent of existing detail data. Regression implemented and passing. |
| P2: progress can outlive the lease or exceed wire bounds | Require current lease freshness plus ID/fence/owner match; validate optional wire fields and guard epochs. |

Local evidence and fleet qualification are recorded separately when the
implementation is complete. No fleet throughput improvement is claimed from
these design decisions alone.
