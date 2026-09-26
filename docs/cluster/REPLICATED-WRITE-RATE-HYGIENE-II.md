# Replicated write-rate hygiene II — the classification lease, the offline claim and the offline expiry sweep

**Status:** in progress — M1–M2 merged
([#540](http://192.168.4.7:3000/noirr/plurx/pulls/540), `4b112204b`); M4 (the
offline expiry sweep, §3.4) built on `plan/K-10-2`; M3's fleet
after-measurement needs a deploy · **Executes:** the two loops K-03's M0
readout flagged outside its scope, and the expiry sweep §3.3 left for a
follow-on
([REPLICATED-WRITE-RATE-HYGIENE-M0.md](REPLICATED-WRITE-RATE-HYGIENE-M0.md) §3,
§5) · **Written:** 2026-09-25 against `plan/K-03` @ `7b7c11f35` (#405, not yet
on `main`) · **Board:** K-10 on the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md)

K-03 took the watched-outbox claim (28.9% of replicated entries) off the idle
cluster. Its M0 attribution found two larger or comparable loops it was not
scoped for: the `metadata-classification` lease cycle, **41.6%** of all
proposals, and the idle offline-package claim, **14.0%** — together ≈ 500,000
of the ≈ 902,000 proposals a day an idle three-voter cluster makes. This plan
applies K-03's hygiene ([REPLICATED-WRITE-RATE-HYGIENE.md](REPLICATED-WRITE-RATE-HYGIENE.md) §3)
to both, and reuses its two primitives rather than adding parallel ones:
`CoordinationStore::lease_expiry_hint` and
`job_lease::acquire_cluster_job_with_policy`. It is stacked on K-03's branch
for that reason.

The same rule governs every change here: **a local read is a hint, never an
authorization.** It decides only whether to ask the authority. The replicated
`UPDATE … RETURNING` claim and the replicated lease stay the only things that
bind work to a node.

## 1. Objective

On an idle cluster, neither loop proposes on a timer except for a bounded
forced check; under load both do the same work as today. No offline package
and no classification entry is lost, processed by two nodes, or delayed past
the bounds in §4; lease handover after a node dies still happens, within a
stated bound.

## 2. Premises, re-verified against the code

Line numbers are at `7b7c11f35`.

### 2.1 The classification worker

- `library_search::worker` ([library_search.rs:46-78](../../crates/plurxd/src/library_search.rs))
  runs on every node. Per iteration: the voter gate (`:51`), then
  `store.acquire_lease("metadata-classification", <random uuid>, now, now+120 s)`
  (`:55-58`), one 32-entry `classify_page` (`:62`), then `release_lease`
  (`:66-69`). It sleeps **1 s** while its cursor is mid-library and 30 s after
  a pass completes (`:76`).
- The cursor is per process, so each voter walks the whole library itself;
  the lease only serialises pages. A node that loses the race sleeps 1 s and
  tries again.
- `acquire_lease` on hiqlite is `execute_returning_map`
  ([hiqlite_coordination.rs:51](../../crates/plurx-core/src/store/hiqlite_coordination.rs)):
  a proposal whether it wins or not; a loss adds two consistent reads
  (`:71`, `:84`). `release_lease` is `execute` (`:160`), a second proposal.
  So an idle library costs up to 2 proposals a second from the winner plus
  one per loser — the readout's 23.7% acquire + 17.9% renew/release.
- `classify_page` ([library_search.rs:79-171](../../crates/plurxd/src/library_search.rs))
  writes an entry only when it is not `unchanged` (`:138-143`): unindexed, no
  record, source JSON or `classification::VERSION` changed, or a provider
  check was made. A provider check is made when TMDB is configured, the kind
  is `movie`/`show`, it has a TMDB id and the last check is older than 30
  days (1 h after an error) (`:126-127`), at most 4 per page. Writes are CAS
  fenced on `revision` and the source JSON
  ([classification.rs `write_sql`](../../crates/plurx-core/src/store/classification.rs)),
  so a duplicate writer cannot overwrite a newer record.
- The page read is `query_consistent_map`
  ([hiqlite_classification.rs:16-26](../../crates/plurx-core/src/store/hiqlite_classification.rs))
  and each page re-reads the TMDB key (`:93-97`): two authority reads per
  page, not proposals.
- The lease is taken directly on the store with a random owner id, not
  through `acquire_cluster_job`, so it is missing from
  `CLUSTER_SINGLETON_RESOURCES` ([state.rs:12618](../../crates/plurxd/src/state.rs)).

### 2.2 The offline claim

- `OfflineManager::run` ([offline.rs:438-520](../../crates/plurxd/src/offline.rs))
  runs on every node. Every `IDLE_POLL` = **2 s** (`:20`) it reads
  `offline.enabled` on the authority (`enabled()`, `:577-589`, a consistent
  read), checks the voter gate and restart admission, then calls
  `claim_next_offline_package(node_id)` (`:491`).
- On hiqlite the claim is `UPDATE offline_packages … RETURNING`
  ([hiqlite_durable.rs:2172-2208](../../crates/plurx-core/src/store/hiqlite_durable.rs)),
  `execute_returning_map`: a proposal whether or not a package is queued.
  Three voters × 43,200 = 129,600 a day, the readout's 127,000.
- The claim is **per node**: `WHERE candidate.node_id = $2 AND state =
  'queued'`. Nothing is contested between nodes; the only other node that can
  make a package `queued` for this node is node removal re-homing it
  (`resolve_offline_packages_for_removal`, `:2983-3040`), and a disable/enable
  (`disable_offline_packages`, `:2142`).
- Packages are created on the node that received the request with
  `node_id: state.node_id` ([http/offline.rs:408](../../crates/plurxd/src/http/offline.rs)),
  so the normal enqueue is always local.
- The claim SQL itself refuses while `offline.enabled` is off.
- Index `offline_packages_queue(node_id, state, created_at)`
  ([hiqlite_durable.rs:145](../../crates/plurx-core/src/store/hiqlite_durable.rs))
  serves a local "anything queued for me?" read.

### 2.3 The offline expiry sweep (M4)

Line numbers are at `main` @ `91f363154`.

- `OfflineManager::run_with_claims`
  ([offline.rs:516-538](../../crates/plurxd/src/offline.rs)) runs the sweep in
  the same loop as the claim: at start and then whenever 60 s
  (`Duration::from_secs(60)`, `:532`) have passed on `std::time::Instant`,
  it calls `expire_offline_packages(now)` (`:529`) with the wall clock in
  seconds, and never sleeps past the next sweep (`:538`). The loop runs on
  every node, a learner included: the expiry half is deliberately ungated
  ([main.rs:2573](../../crates/plurxd/src/main.rs), started at `:2720`).
- On hiqlite the sweep is a two-statement `txn`
  ([hiqlite_durable.rs:2794-2822](../../crates/plurx-core/src/store/hiqlite_durable.rs)):
  delete the cache-consumer pins of packages with `expires_at <= $1` (by
  package id, and by the package's download-lease token), then
  `DELETE FROM offline_packages WHERE expires_at <= $1`. One Raft proposal
  whether or not anything has lapsed. Four nodes × 1,440 a day ≈ 5,800
  proposals a day (4,320 with the learner stopped).
- Both statements match only through a package with `expires_at <= now`: when
  no such package exists the whole `txn` changes nothing. SQLite is the same
  transaction ([sqlite/offline.rs:993-1011](../../crates/plurx-core/src/store/sqlite/offline.rs)).
- `expires_at` is seconds, set to now + 7 days on create and on every renew
  or lease ([http/offline.rs:33, :389, :505](../../crates/plurxd/src/http/offline.rs)),
  so a lapse is rare and a renewal only ever moves it later.
- `offline_packages` has no index on `expires_at`
  ([hiqlite_durable.rs:145-150](../../crates/plurx-core/src/store/hiqlite_durable.rs));
  the table holds one row per offline package, so a local scan is cheap.
- The sweep shares the claim loop, whose `claim_pass` awaits a whole
  preparation (`self.prepare(…)`, `offline.rs:608`): a node busy preparing
  does not sweep until it finishes. Other nodes sweep meanwhile; on a
  single-node install the sweep waits. Pre-existing, unchanged here (§7.8).
- Open PR [#532](http://192.168.4.7:3000/noirr/plurx/pulls/532)
  (`codex/durable-cluster-work`, head `9d0f3f5fb`) moves offline
  *preparation* onto a shared durable-work queue. It does not touch
  `expire_offline_packages`, `offline_queue_hint` or `crates/plurxd/src/offline.rs`
  (its only offline change is 8 lines in `http/offline.rs`); package expiry
  stays a domain-table sweep there. M4 does not overlap it.

## 3. Design

### 3.1 Offline claim (M1): hint, forced claim, backoff, local wake

A policy object `plurx_core::store::offline_claim::OfflineClaimPolicy`,
shaped like K-03's `WatchedDrain` and driven by both the daemon loop and the
three-voter contract:

- **Hint.** New `OfflinePackageStore::offline_queue_hint(node_id)`: `SELECT 1
  FROM offline_packages WHERE node_id = $1 AND state = 'queued' LIMIT 1` on
  the local replica (`query_map` on hiqlite, `with_read` on SQLite). No
  proposal, no consistent read.
- **Order.** The hint runs first. Only when it fires, or a forced claim is
  owed, does the loop read `offline.enabled`, check the voter gate and
  restart admission, and claim — so an idle node also stops the 2 s settings
  read.
- **Forced claim.** Even with a silent hint, claim at least once per
  `HINT_FORCE_INTERVAL` = **30 s** (the K-03 value). A fresh process trusts no
  hint until it has claimed once.
- **Backoff.** 2 s (today's `IDLE_POLL`) while working or hinted; doubling to
  `IDLE_TICK_MAX` = **10 s** on consecutive silent hints; never sleeping past
  the forced-claim deadline.
- **Wake.** A package created on this node for this node (`Created` in
  `http/offline.rs`) calls `OfflineManager::wake()`: the next pass runs at
  once and claims whatever the replica shows.
- **After work.** A claimed package forces the next pass (the queue may hold
  more; this node's own requeue may not be applied locally yet).
- **Gate refused** (disabled, not a voter, restart pending): scheduled as an
  empty claim — back off, next forced check in 30 s — and counted as `gated`.
- Metric `plurx_offline_claim_ticks_total{outcome}`, four fixed values:
  `skipped_hint`, `gated`, `claimed`, `empty_claim`.

### 3.2 Classification (M2): one lease per pass, local pass hint

A schedule object `plurx_core::store::classification_schedule::ClassificationSchedule`
decides, per tick, whether this node should run a pass; `library_search`
owns the lease and the pages.

- **Hold the lease across the pass.** The worker takes
  `metadata-classification` through `acquire_cluster_job_with_policy` (voter
  gate, node id as owner, TTL 90 s, heartbeat 30 s via `ActiveJobLease`),
  walks every page at the unchanged 1 s cadence while it holds it, and
  releases once at the end. A lost lease (`loss_token`) stops the pass after
  the current page; the cursor is kept. The resource joins
  `CLUSTER_SINGLETON_RESOURCES`.
- **Non-owners read the lease row locally.** Before contesting, the worker
  reads `lease_expiry_hint("metadata-classification")`; a row that is visibly
  live costs nothing and is looked at again after `LEASE_RETRY` = **15 s**.
  Only an absent or expired row is worth an `acquire_lease` proposal.
- **Pass hint.** New `ClassificationStore::classification_hint(now)`: one
  local `SELECT EXISTS` over the same `items ⟕ media_classifications` join
  `classification_page` reads, true when any entry is unindexed, has no
  record, a different source JSON, a different `VERSION`, or is due a
  provider check under exactly `classify_page`'s rule (TMDB key set,
  `movie`/`show`, TMDB id, 30 days / 1 h after an error). A pass starts only
  when the hint fires (and this node's own `gap` since its last pass has
  passed) or a forced pass is owed.
- **Forced pass.** At least once per `FORCE_PASS_INTERVAL` = **30 min** a pass
  runs whatever the hint says. The deadline counts from the latest pass
  anywhere in the cluster: the lease row's expiry (a release sets it to the
  release time) is a local, cluster-wide record of the last pass, so nodes do
  not take turns re-walking a library a peer has just walked. A fresh process
  forces one pass unless it sees a peer's pass after its own start.
- **Idle backoff.** `gap` starts at **30 s** (today's between-pass sleep).
  Only a pass the hint started that wrote nothing and made no provider
  request doubles it, up to the forced interval — the case of a hint that
  keeps firing for nothing. A pass with work resets it, and so does any
  decision that reads the hint silent: the false firing it backed off from
  has ended, so the next firing is new work and is owed the idle ceiling. A
  forced pass that found nothing leaves it alone; on an idle library the
  forced passes are the only passes, and letting them double the gap held the
  next change for up to 30 min on a node with no peer to take the hinted pass
  (#540 review, finding 1). The idle hint is polled every 30 s, backing off
  to `IDLE_TICK_MAX` = **2 min**.
- **Wake.** There is no single local enqueue for classification — items are
  written by scans, metadata refreshes, provider jobs and edits on any node —
  so the local hint polled at ≤ 2 min stands in for it. This is the one
  hygiene step that does not transfer literally; §4 states the bound it gives.
- **Mixed fleet.** Old binaries take the same row with a random owner and a
  120 s expiry; the row is exclusive whichever version holds it, so a rolling
  deploy cannot run two passes' pages at once, and an old node keeps its
  churn until it is upgraded.

### 3.3 Not in scope

- The offline expiry sweep was out of scope for M1–M2 and is M4 (§3.4).
- The classification pages' two authority reads per page (§2.1); they fall
  with the pass count, but are not the target.
- No change to `claim_next_offline_package`'s SQL, the classification write
  fence, the lease TTL/heartbeat, or `classify_page`'s per-entry rules.

### 3.4 Offline expiry sweep (M4): hint, forced sweep

A policy object `plurx_core::store::offline_expiry::OfflineExpiryPolicy`,
shaped like M1's `OfflineClaimPolicy` and driven by both the daemon loop and
the three-voter contract:

- **Hint.** New `OfflinePackageStore::offline_expiry_hint(now)`: `SELECT 1
  FROM offline_packages WHERE expires_at <= $1 LIMIT 1` on the local replica
  (`query_map` on hiqlite, `with_read` on SQLite) — the sweep's own
  predicate. No proposal, no consistent read.
- **Order.** A pass runs every `SWEEP_TICK` = **60 s** (today's cadence). The
  hint comes first; only when it fires, or a forced sweep is owed, does the
  pass run the unchanged replicated sweep. A forced pass skips the hint. A
  failed hint counts as "may have lapsed" and sweeps.
- **Forced sweep.** Whatever the hint says, sweep at least once per
  `FORCED_SWEEP_INTERVAL` = **10 min**, and on a fresh process's first pass
  (as today). A failed sweep leaves the forced sweep owed.
- **No early expiry.** The replicated sweep still decides with the
  authority's rows and the same `expires_at <= now`. A replica that has not
  applied a renewal can only make the hint fire for nothing — the sweep then
  deletes nothing.
- **Clock.** The pass schedule moves to Tokio's clock (it was
  `std::time::Instant`), so paused-clock tests follow it; `now` for the
  predicate stays the wall clock, as today.
- Metric `plurx_offline_expiry_ticks_total{outcome}`, three fixed values:
  `skipped_hint`, `swept`, `empty_sweep`.
- **No wake.** A lapse is a time, not a write, so there is nothing to wake
  on; the 60 s pass is the bound.

## 4. Bounds

| Case | Today | After |
|---|---|---|
| Offline package created on its own node | ≤ 2 s | next pass, at once (wake) |
| Offline package made `queued` for this node elsewhere (re-home, re-enable) | ≤ 2 s | ≤ 10 s + apply lag (hint); ≤ 30 s if the hint is wrong (forced) |
| Offline gate flips (enabled, promoted) | ≤ 2 s | ≤ 10 s with a package visibly queued, else ≤ 30 s |
| New or changed item classified | ≤ 30 s + walk to it (one pass) | ≤ 2 min + apply lag + walk to it; ≤ 30 min + walk if the hint is wrong |
| Provider re-check due | at the next pass that reaches it | same, within the bound above |
| Classification owner dies mid-pass | next page by a peer (≤ 1 s) | TTL 90 s + retry 15 s + apply lag ≈ 105 s, resumed by a peer |
| Offline package lapses (M4) | expired < 60 s after `expires_at` by the first sweeping node | same: < 60 s + apply lag (hint); < 10 min + 60 s if every replica's hint is wrong (forced) |
| Offline package not yet lapsed (M4) | never expired | never expired: the authority's `expires_at <= now` decides |

The M4 bounds hold for a node whose offline loop is not inside a preparation
(§2.3, last-but-one bullet): on a cluster another node sweeps meanwhile, and
the forced sweep on the preparing node runs as soon as its preparation ends.

No work item is lost: every queued package stays `queued` until a claim on
the authority takes it, and every stale entry keeps the hint true until a
write clears it. No item is processed twice: the offline claim is unchanged
and per node; classification writes stay CAS-fenced, and the lease still
admits one pass at a time.

**Proposals on an idle three-voter cluster (per day).**

| Loop | M0 | After, expected |
|---|---|---|
| Offline claim | ≈ 127,000 | 3 × 2,880 = 8,640 forced claims |
| `metadata-classification` lease | ≈ 376,000 | one acquire + release per forced pass (48/day) plus 30 s renewals while a pass runs; for a library of *n* items a pass is *n*/32 s, so ≈ 96 + 2,880 × min(1, *n*/57,600) |
| Together | ≈ 503,000 (56%) | < 12,000 |
| Offline expiry sweep (M4) | ≈ 5,800 (4 nodes × 1,440) | 4 × 144 = 576 forced sweeps, plus one per lapse per node that sees it |

## 5. Milestones

### 5.1 M1 — offline claim

Acceptance: three-voter `store_contract` — an idle minute of the pre-change
loop (a blind claim every 2 s) measured at 30 proposals, the policy's minute
at 2 (forced at 0 s and 30 s), 0 consistent reads, one local read per pass;
a local wake claims on the next pass. Both backends: the queue hint follows
the authority for its own node only. `cargo test -p plurxd
offline::tests::claim_` (paused clock): an idle 10 minutes makes 20–21
claims (was 300); a locally created package is claimed within one base tick;
a package queued for this node elsewhere (re-home, re-enable), with no wake,
within the 10 s ceiling; and, over a lagging replica whose hint stays silent,
a woken package is still claimed within one base tick
(`claim_a_wake_claims_what_the_local_replica_does_not_show_yet`). `http::offline`:
a created package wakes this node's worker and an idempotent retry does not
(`a_created_package_wakes_this_nodes_offline_worker`).
`plurx_core::store::offline_claim` unit tests: a hint that stays silent is
overruled at exactly 30 s. Reverting the hint, the forced claim, the wake's
forced claim (`policy.wake()`) or the `create` call site fails a test.

### 5.2 M2 — classification lease and pass hint

Acceptance: three-voter `store_contract` — the pre-change loop's idle minute
measured at 120 proposals for one worker; the schedule's idle minute at 0 for
the owner after its pass and 0 for a non-owner facing a held lease (15 local
lease reads), where a blind contest is one proposal per try. Both backends:
the hint is false for a fully classified library and true for each of the
six conditions `classify_page` acts on. `cargo test -p plurxd
library_search::classification_`: a multi-page pass acquires and releases
the lease once (was once per page); an idle library runs no page and takes
no lease for 10 minutes; a new item is classified within the idle ceiling,
also after six idle hours of forced passes; a peer's dead lease is taken over
within TTL + retry and the pass resumes; `state::tests` enumeration green.
`classification_schedule` unit tests: twelve idle forced passes then a change
60 s later is passed within the idle ceiling; a forced pass that found nothing
does not grow the gap; a silent hint ends a false firing's backoff. Reverting
the lease hold, the lease hint, the pass hint or either gap rule fails a
test.

### 5.3 M3 — fleet after-measurement

Deployed readout with the same gate as K-03 M0/M4; the attributed shares of
`metadata-classification` and `UPDATE offline_packages` must fall below 2%
each. Needs a fleet (GPT, steps in the Execution log). Once M4 is deployed
the same readout also reports `DELETE FROM offline_packages`/`cache_consumer_pins`
sweep entries (§5.4).

### 5.4 M4 — offline expiry sweep

Acceptance: three-voter `store_contract`
(`an_idle_offline_expiry_sweep_proposes_only_its_forced_sweeps_on_three_voters`)
— an idle half hour of the pre-change loop (a blind sweep every 60 s)
measured at 30 proposals, the policy's half hour at 3 (forced at 0, 10 and
20 min), 0 consistent reads, one local read per unforced pass (27); then a
package lapsing 50 s after a forced pass is expired by the next, unforced
pass. Both backends
(`offline_expiry_hint_matches_the_sweep_and_nothing_expires_early_or_late`):
the hint's boundary is the sweep's (`expires_at - 1` silent and the sweep
deletes nothing, `expires_at` fires), and over 20 virtual minutes of passes
two packages (lapsing at 90 s and 1,000 s) are each present at every pass
before their expiry and absent at every pass from it on. `cargo test -p
plurxd offline::tests::expiry_` (paused clock): 30 idle minutes make 3–4
sweeps (was 30); a package that lapsed while the worker idled is expired
within one 60 s tick and one that has not is kept.
`plurx_core::store::offline_expiry` unit tests: an idle hour sweeps 6 times
with no hint read on a forced pass; a hint sweeps on the same pass; a silent
hint is overruled at exactly 10 min; a failed hint sweeps. Reverting the
hint (forced sweep only), ignoring it (sweep every pass), the forced sweep,
or either backend's hint query fails a test.

## 6. Rollout

Both changes are per process and safe on a mixed fleet (§3.2 *Mixed fleet*;
the offline claim is per node). Rollback is a redeploy. No settings key, no
feature gate.

## 7. Decisions

Taken by the executing session (claude-opus-5-5) within the brief; review can
overrule them.

1. Stack on `plan/K-03` to reuse `lease_expiry_hint` and
   `acquire_cluster_job_with_policy` rather than duplicate them on `main`.
2. `FORCE_PASS_INTERVAL` = 30 min for classification, not K-03's 30 s: a
   forced pass is a whole-library walk of authority reads, and the pass hint
   mirrors `classify_page`'s rules (pinned by a parity test), so the forced
   pass only covers a stale replica or a future drift.
3. Offline keeps K-03's 30 s forced claim and 10 s ceiling; its base cadence
   stays 2 s.
4. Classification gets no wake hook (§3.2); the 2 min idle ceiling bounds it.
5. M4 is its own milestone on a new branch (`plan/K-10-2`) of the same plan,
   not a new plan: it is the follow-on §3.3 named, with the same shape.
6. M4 keeps the sweep's 60 s cadence and its SQL. The hint is the sweep's own
   predicate, so the bound on a lapse the replica shows is unchanged; only
   the proposals for nothing go.
7. `FORCED_SWEEP_INTERVAL` = 10 min, not the claim's 30 s: a lapse is a time
   every replica computes from rows it already holds, so the hint is wrong
   only on a replica that stopped applying, and on a cluster any other node's
   hint still fires. 10 min keeps a whole-cluster hint failure bounded well
   inside a 7-day package life at 144 forced sweeps a day per node.
8. M4 leaves the sweep inside the offline loop, so a node busy preparing still
   does not sweep until it finishes (§2.3). Moving it to its own task would
   also change single-node behaviour mid-preparation; that is a separate
   decision, not write-rate hygiene. No `expires_at` index (§2.3).

---

## Execution log

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | plan | #531 | `1173194ce`. Plan written; premises re-verified at `7b7c11f35`; board row K-10 claimed. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1, M2 | #531 | `170dc84b2`. Local hints `offline_queue_hint` and `classification_hint` on both backends; `plurx_core::store::offline_claim` (hint + 30 s forced claim + 2→10 s backoff + local wake) driving `OfflineManager::run`; `plurx_core::store::classification_schedule` (lease-row and pass hints, 30 min forced pass counted from the last pass anywhere, 30 s→2 min idle polls) driving a worker that holds `metadata-classification` through `acquire_cluster_job_with_policy` for a whole pass; the resource joins `CLUSTER_SINGLETON_RESOURCES`; `plurx_offline_claim_ticks_total{outcome}` and `plurx_classification_ticks_total{outcome}`. **Three-voter idle minute, measured:** offline 30 → 2 proposals (0 consistent reads, 8 local reads); classification 120 → 0 (0 consistent reads, 2 decisions × 2 local reads), and a node facing a held lease 0 proposals over 4 local looks where a blind contest is 1 per try. plurxd (paused clock): one lease for a 4-page pass (was one per page); an idle library takes no lease and reads no page for 10 min; a new item classified within 2 min + its pass; two workers on one store walk the library once (every entry revision 1); a dead peer's lease is not contested while live and is taken over when it lapses. Offline: 20–21 claims in 10 idle minutes (was 300), a local creation claimed within one tick, a package queued elsewhere within 10 s. Revert proofs, each failing its tests: offline hint ignored; forced claim removed; wake branch removed; wake not forcing with a silent hint; SQLite queue hint forced silent; lease taken per page; lease-row hint ignored; pass hint forced true; a peer's released row not counted as a pass. The hint parity contract caught a real defect before commit: SQLite numbers `$N` parameters by first appearance, so the first draft bound the version and the clock the wrong way round and the hint always fired. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | merge + gates | #531 | `ff3cb7789` merges `plan/K-03` at `e3687722b` (main `c625dee1f`): Store method count 394, rolling-producer rows re-measured (637, 999), and the `/metrics` format string given its nineteenth placeholder after an auto-merge left 18 for 19 arguments. Gates at the merged head: history-check, validation-lint, validation unittests, operations-check, spike-lock-check, fmt, clippy `-D warnings` all exit 0; `cargo test -p plurx-core -p plurxd` passes every unit and integration target (1291 + 148 + 2932) and exited 101 only on the `plurx-core` doctest target, which could not find the `hiqlite` rlib in the shared target directory — rerun alone, exit 0. `make cluster-store-check` exits 2 with the same 17 failures, by name, as `plan/K-03` at `e3687722b` (one `populated_v14…` parity count and sixteen `subtitle_*` cases on a missing `subtitle_source_publications` table); the four K-10 three-voter cases pass. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | review round | #540 | Adversarial review (comment 5220), two findings. `f86a24e13` merges main at `42ea7a9af` (conflicts: Store method count 395 = main's 393 + K-10's two hints; `/metrics` keeps main's priority-class exposition and K-10's classification ticks, format string 20 placeholders for 20 arguments; rolling-producer time-constructor row measured 1004), built and tested before any change (fmt, clippy, `cargo test -p plurx-core -p plurxd` all 0). **Finding 1 (P1), fixed** in `eb7524c82`: idle forced passes doubled the classification gap to 30 min, so a change on an idle single-node install waited up to 30 min, not the §4 ≤ 2 min; the gap now doubles only after a hinted pass that found nothing, and a silent hint reading resets it (§3.2). Pinned by `classification_schedule::tests::a_change_after_idle_forced_passes_is_passed_within_the_idle_ceiling` (the reviewer's reproduction), `a_forced_pass_that_found_nothing_does_not_grow_the_gap`, `a_silent_hint_ends_the_backoff_a_false_firing_grew` and `library_search::tests::classification_a_change_after_idle_forced_passes_waits_only_the_idle_ceiling`; reverting both hunks fails all four, each hunk alone fails its own unit test (proved). **Finding 2 (P2), addressed** in `2a6ff26d2`: the earlier revert proof "wake not forcing with a silent hint" was a combined mutation, not a standing test, and no plurxd test failed with `policy.wake()` removed. `OfflineManager::run` now delegates to `run_with_claims` (production passes its own store); `offline::tests::claim_a_wake_claims_what_the_local_replica_does_not_show_yet` runs the worker over a lagging-replica stub whose hint stays silent and fails with `policy.wake()` replaced by `{}` (proved; `claim_a_locally_created_package_wakes_the_worker` still passes under that revert, as the review found), and `http::offline::tests::a_created_package_wakes_this_nodes_offline_worker` fails with the `create` call site removed (proved). Rolling-producer rows measured: task-spawn 639, time-constructor 1009 (all new shapes test-only). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 plan | #555 | Branch `plan/K-10-2` from `main` @ `91f363154`. §2.3 premises re-verified at that commit (sweep loop, `txn` shape, predicate, TTL, missing `expires_at` index, the preparation-blocking caveat). Open PR #532 (`codex/durable-cluster-work` @ `9d0f3f5fb`) checked: it moves offline *preparation* to a shared queue and does not touch `expire_offline_packages`, `offline_queue_hint` or `crates/plurxd/src/offline.rs`, so M4 proceeds without overlap. §3.4, §4 rows, §5.4 and decisions 5–8 added; board row K-10 updated. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 | #555 | `d25206f42`. `OfflinePackageStore::offline_expiry_hint` on both backends (the sweep's own `expires_at <= now`, local replica, no proposal); `plurx_core::store::offline_expiry::OfflineExpiryPolicy` (60 s passes, hint first, 10 min forced sweep, fresh-process and failed-hint sweeps) driving the offline worker's expiry pass on Tokio's clock; `plurx_offline_expiry_ticks_total{outcome}`. **Three-voter idle half hour, measured:** 30 → 3 proposals, 0 consistent reads, 27 local reads; then a package lapsing 50 s after a forced pass is expired by the next, unforced pass. Both backends: hint and sweep agree at the boundary, and over 20 virtual minutes two packages are present at every pass before their expiry and gone at the first pass from it (never early, < 60 s late). plurxd (paused clock): 30 idle minutes make 3–4 sweeps (was 30); a lapsed package is expired within one 60 s tick and a live one kept. Revert proofs, each failing its tests: hint never trusted (forced only) — 3 of 4 policy unit tests, both contract tests and plurxd `expiry_a_lapsed_package_is_swept_within_one_tick_and_a_live_one_is_kept`; hint ignored (sweep every pass) — 3 unit tests and the three-voter count; forced sweep removed — 2 unit tests and the three-voter count; SQLite hint forced silent — the both-backend contract; hiqlite hint forced silent — both contract tests; daemon bypassing the policy — `expiry_an_idle_worker_sweeps_only_on_the_forced_interval`. Rolling-producer rows measured: task-spawn 641, time-constructor 1012 (all new shapes test-only). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 fleet | #555 | **needs: fleet** — M4's after-measurement joins M3's readout once deployed; steps below (item 4). |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 | #531 | **needs: fleet** — the after-measurement runs only on a deployed build; steps below. |

### needs: M3 fleet after-measurement (GPT)

```text
GPT prompt (fleet, K-10 M3). After the merge commit carrying K-10 M1–M2
(branch plan/K-write-rate-2, PR #531; it is stacked on K-03 #405, so both
land together) is deployed with the usual ansible playbook to nuc4, m6,
nynuc and nuc3:
1. On each voter, `curl -s http://<ip>:32400/metrics | grep -E
   'plurx_(offline_claim|classification)_ticks_total'` must list four offline
   and five classification outcomes. Over five minutes on an idle fleet,
   offline claimed+empty_claim grows by about 10 per voter (one per 30 s) and
   skipped_hint by more; classification hinted_pass+forced_pass grows on at
   most one voter, the others grow idle or not_owner.
2. Reuse the K-03 M4 capture (or start one the same way): after a qualifying
   12-hour idle window, run `scripts/replicated-write-capture evaluate
   <dir>/samples.tsv --json` and report proposals/day beside
   docs/cluster/REPLICATED-WRITE-RATE-HYGIENE-M0.md §2.
3. Copy (read-only, cp) the learner nuc3's /srv/plurx/hiqlite/logs/*.wal to
   /tmp and run `scripts/replicated-write-capture attribute <copies>`; report
   the shares of `metadata-classification` lease writes and `UPDATE
   offline_packages` claims.
Acceptance (§5.3): each of those two shares is below 2% of replicated
entries on the idle fleet; report the cluster's proposals/day.
4. (K-10 M4, once the merge carrying plan/K-10-2 is deployed to all four
   nodes.) On each node, `curl -s http://<ip>:32400/metrics | grep
   plurx_offline_expiry_ticks_total` must list skipped_hint, swept and
   empty_sweep. Over 30 idle minutes, swept+empty_sweep grows by 3 per node
   (one per 10 min) and skipped_hint by about 27. In the step-3 attribution,
   report the share of `DELETE FROM cache_consumer_pins … offline_package`
   plus `DELETE FROM offline_packages WHERE expires_at` entries; acceptance:
   ≤ 150 a day per node when nothing lapses (was 1,440).
```
