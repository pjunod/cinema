# Live TV start stall and the tvOS playback surface — why every start pauses at 5–14 s, and what the fullscreen screen should be

**Status:** diagnosis + design, awaiting Paul's two rulings (§4, §6) ·
**Measured against:** `main` at `de9f153c` (Apple build 147) · **Written:** 2026-09-13

Companion to [LIVE-TV-GUIDE-AND-START-RELIABILITY.md](LIVE-TV-GUIDE-AND-START-RELIABILITY.md)
(why a start used to take 90 s to be *allowed*) — this is *why a start that
is allowed then freezes a few seconds in, on every client*, and *what the
Apple TV fullscreen surface should look like*, which nobody had revisited
since it was first drawn. Renders are in
[`../mockups/live-tv/`](../mockups/live-tv/) as `surface-*.png`
([surface-controls.png](../mockups/live-tv/surface-controls.png) is the one to look at first) and on the
design canvas "Live TV Playback Surface". The renders are drawn from the
constants in the source and the brand kit, not from a device capture.

Read §1 first — the whole diagnosis is one measured table. §3 is the fix,
with the numbers each option costs. §5 is the surface design; §7 the build
plan. Nothing here touches the guide reducers, the lease, or the input
contract's routing table; a builder who thinks it must, stops and says so.

## 1. The pause is a stall, and the server manufactures it

**What Paul sees:** start a channel, it plays a few seconds, freezes, the
signal readout appears, then it plays on without incident.

**What is happening.** The owner starts FFmpeg with
`-hls_init_time 1 -hls_time 4 -hls_list_size 6`
([`live_tv.rs:127–138`](../../crates/plurxd/src/live_tv.rs)) and answers the
start POST the moment `inspect_scratch` returns a playlist with **one**
listed segment ([`live_tv.rs:4796–4842`](../../crates/plurxd/src/live_tv.rs);
`inventory.segments.is_empty()` is the only gate, at 5758). FFmpeg's hlsenc
cuts segments at `init_time` until the list is full, then a catch-up
segment, then `hls_time`. Measured here with the owner's exact producer
arguments (`LIVE_HLS_OUTPUT_ARGS` plus the encode-route keyframe arguments
at 5375–5382) against a realtime source:

| Listed at | Media sequence | `TARGETDURATION` | Segments in the window |
|---|---|---|---|
| 0.65 s … 5.68 s | 0 | 1 | 1 s, 1 s, 1 s, 1 s, 1 s, 1 s |
| 6.67 s | 1 | 1 | six 1 s (seq 6 is 1 s) |
| 9.64 s | 2 | **3** | … 1 s, **3 s** |
| 13.67 s | 3 | **4** | … 3 s, **4 s** |
| every 4 s after | +1 | 4 | steady |

So the client attaches with **1 s of runway** and stays about 1 s behind
live. A live HLS client can only fetch a segment once it is listed, and a
segment is listed only when it is complete — so when the cadence jumps from
1 s to 3 s and then to 4 s, the playhead reaches the end of what exists and
waits for a whole segment to finish. Driving the three clients' documented
start rules over the measured listing times (download and decode
instantaneous, so these are lower bounds):

| Client start rule | Stalls | Where | Settles |
|---|---|---|---|
| AVPlayer — 3 × `TARGETDURATION` behind the end (Apple default) | **3.6 s in three events** | 0.5 s at position 4 s · 1.5 s at 7 s · 1.5 s at 10 s | 4.3 s behind live |
| hls.js — `liveSyncDurationCount: 2` ([`index.html:16517`](../../crates/plurxd/src/web/index.html)) | same | same | same |
| ExoPlayer — `setTargetOffsetMs(4_000)` ([`LiveTvPlayer.kt:196`](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt)) | same | same | same |

All three are identical because the playlist gives none of them more than
1 s to start from; their live-sync settings never get a say. A real player
merges these into one visible pause: AVPlayer's
`automaticallyWaitsToMinimizeStalling` (the default, and `LiveTvView.swift`
sets nothing else) rebuffers until it believes it can keep up, which is
exactly "a few seconds in, one pause of a few seconds".

**How to read it:** `-hls_init_time 1` buys a first frame ~3 s sooner and
pays for it with 3.6 s of freeze between 5 and 14 s of playback. The
optimisation is a net loss on every client.

### The signal readout is a coincidence, not the cure

The Apple heartbeat ([`LiveTvView.swift:169–196`](../../clients/apple/Sources/LiveTvView.swift))
sleeps 5 s, and if the position advanced (`LiveTvPlaybackWatchdog.observe`,
[`LiveTv.swift:1010–1015`](../../clients/apple/Sources/LiveTv.swift)) it
sends the keepalive and then reads `/status`, which is when `status.signal`
first exists and the strength/quality percentages appear. Tick 1 lands at
5.65 s after publish — inside the first stall. The status handler holds only
the per-session `signal_cache` mutex and fetches `/status.json` from the
HDHomeRun ([`live_tv.rs:3079–3124`](../../crates/plurxd/src/live_tv.rs)); it
never touches the producer, the scratch inventory, or `session.state`. The
stream resumes because segment 8 landed, not because the signal did.

### Why it reads as a pause rather than as buffering

The tvOS live surface has no waiting indicator. `fullscreenSurface`
([`LiveTvView.swift:2300–2486`](../../clients/apple/Sources/LiveTvView.swift))
observes nothing of the player's `timeControlStatus`; the finite player
draws a spinner with a runway line while it waits
([`PlayerView.swift:836–853`](../../clients/apple/Sources/PlayerView.swift)).
A frozen frame with the overlay hidden is indistinguishable from a pause.

## 2. What was ruled out

- **The signal fetch or the keepalive stalling the producer** — no shared
  lock (above), and the web and Android pause the same way with no signal
  readout on screen.
- **The ingress relay** — it forwards playlist and segments; the segment
  timing above is the owner's FFmpeg alone.
- **The encoder being slower than realtime** — the 1 s segments land every
  1.00 s in the measurement and the 4 s segments every 4.0 s; the graph
  keeps up. (On the real tuner the first segment is 5.5–7 s after the
  request because of `-probesize`, [HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md);
  that delays everything equally and changes nothing here.)
- **The start barrier / request-id work** — that decided whether a start
  is allowed; this happens after it is.

## 3. The fix — give the client a whole segment of margin from the first frame

The constraint is physical: a client must start at least one steady-state
segment behind live, or it stalls the first time it catches up. Two levers
move it — the steady segment length, and how much listed media exists when
the owner answers the start. Measured on the same timeline model:

| Producer arguments | Answer the start at | First frame vs today | Stalls (AVPlayer · hls.js · Exo) | Settles |
|---|---|---|---|---|
| today: `init 1 · time 4 · list 6` | 1 listed segment | — | 3.6 s · 3.6 s · 3.6 s | 4.3 s behind |
| today's arguments | 3 listed segments | +2.0 s | 1.6 s · 2.6 s · 1.6 s | 4.3 s |
| `time 4 · list 6`, no `init_time` | 1 listed segment | +3.0 s | 0 · 0 · 0, **zero margin** | 3.7 s |
| **`time 2 · list 12`, no `init_time`** | **2 listed segments** | **+3.0 s** | **0 · 0 · 0, one segment of margin** | **3.6 s** |
| `init 1 · time 2 · list 12` | ≥ 3 s listed | +2.0 s | 0 · 0.1 s · 0 | 2.6 s |

**Recommended: uniform 2 s segments, answer at two listed segments.**
`TARGETDURATION` never changes (RFC 8216 §6.2.1 says it must not; today it
goes 1 → 3 → 4 and every client tolerates it, which is luck), the 24 s
window is unchanged (`12 × 2 s`), every client's existing live-sync setting
already lands 4 s behind the edge (hls.js 2 × 2 s, Exo 4 s, AVPlayer 3 × 2 s
clamped to the list), and the viewer ends up *closer* to live than the
stalled start leaves them today. The cost is 3 s more before the first
frame — the same 3 s the freeze costs now, spent before the picture instead
of during it. The `init 1 · time 2` row saves one of those seconds but
reintroduces a drifting `TARGETDURATION` and needs hls.js moved to
`liveSyncDuration: 3`; it is the fallback if Paul wants the second back.

### 3.1 What changes on the owner

- `LIVE_HLS_OUTPUT_ARGS` ([`live_tv.rs:127`](../../crates/plurxd/src/live_tv.rs)):
  drop `-hls_init_time 1`; `-hls_time 2`; `-hls_list_size 12`;
  `-hls_delete_threshold 2` so the deletion lag stays 4 s of wall time for a
  client holding the previous manifest.
- `MAX_LISTED_SEGMENTS` 6 → 12 and `MAX_DELETION_LAG_SEGMENTS` 1 → 2
  ([`live_tv.rs:142–143`](../../crates/plurxd/src/live_tv.rs)); the
  inventory budget checks at 5776 and 5877 follow the constants.
- The publish gate in the producer loop ([`live_tv.rs:4816`](../../crates/plurxd/src/live_tv.rs)):
  `published` becomes true when `inventory.segments.len() >= 2`, not when it
  is non-empty. `state.publication` keeps updating from the first segment
  as it does now, so nothing served can be older than the inventory.
  `STARTUP_FEEDING_TIMEOUT` (30 s) still covers the measured 18.1 s ATSC 3.0
  first segment plus one more.
- Tests that pin the old numbers change in the same commit:
  `live_hls_publishes_short_startup_segments_before_steady_cadence`
  (8493–8506) becomes the assertion that `init_time` is absent and the pair
  is `2 / 12`; `live_tv_software_hls_argument_baseline_is_stable` (8376)
  re-freezes the argument list; the inventory tests at 8985–9008 use the
  constants already.
- Copy routes (ATSC 3.0 HEVC, fmp4) cut at the source's own keyframes, so a
  3 s GOP gives 3 s segments; the rule "two listed segments" still gives one
  segment of margin by construction, which is why the gate is a count and
  not a number of seconds.

### 3.2 What changes on the clients

Nothing is *required*. The web (`liveSyncDurationCount: 2`), Android
(`4_000` ms) and Apple (default) start rules all sit ≥ 4 s behind the edge
once the playlist offers it. Two things are worth doing in the same lane:

- **Apple: draw waiting.** Observe `player.timeControlStatus`; while it is
  `.waitingToPlayAtSpecifiedRate` show the catching-up tile (§5.4) over the
  picture with the overlay hidden. This is what the finite player already
  does; without it any residual rebuffer is a "pause" again.
- **Apple: raise `preferredForwardBufferDuration` to 0** (let AVFoundation
  choose) or leave it at 12 — it has no effect on a live playlist shorter
  than that and was measured to change nothing; leave it, note it.

## 4. Ruling wanted on the fix

Uniform 2 s segments at two listed segments (+3 s to first frame, zero
stalls, spec-clean), or `init 1 / time 2` at ≥ 3 s listed (+2 s, zero
stalls, drifting `TARGETDURATION`, one web constant). The recommendation is
the first; the second is one line different and also measured.

## 5. The tvOS fullscreen surface — what is wrong and what it becomes

### 5.1 What is there today, from the source

`fullscreenSurface` ([`LiveTvView.swift:2300–2486`](../../clients/apple/Sources/LiveTvView.swift))
draws, over the picture with no scrim: a top-left cluster of programme
title (30 pt), channel (`.caption`, 25 pt on tvOS), the technical summary
(`.caption2`, 23 pt at 78 % opacity — one line that runs off the screen on a
busy channel) and the airing line; five text-only buttons top-right in
`TVReadableButtonStyle(prominent: false)` with `.focusEffectDisabled()`
(2346–2371) — app-palette plates (`surfaceHi` / `onBg`, so in a light
appearance they are white plates on video), a 4 pt stroke as the only focus
cue; and a 4 pt white `ProgressView` with no times (2374–2375). Info opens
the phone sheet `LiveTvTechnicalDetails` (551–608) unchanged: 9 pt labels
in a 54 pt column, `.caption` values.

**Why the buttons stop being navigable.** The reveal layer
(`Color.clear.contentShape(Rectangle()).focusable(true)`, 2317–2326) exists
so a hidden overlay still has something focused to receive a press. It is
full-screen and it stays focusable *while the overlay is visible*. From any
button, Down (or Left from "Guide") is a shorter focus move to the giant
layer than to a neighbour, so the engine moves there; its adapter is wired
with `state: { .fullscreenHidden }`, which routes every direction to
`.reveal` — the overlay is already visible, so nothing changes and focus
never leaves the layer. `onChange(of: overlayVisible)` (2464–2466) only
resets focus when visibility flips, so the trap holds until the 4 s
auto-hide fires and the layer becomes the legitimate owner. The buttons
were never unfocusable; they were unreachable.

### 5.2 The new surface, controls revealed

Render: `surface-controls.png`. Everything sits in a bottom band over a
gradient scrim (520 pt, `rgba(5,5,6)` 0 → 0.62 at 42 % → 0.94), with a
matching 200 pt scrim at the top. Chrome is projection black
(`Palette.playerChrome`) in every appearance, never the app palette — the
brand's "playback is always midnight" rule and what the finite player does.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ ● LIVE  4.2 s behind live        ▮▮▮▮▯ 92% signal · 100% quality         │
│                                   transcode 720p h264  ac-3 5.1  6:41 PM  │
│                                                                          │
│                              (picture)                                   │
│                                                                          │
│ ┌────┐ 7.1 · WPLX-DT · HD                                    ┌────────┐  │
│ │7.1 │ The Late Edition                                      │  art   │  │
│ └────┘ S12 E184 · Tuesday · synopsis, one line               └────────┘  │
│ 6:30 ━━━━━━━━━━━━━━━━──────────── 7:00 PM  19 min left · Next 7:00 · …   │
│ [‖ Pause] [Guide] [Channels] [i Info] [… More]               MENU hides  │
└──────────────────────────────────────────────────────────────────────────┘
```

| Element | Content and source | Type (tvOS pt) |
|---|---|---|
| Top-left status | `LIVE` (accent dot, glows while live; dims to `faint` when paused) · behind-live seconds from `seekableTimeRanges.end − currentTime` | mono 20, chips 40 pt |
| Top-right telemetry | signal bars + `strengthPercent` / `qualityPercent` from `LiveTvStatus.signal` · delivery method from `LiveTvDelivery.videoAction`/`output` · audio from `audioAction`/`audioCodec`/`audioChannels` · clock | mono 20 · clock 24 |
| Logo tile | 112 pt, `raised` with scanlines; `guideNumber` in mono 40, or the guide channel `imageUrl` when it exists | mono 40/700 |
| Eyebrow | `guideNumber · guideName · HD/SD` (`pictureClass`) | mono 22 |
| Title | `airing.now.title`, else `live.title` | 46/600, one line |
| Sub | `episode · episodeTitle · synopsis`, one line, ellipsis | 24 dim |
| Art | `airing.now.imageUrl` at 300 × 169, hidden when absent (the title column widens) | — |
| Progress | `airing.progress` on a 6 pt accent bar with glow; start / end times, minutes left, `Next HH:MM · title` | mono 20 · sans 22 |
| Buttons | Pause / Play live · Guide · Channels · Info · More — the five actions the surface has today, as 56 pt pills, 28 pt stroke icons, 24/600 labels, `rgba(16,16,20,.72)` with a 1 pt line; focused = 4 pt accent ring, accent glow, 1.045 lift (the house `TVReadableButtonStyle` focus, on player chrome) | 24/600 |
| Remote hint | `MENU hides · PLAY/PAUSE pauses` | mono 18 faint |

The existing actions keep their existing handlers: Guide → `temporaryGuide`,
Channels → `requestChannelFocus(); fullscreen = false`, Pause →
`live.togglePause()`, Info → `showingInfo`, More → `showingMore`. No button
is added: `favorite` is a read-only marker from the tuner's lineup
([`live_tv.rs:6439`](../../crates/plurxd/src/live_tv.rs)), so a Favorite
button would have nothing to call.

### 5.3 Focus, and the end of the trap

- The reveal layer is `.focusable(!overlayVisible)`. While the overlay is
  showing there is exactly one focus region, the button row, wrapped in a
  `.focusSection()` so the engine keeps directions inside it.
- Default focus on reveal is **Pause** (`.play`), not Guide — it is the
  button a viewer reaches for on a playback surface.
- Belt and braces: `onChange(of: focusedControl)` — if focus is `.reveal`
  while `overlayVisible`, set it back to `.play`. That is the line that
  would have caught today's trap.
- Every focus move and every press resets the 4 s auto-hide; the routing
  table (`LiveTvInputRouting.tenFoot`) is untouched — `fullscreenControls`
  directions are still `.focusControl`, Select `.activate`, Menu `.hide`.

### 5.4 The three other states

- **Info** (`surface-info.png`): replaces the phone sheet with a 1240 pt
  ledger on `rgba(10,10,12,.96)` — PROGRAMME · CHANNEL in the left column,
  DELIVERY · SIGNAL · PLAYER in the right; mono 18 labels in a 150 pt
  column, 22 pt values, accent 17 pt eyebrows; three signal meters. The
  PLAYER rows come from `AVPlayerItem.accessLog()` (observed bitrate,
  dropped frames, stall count) and `seekableTimeRanges` (behind live,
  buffered). Close is the one focusable and takes focus on appear, as
  `PlaybackStatsView` does.
- **Catching up** (`surface-catching-up.png`): overlay hidden, a centred
  glass tile — spinner, "Catching up to live", mono
  "waiting for the next segment · N s behind" — shown while
  `timeControlStatus == .waitingToPlayAtSpecifiedRate`. This is the finite
  player's waiting tile with live copy.
- **Paused** (`surface-paused.png`): a 132 pt glass pause glyph centred, the
  band stays, Pause becomes "Play live", the status chip reads
  `paused · m:ss`, `LIVE` dims, and the standing message line carries the
  30 s tuner-release rule the controller already publishes.

### 5.5 What the design does not change

No new persisted state, no new routes, no change to `LiveTvInputRouting` or
the contract fixture, no change to the over-picture guide panel (it already
matches the browser's grid), no channel strip on tvOS — Up/Down on a hidden
overlay still only reveals it (the 2026-09-02 ruling), and the Guide and
Channels buttons are the two ways to change channel from fullscreen.

## 6. Ruling wanted on the surface

The band-and-telemetry layout in §5.2 as drawn, or with the telemetry folded
into the band (one fewer thing on screen, but the signal and delivery facts
disappear whenever the band does). The renders show the first.

## 7. Build plan — two independent PRs

Each PR: proper commits on the fast lane only, opened as a draft (`WIP:`),
one adversarial review, findings fixed, the full suite once, merge. Neither
touches the other's files.

### 7.1 Server — the segment cadence (Rust)

Files: `crates/plurxd/src/live_tv.rs` only (§3.1). Acceptance:

```bash
cargo test -p plurxd live_hls -- --nocapture          # the re-pinned argument tests
cargo test -p plurxd inventory                        # 12-segment window, 2-segment deletion lag
scripts/live-tv-hardware --self-host --device <ipv4>  # "real segment duration" reads 2.0 s; startup latency +≈2 s
```

Plus the measurement that matters and that only a device can give: on the
Apple TV, start a channel and confirm no stall in the first 30 s
(`accessLog` `numberOfStalls == 0`) — that is the hand-off in §8.

### 7.2 Apple — the surface (Swift, tvOS only)

Files: `clients/apple/Sources/LiveTvView.swift` (`fullscreenSurface`, the
Info sheet call site, `LiveTvTechnicalDetails` gets a tvOS body),
`clients/apple/Sources/LiveTv.swift` (a `LiveTvPlayerController` publisher
for `waiting` from `timeControlStatus`, and `behindLive`/`buffered`
from the item), `clients/apple/Tests/LiveTvTests.swift`. The phone surface
is untouched. Acceptance: `xcodebuild test` on the tvOS destination; a
1080p simulator screenshot of each of the four states beside its render;
and on the physical Apple TV, Down from every button reaches a button.

## 8. Hand-off for the physical pass

Paste to a session with the hardware after both PRs merge:

> On the Apple TV (plurx build ≥ the one carrying PR "Live TV: 2 s segments"
> and PR "tvOS Live TV fullscreen surface"): open Live TV, start channel
> 7.1 (or any ATSC 1.0 channel), and time from Select to first frame with a
> stopwatch; then watch 30 s and note any freeze. Open Info and read the
> PLAYER rows — report "behind live", "buffered", and "stalls". Then press
> Menu to hide the overlay, press Select to reveal it, and from each of the
> five buttons press Down, Left and Right, reporting where focus lands each
> time (it must never leave the button row). Finally hold the picture paused
> for 35 s and confirm the tuner-released message appears. Report the first-
> frame time, the stall count, and the focus table.
