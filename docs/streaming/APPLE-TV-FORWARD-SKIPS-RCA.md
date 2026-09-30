# Apple TV forward skips — root cause and proposed control-policy repair

**Status:** historical diagnosis; cumulative pacing is merged into main,
physical Apple TV acceptance remains open · **Written:** 2026-09-20
America/New_York (capture and replay: 2026-09-21 UTC) ·
**Updated:** 2026-09-29.

Companion to the [sliding-HLS implementation contract](MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md),
especially §9.2. This diagnosis explains the repeated forward skips in
reference title S (file 6343) on Apple TV. The server publishes rolling
media at approximately twice playback speed, then removes the older prefix
from the advertised window. AVPlayer eventually resynchronizes forward and
omits media. Correct segment bytes do not prevent this failure.

Read §§1–3 for the evidence and §4 for why the earlier control protocol
did not prevent the fault. The [implementation handoff](APPLE-TV-FORWARD-SKIPS-IMPLEMENTATION.md)
records the reviewed build contract; the [delivery status](APPLE-TV-FORWARD-SKIPS-STATUS.md)
records what landed and which physical evidence remains open. The handoff
superseded the original proposed repair sections formerly in this file.

**Revision anchors:** production `c70390bc3`; source review checkout
`0afefd92ac79ca4945bf8ea4ae1ecdf1936b3037`. Re-verify symbols and surrounding
contracts against the intended implementation base; source line numbers can
move. The production capture did not include every served playlist or the
physical Apple TV's segment requests. The controlled replay establishes the
failure mechanism; it is not a packet-for-packet reconstruction of tvOS.

## 1. What the live session established

The serving node was `lab6`, build `v0.3.0-2976-gc70390bc3`, file 6343:
1080p H.264 and E-AC-3 copied from MKV into fragmented-MP4 HLS. The server
selected temporary rolling HLS because the immutable-VOD index was pending.

The same client attempt and producer attempt 1 repeatedly reported
`self_recovered` access-log stalls. From 00:08:28 through 00:25:18 UTC,
successive reports were about 72.15 wall seconds apart, while reported media
position advanced exactly 144 seconds each time. Follow-up logs extend the
same pattern through 00:40:56. This was not a sequence of application-driven
session replacements; the first later buffering-recovery reopen appears at
00:42:58.

Between faults, Activity showed roughly 64–65 seconds of client runway,
132 seconds of server ahead, approximately 1.99× production, 2× producer
pacing, a 16-second publication target, and only the initial producer
suspend. At the brief stalls, the client reported zero contiguous runway.
The between-fault buffer snapshot does not prove the buffer stayed full.

Two retained segments, 324 and 325, passed packet inspection without FFprobe
errors. Their video DTS joined at 1946.000 seconds and audio DTS also joined
at 1946.000 seconds. That rules out a malformed join at that sampled
boundary, not at every boundary in the movie.

## 2. The deployed implementation loses the media-time budget

These line numbers refer to deployed revision `c70390bc3`, not necessarily
the current checkout's line numbering, in
[transcode.rs](../../crates/plurxd/src/transcode.rs).

| Location at deployed revision | Behavior and consequence |
|---|---|
| 7159–7162, `publication_cycle` | Requires only that `next_publish_at` exists. It ignores the stored target instant and admits a complete batch after `available_at + TARGET/2`. With target 16 seconds, a batch can publish after eight wall seconds. |
| 7308 | Stores `next_publish_at = available_at + TARGET`, but the admission predicate does not wait for that target. |
| 1739–1789, `evaluate_flow` | Disables the physical ahead-time cap (`max_secs: 0`) and holds on unpublished staged inventory. Publishing the batch empties that staging budget, allowing production to keep running. |
| 7202–7212 | Advertises the last 180 seconds behind the producer's end, independently of the viewer's position. Faster publication therefore advances both ends of the playlist too quickly. |

At 2× production, three six-second segments take about nine wall seconds to
arrive. The 18-second threshold is reached immediately, and the snapshot exposes
the entire staged writer tail, not an explicitly selected batch. Its staged
budget is consumed, and the cycle repeats. Published media therefore advances
roughly 18 seconds per nine wall seconds. The viewer consumes nine seconds.
The excess grows until the moving window passes the viewer and AVPlayer
recovers into a newer portion of the presentation.

This violates the existing design's §9.2 requirement to track cumulative
consumption and carry segment overshoot across cycles. The publication
change entered in `52c1a0fab`; producer-end-relative windowing entered in
`f78a03443`; both shipped in the sliding-HLS promotion `0cad9b86b` (PR #377).
The later `626ba0e41` adjustment retained early publication and removed the
older alternate arm that allowed a short batch once the stored target was
due. It is not a new segment-byte defect introduced by the subsequent
scratch-reservations release.

The staging hold and publication threshold both become eligible when roughly
16 seconds of unpublished media have accumulated. At this observed cadence,
publication repeatedly consumes the inventory before a sustained hold takes
effect. The log's absence of later production holds is consistent with that
interaction; it is not proof that a particular thread wins every race.

## 3. Controlled AVPlayer replay reproduces missing media

Two muted macOS 27.0 AVPlayer instances ran simultaneously for 320 seconds,
at playback rate 1.0 with a preferred forward buffer of 60 seconds. They
received the same synthetic H.264/AAC fMP4 bytes: six-second segments,
target duration 16 seconds, initial inventory 48 seconds, a 180-second
sliding window, and `EXT-X-START:TIME-OFFSET=0`. No seek commands were issued.
Every segment remained on disk and readable; no loss or HTTP failures were
injected.

The only variable was playlist advancement: an 18-second batch every nine
wall seconds versus every 18 wall seconds. This is an isolated reproduction
of the observed publication behavior, not a run of the complete server or a
patch qualification.

| Result | Accelerated playlist | Playback-paced control |
|---|---|---|
| Self-recovered stalls | 2 | 0 |
| Missing segment requests | 45–46 and 69–70 | None |
| Media never requested per gap | 12 seconds | 0 seconds |
| Sampled position advances at faults | 75.240 s and 72.883 s in about 1 s each | None |
| Position advance beyond elapsed playback | 74.240 s and 71.883 s | No large discontinuities |
| Exact total content never rendered | Not measured directly | No observed continuity failure |
| Gap times | 208.258 and 280.385 seconds | None |
| Player rate | 1.0 | 1.0 |

The first request gap was segment 44 → 47; the second was 68 → 71. Both
left two six-second pieces unrequested despite their continued availability.
The 72.127-second interval closely matches the live fault cadence. These
12-second gaps do not establish total omitted content: already fetched
content can also be discarded or the player's timeline can be remapped.

**Correction after Fable's review:** the first draft incorrectly presented
12 seconds as the total dialogue loss and used it to explain the user's
initial estimate. The failure also includes approximately 72 seconds of
unexplained position advancement per production cycle. Replay position
continuity fails by roughly 74 and 72 seconds after subtracting elapsed wall
time. Position continuity, with segment requests as supporting evidence, is
the primary fix-acceptance metric.

The retained trace does not prove the exact amount of buffered media that
was decoded, rendered or discarded. Immediately before the first stall it
reports a loaded interval `[264, +6]` and player position near 207 seconds;
subtracting the position from the loaded start does not establish a
56-second contiguous buffer. Therefore neither "12 seconds total loss" nor
"exactly 72 seconds never rendered" is a direct measurement from this
harness. A frame/sample trace would distinguish omission from timeline
remapping. This uncertainty does not weaken the demonstrated pacing defect
or the requirement for continuous position in the repaired player.

The harness received stall notifications but no time-jumped notification;
adding that notification alone would not diagnose this replay.

The replay, request logs, packet inspections and production logs are retained
locally in `Claude outputs/apple-tv-skip-2026-09-20/`. `evidence.json` is the
entry point; `replay/result.json` gives the measured comparison, and
`replay/run.sh` rebuilds the synthetic experiment on a Mac with Swift and
FFmpeg. These are local diagnostic artifacts, not committed private media.

### 3.1 Evidence strength and alternative explanations

| Claim | Evidence | Limit |
|---|---|---|
| The affected movie used the explicit control path | Activity showed an explicit 30-second lease, active demand, rendering state, and `publication_clock_explicit` policy | A live lease alone does not prove correct pacing |
| The publication algorithm permits sustained 2× advancement | Deployed predicate, disabled ahead-time cap, 2× producer, and repeating production telemetry | Exact served manifests at the physical-TV fault were not captured |
| Accelerated playlists can make AVPlayer omit otherwise valid media | Controlled replay changes only the publication cadence and produces two request gaps | macOS AVPlayer, synthetic media, and an isolated server |
| Every source join is valid | Not established | Only segments 324–325 were inspected; they are not proven to be the exact fault boundary |
| A remote seek caused the captured pattern | User reports untouched remote; replay has no seek calls and reproduces the pattern | No complete physical remote event trace |
| The protocol repeatedly replaced the session | Contradicted by stable client and producer attempts during the repeating faults | A later recovery reopen at 00:42:58 is a separate event |
| Live TV has the same root cause | Not established | Its producer/controller path needs its own capture |
| Only Apple TV is affected | Not established | The iPad comparison used another title and was not controlled |

The strongest conclusion is a demonstrated server defect with a reproduced
AVPlayer failure mechanism matching the captured movie's cadence. Physical
tvOS verification remains a release requirement. Do not describe the
isolated comparison as a full-server regression test or a deployed fix.

## 4. Why the implemented control protocol did not prevent it

### 4.1 Demand reached the server; cumulative pacing was lost downstream

The [control protocol](../playback-control/PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
already carries the information needed for this policy. The
[M3b demand contract](../playback-control/PLAYBACK-CONTROL-PROTOCOL-M3-DEMAND-LEASE.md)
made accepted position, buffer and rate authoritative for producer pacing.
The subsequent sliding-HLS work deliberately transferred time pacing to the
publication scheduler. Its §9.2 required cumulative consumption accounting,
but the implemented scheduler only enforces a per-batch threshold and an
earliest wall-clock time.

The current path, in
[transcode.rs](../../crates/plurxd/src/transcode.rs) and
[playback_control.rs](../../crates/plurxd/src/playback_control.rs), is:

```text
accepted client position / rate / buffer / render state
    -> actor demand snapshot
    -> rolling_playback_rate -> rate-sized batch threshold
    -> publication_cycle -> earliest-time + completed-batch check
    -> actor publication observation accepted for current attempt
    -> served snapshot advances; 180-second window follows its end
    -> staging empties; evaluate_flow permits more production
```

`evaluate_flow` calculates an ahead-of-playhead diagnostic, but sets the
time ceiling passed to `ahead_hold` to zero. The replacement hold condition
looks at staged, unpublished seconds. Publishing consumes that staging
inventory without charging an enduring consumption budget. Correct demand
can therefore coexist with incorrect output.

### 4.2 Publication fencing does not validate the pacing decision

These are existing interfaces, abbreviated to the relevant fields. Re-check
their definitions in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs) at build
time; the additional interfaces proposed below do not exist yet.

```rust
// Fields from PlaybackDemandSnapshot:
pub position_ms: i64,
pub buffered_through_ms: i64,
pub playback_rate: f64,
pub render_state: RenderState,
pub seek_target_ms: Option<i64>,

// Existing RollingPublicationObservation:
pub(crate) struct RollingPublicationObservation {
    pub producer_attempt: u64,
    pub produced_segment: Option<i64>,
    pub produced_end_ms: Option<i64>,
    pub playlist_ready: bool,
    pub published_segment: Option<i64>,
    pub published_end_ms: Option<i64>,
    pub next_media_sequence: i64,
    pub resolved_fetched_segment: Option<i64>,
    pub resolved_fetched_end_ms: Option<i64>,
}
```

`observe_publication_at` rejects an expired actor, wrong producer attempt,
a frozen published failure, or media beyond verified completion. It advances
the accepted delivery ledger monotonically. It does not check the proposed
published end against consumption, reserve, or playlist-start safety. The
observation also does not carry the candidate first segment or its start
time, so it cannot directly validate the proposed window against playhead.

The scheduler's `next_publish_at` stores a real target instant, but
`is_some_and(|_| ...)` discards that value when deciding eligibility. This is
one concrete error; restoring that comparison alone is insufficient.
Eighteen media seconds every sixteen wall seconds still adds 450 seconds of
excess over an hour of 1× playback.

### 4.3 Native recovery does not need an application seek command

AVPlayer can recover its own rolling-HLS playback without issuing a plurx
control-protocol seek. The isolated replay did exactly that with no seek
calls. Therefore, valid application command sequencing does not protect a
player from a playlist that has already advanced past it.

The existing tests demonstrated staging and publication eligibility for
individual cycles. For example,
`mkv_hls_schedule_stages_short_segments_until_the_publication_clock` checks
an initial snapshot, a short staged segment, and a complete later batch. It
does not prove conservation of media time across repeated cycles or safety
after the 180-second window starts sliding. That missing sustained test let
the implementation diverge from the written §9.2 contract.

## 5. Implement the existing contract through the bounded handoff

The approved sliding-HLS §9.2 already specified cumulative media demand,
carried surplus, accepted-rate accounting and attempt rebasing. The repair
is to implement that missing behavior, rather than define another policy.
Fable's review also established that the existing publication commit path
can be retained: budget state stays in `RollingPublicationClock`, with the
actor validating accepted demand and the protected playlist start.

Follow [APPLE-TV-FORWARD-SKIPS-IMPLEMENTATION.md](APPLE-TV-FORWARD-SKIPS-IMPLEMENTATION.md)
for the exact integration, constants, startup corrections, legacy policy,
focused regression commands and fast-lane workflow. It replaces the first
draft's proposed actor-owned budget and additional commit apparatus. It
also corrects the unproven 90-second client-buffer assumption and separates
demand-observation age from the media-renewed lease.

The handoff adds no feature flags, settings UI, client capability gate or
route-disable requirement. It keeps the existing flow policy for the first
cut and defers predictive scheduling unless a focused test proves a need.

## 6. Scope and remaining evidence limits

This identifies the rolling-movie pacing defect and reproduces its failure
mechanism in macOS AVPlayer. Physical tvOS confirmation against the repaired
server remains part of the targeted release proof. Live TV uses a different
producer/controller and has not been captured for this incident. The iPad
observation used a different title and does not establish Apple-TV
exclusivity.

Use reference title S, file 6343, and node lab6 in committed prose. The local
raw evidence retains the original title, machine alias and media paths and
must remain untracked. Replay commands in the handoff are explicitly local
to Paul's Mac; they are not prerequisites that silently depend on another
reviewer's filesystem.

No playback implementation, setting or deployment was changed during this
investigation or document revision.
