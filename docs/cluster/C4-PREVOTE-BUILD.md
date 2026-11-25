# C4 pre-vote — a stalled voter cannot depose a healthy leader

**Status:** open — plan, build in progress · **Executes:** C4 of the cluster audit
([§R1](CLUSTER-LAYER-AUDIT-2026-11-14.md#r1-a-stalled-voter-can-depose-a-healthy-leader)) ·
**Decision:** Paul, 2026-11-24: build pre-vote as the C4 remedy · **Written:** 2026-11-24

## 1. Measured problem

The passive collection prepared in [VOTER-TIMING-OBSERVATION-BUILD.md](VOTER-TIMING-OBSERVATION-BUILD.md)
ran about 9.3 h on post-C2 builds (2026-11-23 17:13Z → 2026-11-24 02:30Z). Normal voter append
gaps had p99 ≈ 25 ms. The largest stall on a healthy leader was 4.3 s as seen by its followers.
No election followed: the 6.4–8 s committed-follower timer
([OPENRAFT-010-EVALUATION.md §3](OPENRAFT-010-EVALUATION.md#3-c4-source-result--a-failed-campaign-can-still-disturb-the-leader))
covered it.

One voter stalled for 84.85 s. Its runtime stopped while its media mount was hung, and its serving
monitor woke 84.78 s late. The leader stayed healthy throughout. On resume, the stalled voter's
election tick ran `Engine::elect`, which raised its term. Its higher vote then reached the leader on
an append response, and the leader stepped down. Two terms followed within 4 s, and every node lost
serving authority (R2).

**The floor stays at 2.4–4.0 s.** No floor covers an 84 s stall, and a wider floor slows every real
failover. The defect is that a node which fell silent through its own fault can raise the cluster
term. The remedy is pre-vote. (Why the runtime stalled during NFS loss is a separate defect with its
own owner.)

## 2. Mechanism

OpenRaft 0.9.25 is vendored at `vendor/openraft` with a `PLURX-PATCH.md`. The patch:

1. **Election timeout → pre-vote round.** In a multi-voter membership, `handle_tick_election` calls
   `Engine::handle_election_timeout`, which no longer elects directly. It records a round:
   - the proposed vote `Vote(term+1, self)`,
   - the vote and `vote_last_modified` it started from,
   - a start time.

   It then emits `Command::SendPreVote`. Local vote, term and server state do not change.
2. **Peers answer without changing state.** `Engine::handle_pre_vote_req` grants only if all of
   these hold:
   - this node is not the leader,
   - it holds no live committed-leader lease (`now <= vote_utime + leader_lease`, the existing
     vote rule),
   - the candidate's log is at least as up to date,
   - the proposed vote is `>=` the local vote (the `update_vote` comparison).

   It never updates the vote, the timer or the log.
3. **Quorum → ordinary election.** `Engine::handle_pre_vote_resp` counts grants against the
   effective joint quorum set, the candidate counting itself. Grants from a stale round are ignored.
   A round is stale when the local vote or its modification time changed since it started, for
   example because a heartbeat from a live leader arrived. When a fresh round reaches quorum, the
   engine calls `elect()` unchanged.
4. **Pacing.** A failed round changes no state, so it cannot reset the vote timer. The next round
   starts only after one more election timeout from the previous round's start.
5. **A peer past this term.** Pre-vote alone never raises the term, so a node behind a peer that
   ran a campaign it never saw would propose the old term forever, and two survivors could refuse
   each other indefinitely. A refusal whose vote is greater than the local vote is therefore
   adopted, non-committed, exactly as a refused real vote is (`handle_vote_resp`); the next round
   proposes past it. A leader's or leased follower's refusal carries the committed vote the stalled
   voter already holds, which is never greater, so the C4 deposition cannot return this way.
6. **Elections disabled mid-round.** A round in flight ends without electing once elections are
   disabled (Hiqlite patch 30, shutdown handoff) or the node stops being a voter.
7. **Unchanged paths:**
   - a single voter elects at once;
   - `Trigger::elect`, used by Hiqlite patch 4 and C2 shutdown handoff to elect a chosen successor,
     still calls `elect()` directly;
   - the vote and append RPCs, and leader-lease rules, do not change.
8. **Network:** `RaftNetwork::pre_vote` is added with a default that reports *unsupported*. An
   unsupported or unreachable peer is handled differently:
   - *Unsupported* (it cannot answer) counts as a grant, as upstream 0.10 does for mixed versions,
     so an old peer never blocks a needed election. The one exception is the committed leader this
     node last followed: alive, it would refuse, so its silence is not a grant.
   - *Unreachable* or timed out counts as no grant, as for a vote.

## 3. Wire compatibility (Hiqlite)

`RaftStreamRequest` and `RaftStreamResponsePayload` are positional, and an older server closes the
whole stream, appends included, on an unknown request variant (`raft_server.rs`). So:

- `PreVoteDB` and `PreVoteCache` are appended after every deployed ordinal, in both enums.
- Each Raft stream server adds `x-hiqlite-raft-prevote: v1` to its WebSocket upgrade response. The
  client records the header per connection (after any reconnect too). Without it, the connection
  task answers a pre-vote locally as *unsupported* and never writes the new variant. This follows
  patch 21's negotiated `x-hiqlite-write-ack` pattern.
- An old client never sends the new variant. An old server never advertises the header.
- PreVote travels only on the existing challenge-response-authenticated Raft stream.

In a mixed cluster an upgraded voter is protected only by upgraded peers. Full protection needs
every voter on this build. Rollout needs no flag or ordering.

## 4. Qualification

| Case | Where |
|---|---|
| Election timeout in a multi-voter membership starts a pre-vote and leaves term, vote and state unchanged (on today's source it elects) | `vendor/openraft/src/engine/tests/pre_vote_test.rs` |
| The measured case: a follower whose timer expired during a stall sends a pre-vote. Both peers refuse, the follower that holds a live lease and the leader. Nothing changes and no election follows. A heartbeat arriving mid-round makes the round stale | same |
| A peer grants only without a live lease, with an equal or better log and a non-smaller vote, and changes no state | same |
| A quorum of grants → `elect()` with term+1, and a joint-membership quorum is respected | same |
| Leader loss: when every lease has expired, the pre-vote succeeds and the election proceeds | same |
| Pacing: no second round before an election timeout passes | same |
| Single voter elects at once; external trigger elects directly | same |
| Negotiation: only an advertising server receives `PreVote`; an old server's connection answers *unsupported* locally and keeps its append stream | `vendor/hiqlite/src/network/raft_client.rs` tests |
| New variants come after every deployed ordinal | `vendor/hiqlite/src/network/raft_server.rs` tests |
| Existing OpenRaft engine tests, Hiqlite tests, cluster lanes | fast lane once on the final head |

## 5. Boundaries

- Files:
  - `vendor/openraft/**` (new: the 0.9.25 package, then this patch),
  - root `Cargo.toml` and `Cargo.lock` (`[patch.crates-io]`),
  - `vendor/hiqlite/src/network/{raft_client,raft_server,web_socket_connect}.rs`,
  - `vendor/hiqlite/{Cargo.toml,Cargo.lock,PLURX-PATCH.md}` (patch 37),
  - the vendoring checks: `scripts/vendor-audit-lock`, `THIRD-PARTY-NOTICES.md` and their tests,
  - the `cluster-wal-check` lane, which now runs the pre-vote tests and runs the existing OpenRaft
    snapshot regression from the tree instead of the registry package,
  - this document and the cluster index.
- No timer, lease or proof changes. No feature flag. No fleet deploy or restart: Paul deploys.
- Fleet acceptance after deploy: re-run the passive collector. A voter stall, if one recurs, must
  produce no term change.
- The OpenRaft 0.10 port (C5) can retire this patch once native pre-vote is enabled and
  authenticated, and the mixed-version case passes.
