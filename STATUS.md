# Status — what the agent is working on and where it stands

**Updated:** 2026-09-02 (morning) · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## M7 M4 burn-join and current-main corrections

**PR [#794](https://github.com/pjunod/plurx/pull/794) — READY FOR EXACT-HEAD
GITHUB QUALIFICATION.** M7 M4 shipped through the effort train and its first
post-merge repair, then current-`main` CI exposed an ambiguous-success timeout
while confirming a replacement media session. The exact sentinel-guarded
confirmation now uses the bounded idempotent-write path; a zero-row replay is
accepted only after the durable route and current pointer prove the same
activation. Abandonment and ordinary transactions remain non-retryable.

The same qualification work reproduced two existing daemon-harness flakes.
Healthy Activity waves no longer spend 500 ms of the production two-second
peer deadline waiting for synthetic proxy receivers; exact physical-request
counts remain, and a paused unit now proves seven followers share the held
leading fetch. Both summary and detail must also retain node B's two
remote-only streams, closing an adversarially found false-green. Activation
fixtures hold one distinct reserved HTTP/Raft/Hiqlite port set until all three
listeners have been selected, preventing OS port recycling between fixtures.

- [x] Three adversarial reviews found the summary false-green and a rebased
      evidence commit that named a non-ancestor; both findings are fixed.
- [x] The corrective series is split into Store behavior/evidence and daemon
      harness behavior/evidence, with every mapping naming an ancestor.
- [x] The final affected-surface profile passed 12 checks with 0 failures;
      full workspace Rust, 118 executed real-cluster Store contracts, topology
      and failure drills, activation 7/7, and Activity 2/2 are green. Two
      Playwright-only preflights were skipped because Playwright is not
      installed on the device host and remain GitHub-runner work.
- [ ] Push the reviewed exact head to #794, require its full GitHub
      qualification and promotion receipt, merge it, close partial duplicate
      #782 with a cross-link, and verify `main` after the merge.

**Decisions made without Paul (flagged for review):** consolidate #782's useful
activation port reservation into #794, but replace its privacy-only Activity
barrier removal with the complete three-wave repair and deterministic unit.
Close #782 only after #794 is qualified and merged, so GitHub never loses the
visible replacement before the duplicate closes.

## Artwork repair-fence claim flake (same CI job)

**PR [#753](https://github.com/pjunod/plurx/pull/753) — MERGED to main
(`36da0497`, 2026-09-01).** While running #744's acceptance loop, the other
intermittent failure in `replicated store and topology contracts` fired —
`new leader did not exclusively fence artwork repair: []`, previously seen
on the effort-train qualification. Root cause: `claim_artwork_source_repair`
declines with an explicit `fence: None` when the leader's quorum
acknowledgement is older than 1s at the claim instant; on a loaded runner
that instant can fall in a scheduling gap the drill's own successor proof
already tolerates. The drill now retries only that classified no-op while
the same node reports itself leader in the same term, bounded well below
one repair lease; both exclusivity bails carry the node's raft state and
the durable repair row — CI's first capture of that evidence
(`since_last_ack: 1036 ms`, term stable) confirmed the mechanism and
exposed a self-defeating freshness guard in the first cut, fixed before
merge. Two adversarial review rounds; 21/21 local drill runs green; CI
green (one unrelated `activity proof` flake on the first run, green on
re-run — noted for a future look if it recurs). Also merged today:
[#752](https://github.com/pjunod/plurx/pull/752), the RELEASING.md answer
to the writeup's §7 question.

## Exact-count cluster assertions (the `v0.3.0` release blocker)

**PR [#744](https://github.com/pjunod/plurx/pull/744) — MERGED to main
(`0600459f`, 2026-09-01) after two adversarial review rounds, a green
`make check` + `make cluster-harness-check`, and a fully green CI run
including the release-blocking `replicated store and topology contracts`
job.**

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
- [x] Fix, first round: the drill declares its renewing lease in
      `ForceCompaction` and subtracts the lease row's `revision` advance.
      Superseded by the SQL-class accounting below after the next CI run
      showed revision-invisible background entries; in every round the
      assertion stays `!=`, blank/membership entries stay hard failures, and
      nothing is absorbed into slack — tolerance exists only for entries
      attributed to a declared, named class.
- [x] Instrumentation (validation builds only): applied-entry counters by
      payload kind in vendored hiqlite + env-gated per-entry apply log
      (`PLURX_VALIDATION_LOG_APPLIED=1`); both exact-count windows print a
      full accounting line and name contaminating entries in their bails.
- [x] Adversarial review round: clippy blocker fixed; boundary sampling
      made consistent (retried until no entry commits mid-sample); experiment
      argument rejections tested.
- [x] Second background writer caught by the first post-fix CI run: the
      membership heartbeat (one `cluster_node_heartbeat_intents` transaction
      per node per round) plus failed lease-renewal CAS attempts, neither
      visible to the lease row's revision. Reworked to SQL-class accounting:
      applied normal entries are attributed to registered classes at apply
      time, windows declare which classes are legitimate, undeclared classes
      stay hard contamination, and the topology artifact records the
      tolerated count as `window_background_entries` (schema extended).
- [x] Verification: `make cluster-harness-check` green end to end; 10
      consecutive learner-drill runs green with the renewal accounted;
      `make check` green; PR CI fully green (the one red along the way was
      the stale-base mobile-version trap below, cleared by rebasing).
- [x] Merged as `0600459f`. Nothing to deploy: the change is harness and
      validation-only; production `plurxd` compiles none of it.
- [x] Topology's CI contaminant: attributed to the membership heartbeat
      class and tolerated by name; lease-class traffic there remains hard
      contamination, and any unclassified entry still fails with kinds and
      terms named.

**Decisions made without Paul (flagged for review):** no change to
`docs/RELEASING.md` — the writeup's §7 suggestion (one cold-cache CI run
before the tag) is raised in the PR body for Paul to rule on. Trap worth
knowing: while a PR is open, client version bumps landing on main make the
`mobile release version` gate blame the PR through its stale recorded base —
the fix is a rebase, not a bump.
