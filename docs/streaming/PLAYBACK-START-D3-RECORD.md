# Playback start — D3 build records

**Status:** open — what each D3 sub-lane built against [PLAYBACK-START-D3-BUILD.md](PLAYBACK-START-D3-BUILD.md) and where it departs from that contract. D3a recorded; D3t, D3b–D3e not built. Section numbers (§7.1, §9-1 …) refer to the contract.
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
