# Cluster facade slice 4 — operations keep their exact authority

**Status:** open — #1646 landed on main (`2218f207`); integrated with slices 5–7c on one task branch, with the C7-sensitive shutdown wiring migrated; review, gate and landing pending.
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
| `crates/plurxd/src/cluster.rs` (inline operations module) | Cloneable Operations capability and typed Clock/Revocation children; private underlying mechanisms, explicit delegates and shared observations. |
| `crates/plurxd/src/http/cluster.rs` | Existing token, protocol and membership mutations, maintenance and ingress roster through the facade; same typed refusals and ordering. |
| `crates/plurxd/src/http/cluster_operations.rs` | Existing claim guards, serialization, cancellation, drain, private observations and cache producers through the capability. |
| `crates/plurxd/src/desktop_control.rs` | Existing identity/topology checks and prepare/commit/cancel ownership. |
| `crates/plurxd/src/main.rs`, `state.rs` | Composition roots and existing lifecycle wiring; C7-dependent shutdown migration follows actual C7 landing. Sharing/custody/job wiring remains with later slices. |
| `crates/plurxd/src/clock_offset.rs`, `http/internal_clock.rs`, associated startup/forwarding fixtures | Existing proof source, startup/observer identity, leadership, guard and authenticated clock roster paths without extra polling. Keep transport construction visible in its existing composition boundary. |
| `crates/plurxd/src/http/internal_auth_revocation.rs` | Existing claim-bound fresh roster, exact exclusion lifetime, activation and apply acknowledgments. |
| `crates/plurxd/src/http/system.rs`, `http/system/tests.rs` | Move the five remaining raw read-pool/writer telemetry sites to plain observation snapshots. Preserve atomics-only reads, fixed labels, histogram bounds and exact Prometheus output; no new task, timer, Store access or allocation for writer rows. This closes the pre-existing actual-5/pin-3 mismatch by migration, without raising its pin. |
| `crates/plurxd/src/cluster/census.rs` | Exact reductions and the existing facade-owner exemption without adding an exempt path. Keep transport and legitimate data sites visible. |
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

## Preparation findings

The first preparatory all-target check passed on pinned Rust 1.97.1. The
capability graph needs compiler trait-resolution depth 256 for the existing
source-worker Send proof; Send remains enforced. The ratchet forbids adding
an exempt path, so Operations is an inline child of the already exempt facade.
No exemption, matcher, or pin ceiling is loosened. The inline module keeps
that existing ratchet boundary. The measured preparatory reduction is 193,
from ceiling 626 to 433; the existing system Hiqlite discrepancy remains
actual 5/pin 3 and is not absorbed by raising its pin. Eight passive lexical
status-call matches now share the existing topology delegate; process/task
ownership is unchanged.

The first focused batch passed 81 of 87 tests. Three remaining source-shape
assertions needed the new facade method names. Three desktop tests were
refused by their unchanged filesystem security checks: the required external
drive's root is group-writable. Preserve the security requirement and run
these tests on a protected filesystem before final acceptance.

## Evidence and remaining acceptance

The strict census and two telemetry-output fixtures now pass all 12 selected
checks. The ceiling is 430, reduced by 196 from the effort's 626. All five raw
system telemetry sites moved behind plain observations, resolving the old
two-site baseline discrepancy without raising its three-site pin. The real
signed learner-clock causal integration case passed (21 seconds). The
ratchet and process inventory each pass seven tests; the two changed formal
source anchors remain unique and their surrounding statements unchanged,
and all three source mappings report zero drift. These are preparatory
source checks and behavioral results, not final integrated qualification.

Preparation only. C7 is separately qualifying on main; no slice-4 review,
regression, gate, source landing or deployment is claimed. The implementation
will record exact removals and actual qualification here and in its task PR.

## Integration with landed C7 (2026-11-18)

Paul asked for bigger pull requests, so slices 4 through 7c are delivered as
one task into `effort/cluster-facade`. That task still gets one whole-diff
adversarial review, focused regressions and one exact-head Effort
development gate. Current main, with C7 at `2218f207` and the formal item
pins from #1676, is merged under the stacked slices.

- C7's own drain wiring already went through the facade
  (`state.cluster.core_failure()`). The merge kept that delegate next to the
  slice's serving-authority delegates.
- The drain's two raw shutdown handles were the last C7-sensitive callers.
  `state.membership.clone()` was used only for the stop-time leadership
  handoff, and `state.serving.clone()` only for the media-start fence. Both
  now go through one `state.cluster.operations()` handle.
  - `Operations::handoff_for_shutdown` delegates to the manager's single
    bounded handoff.
  - The fence uses the existing `prepare_restart_fence`, which calls the
    same `begin_restart_preparation_until`.
  - C2 order is unchanged, and
    `the_signal_path_hands_leadership_off_before_any_listener_stops`
    follows the new spellings. The handoff is still the drain's first
    await, before the leave token, the fence and both listener stops.
- `main.rs` pins move from membership 15 to 14 and from serving 4 to 3, and
  `CEILING` moves from 201 to 199.
- The rolling-producer owner count reconciles to 927: 936 at the base, minus
  10 from the facade slices, plus 1 from #1736.
