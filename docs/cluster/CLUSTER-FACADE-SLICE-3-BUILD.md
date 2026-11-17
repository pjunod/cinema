# Cluster facade slice 3 — role, admission and artwork answers

**Status:** open — slice 3 implemented and independently reviewed; main promotion waits for the remaining facade effort. Track effort qualification in the task PR checks.
**Owner:** Codex. **Prepared:** 2026-11-16.
**Task PR:** [#1747](http://forge.lan:3000/noirr/plurx/pulls/1747) into `effort/cluster-facade`.

Companion to [the facade contract](CLUSTER-FACADE-BUILD.md),
[the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) and
[slice 2](CLUSTER-FACADE-SLICE-2-BUILD.md). This is the bounded execution
plan and delivery ledger for slice 3. Paul owns deployment and fleet acceptance.

## Outcome

HTTP admission, remote Activity, system and Developer projections, diagnostics,
artwork and hostname readers ask the shared cluster facade. Preserve each
existing answer's freshness, refusal, timeout, ordering and standalone behavior.
The generic peer transport and its wire authentication remain for slice 5.

## Source findings and invariants

The nine named caller files contain 145 pinned census sites on the slice-2
effort base, whose ceiling is 744. The parent plan's approximate 340 target
is stale: even removing all 145 sites would leave 599. Lower exact per-file
pins only for real removals; transport and protocol dependencies remain visible.
Do not rename local state or relax the census to reach an estimate.

`local_serving_role()` reads a heartbeat-published atomic despite its async
signature. It is distinct from `Cluster::role()`, which combines applied
standing with maintenance and fence projections. Preserve the original role
answer and standalone result through an explicit admission projection.
Fresh committed-voter, committed-member and active-voter confirmations retain
their original methods. Active voter includes a consistent removal check and
must never be replaced by a cached directory result.

Artwork tick observation short-circuits active voter, then committed voter,
then committed member. Preserve that sequence, errors and reconciliation mode.
Artwork recovery peers include reachable non-removed nodes with HTTP bases,
including learners, without media readiness or committed-voter filtering.
Keep the existing descending last-seen and ascending Raft-id order and do not
introduce the Activity roster's limit. Add `Purpose::Artwork` over the shared
roster with that rule; retain explicit active-voter confirmation for authority.

Artwork signing, peer verification and repair claim/retirement remain the
existing protocol and consistent operations behind a typed facade domain.
System and diagnostic replication facts retain the canonical monitor source.
Hostname answers retain their existing query and error behavior.

## Owned implementation scope

| Files | Responsibility |
|---|---|
| `crates/plurxd/src/cluster.rs`, `state.rs` | Explicit role/admission and fresh-confirmation answers, typed artwork and status projections; supply existing monitor at composition roots. |
| `crates/plurx-core/src/cluster/membership/directory.rs` and its tests | Artwork roster predicate and ordering over the existing shared snapshot; no new cache. |
| `crates/plurxd/src/http/mod.rs`, `remote.rs`, `activity_starts.rs`, `internal_activity.rs` | Route existing admission, role and voter-roster readers through the facade; retain peer transport dependencies honestly. |
| `crates/plurxd/src/http/system.rs`, `developer.rs`, `crates/plurxd/src/diagnostics.rs` | Existing system, Developer, hostname and diagnostic answers through the facade; preserve settings and admission policy. |
| `crates/plurxd/src/http/images.rs`, `images_worker.rs` | Artwork roster and authority observation, existing authentication and repair operations through the facade. |
| `tests/playback/rolling-producer-owners.toml` | Account for two Developer status readers sharing one facade delegate; no process owner or matcher changes. |
| `crates/plurxd/src/cluster/census.rs` | Exact reductions only; keep transport and legitimate local state pins. |
| `verification/*/mapping.json` | Review affected source anchors and repin unchanged statements; no semantic acceptance inferred from hashes. |
| This plan, cluster index, facade contract, project backlog and `validation/points.toml` | Ownership, scope, delivery and qualification evidence in the same change. |

Later operations, generic transport, sharing/custody, applied-mark widening
and fleet qualification remain separate slices or owners. C7 stays on its own
main train; this task does not depend on its unlanded source.

## Build and qualification

1. Base `codex/cluster-facade-slice-3` on current effort `d19e229d7`. Commit
   this plan, index, backlog and catalog mapping before Rust edits. Reuse the
   owned external workspace, pinned Rust 1.97.1 and existing compiler slot.
2. Implement the complete bounded slice with no admission policy change.
   Preserve local-only constructors and original typed failures. Query scoped
   knowledge where tooling exists; absent pilot coverage means unknown.
3. Run formatting, affected all-target compilation and Clippy locally. Record
   baseline lint failures accurately; do not suppress them or claim success.
   Run focused role/admission, artwork roster/authority and affected Activity
   tests, the census/ratchet, catalog, paperwork and formal source checks.
4. Obtain one independent adversarial review of the complete diff, touched
   outlines and this contract. Fix findings and verify the affected behavior.
5. Open the task PR into `effort/cluster-facade`, name concrete regressions,
   dispatch the Effort development gate after local work, and land only on a
   passing exact-head gate. Verify landing tree and regression trailers.
6. Record actual evidence here and in the task PR. No main promotion until
   the remaining effort slices and fresh main qualification are complete.

## Evidence and remaining acceptance

The local all-target daemon check passed on pinned Rust 1.97.1. The focused
daemon batch passed 92 tests; all seven replicated-store directory tests
passed, including the new artwork SQL differential and uncapped ordering case.
The finalized census has nine passing checks and only the known system
Hiqlite discrepancy (actual 5, pin 3). Its exact ceiling is 626, down by 118
from 744. The census ratchet and the process ownership inventory each pass
seven tests. The latter lowers its broad lexical status-call count by one
because two Developer readers share one delegate; no process owner changes.

One independent adversarial whole-diff review approved the implementation
with no actionable findings. The same reviewer verified the narrow ownership
inventory correction afterward; this was not another whole-code review.
All three formal source mappings pass after reviewing seven unique unchanged
anchor regions. These are source checks, not new model runs.

Clippy with `-D warnings` remains red on the existing unused import/items:
`claim_skipped`, `demand_renewed`, trial-admission `claim`/`record_needed`,
`TrialClaim::mark_admitted`, and `subscribe_playback_qualification_waiters`.
No warning was suppressed and no census pin was raised to absorb the two
writer metrics added after slice 1. The effort gate and main promotion remain
separate qualification steps. This source work deploys nothing.
