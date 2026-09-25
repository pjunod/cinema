# Live TV cluster resource — remove the device's permanent node owner

**Status:** implementation in progress; adversarial design review complete ·
**Written:** 2026-09-25 · **Source inspected:** `bafeb08766ce057634f3fab0850cdd9e03507a98`
plus the working tree · **Requested by:** Paul

This document explains why an independent network tuner became tied to one
plurx node and specifies the replacement, including recording safety. Read
[the original tuner plan](HDHOMERUN-LIVE-TV-PLAN.md) for the existing protocol
and [the DVR implementation](LIVE-TV-DVR-IMPLEMENTATION.md) for recording
semantics. The implementation is tracked on the
[status page](LIVE-TV-CLUSTER-RESOURCE-STATUS.md). The delivery instructions
now call for one batched main-bound PR, one adversarial code review when ready,
and one final unit/fast-lane run after review fixes.

The [adversarial review](LIVE-TV-CLUSTER-RESOURCE-REVIEW.md) records independent
findings and their disposition. File anchors below describe the inspected
source; re-verify symbols against the implementation base before editing.

## Implementation amendments — 2026-09-25

These decisions supersede the corresponding proposed mechanics below. The
original problem analysis and review remain as the design history.

- **Current base shares transports.** Main `8ae8cab1e` already allows viewers
  and recordings to share a channel. The implementation preserves that and
  serializes pending consumer attachment, final detach and socket closure in
  the cluster ledger. A viewer does not necessarily consume another tuner.
- **One indexed record store, one CAS revision.** The typed state machine is in
  [live_tv_resource.rs](../../crates/plurx-core/src/live_tv_resource.rs), with
  SQLite v70 and replicated v48 adapters. Individually indexed records carry
  starts, ingests, capture/finalization claims, and compact legacy retire
  barriers. Only changed records enter transactions. Terminal-history counts
  do not require downloading response history on every renewal. DVR row
  changes invalidate the shared revision through database triggers.
- **No feature-mode switch or physical-fencing gate.** The existing Live TV
  enable setting is exposed in Developer settings on web, Apple and Android.
  Prerequisite observations are advisory. Authentication, committed serving
  authority, per-attempt assignment, generation checks and expired-lease
  rejection still govern actual operations. Legacy owner fields remain wire
  compatibility fields; they do not place work.
- **Protocol 4 intents.** The server persists a `v4_` intent before returning
  its ID. All first-party clients preserve its exact playback envelope on
  replay. Legacy 32-hex IDs retain the bounded 24-hour retire contract. Unknown
  protocol 4 IDs cannot use legacy admission after history collection.
- **DVR publication.** Capture admission and the scheduled-to-recording row
  change commit before local tuner or file work. A stable storage marker
  distinguishes recording namespaces. Capture attempts are append-only;
  finalization copies bounded prefixes into exclusive `.f<epoch>.ts` outputs.
  A fenced manifest and recording-row update commit together. Scanners reject
  uncommitted epoch outputs and reuse the recording's existing item identity.
  Stop retains useful output; Delete prevents publication. Finalization does
  not require a tuner slot or the capture worker to survive.
- **Guide copies.** A separate source-refresh lease limits fetches, while
  serving nodes retain sanitized guide copies on disk and can relay them.
  Guide role changes do not change tuner ingest placement.
- **Delivery evidence.** Compiler and lint checks run during implementation.
  Unit execution is deferred until the complete PR has its adversarial review
  and corrections, as explicitly requested. Status records evidence that
  actually exists; hardware or predecessor-binary acceptance is not implied
  by a compiler result.

## 1. The failure — a reachable device becomes unavailable with one server

Today an administrator supplies a tuner IP and `live_tv.owner_node_id`.
Every ingress forwards Live TV work to that node. If that node fails, another
node that can reach the same tuner cannot start a channel. Moving ownership
can require disabling Live TV and confirming that the old process has been
physically stopped. This makes the server, rather than the network device's
actual availability, the limiting factor.

The original rationale was to prevent four nodes each allocating four local
permits against four physical tuners. Local counters do need coordination if
we promise a cluster-wide policy limit. They do not justify assigning the
entire device permanently to a process.

SiliconDust's [HTTP development guide](https://www.silicondust.com/hdhomerun/hdhomerun_http_development.pdf),
§II, says an HTTP request allocates a tuner, TCP closure or a specified
stream duration releases it, and unavailable requests receive HTTP 503.
The same status also covers tuning and authorization failures. The device
already arbitrates physical capacity. Do not interpret every 503 as proof
that all tuners are busy, and do not claim a database lease can forcibly close
a socket on that device.

**Desired example:** A and B both reach the HDHomeRun. A serves channel 2.1;
B serves channel 5.1. Losing A affects A's session. B can start another
channel when hardware capacity and plurx's policy allow it, with no owner
selection, global disable, or physical-fencing checkbox.

### 1.1 The owner restriction is spread across the control and storage paths

| Responsibility today | Evidence in inspected source | Why deleting one check fails |
|---|---|---|
| Device configuration and admission | [LiveTvConfig and LiveTvManager](../../crates/plurxd/src/live_tv.rs): `validate_static`, `local_snapshot`, `validate_start_config`, `start_local_inner` | A non-owner is refused even for a discovery snapshot; `max_sessions` is counted in a process-local registry. |
| Public start and recovery routing | [HTTP Live TV](../../crates/plurxd/src/http/live_tv.rs): `start_session`, `owner_snapshot`, `owner_start_within`, `retire_start`, `resume_start`, `start_state` | Request recovery routes through the configured owner before a capability is known. Choosing a different node on replay can duplicate a start. |
| Internal authentication and session routing | [Internal Live TV](../../crates/plurxd/src/http/internal_live_tv.rs) and the manager's `LiveTvRequestKey` | Signed peer identity and serving generation remain necessary. An ingress-local request key is insufficient for cluster-wide deduplication. |
| DVR observations, reminders and indexing | [DVR HTTP](../../crates/plurxd/src/http/dvr.rs), engine `reminder_loop` / `sweep_recordings_library`, [recording scan](../../crates/plurx-core/src/scan/recordings.rs) | Activity filters to the configured owner; reminders are owner gated; new artifact paths must preserve recording item identity. |
| Guide fetching and durable cache | Manager `refresh_guide` paths and HTTP `owner_guide`, `guide_readiness` | Every reader currently relies on the owner's cache and outbound network access. |
| Recording scheduling and file lifecycle | [DVR engine](../../crates/plurxd/src/live_tv/dvr.rs): `dvr_loop`, `dvr_start`, `dvr_recover`, retention and recordings-library sweep | `dvr_loop` explicitly has no job lease because the configured owner is its singleton. `dvr_start` calls `attach_sink` before the state transition. Multiple schedulers would open streams and files before one wins the row. |
| Settings and membership checks | [System HTTP](../../crates/plurxd/src/http/system.rs), [store keys](../../crates/plurx-core/src/store/mod.rs), [membership](../../crates/plurx-core/src/cluster/membership.rs) | Owner transitions, generation barriers, compatibility guards, and cluster join rules are part of the contract, not just UI copy. |

The inspected implementation gives each viewer its own tuner HTTP GET;
recordings on the same channel share a local `DvrTransport`. General viewer
sharing remains a separate [open transport plan](LIVE-TV-SHARED-TRANSPORT.md).
Do not assume that plan is already implemented.

### 1.2 Acceptance means availability and truthful limits

1. Any eligible reachable node can ingest from the configured device.
2. No administrator assigns or recovers a permanent tuner owner.
3. A capability still identifies the node hosting that particular session.
4. Concurrent requests use one cluster-wide admission policy, including DVR
   reserve; the HDHomeRun makes the final physical allocation decision.
5. Replaying one start through different ingresses does not create two
   authorized sessions. An ambiguous reply is resolved before reassignment.
6. A recording is claimed before opening a file or tuner. Stale workers
   cannot publish, finalize, delete, or overwrite a successor's output.
7. A node failure does not require unrelated sessions to stop. Existing
   quorum/serving-authority policy remains applicable during partitions.
8. Single-node installations use the same state machine through SQLite.

## 2. Decision — separate device, session, and recording authority

1. **The device belongs to the cluster configuration.** Keep one configured
   HDHomeRun in this effort. Persist its verified DeviceID and address;
   eligibility is measured per node. Multiple-device discovery is separate.
2. **Each active ingest belongs to a worker attempt.** That worker owns its
   socket and cleanup. Session capabilities route to the worker, not to a
   global device owner. Worker choice can differ between channels.
3. **Replicated transactions allocate logical capacity.** They also allocate
   start intent and recording attempts. Membership, configuration generation,
   request identity, and counters must be checked in the same mutation.
4. **The hardware owns physical capacity.** Expiry permits logical recovery;
   it does not prove that an old TCP stream stopped. A partitioned old stream
   may temporarily consume a hardware slot. Handle refusal; never retune or
   forcibly unlock another application's tuner.
5. **Small automatic singleton jobs are acceptable.** Guide refresh and DVR
   schedule reconciliation can each use an automatically acquired fenced job
   lease. Their holder need not be an ingest node and is never a manual
   device owner. Playback does not depend on the guide job's current holder.
6. **File placement is independent of tuner placement.** Storage eligibility
   determines where a recording can be written. A local-only volume limits
   recording failover, not which nodes can watch television.

| Alternative | Benefit | Reason not selected |
|---|---|---|
| Keep the manual owner | Existing simple local accounting | Retains the reported failure and operational ceremony. |
| Elect one device owner automatically | Removes the checkbox | Still funnels every ingest and encoder through one server. |
| Let every node use only local permits | Small patch | Breaks global limits, recording reserve, and request recovery. |
| Replicated per-operation claims with hardware arbitration | Different nodes can ingest safely under one policy | Selected; requires explicit recovery and DVR publication contracts. |

### 2.1 Scope boundaries

Do not add seamless live-session migration, DVR transcoding, channel scan
control, DRM support, arbitrary LAN discovery, or multi-device pooling.
A failed live session ends; the existing server-led recovery flow may create
a new session. Do not splice two HLS timelines under one capability.

Do not fold general shared viewer transports into this effort. Preserve
same-channel DVR sharing across the cluster by placing those recording
consumers together (§6). If the shared-transport work lands first, adapt
admission to count its ingest handles; never restore local capacity counters.
The two efforts overlap manager files and must be integrated serially.

## 3. Resource identity, eligibility, and configuration

### 3.1 Persist a device identity, not a worker preference

Keep existing `live_tv.enabled`, `live_tv.device_ipv4`,
`live_tv.config_generation`, `live_tv.max_sessions` and output settings.
Add `live_tv.device_id` (verified HDHomeRun DeviceID) and
`live_tv.resource_mode` (`legacy_owner` or `cluster_resource`). The latter is
an internal migration state, not a user-facing experiment toggle.

An administrator's address change probes discovery on eligible nodes and
compares DeviceID before saving. An address change for the same DeviceID
increments configuration generation. A different DeviceID is an explicit
replacement, also incrementing generation. A worker discovering a different
device at the saved address reports `device_identity_mismatch` and opens no
stream. Credentials such as DeviceAuth remain transient and redacted.

Retain `live_tv.owner_node_id` and transition keys only for migration and
legacy response compatibility (§8). Cluster mode ignores them for placement;
legacy owner writes must not silently change scheduling.

### 3.2 Eligibility is per operation and refreshed at start

A viewer worker needs committed eligible membership, current serving
authority, the new internal `live_tv_cluster_resource_v1` capability,
reachability of the exact configured device, and the local resources needed
for its negotiated output. A recording worker additionally needs a verified
storage target. A guide worker needs the configured guide source's network
path, not an encoder. Follow the existing voter/learner authority contract;
this effort does not give learners new serving or job privileges.

Each node maintains a bounded cached observation keyed by
`(node_id, process_boot_id, device_id, config_generation)`. Proposed defaults:
30 s freshness and 5 s timeout per readiness probe. Stale observations can
help order candidates but never authorize a start. A start refreshes the
lineup and checks the device identity and its own serving generation.

Placement order is: existing request assignment; existing DVR channel
assignment where applicable; eligible local ingress; remaining candidates by
reported active workload with node ID as stable tie-break. Use at most three
candidates and the existing 35 s public start budget, including probes,
reservation, peer calls and activation. An unreachable candidate cannot take
an unbounded slice of that budget. Stale load affects balance, never safety.

### 3.3 Public readiness describes the cluster

Return per-node observations with node ID, reachability, supported output,
observation age and a typed rejection reason. The aggregate is ready when
at least one eligible worker can start and the configuration is enabled.
Do not require all nodes to reach the tuner. Unknown capacity is unknown,
not zero or free. Report policy occupancy separately from device refusals.

Continue exposing a session's `owner_node_id` as its hosting worker for old
client decoders. Add fields to readiness rather than removing required legacy
fields immediately. New clients label this information "Session server".

## 4. Replicated admission and request recovery

### 4.1 New durable rows and the transaction boundary

The following are proposed interfaces, not existing APIs. Implement through
[Store](../../crates/plurx-core/src/store/mod.rs), the
[SQLite backend](../../crates/plurx-core/src/store/sqlite/mod.rs) and the
[Hiqlite backend](../../crates/plurx-core/src/store/hiqlite.rs).
Reuse the fence/revision discipline in
[cluster coordination](../../crates/plurx-core/src/cluster/coordination.rs).
Generic `acquire_lease` followed by a separate capacity check is insufficient.

| Row | Key and important columns | Purpose |
|---|---|---|
| `live_tv_resources` | DeviceID PK; config generation; admission revision | Serializes device-policy mutations and preserves an epoch across restarts. |
| `live_tv_starts` | `(user_id, request_id)` PK; immutable request digest; device/config; attempt epoch; worker node/boot; state; capability; terminal result | Makes replay independent of the ingress and its serving generation. |
| `live_tv_ingests` | ingest UUID PK; DeviceID; channel; kind `viewer`/`recording`; worker node/boot; attempt fence; state; lease revision/expiry; config generation | Counts one actual intended tuner GET, not one viewer UI or recording file. |
| `dvr_capture_claims` | recording ID PK; worker node/boot; monotone epoch; ingest ID; storage ID; lease revision/expiry; state | Authorizes a particular recording attempt before side effects. |
| `live_tv_ingest_consumers` | `(ingest_id, ingest_epoch, recording_id, capture_epoch)` PK; attach phase; stop intent | Serializes channel sharing against final detach; durable demand is independent of a local sink list. |
| `dvr_capture_artifacts` | `(recording_id, epoch, artifact_id)` PK; storage ID; relative immutable path; sealed size; state | Distinguishes incomplete files from output eligible for publication. |

Use one unique current recording-channel assignment keyed only by
`(device_id, config_generation, channel_id)`. Epoch is the monotone value of
that assignment, never an extra component of its uniqueness key; otherwise
two live epochs could coexist. A conditional index or equivalent
transactional row enforces it; a pre-read HashMap check does not. Older expired
attempt records can remain for diagnostics without blocking the new epoch.
Add checks for legal states, nonnegative counters, positive epochs, and
bounded identifiers. All state transitions return changed/not-changed and the
current durable outcome; transport timeouts return outcome unknown.

```rust
// Proposed Store contract; use owned DTOs and existing async-trait conventions.
async fn reserve_live_tv_start(&self, request: &ReserveLiveTvStart)
    -> Result<StartReservation, StoreError>;
async fn advance_live_tv_start(&self, change: &AdvanceLiveTvStart)
    -> Result<StartOutcome, StoreError>;
async fn renew_live_tv_ingests(&self, batch: &RenewLiveTvIngests)
    -> Result<Vec<IngestRenewal>, StoreError>;
async fn claim_dvr_capture(&self, request: &ClaimDvrCapture)
    -> Result<CaptureClaimOutcome, StoreError>;
async fn publish_dvr_capture(&self, request: &PublishDvrCapture)
    -> Result<bool, StoreError>;
```

DTOs carry authenticated actor identity, expected configuration generation,
expected epoch/revision where updating, worker node and boot ID, and an
operation nonce. The daemon supplies trusted fields; clients cannot pick a
worker, lease expiry, or user ID. Store implementations must enforce legal
state/epoch transitions rather than trust caller-side validation.

**Atomic reserve:** check current enabled mode, generation and committed
worker eligibility; resolve an existing request by user/id; reject changed
payload; expire eligible logical claims; enforce total and DVR reserve;
insert the start and its ingest reservation; commit before dispatch. A losing
transaction opens neither a socket nor a file. A retry after commit timeout
reads the same key consistently before attempting anything new.

**Atomic recording claim:** check due recording state, stop request,
scheduler fence, storage assignment, and generation; reserve or join the
channel ingest and acquire the recording epoch in the same transaction.
Move to an internal starting state before `attach_sink`. Preserve the public
DVR state vocabulary with a separate claim phase if needed.

### 4.2 Count ingests and preserve reservation policy

Let `L = min(live_tv.max_sessions, verified device tuner count)` and
`R = min(dvr.tuner_reserve, L)`. Preserve the current setting range `1..=4`.
Count reserved, opening, active, and draining unexpired ingest claims.
Allow a new viewer ingest only if total counted ingests < L. Allow a new
recording ingest only if total < L and recording ingests < L − R. Joining
an existing recording channel adds a sink, not a slot. Each viewer costs
one ingest on the inspected implementation base.

Do not free logical capacity merely because cancellation was requested.
Normal release follows closure of the HTTP body/reader. Encoder and scratch
cleanup can retain separate local cleanup records without retaining a closed
tuner's slot. Same-user stray eviction keeps its existing intent but targets
a durable assignment and waits for release or the expiry rules below.
External clients are outside this policy ledger and can win physical slots.

### 4.3 Leases bound logical authority, not physical occupancy

Proposed worker leases last 30 s, renew in batches every 5 s, and use a local
monotonic deadline anchored to the beginning of the successful renewal RPC,
not its completion. A response arriving after that deadline is unusable.
Retain configuration/serving generation watches independently of renewal.
Quorum loss or lease loss prevents new opens and publication and initiates
local socket cancellation without waiting for another database write.

Use the existing coordination wall-clock API with its documented trust model:
cluster node clocks should be synchronized. Epoch and CAS predicates provide
publication safety even when clocks jump; expiry alone never proves physical
exclusion. Add forward/backward clock-jump tests. Never evaluate wall-clock
functions independently on replicas during SQL application: pass one bound
timestamp as transaction input. A large skew may reduce availability or
cause duplicate physical attempts; it must not let a stale epoch publish.

After expiry a new epoch can claim logical capacity. An old suspended process
or surviving socket may still occupy hardware until it closes. Thus the
strong guarantee is **bounded authorized claims and fenced publication**, not
"all physical plurx sockets always remain below the configured cap during
partitions." The device always imposes its own physical limit. DVR's viewing
reserve is a plurx admission policy, not a reservation enforceable against
external clients or orphan sockets. Expose suspected orphan occupancy and
refusal reasons; never require global physical fencing to use other capacity.

A resumed worker checks serving/configuration/lease state before consuming or
publishing the next chunk. Late release, renewal, progress, activation and
cleanup must match node, boot, generation, epoch and expected revision. A
late release can close its own resources but cannot release a successor row.

### 4.4 A lost reply cannot choose a second worker implicitly

```text
 absent request
      │ atomic reservation
      ▼
 reserved ──▶ opening ──▶ active ──▶ draining ──▶ closed
      │          │           │           │
      └──────────┴───────────┴───────────┴──▶ failed / expired / retired
```

Persist the worker assignment before dispatch. The worker accepts one local
start per `(start key, attempt epoch, boot ID)` and commits its provisional
capability before the ingress activates it. No capability/activation token
is included in metrics or public list views. Store access retains the same
trust boundary as other session secrets.

The request digest includes channel, requested output/playback capabilities
and other semantic start options, but excludes ingress identity and transient
peer nonces. Replaying the same user/request on another ingress resolves the
same assignment. Different semantic fields return `request_conflict`.

On a definite pre-open rejection, retire that assignment and choose the next
candidate with a conditional epoch increment inside the remaining start
budget. On an ambiguous dispatch/open/activation result, query or retry the
same assignment. Do not start on B merely because A's reply timed out. Once
A's claim expires, terminalize the attempt and allow a fresh start; never
transfer an old capability to B. The public response identifies unresolved
start state so clients retain their request ID until the server resolves it.

Retire, resume, and start-state consult the cluster row and authenticate the
user. Retire atomically records terminal intent before contacting a worker;
late activation is rejected. A retired ID never resumes within its protocol's rejection lifetime (§4.5). Existing capability
resource routes continue parsing the encoded worker node ID and use signed
peer requests, independent of current placement or resource settings.

### 4.5 Admission tickets make pruning enforceable

**New client protocol 4:** add authenticated `POST /live-tv/start-intents`.
The body contains the semantic channel/output request. The server generates
a fresh unpredictable request ID; callers cannot select or reuse its ID.
Protocol-4 IDs have the exact grammar `v4_` followed by 32 lowercase hex
digits (128 random bits). Legacy IDs keep their existing 32-hex grammar.
The namespaces are disjoint and server enforced: any `v4_` ID requires the
protocol-4 path and cannot fall back to legacy admission when a ticket is
missing, invalid or expired. Persist a protocol discriminator on each row
and reject a known row whose protocol disagrees with its ID grammar. Dispatch
validation by ID shape, never just by presence of a header or ticket. Update
start, internal relay, resume, retire and start-state validators together;
unknown `v4_` IDs remain non-admissible even after detailed-record GC.
Before returning, persist an `issued` start row containing the semantic
digest, user ID, ticket expiry and a reserved terminal-record slot. Return
that ID and an opaque authenticated ticket bound to those fields and the
configuration generation. Preparing an intent opens no tuner. A retry of
this preparation can issue another unused ticket, but cannot duplicate an
active start; clients persist one received ticket for the ensuing operation.

Tickets permit the first `issued -> reserved` transition for 5 min after
issue. Only that transition checks the admission window; recovering an
already admitted active session is allowed after 5 min. On start, look up a
matching existing row first; if none exists, validate ticket expiry and
return `start_expired` without allocating. An unknown ticketed ID never
creates work: only the intent endpoint creates `issued` rows. After expiry,
unused rows become terminal. A fresh intentional Watch prepares a new ID.
Neither ingress retry nor resume may issue a replacement ticket implicitly.
Use existing cluster cryptographic key handling; no unsigned timestamps or
client-chosen expiry are accepted. Ticket key rotation must preserve bounded
verification of outstanding tickets, or explicitly expire them without opens.

Retire performs **insert-or-terminalize**, not update-if-present. For a known
row it durably records terminal intent, even if no worker was assigned; all
late dispatch and activation checks observe that state. For an unknown
protocol-4 ID, absence itself prevents admission, so authenticated retirement
can acknowledge without allocating a tombstone. A delayed intent response
still resolves to a retired issued row, never a second row with that ID.

**Legacy protocols 1–3:** preserve arbitrary client IDs for playback
compatibility, with an explicitly bounded 24 h replay/retirement guarantee.
Unknown retire inserts a tombstone keyed by `(user_id, request_id)` before
acknowledgement. An absent legacy POST after that horizon may be treated as a
new operation; indefinite rejection cannot be promised after discarding the
ID. State this in API docs and test it explicitly. Upgraded first-party
clients use protocol 4, including its preparation, cancellation and recovery
flow; do not silently downgrade them on a cluster-mode capable server.

**Capacity and GC:** bound issued/active rows to 32 per user and 4096 for the
cluster; bound retained terminal rows to 1024 per user and 65536 for the
cluster, retaining them for at least 24 h from terminalization. Reserve the
future terminal slot atomically when creating either a new issued intent or
a legacy start. Moving a known operation to retired can therefore never be
refused for lack of history space. Do not evict active rows or shorten the
retention window to make room.

An unknown legacy retire can arrive when those quotas are full. In that case
atomically advance a compact per-user `legacy_admission_blocked_until` to at
least now + 24 h and acknowledge retire. While set, all absent legacy start
POSTs for that user are refused with `start_history_full`; existing operations
remain recoverable and can be stopped. This deliberately sacrifices that
user's new legacy admissions rather than forget their stop. Other users are
not blocked. Rate-limit unknown retire/preparation independently so one user
cannot exhaust the global history budget; expired detailed rows are pruned
in bounded batches. The compact block row is bounded by existing users.

GC is not an authorization step: ticketed absent starts remain rejected,
legacy absent starts follow the documented bounded guarantee, and recovery
endpoints never create a new operation. Test both protocols across GC,
restart, ingress switching, and full quotas. Protocol 4 adds a control round
trip; include intent creation in the user-visible startup measurement and
keep the existing 35 s end-to-end start budget across both calls.

## 5. Guide and discovery survive an ingest-node failure

Device discovery and lineup snapshots are local bounded caches on every
eligible node. A start trusts only its worker's fresh snapshot. A guide
refresh job uses a fenced automatic lease keyed by device and guide config;
an eligible node with source access can take over when it expires.

Keep the existing guide bounds, XMLTV validation, credential redaction,
conditional fetch behavior, and durable cache validation. Publish a sanitized
immutable snapshot with generation, fetched-at time, content digest and
lease fence. Store its bounded snapshot through the existing cache abstraction
and replicate/serve copies to readers; publication metadata alone is not a
copy of the guide. Before accepting the refresh as replicated, obtain an
acknowledged durable copy on a second eligible node when one exists. A single
node stores its local durable copy. Do not put unbounded XMLTV into Raft.

Readers use any compatible cached copy; a failed refresh or replica transfer
keeps the previous snapshot with its true age. Joining nodes fetch and verify
the latest snapshot or report unavailable while warming. A source holder's
death cannot make all existing guide data disappear. Guide unavailability
must not prevent watching a channel in a freshly validated lineup. New guide
publication and DVR expansion require current lease/configuration fences;
old snapshots can be displayed but must not silently create new jobs from
obsolete configuration.

## 6. DVR safety must be rebuilt before enabling distributed ingest

### 6.1 Scheduler coordination does not monopolize tuner access

Run schedule reconciliation under a fenced `live-tv/dvr-scheduler` job lease.
It expands rules, ranks pending recordings with the existing pure
[scheduler](../../crates/plurxd/src/live_tv/schedule.rs), and selects eligible
recording workers. It does not open the tuner or hold all recording files.
Keep existing one-off/rule priority and non-preemption semantics.

Carry the scheduler fence into expansion, reconciliation, stop handling,
allocation and event publication. Existing `DvrTransition.fence_generation`
checks only tuner configuration; it cannot distinguish two scheduler epochs
under the same configuration. Extend it with the applicable scheduler or
capture authority and audit all transitions. Existing progress helpers that
check only recording ID/state must gain capture-epoch checks.

Workers claim each due recording before side effects (§4.1). If another
recording already holds that channel, place the new sink on that channel's
worker when it can write the required storage target. If it cannot, report a
storage-placement conflict rather than violating the one-recording-ingest-per-
channel contract. Two different channels may run on different nodes.

**Channel join versus last detach:** only `opening` or `active` ingests with
current generation, worker boot and unexpired authority accept a consumer.
The claim/join transaction inserts `live_tv_ingest_consumers` and advances
the ingest demand revision. A pending attachment counts as demand. Last
consumer removal and `ingest -> draining` occur in one transaction, testing
that no current pending/attached consumer remains. Both operations lock the
same ingest/channel row; a worker's empty local sink list cannot initiate
closure independently. In SQL terms, the decisive update is:

```sql
-- Proposed predicate, inside the same transaction as conditional detach.
UPDATE live_tv_ingests SET state = 'draining', demand_revision = demand_revision + 1
WHERE id = :ingest_id AND attempt_fence = :ingest_epoch
  AND state IN ('opening', 'active')
  AND NOT EXISTS (
    SELECT 1 FROM live_tv_ingest_consumers c
    WHERE c.ingest_id = :ingest_id AND c.ingest_epoch = :ingest_epoch
      AND c.attach_phase IN ('pending', 'attached')
  );
```

If join wins, the old sink's detach leaves the new consumer intact and cannot
close its reader. If drain wins, join reports `ingest_draining`; retry after
physical close acknowledgement, or claim a successor after logical expiry.
Keep the public/start or recording capture-window deadline: there is no
unbounded wait for a frozen reader. After committing a consumer, attach
rechecks ingest and capture tokens before opening the writer. It must also
check local reader liveness and report a failed ingest rather than attach to
an already dead task. An RPC with uncertain outcome queries the committed
consumer; it never fabricates a second attachment.

Renew ingest and dependent capture authorities together in one transaction,
with capture expiry no later than ingest expiry. Ingest loss revokes all its
consumers for receiving new bytes; every current-progress mutation checks
both epochs. A stopped/expired capture removes its own consumer, not the
whole transport. Any successor ingest requires new capture epochs. Pending
consumers abandoned before attach expire with their claims. Sealed output
may be finalized under separate authority after ingest loss (§6.3).

**Adjacent owner gates:** move reminder delivery/sweeping to a separate
fenced singleton job that runs with DVR capture disabled, preserving current
reminder behavior. Keep atomic due-reminder transitions and existing webhook
semantics. Make Activity aggregate observations by durable capture/ingest
assignment and epoch across all workers, rejecting stale samples rather than
filtering everything to the historical owner. Storage readiness and library
reconciliation resolve the recording's storage target independently of the
device. Include HTTP DVR routes, reminder loop and per-storage reconciliation
in M3; no remaining owner check may silently hide another worker's recording.

### 6.2 A path string does not establish shared storage

Add a persistent DVR storage target ID and an explicit placement kind:
`local` with one node, or `shared` with verified member nodes. Default migration
maps existing `dvr.root` to a local target on the former owner, preserving
where files actually are. This does not restrict viewer placement.

Shared eligibility requires the same configured storage ID, a shared-volume
sentinel, and a cross-node create/read/remove probe in a dedicated diagnostic
directory. Never infer sharing because `/recordings` exists on two hosts.
Probe writable access and free space locally with bounded calls. Unknown or
timed-out storage checks make that node ineligible to record.

Automatic recording continuation on another node is supported only when it
can access that recording's storage target. Local-only recordings cannot
fail over while their volume is inaccessible: record a gap/availability
reason, preserve existing files, and continue to serve unaffected Live TV.
Do not silently substitute another machine's path or claim missing bytes are
recovered. Do not migrate old recording paths during this effort.

### 6.3 Epoch-specific files prevent stale workers corrupting successors

Use a unique attempt directory containing recording ID, monotone claim epoch,
node boot ID and random artifact ID. Create files exclusively. A worker may
write only its own attempt path; no two epochs append to the same file.
Do not use the current node-local attempt-number allocation across workers.

A sink is eligible for finalization only after its writer has closed, synced
and sealed its artifact. Publication is one database CAS checking recording
state, intent revision, configuration, storage target and current finalizer
authority. Stop-capture and delete intent have different effects below.
An old worker may finish closing its own file, but cannot update the current
recording or publish a library item. Late observations remain tagged with the
old epoch and cannot become current byte counts.

**Stop preserves captured bytes:** a Stop records stop attribution and an
intent revision, prevents further capture claims, and cancels/settles the
current sink. It does not suppress publication of useful sealed bytes.
Preserve existing `done`/`partial` classification, minimum-useful-duration and
stopped-by attribution. If the worker dies, an eligible storage worker claims
a fenced finalizer lease for that recording and intent revision, seals only
its own settled artifacts, and publishes already sealed artifacts without
opening a tuner. An unsealed predecessor is excluded and its interval is a
gap. A terminal outcome with no useful sealed bytes follows existing DVR
failure semantics; it must not remain pending forever.

**Delete suppresses publication:** explicit Delete is a separate durable
terminal intent. Final publication predicates reject deleted recordings,
changed finalizer fences and changed intent revisions. A deletion after
publication schedules removal of the exact committed artifacts and library
references. A finalizer losing the race leaves orphan output for cleanup;
it cannot recreate the row/item. Changing capture authority never grants a
worker permission to delete another epoch's files. Add finalizer state and
fence to the proposed DVR claim schema and Store DTOs before implementation.

Do not concatenate an unsealed earlier attempt while its old process could
still be writing. After failover, carry forward sealed artifacts only; mark
any unavailable/unsealed interval as a gap. Keep unsealed orphan files for
later cleanup after positive writer closure or host-restart evidence. Expiry
alone never authorizes reading a file as complete or deleting it under a
writer. Missing old bytes must not block a new independent capture attempt.

Finalization writes a unique immutable output path for its epoch, then
publishes that exact path through the fenced transaction. Never rename over
a shared canonical output before checking authority. The recordings-library
scanner indexes only committed artifact manifests; it must not discover
orphan `.ts` outputs from stale publishers. Adapt the existing targeted DVR library
sweep and full recording-library scan, which assume the single owner is the
only writer. Both must enforce the committed-manifest filter. Preserve legacy
finished recordings by importing their recorded paths as committed legacy
artifacts; do not hide existing library content. New paths containing epochs
must use stable recording-ID-based item identity and the existing human title,
not an epoch directory/basename as a new item key. Keep old item IDs and watch
state; rescanning or changing finalizer must not create a duplicate item.

Retention and explicit deletion atomically claim a particular artifact for
removal. Use its storage ID and immutable path and recheck publication/pins
before the removal claim. A new publication cannot reuse a path marked for
deletion. Failed cleanup stays retryable per artifact and cannot drain the
entire device. Event IDs include recording/epoch/transition so a scheduler
replay does not duplicate lifecycle events. Preserve existing webhook delivery
semantics; exactly-once external notification is not promised.

## 7. Failure behavior and security invariants

| Event | Required behavior | Forbidden shortcut |
|---|---|---|
| Ingress dies after reservation | Another ingress resolves the same request row. | New worker chosen from a local request map. |
| Worker reply lost after opening | Query same assignment; report pending/unknown within the public deadline. | Parallel speculative opens on other nodes. |
| Worker dies with two tuners free | Other workers admit independent requests against policy and device capacity. | Global disable or admin recovery requirement. |
| Worker isolated from quorum but still reaches tuner | Local authority cancellation; majority can use remaining hardware and later reclaim expired logical capacity. | Claim that expiry physically fenced the tuner. |
| Whole cluster loses quorum | No new claims/publications; apply existing serving loss to running sessions. | Independent local fallback counters. |
| Device refuses with 503 | Preserve `tuner_unavailable` without inventing a precise cause; bounded fresh attempts only through server recovery. | Force-retune, unlock, or repeat on every node. |
| Config disable/address change races start | Generation CAS rejects late admission and activation; cancel all known old-generation attempts. | Single old-owner drain barrier for the whole cluster. |
| Old worker resumes after replacement | It closes its own resources; all old-epoch mutations fail. | Let a matching node ID bypass a changed boot ID. |
| Stop races recording claim | Stop blocks new capture and settles the current sink; a finalizer preserves useful sealed bytes. Delete alone suppresses publication. | Attach file/tuner first, or discard recorded bytes on Stop. |
| Storage worker fails | Shared-target eligible worker opens a new unique attempt; local target reports unavailable. | Two nodes append to the same attempt or scan uncommitted output. |
| Old client writes owner settings | Explicit migration/compatibility response, no placement change. | Silently restore manual ownership. |

Keep exact-request and bounded-response signatures on peer control calls,
redirect refusal, replay/nonces, committed membership checks, per-user
capability authorization and redaction. A public start cannot supply an
arbitrary device URL or relay destination. Continue private-address and
pinned-endpoint validation. Fresh DeviceID checks bind discovery to the
configured device; no DeviceAuth enters public snapshots or logs.

Cancellation fans out to recorded attempt holders. Track cleanup state per
attempt and generation. A disabled cluster promises no new authorized work;
it cannot guarantee an unreachable external socket stopped immediately.
Re-enable does not require proving every old process is powered off. Old
claims still count until closed/expired, after which hardware arbitration
applies. Explain this difference in operations docs and UI without presenting
uncertain hardware availability as a successful drain.

## 8. Migration, old clients, and rollback

### 8.1 Server migration is one controlled mode transition

1. Add tables, interfaces and the new internal capability while retaining
   legacy operation. Compatibility/schema versions follow the repository's
   actual upgrade protocol; do not assume arbitrary mixed schema versions can
   form a cluster. Test the supported upgrade sequence.
2. Upgrade all serving nodes, or remove unavailable unupgraded members using
   the existing membership-removal workflow while preserving quorum. Cluster
   mode requires every remaining serving member to advertise support; membership join/promotion must enforce that marker in
   the cluster schema. Avoid cluster triggers referencing application tables,
   the join failure recorded in the original tuner status.
3. On the supported upgraded cluster, briefly stop new legacy admissions,
   drain current legacy sessions/recordings, and import configuration/storage
   identity in one generation transition. Preserve schedule, reminders and
   recording history. Surface interrupted captures honestly.
4. Convert to cluster mode and restart eligible workers automatically. No
   routine owner choice is required. If the old owner is unreachable and was upgraded or its old membership
   identity was removed, migrate without a physical attestation: mark legacy occupancy unknown, rely on
   device arbitration, and never reuse legacy unsealed recording files.
5. In cluster mode reject incompatible nodes joining as serving members.
   Restarted old binaries must not serve using stale owner settings; the exact software
   fence below must be demonstrated in the mixed-version test before release.

Do not enable cluster mode on a partially upgraded fleet by merely clearing
`owner_node_id`. Every process capable of legacy side effects must either
understand the transition or be excluded by serving compatibility. An offline
old binary can still hold a hardware socket: that is accounted for as
uncertain occupancy, not as proof that migration failed.

**Selected restart fence:** M1 introduces a real SQLite schema migration and
a replicated authentication-schema version bump, following the supported
migration adapter. M5 verifies the predecessor binary's existing refusal of a
newer schema, not just the new binary's capability checks. The inspected
SQLite `migrate` in [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs)
refuses `PRAGMA user_version > SQLITE_SCHEMA_VERSION`. The replicated
[startup/migration path](../../crates/plurx-core/src/cluster/migration.rs)
performs committed membership admission in `start_voter` before serving and
opens the Store through its compatibility validation. Preserve those fences.

Mode enable uses a transactionally checked membership/capability census plus
the cluster-schema marker. It rejects pending/unknown incompatible members;
an absent unupgraded member must be upgraded or explicitly removed first.
Do not delete members automatically to make a settings save succeed. The
normal membership removal retires the old identity and quorum authority; a
returning node must follow the supported rejoin path on a compatible binary.
This is a one-time software upgrade prerequisite, not proof that its tuner
socket closed and not a permanent device owner.

Before choosing the final schema/protocol numbers, run a predecessor-binary
restart fixture with its existing data directory, including a stale local
replica. It must fail compatibility or committed-membership admission before
starting legacy guide/DVR/ingest work. Add or backport a prerequisite software
fence release if the predecessor can reach serving through that path. Mode
enablement remains blocked until that path is proven; a new-binary test that
only hides its capability is insufficient. SQLite binary downgrade must fail
at database open. A fully isolated old process may retain a socket, but lacks
new publication authority; physical arbitration still permits spare tuners.

### 8.2 Keep playback wire compatibility and update every settings surface

Preserve `ltv1.<encoded worker node>.<random UUID>` and the existing public
playback endpoints; add protocol 4 intent preparation as specified in §4.5. Retain legacy settings fields in responses for clients
with required decoders; return the saved historical owner value as deprecated
metadata, never as the selected worker. Add explicit cluster resource mode
and node-readiness fields. New clients ignore legacy owner metadata.

For an old settings client, tolerate an unchanged legacy owner field sent with
an otherwise valid save; reject a changed owner or physical-fencing request
with an actionable `settings_client_upgrade_required` response. Do not let an
old client accidentally prevent harmless output/guide edits by always sending
its existing owner field. Existing generation conflict checks remain.

Remove owner input and fencing checkbox from
[web settings](../../crates/plurxd/src/web/pages/settings-live-tv.js),
[Apple settings](../../clients/apple/Sources/LiveTvDeveloperView.swift) and
[Apple DTOs](../../clients/apple/Sources/LiveTv.swift), plus Android's
[API](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt)
and settings call sites. Show eligible nodes and storage placement separately.
Audit client start/resume/retire behavior for unknown outcomes across ingress
changes; server changes must not reintroduce client-side retry loops.

### 8.3 Roll back behavior without losing new durable state

Keep an all-new-binary legacy-mode path during the migration window. To revert,
stop new cluster admissions, revoke/drain current attempts, select a temporary
legacy worker in the administrative rollback operation, and increment mode
and config generation atomically. Ordinary product UI stays owner-free.
Do not downgrade binaries over new schema/artifact state. Reverting binaries
requires the repository's supported schema rollback or a stopped-cluster
restore procedure; no automatic destructive down-migration. Validate rollback
with active captures, pending starts and an unreachable worker.

## 9. Observability — identify what is actually unavailable

Add bounded-label counters for admission result, device refusal, assignment
recovery, lease loss, stale mutation rejection, and guide takeover. Publish
policy counts split into reserved/opening/active/draining; stale worker and
unknown physical occupancy are distinct diagnostic states. Avoid request IDs,
capabilities and arbitrary channel text in metric labels.

Activity shows device, session server, ingest state, recording storage target,
capture epoch, gap duration and cleanup outcome where relevant. An increasing
stale-mutation counter during a failover test demonstrates fencing; continued
increases outside recovery need investigation. A device 503 with policy slots
free can mean external use, orphan use, signal or authorization trouble; the
UI must not diagnose a specific cause from that status alone.

## 10. Implementation sequence and review boundaries

Use `effort/live-tv-cluster-resource`, task branches `codex/ltv-resource-*`,
and task PRs back into the current effort. Files overlap, so the independent
main-branch exception does not apply. Follow
[the development pipeline](../DEVELOPMENT_PIPELINE.md) and establish the
[pinned compiler loop](../ci/AGENT-COMPILE-LOOP.md) before writing Rust.
Archive committed source without credentials if compiling elsewhere. Re-run
against the exact final base after integration. Never use CI as the compiler.

| Task | Files/areas owned for that task | Deliverable and acceptance |
|---|---|---|
| M0: baseline and failure fixture | Daemon two-node tests, test fixture device, validation catalog | Reproduce non-owner rejection today; fixture counts sockets, supports saturation, delayed opens, lost responses and controllable closure. Existing baseline passes. |
| M1: durable authority | Core Store, SQLite/Hiqlite schemas and coordination contracts, DVR DTOs/imports | Atomic admission/start identity/capture claims and publication fences. Race two real backend callers and prove one assignment; reject stale epochs including after snapshot restore/restart. |
| M2: placement and worker lifecycle | Manager, public/internal Live TV HTTP, peer transport, serving integration | Two nodes concurrently ingest different channels; ticket and legacy replay/activation/retire resolve across ingresses; existing capability routes keep working. Mode remains legacy by default. |
| M3: DVR and storage | DVR engine/scheduler, HTTP DVR, reminders, Store mutations, artifacts, scanner and retention | Claim before open; shared/local storage classification; safe new attempts and publication; same-channel recordings share one ingest; old epoch cannot affect new output. |
| M4: guide and readiness | Guide cache/replication, refresh job, HTTP readiness | Kill current refresh holder and retain readable guide; healthy worker remains selectable when another cannot reach device. |
| M5: migration and clients | System settings, membership compatibility, web/Apple/Android Live TV DTOs and settings | Supported upgrade and rollback fixtures; no owner or physical-fencing control in normal settings; legacy playback and unchanged legacy settings saves work. |
| M6: qualification and live docs | Integration tests, validation catalog, maintained reference docs | All §11 cases pass, physical device evidence recorded, final effort promotion qualified on current main. |

Each task ends with its focused regression command and observed result in the
PR. Keep behavior disabled until M1–M5 integrate. When a task changes Store
interfaces, both backends and import/export/migration fixtures change together.
No stub backend or unimplemented default can be accepted as an intermediate
production path. New web files, if needed, follow
[the web shell table](../clients/WEB-SHELL-LAYOUT.md), including assets/tags.

At implementation completion update
[architecture](../ARCHITECTURE.md), [API](../API.md),
[operations](../OPERATIONS.md), [features](../FEATURES.md), the original
[tuner plan](HDHOMERUN-LIVE-TV-PLAN.md),
[tuner status](HDHOMERUN-LIVE-TV-STATUS.md) and
[DVR status](LIVE-TV-DVR-STATUS.md). Add dated supersession notices to the
owner-dependent sections of the guide/shared-transport/session-fence plans;
retain their history. This document supersedes permanent device ownership as
the proposed target, not the still-shipping behavior.

## 11. Verification — prove races, partitions, and the real device

### 11.1 Focused regression commands

These commands target existing suites or the proposed `live_tv_resource`
module/filter. M0/M1 must add the new tests before those filters are evidence;
record nonzero test counts and names. Verify Rust 1.97.1 explicitly.

```bash
rustc --version
cargo fmt --all -- --check
cargo check --locked -p plurxd --all-targets
cargo clippy --locked -p plurxd --all-targets -- -D warnings
cargo test --locked -p plurx-core --features hiqlite-store --lib live_tv_resource
cargo test --locked -p plurxd --bin plurxd live_tv::
cargo test --locked -p plurxd --features cluster-integration-tests --test live_tv_two_node -- --test-threads=1
python3 -m unittest discover -s tests/operations -p test_docs_index.py
node --test tests/web/live-tv.test.js tests/web/settings-sections.test.js
```

M1 also runs the new backend-neutral contract against SQLite and Hiqlite,
including the feature flags needed for those fixtures. M3 runs DVR scheduler,
writer isolation, retention and library scan contracts. M5 runs Apple and
Android compilation and focused settings/recovery tests using the repository's
current platform lanes. Qualification runs the required broader suites and
`make unit-core`; focused filters do not replace the final promotion gate.

### 11.2 Acceptance matrix

| Case | Required observation |
|---|---|
| Two nodes / one device | A and B each perform one HTTP GET for different channels; no configured device owner. |
| Four nodes / capacity L | At most L unexpired logical ingest claims; hardware accepts no more than its capacity; refusals are typed. |
| External client takes a tuner | Plurx accepts device refusal without unlocking/retuning it or blaming a specific failure cause. |
| Concurrent identical start on two ingresses | One durable assignment and one HTTP open in the normal non-expired case. |
| Lost reply at reserve/open/activate | Same request resolves; no speculative second open; public deadline stays bounded. |
| Retire before reserve/activation, including full history | Acknowledged stop cannot be forgotten within the stated protocol lifetime; no authorized late activation. |
| Ticketed/legacy POST after terminal GC | Ticketed start rejected without an open, including stripped-ticket legacy fallback attempts; legacy IDs follow the explicit 24 h guarantee. |
| Dead worker, spare hardware | A fresh request on another worker succeeds without settings edits or global drain. |
| Frozen old worker retains hardware | Expiry permits logical replacement; no physical-release claim; full device refuses cleanly, spare capacity remains usable. |
| Clock jumps and delayed renew response | Stale epoch cannot publish; unusable renewal does not extend local authority. |
| Membership removal and changed boot ID | Removed/stale workers cannot reserve, renew, activate or publish. |
| Disable/re-enable while start is delayed | Old generation cannot activate; new attempts use new generation and truthful occupancy. |
| DVR reserve and admission race | Arrival order does not let recording claims exceed L − R; same-channel sinks add no ingest. |
| Two schedulers claim one recording | Only the claim winner opens a tuner/file; stop wins when ordered before claim. |
| Last DVR detach races new channel join | Join-first preserves the reader; drain-first refuses attach and recovers within its deadline; ingest loss fences capture progress. |
| Stop then worker loss; Delete races finalizer | Stop preserves useful sealed bytes without reopening; Delete cannot resurrect an item. |
| DVR worker fails during write/finalize | Successor uses unique paths; unsealed predecessor excluded; old progress/finalize/delete rejected. |
| Two hosts with unrelated identical root paths | They are not accepted as one shared storage target. |
| Rescan after finalizer changes or legacy artifact import | Stable item ID/title and watch state; no duplicate item or hidden old recording. |
| Shared-volume takeover | A second verified node continues with a new epoch and records the gap; prior sealed output preserved. |
| Local-volume failure | Recording storage unavailable is explicit; other Live TV remains usable. |
| Retention races finalization | Only claimed immutable artifacts removed; new committed output survives. |
| Guide worker dies | Another durable copy remains readable; new holder refreshes under a new fence. |
| Upgrade with offline old worker | Cluster mode works without attestation; old binary cannot regain serving authority; lingering socket treated as unknown physical use. |
| Reminders with DVR off; multi-worker Activity | Reminders still fire through one leased job; observations show every current capture epoch. |
| Old/new web, Apple and Android clients | Existing playback works under the bounded legacy contract; upgraded clients use ticketed starts; new settings lack owner controls; unchanged old owner metadata tolerated. |
| Single-node SQLite restart | Claims and terminal intent survive; boot ID changes fence old assignments; no new cluster-service dependency. |

Physical acceptance uses two actual plurx nodes and one HDHomeRun, records
model/firmware, DeviceID (redacted in public evidence if needed), tuner count,
node revisions, channels, open/close times and outcomes. Observe concurrent
streams and one node loss with spare capacity. Repeat under full capacity
and with an independent HDHomeRun client. Do not force-retune or power-cycle
the device as part of an automated fixture. Synthetic success alone does not
prove firmware behavior or seamless playback recovery.

### 11.3 Promotion and completion

Freeze task merges, merge current main into the effort, run exact-candidate
qualification and obtain the qualification receipt. Merge only after the
Main promotion gate passes on that candidate. If either branch moves,
requalify. A green task compile gate does not make the effort releasable.

Completion requires evidence for §1.2 and §11.2, owner-free settings on all
three clients, recording safety, updated live references and recorded physical
acceptance. A documentation-only review does not satisfy those conditions.

## 12. Review disposition

The independent review raised four findings. All are accepted as design
corrections; no finding is waived. The review preserves its initial objections
and records its accepted re-review verdict. No design blocker remains from
that review. The predecessor-binary proof in §8.1 remains a required
implementation prerequisite, not evidence already obtained.

| Finding | Revision | Required implementation evidence |
|---|---|---|
| F1 / P1: retirement before reserve and replay after GC | §4.5 adds issued server tickets, insert-or-terminalize retirement, reserved history capacity, fail-closed legacy quota handling and an explicit bounded legacy guarantee. | Both protocols, quota exhaustion, restart, post-GC GET counts and attempted ticket-to-legacy downgrade. |
| F2 / P1: last-sink drain races channel join | §4.1/§6.1 add durable consumers, an atomic last-detach predicate, joinable states, demand revision and coupled ingest/capture authority. | Barrier-controlled ordering tests, lease-loss propagation and delayed attach. |
| F3 / P2: Stop must retain captured bytes | §6.3/§7 separate stop from delete and grant independent fenced finalization without tuner acquisition. | Stop before/after sealing, worker loss, deletion/publication race. |
| F4 / P2: existing-member binary restart bypass | §8.1 names schema downgrade and committed-membership fences, upgrade/remove handling for absent members and a predecessor-binary prerequisite test. | Actual supported predecessor restart, stale replica, removed identity and SQLite downgrade. |

The source audit also added multi-worker Activity, reminder ownership,
legacy artifact import and stable recording item identity to M3. These are
required scope, not optional follow-ups. Runtime implementation, compile and
hardware acceptance are still outstanding.
