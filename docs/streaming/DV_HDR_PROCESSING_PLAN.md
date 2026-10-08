# Dolby Vision processing — preserve FEL and measure the result

**Status:** open — proposal for gated feasibility work; no production behavior
changed · **Updated:** 2026-10-08 · **Inspected base:**
`a3158eadc9a4f26d2d8293b4a91f23a94c9012b6`

This proposal answers how Plurx can use more of a Dolby Vision source for
HDR10 output and Profile 7 to 8.1 delivery. The
[build handoff](DV_HDR_PROCESSING_BUILD.md) gives Sol 6.1 implementation tasks,
contracts and acceptance tests. The [review record](DV_HDR_PROCESSING_REVIEW.md)
tracks adversarial findings. Existing behavior is documented in
[DV delivery findings](DV-DELIVERY-FINDINGS.md) and
[permanent conversion status](M5B_STATUS.md).

## 1. Decisions and scope

Prefer the original picture information, including Profile 7 full enhancement
layer (FEL) residuals, when a qualified path can preserve it. Keep native DV
copy for compatible clients. Improve two destinations independently:

| Destination | Preferred new path | Required fallback |
|---|---|---|
| HDR10 client without native DV | Reconstruct original picture, apply supported DV rendering, encode HDR10 | Qualified partial processing, then existing compatible HDR10 base path |
| DV client requiring P8.1 instead of P7 | Reconstruct original picture, encode a new compatible base and produce metadata valid for it | Existing P7 to P8.1 base copy and metadata conversion |

Fallback is part of the feature, not an error that should prevent playback.
Profile 5 has no HDR10-compatible base: preserve its existing qualified color
conversion or existing valid lower-grade route; never label raw P5 as HDR10.
HLG-compatible sources need their own qualified HLG-to-PQ conversion before
being called HDR10. Unknown profiles keep existing supported playback policy;
“DV processing enabled” cannot make an unsupported profile renderable.

These are playback preferences, not permission to rewrite library originals.
The permanent conversion worker remains a separate product decision. There
is no requirement to implement Dolby Vision display signaling on Linux: the
HDR branch produces ordinary HDR10 for the already working ROG HDR path.

The earlier normalization-first proposal is superseded. P7 to P8.1 metadata
normalization discards FEL information before a renderer sees it. Retain it
as a clearly reported partial-processing fallback, not the preferred quality
path. FEL reconstruction, creative display mapping and metadata authoring
are three separate capabilities; proving one does not prove the others.

## 2. What changes from the current code

The existing [converter](../../crates/plurx-core/src/transcode/dvconvert.rs)
and [fMP4 adapter](../../crates/plurxd/src/dvpipe.rs) rewrite RPUs while copying
compressed base picture bytes. This is not a quality re-encode. The vendored
`ConversionMode::To81` removes residual dependence and FEL mapping as needed;
it cannot recover enhancement-layer picture data.

Current [playback policy](../../crates/plurx-core/src/playback/mod.rs) chooses
native copy, eligible P7 conversion, compatible-base stripping or re-encode.
The [pipeline definitions](../../crates/plurx-core/src/transcode/pipeline.rs)
include P5 `DoviPassthrough` using tonemapx and ordinary `Hdr10Passthrough`.
Compatible-base P7/P8.1 output normally does not render DV metadata.

The pinned [Jellyfin tonemapx patch](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.3-1/debian/patches/0053-add-simd-optimized-tonemapx-filter.patch)
requires `disable_residual_flag && vdr_rpu_profile == 0` for its DV transform.
Do not widen that P5 route to every DV profile. Successful process exit and
an HDR tag cannot prove correct colors.

The ROG's [libplacebo 7.360.1 mapper](https://github.com/haasn/libplacebo/blob/v7.360.1/src/include/libplacebo/utils/libav_internal.h)
skips residual-dependent DV mapping. Newer upstream is more promising:
[libplacebo renderer API at 0d043c7](https://github.com/haasn/libplacebo/blob/0d043c7f6f79cd3687c023454bdacbe615e4d96f/src/include/libplacebo/renderer.h)
accepts an enhancement-layer frame, and
[mpv at 36abaa3](https://github.com/mpv-player/mpv/blob/36abaa32d00a7229ee206aae12dc0e97e7962dca/video/out/vo_gpu_next.c)
attaches it with API version 367 or later. The same mpv path explicitly falls
back to base-only rendering when enhancement-layer setup fails. These are
research pins, not qualified Plurx dependencies. Neither establishes that
our FFmpeg CLI filter can decode, pair and deliver FEL to the renderer and
export the result for encoding. That bridge is the first feasibility gate.

DV metadata also contains target-specific creative trims. The inspected
mapper's reshaping, source bounds and L1 scene statistics do not establish
complete L2/L8 processing. Report the supported subset; do not advertise
Dolby-certified or complete creative-intent reproduction without evidence.
See Dolby's [metadata-level description](https://professionalsupport.dolby.com/s/article/Dolby-Vision-Metadata-Levels).

## 3. Architecture and two independent feasibility gates

```text
immutable original + timestamped BL / EL / RPU
                    |
          validated decode and pairing
                    |
          FEL reconstruction when supported
                    |
       scene/picture representation with known semantics
          /                                      \
 supported DV display mapping          P8.1-compatible base encoding
          |                             + valid metadata adaptation
 HDR10 encoding + static metadata                   |
          |                               P8.1 conformance validation
 HDR10 client                                       |
                                             compatible DV client
```

This is a semantic boundary, not a claim that upstream exposes an intermediate
frame with exactly these properties. M0 must establish whether such a frame
can be obtained, its color domain/precision, and which RPU operations have
already been applied. If only final display-mapped frames are exposed, the
HDR branch may proceed; the shared P8.1 branch remains unresolved.

Keep intermediate precision through reconstruction and mapping; a 10-bit
output cannot retain a literal 12-bit signal. It can retain some contribution
from the reconstructed picture after quantization. A new encode can also
lose more detail than FEL recovered, which is why output quality is a gate.

For P8.1, do not simply inject the old RPU into new pictures. The encoded base
must have the expected compatible color representation, and retained or
regenerated metadata must describe that base. A backend must demonstrate a
valid adaptation algorithm, frame correspondence, residual-disabled output
and no duplicate reshaping or display mapping. Lossy encoding does not by
itself require arbitrary regeneration of every creative trim, but changing
the picture domain invalidates assumptions that copying metadata is safe.
If no validated authoring/adaptation backend is available, keep this branch
experimental and deliver existing P8.1 fallback. No placeholder “FEL P8.1”
route may be selected while this gate is unresolved. Independently decode
the new P8.1 base with DV processing disabled and validate its HDR10
compatibility; a plausible DV-rendered picture alone cannot prove that promise.

For HDR10, remove DV configuration/RPUs from delivered output after applying
the supported operations. Set PQ/BT.2020, range and Main10 correctly; derive
or validate static luminance metadata against the actual rendered output.
Do not copy mastering/MaxCLL fields blindly from a differently mapped input.
Use a documented target peak, black level, gamut and mapping policy. The
compositor's 10,000-nit working range is not the panel's measured peak. Until
client luminance is trustworthy, use an explicitly named fixed rendition
policy qualified on physical displays, not an invented panel measurement.

## 4. Settings, reporting and fallback

Proposed new persisted booleans, both initially **off** for opt-in rollout:

| Store key / API field | User label | Scope |
|---|---|---|
| `playback.dolby_vision_hdr_processing` / `dolby_vision_hdr_processing` | Process Dolby Vision for HDR output | Prefer qualified DV rendering when delivering HDR |
| `playback.dolby_vision_fel_reencode` / `dolby_vision_fel_reencode` | Preserve FEL when converting to Profile 8.1 | Prefer qualified reconstruction and new P8.1 encoding |

Defaults are a proposal, not a previously confirmed user preference. These
are separate from existing `playback.dolby_vision_convert`. That existing
conversion permission must still allow P7 conversion before either P8.1
strategy is selected. The new FEL preference chooses how permitted conversion
is performed; it does not enable conversion by overriding the old switch.
Turning either new preference off preserves existing valid P5 handling and
native DV delivery. Saved changes apply to new playback decisions, not a
running movie.

Put both controls in Settings → Developer while incomplete, with a
`devGraduation` entry naming the missing qualification and Playback as the
destination. Readiness is advisory: never disable the switch, reject Save or
rewrite a saved choice because one worker cannot honor it. Runtime capability
checks select safe fallbacks without acting as a hidden feature gate.

Playback details must distinguish source, applied processing and output:
“Source: DV P7 FEL; processing: FEL reconstructed, supported RPU mapping;
output: HDR10”, or “Source: DV P7 FEL; processing: base only; output: DV P8.1;
reason: reconstruction unavailable.” A generic HDR badge is insufficient.
Do not infer applied processing from a command-line option. Record actual
EL/RPU acceptance and supported operations, including base-only fallback
inside a renderer.

Choose only among paths allowed by existing client, subtitle, force-playback,
encoder and admission policy. If no enhanced worker has capacity, use a
qualified partial route or the existing compatible base route promptly.
Preserve the current valid SDR fallback when HDR itself is unavailable.
After publication begins, a failure must retire that generation and reselect
at a verified timeline point; never substitute different pictures/metadata
under the same initialization segment or cache identity.

## 5. Quantifying quality: fidelity, visibility and cost

There is no defensible universal “FEL is X% better” figure. Measure each
source against the same independent reference at the same display target,
then report how much error each route adds. The experiment has three parts:

| Question | Controlled comparison | What it establishes |
|---|---|---|
| What does FEL contribute? | Reconstructed versus base-only intermediate pictures, before delivery encoding | Residual contribution without encode loss |
| Does P7 to P8.1 improve? | Existing base-copy P8.1 and reconstructed/re-encoded P8.1, both rendered by the same validated DV reference path | Net delivered fidelity, including metadata adaptation and encode loss |
| Does DV to HDR10 improve? | Base-only HDR, supported BL/RPU rendering, and FEL plus supported RPU rendering | Metadata and FEL contributions, separately and together |

Each output is compared to the original BL+EL reference rendered for the
same target. The renderer under test cannot serve as its own independent
oracle. Use an independently validated decoder/renderer or a trusted reference
render with provenance. A licensed playback device may support viewing tests,
but its presence alone does not supply reference pixels. Synthetic fixtures
with analytically known residuals establish reconstruction correctness even
when a complete commercial DV reference is unavailable.

Without an independent reference, report differences and blind preferences;
do not turn them into fidelity improvements or conformance claims. A valid
P5 control must still perform required color normalization; raw P5 tagged PQ
is not a valid “processing off” comparison. P8.4/HLG needs a separate study.
Identity RPUs and near-zero FEL effects are valid outcomes, not failed probes.

Use absolute luminance and color-calibrated comparisons, not ordinary SDR
screenshots or a default SDR VMAF score:

| Measurement | Report | Interpretation |
|---|---|---|
| [Delta E ITP, ITU-R BT.2124](https://www.itu.int/rec/R-REC-BT.2124/en) | Mean, p95, p99 and worst-frame color/luminance difference to reference | Potential visibility of error; not a standalone preference verdict |
| [PU21](https://www.repository.cam.ac.uk/items/f011f7ac-170c-40f8-bb6d-7141c62d6aad) PSNR and SSIM | Per-scene and pooled scores using pinned luminance/encoding conventions | HDR perceptual-domain fidelity and structural comparison |
| Signal diagnostics | Clipped/crushed pixel fraction, highlight and shadow crops, banding, frame/metadata alignment | Finds defects that averages hide |
| Temporal diagnostics | PTS correspondence, scene-cut behavior, unexpected flicker and error over time | Prevents a good single-frame score hiding motion defects |
| Blind viewing | Randomized paired preferences, ties, viewer/clip counts and uncertainty | Whether measured differences are visible or preferred on tested displays |
| Processing cost | Sustained speed, startup/seek delay, CPU/GPU/memory and concurrent sessions | Whether improvement is practical for playback |

Freeze raster, frame timestamps, range, chroma reconstruction and target
luminance before measurement. No per-image normalization, exposure fitting,
spatial registration or independent tone mapping that conceals errors.
Report confidence intervals by clip or viewer, not by treating millions of
correlated pixels as independent observations. Treat small samples honestly.
No metric proves creative intent when the reference itself lacks L2/L8 trims.

Measure at both a common high-quality intermediate and final delivery
bitrates. Compare encoder paths at matched **measured bitrate**, resolution,
frame rate, chroma format and encoder settings; equal CRF is not equal rate.
Keep the existing copied base as the real-world baseline even if its bitrate
differs. Separately re-encode both candidate picture paths to common rates to
isolate processing quality. Report compression error against each path's own
pre-encode picture as well as total error against the independent reference.
Use several rates; only calculate rate savings where distortion curves have
a valid overlapping interval. Never extrapolate a single sample into a
library-wide gain.

Before qualification, pre-register tolerances against measurement noise,
reference uncertainty and synthetic quantization bounds. Require correct
signaling/timing, no systematic clipping or temporal regressions, and a net
quality benefit on the intended workload at the selected rates. Report
individual regressions and the worst scenes even if the average improves.
If the advantage disappears under delivery compression, retain the working
fallback for that unqualified route rather than call the extra encode better.

## 6. Evidence already collected


The ROG's separately installed Ubuntu tools report FFmpeg `8.1.2-2ubuntu2`
and libplacebo `7.360.1`. These are development evidence, not qualification of
Plurx's pinned Jellyfin package or its production encoder nodes.

The official [Dolby test repository](https://github.com/DolbyLaboratories/dolby-vision-contents)
provided `BL_RPU_dvhe-08-mapDynamic1000-81_1920x1080@24fps_0_6313.mp4`.
Downloaded bytes: 139,976,302; SHA256:
`5f9e9b1a6892853f95cad06a0c104bd0d857580ea0ea52bca2ef2034c1e11334`.
Probe: HEVC Main10, DV Profile 8 level 3, compatibility ID 1, RPU present,
no enhancement layer, duration 263.083333 seconds.

The two off-screen runs used software HEVC decoding and the RTX 4080 Laptop
GPU's libplacebo renderer, differing only in `apply_dolbyvision=0` versus `1`:

```text
ffmpeg -hide_banner -loglevel verbose -ss 30 -i p81.mp4 -an -sn \
  -vf libplacebo=w=640:h=360:apply_dolbyvision=VALUE:colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084:range=tv:format=yuv420p10le:dithering=none \
  -frames:v 48 -f framemd5 -
```

Both exited zero and decoded without reported errors. All 48 frame hashes
differed. A second comparison kept the original 1920x1080 raster and captured
one frame at the same seek point as raw 10-bit YUV. Of 3,110,400 samples,
844,194 differed; maximum absolute difference was 7 code values and PSNR
between the two outputs was 65.4458 dB. Luma maxima were 726 and 727.
These values establish different rendering, not which output is more accurate.
The 640x360 timing is not production-resolution transcode throughput evidence.

Host evidence is under
`~/native-hdr-test-2026-10-08/dv-hdr-research/`: the source asset,
probe, two frame-hash outputs and logs, full-raster raw frames and logs, and
`p81-pixel-comparison.txt`. Small text evidence was also copied to the local
session directory `/tmp/plurx-dv-hdr-evidence/`. The large sample is not a
repository fixture, and temporary local evidence is not a durable CI receipt.

## 7. Delivery stages and open questions

The [build handoff](DV_HDR_PROCESSING_BUILD.md) defines M0–M6. Start with the
bounded capability and benchmark spike. Do not authorize product routing from
filter discovery, a version number, a changed hash or a successful process.

Open questions are explicit: can the chosen decoder export matched BL/EL/RPU;
can reconstruction be exported before display mapping; can a backend create
valid P8.1 metadata for that picture; which creative metadata is supported;
and does end-to-end quality improve within production resource limits?
HDR processing can qualify independently while enhanced P8.1 remains open.
FEL support is an objective with a feasibility gate, not a promised completed
backend. Build receipts, measured quality and physical playback evidence are
required before either experimental control graduates.
