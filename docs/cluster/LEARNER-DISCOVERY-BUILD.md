# Learner discovery — advertise client-serving membership only

**Status:** built — implemented and independently reviewed. **Owner:** Claude (effort `E-E154B131A2`); implemented by Codex.
**Decision:** Paul, 2026-11-16: “Hide learners from discovery.”

Delivery and qualification evidence is tracked in
[promotion #1811](http://forge.lan:3000/noirr/plurx/pulls/1811). Earlier branch
names and checkpoint receipts below are historical. Fleet acceptance remains
after Paul’s deployment.

C8 from the [cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md).
Implementation branch: `codex/cluster-learner-discovery` (`9b8c85e`), integrated on
`claude/cluster-learner-discovery`; target: `effort/cluster-facade`. This is discovery policy, not an admission change.

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

## Local checkpoint — 2026-11-17

The typed standing watch, owned Bonjour/GDM lifecycle, companion eligibility
loop and exact public eligibility route are implemented. The four tests
affected by acknowledged mDNS withdrawal pass, including promotion, explicit
withdrawal failure, a closed watch and transient HTTP timeout retention. The
real K06 join observes applied Learner then Voter using one watch; it and the
clock timing rejection regression pass. Existing discovery, identity, GDM
and shutdown regressions passed in the earlier focused batch.

The K06 availability probe previously raced the live clock observer by asking
for timing evidence. Its original function reproduced the same failure. The
fixture now verifies the exact signed committed-member response and node ID
without minting timing evidence; production clock verification is unchanged.
A mode-cycle negative control preserves the strict ticket-invalidity assertion
while ordinary join and finalization use their normal advisory policy.

These are local source results on the effort snapshot, not final qualification
or fleet acceptance. Integrate current effort/C7 before final review and
exact-head gate. Reconcile inherited Clippy/census failures on that base
without suppressions, raised pins or duplicate ownership fixes.

## Integration on the finished facade (2026-11-19)

- **Merge.** Merged onto `effort/cluster-facade` at `04bff7f`, after the facade
  slices 2–7 and C7. The drain keeps the facade operations and the C7
  core-failure watch, and adds the standing watch. The joining-learner startup
  test keeps the plain authenticated request (no second clock capture) through
  the facade's test transport. The census stays at zero: discovery reads
  standing only through `Cluster::standing_watch`.
- **Focused regressions.** 146 tests passed: discovery, the three route
  matrices, every `startup_tests` case, the real learner join, the clock
  response security test and the census.
- **One whole-diff review.** It asked for changes. All fixed:
  - With HTTPS enabled, `/api/v1/server/discovery` was redirected, so the
    companion lost every node's advertisement. It is now a machine endpoint,
    and the companion's client follows no redirect.
  - A failed or unconfirmed Bonjour withdrawal dropped the daemon handle. The
    handle is now kept and the withdrawal retried every second. A daemon whose
    thread has exited (`DaemonShutdown`) holds no record.
  - The progress flush and the Bonjour withdrawal now drain concurrently, so
    the stop takes the longer of the two rather than the sum.
  - `visible` has a truth-table test for the handler's answer.
- **Not yet covered.** Nothing tests that `StandingWatch` ignores Raft metric
  updates that change no standing. That needs a replicated store; its loop
  compares the standing before returning.
