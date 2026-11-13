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
| P3 D65 / PQ, 600 nits | 27 | 0.005 nits | 31,545 |
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

## CPU qualification — six corpus frames

The current table data was compared against the licensed reference on all six
CM v2.9 corpus8 frames. Counts cover each frame’s L5 active picture; the
reference pads outside it, while production preserves that content through
pointwise mapping. The gamut correction can change those outside pixels too.

| Frame | Old pixels >0.05 dE | New | Largest worsening dE |
|---|---:|---:|---:|
| godfather2-w28 | 153,917 | 0 | 0.00767 |
| godfather2-w43 | 0 | 0 | 0.00450 |
| smile-w33 | 965 | 2 | 0.00761 |
| dumbmoney-31386 | 21 | 0 | 0.00944 |
| soundofmusic-w7 | 5 | 0 | 0.00563 |
| soundofmusic-w32 | 2 | 0 | 0.00490 |

Every strictly worsening coordinate is retained, with old/new/reference RGB
and dE values. Losses are not hidden by an epsilon. The [qualification record](../../tools/dv_processing/cm29_gamut/qualification.json)
binds the counts, comparison artifacts and renderer data. Smile retains two
pixels over 0.05 dE. Both improve: (424,1989) 0.3334→0.2461 dE and
(424,1990) 0.5963→0.3954 dE. This is bounded improvement, not a claim of
exact frame identity. Broader title coverage and physical display acceptance remain open.

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

The lavapipe campaign accounts for 74 passing cases. Thirty-five passed in
its initial run, including all seven measured-target cases. The remaining 39
analytic-target cases exposed a duplicate source sampler binding when no table
matched. Restoring the original analytic shader and its single binding fixed
all 39 in a targeted retry. The measured lattice shader literal is identical
in both compiled binaries; the other 102 source inputs are unchanged. Passing
results are retained for those unchanged paths. This qualifies the recorded
libplacebo 360 / lavapipe test environment, not a physical display or every
production driver. Log and binary hashes are in the qualification record.

Adversarial review found no actionable issues. The PR records final CI and
merge state. The daemon and bundle both include the generated table header
in their source identities, so future data changes invalidate stale bundles
without changing the dependency-image recipe.
