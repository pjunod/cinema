# Live TV shared transport — one tuner per channel however many are watching, and captions only once they are proven

**Status:** ready for review · **Executes:** L4 / F-ltv-4 (design) and the
closed-captions half of Q9 / L7 / F-ltv-8 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 · **Against:** `main` @ `88a3957a` · **Re-verified:** 2026-09-24 @ `886fc8bd` (§2.4)

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

This is a design document first and a plan second: the assessment's verdict
on L4 is "keep opportunity; design separately", and its list of what the
design must define (slow-consumer eviction, bounded per-viewer queues,
cancellation and reference ownership including DVR consumers, per-plan
authorisation) is §3 here. Read the review's §3.4 row L4 and §3.1 row Q9,
then the assessment rows L4, F-ltv-4, Q9, L7 and F-ltv-8 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md).
The DVR transport this design generalises is
[LIVE-TV-DVR-IMPLEMENTATION.md §3.4–§3.5](LIVE-TV-DVR-IMPLEMENTATION.md);
the per-sink writer it depends on is
[DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION.md §3.3](DVR-SCHEDULER-GUIDE-VIEW-AND-SINK-ISOLATION.md)
(build that first). Work milestone by milestone (§5); each is a draft PR
into `main` under the fast lane. Every `file:line` is against `88a3957a` —
re-verify at build time. If a step seems to require a viewer sharing a
*capability*, a transport authorising anything, a change to the signed
start/activate/resource wire shapes, a change to `live_tv.max_sessions`'s
key or range, or advertising `CLOSED-CAPTIONS` for a graph without a
recorded fixture proof, stop and flag it.

**Correction to the review:** none to the facts. One narrowing: "default
ceiling 2" is the setting's default (`live_tv.rs:267-269`); it is validated
against `1..=4` (`:303`) and against the device's `tuner_count.min(4)`
(`:2899-2904`), so on a FLEX Duo the ceiling is 2 by hardware, not by
default. Sharing changes what a slot *means* (a tuner GET, not a viewer);
it cannot raise the number of slots.

## 1. Objective

1. N viewers on one channel of one device cost **one** tuner GET. A viewer
   joining a channel that is being recorded, or that another viewer is
   watching, takes no tuner slot. Step 1 keeps one FFmpeg per viewer.
2. A slow or stalled consumer (a viewer's FFmpeg that stops reading stdin,
   a DVR sink whose disk stalls) is evicted from the transport by its own
   bounded queue; the tuner reader and every other consumer continue.
3. Ownership is exact: a transport lives while it has a consumer or a DVR
   sink that will want it before `open_until`; the last detach cancels and
   joins the reader task, which is what releases the tuner; every consumer
   detaches on its own cancel, fence or idle timeout with no reference kept.
4. Authorisation stays per viewer: capability, activation, request key,
   fence, idle and stray rules are unchanged; the transport authorises
   nothing and a join is refused unless `(generation, device_id,
   channel_id)` match exactly.
5. `tuner_capacity` is refused only when a **new** transport is needed and
   no slot is free; `tuner_unavailable` is still the device's own 503 at
   open, seen by the consumer that opened.
6. Step 2 (optional, later): viewers whose resolved plans are identical
   share one FFmpeg and one scratch directory, still with per-viewer
   capabilities.
7. Captions: `CLOSED-CAPTIONS` is advertised for a graph only after a
   captioned fixture has been shown to survive that graph end to end; the
   VideoToolbox `-a53cc 0` workaround stays until the SEI failure is
   re-proven either way.

## 2. Contract today

Re-verify at build time.

### 2.1 One session, one tuner, one FFmpeg

Sessions are keyed by `LiveTvRequestKey { source_node_id,
source_serving_generation, user_id, request_id }` (`live_tv.rs:1252-1268`);
the only reuse is a replay of the same request id (`request_session`,
`:3158`). Every other start inserts a session and spawns `run_live_session`
(`:3205-3209`) → `open_tuner_stream` (`:5201`) → `collect_live_prefix` →
`probe_live_source` → `spawn_live_ffmpeg` (`:5296`) → `pump_tuner_stream`
(`:6306-6360`), which writes each chunk to the child's stdin under
`TUNER_READ_TIMEOUT` (30 s) and fails the session with "FFmpeg input
stalled" if the write does not complete.

Capacity at start (`:3161-3187`):

```rust
if registry.sessions.len() + registry.terminals.len() >= MAX_TERMINAL_TOMBSTONES { Capacity(...) }
if registry.held() >= usize::from(config.max_sessions) {
    match registry.stray_to_evict(request.user_id, now) {
        Some(stray) => { stray.cancel.cancel(); self.metrics.observe_stray_eviction(); }
        None => return Err(LiveTvError::Capacity(format!("all {} plurx Live TV session slots are in use", config.max_sessions))),
    }
}
```

with `held() = live_sessions() + transports.len()` (`:1440-1442`),
`transports: HashMap<String /* channel_id */, Arc<dvr::DvrTransport>>`
(`:1405`), and the DVR's own admission `occupancy_admits(sessions,
transports, max_sessions, reserve)` = `sessions + transports < max &&
transports < max - reserve` (`:1466-1470`). `open_tuner_stream`
(`:5567-5595`) maps the device's 503 to `TunerUnavailable`
(`tuner_unavailable`) and 404 to `ChannelNotFound`. `LiveTvError::Capacity`
is `tuner_capacity` (`:926`); the ingress dresses it with
`transport_holders()` (`dvr.rs:322`) so the client can offer to stop a
recording (`http/live_tv.rs:1655-`).

### 2.2 The fan-out primitive that exists

`DvrTransport` (`dvr.rs:65-102`): `{ channel, generation,
owner_serving_generation, cancel, delivered, sinks: Mutex<Vec<Arc<DvrSink>>>,
worker, source }`; `live_sinks()` filters cancelled sinks; `open_until()` is
the furthest sink window end. A sink joins an open transport when
`may_share_transport(transport.open_until(), row.capture_start)`
(`schedule.rs:178-180`), else the DVR asks `may_open_transport`. The
transport's worker (`run_transport`, `dvr.rs:1891-1909`) opens the tuner,
collects the same prefix as the live path, and fans chunks out
(`pump_tuner_fanout`, `:1914-2045`) with a per-chunk serving-fence check.
`close_transport` (`:1596-1619`) cancels the transport and every sink and
joins the worker under `SESSION_DRAIN_TIMEOUT`. After the DVR plan's M2,
each sink has an owned writer and a bounded byte queue; the fan-out's only
per-sink operation is a `try_send`.

### 2.3 Captions

`live_ffmpeg_command_for_input` maps `-map 0:v:0 -map 0:a:0 -sn -dn`
(`:5992`) — subtitle *streams* dropped, which says nothing about A/53 SEI
carried as video frame side data. `live_caption_args` (`:6170-6179`) emits
`-a53cc 0` for `Encoder::VideoToolbox` only, with the recorded reason
(VideoToolbox SEI insertion rejecting A/53 with `AVERROR_INVALIDDATA` and
killing the encode; the stderr diagnostic at `:6183-6184`). No other
encoder is told anything about captions. `activate_local` returns a media
playlist URL (`/api/v1/live-tv/sessions/<cap>/index.m3u8`, `:3296`), so
there is no master playlist and no `EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS`
anywhere on the live path; the VOD master emits `CLOSED-CAPTIONS=NONE`
(`http/hls.rs:12472`). Status: "captions lack end-to-end proof"
([HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md) row 184).

### 2.4 Re-verification at `886fc8bd` (L-03 design milestone, 2026-09-24)

Every premise in §2 and §3 was re-read against `main` @ `886fc8bd` (after
L-01's owned DVR writers and L-02's fence observer landed). Line numbers
below are at that commit; the ones in §2/§3 above are the plan's original
`88a3957a` numbers. **Verdict: the capacity and ownership design stands, but
eight of its premises are false or incomplete (D1–D8), and one of them (D7)
contradicts a guardrail in §4, so M1 cannot be built until it is decided.**

| Premise (plan §) | Code at `886fc8bd` | Verdict |
|---|---|---|
| default 2, range `1..=4`, capped by `tuner_count.min(4)` (preamble) | `live_tv.rs:301-303` (`unwrap_or(2)`), `:340`, `:3270-3275` | holds |
| sessions keyed by `LiveTvRequestKey`; only reuse is a replay (§2.1) | `live_tv.rs:1290-1295`, `request_session` `:1648-1684`, second check under the lock `:3530` | holds |
| every other start spawns `run_live_session` → own tuner GET → own FFmpeg → own pump (§2.1) | spawn `:3579-3582`; `open_tuner_stream` call `:5597-5605`; `collect_live_prefix` `:5606`; `probe_live_source` `:5607`; `spawn_live_ffmpeg` `:5700-5701`; `pump_tuner_stream` `:5725-5730` | holds |
| pump writes under `TUNER_READ_TIMEOUT` (30 s) and fails "FFmpeg input stalled" (§2.1) | const `:106`; writes `:6771-6772`, `:6803-6804` | holds |
| start capacity shape (§2.1) | `:3533-3559`, one registry lock hold from the tombstone check to the insert `:3560-3561` | holds |
| `held() = live_sessions() + transports.len()` (§2.1) | `:1505-1507`; `live_sessions()` **excludes cancelled sessions** `:1498-1503` | holds; the exclusion matters to D4 and D8 |
| `transports: HashMap<channel_id, Arc<DvrTransport>>` (§2.1) | `:1470`; inserted under `row.channel_id` `dvr.rs:1727-1729` | holds |
| `occupancy_admits` = `sessions + transports < max && transports < max - reserve` (§2.1) | `:1531-1535`, `may_open_transport` `:1518-1525` | holds |
| 503 → `tuner_unavailable`, 404 → `channel_not_found` (§2.1) | `:6024-6031`; codes `:962-965` | holds |
| the ingress dresses `tuner_capacity` with `transport_holders()` (§2.1) | `http/live_tv.rs:922`, `:1899-1916`; `transport_holders` `dvr.rs:566-590` lists **every** transport with its live sinks | holds; see D3 for viewer-only transports |
| `DvrTransport` fields, `live_sinks`, `open_until` (§2.2) | `dvr.rs:74-89`, `:92-100`, `:104-110` (`i64::MIN` when no live sink) | holds |
| sinks join when `may_share_transport` (§2.2) | `schedule.rs:178-180`, applied `dvr.rs:1649-1653` | holds |
| **the transport collects the prefix and probes it once (`source`), "exactly as `run_transport` does today"** (§2.2, §3.1) | `run_transport` `dvr.rs:2233-2251` collects the prefix and **never probes it**; `DvrTransport.source` is written nowhere in production — its only writers are the constructor's `None` (`:1725`) and a test (`:3192`) | **false — D1** |
| per-chunk serving-fence check in the fan-out (§2.2) | `dvr.rs:2294-2301` | holds |
| `close_transport` cancels, closes sinks, joins the worker under `SESSION_DRAIN_TIMEOUT` (§2.2, §3.3) | `dvr.rs:1916-1961`: cancel → settle sinks → **remove from the registry** → join (5 s). Returns early, leaving the entry, while a sink is settling (`:1937-1939`); removal `:1944-1951`, join `:1959` | incomplete — D4 |
| after the DVR plan's M2 each sink has an owned writer and a bounded queue; the fan-out only `try_send`s (§2.2, M1 dependency) | `SinkQueue` `dvr.rs:138-142`, `DVR_SINK_QUEUE_BYTES` `:68`, `start_sink_writer` `:351-370`, `try_send` `:2333` | holds — L-01 M2 is on `main`, so M1's dependency is met |
| "inserted in `registry.transports` before its worker is spawned, as the DVR does" (§3.3) | the insert (`dvr.rs:1727-1735`) does precede the spawn (`:1741-1766`), but the **capacity decision (`:1639-1664`) and the insert are two lock holds with an awaited folder creation and `O_EXCL` open between them** (`:1666-1684`) | **false as a model for viewers — D2** |
| a consumer attaches under the registry lock, only once its resources exist (§3.3, `dvr.rs:1438-1446`) | the sink is published after its file is open (`:1704-1715`) but **pushed into `transport.sinks` without the registry lock and without checking the transport is still running** | incomplete — D3 |
| a transport closes when it has no consumer and `open_until` has passed (§3.3) | two closers key on *sinks*: the fan-out returns when `live_sinks()` is empty (`dvr.rs:2302-2305`) and every DVR tick closes any transport with no live sink (`close_finished_transports`, `:1898-1914`) | **must change — D3** |
| `drain_before` closes transports with `generation <` the barrier, then sessions (§3.3) | `live_tv.rs:3864-3900` | holds |
| shutdown = `close_all_transports` then `cancel_and_wait` (§3.3) | `:3902-3913` | holds |
| `TUNER_READ_TIMEOUT` "moves entirely to the pump" (§3.2) | the fan-out needs it on the tuner *read* (`dvr.rs:2274`) or a silent tuner is never noticed | wording — D6 |
| a join requires `(generation, device_id, channel_id)` equality; a different generation → `settings_conflict` (§3.4, §3.5, §4) | `generation` is `live_tv.config_generation`, which **every** Live TV settings save bumps (`http/system.rs:2676-2679`, `:2792`), and a save only drains on a disable or owner transition (`:2702-2727`, `:3295-3300`). A live session ends on the next fence after a save (`validate_start_config` `live_tv.rs:5417-5421`); **a DVR transport does not** — nothing but a drain, disable, owner change or shutdown closes it (`dvr.rs:807-819`, `live_tv.rs:3868-3886`) | **contradiction — D7 (flagged)** |
| stray eviction fires only when a new transport is needed, "cancel it (its transport closes if it was alone) → retry once" (§3.4) | `stray_to_evict` `:1609-1640` picks any idle/retired session of the user; admission proceeds in the same lock hold because `live_sessions()` stops counting a cancelled session at once (`:1498-1503`, `:3547-3551`) | incomplete — D8 |
| `activate_local` returns a media playlist; no `CLOSED-CAPTIONS` on the live path; VOD master says `NONE` (§2.3) | `live_tv.rs:3668`; `http/hls.rs:12636` | holds |
| `-map 0:v:0 -map 0:a:0 -sn -dn`; `-a53cc 0` for VideoToolbox only (§2.3) | `live_tv.rs:6434`, `live_caption_args` `:6622-6631`, diagnostic `:6633-6636` | holds |
| the audit runs "`live_ffmpeg_command_for_input(.., GraphProbe)`'s real argv against the fixture" (§3.7) | `GraphProbe` replaces the input with `lavfi` `testsrc2` + `sine` (`:6418-6431`) and never reads stdin | **impossible as worded — C1** |
| audit hosts `media1`, `maca`, `macb` (§3.7, §5.3, §6.3) | the media inventory now names `nynuc`, `m6`, `nuc4`, `nuc3`; `nuc3`'s production `plurxd` validates QSV and VA-API (its boot log), jellyfin-ffmpeg 8.1.2 | stale names — the prompts below find the owner instead of naming it |

#### Design corrections for M1

- **D1 — the transport has to probe.** Viewers plan from `LiveSourceFacts`,
  and no transport has ever had any. M1 moves `probe_live_source` into the
  transport worker (after `collect_live_prefix`, before the fan-out starts),
  publishes `source` before the first byte reaches any consumer, and a
  viewer's start waits on that publication (bounded by its own
  `STARTUP_TIMEOUT`) before resolving its plan. A probe failure ends the
  transport with the probe's error, which every waiting viewer reports.
  Side effect, recorded here rather than fixed: DVR sidecars have never
  carried source facts, because `transport_source_facts` (`dvr.rs:2183-2203`)
  reads the always-`None` field; M1 fixes that as a consequence.
- **D2 — admission and insertion in one lock hold.** Today a viewer start
  that interleaves with a DVR attach between `dvr.rs:1664` and `:1727` can
  hold one tuner more than `max_sessions` (the device then answers 503 to
  whoever opens last). With viewers creating transports it becomes a
  correctness bug: two starts on one channel both see "no transport", and the
  second `HashMap::insert` replaces the first — whose worker still holds a
  tuner GET and is now neither counted nor reachable by `close_transport`
  (which looks transports up by key). M1 inserts the transport, in an
  `opening` state that joins may attach to, in the **same** lock hold as the
  capacity decision; any later failure removes it only if it is still the
  same `Arc` (`Arc::ptr_eq`, as `close_transport` already does at
  `:1944-1951`). The DVR attach path takes the same shape; its `O_EXCL`
  open moves after the reservation and a failed open releases it.
- **D3 — attach and close are consumer-aware and locked.** A consumer is
  pushed only under the registry lock and only when
  `!transport.cancel.is_cancelled()` and the transport is still the map's
  entry for its key; otherwise the start treats it as absent. The fan-out's
  "no live sink → return" and `close_finished_transports`'s "no live sink →
  close" both become "no live consumer" — as written today, the next DVR tick
  would close every viewer-only transport within 15 s. `transport_holders`
  lists only transports with at least one live `Recording` consumer
  (a viewer-only transport is not a recording the client can offer to stop);
  the watchable rows of §3.4 are a separate list.
- **D4 — the slot is freed before the tuner is.** `close_transport` removes
  the entry before joining the worker, and `live_sessions()` drops a
  cancelled session before its pump has exited. Both mean a start admitted in
  that window (≤ `SESSION_DRAIN_TIMEOUT`) can open while the device still
  counts the old GET and get `tuner_unavailable`. That is today's behaviour
  and sharing does not worsen it; §3.3's "joining it is what releases the
  tuner" stays true, but the slot count is not the device's count during a
  close. M1 keeps the order (removing after the join would refuse starts for
  up to 5 s on every channel change) and documents the window.
- **D5 — a source change invalidates the transport's facts.** Each viewer's
  stderr watcher (`live_tv.rs:5703-5713`, observed `:5761-5765`) ends only
  that viewer with `source_format_changed`. The transport's `source` then
  describes a mux that is gone, and a later joiner would plan from it.
  M1: the first `source_format_changed` from any viewer on a transport marks
  the transport's facts stale; a stale transport admits no joiner (the start
  waits for the transport to close and opens a new one, or is refused
  `tuner_capacity` if the old one still has other consumers).
- **D6 — timeouts.** The fan-out keeps `TUNER_READ_TIMEOUT` on the tuner
  read; only the *write* timeout moves to the per-viewer pump (stdin) and the
  DVR writer (disk). §3.2's sentence is corrected accordingly.
- **D7 — the generation join rule would lock viewers out of a recorded
  channel after any settings save. Flagged; not decided here.** A recording
  transport opened at generation 3 keeps running after an unrelated save
  (say, output height) bumps the generation to 4. Under §3.4 a viewer start
  at generation 4 for that channel is refused `settings_conflict`, and
  "never a second transport for one key" forbids opening its own — for the
  rest of the recording, and "reload channels" (the message) cannot help.
  The plan's own guardrail (§4, "A join never crosses a generation") is what
  produces this, so it is not this session's to relax. Two options for the
  decision: **(a)** keep the guardrail and allow exactly one extra transport
  for a key whose existing transport is on an older generation (the old one
  still drains by generation; costs a tuner in exactly this case);
  **(b)** narrow the join key to what the bytes depend on — `device_id`,
  `channel_id`, the device address the transport opened, and a current
  serving generation — since the transport carries raw tuner bytes and every
  other generation-scoped setting (heights, deinterlace, guide) is applied by
  the joiner's own FFmpeg and plan. The recommendation is (b): a drain still
  closes old-generation transports, and a changed device address or owner
  still refuses the join. Needs Paul's call before M1.
- **D8 — stray eviction must free a transport, and must be waited for.**
  With sharing, cancelling a stray that shares its transport frees nothing,
  so `stray_to_evict` gains a filter: only a stray that is its transport's
  last live consumer (and whose transport has no `Recording` consumer)
  qualifies. And because slots become `transports.len()`, which drops at
  `close_transport`'s removal rather than at cancel, "retry once" is a
  bounded wait: the start releases the lock, awaits that transport's close
  (≤ `SESSION_DRAIN_TIMEOUT`), then re-runs admission once.

#### Captions: what the code and fixtures establish, before any audit run

- **C1 — the audit uses the tuner argv.** `GraphProbe` cannot see a fixture.
  The audit builds `live_ffmpeg_command_for_input(.., Tuner)` from the
  production planner's plan for the production probe of the fixture, and
  feeds the fixture on stdin exactly as `pump_tuner_stream` feeds the tuner.
- **C2 — the fixture is generated, not committed.** The repository keeps no
  binary media fixtures (every existing live-TV media test synthesises its
  input, e.g. `videotoolbox_tests.rs:36-128`, `atsc_audio_tests.rs`), so the
  10 s 1080i MPEG-2 fixture is built at test time by
  `live_tv/caption_audit_tests.rs` (`captioned_fixture`), with its ground
  truth in code. `PLURX_CAPTION_FIXTURE_OUT=<path>` writes a copy for use
  outside the tests.
- **C3 — the existing VideoToolbox fixture proves survival, not captions.**
  `videotoolbox_tests.rs:64-83` injects a single CEA-608 control pair
  (`0x94 0x20`, resume caption loading) with no text, and runs only with
  `-a53cc 0`; it shows the encode survives A/53 input, nothing about
  carrying it.
- **C4 — `-hwaccel none` on every route** (`live_tv.rs:6404`): decode is
  software everywhere, so A/53 always reaches the filter chain as frame side
  data; whether it leaves depends only on the filters and the encoder, which
  is exactly what the per-graph audit varies.
- **C5 — "no advertisement" is not "no caption track" on today's clients.**
  The live path hands every client a media playlist, and each client's own
  default decides what it does with in-band 608: web builds hls.js with no
  caption options (`web/pages/live-tv-controls.js:97`) and Safari plays the
  playlist natively (`:103-104`); Android builds a default
  `ExoPlayer` with `DefaultMediaSourceFactory` (`livetv/LiveTvPlayer.kt:221-224`);
  Apple uses a plain `AVPlayer()` with automatic media selection
  (`LiveTvView.swift:55`). None of the three live players has a caption
  control or a caption test. Whether any of them already surfaces a CC track
  today — and whether a VideoToolbox session then shows an empty one — is a
  device fact, and it is the baseline M4 changes; the §6.3 caption prompt
  records it before M4 is built.
- **C6 — a caption proof belongs to an FFmpeg build, not to an encoder
  family.** Measured by the M3 audit (the per-graph table in
  [HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md)): on the
  default engine (jellyfin-ffmpeg 8.1.2) every audited graph preserves CC1
  and service 1, and `h264_vaapi` already includes `a53_cc` in its default
  `-sei`, so §3.7's "add `-sei +a53_cc` on VAAPI" is not needed there. On the
  distro fallback engine in the same image (FFmpeg 5.1.9, `/usr/bin`) the
  default `send_field` deinterlace (`LiveDeinterlaceOutput::Field`,
  `live_tv_delivery.rs:291-294`) copies each frame's `cc_data` onto both
  fields, so CC1 text doubles and every 708 packet repeats; and `h264_vaapi`
  there has no `a53_cc` flag at all, so the review's flag would be a hard
  error, not a fix. A static "proven encoders" set in M4 would therefore
  advertise a garbled track on any node running the fallback. M4 instead
  needs a boot-time caption probe per validated encoder and deinterlace mode
  — a short slice of the M3 fixture through the live filter and encoder,
  decoded with the M3 decoder, recorded with the encoder caps the way
  `forced_idr` is — and advertises only for a session whose admitted
  (encoder, deinterlace) pair preserved both on this node's own FFmpeg.

## 3. Change

### 3.1 `LiveTransport` — the DVR transport, generalised

```
                          LiveTransport { key: (device_id, channel_id), generation,
                                          owner_serving_generation, cancel, worker,
                                          source: Option<LiveSourceFacts>,
                                          consumers: Mutex<Vec<Arc<Consumer>>> }
  HDHomeRun /auto/v2.1 ──▶ reader task ──┬─▶ Consumer::Viewer  { queue ≤ VIEWER_QUEUE_BYTES } ─▶ pump ─▶ ffmpeg A stdin ─▶ live-tv-<capA>/
   (one GET per key)      (fence/chunk)  ├─▶ Consumer::Viewer  { queue ≤ VIEWER_QUEUE_BYTES } ─▶ pump ─▶ ffmpeg B stdin ─▶ live-tv-<capB>/
                                         └─▶ Consumer::Recording { DvrSink; queue ≤ DVR_SINK_QUEUE_BYTES } ─▶ writer ─▶ <base>.a1.part
```

`DvrTransport` is renamed `LiveTransport`, keyed by `(device_id,
channel_id)` (the DVR keyed by `channel_id` alone; the device id is in
every session already, `LiveTvSession.device_id`), and `sinks` becomes
`consumers`:

```rust
enum Consumer {
    Viewer(Arc<ViewerConsumer>),
    Recording(Arc<DvrSink>),
}
struct ViewerConsumer {
    capability: String,
    cancel: CancellationToken,            // the session's own token, cloned
    queue: SinkQueue,                     // the DVR plan's bounded byte queue
    /// Set when the consumer is evicted, read by the session's observe loop.
    evicted: StdMutex<Option<&'static str>>,
}
/// ≈ 1.6 s of a 19.4 Mbit/s mux. Smaller than the DVR's 8 MiB because a
/// viewer's FFmpeg reading stdin is the fast path; a queue this deep that is
/// still full means the producer has stopped, and PRODUCER_PROGRESS_TIMEOUT
/// would say so 30 s later — eviction says so now, per viewer.
const VIEWER_QUEUE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CONSUMERS_PER_TRANSPORT: usize = 16;   // 16 × 4 MiB is the memory bound
```

The reader task is `pump_tuner_fanout` with the DVR window check applied
only to `Recording` consumers; a `Viewer` receives every chunk from the
moment it attached. Per chunk: serving-fence check, then `try_send` per
live consumer, no awaits on any consumer.

Prefix and probe: the transport collects the prefix and probes it once
(`source`) — **corrected (§2.4 D1): `run_transport` collects the prefix but
has never probed it, and M1 adds the probe** — so every consumer's
sidecar and plan see the same facts. A viewer that attaches later gets the
transport's `source` for its plan without probing (the L6 warm-start
verification applies to a *new* transport; a joiner is verified by the
transport's own probe of this tune, which is strictly better evidence than a
cache). The joiner's first bytes are the live edge, not the prefix — FFmpeg
starts on the next PAT/PMT + keyframe, as it does on any zap.

### 3.2 Slow-consumer eviction and bounded queues

Every consumer has its own `SinkQueue` (bytes and slots bounded). On
overflow the fan-out drops the consumer's queue, sets `evicted` with a
reason, and cancels nothing else:

| Consumer | Overflow means | What happens |
|---|---|---|
| `Viewer` | its FFmpeg stopped reading stdin (stalled encoder, blocked scratch write) | `evicted = "producer_backlog"`; the session's observe loop ends it with `StreamFailed("the live-TV producer stopped consuming tuner bytes")` on its next tick; the pump exits; the child is killed and reaped by the existing cleanup |
| `Recording` | disk stalled | the DVR plan's `disk_write_backlog`: the sink ends its attempt; recovery rolls the next |

The reader never waits on a consumer; the only thing that can stall the
tuner GET is the tuner. `TUNER_READ_TIMEOUT` stays on the fan-out's tuner
*read* and its *write* use moves to the pump (per viewer, writing to its own
stdin) and the DVR writer (per sink); the fan-out has no write timeout
because it has no writes (§2.4 D6).

Metric: `plurx_live_tv_consumer_evictions_total{kind,reason}` with `kind ∈
{viewer, recording}`, `reason ∈ {backlog, fenced}` — four series.

### 3.3 Ownership, cancellation, reference counting

- A transport is created by the first consumer that needs one, under the
  registry lock, after capacity admits it (§3.4); it is inserted in
  `registry.transports` before its worker is spawned. **Corrected (§2.4
  D2):** the DVR decides capacity and inserts in two separate lock holds;
  M1 reserves the entry in the same lock hold as the capacity decision.
- A consumer attaches under the registry lock and only after its own
  resources exist (the viewer's scratch directory and FFmpeg stdin; the
  sink's `O_EXCL` file) — the DVR's "publish the sink only once its file is
  open" rule (`dvr.rs:1438-1446`) generalised.
- A consumer detaches when its cancel fires (viewer stop/idle/fence/stray
  eviction/drain; DVR sink stop/window end/failure). Detach removes it from
  `consumers`; the `Arc<Consumer>` is not kept by the transport afterwards.
- The transport closes when `consumers` is empty **and** `open_until()` has
  passed. `open_until()` is `max(now for any live viewer, window.1 for any
  live sink)`, so a recording transport keeps the DVR's join semantics
  (`may_share_transport`) and a viewer-only transport closes on the last
  detach with no linger (a linger holds a scarce tuner; §7.1).
- Close = `transport.cancel.cancel()`, join the worker under
  `SESSION_DRAIN_TIMEOUT`, remove from the registry — `close_transport`
  today. The worker owns the `reqwest::Response`, so joining it is what
  releases the tuner (the invariant `pump_tuner_stream`'s comment states,
  `:6312-6314`).
- Fencing: `drain_before` closes transports with `generation <
  drain_before_generation` (unchanged, `:3500-3514`), which detaches every
  consumer; each viewer session then ends on its own fence check with the
  existing `settings_conflict`/`owner_unavailable` reasons. The per-chunk
  `serving.is_current(owner_serving_generation)` check stays in the reader.
- Shutdown: `close_all_transports` then `cancel_and_wait(sessions)` — the
  order today, kept.

### 3.4 Capacity: slots are transports

`held()` becomes `transports.len()` — a viewer joining an existing
transport holds no slot. The start path (`:3170-3187`) becomes:

```
if a transport exists for (device_id, channel_id):
    if its generation != the request's                     → settings_conflict (never a join)
    else if consumers.len() < MAX_CONSUMERS_PER_TRANSPORT  → join, no slot, no stray eviction
    else                                                   → tuner_capacity "this channel is full"
                                                             (never a second transport for one key)
else if transports.len() < max_sessions                    → open a new transport
else stray_to_evict(user_id) → cancel it (its transport closes if it was alone) → retry once
else Capacity("all N tuners are in use")                   → tuner_capacity, with transport_holders()
```

`occupancy_admits` keeps its two rules over transports only: `transports <
max && recording_transports < max - reserve`, where a *recording
transport* is one with at least one `Recording` consumer. A viewer joining
a recording transport does not change that count; a recording joining a
viewer's transport turns it into a recording transport and must pass the
reserve rule at that moment (a `DvrSink` join therefore asks
`may_open_transport`-style admission even when it joins — the DVR plan's
"a sink joining needs no slot" comment is narrowed to "needs no *tuner*
slot but does count toward the recording reserve").

The stray rule ("a viewer's own stray goes first") is unchanged and still
per capability; with sharing it fires only when a new transport is needed.
`tuner_capacity`'s body still lists recording holders; it additionally
lists watchable channels (transports with a free consumer seat), so a
refused viewer can be offered "watch 2.1 instead" — the same
`DvrHolder`-shaped rows, a client-side rendering choice, not a new field
type. `tuner_unavailable` is unchanged: only the consumer that opened the
transport can see the device's 503; a later joiner of a transport that then
fails gets the transport's terminal error through its pump.

The setting key `live_tv.max_sessions` and its `1..=4`/tuner-count range
are unchanged; its Developer label gains "(tuners)". Renaming the key is a
client migration for no behavioural gain.

### 3.5 Per-plan authorisation

Nothing about *who may watch* moves into the transport. Public start
authorises the user (`AuthUser`), validates the config and generation,
mints a capability bound to the owner node, and activation binds it to the
starting voter and serving generation (`activation_is_bound_to_the_starting_voter_and_serving_generation`,
`:8317`). All of that is per session and untouched. A join requires the
transport's `(generation, device_id, channel_id)` to equal the request's;
a request with a different `config_generation` is refused with
`settings_conflict` as today rather than joining a transport opened under
another configuration. Each viewer resolves its own `LiveDeliveryPlan`
from the transport's `source` and its own `playback` capabilities — two
clients on one transport may get two different plans — and each runs its
own FFmpeg in step 1.

### 3.6 Step 2 — shared FFmpeg per plan (optional, L, after step 1 is measured)

Key: `(transport key, PlanIdentity)` where `PlanIdentity` is the planner's
output tuple — `video_action, video_codec, height, frame_rate,
deinterlace, audio_action, audio_codec, audio_channels, packaging,
max_bitrate_bps` — computed from `LiveDeliveryPlan.output` and never from
client identity. Viewers with equal `PlanIdentity` attach to one
`Producer { child, scratch dir, stdin pump, consumers: Vec<capability> }`;
each keeps its own capability, activation, `last_touch`, idle timeout and
request key; the producer stops when its last viewer detaches. The
resource routes serve the shared directory under each capability
(`resource_local` already resolves a capability to a directory).
Preconditions before this is built: step 1's `plurx_live_tv_transports`
and consumer counts observed on the fleet for a fortnight show two or more
viewers per transport happening; the encode admission model
(`admit_live_tv`) is extended so one producer holds one permit however
many viewers read it; and the assessment's requirement — equivalent plans
and independent authorisation — is exactly §3.5 plus `PlanIdentity`
equality, tested field by field. Not scheduled in this document.

### 3.7 Captions — audit first, advertise after

Three stages, in order, each its own PR; nothing is advertised until the
first two have a recorded result per graph.

1. **Fixture.** `tests/fixtures/live-tv/captioned-608-708.ts`: a 10 s
   MPEG-2 1080i TS with CEA-608 CC1 and CEA-708 service 1 captions
   (generated with `ffmpeg` + a `.scc`/`.srt` through `-c:s` into A/53 via
   `libcaption`-style tooling, or captured from a broadcast and trimmed —
   whichever the builder can reproduce; the generator command is committed
   beside the fixture). Its ground truth is the caption text per second.
   **As built (§2.4 C2):** generated at test time by
   `live_tv/caption_audit_tests.rs`, not committed as a binary.
2. **Audit per graph.** For each `Encoder` the fleet enables (x264, QSV on
   `media1`, VAAPI where present, VideoToolbox on `maca`/`macb`, NVENC if
   any node has it) and for the copy route: run
   `live_ffmpeg_command_for_input(.., GraphProbe)`'s real argv against the
   fixture (**as built, §2.4 C1:** the `Tuner` argv with the fixture on
   stdin — `GraphProbe` never reads its input), then `ffprobe -show_frames -select_streams v` on the output and
   count frames whose side data lists `Closed Captions`; then extract with
   `ffmpeg -f lavfi -i "movie=out.ts[out+subcc]" -map 0:1 out.srt` and
   diff against the ground truth. Record per graph: preserved / dropped /
   partial, and the exact argv. Where VAAPI drops them, add `-sei +a53_cc`
   and re-run; where x264/QSV/NVENC drop them, the default is not what the
   appendix assumed and the fix is per encoder. VideoToolbox keeps
   `-a53cc 0` and is recorded as "disabled, reason: SEI insertion failure
   (`live_encoder_diagnostic`)" until a run with `-a53cc 1` on the fixture
   on `maca` either reproduces the failure (keep) or does not (open a
   separate change with that evidence).
3. **Advertise, per proven graph only.** `activate_local` returns a master
   playlist URL whose body is
   `#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID="cc",NAME="English",INSTREAM-ID="CC1",LANGUAGE="en"`
   (and `INSTREAM-ID="SERVICE1"` when 708 was proven) plus one
   `#EXT-X-STREAM-INF:...,CLOSED-CAPTIONS="cc"` line pointing at the media
   playlist — only when the session's admitted encoder is in the proven
   set; otherwise `CLOSED-CAPTIONS=NONE` (the VOD master's existing
   choice). Service ids come from what the audit found in the fixture and
   in a broadcast capture, not from a constant. The three clients are
   checked on device: hls.js (`enableCEA708Captions`), AVPlayer's
   `mediaSelectionGroup(forMediaCharacteristic: .legible)`, Media3's
   default CEA-608 track. A phantom track (advertised, empty) fails the
   milestone.

The bitrate/cadence half of Q9 (output fps under `bwdif=send_field`,
rational arithmetic, `LiveDeliveryOutput.frame_rate`) is **not** here; it
belongs with the encoder-by-invariant series in §5.2 of the review and
touches the planner, not delivery.

Metric: none new for captions; the master playlist is observable in the
session status (`delivery.reasons` gains `captions_advertised` when it
is).

## 4. Guardrails (non-goals)

- **No capability is shared, no transport authorises.** One capability =
  one viewer = one activation. The transport is a byte fan-out.
- **A join never crosses a generation.** `(generation, device_id,
  channel_id)` equality is required; `drain_before` still closes by
  generation.
- **The reader never awaits a consumer.** Eviction is by bounded queue;
  `TUNER_READ_TIMEOUT` lives in pumps and writers only.
- **Tuner slots cannot grow.** `live_tv.max_sessions` keeps its key,
  range and tuner-count cap; sharing changes what a slot counts.
- **DVR semantics are kept whole:** `O_EXCL` attempt files, per-sink
  windows, the recording reserve, `open_until` joins, gap accounting, the
  per-chunk serving-fence check.
- **Step 2 is not built on step 1's PR.** It has its own preconditions
  (§3.6) and its own document when it comes.
- **No `CLOSED-CAPTIONS` without a per-graph fixture result.** No
  `INSTREAM-ID` invented; VideoToolbox stays `-a53cc 0` until re-proven;
  copy routes are audited too (captions survive a copy, but the claim is
  still made only after the check).
- **Q9's cadence/bitrate half is out of scope here** (planner series).
- **Not in this plan:** L6 warm start and L2/L3/L9 (their own plan), L1/L10
  (DVR plan), `EXT-X-PROGRAM-DATE-TIME` (F-ltv-13 withdrawn as written).

## 5. Milestones

### 5.1 M1 — `LiveTransport` with viewer consumers; capacity by transport

Files: `live_tv/dvr.rs` → `live_tv/transport.rs` (rename + `Consumer`),
`live_tv.rs` (registry key, `held`, start path, `run_live_session_inner`
attaching instead of opening, pump reading from a queue), `http/live_tv.rs`
(`tuner_capacity` body rows). Depends on the DVR plan's M2 (owned writers).

Tests (`live_tv.rs`/`transport.rs` `mod tests`, in-memory tuner stream as
in the DVR plan; `make live-tv-two-node-check` for the real device fixture,
whose `opens` counter is the oracle for "one GET"):

- `two_viewers_on_one_channel_open_one_tuner_get` (two-node harness):
  start two sessions for `2.1` from two users; fixture `opens == 1`;
  `plurx_live_tv_transports == 1`; both publish; both play 10 s.
- `a_viewer_joins_a_recording_transport_without_a_slot`: one recording on
  `2.1` with `max_sessions = 1`; a viewer start on `2.1` is admitted;
  a viewer start on `4.1` is refused `tuner_capacity` and its body lists
  `2.1` as watchable.
- `a_stalled_viewer_is_evicted_and_its_sibling_continues`: viewer B's
  FFmpeg stdin never read (fake child); B ends with `stream_failed`
  "stopped consuming" within `VIEWER_QUEUE_BYTES / rate`; A's byte count
  keeps rising; `consumer_evictions_total{kind="viewer",reason="backlog"}`
  is 1; the transport is still open.
- `the_last_detach_closes_the_transport_and_releases_the_tuner`: stop both
  viewers; the worker is joined; the in-memory stream is dropped (its
  `Drop` sets a flag); `transports == 0`.
- `a_recording_keeps_the_transport_open_past_the_last_viewer`: viewer
  leaves, sink window still open → transport stays; window ends → closes.
- `a_join_across_a_generation_is_refused`: transport at generation 3; a
  start with `config_generation = 4` → `settings_conflict`, no join.
- `a_drain_closes_a_shared_transport_and_ends_every_viewer`:
  `drain_before(4)` → transport closed, both sessions end with the existing
  reasons; `plurx_live_tv_session_ends_total{reason}` reflects both.
- `a_recording_joining_a_viewer_transport_respects_the_reserve`:
  `max_sessions = 2, tuner_reserve = 1`, one viewer transport, one
  recording transport; a second recording asks to join the viewer's
  transport → refused (would make two recording transports).
- `capacity_rules_over_transports` (pure, replaces `occupancy_admits`
  cases): the table in §3.4 as a property test.
- `stray_eviction_only_when_a_new_transport_is_needed`: a user's stray on
  `2.1` and a new start on `2.1` → join, stray untouched; new start on
  `4.1` at capacity → stray evicted.

Acceptance: `cargo test -p plurxd transport` green; `make unit` green;
`make live-tv-two-node-check` green (host named in the PR body);
`plurx_live_tv_transports` and `plurx_live_tv_transport_consumers{kind}`
(two gauges, `kind ∈ {viewer, recording}`) present on `/metrics`.

### 5.2 M2 — clients: the capacity body's watchable rows (web · Apple · Android)

`tuner_capacity` detail gains `watchable: [DvrHolder-shaped rows]`; the
web toast offers "Watch 2.1 instead"; Apple and Android render the same
from the shared fixture `tests/playback/live-tv-start-cases.json` (a new
case, not a changed one). Acceptance: `node tests/playback/web-policy.test.js`
green with the new case; the Apple and Android contract tests that consume
the fixture green; a device pass per the GPT prompt in §6.3.

### 5.3 M3 — captions fixture and per-graph audit (no advertising)

Files: `tests/fixtures/live-tv/captioned-608-708.ts` + generator script;
`live_tv.rs` (`-sei +a53_cc` for VAAPI if the audit says so);
**as built:** `live_tv/caption_audit_tests.rs` (generator, decoder, tests,
audit) and `scripts/live-tv-caption-audit` (the plan's
`scripts/live_tv_caption_audit.sh`, named like the other `scripts/live-tv-*`
tools); no `live_tv.rs` flag change — see §2.4 C6;
[HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md) row 184 updated
with the per-graph table.

Tests:

- `the_caption_fixture_carries_608_and_708` (unit, ffprobe on the
  fixture): frames with `Closed Captions` side data > 0; extracted text
  equals the committed ground truth.
- `caption_args_per_encoder_match_the_audit_table`: `live_caption_args`
  per `Encoder` equals what the status table records (VideoToolbox
  `-a53cc 0`; VAAPI `-sei +a53_cc` iff recorded; others empty).
- The audit itself is `make live-tv-hardware-check`-adjacent: a script
  `scripts/live_tv_caption_audit.sh <encoder>` that runs the graph-probe
  argv on the fixture and prints preserved/dropped/partial; run on
  `media1` (x264, QSV), `maca` (VideoToolbox, expected disabled), and any
  VAAPI/NVENC node; results pasted into the status doc.

Acceptance: the status table has one row per enabled encoder plus copy,
each with a result and an argv; `make unit` green.

### 5.4 M4 — advertise `CLOSED-CAPTIONS` for proven graphs

Files: `live_tv.rs` (master playlist body, `activate_local` URL,
`delivery.reasons`), `http/live_tv.rs` (route for the master), the three
clients only if a device check shows they need a nudge (hls.js and Media3
default on; AVPlayer surfaces legible options when the master says so).

Tests:

- `a_proven_encoder_gets_a_master_with_a_caption_group_and_an_unproven_one_gets_none`:
  table-driven over `Encoder`.
- `the_master_names_only_services_the_audit_found`: `INSTREAM-ID` set
  equals the recorded set, never a constant.
- Device pass (§6.3): captions toggle on and show text on web, tvOS and
  Android TV for one x264 session and one QSV session; no empty track on a
  VideoToolbox session.

Acceptance: `cargo test -p plurxd caption` green; `make unit` green; the
device pass recorded in the status doc before the PR leaves draft.

## 6. Verification and rollout

### 6.1 Commands

| M | Focused | Gate |
|---|---|---|
| M1 | `cargo test -p plurxd transport` · `make live-tv-two-node-check` | `make unit` |
| M2 | `node tests/playback/web-policy.test.js` · Apple/Android fixture tests | `make unit` |
| M3 | `cargo test -p plurxd caption` · `scripts/live_tv_caption_audit.sh` per node | `make unit` |
| M4 | `cargo test -p plurxd caption` | `make unit` |

### 6.2 Rollout

One draft PR per milestone into `main` under the fast lane. M1 is a change
in what a tuner slot counts and needs no switch: the previous behaviour is
the degenerate case (every viewer opens a transport) and cannot be selected
without also selecting the old bugs. Deploy M1 to `media1` (owner) with the
ansible playbook and watch `plurx_live_tv_transports`,
`plurx_live_tv_transport_consumers{kind}`, `consumer_evictions_total` and
`session_ends_total{reason}` for a day before the rest of the fleet.
Mixed fleet: the ingress relays capabilities, not transports; an old
ingress and a new owner interoperate; no wire shape changes. M4 changes
the activation's `playlist_url` target from a media to a master playlist —
all three clients already load whatever URL activation returns (hls.js,
AVPlayer and Media3 accept either), so no client version gate is needed,
but the device pass in §6.3 is the proof, not this sentence. Rollback is the
previous immutable image tag.

### 6.3 What only devices and the fleet can prove — GPT prompts

```
Shared transport on real hardware. On media1 with the FLEX 4K (2 tuners
in use by plurx, live_tv.max_sessions = 2):
1. Open Harbor Lights (2.1) on the living-room Apple TV, the Shield and a
   phone browser at the same time. Report: `plurx_live_tv_transports`,
   `plurx_live_tv_transport_consumers{kind="viewer"}`, and the HDHomeRun's
   own tuner status page (http://10.42.0.20/tuners.html) — how many
   tuners show 2.1?
2. Schedule a recording on 2.1 while all three watch. Expect no new
   tuner; `{kind="recording"}` becomes 1.
3. On the phone, open Night Tide (4.1). Expect a second tuner. Then on
   the Shield open 6.1: expect `tuner_capacity` with 2.1 and 4.1 listed
   as watchable, and the toast offering them.
4. Pull the Shield's HDMI mid-stream for 20 s (its player stops reading).
   Expect: the other two keep playing without a stall; within ~2 s
   `consumer_evictions_total{kind="viewer",reason="backlog"}` increments
   by 1 (or, if Media3 keeps reading into its buffer, no eviction — report
   which). Report the numbers, not a summary.
```

```
Caption audit on device. With M4 deployed and the status table showing
x264 and QSV as proven:
1. Tune 2.1 (MPEG-2, captioned) on tvOS: Settings → Accessibility →
   Subtitles & Captioning → on; in the player, the legible menu must list
   "English CC"; captions must render text matching the broadcast.
2. Same on Android TV (Media3 caption track) and on web (hls.js CC1).
3. On maca as the owner (VideoToolbox): the legible menu must NOT list a
   caption track and the stream must not fail. Report the encoder label
   from Activity and the master playlist body (curl it).
4. Report any session where a caption track is listed but shows no text
   for 60 s on a channel known to carry captions — that is a phantom
   track and fails the milestone.
```

## 7. Open questions

1. **Linger.** A viewer-only transport closes on the last detach. A 2 s
   linger would make stop→start on the same channel free but holds a tuner
   for 2 s; on a FLEX Duo that can be the difference between a refusal and
   a start for the other person. Proposed: no linger; revisit with the
   `starts_total` / `tuner_capacity` numbers after M1.
2. **`MAX_CONSUMERS_PER_TRANSPORT = 16`.** A bound so the memory ceiling
   is a number; a household will not reach it. A refusal at 16 is
   `tuner_capacity` with the channel listed as full — fine, but say so in
   the toast?
3. **A recording joining a viewer's transport** now consults the reserve
   rule at join time (§3.4). The DVR plan's wording ("needs no slot and
   does not ask") predates viewers sharing; confirm the narrowing with the
   DVR status doc's owner before M1 merges.
4. **Step 2's admission model** (one encode permit per producer, N viewers)
   changes what `plurx_live_tv_sessions` means and how `admit_live_tv`'s
   height/codec inputs are chosen when viewers differ in capability but
   not in plan. Its own document.
5. **Caption fixture provenance.** A synthesised 608/708 fixture proves the
   FFmpeg path; a broadcast capture proves the service ids the fleet
   actually sees. M3 wants both; if only one can be had, the synthesised
   one gates the code and the capture gates the advertising.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Design re-verification (§2.4) | [PR #482](http://192.168.4.7:3000/noirr/plurx/pulls/482) | Done, commit `3cb3951a8`. Every §2/§3 premise re-read at `886fc8bd` with `file:line`; eight M1 design corrections (D1–D8) and six caption findings (C1–C6). The false premises that would have broken M1: the DVR transport has never probed its prefix, so `DvrTransport.source` is always `None` (D1); DVR capacity and insertion are two lock holds, which with viewers lets a second start on one channel overwrite a live transport in the map (D2); both transport closers key on sinks and would close viewer-only transports (D3). **D7 is flagged, not decided:** the plan's own "a join never crosses a generation" guardrail, combined with DVR transports that outlive a settings save, would refuse every viewer on a recorded channel for the rest of the recording after any Live TV settings save. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 captions fixture and per-graph audit | [PR #482](http://192.168.4.7:3000/noirr/plurx/pulls/482) | Landed on the branch, commit `ee4a1752c`. Generated fixture (1080i MPEG-2, B-frames, CC1 pop-on + 708 service 1, ground truth in code), an independent `cc_data` decoder, and the production planner + live argv run end to end. In `make unit`: `the_caption_fixture_carries_608_and_708`, `the_caption_decoder_tells_dropped_and_partial_from_preserved`, `the_software_live_graph_carries_608_and_708_through_both_deinterlace_modes`, `a_copied_h264_route_carries_608_and_708`, `caption_args_per_encoder_match_the_audit_table`. Per-node audit on `nuc3` with `plurx/plurxd:latest`: jellyfin-ffmpeg 8.1.2 preserves CC1 and service 1 for software, QSV, VA-API (12 runs) and copy (2 runs), exit 0; the distro FFmpeg 5.1.9 fallback in the same image garbles CC1 under `send_field`, drops everything on VA-API, and could not start QSV, exit 101 — recorded in the status table and as §2.4 C6. No production code changed: `live_caption_args` is untouched because no audited default-engine graph needed a flag. Mutation check: giving `Encoder::Software` `-a53cc 0` in `live_caption_args` fails `caption_args_per_encoder_match_the_audit_table` and `the_software_live_graph_carries_608_and_708_through_both_deinterlace_modes` (and the existing `live_caption_forwarding_is_disabled_only_for_videotoolbox_encoding`), exit 101; restored before commit. **Not audited here:** NVENC (no node has it) and VideoToolbox's `-a53cc 1` re-proof (needs a Mac) — `needs:` below. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 | — | **Not started — blocked on Paul's decision on §2.4 D7.** D1–D6 and D8 are the corrected design M1 is to be built against; the dependency on the DVR plan's owned writers is met on `main`. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 | — | Not started: depends on M1. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 | — | Not started, deliberately: advertising needs the broadcast service ids (open question 5: "the capture gates the advertising") and today's client baseline (§2.4 C5), both device/tuner facts; and §2.4 C6 changes M4's design from a static proven-encoder set to a per-node, per-build caption probe. |
| 2026-09-24 | — | — | needs: broadcast capture | — | `needs:` real-tuner capture of three captioned channels, analysed with `scripts/live-tv-caption-audit analyze` — GPT prompt A below. |
| 2026-09-24 | — | — | needs: Mac and NVENC audit | — | `needs:` VideoToolbox audit and `-a53cc 1` re-proof on a Mac node's own FFmpeg; NVENC on any node that has it — GPT prompt B below. |
| 2026-09-24 | — | — | needs: client baseline | — | `needs:` what each client shows for in-band captions today with no master playlist — GPT prompt C below. |

#### GPT prompts for the L-03 M3 evidence

Prompt A — broadcast caption services (gates M4's service ids):

```
Live TV caption services from a real broadcast (plurx plan L-03 M3).
Use a checkout of the plurx repo at branch plan/L-03 (or main once PR #482 has
merged) on the node that owns Live TV (Settings -> Live TV names the owner
node and the HDHomeRun IPv4; call them OWNER and DEVICE). Cargo must work
there; on a Docker node add `--image plurx/plurxd:latest` to every
scripts/live-tv-caption-audit command.
1. Pick three captioned ATSC 1.0 channels from the lineup: a network
   affiliate's main MPEG-2 channel, an MPEG-2 subchannel, and an H.264
   subchannel if the lineup has one. Make sure at least one tuner is free
   (Activity page) and nothing is recording on the channel you capture.
2. For each CHANNEL (guide number, e.g. 2.1):
     curl -s --max-time 40 "http://DEVICE:5004/auto/vCHANNEL?duration=30" -o /tmp/cap-CHANNEL.ts
     ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,field_order,r_frame_rate -of csv=p=0 /tmp/cap-CHANNEL.ts
     scripts/live-tv-caption-audit analyze /tmp/cap-CHANNEL.ts
3. Report per channel, verbatim: the ffprobe line and every
   `caption-capture:` line (they carry the 608 channels CC1-CC4 and the 708
   SERVICE numbers present, with short samples). Then delete the captures.
Do not change any setting.
```

Prompt B — VideoToolbox re-proof and NVENC:

```
Live TV caption audit on a Mac and on NVIDIA (plurx plan L-03 M3).
Mac: on a Mac in the plurx fleet that runs plurxd natively with
VideoToolbox (the plan calls them maca/macb; use whichever exists), in a
checkout of plurx at branch plan/L-03 with cargo:
1. Find the ffmpeg and ffprobe that node's plurxd runs (its launchd plist or
   environment: PLURX_FFMPEG / PLURX_FFPROBE; else `which ffmpeg ffprobe`
   for the account plurxd runs as). Report `"$PLURX_FFMPEG" -version | head -1`.
2. export PLURX_FFMPEG=<that ffmpeg> PLURX_FFPROBE=<that ffprobe>
   scripts/live-tv-caption-audit videotoolbox software copy
   Expected: exit 0, every videotoolbox line cc1=dropped service1=dropped
   (the -a53cc 0 workaround), software and copy lines preserved.
3. scripts/live-tv-caption-audit --a53cc 1 videotoolbox
   This run never fails; report every `caption-audit:` and
   `caption-audit-failure:` line verbatim. A line with a non-zero exit and
   diagnostic "VideoToolbox failed while inserting A/53 captions into H.264
   SEI data" reproduces the failure (the workaround stays). Four lines with
   exit status 0 and cc1=preserved service1=preserved mean it does not
   reproduce on this FFmpeg.
NVENC: on every node, `docker logs plurxd 2>&1 | grep "usable hardware encoders" | tail -1`.
On any node showing nvenc=true, in a checkout of plan/L-03 with cargo:
   scripts/live-tv-caption-audit --image plurx/plurxd:latest nvenc
and report the lines. If no node has nvenc=true, say so.
```

Prompt C — what clients show today (the baseline M4 changes):

```
Live TV captions on clients today, before any caption advertising
(plurx plan L-03 §2.4 C5). Deploy nothing. Use a channel prompt A showed
carries CC1 text. Note the encoder label the Activity page shows for each
session (e.g. "software (x264)", "Intel QuickSync", "VA-API",
"Apple VideoToolbox").
1. Web, Chrome: play the channel 30 s, then in DevTools run
   [...document.querySelector("video").textTracks].map(t=>[t.kind,t.label,t.mode,t.cues&&t.cues.length])
   Report the output and whether captions are drawn.
2. Web, Safari on a Mac: same command; also open the player's subtitle menu
   and report what it lists.
3. Apple TV and iPhone: with Settings -> Accessibility -> Subtitles &
   Captioning -> Closed Captions + SDH on, play 60 s: are captions drawn?
   What does the player's subtitle/caption menu list? Repeat with it off.
4. Android TV (Shield) and an Android phone: with system captions on
   (Settings -> Accessibility -> Caption preferences), play 60 s: are
   captions drawn? What does Playback info say for Subtitles? Repeat off.
5. If any Live TV owner in the fleet encodes with Apple VideoToolbox, repeat
   1-4 on one session there and report any caption option that is listed
   but never shows text within 60 s (a phantom track).
Report one line per client: encoder label, track listed (yes/no + label),
text drawn (yes/no), system caption setting.
```
