# Display management precision — shader parity and the CM v2.9 residual

**Status:** done (precision repair and candidate disposition) · **Owner:** Codex · **Updated:** 2026-10-25 ·
**Branch:** `codex/cm29-residual-dm-parity` · **PR:** #1284 (authoritative landing state)

Companion to the [display-target build ledger](DISPLAY-TARGETED-DV-BUILD.md).
The 2026-11-11 CM4 normative ST 2084 helpers (split-float PQ, analytical
black, exact white) and their native fidelity results are tracked in the
[processing ledger](DV_HDR_PROCESSING_STATUS.md); this page's legacy `pq_*`
repair still serves CM v2.9, lattice and horizontal passes.
This page tracks the shader precision repair and the separate near-black
model experiment handed off on 2026-10-25. It records measured scope;
physical display acceptance remains in the build ledger.

## 1. Work and merge state

| Work | State | Remaining |
|---|---|---|
| PQ shader precision | Reviewed; metadata controls and both-driver parity passed | PR #1284 records the current fast-lane result and landing state |
| CM v2.9 near-black residual | Affine candidate rejected; no production correction | Reference operation remains unidentified; private study records the rejected hypothesis |
| PR ownership | Codex owns implementation and merge | Current head, checks and landing are recorded in PR #1284 |

No deployment or service restart is part of this work. Paul owns deployment.
The near-black experiment may close with a documented rejection: a correction
ships only with evidence that every named title improves, including the dark
band that regressed in the previous hue-only experiment.

## 2. PQ arithmetic — repair the conditioning

The original float32 OETF computes a ratio near one and raises it to
78.84375. Rounding in the ratio is amplified by that exponent. The EOTF
subtracts 0.8359375 from a nearby value, losing relative precision. This
accounts for the driver-dependent parity failures measured in the handoff:
real-crop maximum 0.0567 dE_ITP on software Vulkan and a synthetic channel
error of 0.069 PQ on NVIDIA.

The shader now uses algebraically equivalent forms. With `t = (Y/10000)^m1`,
the OETF ratio minus one is exactly
`-0.1640625 * (1-t)/(1+18.6875*t)`. Evaluate its power with compensated
`log1p` and `exp`. For the EOTF, subtract in log space relative to the PQ
code of zero, then use compensated `expm1`; the denominator is
`3.22998046875 - 18.6875*d`. Preserve the existing input and extended-domain
denominator clamps. GLSL provides no native `log1p` or `expm1`.

The double reference and display-management model are unchanged. Method
identity remains unchanged if final measurement confirms numerical precision
only. All real-case limits and synthetic dE limits are unchanged. The synthetic
channel acceptance is extended to permit either the original error below
0.05 PQ or a linear-light error below 1e-4 of the actual target peak, per
channel. This measures a bounded near-black light discrepancy where PQ is
poorly conditioned; it is not proof that all accepted error is rounding.
The per-channel alternative preserves the original bright-channel acceptance
instead of imposing a new blanket linear-light restriction. Both output
rasters must be finite and in [0,1] before any acceptance calculation;
otherwise EOTF clipping could hide an invalid output.

## 3. Near-black model — evidence before a correction

The private experiment sampled two L1 sets, two P3 display peaks (600 and
1000 nits), 30 input intensities, 24 hues and seven fractions of the P3
boundary: 10,080 samples per target. The affine map was trained on the first
L1 set and 16 hues; the second L1 set and remaining hues were held out.
Exclude the 2.63–2.64% union of boundary-touching samples before interpreting
the clean validation statistics. Use the model's pre-gamut base intensity
`Ib`; it agreed with remeasured output IPT intensity to 2.7e-15 on the
synthetic samples.

| P3 peak | Baseline mean / p95 dE_ITP | Affine mean / p95 | Through-origin mean / p95 |
|---|---|---|---|
| 600 nits | 0.5150 / 1.4822 | 0.4721 / 1.0776 | 0.5141 / 1.3029 |
| 1000 nits | 0.5991 / 1.6378 | 0.5451 / 1.2125 | 0.5972 / 1.4961 |

The affine offset is not a valid neutral-axis mechanism: its median P/T
magnitude is 0.000582 / 0.000676, versus reference neutral chroma of only
1.8e-6 / 1.9e-6. Neutral mean dE_ITP worsens from 0.087 to 0.372 and from
0.124 to 0.450. A through-origin fit preserves neutrals but improves clean
held-out mean error by less than 0.4%. These results reject the proposed
affine correction; they do not identify the remaining reference operation.
The expensive title screen was bounded after the neutral failure: no
four-title or Smile dark-band acceptance is claimed. Reference generation
completed; any optional first-title diagnostic is retained privately. Those
acceptance checks would be mandatory for a future shipping candidate. They
are unnecessary to reject this one. No CM v2.9 model change is included.
Reference output, metadata and fitted private research artifacts stay in the
private research repository. Only a justified closed-form correction and
source-backed conclusions belong in plurx.

## 4. Validation and decisions

The committed source control build passed with `-Wall -Wextra -Werror`
in the existing isolated dependency image. It did not execute tests. Scoped
knowledge queries returned no reviewed coverage for these files; that is
not proof of absent contracts.

The independent adversarial agent reviewed source `4dd956372` against
`9bcfa9ca1` and found no merge-blocking source issues. The same review
assessed the measured synthetic residual and approved the guarded channel
acceptance extension. No tests were executed by the reviewer.

The production shader source is unchanged after the initial review; its
SHA-256 is `4718021964d300d692e648d3685989cb4ba8fb9c373d551232e6de3d0c293a87`.
Committed source was archived without Git state or credentials to the
isolated control workspace. Both driver manifests were selected explicitly.

| Driver | Frame set | Cases rendered | Max real dE_ITP | Max real channel PQ error | Max synthetic dE_ITP |
|---|---|---|---|---|---|
| llvmpipe | Last Breath | 29 | 0.0375 | 3.59e-5 | 0.4411 |
| llvmpipe | Sound of Music | 29 | 0.0424 | 3.64e-5 | 0.4411 |
| NVIDIA | Last Breath | 29 | 0.0118 | 6.60e-5 | 0.1246 |
| NVIDIA | Sound of Music | 29 | 0.0152 | 6.60e-5 | 0.1246 |

The original channel-PQ rule passed all NVIDIA cases. The only software
Vulkan failure on each frame set was `cm4_syn_2000_p0`: cyan input
`(0,0.875,0.875)` yielded a red-channel discrepancy of 0.06922 PQ but only
0.12847 nit, or 6.424e-5 of the actual 2000-nit target peak. Its dE mean was
0.0009 and maximum 0.1287. The bounded channel allowance accepts that small
near-black light discrepancy while retaining the original PQ acceptance for
other channels. It does not assert a blanket 1e-4 linear-light bound.

Only those two failed cases were rendered again after the reviewed
controller change; both passed. The actual new predicate and domain guard
were also applied to all 116 retained output rasters: no invalid output or
synthetic channel failure. The remaining successful GPU cases were retained.
`test_dv_dm tools/dv_processing/controls/tags` passed. Named selection uses:

```bash
python3 tools/dv_processing/controls/run_dm_parity.py \
  "$CONTROL" "$SCRATCH" "$FRAMES" cm4_syn_2000_p0  # Rerun this case only.
```

The main fast lane runs the five precision/tolerance regressions in
[the source-extracted test](../../tests/operations/test_dv_dm_pq_precision.py).
Its current-head result and any failed-method-only rerun are recorded in
PR #1284; merge requires green checks. No throughput benchmark or physical
display acceptance is claimed. The display-management method stays v4:
the double equations and creative mapping are unchanged, and measured
real-case differences remain within the existing precision contract.

## Reconstruction sampling — one master for the server and the player

Built 2026-11-12, not deployed. The mapping above runs on a master the helper
and the player each reconstruct; this section records why those masters
differed and how they were made one.

### What was wrong

The server's Dolby Vision helper and the native player both reconstruct a
BT.2020/PQ master from the base layer, the enhancement layer and the RPU, and
then run the same display-management code on it. They did not reconstruct the
same master:

| | Server helper (before) | Native player |
|---|---|---|
| Renderer | libplacebo snapshot, API 374 | libplacebo 7.387 |
| Plane sampling (BL chroma, EL luma and chroma) | nearest neighbour, with a 1/64-pixel tie bias | bilinear, each plane at its own siting |
| Enhancement-layer dequantization | on the unrounded sample | on the resampled code, rounded to an integer |
| 10-bit plane storage | code / 1023 in float | code << 6 in UNORM16 |
| Reconstruction target | 32-bit float | UNORM16 |

The display-management stage was faithful on both sides. Every licensed
comparison of that stage had been made on the player's master, so the helper's
output (HDR10-E, display-targeted HDR10-E, and the picture baked into Profile
8.1) inherited a difference nobody had measured.

### The contract now

The helper builds the release the player builds
([`fetch_renderer_sources.py`](../../tools/dv_processing/fetch_renderer_sources.py)
pins the row in the player's
[`upstream.lock`](../../clients/linux-player/packaging/upstream.lock)) and
gives it the same inputs:

- every BL and EL plane sampled bilinearly, no point policy and no tie bias;
- software planes staged as code << 6 in UNORM16 (bits 16/10/6), which is also
  what a mapped GPU-decoded surface holds;
- reconstruction into a UNORM16 texture, which display management then reads.

[`test_dv_reconstruction_sampling.py`](../../tests/operations/test_dv_reconstruction_sampling.py)
fails if the two sides' renderer pins, plane scalers or plane storage drift
apart again.

### Proof

Two real Profile 7 FEL frames, one per title (SourceB frame 16, a 4000-nit
master; SourceA frame 16, a 1000-nit master). Helper on an Intel integrated
GPU; player master and licensed references from the earlier NVIDIA captures.

Server master against the player's master, 16-bit codes:

| | Before | After |
|---|---:|---:|
| SourceB 16, largest difference | 3293 | 1 |
| SourceB 16, components that differ | 96.3% | 6.4% |
| SourceA 16, largest difference | not measured (ΔE 64.0) | 1 |
| SourceA 16, components that differ | not measured | 4.8% |

The remaining single codes are the two GPUs rounding the same shader
differently; software decode and GPU decode give byte-identical output.

Server output against the licensed reference rendered from the player's
master (ΔE ITP, worse / equal / better per pixel):

| | SourceB 16, 1000 nits | SourceA 16, 400 nits |
|---|---|---|
| Largest / mean ΔE, before | 31.17 / 0.755 | 62.77 / 1.402 |
| Largest / mean ΔE, after | 0.041 / 0.0023 | 0.041 / 0.0023 |
| FEL against the server's own unenhanced render, before | 30,510 / 0 / 7,941,330 | 1,538,707 / 0 / 4,636,013 |
| FEL against the server's own unenhanced render, after | 6 / 3 / 7,971,831 | 190 / 73 / 6,174,457 |
| Largest amount a "worse" pixel is worse by, after | 0.013 | 0.022 |

Display management alone, against the licensed render of the server's own new
master: largest ΔE 0.028 and 0.027.

### Limits

- Two frames, neither brighter than 450 nits. CM v2.9 titles and the SDR
  bucket were not re-measured.
- Which enhancement-layer resampler Dolby specifies is not established here.
  What is established is that the two renderers now agree and that the
  licensed qualification applies to both.
- The published media runtime has to be rebuilt for the helper to change; the
  processed-output cache keys change with it (helper, library and source
  hashes are all part of the backend identity).
