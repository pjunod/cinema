# D3a review — store-method attribution

**Reviewer:** Fable (claude-fable-5-2), adversarial, read-only.
**Date:** 2026-11-12.
**Scope:** `git diff adca8e5b6 068fbfe58` — contract base `origin/claude/ps-d3-contract` to `origin/claude/ps-d3a` **as it exists in `~/work/claude-e1c` on nynuc: 7 commits, head `068fbfe58`**. The brief named head `cec5a9b00` (8 commits); that object is in no clone under `~/work` and the remote could not be fetched read-only, so **the eighth commit is not reviewed**. Contract: `docs/streaming/PLAYBACK-START-D3-BUILD.md` rev 2 §7.1; record: `docs/streaming/PLAYBACK-START-D3-RECORD.md`.
**Method:** source reading and scripted label extraction over `git show` blobs. Nothing was built or run; every "would fail" below is by reading, not by execution.
**Verdict:** **changes requested.** The plumbing (label through `TimedClient`, per-request map, capped metric, budget line) is sound and cheap. One gate is red at the reviewed head (B1), and the labels do not yet attribute what the contract says they attribute off the four pinned shapes (S1, S2), which is exactly where the fleet reading has to work.

## Blockers

### B1 — formal mapping is stale at `068fbfe58`
`verification/durable-jobs/mapping.json` (2 rows) and `verification/operation-progress/mapping.json` (rows 21–23) record `ae28fc06a42d92ff…` for `crates/plurx-core/src/store/hiqlite.rs`. That is the blob at `f83e18808`. The next commit but one, `068fbfe58`, edits `hiqlite.rs:1993-2002` (`validation_duplicate_instance_id_transaction` gains its label) and does not touch the mappings; the head blob hashes `51bb61f093998787…`.
*Failure:* `scripts/check-formal check-mapping` reports source drift on five rows at the reviewed head; the record's §5 lists it as a gate run.
*Fix:* re-review and refresh the three+two hashes after the last `hiqlite.rs` edit (the anchored text is untouched by that hunk). If `cec5a9b00` is that refresh, this closes — confirm its hash is `51bb61f0…` or later-correct, and that nothing after it touches the four pinned files.

## Should fix

### S1 — ~170 call sites are labelled with a generic helper's name, not the Store method
The contract: the label is "passed by the `Store` trait method that issues it"; the record (decision 1) says eight helpers were found and forward. They were found by reading the create-path table only. Off that table, helpers still stamp their own name on every caller:

| Helper (label) | Site | Callers collapsed |
|---|---|---:|
| `execute` | `store/hiqlite.rs:5972` | 47 (`get_or_init_setting`, `prune_unreferenced_book_cover_origins`, durable, DVR, fragment-index, invitations, library-channels, media, catalog) |
| `execute_idempotent` | `hiqlite.rs:5982` | `put_setting` |
| `user_optional` / `key_optional` | `hiqlite.rs:5994`, `:6007` | `get_user`, `get_user_by_username`, API-key lookup |
| `credential_mutation` | `hiqlite.rs:1898` | 21 |
| `atomic_publication` | `hiqlite_publication.rs:317` | 30 |
| `query_channel`, `request_ledger_full` | `hiqlite_library_channels.rs:375`, `:230` | 8, 4 |
| `staged_row`, `recovery_row`, `commit_replay`, `claim_from_row`, `claim_existing_or_reacquire` | `hiqlite_sessions.rs:1162`, `:6714`, `:1051`, `:2162`, `:2207` | 7, 3, 3, 2, 2 |
| `route_projection` | `hiqlite_sessions.rs:771` | ~45, via `LocalSessionSql::load` |
| `read_watch_map`, `read_watch_summary`, `read_progress_rails`, `read_continue_watching`, `read_next_up`, `read_watch_rollup[s]` | `hiqlite_media.rs:4869-5047` | the "watch dispatch" the contract names as forwarding |
| `rows`, `cluster_index_availability_read`, `local_media_shape`, `recently_added_window` | `hiqlite_catalog.rs:206`, `hiqlite_fragment_index_cluster.rs:1641`, `hiqlite_media.rs:1435`, `:1267` | 2–7 each |

*Failure:* §9-4 reads `playback` authority reads by `method` on the fleet. A joined or retried create shows `claim_existing_or_reacquire`, a takeover shows `staged_row`/`commit_replay`, every settings write in any group shows `execute`/`execute_idempotent`, every user lookup `user_optional`. "Whatever else `playback` serves" (record §4) resolves into helper names that 47 methods share, so the ≈25k cannot be attributed outside the five fixture shapes. The record's "label is the Store method" is true for the table and false for the store.
*Fix:* give these helpers a `method` parameter like the eight already done (mechanical, same arity-error loop), and make it stick with a census rule: every label literal in `STORE_SOURCES` is a method declared in a `pub trait … Store` block, or is in a short pinned allow-list (migration/import/validation names). The scripted check found 93 non-trait labels today; the allow-list should end near 60, all bootstrap/import/validation.

### S2 — layout reads are folded into the issuing method, so the "before" table is fixture-shaped
`session_layout` (`hiqlite_sessions.rs:386`) forwards the caller's label for its `cluster_meta` marker and guard reads. The record says 8 of first play's 25 authority reads (12 of reopen's 33) are these, paid only while the cached layout is Legacy — which the fresh fixture is and a v82 fleet is not.
*Failure:* `claim_media_session_request 2/0/1`, `activate_media_session 4/0/1`, `publish… 4/0/1` each contain layout reads the fleet may not make. The fleet reading (§4) will not match the table row for row, and nobody can tell from the metric whether the mismatch is a layout read or a real re-read; the question the record itself leaves open ("whether a fleet store pays them") is unanswerable from the instrument built to answer it. It is also inconsistent: `route_projection` (`:771`) and `renew_with_authority` (`:6777`) pass their own literal to the same helper.
*Fix:* label the layout reads `session_layout` (one bounded label, one more table row; totals unchanged, per-method pins move by exactly the 8/12/8/8/9 the record already measured).

### S3 — the fifth shape does not pin the catalog read, and the test says it does
Contract §7.1-4: "a caps-v2 Auto create with an intent, so the catalog read and the selection write are pinned too." `crates/plurxd/tests/cluster_activity.rs:2251-2253` comments "The capabilities make the create read the catalog … so both are pinned", but the fixture's pool is off and the pinned method list has no catalog row (record §2 last bullet admits this).
*Failure:* D3d ("catalog") has no "before" to update; a regression that adds a `playback_planning_snapshot` per offer passes this test. The comment will be believed.
*Fix:* run the fifth shape with the media pool on (a second media node is already in the fixture as node A), or correct the comment and record the missing pin as an open D3d prerequisite in the contract §14.

### S4 — the owner start scope has no test, and its "0" can be vacuous
`crates/plurxd/src/http/internal_media_sessions.rs:217-272`. Nothing exercises it: the pin fixture never takes the remote path, and no test names the `remote start store operations finished` line. §9-3's target is **0** planning/settings/probe/file reads in this window. A scope proves a floor, not a ceiling: any read made in a task spawned under `create_cluster_session_for_receiver` without the scope (the pattern `create.rs:4813` and `media_pool.rs:497` had to patch by hand) is silently absent, and absent reads look like success.
*Failure:* a later refactor drops the `scope_http_store_operations` wrapper, or D3b moves a read into a spawned task; the acceptance number improves.
*Fix:* one cluster test with a remote owner asserting the line exists, its `methods=` is non-empty and sums to the totals, and (cost-validation build) that the window's total equals the owner process's `validation_store_operation_metric_count` delta for the same interval.

### S5 — `queue_sql` hides which writes the pin holds (deviation 1)
`hiqlite_background_jobs.rs:298-326`. The contract asks the pin to "name each of the 6/8/8/9 writes". Two (encode) and three (rolling) of them are `queue_sql`; the names (`custody register`, `activate`, `suspend_playback_analysis_interest`) exist only in the record's prose.
*Failure:* a lane that drops the custody write and adds another queue write keeps `("queue_sql", (0, 0, 3))` green.
*Fix:* add `method: &'static str` to the `QueueSql` entry points (the trait is already re-reviewed for the formal packages in this lane); if that is refused, at least say in the contract §7.1 disposition that these writes are unpinned by name.

### S6 — `plurx_http_store_reads_total{route_group="cluster"}` changes meaning across this lane, undocumented where it is read
The owner scope (deviation 4) adds every owner-side start operation to `cluster`. `docs/streaming/PLAYBACK-START-E1-BUILD.md:215` and `docs/reviews/CINEMA-END-TO-END-REVIEW-2026-11-07.md:39,141` use per-group deltas of this metric as acceptance ("cluster 149k"); E1c is in flight on the same day.
*Failure:* an E1 before/after pair that straddles the D3a deploy shows `cluster` authority reads rising with remote starts and reads as a regression (or masks an improvement).
*Fix:* one line in the E1 record/acceptance and in D3 record §4: baselines must be taken on a D3a-or-later build; the 2026-11-07 `cluster` figure is not comparable.

## Nits

- **N1 — census rule is fail-open on what it cannot parse.** `consistent_read_census.rs:141` skips any receiver ending `inner()`, and `:162` `continue`s when the text after the turbofish is not `(` (a `->` inside the generics ends the depth scan early). `hiqlite/*.rs` are child modules and can call the private `TimedClient::inner()`: a new `client.inner().query_consistent_map(sql, …)` there is unlabelled, untimed and uncounted by both rules. UFCS on the raw client is not matched at all. Count `inner()` receivers into `RAW_CLIENT_SITES` instead of skipping them, and fail on unparsed sites.
- **N2 — the label and the SQL have the same type.** `method: &'static str` then `sql: Into<Cow<'static, str>>`: swapped arguments compile. The census catches it for consistent reads only; a swapped `execute("UPDATE …", "label", p)` exports SQL text as a metric label until a test runs that path. A `StoreMethod(&'static str)` newtype closes it at compile time.
- **N3 — deviation 3.** `("settle_media_session_activation", (2, 0, 2))` pins a timer artefact (`source_layout_idempotent` times the loop, the inner `txn_source_layout_attempt` times the statement). Disclosed, pre-existing, Paul's call (record decision 4) — but the test has no comment saying one of the two is not a write, and a Confirm retry makes it 4. Comment the row now; the clean fix is an untimed retry loop in `source_layout_idempotent`.
- **N4 — `RAW_CLIENT_SITES` wording.** "run before the store serves a request" is not true of all ten: `committed_schema_version` (`hiqlite.rs:5410` → `cluster/migration.rs:3122`) and the two source guards under `verify_compatibility` (`:5405`) run on a live store. They are still rightly raw; say "outside request accounting" instead.
- **N5 — cap arithmetic.** `http/mod.rs:165` and `http/tests.rs` assert `4096 >= 365 × 9`, ignoring three classes and the write/local labels (472 distinct labels today × 27 = 12.7k theoretical). Harmless in practice (a method is one or two classes, two or three groups); fix the comment, and note that at the cap the choice of survivor is first-come and permanent until restart.
- **N6 — global lock on every request.** `http/mod.rs:785` takes `HttpMethodMetrics.series` once per matched HTTP request, including segment and playlist requests with an empty snapshot; `render` holds it while formatting up to 4096 lines. Return early on an empty slice.
- **N7 — record/commit arithmetic.** `f83e18808` says "durable-jobs: 20 rows; operation-progress: 5 rows" and then "all 24 anchored rows". State which.
- **N8 — no `file:line` debug log (deviation 2)** and **no GPT prompt for the fleet reading (§9, record §4).** Both are contract items; the first is acceptably dropped once S1 lands, the second should be in the PR body.
- **N9 — no `Regression-Test:` trailer on any of the seven commits.** If `scripts/history-audit` wants one on `feat` commits, the three `feat(observability)` commits lack it; the tests to name are `every_consistent_read_names_its_store_method`, `operations_are_split_by_method_and_the_split_sums_to_the_totals`, `the_by_method_series_are_capped_and_the_overflow_is_counted`, `create_path_store_operations_are_pinned_per_shape`.

## Checked and found sound

- Labels are `&'static str` end to end; no path builds one from SQL or a request value (the only `Box::leak` is in a test). Metric text needs no escaping.
- Per-request cap (128 + `other`) and process cap (4096 + `_dropped_total`) are correct at the boundary and race-free (one mutex each; an existing series keeps counting at the cap). The split sums to the totals because both are written in the same `record` call.
- Hot path: one uncontended `std::sync::Mutex` and a `BTreeMap<&'static str, [u64; 3]>` probe per store operation inside a request scope; nothing outside a scope. No allocation per operation after a method's first use.
- The owner scope cannot double count on the ingress (different process, different request) and follows the existing `create.rs:4813` pattern; a start task that outlives a dropped request writes into counts nobody flushes, as before.
- Mechanical labels equal the enclosing function at every sampled site (633 literal sites scanned; the only mismatches are `From::from` impl false positives). The defect is which functions are helpers (S1), not typos.
- Formal review claim spot-checked: the diffs to `hiqlite_background_jobs.rs`, `hiqlite_coordination.rs` and `hiqlite_publication.rs` are label insertions and rustfmt reflow only; their recorded hashes match the head blobs.

## Dispositions (2026-11-12, applied on `claude/ps-e1c-d3a`)

Paul's lead decided each; the build is `claude/ps-e1c-d3a`, E1c and main merged. Test names are under `crates/plurx-core/src/store/` unless a path is given.

| # | Finding | Disposition | Where |
|---|---|---|---|
| B1 | Formal mapping stale after the last `hiqlite.rs` edit | **Fixed.** The `hiqlite.rs` hashes were rebound (anchored text untouched) and rebound again after this round's edits; the review is in the commit messages. | `verification/*/mapping.json`; commits `4347a3db8` and the rebind that follows |
| S1 | ~170 sites labelled with a generic helper's name | **Fixed.** Every helper listed forwards `method`; census rule `every_label_names_a_store_method` fails a helper-name label against the `Store` trait methods, their `local_` twins and the named allow-list `NOT_A_STORE_METHOD` (a reason per entry, kept live by `the_label_allow_list_has_only_live_entries`, proved by `the_label_rule_refuses_a_helper_name_and_admits_store_methods`). Deviation: the sharing `Backend` trait's `sharing_read` and `sharing_txn` stay allow-listed (shared with SQLite, 458 call sites, no playback path). | `consistent_read_census.rs` |
| S2 | Layout reads folded into the issuing method | **Fixed.** `session_layout` is its own label; the five shapes are re-pinned by pasting the printed table (8/12/8/8/9 moved out of the issuing methods, totals unchanged). The test comment and the D3 record say which reads are layout reads and that only a legacy-layout store pays them. | `crates/plurxd/tests/cluster_activity.rs`; D3 record §2 |
| S3 | Test comment claimed a catalog read the fixture does not make | **Fixed.** Comment corrected: the pool is off, the fifth shape pins the selection write only, D3d measures the catalog read. | `cluster_activity.rs` |
| S4 | Owner-start scope untested, its "0" can be vacuous | **Fixed.** `spawn_in_http_store_scope` is the one way the start task is spawned; `a_spawned_start_counts_into_its_own_request_and_a_bare_spawn_counts_nowhere` proves the window counts the owner's operations on the internal request it serves, not the ingress, and that a bare spawn counts nothing. The record states why an absent read is safe today (no unscoped spawn inside the start) and how D3b should check it. | `http_operation_method_tests.rs`; `mod.rs`; D3 record decision 5 |
| S5 | `queue_sql` hides which writes the pin holds | **Deferred to D3b.** `queue_sql` stays one label; the pin comment names the writes it carries per shape, and the D3 record says swapping queue writes is invisible to the pin until D3b. | `cluster_activity.rs`; D3 record §2 |
| S6 | `route_group="cluster"` changes meaning at this landing | **Fixed (docs).** Note added to the E1 record, the D3 record and OPERATIONS.md: from this landing the owner-start scope adds the remote owner's operations to `cluster`; baselines must be on a D3a-or-later build. | E1 record §3; D3 record decision 5; `docs/OPERATIONS.md` |
| N1 | Census fails open on `inner()`, UFCS and unparsed sites | **Fixed.** `inner()` receivers and `Client::query_consistent_map(..)` count into `RAW_CLIENT_SITES` (`hiqlite.rs` 7 to 8, the one `TimedClient` wrapper); a site the scan cannot parse fails. | `consistent_read_census.rs` |
| N2 | Label and SQL share a type | **Skipped.** A `StoreMethod` newtype is deferred as churn (recorded). | D3 record decision 7 |
| N3 | Settle pin counts one physical write twice | **Fixed (comment).** The pin says Confirm is double-timed by `execute_idempotent` (one physical `UPDATE`); left for Paul. | `cluster_activity.rs` |
| N4 | `RAW_CLIENT_SITES` wording | **Fixed.** "Outside request accounting", not "before the store serves a request". | `consistent_read_census.rs` |
| N5 | Cap arithmetic | **Fixed (comment).** Comment and test no longer assert `4096 >= 365 x 9`; the record states that first-come series survive at the cap until restart. | `http/mod.rs`; `http/tests.rs` |
| N6 | Global lock on every request | **Fixed.** `HttpMethodMetrics::record` returns early on an empty snapshot; the cap test asserts it. | `http/mod.rs`; `http/tests.rs` |
| N7 | Record counted 24 and 20+5 rows | **Fixed (wording).** The D3 record now says durable-jobs 20 and operation-progress 5 listings, one anchor cited by both. | D3 record §1-5 |
| N8 | No file:line log; no GPT prompt for the fleet reading | **No change.** The file:line log is dropped once S1 lands; the GPT prompt belongs in the PR body. | - |
| N9 | No `Regression-Test:` trailer on the seven commits | **No change** to the old commits; the new commits carry their own lines. | - |
