# Status — what the agent is working on and where it stands

**Updated:** 2026-09-03 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Nothing played on the web, and every fallback was terminal

**[`agent/hls-startup-demand-deadlock`](https://github.com/pjunod/plurx/pull/840),
merged to main as `9646f99f`, 2026-09-03.** Web playback failed on every title
tried, in three separate browsers, while the TV played the same library —
which is what proved it was the server rather than one browser profile.

Every player reports demand `hold` before it has started, because its
`<video>` has never received a byte and is therefore paused. Explicit flow
control obeyed that hold, so a fresh session's producer was suspended at a
target of zero one second after ffmpeg started. No playlist was ever written;
the playlist request the same client was blocked on spent the whole
`PLAYLIST_WAIT_BUDGET` and returned 503; the client reported
`manifestLoadTimeOut` and the viewer was told the server couldn't build the
stream. The client could not say `active` until it played and could not play
until production ran.

The blast radius was every *fallback*: a session that escalates from a refused
remux to a transcode is a fresh session, so a delivery fault that the fallback
exists to recover became a dead player instead. Observed on nuc4, file 70.

A session below `EXPLICIT_STARTUP_FLOOR_SECS` of published media is *starting*,
and starting suspends the demand hold and the time limiter both — exempting the
hold alone leaves the same deadlock reported as `Time`, because
`time_release_threshold` releases below the floor it would be guarding. The
byte limits are never suspended. The grant is latched per session on
publication, so a producer retry cannot renew it.

Two adversarial reviews, five defects found in the fix itself and all fixed:
a vacuous-and-failing new test, two existing tests silently borrowing the
startup exemption, the disk caps not consulted at all inside the publish gate,
the grant renewing on every retry, and the `Time` relabelling above. Full
qualification green; 1601 unit tests.

**Not fixed here, and still open:** the truncated first segment that caused
the escalation — a DV Profile 7→8.1 remux the browser refused with
`MEDIA_ERR_DECODE` after 12 KB of a 12.5 MB segment. Real, separate, and now
costs a fallback rather than a failure. `vod_index_pending` and
`vod_transcode_unavailable` both fell through to live-HLS recovery on this
file. And the web client reporting `hold` from a player that has never
started is honest to fix at the client too, though the server invariant has to
hold for every client regardless.

## The picker says which machine again

**[`agent/discovery-machine-name`](https://github.com/pjunod/plurx/pull/839),
open into main, 2026-09-02.** Every node of one logical server reports the
same `server.name`, so since `4a7ead78` (2026-08-20) a clustered node
advertised that bare name plus twelve characters of its node id — three rows
of `plurx · 5deeeebc8f39` in the Apple TV picker, on a fleet whose machines
have perfectly good names. It regressed `474e2ee1`, and neither
`deploy/README.md` nor the compose comments ever stopped promising
`m6 · 192.168.1.20`; the discovery companion still runs `uts: host`
specifically so it can read the machine hostname.

The name a node computes for itself is what it advertises now, in both
advertisement paths. The node-id suffix survives as the fallback for a node
that has neither a hostname nor a LAN address — that is the DNS-SD uniqueness
the suffix existed for, and it is the only case that ever needed it, since a
hostname is unique on one LAN and an address is unique by construction. The
full node id stays in the TXT record and in the per-node host record either
way, so nothing that resolves a node loses information.

Three tests, one of them on the call site: reverting the branch that handed a
clustered node its bare `server.name` fails on any host that has a hostname or
an address (mutation-checked — it comes back `plurx · 6b98c6cb8388` against an
expected `vm · 192.0.2.2`). Server-only: no client rebuild, and the fleet needs
a redeploy before a TV shows the difference.

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

**Since ruled (2026-09-02):** the row grammar now describes what each surface
actually renders — a `bar` row for the corner strip every client puts Close in,
`title_info`/`fullscreen`/`airplay` named as web-only, and desktop's relocated
`info` declared in `surface_placement` — so `player-dom.test.js` derives both
rows from the fixture instead of carrying an exception list. Android's hide
delay and skip step read the fixture too.

**Still open, deliberately not decided here:** Home/End and lock-screen
scrubbing have no contract row, and `touch` cannot reach `scrub` through the
table at all (the pointer drag is prose in `surface_notes`). Nothing here has
run on hardware: Chrome remux, Google TV / Shield, an Android phone, Apple TV
and iPhone are all unclaimed.

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
- [x] Integrate #820, #822 and #806 by rebasing onto `main` `b4a1f108`, and
      refresh every SHA-bound mapping to its new ancestor.
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
- [x] **The branch moved underneath this work twice.** Another session pushed
      a merge of current `main` onto the PR head while the review repairs were
      being verified, and `main` itself advanced four times in two hours --
      #806, then #821/#824/#789, then #823. Rather than force-push over that
      session's integration, this series is restacked onto the pushed head and
      current `main` is merged in. Nothing of the other session's work is
      discarded; every corrective commit on the series took a new identity, so
      all nine evidence fragments are renamed and rewritten in one commit --
      filename and payload together, which is the pairing the first refresh got
      wrong. The one merge conflict was the rolling-producer ownership
      inventory: #823's production shadow task and this branch's Activity
      in-flight fixture each described the step 381 -> 382, so the merged count
      is 383 and both narratives are kept. The inventory audit settles that
      number, not the prose.
- [x] Every gate re-run on this exact merged head: catalog 23 points / 27
      checks / 1297 audited files; history 1234 corrective commits / 737
      explicit mappings; operations 156/156; ownership and routing inventories
      11/11; `git diff --check` clean. Under the pinned `rustc 1.97.1`,
      `cargo fmt --all -- --check`, `cargo check --workspace --all-targets` and
      `cargo clippy --workspace --all-targets -- -D warnings` are clean,
      including `plurx-core` with `cluster-read-cost-validation`, and the fast
      unit lane passes 1574 with 0 failed and 3 ignored. The
      real-cluster Store, daemon-harness and hosted lanes remain the
      qualification run's work; they are not claimed here.
- [ ] Push the re-reviewed exact head to #794, require its full GitHub
      qualification and promotion receipt, merge it, close partial duplicate
      #782 with a cross-link, and verify `main` after the merge.

**Decisions made without Paul (flagged for review):** consolidate #782's useful
activation port reservation into #794, but replace its privacy-only Activity
barrier removal with the complete three-wave repair and deterministic unit.
Close #782 only after #794 is qualified and merged, so GitHub never loses the
visible replacement before the duplicate closes.


## M7 R-M3 — one playback owns one subtitle window

**PR [#830](https://github.com/pjunod/plurx/pull/830) — merged `653569d0`,
2026-09-02.** The subtitle sidecar cache is keyed by span, and a seek storm
produces a run of legitimately different spans, so twenty extractions could be
running for one viewer with every registry in the server believing each of them
correct. A registry keyed by **playback session** now holds the anchor being
extracted, the control sequence that justified it, a way to stop the real ffmpeg
and a way to wait for it to be gone. The same anchor joins; a different anchor
with a strictly newer settled sequence aborts its predecessor **and waits for
that abort to settle** before starting a successor; a different anchor with
nothing newer behind it starts nothing.

The window path no longer detaches its extraction — cancelling the outer warm
used to cancel a waiter rather than the worker, because `ensure_vtt_at` spawned
a second owner task — while the whole-track path keeps its detached,
cancellation-independent contract exactly, since that sidecar is what every
other consumer needs. An abandoned window leaves **no negative memo**: nothing
was wrong with it except where the viewer went, so the next request for that
span is a first attempt rather than a suppressed retry. Once bytes exist there
is no cancellation point at all, and a published sidecar is never removed by
window-flight cancellation.

The slot is held by a **destructor** and release **fences** the session
briefly — a panicking task, a runtime shutting down, or a request that read its
authority a moment before teardown would otherwise leave an owner nothing will
ever release, with a terminal cleanup parked behind it. Session end releases the
owner on both lifecycles: rolling after terminal admission, VOD inside
`detach_reader` once the readers guard is dropped.

Admission reads the client's settled destination immediately before warming, and
refuses a window that lies behind it or beyond the forward reach a client
buffers into. That reach is deliberate: containment alone would refuse the
window immediately *in front of* the playhead at every grid boundary, which is
the exact gap the bridge exists to close, recurring once per span for the whole
file.

Acceptance is at the production boundary — a twenty-seek storm through real
control exchanges and the real subtitle handler, with producers counted by a
drop guard and flights counted under the owner registry, so a superseded
extraction is observed dying rather than assumed to. Not deployed.


## Apple pacing-hold freeze — the hold that vetoed its own recovery

**PR [#803](https://github.com/pjunod/plurx/pull/803) — OPEN, awaiting Paul's
merge (`fd23da27` on `agent/apple-pacing-hold-freeze`, 2026-09-02).** A stalled
Apple client with an empty buffer and fetchable media asked the server what to
do, was told `hold { reason: "time" }`, showed *"The server is pacing this
stream."*, and returned without reopening its player item. In explicit lease
mode the stall itself manufactures that hold — the production target is the
client's runway plus a 30-second reserve, measured from its own frozen buffer
anchor — so the loop had no exit but the viewer backing out. Web had the same
veto behind a manual *Try again*.

The invariant, established server-side and defended on both clients:
**production state is never authority over serving; a client that can fetch
published bytes may reconnect to fetch them, and the only question a hold
answers is why the producer paused.**

- `resolve_action` answers `none` when the same request proves the client is
  stalled, its decoder starved, its runway at or under 10 s, and at least 10 s
  of published media unfetched — one predicate for all seven hold reasons. The
  hold is still reported in `delivery.hold_reason`; only the instruction is
  withheld, counted by its own
  `plurx_playback_control_recovery_withheld_total{reason,platform}` so it can
  never be read as the vocabulary gap `actions_suppressed_total` measures.
- A wedged reopen keeps its rung on both sides: the client drops the stall
  ticket, and `normalize_claimed_request` declines the one-rung descent for a
  predecessor carrying the wedge signature (no completed delivery for 16 s with
  ≥ 10 s published and unfetched). A slow link fails the idle term and keeps
  today's descent. The server half covers Android's ticketed reopens too. The
  reopen keys on evidence rather than on the stall kind because the two
  detectors race for one freeze.
- VOD's serving frontier is now the contiguous materialized run measured from
  the client's own fetched segment; `published_end_ms` counts from segment 0
  and sits behind the playhead after a far seek. The activity page keeps
  reading the old one, which answers a different question.
- Local gate on the rebased branch: 1552 Rust tests, clippy `-D warnings`,
  `cargo fmt --check`, all four `tests/playback` suites, `history-audit`, and
  every `validate --profile commit --staged` check that does not need cargo.
  Three mutation checks — the predicate call, the server's wedge branch, and
  web's supply `return` — each fail exactly the tests that own them. Apple
  build 110 claimed; Swift and Kotlin compile on the self-hosted runners, and
  device acceptance is opportunistic because the AVPlayer wedge cannot be
  induced on demand. Not deployed.


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
