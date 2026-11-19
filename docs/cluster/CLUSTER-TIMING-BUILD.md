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

The 16 s Store retry envelope and 12 s media lease have different owners.
The source-backed decision in §7 shows that ordinary renewal does not use
that retry envelope, and confirmation cannot extend the lease. C4 still
requires post-C2 voter stall measurements before floor changes. Keep the
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

## 7. Budget decisions — safety, cleanup and availability are different claims

**Verified source:** integration `be88b1b4dc531808559fc628aa537e66760234d2`,
2026-11-19. These are source decisions and existing-test references, not new
runtime qualification. The root owns combined compilation and any fixes.

### 7.1 The sixteen-second Store envelope does not renew a twelve-second lease

**Decision:** retain the distinct budgets; do not increase a media lease just
because its duration is shorter than the generic exact-state retry envelope.
The previous numerical comparison alone was not evidence of a renewal bug.

[`renew_with_authority`](../../crates/plurx-core/src/store/hiqlite_sessions.rs)
uses a transaction through `session_client().txn`, not `execute_idempotent`.
The Store attempt is bounded by 3 s. The daemon renewal batch owns one shared
4 s absolute caller deadline; at dispatch it re-samples wall time, refuses
routes with at most 4 s left, and proposes the fixed expiry from that dispatch
sample plus 12 s. It does not calculate a new expiry from acknowledgement
time. An error or timeout is commit-unknown: the exact worker is authority
fenced even if the durable mutation later finishes. Cleanup cannot fence a
successor because it checks the incarnation, public session id and owner
epoch. These paths are in
[`lease_loop` and `fence_and_reap_sessions`](../../crates/plurxd/src/media_sessions.rs)
and [the exact-owner fence](../../crates/plurxd/src/transcode/manager/control.rs).

The longer retry envelope belongs to activation **confirmation**. The
`Confirm` arm of `settle_media_session_activation` updates only
`publication_ready_at_ms` and `updated_at_ms`. It matches the exact route,
owner epoch, fixed lease, unpublished sentinel and current playback pointer;
it never writes `lease_expires_at_ms`. A lost response can therefore be
recovered without extending the old lease or stealing a successor epoch.
Ordinary transactions and abandonment do not acquire this retry authority.

The remote activation endpoint's 3 s wait is a response bound, not the
activation task lifetime: on timeout it retains the exact task and returns
202. The owner applies a monotonic lease deadline, reserves at most 3 s or
half the remaining interval for the confirmation recovery read, and accepts
a positive route only while its exact identity is active and its absolute
lease expiry is still in the future. A late result after that expiry is not
new serving authority. See [the activation owner and route predicate](../../crates/plurxd/src/http/hls/create.rs)
and [the target endpoint](../../crates/plurxd/src/http/internal_media_sessions.rs).

Existing focused coverage:

- `store_contract::activation_confirmation_retries_only_its_exact_state_update`
  pins the retry's exact update and excludes transactions/abandonment.
- `store_contract::media_activation_confirmation_recovers_a_committed_timeout`
  injects an error after a real committed confirmation; it requires the
  surviving exact activation to be recovered. It needs
  `cluster-read-cost-validation`, not a bare core unit run.
- `store_contract::hiqlite_stale_activation_transaction_cannot_revoke_a_renewed_lease`
  orders a stale activation after another client's renewal and checks the
  renewed authority survives. It needs `hiqlite-contract-tests`.
- `http::hls::tests::the_confirmation_reserves_recovery_budget_inside_the_owner_lease`
  pins the bounded confirmation/recovery split.
- `media_sessions::tests::takeover_claim_reconciliation_distinguishes_only_exact_authority`
  refuses an observed winning claim once its immutable lease has expired.

These tests are named coverage, not a claim they were rerun during this
source investigation. The playback-control actor's monotonic client lease
and delayed-reply tests are another owner; they do not prove the replicated
media-owner lease's timing.

### 7.2 Five-second grace is bounded fail-closed behavior, not full failover continuity

**Decision:** preserve the serving proof and generation rules. The existing
5 s `SessionGrace` keeps already-running resources through a short unknown
span while admissions and publication remain fenced. Repeated losses spend
one cumulative outage budget; expiry tears work down instead of refreshing
the grace. This is safety and bounded cleanup. It is not proof that all
unplanned leader failures preserve playback.

Source is [the grace owner](../../crates/plurxd/src/serving_fence.rs) and
[the registry outage loop](../../crates/plurxd/src/transcode/manager/control.rs).
The tests `an_unknown_span_holds_for_the_grace_and_ends_on_a_definite_answer`,
`flapping_unknown_spans_share_one_outage_budget` and
`the_published_outage_deadline_is_the_enforced_one` cover the bounded policy.
`serving_fence_preserves_real_media_through_2468ms_and_reaps_after_grace`
qualifies that particular real-media outage and deliberate later teardown;
it does not qualify every election.

The [C2 receipt](LEADER-SHUTDOWN-HANDOFF-BUILD.md#7-execution-log) records
cold-kill recovery at 7.50–7.60 s in three harness runs, and much shorter
successful planned handoffs. The [exact 0.9.25 timer rules](OPENRAFT-010-EVALUATION.md#3-c4-source-result--a-failed-campaign-can-still-disturb-the-leader)
are state-dependent and do not themselves bound the full outage. Neither
source justifies calling 5 s an uninterrupted cold-failover guarantee.
C2 preserves the accepted planned-restart goal; the broader R2/C3
availability relationship remains open with C4. This is not a reduction of
that acceptance requirement.

### 7.3 Two bounded corrections are proposed before changing any timer values

**Activation owner deadline:** `activation_lease_deadline` currently adds
`lease_expires_at_ms - activation.now_ms` to a fresh monotonic `now`. A
request delivered late, or a second call to that helper, can recreate the
original full lease as a resource/commit-guard lifetime. The positive route
predicate still rejects absolute expiry, so this is not evidence of revived
serving authority. Correct it by translating the actual remaining absolute
lease once per owner and retaining that monotonic deadline through prepare,
confirmation and recovery. A delayed-entry and repeated-entry regression
must demonstrate that elapsed time is consumed instead of minted again.

**Renewal priority and reconciliation bound:** the lease loop awaits
`reconcile_owned_publication_fences` before renewals. That function gives
each candidate a fresh 4 s deadline under fanout 16; its total wait depends
on the number of routes. The old explanatory claim that the last renewal
is always at most one 3 s tick plus one 4 s deadline old is consequently not
a proved global bound. Give the phase one absolute deadline or prioritize
renewal, preserving exact publication/epoch semantics. A fixture with more
than one fanout of stalled routes must prove that the phase cannot multiply
its deadline and starve unrelated renewable owners.

Neither correction widens a serving proof, lease, grace or election floor.
The root must review the bounded fixes before implementation. Once their
owner chains are bounded, use C4's actual post-C2 recovery/stall evidence to
choose any wider availability profile and validate the complete renewal
runway. C3's full relationship acceptance must not be marked complete from
constant extraction or these source decisions alone.

## 8. Bounded corrections — one captured lease and one tick budget

[The execution plan](CLUSTER-TIMING-BOUNDED-FIXES.md) is implemented in source,
awaiting root's exact combined compilation and focused regressions. The
activation owner now translates actual remaining absolute expiry using the
route predicate's local clock basis and receives one monotonic deadline
through all local/remote waits. The existing positive route/owner-epoch and
confirmation recovery guards remain intact.

The lease tick captures its existing 4 s deadline immediately after its tick.
Inventory, renewal, cause-read/drain attempts and optional publication work
consume that same budget. Confirmed eligible renewals and ambiguous-owner
fencing precede optional work; newly fenced identities are removed from its
candidates. One fanout can spend only the remaining budget; another batch
cannot start new Store work after expiry. This bounds awaited Store phases,
not operating-system scheduling or arbitrary lock contention. The 3 s tick,
Store attempt, 12 s lease and 5 s grace values are unchanged.

The 24 integration aliases described in §6 were already applied in `be88`;
this correction branch starts from that integrated source. The broader
availability/grace decision stays open with measured C4 acceptance.
