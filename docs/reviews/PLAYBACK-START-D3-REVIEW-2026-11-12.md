# Playback start — D3 build contract: adversarial review

**Status:** open — findings await dispositions in the contract's §13. Review of [PLAYBACK-START-D3-BUILD.md](../streaming/PLAYBACK-START-D3-BUILD.md) (`claude/ps-d3-contract @ 633027fe1`) against `main @ 640c6bee8`, from source only (nothing compiled or run).
**Reviewer:** Claude (claude-fable-5-2, session 01BCWL9xwmpSjitHqZMTRAht), 2026-11-12.
**Verdict:** do not build D3b–D3e as written. 6 blockers, 9 should-fix, 6 nits. D3a (attribution) can start once S7 is applied. The one-plan idea and the view read are sound; the blockers are the write and read ledgers (both contradicted by a count already pinned on main), the helper table (it names takeover, resurrection and worker sites as create sites), the D3b→D3c ordering, a mixed-fleet break in the owner fold, and the processed-catalog skip.

Paths are relative to the repo root, with the contract's short names (`hc.rs`, `tc.rs`, `cand.rs`, `mp.rs`, `ims.rs`, `av.rs`, `hs.rs`, `inv.rs`); `ms.rs` = `crates/plurxd/src/media_sessions.rs`, `desc.rs` = `crates/plurxd/src/transcode/manager/describe.rs`, `act.rs` = `crates/plurxd/tests/cluster_activity.rs`, `hq.rs` = `crates/plurx-core/src/store/hiqlite.rs`.

## What holds (checked, no finding)

- **The view read is admissible without a slot.** The snapshot is one statement over `files`, `settings`, `playback_input_generation` and `items` (`hiqlite_media.rs:3081-3087`), so one local read is internally consistent; rules 1–3 need only the generation, the proof and the floors (`av.rs:430-484`). Snapshot install is covered by the odd generation, lag without a proof by `NoProof`, and a restart by the persisted applied index (E1 record). Lag *with* a proof is bounded by one lease, which E1 already accepted for settings.
- **`start_plan` cannot reach takeover by data flow** if it is `#[serde(skip)]` like `planning_snapshot` (`session_request.rs:532`): takeover and resurrection rebuild their request from the stored recipe. That is a property to pin (B3), not one that fails today.
- **No wire change is compatible in both directions** for D3b: the peer validation only bounds `generation ≥ 0` and a 64-character digest (`mp.rs:1616-1621`), and a legacy 404 is already handled (`mp.rs:1532-1565`).
- **Rules.** No gate (one advisory Developer row), no watchdog, no timer, no new `query_consistent*` site. `QUALITY_PLANNING_KEYS` has exactly three keys outside the trigger globs today, as §3.2 says.
- **Line claims.** About 100 `file:line` claims were opened; the ones not listed under B1–B3, S1, S6 and N1–N2 are correct to within two lines.

## Blockers

### B1. The write ledger is wrong: main pins 6–9 writes per create, and the design reaches 4–7, not 2
- **Claim (§3.5, §4.3, §8):** a local create makes five writes today; D3 makes it two on the ingress and one on a remote owner; "old code: 4–5 mutations".
- **Evidence:** `create_path_store_operations_are_pinned_per_shape` measures (authority, local, writes) on a real two-voter cluster with the ingress as owner: first play 25/1/6, reopen 33/1/8, Auto encode 30/1/8, index-miss rolling 25/1/9 (`act.rs:2233-2238`). Those bodies carry **no intent**, so `record_desired_selection` is not among them. What §3.5 omits:
  - **Activation is two writes.** `activate_media_session` inserts the route with `publication_ready_at_ms = BLOCKED` (`hs.rs:2535`); `settle_media_session_activation(Confirm)` is a second write on the success path (`hc.rs:5331-5341`, `hs.rs:4243-4262`). §4.4 files `hc.rs:5334` under "activation failure". A remote owner runs the same function (`ims.rs:456`), so it makes two writes, not one.
  - **Publication cannot share the activation transaction as designed:** it requires `publication_ready_at_ms = 0` (`hs.rs:4498-4522`), which only Confirm writes; on a reopen Confirm writes a future instant and publication waits for the handoff (`hc.rs:5322-5327`).
  - **Foreground source custody** (register and activate) on encode and rolling starts, and **predecessor projection and handoff** on a reopen (`act.rs:2224-2229`), are in neither §3.5 nor §4.4.
- **The fence the fold removes.** `BLOCKED` is what keeps a prepare that commits behind a timeout unservable until Abandon tombstones it (`hc.rs:5303-5308`). A single `activate_and_publish` that also resolves the request makes a late commit a *resolved* request whose worker the guard has already aborted; a retry then answers a dead session.
- **Fix:** rebuild §3.5 from the pinned test's phase ledger (D3a names each write). Then either (a) restate the target per shape with the writes that stay named — claim, prepare, confirm+publish (one transaction is safe: both run under the owner's guard after the route exists), custody, handoff — and take the new number to Paul with §12-3; or (b) design the confirm fold explicitly, including what Abandon does to a resolved request. Update the pinned test in each sub-lane; it is the ledger test §8 asks for and already exists.

### B2. `authority_reads ≤ 3` is unreachable: the session writes themselves read at least nine times
- **Claim (§1, §8, §9-2):** ≤ 3 authority reads per ingress create; the "expected bulk" table sums to 6–17.
- **Evidence:** the pin is 25–33 (B1). Reads D3 does not touch: claim — `maintain_media_sessions` probe (`hs.rs:5898`), `request_row` before and after the insert (`hs.rs:2926, 2991`); activation — pointer before (`hs.rs:2271`), pointer and route after (`hs.rs:2792, 2823`); confirm — route and pointer (`hs.rs:4291`); publication — live route and `request_row` (`hs.rs:4545, 4556`); the two fences (`hc.rs:3892, 4104`). That is 9–11 before custody, the handoff and the fragment-index lookup. §1 never cites the pinned test or these reads.
- **Fix:** either put the read-backs in scope (answer each write from `RETURNING` or the transaction's own counts where the lost-reply recovery read is not needed; skip `maintain` on create when the claim is new) and design them, or change the acceptance to "0 planning, settings, probe and file reads; the remainder listed by site" and give Paul the measured floor. Do not leave a number the design cannot reach.

### B3. The helper table moves takeover, resurrection and worker reads, and no test would catch it
- **Claim (§3.4, §6.2, §8):** each listed site gets a `…_from_plan` form; "a create-path function calls no Store-reading form" is pinned over function names.
- **Evidence — sites that are not on the create path:**
  - `tc.rs:5581` is in `resurrect_vod_from_recipe_before` (`tc.rs:5440`), the recovery of a session from its recipe; the same function calls `restore_candidate_context_with_deadline` (`tc.rs:5527`) and `vod_settings` (`tc.rs:5568`).
  - `restore_candidate_context_with_deadline` → `cand.rs:1783` is called by **takeover** (`ms.rs:6806`) as well as `ims.rs:199`. §4.2 says the restore "consume[s] that plan instead of reading".
  - `desc.rs:825` (`requested_rate_control`) has five callers, all in the refresh loop, boot and the save path under `rate_control_update` (`desc.rs:844, 863, 925, 932, 978`). No create calls it.
  - `plan.rs:1640` is `encoder_for_offline_package`; `construct.rs:320` (`dv_convert_enabled`) is called by status pages and `state.rs` workers only; `transcode/planning.rs:67` (`unverified_hevc_copy_enabled`) has no production caller.
  - `vod_settings` (`tc.rs:4651`) has eight callers: `tc.rs:897, 3584, 3917, 5568`, `produce.rs:448, 905`, `source_actor.rs:188, 1605`. `remote_placement_ready` has a third, `mp.rs:1288` (`live_tv_worker`).
- **Why the pin cannot work:** create, the prepared successor (`hc.rs:5632`) and the receiver start (`ims.rs:217`) all enter `create_cluster_session_with_priority` (`tc.rs:435`). The same function body must read the Store for one caller and the plan for another, so the rule is a run-time property of `req.start_plan`, not of a function name. This is how E1b's first build failed.
- **Fix:**
  1. Replace §6.2 with a table keyed by **caller chain** (entry point → helper → read), produced by a source walk, and delete the five sites above.
  2. One rule: a helper takes `Option<&StartPlan>`; `None` reads the Store exactly as today. `start_plan` is assigned in two places only (`hc.rs` create, `ims.rs::start`); pin that by source test.
  3. Add the behavioural guard E1 used: a store double whose view answers differ from Authority, driven through takeover, resurrection, a prepared successor, the rate-control refresh and an offline package. Each must make the same Authority reads as on `640c6bee8` and zero view reads. Mutation: hand takeover a plan.

### B4. Folding owner assignment into activation breaks every remote start on a mixed fleet
- **Claim (§4.3-2, §5):** the owner's activation sets `media_session_requests.owner_node_id`; "mixed fleet … correct, slower".
- **Evidence:** the SQL lives in the writer's binary. An old owner's activation requires `owner_node_id = $6` on the request row (`hs.rs:2438-2443`) and never sets it; a new ingress no longer assigns. The insert is refused. The reverse direction works. §5's mixed-fleet row covers only D3b.
- **Fences the fold must keep, by name:** claim incarnation + `starting` + unexpired, and owner null-or-equal (`hs.rs:3083-3085`); the request-owner match in activation (`hs.rs:2441`) and in publication (`hs.rs:4505, 4512, 4522`); the `session:<incarnation>` lease at fence 1 (`hs.rs:2449-2452`); the pointer and desired-revision compares (`hs.rs:2467, 2531-2532`; `hc.rs:4772`); the ingress guard before assignment (`hc.rs:4534-4538`), the owner's commit guard (`hc.rs:5268-5276`) and the ingress guard before publication (`hc.rs:4949-4954`). The fold drops the ingress guard before assignment; say why that is safe (publication still holds one) or keep the check.
- **Fix:** two releases. First: the owner's activation sets the owner when null (old ingress still assigns; harmless). Second, after every node runs the first: the ingress stops assigning, decided per owner from its advertised `build`/`protocol_version` (`mp.rs:155-187`) and never from a setting. Add the old-owner/new-ingress case to the store contract.

### B5. D3b lands before the binding covers what the owner reads from its view
- **Claim (§4.2, §4.5):** the owner's view "at the same binding" holds "the same values the ingress planned with once D3c closes the generation gap"; D3c depends on D3b (§7).
- **Evidence:** the generation moves only for `playback.*` and `transcode.*` (`store/mod.rs:1898-1906`). After D3b's widening the owner reads from its view, with an equal binding, at least `cache.max_gb`, `vod.output_preparation`, `vod.rolling_retention`, `cluster.media_pool_enabled` and `cluster.session_takeover_enabled`. Today those are quorum reads on the owner. Between D3b and D3c a lagging owner builds with the old value of a key the ingress read new. The same hole is permanent for a **legacy route**, which "carries none": `owner_plan` with no expected binding trusts the owner's view outright. §8's settings-change test for those three keys cannot pass on D3b.
- **Fix:** move the trigger step ahead of D3b (S1 makes it additive, so it can ship alone), and make `owner_plan(expected = None)` read Authority.

### B6. Skipping processed descriptions in create scope breaks a create that names a processed candidate
- **Claim (§3.6, §4.6):** the create "discards the processed ones (`hc.rs:2191-2197`)", so `processed_quality_descriptions` can be skipped.
- **Evidence:** before discarding them the create uses them to map a requested processed id to the ordinary rung it enhances (`hc.rs:2210-2225`), and keeps them whole when `processed_selectable` (`hc.rs:2199-2200`, set for a processed predecessor at `:1914`). With the list empty the mapping falls through to the unknown id. `processed_quality_descriptions` is a synchronous lookup of descriptions already built (`mp.rs:2002-2006`), not a Store read. §8's "no processed description is computed in create scope" would pin the regression, and it is the reopen path ruling 3 depends on.
- **Fix:** keep the call. If D3a shows it costs time, measure it; do not remove it for the read ledger, to which it contributes nothing.

## Should-fix

### S1. The schema step is mis-anchored and heavier than "M"
- `PLAYBACK_INPUT_SCHEMA` is `CREATE TRIGGER IF NOT EXISTS` (`store/mod.rs:1898`): editing the `WHEN` clause changes nothing on an installed cluster. The constant is also the **historical witness** the 2026-10-04 lineage bridge recognises (`schema_lineage.rs:343, 431-436`); `:191-196` is that bridge's object query, not an upgrade check. Editing it in place breaks recognition of old backups.
- A cluster schema bump makes every voter still on 82 refuse to open (`hq.rs:7452-7464`), so "rollback from preserved pre-upgrade state" needs the dual-layout work v82 did (`hs.rs:386-420`). A new binary must also claim against a cluster that has no `plan_revisions` column yet.
- **Fix:** (a) add the three trigger events under **new names** for the keys outside the globs, as an extension with its own marker (precedent `hq.rs:4035-4115`); old binaries ignore unknown trigger names, and rollback leaves them in place. (b) Drop the `plan_revisions` column: nothing compares it (§4.3 says the owner's plan wins), and Activity → Starts already has G1's row for attribution. That leaves D3c with no versioned migration.

### S2. The files floor needs more than a statement classifier
- `writes_table` sees only statements that name the table (`replicated.rs:1198-1230`). `files` is also written by triggers (`dv_title_jobs.sql:64, 77, 104, 134`; the probe result applied through `background_job_commands`, `background_jobs_probe.rs:179`) and by foreign-key cascades from `items` and `libraries` (`hiqlite_catalog.rs:36-38, 75`), which no trigger census finds.
- `TableFloors` is `[AtomicU64; 2]` indexed by `AppliedTable` (`av.rs:300-325`) and `Replicated::admit` fails `Unregistered` without a slot (`av.rs:419-437`): "no slot" needs a second admission path beside the one E1 proved.
- **Fix:** a `FILES_TRIGGER_TABLES` list beside `SETTINGS_TRIGGER_TABLES` (`av.rs:331-337`), plus `items` and `libraries` deletes; a census over trigger bodies **and** `REFERENCES … ON DELETE CASCADE`; an `admit_unstamped(floors: &[…])` with its own revocation and floor tests.

### S3. A refusal from an owner must close the ingress's view
- If the ingress's view is the stale side, the owner's Authority read mismatches and refuses (§4.2). The ingress then offers the same plan to the next candidate (`hc.rs:4227`) and, on a retry inside the lease, reads the same view. Today the ingress read by quorum and cannot be the stale side.
- **Fix:** on `catalog_input_unavailable` from an owner or `StaleSource` from a peer, `note_fact` both floors and re-plan once from Authority in the same request. That is E1's "a quorum fact closes the view", not a retry loop. Test: ingress apply held, a setting saved elsewhere, one create succeeds with one refusal.

### S4. "Fan out only when needed" silently changes placement
- `quality_owners` is built from the catalog (`hc.rs:4090-4099`) and, when negotiated, filters the ranked offers (`hc.rs:4216-4218`). A local-only catalog makes it `{this node}`, so every caps-v2 create lands on the ingress while it has a slot, whatever `rank_offers` says about pressure, speed or a peer's cache hit. The comment at `hc.rs:4219-4221` forbids an ineligible local offer bypassing an eligible peer.
- The rule also uses `can_start_now`, which takes a `MediaOffer` (`mp.rs:2442`) the create does not have until placement, after the catalog.
- **Fix:** decide fan-out from placement — compute the offers first (they already run per create, `mp.rs:1661`) and ask the catalog of the nodes placement would consider — or state the placement change as a decision for Paul with a before/after owner distribution in §9.

### S5. The held-descriptor check cites the wrong identity; §12-4's residual is avoidable
- `DecodeCacheIdentity::from_media_file` is the artifact key over id, size and mtime (`plurx-core/src/transcode/decode.rs:605-611`). The node-local decode-fact cache keys on the source identity and a content digest (`decode_facts.rs:509-515`) and validates a seven-field tuple including `ctime`, device and inode, hashing on any difference (`decode_facts.rs:84-93, 890-912`). A same-size, same-mtime replacement moves the inode.
- **Fix:** D3e uses that validation, not `(size, mtime)`. Then §12-4 is not a risk for Paul to accept, and the §5 row becomes "caught by the tuple; refused `vod_source_rescan_required`".

### S6. Two edits inside A1/B1's owned surface
- `vod/serve/create.rs:955` is `reserve_media_group` and `:2659` `prepare_source_rendition`; A1 step 4 owns "`vod/serve/create` — growing attachment and admission" (A1-B1 contract §7), and step 5 owns `source_actor`, which calls `vod_settings` (`source_actor.rs:188, 1605`).
- **Fix:** D3 does not edit `vod/` or `source_actor`. The two reads stay, named in the ledger as "owned by A1 step 4", until that step lands; D3 carries the values in `VodSettings` so A1's owner can drop the reads there.

### S7. D3a's `site` label: wrappers hide it and line numbers do not compare across builds
- `Location::caller()` is the immediate caller. Session reads go through `SessionClient::query_consistent_map` (`hs.rs:636`) and shared helpers (`request_row` `hs.rs:2083`, `route_by` `:2028`, `scalar`, `watch_query`), so one label covers many methods unless every wrapper is also non-`async` and `#[track_caller]`.
- `file:line` changes with every edit, so "every later PR reports a before and after on the same metric" (§7) compares different label sets.
- Cardinality is bounded: 365 census sites (`consistent_read_census.rs`, 29 files) × route groups × roles, emitted only when non-zero. State that bound and cap the exposition.
- **Fix:** label by Store method (a `&'static str` passed by the trait method or a scope like `AuthorityReadScope`, `hq.rs:878`), keep `file:line` in a debug log; or propagate `#[track_caller]` through the listed wrappers and pin that by test.

### S8. The formal mappings pin files D3 edits
- `verification/{durable-jobs,operation-progress}/mapping.json` pin `store/hiqlite.rs` by whole-file sha256 (anchor `pub(super) async fn txn<C, Q>(`), and also `store/sqlite/mod.rs`, `transcode/manager/construct.rs` and `produce.rs`. D3a and D3b edit `hiqlite.rs` (`TimedClient`, `:1300-1400`), D3c `sqlite/mod.rs`. §11 does not list `scripts/check-formal` or the mapping refresh.
- **Fix:** add both to §11, and say in each sub-lane's record that the anchored text is unchanged.

### S9. Three acceptance numbers are not measurable as written
- **p99 over 120 starts** (§9-1) is the second-largest sample, and per cell (30) it is the maximum. State "no start ≥ 1,000 ms; report p50, p95, max", or raise the cell size to ≥ 300.
- **Remote-owner "0"** (§9-2) reads a process-wide counter on the `cluster` group while two sessions play and peers answer catalog requests; a legitimate mismatch read or a takeover moves it. Count per request in the owner's own start scope, or measure quiesced and say so.
- **Per-request formula** (§9-3): `plurx_media_session_route_reads_total` has `site` and `source` labels and no route group (`ms.rs:970-976`). After E1c most route reads are view reads. Subtract only `source="authority"` at the sites the playback group reaches, or use D3a's split directly.

## Nits

- **N1.** §6.1 says 19 production sites. `http/hls/link_receipts.rs:1963` is inside `mod tests` (from `:1484`): 18. Four paths lack `http/` (`http/jellyfin/playback.rs`, `http/shared_playback.rs`, `http/shared_source_direct.rs`, `http/shared_source_ingress.rs`).
- **N2.** `media_sessions.rs:1329` is `:1331`; the activation call is `hc.rs:4813`, and `:5278` is the Store call inside it.
- **N3.** The claim becomes a two-statement transaction. Replicated statements do not roll back together (`hs.rs:2524-2530`), so the selection upsert needs its own predicate tying it to this incarnation's insert, and the SQLite twin the same rule.
- **N4.** §12-6: a create refused after the plan but before the claim (catalog incomplete, capacity) no longer records the ask. Say which typed not-yet answers a client may retry without having been recorded.
- **N5.** Sizes: D3b is L (dual paths in every shared function, B3); D3c is L unless S1 removes the migration.
- **N6.** `membership.media_peers()` and the offers fan-out (`mp.rs:1661-1700`) also run before the answer; §1 and §9-4 should count them beside the catalog fan-out.

## Verdict

D3a can be built now with S7's label change, and its first deliverable should be the per-shape breakdown of the pinned 25/33/30/25 reads and 6/8/8/9 writes; B1 and B2 are answered by that table, not by estimation. D3b needs B3 and B5 resolved in the text first: a caller-chain table, the `Option<&StartPlan>` rule, the takeover-class behavioural test, and the generation triggers ahead of it. D3c needs B1, B4 and S1: a ledger that names every write that stays, a two-release owner fold, and no versioned migration. D3d needs B6 and S4. Two items go to Paul beyond §12: the achievable write and read numbers (B1, B2) and the placement change (S4).
