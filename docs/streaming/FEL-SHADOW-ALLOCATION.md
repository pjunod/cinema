# FEL shadow loss — dark-block allocation cause and quantOffsets candidate

**Status:** open — candidate meets the fixed bar; held-out scene and dose choice pending · **Date:** 2026-11-11

Continuation of the [FEL quality ledger](FEL-PICTURE-QUALITY-INVESTIGATION.md),
using its fixed protocol, frozen models and admission rule. The
[handoff](FEL-CLAUDE-HANDOFF.md) gives the private evidence roots and how
to resume. Offline diagnostics only. No runtime, playback or packaging
change is made.

## Smaller QP groups miss the same shadow tail

Both qg8 encodes are now authored and scored. The [qualifier](../../tools/dv_processing/diagnostics/fel_quality/qualify_spatial_control.py)
adds the two receipts the handoff required before scoring. The encoder logs
report the AQ tuple 2 / 1.0 / 8 / 1. Coded SPS/PPS give CTB64 with
`diff_cu_qp_delta_depth=3`, so the effective QP group is 8 in both the
encoded and the authored stream. The frozen author preserves all 48 VCL
payloads, clocks and RPUs, and the R/F images and reference masks exactly
match the prior run.

Michael5400 frame 24, ModelB, reference below 0.1 nit:

| Arm | Shadow mean | Shadow p99 | Authored Mb/s |
|---|---:|---:|---:|
| Fallback | 1.752768 | 4.193800 | — |
| Single-pass115, qg32 | 1.597174 | 4.340212 | 123.749451 |
| Single-pass115, qg8 | 1.605252 | 4.381678 | 122.864220 |

Michael1800 passes all 8 selected rows at 129.542 Mb/s. Michael5400 passes
frame 0 but fails the frame-24 shadow p99 in all four models, so 4/8 rows
pass. The mean is also slightly worse than qg32. Finer AQ sampling and QP
groups do not protect the shadow tail. No candidate is selected.
[Portable results](../../tools/dv_processing/diagnostics/fel_quality/results/qg8-20261111.json).

The original `/private/tmp` working root had been wiped by a reboot. This run
used an APFS clone of the durable archive with derived, hash-reverified
paths, and the sealed archive was left untouched. The first attempt was
refused because the sealed RPU list names the wiped paths. That refusal is
kept as an invalid test, not as a quality result.

## A source-luma quantOffsets map fixes the shadow tail but costs rate

The [ROI protocol](../../tools/dv_processing/diagnostics/fel_quality/protocol-roi-20261111.json)
was committed before any map output. It defines one arm. Each 16x16 block of
the encoder's own input whose mean Y' decodes below 1 nit gets −3 QP, passed
as `AVRegionOfInterest` side data. No reference, F or error image enters the
map. The [harness](../../tools/dv_processing/diagnostics/fel_quality/roi_encode.c)
first had to reproduce the retained single-pass115 CLI encode byte-for-byte,
both without ROI and with an all-zero ROI. It passed for both fixtures on
attempt 5. Attempts 1–4 are kept: each one was a harness mismatch, and the
gate caught all of them. The mismatches were global headers, x265's
log-level in the info SEI and a stale file sync.

The map covers 31% (M5400) and 39% (M1800) of blocks on average.

| Fixture/frame, ModelB (base = retained single-pass115) | Shadow mean base→ROI (F) | Shadow p99 base→ROI (F) | Full mean base→ROI | Authored Mb/s |
|---|---|---|---|---:|
| M5400 f0 | 1.114→1.068 (1.566) | 3.388→3.281 (4.741) | 1.583→1.588 | 127.399 |
| M5400 f24 | 1.597→1.481 (1.753) | 4.340→3.995 (4.194) | 1.620→1.640 | 127.399 |
| M1800 f0 | 0.907→0.863 (1.642) | 2.134→2.009 (3.184) | 1.220→1.230 | 130.374 |
| M1800 f24 | 0.934→0.909 (1.717) | 2.237→2.115 (3.164) | 1.357→1.376 | 130.374 |

All 16 rows pass every quality criterion, including the M5400 frame-24
shadow p99 that every earlier encoder control failed. Bands from 1 to
1000 nits lose 0.03–0.07 ΔITP and remain far ahead of F. Michael1800's
authored packet average is 130.374 Mb/s, which misses the unchanged
130 Mb/s screen, so under the fixed bar the arm is **not admitted**.
The map's net QP offset is negative and raises rate. The baseline
already ran at 129.7 Mb/s on that fixture.
[Portable results](../../tools/dv_processing/diagnostics/fel_quality/results/roi-20261111.json).

This is the first direct, source-derived evidence for the cause. The tail
is an allocation loss in dark blocks. x265 AQ2 sets block QP from texture
energy alone, the PPS chroma offset is one global value, and Main10 has
no per-CU chroma offset. Lowering block QP lowers QpC with it, and that
removes the tail.

## Zero-mean redistribution passes the fixed bar on the selected frames

The [zero-mean protocol](../../tools/dv_processing/diagnostics/fel_quality/protocol-roi-zeromean-20261111.json)
was committed after arm S and before any of its own output. It keeps the
same dark selection and −3 QP. Every other block gets
+min(3, 3c/(1−c)) QP, where c is that frame's dark fraction, so the frame
mean offset is zero, like x265's own AQ. The positive offset came out at
+1.16 to +1.69 QP on the selected frames. The new harness binary passed
the byte-identity gate again before its results were used.

| ModelB (base / S / Z, F) | Shadow mean | Shadow p99 | Full mean | Authored Mb/s (Z) |
|---|---|---|---|---:|
| M5400 f0 | 1.114 / 1.068 / 1.063 (1.566) | 3.388 / 3.281 / 3.270 (4.741) | 1.583 / 1.588 / 1.611 | 127.332 |
| M5400 f24 | 1.597 / 1.481 / 1.420 (1.753) | 4.340 / 3.995 / 3.839 (4.194) | 1.620 / 1.640 / 1.642 | 127.332 |
| M1800 f0 | 0.907 / 0.863 / 0.856 (1.642) | 2.134 / 2.009 / 1.997 (3.184) | 1.220 / 1.230 / 1.239 | 128.031 |
| M1800 f24 | 0.934 / 0.909 / 0.882 (1.717) | 2.237 / 2.115 / 2.036 (3.164) | 1.357 / 1.376 / 1.393 | 128.031 |

**All 16 rows pass the strict quality bar and the 130 Mb/s screen.** This
is the first candidate admitted under the fixed rule. The cost lands in
brighter content. The 1–10 nit band rises by 0.05–0.10 ΔITP and the 10–100
nit band by 0.05–0.09. Full active p99 rises by up to 0.20 (M1800 f24) and
the full maximum by up to 2.7 (M5400 f24). All remain far better than F.
Rate is still above the retained baseline on M5400 (127.3 vs 123.7 Mb/s),
so zero mean QP offset is not rate-neutral.
[Portable results](../../tools/dv_processing/diagnostics/fel_quality/results/roi-zeromean-20261111.json).

Limits: the arm passes only on the two calibration windows and the selected
frames 0/24. The fixtures were also used to design it, and frame 24 was not
the worst of the earlier 48-frame samples. Before any broader claim it
needs the all-48-frame lattice evaluation and, ideally, a held-out scene.
It is not a perceptual, HRD/CPB, Dolby-conformance or playback
qualification, and no runtime change is made.

## All 48 frames: zero-mean helps broadly but leaves 10 Michael5400 frames

The [lattice evaluator](../../tools/dv_processing/diagnostics/fel_quality/lattice_eval.py)
implements the predeclared stride-8, phase-0/1 lattice (about 500k samples
per frame) on the imported frozen operators. Before any other frame is
scored, it requires several matches. Frames 0/24 must reproduce the
retained full-resolution R/F/C RGB16 exactly at every lattice point, in all
four models (24 checks). Every frame's metadata numerators must equal the
retained 0/24 metadata, and decoded candidate frames 0/24 must match their
qualified hashes. For Michael5400, the source clip was copied from the
worker and hash-checked, then captured with the frozen binaries. That
capture matches the retained events (48/48 canonical matches, all 48 RPU
hashes bound). Sampled statistics do not replace full-resolution p99 and
do not change the admission rule.

Frames, out of 48, where the sampled shadow (<0.1 nit) p99 beats F, ModelB:

| Arm | Michael1800 | Michael5400 | M5400 pooled shadow p99 (F 4.313) |
|---|---:|---:|---:|
| Retained single-pass115 | 48 | 17 | 4.392 |
| ROI arm S (−3, all negative) | 48 | 29 | 4.069 |
| ROI arm Z (−3, zero-mean) | 48 | 38 | 3.959 |

Models A/C/D agree (Z: 43/39/37). Every arm beats F on the full mean in
all 48 frames. The ten Z misses are frames 6, 12, 18, 20, 21, 29, 34,
35, 38 and 43, by at most 0.234. On every one of them, exact UV with
Z's luma, or exact luma with Z's UV, beats F (48/48 for both swaps).
After the redistribution, the remaining loss is joint Y+UV coding error
in the same dark blocks. That points to too small a dose, not a
different mechanism. [M1800](../../tools/dv_processing/diagnostics/fel_quality/results/lattice-michael1800-20261111.json),
[M5400](../../tools/dv_processing/diagnostics/fel_quality/results/lattice-michael5400-20261111.json)
and [swap](../../tools/dv_processing/diagnostics/fel_quality/results/lattice-michael5400-swaps-20261111.json)
results.

## Dose–response confirms the cause: −6 QP zero-mean reaches 46/48

The [last arm](../../tools/dv_processing/diagnostics/fel_quality/protocol-roi-zeromean-qpm6-20261111.json)
was fixed after the residual swap test. It doubles the dark reduction to
−6 QP, still zero-mean, and declares the Michael5400 all-48 lattice as its
readout.

| ModelB | Base | −3 zero-mean (Z) | −6 zero-mean (Z6) | F |
|---|---:|---:|---:|---:|
| M5400 frames, sampled shadow p99 < F (of 48) | 17 | 38 | **46** | — |
| M5400 pooled sampled shadow p99 | 4.392 | 3.959 | 3.775 | 4.313 |
| M5400 pooled sampled 1–10 nit mean | 1.995 | 2.113 | 2.255 | 3.743 |
| M5400 pooled sampled full mean | 1.699 | 1.733 | 1.802 | 3.200 |
| M5400 f24 full-res shadow p99 | 4.340 | 3.839 | 3.610 | 4.194 |
| Authored Mb/s M5400 / M1800 | 123.7 / 129.7 | 127.3 / 128.0 | 129.1 / 128.4 | — |

Z6 passes the fixed selected-frame bar on both fixtures (16/16 rows) and
the rate screen. Models A/C/D give 47/46/46 frames. The two misses, frames
5 and 6, miss by 0.047 and 0.016. The improvement is monotonic in dose,
the component swaps close every residual frame, and the map is computed
from the encoder input alone. Together these concretely identify the
cause. The remaining FEL shadow loss is x265 under-allocating dark blocks,
in luma and especially in chroma, because AQ sees only texture energy and
Main10 offers only a global chroma offset. The cost of correcting it
moves into brighter content, which stays far ahead of fallback.
[Selected-frame](../../tools/dv_processing/diagnostics/fel_quality/results/roi-zeromean-qpm6-20261111.json)
and [all-48](../../tools/dv_processing/diagnostics/fel_quality/results/lattice-michael5400-qpm6-20261111.json)
results.

## Where this leaves the investigation

The investigation now has a candidate that meets the fixed bar, with a
stated mechanism. It is not a qualified production fix:

- It covers two calibration windows only, which were also used to design
  it. A held-out scene is the next requirement.
- Michael5400 still has two sampled frames marginally worse than F in the
  shadow tail.
- The choice of dose is a trade between shadow and bright-content error,
  and it is the owner's call.
- A per-frame source-derived quantOffsets map cannot be expressed through
  ordinary FFmpeg CLI arguments. plurx builds its encodes as CLI argument
  lists (`crates/plurx-core/src/transcode/encoder.rs`). The fit with that
  design is an encoder-policy component that attaches ROI side data inside
  the existing filter chain. One example is a small filter in plurx's
  shipped FFmpeg that computes the zero-mean dark-block map from each
  frame. That keeps it a coding policy, not a watchdog or a separate
  process. Nothing like that is built or deployed here, and no runtime,
  playback or packaging change is made.

