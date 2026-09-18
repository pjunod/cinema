# TRON held-seek stall — two commits, one source change, one recovery spent too early

**Status:** implementation candidate complete; review and fast lane pending, 2026-09-18  
**Incident:** 2026-09-18, approximately 04:17–04:25 UTC
**Incident build:** `v0.3.0-2770-g6fb0901d`
**Exact source:** `6fb0901d3d18c1b181f7299f4faddfb73994fd1a`

Holding Right Arrow while watching *TRON: Ares* split one physical key hold
into two client seek commits. The first commit was superseded before it opened
a server session or attached media; it did not cause the stall. The final
attachment received media and built 9.6 seconds of runway, but recovery still
classified it from the zero-second runway captured at the start. Merely
resampling that value would change the label and call `play()`; the shipped
controller would still reopen immediately. The outcome defect is therefore a
missing presentation-observation interval: a refilled attachment spends the
single reopen at the eight-second supply deadline instead of receiving the
remainder of the existing 20-second absolute deadline to prove progress.

This document records the evidence, causal limits, implemented repair contract,
and tests prepared for review. It complements
[WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md](WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md),
[PLAYBACK.md](../PLAYBACK.md), and
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md). Deployment remains pending the
review and validation sequence in §10.

## 1. Finding — the two defects are independent

The investigation confirmed two client defects, but only the second belongs
to the causal path from the final attachment to `owner_exhausted`:

1. `nudge()` commits after 350 ms of quiet. A held key's first repeat can
   arrive after that deadline, so the initial keydown committed one seek and
   the later repeat events committed a second. The contract faithfully says
   to commit after that much quiet; its defect is using quiet time to model a
   physical key hold when common initial-repeat gaps exceed 350 ms.
2. The final attachment began waiting with no runway, then accumulated
   9.6 seconds. Four recovery and adaptation paths retained or consumed the
   stored start-of-wait value. `persistentWait()` called the wait `supply` and
   reopened at eight seconds. A current-runway resample alone is insufficient:
   the presentation branch also falls through to the same reopen after a
   passive control answer. A loaded presentation must instead observe actual
   clock/frame progress through the remaining absolute deadline.

The two branches are separate:

```text
one held Right Arrow
        |
        +-- 350 ms quiet timer fires before initial key repeat
        |       `-- commit a3
        |               `-- superseded client-side
        |                       `-- no producer, no attachment
        |
        `-- repeat events arrive, then go quiet
                `-- commit a4 at 93.46 s
                        `-- one source change: a2 -> a4
                        |
                        `-- a4 does not present before 8 s recovery
                                |
                                +-- wait begins at 0.0 s runway
                                +-- buffer later reaches 9.6 s
                                +-- recovery still labels it from 0.0 s
                                `-- no remaining-deadline observation
                                        |
                                        +-- reopens same recipe as a5
                                        `-- repeats, then owner_exhausted
```

The server was not starved at the first recovery deadline. The producer was
being held with about 28 seconds and 213 MB ahead, and the browser reported
9.6 seconds buffered. The unresolved browser-level detail is why a4 did not
begin presenting those loaded bytes. The duplicate a3 commit did not replace
the media element, and a later clean single seek began with the same zero
runway but presented. Do not use the split to explain the a4 presentation
failure.

## 2. Impact and boundaries

The viewer saw a blocking “Presentation waiting” surface and had to abandon
the attempt. The held-key burst caused an avoidable client attempt and
`preparing` surface, but no intermediate producer or attachment. The final a4
attachment was reopened as a5 before the 20-second deadline, and a5 exhausted
the one-reopen limit. A later cold start and a single seek presented
successfully.

This RCA makes the following causal distinctions:

- A pending VOD index put Safari on the temporary growing live-HLS path. It
  was a prerequisite for the restartable delivery path, not the root cause.
- The a3 attempt left no server session at 63.46 seconds. Pending-open
  supersession prevented its create from becoming a producer or attachment.
- The first segment response on each stalled attachment ended after a partial
  transfer. That is consistent with replacement or client cancellation; the
  retained evidence does not prove a server-side segment failure.
- A playback-control request was rate limited, then immediately recovered
  with legacy recovery still authoritative. Media delivery was independent,
  so the rate limit did not cause the stall.
- The recently deployed frame-callback change identifies the running build.
  No evidence ties that change to this incident.
- Background metadata classification lease exhaustion was concurrent noise,
  outside the playback chain.
- The server's log label identified the client as Safari. This RCA does not
  infer undocumented browser internals from that label.
- A predecessor frame cleared the `preparing` surface almost immediately on
  a3, a4, and the later successful seek. That surface-ownership defect made
  the freeze less visible before `waiting`; it is a separate follow-up.

Non-goals:

- Do not raise control-plane rate limits as a substitute for fixing duplicate
  seek commits.
- Do not add an unbounded reopen loop or weaken the existing one-reopen limit.
- Do not call loaded-but-stationary media a decoder failure without typed
  decoder evidence.
- Do not treat every zero-runway wait as permanently supply-bound after media
  arrives.
- Do not claim that `play()` on an already-playing element repairs this
  incident. It is a harmless reevaluation, not the load-bearing recovery.
- Do not restart media1, deploy a build, or modify media as part of review.

## 3. Evidence and provenance

### 3.1 Source provenance

The deployed SHA is the 2026-09-18 00:37:10 UTC merge of PR #358, “Fix frame
callback registration across media load and seek.” `origin/main` resolved to
that exact SHA during diagnosis. Symbol names below refer to that source, not
to unrelated local working-tree edits.

The relevant deployed paths are:

- `crates/plurxd/src/web/index.html`: `handlePlayerKeydown()`,
  `applyPlayerOutcome()`, `nudge()`, `commitPendingSeek()`, `beginWait()`,
  `persistentWaitEvidence()`, `persistentWait()`, `playbackProgressTick()`,
  `activeSupplyStall`, hls.js `bufferStalledError` handling, and
  `liveTvChangeChannel()`.
- `crates/plurxd/src/web/playback-policy.js`: `stallRecoveryAction()` and the
  one-automatic-recovery bound.
- `tests/playback/player-input-contract.json`: the 350 ms desktop hotkey
  coalescing promise.
- `tests/playback/player-input-contract.test.js`: the numeric Live TV
  coalescing assertion that must become behavioral coverage.
- `tests/playback/web-control.test.js` and
  `tests/playback/web-policy.test.js`: the current recovery coverage.

### 3.2 Incident timeline

Times are UTC on 2026-09-18.

| Time | Observation | Meaning |
|---|---|---|
| 04:17:58.826 | Initial attachment presented its first frame after 12.7 s with 12.5 s runway | The title and copy-HLS recipe could present before the seek |
| 04:18:52.495 | Seek attempt `a3` entered `preparing` | First client commit from the held arrow; later superseded before attach |
| 04:18:53.106 | Seek attempt `a4` entered `preparing`, 611 ms later | Second commit; the only seek producer and attachment |
| 04:18:56.506 | `a4` first segment response ended after 212,992 of 72,237,690 expected bytes | Partial response observed; cause not independently typed |
| 04:19:03.788 | `a4` persistent wait recorded 8.0 s and 0.0 s starting runway | The wait began empty |
| 04:19:04.023 | Recovery reconnected as `supply`; context showed 9.6 s current runway | Media had arrived, but recovery used the starting runway and did not wait through the 20 s deadline |
| 04:19:14.864 | Recovery attachment `a5` recorded another 8.0 s wait from 0.0 s | Same recipe repeated the failure |
| 04:19:15.126 | Recovery raised `owner_exhausted` | The one permitted automatic reopen was spent |
| 04:19:24.296 | Blocking surface cleared by user action | Viewer abandoned the failed attempt |
| 04:25:20 | Old session released and retired | No playback producer remained permanently wedged |
| 04:25:35 | Cold-start attachment `a6` presented after 12.6 s | Fresh playback still worked |
| 04:25:46.560 | Single-seek attachment `a7` cleared a 2.9 s wait by presenting | A non-burst seek recovered normally |

### 3.3 Supply evidence

The final user seek opened at 93.46 seconds. Its copy-HLS producer used the
expected offset and validated its initialization segment. At the first
persistent-wait deadline the server reported:

| Signal | Observed value |
|---|---:|
| Buffered runway when wait began | 0.0 s |
| Buffered runway at recovery | 9.6 s |
| Published reserve observed after hold | 28 s |
| Published bytes ahead | 213,131,037 |
| Last production time ahead before decision | 38 s |
| Last target time ahead before decision | 40 s |

Those values rule out ongoing server starvation at the recovery decision.
They do not prove why Safari failed to advance presentation.

The a4 and a5 first segments were both 72,237,690 bytes at the same offset;
the a5 session reported four ceiling cuts and no clean cuts. At this title's
approximately 60 Mb/s rate, 72 MB is about 9.6 seconds of media: the loaded
runway at the deadline was one oversized first segment. The later successful
seek's first segment was 24 MB, and the cold start's was 10.6 MB. This is a
testable lead about native HLS startup from a growing playlist, not a finding
about Safari.

### 3.4 Code-to-event match

The first keydown calls `nudge(10)`. `nudge()` freezes the current position,
updates `_seekPending`, and arms `desktop_hotkey_coalesce_ms`, currently
350 ms. It does not know whether the key remains physically down. The separate
`keyup` listener only resets repeat acceleration; it does not own the pending
seek. The current tests stub `setTimeout()` to capture a callback; they do not
model time or a repeat gap at all.

A fake-clock replay against the deployed functions reproduced the incident.
Starting at 53.46 seconds with a 375 ms initial repeat delay and 90 ms repeat
cadence, the client committed 63.46 seconds at +350 ms and 93.46 seconds at
+905 ms. A 500 ms initial delay also split; a 225 ms delay did not. The server
retained no producer or attachment at 63.46 seconds because the pending-open
owner superseded a3 before it completed.

The wait can enter through `beginWait()` or through the seek-landing progress
watch. Both store `p.waitRunway`, and the latter calls `persistentWait()`
directly. Eight seconds later `persistentWait()` assigns
`runway=p.waitRunway` and classifies that value. A `presentation` result calls
`v.play()` and logs `native_reevaluation:wait`; the stale zero instead
produced `supply`.

The resample is necessary but not sufficient. A replay with only
`runway=bufferRunway(v)` produced `presentation-persistent`, one `play()` call,
and then the same-recipe reopen after the passive control answer.
`stallRecoveryAction()` returns `restart` for an unattributed remux in either
classification. The repair must defer that fallthrough while a loaded
presentation still has time inside the existing 20-second absolute deadline.

`stallRecoveryAction()` permits one restart and returns `prompt` when
`alreadyRecovered` is true. That bound behaved as designed and must remain.

### 3.5 A second incident confirms the spent-recovery edge

Build `v0.3.0-2780-ga5454c40` reproduced the outcome on another native-HLS
title after a brief quorum loss had already consumed the player's one reopen.
The later seek entered an 8.1-second `supply-persistent` wait and raised
`owner_exhausted` immediately because no reopen remained. The producer had
materialized and parked its demanded window, but that run did not retain the
current client runway; it does not prove refill or the held-key split.

There is no relevant web or test diff between the original incident SHA and
`a5454c40`. The deployed code still reads `p.waitRunway`, immediately falls
through from a passive presentation answer, commits `nudge()` after 350 ms,
and leaves keyup outside seek ownership. This second incident is acceptance
evidence for the previously-spent-recovery case, not additional proof of the
TRON input branch.

## 4. Root cause — a refilled presentation spent recovery before its deadline

### 4.1 Sibling defect: quiet-time debounce did not model a held key

The desktop contract says to accumulate against a frozen base and emit one
seek after 350 ms of quiet. The implementation satisfies those words, but the
words model held input incorrectly: an initial repeat gap longer than 350 ms
looks like release even though the key remains down. In this incident the
second `preparing` event arrived 611 ms after the first, matching the
fake-clock replay and the split into two client commits.

The exact browser event timestamps were not retained, so the OS repeat delay
is an inference from the reported gesture, two client commits, and the timer
contract. Server evidence rules the first commit out of the stall path: a3
created no producer and attached no media. The deterministic test in §6 must
reproduce the split directly.

### 4.2 Misclassification: recovery froze a changing observation

Starting runway is useful episode telemetry, but it is not a durable cause.
A wait can begin empty, receive media, and remain stationary. At that point it
has changed from a supply wait into a presentation wait unless a typed media
error says otherwise.

The controller correctly gives typed decoder and network errors precedence,
but it applies the runway fallback to the start-of-wait snapshot. The incident
therefore kept the original `supply` label after its factual basis had ended.
That suppressed the native `play()` reevaluation and misled ABR and hls.js
stall accounting. It did not, by itself, select a different reopen: both the
shipped supply path and the resampled presentation path fall through to the
same `restart` action for this unattributed remux.

### 4.3 Outcome defect: presentation got no remaining-deadline observation

The controller reopened a4 about 8.8 seconds after its producer started. Two
cold starts on this title needed about 10.1–10.2 seconds from producer start to
first frame, while the later successful seek needed 3.9 seconds. That timing
makes additional observation plausible, not proven, as the missing recovery.

The shipped presentation branch calls `v.play()` and then reopens after a
passive control answer. On an element already considered playing, that call is
not evidence of progress and is unlikely to change presentation. The existing
progress watch already knows the real success condition: media time and,
where available, presented frames advance.

A refilled presentation wait must therefore observe that condition for the
remainder of `CONTROL_STALL_DEFER_DEADLINE_MS`. If progress arrives, the
existing post-await guard retires the episode. If the absolute 20-second
deadline expires without progress, the controller may spend its one reopen.
The one-reopen limit prevented an infinite loop and must remain; the defect
was reaching it before the loaded attachment's bounded observation ended.

## 5. Repair contract

### 5.1 One held desktop arrow owns one seek

Replace quiet-time ownership for keyboard arrows with gesture ownership:

1. The first eligible Left or Right keydown opens a pending keyboard-seek
   gesture and freezes its base position.
2. Repeat keydowns update the pending target and preview. An opposite arrow
   joins the same gesture against the frozen base, so Right then Left nets out
   on one visible position and one commit.
3. Keep the 350 ms quiet timer, but let it commit only when the owning key is
   already up. If it fires while the key is down, record that the quiet
   deadline elapsed and wait for keyup.
4. Matching keyup commits immediately when the quiet deadline already
   elapsed. If keyup happens first, the existing timer commits once after the
   remainder of its window.
5. Window blur commits the visible pending target because the scrubber already
   promised that destination. Player close and attachment replacement cancel
   because their ownership ended.
6. Pointer skip buttons retain their current quiet-time behavior and do not
   share keyboard ownership state.

The input contract and implementation must describe that lifecycle. A long
initial key-repeat delay must not create an intermediate `seekTo()`.

### 5.2 Live TV held-channel input uses the same ownership rule

`liveTvChangeChannel()` and `channel_coalesce_ms` make the same incorrect
assumption: 350 ms of quiet is treated as the end of a held Channel Up or
Channel Down gesture. Apply the same key-held lifecycle so one physical hold
starts one tuner session at the final previewed channel. The existing
`channel_coalesce_ms >= 350` assertion is not a valid held-key guarantee and
must be replaced with behavioral fake-time coverage.

This is a sibling-risk repair, not part of the TRON incident chain. Leaving it
unchanged would preserve the same session-storm defect on another surface.

### 5.3 Reclassify from current evidence, then observe presentation

Keep both values with distinct names:

- `waitStartedRunway`: immutable episode telemetry.
- `currentRunway`: resampled immediately before every live classification,
  adaptation, and recovery decision.

Typed media errors continue to outrank runway. Without a typed error:

- current runway below `SUPPLY_RUNWAY_SECS` is a supply wait and keeps today's
  eight-second recovery decision;
- current runway at or above the threshold is a presentation wait, even if
  the episode began empty.

The one-line resample is not the outcome fix. A presentation wait must:

1. Log both runway values and perform the existing native `v.play()`
   reevaluation once.
2. Avoid falling through to `stallRecoveryAction()` in the same callback.
3. Re-arm observation for the remaining portion of the existing 20-second
   absolute deadline, using actual media-time and frame progress as success.
4. Reuse the shipped post-await generation guard: progress already clears
   `waitAt`, so a late passive answer cannot reopen a recovered episode.
5. Spend the single same-recipe reopen only if the absolute deadline expires
   without progress.

Do not add another 20-second interval after each callback. The deadline stays
absolute from the original wait start, and the one-automatic-reopen bound
stays unchanged.

### 5.4 Every stale-runway reader gets an explicit input

| Reader | Required runway | Reason |
|---|---|---|
| `persistentWait()` | Current | It chooses the live wait kind and recovery timing |
| `playbackProgressTick()` seek-landing path | Store start value, then let `persistentWait()` resample | This path bypasses `beginWait()` and was eligible in both a4 and a5 |
| `activeSupplyStall` feeding `decideRung()` | Current | Refilled media must not keep steering ABR as active starvation |
| hls.js `bufferStalledError` handling | Current when a wait is open | hls.js accounting must describe the buffer at the event, not eight seconds earlier |
| `endWait()` resumed-stall telemetry | Start value | Retrospective telemetry intentionally describes how the episode began |

Do not implement the repair only in `beginWait()`. The seek-landing progress
watch writes the same snapshot and calls `persistentWait()` directly.

### 5.5 Partial-response telemetry makes the browser lead testable

For a first-segment response that ends early, record the expected and delivered
bytes, segment media duration, start offset, response incarnation, producer
attempt, cut class, and whether producer supersession was observed. A dropped
body does not prove whether the browser cancelled or replaced an attachment;
that client disposition remains explicitly unknown unless a future client
signal supplies it. This must not turn an untyped partial response into a
server-failure verdict. It exists to separate observed in-session replacement
from unclassified transfer loss and to compare oversized first segments with
successful starts.

### 5.6 Preserve ownership and fencing

All delayed key and wait callbacks must remain fenced by the current player,
attachment, seek generation, and control-intent generation. Closing the
player, starting a newer seek, pausing, or changing attachments must prevent
an old callback from committing or recovering.

## 6. Required tests

### 6.1 Keyboard gesture regressions

Add deterministic fake-time coverage for:

1. Right keydown at `t=0`, 375 ms initial delay, 90 ms repeat cadence, then
   matching keyup: zero seeks before the owning commit and one seek at the
   final accumulated target.
2. The same test with a 500 ms initial delay, and with a 225 ms initial delay:
   all profiles produce one commit despite crossing the timer differently.
3. A short Right tap: keyup marks release and the timer commits once.
4. Blur mid-hold commits the visible target once. Player close and attachment
   replacement cancel it, and stale timer callbacks do nothing.
5. Right then Left joins one gesture against the frozen base and commits the
   net preview once.
6. Pointer skip activation retains its current behavior independently of the
   keyboard gesture owner.
7. Held Channel Up and Channel Down use the same repeat profiles and start one
   tuner session at the final previewed channel.

Update `player-input-contract.json` so the test reads the contract value and
lifecycle rather than preserving the defective 350 ms claim for keyboard
holds. Replace the Live TV assertion that a number `>= 350` proves coalescing;
only behavioral input timing can prove one tuner start.

### 6.2 Wait-classification regressions

Rewrite `stallHarness` so `bufferRunway()` derives its answer from a fake
video's `buffered` ranges and `currentTime`. Do not inject a detached
`options.runway` scalar, and rewrite the existing presentation cases that set
`waitRunway: 22` or `waitRunway: 8`; those fixtures currently pass without
the shipped controller ever consulting the video.

Then add named coverage for:

1. **`refilled wait resamples runway before classification`.** The wait stores
   0.0 s, the fake video exposes 9.6 s at the eight-second callback, and no
   typed media error exists: classify as presentation, log both values, call
   `v.play()` once, and do not reopen in that callback.
2. The presentation wait receives a passive control answer and re-arms only
   for the remainder of the 20-second absolute deadline.
3. Media time and frames advance during that interval: clear the episode and
   do not reopen. Exercise the existing post-await guard rather than adding a
   second progress owner.
4. The 20-second deadline expires without progress: spend exactly one reopen.
5. Current runway remains 0.0 s at eight seconds: classify as supply and keep
   today's bounded restart.
6. The seek-landing progress watch enters `persistentWait()` without
   `beginWait()`: it still resamples current runway.
7. `activeSupplyStall` becomes false after refill, and hls.js
   `bufferStalledError` uses current runway while the wait is open.
8. `endWait()` retains the start runway for resumed-stall telemetry.
9. A typed network error with loaded media retains network/supply attribution;
   a typed decoder error with empty media retains decoder attribution.
10. The replacement attachment stalls after one automatic recovery: raise
    `owner_exhausted` without a second reopen.
11. Telemetry carries start runway, current runway, chosen kind, attachment,
    and seek generation without reading a successor attachment.

Add a mutation acceptance check: reverting the live resample to
`runway=p.waitRunway` must fail test 6.2.1 by name. Also verify that applying
only `runway=bufferRunway(v)` still fails the no-immediate-reopen assertion;
that prevents the telemetry fix from masquerading as the outcome fix.

### 6.3 Integration regression

Drive a synthetic growing copy-HLS source through a held Right Arrow whose
initial repeat gap exceeds the old 350 ms timer. Client evidence must show one
seek commit, and server evidence must show one session creation at the final
target. Do not use the incident's lack of an a3 server session as proof that
duplicate client commits are acceptable.

For a loaded attachment that remains stationary, client evidence must show
current-runway reclassification, one native reevaluation, and progress
observation through the remaining absolute deadline before any reopen. Record
first-segment duration and byte size beside the result.

## 7. Validation and acceptance

The implementation pull request must record the exact base SHA and run at
least:

```text
node tests/playback/player-input-contract.test.js
node tests/playback/web-policy.test.js
node tests/playback/web-control.test.js
node tests/web/live-tv.test.js
python3 -m unittest tests.operations.test_docs_index
git diff --check
```

Run the repository's affected-surface validation required by
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) for any code actually
changed. Do not report this RCA's document checks as implementation evidence.

Physical acceptance on a current build requires three held-arrow attempts on
the Safari/native-HLS path:

- Hold Right Arrow past the platform's initial repeat delay and release after
  previewing several minutes ahead.
- Observe exactly one client seek commit and one new media session at the
  final target.
- Confirm a refilled presentation wait is observed through the remaining
  absolute deadline and does not reopen while progress is arriving.
- If a wait begins empty and later refills, confirm logs show both runway
  values, one native reevaluation, and the segment's duration and byte size.
- Confirm a genuinely empty wait still uses one bounded automatic recovery,
  then prompts rather than looping.
- Hold Channel Up past the initial repeat delay and confirm one tuner session
  starts at the final previewed channel.

## 8. Review decisions incorporated

The first review resolved the open policy choices:

1. **Blur commits the visible target.** The preview is a user-visible promise.
   Player close and attachment replacement cancel because ownership ended.
2. **Opposite arrows join one gesture.** Netting against the frozen base keeps
   one preview and prevents an intermediate `seekTo()`.
3. **Presentation gets the remaining observation interval.** `play()` alone
   is not the repair; actual clock/frame progress inside the existing
   20-second deadline is.
4. **Partial-response telemetry records size, duration, and truthful server
   ownership.** The server records response incarnation and producer attempt,
   but does not relabel an unclassified dropped body as a client cancellation.

## 9. Current disposition

Diagnosis changed no service, media, or deployment. The old session retired
after the client released it, and later cold-start plus single-seek playback
succeeded. Implementation now lives in an agent-owned clean clone based on
`a5454c40`; the user's dirty checkout remains untouched. The candidate has a
clean Rust 1.97.1 compile check, but its test suite is intentionally deferred
until the single adversarial review is complete.

## 10. Implementation status

**Base:** `a5454c40` · **Branch:** `codex/tron-held-seek-recovery`  
**Draft PR:** [Forgejo #361](http://192.168.4.7:3000/noirr/plurx/pulls/361)  
**Working clone:** agent-owned Forgejo clone; the user's checkout is untouched

- [x] Original incident and independent review incorporated.
- [x] Fresh spent-recovery reproduction incorporated with its causal limits.
- [x] Held-key ownership, current-runway recovery, telemetry, and regressions implemented.
- [x] Pinned Rust 1.97.1 compile check green on the candidate.
- [x] Reviewable commits and draft pull request.
- [x] One adversarial agent review of the complete candidate.
- [x] Four review findings addressed: post-control evidence resampling, Live TV
  Stop cancellation, adapter harness ownership, and truthful drop telemetry.
- [ ] Fast lane green on the reviewed head.
- [ ] Pull request merged into `main` and branch cleaned up.
