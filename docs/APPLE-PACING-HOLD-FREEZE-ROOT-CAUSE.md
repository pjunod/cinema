# Apple pacing-hold freeze — root cause and repair contract

**Status:** built as PR #803 — see §6.5 for what shipped · **Scope:** Apple and web HLS stall
recovery · **Written:** 2026-09-02 · **Baseline:** `main` at `26cb567b`

Companion to [PLAYBACK.md](PLAYBACK.md) (how growing HLS and its production
window work),
[M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md](M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md)
(why clients began obeying server actions), and
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (the rollout state).
The 2026-09-02 review accepted the causal diagnosis and the serving invariant,
required server and client defenses, replaced the hold-reason matrix with one
state predicate, and found one repair constraint: a delivery-wedge reopen must
not spend the stall ticket that lowers automatic quality.

Read §1 for the verdict and §2 for the explicit-demand mechanism that makes
the freeze stable. Section 3 records the evidence and its limits. Section 6
is the implementation contract; §7 is the regression and device-acceptance
contract. If implementation needs to weaken the positive serving evidence in
§6.1, stop and return the change for review.

## 1. Verdict — the stall manufactures the hold that vetoes its recovery

The Apple client enters a stable deadlock after AVPlayer stops fetching:

1. An accepted playback-control exchange puts the rolling session in
   `RollingLeaseMode::Explicit`.
2. AVPlayer stops requesting published media. Its position stops and its
   local forward runway drains to zero.
3. Explicit flow control recomputes the production target as client runway
   plus 30 seconds of wall-time reserve. At playback rate 1, zero runway
   collapses the target to 30 seconds and the held release line to 15 seconds.
4. Published media remains more than 30 seconds beyond the frozen buffer
   anchor, so the stalled request itself guarantees a `time` hold.
5. The Apple delivery-stall detector correctly reports `render: stalled`,
   `decoder_state: starved`, and `delivery_wedge`.
6. [`resolve_action`](../crates/plurxd/src/playback_control.rs) promotes the
   producer hold to a `hold` action without reading those stall facts.
7. [`applyStallVerdict`](../clients/apple/Sources/PlayerController.swift)
   shows **“The server is pacing this stream.”**, records `server_hold`,
   returns `true`, and skips the bounded item reopen.
8. Position cannot advance while the item remains wedged. The explicit hold
   therefore cannot cross its release line, and the same answer repeats.

Backing out and reopening the title works because a new AVPlayer item requests
the media that remained available throughout the freeze.

**Root cause:** commit `a25182e7` made an unconditional producer-flow
`hold` suppress client recovery from a confirmed empty-buffer delivery
stall. The action conflates two facts:

- **production state:** the producer has deliberately paused; and
- **serving authority:** a stalled client should or should not reconnect to
  fetch bytes already published.

The first fact does not decide the second. In explicit mode the distinction is
even tighter: the stalled position and empty runway lower the production
target, manufacture the hold, and then receive that hold as the reason not to
recover.

**Repair constraint:** the existing Apple reopen is bounded, but it is not
rung-neutral. Every live `stallReopenIntent` carries the predecessor session
and `reopen_reason: "stall"`; server normalization rewrites an automatic
copy/remux or transcode request one quality rung lower. A 2160p delivery wedge
can therefore return as a 1080p QuickSync transcode. The repair must omit that
stall ticket for `.delivery` so it does not trade the visible freeze for a
silent quality drop.

## 2. Mechanism — explicit demand turns zero runway into a 30-second ceiling

[`explicit_production_target_seconds`](../crates/plurxd/src/transcode.rs)
implements:

```text
target_seconds =
  clamp(client_runway_s + ceil(playback_rate × 30 s), 1, configured_max)

ahead_seconds =
  media_origin + published_end - buffer_anchor(position or seek target)

enter time hold when ahead_seconds > target_seconds
release time hold when ahead_seconds ≤ max(target - 30, target / 2, 1)
```

For a player that still intends to play, the mapper reports rate 1. Once a
wedge reaches zero runway, the target is 30 seconds and the release line is
15 seconds. The film position is also the buffer anchor. No progress means no
way to reduce the explicit ahead value, even though the media remains
fetchable.

The 180-second value in retained telemetry describes a different mode:

| Lease mode | Ahead anchor | Time entry and release | What proves it here |
|---|---|---|---|
| Legacy | Published end minus fetched frontier | Configured 180 s · release at 150 s | Retained 182–196 s rows |
| Explicit | Published end minus client buffer anchor | Runway + 30 s · stalled zero-runway target 30 s · release at 15 s | Current control path and source formula |

The legacy rows prove that the AVPlayer fetch wedge, empty local runway, and
fast recovery after an item reopen are repeatable. They do not prove that the
current explicit client held 150 seconds of reserve. Its guaranteed reserve
is approximately 30 seconds—still three times the detector's 10-second
published-but-unfetched threshold, but materially less than the original
document claimed.

### The recovery has bounds, but delivery wedges must keep their rung

[`PlayerController.swift`](../clients/apple/Sources/PlayerController.swift)
already has two loop controls:

- `SameDeliveryStallRecoveryState` permits one reconnect until five seconds
  of film-clock progress rearms it; and
- `RecoveryReopenBudget` permits at most three automatic reopens in a
  rolling 60-second window.

Those bounds remain the authority after a hold falls through. No new retry
loop is required.

The reopen intent is a separate decision. Today
[`stallReopenIntent`](../clients/apple/Sources/PlayerController.swift) binds
every live rolling-session stall to its predecessor. In
[`normalize_claimed_request`](../crates/plurxd/src/transcode.rs), an
automatic predecessor—including copy/remux—is rewritten to
`one_rung_below(previous_height)`. That response is appropriate when
buffering or decode evidence says the recipe may be too ambitious. It is not
appropriate for `.delivery`: the server already published the media and
AVPlayer declined to fetch it.

Expected behavior is therefore:

| State | Expected result | Current result |
|---|---|---|
| Healthy playback with a held producer | Continue; no recovery owner fires | Correct |
| Stalled/starved with a proven fetchable gap | Return `none`; let bounded client recovery run | `hold` vetoes recovery |
| Apple `.delivery` wedge | One same-recipe, same-rung reopen | No reopen; naïve fallthrough would lower the rung |
| Permanent producer decision | Stop with the server's bounded reason | Correct |
| Transient producer decision | Retry on the server's bounded cadence | Correct |

### The initiating wedge and the regression remain separate defects

AVPlayer stopping media requests is the initiating trigger. It predates M5
and is why `DeliveryStarvationDetector` exists. The detector requires a
stuck film clock · runway no greater than 10 seconds · at least 16 seconds
without completed delivery · at least 10 seconds of published but unfetched
media · two consecutive samples.

The regression occurs after accurate detection. Before `a25182e7`, the
detector reached the bounded reopen. After it, a producer hold intercepts the
recovery. This repair makes the wedge survivable at the viewer's selected
rung; it does not claim to explain why AVPlayer stops fetching.

### Web shares the veto; Android does not share this control path

Web's `deferred:hold` branch in
[`index.html`](../crates/plurxd/src/web/index.html) also suppresses its
reopen. It leaves a visible **Try again** button, so the viewer can break the
cycle manually, but the automatic defect is the same.

Android's `sampleStall` reports after a stall ends. By the time Android
applies a hold verdict, its playhead is moving, so it does not enter this
deadlock. Its open-ended-freeze detector gap is a separate defect and is not
part of this repair.

## 3. Evidence — the action path is closed; incidence is not measured

### The exact notice identifies the Apple action

Only `hold { reason: "time" }` and `hold { reason: "bytes" }` map to:

> The server is pacing this stream.

The mapping is reached from `applyStallVerdict` after the stall owner asks
playback control for an action. The branch returns before
`SameDeliveryStallRecoveryState.next` and before `reopen`. Receiving a
full-vocabulary verdict also proves the accepted exchange that selects
explicit lease mode.

### Retained telemetry proves the wedge and old recovery, in legacy mode

A read-only snapshot of nynuc's node-local `telemetry.db` was inspected on
2026-09-02. It retained 297 Apple `stall` rows through 2026-08-31.

| Evidence class | Rows | Interpretation |
|---|---:|---|
| Client runway at or below 0.1 seconds | 174 | AVPlayer had essentially no playable reserve |
| Server producer held | 212 | Production was deliberately paused |
| Runway ≤0.1 s · held · server ahead ≥150 s | 127 | Legacy fetch wedge with a large published reserve |
| Same signature with producer speed above 1× | 122 | The producer was not failing to keep up |

The matching rolling-HLS rows split as follows:

| Delivery | Matching rows | Mean recorded stall | Mean recent producer speed |
|---|---:|---:|---:|
| Copy/remux | 53 | 32.4 s | 10.05× |
| Intel QuickSync transcode | 70 | 60.9 s | 13.57× |

One 2026-08-28 `Love, Simon` row recorded zero runway, 182 seconds ahead,
a `time` hold, and producer speed of 9.47× realtime. The pre-regression
client reopened after a 64.1-second stall and presented a first frame in
3.8 seconds. The latest retained Apple stall, `Heavy Is the Head` on
2026-08-31, recorded zero runway and 186 seconds ahead; its reopen presented
a first frame in 4.2 seconds.

**How to read these counts:** they establish a repeatable AVPlayer fetch
wedge, ample published supply, and successful old item recovery across copy
and QSV. They are legacy-mode rows, not an explicit-mode reserve measurement,
and they mix organic playback with acceptance and stress runs. The exact
post-rollout notice supplies the current hold-veto edge. No retained
post-change `server_hold` reproduction completes the chain in one row.

### The resolver ignores positive stall and serving evidence

`resolve_action` reads no request field beyond `supported_actions` before
turning any recognized `delivery.hold_reason` into `ControlAction::Hold`.
The inputs already contain:

- `request.render_state`;
- `request.observation.decoder_state`;
- `delivery.client_runway_ms`; and
- `delivery.produced_through_ms` and `fetched_through_ms`.

The server therefore answers a healthy player with runway and a
stalled/starved player with fetchable media identically.

### VOD's frontier is materialized, so the serving predicate spans all reasons

[`vodserve::status_publication`](../crates/plurxd/src/vodserve.rs) computes
`published_end_ms` under the rendition manifest lock from the contiguous
run of segments whose state is `Materialized`. A far-seek segment beyond a
hole does not extend it. If working-set eviction creates a hole, the next
status snapshot retreats the frontier.

Therefore a positive
`published_end_ms - fetched_end_ms` gap proves currently materialized,
contiguous VOD media at the status snapshot. `working_set` and `no_room`
need no reason-specific exception: if the gap is proved, recovery may fetch
it; if the gap is absent or unknown, the server must not invent that
authority.

### Existing documentation already states the semantic rule

[PLAYBACK.md](PLAYBACK.md) says:

> A hold is not proof that the producer starved the client. Ahead media is
> already published in the served playlist. If AVPlayer stops fetching while
> that reserve remains available, changing the release point produces more
> media the client is still declining to request.

The producer hold is valid. Treating it as a universal serving verdict is not.

## 4. Causal chain — explicit flow waits on the progress Apple suppresses

```text
 accepted playback-control exchange
                  │
                  ▼
       RollingLeaseMode::Explicit
                  │
                  ▼
 AVPlayer fetch wedge · position stops · runway drains to 0
                  │
                  ▼
 target = 0 s runway + 30 s reserve · release = 15 s
                  │
                  ▼
 published end remains >30 s beyond frozen buffer anchor
                  │
                  ▼
       evaluate_flow returns hold(time)
                  │
                  ▼
 stalled/starved delivery ask ──▶ resolve_action returns hold(time)
                  │                              │
                  │                              ▼
                  │                 Apple shows pacing notice
                  │                 and returns before reopen
                  │                              │
                  └──── position still frozen ◀─┘
```

The loop has no automatic exit. In explicit mode, film-clock progress moves
the buffer anchor and can clear the hold. In legacy mode, media fetches move
the fetched frontier and can clear it. An item reopen enables both. The
comments in Apple and web that a hold is “never lifted by anything this
client does” are false in both modes and must be deleted with the repair.

## 5. Alternative explanations — none account for the veto

| Hypothesis | Evidence | Verdict |
|---|---|---|
| Encoder cannot maintain realtime | 122 matching legacy rows record speed above 1×; copy/QSV means are 10.05×/13.57× | Rejected for this symptom |
| Quality rung is the trigger | Copy/remux wedges exist, producer speed is above 1×, and manual same-rung reopen recovers | Not supported; do not lower the rung as recovery |
| Head start is too small | Explicit mode deliberately keeps about 30 s at zero runway, above the 10 s detector gap | Raising it delays the same loop |
| Ahead suspension is broken | Both lease modes enter their configured flow bounds and keep published media servable | Rejected |
| Storage cannot serve the stream | Matching sessions recover first frame in about 4 s after item reopen | Not supported by this evidence |
| The notice is cosmetic | Its branch returns before the bounded reopen | Rejected; it names the veto |

Network or tvOS behavior may initiate the wedge. That does not change the
diagnosis: bounded recovery exists because Plurx cannot control every trigger.

## 6. Repair contract — serving evidence overrides every advisory hold

### 6.1 R1 — use one server predicate, whatever paused production

Keep producer decisions first. A `terminal` or `retry_resource` decision
continues to outrank any advisory hold. When there is no producer decision,
apply this single serving predicate:

```text
request.render_state == stalled
  ∧ request.observation.decoder_state == starved
  ∧ delivery.client_runway_ms ≤ 10_000
  ∧ delivery.produced_through_ms is known
  ∧ delivery.produced_through_ms - delivery.fetched_through_ms ≥ 10_000
──────────────────────────────────────────────────────────────────────
⇒ resolve_action returns none, whatever the recognized hold reason
```

The reason describes why production paused. The predicate proves the stalled
client can fetch media already served. Applying the rule to `time`,
`bytes`, `global`, `ahead`, `demand`, `working_set`, and
`no_room` avoids repeating the category error in seven special cases.

`DeliveryView::from_status` maps an absent `buffered_through_ms` to zero
runway. Runway alone must therefore never authorize fallthrough. The explicit
`stalled` and `starved` reports plus the known 10-second frontier gap are
the positive evidence. An unknown produced frontier fails the predicate.

Continue reporting `delivery.hold_reason`; returning `none` suppresses an
instruction, not the producer state. Add a dedicated
`ActionMetrics.recovery_withheld` slot and expose it by platform and hold
reason. The existing `suppressed` slot means vocabulary suppression on a
client that cannot accept the action. Reusing it would hide the semantic
difference, while omitting a new slot would make holds appear to vanish from
rollout dashboards.

### 6.2 R2 — Apple and web must not let delivery holds consume recovery

Server R1 fixes every current client at once. Client defense remains required
for older servers, relayed answers, and a future resolver regression.

For Apple, a `hold` received by a confirmed `.delivery` recovery must be a
fallthrough result at the actual call site:

```text
applyStallVerdict(.hold, event: .delivery)
  └─▶ retrySameDeliveryAfterStall continues
       └─▶ SameDeliveryStallRecoveryState.next
            └─▶ bounded reopen or bounded stop
```

A detached pure disposition helper is not sufficient evidence:
`applyStallVerdict` could still return `true`. Pin the call-site behavior,
or encode the verdict/event combination so a delivery hold cannot be
expressed as a terminal disposition.

Web's `deferred:hold` branch must likewise fall through for its confirmed
supply-starvation recovery instead of waiting for the viewer to press
**Try again**. Preserve the existing web recovery bounds. Delete the false
“never lifted” comment in both clients.

Android receives no change in this repair because it does not report the
open-ended stall on this path.

### 6.3 R3 — a delivery wedge reopens with no stall ticket

Key `stallReopenIntent` on `PlaybackStallKind` as well as `isVOD`:

```text
kind == delivery  ⇒ normal intent; no previous_session_id; same recipe/rung
kind == buffering ⇒ existing bound stall ticket and ladder behavior
kind == silent    ⇒ existing bound stall ticket and ladder behavior
isVOD             ⇒ existing normal intent
```

The change preserves the one-attempt state and the three-per-60-second storm
budget. It changes only server normalization: the delivery reopen is no
longer claimed as evidence that the predecessor rung failed.

### 6.4 Review rulings — the four requested decisions are settled

| Decision | Ruling incorporated |
|---|---|
| Root-cause statement | Accepted, sharpened with the explicit-demand feedback loop |
| Serving invariant | Accepted: fetchable production state cannot veto empty-buffer recovery |
| Repair layer | Server correction plus Apple defense, with the same defense on web |
| Hold-reason handling | One positive serving predicate for every recognized reason |

### 6.5 What shipped, and the two places it differs from §6.1–6.3

Built as [PR #803](https://github.com/pjunod/plurx/pull/803), six milestones, rebased onto `main` at `7443b05b`. R1 is
`recovery_outranks_hold` beside `resolve_action`, called between the
vocabulary check and the hold, with the `recovery_withheld` metric slot and
`plurx_playback_control_recovery_withheld_total{reason,platform}`; R2 is
`holdMayDecideStall` at the Apple `applyStallVerdict` call site and the
`kind === "supply"` fallthrough in web's `persistentWait`; R3 is
`stallReopenIntent(…, wedge:)` plus a server backstop.

Two amendments came out of the second review round, and both widen the repair
rather than narrowing it:

- **R3 keys on wedge evidence, not on `PlaybackStallKind`.** The two detectors
  race for the same freeze: a buffer that drains for sixteen seconds trips the
  delivery watchdog, while a client with under twelve seconds buffered when
  fetching stops trips the position ladder first and reports `.buffering`.
  Keying the ticket on `.delivery` would leave the identical wedge, with the
  identical server evidence, stepped one rung down. The client therefore reads
  the signature — no completed delivery for 16 s while ≥ 10 s of published
  media is unfetched — from its last status poll, and a `.delivery` event
  short-circuits to it. Because Android's stall reopens carry the same ticket,
  the same signature is also enforced on the server in
  `normalize_claimed_request`, which is where a slow link keeps the one-rung
  descent it deserves: a link that is merely slow is still completing
  deliveries.
- **VOD's frontier is measured from the fetched side.** `published_end_ms` is
  the contiguous materialized run from segment 0, so after a far seek past a
  hole it sits *behind* the playhead and `produced − fetched` is negative
  while the segments ahead of the client are materialized. `VodSessionInfo`
  gained `ready_ahead_end_ms` — the run measured from the client's own last
  served segment — and the control plane reads that. `published_end_ms` keeps
  its meaning for the activity page, which asks a different question.

## 7. Verification — test the loop, the call site, and the preserved rung

### 7.1 Server tests must join flow evaluation to action resolution

Add focused tests beside
[`evaluate_flow`](../crates/plurxd/src/transcode.rs) and
[`resolve_action`](../crates/plurxd/src/playback_control.rs):

1. An explicit-mode, rate-1, stalled client with zero runway and more than
   30 seconds published beyond its frozen anchor produces `hold(time)`.
   Passing that delivery and request through `resolve_action` returns
   `none`. This proves the actual feedback loop, not only half of it.
2. A rendering client with healthy runway still receives each recognized
   hold it declares.
3. Loop over all seven recognized hold reasons: the same stalled · starved ·
   runway ≤10 s · known gap ≥10 s predicate returns `none`.
4. Permanent and transient producer decisions still return `terminal` and
   `retry_resource` before the hold predicate runs.
5. An unknown produced frontier does not authorize fallthrough. An absent
   buffered frontier, which maps to zero runway, does authorize fallthrough
   only when stalled · starved · known gap evidence is also present.
6. VOD `working_set` and `no_room` with a positive materialized gap return
   `none`; without the gap they retain `hold`.
7. A state-withheld hold increments `recovery_withheld` by platform and
   reason, never the vocabulary `suppressed` slot.

Before changing Rust, establish the repository-pinned Rust 1.97.1 compile loop
in [AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md). Retain:

```bash
cargo test -p plurxd --bin plurxd playback_control
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

### 7.2 Apple tests must pin the owner and the create body

Add tests in
[`AppleClientTests.swift`](../clients/apple/Tests/AppleClientTests.swift)
that prove:

- `applyStallVerdict(.hold, event: .delivery)` reaches
  `retrySameDeliveryAfterStall` and its bounded decision;
- every hold reason is non-terminal for a confirmed delivery wedge;
- the one-attempt and three-per-60-second bounds still stop immediate loops;
- `terminal` still stops and `retry_resource` still defers;
- a delivery-wedge create omits `previous_session_id` and
  `reopen_reason`;
- a 2160p automatic copy session reopens as 2160p copy, with the same recipe
  and selected rung; and
- buffering and silent stalls retain their existing ticket and ladder
  behavior.

A pure helper test alone does not satisfy this contract. The assertions must
cover the call site and the emitted create body.

```bash
make apple-test
git diff --check
```

### 7.3 Web tests must prove hold fallthrough remains bounded

Add a focused static or JavaScript policy test for
[`persistentWait`](../crates/plurxd/src/web/index.html):

- a confirmed supply-starvation hold reaches the existing recovery action;
- a healthy player never enters that owner;
- terminal and retry-resource behavior is unchanged; and
- repeated recovery remains bounded and keeps the manual controls available.

Run the repository web contract named by the implementation surface, then
retain `git diff --check`.

### 7.4 Physical Apple TV acceptance is opportunistic, not the merge gate

There is no known deterministic way to induce the AVPlayer fetch wedge. The
2026-08-18 diagnosis observed it across 43 sessions without identifying a
trigger. A device run can prove the healthy path immediately, but wedge
acceptance is opportunistic; the coupled server unit contract in §7.1 is the
merge gate.

When a physical wedge occurs, retain these observable facts:

1. A healthy producer hold does not reopen advancing playback.
2. A detected delivery wedge performs at most one automatic reopen and does
   not remain behind repeated `server_hold` outcomes.
3. A 2160p automatic copy session returns as 2160p copy—not 1080p QSV—and
   the selected recipe is unchanged.
4. First-frame latency is recorded, and five seconds of film-clock progress
   rearms the ordinary recovery state.
5. An immediate repeat reaches the existing stop bound rather than looping.
6. Server telemetry retains runway · published/fetched frontiers · lease
   mode · production target · hold reason · recovery outcome · returned rung
   and recipe.

One twenty-minute play that never wedges proves only the happy path. Record it
as such; do not delay the source-level repair waiting for a nondeterministic
trigger.

## 8. Non-goals — make the wedge survivable without widening the project

- **Do not claim to diagnose the initiating AVPlayer wedge.** No repeatable
  trigger is known; this repair restores bounded recovery.
- **Do not fold Android's stall-ends-only detector into this patch.** It is a
  separate open-ended-freeze defect with a different owner.
- **Do not remove ahead-window suspension.** It bounds scratch and behaves
  correctly for healthy viewers.
- **Do not raise the 180-second legacy ceiling or 30-second explicit reserve
  as the fix.** More unpublished demand does not make a wedged item fetch.
- **Do not weaken the positive serving predicate.** Unknown runway maps to
  zero, so stalled · starved · known gap must remain conjunctive evidence.
- **Do not add an unbounded client retry loop.** Existing attempt and storm
  budgets remain authoritative.
- **Do not lower quality for a delivery wedge.** The detector proves a fetch
  failure after publication, not a recipe that could not keep up.
- **Do not special-case hold reasons once serving is proved.** Production
  cause does not change whether already materialized media can be fetched.

## 9. Evidence gaps — what implementation and rollout must not infer

- The 2026-09-02 live process had restarted, so its in-memory control counters
  were zero and no post-restart session could be joined.
- Retained qualifying rows record legacy-mode `reopen`, not a preserved
  post-change `server_hold` reproduction. The current viewer's exact notice
  supplies that final edge of the chain.
- Some of the 70 QSV telemetry rows may be automatic copy/remux predecessors
  already stepped down by an earlier stall reopen. The snapshot cannot exclude
  that interpretation.
- Aggregate counts mix organic playback with acceptance and stress runs. They
  prove repeatability and cross-pipeline shape, not prevalence.
- No known action induces the AVPlayer wedge on demand. Physical-device
  acceptance of the repaired transition is therefore opportunistic.
- Web shares the automatic hold veto and is in the repair. Android does not
  share this path and remains uncertified for its separate open-ended freeze.

The code path is closed enough to implement: explicit flow manufactures the
hold, resolution ignores positive serving evidence, Apple and web suppress
their recovery, and Apple's naïve fallthrough would lower the quality rung.
What remains to measure after the repair is the end-to-end transition on a
physical Apple TV when the nondeterministic wedge next appears.
