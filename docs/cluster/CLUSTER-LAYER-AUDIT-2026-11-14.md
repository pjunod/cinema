# Cluster layer audit — what the quorum costs, who goes around it, and what to change

**Status:** open — findings and action items; nothing here is built ·
**Owner:** Claude (Fable 5.2), session `session_01BCWLA3JD1HCNMEid3AZrc3` ·
**Verified against:** `main @ 8785492be` ·
**Fleet measured:** 2026-11-14 20:32–20:34 UTC (media1, lab6, lab4 voters;
lab3 learner) · **Written:** 2026-11-14

> **Who reads this.** Paul: §1 for the verdict, §8 for the rulings only you
> can make. This is the review, not a build plan. Each action item in §7 you
> approve gets its own plan document (to Astra for review, then to a build
> agent); the P0 items are small enough to go straight to a build agent.

Companion to [README.md](README.md) (the index of everything already written
about the cluster) and
[RAFT-READ-CONFIRM-DIAL-RCA-2026-11-14.md](RAFT-READ-CONFIRM-DIAL-RCA-2026-11-14.md)
(the last incident). Those documents each diagnose one failure. This one asks
the question underneath them: is there a cluster layer, is it used, and does
the cluster pay for itself.

How it was done: six Sonnet sub-agents swept the source (bypass inventory,
peer transport, background loops, request paths, Raft configuration and fork,
shared libraries); the load-bearing claims were then re-read in source and
checked against live `/metrics` from all four nodes. A claim marked **†** was
read by a sub-agent and not re-verified by me. Everything else was.

## 1. Verdict

The Raft cluster itself is sound. What sits on top of it is not a layer.

1. **There is no cluster layer, there are parts of one.** Feature code can
   reach thirteen cluster handles on `AppState`, six different "what is my
   role" answers, eight different peer rosters and three raw Raft handles,
   and it assembles its own conclusions from them. About 25 sites outside the
   fence do their own cluster reasoning. The shared-library bug was this
   pattern, and it is still the pattern in that code after the fix.
2. **The store makes a quorum round trip the default read.** A `Store` read
   on a replicated node is linearizable unless the call site opted in to one
   of three separate local-read mechanisms. Few did. On the fleet, 93 % of
   store reads are quorum reads.
3. **The measured cost is roughly ten times the designed idle cost.** With
   almost no user traffic the fleet ran ~210 quorum reads/s and 38 Raft
   proposals/s. The source-derived idle floor is ~22 reads/s and under one
   proposal/s. More than half of the log is one defect: background workers
   proposing job claims that are refused, forever.
4. **Brittleness comes from three habits,** not from Raft: every component
   polls replicated state with quorum reads because nothing adopted the
   change feed; a failed read is treated as a revocation or an outage; and
   timeouts were chosen per call site, so they contradict each other.

The fix is a design change, not another round of spot repairs: one cluster
facade that answers a small fixed set of questions, a store whose default
read is the applied replica under the serving proof, one change feed, and one
peer call path. §6 describes it; §7 orders the work so the cheap, large wins
land first.

## 2. What the fleet is doing right now

Two `/metrics` scrapes 64 s apart on every node. User traffic in the window
was under 4 requests/s per node.

| Node | Role | Quorum reads/s | Local reads/s | Proposals/s |
|---|---|---|---|---|
| media1 | voter, leader | 62.7 | 12.0 | 4.1 |
| lab6 | voter | 40.6 | 3.9 | 3.2 |
| lab4 | voter | 52.7 | 4.7 | 14.4 |
| lab3 | learner | 54.6 | 3.9 | 16.0 |
| **Cluster** | | **~210** | ~25 | **38** (commit index: 38.0/s) |

**How to read it.** The quorum-read total matches the 223 SYN/s the dial RCA
counted on the leader, so that RCA's read rate was not a burst, it is the
steady state. Patch 28 removed the dial from each of those reads; it did not
remove the reads. Each one is still a separate confirmation round to every
voter (§4.1). The commit index has passed 130 million entries.

What the reads and writes are, where the metrics can say:

| Source | Rate | Evidence |
|---|---|---|
| Refused job claims (proposals) | ≥ 21/s of the 38 | lab4 `subtitle_extract` 7.5/s + `dv_title_scan` 2.2/s; lab3 7.5/s + 4.0/s; every one `claim_refused` |
| `sharing_read` on media1 | 20 reads/s | against ~1.3 requests/s on that route group |
| Fence sampler + offline-probe poll | 16 reads/s | two 500 ms loops on four nodes (source) |
| `get_setting` on peer-facing cluster routes | ~6 reads/s | 2.2/s on each of lab6 and lab4 |
| Not attributable | ~60 % of quorum reads | only HTTP-attributed store calls carry a method label |

## 3. Design findings — the layer that isn't one

### D1. Feature code assembles cluster answers from raw parts

`MembershipManager` (`crates/plurx-core/src/cluster/membership.rs`, 14,520
lines) is handed whole to feature code as `state.membership`. Its public
surface includes six local-role answers with different costs (one of them a
quorum read), eight peer directories with different eligibility rules
(`activity_peers`, `media_peers`, `operations_peers`,
`reachable_peer_http_urls`, `clock_peers`, …), and raw Raft metrics handles.
`ServingFence` lives in a different crate. A feature that needs "may I serve,
and who can help" combines both by hand.†

The same hand-built predicate — `peers.len() + local_is_voter ==
voter_count` — exists in `shared_cache.rs:395-421` and
`media_pool.rs:1540-1565`. Both carry a comment about the learner bug it once
had. The bug was fixed twice because the logic exists twice.† Seven sites in
`http/live_tv.rs` each fetch `activity_peers` and filter it themselves.†
`http/cluster_operations.rs` and `desktop_control.rs` each compute their own
"is it safe to take this node down" from quorum arithmetic.†

What does hold: raw Hiqlite access is confined. Outside `plurx-core`'s store
and cluster modules, no production code calls `query_consistent`. The
boundary exists at the SQL level. It is missing one level up, where the
decisions are made.

### D2. The store's default read is a quorum round trip

On the replicated backend a plain `Store` read method is
`query_consistent_map`. Three mechanisms can answer locally instead, each
with its own proof and its own opt-in:

| Mechanism | Covers | Adoption |
|---|---|---|
| `CatalogueReader` (bounded replica) | 16 catalogue/watch reads | `get_item` 3 call sites of 29, `get_file` 3 of 46, `watch_state` 0 of 8 † |
| `AppliedView` | settings, session routes, the create planning snapshot | ~25 pinned call sites † |
| Local hints + `SettingsChangeSource::Applied` | a few background gates | 3 consumers † |

The consistent-read census pins 374 call sites with a ceiling of 237
unannotated. It stops growth; it was never going to produce shrinkage,
because the cheap path is the one a new call site gets by not thinking.

Consequences on real paths (all †, from source):

- Every authenticated request: one quorum read for the token
  (`authenticate_token_for`, `store/hiqlite.rs:7351`; I verified this one).
  No cache.
- Home: five parallel requests, 12–20 quorum reads.
- One progress beat: about five quorum reads, two of them duplicates.
- One relayed segment: a quorum read on the owner for every segment
  (`RouteReadSite::Verdict`, `http/internal_media_sessions.rs:901-908`; verified).
- Playback create: 8–12 quorum reads and three proposals.

### D3. Nothing adopted the change feed, so everything polls with quorum reads

Patch 27 added the applied mark, and `AppliedView` keeps per-table revisions
from it. Three consumers use it. Everything else that needs to notice a
change polls, and polls through the leader. The source says so in as many
words (`http/shared_library.rs`, the body monitor): "replicated revocation
has no change feed, so authority is re-observed while this body is alive" —
once a second, per open stream.

The standing pollers, cluster-wide on four nodes (†, except where noted):

| Loop | Period | Cost per tick | Rate |
|---|---|---|---|
| `offline_source_probe_loop` (`membership.rs:12682`, verified) | 500 ms | 1 quorum read, "nothing to do" | 8/s |
| Quorum watermark sampler (`migration.rs:5585`, verified) | 500 ms | 1 leader confirmation | 8/s |
| Playback cache ceiling (`playback_cache.rs`, verified) | 1 s while in use | 1 quorum `get_setting` | ≤ 4/s |
| `media_sessions::lease_loop` | 3 s | 1 quorum read, even with no sessions | 1.3/s |
| Sharing run + claim loops | 5 s | 2 quorum reads, even with sharing off | 1.6/s |
| Background lanes, subtitle worker, Live TV config | 5–60 s | 2–5 quorum settings reads | ~3/s |
| `peer_status_cache_loop` | 3 s | HTTP to every peer, new client each time | 4 HTTP/s |

The K-03 and K-10 write-hygiene work did land: the watched outbox, offline
claim and classification writers that made up 85 % of the idle log at M0 are
fixed in source. What replaced them at the top of the log is §4.2.

### D4. Shared libraries still prove their own authority

After the fix, on `main`:

- `authority()` (`http/shared_library.rs:98-140`) asks membership for the
  sharing member floor on every request; that is a quorum read. The body
  monitor repeats it, plus three more quorum reads, every second per open
  stream (verified).
- Every sharing store read goes through one helper, `sharing_read`
  (`store/hiqlite_sharing.rs:84`), which is always linearizable (verified).
- `observe_source_admission_members` costs four quorum reads and runs three
  times in one Source Start.†
- A scope proof is a fresh pinned-TLS dial plus ~16 quorum reads across both
  clusters, uncached by recorded design; one poster needs two.†
- The serving fence is consulted at exactly one sharing call site. The
  sharing peer listener is not behind the serving gate at all.†
- **A read error is treated as a revocation.** The listener polls `enabled()`
  each second and tears down every body on one failed or slow read; the body
  and receiver monitors retire the session as Revoked the same way. Sharing's
  own 1 s timeouts sit inside the store's 5 s leader-recovery retry, so that
  retry can never help. Local HLS survives the same blip through
  `SessionGrace`; sharing never calls it.†

This is why shared playback dies on events that local playback rides out.

### D5. One signing type, many transports, and a quorum read per peer request

`PeerTransport` is the one signing wrapper, which is good. But it is
constructed about six times as long-lived state and about thirteen times per
call, each with a private connection pool.† `peer_status_cache_loop` builds a
new one every 3 s on every node (`http/cluster_operations.rs:1787`,
verified). There is no connect timeout: a blackholed peer consumes the
caller's whole deadline.†

On the receiving side, every mutation-class peer request runs a quorum read
to prove the sender is a live member (`membership.rs:9719`, verified). Only
three read families use the 1 s cached proof. Response-verified calls pay a
second quorum read on the sender.† All of it shares a 64/s global
pre-verification cap, and a refusal for rate, replay-cache or store error is
answered 401 — indistinguishable from a bad signature.†

A peer is "reachable" if it heartbeated within 30 s. A node that just died is
therefore offered work, and waited on for a full deadline, for up to 30 s:
2 s on page fan-outs, 8 s per holder on serial index and subtitle hydrates.†

### D6. One slow voter can take a shared feature down

The shared-cache canary joins on every voter with a 3 s timeout, and any one
failure revokes the shared cache on the node that asked.† That is fate
sharing in the wrong direction: the cluster makes that feature less available
than a single node would be.

## 4. Efficiency findings

### 4.1 Every quorum read is its own round, and followers forward the query

OpenRaft 0.9's `ensure_linearizable` sends one empty AppendEntries to each
voter per call. Hiqlite calls it per `query_consistent`, discards the read
index, and a follower forwards the whole query to the leader to execute.†
Nothing coalesces: a hundred concurrent reads are a hundred rounds. So read
cost scales with call count, and the leader does every node's read work.

### 4.2 More than half the Raft log is refused job claims

A claim is a replicated transaction. When its guard refuses, the worker moves
to the next candidate and proposes again (`background_jobs.rs:2948`). With
429 `subtitle_extract` jobs queued and nothing admitting them:

| Node (uptime) | Kind | Claims proposed | Accepted |
|---|---|---|---|
| lab6 (14.6 h) | `subtitle_extract` | 155,873 | 7 |
| lab6 | `dv_title_scan` | 73,617 | 6 |
| lab6 | `library_scan` | 3,482 | 5 |
| lab6 | `encoded_output_prepare` | 2,705 | 2 |
| lab6 | `fragment_index_build` | 1,598 | 0 |

On lab6 that is 54 % of every proposal the node made. The learner does it
too. I did not trace which guard refuses; whichever it is, the decision is
made inside the replicated transaction and nowhere before it.

### 4.3 The rest

- Auth, Home, progress and relay costs: D2.
- Quality-catalog fan-out to every peer runs up to three times per create,
  uncached, while a 10 s snapshot of the same peers already exists.†
- Three independent N² probe loops (peer status 3 s, media pool 10 s, clock
  10 s) run on every node whether or not anyone is looking.†
- 60 % of quorum reads cannot be attributed from metrics. That is why this
  took an audit instead of a query.

## 5. Robustness findings

### R1. A stalled voter can depose a healthy leader

Heartbeat 800 ms, election timeout 2.4–4.0 s
(`migration.rs:160-164`). OpenRaft 0.9 has no pre-vote, so a voter starved
for ~2 s campaigns with a higher term and the healthy leader steps down. This
is the probable trigger of the 2026-10-31 failure
([PLAYBACK-QUORUM-GAPS-RCA.md](PLAYBACK-QUORUM-GAPS-RCA.md)), and nothing
structural has changed since.

### R2. Every election fences every node

The serving lease is 1 s, sampled every 500 ms with a 750 ms timeout. An
election takes at least 2.4 s. So any leader change makes all four nodes lose
serving authority; running sessions survive only inside the 5 s session
grace. A 4.0 s timeout plus one vote round is about 5 s. The measured
2,468 ms outage is the good case, not the bound.

### R3. A plain restart of the leader is an unplanned election

Leadership handoff exists (`handoff_leadership`, `membership.rs:8435`) and is
called from maintenance, leave and removal. `scripts/ship` does not enter
maintenance or restart-preparation before restarting a node, and the daemon's
shutdown path does not hand off. So a deploy that restarts the leader takes
R2's outage on every node. That is the opposite of the standing expectation
that a leader restart must not interrupt playback elsewhere. (The Ansible
playbooks are outside the repository; I did not read them.)

### R4. Timeouts contradict each other

| Timer | Value | Conflict |
|---|---|---|
| Read-confirm TTL / sampler timeout | 800 ms / 750 ms | below the 1 s SYN retransmit: one lost SYN on any new dial is a timeout |
| `STORE_TIMEOUT` | 3 s | inside the election window; the fork's 8 s and 14 s leader-recovery budgets are dead code under it † |
| Sharing read timeouts | 1 s | inside the store's 5 s quorum-recovery retry |
| Serving lease / session grace | 1 s / 5 s | vs. elections of 2.4–5 s |
| Peer "reachable" window | 30 s | vs. peer call deadlines of 0.2–8 s |

No one place defines these, so each was correct when written and the set is
not.

### R5. Acknowledged writes are not power-loss durable

The WAL runs `ImmediateAsync`: `msync(MS_ASYNC)`, with the acknowledgement
sent before the flush (`vendor/hiqlite-wal/src/writer.rs:304-316`, verified).
A process crash is safe. A power cut that takes all voters at once — one
house, one breaker — can lose acknowledged entries. That may be an acceptable
trade; it is not a recorded decision.

### R6. The fork carries the design mismatches

28 patches, 18 of them plurx policy; one has an upstream PR.† Patches 4, 6
and 28 each work around something OpenRaft 0.10 provides (leader transfer, a
read-index API, a dedicated heartbeat client). The upgrade is not tracked
anywhere.

### R7. Seen on the fleet, not explained

- media1 logged 840 failed writes (3.4 %) in its first 30 minutes after
  restart; the other nodes have 0–4 in 14 hours.
- lab3 (the learner) is answering ~0.5 settings GETs per second with 5xx.
  Something polls a learner for a route it cannot serve.

## 6. Target design — one facade, six questions

```
 feature code (HLS, Live TV, sharing, jobs, artwork, UI)
        │  may only import `cluster::Cluster`
        ▼
 ┌──────────────────────── Cluster ────────────────────────┐
 │ authority()   may this node serve / commit now; grace   │
 │ role()        voter · learner · standalone · fenced     │
 │ peers(purpose) one directory, cached, typed eligibility │
 │ read(...)     applied by default; linearizable declared │
 │ watch(table)  change feed from the applied mark         │
 │ call(peer, …) one pool, deadlines, typed errors, health │
 └───────┬───────────────┬───────────────┬─────────────────┘
         ▼               ▼               ▼
   serving fence    membership     replicated store
   (quorum proof)   (roster, role)  (Hiqlite / Raft)
```

The rules that make it a layer:

1. **`AppState` exposes `cluster`, not `membership`, `replication`,
   `serving` and the caches.** A source test in the style of the read census
   fails any `state.membership.` or raw metrics handle outside the facade.
   Without the test this decays within a month.
2. **One peer directory, parameterised by purpose** (`Voters`,
   `MediaWorkers`, `Operations`, `Artwork`, `Clock`). Eligibility rules live
   in one table. The directory is cached and invalidated by an applied
   membership change, so `media_peers()` stops being SQL per relayed segment.
3. **Reads are applied-replica by default.** A read is linearizable only
   where the call site says so and says why: a fence, a final verdict, a read
   before a write. `CatalogueReader`, `AppliedView` and the hint readers
   become one mechanism with one proof and per-table floors. This inverts
   today's default and is the single largest change in cost.
4. **One change feed.** Table revisions from the applied mark, exposed as
   `watch(table)`. Settings pollers, revocation monitors and "is there work"
   probes subscribe instead of polling.
5. **One outage policy.** A lost proof is an outage, resolved through
   `SessionGrace`. Only an affirmative read of a revoked grant is a
   revocation. Sharing, Live TV and HLS use the same path.
6. **One peer call path.** One pooled transport on `AppState`, a connect
   timeout, the cached 1 s membership proof for every authorizer, and typed
   outcomes (`refused · rate_limited · unreachable · timeout`). Peer health
   comes from call outcomes, not from a 30 s heartbeat window.
7. **One timing table.** Lease, grace, store timeout and feature timeouts are
   derived from the Raft constants in one module, so they cannot disagree.

**Non-goals.** No new consensus system and no replacement for Hiqlite. No
per-symptom watchdog or retry added anywhere; every item below removes a
cause. No feature gates: where something needs enabling it goes in Settings →
Developer with advisory readiness, per the standing rule. The cross-cluster
sharing client stays a separate trust domain and a separate transport.

## 7. Action items

Ordered by return for effort. P0 items are independent and small; each has an
acceptance check that is one `/metrics` comparison.

### P0 — stop the measured waste

| ID | Change | Fixes | Acceptance |
|---|---|---|---|
| A1 | Never propose a claim the node can already see will be refused: evaluate the guard against applied state first, and hold a refused kind until that state changes. Find and name the refusing guard as step one. | §4.2 | `claim_refused / claim_write` < 5 % per kind; idle commit-index growth < 2/s |
| A2 | Add a `method` label for store operations issued outside HTTP. | §2 | 100 % of quorum reads attributable by method |
| A3 | Token authentication reads the applied replica under the serving proof, with a floor raised by token writes and revocations. | D2 | `authenticate_token_for` authority reads → ~0 on a node with a proof |
| A4 | `offline_source_probe_loop` wakes on an applied change to its table instead of a 500 ms quorum poll. Same for the playback-cache ceiling and the settings pollers in D3. | D3 | source-derived idle floor < 6 quorum reads/s cluster-wide |
| A5 | One `PeerTransport` on `AppState`, with a connect timeout; delete the per-call constructions, starting with `peer_status_cache_loop`. | D5 | `TcpActiveOpens` to peer HTTP ports near zero at idle |
| A6 | Sharing: the member floor comes from one cached cluster answer; body, receiver and listener monitors subscribe to changes and use `SessionGrace`; a read error is never a revocation. | D4 | `sharing_read` ≤ 2 per request, 0 per idle stream-second; a 3 s leaderless window does not end a shared session |
| A7 | Shared-cache canary: a voter that fails the canary is excluded for that node, not grounds to revoke. | D6 | one stopped voter leaves the shared cache serving on the others |

### P1 — build the layer

| ID | Change | Fixes |
|---|---|---|
| B1 | The `Cluster` facade and its source test; migrate callers; delete the duplicated predicates and the seven Live TV filters. | D1 |
| B2 | The single purpose-keyed peer directory, cached on applied membership. | D1, D5 |
| B3 | Flip the store default to applied reads with declared linearizable sites; merge the three local-read mechanisms. Stage it table by table, floors first. | D2 |
| B4 | `watch(table)` as the general change feed; retire remaining pollers. | D3 |
| B5 | One peer call path: cached proof for all authorizers, typed errors end to end, health from outcomes, hedged (not serial) hydrates. | D5 |
| B6 | Placement and quality catalog answer from the peer snapshot the node already holds instead of a fan-out per create. | §4.3 |

### P2 — the Raft layer

| ID | Change | Fixes |
|---|---|---|
| C1 | Coalesce read confirmations in the fork: one round in flight, every reader that arrived before it started shares its result. The fence sampler becomes a reader of the same round. | §4.1 |
| C2 | Hand off leadership on graceful shutdown, and make `scripts/ship` and the playbooks restart through restart-preparation. | R3 |
| C3 | The single timing module; derive lease, grace, `STORE_TIMEOUT` and feature timeouts from it. | R2, R4 |
| C4 | Election behaviour under a starved voter: pre-vote if OpenRaft 0.10 is taken, otherwise a wider floor chosen from a measurement. | R1 |
| C5 | Open a tracked item for OpenRaft 0.10 and retire patches 4, 6 and 28 against it. | R6 |
| C6 | Explain R7's two observations. | R7 |

A1 alone should cut the Raft log by more than half. A1–A6 together should
take the fleet from ~210 quorum reads/s to tens. B3 and C1 are what make that
hold as features are added.

## 8. Rulings needed

1. **WAL durability (R5).** Keep `ImmediateAsync` and record the power-loss
   trade, or move to a real flush and accept slower writes. My pick: keep it
   until A1 lands (write volume is the cost driver), then switch and measure.
2. **Scope-proof caching (D4).** The recorded design is "nothing cached, a
   fresh Source scope request per response". Collapsing two proofs per poster
   into one short-lived proof per import generation contradicts it. This is
   yours to reconcile; I have not assumed either way in A6.
3. **Election floor (C4).** A wider floor means fewer spurious elections and
   slower recovery from a real leader loss. With C2 in place, real leader
   loss is rare, which argues for wider. Needs your call on the trade.
4. **B3, the default flip.** It is the centre of this proposal and touches
   every store consumer. It should go to Astra before anything is built.

## 9. What is sound and should not be disturbed

- The serving fence: store-free, generation-monotonic, never extends a lease.
- `AppliedView`'s rule (proof, generation, floor) — B3 generalises it rather
  than replacing it.
- The durable job claim as the atomic ownership primitive. A1 changes when a
  claim is proposed, not what a claim is.
- Typed retry on playback create, and store-free segment serving on a local
  owner.
- Patch 28, and the write-hygiene work of K-03 and K-10.

## 10. Limits of this audit

- One two-minute fleet sample, taken while a library scan and a deep job
  queue were active and with media1 thirty minutes past a restart. Rates are
  a snapshot. The claim counters are cumulative over 14.6 hours and are not.
- Per-request read counts in D2 and D4 are summed from call sites, not
  traced on a live request.
- Not examined: the Ansible playbooks, the fragment-index cluster timers, the
  Trakt sweep, the owner side of a remote create.
- No code was changed and nothing was deployed.
