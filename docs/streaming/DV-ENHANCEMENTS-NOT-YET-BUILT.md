# Dolby Vision enhancements that are possible but not yet built

**Status:** open · **Updated:** 2026-10-18 · **Backlog:** P30 in the
[project backlog](../features/PROJECT-BACKLOG.md)

These came up during the HDR10-E "never plays" investigation of 2026-10-18
(PR on `fix/hdr10e-all-resolutions`). That PR makes HDR10-E work at every
resolution: GPU decode, GPU colour conversion, no 720p cap, no speed gate.
It also adds the FEL / RPU / DYN indicator. The items below are separate
work that nobody has designed or built yet. Each entry says what it would
give the viewer, what exists today, and what building it involves.

## 1. HDR10+ dynamic metadata from Dolby Vision (HDR10-E output)

**What the viewer gets:** on an HDR10+ display (Samsung TVs, many Android
TVs, recent Apple TV and Apple displays), per-shot tone mapping instead of
one static curve for the whole film. Today HDR10-E is static HDR10, so a
dark shot and a bright shot share one curve.

**What exists:** the helper parses every RPU and validates L1 (per-shot
min/avg/max PQ), L2 and L8 (trims for specific target displays), L5 (active
area), L6 (MaxCLL/MaxFALL), L9 and L11. It deliberately applies only the
master reconstruction (`creative_trims_applied: false`). Nothing is written
into the output beyond static HDR10 metadata.

**Building it:**
- Map DV L1 to SMPTE ST 2094-40 per scene: maxscl from the L1 max, average
  maxrgb from the L1 avg. The distribution percentiles have to be estimated
  (DV carries no histogram); measuring them from the reconstructed frames is
  more accurate. L2/L8 trims have no direct HDR10+ equivalent; the bezier
  knee/anchors could approximate the 1000-nit trim.
- Write it into the bitstream as an SEI per frame. x265 accepts it
  (`--dhdr10-info`); hevc_vaapi and hevc_nvenc do not. For the hardware
  encoders this needs a post-encode SEI insertion step in the daemon's
  completed-window path (the same place P8.1 authoring rewrites samples).
- Signal it to clients: an `HDR10+` grade in caps, playlist
  `SUPPLEMENTAL-CODECS` where supported, and a DYN badge for HDR10-E when
  it is present.
- Prove it on one HDR10+ display per client family (the narrow real check,
  not a matrix).

**Shipping:** the display-targeted variant maps per-shot L1 on the server for
the viewer's display peak (#1271); the L2/L8 trims are still open. Build plan:
[DISPLAY-TARGETED-DV-BUILD.md](DISPLAY-TARGETED-DV-BUILD.md).

## 2. Dolby Vision → SDR using the RPU's own trims

**What the viewer gets:** on an SDR display, the colourist's SDR grade (the
L2/L8 trims at 100 nits) instead of a generic HDR→SDR tone map.

**What exists:** `DvDestination` has only `Hdr10` and `Profile81`. SDR
clients get the ordinary tone-mapped path.

**Building it:** add an SDR destination: reconstruct, then apply the
L1/L2/L8 trim for a 100-nit target through libplacebo's DV display mapping
in the helper, with a BT.709 export. Same pipeline and qualification as
HDR10-E. This is where "DV metadata at render time" pays off most.

**Planned:** phase 3 of [DISPLAY-TARGETED-DV-BUILD.md](DISPLAY-TARGETED-DV-BUILD.md).

## 3. FEL vs MEL known at scan time

**Today:** the catalog stores only `el_present`. A P7 source is assumed to be
FEL (`DvProcessingMode::Fel` at preparation), so a MEL source refuses its
first window and falls back. The real kind is only seen while converting
(fragindex logs `enhancement_layer=Full`) or inside processing selection.

**Building it:** read the RPU `el_type` (or the conversion pass's
`EnhancementLayer`) at scan and store it with the DV facts. Pick BaseRpu for
MEL. Show "MEL: no picture detail to recover" in the indicator details.

## 4. Profile 5 through the helper

**Today:** HDR10 intent for P5 still needs the ffmpeg DV renderer proof
(`dovi_p5_render`, `require_dovi_renderer`), even though the helper does the
reconstruction itself. Nodes without that proof deliver P5 as SDR.

**Building it:** let a P5 source reach HDR10 output whenever the helper is
installed and the Developer setting is on, without the ffmpeg-renderer
requirement.

## 5. Processed output on every route

- **Continuous adaptive route (Android Auto ABR):** there is no processed
  rendition, so the indicator says "not on the continuous route yet".
  Needs processed renditions in the continuous family.
- **Multiple processed rungs:** today one processed rendition at the
  selected height. An ABR ladder of processed rungs would let Auto move
  between them.
- **Burned-in subtitles:** the reconstructed graph refuses `-filter_complex`.
  Needs the subtitle overlay drawn in the helper (PQ-correct, on the GPU)
  or after the YCbCr export.
- **Live HLS recovery:** the rolling engine has no processing graph, so a
  recovery degrades to the ordinary rendition.
- **High frame rates:** a processed window holds at most 64 pictures (the
  helper's coded-window bound). A 2 s segment at 50/60 fps is 100/120
  pictures, so high-frame-rate DV (phone video, sports) needs shorter
  processed segments or a larger helper window.

## 6. Dolby Vision profiles not handled

- **P8.4 (HLG base) and P8.2 (SDR base):** refused by the preflight.
  P8.4 → HLG-E or HDR10-E is the useful case (broadcast and phone content).
- **P7 dual-layer passthrough:** no client in the fleet decodes dual-layer,
  so plurx converts. Only worth building if a dual-layer client appears.

## 7. Pipeline performance beyond this PR

- **Persistent per-playback helper:** each window is a fresh helper and
  encoder process, with a keyframe seek and preroll decode (34 of 82
  decoded frames on a 2 s P8 window). Keeping one helper per playback
  removes the preroll and the process/Vulkan start-up.
- **Zero-copy encode:** frames still leave the GPU as 25 MB of YCbCr per 4K
  frame and are uploaded again for VA-API. Exporting the Vulkan image as a
  dmabuf straight into the VA-API encoder (or Vulkan Video encode) removes
  both copies.
- **NVIDIA:** GPU decode in the helper is VA-API only. NVDEC/CUDA → Vulkan
  interop would give the same zero-copy decode on NVIDIA nodes.
- **FEL upload:** the FEL path still uploads both layers as float planes
  when decoding in software; GPU decode maps them in place.

## 8. Quality evidence (acceptance, not a feature)

The P01/P02 milestone M5 still lacks a matched-bitrate comparison of HDR10-E
against the HDR10 base, and of FEL→P8.1 against the base conversion, on
held-out titles. Without it, "better" is a claim about mechanics, not about
the picture.
