# Live TV start stall and tvOS surface — the implementation plan

**Status:** v2 after Astra's review (17 findings folded, §0) — **ready to
build; Sol builds it** · **Executes:** §3 and §5 of
[LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md](LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md)
· **Against:** `main` at `a605d03c` (Apple build 152) · **Lane:**
`effort/live-tv-start-stall` (create it from current `main`) ·
**Written:** 2026-09-13 · **Revised:** 2026-09-13

Companion to the diagnosis (why every start freezes and why the tvOS
buttons are unreachable) — this is *exactly what to change, in which
order, and how each step proves itself*. Three PRs that share no files:
**S** (the owner's segment cadence and the two watchdogs it exposed, Rust),
**N** (one Android constant, Kotlin), **A** (the tvOS fullscreen surface,
Swift). A builder works milestone by milestone and stops to flag anything
that would need the guide reducers, the lease, the input contract's
routing table, or a new route — none of this touches them. The one
contract boundary this plan does cross on purpose is named in §3.7.

The two rulings the diagnosis left open are taken here as **uniform 1 s
segments answered at two listed segments** (Paul's original intent for
`init_time` was a start no slower than the tuner, and this keeps it) and
**the telemetry strip separate from the band, as rendered**. §8 says what
changes if either goes the other way.

Line numbers cite `main` at `a605d03c` and are re-verified at build time;
function names are the durable anchors.

**How to work (Sol).** Read the diagnosis first — it says *why* every line
below exists. Then §10 for the mechanics of this repository, then build
§5 in order: S1 → S2 → S3, N1 beside S3, A1 → A2 → A3. Each milestone is
one task PR into the lane, opened `WIP:`, reviewed once adversarially,
findings implemented, the smallest focused regression recorded in the PR
body; the full suites run once, at the lane's promotion to `main`
(`AGENTS.md`, "Large efforts"). Nothing physical can be done from a
session without the hardware — S3's device runs, N1's device check and
A3's device check are hand-offs (§7) that Paul gives to a GPT session;
write the exact prompt into the PR body when you reach one and carry on
with the next milestone rather than waiting. A second Astra pass on this
v2 may still arrive; its findings fold as amendments to the milestone
they touch, not as a reason to stop.

**Standing instruction.** If a step seems to require any of these, stop
and flag it rather than doing it: changing `LiveTvInputRouting`, the
input-contract fixture, or `PlayerRemoteAdapter.swift`; changing a signed
internal wire shape (`LiveTvStartRequest`, `LiveTvStartRequestV2`,
`LiveTvActivateRequest`, `LiveTvStopRequest`); changing
`CAPABILITY_IDLE_TIMEOUT`, `PROVISIONAL_TIMEOUT`, `STARTUP_TIMEOUT`,
`STARTUP_FEEDING_TIMEOUT`, `PRODUCER_PROGRESS_TIMEOUT` or the relay's
`START_EXCHANGE_ATTEMPT` / `PUBLIC_START_DEADLINE`; raising
`MAX_SESSION_BYTES`; touching the phone surface; adding a route; fetching
a third-party image from a client; putting Live TV under the
playback-surface reducer (§3.7 is a ruling, not a task); or gating any of
this behind a flag.

## 0. What the review found, and what this revision does about it

Astra's review of the v1 plan (tip `da8f008d`) is folded in below. Each
row says where.

| # | Finding | Disposition |
|---|---|---|
| 1 | **Blocker.** `last_progress` advances only when `media_sequence` advances; a 24-entry window takes 24 s (encode) or 48 s (2 s GOP) to slide, and `PRODUCER_PROGRESS_TIMEOUT` is 30 s. Every copy stream would start and then die. | Accepted, confirmed at `live_tv.rs:4804–4808` and 4875. Progress is now the newest **listed** sequence, §3.3. Test in §3.5. |
| 2 | One listed segment is now `published == false`, so the "stopped advancing" check never applies and `startup_overdue` fires at ≈ 28 s (`wait_for_startup` adds 2 s) with a message that says no segment exists. | Accepted. `startup_overdue` takes the listed count and says "one segment, waiting for a second", §3.4. |
| 3 | **Blocker.** Two `TARGETDURATION`s do not give every client a spare segment: Android asks for a 4 s live offset and Media3 snaps to a segment boundary, so two 6 s copy segments leave it starting on the second with zero margin. | Accepted. PR **N** drops Android's absolute `setTargetOffsetMs(4_000)` so Media3 uses its own 3 × `TARGETDURATION` default, clamped by the existing 8 s maximum; every client then starts on the first listed segment, §3.9. A growing `TARGETDURATION` on a variable-GOP mux is recorded as a limit, §9. |
| 4 | Model results presented as playback guarantees; 2.77 s is not a tuner floor (0.36 s on another channel); 1 s segments absorb less publish jitter than 2 s. | Accepted. Claims are re-worded as model results with acceptance under named conditions (§1, §5); the 2 s cadence stays a resilience fallback (§8) and the jitter case is measured on the candidate build (§5 S3). |
| 5 | **Blocker.** The 8.997 s figure is the H.264-encode, 4 s-segment path, not HEVC copy; `scripts/live-tv-hardware` sends no playback capabilities, self-hosts a local owner (no relay), and pins `STARTUP_BUDGET = 15` and `LISTED_SEGMENTS = 6`; the relay retries a 20 s attempt once and exhaustion is `owner_unavailable`. | Accepted. S3 is rewritten: the script gains a copy-capable start, the two constants, a non-owner ingress mode, and the outcome table for a start that outlives one attempt (§5 S3). |
| 6 | The disk-budget arithmetic is wrong for long GOPs and high bitrates. | Accepted. The bound is stated as an envelope, `files × TARGETDURATION × bitrate`, with the numbers (§3.1). |
| 7 | **Blocker.** `run_graph_probe` passes two output URLs; the second is unbounded; it times out at 20 s under the new arguments and cannot prove anything. | Accepted (it is advisory — `ffmpeg_graph` on the Developer card never blocks a start, `http/system.rs:2576–2593` — but a row that always says "timed out" is worse than none). Rebuilt on the production command builder, §3.6. |
| 8 | The proposed tests do not prove the production fix: the gate test is on a helper; the "atomic" window test writes everything before inspecting; the two-node walk fetches only `names.last()`; `cargo test live_hls_` does not select the baseline test; the table still said 12 twice. | Accepted. §3.5 adds a producer-loop test on a scratch directory and a full-walk assertion in the harness; acceptance commands name tests exactly (§5); the 12s are gone. |
| 9 | `.waitingToPlayAtSpecifiedRate` includes `evaluatingBufferingRate`; the plan has no reason handling and no debounce, so normal starts flash the tile; the surface contract says Live TV joins its fixture "when overlay work resumes". | Accepted. Waiting is `reasonForWaitingToPlay == .toMinimizeStalls`, debounced 350 ms — the contract's own `buffering` semantics — and the fixture boundary is stated as a ruling (§3.7). |
| 10 | `accessLog().events.last` is one uninterrupted period, not the interval; `numberOfStalls` can be −1. | Accepted. Sum over events, treat negatives as unknown, label the panel a snapshot (§3.8, §5 A4). |
| 11 | The buffered sampler sums disjoint ranges; "behind live" is distance from the seekable edge; the "1 s segments" literal is false on a 2 s GOP or an older server. | Accepted. Range containing the playhead; renamed "behind the edge"; the literal is gone (§3.8). |
| 12 | Focus: the yielded task does not prove a destination exists; no panel-ownership guards; the temporary-guide layer was missing from the ZStack list and its panel would cover the band; the Info sheet does not receive the root adapter's commands. | Accepted. §3.10 and §3.11: the guide layer is listed, the band hides under it, the guards check the cover and every panel, the sheet's dismissal is the native one and says so. |
| 13 | `frame(maxWidth: .infinity, alignment: .bottom)` does not anchor to the bottom; "never scrolls" is unsupported. | Accepted. Explicit `maxHeight: .infinity` anchoring; every value `lineLimit(2)`, fixed panel height, long-content screenshot (§3.10, §3.12). |
| 14 | A pause expiring while the controls are hidden leaves a black picture and a message nobody sees; lineup-refresh copy can sit in the band indefinitely. | Accepted. A stop that ends the session while the cover is up returns to the browser; the band draws a `surfaceMessage` the controller owns, not `message` (§3.8, §3.10). |
| 15 | `airing.next` can be nil with `now` present; signal fields are independently optional; the "Favorite" grep catches "Favorites"; `testing(...)` cannot drive `timeControlStatus`. | Accepted. Each in §3.10/§3.13; the waiting decision becomes a pure function with a seam. |
| 16 | Acceptance happened after merge; the before-state had no first-frame time; web and Android never run. | Accepted. Candidate builds are qualified on the hardware **before** un-draft (§5), the before-state records a stopwatch time, and S's acceptance covers all three clients. |
| 17 | Confirmed: encode cadence, single-segment gate, final-file inventory, reveal routing, hlsenc rename-before-rewrite (`+ 1`, not `+ 2`), harness 180 s walk, KVO fencing, style precedent; stale numeric anchors for `hideControls` / `requestInitialFocus`. | Kept. Anchors are now by name. |

## 1. Objective

A Live TV start that does not stall in its first minute on any client in
the model, and measurably does not on the candidate build (§5), at a cost
of one segment (1 s on an encode route) to the first frame — the tuner,
the probe and one segment stay the floor, and the segment is the only one
of those the server chooses; and a tvOS fullscreen surface on which every
button is reachable from every other button, waiting is drawn for the
right reason, and the facts about the stream are on screen.

Acceptance for the whole effort, on candidate builds before either PR is
un-drafted (§5): first frame ≤ before-state + 1 s by stopwatch on the same
channel; the sum of `numberOfStalls` over every access-log event with a
known count is 0 across 60 s on an ATSC 1.0 encode route and on the ATSC
3.0 copy channel, on Apple TV, web and Android; and a Down / Left / Right /
Up press from each of the five buttons lands on a button.

## 2. What is settled, and what it rests on

| Fact | Where it is proven |
|---|---|
| `-hls_init_time 1` cuts seven 1 s segments, a 3 s catch-up, then 4 s; `TARGETDURATION` 1 → 3 → 4 | diagnosis §1; reproduced independently by the review (finding 17) |
| The owner answers the start at one listed segment | `live_tv.rs:4796–4842`; `parsed.segments.is_empty()` at 5758 |
| In the model, every client starts ≤ 1 s behind live and stalls 3.6 s in three events; no real player was run | diagnosis §1 |
| In the model, uniform 1 s at two listed segments: 0 stalls, +1.0 s, ≈ 2 s behind the edge; at one listed segment, one 0.5 s hiccup | diagnosis §3 |
| The tuner's first byte was 2.77 s on one channel and 0.36 s on another; the probe is `-probesize 524288 -analyzeduration 1000000` | [HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md) rows for 2026-09-05 |
| `last_progress` advances only on a media-sequence advance | `live_tv.rs:4804–4808`, watchdog at 4875 (finding 1) |
| `ScratchInventory.segments` is every final file on disk, not the listed set | `live_tv.rs:5772–5790` |
| hlsenc renames a finished segment before rewriting the playlist; deletion precedes the rewrite; the transient is `threshold + 1`, never `+ 2` | review finding 17 against FFmpeg 8.0 `hlsenc.c`; diagnosis §3.1 sampling |
| `run_graph_probe` supplies two output URLs and the second is unbounded | `live_tv.rs:6673`; reproduced by the review under the new arguments (20 s timeout) |
| The reveal layer stays focusable while the overlay is visible and its adapter answers every direction with `.reveal` | `LiveTvView.swift:2317–2326`, `PlayerRemoteAdapter.swift:77–83`; inferred, not yet seen on a device |
| Media3 with an explicit target offset snaps to the segment boundary at or before it | review finding 3, `HlsMediaSource` start-position logic; check at the build's dependency version |

### 2.1 Where the code is — the seams this plan touches

| Seam | File and anchor | PR |
|---|---|---|
| Producer arguments and inventory constants | `crates/plurxd/src/live_tv.rs` `LIVE_HLS_OUTPUT_ARGS` (127), `MAX_LISTED_SEGMENTS` / `MAX_DELETION_LAG_SEGMENTS` (142–143) | S |
| Playlist parser | `parse_playlist_bytes` (5799–5904), `ParsedPlaylist` (5793) | S |
| Scratch inventory | `inspect_scratch` (5679–5791), `ScratchInventory` (4303) | S |
| Producer loop: publish and progress | the `match inspect_scratch(...)` at 4796–4846; the watchdog at 4875 | S |
| Startup budget | `startup_overdue` (4929), callers at 4850 and `wait_for_startup` 4364 | S |
| Session state | `LiveTvSessionState` (`last_progress`, `media_sequence`, `publication`) | S |
| Graph probe | `run_graph_probe` free function (6652), `LiveTvTranscodePlan::new`, `live_ffmpeg_command` (5330) | S |
| Owner tests | `mod tests` in `live_tv.rs` (8376, 8493, 8929, 8948, 8979; fixture-tuner tests ~7617) | S |
| Two-node harness | `crates/plurxd/tests/live_tv_two_node.rs` (`LISTED_SEGMENTS` 779, walk 692–760) | S |
| Hardware script | `scripts/live-tv-hardware` (constants 84–92, the start at 646–660) | S |
| Android live configuration | `clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt:194–199` | N |
| Apple controller | `LiveTvPlayerController` at the top of `clients/apple/Sources/LiveTvView.swift` (`attach` 148, heartbeat 169, `togglePause` 348, `detach` 386, `load` 66) | A |
| Apple fullscreen surface | `fullscreenSurface` (2300–2486), `applyLiveOutcome` (2491), `liveInputState` (1410), the Info sheet (1363) | A |
| Apple type and style | `LiveTvType` (489–497); `TVReadableButtonStyle` in `Theme.swift` (115) | A |
| Apple tests | `clients/apple/Tests/LiveTvTests.swift` (source-pin pattern at 278; `testing(...)` factory at `LiveTvView.swift:12`) | A |
| Status and docs | `STATUS.md` top entry, `docs/apple-builds/<n>-live-tv-surface.md`, the evidence table in `docs/features/HDHOMERUN-LIVE-TV-STATUS.md` | all |

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
/// Uniform one-second segments, for the whole session. The short first
/// segment `-hls_init_time 1` used to cut is what keeps a start no slower
/// than the tuner; what it also did was let the cadence grow to 4 s once the
/// list filled, which left every client — attached 1 s behind the edge —
/// waiting a whole segment at each jump (3.6 s of stalls in the model, the
/// cadence itself measured). Keeping the cadence at 1 s removes the jump;
/// TARGETDURATION is 1 on an encode route and never changes (RFC 8216
/// §6.2.1). Encode routes already force a keyframe every second, so the
/// encoder does no extra work; copy routes cut at the broadcast's own
/// keyframes, so their segments are the source GOP and TARGETDURATION is
/// that GOP rounded up. The window is 24 entries — 24 s on an encode route,
/// 24 GOPs on a copy route (§3.1 says what that costs).
const LIVE_HLS_OUTPUT_ARGS: [&str; 8] = [
    "-f", "hls",
    "-hls_time", "1",
    "-hls_list_size", "24",
    "-hls_delete_threshold", "4",
];
/// The owner answers a start once the playlist lists this many segments AND
/// this many target durations of media. Two of each: every client starts on
/// the first listed segment (§3.9 makes that true of Android too) with the
/// second in hand — one segment of margin — and on a copy route whose
/// segments are longer than the cadence the seconds half still means "two
/// target durations", which is what the margin actually is.
const STARTUP_LISTED_SEGMENTS: usize = 2;
const STARTUP_LISTED_TARGET_DURATIONS: f64 = 2.0;
const HLS_DELETE_THRESHOLD: usize = 4;
const MAX_LISTED_SEGMENTS: usize = 24;
/// hlsenc renames a finished segment to its final name *before* it rewrites
/// the playlist, so for an instant the scratch holds `threshold + 1` final
/// files the playlist does not list. A budget equal to the threshold turns
/// that instant into `StreamFailed` and ends a healthy session. Deletion
/// precedes the rewrite, so it is never `+ 2`.
const MAX_DELETION_LAG_SEGMENTS: usize = HLS_DELETE_THRESHOLD + 1;
```

The literal `"-hls_delete_threshold", "4"` and `HLS_DELETE_THRESHOLD` must
agree; a test pins it (§3.5).

**The disk envelope.** `MAX_SESSION_BYTES` (128 MiB) stays, and what it
admits is `29 files × TARGETDURATION × bitrate` plus one temporary file:
at 1 s segments ≤ 37 Mbit/s; at 2 s ≤ 18 Mbit/s; at 4 s ≤ 9 Mbit/s; a
12 s GOP fits only under 3 Mbit/s. The measured ATSC 3.0 channel is
3.3 Mbit/s, so a 2 s or 4 s GOP there is 23–46 MiB. Today's `6 / 1` window
has the same shape (`8 × 4 s × bitrate`: 33 Mbit/s at most), so this is
not a new limit, but it is a limit the plan now states instead of waving
at, and S3 records the ATSC 3.0 GOP so the envelope is checked against a
real mux rather than assumed. If that GOP is over 4 s, bounding the window
by seconds rather than entries is the follow-up (§9), not a bigger byte
cap.

The 250 ms `inspect_scratch` walk covers ~30 entries instead of ~8; the
playlist is ≈ 1.2 KB against the 64 KiB cap.

Two hard-coded copies of the old numbers follow the constants by hand:
the message `"live-TV playlist exceeds six segments"` at 5879 (make it
`format!("live-TV playlist exceeds {MAX_LISTED_SEGMENTS} segments")`), and
`crates/plurxd/tests/live_tv_two_node.rs:779` `const LISTED_SEGMENTS: usize
= 6` → 24 (the walk at 740 then needs 48 s of streaming, inside its 180 s
deadline; the 20 s at 516–517 is per request).

### 3.2 S · the inventory carries what the playlist lists

`parse_playlist_bytes` (5799–5904) validates the header, the absence of
`ENDLIST`, `#EXT-X-MEDIA-SEQUENCE`, the `#EXT-X-MAP` line, `EXTINF` ↔
resource binding and the segment names; it reads no durations. It gains
two tags, read as strictly as the media sequence — hlsenc always writes
both, and a playlist without them is not one this owner produced:

```rust
struct ParsedPlaylist {
    media_sequence: u64,
    /// `#EXT-X-TARGETDURATION`, required by RFC 8216 §4.3.3.1. Refused when
    /// missing, repeated, zero, or not an integer.
    target_duration: u64,
    /// The sum of every `#EXTINF` duration in the list — the media a viewer
    /// handed this playlist could play before reaching the edge.
    listed_seconds: f64,
    init: Option<String>,
    segments: Vec<u64>,
}
```

The `#EXTINF:` branch (5844–5850) parses the text before the first comma
as `f64` (hlsenc writes `#EXTINF:1.000000,` with an empty title; a title
after the comma is ignored, never refused) and refuses a value that is not
finite, is negative, or exceeds 60 — the same shape of refusal as the
segment-name and sequence checks around it. Astra's question of whether
hlsenc ever writes a duration outside that on a real mux is a §6 item
and an S3 observation; the refusal bounds are deliberately loose.

`ScratchInventory` (4303–4308) carries the three facts the gate and the
watchdog need, filled at 5781–5790:

```rust
struct ScratchInventory {
    playlist: Vec<u8>,
    media_sequence: u64,
    /// How many segments the playlist LISTS, and how much media that is.
    /// `segments` below is every final file on disk — listed, plus the
    /// deletion lag, plus a segment hlsenc has renamed but not yet written
    /// into the playlist — and is the wrong thing to gate a start on: in that
    /// rename-before-rewrite instant it counts two while the viewer would be
    /// handed a one-segment playlist.
    listed: usize,
    listed_seconds: f64,
    target_duration: u64,
    init: Option<(String, u64)>,
    segments: HashMap<u64, (String, u64)>,
}

impl ScratchInventory {
    /// The newest sequence number the playlist lists. This, not
    /// `media_sequence`, is what "the producer advanced" means: the media
    /// sequence only moves once the window is full and slides, which with a
    /// 24-entry window is 24 s on an encode route and 24 GOPs on a copy route
    /// — longer than the 30 s progress watchdog on the second. (Review
    /// finding 1: the v1 plan would have started every copy stream and then
    /// ended it at 30 s.)
    fn newest_listed(&self) -> Option<u64> {
        self.listed
            .checked_sub(1)
            .map(|offset| self.media_sequence + offset as u64)
    }
}
```

### 3.3 S · progress is the newest listed segment

`LiveTvSessionState` gains `newest_listed: Option<u64>` beside
`media_sequence`, and the producer loop (4796–4842) becomes:

```rust
            match inspect_scratch(&session.directory).await {
                Ok(Some(inventory)) => {
                    let now = tokio::time::Instant::now();
                    // Read before the move below; `inventory` is gone after it.
                    let publishable = startup_publishable(&inventory);
                    let newest = inventory.newest_listed();
                    {
                        let mut state = session.state.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        // Progress is a new segment becoming fetchable, not the
                        // window sliding. The first inventory counts.
                        if newest > state.newest_listed || state.publication.is_none() {
                            state.last_progress = now;
                        }
                        state.newest_listed = newest;
                        state.media_sequence = inventory.media_sequence;
                        state.publication = Some(inventory);
                    }
                    if !published && publishable {
                        // … unchanged: ensure_session_fence, the LiveTvProvisional,
                        // state.phase = Provisional, state.startup = Some(Ok(..)),
                        // session.changed.notify_waiters(), published = true
                    }
                }
                Ok(None) => {}
                Err(error) => break Err(error),
            }
```

with, beside `inspect_scratch`:

```rust
/// The start is answered when the viewer will have a whole segment in hand
/// after the one they start on — and, on a copy route whose segments are
/// longer than the cadence, a whole TARGETDURATION in hand. Counted from the
/// playlist, never from the directory (§3.2).
fn startup_publishable(inventory: &ScratchInventory) -> bool {
    inventory.listed >= STARTUP_LISTED_SEGMENTS
        && inventory.listed_seconds
            >= STARTUP_LISTED_TARGET_DURATIONS * inventory.target_duration as f64
}
```

Nothing else in the loop moves: `provisional_expired`, the 45 s idle
timeout and `PRODUCER_PROGRESS_TIMEOUT` keep their meanings, and the
watchdog at 4875 now measures what it was always meant to — a producer
that has stopped listing segments — instead of a window that has not yet
filled. `media_sequence` stays on the state because the resource handlers
read it.

### 3.4 S · the startup budget names what it is waiting for

With the gate, a session that has listed one segment is still
`published == false`, so the "stopped advancing" branch (which requires
`published`) does not apply to it; `startup_overdue` (4929–4943) does,
and `wait_for_startup` asks it with `now + 2 s` (4364–4370), so the
waiter's verdict lands at ≈ 28 s after `session.started`. That is the
right budget — a tuner that lists one segment and then nothing for 28 s
is not going to list a second — but the message would say "no complete
live segment", which is false. `startup_overdue` takes the listed count:

```rust
fn startup_overdue(
    started: tokio::time::Instant,
    now: tokio::time::Instant,
    tuner_bytes: u64,
    listed: usize,
) -> Option<String> {
    if tuner_bytes == 0 {
        return (now.duration_since(started) >= STARTUP_TIMEOUT).then(|| {
            "the tuner sent no data before the start budget ran out; the channel may have \
             no signal on this device".to_owned()
        });
    }
    (now.duration_since(started) >= STARTUP_FEEDING_TIMEOUT).then(|| match listed {
        0 => format!("the tuner sent {tuner_bytes} bytes but no complete live segment in time"),
        n => format!(
            "the tuner sent {tuner_bytes} bytes and {n} live segment(s), but not the \
             {STARTUP_LISTED_SEGMENTS} a player needs to start without stalling"
        ),
    })
}
```

Both call sites (4850, 4364) pass `state.publication.as_ref().map_or(0,
|p| p.listed)`. The wire code stays `startup_timeout`; only the sentence
changes.

### 3.5 S · tests that change, tests that appear

All in `crates/plurxd/src/live_tv.rs`'s `mod tests` unless said otherwise.
Names are exact, because `cargo test <substring>` selects by them.

| Test | Today | After |
|---|---|---|
| `live_hls_publishes_short_startup_segments_before_steady_cadence` (8493–8506) | asserts `init_time 1 / time 4 / list 6` and looks for `-force_key_frames` **in `LIVE_HLS_OUTPUT_ARGS`, which does not contain it** — as written its closure panics; the builder finds out what the gate actually runs before touching it | renamed `live_hls_cuts_uniform_one_second_segments_and_keeps_a_24_entry_window`: `-hls_init_time` absent, `time 1`, `list 24`, `delete_threshold` == `HLS_DELETE_THRESHOLD`, `MAX_DELETION_LAG_SEGMENTS == HLS_DELETE_THRESHOLD + 1`; the keyframe assertion moves to `live_ffmpeg_command` output (5375–5382) |
| `live_tv_software_hls_argument_baseline_is_stable` (8376) | freezes the full argument vector; the review reports its frozen keyframe ordering already differs from the builder — confirm whether it is red on `main` first | the vector without `-hls_init_time 1`, with `1 / 24 / 4`, in the order the builder actually emits |
| `live_playlist_accepts_only_the_closed_numeric_inventory` (8929) | `live_playlist(1, 7)` refused; the fixture writes no `TARGETDURATION` | the fixture writes `#EXT-X-TARGETDURATION:1` and `#EXTINF:1.000000,`; `live_playlist(1, MAX_LISTED_SEGMENTS + 1)` refused, `live_playlist(1, MAX_LISTED_SEGMENTS)` accepted with `target_duration == 1`, `listed_seconds == 24.0`; the refused list (8935–8941) gains no-`TARGETDURATION`, `TARGETDURATION:0`, `#EXTINF:nan,`, and `#EXTINF:1.0,title` is *accepted* |
| `live_scratch_inventory_enforces_list_deletion_and_temp_budgets` (8948) | one lag ok, two refused | one to five ok, six refused; `inventory.listed == 3` while `segments.len()` counts the lag; `newest_listed() == Some(12)` |
| `ten_live_windows_publish_manifest_and_bounded_deletion_lag_atomically` (8979) | writes all files, then inspects | unchanged; it never claimed to test the instant |
| **new** `the_start_is_answered_at_the_second_listed_segment` | — | `startup_publishable`: false for `listed 1 / 1.0 s / TD 1` with two files on disk; true for `listed 2 / 2.0 s / TD 1`; false for `listed 2 / 4.0 s / TD 3`; true for `listed 3 / 7.0 s / TD 3` |
| **new** `the_producer_answers_only_once_two_segments_are_listed` | — | drives the real producer loop against a scratch directory the test writes into (the same shape as the fixture-tuner tests around 7617, which already `printf` a playlist): one segment listed → `wait_for_startup` still pending after two ticks and `state.startup.is_none()`; a second listed → the provisional is delivered. Revert the gate and this fails; that is the point (finding 8) |
| **new** `progress_is_the_newest_listed_segment_not_the_window_sliding` | — | with a 24-entry window and one new listed segment every tick, `last_progress` advances on every tick for 40 simulated seconds and the watchdog never fires; with the listed set frozen, it fires at 30 s |
| **new** `the_rename_before_rewrite_instant_is_not_a_failure` | — | playlist lists 10..=12; disk holds 6, 7, 8, 9 (the threshold) and 13 (renamed, unlisted): `Ok(Some)` with `listed == 3`; add 5 → `Err` |
| **new** `startup_overdue_says_how_many_segments_it_has` | — | the two messages at 28 s for `listed 0` and `listed 1` |
| `crates/plurxd/tests/live_tv_two_node.rs` (`--features cluster-integration-tests`) | `LISTED_SEGMENTS = 6`; the walk fetches `names.last()` and tolerates non-200 | 24; **the first playlist the client receives lists ≥ 2 segments**; the walk fetches **every** listed name and asserts 200 on each; the walk asserts the listed count reaches 24 and the media sequence then advances |

### 3.6 S · the graph probe is rebuilt on the production command

`run_graph_probe` (6652–6700) builds its own argument list, then does
`command.arg(segments).arg(&playlist)` — two output URLs, `-t 4.25` bound
to the first, the second unbounded with defaults, so the process runs
until the 20 s timeout (reproduced by the review). It is advisory (the
`ffmpeg_graph` row on the Developer card; enabling never waits on it) but
an advisory row that always reads "timed out" is worse than none. It
becomes a call into the same builder the producer uses:

```rust
    let plan = LiveTvTranscodePlan::new(system, test_encode_delivery(height), Some(encoder), threads)?;
    let mut command = live_ffmpeg_command(system, &plan, directory)?;   // production args, one output
    // Replace the tuner pipe with the synthetic source and bound it.
    …  `-f lavfi -i testsrc2…`, `-f lavfi -i sine…`, `-t 4.25`
```

(`live_ffmpeg_command` reads `pipe:0`; the builder gains an input override
for the probe, or the probe constructs the output half from
`LIVE_HLS_OUTPUT_ARGS` plus `-hls_segment_filename` — the builder chooses,
and the test below pins the result.) The probe then asserts the playlist
it reads: `TARGETDURATION` 1, at least three `#EXTINF:1.0` entries, no
`ENDLIST` (the production flags include `omit_endlist`). New test
`the_graph_probe_publishes_the_production_cadence`.

### 3.7 A · waiting is drawn for the right reason, and the boundary is named

`LiveTvPlayerController` (`LiveTvView.swift:7–400`) gains:

```swift
    /// True while AVPlayer has stopped to buffer mid-stream — a stall, not the
    /// buffering-rate evaluation every start goes through. Apple says not to
    /// draw waiting UI for `.evaluatingBufferingRate`, and the surface contract's
    /// `buffering` class debounces 350 ms for the same reason: a wait shorter
    /// than that is not something a viewer should be told about.
    @Published private(set) var waiting = false
    private var timeControlObservation: NSKeyValueObservation?
    private var waitingDebounce: Task<Void, Never>?

    /// Pure, so a test can drive it without an AVPlayer.
    static func waitingDecision(status: AVPlayer.TimeControlStatus,
                                reason: AVPlayer.WaitingReason?) -> Bool {
        status == .waitingToPlayAtSpecifiedRate && reason == .toMinimizeStalls
    }
```

In `attach`, after `player.play()`:

```swift
        timeControlObservation = player.observe(\.timeControlStatus, options: [.initial, .new]) {
            [weak self] player, _ in
            let decision = Self.waitingDecision(status: player.timeControlStatus,
                                                reason: player.reasonForWaitingToPlay)
            Task { @MainActor [weak self] in
                guard let self, self.serial == expected else { return }
                self.waitingDebounce?.cancel()
                if decision {
                    self.waitingDebounce = Task { @MainActor [weak self] in
                        try? await Task.sleep(nanoseconds: 350_000_000)
                        guard !Task.isCancelled, let self, self.serial == expected else { return }
                        self.waiting = true
                    }
                } else {
                    self.waiting = false
                }
            }
        }
```

`detach()` invalidates the observation, cancels the debounce and resets
`waiting`. The 350 ms is `tests/playback/playback-surface-contract.json`'s
`buffering` debounce; the plan transcribes it rather than reading the
fixture, because Live TV is not in the fixture.

**The boundary, stated for a ruling.** The surface contract's scope says
Live TV joins its fixture "when overlay work resumes"
(`docs/clients/PLAYBACK-SURFACE-CONTRACT.md` ~780), and `LiveTvView.swift`
is outside `scripts/playback-surface-fence`'s scanned set. This plan draws
one wait tile with the contract's reason and debounce semantics and does
**not** put Live TV under the reducer, because Live TV has no faults to
classify — its failures already end the session through `LiveTvFailure`
and the browser's status line — and a reducer with one class is a
tautology. If Paul wants Live TV in the fixture, this tile is the first
row of that work, not a competitor to it. Astra's finding 9 asked for this
to be explicit; it is.

### 3.8 A · the controller publishes what the surface draws

```swift
    /// `seekableTimeRanges` end − `currentTime`: distance from the edge the
    /// playlist offers, which is what a viewer can act on. It is NOT latency
    /// from the broadcast — the encoder, the segment and the tuner sit in
    /// front of the edge — and the surface never calls it that.
    @Published private(set) var behindEdgeSeconds: Double?
    /// The loaded range that contains the playhead, measured forward from it.
    /// A later disjoint range is not buffered media for the viewer's purposes.
    @Published private(set) var bufferedSeconds: Double?
    @Published private(set) var pausedAt: Date?
    /// What the surface's message row draws. Owned here so lineup copy from
    /// `load()` — "N channels", "cached lineup" — never reaches the picture.
    @Published private(set) var surfaceMessage: String?
```

`sampleLiveEdge(item:position:)` runs with the heartbeat and after any
seek: `behindEdgeSeconds = max seekable end − position` (clamped ≥ 0);
`bufferedSeconds = end − position` for the loaded range whose start ≤
position < end, else 0; both `nil` when `position` is not finite or the
ranges are empty. `surfaceMessage` is set by `attach` (to `nil`),
`togglePause` (the tuner-release sentence), the compatibility retry, and
the heartbeat's failures; `load()` keeps writing `message` for the
browser and never touches it. The PLAYER rows of the Info panel read
`item.accessLog()?.events` once when the panel opens: stalls are the sum of
`numberOfStalls` over events with a non-negative count (a negative count
is "unknown" and is shown as such), dropped frames the same, bitrate the
last event's `observedBitrate`; the panel header says "as of h:mm:ss" and
Close is the only action.

`preferredForwardBufferDuration = 12` (156) stays; nothing here measured
AVPlayer.

### 3.9 N · Android starts on the first listed segment

`LiveTvPlayer.kt:194–199` today asks Media3 for a 4 s live offset with an
8 s maximum. Media3 honours the offset by starting at the segment boundary
at or before `edge − 4 s`, so on a two-segment playlist of 6 s copy
segments it starts on the *second* segment with no margin (review finding
3). Drop the target and keep the ceiling:

```kotlin
.setLiveConfiguration(MediaItem.LiveConfiguration.Builder().setMaxOffsetMs(8_000).build())
```

With no explicit target Media3 uses the playlist's own hold-back — three
`TARGETDURATION`s — clamped by the 8 s maximum, so on a fresh two-segment
playlist it starts on the first segment whatever the GOP, and settles
3 × `TARGETDURATION` behind on an encode route (3 s). `setBufferDurationsMs(4_000,
12_000, 1_000, 2_000)` (166) is unchanged: a 2 s re-buffer floor is two
1 s segments, which is exactly what the gate hands it. One Kotlin test:
`liveConfigurationLeavesTheTargetOffsetToThePlaylist` asserts
`targetOffsetMs == C.TIME_UNSET` and `maxOffsetMs == 8_000` on the built
item. The web's `liveSyncDurationCount: 2` and `liveMaxLatencyDurationCount:
4` (`index.html:16517`) are multiples of `TARGETDURATION` and already start
on the first listed segment; on a 1 s cadence the max-latency catch-up
becomes 4 s with a 2 s jump — noted, unchanged, and the first suspect if
the web is seen catching up on a LAN.

### 3.10 A · the fullscreen surface

`fullscreenSurface` (`LiveTvView.swift:2300–2486`) is rebuilt on tvOS
only; the iOS branch (2362–2366, 2376–2399) is untouched. The ZStack, in
order:

1. `Color.black`, `PlayerSurface(player: live.player, …)` — unchanged.
2. **The reveal layer**, `.focusable(!overlayVisible && !temporaryGuide)`,
   otherwise exactly today's (2317–2326): the adapter and its constant
   `.fullscreenHidden` state stay; the layer is simply not a focus target
   while anything else is.
3. **The waiting tile**, whenever `live.waiting && !live.paused`: spinner,
   "Catching up to live" 28/semibold, mono detail `"\(behind) behind the
   edge"` from `behindEdgeSeconds` formatted `%.1f s`, nothing when nil;
   `.ultraThinMaterial`, radius 20, as the finite player's wait tile.
4. **The paused glyph**, while `live.paused`: a 132 pt circle on
   `Palette.playerChrome.opacity(0.72)` with `pause.fill` at 64 pt.
5. **The top scrim + telemetry strip** and **the bottom scrim + band**,
   both `if overlayVisible && !temporaryGuide`. Geometry is explicit —
   review finding 13 — each wrapped as
   `.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)` /
   `.bottom` so the ZStack cannot centre them.
6. **The temporary-guide panel** (today's 2405–2435), unchanged and last,
   so it is above everything; while it is up the band is not drawn and the
   reveal layer is not focusable, so the panel's `Close` and grid are the
   only focus targets, as today.

Fonts on tvOS are `Font.system(size:)` via `LiveTvType` (489–497), which
gains `surfaceTitle` 46/semibold, `surfaceBody` 24, `surfaceButton`
24/semibold, `surfaceMono` 20/medium monospaced, `surfaceEyebrow`
22/medium monospaced, `surfaceHint` 18 monospaced. Colours are player
chrome, never the app palette: `.white`, `.white.opacity(0.6)`,
`.white.opacity(0.36)`, `Palette.playerChrome` at 0.72 / 0.62 / 0.9,
`Palette.accent` for LIVE, the progress fill and the focus ring. No new
`Palette` members.

**The band** (`VStack(spacing: 28)`, `.padding(.horizontal, 80)`,
`.padding(.bottom, 64)`):

| Row | Content | Source and absence rule |
|---|---|---|
| identity | 112 pt tile with `guideNumber` in 40/bold mono · eyebrow `guideNumber · guideName` + ` · pictureClass` when present · title `airing.now?.title ?? live.title ?? "Live television"` one line · sub `[episode, episodeTitle, synopsis].compactMap { $0 }.joined(" · ")`, one line, omitted when empty. **No art tile and no logo image**: `image_url` is a third-party https URL the owner passes through unproxied ([LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) guardrail 3); the render's art slot waits for a proxy | `LiveTvChannel`, `LiveTvAiring`; the whole row draws from the channel alone when there is no guide |
| progress | `liveTvTime(now.start)` · a 6 pt capsule track with an accent fill at `airing.progress` · `liveTvTime(now.end)` · `N min left` · `Next HH:MM · title` **only when `airing.next` is non-nil** (finding 15) | omitted entirely when `airing.now` is nil |
| message | `live.surfaceMessage`, mono 20 dim | omitted when nil |
| buttons | Pause / Play live · Guide · Channels · Info · More as `LiveSurfacePillStyle` pills (§3.11), a `Spacer()`, the hint `MENU hides · PLAY/PAUSE pauses` / `resumes` | — |

The five actions keep today's handlers verbatim (2348–2361). No Favorite:
`favorite` is a read-only lineup marker (`live_tv.rs:6439`).

**The telemetry strip** (`HStack`, `.padding(.horizontal, 80)`,
`.padding(.top, 48)`), mono 20 chips on `playerChrome.opacity(0.62)`,
built by a pure `static func liveSurfaceChips(status:delivery:behindEdge:paused:)
-> [LiveSurfaceChip]` so it is testable:

| Chip | Text | Absence rule |
|---|---|---|
| LIVE | accent dot (glow while playing, faint while paused) + `LIVE` | always |
| behind | `"%.1f s behind the edge"`, or `"paused · m:ss"` from `pausedAt` | omitted until the first sample |
| signal | bars from `strengthPercent / 20`, then `"\(strength)% signal"`, then `" · \(quality)% quality"` — **each part only when its field is non-nil** (finding 15) | omitted when `signal` is nil or both fields are |
| method | `videoAction == "copy" ? "direct \(output.videoCodec)" : "transcode \(output.height)p \(output.videoCodec)"` | omitted when no delivery |
| audio | `"\(output.audioCodec) \(channelsLabel) \(audioAction == "copy" ? "copied" : "transcoded")"` | omitted when no delivery |
| clock | `Date.now.formatted(date: .omitted, time: .shortened)` off the 30 s `tick` | always |

### 3.11 A · focus

```swift
        .onAppear { focusedControl = overlayVisible ? .play : .reveal }
        .onChange(of: overlayVisible) { _, visible in
            if visible {
                focusedControl = .play
            } else {
                // The finite player's order (`hideControls`): the layer becomes
                // focusable in this transaction; focus lands on it next turn.
                focusedControl = nil
                Task { @MainActor in
                    await Task.yield()
                    // Only if this cover is still the one on screen and no
                    // panel has taken focus in the meantime (finding 12).
                    guard fullscreen, !overlayVisible, !temporaryGuide,
                          !showingInfo, !showingMore, !showingLayout, detail == nil
                    else { return }
                    focusedControl = .reveal
                }
            }
        }
        .onChange(of: focusedControl) { _, target in
            if overlayVisible { overlayGeneration &+= 1 }
            if overlayVisible, target == .reveal { focusedControl = .play }
        }
        .onChange(of: temporaryGuide) { _, up in
            if !up, fullscreen { focusedControl = overlayVisible ? .play : .reveal }
        }
        // A session that ended while the cover was up — the 30 s pause
        // expiry, a stream failure — leaves a black picture and a message the
        // hidden band cannot show. Leave the cover; the browser's status line
        // has the sentence. `busy` guards the mid-tune detach `watch()` does
        // (the comment at 2476–2482), which must not dismiss anything.
        .onChange(of: live.playing) { _, playing in
            if !playing && !live.busy { fullscreen = false; temporaryGuide = false }
        }
```

`FocusTarget` keeps its cases; `.play` is the default, not `.guide`. The
`.task(id: overlayGeneration)` auto-hide (2452–2459) and `applyLiveOutcome`
(2491–2530) are unchanged. `LiveTvInputRouting` and
`PlayerRemoteAdapter.swift` are unchanged. The Info sheet is a `.sheet`;
tvOS dismisses a sheet on Menu natively and the root adapter's
`onExitCommand` does not fire while it is presented, so `.streamInfo ×
back → closePanel` is the table's description of that native dismissal,
not a code path this plan adds — finding 12, agreed and stated.

`LiveSurfacePillStyle` (in `Theme.swift`, beside `TVReadableButtonStyle`):
the same focus vocabulary — 4 pt accent stroke, 1.045 lift, accent glow —
on `Palette.playerChrome` 0.72 / 0.92, white label, 56 pt tall, radius 14,
`Label` with `pause.fill` / `rectangle.grid.3x2` / `list.bullet` /
`info.circle` / `ellipsis`. `.focusEffectDisabled()` stays on the row as
today.

### 3.12 A · Info becomes a ledger

`showingInfo` keeps presenting a `.sheet` (1363–1370); on tvOS its content
is a new `LiveTvStreamInfoPanel`; `LiveTvTechnicalDetails` stays the
phone's view. 1240 × 984 pt fixed, `Palette.playerChrome.opacity(0.96)`,
radius 22, padding 40/44; header "Stream info · as of h:mm:ss" and a Close
pill that takes focus on appear the way `PlaybackStatsView.requestInitialFocus`
does. Two columns: PROGRAMME · CHANNEL left, DELIVERY · SIGNAL · PLAYER
right. Rows are a 150 pt mono 18 label and a 22 pt value with
`lineLimit(2)` — every value, not only the synopsis (finding 13) — and the
panel height is fixed, so a long title truncates rather than grows; the
A3 acceptance includes a screenshot with the longest strings the models
allow. Rows and sources are those of the v1 plan with these corrections:
DELIVERY's stream row is `"HLS · \(packaging.uppercased())"` with no
segment-length literal; PLAYER's live row reads `behindEdgeSeconds` /
`bufferedSeconds` and is labelled "behind the edge · buffered"; its rate
row uses the summed access-log counters (§3.8) and prints "unknown" for a
negative one. Every row is omitted when its value is nil. Rows are built
by a pure `LiveTvStreamInfoPanel.rows(programme:channel:status:delivery:player:)`.

### 3.13 A · tests

In `clients/apple/Tests/LiveTvTests.swift`, following its patterns:

| Test | Asserts |
|---|---|
| `testTheRevealLayerIsNotFocusableWhileTheOverlayOrTheGuideIsVisible` | between `private var fullscreenSurface` and the next `private func`, the source contains `.focusable(!overlayVisible && !temporaryGuide)` and no `.focusable(true)` |
| `testFullscreenFocusDefaultsToPauseAndReturnsThereFromTheRevealLayer` | the guard `target == .reveal { focusedControl = .play }` and the `.onAppear` default are present; the yielded restore checks `fullscreen` and every panel flag |
| `testWaitingIsOnlyDrawnForAStallAndNeverForBufferingRateEvaluation` | `waitingDecision` is true only for `(.waitingToPlayAtSpecifiedRate, .toMinimizeStalls)`; false for `.evaluatingBufferingRate`, `.noItemToPlay`, `nil`, and for `.playing` / `.paused` with any reason |
| `testWaitingIsDebouncedAndClearsOnDetach` | with `testing(...)`, driving the controller's observation seam (`applyTimeControl(status:reason:)`, internal) publishes `waiting == true` only after 350 ms of sustained stall, never for a 200 ms one; `stop()` clears it |
| `testTheTelemetryStripOmitsWhatItDoesNotKnow` | `liveSurfaceChips`: no signal chip for nil signal; `"92% signal"` alone when quality is nil; `"direct mpeg2video"` for a copy route; `"transcode 720p h264"` for encode; `"ac3 5.1 copied"`; no behind chip before the first sample |
| `testTheProgressRowSurvivesAMissingNextProgramme` | the row builder with `now` present and `next` nil yields no "Next" text and no crash |
| `testTheStreamInfoPanelReadsEveryFieldTheModelsCarryAndSumsTheAccessLog` | `rows(...)` returns the documented labels in order, drops nil rows, sums stalls over events, prints "unknown" for −1 |
| `testTheSurfaceMessageIsNotTheLineupMessage` | after `load()` writes "N channels", `surfaceMessage` is nil; after `togglePause()` it is the release sentence; after `attach` it is nil again |
| `testFiveFullscreenActionsAndNoFavorite` | the fullscreen button row (the same source slice as the first test) names exactly `Pause`/`Play live`, `Guide`, `Channels`, `Info`, `More`; the browse toolbar's `Favorites` toggle at 1584 is out of scope |

The observation seam: `attach` routes the KVO callback through an internal
`applyTimeControl(status:reason:)` that tests call directly; `testing(...)`
stubs requests, not AVPlayer, and that is the honest limit (finding 15).
Source-string tests pin the shape; they do not prove the focus engine, and
§5 A3 does that on a device.

Plus `make apple-build-bump` in the same PR, after the final rebase (the
`mobile release version` job compares against `main` at merge time), and
a `docs/apple-builds/<n>-live-tv-surface.md` note.

## 4. Non-goals — guardrails for the builder

- **No change to `LiveTvInputRouting`, the contract fixture, or
  `PlayerRemoteAdapter.swift`.** The trap was a focusable view, not a
  routing answer.
- **No channel strip on tvOS, no Up/Down channel change on the hidden
  overlay** (the 2026-09-02 ruling). Guide and Channels are the two ways
  off this channel.
- **No Favorite button, no favourite route.**
- **No client live-sync changes beyond §3.9** — one Android line, and
  nothing on the web or Apple.
- **No phone changes.** The iOS branch of `fullscreenSurface`, the phone
  picture, caption and `LiveTvTechnicalDetails` are untouched.
- **No third-party image fetch from the client.** Programme and channel
  `image_url`s stay unrendered on tvOS until the owner proxies them.
- **No new persisted state and no new `Palette` members.**
- **No gating.** Nothing here is behind a flag.
- **Live TV does not join the surface fixture in this lane** (§3.7) —
  unless Paul rules that it should, in which case §3.7 is the first row.
- **Do not fold S, N and A together.** Different languages, different
  gates; S fixes the pause on every client and must not wait for Swift.

## 5. Milestones

Each PR: proper commits, the fast lane only, opened as a draft (`WIP:`),
one adversarial review, findings fixed, **the candidate build qualified
on the hardware (below)**, the full suite once, then merged by its author.
All three edit the top of `STATUS.md`; rebase before un-drafting.

### S1 · the arguments, the constants, the probe (`live_tv.rs`)

§3.1 and §3.6. Acceptance:

```bash
cargo test -p plurxd live_hls_cuts_uniform_one_second_segments_and_keeps_a_24_entry_window
cargo test -p plurxd live_tv_software_hls_argument_baseline_is_stable   # by its full name — `live_hls_` does not select it
cargo test -p plurxd live_playlist_accepts_only_the_closed_numeric_inventory
cargo test -p plurxd the_graph_probe_publishes_the_production_cadence
grep -n '"6"\|six segments\|hls_init_time' crates/plurxd/src/live_tv.rs   # nothing left that means the old window
```

### S2 · the listed count, the gate, the watchdog, the message (`live_tv.rs`)

§3.2–§3.5. Acceptance:

```bash
cargo test -p plurxd the_start_is_answered_at_the_second_listed_segment
cargo test -p plurxd the_producer_answers_only_once_two_segments_are_listed   # fails with the gate reverted
cargo test -p plurxd progress_is_the_newest_listed_segment_not_the_window_sliding
cargo test -p plurxd the_rename_before_rewrite_instant_is_not_a_failure
cargo test -p plurxd startup_overdue_says_how_many_segments_it_has
cargo test -p plurxd live_scratch_inventory_enforces_list_deletion_and_temp_budgets
```

### S3 · the harness, the hardware script, the candidate build (evidence)

`scripts/live-tv-hardware` changes in this PR: `LISTED_SEGMENTS = 24`,
`STARTUP_BUDGET` follows the relay (`START_EXCHANGE_ATTEMPT` 20 s, two
attempts under `PUBLIC_START_DEADLINE` 40 s) rather than `STARTUP_TIMEOUT`;
a `--copy` flag that sends the start with a playback envelope advertising
HEVC/AC-3 so the owner chooses a copy route (today's bodiless POST is the
H.264 compatibility profile, which is why every historical number is an
encode number — finding 5); a `--via <ingress>` mode that sends the public
start through a named non-owner node so the relay's attempt and cancel
paths are the ones exercised; and it records segment duration, `TARGETDURATION`,
time to the first listed segment, time to the answer, and the observed
bitrate.

```bash
cargo test -p plurxd --features cluster-integration-tests --test live_tv_two_node   # 24-entry window; first playlist lists ≥ 2; every listed name serves 200
scripts/live-tv-hardware --self-host --device <ipv4> --channel <ATSC 1.0>            # segment 1.0 s; answer ≈ before-state + 1 s
scripts/live-tv-hardware --self-host --device <ipv4> --channel <ATSC 3.0> --copy     # segment = that mux's GOP; answer time; TARGETDURATION
scripts/live-tv-hardware --via <nuc> --device <ipv4> --channel <ATSC 3.0> --copy     # the relayed copy start: answered on attempt 1 or 2, or its exact refusal code
```

The outcome table the last command must fill, for a start that outlives
one 20 s attempt: whether attempt 2 joins the same owner session (same
request id) or the owner cancelled it when the first waiter left
(`StartupWaiter::drop`), what code the viewer receives
(`owner_unavailable` vs `startup_timeout`), and the total time to an
answer. If the ATSC 3.0 copy answer exceeds 16 s, stop: the relay budget
needs a ruling, not a bigger constant. Record every number in
[HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md)'s evidence
table.

**Candidate build, before un-draft** (finding 16): the S branch deployed
to the owner (Paul deploys), then on Apple TV, web (Chrome) and Android,
the same ATSC 1.0 channel: stopwatch first-frame time against the
before-state recorded first, and stalls over 60 s (Apple: summed
`numberOfStalls`; web: hls.js `BUFFER_STALLED_ERROR` count / `waiting`
events; Android: `onPlayerStateChanged` buffering events after the first
ready). Then the ATSC 3.0 copy channel on Apple TV for its first minute.
Then the jitter case: `kill -STOP` the owner's FFmpeg for 1.5 s and
`-CONT` it, once, during playback on each client, and record whether the
picture stalled and for how long — this is the case the 2 s fallback (§8)
absorbs and 1 s does not, and it decides whether §8 is needed.

### N1 · the Android offset (`LiveTvPlayer.kt`)

§3.9. Acceptance: the JVM test; on the Android device, the ATSC 1.0 start
plays its first minute without a buffering event after the first ready.
Ships with S's candidate qualification, not separately.

### A1 · the controller (`LiveTvView.swift`)

§3.7 and §3.8. Acceptance: the four controller tests in §3.13 green on the
tvOS simulator destination.

### A2 · the surface and the focus (`LiveTvView.swift`, `Theme.swift`)

§3.10 and §3.11. Acceptance: the source-pin tests; 1080p simulator
screenshots of controls-revealed, paused and catching-up beside the
renders (the waiting state driven through `applyTimeControl` in a debug
build — say so in the PR).

### A3 · the Info ledger, and the device (`LiveTvView.swift`)

§3.12. Acceptance: the panel test; a screenshot beside `surface-info.png`
and one with the longest strings; then **the candidate build on the
Apple TV before un-draft**: Down / Left / Right / Up from each of the five
buttons lands on a button; Menu hides, Select reveals, first press after
reveal lands on Pause; Guide opens the panel over a hidden band and Close
returns focus to Pause; Info opens, Menu closes it, focus returns to Pause;
pause with the controls hidden, wait 35 s, and the cover returns to the
browser with the release sentence on the status line.

## 6. What the second review should attack

1. **§3.3's progress definition.** `newest_listed` advances on a new
   segment; is there a healthy producer shape where it does not for 30 s —
   a copy route with a 30 s+ GOP (is that a real broadcast?), a discontinuity
   that resets `media_sequence`?
2. **§3.4's budget.** One segment listed, the second slow: at 28 s the
   waiter answers `startup_timeout` while the producer may still be
   running for 2 s; does the relay's second attempt (same request id) ever
   observe the provisional that lands in that gap?
3. **§3.9.** Media3's default hold-back with `setMaxOffsetMs(8_000)` and a
   two-segment 1 s playlist — confirm at the build's dependency version
   that it starts at the first segment and does not speed-adjust into a
   stall. And whether `C.TIME_UNSET` is what an unset target reads back as.
4. **The disk envelope (§3.1).** Is 128 MiB the right cap once the ATSC
   3.0 GOP is known, or should the window be bounded in seconds? Say which
   before S1 lands, not after.
5. **§3.7's debounce.** 350 ms transcribed from the fixture: does the
   surface contract intend it to apply to a wait that is *not* a fault
   raise, and is there a second timer here that the contract forbids?
6. **§3.11's dismissal on `!playing && !busy`.** Enumerate the paths that
   set `playing = false` with `busy == false` and confirm each one wants
   the cover gone: `stop()` after the pause expiry, `stopChecked()` from
   the scene-phase background, the heartbeat's `stream_failed`. Is there
   one that should leave the cover up with a message instead?
7. **§3.10's telemetry chips on a no-guide, no-status start.** Only LIVE
   and the clock exist for the first 5 s; is that acceptable, or should the
   method chip come from `info.delivery` at attach time (it can)?
8. **The candidate-build qualification (§5).** Is the jitter case
   representative — a 1.5 s `SIGSTOP` on FFmpeg is a publish stall, not a
   network one; what is the network case, and does the 2 s fallback absorb
   that too?
9. **`parse_playlist_bytes` (§3.2).** hlsenc on a discontinuity, a clock
   jump, a `-0.000000` duration — what does it actually write, and does
   the refusal end a session that would have played?
10. **Everything §0 marks accepted** — check the fold was faithful.

## 7. Hand-off for the physical pass

Paste to a session with the hardware, for the candidate builds (before
un-draft; Paul deploys the S branch to the owner and installs the A
build):

> Before anything else, on today's build: on the Apple TV open Live TV,
> start channel <ATSC 1.0> and time Select to first frame with a
> stopwatch — that number is the before-state; write it down. Stay on the
> inline preview 30 s and say whether it freezes and roughly when; start
> again, go fullscreen immediately, and say the same. Then on the
> candidate builds (owner on the S branch, Apple TV on the A build): start
> the same channel, time Select to first frame; watch 60 s and note any
> freeze; open Info and report "behind the edge", "buffered" and "stalls".
> Press Menu to hide the controls, Select to reveal them, and say which
> button is focused; from each of the five buttons press Down, Left,
> Right and Up and report where focus lands (it must never leave the
> button row). Open Guide, close it, say where focus is. Open Info, press
> Menu, say where focus is. Pause with the controls hidden, wait 35 s, and
> say what is on screen. Then start <ATSC 3.0> and watch its first minute
> the same way. Then the same 60 s stall count on the web (Chrome, the
> plurx page's Live TV) and on the Android phone for <ATSC 1.0>. Report:
> the before and after first-frame times, every stall count, the focus
> table, and what the pause expiry showed.

## 8. If Paul rules the other way

- **Answer at one listed segment instead of two** (first frame as today;
  one modelled 0.5 s hiccup at 4 s). `STARTUP_LISTED_SEGMENTS = 1`,
  `STARTUP_LISTED_TARGET_DURATIONS = 1.0`; the two gate tests flip; nothing
  else moves. One constant, so it can also be tried after the fact.
- **Uniform 2 s segments** (the v1 draft): `time 2 / list 12 /
  delete_threshold 2`, `MAX_DELETION_LAG_SEGMENTS 3`; +3 s to the first
  frame, half the request rate, ≈ 3.6 s behind the edge, and 1.5 s of
  publish jitter absorbed where 1 s segments stall (finding 4). Worth it
  only if S3's jitter case shows stalls on the LAN.
- **Telemetry inside the band.** The strip's chips become a fourth band
  row above the buttons; the top scrim goes. Focus and everything else
  unchanged.
- **Live TV joins the surface fixture.** §3.7's tile becomes the first
  `buffering` row of that work; the reason/debounce semantics are already
  the contract's.

## 9. What this plan does not know

- Whether the focus trap has been seen on the device; the fix is right
  either way, and §7 records the before-state.
- The ATSC 3.0 mux's GOP, i.e. its segment length and `TARGETDURATION`
  under `-hls_time 1`, its bitrate, and therefore whether the disk
  envelope and the two-target-duration gate are comfortable there (S3
  measures all three). A variable-GOP mux that grows `TARGETDURATION`
  mid-session is outside what the gate promises; if S3 sees one, bounding
  the window by seconds is the follow-up.
- How `numberOfStalls` behaves across a `replaceCurrentItem` on this tvOS;
  §5 reads it summed over events after a clean start, which is the case
  that matters.
- Whether the 20 s relay attempt is generous enough for a long-GOP copy
  route; S3's `--via` run answers it with a real number, and 16 s is the
  line at which that becomes Paul's ruling.

## 10. Mechanics for the builder

- **Clone your own.** Never work in Paul's checkout. Clone from Forgejo
  (`http://forge.lan:3000/noirr/plurx.git`) into your own scratch
  (`/tmp/<name>` on the device VM if its `$HOME` is full — it usually is);
  `--filter=blob:none` is fine. `docs/ci/AGENT-COMPILE-LOOP.md` is how
  Rust gets compiled when the clone and `cargo` are on different machines;
  set it up before writing Rust.
- **The lane.** `git checkout -b effort/live-tv-start-stall origin/main`,
  push it, then one branch per milestone based on the lane
  (`s1/cadence-and-probe`, `s2/listed-gate-and-progress`, `s3/harness-and-hardware`,
  `n1/android-live-offset`, `a1/controller-waiting`, `a2/surface-and-focus`,
  `a3/info-ledger`), each opened as a PR into the lane with a `WIP:` title
  until it is ready. Forgejo has no draft flag; the `WIP:` prefix is the
  draft, and Forgejo refuses the merge while it is there. Un-draft with
  `PATCH /api/v1/repos/noirr/plurx/pulls/<n>` `{"title": ...}`; the fast
  lane's gate runs on every ready PR. Merge your own PRs; Paul does not
  want to click.
- **Gates.** Task PRs get `Effort development gate` (policy, formatting,
  static web contracts, affected compiles). `make history-check` must be
  green: every corrective commit needs a `regressions.d` mapping or a
  `tests/client-fixes.toml` anchor row — it is red on `main` today because
  the 2026-09-13 lanes merged without theirs, so expect to rebase onto a
  fixed `main` or to be blocked until it is; do not "fix" other lanes'
  anchors in this effort. A test that names a `docs/` path must name one
  that exists (`tests/operations/test_docs_index.py`).
- **Apple.** No Xcode in a Linux session. Compile on a Mac with Xcode
  and `xcodegen` — `cd clients/apple && xcodegen generate`, then
  `make apple-test` on both destinations, as
  [`../clients/PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md`](../clients/PLAYBACK-SURFACE-APPLE-BUILD-PROMPT.md)
  lays out — or through CI's `fast Apple compile`. If you have neither,
  the Swift is written blind and the Apple PR body says so; the compile
  becomes the first line of the A hand-off. `make apple-build-bump` after the final rebase,
  never before — the `mobile release version` job compares against `main`
  at merge time. Every Apple PR carries a `docs/apple-builds/<n>-*.md`
  note.
- **Android.** JVM tests only from here (`./gradlew testDebugUnitTest`);
  the device check is a hand-off.
- **Evidence.** Record the focused regression command in each PR body.
  The physical results go into `HDHOMERUN-LIVE-TV-STATUS.md`'s evidence
  table and the STATUS.md entry in the same PR that consumed them.
- **Promotion.** When S3, N1 and A3 have their device evidence, freeze
  task merges, merge current `main` into the lane, open the lane into
  `main`, wait for `Main promotion gate`, merge.
- **Reading order for anything you are unsure of:** this document → the
  diagnosis → `docs/clients/PLAYBACK-SURFACE-CONTRACT.md` §4 (for the one
  boundary in §3.7) → `docs/features/LIVE-TV-RELIABILITY-IMPLEMENTATION.md`
  (the previous Live TV lane, same shape as this one) → `AGENTS.md`.
