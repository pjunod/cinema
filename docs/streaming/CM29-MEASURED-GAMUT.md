# CM v2.9 measured gamut — reference data and qualification

**Status:** built; locally qualified, merge checks recorded in PR · **Owner:**
Codex · **Updated:** 2026-11-13

Companion to [the display-targeted build ledger](DISPLAY-TARGETED-DV-BUILD.md).
This records the measured gamut correction, its limits and its evidence.

## The analytic cube misses the reference’s gamut mapping

The reference uses target-specific gamut tables. An inverse IPT transform and
channel clipping reproduce much of the picture, but miss near-black chroma
and the desaturation near the top intensity plane. Changing the tone curve
to compensate would change otherwise correct pixels. The correction belongs
in the gamut lattice.

The implementation shares generated 16-bit vertices between the double CPU
reference and the GPU. The GPU uploads the native values once when the exact
target changes and copies supported vertices into its existing frame lattice.
There are no large GLSL constant arrays and no new feature switch.

## Only measured targets and supported vertices use the data

| Target | Reference ID | Black | Supported vertices |
|---|---:|---:|---:|
| P3 D65 / PQ, 600 nits | 27 | 0.005 nits | 40,726 |
| P3 D65 / PQ, 1000 nits | 48 | 0.005 nits | 30,309 |
| P3 D65 / PQ, 2000 nits | 37 | 0.005 nits | 28,338 |

The lattice has intensity planes 0–64 and chroma indices −16–16. A missing
vertex retains the analytic calculation. Other peaks, black levels, gamuts
and encodings also retain it. The 400- and 1600-nit buckets are unmeasured;
the reference requires suitable exact L2 metadata and refuses the attempted
peak overrides. This change makes no claim about those buckets or BT.709.

The 600-nit black-level probe compares 28,399 common direct vertices at zero
and 0.005 nits. The maximum difference is 0.67 code, with a median channel
difference of 0.012 code. That does not establish black-level independence;
the lookup remains limited to the measured 0.005-nit identity.

## Acquire native output and reject unsupported extrapolation

All measurements are black-box runs of licensed `cm_offline` 5.6.4, CPU only.
No table was extracted from its binary. The synthetic metadata uses exact L2 trims and a 4000-nit mastering display.
Two neutral seeded datasets cover the full intensity range. A second pair
with lift −0.5 reaches additional dark chroma corners that neutral metadata
cannot reach. It fills only previously unobserved vertices; neutral data wins
overlap. The two families can differ by up to 8, 3 and 3 quantized codes in
shared vertices, so these are effective measured vertices, not a claim to
have recovered the reference’s internal table bits. Each sample is an 11×5 flat
tile covering the detail filter’s support.

An earlier prototype converted float32 BT.2020 output back into native P3.
Near-zero target channels magnified inverse-matrix rounding into large PQ
code errors. Small-cell fit residuals fell from median 5.38 codes to 0.011
when excluding those channels. A 53-sample large-tile probe reproduced the
original raw BT.2020 reference values within 0.00004 code, excluding tile
support as the cause. Acquisition now requests native P3/PQ output directly.

Directly reached vertices are observed. Each vertex must be supported by two
independent random probe sets. In each local SVD solve its center must be
identifiable, its pseudoinverse row norm below 8, and its maximum fit residual
below 0.25 native code. The two unrounded estimates must also agree within
0.25 code in every channel. Their mean is rounded to 16-bit codes; unknown
values carry a sentinel. A fitted value merely being near an integer never
establishes support. The critical nine lowest-plane corners agree within
0.053, 0.030 and 0.038 code at 600, 1000 and 2000 nits respectively. The data therefore
represents quantized measured observations, with an analytic fallback, rather
than a claim that every virtual corner of the reference was recovered.

The [provenance manifest](../../tools/dv_processing/cm29_gamut/provenance.json)
binds input metadata, generated pixels, coordinate records, reference output
and exported vertex lists by SHA-256. The [generator](../../tools/dv_processing/cm29_gamut/generate_tables.py)
validates those lists and emits [the shared header](../../tools/dv_processing/dm_cm29_gamut_tables.h).

```bash
python3 tools/dv_processing/cm29_gamut/generate_tables.py --check
```

A successful check means the generated header matches the recorded vertex
lists. It does not establish licensed-reference execution or picture fidelity.

## The remaining Smile errors were a coverage gap

PR #1575 left two Smile pixels above 0.05 dE. An exact-context 64×64 crop
reproduces the saved full-frame CPU RGB bit-for-bit. Both pixels interpolate
around I=16–17, P=−5..−4, T=−10..−9. Vertex `(16,-5,-10)` was unobserved by
the neutral and darkening probe families. The fallback maps its green to
0.0479 native code; the licensed reference gives about 4,784 codes. Replacing
only that vertex in an isolated diagnostic renderer drops the errors from
0.2461/0.3954 to 0.00055/0.00232 dE, establishing the cause.

The production correction extends generic calibration coverage. Authored
saturation gain 0.5 expands the set of lattice coordinates reachable from
physical BT.2020 RGB. Two full-grid synthetic datasets, seeds 71 and 197,
add 9,181 supported 600-nit vertices with the existing conditioning,
residual and agreement limits. Every one of the previous 31,545 vertices is
unchanged; neutral and dark observations retain priority over all overlaps.
The 1000- and 2000-nit tables are unchanged. No film pixels train the data,
and runtime interpolation and renderer arithmetic are unchanged.

The saturation family differs from existing observations by up to seven
quantized codes at overlaps, which remain owned by the existing data. As
with the earlier dark supplement, these are effective observations rather
than recovered internal LUT bits. Full-frame qualification bounds this change.

The probe and coordinate tools accept `[LIFT [SATURATION_GAIN]]`; omitted
values retain the original neutral acquisition. For this supplement both
use `0 0.5`. Author the matching MDF L2 Trim as
`0,0,0,0,0,0,0,0.5,0` at both shot and frame levels in `neutral.xml`, retain
its L1/mastering metadata, and capture native P3/PQ output at target ID 27.
The provenance manifest binds the exact two generated rasters, coordinates,
metadata and licensed outputs. Regenerate with the original neutral pair
first, the dark pair as first `--supplement`, and the saturation pair as the
second `--supplement` to `fit_vertices.py`; then run `generate_tables.py`.

## CPU qualification — six corpus frames

The expanded table was compared against PR #1575 and the same cached licensed
reference on all six CM v2.9 corpus8 frames. Counts cover each L5 active
picture; the reference pads outside it. No outside-L5 pixel changed in this
extension. Every strictly worsening active coordinate is retained with
old/new/reference RGB and dE, with no epsilon. Outside-picture comparisons
are also retained separately for complete coordinate accounting.

| Frame | PR #1575 pixels >0.05 dE | Current | Worsening coordinates | Largest worsening dE |
|---|---:|---:|---:|---:|
| godfather2-w28 | 0 | 0 | 0 | 0 |
| godfather2-w43 | 0 | 0 | 0 | 0 |
| smile-w33 | 2 | 0 | 634 | 0.00349 |
| dumbmoney-31386 | 0 | 0 | 11 | 0.00179 |
| soundofmusic-w7 | 0 | 0 | 0 | 0 |
| soundofmusic-w32 | 0 | 0 | 0 | 0 |

The two Smile points improve to 0.003387 dE at (424,1989) and 0.000952 at
(424,1990). Across all six frames, no active pixel exceeds 0.05 dE; the
largest remaining error is 0.013680 dE. The
[qualification record](../../tools/dv_processing/cm29_gamut/qualification.json)
binds the counts and every worsening artifact hash. This is bounded corpus
qualification; broader titles and physical displays remain separate.

Both affected 600-nit GPU cases pass again. An additional exact-context crop
has maximum GPU/CPU difference 0.001262 dE; both resolved pixels are below
0.003567 dE against the reference on GPU. The prior 72 unaffected parity cases
retain their evidence: all other 102 build inputs and the complete 1000/2000
arrays are byte-identical. The existing renderer uses the extended table
through its ordinary source identity and lattice upload path.

## Docker controls distinguish analytic and measured models

The measured-gamut integration also exposed stale build-time test assumptions.
The original analytic lattice literal is still byte-identical under
`dv_dm_body_lattice_analytic`; the measured lattice is a separate literal.
The active-area control pins both independently. L5 absent/zero and adversarial
bar-domain cases exercise both analytic and measured CPU paths.

The chroma-detail fixture retains its original analytic goldens and adds
separate measured-model goldens at the unchanged `1e-6` tolerance. These are
CPU-model characterizations, not licensed chroma-plane measurements. Removing
only the chroma detail pass fails all four combinations of analytic/measured
model and negative/positive chroma weight. All nine Docker C controls pass on
macOS with libplacebo 7.387 and in the cached Linux compiler/SDK 7.360 environment.
This is source-bound CPU-control execution, not a claim to have built the full
application image. Its receipt wrapper also binds the generated table header;
a regression proves a table-only change rejects a stale compiled receipt.

## Parity distinguishes matrix precision from transfer-helper exactness

The old CM4 crop test applies a 1.5e-6 PQ limit to every channel. At a nearly
zero blue channel, float32 matrix cancellation before PQ left 3.99e-8 nits,
which PQ expands to a 2.42e-5 signal difference. A diagnostic shader retained
the pre-PQ blue value in alpha while preserving every rendered RGB bit.
Across the two failing crops, the largest such error was 1.46 binary32
epsilons of that pixel’s peak light. The renderer arithmetic is unchanged.

The crop criterion keeps the 1.5e-6 PQ budget, with an alternative of four
binary32 epsilons of the pixel’s peak in linear light. Maximum dE must still
be below 0.05. This is an empirical arithmetic tolerance, not a formal bound;
it is far tighter than the existing synthetic-raster light allowance. The
historical test ID remains stable. The separate PQ-helper exactness and
mutation checks retain their original contract.

A separate spatial MS-weight probe uses 112 gray speck tiles and L2 weights
−0.75/−0.25 authored at 100/1000 nits, rendered at 600 nits. The current
interpolated prediction matches the reference’s change within 0.637 code
(RMS 0.134); holding the lower/upper endpoint misses by up to 1,765/521
codes. Disabling the weight misses by 1,664 codes. This verifies the bounded
spatial case rather than relying on flat tiles; no selection change is made.

The original PR #1575 lavapipe campaign accounts for 74 passing cases. Thirty-five passed in
its initial run, including all seven measured-target cases. The remaining 39
analytic-target cases exposed a duplicate source sampler binding when no table
matched. Restoring the original analytic shader and its single binding fixed
all 39 in a targeted retry. The measured lattice shader literal is identical
in both compiled binaries; the other 102 source inputs are unchanged. Passing
results are retained for those unchanged paths. This qualifies the recorded
libplacebo 360 / lavapipe test environment, not a physical display or every
production driver. Log and binary hashes are in the qualification record.

[PR #1575](http://forge.lan:3000/noirr/plurx/pulls/1575) records the original
adversarial review, CI and landing. The owning follow-up PR records review
and final CI for the saturation coverage extension. The daemon and bundle both include the generated table header
in their source identities, so future data changes invalidate stale bundles
without changing the dependency-image recipe.
