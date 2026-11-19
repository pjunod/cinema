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

The source-only formal correspondence checks also pass for durable jobs,
operation progress and worker refinement. Reviewed alias items retain their
values, and explicit timing-module dependencies keep future shared-value
changes inside the drift checks. No model or configuration changed; there
is no new TLC execution claim.

Strict replicated-core all-target Clippy passes after the new test's panic
message was made explicit. The two existing Store recovery-envelope cases
also pass. The default core build passes with its existing default-feature
warnings. Documentation routing, facade-ratchet checks and all three formal
mappings pass. Paperwork's sole new-file mapping failure was corrected and
its catalog check passes.

The daemon all-target check exposed the inherited macOS `dv_runtime`
reference blocker and a test-only handoff-window import removed by this
extraction. The fixture import is restored under `cfg(test)`. The build was
then interrupted when the external drive filled; no completed daemon check
is claimed. Root integration owns the next combined compiler run. Only this
lane's inactive incremental cache was removed to reclaim space; source and
receipts remain preserved.

The exact committed-follower election timer found during C4 source review is
also longer than the current 5 s session grace. The shared table exposes
this relationship; it does not prove outage coverage or change the policy.
Resolve the actual anchored deadline chain from measured failure evidence
before describing the set of timers as mutually safe.

## 6. Completion inventory — proof, revocation and peer observation bounds

Root's acceptance review extends the same mechanical extraction to the
remaining fixed cluster proof, revocation, operation-observation, media-pool
and holder-call bounds. The original 86 declarations were not a claim that
every cluster clock had moved. This completion preserves independently named
protocol policies even when their numbers happen to match.

Add the existing admin proof/revocation TTLs and bounded fallback/admission
waits; the one-second role-specific peer-authority lease and wire freshness
windows; revocation exclusion/apply/fanout/retry bounds; operation status and
transport-observation bounds; media-pool snapshot/offer/root-observation
bounds; and each holder protocol's existing eight-second call deadline.
The named cluster startup health/admission/join call bounds also move.

Root owns active membership, extractor, media-pool and holder files, so this
lane supplies their exact compatibility aliases in a durable integration
manifest instead of editing those active sources. Root applies those aliases
with the combined change. Other timing-declaration replacements remain
mechanical and have an exact-base value control.

`auth::ACTIVITY_REFRESH_SECS` remains the canonical token-activity coalescing
policy. The timing table references it; it does not duplicate a new sixty
second literal or widen the token-proof policy. Dynamic user timeouts,
filesystem lock/storage-deferral machinery, diagnostic logging intervals,
wire-size limits and unrelated local extraction timeouts remain with their
own policy owners. They are not interchangeable authority leases. No election
floor or proof freshness decision changes in this completion.

**Prepared completion:** 48 more independently named fixed declarations join
the original 86; one additional value references the canonical token-activity
policy. The root-owned alias manifest covers 24 extractor, membership,
media-pool and holder callsites and must be applied by the integration owner.
The remaining 24 declarations are mechanically aliased in this lane's
revocation, operation and startup sources.

| Policy | Disposition |
|---|---|
| Admin proofs, remote tombstones, bounded fallback and revocation admission | Shared `timing::proofs`; remote tombstone lifetime retains its existing dependency on the proof TTL. |
| Signed wire freshness and one-second role-specific membership proof cache | Separate named `timing::proofs` policies; clock domains, read anchors and stamp checks remain with their existing authorizers. |
| Exclusion/fanout/apply/cleanup/activation bounds | Shared `timing::revocation`; relation tests cover individual phase bounds, not arbitrary roster-change passes. |
| Aggregate/peer/directory calls and passive operation observations | Shared `timing::operations`; advisory freshness never becomes permission to mutate. |
| Media snapshot/offer/root observations and viewer envelope ceiling | Shared `timing::pool`; existing source-bound B6 invalidation remains unchanged. |
| Fragment index, copy index and subtitle holder calls | Three separately named eight-second `timing::holders` budgets; each keeps its single absolute deadline. |
| Cluster health/admission/join call bounds | Shared `timing::startup`; equal forty-five-second values remain separately named. |
| Token activity refresh | Canonical `auth::ACTIVITY_REFRESH_SECS`; the table references it instead of declaring another sixty-second policy. |
| User-configured snapshot intervals and context-bounded call deadlines | Remain dynamic values; no fixed global replacement. |
| Filesystem lock retries, local snapshot-storage deferral, diagnostic log cadence | Remain startup/storage mechanism policy; they do not validate peer proofs or renew serving authority. |
| General HTTP listener resource bounds and local format/extraction limits | Remain with their server/format owner; this completion does not claim every application timeout has moved. |

Three new relation tests check tombstone coverage and bounded revocation
phases, passive observation refresh/expiry, and separation from canonical
wire/token authentication policy. They add no policy choices or floor tuning.

## Root integration of remaining aliases

All 24 requested proof, membership, catalogue and holder aliases are applied
in the combined tree. Each original expression was checked against the
reviewed alias manifest before replacement; the shared table's exact-base
control retains all 134 fixed values. This is source/value correspondence,
not final combined runtime qualification.
