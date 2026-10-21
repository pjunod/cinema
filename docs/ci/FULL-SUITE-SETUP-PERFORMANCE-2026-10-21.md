# Full-suite setup performance — measure fixture cost before optimizing it

**Status:** open — two focused baselines measured; optimizations implemented
but uncompiled and unqualified · **Written:** 2026-10-21 · **Owner:** Codex ·
**Branch:** `codex/full-pr-suite-m2`

Companion to the [canonical every-PR plan](FULL-SUITE-EVERY-PR-PLAN.md)
(the complete execution contract) and the
[M1 repair ledger](RUST-SUITE-REPAIR-2026-10-20.md) (the corrected Rust
baseline). This ledger owns M2 measurements and the resulting fixture and
queue preparation changes. Work uses an independent checkout; the user
checkout is not used for edits or tests. Preserve all assertions, identities,
capacity boundaries, production deadlines and scenario concurrency.

M1 exact-source qualification is recorded in
[PR #1244](http://forge.lan:3000/noirr/plurx/pulls/1244). On 2026-10-21 the
user authorized its complete merge into `effort/full-pr-suite`; M1 is not yet
merged there. Failed gates 5728 and 5733 retain their original ancestry and
writer-pin verdicts. The committed repair refresh
`8de7499e5bcdbca76ed201b84d95e3f6e9d2a950` incorporates main
`7f5af452476b6911ffb84e5fcf4fb3f95de054a6`, worker refinement and the
reviewed writer binding repair `2107c648`. Current-source development-gate
and complete native qualification remain pending. M2 retains profiling
ancestry `3ce97c94e` and has prepared the local merge of that refresh,
preserving its frozen Rust changes and both baseline receipts below. The
combined M2 source is uncompiled and unqualified. No history guard is
weakened. The older green result does not qualify this refreshed candidate,
other platform lanes or automatic every-PR execution.

## 1. The green M1 run is the comparison baseline

| Measure | Frozen observation |
|---|---|
| Source | `0f7b6325cce563fec1d8001dc141a46d432db573` |
| Run | Forgejo display run 5713; exact receipt linked from PR #1244 |
| Ordinary native inventory | 6,977 cases: 6,928 passed, zero failed, 49 ignored |
| Doctest selection | Seven selected targets, accounted separately from native cases |
| Full ordinary command | 2,636.152 seconds (43m56.152s) |
| Core native bodies | 958.99 seconds |
| Ordinary Store native bodies | 413.95 seconds |
| Daemon native bodies | 1,233.33 seconds |
| Complete job | 5,463 seconds (91m03s), including setup and other phases |
| Declared repetition controls | 400 affected-cohort bodies and 20 writer bodies passed |

**How to read it:** command wall time, native binary body time and complete
job time have different boundaries. Their difference does not measure
migrations, SQL preparation or teardown. Section 2 measures three focused
cases separately; other isolated costs remain unknown. The 49 ignored
statuses remain ignored; serial passes are separate observations. M2 must
retain every baseline identity and seven doctest targets.

## 2. Two local baselines identify different dominant phases

Both `baseline-01` and `baseline-02` executed committed source
`3ce97c94e6168012425781360267a04f85448fc0`, tree
`c43ad8530a1bc87516ac8f0220b988e521210b9f`. Every selected native body passed,
returned zero and qualified process-group cleanup in both samples.

The environment is local Linux ARM64, pinned Rust 1.97.1, debug optimization
level zero and debug information zero, Cargo build jobs one, and only the
`hiqlite-store` feature. Debug assertions and overflow checks are enabled.
Native tests run individually with `--exact --test-threads=1`, timing enabled
and `RUST_MIN_STACK` unset. This is focused local evidence, not a CI
whole-suite result or a direct speed comparison against the differently
scheduled M1 run.

| Exact case suffix | Baseline 01 command seconds | Baseline 02 command seconds |
|---|---:|---:|
| `terminal_analysis_history_has_a_hard_global_cap` | 136.207 | 135.931 |
| `migration_overflow_retains_all_accepted_work_until_capacity_frees` | 102.477 | 100.742 |
| `recently_added_matches_the_whole_catalogue_ranking` | 53.777 | 53.567 |

**Baseline 01 phase measurements:**

| Case | Actual measured work | Seconds |
|---|---|---:|
| Terminal analysis history | Cold open | 0.23797 |
| Terminal analysis history | 8,193 real terminal inserts | 135.90010 |
| Terminal analysis history | Actual cap transition | 0.02383 |
| Migration overflow | SQL preparation, 4,100 statements across 257 queue transactions | 70.26632 |
| Migration overflow | Statement stepping, same transactions | 20.13291 |
| Migration overflow | Commit, same transactions | 0.40956 |
| Catalogue ranking | 200 cold Store opens | 46.79879 |
| Catalogue ranking | 200 seeding operations, 9,339 rows in total | 0.32306 |
| Catalogue ranking | 5,000 production queries | 3.83321 |
| Catalogue ranking | 5,000 independent oracle queries | 2.54224 |

The history measurement combines repeated preparation and execution inside
its insert loop; it does not isolate preparation alone. The migration path
explicitly times preparation while the writer lock is held. Catalogue
construction dominates that case, while its production and oracle queries
still perform substantial real work. The candidate targets these measured
costs; no post-change improvement is claimed yet.

Raw native logs, phase rows, source and executable bindings, and cleanup
receipts remain in ignored `target/rust-performance-local/` sample folders.
The two `summary.json` SHA-256 digests are:

- Baseline 01: `31558d22bdc9f08a99d1eab1639361b909db877dd608d2009acb062f09136b27`.
- Baseline 02: `c14399e6a626efd698d8d94f720686b3c5ebaec646566ab02914c292d1bb21c0`.

## 3. The candidate keeps implementation ownership narrow

The following changes are implemented in the working tree, **not compiled,
executed or qualified**. The overall performance work is authorized; the
production queue extension has a measured preparation basis and requires
its substantive controls, rather than another optional-feature approval.

| Owned surface | Exact files and implemented boundary |
|---|---|
| Analysis-history fixture | [fragment_index_cluster/tests.rs](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster/tests.rs) prepares the same INSERT once and still executes all 8,193 actual rows. A new pre-cap assertion proves the threshold was exceeded; the original 8,192 cap and queued/final-state assertions remain. |
| Production queue adapter | [background_jobs.rs](../../crates/plurx-core/src/store/sqlite/background_jobs.rs) uses the connection's bounded `prepare_cached` only in `queue_transaction`; SQL, fresh bindings, complete row stepping and commit/rollback semantics remain. Five real controls cover mixed request order, late-row errors, rollback and trigger effects, changed-schema refusal, independent Stores and durable reopen. |
| Explicit initialized fixture | New [fixture.rs](../../crates/plurx-core/src/store/sqlite/fixture.rs), gated by [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs), copies an immutable initialized template into a private memory database, reapplies initialization and regenerates dynamic settings. Five controls cover cold equivalence, dynamic state/PRAGMAs, mutation/rollback/FTS isolation, parallel connections and the real cold-file backfill/reopen path. Cold constructors are unchanged. |
| Single helper caller | Only `recently_added_matches_the_whole_catalogue_ranking` in [media/tests.rs](../../crates/plurx-core/src/store/sqlite/media/tests.rs) adopts the explicit fixture helper. Its 200 independent Stores, 9,339 rows and 5,000 production/oracle comparisons remain. No general Source or migration-fixture conversion is claimed. |
| Source inventory | [placeholder_census.rs](../../crates/plurx-core/src/store/placeholder_census.rs) and [replicated.rs](../../crates/plurx-core/src/store/replicated.rs) register the new fixture module in their exact source inventories. The transaction total remains unchanged. Unchecked SQLite arity remains 103; the [consistent-read census](../../crates/plurx-core/src/store/consistent_read_census.rs) ceiling remains 237. |
| Integration and evidence | Root owns Rust integration, compiler sequencing and commits; this ledger records measurements and acceptance. No mutable Store is shared between cases. |

Preparation caching must never retain previous request values, skip streamed
rows, ignore new schema/trigger refusals or turn a failed transaction into a
success. Reverting the narrow cache call restores uncached preparation;
rollback correctness is independently required by the new controls. Fixture
copies cannot replace genuine empty/upgrade/backfill paths or discard
connection-local PRAGMAs, ownership and WAL/sidecar lifecycle obligations.

**Formal source review, 2026-10-21:** the durable-jobs mapping retains its
model, configurations, authority anchors, bounds and fairness. Seven source
hashes now bind the reviewed queue cache and fixture-module declaration,
including relocation of the unchanged timing helpers below the production
adapter. The five new copied-source controls reject byte drift, missing or
duplicate anchors with fresh hashes, and a single stale module reference.
All 33 Python runner controls and the durable-jobs mapping check pass. The
earlier `b7644c04b` transaction-census failure remains retained: the root
test-only helper declaration had hidden the production adapter from the
existing scanner. Its file-order correction changes no scanner or expected
count and still requires fresh native census acceptance. After integration
of `8de7499e5`, all three package mapping checks report zero source drift and
all 52 focused Python formal package, runner, CI and toolchain controls pass.
The operation-progress writer bindings are incorporated from M1; worker
refinement needs no new source-pin repair. No model checker was run for this
integration. These guard results do not complete M2 qualification or
establish a speed improvement.

## 4. Qualification must retain the work being measured

- [x] Record two complete focused native baselines with isolated phase rows,
  exact source/tree and process cleanup.
- [x] Assign the narrow helper and measured queue preparation surfaces; retain
  all existing assertions and add real isolation/schema/rollback controls.
- [ ] Compile the exact integrated candidate and run all added controls through
  the established pinned compiler loop. Implementation alone is not a pass.
- [ ] Repeat matched local measurements with unchanged case identities,
  assertions, resources and scheduling; record actual improvement and limits.
- [ ] Obtain fresh source-bound complete native and doctest evidence after
  integration. Retain all failures; later passes cannot erase earlier results.
- [ ] Preserve other platform and automatic ready-PR cutover obligations in
  the [canonical plan](FULL-SUITE-EVERY-PR-PLAN.md).

M2 does not remove expensive cases, adopt old outcomes as fresh execution,
increase deadlines, change ignore dispositions or weaken assertions.
