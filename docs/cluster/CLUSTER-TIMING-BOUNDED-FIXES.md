# Bounded timing corrections — one activation lease and renewal priority

**Status:** implemented in source; combined validation pending · **Executes:** C3 relationship decisions in
[the timing ledger](CLUSTER-TIMING-BUILD.md) · **Written:** 2026-11-19

Build from integration `be88b1b4dc531808559fc628aa537e66760234d2` in an
owned external worktree. Root owns the working pinned source-only compiler
slot and the final combined test batch; do not create another heavy build.
These fixes change neither timer values nor admission/proof policy.

## 1. Activation — consume elapsed time once

The current helper adds the original `lease_expires_at_ms - activation.now_ms`
to a fresh monotonic clock. Delayed delivery or another invocation can mint
the full original interval again. Existing route/epoch/absolute-expiry checks
prevent expired publication, but the owner may retain resources too long.

Translate the remaining absolute lease using the same local clock basis as
`route_matches_activation`, only after validated ingress, capped by the
existing lease ceiling. Capture a monotonic deadline once per activation
owner and pass it through preparation, confirmation, recovery, the remote
call and its outer wait. A remote target captures its own remaining deadline
after exact request validation; it does not trust a foreign monotonic clock
or use the foreign `activation.now_ms` as current time. Preserve the
confirmation recovery reservation and exact positive route predicate.

## 2. Lease tick — renewable work precedes optional reconciliation

Both Store backends already exclude staged/unconfirmed starting routes from
owner inventory. Renewal independently repeats those predicates. Do not
change eligibility or renew an unconfirmed worker.

Run the existing renewal and ambiguous/lost-owner fencing before optional
publication reconciliation. Do not reconcile a newly fenced identity. The tick's one
existing four-second absolute deadline includes inventory, renewal and the
optional reconciliation phase under the same fanout; no route gets a fresh
interval. Optional work receives only the budget the critical work leaves,
so it cannot spend another four seconds after renewal. An expired optional budget performs no new
Store work. A late result still needs the exact owner/epoch and active,
unexpired route checks before cache publication.

## 3. Owned files — focused changes and meaningful regressions

| Files | Change |
|---|---|
| `http/hls/create.rs` and `http/internal_media_sessions.rs` | Capture/pass remaining activation deadline; preserve all exact publication and ownership guards. |
| `http/hls/tests/chunk_01.rs` | Direct delayed-entry and re-entry deadline regressions; retain confirmation reservation control. |
| `media_sessions.rs` | Renewal priority, newly fenced identity filtering and one bounded optional reconciliation phase. |
| `media_sessions/lease_inventory_tests.rs` | More than 16 stalled reconciles must share a global bound and leave unrelated renewals unstarved; retain unconfirmed/stale-owner negative cases. |
| Timing module header and timing/evaluation documents | Correct the false 16 s/12 s renewal contradiction; keep C4 availability acceptance open. |

Run only source/static checks here. Root compiles the exact combined tree and
runs the focused new cases plus original confirmation, exact-epoch and
lease-eligibility cases. Do not claim those executions before their receipts.

## 4. Acceptance — preserve safety and keep the larger decision open

Delayed entry consumes the original lease; re-entry cannot restart its
monotonic deadline. Confirmation never changes expiry. A stalled route list
cannot multiply the optional phase timeout, mint eligibility or revive a
fenced owner. No new timer task or feature switch appears.

The five-second grace is still bounded short-gap survival, not a proved
cold-failover promise. C4's measured recovery/renewal chain governs any wider
availability profile; these corrections do not tune an election floor,
serving proof, media lease or grace period.

## 5. Source delivery — compiler and runtime evidence remain with root

The source adds direct delayed-entry/re-entry controls and a paused-time
fixture with nonzero inventory/renewal time followed by more than one fanout
of stalled optional routes. A separate expired-budget case proves no new
Store operation is constructed after the cycle. The production wiring guard
pins renewal/fencing before optional work and unchanged Store eligibility.
Root runs the exact combined daemon build and these cases with the original
confirmation, late-ACK, exact-owner and unconfirmed-route controls. No new
compile or runtime pass is claimed by this source delivery alone.
