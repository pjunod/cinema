# Playback start — D3 build records

**Status:** open — what each D3 sub-lane built against [PLAYBACK-START-D3-BUILD.md](PLAYBACK-START-D3-BUILD.md) and where it departs from that contract. D3a and D3t recorded; D3b–D3e not built. Section numbers (§7.1, §9-1 …) refer to the contract.
**Author:** Claude (claude-sonnet-5-5, session 01BCWL9xwmpSjitHqZMTRAht).

## 1. D3a build record (2026-11-12)

Built on `claude/ps-d3a` from the contract branch. No behaviour change: the four pinned totals are unchanged (25/1/6, 33/1/8, 30/1/8, 25/1/9) and a fifth shape is pinned beside them. What §7.1 asked for, and what was built:

1. **Store-method label on every operation.** `TimedClient::{query_consistent_map[_scoped], query_map, execute, execute_acked, execute_idempotent, source_layout_idempotent, execute_returning_map[_acked,_one], txn, txn_source_layout_attempt, batch}` take `method: &'static str` first. The label is the name of the Store method; `SessionClient::{query_consistent_map, execute, execute_idempotent, txn, attempt}`, `route_by`, `request_row`, `session_layout`, `activate_with_authority`, `desired_row`, `live_local_route`, `scalar[_with]`, `pairs`, `local_scalar`, `local_pairs`, `watch_query`, `watch_write[_returning]` forward their caller's label. The remaining ~700 call sites were labelled mechanically from the compiler's arity errors (the label is the enclosing function's name), then the table below was read to find helpers that stand where a Store method should. The consistent-read census gains the rule: a consistent read's first argument is an identifier-shaped literal or the forwarded `method` (`every_consistent_read_names_its_store_method`).
2. **Request-scoped counts.** `HttpStoreOperationCounts` keeps a per-method (local, authority, write) map beside the totals, bounded at 128 methods per request (`MAX_METHODS_PER_REQUEST`; the rest fold into `other`). The create budget line logs `methods=name=authority/local/write,…` for its own window (`method_snapshot_since`). The owner's `ims.rs::start` logs `remote start store operations finished` with `incarnation_id` and `methods=`.
3. **Metric** `plurx_http_store_operations_by_method_total{route_group,class,method}`, emitted only for series that exist. **Bound:** labels are `&'static str` chosen at Store call sites, so at most one per site (365 consistent reads plus the write and local-read sites); series are `route_group × class × method`; the exposition holds at most `HTTP_METHOD_SERIES_CAP = 4096` series and counts anything past it in `plurx_http_store_operations_by_method_dropped_total`. `the_by_method_series_are_capped_and_the_overflow_is_counted` pins the cap and the drop counter.
4. **Per-shape pin** (`create_path_store_operations_are_pinned_per_shape`): every method's (authority, local, writes) per shape, the split-sums-to-total check, and the fifth shape (caps-v2 Auto with an intent).
5. **Formal mappings.** `store/hiqlite.rs` and three more pinned files changed; `scripts/check-formal check-mapping` for durable-jobs and operation-progress listed the drift (worker-refinement unaffected). The review is in the commit message. In short: the anchored text is unchanged in all 24 rows (each anchor occurs once in the reviewed blob and once now); `hiqlite_background_jobs.rs`, `hiqlite_coordination.rs` and `hiqlite_publication.rs` are identical to the reviewed blob modulo inserted label literals, whitespace and trailing commas; in `hiqlite.rs` the anchored `txn` and `STORE_TIMEOUT` text is unchanged and its body only forwards `method`.
6. **Output:** the table below.

## 2. The "before": operations per shape, by Store method

Measured on the two-voter fixture of `create_path_store_operations_are_pinned_per_shape` (ingress node B, a follower; media pool off, so the ingress owns the session). Cells are **authority/local/writes**; `·` is none. These are the numbers every later D3 lane's PR updates by exactly what it removes.

| Store method | first play | reopen | Auto encode | rolling copy | caps-v2 + intent |
|---|---:|---:|---:|---:|---:|
| `playback_planning_snapshot` | 3/0/0 | 3/0/0 | 3/0/0 | 4/0/0 | 3/0/0 |
| `get_settings` | 2/0/0 | 2/0/0 | 2/0/0 | 2/0/0 | 2/0/0 |
| `get_setting` | 1/0/0 | 1/0/0 | 4/0/0 | 1/0/0 | 4/0/0 |
| `local_get_settings` | 0/1/0 | 0/1/0 | 0/1/0 | 0/1/0 | 0/1/0 |
| `get_file_probe_json` | 1/0/0 | 1/0/0 | 3/0/0 | 1/0/0 | 3/0/0 |
| `get_file` | · | · | · | 1/0/0 | · |
| `timeline_annotation_set` | 2/0/0 | 2/0/0 | 2/0/0 | · | 2/0/0 |
| `record_desired_selection` | · | · | · | · | 2/0/1 |
| `maintain_media_sessions` | 2/0/0 | 2/0/0 | 2/0/0 | 2/0/0 | 2/0/0 |
| `claim_media_session_request` | 2/0/1 | 2/0/1 | 2/0/1 | 2/0/1 | 2/0/1 |
| `assign_media_session_request_owner` | 0/0/1 | 0/0/1 | 0/0/1 | 0/0/1 | 0/0/1 |
| `media_session_route_for_playback` | 2/0/0 | 2/0/0 | 2/0/0 | 2/0/0 | 2/0/0 |
| `media_session_route_by_incarnation` | · | 2/0/0 | · | · | · |
| `activate_media_session` | 4/0/1 | 6/0/1 | 4/0/1 | 4/0/1 | 4/0/1 |
| `complete_media_session_terminal_projection` | · | 2/0/1 | · | · | · |
| `complete_media_session_handoff` | · | 2/0/1 | · | · | · |
| `queue_sql` | · | · | 0/0/2 | 0/0/3 | 0/0/2 |
| `settle_media_session_activation` | 2/0/2 | 2/0/2 | 2/0/2 | 2/0/2 | 2/0/2 |
| `publish_media_session_activation` | 4/0/1 | 4/0/1 | 4/0/1 | 4/0/1 | 4/0/1 |
| **total** | **25/1/6** | **33/1/8** | **30/1/8** | **25/1/9** | **32/1/9** |

**Reading the table.**

- **The planning, settings, probe and file re-reads §9-1 counts** are `playback_planning_snapshot`, `get_settings`, `get_setting`, `get_file_probe_json` and `get_file`: 7 authority reads in first play and reopen, 12 in Auto encode and the intent shape, 9 in rolling copy. Three planning snapshots in every VOD shape (four in rolling) confirm §1's list of re-read sites; which three of the seven named sites fire is by reading `create.rs`/`candidates.rs`, not measured per site (the label is the method, not the line).
- **Encoded shapes read three more settings and two more probes** than copy: `get_setting` 1 → 4, `get_file_probe_json` 1 → 3 (§1: `vod_settings`, `live_hls_recovery_enabled`, `get_file_probe_json` at `tc.rs:3038`). The rolling shape's scratch bounds (`ahead_limits`) are three further `get_setting` reads when its two-second cache is stale; the test subtracts them from `get_setting`.
- **Writes, named.** First play (6): `claim_media_session_request`, `assign_media_session_request_owner`, `activate_media_session` (the `BLOCKED` prepare), `settle_media_session_activation` ×2, `publish_media_session_activation`. Reopen (8) adds `complete_media_session_terminal_projection` and `complete_media_session_handoff`. Auto encode (8) adds two `queue_sql` writes, the foreground source custody register and activate. Rolling copy (9) adds a third `queue_sql` write, `suspend_playback_analysis_interest`. The intent shape (9) is Auto encode plus `record_desired_selection`.
- **`settle_media_session_activation` is one physical write counted twice.** Confirm is a single exact-state `UPDATE` through `execute_idempotent`; `source_layout_idempotent` times the retry loop as a write and the guarded `txn` inside it times the same statement again. The pinned 6/8/8/9 therefore hold five, seven, seven and eight distinct durable writes. D3a keeps the pinned totals (no behaviour change); see decision 4.
- **The claim, activation, Confirm and publication read-backs and the two fences are the unchanged, out-of-scope rest** (§9-1, §10): `claim_media_session_request`, `maintain_media_sessions`, `activate_media_session`, `publish_media_session_activation`, `settle_media_session_activation`, `media_session_route_for_playback` and `media_session_route_by_incarnation` make 16 of first play's 25 authority reads, 24 of reopen's 33, 16 of rolling's 25.
- **The session-layout re-reads.** Before the layout helper forwarded its caller's label, the first run split them out: 8 of first play's 25 authority reads, 12 of reopen's 33, 8 of Auto encode's 30, 8 of rolling's 25 and 9 of the intent shape's 32 are `cluster_meta` layout reads (`session_layout`), which `SessionClient` makes whenever the cached layout is not the permanent v82 one. They are folded into the issuing method in the table. Whether a fleet store pays them depends on whether it has installed the permanent layout; the fixture's fresh cluster has not. Nothing in D3 changes them.
- **The catalog read is not exercised by this fixture.** The pool is off, so a caps-v2 create builds no peer catalog and the fifth shape adds nothing but its selection write (`record_desired_selection`: one write and two authority reads, the `desired_row` read-back and a layout read) and the `timeline_annotation_set` reads Auto encode already makes. The catalog's `playback_planning_snapshot` read (`mp.rs:1985`) and its fan-out are read from the fleet by the metric (§4).

## 3. Decisions taken

1. **The label is the Store method name, set at the call site, forwarded by wrappers.** Eight helpers in the table stand where a Store method does (`session_layout`, `activate_with_authority`, `desired_row`, `live_local_route`, `scalar`, `pairs`, `watch_query`, `watch_write`): each takes the label and forwards it. Taken: file:line never reaches a metric.
2. **Labels that are not a Store method, and why.** The queue bridge (`queue_sql`, `queue_transaction`) keeps its own name: its callers are generic over `QueueSql`, shared by both stores and the background-jobs code that the formal packages pin, and threading a label through them touches every body function for attribution no D3 step needs; its create-path writes are named in the table by what they are. Schema migration, import and digest helpers carry their function name. Ten consistent reads are made on the raw vendor client (bootstrap, compatibility and shape probes that run before the store serves a request): they never reach the counts, cannot carry a label, and are pinned per slice in `RAW_CLIENT_SITES` so a new one fails the census until it is labelled or added on purpose.
3. **The file:line debug log is not built.** `#[track_caller]` does not carry through async functions and `async_trait` Store methods, so a call-site location would need a second argument at every site. The label already locates the Store method; the SQL statement locates the rest. A later lane that needs the line can add `file!()`/`line!()` to a trace event at the same sites.
4. **For Paul: count the Confirm write once?** `execute_idempotent` is the only wrapper that times a write at two layers. Counting it once would make the pinned totals 5/7/7/8 and `plurx_http_store_reads_total{class="write"}` one lower per create; D3a did not, because the contract says the totals stay. The coordinator's §9-1 "writes unchanged" holds either way; say which spelling the acceptance should use.
5. **The owner start now has a Store-operation scope.** A task spawned by `ims.rs::start` did not inherit the request's task-local counts, so a remote owner's own planning, settings, probe and file reads were counted nowhere. The start task now runs inside the request's scope and logs its own window (§7.1-2). `plurx_http_store_reads_total{route_group="cluster"}` therefore rises by the owner's start operations: attribution, not new reads.
6. **Per-request cap 128, process cap 4096.** The widest create measured touches 15 methods; the caps bound a mistake, and the overflow is visible (`other`, `…_dropped_total`).

## 4. Fleet reading (to run after the deploy; Paul deploys)

With the build on a voter and one ingress, drive §9's scenario and read, per node, `plurx_http_store_operations_by_method_total{route_group="playback",class="authority_read"}` by `method`, before and after, divided by `plurx_http_route_seconds_count{route_group="playback"}`. The sum over `method` equals `plurx_http_store_reads_total{route_group="playback",class="authority_read"}` apart from `other`; any `…_dropped_total` above zero is itself a finding. The ≈ 25,150 playback authority reads the 10.6 h baseline could not attribute resolve into the rows of §2's table plus whatever else `playback` serves (session `route`, `receipt` and `status` reads, E1c's). The remote owner's share is in its `remote start store operations finished` lines.

## 5. Gates

Run on the build host, none against CI: `cargo fmt --check`; clippy for `-p plurxd -p plurx-core --all-targets` with and without `plurx-core/hiqlite-contract-tests` (and the `cluster-integration-tests` tests); `cargo test -p plurx-core --features hiqlite-store --lib` for the census, placeholder census and the new method-split tests; `cargo test -p plurxd --bin plurxd` for the metric tests; `cargo test -p plurxd --features cluster-integration-tests --test cluster_activity create_` for the per-shape pin; `scripts/check-formal`; `scripts/history-audit`; `make validation-lint`; `scripts/paperwork`. Results are in the PR.

## 6. D3t build record (2026-11-12)

Built on `claude/ps-d3t` from `claude/ps-e1c-d3a`. §4.1 exactly: additive triggers that bind every planning key to `playback_input_generation`, on both stores, with a pinned key list. `PLAYBACK_INPUT_SCHEMA` is untouched and the cluster schema version does not move.

**What was built.**

- `store/playback_input_keys.sql` and `store/playback_input_keys.rs`: a marker table `playback_input_keys_schema(singleton, version=1)` and three triggers, `playback_input_keys_insert`, `playback_input_keys_update` (new or old key in the list) and `playback_input_keys_delete`, each running the same `UPDATE playback_input_generation SET generation = generation + 1` as the glob triggers. New names, so old binaries ignore them and they keep firing after a rollback.
- Hiqlite: an extension installed beside source arbitration and analysis coordination, at bootstrap and on every `SchemaMigrationAction::Current` open (`install_playback_input_keys_schema`). `playback_input_keys_shape` compares every installed object's SQL with what SQLite itself stores for the canonical text (`schema_objects`), refuses a partial or mutated shape, and requires the committed marker. The install is one replicated transaction guarded by a raced-install check; settlement is by re-reading the shape. Both shape reads carry the method label `playback_input_keys_shape` and an `// authority:` reason; the consistent-read census row for `hiqlite.rs` rises 67 to 69.
- SQLite: one `MIGRATIONS` entry, v129, the same SQL. (SQLite has no extension mechanism; its marker is `user_version` plus the marker table.)
- Test, both stores: `store_contract::every_planning_key_write_moves_the_playback_input_generation` writes (insert, update, delete) every key in the list plus a glob key of each family and asserts the generation moves, and that a `jobs.*` key does not. Delete goes through a raw handle (`StrandedFieldOrder::delete_setting`) because the Store trait has no setting delete. Unit test `playback_input_keys::tests::every_bound_key_moves_the_generation_and_others_do_not` does the same on raw SQL, including rollback, renames into and out of the list, and idempotent install.
- Pins: `the_extra_key_list_is_pinned` (the literal list, sorted, none inside a glob), `the_sql_names_exactly_the_pinned_keys` (four IN-lists, same list), and in `plurxd` (`transcode/manager/candidates.rs`) `every_planning_key_moves_the_playback_input_generation` (every `QUALITY_PLANNING_KEYS` entry is covered by a glob or the extension) and `every_key_the_create_chain_reads_moves_the_playback_input_generation` (the derived chain list below).

**The key list** (`PLAYBACK_INPUT_EXTRA_KEYS`): `cache.max_gb`, `cluster.media_pool_enabled`, `cluster.session_takeover_enabled`, `vod.output_preparation`, `vod.rolling_retention`. These are the keys the create chain reads that fall outside `playback.*` and `transcode.*`; D3b's widening of `QUALITY_PLANNING_KEYS` adds only keys inside the globs or this list.

**How it was derived.** From every `get_setting`, `get_settings`, `get_setting_pair`, `num_setting` and `keys::` read reachable from the contract's §6.1 chains and the D3a per-method table (`get_settings` 2, `get_setting` 1 or 4, `local_get_settings` 1):

- `vod_settings` (`tc.rs:4661`): `playback.vod_presentation`, `.vod_working_set_bytes`, `.vod_block_budget_secs`, `.vod_materialize_budget_secs`, `.vod_blocked_get_cap`, `.sdr_master_codecs`, `.vod_index_cluster_cache`, `.hevc_unverified_copy`, `.vod_live_recovery` (all inside the glob), `cache.max_gb` and `vod.output_preparation` (outside);
- `cluster_playlist_is_typeless` (`desc.rs:140`): `playback.hls_typeless_sliding` (glob) and `cluster.session_takeover_enabled` (outside);
- `remote_placement_ready` (`mp.rs:1363`): `cluster.media_pool_enabled` (outside);
- the rolling start (`start.rs:67-110`): `vod.rolling_retention` and `cache.max_gb` (outside), the readrate, burst and ahead limits (glob);
- admission (`describe.rs:164-176`, `vodencode.rs` pairs): `transcode.max_hw_sessions`, `transcode.software_pool_threads` (glob);
- the rest are already in `QUALITY_PLANNING_KEYS` (all glob keys).

Read but not bound, with reasons: `playback.prepared_quality_handoff` (`preparation.rs`, runs after the answer), `library_channels.enabled` (`control.rs`, a control route), `playback.macos_*` and `playback.automatic_decoder_recovery` and `transcode`-artifact qualification (published at startup and refreshed by the settings owner, not read by a create). `live_tv.*` and `dvr.*` are not read by the create chain. A key a later lane makes the create read must be added to the SQL, the Rust list and both pins in one change, with a new marker version.

## 7. D3t decisions

1. **SQLite takes v129; the cluster takes no version.** The contract says no schema version bump, which binds the cluster (a bump makes old voters refuse to open). SQLite has only the `MIGRATIONS` ordinal, so v129 is the only way an existing SQLite database gains the triggers; the replicated store gets them by extension. A SQLite backup at v129 is refused by an older binary, as for every SQLite migration.
2. **The list is five keys, not three.** The contract named the three outside-glob keys already in `QUALITY_PLANNING_KEYS` plus "what D3b widens it by". The two cluster switches are what D3b adds (typeless playlist, placement); they are bound now so D3b needs no second trigger change.
3. **Deletes are tested through a raw handle.** The Store trait has no setting delete; the contract test adds a raw delete helper rather than a trait method that no production caller wants.
4. **Co-author line.** Commits carry the session's `Claude Sonnet 5.5` attribution (the model that built them), not the `Claude Opus 5.5` line the E1 brief quotes.

## 8. D3t gates

On the build host, none against CI: `cargo fmt --check`; clippy `-D warnings` for `-p plurxd -p plurx-core --features plurx-core/hiqlite-store --tests` and with `plurx-core/hiqlite-contract-tests,plurxd/cluster-integration-tests`; the new tests and the planning-snapshot store-contract case on both backends; the consistent-read census and placeholder census; the `cluster_activity` per-shape pin (unmoved); `scripts/check-formal check-mapping` for the three packages (hiqlite.rs, sqlite/mod.rs and store_contract.rs rebound, reviewed in the commit message); `scripts/paperwork --merge-target origin/main`, `scripts/history-audit`, `scripts/validate lint`, `scripts/knowledge lint`, `scripts/scrub-infra-names --check`. Results are in the report to the coordinator.
