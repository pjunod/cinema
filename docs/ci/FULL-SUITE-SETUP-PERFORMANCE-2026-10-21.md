# Full-suite setup performance — measure fixture cost before optimizing it

**Status:** open — integrated candidate compiled; focused controls and matched
local measurements passed; full M2 qualification pending ·
**Written:** 2026-10-21 · **Owner:** Codex · **Branch:** `codex/full-pr-suite-m2`

Companion to the [canonical every-PR plan](FULL-SUITE-EVERY-PR-PLAN.md)
(the complete execution contract) and the
[M1 repair ledger](RUST-SUITE-REPAIR-2026-10-20.md) (the corrected Rust
baseline). This ledger owns M2 measurements and the resulting fixture and
queue preparation changes. Work uses an independent checkout; the user
checkout is not used for edits or tests. Preserve all assertions, identities,
capacity boundaries, production deadlines and scenario concurrency.

M1 exact-source qualification is recorded in
[PR #1244](http://forge.lan:3000/noirr/plurx/pulls/1244). The user-authorized
M1 merge into `effort/full-pr-suite` completed at 2026-10-22 00:40 UTC:
development gate 5740 and complete Rust qualification 5743 passed.
The qualified source is `761b371c3ddd136ba7a724fd244dd91746acac73`, landed as
`0aa94aefcc9bfa35fde312fcc372ca5438753c30` with the exact qualified tree and
all 63 regression fields. Earlier failed gates 5728 and
5733 retain their original ancestry and writer-pin verdicts; gate 5736
retains its unsupported-landing-title refusal.

Measured M2 source `45ffd5434162b0f448e670073a19595e90d5e4a1` incorporates
M1 `8de7499e5`. M1 `761b371c` changes only history metadata and its ledger,
so its Rust base is identical; M2 differs in exactly eight Rust paths. It has passed the pinned
compiler loop, focused controls and the paired measurements in §2.2. Those
local observations do not qualify its complete native suite, doctests,
other platform lanes or automatic every-PR execution. M2 retains the older
`3ce97c94e` measurements as bottleneck reconnaissance; only the new
current-base pair is used to attribute the measured improvements. No history
guard is weakened.

## 1. The merged M1 run supplies whole-suite context

M1 landing `0aa94aefc` has exactly the tree qualified by run 5743 on source
`761b371c3`: `a5736c391563b9e96dd3d857ad7e009f2803a6fc`.

| Measure | Qualified observation |
|---|---|
| Source | `761b371c3ddd136ba7a724fd244dd91746acac73` |
| Run | Forgejo display run 5743; exact receipt linked from PR #1244 |
| Ordinary native inventory | 7,011 cases: 6,961 passed, zero failed, 50 ignored |
| Doctest selection | Seven selected targets; one actual case passed |
| Full ordinary command | 2,687.031 seconds (44m47.031s) |
| Core native bodies | 971.32 seconds |
| Ordinary Store native bodies | 413.60 seconds |
| Daemon native bodies | 1,269.53 seconds |
| Separate serial VFR / bitmap commands | 5.771 / 5.149 seconds |
| Complete job | 5,250 seconds (87m30s), including setup and other phases |
| Declared repetition controls | 400 affected-cohort bodies and 20 writer bodies passed |

**How to read it:** command wall time, native binary body time and complete
job time have different boundaries. Their difference does not measure
migrations, SQL preparation or teardown. Section 2 measures three focused
cases separately; other isolated costs remain unknown. All 49 older ignored
identities remain, and main adds one ignored GPU comparison; serial passes
are separate observations. M2 must
retain every baseline identity and seven doctest targets.

The strict qualified-summary SHA-256 is
`16d1cb18ab74e42d33ed06382c9db86ae0221c933d07c120dca5661d83f53f4b`.
The later main update `7f5af4524` → `92b259b25` adds Python tooling and
documentation without changing Rust, Cargo, toolchain, image or test-observer
inputs. It does not invalidate the frozen focused comparisons in §2;
Python/static scheduling and total job cost may change. The final integrated
M2 candidate still requires fresh complete qualification.

## 2. Focused measurements separate bottleneck discovery from comparison

### 2.1 The earlier baselines identify dominant phases

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
costs. These older observations establish the optimization targets; the
current-base comparison in §2.2 measures the changes.

Raw native logs, phase rows, source and executable bindings, and cleanup
receipts remain in ignored `target/rust-performance-local/` sample folders.
The two `summary.json` SHA-256 digests are:

- Baseline 01: `31558d22bdc9f08a99d1eab1639361b909db877dd608d2009acb062f09136b27`.
- Baseline 02: `c14399e6a626efd698d8d94f720686b3c5ebaec646566ab02914c292d1bb21c0`.

### 2.2 The current-base paired samples retain all measured work

The timing-only baseline is source
`9acf0c5dc1a5c417dffbdfb4b14ebd65d710b33f`, tree
`1b45f4b3c444a7992a861ebfdb46d9f5b9365896`, based on M1
`761b371c3ddd136ba7a724fd244dd91746acac73`. That base has identical Rust
source to `8de7499e5`, incorporated by optimized source
`45ffd5434162b0f448e670073a19595e90d5e4a1`, tree
`e77b6a5c256a49f574fc153fa592616dfb55e109`. The baseline retains uncached
preparation, all 200 cold `open_in_memory` calls and all 8,193 actual
history inserts. The new pre-cap assertion is mirrored in that baseline,
so both variants prove the threshold was exceeded before applying the cap.

Each variant has two fresh-process observations, interleaved baseline 01,
optimized 01, baseline 02, optimized 02. All use local Linux ARM64, Rust
`1.97.1 (8bab26f4f 2026-07-14)`, `hiqlite-store`, optimization level zero,
no debug information, enabled debug assertions and overflow checks, and
16 logical/16 affinity CPUs. Each native command explicitly unsets
`RUST_MIN_STACK` and uses `--exact --test-threads=1 --nocapture` with
`PLURX_TEST_SETUP_TIMING=1`. The raw argv, source and executable hashes,
phase rows, CPU deltas, native verdicts and owned process-group cleanup
receipts are retained with each sample.

| Exact case suffix | Baseline 01 / 02 seconds | Optimized 01 / 02 seconds | Baseline median seconds | Optimized median seconds | Ratio of medians |
|---|---:|---:|---:|---:|---:|
| `terminal_analysis_history_has_a_hard_global_cap` | 120.523 / 112.459 | 0.483 / 0.464 | 116.491 | 0.4735 | 246.02× |
| `migration_overflow_retains_all_accepted_work_until_capacity_frees` | 88.710 / 91.440 | 19.235 / 19.221 | 90.075 | 19.228 | 4.68× |
| `recently_added_matches_the_whole_catalogue_ranking` | 48.962 / 48.885 | 7.589 / 7.894 | 48.9235 | 7.7415 | 6.32× |

**How to read it:** these are three isolated case command durations, with
only two observations per variant. The displayed pairs are also their
observed ranges. They establish focused improvements under this compiler
and scheduling contract; they do not predict whole-suite or Forgejo job
durations. All 12 native bodies passed, returned zero, retained an owned
leader through cleanup, and left no live or unknown process group.

| Case | Unchanged actual work verified in all four raw profiles | Dominant phase median, baseline → optimized seconds |
|---|---|---:|
| History | 8,193 real loop executions; one queued insert, cap update, commit and final read; unchanged native cap assertions | Insert loop 116.20772 → 0.21134 |
| Overflow | 4,100 accepted; initially 3,840 materialized/260 pending; 260 cancellations; drained 4,100 materialized/zero pending/zero failed; 257 completed and committed queue transactions with all 4,100 statements completed | Queue preparation 62.88632 → 0.16743 |
| Catalogue | 200 independent Store opens/seeds, 9,339 rows, 5,000 production and 5,000 oracle queries; all paired assertions retained | Opens 43.02420 → 2.07961 |

The overflow SQL volume is identical at 121,401,000 bytes. Its 257
transaction statement counts remain 256 groups of 16 and one group of four.
Queue timing is nested inside the overflow phases; adding both would double
count time. Catalogue production-query medians remain 3.26090 → 3.13003
seconds and oracle-query medians 2.14417 → 2.06570 seconds: the measured
construction reduction preserves the real queries.

| Case | Baseline median CPU seconds | Optimized median CPU seconds | Baseline / optimized median CPU seconds per wall second |
|---|---:|---:|---:|
| History | 116.46236 | 0.46153 | 0.99976 / 0.97516 |
| Overflow | 89.77905 | 18.85278 | 0.99672 / 0.98049 |
| Catalogue | 48.79928 | 7.61006 | 0.99746 / 0.98299 |

CPU values are measured user-plus-system child CPU deltas. The nearly
one-core baseline consumption supports preparation/construction CPU cost
for these isolated observations; it does not diagnose the earlier shared
CI slowdown. The recorded child maximum RSS values span 46,576–61,540 KiB
and are cumulative process-child high-water marks. They are neither
per-case memory peaks nor peak deltas and establish no memory improvement.

**Freshness guard:** restoring an older source archive initially produced a
0.35-second Cargo result after the baseline build, which could have reused
the baseline executable because archive mtimes were older. It was rejected
before recording any optimized profile. Changed-file source synchronization
now refreshes mtimes while preserving identical-file mtimes. A forced fresh
optimized compilation then passed all 15 native controls and recorded a
distinct executable. Both optimized repeats and the fresh control receipt
share that executable. Both baseline repeats share their different
executable. The saved listings preserve the entire 1,951-case baseline set
and add exactly the ten named M2 controls, yielding 1,961 identities.
`--list` does not establish ignored dispositions; complete qualification
must verify those separately.

**Evidence bindings:** the strict local comparison helper validated the
raw native and timing logs against every summary, all work metrics and the
fresh 15-control receipt. Its ignored report is
`target/rust-performance-local/current-base-comparison.json`, SHA-256
`eb46aaffcca02e324b32953da019ccb549ab405cc8505a78447e6671fb07b589`.
Executable hashes are receipt bindings; the local analysis did not rehash
executable bytes from the compiler host. Source ancestry and patch scope
are separately reviewed, rather than inferred from timing metadata.

| Binding | Baseline | Optimized |
|---|---|---|
| Source archive SHA-256 | `c16c02eb9456ff5578253bcf872041b2b98f6d4bc6ef83df3ac2580b92a0a8bb` | `084ebfd20b538152a980c2d37c8fc790d451067d5b7174814c209a64441a6162` |
| Executable SHA-256, same within both repeats | `cd3352fcc088917a81cc9fedc63a40b08f45af4f54a66288c8956b198c4973bd` | `384ee8dcd68e2ea7aa8275cff8ddf3756d2f75f4c0e8f607f6acf66857f440f0` |
| Saved listing SHA-256, same within both repeats | `1c3789fe1a020026b9f40c30ed8abace5784bf09bf15bbab59805b1e55e3966f` | `45c0388e009847da28e5cebdd569c667810d4345a3dd721f75b125aa098c2016` |

The four raw `summary.json` digests are:

- Current baseline 01: `cee3d1a26cc9b3c2bbb02bf05779035b576c6bd093fe38b7b099b20bc46402ca`.
- Current baseline 02: `c9c7776e75be040fd801b4b03528e5891de7127c5c5c32f7ca79e617be508d2d`.
- Current optimized 01: `eb912c3869dd47cd7bc3f4b6cd1b19531006568c41f57abe2803029fba04e0f3`.
- Current optimized 02: `2e48e4c85a8bec23a743a8835b181074b2ac1f3720a8d48f467ffd4359569b85`.
- Fresh 15-control receipt: `df06a210cf6e1e9e6bdc3928389d841a6dc1b15e63feb45ba34e4891a5b4e0b5`.

### 2.3 Fresh-controller samples measure the memory tradeoff separately

A separate alternating baseline/optimized pair measures each native test's
peak resident memory. Each case starts in a fresh forked Python controller
with zero prior child CPU and maximum-RSS counters. The controller launches
the one native executable, retains the original exit and owned cleanup,
then reads Linux child resource usage after reaping it. These SQLite cases
have no external worker processes. This measures the largest waited native
child's peak, including its threads; it does not measure a process-group
sum, container peak or whole-runner memory.

The timing baseline remains `9acf0c5dc`; optimized source is
`e0fc5b9334a9a7b5ccb0732b45fcbddc7f4921dc`, whose Rust inputs are identical
to measured `45ffd543`. All four samples retain the same respective
executable hashes, full native listings, compiler/features/profile, actual
work counters and scheduling as §2.2. All 12 native bodies and the fresh
15-control receipt pass with qualified cleanup. No local compilation runs
concurrently with these samples.

| Case | Baseline peak KiB, both samples | Optimized peak KiB, both samples | Median increase KiB |
|---|---:|---:|---:|
| History | 46,636 / 46,512 | 48,060 / 48,828 | 1,870 |
| Overflow | 58,360 / 58,368 | 61,404 / 60,188 | 2,432 |
| Catalogue | 45,548 / 45,548 | 52,600 / 51,508 | 6,506 |

The measured speed improvements therefore have a small memory cost in
these isolated cases. Two samples establish their observed range, not a
stable production or full-suite memory bound. Statement caching and the
initialized fixture remain bounded, but full host admission and shard
memory measurements remain acceptance work in the canonical plan.

The strict memory comparison binds raw native logs, phase rows, individual
controller receipts and all workload counts to the frozen timing report.
Its ignored report is
`target/rust-performance-local/current-base-memory-comparison.json`, SHA-256
`1b958d30a954e72886367c847272d9860ac7af1820a124ec4c0771a53950edcb`.
The raw `summary.json` SHA-256 digests are:

- Memory baseline 01: `61f4a7ad154fa15cae6da8caaecadb227a8c8536333b729a5dc5ef2122fabd60`.
- Memory optimized 01: `32e897d799e8a8aef48af53f4e6c00b3ed9a2cb835636b02071de658dd00647a`.
- Memory baseline 02: `143cbf9f1220b8c4517cb3e08b8d9485ef7c114ed3f5109b55acc2ad6259e390`.
- Memory optimized 02: `d55d0fa138a0a562a5af58e18f8b1cbf596831e80dd05b18b8ce171500be3512`.

## 3. The candidate keeps implementation ownership narrow

The following changes are committed in `45ffd5434162b0f448e670073a19595e90d5e4a1`,
compiled and exercised by the focused controls and measurements above.
Complete M2 qualification is pending. Measured queue preparation costs
motivated the bounded cache; its real controls exercise bindings, row
draining, schema changes, isolation and rollback.

| Owned surface | Exact files and implemented boundary |
|---|---|
| Analysis-history fixture | [fragment_index_cluster/tests.rs](../../crates/plurx-core/src/store/sqlite/fragment_index_cluster/tests.rs) prepares the same INSERT once and still executes all 8,193 actual rows. A new pre-cap assertion proves the threshold was exceeded; the original 8,192 cap and queued/final-state assertions remain. |
| Production queue adapter | [background_jobs.rs](../../crates/plurx-core/src/store/sqlite/background_jobs.rs) uses the connection's bounded `prepare_cached` only in `queue_transaction`; SQL, fresh bindings, complete row stepping and commit/rollback semantics remain. Five real controls cover mixed request order, late-row errors, rollback and trigger effects, changed-schema refusal, independent Stores and durable reopen. |
| Explicit initialized fixture | New [fixture.rs](../../crates/plurx-core/src/store/sqlite/fixture.rs), gated by [sqlite/mod.rs](../../crates/plurx-core/src/store/sqlite/mod.rs), copies an immutable initialized template into a private memory database, reapplies initialization and regenerates dynamic settings. Five controls cover cold equivalence, dynamic state/PRAGMAs, mutation/rollback/FTS isolation, parallel connections and the real cold-file backfill/reopen path. Cold constructors are unchanged. |
| Single helper caller | Only `recently_added_matches_the_whole_catalogue_ranking` in [media/tests.rs](../../crates/plurx-core/src/store/sqlite/media/tests.rs) adopts the explicit fixture helper. Its 200 independent Stores, 9,339 rows and 5,000 production/oracle comparisons remain. No general Source or migration-fixture conversion is claimed. |
| Source inventory | [placeholder_census.rs](../../crates/plurx-core/src/store/placeholder_census.rs) and [replicated.rs](../../crates/plurx-core/src/store/replicated.rs) register the new fixture module in their exact source inventories. The transaction total remains unchanged. Unchecked SQLite arity remains 103; the [consistent-read census](../../crates/plurx-core/src/store/consistent_read_census.rs) ceiling remains 237. |
| Overflow instrumentation | [background_jobs_migration.rs](../../crates/plurx-core/src/store/background_jobs_migration.rs) records opt-in setup phase counts and timings; accepted/materialized/pending work and all assertions remain. Together with the seven paths above, this makes exactly eight changed Rust paths against M1 `761b371c`. |
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
count and subsequently passed the five fresh native censuses. After
integration of `8de7499e5`, all three package mapping checks report zero source drift and
all 52 focused Python formal package, runner, CI and toolchain controls pass.
The operation-progress writer bindings are incorporated from M1; worker
refinement needs no new source-pin repair. No model checker was run for this
integration. These guard results do not complete M2 qualification. The later
focused measurements in §2.2 establish only their stated local improvements.

## 4. Qualification must retain the work being measured

- [x] Record two complete focused native baselines with isolated phase rows,
  exact source/tree and process cleanup.
- [x] Assign the narrow helper and measured queue preparation surfaces; retain
  all existing assertions and add real isolation/schema/rollback controls.
- [x] Compile the measured source `45ffd543` and run all ten added controls
  plus five retained native censuses through the pinned compiler loop.
- [x] Repeat matched local measurements with unchanged case identities,
  assertions, resources and scheduling; record actual improvement and limits.
- [ ] Obtain fresh source-bound complete native and doctest evidence after
  integration. Retain all failures; later passes cannot erase earlier results.
- [ ] Preserve other platform and automatic ready-PR cutover obligations in
  the [canonical plan](FULL-SUITE-EVERY-PR-PLAN.md).

**Focused compiler evidence:** all 15 native controls passed with
`hiqlite-store` and again with `hiqlite-store,fixtures,scratch-fault-injection`.
The pinned Linux production Clippy check passed in 39.64 seconds and the
fixture-feature Core compilation passed in 66 seconds. The normal Mac
commit hook passed in 424 seconds, including its 412-second Clippy check.
These checks compile and exercise affected paths; they do not substitute
for the pending complete M2 native/doctest run. The development gate's
`compile_only` formal mode preserves `ci-inputs` checks and provides no new
TLC execution claim.

M2 does not remove expensive cases, adopt old outcomes as fresh execution,
increase deadlines, change ignore dispositions or weaken assertions.
