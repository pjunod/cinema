# General cluster change feed

**Status:** ready to build · **Executes:** B4 from [cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) · **Written:** 2026-11-19

## 1. Objective

Expose `cluster.watch(tables)` as the common per-table wake source. Begin by moving the existing A6 listeners and body monitors to it, preserving authority reads, rearming before observation, expiry terms and shutdown. A wake is a hint to re-observe, never authority.

## 2. Contract

Re-verify against `store/applied_view.rs`: existing `SharingWatch` uses the trusted applied mark and registered table slots; missing or untrusted marks and unknown tables fall back to one second. General `TableWatch` retains that behavior. A watch must be armed before the read it follows. An image replacement wakes existing watchers. SQLite remains interval based; no false local-change guarantees.

The facade retains the same mark used to compose its directory, clones it without exposing the handle, and returns a narrow watch. `AppliedView::sharing_watch` remains a compatibility delegate. Existing consumer loops retain time-based expiry and retry deadlines.

## 3. Ownership

Own `crates/plurx-core/src/store/{applied_view.rs,mod.rs}`, `crates/plurxd/src/cluster.rs`, `sharing.rs`, `http/{shared_library.rs,shared_receiver_playback.rs}` and this document. Other audit sessions own peer-proof behavior, placement and general timing. Any remaining polling migration needs a source-backed proof of wake coverage; lease expiry, external network discovery and periodic proof renewal are not table-change-only waits.

## 4. Delivery

1. Establish exact-source core compile baseline in the workflow-owned compiler slot.
2. Generalize the existing watch without changing its fallback or trust semantics; migrate A6 factories through the facade.
3. Compile core, test table identity/unknown table behavior and existing monitor regressions on the merged source once ready.
4. Inventory remaining D3 pollers and report unbuilt migrations explicitly. This first delivery does not claim complete B4 acceptance or the measured idle floor.
