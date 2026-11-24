# Vendored OpenRaft 0.9.25

This directory is the crates.io `openraft` 0.9.25 package, the exact release `Cargo.lock` resolved
before vendoring (checksum `a97014fb78acb77be3a40ac2da305f6dd3a6b243f3a908ace87d29b3972eaafd`, kept in
`registry-provenance.lock` for the weekly advisory scan). It is licensed MIT OR Apache-2.0. The
published package ships no license file, so `LICENSE-APACHE` carries the Apache-2.0 text it is
redistributed under. `Cargo.lock` is the package's own lock, kept so the crate's unit tests build
offline. The first commit that added this directory is the unmodified package, so `git diff` against
it shows exactly this patch.

**Owner:** Paul Junod (repository owner).

| # | Patch | Kind | Upstream | Drop condition |
|---:|---|---|---|---|
| 1 | Pre-vote before a timer-driven election | plurx policy | — | Never as a Plurx requirement; retire this implementation once the OpenRaft 0.10 port (C5, `docs/cluster/OPENRAFT-010-EVALUATION.md`) enables native pre-vote over an authenticated, negotiated RPC and the mixed-version case passes. |

- **What it fixes.** In 0.9.25 an expired election timer calls `Engine::elect`, which raises the
  term at once. A voter whose runtime was stalled sees an expired timer on resume even though the
  leader stayed healthy. Its higher vote reaches the leader on an append response, and the leader
  steps down. The fleet measured this on 2026-11-23: an 84 s voter stall deposed a healthy leader,
  and two terms followed (`docs/cluster/C4-PREVOTE-BUILD.md`).
- **The mechanism.**
  - In a multi-voter membership the tick now calls `Engine::handle_election_timeout`. That starts a
    pre-vote round (`engine/pre_vote.rs`) and emits `Command::SendPreVote`. The local vote, term and
    server state do not change.
  - Peers answer through `Raft::pre_vote` → `RaftMsg::RequestPreVote` → `Engine::handle_pre_vote_req`,
    which applies the vote request's lease, log and vote checks without updating anything. A leader
    always refuses.
  - Only a joint quorum of grants leads to the unchanged `elect()`.
  - A round is discarded once the local vote, its modification time or the effective membership
    changes, for example when a live leader's heartbeat arrives mid-round.
  - A failed round cannot reset the vote timer, so rounds are paced by one election timeout from
    the previous round's start.
- **What stays the same:**
  - a single voter elects at once;
  - `Trigger::elect` (Hiqlite patch 4, the C2 shutdown handoff) still calls `elect()` directly;
  - the vote and append RPCs, leader lease, timers and storage format are unchanged.
- **Network.** `RaftNetwork::pre_vote` returns `Ok(None)` by default, meaning the target cannot
  answer pre-vote, and that counts as a grant so an older peer never blocks an election. A
  timeout or transport error is no grant. Hiqlite patch 37 negotiates the RPC per Raft stream.
- Unit tests: `engine::tests::pre_vote_test` (the measured stall, stale rounds, peer checks, quorum
  and joint quorum, unsupported peers, pacing, triggered elections).

Files: `src/engine/pre_vote.rs`, `src/engine/tests/pre_vote_test.rs`, `src/engine/mod.rs`,
`src/engine/engine_impl.rs`, `src/engine/command.rs`, `src/engine/engine_output.rs`,
`src/core/raft_core.rs`, `src/core/raft_msg/mod.rs`, `src/core/notify.rs`, `src/raft/mod.rs`,
`src/network/network.rs`.

Cargo records this package as path-sourced, which means cargo-audit skips it. `scripts/vendor-audit-lock`
restores its registry source and checksum from `registry-provenance.lock` before the second advisory
scan. That must remain until this directory is removed.

Source: <https://crates.io/crates/openraft/0.9.25>
