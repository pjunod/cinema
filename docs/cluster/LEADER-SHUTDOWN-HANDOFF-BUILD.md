# Leader handoff on shutdown — a leader never restarts cold

**Status:** built — fleet verification pending · **Executes:** C2 of the cluster layer
audit (`CLUSTER-LAYER-AUDIT-2026-11-14.md` §R3, ruling §8-3) · **Design:**
Claude (Fable 5.2), session `session_01BCWLA3JD1HCNMEid3AZrc3` ·
**Verified against:** `main @ e228ab3ed` · **Written:** 2026-11-15

> **Who reads this.** The build agent. Work §5 in order, one commit per
> milestone, and record each in §7. If a claim in §2 no longer holds, stop
> and say so in §7 instead of building around it.

## 1. Objective

A plurxd that is told to stop while it is the Raft leader keeps leading
through its whole drain and then stops its Raft node cold. The other nodes
then wait out a timeout-driven election, and every node's serving fence
(1 s lease, 5 s session grace) lapses.

After this change a stopping leader moves leadership to a caught-up voter
as the first act of its drain, while its Raft node and listeners are fully
up, and a stopping node never campaigns again. The deploy playbooks are
outside the repository; this stands without them.

Acceptance: on three voters, SIGTERM to the leader's plurxd leaves the
survivors with a surviving leader while the old process is still running,
and that leader does not change when the old process exits.

## 2. What happens today

Re-verify each at build time; line numbers are for `e228ab3ed`.

| Step | Where | Fact |
|---|---|---|
| Signal | `plurxd/src/main.rs:5059` `shutdown_signal`, `:1910` `ShutdownWatch` | First SIGTERM/SIGINT resolves one future. The streams are consumed; a second signal does nothing. |
| Drain | `main.rs:2608-2640`, the `shutdown` future in `boot` | Cancel `state.shutdown`, block media starts 10 s, drain telemetry, stop Live TV, then cancel the listener tokens. Listeners accept until this future completes. |
| HTTP | `main.rs:4367`, `:4369` | 5 s connection drain, 3 s progress flush. |
| Store | `main.rs:2028-2040` → `plurx-core/src/cluster/migration.rs:380` `SelectedStore::shutdown` → `vendor/hiqlite/src/client/mgmt.rs:817` | Bounded 30 s (`mgmt.rs:16`). Sleeps 9.5 s when not single-node, waits up to 5 s for a leader, then `raft.shutdown()`. No handoff: its own TODO defers one to OpenRaft 0.10. |
| Grace | `deploy/docker-compose.yml:114` | `stop_grace_period` defaults to 65 m. |
| Handoff | `plurx-core/src/cluster/membership.rs:8435` `handoff_leadership` | Private. Callers: `enter_maintenance` `:8225`, `exit_maintenance` `:8310`. Nothing in `crates/plurxd/src` calls it. |

What `handoff_leadership` does: reads local Raft metrics and the status
passed in; picks the first voter that is reachable, not in maintenance,
not pending removal, with `apply_lag_entries == Some(0)`; POSTs
`/cluster/elect/sqlite` to it (`trigger_election` `:13399`, 1 s budget);
then `await_survivor_leader` (`:12385`) polls the survivors' metrics every
250 ms until a majority of them name the same non-departed leader three
rounds running, or `SURVIVOR_LEADER_WAIT` (8 s, `:134`) passes. It returns
the successor's Raft id. It writes nothing and does not require
maintenance. `status()` (`:9880`) is local reads only.

The election trigger is fork patch 4 (`vendor/hiqlite/PLURX-PATCH.md`,
`src/helpers.rs:186`): OpenRaft 0.9.25 `trigger().elect()`. It is not a
transfer. Read from the OpenRaft source (registry copy, not vendored):

- A voter refuses a vote while its leader lease (4.0 s, refreshed by each
  heartbeat) is live (`engine_impl.rs:305-318`). The leader's own vote
  time is not refreshed, so the leader grants, if the candidate's log is
  not behind its own (`:322`). Candidate plus old leader is the quorum.
- If the leader refuses (candidate one entry behind, or leader under 4 s
  old), the candidate's higher term still deposes it
  (`raft_core.rs:1262-1277`). The old leader holds the longest log and the
  shortest timer, so it is likely to win leadership back, and then stops
  cold.
- Followers of a committed leader wait lease plus election timeout
  (`raft_core.rs:1475`).

## 3. Design

One mechanism, one call site.

1. **`MembershipManager::handoff_for_shutdown(&self) -> ShutdownHandoff`**
   in `membership.rs`, beside `handoff_leadership`. In order:
   - No replicated store: `NotReplicated`. No await.
   - Disable this node's election timer (step 2). Every replicated node
     does this, leader or not.
   - Local metrics: `current_leader` is not this node: `NotLeader`. One
     voter: `SoleVoter`. Neither touches the network.
   - Otherwise `status()` then the existing `handoff_leadership(self,
     &status)`, unchanged, under one `tokio::time::timeout` of
     `SHUTDOWN_HANDOFF_WAIT = SURVIVOR_LEADER_WAIT + 2 s` (the trigger's
     1 s plus one probe round's 1 s). Outcomes: `HandedOff { successor }`,
     `NoCandidate`, `TimedOut`, `Failed`.
   - One log line, target `plurx::cluster`: outcome, successor, elapsed
     ms. Info for the first four outcomes, warn for the last three.
   It never returns an error: the caller always continues its drain.
2. **Fork patch 30: `Client::disable_local_db_elections()`**, local-only
   like patch 5, calling `raft.runtime_config().elect(false)`. Process
   state, not persisted; the next start campaigns normally. This is what
   makes a lost handoff cost one election, as a cold stop does today,
   instead of two. Add the ledger row and bullet.
3. **Call site:** the first await in `boot`'s `shutdown` future
   (`main.rs:2608`), after the `select!` and before
   `leave_shutdown.cancel()`. Clone `state.membership` before
   `http::router(state)` takes the state. Placing it first means the
   drain's own writes go to the successor instead of racing a leadership
   change, and Hiqlite's 9.5 s sleep is spent as a follower.

No metric: no leader, election or handoff counter family exists in
`crates/` to extend.

Worst case added to a stop is 10 s; typical is under 1 s on the leader
and nothing elsewhere. Bounded phases then total about 50 s against the
65 m default.

## 4. Decided edge cases

| Case | Behaviour | Why |
|---|---|---|
| Follower or learner | `NotLeader`, no network, no wait | Nothing to move. |
| Single voter | `SoleVoter`, no wait | No successor exists; the outage is the documented one. |
| No caught-up reachable voter | `NoCandidate` at once, drain continues | Waiting cannot produce one; elections stay disabled here. |
| Leadership does not move | `TimedOut` at 10 s, drain continues | One bounded wait; the survivors then elect as today. |
| Second signal during the wait | No effect, as everywhere in the drain today | The wait is bounded, and the unskippable 9.5 s sleep follows. An immediate exit would skip the WAL and writer drain. |
| Requests after a successful handoff | Served as any follower serves them | Writes forward to the successor; media starts are already blocked. |
| Maintenance already entered | `NotLeader` | Maintenance handed off before its row committed. |
| Restart preparation already ran | Usually `NotLeader`; if leadership moved here since, handed off | The verdict is a point-in-time proof. |
| Signal before `boot` (activation, probing) | Not covered | No listener served. |
| Committed leave | `NotLeader` | The leave path already handed off. |

`plurxd cluster prepare-restart` and the rollout verdict
(`http/cluster_operations.rs:2358-2379`) do not change. The verdict names
one drained non-leader so every observer picks the same node; teaching it
to hand off would be a second mechanism. What changes is the consequence
of stopping a leader without it: leadership now moves first. Draining a
leader's sessions is still maintenance's job.

## 5. Milestones

Each test must be shown failing with its subject removed.

**M1 — the core call and patch 30.**
- `plurx-core/src/cluster/membership/tests.rs::`
  `shutdown_handoff_without_a_replicated_store_returns_at_once`,
  `shutdown_handoff_wait_is_bounded` (paused clock, a pending attempt,
  `TimedOut` at exactly the bound),
  `shutdown_handoff_reuses_the_one_handoff_and_writes_nothing` (source
  shape: calls `handoff_leadership`; no `execute`, `txn` or
  `query_consistent`).
- `plurx-core/src/cluster/migration/membership_api.rs::`
  `a_sole_voter_shutdown_handoff_returns_at_once` (real voter).
- `vendor/hiqlite/src/client/mgmt.rs::`
  `local_election_switch_has_no_remote_fallback`.
- Check: `cargo test --locked -p plurx-core --lib shutdown_handoff` and
  `make hiqlite-vendor-clippy`.

**M2 — three processes.** New harness requests `ShutdownHandoff` and a
`leader-shutdown` subcommand, added to `controller()`, in
`plurx-cluster-check/src/lib.rs`:
- `run_leader_shutdown_handoff_case`: leader older than 4 s, heartbeat
  loops on. Sample both survivors' `Metrics` every 100 ms. After the
  request returns `HandedOff`: survivors name the successor, and the
  longest sampled leaderless gap is under 2.4 s (so no timeout election
  ran; record the number against the 1 s lease in §7). Kill the old
  leader; for 10 s leader and term do not change.
- `run_follower_shutdown_noop_case`: `NotLeader`, under 100 ms, leader and
  term unchanged.
- `run_shutdown_without_successor_case`: both followers paused; returns
  `Failed` or `TimedOut` within 11 s and the process exits orderly.
- `run_departing_voter_never_campaigns_case`: after the request on a
  follower, kill the leader and pause the third voter; this node's term
  does not move for 10 s.
- Extend `tests/harness.rs::`
  `a_one_voter_cluster_proves_the_whole_request_protocol` (`SoleVoter`),
  the encoding test and the argument-rejection test.
- Check: `cargo run --locked -p plurx-cluster-check -- leader-shutdown`.

**M3 — the daemon.**
- `plurxd/src/startup_tests.rs::`
  `the_signal_path_hands_leadership_off_before_any_listener_stops`
  (source shape: one call in `main.rs`, before `leave_shutdown.cancel()`
  and `public_listener_stop.cancel()`).
- `plurxd/tests/cluster_activity.rs::`
  `sigterm_on_the_leader_moves_leadership_before_its_raft_stops`
  (`ThreeVoters`; within 3 s of SIGTERM both survivors name a surviving
  leader while the old process is still running; no sample without a
  leader until 10 s after it exits) and
  `sigterm_on_a_follower_changes_no_leader`. The fixture's `Daemon` needs
  a SIGTERM method.
- Check: `cargo test --locked -p plurxd --features
  cluster-integration-tests --test cluster_activity sigterm_on --
  --test-threads=1`.

**M4 — paperwork.** Index row in `docs/cluster/README.md`.
`docs/OPERATIONS.md`, "Planned node maintenance": reword in place to say
a plain stop of the leader hands leadership off and that maintenance is
still what drains sessions; the file must not grow (292 bytes under its
ceiling; lower the ceiling if it shrinks). Mark C2 in the audit. No
`validation/points.toml` change: every touched path is already mapped
(`cluster.membership`, `crates/plurx-cluster-check/**`, `vendor/**`).
Then `cargo fmt --check`, clippy on the three crates with `-D warnings`,
`scripts/paperwork`, and one adversarial review of the whole diff.

## 6. Non-goals

- **No change to Hiqlite's `shutdown_execute`**, its 9.5 s sleep or its
  timeouts. C3 owns timing.
- **No change to `handoff_leadership`, its candidate rule or its
  callers.** A leader-side replication match would pick a better
  candidate; that is a follow-up, after M2 gives a number.
- **No membership or settings write, no maintenance row, no lease.**
- **No flag, setting, retry loop, watchdog or second timer.** One bounded
  wait.
- **No election-floor change.** Ruling §8-3 waits on this landing.
- **No playbook work.** Whether the deploy path should also use
  maintenance is decided when the playbooks are read.

## 7. Execution log

| Date | Milestone | Agent-Model | Agent-Session | Result |
|---|---|---|---|---|
| 2026-11-15 | Design | claude-fable-5-2 | session_01BCWLA3JD1HCNMEid3AZrc3 | this document |
| 2026-11-15 | M1 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Built as planned, except the patch is **29**, not 30: the ledger ends at 28 and its test requires contiguous rows. Four core tests and the vendor test pass; `make hiqlite-vendor-clippy` passes. Each test fails with its subject mutated (see "Mutations"). |
| 2026-11-15 | M2 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Four cases built; `leader-shutdown` passed 5 of 5 runs on media1. Handoff 763–771 ms, term +1 and the triggered candidate won every time; survivors moved from old leader to successor within one 100 ms sample (no sample without a leader); successor and term held 10 s after the kill. Cold-stop baseline (leader killed, no handoff): survivors named a new leader after 7.50, 7.60 and 7.50 s. |
| 2026-11-15 | M3 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Built as planned; `the_signal_path_…` passes. **The leader test is not reliable: the handoff lost the vote race in 5 of 14 runs.** In the 9 won runs both survivors named the triggered candidate 122–131 ms after SIGTERM, the old process exited 0.92–0.94 s after it, and the leader and term then held for 10 s. In each lost run the candidate was behind an append in flight from the old leader, so the old leader refused its vote and was deposed, as §2's second bullet warns. With its elections off the old leader never won back, but no leader existed for about one election. The 3 lost runs under a 40 s diagnostic bound: two recovered in 7.80 and 7.84 s, which matches the 7.5 s cold-stop baseline. In the third the handoff gave up (`failed`, 8127 ms), and there was still no leader when the old process exited 8.2 s after SIGTERM. The other 2 lost runs failed at the committed 3 s bound. M2's quieter harness never lost (0 of 5). No retry was added (§6); the root-cause follow-up is a transfer that does not trigger while the candidate is behind the leader's last log entry (OpenRaft 0.10's `transfer_leader`, or the leader-side replication match §6 defers). The follower test passed 1 of 1 runs. | |
| 2026-11-15 | M4 | claude-opus-5-5 | session_01BCWLA3JD1HCNMEid3AZrc3 | Index row, `docs/OPERATIONS.md` reworded in place (418524 → 418519 bytes, ceiling lowered to match), audit C2 marked built in this PR. Paperwork and the adversarial review are left to the main session. |

### Source claims, as found

- **§2 Store row — false for the daemon.** `SelectedStore::shutdown` calls
  `shutdown_voter`, which takes Hiqlite's `shutdown_retained_startup`
  whenever staged-startup listeners are retained (patch 23). That path has
  no 9.5 s sleep, no wait for a leader and no 30 s bound. Every M3 run's old
  process exited 0.92–0.94 s after SIGTERM. The design does not depend on
  the sleep; "the 9.5 s sleep is spent as a follower" simply does not arise.
- **The old leader grants the triggered candidate's vote** — true when the
  candidate's log is not behind: 5 of 5 M2 runs. In M3 the daemon's
  background writes race the trigger: see the M3 row. In both lost runs the
  old leader's replication stream to the candidate met the higher vote
  first, which is the "candidate one entry behind" case of §2: the old
  leader refuses, is deposed, and, with its elections off, cannot win back.
- **`runtime_config().elect(false)` stops this node campaigning** — true.
  With the call removed, the stopping voter campaigned 8.8 s after the
  leader died; with it, its term held for 10 s.
- **A leader with both followers paused still reports itself leader** —
  true in 5 of 5 runs.
- **The harness uses production Raft timing** — true
  (`production_hiqlite_defaults_with_read_pool`: 800 ms heartbeat,
  2.4–4.0 s election timeout).
- **`status()` is local reads only** — true for an embedded voter: local
  metrics, `query_map` on the local state machine, `Read::Local` protocol
  reads and the replication monitor's local metrics. The M1 source test
  pins it.
- **Fork patch counting** — `tests/operations/test_hiqlite_patch_ledger.py`
  counts rows, bullets, the intro's number words and the removal paragraph;
  updated for 29. Nothing else counts them.
- **Telemetry drain and Live TV stop** — the telemetry drain owns a 2 s
  bound; Live TV's worker waits are deadline-bounded.
- **Tokio signals after the first** — the handler stays installed for the
  process lifetime, so later signals are swallowed: a second signal does
  nothing, as §4 decides. Read, not run.
- **`-p plurx-core --lib shutdown_handoff`** — selects all four core tests,
  but only with `--features hiqlite-store`; the membership module is behind
  that feature.
- **Cold-stop gap** — measured 7.5–7.6 s from loss to a named successor,
  matching the 6.4–8.0 s source reading, not the audit's ~2.5 s.

### Mutations (each test shown failing with its subject removed)

| Test | Mutation | Failure |
|---|---|---|
| `shutdown_handoff_without_a_replicated_store_returns_at_once` | 1 ms sleep before `NotReplicated` | elapsed not zero |
| `shutdown_handoff_wait_is_bounded` | no `tokio::time::timeout` | outer 11 s guard fires |
| `shutdown_handoff_reuses_the_one_handoff_and_writes_nothing` | no `disable_local_db_elections` call | "every replicated stopping node switches its own elections off" |
| `a_sole_voter_shutdown_handoff_returns_at_once` | no `SoleVoter` check | `NoCandidate` |
| `local_election_switch_has_no_remote_fallback` | `elect(true)` | missing `elect(false)` |
| `run_leader_shutdown_handoff_case` | handoff returns `ElectionCandidateUnavailable` | "did not hand off: no_candidate" |
| `run_follower_shutdown_noop_case` | no `NotLeader` check | "a follower's stop reported handed_off" |
| `run_departing_voter_never_campaigns_case` | no `disable_local_db_elections` call | term 1 → 2 at 8.8 s |
| `run_shutdown_without_successor_case` | no outer timeout, and an attempt whose trigger fails never ends | request timed out at 15 s |
| `the_signal_path_hands_leadership_off_before_any_listener_stops` | handoff call removed from `main.rs` | "exactly one stop-time handoff call" |
| `sigterm_on_the_leader_moves_leadership_before_its_raft_stops` | handoff call removed from `main.rs` (the old code) | old leader exited with both survivors still naming it at term 1 |
| `sigterm_on_a_follower_changes_no_leader` | the follower triggers an election on itself before returning `NotLeader` | leadership changed 136 ms after SIGTERM |

The two follower tests also pass on the old code, because a follower's stop
never moved anything there. They guard the `NotLeader` branch. With only the
`NotLeader` check removed, the M3 follower test still passes, because the
first candidate is then the leader itself and triggering an election on a
leader moves nothing.
