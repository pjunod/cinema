# Display-targeted DV trims — the unaccepted RGB candidate and remaining color contract

**Status:** private candidate infrastructure, unmerged and unqualified ·
**Executes:** phase 2 of [the build plan](DISPLAY-TARGETED-DV-BUILD.md) ·
**Written:** 2026-10-22 · **Public SOP/L2 chroma continuation:** 2026-10-22

Companion to [the build plan](DISPLAY-TARGETED-DV-BUILD.md): this records what
can be implemented from public sources, what remains unresolved, and the exact
observations needed before rendering creative trims in a viewer session. The
official software-reference path in §7 supports numerical color QC independently
of the physical-device path; neither path is mislabeled as the other.
Production remains L1-only, with `creative_trims_applied: false`. No setting,
feature gate or production helper includes this candidate. Do not wire it into
serving while the ICtCp contract and independent acceptance remain unresolved.

## 1. The public sources establish different boundaries

| Source | Establishes | Does not establish |
|---|---|---|
| [SMPTE ST 2094-10:2021](https://pub.smpte.org/latest/st2094-10/st2094-10-2021.pdf), §6.2 and B.3 equation (8) | Gain/offset/gamma applied to target-normalized linear RGB, then rescaled to cd/m²; neutral gain/gamma 1 and offset 0. | Dolby RPU integer conversion or Dolby device equivalence. Annex B is informative. |
| [Dolby patent US10701399B2](https://patents.google.com/patent/US10701399B2/en), A4.2 | L2 field encoding centered at raw 2048 with a 4096 scale; multiscale has a different encoding. | That a centered slope/power delta is added to B.3 gain/gamma, or an ICtCp compensation function. |
| [Dolby patent US20210076042A1](https://patents.google.com/patent/US20210076042A1/en), equations (2)–(4) and following SOP rescaling | An explicit public embodiment rescales L2 and L8 SOP differently before the trim-pass function. | L8 raw-unit normalization, CM4 equivalence in B.3 linear RGB, or missing chroma/detail/L3 behavior. |
| [ETSI TS 103 572 v1.3.1](https://www.etsi.org/deliver/etsi_ts/103500_103599/103572/01.03.01_60/ts_103572v010301p.pdf), equations (4)–(8) | L2 raw SOP and chroma/saturation encoding, mapped to ST 2094-10 §6.2/6.3. | L8 conversion or a complete Dolby CM renderer. |
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
float32 SOP/CW/SG/luminance-weight parameters, L2 stage applicability, selection
schema version, metadata-interpretation/direct parameter mode and named candidate,
SOP/L2 bridges, B.3 stages, numerical-boundary and L1 method versions.
Method strings are NUL-delimited before the scalar stream is hashed. Frame
number is excluded so repeated shots can reuse identity. A selected instruction
change changes its identity even when that field remains unapplied.

The candidate uses a direct shader instead of a tone-map LUT: the production
Annex B.2 path already exports native RGB through a custom dispatch. Every
SOP dispatch supplies gain, offset, gamma and target as actual uniforms; the
separate L2 pass supplies actual CW/SG and target luminance coefficients.
There is no LUT callback or external trim state whose change can be missed by
a LUT cache. The native A→B→A regression is required evidence that reuse obeys
those parameters; the API description alone is not a run.

## 3. Public rescaling and assumed L8 raw units stay distinct from the RGB domain

For mapped linear component `E` in cd/m² and bucket peak `T`, B.3 equation (8)
is retained as the separately named `st2094-10-2021-b3-eq8-linear-rgb-v1`:

```text
F = T × clamp((E / T) × gain + offset, 0, 1)^gamma
```

`public-rescaling-l8-raw4096-assumed-v2` fixes the interpretation explicitly.
The patent publishes the level-specific SOP rescaling below. ETSI TS 103 572
v1.3.1 equations (4)–(6) establish the L2 raw normalization. For L8, this
candidate **assumes** the patent's metadata deltas use the same centered raw
units: `(raw − 2048)/4096`. Neither the L2 standard nor the cited patent
explicitly establishes that L8 raw normalization. It remains a fixed engineering
interpretation, not a reference-derived fit or an accepted Dolby conversion.

| Actual selected level | gain | offset | gamma | Raw normalization |
|---|---|---|---|---|
| L2 | `1 + s` | `o` | `1 + p` | Public ETSI L2 inverse, `(raw − 2048)/4096` |
| L8 | `1 + 2s` | `2o` | `1 + 2p` | Assumed centered `/4096`, explicitly versioned |
| No eligible trim | `1` | `0` | `1` | No raw conversion |

Selection carries its actual level; unsupported levels or nonneutral values
without a selected level refuse. The
[public libdovi XML importer at the pinned revision](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/dolby_vision/src/xml/parser.rs#L650)
establishes integer serialization, not corroboration of this L8 rendering
interpretation. Composing its expressions with this candidate, before integer
quantization/clipping, actually yields:

```text
L8 gain   = (G + 2) × (1 − L/2) − 1
L8 offset = (G + 2) × L/2
L8 gamma  = 2 / (1 + C/2) − 1
```

Here `G`, `L`, `C` are the XML gain, lift and gamma controls. For `G=0.5`,
`L=C=0`, the importer writes slope 3072 and this candidate produces gain 1.5.
The previously claimed 1.25 used the old `/4096` final gain increment and was
incorrect for the new L8 interpretation. It is not evidence to revert or fit
the bridge. The focused authored control preserves this distinguishing example.

The method `l8-centered-raw4096-engineering-assumption-v1` and
`raw_normalization_assumed: true` identify every selected L8 metadata-mode
receipt. L2 instead names `etsi-ts103572-1.3.1-l2-sop-raw-v1` with the assumption
flag false; direct controls and no eligible trim identify their own provenance.
Both normalization methods participate in the identity, alongside selected level
and the new overall candidate version. Previous candidate hashes are distinct.

The L2 normalization and patent rescaling are public; the L8 raw-normalization
composition remains assumed. Neither establishes the full rendering domain or
Dolby CM equivalence. The candidate still applies those parameters to B.3
linear RGB after the L1 transform; it does not substitute that RGB operation
for a patent ICtCp process. `direct-b3=gain,offset,gamma` remains a separate
explicit-parameter formula control. Neutral SOP takes an exact texture copy.

**Numerical boundary policy:** raw values are integers 0–4095. L2 gain/gamma
span `[0.5, 1.5 − 1/4096]`, offset `[-0.5, 0.5 − 1/4096]`. Under the stated assumption, L8 gain/gamma span
`[0, 2 − 1/2048]`, offset `[-1, 1 − 1/2048]`. The shader and direct controls
accept finite gain/gamma `[0,2)`, offset `[-1,1)` and target `(0,10000]`; they
reject other values without narrowing or silently clamping parameters. B.3's
affine input is clipped to `[0,1]`. The named local policy
`b3-unit-clamp-zero-power-one-v1` defines zero exponent to produce one even at
clipped base zero. Explicit CPU/shader branches avoid GLSL `pow(0,0)`; positive
power at base zero remains zero. This endpoint convention is an implementation
choice, not a claimed Dolby rule. One-LSB fractions are exactly representable
as float32; no epsilon or reference-derived fitting is introduced.

The instrument records `scope: rgb-sop-and-l2-chroma-saturation`,
`acceptance: unaccepted`, selected metadata level, parameter source, raw-unit
method/assumption flag, B.3 method, numerical policy, actual parameters,
complete selected trim words and `creative_trims_applied: false` per frame.
Selected L2 chroma/saturation now use the separate pass in §4; L8 chroma,
L8 saturation, multiscale/detail, L3 and extended controls remain unapplied.
The bounded candidate correction has authored controls only: independent
review precedes compilation, native execution and a fresh software-reference
comparison. Earlier reference errors are not repaired evidence; this source
change establishes neither CM conformance nor full phase E acceptance.

## 4. L2 chroma uses a coherent RGB pass; L8 remains unresolved

ETSI's L2 equations map the actual raw fields to `c = (raw_chroma − 2048)/4096`
and `SG = (raw_saturation − 2048)/4096`. The new
`etsi-ts103572-1.3.1-l2-cw-sg-v1` bridge applies only to selected L2 in public
metadata mode. L8 and explicit `direct-b3` controls emit zero CW/SG with an
unapplied source. No multiplier is inferred from L8's SOP scaling.

[`trim_l2_chroma_shader.h`](../../tools/dv_processing/controls/trim_l2_chroma_shader.h)
is a separate dispatch after SOP. It follows B.3 equation (9) entirely in
linear RGB, named `st2094-10-2021-b3-eq9-post-sop-bt2020-v1`:

```text
Y = 0.2627 F_R + 0.6780 F_G + 0.0593 F_B
G_i = F_i × (1+c) × (F_i/Y)^SG
    = (1+c) × Y × (F_i/Y)^(1+SG), for Y > 0
G_i = 0, for Y = 0
```

`F` is the post-SOP result. The luminance coefficients are from
[BT.2020-2 tables 3/4](https://www.itu.int/rec/R-REC-BT.2020-2-201510-I/en)
for this instrument's explicit BT.2020/D65 output; the cinema-primary example
coefficients in SMPTE are not substituted. Both actual float32 coefficients and
CW/SG uniforms enter the identity. Neutral CW/SG returns the exact SOP texture.

**Numerical policy:** CW/SG span `[-0.5, 0.5 − 1/4096]`. Positive weights bound
each ratio and `1+SG` is at least 0.5. The stable form therefore extends
continuously to black and zero channels without an epsilon or `0 × infinity`.
The named `continuous-black-no-bucket-clip-pq-ceiling-10000-v1` policy leaves
formula output unbounded by the bucket; final PQ serialization clips only to
its representable `[0,10000]` cd/m² range. This is an explicit transport ceiling,
not a fitted chroma rule. Invalid parameters refuse without narrowing clamps.

The receipt separately reports L2 chroma/saturation applicability, methods,
CW/SG, luminance coefficients and boundary policy. L8 remains explicitly
unapplied even when its SOP is nonneutral. L3, detail and extended controls
remain unapplied at every level; `creative_trims_applied` stays false.

The plan's proposed ICtCp pass remains unresolved. The RGB implementation does
not combine ICtCp intensity with RGB ratios. Public L2 coverage does not supply
L8 chroma conversion, an ICtCp compensation algorithm, target-gamut treatment or
Dolby CM equivalence. Those require source-backed work and matched numerical
reference acceptance; no reference-output fitting supplies them.

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
Official software-rendered references can supply matched digital color values
through §7; physical photometry is a separate acceptance scope.

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
same-raw L2/L8 differentiation, the XML gain-0.5/raw-3072 interpretation witness,
neutral/endpoints/one-LSB fractions, full-range
parameter refusal, explicit zero-power CPU/native behavior, exact neutral pixels,
actual same-dispatch-cache L2→L8→L2 output sensitivity, independent direct
L2 chroma equation witnesses, grayscale/color/black/extreme controls, distinct
CW versus SG effects and same-cache chroma A→B→A output sensitivity.
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
matched color observations and an explicit owner/reviewer disposition. The
software-reference path below provides digital QC without requiring the viewer
to supply instruments or technical packets.


## 7. Official software references support numerical color QC

With the user's EULA acceptance, the coordinator acquired source-bound output
through the official Dolby Professional Tools 5.6.4 documented analysis,
validation, target-info and CPU render interfaces. The source is an engineered,
self-owned 256×128 full-range RGB16 BT.2020/PQ sequence: three identical frames
at 24 fps. Fourteen references contain zero L8, positive/negative single-field
L8 edits and a combined case. These are actual reference-rendered images,
not a candidate run, physical photometry or completed trim acceptance. Vendor
executables and documentation stay outside the repository. The coordinator
owns packet preparation; the viewer need not identify equipment or supply
technical files.

The source is mastered at 4000 nits/0.005 nits with BT.2020/D65 primaries.
The custom target is ID 255, 1000 nits/0.005 nits, PQ RGB computer range,
16-bit BT.2020/D65. XML is 4.0.2, with actual CM version metadata preserved.
Official target-info reports Application Type **ALL**. A friendly target name
containing HOME does not establish HOME/CINEMA-specific behavior in this XML
version. The adapter freezes and records these facts, source frame 0, PTS 0
and duration 1/24 second, and the actual source/reference binary hashes.

**XML-to-RPU association:** the new `xml-reference` mode in the existing
[control generator](../../tools/dv_processing/controls/generator/src/xml_reference.rs)
uses pinned public libdovi `CmXmlParser::parse_file`, overrides the documented
profile to 8.1, calls `GenerateConfig::generate_rpu_list`, serializes each actual
HEVC NAL and parses it again. This is the same supported importer path used by
[dovi_tool's public generator](https://github.com/quietvoid/dovi_tool/blob/83e1fdad6dcd5995556235946e7c5c0f9010d5a1/src/dovi/generator.rs),
not a hand-written six-vector-to-RPU formula. Imported config, actual NAL,
round-tripped JSON, shot ID and source-frame/edit offset are bound in the
observation report. The generator refuses noncontiguous shots, gaps/overlaps,
empty coverage and out-of-range edits because the library emits shots
sequentially. These receipts establish the exercised public conversion path;
they do not establish proprietary renderer equivalence.

**Decoded source association:**
[`compare_trim_software_reference.py`](../../tools/dv_processing/controls/compare_trim_software_reference.py)
verifies bounded transferred acquisition packets against both transport hashes
and immutable acquisition `RESULT.json.files` hashes, with explicit RESULT and
OUTER-RESULT transfer coverage and true zero exits. Recomputed transport hashes
cannot authorize replacement source pixels or a replacement input manifest. A
pinned public FFmpeg TIFF decoder reads the exact vendor-input source image and
reference image as RGB16, preserving full-range channels without a color
transform. The source codes are divided by 65535 and imported into native
RGBA float32 PQ for the isolated candidate. Probe format/raster, decoder argv,
source image, decoded bytes and float input all have separate hashes. This is
a direct self-owned master-RGB mapping control, not an encoded-DV reconstruction
or decoder-association proof. Each candidate-selected L8/L10 is checked against
the actual converted RPU; XML vector labels alone cannot satisfy that check.

**Baseline and trim delta:** the zero-L8 official XML also contains nonneutral
derived L2 and L3 instructions. Selection chooses L8, so neither its companion L2 nor L3 is applied. The new
L2 chroma pass therefore remains unapplied for this L8 reference packet.
Metafier also regenerates companion L2 instructions when applying the global L8
edits. Their XML vectors and actual quantized RPU fields are recorded per case;
this is an **L8-plus-companion-L2 metadata delta**, not a causally isolated
L8-only experiment. The adapter makes no assumption about whether the official
CM4 renderer uses those L2 values. All remaining XML/RPU metadata, including
L1/L3, matrices and targets, must remain frozen (RPU CRC excluded). The adapter
reports these errors separately, for all pixels and registered source patches:

```text
zero baseline error = candidate_zero − reference_zero
absolute case error = candidate_case − reference_case
trim delta error    = (candidate_case − candidate_zero)
                    − (reference_case − reference_zero)
```

Each comparison uses normalized RGB/PQ and linear-light CIE XYZ in cd/m².
XYZ is derived from public [BT.2020-2 table 3 primaries and D65](https://www.itu.int/rec/R-REC-BT.2020-2-201510-I/en)
with the public ST2084 EOTF. There is no vendor-output fit or color calibration
curve. Reports include signed mean, absolute mean, RMS and maximum absolute
channel errors; the reference PQ code step is 1/65535. Delta separation does not
remove nonlinear interactions with baseline or regenerated companion instructions. No numerical
pass threshold or equivalence claim is manufactured.

The coordinator compiles the generator with its pinned Rust 1.97.1 loop and
public dependency source mounted at the control crate's existing `/dovi` path:

```bash
cargo build --locked --release \
  --manifest-path tools/dv_processing/controls/generator/Cargo.toml
```

The `xml` feature adds the existing pinned `roxmltree` 0.20.0 dependency; its
version/checksum are preserved from the repository's parser dependency lock.
After completed-code independent review and fixes, run the authored boundary
regressions and the single numerical-QC attempt:

```bash
PLURX_DV_CONTROL_SCRATCH="$OWNED_SCRATCH" \
PLURX_DV_REFERENCE_PACKET="$TRIM_PACKET" \
  python3 tools/dv_processing/controls/test_trim_software_reference.py

python3 tools/dv_processing/controls/compare_trim_software_reference.py \
  --baseline-packet "$SOURCE_PACKET" --reference-packet "$TRIM_PACKET" \
  --converter "$CONTROL_GENERATOR" --decoder "$FFMPEG" --probe "$FFPROBE" \
  --candidate "$TRIM_CANDIDATE" --source-archive "$EXACT_SOURCE_ARCHIVE" \
  --dependency-manifest "$DEPENDENCY_MANIFEST" --output "$NEW_ATTEMPT"
```

The regression checks separate baseline/delta arithmetic, independent D65/XYZ
witnesses, exact raster/range checks, converted-RPU selection, immutable fixed
metadata, altered source-frame refusal and hash/true-exit boundaries. No tests
or controls were run when authoring this continuation; compile/review/execution
belong to the coordinator's evidence-bound batch.

**How to read it:** `official-dolby-software-reference-numerical-qc` with
`observations-only-not-acceptance` is useful digital reference evidence. It is
not photometry, full Dolby conformance or automatic creative-trim acceptance.
Public sources justify the supported XML importer, frozen input/output color
math, selection/identity mechanics and illustrative RGB SOP equation. They
also justify the bounded L2 raw-to-B.3 RGB chroma/saturation pass. They do not
identify L8 chroma conversion or an ICtCp chroma-weight/saturation algorithm,
and fitted weights do not supply those gaps. Public SOP rescaling and the assumed L8 raw-unit composition do not establish
the B.3 RGB domain as Dolby rendering. Those are implementation/contract gaps even after reference
access has been resolved; E is not complete and production remains L1-only.


**Strict L8-only authored variant:** a separate packet may copy the original
zero-case XML and replace only its four matching Level8 nodes with a case's
original L8 nodes. Ordinary official XML validation/target-info/render calls
then produce a new reference; no skip/update-suppression flag is substituted
for validation. The original mixed packet remains immutable. This is an
explicitly authored XML experiment, not original unmodified Metafier output.

Its result declares `metadata_delta_scope: strict-l8-only-authored-xml`, binds
`parent_reference_result_sha256`, and includes a `metadata_authoring` record
per case with `name`, `baseline_xml_sha256`, `donor_xml_sha256`,
`output_xml_sha256` and `replaced_l8_nodes: 4`. Pass its original packet with
`--parent-reference-packet`. The adapter verifies parent/source hashes, donor
vectors, authoring output identity, complete case coverage and unchanged
non-L8 original zero-case XML. It then freezes all non-L8 actual RPU metadata,
including L2. Reports distinguish strict authored L8 input variation from the
mixed original packet's L8-plus-companion-L2 variation. A controlled input
variation supports numerical QC of that input; it does not identify a
proprietary algorithm or establish CM2.9 equivalence.

**Neutral-Level3 strict variant (authored, not yet qualified):** the acquired
14-case packet declares `strict-l8-only-authored-xml-neutral-l3` and binds the
strict packet's `RESULT.json` as its parent. Each case changes the matched
parent XML's four shot/frame `Level3/L1Offset` texts from
`0 -0.178274 0` to `0 0 0`. The adapter verifies every parent/output hash,
four direct Level3 nodes, the authoring receipt, and full XML-tree equality
after only those offsets are normalized. Source, target, L1, L2, L8, order
and vectors remain fixed across that parent transition. Within the new
packet, only L8 varies; the actual imported RPU must also hold non-L8
metadata fixed. Pass the strict packet as `--parent-reference-packet`.

The zero reference now has neutral Level3 offsets but retains nonneutral
Level2 instructions and Level1 image character. The candidate still applies
no Level3, and these official software comparisons remain numerical
observations, not Dolby algorithm acceptance. Independent review precedes
the one new authoring regression and the official comparison attempt; neither
is claimed as run here.
