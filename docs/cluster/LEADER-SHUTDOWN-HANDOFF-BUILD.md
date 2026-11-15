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
| Store | `main.rs:2028-2040` → `SelectedStore::shutdown` → `shutdown_voter` → `vendor/hiqlite/src/client/mgmt.rs` `shutdown_retained_startup` | *Corrected at build.* The daemon keeps its staged-startup listeners (patch 23), so its stop takes `shutdown_retained_startup`: no 9.5 s sleep, no wait for a leader, no 30 s bound. Old leaders exited 0.92–0.94 s after SIGTERM. |
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
     &status)` (a transfer since build, step 4), under one `tokio::time::timeout` of
     `SHUTDOWN_HANDOFF_WAIT = SURVIVOR_LEADER_WAIT + 2 s` (the trigger's
     1 s plus one probe round's 1 s). Outcomes: `HandedOff { successor }`,
     `NoCandidate`, `TimedOut`, `Failed`.
   - One log line, target `plurx::cluster`: outcome, successor, elapsed
     ms. Info for the first four outcomes, warn for the last three.
   It never returns an error: the caller always continues its drain.
2. **Fork patch 30: `Client::disable_local_db_elections()`**, local-only
   like patch 5, calling `raft.runtime_config().elect(false)`. Process
   state, not persisted; the next start campaigns normally. A lost handoff
   then costs one election, not two.
3. **Call site:** the first await in `boot`'s `shutdown` future
   (`main.rs:2608`), after the `select!` and before
   `leave_shutdown.cancel()`. Clone `state.membership` before
   `http::router(state)` takes the state. Placing it first means the
   drain's own writes go to the successor instead of racing a leadership
   change.
4. **A real transfer inside `handoff_leadership`** (added at build), used
   by shutdown, maintenance enter and exit, and a leader's self-leave.
   Fork patch 31 gates every database Raft proposal. With no eligible voter
   it answers `NoCandidate` at once, holding nothing. Otherwise:
   (a) it holds proposals: new ones wait, and the hold gets 0.5 s;
   (b) it waits on the metrics watch until an eligible voter's matched
   index equals its last log index (none in time: `NoCandidate`);
   (c) it triggers that voter once;
   (d) it waits until it follows a named new leader, then drops the hold;
   (e) it confirms with `await_survivor_leader`.
   Everything under the hold ends 2 s after it began
   (`PROPOSAL_HOLD_WAIT`), so writers here wait at most that long. All
   phases share one `SURVIVOR_LEADER_WAIT` deadline. Operator force election
   is still a plain trigger. With three voters the candidate plus the old
   leader is a quorum; with five, the other followers' live leases refuse
   the first round, and the candidate needs a second round after they
   expire.

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
| Leadership does not move | Usually `Failed` (trigger error or 8 s survivor wait); `TimedOut` only at the 10 s backstop | Elections stay off here (S4 ruling): a won-back leader that then stops cold would cost a second outage. The cluster is leaderless until a survivor's lease plus election timeout, about 6–8 s (measured 7.5–7.8 s), and this node's drain writes fail in that window. The transfer is what makes this path rare. |
| Second signal during the wait | No effect, as everywhere in the drain today | The wait is bounded. An immediate exit would skip the WAL and writer drain. |
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

Each test is shown failing with its subject removed (hand-back reports).

- **M1, core call and patch 30** (built as 29; renumbered when main's
  ledger gained its own row 29): `membership/tests.rs` `shutdown_handoff_*`, the real-voter
  `membership_api.rs::a_sole_voter_shutdown_handoff_returns_at_once`, and
  `mgmt.rs::local_election_switch_has_no_remote_fallback`.
- **M2, three processes:** the `leader-shutdown` mode, also in
  `controller()`. Its cases: the handoff (survivors name the successor,
  no sampled leaderless gap of 2.4 s, held 10 s after the kill), the same
  under two writers (`leader-shutdown-load`), the follower no-op, the
  leader with both followers paused, and the departing voter that never
  campaigns.
- **M3, the daemon:** `startup_tests.rs`, a source test that the call is
  the drain's first await, plus `cluster_activity.rs`. The leader SIGTERM
  test requires both survivors to name the successor within 3 s and before
  the old process exits, observed by a dedicated `waitid` thread. There is
  also a follower guard.
- **M4, paperwork:** index row; `docs/OPERATIONS.md` reworded in place,
  not grown; audit C2 marked; no `validation/points.toml` change.

## 6. Non-goals

- **No change to Hiqlite's `shutdown_execute`** or its timeouts. C3 owns timing.
- *Removed at build:* "no change to `handoff_leadership`". The ungated
  trigger lost its vote race in 5 of 14 daemon stops and 10 of 10 harness
  stops under write load, so the transfer (§3 step 4) is in scope.
- **No membership or settings write, no maintenance row, no lease.**
- **No flag, setting, retry loop, watchdog or second timer.** One bounded
  wait.
- **No election-floor change.** Ruling §8-3 waits on this landing.
- **No playbook work.** Whether the deploy path should also use
  maintenance is decided when the playbooks are read.

## 7. Execution log

| Date | Milestone | Agent-Model | Result |
|---|---|---|---|
| 2026-11-15 | Design | claude-fable-5-2 | this document |
| 2026-11-15 | M1–M4 | claude-opus-5-5 | As planned; patch 30 (built as 29, renumbered after main's row 29). |
| 2026-11-15 | Transfer | claude-opus-5-5 | §3 step 4, patch 31. |
| 2026-11-15 | Review fixes | claude-opus-5-5 | Learner catch-up outside the gate; the hold bounds the trigger; `NoCandidate` at once; self-leave uses the transfer. |

Session `session_01BCWLA3JD1HCNMEid3AZrc3` throughout.

| Measured on media1 | Before (ungated trigger) | After (transfer) |
|---|---|---|
| Cold stop, leader killed (harness) | 7.50–7.60 s to a new leader, 3 runs | — |
| Harness, quiet | won 5/5, 763–771 ms | won 1/1, 536 ms |
| Harness, two writers through the handoff | won 0/10; 7.1–8.1 s without a leader; failed writes in 7 of 10 runs (127–151 each) | won 20/20; 532–785 ms; 0 failed; slowest write 72 ms; committed = acknowledged |
| Daemon SIGTERM to the leader (3 s bound) | won 9/14; losses cost 7.8 s or more | won 20/20, twice; second set, with the exit-based test: both survivors named the successor 123–134 ms after SIGTERM, before the old process exited (0.63–0.91 s) |
| Survivors' switch, old leader to successor | ≤ 101 ms when won | ≤ 102 ms |

Findings: the old leader grants the vote only to a candidate that has
matched its last entry, which, under writes, the ungated trigger often
missed. `elect(false)` stops campaigning. A leader with both followers
paused still reports itself leader. Heartbeats append nothing. Writers
waiting on the hold are released once this node names the new leader;
they get `ForwardToLeader` with that leader, and Hiqlite's leader-change
retry resends them. A first build released at step-down; one forwarded
write then failed on `ForwardToLeader(None)`.
