# Native adaptive quality — one policy, three players, five kinds of evidence

**Status:** ready for review · **Executes:** §3.8 from
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

**Correction to the review (one, in its favour):** §3.8 says Apple's
`stallReopenIntent(wedge:)` "has no call site". The private instance method
at
[PlayerController.swift:5683](../../clients/apple/Sources/PlayerController.swift)
indeed has none. The `nonisolated static` overload it forwards to
(`:5668`) does have four — all in `clients/apple/Tests/AppleClientTests.swift`
(`:2355, :2401, :2410, :2420`). So the plumbing is not merely uncalled, it
is **uncalled and pinned by tests**, which matters for §5's delete-versus-
revive decision: deleting it deletes four passing tests, and that is an
argument, not an obstacle.

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
| `cooldownMs` | 20 000 | minimum gap between switches |
| `upgradeHeadroom` | 1.8 | estimate margin required to go up |
| `upgradeHoldMs` | 45 000 | how long that margin must hold |
| `upgradeSpeedFloor` | 1.15 | predicted post-switch encode pace, x realtime |
| `stallWindowMs` | 60 000 | window over which stall events are counted |
| `dwellMs` | 60 000 | the horizon the restart-cost model amortises over |
| `nearEmptyRunwaySeconds` | 1.5 | emergency threshold |
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
delivery** — no rung change. `stallReopenIntent(wedge:)` (`:5683`), the
only thing that would carry a ladder step, is unreferenced (see the
correction above), and its static doc already states the rule a revival
would have to keep:

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

Where each field comes from per platform — this table is the design's real
content, because "the same policy" is worth nothing if `runwaySeconds`
means three different things:

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
| **Decoder failure** | `decode:` | a codec error, a repeated decode stall at an unchanged presentation time with a healthy buffer | add this height to `blockedHeights` and step down; a rung change is not a cure for a codec the device cannot decode, so a second failure must escalate to a delivery change, not a third rung |
| **Deliberate hold** | `hold:` | `loader-suspended`, `producer_state == "held"`, a control verdict of `hold`/`retry` | **do nothing to quality.** Repair the transport or wait out the verdict |
| **Denied authority** | `authority:` | 401/403/410, `owner_lost`, `owner_transition`, `node_removal_fenced`, `learner_route_ineligible` | **do nothing to quality.** Re-establish ownership; a rung change on a session you no longer own is a second session |
| *(unknown / stale)* | `unknown:` | nothing fresher than `causeMaxAgeMs` | **do nothing.** Not bandwidth pressure (§2.3) |

Android's existing refusal (§2.5) is exactly the last row, and is preserved
by construction: a stationary presentation with no fresh cause is
`unknown`, and `unknown` never moves the rung.

The two "do nothing to quality" rows are the ones a naive port would get
wrong, and they are why the classifier is part of the shared policy rather
than each platform's own reading of "it stalled".

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
have that seam (`PlaybackIntent.adoptQuality`, `Controller.kt:64`).

### 3.4 Hysteresis, budget, and going back up safely

- **Cooldown.** No switch within `cooldownMs` of the last one, except an
  `emergency` decision (runway below `nearEmptyRunwaySeconds`).
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
| **A control verdict in force** | off until the verdict's deadline | the hold/retry rows of §3.2 |

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
switch that already exists, and it is replicated and surfaced in
Settings → Developer with the advisory readiness list, which is the
project's standing answer to "this needs a switch".

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
  controller adds a height to `blockedHeights` and steps down; it does not
  change delivery method.
- **A6's poll cadence is not touched.** `startStatusPolling`'s 2 s is
  recovery evidence, and this design adds a *reader* of it, which makes
  backing it off worse, not better.
- **No in-code feature gate.** `playback_auto_abr` is a replicated setting
  already; the native clients read the same one. Nothing new.
- **Viewer intent is never silently overridden** (§3.5). A controller that
  moves a rung raises the existing `degraded_notice` surface exactly as the
  web one does, so the change is visible.
- **Live TV is out of scope.** Its rung story, its `BEHIND_LIVE_WINDOW`
  handling (D3) and its shared-transport plan are separate; this design is
  finite playback only.

## 5. Milestones

This is a design document; its milestones are design deliverables, one
decision, and the build plan that follows. No production client code
changes under A-04 itself.

### 5.1 D1 — the shared policy artifact and its fixtures

Deliverable: `tests/playback/auto-quality-policy.json` at schema 1 (§3.6),
plus `web-policy.test.js` extended to drive `decideRung` from it, so the
browser's *existing* behaviour is captured as the baseline before anything
native is built. Any case where today's browser disagrees with the intended
policy is recorded in the JSON as `"web_current"` beside `"expect"` — a
disagreement is a finding, not a test failure to paper over.

Acceptance: `node tests/playback/web-policy.test.js` green with the new
cases; `make web-check` green; every row of §3.2 and §3.5 has at least one
case; the file is under 600 lines or it is too clever.

### 5.2 D2 — the adapter specification, per platform

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

### 5.3 D3 — one shaped-network trace per platform, on today's build

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

### 5.4 D4 — the build plan

Deliverable: a separate implementation document (a new board row) that
takes D1's fixtures, D2's specification and D3's baseline and sequences the
work — shared policy first, one platform second, the second platform only
after the first has a shaped trace that improves on D3's baseline.

The enabling gate, written into that plan and repeated here because it is
§3.8's own condition: **a platform's controller ships disabled until its
shaped-network trace shows stalled seconds down and unexpected SDR
transitions at zero against D3's baseline for that platform.** A green
`auto-quality-policy.json` run is not that evidence.

### 5.5 D5 — the dormant-plumbing decision

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

### 7.1 Delete or revive the stall-ticket plumbing — Paul's call

The dormant surface: Apple's `stallReopenIntent(wedge:)`
(`PlayerController.swift:5683`, no production call site) and its static
overload (`:5668`, four test call sites); Android's `previousSessionId` /
`reopenReason` parameters (`SubtitlePolicy.kt:363-365`, every caller at the
default); and `ReopenReason::Stall` on the wire (`transcode.rs:11023`),
which the **web client does use** and which the server acts on in two
places (§2.7).

**The case for deleting it:**

- It has never run in production on either native client. Code that has
  never executed is not "nearly working"; it is unmeasured.
- Its semantics are already subtly wrong for the design in §3.2. A ticket
  says "this rung is too heavy for the link" and the server steps one rung
  down (`auto_height_from_prior`). That is the `link:` row and only the
  `link:` row. Under `encode:` the right move is also one rung down but for
  a different reason, under `decode:` the rung must be *blocked* rather
  than merely stepped, and under `hold:`/`authority:` no move is right at
  all. A single untyped ticket cannot carry five causes, so reviving it
  means widening `ReopenReason` anyway — at which point the old field is a
  name, not an implementation.
- Apple's own doc comment already describes a special case it gets wrong
  (`wedge`), which is a sign the abstraction is at the wrong level.
- Deleting it removes a false affordance: the next reader sees "the native
  clients have no ladder plumbing" rather than "they have some, which must
  nearly work".
- Cost is bounded and visible: one private method, one static, four tests,
  two default parameters.

**The case for reviving it:**

- The server half is built, typed, tested and already exercised by the web
  client. `ReopenReason` was deliberately made an enum so an unknown value
  is refused rather than coerced (`transcode.rs:11020-11022`) — it was
  designed to grow.
- `auto_height_from_prior` and the network prior (§2.7) are a *server-side*
  memory of what starved, shared across sessions and clients. A native
  client that reports nothing contributes nothing to it, and a native
  client that reopens without a ticket actively discards evidence the
  server would have used.
- The prepared-switch suppression at `hls.rs:7378-7391` depends on the
  ticket to tell a client-driven failure recovery from a viewer's ask. If
  the native clients start changing rungs without it, they will pollute the
  prepared-switch accounting that
  [CLIENT-PREPARED-SWITCH-CONTRACT.md](../playback-control/CLIENT-PREPARED-SWITCH-CONTRACT.md)
  measures.
- Reviving is strictly less work than re-inventing: the field names, the
  wire, the server behaviour and the Apple tests all exist.

**The recommendation, for Paul to accept or reject:** revive the **wire**
(`previous_session_id` + `reopen_reason`) and delete the **Apple helper**.
The wire is the part with a server behind it and a real consumer; the
helper is one platform's guess at when to use it, made before the five-class
classifier existed, and §3.2 replaces it. Widen `ReopenReason` from `Stall`
to the five causes in the same change, so the field stops meaning "the link
was slow" and starts meaning what the client actually observed. That third
point matters most: the argument against reviving it is entirely about its
single untyped cause, and typing the cause answers it.

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

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
