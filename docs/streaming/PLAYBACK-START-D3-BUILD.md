# Playback start — D3 build contract (create collapse)

**Status:** open — design and build contract for lane **D3** of [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md) revision 3 (§3 D3, sequenced after E1 in §4). Revision 2: the adversarial review [PLAYBACK-START-D3-REVIEW-2026-11-12.md](../reviews/PLAYBACK-START-D3-REVIEW-2026-11-12.md) is applied (§13) with the coordinator's decisions (§12). D3a can be claimed now; D3b onward needs Paul's answers to §12. Where this contract and the plan disagree, the plan's §0 rulings win; raise the conflict in the lane PR.
**Verified against:** `main @ 640c6bee8` (2026-11-12), from source only — nothing was compiled or run. Re-anchor line numbers with `scripts/outline` and `git grep` before editing.
**Author:** Claude (claude-opus-5-5, session 01BCWL9xwmpSjitHqZMTRAht). **How to work:** [PLAYBACK-START-BUILD.md](PLAYBACK-START-BUILD.md) §0.

Short paths: `hc.rs` = `crates/plurxd/src/http/hls/create.rs`; `tc.rs` = `crates/plurxd/src/transcode/manager/create.rs`; `cand.rs` = `…/transcode/manager/candidates.rs`; `desc.rs` = `…/transcode/manager/describe.rs`; `mp.rs` = `crates/plurxd/src/media_pool.rs`; `ms.rs` = `crates/plurxd/src/media_sessions.rs`; `ims.rs` = `crates/plurxd/src/http/internal_media_sessions.rs`; `act.rs` = `crates/plurxd/tests/cluster_activity.rs`; `av.rs` = `crates/plurx-core/src/store/applied_view.rs`; `hs.rs` = `crates/plurx-core/src/store/hiqlite_sessions.rs`; `hq.rs` = `crates/plurx-core/src/store/hiqlite.rs`; `inv.rs` = `crates/plurxd/src/http/applied_settings_inventory.rs`.

## 1. Goal

Plan §1: "Create is one snapshot read → one pure decision → at most two durable writes … Nothing advisory or speculative runs before the answer. Create p99 < 1 s on the fleet."

D3 removes the part of that gap that is a root cause and not a fence: **the create path re-reads, by quorum, planning inputs it already holds or can read once** — the planning snapshot, settings, the stored probe and the file row. The session writes and their read-backs are a separate question, out of D3's scope (§10, §12).

**What main already pins.** `create_path_store_operations_are_pinned_per_shape` (`act.rs:1933`) measures (authority, local, writes) for the create handler and the activation task it awaits, on a two-voter cluster with the ingress as owner (`act.rs:2224-2238`):

| Shape | Authority | Local | Writes | What the writes include (test comment `act.rs:2224-2229`) |
|---|---|---|---|---|
| First play, VOD index | 25 | 1 | 6 | claim, owner assignment, activation (prepare, `BLOCKED`), Confirm, publication, … |
| Reopen, new position | 33 | 1 | 8 | the above + predecessor projection and handoff |
| Auto encode, VOD | 30 | 1 | 8 | the above + foreground source custody register and activate |
| Index miss, rolling copy | 25 | 1 | 9 | custody + `suspend_playback_analysis_interest` |

The bodies carry neither caps nor an intent (`act.rs:2150-2197`), so the catalog read and `record_desired_selection` are not in these numbers. Which Store method makes each count is not known: **D3a's first deliverable is that breakdown** (§7).

**Fleet baseline** (a busy voter, build `7929c2439`, 10.6 h): `playback` made 52,307 authority reads over 9,187 requests; 27,151 are session-route reads (E1); ≈ 25,150 are unattributed. D3a's method label attributes them.

**Re-reads in scope, from code reading** (D3a confirms or corrects per shape): planning snapshot `hc.rs:3341`, then again in the local catalog (`mp.rs:1985`), the candidate re-validation (`tc.rs:752` via `:849`), `vod_preparation_snapshot` (`tc.rs:1242`), `try_vod_session` (`tc.rs:4384`), `rolling_start_inputs` (`transcode/manager/start.rs:67`), the fallback (`tc.rs:964`); settings `desc.rs:140` (typeless), `mp.rs:1362` (placement), `tc.rs:4660` (`vod_settings`), `tc.rs:1071` (live recovery); probe `hevc_census.rs:64` via `hc.rs:2656`, `tc.rs:3038`; file `tc.rs:973`. A remote owner repeats the snapshot at `cand.rs:1783` and `tc.rs:849` and the setting at `ims.rs:188`. Before the answer the create also lists media peers and fans out offers (`mp.rs:1661-1700`) and catalogs (`mp.rs:1468-1530`); D3 counts them (§9-4) and changes neither (§12-C5).

## 2. Constraints that bind the build

- **Rulings and rules.** No feature gate: one advisory Developer row (§4.6). Root causes: no watchdog, no "just in case" re-read, no retry loop around the view. Every hardware-using job stays attributable (a held probe that still runs is named in the start's G1 phases). Processed DV is a post-first-frame upgrade, and the create still needs processed descriptions to map a named processed id to its ordinary rung (`hc.rs:2199-2225`): D3 keeps computing them. One copy engine: D3 changes no engine (§3.6).
- **E1's view and inventory.** `AppliedView` rules 1–4 and the floor apply unchanged; a fence never reads the view. `inv.rs` `STAYS` (`:199-228`) keeps `transcode/`, `vod/`, `vodencode.rs`, `source_probe/` and `ims.rs` free of view reads; D3 hands those files values and pins the exceptions as caller chains (§6).
- **Media-pool wire.** `tests/operations/test_media_pool_wire.py` refuses a strict-envelope change without a `PROTOCOL_VERSION` bump (`mp.rs:37`, now 9). D3 changes no envelope.
- **No schema version bump.** A cluster schema bump makes voters on the old version refuse to open (`hq.rs:7452-7466`). D3's one schema change is additive triggers with their own extension marker (§4.1).
- **Store twins** change together, through `crates/plurx-core/tests/store_contract.rs`. Replicated statements in one transaction do not roll back together (`hs.rs:2522-2531`): every statement carries its own predicate.
- **Consistent-read census** (`store/consistent_read_census.rs`, 365 sites in 29 files): D3 adds no `query_consistent*` site; a removed site lowers its row in the same commit.
- **Formal mappings.** `verification/{durable-jobs,operation-progress}/mapping.json` pin `hq.rs`, `store/sqlite/mod.rs`, `transcode/manager/construct.rs` and `produce.rs` by whole-file digest. A lane that edits one runs `scripts/check-formal`, refreshes the digest and states in its record that the anchored text is unchanged.
- **Rolling-producer census:** D3 adds no spawn and no timer.
- **E1c** (`claude/ps-e1c`, routes) is in flight; D3 designs nothing around route reads, and D3c rebases after E1c because both edit `hs.rs`.
- **A1/B1** (PR #1525 to `effort/playback-start-phase2`): D3 states the interface it assumes (§3.6) and edits nothing in `vod/` or `transcode/source_actor.rs`.

## 3. What exists today (the seams)

**3.1 Entry points that reach the shared helpers.** HTTP create `create_with_purpose_inner` (`hc.rs:3235`) runs the ingress plan and, for a local owner, `create_cluster_session` → `create_cluster_session_with_priority` (`tc.rs:360, 435`) → `create_session_inner` (`:767`). A remote owner enters at `ims.rs::start` (`:164`) → `create_cluster_session_for_receiver` (`ims.rs:217`). The same `create_cluster_session_with_priority` is entered by the prepared successor (`create_cluster_prepared_session`, `hc.rs:5632`). Not creates, but sharing helpers: takeover (`attempt_takeover`, `ms.rs:6664`; restore at `:6806`, live-recovery switch at `:6746`), resurrection from a recipe (`resurrect_vod_from_recipe_before`, `tc.rs:5440`; restore `:5527`, `vod_settings` `:5568`, `get_file` `:5581`), the rate-control refresh (`requested_rate_control`, `desc.rs:820`, callers `:844, 863, 925, 932, 978`), offline packaging (`encoder_for_offline_package`, `plan.rs:1632`), `produce.rs:448, 905` and `source_actor.rs:188, 1605` (`vod_settings`), and `live_tv_worker` (`mp.rs:1288`, `remote_placement_ready`). **Whether a helper may use plan values is a property of the call, not of the function.**

**3.2 The binding.** `PlanningBinding { generation, source_digest }` (`mp.rs:318-336`) hashes file, probe and reorder flag; `generation` moves only on `playback.*`/`transcode.*` writes (`store/mod.rs:1890-1906`, `CREATE TRIGGER IF NOT EXISTS`). `QUALITY_PLANNING_KEYS` (`cand.rs:2264`) holds three keys outside those globs — `cache.max_gb`, `vod.output_preparation`, `vod.rolling_retention` — so a change to them moves no binding. `PLAYBACK_INPUT_SCHEMA` is also the historical witness the lineage bridge recognises (`schema_lineage.rs:341-345, 429-437`): it is not edited. The snapshot is process-private (`session_request.rs:532`); `CandidateCatalogContext { caps, candidate, binding }` (`ms.rs:1331`) travels; a peer bounds only `generation ≥ 0` and a 64-character digest (`mp.rs:1616-1621`).

**3.3 The store and the view.** The snapshot is one quorum statement over `files`, `items`, `settings` and `playback_input_generation` (`hiqlite_media.rs:3074-3101`). The view registers `settings` and `media_sessions` (`av.rs:56`); `TableFloors` is indexed by `AppliedTable` (`av.rs:300-325`) and `Replicated::admit` fails `Unregistered` without a slot (`av.rs:419-437`). Settings writers are classified by `writes_settings` plus `SETTINGS_TRIGGER_TABLES` (`av.rs:331-345`). `files` is also written by triggers (`store/dv_title_jobs.sql:64, 77, 104, 134`), by the probe result applied through `background_job_commands` (`background_jobs_probe.rs:179`) and by cascades from `items` and `libraries` (`hiqlite_catalog.rs:36-38, 75`).

**3.4 The writes.** Claim (`hs.rs:2904`), owner assignment (`hs.rs:3055`), activation (`activate_session_under_authority`, called at `hc.rs:4813`; Store call `:5278`) inserting the route with `publication_ready_at_ms = BLOCKED` (`hs.rs:2535`; the owner's activation requires the request's `owner_node_id = $6`, `:2438-2443`), Confirm (`settle_media_session_activation`, `hc.rs:5331-5341`, `hs.rs:4243-4262`), publication requiring `publication_ready_at_ms = 0` (`hs.rs:4496-4524`), custody, predecessor projection and handoff. `BLOCKED` keeps a prepare that commits behind a timeout unservable until Abandon tombstones it (`hc.rs:5303-5308`). A remote owner makes activation and Confirm (`ims.rs:456`). With an intent, `record_desired_selection` (`hs.rs:3319`) is one more write plus a read-back (`desired_row`, `:3367`).

**3.5 The held probe.** Every encoded start probes the held descriptor (`probe_vod_source_once`, `plan.rs:483`; or `held_source_probe_json_bound`, `tc.rs:3070-3085`; phase `held_source_probe`, `:3092`). The node-local decode-fact cache keys on the source identity and a content digest (`decode_facts.rs:507-515`) and validates a seven-field tuple of the held descriptor, the path and the snapshot, including device, inode and `ctime`, hashing the content on any difference (`decode_facts.rs:82-93`, `ProbeFileIdentity` `:890-915`).

**3.6 A1/B1 interface D3 assumes** ([PLAYBACK-START-A1-B1-BUILD.md](PLAYBACK-START-A1-B1-BUILD.md) §1, §5.1, §7): create takes one snapshot; preparation runs under the existing durable start identity and the analysis claim attaches worker-side after the answer; `CopyPlanState` lives in `vod/`. D3 assumes the copy/index decision stays in the owner's VOD runtime (`try_vod_session`, `tc.rs:4336`) fed by the plan's file and settings; that A1 adds no create-side read or write; that A1 removes the index-pending rolling fallback's trigger (D3 keeps the fallback and its writes until then). The reads at `vod/serve/create.rs:955` (`reserve_media_group`) and `:2659` (`prepare_source_rendition`) and `source_actor.rs:188, 1605` stay, **owned by A1 steps 4 and 5**; D3 makes the plan's values available in `VodSettings` so A1 can drop them.

## 4. Design

### 4.1 The binding covers every planning key (D3t, before D3b)

New triggers under **new names** — insert, update and delete on `settings` for the keys in `QUALITY_PLANNING_KEYS` outside the `playback.*`/`transcode.*` globs, as D3b will widen it (the list is fixed in D3t and pinned) — bump `playback_input_generation`. Installed as an extension with its own shape marker (precedent `source_arbitration_shape`, `hq.rs:4027-4115`) on both stores; `PLAYBACK_INPUT_SCHEMA` is untouched, so the lineage bridge still recognises old backups. No schema version bump: old binaries ignore unknown trigger names, triggers fire identically on every replica because they are part of the replicated schema, and a rollback leaves them in place. A pin test asserts that a write to every key in `QUALITY_PLANNING_KEYS` moves the generation on both stores.

### 4.2 One read: `StartPlan` (D3b)

```rust
pub(crate) struct StartPlan {                    // http/hls/start_plan.rs
    pub snapshot: Arc<PlaybackPlanningSnapshot>, // file, probe, planning keys, generation, title
    pub binding: PlanningBinding,                // unchanged wire type
    pub source: ViewSource,                      // ledger: View | Authority
}
impl AppliedView {
    pub async fn planning(&self, file_id: i64, keys: &[&str])
        -> Result<Option<(PlaybackPlanningSnapshot, ViewSource)>, StoreError>;
}
```

- **Keys.** `QUALITY_PLANNING_KEYS` stays the one list (`cand.rs:2255-2263`) and gains the settings the create path reads after the snapshot: the `vod_settings` keys (`tc.rs:4660-4679`), `HLS_TYPELESS_SLIDING`, `CLUSTER_SESSION_TAKEOVER_ENABLED`, `CLUSTER_MEDIA_POOL_ENABLED`, and any encoded-path key D3a finds. D3t's triggers cover exactly the list.
- **The read** is the snapshot statement on the local read pool under E1 rules 1–3, **unstamped** (nothing caches it): `admit_unstamped(floors: &[Floor])` beside `admit`, with its own revocation-during-read and floor tests. Two floors: the settings floor (E1b) and a **files floor**. The files floor is raised by every replicated statement that writes `files` directly (`writes_table`), through a trigger (`FILES_TRIGGER_TABLES` beside `SETTINGS_TRIGGER_TABLES`, including `background_job_commands`), or through a cascade (deletes from `items` and `libraries`). A census over trigger bodies **and** `REFERENCES … ON DELETE CASCADE` keeps the list true. `playback_input_generation` is written only by settings triggers (settings floor); `items` gives only the title. SQLite: a store read; remote-authority: authority.
- **One rule for every shared helper:** it takes `plan: Option<&StartPlan>`. `Some` answers from the plan; `None` reads the Store exactly as on `640c6bee8`. `SessionRequest.start_plan: Option<Arc<StartPlan>>` is `#[serde(skip)]`, so takeover and resurrection, which rebuild their request from the stored recipe, cannot receive one. It is assigned in exactly two places — `create_with_purpose_inner` and `ims.rs::start` — pinned by a source test.

### 4.3 The owner

- **Local owner:** the `Arc<StartPlan>` is handed over in-process.
- **Remote owner, no wire change.** `ims.rs::start` calls `start_plan::owner_plan(&state.applied, file_id, expected)` once and passes the result into `restore_candidate_context_with_deadline` and the create. With `expected = Some(b)`: one view read; equal binding → use it; else one authority read; equal → use it; else `catalog_input_unavailable` (`transcode/errors.rs:238`), surfaced as the typed `quality_catalog_unavailable` (`hc.rs:6127-6130`). With `expected = None` (a legacy route): **authority**, as today.
- **A refusal closes the ingress's view.** When an owner answers `catalog_input_unavailable` or a peer `StaleSource`, the ingress may be the stale side. It calls `note_fact` for both floors and re-plans once from Authority in the same request before offering the next candidate. One re-plan per request, not a loop: E1's "a quorum fact closes the view".

### 4.4 Settings change during dispatch

The ingress plans at binding *b*. An owner whose view is past *b* reads authority once and refuses; the ingress re-plans from Authority (§4.3). An owner still at *b* builds at *b*. The session is built from one revision; since D3t, every planning key moves the binding, so "at *b*" means the same values the ingress read.

### 4.5 Writes (D3c): only the selection fold

The claim takes `desired: Option<DesiredSelectionWrite>`. One replicated transaction: the claim's statements, then the selection upsert (today's SQL and idempotence, `hs.rs:3329-3356`) with an added predicate tying it to **this incarnation's** claim — `EXISTS (SELECT 1 FROM media_session_requests WHERE <owner> AND request_id = $r AND incarnation_id = $i AND state = 'starting' AND claim_expires_at_ms > $now)` — so a refused or joined claim upserts nothing. The incarnation is a fresh UUID, so no earlier attempt matches. The claim's post-insert `request_row` read (`hs.rs:2991`) also selects the desired revision, replacing `desired_row`. The SQLite twin applies the same predicate, not its rollback. The write moves from before validation to after the plan: a create refused before the claim records no ask (§12-C7). If the predicate cannot be made to hold on both stores under the store contract's concurrency cases, D3c is dropped and the selection stays where it is.

Not in D3: owner assignment and publication stay separate writes, with the `BLOCKED` fence and Confirm (§12-C3).

### 4.6 The catalog (D3d) and the Developer row

- In create scope the local catalog takes the create's `StartPlan` (the `CREATE_STARTUP_BUDGET` scope, `mp.rs:1456-1466`, already carries the binding) instead of reading at `mp.rs:1985`. A peer's catalog request reads its view with the `expected_binding` it is sent (`mp.rs:1500-1502`); mismatch → authority once → `StaleSource` (which then closes the ingress's view, §4.3).
- Fan-out, peers and placement are **unchanged** (§12-C5). Processed descriptions are still computed (`mp.rs:2002-2006`).
- A per-candidate cache is built only if D3a shows local catalog compute ≥ 100 ms at p90 (§12-5).
- Developer row "Create plan" (`http/developer.rs`, beside "Applied view"): share of plans from the view, last authority reason, re-plans after a refusal, per-shape counts from G1 (computed at read time). Advisory, no switch.

### 4.7 The held probe (D3e)

The owner validates the held descriptor with the decode-fact cache's seven-field tuple (`decode_facts.rs:82-93, 890-915`) against the cache entry for the plan's source identity. On a hit the cached facts serve decoder planning and the held probe is skipped; on any tuple difference the cache hashes the content, and a different content is refused `vod_source_rescan_required`, as the held probe refuses today. If the cache cannot supply every fact the held probe document provides, the lane records the gap and keeps the probe.

## 5. State transitions and failure modes

| Event | Plan read | Outcome |
|---|---|---|
| Healthy replica, proof held | view | 1 local read for planning |
| No proof, odd generation, local SQL error | authority | today's cost for that read |
| This node wrote a setting or a `files` row (directly, by trigger or cascade) | authority until a later watermark applies | read-your-write |
| Owner replica behind the ingress | owner: view mismatch → authority → match | builds at *b* |
| Ingress replica behind | owner refuses; ingress closes its view, re-plans from authority | one refusal, one success |
| Setting saved between plan and owner | §4.4 | one revision |
| Legacy route (no binding) on a remote owner | authority | as today |
| Readable same-path replacement | tuple differs → content hash → refusal; binding mismatch on the owner | no session on the old plan |
| Same size, same mtime replacement | inode/`ctime` differ → content hash | refused `vod_source_rescan_required` |
| Mixed fleet, D3b | old owner reads as today | correct, slower; no wire change |
| Mixed fleet, D3t | old binary ignores the new triggers; they still fire | binding covers the keys fleet-wide |
| Mixed fleet, D3c | selection and claim are both ingress SQL; the owner's desired-revision compare reads the same row | unchanged |

## 6. Call-site classification

### 6.1 Caller chains (seed; D3b's first commit replaces it with the full source walk, pinned in the style of `inv.rs`)

| Entry → helper → read | Plan |
|---|---|
| create `hc.rs:3235` → `hc.rs:3341` snapshot | **view → `StartPlan`** |
| create → `quality_catalog` `mp.rs:1423` → `local_quality_catalog` → `mp.rs:1985` | `Some` |
| peer catalog request → `local_quality_catalog` → `mp.rs:1985` | view + `expected_binding` |
| create → `validate_hevc_copy_transport` `hc.rs:2645` → `probe_json_for_copy` → `hevc_census.rs:64` | `Some` (`probe_json_for_copy_from`) |
| create → `cluster_playlist_is_typeless` `desc.rs:136` | `Some` (only caller) |
| create → `remote_placement_ready` `mp.rs:1360`; `ims.rs::start` → same (`:188`) | `Some` (§12-1); `live_tv_worker` `mp.rs:1288` → `None` |
| create / `ims.rs::start` → `create_session_inner` → `validate_candidate_planning_binding` `tc.rs:849` | `Some`; `prepare_processed_upgrade` `tc.rs:2269` → `None` |
| `create_session_inner` → `vod_settings` `tc.rs:897` → `tc.rs:4660`; → `live_hls_recovery_enabled` `tc.rs:903` | `Some`; `tc.rs:3584, 3917, 5568`, `produce.rs`, `source_actor.rs` → `None`; takeover `ms.rs:6746` → `None` |
| `create_session_inner` fallback → `tc.rs:964` re-validation, `tc.rs:973` `get_file` | `Some` |
| `try_vod_session` `tc.rs:4336` → `tc.rs:4384`; `vod_preparation_snapshot` → `tc.rs:1242`; `rolling_start_inputs` → `start.rs:67` | `Some` |
| encoded preparation → `tc.rs:3038` `get_file_probe_json` | `Some` |
| `ims.rs::start` → `restore_candidate_context_with_deadline` → `cand.rs:1783` | `owner_plan`; takeover `ms.rs:6806`, resurrection `tc.rs:5527` → `None` |
| prepared successor `hc.rs:5632` → `create_cluster_session_with_priority` | `None` |
| resurrection `tc.rs:5440` (`:5568`, `:5581`); rate refresh `desc.rs:820`; offline `plan.rs:1632`; `dv_convert_enabled` (status pages, `state.rs`) | `None` — not create paths |
| `hc.rs:3892`, `:4104` | **stay** — fences |

### 6.2 Other `playback_planning_snapshot` sites (18 in production)

`tc.rs:4139` and `http/hls/processed_upgrade_qualification.rs:334` (after first frame), `cand.rs:820`, `http/stream.rs:2678`, `http/jellyfin/playback.rs:56, 843, 1031`, `http/shared_playback.rs:196`, `http/shared_source_direct.rs:168`, `http/shared_source_ingress.rs:1173` and `hc.rs:6659`: **unchanged** in D3 (§12-7).

## 7. Sub-lanes, order, size

| # | Lane | Content | Size | Depends |
|---|---|---|---|---|
| 1 | **D3a** — attribution | §7.1 | M | — |
| 2 | **D3t** — binding | §4.1: additive triggers, marker, both stores, pin | S–M | D3a's key findings |
| 3 | **D3b** — one plan | §4.2–4.4: `AppliedView::planning`, `admit_unstamped`, files floor and census, `Option<&StartPlan>` through every chain in §6.1, `owner_plan`, refusal re-plan, inventory chains, Developer row | L | D3t, E1b |
| 4 | **D3c** — selection fold | §4.5 | S | D3b; after E1c lands |
| 5 | **D3d** — catalog | §4.6 | S–M | D3b |
| 6 | **D3e** — held probe | §4.7 | S–M | D3b |

### 7.1 D3a, exactly

1. **A Store-method label on every operation.** Each `TimedClient` consistent read and write (`query_consistent_map[_scoped]`, `execute*`, `txn`, batch) takes a `method: &'static str`, passed by the `Store` trait method that issues it — the `AuthorityReadScope` pattern (`hq.rs:878`). Wrappers forward the caller's label: `SessionClient::query_consistent_map` (`hs.rs:636`), `request_row` (`:2083`), `route_by` (`:2028`), `scalar`, `watch_query`, and the watch dispatch. `file:line` goes to a debug log only. The consistent-read census gains a rule: no site without a label.
2. **Request-scoped counts by method.** `HttpStoreOperationCounts` (`store/mod.rs:6183`) gains a bounded per-method map of (authority, local, write); the create budget line (`hc.rs:3032-3043`) logs it as `methods=`; the owner's `ims.rs::start` logs its own scope's map keyed by incarnation.
3. **Metric** `plurx_http_store_operations_by_method_total{route_group,class,method}`, emitted only when non-zero. Bound: method labels ≤ 365 consistent-read sites plus the write sites, × 9 route groups × 6 roles; the exposition caps the series count.
4. **The per-shape breakdown.** `create_path_store_operations_are_pinned_per_shape` pins, for each of the four shapes, every method's authority, local and write counts beside today's totals, and names each of the 6/8/8/9 writes. It gains a fifth shape: a caps-v2 Auto create with an intent, so the catalog read and the selection write are pinned too.
5. **No behaviour change.** The totals stay 25/1/6, 33/1/8, 30/1/8, 25/1/9. `hq.rs` changes, so the formal mappings are refreshed with `scripts/check-formal` and the record states the anchored `txn` text is unchanged.
6. **Output:** the table (in the D3a record) is the "before" every later lane's PR updates, and the fleet reading of the metric over §9's scenario attributes the ≈ 25k.

## 8. Tests

**New behaviour — fails against `640c6bee8`** (each PR records the counterfactual run):
- *Per-shape pin* (`act.rs`): each lane updates the per-method counts by exactly the reads it removes; D3b's target is §9-1.
- *Binding* (D3t, both stores): a write to each key in `QUALITY_PLANNING_KEYS` moves the generation; old code: `cache.max_gb`, `vod.output_preparation`, `vod.rolling_retention` do not.
- *Settings change during dispatch:* hold the owner's start, save a planning key, release: the session is built from one binding (refusal + re-plan, or the old values), never mixed.
- *Stale ingress:* hold the ingress's apply, save a setting elsewhere: one create succeeds after one owner refusal and one authority re-plan.
- *Legacy route:* a remote start with no binding reads authority.
- *Files floor:* a direct `files` write, a `dv_title_jobs` trigger write, a probe result through `background_job_commands` and an `items` delete each close the planning view on the writing node until the next applied watermark.
- *Readable same-path replacement* and *same size and mtime replacement:* both refused, no session on the new bytes under the old plan.
- *Selection fold* (D3c, store contract, both stores): claim and selection in one transaction; a refused, joined or overloaded claim upserts nothing; a repeated digest does not move the revision; concurrent claims for one request upsert at most once.

**Guards — pass today, must keep passing; mutation counterfactual in the PR:**
- *Takeover class* (E1's behavioural guard): a store double whose view answers differ from Authority, driven through takeover (`ms.rs:6664`), resurrection (`tc.rs:5440`), a prepared successor (`hc.rs:5598`), the rate-control refresh (`desc.rs:844`) and an offline package (`plan.rs:1632`). Each sees Authority's values, makes the same Authority reads as on `640c6bee8` and zero view reads. Mutation: hand takeover a plan.
- *Assignment sites:* `start_plan` is set only in `create_with_purpose_inner` and `ims.rs::start` (source test).
- *Fences:* zero view calls in `hc.rs:3892, 4104`, every `authoritative_*` and the receipt fences.
- *Placement:* the owner distribution for a fixed offers fixture is identical before and after D3d.
- *Inventory* (`inv.rs`): the §6.1 chains are the only paths from a `STAYS` file to a view read.
- `test_media_pool_wire` unchanged; consistent-read census unchanged or lowered.

## 9. Acceptance — what is read and how

"Built on effort" and "accepted on fleet" stay separate; Paul deploys; the PR carries the GPT prompt for the client run.

1. **Per shape, as deltas against the pinned counts.** Each shape's planning, settings, probe and file re-reads that D3a attributes are gone: for each shape, authority = pinned total − (those re-reads) and +0 or +1 local read; writes unchanged (6/8/8/9), and −1 write for the intent shape after D3c. The claim, activation, Confirm and publication read-backs and the two fences are listed, unchanged, and out of scope.
2. **Create latency** from G1 rows over one scripted fleet scenario — 30 cold and 30 warm starts each of copy and encode, half through a second ingress with a remote owner, while two other sessions play — before and after: **no start ≥ 1,000 ms; report p50, p95 and max** per cell.
3. **Remote owners:** planning, settings, probe and file reads per remote start, counted in the owner's own `ims.rs::start` scope (§7.1-2): target 0.
4. **Per request:** D3a's `method` split of `playback` authority reads per request, before/after; peer catalog requests, media-peer listings and offers per create, unchanged.
5. **Developer row** reads plan view share, re-plans and the per-shape counts.

## 10. Out of scope

The claim, activation, Confirm and publication writes and their read-backs; the two fences (`hc.rs:3892, 4104`); custody and handoff; folding owner assignment or publication (§12-C3); route and fence reads (E1c); auth and token reads; catalog fan-out and placement (§12-C5); Jellyfin, sharing, `/decision` and compatibility creates (§12-7); `vod/` and `source_actor` (A1); index v3 and the copy engine (A1/B1); processed-upgrade internals (D1); D2's deadlines and result lookup, which D3 preserves; the media-pool wire; background-worker settings reads.

## 11. Before pushing

`python3 scripts/paperwork` (or `make paperwork`); `scripts/scrub-infra-names --check`; `scripts/check-formal` and the mapping refresh when `hq.rs`, `store/sqlite/mod.rs`, `construct.rs` or `produce.rs` change; `cargo clippy -p plurxd --tests -- -D warnings` (+ `-p plurx-core --features plurx-core/hiqlite-contract-tests` for store changes); `cargo fmt --check`; the focused tests above and the per-shape pin; `python3 -m unittest tests.operations.test_media_pool_wire tests.validation.test_rolling_producer_ownership_inventory`; store contract on both backends for D3t and D3c; before/after readings in plan §6 row D3.

## 12. Open decisions

**Decided by the coordinator, for Paul to review:**
- **C1 (B1/B2).** No absolute targets. D3a's first deliverable is the per-shape breakdown by Store method; acceptance is per-shape deltas (§9-1). Claim, activation, Confirm and publication read-backs and the two fences are out of scope. The write ledger is the pinned 6/8/8/9.
- **C2 (B3).** Caller-chain table; `Option<&StartPlan>`, `None` reads the Store; the takeover-class behavioural guard.
- **C3 (B4).** No owner-assignment fold and no publication fold; `BLOCKED` and Confirm stay. Follow-up idea, needing two releases: the owner's activation sets the owner when null; after every node runs that, the ingress stops assigning, decided per owner from its advertised `build`/`protocol_version` (`mp.rs:155-187`). The selection fold stays only with its per-statement predicate (§4.5).
- **C4 (B5, S1).** The binding step is its own lane ahead of D3b, as additive triggers under new names, no schema version bump, no `plan_revisions` column. A legacy route reads authority.
- **C5 (S4, B6).** Placement and fan-out unchanged in D3; fan-out collapse is a measured follow-up. Processed descriptions are still computed.
- **C6 (S5, S6).** The held probe uses the cache's seven-field tuple, so no residual risk remains to accept. D3 edits nothing in `vod/serve/create.rs` or `source_actor`.

**For Paul** (numbers kept from revision 1; 3 and 4 are decided above as C3 and C6):
1. **Planning and admission settings read before a local reservation move to the plan, bound by the binding.** E1 kept a read that precedes a reservation on quorum; encoder admission and `remote_placement_ready` precede a permit or a placement. The permit is a node-local semaphore, and the binding (complete after D3t) makes the owner's values the ingress's. Cost: a changed `max_hw_sessions` or media-pool switch reaches a start up to one lease late. Recommended: move them.
2. **No wire change.** The owner re-reads its view at the carried binding rather than receiving the values (which needs protocol 10 and splits the pool during rollout). Recommended: no wire change.
5. **Catalog cache** only if D3a shows local catalog compute ≥ 100 ms at p90. Recommended: decide on the numbers.
6. **The ask is recorded only for creates that reach the claim.** Answers a client may retry before any ask is recorded: `400 invalid_capabilities`, `422 hdr_subtitle_burn_refused`, the HEVC copy-transport refusal, `503 quality_catalog_unavailable` (`hc.rs:2357`) and `503 media_session_overloaded` (the claim refused, `hc.rs:3845-3851`). None was ever reported accepted. Recommended: confirm.
7. **Other create flavours** (§6.2) keep their reads unless D3a shows them hot. Recommended: leave until measured.

## 13. Review disposition

Every finding was checked against `main @ 640c6bee8` before it was answered. None is rebutted.

| # | Disposition | Where / evidence |
|---|---|---|
| B1 | **fixed (coordinator C1)** — ledger rebuilt from the pinned test: 6/8/8/9; activation is prepare + Confirm (`hs.rs:2535`, `hc.rs:5331-5341`); custody, projection and handoff named; no fold | §1, §3.4, §9-1, §10 |
| B2 | **fixed differently (C1)** — no absolute target; acceptance is per-shape deltas; read-backs and fences out of scope and listed | §1, §9-1, §10 |
| B3 | **fixed** — caller chains; `tc.rs:5581`, `desc.rs:825`, `plan.rs:1640`, `construct.rs:320`, `planning.rs:67` removed; `Option<&StartPlan>`; two assignment sites pinned; takeover-class guard | §3.1, §4.2, §6.1, §8 |
| B4 | **fixed differently (C3)** — owner and publication folds removed; two-release fold recorded as follow-up; selection fold kept with its own predicate | §4.5, §12-C3 |
| B5 | **fixed** — D3t before D3b; `owner_plan(None)` reads authority | §4.1, §4.3, §7 |
| B6 | **fixed** — processed descriptions kept; the §8 pin removed | §2, §4.6 |
| S1 | **fixed** — additive triggers under new names with a marker; `PLAYBACK_INPUT_SCHEMA` untouched; no version bump; `plan_revisions` dropped | §3.2, §4.1 |
| S2 | **fixed** — `FILES_TRIGGER_TABLES`, cascade census, `admit_unstamped` with its own tests | §3.3, §4.2, §8 |
| S3 | **fixed** — refusal or `StaleSource` → `note_fact` both floors → one authority re-plan; test | §4.3, §8 |
| S4 | **fixed differently (C5)** — placement and fan-out unchanged; placement guard test | §4.6, §8, §12-C5 |
| S5 | **fixed** — seven-field tuple; §12-4 deleted; §5 row | §3.5, §4.7, §5 |
| S6 | **fixed** — no edits in `vod/` or `source_actor`; reads named as A1's | §3.6, §10 |
| S7 | **fixed** — label by Store method, forwarded through wrappers; `file:line` in a debug log; cardinality stated and capped | §7.1 |
| S8 | **fixed** — `scripts/check-formal` and mapping refresh | §2, §7.1-5, §11 |
| S9 | **fixed** — max/p50/p95 per cell; remote owner counted in its own start scope; per request from D3a's split | §9 |
| N1 | **fixed** — 18 sites; `http/` prefixes | §6.2 |
| N2 | **fixed** — `ms.rs:1331`; activation call `hc.rs:4813`, Store call `:5278` | §3.2, §3.4 |
| N3 | **fixed** — per-statement predicate on the selection upsert; SQLite twin the same rule | §4.5 |
| N4 | **fixed** — the not-yet answers before the claim listed | §12-6 |
| N5 | **fixed** — D3b L; D3c S now that no migration and no owner fold remain | §7 |
| N6 | **fixed** — media peers and offers counted beside the catalog | §1, §9-4 |

## 14. Build records

None yet. Each sub-lane records what it built and where it departs from this text in `PLAYBACK-START-D3-RECORD.md`, as E1 does.
