# Playback start — D3 build contract (create collapse)

**Status:** open — design and build contract for lane **D3** of [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md) revision 3 (§3 D3, sequenced after E1 in §4). Not reviewed; not built. Needs one adversarial review and Paul's answers to §12 before a build agent claims D3b. Where this contract and the plan disagree, the plan's §0 rulings win; raise the conflict in the lane PR.
**Verified against:** `main @ 640c6bee8` (2026-11-12), from source only — nothing was compiled or run. Re-anchor line numbers with `scripts/outline` and `git grep` before editing.
**Author:** Claude (claude-opus-5-5, session 01BCWL9xwmpSjitHqZMTRAht). **How to work:** [PLAYBACK-START-BUILD.md](PLAYBACK-START-BUILD.md) §0.

Short paths: `hc.rs` = `crates/plurxd/src/http/hls/create.rs`; `tc.rs` = `crates/plurxd/src/transcode/manager/create.rs`; `cand.rs` = `…/transcode/manager/candidates.rs`; `mp.rs` = `crates/plurxd/src/media_pool.rs`; `ims.rs` = `crates/plurxd/src/http/internal_media_sessions.rs`; `av.rs` = `crates/plurx-core/src/store/applied_view.rs`; `hs.rs` = `crates/plurx-core/src/store/hiqlite_sessions.rs`; `inv.rs` = `crates/plurxd/src/http/applied_settings_inventory.rs`.

## 1. Goal

Plan §1: "Create is one snapshot read → one pure decision → at most two durable writes (admission claim; activation publication — the desired selection folds into the claim) → spawn → answer. Nothing advisory or speculative runs before the answer. Create p99 < 1 s on the fleet."

**Baseline** (a busy voter, build `7929c2439`, 10.6 h uptime): the `playback` route group made 52,307 authority reads over 9,187 requests (5.7 per request). 27,151 are session-route reads (E1's scope). The other ≈ 25,150 (2.7 per request) are not attributed: no metric names the Store method. Per-create counts do exist: G1 records `authority_reads`, `local_reads`, `mutations` and per-phase splits for every start (`http/activity_starts.rs:66-68`, logged at `hc.rs:3032-3043`). D3's ledger reads those.

**Expected bulk, from code reading** (one ordinary caps-v2 create, local owner; D3a measures it):

| Read | Sites | Per create |
|---|---|---|
| Planning snapshot | `hc.rs:3341`; `mp.rs:1985` (once per catalog call); `tc.rs:752` via `:849` (candidate re-validation), again via `:964` on fallback; `tc.rs:1242`, `:4384`; `transcode/manager/start.rs:67` | 2–4 |
| Settings | `transcode/manager/describe.rs:140` (typeless, from `hc.rs:4157`); `mp.rs:1362` (`remote_placement_ready`, `hc.rs:4189`); `tc.rs:4660` (`vod_settings`); `vod/serve/create.rs:955` or `vodencode.rs:947-1000` (encoder admission pair); `vod/serve/create.rs:2659`; encoded path: `plan.rs:1518, 1640, 1667`, `construct.rs:223, 320`, `describe.rs:239, 825, 1219`; `transcode/planning.rs:67`; `tc.rs:1071` | 3–8 |
| Stored probe | `hevc_census.rs:64` via `hc.rs:2656` (copy with caps); `tc.rs:3038` (encoded) | 0–2 |
| File row | `tc.rs:973` (fallback), `:5581` | 0–1 |
| Fences (stay) | `hc.rs:3892` (`media_session_route_for_playback`), `:4104` (predecessor verdict) | 1–2 |

Every one of the first four rows re-reads something the first snapshot already returned or could have: the snapshot carries the file, its probe and 25 settings (`cand.rs:2264`), and `VodSettings.index_cluster_cache` (`vodserve.rs:219`) is read and then read again at `vod/serve/create.rs:2659`. That is the root cause: helpers shared with workers read the Store themselves, so the create path pays each one as a quorum read. A remote owner repeats most of them on its `cluster` route group (`ims.rs:188` placement setting, `cand.rs:1783` snapshot, `tc.rs:849` the same snapshot again), and each catalog peer — up to 64 (`mp.rs:45`) — does one snapshot read per create.

Other `playback` requests also read: `/decision` (`http/stream.rs:2678` snapshot, `:2524` probe), control (`http/hls/control.rs:335, 2114` `get_file`), token authentication on a proof-cache miss (`http/extract.rs:779-781, 924`). If D3a shows creates are not the bulk of the 25k, D3's per-create acceptance still stands and the remainder is reported with an owner, not claimed.

## 2. Constraints that bind the build

- **Rulings and rules.** No feature gate: one advisory Developer row (§4.7). Root causes: no watchdog, no "just in case" re-read, no retry loop around the view. Every hardware-using job stays attributable: a held-source probe or HEVC census measurement that still runs is named in the start's phases (G1). Processed DV is a post-first-frame upgrade: create computes no processed candidate (§4.6). One copy engine: D3 changes no engine (§3.8).
- **E1's view and inventory.** `AppliedView` rules 1–4 and the floor (E1 contract §4.2) apply unchanged; a fence never reads the view. The pinned inventory (`inv.rs`, `STAYS` `:199-228`) lists `transcode/`, `vod/`, `vodencode.rs`, `source_probe/` and `ims.rs` as files that never read the view. D3 keeps that: those files receive plan values; the one new owner-side view call is pinned by name (§6.1).
- **Media-pool wire.** `tests/operations/test_media_pool_wire.py` refuses any strict-envelope change without a `PROTOCOL_VERSION` bump (`mp.rs:37`, now 9) and a dated `docs/OPERATIONS.md` line; a bump splits the worker pool during rollout. **D3 changes no envelope** (§12-2).
- **Store twins** change together, through `crates/plurx-core/tests/store_contract.rs`. D3c's schema step follows H2: both stores, old/new startup, upgrade order, rollback from preserved pre-upgrade state.
- **Consistent-read census** (`store/consistent_read_census.rs`): D3 adds no `query_consistent*` site. Removing calls changes no site count; removing a site lowers its row in the same commit.
- **Rolling-producer census:** D3 adds no spawn and no timer.
- **E1c** (`claude/ps-e1c`, routes) is in flight. D3 designs nothing around route reads. D3c edits `hs.rs` claim and activation SQL, which E1c's local route twin also touches: D3c rebases after E1c lands.
- **A1/B1** (PR #1525 to `effort/playback-start-phase2`) owns the source-growing engine and copy index v3. D3 assumes the interface in §3.8 and redesigns none of it.

## 3. What exists today (the seams)

**3.1 The ingress create** (`create_with_purpose_inner`, `hc.rs:3235`), in order: `record_desired_selection` (`:3304`, the first write, before any validation) → planning snapshot (`:3341`, authority; `capture_create_planning_binding`, `mp.rs:674`) → plan review and ladder ceiling (from the snapshot) → network prior (`stored_prior` with `state.applied`, `:3473`; E1b) → `resolve_plan_with_continuous` (`:3483`; catalog at `:2176`) → `validate_hevc_copy_transport` (`:3535` → `:2656`, probe re-read) → claim (`:3622`) → predecessor route (`:3892`) and verdict (`:4104`) → typeless (`:4157`) → placement (`:4189`, offers `:4202`) → local `create_cluster_session` (spawned `:4264`) or remote start → owner assignment (`:4541`) → activation (local `activate_session_under_authority`, `:5258`; remote `activate_remote`) → publication (`:4958`). Phases `lookup … registration` are already marked (`mark_create_phase`, `mp.rs:598`).

**3.2 The binding.** `PlanningBinding { generation, source_digest }` (`mp.rs:318-336`) hashes file, probe and reorder flag; `generation` is `playback_input_generation`, bumped by triggers on `playback.*` and `transcode.*` keys only (`store/mod.rs:1890-1906`). `QUALITY_PLANNING_KEYS` also holds `cache.max_gb`, `vod.output_preparation` and `vod.rolling_retention`: **a change to those moves no generation and no binding** (finding; fixed in D3c). The snapshot itself is process-private (`CandidateExecutionContext.planning_snapshot`, `transcode/session_request.rs:532`); what travels is `CandidateCatalogContext { caps, candidate, binding }` (`media_sessions.rs:1329`). A remote owner rebuilds the snapshot (`cand.rs:1783`), compares the binding, stores it in the context (`:1872`), and `tc.rs:849` then reads it a third time.

**3.3 The store.** The snapshot is one quorum statement over `files`, `items`, `settings` and `playback_input_generation` (`hiqlite_media.rs:3074-3101`). The view registers `settings` and `media_sessions` (`av.rs:56`); `Replicated::admit` (`av.rs:430`) wants a registered slot because it stamps; floors are `confirmed_since(t) ≤ applied_index` (`av.rs:423`), which needs no slot.

**3.4 Shared helpers.** The split already exists in places: `try_permit` / `try_permit_after` (`vodencode.rs:947, 980`), `planning_hwaccel_preference(snapshot)` beside `hwaccel_preference` (`describe.rs:239`), `capability_height_ceiling_for_preference` beside `_for_request`, `vod_dv_preferences_from_snapshot` (`tc.rs:1299`), `probe_json_for_copy_from` (`hevc_census.rs:71`). Where it does not: `live_hls_recovery_enabled` (`tc.rs:1068`, also takeover), `cluster_playlist_is_typeless` (`describe.rs:136`, also takeover), `requested_rate_control` (`describe.rs:820`, also the rate-control refresh loop), `lang_prefs` (`:1210`), `dv_convert_enabled` (`construct.rs:317`), `encoder_for_file` (`plan.rs:1660`), `vod_settings` (`tc.rs:4651`), `remote_placement_ready` (`mp.rs:1360`, also `ims.rs:188`).

**3.5 Writes on a local create today: five.** `record_desired_selection` (`hs.rs:3319`), the claim (`hs.rs:2904`; it may maintain the session tables first, `store/mod.rs:5169-5174`), owner assignment (`hs.rs:3055`), activation (`activate_media_session`, from `hc.rs:5278`), publication (`hs.rs:4471`). A Library-channel start adds `record_library_channel_session_recipe`. G1's `mutations` counts them physically.

**3.6 The catalog.** `quality_catalog` (`mp.rs:1423`) runs the local catalog and fans out to every reachable peer (`:1468-1530`) under a 2 s deadline (`mp.rs:27`), 1.5 s per peer. Each runs `local_quality_catalog` (`:1961`): a quorum snapshot (`:1983-1987`), `processed_quality_descriptions` (`:2005`), the ordinary candidates (`:2016-2027`). The create then discards the processed ones (`hc.rs:2191-2197`). Peers already advertise `encoders: Vec<EncoderOfferCapability { family, max_target_height, decoders }>` and slot use in the 15 s `MediaNodeSnapshot` (`mp.rs:155-187, 81-85, 40`).

**3.7 The held probe.** Every encoded start runs a descriptor-bound probe (`probe_vod_source_once`, `plan.rs:483`, "No full document is cached" `:482`; or `held_source_probe_json_bound`, `tc.rs:3070-3085`; phase `held_source_probe`, `:3092`). It supplies both source verification and decoder facts. `decode_facts.rs` has a node-local descriptor-bound cache (`:1-6`; identity `plan.rs:932`) that this path does not use.

**3.8 A1/B1 interface D3 assumes.** From [PLAYBACK-START-A1-B1-BUILD.md](PLAYBACK-START-A1-B1-BUILD.md) §1, §5.1: create takes one snapshot and makes ≤ 2 mutations; preparation is scheduled under the existing durable start identity and the analysis claim attaches worker-side after the answer; `CopyPlanState::Growing | Frozen` lives in `vod/`. D3 assumes (a) the copy/index decision stays inside the owner's VOD runtime (`try_vod_session`, `tc.rs:4336`), fed by the plan's file and settings; (b) A1 adds no create-side read or write; (c) A1 removes the index-pending rolling fallback's trigger — D3 keeps that fallback and enumerates its writes until then; (d) D3 edits A1's files only at `vod/serve/create.rs:955` and `:2659` (two settings reads become plan values) and rebases onto A1 there if A1 lands first.

## 4. Design

### 4.1 One read: `StartPlan`

```rust
pub(crate) struct StartPlan {                    // http/hls/start_plan.rs
    pub snapshot: Arc<PlaybackPlanningSnapshot>, // file, probe, planning keys, generation, title
    pub binding: PlanningBinding,                // unchanged wire type
    pub settings_digest: SettingsDigest,         // av.rs:188, over the snapshot's keys
    pub source: ViewSource,                      // ledger: View | Authority
}
impl AppliedView {
    pub async fn planning(&self, file_id: i64, keys: &[&str])
        -> Result<Option<(PlaybackPlanningSnapshot, ViewSource)>, StoreError>;
}
```

- **One key list.** `QUALITY_PLANNING_KEYS` stays the one list (its comment, `cand.rs:2255-2263`, says widening changes no identity) and grows by every setting the create path reads after the snapshot: the `vod_settings` keys (`tc.rs:4660-4679`), `HEVC_UNVERIFIED_COPY`, `HLS_TYPELESS_SLIDING`, `CLUSTER_SESSION_TAKEOVER_ENABLED`, `CLUSTER_MEDIA_POOL_ENABLED`, and the encoded path's keys that are not already in it. The build derives the list from D3a's site counts plus a source walk of §6.2 and pins it (§8).
- **The read** runs the snapshot statement on the local read pool under E1 rules 1–3. It is **unstamped** — nothing caches it — so it needs no slot, no new registered table and no vendor change: admission is even generation + proof + floors. Two floors: the settings floor (E1b) and a new **files floor**, raised by every replicated statement that writes `files` (a classifier like `writes_settings`, `av.rs:342`, with the same census over production SQL). `playback_input_generation` is written only by settings triggers, so the settings floor covers it; `items` supplies only the title, not a planning input. On SQLite it is a store read; on a remote-authority process, authority.
- **Pass-down.** `SessionRequest` gains a process-private `start_plan: Option<Arc<StartPlan>>`, never serialized (like `planning_snapshot`). Every create-path helper in §6.2 gets a pure `…_from_plan(&StartPlan)` form; the Store-reading form stays, unchanged, for workers, takeover and the refresh loop. A create-path caller may call only the pure form (§8 pin).

### 4.2 The owner

- **Local owner:** the `Arc<StartPlan>` is handed over in-process. Zero reads for planning inputs.
- **Remote owner, no wire change.** The ingress sends what it sends today (`CandidateCatalogContext.binding`; a legacy route carries none). `ims.rs::start` builds the owner's plan with `start_plan::owner_plan(&state.applied, file_id, expected)`: one view read; if its binding equals `expected`, use it; else one authority read; if that equals `expected`, use it (the owner's replica was behind); else answer `catalog_input_unavailable: planning inputs changed before admission` (`transcode/errors.rs:238`), and the ingress re-plans on its retry. `restore_catalog_context` (`cand.rs:1729`) and `tc.rs:849` consume that plan instead of reading. Settings used on the owner are then the owner's view at the same binding — the same values the ingress planned with once D3c closes the generation gap (§3.2).
- **Held descriptor (D3e).** The owner already opens and fences the source. It compares the held descriptor's `fstat` (size, mtime) with `plan.snapshot.file`; on a match it takes decoder facts from the node-local decode-fact cache (`DecodeCacheIdentity::from_media_file`) when that entry holds every fact the held probe supplies; on a miss or a mismatch it probes exactly as today. If the cache cannot supply every fact, the lane records the gap and keeps the probe (§12-4).

### 4.3 Writes: two on the ingress

1. **Claim with selection.** `claim_media_session_request` takes `desired: Option<DesiredSelectionWrite>` and `plan: PlanRevisions { generation, source_digest, settings_digest }`. One transaction inserts the claim, upserts the desired selection with today's idempotence (the revision moves only for a new digest, `store/mod.rs:5277-5282`) and returns that revision as `recorded_ask_revision`. A joined or resolved claim does not rewrite it. The write moves from before validation to after the plan: a create refused by validation records no ask. The rule the comment names (`hc.rs:3279-3290`, "persisted *before* … reported accepted") still holds, because the claim precedes every answer that reports acceptance.
2. **Owner assignment folds into the owner's activation.** `activate_media_session` sets `media_session_requests.owner_node_id` in its own transaction under the predicate `assign_media_session_request_owner` uses today (`hs.rs:3055-3089`: same incarnation, `starting`, unexpired, owner null or equal). A refused assignment refuses the activation, and the armed guard aborts the worker, as `Ok(false)` does at `hc.rs:4552`.
3. **Publication.** A **local** owner activates and publishes in one transaction, `activate_and_publish`, under the one serving authority (ingress and owner are the same `ServingAuthority`; one commit guard). A **remote** owner activates (its write) and the ingress publishes (`hs.rs:4471`) as today.

Result: ingress ≤ 2 mutations on every create; a remote owner ≤ 1 (§12-3).

**Plan revisions in the claim (D3c schema step).** `media_session_requests` gains nullable `plan_revisions` (JSON). A retry that joins an in-flight claim through another ingress joins it whatever its own plan says — the owner's plan wins, and Activity → Starts shows both. The same step widens the generation triggers to every key in `QUALITY_PLANNING_KEYS` (the existing globs, or `key IN (…)`), so the binding covers every planning input. `schema_lineage.rs:191-196` fingerprints those triggers: the step follows H2's upgrade order.

### 4.4 Every retained fallback's writes

| Fallback | Reads today → after D3 | Writes (kept) |
|---|---|---|
| VOD → live recovery (`tc.rs:950-1040`) | re-validation `:964` → none (plan); `get_file` `:973` → plan file | `suspend_playback_analysis_interest` (`:981`); `restore_playback_analysis_interest` on drop or session end (`tc.rs:220-258`); the rolling start's own |
| Processed → ordinary degrade (`hc.rs:4457+`) | none | none extra: claim and activation describe the ordinary request |
| Next owner candidate (`hc.rs:4227` loop) | none | remote abort (`ims.rs:630`, `settle_remote_abort` `:676`) for a started worker |
| Spawn failure / early return after the claim | none | the guard settles the claim (`http/hls/session_guard.rs:152`) |
| Activation failure | none | `settle_media_session_activation` (`hc.rs:5334`); abandonment (`:5164`) |
| HEVC census first measurement (`hevc_census.rs:104`) | probe re-read → plan probe | the census record; a source read before the answer, attributed in phases; A1 (v) owns copy eligibility |

### 4.5 Settings change during dispatch

The ingress plans at binding *b*. If an owner's view is already past *b*, it reads authority once and refuses with `catalog_input_unavailable`; if not, it builds at *b*. Either way the session is built from one revision — never the ingress's rung with the owner's newer settings — and the claim records which. Neither path waits.

### 4.6 The catalog

- **Create scope reads nothing.** The local catalog takes the create's `StartPlan` (the `CREATE_STARTUP_BUDGET` scope, `mp.rs:1456-1466`, already carries the binding) instead of `mp.rs:1985`. A peer's catalog request (`/internal/v2/media/quality-candidates`) reads its view with the `expected_binding` it is sent (`mp.rs:1500-1502`); mismatch → authority once → `StaleSource`.
- **Fan out only when needed,** decided from the cached snapshots with no read: ask peer P only if P advertises an encoder family, height or decoder this node lacks for the request's ladder ceiling, or this node's own snapshot says it cannot start now (slots or pressure, `can_start_now`, `mp.rs:2442`). Otherwise the catalog is local, `quality_owners` is this node and placement stays local.
- **No processed candidates before the answer.** `processed_quality_descriptions` (`mp.rs:2005`) is skipped in create scope; D1's post-first-frame qualification resolves its own (`http/hls/processed_upgrade_qualification.rs:327`).
- **Cache, only on numbers.** Per (source digest, settings digest, canonical caps digest, node capability digest = advertised encoders + ffmpeg build), keep the ordinary candidate list before request-specific evaluation (audio index and offset, burn-in, presentation, copy contract, audio delivery/claim — the `QualityCatalogRequest` fields applied at `mp.rs:2016-2027`). Built only if D3a shows local catalog compute ≥ 100 ms at p90, and only for the part the build proves request-independent (§12-5).

### 4.7 Developer row "Create plan"

`http/developer.rs` beside "Applied view": share of plans from the view and the last authority reason; catalog fan-outs per create; G1's authority reads and mutations per create (p50/p99, computed at read time); peers whose catalog verified the binding. Advisory, no switch.

## 5. State transitions and failure modes

| Event | Plan read | Outcome |
|---|---|---|
| Healthy replica, proof held | view | 0 authority reads for planning |
| No proof, odd generation, local SQL error | authority | today's cost for that read only |
| This node just wrote a setting or a `files` row | authority until a later watermark applies | read-your-write |
| Owner replica behind the ingress | owner: view mismatch → authority → match | builds at the ingress's binding |
| Setting saved between plan and owner | §4.5 | one revision; typed refusal or build |
| Same-path file replaced, readable | owner: `fstat` ≠ plan → probe; binding read → mismatch → refusal | no session on the old plan |
| Same size and mtime replacement | not caught by `fstat` | §12-4 |
| Remote owner refuses before publication | ingress claim stays `starting`; guard settles | next candidate or typed not-yet (D2) |
| Reply lost after activation | owner wrote `owner_node_id` with the activation | D2's durable result lookup finds it |
| Mixed fleet (owner without D3b) | owner reads as today | correct, slower; no wire change |
| Capability snapshot stale (≤ 15 s) | local-only catalog when a peer had a better encoder | the start still plays; the row counts it |

## 6. Call-site classification

### 6.1 `playback_planning_snapshot` — 19 production sites

| Site | D3 | Reason |
|---|---|---|
| `hc.rs:3341` | **view → `StartPlan`** | the one read |
| `mp.rs:1985` | **plan** in create scope; **view + binding** on a peer | §4.6 |
| `tc.rs:752` (via `:849`, `:964`) | **plan**; the fn stays for `tc.rs:2269` | `prepare_processed_upgrade` is post-first-frame (D1) |
| `tc.rs:1242`, `:4384`; `start.rs:67` | **plan** | create path |
| `cand.rs:1783` | **`owner_plan`**, called once in `ims.rs::start` (view + binding; pinned in `inv.rs`) and handed to the restore | owner restore |
| `tc.rs:4139`; `processed_upgrade_qualification.rs:334` | **stays** | processed upgrade, after first frame |
| `cand.rs:820`; `http/hls/link_receipts.rs:1963` | **stays** unless D3a shows them hot | receipt and recovery paths |
| `http/stream.rs:2678`; `jellyfin/playback.rs:56, 843, 1031`; `shared_playback.rs:196`; `shared_source_direct.rs:168`; `shared_source_ingress.rs:1173`; `hc.rs:6659` | **stays** in D3 | other create flavours; move with the same `AppliedView::planning` in a follow-up if D3a shows them hot (§12-7) |

### 6.2 Settings, probe and file reads on the create path → plan values

| Site | Pure form |
|---|---|
| `describe.rs:140` typeless | from plan keys (takeover keeps the read) |
| `mp.rs:1362` placement (ingress and `ims.rs:188`) | from plan (§12-1) |
| `tc.rs:4660` `vod_settings` | `VodSettings::from_plan` |
| `tc.rs:1071` live recovery | `VodSettings.live_recovery` (takeover keeps the read) |
| `vod/serve/create.rs:955`, `vodencode.rs:947-1000` | `try_permit_after(ready(plan pair))`; workers keep `try_permit` |
| `vod/serve/create.rs:2659` | `VodSettings.index_cluster_cache` |
| `plan.rs:1518, 1640, 1667`; `construct.rs:223, 320`; `describe.rs:239, 825, 1219`; `transcode/planning.rs:67` | `…_from_plan`; workers, offline and the refresh loop keep the reads |
| `hc.rs:2656` → `hevc_census.rs:64`; `tc.rs:3038` | `probe_json_for_copy_from(plan probe)`; plan probe |
| `tc.rs:973`, `:5581` | plan file |
| `hc.rs:3892`, `:4104` | **stays** — fences (E1 §6.2) |

### 6.3 Writes

`record_desired_selection` → folds into the claim; `assign_media_session_request_owner` (create path) → folds into activation; the other callers (`store/sharing_receiver_*.rs`, `http/shared_receiver_playback.rs:1643`) are unchanged; local activation + publication → one transaction. The §4.4 writes stay.

## 7. Sub-lanes, order, size

| Lane | Content | Size | Depends |
|---|---|---|---|
| **D3a** — attribution | `#[track_caller]` on `TimedClient`'s consistent-read entry points (made non-`async` fns returning the future, so the location is captured at the call) → `plurx_http_store_authority_reads_total{route_group,site}`, `site` = `file:line` of the Store method's consistent read, plus `dispatch:<kind>` for `CONSISTENT_DISPATCH` helpers. Cardinality is bounded by the consistent-read census. No behaviour change. Records the baseline over the §9 scenario: which sites make the 25k, and the create share | S | — |
| **D3b** — one plan | `AppliedView::planning` with the files floor; `StartPlan`; widened key list; `…_from_plan` forms; `owner_plan`; inventory amendments; Developer row | M–L | D3a, E1b |
| **D3c** — two writes | schema step (both stores, H2); claim with selection and plan revisions; owner in activation; local `activate_and_publish`; generation triggers | M | D3b; after E1c lands |
| **D3d** — catalog | create scope reads nothing; fan-out rule; no processed descriptions in create; cache only per §12-5 | M | D3b |
| **D3e** — held descriptor | `fstat` verification and decode-fact cache reuse, or a recorded gap | S–M | D3b |

D3c and D3d may run in parallel. D3a lands first so every later PR reports a before and after on the same metric.

## 8. Tests

**New behaviour — fails against `640c6bee8`** (each PR records the counterfactual run):
- *Ledger* (daemon, embedded replicated store with a held proof): an ordinary caps-v2 create with a local owner costs `authority_reads ≤ 3` and `mutations ≤ 2` in its G1 row; with a remote owner the ingress row is the same, and the owner's start makes no planning, settings, probe or file authority read (D3a's `site` counter on its `cluster` group) and one mutation. Old code: more than 3, and 4–5 mutations.
- *Settings change during dispatch:* hold the owner's start; save a planning key; release. The session is built from exactly one binding — the refusal or the old values — never mixed (§4.5). Repeat with each of `cache.max_gb`, `vod.output_preparation`, `vod.rolling_retention` (old code: binding unchanged, owner builds with the new value).
- *Readable same-path replacement:* rename a different file over the planned path between plan and owner; the owner refuses (`fstat` and binding) and no session plays the new bytes under the old plan.
- *Remote owner rejection before publication:* the owner refuses activation; the ingress publishes nothing, the claim settles, the next candidate or a typed not-yet answers.
- *Read-your-write:* a `files` write on node F (scan) then a create on F reads authority until F applies the next watermark; same for a settings save (E1b's case, through the planning read).
- *Selection fold:* a create with an intent writes one transaction containing claim and selection; a validation refusal writes nothing; a repeated digest does not move the revision; a joined retry does not rewrite it.
- *Owner in activation:* a stale incarnation, an expired claim, or a different recorded owner refuses the activation and leaves no route row.
- *Catalog:* with peers advertising nothing this node lacks and free slots, a create sends no peer catalog request; with a peer advertising a missing family, exactly that peer is asked; no processed description is computed in create scope.
- *Mixed fleet:* an owner without D3b given today's envelope starts correctly (old/new decode in `test_media_pool_wire`'s fixtures; no envelope change).

**Guards — pass today, must keep passing; mutation counterfactual in the PR:**
- Fences: zero view calls in `hc.rs:3892, 4104`, every `authoritative_*`, and the receipt fences (E1's counting double).
- Inventory (`inv.rs`): no `STAYS` file reads the view or calls a view helper except the one pinned `owner_plan` call in `ims.rs::start`, whose result `restore_candidate_context_with_deadline` receives; a create-path function calls no Store-reading form listed in §6.2 (a new pin over the §6.2 function names).
- Store contract, both backends: claim + selection, owner-in-activation and `activate_and_publish` under concurrent claims, expiry and takeover; the migration's old/new startup and rollback.
- `test_media_pool_wire` unchanged; consistent-read census unchanged or lowered.

## 9. Acceptance — what is read and how

"Built on effort" and "accepted on fleet" stay separate; Paul deploys; the PR carries the GPT prompt for the client run.

1. **Create latency:** create `elapsed_ms` p50/p99 from G1 rows over one scripted scenario on the fleet — 30 cold and 30 warm starts each of copy and encode, half through a second ingress with a remote owner, while two other sessions play — before and after. Target p99 < 1,000 ms.
2. **Ledger:** from the same rows, `authority_reads` and `mutations` per ingress create, target ≤ 3 and ≤ 2; on remote owners, D3a's planning/settings/probe/file `site` counts per remote start, target 0, and one mutation. Every row above target is listed with its phases.
3. **Per request:** `Δ plurx_http_store_reads_total{route_group="playback",class="authority_read"}` minus the route reads (`plurx_media_session_route_reads_total`) ÷ `Δ plurx_http_route_seconds_count{route_group="playback"}`, per role, before/after; with D3a's `site` split naming what remains.
4. **Catalog:** peer catalog requests per create (`cluster` group on peers) before/after; local catalog compute p90 (decides §12-5).
5. **Developer row** reads plan view share, fan-outs and the ledger percentiles.

## 10. Out of scope

Route and fence reads (E1c, E1 §6); auth and token reads; Jellyfin, sharing, `/decision` and compatibility-preview creates beyond what shares the changed helpers (§12-7); the copy engine, index v3 and fragment identity (A1/B1); processed-upgrade internals (D1); D2's deadlines, codes and cross-ingress result lookup, which D3 preserves; the media-pool wire; background-worker settings reads.

## 11. Before pushing

`python3 scripts/paperwork` (or `make paperwork`); `scripts/scrub-infra-names --check`; `cargo clippy -p plurxd --tests -- -D warnings` (+ `-p plurx-core --features plurx-core/hiqlite-contract-tests` when the store changes); `cargo fmt --check`; the focused tests above; `python3 -m unittest tests.operations.test_media_pool_wire tests.validation.test_rolling_producer_ownership_inventory`; store contract on both backends for D3c; before/after readings in plan §6 row D3.

## 12. Open decisions for Paul

1. **Planning and admission settings read before a local reservation move to the view, bound by the binding.** E1's rule keeps a read that precedes a reservation on quorum; encoder admission (`vodencode.rs`) and `remote_placement_ready` precede a permit or a placement. The permit is a node-local semaphore, not a durable write, and the binding makes the owner's values the ingress's. Cost: a changed `max_hw_sessions` or media-pool switch reaches a start up to one lease late. Recommended: move them.
2. **No wire change.** The owner re-reads its own view at the carried binding (§4.2) instead of receiving the plan's values. The alternative, protocol 10 carrying the values, saves one local read per remote start and splits the worker pool during rollout. Recommended: no wire change now; carry values only if another lane bumps the protocol anyway.
3. **What "≤ 2 durable writes" counts.** The plan's two are the claim and the publication. A remote owner's activation is a third write on a second node (it holds the owner's fence and cannot move to the ingress). This contract counts ≤ 2 per ingress create and ≤ 1 on a remote owner, and makes the local case two in total. Recommended: confirm.
4. **Held probe.** A same-size, same-mtime replacement is not caught by `fstat`. Today's per-start probe catches it for encoded starts. Recommended: accept the residual where the decode-fact cache supplies every fact (encodes read the bytes anyway, and a wrong map fails typed); keep the probe where it does not.
5. **Catalog cache** only if D3a shows local compute ≥ 100 ms at p90. Recommended: decide on the numbers.
6. **The ask is recorded only for creates that reach the claim** (today: before validation, so a refused create also records it). Recommended: confirm.
7. **Other create flavours** (Jellyfin, sharing, `/decision`) keep their reads in D3 unless D3a shows them hot. Recommended: leave until measured.

## 13. Review disposition

Not reviewed. One adversarial review of this contract before D3b is claimed; its findings and dispositions go here.

## 14. Build records

None yet. Each sub-lane records what it built and where it departs from this text in a `PLAYBACK-START-D3-RECORD.md`, as E1 does.
