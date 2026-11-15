# Playback start — E1 build contract (replication classes as designed)

**Status:** open — design and build contract for lane **E1** of [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md) revision 3 (§3 E1). E1a and E1b merged (PRs #1500, #1503); E1c built on `claude/ps-e1c`. What each built and where it departs from this text: §14. Revision 2: the adversarial review [PLAYBACK-START-E1-REVIEW.md](../reviews/PLAYBACK-START-E1-REVIEW-2026-11-11.md) is applied (§13). Needs Paul's answers to §12 before a build agent claims it. Where this contract and the plan disagree, the plan's §0 rulings win; raise the conflict in the lane PR.
**Verified against:** `main @ 0c57d812` (2026-11-11), from source only — nothing was compiled or run. Re-anchor line numbers with `scripts/outline` and `rg` before editing.
**Author:** Claude (claude-fable-5-2, session 01BCWL9xwmpSjitHqZMTRAht). **How to work:** [PLAYBACK-START-BUILD.md](PLAYBACK-START-BUILD.md) §0.

Short paths: `ms.rs` = `crates/plurxd/src/media_sessions.rs`; `mig.rs` = `crates/plurx-core/src/cluster/migration.rs`; `writer.rs`, `sm.rs` = `vendor/hiqlite/src/store/state_machine/sqlite/{writer,state_machine}.rs`; `http/` = `crates/plurxd/src/http/`; `store/` = `crates/plurx-core/src/store/`.

## 1. Goal

Settings and media-session routes are answered from this node's own applied replica whenever the node can prove that replica is current, and from a quorum read only when it cannot. Fences keep their quorum reads. The route cache is invalidated by what the replica applied, not by a clock.

Measured problem (review 2026-11-07 §2): `plurx_http_store_reads_total{class="authority_read"}` — playback 168k, settings 82k; "~0.7 quorum reads/s on playback alone", i.e. one route-cache miss per second per watched session (`ROUTE_CACHE_TTL = 1 s`, `ms.rs:143`).

**What the gain is, stated honestly.** `media_sessions` is written about once a second cluster-wide (renewals every 3 s per owning node, `ms.rs:125`), so with per-table invalidation a cached route lives about as long as it does today. E1 turns the miss from a quorum round trip into a local read-pool read; it does not make misses rare. Whether per-row invalidation is worth adding is decided on E1a's numbers (§12-7).

## 2. Constraints that bind the build

- **Plan §1:** "Settings, routes and catalogue are node-local views of the applied raft log; only fences are consistent reads." **Plan §3 E1:** the view survives lag, snapshot replacement, release, lease expiry and same-epoch renewal; fences and the pre/post pending-release checks stay consistent; a fence never reads the view.
- **This supersedes an older rule for settings only.** `docs/cluster/CLUSTER-PERFORMANCE-PLAN.md` §3.2 and `BOUNDED-REPLICA-READS-ROLLOUT.md` §4 keep "settings that gate work" on `Authority` (§12-1). Both get a dated correction in the E1b PR.
- **No feature gate, no new switch.** `cluster.bounded_replica_reads` (`store/mod.rs:2144`) is not consulted (§12-3). One advisory Developer row (§9).
- **Store twins change together**, tested through `crates/plurx-core/tests/store_contract.rs`; the replica tests need `--features cluster-read-cost-validation` (it implies `hiqlite-contract-tests`, `crates/plurx-core/Cargo.toml:21, 30`).
- **hiqlite `$N` placeholders first appear in numeric order** (`store/placeholder_census.rs`).
- **Consistent-read census** (`store/consistent_read_census.rs:38-90`): E1 adds no `query_consistent*` site; if a count moves, change the table in the same commit.
- **Fork ledger:** a `PLURX-PATCH.md` row and bullet, a correct `PLURX-FILES.toml` (`tests/operations/test_hiqlite_patch_ledger.py`).
- **Rolling-producer census** (`tests/playback/rolling-producer-owners.toml`): recount for any spawn or timer. **This design adds none.** If the build needs one, stop and raise it.
- **No band-aids.** No watchdog, no re-read "just in case", no loop waiting for the replica. The only time bound in the design is `QUORUM_WATERMARK_LEASE` (`mig.rs:5550`), the lease the quorum proof already has; §4.4 uses the same constant for a direct quorum observation and nothing else.

## 3. What exists today (the seams)

**Read classes.** `StoreOperationClass::{LocalRead, AuthorityRead, Write}` (`store/hiqlite.rs:686-690`) feeds `plurx_http_store_reads_total{route_group,class,role}` (`http/mod.rs:128-139, 208-221`, middleware `:673-700`). An authority read is `query_consistent_map[_scoped]` (`store/hiqlite.rs:1371-1427`) → `raft.ensure_linearizable()` then the local read pool (`vendor/hiqlite/src/query/mod.rs:19-30`; the read index is discarded at `:29`); a follower goes to the leader first. A local read is `query_map` (`store/hiqlite.rs:1429`).

**The proof exists.** One loop per node samples a leader quorum watermark every 500 ms with a 1 s monotonic lease (`mig.rs:5548-5550, 6916, 7171-7223`). `eligible_serving_proof()` (`mig.rs:6169`) is `Some` only when this node has applied a quorum-confirmed commit index of the current term/leader/observation epoch whose confirmation is inside its lease; `serving_proof_remains_valid` (`:6325`) rechecks it. Because `apply` awaits the writer's reply for every entry (`sm.rs:1166-1175, 1307-1326`), "applied" means visible to the read pool. The proof is void after a restart, a term or leader change, or lease expiry (`:5791-5808`), and is `None` on SQLite and remote-authority processes (`:6166-6168`). `CatalogueReader` (`store/mod.rs:6398-6531`) already uses the same family for catalogue rows and presentation settings (`:6908-6927`).

**The applied view is the local replica.** It is fed by the raft log (`sm.rs:1097-1330` → the single writer thread, `writer.rs:285-880`) and bootstraps from a snapshot. `WriterRequest::SnapshotApply` (`conn.restore`, `writer.rs:681-737`) has **three** senders: `update_state_machine_` (`sm.rs:763-780`), reached at start-up when the database is absent (`sm.rs:402-407`) and from `recover_pending_snapshot` (`sm.rs:838`), and the install task (`sm.rs:1473-1512`). Each reconnects the read pool afterwards. E1 adds no second materialisation; it adds a signal of what changed.

**Missing.** The only local change signal is openraft's metrics watch (`vendor/hiqlite/src/client/mgmt.rs:287-294`), which names no table. rusqlite's `hooks` feature is off (`vendor/hiqlite/Cargo.toml:306-322`).

**Routes today.** `raw_route_before` (`ms.rs:2562-2615`): pending-release check → cache → per-shard single flight → cache → `store.media_session_route` (`:2602`; `route_by`, `store/hiqlite_sessions.rs:2028-2045`) → generation-guarded insert (`:3037-3063`) → pending-release check. Entries expire at `now + ROUTE_CACHE_TTL` (`:3776-3797`); classification by clock happens on every use (`:2434`). This process's own facts enter through `cache_route` (`:2952`), `cache_route_if_generation` (`:2966`) and `cache_terminal_route` (`:2995`; the tombstone that stops a released session falling through to a live local actor, `:2991-2994`). Session SQL is a per-layout template; while the layout is `Legacy` every read is re-checked (`store/hiqlite_sessions.rs:386-420, 636-665`).

**Device profiles are not a replicated read** — an embedded TOML parsed once (`crates/plurx-core/src/playback/mod.rs:38-49`). Nothing to move.

## 4. Design

### 4.1 The applied mark (vendored hiqlite, patch row 27)

```rust
pub struct AppliedMark {              // Arc, created with the state machine; lock-free
    generation: AtomicU64,            // even = image stable, odd = image being replaced
    applied_index: AtomicU64,         // last log index whose effects the read pool can see
    tables: [AtomicU64; 8],           // one dedicated revision per registered table
}
impl Client { pub fn local_db_applied_mark(&self) -> Result<AppliedMarkHandle, Error>; }
```

- **Registered tables, dedicated slots.** `NodeConfig` carries up to eight table names (plurx registers `settings` and `media_sessions`). The writer's `update_hook` (rusqlite `hooks`) compares the table name against that list and sets a bit in a writer-local mask; other tables cost one short compare and no revision. No hashing, no collisions.
- **Publish after commit, before the reply.** Every arm replies inside itself (`writer.rs:346, 591-595, 632-640`) and several `continue` early (`:312-316, 339-342, 436-441, 782-786`). One helper — bump each masked slot, then store `applied_index`, clear the mask — runs after the SQLite work has committed or rolled back and **before** every `tx.send`/`ack.send` of an arm that applies a log entry, the early-`continue` paths included. Published after the reply, a memo hit could serve the old value after the write was acknowledged on this node; published from the hook, a reader could cache pre-commit data under the post-commit revision. A source test pins the order.
- **DDL.** The hook reports a table rebuild's copy under its temporary name and a `DROP`, `RENAME` or `ALTER` not at all, and plurx's schema steps on `media_sessions` travel as `Transaction`s (`schema_migration_transaction`, the Source-layout rebuild), not only through `Batch` and `Migrate`. So the publish helper re-reads `PRAGMA schema_version` before every publish and bumps every slot when it moved (or could not be read), whichever arm carried the change; `Batch`, `Migrate` and a successful restore also bump every slot. *(Revised in E1a, §14: the arm-based rule missed transactions.)*
- **Image replacement, paired where the pool is reconnected.** A guard taken in `sm.rs` — not in the writer arm — makes `generation` odd before `SnapshotApply` is sent and even after `reconnect_read_pool` returns, at both reconnect points: inside `update_state_machine_` (covers start-up restore and pending-snapshot recovery) and in the install task. Dropped without completing (restore failed) it stays odd: the view is closed until restart, and the Developer row says so.
- **Local only**, like `local_db_raft_metrics`: a remote client returns `Err`, no HTTP fallback (source test in the style of `mgmt.rs:1410-1423`).
- **Hook limits.** No callback for `WITHOUT ROWID` tables (none exist) or a `DELETE` without `WHERE`; a source census over `STORE_SOURCES` refuses both on registered tables. Rows removed by `ON CONFLICT REPLACE` are not reported, but the replacing insert is, so the slot still moves. The census reads this binary's string literals: SQL built with `format!` is outside it, and an unqualified `DELETE` in a log entry an older binary wrote applies here as written, without the hook.
- Files: `Cargo.toml`, `src/config.rs`, `…/sqlite/writer.rs`, `…/sqlite/state_machine.rs`, `src/app_state.rs`, `src/client/mgmt.rs`, `src/lib.rs` — all already `patched`.

### 4.2 `AppliedView` (`store/applied_view.rs`)

A sibling of `CatalogueReader`, constructed beside it (`AppState.applied`, `crates/plurxd/src/state.rs:800`). One rule for every view read, with `P = eligible_serving_proof()`:

1. Load `g0 = generation`, `r0 = tables[slot]`. If `g0` is odd, or `P` is `None`, or the read's **floor** (below) is not met → **authority**.
2. Run the local statement, or take the memo/cache entry.
3. Require `generation == g0` and `serving_proof_remains_valid(&P)`; otherwise discard → **authority**. A local SQL error → **authority**.
4. An entry made from the result is stamped `(g0, r0)`, the values loaded *before* the read. It is a hit only while `generation == g0 && tables[slot] == r0` and step 1 passes at hit time.

"Authority" is the existing `Store` method under the caller's deadline. No wait, no retry.

**Bound.** A view answer is at least as new as a quorum-confirmed index confirmed ≤ 1 s ago on this node's monotonic clock. The time bound equals the catalogue reader's; the entry lag behind the newest watermark is not bounded (the proof is kept *because* apply may trail, `mig.rs:6307-6317`). The E1c PR amends that comment, which today says the read path is untouched.

**The floor — read-your-write and monotonic reads without a commit index.** A lagging replica with a valid proof can be *older* than something this process already knows: its own acknowledged write, or a quorum read it just made. Such a fact, learned at monotonic instant `t`, sets a floor: the view may not be used for that key until a watermark confirmation whose attempt **started after `t`** has `committed_index ≤ mark.applied_index`. A confirmation started after `t` has a read index ≥ the leader's commit index at `t`, hence ≥ the write; once this replica has applied it, the replica contains the fact. One accessor on `PassiveRaftMetrics` — `confirmed_since(t) -> Option<u64>`, from the watermark the seqlock view already carries (`WatermarkView`, `mig.rs:5761-5786`) and on the same clock (`started_at.elapsed()`). No vendor wire change; it covers transactions, which `WriteAck` does not (`vendor/hiqlite/src/client/execute.rs:20-23, 54, 143`).

**Standalone.** On SQLite and remote-authority processes there is no proof to hold: `AppliedView::authority(store)` (the `CatalogueReader::authority` pattern, `store/mod.rs:6419`) forwards to `Store`. For the route cache the proof test and floor are defined as always-met on SQLite (the store is the only copy) and never-met on a remote-authority process. **SQLite hands out no stamps** *(revised in E1a, §14)*: every view read there is a store read, and `check` answers `Stale`, so a `View` entry never hits on SQLite and an `Observed` entry is retired at once (its floor is always met). Process-local revisions would need every SQLite write to those tables to bump them after commit; the read they save is one local primary-key read, what the view costs on a replica anyway.

**Surface:**

```rust
impl AppliedView {
    pub async fn setting(&self, key: &str) -> Result<Option<String>, StoreError>;
    pub async fn switch(&self, key: &str, default: bool) -> Result<bool, StoreError>;
    pub async fn settings(&self, keys: &[&str]) -> Result<AppliedSettings, StoreError>;
    pub async fn route(&self, session_id: &str) -> Result<AppliedRoute, StoreError>;
    pub fn observation(&self) -> AppliedViewObservation;        // advisory
}
pub struct AppliedSettings { pub values: BTreeMap<String, String>, pub digest: SettingsDigest, pub source: ViewSource }
pub struct AppliedRoute    { pub route: Option<MediaSessionRoute>, pub stamp: Option<ViewStamp>, pub source: ViewSource }
```

`SettingsDigest` hashes the requested keys and values in key order: identical on every node for the same committed values and restart-proof. It is what D3 carries in `StartPlan` and compares on the owner; mark revisions are process-local and never cross the wire. **D3 consumes `AppliedView::settings(&QUALITY_PLANNING_KEYS)` and `AppliedView::switch(keys::PLAYBACK_NETWORK_PRIORS, false)`** (replacing `http/network.rs:164`).

### 4.3 Settings view

A memo of the whole table (`Arc<BTreeMap>` + stamp), reloaded by one local `SELECT key, value FROM settings ORDER BY key` when the stamp no longer matches; reloads are single-flighted behind one async mutex. `plurx_applied_view_reloads_total{view="settings"}` shows the rate (saves and job stamps, `state.rs:13475`).

**Read-your-write.** The hiqlite store records the monotonic instant of this process's latest settings mutation (`put_setting`, `store/hiqlite.rs:6196-6204`, `get_or_init_setting`, and every other statement writing `settings` — a source test over `STORE_SOURCES` pins the list). That instant is the settings floor: after a save through this node the view is closed for settings until a later watermark is applied, about one sampling period. A save through another node is bounded by the lease.

**The save response stays a quorum read.** `settings_dto` (`http/system.rs:2726-2732`) takes its source as a parameter: the `PUT` response (`:4872`) passes authority — the web client renders it (`crates/plurxd/src/web/pages/settings-developer.js`) and it carries `live_tv_config_generation`, which the next save must echo (`http/system.rs:3662-3670`); `GET` (`:3263`) passes the view.

### 4.4 Route view and the cache

`media_session_route` gains a local twin (same projection as `route_by`, `query_map`). **It runs only when `media_session_projection == 2`** (`store/hiqlite_sessions.rs:391-395`); in the legacy layout the view answers "authority". `raw_route_before` keeps its shape and its four pending-release checks (`ms.rs:2567, 2575, 2594, 2613`). Changes:

1. The read at `:2602` becomes `applied.route(session_id)`.
2. `CachedRoute { route, expires_at }` becomes `CachedRoute { route, origin }`:
   - **`View(stamp)`** — hit while §4.2 rule 4 holds. A renewal, release, takeover or expiry sweep applied here moves the slot; the next request re-reads locally.
   - **`Observed { at }`** — a fact this process holds directly: a row it just committed (the existing `cache_route*` callers) or the result of a quorum read. `at` is its floor instant. It is **retired** — removed, so later lookups use the view — only when the floor is met and the generation is even. Until then it is the answer while `now − at < QUORUM_WATERMARK_LEASE`; after that the lookup takes a fresh quorum read, which becomes the new `Observed`. A view read never replaces an unretired `Observed` entry, whatever the slot does: that is what stops a replica three entries behind from reviving the pre-release row after two unrelated renewals apply.
   With a healthy proof an `Observed` entry lives about one sampling period. With no proof (lag, leader gap, a rolling upgrade's protocol mismatch `mig.rs:5807`, a remote-authority process) the cache degrades to exactly today's behaviour: one quorum read per session per second.
3. `ROUTE_CACHE_TTL` and the expired-entry sweeps (`ms.rs:2974, 3023, 3044`) go. Stale `View` and retired `Observed` entries are removed when a lookup touches them; at `MAX_ROUTE_CACHE_ENTRIES` an insert evicts by one O(n) scan (any stale entry, else the lowest insert sequence), the same cost as today's sweep but only when full. `plurx_media_session_route_prune_entries` keeps its meaning for that scan.

`route_generations` and the release fences are untouched; they order this process's own publications against in-flight reads. **Lease expiry** needs no invalidation (classification by clock on every use); **same-epoch renewal** is a row change like any other.

**Final verdicts stay quorum reads.** Every `authoritative_*` resolution (`ms.rs:2463-2506`) is unchanged, between its two pending-release checks (`:2467, 2487`). The view only *admits* a request to the owner it names; it never ends one, and it is never the read before a claim or a write.

## 5. State transitions and failure modes

| Event | Mark / proof | Reader |
|---|---|---|
| Process start | generation even; no proof until the first watermark | Authority, then view; caches start empty |
| Start-up restore (no DB file, snapshot present) or pending-snapshot recovery | guard in `update_state_machine_`: odd → even after reconnect | Authority while odd; view opens afterwards |
| Replica lags | proof `None` once no applied confirmation is inside its lease | Authority; returns by itself |
| Floor not met (own write or quorum read newer than the replica) | — | settings: authority; routes: the `Observed` entry, then a quorum read per lease |
| `InstallSnapshot` | odd during; even after reconnect; every stamp mismatches | Authority during; reload after |
| Restore fails | stays odd | Authority until restart; row and metric say `image_replacing` |
| Leader / term change | proof void until a watermark from the new leader | Authority for the gap; stamps stay valid (the image only moves forward) |
| Deposed or partitioned node | proof gone within 1 s | Quorum reads, which fail closed as today |
| Local SQL error, pool exhaustion | — | Authority for that read |
| Release through this node | release fence first (`ms.rs:2697-2698`); `cache_terminal_route` writes `Observed` | pending-release checks reject before and after; then the tombstone until its floor is met, then the view (which now holds the terminal row) |
| Release / takeover through a peer | slot moves when applied here | ≤ 1 lease stale before that: the request reaches the old owner, which refuses, and the relay's `authoritative_*` reclassification decides (`http/hls/relay.rs:27-52, 107-123`) |
| Owner stops renewing | no row change | classification by clock |
| Rolling upgrade, old leader | proof closed (`mig.rs:5807`) | today's rate: one quorum read per session per lease |
| Legacy session layout | — | routes: authority |
| SQLite | no mark, no stamps | proof and floor always met; every read is a store read |

## 6. Call-site classification

### 6.1 Routes — `media_session_route` (session-keyed; the review confirmed the list is complete)

| Site | E1 | Reason |
|---|---|---|
| `ms.rs:2602` (`raw_route_before`; via `route_resolution_before` `:2401`, `media_route_resolution_before` `:2452`, i.e. `http/hls/relay.rs:17, 22`) | **view** | ordinary routing; the owner and the reclassification are the fence |
| `http/hls/status.rs:39` | **view first** (`media_route_resolution_before`; quorum to confirm a non-active verdict, as `relay.rs:14-52`) | routing |
| `http/hls/status.rs:64` | **stays** | the post-publication half of a pre/post bracket around local actor state (`:55-75`, `same_route_authority`) |
| `http/hls/status.rs:141` | **stays** | reclassification after a relayed peer answer |
| `http/system.rs:1176` | **view** | activity detail, read-only |
| `http/hls/link_receipts.rs:1014`; `prepared_link.rs:426`; `candidate_recovery.rs:86` | **stays** | each is the fence before a claim or durable observation (`link_receipts.rs:1011-1037` "Claim only after its current authority fence"; `prepared_link.rs:426-457` → `register_staged`; `candidate_recovery.rs:79-86`) |
| `http/hls/link_receipts.rs:492, 610, 627, 947`; `prepared_link.rs:357` | **stays by default** | may move in E1c only if E1a's baseline shows the site hot and the PR shows no write or claim behind it |
| `ms.rs:2482, 2634` and callers `http/hls/segment.rs:506`, `response.rs:353`, `relay.rs:31-40, 113-122`, `internal_media_sessions.rs:817, 826`, `http/hls/control.rs:1681`, `quality_control.rs:211`, `quality_schedule.rs:173`, `create.rs:2771` | **stays** | final verdicts and pre-mutation reads |
| `playback_control.rs:2507`, `vod/shared.rs:209`, `vod/serve/delivery.rs:914`, `internal_media_sessions.rs:998`, `http/hls/create.rs:4103`, `http/jellyfin/playback.rs:295, 331`, `shared_receiver_playback.rs:2160`, `shared_receiver_direct.rs:206` | **stays** | ownership/terminal proofs or reads before a mutation |

### 6.2 Routes — everything else stays

- `media_session_route_by_incarnation`, all ≈ 50 sites: an exact incarnation read is an ownership or publication fence.
- `media_session_route_for_playback` (`ms.rs:2544`; `http/hls/create.rs:3889`; `candidate_recovery.rs:549, 597`; `jellyfin/transport.rs:24`) and `staged_media_session_for_playback` (`ms.rs:526, 554, 2528`; `control.rs:2596`; `transcode/manager/create.rs:4995`): prepared-publication authority (`ms.rs:2441-2447`).
- Every mutation, the takeover switch read (`ms.rs:5536`), and the serving fence (`crates/plurxd/src/serving_fence.rs`), which reads the proof and never the view.

### 6.3 Settings — 178 production call sites (`get_setting` 142, `_pair` 11, `get_settings` 7, `settings_snapshot` 15, `local_setting_hint` 3) plus `playback_planning_snapshot`

Rule: a read **moves** when its answer only chooses how to serve, plan or present, and a ≤ 1-lease-old value gives at worst the behaviour of a request that arrived a second earlier. It **stays** when it precedes a write or reservation in the same operation, sits under a lock that a settings save takes, decides authentication/authorisation/membership/ownership, builds a save response, or runs before the node serves. Anything inside a control-exchange handler stays unless the PR shows it is read-only.

| Group | E1 | Notes |
|---|---|---|
| Playback request path — `http/hls/link_receipts.rs:215, 1251`; `http/hls/create.rs:2120`; `http/network.rs:164`; `http/stream.rs:82, 2495, 2647`; `http/preparation.rs:373, 380`; `http/chapter_thumbs.rs:185`; `http/internal_media.rs:278`; `telemetry.rs:1572` | **view** | preference reads |
| `http/hls/preparation.rs:1921, 2442`; `http/shared_receiver_control.rs:329` | **stays** | `PREPARED_QUALITY_HANDOFF` is read under the transition lock a save takes (`http/system.rs:4469-4487`; `preparation.rs:2434-2444`); a view answer reopens that race |
| `http/hls/control.rs:1923, 2547` | **stays** | "after routing, before any owner mutation" |
| Planning/describe — `transcode/manager/describe.rs` (10), `plan.rs:1518, 1640, 1667`, `construct.rs:223, 320, 1019`, `create.rs:1071, 4508`, `transcode/planning.rs:67`, `content_encoding.rs:183`, `source_subtitles.rs:227`; `vod/serve/create.rs:955, 2659`; `vodencode.rs:799, 949, 964, 994`; `source_probe/fel_qualification.rs:968`; `transcoder_optimization.rs:92`, `comparison.rs:126, 129`; `playback_cache.rs:52`; `cachekeep.rs:835` | **view** | recipe, hwaccel and quality preferences; each checked against the rule in the PR |
| Polled pages — `http/system.rs:77, 82, 87, 116`, `:207`, `:420, 429, 436`; `settings_dto` from `GET` (`:3263`); `http/developer.rs:416`; `media_pool.rs:1324, 1334, 1359` | **view** | the 82k |
| `settings_dto` from `PUT` (`http/system.rs:4872`); `:3667, 3787, 4812`; `get_or_init_setting` | **stays** | save response and pre-mutation reads |
| `playback_planning_snapshot` (20 sites) | **stays in E1** | D3 collapses it onto `AppliedView::settings` with the digest |
| Auth, trust, membership — `http/users.rs:135`; `http/remote.rs:181`, `remote/owner.rs:603, 700`; `http/remote/invitations*`; `sharing*.rs`; `clock_offset.rs:748` | **stays** | revocability and admission |
| Ownership — `ms.rs:5536`; `http/offline.rs:56, 66`; `offline.rs:717`; `backup.rs` | **stays** | |
| Boot — `main.rs:1790, 3191, 3426` | **stays** | before the first proof |
| Background workers — `state.rs` (37), `subtitle_*` (≈ 22), `live_tv*.rs`, `http/dvr.rs`, `library_search*`, `trakt.rs`, `watched.rs`, and the remaining `http/` singles | **not in E1** | not on the acceptance metric (§12-6) |

The table becomes a pinned inventory test in the style of `bounded_catalogue_handler_inventory_keeps_reads_and_mutations_separate` (`http/tests.rs:1990`). *(Built in E1b as `crates/plurxd/src/http/applied_settings_inventory.rs`; the planning/describe group, encoder admission, FEL qualification and `media_pool.rs:1359` stay with the six sites the first build kept — E1b record, §14.)*

## 7. Sub-lanes, order, size

| Lane | Content | Size | Depends |
|---|---|---|---|
| **E1a** — mark, view, baseline | patch 27 complete (registered slots, publish-before-reply helper, DDL bumps, the `sm.rs` replace guard at both reconnect points) with vendor tests and ledger; `confirmed_since` on `PassiveRaftMetrics`; `AppliedView` with the floor, standalone forms and store-contract tests; metrics; Developer row. **Baseline:** `plurx_media_session_route_reads_total{site,source}` (`site ∈ routing, status_first, status_bracket, status_reclassify, control, verdict, receipt_fence, receipt_candidate, observer` — one label per decision §6.1 and §12-4/5 leave open; revised in E1a, §14), `plurx_applied_view_table_changes_total{view}` against request rate (the most often per-table invalidation could reload; `plurx_applied_view_reloads_total{view}` is exported but stays 0 until E1b/E1c have readers), and read-pool wait — so the "before" says which sites make the 168k and whether per-table invalidation is enough. No call site moves | M–L | — |
| **E1b** — settings | settings floor; memo; `settings_dto` source parameter; move the §6.3 view rows; inventory test; correct the two cluster docs | M | E1a |
| **E1c** — routes | local twin behind the layout check; `View`/`Observed` cache; delete `ROUTE_CACHE_TTL`; `status.rs:39`; `system.rs:1176`. **Sized after E1a's baseline**: with status `:64, :141`, control and the receipt fences staying, the measured gain may be mostly the `:2602` miss | M | E1a |

E1b and E1c may run in parallel after E1a. D3's applied-settings reads start after E1b.

## 8. Tests

**New behaviour — fails against `0c57d812`** (each PR records the counterfactual run):
- *Vendor:* a committed write to a registered table moves its slot and `applied_index` no earlier than the row is visible and no later than the reply (hold the commit with the instance-local precommit hold, `writer.rs:54-65`); an unregistered table moves nothing; `Batch`/`Migrate` move every slot; a restore makes `generation` odd then even through **each** of the three senders, including a node started with no database file and a current snapshot; a failed restore leaves it odd; the accessor has no remote fallback and no `.await`.
- *Store contract* (`cluster-read-cost-validation`; hold a follower with the instance-local precommit hold or a new per-instance apply hold — **not** `validation_wait_for_apply`, a process-global flag, `sm.rs:37-41, 161-176`):
  - `applied_view_matches_authority_and_falls_back_exactly_once`, modelled on `store_contract.rs:29949`: with a proof, N reads cost 0 consistent calls; with the proof revoked during the read (`mig.rs:6013`), exactly one.
  - **The review's B1 sequence:** follower F releases a session at index W while its replica is at W−3 and W−2, W−1 are other sessions' renewals. As W−2 and W−1 apply, F never answers active. Same for a takeover (never the old owner) and an activation (never absent).
  - **Stale replica during release/replace through the leader:** F's cached row is invalid the moment the entry applies, with no clock advance; while it is held, F's view closes when the proof lapses.
  - **Snapshot replacement** (a learner joins by `InstallSnapshot` with a memo and cached routes present), **restart** (authority, then view; a session released while down is never active), **leader change** (view closes within one lease under paused time, reopens on the new leader's watermark).
  - **Settings read-your-write:** save through F with F's apply held; `AppliedView::setting` on F answers the new value (authority) and returns to the view after the next applied watermark.
  - **Same-epoch renewal:** the next read returns the new lease, `owner_epoch` unchanged, zero consistent calls.
  - **Legacy layout:** the route view answers authority. **SQLite:** no proof object; every read is a store read (§4.2 "Standalone" as revised).
- *Daemon:* **no clock in a `View` entry** — under steady renewals of *other* sessions and paused time advanced 10 s, lookups cost local reads only and zero consistent calls (old code: one quorum read per second); **degraded mode** — with no proof, quorum reads are one per session per lease, not one per request; **cache-miss storm** — 4,096 random ids under one proof, 0 consistent reads; `PUT /api/v1/settings` through a lagging follower returns the saved values (E1c's daemon harness: the one E1b case deferred, record §2); the settings inventory.

**Guards — pass today, must keep passing; each gets a mutation counterfactual in the PR:**
- **A fence never reads the view:** a counting double asserts zero view calls across `authoritative_route_resolution_before`, `control_route`, `authorize_prepared_media_before`, `verify_authority`, `status.rs:64, 141`, the three receipt fences, takeover and release; plus a source test on `ms.rs:2463-2491, 2620-2641`. Mutation: point one at `applied`.
- **Pending-release ordering** (`ms.rs:2613`) and **lease expiry by clock** with no read.
- Source census: no unqualified `DELETE`, no `WITHOUT ROWID` on registered tables.

## 9. Acceptance — what is read and how

From `/metrics` on each node; "built on effort" and "accepted on fleet" stay separate. Paul deploys; the PR carries the GPT prompt for the client run.

1. **Per comparable request:** `Δ plurx_http_store_reads_total{route_group,class="authority_read",role}` ÷ `Δ plurx_http_route_seconds_count{route_group,role}` for `playback` and `settings`, for each role present (`standalone, voter, learner, remote_authority, fenced, unknown`; the leader is a `voter`), before and after, over one scripted scenario (a direct play, a remote-owner play through a second ingress, a settings page left open; 10 minutes each).
2. **Per session:** the same delta ÷ the time-integral of `plurx_transcode_sessions_active` (`http/system.rs:6587`) over the window.
3. **Cache:** `plurx_media_session_route_lookups_total{result}` (`ms.rs:1028-1033`; fix its "one-second" help text) gains `view` and `observed`; `plurx_applied_view_reads_total{view,source,reason}`, `reason ∈ {ok, no_proof, image_replacing, floor, revoked_during_read, legacy_layout, local_error, unregistered, reload_in_flight}` (`unregistered`, added in E1a: a configuration error; `reload_in_flight`, added in E1b: a snapshot that met another reader's memo reload). Until E1b/E1c, view-side rows read 0: not a measurement.
4. **Induced fences and degraded mode** (lab): a leader stop, a snapshot install on a learner, a release during play, a takeover, and a rolling upgrade with one old leader. Record the authority-read rate during each (expected ceiling: one per session per second) and its return to steady state; `plurx_playback_rolling_terminal_events_total{event="authority_fence"}` stays 0 outside the induced cases.
5. **Target:** the review's order-of-magnitude drop for `settings` and `playback` at equal session counts. The stay-list is now larger (control exchanges, status `:64, :141`, receipt fences). E1a's baseline states the floor it leaves **before** E1c is sized; report that floor rather than moving a fence (§12-4).
6. **Developer row "Applied view"** (`http/developer.rs:409`; `crates/plurxd/src/web/pages/settings-developer.js`, `devGraduation`): proof present, generation stable, last fallback reason, view/authority counts since start (computed at read time, no timer); reads "no readers yet" until E1b/E1c land. Advisory, no switch.

## 10. Out of scope

Auth, key and token reads; watch state (K-04 M2); `playback_planning_snapshot`, the catalog fan-out and create's read budget (D3); telemetry and priors (already local); extending `WriteAck` to transactions (the floor makes it unnecessary); per-row invalidation unless §12-7 says otherwise; the watermark cadence, lease and `bounded_replica_max_lag_entries`; media-pool wire envelopes (no field added); any replicated schema step; background-worker settings reads.

## 11. Before pushing

`make paperwork`; `scripts/scrub-infra-names --check`; `python3 -m unittest tests.operations.test_hiqlite_patch_ledger tests.validation.test_rolling_producer_ownership_inventory`; store contract on both backends; before/after readings in plan §6 row E1.

## 12. Open decisions for Paul

1. **Settings that gate work move off quorum reads.** The 2026-09 cluster contract keeps them `Authority`; plan §1 says only fences are. This contract follows the plan, keeps §6.3's stays-list on quorum, and gives the saving node read-your-write. Cost: a saved setting reaches *other* nodes' requests up to one second late. Recommended: confirm.
2. **Vendor patch 27 turns on rusqlite `hooks`** — a permanent policy row. The alternative is an applied-index-only mark with no hook. For routes it may be nearly as good, because `media_sessions` is written about once a second anyway; for settings it would reload the memo at the cluster's write rate unless the reload compares content first. Recommended: the hook; E1a's baseline can overturn it for routes.
3. **E1 ignores `cluster.bounded_replica_reads`** and adds no switch. Honouring it would make it a gate on playback routing.
4. **The 10× target will probably not be met on `playback` by E1 alone.** Control exchanges (`http/hls/control.rs:1681`), status `:64, :141` and the receipt fences stay on quorum reads by the plan's own rule. Accept the measured floor, or open a follow-up that redesigns those brackets (for example a non-mutating control exchange that takes the quorum read only when it is about to write). Recommended: accept; decide on E1a's numbers.
5. **Status:** only the first resolution (`http/hls/status.rs:39`) goes view-first. On an ingress node that removes the quorum read; on the owner one remains per call (`:64`). Recommended: move `:39`.
6. **Background-worker settings reads** (≈ 80 sites) stay on quorum reads. Recommended: leave until a measurement asks.
7. **Per-row route invalidation.** Per-table invalidation keeps cache lifetime near one second; the hook already receives the rowid. Decide after E1a reports local re-reads per request and read-pool wait. Recommended: per-table unless those numbers show contention.

**Dispositions (2026-11-12, on E1a's fleet baseline; record §3).** 1 and 6 as E1b recorded; 2: the hook; 3: no switch; 4: accept the measured floor (verdict, control, status bracket and receipt fences stay; redesigning them is a follow-up); 5: `:39` moves; 7: per-table (no read-pool contention). The five `receipt_candidate` reads stay.

## 13. Review disposition

Every finding of [PLAYBACK-START-E1-REVIEW.md](../reviews/PLAYBACK-START-E1-REVIEW-2026-11-11.md) was checked against source; none is rebutted in substance. Two are fixed by a different mechanism than the one proposed, noted below.

**Blockers.**
- **B1 fixed** — §4.2 "The floor", §4.4 `Observed`. Neither proposed fix was taken as written: comparing `(owner_epoch, state)` has no order across incarnations, and a commit index needs a wire change for transactions and still leaves quorum-read results uncovered. A watermark confirmation started after the fact, once applied, proves the replica contains it; that needs no vendor wire change and covers both cases. Test in §8.
- **B2 fixed** — §3 names the three senders; §4.1 pairs the flag in `sm.rs` at both pool-reconnect points; §5 rows; §8 start-up test. The parity is kept, but owned by one guard on the reconnect side, so there is no cross-thread pairing.
- **B3 fixed** — §4.3: `settings_dto` takes its source; `PUT` stays authority; §6.3.
- **B4 fixed** — §6.3 keeps the three handoff sites and both `control.rs` sites on quorum; §4.3 adds settings read-your-write through the floor (the write instant rather than `WriteAck`: one mechanism, and it also covers statements that are not `execute`).
- **B5 fixed** — §6.1: only `status.rs:39` moves; §12-5 re-asked.
- **B6 fixed** — §6.1: the three fences stay; the other five stay by default with an explicit bar for moving.

**Should-fix.** 1 fixed (§4.4 `Observed` lease, §2, §9-4). 2 fixed (§4.2 "Standalone", §5, §8). 3 fixed (§4.1 publish before the reply). 4 fixed (§4.1 DDL, hook limits). 5 fixed (§4.1 registered slots; `media_playback_pointers` dropped). 6 fixed (§4.4, §5, §8). 7 fixed as a stated expectation plus E1a measurements and §12-7; per-row is not scoped in without numbers. 8 fixed (§8 split into new behaviour and guards; hold mechanism and feature flag corrected). 9 fixed (§9-1, §9-2).

**Nits.** 1 fixed (§4.2 "Bound"; comment amended in E1c). 2 noted (§3). 3 fixed. 4 fixed (§4.4-3). 5 fixed (§9-6). 6 fixed (§12-2).

## 14. Build records

What E1a and E1b built, and where each departs from this text: [PLAYBACK-START-E1-RECORD.md](PLAYBACK-START-E1-RECORD.md) §1 (E1a, 2026-11-11), §2 (E1b, 2026-11-11) and §3 (E1c, 2026-11-12). References to "§14" above point there.
