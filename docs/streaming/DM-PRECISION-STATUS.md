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
| CM v2.9 near-black residual | Experiment running; lower-model agent owns measurements | Chroma-dependent sweep; affine hypothesis; held-out validation; title checks if justified |
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

The private experiment samples two L1 sets, two P3 display peaks, 30 input
intensities, 24 hues and seven fractions of the P3 boundary. It compares
reference output P/T against an affine map of model output P/T by output
intensity. Boundary-touching samples are flagged before interpreting a fit.
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
development. Failed checks are rerun individually; unrelated passing suites
are retained. Final source pins, commands, results and review disposition
will be recorded here before landing.
