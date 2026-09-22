# Codec and GPU qualification — widen the measured boundary, one graph at a time

**Status:** blocked: M0 one-week deployed observation · **Executes:** Q12 (§3.1.3), Q6 / F-stream-10,
Q8 / F-stream-16 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **S-11**. Companion to
[ENCODER-RATE-CONTROL-DEFAULTS.md](ENCODER-RATE-CONTROL-DEFAULTS.md) (how
the last encoder decision was measured and ratified),
[../BENCHMARKING.md](../BENCHMARKING.md) (the A/B harness and its identity
rules), [VOD-ENCODING.md](VOD-ENCODING.md) (the immutable VOD recipe and
what may not move inside it) and
[TONE-MAP-CHAIN-CORRECTIONS.md](TONE-MAP-CHAIN-CORRECTIONS.md) (what the
colour chain already got wrong once).

This is a **qualification programme**, not a set of argument edits. The
review is explicit about that in all three items: Q12 is "qualify one more
fleet-relevant codec/GPU graph end to end"; Q6 "only matters if an NVENC
node exists — open question"; Q8 is "three separate small changes, each with
its own check — not a bundle". Read §2 before proposing any flag: every
encoder default in this tree is either a recorded measurement or a recorded
refusal, and §2.5 lists which is which. **If a step seems to require
changing `Encoder::video_codec_for`'s `None` arms without a measurement on
the node in question, changing the VOD recipe's `-bf 0` / frame-grid
contract, or landing a flag whose family has not passed a probe, stop and
flag it.** Line numbers are from `0f02b7ea`; re-verify at build time by
function name.

**Correction to the review (one):** F-stream-16's first bullet says the
rolling path has "no `-g`/`-keyint_min`/`-sc_threshold 0`
(`mod.rs:1651-1653` only `-force_key_frames`)". That is true of production;
the grep that finds `-g`/`-sc_threshold` elsewhere in
`crates/plurx-core/src/transcode/mod.rs` lands at `:3937-3941`, which is
inside a test fixture builder (`write_long_gop_clip`), not a code path. The
finding stands; do not "discover" the flags already present.

## 1. Objective

1. Make codec, bit depth, dynamic range, rate control and encoder family
   explicit, independently chosen dimensions of the output contract, and
   publish source, delivered and rendered facts separately (§3.1.3's words).
2. Qualify **one** more fleet-relevant GPU graph end to end — decode,
   filters, subtitle composition, metadata, delivery, cache identity — with
   the measurement protocol in §3.5, and keep H.264 as the compatibility
   choice while doing it.
3. Settle Q6 by inventory first: NVENC and VideoToolbox argument and
   zero-copy work happens only on a node the fleet actually selects them
   on, and does not happen at all otherwise.
4. Settle each of Q8's three items on its own evidence: segment starts for
   `-sc_threshold`, client container support for HEVC-in-fMP4, and a colour
   comparison for the tone-map operator.

Board id S-11. One draft implementation PR owns the whole plan under the fast
lane. Milestones are logical commits and Execution-log rows in that PR; M0 and
M6 are measurement milestones whose deliverable includes a table in this
document.

## 2. Contract today

Re-verify line numbers at build time; they are from `0f02b7ea`.

### 2.1 Codec selection is a function of dynamic range alone

[`encoder.rs:239-252`](../../crates/plurx-core/src/transcode/encoder.rs):

```rust
    pub fn video_codec_for(self, grade: OutputGrade) -> Option<&'static str> {
        match grade {
            OutputGrade::Sdr => Some(self.video_codec()),
            // Measured: jellyfin-ffmpeg7 7.1.4-3, 2 cores, 1080p —
            // Profile 5 decode -> tonemapx passthrough -> libx265 Main10
            // veryfast at 11.0 fps, against 20.3 fps for today's SDR chain.
            OutputGrade::Hdr10 => match self {
                Encoder::Software => Some("libx265"),
                Encoder::Qsv => Some("hevc_qsv"),
                Encoder::Nvenc | Encoder::Vaapi | Encoder::VideoToolbox => None,
            },
        }
    }
```

Above it, `encoder.rs:225-238` records why the `None`s are `None`: "`None`
is a real answer and callers must handle it: it is what keeps an unmeasured
hardware claim out of production. Selecting an encoder here that nobody has
run means a viewer's first press of play is the experiment." Astra's Q12
row agrees — "A deliberate qualification boundary (the comment records the
measurement), not a defect."

There is no HEVC **SDR** output path at all on any family: `Sdr` takes
`self.video_codec()`, which is the family's H.264 encoder. Codec is
therefore coupled to dynamic range in both directions, and that coupling —
not the `None` arms — is what §3.1.3 asks to be made explicit.

### 2.2 The SDR argument lists, per family

[`encode_args_for`, encoder.rs:384-479](../../crates/plurx-core/src/transcode/encoder.rs).
The common bound is built once:

```rust
        let br = format!("{bitrate_kbps}k");
        let maxrate = format!("{}k", bitrate_kbps * 3 / 2);
        let bufsize = format!("{}k", bitrate_kbps * 2);
```

and then, per family, in full:

| Family | Base argv | Extra |
|---|---|---|
| Software | `-c:v libx264 -preset veryfast` | `-profile:v high`; `-threads N` when the pool granted a budget; `-crf q` under `Qvbr` (target omitted) |
| Nvenc | `-c:v h264_nvenc -preset p4` | `-rc vbr -cq q` under `Qvbr`. **No `-profile:v`, no `-bf`, no `-b_ref_mode`, no `-spatial-aq`/`-temporal-aq`, no `-rc-lookahead`** |
| VideoToolbox | `-c:v h264_videotoolbox` | `-q:v q` under `Qvbr`. No profile, no B-frames |
| Vaapi | `-c:v h264_vaapi` | `-rc_mode QVBR -global_quality q` under `Qvbr`; the comment at `:454-460` explains why `-rc_mode` is *not* forced under VBR |
| Qsv | `-c:v h264_qsv` | `-global_quality q` under `Qvbr` |

Every family gets `-maxrate`, `-bufsize`, the target `-b:v` (software omits
it under CRF), and a forced-IDR flag when `forced_idr` was probed.

`hdr10_encode_args` (`encoder.rs:481` onwards) is a separate path, and
`encode_args_for` refuses quality mode on the HDR10 grade for a reason
spelled out at `:375-383`: "`-crf 23` means one thing to x264 and a
different one to x265, the node sweep that produced the per-family defaults
was run against H.264 only".

### 2.3 The pipelines, and which of them stay on the GPU

[`pipeline.rs:36-113`](../../crates/plurx-core/src/transcode/pipeline.rs).
`CANDIDATES` — the list the boot probe walks — is
`[VppQsv, TonemapVaapi, Libplacebo, TonemapOpencl, Cpu]`. The three Dolby
Vision / HDR10 renderers (`DoviTonemapx`, `DoviPassthrough`,
`Hdr10Passthrough`) are deliberately outside it, each with its own probe;
`pipeline.rs:590-611` asserts that in a test.

There is **no `Pipeline::Cuda`**. `decode_args` (`pipeline.rs:243-269`)
has hardware-surface decode only for `VppQsv` (`-hwaccel qsv
-hwaccel_output_format qsv`) and `TonemapVaapi` (the VA-API pair);
everything else, including `TonemapOpencl` and `Libplacebo`, maps from
system memory or VA-API frames and the chain uploads. So an NVENC node
today decodes on the CPU, filters on the CPU (or uploads to OpenCL/Vulkan
and downloads again), and encodes on the GPU — at least two full-frame
copies per frame that a CUDA graph would not make.

`tonemap_opencl`'s operator, verbatim (`pipeline.rs:359-368`):

```rust
                    format!(
                        "{scale},format=p010,hwupload,\
                         tonemap_opencl=tonemap=hable:transfer=bt709:matrix=bt709:primaries=bt709:format=nv12,\
                         hwdownload,format=nv12"
                    )
```

Compare `Libplacebo` at `:346-353`, which already uses `bt.2390`, and
`DoviTonemapx` at `:373-380`, which already uses `bt2390`. `tonemap_opencl`
is the only operator in the file still on `hable`.

### 2.4 The rolling argument list, where Q8's first two items live

[`hls_args_inner`, mod.rs:1635-1706](../../crates/plurx-core/src/transcode/mod.rs).
After the encoder's own args, the only keyframe control is:

```rust
    // Segment-aligned keyframes so each segment is independently decodable.
    args.push("-force_key_frames".into());
    args.push(format!("expr:gte(t,n_forced*{SEGMENT_SECONDS})"));
```

and the muxer is MPEG-TS unconditionally:

```rust
            "-hls_segment_type",
            "mpegts",
```

The VOD recipe does have the grid flags
([vod.rs:272-281](../../crates/plurx-core/src/transcode/vod.rs)):
`-bf 0`, `-flags +cgop`, `-g <frames_per_segment>`,
`-keyint_min <frames_per_segment>`, `-sc_threshold 0`.

Audio on the rolling path is `-c:a aac -ac <channels> -b:a <kbps>` with no
`-ar` — Q5's finding, owned elsewhere, and not touched here.

### 2.5 What is already measured, and what is a candidate

This is the inventory a qualification programme starts from; nothing below
is re-measured without a reason.

| Fact | Status | Where it is recorded |
|---|---|---|
| QSV H.264 QVBR at q=22 | **Measured**, media1, 2026-08-14 D5 sweep; 21 and 22 byte-identical, 23 failed on +940 bytes / -0.004328 VMAF | [../OPERATIONS.md](../OPERATIONS.md) "rate control" section |
| Other families' quality defaults | **Candidates**, explicitly: "remain calibration candidates until the corpus runs on hardware that can select them; a successful 15-frame validation probe is capability evidence, not D5 calibration" | [../OPERATIONS.md](../OPERATIONS.md), same section |
| Profile 5 decode -> tonemapx -> QSV Main10 | **Measured**, media1, 2026-08-22: 1.52x realtime at 1080p, 1.26x at 2160p; 4K output probed Main 10 / yuv420p10le / PQ / BT.2020NC / limited | `encoder.rs:233-238`, `transcode.rs:27529-27538` |
| Profile 5 -> tonemapx SDR -> QSV H.264 at 2160p | **Measured**, media1, 2026-08-22: 1.37x realtime | `transcode.rs:27546-27552` |
| `HDR10_HLS_CODEC` / `HDR10_4K_HLS_CODEC` | **Measured** from the real hvcC | `transcode.rs:27580-27600` |
| `DoviPassthrough` on non-DV input | **Measured failure**: broken picture, correct tags, exit 0 | `pipeline.rs:70-76` |
| NVENC / VAAPI / VideoToolbox HDR10 | **Unmeasured**, hence `None` | `encoder.rs:249` |
| HEVC SDR on any family | **Unmeasured**, and not reachable: `Sdr` takes `video_codec()` | `encoder.rs:241` |
| `tonemap_opencl=hable` vs `bt2390` | **Unmeasured** on this pipeline | `pipeline.rs:367` |
| Whether the forced 2 s grid drifts | **Unmeasured** — the assessment's Q8 row says extra scene-cut keys "do not by themselves prove that the forced grid drifts" | `mod.rs:1652-1653` |

### 2.6 The instruments that already exist

- **`scripts/bench`** (`scripts/bench:1804-1880`) has three subcommands:
  `fixtures` (build the corpus), `run --base --token --height --seconds
  --only` (play the corpus, print a table), and `rate-control --corpus
  --modes --vmaf-ffmpeg --vmaf-model --vmaf-subsample --rate-window --json`
  (capture production HLS bytes from real plurxd sessions, then score them
  **offline** with a separate libvmaf-capable FFmpeg that never touches the
  play path). It fingerprints the scorer's `libvmaf` filter
  (`scripts/bench:174`) and behaviour-probes the model (`:368`), so a
  scoring result names the tool that produced it.
- **The corpus** (`scripts/bench:55-99`) is six shapes chosen for the
  failures they catch: `1080p-h264`, `4k-hevc-sdr`, `4k-hdr10`, `4k-hlg`,
  `sparse-gop`, `grainy`. `DURATION = 45` s each.
- **`scripts/perf2-rate-control-{smoke,n1}-corpus.json`** are the versioned
  scoring corpora, with per-fixture `identity`, `class`, `dynamic_range`,
  `trim` and `rung`.
- **`scripts/gop-census`** runs the server's own fragmented-MP4 command
  down a pipe and reports what fraction of GOP boundaries are true random
  access points. It is the instrument for Q8's first item.
- **`docs/BENCHMARKING.md`**'s harness is the A/B against Plex; its
  identity rules (same bytes, same digest, contract verified before timing)
  are the rules this programme borrows.
- **`detect_encoders`** (`encoder.rs:1137-1197`) already test-encodes each
  compiled family and then probes its quality-rate-control arguments,
  recording both in `EncoderCaps` (`encoder.rs:631-640`) — that is the hook
  every new flag in this plan hangs from.
- **`MediaNodeRuntime`** (`transcode.rs:13536-13553`) publishes
  `encoder_families`, `decoders`, `tone_map_pipelines`, `max_target_height`
  and the hardware/software slot counts per node in the cluster media
  snapshot. **There is no encoder gauge on `/metrics`** — `system.rs`'s
  exposition has cluster, raft and media counters only. M0 adds one.

```text
  source ----> decode ----> filters ----> encode ----> mux ----> HLS
               |            |             |            |
               |            |             |            +-- mpegts (rolling)
               |            |             |                fmp4   (VOD)
               |            |             +-- encode_args_for(grade, kbps, rc)
               |            +-- Pipeline: VppQsv | TonemapVaapi | Libplacebo
               |                | TonemapOpencl | Dovi* | Cpu
               +-- decode_args: hardware surfaces ONLY for VppQsv, TonemapVaapi

  qualification = every arrow measured on the target host, for one graph,
  before the graph is selectable.
```

## 3. Change

Nothing here is a flag edit that ships on its own. The shape of every
milestone is: **inventory -> corpus -> measure -> decide -> gate behind the
existing probe -> re-measure on the fleet**.

### 3.1 Make the output contract's dimensions explicit (§3.1.3)

Today an output is described by `(OutputGrade, target_height, Encoder,
Pipeline, EffectiveRateControl)`, and codec and bit depth are *derived*
from grade inside `video_codec_for`. Make them named, independently
resolvable fields of the resolved contract:

```rust
/// What this session will emit, decided once and carried as one value.
pub struct OutputCodecContract {
    pub codec: VideoCodec,          // H264 | Hevc  (Av1 later, fleet-driven)
    pub bit_depth: u8,              // 8 | 10
    pub grade: OutputGrade,         // Sdr | Hdr10
    pub rate_control: EffectiveRateControl,
    pub encoder: Encoder,
    pub pipeline: Pipeline,
}
```

The rule that keeps this safe is a single `qualified()` predicate: a
contract is selectable only if the exact tuple has a recorded measurement
on this node's family **and** a boot probe that reproduced it. The current
`video_codec_for` becomes one implementation of that predicate, with its
`None` arms preserved as "no measurement for this tuple" rather than
hard-coded refusals — same behaviour, stated as data.

Publishing source, delivered and rendered facts separately (§3.1.3's last
clause) is the same change seen from the API: `file.*` stays the source,
the session's contract is the delivered fact, and the client's own report
is the rendered fact. The delivered fact already reaches the master
playlist through S-10
([HONEST-MASTER-PLAYLIST.md](HONEST-MASTER-PLAYLIST.md)); this contract is
what S-10's `CODECS` and `RESOLUTION` should be read from once both land.

**Cache identity:** the contract is already hashed into the recipe (codec
and pipeline names are in the argument list and `Pipeline::name()` is the
stable identifier, `pipeline.rs:117`). Adding a field that does not change
argv changes nothing; adding one that does invalidates that family's cached
entries, and each milestone below says which.

### 3.2 The fleet inventory decides what is worth qualifying (Q6's condition)

Q6's remedy is conditional — "Only matters if an NVENC node exists — open
question" — and the assessment's fleet row says "Inventory configured
versus successfully selected encoders and actual sessions. Hardware
presence alone does not prove that Q6 is affecting playback."

So M0 measures, and publishes the measurement as a metric with bounded
labels:

```text
  # HELP plurx_encoder_available Whether boot validation test-encoded through this family.
  # TYPE plurx_encoder_available gauge
  plurx_encoder_available{family="software|nvenc|qsv|vaapi|videotoolbox"} 0|1

  # HELP plurx_encoder_sessions_total Sessions started on each encoder family and grade.
  # TYPE plurx_encoder_sessions_total counter
  plurx_encoder_sessions_total{family="...",grade="sdr|hdr10"} N

  # HELP plurx_tone_map_pipeline_sessions_total Sessions started on each tone-map pipeline.
  # TYPE plurx_tone_map_pipeline_sessions_total counter
  plurx_tone_map_pipeline_sessions_total{pipeline="vpp_qsv|tonemap_vaapi|libplacebo|tonemap_opencl|dovi_tonemapx|dovi_passthrough|hdr10_passthrough|cpu"} N
```

Both label sets are closed enums (`Encoder`, `Pipeline`), so cardinality is
bounded by construction; `family` and `pipeline` come from
`Encoder::name()`-equivalents and `Pipeline::name()`, never from a string
the request supplies. The values come from `EncoderCaps` and from the same
place `MediaNodeRuntime` reads. No settings key is added.

The answer to "is NVENC in use?" is then `plurx_encoder_sessions_total{family="nvenc"}`
over a week, not an inspection of hardware.

### 3.3 One fleet GPU first: QSV, and HEVC SDR where it measures a benefit

QSV is the fleet's GPU. [../OPERATIONS.md](../OPERATIONS.md) records the
2026-08-14 D5 sweep on **media1** and says "media1 performs production
encoding"; the Profile 5 / QSV Main10 measurements at `encoder.rs:233-238`
are also media1's, and `PLURX_HWACCEL: "qsv"` is the documented preference
for Arc-class GPUs. Confirm the current selection per node from M0's metric
before starting M1 — the doc is the prior, the metric is the fact.

The widening, in order of what it buys:

1. **HEVC SDR** on QSV (`hevc_qsv`, Main profile, 8-bit) for the 1080 and
   2160 rungs. Roughly 25-40 % fewer bits at equal quality is the
   general-literature expectation and is exactly what §3.5 measures rather
   than assumes. The compatibility cost is real: it is why H.264 stays the
   default. It becomes selectable only for a client whose caps prove HEVC
   in the delivered container on the delivered transport — and Q11 has
   already ruled that a fixture asserting a caps string is not decoder
   evidence, so the client side of this is a device qualification, not a
   caps-table edit.
2. **HEVC HDR** beyond today's `hevc_qsv` HDR10 point: the existing rung is
   1080p software / 1080p+2160p QSV (`hdr10_rung_fits`,
   `transcode.rs:27619-27637`). Widening means more (encoder, height)
   pairs, each with its own `HDR10_*_HLS_CODEC` measured from the real
   hvcC, and each with the level/luma-sample bound the existing code
   already enforces (`HDR10_MAX_LUMA_SAMPLES`, `transcode.rs:27572-27578`).

Neither is enabled by a flag. Each is a tuple in §3.1's `qualified()` table
whose entry is written by a measurement run, with the date and node in the
comment — the way `HDR10_HLS_CODEC`'s was.

### 3.4 NVENC arguments and a CUDA graph — only if M0 finds an NVENC node

Conditional on `plurx_encoder_sessions_total{family="nvenc"} > 0` over M0's
window. If it is zero, M3 does not happen and this section is the record of
why.

If it does happen, two separable pieces:

1. **Arguments.** Q6's list is `-profile:v high -bf 3 -b_ref_mode middle
   -spatial-aq 1 -temporal-aq 1 -rc-lookahead 20`. The assessment amends
   it: "Defaults and supported flags depend on the shipped build/GPU. Keep
   this conditional on actual fleet use and measured compatibility,
   latency, memory and throughput; do not force one NVENC recipe onto
   VideoToolbox or every HDR grade." So each flag is probed individually
   through the existing `validate`/`detect_encoders` path and recorded in
   `EncoderCaps`, and the argv carries only the subset the node's driver
   accepted. `-profile:v high` is additionally S-10's dependency
   ([HONEST-MASTER-PLAYLIST.md](HONEST-MASTER-PLAYLIST.md) §3.3) and lands
   there if it lands first.

   **`-bf 3` is refused on the VOD path and is not proposed for it.** The
   encoded fragment validator (`vodgen.rs`, the frame-grid landing check)
   rejects any sample with a nonzero composition offset, and Q2 is
   withdrawn as a flag change (§0 of the review). B-frames on the **rolling**
   path do not cross that validator, but they do change decode order inside
   a segment and therefore need their own segment-boundary proof, which is
   part of M3's acceptance and not assumed.
2. **`Pipeline::Cuda`,** behind the same boot probe as every other
   pipeline: `decode_args` returns `-hwaccel cuda -hwaccel_output_format
   cuda`, the filter segment is `scale_cuda` (+ `tonemap_cuda` where the
   build has it, else the frames come down for the tone-map and go back
   up), `init_args` names the device, and the variant joins `CANDIDATES`
   only after its probe passes. The probe is a real encode of a few frames
   through the exact production graph, like `has_dovi_passthrough`
   (`ffmpeg.rs:2408`) — not a `-filters` grep. Zero-copy is the point, so
   its acceptance is a measured reduction in host-device copies (one full
   frame each way per frame today), not merely "it ran".

VideoToolbox gets the same treatment on the Mac nodes (maca/macb) and only
if M0 shows sessions there: `-hwaccel videotoolbox -hwaccel_output_format
videotoolbox` with the `scale_vt`/`tonemap_vt` graph where the build has
it. Same probe, same acceptance, separate milestone, no shared recipe with
NVENC.

### 3.5 The measurement protocol — what "qualified" means

§3.1.3's acceptance, made runnable. For one (codec, bit depth, grade,
encoder, pipeline) tuple on one host:

| Axis | Instrument | Passing means |
|---|---|---|
| Quality per byte | `scripts/bench rate-control --modes vbr,qvbr --vmaf-model vmaf_v0.6.1 --vmaf-subsample 1` against the extended corpus | VMAF per delivered megabyte no worse than the incumbent tuple on every corpus class, and no class regresses by more than the harness's own repeat spread |
| Realtime headroom | `scripts/bench run --height <rung> --seconds 60`, the `speed` column | >= 1.25x realtime at the rung, on an otherwise idle host, and >= 1.0x with the node's documented concurrent-session ceiling running |
| GPU / CPU load | `intel_gpu_top -J` (QSV/VA-API), `nvidia-smi --query-gpu=utilization.gpu,utilization.enc,memory.used --format=csv -l 1` (NVENC), `powermetrics --samplers gpu_power` (VideoToolbox); `pidstat -p <plurxd> 1` for CPU | Reported, with the incumbent's numbers beside them. There is no pass threshold — the number is the deliverable |
| Power | Wall power at the host if a meter is available, else the GPU package power the tool above reports | Reported |
| HDR fidelity | §3.6 | Reported, and no clipping or hue regression against the incumbent |

Content classes the tuple must be measured across, from §3.1.3: **grain,
animation, dark gradients, sport, HDR10, HLG, DV variants, burns**. The
corpus has grain (`grainy`), HDR10 (`4k-hdr10`) and HLG (`4k-hlg`). It is
missing animation, dark gradients, sport, DV and a subtitle burn. M1 adds
them to `scripts/bench`'s `FIXTURES` and to a new versioned corpus JSON:

| New fixture | Shape | The failure it catches |
|---|---|---|
| `animation` | flat colour fields, hard edges, low motion, 24 fps | banding and edge ringing that grain-tuned rate control hides |
| `dark-gradient` | near-black ramps, 10-bit source | banding and black crush — the failure a tone-map operator change shows first |
| `sport` | high-motion pan, fine texture, 50/60 fps | motion estimation and the rate-control burst that `grainy` does not reach |
| `dv-p5` | Dolby Vision Profile 5 | the reshape path; the repo already carries `tests/playback/dv-p7-rpu.hex` for the P7 side |
| `burn-pgs` | 1080p + a PGS track marked for burn | the subtitle composition step, which no current fixture exercises |

Generated the same way the others are (synthetic, `ffmpeg`-built, in
`scripts/bench fixtures`), except `dv-p5`, which needs a real RPU and
therefore a checked-in sample or a documented generation recipe — §7's open
question 3.

### 3.6 HDR evaluation beyond VMAF

Q12's row is explicit: "VMAF alone cannot certify highlights or DV
conversion." VMAF is trained on SDR; scoring a PQ output against a PQ
reference with `vmaf_v0.6.1` produces a number, and the number does not
know about the highlight roll-off. So an HDR tuple's evaluation adds:

1. **Objective, HDR-aware:** a PQ-domain PSNR/SSIM pass plus an
   HDR-targeted metric where the scoring build has one (`libvmaf` with a
   PQ-transfer model, or `ssimulacra2` if the controller has it). Report
   the tool and its version, the way `scripts/bench` already fingerprints
   its scorer.
2. **Highlight and shadow census, measurable without a reference model:**
   per-frame maxima and the fraction of pixels above the knee, compared
   between reference and output. Clipping shows up here even when the
   aggregate metric does not move.
3. **Metadata survival:** MaxCLL / MaxFALL / mastering display present and
   unchanged on the output, `color_transfer`/`color_primaries`/`color_space`
   as the grade demands, no residual DV RPU where one was meant to be
   consumed. The `DoviPassthrough` doc (`pipeline.rs:64-68`) already gives
   the exact ffprobe fields to check.
4. **A human A/B on the target display**, named and dated, for the classes
   where 1-3 disagree. This is the only instrument that catches the
   `DoviPassthrough`-on-HDR10 failure mode (`pipeline.rs:70-76`: broken
   picture, correct tags, exit 0) and it is in the programme for that
   reason.

### 3.7 Q8, as three independent changes

Each has its own milestone and its own check. F-stream-16's
disposition is "Amend all six subitems"; the three that belong to this plan
are below, and the three that do not (direct-play validators, source-fence
sampling, whole-segment buffers) are named in §4 as out of scope.

**Q8a — `-sc_threshold 0` on the rolling path.** The claim to test first is
not "extra IDRs exist" but "segment starts drift off the 2 s grid". Measure
before changing: for each corpus fixture at each rung, open a rolling
session, read the published playlist's `#EXTINF` values, and probe each
segment's first frame:

```bash
# do the published segments actually start on the grid, and on an IDR?
for s in seg000*.ts; do
  ffprobe -v error -select_streams v:0 -show_entries frame=key_frame,pict_type \
    -read_intervals '%+#1' -of csv=p=0 "$s"
done
```

If every segment's first frame is a keyframe and every `#EXTINF` is within
one frame of `SEGMENT_SECONDS`, the grid does not drift and the only cost
of scene-cut IDRs is bits — a rate-control question, measured by §3.5, not
a correctness one. If it does drift, add `-sc_threshold 0` (x264) /
`-x264-params scenecut=0`, plus `-g`/`-keyint_min` at the segment's frame
count as the VOD path already does, and re-run the same census.
`scripts/gop-census` is the same instrument pointed at the source side.
**This changes recipe identity** for the rolling path if it lands.

**Q8b — HEVC in fMP4 for the rolling HDR10 rung.** Apple's authoring spec
wants HEVC in fMP4; the rolling path is MPEG-TS
(`mod.rs:1696-1697`). The gate is *client container support*, not the spec:
switching `-hls_segment_type` to `fmp4` for the HEVC grade changes what
every client must accept, introduces an `init.mp4` on a path that has never
had one (which S-10's §3.2 then reads), and changes the segment file
extension the playlist emits. The check is a device matrix — the same three
televisions, Apple TV, iPhone, and the three browsers — run on a build
where only the HDR10 rolling rung is fMP4. A single client that regresses
sinks it; the HDR10 rolling rung is a *fallback* path (the VOD path is
already fMP4), so the cost of not doing it is small and the cost of
breaking it is a black screen on the fallback.

**Q8c — the tone-map operator.** `tonemap_opencl=tonemap=hable` is the odd
one out (§2.3). Change it to `bt2390` only with a colour comparison, not
because the other two filters use it: different filters implement the same
named curve differently, and [TONE-MAP-CHAIN-CORRECTIONS.md](TONE-MAP-CHAIN-CORRECTIONS.md)
plus Q3's disposition both warn against substituting an operator across
filters blind. The comparison is §3.6's items 1-3 plus a side-by-side on
the `4k-hdr10`, `4k-hlg` and new `dark-gradient` fixtures, on a node where
`tonemap_opencl` is the selected pipeline. **This changes recipe identity**
(`Pipeline::name()` is unchanged but the filter string is in the argv), so
it invalidates cached entries produced on OpenCL nodes.

## 4. Guardrails (non-goals)

- **No flag ships without a probe on the family that gets it.** Q6's
  disposition: "Defaults and supported flags depend on the shipped
  build/GPU." Every argument added in M3/M4 is probed individually through
  `detect_encoders`/`validate` and recorded in `EncoderCaps`; a family
  whose probe refuses keeps today's argv exactly.
- **One NVENC recipe is not a VideoToolbox recipe** (Q6's disposition,
  verbatim: "do not force one NVENC recipe onto VideoToolbox or every HDR
  grade"). M3 and M4 are separate milestones with separate measurements,
  and neither touches the HDR10 argument path (`hdr10_encode_args`).
- **B-frames are not a flag here either.** Q2 is withdrawn; the VOD
  validator refuses nonzero composition offsets; F-stream-10's disposition
  says "B-frames also require the independent VOD timeline change." `-bf 3`
  appears only in M3, only on the rolling path, only with a segment-boundary
  proof, and never on VOD.
- **Extra IDRs do not prove grid drift** (Q8's disposition, and
  F-stream-16's). Q8a measures the grid before it changes an argument, and
  reports "no drift" as a legitimate outcome that closes the item.
- **HEVC fallback packaging and tone-map algorithms each need their own
  qualification** (F-stream-16's disposition). Q8b and Q8c are separate
  milestones with separate acceptance; neither rides on the other, and
  neither rides on Q8a.
- **H.264 stays the compatibility choice** (Q12's row). Nothing in this
  plan changes the default codec for a client that has not proven HEVC on
  the delivered container and transport. Q11's rule applies: "a fixture
  asserting the new string is not decoder evidence."
- **AV1 is later and fleet-driven** (Q12's row). It is not in any
  milestone. §7 records why.
- **A valid argument string is not device acceptance** (the assessment's
  copy/remux row). Every milestone whose output reaches a client ends in a
  device run, written as a GPT prompt in §6.
- **The three F-stream-16 subitems that are not ours:** direct-play
  validators (C11, and its "must preserve auth and file identity"
  disposition), source-fence sampling (F-stream-9/16 — "do not rate-limit
  the final source-fence check across immutable publication"), and
  whole-segment buffers (owned by
  [MEDIA-BODY-BUFFERS.md](MEDIA-BODY-BUFFERS.md)). Not in scope.
- **No feature gate.** Paul refuses in-code gates. Selection is decided by
  the boot probe plus the `qualified()` table, both of which are facts, not
  switches. Where an operator genuinely needs to force a family, the
  existing `PLURX_HWACCEL` preference already exists and is advisory — a
  preference the probe may refuse (`../OPERATIONS.md`: "If you set `qsv`
  but see software selected, the QSV probe was rejected").
- **No claimed magnitude.** "HEVC saves 30 %" is not a finding until §3.5
  measures it on this corpus on this host. Every PR body carries its
  numbers or does not merge.

## 5. Milestones

### 5.1 M0 — fleet inventory: configured versus selected versus used

Code: §3.2's three metrics, in `system.rs`'s exposition beside the existing
families, fed from `EncoderCaps` and the accepted-session boundaries. The
counter advances after manager registration for rolling, after VOD reader
attachment, or after the first publishable Live TV inventory crosses its
serving fence. It does not claim first-media publication for rolling or VOD.
Bounded labels only. No behaviour change.

Then one week of reset-aware collection, then the GPT prompt in §6 to read it
off every node. These counters are process-local. A final zero from a direct
scrape is never week-long absence evidence: use a continuously scraped
Prometheus `increase(...[7d])` and verify the exact `plurx_build_info` series
throughout that interval, or retain start/end scrapes and prove
`plurx_uptime_seconds` was uninterrupted for the entire interval. Any restart
invalidates the second method and restarts its seven-day window.

Acceptance: a scrape from each current node prints one
`plurx_encoder_available` line per family and a
`plurx_encoder_sessions_total` counter that advances once at the accepted
start boundary; focused rolling, VOD and Live TV seam tests prove one increment
and pre-boundary failure/replay non-increments; `cargo test -p plurxd
metrics_encoder` green; `make unit` green; and, one week later, the §6 table
filled from reset-aware evidence for every node, stating for each: families
compiled, families the probe accepted, family actually selected, sessions
per family and grade, tone-map pipelines used.

**This milestone decides whether M7 and M8 exist at all.**

#### 2026-09-21 pre-instrumentation fleet inventory

The maintained Ansible inventory currently names four Plurx nodes (`nynuc`,
`m6`, `nuc4`, `nuc3`), rather than the historical host list in §6. The table
below comes from read-only SSH discovery against those four nodes. It separates
an encoder present in FFmpeg's build from one accepted by the boot probe. An
unset preference means the existing automatic ordering selects the first
accepted family; it is not an operator enable switch.

| Node | GPU / kernel driver | FFmpeg build exposes | Boot probe accepts | Automatic selection / tone-map | Historical use available? |
|---|---|---|---|---|---|
| `nynuc` | Intel Arrow Lake-P Arc Pro 130T/140T · `i915` · Linux `7.0.0-31-generic` | H.264/HEVC NVENC, QSV, VA-API | software, QSV, VA-API | QSV / `vpp_qsv` | No: container started `2026-09-21T04:18:48Z`; M0 metrics absent |
| `m6` | AMD Phoenix1 · `amdgpu` · Linux `7.0.0-31-generic` | H.264/HEVC NVENC, QSV, VA-API | software, VA-API | VA-API / CPU fallback | No: container started `2026-09-21T04:20:03Z`; M0 metrics absent |
| `nuc4` | Intel Alder Lake-P Iris Xe · `i915` · Linux `7.0.0-31-generic` | H.264/HEVC NVENC, QSV, VA-API | software, QSV, VA-API | QSV / `vpp_qsv` | No: container started `2026-09-21T04:27:49Z`; M0 metrics absent |
| `nuc3` | Intel Alder Lake-P Iris Xe · `i915` · Linux `7.0.0-31-generic` | H.264/HEVC NVENC, QSV, VA-API | software, QSV, VA-API | QSV / `vpp_qsv` | No: container started `2026-09-21T04:20:40Z`; M0 metrics absent |

All four images are `plurx/plurxd:latest`; none sets `PLURX_HWACCEL` or
`PLURX_TONEMAP`. NVENC's symbols are compiled into the shipped FFmpeg, but its
boot probe fails with `Cannot load libcuda.so.1` and no node has NVIDIA
hardware. VideoToolbox is neither compiled nor probe-accepted because the
inventory has no macOS Plurx node. Consequently M7 and M8 do not exist for
this fleet snapshot: adding either path would invent hardware support. A later
fleet change reopens that decision through a new M0 observation, not through a
feature gate.

The existing image exports none of the three M0 metric families, and its
pre-change VOD logs do not preserve the resolved encoder as a countable
session field. Recent container logs therefore cannot reconstruct a truthful
one-week use table. This PR adds the bounded process counters, but M0 remains
open until that image is deployed and continuously scraped for one reset-aware
week. The direct-scrape fallback is valid only when start/end evidence proves
uninterrupted uptime; otherwise its window restarts. M1-M6 do not begin on an
invented baseline.

### 5.2 M1 — extend the corpus to §3.1.3's content classes

Code: five new `FIXTURES` entries (§3.5) in `scripts/bench`, a new
`scripts/codec-qualification-corpus.json` at schema version 1 with
`identity`/`class`/`dynamic_range`/`trim`/`rung` rows matching the existing
corpus shape, and the `dv-p5` generation recipe (or its checked-in sample —
§7 question 3).

Acceptance: `scripts/bench fixtures --dir ./bench-media` builds all eleven
fixtures with no error; `ffprobe` of each new fixture reports the intended
`color_transfer`/`color_primaries`/frame rate; `scripts/bench rate-control
--corpus scripts/codec-qualification-corpus.json --modes vbr` completes
against a dev server and writes its JSON; the corpus JSON is committed and
`make unit` is unaffected.

### 5.3 M2 — the explicit output-codec contract

Code: §3.1. `OutputCodecContract` plus `qualified()`, with
`video_codec_for` reimplemented on top of it so behaviour is identical.
Pure refactor: the same tuples are selectable before and after.

Tests:

| Test | Asserts |
|---|---|
| `the_qualified_table_matches_todays_selection` | for every (Encoder, OutputGrade) pair, `qualified()` agrees with `0f02b7ea`'s `video_codec_for` |
| `an_unqualified_tuple_is_never_selectable` | HEVC SDR on every family is refused until its row exists |
| `the_contract_is_in_the_recipe_identity` | two contracts differing only in codec hash differently |
| `the_delivered_facts_are_separate_from_the_source_facts` | a session's contract does not read `file.video_codec` |

Acceptance: `cargo test -p plurx-core output_codec_contract` and
`cargo test -p plurxd recipe_identity` green; `make unit` green; no cached
entry invalidates (the argv is byte-identical — assert that in a test that
hashes the argument list before and after).

### 5.4 M3 — Q8a: does the rolling grid drift?

Measurement first, code only if it does. Run the §3.7 Q8a census on media1
across the corpus at 360/480/720/1080, plus two real library titles (a
grain-heavy film and a fast-cut one, named by hash not title).

If no drift: record the table here under a dated heading, and close Q8a
with "measured, no change" — that is a complete outcome.

If drift: add `-sc_threshold 0`, `-g` and `-keyint_min` to `hls_args_inner`
(mirroring `vod.rs:276-281`), re-run the census, and state the recipe
invalidation in the PR body.

Acceptance either way: the census table in this document with per-fixture
`#EXTINF` spread, keyframe-at-start rate, and (if changed) the same three
after. `cargo test -p plurx-core hls_args_inner` green; `make unit` green.

### 5.5 M4 — Q8c: the tone-map operator, compared

Only on a node where `tonemap_opencl` is the selected pipeline. §3.6 items
1-3 plus a side-by-side on `4k-hdr10`, `4k-hlg` and `dark-gradient`,
`hable` versus `bt2390`, same source bytes, same rung, same encoder.

Acceptance: the comparison table here (metric, highlight census, metadata
row, and the human A/B's verdict with the display named); a change lands
only if `bt2390` is not worse on any class and better on at least one.
Recipe invalidation for OpenCL nodes stated in the PR body.

### 5.6 M5 — Q8b: HEVC fMP4 for the rolling HDR10 rung

Code: `-hls_segment_type fmp4` for the HEVC grade only, in `hls_args_inner`,
plus the segment filename and init-segment handling that implies. Then the
device matrix in §6.

Acceptance: every device in the §6 matrix plays the HDR10 rolling rung to
completion with no container error; `mediastreamvalidator` clean on that
session's playlist; `cargo test -p plurx-core hls_args_inner` green;
`make unit` green. Any single device failure reverts the change and the
result is recorded here.

### 5.7 M6 — qualify HEVC SDR on QSV, end to end

The §3.5 protocol for `(Hevc, 8, Sdr, Qsv, VppQsv)` at 1080 and 2160, on
media1, against the M1 corpus, with H.264/QSV as the incumbent. Then the
client half: prove HEVC-in-fMP4 and HEVC-in-TS acceptance on each target
device before any client may be offered the tuple.

Acceptance: the §3.5 table filled for every axis and every content class;
the client table filled; the `qualified()` row added with the date, node
and measurement in its comment; `cargo test -p plurx-core
output_codec_contract` green. If quality per byte does not improve on this
corpus at this rung, the row is **not** added and the negative result is
recorded here — that is also a completed milestone.

### 5.8 M7 — NVENC arguments and `Pipeline::Cuda` (conditional on M0)

Runs only if M0 found NVENC sessions. Code: §3.4, both pieces, each flag
probed and recorded in `EncoderCaps`; `Pipeline::Cuda` joins `CANDIDATES`
only behind its own boot probe.

Acceptance: per-flag probe verdicts on the node; the §3.5 table for
`(H264, 8, Sdr, Nvenc, Cuda)` against `(H264, 8, Sdr, Nvenc, Cpu)`;
measured host-device copies per frame before and after (from
`nvidia-smi dmon` or the ffmpeg filter graph's own `hwupload`/`hwdownload`
count in the argv, whichever the build supports); the segment-boundary
proof if `-bf 3` is included; `make unit` green.

### 5.9 M8 — VideoToolbox zero-copy (conditional on M0)

Same shape as M7, on maca/macb, with `powermetrics` as the power
instrument. Separate milestone and measurements, no shared recipe with M7.

## 6. Verification and rollout

Fast lane for the plan PR: `make unit`. Focused per milestone as named in §5.
`make benchmark-check` still gates the committed A/B coverage
([../BENCHMARKING.md](../BENCHMARKING.md)); M1's new fixtures do not enter
that matrix and must not silently change it.

**GPT prompt — fleet encoder inventory (M0, after one reset-aware week):**

```text
On the maintained Plurx nodes `nynuc`, `m6`, `nuc4`, and `nuc3`, with the
exact candidate build running:
1. From the continuously scraped Prometheus history, report seven-day
   `increase()` values for `plurx_encoder_sessions_total` and
   `plurx_tone_map_pipeline_sessions_total`, grouped by instance and their
   closed labels. Verify that `plurx_build_info` names the candidate build for
   the whole interval. Counter resets are included by `increase`; a node with
   missing scrape history or another build is incomplete, not zero.
2. If Prometheus history is unavailable, retain start and end outputs of
   `curl -fsS http://<host>:32400/metrics | grep -E
   'plurx_encoder_available|plurx_encoder_sessions_total|plurx_tone_map_pipeline_sessions_total|plurx_build_info|plurx_uptime_seconds'`.
   Accept the counter delta only when the same build and uninterrupted uptime
   prove the process spans the full seven days; otherwise restart the window.
3. `curl -fsS -H "Authorization: Bearer $TOKEN"
   http://<host>:32400/api/v1/system` — report the Hardware pills, the
   Transcoder line (selected encoder and PLURX_HWACCEL preference), and the
   ffmpeg version string.
4. `ffmpeg -hide_banner -encoders | grep -E 'nvenc|qsv|vaapi|videotoolbox'`
   on each host, to separate "compiled in" from "probe accepted".
5. If the host has a GPU: `intel_gpu_top -l -s 1000 | head -5` (Intel),
   `nvidia-smi --query-gpu=name,driver_version --format=csv` (NVIDIA), or
   `system_profiler SPDisplaysDataType | head -20` (Mac).
Report one row per host: compiled families, probe-accepted families,
selected family, sessions per family and grade over the week, tone-map
pipelines used, GPU model and driver version.
```

**GPT prompt — rolling segment-grid census (M3):**

```text
On media1, with the current plurxd and a library containing the
scripts/bench corpus:
For each of 1080p-h264, 4k-hevc-sdr, 4k-hdr10 and grainy, and for rungs
360, 480, 720 and 1080:
1. Start a rolling transcode session (POST /api/v1/files/{id}/hls with
   `height`), let it publish at least 30 segments.
2. Save the playlist and list every #EXTINF value.
3. For each published segment, run
   `ffprobe -v error -select_streams v:0 -show_entries frame=key_frame,pict_type
    -read_intervals '%+#1' -of csv=p=0 <segment>`
4. Report per fixture and rung: number of segments, min/median/max EXTINF,
   how many segments started on a key frame, and the largest deviation from
   2.000 s.
Also run `scripts/gop-census` on two real library files (report the file
hash, not the title) and paste its verdict.
```

**GPT prompt — HEVC fMP4 rolling device matrix (M5):**

```text
With a build carrying the S-11 M5 change deployed to media1:
Play a Dolby Vision or HDR10 title forced onto the ROLLING HDR10 rung (the
fallback path, not the cached VOD path) on each of: Apple TV 4K, iPhone,
Lenovo Android TV, Google TV, Shield, an Android phone, Safari, Chrome,
Firefox.
For each: does it start, time to first frame, does it play 2 minutes
without error, and the exact error text if not. Also run Apple's
`mediastreamvalidator` against that session's master.m3u8 and paste the
summary.
Report one table. Name the OS/browser versions.
```

**GPT prompt — HEVC SDR client acceptance (M6):** the same device list,
playing an HEVC SDR transcode at 1080 and 2160 in both fMP4 and MPEG-TS,
with the same four columns.

Rollout: one draft plan PR into `main`, with logical milestone commits and
Execution-log rows, then the fast lane. Metric names
and labels are fixed by §3.2 and are the only observability surface added.
No settings key is added; `PLURX_HWACCEL` and `PLURX_TONEMAP` keep their
current meaning. Cache identity, per milestone: M0 and M2 invalidate
nothing (M2 asserts byte-identical argv in a test); M3 invalidates rolling
entries **if** it changes the argument list; M4 invalidates entries
produced on OpenCL nodes; M5 invalidates rolling HDR10 entries; M6's new
`qualified()` row creates a new contract rather than invalidating an old
one; M7 and M8 invalidate their own family's entries. Each PR body states
which, because a silent invalidation looks like a cache bug.

Rollback: every milestone reverts independently. The two with a rollback
cost are the ones that invalidate a cache twice (M3 and M4); they deploy to
media1 alone for a week first.

## 7. Open questions

1. **Does the fleet select NVENC or VideoToolbox at all?** M0 answers it
   and M7/M8 exist or do not on that answer. Stated here so a later reader
   does not mistake their absence for an oversight.
2. **What is `hevc_qsv`'s SDR quality default?** The D5 sweep that produced
   q=22 was H.264/QSV. `encode_args_for`'s own comment (`:375-383`) refuses
   to carry a quality number across codecs, which is right. M6 either
   sweeps `hevc_qsv` the same way or ships it bitrate-bounded; the sweep is
   a day and the answer is worth having, but it is a decision, not a
   default.
3. **Where does the `dv-p5` fixture come from?** A real Profile 5 RPU
   cannot be synthesised by `ffmpeg` the way the other fixtures are. The
   repo carries `tests/playback/dv-p7-rpu.hex` for the Profile 7 case.
   Options: check in a small P5 sample, document a generation recipe using
   `dovi_tool`, or measure DV on a named library file by hash. Paul's call
   on whether a binary sample belongs in the tree.
4. **Which HDR-aware metric?** §3.6 item 1 names a PQ-domain
   PSNR/SSIM and "an HDR-targeted metric where the scoring build has one".
   Which one, and whether the scoring FFmpeg on the controller has it, is
   unknown from the tree; `scripts/bench` currently hard-requires
   `vmaf_v0.6.1` for its acceptance mode (`RATE_CONTROL_ACCEPTANCE_MODEL`,
   `scripts/bench:107`). Extending that gate is part of M1 if the metric is
   chosen before M1 ships, otherwise M6.
5. **AV1.** Q12's row says "AV1 later, fleet-driven". It is not in this
   plan because no fleet node has an AV1 encoder in M0's inventory as
   written, and because the client side (which devices decode AV1 in which
   container) is a larger qualification than the server side. Revisit when
   M0's inventory changes.
6. **Does `-rc-lookahead` interact with the forced-keyframe expression?**
   `-force_key_frames expr:...` and a lookahead window both decide where
   keys go. If M7 happens, its segment-boundary proof must cover it; nobody
   has looked.

---

## Execution log

Executing sessions append one row per logical milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M0 | [`17dfebc4` / #422](http://192.168.4.7:3000/noirr/plurx/pulls/422) | Implemented five-family availability, family/grade accepted-start, and eight-pipeline counters with closed enum labels. Count points are manager registration for rolling, reader attachment for VOD, and first publishable/fenced Live TV inventory (encoder only; Live TV currently refuses tone-map-required routes). Read-only inventory found QSV/VA-API nodes only; M7 NVENC and M8 VideoToolbox are refused for this fleet. Review correction: the seven-day gate is reset-aware and bound to the exact build; focused production-seam tests cover rolling, VOD and Live TV once-only/pre-boundary behavior. Needs: deploy and collect one valid reset-aware week before M1-M6. |
