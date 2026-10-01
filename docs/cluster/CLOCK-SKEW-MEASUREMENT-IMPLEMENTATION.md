# Clock measurement — the separately owned observation release

**Status:** open — measurement runtime implemented; release review/gate and fleet evidence pending
· **Executes:** K-06 measurement handoff · **Written:** 2026-09-30 ·
**Source baseline:** effort `f319fa779`.

Companion to [the accepted design](CLOCK-SKEW-GUARD-DESIGN.md), which fixes
the protocol and executable fixtures. This plan satisfies its §5 handoff;
it does not claim its future implementation. Read design §§3–6 before
claiming a separate implementation PR on the current effort. The
[enforcement plan](CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md) cannot be claimed
until this release's identified fleet evidence exists. Neither plan closes
the K-06 architecture issue on the [workboard](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md).

## 1. Ownership and the September 30 enablement clarification

| Phase | Future owner / session | PR | State |
|---|---|---|---|
| Measurement release | gpt-6.1-sol · agent:/root/k06_runtime_sol61 | `codex/k06-measurement-runtime` | runtime implementation; review and exact-source evidence pending |
| Enforcement release | unclaimed; separate owner claim | none | waits on measurement fleet evidence |

Paul clarified on 2026-09-30 that a switch belongs only where manual on/off
has a meaningful purpose; unfinished work alone does not require one. K-06
retains the accepted no-switch design: observation is automatic, enforcement
is a separate reviewed release after measurement evidence, and Developer
facts are read-only and advisory. A manual per-node safety-policy switch has
no useful role in that contract. This ruling authorizes the assigned runtime
implementation, not a rollout, clock step or fleet mutation.

## 2. Current entry points — re-verify before writing runtime code

**2026-10-01 coordinator sequencing ruling:** Paul's newer effort/main-at-end
workflow permits separately owned E0 pure-policy preparation now; the earlier
before-claim wording applies to active enforcement, not this preparation.
Active production refusal consumers still need successful identified
measurement evidence before effort integration. An immutable measurement-only
current-effort artifact in an owned isolated four-node LAN lab can meet that
clock-model safety bar without being a main release or fleet qualification.
Retain the one-hour idle plus 60-second actual network-load protocol, full
identity/coverage/uncertainty/continuity/cost receipt and every original
failed/missing observation. NTP points alone are not success. Bounds and
auth windows are unchanged; no switch, deployment, lab launch or clock step
is authorized. The 24-hour enforcing acceptance and approved drill remain
later qualification, as recorded in the companion design's October 1 ruling.

Inspected at `f319fa779`; symbols are more durable than September 21 line
numbers. No clock route, prober or shared clock-policy runtime exists here.

| Surface | Current symbol and handoff constraint |
|---|---|
| Signed transport | [peer_transport.rs](../../crates/plurxd/src/http/peer_transport.rs): `request_with_optional_header` reads the signed `timestamp_ms`, then `read_bounded`, then awaited response/member verification. Expose that exact signed t1 and capture t4 after bounded bytes, before verification; never take a replacement t1 outside transport. `PeerResponse` currently returns status/body. |
| Responder/router | [http/mod.rs](../../crates/plurxd/src/http/mod.rs) and [internal_activity.rs](../../crates/plurxd/src/http/internal_activity.rs): add the proposed clock route with the existing exact-member proof. t2 precedes awaited authorization; t3 follows it before response serialization. Failures do not produce trusted timing. |
| Auth/roster | [membership.rs](../../crates/plurx-core/src/cluster/membership.rs): `authorize_internal_peer_request`, `sign_internal_peer_response`, 30,000 ms exact/5,000 ms read constants; `operations_peers` retains stale peers, while `cache_admin_revocation_peers` demonstrates fail-closed exact-roster coverage. Reuse bounded committed-membership semantics, not reachable-only placement or heartbeat age. |
| Shared local state | [cluster.rs](../../crates/plurx-core/src/cluster.rs) and [state.rs](../../crates/plurxd/src/state.rs): introduce the design's shared core policy handle; daemon owns HTTP/filtering. Initialize replicated state Incomplete until exact roster proof, standalone NoPeers. |
| Exposition/UI | [system.rs](../../crates/plurxd/src/http/system.rs): `metrics` consumes `MetricsState` passive snapshots without a scrape-time Store read; [developer.rs](../../crates/plurxd/src/http/developer.rs): `readiness` owns existing advisory facts. State plumbing must preserve those boundaries; enablement awaits §1. |

## 3. Milestones — observation without decision consumers

**M0 — exact signed exchange.** Implement the route and specialized timing
metadata under design §§3.1–3.2. Retain nonce/method/path/body/target proof,
bounded response/deadline and unchanged auth windows. Invalid/unsigned
responses, 404, timeout or roster failure yield Unknown. Tests must distinguish
responder service time from network time and verification delay from t4.

**M1 — arithmetic, filter and continuity snapshot.** Implement design
§§3.1–3.3 and the exact §5.2 fixtures, including half-millisecond retention,
checked overflow, 1 ms comparison floor, 25 s time expiry, disjoint-interval
reset and current-round Unknown. Use the fixed wall/monotonic anchor with
250 ms tolerance and serialized clock/state generations; roster changes,
explicit failures and expiry invalidate tickets. Common-mode steps must
invalidate evidence even when relative offsets remain zero. The prober's
generation check brackets t1/t4. Bound fan-out to the committed roster and
existing concurrency budget; never omit an unknown member to manufacture
complete coverage. Measurement has **no refusal call site**, lease/membership
mutation, readiness consequence, recipe or SQL change.

**M2 — passive facts and identified observation.** Implement bounded design
§3.6 metrics and advisory facts after §1 is resolved. Preserve Unknown versus
zero, remove departed-peer series and measure authorization-read cost rather
than claiming the authenticated route never reads Store. Publish the identified
measurement binary through normal review/qualification before an authorized
observation. Execute design §5.5's one-hour idle and 60-second loaded protocol
only within an assigned operational window; do not step host clocks.

## 4. Acceptance and handoff — actual observations unlock enforcement

### 4.1 Implemented runtime seams and focused evidence

The daemon's [clock observer](../../crates/plurxd/src/clock_offset.rs) runs
automatically under background shutdown, at ten seconds with eight concurrent
peers. `MembershipManager::clock_peers` proves a bounded exact committed
roster before and after fanout, retaining stale and pending-removal members.
An incomplete roster invalidates the whole round. Each two-second request
retains the original signed `t1` and captures `t4` before verification.
The [core handle](../../crates/plurx-core/src/cluster/clock.rs) is shared by
`MembershipManager` clones and owns serialized continuity and evidence.
No acquisition, membership mutation or `/readyz` consumer is connected.

Passive metrics drop expired numeric observations and departed peer series.
`plurx_cluster_clock_authority_reads_total` counts actual consistent authority
query attempts reached by inbound clock-route authorization, including failed
queries, rather than all inbound requests or an inferred process-wide delta.
The two consistent roster reads per round are separate from that route cost.
Unknown rounds and local discontinuities have process-lifetime counters.
Developer's clock card has no switch or Save and reads the same snapshot.

Focused commands (pinned Rust 1.97.1; results recorded below when complete):

```bash
cargo test -p plurxd clock_offset::tests
cargo test -p plurxd clock_route_refuses_household_and_forged_proofs_without_timing
cargo test -p plurxd learner_route_matrix_admits_only_bounded_reads_and_node_local_media
cargo test -p plurxd http::peer_transport::tests
cargo test -p plurx-core --features hiqlite-store cluster::clock::tests
cargo test -p plurx-core --features hiqlite-store cache_admin_revocation_roster_includes_pending_removals_and_fails_on_omission
node --test tests/web/settings-sections.test.js
python3 -m unittest tests.operations.test_clock_skew_guard_design tests.operations.test_docs_index
```

The original design's `local_discontinuity_invalidates_generation` fixture
lives in core `cluster::clock::tests`, where the actual serialized handle can
take deterministic wall/monotonic readings; it covers all four named schedules
without stepping a host clock. The arithmetic/filter fixtures retain their
original names in daemon `clock_offset::tests`. This relocates the shared
handle test, not the measurement/enforcement evidence boundary. The existing
roster fixture exercises the exact query and fail-closed directory reused by
clock discovery, including pending removals and missing endpoints.

Local Rust 1.97.1 workspace all-targets check passed. Focused daemon `clock_`
selection passed sixteen tests (including the four arithmetic/filter fixtures,
router refusal and delayed-verification transport test); core clock passed
three with `hiqlite-store`, the reused exact-roster fixture passed one, the
transport suite three, and the learner matrix one. Web settings passed 34
checks; Python design and docs index passed nine. These are development
proofs, not fleet acceptance. Normal hook and sole review are recorded in
the owning pull request against its exact source head.

No measurement binary has been deployed or identified fleet receipt taken.
Runtime review and gate remain pending; one-hour idle,
loaded and subsequent enforcement acceptance remain open.

### 4.2 Release and fleet acceptance

Establish the pinned Rust compile loop before edits, use the named design
§5.2 Rust tests and focused transport/roster/metric contracts, and preserve the
[Python design oracle](../../tests/operations/test_clock_skew_guard_design.py).
Replicated coverage must enable `hiqlite-store`; record canonical regression
fields and current exact-source compile evidence in the owning PR. Follow the
[development pipeline](../DEVELOPMENT_PIPELINE.md); an Effort gate proves
integration readiness, not release or fleet acceptance.

The handoff receipt names source/build/image and each node, uptime/resets/gaps,
clock discipline/host type, offset and uncertainty distributions, upper bounds,
Unknown rounds, healthy-load discontinuities and attributable authority reads.
Use design §5.5/§7 thresholds; missing series or NTP synchronization alone is
not success. If healthy bounds approach/exceed 2,000 ms or continuity/cost
assumptions fail, stop and retain the failure. Do not tune safety constants
from the dashboard. Attach that receipt to this plan and the workboard before
the separate enforcement owner may claim implementation. Runtime and
operational acceptance remain open until actually recorded.

## 5. Execution log

| Date | Model | Session | Milestone | Outcome / evidence |
|---|---|---|---|---|
| 2026-10-01 | gpt-6.1-sol | agent:/root/s14_resume_sol61 | E0 sequencing clarification | `codex/k06-pure-clock-policy` prepares core policy only on current effort. No production consumer or measurement receipt is invented; an identified owned-lab artifact is eligible for the unchanged observation safety bar, not main/fleet qualification. |
| 2026-09-30 | gpt-6.1-sol | agent:/root/k06_runtime_sol61 | M0–M2 measurement runtime | `codex/k06-measurement-runtime`, based on effort `8a7dbf533`; exact signed exchange, core continuity/generations, roster/filter observer, passive metrics and read-only Developer facts implemented. §4.1 records focused development proofs. Sole review, release gate and identified fleet evidence remain open. |
