# Cluster layer audit — what the quorum costs, who goes around it, and what to change

**Status:** open — findings and action items; nothing here is built ·
**Owner:** Claude (Fable 5.2), session `session_01BCWLA3JD1HCNMEid3AZrc3` ·
**Verified against:** `main @ 8785492be` ·
**Fleet measured:** 2026-11-14 20:32–20:34 UTC (media1, lab6, lab4 voters;
lab3 learner) · **Written:** 2026-11-14 ·
**Reviewed:** one adversarial agent review (Fable), 12 findings, all
addressed in this revision

> **Who reads this.** Paul: §1 for the verdict, §8 for the rulings made.
> This is the review, not a build plan. The P0 items in §7 are small enough
> to go straight to a build agent; the P1 and P2 items each get a plan with
> one adversarial review first.

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
read by one sub-agent and confirmed by neither me nor the adversarial
reviewer. Everything else was confirmed by at least one of us.

## 1. Verdict

The Raft cluster itself is sound. What sits on top of it is not a layer.

1. **There is no cluster layer, there are parts of one.** Feature code can
   reach thirteen cluster handles on `AppState`, six different "what is my
   role" answers, eight different peer rosters and three raw Raft handles,
   and it assembles its own conclusions from them. About 25 sites outside the
   fence do their own cluster reasoning (the handle and site counts are a
   sub-agent's tally †; the role answers and rosters are confirmed). The shared-library bug was this
   pattern, and it is still the pattern in that code after the fix.
2. **The store makes a quorum round trip the default read.** A `Store` read
   on a replicated node is linearizable unless the call site opted in to one
   of three separate local-read mechanisms. Few did. On the fleet, about 90 %
   of counted store reads are quorum reads.
3. **The measured cost is roughly ten times the floor the source implies.**
   In a sample taken with almost no user traffic but a library scan and a
   deep job queue running, the fleet ran ~210 quorum reads/s and 38 Raft
   proposals/s. Reading the source, an idle cluster should need ~22 reads/s
   and under one proposal/s. Most of the proposals are one defect: every
   node proposes a claim for every queued job while the cluster-wide source
   slot is taken, and is refused each time.
4. **The cluster makes background work slower, not faster.** All background
   media work is serialised through a single source reader for the whole
   cluster (§4.2). Four nodes process the 429-job subtitle queue one job at a
   time.
5. **Brittleness comes from three habits,** not from Raft: every component
   polls replicated state with quorum reads because nothing adopted the
   change feed; a failed read is treated as a revocation or an outage; and
   timeouts were chosen per call site, so they contradict each other.

The fix is a design change, not another round of spot repairs: one cluster
facade that answers a small fixed set of questions, a store whose
reads declare whether they need a quorum round, one change feed, and one
peer call path. §6 describes it; §7 orders the work so the cheap, large wins
land first.

## 2. What the fleet is doing right now

Two `/metrics` scrapes 64 s apart on every node. User traffic in the window
was under 4 requests/s per node. The read and write columns are
`plurx_store_operations_total` by class. That counter sits on the store's
client only: the membership manager reads through its own Hiqlite client
(`membership.rs:4723-4741`), so its 55 quorum-read sites — the peer
authorizer, the sharing member floor, source admission — and the fence
sampler are **not** in these numbers. The true quorum-read rate is higher.

| Node | Role | Quorum reads/s | Local reads/s | Proposals/s |
|---|---|---|---|---|
| media1 | voter, leader | 62.7 | 12.0 | 4.1 |
| lab6 | voter | 40.6 | 3.9 | 3.2 |
| lab4 | voter | 52.7 | 4.7 | 14.4 |
| lab3 | learner | 54.6 | 3.9 | 16.0 |
| **Cluster** | | **~210** | ~25 | **38** (commit index: 38.0/s) |

**How to read it.** The counted total is the same order as the 223 SYN/s the
dial RCA saw from the leader to one voter, so that RCA's read rate was not a
burst, it is the steady state. Patch 28 removed the dial from each of those reads; it did not
remove the reads. Each one is still a separate confirmation round to every
voter (§4.1). The commit index has passed 130 million entries.

What the reads and writes are, where the metrics can say:

| Source | Rate | Evidence |
|---|---|---|
| Refused job claims | ≥ 21 proposals/s of the 38, plus ≥ 21 quorum reads/s | lab4 `subtitle_extract` 7.5/s + `dv_title_scan` 2.2/s; lab3 7.5/s + 4.0/s; every one `claim_refused`. Each attempt is a quorum pre-read then a proposal (§4.2) |
| `sharing_read` on media1 | 20 reads/s | against ~1.3 requests/s on that route group |
| Offline-probe poll | 8 reads/s | one 500 ms loop on four nodes (source) |
| Fence sampler (not in the counter) | 8 confirmations/s | one 500 ms loop on four nodes (source) |
| `get_setting` on peer-facing cluster routes | ~6 reads/s | 2.2/s on each of lab6 and lab4 |
| Not attributable | ~60 % of counted quorum reads, and all membership reads | only HTTP-attributed store calls carry a method label |

## 3. Design findings — the layer that isn't one

### D1. Feature code assembles cluster answers from raw parts

`MembershipManager` (`crates/plurx-core/src/cluster/membership.rs`, 14,520
lines) is handed whole to feature code as `state.membership`. Its public
surface includes six local-role answers with different costs (one of them a
quorum read), eight peer directories with different eligibility rules
(`activity_peers`, `media_peers`, `operations_peers`,
`reachable_peer_http_urls`, `clock_peers`, …), and raw Raft metrics handles.
`ServingFence` lives in a different crate. A feature that needs "may I serve,
and who can help" combines both by hand. (The reviewer counts nine rosters,
`membership.rs:8556-8971`.)

The same hand-built predicate — `peers.len() + local_is_voter ==
voter_count` — exists in `shared_cache.rs:395-421` and
`media_pool.rs:1540-1565`. Both carry a comment about the learner bug it once
had. The bug was fixed twice because the logic exists twice. Seven sites in
`http/live_tv.rs` each fetch `activity_peers` and filter it themselves
(lines 147, 194, 794, 1092, 1852, 1952, 2018).
`http/cluster_operations.rs` and `desktop_control.rs` each compute their own
"is it safe to take this node down" from quorum arithmetic.†

What does hold: raw Hiqlite access is confined. Outside `plurx-core`'s store
and cluster modules, no production code calls `query_consistent`. The
boundary exists at the SQL level. It is missing one level up, where the
decisions are made.

### D2. The store's default read is a quorum round trip

On the replicated backend most plain `Store` read methods are
`query_consistent_map`; a handful are deliberately local (`search_items`,
`items_with_artwork`, `classification_hint`, advisory job listing). Three
mechanisms can answer the rest locally instead, each with its own proof and
its own opt-in:

| Mechanism | Covers | Adoption |
|---|---|---|
| `CatalogueReader` (bounded replica) | 16 catalogue/watch reads | by `state.catalogue.` vs `state.store.` call sites: `get_item` 3 vs 13, `get_file` 3 vs 30, `watch_state` 0 vs 8 |
| `AppliedView` | settings, session routes, the create planning snapshot | ~25 pinned call sites † |
| Local hints + `SettingsChangeSource::Applied` | a few background gates | 3 consumers † |

The consistent-read census pins 374 call sites with a ceiling of 237
unannotated. It stops growth; it was never going to produce shrinkage,
because the cheap path is the one a new call site gets by not thinking.

Consequences on real paths (per-request counts are †, summed from call
sites):

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
- `observe_source_admission_members` costs about four quorum reads
  (`membership.rs:3632` onward) and runs three times in one Source Start.
- A scope proof is a fresh pinned-TLS dial plus ~16 quorum reads across both
  clusters, uncached; one poster needs two.†
- The serving fence is consulted at exactly one sharing call site
  (`http/shared_source_ingress.rs:1160`). The sharing peer listener is not
  behind the serving gate at all.†
- **A read error is treated as a revocation.** The body monitor turns an
  error or timeout into a revoke (`shared_library.rs:615-643`, `:770-778`).
  The listener polls `enabled()` each second and tears down every body on
  one failed or slow read, and the receiver monitor retires the session as
  Revoked the same way.† PR #1598 (open, another session) fixes the listener
  case. Sharing's
  own 1 s timeouts sit inside the store's 5 s leader-recovery retry, so that
  retry can never help. Local HLS survives the same blip through
  `SessionGrace`; sharing never calls it.†

This is why shared playback dies on events that local playback rides out.

### D5. One signing type, many transports, and a quorum read per peer request

`PeerTransport` is the one signing wrapper, which is good. But it is
constructed six times as long-lived state and thirteen times per call, each
with a private connection pool (`http/peer_transport.rs:53-61`). `peer_status_cache_loop` builds a
new one every 3 s on every node (`http/cluster_operations.rs:1787`,
verified). There is no connect timeout: a blackholed peer consumes the
caller's whole deadline.

On the receiving side, every mutation-class peer request runs a quorum read
to prove the sender is a live member (`membership.rs:9719`, verified). Only
three read families use the 1 s cached proof. Response-verified calls pay a
second quorum read on the sender.† All of it shares a 64/s global
pre-verification cap (`membership.rs:1106`), and a refusal for rate, replay-cache or store error is
answered 401 — indistinguishable from a bad signature.†

A peer is "reachable" if it heartbeated within 30 s. A node that just died is
therefore offered work, and waited on for a full deadline, for up to 30 s:
2 s on page fan-outs, 8 s per holder on serial index and subtitle hydrates.†

### D6. One slow voter can take a shared feature down

The shared-cache canary joins on every voter with a 3 s timeout, and any one
failure revokes the shared cache on the node that asked
(`shared_cache.rs:28`, `:439-457`). The canary exists to prove every voter
sees the same mount, so an affirmative mismatch must keep revoking; the
defect is that an unreachable voter counts the same as a mismatch. That is fate
sharing in the wrong direction: the cluster makes that feature less available
than a single node would be.

## 4. Efficiency findings

### 4.1 Every quorum read is its own round, and followers forward the query

OpenRaft 0.9's `ensure_linearizable` sends one empty AppendEntries to each
voter per call. Hiqlite calls it per `query_consistent`, discards the read
index (`vendor/hiqlite/src/query/mod.rs:29`), and a follower forwards the
whole query to the leader to execute (`client/query.rs:287-314`).
Nothing coalesces: a hundred concurrent reads are a hundred rounds. So read
cost scales with call count, and the leader does every node's read work.

### 4.2 One source reader for the whole cluster, and every node spinning on it

Two defects, one visible through the other.

**The cluster admits one background source reader at a time.** The claim
statement (`CLAIM_SQL`, `plurx-core/src/store/background_jobs.rs:513-688`)
refuses a job with no live-playback interest while any other maintenance
reader holds an overlapping `source_io` key (`:612-688`). Every job requires
a `source_io` key, and with no storage domain mapped the key is the generic
one that overlaps everything (`background_jobs_resources.sql:9-21`). On the
fleet `plurx_background_resource_reservations{resource_class="source_io"}`
was 1 on every scrape, `plurx_background_source_recovery_pending` was 0, and
the subtitle queue moved from 2 to 5 succeeded in half an hour. So this is
not the stuck-custody incident PR #1597 fixed earlier in the day; it is the
design working as written. Four nodes share one reader.

**Every node proposes a claim for every queued job anyway.** Workers list
candidates from the local replica with a filter that has none of the capacity
predicates (`background_jobs.rs:4232-4290`). For each candidate `claim_job`
makes a quorum pre-read (`:4826-4829`), which can refuse without proposing
only for a missing, cancelled or replayed job, and then always proposes
`CLAIM_SQL` (`:4865-4871`). A refusal is `continue` to the next candidate. A
pass therefore costs one quorum read and one proposal per queued job, then
sleeps 25–30 s and repeats. `dv_title_scan` adds two more proposals per
candidate before the claim (`dv_title_jobs.rs:72-125`).

| Node (uptime) | Kind | Claims attempted | Accepted |
|---|---|---|---|
| lab6 (14.6 h) | `subtitle_extract` | 155,873 | 7 |
| lab6 | `dv_title_scan` | 73,617 | 6 |
| lab6 | `library_scan` | 3,482 | 5 |
| lab6 | `encoded_output_prepare` | 2,705 | 2 |
| lab6 | `fragment_index_build` | 1,598 | 0 |

That attempts are proposals is corroborated by the write counters: lab4
wrote 14.4/s against 9.7 claim attempts/s, lab3 16.0/s against 11.5/s, and
the commit index advanced at exactly the sum of the nodes' write rates. On
lab6 the refused attempts are 54 % of every write the node made. The learner
is a legitimate claimant for these kinds and pays an extra quorum read per
candidate in `may_execute_job`.

There is no refusal-reason metric; the only event is `claim_refused`.

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
(`migration.rs:160-164`). A voter starved for ~2 s campaigns with a higher
term and the healthy leader ends up stepping down.† The exact mechanism is
unverified: OpenRaft is pinned at 0.9.25 but not vendored, so whether
followers reject the vote and the leader steps down on a later reply, or the
vote succeeds outright, has to be checked against that source. Either way
this is the probable trigger of the 2026-10-31 failure
([PLAYBACK-QUORUM-GAPS-RCA.md](PLAYBACK-QUORUM-GAPS-RCA.md)), and nothing
structural has changed since.

### R2. Every election fences every node

The serving lease is 1 s, sampled every 500 ms with a 750 ms timeout. An
election takes at least 2.4 s. So any unplanned leader loss makes all four
nodes lose serving authority; running sessions survive only inside the 5 s
session grace. A planned handoff (patch 4's election trigger) avoids the
timeout wait and should fit inside the lease; that is not measured here. A 4.0 s timeout plus one vote round is about 5 s. The measured
2,468 ms outage is the good case, not the bound.

### R3. Nothing makes a leader restart a planned handoff

Leadership handoff exists (`handoff_leadership`, `membership.rs:8435`) and is
called from maintenance, leave and removal. Nothing in `crates/plurxd/src`
calls it on shutdown. Restart-preparation does not help: its rollout verdict
requires a drained non-leader (`http/cluster_operations.rs:2358-2379`), so it
refuses on the leader; only maintenance admits the leader and hands off. A
`plurxd cluster prepare-restart` command already exists (`main.rs:894`).

Whether a deploy restarts the leader cold depends on the Ansible playbooks,
which are outside the repository and unread; `scripts/ship --deploy` only
invokes them. If they restart the container without entering maintenance, a
deploy of the leader takes R2's outage on every node, against the standing
expectation that a leader restart must not interrupt playback elsewhere.

### R4. Timeouts contradict each other

| Timer | Value | Conflict |
|---|---|---|
| Read-confirm TTL / sampler timeout | 800 ms / 750 ms | below the 1 s SYN retransmit: one lost SYN on any new dial is a timeout |
| `STORE_TIMEOUT` | 3 s (`store/hiqlite.rs:298`) | inside the election window; the fork's 8 s and 14 s leader-recovery budgets are dead code under it † |
| Sharing read timeouts | 1 s | inside the store's 5 s quorum-recovery retry |
| Serving lease / session grace | 1 s / 5 s | vs. elections of 2.4–5 s |
| Peer "reachable" window | 30 s | vs. peer call deadlines of 0.2–8 s |
| Idempotent-write leader-recovery budget | 16 s (`migration.rs:167`) | longer than the 12 s session lease it is meant to protect |

No one place defines these, so each was correct when written and the set is
not.

### R5. Acknowledged writes are not power-loss durable

The WAL runs `ImmediateAsync`: the flush is `msync(MS_ASYNC)`
(`vendor/hiqlite-wal/src/writer.rs:314-320`), which schedules the write and
returns. OpenRaft is told the entry is durable after that call, not after the
data reaches the disk.
A process crash is safe. A power cut that takes all voters at once — one
house, one breaker — can lose acknowledged entries. That may be an acceptable
trade; it is not a recorded decision.

### R6. The fork carries the design mismatches

28 patches, 18 of them plurx policy; one has an upstream PR
(`vendor/hiqlite/PLURX-PATCH.md`). Patches 4, 6
and 28 each work around something OpenRaft 0.10 provides (leader transfer, a
read-index API, a dedicated heartbeat client). The upgrade is not tracked
anywhere.

### R7. Seen on the fleet — one explained, one not (updated 2026-11-15)

- **lab3 answers ~0.5 GETs per second with 503 — explained.** The caller is
  plurx's own Bonjour companion on the same host (`plurxd advertise`,
  `plurxd/src/main.rs` `await_discovery_identity`), which asks
  `GET /api/v1/server` every 2 s and retries forever. A learner's route
  allowlist (`http/mod.rs` `learner_route_eligible`) does not contain that
  path, so every poll is refused with `learner_route_ineligible`, the
  companion never advertises the node, and the request log fills with
  warnings. Measured again 2026-11-15: 17 refusals in 28 s.
- **media1's 840 failed writes in its first 30 minutes — not explained.** The
  counter is `plurx_store_operations_total{class="write",outcome="error"}`;
  it has no method or error label, and it counts a statement error the same
  as a timeout. All 840 came in the first 1,758 s and none in the next 63 s.
  The container was recreated on 2026-11-15 and its log went with it. The
  peers' logs for that window show a decaying backlog of slow state-machine
  applies and lapsed fences, which fits a leader restart, and proves no more.
  A2's method label (#1626) is what makes the next occurrence readable.

### R8. A node whose Raft core has died stays up, fenced, until someone restarts it

Seen on lab3, 2026-11-15. The host's root filesystem filled (free space fell
about 50 MB a minute from 709 MB at 07:59 UTC). Snapshot admission deferred
for its ten minutes, then "attempted the existing snapshot path"
(`vendor/hiqlite/src/snapshot_admission.rs:70`); the build failed with
`SQLITE_FULL` at 08:09:19 and OpenRaft's core task quit on the storage
error. The filesystem has had 138 GB free since, but a quit core does not
come back: the process kept running, self-fenced, refusing every append
from the leader twice a second (22,444 error lines in twelve hours) and
answering its health check with 503. Nothing in plurxd treats "my Raft
core has exited" as the end of the process, so the container's restart
policy never gets the chance to recover it. A voter in this state is a
voter lost until a person notices.

Two smaller things in the same event: admission that knows there is not
enough room goes ahead anyway when its deferral expires, and the failed
build left a 144 MB `.temp` snapshot behind.

## 6. Target design — one facade, six questions

```
 feature code (HLS, Live TV, sharing, jobs, artwork, UI)
        │  may only import `cluster::Cluster`
        ▼
 ┌──────────────────────── Cluster ────────────────────────┐
 │ authority()   may this node serve / commit now; grace   │
 │ role()        voter · learner · standalone · fenced     │
 │ peers(purpose) one directory, cached, typed eligibility │
 │ read(class)   applied or linearizable, always declared  │
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
3. **Every read declares its class; there is no default.** Two classes.
   *Applied*: answered from this node's replica under the serving proof,
   stale by at most the lease plus apply lag. *Linearizable*: a fence, a
   final verdict, a read before a write, a uniqueness check, and anything
   whose staleness is a security matter — tokens, grants, membership
   removal. An undeclared site stays linearizable. The existing floor is
   process-local: it protects a node from reading behind its own writes, not
   behind a write committed through another node
   (`store/applied_view.rs:21-25`). So an applied read that must see another
   node's write needs the floor to travel with the request as a commit index
   (patch 21's `WriteAck` already reports one). `CatalogueReader`,
   `AppliedView` and the hint readers become one mechanism with one proof.
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

Ordered by return for effort. Each P0 item has an acceptance check that is
one `/metrics` comparison.

### Work already in flight — do not duplicate

| Where | Owner | Covers |
|---|---|---|
| PR #1597, source lease release, and its plan steps 2–4 (one lease table, per-node capacity) | Claude Opus session `01BCWLA1BAX…` | Stuck custody, and the single-reader capacity in §4.2 |
| PR #1598, sharing listener keeps serving through an unreadable switch | Claude session `01BCWLA21tC…` | The listener half of D4's "read error is a revocation" |
| Proposal "stop paying a quorum round trip per Store read" (project doc `claude/proposal-shared-read-authority-2026-11-14.md`) | same session | B3 and C1: the same two-class design, with a site inventory |
| PR #1248, adaptive playback resources and cluster placement | effort branch | Overlaps B6 |

B3 and C1 below are that proposal, not a second one. The session that owns it
builds it; this audit adds the cross-node floor requirement and the list of
classes that must stay linearizable.

### P0 — stop the measured waste

| ID | Change | Fixes | Acceptance |
|---|---|---|---|
| A1 | Stop proposing claims the node can see will be refused. (a) Split `CLAIM_SQL`'s WHERE clause into shared predicate macros, as `translation_admission!` already is, and build both `CLAIM_SQL` and the local candidate listing from them, so the two cannot drift and a blocked job is never a candidate. (b) A local per-job verdict runs before the quorum pre-read and returns a reason without proposing. A capacity reason ends the pass for that source key; it is per job and per key, never per kind. (c) The worker re-runs the free local verdict when the applied index moves or at the job's next time boundary (lease expiry, `not_before`), so a time-only change still wakes it. No replica or no serving proof: propose as today. (d) Count `claim_write` only for real proposals and add `claim_skipped{reason}`. (e) `dv_title_scan` runs the verdict before its bind and makes its demand-reset write conditional. | §4.2 | `claim_refused / claim_write` < 5 % per kind with the same queue; `claim_skipped` names the reason |
| A1b | The single cluster-wide source reader is the throughput defect behind A1. It belongs to PR #1597's plan steps 2–4; this audit only records that per-node (or per-storage-domain) capacity is what lets four nodes work the queue in parallel. | §4.2 | more than one background source job running across the cluster |
| A2 | A `method` label for store operations issued outside HTTP, and the same class counters on the membership manager's client. | §2 | every quorum read attributable by method, membership included |
| A4 | `offline_source_probe_loop` wakes on an applied change to its table instead of a 500 ms quorum poll. Same for the playback-cache ceiling and the settings pollers in D3. | D3 | source-derived idle floor < 10 quorum reads/s cluster-wide (the sampler's 8/s goes with C1) |
| A5 | One `PeerTransport` on `AppState`, with a connect timeout; delete the per-call constructions, starting with `peer_status_cache_loop`. | D5 | `TcpActiveOpens` to peer HTTP ports near zero at idle |
| A6 | Sharing: the member floor comes from one cached cluster answer; one cached scope proof per import generation; body and receiver monitors subscribe to changes and use `SessionGrace`; a read error is never a revocation. Builds on #1598. | D4 | `sharing_read` ≤ 2 per request, 0 per idle stream-second; a 3 s leaderless window does not end a shared session |
| A7 | Shared-cache canary: a voter that cannot be reached or times out is left out of that node's verdict; an affirmative mismatch (reply not visible, wrong `storage_id`) still revokes. A node that cannot complete its own canary stays revoked for itself. | D6 | one stopped voter leaves the shared cache serving on the others; a node on a different mount still revokes |

### P1 — build the layer

| ID | Change | Fixes |
|---|---|---|
| B1 | The `Cluster` facade and its source test; migrate callers; delete the duplicated predicates and the seven Live TV filters. | D1 |
| B2 | The single purpose-keyed peer directory, cached on applied membership. | D1, D5 |
| B3 | Two read classes, every site declared, undeclared stays linearizable; the cross-node floor carried as a commit index; merge the three local-read mechanisms. Owned by the in-flight proposal above. | D2 |
| B3a | Token authentication off the per-request quorum read. Today's choice is deliberate: "every voter judges a token against the committed policy rather than a node-local cache" (`store/hiqlite.rs:7333-7335`). The safe route is the one the codebase already uses for cached admin proofs: a local proof invalidated by the revocation fan-out, which requires every committed member to acknowledge before the revocation is admitted (`http/internal_auth_revocation.rs`). An applied-replica read alone would accept a revoked token for the lease plus apply lag and is not proposed. | D2 |
| B4 | `watch(table)` as the general change feed; retire remaining pollers. | D3 |
| B5 | One peer call path: cached proof for all authorizers, typed errors end to end, health from outcomes, index and subtitle holders raced with the first answer winning instead of tried serially. | D5 |
| B6 | Placement and quality catalog answer from the peer snapshot the node already holds instead of a fan-out per create. Coordinate with #1248. | §4.3 |

### P2 — the Raft layer

| ID | Change | Fixes |
|---|---|---|
| C1 | Coalesce read confirmations in the fork: one round in flight; a reader shares a round only if it arrived before that round started, later arrivals wait for the next; the read index is honoured; one failed round fails every waiter on it. The fence sampler becomes a reader of the same round. Owned by the in-flight proposal above. | §4.1 |
| C2 | A leader never restarts cold: plurxd's graceful shutdown hands leadership off first, and the deploy path puts a leader through maintenance (restart-preparation refuses a leader). Read the playbooks first. **Built in this PR, not merged:** the shutdown half, [build plan and log](LEADER-SHUTDOWN-HANDOFF-BUILD.md); the deploy-path half waits on the playbooks. | R3 |
| C3 | The single timing module; derive lease, grace, `STORE_TIMEOUT` and feature timeouts from it. | R2, R4 |
| C4 | Election behaviour under a starved voter: verify the mechanism against OpenRaft 0.9.25, then pre-vote if 0.10 is taken, otherwise a wider floor chosen from a measurement. | R1 |
| C5 | Open a tracked item for OpenRaft 0.10 and retire patches 4, 6 and 28 against it. | R6 |
| C6 | Explain R7's two observations. Done 2026-11-15 for the learner polling; the failed writes wait for A2's method label and the next leader restart. | R7 |
| C7 | A plurxd whose Raft core has exited on a storage error exits non-zero, so the supervisor restarts it and the node recovers when the cause clears. Snapshot admission that still lacks room when its deferral expires skips the build instead of attempting it, and a failed build removes its `.temp` file. | R8 |
| C8 | The Bonjour companion stops asking a learner for a route it refuses: either the learner answers `GET /api/v1/server` (it is node-local identity) or the companion reads a learner-eligible path. Decide whether a learner is advertised to clients at all. | R7 |

A1 should remove more than half of the Raft log; A1b is what makes the
cluster work the queue faster than one node would. How far A2–A6 take the
quorum-read rate cannot be stated until A2 makes the other 60 % attributable.

## 8. Rulings

Recorded 2026-11-14.

1. **WAL durability (R5) — Paul agreed.** Keep `ImmediateAsync` until A1
   lands, because write volume is the cost driver; then switch to a real
   flush and measure.
2. **Scope-proof caching (D4) — cache it.** "Nothing cached, a fresh Source
   scope request" is a code comment (`http/shared_library.rs:950`), not a
   decision Paul made; he favours caching. A6 therefore includes one
   short-lived scope proof per import generation instead of a fresh dial and
   proof per response.
3. **Election floor (C4) — an engineering call, not Paul's.** Decision: land
   C2 first so planned restarts stop causing elections, verify the
   starved-voter mechanism, then widen the floor from a measurement of real
   voter stalls. No change to the floor before that measurement.
4. **B3 — no external review step.** It follows the normal path: plan, one
   adversarial agent review of the whole plan, then build. The review of
   this audit changed B3 from "flip the default" to "every site declares its
   class", and moved token authentication out of P0 (B3a).

5. **Learner discovery (C8) — Paul decided 2026-11-16:** hide learners from
   discovery. [Execution plan](LEARNER-DISCOVERY-BUILD.md) covers in-process
   Bonjour/GDM and the host companion, promotion and withdrawal. Source and
   fleet acceptance remain open; the decision itself is resolved.

## 9. What is sound and should not be disturbed

- The serving fence: store-free, generation-monotonic, never extends a lease.
- `AppliedView`'s rule (proof, generation, floor) — B3 generalises it rather
  than replacing it.
- The durable job claim as the atomic ownership primitive. A1 changes when a
  claim is proposed, not what a claim is; the dispatch-turn trigger stays
  authoritative because it writes.
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
- Not examined, though a reader might assume otherwise: snapshot and learner
  catch-up (patches 12, 22, 25), the clock-skew guard and split-brain probe,
  backup and restore, membership-change safety, the job lease and takeover
  path, and what happens to in-flight proposals on leader loss (patch 14).
- OpenRaft is pinned, not vendored; R1's mechanism is unverified.
- No code was changed and nothing was deployed.
