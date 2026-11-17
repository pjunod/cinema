# Learner discovery — advertise client-serving membership only

**Status:** open — plan committed before implementation. **Owner:** Codex.
**Decision:** Paul, 2026-11-16: “Hide learners from discovery.”

C8 from the [cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md).
Implementation branch: `codex/cluster-learner-discovery`; integration target:
`effort/cluster-facade`. This is discovery policy, not an admission change.

## Contract

Advertise standalone nodes and applied voters. Hide applied learners and
absent members. Use the existing `Cluster::role().standing` projection;
readiness, maintenance, clock admission, leader identity and transient quorum
loss do not decide discovery. Do not change that standing projection's
running/committed predicates in this task.

Both in-process protocols are covered: Bonjour records and Plex GDM search
responses. The host-network `plurxd advertise` companion follows the same
policy. A learner makes no identity requests to the forbidden `/api/v1/server`
route. Promotion becomes discoverable without restart; explicit loss of
eligible membership withdraws advertising. Preserve instance/node identity,
configured labels, published host ports, LAN-bind rules and environment controls. Normal withdrawal waits for the
mDNS daemon's shutdown acknowledgement before a replacement registration.
A bounded, unconfirmed withdrawal is logged and stops that Bonjour owner;
GDM keeps its own standing checks. Cancellation still has a synchronous
best-effort Drop cleanup, and normal drain joins the protocol tasks.

An HTTP timeout, malformed response or 5xx is an unknown observation, not
proof of learner status. The companion retains an existing advertisement
until an explicit ineligible answer or shutdown. At cold start, unknown does
not create an advertisement. A new, bounded public eligibility response is
Store-free, privacy-safe and accessible to learners and maintenance nodes.

## Implementation and ownership

| Files | Responsibility |
|---|---|
| `crates/plurxd/src/cluster.rs` | Owned typed standing watch over the existing local Raft-metrics watch. Clone the watch before initial sampling; emit only standing changes; no Store reads, timers, new tasks or raw handle getter. A closed watch ends notifications without inventing a learner event or spinning. |
| `crates/plurxd/src/discovery.rs`, `main.rs` | Owned discovery lifecycle and companion loop. GDM checks current standing for every response. Bonjour registers/withdraws on standing changes. Shutdown owns cancellation and task settlement. HTTP/HTTPS, Hiqlite/Store listeners, startup admission, C2/C7 and Store close ordering remain with their existing owners. |
| `crates/plurxd/src/http/mod.rs`, `http/system.rs` | Small discovery eligibility route, explicitly allowed through learner/maintenance routing, using only the facade standing. Keep existing server identity semantics. |
| `crates/plurxd/src/startup_tests.rs`, associated HTTP/cluster tests | Real lifecycle/router and synthetic-watch regressions using existing advertiser/GDM seams. Preserve current security and identity tests. |
| `crates/plurxd/src/cluster/census.rs` | Measured reductions only, if any; no new exemptions or raised pins. |
| `docs/API.md`, this plan, cluster index, audit ruling, project backlog, `validation/points.toml` | Route and delivery ledger in the same change. |
| `verification/*/mapping.json`, ownership inventory if measured syntax moves | Review unchanged anchors and actual producer ownership before repinning. Keep all sentinels intact. |

No core or vendor change is needed: `LocalDbRaftMetrics::wait_for_change`
already supplies cancellation-safe change notification. Standalone discovery
has no membership transition source and stays shutdown-interruptible. The
existing companion retry cadence is retained; no separate symptom watchdog
is introduced.

## Delivery and evidence

1. Start in an owned external workspace from verified effort `53d99f6c6`.
   Commit this plan before Rust edits. Establish the pinned 1.97.1 local
   compiler loop, using the existing external compiler/cache slot.
2. Implement the typed standing source and owned discovery state machine;
   preserve the current role predicate and best-effort multicast behavior.
3. Prove learner zero-registration/zero-GDM response, learner-to-voter
   promotion, explicit withdrawal, standalone/voter identity and host port,
   no identity requests while hidden, retention on transient HTTP failure,
   shutdown during every waiting state, and no event for non-standing Raft
   updates. Exercise the actual learner/maintenance router with no Store IO.
4. Integrate the current effort before final qualification, preserving the
   facade and C7 shutdown changes. Recheck the exact intended tree. Run fmt,
   all-target check, Clippy, focused regressions, census/ratchet, ownership,
   formal source mappings and paperwork. Record environment limitations.
5. Obtain one adversarial review of the complete diff, fix findings, dispatch
   the Effort development gate once on the final head, and land only on its
   pass with the exact Regression-Test trailers. Main promotion and fleet
   deployment/acceptance are separate; deployment remains Paul's action.

No source build, review, test pass, gate, merge or fleet acceptance is claimed
by this plan. The slice-4 workspace remains active and separately owned.
