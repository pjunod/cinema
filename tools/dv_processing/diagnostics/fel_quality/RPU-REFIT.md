# FEL quality without an encode: re-fitted 8.1 RPU curves

**Status:** offline result, not built · **Date:** 2026-11-12

Companion to [FEL-PICTURE-QUALITY-INVESTIGATION.md](../../../../docs/streaming/FEL-PICTURE-QUALITY-INVESTIGATION.md).
Idea and design by Fable 5.2; [protocol](protocol-rpu-refit-20261112.json)
fixed before outputs, amendments 3–4 after Fable's adversarial review;
[results](results/rpu-refit-20261112.json).

## The idea

Today's EL-dropped serve (F) converts the Profile 7 RPU to 8.1 with identity
mapping curves and plays the original BL bits. A single-layer 8.1 RPU can
still carry real curves: up to 8 luma pieces of 2nd-order polynomials and a
3rd-order chroma MMR over Y, Cb and Cr. Fitted per scene so that the untouched
BL lands as close as possible to the FEL composition, they recover the part
of the EL that is a function of each pixel's own BL value. That needs no
encode, keeps the original BL bits, and plays as ordinary 8.1 Dolby Vision.
Spatial detail and grain in the EL are out of its reach.

## The deployable fit (F+ = arm Fp_safe)

The first, unconstrained fit (Fp_poly) scored well but was not safe to ship.
Luma pieces with few samples went wild: Michael1800 departs from identity by
407 12-bit codes and Beekeeper by 1,315, with non-monotone pieces and steps at
pivots. Its chroma MMR leaves identity by up to 124,862 codes on a 17³ grid.
Fp_safe fixes that:
- luma fits a monotone (isotonic) target, continued by the identity slope
  outside the codes the scene uses;
- the 8 pieces are fitted jointly and joined continuously at the pivots;
- the chroma MMR is ridge-regressed toward identity.

| Fit diagnostic (worst over the 3 fixtures) | Fp_poly | Fp_safe |
|---|---:|---:|
| Luma departure from identity, 12-bit codes | 1,315 | 33 |
| Luma step at a pivot beyond the local slope | 1,325 | 0.002 |
| Luma monotone | no (2 of 3) | yes |
| Chroma departure from identity on a 17³ grid | 124,862 | 80 |
| Largest coefficient | 32.8 | 1.5 |

Fp_safe costs at most 0.014 of pooled ΔITP against Fp_poly. Every number
below is Fp_safe.

## Result

All-48 lattice, pooled whole-picture ΔITP against the composition. Model A
is float composition, the family libplacebo-style decoders implement; model
B is a 12-bit composition. C and D track B within 0.001.

| Fixture | F (A / B) | F+ (A / B) | F's error removed (A / B) | F+ cross-fitted (B) |
|---|---:|---:|---:|---:|
| Michael5400 | 3.239 / 3.200 | **1.441 / 1.462** | 56% / 54% | 1.466 |
| Michael1800 | 3.222 / 3.190 | **1.318 / 1.342** | 59% / 58% | 1.342 |
| Beekeeper | 1.269 / 1.465 | **0.940 / 0.964** | 26% / 34% | 0.964 |

- **Cross-fitted:** curves fitted on frames 24–47 score frames 0–23, and the
  reverse. They lose under 0.005, so the gain is not overfitting the window.
  Beekeeper is a third title, not a held-out test: its curves are fitted on
  its own frames, as the product would do.
- F+ beats F in all 48 frames, on every fixture and model, for the whole
  picture and for shadow p99. The worst per-frame p99 falls on every fixture
  (Michael5400 7.56 → 6.00, Michael1800 6.76 → 4.51, Beekeeper 5.08 → 4.73).
  The worst single pixel rises on Beekeeper (14.07 → 14.51).
- One fit per window is enough. A per-frame fit (Fp_frame) gains under 0.002.
  Fp_frame is a per-frame fit of code error, not a ΔITP optimum or a bound.
- Taking the chroma MMR's luma at the chroma sample site changes nothing
  measurable.

### Against the encodes

The best re-encode at ~130 Mb/s (Beekeeper up to 158) is taken separately for
each column, which favours the encodes. All figures are model B ΔITP, written
F+ / best encode:

| Fixture | Lattice pooled | Frame 0 mean | Frame 0 shadow p99 | Frame 24 mean | Frame 24 shadow p99 |
|---|---|---|---|---|---|
| Michael5400 | **1.462** / 1.599 | 2.053 / **1.517** | 4.051 / **2.899** | **1.389** / 1.547 | **3.206** / 3.462 |
| Michael1800 | **1.342** / 1.373 | 1.438 / **1.198** | 1.980 / **1.875** | **1.205** / 1.340 | 1.909 / **1.884** |
| Beekeeper | **0.964** / 1.455 | **0.664** / 1.121 | **1.587** / 2.046 | **1.273** / 1.531 | 3.338 / **2.825** |

- F+ wins pooled on all three fixtures. The Michael margins are small: 0.14
  and 0.03 in model B, and 0.20 and 0.07 in model A, whose best encode is a
  different arm (`base`).
- The best encodes win clearly on frame 0 of both Michael windows, and on
  several shadow tails. Frame 0 most likely carries EL detail that a
  per-pixel curve cannot reproduce. **F+ does not dominate the encodes.** It dominates F.
- On Beekeeper, no encode at ≤158 Mb/s beats F in all four models. The
  unencoded export, though, scores 0.763, below F+, so a high-rate re-encode
  could in principle still win there.

## Real RPUs

`rpu_refit_write` (dolby_vision 3.3.1) writes each frame's 8.1 RPU with the
Fp_safe curves in the normative domain. Read back from the written bytes and
applied with libplacebo's `pl_shader_dovi_reshape` semantics, they score
identically to Fp_safe (difference < 0.0001, every fixture and model). The
writer refuses integer parts outside [−64, 63). This round trip validates
the encoding only. That decoders use the same semantics is what the device
checks test.

## Do decoders apply the curves?

**macOS: yes, closely.** AVAssetReader and AVPlayerItemVideoOutput only
attach the RPU, so `dv_display_check.swift` renders an AVPlayerLayer
offscreen with CARenderer to run the platform's own Dolby Vision display
path.
- **Gross check:** the same frames with luma ×0.5 or a +0.15 Cr constant
  move mean linear R from 0.202 to 0.012 and 0.483.
- **Flat-patch check** (`dv_patch_check.py`, `dv_patch_clips.sh`): 48 flat
  patches in three clips.
  - A: codes c with the Michael5400 F+ RPU.
  - B: codes round(f(c)/4) with the identity RPU, where f is the curve under
    libplacebo semantics.
  - C: A's codes with the identity RPU.
- If the Mac applies f as libplacebo does, A matches B. If it ignores the
  RPU, A matches C.
- **Result:** the median per-patch difference is 0.07% for A vs B and 2.4%
  for A vs C, and 44 of 48 patches sit closer to B. Where the curve moves the
  picture (33 patches), the median patch reproduces 84% of the predicted
  change, and 31 reproduce more than half.
- The misses sit at outputs near the display's clip point, and B's codes
  carry up to half a code of rounding. A repeat render is bit-identical.
- iOS and tvOS share the AVFoundation pipeline but were not measured.

**Android, Lenovo TB322FC with Google Photos: the mapping is ignored.**
Photos plays the clips through `c2.dolby.decoder.hevc` (Dolby Vision Mobile
3.1, GPU processing, RGB output), and `adb screencap` sees the processed
picture.
- **The RPUs are read.** With L1 max/avg overridden to 4095/3600 (the
  writer's diagnostic `l1` plan field), the same frames drop from a mean of
  76.5 to 48.0 on 8-bit red. An L1 max of 2081 changes almost nothing (max 3
  levels). The decoder logs profile 8.1. Its one `parseRPU failed`
  (`VesDmx Failed for pts : 0`) appears for the production author's 8.1
  output too.
- **The mapping changes nothing.**
  - Luma ×0.5 gives a screenshot byte-identical to the identity RPU's.
  - Patch clip A (F+ curves) gives screenshots byte-identical to C (identity
    RPU), in two repeats.
  - The patches vary Cb and Cr by up to ±72 codes, so this covers the chroma
    MMR as well as luma. B differs from A on 30 of 48 patches (by up to 10
    levels), so the test can see a change of that size.
  - A +0.15 Cr constant showed no visible tint. That capture landed on a
    different frame, so it was judged by eye.
- **Scope.** This holds for one device, one app and one decoder build, with
  8.1 RPUs converted from P7 by the same `dolby_vision` crate that production
  uses. It rules out pre-composer capture, because L1 changes show. It does
  not tell a deliberate 8.1 composer bypass from a silent rejection of
  non-identity curves. For those clients, F+ looks exactly like F.
- The Google TV Streamer still can't be observed: its video plane is black to
  screencap and its monitor is HDR10-only. The Pixels have no Dolby Vision
  display support.

## What it changes

- F+ is a strict improvement over today's F at no encode cost where the
  decoder applies the 8.1 mapping (measured on macOS), and identical to F
  where it doesn't (measured on the Lenovo TB322FC). It should replace identity
  curves in the 8.1 conversion for P7 FEL titles.
- It is not a replacement for re-encoding where frame detail matters. The
  per-title rule needs both arms: serve F+ by default, and re-encode only
  where a trial beats F+ by enough to pay for the encode.
- To build it, the per-title job fits Fp_safe curves from BL+EL once per
  scene (CPU seconds) and stores them, and the 8.1 conversion writes them
  into each RPU.
