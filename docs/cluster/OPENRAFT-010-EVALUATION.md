# OpenRaft 0.10 — patch retirement and starved-voter evaluation

**Status:** tracked evaluation; no upgrade or patch removal · **Executes:** C5
and the source-verification part of C4 from
[the cluster audit](CLUSTER-LAYER-AUDIT-2026-11-14.md) · **Written:** 2026-11-19

Companion to [the timing delivery](CLUSTER-TIMING-BUILD.md) and
[the fork inventory](../../vendor/hiqlite/PLURX-PATCH.md). The integration owner
maintains this item under the P47 cluster programme. The next step is an
isolated dependency-port branch after the current facade promotion, not a
version bump in the timing extraction.

## 1. Decision — replace mechanisms only after preserving their contracts

The deployed dependency remains OpenRaft **0.9.25**, locked by checksum
`a97014fb78acb77be3a40ac2da305f6dd3a6b243f3a908ace87d29b3972eaafd`
in [`Cargo.lock`](../../Cargo.lock). The comparison target is the immutable
[v0.10.0 release source](https://github.com/databendlabs/openraft/tree/6a6c8e71e4d086090e863594564108099a71c983),
not a mixture of mutable `latest` documentation and the development branch.

The release's [upgrade guide](https://github.com/databendlabs/openraft/blob/6a6c8e71e4d086090e863594564108099a71c983/openraft/src/docs/upgrade_guide/upgrade-v09-v10.md)
requires adapting storage, state-machine and network interfaces as well as
Raft calls and feature flags. Hiqlite's wire ordinals, snapshot image/WAL
compatibility, deployed rollback path, TLS/transport policy and application
proof formats remain separate acceptance obligations. API availability alone
is not evidence that a rolling upgrade or downgrade is safe.

## 2. Patch disposition — three candidates, no automatic deletion

| Patch | Native mechanism to evaluate | Retirement condition |
|---|---|---|
| 4 · election-trigger bridge | [`Trigger::transfer_leader`](https://github.com/databendlabs/openraft/blob/6a6c8e71e4d086090e863594564108099a71c983/openraft/src/raft/trigger.rs) | Replace the 0.9 trigger bridge only after native transfer targets an eligible survivor, catches it up, and the caller observes actual leadership movement inside its original deadline. Preserve local election disabling, proposal hold and ordered shutdown; related patches 30/31 need explicit dispositions. |
| 6 · quorum watermark bridge | [`Raft::get_read_linearizer`](https://github.com/databendlabs/openraft/blob/6a6c8e71e4d086090e863594564108099a71c983/openraft/src/raft/mod.rs) | Replace the 0.9 proof acquisition implementation, retaining Plurx's leadership, membership generation, fixed freshness and local apply-through-index checks. A linearizer is not permission to serve before its apply condition is satisfied. Application proof wrappers remain policy. |
| 28 · reused read-confirm connections | [`RaftNetworkFactory::new_heartbeat_client`](https://github.com/databendlabs/openraft/blob/6a6c8e71e4d086090e863594564108099a71c983/openraft/src/network/factory.rs) | Implement a genuinely independent, reusable heartbeat transport and prove its lifecycle before removing the 0.9 client-leasing workaround. The upstream default calls `new_client`; merely upgrading leaves transport reuse and head-of-line isolation unproved. Preserve bounded request queues, cancellation, reset, peer identity and socket-owner joins. |

These are implementation replacement candidates. Their Plurx policy
contracts do not disappear with the dependency upgrade. The fork inventory's
three drop conditions link here instead of calling the 0.9 mechanism eternal.

## 3. C4 source result — a failed campaign can still disturb the leader

This chain was inspected in the exact cached 0.9.25 registry source selected
by the lockfile. It establishes a possible mechanism, not the trigger or
stall duration of a historical fleet incident.

1. `src/core/raft_core.rs::handle_tick_election` returns for a leader,
   non-voter or disabled election timer. A voter whose timer expires calls
   `Engine::elect`. A task that was not scheduled can observe the expired
   monotonic deadline when it resumes; source does not prove which queued
   heartbeat or tick is processed first after a real stall.
2. `src/engine/engine_impl.rs::elect` increments the local term, installs its
   own vote and sends vote requests. There is no pre-vote phase on that path.
3. `handle_vote_req` first rejects a vote while its existing committed leader
   lease is live, and also rejects an inferior log before updating the vote.
   Therefore healthy peers can refuse the campaign without adopting its term.
4. The campaigning node already retained its higher vote. A later old-term
   AppendEntries can receive a higher-vote rejection. Both heartbeat and
   replication responses report that vote to `RaftCore`; when the originating
   leader vote still matches, the core calls `VoteHandler::update_vote`.
5. `src/engine/handler/vote_handler/mod.rs::update_vote` persists the greater
   vote and updates internal state. A vote for another node takes the leader
   into the following state. A successful majority election was not required
   for this disruption.

**The audit's approximate two-second threshold is not verified.** In this
source, `EngineConfig::new` sets the leader lease to the configured election
maximum. A committed follower's ordinary timer is that lease **plus** its
random election timeout. At the current 2.4–4.0 s configuration that is
6.4–less-than-8.0 s since the last vote touch, before scheduling delay. An
uncommitted candidate uses only the random election timeout. The greater-log
condition adds another 8 s. These state-dependent rules are also documented
in upstream 0.9.25's `src/docs/protocol/leader_lease.md`; the configured floor
alone is not the complete elapsed outage bound.

## 4. Qualification — preserve failures and measure before tuning

The isolated port must exercise all of these cases before retiring code:

- Native transfer during ordinary traffic, a lagging target, a failed target,
  sole-voter refusal, cancellation and shutdown. Check the actual successor
  and the existing maintenance/claim ordering, not only an accepted RPC.
- A read that races a newer leader, delayed local apply, membership change,
  partition and snapshot replacement. Old proofs and outstanding callers
  must fail closed; read arrival/coalescing order stays intact.
- The existing patch-28 socket reuse, reset, timeout, idle-close, cap and
  shutdown cases, plus a blocked replication/snapshot stream while an
  independent heartbeat continues. Count real dials and owned sockets.
- Persisted 0.9 stores and snapshots opened on the candidate port, explicit
  protocol/version refusal, and a proved rollback or a documented refusal
  before any irreversible format change. Never infer compatibility from a
  successful fresh-cluster test.
- A controlled, identified voter scheduling stall and recovery, with observed
  vote/term changes and peer RPC order. Evaluate 0.10 pre-vote separately;
  if retaining 0.9, choose any wider floor from actual post-C2 stall data.

**C4 remains open:** fleet stall measurements and the chosen remedy are not
present in this lane. The timing extraction changes no election floor,
lease, proof validity or retry deadline. The 16 s recovery versus 12 s normal
media lease mismatch also remains an explicit decision owed by the cluster
owner; the longer takeover claim does not repair the ordinary lease.

## 5. Evidence — source provenance is not runtime qualification

External lane receipts preserve hashes of seven deployed 0.9.25 source files
and the immutable 0.10.0 source files/upgrade guide. The inspected source is
consistent with the mechanism above. No OpenRaft port, mixed-version run,
pre-vote experiment, fleet stall reproduction or patch retirement has run.
The integration owner records those results here when they exist.
