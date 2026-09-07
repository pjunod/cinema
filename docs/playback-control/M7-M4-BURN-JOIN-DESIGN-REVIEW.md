# M7 M4 (burn-join) design review — approve, with one contract correction

**Status:** review complete · **Reviews:** the M4 burn-join design of
2026-09-01 · **Verified against:** Paul's plurx working tree by direct file
reads, 2026-09-01 (post-M3: `subtitle_readiness` is present; the tree could
not be pinned to `1a64fa8` exactly, so re-verify quoted line numbers at
build time) · **Written:** 2026-09-01

**Verdict: approve.** The architecture is right, the transaction reasoning
is right, and the tree confirms nearly every claim. Before building: fix
the `Ok(None)` contract (§2 — it is wrong for one case the store already
handles correctly), add the two audit manifests the trait addition must
touch (§3), and resolve two dangling document references (§4). §5 is good
news: the Hiqlite half is more tractable than the design budgets for.

---

## 1. What the tree confirms

Every load-bearing claim checked out:

| Claim | Verified |
|---|---|
| Four trait methods at mod.rs 2525 / 2531 / 2556 / 2577 | ✓ (trait is `MediaSessionStore`, declared mod.rs:2469 — see §6) |
| `media_session_preparations` schema, PK `(user_id, playback_id)` | ✓ (mod.rs:118–129; the quote drops two `CHECK (length(...))` constraints — see §6) |
| Nothing in `plurxd` calls any of the four | ✓ — callers exist only in the two impls and `store_contract.rs` |
| §4's abort filter, quoted verbatim | ✓ — sqlite/sessions.rs:1785–1790 |
| `prepare_media_session` body wholly inside `unchecked_transaction` | ✓ — tx opens at sessions.rs:1292 |
| Abort = tx + `abort_staged_generation(&tx, …)` | ✓ — sessions.rs:1772–1773 |
| hiqlite_sessions.rs:1601 is `abort_media_session_preparation` | ✓ |
| `hiqlite-contract-tests` feature and the §5 test invocation | ✓ — plurx-core/Cargo.toml:29; `media_session` matches the suite's names |

The §1 rule is consistent with the pointer semantics in the tree: abort
never moves `media_playback_pointers`, only commit does, so "subtitle work
never commits an advance on its own" holds by construction. The SQLite
factoring plan (`prepare_within` as a pure refactor, then rejoin =
one tx + `abort_staged_generation` + `prepare_within`) is sound as written.

---

## 2. The `Ok(None)` contract is wrong for crash-retry of a successful rejoin

The design's doc comment says the named-row-gone case means the caller
should re-prepare, and that retrying the rejoin "will keep losing." One
case breaks that: **the owner whose rejoin succeeded, then crashed before
seeing the response.**

SQLite's `prepare_media_session` opens with a replay-by-exact-identity
branch (sessions.rs:1293–1330): a retry carrying the same staged
incarnation id and predecessor finds its own staged row and gets its route
back — the comment there says exactly why, the ledger's primary key would
otherwise turn the retry into a refusal from its own earlier attempt. A
rejoin composed as abort-then-`prepare_within` inherits this for free: the
abort of the now-gone old id no-ops (every statement is ledger-gated), then
`prepare_within` hits the replay branch and returns `Ok(Some(route))`.

So the composed implementation is *correct* and the documented contract is
not. Two changes:

1. Fix the doc: `Ok(None)` means the named row is gone **and** the caller's
   own merged preparation is not the staged row. A retry that finds its own
   staged row is a replay and returns the route.
2. Add a fourth acceptance scenario to §5's list: **idempotent replay of a
   successful rejoin returns the merged route, both backends.** The Hiqlite
   twin must implement this classification explicitly — it has no
   `prepare_within` to inherit from; `commit_media_session_preparation`'s
   fresh-commit-vs-transaction-replay changed-count pattern
   (hiqlite_sessions.rs, ~1560) is the model to copy.

---

## 3. Two audit manifests the design does not list

A new `MediaSessionStore` method must be registered in both, or the
existing audits fail — these are exactly the blind-compile call sites the
design's own §6 warns about, so name them up front:

- `MEDIA_SESSION_METHODS` in `crates/plurx-core/tests/store_contract.rs`
  (~line 500) — the per-method coverage manifest. Add
  `"rejoin_media_session_preparation"`.
- The `SqliteTransactionSite` inventory in
  `crates/plurx-core/src/store/replicated.rs` (~line 430 onward) — every
  sessions.rs transaction method is catalogued with its mechanism and
  shape. Add an entry (`RusqliteTransaction`, `ReadBranchWrite`).

Also: `crates/**` is inside `validation/points.toml` `audit_paths`, so this
PR needs a functionality-point entry — the docs-only exemption the M7
planning session recorded does not apply to a code PR.

---

## 4. Two cited documents are not in the tree

- `docs/playback-control/M7-REMAINDER-HANDOFF.md` — the "§6"/"§9.4" the design quotes — is
  absent from `docs/`, even though M1's `subtitle_readiness` is present.
  PR #740 appears to still be unmerged. Merge it (Paul's call) before
  building, or the design's section references dangle.
- `COMPILE-LOOP.md` exists nowhere in the repo. The design's own closing
  instruction is "see COMPILE-LOOP.md" — give it a real path or inline the
  loop it describes.

---

## 5. Good news: the Hiqlite stop condition almost certainly won't trigger

The design budgets for the possibility that one Raft proposal cannot
express abort-then-prepare. The tree says it can: the Hiqlite abort is
already a **single `client().txn(vec![...])` proposal of four ledger-gated
statements executed in order** (hiqlite_sessions.rs:1601 onward). One
proposal expresses the join by concatenating abort's statements with
prepare's inlined-check INSERT — statements in a txn batch run
sequentially, so the INSERT's gates (pointer = expected predecessor,
admission counts, no ledger row) evaluate after the DELETE has run.

The practical recipe mirrors the SQLite factoring: pull the
statement-vector construction of the Hiqlite abort and prepare into helpers
returning `Vec<(&str, hiqlite::Params)>`, and have rejoin concatenate them
into one `txn()` call. The post-txn reads (replay classification per §2,
then the §4 second-line filter) stay outside the proposal, as they already
do in abort and commit. Budget for the work, but expect it to fit — the
flag stays available if the gating genuinely cannot be expressed in SQL.

---

## 6. Citation fixes (cosmetic — correct before handing to the builder)

- The trait is `MediaSessionStore` (mod.rs:2469), not `Store`.
- "The replicated twin inlines the same check into its INSERT" is an impl
  comment at **sqlite/sessions.rs:1395**, not the trait doc at mod.rs:2583
  (2583 sits inside abort's own doc).
- The schema quote is presented as exact but drops the
  `CHECK (length(...) BETWEEN 1 AND 128)` constraints on `playback_id` and
  `expected_predecessor_incarnation_id`.

None of these change the design; all three would misdirect a builder
navigating by the quoted locations.
