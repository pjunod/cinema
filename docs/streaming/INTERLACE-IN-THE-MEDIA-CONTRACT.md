# Interlace in the media contract — field order as a fact, deinterlace as a decision

**Status:** open — M1–M4 merged (#417); M5 measured 2026-10-02 (§9): hardware deinterlace **not adopted**, CPU bwdif stays; routing by the M4 verdict still open (§8) · **Executes:** Q4 / §3.1.1 / F-stream-4 and
the field-rate bitrate half of Q9 / F-ltv-7 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read the review's §3.1.1 (the reproduced defect and the §9 commands), the
assessment's Q4, F-stream-4, Q9 and F-ltv-7 rows
([ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)),
and the Live TV planner
([`live_tv_delivery.rs`](../../crates/plurxd/src/live_tv_delivery.rs)),
which already carries `field_order` and is the precedent this plan copies
onto the file path. Work the milestones in order in one whole-plan draft PR
to `main` under the fast lane. Every `file:line` was read at `88a3957a` and is
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
  Each is its own logical M5 commit in the plan PR with `idet` counts and a
  picture comparison against the CPU bwdif output on the same fixture.

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
boot probe's verdict, each its own PR. Until a graph passes, the M2 decline
arm stands.

> **Superseded 2026-10-02 (§9).** The bars originally written here — `idet`
> ≥ 95 % progressive and mean Y/U/V within `MAX_CHANNEL_DELTA` of CPU bwdif —
> cannot judge a deinterlacer and are replaced by §9.3. Measured against
> them, neither hardware graph is adopted (§9.4).

GPT prompt: "On media1 run the 1080i fixture through
`-hwaccel qsv -hwaccel_output_format qsv -vf
vpp_qsv=w=1920:h=1080:deinterlace=2:format=nv12,hwdownload,format=nv12` and
through the CPU `bwdif=send_frame,scale` chain; report `idet` summaries,
`ffprobe avg_frame_rate`, mean Y/U/V from `signalstats`, and wall time for
each."

Acceptance: the QSV graph's numbers in the PR; `Pipeline::declined` no
longer names interlace for QSV once it passes. Judged by §9.3, not by the
two bars above.

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
4. ~~Whether `unknown` on MPEG-2 sources should trigger M4's `idet`.~~
   **Closed 2026-10-02: no.** A read-only audit of media1's catalogue after
   the M1 backfill found no `mpeg2video` row reading `unknown`; FFprobe
   reports a field order for every MPEG-2 file there, so the extra pass would
   cost a probe for no row. The rows that do read `unknown` are the
   key-omitting codecs (HEVC above all) and audio-only files. Reopen only if
   a catalogue shows MPEG-2 `unknown` rows.

## 8. Execution decisions

- SQLite v64 and replicated v43 were still the next free migrations after
  rebasing onto `main` at `665b8b5c`; the implementation uses those numbers.
- The resolved-plan version is 2 because adding the deinterlace decision
  changes artifact identity. The facts cache projection is version 3 after M4
  because the cached value now includes the content verdict as well as
  `field_order`.
- `live_tv.deinterlace_output` is intentionally independent of the Live TV
  configuration-generation compare-and-swap. It changes subsequent encoder
  planning, not tuner discovery, and the Developer control stays editable
  while Live TV is enabled. Its prerequisites and unmeasured encoder status
  are advisory and never disable the control.
- The first corrected misflagged fixture measured 35.29 dB after bwdif, below
  M2's 40 dB shipping threshold. M4 therefore became mandatory for M2. The
  verifier reuses the startup-snapshotted FFprobe, the exact held source
  descriptor and its single offset lane; it does not introduce a pathname
  reopen or a second executable identity. The final `idet` summary must be
  strictly more than 90 percent progressive, including undetermined frames in
  the denominator, before the field-order hint is overruled.
- Descriptor-bound probing is production-supported on Unix/Linux. Windows
  records `idet_unavailable` and retains the conservative field-order decision
  until its held-handle execution path can provide the same invariant.
- M5 has no safe implementation without the plan's media1 QSV/VAAPI fixture
  measurements. No hardware filter string or hidden enable gate was added.
- **2026-10-02 — the M1 `NULL` defect, fixed.** The scanner wrote `NULL`
  when FFprobe omitted `field_order` (typical for HEVC) while the backfill
  wrote `unknown` for the same document, so 81 rows scanned on media1 after
  `jobs.field_order_backfilled` was stamped stayed `NULL` forever and
  identical media stored a different token depending on which path wrote it.
  `parse_probe_json` is now the single owner of "probed, no field order": it
  reports `unknown` whenever the selected playable video has no token,
  including audio-only and cover-art-only files (the token the backfill
  already wrote for those rows), so the SQLite and Hiqlite upserts and the
  background probe job's facts document all store the same value; `NULL`
  now means only "never probed". The backfill was re-armed under new keys
  (`jobs.field_order_backfilled_v2`, `jobs.field_order_backfill_v2_cursor`)
  and, in the transaction that stamps it done, deletes the first pass's
  stamp and both passes' node-local cursors (`SettingsStore::put_setting_retiring`).
  **Identity:** no artifact key moves. The catalogue column feeds only
  `ScanType::from_field_order`, which reads `NULL` and `unknown` alike, so
  the deinterlace decision and `plan_digest` are unchanged; `FactsDigest`
  hashes the held-descriptor or retained FFprobe document's own
  `field_order` (absent in both routes for these sources), not the column;
  `DecodeCacheIdentity` is id/size/mtime. The only digest that sees the
  column is the per-session rolling-provenance binding, which serialises the
  whole row (`scanned_at` included) and so moves once for each backfilled
  row, as it does on any rescan.
- **2026-10-02 — routing by the M4 verdict: not done; the gap, precisely.**
  The hardware graph is declined twice. First at request time, in
  `options_for_tone_map` (`manager/plan.rs`, `Pipeline::for_session_with_scan`
  → `declined_with_scan`, `pipeline.rs`), from the catalogue's
  `file.field_order` — before any decode facts exist, so no `idet` verdict
  can be there. Second in `resolve_transcode` (`decode.rs`, the
  `Deinterlace::BwdifSendFrame` guard), from `facts.scan_type()`, which *is*
  verdict-aware — but that guard can only downgrade a GPU request to the
  software renderer, never restore one the first guard already removed. So
  a mis-flagged *heavy* source (HEVC 4K/HDR flagged `tt`, which M4
  overrules) still loses its GPU graph: correct picture, CPU renderer,
  slower. Dropping the request-time guard is not enough on its own, because
  the request options outlive the resolve: the colour-safe retry is prepared
  from the request's `opts.pipeline` (`PrepublicationTranscodeRetry::prepare`
  in `manager/start.rs`; a GPU request would "retry" onto the exact plan it
  already got), and the start log and session descriptor print
  `opts.pipeline` (`start.rs`, `construct.rs`). The fix is to make every
  post-resolve reader take `plan.options().pipeline` (retry preparation, log,
  descriptor, and the other `live_lookup_options` call sites in
  `create.rs`, `candidates.rs`, `produce.rs`) and then drop the scan argument
  from the request-time guard, leaving `resolve_transcode` the sole,
  verdict-aware authority. That is a restructuring of the request/plan
  boundary, not a routing tweak, so it is recorded here instead of forced.
  Practical reach today: interlaced sources are MPEG-2/H.264 8-bit, which
  `heavy_source` keeps on the CPU chain anyway; only a mis-flagged heavy
  HEVC file pays.

## 9. M5 evidence and decision (2026-10-02)

Measured on lab3 (Alder Lake-P iGPU, iHD VA-API driver with libva 1.24,
oneVPL 2.17) with the shipped image's FFmpeg 8.1.3, production thread caps
(`-threads 6 -filter_threads 6`), timing including decode and
`hwdownload`, no encoder. The private receipt with source paths is kept off
the repository; the numbers below are its.

### 9.1 Inputs

| Label | Source | Source `idet` (multi-frame) |
|---|---|---|
| broadcast-1080i | H.264 High 1920×1080 30000/1001 broadcast recording, `tt` | 100 % TFF at two offsets |
| dvr-480i | MPEG-2 704×480 30000/1001 DVR recording, `tt` | 99.4–100 % TFF |
| fixture-1080i | generated `testsrc2` → `tinterlace=interleave_top,setfield=tff` → MPEG-2 | 100 % TFF |
| misflagged-1080 | H.264 1080 30000/1001, every frame `interlaced_frame=1` | 97–100 % progressive |

### 9.2 Results (60 s windows; fixture whole file)

| Graph | `idet` % progressive on output (1080i / 480i / fixture) | Mean Y/U/V Δ vs CPU | Picture vs CPU bwdif, SSIM / PSNR | Wall vs CPU (1080i) | CPU-seconds per minute of 1080i |
|---|---|---|---|---|---|
| CPU `bwdif=send_frame` (reference) | 78.8 / 83.2 / 100 | — | — | 1.00 (4.9 s) | 27.5 |
| No deinterlace at all (weave) | 0 / 0.3 / 0 | 0.23 / 0.09 / 0.07 | 0.974 / 34.8 dB | — | — |
| QSV `vpp_qsv deinterlace=2` | 35.7 / 64.1 / 98.7 | 0.08 / 0.02 / 0.04 | 0.9971 / 51.5 dB (1080i); **one field ahead** on dvr-480i (26.1 dB as-is, 43.9 dB re-aligned) | 0.89× (5.45 s) | 4.4 |
| VA-API `deinterlace_vaapi motion_adaptive` | 37.1 / 65.4 / 98.7 | 0.29 / 0.05 / 0.02 | **one field late** on all three (30.6 dB as-is); 0.9972 / 52.2 dB re-aligned | 0.58× (8.4 s) | 5.0 |

Also measured: `motion_compensated` and `default` are byte-identical to
`motion_adaptive` on this driver; `deinterlace_qsv=mode=advanced` defaults to
field rate (59.94), unlike `vpp_qsv=…:deinterlace=2` (frame rate, as the plan
intends); hardware outputs drop one edge frame. The misflagged source through
bwdif without M4's overrule measures PSNR-Y 52.5 dB / SSIM 0.9966 against
untouched — mild softening, which M4 removes.

### 9.3 Bars that can judge a deinterlacer (replace §5.5's)

What failed as written: `idet` ≥ 95 % progressive fails real content *for
the CPU reference itself* (78.8 %, 83.2 %) while every graph passes the
generated fixture, because `idet` on deinterlaced real video reacts to
residual line structure; and "mean Y/U/V within `MAX_CHANNEL_DELTA` (24)"
passes the un-deinterlaced weave at 0.23, so it cannot tell a deinterlacer
from none. A 1.2× wall-time rule (the boot probe's `MIN_SPEEDUP`) applied to
deinterlace alone rejects both GPU paths at these resolutions, though they
use 5–6× less CPU. A future hardware candidate is judged, per source in
§9.1 (the real recordings, not only the fixture), by:

1. **Field timing first.** PSNR against CPU `bwdif=send_field` with the
   reference shifted by −1, 0 and +1 field (`setpts=PTS±1/(2·rate)/TB`): the
   0 shift must be the best of the three on every source. A constant offset
   is a timing defect even when the picture is good, and it is
   source-dependent (QSV was aligned on 1080i and ahead on 480i).
2. **Picture, time-aligned.** SSIM ≥ 0.995 and mean PSNR ≥ 45 dB against
   CPU `bwdif=send_frame` on pts-synchronised frames, with fewer than 1 % of
   frames below 30 dB.
3. **`idet` relative to the reference, not absolute.** Output progressive
   share no more than 10 points below the CPU reference's on the same
   window; the generated fixture keeps the absolute ≥ 95 % as a boot-probe
   sanity check only.
4. **Cost reported, not gated.** Wall time and CPU-seconds beside the CPU
   chain, measured feeding the hardware encoder directly (not via
   `hwdownload`); the operator decides what CPU saved is worth.
5. Cadence: output `avg_frame_rate` equals the source (frame mode), and the
   frame count is within one of the reference.

### 9.4 Decision

**Hardware deinterlace is not adopted; the CPU `bwdif=send_frame` chain
stays the only file-path deinterlacer**, and the decline arm stands for all
four GPU graphs. VA-API fails bar 1 on every source (constant one-field,
16.7 ms, video lag). QSV fails bar 1 on the DVR recording (one field ahead)
and bar 2 there as a consequence, while passing on the broadcast 1080i — a
source-dependent timing, which a boot probe on one fixture cannot certify.
Neither is faster in wall time here, so nothing is lost by declining: an
interlaced source is still encoded by the hardware *encoder*, fed after the
CPU chain (`hwupload` suffix). A QSV adoption would need bar 1 met on a
wider DVR corpus or a per-source phase check; neither is planned.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M1 | [#417](http://192.168.4.7:3000/noirr/plurx/pulls/417) (`4fc76aa3b`) | Field order captured, migrated and exact-fence backfilled on SQLite/Hiqlite; shared conservative `ScanType`, decode-fact/cache identity and read-only DTO covered by focused probe, migration and store-contract tests. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M2 | [#417](http://192.168.4.7:3000/noirr/plurx/pulls/417) (`4fc76aa3b`, acceptance corrected in `a55bad845`) | CPU `bwdif=send_frame` precedes scale; interlaced GPU graphs decline to CPU; resolved-plan identity/logging and generated TFF/BFF/progressive/telecine/misflag fixtures are executable. M4 is part of this shipping boundary because the unverified misflag measured 35.29 dB. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M3 | [#417](http://192.168.4.7:3000/noirr/plurx/pulls/417) (`4fc76aa3b`) | Exact rational output cadence drives reported frame rate and bitrate (15,984/7,992 kbps); cap preserved; replicated Developer setting saves independently and remains advisory-only. Fleet playback/journal observation remains post-deploy evidence. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M4 | [#417](http://192.168.4.7:3000/noirr/plurx/pulls/417) (`a55bad845`) | Ten-second-media `idet` pass uses the snapshotted FFprobe and held descriptor under the existing overall deadline, output caps, cancellation/reap and offset ownership. Verdict is cached/digested, strict >90% progressive overrides bwdif, and three fixed metric labels are exported. Real FFmpeg/FFprobe descriptor acceptance confirms both overrule and confirmation. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | M5 | [#417](http://192.168.4.7:3000/noirr/plurx/pulls/417) | **needs:** run the §5.5 media1 QSV/VAAPI prompt and record idet, cadence, signalstats and wall-time comparisons. Hardware graphs remain conservatively declined; no unqualified filter was added. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/s01_builder | Review disposition | [#417 comment 3298](http://192.168.4.7:3000/noirr/plurx/pulls/417#issuecomment-3298) (`9789ca4ed`, merged with `6063b37c0` in `3fd0d8ac1`) | Both P1 findings resolved: the three special Dolby Vision/HDR10 graphs apply the recorded bwdif decision before scale without moving Dolby Vision reshape, and Live TV selects one complete final H.264 limit only after output cadence is known. Focused argv, 30/60 fps capability, rational bitrate, affected compile and all-target Clippy evidence is green. M5 remains honestly blocked on media1 qualification. |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M1 defect + M5 | branch `opus/s08-interlace-continuation` (unpushed) | `parse_probe_json` owns `unknown`; backfill re-armed under v2 keys and retires the first pass's stamp and cursors in its completing write; store contract on both backends. M5 measured on lab3 (§9): hardware deinterlace not adopted, CPU bwdif stays; §5.5 bars replaced by §9.3. Open question 4 closed. Routing by the M4 verdict recorded as a gap (§8), not forced. |
