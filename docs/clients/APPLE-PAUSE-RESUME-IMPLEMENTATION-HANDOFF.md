# Apple pause/resume — build a bounded return to the picture

**Status:** open — adversarially reviewed build contract; implementation and
physical-device acceptance remain. **Written:** 2026-09-17, America/New_York.
**Revised:** 2026-09-18 after [Fable’s review](APPLE-PAUSE-RESUME-HANDOFF-REVIEW.md).
**Evidence collected:** 2026-09-18 UTC. **Reviewed base:**
`6fb0901d3d18c1b181f7299f4faddfb73994fd1a`.

Companion to [playback lifecycle coverage](../playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md),
the [lifecycle implementation](../playback-control/PLAYBACK-LIFECYCLE-IMPLEMENTATION.md),
its [status ledger](../playback-control/PLAYBACK-LIFECYCLE-STATUS.md), and the
[Apple input contract](PLAYER-INPUT-CONTRACT.md). This document gives
another session the incident, required behavior, reviewed design, build
sequence, and acceptance criteria. It is sufficient to begin implementation
without recovering the originating conversation.

Read [AGENTS.md](../../AGENTS.md), the [docs index](../README.md), and the
[current development workflow](../DEVELOPMENT_PIPELINE.md) first. Work through
§4–§8 in order. Use the existing playback owner and presenter. A change to
server pacing, codec policy, or the wire protocol requires new evidence;
none is justified by this incident. This repair targets a concrete instance of the
lifecycle plan’s loaded-media reevaluation and L12–L14 prompt active-demand
obligations. Keep those documents consistent with the implemented behavior.

The user reported Heartstopper taking about 45 seconds to resume after a
roughly five-minute pause, then clarified that it might have been 18 seconds.
Their requirement is: **resuming should never take longer than starting a
new stream.** The server timeline supports approximately 18 seconds and
shows enough retained client media to make that wait unnecessary.

## 1. Evidence — the paused player already had 62.3 seconds buffered

Read-only collection used `docker logs --since 12h --tail 30000 plurxd` on
`m6`, `nynuc`, `nuc3`, and `nuc4`, through the existing deployment SSH access.
The matching Heartstopper session was on `m6`, file ID `1674`, episode
“Meet,” with copied HEVC video at 2160p. Server build:
`v0.3.0-2770-g6fb0901d`. No server or player was restarted during collection.

The following is the retained projection. Raw media paths, credentials,
network addresses, and capability URLs are intentionally omitted.

| UTC on 2026-09-18 | Observed event | Meaning |
|---|---|---|
| 03:12:35.154 | Actor-owned copy/remux session starts | Original delivery, producer attempt 1. |
| 03:12:37.445 | Apple `ttff_ms=5145`, reason `cold-start` | Original cold startup took about 5.1 seconds by existing client telemetry. |
| 03:13:07.602 | Producer held, `hold_reason=Demand` | Client explicitly paused; producer suspension was intentional. |
| 03:21:47.516 | Apple `surface_raised`, `62.3 s client loaded`, no server HTTP waits | First recorded resume buffering surface; film position later remains 29,661 ms. |
| 03:21:52.340 | Hold changes from Demand to Time; ahead 68 s, target 93 s, release 63 s | Active demand reached the server roughly 4.8 seconds after the surface. Producer remained held under time hysteresis. |
| 03:22:01.304 | Buffering stall, `stall_ms=12179`, runway 62.3 s, outcome reopen | Report logged after about 12.2 s of detector time and the subsequent control ask. |
| 03:22:03.237 | Predecessor retired as superseded | Replacement started; the original session was not simply an idle-expired session. |
| Before replacement TTFF | Telemetry row 66715: `session_start method=remux encoder=vod`, `presentation=vod` | The repair selected the VOD cache, not another rolling copy session. |
| 03:22:05.531 | Replacement `ttff_ms=4265`, reason `stall-buffering`, `method=transcode encoder=vod` | About 4.3 s to playback from the VOD-cache presentation. |
| 03:22:19.759 | Row 66722: `429 control_rate_limited` | Subsequent control-budget rejection; not established as caused by the resume toggle. |

The observed interruption is 18.015 seconds from buffering surface to
replacement TTFF: approximately 13.8 seconds before the reopen and 4.3 seconds
inside it. The recorded pause spans about 8 minutes 40 seconds, rather than
exactly five minutes. These are event timestamps, not exact remote-button or
physical display measurements. The installed Apple build number was not
established by this collection.

**Established by the stored diagnostic snapshot:** Fable read the node-local
`playback_events.extra` records from `/srv/plurx/hiqlite/telemetry.db`, rows
66687–66723, as well as the logs. The supplied review records row 66713:

```text
time_control_status = waiting
waiting_reason = AVPlayerWaitingToMinimizeStallsReason
playback_buffer_empty = false
playback_buffer_full = true
playback_likely_to_keep_up = false
runway = 62.338 s; position_ms = 29661
server: hold_reason=time; suspended=true; delivered_idle_ms=532200
        published_end_ms=98000; fetched_end_ms=92000; last_request=control
```

This confirms the specific minimize-stalls wait with a full retained buffer;
it is no longer inferred solely from the ordinary log line. The additional
row evidence is from [Fable’s supplied review](APPLE-PAUSE-RESUME-HANDOFF-REVIEW.md),
not a second database collection by the document editor.

The 13.8 seconds before reopen comprise approximately 12.2 seconds of the
six-check detector plus the 1.5-second `controlVerdictForStall` ask. The report
is logged after that ask; the successor TTFF clock begins after it too.
`holdMayDecideStall` refuses a hold with runway above 10 seconds. The native
nudge was due around 6–8 seconds by detector arithmetic, but has no event
timestamp; its existing `play()` did not break this wait.

The producer was held with 68 seconds ahead and a 63-second release threshold:
it needed roughly five seconds of consumption before resuming. AVPlayer
waited to consume until it considered supply sufficient. The server’s
`recovery_outranks_hold` can withhold a hold instruction for this stalled,
loaded client; it neither resumes the producer nor plays the retained media.
The client owns those bytes and must break the wait. This supports a client
repair without changing server hysteresis.

Apple documents that
[`playImmediately(atRate:)`](https://developer.apple.com/documentation/avfoundation/avplayer/playimmediately(atrate:))
can play available buffered media without waiting for enough media to
minimize stalls. Use this targeted operation; globally disabling buffering
is not the proposed fix.

## 2. What the investigation built, and what remains

A local prototype was compiled against checkout `10f2afe60`, Apple build
144. It added a buffered immediate-play path, urgent pause/resume control
updates, and shared explicit play/pause handling for remote commands.
Seven new command-selection tests and one control-pump test ran with
existing ownership/session tests: **46 tvOS tests passed**. An iOS simulator
build also succeeded, using Xcode 27.0 (`27A266a`), with tvOS 26.5 simulator
runtime for the test run.

Those results prove compilation and selected command/control behavior on
that older source. They do not prove resumed pictures, physical Apple TV
latency, or compatibility with current main. The adversarial review found
missing deadline and ownership coverage, recorded in §3.

**The incomplete prototype was removed from the checkout after review.**
This handoff, the retained Fable review, and their index rows are the
documentation deliverable. Do not look for a ready-to-ship implementation in the Apple source files. The
prototype patch and tests were retained temporarily under `/tmp` for local
inspection, but this contract does not depend on their survival.

The fetched main at investigation end is
`6fb0901d3d18c1b181f7299f4faddfb73994fd1a`, whose Apple source build is 167.
It contains substantial player changes missing from the prototype base.
Fetch again at implementation time; neither number is a release reservation.

## 3. Adversarial review — required corrections and their disposition

A separate adversarial agent reviewed source, tests, incident evidence, and
current-main differences. These are design corrections incorporated into
this build contract, **not claims of completed implementation**.

| Finding | Severity | Required disposition | Acceptance |
|---|---|---|---|
| Immediate-play has no presentation acknowledgement or resume bound; a stuck player can still spend 12+ seconds before repair. | P1 | Add the attempt/deadline ownership in §5. A command or `.playing` never settles a video resume. | Ignored immediate-play command triggers one repair within the fast-path deadline; total budget survives replacement. |
| Pending-seek “guard” only excluded immediate-play; fallback `play()` still started the predecessor. The test only forbade the former. | P1 | Suppress every positive-rate command to the unexecuted predecessor. Reconcile intent on the correctly owned target as in §4.3. | Assert no `play`, immediate-play, or positive rate on the predecessor, then prove target playback. |
| Old-base code lacks current presenter and same-recipe recovery semantics. | P1 | Port narrowly onto fresh main; preserve all four invariants in §4.1. | Current-base presenter, ownership, and recovery tests pass. |
| Tests proved command selection, not resumed frames or rapid-toggle safety. | P2 | Add deterministic presentation/deadline and held-response races in §6, then physical-device acceptance in §8. | Command spies alone cannot satisfy acceptance. |
| Resume telemetry cannot distinguish command latency, presentation latency, and repair time. | Follow-up adopted | Add bounded, credential-free resume measurements in §5.5. | Evidence records actual resume outcome and preserves existing startup TTFF meaning. |

### 3.1 Fable’s review — incorporated corrections A–L

Fable’s verdict was **APPROVE WITH CHANGES**, against the pinned source and
additional stored incident diagnostics. The [full review](APPLE-PAUSE-RESUME-HANDOFF-REVIEW.md)
is retained unchanged. The following dispositions govern this revised contract;
they are not claims of built or physically accepted behavior.

| Finding | Disposition in this revision |
|---|---|
| A — stored snapshot and timeline | §1 cites row 66713, separates detector/control-ask time, identifies the cache successor and later 429. §5.5 reuses the existing diagnostic field. |
| B — cold-start conflict | §5.2 limits the 15-second budget to established-item resume and its owned repair. Cold startup and unrelated seek/preparation budgets remain unchanged. |
| C — hidden control wait | §5.4 requires the admitted resume repair to use `consultControl: false`; timing and sequence-floor waits are accounted for explicitly. |
| D — intent ordering | §4.2 uses `reportIntent()` plus `retainControlSequence`; the proposed `playerChanged(urgent:)` extension is withdrawn. |
| E — inert ordinary nudge | §4.4 requires qualified immediate-play reevaluation in the ordinary nudge too; M4 updates the lifecycle ledger. |
| F — stale frame and momentary motion | §5.3 baselines the paused output and requires strictly newer frames plus sustained progress. §4.4 increases the runway guard. |
| G — source fences and stale comment | M1 updates the existing single-stop-helper fence and lock-screen comment in the same change; behavioral tests complement that fence. |
| H — workspace and build issue | §4.1 requires an independent Forgejo clone on agent-owned disk; M4 files/reuses an issue before its build fragment. |
| I — actual recovery fences | §4.1/§5.4 distinguish queue serialization, generation fencing, one-shot repair state, and the separate storm cap. |
| J — staged successor on pause | §4.2 explicitly preserves current abort-on-transition behavior for this repair, with desired-selection retention and exactly-once abort tests. Staged-pause retention is out of scope. |
| K — lifecycle integration | Opening links and M4 connect this repair to loaded-media reevaluation and L12–L14 in the implementation/status documents. |
| L — presentation comparator | §8 records `encoder` and `presentation` per trial and compares cache with cache, rolling with rolling. |

The original prototype review found no reporter-internal ordering defect.
Fable correctly identified that the proposed caller discarded the sequence
floor needed by a later create. Preserve the existing reporter machinery and
use its existing intent seam; a safe pump alone does not order its caller’s
replacement.

## 4. Implementation contract — preserve the current playback owner

### 4.1 Establish a clean, current base

The originating checkout is Paul’s clone and contains unrelated changes.
Do not build in it, attach worktrees to its `.git`, reset it, stash it, or
commit its contents. Use an **independent Forgejo clone on agent-owned disk**,
following the agent-access convention supplied with Fable’s review. The
credential is available at `~/code/plurx-agent/github_token`; obtain the
Forgejo clone endpoint from the configured agent-access notes, and use a
non-persisting credential helper. Never put the token in the URL, command
arguments, printed output, or `.git/config`.

With `PLURX_FORGEJO_AGENT_URL` set to that credential-free HTTPS endpoint and
the session’s secure credential mechanism configured:

```bash
mkdir -p /tmp/agent
git clone --filter=blob:none "$PLURX_FORGEJO_AGENT_URL" \
  /tmp/agent/apple-pause-resume
cd /tmp/agent/apple-pause-resume
git fetch origin main
git switch -c codex/apple-pause-resume origin/main
git rev-parse HEAD
```

If the directory/branch already exists, inspect and resume the agent-owned
clone rather than overwrite it. Do not clone from Paul’s working tree or
borrow its SSH configuration. Import this handoff and the retained review;
merge their two rows into the fresh docs index. All build and Git mutation
commands below run in the independent clone. The current session only edits
the explicitly requested documentation in the supplied workspace.

Re-resolve symbols in
[PlayerController.swift](../../clients/apple/Sources/PlayerController.swift),
[PlaybackControlSession.swift](../../clients/apple/Sources/PlaybackControlSession.swift),
and [PlaybackControlReporter.swift](../../clients/apple/Sources/PlaybackControlReporter.swift).
At the reviewed main, preserve:

1. `wantsPlayback.didSet` emits `.playbackRequested` to the presenter. A pause
   retires an irrelevant buffering surface; a resume may establish a new one.
2. `beginViewerAction()` emits `.intentSuperseded`, invalidates old verdicts,
   and abandons staged replacements. Resume monitoring must respect it.
3. Timer-only recovery uses `PlayerOpenIntent.sameDeliveryRepair`. A wait
   alone cannot change codec, dynamic range, height, or the requested recipe.
4. `PlayerReopenQueue` serializes session replacements; `openGeneration`
   rejects stale attachment results; `SameDeliveryStallRecoveryState.attempted`
   admits one timer repair until five seconds of established progress rearms
   it. `recoveryReopenBudget` is a separate cap of three automatic reopens per
   60 seconds, not a one-replacement lock. Extend these fences as in §5.4.

No Rust change is initially required. If evidence expands scope into Rust,
first verify the repository-pinned toolchain and establish the
[compile loop](../ci/AGENT-COMPILE-LOOP.md). This host's default Homebrew
`rustc` was 1.98.0, while `rustup run 1.97.1 rustc --version` verified the
pin. The default executable is not equivalent validation.

### 4.2 Centralize explicit intent and urgently report it

Proposed controller entry points:

```swift
func togglePlayPause()
func setPlaybackRequested(_ requested: Bool)
```

Keep `playbackControlPlayerChanged()` and session `playerChanged()` as the
ordinary coalesced path. **Do not add an `urgent:` overload.** The existing
interactive seam is:

```swift
let floor = await playbackControl.reportIntent()
// After checking that this task still owns the same lifecycle/intent/session:
retainControlSequence(floor)
```

`togglePlayPause()` delegates using `!wantsPlayback`. Explicit Play and Pause
are idempotent; repeat commands create no new attempt or publication. Route
iOS remote Play/Pause through the same controller entry point. Preserve tvOS
input routing and the invariant already enforced by the progress observer:
only `.playing` with positive rate may update `preferredRate`.

On a real transition, update viewer ownership and retained intent before an
AVPlayer callback can see an obsolete intent. Publish through `reportIntent()`
and retain its returned request-ordering floor for any owned replacement;
`open()` already folds `pendingControlSequence` into its create body. This
await queues an urgent capture and obtains a floor; it does **not** wait for
an HTTP response. Do not let a repair create overtake floor acquisition.

Use one cancellable/latest-intent controller task. Check its lifecycle,
action epoch, current session/attachment, and cancellation before entering
`reportIntent()` and again after the actor hop before retaining the floor or
starting media. Synchronous Pause stops the transport immediately; a queued
old Resume must never start it later. A superseding seek transfers publication
and creation to the seek owner, not a stale resume task. Keep a bounded path
when bootstrap is absent; a nil floor must not prevent local playback.

Valid buffered playback must not wait for a server round trip. The short
actor hop, any scheduling delay, and floor acquisition still count against
the resume deadline. Publish before the authorized create; do not claim
that an urgent capture has already been accepted on the server. A hung
publication task is cancelled/fenced at the outer deadline rather than
creating an unordered successor.

Ordinary progress remains coalesced. Publish urgently once per real
transition; rapid toggles replace pending intent rather than queueing one
wire exchange per button press. Preserve reporter minimum spacing, backoff,
in-flight protection, source revisions, and capability-less operation.
The observed subsequent 429 makes a held-response toggle-storm regression
mandatory. This repairs the lifecycle L12–L14 obligation: main’s current
coalesced toggle can delay active demand by an ordinary five-second cadence.

**Prepared-successor decision for this repair:** preserve
`beginViewerAction()` and its `abandonWithoutFallback(.aborted)` on both Pause
and Resume. Although Pause does not change the selected recipe, retaining an
M6 successor across pause would change prepared-commit ownership and needs a
separate lifecycle design. This repair deliberately accepts losing that
preparation work, preserves the desired quality/audio/subtitle selection,
and tests one aborted acknowledgement with no stale commit or leaked player.
The epoch bump remains necessary to invalidate an old control ask or resume.
This is an explicit scope decision, not a claim that aborting staging is the
only possible long-term pause policy.

### 4.3 Separate a paused attached item from an unexecuted destination

Do not implement `if pendingSeek { player.play() }` as a fallback. Do not
blindly ban every play during `isChangingStream` either: current attach code
intentionally starts an owned new item before waiting for readiness, because
some tvOS items do not ready reliably at rate zero.

| Resume state | Controller action | Who may start audible playback |
|---|---|---|
| Attached current item, no pending seek/change, ready, sufficient contiguous runway | Start immediately at retained rate and begin presentation tracking. | Resume owner for that exact item. |
| Native seek requested but not yet executed/landed | Retain Play, urgently report its target, keep predecessor paused. | Native seek completion after current action, seek generation, item, and open-generation checks. |
| Replacement creating; predecessor still attached | Retain Play; do not start predecessor. | Replacement owner once the successor is attached/prepared for the retained destination. |
| Successor attached and readiness/target seek still in progress | Reconcile current Play/Pause with that preparation owner; preserve its readiness-loading path. | Owned preparation code; target presentation still has to qualify. |
| Already executed seek awaiting first target picture | Continue through the seek presentation owner; do not arm a competing replacement. | Existing target owner with current intent. |
| Stop, end, blocking fault, background ownership change | Cancel/retire this resume attempt. | No delayed resume callback may start the player. |

For the native path, after `player.seek` and media-selection awaits,
revalidate action epoch, seek generation, current item identity, and
`openGeneration`. If the landing requires a replacement, take that route
before playback. Otherwise reconcile the latest intent/rate, mark execution,
and retain the presentation monitor. A newer Pause during any await wins.

For replacement preparation, distinguish predecessor from successor using
item and attachment ownership, not just the global `isChangingStream` flag.
Keep readiness preparation and permission for audible playback separate.
Test pause/resume while waiting for readiness as well as before attachment.

### 4.4 Use buffered media without changing global buffering policy

The ten-second guard aligns this path with the existing loaded-versus-low-
runway distinction and avoids consuming a one-second buffer then immediately
falling back into the ordinary stall wait. Qualify it across playback rates.
For an eligible attached item, require `.readyToPlay` and **more than ten
wall-time seconds** of contiguous runway at the intended rate:

```text
required media seconds > intended playback rate × 10.0 seconds
62.338 media seconds at 1× qualifies
10 media seconds at 1× does not qualify
15 media seconds at 2× does not qualify
a disjoint future range does not qualify
```

Reuse `bufferedRunwaySeconds()` and its existing range/timeline conversion.
Unknown, nonfinite, negative, or insufficient runway does not qualify. Keep
`automaticallyWaitsToMinimizeStalling=true`, the existing 60-second growing
HLS preference, and direct/VOD buffering configuration.

Call `player.playImmediately(atRate: preferredRate)` once for a qualifying
resume. The ordinary watchdog’s nudge **must** use the same guarded immediate-play
reevaluation when runway qualifies; repeating `play()` leaves the observed
minimize-stalls wait intact. Preserve its cadence, one-nudge ownership, and
existing behavior when the guard fails. It must not restart the resume clock. Empty/unready media takes the bounded
existing recovery/preparation path in §5 rather than first paying the
established-stall wait. An existing seek/attachment preparation owner takes precedence: give
that work the retained intent and its owning deadline instead of creating
another session because its buffer has not filled yet. Only a repair owned
by an established-resume attempt inherits the new 15-second deadline. Do not rebuild a healthy, buffered item merely because the pause was
long; age alone does not prove its session is dead.

## 5. Bound the whole resume attempt, not each layer independently

### 5.1 One attempt owns intent, position, time, and presentation evidence

Add a small, testable state machine; the following is a **proposed interface**,
not an existing source type:

```swift
struct PlaybackResumeAttempt {
    let id: UUID
    let lifecycleGeneration: Int
    let viewerActionEpoch: Int
    let attachmentGeneration: Int
    let itemIdentity: ObjectIdentifier
    let targetMs: Int
    let startedAt: TimeInterval        // monotonic uptime
    let fastPathDeadline: TimeInterval? // ready retained-item path only
    let expiresAt: TimeInterval        // established-resume deadline, inherited by its repair
    var repairAdmitted: Bool
}
```

Keep the current attempt on MainActor and put elapsed-time decisions in a
pure policy with an injected clock. A newer pause, seek, selection, stop,
background transition, or unrelated attachment invalidates it. An owned
repair explicitly creates a successor binding with the new item identity and
attachment generation while copying the original `id`, root intent,
`startedAt`, and `expiresAt`. Clear `fastPathDeadline` and retain the spent
repair admission. These immutable fields describe one binding; do not mutate
an old token to make its late callbacks appear current. A generation change
must not reset the budget or discard successor presentation obligations.

Track pause duration separately for diagnostics. Five paused minutes do not
spend active resume time. A return after background must revalidate the
current item and intent rather than run an overdue callback against it.

### 5.2 Exact timing policy and its limits

Use these initial implementation constants, with injected time in tests:

| Budget | Initial value | Rule |
|---|---|---|
| Buffered fast-path progress allowance | 1,000 ms | From explicit resume, independently of the two-second health tick. Obtain fresh continuing presentation (§5.3) or admit one repair by this deadline. |
| Established item without usable retained media, or known retired/failed session | 0 ms grace before the existing recovery path | Do not spend the ordinary established-stall delay first. Existing seek/preparation ownership still takes precedence. |
| Established-item resume and its owned repair | 15,000 ms total | One monotonic deadline across intent publication, create, readiness, target seek, and continuing presentation. The repair inherits remaining time. |
| Presentation sampling | At most 100 ms while the attempt is active | Reuse the existing 50-ms seek-presentation scheduling/observation mechanism where appropriate; a periodic time observer may stop during a wait. |

The 15-second budget applies **only to resuming an established, already-
attached on-demand item and the single successor created to repair that
resume**, including its offline route. It is a new resume-specific upper
bound requiring device qualification. It is not a cold-start policy and is
not justified by the coincidentally equal inner readiness timeout.

Leave cold startup unchanged: `PlaybackCreateRetry` retains `[1, 2, 4]` seconds
and its absolute 60-second deadline; unestablished playback retains the
15-check (~30-second) leash; ordinary stalls retain the lifecycle plan’s
20-second advisory deferral ceiling. The 15-second item-readiness gate has
narrow VOD/direct conditions and never established a rolling startup bound.
Keep the web/Android/Apple create-retry fixture agreement intact.

A Play command during initial startup or an unrelated seek/preparation uses
that owner’s existing budget, not a new resume deadline. An already owned
resume repair retains its original deadline through preparation. In either
case `fastPathDeadline` is nil during seek/successor work: there is no
competing one-second repair and no second create merely because the new
buffer is empty. Live TV and library-channel lifecycles remain out of scope.

For an eligible established resume, pass the remaining outer budget into
its repair waits, or cancel and fence them at expiry. Do not change shared
cold-start constants to enforce that deadline. Annotate the narrower
resume exception in the lifecycle implementation/status documents while
preserving their ordinary-stall policy. Readiness is not presentation.

Normal buffered resume should return continuing motion within one second.
Fifteen seconds is a failure bound, not a successful-resume latency target.
An unavailable network or decoder cannot be made to render by a timer; when
the budget expires, stop through the existing owner and show a truthful,
actionable failure. Physical acceptance still requires a successful resume
no slower than the matched fresh-presentation comparator (§8).

The admitted resume repair uses `consultControl: false` (§5.4). Without that
change, the real cost would be 1 second of fast-path grace + 1.5–3 seconds of
control ask + 4.3 seconds of cache startup, or about **6.8–8.3 seconds**.
After bypassing the ask, the example is **1 + 4.3 = 5.3 seconds**, plus any
actual local intent-publication scheduling overhead. That still loses to a
matched 4.3-second fresh cache open. Record this overhead; do not claim an
absolute deadline proves the user’s comparative requirement. If it occurs,
leave acceptance open until the overhead is safely removed or overlapped.

Do not rearm grace or the total budget on retries. No control hold can defer
this admitted repair for another 20 seconds. Fatal classified errors keep
their existing classification/route, subject to the single admission below.

### 5.3 Fresh presentation, not transport status, settles video resume

Reuse the existing video-output observation, but **not** the seek plausibility
predicate unchanged. `PlayerSeekState.isPlausibleLanding` accepts a frame
within 250 ms behind the target, including the paused frame itself. Also,
`hasNewPixelBuffer` is new relative to the last copy, not relative to Resume.

Baseline the output at resume start: consume/record the paused frame’s own
`displayTime` through the shared observer, or retain the last displayed
sample timestamp. Use `copyPixelBuffer`’s returned display time and the
correct item-to-film mapping. A qualifying frame must be at least one
observed/known frame interval strictly beyond the paused baseline and belong
to the current item/attachment. Do not invent a frame interval when unknown:
require distinct forward display timestamps and establish the interval from
those observations before declaring success.

One fresh frame records first-picture latency but does **not** settle the
attempt. Require at least 250 ms of continuing forward video presentation,
with distinct advancing samples across that interval, while intent stays
active and no buffer wait or ownership change intervenes. Repeated old
samples, audio-only movement, or a return to waiting resets that continuity.
Complete this check inside the one-second fast-path allowance; otherwise
admit repair. Keep actual first-picture latency separate from stabilization
latency in telemetry and device measurements.

Reuse the surface model’s continuous-presentation semantics for resetting on
wait, hidden state, and generation changes. The owner supplies the qualified
video progress: `presentingContinuousMs` describes a retirement rule, not an
independent decoder probe, and `.playing` plus wall time cannot manufacture
evidence. Do not change existing presenter notice-retirement thresholds to
introduce this resume-specific 250-ms check.

Share observations with the seek monitor or transfer its ownership; never
install a second pixel-buffer consumer that steals frames. Audio-only items
use qualified continuing forward audio-clock progression. AirPlay/PiP and
background paths need explicit supported evidence and ownership; absent local
video samples cannot silently prove success or force healthy external media
to restart. Cancel/transfer observation according to those existing owners.

A returned sample proves decoded availability, not photons on the TV. Physical
qualification separately measures first visible motion and continuing motion.
Neither `presentationSize`, `.playing`, an HTTP success, nor an issued play
command settles video resume.

### 5.4 Admit recovery once and preserve the delivery recipe

Route an admitted resume repair through
`retrySameDeliveryAfterStall(event, consultControl: false)`, as the existing
seek-presentation timeout does, preserving `sameDeliveryRepair`. Retain the
intent sequence floor before its create; bypassing the advisory ask does
not bypass request ordering, server authorization, or backoff. Publish any
recovery evidence through the existing `reportEvidence()` seam without
adding an awaited verdict. For this loaded incident a hold cannot decide
anyway, and a `retry_resource` must not rearm an already admitted repair.

Reuse `PlayerReopenQueue`, `openGeneration`, and
`SameDeliveryStallRecoveryState.attempted`. Delivery starvation, deferred
stall, seek timeout, and ordinary timer stalls already funnel through that
one-shot state. Its healthy-playback rearm must not grant a second repair
inside an unresolved resume attempt. The separate three-per-minute
`recoveryReopenBudget` continues to cap longer-term storms. Classified
`handleItemFailure` compatibility handling is outside the timer-only flag:
explicitly fence it with the current resume attempt’s admission too. Resume expiry, the general stall monitor, delivery starvation,
item failure, and seek-presentation timeout must share one admission decision
for the current owner: **at most one repair per resume attempt across all
detector labels**. A classified failure may select the appropriate existing
recovery route before admission. Once repair is admitted, a failed successor
cannot obtain another replacement by changing detector labels; exhaustion
uses the existing actionable failure path. Mark admission before awaiting
control or network work. Revalidate lifecycle, intent, and item after every await.

Suppress competing timer recovery while the same attempt already owns a
repair. If an older async task later returns, it must release only its own
resources and cannot attach, play, fail, or change the newer attempt. Respect
normal server-session supersession and predecessor cleanup; do not create
an additional raw `createHlsSession` call outside the existing queue.

A resume timer does not prove a codec/HDR incompatibility or a slow link.
Preserve quality, HDR/Dolby Vision, audio/subtitle choices, current truthful
film position, and requested rate. Preserve offline reload handling.

### 5.5 Record enough to qualify the next physical build

Use the existing client-log infrastructure and redaction rules. Record
resume attempt ID, pause duration, initial runway, initial time-control state,
selected command/path, time until repair admission, first fresh picture,
continuing-presentation settlement, and terminal outcome. Reuse
`ApplePlaybackDiagnosticSnapshot.waitingReason` and its retained
`playback_events.extra` representation; do not add a second wait-reason
field or assume the ordinary text log contains the full snapshot. Bound detail
length and log at state transitions, not every 100-ms sample.

Keep total user interruption and successor startup as separate measurements.
Do not reuse `ttff_ms` in a way that overwrites the original cold-start
measurement or falsely calls a command a first frame. No credentials, signed
URLs, headers, or raw media paths belong in diagnostics.

## 6. Regression cases — make the adversarial failures executable

Add `PlayerResumeTests.swift` under the existing Apple test source directory;
XcodeGen includes that directory. Extend existing
[ownership tests](../../clients/apple/Tests/PlayerOperationOwnershipTests.swift),
[control session tests](../../clients/apple/Tests/PlaybackControlSessionTests.swift),
[reporter tests](../../clients/apple/Tests/PlaybackControlReporterTests.swift),
and the current-main presenter tests where those seams are already covered.

| Case | Required assertion |
|---|---|
| Heartstopper fixture: paused at 29,661 ms, 62.338 seconds of runway, 1× | Immediate-play once; no session creation when new presentation arrives; fresh frames and 250 ms continuing motion within 1,000 ms in virtual time. |
| Immediate-play ignored by AVPlayer | `.playing` alone does not settle; admit one repair by 1,000 ms and retain the original total deadline. |
| Audio moves but video does not | Video resume remains unsettled; cannot clear on a film-clock tick alone. |
| Only pre-pause frame is “new” relative to last copy | Baseline it; timestamp at paused position cannot qualify. Fresh strictly later samples must also establish 250 ms continuing progress. |
| One fresh frame followed by waiting | First-picture metric is recorded; attempt remains unsettled and repairs by its unchanged deadline. |
| Ordinary watchdog nudge with ample loaded runway | Calls guarded immediate-play once; cannot reset resume or ordinary stall time. |
| Empty, unknown, disjoint, NaN, and insufficient runway | No buffered fast-path claim; bounded fresh-start-equivalent path begins without a 12-second stall wait. |
| 1.5× and 2× rates | Preserve rate; use wall-time runway requirement; pause while rate is zero does not replace retained rate. |
| Paused seek followed by Play in coalescing window | No positive-rate command on predecessor; landed current target eventually plays. |
| Play during successor readiness | Owner's loading still progresses; no stale predecessor playback; final Pause wins. |
| Pause/seek/stop/new attachment before deadline | Old callback neither reopens nor starts playback nor raises a new fault. |
| Resume and general watchdog expire together | Exactly one recovery/session request and one budget spend. |
| Repair changes attachment | Same original absolute deadline; correct successor ownership; no fresh 15 seconds. |
| Rapid pause/resume/pause with control response held | Latest demand is Hold; old response cannot restart playback. No requirement that every intermediate toggle reaches the wire. |
| Long ordinary control cadence | Explicit intent reaches next allowed exchange without heartbeat sleep; ordinary progress stays coalesced. |
| Server 429 or transport backoff | Urgency respects legal spacing/backoff; deadline still bounds the viewer's wait. |
| Current-main presenter around pause/resume | Pause retires irrelevant wait; resume can raise a new wait; successful fresh presentation clears it under current ownership. |
| Established resume repair exhausts 15 seconds | One inherited deadline; inner timers cannot stack. |
| Cold startup / unrelated preparation | Existing 60-second create ladder, ~30-second unestablished leash, and ordinary 20-second stall deferral remain unchanged. |
| Repair before intent-publication task returns | Create waits for its current floor, old task cannot retain a stale-session floor, and outer expiry fences a stuck publication. |
| Staged M6 successor plus Pause/Resume | Exactly one abort acknowledgement, desired selection retained, no late commit or leaked pipeline. |
| Offline, direct, finite VOD, rolling HLS, external playback | Correct route and presentation evidence; no server creation for an offline item. |

Use fake clocks and suspended continuations for policy and ownership tests.
For AVPlayer command spies, let status and presentation vary independently:
`playImmediately` must not automatically manufacture a successful frame.
Behavioral tests prove these behaviors. Preserve and deliberately update the
existing source fence described in M1; do not delete it just because it is
not sufficient behavioral proof.

## 7. Build sequence and commands

### 7.1 M0 — import contract and capture the new baseline

Record base SHA, Apple build, Xcode version, and available simulator IDs.
Resolve current presenter test class names and attachment owners. Preserve
unrelated changes. Create failing regressions for ignored immediate-play and
pending predecessor playback before implementing the fix.

```bash
xcodebuild -version
xcrun simctl list devices available
git rev-parse HEAD
git status --short
```

**Acceptance:** baseline and failing behavior are recorded; failures must be
behavioral assertions, not missing SDKs, nonexistent destinations, or compile
errors accidentally presented as reproduced bugs.

### 7.2 M1 — explicit intent, immediate buffered play, and seek ownership

Implement §4 and its rate/seek/control tests. Preserve current-main presenter
integration. Use `reportIntent()`/`retainControlSequence`, not a new reporter
API. In the same commit, update
`AppleClientTests.testTheOwnerStopsThePlayerInExactlyOnePlace`: it currently
requires the hand-written pause block inside `togglePlayPause` and exactly
five `wantsPlayback = false` writers, including the separate lock-screen
writer. The centralized setter changes that location/count. Retain the
single owner-stop-helper invariant and explicitly enumerate remaining
writers; supplement it with runtime ownership tests. Rewrite the attach
comment claiming lock-screen `playCommand` calls `player.play()` directly.
Do not publish the partial patch as a completed latency fix.

**Acceptance:** command selection, native landing, successor readiness, and
latest-intent tests pass on the current branch for iOS and tvOS.

### 7.3 M2 — shared deadline, presentation proof, and single repair admission

Implement §5 with an injected monotonic clock for established-item resume
and its owned repair. Keep cold-start and unrelated preparation policy
intact. Integrate presentation sampling with the existing seek observer. Add the ignored-command, stale
callback, simultaneous-detector, and successor-budget regressions.

**Acceptance:** deterministic tests cannot produce two recoveries or a
15-second budget reset; blocked playback reaches the existing actionable
failure surface; actual fresh presentation is the only video success signal.

### 7.4 M3 — compile and run the affected Apple regressions

Generate from the independent agent clone, then test. These commands use the
simulator names observed in this investigation; select an installed runtime
from M0 if they differ. Use a clone-local DerivedData directory.

```bash
cd clients/apple
xcodegen generate

xcodebuild -project plurx.xcodeproj -scheme plurx-tvOS \
  -destination 'platform=tvOS Simulator,name=Apple TV 4K (3rd generation)' \
  -derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO \
  -only-testing:plurx-tvOSTests/PlayerResumeTests \
  -only-testing:plurx-tvOSTests/PlayerOperationOwnershipTests \
  -only-testing:plurx-tvOSTests/PlaybackControlSessionTests \
  -only-testing:plurx-tvOSTests/PlaybackControlReporterTests \
  -only-testing:plurx-tvOSTests/AppleClientTests test

xcodebuild -project plurx.xcodeproj -scheme plurx-iOS \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO \
  -only-testing:plurx-iOSTests/PlayerResumeTests \
  -only-testing:plurx-iOSTests/PlayerOperationOwnershipTests \
  -only-testing:plurx-iOSTests/PlaybackControlSessionTests \
  -only-testing:plurx-iOSTests/PlaybackControlReporterTests \
  -only-testing:plurx-iOSTests/AppleClientTests test
```

Add the current presenter and affected startup/seek test selectors to these
commands after discovering their actual classes. Preserve test counts and
`.xcresult` paths. `TEST SUCCEEDED` with zero selected tests is not evidence.
Run the existing cross-client create-retry fence to prove its policy did not
drift, from the clone root:

```bash
node --test tests/playback/web-policy.test.js
```

A failure in that fence is not authorization to change its shared numbers.

From the repository root, validate documentation and catalog integration:

```bash
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
make validation-lint
git diff --check
```

**Acceptance:** both platforms compile and execute the relevant tests; docs
links/index pass; each result names the exact tested base and patch. A result
from `10f2afe60` does not validate a port to `6fb0901d3` or later main.

### 7.5 M4 — release counters, review, and build delivery

File or reuse the Forgejo issue for this repair first and retain its number.
Per [Apple build-note rules](../apple-builds/README.md), create an issue-keyed
fragment with the required `Issue:` and `Build:` fields. Then claim the next
Apple build with the helper, not a hardcoded 168 or the prototype’s 144.

Update the lifecycle implementation’s loaded-media reevaluation and L12–L14
rows to name urgent ordered intent, the working immediate-play nudge, and the
narrow established-resume deadline. Update the lifecycle status ledger to
correct the claim that the old `play()` nudge was a proved loaded-media
reevaluation. Preserve historical campaign provenance; add the correction
and new evidence rather than rewrite history as though this fix had shipped
with the old campaign. Link this handoff and its review, and keep physical
acceptance pending until measured.

```bash
git fetch origin main
python3 -m validation.apple_build --merge-target origin/main
```

If the base moves while integrating, reconcile it and repeat relevant tests
against the actual candidate. Commit only this repair, its tests, and its
documentation/build records. Follow the current
[development pipeline](../DEVELOPMENT_PIPELINE.md) and blocking repository
merge rules; at the reviewed base, the pipeline's dated correction specifies
a draft main-bound PR, one adversarial review, addressed findings, then the
ready/fast-lane process. This investigation's prototype review does not
approve unseen implementation code. Obtain that implementation review once;
use follow-up on its findings rather than unrelated repeated reviews.

A passing merge does not build or deploy the app. Follow
[publishing](../PUBLISHING.md) for signed archives and device/TestFlight
delivery. Useful commands, to run only in the implementation/delivery phase:

```bash
scripts/ship-physical --apple --build-only  # Build signed artifacts; no installation.
scripts/ship --apple --dry-run             # Inspect the configured TestFlight invocation.
```

Record installed build and source SHA before device qualification. Keep
signing credentials outside the source and evidence. Do not restart servers
or change their configuration merely to ship this client repair.

## 8. Physical Apple TV acceptance — compare against a fresh start

Use the same Apple TV, episode/position, selected delivery recipe, network,
and server. Record TV/tvOS/app build and server build. For **each** resume,
repair, and fresh-open trial record `session_start`’s `encoder` and
`presentation`, cache state, and recipe. Compare resume/repair-to-cache with
open-to-cache and rolling with rolling; report an unavailable matched
comparator as unqualified. Do not evict production caches to force a match.
The original repair used VOD cache, so its 4.3 seconds is not interchangeable
with the original rolling-copy 5.1-second cold start. The original incident
was HEVC 2160p remux; avoid comparing a 4K copy resume with an easier SDR
transcode startup and calling it a latency improvement.

1. Measure at least five fresh opens at the comparison position. Capture
   input-to-visible-motion time and current client startup telemetry. Record
   individual values, median, and maximum; do not substitute server creation
   time for the visible result.
2. Play until the picture is established, pause for 5 minutes, then resume.
   Repeat at least five times, including a retained-runway case. Measure from
   the explicit Play input to the first fresh visible frame and continuing
   motion, not disappearance of the spinner alone.
3. Repeat a short 5-second pause and a 10-minute pause. Include rate 1.5×,
   pause while buffering, and paused seek followed immediately by Play.
4. Exercise deliberately unavailable/stale media in a controlled test
   environment, plus resume immediately followed by Pause. Do not terminate
   the user's live session to manufacture the test.

**Pass criteria:** buffered resumes normally show motion within one second
without replacement; each successful resume is no slower than its comparable
fresh-open measurement under matched conditions. Record timing resolution
and raw pairs. No extra ordinary stall/control wait precedes recovery. No
wrong-position audio/video, quality/HDR change, duplicate session, stale
callback resurrection, or orphaned producer occurs. Broken sessions resolve
through the bounded repair/failure contract, rather than a frozen screen.

If decode samples arrive but the display remains frozen, the resume is not
accepted. Retain evidence and investigate the render boundary. If physical
hardware is unavailable, deliver a compiled, reviewed candidate and explicitly
leave this section unaccepted. Simulator tests cannot close it.

## 9. Scope and completion ledger

Do not change server pacing/hysteresis, lease TTL, retained segment windows,
codec ladders, quality policy, cold-start budgets, Live TV pause rules,
Android, or web playback.
The incident establishes a native on-demand resume defect. Broadening it
would obscure causality and add unrelated acceptance requirements.

Do not ship the discarded old-base prototype or globally turn off automatic
buffering. Do not promise successful playback through arbitrary network or
decoder failure; enforce an owned deadline and truthful outcome.

| Item | State at handoff | Evidence/update required from implementation session |
|---|---|---|
| Incident timeline | Collected | §1, matching server build and client event measurements. |
| Independent adversarial review | Complete; design dispositions and follow-up corrections recorded | §3; original findings plus conditional fast-path timing, explicit successor binding, comparator overhead, on-demand scope, cross-detector admission, and preparation precedence incorporated. |
| Old-base feasibility prototype | 46 tvOS tests and iOS build passed; then removed | §2; not current-base or physical acceptance. |
| Fable review A–L | Incorporated into this revised design | §3.1; implementation and physical proof still pending. |
| Handoff/review index and link checks | Passed after Fable revision, 2026-09-18 | All four docs-index tests including both untracked documents; `git diff --check`; retained review matches the supplied attachment byte-for-byte. |
| Current-base implementation M0–M2 | Not started | Commit/base, scope, ownership and budget decisions. |
| Final iOS/tvOS regressions M3 | Not run | Commands, counts, results, and retained `.xcresult` paths. |
| Implementation review | Not performed | Ranked findings and their fixes on the actual candidate. |
| Apple release build and installation | Not performed | Claimed build, source SHA, archive and installed build. |
| Physical latency and race acceptance | Not performed | §8 raw measurements and explicit pass/fail. |

Suggested prompt for the receiving session:

> Implement this handoff on fresh main in an independent agent-owned Forgejo
> clone. Preserve
> current presenter and same-delivery recovery semantics, implement all four
> original adversarial corrections and Fable’s A–L dispositions. Preserve
> cold-start policy and use ordered `reportIntent()`. Run iOS/tvOS regressions
> before pushing. Record build and test evidence in this ledger. Do not treat the
> discarded prototype or its passing tests as the implementation. Keep the
> physical Apple TV acceptance status explicit.
