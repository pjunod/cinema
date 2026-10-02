# Codec and GPU qualification — widen the measured boundary, one graph at a time

**Status:** open: M1 corpus/M2 contract; sixteen internal M3 cells recorded,
original qualification open, 2026-10-02 · **Executes:** Q12 (§3.1.3), Q6 / F-stream-10,
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

**2026-09-30 prerequisite amendment:** Paul's ruling makes the organic-use
week supplementary observation, not eligibility for M1 or M2. Current scoped
hardware, compiled-family, boot-probe and selected-graph evidence determines
which graphs to investigate. A zero process counter does not prove absence.
All content, exact graph/device, quality, throughput and HDR acceptance bars
below remain unchanged; no new HEVC default or qualification follows from
this amendment. M7/M8 are inapplicable only for an inventory that actually
proves the relevant hardware/family absent, and reopen when that inventory
changes. Meaningful manual choices remain advisory, never readiness gates;
automatic contract correctness does not acquire an unfinished-feature toggle.

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

The supplementary answer to "how much organic NVENC use was observed?" is
reset-aware `plurx_encoder_sessions_total{family="nvenc"}` over a week.
Eligibility to begin corpus/contract work instead uses the current scoped
inventory: hardware, compiled families, accepted boot probes and selected
graphs. Missing history is unknown, and a final zero is not absence evidence.

### 3.3 One fleet GPU first: QSV, and HEVC SDR where it measures a benefit

QSV is the fleet's GPU. [../OPERATIONS.md](../OPERATIONS.md) records the
2026-08-14 D5 sweep on **media1** and says "media1 performs production
encoding"; the Profile 5 / QSV Main10 measurements at `encoder.rs:233-238`
are also media1's, and `PLURX_HWACCEL: "qsv"` is the documented preference
for Arc-class GPUs. Confirm current accepted families and selected graphs
from scoped M0 inventory before widening a graph — the doc is the prior,
the current evidence is the fact. M1/M2 need no completed organic-use week.

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

Conditional on current scoped inventory identifying an applicable NVENC
node/graph. The organic-use counter is supplementary; zero does not prove
absence. If actual hardware/family/probe evidence proves no applicable node,
M7 does not happen for that inventory and this section records why.

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
if the current M0 inventory identifies an applicable node/graph there:
`-hwaccel videotoolbox -hwaccel_output_format
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

Collect a supplementary reset-aware usage week with the GPT prompt in §6;
do not wait for it to begin M1/M2. These counters are process-local. A final zero from a direct
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
metrics_encoder` green; and the current scoped §6 inventory table
filled for every node, stating for each: families
compiled, families the probe accepted, family actually selected, sessions
per family and grade where observed, tone-map pipelines selected/used. The
organic usage week is supplementary; unknown history remains labelled unknown.

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
open for supplementary organic-use evidence until that image is deployed and
continuously scraped for one reset-aware week. The direct-scrape fallback is valid only when start/end evidence proves
uninterrupted uptime; otherwise its window restarts. M1-M6 do not begin on an
invented baseline. **Superseded 2026-09-30:** this historical waiting rule
does not prevent M1/M2; use the current inventory and preserve unknowns.

### 5.2 M1 — extend the corpus to §3.1.3's content classes

**2026-09-30 implementation scope:** the effort already contains
`1080p-animation`, `sport` and the incumbent N1 eight-bit `dark-gradient`.
Keep those byte identities unchanged. `dark-gradient-10bit` is a separate
HEVC ten-bit input; `burn-pgs` muxes the existing deterministic `scripts/mkpgs`
SUP as an actual copied subtitle stream, rather than pre-burning pixels.
`dv-p5` requires `PLURX_DV_P5_FIXTURE` to name genuine media and verifies the
HEVC Profile 5 / RPU-present probe record before copying it into the corpus.
`scripts/bench fixtures --qualification` includes these acquisition-dependent
inputs; the ordinary incumbent corpus remains unchanged. Missing media and failed generation fail the command, never count as a built
class. No genuine P5 source is yet identified for this run.

**2026-10-01 continuation:** a bounded read-only private-library header census
identified a genuine HEVC Profile 5 / RPU-present candidate; private path and
stat/prefix-hash facts stay in the owned receipt. Whole-source hash, acquisition
and per-frame RPU evidence remain open. The independent
[HDR reference scorer](../performance/HDR-REFERENCE-SCORING.md) adds matched
decoded PQ-domain PSNR/SSIM, highlight/shadow code census and exact metadata
checks, plus a separately pinned explicit BT.709 grade for SDR VMAF. Actual
eight-frame authored-reference generation, a distinct lossy PQ comparison and
bounded offline grade/model execution are diagnostics, not genuine-film,
GPU, production-session or physical A/B acceptance. The incumbent generator,
fixtures and SDR scorer are unchanged; HLG/unreshaped DV remain refused.

M1 is **not accepted** by the generator or its mock tests. Actual generation,
the intended per-class stream facts, production bitmap-burn selection and
captured session matrix remain owed. The incumbent rate-control scorer refuses
HDR references because it lacks a reference tone-map: do not bypass that
refusal or score PQ/HLG against SDR as if the result meant quality. A versioned
all-class corpus receipt requires genuine inputs and appropriate HDR-reference
scoring, plus §3.6's independent fidelity checks. No manifest alone closes it.

**2026-10-01 actual acquisition continuation:** unchanged 45-second animation,
sport and PGS-track fixtures were generated in an owned CPU-only container on
the frozen `4f243a01` tool image, using the current `b65be8773` harness. A
small genuine P5 packet copy also yielded 50 actual Profile 5 RPUs; it is not
a 45-second corpus input, decoded reshape or independently graded reference.
The ten-bit gradient encoded but its unchanged checker refused it: the HEVC
VUI contains unspecified primaries/transfer (2/2), with BT.709 matrix (1).
An actual two-frame experiment showed explicit x265 `colorprim`, `transfer`,
`colormatrix` and limited `range` alone still emitted unspecified transfer
and primaries. The final narrow candidate also binds these properties on
the frames with `setparams` after the existing GEQ expression: metadata only,
not a pixel transform. Existing generic flags and the refusal remain; every
other fixture argv is unchanged. The corrected committed `5ec437ea7` bench
archive then passed a distinct two-frame probe and actual SPS check
(primaries/transfer/matrix 1/1/1, limited range 0), followed by the unchanged
45-second 1920×1080/24 fps gradient and its strict metadata checker. The
626,292-byte corrected fixture SHA-256 is
`853c3087e1fd650df536196123b6b0d8430526935b6c973bb2b545b5130c7249`;
video is 45.000 s (AAC/container 45.023 s). Original refused bytes remain
retained. This proves this fixture's metadata, not perceptual fidelity.
No GPU, production bitmap-burn or physical HDR acceptance follows.

A separate requested-45-second genuine P5 stream-copy then acquired 1,082
packets and 1,082 actual Profile 5/CM v4.0 RPUs in a bounded owned CPU-only
window. Packet PTS spans 0–45.167 s; GOP/container extent is 45.208 s, not
exactly 45 s. The 104,533,274-byte acquired fixture SHA-256 is
`7b8b1efbe6172783824b9dd3fff542473db7c9fe46faceff1c10e9f4b46fdbed`;
the extracted 310,279-byte RPU SHA-256 is
`d9c3f67b4d85196228b06ef8fa0b6784ce6bfe2c898ced4d5005d71166fbf780`.
The private source's size/mtime/inode were unchanged before/after; this is
not a full-source hash. This longer fixture supersedes no earlier limited
sample receipt and has not been decoded, independently reshaped/graded or
physically compared. M1's all-class corpus and fidelity acceptance stay open.

**2026-10-02 M1 reference-association implementation — incomplete corpus:**
`scripts/codec-qualification-corpus.json` now accounts for all thirteen existing
generators, including the separate ten-bit gradient, real-PGS input and genuine
P5 input. It is a versioned metadata plan, not a completed acquisition receipt.
Known full acquired-fixture hashes retain their historical source identity;
the separate 70-second H.264, grain, SDR4K and PQ4K inputs are not relabelled as
the default 45-second generator output. Unknown hashes stay null. Current file
availability, decoded grade, captured-session output and fidelity are unmeasured.

Run `scripts/bench qualification-corpus` to validate this metadata without
opening media, generating fixtures, probing tools or connecting to a server.
`--require-associated` refuses missing reference associations only in this
measurement command; it never controls ordinary playback. Metadata JSON reads
are bounded to 1 MiB per regular no-follow file and 4 MiB total, with held-file
identity and receipt-hash checks. The summary always reports
`measurement_executed: false` and `qualified: false`, even when associations are
complete. The existing capture, rate-control and scoring commands are unchanged.

SDR source references remain in the BT.709 VMAF domain. PQ-to-PQ references
remain in PQ code values; PQ-to-SDR requires a separately hash-bound independent
BT.709 grade receipt matching the parent, reference and zero-start interval.
An association validates those receipt fields, **not** its pixels or graph:
the independent [HDR scorer](../performance/HDR-REFERENCE-SCORING.md) still
validates decoded metadata, parent geometry, the exact grade graph and scoring
execution. HLG and unreshaped DV explicitly retain unsupported/unmeasured
associations; no SDR score is substituted. This does not complete §3.1.3's
all-class corpus, §3.6's fidelity checks, the session matrix or M1 acceptance.
The historical acceptance command below still needs a domain-aware measurement
consumer; this metadata command does not pretend the SDR-only rate-control
loader can consume PQ/HLG/DV references.

For the unchanged 64-document local-unit receipt bound, this task retires only
the inactive merged-PR671 discovery copy
`validation/python-unit-local/1d47b568736d9e306fee3f643db49e9118b9b4b85598c6325df8e35d6c6d4b62.json`.
Its exact 1,262-byte SHA-256 is the filename; Git blob is
`963c29967b09561bc077844ac5a0f2f5838da190`. Original null-source attribution,
authenticated comment6937 and landing history remain unchanged. Recover the
original with `git show b353c4f8c0129b17e6217680f6bdf757a3ede65b:validation/python-unit-local/1d47b568736d9e306fee3f643db49e9118b9b4b85598c6325df8e35d6c6d4b62.json`.
No active-task proof, receipt loader, cap or replay policy is changed.

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

**2026-09-30 runtime:** `OutputCodecContract` lives in
`transcode/encoder.rs`, not a parallel manifest. The pure production resolver
stores it on the private `ResolvedTranscode` and derives the existing
`PresentationContract` delivered codec/encoder from it. Session reporting and
recipe identity continue consuming that presentation, so source `file.*` facts
cannot substitute for delivered facts. `video_codec_for` delegates to the
same measured codec/depth/grade/family table. The renderer pairing and grade
are checked by `qualified()`; existing boot capability validation remains in
the node's resolution path. No new tuple is admitted, no argv changes, and
the plan/digest/recipe versions stay unchanged.

**Sole review disposition, 2026-09-30 — [#649 review 22](http://192.168.4.7:3000/noirr/plurx/pulls/649#issuecomment-6639):**
P2 accepted: the incumbent HDR10 builder always emits bitrate-bounded VBR,
so the new delivered contract normalizes a supplied HDR10 QVBR preference
to VBR and `qualified()` refuses a manually malformed HDR10/QVBR contract.
SDR retains its supplied effective mode. The incumbent options and recipe
field bytes remain unchanged, including their historical HDR10 option key
space; no cache version bump or invalidation is smuggled into the correction.
Focused regressions prove effective-mode truth, malformed-tuple refusal,
byte-identical encoder argv and retained legacy options/identity. P3 accepted:
the rollout paragraph now follows task PRs into the existing effort, focused
regressions/current-head Effort gate and separate exact-tree final promotion.
No second formal review or new media qualification is claimed.

Focused proof:
`cargo +1.97.1 test -p plurx-core --features hiqlite-store --lib output_codec_contract`
covers all legacy family/grade pairs and renderer pairings, refused HEVC SDR
and mismatched depth/grade, every supported legacy rate/forced-IDR encoder
argv, actual source-HEVC-to-delivered-H264 resolution, codec-distinct recipe
identity and the unchanged preexisting golden SHA-256. This is not new GPU or
device qualification.

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

**2026-10-01 offline evidence collector candidate:**
`scripts/rolling-grid-census` captures only flat, owned, private copied
MPEG-TS rolling-transcode outputs; it never starts a session or opens a server URL.
It verifies the selected local ffprobe binary hash, retains bounded raw packet
probes and media hashes, and consumes them again to report EXTINF spread,
presentation-start spacing, distinct packet cadence, packet-span/EXTINF
disagreement and keyframe-at-start counts. Duplicate timestamps or incomplete
packet timelines refuse; truthful complete timelines off the two-second grid
remain measured drift. Fractional cadence uses exact rational arithmetic and
bounded actual-timebase quantization, not a nominal integer fps guess.
Supplied source/daemon/graph provenance is recorded, not authenticated by
this tool: the campaign owner must independently bind it to the actual
executed session before using a result for M3 acceptance. Keyframe flags are
not NAL IDR/CRA proof. One cell is not the full four-fixture×four-rung census.

```bash
# Read an already copied, owned mode700 directory; no encoder/server activity.
python3 scripts/rolling-grid-census capture --root /private/owned/cell-input \
  --output /private/owned/new-cell-evidence --ffprobe /absolute/ffprobe \
  --provenance /private/owned/cell-provenance.json
# Recompute the copied objects and raw probe hashes; do not trust a stored verdict.
python3 scripts/rolling-grid-census validate --root /private/owned/new-cell-evidence
```

The provenance object requires `route: rolling-transcode`, source commit,
source/graph-argv/daemon/tool SHA-256 identities, session/family/pipeline,
fixture/rung, tool version, exact rational `output_cadence: [numerator,
denominator]` and `output_geometry: [width,height]`. Unknown facts refuse.
Every playlist URI must name one consecutive local `segN.ts`
object; URLs, missing/duplicate segments, symlinks and changed copies refuse.
`#EXT-X-MAP` and `.m4s` refuse explicitly: this collector does not yet capture
the initialization context required to measure fMP4. This is an evidence
format limit, not a production setting or gate.
The completed-segment window requires 30–256 entries: do not substitute a
short terminal segment or loop/relabel a 45-second corpus fixture to earn
that count. Acquire separately identified ≥60-second measurement inputs.
Bounded capture is ≤600 s aggregate, ≤20 s/probe, ≤512 MiB copied evidence,
≤16 MiB/probe output and ≤1 MiB/probe stderr; existing destinations refuse.
Every text/JSON input is capped before decoding/parsing using cap+1 reads:
playlist/receipt 1 MiB, provenance/tool-version 64 KiB, raw probe 16 MiB.
Failed partial captures retain their intent and files, not a completed receipt.
No drift means every completed interval/start spacing lies within one actual
output frame and every segment's first presented packet has a keyframe flag;
extra internal keys are reported separately, not treated as grid drift.
Physical/graph acceptance, authenticated campaign provenance and the original
full census below remain open. No production GOP flag changes follow here.

**2026-10-01 real-owner acquisition candidate:**
[`scripts/rolling-grid-acquire`](../../scripts/rolling-grid-acquire) and the
feature-enabled ignored `rolling_grid_campaign::owned_real_rolling_cell`
entry point provide a private loopback acquisition path. They do not add a
public Live create field, force a VOD refusal or change production flags.
The internal Live request runs the existing manager/producer, then real
in-memory Store claim/assign/activate APIs bind its active route. The bridge
delegates to shipped playlist/segment handlers and their actual downstream
EOF pump: buffering bridge bytes is never an extra delivery commit.

The active viewport video runs at1× with vendored hls.js. Real
`requestVideoFrameCallback` media time/presented-frame observations are sampled
at500ms, not every33ms; two actually accepted advancing observations must
satisfy the existing30s startup policy. No test-only presented marks, download
frontier relabel or synthetic Rendering is permitted. Nonce, session,
generation and producer attempt fence callbacks; absolute origin is applied
once at server control ingestion. Missing callbacks, stalls, refusal or changed
source preserve partial evidence, never a completed cell.

**2026-10-02 reporting-cadence repair candidate:** the bridge's existing
450ms acceptance floor starts when a control exchange settles, not when the
browser's preceding500ms interval tick fired. A delayed accepted response can
therefore make the next interval arrive too early. The page now waits at least
500ms after the previous response/error settles before reporting again; busy,
stale, paused, seeking and non-advancing observations still refuse. An actual
409 remains a terminal failed cell, not an automatic retry or synthetic pass.
One new synthetic contract executes the actual page with delayed responses
and checks the cadence, single in-flight exchange and retained refusal. It
passed once0.127s; this is not real-browser/corpus qualification. The prior
codec-capable diagnostic preserved one actual accepted frame observation,
then failed409 with no complete/bridge-final receipt. Its old5eb binary and
new38026 launcher retain separate attribution; no current-tree pass is claimed.

Supply an existing hash-pinned browser and reviewed feature-enabled test
binary; no build/download/install occurs in this controller. Its manifest pins
source, FFmpeg, ffprobe, browser, test binary and vendored hls.js. Source must
provide≥64s without looping and support the requested360/480/720/1080 rung.
Prepare the exact reviewed page only in a fresh owned mode700 root, then
validate without launching:

```bash
python3 scripts/rolling-grid-acquire --page-template /private/tmp/owned-cell/page.html
python3 scripts/rolling-grid-acquire /private/tmp/owned-cell/manifest.json
```

Operational `--execute` needs a separately authorized owned Linux cgroup
ceiling≤2CPU/2GiB/256PID for the complete local producer/browser tree, with this
controller as PID 1 in a fresh private PID namespace; bare-host, shared-PID or
already populated namespace execution refuses. A normal owned container can
provide this without writable cgroup delegation or privileged mounts. The
independent PID 1 supervisor bounds synchronous browser/body/drain/close calls:
585 s work plus a 15 s cleanup reserve within the absolute 600 s deadline.
The deadline starts before cheap bounded owner/root admission and launch
setup; full source/tool hashing, probe and page validation run only inside the
supervised worker, with no deadline reset. At operational execution, the outer
owner must also impose a 600 s container wall watchdog plus bounded exact-ID
terminal cleanup, recording nonce/labels/container ID/start/deadline/exit and
namespace termination. This protects against controller setup or filesystem
syscalls themselves failing to return; an internal receipt cannot prove that
external terminal condition. No operational container/watchdog ran here.
Cleanup signals only pidfd-bound identities in the initially empty task-owned
namespace, reaps descendants (including detached sessions), and refuses success
unless only PID 1 remains. Kernel namespace teardown on PID 1 exit is the final
backstop; the later operational owner must retain exact container identity and
terminal state, not infer them from a worker receipt. Stop, receipt or browser
errors cannot bypass subtree cleanup. Require successful `supervisor-final.json`
alongside `complete.json`; completion alone is not clean terminal evidence.
One browser/page/cell,≤4 concurrent media response bodies,
≤4096 observations/snapshots,≤256 segments/512MiB media,64MiB logs and10min wall
deadline. The fresh standalone child limiter caps file/CPU resources without
threaded post-fork callbacks. The later16-cell serial campaign retains its
160min aggregate bound; this tool does not authorize that campaign or reserve
K06 hosts. Pin actual browser build separately in the operational receipt.

Raw playlist revisions and exact served object names/hashes remain immutable;
an accumulated sliding window is derived evidence, not an original server
playlist. Completion requires≥30 distinct contiguous fully consumed segments,
≥60s of their actual advertised durations and actual browser media progress.
The census still measures packet/GOP facts; browser callbacks are not NAL IDR
proof. This is test-binary/internal-manager/shipped-handler/headless-browser
presentation evidence, NOT public create-route, physical, native, artist-HDR,
GPU or whole-M3 qualification. No actual acquisition has run for this candidate.
Two initial synthetic ownership contracts and two later supervisor failure
contracts passed once. The latter model blocked operations and detached
descendants; they do not execute browser, encoder or kernel namespace cleanup.
The old candidate lacks the supervisor consumer and refuses those new tests;
no previous unit successes repeated. Review 50's deadline and cleanup findings
are repaired in this same candidate, pending independent disposition.

**2026-10-02 internal acquisition continuation — not original M3 acceptance:**
the [sanitized sixteen-cell ledger](S11-INTERNAL-ROLLING-CELLS-2026-10-02.md)
records four separately hash-pinned synthetic 70.023-second inputs at
360/480/720/1080, measured using source `fb4360792` / tree `cc943750`,
lab binary SHA-256 `656e7589` and ARM64 runtime `b7bc6f79`. This is older
measured-source evidence, not runtime qualification of the current effort.
Each cell fetched 35 contiguous complete objects/70 advertised seconds,
presented at least60 seconds at real1×, and independently censused1,680
video packets at24fps. Probed outputs are640×360,854×480,1280×720 and
1920×1080. EXTINF min/median/max is2/2/2 seconds,35/35 first-packet keyflags,
maximum grid error0.0006666666666666666ms: no measured packet-grid drift.
Grain720 has **two extra internal keyflags**, at13.958333s and38.000000s;
the other15 cells have zero. Extra keys remain separate from grid drift.

Actual NUL encoder argv/source-FD/executable/parent/PUT-socket ownership,
complete served revisions, all35 fetched/retained/census byte equalities,
raw probes and exact cleanup/terminal watchdog receipts are retained
privately and bound by the ledger's hashes. Capability-bearing argv is not
published. Original acquisition2CPU/2GiB/256PID/600s and offline
2CPU/1GiB/64PID/690s bounds stayed unchanged. Distinct source-generation
resource variants and failed-only verifier repairs retain separate failures;
the PQ10-bit/BT.2020 source has actual frame/SPS-VUI evidence, but unspecified
mastering/CLL and no calibrated HDR fidelity. All cells delivered software
H264/yuv420p; PQ source class does not imply HDR output or tone-map fidelity.

This is **16 internal cells /4 synthetic inputs /0 original fully-qualified
public cells**. Every successful context ran once; no prior passing unit,
source or cell was replayed. Exact owned containers/volumes and temporary
watchdog/expiry owners were removed/terminal before each lease handback;
original finite input/runtime expiries were not extended. Earlier failed
acquisitions, OOM partials, missing raw verifier results and the reconstructed
HDR360 admission-storage record remain honestly disclosed. No production
GOP flags, qualified tuple, cache identity or gate changed. The original
media1/public-create-route matrix, two hash-named real-film censuses,
NAL-level IDR/CRA and applicable physical/native/GPU/fidelity acceptance
remain open. The preceding "no actual acquisition" statements describe
their dated candidate, not this separately authorized internal continuation.

**2026-10-02 retained Grain720 NAL continuation — one header context only:**
the [reviewable evidence ledger](../reviews/S11-GRAIN720-NAL-EVIDENCE-20261002.md)
records one new offline parse of unchanged retained TS/probe bytes. Actual
PAT/PMT/PES/Annex-B/AUD/picture/SPS-PPS/PTS parsing measured 1,680 access
units, 35/35 first type-5 IDR headers and 37 total IDRs. The two extras at
13.958333333…s and38s match the old Grain720 keyflags; no GOP code changed.
Nine tiny new syntax controls and this one corpus attempt passed once;
no producer/probe/decoder, old test or successful cell was replayed.

Historical parser/control/wrapper, sanitized AU facts, ordered input hashes
and actual process/cgroup/export/cleanup receipts are review snapshots, not
registered tests or permanent tooling. Private audit-byte attachment retention
is separate from unchanged original input/runtime expiry. The original raw
receipt/result identities stay distinct from sanitized publication. The
bounded parser proves header identity, not entropy/pixel decode, closed-GOP
reference independence, decoder/device random access or current-effort
runtime acceptance. Fifteen other internal NAL contexts remain unmeasured.
The original public-route/media1/real-film/physical/native/GPU/fidelity bars
and whole-S11 qualification remain open: 16 internal /4 synthetic /0 original
fully-qualified public cells, with old measured fb436 provenance unchanged.

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

Runs only if the current scoped M0 inventory identifies an applicable NVENC
node/graph; an organic-use week is not eligibility. Code: §3.4, both pieces, each flag
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

#### 2026-09-30 current scoped inventory, without a new production probe

Read-only SSH inspected PCI display/3D hardware, shipped FFmpeg encoder lists
and existing startup validation logs. No encode/probe was initiated by this
run, and no process-local zero was used as absence evidence.

| Node | Hardware | Compiled H.264 families | Existing boot validation | Selection evidence |
|---|---|---|---|---|
| `nynuc` | Intel Arc Pro 130T/140T, `8086:7d51` | NVENC, QSV, VA-API | QSV/VA-API accepted; NVENC refused `libcuda.so.1`; VideoToolbox false | Existing production pre-transcode records name Intel QuickSync |
| `m6` | AMD Phoenix1, `1002:15bf` | NVENC, QSV, VA-API | VA-API accepted; QSV device creation refused; NVENC refused `libcuda.so.1`; VideoToolbox false | Existing boot caption graph records use VA-API |
| `nuc4` | Intel Iris Xe, `8086:46a6` | NVENC, QSV, VA-API | QSV/VA-API accepted; NVENC refused `libcuda.so.1`; VideoToolbox false | Accepted-family evidence; ordinary-session selection not independently sampled |
| `nuc3` | Intel Iris Xe, `8086:46a6` | NVENC, QSV, VA-API | QSV/VA-API accepted; NVENC refused `libcuda.so.1`; VideoToolbox false | Accepted-family evidence; ordinary-session selection not independently sampled |

Container starts were respectively 16:34:55, 18:32:39, 18:43:58 and
16:39:35 UTC on 2026-09-30. These are current capability observations, not
a seven-day organic-use receipt. The four scoped Linux nodes have no NVIDIA
display/3D hardware or accepted NVENC route and no VideoToolbox route; M7/M8
are inapplicable for this inventory only. A new node or changed inventory
reopens them. Generic HEVC encoder symbols are not qualified HEVC SDR graphs.

Task PRs use the focused per-milestone regressions named in §5 and the
blocking Effort development gate. Full-suite `make unit` evidence belongs
to final qualification, not a claimed result of this scoped M2 task.
`make benchmark-check` still gates the committed A/B coverage
([../BENCHMARKING.md](../BENCHMARKING.md)); M1's new fixtures do not enter
that matrix and must not silently change it.

**GPT prompt — current fleet encoder inventory (M0; organic week supplementary):**

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
Steps 3-5 establish today's scoped eligibility inventory immediately. Steps
1-2 are supplementary organic-use observation, not a prerequisite to M1/M2.
Do not infer absent hardware or an inapplicable M7/M8 from zero counters.
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

Rollout: reviewable milestone task PRs into the existing
`effort/architecture-review-2026-09-20`, with logical commits and Execution-log
rows, focused local regressions recorded in each PR and the blocking current-head
Effort development gate. This M2 task did not run the full `make unit` suite.
Final promotion is separate: freeze task merges, merge current main into the
effort, qualify that exact tree and pass the Main promotion gate before merging
the effort into main. A moved base requires new exact-tree evidence. Metric names
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
| 2026-10-02 | gpt-6.1-sol | agent:/root/s11_next_cell_sol61 | M3 retained Grain720 header evidence, original qualification open | evidence-only continuation | [NAL ledger](../reviews/S11-GRAIN720-NAL-EVIDENCE-20261002.md): one old fb436 context,35 first IDRs/1680 actual AUs/37 total IDRs including TWO extras, matched to retained probes. Nine new syntax controls and one offline corpus success retained once; no media or test replay. Historical review snapshots, sanitized-not-raw provenance, durable private bytes and exact cleanup bounds recorded;15 other NAL contexts and original public/film/native/GPU/fidelity acceptance remain open. |
| 2026-10-02 | gpt-6.1-sol | agent:/root/s11_next_cell_sol61 | M3 internal acquisition/census evidence, original acceptance incomplete | evidence-only continuation | [Sixteen-cell sanitized ledger](S11-INTERNAL-ROLLING-CELLS-2026-10-02.md): four synthetic inputs,16 distinct internal once-successful cells at measured fb436 source/cc943750 tree/656e binary/b7bc runtime;35 objects/1680 packets/60+s real1× each,35 first keys/no drift. Grain720 has two internal extra keyflags, unchanged. Actual source-FD/PUT owner and four-way byte equality/exact cleanup retained privately; failures remain failed. No current-effort/public/NAL/physical/native/HDR-fidelity/GPU/full-M3 qualification or production GOP change. Independent review/current-head effort gate remain separate. |
| 2026-10-02 | gpt-6.1-sol | codex://threads/01a0c165-d718-73a1-93e9-e81380017705 | M3 acquisition reporting cadence; incomplete | [#710](http://192.168.4.7:3000/noirr/plurx/pulls/710) | Wait500ms after each accepted response before another frame report; existing bridge450ms floor, refusal and real-frame guards unchanged. One NEW actual-page synthetic control passed once0.127s; normal hook79442 passed; sole independent review60 approved with controller/PAGE hash-label correction. Ten already-landed #690 local receipts are retired only after byte-identical private preservation and complete923-pass journal1539 coverage; immutable history retained, no cap change or unit replay. Current composition/gate and real browser/corpus/matrix qualification remain owed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M0 | [`17dfebc4` / #422](http://192.168.4.7:3000/noirr/plurx/pulls/422) | Implemented five-family availability, family/grade accepted-start, and eight-pipeline counters with closed enum labels. Count points are manager registration for rolling, reader attachment for VOD, and first publishable/fenced Live TV inventory (encoder only; Live TV currently refuses tone-map-required routes). Read-only inventory found QSV/VA-API nodes only; M7 NVENC and M8 VideoToolbox are refused for this fleet. Review correction: the seven-day gate is reset-aware and bound to the exact build; focused production-seam tests cover rolling, VOD and Live TV once-only/pre-boundary behavior. Needs: deploy and collect one valid reset-aware week before M1-M6. |
| 2026-09-30 | gpt-6.1-sol | agent:/root/k06_runtime_sol61 | M2 runtime; M1 generator seams, incomplete | pending | Typed contract drives the actual resolved plan/delivered presentation; all legacy selections and golden recipe identity preserved, five focused storage-enabled core regressions green. Three fixture-seam tests and all 59 existing harness tests green. No new HEVC qualification/default or production change. Current four-node hardware/compiled/existing-probe evidence above, with unknown ordinary-session observations retained. Organic week now supplementary by Paul's ruling; M1 remains open for genuine P5 acquisition, actual generation/session burn and HDR-aware scoring/fidelity evidence. |
| 2026-10-01 | gpt-6.1-sol | agent:/root/s09_665_resume_sol61 | M1 bench metadata/acquisition continuation, incomplete | candidate | Two new focused guards passed once after negative controls; first is argument-only. Actual x265-parameter-only counterexample retained; final metadata-only frame tail proves tiny SPS and full unchanged 45-second ten-bit gradient metadata. Three other generated fixtures plus a distinct requested-45-second genuine P5/RPU fixture acquired; GOP extent and source-stat limits recorded above. Organic week is supplementary, not M1/M2 eligibility. No production encoder/default, GPU, independent HDR grade or whole-corpus qualification. |
