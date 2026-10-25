# Display management precision — shader parity and the CM v2.9 residual

**Status:** open · **Owner:** Codex · **Updated:** 2026-10-25 ·
**Branch:** `codex/cm29-residual-dm-parity` · **PR:** #1284 (draft)

Companion to the [display-target build ledger](DISPLAY-TARGETED-DV-BUILD.md).
This page tracks the shader precision repair and the separate near-black
model experiment handed off on 2026-10-25. It records measured scope;
physical display acceptance remains in the build ledger.

## 1. Work and merge state

| Work | State | Remaining |
|---|---|---|
| PQ shader precision | Implemented, awaiting review | Independent adversarial review; focused regressions; llvmpipe and NVIDIA parity; fast lane; merge |
| CM v2.9 near-black residual | Affine candidate rejected; title screen finishing | Preserve private measurements and document the negative result; no production model change |
| PR ownership | Codex owns implementation and merge | Keep the draft description current through landing |

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
only. No parity threshold has been changed.

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
The title screen supplies further evidence, not authority to ship an
unexplained table. No CM v2.9 model change is included in this PR.
Reference output, metadata and fitted private research artifacts stay in the
private research repository. Only a justified closed-form correction and
source-backed conclusions belong in plurx.

## 4. Validation and decisions

The committed source control build passed with `-Wall -Wextra -Werror`
in the existing isolated dependency image. It did not execute tests. Scoped
knowledge queries returned no reviewed coverage for these files; that is
not proof of absent contracts.

Pending. Unit suites and the ready-PR fast lane run after the one final
adversarial review. Numerical exploration supplies model evidence during
development. The parity controller accepts optional case names after the three required
arguments so failed cases can be rerun individually. Unknown names fail.
Failed checks are rerun individually; unrelated passing suites
are retained. Final source pins, commands, results and review disposition
will be recorded here before landing.
