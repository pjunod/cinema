# Tone-map chain corrections — an explicit peak, primaries before the curve, dither

**Status:** ready for review · **Executes:** Q3 / F-stream-3 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read the review's §3.1 row Q3, the assessment's Q3 and F-stream-3 rows
([ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)),
and the module header of
[`pipeprobe.rs`](../../crates/plurxd/src/pipeprobe.rs) — the CPU chain this
plan edits is the *reference* every GPU graph is judged against at boot, so
a change here moves the yardstick. Work M0 → M3 in order; each milestone is
one draft PR into `main` under the fast lane. Every `file:line` was read at
`88a3957a` and is marked **re-verify at build time**. If a step seems to
require changing a GPU graph's operator (`bt.2390` in libplacebo,
`tonemap=1` in vpp_qsv, `hable` in tonemap_opencl), stop and flag it: those
belong to
[CODEC-AND-GPU-QUALIFICATION.md](CODEC-AND-GPU-QUALIFICATION.md).

**Correction to the review:** the review and appendix describe two LIKELY
failure modes for the CPU chain when no `peak=` is given — a "dark" map at
`peak=100` when only the SEI is lost, and highlight clipping at `peak=1.0`
when `color_trc` is also lost. Neither can occur in the shipped chain. By the
time `tonemap` runs, the frame has passed `zscale=…:t=linear`, so its
`color_trc` is *linear*, and FFmpeg's fallback (`ff_determine_signal_peak`,
identical in 6.1 and in the 8.0 release source) is `100.0` **only** for
`smpte2084` and `10.0` for everything else. Measured here on ffmpeg 6.1.1
with the exact production filter string (§2.2): with MaxCLL/MDCV side data
present the peak follows it (MaxCLL 4000 → 40); with side data absent or
stripped the peak is 10.0, i.e. the 1,000-nit default the review proposes
as policy. So the *variance* the review worries about is real but narrower:
the picture differs only between "MaxCLL-driven" and "1,000-nit assumed",
and only when a source's MaxCLL is not 1,000 and the side data does not
reach the filter. The plan below still makes the peak explicit — an implicit
default that happens to be right is not a contract — but the severity and
the acceptance criteria are written against what actually happens.

## 1. Objective

1. Establish, on the shipped build and the real hardware-download path,
   whether MDCV/CLL side data reaches `tonemap` (M0) — the fact everything
   else is conditional on.
2. Store the source's peak luminance facts at scan (M1) so the chain can be
   told the peak instead of inferring it, with a stated 1,000-nit policy
   default when the source carries none.
3. Emit `peak=` in the chain's own units, move the gamut conversion ahead of
   the tone curve, and dither the final 8-bit quantisation (M2), each
   proved by a before/after image comparison rather than by argv.
4. Keep the GPU graphs' boot verdicts stable through the change (M3): the
   CPU chain is their reference, and a reference that moved silently would
   re-select or de-select hardware on the next boot without anyone deciding.

## 2. Contract today

### 2.1 The chain and where it runs

[`mod.rs:1052-1056`](../../crates/plurx-core/src/transcode/mod.rs), inside
`video_filters_for_contract`, for `ToneMap::Zscale` when `input_is_hdr`:

```text
scale=-2:'min({h},ih)',
zscale=tin={smpte2084|arib-std-b67}:min=bt2020nc:pin=bt2020:t=linear:npl=100,
format=gbrpf32le,
tonemap=tonemap=hable:desat=0,
zscale=p=bt709:t=bt709:m=bt709:r=tv,
format=yuv420p
```

The comment above it (`:1041-1046`) declares the *input* tags explicitly
because "Hardware decode (QSV/VAAPI) frequently drops color metadata across
the hwdownload". It fixes zscale's input; it says nothing to `tonemap`.

Who reaches this chain (`options_for_tone_map`,
[`transcode.rs:14576-14665`](../../crates/plurxd/src/transcode.rs);
`Pipeline::for_session` / `declined`,
[`pipeline.rs:487-540`](../../crates/plurx-core/src/transcode/pipeline.rs)):

- every node whose boot probe selected `Pipeline::Cpu` (no proven GPU graph
  — the lab nodes today per
  [DECODER_SELECTION_RECOVERY_STATUS.md](../DECODER_SELECTION_RECOVERY_STATUS.md)
  "FFmpeg and client qualification gaps");
- on a node with a proven graph: HLG sources (`VppQsv`/`TonemapVaapi`
  handle only `hdr10`/`hdr10plus`), Dolby Vision that is not
  HDR10-compatible, and "light" sources — but `heavy_source`
  (`mod.rs:1236-1241`) is HEVC ∧ (HDR ∨ ≥2160p), so every HDR HEVC source is
  heavy and a light HDR source is a non-HEVC one (VP9/AV1 HDR).

Decode side (`decode_setup_with_compatibility`, `mod.rs:1248-1298`): QSV and
VA-API decode heavy sources to hardware surfaces and prepend
`hwdownload,format=p010le` (`:1277-1294`); NVENC/VideoToolbox decode into
system memory; software decode otherwise. **So the hardware-download + CPU
tone-map combination is exactly: HLG on media1/lab3/lab4 (QSV proven), any
HDR source on a QSV/VA-API node whose GPU graph failed its probe, and a
non-compatible DV source there.** On a software-decode node the side data
is whatever the software decoder attached.

### 2.2 What `tonemap` does with no `peak=` — measured

FFmpeg's `vf_tonemap` calls `ff_determine_signal_peak(in)` when `peak` is 0.
The 8.0 release source (`libavfilter/colorspace.c`, read 2026-09-20):

```c
    // For untagged source, use peak of 10000 if SMPTE ST.2084
    // otherwise assume HLG with reference display peak 1000.
    if (!peak)
        peak = in->color_trc == AVCOL_TRC_SMPTE2084 ? 100.0f : 10.0f;
```

after MaxCLL, then mastering `max_luminance`, each divided by
`REFERENCE_WHITE` (100). Units are therefore multiples of 100 nits, which is
also what `npl=100` on the preceding zscale makes 1.0 mean.

Run here (ffmpeg 6.1.1-3ubuntu5, software decode, generated PQ fixtures,
production filter string, `-loglevel debug` to read `Computed signal peak`):

| fixture | side data | frame trc at `tonemap` | computed peak |
|---|---|---|---|
| PQ, no CLL/MDCV | none | linear | **10.0** |
| PQ, MaxCLL 1000 | CLL+MDCV | linear | 10.0 |
| PQ, MaxCLL 4000 | CLL+MDCV | linear | **40.0** |
| PQ, MaxCLL 4000, `sidedata=mode=delete` before zscale | stripped | linear | **10.0** |
| PQ, no CLL, `setparams=color_trc=smpte2084` injected after zscale | none | smpte2084 | 100.0 |

Output bytes for the no-CLL fixture with no `peak=` were MD5-identical to
`peak=10`; for the MaxCLL-4000 fixture identical to `peak=40`; `peak=1`
and `peak=100` each produced a different picture (mean luma 111 and 78
against 84). Commands are in §5.1 so M0 can repeat them on the shipped
build.

Two consequences for the plan: (a) the only way the shipped chain maps
"dark" is a frame that reaches `tonemap` still tagged `smpte2084`, which the
`zscale=t=linear` step prevents; (b) `peak=` in this chain is
`nits / 100`, so MaxCLL 1000 → `peak=10`, and the 1,000-nit default the
review proposes is `peak=10` — the same value the fallback already yields.

### 2.3 Facts the scanner keeps

[`scan/probe.rs:190-240`](../../crates/plurx-core/src/scan/probe.rs) runs
`ffprobe -v error -print_format json -show_format -show_streams
-show_chapters`; `parse_probe_json` (`:243-316`) reads the video stream's
`codec_name`, `profile`, `width`, `height`, `bits_per_raw_sample`/`pix_fmt`,
`color_transfer` and `side_data_list` (for DOVI) — nothing about content
light level or mastering luminance. The raw document is retained as
`files.probe_json`. Measured here: for an MKV whose MDCV/CLL exist only as
HEVC SEI (libx265 output), `-show_streams` carries **no** mastering or
content-light entries; `-show_frames -read_intervals '%+#1'` does
(`side_data_type: "Mastering display metadata"` with `max_luminance`
`"10000000/10000"`, and `"Content light level metadata"` with
`max_content`, `max_average`). A container that carries the metadata
itself (Matroska `MasteringMetadata`, MP4 `mdcv`/`clli`) exposes it at
stream level. So a backfill from `probe_json` alone recovers the peak only
for container-tagged files; SEI-only files need one bounded frame probe.

[`decode_facts.rs:3343`](../../crates/plurxd/src/decode_facts.rs) — the
held-descriptor probe's `-show_entries` list names
`stream_side_data=side_data_type` only; `DecodeFacts`
([`decode.rs:653-674`](../../crates/plurx-core/src/transcode/decode.rs))
has no luminance field and `FactsDigest` (`:686-703`) none to feed.

### 2.4 The boot probe that uses this chain as its reference

[`pipeprobe.rs:1-30`](../../crates/plurxd/src/pipeprobe.rs): each GPU graph
must (1) run, (2) tag BT.709, (3) match the CPU reference's mean Y/U/V within
`MAX_CHANNEL_DELTA = 24.0` (`:52`), and (4) be ≥ `MIN_SPEEDUP = 1.2` faster
(`:43`), on a generated 1.5 s 4K HDR10 fixture (`:54-61`). The fixture is
PQ-tagged HEVC produced by the probe itself; whether it carries CLL is part
of M0's reading. Candidates: `CANDIDATES` (`pipeline.rs:107-113`) —
VppQsv, TonemapVaapi, Libplacebo, TonemapOpencl, Cpu.

## 3. Change

### 3.1 M0 — the hardware-download metadata test (a reading, not a change)

On media1 (QSV) and on any VA-API node that exists, with the shipped
container's jellyfin-ffmpeg 8, two fixtures (with and without CLL/MDCV;
`scripts/bench fixtures` builds `4k-hdr10` without, and this plan adds
`4k-hdr10-cll` with `master-display=…:max-cll=4000,1000`), three readings
each:

1. **Metadata after download** — `showinfo` immediately after
   `hwdownload,format=p010le`: which `side data -` lines survive, and the
   frame's colour properties.
2. **Effective peak** — the production chain with `-loglevel debug`, hardware
   decode versus `-hwaccel none`: the `Computed signal peak` values.
3. **Picture** — `signalstats` mean/max Y of the two decode paths through
   the identical chain; identical means the download changed nothing that
   `tonemap` reads.

M0's result decides how much of M1/M2 is corrective and how much is
contractual. The plan proceeds either way, because an inferred peak is not a
contract even when it infers correctly.

### 3.2 M1 — peak luminance facts at scan

New nullable columns on `files`: `max_cll INTEGER` (cd/m², from
`max_content`), `max_fall INTEGER`, `mastering_max_luminance INTEGER`
(cd/m², from `max_luminance` rational × 1/10000), `luminance_source TEXT`
(`stream` | `frame` | `none`). `ProbeResult`/`MediaFile` gain the same
fields; `FILE_COLS` ([`sqlite/mod.rs:1218-1223`](../../crates/plurx-core/src/store/sqlite/mod.rs))
and the hiqlite projection
([`hiqlite_media.rs:36,439`](../../crates/plurx-core/src/store/hiqlite_media.rs))
extend together. Migrations: SQLite `v64` appended to `MIGRATIONS`
(`sqlite/mod.rs:1117-1128`, currently ends at v63) and replicated `v43`
after `CONTENT_ANALYSIS_REPAIR_SCHEMA_VERSION = 42`
([`hiqlite.rs:99-102`](../../crates/plurx-core/src/store/hiqlite.rs)), both
`ALTER TABLE files ADD COLUMN …`, both following the `video_codec_tag`
precedent (`FILES_VIDEO_CODEC_TAG_COLUMN`,
[`store/mod.rs:151`](../../crates/plurx-core/src/store/mod.rs); v60 / v40).
Version numbers are the next free ones at `88a3957a` — re-verify at build
time.

Scan: `parse_probe_json` reads stream-level `side_data_list` entries of
type `Mastering display metadata` and `Content light level metadata`. When
`color_transfer` is `smpte2084` or `arib-std-b67` and neither is present,
the scanner runs one additional bounded probe on the same file —
`-select_streams v:<index> -show_frames -read_intervals '%+#1'
-show_entries frame_side_data=side_data_type,max_content,max_average,
max_luminance` — through the bounded primitive C12 introduces
(`ffmpeg.rs::bounded_command_output_with_limits`; if C12 has not landed,
this milestone waits for it rather than adding another unbounded spawn).
Decoding one 4K HEVC frame is bounded work; the scan already pays an
ffprobe per file.

Backfill without a rescan: a bounded job modelled on
`backfill_video_codec_tags`
([`state.rs:6284-6350`](../../crates/plurxd/src/state.rs)) and
`files_missing_video_codec_tag` / `set_file_video_codec_tag`
(`hiqlite_media.rs:3054-3122`): rows with `hdr IS NOT NULL AND
luminance_source IS NULL`, 256 per tick, fenced by path/size/mtime/
`probe_json`. It recovers stream-level values from `probe_json` at no I/O
cost and marks `luminance_source = 'none'` when the stored document lacks
them — it does **not** open media. SEI-only files are then picked up by
the next ordinary rescan of that file, or by the decode-fact route below.

Decode facts: add `stream_side_data=max_content,max_average,max_luminance`
to the `-show_entries` list at `decode_facts.rs:3343`; `DecodeFacts` gains
`max_cll`, `mastering_max_luminance`; `FactsDigest` feeds them. Because
`CacheKey` (`decode_facts.rs:52-57`) does not include the probe schema, add
a `PROBE_SCHEMA_VERSION` constant to it so a cached document from the old
argument list is never read as "no luminance". The in-memory LRU
(`MAX_CACHE_ENTRIES = 256`) empties on deploy anyway; the constant is for
the persisted attestation F-stream-9 is designing.

### 3.3 M2 — the chain

```text
scale=-2:'min({h},ih)',
zscale=tin={tin}:min=bt2020nc:pin=bt2020:t=linear:npl=100,
format=gbrpf32le,
zscale=p=bt709,
tonemap=tonemap=hable:desat=0:peak={peak},
zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,
format=yuv420p
```

- `peak={peak}` where `peak = max_cll / 100` when `max_cll` is known, else
  `mastering_max_luminance / 100`, else **`10`** — the 1,000-nit policy
  default, stated as policy in the recipe (`tone_map_peak` field) and in
  the session's playback-info. The review's assessment calls the default
  "a policy assumption, not source truth"; the plan says so wherever the
  value is printed. HLG sources keep FFmpeg's own HLG handling: the same
  `10` (1,000-nit reference display) unless the source states otherwise.
- `zscale=p=bt709` **before** `tonemap`: FFmpeg's documented chain and
  Jellyfin's `GetSwTonemapFilter` map gamut in linear light before the
  curve; mapping after compresses out-of-709 values that are then clipped,
  which shifts hue on saturated highlights. The trailing zscale drops `p=`.
- `dither=error_diffusion` on the final zscale: the float→8-bit step is
  where tone-mapped gradients band. Cost is measured in M2's acceptance, not
  assumed small.
- Not changed: the operator (`hable`), `desat=0`, `npl=100`. The assessment
  forbids substituting the `bt2390` spelling across different filters, and
  `tonemap` has no `bt2390` operator (its set is none/linear/gamma/clip/
  reinhard/hable/mobius — `ffmpeg -h filter=tonemap`).

Identity: the plan digest gains `tone_map_peak` (the number and its
provenance: `cll` | `mdcv` | `default`), and the filter string changes for
every `ToneMap::Zscale` session, so every CPU-tone-mapped recipe key moves.
`CACHE_RECIPE_VERSION` stays 3; invalidation is by mismatch. Encoded-VOD
renditions on the CPU chain get new keys; existing renditions are untouched.
The tone-map field already in the digest (`decode.rs:2098-2106`) is
unchanged.

### 3.4 M3 — the boot probe after the reference moved

`pipeprobe` compares GPU graphs to this chain. After M2 the reference
picture has explicit peak, gamut-before-curve and dither; GPU verdicts may
move within the 24-level tolerance or across it. M3 re-runs the probe on
media1 (QSV → VppQsv), records the before/after `speedup` and channel deltas
per candidate, and only then deploys. If a graph that passed at `88a3957a`
fails against the new reference, that is a finding about the graph or the
tolerance, resolved in CODEC-AND-GPU-QUALIFICATION.md — never by loosening
`MAX_CHANNEL_DELTA` in this PR.

## 4. Guardrails (non-goals)

- **No GPU graph changes here.** `vpp_qsv=tonemap=1`, `tonemap_vaapi`,
  `libplacebo=bt.2390`, `tonemap_opencl=hable` stay as they are.
- **No operator change on the CPU chain.** `hable` stays; a different curve
  is a picture decision with its own comparison.
- **Do not synthesise MDCV/CLL for the HDR10 grade** — `hdr10_encode_args`
  ([`encoder.rs:489-503`](../../crates/plurx-core/src/transcode/encoder.rs))
  records why: "Deriving one would mean inventing numbers." Storing the
  source's values (M1) is the honest input for that gap later; it is not
  used to emit metadata in this plan.
- **The default is policy and is labelled as policy.** `peak=10` with
  `provenance=default` appears in the recipe, the log line and playback
  info. A reviewer must be able to tell "the source said 1,000" from "we
  assumed 1,000".
- **No blanket rescan.** M1's backfill reads `probe_json`; only files whose
  document lacks the facts and whose transfer is PQ/HLG get a bounded frame
  probe, and only when they are next scanned or planned.
- **Do not widen `MAX_CHANNEL_DELTA` or lower `MIN_SPEEDUP`** to keep a
  verdict.
- **Do not rate-limit or skip the decode-fact probe** to pay for the new
  entries; F-stream-9's disposition owns that.

## 5. Milestones

### 5.1 M0 — metadata retention on the shipped build (fleet reading)

GPT prompt (media1, inside the running container so the build is the
shipped one):

```bash
# Fixtures: the bench 4k-hdr10 (no CLL) plus one with CLL/MDCV.
F=/mnt/nas/media/plurx-perf2/bench-media
ffmpeg -hide_banner -nostdin -y -f lavfi -i 'testsrc2=size=3840x2160:rate=24:duration=3' \
  -pix_fmt yuv420p10le -c:v libx265 -x265-params \
  'log-level=none:hdr-opt=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(40000000,50):max-cll=4000,1000' \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc $F/4k-hdr10-cll.mkv
CHAIN="scale=-2:'min(1080,ih)',zscale=tin=smpte2084:min=bt2020nc:pin=bt2020:t=linear:npl=100,format=gbrpf32le,tonemap=tonemap=hable:desat=0,zscale=p=bt709:t=bt709:m=bt709:r=tv,format=yuv420p"
for f in 4k-hdr10 4k-hdr10-cll; do
  # 1. what survives the download
  ffmpeg -hide_banner -nostdin -init_hw_device qsv=hw -filter_hw_device hw \
    -hwaccel qsv -hwaccel_output_format qsv -i $F/$f.mkv -frames:v 1 \
    -vf hwdownload,format=p010le,showinfo -f null - 2>&1 | grep -E 'side data|color_' 
  # 2. effective peak, hardware vs software decode
  for hw in "-init_hw_device qsv=hw -filter_hw_device hw -hwaccel qsv -hwaccel_output_format qsv" "-hwaccel none"; do
    ffmpeg -hide_banner -nostdin $hw -i $F/$f.mkv -frames:v 3 -loglevel debug \
      -vf "$( [ "$hw" = "-hwaccel none" ] || echo 'hwdownload,format=p010le,')$CHAIN" \
      -f null - 2>&1 | grep -m1 'Computed signal peak'
  done
  # 3. picture equivalence
  for hw in "-init_hw_device qsv=hw -filter_hw_device hw -hwaccel qsv -hwaccel_output_format qsv" "-hwaccel none"; do
    ffmpeg -hide_banner -nostdin $hw -i $F/$f.mkv -frames:v 24 \
      -vf "$( [ "$hw" = "-hwaccel none" ] || echo 'hwdownload,format=p010le,')$CHAIN,signalstats,metadata=mode=print:key=lavfi.signalstats.YAVG:file=-" \
      -f null - 2>/dev/null | grep YAVG | tail -1
  done
done
# Also record: ffmpeg -version | head -1, and the same three readings with
# -hwaccel vaapi -hwaccel_output_format vaapi on any VA-API node.
```

Report the table: fixture × decode path × (side-data lines, computed peak,
YAVG). Also read the boot probe's own fixture: `ffprobe -show_frames
-read_intervals '%+#1' -show_entries frame_side_data=side_data_type
<data_dir>/pipeprobe/*.mkv` (path re-verified from `pipeprobe::fixture`).

Acceptance: a filled table in the M0 PR body (docs-only PR), each cell an
observation, plus the same three readings from this document's §2.2 re-run
on the shipped build. The M2 default is confirmed or corrected from it.

### 5.2 M1 — luminance facts

Columns, migrations on both backends, `parse_probe_json` stream-level
extraction, bounded frame probe for PQ/HLG files lacking it, backfill job,
decode-fact entries and digest, `FileDto` exposure (read-only).

Tests: `cargo test -p plurx-core scan::probe` with fixture JSON for
container-tagged (stream side data), SEI-only (frame side data), and none;
`cargo test -p plurx-core store` migration tests on both backends (v64/v43
apply from v63/v42; older binary refuses newer schema); `cargo test -p
plurxd backfill` for the fenced UPDATE; `cargo test -p plurx-core
transcode::decode` for `FactsDigest` including the new fields (two documents
differing only in `max_content` hash differently).

Acceptance: after deploy, `sqlite3 plurx.db "SELECT luminance_source,
COUNT(*) FROM files WHERE hdr IS NOT NULL GROUP BY 1"` on a lab node shows
no NULL rows once the backfill stamps `jobs.luminance_backfilled` (key
named after `jobs.video_codec_tag_backfilled`,
[`store/mod.rs:1853`](../../crates/plurx-core/src/store/mod.rs));
`journalctl -u plurxd | grep 'luminance backfill: complete'` present.

### 5.3 M2 — the chain, with a before/after image protocol

Filter change; `tone_map_peak` in options and digest; the log line and
playback-info carry `peak_nits` and `peak_source`.

Image comparison protocol (run on lab4 with `-hwaccel none` so decode is
not a variable; repeated on media1 for the QSV hwdownload path):

1. Sources: `4k-hdr10` (no CLL), `4k-hdr10-cll` (4000), a generated
   gradient ramp (`-f lavfi -i 'gradients=size=1920x1080:duration=2'`
   encoded PQ), and one real HDR10 disc remux Paul names (Harbor Lights),
   frames at 10 %, 50 %, 90 %.
2. Before: the `88a3957a` chain. After: §3.3's chain with the stored peak.
3. Readings per frame: `signalstats` YAVG/YMAX/YMIN; a histogram
   (`histogram` filter to PNG); `psnr`/`ssim` between before and after to
   quantify the change; visual stills side by side in the PR.
4. Banding: on the gradient source, count distinct 8-bit luma levels across
   a horizontal line (`-vf crop=1920:1:0:540,format=gray -f rawvideo` piped
   through `sort -u | wc -l`) before and after dither.
5. Cost: `-benchmark` wall time before/after at 1080p on lab4; the added
   zscale pass and dither must stay within 5 % or the PR says why it is
   worth more.

Tests: `cargo test -p plurx-core transcode::` argv baselines updated with
the new filter string and a new `the_cpu_tone_map_names_its_peak` test
asserting `peak=10` with `default` provenance for a source with no facts,
`peak=40` for MaxCLL 4000, `peak=` from mastering luminance when only MDCV
exists; the `assert_no_pq_at_8_bit` guard still passes.

Acceptance: the PR carries the stills, the level counts (after > before on
the ramp), `psnr` numbers, the cost delta, and `cargo test -p plurx-core
transcode` green.

### 5.4 M3 — boot-probe re-qualification

Deploy M2 to media1 only; capture `GET /api/v1/system` `pipeline` report
before and after; compare `verdicts[*].speedup` and rejection text; then the
rest of the fleet.

GPT prompt:

```text
On media1 before and after the M2 deploy: curl -s
http://media1:32400/api/v1/system | jq .pipeline  — save both. Report each
candidate's passed/speedup/rejected. Then play Harbor Lights (HDR10) to the
living-room Apple TV in SDR output mode and to Chrome on the laptop, and
note pipeline= in journalctl for each session and whether the picture
brightness matches the M2 stills.
```

Acceptance: the selected pipeline is unchanged on media1 or the change is
explained by a verdict delta recorded in the PR; no session logs
`pipeline=cpu` for an HDR10 source on media1 where `vpp_qsv` previously
served it.

## 6. Verification and rollout

- Fast lane: `make unit`; focused: `cargo test -p plurx-core scan::probe`,
  `cargo test -p plurx-core transcode`, `cargo test -p plurxd decode_facts`,
  `cargo test -p plurxd backfill_video_codec_tags` (pattern for the new
  job's tests).
- Metrics: `plurx_tone_map_peak_total{source="cll"|"mdcv"|"default"}`
  (counter, three bounded labels) incremented at recipe capture; alert
  owner Paul; expected demand: every CPU-tone-mapped session; observation
  window one week after M2. A fleet where `default` dominates says the
  scan facts are not reaching sessions.
- Settings: none new. The 1,000-nit default is code policy, printed as such.
- Rollout: M0 (docs) → M1 (schema, both backends, coordinated deploy as
  every schema change is) → M2 (media1 first, then fleet) → M3 verdict
  record. One PR each.
- Rollback for M2: revert the filter string; recipe keys return to their
  previous values.

## 7. Open questions

1. Should the HLG branch keep FFmpeg's HLG handling or receive the same
   explicit `peak=`? This plan passes `peak=` on both because the value is a
   source fact when known; if M0 shows HLG sources rarely carry it, the
   default `10` matches FFmpeg's own HLG reference display.
2. Whether `luminance_source = 'none'` after a frame probe should be
   retried on a later scan (a broken frame probe would otherwise pin
   "none" forever). Proposed: retry when `probe_json` changes, not
   otherwise.
3. Whether the `desat=0` choice should be revisited alongside the gamut
   order: it is deliberately out of scope; note it if the M2 stills show
   hue shifts that gamut-before-curve does not remove.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
