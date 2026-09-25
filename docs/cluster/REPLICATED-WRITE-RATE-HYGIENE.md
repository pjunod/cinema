# Replicated write-rate hygiene — stop proposing no-ops every second on every voter

**Status:** in progress — M0 done at Paul's 12-hour gate
([readout](REPLICATED-WRITE-RATE-HYGIENE-M0.md)); M1–M3 built on `plan/K-03`
(#405); the M4 after-measurement needs a fleet deploy ·
**Executes:** S3, F-sc-3 and the takeover-loop
audit from S3's row in
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 for the two loops as they are, then §3 for the four changes, each
of which keeps the property the assessment insists on: the local check is a
hint and the replicated claim stays atomic. Execute §5 in order; M0 is the
number everything after it is judged against. If a step seems to require
delivering from a node that is not a committed voter, dropping the
`UPDATE … RETURNING` claim in favour of a local read, or reading the Monarr
settings once at startup and never again, stop and flag it.

**Correction to the review:** none on the mechanism. One clarification: the
takeover loop's two `get_setting` reads are followed by
`remote_rollout_ready()` ([media_pool.rs:705-730](../../crates/plurxd/src/media_pool.rs)),
which performs three further membership reads under a deadline, so the
per-tick cost with the feature disabled is two consistent reads plus that
short-circuit — the settings reads happen first and are what §3.4 skips.

## 1. Objective

The watched outbox and the session-takeover loop pay Raft or consistent-read
cost only when there is work or when a setting can have changed, with
measured before/after proposal counts, without weakening delivery (a node
with no vote never drains; two voters never deliver the same row) or making
a Curator configured five minutes ago wait longer than a bounded interval.

## 2. Contract today

Re-verify each line at build time.

### 2.1 The outbox drain

```rust
// crates/plurxd/src/watched.rs:188-200
pub async fn run(self: Arc<Self>, authority: Arc<dyn ClusterJobAuthority>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        if !authority.may_run_cluster_jobs().await { continue; }
        self.deliver_due().await;
    }
}
// :202-230
async fn deliver_due(&self) {
    let due = match self.store.due_watched(BATCH).await { .. };   // BATCH = 20 (:39)
    if due.is_empty() { return; }
    let url = self.store.get_setting(keys::MONARR_URL).await…;    // only when due
    let key = self.store.get_setting(keys::MONARR_API_KEY).await…;
    for entry in due { self.attempt(entry, &url, &key).await; }
}
```

Spawned once per process at [main.rs:2375-2377](../../crates/plurxd/src/main.rs)
with `state.membership` as the authority. `may_run_cluster_jobs`
([membership.rs:5034-5042](../../crates/plurx-core/src/cluster/membership.rs))
answers `true` unclustered, `false` under local maintenance, otherwise
"is this node a committed voter" from local applied state — a cheap call,
so every voter ticks every second.

The claim on the replicated backend:

```rust
// crates/plurx-core/src/store/hiqlite_durable.rs:702-722
async fn due_watched(&self, limit: i64) -> Result<Vec<OutboxEntry>, StoreError> {
    let now = self.now()?;
    let claim_until = now.saturating_add(60);
    let sql = "UPDATE watched_outbox SET claim_until = $1 \
             WHERE id IN (SELECT id FROM watched_outbox \
               WHERE status = 'pending' AND next_at <= $2 AND claim_until <= $2 \
               ORDER BY next_at, id LIMIT $3) \
             RETURNING id, payload, attempts, last_error, status, next_at, claim_until";
    validate_sql(sql)?;
    Ok(self.client().execute_returning_map::<_, OutboxRow>(sql, params!(claim_until, now, limit)).await? …)
}
```

`execute_returning_map` is a Raft proposal whether or not any row matches.
Three continuously eligible voters therefore propose 3 × 86,400 = 259,200
entries per day with an empty outbox. That is a count of *proposals*
(commit-index advances, one apply per voter each); hiqlite's WAL batches
fsyncs, so it is not 259,200 independent fsyncs, and the plan does not claim
it is. What it does drive is S2's snapshot cadence: 259,200 ÷ 10,000 ≈ 26
snapshot builds per voter per day for nothing.

The 60 s `claim_until` is what makes two voters safe: whichever proposal
applies first sets the claim, the second sees `claim_until > now` and
selects nothing. `settle_watched` (`:724-`) writes back only `WHERE id = $6
AND claim_until = $7`, so a claim that expired and was re-claimed by a peer
cannot be settled by the first node. The SQLite twin
([sqlite/outbox.rs](../../crates/plurx-core/src/store/sqlite/outbox.rs)) runs
the same statement on the writer mutex once a second — a write-lock
acquisition, no page written when nothing matches.

The enqueue side is gated on `MONARR_WATCHED_SYNC == "1"`
([watched.rs:115-124](../../crates/plurxd/src/watched.rs)); the drain side
reads `MONARR_URL`/`MONARR_API_KEY` only once rows are due, and an unset
URL fails the row permanently ("monarr is not configured", `:233-237`).

### 2.2 The takeover loop

```rust
// crates/plurxd/src/media_sessions.rs:153,155
const TAKEOVER_INTERVAL: Duration = Duration::from_secs(2);
const TAKEOVER_BATCH: usize = 16;
// :4353-4377
loop {
    interval.tick().await;
    let media_pool_enabled = state.store.get_setting(keys::CLUSTER_MEDIA_POOL_ENABLED).await…== Some("1");
    let takeover_enabled  = state.store.get_setting(keys::CLUSTER_SESSION_TAKEOVER_ENABLED).await…== Some("1");
    if !media_pool_enabled || !takeover_enabled || !state.media_pool.remote_rollout_ready().await {
        scan_cursor = None;
        continue;
    }
```

`get_setting` on hiqlite is `query_consistent_map`
([hiqlite.rs:3453-3461](../../crates/plurx-core/src/store/hiqlite.rs)): a
leader round trip that pauses Raft at a quorum-applied point. Two of them
every 2 s on every node, with the feature off by default on the whole fleet.
`get_setting_pair` (`:3476-3495`) already reads two keys in one statement.

### 2.3 The lease everything else uses

`acquire_cluster_job` ([job_lease.rs:261-290](../../crates/plurxd/src/job_lease.rs)):
authority check first, then `coordinator.acquire(resource, 90 s)`, renewed
every 30 s by `ActiveJobLease`; loss self-fences. Resources are enumerated
in `CLUSTER_SINGLETON_RESOURCES`
([state.rs:10282-10288](../../crates/plurxd/src/state.rs)).

### 2.4 What is measured

`plurx_raft_commit_index` (gauge) — its rate is proposals per second.
`plurx_watched_outbox{status}` (gauge, three values,
[system.rs:4852-4856](../../crates/plurxd/src/http/system.rs)).
`plurx_store_operations_total{class,outcome}` counts authority reads and
writes per process.

## 3. Change

### 3.1 Local hint, atomic claim retained

Before `due_watched`, the drain asks a **local** read whether anything can
be due: on hiqlite a `query_map` (local replica, no consensus) of
`SELECT 1 FROM watched_outbox WHERE status = 'pending' AND next_at <= $1
AND claim_until <= $1 LIMIT 1`; on SQLite the same on `with_read`. The
answer is a hint in both directions and the code says so:

- **hint says nothing** → skip the tick. A row enqueued on the leader and
  not yet applied here is picked up on a later tick; `next_at` is seconds
  and the backoff ladder is 5/30/120 s, so a bounded delay is within the
  contract. Bound it explicitly: even when the hint says nothing, run the
  real claim at least once per `HINT_FORCE_INTERVAL = 30 s` so a replica
  that stopped applying cannot silence the outbox forever.
- **hint says something** → run `due_watched` exactly as today. The
  `UPDATE … RETURNING` remains the only thing that claims; two voters whose
  hints both fire still produce one claimant per row.

Local reads on a follower may be stale (CLUSTER-PERFORMANCE-PLAN.md §3.2
"BoundedReplica"). That is acceptable *only* because the hint never
authorizes anything; it decides whether to ask the authority. The census
test in `state::tests` that keeps local reads out of authority paths must
list this call as a hint with that justification.

### 3.2 Skip when unconfigured, refresh on a bounded interval

The drain caches `(MONARR_URL, MONARR_API_KEY)` from one `get_setting_pair`
and refreshes the pair every `SETTINGS_REFRESH = 60 s` (and immediately when
the settings route writes either key on this node — the write handler
already knows; a `Notify` is cheap). While the cached URL or key is empty,
the drain does not run the claim at all — but it still runs the hint, and if
rows are pending under an unconfigured Curator it fails them permanently on
the next configured pass exactly as today, never earlier. The assessment's
requirement is honoured: a Curator configured on another node is seen within
60 s on every node, not at the next restart.

### 3.3 Idle backoff

Tick interval becomes adaptive: 1 s while the last claim returned rows or
the last hint fired; doubling to a ceiling of `IDLE_TICK_MAX = 10 s` after
consecutive empty hints; reset to 1 s by the enqueue path's `Notify` on the
local node and by any non-empty hint. Delivery latency for a locally
enqueued event stays ≤ 1 s; for an event enqueued on another node it is
bounded by the ceiling plus replication lag.

### 3.4 Leader-singleton via the job lease, bounded failover

Only one node needs to drain a cluster-wide outbox. The loop acquires
`acquire_cluster_job("watched:outbox")` and holds it across ticks (a
long-lived `ActiveJobLease`, renewed every 30 s by the existing heartbeat
task); a node that does not win checks again every `LEASE_RETRY = 15 s`.
`acquire_lease` on hiqlite is itself an `execute_returning_map`
([hiqlite_coordination.rs:51](../../crates/plurx-core/src/store/hiqlite_coordination.rs))
— a proposal even when the answer is `Held` — so a non-owner must not retry
it blindly or two idle voters would add 11,520 proposals/day back. The
non-owner first reads the lease row **locally** (`SELECT expires_at_ms FROM
job_leases WHERE resource = $1`, a hint by the §3.1 rule) and calls
`acquire` only when the row is absent or its expiry has passed on this
node's clock; a stale replica delays failover by its lag, never grants
anything. Failover bound: TTL 90 s + retry 15 s + apply lag ≈ 105 s worst
case before a successor drains; against a 5–120 s retry ladder that is
acceptable and is stated in the code. The claim semantics are unchanged:
even if two nodes briefly both believed they held the lease across a leader
change, the `UPDATE … RETURNING` still admits one claimant per row.

The takeover loop gets the same treatment for its *reads*: one
`get_setting_pair(CLUSTER_MEDIA_POOL_ENABLED, CLUSTER_SESSION_TAKEOVER_ENABLED)`,
an "off" answer cached for 60 s and dropped by a local switch write, and the
loop sleeps at `IDLE_TICK_MAX` while either is off. An "on" answer is never
cached (§4): with takeover on, each 2 s tick reads the pair again, so turning
it off stops the scan and CAS on the next tick on every node. It does **not** become a singleton:
every candidate must independently prove eligibility and the store CAS
admits one successor (its doc comment at `:4348-4352`), and takeover latency
when the feature is on stays at the 2 s tick.

### 3.5 Measurement

Before/after, per voter, 12 h each (Paul, 2026-09-25; the plan said 24 h):
Δ`plurx_raft_commit_index` (proposals per
day), `plurx_store_operations_total{class="authority_read"}` rate,
`plurx_store_operations_total{class="write"}` rate, snapshot builds per day
from `plurx_raft_snapshot_seconds_count{operation="build"}`. Expected: the
outbox's share falls from ~86,400 proposals/node/day to ≤ 8,640 (30 s forced
claims on one node) plus real work; the takeover loop's authority reads fall
from ~86,400/node/day to ≤ 1,440. The number that matters is the
commit-index delta on an idle fleet overnight.

New series: `plurx_watched_outbox_ticks_total{outcome="skipped_hint"|
"skipped_unconfigured"|"claimed"|"empty_claim"|"not_owner"}` (five fixed
values) so the skip reasons are visible.

## 4. Guardrails (non-goals)

- **The replicated `UPDATE … RETURNING` claim stays** (F-sc-3, assessment
  S3). No local read ever selects a row for delivery.
- **`may_run_cluster_jobs` stays in front of everything** — a learner or a
  node in maintenance neither hints nor claims nor takes the lease.
- **Settings are Store settings, not environment** (`MONARR_URL` is a
  replicated key); the cache is bounded at 60 s and notify-refreshed, never
  read-once.
- **Proposal counts are reported as proposals**, never as fsyncs.
- **The takeover CAS is not made a singleton** and its 2 s cadence when
  enabled is unchanged; only its disabled-state settings reads are cached.
- **No change to `claim_until` (60 s), `BATCH`, or the backoff ladder** —
  they are the delivery contract with Curator.
- **No new settings key.** Constants are documented in code; nothing here
  needs a switch.

## 5. Milestones

### 5.1 M0 — baseline

Twelve hours of the four series in §3.5 from the three voters with an empty
outbox, attached to the M1 PR. (The plan said twenty-four; deploys reset the
capture every few hours and no 24-hour window ever completed, so Paul set the
gate to 12 hours on 2026-09-25.)

Acceptance: the PR body carries per-node proposals/day and authority
reads/day, with the commit-index delta.

```text
GPT prompt (fleet): On the three voters, with no viewers, record over 12 h the
increase of plurx_raft_commit_index, the rate of
plurx_store_operations_total{class="authority_read"} and {class="write"},
plurx_raft_snapshot_seconds_count{operation="build"}, and
plurx_watched_outbox{status="pending"}. Repeat after <M1..M4 sha> is
deployed and report both tables side by side.
```

Read-only discovery on 2026-09-20 found that the old lab names no longer
describe the live voter set: `192.168.4.7` reports itself as the learner,
while `nuc4` (`192.168.4.8`), `m6` (`192.168.4.14`) and `nynuc`
(`192.168.5.236`) report themselves as the three voters. No historical
Prometheus-compatible endpoint was exposed on those nodes' standard ports,
and the supplied deployment key was refused by all four hosts, so no
node-local history could be inspected.

A persistent read-only `/metrics` capture started at 2026-09-21T03:23:20Z. It samples
the three voters every 60 s and starts the acceptance window only when all
three are reachable, remain voters on one build, report zero pending outbox
rows, and report no transcode, Live TV or protected-playback activity. It
resets the window on activity, reachability, build or role change, or a
counter rollback. `m6` reported one active transcode at launch, so the
continuous 24-hour window had not started yet. The sampler deploys nothing
and performs only unauthenticated `GET /metrics` reads.

**Result (2026-09-25).** The sampler is now in the repo as
`scripts/replicated-write-capture-sampler` and its evaluator as
`scripts/replicated-write-capture`, both at the 12-hour gate. Replaying the
capture finds 57 idle windows; one qualifies — 2026-09-22T03:36:36Z to
19:39:03Z, 16.04 h, 956 samples. Over it the cluster proposed **902,512
entries a day** (10.45/s), each voter paid ≈ 1.07 million authority reads and
built 91 snapshots a day. An attribution of 25,170 contiguous replicated-log
entries (a read-only copy of the learner's WAL) puts the watched-outbox claim
at 28.9% of all proposals, behind a `metadata-classification` lease cycle
(41.6%) and ahead of the idle offline-package claim (14.0%) — both outside
this plan and flagged in the readout. Full numbers:
[REPLICATED-WRITE-RATE-HYGIENE-M0.md](REPLICATED-WRITE-RATE-HYGIENE-M0.md).

### 5.2 M1 — hint + forced claim + settings cache + idle backoff (`watched.rs`)

§3.1–§3.3 in one PR; they share the tick loop.

Acceptance: `cargo test -p plurxd watched::` with paused time: an enqueued
row is delivered within 1 s on the enqueuing node; a row visible only in the
authority (hint stubbed to "nothing") is claimed within 30 s; an unconfigured
Curator produces zero claims and the settings pair is re-read at 60 s; after
20 empty ticks the interval is 10 s and a `Notify` returns it to 1 s; the
existing two-node contract in `make cluster-store-check` still admits one
claimant per row.

### 5.3 M2 — `watched:outbox` singleton

Add the resource to `CLUSTER_SINGLETON_RESOURCES`; hold the lease across
ticks; `LEASE_RETRY = 15 s`.

Acceptance: `cargo test -p plurxd watched::singleton_` : two processes over
one SQLite store — one drains, the other logs `not_owner`; kill the owner's
heartbeat, the other drains within TTL + retry; `state::tests` enumeration
green. `make cluster-daemon-check` unchanged.

### 5.4 M3 — takeover-loop settings cache (`media_sessions.rs`)

Acceptance: `cargo test -p plurxd media_sessions::takeover_loop_` : with
both keys off the loop performs one settings read per 60 s and never calls
`expired_media_sessions`; flipping the key locally wakes it within one tick;
with both on, cadence is 2 s and the existing takeover contract tests in
`make cluster-check` are unchanged.

### 5.5 M4 — after-measurement and doc

Re-run §5.1's readout; update
[CLUSTER-PERFORMANCE-PLAN.md](CLUSTER-PERFORMANCE-PLAN.md) §6.5's retained
recurring-write inventory with rows for the outbox claim and the takeover
poll.

Acceptance: the before/after table in the PR body shows the outbox's
proposals/day below 10,000 per cluster and the takeover loop's authority
reads below 1,500 per node per day on an idle fleet.

## 6. Verification and rollout

Fast lane: `make unit` per PR plus the named filters. M1 and M3 are safe on
a mixed fleet (per-process behaviour). M2 introduces a lease resource that
older binaries do not contest; during a rolling deploy an old node keeps
draining at 1 Hz while new nodes defer to the lease — duplicate delivery is
still prevented by the claim, so the order does not matter. Rollback is a
redeploy.

## 7. Decisions

The coordinator recorded the delegated decisions on 2026-09-20:

1. Use `HINT_FORCE_INTERVAL = 30 s` and `IDLE_TICK_MAX = 10 s`, inside the
   review's approved 5–10 s idle-backoff range.
2. Keep the singleton lease on standalone SQLite. It preserves one-drainer
   semantics across multiple processes, and its lease cost is negligible
   beside the 1 Hz empty claims being removed.
3. Keep the settings-write `Notify` local to the watched and takeover loops.
   A general notification facility would expand K-03 beyond its plan.

Paul decided on 2026-09-25:

4. The M0 (and M4) capture gate is **12 hours**, not 24.

The executing session (claude-opus-5-5) read two lines of §3 as follows and
records them here so review can overrule them:

5. §3.2's unconfigured case. An idle drain with no Curator URL or key makes
   no claim at all, not even the 30 s forced one. When rows *are* waiting
   (the local hint sees them, or this node just enqueued one), the drain
   re-reads the settings pair on the authority first and then claims exactly
   as before, so a still-unconfigured Curator fails them permanently ("monarr
   is not configured") and one configured on another node since the cached
   read receives them. Rows are never failed on a cached answer.
6. §3.3's "≤ 1 s for a locally enqueued event" holds on the lease owner.
   With M2, a row enqueued on a voter that does not own `watched:outbox` is
   delivered by the owner within its idle ceiling (10 s) plus replication
   lag — the bound §3.3 already states for an event enqueued on another node.
7. The takeover loop keeps §3.4's fail-closed read: an unreadable switch is
   off for that tick and the next tick reads again. Only "off" is cached
   (§4); an "on" is re-read every tick. (The first M3 build cached "on" for
   60 s as well, which let a local disable go unheeded for up to a minute;
   the PR #405 review caught it and it was changed to match §4.)

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M0 | #405 | Read-only discovery found the live three-voter set is `nuc4`, `m6`, and `nynuc`; the old `lab1`–`lab3` names are stale and `192.168.4.7` is now the learner. No historical metrics endpoint was found and node SSH refused the supplied key. A persistent 60 s `/metrics` sampler started at 2026-09-21T03:23:20Z and will complete only after a continuous 24-hour idle, empty-outbox, stable-build/role and monotonic-counter window; it was waiting because `m6` had one active transcode. The coordinator approved the 30 s forced claim, 10 s idle ceiling, unchanged SQLite singleton lease, and local-only notifications. No Rust was changed. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M0 | #405 | **Unblocked by Paul's 12-hour gate.** `5010892f3` puts the sampler (`scripts/replicated-write-capture-sampler`, 43,200 s, append-safe) and its evaluator (`scripts/replicated-write-capture`) in the repo with `tests/operations/test_replicated_write_capture.py`; the running Mac copy's `capture.sh` was replaced in place (new inode) with the same 12-hour, append-safe script and the sampler was not stopped. Replaying all 16,783 sample lines (2026-09-21T03:23:20Z – 2026-09-25T01:25:21Z) finds 57 idle windows, one qualifying: 2026-09-22T03:36:36Z – 19:39:03Z, 16.04 h. `52c03014c` records the readout: 902,512 proposals/day cluster-wide, ≈ 1.07 M authority reads and 91 snapshot builds/day per voter; the learner-WAL attribution puts the outbox claim at 28.9% of entries, a `metadata-classification` lease cycle at 41.6% and the idle offline-package claim at 14.0% (both flagged, out of scope). The sampler's last sample is 01:25:21Z and nothing was written by 09:15Z; its launchd state was not readable from the agent workspace. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1, M2 | #405 | `b12e18636`. Local outbox and lease-expiry hints on both backends; `plurx_core::store::watched_drain` (hint + 30 s forced claim + 60 s settings pair cache + 1→10 s backoff, woken by local enqueue and settings writes); the `watched:outbox` singleton lease with 15 s local-read retry; `plurx_watched_outbox_ticks_total{outcome}`. Three-voter `store_contract`: an idle configured minute proposes 2 claims (was 60), an unconfigured minute 0, a lease hint 0 against an acquire's 1. plurxd: 1 s local delivery, ≤ 30 s forced claim, 60 s settings refresh, 10 s backoff, two-drainer failover within TTL + retry with zero non-owner acquires. Ten production-hunk reverts each fail their test. Decisions 5–6 in §7 record how §3.2 and §3.3 were read. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 | #405 | `3763395d1`. Takeover switches read as one pair cached for 60 s; 10 s idle sleep while off, woken by a local switch write; 2 s cadence and CAS unchanged when on. Paused-clock tests: 10–11 reads in ten minutes off and no inventory tick; a local flip acted on within one tick; 2 s cadence with ≤ 2 reads in two minutes on. Reverting the cache or the wake fails them. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 (review) | #405 | Review P2: the first M3 gate cached "on" too and only listened for the local write while off, so a local disable took up to 60 s (the reviewer measured 29 more acting ticks over 68 s). Now only "off" is cached, keyed on a local write generation; "on" is re-read every tick, so a disable is seen on the next 2 s tick on every node, and a wake left over from a write made while on costs no read. New paused-clock tests: `takeover_loop_stops_acting_within_one_tick_of_a_local_disable`, `takeover_loop_sees_a_remote_disable_on_the_next_tick`, `takeover_loop_ignores_a_wake_left_over_from_a_write_made_while_on`; the cadence test now asserts one pair read per enabled tick. The Curator settings-route comment now states the 60 s bound for a write on a non-owner. Merged main at `b47c5ff88`. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 | #405 | **needs: fleet** — the after-measurement runs only on a deployed build; steps below. The CLUSTER-PERFORMANCE-PLAN §6.5 rows wait for its numbers. |

### needs: M4 fleet after-measurement (GPT)

```text
GPT prompt (fleet, K-03 M4). After the merge commit carrying K-03 M1–M3
(branch plan/K-03, PR #405) is deployed with the usual ansible playbook to
nuc4, m6, nynuc and nuc3:
1. On each voter, `curl -s http://<ip>:32400/metrics | grep
   plurx_watched_outbox_ticks_total` must list five outcomes. Over five
   minutes exactly one voter's claimed+empty_claim+skipped_hint grows; the
   other two grow only not_owner (every 15 s). Report which voter owns it.
2. Start a fresh capture: copy scripts/replicated-write-capture-sampler to
   ~/code/plurx-agent/workspaces/k03-m4-<UTC stamp>/capture.sh on the Mac and
   run it under launchd or nohup (read-only; it exits 0 after one qualifying
   12-hour idle window). Do not touch the M0 workspace
   k03-m0-20260921T032002Z.
3. When status.txt says complete, run
   `scripts/replicated-write-capture evaluate <dir>/samples.tsv --json` and
   report, per voter, proposals/day, store writes/day, authority reads/day and
   snapshot builds/day beside docs/cluster/REPLICATED-WRITE-RATE-HYGIENE-M0.md
   §2. Also report the owner's plurx_watched_outbox_ticks_total claimed +
   empty_claim delta over the same window scaled to a day.
4. Copy (read-only, cp) the learner nuc3's /srv/plurx/hiqlite/logs/*.wal to
   /tmp and run `scripts/replicated-write-capture attribute <copies>`; report
   the `UPDATE watched_outbox` and `job_leases ... watched:outbox` shares.
Acceptance (§5.5): outbox proposals (claims + watched:outbox lease writes)
< 10,000/day per cluster; authority reads per voter down by ≈ 85,000/day.
```
