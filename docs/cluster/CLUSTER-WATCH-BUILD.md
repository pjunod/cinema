# General cluster change feed

**Status:** built — implemented and independently reviewed · **Executes:** B4 from [cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) · **Written:** 2026-11-19

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811). Earlier branch
names and checkpoint receipts below are historical. Fleet acceptance remains
after Paul’s deployment.

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

## 7. D3 disposition at source 2026-11-19

| Former standing poll | Disposition and retained boundary |
|---|---|
| Offline source probes, membership `offline_source_probe_loop` | Known-empty indexed hints park on both joined tables. Unknown hints, real filesystem work and failures retain the 500 ms retry. |
| Quorum watermark sampler | Separately owned C1. Its confirmation/proof renewal cannot be replaced by table silence. |
| Playback-cache ceiling, `AppliedCeilingSource` / `watch_ceiling` | Existing A4 reads `AppliedView::setting`, not unconditional Store quorum reads. Retain one-second proof/floor re-observation: a local settings floor can rise before its write applies, and mark silence alone cannot prove that floor or current serving readiness. The tick also retires the task after its last cache owner drops. |
| Media-session lease inventory | Preserve three-second safety tick for local release/settlement/expiry work. A trusted unchanged media_sessions watch skips repeat empty-inventory hint SQL only after a successful empty pass; active local workers/seeds and unreadable/moved images force the original inventory path. |
| Sharing claim loop | Confirmed disabled or empty imports park on settings/imports changes and local wake/shutdown. Populated imports retain original due-time and bounded retry/network refresh. |
| Sharing listener | Running listener already subscribes to serving/watch changes through A6; disabled/bind/TLS retry loop retains five-second retries and the applied settings view, because host/socket/TLS recovery need not alter a database row. |
| Live TV settings | Existing `settings_observer_loop` parks after proof using SettingsChangeSource; running transports, unproved reads and remote-only processes retain the original fence observation interval. |
| Subtitle transcription settings | Existing idle worker `pass_settings` uses applied view; the source test `the_pass_reads_its_settings_through_the_applied_view` covers it. Explicit user actions and active configuration/workflow reads remain authoritative. Subtitle foreground software-budget reads are job admission, not a standing idle poll. |
| Peer status cache | HTTP observation detects remote process/network recovery which does not change local SQL. Retain its three-second bounded health refresh; peer transport/outcome ownership belongs to B5. |

This table disposes the audit's named D3 standing pollers, not every timer in the repository. Proof renewal, lease expiration, active external work and I/O recovery remain time terms. Fleet idle-read rate acceptance remains a measured integration obligation; source changes alone cannot establish it.

## 8. Peer-proof invalidation registration

The B5 integration appends `cluster_node_activity_keys` after the two work
slots, at index 19. The original 17 slots and work slots 17–18 retain their
identities. The combined registry uses 20 of 32 slots; no table or schema is
created by this registration. Missing or untrusted key observations retain
the peer-proof reader's conservative applied-index/fresh-read fallback.
