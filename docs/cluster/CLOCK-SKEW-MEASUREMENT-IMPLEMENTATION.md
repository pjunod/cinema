# Clock measurement — the separately owned observation release

**Status:** open — owner unclaimed; runtime not started; enablement ruling
pending · **Executes:** K-06 measurement handoff · **Written:** 2026-09-30 ·
**Source baseline:** effort `f319fa779`.

Companion to [the accepted design](CLOCK-SKEW-GUARD-DESIGN.md), which fixes
the protocol and executable fixtures. This plan satisfies its §5 handoff;
it does not claim its future implementation. Read design §§3–6 before
claiming a separate implementation PR on the current effort. The
[enforcement plan](CLOCK-SKEW-ENFORCEMENT-IMPLEMENTATION.md) cannot be claimed
until this release's identified fleet evidence exists. Neither plan closes
the K-06 architecture issue on the [workboard](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md).

## 1. Ownership and the unresolved enablement decision

| Phase | Future owner / session | PR | State |
|---|---|---|---|
| Measurement release | unclaimed | none | blocked on the ruling below |
| Enforcement release | unclaimed; separate owner claim | none | waits on measurement evidence and ruling |

The accepted design §§3.8/4/5.3 requires no enablement switch and describes
measurement-only and enforcing binaries. Paul's current instruction requires
unfinished features to have an explicit Settings → Developer switch whose
readiness never disables the switch, rejects Save or overrides the saved
choice. These conflict for K-06; the human ruling is pending. Do not silently
choose either policy, rewrite the accepted design, or implement production
wiring before that ruling. Record the ruling and its consequences in both
plans before claiming runtime work. This docs-only handoff creates no switch
and authorizes no rollout, clock step or fleet mutation.

## 2. Current entry points — re-verify before writing runtime code

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
