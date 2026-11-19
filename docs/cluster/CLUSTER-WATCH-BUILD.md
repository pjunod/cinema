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

## 5. Idle lease inventory

The current lease loop already avoids idle quorum reads via `owned_media_sessions_hint`, but still repeats that local SQL every three seconds. Retain the three-second safety tick and all release/settlement queues. Cache only an empty inventory, guarded by a watch armed over `media_sessions` before that inventory read. On a later tick reuse empty only when the prior watch was trusted and has not moved, and the original process-local no-worker/no-seed checks still pass. A changed image/table, unknown watch, active worker, seed or failed read takes the original path. This is work-arrival observation, not permission to extend any lease.

Own the lease-loop body and focused fixtures in `media_sessions.rs`; the timing worker owns only declaration values. Acceptance includes original idle-hint and takeover regressions, plus a regression that a nonempty or failed inventory cannot carry an idle watch forward. The periodic tick remains necessary for process-local release reconciliation, settlement deadlines and lease expiry; eliminating it would require a complete local-work arrival feed.

## 6. Remaining empty-work loops

Extend the owned paths to `store/counted_client.rs` (local mark only) and `cluster/membership.rs` offline probe body. Register `offline_source_probes` and its joined `offline_packages` table after all existing canonical slots, preserving their indices (17 existing slots, 19 total, under the 32-slot bound). A private probe pass distinguishes known-empty hint from a pass that attempted filesystem work; only the former waits on a watch. Errors and real work retain PROBE_POLL retries, because filesystem recovery is not a table event. Unknown/untrusted marks retain the original polling.

The sharing claim loop may park only after a successful empty imports inventory or a confirmed disabled switch. Watch settings and sharing_imports before those reads, retain local notification and shutdown, and retain the original scheduled retry deadline path whenever imports exist or reads fail. No pending import can be hidden behind an empty-work park.
