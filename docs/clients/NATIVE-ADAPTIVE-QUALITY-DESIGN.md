# Native adaptive quality — one policy, three players, five kinds of evidence

**Status:** open — D1, D2, D4 and D5 are on branch `plan/A-04`; D3's
shaped-network traces are pending on every platform (§6's prompts)
· **Executes:** §3.8 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **A-04**. This is a **design document**, not a build plan: §3.8 is
sized "P2, L" and the review's own instruction is that the work is
"measured, fleet-driven". It ends in a decision Paul takes (§7.1) and a
build plan someone else writes from it.

Companion to [../streaming/ADAPTIVE-QUALITY.md](../streaming/ADAPTIVE-QUALITY.md)
(why the adaptation brain lives in the client and exactly one encode runs),
[../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md](../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md)
(why every rung change today is a reopen and what a handoff would cost
instead),
[../playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md](../playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md)
(the wire all three adapters already implement) and
[PLAYBACK-SURFACE-CONTRACT.md](PLAYBACK-SURFACE-CONTRACT.md) (who may pause
or stop a player, and what a viewer pause means).

Read §2 before proposing anything. The browser's controller is not a
reference implementation to port — it is a record of which evidence turned
out to be trustworthy and which did not, and half of this design is
carrying those refusals across rather than the code. **If a step seems to
require a second `#EXT-X-STREAM-INF`, letting AVPlayer or Media3 choose a
rung on its own, treating a stationary presentation as bandwidth evidence,
or overriding an explicit viewer quality choice, stop and flag it.** Line
numbers are from `0f02b7ea`; re-verify by function name.

**Correction to the review (one, in its favour), and what became of it:**
§3.8 said Apple's `stallReopenIntent(wedge:)` "has no call site". The private
instance method indeed had none; the `nonisolated static` overload it
forwarded to had four, all in `clients/apple/Tests/AppleClientTests.swift`. So
the plumbing was not merely uncalled, it was **uncalled and pinned by tests**,
which is what made §7.1 a real decision rather than a cleanup. Both overloads
and those call sites are now deleted under D5; the wire they minted onto is
not. §7.1 has the decision and what it did and did not execute.

## 1. Objective

State a design that another session can build from, for adaptive quality on
the Apple and Android clients, such that:

1. The **policy** — which rung, when, and why — is one shared artifact with
   fixtures in `tests/playback`, and each platform supplies its own real
   measurements to it rather than its own idea of what a stall means.
2. Five kinds of evidence are distinguished by name, on every platform:
   **constrained delivery, producer capacity, decoder failure, deliberate
   hold, denied authority.** A sixth state — *unknown* — is explicitly not
   any of them and never triggers a downward move.
3. A healthy rung change uses the prepared handoff; a change made while the
   incumbent is already stalled uses a bounded recovery reopen. The two
   paths are separate because one can afford to wait and the other cannot.
4. Hysteresis, a switch budget and a safe upward-recovery rule exist and
   are the same numbers on every platform.
5. Original, an explicit manual rung, a paused player and HDR fidelity are
   honoured — the controller never silently undoes a viewer's choice.
6. The dormant stall-ticket plumbing is either deleted or revived, with the
   argument recorded either way.
7. Acceptance is one shaped-network trace on every platform with named
   metrics. A green policy unit test does not enable anything.

Board id A-04.

## 2. Contract today

Re-verify at build time; line numbers are from `0f02b7ea`.

### 2.1 The browser has a controller; nothing native does

`autoControllerTick`
([stall-diagnosis.js:368-430](../../crates/plurxd/src/web/player/stall-diagnosis.js))
runs on a timer and is gated first by the server setting:

```js
  if(!(SERVER&&SERVER.playback_auto_abr)||!p||!v||!p.abr||qualityForce()!=='auto'||!p.started||v.paused||p.abr.switching) return;
```

`playback_auto_abr` is a replicated setting (`system.rs:50, 68, 1720`).
No Swift or Kotlin source reads it, and neither native client runs a
throughput/runway loop of its own.

The decision itself is a pure function:
`PlaybackPolicy.decideRung({...})`
([playback-policy.js:299-345](../../crates/plurxd/src/web/playback-policy.js)),
which "never reads hls.js, the DOM, a clock, or persistence" (its own
comment at `:280-282`). Its inputs are the ladder, the current height, two
bandwidth estimates and their age, runway now and previously, the server's
`recent_speed`, an active-stall flag, a stall count, four timestamps, the
player's pixel height, a blocked-height set and a cause-evidence value.
That separation — measurement on one side of a function boundary, policy on
the other — is the single most reusable thing the browser built.

### 2.2 The tuned constants, all in one frozen object

`AUTO_DEFAULTS` ([playback-policy.js:14-39](../../crates/plurxd/src/web/playback-policy.js)):

| Key | Value | What it governs |
|---|---|---|
| `sampleMs` | 5 000 | controller tick |
| `safeEstimateFactor` | 0.95 | headroom on the estimate when picking a rung |
| `severeEstimateRatio` | 0.7 | below this fraction of source bitrate the link is "bandwidth-limited" |
| `mildHeadroom` | 1.3 | margin a rung must clear on a mild downgrade |
| `mildSamples` | 2 | consecutive mild samples before a mild downgrade |
| `cooldownMs` | 20 000 | minimum gap between voluntary switches; an `emergency` decision is exempt. The browser's voluntary gate is in fact `max(cooldownMs, dwellMs)` (build plan M0.2) |
| `upgradeHeadroom` | 1.8 | estimate margin required to go up |
| `upgradeHoldMs` | 45 000 | how long that margin must hold |
| `upgradeSpeedFloor` | 1.15 | predicted post-switch encode pace, x realtime |
| `stallWindowMs` | 60 000 | window over which stall events are counted |
| `dwellMs` | 60 000 | the horizon the restart-cost model amortises over |
| `nearEmptyRunwaySeconds` | 1.5 | runway at or under which the player counts as starving (`nearEmpty`, one input to `starvation`). Urgency, not cause: on its own it makes `decideRung` **suppress** (`insufficient-evidence`), never switch, and it never makes a decision `emergency` — only a fresh bandwidth cliff does (§3.4) |
| `restartCostSeconds` | 2.5 | what a reopen costs the viewer |
| `causeMaxAgeMs` | 15 000 | cause evidence older than three ticks explains nothing |
| `recentSampleMaxAgeMs` | 15 000 | same, for the throughput sample |

`upgradeSpeedFloor`'s comment is the most important sentence in the file
and is a JIT-specific fact no off-the-shelf ABR carries:

```js
    // The estimate alone is not evidence that a higher rung is sustainable.
    // On a JIT server hls.js measures min(link, encode) of the CURRENT rung,
    // so a fast 720p encode reads as ~200 Mb/s and clears any bandwidth bar
    // the 1080p rung can set. What actually fails one rung up is ENCODE
    // headroom, and the server reports it: `recent_speed` is its pace as a
    // multiple of realtime.
```

`predictedSpeed` (`playback-policy.js:286-298`) turns that into arithmetic:
pace scales by the inverse square of height, and a `null` measurement must
not block an upgrade.

### 2.3 The evidence classifier, and what it refuses to conclude

`autoCauseEvidence`
([stall-diagnosis.js:220-258](../../crates/plurxd/src/web/player/stall-diagnosis.js))
returns exactly one `{kind, ageMs, code?}`, in priority order:

| `kind` | Source | Meaning |
|---|---|---|
| `loader-suspended` | `p.hlsStartup.establishedSuspension` on this attachment | the loader is parked; repair the transport, do not change quality |
| `authority-refused` | HTTP 401/403/410, or a code matching `authority\|owner_(lost\|transition)\|node_removal_fenced\|learner_route_ineligible` | this client no longer owns the session |
| `producer-failed` | a terminal startup code, or `health.producer_state == "failed"` | the encode died |
| `delivery-refused` | a code matching `publication\|segment_\|response_` | the server declined to publish |
| `capacity-shortfall` | `0 < health.recent_speed < 1` with `producer_state` in `{running, held}` | the encoder cannot keep up |
| `bandwidth-limited` | a fresh throughput sample below `severeEstimateRatio` x source kb/s | the link is the constraint |
| `stale` / `unknown` | nothing fresh enough | **not** bandwidth pressure |

The last row is the refusal. `causeMaxAgeMs`'s comment: "Cause evidence
older than three controller samples cannot explain the starvation in front
of the viewer. Unknown is not bandwidth pressure."

### 2.4 Two paths out: prepared handoff, and bounded reopen

`switchAutoRung`
([stall-diagnosis.js:278-311](../../crates/plurxd/src/web/player/stall-diagnosis.js))
is the healthy path. It sets `p.autoRequestedHeight` **before** the ask, so
"the very first exchange after this decision already carries the rung and
the server sees a selection change", and it falls back to `reopen()` when
the incumbent cannot hand off:

```js
    // A stalled incumbent has nothing to hand off from. The server cancels a
    // preparation on `waiting` or `stalled` anyway, so asking would spend the
    // whole offer bound to be told no and leave the viewer stalled for it.
    if(!preparedHandoffOffered(p)||!directedChangeIncumbentReady(p,v)){
      await reopen();
      return;
    }
```

`rescueAutoSupply` (`:312-367`) is the stalled path: reached by repeated
supply stalls, it reopens into **Auto** rather than a named rung,
"publishes no requested height", and is wrapped in the same single
automatic claim (`claimAutoFallback`/`releaseAutoFallback`) as the rung
switch, so only one automatic move is ever in flight.

### 2.5 Android already refuses the wrong inference — keep it

`Controller.onStall`
([Controller.kt:1836-1838](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt)):

```kotlin
    /**
     * Called when a detected stall measurement is available. Reopens the same
     * recipe without the legacy stall ticket: a stationary presentation does
     * not prove decoder failure or insufficient capacity, so it cannot ask the
     * server to lower Auto quality. The existing same-rung budget still bounds
     * repeated repairs that do not improve presentation.
     */
```

The body captures ownership before publishing evidence (`stallGuard.observeStall()`),
spends one free Media3 re-evaluation when the item is merely `loaded` and
waiting, and then asks the control plane for a verdict under
`CONTROL_ASK_MS`/`CONTROL_ASK_CAP_MS`. §3.8 says "keep that", and this
design does: the refusal becomes the `unknown` row in §2.3's table rather
than being deleted.

`subtitleSessionBody`
([SubtitlePolicy.kt:363-365](../../clients/android/app/src/main/java/tv/plurx/app/player/SubtitlePolicy.kt))
carries `previousSessionId: String? = null, reopenReason: ReopenReason? = null`
and every caller leaves them at their defaults — the Android half of the
dormant plumbing.

### 2.6 Apple's recovery reopens the same delivery

`observeDeliveryStarvation`
([PlayerController.swift:5064-5090](../../clients/apple/Sources/PlayerController.swift))
is fed by `startStatusPolling` at a 2 s cadence, guards eligibility on
seven conditions, and fires
`retrySameDeliveryAfterStall(event)` with `action: .reopen`. **Same
delivery** — no rung change. `stallReopenIntent(wedge:)` was the only thing
that would have carried a ladder step; it was unreferenced and D5 deleted it.
Its doc comment stated the rule any revival still has to keep, and it is
quoted here because that rule outlived the function:

```swift
    /// A wedge reopens unbound for a different reason. The server reads a
    /// ticketed automatic reopen as evidence that this rung is too heavy for
    /// the link and rewrites it one rung down — right for a slow link, wrong
    /// for a session whose published bytes were simply never fetched, which
    /// must come back on the rung it was already serving.
```

A6's disposition also binds anything that touches the poll: `startStatusPolling`
"feeds `observeDeliveryStarvation` and prepared-switch sampling, not only
the stats panel", so its cadence is recovery evidence and is not to be
backed off for telemetry reasons.

### 2.7 What the server already does with a ticketed reopen

The wire fields are `previous_session_id` and `reopen_reason`
([hls.rs:585-586](../../crates/plurxd/src/http/hls.rs)), and `ReopenReason`
has exactly one variant, `Stall`
([transcode.rs:11023-11027](../../crates/plurxd/src/transcode.rs)) —
deliberately typed "even while `stall` is the only server-normalized cause:
an unknown future value must be refused, not accidentally treated as
ordinary create."

The server consumes it in two places that matter here:

1. **It suppresses prepared-switch accounting.** `hls.rs:7378-7391`: a
   `reopen_reason: Some(Stall)` create is excluded from
   `remember_delivered_selection` because "that is the client adapting to a
   failure, not a viewer asking for something, and §3.3's own rule is that
   M6 prepares for a change the *client asked for*."
2. **It steps the ladder from the prior.** `auto_height_from_prior`
   ([transcode.rs:27749-27787](../../crates/plurxd/src/transcode.rs)) takes
   an `active_starved_rung` as stronger evidence than a healthy EWMA and
   starts below it, then clamps by `sustained_kbps` against each rung's
   `peak_kbps`.

The prior itself is fed by **client telemetry already on the wire**:
`POST /api/v1/client-log` with `{event, bandwidth, height, detail, ua}`
updates the per-credential/UA/subnet network prior when the replicated
setting `playback_network_priors` is on (`system.rs:1717, 1855`;
`store::keys::PLAYBACK_NETWORK_PRIORS`). The web client sends `ttff` and
`stall` events with a `detail` like `supply:empty`. **This is the shared
evidence bus that already exists**, and §3.2's vocabulary is a
formalisation of its `detail` field rather than a new channel.

### 2.8 The one thing no player can do for us

Every rung is its own server session; the master carries exactly one
`#EXT-X-STREAM-INF`
([QUALITY-SWITCH-CONTINUITY-PLAN.md](../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md)
§1.1, and [../streaming/HONEST-MASTER-PLAYLIST.md](../streaming/HONEST-MASTER-PLAYLIST.md)
§4, which keeps it that way). §3.8: "A single-rendition manifest does not
become a multi-rung service because AVPlayer or Media3 supports ABR." Both
native players' own adaptation logic is therefore inert here and must stay
inert; the controller this design describes is the only thing that changes
a rung.

```text
                      one shared policy (pure function + fixtures)
                                    ^
          real measurements         |          real measurements
     +--------------------+         |         +---------------------+
     | hls.js: bandwidth  |         |         | Media3: bitrate     |
     | estimate, runway,  |---------+---------| estimate, buffered  |
     | waiting events     |         |         | position, codec err |
     +--------------------+         |         +---------------------+
                                    |
                      +-------------+--------------+
                      | AVFoundation: accessLog    |
                      | observedBitrate, stalls,   |
                      | loadedTimeRanges           |
                      +----------------------------+

     server evidence, same on all three: session health (producer_state,
     recent_speed, delivered/fetched/published), control verdicts, HTTP status
```

## 3. The design

### 3.1 One policy, three measurement adapters

The policy is a pure decision function with no I/O — the property
`playback-policy.js:280-282` already asserts of `decideRung`. Each platform
supplies an **adapter** that fills the same input record from its own
player, and consumes the same output record.

The input record, named once:

```text
  AutoSample {
    ladder:              [{height, total_kbps, peak_kbps}]  // server-advertised
    currentHeight:       int
    estimateKbps:        float|null   // this instant's throughput estimate
    recentEstimateKbps:  float|null   // the last trustworthy sample
    recentEstimateAtMs:  int|null
    runwaySeconds:       float|null
    previousRunwaySeconds: float|null
    recentSpeed:         float|null   // server encode pace, x realtime
    activeSupplyStall:   bool
    supplyStalls:        int          // within stallWindowMs
    decodeStalls:        int          // within stallWindowMs
    lastStallAtMs:       int|null
    lastSwitchAtMs:      int|null
    mildSamples:         int
    upgradeSinceMs:      int|null
    playerHeight:        int          // presentation pixels, ceiling only
    blockedHeights:      set<int>
    cause:               AutoCause     // §3.2
    nowMs:               int
  }

  AutoDecision { height|null, reason|null, emergency, action, evidence,
                 mildSamples, upgradeSinceMs }
```

Where each field comes from per platform. **This table is a sketch and §8 is
the specification**: reading the three codebases against it found seven
corrections, listed in §8.5, and five fields rather than two that are not
interchangeable. Keep it for the shape; use §8 for the facts.

| Field | Web | Apple | Android |
|---|---|---|---|
| `estimateKbps` | `hls.bandwidthEstimate` | `accessLog().events.last.observedBitrate` (`PlayerController.swift:2408`) | Media3 `BandwidthMeter.getBitrateEstimate()` |
| `runwaySeconds` | `bufferRunway(video)` | `bufferedRunwaySeconds()` (`PlayerController.swift:2440`) | `player.bufferedPosition - player.currentPosition` |
| `recentSpeed` | `health.recent_speed` from the session-status poll | same field, same poll (`startStatusPolling`, 2 s) | same field, same poll (`Controller.kt:2225-2256`) |
| `activeSupplyStall` | `p.waitAt` and runway under the supply threshold | `deliveryStarvation.observe(...)` verdict (`:5072-5080`) | `OpenPlaybackStallTracker.Event` with `controlMayDefer` |
| `supplyStalls` | `p.abr.stallEvents.supply` | starvation events in the window | stall-tracker events in the window |
| `decodeStalls` | `p.abr.stallEvents.decode` | `accessLog().events.last.numberOfStalls` delta | Media3 `PlaybackException` codec errors |
| `playerHeight` | `playerPixelHeight(v)` | layer bounds x `UIScreen.scale` | `SurfaceView` height x density |

Two of these are **not** interchangeable and the adapters must say so:
`observedBitrate` is AVFoundation's own estimate over completed transfers
and is per-item, not per-segment; Media3's bandwidth meter is an
exponential-weighted sliding percentile. Neither is hls.js's EWMA. The
policy's thresholds were tuned against hls.js, so §5.3's shaped trace is
what says whether `upgradeHeadroom = 1.8` means the same thing on all
three — and if it does not, the constant becomes per-platform **data in the
shared fixture**, not a per-platform code branch.

### 3.2 The five evidence classes, named on the wire

`AutoCause` is a closed enum, and it is the same value the client already
puts in `client-log`'s `detail` (§2.7), so adopting it costs no new
endpoint:

| Class | `detail` prefix | Fed by | What the policy does |
|---|---|---|---|
| **Constrained delivery** | `link:` | throughput below `severeEstimateRatio` x source, or a `publication`/`segment_`/`response_` refusal | step down by the estimate; this is the only class that may step more than one rung |
| **Producer capacity** | `encode:` | `0 < recent_speed < 1` with `producer_state` in `{running, held}` | step down one rung; **never** step up, whatever the estimate says |
| **Decoder failure** | `decode:` | a codec error, a repeated decode stall at an unchanged presentation time with a healthy buffer | add this height to `blockedHeights` and step down **one** rung. That is the quality controller's whole response, and it never changes delivery method (§4). A rung change is not a cure for a codec the device cannot decode, so a second decode failure is **not** answered with a third rung: the controller takes no further decode move, and the item failure belongs to the platform's compatibility owner (Apple's compatibility ladder, Android's compatibility budget, the web's decode rescue), the only thing allowed to change delivery |
| **Deliberate hold** | `hold:` | `loader-suspended`, or a `hold`/`retry_resource` verdict that answered **this client's own `stalled` ask** (fixture kind `control-stall-verdict`) | **no downward move** while it is in force, bounded by the client's stall deferral (20 s on the web). Repair the transport or wait out the verdict. Upgrades need no rule of their own: the stall that prompted the ask already refuses them through `stallFree`. **Not** fed by the routine advisory hold or by `producer_state == "held"`, which are the healthy paced steady state (below) |
| **Denied authority** | `authority:` | 401/403/410, `owner_lost`, `owner_transition`, `node_removal_fenced`, `learner_route_ineligible` | **do nothing to quality.** Re-establish ownership; a rung change on a session you no longer own is a second session |
| *(unknown / stale)* | `unknown:` | nothing fresher than `causeMaxAgeMs` | **do nothing.** Not bandwidth pressure (§2.3) |

Android's existing refusal (§2.5) is exactly the last row, and is preserved
by construction: a stationary presentation with no fresh cause is
`unknown`, and `unknown` never moves the rung.

The `hold:` and `authority:` rows are the ones a naive port would get
wrong — `hold:` in both directions, as the next paragraphs show — and they
are why the classifier is part of the shared policy rather than each
platform's own reading of "it stalled".

**What the server calls a hold, and which half of it is evidence.** Most
`hold`s on the wire are not trouble. `resolve_action`
(`crates/plurxd/src/playback_control.rs:2027-2087`) runs on every control
exchange (`http/hls.rs:4911`) and returns `ControlAction::Hold` whenever
`delivery.hold_reason` is set, and the reasons include the rolling
producer's ahead-window `demand`/`time`/`bytes`/`global` (`HoldReason`,
`:1634-1642`, mapped from `AheadHoldReason` at `:1143-1150`). The only
exception, `recovery_outranks_hold` (`:2017-2025`), withholds it from a
client that reports `Stalled` with loaded or fetchable media, and its own
doc says "A quiet or paused player must still receive the producer hold it
asked for" (`:2016`). `producer_state` likewise reads `"held"` whenever the
producer is suspended (`transcode.rs:8372-8373`). So a hold with a big
buffer, a fast link and encode headroom is the ordinary steady state of a
JIT producer paced ahead of its viewer, which is the state upward recovery
is supposed to happen in, and §2.3's `capacity-shortfall` already counts
`held` as a live producer. That half is **not evidence in either
direction**; the fixture names it `producer-paced` and lists it under
`not_evidence`, and a case pins that it never blocks an upgrade.

`retry_resource` is a different kind of answer: `resolve_action` builds it
only from a non-permanent `producer_decision` (`:2039-2068`), a producer
that stopped, never from pacing. On a routine exchange no client retains it
and it is not evidence on its own; if a stall follows, the ask that stall
prompts returns it again.

The half that *is* evidence is the verdict that answered the client's own
`stalled` ask, and that is also the only place any client consumes one
today. The web keeps only `terminal` verdicts from routine exchanges
(`web/player/directed-change.js:426-430`) and acts on a
`hold`/`retry_resource` only inside `persistentWait`
(`web/player/measurements.js:498-549`), after that wait has already
recorded the stall (`:416` → `recordWaitStall` → `noteAutoStall`, which sets
`abr.lastStallAtMs`, `:271-284`). Android's `hold`/`retry_resource` arm is
in `applyStallVerdict` (`Controller.kt:1849`), reached only from `onStall`
(`:1945`). The web's deferral is capped at `CONTROL_STALL_DEFER_DEADLINE_MS
= 20000` (`measurements.js:244`), well inside `stallWindowMs = 60000`, so
while a web stall verdict is in force `stallFree`
(`playback-policy.js:507-508`) already refuses every upgrade. What no
platform does is hand the verdict to `decideRung`, so a fresh slow transfer
during a held stall — which on a JIT server measures the paused producer,
not the link — can still take the emergency downswitch. That downward gap
is the fixture's `control-stall-verdict` case and the build plan's M0.1.

An earlier draft of this section fed `hold:` from `producer_state == "held"`
and any `hold`/`retry` verdict, told the policy to "do nothing to quality",
and called the missing verdict "a live web defect" that let a hold with a
healthy buffer be upgraded through. That premise was wrong for the reasons
above, and built as written it would have stopped Auto upgrading on every
session whose producer is paced ahead.

### 3.3 Prepared handoff when healthy, bounded recovery when stalled

The same split the browser makes (§2.4), stated as the rule:

```text
  decision to change rung
        |
        +-- incumbent ready (playing, runway above the emergency floor,
        |   a prepared offer is outstanding on this playback)
        |        -> declare the rung on the NEXT control exchange, let the
        |           server prime a successor, switch on commit
        |           (CLIENT-PREPARED-SWITCH-CONTRACT.md's eight steps)
        |
        +-- incumbent stalled, or no offer, or the offer names a different
            playback -> bounded recovery reopen:
                 * one automatic move in flight, ever (claim/release)
                 * carry the rung explicitly when the cause names a rung
                   (link:, encode:), carry Auto when it does not
                 * spend the platform's existing same-rung repair budget
                   first, not a new one
```

The reason the stalled branch does not simply ask is in the browser's own
comment (§2.4): the server cancels a preparation on `waiting`/`stalled`
anyway, so asking costs the whole offer bound and leaves the viewer
stalled for it. That is a server behaviour, not a browser quirk, so it
binds all three adapters.

The prepared path also needs the ask to carry the rung *before* the
exchange, not after — the `p.autoRequestedHeight` ordering at
`stall-diagnosis.js:305-308`. On Apple and Android the equivalent is
setting the desired quality on `PlaybackIntent` / the controller's pending
selection before the next `reportControlEvidence`, and both clients already
have that seam (`PlaybackIntent.adoptQuality`, `PlaybackIntent.kt:64`).

### 3.4 Hysteresis, budget, and going back up safely

- **Cooldown.** No voluntary switch within `cooldownMs` of the last one
  (the browser's gate is in fact `max(cooldownMs, dwellMs)`, build plan
  M0.2). The one exemption is an `emergency` decision, and only a **fresh
  bandwidth cliff** makes one: a completed-transfer sample no older than
  `recentSampleMaxAgeMs` below `severeEstimateRatio` x the current rung
  (`freshBandwidthCliff`, `playback-policy.js:361-363`; `severe`, `:402`).
  A runway at or under `nearEmptyRunwaySeconds` is **not** an emergency: it
  feeds `starvation` (`:359-365`), which without a fresh cliff makes the
  decision `suppressed`/`insufficient-evidence` (`:382-384`) — "Empty runway
  establishes urgency, not cause" (`:420`). An adapter that downswitched on
  an empty runway, cooldown or not, would be inferring bandwidth from a
  stationary presentation, which §2.5 and §4 forbid.
- **Mild downgrades need repetition.** `mildSamples` consecutive samples
  over `mildHeadroom`, so a single bad sample does not cost a restart.
- **Restart cost is amortised, not ignored.** `restartCostSeconds` against
  `dwellMs` is the browser's existing model (`playback-policy.js:270-278`);
  a downgrade that would not pay for itself over the dwell horizon is not
  taken.
- **Switch budget.** A hard count of automatic rung changes per playback
  per hour. The browser has no such counter today — it has a cooldown and a
  stall window, which bound the *rate* but not the *total*. §3.8 asks for a
  budget by name. Proposal: **six automatic changes per playback-hour**,
  after which the controller holds the current rung and raises the existing
  `degraded_notice` surface once; the budget resets on a viewer-initiated
  quality change or a seek. The number is a starting proposal to be moved
  by §5.3's traces, not a measurement.
- **Upward recovery is gated on encode headroom, not bandwidth.**
  `upgradeHeadroom` x the target rung's `total_kbps` must be clear for
  `upgradeHoldMs`, **and** `predictedSpeed(recentSpeed, current, target)`
  must be at or above `upgradeSpeedFloor`, **and** the target must not be
  in `blockedHeights`, **and** the cause must not be `encode:`. A `null`
  `recentSpeed` does not block the upgrade (`playback-policy.js:292-296`);
  absence of evidence is not evidence.
- **One rung at a time upward, always.** Downward may skip rungs only under
  `link:` evidence with a fresh estimate.

### 3.5 What the controller must not override

| Viewer state | Rule | Why |
|---|---|---|
| **Original** | the controller is off entirely | Original is a fidelity promise, not a rung; stepping it down is the advertised-vs-delivered defect `auto_height` already fixed once (`transcode.rs:27655-27660`) |
| **Manual rung** | off; `qualityForce() !== 'auto'` is already the browser's first gate | an explicit pick is an instruction, not a hint |
| **Paused** | off; resume re-arms after one full sample, not immediately | a paused player's runway is meaningless and its estimate is stale |
| **Background / PiP** | off while not presenting | same reason, plus D2's lifecycle work owns what "backgrounded" means on Android |
| **HDR fidelity** | a rung change must not silently change `OutputGrade` | an unexpected SDR transition is one of §3.8's named acceptance metrics; the HDR10 rung exists only at specific heights (`hdr10_rung_fits`, `transcode.rs:27619-27637`), so a downward step out of HDR must be *reported*, and preferably refused while the cause is `link:` only |
| **A stall-scoped control verdict** | no downward move while it is in force (the stall deferral, 20 s on the web). Not a tick gate, and not the routine paced hold, which gates nothing | §3.2's `hold:` row and the paragraphs after it: the server sends `hold` on every exchange while a paced producer is ahead, so a gate keyed on any hold would freeze Auto in the healthy state |

### 3.6 Shared fixtures, and what belongs in them

`tests/playback/` already holds cross-platform contract fixtures that both
native clients are held to — `player-input-contract.json`,
`playback-surface-contract.json`, `decode-limit-identity.json` — and the
web side embeds generated tables from them
(`playback-policy.js:50-52`: "generated from
tests/playback/player-input-contract.json by scripts/player-contract-table
--embed; do not edit by hand"). That is the mechanism.

Add `tests/playback/auto-quality-policy.json`:

```text
  {
    "schema": 1,
    "defaults": { ...AUTO_DEFAULTS, plus switchBudgetPerHour... },
    "causes": ["link","encode","decode","hold","authority","unknown"],
    "cases": [
      { "name": "a stale sample never steps down",
        "sample": {...}, "expect": {"height": null, "reason": null} },
      { "name": "encode shortfall steps one rung and blocks the upgrade",
        "sample": {...}, "expect": {"height": 720, "reason": "encode"} },
      ...
    ]
  }
```

Consumed by `node tests/playback/web-policy.test.js` (already in
`make web-check`, `Makefile:1421`), by a Swift test in
`clients/apple/Tests/`, and by a JVM test under `make android-test`. One
JSON, three runners: that is what "share policy fixtures across platforms"
means concretely, and it is the only part of this design that can be built
before the measurement work.

The cases must include, at minimum, one per row of §3.2's table, both
hysteresis boundaries (just inside and just outside `cooldownMs`), the
upgrade gate with and without `recentSpeed`, the blocked-height path, the
switch-budget exhaustion, and every row of §3.5.

### 3.7 Telemetry, so a trace can be read afterwards

The existing `client-log` beacon (§2.7) gains nothing new; the `detail`
field takes §3.2's prefixes. What is missing is a *decision* record. The
browser has one — `recordAutoDecision`
(`stall-diagnosis.js:259-277`) — and it is local. Proposal: every automatic
decision, including a suppressed one, emits one beacon with
`{event: "auto_decision", height_from, height_to, reason, cause, runway_ms,
estimate_kbps, recent_speed, budget_left}`. Bounded: at most one per
controller tick, i.e. one per `sampleMs`, per session.

Server-side, one metric with bounded labels:

```text
  # HELP plurx_auto_quality_decisions_total Automatic rung decisions reported by clients.
  # TYPE plurx_auto_quality_decisions_total counter
  plurx_auto_quality_decisions_total{platform="web|apple|android",action="down|up|hold|suppressed",cause="link|encode|decode|hold|authority|unknown"} N
```

Both label sets are closed enums; the values come from the typed beacon,
never from a free string. No new settings key: `playback_auto_abr` is the
switch that already exists. It is replicated (`http/system.rs:71-75`, off
unless stored as `1`), but it is **not** in Settings → Developer and has no
readiness information: it is an ordinary toggle, "Adjust Auto quality while
playing", in the Playback panel's Streaming card
(`web/pages/settings-panels.js:546`), and `http/system.rs` builds no
readiness entry for it. The project rule is that optional functionality
lives in Developer with advisory readiness that never gates enablement, so
moving it there, with per-platform readiness rows such as "controller
present on this client" and "shaped trace recorded", is follow-up **F-1** in
the build plan. A-04 does not move it.

## 4. Guardrails (non-goals)

- **Not a multi-variant manifest.** §3.8, verbatim: "A single-rendition
  manifest does not become a multi-rung service because AVPlayer or Media3
  supports ABR." The native players' own ABR stays inert;
  [../streaming/HONEST-MASTER-PLAYLIST.md](../streaming/HONEST-MASTER-PLAYLIST.md)
  keeps the master at one variant and is not a prerequisite for this work
  (it does make `indicatedBitrate` honest, which this design's Apple
  adapter would otherwise have to work around — see §7.4).
- **Android's `onStall` refusal is preserved, not ported around.** §3.8:
  "Android's `onStall` correctly refuses to treat every stationary
  presentation as bandwidth evidence; keep that." §3.2's `unknown` row is
  that refusal, generalised to all three platforms.
- **Per-player measurements, not a modelled link.** §3.8: "share policy
  fixtures and evidence semantics across platforms **over each player's
  real measurements**." §3.1's adapter table is the whole point; no
  platform infers a number another platform could have measured.
- **Not enabled on a pure policy test alone** (§3.8's last sentence, and
  §5.4's gate). A green `auto-quality-policy.json` run is necessary and
  nowhere near sufficient.
- **No new recovery ladder.** Apple's compatibility ladder and Android's
  compatibility budget own codec/HDR fallback
  (`PlayerController.swift:5093-5097`: "Actual item failures remain the
  sole owner of the codec/HDR compatibility ladder"). The quality
  controller adds a height to `blockedHeights` and steps down one rung,
  once; it does not change delivery method, and a second decode failure is
  the compatibility owner's, not a third rung (§3.2's `decode:` row says the
  same).
- **A6's poll cadence is not touched.** `startStatusPolling`'s 2 s is
  recovery evidence, and this design adds a *reader* of it, which makes
  backing it off worse, not better.
- **No in-code feature gate.** `playback_auto_abr` is a replicated setting
  already; a native tick reads the same one, exactly as the web tick does,
  once it merges. "Ships disabled" (§5.4) is not a flag: a platform's tick
  does not merge until its shaped trace exists, and that trace runs on an
  unmerged build of the milestone's own branch, sideloaded onto the lab
  devices (build plan M3). Where the switch lives in the UI is follow-up
  F-1 (§3.7).
- **Viewer intent is never silently overridden** (§3.5). A controller that
  moves a rung raises the existing `degraded_notice` surface exactly as the
  web one does, so the change is visible.
- **Live TV is out of scope.** Its rung story, its `BEHIND_LIVE_WINDOW`
  handling (D3) and its shared-transport plan are separate; this design is
  finite playback only.

## 5. Milestones

This is a design document; its milestones are design deliverables, one
decision, and the build plan that follows. The only production client change
A-04 makes is D5's deletion of two Apple functions that had no call site,
which changes no behaviour; nothing else under A-04 touches runtime code.

### 5.1 D1 — the shared policy artifact and its fixtures — DELIVERED

`tests/playback/auto-quality-policy.json`, schema 1, 30 cases and 12
controller-gate rows, driven by `node tests/playback/web-policy.test.js`.
Five cases (M0's four disagreements plus the unimplemented switch budget)
and three gate rows carry a `web_current`/`finding` disagreement; they are
summarised in §8.4 and §7.6.

Deliverable: `tests/playback/auto-quality-policy.json` at schema 1 (§3.6),
plus `web-policy.test.js` extended to drive `decideRung` from it, so the
browser's *existing* behaviour is captured as the baseline before anything
native is built. Any case where today's browser disagrees with the intended
policy is recorded in the JSON as `"web_current"` beside `"expect"` — a
disagreement is a finding, not a test failure to paper over.

Acceptance: `node tests/playback/web-policy.test.js` green with the new
cases; `make web-check` green; every row of §3.2 and §3.5 has at least one
case; the file is under 600 lines or it is too clever.

### 5.2 D2 — the adapter specification, per platform — DELIVERED

§8.

Deliverable: §3.1's table turned into a written specification per platform
naming, for each input field, the exact API, its units, its update cadence
and its failure mode (what the adapter supplies when the API returns
nothing). Plus, for each platform, where in the existing code the
controller tick would live and which existing guard it must sit behind
(`isChangingStream`, `seekState.allowsStallRecovery`, `stallGuard`,
`playbackOwnsAttachedMedia`).

Acceptance: a reviewer can point at every field in §3.1's `AutoSample` and
find the API that fills it on all three platforms, or find a written
statement that it is unavailable there and what the policy does about it.

### 5.3 D3 — one shaped-network trace per platform, on today's build — NOT RUN

**No trace has been taken on any platform and no metric in this section has a
value.** The prompts in §6 are the work; it needs a deployed build, three
browsers and five physical devices, none of which the executing session has.
Recorded as `needs:` in the execution log under the work board's rule 9.

This is the measurement §3.8 requires, and it runs **before** any
controller exists, so the "after" has a "before".

The instrument exists. `scripts/playback-lab` carries a loopback token-
bucket shaper with a mandatory descent (`playback-lab:42-98`; a profile
that rises or holds flat is refused, because "silently accepting one would
let a green stall-recovery run mean nothing at all"), and two commands
built for physical devices:

```text
  scripts/playback-lab device-proxy --target URL --listen HOST --public-host HOST
                              --network-profile PROFILE --control-file PATH
                              [--port PORT] [--auto-advance]
                              [--recovery-cliff-after SECONDS]
  scripts/playback-lab device-run --device UDID --target URL --public-host HOST
                              --file-id ID --network-profile PROFILE [--item-id ID]
```

The named profile is `8mbps-to-1.5mbps` (PERF2-PLAN §7's acceptance shape);
the grammar accepts `8mbps-to-1.1mbps-to-350kbps@12` for a two-cliff
descent. `device-run` is Apple-only (it takes a UDID); Android points the
APK at `device-proxy`'s `--public-host` and is driven by hand or by `adb`.
That asymmetry is D3's one build item: either an Android `device-run`, or a
written manual protocol. Prefer the protocol first — a harness for a
measurement nobody has taken yet is speculative.

The metrics, which are §3.8's list made countable:

| Metric | Definition |
|---|---|
| Stalled seconds | wall seconds with the presentation clock stationary while `wantsPlayback` |
| Switches | automatic rung changes (0 on today's build, by construction) |
| First-frame gap | seconds from a switch or reopen to the next presented frame |
| Quality regained | seconds from the cliff to the first frame at a rung the link can sustain |
| Unexpected SDR transitions | count of HDR -> SDR grade changes the viewer did not ask for |
| Delivered vs advertised | mean delivered kb/s over the post-cliff window, against the rung's `total_kbps` |

Acceptance: one JSON report per platform per profile, normalized with
`scripts/playback-lab normalize` so two runs compare, all six metrics
filled, recorded under a dated heading in this document. The GPT prompt is
in §6.

### 5.4 D4 — the build plan — DELIVERED

[NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md](NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md),
board row A-05. It opens on D3's traces by construction: its M0 is the four
disagreements, and no adapter milestone can start before the baseline exists.

Deliverable: a separate implementation document (a new board row) that
takes D1's fixtures, D2's specification and D3's baseline and sequences the
work — shared policy first, one platform second, the second platform only
after the first has a shaped trace that improves on D3's baseline.

The enabling gate, written into that plan and repeated here because it is
§3.8's own condition: **a platform's controller ships disabled until its
shaped-network trace shows stalled seconds down and unexpected SDR
transitions at zero against D3's baseline for that platform.** A green
`auto-quality-policy.json` run is not that evidence. The trace runs on an
unmerged build of that milestone's branch, built from its PR head and
sideloaded onto the lab devices with `playback_auto_abr` on for the lab
server; D3's baseline is the same matrix on a `main` build. The PR merges
after the trace, so `main` never carries an unmeasured tick.

### 5.5 D5 — the dormant-plumbing decision — DELIVERED

Answered and executed; §7.1.

§7.1 is Paul's call. D5 is writing it down and executing it: either four
files' worth of deletion, or a revival with the call sites the design
requires.

## 6. Verification and rollout

Lanes: `make web-check` for D1 (it already runs `web-policy.test.js`),
`make android-test` and the Apple test target for D2's fixtures once they
exist. `make unit` is unaffected — no Rust changes in A-04 — except that
§3.7's metric, if it lands with the build plan, adds
`cargo test -p plurxd metrics_auto_quality`.

**GPT prompt — shaped-network baseline, all platforms (D3):**

```text
With the current plurx build deployed to media1 and the repo checked out on
a controller machine that can reach it:

1. Browser baseline, three browsers:
   scripts/playback-lab run --suite stall-recovery --browser chrome \
     --network-profile 8mbps-to-1.5mbps --json out/web-chrome.json
   Repeat with --browser safari and --browser firefox. Then
   scripts/playback-lab normalize --json out/web-chrome.json for each.

2. Apple physical devices (Apple TV 4K and an iPhone):
   scripts/playback-lab device-run --device <UDID> --target http://media1:32400 \
     --public-host <controller LAN name> --file-id <id of a 4K HDR title> \
     --network-profile 8mbps-to-1.5mbps --observe 180 --json out/apple-<device>.json

3. Android (Lenovo Android TV, Google TV, Shield, and an Android phone):
   start the shaper by hand —
   scripts/playback-lab device-proxy --target http://media1:32400 \
     --listen 0.0.0.0 --public-host <controller LAN name> \
     --network-profile 8mbps-to-1.5mbps --control-file out/android-control.json \
     --auto-advance
   point the app's server URL at the proxy, play the same title for 180 s,
   then stop the proxy and keep its control file.

4. For every run report: stalled seconds, number of automatic quality
   changes, first-frame gap after each reopen, seconds from the cliff to the
   first sustainable frame, any HDR-to-SDR transition, and the delivered
   kb/s over the last 60 s. Take the stall/quality facts from the playback
   info panel on native and from the report JSON on web, and paste the
   Settings -> Logs beacons for each session.

5. Repeat the whole matrix once with --network-profile
   8mbps-to-1.1mbps-to-350kbps@12 so there are two cliffs of evidence.
Report one table per platform.
```

**GPT prompt — HDR fidelity under the cliff (D3, second pass):** the same
matrix on a Dolby Vision title, reporting only whether the picture ever
leaves HDR and at which moment, since that is the one metric a report JSON
cannot see.

Rollout: A-04 lands as one draft PR carrying this document's updates
(D1's fixture file, D3's results tables, D5's decision) into `main` under
the fast lane. The build plan D4 produces gets its own board row and its
own PRs. No setting, no metric and no client behaviour changes under A-04.

## 7. Open questions

### 7.1 Delete or revive the stall-ticket plumbing — ANSWERED 2026-09-23

**The decision:** revive the **wire** (`previous_session_id` +
`reopen_reason`) and delete the **Apple helper**. Taken by the repository
owner on 2026-09-23 and recorded on the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md). The
recommendation below was put to him as written and accepted as written; the
two cases are kept because the reasons are what bind the build plan, not
because the question is still open.

**What A-04 executed.** `PlayerController.stallReopenIntent(sessionId:isVOD:requestId:wedge:)`
and the private `stallReopenIntent(wedge:)` that forwarded to it are deleted
(`clients/apple/Sources/PlayerController.swift`), together with the three test
call sites that existed only to exercise them and the one assertion inside the
delivery-watchdog floor test. Nothing in `clients/apple/Sources` constructed a
`.stallReopen` intent through either, so no code path can reach a different
answer; the deletion changes no behaviour and is not covered by a new test,
because there is no behaviour to assert the absence of. It is anchored in
`tests/client-fixes.toml` to the doc comment a revival would have to delete.

**What A-04 deliberately did not execute, and why.** `ReopenReason` is **not**
widened from `Stall` to the five causes. §6 of this document states that A-04
makes no Rust changes and §5 that it makes no production client behaviour
change, and widening the enum is both. More to the point it would be
unexercised wire: the server's two consumers act on a ticket by *stepping the
ladder down* (`auto_height_from_prior`) and by *suppressing prepared-switch
accounting* (`hls.rs`), and neither is the right response to `hold:` or
`authority:`, where §3.2 says do nothing at all. Adding four variants that all
behave like `Stall` would make the field's name a lie; adding four variants
with distinct server behaviour is a server change that needs a shaped trace
behind it. Both belong to the build plan, whose M1 (§3 of
[NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md](NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md))
owns them.

**What survives on Apple, and is meant to.** `PlayerOpenIntent.stallReopen`,
`StallReopenTicket`, the ticket branch of `applyOpenIntent`,
`unboundStallRetry`, `stallReopenReason`, the `.stallReopen` arm of
`StallReopenBudget`, and `previousSessionId`/`reopenReason` on
`CreateSessionRequest`. On Android, `subtitleSessionBody`'s
`previousSessionId`/`reopenReason` parameters stay at their defaults for the
same reason: they are the wire, not the helper. The controller the build plan
writes mints onto all of this rather than rebuilding it, which is the entire
content of "revive the wire".

**The case for deleting the helper:**

- It never ran in production on either native client. Code that has never
  executed is not "nearly working"; it is unmeasured.
- Its semantics are already subtly wrong for §3.2. A ticket says "this rung is
  too heavy for the link" and the server steps one rung down. That is the
  `link:` row and only the `link:` row. Under `encode:` the right move is also
  one rung down but for a different reason, under `decode:` the rung must be
  *blocked* rather than merely stepped, and under `hold:`/`authority:` no move
  is right at all. A single untyped ticket cannot carry five causes.
- Apple's own doc comment described a special case it got wrong (`wedge`),
  which is a sign the abstraction sat at the wrong level.
- Deleting it removes a false affordance: the next reader sees "the native
  clients have no ladder plumbing" rather than "they have some, which must
  nearly work".

**The case for keeping the wire:**

- The server half is built, typed, tested and already exercised by the web
  client. `ReopenReason` was deliberately made an enum so an unknown value is
  refused rather than coerced — it was designed to grow.
- `auto_height_from_prior` and the network prior (§2.7) are a *server-side*
  memory of what starved, shared across sessions and clients. A native client
  that reports nothing contributes nothing to it, and one that reopens without
  a ticket actively discards evidence the server would have used.
- The prepared-switch suppression in `hls.rs` depends on the ticket to tell a
  client-driven failure recovery from a viewer's ask. Native clients that
  start changing rungs without it would pollute the accounting
  [CLIENT-PREPARED-SWITCH-CONTRACT.md](../playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md)
  measures.
- Reviving is strictly less work than re-inventing: the field names, the wire,
  the server behaviour and the Apple tests all exist.

The third point of the deletion case is the one that decided it, and it is
also the instruction the build plan inherits: widen `ReopenReason` from
`Stall` to the five causes **in the change that first sends one**, so the
field stops meaning "the link was slow" and starts meaning what the client
actually observed. The argument against the wire was entirely about its single
untyped cause; typing the cause answers it.

### 7.2 Do the browser's constants transfer?

`upgradeHeadroom = 1.8`, `severeEstimateRatio = 0.7` and
`upgradeSpeedFloor = 1.15` were tuned against hls.js's EWMA. AVFoundation's
`observedBitrate` and Media3's sliding percentile have different bias and
different lag. D3's traces are the first evidence; if the constants differ
per platform they belong in the fixture file as per-platform data (§3.1),
never as a branch in three codebases.

### 7.3 Is a switch budget the right bound, or is a switch *rate*?

§3.4 proposes six per playback-hour. The alternative is to let the existing
`cooldownMs` + stall window bound the rate and add no counter, which is
what the browser does today and which has not visibly misbehaved. A budget
is a stronger promise to the viewer ("this will settle") and a weaker one
to the link ("even if conditions keep changing"). D3's traces should show
how many switches a real cliff actually provokes; if it is two, the budget
is theatre.

### 7.4 Does this need the honest master playlist first?

Not strictly. The Apple adapter's `estimateKbps` comes from
`observedBitrate`, which is measured, not advertised — so it is correct
today. But `indicatedBitrate` (advertised) is what the panel compares
against, and S-10
([../streaming/HONEST-MASTER-PLAYLIST.md](../streaming/HONEST-MASTER-PLAYLIST.md))
makes it honest. If S-10 lands first, the Apple adapter can use the ratio
directly as a second signal; if it does not, the adapter must not, and D2's
specification should say so explicitly rather than leaving it to the
implementer.

### 7.5 What drives the Android shaped run?

D3 proposes a manual protocol through `device-proxy` rather than an
Android `device-run`. If the fleet ends up running this trace more than
twice, the harness is worth building — `adb shell am start` plus the
existing control-file protocol is most of it. Decide after D3.

### 7.6 Does `link:` really carry a server's refusal to publish? — opened by D1

§3.2's `link:` row is fed by "throughput below `severeEstimateRatio` x source,
**or** a `publication`/`segment_`/`response_` refusal", and its action is to
step down by the estimate. `decideRung` disagrees: it puts `delivery-refused`
in its `namedSuppression` list and retains the rung, on §2.3's argument that a
server declining to publish is not the link being slow.

The browser is almost certainly right. `response_owner_transition` and
`segment_pending` describe a server that is moving or still building, not a
link that cannot carry the rung, and stepping a rung on one of them would show
the viewer a quality drop caused by an ownership move. But this document is
what three adapters will be written from, so the table cannot simply be
corrected in passing by an executing session. The case is recorded in
`tests/playback/auto-quality-policy.json` with `expect` reading the design and
`web_current` reading the code, and it is the build plan's M0 to settle.

The likely settlement, for whoever takes it: `link:` is throughput evidence
only, and a delivery refusal becomes a sixth class or joins `hold:` — it is a
"repair the transport, do not change quality" answer. If it joins `hold:`,
it joins the stall-scoped half (§3.2), never the routine paced hold.

### 7.7 Can Apple see a cliff at all? — opened by D2

`decideRung`'s emergency branch fires only on `recentEstimateKbps`, a
per-completed-transfer sample, and Apple has no such measurement today
(§8.4.1). Until D3 says whether successive access-log events yield one, the
Apple adapter's `link:` class has no evidence and its emergency branch is
unreachable. This is not a reason to weaken the branch to accept
`observedBitrate`: the browser's `upgradeSpeedFloor` comment explains why a
smoothed estimate is the wrong input at a cliff, and the same reasoning
applies to a smoothed estimate used as cliff evidence. It is a reason to
measure first.

## 8. The adapter specification (D2)

Written 2026-09-23 against `main` @ `8839cc72`, by reading the three
codebases rather than the review's summary of them. Where this section
contradicts §3.1's table, this section is the later reading and §8.5 lists
each correction with the evidence.

D2's acceptance is that "a reviewer can point at every field in §3.1's
`AutoSample` and find the API that fills it on all three platforms, or find a
written statement that it is unavailable there and what the policy does about
it." §8.1–§8.3 are that, field by field. **Six fields are unavailable on at
least one platform**, and those six are the whole content of this section —
the rest is bookkeeping.

### 8.1 Web — the reference, and what it measures

The browser is not the specification; it is the only implementation, so its
numbers are what the constants were tuned against (§7.2). Cadence for
everything below is `autoControllerTick`'s `sampleMs`, 5 s.

| `AutoSample` field | API | Units | Cadence | When it has nothing |
|---|---|---|---|---|
| `estimateKbps` | `p.hls.bandwidthEstimate`, else `p.bandwidthSeedBps` | bits/s ÷ 1000 | hls.js EWMA, updated per fragment | falls back to `p.priorKbps`, else `null`; a null estimate makes `estimatePressure` and the upgrade gate both false |
| `recentEstimateKbps` / `recentEstimateAtMs` | `p.abr.recentEstimateKbps` from `transferSampleKbps({loadedBytes, loadingStartMs, loadingEndMs})` | kb/s, ms | per completed fragment | stale past `recentSampleMaxAgeMs`; the severe branch then cannot fire at all |
| `runwaySeconds` | `bufferRunway(video)` | s | per tick | `null`; `nearEmpty` is false, not true |
| `previousRunwaySeconds` | `p.abr.previousRunway`, written by the previous tick | s | per tick | `null`; `draining` is false, so `serverPressure` cannot fire on the first sample |
| `recentSpeed` | `health.recent_speed` from `pollSessionHealth` | × realtime | per tick (the tick awaits the poll) | `null`; `predictedSpeed` returns `null` and the upgrade is **not** blocked |
| `activeSupplyStall` | `!!p.waitAt && runway < SUPPLY_RUNWAY_SECS` | bool | per tick | false |
| `supplyStalls` | `p.abr.stallEvents.supply.length`, pruned to `stallWindowMs` | count | per event | 0 |
| `decodeStalls` | `p.abr.stallEvents.decode` — **collected but not passed to `decideRung`** | count | per event | see §8.4 |
| `lastStallAtMs`, `lastSwitchAtMs`, `mildSamples`, `upgradeSinceMs` | `p.abr.*`, written back from the previous decision | ms / count | per tick | `null`/0 |
| `playerHeight` | `playerPixelHeight(v)` — CSS height × `devicePixelRatio`, ratio clamped to 4, else the intrinsic decoded height | pixels | per tick | `Infinity`; a ceiling only, which is why it can never strand a downgrade |
| `blockedHeights` | `p.abr.failedHeights`, written by `maybeDecodeRescue` | set | per decode failure | empty |
| `cause` | `autoCauseEvidence(p, now)` | see §2.3 | per tick | `{kind:"unknown"\|"stale"}` |

Tick location: `autoControllerTick`, `crates/plurxd/src/web/player/stall-diagnosis.js`.
Guards, in the order the function applies them — these are the eight guards
the fixture's `controller_gates` pins, each asserted on its own side of the
awaited health poll: `playbackOwnsAttachedMedia(p)`,
`SERVER.playback_auto_abr`, `qualityForce()!=='auto'`, `!p.started`,
`v.paused`, `p.abr.switching`, `p.autoFallbackInFlight`, and then the whole
identity tuple (`PLAYER`, `mediaAttachment`, `sessionId`, `streamId`)
re-checked after the awaited health poll.

### 8.2 Apple — what exists, and the two fields that do not

| `AutoSample` field | API | Units | Cadence | When it has nothing |
|---|---|---|---|---|
| `estimateKbps` | `PlayerController.observedBitrate` → `player.currentItem?.accessLog()?.events.last?.observedBitrate` (`PlayerController.swift`) | **bits/s**, so ÷1000 | **not a timer.** AVFoundation appends an access-log event on item and rendition transitions, so the value changes when an event is appended, not every 2 s | `nil` when there is no item or no events. AVFoundation uses a negative value for unknown, so the adapter must treat `< 0` as `nil` — `stalls` and the prepared-switch sampler already do exactly that |
| `recentEstimateKbps` / `recentEstimateAtMs` | **none** — see §8.4.1 | — | — | the adapter supplies `null`, and the consequence is that the whole severe branch is unreachable |
| `runwaySeconds` | `bufferedRunwaySeconds()` (`PlayerController.swift`) — playable seconds *contiguous with the clock*, a later island past a gap deliberately excluded | s | on demand | `nil`, and its own doc comment states the rule: `nil` means unknown and the watchdog must not read it as healthy |
| `previousRunwaySeconds` | adapter state; no API | s | per tick | `null` |
| `recentSpeed` | `PlaybackSessionStatus.recent_speed` from `startStatusPolling` | × realtime | 2 s | `nil` before the first poll and from a server predating the field; must not block an upgrade |
| `activeSupplyStall` | `DeliveryStarvationDetector.observe(...)` (`PlayerController.swift`), fed by `observeDeliveryStarvation` | bool | 2 s | false. **Not the same predicate as the web's** — see §8.4.2 |
| `supplyStalls` | starvation firings kept in a `stallWindowMs` window — adapter state; nothing counts them today | count | per firing | 0 |
| `decodeStalls` | `accessLog().events.last.numberOfStalls` delta, with `PlaybackStallObservationState.take(numberOfStalls:)` already handling the per-event baseline reset | count | per event | `nil` for a negative value. **This is not a decode count** — see §8.4.3 |
| `lastStallAtMs`, `lastSwitchAtMs`, `mildSamples`, `upgradeSinceMs` | adapter state; no API | ms / count | per tick | `null`/0 |
| `playerHeight` | **not wired.** The layer's bounds times the screen scale; nothing in `clients/apple/Sources` reads either for this purpose today | pixels | per tick | `Infinity` — a ceiling only, so an unknown ceiling is safe by construction |
| `blockedHeights` | new. Apple's compatibility ladder owns codec/HDR fallback and is explicitly *not* this (§4, and `PlayerController.swift`: "Actual item failures remain the sole owner of the codec/HDR compatibility ladder") | set | per decode failure | empty |
| `cause` | new classifier. The inputs exist: `APIError.httpStatus` and refusal codes for `authority:`/`link:`, `PlaybackSessionStatus.producer_state`/`recent_speed` for `encode:`, the verdict answering its own `stalled` ask for `hold:` (never the routine paced hold, §3.2) | see §3.2 | per tick | `unknown` |

Tick location: its own timer at `sampleMs` (5 s), reading the values
`startStatusPolling` already samples at 2 s. **Not** folded into the 2 s poll:
A6's disposition binds that cadence as recovery evidence, and a reader that
made the poll do more work is the argument for backing it off.

Guards: the exact eligibility tuple `observeDeliveryStarvation` already
computes — `started && wantsPlayback && !finished && !isPlaybackBlocked &&
!isChangingStream && seekState.allowsStallRecovery && player.currentItem != nil`
— plus three this design adds: `selectedHeight == nil` (an explicit rung is an
instruction), not Original, and no `.stallReopen`/`.sameDeliveryRepair`/
`.resumeRepair` open in flight. `selectedHeight` and the Original flag are the
two halves `applyOpenIntent` already reads to compute `qualityAuto`, so the
gate and the wire agree by construction.

### 8.3 Android — what exists, and the one field that is not connected

| `AutoSample` field | API | Units | Cadence | When it has nothing |
|---|---|---|---|---|
| `estimateKbps` | **not connected.** `BandwidthMeter.getBitrateEstimate()` exists in Media3, but `ExoPlayer.Builder` in `Controller.kt` does not call `setBandwidthMeter` and nothing under `clients/android/app/src/main/java/tv/plurx/app/player` names either symbol. See §8.4.4 | bits/s ÷ 1000 | sliding percentile over completed transfers | — |
| `runwaySeconds` | `(player.bufferedPosition - player.currentPosition).coerceAtLeast(0)` / 1000, already computed in `Controller.kt` and `PlaybackTelemetry.kt` | ms → s | on demand | `C.TIME_UNSET` before the timeline is known. Only the runway *ahead* counts, which `Controller.kt`'s own comment already states |
| `previousRunwaySeconds` | adapter state; no API | s | per tick | `null` |
| `recentSpeed` | `Controller.sessionStatus?.recent_speed`, and **`Controller.sessionStatusAgeMs`** beside it | × realtime | 2 s (`startStatusPolling`) | the poll deliberately keeps the last sample when a request fails, so the policy must read the age, not only the value. Android is the only platform that already tracks it |
| `activeSupplyStall` | `OpenPlaybackStallTracker.Event`, with `controlMayDefer`, from the sampling loop in `Controller.kt` | bool | `openStallTracker.nextSampleDelayMs(...)` | false |
| `supplyStalls` | tracker events in a `stallWindowMs` window — adapter state; nothing counts them today | count | per event | 0 |
| `decodeStalls` | `PlaybackException` with a codec error, at the `onPlayerError` seams in `Controller.kt` | count | per error | 0 |
| `lastStallAtMs`, `lastSwitchAtMs`, `mildSamples`, `upgradeSinceMs` | adapter state; no API | ms / count | per tick | `null`/0 |
| `playerHeight` | **not wired.** `SurfaceView` height × display density; nothing reads it for this purpose today | pixels | per tick | `Infinity` |
| `blockedHeights` | new; Android's compatibility budget owns codec/HDR fallback and is not this | set | per decode failure | empty |
| `cause` | new classifier, and the *easiest* of the three: the stall-scoped verdict is already destructured at the `hold`/`retry_resource` arm of `applyStallVerdict`, reached only from `onStall`, which is exactly the `control-stall-verdict` §3.2 wants and nothing broader | see §3.2 | per tick | `unknown` |

Tick location: the existing `openStallTracker` sampling loop in
`Controller.kt` already runs on its own cadence with the ownership checks in
place; the quality tick is a second branch inside it or a sibling coroutine at
`sampleMs`. Guards: `stallGuard.isCurrent(requestVersion)`,
`stallGuard.defersPredecessorRecovery(recipeOwnership.needsMediaReplacement(currentRecipe()))`,
`playbackIntent.playbackRequested`, and `plan.requestedQuality ==
PlaybackQuality.Auto` (`ViewerPreferences.kt`: `rungHeight` is `null` for
`Auto` and `Original` alike, so the quality gate must test the enum case and
not the height, or Original would be treated as Auto).

### 8.4 The six fields that are not simply available

#### 8.4.1 Apple has no per-transfer throughput sample, so it cannot see a cliff

`recentEstimateKbps` is not a convenience. It is what makes `decideRung`'s
severe branch fire: `freshBandwidthCliff` is computed **only** from
`recentEstimateKbps`/`recentEstimateAtMs`, never from `estimateKbps`, and
without it `severe` is permanently false. An Apple adapter that supplies
`null` therefore gets a policy that can suppress, hold, mildly downgrade and
upgrade — but that can **never take the emergency downward move**, which is
the one branch a shaped cliff exists to exercise. `observedBitrate` cannot
substitute: it is AVFoundation's own average over the item's completed
transfers, deliberately smoothed, and the browser's comment explains exactly
why a smoothed estimate is the wrong input at a cliff.

AVFoundation's access-log event carries more than `observedBitrate`, and the
deltas between two successive events are the obvious candidate for a
per-transfer sample. This document does **not** assert that those deltas are a
usable measurement — nobody has taken one. That is D3's first question on
Apple, and until it is answered the Apple adapter's honest position is that
the `link:` class has no Apple evidence and the emergency branch is dead code
there. **The build plan must not ship an Apple controller that pretends
otherwise**, and the acceptance gate in §5.4 would not pass one, because a
controller that cannot react to a cliff cannot show stalled seconds down
against its own baseline.

#### 8.4.2 `activeSupplyStall` means three different things

- Web: `p.waitAt` is set and the runway is under `SUPPLY_RUNWAY_SECS`. One
  sample, no confirmation.
- Apple: `DeliveryStarvationDetector` requires **two** confirmations, a film
  clock stuck within 250 ms, at least 16 s of delivered-idle, at least 10 s of
  pending published media, and a runway at or under 10 s.
- Android: an `OpenPlaybackStallTracker.Event`, on that tracker's own
  progress-age cadence.

The Apple predicate is far stricter and far slower than the web's: it cannot
fire in under about four seconds and needs sixteen seconds of delivery
silence. `supplyStalls >= 3` — the browser's `supplyBurst` — therefore means
something like "48 s of delivery silence" on Apple and "three ticks" on the
web. Either the policy takes a normalized flag whose definition lives in the
fixture, or `supplyStalls` stops being a shared threshold. This is exactly the
class of divergence §3.1 warned about for `runwaySeconds` and did not catch
here.

#### 8.4.3 `numberOfStalls` is not a decode count

§3.1 maps Apple's `decodeStalls` to the delta of
`accessLog().events.last.numberOfStalls`. That counter is AVFoundation's
count of **playback** stalls on the item: it counts a starved buffer exactly
as it counts a decoder that could not keep up, and it is the number a delivery
failure moves first. Feeding it to the policy as `decodeStalls` would route
delivery starvation into the `decode:` class, whose action is to **block the
rung permanently for this playback** — a link blip would cost the viewer the
rung for the rest of the film, which is the opposite of what §3.2 wants. On
Apple the only honest decode evidence is an `AVPlayerItem` failure with a
decode-shaped error, which is already the compatibility ladder's input.
Android's `PlaybackException` codec error is genuine decode evidence and does
not have this problem.

#### 8.4.4 Android's bandwidth meter is not connected to anything

Media3 constructs a `DefaultBandwidthMeter` internally when the builder is not
given one, so an estimate exists inside the player; what does not exist is a
handle the client can read. The adapter passes its own meter to
`ExoPlayer.Builder.setBandwidthMeter` rather than reaching for the process
singleton, because the singleton is shared across every player in the
process — offline downloads and the Live TV player included — and a
per-playback policy reading a process-wide estimate would be measuring other
work. This is one line in `Controller.kt` plus a field, and it is the only
missing *wiring* in this whole section; everything else missing is missing by
design and needs a decision.

#### 8.4.5 `playerHeight` is wired on neither native client

Both supply `Infinity` until the adapter reads the presentation size. That is
safe rather than merely tolerable: `playerHeight` is a ceiling only, and
`decideRung` deliberately computes the ladder unfiltered so the ceiling can
never strand a downgrade from a rung above it (the fixture pins that). The
cost of `Infinity` is that a 4K rung can be chosen for a small window, not
that anything breaks. It is therefore the last thing to wire, not the first.

#### 8.4.6 Nobody hands the stall verdict to the policy

Every client already consumes a `hold`/`retry_resource` verdict, and only
where it answers its own `stalled` ask: the web inside `persistentWait`,
Android inside `applyStallVerdict`. Neither passes it to the quality policy:
`autoCauseEvidence` has no such kind and `autoControllerTick` reads no
verdict. Upward this costs nothing, because the stall that prompted the ask
already refuses upgrades through `stallFree` for `stallWindowMs`, longer than
any deferral. Downward it does: a fresh slow transfer during a held stall
takes the emergency branch. The fixture records that as its
`control-stall-verdict` disagreement, and it is the build plan's M0.1.

The routine advisory hold the server sends on every exchange while a paced
producer is ahead, and `producer_state == "held"`, are deliberately **not**
this (§3.2): a policy that treated them as evidence would never upgrade a
paced session.

### 8.5 Corrections to §3.1's table

| §3.1 said | Actually |
|---|---|
| Apple `estimateKbps` at `PlayerController.swift:2408` | The property is `observedBitrate`; the line has moved. Re-verify by name, as the document's own header instructs |
| Apple `runwaySeconds` at `:2440` | `bufferedRunwaySeconds()` has moved. Same rule |
| Apple `activeSupplyStall` = `deliveryStarvation.observe(...)` at `:5072-5080` | Right function, moved lines, and a much stricter predicate than the web's (§8.4.2) |
| Apple `decodeStalls` = `numberOfStalls` delta | Wrong class of evidence (§8.4.3) |
| Android `recentSpeed` "same field, same poll (`Controller.kt:2225-2256`)" | Right in substance, wrong lines: at this head that range is `embeddedLanguages`/`trackAt`. The poll is `startStatusPolling` and the field is held on the controller as `sessionStatus`, with `sessionStatusAgeMs` beside it |
| Android `estimateKbps` = Media3 `BandwidthMeter.getBitrateEstimate()` | The API exists; the client does not hold a meter (§8.4.4) |
| Both native `playerHeight` | Not wired on either (§8.4.5) |
| §3.1 named two fields as "not interchangeable" | Three more are: `activeSupplyStall`, `supplyStalls` and `decodeStalls` |

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | D1 | [#458](http://192.168.4.7:3000/noirr/plurx/pulls/458) | `tests/playback/auto-quality-policy.json` at schema 1 — 29 cases, 11 controller-gate rows, every §3.2 class and every §3.5 row covered, 516 lines. `node tests/playback/web-policy.test.js` green with the new cases; `make validation-lint` governs the new file (2157 → 2158 audited). Proved by reverting five things under test and re-running: `stallFree` out of `decideRung`'s upgrade gate fails "upgrade: a recent stall holds the rung even once the hold has elapsed: height, 1080 !== 720"; `causeMaxAgeMs` 15000 → 14000 fails the defaults-equality test; `!p.started` out of `autoControllerTick` fails "Not yet started: autoControllerTick no longer contains !p.started"; dropping the HDR row from the fixture fails the §3.5 coverage test; shortening a disagreement's `finding` fails the finding requirement. Four disagreements with today's browser recorded in `web_current`, not papered over: no control-verdict gate anywhere, a 60 s rather than 20 s voluntary gap, no decode class inside the policy, and §3.2's `link:` row over-collecting a publication refusal (§7.6). |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | D2 | [#458](http://192.168.4.7:3000/noirr/plurx/pulls/458) | §8, written by reading the three codebases. Every `AutoSample` field has an API, units, a cadence and a failure mode on all three platforms, or a written statement that it is unavailable and what the policy does. Six fields are not simply available (§8.4) and seven of §3.1's rows were wrong (§8.5). The load-bearing one: Apple has no per-completed-transfer throughput sample, so `decideRung`'s emergency branch is unreachable there as the policy stands (§8.4.1, §7.7). |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | D3 | — | **needs:** one shaped-network trace per platform per profile, on today's build, with all six §5.3 metrics filled. Nothing has been measured and no number in §5.3 has a value. The prompts are in §6; they need a deployed build, three browsers, two Apple devices and four Android devices. Post-merge evidence under the work board's rule 9, appended here through the evidence-only docs PR. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | D4 | [#458](http://192.168.4.7:3000/noirr/plurx/pulls/458) | [NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md](NATIVE-ADAPTIVE-QUALITY-BUILD-PLAN.md), board row A-05, `unclaimed`. Sequenced so nothing can be enabled before D3's baseline exists. |
| 2026-09-23 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | D5 | [#458](http://192.168.4.7:3000/noirr/plurx/pulls/458) | The decision executed: both `stallReopenIntent` overloads and their four test call sites deleted; `PlayerOpenIntent.stallReopen`, `StallReopenTicket`, `applyOpenIntent`'s ticket branch, `unboundStallRetry`, the floor budget's `.stallReopen` arm and the Android parameters all retained. `ReopenReason` deliberately **not** widened — §7.1 says why, and the build plan's M1 owns it. No behavioural change and no new test, because neither deleted function had a production call site; anchored in `tests/client-fixes.toml`. **The Apple target was not compiled and its suite was not run**: no Swift toolchain was reachable from the executing session. That is the one verification this milestone owes. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | review of D1–D5 | [#458](http://192.168.4.7:3000/noirr/plurx/pulls/458) | The one adversarial review (comment 4105) found seven things, all taken. **`hold:` was split** (§3.2): the server sends `hold` on every exchange while a paced producer is ahead (`resolve_action`, `playback_control.rs:2027-2087`) and `producer_state` reads `held` whenever it is suspended, so the old row and its fixture case would have stopped Auto upgrading on every paced session. Now only a verdict answering the client's own `stalled` ask is evidence, and it suppresses a downward move; the routine hold is not evidence and a new fixture case pins that it never blocks an upgrade. The "live web defect" premise is withdrawn: while a web stall verdict is in force `stallFree` already refuses upgrades. **`playback_auto_abr` is not in Developer** — it is a Playback-panel toggle with no readiness entry (`settings-panels.js:546`); §3.7 and §4 now say so, and moving it is follow-up F-1 in the build plan, not done here. **Emergency** is defined as the code defines it (§2.2, §3.4): only a fresh bandwidth cliff, never an empty runway. **Decode** has one answer (§3.2, §4): block and step down one rung once, never change delivery; a second failure is the compatibility owner's. **The fixture now pins all eight §8.1 guards**, each on its own side of the awaited poll; deleting `if(p.autoFallbackInFlight) return;` fails "An automatic fallback already claimed: autoControllerTick no longer contains p.autoFallbackInFlight before poll" (proved by revert). **The trace build is named** (§5.4, build plan M3): an unmerged branch build sideloaded onto the lab devices. The Apple watchdog test's doc comment names `.sameDeliveryRepair`. Two citation slips fixed (`PlaybackIntent.kt:64`; §7.1's pointer to the build plan). The D5 row's missing Apple verification is now taken: the target compiles and `make apple-test` on the lab Mac fails the same five iOS cases (`testDetailBadgesCarryTheSourceDynamicRangeAfterTheCodec`, three `LiveTvTests`, one `PlayerResumeTests`) that `main` fails at the same Swift, so the deletion adds none. |
