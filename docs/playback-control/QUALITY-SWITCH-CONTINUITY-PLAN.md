# Quality switch continuity — why every rung change is still a reopen, and the plan to make it a handoff

**Status:** assessment complete, plan awaiting Paul's rulings in §9 ·
**Anchors:** `main` at `c9e4edf4` (2026-09-16, deployed to nynuc/m6 the same
evening) · **Written:** 2026-09-16 · **Companions:**
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) (what M6 built),
[M6-CLIENT-REPLACEMENT-CONTRACT.md](M6-CLIENT-REPLACEMENT-CONTRACT.md) (the
wire), [M6-SERVER-PRIME-HANDOFF.md](M6-SERVER-PRIME-HANDOFF.md) (the prime),
[../streaming/ADAPTIVE-QUALITY.md](../streaming/ADAPTIVE-QUALITY.md) (the
ladder and Auto).

Read §1 and §2 first: they say what the tree does today and why the viewer
still sees a gap, with the line numbers. §3–§6 are the plan. §9 lists the
decisions that are Paul's; nothing in §5 should be built before they are
taken. Line numbers are from `c9e4edf4` — re-verify against the file before
editing, this tree moves daily.

The one-sentence finding: **the prepared handoff is built and deployed on
every side, and no viewer action on any client can reach it.** The web and
Android quality menus reopen before a `Prepare` can exist; Apple asks for
one on the exchange that cannot yet carry it, gets `none`, and reopens 1.5 s
later — and its own reopen then pre-empts the successor the server had just
started priming for it. Every rung change the fleet has ever served has been
the fallback path, whose interruption is measured at 271–2,246 ms on the web
and 353–766 ms on Android.

## 1. How a rung change works today

### 1.1 The ladder is per-session, not per-playlist

Each rung is its own server session. `advertised_ladder` (`transcode.rs:25421`)
returns `Rung { height, total_kbps, peak_kbps }` rows from
`LADDER_HEIGHTS = [360, 480, 720, 1080]` (`transcode.rs:25348`) capped by
source height and the encoder ceiling (`capability_height_ceiling_for_request`,
`transcode.rs:18908`; software ceiling `AUTO_SOFTWARE_HEIGHT = 720`,
`transcode.rs:243`). The rows reach the client only as
`StartResponse.ladder` JSON (`http/hls.rs:140`, `:2560`). The HLS master
playlist emits exactly one `#EXT-X-STREAM-INF` (`http/hls.rs:11575-11583`,
`:11640`) — the source's bitrate and resolution, one `index.m3u8`. No player
can switch variants natively; a rung change is a new `POST
/api/v1/files/{id}/hls` with `height` and `start` (`http/hls.rs:1687`,
`:746`).

This is the design [ADAPTIVE-QUALITY.md](../streaming/ADAPTIVE-QUALITY.md)
chose on purpose — one JIT encode, the adaptation brain in the client — and
its own Phase 3 says: "live with the switch blip for a week, and build this
only if it actually grates." It grates.

### 1.2 The reopen is what every client does

| Client | Directed change (menu) | Automatic change | Anchor |
|---|---|---|---|
| Web | `setQuality` → `play()` at position | `switchAutoRung` / `rescueAutoSupply` → `executePlaybackMediaChange` | `index.html:13752-13768`, `:11864-11879`, `:10419-10455` |
| Apple | `selectQuality` → `offerPreparedQualityChange` (1.5 s ask) → `reopen(at:)` | `retrySameDeliveryAfterStall` → `reopen` | `PlayerController.swift:3214-3248`, `:3298-3331`, `:4573` |
| Android | `Controller.prepareReplacement` → publish intent → `planReplacement.route(force = true)` → Compose rebuilds the `Controller` | `onStall` → `restartAt` / `openSession` | `Controller.kt:1187-1207`, `PlayerScreen.kt:555-561`, `:1630`, `:1743-1759` |

The reopen tears down the decoder, creates a session at the current film
position, waits for the server's first media, and re-attaches. What the
viewer sees is the "Preparing the stream…" surface over a black element
(web `index.html:10436-10437`), or the Android preparing surface
(`Controller.kt:1380-1382`), until the new session's first frame.
Measured fallback interruption: web/Safari 271–2,246 ms, mean 1,121; Android
/ Google TV 353–766 ms, mean 471
([PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) line 696,
`Controller.kt:3358-3360`). Apple's has never been measured — the instrument
exists (`PlayerController.swift:8760`) and is fed only from the
`switchedWithoutAFrame` branch.

### 1.3 The prepared handoff is fully built — server and all three clients

M6 is not a stub any more. On `main`:

- The server stages **and primes** a real speculative successor:
  `stage_and_prime_prepared_successor` (`http/hls.rs:8312`) → durable row →
  `vod_resurrect_before(…, speculative = true)` attaches a real VOD worker
  (`:8641-8649`, `transcode.rs:17774`) → only then is the actor's slot
  published and `Prepare` announced (`:8686-8696`). Prime budget 45 s
  (`PREPARATION_PRIME_BUDGET`, `:8254`). The successor's playlist is servable
  before commit through the preparation ledger
  (`authorize_prepared_media_before`, `media_sessions.rs:1684-1799`) — the
  2026-09-08 "staged not primed" defect is closed (PR #203, `e3cbb42a`).
- The decision has one refusal reason left, `client_cannot_prepare`
  (`playback_control.rs:1310-1327`). There is **no throughput floor** — the
  `_conditions` parameter is unused (`:1483`) — and no axis allow-list;
  `PREPARED_AXIS` exists only in docs. All five axes reach `Prepare`.
- All three clients declare `prepare_replacement` unconditionally and
  advertise `dual_player_preparation` **on by default** (web
  `index.html:7883-7886`; Apple `SettingsStore.swift:170-172`; Android
  `ViewerPreferences.kt:140`, `SettingsStore.kt:88`). Each has a complete
  second pipeline, alignment, commit-on-first-frame and rollback.
- The `canOfferPreparation` retirement rule the status page still describes
  is gone from the Swift — `canPrepare: true` is hard-coded
  (`PreparedReplacement.swift:423`) and the `.declined` arm is dead. Correct,
  since priming landed; the status page is stale there.

The fleet is on this code: nynuc and m6 report `v0.3.0-2633-gc9e4edf4`,
nuc4 `-2609`. Server setting `prepared_quality_handoff` defaults on
(`http/hls.rs:8082-8095`).

## 2. Why the viewer still sees a gap — three independent defects

Any one of these alone would keep the count of prepared handoffs in
production at zero. All three are present. nynuc's counters 28 minutes after
today's deploy read `preparation_observations_total{seam="in_session"} 2`,
`preparation_staged_total{outcome="refused"} 1`, `staged 0`, `actions
prepare 0` — two selection changes observed, one staging attempted and lost,
nothing ever offered.

### 2.1 `Prepare` cannot arrive on the exchange that asked for it (server × Apple)

The exchange that carries a changed selection spawns
`process_preparation_candidate` **after** its response is built — "Spawned,
never awaited … the response is already built" (`http/hls.rs:7210-7234`). The
slot is `Empty` when `staged_successor_action` (`:6358`) computes the
response's action, so that exchange always answers `none`. The earliest a
`Prepare` can be announced is the *next* exchange, and the reporter's cadence
is `NEXT_EXCHANGE_MS = 5_000` (`playback_control.rs:26`) unless the client
notifies sooner — after a prime that itself takes store reads plus encoder
admission.

Apple's `offerPreparedQualityChange` publishes the new selection and waits
for *that exchange's* answer: `askForAction(bound: 1.5, cap: 3)`
(`PlayerController.swift:3316-3318`, `:4667-4668`) settles on
`answers.answer(atOrAfter: floor)` (`PlaybackControlSession.swift:419-424`),
and the recorded `action` is the non-optional `ControlAction` of type
`"none"` (`PlaybackControlReporter.swift:637`, `:1077`). `settled()` returns
it, `PreparedReplacementAction(answer)` is nil, the function returns `false`,
and `selectQuality` reopens (`:3246-3247`). The 1.5 s is spent for nothing on
every directed change; the wait can only ever be won by a `Prepare` that was
already staged before the tap.

### 2.2 Web and Android reopen unconditionally (client)

Web `setQuality` calls `play()` synchronously (`index.html:13768`). `play()`
nulls `PLAYER.mediaAttachment` (`:9978`); the reporter's `capture` and
`onExchange` both require `p.mediaAttachment === attachment`
(`:8937`, `:8945-8947`), so the exchange queued by `beginPlaybackControlSeek`
never leaves and a `Prepare` could not be handled if it did. The prepared
block (`:7833-8400`) is live code that only a server-spontaneous staging on an
ordinary exchange can reach — and nothing in the server stages spontaneously
(§1.3: `SelectionChange` and `PlannedRelocation` only). The same holds for
`switchAutoRung` (`:11864`) and `rescueAutoSupply` (`:11880`).

Android `Controller.prepareReplacement` (`Controller.kt:1187-1207`) awaits
`publishIntent` — so the server *does* receive the change — and then calls
`planReplacement.route(force = true)` regardless, which disposes the
`Controller` and its `ExoPlayer` (`PlayerScreen.kt:555-561`, `:737-753`). The
name refers to replacing the *plan*, not to the M6 handoff; the M6 entry is
`onPrepareAction` (`Controller.kt:2996`), reachable only from the reporter's
push on a later exchange, by which time the reporter is gone.

### 2.3 The client's own reopen kills the successor the server started for it (server)

On Apple and Android the changed selection *is* published before the reopen,
so the server reserves a row and starts priming a speculative worker. Nothing
cancels that work when the predecessor session is superseded: the
cancellation edges are commit, explicit abort, settings disable, incumbent
`Waiting|Stalled`, foreground contention and the 330 s deadline
(`http/hls.rs:7274-7330`, `:8745-8755`) — not `end_media_session` and not
the reopen's `previous_session_id` retirement. Two outcomes, both bad: on a
node with encoder headroom the orphan encodes for up to 330 s for nobody; on
a node at `DEFAULT_MAX_HW_SESSIONS = 2` (`admission.rs:41`) the reopen's own
live create sets `live_is_waiting`, the 25 ms foreground watch tears the
successor down as "foreground playback claimed prepared capacity"
(`:7307-7329`), and `record_preparation_staged(false)` is counted. That is
the `refused 1` on nynuc today.

### 2.4 Two things that are *not* the problem

The server's prime is real and the successor's playlist is servable
(§1.3). And two encoded renditions of one file share the frame grid
(`vodencode::frame_grid`, `vodencode.rs:236-247`; `grid.plan` at
`vodserve.rs:5698-5704`), so their segment boundaries coincide and a client
that aligns by film position lands on the same GOP. The mechanism for a clean
switch exists; it is the choreography around it that is wrong. (Copy ↔
transcode crossings take boundaries from the source fragment index instead —
`vodserve.rs:5706-5709` — and are not boundary-aligned; the clients already
align by seek, so this costs runway, not correctness.)

## 3. What "transparent" means here, exactly

A rung change the viewer cannot detect except by the picture changing:

- The incumbent keeps decoding and rendering, audio uninterrupted, from the
  tap until the successor's first frame is on screen. No overlay, no black,
  no pause.
- The successor is built while the incumbent plays; the switch happens at a
  film position ahead of the playhead that both pipelines have buffered.
- Failure of any step returns the viewer to exactly what they had — the
  incumbent — and only then, if the change was directed, falls back to the
  reopen they get today. A failed preparation must never cost more than no
  preparation would have.
- The time from tap to new quality on screen is allowed to be several
  seconds. It is not the metric. The metric is *frames lost*, which should be
  zero, and *audio glitch at the switch*, which should be inaudible.

Bandwidth is not the concern; encoder capacity is. The server already runs
the successor at `Priority::Speculative` and pre-empts it for any foreground
start (`admission.rs:220-222`, `hls.rs:7307-7329`), which is the right
trade. The plan keeps that.

## 4. Protocol changes — small, additive, gated on declared vocabulary

Every new field is optional on the wire and is emitted only to a client
whose request declares the action or capability that reads it, because
Apple's reporter throws `ControlProtocolError` on an action type outside its
declared vocabulary (`PlaybackControlReporter.swift:1077-1110`) and would
stop reporting for the rest of the session. Kotlin and Swift decoders ignore
unknown object keys (`Net.kt:19`, Codable default), so *fields* are safe
where *action types* are not.

### 4.1 `delivery.preparation` — tell the client a successor is being built

Add to `DeliveryView` (`playback_control.rs:898`), following
`subtitle_readiness`'s shape (optional, absence ≠ healthy):

```rust
/// State of the successor this playback's one preparation slot holds, when
/// this server evaluated it. `staging` — reserved or priming, no `Prepare`
/// yet; `offered` — a `Prepare` is in the action; `none` — slot empty.
/// Absent when this server did not evaluate it (an older relay peer).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub preparation: Option<String>,
```

`staging` is set on the dispatch exchange itself — the candidate is spawned
before the response is serialized only because the response *action* is
already decided; the field is cheap to add at `http/hls.rs:7184` — and on
every exchange while `active_preparation(...)` for this playback is
registered. Re-verify the emit site against `http/hls.rs` at build time.

Why a field and not a `hold`: `hold` means "production is deliberately not
advancing" and clients act on it as a reason not to reopen a stalled
session; overloading it would make a healthy preparing session look held.

### 4.2 Faster cadence while staging — `renew_after_ms` stays pinned, so the client polls

`is_valid_for` pins `lease.renew_after_ms == NEXT_EXCHANGE_MS`
(`playback_control.rs:751`) and both native reporters validate it, so the
server cannot shorten the interval without a protocol bump. Instead the
client, on seeing `preparation: "staging"`, notifies its reporter every
1,000 ms until `offered`, `none`, or its bound (§5). The server's per-session
exchange floor is 250 ms (`hls.rs:7256`), so 1 Hz is well inside it; the
extra exchanges last only as long as a prime, and there is at most one
preparation per playback.

### 4.3 Supersession cancels the preparation

`settle_activation_predecessor` (`http/hls.rs:3469-3531`) and the
`media_session_superseded` path (`:2215-2231`) take
`take_active_preparations_for_playback(playback_id)` and settle each as
`"predecessor superseded"`, exactly as `cancel_preparations_for_incumbent_wait`
does (`:7404-7408`). A client that reopens anyway — old build, failed
successor, viewer seeked away — must not leave a speculative encoder running
for 330 s. Also `end_media_session_for_release` (`:4396`). This is a fix in
its own right and lands first (§8 M0).

### 4.4 `quality: {mode: "auto", height: N}` — the Auto controller's ask (decision D3)

`QualitySelection` has `Auto | Original | Manual { height }`
(`playback_control.rs:380-386` and its `desired()`). The web Auto controller
picks a rung the server does not know about, so its move cannot be a
`SelectionChange` and cannot be prepared. The smallest wire change that lets
it be: `Auto` carries an optional `height` — "still Auto, and this is the rung
I want now" — treated by `take_preparation_dispatch`'s digest as a change and
by `candidate_request` as an explicit height while keeping `quality_auto =
true` on the recipe so the server's own Auto policy is not switched off. See
D3 before building this.

## 5. Client changes

The shape is the same on all three and Apple already has most of it:
**publish → wait, with the incumbent playing → build → commit → or fall back.**

### 5.1 Common contract

1. On a directed change, do not touch the incumbent. Publish the new
   selection (`reportIntent` / `notifyUrgently` / `notifyPlaybackControl`).
2. Wait for one of: a `Prepare` (→ existing build path), `preparation:
   "none"` after the ask was accepted (server declined → reopen now),
   or the bound. **Bound: 12 s** from the accepted exchange. Rationale: the
   viewer is watching the old quality throughout, so waiting costs nothing
   visible; 12 s covers store reservation (5 s budget) plus a cold encoder
   admission (`QUEUE_WAIT = 5s`, `admission.rs:46`) with margin, and is far
   below the 45 s prime budget the server allows itself. A prime that has not
   produced a `Prepare` in 12 s is one the client should stop waiting for
   and the server should be told to abort (§5.1 step 4).
3. While waiting and `preparation == "staging"`, exchange at 1 Hz (§4.2).
4. On bound expiry: send `aborted` if an `action_id` was seen, else nothing
   (the server's supersession cancel from §4.3 covers the rest), then reopen
   exactly as today.
5. A seek during the wait cancels the wait and reopens at the seek target —
   position is not in the digest (§1.3), so the successor would be built at
   the wrong second. (The server's `incumbent_waiting` cancel does not fire
   on `Seeking`, `hls.rs:7096-7101`; the client owns this.)
6. Show nothing during the wait except the toast the menu already shows. The
   quality badge changes on commit, not on tap — `renderPlayerInfo()` /
   `sessionHeight` already move with the session identity on all three.

### 5.2 Web

- `setQuality` (`index.html:13752`): replace the `play()` with
  `requestPreparedQualityChange(PLAYER, pos)` implementing §5.1, falling back
  to the existing `play(...)` call. Keep `PENDING_ATTEMPT_REASON = "quality"`
  for the fallback only.
- `switchAutoRung` (`:11864`) and `rescueAutoSupply` (`:11880`): same helper
  when the incumbent is *not* stalled (the server cancels preparations on
  `Waiting|Stalled` anyway, so a stalled reopen stays a reopen); the Auto ask
  needs §4.4.
- The existing prepared block needs one change: `beginPreparedReplacement`
  runs the successor muted *and playing* (`:8127`), commits at a 4 s lead
  (`PREPARED_BUFFER_LEAD_MS`, `:7856`), hides and mutes the incumbent in the
  same synchronous block as it exposes the successor (`:8254-8266`). That is
  already the right choreography. Measure the audio seam (§8 M3) before
  changing the lead.
- Do not reopen the reporter across the wait: `p.mediaAttachment` must stay
  set, since that is the guard that dropped the `Prepare` in §2.2.

### 5.3 Apple

- `offerPreparedQualityChange` (`PlayerController.swift:3298`): the wait
  moves off `askForAction`'s single-exchange settle. Publish, then loop on
  the answers slot until a `Prepare`, `preparation == "none"`, or 12 s,
  notifying at 1 Hz while `staging`. `askForAction`'s stall semantics are
  right for stalls and wrong here; add `awaitPreparedOffer(bound:)` beside
  it rather than widening the stall bound.
- Everything downstream (`startPreparedSuccessor`, `commitPreparedSuccessor`,
  `replaceCurrentItem`, first-frame proof, `committed`) stays. The
  hardware-measured first-frame cost of the item swap is 24–160 ms
  same-codec (`Caps.swift:284-286`); whether that reads as a visible hitch
  is what §8 M3 measures. If it does, the alternative is a second
  `AVPlayerLayer` cross-faded on the successor's first frame — a larger
  change, deferred until measured.
- Update the status page: `canOfferPreparation` no longer exists.

### 5.4 Android

- `Controller.prepareReplacement` (`Controller.kt:1187`): after
  `publishIntent`, await the offer per §5.1 instead of
  `planReplacement.route(force = true)`; route only on decline/bound.
  `onPrepareAction` (`:2996`) is the build path and stays.
- Remove the incumbent freeze during final alignment (`player.playWhenReady
  = false` at `:3133`, `:3142`). It is a visible pause on every prepared
  switch and web proves it is unnecessary: run the successor at volume 0
  with `playWhenReady = true` (already `:3623-3631`) and commit when its
  buffered-through leads the *moving* playhead by the runway, seeking the
  successor onto the incumbent's second only if drift exceeds the 250 ms
  slack (`PREPARED_ALIGNMENT_SLACK_MS`). The `DISCONTINUITY_REASON_SEEK`
  proof (`:3052-3063`) still gates the commit.
- `preparedReplacementEnabled` is read once per player (`:562-576`); fine,
  document it in the Developer row rather than changing it.

### 5.5 Developer tab

Each client's existing "prepared handoff" card gains two advisory rows: the
last change's outcome (`committed <first-frame ms>` · `declined <reason>` ·
`timed out` · `fell back`) and the server's `preparation` state as of the
last exchange. Advisory only, never gating — per the standing rule.

## 6. Auto — where server-driven adaptation fits (decision D3)

Today only the web has an Auto controller (`autoControllerTick`,
`index.html:11924`, gated by the server's `playback_auto_abr` switch), its
evidence is hls.js's `bandwidthEstimate`, which on a JIT server measures
`min(link, encode)` of the *current rung* and jumps ~2× when the rung
changes — the 1080↔720 limit cycle of 2026-08-18. Apple and Android have no
controller; "Auto" is the server's create-time choice plus a one-rung stall
step-down inside the reopen (`transcode.rs:18077-18088`).

Two ways to make Auto changes prepared rather than reopened:

- **D3-a, client asks (§4.4).** Minimal server change; keeps the brain where
  [ADAPTIVE-QUALITY.md](../streaming/ADAPTIVE-QUALITY.md) put it; leaves the
  rung-dependent evidence problem in place and leaves Apple/Android without
  Auto.
- **D3-b, server proposes.** The server already has the evidence the doc
  says is right — `recent_producer_speed`, `delivered_bps`, the client's
  `client_runway_ms` and `observed_download_bps` all arrive on every
  exchange. A `PreparationPurpose::AutoAdaptation` that stages a rung change
  when the producer sustains `recent_speed < 1.0` with runway shrinking (down)
  or `> 2.0` with runway full and a rung above available (up), with
  hysteresis and a failed-rung memory, gives all three clients Auto through
  the same `Prepare` they already implement, and deletes the web controller.
  It is the larger change and it moves a policy the doc deliberately placed
  in the client.

The recommendation is D3-a now (it is needed for the web's existing
controller either way) and D3-b as its own plan after M3's measurements
exist. Neither is in M1–M3.

## 7. Non-goals

- **Multivariant HLS (ADAPTIVE-QUALITY Phase 3).** One session advertising
  every rung with lazily started encoders would let hls.js/AVPlayer/ExoPlayer
  switch natively. It is a rewrite of the session model, it cannot express
  the DV/HDR/copy-vs-transcode crossings M6 already handles as axes, and it
  buys nothing the prepared path does not once §5 lands. Not this plan.
- **Server-shortened exchange cadence.** `renew_after_ms` is pinned by every
  client's validator; changing it is a protocol version, not a field.
- **Two-encode steady state.** The successor is speculative and pre-emptible
  and the incumbent is retired at commit. Nothing here keeps two encodes
  running for longer than one switch.
- **Changing what a stall does.** A stalled incumbent has nothing to keep
  continuous; the server cancels its preparations and the reopen stays.
- **Any code gate on the feature.** Developer tab rows are advisory.

## 8. Milestones

Each is one PR, built in the fast lane, adversarially reviewed once as a
whole, full suite once after the findings, then merged.

### M0 — the server stops paying for successors nobody will watch

§4.3 supersession cancel, plus `delivery.preparation` (§4.1). Acceptance: a
unit test that stages a successor, supersedes the predecessor via
`previous_session_id`, and observes `settle_cancelled_preparation` with
reason `predecessor superseded` and the speculative worker gone; the wire
conformance test (`tests/validation/test_control_wire_conformance.py`)
pins the new optional field; `preparation_staged_total{outcome="refused"}`
stops incrementing on a directed change from a current client (checked on
nynuc's `/metrics` after deploy).

### M1 — web: the quality menu and Auto go through the handoff

§5.2 and §4.4 (client side only; the server accepts `height` on `auto`
in the same PR as a no-op digest input if D3 is undecided — see D3).
Acceptance: `tests/playback/web-control.test.js` cases for publish → offer →
commit, publish → `none` → reopen, publish → 12 s → `aborted` + reopen, and
seek-during-wait → reopen at target; a browser run against nynuc with
`plurx_playback_control_actions_total{action="prepare",platform="web"}`
incrementing and one `committed` acknowledgement per menu change; client
log `quality_switch` carries `via=prepared|fallback`.

### M2 — Apple and Android wait for the offer

§5.3 and §5.4. Acceptance: the `PreparedReplacement.swift` /
`PreparedReplacement.kt` unit suites cover the wait state machine
(offer · decline · bound · seek); no compile is possible in the session VM,
so the review reproduces the reducers in Python against the fixtures as the
playback-surface PRs did; then the hardware prompt
([M6-APPLE-HARDWARE-ACCEPTANCE.md](M6-APPLE-HARDWARE-ACCEPTANCE.md), and an
Android twin) run by a GPT session with device access.

### M3 — measure the switch itself

Instrument on all three: frames presented in the 2 s around commit (dropped
> 0 is a finding), audio discontinuity (web `AudioContext` analyser on the
element; Apple `AVPlayerItemAccessLog` + a `MTAudioProcessingTap` if needed;
Android `AudioSink` underrun counters), and tap-to-new-quality wall time.
Report into the Developer rows (§5.5) and the client log. Acceptance: twenty
consecutive directed changes on each platform with zero dropped frames and
no audible seam, from a realistic runway, on the fleet build — the number
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) says nobody has.

## 9. Decisions that are Paul's

- **D1 — the wait bound.** 12 s proposed (§5.1). Shorter makes the fallback
  more common on a busy node; longer only delays the new quality on a node
  that will never answer.
- **D2 — `delivery.preparation` as a field vs a new advisory action type.**
  Field proposed; an action type would need per-client vocabulary
  declaration before any client could see it.
- **D3 — Auto.** D3-a (client asks, §4.4) now and D3-b (server proposes,
  §6) later, or straight to D3-b and delete the web controller.
- **D4 — Apple's switch primitive.** Keep `replaceCurrentItem` (measured
  24–160 ms first frame) and measure in M3, or build the second-layer
  cross-fade now.
- **D5 — order.** M0 → M1 → M2 → M3 as written, or M0 → M2 (Apple is
  closest) → M1.
