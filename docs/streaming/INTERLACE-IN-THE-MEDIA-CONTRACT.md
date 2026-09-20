# Interlace in the media contract — field order as a fact, deinterlace as a decision

**Status:** ready for review · **Executes:** Q4 / §3.1.1 / F-stream-4 and
the field-rate bitrate half of Q9 / F-ltv-7 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

Read the review's §3.1.1 (the reproduced defect and the §9 commands), the
assessment's Q4, F-stream-4, Q9 and F-ltv-7 rows
([ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)),
and the Live TV planner
([`live_tv_delivery.rs`](../../crates/plurxd/src/live_tv_delivery.rs)),
which already carries `field_order` and is the precedent this plan copies
onto the file path. Work the milestones in order; one draft PR each into
`main` under the fast lane. Every `file:line` was read at `88a3957a` and is
marked **re-verify at build time**. If a step seems to require doubling the
encoded-VOD frame grid, changing a GPU graph's filter string, or treating
`field_order != progressive` as proof, stop and flag it.

Q9's caption half (A/53 SEI preservation, `CLOSED-CAPTIONS`) is **not** in
this document; it needs its own captioned-fixture audit and is not started
by anything below.

## 1. Objective

1. Carry the source's field order into the resolved media contract — the
   catalogue row, the decode facts, their digests and the plan — without a
   blanket rescan.
2. Deinterlace on the file transcode path, before `scale`, with a chosen
   frame-rate output policy, so combed fields are no longer scaled and
   encoded as progressive frames (the reproduced 90/90 combed frames tagged
   progressive).
3. Make Live TV's bitrate and reported frame rate follow the *output*
   cadence its `bwdif=send_field` graph produces, with rational arithmetic
   and the existing bandwidth caps kept.
4. Treat the flag as a hint: never deinterlace progressive material, never
   treat telecine as interlace, and verify with `idet` where the flag is
   suspect.

## 2. Contract today

```text
 ffprobe -show_streams  --> probe_json (field_order RETAINED, never parsed)
        |                                   |
        v                                   v
 parse_probe_json (:279-287)      decode_facts -show_entries (:3343)
   no field_order                    no field_order
        |                                   |
        v                                   v
 MediaFile / FileDto              DecodeFacts / FactsDigest / plan_digest
   no field_order                    no field_order, no deinterlace
        |
        v
 video_filters_for_contract (:981-1065):  scale -> [tonemap] -> format
   no deinterlace decision exists to make
```

- [`scan/probe.rs:279-287`](../../crates/plurx-core/src/scan/probe.rs) —
  the normalised video fields: `codec_name`, tag, `profile`, `width`,
  `height`, bit depth, `hdr`, `hdr_format`, `dolby_vision`. `field_order` is
  in the raw `-show_streams` document (`raw_json`, kept as
  `files.probe_json`), so existing rows already hold it — the review's
  "no blanket rescan is needed" is correct.
- [`decode_facts.rs:3343`](../../crates/plurxd/src/decode_facts.rs) — the
  held-descriptor probe's list:
  `stream=index,codec_type,codec_name,profile,pix_fmt,width,height,bits_per_raw_sample,avg_frame_rate,r_frame_rate,color_range,color_space,color_transfer,color_primaries:stream_disposition=attached_pic:stream_side_data=side_data_type`.
  No `field_order`. `CacheKey` (`:52-57`) has no probe-schema member.
- [`decode.rs:653-703`](../../crates/plurx-core/src/transcode/decode.rs) —
  `DecodeFacts` and `FactsDigest`: no scan-type field.
- [`mod.rs:1023-1024`](../../crates/plurx-core/src/transcode/mod.rs) — the
  CPU chain starts with `scale=-2:'min({h},ih)'`; GPU graphs
  ([`pipeline.rs:314-414`](../../crates/plurx-core/src/transcode/pipeline.rs))
  scale inside `vpp_qsv` / `scale_vaapi` / `libplacebo`. No deinterlacer on
  any file graph; `rg -n 'bwdif|yadif' crates/` finds only Live TV.
- [`live_tv_delivery.rs:189,212-218`](../../crates/plurxd/src/live_tv_delivery.rs)
  — `LiveSourceFacts.field_order: Option<String>` and

  ```rust
  fn interlaced(&self) -> bool {
      self.field_order.as_deref()
          .is_some_and(|order| !matches!(order, "progressive" | "unknown"))
  }
  ```

  `:715` `deinterlace: source.interlaced() && video_action == Encode`;
  `:705` `frame_rate: source.frame_rate` on the *output* description.
- [`live_tv.rs:6127-6139`](../../crates/plurxd/src/live_tv.rs) —
  `live_video_filter` pushes `bwdif=mode=send_field:parity=auto:deint=interlaced`
  first, then `scale=-2:{output_height}` — the right order. `:6110-6125`:

  ```rust
  fn live_video_bitrate_kbps(plan: &LiveDeliveryPlan) -> u32 {
      let base = match plan.output.height { 0..=480 => 2_000u64, 481..=720 => 4_000, 721..=1080 => 8_000, _ => 20_000 };
      let frame_scaled = plan.source.frame_rate.map_or(base, |rate| {
          let fps = u64::from(rate.num) / u64::from(rate.den).max(1);
          base.saturating_mul(fps.clamp(24, 60)) / 30
      });
      let bounded = plan.max_bitrate_bps.map_or(frame_scaled, |limit| frame_scaled.min(limit / 1_000));
      u32::try_from(bounded.max(500)).unwrap_or(u32::MAX)
  }
  ```

  `30000/1001` integer-divides to 29, so 1080i29.97 → 8000·29/30 = 7,733
  kbps, while `send_field` emits 59.94 fps. A true 1080p59.94 source gets
  16,000. The output description also reports 29.97.
- Encoded VOD's cadence: `frame_grid` ([`vodencode.rs:236-246`](../../crates/plurxd/src/vodencode.rs))
  takes `avg_frame_rate`/`r_frame_rate` from the probe; `frames_per_segment`
  ([`vod.rs:26,50`](../../crates/plurx-core/src/transcode/vod.rs)) is the
  nearest whole frame count to two seconds. A field-rate output would need
  a doubled grid, doubled `-g`/`-keyint_min`, and a different `fps` filter
  target — the "changes the grid" cost the review names.
- Master playlist `FRAME-RATE` comes from `HlsContext.frame_rate`, the
  source probe ([`transcode.rs:9121-9123`](../../crates/plurxd/src/transcode.rs)).
- Reproduction (review §9, re-run there on ffmpeg 6.1.1): 3 s TFF MPEG-2 at
  29.97 through `scale,format` + x264 → `idet` 90 TFF / 0 progressive,
  tagged progressive; with `bwdif=send_field` first → 59.94 fps, 4 TFF /
  176 progressive.

## 3. Change

### 3.1 Field order as a fact (M1)

`ProbeResult`/`MediaFile` gain `field_order: Option<String>` holding
ffprobe's token verbatim (`progressive`, `tt`, `bb`, `tb`, `bt`, `unknown`).
`parse_probe_json` reads it from the selected video stream. Storage: a
nullable `field_order TEXT` column on `files`, SQLite `v64` and replicated
`v43` (next free at `88a3957a`; shared with
[TONE-MAP-CHAIN-CORRECTIONS.md](TONE-MAP-CHAIN-CORRECTIONS.md) M1 if both
land in one migration — coordinate; otherwise the later one takes the next
number). `FILE_COLS` and the hiqlite projections extend together, as
`video_codec_tag` did (`FILES_VIDEO_CODEC_TAG_COLUMN`,
[`store/mod.rs:151`](../../crates/plurx-core/src/store/mod.rs)).

Backfill from `probe_json`, no media I/O: a bounded job on the
`backfill_video_codec_tags` pattern
([`state.rs:6284-6350`](../../crates/plurxd/src/state.rs); fenced `UPDATE
… WHERE id AND path AND size AND mtime AND probe_json AND field_order IS
NULL`), 256 rows per tick, stamped by `jobs.field_order_backfilled`. Rows
whose document lacks the key get `unknown`.

Decode facts: add `field_order` to the `-show_entries` list, to
`DecodeFacts`, and to `FactsDigest`, with a `PROBE_SCHEMA_VERSION` member in
`CacheKey` so an in-memory document collected under the old list is not
read as `unknown` (the LRU is process-local, but the persisted attestation
F-stream-9 is designing will not be).

Typed reading, one function used by every consumer:

```rust
pub enum ScanType { Progressive, Interlaced(FieldOrder), Unknown }
// tt | bb -> Interlaced(Tff|Bff); tb | bt -> Interlaced(TffCoded|BffCoded)
// progressive -> Progressive; unknown | absent -> Unknown
```

Live TV's `interlaced()` is rewritten in terms of it so the two paths cannot
disagree on what `unknown` means (today: not interlaced — kept).

### 3.2 Frame-rate policy (M2, decided here)

The file transcode path deinterlaces with **`bwdif=mode=send_frame:parity=auto:deint=interlaced`**,
placed before `scale` in the CPU chain, when `ScanType::Interlaced`.

Reason for `send_frame`: it preserves the source cadence, so the encoded-VOD
frame grid, `bitrate_for_height`, the ladder's `Rung` totals, the master's
`FRAME-RATE`, client decode ceilings (1080p60 needs H.264 level 4.2 and
double the decode budget on a TV box) and the prepared-handoff timeline all
stay as they are. `send_field` is the better picture for sport and the
review's Live TV graph already uses it; on the file path it is a second,
measured step (§7) because it doubles the cadence and therefore every one of
the contracts above. `deint=interlaced` rather than `all`: bwdif then
processes only frames the decoder flagged interlaced, which is what lets
soft-telecined and mixed content pass its progressive frames untouched.

Live TV keeps `send_field` — nothing in the reviewed defect says the field
rate is wrong, only that the bitrate and reported rate ignore it — and gains
a replicated setting `live_tv.deinterlace_output` (`field` | `frame`,
default `field`) surfaced in Settings → Developer with an advisory readiness
line ("software encoder on this node: 1080p59.94 H.264 is X× realtime in the
last boot probe"), never blocking. The assessment's F-ltv-7 row forbids
silently halving temporal resolution because the encoder is software; this
makes the choice explicit and the operator's.

### 3.3 Placement per graph (M2, M5)

- CPU chain (`video_filters_for_contract`): `[hwdownload,format=…,]
  bwdif=…, scale=…, [tone-map], format=…`. Software-decoded frames and
  hwdownloaded frames are both system memory, so bwdif applies to either.
- GPU graphs (`VppQsv`, `TonemapVaapi`, `Libplacebo`, `TonemapOpencl`):
  **declined for interlaced sources until M5 qualifies a hardware
  deinterlacer** — a new arm in `Pipeline::declined`
  (`pipeline.rs:512+`) returning "interlaced source — hardware deinterlace
  not qualified on this graph", so the session takes the CPU chain and the
  log says why. In practice interlaced sources are MPEG-2/H.264 8-bit SDR,
  which `heavy_source` (`mod.rs:1236-1241`) already routes to software
  decode and the CPU chain; the arm matters for the rare interlaced HEVC.
- M5 adds, after qualification on media1: `vpp_qsv=…:deinterlace=2`
  (advanced, frame-rate output) inside the VppQsv string and
  `deinterlace_vaapi=mode=motion_adaptive:rate=frame` before `scale_vaapi`.
  Each is its own PR with `idet` counts and a picture comparison against the
  CPU bwdif output on the same fixture.

### 3.4 Live TV output cadence and bitrate (M3)

In `plan_live_delivery` (`live_tv_delivery.rs:694-718`): compute
`output_frame_rate = if deinterlace && mode == Field { 2·num / den } else
{ source }` as a `LiveRational` (reduce the fraction), put **that** in
`LiveDeliveryOutput.frame_rate`, and rewrite `live_video_bitrate_kbps` to
use rational arithmetic on the output rate:

```rust
let fps_milli = output.frame_rate.map_or(30_000, |r| (u64::from(r.num) * 1_000 / u64::from(r.den).max(1)));
let frame_scaled = base.saturating_mul(fps_milli.clamp(24_000, 60_000)) / 30_000;
```

1080i29.97 with field output → 59.94 → 8000·59940/30000 = 15,984 kbps;
1080p29.97 progressive → 7,992 (today 7,733 from the integer division);
the `max_bitrate_bps` cap and the 500 kbps floor are unchanged. The
`plurx_live_tv_starts_total{outcome}` series is unaffected; the plan JSON
(`/api/v1/live/...` session detail, re-verify the route) now reports the
real output rate, which the clients' badges read.

### 3.5 Identity

- `plan_digest` gains `deinterlace` (`none` | `bwdif:send_frame` |
  `vpp_qsv:2` | `vaapi:motion_adaptive`) and `FactsDigest` gains
  `field_order`. Only sessions whose source is interlaced (or whose
  facts changed from absent to `progressive`/`unknown`) get a different
  key; the `FactsDigest` change alone moves every encoded-VOD rendition key
  whose facts now carry a field — accept it, state it in the PR, and land
  it with the tone-map M1 digest change so the fleet's caches turn over
  once, not twice. `CACHE_RECIPE_VERSION` stays 3.
- Live TV plans are not cached; nothing to invalidate.

### 3.6 Mis-flagged and telecined sources

- **Progressive content flagged interlaced**: `deint=interlaced` still
  processes those frames (the flag says so). M4 adds a bounded `idet`
  verification at first plan for `ScanType::Interlaced` sources: 10 s from
  the decode-fact probe's descriptor through
  `idet,metadata=print:file=-`, reading the final `Multi frame detection`
  line; if progressive frames dominate (> 90 %), the plan records
  `deinterlace=none` with reason `idet_progressive` and the session is
  encoded without bwdif. Result cached with the decode facts (it is a
  source fact). Counter `plurx_interlace_verdicts_total{verdict="flag_confirmed"|"flag_overruled"|"idet_unavailable"}`.
- **Hard telecine (24p in 60i)**: `idet` reports mostly progressive with
  repeated fields; bwdif with `deint=interlaced` leaves the progressive
  frames and deinterlaces the two combed ones per five. Inverse telecine
  (`fieldmatch,decimate`) is a non-goal here: it changes the cadence to
  23.976 and therefore the grid. Recorded in §7.
- **Soft telecine (repeat-field flags)**: frames decode progressive;
  nothing fires.
- **`unknown`**: treated as progressive, as Live TV does today; the M4
  `idet` check is not run for `unknown` (cost without a hint).

## 4. Guardrails (non-goals)

- **`field_order != progressive` is a hint** (assessment F-stream-4). Every
  place that acts on it either uses `deint=interlaced` (per-frame flag) or
  the M4 `idet` verdict.
- **No field-rate output on the file path in this plan**; it is §7's
  measured follow-up because it changes the grid, ladder, FRAME-RATE and
  decode ceilings.
- **Live TV's `send_field` stays default**; the setting makes the
  alternative explicit rather than silently halving rate.
- **Bandwidth caps preserved**: `max_bitrate_bps` and the 500 kbps floor
  unchanged; `Rung.peak_kbps` unchanged on the file path because the output
  cadence is unchanged.
- **No blanket rescan**: backfill reads `probe_json`; rows without the key
  become `unknown` and correct themselves on their next ordinary scan.
- **GPU deinterlace only after qualification** (M5); until then interlaced
  sources decline the GPU graph with a logged reason.
- **Captions are out of scope**: nothing here touches `-sn -dn`
  (`live_tv.rs:5992`) or advertises `CLOSED-CAPTIONS`.
- **No inverse telecine.**

## 5. Milestones

### 5.1 M1 — field order captured and carried

Column + migrations on both backends; `parse_probe_json`; backfill job;
`ScanType` reader shared with Live TV; decode-fact entry, `DecodeFacts`
field, `FactsDigest` member, `PROBE_SCHEMA_VERSION` in `CacheKey`; `FileDto`
exposes `field_order` read-only.

Tests: `cargo test -p plurx-core scan::probe` (fixtures for `tt`, `bb`,
`progressive`, `unknown`, absent); `cargo test -p plurx-core store`
migrations; `cargo test -p plurxd backfill` fenced update; `cargo test -p
plurx-core transcode::decode` digest differs on `field_order`; `cargo test
-p plurxd live_tv_delivery` unchanged verdicts for `unknown`.

Acceptance: on lab4 after deploy, `SELECT field_order, COUNT(*) FROM files
GROUP BY 1` shows no NULL after `jobs.field_order_backfilled` is stamped,
and a known 1080i DVR recording's `FileDto.field_order` reads `tt` or `bb`.

### 5.2 M2 — bwdif before scale on the CPU chain, send_frame

Filter insertion for `ScanType::Interlaced`; `Pipeline::declined` arm;
`plan_digest` `deinterlace` field; log line `deinterlace=bwdif:send_frame`.

Fixtures (generated, checked into `tests/playback/` as a script, not as
media): reuse review §9's 480-line TFF MPEG-2 fixture and add 576i25 and
1080i29.97 (`tinterlace=mode=interleave_top,setfield=tff`), a BFF variant, a
progressive 1080p control, a hard-telecined 23.976→59.94i
(`telecine=pattern=32`), and a progressive clip mis-flagged with
`setfield=tff`.

Acceptance (runs on the shipped ffmpeg, in the PR body and as an `#[ignore]`
Rust test that executes when `PLURX_FFMPEG` is set, per the repo's existing
`#[ignore]`-with-reason rule): for each interlaced fixture the produced
segment stream, concatenated and run through `idet -an -f null -`, reports
progressive ≥ 95 % of frames on the final `Multi frame detection` line and
`ffprobe -show_entries stream=avg_frame_rate` equals the *source* rate; the
progressive control's output is byte-identical to the `88a3957a` chain's
output for the same argv; the mis-flagged clip's output PSNR against the
progressive control's output is ≥ 40 dB (bwdif on genuinely progressive
frames is near-transparent — if it is not, M4's `idet` gate becomes
mandatory before M2 ships). `cargo test -p plurx-core transcode` argv
baselines updated.

### 5.3 M3 — Live TV output cadence

Planner computes the output rational; bitrate uses it; output frame rate
reported; `live_tv.deinterlace_output` setting read by the planner
(default `field`), Developer page row with advisory readiness.

Tests: `cargo test -p plurxd live_tv_delivery` — 1080i 30000/1001 with
field output plans 60000/1001 and 15,984 kbps; the same source with the
setting `frame` plans 30000/1001 and 7,992; a 1080p59.94 progressive source
is unchanged at 15,984; `max_bitrate_bps = 6_000_000` still caps to 6,000;
`cargo test -p plurxd live_video_bitrate`.

GPT prompt (fleet): "On media1 tune a 1080i channel (a network affiliate)
with Live TV → Original quality off; capture `journalctl -u plurxd | grep
'live ffmpeg'` for the session and confirm `-b:v 15984k -maxrate 23976k`
and `bwdif=mode=send_field`; read the session's plan JSON and confirm
`output.frame_rate` is 60000/1001; play it on the living-room Apple TV for
five minutes and report stalls and `plurx_live_tv_starts_total{outcome}`
before/after. Then set `live_tv.deinterlace_output=frame` in Developer and
repeat, confirming `-b:v 7992k` and 30000/1001."

Acceptance: both journal lines as stated; no increase in
`plurx_live_tv_starts_total{outcome!="ok"}` over the session.

### 5.4 M4 — `idet` verification for flagged sources

Bounded 10 s `idet` pass through the decode-fact probe primitive for
`ScanType::Interlaced`; verdict cached with the facts; counter with the
three bounded labels; plan reason `idet_progressive` overrides bwdif.

Acceptance: the mis-flagged fixture from M2 plans `deinterlace=none`; the
1080i fixture plans bwdif; `curl -s :32400/metrics | grep
plurx_interlace_verdicts_total` shows both labels after playing both.

### 5.5 M5 — hardware deinterlace after qualification (media1)

`vpp_qsv=…:deinterlace=2` and `deinterlace_vaapi` variants, each behind the
boot probe's verdict (a fixture run through the graph must produce
progressive ≥ 95 % by `idet` and match the CPU bwdif output within
`MAX_CHANNEL_DELTA`), each its own PR. Until a graph passes, the M2 decline
arm stands.

GPT prompt: "On media1 run the 1080i fixture through
`-hwaccel qsv -hwaccel_output_format qsv -vf
vpp_qsv=w=1920:h=1080:deinterlace=2:format=nv12,hwdownload,format=nv12` and
through the CPU `bwdif=send_frame,scale` chain; report `idet` summaries,
`ffprobe avg_frame_rate`, mean Y/U/V from `signalstats`, and wall time for
each."

Acceptance: the QSV graph's numbers in the PR; `Pipeline::declined` no
longer names interlace for QSV once it passes.

## 6. Verification and rollout

- Fast lane `make unit`; focused commands per milestone above. The
  `#[ignore]` ffmpeg-executing fixture test carries its reason ("needs the
  shipped ffmpeg; run with PLURX_FFMPEG set") and is run by hand in the PR.
- Metrics: `plurx_interlace_verdicts_total{verdict}` (M4);
  `plurx_live_tv_starts_total{outcome}` (existing) watched for a week after
  M3; alert owner Paul. Expected demand: Live TV sessions on interlaced
  channels, which the plan JSON now identifies.
- Settings: `live_tv.deinterlace_output` (replicated; Developer page;
  advisory readiness). No file-path switch: bwdif on a flagged-and-verified
  interlaced source is the correct output, not an option.
- Rollout: M1 (schema, coordinated) → M2 (media1 then fleet) → M3 → M4 →
  M5. M1 and the tone-map plan's M1 share a migration number if they land
  together.
- Rollback: M2/M3 are argv/planner changes; revert restores the previous
  keys and rates.

## 7. Open questions

1. **Field-rate output on the file path.** Sport and fast pans look better
   at 59.94; the cost is a doubled VOD grid (`frames_per_segment`, `-g`,
   `fps` target), a doubled ladder entry per rung, master `FRAME-RATE`, and
   a client ceiling check (`profile_max_heights`,
   [`playback/mod.rs:196`](../../crates/plurx-core/src/playback/mod.rs)).
   Proposed as a later plan with the same fixtures once M2 has shipped.
2. **Inverse telecine** for hard-telecined film on disc/DVR: out of scope;
   would change cadence to 23.976 and belongs with question 1.
3. Whether the DVR should record the planner's `field_order` beside the
   recording so a saved programme and its live session take the same path
   (§3.1.1 notes they diverge today). The column from M1 covers it once the
   recording is scanned; a DVR-side hint is optional.
4. Whether `unknown` on MPEG-2 sources should trigger M4's `idet` (MPEG-2
   broadcast often reports `unknown` at container level). Proposed: yes for
   `mpeg2video` only, decided after M1's backfill shows how many rows read
   `unknown`.
