# Cluster performance — turn replicated correctness into useful capacity

**Status:** P0–P5 implementation and deterministic acceptance delivered; M4
singleton and serving-partition proofs delivered; P5 storage-pressure behavior
revised after adversarial review (§6.6), with four of its storage guards still
design intent rather than pinned behavior; P0c/P2f/P5 physical evidence and
P6–P7 remain · **Extends:** [CLUSTERING-PLAN.md](CLUSTERING-PLAN.md)
after functional multi-voter membership · **Written:** 2026-08-21 against
`main` @ `aee2cbe0`

Companion to [ARCHITECTURE.md](ARCHITECTURE.md) (why plurx embeds Raft),
[OPERATIONS.md](OPERATIONS.md) (how to operate membership), and
[PERF2-PLAN.md](PERF2-PLAN.md) (playback-path performance) — this plan makes a
working multi-voter cluster responsive under ordinary API, catalogue, and
playback load. Read §3 before changing a consistency boundary, then execute
§6 one pull request at a time. Re-verify every source path and literal against
the branch being built; if a step appears to require stale authorization,
unacknowledged durable writes, or an unhealthy follower serving traffic, stop
and amend the contract rather than improvising.

## 1. Objective — spend consensus only where correctness needs it

Raft is the authority for state whose acknowledged loss would be a defect. It
is not the request router, the transcode scheduler, or a reason to send every
read and activity timestamp through the leader. The completed work must:

1. preserve the existing promise that a successful Store write is
   quorum-acknowledged;
2. preserve immediate, fail-closed authorization and membership decisions;
3. make three voters the normal minimum-HA topology and document what a fourth
   voter costs;
4. let healthy followers serve reads whose callers explicitly tolerate bounded
   staleness;
5. remove avoidable Raft entries produced by activity bookkeeping;
6. expose enough bounded telemetry to distinguish network, disk, leader, read,
   and application latency before any timer is tuned; and
7. use additional machines for HTTP, streaming, and transcode capacity without
   requiring every machine to increase the voting quorum.

The first optimization is less work, not looser durability.

## 2. Evidence — the current shape leaves capacity on the table

### 2.1 Four voters pay for a larger quorum without surviving another loss

The quorum is `floor(voters / 2) + 1`:

| Voters | Quorum | Failures tolerated | A write needs |
|---:|---:|---:|---|
| 3 | 2 | 1 | leader + fastest one of two followers |
| 4 | 3 | 1 | leader + fastest two of three followers |
| 5 | 3 | 2 | leader + fastest two of four followers |

Three and four voters both tolerate one unavailable node. Four voters add one
replica's network, apply, storage, heartbeat, and snapshot work while moving
the commit point from the fastest follower acknowledgement to the second
fastest. Five voters are justified only when surviving two voter losses is an
explicit requirement.

This plan does not automatically remove an operator's fourth voter. §6.1 first
records a comparable four-versus-three-voter workload, then the operator
removes a follower through the existing safe membership API. The current
leader is never a removal target.

### 2.2 Authentication performed consensus bookkeeping on the request path

This was the pre-#493 baseline. PR #493 completed P1 by retaining the
authority lookup while suppressing activity writes inside the durability
window and coalescing concurrent touches per process.

`AuthUser::from_request_parts` calls `Store::user_for_token` for every
authenticated request. The Hiqlite implementation performs a consistent user
lookup and then submits this statement through Raft:

```sql
UPDATE tokens SET last_seen_at = $1
WHERE token_hash = $2 AND last_seen_at < $3
```

The predicate prevents a SQLite row change inside the state machine, but it
does not prevent the command from becoming a Raft log entry. A segment, image,
or JSON request inside the 60-second activity window therefore used to pay for
a no-op consensus write.

Scoped API keys had the same shape with no SQL-side window at all:

```sql
UPDATE api_keys SET last_used_at = $1 WHERE id = $2
```

`last_seen_at` and `last_used_at` are operator activity hints, not authorization
facts. They remain durable enough to be useful without earning one physical
commit per request. §6.2 records the landed behavior and retained evidence.

### 2.3 Most replicated reads still rendezvous at the leader

At the plan baseline, the `hiqlite*.rs` Store implementation contains about 85
consistent-query call sites and 13 local-query call sites. Hiqlite's
`query_consistent_map` runs on the leader, takes network round trips, and pauses
Raft at a quorum-applied point. Adding HTTP nodes does not distribute that read
work when ordinary catalogue calls retain this primitive.

The opposite blanket change would also be wrong. A partitioned follower can be
arbitrarily stale, so authorization, admission, leases, membership, and
write-followed-by-read behavior cannot silently become local reads. §3.2 makes
the classification explicit and §6.4 adds a lag gate before callers may opt
in.

### 2.4 Durable state and heavy scratch I/O share one configured root

`storage.data_dir` contains `hiqlite/`, the content-addressed cache, offline
packages, subtitle cache, and live transcode work. Operators can place child
directories on separate mounts, but plurx has no first-class durable-versus-
scratch path contract. A cache sweep, offline preparation, or ffmpeg write can
therefore contend with the log and state-machine database on one device.

The Hiqlite node is also built with fixed production choices: a 16 MiB WAL, a
10,000-entry snapshot policy, the default local read pool of four connections,
500 ms heartbeats, and 1.5–3 second election timeouts. These are coherent fast-
LAN defaults. They are not evidence that exposing all of them as knobs will
improve a real workload.

### 2.5 Existing metrics describe playback better than the Store

`GET /metrics` exposes playback TTFF, stalls, sessions, cache results, offline
work, and bounded application counters. Replication status is visible in the
admin system projection, but Prometheus cannot currently answer:

- which Store class is slow: local read · consistent read · write;
- how many requests were forwarded to the leader;
- how long a quorum write or consistent read took;
- whether commit-to-apply lag grew on this node;
- whether leader changes correlate with CPU, disk, or network pressure; or
- how much work auth-touch suppression removed.

Timer tuning without those answers would trade one unexplained symptom for
another.

## 3. Contract — performance does not weaken the HA promise

### 3.1 Writes remain durable at the Store boundary

Every successful Store mutation remains quorum-acknowledged. No milestone may:

- acknowledge a durable write before Hiqlite reports success;
- switch the durable WAL to interval sync or memory-only storage;
- move user, settings, watch, library, membership, or offline ownership rows
  into a node-local cache; or
- hide a failed write behind an optimistic HTTP response, except for the
  already-documented intermediate progress coalescer.

Coalescing is allowed only when the API contract already treats the field as
activity or replaceable progress. The coalescer must publish its durability
boundary in code and documentation.

### 3.2 Every read belongs to one named consistency class

| Class | Primitive | Allowed examples | Refused examples |
|---|---|---|---|
| `Authority` | leader/quorum-consistent | token and key validation · settings that gate work · membership · leases · offline ownership · migration/version guards | catalogue cards solely because the existing code used a consistent helper |
| `ReadYourWrite` | return the write result, revision-fenced local read, or consistent fallback | a settings save response · watch/unwatch response · membership mutation result | an unfenced follower query immediately after mutation |
| `BoundedReplica` | local query only with a fresh quorum-confirmed leader watermark and applied lag within the declared budget | catalogue browse · item/file metadata · genre/filter facts · dashboard aggregates | auth · locks · admission · ownership · schema state |
| `NodeLocal` | process or node-local storage | playback telemetry · transcode scratch · regenerable indexes and caches | any acknowledged user or library fact |

The code should name the class at the call site. A generic helper named only
`query` or `query_map` is insufficient once two correctness contracts exist.

`BoundedReplica` requires two bounds. First, the node must hold a leader-issued
watermark containing `(term, leader_id, committed_index)` that was renewed by a
successful quorum `ReadIndex` or equivalent heartbeat proof. The receiver sets
a non-persisted monotonic `Instant` deadline one second after the bounded quorum
request began; network time consumes the lease instead of extending it. The
proof is invalid after process restart or any observed term or leader-identity
change. Wall-clock timestamps may be diagnostic fields, but never establish
freshness. Tests jump wall time both forward and backward and prove the
monotonic deadline still expires. An isolated former leader cannot renew it.
Second, the local applied index must be known and no farther behind that
committed index than the configured entry budget. The time lease bounds commits
made after the sampled index; the entry gate bounds apply backlog at the
sample. A node that cannot prove every fact falls back to an `Authority` read
or fails readiness according to the caller's contract. Local `metrics_db()`
state and `last_log_index - last_applied` alone are explicitly insufficient:
they can look current on a partitioned follower while a new majority keeps
committing.

### 3.3 Authorization remains immediately revocable

Token and API-key lookup remains an `Authority` read until an invalidation
protocol independently proves an equivalent revocation guarantee. The first
auth optimization only suppresses no-op activity writes:

- include the durable activity timestamp in the consistent lookup;
- submit a touch only when that timestamp is outside the 60-second window;
- reserve that credential in a process-local keyed singleflight before the
  mutation and release the reservation if the write fails;
- retain the SQL predicate as the race-safe final guard; and
- bound a concurrent window-edge burst to at most one submitted mutation per
  serving process. Across `N` HTTP nodes, at most `N` entries may race; the
  SQL predicate permits only one timestamp change.

A future successful-auth cache requires explicit revoke notifications, a
bounded fallback TTL, loss/reconnect tests, and fail-closed behavior. It is not
part of the first implementation slice.

### 3.4 Readiness means this node is safe for the traffic it receives

`/healthz` remains process liveness. `/readyz` remains the load-balancer
surface, but its replicated-store answer must eventually include local apply
health once `BoundedReplica` reads ship. A node with a live process and leader
connectivity but excessive local apply lag must not serve stale catalogue
traffic merely because a remote consistent ping succeeded.

Streaming requests should be sticky to preserve process-local sessions and
cache warmth. Stickiness is an optimization, not an availability dependency:
when a node fails readiness, the load balancer may send the next request to a
survivor and plurx's deterministic segment contract remains responsible for
recovery.

### 3.5 Configuration is symmetric and bounded

Every Raft setting must be identical on all voters. New config fields require:

- one safe default matching today's behavior;
- a documented range and unit;
- startup refusal for invalid combinations;
- an operations entry describing when the setting takes effect; and
- a cluster-upgrade procedure that never runs incompatible timer sets
  indefinitely.

The first tunable is `read_pool_size`, after local reads create evidence that
the pool is a limit. Election and snapshot settings stay internal until §6.6
shows a repeatable need.

## 4. Target shape — three voting replicas, distributed application work

```text
                         clients
                            │
                readiness + sticky streams
                            ▼
                 ┌─────────────────────┐
                 │ reverse proxy / VIP │
                 └──────┬──────┬───────┘
                        │      │
             ┌──────────┘      └──────────┐
             ▼                            ▼
      ┌─────────────┐              ┌─────────────┐
      │ voter A     │◀──── Raft ───▶│ voter B     │
      │ HTTP + work │               │ HTTP + work │
      └──────┬──────┘               └──────┬──────┘
             │          Raft               │
             └──────────┬──────────────────┘
                        ▼
                 ┌─────────────┐
                 │ voter C     │
                 │ HTTP + work │
                 └─────────────┘

     optional fourth machine after §6.7:
       non-voting learner/read worker · proxy · metrics
       (a non-quorum replicated copy, not a restore-tested backup)
```

Every voter must be capable of leadership: comparable durable storage, stable
wired networking, and CPU/I/O headroom protected from ffmpeg saturation. A
leadership preference is not a substitute for making all voters safe leaders.

## 5. Measurement contract — establish causality before tuning

### 5.1 Bounded Prometheus surface

Add these families with fixed label vocabularies:

| Metric | Type | Labels | Source and meaning |
|---|---|---|---|
| `plurx_store_operation_seconds` | histogram | `class`, `outcome` | `TimedClient`: local read · authority read · write latency |
| `plurx_store_operations_total` | counter | `class`, `outcome` | `TimedClient`: volume and error rate by consistency cost |
| `plurx_auth_activity_writes_total` | counter | `kind`, `result` | auth coalescer: token/key touch submitted · suppressed · failed |
| `plurx_raft_commit_index` | gauge | none | most recent quorum-confirmed leader watermark; meaningful only while its validity gauge is `1` |
| `plurx_raft_applied_index` | gauge | none | OpenRaft local metrics watch: latest entry applied on this node |
| `plurx_raft_current_term` | gauge | none | OpenRaft local metrics watch: term observed by this node |
| `plurx_raft_leader_known` | gauge | none | OpenRaft local metrics watch: `1` while this node identifies a leader |
| `plurx_raft_is_leader` | gauge | none | OpenRaft local metrics watch: `1` when the observed leader is this node |
| `plurx_raft_apply_lag_entries` | gauge | none | saturating watermark commit minus local applied; meaningful only while both sources are valid |
| `plurx_raft_leader_changes_total` | counter | none | OpenRaft metrics watch: process-observed leader identity changes |
| `plurx_raft_snapshot_seconds` | histogram | `operation`, `outcome` | explicit `build` · `install` snapshot start-to-finish hooks, not monitor polling |
| `plurx_raft_metric_sample_errors_total` | counter | `source` | failed samples from the fixed `watermark` · `local` vocabulary |
| `plurx_raft_metric_sample_age_seconds` | gauge | `source` | age of the last successful `watermark` · `local` sample |
| `plurx_raft_metric_sample_valid` | gauge | `source` | `1` only while the `watermark` · `local` source is present and inside its freshness contract |

Do not label by SQL, route, token, user, node UUID, item, session, or dynamic
error text. Those values either leak data or create unbounded series.

### 5.2 Comparable scenarios

Each benchmark run records the build SHA, voter count, leader, request target,
node hardware, storage device, network path, dataset size, concurrency, and raw
result. At minimum:

| Scenario | Load | Readout |
|---|---|---|
| auth burst | one token · 120 authenticated requests in 60 s | physical activity commits · p50/p95/p99 request latency |
| browse | home, library, item, and search mix against each node | authority/local counts · p50/p95/p99 · follower lag |
| progress | existing 80-stream deterministic coalescer fixture | incoming beats · physical commits · compacted bytes |
| topology | identical write mix on fresh independent three- and four-voter clusters, with run order counterbalanced | p50/p95/p99 acknowledged-write round trip · CPU · network · disk |
| follower loss | kill one non-leader under load | errors · latency peak · recovery time |
| leader loss | kill the reported leader under load | failed requests · election time · time until readiness |
| snapshot | cross two 10,000-entry snapshot cycles | write tail latency · snapshot duration · retained bytes |

P0 runs at least three paired repetitions of each scenario on the named
cluster-performance runner. The topology estimand is the paired four-voter ÷
three-voter ratio for write p99, CPU seconds, storage-write bytes, and network
transmit bytes. Runs alternate `3→4` and `4→3`; after three pairs, collection
may stop only when every ratio's two-sided 95% Student-t confidence interval on
the log scale has a half-width no greater than 5%. Collection stops after seven
pairs even if that precision is not reached; such a result is recorded as
`inconclusive` and cannot justify a tuning or latency claim. This pre-registered
3–7-pair rule prevents optional stopping. Every run records
the external load-generator host, its resource isolation, and sample count;
the same pinned client placement is used for both topologies. P0 commits the
median baseline plus every raw artifact under a versioned schema. It
also records concrete go/no-go limits before later behavior PRs begin. The
initial limits are: zero additional request errors; no more than a 10% p99
regression in unaffected Store classes; less than 2% CPU and wall-time overhead
from metrics-only P2; at least a 95% reduction in sequential auth activity
entries for P1; and at least a 70% reduction in authority catalogue reads for
the P3 slice without a greater than 10% browse p99 regression. P0 replaces
these provisional percentages only with reviewed numbers justified by runner
variance.

Deterministic CI contracts enforce correctness and physical-entry bounds.
Hardware results enforce the recorded comparative budgets; noisy benchmark
percentiles do not become unit-test assertions. Until that baseline exists, a
PR must not claim an absolute latency improvement from a synthetic
microbenchmark alone.

## 6. Milestones — one correctness boundary per pull request

Milestone numbers describe dependency order, not necessarily one GitHub pull
request. P0 and P2 cross operational, deterministic-CI, vendor, and
hardware-evidence boundaries, so they are delivered in the following truthful
slices:

- **P0a — operations contract:** voter-count guidance, readiness probe cadence,
  sticky-stream behavior, and child-mount storage layout;
- **P0b — deterministic topology artifact:** a versioned report schema,
  percentile/unit tests, fresh independent three- and four-process semantic
  runs, a stable leader/term boundary, and per-voter local corpus validation
  proving that both runs use identical logical work;
- **P0c — named-runner evidence:** after the separate-process M4 singleton
  takeover proof and the distinct serving-node partition proof, fresh
  independent clusters on the pinned four-machine runner, alternating run
  order, at least three paired runs continued until uncertainty is narrower
  than the acceptance budget, a pinned isolated load-generator placement,
  per-node resource captures, medians, the pre-registered paired confidence
  intervals above, and reviewed budgets;
  the remote/container runner, resource capture, campaign schema, and
  pre-registered stopping validator are implemented separately from the raw
  hardware evidence so CI cannot be mistaken for the named result;
- **P2a — observer-safe metrics snapshots:** remove Store calls from the scrape
  path before Store instrumentation can observe itself;
- **P2b — Store primitives:** RAII timing for `local_read`, `authority_read`,
  and `write`, including `cancelled` outcomes;
- **P2c — passive local Raft state:** applied index, term/leader observations,
  and monotonic sample validity without claiming a commit watermark;
- **P2d — quorum watermark:** a narrow vendored leader proof bound to term and
  leader identity;
- **P2e — snapshot hooks:** separate build/install operation labels and vendor
  patch documentation; and
- **P2f — named-runner overhead evidence:** the before/after artifact enforcing
  the P0 instrumentation budget.

GitHub-hosted CI accepts schemas, semantics, fixed labels, state transitions,
and physical-entry budgets. It does not claim stable multi-machine CPU,
network, disk, election, or p99 evidence; those claims require P0c/P2f on the
named runner.

### 6.1 P0 — document and measure the three-versus-four-voter choice

**Change:** After membership PRs #490 and #491 land, rebase before touching
their shared cluster harness and `OPERATIONS.md` surfaces. Add the odd-voter
guidance, reverse-proxy readiness contract, sticky stream recommendation, and
durable-versus-scratch storage layout to the authoritative operations text.
Extend the cluster benchmark harness to run the same write mix on fresh,
independent three- and four-voter clusters and emit the §5.2 record. Alternate
the topology order on the named runner; never compare a warm post-removal
three-voter run with the four-voter cluster that preceded it.

Do not remove a live voter from automation. The operator selects the follower
after confirming it owns no in-flight transfer and the existing removal API
accepts the change.

The M4 core landed in #505: generic lease-fenced publications and coordinated
scan, provider, genre, and candidate schedulers now share one production
boundary. The singleton process slice compiles that exact production lease
lifecycle into the three-process harness, pauses a follower owner past its
authoritative TTL, admits one successor, bounds provider calls and Raft
entries, and rejects the resumed token inside the publication transaction.
That closes the duplication risk which could contaminate an ordinary-load
baseline. The separate serving-node partition/readiness/capability proof also
landed in #526; `SIGSTOP` remains process unavailability and is not relabelled
as a live network partition. P0c now awaits only the named-runner physical
baseline and its reviewed evidence.

**Acceptance:** CI's replicated-storage contract passes and validates that the
artifact contains both voter counts, exact workload identity, quorum, commit
entry count, acknowledged-write round-trip percentiles, per-voter corpus
digests, and declared units. CI may emit semantic/synthetic timings but
does not certify resource or tail-latency claims. The named runner, raw
three-run results, median baseline, variance, and reviewed comparative budgets
are committed in P0c before P2-P3 performance claims are accepted. P1 already
landed as the separately bounded no-op-write removal in #493.

### 6.2 P1 — suppress no-op auth activity writes

**Landed change:** `HiqliteAuthStore::user_for_token` returns the durable
`last_seen_at` beside the user and skips `execute` inside the 60-second window.
The API-key path applies the same due check to `last_used_at` before the
extractor calls `touch_api_key`. A process-local per-credential singleflight
reservation makes concurrent requests on one process submit only one entry and
clears on write failure. Both authority lookups and the SQL race predicates
remain intact.

The predicate is pinned as `last_activity < now - 60`: never-used is due, 59
seconds and exactly 60 seconds are suppressed, 61 seconds is due, and a clock
rollback suppresses the best-effort touch. Disabled, deleted, or otherwise
unauthorized credentials never reserve or touch. The implementation gives one
process at most one submitted touch per credential per window; `N` serving
processes may submit at most `N`, independent of request count.

**Retained evidence:** `cluster.auth` and the HTTP auth matrix keep revocation
immediate. Replicated-store contracts pin synchronized 120-request bursts
through one and three independently bootstrapped Store instances for both
token and API-key paths. The three-Store serving-process model uses distinct
leader-discovering clients and submits between one and three Raft entries while
every accepted write converges on the fixed-clock durable timestamp. A
separate synchronized unit contract proves that each independent gate admits
exactly one operation. Warm sequential requests submit no additional entries,
disabled or deleted keys never touch, and the operation-level reservation
contract proves a failed token or API-key touch releases its reservation for a
successful retry. Those checks remain required after later Store
instrumentation changes.

### 6.3 P2 — instrument Store and Raft cost

**P2a observer boundary:** `/metrics` reads an in-memory snapshot for library,
user, offline-package, and watched-outbox gauges. One aggregate authority read
refreshes the complete snapshot on a node-staggered 30–44 second cadence, with
bounded exponential backoff after failures. A failed sample preserves the
preceding complete value; boot-without-a-sample omits the Store-backed gauge
families, and fixed validity, monotonic-age, and error series distinguish
fresh, stale, and absent data. Session, active-cache, and sampled-Store values
are lock-free on the scrape path. The handler extracts a Store-free substate,
so it cannot observe the instrumentation it is about to expose. On four
voters, the idle load is roughly one aggregate authority read per node per
staggered interval rather than the prior draft's sixteen synchronized reads
every fifteen seconds.

**P2b Store primitives:** the single `TimedClient` boundary records fixed-label
latency histograms and counts for `local_read`, `authority_read`, and `write`.
Each class reports `ok`, `error`, and caller-`cancelled` outcomes. An RAII timer
publishes cancellation when an in-flight future is dropped, while SQL rejected
before I/O is not mislabeled as an attempted Store call. Transaction and
returning-write helpers classify nested statement failures as `error` and share
the same `write` class. The local/management Raft health probe is excluded;
readiness counts only its subsequent authority SQL read. The exposition reads
only process-local saturating atomics.

**P2c passive local Raft state:** the vendored Hiqlite boundary exposes a
local-only watch wrapper that cannot fall back to its management HTTP API.
`ReplicationMonitor` publishes the current term, local applied index,
leader-known/local-leader state, and distinct known-leader changes into a
coherent atomics-only sample. Watch changes publish immediately and a
five-second refresh keeps an unchanged healthy cluster fresh. A closed watch,
an unhealthy running state, or a regressed term or
applied index increments the fixed `source="local"` error counter and preserves
the preceding sample until it becomes invalid after 15 seconds. The local
applied index is not a quorum-confirmed commit watermark; P2d remains the only
slice allowed to add that claim and derive apply lag from it. The system status
path remains separate from this narrow observer and retains its management
fallback for maintenance callers.

**P2d quorum watermark:** the vendored Hiqlite client asks the current database
leader for OpenRaft's quorum-backed linearizable-read proof and returns only
`(term, leader_id, committed_index)`. Followers carry that reserved request in
the existing authenticated consistent-query envelope, so an older leader
returns a harmless SQL error instead of failing the shared stream on an unknown
wire variant. Both client and leader waits are bounded. Each process anchors a
one-second monotonic lease before sending, renews at a staggered 500 ms cadence,
and never extends the deadline after an error.

`ReplicationMonitor` publishes the proof under the same seqlock as the passive
local sample. Any term or `Option<leader_id>` transition advances an internal
epoch, including `Some -> None -> same`, and permanently invalidates the old
proof. A watermark is valid only while its pre-request deadline has not arrived,
the local source is fresh, the local epoch, term, and leader still match, and a
local applied index exists. Only that state derives saturating commit-to-apply
lag. Metrics retain an invalid prior value for diagnosis, expose fixed
`source="watermark"` validity, age, and error series, and never render either
node identity.

A distinct serving process is a separate authority-only case: it owns no local
Raft replica, so it renews the same bounded quorum proof without polling or
inventing a local applied index. Its proof exposes no apply-lag value and may
gate media readiness only. It is categorically ineligible for `BoundedReplica`;
that class still requires the locally bound term, leader, epoch, and applied
index above. A failed remote renewal retains the preceding proof only until its
original pre-request monotonic deadline, after which serving self-fences.

**P2e snapshot hooks:** the vendored SQLite state-machine builder and installer
start an RAII timer at their real operation boundary and publish exactly one
fixed `build|install` and `ok|error` outcome on every exit, including an early
error or cancelled future. The local Hiqlite client exposes only cumulative
integer histogram state. Prometheus converts that state to seconds without
polling OpenRaft, SQLite, the filesystem, or a Store, and no path, snapshot id,
or node identity enters a label.

**Change:** Instrument `TimedClient` once so every replicated Store module uses
the same bounded local-read · authority-read · write histograms and counters.
Extend the OpenRaft/Hiqlite metrics boundary with the explicit §5.1 sources:
local applied/leader observations from the metrics watch, quorum-confirmed
commit watermarks from a bounded background sampler, and snapshot duration
from build/install start-and-finish hooks. `ReplicationMonitor` projects those
samples but does not invent unavailable values. Expose them through the
existing `/metrics` body.

Metrics collection must be non-blocking and must not execute a Store operation
to describe a Store operation. A failed sample preserves the last known value,
sets its validity gauge to `0` after the freshness limit, publishes sample age,
and increments the bounded source error counter rather than delaying the
request path.

**Acceptance:** exact exposition tests prove HELP/TYPE lines, fixed labels,
monotonic counters, age/validity transitions, and absence of SQL or identifiers.
Cluster CI proves a write, consistent read, and local read each increment only
their class. The named performance runner rejects instrumentation above the P0
overhead budget.

### 6.4 P3 — add lag-gated bounded-replica reads

**Change:** Introduce named authority and bounded-replica helpers. Start with a
small catalogue slice: library list · item/file lookup · recently added · genre
and technical aggregates. Keep search local as it is today. Do not move watch
state, authentication, settings, leases, membership, jobs, cache ownership, or
offline state.

Before a bounded-replica query, require the fresh quorum-confirmed term/leader/
commit watermark and local applied-index proof from §3.2. If either proof is
absent, mismatched, expired, or outside the entry budget, use an authority read.
Extend readiness so a persistently unproved or lagged node leaves load-balancer
rotation before it serves stale browse traffic.

Write-followed-by-read handlers return the write result or use authority until
a revision fence exists. They do not rely on timing.

**Acceptance:** three-voter tests pause one follower's apply path, prove that it
falls back or becomes unready, resume it, and prove local reads return only
after the lag gate recovers. A separate test partitions one follower from the
leader while the remaining majority continues committing; the isolated node
must stop local reads before the one-second watermark lease expires even when
its local log/apply gap is zero. Existing store parity remains byte-identical
after follower and leader loss. Metrics meet the P0 authority-read reduction
and browse p99 budgets instead of merely reporting them.

### 6.5 P4 — coalesce remaining replaceable write traffic

**Change:** Inventory every recurring `execute` from auth, membership, cache,
leases, and playback. Classify it under §3.1. Coalesce only replaceable activity
facts, beginning with cache-use touches and node heartbeat rows. Prefer one
newest value per identity and a bounded flush interval; terminal state changes
remain synchronous.

Membership reachability must not become optimistic. If heartbeat batching
changes its interval, update the 30-second reachable window and prove the node
cannot remain reachable after its allowed missed beats.

**Acceptance:** deterministic paused-time tests pin each commit window and
terminal exception. A load control bypassing the coalescer must violate the
physical-commit budget, matching the existing progress-growth gate pattern.

The retained recurring-write inventory is:

| Traffic | Class | Durable boundary |
|---|---|---|
| Token/API-key activity | Replaceable activity | P1 keeps the Authority lookup and admits one successful touch per credential window |
| Unfenced cache claim `last_seen_at` | Replaceable activity | one successful quorum write per process-local recipe/node/claim identity per five seconds; active fenced producer renewals stay synchronous because they also advance the lease |
| Cache use `last_used_at` | Replaceable activity | one successful quorum write per recipe/node/use identity per five seconds; timestamps advance monotonically |
| Cache manifest scrub cursor | Bounded maintenance progress | at most 128 location observations share one quorum transaction; observation time is monotone while the per-manifest cursor advances synchronously with that batch |
| Membership node heartbeat | Replaceable liveness | concurrent/duplicate submissions inside 250 ms share the first successful quorum result; the ordinary 10 s cadence is unchanged |
| Playback progress | Replaceable intermediate progress | the existing progress coalescer retains its leading/trailing flush and shutdown drain contract |
| Join, removal, cache completion/invalidation/removal, watch terminal state | Terminal fact | synchronous and never admitted to a replaceable-write gate |
| Job/pre-transcode/offline leases and ownership | Authority/fence | synchronous renewal or settlement; never coalesced because losing one changes who may act |

Unfenced cache-touch and heartbeat waiters observe the first write's result before they
may report suppression. A failed first write releases the identity for an
immediate retry. Cache identity accounting is bounded; exceeding the bound
degrades to an ordinary durable touch rather than merging unrelated identities
or growing process memory without limit.

### 6.6 P5 — separate storage pressure and tune only proven limits

**Change:** Add distinct configuration for durable Hiqlite state, persistent
cache/offline bytes, and disposable transcode scratch while preserving
`storage.data_dir` as the compatibility default. Migration must never infer a
new durable database from a scratch path. Document mount, permissions,
capacity, restart, and rollback behavior.

After P3 metrics show local read-pool contention, expose a bounded
`cluster.read_pool_size` with default `4` and benchmark `4 · 8 · 16`. Retain the
smallest value whose p95 improves without increasing write p99 or memory beyond
the recorded budget.

Keep the 16 MiB WAL, 10,000-entry snapshot policy, immediate-async sync, and
current election timers unchanged unless the §5 evidence isolates one as the
limit. Never change `max_in_snapshot_log_to_keep`; Hiqlite ties it to disaster
recovery.

**Acceptance:** upgrade tests prove old configuration selects identical paths;
restart tests preserve cache/offline content and discard only declared scratch;
disk-pressure tests on the cache/scratch device do not corrupt or relocate the
durable target. The chosen read-pool value has a retained benchmark artifact.

Two of those acceptance clauses are not met as written. The disk-pressure clause
is satisfied by an ENOTDIR proxy rather than by a device that actually fills, and
a real out-of-space exercise looks privilege-gated wherever it lands; the
read-pool artifact is deferred with P0c/P2f. Both are stated as such below and
in [OPERATIONS.md](OPERATIONS.md) rather than quietly counted as delivered.

The delivered path contract keeps `storage.data_dir` authoritative for the
Hiqlite database and every durable migration marker. `storage.cache_dir` moves
persistent artwork, transcode/offline, subtitle, and admitted-rendition bytes,
plus regenerable ffmpeg runtime state, while
`storage.transcode_dir` names disposable live-transcode scratch, which must sit
outside `data_dir`; naming `<data_dir>/transcode` explicitly is refused, and
omitting the key is the supported way to run the legacy layout. Omitting both
new roots preserves every legacy path byte-for-byte, and setting `cache_dir` on
an existing install migrates nothing: the daemon warns while the legacy trees
still hold bytes and starts anyway, so OPERATIONS.md carries the old→new path
table an operator has to apply by hand. Runtime is explicitly discarded during
that move; renditions are copied with the other persistent children. Every
managed child is derived from the configured cache root before canonicalization
and passed explicitly to the transcode manager, so relocating only the legacy
`cache/transcode` leaf cannot silently relocate its `subs`, `runtime`, or
`renditions` siblings. Explicit roots are canonicalized and rejected when they
overlap a protected durable identity, credential key, another configured root,
or a Linux mount-source ancestor.

Scratch cleanup begins only after the daemon lock is held and uses
descriptor-relative traversal. Ownership is the discriminator: the root itself
must belong to the daemon uid, a foreign-owned root refuses startup, and a
daemon-owned root that is group- or world-writable is repaired to `0700` with a
warning. Entries inside a daemon-owned root are the daemon's own leftovers, so
their modes and owners no longer refuse — they are deleted. Links, devices,
sockets, and FIFOs inside scratch still abort startup without touching
anything, and a mount point inside scratch is reported as one. The entry-count
and depth bounds guard an unknown directory: in an owned root, exceeding one
leaves that scratch untouched for the boot with a warning and the next boot
retries; in an unowned root it is still a refusal. A crash-safe ownership claim
is published before anything is deleted, and a `.claiming` marker truncated by
a crash is retried as an interrupted claim. The marker names the durable root
it was claimed for, so moving `data_dir` under an existing scratch root gives an
explicit "claimed by another durable root" error rather than silent reuse. Cache
and offline content are never restart cleanup targets. An available shared-cache
mount is a separately protected persistent root; a missing mount keeps the
existing node-local fallback and is not created by this preflight.

The delivered coverage is narrower than that contract. Mutation runs found the
cross-device bound, the depth and entry-count bounds, and the credential-key
inode protection implemented but unreached by any test, and the only real
mount-namespace exercises are opt-in and privileged. The follow-up work added
tests for root repair, the foreign-uid root refusal, permissive-mode and
foreign-uid children, a faulting readdir, the interrupted pending claim, the
truncated published marker, the foreign-durable-root marker message, and the
depth ceiling in the owned-root direction. Three gaps remain, and operator-facing
text must not restate them as proven: the entry-count ceiling, the cross-device
bound, and the wiring that makes the credential key one of the protected
identities cleanup receives — the preflight that honours a protected identity is
tested, the choice of that identity is not. The mount-point error and the
bind-source-ancestry refusal now have real exercises, but only under
`make bind-mount-check` (`PLURX_RUN_BIND_MOUNT_TEST=1`), which needs
CAP_SYS_ADMIN and is deliberately outside `check` and CI.

`cluster.read_pool_size` is now an explicit bounded `1..=16` node-local
setting with the previous value, `4`, as its default, and it now reaches the
named-host runner's node configuration — before that the runner built its own
config and a 4/8/16 sweep would have measured the default three times. The
schema-v2 workload now seeds a fixed 32-row catalogue and measures 256 bounded
follower reads at concurrency 32. Raw evidence records every latency,
p50/p95/p99, local-versus-Authority call counts, and the configured pool;
campaign evidence records read p95, write p99, aggregate peak RSS, and fixed
10% write/RSS selection guardrails. The code and deterministic
configuration/storage proofs are delivered, but the retained physical
`4 · 8 · 16` named-host artifact is not: it remains grouped with P0c/P2f and
requires operator authorization to transfer the private runner image to the
four named machines. Until all three hash-bound campaigns exist, production
keeps `4`; this milestone does not claim that a larger pool improves the
measured workload, and no earlier deferred artifact should be read as having
measured one.

### 6.7 P6 — make the fourth machine useful without adding a vote

**Change:** Add an explicit non-voting learner/read-worker role only after P3's
lag gate, P5's voter-grade storage eligibility, and `CLUSTERING-PLAN.md` M4's
remaining separate-process acceptance. That pause/takeover/partition proof is
also a prerequisite for P0c ordinary-load evidence; without it, an undetected
scheduler/provider duplication defect could contaminate the baseline. The role receives replication, serves
readiness-gated local reads and explicitly eligible HTTP/media work, never
becomes leader, and never contributes to quorum. Joining as a voter remains the
default because silently changing an existing token's role would alter
availability.

Learner admission is a new versioned protocol, not an optional field on the v1
voter flow. Use a distinct endpoint plus v2 token prefix/AAD/payload and a v2
`membership.json`; bind the role in the issued token record and derive it on
redeem/finalize. An old coordinator or joiner must reject the v2 flow before it
persists secrets or admits a voter. Effective role always comes from live
committed membership: Hiqlite's `learner_only` startup hint is not a permanent
leadership guard after promotion.

**P6a delivered 2026-08-25.** The distinct issuance/redeem/finalize endpoints,
v2 prefix/AAD/payload, replicated learner-role binding, v2 local membership,
v1 compatibility, and three-voter-plus-learner real-process gate are on `main`.
The explicit security choice retains Hiqlite's shared secrets and treats every
admitted voter or learner as a trusted cluster principal. Later P6 slices must
not reinterpret that trust choice as route or job eligibility: committed role
enforcement and the eligibility matrix remain the next two boundaries.

Publish one route/job eligibility matrix. Learners may run readiness-gated
bounded catalogue reads and declared node-local media work, but never
authority reads locally or any scheduler, migration, membership, provider, or
other leader-singleton job. Report voter replication health and learner
catch-up separately so a lagged learner cannot claim degraded voter
redundancy.

Promotion and removal are explicit admin operations. Promotion requires the P3
watermark/apply catch-up proof, a blocking replication barrier, voter-grade
durable-storage/headroom preflight, and the same crash-safe joint-configuration
reconciliation as voter removal. Learner removal first ejects the target from
routing, activates a generalized admission fence for every eligible HTTP/media
route, and drains current work. It then settles node-local ownership, persists
the durable removal fence, removes the learner node, tombstones the data
directory, and preserves restart refusal without imposing a voter-quorum-size
check. Every stage is idempotent across crashes and retries, and no new
node-local work may appear after settlement. Loss
of every voter still makes authority operations unavailable even if the learner
process is healthy.

The present shared Hiqlite API secret makes every admitted node part of the
membership trust boundary: a learner that holds it can call private membership
routes directly. Either retain and state that all cluster nodes are trusted, or
vendor a separate membership-mutation credential that learners never receive;
the public UI alone is not an authorization boundary.

Activation uses a bridge protocol rather than a one-step version bump: deploy
binaries that support `[4,5]`, actively prove every current voter supports the
learner protocol, commit activation to `5..5`, and only then admit learner
traffic. Removal remains available during degraded rollback. Downgrade first
removes every learner, proves voter-only membership from every voter, and
deactivates the marker when the protocol permits it.

**Acceptance:** a real separate-process three-voter-plus-learner cluster
preserves quorum size three, distributes only bounded reads to the learner,
refuses learner leadership and singleton work, catches up after restart and
snapshot install, removes/drains safely, and promotes only after catch-up and
storage preflight. Mixed-version tests prove an old voter blocks activation and
cannot reinterpret v2 admission as a voter. The UI and operations text
distinguish compute/read capacity, a non-quorum replicated copy, and voting
redundancy.

### 6.8 P7 — close the loop with load-balancer and failure drills

P2 and P7 fulfill and extend the metrics, mixed-version, proxy, and failure
proofs already owned by `CLUSTERING-PLAN.md` M5/M6; they do not create a second
operations contract. Update both plans in the behavior PR, and treat the older
milestone as satisfied only when its original acceptance plus the performance
budget here are both met.

**Change:** Ship Caddy/nginx/Traefik-neutral guidance: probe `/readyz`, keep HLS
and segment requests sticky, cap connection/drain times, and fail over without
retrying unsafe mutations automatically. Extend the M5/M6 cluster validation
harness with an executable local proxy fixture rather than blessing one
vendor's syntax as the application contract.

Record three-voter, three-voter-plus-learner, follower-loss, leader-loss, and
lagged-worker runs. Lock absolute SLOs only from those artifacts.

**Acceptance:** under the §5.2 workload, a follower loss preserves writes, a
leader loss recovers inside the accepted election budget, a lagged learner
leaves rotation, and an HLS session survives one backend loss with the existing
documented discontinuity behavior.

## 7. Pull-request sequence — small diffs, explicit dependencies

### 7.1 Sequence

| PR | Scope | Depends on | May proceed in parallel with |
|---|---|---|---|
| 0 | this plan | functional membership | none |
| 1 | auth activity-write suppression | PR 0 reviewed, not necessarily merged | PR 2 design only |
| 2 | Store/Raft metrics | PR 0 | PR 1 after shared-file conflict check |
| 3 | lag-gated catalogue reads | PR 2 | PR 4 inventory only |
| 4 | replaceable-write coalescers | PR 2 | PR 5 path design |
| 5 | storage roots + measured read pool | PR 2; P3 measurements for pool | PR 4 |
| 6 | non-voting learner/read worker | PR 3; PR 5 storage eligibility; clustering M4 singleton fencing | none |
| 7 | proxy fixture + final failure/SLO record | PRs 3, 5, and 6 | none |

`CLUSTERING-PLAN.md` M4's core implementation landed in #505. Its real-process
singleton pause/takeover proof landed in #523, and the serving-node partition
acceptance landed in #526. Code instrumentation may proceed before the
named-runner baseline, but P0c/P2f acceptance and tuning decisions remain
blocked until the physical evidence is recorded and reviewed.

### 7.2 Existing work and shared-file ownership

Membership PRs #490 and #491 have landed. The adversarial follow-up PR #496
owns their shared artwork/removal corrections in
`crates/plurx-cluster-check/src/lib.rs`, `crates/plurx-core/tests/store_contract.rs`,
`docs/CLUSTERING-PLAN.md`, and `docs/OPERATIONS.md`. P0a/P0b are reconstructed
from #496's reviewed head rather than replaying its superseded commits. P1's
implementation landed as #493 and retains the follow-up gate above. Later
milestones extend the merged M5/M6 contracts rather than copying them into a
second harness or document.

Before opening each PR, compare its path list with every open cluster PR. One
branch owns a shared contract at a time; another either waits, moves an
independent test to a non-overlapping surface, or declares the dependency in
its PR body. A green head from before the dependency merged is not sufficient.

### 7.3 Mixed-version and downgrade matrix

Every behavior PR records supported old/new combinations and exercises them in
the `CLUSTERING-PLAN.md` M6 mixed-version fixture:

| Milestone | Feature gate and old-node behavior | Upgrade order | Downgrade gate |
|---|---|---|---|
| P1-P2 | no schema/wire change; old nodes remain correct but do not coalesce or export new metrics | non-leader nodes, then current leader | unrestricted after disabling dashboards that require the new series |
| P3 | bounded reads stay off unless the serving node and quorum-confirmed watermark source advertise the same protocol feature; old nodes use `Authority` | upgrade all voters, verify feature advertisements, then enable per-node traffic | force the authority-read kill switch cluster-wide before installing an old binary |
| P5 | new paths are node-local config; an omitted field preserves the old root exactly, but a *set* `[storage]` field is not ignored by an older binary — see below | move one non-leader only after its reverse path is proven | move bytes back, then delete `storage.cache_dir` and `storage.transcode_dir` from every config file before installing an older binary |
| P6 | v2 learner admission is refused until an active challenge proves every voter runs the `[4,5]` bridge; old endpoints/joiners reject rather than ignore the role, then activation commits `5..5` | upgrade all voters, prove capability, activate protocol, add a learner, then enable only its eligible traffic | remove every learner and verify voter-only membership from every voter before marker deactivation/downgrade; if activation is irreversible, rollback is a forward fix |
| P7 | proxy behavior keys only on stable readiness/HTTP contracts | upgrade backends before enabling new routing policy | restore the prior routing policy before backend downgrade |

Each PR pins `protocol_min`/`protocol_max` expectations, old-binary startup or
refusal, feature negotiation, upgrade order, and rollback outcome. An older
node may never promote a learner or opt another node into bounded reads by
accident.

**P5 ships two different downgrade behaviors for one feature.** `[storage]`
carries `#[serde(deny_unknown_fields)]` and `[cluster]` deliberately does not.
An operator who sets `storage.cache_dir` or `storage.transcode_dir` and then
installs an older binary gets a hard TOML parse failure on the whole file: the
node does not start, mid-rollback, and the message names an unknown field rather
than a downgrade. Both keys MUST be deleted from every config file — not
emptied, not commented past — before an older binary is installed. Setting
`cluster.read_pool_size` has no such effect, because the lenient `[cluster]`
section already exists to let an older clustering release ignore keys it does
not know. The asymmetry is intentional in the config code but is a rollback
trap in practice, so the reverse-move checklist in
[OPERATIONS.md](OPERATIONS.md) treats key removal as a step, not a tidy-up.

Each behavior PR contains its tests and updates the authoritative operations or
architecture text in the same commit. A branch is cut from current `main` in a
fresh clone, uses the `codex/` prefix, and contains no unrelated cleanup.
GitHub CI is the acceptance environment; do not spend a second full local run
before asking CI the same question. If CI exposes a focused failure, reproduce
only when a local run adds diagnostic value, fix it, push once, and require the
new PR head to pass.

## 8. Guardrails — deliberate non-goals

- **Do not promise linear write scaling.** Raft serializes writes at one leader;
  additional nodes improve availability and application capacity, not the
  number of independent write authorities.
- **Do not make four voters the recommended HA topology.** It tolerates the same
  one failure as three and raises the quorum.
- **Do not route all HTTP traffic to the leader.** That saves a hop by turning
  the leader into the application bottleneck and defeats bounded-replica work.
- **Do not cache authorization merely because catalogue caching worked.** A
  stale poster title is tolerable; a revoked token is not.
- **Do not put the Raft log on shared media storage.** NFS/SMB availability and
  latency must not become consensus durability.
- **Do not expose every OpenRaft knob.** A setting without a measured workload,
  safe range, and cross-node rollout contract is an operational footgun.
- **Do not make sticky sessions mandatory.** They improve cache locality; the HA
  contract must still recover on another node.
- **Do not use benchmarks as tests of correctness.** Deterministic contracts
  reject regressions; benchmark artifacts explain cost and set SLOs.

## 9. Rollout and rollback — optimize one boundary at a time

For each behavior PR:

1. capture the before artifact on the same topology and workload;
2. prove the §7.3 mixed-version row, then deploy to one non-leader only when
   its feature gate permits the combination;
3. confirm readiness, apply lag, and auth/write counters before adding traffic;
4. expand traffic while watching p95/p99, leader changes, lag, and errors;
5. exercise follower loss, then leader loss, before declaring the slice done;
6. roll back the application change without deleting Hiqlite state if any
   correctness or tail-latency gate fails; and
7. preserve both before and after artifacts with the build SHA.

P3 local-read rollout has an additional kill switch: force all replicated reads
back to `Authority` without changing schema or membership. P6 learner rollout
can remove the learner without changing the three-voter quorum. P5 path rollout
requires a documented reverse move before any operator relocates durable data.

## 10. Revisit as the cluster grows

Re-evaluate the design when one of these facts becomes true:

- the operator explicitly requires two simultaneous voter failures — compare
  five voters with three, not four with three;
- authority reads dominate after catalogue reads move local — design an auth
  invalidation protocol rather than lengthening a blind TTL;
- one leader's write CPU saturates after replaceable writes are coalesced —
  revisit data shape and transaction batching before changing durability;
- learner read capacity exceeds voter read capacity — add capacity-aware proxy
  weights while retaining lag-gated readiness;
- snapshot duration dominates write p99 — measure incremental or off-path
  snapshot options without changing disaster-recovery retention; or
- clusters span sites — keep the voting quorum in one low-latency failure
  domain and treat remote copies as learners/backups unless cross-site quorum
  latency is an accepted product requirement.

The plan ends when three voters provide the durability promise, every extra
machine adds measured application capacity without silently raising quorum,
and the metrics can explain the next bottleneck without another architecture
guess.
