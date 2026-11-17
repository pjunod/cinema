# Roster applied marks — invalidate directory snapshots by table

**Status:** open — plan committed before implementation. **Owner:** Codex.

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
| `crates/plurx-core/src/cluster/membership/directory/{tests,invalidation_tests}.rs` | Registration and selective cache invalidation evidence. |
| `vendor/hiqlite/PLURX-PATCH.md`, existing fork ledger checks | Extend patch 27 consistently; no renumbering or upstream identity rewrite. |
| This plan, cluster index, project backlog, functionality catalog | Delivery and evidence ledger. |

The existing migration collector needs no code change. Core migration,
vendor startup and listener handoff remain with their current owners.

Establish pinned Rust 1.97.1 compilation before edits. Run focused vendor
mark/watch tests, upper slots 16/31, full 32-slot invalidation, rejection of
33 tables, writer publication and snapshot replacement tests. Run the real
directory contract with `hiqlite-contract-tests`, sharing-watch contract
with `cluster-read-cost-validation`, and qualified-DELETE/update-hook
census with `hiqlite-store`. Run formatting, relevant Clippy, fork ledger,
catalog and paperwork checks. Record actual results and limitations.

Integrate current effort/C7 before final qualification. Obtain one
adversarial review of the complete diff, fix findings, run the Effort
development gate once on the exact final head and land with its regression
trailers only on a pass. Main promotion and fleet acceptance remain
separate. No implementation or execution success is claimed by this plan.
