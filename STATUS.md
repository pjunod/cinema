# Status — what the agent is working on and where it stands

**Updated:** 2026-09-01 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Exact-count cluster assertions (the `v0.3.0` release blocker)

**PR [#744](https://github.com/pjunod/plurx/pull/744)
(`agent/exact-count-drill-accounting`) — built, verified, in review/CI.**

The two exact-count assertions that failed the `v0.3.0` cut (#736) on a
no-Rust-behaviour diff were investigated per the exact-count writeup,
evidence first:

- [x] Root cause proven: the learner drill's own `provider:artwork` lease
      heartbeat (production `acquire_cluster_job`, renew every 30s) commits
      one Raft entry per renewal; any compaction window slower than 30s is
      off by exactly one. Reproduced deterministically; the contaminating
      entry named down to its SQL.
- [x] §4.3 (watermark read appends per call) refuted by experiment — new
      `plurx-cluster-check -- watermark-experiment N` subcommand, 100 idle
      pairs, zero movement.
- [x] Fix: the drill declares its renewing lease in `ForceCompaction`; the
      window subtracts exactly the lease row's `revision` advance under an
      unchanged owner and fence. Blank/membership entries stay hard
      failures. No tolerance, no widened assertion, no retry.
- [x] Instrumentation (validation builds only): applied-entry counters by
      payload kind in vendored hiqlite + env-gated per-entry apply log
      (`PLURX_VALIDATION_LOG_APPLIED=1`); both exact-count windows print a
      full accounting line and name contaminating entries in their bails.
- [x] Adversarial review round: clippy blocker fixed; boundary sampling
      made consistent (retried until no entry commits mid-sample); lease
      identity (owner + fence) asserted across the window; experiment
      argument rejections tested.
- [~] Verification: `make cluster-harness-check` green end to end; 10
      consecutive learner-drill runs green with the renewal accounted;
      full-suite run and PR CI in progress — merge follows green.
- [ ] Topology's CI contaminant: not reproduced locally (all windows
      64/64 normal). Deliberately left strict; the run-time check now
      names payload kinds and terms, so the next occurrence is a one-look
      diagnosis.

**Decision made without Paul (flagged for review):** no change to
`docs/RELEASING.md`; the writeup's §7 suggestion (one cold-cache CI run
before the tag) is raised in the PR body for Paul to rule on.
