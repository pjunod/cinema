# Status — what the agent is working on and where it stands

**Updated:** 2026-09-02 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## The player input contract, reviewed and finished

**Lane [`effort/player-input-contract`](https://github.com/pjunod/plurx/pull/814),
open into main, 2026-09-02.** M1–M4 (web #795, Android #797, Apple #799,
fence and settings fold #810) landed first. Each was then reviewed
adversarially against the fixtures rather than against its own description,
which found 29 defects the suites could not see — five of them blockers —
and each became its own task PR into the lane:

- **#825, the gates.** The lane's `check` job had grown an ffmpeg step it has
  no runner label for, so the fast Rust gate was red and the Store contracts
  and the promotion gate cascaded behind it. Two selection gaps went with it:
  `player-input-fence` hung off no client point, so the Kotlin and Swift diffs
  it exists to police never selected it, and a fixture-only diff selected no
  client suite and ran `player-input-contract.test.js` in no job at all — a
  ruling could have been deleted from the contract and merged green.
- **#826, web.** Four producers of playback position became one pending seek:
  a pointer click used to leave a keyboard preview pending (the clock froze
  on a time nobody could see), and a skip's 350 ms debounce could fire over
  the commit that replaced it — or over a player that had already closed.
  `idle` is now an input like any other, so auto-hide stopped keeping its own
  drifting copy of the suppression list, and the second `keydown` listener
  that made every `ignore` row act anyway is gone.
- **#827, Android.** `inputState()` asked a stale focus flag before it asked
  whether the chrome was hidden, so a direction could scrub a player with no
  chrome on screen — the one thing ruling 1 forbids. `reveal` came back on
  Play/Pause every time because `Controls` re-requested its own initial focus
  a frame later. `ignore` was implemented as "not consumed", which handed the
  key to the Media3 session.
- **#828, Apple.** The routing fixture was decoded with
  `.convertFromSnakeCase`, which renames dictionary keys: the contract's own
  input names no longer matched their raw values. tvOS Standard still printed
  a stall-count pill beside the `Stalls` row; focus was sent to a marker
  button that is drawn only while a marker is offered; the invisible reveal
  surface was drawn in front of the failure view, eating every press; and the
  lock screen skipped straight past the reducer.
- **#829, enforcement.** The fence knew one spelling of platform key handling
  (Compose's `Key.` constants, an `onkeydown=` attribute and
  `MPRemoteCommandCenter` all passed) and failed *open* on a missing file.
  Three tables in the contract were hand-written copies of the fixture; they
  are generated now, and the web reads `hide_after_ms`, `skip_seconds` and
  the coalesce window from the fixture instead of repeating them.
- **#831, the rest of the navigation (M5).** Cards and episode rows are
  reachable from a keyboard and a D-pad in every web layout — Classic, the
  default, had no path through the library at all — the header search keeps
  its caret across the re-render its own typing causes, the lightbox and edit
  dialogs announce themselves and return focus, Android's library, search and
  settings screens start with focus somewhere, Search uses the select-to-edit
  field a television needs, and tvOS detail stops re-grabbing focus on every
  appearance. `tests/ui-structure.golden` was regenerated for the new tab
  stops.

Reviewing my own work found five more: a focus request that had become a
value and so stopped moving focus at all, an `ignore` that swallowed
directions on every non-television device, a request that could crash on an
uncomposed node, Home re-grabbing focus on each reload, and Apple's Mini
strip stranded on screen by its own auto-hide fix.

**Open, and deliberately not decided here:** the web's transport row does not
match `controls.rows` (playback `info` and `close` sit in the top bar, and
fullscreen and title info have no fixture entry); Home/End and lock-screen
scrubbing have no contract row; `touch` cannot reach `scrub` through the
table at all; and Android's hide delay and skip step are still literals
rather than fixture reads. Nothing here has run on hardware: Chrome remux,
Google TV / Shield, an Android phone, Apple TV and iPhone are all unclaimed.

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
