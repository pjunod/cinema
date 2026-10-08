# macOS video processing — accelerate the work, measure the memory cost

**Status:** proposed; one adversarial review completed, author corrections applied ·
**Written:** 2026-10-07 ·
**Source inspected:** `890bca0fc186d00f7b10a379cd7dd2f1a37dc37c` ·
**Decision owner:** Paul after review · **Runtime changes:** none.

This document answers which native macOS processing paths plurx should add,
why, and what evidence would justify them. The companion
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md) defines the
milestones, code ownership, tests and review handoff. Read both before building.
The source locations below describe the inspected checkout; re-verify their
symbols against the implementation base. The [review record](MACOS-VIDEO-PROCESSING-REVIEW.md)
preserves the original request-changes verdict and the author dispositions.
The revised documents have not received a second independent review.

## 1. Decision proposed — optimize processing before buffer movement

Keep FFmpeg as the media worker. Evaluate its existing VideoToolbox and
Metal filters before creating a native helper or custom shader backend.
Start with progressive SDR scaling and HDR10-to-SDR tone mapping. Measure
CPU time, throughput, latency, energy and image quality against the current
production graph on the same Mac. Treat avoiding copies as an independently
testable benefit, not the reason the project must succeed.

Apple Silicon shares physical memory between CPU, GPU and media engines.
There is no discrete-GPU PCIe upload/download penalty to remove. The shared
DRAM is not synonymous with GPU-local on-chip storage. A CPU-accessible
frame can alias the existing pixel buffer, or a library can copy it into
another allocation. Format conversion and synchronization can also cost
work without crossing a device bus. Apple's
[storage-mode documentation](https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus)
and [shared-resource contract](https://developer.apple.com/documentation/metal/mtlstoragemode/shared)
make these distinctions important.

Upstream FFmpeg's
[VideoToolbox implementation](https://github.com/FFmpeg/FFmpeg/blob/master/libavutil/hwcontext_videotoolbox.c)
contains both direct pixel-buffer mapping and transfer functions that call
`av_frame_copy`. That establishes possible mechanisms, not which copies
our installed executable performs per frame. Capture the exact FFmpeg
revision and inspect/profile its actual path in the experiment.

For scale, one tightly packed 3840×2160 P010 frame occupies 24,883,200
bytes. One copy reads and writes it: about 2.99 GB/s at 60 frames/s, ignoring
padding and cache effects. This arithmetic is not a measurement of plurx,
and does not establish a bottleneck. The larger opportunity may be replacing
CPU scaling and floating-point color processing with specialized processing.

**Success is a useful measured processing improvement with correct output.**
A negative result is valid: retain the existing graph when the candidate
adds complexity without a material gain.

## 2. Current implementation — what is established and what is unknown

| Area | Verified in the inspected source | Consequence |
|---|---|---|
| Video decode | `DecodeBackend::VideoToolbox` is already selected; argv uses `-hwaccel videotoolbox` | This is not a proposal to introduce hardware decode |
| Frame representation | `FrameDomain` has no VideoToolbox variant; `surface_contract` represents its output as `SystemMemory` | The current contract cannot describe a retained VideoToolbox surface |
| Video encode | `Encoder::VideoToolbox` emits `h264_videotoolbox` | Hardware H.264 encoding already exists |
| HDR output | `video_codec_for(OutputGrade::Hdr10)` returns `None` for VideoToolbox | HEVC/Main10 needs a separate output-contract qualification |
| CPU processing | `video_filters_for_contract` uses CPU scale and, for the zscale route, linear floating-point tone mapping | Offloading the processing is a concrete candidate |
| Pipeline selection | `TranscodeManager` receives one startup-selected `Pipeline`; `Pipeline::for_session_with_scan` narrows it | Multiple Mac processing choices need per-session selection without replacing all other backends |
| Startup tone-map probe | `pipeprobe` compares a 1.5 s, 24 fps generated HDR10 clip at 1080p output; speedup threshold is 1.2 and mean channel tolerance is 24 code values | Useful smoke evidence, insufficient for sustained performance or perceptual qualification |
| Dolby rendering | `DoviTonemapx` currently forces software decode to preserve RPU side data | Existing implementation policy, not proof that VT hardware decode loses metadata on every build |
| Interlace | Generic hardware graphs are declined for interlaced files; Live TV separately builds a CPU `bwdif` chain | File and Live TV paths require separate integration and cadence evidence |
| Captions | Live H.264 VideoToolbox applies `-a53cc 0`; caption-bearing file input has a documented follow-up | Preserve the live policy and exercise file-caption failures explicitly |

Code anchors:
[decode/surface contracts](../../crates/plurx-core/src/transcode/decode.rs),
[argv and filters](../../crates/plurx-core/src/transcode/mod.rs),
[encoder](../../crates/plurx-core/src/transcode/encoder.rs),
[pipelines](../../crates/plurx-core/src/transcode/pipeline.rs),
[startup probe](../../crates/plurxd/src/pipeprobe.rs),
[manager construction](../../crates/plurxd/src/transcode/manager/construct.rs),
[Live TV](../../crates/plurxd/src/live_tv.rs).

**Local binary observation, not daemon evidence:** the shell's
`/opt/homebrew/bin/ffmpeg` reported 9.0.1 and listed `scale_vt`,
`yadif_videotoolbox` and `hevc_videotoolbox` on 2026-10-07. It did not list
`tonemap_videotoolbox`, `zscale` or `libplacebo`. This is not proof of the
configured daemon's executable, successful hardware access, or a usable
baseline tone-map graph. M0 must resolve `PLURX_FFMPEG` through the daemon's
actual launch environment and inventory that executable and its libraries.

No current Mac throughput, energy, copy-count or concurrency benchmark was
performed for this proposal. Historical Linux results in pipeline comments
must not be attributed to Apple Silicon.

The existing [codec/GPU plan](CODEC-AND-GPU-QUALIFICATION.md) deferred its
VideoToolbox milestone after an inventory without a Mac daemon. The
[live-caption incident](LIVE-TV-VIDEOTOOLBOX-ATSC1-ROOT-CAUSE-AND-FIX.md)
separately records Mac behavior. Neither is a current fleet census. This
proposal supplies a new Mac-specific experiment; it does not claim those
older measurements qualified it.

## 3. Scope and alternatives

### 3.1 Scope to implement first

Native macOS server workers on measured Apple Silicon models; existing VOD
and rolling-HLS delivery; progressive SDR scaling; HDR10-to-SDR conversion;
unchanged H.264 output and existing audio, subtitle, timestamp and playback
ownership contracts. Keep the current graph available on every node.

The implementation plan then has separately accepted extensions for HLG,
Dolby Vision, subtitles, interlace/Live TV and HEVC/Main10. One successful
HDR10 graph grants none of those capabilities automatically.

**Recommended starting order (2026-10-07):** begin with M0/M1 inventory
and experiments, then implement only candidates that meet the existing
quality and benefit criteria. The first delivery is progressive SDR scaling
plus HDR10-to-SDR tone mapping, qualified through M6.

| Priority | Candidate | Why this order |
|---|---|---|
| 1 | Hardware decode → native SDR scaling → hardware H.264 encode | The simplest processing candidate establishes surface handling, capability detection and fallback without adding HDR color conversion. |
| 2 | HDR10-to-SDR processing through the measured native or Metal mapper | A strong potential benefit where CPU tone mapping dominates; actual gains and the complete Jellyfin feature set must be demonstrated. |
| 3 | P5 hardware decode with existing Dolby-aware CPU processing, then GPU Dolby processing | Isolate decoder gains from renderer/color risk; both need the strict metadata contract before production use. |
| 4 | Subtitle compositing | Prioritize when measured subtitle burn-in cost warrants it; retain existing shaping and geometry semantics. |
| 5 | Live TV/deinterlacing and HEVC/Main10 output | Choose these separate follow-ups according to measured workload and client demand; neither blocks the initial delivery. |

Run the P5 four-way experiment in §3.5 during M1 so metadata preservation
and decoder feasibility are known early. This early experiment does not
bring Dolby production integration into the initial SDR/HDR10 delivery.
Extension order is a default priority, not a new dependency between E1–E4;
each retains its own acceptance criteria and can retain the incumbent.

### 3.2 Explicit non-goals

- No native Apple client/player rewrite or custom display renderer.
- No changes to direct-play/remux preference, source-media bytes or library
  metadata to make a candidate appear compatible.
- No Linux container access to macOS media engines; this is native execution.
- No initial frame interpolation, denoising, super resolution or sharpening.
  Those change the picture and consume additional processing; they are
  separate product choices, not necessary for this optimization.
- No assumption of AV1 hardware encoding, or of one codec/profile matrix
  across all Apple Silicon and Intel Macs.
- No generic rewrite of QSV, VA-API, CUDA, decoder recovery or cluster leases.
- No promise of byte-identical output across hardware encoders or OS builds.
- No new compile-time feature flag, environment-only enablement mechanism,
  or readiness receipt that prevents an operator saving the feature switch.

### 3.3 Alternatives and their costs

| Option | Benefit | Cost / limitation | Proposed disposition |
|---|---|---|---|
| Current CPU filters plus VideoToolbox codecs | Existing behavior, easiest reference | May spend significant CPU on scale/color work | Keep as incumbent and valid outcome |
| Upstream FFmpeg `scale_vt` | Existing VideoToolbox scaler; small integration surface | Color-conversion options alone do not prove HDR tone mapping | First SDR experiment |
| Jellyfin FFmpeg native VT and Metal processing | Existing media-server filters, including Metal tone mapping | Must pin and ship a compatible build; exact filters differ from upstream | First HDR experiment |
| Direct Core Image/VideoToolbox helper | More control over Apple APIs | New process or FFI boundary, ownership, color, timestamp and packaging work | Only reconsider if existing filters fail a measured requirement |
| Custom Metal kernels | Control of fused processing and color math | Shader correctness, interop and long-term maintenance | Deferred until an unmet requirement is established |
| Vulkan/libplacebo on macOS | Reuse a cross-platform color renderer | Additional API/interop/build surface to qualify | Secondary experiment if native choices fail |

FFmpeg documents [`scale_vt`](https://ffmpeg.org/ffmpeg-filters.html#scale_005fvt)
as a VTPixelTransferSession scaler/color converter. Its
[`yadif_videotoolbox` source](https://www.ffmpeg.org/doxygen/8.0/vf__yadif__videotoolbox_8m_source.html)
provides an existing Metal deinterlacer. These are candidate mechanisms;
YADIF and the current BWDIF are not interchangeable quality references.

[Jellyfin's Mac guide](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/apple/)
distinguishes native VideoToolbox tone mapping from Metal tone mapping,
with Dolby Vision P5 support attributed to the Metal path. Its
[FFmpeg feature inventory](https://github.com/jellyfin/jellyfin-ffmpeg/wiki/Features)
names `tonemap_videotoolbox` and additional Apple processing filters.
Treat these as upstream capability claims. M1 must pin a source revision,
inspect its patches, record exact filter names/options and prove each graph.
The older plan's shorthand `tonemap_vt` is not a verified filter name.

### 3.4 Jellyfin is a dependency, not an optional optimization package

On 2026-10-07, the official
[Jellyfin FFmpeg releases](https://github.com/jellyfin/jellyfin-ffmpeg/releases)
listed `v8.1.3-1` as latest. No released Jellyfin FFmpeg 9 was verified.
The local Homebrew 9.0.1 inventory is therefore not evidence about a
“Jellyfin 9” distribution. Neither `scale_vt` nor Metal deinterlacing is a
new FFmpeg-9-only prerequisite; both predate that major version.

The pinned
[v8.1.3-1 patch series](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/debian/patches/series)
lists the following relevant additions:

| Patch | Capability to retain or evaluate |
|---|---|
| 0043 | VideoToolbox scaler output-format control |
| 0044 / 0045 | VT overlay and Core Image transpose |
| 0046 | Metal `tonemap_videotoolbox` |
| 0053 | SIMD software `tonemapx` |
| 0054 / 0084 | AC-4 decoder and MPEG-TS detection fixes |
| 0056 | `bwdif_videotoolbox` |
| 0027 / 0061 | Dolby HLS/TS side-data and Dolby/HDR10+ bitstream handling |

The [Metal filter patch](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/debian/patches/0046-add-vf-tonemap-videotoolbox-filter.patch)
requires Metal, CoreVideo and VideoToolbox at build time. It reads
`AV_FRAME_DATA_DOVI_METADATA` when `apply_dovi` is enabled. This is concrete
source support, not a guarantee that an arbitrary macOS binary includes or
successfully executes the filter.

Keep Jellyfin's required AC-4, Dolby, subtitle and muxing/pacing functionality
throughout the effort. Prefer the installed Jellyfin build if sufficient;
evaluate the pinned 8.1.3-1 source only if an upgrade is needed. Never switch
the daemon to upstream/Homebrew FFmpeg merely to obtain a higher version.
The package acceptance matrix in implementation M0/M1 is mandatory even
when the replacement calls itself Jellyfin FFmpeg.

### 3.5 Profile 5 support exists; hardware decode is an open opportunity

This project does not add P5 playback support. Existing direct delivery and
Dolby-aware transcoding remain the controls. The optimization question is
whether hardware HEVC reconstruction and/or Metal Dolby processing can
replace the expensive parts of the current software route without losing
per-frame metadata or applying the Dolby transform twice.

The current planner explicitly restricts `DoviTonemapx` and
`DoviPassthrough` to software decode. Comments attribute this to hardware
paths dropping RPU side data. That blanket statement has not been reproduced
against the target Mac and current Jellyfin build in this investigation.
Do not promote the comment into a hardware limitation.

In the pinned Jellyfin
[HEVC decoder source](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/libavcodec/hevc/hevcdec.c),
RPU parsing and `ff_dovi_attach_side_data` exist in the decoder framework
that also supports hardware acceleration. The Metal filter consumes such
side data on VT frames. These observations motivate an experiment; they do
not prove its end-to-end behavior on a target Mac. CPU bitstream/metadata
parsing can coexist with hardware pixel decoding and is not equivalent to
software decoding the entire HEVC picture.

M1 must compare software decode + `tonemapx`, VT decode + `tonemapx`,
software decode + Metal, and VT decode + Metal where executable. Keep the
same encoder/output and record each transition. At the renderer input,
observe metadata presence and frame/PTS association, test RPU application on
and off using a non-identity sample, and compare against known-correct
output. Output differences alone do not establish correct Dolby processing.
Confirm actual hardware decode and exclude silent VT software fallback.
Check whether the decoder returns raw P5 samples or already transformed
samples so the renderer neither skips nor duplicates reshaping.

Keep the incumbent software route until this evidence exists. E1 may then
replace the coarse software-only rule for the proven tuple. Make
`requires_dolby_metadata` independent of `requires_software_decode`:
`surface_contract` currently derives its side-data requirement from the
latter, which would become wrong as soon as hardware decode is admitted.

### 3.6 The P5 hardware-decode question extends beyond Macs

Jellyfin documents accelerated P5 processing on other vendors as well.
The crucial distinction is the decoder implementation and metadata path,
not merely whether a GPU supports HEVC Main10.

| Platform | Documented P5 processing route | Decoder caveat |
|---|---|---|
| Intel | Native VA-API/DXVA hardware decode with OpenCL Dolby processing; QSV can still encode | Jellyfin requires its native-decoder preference for Dolby Vision; do not assume `hevc_qsv` has the same metadata behavior |
| NVIDIA | NVDEC with CUDA Dolby processing and NVENC output | Jellyfin requires the enhanced NVDEC path rather than its older CUVID decoder path |
| AMD | Hardware decode with OpenCL, or Vulkan/libplacebo on Linux | Hardware generation, OS and interop determine which complete graphs work |

Sources checked 2026-10-07:
[Intel](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/intel/),
[NVIDIA](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/nvidia/),
[AMD](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/amd/).
These establish existing upstream routes, not qualification of plurx's
current binaries, commands or fleet. In particular, Intel's HDR10 VPP mapper
is not a substitute for a Dolby-aware renderer merely because both run on
the same Intel GPU.

The general architecture is hardware HEVC pixel reconstruction plus
CPU-side bitstream/RPU parsing, followed by Dolby-aware color processing
and hardware encoding. CPU metadata parsing does not imply CPU picture
decoding. A hardware-decode + CPU `tonemapx` experiment can isolate decode
savings before changing the renderer on any vendor.

The broad software-only rule in plurx therefore warrants a cross-platform
follow-up. On the Intel fleet, test a metadata-preserving native decode
path before concluding that QSV hardware cannot help P5. Keep decoder and
encoder families independent. Reuse the four-way experiment and its RPU,
frame association, pixel and actual-hardware checks, using the appropriate
renderer instead of Metal. Do not lift restrictions for unmeasured tuples.
On 2026-10-08 the user authorized this investigation and implementation in
the separate `effort/video-processing-followups` effort. Its native-decoder
observations and strict metadata policy share the existing processing owners;
Mac qualification remains distinct from every non-Mac tuple. See the
[execution ledger](MACOS-VIDEO-PROCESSING-STATUS.md) for current ownership.

## 4. Proposed architecture — freeze a complete processing choice

### 4.1 Data flow

```text
source facts + requested presentation + operator preferences
                           |
node-local Mac processing capabilities and implementation identity
                           |
             resolve one complete immutable plan
                           |
      decoder -> processing -> encoder -> existing mux/publication
                           |
    actual plan identity, diagnostics, accepted-start counters
```

The preferred SDR experiment is VideoToolbox decode, a VideoToolbox surface,
`scale_vt`, and VideoToolbox encode. HDR experiments replace the processing
stage with a verified native or Metal tone-map graph, including any scale,
format and metadata operations it actually needs. CPU rendering remains a
valid choice. A mixed graph that demonstrably wins is acceptable; a graph
is not rejected merely because it contains a CPU stage or frame mapping.

### 4.2 Extend the existing contracts, do not add a second planner

Proposed stable processing variants are `VtScaleSdr`, `VtToneMapNative` and
`VtToneMapMetal`, with stable identifiers `vt_scale_sdr`,
`vt_tonemap_native`, `vt_tonemap_metal`. A later HDR-preserving variant is
`VtScaleHdr10`; its grade differs and it cannot share the SDR fallback rule.
E1 adds a separate Dolby-aware Metal variant (`DoviVtMetal`) if proved,
with no generic SDR fallback. These are proposed Rust enum variants, not
FFmpeg filter names.

Extend `FrameDomain` with `VideoToolbox`. Here a domain denotes the API and
pixel-buffer representation, not a physically separate memory bank. Carry
NV12/P010 and explicit transitions in `DecodeSurfaceContract`; do not equate
`SystemMemory` with a proved copy or `VideoToolbox` with no copies.

The existing `ResolvedTranscode` remains the sole owner of the chosen
decoder, renderer, output contract and side-data requirements. Both VOD and
rolling argv must come from that same resolved plan. Updating
`Pipeline::decode_args` alone is insufficient: the resolved-plan argv path
also matches `DecodeBackend` directly and currently emits only `-hwaccel
videotoolbox`.

Introduce a small node-local Mac capability collection alongside the legacy
selected pipeline. It contains complete supported graph classes, not one
`metal_available` boolean. Do not add Mac candidates indiscriminately to
`PIPELINE_CANDIDATES`: the existing probe is HDR10-specific and assumes a
single selected result. Existing non-Mac selection remains unchanged.

Each graph class identifies:

| Field | Meaning |
|---|---|
| Processing variant and revision | Which implementation and normalized filter parameters run |
| Decoder and surface format | Hardware or software decoder; actual surface and sample representation |
| Input color family | SDR, HDR10, HLG or a separately understood Dolby profile/base-layer route |
| Geometry and cadence envelope | Measured maximum raster/rate plus supported normalization operations |
| Subtitle and scan modes | None, specific burn mode; progressive or specific deinterlace policy |
| Encoder/output contract | H.264 SDR initially; HEVC Main10 only in its later stage |
| Runtime compatibility result | Available, unavailable with cause, or pending probe |
| Benchmark evidence | External experiment reference; advisory to operators, not a runtime permission token |

Do not infer all combinations from independently working components. A
decoder, filter and encoder can work individually and fail when connected.

### 4.3 Deterministic routing and setting semantics

Add the proposed persisted setting `macos_video_processing_enabled`, default
false during development. It applies to new plans on native macOS workers;
other nodes retain it but do not change their processing. Settings and API
readback report the saved value regardless of readiness. A session captures
one settings snapshot; saving does not mutate a running producer.

With the switch on, attempt an implemented compatible Mac graph selected by
input/output class and a fixed preference table established by M1. If both
native and Metal HDR10 paths pass, M1 chooses their ordering using measured
results; the decision and table revision become reviewable code. Do not
choose afresh using transient CPU load or benchmark each movie at startup.

Missing offline receipts, missing fleet inventory, an unfavorable speed
number or an unreviewed client matrix never disable the switch or override
its saved value. Readiness reports these facts. Runtime incompatibility
(missing filter, unsupported pixel format, failed graph execution, mandatory
side data absent) still requires a compatible fallback or an honest failure;
it cannot be cured by an enabled switch. Distinguish those reasons in logs.

Missing/ambiguous input color facts use existing routing. Never guess PQ
from bit depth or interpret unknown metadata as SDR merely to use the new
path. Existing explicit software-decode overrides and continuation
restrictions win; select a compatible mixed graph only if separately
implemented and tested, otherwise keep the incumbent path.

Until each extension lands, interlaced input, burns, rotation, non-square
pixel normalization and frame-rate normalization use the incumbent when
the candidate cannot reproduce the existing presentation contract. Record
the unsupported operation, rather than silently omitting its filter.

### 4.4 Probe compatibility without turning benchmarks into startup policy

Separate three kinds of evidence:

1. **Inventory:** exact executable and linked libraries, filter options,
   architecture, OS build, hardware model, service context. A listing only
   proves compilation.
2. **Runtime smoke:** execute the production graph with a compressed fixture,
   decode the output, check frame count, timestamps and color contract. This
   determines whether the local implementation can execute the class.
3. **Offline qualification:** repeated sustained performance, image inspection
   and client playback. This decides whether to ship/default the path; it is
   not a hidden setting gate.

Retain the existing probe behavior for existing backends. New Mac probes
must not inherit its 1.2 speed threshold as a runtime enablement condition.
Use the bounded process helpers in [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs)
for timeout, bounded diagnostics, cancellation, kill and reap.

Proposed initial bounds: one Mac smoke probe at a time, 10 s wall time per
candidate and 30 s for the initial three candidates. Fixtures are generated
offline and bundled as described below; startup does not encode fixtures.
Run after server startup;
pending or unavailable results leave new sessions on the incumbent without
delaying their start. A missing fixture is an unavailable observation, not
permission to synthesize a long encode on the playback path. Revalidate on
executable/library, macOS build or hardware identity change. No permanent
negative result from a transient busy device; provide an explicit reprobe
operation and repeat on the next daemon start.

The exact bounds are proposed operational limits, not measured timings.
M1 must test whether they work on the slowest supported machine. Shader
first-use latency belongs in the cold-start measurement, not a discarded
warmup result.

### 4.5 Runtime fixtures are a shipped dependency

M1 owns generating and licensing a small synthetic compressed smoke corpus;
M3 owns embedding/loading it, and M6 owns clean-install validation. The
proposed source location is `crates/plurxd/fixtures/macos-processing/`.
Embed the initial corpus and manifest into the daemon with an aggregate
compressed size budget of 2 MiB, so a fresh offline install has every input
needed by the initial probe classes. If M1 cannot fit an adequate corpus,
revise this explicit budget before integration rather than downloading media
at runtime or silently skipping a class.

Include SDR 8/10-bit and PQ/BT.2020 HEVC patterns exercising the chosen
initial graphs. The manifest records schema/version, SHA-256, source/generator
provenance, redistribution terms, input format/frame count and expected
output observations for each graph. Expected output pixels use documented
tolerances; hardware-encoded bytes are not required to hash identically.
M1's private visual-quality corpus is separate and never a runtime dependency.
E1 and subsequent extensions must add their own lawfully distributable
smoke fixtures before advertising those new runtime capabilities.

M3 verifies the embedded manifest and writes fixtures into a private
content-addressed cache using bounded asynchronous file operations and
atomic publication. Set a 5 s aggregate preparation deadline, independent
of the graph-execution budget; no network access, subprocess encoding or
playback-path generation is allowed. A corrupt cached copy is replaced from
embedded bytes. Disk/full/permission failures produce a specific unavailable
observation, preserve the setting and incumbent playback, and can be retried
by explicit reprobe or restart after the environment is repaired. Invalid
embedded bytes are a packaging failure and must fail release validation.

Test a clean offline installation, no existing cache, corrupt cached bytes,
manifest mismatch, unwritable/full cache, cancellation and successful reprobe.
These checks establish installability; they do not turn the smoke corpus
into a substitute for the sustained and perceptual benchmarks.

## 5. Color, timing and failure contracts

### 5.1 Color correctness precedes performance

HDR10-to-SDR output must be BT.709, with explicit transfer, matrix, primaries,
range and output pixel format. Verify decoded pixels as well as container
and elementary-stream declarations. Remove stale HDR/Dolby signaling only
when the output has actually been converted to SDR. Record peak-luminance
provenance and whether the backend consumes, ignores or internally chooses
it; a native mapper with opaque controls must not claim the CPU Hable curve
or its explicit peak was applied.

Use the current [tone-map corrections](TONE-MAP-CHAIN-CORRECTIONS.md) as
context for peak handling, gamut mapping and dithering. Do not require
pixel identity between different tone curves, and do not accept mean luma
agreement as proof of correct highlights, shadows, gamut or temporal
stability. Preserve 10-bit precision until the tone-map/output conversion;
reject an accidental early 8-bit conversion. Verify gradients for banding.

HDR10+ dynamic metadata handling must be stated honestly. An intentional
static-HDR10 base treatment is a separate route with that limitation, not a
claim of dynamic HDR10+ processing. HLG has distinct transfer/reference-white
behavior and receives its own fixtures and acceptance row.

Dolby Vision P5 must never fall back to a generic PQ mapper. First verify the
selected fork's RPU parsing, reshaping and metadata transport using a sample
where applying the RPU visibly changes pixels. Keep software decode if the measured tuple requires it; do not impose it
on all future hardware tuples. P7 enhancement-layer and P8 base-layer cases keep existing
routing until individually exercised; a P5 result does not qualify them.

**E1 execution requirement:** `apply_dovi=1` is not a strict guarantee. In
the pinned Metal filter it uses metadata when present, but absent metadata
can still take an ordinary PQ/HLG path with exit zero. Planning flags and a
successful initial RPU probe cannot prevent wrong pixels after a seek or
midstream metadata loss.

An accelerated P5 graph must enforce required effective Dolby metadata at
the renderer input for every frame it processes. Effective metadata means
the parsed, applicable Dolby state associated with that frame, including
legitimate bitstream-defined reuse resolved by the decoder. It does not mean
every compressed packet must contain a fresh RPU, and a stale attachment
from a different frame/seek is not valid state. The implementation must
preserve and validate that association across reordering, resets and seeks.

Use a proved strict mode if the pinned implementation supplies one;
otherwise E1 owns a minimal reviewed FFmpeg boundary check/patch before
introducing the accelerated P5 production route. The inspected patch's
`apply_dovi` option is insufficient. The check must reject missing or invalid
required metadata before returning the affected frame downstream, producing
a distinct diagnostic and nonzero failure. Valid earlier output may drain;
no affected frame may enter an encoder or published segment. The existing
recovery owner then selects a Dolby-aware replacement or reports failure.
A warning, sampled probe or post-encode spot check is insufficient.

Include initial absence, metadata lost midstream, valid reuse, stale state
after seek and reordered frames in E1 tests. Confirm the strict mode does
not reject ordinary HDR10/HLG graphs where Dolby metadata is not required.
Bind the enforcement mode and patch revision into the effective recipe.
If this execution contract cannot be implemented, keep the existing P5
route; an enabled processing switch does not imply that every renderer is
compatible. This requirement does not delay M0/M1 SDR/HDR10 experiments.

### 5.2 Timing, subtitles and captions remain delivery contracts

Preserve frame count, rational cadence, PTS policy, keyframe/segment rules,
SAR, rotation and aspect ratio. Preserve the existing VOD no-reorder/B-frame
policy; tuning encoder speed, B-frames or rate control is a separate A/B.
Tests must include seeks and resume, not only a linear FFmpeg conversion.

For interlaced input, deinterlace before scaling. File and Live TV frame/field
output policies remain distinct; do not double the rate without updating
its presentation contract and bitrate calculation. YADIF replacing BWDIF
requires motion-quality acceptance, not just matching dimensions.

The initial implementation retains CPU subtitle rendering/compositing. The
2026-10-08 follow-up now implements and compares an explicit download after
GPU processing against a qualified GPU overlay path.
Text shaping/font behavior remains libass's responsibility; GPU compositing
does not replace it. Test colored PGS cues, ASS positioning, alpha, seeked
cues and end-of-subtitle behavior. Compose SDR subtitle white in the SDR
output space. HDR subtitle burn-in is outside this effort's first contract.

Keep Live TV's scoped `-a53cc 0` policy. Do not copy it to the shared encoder
or VOD. The [file-caption follow-up](VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md)
must be resolved or explicitly excluded with an existing viable path before
claiming support for caption-bearing DVR/file input.

### 5.3 Fallback preserves the resolved presentation

A failed SDR candidate can use the existing compatible SDR renderer through
the established recovery owner and attempt fences. It creates a new resolved
plan and recipe identity. Do not append fallback bytes to a cached asset
named for the first graph, rewrite argv behind a frozen plan, or add a second
retry loop inside the filter builder.

Respect the existing automatic-decoder-recovery setting when a retry changes
the decoder. Processing fallback and decoder fallback are separate facts.
After publication, use the current replacement/handoff mechanism or terminate
through the current error path; do not splice a changed stream into published
segments. Test before-publication and after-publication failure separately.

An HDR-preserving failure may only retry an equivalent HDR output contract.
Changing HDR to SDR requires the existing negotiated replacement route.
For P5, the fallback must be Dolby-aware; if no such graph is executable,
report failure rather than producing plausible-looking wrong pixels.

### 5.4 Recipe identity and mixed-node behavior

The [recipe](../../crates/plurx-core/src/transcode/recipe.rs) hashes the resolved
plan and FFmpeg build. Add effective processing variant/revision, normalized
parameters, representation transitions, actual output codec/grade and
backend implementation identity where not already covered. Audit both the
resolved-plan digest and daemon engine attestation; the version's first line
alone cannot distinguish locally patched builds.

Apple framework behavior can change with macOS. Bind the Mac implementation
identity to the OS build, executable/library attestation, architecture and
relevant hardware class. Exclude hostname, probe time, benchmark result and
whether a settings page was opened. More conservative cache separation is
acceptable; accidental equivalence between two processing implementations
is not. Version the new Mac identity extension without needlessly changing
unaffected non-Mac recipe hashes; require golden tests for those routes.

Mac graph capabilities belong to the worker that executes them. For this
effort, pipeline strings in cluster snapshots/offers remain diagnostic:
they are not instructions requiring a peer to execute that named graph.
Current `media_pool` validation accepts bounded nonempty strings; it does
not reject an unknown enum value. `RemoteStartRequest` carries a requested
presentation/session, not a required processing-graph identifier.

Keep detailed Mac graph classes node-local. The receiving worker resolves
its own immutable plan using its capabilities and saved settings and must
satisfy the requested presentation and decoder constraints before acceptance.
A coarse snapshot is only a prefilter, never proof that a graph or exact
presentation is executable. M3 must verify that candidate/offer-to-start
revalidation covers a changed capability generation: replan or decline
through the existing owner rather than execute a stale plan or claim a
stale cache hit. The actual worker plan/recipe determines the artifact key.

An old worker may legitimately use its incumbent renderer for a request a
new Mac handles with Metal. It need not recognize the other worker's graph
name. Test both old-to-new and new-to-old dispatch and ensure unsupported
presentation contracts are declined. Takeover always resolves on the new
worker; a changed recipe creates new output under existing replacement and
publication fences, never an append to the previous producer's artifact.
Completed immutable artifacts may be served across platforms only when the
verified identity matches.

Do not introduce new required graph fields under the existing protocol
version. If implementation discovers a need to mandate remote graph
identity or change the worker acceptance contract, revise the plan to include
a versioned request/capability envelope and explicit mixed-version refusal
before implementing that extension. An enum added to a new binary cannot
teach already deployed binaries to reject an unfamiliar string.

## 6. Experiment — separate attribution from the product result

### 6.1 Hardware and corpus

Measure at least the target production Apple Silicon Mac. A second Apple
Silicon generation is required before claiming broad Apple Silicon support;
Intel Mac support is separately measured and not implied. Capture model,
SoC, RAM, OS build, native/translated architecture, power mode, thermal state,
headless/display/session context, storage and FFmpeg/library hashes.

| Input | Required variation / reason |
|---|---|
| 1080p H.264 SDR | Light-work control; startup overhead can dominate |
| 2160p HEVC SDR | 8/10-bit as supported, 24 and 60 fps; scale and decode costs |
| 2160p HDR10 | 24/60 fps, 1000/4000-nit metadata, missing peak metadata |
| HDR color charts | Near black, highlight ramp, saturated wide-gamut patches, gradients and scene cuts |
| HLG | Later independent HDR-to-SDR row |
| Dolby Vision | P5 RPU-positive sample; separate P7/P8 routing controls |
| Interlaced broadcast | 1080i TFF/BFF and progressive 720p59.94 control |
| Subtitles/captions | PGS, ASS, A/53-positive MPEG-2 and DVR capture |
| Presentation edges | Non-square SAR, rotation, odd source dimensions, VFR and seek/resume |

Use existing [bench fixtures](../../scripts/bench) and
[interlace fixtures](../../tests/playback/interlace-fixtures.sh) where suitable.
Synthetic metadata-tagged patterns alone are insufficient HDR/Dolby visual
references. Retain a licensed or private corpus by hash with provenance;
never commit private movie bytes or redistribute them as test fixtures.

### 6.2 Controlled comparisons

A is the exact current production graph. B uses the same FFmpeg build,
decoder, output codec, bitrate/rate control, geometry, cadence, audio,
segment policy and subtitle choice, with only processing changed. When a
new FFmpeg build is required, run A and B on that new build as well as A on
the deployed build; otherwise packaging changes confound the result.

Record unavoidable differences, such as NV12 versus planar YUV or the
native tone curve, as part of the treatment. Never attribute the combined
gain solely to memory-copy removal.

Add diagnostic variants only when they preserve the same mathematical work:
map versus copy the same format; CPU versus VT scaling at identical geometry;
CPU versus native/Metal tone mapping at a fixed output; processing-only
microbenchmarks where achievable. Unsupported mapping is a reported result,
not a reason to invent an equivalent graph. These variants explain costs;
only complete production sessions establish a user-visible improvement.

### 6.3 Sampling and measurements

Use clips of at least 60 s for sustained runs. Perform five paired repetitions
with alternating A/B order, one explicit cold run and separately reported
warm runs. Do not force OS cache purges; label cold-process versus cold-file
conditions precisely. For startup distributions, collect at least 30 starts
per accepted route, separating existing-cache hits from real encoding starts.

Test one session first, then two and four when capacity allows. Run a 30 min
soak at the proposed supported concurrency, with no unrelated encoding jobs.
Retain failures and slow runs. Do not advertise capacity from a short burst.

| Metric | Collection and interpretation |
|---|---|
| Throughput | Media seconds / wall seconds, plus 5 s steady windows; FFmpeg final speed alone is insufficient |
| Startup | Request to first publishable media, and client request to first presented frame, using existing event origins |
| CPU cost | Child user+system CPU seconds / output media second; report core-equivalents rather than ambiguous CPU percent |
| Energy | External meter if available, otherwise supported `powermetrics` sampling with sampler/version recorded; idle-subtracted joules per media minute |
| Memory | Peak resident memory and available memory-pressure signals; do not sum shared mappings as physical allocations |
| Synchronization/copies | Instruments trace or exact-source instrumentation of mapping, allocation and copy boundaries; distinguish measured bytes from estimates |
| Video correctness | Decoded frame/timestamp/color checks, visual comparisons and metric reports in the same output color space |
| Streaming behavior | Rebuffering, reserve evolution, seek/resume, replacement and cancellation/resource release |

Missing energy or copy instrumentation is `unmeasured`, not zero. Hardware
codec activity need not appear as Metal GPU utilization. Do not require a
high GPU percentage to call a hardware encoder active.

### 6.4 Proposed acceptance thresholds — review before measuring

All correctness and delivery checks pass first. Then a candidate is worth
promotion for a particular workload if it achieves at least one of:

- At least 20% higher median sustained throughput.
- At least 20% lower CPU seconds per media second, with throughput within 5%.
- At least 15% lower joules per media minute, with throughput within 5%.

Require consistent improvement in at least four of five paired sustained
runs. A noisy or contradictory result triggers more measurement, not a pass.
These are proposed engineering thresholds; they are not observed results
and must be settled by review before examining candidate scores.

For a supported realtime workload, the tenth percentile of steady 5 s
throughput windows must be at least 1.25× at the advertised concurrency.
At 30 or more starts, candidate p95 startup may not exceed baseline p95 by
more than the larger of 100 ms or 10%. No new playback errors, stalls,
A/V discontinuities or unbounded memory growth in the soak. Report any
tradeoff outside these bounds for a decision rather than averaging it away.

For SDR scaler comparisons, use aligned decoded references and report
PSNR/SSIM/VMAF where available, plus visible ringing, aliasing and banding.
For HDR-to-SDR, do not score raw HDR against SDR or rank tone curves by
VMAF alone. Require blinded A/B inspection on a named calibrated display,
with highlight/shadow/gamut/temporal findings per clip. Record the reviewer
and verdict. Do not invent a universal numeric tone-map quality threshold.

## 7. Shipping, diagnostics and rollback

Pin the selected FFmpeg source revision, patch set, build recipe, architectures,
minimum macOS target and checksums. Audit code-signing/distribution needs and
runtime library resolution in the existing native install workflow. A fork
upgrade must retain all filters required by the incumbent, not only the new
Mac graph. Do not install Homebrew's unrelated executable over the daemon's
configured one.

Expose saved preference, chosen processing variant, decoder, surface format,
output grade, implementation revision and reason for incumbent selection in
existing diagnostics. Use bounded enum labels for counters; put build hashes
and file-specific facts in structured logs, not Prometheus labels. Existing
pipeline counters must include every new variant exactly once at the existing
accepted-start boundary.

Developer card readiness reports local filter/runtime observations and the
offline evidence still owed. It must carry a concrete graduation line:
“Leaves Developer when the supported Mac workload matrix and playback
regressions pass. Then moves to Playback → Advanced server delivery.”
The proposed permanent switch remains useful for OS/driver regressions;
grade/codec experiments get their own explicit control when implemented.

Rollback turns the preference off for new plans and restores the prior pinned
FFmpeg package if needed. Running sessions keep their captured implementation
until they finish or use existing recovery. Retain the old executable while
referenced, or drain workers before package replacement. Cache invalidation
is by identity mismatch, never deletion or relabeling.

## 8. Decisions requested from Opus

1. Is the initial SDR/HDR10 scope small enough, and are its unsupported-source
   boundaries explicit without creating hidden feature gates?
2. Is a node-local Mac capability collection alongside the legacy selected
   pipeline sufficient, or does a concrete caller require a broader change?
3. Can `DecodeSurfaceContract` express every required mapping/format step
   without conflating representation with physical memory placement?
4. Are native versus Metal tone-map choices and Dolby requirements testable
   with the proposed fixtures? Which fork revision should M1 pin?
5. Are the benchmark comparisons and thresholds fair, particularly startup,
   energy attribution and differing tone curves?
6. Does fallback preserve grade, decoder-recovery policy, cache identity and
   publication ownership in both VOD and rolling engines?
7. Is implementation identity sufficiently conservative across OS changes and
   mixed nodes without unnecessarily invalidating existing non-Mac caches?
8. Which extensions should follow the first release: HLG/Dolby, subtitles,
   Live TV, or HEVC/Main10? Each has a separate acceptance boundary.

No source inspection or document approval establishes a performance win.
The execution record must contain the measurements before a default changes.
