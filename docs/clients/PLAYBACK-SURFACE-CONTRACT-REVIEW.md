# Playback surface review — recovery authority cannot belong to an overlay

**Status:** reviewed 2026-09-13 · **Reviews:** PR #274 @ c2deda1

Reviews [the proposal](PLAYBACK-SURFACE-CONTRACT.md) against its stated
`10f2afe6` baseline and the branch's shipped source at `c2deda1`. Evidence
lines below use the latter. No implementation changes.

Source shorthand: **W** = [index.html](../../crates/plurxd/src/web/index.html),
**P** = [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js),
**A** = [PlayerController.swift](../../clients/apple/Sources/PlayerController.swift),
**V** = [PlayerView.swift](../../clients/apple/Sources/PlayerView.swift),
**C** = [Controller.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt),
**S** = [PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt).

## 1. Blocker — a prompt does not prove recovery exhaustion

**Concerns:** §3.1, “at exactly the sites that show a prompt today”; §3.3
readiness deadlines; §4's unchanged-ladders promise.

**Evidence:** W:3981 sets `stallPrompt=true` but W:3986 schedules
`persistentWait` again before the 20-second deadline. A:3769 calls
`retryAfterReadinessTimeout` before failing; A:4291 can select a
same-delivery reopen, and A:5272 can attempt node failover. C:1441 returns on
`!player.playWhenReady`; C:2187 separately permits target-deadline recovery.

**Correction:** Require an explicit exhaustion result from the recovery
owner, including outstanding work, before pausing. Otherwise a readiness
prompt suppresses a compatibility rung, or a pause during Android's control
ask prevents its reopen. Define Keep waiting's budget and transport effects:
Apple's cited retry resets only `recoveryReopenBudget` (A:2522), not every
ladder. These pauses and budget resets are additional behavior changes.

## 2. Blocker — clock movement can forgive a failed picture or destination

**Concerns:** §§3.1–3.4, “only the clock may make it clear.”

**Evidence:** W:11249 requires both clock and available frame-counter advance,
not 500 ms alone. A:6092 explicitly describes a black picture with “audio is
still playing”; its exhausted black-frame ladder returns without stopping
(A:6097), contrary to §3.3. C:2170 says progress on a departed timeline cannot
cancel the target deadline. A:4989 excludes `isChangingStream` from periodic
observations; W:9570 retains the failed `pendingMediaChange`.

**Correction:** Separate attached-media identity, requested-intent identity,
and presentation evidence. A predecessor can advance while its replacement
fails; a seek can finish between samples and manufacture a 500 ms jump.
Define baseline resets and commit/rollback ownership for M6, including
Android `mediaMutationEpoch`. Specify video, audio-only, AirPlay, PiP and
background evidence separately: a remote clock is not proof of remote video,
and hidden-page sampling must not preserve an obsolete blocker. Do not clear
an unresolved destination merely because another picture moves. A recovered
segment refusal can also remain within one generation; key its causal
request/attempt and retirement, not just the generation.

## 3. Blocker — `surfaceHold` leaves other recovery and reporting paths active

**Concerns:** §3.2's two guarded functions and internal pause.

**Evidence:** W:3886/3935 lets an already scheduled `persistentWait` continue
with `wantsPlayback=true` even if paused. W:7191 reports `demand="active"`;
W:7192 defaults to `render="rendering"`. W:10541 applies playback intent by
calling `play()` independently of the proposed hold.

**Correction:** Define hold acquisition/release, timer cancellation, awaited
callback fencing and an accurate control snapshot. Guarding only `beginWait`
and `playbackProgressTick` does not fence existing work. Specify releasing the
hold after disagreement, or the recovered stream loses stall detection.
Preserve user intent without reporting a frozen player as rendering.

## 4. Blocker — the source table has conflicting decisions

**Concerns:** §3.3's exhaustive mapping and three ▲ exceptions.

**Evidence:** A change-create 503 matches both `preparing` and `refused`; successor
abandonment matches both `recovering` and no surface.
[http/hls.rs:3777](../../crates/plurxd/src/http/hls.rs) emits 409
`vod_source_rescan_required`, not another-device supersession. P:707 returns
only `{status,code,message}`: moving this parser “unchanged” loses the promised
`film_position_ms`. W:3940 waits for local exhaustion before applying terminal
wording, respecting [D1](../playback-control/M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md#46-ruling-d1--what-terminal-does-to-the-player).

**Correction:** Specify precedence, attachment ownership and action policy.
Preserve the transport-error exception to terminal wording (A:5327); an
armed recipe verdict does not explain every later network failure.
A segment 410 can arrive while buffered playback remains; immediate `stopped`
interrupts it. Bound create retries by an absolute deadline and define request
identity, cancellation and late-session disposal. Share one hls.js retry
budget between the overlapping fatal-network/503 rows and preserve position.
[Media3's default-position recovery](https://developer.android.com/media/media3/exoplayer/live-streaming)
is a live-edge policy; importing it into finite playback can skip content.
Require a finite-timeline rule and retry limit. These additions are not yet
safe specifications.

Missing sources include decision/create transport timeouts and cancellation,
Android target-deadline exhaustion, and VOD refusals such as `vod_disabled`,
`vod_index_pending`, `vod_engine_unattested` and unsupported tracks
(http/hls.rs:3758–3783). Add explicit cold-start and failed-change outcomes;
a generic HTTP class cannot distinguish an index still building from disabled
service.

## 5. Should-fix — native body handling is not implemented by the proposed seam

**Concerns:** §4 adapter and M2.

**Evidence:** [PlurxAPI.swift:196](../../clients/apple/Sources/PlurxAPI.swift)
decodes URLSession response data. A:5210 receives AVPlayer's error log, not
playlist/segment response bytes. [AppModel.swift:288](../../clients/apple/Sources/AppModel.swift)
recognizes authentication failure only as `.http(let code)`.

**Correction:** Prove how AVFoundation refusals reach the adapter. Changing
`check` does not supply media response bodies. Preserve status-based auth and
existing retry matchers when introducing `.refused`; otherwise structured
401/403 stops triggering sign-in. Include position and unstructured-error
fallbacks in both native error types.

## 6. Should-fix — actions and failure routing disagree

**Concerns:** §4, “only `stalled` and `stopped`, the two classes with actions.”

**Evidence:** §3.2 retains Try again after demotion to `degraded`; §3.1 expires
that notice after five seconds, before the example's 30-second buffer drains.
W:10968 offers Force transcode, absent from the new actions. The
[input fixture](../../tests/playback/player-input-contract.json) gives
`failed` its own focus/ignore behavior.

**Correction:** Specify action availability and focus independently of class,
including demoted recovery, sign-in, no-retry conflicts and Force transcode.
Define an underlying fault that survives banner expiry. Pin cross-product
input tests: changing which prompts enter `failed` changes behavior even
without editing the routing table.

## 7. Should-fix — the fixture cannot decide the proposed transitions

**Concerns:** §§3.5, 4–5.

**Evidence:** Cases provide `moving` and one `then`; effects allow only
`pause`, `log`, `clear`, while Keep waiting requires resume/reopen and budgets.
§3.5 says stalled clears only on user input; §3.2 also clears on disagreement.

**Correction:** Add ordered, timed events; multiple faults and precedence;
attachment/intent IDs; pause acknowledgement; deadline/budget state; actions;
late callbacks and replacement rollback. Specify fault expiry and whether
10 seconds means continuous progress or accumulated playback. Give ledger
entries playback/session/attempt identities: a local generation number cannot
join independently restarted clients to server sessions. Snapshot-only cases
cannot prove those guarantees.

## 8. Should-fix — scope and the textual fence are porous

**Concerns:** §§4, 7.

**Evidence:** W:19762 runs Library Channels through `play(...)`, sharing the
supposedly excluded renderer. A:2437 writes `failed=false` for included Apple
offline playback. C:536 writes `playbackNotice`, absent from Android's banned
write patterns. W:11018 clears classes through `classList.remove`.

**Correction:** Declare boundaries for shared channels, offline, audiobook
and external playback surfaces. Scan assignments, aliases, bulk updates,
class toggles/removals and PiP state, not just listed spellings. Conversely,
raw error/status references in readiness, recovery and diagnostics must remain
legal: the adapter-only restriction would flag required ladder code. Pin both
positive and negative fence fixtures rather than claiming complete ownership
from a shrinking allowlist.

## 9. Should-fix — §6.1 overstates reachability and omits an origin fallback

**Concerns:** “Structurally reachable on every seek and track switch.”

**Evidence:** [transcode.rs:17115](../../crates/plurxd/src/transcode.rs)
rejects non-VOD creation; C:986 seeks VOD natively. For progressive remux,
C:975–979 attaches without correction;
[MediaOrigin.kt:269](../../clients/android/app/src/main/java/tv/plurx/app/player/MediaOrigin.kt)
only updates `originMs`;
[PlaybackIntent.kt:461](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackIntent.kt)
only resets the recovery deadline. However,
[http/stream.rs:63](../../crates/plurxd/src/http/stream.rs) returns the requested
origin after a one-second probe timeout. That can conceal keyframe preroll
from the 250 ms check. A long GOP alone does not prove the target misses its
nearest preceding keyframe by more than 250 ms.

**Correction:** Separate current VOD, progressive and legacy-server cases.
No compensating client seek was found in the named progressive paths; the
prepared successor does seek (C:2441). Require achieved-origin and first-frame
measurements before prescribing the repair.

**Device evidence:** Google TV Streamer, build 89: Nosferatu reported Remux,
73 Mb/s, no server-side session, and reached 0:36. A single timeline tap
advanced to 11:14; the resulting transport was paused, so I resumed it. The
observed failure was “Playback stopped responding after retrying this
stream.”, not the predicted target-deadline sentence. This is an inconclusive
variant of recipe (c), not confirmation of its 8/16-second sequence. The
1080p comparison could not be established: after selection, the accessibility
dump was killed and the screenshot was black. No target-timeout log was
obtained; copy-HLS and a controlled transcode comparison remain unverified.

## 10. Should-fix — milestone acceptance contradicts the contract

**Concerns:** §8 M1–M4.

**Evidence:** M2 requests a 15-second cold-transcode readiness timeout;
§2.2 says cold transcodes never hit it, but A:1631 admits fresh VOD
transcodes. The delivery and exhausted rung must be named. M3 expects
`onIsPlayingChanged(true)` alone to cause disagreement, expressly forbidden by §3.1. M1 demands an indicator during a
network drop without specifying whether buffered presentation survived.

**Correction:** Specify reachable injected failures and elapsed progress,
remaining buffer and retry state. Test the platform pause effect, not only
the pure reducer. M4's “every instance is explained” needs allowed outcomes
and evidence requirements; otherwise any regression can be explained away.

## 11. Should-fix — §2 claim-by-claim audit needs corrections

Bullets are numbered in their existing order. **Confirmed** means the stated
mechanism exists, not that every reported incident followed it. Apply the
qualifications below; §2.4's blanket “none ... told what the player did next”
also overstates existing progress callbacks. Its 12 Apple/8 Android source
counts are consistent; its web count is not.

| Bullet / claim | Verdict | Evidence and correction |
|---|---|---|
| 2.1.1 surface/count/spinner | Overstated | W:11006 renders directly; four failed additions, no failed-spinner CSS. Baseline has 35 calls; 37 counts declaration and comment. |
| 2.1.2 fatal → buffer → reopen | Overstated | W:6993 has no pause; W:6933 recovery-failure branch and W:3935 guards prevent the universal sequence. |
| 2.1.3 failed change sticks | Confirmed | W:9570 “Retain the desired recipe”; W:10658 excludes `pendingMediaChange`; W:9057 retains failed preparation. |
| 2.1.4 refusal lifetime | Overstated | P:727 is 90 seconds; W:6682 clears retryables only. P:695/706 rejects malformed/empty bodies: not every ≥400 response is recorded. |
| 2.1.5 waiting paints immediately | Confirmed | W:11543 calls `setLoading`; W:3812 separately defines `STALL_MIN_MS=350`. |
| 2.1.6 every non-direct seek dims | False | W:11768 includes `PLAYER.vod` in native seeks; no `executePlaybackMediaChange`. That function's overlay claim is true (W:9523). |
| 2.1.7 DOM-derived failed | Confirmed | W:11056 checks `classList.contains("failed")`; desktop non-failed arrows can skip. |
| 2.2.1 published fields/banner | Confirmed | A:1760 onward declares fields; V:779 and V:1058 select failure/banner. |
| 2.2.2 every readiness failure uses fail | Overstated | A:4830 lacks pause; cited terminal writers pause. A:5384 swallows failover-readiness errors instead. |
| 2.2.3 every VOD seek has deadline | Overstated | A:6147 throws timeout; A:2673 performs native `player.seek` without `awaitItemReady`; fresh VOD transcodes also qualify (A:1631). Late playback is possible, not measured. |
| 2.2.4 error never clears on progress | Overstated | A:4985 observer does not clear it. Alleged post-attach clears are `failed=false` (A:2437/3787), not `playbackError=nil`. |
| 2.2.5 bodies discarded/410 fatal | Overstated | PlurxAPI.swift:196 decodes only 409; A:5300 can still retry established HDR before terminal. |
| 2.2.6 lock-screen play bypass | Confirmed | A:6969 calls `player.play()` directly. |
| 2.2.7 persistent PiP clear | Confirmed | PlayerSurface.swift:119/133/219 calls `clearErrorMessage` on PiP events. |
| 2.3.1 sticky callback/nine sites | Overstated | S:674/691 stores the string without reset. C has eight `onError(...)` calls, also eight at baseline. |
| 2.3.2 transparent blocking failure | Confirmed | S:629 is `fillMaxSize()` without background; S:790 selects `Failed`. |
| 2.3.3 watchdog always restarts | Overstated | C:660 does not pause, but C:832 requires foreground and no pending seek; C:1441/1463 checks intent/budget. Reachable, not inevitable. |
| 2.3.4 B–G do not stop | Confirmed | C:1288/1344/1451/1464/1551/2186 call `onError` without stopping. |
| 2.3.5 HTTP/1002 always Fail | Overstated | Bodies are unread; C:560 attempts failover. PlaybackPolicy.kt:72 allows established HDR retry before the non-compatibility Fail at :77. |
| 2.3.6 no named tests | False | AppleClientTests.swift:2369/2384 asserts `playbackNotice`, including at baseline. Narrow to uncovered surfaces. |
| 2.3.7 photo proves watchdog chain | Overstated | Photo supports visible overlay; C:831 makes restart plausible. No correlated log proves the photographed cause. |

Reject: the proposal transfers recovery authority to presentation without a
complete ownership, evidence or action contract. Its factual overclaims and
incompatible acceptance checks would permit a green implementation that
interrupts recoverable playback, loses the requested destination, or hides a
still-actionable failure.
