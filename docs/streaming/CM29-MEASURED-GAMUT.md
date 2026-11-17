# CM v2.9 measured gamut — reference data and qualification

**Status:** built; target-black extension CPU and isolated GPU qualified;
served application acceptance open · **Owner:** Codex · **Updated:** 2026-11-16

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
| P3 D65 / PQ, 600 nits | 27 | 0–0.005 nits | 40,726 |
| P3 D65 / PQ, 1000 nits | 48 | 0–0.005 nits | 30,309 |
| P3 D65 / PQ, 2000 nits | 37 | 0–0.005 nits | 28,338 |

The lattice has intensity planes 0–64 and chroma indices −16–16. A missing
vertex retains the analytic calculation. Other peaks, black levels outside
the finite 0–0.005-nit range, gamuts and encodings also retain it. The 400- and 1600-nit buckets are unmeasured;
the reference requires suitable exact L2 metadata and refuses the attempted
peak overrides. This change makes no claim about those buckets or BT.709.

## Target black changes the curve, not the normalized gamut vertices

The original 600-nit probe found at most 0.67 native code between 28,399 common
direct vertices at zero and 0.005 nits. That narrow observation originally
left lookup eligibility at exactly 0.005. At 0.0001 nits the guard selected
the analytic lattice instead, reintroducing missing dark-chroma correction.
The full preserved Dumb Money frame has 23 active pixels above 0.05 dE in the
double CPU renderer's float export, with a maximum of 0.26448. Keeping its
correct 0.0001-nit curve and using the existing measured vertices reduces the
maximum to 0.01168, with none above 0.05. These are double CPU numerical
measurements; the isolated GPU helper observations below have their own binary
and coded-input identity.

The [black-range qualification manifest](../../tools/dv_processing/cm29_gamut/black-range-qualification.json)
binds 48 new licensed CPU reference runs, their inputs and outputs, measurement
sources and all coordinate-level scores. Private rasters and scripts remain in
the named retained artifact directory. No existing CSV vertex changes.

| Paired native observation against 0.005 nits | Samples | Maximum difference, 16-bit native codes |
|---|---:|---:|
| Integer normalized IPT vertices; neutral and lift −0.5; all three peaks and black 0, 0.0001, 0.001 | 426,189 | 1.0342 |
| Fixed seeded positions inside cells; lift −0.5; same peaks and black anchors | 834,943 | 1.1040 |

Each point is an 11×5 flat tile; the reference output is native P3/PQ float32,
so no inverse BT.2020 matrix contaminates dark native channels. The source is
constructed independently for each target black to hold the normalized IPT
coordinate fixed. Median channel differences are 0.0039–0.0156 code for direct
vertices and 0.0049–0.0156 for cell interiors. Each family has one 2000-nit,
zero-black comparison above one code; all 0.0001/0.001 comparisons stay below
one. These measurements include source float32 construction and licensed
renderer numerical variation; they do not assert identical internal table bits.

The implementation's P3/PQ lattice coordinates and native decoding depend on
peak and gamut; black enters the upstream tone curve. Together with the paired
observations, that separation supports reuse across the bounded measured black
interval. Selection therefore accepts finite black from zero through 0.005
nits for the same exact P3 peaks. Negative, nonfinite and higher blacks retain
the analytic path, as do unsupported peaks/gamuts and missing vertices. The
regression compares a fixed dark-red source tile against twelve licensed float
outputs, exercising both the changing curve and the native lattice.

## Isolated Intel and AMD helper output removes the real-frame outliers

A helper built from source commit `0f3cfd1dac8119aacc196d54f48f3bfb4669e1e7`
(SHA-256 `40095abfda7f982abe47c64973324dea215fa55e58099415bf07b6eb0d52d301`)
was mounted as an isolated executable override on Intel and AMD GPUs. Runtime
image, package-manifest and engine digests match each device's preserved baseline.
The enclosing Intel application revision advanced from `314b15590` to
`32fa105ee` between captures; AMD's enclosing revision matches. These observations
bind the helper executable and runtime dependencies, rather than asserting whole
application source equivalence. Each comparison uses the same-device baseline
helper output, identical coded packet/RPU identities and the same licensed float
reference. The manifest binds capture, transfer, coded-input,
RGB48 raster, score and complete worsening-coordinate receipts.

| GPU | Dumb Money, CM29 P3 600 / 0.0001 nits: baseline → candidate max dE | Pixels above 0.05: before → after | Worsened pixels, all below 0.05 |
|---|---:|---:|---:|
| Intel | 0.2689962 → 0.0494505 | 22 → 0 | 611,712 |
| AMD | 0.2689962 → 0.0487903 | 22 → 0 | 601,666 |

All 6,167,040 active pixels are scored. The CM4 Last Breath control at P3
2000 / 0.005 nits is byte-identical before and after on each device: maximum
dE remains 0.0367454 on Intel and 0.0436621 on AMD across 8,294,400 pixels.
This is source-pinned isolated helper qualification. It does not establish
served application deployment, physical display output, broader scene coverage,
or universal mapper fidelity on the stress grid. Integrated adversarial review and affected test state are recorded in
[PR #1727](http://forge.lan:3000/noirr/plurx/pulls/1727).

## The missing Smile low-black reference confirms the same cause

A new licensed 600 / 0.0001-nit reference changes only target ID27's minimum
brightness. The fresh unchanged 0.005 control is byte-identical to the preserved
reference. On all 7,372,800 active pixels of the preserved Smile w33 master,
the research double CPU control changes maximum dE from 0.6741254 to 0.0094134
and pixels above 0.05 from 16,014 to zero. All 2,994,323 worsening coordinates
are retained and hash-bound, with none above 0.05; 4,378,477 pixels improve.
The manifest binds both rasters, binary receipts, references, metadata and the
complete coordinate archive. This fills the licensed-reference and CPU-analysis
gap; it does not claim P7 GPU, served or physical qualification at this target.

## The whole stress grid remains an explicit mapper limitation

A separate unpartitioned 120,937-color grid mixes a 33³ BT.2020/PQ cube, seeded
random colors, log-dark colors and zero-channel colors. Its [recorded scores](../../tools/dv_processing/cm29_gamut/black-range-qualification.json)
retain every sample and every worsening; no favorable residual subset defines
the qualified black domain. The grid deliberately reaches outside the declared
P3 / 4000-nit source mastering volume. At the existing 0.005-nit setting the
measured mapper already has these absolute residuals:

| Peak | Maximum dE | Samples above 0.05 | Measured worse than analytic and above 0.05 |
|---|---:|---:|---:|
| 600 | 122.8288 | 14,742 | 277 |
| 1000 | 104.5208 | 17,600 | 612 |
| 2000 | 98.9899 | 15,122 | 556 |

At 0.0001 nits, measured reuse has maxima 119.3581 / 99.6069 / 97.6558 and
14,267 / 17,074 / 14,763 samples above 0.05 at those peaks; 266 / 592 / 473
samples are worse than analytic and above 0.05. These stress residuals are
reported, not accepted as universal mapper fidelity. The black-range conclusion
comes from paired native coordinates and the complete real frame, while broader
mapper perfection on arbitrary stress inputs remains unproven. All twelve scalar
grid evaluations are bound to the candidate headers and are bit-identical to
the diagnostic table-reuse evaluations; the full-frame score retains all 696,117
small worsenings, with none above 0.05.

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
