# Cinema job queue — why scans stop and how to restore useful scheduling

**Status:** open · queue recovered, bounded dispatch and Activity fixes implemented on the incident effort ·
**Updated:** 2026-11-08 · **Original source baseline:** `edf6d08faf549554b9892dee582406ff79af1c97`

Companion to [the API reference](../API.md#56-activity) (existing operator
surfaces), [scan hygiene](SCAN-AND-ENRICHMENT-HYGIENE.md) (walker and provider
bounds), and [worker refinement](../cluster/WORKER-REFINEMENT-STATUS.md)
(worker lifecycle evidence). This document owns the incident diagnosis,
proposed repair, scheduling contract, queue management design, and remaining
work. It is the canonical ledger for backlog project P42.

## November 7 recovery and release status

Nine manual Movies requests shared a scan, completed at 18:19:41 UTC. TV completed at 18:19:43 UTC. Three remaining metadata refreshes had scheduled origins. Each accepted request retains a waiter on its shared job.

A completed subtitle job retained a reader. A second failure at library completion held the job-fence lock while a source-pressure check waited for it. The repair verifies the immutable claim identity in the Store. The active Dolby Vision scan finished its pass.

Refused claims create no running attempts. The incident changes identify the blocking reader in both Activity views and monitoring, flag manual library requests after five eligible minutes, and report five-minute successful and failed attempts per minute for the cluster and each worker. Ready manual requests get the next eligible source turn across job types, subject to playback and bounded maintenance service. These incident changes are live; all four workers report capacity. [Activity API](../API.md#56-activity) · [Recovery ledger](SOURCE-CUSTODY-RECOVERY.md)

November 8: same-kernel reader witnesses cover native macOS/Linux and containers.
Five macOS process regressions pass; Linux and legacy migration are pending.
Cinema recovery needs no host reboot. M1 proof and acceptance remain
in the [recovery ledger](SOURCE-CUSTODY-RECOVERY.md).

## 1. The immediate failure is an abandoned shared reader

Cinema is refusing to start work because an abandoned physical source-reader
record still occupies the generic `source_io` resource. That resource matches
every library's source-reading work. The former job has disappeared, but its
custody record remains active, with expired authority and no settlement proof.
Workers can have free CPU/GPU capacity and still be forbidden to claim work.

This is a liveness failure in recovery, not evidence that the machines need
more worker slots. The physical safety rule is intentional: a clock expiring
does not prove a process stopped reading. The missing part is a recovery path
that proves termination after a crash and settles the exact abandoned owner.

### 1.1 Read-only fleet observations establish the obstruction

On 2026-11-04, the deployed build identified itself as
`plurx-player-v0.3.2-410-gedf6d08fa`, matching the source baseline. Browser,
Prometheus, and read-only SQLite observations were collected during the same
investigation. They are distinct observations, not one atomic cluster snapshot.
Infrastructure names below use the repository's neutral naming convention.

| Observation | Evidence and interpretation |
|---|---|
| Queue at 19:21 UTC | 1,124 queued jobs: 755 DV title scans, 279 subtitle extractions, four library scans, one metadata refresh, and other preparation/repair work. Counts changed during inspection. |
| Reported capacity | Three of four nodes supplied fresh capacity observations. Repeated snapshots showed all three heavy slots free; other snapshots briefly showed occupied slots without durable assignments or children. CPU counts are reservations, not measured utilization. |
| Library work | All five executable library jobs were queued with fence `0`, no charged failures, and no retry deadline. They had never been claimed. Movies had waited about 18 hours with priority `2`; TV about 13 hours with priority `2`. |
| Long-standing backlog | Some subtitle jobs had waited over nine days; the oldest queued fragment build was about 22 days old. Age includes waiting and retries, not execution time. |
| Answering node | Uptime was about five hours. Its scan-walk and scan-probe counters were zero. This establishes no scan progress on that process, not on every peer. |
| Physical custody | Custody `516e85c7-ad3b-4c81-baf4-a7f5048eec87`, job `c6f8e2ec-a7ea-4a52-8c6c-8931fc8217ec`, fence `1`: resource `source_io`, phase `active`, `active_occupancy=1`, `settlement_proof IS NULL`. The corresponding job row was absent. |
| Expired owner | Custody began 2026-11-03 23:13:38 UTC and its recorded lease expired at 23:14:08 UTC, or 18:14:08 Eastern. The owning node's current container started 2026-11-04 14:20:30 UTC. These timestamps establish a surviving record across restart; they do not themselves attest that every old child was joined. |
| Replay material | No local settlement journal files were listed on the owning node. No join proof was present in the custody row. |
| Other proposed explanations | Only 1,009 attempts were retained, below the claim ceiling of 40,000. The five library jobs had elapsed `not_before_ms`, zero retry deadlines, and no reported local root-readiness error. |

The database and metrics were read without mutation. Private raw observations
remain in the originating investigation chat; this table retains the decisive
identities, timestamps, build, values, and limits without credentials or real
hostnames. Changing terminal counts are not a completion-rate measurement:
retention and rediscovery can change them too.

### 1.2 What is verified, inferred, and still unknown

**Verified:** the active expired custody overlaps all four library scan
resource requirements. The SQL claim predicate refuses precisely that state.
All five library jobs had never acquired an execution fence. The record has
neither a live job row nor a settlement proof, and ordinary replay cannot
settle it.

**Inferred mechanism:** a reader lost its owner before recording a completed
join, leaving this orphan. A crash or interrupted shutdown is consistent with
the state and the source gap; the original termination event was not captured.
Do not report a particular crash, signal, deploy, or host failure as proven.

**Still unknown:** the former job's kind and final outcome, whether its reader
descendants survived the original termination, why the fourth node did not
report capacity, and which additional blockers remain after safe recovery.
The stale subtitle assignment on that unavailable node is a separate symptom.
Releasing this obstruction is necessary, not proof that every queue lane will
then complete successfully.

## 2. Claim refusal and recovery leave the orphan in place

### 2.1 The claim path fails before recording an attempt

Re-verify literals and interfaces against the linked source at build time.
The decisive predicate in [background job claiming](../../crates/plurx-core/src/store/background_jobs.rs)
is:

```sql
AND NOT EXISTS (
  SELECT 1
  FROM background_job_required_resources required
  JOIN source_io_occupancy held
    ON (held.resource_key = required.resource_key
      OR (held.resource_key LIKE 'source_io%'
        AND required.resource_key LIKE 'source_io%'
        AND (held.resource_key = 'source_io'
          OR required.resource_key = 'source_io')))
  WHERE required.job_id = json_extract($1, '$.job_id')
    AND held.custody_id IS NOT NULL
    AND held.active_occupancy = 1
    AND held.expires_at_ms <= json_extract($1, '$.now_ms')
)
```

Generic `source_io` deliberately overlaps each specific source resource.
The custody-backed row remains in the [occupancy view](../../crates/plurx-core/src/store/background_jobs_foreground.sql)
after the ordinary reservation and job are gone; absent current authority is
represented as expiry `0`. This prevents a second reader from treating missing
job history as proof of an empty disk lane.

[Candidate selection](../../crates/plurx-core/src/store/background_jobs.rs)
does not apply this resource predicate. A library worker can select a
candidate, acquire local heavy/CPU admission, lose the durable claim, release
admission, and repeat. A rejected claim creates no execution attempt. That
explains free capacity, brief reservation pulses, no child process, fence `0`,
and an unhelpful “waiting for a worker” message together.

```text
queued library request
         │ candidate selected; local capacity acquired
         ▼
durable claim checks shared source ownership
         │ abandoned active custody with expired authority
         ▼
claim refused ──▶ release local capacity ──▶ poll again
         │
         └── no attempt, no scan, no useful waiting reason
```

### 2.2 A successful activation removes the only pre-read journal

The [activation helper](../../crates/plurxd/src/source_custody.rs)
`activate_background_with_journal` writes an activation intent, activates the
exact custody, then clears that journal on success before the reader runs.
Activation clears the initial no-reader settlement proof. The
[background worker](../../crates/plurxd/src/background_jobs.rs) writes the
separate settlement receipt after physical work has stopped and joined.

An owner lost between activation and the verified join can therefore leave
both `settlement_proof=NULL` and no replayable local file. The
[local replay loop](../../crates/plurxd/src/source_custody.rs) only enumerates
existing files. The recurring
[settled-custody query](../../crates/plurx-core/src/store/background_jobs_resources.rs)
only selects `WHERE settlement_proof IS NOT NULL`. Neither reconstructs proof
for an abandoned active reader.

**Decision proposed:** preserve the physical safety rule and add explicit
recovery proof. Deleting custody on timeout would exchange this incident's
failure to make progress for unaccounted simultaneous readers.

## 3. Scheduling and presentation also need correction

These source-backed deficiencies can delay work even after the orphan is
settled. They are not alternate explanations for the verified claim refusal.

| Deficiency | Existing behavior and consequence |
|---|---|
| Manual work is still background admission | [Library claiming](../../crates/plurxd/src/background_jobs.rs) calls `admit_fragment`. [Admission](../../crates/plurxd/src/transcode/manager/construct.rs) asks for the whole software budget, behind the single [heavy slot](../../crates/plurxd/src/transcode.rs), and requires no other CPU or GPU use. A higher library priority does not change admission. |
| No priority arbitration across consumers | Library, fragment, preparation, and subtitle loops compete separately for shared capacity. Some local indexing waits in the semaphore's FIFO; library admission only tries to acquire it. Candidate priority is not a shared-resource scheduling policy. |
| Targeted imports follow full scans | [Library processing](../../crates/plurxd/src/library_work.rs) executes a batch's full scans before targeted groups. One awaited library driver per node holds admission through scanning and full-library enrichment in [the scan path](../../crates/plurxd/src/state.rs). |
| Repeated yielding can conceal starvation | [Worker pressure handling](../../crates/plurxd/src/library_work/env.rs) and [the lifecycle controller](../../crates/plurx-worker-control/src/library_lifecycle.rs) can interrupt work and requeue without charging failure or retaining a scan checkpoint. Partial publications can survive while the request remains pending. This is a possible scenario, not this incident's fence-zero history. |
| Wakeup is local | Enqueue notifies only the answering process. Other nodes use [idle polling](../../crates/plurxd/src/background_jobs.rs) that can back off to roughly 30 seconds. |
| Inventory looks like execution order | [Queue listing](../../crates/plurx-core/src/store/background_jobs.rs) pages by UUID. [Activity](../../crates/plurxd/src/web/pages/activity.js) displays raw priority numbers beside that inventory, not actual dispatch order. |
| Controls and reasons are incomplete | Activity offers single-job Cancel and selected Retry support. The [HTTP API](../../crates/plurxd/src/http/background_jobs.rs) lacks run-next, maintenance pause, search, library/source filters, and general admission reasons. |
| Information exists on another surface | [Content Analysis](../../crates/plurxd/src/web/pages/analysis.js) consumes the [source-holder projection](../../crates/plurx-core/src/store/background_jobs_observation.rs), including cleanup-pending custody. The main Jobs view does not explain library blockage with that evidence. Reuse and extend the projection rather than inventing a second ownership registry. |

The [library enqueue policy](../../crates/plurx-core/src/store/background_jobs_library.rs)
assigns scheduled work `0`, startup work `1`, and manual/targeted work `2`.
Those distinctions exist. Their effect stops at the candidate-list and
resource boundaries above. Raising the number cannot bypass abandoned
custody and must not bypass physical safety.

## 4. Repair ownership recovery before increasing concurrency

### 4.1 Recover this legacy orphan with verified physical fencing

This is a proposed recovery procedure, not an executable SQL workaround.
Existing rows lack the physical identity needed for automatic crash recovery.

1. Capture the exact custody identity, immutable owner, activation, resource
   keys, authority expiry, and related request state. Preserve a rollback and
   audit receipt outside compiler/cache storage. Re-read before acting because
   another successful settlement may already have removed the obstruction.
2. Identify the owning daemon and every way it could delegate reads: daemon
   threads, probe/encoder children, process groups, and source bridges. An
   absent job row or a new daemon start time is insufficient evidence.
3. Quiesce the owner through a controlled recovery path, prevent concurrent
   reader activation while proving termination, and verify the former
   service's descendants and reads are stopped. A proven drained container or
   service cgroup, or a verified intervening OS reboot for that owner, can be
   evidence; a reused PID, peer timeout, or membership removal cannot.
4. Through the authoritative Store path, bind the proof to the exact old
   custody/activation and mark settlement before release. Do not edit one
   replica's SQLite state, resurrect the old lease, or release an unrelated
   replacement owner. The legacy recovery adapter and its authorization,
   proof format, and idempotency tests must exist before this step is run.
5. Resume normal admission and observe the already-accepted Movies/TV requests
   acquire fences, publish newly discovered media, and reach terminal results.
   Record remaining blockers rather than deleting queued work to reduce totals.

If physical termination cannot be proved, keep the record held and show an
explicit recovery-required error. There is no unrestricted “force unlock.”

### 4.2 Retain recovery identity for foreground and background readers

**Proposed record:** persist the exact custody token and activation identity,
node identity, daemon-instance identity, OS boot identity, and verified
process/container/cgroup identity before the first read can begin. Track
activation intent, active reading, stop requested, joined proof, and released
states. These are design fields, not an existing schema or API.

Apply this protocol to both foreground and background custody, including
background job, analysis, lease, and offline owner variants. The
[foreground activation helper](../../crates/plurxd/src/source_custody.rs)
also deletes its intent on successful activation, and expired foreground
custody independently refuses background claims. A background-job-only fix
does not close the recovery gap. Preserve each variant's existing ownership
and publication identity; a generic receipt must not weaken those selectors.

The record must survive successful activation until the exact reader and all
delegated reads are joined and authoritative release is acknowledged. Every
crash window must leave either proof that no reader could have started or
enough physical identity to fence an old reader. Do not merely retain the
old activation-intent file: its abort semantics assume a pre-read operation.

Recovery requires exclusive ownership of the node's daemon instance and a
platform-specific termination verifier. Establish or verify that exclusivity
at startup; this proposal does not assume the present daemon lock already
provides the required guarantee. On duplicate startup, lost authority, unknown
process identity, missing journal, corrupt evidence, or unreachable owner,
refuse release and retain a visible recovery state.

Use the existing exact-owner settlement and release operations through Store;
expiry of the queue fence must not require reviving the old job. Settlement
must be idempotent across lost acknowledgements, repeated restarts, and peer
replay. A legacy row without trusted physical identity needs §4.1 rather than
an invented startup proof. Linux, macOS, and other supported worker platforms
each need explicit evidence; a Linux cgroup test proves no other platform.

**Format and rollout boundary:** use a versioned recovery record distinct from
all existing activation and settlement receipt types. Existing replay treats
activation as abortable before reads and settlement as proof of completed
join. Never encode active-reader evidence as either one. Keep new records in
a separately versioned namespace; unsupported versions retain ownership and
surface an error. Update the recovery-aware reader before enabling creation
of the new record, and test old/new replay against each crash state.

Enable cluster arbitration only after every admitted worker advertises its
required protocol capability. During a mixed-version rollout, use the existing
policy without claiming M2 priority guarantees, or fence incompatible workers
from the contested lane. An older try-acquire consumer cannot remain a bypass.
Before downgrade, stop new activations and drain/join/release the affected
owners. If that cannot finish, retain the recovery-capable binary and journal
until proof is settled; do not start an old binary on unresolved new-format
state or promise a safe arbitrary binary rollback. Migration and legacy-row
recovery remain independently testable and idempotent.

### 4.3 Give every refusal an observable reason

Extend claim/admission observation to distinguish candidate eligibility,
physical capacity, shared source ownership, authority, destination readiness,
disabled work, retry delay, and queue/attempt limits. A failed compare-and-swap
is a benign race; repeated refusal for the same expired physical owner is an
operator fault. Report these differently.

Use bounded, timestamped observations. Include the blocker work ID, node,
resource, expiry, proof status, affected scope, and last verified progress
where known. Do not expose custody tokens, claim/boot secrets, or source paths
to ordinary clients. Observation must not create one replicated write per
poll or a high-cardinality metric series per job.

## 5. Give manual requests the next safe turn

### 5.1 One policy must govern the shared resources

| Class | Examples | Proposed scheduling rule |
|---|---|---|
| Active playback | Needed stream data and viewer-dependent preparation | First claim on required resources; retain the existing protected playback capacity and source-safety checks. |
| Interactive library work | Manual Scan, explicit refresh, targeted new-download import | Highest ordinary non-playback priority; next safe turn subject to the bounded maintenance fairness turn below. Full-library work remains chunked. |
| Maintenance | Scheduled reconciliation, speculative indexes, DV title scans, subtitle backfills, cache preparation | Use remaining capacity, plus the bounded fairness turn below, across libraries/types; yield at verified boundaries. |

Classify work from authenticated consumer intent, not a caller-supplied
priority number. Preserve every accepted waiter when deduplicating work;
promote the effective class while an interactive waiter remains, and remove
that promotion when its interest ends. A manual request joining a scheduled
scan must inherit prompt service without creating a duplicate library owner.

Arbitrate at each contested resource, not only inside each kind's list. A
central in-memory dispatch loop alone cannot guarantee cluster-wide priority:
the Store grant must atomically choose or honor a bounded ordered reservation
among eligible contenders, with owner expiry and lost-ack reconciliation.
Every consumer of the shared heavy/source lanes must participate, including
the existing FIFO index path. Do not mix an ordered dispatcher with bypassing
try-acquire consumers.

**Fairness policy proposed for M2:** after at most eight consecutive
interactive turns on an overlapping resource, grant one eligible maintenance
chunk if maintenance has waited at least 60 seconds. With no eligible
maintenance, continue interactive service. Count turns in authoritative shared
state, survive leader change, and reset the burst only after a maintenance
turn or the competing class drains. Selection within a class rotates eligible
libraries/consumers; a repeated full scan cannot monopolize its class.

Under continuous interactive arrivals, an eligible maintenance contender gets
a fairness opportunity within nine non-playback dispatch turns after its
60-second age threshold. With at most `K` continuously eligible maintenance
scopes and round-robin selection, each scope begins one chunk within `9*K`
such turns; the acceptance fixture uses `K=3` and a 27-turn bound. Count only
turns that begin execution, not failed grants. This is a turn bound, not a wall
clock guarantee during playback saturation, lost authority, or unsafe custody.
Interactive load exceeding service capacity must show increasing wait and
bounded ingress refusal; no scheduler can promise immediate completion of
unbounded accepted demand. Preserve accepted work when ingress is refused.

**Acquisition rule:** choose a complete eligible resource bundle before
granting its shared reservations atomically through Store. Do not retain a
local heavy/CPU permit while waiting for another resource. After the grant,
try local resources in one documented order; if unavailable, relinquish the
exact shared reservation promptly and retry without holding a partial bundle.
Use finite reservation expiry, exact-owner lost-ack reconciliation, and a
bounded hold deadline that does not release physical custody. The M2 task
must specify these deadlines and cover every consumer before implementation;
an ineligible head contender must not prevent another eligible bundle.

Keep the existing maximum of two overlapping source readers and the protected
playback share. Manual work must not turn both readers into non-playback work.
The initial change retains one non-playback source reader per overlapping
resource; improve service by priority and shorter ownership, not extra reads.
Use estimates appropriate to the actual stage rather than reserving every
software thread for a directory walk or provider request.

### 5.2 Separate discovery from optional readiness

Targeted paths go first at a library's next safe boundary. A newly arriving
targeted request must not wait for an entire full-library enrichment pass.
Keep the single library mutation owner and publication fencing; do not run
two unfenced scanners against the same library to make the UI feel faster.

Split full scans into bounded discovery/probe batches with durable cursors,
source identity, and generation-aware completion. Publish discovered media
incrementally. Release physical read admission during provider-only waits.
Enqueue enrichment, artwork, structural analysis, DV measurements, and
subtitle preparation separately, each with its own result and retry policy.

The existing targeted request result must retain its documented fields and
meaning. If discovery and enrichment need separate public completion states,
version that contract explicitly and update integration/client callers;
do not silently return an incomplete legacy result as “completed.” Missing
optional preparation must not hide a catalogued title or imply it is unplayable.

Safe yielding must retain enough checkpoint state to avoid repeating an
entire library. A fresh generation must invalidate stale cursors; completing
an old pass must not overwrite a newer request's outcome. Retain the worker
driver's stop, revoke, join, release order and corresponding conformance tests.

### 5.3 Measure dispatch latency and useful progress

These are proposed acceptance targets, not measurements of existing behavior:

| Condition | Target and interpretation |
|---|---|
| Normal notification delivery, healthy eligible worker, no earlier interactive/fairness turn, and uncontended resources | Begin a claimed discovery turn within 2 seconds of durable acceptance. This measures dispatch, not completion of a large scan. |
| Maintenance owns a needed resource | Request cooperative yield within 1 second of observing interactive demand. For cancellable stages, declare and test stop/join bounds. If physical stop remains unproved after 5 seconds, show `stop_pending`; after 30 seconds, show `recovery_required`, retaining custody. Those observation deadlines never authorize forced release. Dispatch latency is unbounded while physical termination is unproved. |
| Lost notification, receiving node busy, compatible peer free | Reconciliation discovers durable intent within 5 seconds, then grants within 2 seconds if the first row's other conditions hold: at most 7 seconds in the lost-wakeup case. Preserve intent across repeated notification loss; show actual waiting under resource contention. |
| Unsafe or incompatible worker | Show the observed blocker within 5 seconds of observation, with its timestamp. Never label unknown capacity as idle or a lease heartbeat as useful progress. |
| A request repeatedly yields without progress | Surface a starvation diagnosis with last progress and accumulated waiting; preserve accepted work and bound automatic retry frequency. |

Do not promise an ETA for probes or full-file analysis without measured
progress. Count arrivals and successful completions separately from cancels,
failures, yields, deduped interests, and retained-history eviction. This is how
the operator can tell useful work from queue churn.

## 6. Make Activity a queue you can understand and manage

### 6.1 Put active and interactive work ahead of the inventory

Use one Activity job model with these views: **Running now · My/manual
requests · Ready · Blocked · Retry later · Needs attention · History**.
Keep authorization on the server; “my” filters requests by authenticated
ownership, while administrator manual-work filters can include integrations.
Group a logical scan and its dependent stages without counting every waiter
as another executable job.

```text
Jobs   1 running | 5 interactive waiting | 1 recovery required
       completed / minute | arriving / minute | oldest eligible wait

Needs attention
  Shared media reads blocked by abandoned reader
  media1 · authority expired 2026-11-03 23:14 UTC · proof missing
  affects library discovery and other overlapping source work
  [Inspect recovery evidence]             [Open recovery procedure]

Manual requests
  Movies scan · requested 18h ago · blocked by shared reader
  [Inspect] [Run next] [Cancel request]
```

This wireframe is illustrative, combining different views of the incident;
it is not a synchronized live snapshot or an implemented screen.

For each row show the request source/class, library/title, requested age,
actual running elapsed, last useful progress, current worker, retry time,
blocking resource/owner, and available actions. Distinguish **ready but
waiting**, **blocked**, **delayed retry**, **recovering**, and **capacity
unknown**. An expired lease is not a live worker.

Offer actual dispatch-policy order and oldest-wait order with stable, bounded
pagination. Policy order is conditional on kind, scope fairness, compatible
workers, and resource availability: explain those limits rather than showing
a fictitious globally exact queue position. Carry the observation time and
filter/sort identity in opaque cursors; do not locally sort one UUID page and
claim it represents the whole queue.

### 6.2 Give controls precise semantics

| Control | Contract |
|---|---|
| Run next | Promote the selected accepted interest to interactive scheduling. Never bypass active playback, source custody, authority, or destination checks; show the remaining blocker. |
| Pause maintenance / Resume | Persist a scoped scheduling pause. Stop new maintenance admissions and cooperatively yield active maintenance at safe boundaries. Retain interactive/playback service and all accepted work; show a non-instant pause transition while joining. |
| Cancel request | Cancel the selected caller's interest. Shared execution continues while other live consumers need it. An administrator's explicit whole-job cancellation must show its broader scope. |
| Retry | Revalidate current generation, payload support, resource/config readiness, and authority; preserve prior failure history. Unsupported retry explains why and offers the appropriate domain action. |
| Bulk Cancel / Retry | Preview the exact bounded selection, use item revisions to detect changes, and return per-item outcomes. Never silently include matching work added after the selection. |
| Search and filters | Server-side title, library, kind, source, class, worker, state, and age; searches and pagination remain bounded. |

Do not add a new unrestricted administrative unlock endpoint. Recovery is
an audited exact-owner operation with physical proof, separate from retry or
priority controls. A paused background queue must not disable the recovery
loop or expire accepted requests merely because the operator paused dispatch.

After Scan, say **Queued**, link to the accepted request, and transition to
**Running**, then **New media available** when discovery actually publishes.
Show optional preparation separately. “Scan started” is not valid merely
because enqueue returned success.

### 6.3 Extend existing observation instead of replacing ownership

Reuse worker freshness fences, attempt history, source-holder projection,
and existing `oldest_age_ms` counts. Add the missing request provenance,
blocked-reason projection, stage results, dispatch ordering, and management
operations to the existing authenticated API. Specify API changes in
[the reference](../API.md) when implemented, not as fictitious existing routes.

The global source blocker belongs on Activity's default view and affected job
rows, even when the owner job has been pruned. Missing legacy source details
must remain “unknown,” not “no blocker.” Preserve older-peer handling and
the bounded, independently refreshed durable queue read so a slow peer does
not freeze the live overview. Preserve keyboard focus and dialogs during
polling, and test mixed-version, partial-cluster, and non-admin responses.

## 7. Build and accept the repair in separate milestones

**Execution:** Codex owns the initial independent main-bound incident recovery
fix on `codex/source-custody-recovery`; see [the task ledger](SOURCE-CUSTODY-RECOVERY.md)
for its narrower proof boundary. Remaining containment and M2–M4 use
`effort/cinema-job-queue`; reviewed task branches target that effort.
Shared store, scanner, worker, and UI files make the independent
file-ownership exception inappropriate. Follow
[the development pipeline](../DEVELOPMENT_PIPELINE.md), including the pinned
compiler loop before Rust edits, focused regressions, effort gates, and exact
candidate main qualification. Corrective implementation subjects use `fix(`
or `perf(` and carry real `Regression-Test:` entries.

**November 7 incident release.** Qualify the Activity rate and overdue-request
display, cross-type scan priority and library completion deadlock together on
the current effort candidate. Use the ordinary effort gate, main promotion
receipt and rolling release. Completion requires observing the installed
Activity page and a real manual scan handoff. Queue management, scan chunking
and discovery/enrichment separation remain subsequent P42 work.

The observed TV scan committed success while its pressure controller waited
on the same mutex as the producer whose completion it needed to poll. This
left a physical reader after the job disappeared from running work. The
repair validates the physical owner in the Store and allows the producer to
finish polling its pending write. A worker with verified library paths and
available capacity advertises a manual request for seven seconds and
refreshes it every three seconds. A busy worker can reserve the next turn
for four seconds while it holds an overlapping ordinary source lease.
When a source-free job occupies its heavy lane, peers may use the disk
while the manual scan keeps its place on that worker. Existing
physical readers retain their custody until joined or recovered by the
documented same-host reboot procedure.

| Milestone | Scope and acceptance before moving on |
|---|---|
| M1 · Recovery and visibility | Add exact-owner legacy recovery, versioned durable identity for foreground and every background owner variant, and an Activity blocker projection. Real-process crash/restart cases prove exact settlement for both custody families; surviving or unproven descendants retain custody. Test old/new replay, unknown versions, duplicate startups, and downgrade refusal/drain. Fleet recovery remains separately recorded. |
| M2 · Interactive discovery | Implement complete-bundle arbitration and §5.1 fairness across every consumer; chunk full scans and separate discovery/enrichment. Specify and test reservation/stop bounds before building. Normal and lost-wakeup dispatch meet §5.3 under its stated conditions; three eligible maintenance scopes progress within 27 dispatch turns under continuous interactive arrivals. New media is visible before optional readiness completes. |
| M3 · Queue controls | Implement §6 filtering, sorting, logical requests, Run next, maintenance pause/resume, and scoped bulk actions. Verify unchanged shared consumers survive request cancellation and mixed/partial/stale observations remain honest. |
| M4 · Qualification and rollout | Qualify the integrated current candidate, test mixed-version capability fencing and downgrade/drain, roll out with retained recovery evidence, and observe arrivals/completions plus repeated fresh imports. A lower queue count alone is not acceptance; preserve a recovery-capable binary while new-format custody is unresolved. |

**Required regression cases, to be implemented and named in task PRs:**

- Crash windows before activation, after activation but before first read,
  during daemon/child reads, after physical join, and after release with a
  lost acknowledgement. Recovery is idempotent; unrelated current custody
  never clears; corrupt/missing identity and live descendants fail closed.
  Cover foreground plus job, analysis, lease, and offline background owners;
  unknown journal versions and old/new replay cannot forge a join proof.
- An orphan whose job/attempt history has been pruned still appears in
  blocker observation. Recovery does not rely on history retention.
- The global resource and specific source resources overlap correctly;
  reader limits and protected playback capacity hold under concurrent claims.
- A busy receiving node and an idle peer honor interactive intent across
  lost wakeups, leader change, pause, duplicate enqueue, and changed generation.
  Test reverse resource demand, failed local admission, expired grants, and an
  ineligible first contender without partial-bundle deadlock or head blocking.
- A targeted request arriving during a full scan runs at the next safe
  boundary; source and library publication remain fenced through stop/join.
  Uninterruptible work changes to stop-pending/recovery-required on time,
  retaining custody rather than pretending a dispatch deadline can be met.
- A manual waiter promotes shared work; cancelling that waiter preserves other
  consumers and returns the effective class to its remaining demand.
- Queue sort/pagination, blocker age/freshness, null progress, inaccessible
  paths, partial peers, keyboard controls, and conflicting bulk actions have
  meaningful API/UI coverage. Compare arrivals/completions independently of
  terminal retention and rediscovery.

Run focused Store regressions with `hiqlite-store` or `make unit-core`, not
an unfeatured core test run. Exercise the production driver and real process
boundary, not only a pure controller model. Record actual commands and
receipts in this ledger as milestones are built; the cases above are planned,
not tests claimed to exist or pass.

## 8. Non-goals and rejected shortcuts

1. **No timeout-based custody deletion.** Expiry diagnoses lost authority;
   verified physical stop is what permits release.
2. **No bigger worker limit as the first fix.** Extra CPU slots cannot bypass
   this global source obstruction and could overload shared storage.
3. **No direct replica database surgery.** Repair must use the authoritative
   Store protocol with exact identity, proof, and repeatable acknowledgement.
4. **No queue purge to manufacture progress.** Accepted work and consumer
   identities survive recovery; history retention is not useful completion.
5. **No toggle for crash recovery correctness.** This is automatic internal
   infrastructure. Maintenance pause is a meaningful operator control, not a
   Developer feature gate for an unfinished fix.
6. **No permanent starvation of maintenance.** Interactive demand gets prompt
   service, while bounded turns, aging/fairness, and progress diagnostics must
   keep accepted maintenance from disappearing behind continuous new requests.

## 9. Review and evidence ledger

| Date | State | Evidence / next action |
|---|---|---|
| 2026-11-04 | Root cause verified; proposal drafted | Read-only deployed-source, browser, metrics, and database observations in §1. No production mutations or implementation changes. |
| 2026-11-04 | Astra first review completed; findings addressed | Astra independently verified source predicates, occupancy/replay behavior, foreground's equivalent gap, and index/backlog entries. Fleet facts were supplied by the investigation; Astra made no fleet calls and ran no behavioral tests. Findings and dispositions follow. |
| 2026-11-04 | Astra re-review: no remaining blockers at proposal level | Astra re-read the revised contracts and acceptance ledger, closing all four findings and the acquisition follow-up. This is review of the proposed fix, not implementation readiness, production recovery authorization, or runtime acceptance. |

| Finding | Disposition in this revision |
|---|---|
| P1 · Foreground recovery missing | §4.2 and M1 explicitly cover both custody families and every background owner variant, with real-process cases and unchanged identity selectors. |
| P1 · Upgrade/rollback protocol unspecified | §4.2 requires distinct versioned records, fail-closed replay, capability fencing, and drain before downgrade. M1/M4 and regressions test those boundaries. |
| P2 · Strict interactive priority contradicts maintenance liveness | §5.1 specifies eight interactive turns, one aged maintenance turn, authoritative accounting, scope rotation, and a 27-turn acceptance bound for three maintenance scopes. |
| P2 · Dispatch and stop bounds contradict assumptions | §5.3 separates two-second normal delivery from seven-second lost-wakeup recovery and makes unproved stop an observable retained-custody fault, not a forced-release deadline. |
| Follow-up · Partial resource acquisition can deadlock | §5.1 requires all-or-none shared grants, no local permit held while waiting, and bounded exact relinquishment. M2 must specify deadlines and test reversed demand/ineligible contenders before implementation. |

**Review limits retained:** the eight-to-one fairness policy and 60-second
threshold are proposed product choices, not measured tuning. Before building,
M1 must specify physical identity, descendant containment, daemon exclusivity,
and exact-owner proof authorization on each supported platform; M2 must fix
reservation deadlines and prove all consumers participate. Real-process and
runtime acceptance evidence in §7 remains outstanding. Astra checked relevant
source and the revised document, not the live fleet or behavioral tests.
