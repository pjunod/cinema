# Display-targeted DV trims — the unaccepted SOP candidate and missing color contract

**Status:** private candidate infrastructure, unmerged and unqualified ·
**Executes:** phase 2 of [the build plan](DISPLAY-TARGETED-DV-BUILD.md) ·
**Written:** 2026-10-22

Companion to [the build plan](DISPLAY-TARGETED-DV-BUILD.md): this records what
can be implemented from public sources, what remains inferred, and the exact
observations needed before rendering creative trims in a viewer session.
Production remains L1-only, with `creative_trims_applied: false`. No setting,
feature gate or production helper includes this candidate. Do not wire it into
serving while the ICtCp contract and independent acceptance remain unresolved.

## 1. The public sources establish different boundaries

| Source | Establishes | Does not establish |
|---|---|---|
| [SMPTE ST 2094-10:2021](https://pub.smpte.org/latest/st2094-10/st2094-10-2021.pdf), §6.2 and B.3 equation (8) | Gain/offset/gamma applied to target-normalized linear RGB, then rescaled to cd/m²; neutral gain/gamma 1 and offset 0. | Dolby RPU integer conversion or Dolby device equivalence. Annex B is informative. |
| [Dolby patent US10701399B2](https://patents.google.com/patent/US10701399B2/en), A4.2 | L2 field encoding centered at raw 2048 with a 4096 scale; multiscale has a different encoding. | That a centered slope/power delta is added to B.3 gain/gamma, or an ICtCp compensation function. |
| Pinned public libdovi [L2](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/dolby_vision/src/rpu/extension_metadata/blocks/level2.rs), [L8](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/dolby_vision/src/rpu/extension_metadata/blocks/level8.rs), [L10](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/dolby_vision/src/rpu/extension_metadata/blocks/level10.rs) | Actual field layouts, neutral defaults, L8 lengths and custom target indices; CM4 L8 is used to compute L2. | A complete preset-display table or the physical display-management algorithm. |
| Pinned libplacebo [custom shader API](https://github.com/haasn/libplacebo/blob/0d043c7f6f79cd3687c023454bdacbe615e4d96f/src/include/libplacebo/shaders/custom.h) | Each custom dispatch receives explicit variable data. | Executed native API 374 behavior; that requires the authored regression. |

## 2. Selection and identity are independent of the approximation

[`trim_selection.h`](../../tools/dv_processing/trim_selection.h) copies each
frame's chosen trim before parser objects are freed. It picks the greatest
signalled target PQ at or below the chosen bucket's rounded 12-bit PQ code.
The quantization matters: nominal 1000 nits encodes as 3079 and its decoded
luminance is slightly above 1000. Comparing unrounded nits would lose an exact
nominal boundary. Unsorted lists do not change the answer.

L8 resolves its index against the same frame's actual L10 list. Missing preset
indices refuse explicitly; no guessed preset luminance or stale previous L10
is used. Duplicate L2 peaks, duplicate L8/L10 indices and equal-peak distinct
L8 targets refuse. On an exact cross-version target tie, L8 wins because it
contains the original CM4 controls. No eligible trim produces neutral SOP.
Lists are bounded at 32 entries each. This is incomplete preset support, not a
claim that the real title has no applicable trim.

The SHA-256 identity serializes explicit little-endian scalar words, never C
structure padding or parser addresses. It covers the selected level/index,
all signalled trim controls (including currently unapplied multiscale,
chroma, saturation, extended fields), resolved target peak/floor/primaries,
L1, title mastering endpoints, bucket, raster, provenance mode, actual
float32 SOP parameters and both method versions. Frame number is excluded so
repeated shots can reuse identity. A selected instruction change changes its
identity even when that field remains unapplied.

The candidate uses a direct shader instead of a tone-map LUT: the production
Annex B.2 path already exports native RGB through a custom dispatch. Every
candidate dispatch supplies gain, offset, gamma and target as actual uniforms.
There is no LUT callback or external trim state whose change can be missed by
a LUT cache. The native A→B→A regression is required evidence that reuse obeys
those parameters; the API description alone is not a run.

## 3. The SOP formula is published; the integer bridge remains inferred

For mapped linear component `E` in cd/m² and bucket peak `T`, the directly
parameterized B.3 equation (8) is:

```text
F = T × clamp((E / T) × gain + offset, 0, 1)^gamma
```

The isolated `inferred-raw-sop-v1` mode proposes:

```text
gain   = 1 + (trim_slope  - 2048) / 4096
offset =     (trim_offset - 2048) / 4096
gamma  = 1 + (trim_power  - 2048) / 4096
```

This adds the patent's centered delta to the standard's neutral gain/gamma.
That bridge, especially its use for L8, is **inferred and unaccepted**. It is
not a source-backed claim about Dolby rendering. `direct-b3=gain,offset,gamma`
accepts explicit standard parameters for formula controls independently of the
bridge. Neutral SOP takes an exact texture-copy path, avoiding a PQ round trip.

The instrument applies the existing L1 transform, followed by SOP only. It
prints `scope: rgb-sop-only`, `acceptance: unaccepted`, the actual parameters,
complete selected metadata words and `creative_trims_applied: false` on every
frame. Chroma, saturation, multiscale and extended controls remain unapplied.
Its name and report cannot be used as a viewer's creative-trim claim.

## 4. ICtCp compensation has an unresolved algorithm contract

The plan asks for a separate ICtCp chroma/saturation pass. ST 2094-10 B.3
instead illustrates RGB chroma adjustment in equation (9). §6.3 describes
chroma weight as depending on luminance and saturation inputs; it does not
specify an ICtCp equivalent. The public raw-field descriptions do not choose:

- Whether compensation uses pre-map intensity, post-L1 intensity or post-SOP
  intensity, nor which saturation measure it uses.
- Whether the raw chroma-weight delta scales Ct/Cp directly or weights a
  mapping-dependent compensation, and the sign/scale of that relationship.
- The relative order of saturation and compensation, target normalization,
  near-black behavior, or output-gamut handling.

No ratio/power function, epsilon, arbitrary clamp or private/vendor LUT is
introduced to fill this gap. SOP-only execution is useful infrastructure; it
is not a completed E implementation or acceptance.

The smallest distinguishing matched-reference packet contains the SAME source
frame and target with: neutral controls; slope-only and power-only changes;
saturation-only positive and negative changes; chroma-weight-only positive and
negative changes under two different L1 compression strengths; then one combined
SOP/chroma/saturation case. Use an achromatic ramp plus low/high-saturation
patches at matched intensity, including near black and a bright shoulder.
Record measured Y and chromaticities (or independently calibrated XYZ) for the
registered patches. Y alone cannot distinguish Ct/Cp algorithms. The two
compression strengths distinguish fixed chroma scaling from mapping-dependent
compensation; single-field changes distinguish sign/scale and combined changes
reveal ordering. Capture/reference creation must be independently authorized
and source-bound; an engineered reference packet is still labeled as such.

## 5. Build and review precede every execution

The coordinator compiles against the pinned native API 374 dependency bundle:

```bash
sh tools/dv_processing/controls/build_trim_candidate.sh \
  "$FFMPEG_PREFIX" "$PLACEBO_PC" "$DOVI_PREFIX" "$FRESH_OUTPUT"
```

This compiles both instruments without running them. After completed-code
independent review and fixes, the focused engineering regression is:

```bash
"$FRESH_OUTPUT/test_trim_candidate"
```

It covers exact nominal target boundaries, list-order independence, no eligible
trim, duplicate/missing target refusal, L8/L10 resolution and cross-version
ties, all selected-field identity sensitivity, explicit parameter identity,
exact neutral pixels, and actual same-dispatch-cache A→B→A output sensitivity.
Synthetic structs and pixels prove mechanics; they do not prove optics.

## 6. The real-reference adapter emits observations, never acceptance

[`compare_trim_reference.py`](../../tools/dv_processing/controls/compare_trim_reference.py)
accepts an `e-trim-reference-v1` JSON packet. Every file binding is an object
with `path` and `sha256`. The packet binds `source_media`,
`reconstruction_receipt`, `capture_artifact`, `instrument_calibration`,
`source_archive` and `dependency_manifest`. Its `provenance` records `kind:
real-dv-device-photometry`, `synthetic: false`, operator, device, DV route,
capture UTC, instrument, measured display peak in nits, display mode and
spatial registration. Those are operator declarations, not authentication.

Top-level `width`, `height`, `target_nits`, `title_min_1e4` and `title_peak`
identify the candidate policy. Each of 1–16 `frames` has a unique `frame_id`,
exact rational `pts: [numerator, denominator]`, bound `rpu` and
`master_rgba32f` (native BT.2020/PQ, four little-endian float32 components),
and nonempty `patches`. A patch records `id`, native-raster `x`, `y`, `width`,
`height`, measured `reference_y_nits` and `instrument_uncertainty_nits`.
The reconstruction receipt must independently bind source frame/PTS to that
RPU and RGB; a copied JSON label cannot establish this association.

```bash
python3 tools/dv_processing/controls/compare_trim_reference.py \
  --packet "$MATCHED_PACKET" --candidate "$FRESH_OUTPUT/trim_candidate" \
  --output "$NEW_ATTEMPT"
```

The adapter runs the candidate once across the frame sequence, retains native
stdout/stderr, checks complete output, and emits per-patch signed Y differences
with source/binary/dependency/packet/adapter hashes. **How to read it:**
`observations-only-not-acceptance` means exactly that. The adapter does not
invent thresholds, authenticate a device, prove chroma fidelity or authorize
serving. Nonzero paired frames/patches and a true zero process exit are required
for observations; none substitute for independent capture review. Actual
creative-trim acceptance still needs the unresolved color contract, independently
matched color measurements and an explicit owner/reviewer disposition.
