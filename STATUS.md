# Status — what the agent is working on and where it stands

**Updated:** 2026-09-02 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## M7 M4 burn-join and current-main corrections

**PR [#794](https://github.com/pjunod/plurx/pull/794) — FINAL CI-PREREQUISITE
FIX UNDER REVIEW.** M7 M4 shipped through the effort train and its first
post-merge repair, then current-`main` CI exposed an ambiguous-success timeout
while confirming a replacement media session. The exact sentinel-guarded
confirmation now uses the bounded idempotent-write path; a zero-row replay is
accepted only after the durable route and current pointer prove the same
activation. Abandonment and ordinary transactions remain non-retryable.

The same qualification work reproduced two existing daemon-harness flakes.
Healthy Activity waves no longer spend 500 ms of the production two-second
peer deadline waiting for synthetic proxy receivers; exact physical-request
counts remain, and a paused unit now proves seven followers serialize behind
one leading physical fetch and are handed its completed snapshot. The summary
must retain node B's two-stream count and the detail read must name node B
among its deliveries even on a wave the summary led, closing an adversarially
found false-green. Activation fixtures hold all three reserved
HTTP/Raft/Hiqlite listeners open together until the set is chosen, so no
fixture can hand the same ephemeral port to two of its own listeners. The reservation is released before the daemon binds, so
it narrows one fixture's own selection rather than sequencing two fixtures.

- [x] Three adversarial reviews found the summary false-green and a rebased
      evidence commit that named a non-ancestor; both findings are fixed.
- [x] The corrective series is split into Store behavior/evidence and daemon
      harness behavior/evidence, with every mapping naming an ancestor.
- [x] **Superseded evidence, kept for the record.** Before the rebase and the
      two review rounds below, the affected-surface profile passed 12 checks
      with 0 failures over full workspace Rust, 118 executed real-cluster Store
      contracts, topology and failure drills, activation 7/7 and Activity 2/2,
      and hosted run
      [33622277315](https://github.com/pjunod/plurx/actions/runs/33622277315)
      passed policy, WAL, daemon, Store, topology and both package lanes. That
      run's only failure was all 107 ffmpeg-backed Rust cases missing `ffmpeg`
      on the GitHub image, against 1454 other passes from the same binary --
      the prerequisite this branch repairs. Eleven commits have landed since,
      so none of those numbers describes the current head; they are diagnosis,
      not merge evidence. Two Playwright-only preflights were skipped because
      Playwright is not installed on the device host and remain runner work.
- [x] The fast Rust lane now provisions pinned major 6 on hosted runners and
      requires the matching `ffmpeg-6` capability on self-hosted runners, with
      the operations contract binding both sides and pinning the step ahead of
      the gate it provisions. Seven self-hosted runners carry that capability
      alongside `high-cpu`, and two other required jobs already select the same
      class, so the label narrows the pool without stranding the gate.
- [x] Integrate #820, #822 and #806 by rebasing cleanly onto current `main`
      `b4a1f108`; refresh every SHA-bound mapping to its new ancestor. The
      intervening changes are confined to M6 playback-selection telemetry, its
      handoff, and the learner internal-peer authority repair, none of which
      touch the confirmation or Activity paths.
- [x] Two independent adversarial reviews of the rebased head both found that
      the ancestry-refresh commit had renamed the four mapping fragments to
      their new ancestors while leaving each fragment's `commits` field on the
      previous generation, so `make history-check` failed on the committed tree
      while passing in the working tree. Fixed, and six further findings
      implemented: the retried confirmation now reserves a recovery window
      inside the owner lease, the zero-row replay is bound to this call's own
      publication boundary, that guard and the durable-pointer proof are pinned
      at the call site, the after-commit fault injection is scoped to the
      activation statement, the summary-led wave now asserts detail retains node
      B's remote-only streams, and the operations contract pins the hosted
      ffmpeg step ahead of the gate it provisions.
- [x] A second pair of independent adversarial reviews of the repaired head
      agreed on two further defects and found three more. The recovery
      reservation was taken only when the whole margin fitted, so it vanished
      in exactly the case that needs it -- the split is now proportional to the
      lease actually left and lives in one named function. Nothing executed
      that reservation, so a source contract now pins it; four mutations were
      checked against that contract and all four turn it red. The embedded
      SQLite twin had not received the replay-boundary proof its replicated
      counterpart got, and the contract now scans both twins. One assertion in
      the paused Activity unit could not be failed by any mutation and is
      gone. The recovery-window mapping claimed a contract that did not yet
      exist and now states what is actually enforced.
- [x] Every gate re-run on this exact head: catalog 23 points / 27 checks /
      1296 audited files; history 1232 corrective commits / 736 explicit
      mappings; operations 156/156; ownership and routing inventories 11/11;
      `git diff --check` clean. Under the pinned `rustc 1.97.1`, `cargo fmt
      --all --check`, `cargo check --workspace --all-targets` and
      `cargo clippy --workspace --all-targets -- -D warnings` are clean,
      including `plurx-core` with `cluster-read-cost-validation`, and the fast
      unit lane passes 1567 with 0 failed and 3 ignored. The real-cluster
      Store, daemon-harness and hosted lanes remain the qualification run's
      work; they are not claimed here.
- [ ] Push the re-reviewed exact head to #794, require its full GitHub
      qualification and promotion receipt, merge it, close partial duplicate
      #782 with a cross-link, and verify `main` after the merge.

**Decisions made without Paul (flagged for review):** consolidate #782's useful
activation port reservation into #794, but replace its privacy-only Activity
barrier removal with the complete three-wave repair and deterministic unit.
Close #782 only after #794 is qualified and merged, so GitHub never loses the
visible replacement before the duplicate closes.

## Everything a read-only cluster member could not do

**PRs [#806](https://github.com/pjunod/plurx/pull/806) (`b4a1f108`) and
[#821](https://github.com/pjunod/plurx/pull/821) (`27e751ef`) — both MERGED to
main, 2026-09-02.** Started from one screenshot: `nuc3`, a committed learner,
showing a fresh heartbeat, zero apply lag and a green *Read worker ready* pill
while the same card said *Not observed · unreachable*. Two independent defects,
both of the same shape — code that asked whether the local node is a voter by
assuming it is.

**#806, the authority check.** `verify_live_activity_authority` required a
committed **voter at both ends** of every internal peer proof: the answering
node and the signer. So a learner answered 401 to every signed peer request —
its own operations-status reply to the Cluster panel, and every media-session
relay, control and abort a learner ingress originated — and every voter refused
a learner's. Meanwhile `learner_route_eligible` publishes exactly that surface
to learners, `operations_peers`/`media_peers` name them as fan-out targets in
both directions, and `docs/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md` §2 already
specified the member-scoped predicate here, giving the voter predicate to
membership mutation alone. The implementation was stricter than its own design.
`PeerAuthorityRole` now names the role each surface asks for; activity
aggregation stays voter-only because its directory never names a learner. The
aggregator also stopped folding a non-success response into `unreachable`
alongside a dead socket — 401/403 are `refused`, other statuses `http_error` —
and every observation code reaches the operator as a sentence rather than its
raw identifier. The cluster-check learner scenario now sends a proof in both
directions and was executed fails-first against the old predicate and against a
signer-only revert.

**#821, the placement arithmetic.** `activity_peers` returns the *other* voters,
so the voters a node can see are `peers.len()` plus itself only when it is one.
`remote_rollout_ready` and the shared-cache canary both added that `1`
unconditionally, which on a learner compares `n` against `n + 1` — false for
every roster size. A learner therefore answered 503 to every delegated media
session while still returning eligible offers a voter ingress would rank and
select, could never place one itself, and never published a verified
shared-cache root; a ready learner sitting at zero active streams was the
visible symptom. Both now count the voters this node can actually see.
Separately, `/internal/media/fragment-index/{key}` was missing from the learner
route matrix while `fragment_index_cluster` hydrates from `media_peers()` — a
peer directory pointing at a door the matrix had nailed shut.

Thirteen mutations across the two branches, all caught. `package and smoke
(amd64)` flaked once on a host-wide port collision with a concurrent job on the
same runner (`127.0.0.1:32402 … Address already in use`); re-run, green. **Not
yet verified on hardware** — when the fleet next takes a build, nuc3 should read
*Direct status ready* rather than *Not observed*, and its active-stream count
should stop being structurally zero.

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
