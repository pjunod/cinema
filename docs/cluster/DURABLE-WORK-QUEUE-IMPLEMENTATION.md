# Durable cluster work — one queue, useful workers, a bounded delivery

**Status:** adversarial findings addressed; awaiting external review ·
**Written / revised:** 2026-09-25 ·
**Executes:** Paul's cluster-utilization direction, with the durable job queue
as the first deliverable. Reviewed against checkout `bafeb08766ce057634f3fab0850cdd9e03507a98`;
running-fleet configuration and performance have not been measured here.

Read this document for the build order, queue contract, and stopping points.
[CLUSTER-MEDIA-POOL-PLAN.md](CLUSTER-MEDIA-POOL-PLAN.md) explains the existing
media pool; [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) and its dated
amendments govern delivery. This plan adopts the repository's current fast
lane rather than restoring its older automatic full-suite workflow.

Build a useful queue first. Publish that release after M1–M3; the extensions
in §9 have concrete contracts but do not delay it. Preserve live-session
ownership, media identity, authorization, and artifact verification. If a
change needs to weaken one of those contracts, resolve that design explicitly
instead of treating the queue as permission to bypass it.

## 1. Outcome and limits — finish a product, not a scheduler platform

The first release gives every eligible worker access to one durable work
queue. It consolidates pre-transcode and fragment-index execution and shows
why work is queued, running, retrying, or finished. New subtitle and library
maintenance adapters follow in E0; they do not delay the durable core.
Restarting a daemon does not lose accepted work. Losing an owner does not
allow it to publish after another worker takes over. Idle capacity becomes useful without stealing a
viewer's encoder or flooding shared storage.

### 1.1 Three core milestones, then bounded extensions

| Delivery | Contents | Completion boundary |
|---|---|---|
| M1 | Durable queue, both Store backends, worker lifecycle, pre-transcode adapter | Real jobs survive restart and exclusive publication is proved |
| M2 | Existing fragment-analysis queue and targeted hydration | Both existing durable adapters use one ownership system; equivalent builds converge |
| M3 | Activity/Developer integration, advisory requirements, migration and focused cluster evidence | First useful release can merge and ship |
| E0 | Shared subtitle preparation, independent library maintenance, learner artifact execution | New adapters use the established queue without redesigning it |
| E1 | Local catalogue reads, coordinated artifact caching, bounded predictive preparation, artwork derivatives | Faster browsing and fewer repeated preparations |
| E2 | Shared semantic embeddings, bounded probe work, integrity/repair scheduling | Expensive batch work uses idle workers without duplicate computation |
| E3 | Better media placement and shared Live TV ingest | Spare encoding capacity and tuner sharing improve concurrent playback |

M1–M3 are one effort with three intended task PRs. A milestone may take one
small additional integration PR if needed to keep its migration reviewable;
three is a planning bound, not a demand for oversized diffs. Do not split every
table, endpoint, and test into a separate milestone. E0–E3 follow the first
release; each is independently useful and can be finished without the next. Keep this document as the execution ledger (§12), not a new
plan/status/handoff document set for each adapter.

### 1.2 What this effort does not build

- No Kafka, Redis, external scheduler, service discovery service, or new daemon.
  The existing Store and authenticated peer transport are sufficient.
- No arbitrary task scripts, user-defined DAGs, plugin executors, or workflow
  language. Job kinds and their payloads are a closed Rust enum.
- No distribution of one live encode across segment ranges. Place whole live
  sessions; distribute independent preparation jobs.
- No rewrite of playback control, scan identity, encoder recipes, DVR recording
  ownership, or metadata-provider matching. Adapt their existing contracts.
- No media or artifact bytes in Raft. Replicate ownership and small manifests;
  transfer bytes over the existing peer transport or verified shared cache.
- No requirement to run every optimization to make the queue useful. Existing
  user choices to disable speculative encoding or semantic search remain valid.
- No zero-downtime migration framework for the first queue conversion. Use the
  bounded maintenance cutover in §7 instead of maintaining two live schedulers.

### 1.3 Features run without certification gates

Normal functionality runs by default when implemented. There are no release
receipts, benchmark pass bits, hidden environment switches, fleet certification
flags, or all-voters-compatible predicates deciding whether a feature is allowed.

An existing enable/disable preference may remain in **Settings → Developer**,
with observed requirements beside it. Saving the preference succeeds even when
requirements are unmet, unknown, or temporarily unobservable. Requirements are
advisory. No second switch for the same feature, and no per-job-kind rollout
flags. The durable queue itself is infrastructure, not a feature toggle.

Execution still needs actual resources and valid authority: an unreadable file
cannot be read, an unsupported codec cannot be decoded, a full disk cannot
accept output, and an expired owner cannot commit. These return explicit
operation results or leave work waiting for a suitable worker. They do not
disable the feature cluster-wide or prevent saving a setting. Missing speed
history alone must not reject an otherwise supported operation.

Distinguish this product policy from CI: the existing merge checks still block
bad changes. No new product gate is justified by calling it a quality check.

## 2. Starting point — reuse the working contracts

Re-verify symbols against the intended implementation base before editing.
These are source observations, not assertions about deployed settings.

| Existing seam | Source | Preserve / change |
|---|---|---|
| Distributed whole-title queue | `PretranscodeJobStore` in [store/mod.rs](../../crates/plurx-core/src/store/mod.rs), [hiqlite_pretranscode.rs](../../crates/plurx-core/src/store/hiqlite_pretranscode.rs), [sqlite/pretranscode.rs](../../crates/plurx-core/src/store/sqlite/pretranscode.rs) | Preserve atomic claims, fenced cache publication, retry semantics and resumable local output; move ownership to the common queue |
| Fragment jobs and analysis requests | [fragment_index_cluster.rs](../../crates/plurx-core/src/store/fragment_index_cluster.rs), [state.rs](../../crates/plurxd/src/state.rs) | Preserve analysis request IDs, attempt history, source attestations, targeted hydration and typed retry policy |
| Verified peer artifacts | [fragment_index_cluster.rs](../../crates/plurxd/src/fragment_index_cluster.rs), [http/images.rs](../../crates/plurxd/src/http/images.rs), [shared_cache.rs](../../crates/plurxd/src/shared_cache.rs) | Reuse manifests, digest verification, holder discovery and bounded streaming |
| Singleton publication fence | [job_lease.rs](../../crates/plurxd/src/job_lease.rs), [publication.rs](../../crates/plurx-core/src/store/publication.rs) | Preserve transaction-level rejection of stale catalogue writers |
| Local encoder admission | [admission.rs](../../crates/plurxd/src/admission.rs) | One owner of physical encoder/thread permits; no competing queue counters |
| Media offers and routes | [media_pool.rs](../../crates/plurxd/src/media_pool.rs), [media_sessions.rs](../../crates/plurxd/src/media_sessions.rs), [http/hls.rs](../../crates/plurxd/src/http/hls.rs) | Keep live-session ownership separate from background jobs |
| Local catalogue reads | `CatalogueReader` in [store/mod.rs](../../crates/plurx-core/src/store/mod.rs), [config.rs](../../crates/plurx-core/src/config.rs) | Existing mechanism is opt-in; E1 widens verified route coverage and makes the normal path automatic |
| Semantic search | [library_search/semantic.rs](../../crates/plurxd/src/library_search/semantic.rs) | Keep local serving index; E2 distributes versioned item embeddings |
| Settings and web layout | [http/system.rs](../../crates/plurxd/src/http/system.rs), [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) | Change backend enablement as well as checkbox behavior; use the existing split scripts |

The current pre-transcode queue already uses a 30-second claim renewed every
10 seconds, a 4,096-active-row ceiling, bounded retries, and atomic cache
publication. Fragment indexing already has a separate durable queue and can
settle a request by hydrating another node's artifact. This is consolidation,
not a claim that background durability or peer reuse must be invented.

Current media settings still refuse enablement until every voter publishes
the current protocol. Runtime placement also checks that fleet-wide predicate.
E3 removes that **policy prerequisite** and improves selection. The common
queue never inherits it. Actual peer RPC compatibility remains a per-peer
execution constraint.

## 3. Architecture — durable intent, one publication owner, local execution

```text
 scan / user request / schedule / prediction
                    |
             typed enqueue + dedupe
                    v
          Store: background_jobs + waiters
                    |
     candidate reads, capability matching, local reservation
                    |
       atomic claim + shared-resource reservation
                    v
             worker on node A / B / C
                    |
          existing domain implementation
                    |
       immutable private output + verified manifest
                    |
       fenced publication transaction + waiter completion
                    v
     artifact/catalogue result -> existing serving paths
```

Every daemon hosts the same worker service. SQLite runs it locally; Hiqlite
allows distributed claims. There is no elected scheduling master for execution.
Existing singleton producers may still discover work once and enqueue it.
Queue discovery is advisory; a Store transaction decides the owner.

Live stream creation stays on its existing bounded request path. It does not
wait behind durable background tasks. Playback may request preparation and
join its result within the existing start deadline; an expired caller deadline
detaches that caller, not every consumer of the shared job.

**Delivery guarantee:** execution is at least once. A lost worker may have
performed work before its death became visible. Committed publication is
exclusive and idempotent. Do not promise exactly-once process execution.

### 3.1 Keep ownership distinct from domain records

The common queue owns dispatch, claim generation, renewal, cancellation,
backoff, resource reservations, and terminal status. Domain tables continue
to own cache recipes, analysis requests/history, offline package permissions,
scan generations, and artifact manifests.

An analysis request or offline package becomes a **waiter** for work, not a
second worker lease. Several requests can wait for one computation. A build
succeeds only when its artifact pointer and job completion commit together.
Waiters needing only that artifact reference can settle in the same transaction.
A target-node request needing local bytes instead remains `awaiting_hydration`:
the same commit records its artifact identity and deterministic hydration intent.
A bounded dispatcher materializes that intent as an `artifact_hydrate` job when
queue space permits. Verified local installation, holder publication and waiter
completion then commit together. Cancellation at either phase prevents that
waiter's success; it need not cancel another consumer's build or hydration.

This is the only deferred-finalization path in the first release. It has a
durable domain record even if the queue is full, and a uniqueness key of
`(artifact_key, target_node_id)`. There is no unspecified generic follow-up
callback or claim that a successful shared build has already delivered bytes
to every target. Domain request status remains pending until its own contract
is satisfied.

Retain a separate singleton lease only where the domain requires serialized
catalogue mutation, such as a library scan. Its renewed token is checked in
the same transaction as the job token on every mutation. Losing either stops
the operation. Do not repurpose a media-session lease as a background claim.

### 3.2 Worker eligibility is an explicit role contract

The core release preserves current worker-role eligibility: voters and
single-node daemons execute the two migrated queues. It does not widen learner
mutation authority as a side effect of changing their Store traits. Existing
learner media serving and artifact reads are unchanged.

E0 adds learner execution through `may_execute_job(kind)` alongside the existing
singleton authority check; never set `may_run_cluster_jobs` universally true.
A ready learner's own fenced completion may atomically publish only verified
immutable artifact metadata, cache/holder locations and the matching immutable-
result waiter transitions, including `awaiting_hydration`. It may not mutate
catalogue identity, watch state, user/package authorization or quota policy,
scan/provider ownership, schema or membership. Those remain voter operations.
Existing offline quota checks precede waiter attachment; completing a matched
artifact does not grant a download capability.

Both Store backends use the same typed operation allowlist. Validate committed
membership/serving authority at dispatch and immediately before publication,
and use existing removal/serving fences; a self-reported worker role is not
authorization. Reject a removed worker's result. This is a narrow publication
contract, not a new voter-finalization service. If a result needs a catalogue
mutation, that kind stays voter-only. Internal endpoints retain peer auth.

## 4. Queue contract — the part that deserves most of the engineering

Names in this section are **proposed interfaces**, not functions already in
the tree. Add the common types and Store trait in a dedicated background-jobs
module, with SQLite and Hiqlite implementations. Keep the daemon worker in
its own module instead of adding another large loop to `state.rs`.

### 4.1 Durable schema and bounded inputs

Use equivalent strict SQL in both backends. The essential row is:

```sql
CREATE TABLE background_jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    payload_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    source_generation TEXT NOT NULL,
    priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 3),
    state TEXT NOT NULL CHECK (state IN
        ('queued','running','cancelling','succeeded','failed','cancelled')),
    target_node_id TEXT,
    owner_node_id TEXT,
    owner_boot_id TEXT,
    claim_id TEXT,
    fence INTEGER NOT NULL DEFAULT 0 CHECK (fence >= 0),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    lease_expires_ms INTEGER,
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    not_before_ms INTEGER NOT NULL,
    deadline_ms INTEGER,
    checkpoint_json TEXT,
    result_ref TEXT,
    last_error_code TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX background_jobs_active_key
ON background_jobs(dedupe_key)
WHERE state IN ('queued','running','cancelling');
CREATE INDEX background_jobs_due
ON background_jobs(state, not_before_ms, priority DESC, created_at_ms, id);
CREATE INDEX background_jobs_owner
ON background_jobs(owner_node_id, owner_boot_id, state);
```

Add checked state/owner consistency in the final migration: a running or
cancelling row has an owner, boot ID, claim ID, positive fence and expiry;
queued/terminal rows do not carry a live owner. Enforce string, JSON and enum
limits before submitting a replicated write. Typed payloads accept identifiers
and normalized choices, never shell commands, bearer tokens, arbitrary URLs,
or operator filesystem paths supplied by a client.

Three small companion tables are required:

| Table | Required identity and purpose |
|---|---|
| `background_job_waiters` | Unique `(request_scope, request_id)` plus job ID, normalized request digest, consumer kind/reference, expiry and cancellation state. Repeated request IDs return the same job; different choices return conflict. User requests are scoped by user; internal producers have a separate namespace. |
| `background_job_attempts` | `(job_id, fence)`, claim ID, owner/boot, start/end, outcome, bounded error code and checkpoint reference. Retain actual attempt history without rewriting a growing JSON array on each heartbeat. |
| `background_job_reservations` | `(resource_key, slot)` plus job ID/fence and expiry. Atomically reserve the core shared source-I/O budget with the claim, renew with it, and release with settlement. E0 extends it to provider concurrency. Expired reservations cannot authorize an old owner. |

Candidate: 4,096 active jobs, 10,000 retained job rows, 16 KiB payload,
4 KiB checkpoint, 256-byte error detail and 64-byte error code. Reserve 256
active slots for explicit user/foreground preparation; automatic producers
stop at 3,840. Cap live waiters at 16,384 and 128 per user. Bound attempt rows
globally at 40,000. Keep at most 16 **resolved detailed** attempts per job;
that is a history window, not a lifetime claim/yield limit. Never reset the
monotone fence or charged-failure count when compacting history. Fold older
resolved yields into bounded totals (yield count, abandoned count, last outcome)
once their claim-resolution retry window has expired. Current and unresolved
claims are protected separately from that 16-row window.

An unacknowledged initial claim RPC has a two-minute retry/resolution window
from dispatch. After that window, its caller may inspect the job but may not
use the old claim ID to start or reacquire work. An already acknowledged running
claim instead retains its current token and receipt for its entire live lease;
long-running work and renewal resolution are not limited to two minutes. `resolve_claim` returns `expired_or_pruned`, never permission
to reissue a forgotten claim. Retain unresolved claim receipts until both this
window and their durable lease expire; thereafter even a delayed original
caller must not start. Admit new jobs only with reserved accounting room for
their current/unresolved receipts. If the global bound is reached, pause new
admission and compact eligible history; do not deadlock a healthy accepted job
at its seventeenth preemption. Record a test with more than 16 yields.

Accepted request-id receipts have a fixed seven-day minimum window from
acceptance, extended while the request is pending. State that expiry in the API.
The same ID and digest returns its original outcome throughout the window;
pressure cannot silently shorten it. After expiry, a request is new unless a
domain reference still requires its compact identity. Keep that compact
request/result mapping, not every old attempt, for long-lived domain records.
Bound these receipts with the waiter/retained-row accounting; reserve one on
acceptance and return `queue_full` when protected records leave no room.
Terminal job details may be pruned after seven days once references have been
compacted. Never prune active leases, live waiters or artifact pins to meet a
limit. These are initial constants, not new Developer knobs or certifications.

Large jobs keep a cursor and enqueue bounded pages. A million-file library
must not turn into a million replicated queue rows. Page size is at most 128;
enqueue insertion/dedupe counts and cursor advancement are transactional so a
restart neither skips a page nor duplicates accepted work.

### 4.2 Identity: requests, work, attempts and artifacts are different

- **Request ID** identifies one caller's intent and makes retries idempotent.
- **Job ID** is a UUID that is never reused, including after terminal cleanup.
- **Dedupe key** is a versioned digest of kind, source identity, normalized
  recipe/track/model choices, and pipeline version. It does not contain the
  ingress node. Include a target node only for inherently local work such as
  hydrating or verifying that node's copy.
- **Fence** increases on every claim/takeover of one job. **Revision** changes
  only on ownership/lifecycle transitions and renewals; it distinguishes stale
  same-owner tokens. Attaching/detaching waiters and adjusting scheduling
  priority do not change this ownership revision.
- **Artifact key** identifies immutable output. Completion may reference an
  existing verified artifact instead of rebuilding it.

Enqueue first resolves the scoped request ID. An already accepted request
returns that same request's outcome, including its cancellation. For a new ID,
only queued/running jobs accept new interests. A matching `cancelling` row
returns `job_cancelling` with a one-second retry hint, without inserting a waiter
or accepting work. Retry uses the same new request ID within the caller's own
deadline; after predecessor cleanup, enqueue can create a fresh job. There is
no successor queue or revival of cancelled execution in the first release.

An enqueue checks for a usable artifact before creating work. A terminal job
alone does not prove bytes still exist. Missing/corrupt locations cause repair
or a fresh job with the same artifact key. New job UUIDs prevent old completion
tokens from becoming valid after retention deletes an earlier row.

Source identity uses existing attested object generations and content hashes
where available. Size/mtime and a path alone must not become a new portable
identity. Reuse the current source-fence implementation and recheck it before
publication. Hardware-dependent facts additionally include the relevant
hardware/driver/decoder identity; never reuse one GPU's success as another's.

### 4.3 Proposed Store boundary

```rust
struct JobToken {
    job_id: String,
    node_id: String,
    boot_id: String,
    claim_id: String,
    fence: i64,
    revision: i64,
    lease_expires_ms: i64,
}

// Signatures shown without async_trait boilerplate.
async fn enqueue_job(req: EnqueueJob) -> Result<EnqueueOutcome, StoreError>;
async fn job_candidates(req: CandidateQuery) -> Result<JobPage, StoreError>;
async fn claim_job(req: ClaimJob) -> Result<ClaimOutcome, StoreError>;
async fn resolve_claim(req: ResolveClaim) -> Result<ClaimResolution, StoreError>;
async fn renew_jobs(batch: RenewJobs) -> Result<Vec<RenewOutcome>, StoreError>;
async fn settle_job(req: SettleJob) -> Result<SettleOutcome, StoreError>;
async fn cancel_waiter(req: CancelWaiter) -> Result<CancelOutcome, StoreError>;
async fn cancel_job(req: AdminCancelJob) -> Result<CancelOutcome, StoreError>;
async fn list_jobs(req: JobQuery) -> Result<JobPage, StoreError>;
```

`ClaimJob` contains one candidate ID, expected revision, fresh claim UUID,
node/boot identity, supported payload version and typed resource requirements.
`ClaimOutcome` distinguishes claimed, already claimed by this request,
contended, resource unavailable, cancelled, and unsupported kind/version.
`SettleJob` is a closed enum: publish typed result, checkpoint-and-yield,
retry typed failure, finish cancellation, or permanent failure. It does not
take arbitrary SQL or an unverified artifact pointer.

The typed publish implementation checks the job token, current source and
domain generations, waiter validity where required, and the domain's existing
publication contract **inside one transaction**. It then publishes the
artifact/catalogue result, settles the job, and releases reservations.
Existing public Store methods can delegate to this boundary during extraction;
they must not retain an independent claimable queue.

The closed registry names each delivery stage explicitly:

| Kind / stage | Typed input / output | Worker and resume policy |
|---|---|---|
| `transcode_prepare` · core | File generation + normalized recipe → existing cache generation | Core voter; E0 learner; heavy; existing same-node part checkpoint |
| `fragment_index_build` · core | Attested source + pipeline digest → verified index artifact | Core source-readable voter; E0 learner; heavy; restart or existing proven checkpoint |
| `artifact_hydrate` · core | Artifact identity + target node → verified holder location | Target node only; bounded transfer; restart incomplete object |
| `subtitle_extract` · E0 | Attested source + track/options → sidecar manifest | Source-readable voter/learner; heavy; restart incomplete extraction |
| `library_scan` · E0 | Library + normalized full/targeted intent + scan generation → existing scan result | Voter coordinator; singleton publication lease; restart census |
| `metadata_refresh` · E0 | Library + refresh policy/generation → existing enrichment result | Voter coordinator; provider budget and publication lease |

E0–E2 add only the explicitly named kinds in §9. Do not store arbitrary future
requirements as executable expressions. Each handler owns payload decoding,
source validation, resource estimates, progress reporting, checkpoint rules,
and typed result validation. Unknown versions remain visible but unclaimed.

### 4.4 Claim, renew and settle are linearizable

Candidate reads can be bounded local reads; they confer no ownership. Claim is
one authoritative transaction checking due state or expired ownership,
revision, cancellation, role, target, kind/version and shared-resource slots.
It advances the fence, installs owner/boot/claim identity, creates the attempt,
and reserves the slots. Concurrent claimers can see the same candidate; only
one succeeds. Do not perform a SELECT and a later unguarded UPDATE.

Use a 30-second lease, renewal every 10 seconds, with deterministic per-node
jitter. Renew at most 64 claims in one transaction per worker tick; each row
has its own outcome. Serialize renewal and settlement for one job so a late
renewal cannot resurrect a released owner or invalidate a successful publish.
Use explicit time parameters as the existing Store does; capture a conservative
local monotonic deadline before dispatch. A late response cannot extend local
authority past that deadline. Fences/revisions remain the publication authority;
clock assumptions and skew are recorded in the test evidence, not disguised as
a newly solved distributed-clock protocol.

A timeout is an **unknown result**, not proof that the write failed. Retry the
same claim UUID and resolve it before starting a child or claiming a different
job with those resources. Persist its identity in the attempt row. Unknown
completion is resolved by job/attempt/result identity; do not rerun work merely
because its acknowledgement was lost. Authoritative reads and reconciliation
use existing Store deadlines. Each worker has at most one unresolved claim
per local permit; no detached retry flood.

**Lost renewal reply:** resolve against the exact tuple
`(job_id, node_id, boot_id, claim_id, fence)`. `resolve_claim` returns one of
`running(current_token)`, `cancel_requested(cleanup_token)`, `settled(result)`,
`lost_ownership`, or `expired_or_pruned`. It never returns a successor owner's
token. `ResolveClaim` carries these identities; only initial-claim resolution
may omit the as-yet-unknown fence, and then the never-reused claim UUID must
match its recorded attempt. Renewal resolution requires the known fence.
A serialized per-job lifecycle task may adopt a newer revision only for
that exact running claim. A cancellation token permits cleanup settlement only;
it cannot renew execution or publish output. Until resolution succeeds, the
last confirmed conservative monotonic deadline remains binding. A response
arriving after it cannot revive a stopped worker, even if the durable renewal
committed and delays takeover. Drain in-flight writes before final release.

Waiter/priority edits preserve the ownership revision and therefore cannot
invalidate an otherwise healthy worker's next renewal. Candidate ordering is
advisory: claim rechecks current due state and requirements in its transaction.
Test renewal-commit/reply-loss both with a waiter joining and with cancellation
or takeover occurring before resolution.

On quorum loss, deadline expiry, cancellation or serving-authority loss, the
worker stops publication and cancels its child. Keep the local resource guard
until the child actually exits and file writers join. A guard must not release
a GPU slot merely because its HTTP request was cancelled. A bounded supervisor
owns unresolved shutdown, while durable expiry allows another node to proceed.

Lease heartbeat is liveness of the worker service, not proof that its task is
making progress. Carry each handler's existing bounded I/O/child timeouts into
the adapter. Add a progress watchdog for new handlers: advancing bytes/frames,
completed entries or a documented waiting phase. A renewing worker with a hung
child must eventually cancel/join it and settle a typed retry. Persist coarse
checkpoints at most every 30 seconds or at meaningful phase boundaries; send
fine progress through live telemetry, not one consensus write per frame/file.

### 4.5 State transitions and cancellation

```text
 queued --claim---------------------------> running
   |                                         |
   | cancel                           verified publication -> succeeded
   v                                         |
 cancelled                         retry / yield -> queued
                                             |
                                   permanent error -> failed
                                             |
                                     cancel -> cancelling
                                                 |
                                   joined child or expired claim
                                                 v
                                             cancelled
```

An expired running claim is eligible for takeover with a higher fence.
An expired cancelling claim is finalized as cancelled, never executed again.
Cancellation wins by transaction ordering: if completion committed first,
report already completed; if cancellation committed first, publication is
rejected. Cancelling one waiter does not cancel work needed by another waiter
or by a still-valid background intent. Record that background interest
explicitly; it is not inferred from a priority number. An administrator can
cancel the whole job.

No task can be left in `cancelling` indefinitely: worker cleanup handles the
normal path; an expiry sweeper finalizes abandoned rows in bounded batches.
Cleanup never deletes an active reader's artifact or a current writer's
directory. Ready output and job history have different retention lifetimes.

Caller deadlines belong to waiters. Expiring the earliest viewer must not
expire a shared job still needed by another viewer or an offline package.
Derive effective priority from active interests, and lower it when the urgent
waiter leaves. `deadline_ms` on the job is its own kind-specific lifetime,
never a copy of the first HTTP request's timeout.

### 4.6 Retry policy distinguishes failure from lack of opportunity

| Outcome | Queue behavior |
|---|---|
| Another node owns it | No attempt charged; try another eligible candidate |
| Local mount absent / unsupported capability | Skip locally for 30 s; other nodes remain eligible immediately |
| Foreground preemption / capacity full | Checkpoint if supported, yield after child exit; no failure charged |
| Transient I/O or provider timeout | Retry at 5 s, 30 s, 2 min, 10 min; fifth charged failure is terminal, with bounded jitter |
| Source replaced/deleted | Cancel obsolete generation; enqueue current generation only if demand still exists |
| Invalid payload / unsupported payload version | Reject invalid enqueue; otherwise wait for a compatible worker, without consuming failures |
| Corrupt produced output | Never publish; typed failure and bounded retry |
| Node crash / expired execution | Record abandoned attempt; recover checkpoint or restart; repeated identical crashes consume the finite retry budget |

These defaults apply to new kinds. Preserve the existing fragment-analysis
retry cycle, diagnostics and explicit retry semantics during migration; map
them to a typed policy rather than silently shortening them to five attempts.
Source-unreadable exclusions are node-local and bounded by active job count.
Do not set a six-hour global delay because one node lacks the mount.

Explicit Retry makes a new audited request/retry generation; ordinary scheduler
ticks cannot erase the retry budget. Synthetic producer intents expire after
24 hours unless refreshed by real demand. User-requested jobs remain visible
until completion/cancellation or the domain's documented deadline.

### 4.7 Publication has a filesystem crash boundary

Write into a private directory keyed by job ID and fence. Validate the output,
write its bounded manifest, and atomically rename to an immutable generation
on the same filesystem. Flush according to the existing artifact durability
contract before publishing the pointer. Then perform the fenced Store commit.

Crashing before rename leaves staging. Crashing after rename but before commit
leaves an unreferenced generation. Crashing after commit but before response
leaves a result that reconciliation can return. A stale worker's final files
may exist but cannot replace a newer generation or become a valid holder row.
Bounded orphan cleanup removes them only after checking current ownership and
reader pins. Do not make the replicated commit pretend it atomically renamed
files on a different machine.

## 5. Scheduling — protect playback and spend spare capacity

### 5.1 Priorities and fair selection

| Priority | Work | Rule |
|---|---|---|
| 3 | Preparation explicitly needed by a waiting viewer | Highest durable-queue priority; caller deadline still applies |
| 2 | Explicit maintenance and offline requests | Ahead of speculation; respect per-user/domain limits |
| 1 | Repair, scheduled maintenance and likely-next preparation | Use spare capacity with bounded age preference |
| 0 | Speculative whole-title encoding and cold prewarming | First to yield |

Live media permits outrank every queue class. Within a class, use oldest
eligible work with round-robin user/library scope; avoid a bulk request owning
every lane. For background-only service, allow one eligible lower-class item
after eight higher-class completions. This never jumps ahead of a waiting live
viewer or priority-3 request. Continuous foreground saturation may legitimately
starve maintenance; show that reason rather than claiming a hard completion SLA.

Read candidates in pages of 128 with a stable cursor. Skip incompatible rows
and continue bounded pagination, retaining the cursor between ticks; the first
page of GPU jobs must not permanently starve CPU work. Candidate reads and
empty polling must not write leases or timestamps to Raft. Poll idle workers
with jittered 5–30 s backoff; authenticated wake hints and local enqueue signals
reduce latency but are disposable, not the queue's durability mechanism.

### 5.2 Resource accounting has one owner per resource

Use existing admission guards for GPU sessions, software threads and scratch.
Reserve locally before attempting the durable claim; release on contention.
Do not hold a claimed job while waiting an unbounded time for a local permit.
Start with one heavy background task per node and at most two light tasks.
Heavy means full-source reads, FFmpeg, hashing or model inference, not a label
chosen by the producer. This is a conservative starting budget, not a throughput
claim. Existing explicit user limits remain authoritative.

The core needs one conservative cluster-wide source-I/O domain, defaulting to
two heavy background readers. Its reservation rows use the same claim/expiry
transaction; there is no storage-domain configuration project in M1–M3.
E0 adds a replicated storage-domain ID to library roots so aliases of one NAS
share a limit, with the global domain as the unmapped fallback. It also adapts
the existing provider rate policy into a global allowance rather than multiplying
it by node count. Concurrency leases are not rate limits: E0 adds a bounded
token/refill record when the provider needs a time-based quota. Provider/scanner
machinery remains unchanged until that adapter is implemented.

Claim all required shared slots in the same transaction or claim none. Resource
ordering cannot deadlock a worker halfway through acquiring two domains.
Renew/release reservations with the job token. A freed shared slot does not
release a still-running local child; the local guard remains until exit.
Foreground I/O is not forced through the background queue. Stop new background
reads when observed playback/storage pressure rises; existing cancellation
points bound how quickly a heavy reader yields.

The existing foreground encoder yield budget is five seconds. Test that bound
with an actual child, not a counter-only fixture. Pausing an encoder with
SIGSTOP does not return its GPU session; terminate/checkpoint background work
when the viewer needs that resource.

Reservations bound admitted work, not every possible physical read after a
partition. An old child can briefly overlap a replacement until it observes
loss and exits. Publication fencing still holds. Measure this overlap in the
loss drill and report it; do not claim a leased semaphore makes external I/O
exactly once. Provider calls already in flight have the same limitation, so
retain provider timeout/backoff rules and charge dispatched calls to the rate
budget even if their responses are lost.

### 5.3 Telemetry informs selection without becoming permission

The core reuses node snapshots for free encoder/thread slots, scratch
reservations, source readability, queue permits and completed-job timings. E3
adds measured disk-read latency and peer/client output throughput; these new
measurements do not delay the durable queue. Label unknown data as unknown; do not substitute an
active-session count and call it measured network pressure.

Fresh capability facts select where a job can execute. Historical speed and
pressure rank candidates; they do not certify that a user may try a feature.
The selected worker always performs atomic local admission. A snapshot alone
cannot reserve a slot or make concurrent starts safe.

## 6. Core adapters — useful work in the first release

### 6.1 Whole-title transcode and offline demand

Move speculative transcode ownership to the common queue; keep the current
recipe hash, cache budget, manifests, part resume, encoder invocation and
output validation. Existing offline preparation joins the same recipe job
when output choices match, at priority 2, without exposing another user's
download capability. Raising a job's priority is atomic and never creates a
second producer. Preserve per-user offline quotas before attaching a waiter.

The old candidate singleton becomes an enqueue producer only. Remove its old
execution loop in the same milestone. Preserve completed cache locations and
existing package references; do not re-encode the library during migration.

### 6.2 Fragment indexes and portable source preparation

Convert build ownership to one job per source/pipeline artifact key. Today's
targeted per-node fragment requests become waiters plus separate node-targeted
hydration jobs where local bytes are needed. One successful build can satisfy
several requests without attributing its output to the hydrating node.

Preserve analysis phase history, admin retry/cancel behavior, source generation
checks and the existing hydration completion contract. Add portable probe facts
only where the current consumer can validate their schema and source identity;
decoder/hardware execution evidence remains local.


## 7. Migration and rollback — one ownership system after cutover

Use the next SQLite/Hiqlite schema versions on the actual base; do not reserve
a version number in this plan. Migration changes both backends and their import,
backup, restore and contract fixtures in the same milestone.

### 7.1 Deliberate maintenance cutover

The first deployment has a short, explicitly scheduled maintenance window.
It is a deployment procedure, not a runtime feature qualification requirement.
Do not claim seamless mixed-version queue operation.

1. Take the existing supported consistent backup and record the source schema.
2. Stop old daemon processes and maintenance CLI workers, including their
   children, before any new binary starts. Stage the new binary on every node;
   preserve data directories and cluster membership. An offline member must
   have its old service disabled or be removed by the existing membership
   procedure so it cannot later rejoin as a legacy writer.
3. Start the new voting binaries using the existing coordinated schema
   migration path. They restore quorum; the migration transaction installs the
   new schema and seals the legacy queue snapshot before application job
   services start. Learners never migrate. No legacy row can be claimed again.
4. Build the deterministic mapping described below. Imported old running jobs
   become queued only after the stopped-process boundary; record the abandoned
   legacy attempt and new execution identity. Cancelling jobs become cancelled
   after cleanup, never executable. Preserve ready artifacts, domain histories,
   package references and live request IDs.
5. Materialize the first bounded page. Commit new jobs, waiter/request mappings,
   checkpoint references and the migration cursor together. Only committed
   pages are claimable; unmaterialized requests remain accepted in the sealed
   backlog, visible as `awaiting_import`. A crash repeats a page idempotently.
6. Start new workers and serving traffic. Continue materializing backlog pages
   as capacity frees. Verify request conservation and canonical job counts,
   rather than requiring old queue-row count to equal new job count. Legacy
   execution code is removed; historical rows needed by APIs remain read-only.

**Canonical mapping, not row copying.** Legacy fragment jobs are keyed by
`(cache_key, target_node_id)`. Group compatible rows by source/pipeline artifact
identity into one build job with a deterministic migration UUID. Preserve each
legacy target request, request ID, retry cycle and history as a separate waiter
or historical domain record. Its target obligation becomes hydration of the
canonical result. A valid existing artifact skips the build; it does not imply
that every target already has the bytes. Pre-transcode jobs group only when the
complete normalized recipe and source generation match.

Select at most one resumable checkpoint deterministically: greatest verified
progress from a compatible source/pipeline, then stable node/job-ID ordering.
Keep its staging-node affinity. If that node is unavailable, restart elsewhere;
never combine parts from different attempt generations. Preserve other staging
references for bounded orphan cleanup, not as concurrently resumable owners.
Initial priority is the highest live interest, and a build is due when at least
one live interest is due. Keep each migrated fragment request's charged failures,
error history and retry deadline; joining the canonical build does not reset
its budget. Charge an execution failure once to each participating due interest
whose typed policy charges that failure. Exhausted/cancelled interests do not
schedule more work, while another valid interest may still obtain the artifact.
Do not sum failures from duplicate old target jobs into an artificial failure
budget for the shared build. The common job records execution totals; the
preserved domain retry ledgers decide which interests still authorize retry.

**Capacity during migration.** The two old queues can each contain 4,096 active
rows. Do not drop requests or force all of them into the new 4,096-job limit.
Keep the sealed legacy rows as a finite migration backlog with a durable cursor
and unique legacy-to-canonical mapping. Import at most 128 entries per page and
only as many jobs/receipts as current limits admit. Mapping an additional target
to an existing canonical build is idempotent; if waiter capacity is exhausted,
that target remains in the backlog. Reserve normal foreground admission headroom;
new background producers yield to backlog draining. Existing backlog is not a
new generic overflow queue, and no new requests may write it.

The migration marker, sealed snapshot identity and initial cursor commit with
the schema. Page commits are atomic and resumable. Retain the sealed source
until every accepted legacy request has a result, a new waiter, an explicit
cancel/failure outcome or a still-visible backlog entry; no request disappears
because its canonical group was seen on an earlier page. A compact artifact
result mapping lets later pages reuse a build that already completed. Track
request counts by outcome and canonical group, not one-to-one job-row counts.
Test two target rows for one artifact, incompatible recipe generations,
competing checkpoints, a restart mid-page, and a valid old snapshot with more
than 4,096 combined active requests. No partial page is reported as imported.

No rolling bridge, dual writes, or repeated all-voter feature checks are added.
The old binary's new-schema refusal remains a database compatibility check.
Stopping all old processes matters because a startup schema check alone cannot
fence a process that was already running. If a seamless rolling upgrade becomes
a requirement, it is a separately scoped compatibility protocol, not a shortcut
to remove this boundary.

### 7.2 Recovery is concrete

Before migration commit, restart the matching old build and supported snapshot.
After schema conversion, prefer rolling forward with a repair build. A binary
downgrade alone is not supported. Restoring the pre-cutover snapshot requires
stopping all writers again and explicitly accepting loss of subsequent durable
changes. Retained cache bytes can be revalidated; they cannot reconstruct lost
user mutations. Do not delete old snapshots until the new queue has passed the
post-deploy checks and the normal backup retention policy permits it.

## 8. Operator surface — explain the work without adding a control panel

Use existing Activity and Developer pages. No new top-level navigation item.
Core Developer work is read-only job/capability observation; there is no queue
switch, new feature-certification rule or redesign of existing media routing.
The E3 changes called out below remove old media-pool policy gates when that
surface is implemented; they are not a dependency of the durable core.
Activity shows kind, title/library, priority, state, owner, elapsed time,
last progress, retry time and a bounded waiting/error reason. Paginate at 100
rows with a cursor. Show durable status separately from live worker progress:
stale telemetry is “last observed”, not proof of a dead worker.

Proposed admin API, added to [API.md](../API.md) in the implementation commit:

| Endpoint | Meaning |
|---|---|
| `GET /api/v1/cluster/jobs?state=...&kind=...&cursor=...` | Bounded list, counts and next cursor; no payload secrets or source paths |
| `GET /api/v1/cluster/jobs/{id}` | Authorized detail, waiters summary and bounded attempt history |
| `POST /api/v1/cluster/jobs/{id}/cancel` | Idempotent administrator cancellation |
| `POST /api/v1/cluster/jobs/{id}/retry` | Explicit new retry request; conflict if still executing |

Ordinary users continue through domain endpoints for their own offline or
playback requests; they do not get cluster-job administrator access. There is
no public generic enqueue endpoint.

Developer displays existing relevant enable/disable preferences, capability
observations, storage availability and reported version compatibility. Values
are `met`, `unmet`, `unknown` or `unavailable`, with observation timestamps.
Save remains enabled in every observation state. Slow readiness fetches cannot
block initial form rendering or erase unsaved edits.

**E3 owns existing media behavior changes:** remove media-pool/all-voter
enablement refusals from settings and runtime global
placement, retaining per-peer compatibility and actual admission. Treat an
incompatible or missing peer as one unavailable candidate. With two compatible
nodes and one stale peer, those two still place work. Missing compatible peers
falls back to an eligible local worker or leaves the job queued with its reason.
Retain explicit existing disabled preferences; use enabled defaults for newly
introduced automatic behavior, without turning on a previously disabled
speculative transcode or semantic-search feature.

E3 also keeps existing takeover preferences editable without fleet receipts. Actual
takeover still obeys recipe compatibility, ownership expiry and playback format
constraints. This plan does not promise that every existing session format can
be taken over.

Metrics use bounded labels: kind, state, outcome, resource class. No job IDs,
titles, users or paths as Prometheus labels. Publish queue count/oldest age,
claim latency, completion duration, charged retries, yields, takeover count,
fenced-publication rejections, reserved slots and worker-poll writes.

**How to read it:** queued work plus idle compatible workers suggests dispatch,
mount or resource-accounting trouble. Rising age with all lanes occupied is
capacity pressure. A fenced rejection following takeover is expected recovery;
repeated rejection without takeover is a defect. A zero-length queue with
nonzero idle claim-write rate is a replication-cost regression.

## 9. Extensions — concrete additions after the queue release

These reuse the queue rather than each inventing a scheduler. Implement E0,
then E1–E3 if executing the whole programme. Each has one bounded exit.
Existing related plans supply domain detail; this document's no-certification
policy supersedes their rollout flags, not their correctness requirements.

### 9.1 E0: new preparation and maintenance adapters

This is the immediate follow-on, not part of the queue release. Add the E0
registry entries, provider/storage-domain budgets in §5.2, and the narrow
learner artifact-publication permission in §3.2. Do not change catalogue
identity or the live subtitle-window lifecycle while adding adapters.

**Subtitle preparation.**

Add typed `subtitle_extract` jobs keyed by source generation, selected track,
normalization options and extractor version. Publish bounded immutable sidecars
with existing subtitle size limits. Reuse verified peer fetching and retain
per-request authorization. Two viewers selecting the same track join one job;
different tracks are different jobs.

Keep the existing immediate local path available when remote preparation cannot
meet a playback deadline. Both paths must claim/join the same artifact build
identity; a “fallback” must not launch a second unfenced extractor. Windowed
playback subtitle work and burn-in stay under their current session lifecycle;
do not convert every seek/window into a durable job.

**Library scans and metadata refresh.**

Represent an accepted per-library scan/refresh as durable intent. Queue dispatch
chooses an eligible voter with source access; the worker obtains the existing
`scan:library:<id>` publication lease before starting. All full, targeted,
startup and manual entry points must converge on this dispatch path or join an
existing request. An older direct path must not bypass it and start a second
scan. Keep targeted-path coalescing and identity hints intact.

If the required library publication lease is held, this is uncharged contention.
Release the queue claim and all local/shared reservations, and requeue with a
bounded delay; do not occupy an encoder/I/O slot waiting for the domain lease.
The same rule applies to any existing singleton producer lease.

Distribute **different libraries** first. Keep each library's walker, matching,
prune decision and completion generation under one coordinator. A restarted
scan may rerun from the beginning under the existing contract; never resume a
partial file census and treat it as a complete deletion list. E0 does not shard
catalogue identity mutation. E2 can distribute pure leaf probes and return
versioned results to this coordinator.


**Acceptance:** equivalent subtitle requests share one extractor; cancelling
one waiter preserves another; different libraries run on different voters;
competing intents for one library yield without charging failures; providers
respect the global rate budget. Learner artifact/cache publication and matching
waiter completion work, while catalogue/scan/provider/schema mutations are
refused. This follow-on ends with these cases and the ordinary delivery checks.

### 9.2 E1: local reads, artifact placement and likely-next preparation

**Build:** finish the route coverage in
[BOUNDED-REPLICA-READS-ROLLOUT.md](BOUNDED-REPLICA-READS-ROLLOUT.md), using the
existing term/lag/watermark proof per read and authority fallback. Make it the
normal catalogue path once implemented. Preserve strong auth and user
read-after-write behavior. Move any remaining opt-out to Developer; no receipt
is consulted by request handling.

Add `artifact_hydrate`, `artifact_verify` and `artwork_variant` jobs. Reuse
existing manifests and holder APIs; share a transfer helper rather than
rewriting every artifact store. Select one producer, then copy to a second
healthy holder for currently demanded/hot artifacts when space permits. Keep
cold artifacts on one holder. Do not count a stale row as a usable copy, or
replicate every artifact onto every disk.

Start with a 24-hour, bounded top-128 demand set and a maximum of 64 live
predictive intents. Next-up predicts at most one episode per active viewer;
library channels predict the next scheduled item. Preparation means indexes
and selected subtitle sidecars first; speculative full encoding continues to
obey its existing preference and byte budget. A prediction expires or is
cancelled when its source/demand changes. Completed useful artifacts may stay
under ordinary cache policy.

Artwork jobs use `(source_digest, width, format, pipeline_version)`, generate
only the supported sizes, and preserve original hero/backdrop bytes. Follow
[IMAGE-SERVING-AND-DERIVATIVES.md](../server/IMAGE-SERVING-AND-DERIVATIVES.md)
for digest identity and serving semantics. Missing derivatives use the current
image path while one bounded job prepares them.

**Acceptance:** stale replicas fall back correctly; a source/recipe requested
through two nodes is built once and both obtain verified bytes; popular output
survives one holder loss; cold output is not copied to every node; prediction
cancellation stops future work; cache pressure evicts unpinned output only.
Measure Home/item p95 and start/seek latency against the same warmed/cold
fixtures. No invented percentage improvement is a requirement for execution.

### 9.3 E2: embeddings, leaf probes and repair batches

**Build:** `semantic_embed` uses `(item content digest, model digest, tokenizer
version, dimensions, normalization version)` and publishes portable bounded
vectors. Each node builds/updates its local search index from those verified
artifacts. Model changes use a new generation; queries use one internally
consistent model/index generation, retaining the prior one until the new local
index is usable. This does not enable semantic search when the user disabled it.

Add pure `source_probe` units for scan coordinators that can consume them
without changing identity matching. Coordinator generation and file snapshot
travel with every result. The coordinator alone decides catalogue mutations
and scan completion. Limit outstanding work to 128 files per coordinator;
do not shard filesystem discovery/deletion semantics in this extension.

Use node-targeted `artifact_verify` pages for stored bytes and deduplicated
`artifact_repair` jobs when verification fails. A verified peer supplies repair
where possible; otherwise the original typed producer rebuilds. Keep existing
reader pins, quarantine and shared-cache GC ownership. Scope verification to
rebuildable artifacts; this does not hash or repair every operator media file.

**Acceptance:** unchanged items do not get re-embedded after another node joins;
wrong-model vectors are rejected; replacing a file while a probe runs cannot
change the new catalogue generation; one corrupt copy is repaired from a valid
holder without evicting active-reader bytes; work yields under playback demand.

### 9.4 E3: media placement and shared Live TV ingest

**Build:** consume real resource observations from §5 in current media offers.
Keep selection bounded and resource reservation on the chosen worker atomic.
Missing measurements affect ranking only. Remove request admission based solely
on absent historical speed proof in the touched path; respect actual codec,
memory, scratch and concurrency limits, and report observed execution failure
honestly. Busy/dead/incompatible nodes are individual candidates, not reasons
to turn off the whole pool. Keep existing sessions on their owner unless their
existing recovery/handoff protocol changes ownership.

Implement the shared-ingest portion of
[LIVE-TV-SHARED-TRANSPORT.md](../features/LIVE-TV-SHARED-TRANSPORT.md): one owner
per `(device, channel, configuration_generation)`, separate bounded queues for
viewer and recording consumers, and exact last-consumer cleanup. Other nodes
can perform compatible viewer processing from that ingest using bounded peer
streaming. One slow peer must be dropped independently; it cannot stall the
tuner reader, DVR sink or other viewers. Transport occupancy, not viewer count,
accounts for tuner slots; per-viewer authorization and session cleanup remain.

**Acceptance:** a saturated ingress places a new session on a free compatible
worker; one unrelated stale peer does not prevent it; racing starts cannot
oversubscribe local resources; three viewers of one channel consume one tuner
ingest; stopping one viewer leaves the others and recordings running; a slow
remote consumer cannot block the shared source.

**Exit boundary:** shared encoding for identical Live TV viewers and direct
client-to-worker redirects are subsequent optimizations, not E3 prerequisites.
The former needs shared producer/session semantics; the latter needs reachable
public origins, capability URLs and client transport handling. The first E3
delivery keeps the existing proxy contract and states that ingress egress
bandwidth is therefore still consumed.

## 10. Implementation and evidence — prove the queue once, extend the corpus

### 10.1 Milestone ownership and completion

| Milestone | Principal ownership | Required result |
|---|---|---|
| M1 | New core queue module/backends, domain types, schema/import fixtures, daemon worker, pre-transcode paths in `state.rs`, admission integration | One real typed job runs end to end; both backends pass ownership/publication/retry contracts; old pre-transcode execution is removed |
| M2 | Fragment Store/worker paths, domain publication and hydration adapters | Legacy analysis semantics retained; shared build/hydration jobs work; old fragment execution removed |
| M3 | Job API, existing Developer/Activity scripts, metrics, migration drill and cluster-check scenarios, evidence wrapper only if still needed, maintained docs | Bounded observable queue; advisory requirements; final migration/recovery evidence on current tree |

Files overlap, so task branches start from the latest effort and integrate
serially. Do not invoke the disjoint-file/main-branch exception. Keep unrelated
changes in the shared checkout out of the implementation branches.

### 10.2 Mandatory behavioral corpus

Use one named `background_jobs` test family for the new common contracts.
Run it on SQLite with independent connections and Hiqlite with independent
clients, plus separate-process worker drills. Domain tests remain valuable;
adapting ownership must not delete the tests that protect output correctness.

| Case | Required observation |
|---|---|
| Concurrent duplicate enqueue | One active job, multiple correct waiters, no duplicate child |
| Concurrent claim | One current owner; unrelated jobs run on different workers |
| Lost claim/completion reply | Same request resolves to committed outcome; no extra execution |
| Lost renewal reply / priority update | Exact claim resolves its current revision before the confirmed deadline; cancellation/takeover never grants execution authority |
| Pause past lease, takeover, resume | Higher fence succeeds; every stale publish/mutation is rejected |
| Worker restart with same node ID | New boot ID cannot accidentally inherit an old live claim |
| Death before/after output rename/commit | Staging/orphan/committed result each reconciles without bad pointers |
| Cancellation racing completion/renewal | One transactional verdict; no resurrection or leaked permits |
| One of two waiters cancels | Other waiter still gets the result |
| New demand during cancelling | Typed nonacceptance and retry; no doomed waiter or revived job |
| More than 16 preemptions | Resolved history compacts; the same healthy job finishes with monotone fences and intact failure counts |
| Receipt retention pressure | Seven-day request identity stays stable; admission refuses instead of silently expiring it |
| Build complete, target absent | Build succeeds while target request remains pending; verified hydration alone completes that request |
| Quorum loss and restore | Publication stops; children settle; queue resumes without split ownership |
| Source replacement and recipe/model change | Old output never becomes current for the new identity |
| Local mount loss / unsupported worker | Compatible peer remains eligible; retries are not exhausted |
| Live request during background encode | Encoder capacity returned within the existing five-second budget |
| Source-I/O contention | Core shared budget holds across healthy nodes; expiry releases reservation; physical overlap on loss is measured |
| Provider/domain contention (E0) | Global provider policy holds; busy singleton lease yields resources without charging a failure |
| Busy first candidate / first page incompatible | Another worker/job proceeds within bounded selection |
| Learner execution (E0) | Narrow immutable publication and waiter completion work; catalogue/scan/provider/schema mutation is refused |
| Queue and history pressure | Limits hold; foreground headroom survives; active pins/claims are retained |
| Old queue migration and rerun | Canonical dedupe, per-target obligations, retry history and valid checkpoint selection survive; >4,096 legacy requests drain without loss |
| Unknown readiness | Core job observations remain advisory; missing metrics do not forbid otherwise valid execution |
| Stale third media peer (E3) | Settings save; compatible workers continue; per-peer failures are explicit |

Use deterministic clocks/barriers for transition races and actual child
processes for teardown. The cluster drill must run production claim/renewal
code, not a parallel test-only lease implementation. Include a retained fixture
with no work to show idle workers do not generate recurring queue writes.

### 10.3 Commands and what they prove

Before changing Rust, establish the pinned compiler loop from
[AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md). Verify Rust 1.97.1 on the
actual compiler host. Use source-only `git archive` when compiling elsewhere,
keep target artifacts warm, and rerun on the exact intended branch after a
base change. A source-only documentation task does not need this loop.

Existing commands:

```bash
cargo fmt --all --check
cargo check --locked -p plurxd --all-targets
cargo clippy --locked -p plurxd --all-targets -- -D warnings
make unit-core
python3 -m unittest discover -s tests/operations -p test_docs_index.py
node tests/web/settings-sections.test.js
git diff --check
```

Planned test filters, to be created with M1/M3 and recorded with actual nonzero
counts; these commands are **not evidence until the named tests exist**:

```bash
cargo test --locked -p plurx-core --features hiqlite-store --lib background_jobs
cargo test --locked -p plurxd --bin plurxd background_jobs
cargo test --locked -p plurx-core --features hiqlite-contract-tests \
  --test store_contract background_jobs -- --test-threads=1
cargo test --locked -p plurx-cluster-check background_jobs -- --test-threads=1
```

Keep the focused process corpus to a minutes-scale developer run. Do not make
each adapter rerun a forty-cycle transport campaign. Run applicable existing
domain regressions, and retain the full cluster/store suites in their current
manual/release workflow. Record fixture, SHA/tree, toolchain, test count,
commands and observed limitations; “command exited zero” with zero tests is
not evidence.

### 10.4 One small measurement set

At M3, run the same finite mixed workload on one and three voter workers: at least
12 independent real jobs, including index work and compatible encodes, with
one foreground playback start during background activity. Record total batch
time, queue wait p50/p95, per-node work, foreground start latency, NAS reads,
CPU/GPU occupancy and queue-attributable replicated writes. Repeat three times
with cache state documented. Report median and worst run; do not call a single
warm-cache comparison a scaling result.

Success means idle compatible nodes actually work, duplicate publications stay
zero, playback retains priority, and remaining bottlenecks are identified.
Do not assert 3× speedup from three nodes sharing one NAS. If queue overhead
outweighs parallel work, correct polling/batching before adding more adapters.
Benchmarks inform engineering; no result becomes a runtime unlock bit.

## 11. Delivery pipeline — preserve velocity and the quality bar

### 11.1 Follow the amendments, not superseded prose

The September 10/13 correction and September 20 test amendment at the top of
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) match the workflow files:

| Current behavior | Source and consequence |
|---|---|
| Ready main-bound PRs run the fast lane | [main-fast-lane.yml](../../.github/workflows/main-fast-lane.yml); drafts allocate no jobs; ready-for-review starts it |
| Rust compilation, Clippy and unit tests block main merge | Fast Rust job runs `make effort-rust-check`, vendor Clippy, `make lint`, and `make unit` |
| Effort compiler workflow is manual | [effort-ci.yml](../../.github/workflows/effort-ci.yml); no automatic task-PR compile fan-out |
| Full sweep is manual or explicit release tag | [ci.yml](../../.github/workflows/ci.yml); no automatic full run on every task push or main merge |
| Publication/deployment is separate | A merged queue change is not authorization to restart the fleet |

Do not revive the removed `fast-lane` label, periodic runtime-test schedules,
full browser/device matrices on every patch, or “use CI as the compiler”.
The development gates in [AGENTS.md](../../AGENTS.md) remain required; where
the current workflow makes them manual, dispatch deliberately rather than
rewiring triggers. Retain the required exact-tree qualification receipt for
effort promotion. Never claim that the main fast lane alone produced a full
effort receipt.

**Known documentation/workflow mismatch:** at the inspected SHA, the receipt
steps in `ci.yml` run only when `qualification=true`, but its scope sets that
value only for a pull-request event and the workflow accepts only dispatches
and tags. A plain manual sweep therefore does **not** emit the old effort
receipt. Do not leave the builder to discover this at final promotion.

M3 owns one bounded evidence-collection wrapper if the mismatch remains. It
uses a versioned mapping from **workflow job IDs**, not display names or a dump
of every run job. For the inspected workflow, the mapping is:

| Receipt key | Required workflow result |
|---|---|
| `scope`, `mobile_version`, `preflight` | Same-named job succeeds; scope selects every required evidence surface |
| `rust` | `check` succeeds (the receipt's historical key differs from the workflow ID) |
| `cluster_store` | Same-named aggregate succeeds and the implementation selected by captured execution mode succeeds |
| `cluster_topology`, `cluster_wal`, `cluster_daemon` | Same-named jobs succeed |
| `cluster_transport_recovery` | Aggregate plus contracts, voter and learner evidence all succeed on the same first run attempt |
| `web_layout`, `vod_web`, `android_jvm`, `apple`, `android_device` | Same-named jobs succeed |
| `package_smoke` | Both expected matrix children, `amd64` and `arm64`, succeed; missing or extra/unrecognized matrix shape is an error |

Additionally require `rust_compile` and `windows_compile` success; retain those
results in the receipt under their real IDs. Resolve reusable-workflow children
through run provenance, not by accepting an aggregate whose required children
are missing. The versioned mapping captures the resolved execution mode.
`legacy` and
`shadow` require `cluster_store_legacy`; `accelerated` requires all expected
`cluster_store_shards` children. `cluster_store_shadow` is explicitly
observational: do not wait for it, require its success, or promote it to the
required path. Record its state separately. Only inactive required alternatives
may use the workflow's documented neutral skipped/success result. A changed
mode/matrix/workflow without an updated mapping
fails evidence collection explicitly instead of silently losing coverage.

Skipped `pr_gate`, `publish`, `publish_main`, `coverage` and `badge` are allowed
when their documented event conditions exclude a manual dispatch; they are
not receipt evidence. Record those exclusions separately. Do not include them
as fabricated successes, and do not treat a skipped required evidence job as
an allowed incidental skip. An unexplained failure still requires investigation;
failure to publish a required evidence artifact prevents a receipt. A queued/failed observational shadow or
non-evidence badge does not delay qualification when the existing required
surface graph has succeeded. Preserve that distinction rather than requiring
the overall run to finish its deliberately nonblocking work.

Before dispatch, record PR number, head SHA, base SHA and expected candidate
commit/tree. Use a dedicated non-release evidence ref, pinned to that candidate
and never moved/reused. Dispatch the existing full sweep against that ref.
Check that the actual run SHA, checkout commit/tree and workflow revision match
that candidate; bind the captured execution mode and matrix to the same run.
Before writing the receipt, resolve current PR head/base again. A moved ref,
wrong run/workflow, missing child, changed tree or second run attempt is rejected.
A protected/fixed evidence ref is not a `v*` tag and must not trigger release.

Invoke [validation.qualification](../../validation/qualification.py) with real
run metadata and normalized evidence results; retain the raw job-ID/matrix
results, exclusions and captured PR bindings beside its output. PR identity
is an external promotion binding; do not claim the dispatch was PR-triggered.
Do not manufacture success values or loosen `REQUIRED_JOBS`. Test `check`→`rust`,
failed/missing matrix members, permitted incidental skips, inactive-mode skips,
missing selected children, stale PR refs and wrong checkout/workflow provenance.

This closes the existing evidence wiring gap without changing workflow triggers
or adding full-suite runs to ordinary pushes. If main repairs that mechanism
first, use the repaired mechanism and omit the wrapper.

### 11.2 Three planned task PRs and one promotion

1. Create `effort/cluster-work` from current main. Each `codex/cluster-work-mN`
   branch starts from the latest effort. Run pinned local compile/lint and the
   smallest meaningful regression before pushing. Commit normally with the
   tracked hook policy; do not disable hooks or rewrite CI to save time.
2. Open each milestone PR into the effort, record focused commands and their
   actual results, and obtain the required manual `Effort development gate`.
   Integrate once that current candidate is green. Do not attach full release
   testing to every milestone.
3. After M3, freeze task merges and merge current main into the effort. Open
   the main-bound promotion as draft. Obtain exactly one adversarial review
   and address its findings before spending the full qualification run.
4. Rerun focused queue/migration evidence on the settled exact tree. Obtain
   the repository's required manual effort qualification and receipt for this
   candidate, then mark ready. That starts the current fast lane, including
   blocking Rust unit tests and Clippy. No repeated review loop and no automatic
   whole-fleet testing per correction.
5. Merge only with a green current-head **Main promotion gate** and the required
   current-tree receipt. Changed head/base invalidates affected evidence; rerun
   what the repository contract requires. Release/build/deploy explicitly after
   merge, using §7 for the queue conversion.

For E0–E3, if building the extensions together, use one subsequent
`effort/cluster-capacity` with the same convention. A single independently
commissioned extension can use an ordinary main-bound draft PR and its affected
checks. Shared-file changes do not qualify for the disjoint-ownership exception.

Update [ARCHITECTURE.md](../ARCHITECTURE.md), [OPERATIONS.md](../OPERATIONS.md),
[API.md](../API.md), [FEATURES.md](../FEATURES.md) and validation ownership with
the behavior they describe, in the same changes. Preserve regression-history
requirements as implemented on the current base; the separate ledger cleanup
proposal is not authorization to omit receipts. Web asset additions follow
the shell/asset/index contract. Native-client changes are unnecessary for M1–M3.

## 12. Execution ledger and stop points

| Unit | Status | Branch/PR | Evidence |
|---|---|---|---|
| Implementation plan | review findings addressed 2026-09-25; external review pending | Working-tree documentation only | One independent adversarial review; dispositions in §13; four docs-index tests and explicit new-file link/whitespace checks |
| M1 queue + pre-transcode | not started | — | — |
| M2 fragment build + hydration | not started | — | — |
| M3 operations + qualification | not started | — | — |
| Core promotion/deployment | not started | — | — |
| E0 preparation + maintenance adapters | not started | — | — |
| E1 reads + cache preparation | not started | — | — |
| E2 embeddings + batch analysis | not started | — | — |
| E3 placement + shared ingest | not started | — | — |

**First-release done:** accepted jobs survive restart; both backends enforce
exclusive publication; compatible nodes share real work; cancellation and
foreground preemption release actual children; migrated requests/results remain
valid; Activity explains waiting; Developer requirements are advisory; required
current-tree development and promotion evidence exists. Stop and deliver that
release. Do not hold it for a general DAG engine, live-session migration, or
perfect fleet utilization.

**Programme done:** E0–E3 meet their stated acceptance and the same delivery
contract. Further tuning comes from observed bottlenecks, not from keeping this
effort alive indefinitely.


## 13. Adversarial review — findings and dispositions

One independent agent reviewed the draft on 2026-09-25 without editing it.
The reviewed input had SHA-256
`163697d84fdd98ddae659beb68e63ac761df4b48d5d250053174bc55318bc43a`.
Original line anchors below refer to that draft, not the revised line numbers.
The author addressed all seven findings and the three additional suggestions;
no second agent review was requested. This records design corrections, not
implementation/test acceptance. External review remains pending.

| ID / priority | Finding in reviewed draft | Disposition in this revision |
|---|---|---|
| A1 · P1 · lines 358–374 | Lost renewal reply has no safe revision recovery; waiter updates may stale an owner token | §4.2/§4.4 separate ownership revision from scheduling edits and define exact-claim resolution, cancellation-only cleanup and the original conservative deadline. Added renewal-loss/racing-waiter acceptance. |
| A2 · P1 · lines 223–243, 408–415 | Fresh demand can join an irrevocably cancelling job | §4.2 returns typed nonacceptance with a bounded retry hint; no waiter or new accepted request is recorded until cleanup permits a new job. Existing request-ID outcomes remain stable. |
| A3 · P1 · lines 561–564, 622–631 | Per-target fragment jobs collide when migrated to one build; combined old queues exceed new limits | §7.1 specifies canonical many-to-one mapping, per-target history/hydration obligations, deterministic checkpoint choice and a sealed finite import backlog drained in atomic pages. Added migration fan-in/overflow/restart cases. |
| A4 · P2 · lines 250–259, 434–450 | Sixteen preemptions can permanently block healthy accepted work | §4.1 distinguishes lifetime fence/failure totals from retained detail, compacts resolved yields and protects bounded live claim receipts. More than 16 yields must still complete. |
| A5 · P2 · lines 174–183, 326–340 | Learners may publish jobs but are forbidden the very domain writes publication needs | §3.2 names the exact artifact/cache/waiter write allowance and forbidden catalogue/authority mutations; E0 implements it, while the core preserves current worker eligibility. |
| A6 · P1 · lines 938–947 | Raw manual CI jobs do not match receipt keys and legitimate skips prevent qualification | §11.1 maps stable job IDs, `check`→`rust`, both package matrix members, selected execution paths, compiler prerequisites and observational exclusions; binds real run/checkout/workflow provenance to pinned candidate and current PR refs. |
| A7 · P2 · lines 21–41, 333–342, 571–599, 674–691 | Three milestone labels hide too much first-release work | Core now consolidates only the two existing durable adapters plus essential operations/migration evidence. New subtitles, scans, provider/domain budgets and learner execution move to E0; media policy/telemetry changes stay in E3. One small integration split is allowed if a milestone cannot be reviewed coherently. |
| O1 · additional · lines 587–592 | Busy singleton lease may charge failures or retain scarce queue resources | E0 explicitly yields the claim and all reservations as uncharged contention before retry. |
| O2 · additional · lines 162–166, 326–329, 561–564 | Successful build is confused with delivery to every target | §3.1 adds durable `awaiting_hydration` intent; only verified local installation settles a target-dependent waiter. |
| O3 · additional · lines 252–262 | Retention pressure can silently shorten request idempotency | §4.1 fixes the seven-day minimum, extends pending requests, separates compact domain identity from attempt history, and rejects new admission when protected receipts fill capacity. |
