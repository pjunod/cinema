# Shadow loss — encoder diagnosis and offline fix tests

**Status:** open — cause identified; zero-mean quantOffsets candidate meets the fixed bar, held-out scene pending · **Date:** 2026-11-11

Companion to the [DV processing ledger](DV_HDR_PROCESSING_STATUS.md).
This is the canonical offline picture-quality investigation record for P02.
Frozen source: `087e836d369eb2b8f94ec1d0454ea6c157612df9`.
The investigation changes no live playback, worker service, client setting
or packaging code. Its raw evidence is retained separately; numerical tools
and diagnostic source deltas are committed in
[`tools/dv_processing/diagnostics/fel_quality/`](../../tools/dv_processing/diagnostics/fel_quality/README.md).

## Earlier calibration changes the interpretation of old scores

The original helper had storage-dependent Dolby colour offsets, sensitive
NLQ arithmetic and a cancelling nearest-chroma coordinate bias. Bound controls
restore representation/backend consistency; they do not establish the
absolute Dolby composer domain or normative spatial filter. Four paired
models retain explicit alternative composition/colour conventions.

The Opus review's swscale gain and inherited-colour black-lift concerns were
reproduced. Its proposed inherited nonstandard RPU matrix explanation did
not hold for the bound fixtures: To81 variants were byte-identical there.
Historical mixed-path stage attributions are marked superseded in the
retained reports. Native pixel/RPU identity and packaging observations keep
their narrower scopes. None of these measurements is an optical DV master.

## The remaining error is local coding loss

The input is the same hash-bound FEL-derived point export. All tested outputs
use the unchanged frozen production Rust author. All 48 coded-picture payloads,
packet clocks and converted RPU hashes survive authoring unchanged. The new
quality measurements use full frames 0 and 24 under the four previously fixed
composition/colour models, with identical source R/F images and
reference-defined luminance masks. This is selected-frame calibration, not a
48-frame quality score, held-out test or native-display qualification.

The direct code-plane audit rules against a simple global brightness offset.
In Michael 5400 frame 24 below 0.1 nit, baseline mean signed Y/Cb/Cr errors
relative to the raw export are approximately +0.037/−0.015/+0.006 codes.
Their RMS values are 1.447/1.326/0.922 codes. The raw-export-minus-original-BL
chroma differences have RMS 0.898/0.681 codes: coding noise can exceed the
exported colour correction. That correction includes conversion; it is not an
isolated normative EL residual.

At one retained tail pixel, (621,1160), raw Y/Cb/Cr [113,514,513] becomes
[113,509,521] in AQ3. Luma is identical; chroma changes −5/+8 codes. This point
is not a population-wide proof. The region-level and hybrid tests provide
separate evidence.

## Ideal component swaps isolate the broad shadow failure

The simulated swaps retain baseline encoded Y with exact raw-export UV, or
exact raw-export Y with baseline encoded UV. They are counterfactual pixel
arrays interpreted with the fixed P8.1 models, not authored streams or an
implementable codec repair. Component error budgets are not additive.

Keeping raw-export UV restores below-0.1-nit mean and p99 advantage versus
fallback in both frames under all four models (8/8). Keeping raw-export Y
alone restores that joint advantage in 0/8. Source-direct Model B, frame 24:

| Interpretation | Shadow mean ΔE_ITP | Shadow p99 |
|---|---:|---:|
| Fallback | 1.7528 | 4.1938 |
| Baseline high-rate encode | 2.0158 | 5.9916 |
| Encoded Y + raw-export UV | 1.1843 | 3.5297 |
| Raw-export Y + encoded UV | 1.6729 | 5.7334 |

This supports protecting UV coding as a targeted remedy for this broad
shadow-tail failure. Deep-black luma error remains in the ideal-UV swap, so
it does not establish that chroma explains every error.

## The first three controls help distinguish encoder policies

The [source matching logged x265 revision e9b8812](https://raw.githubusercontent.com/Multicorewareinc/x265/e9b8812/source/encoder/slicetype.cpp)
adds +3 QP below block-average luma 301 when HDR10 optimization is enabled.
The flag also has other coding effects. Turning it off retains HDR signaling
and Dolby metadata; it does not turn off HDR playback.

All other requested settings stay fixed: slow, AQ2, target 100/max 120 Mb/s,
136-Mbit buffer, no B frames, closed 48-frame GOP, native 24000/1001 and
TopLeft type 2. Encoder reports/user-data identify the requested coding policies; normative
PPS/slice and colour syntax are checked separately where applicable.
Actual rates differ and are reported, so this is not equal-realized-rate
proof of one isolated instruction's effect.

| Michael 5400 control | Actual authored Mb/s | Frame 24 shadow mean / p99, Model B |
|---|---:|---:|
| Baseline | 106.431 | 2.0158 / 5.9916 |
| HDR10 quantization optimization off | 108.127 | 1.9203 / 5.7431 |
| Both psycho-visual terms zero | 105.225 | 1.8917 / 5.8356 |
| SAO off | 106.333 | 2.0300 / 6.0767 |
| Fallback | 76.235 | 1.7528 / 4.1938 |

The four models agree on the relevant directions. None of these controls
removes the frame-24 shadow regression. Disabling HDR optimization improves
shadow mean and p99 in both frames; the other controls have mixed outcomes.
No HDRopt0 result is attributed solely to one luma branch.

## Finer chroma coding improves the result but shifts the budget

The next fixed test changes only Cb/Cr QP offsets to −6. All four models show
better full-picture mean and p99 in both windows. Michael 1800 passes the
below-0.1-nit mean/p99 check against fallback in both frames under all models.
Michael 5400 frame 24 still fails it: Model B improves to 1.7862/5.0514,
versus fallback 1.7528/4.1938. Its frame 0 passes.

Actual rates are 110.613/108.325 Mb/s for Michael 1800/5400, respectively,
0.47%/1.78% above the previous high-rate pair. Coded PPS offsets change from
0 to −6; all observed slice offsets remain zero. Frame-24 nominal luma QP
rises by one in both windows. In Michael 5400's shadow mask, Y error RMS
rises 1.447→1.605 codes while Cb/Cr fall 1.326→1.011 and 0.922→0.746.
Neighbour-gradient luma error also increases. These are code-level tradeoffs,
not a perceptual texture verdict.

Pinned x265 source shows that chroma offsets affect quantizer mapping and RD
weights as well as allocation. The potential HDR-adaptive negative slice
offsets are not observed in this fixture: baseline slice offsets are zero.
The requested −6 is therefore not described as an independently measured
halving of every local quantizer step.

## Combined policies improve the result but still fail the fixed shadow bar

Both combinations, HDR optimization off plus Cb/Cr offsets −6 or −9, improve
full-picture mean and p99 versus fallback and the earlier high-rate recipe
in both frames/windows under all four models. Both improve shadow means,
but Michael 5400 frame 24 still has worse shadow p99 under every model.
The predeclared selection is therefore **none** at the original budget.

Source-direct Model B, reference luminance below 0.1 nit, frame 24:

| Arm | Shadow mean | Shadow p99 |
|---|---:|---:|
| Fallback | 1.7528 | 4.1938 |
| Baseline high-rate | 2.0158 | 5.9916 |
| HDR optimization off, chroma −6 | 1.7293 | 4.8173 |
| HDR optimization off, chroma −9 | 1.7093 | 4.7303 |

The −9 combination reduces Cb/Cr code RMS to 0.890/0.685 while Y rises to
1.648; the baseline values are 1.326/0.922 and 1.447. This is a measured
allocation tradeoff. Nominal frame-24 slice Y QP rises 12→16/17, but this is
not a measured +4/+5 local-Y penalty: HDR optimization off also removes the
local dark-block adjustment, and CU/QG deltas are not traced.

Nine new encoded cases are complete at this stage (three initial controls,
two chroma-only windows and four combined-policy cases). The hybrid arrays
are simulations, not additional encodes. Every prior outcome remains in the
combined artifact.

## Higher budgets improve mean but still miss the fixed shadow-p99 bar

The stronger combined policy was tested at target 110 and 115 Mb/s, both
max 125 Mb/s and 136-Mbit buffer. Compared with the original target100/max120,
this changes target and maximum; the two new rungs share the same maximum.
All other settings, models, metadata and masks remain fixed.

Source-direct Model B, Michael 5400 frame 24 below 0.1 nit:

| Arm | Shadow mean | Shadow p99 |
|---|---:|---:|
| Fallback | 1.7528 | 4.1938 |
| Target 110 | 1.6284 | 4.4550 |
| Target 115 | 1.5972 | 4.3402 |

All four models retain this remaining p99 regression. Both windows pass the
predeclared authored packet-average screen of 130 Mb/s. Actual rates are
120.091/123.749 Mb/s for Michael 5400 and 124.882/129.764 Mb/s for Michael 1800.
The screen is not proof of peak rate, level or HRD/CPB conformance. The highest
rate leaves little screen headroom. Full-picture scores and shadow means
improve, but **no rung is selected** under the fixed no-worse-shadow-p99 rule.
All thirteen new encoded cases and two simulated hybrid cases are retained,
including failed controls. A better mean is not presented as a complete fix.

## Packaging remains a separate compatibility problem

The frozen Rust author unconditionally declares `hev1` through
`declare_hevc_parameter_sets_in_band`. The retained native failing output
starts decoding after only two sample-entry tag bytes change to `hvc1`.
A separate proper out-of-band normalization preserves protected picture/RPU
payloads, clocks, Dolby configuration and native decoded pixels/RPU
attachments for one tested window. A tag-only edit is not a general repair.

The separate two-window configuration audit checks matching configurations;
those newer files have no in-band parameter sets and do not exercise stripping.
Multi-window rewrite integrity, playlist declarations, seeking and native HLS
still need qualification. No packaging fix is applied by this investigation.

## Retained evidence and reproducibility

The committed [evidence index](../../tools/dv_processing/diagnostics/fel_quality/evidence-index.json)
records exact source-copy hashes, the earlier 2,731-file calibration manifest,
and the complete shadow result table hash. Private raw artifacts live under
`~/.local/state/plurx-fel-quality/20261026-measurement-calibration/` and
`~/.local/state/plurx-fel-quality/20261026-shadow-followup/`.
The latter archive is sealed; the evidence index records its manifest. Original requests and
receipts keep their source paths and hashes; location maps describe relocation.
Raw media, binaries and large decoded arrays are deliberately not Git blobs.
The commit preserves code and findings, not an off-machine backup of that media.

A final earlier worker snapshot showed installed-helper hash drift and an
active session. All earlier tests used pinned isolated copies; this follow-up
runs only on retained Mac inputs. The findings do not certify the newer live
binary. This follow-up makes no worker calls or live changes.

No held-out scenes, full-window quality rerun, complete HRD/CPB check,
Dolby spatial-filter conformance or native display presentation is established
by this batch. Packaging remains a separate investigation.

## Evidence map

| Artifact | What it establishes |
|---|---|
| `PROTOCOL*.json` | Fixed staged controls, confirmation and selection before their outcomes |
| `codeplanes-v1/` | Signed errors, RMS, UV-footprint checks, spatial tails and immutable masks |
| `qp-audit/` | Baseline/AQ3 slice syntax and encoder allocation receipts |
| `quality-hybrids-v1/` | Counterfactual component-swap outcomes under all four models |
| `quality-*-v1/` | Actual candidate input/RPU bindings and selected-frame model results |
| `corrected-encodes/` | Frozen author, coded-picture/RPU/clock checks, software planes and encoder logs |
| `offsets-audit/` | Actual PPS/slice offsets and nominal QP; not per-CU quantizers |
| `shadow-stage-comparison-v4.json` | Complete earlier/current outcome table without winner-only reporting |
| `ASTRA-SOURCE-HYPOTHESES.md` | Independent source review and coupled-policy caveats |

## Checkpoint validation has a platform boundary

The 14 paperwork checks pass. All 16 copied/adapted source hashes match the
evidence index; Python syntax and the encoder adapter's CLI parse. The normal
commit hook passes catalog/knowledge lint and pinned Rust 1.97.1 formatting,
but its Mac workspace Clippy/build fails on unchanged Linux-only `dv_runtime`
references in `transcode/manager/create.rs` and an unused
`held_dv_source_origin`. The implicated Rust files are byte-identical to the
frozen HEAD; no Rust, Cargo or Makefile input changes are part of this commit.
The raw failed hook log and source-equality receipt are retained privately.

This is a documentation/diagnostic checkpoint, not a passing Rust or release
qualification. The failure is retained, not repaired by changing playback code.

## Next checkpoint tests quantization policy at the retained rate budget

The next fixed control uses target115/max125/buffer136 with the existing
HDRopt0/chroma−9 policy. It disables RDOQ and its coupled `psy-rdoq` term,
retaining `psy-rd=2` and all other requested settings. At level2, RDOQ can
discard encoder prediction-residual coefficient groups; these are not
identifiable FEL residual coefficients. This is a coupled quantization-policy
test, not proof that one particular branch caused the measured error.

The [committed protocol](../../tools/dv_processing/diagnostics/fel_quality/protocol-quantization-20261026.json)
fixes both existing calibration windows, all four models, masks, rate screen
and strict quality criterion before new outputs. The tool requires a separate
output root. Results remain pending; no live or packaging change is proposed.
A later two-pass comparison, if needed, will retain the same115/125 budget:
125/125 would also change the encoder's internal rate-control regime.

## RDOQ-off result retains the shadow-tail failure

The paired control is complete and [all model/frame summaries are retained](../../tools/dv_processing/diagnostics/fel_quality/results/rdoq0-20261026.json).
Disabling RDOQ and coupled psy-RDOQ does not fix the difficult frame: Model B
below0.1nit p99 rises 4.3402→4.4356, versus fallback4.1938. Its mean changes
1.5972→1.6423. The four models agree on the remaining failure. Actual rates
fall to122.506/126.200Mb/s, so this is a coupled policy/allocation outcome,
not an isolated coefficient-group diagnosis. No candidate is selected.

The next [fixed protocol](../../tools/dv_processing/diagnostics/fel_quality/protocol-two-pass-20261026.json)
tests two-pass allocation at the same requested115/125/136 rate settings,
returning to RDOQ2/psy-RDOQ1. Both passes use the same coding settings and
fresh per-window statistics; first-pass48-record identity and successful
second-pass consumption are checked. The horizon is48frames, not a whole
stream. All quality criteria, rate screens, source inputs and models remain
unchanged. Two-pass results are pending. No live or packaging change.

## Smaller quantization groups test spatial allocation next

The [spatial protocol](../../tools/dv_processing/diagnostics/fel_quality/protocol-spatial-20261026.json)
is fixed before new outputs. It changes only requested `qg-size=32` to8
against the retained single-pass target115/max125/buffer136 HDRopt0/chroma−9
policy. Both baseline bitstreams report qg32; their logs report AQ2,
strength1 and cu-tree1. At pinned x265 e9b8812, the AQ energy sample size and
QP-group granularity change with qg8. This tests local allocation, not guaranteed
shadow protection. RDOQ2/psy-RDOQ1 and the other coding settings remain fixed.
Actual rates may differ; the same selected-frame quality rule and130Mb/s
packet-average screen apply. Both two-pass outputs have completed authoring
with all48 VCL/RPU/clocks preserved; their quality scores are still pending.
No live playback or packaging changes are included.

## Two-pass misses the same shadow tail; source capture now covers48frames

The [two-pass summaries](../../tools/dv_processing/diagnostics/fel_quality/results/twopass-20261026.json)
retain all sixteen model/frame rows. Michael5400 frame24 ModelB shadow
mean/p99 are1.6386/4.5113, versus fallback1.7528/4.1938 and single-pass115
1.5972/4.3402. All four models retain the p99 regression; Michael1800 passes
all eight selected-frame rows. Actual authored packet rates are114.842 and
116.159Mb/s. Both passes successfully used identical requested coding policies
and fresh48-record statistics. Lower actual allocation prevents attributing
this outcome to pass mode alone. No candidate is selected.

The new decoder-only capture and [bound comparison receipts](../../tools/dv_processing/diagnostics/fel_quality/results/layer-capture-20261026.json)
match all48 original Michael1800 BL/EL canonical plane hashes, RPUs, packet
hashes, timestamps, durations and rasters. Every dumped payload is rehashed.
The source has304 coded AUs and302 paired outputs, with two explicitly retained
pre-initial-random-access non-output AUs outside the requested window; their
classification is by timestamp, not a proven RASL NAL type. A clean C build
and five admission/refusal checks pass. This establishes source identity for
broader offline measurements; no full-window pixel-quality score follows yet.

## Claude continuation checkpoint

GPT handed the investigation to Claude on 2026-10-28 with both qg8 encodes
finished but not yet qualified. The sections below record what followed.
[Resuming](#resuming) replaces that handoff.

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

## Resuming

This replaces the 2026-10-28 GPT→Claude handoff. Its boundaries still hold:
offline diagnostics only, with no deployment, live playback, player or
packaging change, and packaging as a separate workstream.

### Where it stands

- **Cause, concretely identified.** x265 under-allocates dark blocks in luma
  and especially chroma. AQ2 sets block QP from texture energy alone, the
  PPS chroma offset is one global value, and Main10 has no per-CU chroma
  offset. The evidence for this chain:
  - Component swaps isolate the loss to coding in those blocks.
  - A source-derived dark-block QP reduction removes the tail.
  - The effect is monotonic in dose (all-48 Michael5400 frames beating F:
    17 → 38 → 46).
- **Candidate.** Encode with a zero-mean per-frame quantOffsets map built
  from the encoder's own input. Each 16×16 block whose mean Y' decodes
  below 1 nit gets −3 or −6 QP, and every other block gets the balancing
  positive offset. Both doses pass the fixed selected-frame bar (16/16
  rows) and the 130 Mb/s screen. At −6 QP, Michael5400 beats F in 46/48
  sampled frames.
- **Not done.**
  - A held-out scene. Both windows were used for the design.
  - The dose choice, which trades shadow error against bright-content
    error and is the owner's decision.
  - Two marginal Michael5400 frames (5 and 6).
  - Any production implementation.

### Evidence and tools

Committed tools and portable results live under
`tools/dv_processing/diagnostics/fel_quality/`, indexed in its README and
`evidence-index.json`. Media, decoded planes and binaries are private.

| Private root (on the M3 MacBook, `pauls-macbook-pro-3.local`) | Contents |
|---|---|
| `~/.local/state/plurx-fel-quality/20261026-shadow-rate-control` | Sealed GPT archive; never modified |
| `~/.local/state/plurx-fel-quality/20261111-qg8-relocated` | APFS clone of the archive; qg8 qualification/scoring (`RELOCATION-20261111.json`) |
| `~/.local/state/plurx-fel-quality/20261111-roi-control` | ROI harness, gates, arms S/Z/Z6, M5400 source capture, lattice runs (`RELOCATION-20261111.json`) |

The original `/private/tmp` working roots were wiped by a reboot. Relocated
paths are derived with every hash re-verified, and sealed originals are
never edited. Refused attempts are kept under each root's `logs/`.

### Next steps

1. **Held-out scene.** Pick a FEL title not used so far. Capture it with the
   frozen `capture_layers` binary and verify it with
   `verify_layer_capture.py`. Then run the baseline CLI encode, the
   `encode_roi_control.py` gates (`none`, `zero`), the arm at the chosen
   dose (`lumazm`), `qualify_spatial_control.py --qg-size 32`,
   `prepare_quality.py`/`run_quality.py` and `lattice_eval.py`. Commit a
   protocol before any output, as every arm here did.
2. **Product path, if approved.** plurx builds encodes as FFmpeg CLI
   argument lists, and a per-frame map cannot be passed that way. The fit
   is an encoder-policy component that attaches the same zero-mean ROI map
   inside the filter chain, for example a small filter in plurx's shipped
   FFmpeg. It must be gated by the same byte-identity test as
   `roi_encode.c` (all-zero map ≡ baseline). It needs its own branch, plan
   and review.

### Validation limits

Selected-frame scores are full resolution. All-48 scores are on a sampled
lattice whose frames 0/24 equal the retained full-resolution RGB exactly.
None of this is a perceptual metric, HRD/CPB or Dolby peak-rate
conformance, a normative Dolby composer or a native-display qualification.
