# Playback start — E1 build contract (replication classes as designed)

**Status:** open — design and build contract for lane **E1** of [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md) revision 3 (§3 E1, §2 row "§1.5/§5.5 consensus for everything"). Not built. Needs the Fable design review the plan requires (§5 "A1+B1 and E1 designs: Fable again before build") and Paul's answers to §12 before a build agent claims it. Where this contract and the plan disagree, the plan's §0 rulings win; raise the conflict in the lane PR.
**Verified against:** `main @ 0c57d812` (2026-11-11), from source only — nothing here was compiled or run. Line numbers are from that SHA; re-anchor with `scripts/outline` and `rg` before editing.
**Author:** Claude (claude-fable-5-2, session 01BCWL9xwmpSjitHqZMTRAht).
**How to work:** as [PLAYBACK-START-BUILD.md](PLAYBACK-START-BUILD.md) §0.

## 1. Goal

Settings and media-session routes are answered from this node's own applied replica whenever the node can prove that replica is current, and from a quorum read only when it cannot. Fences keep their quorum reads. The route cache stops expiring on a clock and is invalidated by what the replica applied.

Measured problem (review 2026-11-07 §2): `plurx_http_store_reads_total{class="authority_read"}` — playback 168k, settings 82k; "~0.7 quorum reads/s on playback alone, continuously", which is one route-cache miss per second per watched session (`ROUTE_CACHE_TTL = 1 s`, `media_sessions.rs:143`).

## 2. Constraints that bind the build

- **Plan §1 target:** "Settings, routes and catalogue are node-local views of the applied raft log; only fences are consistent reads." **Plan §3 E1:** the view survives lag, snapshot replacement, release, lease expiry and same-epoch renewal; fences and the pre/post pending-release checks stay consistent; a fence never reads the view.
- **This supersedes an older rule for settings only.** `docs/cluster/CLUSTER-PERFORMANCE-PLAN.md` §3.2 and `BOUNDED-REPLICA-READS-ROLLOUT.md` §4 list "settings that gate work" as `Authority`. E1 moves most of them. That is a contract change, not an implementation detail — §12 decision 1. Both documents get a dated correction in the E1b PR.
- **No feature gate.** The view is on when built. `cluster.bounded_replica_reads` (`store/mod.rs:2144`) is the catalogue reader's preference and is **not** consulted by E1 (§12 decision 3). One advisory row in Settings → Developer (§9).
- **Store twins change together** (SQLite + hiqlite), tested through `crates/plurx-core/tests/store_contract.rs` with `--features hiqlite-contract-tests`.
- **hiqlite `$N` placeholders first appear in numeric order** (`store/placeholder_census.rs`); every new local statement obeys it.
- **Consistent-read census** (`store/consistent_read_census.rs:38-90`): E1 adds no `query_consistent*` site, only local (`query_map`) twins, so `CONSISTENT_READ_SITES` should not move; if a count moves, change the table in the same commit.
- **Vendored fork ledger:** any change under `vendor/hiqlite` needs a `PLURX-PATCH.md` row and bullet and a correct `PLURX-FILES.toml` (`tests/operations/test_hiqlite_patch_ledger.py`).
- **Rolling-producer census** (`tests/playback/rolling-producer-owners.toml`, `tests.validation.test_rolling_producer_ownership_inventory`): recount for any new spawn or timer. **This design adds none** — no task, no interval, no sleep. If the build finds it needs one, stop and raise it.
- **No band-aids.** No TTL re-introduced under another name, no watchdog that re-reads "just in case", no retry loop waiting for the replica to catch up. A reader that cannot prove the view takes the quorum read it takes today.

## 3. What exists today (the seams)

**Read classes.** `StoreOperationClass::{LocalRead, AuthorityRead, Write}` (`store/hiqlite.rs:686-690`) is what `plurx_http_store_reads_total{route_group,class,role}` counts per request (`http/mod.rs:132, 208-221`, attribution middleware `:673-700`, route groups `:264-650`). An authority read is `TimedClient::query_consistent_map[_scoped]` (`hiqlite.rs:1371-1427`) → `hiqlite::Client::query_consistent_map` (`vendor/hiqlite/src/client/query.rs:46-54`) → `raft.ensure_linearizable()` then the local read pool (`vendor/hiqlite/src/query/mod.rs:19-30`); on a follower it travels to the leader first. A local read is `query_map` (`hiqlite.rs:1429`; `client/query.rs:93`).

**The proof that a local read is safe already exists.** A per-node loop samples a leader quorum watermark every 500 ms with a 1 s monotonic lease (`cluster/migration.rs:5548-5550`, loop `:6916`, spawned once in `passive_metrics_loop` `:7171-7223`). `PassiveRaftMetrics::eligible_serving_proof()` (`:6169`) returns a `ServingProof` only when this node has applied a quorum-confirmed commit index of the current term/leader/observation epoch inside that lease; `serving_proof_remains_valid` (`:6325`) rechecks it; `run_bounded_replica_after(max_lag, min_applied_index, read)` (`:6112-6133`) is the read-your-write form. The proof is void after a restart, a term or leader change, or lease expiry (`WatermarkView::bound_to_current_generation`, `:5791-5808`). `CatalogueReader` (`store/mod.rs:6398-6531`) uses it for catalogue rows and already for presentation-only settings (`display_settings`, `:6908-6927` → `local_get_settings`, `hiqlite.rs:1747-1760`). `local_setting_hint` (`mod.rs:2438`, `hiqlite.rs:6184`) is an unproven local read used only to decide whether a quorum read is owed.

**The node-local applied view is the local hiqlite replica.** It is fed by the raft log (`StateMachineSqlite::apply`, `vendor/hiqlite/src/store/state_machine/sqlite/state_machine.rs:1097-1330` → the single writer thread, `writer.rs:285-880`, which records `sm_data.last_applied_log_id` per request at `:302, 351, 432, 607, 645, 766, 771, 776, 844`), and it bootstraps from a snapshot (`install_snapshot`, `state_machine.rs:1397-1528`: stage file → `WriterRequest::SnapshotApply` → `conn.restore("main", …)` at `writer.rs:681-737` → `reconnect_read_pool` → publish pointer; `snapshot_recovery_pending` refuses `apply` meanwhile). E1 does not build a second materialisation. It adds the one thing the replica lacks: a signal, published by the writer itself, of **what changed and when the image was replaced**.

**What is missing.** The only local change signal is openraft's metrics watch (`Client::local_db_raft_metrics`, `vendor/hiqlite/src/client/mgmt.rs:287-294`), which is updated by the raft core after the writer commits and says nothing about which table changed. The fork has no `update_hook`/`commit_hook` (rusqlite's `hooks` feature is off, `vendor/hiqlite/Cargo.toml:306-322`).

**Routes today.** `MediaSessions::raw_route_before` (`media_sessions.rs:2562-2615`): pending-release check → cache → per-shard single flight → cache → `store.media_session_route` (`:2602`, a quorum read: `route_by`, `store/hiqlite_sessions.rs:2028-2045`) → `cache_queried_route_result` guarded by `route_generations` (`:3037-3063`) → pending-release check. Entries expire at `now + ROUTE_CACHE_TTL` (`insert_cached_route`, `:3776-3797`); every insert sweeps expired entries under the map lock (`:2974, 3023, 3044`). Classification (`classify_durable_route(route, node, unix_ms())`, `:2434`) happens on every use, outside the cache, so a cached row's lease is judged against the clock each time. Locally authored facts enter through `cache_route` (`:2952`), `cache_route_if_generation` (`:2966`) and `cache_terminal_route` (`:2995`) from activation, takeover and release (`http/hls/create.rs:3795, 4865, 4991, 5357, 5907`; `http/hls/control.rs:1080, 1172`; `http/internal_media_sessions.rs:852`; `media_sessions.rs:578, 2751, 2810, 3003, 4574`).

**Device profiles are not a replicated read.** They are an embedded TOML parsed once per process (`plurx-core/src/playback/mod.rs:38-49`); client capabilities arrive in the request. No table holds them. E1 has nothing to move; the review's wording counted them with settings. Node capability rows (`cluster_node_capabilities`) belong to D3's catalog work.

## 4. Design

### 4.1 The applied mark (vendored hiqlite, new patch row 27)

A process-local, lock-free structure owned by the SQLite writer thread and readable without I/O:

```rust
pub struct AppliedMark {            // Arc, created with the state machine
    generation: AtomicU64,          // even = image stable; odd = image being replaced
    applied_index: AtomicU64,       // last log index whose effects are committed locally
    tables: [AtomicU64; 64],        // revision per table slot
}
pub fn applied_table_slot(table: &str) -> usize;              // stable hash % 64, exported
impl Client { pub fn local_db_applied_mark(&self) -> Result<AppliedMarkHandle, Error>; }
```

- **Row changes.** The writer registers `Connection::update_hook` once (needs rusqlite `hooks`). The hook only ORs `1 << applied_table_slot(table)` into a writer-local mask. It never publishes: a revision visible before the commit would let a reader cache pre-commit data under the post-commit revision, which would then never be invalidated.
- **Publish after commit.** At the end of each writer arm that applies a log entry (the `:302…:844` sites), after the SQLite work has committed or rolled back: `tables[i].fetch_add(1)` for each masked slot, then `applied_index.store(index)`, clear the mask. A rollback publishes a spurious bump — harmless.
- **Image replacement.** `SnapshotApply` does `generation.fetch_add(1)` (→ odd) before `conn.restore` (`writer.rs:690`). The install task does the second `fetch_add(1)` (→ even) only after `reconnect_read_pool` returns (`state_machine.rs:1499-1507`). A failed restore leaves it odd: the view stays closed and every read is a quorum read until the process restarts. A backup restore takes the same path.
- **Local only.** Like `local_db_raft_metrics`, a remote client returns `Err` and never falls back to management HTTP; pin that with a source test in the style of `local_watch_accessor_has_no_remote_fallback` (`mgmt.rs:1410-1423`).
- **Known hook limits, each closed by a test.** SQLite does not call the update hook for `WITHOUT ROWID` tables (the store has none today) or for a `DELETE` with no `WHERE` (truncate optimisation). A source census over `STORE_SOURCES` refuses both for the viewed tables (`settings`, `media_sessions`, `media_playback_pointers`). Slot collisions only cause spurious invalidation.
- Files: `Cargo.toml`, `src/store/state_machine/sqlite/writer.rs`, `…/state_machine.rs`, `src/app_state.rs`, `src/client/mgmt.rs`, `src/lib.rs` — all already `patched` in `PLURX-FILES.toml`.

### 4.2 `AppliedView` (plurx-core, `store/applied_view.rs`)

The application-facing boundary, a sibling of `CatalogueReader` and constructed beside it (`AppState.applied`, next to `catalogue` at `plurxd/src/state.rs:800`).

One rule, used by every view read. With `M` = the mark slot for the table and `P` = `eligible_serving_proof()`:

1. Load `g0 = generation`, `r0 = tables[M]`. If `g0` is odd, or `P` is `None`, or a caller-supplied `min_applied_index` exceeds `applied_index` → **authority**.
2. Run the local statement (or take the memo, §4.3/§4.4).
3. Require `generation == g0` and `serving_proof_remains_valid(&P)`. Otherwise discard → **authority**. A local SQL error → **authority**.
4. A memo/cache entry made from the result is stamped `(g0, r0)` — the values loaded **before** the read. It is a hit only while `generation == g0 && tables[M] == r0` and step 1's proof test passes at the time of the hit.

"Authority" means the existing `Store` method, unchanged, under the caller's existing deadline. There is no wait for the replica and no retry.

**Staleness bound.** A view answer reflects every write committed up to a quorum-confirmed index that was confirmed at most one lease (1 s) ago, plus everything this replica applied since. That is the bound today's route cache gives (a quorum read up to 1 s old) and it is tighter than the catalogue reader's 64-entry budget. A node that cannot renew its proof — partitioned, deposed leader, stalled apply — loses the view within the lease and pays quorum reads, which then fail or succeed on their own merits.

**Surface (what callers and D3 use):**

```rust
impl AppliedView {
    pub async fn setting(&self, key: &str) -> Result<Option<String>, StoreError>;
    pub async fn switch(&self, key: &str, default: bool) -> Result<bool, StoreError>;
    pub async fn settings(&self, keys: &[&str]) -> Result<AppliedSettings, StoreError>;
    pub async fn route(&self, session_id: &str) -> Result<AppliedRoute, StoreError>;
    pub fn observation(&self) -> AppliedViewObservation;      // advisory: Developer row, metrics
}
pub struct AppliedSettings { pub values: BTreeMap<String, String>, pub digest: SettingsDigest, pub source: ViewSource }
pub struct AppliedRoute    { pub route: Option<MediaSessionRoute>, pub stamp: Option<ViewStamp>, pub source: ViewSource }
pub enum   ViewSource      { View, Authority }
```

`SettingsDigest` is a hash over the requested keys and their values in key order. It is the same on every node for the same committed values, survives restarts, and is what D3 carries in `StartPlan` "with its revisions" and compares on the owner — the mark's revisions are process-local and must never cross the wire. **D3 consumes `AppliedView::settings(&QUALITY_PLANNING_KEYS)` and `AppliedView::switch(keys::PLAYBACK_NETWORK_PRIORS, false)`**; the latter replaces the enable read at `http/network.rs:164`.

### 4.3 Settings view

A memo of the whole `settings` table: `Arc<BTreeMap<String, String>>` + stamp, reloaded by one local `SELECT key, value FROM settings ORDER BY key` when the stamp no longer matches. Readers share the `Arc`; a reload is single-flighted behind one async mutex (a second reader waits for the first reload instead of issuing its own). The table is written by settings saves and by job stamps/cursors (`state.rs` `job_stamp`, `:13475`), so reloads are minutes apart, not per request; `plurx_applied_view_reloads_total{view="settings"}` shows the rate.

Saving a setting is unchanged: the handler writes, then answers from its own quorum snapshot (`http/system.rs:3787, 4812`). `ReadYourWrite` for the *next* request through another node is bounded by the lease; the web client renders the save response, not a refetch — verify in `web/pages/settings*.js` and fix the client if it refetches.

### 4.4 Route view and the cache

`store.media_session_route` gains a local twin (`local_media_session_route`, same projection as `route_by`, `query_map`), SQLite passthrough. `raw_route_before` keeps its shape — pending-release check, cache, single flight, cache, read, generation-guarded insert, pending-release check (`:2567, 2575, 2594, 2613` do not move) — with three changes:

1. The read at `:2602` becomes `applied.route(session_id)`.
2. `CachedRoute { route, expires_at }` becomes `CachedRoute { route, origin }`:
   - `Origin::View(stamp)` — hit while §4.2 rule 4 holds for `media_sessions`. A renewal, release, takeover or expiry sweep applied on this replica bumps the slot and the next request re-reads locally. Nothing else invalidates it; nothing times it out.
   - `Origin::Authored(stamp_at_write)` — a row this process just committed (the existing `cache_route*` callers). Served while the proof holds **and** the slot still equals `stamp_at_write` (nothing has applied locally since, so nothing newer can be known here). Once the slot moves, the next lookup reads the view: if the view row matches the authored row on `(incarnation_id, owner_node_id, owner_epoch, state, publication_ready_at_ms)` or is terminal, the view row replaces it; otherwise the replica has not applied this process's own write yet and the lookup takes **one** quorum read, cached as `Origin::View` under the stamp loaded before it. This is the read-your-write rule; it needs no commit index (the fork's `WriteAck` covers `execute`/`execute_returning` only, `vendor/hiqlite/src/client/execute.rs:20-23, 54, 143`, and session mutations are transactions).
   - A quorum-read result (fallback) is cached as `Origin::View` under the pre-read stamp: it is at least as new as the replica, and the replica's catching up bumps the slot.
3. `ROUTE_CACHE_TTL`, `expires_at` and the three expired-entry sweeps are deleted. Size stays bounded by `MAX_ROUTE_CACHE_ENTRIES`; eviction picks any entry whose stamp is already stale, else the oldest insert (a monotonic sequence number, not a clock). `plurx_media_session_route_prune_entries` goes with the sweeps — remove it from the exposition and its tests in the same change.

`route_generations` and the release fences are untouched: they order this process's own activation/terminal publications against in-flight reads and are not replication state.

**Lease expiry** needs no invalidation: classification compares `lease_expires_at_ms` with the clock on every use, so a cached active row whose owner stopped renewing classifies as `OwnerTransition` on time, exactly as today. **Same-epoch renewal** is a row change like any other; the re-read picks up the new lease.

**Final verdicts stay quorum reads.** Every `authoritative_*` resolution (`media_sessions.rs:2463-2491, 2494-2506`) is unchanged: a 404/410, an owner-transition answer, or a relay reclassification is still proved by `store.media_session_route` between two pending-release checks (`:2467, 2487`). The view only ever *admits* a request to the owner it names; it never ends one.

### 4.5 SQLite (standalone)

Every read is already local and current. `AppliedView::authority(store)` (the `CatalogueReader::authority` pattern, `mod.rs:6419`) forwards to the `Store` methods with `source = Authority`; no memo, no mark. The route cache needs a stamp, so the SQLite store exposes a process-local revision bumped by its own media-session and settings mutations (one `AtomicU64` each, bumped after commit) and `Origin::Authored`/`View` work unchanged with generation fixed at 0. The store-contract tests run the same view contract on both backends; the lag, snapshot and leader cases are hiqlite-only.

## 5. State transitions and failure modes

| Event | Mark / proof | Reader behaviour |
|---|---|---|
| Process start | generation 0; no proof until the first watermark (≤ 500 ms + stagger) | Authority until `eligible_serving_proof()` is `Some`; memo and cache start empty |
| Start with a pending snapshot (crash mid-install) | recovery completes before the client is handed out — **builder verifies** in `start.rs`/`startup_cleanup.rs`; if not, start the mark odd and flip it when recovery ends | Authority while odd |
| Replica lags (apply behind the watermark) | proof `None` once no applied confirmation is inside its lease | Authority; view returns by itself when apply catches up |
| Required index not applied (`min_applied_index`) | — | Authority for that read |
| `InstallSnapshot` | odd during restore; even after pool reconnect; all stamps mismatch | Authority during; first read after reloads the memo and re-reads routes |
| Restore fails | stays odd | Authority until restart; Developer row and metric say `image_replacing` |
| Leader change / term change | proof void (`local_observation_epoch`/term mismatch) until a watermark from the new leader | Authority for that gap; stamps are still valid afterwards (the image did not change) |
| Deposed or partitioned node | cannot renew; proof gone within 1 s | Authority reads, which fail closed as today |
| Local SQL error or pool exhaustion | — | Authority for that read (no error surfaced from the view) |
| Release through this node | release fence set first (`:2697-2698` bumps `route_generations`); `cache_terminal_route` writes `Origin::Authored` terminal | pending-release checks reject before and after; then terminal from the overlay |
| Release/takeover through a peer | slot bumps when this replica applies it | cached row invalid at apply; before that, ≤ 1 lease stale — the request reaches the old owner, which refuses, and the relay's `authoritative_*` reclassification decides (`http/hls/relay.rs:27-52, 107-123`) |
| Owner stops renewing | no row change | classification by clock, unchanged |
| Rolling upgrade, old leader | watermark protocol version mismatch keeps the proof closed (`:5807`) | Authority, as the catalogue reader does |
| Remote-authority process (no local replica) | `local_db_applied_mark` is `Err` | `AppliedView::authority` |

## 6. Call-site classification

### 6.1 Routes — `media_session_route` (session-keyed)

| Site | Today | E1 | Reason |
|---|---|---|---|
| `media_sessions.rs:2602` (`raw_route_before`; reached from `route_resolution_before` `:2401`, `media_route_resolution_before` `:2452`, i.e. `http/hls/relay.rs:17, 22`) | quorum on cache miss | **view** | ordinary routing of a media request; the owner and the reclassification path are the fence |
| `http/hls/status.rs:39, 64, 141` | `authoritative_media_…` on each call | **view first** (`media_route_resolution_before`), quorum only to confirm a non-active verdict, the pattern at `relay.rs:14-52` | a positive answer is routing; only an ending needs proof |
| `http/hls/link_receipts.rs:492, 610, 627, 947, 1014`; `http/hls/prepared_link.rs:357, 426`; `http/hls/candidate_recovery.rs:86`; `http/system.rs:1176` | quorum | **view** | advisory binding/observation checks that already return `None` on any mismatch; a stale row only drops a measurement |
| `media_sessions.rs:2482, 2634` (`authoritative_route_resolution_before`, `control_route`) and their callers `http/hls/segment.rs:506`, `response.rs:353`, `relay.rs:31-40, 113-122`, `internal_media_sessions.rs:817, 826`, `http/hls/control.rs:1681`, `quality_control.rs:211`, `quality_schedule.rs:173`, `create.rs:2771` | quorum | **stays** | final status verdicts and pre-mutation reads |
| `playback_control.rs:2507` (`verify_authority`), `vod/shared.rs:209` (`terminal_route_durably_non_live`), `vod/serve/delivery.rs:914`, `http/internal_media_sessions.rs:998`, `http/hls/create.rs:4103`, `http/jellyfin/playback.rs:295, 331`, `http/shared_receiver_playback.rs:2160`, `http/shared_receiver_direct.rs:206` | quorum | **stays** | ownership/terminal proofs or reads that precede a mutation |

### 6.2 Routes — everything else stays

- `media_session_route_by_incarnation` — all ≈ 50 sites (`rg '\.media_session_route_by_incarnation\(' crates/plurxd/src`: `http/hls/create.rs` 8, `control.rs` 4, `transcode/manager/create.rs` 5, `source_actor.rs` 3, `media_sessions.rs` 5, `internal_media_sessions.rs` 3, the `shared_receiver_*` files, and singles). An exact incarnation read is an ownership or publication fence.
- `media_session_route_for_playback` (`media_sessions.rs:2544`; `http/hls/create.rs:3889`; `candidate_recovery.rs:549, 597`; `jellyfin/transport.rs:24`) and `staged_media_session_for_playback` (`media_sessions.rs:526, 554, 2528`; `control.rs:2596`; `transcode/manager/create.rs:4995`): prepared-publication authority while the control publication is fenced (`:2441-2447`).
- Every mutation (`renew_media_sessions`, `claim_media_session_takeover`, `end_media_session*`, `*_terminal_projection`, `abort_media_session_preparation`) and the takeover switch read (`media_sessions.rs:5536`, already hint-gated at `:5631, 5639`).
- The serving fence (`plurxd/src/serving_fence.rs`) reads the proof, never the view.

### 6.3 Settings — 178 production call sites (`get_setting` 142, `get_setting_pair` 11, `get_settings` 7, `settings_snapshot` 15, `local_setting_hint` 3) plus `playback_planning_snapshot`

Rule: a read **moves** when its answer only chooses how to serve, plan, present or whether an advisory step runs, and a ≤ 1-lease-old value produces at worst the behaviour of a request that arrived one second earlier. A read **stays** when it precedes a write in the same operation, decides authentication/authorisation/membership/ownership, or runs before the node serves.

| Group (file: lines) | E1 | Notes |
|---|---|---|
| Playback request path — `http/hls/control.rs:1923, 2547`; `http/hls/preparation.rs:1921, 2442`; `http/hls/link_receipts.rs:215, 1251`; `http/hls/create.rs:2120`; `http/network.rs:164`; `http/stream.rs:82, 2495, 2647`; `http/preparation.rs:373, 380`; `http/chapter_thumbs.rs:185`; `http/internal_media.rs:278`; `http/shared_receiver_control.rs:329`; `telemetry.rs:1572` | **view** | per-request preference reads; the playback group's settings share |
| Planning/describe — `transcode/manager/describe.rs` (10 sites `:89…1219`), `plan.rs:1518, 1640, 1667`, `construct.rs:223, 320, 1019`, `create.rs:1071, 4508`, `transcode/planning.rs:67`, `content_encoding.rs:183`, `source_subtitles.rs:227`; `vod/serve/create.rs:955, 2659`; `vodencode.rs:799, 949, 964, 994`; `source_probe/fel_qualification.rs:968`; `transcoder_optimization.rs:92`, `comparison.rs:126, 129`; `playback_cache.rs:52`; `cachekeep.rs:835` | **view** | recipe/hwaccel/quality preferences |
| Polled pages in the `settings` group — `http/system.rs:77, 82, 87, 116` (server info), `:207` (features), `:420, 429, 436` (system info), `:2732` (`GET /api/v1/settings`); `http/developer.rs:416`; `media_pool.rs:1324, 1334, 1359` | **view** | the 82k; display and placement preference |
| `playback_planning_snapshot` (all 20 sites, e.g. `http/hls/create.rs:3341`, `transcode/manager/create.rs:752`) | **stays in E1** | one quorum read of file + settings; D3 collapses it onto `AppliedView::settings` + the catalogue reader with the digest |
| Save/mutation handlers — `http/system.rs:3667, 3787, 4812`; `get_or_init_setting` (3) | **stays** | pre-mutation / read-your-write response |
| Auth, trust, membership — `http/users.rs:135`; `http/remote.rs:181`, `remote/owner.rs:603, 700`; `http/remote/invitations*` (5); `sharing*.rs`, `http/sharing.rs`; `clock_offset.rs:748` (hint at `:755` stays a hint) | **stays** | revocability and admission |
| Ownership/admission — `media_sessions.rs:5536`; `http/offline.rs:56, 66`; `offline.rs:717`; `backup.rs` (3) | **stays** | offline ownership, takeover |
| Boot — `main.rs:1790, 3191, 3426` | **stays** | before the first proof exists |
| Background workers — `state.rs` (37: DV-disk policy `:4092-4529`, backfills `:7361-8126`, schedulers `:6497-7006`, `job_interval_or`/`job_stamp` `:13464, 13475`, …), `subtitle_*` (≈ 22), `live_tv*.rs`, `http/dvr.rs`, `library_search*`, `trakt.rs`, `watched.rs`, `http/comingsoon.rs`, `http/plex.rs`, `http/jellyfin.rs`, `http/annotations.rs`, `http/library_channels.rs`, `channel_subjects.rs` | **E1b-2, optional** | not on the acceptance metric (not request-attributed, or outside the two groups). Move only pure "enabled?/how?" polls; cursors, stamps and anything read-then-written stay. Each moved site is listed in the PR |

The builder turns this table into a pinned inventory test in the style of `bounded_catalogue_handler_inventory_keeps_reads_and_mutations_separate` (`http/tests.rs:1990`): the stays-list may not call `state.applied`, the moves-list may not call `state.store.get_setting*`.

## 7. Sub-lanes, order, size

E1 is three PRs into the effort branch, in this order. Total L.

| Lane | Content | Size | Depends |
|---|---|---|---|
| **E1a** — mark and view | hiqlite patch 27 (§4.1) with vendor tests and ledger; `AppliedView` + SQLite passthrough + store-contract tests; metrics and the Developer row; **baseline split**: `plurx_media_session_route_reads_total{site,source}` (bounded `site` set: routing, status, control, verdict, observer) so the "before" reading says which sites make the 168k. No call site moves | M | — |
| **E1b** — settings | settings memo; move §6.3 view rows; inventory test; correct the two cluster docs. E1b-2 (workers) is a follow-up PR only if Paul wants it | M | E1a |
| **E1c** — routes | local route twin; `Origin` cache; delete `ROUTE_CACHE_TTL` and sweeps; status view-first; observer sites | M–L | E1a |

E1b and E1c may run in parallel after E1a. D3's applied-settings reads start after E1b.

## 8. Tests (each fails against `0c57d812`)

Vendor (`vendor/hiqlite`, patch 27):
- A committed write to table T bumps `tables[slot(T)]` and `applied_index`, never before the read pool can see the row (hold the commit with the existing validation precommit hold, `writer.rs:44-65`, and assert the revision has not moved).
- A snapshot restore makes `generation` odd for its duration and even after reconnect; a failed restore leaves it odd.
- Source test: the accessor has no remote fallback and no `.await`.

Store contract (both backends unless marked):
- `applied_view_matches_authority_and_falls_back_exactly_once` — modelled on `bounded_catalogue_reader_matches_authority_and_falls_back_exactly_once` (`store_contract.rs:29949`): with a valid proof, N settings/route reads cost 0 consistent calls; with the proof revoked after the local read (`validation_revoke_bounded_proof`, `migration.rs:6013`), the result is discarded and exactly one consistent call follows.
- *(hiqlite, three voters)* **stale replica during release:** hold apply on follower F (`validation_wait_for_apply`), end the session through the leader; F's view read must not return the active row once its proof lapses, and before that the relay path's reclassification returns the terminal verdict. Release the hold: F's cached row is invalid without any clock advance.
- *(hiqlite)* **stale replica during replace:** same with a takeover (`owner_epoch + 1`) and with a prepared successor commit.
- *(hiqlite)* **snapshot replacement:** a learner joins by `InstallSnapshot` while a memo and cached routes exist; nothing stamped before the install is served after it.
- *(hiqlite)* **restart:** stop and start a node; the first reads are authority, then view; a session released while it was down is never served as active.
- *(hiqlite)* **leader change:** depose the leader; its view closes within one lease (paused tokio time) and reopens only on a watermark from the new leader.
- **Same-epoch renewal:** renew a lease; the next view read returns the new `lease_expires_at_ms` with `owner_epoch` unchanged and zero consistent calls. **Lease expiry:** no renewal; classification turns `OwnerTransition` at the durable boundary with no read at all.
- Source census: no unqualified `DELETE` and no `WITHOUT ROWID` on the viewed tables.

Daemon (`cargo test -p plurxd --bin plurxd`):
- **A fence never reads the view:** a counting `AppliedView` double asserts zero view calls across `authoritative_route_resolution_before`, `control_route`, `authorize_prepared_media_before`, `verify_authority`, takeover claim and release; plus a source test that the function bodies at `media_sessions.rs:2463-2491, 2620-2641` name `self.store.media_session_route` and not `applied`.
- **Pending-release ordering:** with a release fence installed between the view read and its return, `raw_route_before` errors (`:2613`), as today.
- **No clock in the cache:** advance paused time by an hour with no applied change — still a hit, zero store calls. Counterfactual: old code misses after 1 s.
- **Authored overlay:** activation on a follower whose replica has not applied it — the next lookup serves the authored row; after an unrelated `media_sessions` change applies first, exactly one consistent read resolves it; after its own row applies, zero.
- **Cache-miss storm:** 4,096 random session ids under one proof issue 0 consistent reads (today: bounded by the shards but all quorum).
- Settings inventory test (§6.3) and `http/tests.rs` exposition updates.

## 9. Acceptance — what is read and how

All from `/metrics` on each node; "built on effort" and "accepted on fleet" stay separate (plan §6). Paul deploys; the after-reading needs a client run a GPT session can drive — the PR carries that prompt.

1. **Per comparable request:** `Δ plurx_http_store_reads_total{route_group,class="authority_read",role}` ÷ `Δ plurx_http_route_seconds_count{route_group,role}` for `playback` and `settings`, per role (voter, learner, leader), before and after, over the same scripted scenario (one direct play, one remote-owner play through a second ingress, a settings page left open, 10 minutes each).
2. **Per session:** the same delta ÷ active session-minutes (`local_active_sessions`), so equal session counts are compared, not wall time.
3. **Cache misses:** `plurx_media_session_route_lookups_total{result}` (`RouteLookup::{CacheHit, SingleFlightHit, Store}`, `media_sessions.rs:1028-1033, 2574-2599`; fix its "one-second route cache" help text) gains `View`; target `Store` ≈ authored-overlay resolutions + fallbacks only. `plurx_applied_view_reads_total{view,source,reason}` with `reason ∈ {ok, no_proof, image_replacing, min_index, revoked_during_read, local_error}` (fixed set).
4. **Induced fences:** in the lab, one leader stop, one snapshot install on a learner, one release-during-play and one takeover. Record the authority-read spike per event and that it returns to the steady rate; `plurx_playback_rolling_terminal_events_total{event="authority_fence"}` stays 0 outside the induced cases.
5. **Target:** the review's — `authority_read` for `settings` and `playback` down by an order of magnitude at equal session counts. E1a's split says in advance whether the stay-list (control exchange, verdicts, incarnation fences) leaves a floor above that; if it does, report the floor rather than moving a fence (§12 decision 4).
6. **Developer row "Applied view"** (`http/developer.rs:409`, `web/pages/settings-developer.js` with its `devGraduation` text): proof present, image generation stable, last fallback reason, view/authority ratio for the last minute; "waiting on: fleet read-cost reading". Advisory, read-only, no switch.

## 10. Out of scope

- Authentication, API-key and session-token reads (`BOUNDED-REPLICA-READS-ROLLOUT.md` §4 "no ordinary-auth cache") and watch state (K-04 M2 owns it).
- `playback_planning_snapshot`, the catalog fan-out and create's read budget — D3.
- Telemetry, network priors and playback events — already node-local (`store/telemetry.rs`, `http/network.rs:163`).
- Extending `WriteAck` to transactions, per-row (rather than per-table) invalidation, and any change to the watermark cadence, lease or `bounded_replica_max_lag_entries`.
- Media-pool wire envelopes: no field is added (H2's rule); the mark and stamps never leave the process.
- A replicated schema step: none is needed.

## 11. Before pushing

`make paperwork`; `scripts/scrub-infra-names --check`; `python3 -m unittest tests.operations.test_hiqlite_patch_ledger tests.validation.test_rolling_producer_ownership_inventory`; store contract on both backends; before/after readings in plan §6 row E1.

## 12. Open decisions for Paul

1. **Settings that gate work move off quorum reads.** The 2026-09 cluster contract keeps them `Authority`; plan §1 says only fences are. This contract follows the plan and keeps auth, trust, membership, ownership and pre-mutation reads on quorum (§6.3). Recommended: confirm. The cost is that a saved setting reaches other nodes' requests up to one second late.
2. **Vendor patch 27 turns on rusqlite `hooks`** and adds a writer-published mark — a new permanent Plurx policy row in the fork. Recommended. The alternative without the hook is an applied-index-only mark: still correct, but every applied entry of any kind invalidates the settings memo and every cached route, so local re-reads track the cluster's write rate.
3. **E1 ignores `cluster.bounded_replica_reads`** and adds no switch of its own (the rollout doc already recommends removing that switch). The alternative is to honour it, which makes it a gate on playback routing.
4. **If the stay-list leaves a floor above 10×** (most likely the per-exchange `control_route` read, `http/hls/control.rs:1681`), accept the measured floor in E1, or open a follow-up that lets a non-mutating control exchange resolve through the view and take the quorum read only when it is about to write. Recommended: accept the floor here; decide the follow-up on E1a's numbers.
5. **Status polls become view-first** (`http/hls/status.rs:39, 64, 141`), with a quorum read only to confirm an ending. Today each of those calls is a quorum read by design ("same exact preparation authority as media", `media_sessions.rs:2443`). Recommended: move.
6. **Background-worker settings reads (E1b-2)** are not needed for acceptance. Do them now (one mechanical PR, ≈ 80 sites to classify) or leave them on quorum reads until a measurement asks for it. Recommended: leave.
