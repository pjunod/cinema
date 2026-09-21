# Store-contract coverage and placeholder validation — close the ten paths, then check the other dialect

**Status:** implementation complete; draft review pending · **Executes:** S10 / F-sc-13 and the
prescriptions of F-hist-1 / F-hist-2 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 in full before writing any code: it contains the executed selector
output, the two commit bodies that describe the defect class, and the exact
difference between the two placeholder dialects. §3 is four changes of very
different size — M0 is a routing edit, M3 is a design. Execute §5 in order.
If a step seems to require a mechanical `?N`→`$N` rewrite, a per-statement
exemption marker in either census, or turning every best-effort cleanup into
an error log, stop and flag it: each is refused in §4, with its reason.

**Correction to the review:** three.

1. **F-hist-2's own audit command misses the site F-hist-2 names.** The
   appendix prescribes `rg 'let _ = .*store\.' crates/`. On `0f02b7ea` that
   returns 13 lines and
   [vodserve.rs:2537-2541](../../crates/plurxd/src/vodserve.rs) — the "no
   verified holder" arm the commit body is about — is not among them,
   because rustfmt wrapped it across five lines. A multiline pattern finds
   **29** production sites in eleven files, not 13. §2.5 gives the working
   command; the audit in M2 is scoped to 29 sites, not 13.
2. **The appendix's list of out-of-scope hiqlite modules is stale.** It names
   seven (`hiqlite_{classification,dv_conversion,dvr,fragment_index_cluster,
   library_channels,shared_cache,timeline_annotations}.rs`) and 20 of 24
   `sqlite/*.rs`. The executed selector says 3 and 7, which is what §0 of the
   review already corrected and what §2.1 reproduces here. Plan against 3 and 7.
3. **The four hiqlite modules that *are* in scope are in scope by accident.**
   `hiqlite_dv_conversion.rs`, `hiqlite_fragment_index_cluster.rs`,
   `hiqlite_shared_cache.rs` and `hiqlite_timeline_annotations.rs` match only
   the `core.media` point; they reach `cluster_auth` because `select_points`
   pulls in every *dependent* point and `cluster.auth` declares
   `depends_on = ["core.media", …]`. Nothing routed them deliberately. A
   future narrowing of `core.media`'s globs silently drops four replicated
   store slices out of the three-voter lane. M0 therefore adds explicit paths
   **and** a test that derives the expected set from the directory, rather
   than trusting the dependency closure.

## 1. Objective

Board id **K-07**. Every replicated store slice and its SQLite twin selects
the three-voter lane on the PR that changes it, and neither dialect can ship
a statement whose placeholders do not match the values bound to it. A store
write whose result is discarded is either a named, counted, best-effort
operation or a logged error — never silence. Long term, one SQL source per
method emits both placeholder styles with a parameter order that is correct
by construction.

## 2. Contract today

Re-verify every line and every number below at build time.

### 2.1 The executed selector

`scope_for_paths`
([ci_scope.py:351-416](../../validation/ci_scope.py)) maps a changed-path
tuple to the CI surfaces a PR must run. `cluster_auth` is true when the
selection reaches one of three points:

```python
# validation/ci_scope.py:395-397
"cluster_auth": bool(
    {"cluster.auth", "cluster.membership", "cluster.page-reads"} & point_ids
),
```

Run against the store directory, one file at a time:

```bash
python3 - <<'PY'
import pathlib, sys
sys.path.insert(0, '.')
from validation.ci_scope import scope_for_paths
from validation.runner import load_catalog
catalog = load_catalog()
root = pathlib.Path("crates/plurx-core/src/store")
groups = {
    "hiqlite_*.rs": sorted(str(p) for p in root.glob("hiqlite_*.rs")),
    "sqlite/*.rs": sorted(str(p) for p in (root / "sqlite").glob("*.rs")),
}
for label, paths in groups.items():
    out = [p for p in paths if not scope_for_paths(catalog, (p,))["cluster_auth"]]
    print(f"{label}: {len(out)}/{len(paths)} outside cluster_auth")
    for path in out:
        print("   ", path)
PY
```

Output on `0f02b7ea`, verbatim:

```
hiqlite_*.rs: 3/16 outside cluster_auth
    crates/plurx-core/src/store/hiqlite_classification.rs
    crates/plurx-core/src/store/hiqlite_dvr.rs
    crates/plurx-core/src/store/hiqlite_library_channels.rs
sqlite/*.rs: 7/24 outside cluster_auth
    crates/plurx-core/src/store/sqlite/classification.rs
    crates/plurx-core/src/store/sqlite/dvr.rs
    crates/plurx-core/src/store/sqlite/library.rs
    crates/plurx-core/src/store/sqlite/library_channels.rs
    crates/plurx-core/src/store/sqlite/outbox.rs
    crates/plurx-core/src/store/sqlite/trakt.rs
    crates/plurx-core/src/store/sqlite/watch.rs
```

Ten paths, matching the review's §3.2 S10 row exactly. What each of the ten
selects instead: `hiqlite_dvr.rs` and `sqlite/dvr.rs` → `live.dvr`;
`sqlite/watch.rs` → `watch-state.sync`; `sqlite/library.rs` →
`library.catalog`; and so on. Each runs `rust-gate`, none runs
`cluster-auth`.

The `cluster.auth` point's path list is
[points.toml:451-494](../../validation/points.toml); it names nine
`hiqlite_*.rs` files and four `sqlite/*.rs` files explicitly. Its check list
is `["rust-gate", "rust-gate-ci", "hiqlite-vendor-clippy", "cluster-auth",
"cluster-transport-recovery"]`, and `cluster-auth` is
`command = "make cluster-check"` with `timeout_seconds = 3600`
([points.toml:265-272](../../validation/points.toml)).

### 2.2 The hiqlite census, which exists

```rust
// crates/plurx-core/src/store/placeholder_census.rs:1-9
//! A repo-wide census of the placeholder rule every replicated statement is
//! held to at runtime.
//!
//! hiqlite binds parameters by rusqlite's numeric index while SQLite treats
//! `$N` as a *named* parameter indexed by first appearance. A statement that
//! introduces `$6` before `$4` is therefore refused by
//! [`validate_sql`](super::hiqlite::validate_sql) *before any I/O* — and every
//! caller in this tree discards or misreads that error, so the refusal is
//! silent.
```

`STORE_SOURCES` ([placeholder_census.rs:33-76](../../crates/plurx-core/src/store/placeholder_census.rs))
lists all seventeen `hiqlite*.rs` slices by `include_str!`, and
`the_census_covers_every_slice_exactly_once`
([:678](../../crates/plurx-core/src/store/placeholder_census.rs)) fails if a
new slice appears without being added. `every_replicated_placeholder_is_introduced_in_order`
([:606](../../crates/plurx-core/src/store/placeholder_census.rs)) runs the
production validator over every statement, with `format!` templates resolved
first. There is deliberately **no per-statement exemption marker**, because
a marker is a loophole a future statement can opt into — keep that property.

The validator it runs:

```rust
// crates/plurx-core/src/store/hiqlite.rs:4140-4151
pub(super) fn validate_sql(sql: &str) -> Result<(), StoreError> {
    ReplicatedSql::new(sql)
        .map(|_| ())
        .map_err(|error| StoreError::Database(error.to_string()))?;
    validate_parameter_order(sql)
}

/// hiqlite binds parameters with rusqlite's numeric parameter index. SQLite
/// treats `$N` as a *named* parameter, so its index is assigned by first
/// appearance rather than by the number after `$`. Requiring first appearance
/// to be `$1`, `$2`, ... keeps `params!(...)` aligned with the statement.
fn validate_parameter_order(sql: &str) -> Result<(), StoreError> {
```

`validate_sql` is called at fourteen sites in `hiqlite.rs`, on every
replicated statement, before any I/O.

### 2.3 The `?N` side, which has no check at all

rusqlite's `?NNN` is not the same mechanism. SQLite assigns the parameter
index from the number itself, `sqlite3_bind_parameter_count()` returns the
**largest** index used, and rusqlite's `params![…]` binds positionally
`1..=n` and refuses the call when `n != parameter_count()`. Three
consequences, and only two of them are loud:

| Statement uses | `params!` arity | What happens |
|---|---|---|
| `?1..?13` | 12 | `InvalidParameterCount(12, 13)` every call — loud, and this is `cfba5ff6` |
| `?1..?11` | 12 | `InvalidParameterCount(12, 11)` every call — loud |
| `?1, ?2, ?4` | 4 | **silent**: index 3 is bound and never read, `?4` receives the fourth value, and an author who wrote `params!` in order of appearance has shifted everything after the gap |
| `?2` before `?1` | 2 | correct on rusqlite (index is the number), wrong if the same text is reused on hiqlite |

Row three is the case no runtime error catches and the reason the `?N` check
must exist as a static census rather than as a runtime validator: rusqlite
already produces rows one and two at the first call, and row three cannot be
produced at runtime at all.

### 2.4 The two incidents, in their own words

`120a3d29` (2026-09-03), F-hist-1:

> `ce253a55` made `target_node_id` part of the jobs primary key and appended
> `AND target_node_id = $6` to `renew_cluster_fragment_index` and
> `yield_cluster_fragment_index` — in front of `owner_node_id = $4` and
> `fence = $5`. hiqlite binds by rusqlite's numeric index while SQLite indexes
> `$N` by first appearance, so `validate_sql` refuses a statement whose
> placeholders are introduced out of order, and it refuses it *before any
> I/O*. … 12,193 claims, 9,915 lost leases, 1,933 dead jobs over 617 cache
> keys, and not one artifact built in seventy-two hours. Store metrics showed
> zero write errors the whole time, because the statement never reached the
> store.

and, on why no test saw it:

> The embedded twin was already correct — rusqlite's `?N` binds by number —
> which is why no SQLite-only test noticed.

`cfba5ff6` (2026-09-17), F-hist-2 — the same class, the other direction:

> `requeue_cluster_fragment_index` binds twelve parameters and asked its
> active queue cap for the thirteenth, so rusqlite refused the whole
> statement every time. Its one production caller discards the error —
> `let _ = …` in the "no verified holder could supply the v2 artifact" arm —
> so on the SQLite backend an artifact whose holders have all gone away has
> never been requeued for rebuild, silently, and the session answers "no
> verified holder" forever. The hiqlite twin binds the same twelve values
> against `$12` and is correct, so this is one backend drifting rather than
> a design that was wrong twice.
>
> That class is invisible here only because the placeholder-order validator
> is hiqlite-only: `validate_parameter_order` would have refused this
> statement at the first call with "expected $12, found $13". The same check
> is a pure function over the SQL and would be cheap to apply to the `?N`
> side; that is worth doing and is not done here.

This plan is the "worth doing and is not done here".

### 2.5 The discarded results

The appendix's command misses its own subject (correction 1). The working
one, and its result on `0f02b7ea`:

```bash
rg -nU --multiline-dotall \
  'let _ = (?:[A-Za-z_][A-Za-z0-9_]*\s*\.\s*)*store\s*\.[\s\S]{0,240}?\.await' \
  crates/plurxd/src crates/plurx-core/src --no-heading -o | rg ':let _ = '
```

29 production sites in eleven files: `state.rs` ×11, `transcode.rs` ×4,
`fragment_index_cluster.rs` ×4, `shared_cache.rs` ×2,
`http/internal_media.rs` ×2, and one each in `vodserve.rs` (the F-hist-2
arm), `offline.rs`, `live_tv/dvr.rs`, `library_search.rs`,
`http/extract.rs` and `channel_subjects.rs`. They are not one kind of
thing:

```rust
// crates/plurxd/src/vodserve.rs:2536-2543 — lost work, silent
let _ = self
    .shared
    .store
    .requeue_cluster_fragment_index(&repair)
    .await;
return Err("no verified holder could supply the v2 artifact".to_owned());

// crates/plurxd/src/shared_cache.rs:1250 — best-effort release; the lease
// expires on its own if this fails
let _ = self.store.release_lease(&lease, unix_ms()).await;
```

The assessment's F-hist-2 row is explicit about the difference:

> Do not turn every intentionally best-effort cleanup result into an
> unbounded error stream. Classify operation, cancellation, retry and failure
> severity; retain a regression through the actual no-holder path.

### 2.6 What adding the ten paths costs

Of the 19 non-merge commits since 2026-08-20 that touched one of the ten
files, **3** select no cluster lane under today's routing. Those three are
the whole new cost of M0: three extra `make cluster-check` runs in a month.

## 3. Change

### 3.1 Route the ten paths, and derive the expectation from the directory

Add the ten paths of §2.1 to `cluster.auth`'s `paths`
([points.toml:451-494](../../validation/points.toml)). Adding them to
`cluster.auth` rather than `cluster.page-reads` is deliberate: `cluster.auth`
is the point whose contract sentence is the backend-neutral `Store` suite on
SQLite and three voters, which is the property these files can break.

Then a test in [tests/validation/test_runner.py](../../tests/validation/test_runner.py)
that enumerates `crates/plurx-core/src/store/{hiqlite_*.rs,sqlite/*.rs}` from
the filesystem and asserts `scope_for_paths` returns `cluster_auth = True`
for each. Derived from the directory, not from a list, so a new store slice
cannot be added without either routing it or failing the test — the same
discipline `the_census_covers_every_slice_exactly_once` already applies on
the Rust side. It also pins correction 3: if `core.media`'s globs narrow, the
four accidentally-covered modules fail the test instead of going quiet.

### 3.2 A `?N` census, which is a different check

A second census in
[placeholder_census.rs](../../crates/plurx-core/src/store/placeholder_census.rs),
sharing the existing literal/`#[cfg(test)]`-stripping machinery
(`literals_and_code_mask`, `test_item_ranges`, `resolve_template`) and adding
`SQLITE_SOURCES` over all 24 `sqlite/*.rs` files, with the same
directory-matches-the-list test.

What it checks, per statement:

1. Every `?N` satisfies `1 ≤ N ≤ max`, and the set of distinct `N` used is
   exactly `{1..=max}` — **no gaps**. This is §2.3 row three, the silent one.
2. No bare `?` (anonymous parameter) appears in a statement that also uses
   `?N`. SQLite assigns a bare `?` the next unused index, which makes the
   text's meaning depend on what precedes it.
3. Where the statement literal and its binding are the same call expression —
   `conn.execute(<sql>, params![…])`, `query_row`, `query_map`,
   `prepare(…)` followed by a bind in the same block — `max` equals the
   literal arity of the `params!`/slice. A call whose arity is not statically
   determinable (a built `Vec`, `json_each` fan-out) is reported as
   `unchecked` and counted; the test fails if the `unchecked` count grows
   above a pinned number, so the unchecked set can only shrink.

Check 3 is what would have caught `cfba5ff6` at the first commit. Checks 1
and 2 are what rusqlite can never report.

No per-statement exemption marker, for the reason `120a3d29` gives: a marker
is a loophole a future production statement can take.

### 3.3 Discarded results: classify, log, count

A helper in `crates/plurxd/src/store_result.rs`:

```rust
/// Every store call whose Result is not propagated goes through here, so the
/// decision to drop it is written down and counted rather than typed as `_`.
pub(crate) enum Discard {
    /// Failure costs work a user is waiting for. Logged at error level.
    LostWork,
    /// A lease, cache row or cursor that another mechanism reclaims on its
    /// own schedule. Logged at debug level, rate-limited.
    BestEffort,
    /// The caller is already unwinding; the store error is not the cause.
    Cancelled,
}

pub(crate) fn observe<T, E: std::fmt::Display>(
    operation: Operation,      // a fixed enum, never a free string
    severity: Discard,
    result: Result<T, E>,
);
```

`Operation` is an enum with one variant per call site, so the metric's label
set is closed at compile time:

```
# HELP plurx_store_discarded_results_total Store results the caller did not propagate.
# TYPE plurx_store_discarded_results_total counter
plurx_store_discarded_results_total{operation="requeue_cluster_fragment_index",severity="lost_work",outcome="error"} 0
plurx_store_discarded_results_total{operation="release_lease",severity="best_effort",outcome="error"} 0
```

Labels: `operation` ∈ the enum (29 today, one per site); `severity` ∈
{`lost_work`, `best_effort`, `cancelled`}; `outcome` ∈ {`ok`, `error`}.
Error-level logs are rate-limited per operation (one per 30 s, with a
suppressed count) so a peer outage cannot turn a cleanup path into a log
flood — the assessment's "unbounded error stream" guardrail.

The classification of all 29 sites is part of M2's PR and is reviewed
site-by-site; `vodserve.rs:2537` is `LostWork`, `shared_cache.rs:1219,1250`
are `BestEffort`. Each classification carries a one-line reason at the site.

And a lint so the audit does not have to be repeated: a test in
`placeholder_census.rs`'s neighbourhood that runs the §2.5 multiline pattern
over `crates/*/src` and fails on any match, because every site now goes
through `observe`. Directory-derived, no allow-list.

### 3.4 One SQL source, and why the rewrite cannot be mechanical

The two dialects differ in *where the index comes from*:

```
SQL text:   UPDATE jobs SET … WHERE target_node_id = ?6
                                 AND owner_node_id = ?4 AND fence = ?5

rusqlite ?N : index is the number.        ?6 -> 6, ?4 -> 4, ?5 -> 5   (correct)
hiqlite  $N : index is first appearance.  $6 -> 1, $4 -> 2, $5 -> 3   (swapped)
```

That is `120a3d29` exactly. A textual `?`→`$` substitution preserves the
characters and inverts the meaning, and it inverts it *silently on one
backend only*. So the shared representation cannot be a string that both
sides format; it has to own the parameter order:

```rust
// sketch — the shape, not the API
let stmt = Stmt::new("UPDATE cluster_fragment_index_jobs SET state = 'queued'")
    .where_eq("target_node_id", Param::Node)      // declared order is
    .where_eq("owner_node_id",  Param::Owner)     // binding order
    .where_eq("fence",          Param::Fence);
stmt.sqlite()   // "... target_node_id = ?1 AND owner_node_id = ?2 AND fence = ?3"
stmt.hiqlite()  // "... target_node_id = $1 AND owner_node_id = $2 AND fence = $3"
stmt.params()   // [node, owner, fence] — one list, both dialects
```

The invariant the generator guarantees, and the reason it is worth the
work: **first appearance order equals numeric order equals binding order**,
in both dialects, by construction. Under that invariant `?N` and `$N` are
interchangeable — which is exactly why the mechanical rewrite is unsafe
*before* the invariant is established and safe after it.

This is the M-sized part. M3 pilots it on one method pair with an
equivalence test; whether it spreads is a §7 question, not a commitment.

## 4. Guardrails (non-goals)

- **The `?N` check is a different check, not the same code** (S10 row,
  F-sc-13 row: "SQLite binds by index, so it is a different check"). Honoured
  by §3.2: three rules, two of which have no `$N` equivalent, and a table in
  §2.3 showing why.
- **No mechanical `?N`→`$N` rewrite** (F-sc-13 row: "unsafe because
  first-occurrence binding differs"). Honoured by §3.4, which makes the
  generator own the order rather than the text, and states the invariant
  under which the two become equivalent.
- **Do not turn every best-effort cleanup into an error stream** (F-hist-2
  row). Honoured by §3.3's three severities, debug level for `BestEffort`,
  and the per-operation rate limit.
- **Retain a regression through the actual no-holder path** (F-hist-2 row).
  M2's PR includes a store-contract scenario that drives
  `requeue_cluster_fragment_index` through the `vodserve.rs:2537` arm on
  both backends, asserting the row returns to `queued` — the test that would
  have caught 22 days of silence.
- **Instrument validation failures before I/O, on both backend contracts**
  (F-hist-1 row). M1's PR adds `plurx_store_validation_refusals_total`
  next to the existing store error families, so a refusal is an error metric
  distinct from an I/O error — `120a3d29`'s "zero write errors the whole
  time" is the symptom this closes.
- **Dead-job alerts need workload and time-window definitions** (F-hist-1
  row). This plan does **not** add a `jobs_dead_total` alert; it adds the
  counter and leaves the alert to the plan that owns queue observability, so
  that a threshold is not invented here without a workload. Named in §7.
- **Current main compile policy is a separate execution gap** (F-sc-13 row).
  This plan changes routing, not the main-branch policy; §2.2 of the review
  owns that and is a different board row.
- **No per-statement exemption marker** in either census (`120a3d29`'s own
  rule). Honoured in §3.2.
- **No settings key, no metric relabelling, no recipe or cache-digest
  change.** Nothing here enters a transcode recipe or a cache key.

## 5. Milestones

One draft PR per milestone into `main` under the fast lane.

### 5.1 M0 — route the ten paths

`points.toml` edit plus the directory-derived scope test.

Acceptance: the §2.1 script prints `hiqlite_*.rs: 0/16 outside cluster_auth`
and `sqlite/*.rs: 0/24 outside cluster_auth`;
`python3 -m unittest discover -s tests/validation -p 'test_*.py'` green,
including the new `test_every_store_slice_selects_the_replicated_lane`;
deleting one of the ten paths from `points.toml` makes it fail. The PR body
carries the before/after of §2.6 (19 commits touched the ten in 30 days;
3 of them select no cluster lane today).

Note for whoever runs it: a `points.toml`-only edit forces the cluster lane
by itself ([ci_scope.py:400-407](../../validation/ci_scope.py)), so this PR
runs `make cluster-check` on its own account.

### 5.2 M1 — the `?N` census

`SQLITE_SOURCES` and the three checks of §3.2, plus
`plurx_store_validation_refusals_total`.

Acceptance: `cargo test -p plurx-core store::placeholder_census` green;
introducing `?13` into a statement that binds twelve values, or a `?1, ?2,
?4` gap, makes it fail with the offending file, line and statement;
`cargo test -p plurx-core store::placeholder_census::sqlite_module_list`
fails when a new `sqlite/*.rs` is added without being listed. `make unit`
green.

### 5.3 M2 — the discarded-result audit

`store_result.rs`, all 29 sites classified, the metric, the rate limit, the
lint, and the no-holder regression.

Acceptance: the §2.5 command returns no matches;
`cargo test -p plurxd store_result` covers the rate limit and the three
severities; `cargo test -p plurx-core --features hiqlite-contract-tests
requeue_through_the_no_holder_arm` green on both backends (this one needs
the replicated lane, so it is `make cluster-check`, not `make unit`);
`make unit` green. The PR body lists the 29 sites with their severity and
one-line reason.

### 5.4 M3 — one SQL source, piloted

The `Stmt` builder of §3.4 applied to exactly one method pair. `next_up` is
the candidate the review names
([sqlite/watch.rs:488-520](../../crates/plurx-core/src/store/sqlite/watch.rs)
vs [hiqlite_media.rs:3904-3931](../../crates/plurx-core/src/store/hiqlite_media.rs)),
and it is also one of the ten files M0 routes, so the three-voter lane covers
it from M0 onward.

Acceptance: `cargo test -p plurx-core store::sql_source` proves
`stmt.sqlite()` and `stmt.hiqlite()` differ only in the sigil and that both
pass their dialect's validator; `cargo test -p plurx-core
--features hiqlite-contract-tests next_up` returns identical rows from both
backends on the same fixture; the two hand-written `next_up` statements are
deleted in the same PR, and `git diff --stat` shows the net line change.

### 5.5 M4 — decide the spread

No code. A section appended to this document recording, from M3's measured
cost, whether the generator spreads to the other ~50 duplicated statements,
stays at `next_up`, or is reverted. The review's own framing — "~50 hot
statements exist twice" — is a count, not a mandate.

Acceptance: the section exists, names a number of statements, and the board
row moves to `done`.

## 6. Verification and rollout

Fast lane `make unit` per PR plus the named filters; M0 and M2 additionally
run `make cluster-check` (M0 because a catalog edit forces it, M2 because its
regression is a replicated contract). Nothing here changes a running
binary's behaviour except M2's logging and metrics, so a mixed fleet is not a
consideration; rollback is a revert.

M0 before M1 before M2 is not arbitrary: M1's census will find statements in
the seven `sqlite/*.rs` files M0 routes, and M2's regression runs on the lane
M0 turns on for those files. Running them out of order means finding a defect
on a lane that does not yet execute.

No device or fleet step is needed for any milestone; there is no GPT prompt
in this plan.

## 7. Open questions

1. Does adding ten paths to `cluster.auth` change the *median* PR's lane set?
   §2.6 says three commits in thirty days, which suggests no — but that count
   is over commits, and a PR bundles several. Measure it on the first ten PRs
   after M0 and record it in the Execution log.
2. The `unchecked` pinned count in §3.2 check 3: what is it on `0f02b7ea`?
   M1 computes it; if it is large, check 3 is weak on arrival and the number
   itself is the finding.
3. `jobs_dead_total` rising while `artifacts_built_total` is flat is
   F-hist-1's proposed alert. It needs a workload definition and a time
   window this plan does not have. Which board row owns it?
4. Should `validate_parameter_order`'s `$N` rule and the new `?N` rule share
   an error type, so a future shared-source violation reports once rather
   than twice? Cheap either way; decide at M3.
5. Whether `cluster.page-reads` rather than `cluster.auth` is the better home
   for `sqlite/library.rs` and `sqlite/watch.rs` — both are page-read
   surfaces and `cluster.page-reads` already names `sqlite/media.rs`. This
   plan puts all ten in `cluster.auth` because the property at risk is
   backend parity, not page latency; Paul confirms.

## 8. M4 spread decision

Keep the generator at the `next_up` pilot. The pilot replaced **two**
hand-written statements with **one** typed source, but its measured patch was
119 insertions and 61 deletions (net +58), including a 112-line source module.
That is a reasonable fixed cost for proving the invariant, not evidence for a
mechanical conversion of the review's approximately **50** duplicated hot
statements. The current two-parameter API also has not yet proved the predicate,
list-expansion or optional-clause shapes those other statements use.

This is deliberately a stay decision rather than a revert. The generated
SQLite and hiqlite texts differ only by sigil, both dialect validators accept
them, and the backend-neutral watch contract returns the same `next_up` row.
The next proposal to spread it must first inventory the actual duplicated SQL
shapes and demonstrate a net reduction across at least three unlike methods;
it must not mechanically rewrite sigils.

M1 measured **91** statement variants whose binding arity is not local enough
for the conservative static scanner to prove. That count is pinned and may
only fall without review. It is a useful guard against regression, but large
enough that arity coverage should be tightened before shared generation is
presented as the primary safety mechanism.

Decisions made during execution where the plan named no owner:

- Keep the `$N` and `?N` validation errors separate. Their invariants differ,
  and collapsing them would obscure whether first appearance or numeric gaps
  caused a refusal.
- Assign the deferred dead-job/workload alert definition to C-08 observability;
  this plan supplies the refusal counter but has no evidence-based alert window.
- Retain `cluster.auth` for all store slices and the two new shared enforcement
  modules. The property is backend parity, not page-read latency.
- Measure the median-PR lane effect over the first ten post-merge PRs as
  follow-up evidence. No post-M0 PR sample exists while this implementation is
  still a draft, so that observation is not fabricated here.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M0 | [#411](http://192.168.4.7:3000/noirr/plurx/pulls/411) / `07796d70` | Routed all store slices; selector moved from hiqlite 3/16 + SQLite 7/24 outside `cluster_auth` to 0/16 + 0/24. Directory-derived regression passed. Historical cost remains 3 extra cluster lanes among 19 touching commits in 30 days. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M1 | [#411](http://192.168.4.7:3000/noirr/plurx/pulls/411) / `6e1554ed` | Added 24-file SQLite census, gap/mixed-spelling/local-arity checks, pinned 91 unchecked variants, fixed two real gaps, and exposed the fixed-cardinality pre-I/O refusal counter. Fourteen focused census tests and the counter regression passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M2 | [#411](http://192.168.4.7:3000/noirr/plurx/pulls/411) / `3ebc50be` | Classified all 29 discarded results: 25 best-effort, 3 lost-work, 1 cancelled. Closed labels, 30-second per-operation log windows, metrics, source lint, and actual VOD no-holder requeue regression passed; backend-neutral repair contract passed on SQLite. Replicated-lane execution remains CI evidence. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M3 | [#411](http://192.168.4.7:3000/noirr/plurx/pulls/411) / `8cfd50a5` | One typed parameter order now renders both `next_up` dialects; equivalence/validator, SQLite behavior, and backend-neutral watch-contract regressions passed. Measured patch: +119/-61, net +58. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M4 | [#411](http://192.168.4.7:3000/noirr/plurx/pulls/411) / this commit | Decision: keep the safe pilot, do not spread or revert. A future spread needs a shape inventory and net reduction across at least three unlike methods. |
