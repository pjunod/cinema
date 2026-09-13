# Live TV start stall and tvOS surface — the implementation plan

**Status:** ready for review (Astra), then build · **Executes:** §3 and §5 of
[LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md](LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md)
· **Measured against:** `main` at `a605d03c` (Apple build 152) ·
**Written:** 2026-09-13

Companion to the diagnosis (why every start freezes and why the tvOS
buttons are unreachable) — this is *exactly what to change, in which
order, and how each step proves itself*. Two independent PRs: **S** (the
owner's segment cadence, Rust) and **A** (the tvOS fullscreen surface,
Swift). They share no files. A builder works milestone by milestone and
stops to flag anything that would need the guide reducers, the lease, the
input contract's routing table, the playback-surface contract, or a new
route — none of this touches them.

The two rulings the diagnosis left open are taken here as **uniform 2 s
segments answered at two listed segments** (§3 there) and **the telemetry
strip separate from the band, as rendered** (§6 there). If Paul rules the
other way on either, §8 says what changes; nothing else in this plan moves.

Line numbers cite `main` at `a605d03c`. Re-verify each anchor at build
time; the files have been moving daily.

## 1. Objective

A Live TV start that never stalls in its first thirty seconds on any
client, at a cost of no more than 3 s to the first frame; and a tvOS
fullscreen surface on which every button is reachable from every other
button, waiting is drawn, and the facts about the stream are on screen.

Acceptance for the whole effort, on the physical Apple TV: first frame
≤ today + 3 s, `AVPlayerItemAccessLogEvent.numberOfStalls == 0` over 30 s
on an ATSC 1.0 channel, and a Down/Left/Right/Up press from each of the
five buttons lands on a button.

## 2. What is settled, and what it rests on

| Fact | Where it is proven |
|---|---|
| `-hls_init_time 1` cuts seven 1 s segments, a 3 s catch-up, then 4 s; `TARGETDURATION` 1 → 3 → 4 | diagnosis §1, measured with the producer's arguments |
| The owner answers the start at one listed segment | `live_tv.rs:4796–4842`; `parsed.segments.is_empty()` at 5758 is the only gate |
| Every client starts ≤ 1 s behind live and stalls 3.6 s in three events | diagnosis §1, modelled from the recording; lower bound |
| Uniform 2 s at two listed segments: 0 stalls, +3.0 s, one segment of margin | diagnosis §3 |
| hlsenc renames a finished segment before rewriting the playlist, so `threshold + 1` unlisted files exist for an instant | diagnosis §3.1, sampled at 2 ms |
| `ScratchInventory.segments` is every final file on disk, not the listed set | `live_tv.rs:5772–5790` |
| The reveal layer stays focusable while the overlay is visible and its adapter answers every direction with `.reveal` | `LiveTvView.swift:2317–2326`, `PlayerRemoteAdapter.swift:77–83`; inferred, not yet seen on a device |

## 3. Contract — exact interfaces

### 3.1 S · the producer arguments and the inventory constants

`crates/plurxd/src/live_tv.rs:123–143` today:

```rust
const LIVE_HLS_OUTPUT_ARGS: [&str; 10] = [
    "-f", "hls",
    "-hls_init_time", "1",
    "-hls_time", "4",
    "-hls_list_size", "6",
    "-hls_delete_threshold", "1",
];
pub(crate) const MAX_PLAYLIST_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_SEGMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SESSION_BYTES: u64 = MAX_SEGMENT_BYTES;
const MAX_LISTED_SEGMENTS: usize = 6;
const MAX_DELETION_LAG_SEGMENTS: usize = 1;
```

becomes:

```rust
/// Uniform two-second segments. Every client's live-sync rule — hls.js at
/// 2 × TARGETDURATION, ExoPlayer at 4 s, AVPlayer at 3 × TARGETDURATION —
/// then sits a whole segment behind the edge from its first frame, which is
/// the margin a live HLS client needs to never wait for a segment to finish.
/// The 24-second window is unchanged (12 × 2 s). `-hls_init_time` is gone on
/// purpose: it bought ~3 s to the first frame and paid 3.6 s of stalls for
/// it, because the cadence jump 1 → 3 → 4 s left the client with nothing to
/// play; and it made TARGETDURATION drift, which RFC 8216 §6.2.1 forbids.
const LIVE_HLS_OUTPUT_ARGS: [&str; 8] = [
    "-f", "hls",
    "-hls_time", "2",
    "-hls_list_size", "12",
    "-hls_delete_threshold", "2",
];
/// How many listed segments the owner waits for before it answers a start.
/// Two: the player starts on the first and has the second in hand, which is
/// one segment of margin whatever the segment length turns out to be on a
/// copy route — the rule is a count, not seconds, for exactly that reason.
const STARTUP_LISTED_SEGMENTS: usize = 2;
const HLS_DELETE_THRESHOLD: usize = 2;
const MAX_LISTED_SEGMENTS: usize = 12;
/// hlsenc renames a finished segment to its final name *before* it rewrites
/// the playlist, so for an instant the scratch holds `threshold + 1` final
/// files the playlist does not list. A budget equal to the threshold turns
/// that instant into `StreamFailed` and ends a healthy session.
const MAX_DELETION_LAG_SEGMENTS: usize = HLS_DELETE_THRESHOLD + 1;
```

The literal `"-hls_delete_threshold", "2"` and `HLS_DELETE_THRESHOLD` must
agree; a test pins it (§3.4). `MAX_SESSION_BYTES` stays: fifteen 2 s
segments on disk is less media than today's eight 4 s ones.

Two hard-coded copies of the old numbers follow the constants by hand:
the message `"live-TV playlist exceeds six segments"` at 5879 (make it
`format!("live-TV playlist exceeds {MAX_LISTED_SEGMENTS} segments")`), and
`crates/plurxd/tests/live_tv_two_node.rs:779`
`const LISTED_SEGMENTS: usize = 6; // MAX_LISTED_SEGMENTS …` → 12. That
harness also waits until `latest - first >= LISTED_SEGMENTS` (line 740):
24 s to fill the window and 24 s for it to slide twelve places, the same
48 s of streaming the 6 × 4 s window costs today; confirm the fixture's
producer keeps up at 2 s rather than assume it.

### 3.2 S · the inventory carries the listed count

`ScratchInventory` (4303–4308) gains one field, filled where the playlist
is parsed (5757):

```rust
struct ScratchInventory {
    playlist: Vec<u8>,
    media_sequence: u64,
    /// How many segments the playlist LISTS. `segments` below is every final
    /// file on disk — listed, plus the deletion lag, plus a segment hlsenc
    /// has renamed but not yet written into the playlist — and is the wrong
    /// thing to gate a start on: in that rename-before-rewrite instant it
    /// counts two while the viewer would be handed a one-segment playlist.
    listed: usize,
    init: Option<(String, u64)>,
    segments: HashMap<u64, (String, u64)>,
}
```

```rust
    Ok(Some(ScratchInventory {
        playlist,
        media_sequence: parsed.media_sequence,
        listed: parsed.segments.len(),
        init,
        segments: final_segments,
    }))
```

### 3.3 S · the publish gate

The producer loop, `live_tv.rs:4796–4842`, today publishes on the first
`Ok(Some(inventory))`. It becomes:

```rust
            match inspect_scratch(&session.directory).await {
                Ok(Some(inventory)) => {
                    let now = tokio::time::Instant::now();
                    // Read before the move below; `inventory` is gone after it.
                    let publishable = startup_publishable(&inventory);
                    {
                        let mut state = session.state.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if inventory.media_sequence > state.media_sequence
                            || state.publication.is_none()
                        {
                            state.last_progress = now;
                        }
                        state.media_sequence = inventory.media_sequence;
                        state.publication = Some(inventory);
                    }
                    if !published && publishable {
                        // … unchanged from today: ensure_session_fence, the
                        // LiveTvProvisional, state.phase = Provisional,
                        // state.startup = Some(Ok(provisional)),
                        // session.changed.notify_waiters(), published = true
                    }
                }
                Ok(None) => {}
                Err(error) => break Err(error),
            }
```

with, next to `inspect_scratch`:

```rust
/// The start is answered when the viewer will have a whole segment in hand
/// after the one they start on. Counted from the playlist, never from the
/// directory (§3.2).
fn startup_publishable(inventory: &ScratchInventory) -> bool {
    inventory.listed >= STARTUP_LISTED_SEGMENTS
}
```

Everything else in that loop stays: `state.publication` keeps being
refreshed from the first segment on, so a playlist served after the answer
is never older than the inventory; `startup_overdue` (4849–4857) keeps
its budget; `provisional_expired`, the idle timeout and
`PRODUCER_PROGRESS_TIMEOUT` are untouched. `last_progress` is still stamped
on the first inventory, so a producer that lists one segment and then stops
is still "stopped advancing" after 30 s rather than "never started".

**The budget the second segment must fit** is not the producer's 30 s but
the relay's: a non-owner ingress gives one POST `START_EXCHANGE_ATTEMPT`
= 20 s (`http/live_tv.rs:25`), and when the last waiter leaves an
unpublished session `StartupWaiter::drop` (4318–4334) cancels it. The
measured ATSC 3.0 first segment after the probesize fix is 8.997 s
([HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md), 2026-09-05);
a first segment of 2 s instead of 1 plus a second one lands at ≈ 12 s.
That is inside 20 s with 8 s to spare, and §5 S3 records the real number.

### 3.4 S · tests that change, tests that appear

All in `crates/plurxd/src/live_tv.rs`'s `mod tests` unless said otherwise.

| Test | Today | After |
|---|---|---|
| `live_hls_publishes_short_startup_segments_before_steady_cadence` (8493–8506) | asserts `init_time 1 / time 4 / list 6` and looks for `-force_key_frames` **in `LIVE_HLS_OUTPUT_ARGS`, which does not contain it** — as written its `value_after` closure panics; find out what the gate actually runs before touching it | renamed `live_hls_cuts_uniform_two_second_segments_and_keeps_a_24_second_window`: asserts `-hls_init_time` is absent, `time 2`, `list 12`, `delete_threshold` equals `HLS_DELETE_THRESHOLD`, and `MAX_DELETION_LAG_SEGMENTS == HLS_DELETE_THRESHOLD + 1`; the keyframe assertion moves to a check on `live_ffmpeg_command` output where the argument actually is (5375–5382) |
| `live_tv_software_hls_argument_baseline_is_stable` (8376) | freezes the full argument vector with the old four pairs (8465–8472) | the vector without `-hls_init_time 1`, with `2 / 12 / 2` |
| `live_playlist_accepts_only_the_closed_numeric_inventory` (8929) | `live_playlist(1, 7)` is refused | `live_playlist(1, MAX_LISTED_SEGMENTS + 1)` is refused and `live_playlist(1, MAX_LISTED_SEGMENTS)` is accepted; the fixture's `#EXTINF:4.000` becomes `2.000` (cosmetic; the parser reads no durations) |
| `live_scratch_inventory_enforces_list_deletion_and_temp_budgets` (8948) | one lag segment ok, two refused | one, two and three lag segments ok (`MAX_DELETION_LAG_SEGMENTS`), four refused; and the returned `inventory.listed == 3` while `inventory.segments.len()` counts the lag too |
| `ten_live_windows_publish_manifest_and_bounded_deletion_lag_atomically` (8979) | `MAX_LISTED_SEGMENTS + 1` files per window | unchanged in shape; passes with 12 because the lag it writes is one |
| **new** `the_start_is_answered_at_the_second_listed_segment` | — | `startup_publishable` is false for `listed: 1, segments: 2 files` (the rename instant), true for `listed: 2`; the assertion names the instant in its message |
| **new** `the_rename_before_rewrite_instant_is_not_a_failure` | — | playlist lists 10..=12, disk holds 8, 9 (the threshold) and 13 (renamed, unlisted): `inspect_scratch` is `Ok(Some)` with `listed == 3`; add 7 and it is `Err` |
| `crates/plurxd/tests/live_tv_two_node.rs` (`--features cluster-integration-tests`) | `LISTED_SEGMENTS = 6` | 12; the assertion at 721 and the wait at 740 follow |

`run_graph_probe` (6652–6700) shares `LIVE_HLS_OUTPUT_ARGS` and encodes
4.25 s of synthetic source; with 2 s segments it publishes two and the
`.ts` check at 6700 holds. Leave it.

### 3.5 A · the controller publishes what the surface draws

`LiveTvPlayerController` (`LiveTvView.swift:7–400`) gains three published
values and one observer. Nothing about starting, stopping, the heartbeat
or the lease changes.

```swift
    /// True while AVPlayer is waiting for media it does not have yet — the
    /// start stall, a mid-stream rebuffer. The surface draws a tile for it;
    /// today it drew nothing and a stall was indistinguishable from Pause.
    @Published private(set) var waiting = false
    /// `seekableTimeRanges.end − currentTime`, sampled with the heartbeat.
    /// nil until the first sample.
    @Published private(set) var behindLiveSeconds: Double?
    /// `loadedTimeRanges` end − currentTime, sampled with the heartbeat.
    @Published private(set) var bufferedSeconds: Double?

    private var timeControlObservation: NSKeyValueObservation?
```

In `attach(_:channel:api:expected:compatibilityRetry:)`, after
`player.play()`:

```swift
        timeControlObservation = player.observe(\.timeControlStatus, options: [.initial, .new]) {
            [weak self] player, _ in
            Task { @MainActor [weak self] in
                guard let self, self.serial == expected else { return }
                self.waiting = player.timeControlStatus == .waitingToPlayAtSpecifiedRate
            }
        }
```

In the heartbeat body, next to `let position = self.player.currentTime().seconds`:

```swift
                    self.sampleLiveEdge(item: item, position: position)
```

```swift
    private func sampleLiveEdge(item: AVPlayerItem, position: Double) {
        func end(_ ranges: [NSValue]) -> Double? {
            ranges.compactMap { CMTimeRangeGetEnd($0.timeRangeValue).seconds }
                .filter(\.isFinite).max()
        }
        guard position.isFinite else { return }
        behindLiveSeconds = end(item.seekableTimeRanges).map { max(0, $0 - position) }
        bufferedSeconds = end(item.loadedTimeRanges).map { max(0, $0 - position) }
    }
```

`detach()` invalidates the observation and resets all three (`waiting =
false`, both seconds `nil`). The Info ledger's PLAYER rows read
`item.accessLog()?.events.last` for `observedBitrate`,
`numberOfDroppedVideoFrames` and `numberOfStalls` at open time — a read,
not a publisher; the panel is not live-updating and says so nowhere,
because a value that is a few seconds old is fine in a panel the viewer
opened on purpose.

`preferredForwardBufferDuration = 12` (156) stays. Nothing here measured
AVPlayer, and the window is 24 s either way.

### 3.6 A · the fullscreen surface

`fullscreenSurface` (`LiveTvView.swift:2300–2486`) is rebuilt on tvOS
only; the iOS branch of the same function (the mute / PiP / Exit row and
the neighbour strip, 2362–2366 and 2376–2399) is untouched. The ZStack
order becomes:

1. `Color.black`, `PlayerSurface(player: live.player, …)` — unchanged.
2. **The reveal layer**, now `.focusable(!overlayVisible)`:

```swift
            Color.clear
                .contentShape(Rectangle())
                .focusable(!overlayVisible)
                .focused($focusedControl, equals: FocusTarget.reveal)
                .accessibilityHidden(true)
                .liveTvRemoteAdapter(.revealSurface,
                                     state: { .fullscreenHidden },
                                     apply: { outcome, _ in applyLiveOutcome(outcome) })
```

   The adapter and its constant state stay exactly as they are; the
   layer is simply not a focus target while the buttons exist. This is
   the one-line fix for the trap.
3. **The waiting tile**, drawn whenever `live.waiting && !live.paused`,
   overlay or no overlay (a stall with the chrome up is still a stall):
   spinner, `Text("Catching up to live")` 28/semibold, and the mono detail
   `"waiting for the next segment · \(behind) behind"` where `behind` is
   `behindLiveSeconds` formatted `%.1f s`, or nothing when nil. Style as
   `PlayerView.swift:836–853` (`.ultraThinMaterial`, radius 12 → 20 here).
4. **The paused glyph**: a 132 pt circle on `Palette.playerChrome.opacity(0.72)`
   with `Image(systemName: "pause.fill")` at 64 pt, drawn while
   `live.paused`.
5. **The top scrim + telemetry strip** and **the bottom scrim + band**,
   both `if overlayVisible`. Fonts are `Font.system(size:)` on tvOS as
   `LiveTvType` already does (489–497); add to that enum:

```swift
    static let surfaceTitle = Font.system(size: 46, weight: .semibold)
    static let surfaceBody = Font.system(size: 24)
    static let surfaceButton = Font.system(size: 24, weight: .semibold)
    static let surfaceMono = Font.system(size: 20, weight: .medium, design: .monospaced)
    static let surfaceEyebrow = Font.system(size: 22, weight: .medium, design: .monospaced)
    static let surfaceHint = Font.system(size: 18, design: .monospaced)
```

   Colours are player chrome, never the app palette: `Color.white`
   (ink), `.white.opacity(0.6)` (dim), `.white.opacity(0.36)` (faint),
   `Palette.playerChrome` at the opacities the renders give (band pills
   0.72, chips 0.62, tiles 0.9), `Palette.accent` for LIVE, the progress
   fill and the focus ring. No new `Palette` members.

**The band** (`VStack(spacing: 28)`, `.padding(.horizontal, 80)`,
`.padding(.bottom, 64)`, `frame(maxWidth: .infinity, alignment: .bottom)`):

| Row | Content | Source |
|---|---|---|
| identity `HStack(spacing: 28)` | 112 pt tile: `guideNumber` in 40/bold mono on `playerChrome.opacity(0.9)` with a 1 pt white 0.1 line · a `VStack(alignment: .leading, spacing: 8)`: eyebrow `"\(guideNumber) · \(guideName)" + (pictureClass.map { " · \($0)" } ?? "")`, title `airing.now?.title ?? live.title ?? "Live television"` one line, sub `[episode, episodeTitle, synopsis].compactMap{$0}.joined(" · ")` one line. **No art tile and no logo image in this PR**: `image_url` is a third-party `https` URL the owner passes through unproxied ([LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) §3 of the guardrails — clients draw the chip instead), and fetching it from the Apple TV would be a new outbound host from a client. The render shows the art slot for the day a proxy exists; the title column takes the width until then | `LiveTvChannel`, `LiveTvAiring` |
| progress `HStack(spacing: 20)` | `liveTvTime(now.start)` mono · a 6 pt `Capsule` track `.white.opacity(0.16)` with an accent fill at `airing.progress` and a 14 pt accent glow · `liveTvTime(now.end)` · `"\(minutesLeft) min left"` · `"Next \(liveTvTime(next.start)) · \(next.title)"`; the whole row is omitted when `airing.now` is nil | `LiveTvAiring` |
| message | the one-line status text the page already computes (`statusText`, 1427–1431), mono 20 dim, only when non-nil — this is where "Paused · the tuner is released after 30 s…" lives | `live.message` |
| buttons `HStack(spacing: 16)` | five `LiveSurfacePill`s (below) in this order: Pause / Play live · Guide · Channels · Info · More; then a `Spacer()` and the hint `MENU hides · PLAY/PAUSE pauses` (or `resumes`) | — |

The five actions keep today's handlers verbatim (2348–2361): Guide sets
`temporaryGuide`, bumps `overlayGeneration`, `requestGuideFocus()`;
Channels `requestChannelFocus(); fullscreen = false`; Pause
`live.togglePause()`; Info `showingInfo = true`; More `showingMore = true`.
No Favorite: `favorite` is a read-only lineup marker (`live_tv.rs:6439`).

**`LiveSurfacePill`** — a `ButtonStyle` in `Theme.swift` beside
`TVReadableButtonStyle`, same focus vocabulary on player chrome:

```swift
#if os(tvOS)
/// The fullscreen live surface's button: the house focus ring, lift and
/// glow from `TVReadableButtonStyle`, on player chrome rather than the app
/// palette, because it sits on the picture in every appearance.
struct LiveSurfacePillStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View { Body(configuration: configuration) }
    struct Body: View {
        let configuration: Configuration
        @Environment(\.isFocused) private var isFocused
        var body: some View {
            configuration.label
                .labelStyle(.titleAndIcon)
                .font(LiveTvType.surfaceButton)
                .foregroundStyle(.white)
                .padding(.leading, 22).padding(.trailing, 26)
                .frame(height: 56)
                .background(Palette.playerChrome.opacity(isFocused ? 0.92 : 0.72),
                            in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                .overlay {
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .stroke(isFocused ? Palette.accent : .white.opacity(0.10),
                                lineWidth: isFocused ? 4 : 1)
                }
                .scaleEffect(isFocused ? 1.045 : (configuration.isPressed ? 0.98 : 1))
                .shadow(color: Palette.accent.opacity(isFocused ? 0.35 : 0), radius: 14, y: 10)
                .animation(.easeOut(duration: 0.14), value: isFocused)
        }
    }
}
#endif
```

Labels are `Label("Pause", systemImage: "pause.fill")`, `"Guide"` /
`"rectangle.grid.3x2"`, `"Channels"` / `"list.bullet"`, `"Info"` /
`"info.circle"`, `"More"` / `"ellipsis"`. `.focusEffectDisabled()` stays
on the row — the style draws the whole focus state, as today.

**The telemetry strip** (`HStack`, `.padding(.horizontal, 80)`,
`.padding(.top, 48)`, `frame(maxWidth: .infinity, alignment: .top)`), all
mono 20 chips on `playerChrome.opacity(0.62)`:

| Chip | Text | Source |
|---|---|---|
| LIVE | accent dot (glow while playing, `.white.opacity(0.36)` while paused) + `LIVE` | `live.paused` |
| behind | `"%.1f s behind live"`, or `"paused · m:ss"` from a pause clock the controller starts in `togglePause()` | `behindLiveSeconds`; a new `pausedAt: Date?` |
| signal | five bars from `strengthPercent / 20` rounded, `"\(strength)% signal · \(quality)% quality"`; omitted until `live.status?.signal` exists | `LiveTvStatus.signal` |
| method | `videoAction == "copy" ? "direct \(output.videoCodec)" : "transcode \(output.height)p \(output.videoCodec)"` | `LiveTvDelivery` (`status?.delivery ?? live.delivery`) |
| audio | `"\(output.audioCodec) \(channelsLabel) \(audioAction == "copy" ? "copied" : "transcoded")"`, `channelsLabel` = `2 → "stereo"`, `6 → "5.1"`, else `"\(n) ch"` | `LiveTvDelivery.output` |
| clock | `Date.now.formatted(date: .omitted, time: .shortened)` off the existing 30 s `tick` | — |

Chips are omitted, never blank, when their source is nil.

### 3.7 A · focus

```swift
        .onAppear { focusedControl = overlayVisible ? .play : .reveal }
        .onChange(of: overlayVisible) { _, visible in
            if visible {
                focusedControl = .play
            } else {
                // The finite player's order (PlayerView.swift:1264): the
                // layer becomes focusable in this transaction; focus lands on
                // it next turn, so there is never a "no focusable views"
                // moment for the engine to resolve on its own.
                focusedControl = nil
                Task { @MainActor in
                    await Task.yield()
                    if !overlayVisible { focusedControl = .reveal }
                }
            }
        }
        .onChange(of: focusedControl) { _, target in
            if overlayVisible { overlayGeneration &+= 1 }
            // Belt and braces for the trap the diagnosis found: a focusable
            // layer behind the buttons is gone (§3.6 item 2), and if the
            // engine ever lands there while the chrome is up anyway, put it
            // back on the button a viewer reaches for.
            if overlayVisible, target == .reveal { focusedControl = .play }
        }
```

`FocusTarget` keeps its cases; `.play` is the default now, not `.guide`.
The `.task(id: overlayGeneration)` auto-hide (2452–2459) is unchanged.
`applyLiveOutcome` (2491–2530) is unchanged. `LiveTvInputRouting` and
`PlayerRemoteAdapter.swift` are unchanged — `scripts/player-input-fence`
would trip on a `MoveCommandDirection` anywhere else, and nothing here
needs one.

### 3.8 A · Info becomes a ledger

`showingInfo` keeps presenting a `.sheet` (1363–1370), but on tvOS its
content is a new `LiveTvStreamInfoPanel` rather than
`LiveTvTechnicalDetails` (which stays as the phone's view, unchanged):

- 1240 × (content) pt, `Palette.playerChrome.opacity(0.96)`, radius 22,
  padding 40/44; header `"Stream info"` 34/semibold and a Close pill in
  `LiveSurfacePillStyle` that takes focus on appear the way
  `PlaybackStatsView.requestInitialFocus` does (2906–2911).
- A two-column `LazyVGrid` / `HStack` of sections: PROGRAMME · CHANNEL on
  the left, DELIVERY · SIGNAL · PLAYER on the right. Section eyebrows are
  mono 17/bold in `Palette.accent`, tracking 0.1 em; rows are
  `HStack(alignment: .firstTextBaseline, spacing: 20)` of a 150 pt mono 18
  label in faint and a 22 pt value in white, wrapping.
- Rows and sources: PROGRAMME title / episode / airing (`start–end ·
  N min left`) / next / synopsis / aired (`originalAirDate`, `filters`
  joined) — all `LiveTvProgramme`; CHANNEL channel (`guideNumber ·
  guideName · pictureClass · "favorite on the tuner"` when `favorite`) /
  source (`sourceFormatDescription`) / observed (`sourceFormat.observedAt`
  formatted as today at 563–567); DELIVERY method (`playbackMethod`) /
  video (`videoDescription`, plus `" · \(status.encoder)"` when encoding and
  `encoder != "pending"`, plus `" on \(status.ownerNodeId)"`) / audio
  (`audioDescription`) / stream (`"HLS · \(packaging.uppercased()) · 2 s
  segments"`); SIGNAL three meters (label, `%`, a 6 pt accent bar) from
  `signal.strengthPercent / qualityPercent / symbolQualityPercent`, each
  omitted when nil; PLAYER live (`behindLiveSeconds`, `bufferedSeconds`) /
  rate (`observedBitrate` as Mb/s, `numberOfDroppedVideoFrames`,
  `numberOfStalls` from the last access-log event) / session (`"owner
  \(ownerNodeId) · \(minutes) min"` from the attach time).
- Every row is omitted when its value is nil. The panel never scrolls at
  these sizes; if a synopsis is long it is limited to three lines.

`liveInputState` (1410–1416) already maps `showingInfo` to `.streamInfo`,
whose ten-foot row is `focusPanel / activate / closePanel`; nothing to add.

### 3.9 A · tests

In `clients/apple/Tests/LiveTvTests.swift`, following its existing
patterns (source-grep tests like `testTheOldRestartBarrierIsGoneFromTheSource`
at 278 and controller tests with `LiveTvPlayerController.testing(...)`):

| Test | Asserts |
|---|---|
| `testTheRevealLayerIsNotFocusableWhileTheOverlayIsVisible` | `LiveTvView.swift` contains `.focusable(!overlayVisible)` and does **not** contain `.focusable(true)` inside `fullscreenSurface` (grep between the `private var fullscreenSurface` line and the next `private func`) — a pin, so the trap cannot come back as a refactor |
| `testFullscreenFocusDefaultsToPauseAndReturnsThereFromTheRevealLayer` | the `onChange(of: focusedControl)` body contains `target == .reveal { focusedControl = .play }` and `.onAppear { focusedControl = overlayVisible ? .play : .reveal }` |
| `testWaitingFollowsTheTimeControlStatusAndClearsOnDetach` | with `testing(...)`, an attached controller whose `player` is driven to `.waitingToPlayAtSpecifiedRate` publishes `waiting == true`; `stop()` publishes `false` and nils both seconds |
| `testTheTelemetryStripOmitsWhatItDoesNotKnow` | the chip builder (make it a pure `static func liveSurfaceChips(status:delivery:behind:paused:) -> [String]`) returns no signal chip for a nil signal, `"direct mpeg2video"` for a copy route, `"transcode 720p h264"` for an encode route, `"ac3 5.1 copied"` for six copied channels |
| `testTheStreamInfoPanelReadsEveryFieldTheModelsCarry` | `LiveTvStreamInfoPanel.rows(programme:channel:status:delivery:player:)` (a pure function the view renders) returns the documented labels in order and drops nil rows |
| `testFiveActionsAndNoFavorite` | the fullscreen button row names exactly `Pause`/`Play live`, `Guide`, `Channels`, `Info`, `More` and the word `Favorite` does not occur in `LiveTvView.swift` |

Plus `make apple-build-bump` in the same PR — the `mobile release version`
job compares the claimed build against `main` *at merge time*, so bump
after the final rebase, not before — and a
`docs/apple-builds/<n>-live-tv-surface.md` note as every Apple PR carries.

## 4. Non-goals — guardrails for the builder

- **No change to `LiveTvInputRouting`, the contract fixture, or
  `PlayerRemoteAdapter.swift`.** The ten-foot rows are right; the trap
  was a focusable view, not a routing answer. A builder who needs a
  routing change has misread the surface — stop and say so.
- **No channel strip on tvOS, no Up/Down channel change on the hidden
  overlay.** The 2026-09-02 ruling stands: a direction on a hidden overlay
  reveals it. Guide and Channels are the two ways off this channel.
- **No Favorite button, no favourite route.** Nothing on the owner writes
  a favourite.
- **No client live-sync changes.** hls.js `liveSyncDurationCount: 2`,
  ExoPlayer `4_000` ms and AVPlayer's default all land ≥ 4 s behind a
  2 s-segment edge on their own. (hls.js's `liveMaxLatencyDurationCount: 4`
  becomes 8 s of tolerance rather than 16; still twice the sync distance.
  Leave it, note it in the PR.)
- **No phone changes.** The iOS branch of `fullscreenSurface`, the phone
  picture, caption and `LiveTvTechnicalDetails` are untouched.
- **No new persisted state and no new `Palette` members.** Player chrome is
  literal on purpose: it must not follow the app theme.
- **No third-party image fetch from the client.** Programme and channel
  `image_url`s stay unrendered on tvOS until the owner proxies them; the
  art slot in the render is a placeholder for that milestone, not a task
  here.
- **No gating.** Nothing here is behind a flag; the Developer tab is the
  only switch there is and this needs none.
- **Do not fold S into A or A into S.** Different languages, different
  gates, different reviewers' attention; and S is the one that fixes the
  pause on every client, so it must not wait for Swift.

## 5. Milestones

Each PR: proper commits, the fast lane only, opened as a draft (`WIP:`),
one adversarial review, findings fixed, the full suite once, then merged
by its author (Paul's lifecycle, 2026-09-07). Both PRs edit the top of
`STATUS.md` and one `docs/README.md` row is already in place (#296), so
rebase before un-drafting.

### S1 · the arguments and the constants (`live_tv.rs`)

§3.1 in full, including the 5879 message and the two-node harness
constant. Acceptance:

```bash
cargo test -p plurxd live_hls_                       # the renamed cadence test + the frozen baseline, green
cargo test -p plurxd live_playlist_accepts            # 12 accepted, 13 refused
grep -n '"6"\|six segments' crates/plurxd/src/live_tv.rs   # nothing left that means the old window
```

### S2 · the listed count and the gate (`live_tv.rs`)

§3.2 and §3.3, with the two new tests and the updated lag-budget test from
§3.4. Acceptance:

```bash
cargo test -p plurxd scratch_inventory                # lag 1..3 ok, 4 refused; listed counted from the playlist
cargo test -p plurxd the_start_is_answered            # false at one listed segment with two on disk
cargo test -p plurxd the_rename_before_rewrite         # the instant is Ok(Some), not StreamFailed
```

### S3 · the harness and the hardware (evidence, not code)

```bash
cargo test -p plurxd --features cluster-integration-tests --test live_tv_two_node   # 12-deep window, relay still serves every listed segment
scripts/live-tv-hardware --self-host --device <ipv4> --channel <ATSC 1.0>          # "real segment duration" 2.0 s; startup latency recorded
scripts/live-tv-hardware --self-host --device <ipv4> --channel <ATSC 3.0>          # startup latency ≈ 12 s, inside the relay's 20 s attempt
```

Record both startup numbers in
[HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md)'s evidence
table. If the ATSC 3.0 number exceeds 16 s, stop: the relay budget needs a
ruling before this merges, not a bigger constant.

### A1 · the controller (`LiveTvView.swift` top, `LiveTv.swift` untouched)

§3.5: `waiting`, `behindLiveSeconds`, `bufferedSeconds`, `pausedAt`, the
observer, the sampler, the detach reset, and the pure chip builder.
Acceptance: `testWaitingFollowsTheTimeControlStatusAndClearsOnDetach` and
`testTheTelemetryStripOmitsWhatItDoesNotKnow` green under
`xcodebuild test` on the tvOS simulator destination.

### A2 · the surface and the focus (`LiveTvView.swift`, `Theme.swift`)

§3.6 and §3.7. Acceptance: the two source-pin tests green; a 1080p tvOS
simulator screenshot of the controls-revealed state beside
`docs/mockups/live-tv/surface-controls.png`, and of the paused and
catching-up states beside theirs (`xcrun simctl io booted screenshot`;
drive the waiting state by pausing the fixture producer, or by
`player.pause()` on the item with `waiting` forced in a debug build — say
which).

### A3 · the Info ledger (`LiveTvView.swift`)

§3.8. Acceptance: `testTheStreamInfoPanelReadsEveryFieldTheModelsCarry`
green; a screenshot beside `surface-info.png`; Close takes focus on open
and Menu closes the sheet (`.streamInfo × back → closePanel` already).

### A4 · the physical pass (hand-off, §7)

Both PRs merged and the Apple build installed. Acceptance is the hand-off
report: first frame ≤ today + 3 s, `numberOfStalls == 0` over 30 s, and a
focus table with no entry that says "nothing" or "the picture".

## 6. What the reviewer should attack

Written for Astra; each is a place this plan could be wrong.

1. **The rename-before-rewrite instant.** The `+ 1` rests on a 2 ms
   sampling of hlsenc in this session and on reading `hls_write_packet`:
   `hls_rename_temp_file` before `hls_window`. Does the *deletion* side —
   `hls_delete_old_segments` after the rewrite — ever leave `threshold + 2`
   finals on disk for an instant, which would make the budget `+ 2`?
2. **The gate reads `listed` from a parse of the same bytes it serves.**
   Is there any path by which `state.publication` is refreshed from a
   *later* inventory than the one that flipped `published`, such that the
   first playlist a viewer receives lists fewer segments than the gate
   counted? (The move at 4814 happens every tick; the answer notifies on
   the tick that flips.)
3. **`startup_overdue` and the second segment.** With one segment listed
   and the second slow (a copy route with a long GOP), does the producer's
   30 s feeding budget or the relay's 20 s attempt fire first, and does the
   viewer get `startup_timeout` or a silent cancel? The plan claims
   12 s on the worst measured channel; find the channel shape that breaks
   it (a 4 s+ GOP HEVC mux would list its second segment at ≈ 9 + 8 s).
4. **`StreamFailed` on `MAX_LISTED_SEGMENTS`.** `parse_playlist_bytes`
   refuses more than the constant. hlsenc lists exactly `hls_list_size`;
   is there a flag combination or an FFmpeg version in the fleet where the
   playlist briefly lists `list_size + 1`?
5. **The two-node harness time.** 12 sequences at 2 s each before the
   window slides — does the fixture producer in `live_tv_two_node.rs:254`
   run realtime, and does the test's own timeout (516–517: 20 s per
   request) still cover the wait at 740?
6. **`.focusable(!overlayVisible)` flipping in the same transaction as the
   focus move.** The plan copies the finite player's yield-then-focus
   order; is there a first-appear case (`onAppear` sets `.play` before the
   buttons exist in the hierarchy) where focus lands nowhere and Menu is
   the only way out — the exact failure the finite player's comment at
   `PlayerView.swift:726–731` describes?
7. **`.focusEffectDisabled()` on the row plus a custom `isFocused`
   style.** On tvOS 26 with the glass button treatment, does
   `@Environment(\.isFocused)` inside a `ButtonStyle` body still report the
   focused button (the `TVReadableButtonStyle` comment at `Theme.swift:
   111–114` says this was a real problem)?
8. **The KVO observer's lifetime.** `player.observe` retains the closure;
   the plan invalidates on `detach()`. Is there a path — `resumeIfRecent`,
   the codec-retry `watch(channel, compatibilityRetry: true)` — that calls
   `attach` twice without a `detach` between, leaking an observer whose
   `expected` serial is stale but which still writes `waiting`?
9. **`accessLog()` on a live item.** Is `events.last` the current event
   for a single-variant live playlist, and is `numberOfStalls` counted the
   way the physical pass will read it (cumulative per event)?
10. **The message row.** `statusText` returns nil for the steady-state
    string only; is there any other message the controller publishes while
    playing that would now sit permanently in the band (the compatibility
    retry sentence, the stale-lineup sentence)?
11. **Nothing in §3.6 relies on `live.guide` being present.** Confirm every
    guide-derived element degrades to the channel's own fields — a
    Live TV with `guide_source = off` must still draw a complete band.
12. **The waiting tile and the playback-surface contract.** `LiveTvView.swift`
    is outside the surface fence's scanned set and Live TV has no reducer;
    is that still the right boundary once the live surface draws a wait,
    or does §4 of the contract want `media_waiting` here too? The plan
    says no — Live TV has its own message channel by design — and wants
    that answer challenged.

## 7. Hand-off for the physical pass

Paste to a session with the hardware, after A merges and the build is on
the Apple TV:

> First, on the build BEFORE these changes, so there is a before: open
> Live TV on the Apple TV, start any ATSC 1.0 channel, stay on the inline
> preview for 30 s and say whether it freezes and roughly when; then start
> it again, go fullscreen immediately, and say the same. Then on the build
> carrying "Live TV: 2 s segments" and "tvOS Live TV fullscreen surface":
> open Live TV, start the same channel, time from Select to first frame
> with a stopwatch; watch 30 s and note any freeze. Open Info and read the
> PLAYER rows — report "behind live", "buffered", and "stalls". Press Menu
> to hide the overlay, press Select to reveal it, and from each of the
> five buttons press Down, Left, Right and Up, reporting where focus lands
> each time (it must never leave the button row). Hold the picture paused
> for 35 s and confirm the tuner-released message appears in the band.
> Finally, on an ATSC 3.0 channel, time Select to first frame. Report the
> two first-frame times, the stall count, and the focus table.

## 8. If Paul rules the other way

- **`init 1 / time 2` at ≥ 3 s listed instead of uniform 2 s at two
  segments.** §3.1 keeps `-hls_init_time 1`; `STARTUP_LISTED_SEGMENTS`
  becomes a seconds rule — `ScratchInventory` carries `listed_seconds`
  parsed from `#EXTINF` (the parser reads no durations today, so that is
  new parsing with its own refusal rules) and `startup_publishable` is
  `listed_seconds >= 3.0`; the web's `liveSyncDurationCount: 2` becomes
  `liveSyncDuration: 3` (seconds) at `index.html:16517`, or hls.js starts
  2 s behind a 1 s edge and stalls 0.1 s at the cadence change. Everything
  in A is unchanged.
- **Telemetry inside the band instead of a strip.** §3.6's strip chips
  become a fourth band row above the buttons; `scrim_top` goes; the clock
  moves to the band's right edge. Focus and everything else unchanged.

## 9. What this plan does not know

- Whether the trap has actually been seen on the device; the fix is right
  either way, and A4 is where the before-state gets recorded.
- The real ATSC 3.0 second-segment time (S3 measures it).
- How `numberOfStalls` behaves across a `replaceCurrentItem` on this tvOS;
  A4 reads it once, after a clean start, which is the case that matters.
