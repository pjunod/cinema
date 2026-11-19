# Cluster timing — one budget table and explicit remaining decisions

**Status:** implemented; final integration qualification remains · **Executes:** C3 and C5 from
[the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) · **Written:** 2026-11-19

Companion to [the facade contract](CLUSTER-FACADE-BUILD.md). This lane starts
from the qualified-promotion candidate `09d4d9502`; it does not modify the
promotion branch. Read the audit's R2, R4 and R6 before changing a timer.
If a change needs a larger serving lease or election floor, record the
relationship and return it to the fleet-measurement owner instead of tuning
it without evidence.

## 1. Contract — retain every duration and authority boundary

Move cluster timing declarations into
[`plurx_core::cluster::timing`](../../crates/plurx-core/src/cluster/timing.rs).
Existing public names remain compatibility re-exports or aliases. Consumers
keep their absolute deadlines, retry counts, clock domains, watch ownership,
proof sampling and refusal behavior. There are no new timers or switches.

The table owns Raft heartbeat/election/recovery, quorum sampling and proof
lease, Store retry envelopes, membership reachability and peer transport,
serving grace, media start/assignment/renewal/claim bounds, job leases and
clock observation bounds. Dynamic configuration remains dynamic. Constants
inside independent cache and receipt protocols keep their own policy owners.

The current 16 s write recovery budget exceeds the normal 12 s media lease.
Centralizing it cannot make that relationship safe. Record it as unresolved;
C4 requires post-C2 voter stall measurements before floor changes. Keep the
existing 24 s takeover claim and its separate runway policy intact.

## 2. Ownership — isolated source and compiler slot

| Files | Change |
|---|---|
| `crates/plurx-core/src/cluster.rs`, new `cluster/timing.rs` | Public timing module and budget relations. |
| Core `cluster/migration.rs`, `membership.rs`, `clock.rs`, `coordination.rs`, `store/hiqlite.rs` | Replace timing declaration values with the shared table; no algorithms. |
| Daemon `serving_fence.rs`, `media_sessions.rs`, `job_lease.rs`, `clock_offset.rs` | Keep compatibility names and replace their timing declaration values. |
| This document and `OPENRAFT-010-EVALUATION.md` | C3 evidence and C5 patch-retirement work item. |
| Cluster index and the P47 backlog row | Same-change document routing; root integrates parallel metadata edits. |

The B4 lane owns watch implementation and sharing watcher callsites. This
lane does not change their fallback constants or the daemon facade. The B6
lane owns quality-catalog code. Build inputs and receipts use an independent
external source-only slot and Cargo target, never the historical shared
Cargo target. Compile jobs are capped at four.

## 3. Delivery — implement, inspect, then run affected checks

1. Establish the pinned 1.97.1 exact-source baseline compiler loop.
2. Move the declarations mechanically, then review every old/new value.
   Preserve the existing public import paths and feature combinations.
3. Derive existing related budgets from the shared values. Add meaningful
   relation checks for retry envelopes and lease runway, not tests that only
   repeat the constants. Document any unresolved relationship explicitly.
4. Inspect current vendored patches 4, 6 and 28 and authoritative OpenRaft
   0.10 interfaces. Record concrete retirement prerequisites and acceptance
   tests. C5 opens the tracked evaluation; it does not silently upgrade Raft.
5. Compile affected core and daemon targets, format, and run the smallest
   affected regression batch once the complete implementation is reviewed.
   Rerun failed or invalidated checks only. Qualification and landing remain
   with the root integration owner.

## 4. Evidence — interpretation and limits

The baseline compiler and all final runs record the exact committed source,
source checks before and after, and a dedicated target. Compilation proves
API consistency, not fleet failover or crash durability. A passing relation
check proves only its named envelope. No C4 measurement or physical
acceptance is claimed by this lane.

## 5. Local implementation — exact values retained

The extraction centralizes 86 declarations and retains all prior import
names. Cross-domain formulas now derive membership reachability and protocol
absence from the heartbeat, job TTL from renewal cadence, media lease/claim
from lease cadence, and Store-related activation deadlines from the shared
Store attempt bound. Existing media envelope/runway formulas move unchanged.

At source `38bcf1409`, the isolated replicated-core compile passed. Four
budget and runway relation tests passed. An independent compiled control
read the 86 original declarations from the exact `09d4d9502` archive and
compared their values with the new module: all 86 matched. This control also
checks its supplied domain safety-window scalar against that archive.

The [C5 evaluation](OPENRAFT-010-EVALUATION.md) pins the actual 0.10.0 release
and corrects the audit's unverified C4 stall threshold. No C4 fleet result is
claimed. Daemon compilation, strict Clippy, final-base checks and landing
remain integration obligations until their receipts are recorded.
