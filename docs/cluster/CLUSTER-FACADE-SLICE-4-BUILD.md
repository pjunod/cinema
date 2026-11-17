# Cluster facade slice 4 — operations keep their exact authority

**Status:** open — preparatory wrappers on the slice-4 task branch; C7-sensitive shutdown wiring and final qualification await #1646 landing.
**Owner:** Codex. **Prepared:** 2026-11-16.

Companion to [the facade contract](CLUSTER-FACADE-BUILD.md),
[the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) and
[slice 3](CLUSTER-FACADE-SLICE-3-BUILD.md). This is the execution plan
for operations, clock proofs and revocation rosters, not a policy redesign.

## Outcome

Administrative feature code asks `cluster.operations()` for typed control
operations. The capability owns a private clone of the existing shared
cluster, with explicit membership, fence and replication delegates. Clock
and revocation get typed child capabilities with their original freshness
and ownership rules. The clock child also supports pre-AppState startup from the existing manager,
without constructing a fence or replication monitor. No raw-handle getter or string-dispatched command API
is introduced. Existing readiness and observation answers remain shared.

C2 and C7 remain the owners of their runtime behavior. Preserve signal/leave
precedence, nonzero dead-core exit, bounded store close and runtime shutdown.
Generic peer calls remain for slice 5; moving `PeerTransport::new` into a
different file is not a peer-call migration.

## Source findings and invariants

The slice-3 effort has ceiling 626. The six named caller files hold 306
pinned spellings: cluster HTTP 46, operation HTTP 109, desktop control 21,
main 42, clock 54 and revocation 34. Even removing every spelling would
leave 320, so the parent's approximate 170 estimate is stale. Some spellings
are legitimate response fields or local authority methods. The associated
internal clock observation context adds eight pins, making the complete
seven-file bound 314 and its theoretical remaining floor 312. Recount exact
removals after dependency integration; never rename local data to hide it.

Mutation preflight refreshes membership, its operations roster and bounded
direct observations. Page caches never authorize mutations. Clock proofs
retain their held proof, applied-index invalidation, exact member/identity/
origin comparison and fail-closed retry. A fresh answer must not restore
two quorum reads on every clock round. Authenticated-request roster checks
remain explicit.

Planned-outage claim ownership exists before polling the write. Preserve
the detached owner, serialized operation guard, cancellation cleanup, exact
release retry and retained local fence. Maintenance keeps claim acquisition,
bounded local expiry, fence preparation, admission settlement and commit in
that order. After an ambiguous commit, exact replicated release precedes
fresh reconciliation; failed reconciliation cannot authorize unfencing.

Desktop commit reasserts the original claim and deadline before fresh
topology, verdict and generation checks. Revocation rosters remain bound
to their exact claim and deadline. Keep exclusion through final roster
validation and peer End, local claim-apply acknowledgments and exact cleanup.
Neither cached operations peers nor generic voter confirmation substitutes
for these authorizers. Handoff precedes cancellation and admission drain.

## Owned implementation scope

| Files | Responsibility |
|---|---|
| `crates/plurxd/src/cluster.rs`, `cluster/operations.rs` | Cloneable Operations capability and typed Clock/Revocation children; private underlying mechanisms, explicit delegates and shared observations. |
| `crates/plurxd/src/http/cluster.rs` | Existing token, protocol and membership mutations, maintenance and ingress roster through the facade; same typed refusals and ordering. |
| `crates/plurxd/src/http/cluster_operations.rs` | Existing claim guards, serialization, cancellation, drain, private observations and cache producers through the capability. |
| `crates/plurxd/src/desktop_control.rs` | Existing identity/topology checks and prepare/commit/cancel ownership. |
| `crates/plurxd/src/main.rs`, `state.rs` | Composition roots and existing lifecycle wiring; C7-dependent shutdown migration follows actual C7 landing. Sharing/custody/job wiring remains with later slices. |
| `crates/plurxd/src/clock_offset.rs`, `http/internal_clock.rs` | Existing proof source, startup/observer identity, leadership, guard and authenticated clock roster paths without extra polling. Keep transport construction visible in its existing composition boundary. |
| `crates/plurxd/src/http/internal_auth_revocation.rs` | Existing claim-bound fresh roster, exact exclusion lifetime, activation and apply acknowledgments. |
| `crates/plurxd/src/cluster/census.rs` | Exact reductions and one narrowly justified internal facade-owner exemption for the new operations module. Keep transport and legitimate data sites visible. |
| `tests/playback/rolling-producer-owners.toml` | Review any conservative syntax-count movement against actual task/process ownership; never change the matcher to make counts pass. |
| `verification/*/mapping.json` | Deliberate source-anchor review and whole-file repins; no semantic acceptance inferred from hashes. |
| This plan, cluster index, facade contract, project backlog and `validation/points.toml` | Source delivery, dependency/base identity and qualification evidence. |

## Dependency integration and qualification

1. Start preparatory work on `codex/cluster-facade-slice-4` from the verified
   effort `53d99f6c6` (tree identical to slice-3 tested head `5fc4cf4b7`).
   The pinned 1.97.1 all-target check and Effort gate 8240 establish the
   working compiler loop. Commit this plan before Rust edits. Prepare
   independent explicit API delegates while C7 qualifies; do not publish
   or merge an incomplete dependent implementation.
2. After #1646 lands, integrate actual current main with the effort before
   final shutdown migration and qualification. Preserve both landed facade
   migrations and C7. Review merge resolution source and recount; the old
   tables are not competing authorities. Do not cherry-pick an unlanded C7
   head or alter its runtime/vendor behavior. If integration is isolated
   as a task, it receives the required effort gate before landing.
3. Finish all bounded callers on the intended integrated base. Keep shared
   directory and original authority semantics; no new cache or runtime task.
   Use scoped knowledge where available; absent tooling means unknown scope.
4. Run fmt, affected all-target compilation, Clippy, focused maintenance/
   outage/desktop/clock/revocation and landed C7 regressions, census/ratchet,
   ownership inventory, formal source checks, catalog and paperwork locally.
   Record actual baseline failures without suppressing or raising pins.
5. Obtain one independent adversarial review of the complete final task
   diff, touched outlines and this contract. Fix findings and verify affected
   behavior. Requalify exact source after any intended-base change.
6. Open the completed task into `effort/cluster-facade`, name concrete
   regressions, dispatch the Effort development gate once after local work,
   and land only after its exact-head pass. Verify tree and regression
   trailers. Main promotion waits for the finished effort and fresh main
   qualification; Paul owns deployment and fleet acceptance.

## Evidence and remaining acceptance

Preparation only. C7 is separately qualifying on main; no slice-4 review,
regression, gate, source landing or deployment is claimed. The implementation
will record exact removals and actual qualification here and in its task PR.
