# Dolby Vision M0 — existing tooling and reuse findings

**Status:** open — source investigation and bounded experiments; end-to-end
FEL/HDR10/P8.1 qualification remains open · **Updated:** 2026-10-08 ·
**Investigators:** two GPT-6.1 Sol sessions, FEL/HDR and P8.1/quality.

Companion to the [proposal](DV_HDR_PROCESSING_PLAN.md),
[build contract](DV_HDR_PROCESSING_BUILD.md) and
[project queue P01/P02](../features/PROJECT-BACKLOG.md). This records what the
existing ecosystem can contribute before Plurx writes another renderer.
No production library, playback route or ROG installation changed.

## 1. The idea already has implemented precedents

[mpv's manual](https://mpv.io/manual/stable/) documents DV-aware HDR output,
including per-scene luminance derived from dynamic metadata.
[JRiver's release record](https://wiki.jriver.com/index.php/Release_Notes_Media_Center_v21_To_v34_%28Windows%29)
records non-passthrough DV rendering and per-scene brightness processing.
These establish implemented player features, not usage counts, complete trim
coverage, FEL support in every released build or a measured universal gain.

[vs-placebo 2.0.3](https://github.com/Lypheo/vs-placebo/releases/tag/2.0.3)
explicitly adds P7 FEL. [DoViBaker](https://github.com/erazortt/DoViBaker)
reconstructs BL/EL/RPU into PQ picture data, while
[DoVi_Scripts](https://github.com/R3S3t9999/DoVi_Scripts) includes actual
FEL-baked HDR10 and P8 encode workflows. “HDR10-E” is Plurx's proposed badge
for an effective qualified processing path, not an existing transport standard.

## 2. Reuse candidates and the work they do not remove

| Candidate | Reusable work | Integration boundary |
|---|---|---|
| libplacebo + mpv integration | GPU reconstruction, reshaping/color stages, target rendering and example dual-layer plumbing | Plurx still needs decoder/frame association, export for encoding, actual-operation receipts and producer ownership. A player path is not an FFmpeg filter implementation. |
| vs-placebo 2.0.3 | Linux-capable VapourSynth BL/EL input bridge into libplacebo and downloadable HDR frames; LGPL-2.1 | Pin a FEL-capable libplacebo build, validate indexing/timing and isolate mutable metadata state; no existing Plurx receipt or admission contract. |
| DoViBaker `ffba39830b694ddca0bf5f73dcf1b462713bb7f4` | CPU reconstruction into PQ12 carried by RGB16, before default display management; GPL-3.0 | Restricted offline cross-check first. Validate matrix domain, residual bounds/rounding and Linux architecture build. Optional experimental CM2.9 trims do not prove CM4/L8 support. |
| DoVi_Scripts 3.1.4 job 8.6.1 | Existing reconstruction-to-P8.1 recipe, using DoViBaker plus x265/NVEnc and adapted RPU | Windows interactive wrapper, external tool pack and no license file found in inspected tree. Reuse the understood technique and qualified dependencies; do not copy or distribute its batch/tool pack by assumption. |
| Existing Plurx `dolby_vision` / `To81` | RPU parsing, editing, serialization and compatible conversion building blocks | Does not reconstruct FEL or establish that metadata describes a newly encoded picture. |

The concrete HDR export candidate is vs-placebo
`dbfc5e45b85c12857f44fecf73133bc84c27560d` with its source-wrap libplacebo
`a7a18af88ff0a17c04840dcb3246047bb6b46df3`. Wheel recipes using floating
libplacebo `master` are not a reproducible substitute. The newer FFmpeg split
reference is `bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa` (n9.0.1).

Source inspection identified specific traps to test rather than hide:

- Production Jellyfin FFmpeg 8.1.3 lacks the newer `dovi_split` bridge used by
  the inspected mpv integration. Its HEVC path skips outer NAL63 EL, and its
  `vf_libplacebo` does not attach a separate enhancement-layer frame. Merely
  upgrading libplacebo cannot connect that path.
- The inspected vs-placebo code compiles EL attachment only with sufficiently
  new libplacebo API (369 in that source), although its declared general minimum
  is older. BL/EL frames are paired by frame index, and RPU lookup prefers EL
  properties. Validate source timing explicitly and inspect shared mutable
  metadata for cross-clip/out-of-order contamination before concurrent use.
- DoViBaker also relies on frame-index association. Its inspected color path
  applies YCC-to-RGB without consuming every possible RGB-to-LMS metadata
  transform; restrict accepted source domains and independently compare them.
  Different rounding/clamping from libplacebo is not automatically an error
  in either implementation, and needs a trustworthy reference.

These are inspected candidate boundaries, not a claim that all versions or
forks have the same behavior. Recheck exact revisions before an upstream patch
or deployment. Confirm each dependency's license/build/distribution obligations
for the selected packaging rather than treating a subprocess as an exemption.

## 3. The existing P8.1 recipe is a useful hypothesis

The inspected [DoVi_Scripts 3.1.4 batch asset](https://github.com/R3S3t9999/DoVi_Scripts/releases/tag/3.1.4)
has SHA256
`20e30b5d29ade552de3ca9166d23e137676ca95fa49148b214360111a9612dfb`.
Job 8.6.1:

1. Keeps the original P7 RPU for `DoViBaker(bl, el, rpu=original)`.
2. Separately converts output metadata with mode 2 and `remove_cmv4:false`.
3. Converts reconstructed RGB/PQ/BT.2020 full range to limited-range
   YUV420P10, BT.2020 non-constant luminance/PQ.
4. Encodes with x265 or NVEnc and the edited P8.1 RPU.

Default reconstruction does not bake display-management trims. This route
therefore suggests retaining compatible creative metadata after clearing old
residual/reshaping dependence, rather than generating every metadata block
from scratch. Its default branch does not run fresh content analysis; other
jobs provide separate regeneration.

The wrapper's initial metadata sampling and tail-RPU duplication/removal on
frame-count mismatch are not acceptable timestamp proofs for Plurx. No
independent DV-on/DV-off pixel-reference gate was found in that encode path.
Its existence makes the route worth testing, not qualified for our clients.
A pre-display-mapping BT.2020/PQ export may suffice even without raw VDR-domain
export, but only after its semantics and output RPU mapping are proved. Avoid
baking mapping and having the DV client apply it again.

## 4. Measurement foundations and missing references

A bounded offline numerical spike checked the metric formulas; it did not
render a Dolby Vision movie or measure a fidelity gain:

| Control | Observation |
|---|---|
| Nine upstream Colour Delta E ITP vectors | Maximum absolute difference below `3.55e-8` |
| BT.2100 RGB-to-ICtCp reference vector | Maximum difference below `4.70e-9` |
| PU21 scalar versus 50-digit decimal arithmetic | Maximum difference below `1.14e-13`; MATLAB/SSIM parity not run |
| 21 NLQ normalization algebra controls | Maximum difference below `5.56e-17`; equations derived from candidate source, so not independent Dolby conformance evidence |

A separate C spike recorded 15 synthetic packet observations for the newer
FFmpeg `dovi_split` filter: packet timing/property preservation, EL extraction,
mode behavior and missing-EL refusal. Its payloads were not decodable P7/RPU,
so it proves packet mechanics only. A local Vulkan probe returned
`VK_ERROR_INCOMPATIBLE_DRIVER`; no GPU frame export or full backend runtime
was qualified. No driver/system-library installation was attempted.

Research pins: [PU21 `78340c0`](https://github.com/gfxdisp/pu21/tree/78340c0c4c20908c6bdcf0931dfde763c53a24ad)
and [Colour `248121e`](https://github.com/colour-science/colour/tree/248121e33fcae458e62190ac872f3e16ab2044bf).
The PU21 helper's luminance weights are BT.709; supply BT.2020 luminance
explicitly (`0.2627, 0.6780, 0.0593`) for this corpus. Pin `banding_glare`,
absolute cd/m², clamp convention and the metric's peak parameter; the inspected
PU21 PSNR convention uses 256, not its encoded 10,000-nit value. Disable
camera-response/exposure fitting because those errors are part of our comparison.

[Netflix Open Content](https://opencontent.netflix.com/) offers HDR masters and
DV metadata useful for reference work, but those assets do not themselves
supply a paired P7 FEL encode and reconstruction oracle. Match mastering and
target assumptions before comparing alternate deliveries. Dolby's professional
analysis/render tools are candidates; this investigation did not establish
local access to a licensed reference renderer. A legally usable paired FEL
fixture and independent DV pixel reference are still missing.

Temporary scripts/receipts were retained in the two sessions' scratch folders.
The values above are an investigation summary, not portable release evidence.
M0 must retain a reproducible source/command/artifact bundle with the actual
backend spike before claiming its capability or using metrics for qualification.

## 5. Recommended next build

Start an isolated **vs-placebo/libplacebo HDR10-E spike**, pinning the complete
build and observing accepted EL/RPU frames. Use **DoViBaker as a restricted CPU
cross-check** and investigate its pre-display picture as a P8.1 source.
Preserve actual timestamps, reject unsupported matrix/metadata domains, and
run zero/nonzero, missing/mispaired EL and double-processing controls.

Then test the P8.1 adaptation recipe against both DV-enabled rendering and
independent HDR10-base decoding. Qualify output/metadata and matched-bitrate
quality before resource/performance admission or production routing. Keep the
existing base fallback throughout. The evidence so far supports reuse-based
experiments for **both** destinations; neither destination is yet qualified.
