# Cluster facade — one layer, and a test that keeps it one

**Status:** built — slice 1 of the build contract for audit items **B1**
and **B2** of [CLUSTER-LAYER-AUDIT-2026-11-14.md](CLUSTER-LAYER-AUDIT-2026-11-14.md)
§7 is built and reviewed (§10.1, §10.2); slices 2–7 follow the PRs named in
§8.
**Verified against:** `main @ dd76bb88e`, from source only — nothing was
compiled or run. Every `file:line` below is **re-verify at build time**.
**Author:** Claude (Fable 5.2), session `01BCWLA3JD1HCNMEid3AZrc3`.

Short paths: `mem.rs` = `crates/plurx-core/src/cluster/membership.rs`;
`d/` = `crates/plurxd/src/`; `ltv.rs` = `d/http/live_tv.rs`.

Shared DV/HDR integration adds `Purpose::Operations` for Activity's committed
member roster, including learners and stale rows. Activity uses the shared
directory and facade role/authority answers; member read/write authorization
and response signing remain fresh proofs through the facade, never cached
roster evidence. Its legacy transport construction remains counted until
`call` lands. This extension lowers the census ceiling from 765 to 754;
final qualification is pending.

## 1. Goal

Feature code asks the cluster six questions through one type,
`state.cluster`, and a source test fails any other route. Slice 1 lands
the test, the type with `authority()`, `role()` and `peers(purpose)`, and
the first callers. `read(class)` (B3), `watch(table)` (B4) and
`call(peer, …)` (A5/B5) are reserved names with no body here.

No wrapped mechanism changes behaviour in slice 1. The fence, the
membership manager and the roster SQL keep their rules; only who may
touch them changes.

## 2. What exists today (the seams)

**Handles.** `AppState` (`d/state.rs:774`) exposes `replication` (`:807`),
`serving` (`:809`) and `membership` (`:811`). It has one struct literal
(`:1398`). `MembershipManager` is also cloned into subsystems as a
private field (`media_pool`, `shared_cache`, `clock_offset`,
`PeerTransport`, `subtitle_source`, …), so a census of `state.membership.`
alone misses about 40 % of the reach-arounds.

**Crate boundary.** `ServingFence` and `PeerTransport` live in plurxd
(`d/serving_fence.rs`, `d/http/peer_transport.rs`); membership lives in
plurx-core. The facade therefore lives in **plurxd**, as `d/cluster.rs`
with children under `d/cluster/`. The roster read lives in plurx-core as
a new child file, `cluster/membership/directory.rs`, which can see the
manager's private client. `mem.rs` gains one `mod directory;` line and
nothing else.

**Role answers** (`mem.rs`): `is_replicated` (`:5321`),
`local_serving_role` (`:7789`, an atomic the heartbeat refreshes every
10 s, `Fenced` until the first one), `local_maintenance_active`
(`:7734`), `local_node_is_committed_voter` (`:7759`) and `…_member`
(`:7776`) (both in-process Raft metrics, no IO), `…_is_active_voter`
(`:7689`, adds a quorum read), `may_run_cluster_jobs` (`:7720`).

**Rosters** — all local (`metrics_db()` on a node client is an in-process
borrow, `vendor/hiqlite/src/client/mgmt.rs:552`; rows via `query_map`)
unless marked Q:

| Producer (`mem.rs`) | Rule | Callers |
|---|---|---|
| `activity_peers` `:8694` | other voters, not removed or fenced; keeps unreachable | 14 sites, 6 files |
| `activity_voter_count` `:9102` | voters incl. self | `shared_cache`, `media_pool` |
| `media_peers[_for_route]` `:9026` | members incl. learners, `bounded_read_ready`, progress ≤ 30 s | 11 sites, 8 files; `media_sessions.rs:3910` `peer_base` serves 13 relay calls, one per segment |
| `operations_peers` `:8785` | committed members, keeps stale rows | 8 sites, 6 files |
| `reachable_peer_http_urls` `:8618` | not removed, seen ≤ 30 s; no Raft filter | 7 sites (artwork, ingress) |
| `node_hostnames` `:8756` | not removed; no Raft filter | `http/system.rs:5017` |
| `clock_peers` Q `:8812`, `_hint` `:8820` | exact members, fails closed | `clock_offset.rs:53,57` |
| `clock_peers_after_authenticated_request` Q `:8836` | same, plus sender fence | `clock_offset.rs:532` |
| `cache_admin_revocation_peers` Q `:8928` | exact members, with a write | `http/internal_auth_revocation.rs:597` |

`reachable` is `now − last_seen_at ≤ 30 s` (`mem.rs:67`, `:4948`): a
function of the clock, not of a row change.

**Invalidation signals that already exist.** (a) The applied Raft
membership: `LocalDbRaftMetrics::membership_snapshot()` (`mgmt.rs:118`)
gives `membership_log`, `members`, `voters` and `committed`, with no IO;
the manager holds the watch (`mem.rs:4830`). (b) The applied mark
(patch 27): image generation plus one revision per registered table
(`mgmt.rs:300`), eight slots (`vendor/hiqlite/src/applied_mark.rs:31`),
two used (`store/applied_view.rs:81`). Roster rows change through
ordinary log entries, so (a) alone misses a removal fence; both are
needed.

## 3. Findings from the investigation

**F1 — the duplicated predicate.** `shared_cache.rs:540-562` and
`media_pool.rs:1540-1565` (pure half `:1969-1984`) both test
`peers.len() + local_is_voter == voter_count`. They agree on that
clause. They differ in that `media_pool` first returns false when not
replicated or `voter_count <= 1`, and takes its three reads in one
`join!`; `shared_cache` takes them in sequence with no replicated guard.
Each assembles one answer from three reads that can straddle a
membership change. Right answer: one method on one snapshot (§4.3).

**F2 — the seven Live TV filters** (`ltv.rs`):

| Line | Shape | Rule |
|---|---|---|
| 147 | list, take 3 | reachable, not self |
| 794 | list, sorted by id | reachable, has base, not self |
| 1952 | list, raft order, take 3 | reachable, has base, not self |
| 1092 | list | every voter, unfiltered (readiness page) |
| 194, 1852, 2018 | lookup by node id | reachable, has base |

`not self` is dead: the SQL already excludes this node. Line 147 omits
`has base`, so a voter with no HTTP row burns one of three guide
attempts and fails at 194; `has base` is right. The three lookups are
one rule under three error strings. 1092 is a different purpose (show
silence) and stays unfiltered. Orders differ (id at 794, raft id
elsewhere); slice 1 keeps each caller's order. An eighth filter,
`media_pool.rs:1436`, is the reachable-voter set.

**F3.** `LiveTvPeers` keeps a 5 s TTL memory of the owner address in
front of `activity_peers` (`ltv.rs:1623`, `:1710`). Behind a directory
whose hit is two atomic loads it saves nothing and adds 5 s. Its tests
are pinned by `validation/regressions.d/4da98a45-…toml`, and its
drop-on-failed-exchange rule is B5's. It goes in slice 5, not here.

**F4.** No roster excludes a peer in maintenance. Draining is enforced by
the peer's own answer (`http/internal_activity.rs:125`,
`media_pool.rs:2140`). That is correct for relays, which must still
reach a draining owner, and slice 1 keeps it.

**F5.** `committed_voter_ids!` (`mem.rs:3139`) is `voter_ids()` of the
effective membership; "committed" is the name, not a check.
`membership_snapshot().committed` is the check.

## 4. Contract

### 4.1 The census (`d/cluster/census.rs`, test-only)

Modelled on `store/consistent_read_census.rs` (per-file counts pinned
exactly, `:49-90`; one ceiling, `:117`; synthetic controls proving a new
site fails, `:271-331`) and `d/http/applied_settings_inventory.rs`
(walks every production file under `src/`, `:282-330`).

- Parses production files with `syn` (already a dev-dependency), found
  through `tests/support/rust_source_inventory.rs`. `rustfmt` splits
  `state` / `.membership` / `.call()` across lines, so a line scan
  miscounts.
- Counts, per file and per class: field access named `membership`,
  `replication` or `serving` on any receiver; paths naming
  `MembershipManager`, `ReplicationMonitor`, `ServingFence`,
  `PassiveRaftMetrics`, `PassiveMembershipMetrics`, `LocalDbRaftMetrics`
  or `hiqlite::`; calls to `PeerTransport::new`; and calls to the role
  and roster methods of §2 by name.
- Exempt: `cluster.rs`, `cluster/**`, `serving_fence.rs`,
  `http/peer_transport.rs`, and the wiring in `state.rs` and `main.rs`,
  each listed with a reason.
- `REACH_AROUNDS: &[(file, class, count)]` is pinned exactly: a file
  that gains a site fails, and one that loses a site fails until its
  entry drops. A file with no entry must have zero. `CEILING` must equal
  the table's sum, so it holds no headroom.
- **A ceiling may only shrink.** A Rust test cannot see history, so
  `tests/operations/test_cluster_facade_ratchet.py` reads `CEILING` at
  the merge base and at HEAD and fails if HEAD is larger — the base
  lookup of `tests/operations/test_media_pool_wire.py:664-670`.
- Controls: a synthetic `state.membership.activity_peers()`, a split-line
  spelling, a cloned-field spelling and a `PeerTransport::new` each fail.

Baseline, by a regex scan at `dd76bb88e` (the census's first run is the
number that gets pinned): `.membership` 305 sites in 46 files,
`.serving` 109 in 21, `.replication` 14 in 5, the type names 105 in 26,
`PeerTransport::new` 19 in 17, `hiqlite::` 28 in 8 — about **580**.

### 4.2 The facade (`d/cluster.rs`)

```rust
#[derive(Clone)]
pub(crate) struct Cluster { /* fence, membership, replication, directory */ }
impl Cluster {
    pub(crate) fn authority(&self) -> ServingAuthority;   // + is_ready, subscribe
    pub(crate) fn role(&self) -> LocalRole;               // no IO
    pub(crate) async fn peers(&self, p: Purpose) -> Result<Peers, ClusterError>;
    // reserved: read(class) B3 · watch(table) B4 · call(peer, ..) A5/B5
}
pub(crate) struct LocalRole { standing: Standing, admission: Admission }
pub(crate) enum Standing { Standalone, Voter, Learner, Absent }
pub(crate) enum Admission { Open, Maintenance, Fenced }
pub(crate) enum Purpose { Voters, MediaWorkers, Operations }
```

`standing` comes from the applied Raft membership — the source
`local_node_is_committed_voter` reads — so no caller moves onto the 10 s
atomic. `admission` is the two published atomics. `AppState` gains
`pub(crate) cluster: Cluster`; the old fields stay until slice 7.

### 4.3 The peer directory (B2)

One snapshot serves every purpose. `MembershipManager::roster_snapshot()`
runs one local statement — the union of the `activity_peers` and
`media_peers` projections without their `LIMIT` — and pairs it with
`membership_snapshot()`. `roster_stamp()` is the IO-free half:

```
RosterStamp { image_generation, membership_log,
              revisions: [cluster_nodes, cluster_node_http,
                          cluster_node_removals, cluster_node_progress] }
```

Rule (the one `AppliedView` uses, `store/applied_view.rs`): load `s0`;
refuse to cache if the generation is odd or the membership is not
`committed`; read; reload `s1`; keep the `Arc<RosterSnapshot>` only if
`s0 == s1`; it is a hit only while the current stamp equals `s0`. A miss
is one local read. No timer, no TTL, no background task.

The four tables are added to `APPLIED_VIEW_TABLES` (six of eight slots).
They are rowid tables with no cascade, and their only `DELETE`
(`mem.rs:2874`) is qualified. The hook census
(`store/placeholder_census.rs:3456`) reads store slices only, so slice 1
adds a sibling that reads `mem.rs` and its children for the same two
hazards.

Eligibility is one table of pure functions over the snapshot, evaluated
at call time, so `reachable` and progress freshness track the clock on a
cached snapshot. Each purpose reproduces its producer's rule exactly,
caps included. `Peers` offers `all()`, `reachable()`, `find(node_id)`,
`complete()` (F1's clause on one snapshot) and `local_is_voter()`.

**Staleness.** Applied state of this node: never ahead of what it has
applied, behind the leader by apply lag. The directory does not consult
the fence, as today's rosters do not; a node without a serving proof
still needs its roster to render status and to recover. Callers that
place work already hold `authority()`. A learner sees the same rows;
`Voters` never names it and `local_is_voter()` is false. An unclustered
process gets `Standalone` and an empty roster. A snapshot install, an
uncommitted membership or a missing slot means an uncached read — today's
cost, never a stale answer.

**Stay linearizable** (untouched in slice 1, each later an explicit
facade method, never `peers()`): `cache_admin_revocation_peers`;
`clock_peers` and `…_after_authenticated_request`; every
`authorize_internal_peer_*` (`mem.rs:9207-9435`);
`local_node_is_active_voter` (`http/images.rs:2918`); the learner branch
of `may_execute_job` (`mem.rs:1408`);
`reconcile_local_maintenance_commit`; the sharing member and custody
observations; every membership mutation.

## 5. Non-goals (guardrails)

- No feature gate, no setting, no Developer switch. The facade is not
  optional.
- No timer, TTL, poller or watchdog. If the build seems to need one,
  stop and raise it.
- No behaviour change to the fence, role publication, roster rules or
  peer authorisation. F2's `has base` fix is the one deliberate change
  and gets its own commit and test.
- No vendor edit (#1642 and #1646 own `vendor/hiqlite`). No edit to
  `handoff_leadership`, the shutdown future, job claims (#1625),
  `counted_client.rs` or the membership operation census (#1626), the
  idle loops (#1629), sharing monitors (#1614, #1619, #1622) or custody
  (#1597).
- No file under `d/http/hls/`, `d/transcode/` or `d/vod/` in slice 1
  (start-path changes carry a `/metrics` obligation, AGENTS.md).
- `read`, `watch` and `call` are not implemented. `MediaPool`'s wire
  protocol does not change.
- `docs/OPERATIONS.md` does not grow.

## 6. Slice 1 — milestones

Each is one commit; the fast lane only until the PR is together.

**M1 — census and ratchet.** `d/cluster.rs` (module doc is the contract),
`d/cluster/census.rs`, the ratchet test, `mod cluster;` in `main.rs`.
*Accept:* `cargo test -p plurxd cluster::census` is green; with a
scratch `state.membership.activity_peers()` added to `d/watched.rs` it
fails naming file and class; `python3 -m unittest
tests.operations.test_cluster_facade_ratchet` fails on a raised ceiling.

**M2 — directory in plurx-core.** `cluster/membership/directory.rs`, four
table names, the hook census sibling, pure eligibility functions.
*Accept:* `cargo test -p plurx-core cluster::membership::directory`;
pure tests feed one row set through old and new filters for a matrix of
{voter, learner, joining, removed, fence row, unready, stale progress,
no HTTP row, over-limit} and assert equal output per purpose. The old
filter bodies are copied into the test, since they are deleted in M4.

**M3 — facade and invalidation.** `Cluster`, `AppState.cluster`,
`plurx_cluster_directory_reads_total{outcome="hit|reload|uncached"}`.
*Accept:* on the three-voter lane (`--features hiqlite-contract-tests`):
a removal fence, a learner promotion and an unready progress row each
change the next `peers()` answer with no sleep; N reads with no roster
write return the same `Arc` and add N to `hit` under a paused clock;
advancing that clock 31 s flips `reachable` on the same `Arc`. A TTL
implementation fails the first; a read-per-call one fails the second.

**M4 — callers.** `ltv.rs` seven sites onto two helpers (`voters`,
`voter(node_id)`) and `:1993`; `media_pool.rs` `:1120`, `:1385`, `:1436`,
`:1540-1565`, `:1656`, `:1879`; `shared_cache.rs:540-562`;
`media_sessions.rs:3910`. Delete what they replaced; lower the pins.
*Accept:* census green with those files' roster classes at zero;
Live TV tests written before the change (a pinned candidate list per
site for a fixed roster) pass after it, with the 147 case changed in its
own commit; `http_tests`, `live_tv_two_node` and `cluster_activity`
green; `CEILING` about 35 lower.

**M5 — docs and paperwork.** §9. *Accept:* `make paperwork`.

Size: about 1,400 changed lines without the baseline table.

## 7. Risks

- `ltv.rs:147-194` sits beside a 60 s loop and `media_sessions.rs` holds
  the lease loop; #1629 touches both files. M4's hunks are helper
  swaps of a few lines, and `peer_base` is one function far from the
  loop. If #1629 lands first, rebase and recount.
- Six of eight mark slots used. B4 must widen the mark (a vendor patch)
  before it registers more than two tables; say so in its plan.
- The union read drops the `LIMIT`; rosters are capped at 64 members
  (`mem.rs:1058`), so the row count is bounded by membership.

## 8. Later slices

| Slice | After | Callers | Ceiling |
|---|---|---|---|
| 2 | #1629 | media-roster readers (`subtitle_source`, `subtitle_ranges`, `fragment_index_cluster`, `copy_index_v3`); `.serving` in `live_tv.rs`, `live_tv/**`, `http/internal_live_tv.rs`, `http/dvr.rs` | ~480 |
| 3 | #1626 | role and admission: `http/mod.rs` gates, `remote`, `activity_starts`, `internal_activity`, `system`, `developer`, `diagnostics`; artwork (`Purpose::Artwork`, explicit active-voter confirm); hostnames | ~340 |
| 4 | #1642, #1646 | operations: `http/cluster.rs`, `cluster_operations.rs`, `desktop_control.rs`, `main.rs` behind `cluster.operations()`; clock and revocation fresh rosters | ~170 |
| 5 | with A5/B5 | `PeerTransport::new` → `call`; delete F3's TTL memory and replace its regression record | ~120 |
| 6 | #1614, #1619, #1622, #1597; with A6 | sharing and custody files | ~0 |
| 7 | all | `AppState` fields private, manager roster and role methods `pub(super)`, census table empty | 0 |

**End condition:** `CEILING == 0`, the three `AppState` fields are
private, and the census is a ban with an empty table.

## 9. Docs in the slice 1 PR

- A row for this file in [README.md](README.md), status `open`.
- The contract for feature authors is the module doc of `d/cluster.rs`:
  the six questions, the staleness rule, the linearizable list.
- `docs/ARCHITECTURE.md` §2: one paragraph after "All cluster access
  stays behind the one internal `Store` trait" (file is 43.6 KB against
  a 48.1 KB ceiling). `docs/OPERATIONS.md`: nothing.
- `AGENTS.md`: a short section, "Cluster questions go through
  `state.cluster`", naming the census and the ratchet. It has no such
  rule today.
- `validation/points.toml`: `crates/plurxd/src/cluster.rs` and
  `crates/plurxd/src/cluster/**` beside `serving_fence` (`:750`) and
  under `cluster.membership`; `cluster/membership/**` (`:783`) already
  covers the new core file; map the ratchet test as its siblings in
  `tests/operations/` are mapped.

## 10. Execution log

| Date | Milestone | Agent-Model | Agent-Session | Result |
|---|---|---|---|---|
| 2026-11-15 | Design | claude-fable-5-2 | session_01BCWLA3JD1HCNMEid3AZrc3 | this document |
| 2026-11-15 | M1–M5 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | slice 1 built; compiled and tested on media1; deviations below |

### 10.1 Slice 1 as built

**Census (M1).** The first `syn` run at `dd76bb88e` counted **710** sites in
71 files. The six classes of §4.1 sum to exactly the estimated 580
(`.membership` 302 in 44 files, `.serving` 131 in 24, `.replication` 13 in
5, type names 94 in 28, other `hiqlite::` 21 in 5, `PeerTransport::new` 19
in 17); the role and roster method classes of §2 add 78 (30 files) and 52
(27 files). After M4 `CEILING` is **660** (−50): `ltv.rs` roster 7→0,
membership 17→8, role 9→7; `media_pool.rs` roster 7→0, membership 12→1,
role 5→1; `shared_cache.rs` roster 2→0, membership 3→0, role 1→0, type
3→2; `media_sessions.rs` roster 1→0, membership 3→2, type 3→2.

**Deviations, each the smallest change that fits.**

- The cache lives in plurx-core, `directory.rs`'s `RosterDirectory`, and
  plurxd's `cluster::Directory` wraps it. The invalidation test needs a real
  replicated node, and the in-process fixtures that build one are in
  plurx-core. `mem.rs` still gains one line.
- `Cluster` holds the fence, the manager and the directory; no
  `replication` field and no `Purpose::Operations` yet, because nothing in
  slice 1 reads them (dead code fails Clippy). `RosterSnapshot::operations`
  exists and is tested against `operations_peers`.
- `Admission` ranks `Maintenance` above `Fenced`, so
  `admission == Maintenance` is exactly `local_maintenance_active()`; the
  three media-pool maintenance checks moved to it.
- `state.rs` and `main.rs` are counted and pinned, not exempt: a whole-file
  exemption would hide every non-wiring use in a 13,700-line file. Slice 7
  can exempt the constructors item by item.
- M3's test runs on one real voter that admits one real learner, not three
  voters, and the learner's admission is the Raft membership change in
  place of a promotion (both move only the membership log; a promotion
  needs the whole promotion protocol). Reachability is wall-clock
  (`SystemTime`), not tokio time, so the 31 s step is an explicit `now`
  on the same snapshot instead of a paused clock. The removal fence is
  written as the coordinator writes it (attempt, intent, fence in one
  transaction); a bare insert is refused by its trigger.
- Subsystems that keep a manager clone (`MediaPool`, `SharedCacheCoordinator`,
  `MediaSessionCoordinator`) take a `Directory` handle at construction, so
  the process has one cache; test constructors build their own.
  `SharedCacheCoordinator` and `MediaSessionCoordinator` no longer keep the
  manager, only the transport made from it.
- The Live TV sites are two helpers (`voters`, `voter(node_id)`) and three
  pure selectors, each pinned against a copy of the inline filter it
  replaced; `ltv.rs:1852` swaps only the read behind F3's memory.
- Size: about 3,000 changed lines without the baseline table, against
  §6's 1,400; about 1,500 of them are tests (the old-producer copies, the
  replicated invalidation fixture, the census and its controls). The
  caller migration itself is about 500.

**Plan claims found false at build time.** M4's "delete what they
replaced": the producers `activity_peers`, `media_peers` and
`activity_voter_count` keep callers outside slice 1 and stay; what M4
deletes is the callers' own filters and the hand-built predicate. The
audit's `shared_cache.rs:395-421` is `:540-562` at `dd76bb88e` (this
document had it right). Every other `file:line` in §2–§4 held.

### 10.2 Review and landing

One adversarial review (Fable 5.2): no blocker, no behaviour drift; six
should-fix findings, all taken.

- **S1** The census also counts a handle taken out by a pattern or moved
  by a shorthand field, and every public `MembershipManager` method
  (class `manager`), read from plurx-core's source less five shared names
  (`NOT_CLUSTER`). Its module doc lists what it still cannot see.
- **S2** The ratchet compares the whole census with the base: `CEILING`,
  every (file, class) pin, `EXEMPT`, the name lists and `NOT_CLUSTER`.
- **S3** It fails, not passes, when CI or a named base cannot resolve the
  base, and it is a paperwork rule: the fast lane's `paperwork` job runs it
  against the pull request's base (`--base "$PR_BASE_SHA"`). A branch
  behind a `main` whose pins fell since reads as loosened: rebase it.
- **S4** Migrated code asks `role().is_clustered()` instead of
  `state.membership.is_replicated()`, and fixtures replace the manager and
  the facade together (`AppState::replace_membership`).
- **S5** plurxd tests the purpose mapping on a clustered snapshot, and
  `peer_base`, the media-pool rollout answer and the shared-cache canary
  as behaviour.
- **S6** `cfg(all(test, ..))` items and `#[cfg(test)]` statements and
  blocks are skipped; `any(test, ..)` is counted.
- Nits: `PeerDirectory` → `RosterDirectory`; `Voters` alone carries
  `complete()`; the four mark slots resolve once and a hit compares the
  revisions before the membership watch (which still builds the member
  sets: the vendor watch has no IO-free membership-log accessor); the
  invalidation test waits on the Raft watch and evaluates at one instant;
  no directory exemption; `..serving` in a macro is not a field.

**Landing on main.** Main's store attribution (#1626) put the manager's
SQL behind `CountedClient`, which hands out no raw client, so the
directory takes the applied mark from `AppliedView::applied_mark()`
instead, and the membership operation census covers `directory.rs`. With
the review's census classes, slice 1 counts **755** sites at `dd76bb88e`
and **765** in 75 files on the merged tree (main's sharing work, A6 M1
and M3, added eight `.serving` sites, and the leader handoff, #1642, two
`main.rs` sites). On the merged tree: `.membership` 276 in 42
files, `.serving` 144 in 28, `.replication` 19 in 6, type names 86 in 28,
other `hiqlite::` 10 in 4, `PeerTransport::new` 19 in 17, role 62 in 27,
roster 33 in 22, and the new `manager` class 116 in 35. The first two
classes moved against §10.1 because test-only `cfg(all(test, ..))` code is
no longer counted and patterns and shorthand fields now are.

**The mark has no room for the roster yet.** Main's A6 M3 (#1622) widened
the applied mark to sixteen slots and registered thirteen (the two applied
views and eleven sharing watch tables), so §4.3's four roster tables do not
fit. Until a later change widens the mark again (a vendor patch, which this
slice may not make) and registers them, the directory stamps with the
applied index: any applied entry on this node ends a hit. That is coarser
than planned — the cache holds between writes, not between roster writes —
but never stale, and it needs no timer. The per-table stamp is kept and
takes over once all four tables have slots.
