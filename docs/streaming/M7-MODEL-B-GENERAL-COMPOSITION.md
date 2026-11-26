# M7 Model B v2: exact composition of general P7 FEL mappings

M7 compares a title's served F+ copy against its original FEL source. The
source arm is a software model of the FEL signal at each lattice site. Model
B v1 defined that signal only for identity mapping (two pivots, `0 + 1·x` on
every component) with LINEAR_DZ offset 512, slope 2048/2^23 and no
threshold. Any other P7 FEL title was refused with "M7 FEL requires identity
mapping and calibrated linear NLQ", so M7 recorded no quality claim for it.

The lab's second P7 FEL title has the shape most real FEL masters have:
nine luma pivots with first-order polynomial pieces (identity-valued in most
frames, not in all), one third-order MMR piece per chroma component,
LINEAR_DZ offset 512 with either 2048 or (in the opening frames) 2304 as
slope, and the calibrated Q13/Q28 colour of the v1 source. v2 composes that
shape, and any other shape listed under [Admission](#admission), exactly.

## Reference

FFmpeg parses Dolby Vision RPUs but has no composer. The pinned
reconstruction is libplacebo v7.387.0 (`pl_shader_dovi_reshape` and
`sh_dovi_compose_nlq` in `src/shaders/colorspace.c`, with
`pl_map_dovi_metadata` in `src/include/libplacebo/utils/libav_internal.h`).
It runs in binary32 on a GPU, so it cannot be reproduced bit for bit, and
binary64 does not represent a third-order MMR's 2^-113 terms either.

**Model B v2 is libplacebo v7.387.0's composer formula evaluated in exact
rational arithmetic.** For each component c, with BL codes b and EL codes e:

1. k_c = clamp(b_c, first pivot_c, last pivot_c); x_c = k_c / 2^10
   (`pivots[i] = code / 2^bl_bit_depth`; the sample reads code / 2^10).
2. The piece is the last i with k_c ≥ pivot_c[i] (`s >= pivots[i]`), using
   the component's own clamped code.
3. Polynomial: a0 + a1·x_c + a2·x_c², a_j = Q23 integer / 2^23, with unused
   orders zero. MMR: constant + Σ_{p ≤ order} Σ_t m[p][t]·T_t^p over
   T = (x0, x1, x2, x0x1, x0x2, x1x2, x0x1x2), which reads all three clamped
   components (`reshape_mmr`).
4. mapped_c = clamp(value, 0, 1) (`color[c] = clamp(s, 0.0, 1.0)`).
5. d = e_c − offset; residual = 0 for d = 0, otherwise
   sign(d)·((|d| − ½)·S + T) / 2^23. libplacebo clamps it to ±vdr_in_max;
   admission only accepts limits no EL code reaches, so that clamp never
   binds.
6. composed_c = clip(⌊(mapped_c + residual)·4096 + ½⌋, 0, 4095).

Model B then applies the unchanged frozen arithmetic: signed Q13 integer
YCC to RGB16 (`refit_topleft_rgb`, `ColorTransform::integer_rgb`).

## Exact integer arithmetic

Every value is held as a numerator over 2^113 = 2^23·1024^9:

| Quantity | Numerator over 2^113 |
| --- | --- |
| polynomial piece | a0·2^90 + a1·k·2^80 + a2·k²·2^70 |
| MMR constant | c·2^90 |
| MMR term t, order p | m·T̂_t^p·2^(90 − 10·deg_t·p), T̂ = integer code products, deg = (1,1,1,2,2,2,3) |
| residual | R·2^89, R = sign(d)·((2\|d\| − 1)·S + 2T) over 2^24 |
| composition | ⌊(N + R·2^89 + 2^100) / 2^101⌋, N clamped to [0, 2^113] |

Admission bounds every Q23 coefficient by |a| < 2^31 (|value| < 256). Then
each of at most 22 terms is below 2^31·2^90 = 2^121, the sum is below 2^126,
and signed 128-bit integers hold every step exactly. Nothing is rounded
before the final floor, so the result does not depend on compiler,
architecture or evaluation order.

The helper's [`refit_exact.h`](../../tools/dv_processing/refit_exact.h)
(`__int128`) and plurx-core's
[`dv_m7_scoring::composer`](../../crates/plurx-core/src/transcode/dv_m7_scoring/composer.rs)
(`i128`) implement this table independently, from FFmpeg's and libdovi's
parses respectively.

## Admission

Composed exactly, and therefore admitted:

- a fresh P7 FEL RPU (not reused, residual enabled), Q23 coefficients
  (`coefficient_data_type` 0, `log2_denom` 23), BL/EL/VDR depths 10/10/12,
  video-range BL, mapping colour space 0, 4:2:0 mapping, one partition;
- per component 2–9 strictly increasing pivots within 0–1023, every piece
  of one method (libdovi's representation and the helper's parser
  cross-check carry one method per component);
- polynomial pieces of order 1–2 without linear interpolation, MMR pieces of
  order 1–3, every coefficient with |Q23| < 2^31;
- LINEAR_DZ with one NLQ piece, offset ≤ 1023, slope and threshold below 1,
  and a vdr_in_max that no EL code reaches, with the 2^-16 reserve of
  `nlq_clip_is_nonbinding`;
- the calibrated colour (unchanged from v1: `refit_topleft_color`,
  `DmCoefficients::CALIBRATED`).

Still refused: MMR or polynomial pieces mixed within a component, linear
interpolation, partitions, MEL/no-residual sources, nonlinear-quantizer
methods other than LINEAR_DZ, binding vdr_in_max, coefficients of magnitude
256 or more, other bit depths, and uncalibrated colour.

## The four models

All four models score the same source arm:

- **B** uses the exact composition (above), then the frozen Q13 RGB.
- **C** and **D** read only the 12-bit composition, so they take the same
  exact codes unchanged.
- **A** (continuous BL and residual) read the BL as code/1023 and added the
  unrounded residual. v2 keeps that reading with the mapped value in place
  of the BL code: (1024·mapped_c, rounded once to binary64)/1023 + residual,
  minus the frozen 10-bit offsets.

The F+ arm and the authored-candidate arm are unchanged.

## Identity is byte-identical to v1

With identity curves (mapped = k/1024) and the v1 NLQ, the exact
composition reduces to v1's integer form, 4b + d + [d < 0] clipped. Model
A's input reduces bit for bit too: 1024·mapped is k exactly, and
R/2^24 = sign(d)(|d| − ½)/4096 is exact in binary64. Tests check this for
every BL × EL code pair in Rust (codes and Model A's binary64 bits) and in C
(against both v1's frozen formula and the binary64 renderer model). The
full-plane and TopLeft exports therefore write the same bytes for identity
sources.

## Versions and recalibration

- `MODEL_VERSION` is now `plurx-m7-four-model-v2`. Receipt recovery binds
  `model_version == MODEL_VERSION`, so no v1 receipt or completion binds a
  v2 evaluation. The automatic protocol embeds the model version, so its
  frozen-protocol SHA-256, and with it the experiment identity, changes too.
- The TopLeft export format, which is also the NP1 candidate recipe's
  `input_format`, is now `plurx-composed-model-b-topleft-v2`.
- Recalibration needs no new constant. The automatic protocol already
  calibrates each title's margin from its own two calibration exposures,
  scored under the model in force: max(0.05, largest four-model mean spread
  rounded up to 0.01). A v2 title is calibrated under v2 when its protocol
  is frozen. The historical Michael trial stays frozen under v1, as its
  receipts record.
- The helper and source changes also change M7's automatic identity, so the
  idle sweep reopens titles recorded as Unsupported under the old one.

## Evidence

- `tools/dv_processing/test_refit_exact.c`: identity with the v1 NLQ, for
  all 2^20 BL/EL pairs, equals v1's frozen formula and the binary64 renderer
  model. Identity with 75 other LINEAR_DZ parameter sets (every third BL
  code against every EL code) equals the renderer model, which is exact
  there. Nine nonidentity mappings (a two-piece-MMR P7
  fixture and seven frames of the lab title) agree with the binary64 model
  except within 10^-9 code of a rounding boundary, by at most one code, in
  fewer than 1 in 10^5 channel samples. Also a refusal grid and a
  worst-case-coefficient overflow check.
- `crates/plurx-core/tests/fixtures/dv-m7/exact-composer-vectors.json`
  (6,448 samples): written by the C test, replayed bit for bit (codes and
  2^-113 numerators) by `composer::tests`, and recomputed by
  `fraction_oracle.py` with Python `Fraction`s, directly from the formula
  above.
- Real frames, offline (not a test fixture): the rebuilt helper admits two
  48-frame windows of the lab title in `--m7-sample-fel-v1` and
  `--m7-export-full-planes-v1`. These are film-origin frames 0–47, and
  keyframe-origin frames 120–167, whose per-frame NLQ slope varies over
  2048, 2112, 2160, 2304 and 2560. In both windows, the Rust composer
  followed by the frozen Model B RGB and TopLeft packing reproduces the
  helper's composed luma at every one of the 24,883,200 lattice sites, and
  its Cb/Cr at all 6,220,800 even sites. With FFmpeg's own parse of each
  frame's RPU, the `Fraction` oracle agrees with the Rust composer on a
  1-in-173 subset (143,856 sites per window).

## Not changed

The F+ fitter's statistics (`refit_compose`, binary64) still model the
renderer. They agree with v2 except at rare rounding ties. Changing them
would change fitted tables and is a separate decision.
