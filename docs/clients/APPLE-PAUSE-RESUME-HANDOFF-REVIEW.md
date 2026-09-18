# Review — Apple pause/resume handoff (bounded return to the picture)

**Verdict: APPROVE WITH CHANGES.** The lever is right and the incident
evidence now *proves* it (see A below — the handoff calls it an inference
because it never read the snapshot it already had). Three things must change
before an implementer picks this up: the §5.2 cold-start ceiling (B), the
fast-path repair arithmetic (C), and the reporter seam (D). Everything else
folds in.

**Verified against:** `origin/main @ 6fb0901d3d18c1b181f7299f4faddfb73994fd1a`
(re-fetched from Forgejo at review time; still the tip). Every file read via
`git archive <sha>`, never the working tree. Reviewed document: the untracked
`docs/clients/APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md` in Paul's checkout
(607 lines, byte-identical to the pasted text). Swift claims are reasoned from
source, not compiled — no toolchain here. Incident evidence re-pulled from
`m6` read-only: `docker logs` plus `playback_events` rows 66687–66723 in
`/srv/plurx/hiqlite/telemetry.db`.

Line numbers below are `clients/apple/Sources/PlayerController.swift` at the
pinned sha unless another file is named.

## Required corrections

### A · The evidence already settles the "inference" — cite it (P1, evidence)

The stall report at 03:22:01.303 carries the full diagnostic snapshot
(`reportPlaybackStall` → `playbackDiagnosticSnapshot`, 6802–6824). The server
does not print it in the log line; it is stored in `playback_events.extra`
(`http/system.rs` 1153–1253). Row 66713 on m6 says:

```
time_control_status = "waiting"
waiting_reason      = "AVPlayerWaitingToMinimizeStallsReason"
playback_buffer_empty = false     playback_buffer_full = true
playback_likely_to_keep_up = false
runway = 62.338 s   position_ms = 29661
server: hold_reason=time, suspended=true, delivered_idle_ms=532200,
        published_end_ms=98000, fetched_end_ms=92000, last_request=control
```

That is the exact state `playImmediately(atRate:)` is documented to override:
AVPlayer parked in `.waitingToPlayAtSpecifiedRate` for `.toMinimizeStalls`
with a full 62 s buffer and `isPlaybackLikelyToKeepUp == false`. §1's
"Inference" paragraph should become "Established", with these fields quoted,
and §5.5's "wait reason if available" should say the field already exists
(`ApplePlaybackDiagnosticSnapshot.waitingReason`, 389/2444) and is retained
server-side — the implementer must not add a second one.

Three smaller evidence corrections while you are in §1:

- **The 13.8 s before the reopen is 12.2 s of detector plus ~1.5 s of control
  ask.** `stall_ms=12179` is the detector (6 stagnant 2 s checks,
  `establishedReopenChecks`, 1317). The log line is written *after*
  `controlVerdictForStall` (4707 → 4736), whose bound is
  `controlAskSeconds = 1.5` (4782). The hold verdict was then refused by
  `holdMayDecideStall` because runway 62 s > 10 s (4967–4971), and the reopen
  went ahead. Say so — it is the same 1.5 s that C below has to account for.
- **The replacement was not the same presentation.** Row 66715:
  `session_start method=remux encoder=vod {"presentation":"vod"}`; the TTFF
  row is `method=transcode … encoder=vod`. The same-delivery repair landed on
  the VOD-cache presentation, not a fresh rolling copy session. 4.3 s is the
  cache's TTFF. §8's comparator must record which presentation a *fresh open
  now* actually gets and compare like with like (see L).
- **A `429 control_rate_limited` followed 14 s after the repair** (row 66722,
  `hls.rs` 6134/6965 — per-session control budget). Not caused by this
  incident's toggle, but it means §4.2's urgent exchange has to be one per
  real transition; a Play/Pause storm must not queue exchanges.

Worth one sentence in the doc because it is why a client-only fix is correct
here: at 03:21:52 the hold moved Demand→Time with `ahead 68 s ≥ release 63 s`,
so the server would not produce until the client consumed ~5 s, and the client
would not consume until the playlist grew. `recovery_outranks_hold`
(`playback_control.rs` 2009–2017) does not break that: with runway > 10 s and
a `stalled` report it only *withholds the hold instruction* — it neither
resumes the producer nor plays the client's media, and the client does not
report `stalled` until its own 12 s detector fires. The client holds the
media, so the client breaks the deadlock. Record the shape so nobody later "fixes"
the client by re-enabling the wait.

### B · The 15 s ceiling must not be applied to cold startup (P1, design conflict)

§5.2 says "Apply the same deadline primitive to on-demand cold startup as to
resume." At the pinned base that collides with three things:

1. **The cross-client create-retry ladder.** `PlaybackCreateRetry` (1542–1562)
   is `[1, 2, 4] s` with an **absolute 60 s** deadline; it is the playback-surface
   contract's `create_503_not_yet` row, and
   `tests/playback/web-policy.test.js` 5624–5629 reads those numbers back out
   of the Swift and fails if Apple drifts from web/Android. A 15 s cold-start
   ceiling cannot be honoured without changing all three clients and the
   fixture — which the handoff's own §9 forbids.
2. **The unestablished-item leash.** `unestablishedReopenChecks = 15` (≈30 s,
   1318–1325) exists precisely so a first attach or fresh successor "may
   legitimately buffer for a while before its first frame … long enough not
   to duplicate a cold start" during the server's publish gate.
3. **The lifecycle plan on main.** `docs/playback-control/PLAYBACK-LIFECYCLE-IMPLEMENTATION.md`
   §4.4 (563–565): "Retain Apple's existing 20-second absolute stall-deferral
   ceiling as an initial upper bound." Two main-bound documents would then
   state two different bounds for the same owner.

Fix: scope the 15 s whole-attempt ceiling to **resume of an established,
already-attached item** (which is the incident) and to the successor that
resume's single repair creates. Leave cold-start budgets where they are, and
strike the "same primitive for cold startup" sentence — or, if Paul wants one
latency contract for both, that is a ruling for him, taken to the lifecycle
plan first, not smuggled in through an Apple-only handoff.

The `itemReadinessDeadlineSeconds = 15` (1745) the handoff cites as a
"reference point" only bounds VOD/direct fresh starts with no seek
(`shouldBoundFreshStartReadiness`, 1779–1786); it never applied to a rolling
session. Do not let the number's coincidence carry the argument.

### C · The fast-path repair arithmetic omits the control ask (P1, timing)

§5.2's worked example (1 s + 4.3 s = 5.3 s) assumes the repair starts at the
1 s deadline. At the pinned base every timer-only repair goes through
`retrySameDeliveryAfterStall(consultControl: true)` (4688–4720): a 1.5 s ask,
extended to 3 s if the reporter is mid-exchange, before `reopen` is even
called. Real cost: 1 s + 1.5–3 s + 4.3 s ≈ 6.8–8.3 s against a 4.3 s
comparator. The incident's own 4.3 s TTFF was measured *after* the ask
(`ttffMeasurement.opened` at 4740 runs after 4707 returned).

Fix: the resume attempt's admitted repair calls
`retrySameDeliveryAfterStall(…, consultControl: false)`, the way the seek
presentation timeout already does (6159–6167). Nothing is lost: with retained
runway > 10 s a `hold` verdict cannot decide anyway (4967–4971), and a
`retry_resource` would only defer a repair the client has already decided to
make. State the resulting budget honestly: 1 s + repair. If the comparator is
still missed, §5.2's "record the overhead and leave acceptance open" clause
stands.

### D · Use the seams that exist; do not add `playerChanged(urgent:)` (P2, design)

§4.2 proposes extending `PlaybackControlSession.playerChanged()` with an
`urgent:` flag. `PlaybackControlSession.swift` already has both halves:

- `reportEvidence()` (358–361) — `notifyUrgently` for a recovery owner.
- `reportIntent() async -> UInt64?` (366–371) — "Publish an interactive
  intent before the media mutation it authorizes", returns the sequence floor.
  Every seek already does `retainControlSequence(await playbackControl.reportIntent())`
  (2980, 3115, 3238, 3378), and `open()` folds `pendingControlSequence` into
  the create body (3934–3940) so the server orders the intent before the
  session it authorises.

Pause/Resume is an interactive intent that may authorise a repair create. It
should take the same path: `reportIntent()` + `retainControlSequence`, so the
resume's `active` is sequenced ahead of the successor's create. A fire-and-forget
`playerChanged(urgent:)` returns no floor and leaves that ordering to luck.
Today `togglePlayPause` calls `playbackControlPlayerChanged()` (2815 → 8484 →
`playerChanged()` 343), which is the coalesced path — that is the 4.8 s in the
timeline, and the lifecycle plan's L13 obligation ("resume promptly sends
active even with native rate zero", `PLAYBACK-LIFECYCLE-IMPLEMENTATION.md`
603) is simply unmet at main. Say that; it is the cleanest statement of the
defect.

Also in §4.2: "A buffering rate of zero must not overwrite the saved rate" is
already true — `preferredRate` is only written when
`timeControlStatus == .playing && rate > 0` (6229–6231, 6264). Word it as an
invariant to preserve, not behaviour to build.

## Findings to incorporate

### E · The existing nudge is inert; make the `playImmediately` upgrade mandatory (P2)

The recovery monitor's `.nudge` (4668–4671) is `player.play()` plus a rate
restore. With `automaticallyWaitsToMinimizeStalling = true` that is a no-op in
both regimes: `play()` while `.waitingToPlayAtSpecifiedRate` changes nothing,
and while `.playing` with a stuck clock it changes nothing either. The
incident shows it: the item was established (30 s played before the pause),
so by the detector's arithmetic (`establishedNudgeChecks = 3`, 1316) the
nudge fired at ~6–8 s of stagnation, and the player stayed waiting to 12 s —
the nudge is not logged, so this is arithmetic, not a timestamp. `PLAYBACK-LIFECYCLE-STATUS.md` (158) records "Apple retains its one
pre-ask nudge" as the loaded-media reevaluation; it never was one. §4.4 says
the nudge "may" reuse the guarded immediate-play — make it "must", under the
same runway qualification, and update the STATUS row. Otherwise the ordinary
stall path keeps a known-dead reevaluation while the explicit-resume path gets
a live one.

### F · Presentation proof: the pre-pause frame will pass the current plausibility test (P2)

§5.3 says to reuse the seek presentation machinery. Two properties of that
machinery bite a plain resume:

- `PlayerSeekState.isPlausibleLanding` accepts `delta ≥ −250 ms` (834–837), so
  a frame at exactly the paused position — the one still on screen —
  satisfies `presentedVideo` (852–857).
- `AVPlayerItemVideoOutput.hasNewPixelBuffer` is "new" relative to the last
  `copyPixelBuffer`, and the seek monitor only copies during seeks
  (6108–6125). After a pause, the pre-pause frame can report as new.

The §6 case "Only the pre-pause frame is available → does not settle" is
therefore not satisfiable by reuse alone. Specify the mechanism: at resume
start, baseline the output (one `copyPixelBuffer` at the current item time, or
record the paused `displayTime`) and require the settling frame's own
`displayTime` to be strictly later than the paused position by at least one
frame interval.

And require *continuing* motion, not one frame. With the §4.4 threshold of one
wall-second of runway, `playImmediately` can present a frame, settle the
attempt, drain 1 s and re-enter the ordinary 12 s established-stall path — a
worse outcome than an immediate repair. Either raise the qualifying runway
(the wedge logic already treats ≤ 10 s as "runway gone",
`DeliveryStarvationDetector.runwayCeilingSeconds`, 1064) or settle only after
the surface model's `presentingContinuousMs` notion of continued presentation,
which the presenter already computes. §8's pass criterion says "continuing
motion"; the code's settle rule should say the same.

### G · Source-text fences will trip on the §4.2 refactor — plan the update (P2)

`Tests/AppleClientTests.swift` 3745–3771 pins the §3.4 single-stop-helper
property by spelling: exactly one hand-written
`player.pause() / isPlaying = false / wantsPlayback = false` block, and it
must sit inside `func togglePlayPause() {`; and exactly **5** writers of
`wantsPlayback = false`, two of which are "the on-screen transport and the
lock screen". Routing the iOS `pauseCommand` (8257–8265) through a shared
`setPlaybackRequested(_:)` and moving the by-hand stop out of `togglePlayPause`
changes both counts. The comment at 4140 ("The lock-screen `playCommand`
still calls `player.play()` directly … the contract's answer is to record
it") also becomes false once `playCommand` (8248–8256) is routed. These are
the right changes — the iOS remote handlers today skip `beginViewerAction`,
the rate restore, the reporter and the pending-seek drain — but §7.2 must say
the fence and the comment are updated deliberately in the same commit, or M1
fails on a test the handoff told the implementer to ignore ("Do not use
source-text spelling assertions as the proof").

### H · Process: do not build in Paul's checkout (P2)

§4.1's `git worktree add … origin/main` from the originating checkout adds a
worktree to *his* `.git` — his rule is never to work in his clones (and that
clone has `core.bare=true` set and is 679 commits behind at 10f2afe60; its
`origin` is Forgejo over ssh with his key). The receiving session clones from
Forgejo with the token in `~/code/plurx-agent/github_token` into its own disk
(`/tmp/agent/<name>`, `--filter=blob:none`), per the repo's agent-access
notes. Also: `docs/apple-builds/README.md` keys the per-build fragment by
**issue number**, so M4 needs a Forgejo issue filed first; the handoff does
not say so.

### I · Name the actual single-replacement fences (P3)

§4.1 item 4 says `recoveryReopenBudget` prevents a second observer minting a
second replacement. It is a storm cap — 3 automatic reopens per 60 s
(1116–1118) — and permits three. The fences the handoff means are
`PlayerReopenQueue` (569–605, never two server-session replacements in
flight), `openGeneration` (2259, P2-6), and
`SameDeliveryStallRecoveryState.attempted` (1009–1017, one timer-only repair
until 5 s of established playback rearms it, 6269–6270). §5.4's "at most one
repair per resume attempt across all detector labels" is largely already the
`attempted` flag — delivery starvation, the deferred-stall expiry, the seek
presentation timeout and the timer stall all funnel through
`retrySameDeliveryAfterStall` and its `next(for:)`. What does *not* share it
is `handleItemFailure`'s compatibility ladder. Say which is which so the
implementer extends rather than re-invents.

### J · Decide whether Pause is a "viewer action" for the prepared coordinator (P3)

`togglePlayPause` calls `beginViewerAction()` (2799), which does
`preparedReplacement.abandonWithoutFallback(.aborted)` (2789) — every Pause
and every Resume aborts a staged M6 quality successor and sends the server
`aborted`. §4.1 invariant 2 tells the implementer to preserve that. It is
probably wrong for a pause (the viewer has not "moved on from the
destination"), and it also bumps `viewerActionEpoch`, which drops any
in-flight `retrySameDeliveryAfterStall` ask (4712–4716). The handoff's own
design makes the resume attempt the recovery owner, so the epoch bump is
fine for recovery; the M6 abort deserves an explicit ruling in §4.2 rather
than inheritance.

### K · Tie it to the lifecycle effort (P3)

The handoff names `PLAYBACK-LIFECYCLE-COVERAGE.md` as a companion but not
`PLAYBACK-LIFECYCLE-IMPLEMENTATION.md`, whose §1 (313–316) already recorded
"substantial loaded buffers while the rolling producer was time-held" as an
observed Apple TV wait, whose §4.4 row 3 assigns it to "one native
play/readiness reevaluation" by the client owner, and whose L12–L14 row (603)
mandates the urgent `active`. This incident is that open item with the
decisive client-side reason attached. Cross-reference both, and add the
STATUS ledger update to M4's deliverables.

### L · §8 comparator: match the presentation, not just the recipe (P3)

Per A, the incident's repair served from the VOD cache. A "fresh open" of the
same file at the same position today may also serve from the cache (4.3 s) or
may build a rolling copy session (5.1 s in this incident). §8 should record,
per measurement, `encoder`/`presentation` from the `session_start` row and
compare resume-to-cache with open-to-cache, resume-to-rolling with
open-to-rolling. Otherwise a resume that lands on the cache "beats" a cold
rolling open for reasons that have nothing to do with this fix.

## Anchors checked and found correct

Everything else the handoff asserts about the base resolves: `wantsPlayback.didSet`
→ `.playbackRequested` (2279–2284); `beginViewerAction` → `.intentSuperseded`
+ `clearVerdict` + `deferredStall = nil` (2777–2791); timer-only recovery is
`.sameDeliveryRepair` (4747–4750); attach starts the owned item before
readiness and honours the latest pause (4150–4151, with the tvOS rate-0
rationale in the comment); `automaticallyWaitsToMinimizeStalling = true`
(2595, 2647); the 60 s growing-HLS forward buffer applies to this session
shape (`configureBuffering(growingHLS: sessionId != nil && !isVOD)`, 4113;
the incident session was `cached=false`, hence 62 s buffered);
`bufferedRunwaySeconds` contiguity semantics (2398–2438); 2 s health cadence
and the 20 s advisory deferral (4784–4786); 100 ms-independent presentation
sampling is already the seek monitor's 50 ms loop (6171); Apple build 167
(`project.yml` 18); schemes/targets `plurx-iOS(Tests)` / `plurx-tvOS(Tests)`
and the simulator names match the Makefile; `validation.apple_build
--merge-target` is what `make apple-build-bump` runs; `scripts/ship-physical
--apple --build-only` and `scripts/ship --apple --dry-run` exist with those
flags; `DEVELOPMENT_PIPELINE.md` 3–11 carries the draft → one adversarial
review → ready rule; all cited docs and test files exist; the docs-index row
in Paul's tree is well-formed. The P1 "Play during a pending seek starts the
predecessor" finding is real at main, not only in the prototype:
`togglePlayPause` calls `player.play()` unconditionally (2809) and only then
drains `seekState.pendingMs` (2816–2820), and during `isChangingStream` the
paused predecessor is still `currentItem` until 4128.

The 46-test prototype result and the Xcode 27.0 / tvOS 26.5 details are not
verifiable here (the prototype was removed) and the handoff correctly gives
them no weight.

## Ledger for the receiving session

| Item | Disposition |
|---|---|
| A · cite the stored snapshot; fix the three timeline readings | required, doc-only |
| B · scope the 15 s ceiling to established-item resume; drop cold-start | required, or Paul's ruling |
| C · resume repair is `consultControl: false`; restate the budget | required |
| D · `reportIntent()` + `retainControlSequence`, no new seam | required |
| E · nudge becomes guarded `playImmediately`, STATUS row updated | incorporate |
| F · strictly-after-pause frame + continuing motion as the settle rule | incorporate |
| G · update the AppleClientTests fence and the 4140 comment in M1 | incorporate |
| H · own clone from Forgejo; file the issue before the build fragment | incorporate |
| I · name `reopenQueue` / `openGeneration` / `attempted` as the fences | incorporate |
| J · rule on Pause vs. staged M6 successor | ruling wanted |
| K · cross-reference the lifecycle implementation/status | incorporate |
| L · comparator records presentation per measurement | incorporate |
