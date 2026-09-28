# Cluster — status history

**Status:** done · records moved verbatim from `STATUS.md` on 2026-09-24

**Moved here from [STATUS.md](../../STATUS.md) on 2026-09-24**, verbatim, by
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](../ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
M6. Each section keeps its original heading under the date it was first
recorded in `STATUS.md`; relative links are re-based to this folder and
nothing else changed. Newest first. These are records: a section here
describes the state on the day it was written, and
`tests/operations/test_status_pr_claims.py` keeps holding it to the same
merged-pull-request rule it held in `STATUS.md`.

## 2026-09-08 · The transport-recovery campaign asserted a property the system does not have

**Decided and built; the lane that has been red on `main` and every PR since
`14d0519a` gets an assertion it can pass without losing its teeth.**
`ci / cluster transport recovery campaign` compared every one of its forty
cycles against a single warmed sample with all three margins at zero. The
counts it samples do not sit at a value: sockets and owned async tasks
alternate between two states one recovery apart — one connection and one
owned task per peer, still open when `operation_owns_work` clears — and
threads jitter on their own. Forty cycles needed forty favourable draws,
which is why the failure moved between cycles 1, 4, 6, 9 and 13 and why
`Main promotion gate` failed in two seconds behind it on every PR.

**What it asserts now is an envelope.** The series per node is the warmup
plus the twenty cycles, split into an opening half (cycles 0–10) and a
closing half (11–20); neither the floor nor the ceiling of the closing half
may exceed the opening half's by more than the allowance — zero for sockets
and owned async tasks, two for threads. A leak that starts early adds every
cycle and lifts the closing floor by ten or more; one that starts late puts
most of the closing window above anything the opening half showed — the
closing ceiling is the fourth-highest sample, because the high side is
in-flight residue and the record already shows it wobbling by one, so three
spikes are spikes and four are a trend. The drain visits both of its states
within a few cycles and moves neither edge. The option first written up —
last cycle against first cycle — was rejected on the way: two samples of a
two-state range is the same coin flip, drawn twice instead of forty times.
What still hides is stated, not implied, against the alternating series
the lane samples: a single connection leaked after about cycle 13, a
per-cycle leak that starts after about cycle 15, and a thread leak of one
per four recoveries or slower.

What moved: the sampler no longer waits for a ceiling, only for idle and two
identical samples; the record-time bail is gone; the campaign computes and
prints both bands per node at the end of each role and fails there, with the
whole per-cycle series and each cycle's snapshot source in the log (a leader
that moved between the halves is the one benign thing that looks like a
leak); the offline validator recomputes the envelopes from the cycles and
refuses a hand-written or reordered record. A smoke shorter than nineteen
cycles records and prints its envelope but does not fail on it — two-sample
windows are the coin flip again. Artifact schema is version 2
(`resource_*_envelope_allowance`, `resource_envelopes` per role). 39 focused
tests; the mutations prove a socket, thread or task leaked every cycle, a
leak starting at cycle 13, and one socket leaked at cycle 15 all still fail —
and that the measured drain and jitter pass. Decision document:
[docs/cluster/TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md](TRANSPORT-RECOVERY-RESOURCE-CONTRACT-DECISION.md)
§0. The lane's `timeout-minutes` goes from 120 to 240 in the same change: a
voter recovery is ~4.7 min on the two-core `ci-topology` runner, so the
voter half alone is ~100 min, and no Forgejo run has ever reached the
learner half to find that out. Still open: Option C there — making `idle`
mean the transports are released, which is a vendored hiqlite change and
helps everything that reads `operation_owns_work`, not only this lane.

## 2026-09-08 · Phase 3 is buildable today, and the first answer to that question was wrong

**Documentation only, and a decision taken in Paul's absence — overrule it
freely, the reasoning is written down so that is cheap.**
`M6-SERVER-PRIME-HANDOFF.md` §5 asked whether to hold server-side priming for
D6 or narrow `PREPARED_AXIS_SETS` to copy-only transitions. This session
answered "hold for D6", an adversarial review checked the premise against the
code rather than the prose, and it does not survive.

**There is an admitted, receipted, copy-only transition, and the VOD engine
serves it today.** `candidate_request` has three arms that leave a `Copy`
predecessor a `Copy` — `Auto`, `Original`, and a `Manual` ask at the source
height — and `EffectiveSelection` maps `Copy` to codec `source` on both sides,
so no `DeliveryMethod` crossing occurs. A direct-playing session toggling
between Auto and Original therefore crosses `{ResolutionOrBitrate}` **alone**:
in the axis table, receipted by M5.5 on Apple at 20/20, and yielding a `Copy`
candidate that `try_create_with_release_fence` serves, so the
`vod_transcode_unavailable` refusal never fires. The module's own test asserts
it returns `Prepare { ResolutionOrBitrate }`. It is a transition a viewer
makes.

So **phase 3 should be built now and proven against that**, with no new
hardware receipt and no change to the axis table. What stays blocked on D6 is
priming for the transition the fleet actually produces — a copy dropping to a
transcoded rung — and building the copy-to-copy case does not front-run it: it
means D6 lands into working machinery instead of an unbuilt phase.

**How the first answer went wrong, recorded because the shape recurs.** It
enumerated the copy-only transitions as *"`{AudioTrackOrOffset}` and a subtitle
burn removal"* and concluded that neither has a receipt. The handoff's own
option 2, four paragraphs above, says *"audio track, source-height
**Original**"* — and the item quietly dropped is the one carrying the receipt.
The substituted example is unreachable besides: `try_create_with_release_fence`
refuses `subtitle_burn.is_some()` independently of D6, so a VOD session with a
burn cannot exist to transition out of. **Swapping an example for the one on
the list is how a false premise reads as true**, and it is the same family as
the guards that could not fail: the reasoning looked careful and checked
nothing.

## 2026-09-08 · The axis receipt said do not widen, and the fleet widened anyway — correctly

**Documentation only.** `docs/playback-control/M6-AXIS-CASE-RESULTS.md` carries
a live verdict — *"do not widen `PREPARED_AXIS`"*, *"do not build §3.4 on this
evidence"* — and `PREPARED_AXIS_SETS` on `main` contains exactly the pair it
refuses. The code is right, and nothing said so.

**There were two runs on 2026-09-03 and that report is the first one.** It used
a 30 Mbit/s shaping proxy against an 18.183 Mbit/s predecessor: 1.65x headroom,
below the 2.0x floor `headroom_refusal` enforces, so it measured a transition
the server would have declined anyway. The re-run on a 40 Mbit/s link, 2.20x,
came back 20/20 clean, and `0cb370ac` admitted the pair on that. A dated
correction now heads the file, its title and status line say superseded, and
`docs/README.md`'s row says so too — that index is how a reader arrives.

**Two things the correction is careful not to claim.** The throughput floor is
why the run does not bear on *admission*; it is not a root cause for the eight
failures. §8 of that report declines to name one and §4's receipts point
elsewhere — failure tracks the commit boundary rather than the link, and every
failed row records `Pred stalls 0`. And **the receipt for the run that admitted
the pair is not in the folder**: it exists only in `0cb370ac`'s commit message.
The axis table is receipt-driven by design, so that is a gap, named rather than
filled by someone who was not there.

## 2026-09-07 · Settings put the operator on the login page, and the cause was a tombstone

**PR [#101](http://forge.lan:3000/noirr/plurx/pulls/101) — merged into
`main` as `a1b1fa34`, 2026-09-07. Not deployed.** Opening Settings
on the fleet returned the sign-in screen. The credential was valid the whole
time.

`cluster_node_removals` and `cluster_node_removal_attempts` outlive the removal
they describe: `finalize_node_removal` deliberately leaves both rows behind as
the durable tombstone, and a trigger protects them. Both guarded acquisitions —
the cache-admin exclusion and the planned-outage lease — read those tables
table-wide, so **the first node a cluster ever removes disables both for the
life of the cluster.** This fleet removed two nodes in August. Since then the
one-time credential-guard activation has retried every three seconds and never
completed (478 refusals in the 24 hours sampled, on each of four nodes, naming
nothing); `cache_admin_revocation_ready` therefore answered false, which fences
the Store-free admin proof cache closed on every node; `/cluster/status`, whose
guard consulted only that cache, answered **401** to an administrator; and the
web client, reading any 401 as "your session is over", called `logout()`.
Settings remembers its last section, so an operator whose last section was
Cluster met the login page on every attempt. `tcpdump` on media1 confirms the
shape: not one Begin fanout ever leaves the node — every attempt dies at the
lease. `plurx_cluster_removals_pending` correctly reported `0` throughout,
because the gauge already counts only untombstoned removals; the acquire SQL
and the gauge disagreed, and the acquire was wrong.

Four corrections, each with a regression that fails without it: the removal
predicate now means what the gauge means (a removal row whose node is not yet
tombstoned) in both acquisitions and in `lifecycle_operation_pending`; the
cache-only admin guard answers the cached proof first and falls back to a
bounded Store read, so a valid administrator gets `200`, a viewer gets `403`,
an unknown token gets `401`, and a node that cannot find out gets a named
`503` — never a 401 that ends a working session; the web client keeps the
session through a cluster-recovery refusal and paints it in the panel that
reports the cluster; and the activation loop names the precondition blocking
it and backs off to a minute instead of logging one unattributed sentence
every three seconds forever.

An adversarial review then found four gaps in that first tree, all closed
before the merge: the fence was corrected in two of the three statements that
carry it and `enter_maintenance`'s own admission still read the table wide;
the new predicate failed open on a removal row whose target cannot be resolved
at all; the blocker attribution named eight of the acquire's eleven standing
conditions and reported this node's own exclusion as another node's; and the
client guessed rather than asking, so `keepSessionOn401` now confirms the
credential against `/me` before anything is decided — a node still running the
older build refuses these reads with 401 while the session is fine, and a token
that dies mid-tick must still end it.

## 2026-09-02 · Everything a read-only cluster member could not do

**PRs [#806](https://github.com/pjunod/plurx/pull/806) (`b4a1f108`) and
[#821](https://github.com/pjunod/plurx/pull/821) (`27e751ef`) — both MERGED to
main, 2026-09-02.** Started from one screenshot: `lab3`, a committed learner,
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
both directions, and `docs/cluster/MEMBERSHIP-CREDENTIAL-SPLIT-PLAN.md` §2 already
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
yet verified on hardware** — when the fleet next takes a build, lab3 should read
*Direct status ready* rather than *Not observed*, and its active-stream count
should stop being structurally zero.

## 2026-09-01 · Exact-count cluster assertions (the `v0.3.0` release blocker)

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
