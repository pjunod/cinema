# Full-suite setup performance — measure fixture cost before optimizing it

**Status:** open — M2 profiling in progress; no setup optimization qualified ·
**Written:** 2026-10-21 · **Owner:** Codex ·
**Branch:** `codex/full-pr-suite-m2`

Companion to the [canonical every-PR plan](FULL-SUITE-EVERY-PR-PLAN.md)
(the complete execution contract) and the
[M1 repair ledger](RUST-SUITE-REPAIR-2026-10-20.md) (the corrected Rust
baseline). This ledger owns M2 setup measurements and any resulting fixture
optimization. Work uses an independent checkout; the user checkout is not
used for edits or tests. Preserve all assertions and scenario concurrency.
If a measured optimization needs production behavior changes, record and
review that extension before implementing it.

M1 is qualified at its exact source through the receipt in
[PR #1244](http://forge.lan:3000/noirr/plurx/pulls/1244). Its merge into
`effort/full-pr-suite` is held for user clarification because that PR carries
M0 diagnostics and the writer correction as well as test repairs. A green
Rust result neither merges the PR nor qualifies other platform lanes or an
automatic every-PR policy cutover.

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
SQLite migrations, SQL preparation, media generation or teardown. Isolated
phase costs are unknown until M2 records them. The 49 ignored statuses remain
ignored; successful serial executions are separate observations. M2 must
retain the complete baseline identities and seven doctest targets rather
than making a faster run by selecting fewer tests.

## 2. Measure construction, body and cleanup separately

Record source/tree, toolchain, features, command, runner limits, parallelism
and artifact hashes with each comparison. Use the same candidate and fixed
resources to separate fresh Store construction, migration/backfill, fixture
population, actual test work and owned shutdown. For Source/media fixtures,
separate database construction, media generation, probing, body and cleanup.
Report phase boundaries and samples; do not subtract unrelated whole-job
measurements and call the remainder setup.

Keep the existing four-thread ordinary execution and 22-thread focused
cohort as separately labelled measurements. Cache state and external load
must be recorded as limitations when uncontrolled. A before/after comparison
must establish that the intended work was performed in both samples.

## 3. Keep implementation ownership narrow

| Surface | Intended M2 ownership and boundary |
|---|---|
| Test Store initialization | Test-only helper work around [SQLite construction](../../crates/plurx-core/src/store/sqlite/mod.rs) and [Store contract fixtures](../../crates/plurx-core/tests/store_contract.rs), selected only after phase measurements. Preserve real empty/upgrade/backfill cases and independent databases. |
| Core domain fixture setup | [Queue fixture helpers](../../crates/plurx-core/tests/support/queue_fixture.rs), if measured setup or statement preparation is material. Keep domain assertions, queue transitions and synthetic-clock contracts intact. |
| Daemon fixture construction | [Source actor fixtures](../../crates/plurxd/src/transcode/tests/source_actor.rs) and [HTTP Source fixtures](../../crates/plurxd/src/http/shared_source_playback.rs), if measured construction dominates. Preserve real Store/producer ownership and actual media/proof behavior. |
| Production queue statement cache | Conditional extension only. No production queue optimization is claimed or approved by a fixture-only measurement. Require measured preparation cost and the controls below before extending implementation scope. |
| Integration and evidence | Root agent owns Rust integration and commits; this ledger records measurements, reviewed scope and acceptance. File ownership must be assigned before parallel edits. |

A reusable initialized fixture must copy immutable state into each test's
private database. It must retain the schema version, PRAGMAs, identity,
WAL/sidecar lifecycle and independent mutation behavior. Never share a
mutable Store across test cases or replace migration tests with a prepared
current-schema database.

A production queue cache extension needs a measured SQL preparation cost on
the real queue path, not an assumption from total Store time. Retain SQL and
transaction semantics. Prove fresh parameter binding for repeated distinct
requests, commit and rollback behavior, schema-change invalidation or safe
repreparation, concurrent ownership and shutdown. Compare the uncached
baseline and include a rollback path for the change. Caching must not retain
a previous request's values or turn an error into a successful result.

## 4. Qualification must retain the work being measured

- [ ] Capture isolated phase measurements and name the source, resources and
  cache state; rank changes by measured cost rather than presumed setup cost.
- [ ] Assign exact helper files and review each implementation against its
  fixture boundaries before changing production scope.
- [ ] Preserve all existing assertions, identities, ignores, doctest targets,
  production deadlines and scenario concurrency. Add isolation, lifecycle and
  schema/rollback controls for any reused setup or prepared statement cache.
- [ ] Run focused controls through the established compiler loop, then obtain
  fresh source-bound complete native and doctest evidence for the integrated
  candidate. Retain failures; a later pass cannot erase an earlier result.
- [ ] Record measured improvement and unresolved costs without claiming other
  platform qualification or automatic ready-PR execution.

M2 does not delete expensive cases, adopt earlier test outcomes as fresh
execution, increase deadlines to hide setup, change ignore dispositions, or
weaken assertions. The [canonical plan](FULL-SUITE-EVERY-PR-PLAN.md) still
owns broader lane repair, merge-candidate binding and the eventual cutover.
