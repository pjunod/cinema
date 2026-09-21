# Live TV start stall and the tvOS playback surface — why every start pauses at 5–14 s, and what the fullscreen screen should be

**Status:** done — diagnosis + design, awaiting Paul's two rulings (§4, §6) ·
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
`parsed.segments.is_empty()` at 5758 is the only gate). FFmpeg's hlsenc
cuts segments at `init_time` until the list is full, then a catch-up
segment, then `hls_time`. Measured here with the owner's exact producer
arguments (`LIVE_HLS_OUTPUT_ARGS` plus the encode-route keyframe arguments
at 5375–5382; the encoder itself ran `ultrafast` at 640×360, which changes
encode latency and nothing about where hlsenc cuts) against a realtime
source:

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
1 s to start from; their live-sync settings never get a say. These are
model numbers — no real player was run against the recording (headless
Chromium here has no H.264 decoder, and there is no AVPlayer) — so how the
three events present is inferred, not measured: AVPlayer's
`automaticallyWaitsToMinimizeStalling` (the default; `LiveTvView.swift` sets
nothing else) rebuffers until it believes it can keep up, and ExoPlayer's
`setBufferDurationsMs(4_000, 12_000, 1_000, 2_000)`
([`LiveTvPlayer.kt:166`](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt))
needs 2 s buffered after any rebuffer, so both lengthen and merge the
events. The physical pass (§8) reads `numberOfStalls` and the stall
durations off `accessLog` and settles it.

**How to read it:** `-hls_init_time 1` buys a first frame ~3 s sooner and
pays for it with 3.6 s of freeze between 5 and 14 s of playback. The
optimisation is a net loss on every client.

### The signal readout is a coincidence, not the cure

The Apple heartbeat ([`LiveTvView.swift:169–196`](../../clients/apple/Sources/LiveTvView.swift))
sleeps 5 s, and if the position advanced (`LiveTvPlaybackWatchdog.observe`,
[`LiveTv.swift:1010–1015`](../../clients/apple/Sources/LiveTv.swift)) it
sends the keepalive and then reads `/status`, which is when `status.signal`
first exists and the strength/quality percentages appear. Tick 1 lands 5 s
after publish — in the model, 0.5 s after the first stall ends and 2.5 s
before the second begins, i.e. in the middle of the cluster; where it
lands relative to the *visible* pause depends on how the player merges the
events (above). What it cannot be is the cause: the status handler takes
`session.state` for microseconds to check the phase and stamp `last_touch`
(2960–2976, 1254–1273), holds the per-session `signal_cache` mutex across
one `/status.json` GET to the HDHomeRun
([`live_tv.rs:3079–3124`](../../crates/plurxd/src/live_tv.rs)), and never
touches the producer or the scratch inventory. The stream resumes because
segment 8 landed, not because the signal did.

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
- **Not ruled out, and cheap to check on the device:** on tvOS a start
  plays *inline* first (`selectAiring`, 2289–2295) and goes fullscreen on a
  later press, which dismantles the inline `PlayerSurface`
  (`playerLayer.player = nil`, [`PlayerSurface.swift:290–299`](../../clients/apple/Sources/PlayerSurface.swift))
  and re-hosts the `AVPlayer` in a new layer under the cover. That is also
  "a few seconds in". The web and Android pause the same way with no such
  re-hosting, so it is not *the* cause, but the hand-off in §8 records
  whether the freeze happens while still inline. Nothing else on any client
  pauses, seeks or replaces the item after `play()` (Apple 155–167, web
  16517–16522, Android 194–199).

## 3. The fix — keep the short cadence, drop the jump

The constraint is physical: a client must start at least one steady-state
segment behind live, or it stalls the first time it catches up. Two levers
move it — the steady segment length, and how much listed media exists when
the owner answers the start. The first frame, in turn, is bounded below by
the tuner (first byte in 0.36 s on one measured channel and 2.77 s on
another — not a constant), the probe (~1 s of `-analyzeduration` on a
broadcast mux), one segment, and the fetch — the segment is the only one
of those the server chooses. Paul's original
intent with `-hls_init_time 1` was exactly that: a short first segment so
the start is no slower than the tuner. The mistake was not the short
segment; it was letting the cadence *grow* afterwards. Measured on the
same timeline model:

| Producer arguments | Answer the start at | First frame vs today | Stalls in the model (AVPlayer · hls.js · Exo) | Settles |
|---|---|---|---|---|
| today: `init 1 · time 4 · list 6` | 1 listed segment | — | 3.6 s · 3.6 s · 3.6 s | 4.3 s behind |
| today's arguments | 3 listed segments | +2.0 s | 1.6 s · 2.6 s · 1.6 s | 4.3 s |
| `time 1 · list 24`, no `init_time` | 1 listed segment | **as today** | 0.5 s · 0.5 s · 0.5 s, one hiccup at 4 s | 1.9 s |
| **`time 1 · list 24`, no `init_time`** | **2 listed segments** | **+1.0 s** | **0 · 0 · 0, one segment of margin** | **≈ 2 s** |
| `time 2 · list 12`, no `init_time` | 2 listed segments | +3.0 s | 0 · 0 · 0 | 3.6 s |
| `time 4 · list 6`, no `init_time` | 1 listed segment | +3.0 s | 0 · 0 · 0, zero margin | 3.7 s |
| `init 1 · time 2 · list 12` | ≥ 3 s listed | +2.0 s | 0 · 0.1 s · 0 | 2.6 s |

**Recommended: uniform 1 s segments, answer at two listed segments.**
(Model numbers; the candidate build measures them on all three clients
before anything merges — the implementation plan's §5.) The
first seven segments are already 1 s today; this keeps that cadence for the
whole session instead of jumping to 4 s. `TARGETDURATION` is 1 and never
changes (RFC 8216 §6.2.1 says it must not; today it goes 1 → 3 → 4 and
every client tolerates it, which is luck), the 24 s window is unchanged
(`24 × 1 s`), every client's existing live-sync setting lands ≥ 2 s behind
the edge (hls.js 2 × 1 s, Exo 4 s, AVPlayer 3 × 1 s clamped to the list),
and the viewer settles about 2 s behind live instead of 4.3 s. The cost is
one segment — 1 s — before the first frame, which is the answer arriving at
the end of segment two rather than segment one; nothing else in the start
path moves. Answering at one segment keeps today's first-frame time but
leaves a single 0.5 s hiccup at 4 s in the model, which is the thing Paul
reported, only smaller; the second segment is what buys the margin.

What 1 s segments cost that 4 s ones did not: one playlist reload and one
segment fetch per second per viewer (against a LAN owner and a relay that
forwards per request), a 24-entry playlist (~1.2 KB, against a 64 KiB
cap), and a scratch directory of ~29 files for `inspect_scratch` to walk
every 250 ms. Encode routes already force a keyframe every second
(`-force_key_frames expr:gte(t,n_forced*1)`, 5376–5382), so the encoder
does no extra work. Copy routes cut at the broadcast's own keyframes: an
ATSC 1.0 MPEG-2 GOP is ~0.5 s and gives 1 s segments; an ATSC 3.0 HEVC
mux with a 2 s GOP gives uniform 2 s segments and `TARGETDURATION` 2, and
the gate below is written so that still means two target durations of
media in hand.

### 3.1 What changes on the owner

- `LIVE_HLS_OUTPUT_ARGS` ([`live_tv.rs:127`](../../crates/plurxd/src/live_tv.rs)):
  drop `-hls_init_time 1`; `-hls_time 1`; `-hls_list_size 24`;
  `-hls_delete_threshold 4` so the deletion lag stays 4 s of wall time for
  a client holding the previous manifest.
- `MAX_LISTED_SEGMENTS` 6 → 24 and `MAX_DELETION_LAG_SEGMENTS` 1 → **5**
  ([`live_tv.rs:142–143`](../../crates/plurxd/src/live_tv.rs)). Five, not
  four: hlsenc renames a finished segment to its final name *before* it
  rewrites the playlist, so for an instant the scratch holds
  `threshold + 1` final segments that the playlist does not list. Measured
  at 2 ms sampling with a threshold of 2: unlisted count `{0, 1, 2}`
  throughout and `3` once in one of two 45 s runs — rare, and a 250 ms
  `SESSION_TICK` sample that lands in it is fatal. `inspect_scratch` at
  5776 answers that instant with `StreamFailed("… exceeded its segment
  inventory budget")`, which ends the session — the constant must be
  `threshold + 1`. (Today's `1 / 1` pair has the same hole: two unlisted
  for a few milliseconds every four seconds. Fix it in the same commit; it
  is the latent version of the same bug.) The literal "exceeds six
  segments" at 5879 and the `+ 1` at 9008 are hard-coded and follow the
  constants by hand.
- The publish gate in the producer loop ([`live_tv.rs:4816`](../../crates/plurxd/src/live_tv.rs)):
  `published` becomes true when the playlist **lists** at least two
  segments *and* at least two `TARGETDURATION`s of media.
  `ScratchInventory.segments` is `final_segments` — every final file on
  disk, listed or not (5772–5790) — so it is the wrong thing to count: in
  the rename-before-rewrite instant above it would answer the start with a
  one-segment playlist and reproduce today's stall for that viewer.
  `parse_playlist_bytes` (5799) reads neither `#EXT-X-TARGETDURATION` nor
  the `#EXTINF` durations today; it gains both, as strictly as it reads
  `#EXT-X-MEDIA-SEQUENCE` (hlsenc always writes them), and
  `ScratchInventory` carries `listed`, `listed_seconds` and
  `target_duration`. Read them *before* line 4814 moves the inventory into
  `state.publication`. The seconds half is what makes a copy route with a
  long GOP safe: two 2 s segments behind a `TARGETDURATION` of 2 is still
  a segment of margin, and a 1 s + 3 s pair behind a 3 is not answered
  until a third segment lands.
- **The progress watchdog must count listed segments, not the media
  sequence** (Astra, review finding 1): `last_progress` advances only when
  `media_sequence` advances (4804–4808), which with a 24-entry window is
  24 s on an encode route and 48 s on a 2 s-GOP copy route — past the 30 s
  `PRODUCER_PROGRESS_TIMEOUT` at 4875. Without this, every copy stream
  would start and then end half a minute in. The plan's §3.3 makes the
  newest listed sequence the progress mark.
- **Android starts where its explicit 4 s offset lands it**, which on a
  long-GOP copy route is the *second* listed segment with no margin
  (Astra, finding 3); the plan's §3.9 drops `setTargetOffsetMs(4_000)` so
  Media3 uses the playlist's own hold-back and starts on the first.
- The budget the second segment must fit is the relay's, not the
  producer's: a non-owner ingress gives the owner `START_EXCHANGE_ATTEMPT`
  = 20 s per POST ([`http/live_tv.rs:25`](../../crates/plurxd/src/http/live_tv.rs)),
  and when the last HTTP waiter leaves an unpublished session
  `StartupWaiter::drop` cancels it (4318–4334). The measured ATSC 3.0 first
  segment after the probesize fix is 8.997 s
  ([HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md), 2026-09-05
  row); one more segment lands at about 10–11 s. The hardware check in
  §7.1 records the new number.
- Tests that pin the old numbers change in the same commit:
  `live_hls_publishes_short_startup_segments_before_steady_cadence`
  (8493–8506) becomes the assertion that `init_time` is absent and the
  pair is `1 / 24` — note it currently looks for `-force_key_frames` inside
  `LIVE_HLS_OUTPUT_ARGS`, which does not contain it (the keyframe arguments
  are added at 5375–5382), so as written its closure panics; the builder
  confirms what the gate actually runs. `live_tv_software_hls_argument_baseline_is_stable`
  (8376) re-freezes the argument list; the inventory tests at 8985–9008
  take `MAX_LISTED_SEGMENTS` but hard-code the lag.

### 3.2 What changes on the clients

Nothing is *required* for the stall. The web (`liveSyncDurationCount: 2`),
Android (`4_000` ms) and Apple (default) start rules all sit ≥ 2 s behind
a 1 s-segment edge once the playlist offers it. Two settings change
meaning and are worth a line in the lane:

- **Web `liveMaxLatencyDurationCount: 4`** (same line as the sync count)
  is a multiple of `TARGETDURATION`: hls.js's forced catch-up seek moves
  from 16 s behind to 4 s behind. Still twice the sync distance, and a
  catch-up seek on a 1 s cadence is a 2 s jump, not a 16 s one; keep it,
  but the builder should know the tolerance is a quarter of what it was.
- **Android `maxOffsetMs 8_000`** is absolute and unchanged; the min
  buffer for playback to resume after a rebuffer is 2 s (`setBufferDurationsMs`
  above), two segments — which is exactly what the gate hands it.
- **Apple `preferredForwardBufferDuration = 12`** ([`LiveTvView.swift:156`](../../clients/apple/Sources/LiveTvView.swift))
  stays. Nothing here measured AVPlayer; the window is 24 s either way and
  the distance to the edge is what bounds the buffer, not this.

And one thing the Apple lane should do regardless: **draw waiting.**
Observe `player.timeControlStatus`; while it is
`.waitingToPlayAtSpecifiedRate` show the catching-up tile (§5.4) over the
picture with the overlay hidden. This is what the finite player already
does; without it any residual rebuffer is a "pause" again.

## 4. Ruling wanted on the fix

Uniform 1 s segments answered at two listed segments (+1 s to first
frame, zero stalls, ≈ 2 s behind live), or answered at one (first frame as
today, one 0.5 s hiccup in the model at 4 s, ≈ 1.9 s behind). The
recommendation is the first; the second is one constant different and the
physical pass can measure whether the hiccup is real on AVPlayer.

## 5. The tvOS fullscreen surface — what is wrong and what it becomes

### 5.1 What is there today, from the source

`fullscreenSurface` ([`LiveTvView.swift:2300–2486`](../../clients/apple/Sources/LiveTvView.swift))
draws, over the picture with no scrim: a top-left cluster of programme
title (30 pt), channel (`.caption`, 25 pt on tvOS), the technical summary
(`.caption2`, 23 pt at 78 % opacity — one line that runs off the screen on a
busy channel) and the airing line; five text-only buttons top-right in
`TVReadableButtonStyle(prominent: false)` with `.focusEffectDisabled()`
(2346–2371) — app-palette plates (`surfaceHi` / `onBg`, so in a light
appearance they are white plates on video) whose focus cue is the style's
own 4 pt accent stroke, 1.045 lift and glow, on a plate that is otherwise
the same grey as its neighbours; and a default-height white `ProgressView`
with no times (2374–2375). Info opens the phone sheet
`LiveTvTechnicalDetails` (551–612) unchanged: 9 pt labels in a 54 pt
column, `.caption` values.

**Why the buttons stop being navigable — inferred from the source, not yet
seen on a device.** The reveal layer
(`Color.clear.contentShape(Rectangle()).focusable(true)`, 2317–2326) exists
so a hidden overlay still has something focused to receive a press. It is
full-screen and it stays focusable *while the overlay is visible*. SwiftUI's
tvOS focus engine picks the nearest focusable frame in the pressed
direction and does not occlusion-test, so from any button Down, Up, Left
from "Guide" or Right from "More" is a move onto the giant layer rather
than to a neighbour; its adapter is wired with `state: { .fullscreenHidden }`
(2324), which routes every direction to `.reveal` — a no-op when the overlay
is already visible (2493–2496) — and `onMoveCommand` on the focused layer
swallows the press (`PlayerRemoteAdapter.swift:80–83`). Every trapped
press bumps `overlayGeneration`, so the 4 s auto-hide restarts each time;
the trap ends only after 4 s *without* a press, or with Menu (`.hide`), and
never by auto-hide while paused (2453–2457). `onChange(of: overlayVisible)`
(2464–2466) only resets focus when visibility flips. The buttons were never
unfocusable; they were unreachable. The finite player's equivalent layer
sits *in front of* its buttons and is inserted only when the chrome is
gone, then focused (`PlayerView.swift:741–745`, 1264) — that insert-then-
focus order is the pattern to copy.

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

Colour names below are the brand kit's (`ink` `#ededef` · `dim` `#9a9aa3`
· `faint` `#5c5c66` · `raised` `#16161b` · accent `#e5484d`); the Swift
side spells the first four `Palette.onBg` / `.muted` / a new
`Palette.faint` / `.surfaceHi`, or literal values on player chrome.

| Element | Content and source | Type (tvOS pt) |
|---|---|---|
| Top-left status | `LIVE` (accent dot, glows while live; dims to `faint` when paused) · behind-live seconds from `seekableTimeRanges.end − currentTime` | mono 20, chips 40 pt |
| Top-right telemetry | signal bars + `strengthPercent` / `qualityPercent` from `LiveTvStatus.signal` · delivery method from `LiveTvDelivery.videoAction` and `.output.height`/`.videoCodec` · audio from `audioAction` and `.output.audioCodec`/`.audioChannels` · clock | mono 20 · clock 24 |
| Logo tile | 112 pt, `raised` with scanlines; `guideNumber` in mono 40, or `LiveTvGuideChannel.imageUrl` (via `live.guide?.channels`, matched on `id`) when it exists | mono 40/700 |
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

- The reveal layer is `.focusable(!overlayVisible)` — flipped in the same
  transaction as the focus move, in the finite player's insert-then-focus
  order. While the overlay is showing, the button row is the only focusable
  thing on the cover, which is what keeps directions inside it; a
  `.focusSection()` on the row only makes the row's whole frame a target for
  moves *into* it, so it is a convenience for the reveal → row hop, not the
  confinement.
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
scripts/live-tv-hardware --self-host --device <ipv4>  # "real segment duration" reads 2.0 s; startup latency ≈ 12 s on ATSC 3.0, inside the relay's 20 s
```

Plus the measurement that matters and that only a device can give: on the
Apple TV, start a channel and confirm no stall in the first 30 s
(`accessLog` `numberOfStalls == 0`) — that is the hand-off in §8.

### 7.2 Apple — the surface (Swift, tvOS only)

Files: `clients/apple/Sources/LiveTvView.swift` (`fullscreenSurface`, the
Info sheet call site, `LiveTvTechnicalDetails` gets a tvOS body),
`LiveTvPlayerController` (top of `LiveTvView.swift`; a publisher for
`waiting` from `timeControlStatus`, and `behindLive`/`buffered` from the
item), `clients/apple/Tests/LiveTvTests.swift`. The phone surface
is untouched. Acceptance: `xcodebuild test` on the tvOS destination; a
1080p simulator screenshot of each of the four states beside its render;
and on the physical Apple TV, Down from every button reaches a button.

## 8. Hand-off for the physical pass

Paste to a session with the hardware after both PRs merge:

> First, on today's build, so there is a before: open Live TV on the Apple
> TV, start any ATSC 1.0 channel, stay on the inline preview for 30 s and
> say whether it freezes and roughly when; then start it again, go
> fullscreen immediately, and say the same. Then on the build carrying PR
> "Live TV: 1 s segments" and PR "tvOS Live TV fullscreen surface": open
> Live TV, start the same channel, time from Select to first frame with a
> stopwatch; then watch 30 s and note any freeze. Open Info and read the
> PLAYER rows — report "behind live", "buffered", and "stalls". Then press
> Menu to hide the overlay, press Select to reveal it, and from each of the
> five buttons press Down, Left and Right, reporting where focus lands each
> time (it must never leave the button row). Finally hold the picture paused
> for 35 s and confirm the tuner-released message appears. Report the first-
> frame time, the stall count, and the focus table.
