# Roster applied marks — invalidate directory snapshots by table

**Status:** built — implemented and independently reviewed. **Owner:** Codex.

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811). Earlier branch
names and checkpoint receipts below are historical. Fleet acceptance remains
after Paul’s deployment.

Companion to [the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md)
and [the facade contract](CLUSTER-FACADE-BUILD.md). This change makes the
existing directory cache selective; it changes neither membership policy
nor transport, listener ownership, admission or snapshot durability.
Integration target: `effort/cluster-facade`.

## Source findings and contract

At effort `53d99f6c6`, production registers 13 tables: two applied-view
tables and eleven sharing-watch tables. The four canonical roster tables
therefore require 17 slots. Widen the bounded mark from 16 to 32 using
`AtomicU32` and `u32` masks, with an explicit full-width mask case. Keep
revision/index/generation widths, atomic orderings and the public API.
Repeat the registration count on the final integrated base.

Append `membership::directory::ROSTER_TABLES` to `applied_mark_tables()`;
preserve all existing slot order and keep actual view consumers separate.
Use the canonical list rather than duplicating table names. Each roster
write invalidates the very next directory read. An unrelated settings
write preserves the held snapshot and reload count. Unknown or absent
slots retain conservative applied-index fallback.

Keep main-database-only row hooks, tokens delete-only behavior, schema-wide
invalidation, publication before applying replies, no lost wakeup, and
odd/untrusted image generations until replacement and pool reconnection
finish. Existing qualified-DELETE census expands with the registry; never
weaken it or its historical replay caveat. No wire or persisted layout
change, new polling, feature switch or fleet deployment is required.

## Ownership and delivery

| Files | Work |
|---|---|
| `vendor/hiqlite/src/applied_mark.rs` | Bounded width and upper-slot regressions; retain existing atomic protocol. |
| `crates/plurx-core/src/store/applied_view.rs` | Append canonical roster registrations, with existing consumers unchanged. |
| `crates/plurx-core/src/cluster/membership/directory.rs` and its tests | Registration and selective cache invalidation evidence. |
| `vendor/hiqlite/PLURX-PATCH.md`, existing fork ledger checks | Extend patch 27 consistently; no renumbering or upstream identity rewrite. |
| This plan, cluster index, project backlog, functionality catalog | Delivery and evidence ledger. |

The sharing member-floor fixture qualifies its removal-table cleanup DELETE
with `WHERE 1 = 1`, preserving whole-table cleanup while ensuring SQLite
reports row deletes for this newly registered table.

The existing migration collector needs no code change. Core migration,
vendor startup and listener handoff remain with their current owners.

Establish pinned Rust 1.97.1 compilation before edits. Run focused vendor
mark/watch tests, upper slots 16/31, full 32-slot invalidation, rejection of
33 tables, writer publication and snapshot replacement tests. Run the real
directory contract with `hiqlite-contract-tests`, sharing-watch contract
with `cluster-read-cost-validation`, and qualified-DELETE/update-hook
census with `hiqlite-store`. Run formatting, relevant Clippy, fork ledger,
catalog and paperwork checks. Record actual results and limitations.

Integrate the current effort before task qualification; preserve C7 when
current main is integrated for effort promotion. Obtain one
adversarial review of the complete diff, fix findings, run the Effort
development gate once on the exact final head and land with its regression
trailers only on a pass. Main promotion and fleet acceptance remain
separate. No implementation or execution success is claimed by this plan.

## Local evidence — 2026-11-17

Pinned Rust 1.97.1 baseline core all-target compilation with `hiqlite-store`
passed before source edits. The widened applied-mark suite passes 13 tests,
including upper slots, the full-width schema mask, delete-only behavior,
watch wakeup and rejection of a 33rd table. All six writer applied-mark
regressions pass. Both snapshot generation/failed restore cases pass.

All eight directory cases pass with `hiqlite-contract-tests`, including the
real replicated fixture: an unrelated settings write preserves the cached
Arc and reload count, while node/HTTP/progress/removal changes alter the
next answer. Both update-hook census tests pass with `hiqlite-store`.
Fork-ledger and ownership checks pass 22 tests with one existing skip.
These are local results; remaining checks, exact-head review/gate and
landing are not claimed. No deployment or fleet acceptance is claimed.
