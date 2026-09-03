# Status — what the agent is working on and where it stands

**Updated:** 2026-09-03 · Kept current by the working agent in the same
commit as the work it describes; a stale entry here is a bug. Newest effort
first.

## Dolby Vision Profile 7 on the web — what was actually left

**Effort `effort/dv-p7-web-delivery`, merged as `206ab3c3` (#869) on
2026-09-03.** An adversarial
review of the 2026-09-03 remux-refusal diagnosis found its mechanism right and
its fix already merged: #842 (`60e1be68`) closed the no-caps arm's silent
downgrade, and the argv in the diagnosis is from a build the fleet no longer
runs. What is left is four smaller things, one milestone each.

**M0 — correct the record, pin the call sites.** The #840 entry below called
the refused stream a 7→8.1 conversion; it was the *raw* Profile 7 remux from a
pre-#842 build, and no converted stream has ever reached a browser. Two
regressions added, each mutation-checked: the create arm that gives a no-caps
build a review at all (restore the pre-#842 `None` and the session serves raw
Profile 7 again — every review-level test stays green), and the
`served_copy_options` *call* in the live-recovery copy (delete it and the
spawned argv gains `-strict unofficial` and keeps NAL 62/63, which is exactly
the argv the production log carried). The second reads the argv out of a
scoped `tracing` subscriber, because nothing in the crate captured a spawned
command line before. Paul's R3 ruling — leave the preserved-Profile-7
`dvh1.07`-over-`hvc1` inconsistency — recorded in `docs/PLAYBACK.md`, with the
correction the review forced: Android *does* enumerate Profile 7
(`CapsPolicy.kt` maps `DVHE_DTB -> 7`), so the state is reachable from a
dual-layer box. The delivery plays; what it cannot rely on is the master
playlist and the init segment agreeing about the fourcc.

**M2 — the rejection report names both parties.** `stream_rejected` said
"browser refused the remux stream", which is the browser doing exactly what
its own capabilities document promised, and it carried no `session`, so the
server could not join it to the session it superseded. Both rejection paths
now send the join and three facts: the range and Dolby Vision profile the
create response said this session carries, and the profiles this browser
declared. The server prints them, recomputes `caps_mismatch` rather than
believing the client's copy, and keeps them in the stored event's `extra`.

**M3 — the indexer remembers what it could not do.** A `Truncated` or
`Unsupported` build was a log line: the cursor moved on, and the next wrap of
the library spent the same whole-file read — up to thirty minutes of one
node's disk — while `vodserve` answered `vod_index_pending` for a title that
may never have an index. A node-local `fragment_index_outcomes` table now
records the refusal, keyed and invalidated exactly like `fragment_indexes`, so
a replaced file is eligible again with nothing having to notice. Truncated
backs off (30 min doubling to a day) because the per-file budget is a
wall-clock guess; unsupported is terminal because it is a property of the
bytes. The cluster worker records the same row so a clustered node's badge and
background pass know what it found, but its *queue* policy is left alone —
that is `effort/fragment-index-queue-repair`'s, and it is rewriting the lease
and attempt budget this call feeds. The admin badge gained `refused` and
carries the builder's own reason. A review caught the backoff being inert on
every clustered voter: the hiqlite store's injected clock answers in unix
seconds and the deadline is compared against milliseconds.

**What merging main cost, and what it found.** Adding a v45 migration broke
three fixtures that describe an older schema relative to the newest one
rather than by name. Two `sqlite_v43_guard_migration_*` cases called v43
`SQLITE_SCHEMA_VERSION - 1` and downgraded a current database by undoing v44
alone; `populated_v14_import_fixture` builds a current database and walks it
back by hand, and its list stopped at v44. The third one is the interesting
one: it left `fragment_index_outcomes` in place under a `user_version` of
14, so activation replayed the CREATE onto a database that already had the
table, the voter process died inside `select_daemon_store`, and the one-voter
contract reported it as *activation voter exited before ready* — a failure
whose message names neither the migration nor the table. All three now name
the version they mean and drop everything above it. The whole replicated
Store lane passes locally: 120 of 120.

**The deployed-build re-test, as far as it goes.** The fleet was read over
SSH on 2026-09-03. The handoff's premise holds: #842 (`60e1be68`) is an
ancestor of every binary now running, so the arm it closed is closed in
production. Three nodes run `v0.3.0-515-gc2702f61`, matching their
checkouts; **nuc4 runs `v0.3.0-487-gd7194b05`** while its own checkout sits
at 515 — twenty-eight commits of drift, and nuc4 is the node the M5
verification document names. That has to be redeployed before any play
against it means anything. nuc4's `plurxd` logged no plan-derivation traffic
at all in twelve hours, which is the honest reason the `plan_derivation`
counters cannot be re-tested from the outside: they only move when someone
plays something. The live store is hiqlite; `/var/lib/plurx/plurx.db` was
last written 2026-08-26 and reading it would answer a stale question.

**Not verified on hardware.** The fleet serves this code now (below), but
nothing here has been played from a browser against it.
`docs/M5-VERIFICATION-PROMPT.md` is the hand-off, and it is gated on ops:
raft membership, then the analysis queue draining, then file 70's converting
identity being built.

**Merged to `main` 2026-09-03 as `206ab3c3`**, all six milestones, with the
`Main promotion gate` green across every one of its 21 jobs. Two late
corrections landed with it: the caps accusation is scoped to a delivery that
actually failed and reads exactly the declared set it prints (#870), and a
tripwire in the fast Rust gate now fails on the commit that appends the next
schema migration (#872) — v45 broke three "wind a current database backwards"
fixtures at once, in three files none of which the appending change touched,
and the worst of them took twenty minutes of `cluster-store-check` to surface.

**Deployed to the four servers 2026-09-03, `v0.3.0-568-gd4c67ff4`** — nynuc,
m6, nuc4, nuc3, all healthy, all answering `/readyz`. The first attempt did
not get there: a `deploy.yml` run from an agent session
restarted nynuc and was then killed mid-task by that session's own command
timeout, leaving the node out of the cluster for forty minutes while the
other three were never touched. What that node did while it was out is worth
recording, because the obvious reading of it was wrong. It looked like a
catch-up budget too small for the backlog — `install_snapshot_timeout_secs`
(120) plus the 45s Hiqlite start timeout, so 165s — with each expiry shutting
Raft down and the node losing ground every cycle: applied 6660079, then
6661534, then 6661534 again while the quorum watermark climbed 6662973 →
6665155. Raising that budget to 1800s changed nothing. Under it the process
sat for twenty minutes with its Raft port listening, its threads parked, and
not one byte written to `hiqlite/`; the leader logged
`AppendEntries … Unreachable … deadline has elapsed` against it the whole
time. **`docker compose up -d` — recreating the container rather than
restarting the process — caught it up in under a second**, and the node has
been healthy since. The wedge was in that container, not in the budget, and
the raised timeout was removed before the deploy.

**The deploy itself avoids the hole it fell into.** `tasks/app.yml` stops the
stack for the pre-deploy database snapshot and only then runs `make
docker-up`, so a cold Rust build happens with the voter down — which is
exactly how a node ends up thousands of entries behind. Each node here was
brought up with the same steps in the same order, with one addition: a
`docker compose build` *before* the stop, so the `--build` inside `docker-up`
is a cache hit. Build time stayed outside the outage and each node was absent
for about twelve seconds — healthy in 5s, `/readyz` 200 in 5s, no restart
during the attempt, no `unreplicated SQLite` or failed migration in the
attempt's logs. `main` moved twice during the rollout (`35a4773b`, then
`d4c67ff4`), so the first three nodes were run a second time; the whole fleet
is on one commit rather than three.

**That pre-warm belongs in `app.yml`.** It is the difference between a routine
deploy and the forty-minute recovery above, and it is four lines. Ansible
could not run from this session — the linked machine had no
`ansible-playbook` and 3MB of free disk — so the per-node steps were executed
directly over ssh instead; that is a deviation to close, not a new pattern.

## The ✕ on an iPhone could not leave a film

**PR [#853](https://github.com/pjunod/plurx/pull/853) — MERGED to main as `e31a6cb4`, 2026-09-03, branch `fix/close-control-exits`, fix commit `0f904213`.** Paul: "the x to
close out media playback does not work on apple devices. There's no way to
get out of the movie except force close." Confirmed at source, not on a
device: the iOS ✕ fed `back` to the touch routing table, and `back` while
chrome is visible is `hide` — right for a key, wrong for the one button whose
purpose is to leave. So the ✕ hid the chrome in `transport` (the state it is
tapped from), closed the panel in `info`, cancelled in `scrub`, and exited
only from `hidden` and `failed`, where it is not drawn. Android's phone back
arrow had the same fault in both `PlayerScreen` and `OfflinePlayerScreen`;
the system back gesture there still left after two presses, which is why
only Apple was reported. The web's `✕ Close` calls `closePlayer()` directly
and was never affected.

The contract now says what its own touch note already claimed: the ✕ is the
`close` control, not `back`. `close_control` in
`tests/playback/player-input-contract.json` gives it one outcome list per
state — close whatever is open, then `exit`, every row ending in `exit` —
transcribed as `PlayerInputRouting.closeSteps` (Apple) and
`PlayerInputPolicy.closeSteps` (Android), each checked against the fixture
by its client suite; the JS contract test pins the fixture's shape, and the
Apple suite pins the call site (the ✕ and the failure view's Close run
`closePlayer()`, and nothing in `PlayerView` manufactures a `back` press).
Apple build 115, Android versionCode 70 (main took 114/69 while this was open). Swift and Kotlin compile only on
CI; `make web-check`'s player suites and the input fence are green in the
clone. **Not run on hardware** — the device pass is the iPhone/iPad ✕ from
transport and mid-scrub (one tap exits), from info Standard and Debug (the
panel's backdrop takes the first tap, the second exits — the ✕ sits under
the panel by design), and the Android phone arrow from transport and
mid-scrub.

## Activity's Now playing row, read as a card

**PR [#849](https://github.com/pjunod/plurx/pull/849) — merged to main as
`a94a32cb`, 2026-09-03.** Paul asked for the activity status display to be "a
lot nicer": the Stream cell was one " · "-joined sentence of every session
fact, with the three things an operator brings to the page — is it playing,
is the server keeping up, is it held and why — buried among sequence
numbers. The cell now leads with a state pill, then named meters under the
player info panel's own labels (Position, Server ahead, Demand window
against target with a fill bar, Client runway, Suspends, Delivery rate),
then a "Technical details" disclosure carrying everything the sentence used
to say, which stays open and keeps keyboard focus across the repaint. One
adversarial review, nine findings, all implemented — the two that mattered:
"Server ahead" was about to show the demand window under the player's
label for the physical reserve (now two meters, matching the player), and a
failed producer would have worn a green Active pill (failures now outrank
everything but a dead lease). Also found in passing: the rung and encoder
were read off `deliveries[]`, which never carries them, so "1080p · vaapi"
had never rendered against a real server. `make web-check`'s node suites,
`js-check` and `contrast-check` all green in the clone; 27 painter tests.
Web-only — nothing to deploy to the nodes beyond the next server image;
the Activity golden has no live streams so `ui-check` is unaffected.

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
the escalation — the **raw Profile 7** remux a pre-#842 build served through
live-HLS recovery, which the browser refused with `MEDIA_ERR_DECODE` after
12 KB of a 12.5 MB segment. Not a 7→8.1 conversion: the conversion exists only
on the VOD path, file 70 has never had its converting fragment index built,
and no converted stream has yet been served to a browser at all. Real,
separate, and now costs a fallback rather than a failure. `vod_index_pending`
and
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
merged into main, 2026-09-02.** M1–M4 (web #795, Android #797, Apple #799,
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

**PR [#794](https://github.com/pjunod/plurx/pull/794) — merged `a664dfa5`,
2026-09-02.** M7 M4 shipped through the effort train and its first
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

**PR [#803](https://github.com/pjunod/plurx/pull/803) — merged `d9998f6c`,
2026-09-02.** A stalled
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
