# FEL reference composition — build plan

**Status:** in progress — owner approved milestones 5.1–5.2 on 2026-11-24;
5.1 built (section 8); 5.2 measured on real titles and the synthetic
controls with a **go** result (section 9) · **Written:** 2026-11-23 · **Owner:** Claude ·
**Source baseline:** `bb2f165`

Companion to [FEL-P81-RECONSTRUCTION.md](FEL-P81-RECONSTRUCTION.md) (the
served route) and
[FEL-PICTURE-QUALITY-INVESTIGATION.md](FEL-PICTURE-QUALITY-INVESTIGATION.md)
(the quality ledger). That ledger records that the bound controls "do not
establish the absolute Dolby composer domain or normative spatial filter".
The gap was raised again by the
[CB1 comparison](../reviews/CB1-COMPARISON-2026-11-23.md). This plan closes it: it builds an independent reference for Profile 7
FEL composition, measures how far today's renderer is from it, and replaces
the composition only if the measurement says it matters.

Work milestone by milestone. Milestones 1–2 change no served picture. If a
step seems to require changing live playback, a client setting, the encoder
or the F+ method before milestone 3's decision, stop and flag it.

## 1. Objective

Cinema composes the FEL picture with stock libplacebo: every base and
enhancement plane is upsampled bilinearly to full resolution, then the
residual is added in floating point. The published composer model
(ETSI GS CCM 001) specifies its own enhancement-layer filters, composes on
the 4:2:0 grids before chroma upsampling, works in integer code values and
clamps the residual at `vdr_in_max`. Cinema's renderer has no clamp, so it
refuses every title where the clamp could bind.

Three outcomes, in order:

1. A reference that shares no code with the renderer under test.
2. A measured deviation of today's composition from that reference, by
   luminance band, on real titles.
3. If the deviation is material: a composition stage that matches the
   reference, and admission of the titles the missing clamp refuses today.

## 2. What is true today (re-verify at build time)

| Fact | Where |
|---|---|
| Composition is `pl_render_image` with `plane_upscaler = pl_filter_bilinear` | [`fel_renderer.h`](../../tools/dv_processing/fel_renderer.h) lines 589–597 |
| Admitted subset: denominator 23, 10-bit BL and EL, 12-bit VDR, LINEAR_DZ, one partition, no explicit resampling filter | `fel_renderer.h` `map_fel_rpu`, lines 119–151 |
| No `vdr_in_max` clamp; a title is admitted only when the clamp cannot bind | [`nlq_clipping.h`](../../tools/dv_processing/nlq_clipping.h) lines 8–33 |
| BL and EL frames pair by exact PTS and duration | [`segment_decode_render.c`](../../tools/dv_processing/segment_decode_render.c) near line 730 |
| The helper's FEL reference is the same libplacebo composition | [`fel_composition_proof.py`](../../tools/dv_processing/controls/fel_composition_proof.py) |
| The helper's libplacebo patch series is empty by design | [`clients/linux-player/packaging/patches/libplacebo/series`](../../clients/linux-player/packaging/patches/libplacebo/series) |
| The Linux player's renderer is the shared one from `rog_hdr_dv`, not this repository | [LINUX-PLAYER-CINEMA-UI.md](../clients/LINUX-PLAYER-CINEMA-UI.md) |
| Representation identity strings name the sampling (`pq-bilinear-rgb48-v1`) | `fel_composition_proof.py` docstring |

The fourth and fifth rows are why existing parity numbers cannot see this
deviation: player and helper agree with each other to ΔE_ITP mean 0.008
because they run the same composition.

## 3. Design decisions

1. **The reference comes from the specification, not from another project.**
   LibreELEC CB1 implements these filters and has an integer oracle, but its
   engine is GPL-2.0-or-later and this code is shared with the server's
   FFmpeg path. Derive every formula from ETSI GS CCM 001 and cite the
   clause beside it. CB1's documentation may be read to find where the
   specification is ambiguous; its code and test vectors are not copied.
2. **Measure before replacing.** The deviation may be below what the encode
   loses. Milestone 2 ends in a go/stop decision with numbers.
3. **The composition stage is Cinema's own shader pass, not a libplacebo
   patch.** The series is empty on purpose, and the display mapper already
   ships this way as a shared header
   ([`dv_dm.h`](../../tools/dv_processing/dv_dm.h)). A shared header also
   lets `rog_hdr_dv` adopt the same code for the player.
4. **A new representation identity, never a silent change.** Composed
   pictures feed prepared outputs, F+ fits and stored evidence. The new stage
   publishes under a new identity so old artifacts are never mistaken for it.
5. **No feature gate.** If both compositions must coexist during
   qualification, the choice is a Developer-tab card with advisory readiness,
   per the repository rule.

## 4. Non-goals

- **No change to pairing, MEL handling or EL recovery.** Worth doing, but a
  separate change; mixing it in would hide which change moved the picture.
- **No encoder, rate-control or F+ method change.** Milestone 5 re-scores
  them; it does not retune them.
- **No change to display mapping.** `dv_dm.h` is downstream and stays as is.
- **No AV1 or dual-track Dolby Vision.** Different gap, different plan.
- **No claim of Dolby conformance.** The reference is the published model as
  read here; it is not a certified decoder.

## 5. Milestones

### 5.1 Reference oracle

A CPU integer implementation of the composer for the admitted subset plus
the clamp: base-layer reshaping (polynomial and MMR), enhancement-layer
upsampling with the specified filters, LINEAR_DZ dequantisation,
`vdr_in_max` clamp, composition and final rounding. Plain C beside the other
helpers, with no libplacebo dependency.

**Acceptance:** unit tests pass on hand-computed vectors for each stage,
including dead-zone edges, clamp-binding inputs and filter edge rows; a table
in this document maps each formula to its specification clause and records
any ambiguity and the reading chosen.

### 5.2 Measure today's composition against it

Run the oracle and the current helper over the same decoded layers: the
synthetic controls, the two frozen *Michael* frames (0 and 24) from the
quality ledger, and one window from two further FEL titles.

**How to read it:** report code error (12-bit) and ΔE_ITP as mean, p99 and
max, split by the ledger's luminance bands, with the band below 0.1 nit
called out because that is where the open shadow loss lives.

**Acceptance and decision:** a committed JSON result and a short findings
section. Continue to 5.3 if any title shows mean ΔE_ITP above 0.01, max
above 0.1, or band RMS above 0.5 code. Those are the floors the existing
player-versus-helper parity already resolves, so anything above them is a
real difference. Below them on every title: stop, record the result, and
keep only the oracle as a test.

### 5.3 Matching composition stage in the helper

A GPU pass that reproduces the oracle: specified EL filters, composition at
component resolution before chroma upsampling, the clamp. Published under a
new representation identity.

**Acceptance:** shader-versus-oracle parity control in the style of
[`run_dm_parity.py`](../../tools/dv_processing/controls/run_dm_parity.py),
at most 1 code (12-bit) on every vector and every measured real frame; 4K
frame time on the worker GPU within 10% of the current stage.

### 5.4 Admit clamp-binding titles

Remove the non-binding-only refusal now that the clamp exists.

**Acceptance:** `test_nlq_clipping` updated with binding cases that pass
through the oracle; a library census of FEL titles refused for this reason
before and after.

### 5.5 Requalify what depends on the composed picture

Re-run `fel_composition_proof.py`, the per-title FEL gain qualification and
the F+ comparison on the new identity, only for evidence the change
invalidates.

**Acceptance:** each verdict recorded against the new identity; any title
whose FEL gain verdict flips is listed with both scores.

### 5.6 Player

Carry the shared header into `rog_hdr_dv`'s renderer so the Linux player
composes the same way. This is in another repository and needs its owner's
scheduling.

**Acceptance:** player-versus-helper parity at or below today's figures
(ΔE_ITP mean 0.008, max 0.052 on *Michael*); owner's visual check last.

## 6. Risks

- **The specification may be ambiguous in places.** CB1 records an erratum
  reading of one MMR index. Each ambiguity is listed in 5.1's table with the
  reading chosen, so a later correction is one edit.
- **There is no certified ground truth.** Without a capture from a certified
  player, the oracle proves agreement with the published model only. If a
  Dolby reference decode of a FEL title is available, add it to 5.2.
- **The deviation may be small.** That is what 5.2 is for.
- **Evidence churn.** A new identity invalidates stored proofs; 5.5 bounds
  the re-run to what actually depends on composition.

## 7. Related work

- `E-19CBADF32B` — offline FEL shadow-loss investigation (handoff, stale).
  Milestone 5.2's shadow-band result is direct input to it.
- `E-C599053EB8` — F+ qualification and PR #1565. Milestone 5.5 re-scores
  against it and must not run while its trials are in flight.
- `E-AEFB11302B` — `rog_hdr_dv` upstream work. Milestone 5.6 lands there.

## 8. Specification clause map (milestone 5.1)

The oracle is [`fel_reference_composer.h`](../../tools/dv_processing/fel_reference_composer.h);
its hand-computed vectors are
[`test_fel_reference_composer.c`](../../tools/dv_processing/test_fel_reference_composer.c),
run by `tools/dv_processing/test_metadata.py`
(`test_fel_reference_composer_vectors`, under the undefined-behaviour
sanitizer). Source: ETSI GS CCM 001 V1.1.1 (2017-02), the free ETSI
publication; the PDF is not committed.

| Stage | Clause | Function | Vectors cover |
|---|---|---|---|
| Arithmetic shift | 3.2.2 | `frc_asr` | negative values floor |
| Pivot values | 5.3.2 `Assign_pivot_values` | `frc_assign_pivot_values` | running sum |
| Pivot index | 5.4.2.2 | `frc_pivot_index` | each boundary, top pivot (R2) |
| Polynomial mapping | 5.4.2.3.2 | `frc_map_polynomial` | linear, quadratic, negative and 16-bit clamps, pivot clamp, 8-bit BL |
| MMR luma down-sampling | 5.4.2.3.3 | `frc_downsample_luma` | interior and replicated edge (R1) |
| MMR mapping | 5.4.2.3.3 | `frc_map_mmr` | each order's cross term, a square term, per-component pivot clamp, largest legal coefficients (R6) |
| NLQ_LINEAR_DZ and the `hdr_in_max` (`vdr_in_max`) clamp | 5.4.3.2 | `frc_nlq_linear_dz` | dead centre, ±1 code, range ends, threshold, binding clamp inside the code range, floor of negatives, 8-bit EL |
| Reconstruction | 5.4.3.3 | `frc_reconstruct` | half-up rounding, both clamps, `disable_residual_flag`, 10-bit output |
| EL vertical up-sampling, luma and chroma | Annex B.3 | `frc_el_up_vertical` | step response, overshoot (R4), chroma phase |
| EL horizontal up-sampling | Annex B.2, B.3 | `frc_el_up_horizontal` | even copy, odd taps, undershoot clip |
| Whole picture | 5.4, Annex B.3 | `frc_compose` | zero and constant residual, MMR at a chroma site, refusals |

**Readings where the text is ambiguous.** Each is one edit if a certified
capture later disagrees.

- **R1. Down-sampling index order.** Clause 5.4.2.3.3 writes
  s\*<sub>i,j</sub> from s<sub>2i±1,2j</sub> and s<sub>2i±1,2j+1</sub> without
  naming which index is horizontal. Read as *i* horizontal: a [1 2 1]
  horizontal tap on two rows, averaged, which is chroma co-sited
  horizontally and midway vertically. That is the siting Annex B.3's 1/4–3/4
  vertical chroma filter implies.
- **R2. Index past the last piece.** Clause 5.4.2.2 returns
  `num_pivots_minus2 + 1` for a sample at or above the top pivot, which names
  no coefficients. The mapping clamps such a sample to the top pivot, so the
  last piece is used.
- **R3. Resampling is informative.** Clause 4 note 1 makes resampling
  non-normative; Annex B is "a reference implementation". The oracle uses
  Annex B for a half-resolution EL because the helper admits only RPUs with
  `el_spatial_resampling_filter_flag` set and no explicit filter. The
  oracle does not cover the BL resampling of Annex B.2 (Profile 7 BL is full
  resolution).
- **R4. EL overshoot.** Annex B clips filter output to 0..0xFFFF, not to
  the EL code range, so an up-sampled EL code can exceed 2<sup>EL_bit_depth</sup>−1
  before dequantisation. Kept as written.
- **R5. Denominator range.** Clause 5.3.2 says `EL_bit_depth + 5` to 32;
  Annex A says 0 to 23. The oracle accepts 5.3.2's range (its lower bound
  keeps clause 5.4.3.2's final shift nonnegative). The admitted subset's 23
  satisfies both.
- **R6. Register width.** No clause states one. The largest coefficients
  the syntax allows overflow a 64-bit MMR sum (22 terms of up to 2<sup>59</sup>), so
  the oracle sums in 128 bits; every other intermediate fits 64 bits exactly.
- **R7. Typography.** Two expressions have an unbalanced parenthesis
  (`RR = (R << …` in 5.4.3.2, `v = (rr >> …` in 5.4.2.3.3), read with the
  only balanced meaning. `rr <<= 1` on a negative `rr` is a two's-complement
  left shift (3.2.2), implemented as a multiply.
- **R8. MMR order.** Clause 5.4.2.3.3 lists `MMR_order` as 0 to 3;
  clause 5.3.3.3 bounds `mmr_order_minus1` to 0..2. The oracle takes 1 to 3.
- **R9. RPU names.** CCM 001 names `hdr_in_max` and `hdr_bit_depth`; the
  Profile 7 RPU carries `vdr_in_max` and `vdr_bit_depth`. Clause 5.3.1 lets
  the parameters come "by other means"; the 5.2 adapter maps one to the
  other and records it beside the measurement.
- **R10. Out of scope.** A BT.1886 base layer (14-bit reconstruction and
  clause 5.5's conversion) is refused; Profile 7 BL is PQ.
- **R11. Chroma siting.** Annex B.3's vertical EL chroma filter (¼, ¾)
  assumes chroma midway between rows, and clause 5.4.2.3.3's MMR
  down-sampling averages two rows; the measured streams signal top-left
  siting. The oracle follows the text as written, which puts its EL chroma
  ⅛ of an EL chroma row from the renderer's top-left sampling.

The risk in section 6 about an erratum in one MMR index was checked against
the text alone: the term table in 5.4.2.3.3 is internally consistent (each
second- and third-order term is a product of lower ones), so no erratum
reading was needed. CB1 was not consulted.

## 9. Measurement and decision (milestone 5.2)

Tooling: `diagnostics/fel_quality/fel_reference_compose.c` runs the oracle
on a layer capture; `fel_reference_measure.py` compares it with the
helper's own `pq-bilinear-rgb48-v1` render of the same window, bound frame
by frame by RPU hash. Both pictures go through one display path (bilinear
sampling at the stream's siting, then the RPU's `ycc_to_rgb`). Today's
YCbCr codes come from the helper's output through the inverse of that
matrix. A CPU model of the renderer confirms its sampling phases and the
display path to ΔE_ITP 0.04 mean. Result:
`diagnostics/fel_quality/results/fel-reference-measure-20261124.json`.

**Where the renderer samples.** Fitted against its own output: BL chroma
co-sited (top-left, as signalled); the EL co-sited horizontally and
centre-aligned vertically. That is Annex B's phase, so today and the
reference read the EL at the same positions and differ in filter taps,
in composing chroma after rather than before up-sampling, and in rounding.

**Two separable differences.** "Filters" compares the helper with the
reference's unrounded sum `v + r`; "rounding" is clause 5.4.3.3 alone,
which rounds the half-code residuals of these titles' NLQ (slope 2048,
threshold 0) up, so the renderer sits 0.5 code low on every residual
sample (ΔE_ITP about 0.25 mean on its own).

| Title (frames) | Filters: ΔE_ITP mean / p99 / max | Filters, < 0.1 nit: ΔE mean · Y RMS | Filters: Y RMS (code) | Total ΔE mean |
|---|---|---|---|---|
| *Michael* 5400 (0, 24) | 0.26–0.37 / 0.80–1.14 / 2.1–4.5 | 0.18 · 0.87–0.89 | 1.00–1.30 | 0.40–0.49 |
| Private window b2-29, live action (0, 2) | 0.13–0.17 / 0.56–0.64 / 2.3–2.6 | 0.11–0.14 · 0.43–0.50 | 0.46–0.55 | 0.28–0.30 |
| Private window b2-19, animation, MMR chroma (0, 2) | 0.08–0.10 / 0.44–0.50 / 1.1–1.2 | 0.01 · 0.35–0.36 | 0.31–0.38 | 0.16–0.17 |
| Synthetic `valid` control, 64×64, EL at BL size (0, 5, 11) | 0.03 / 0.04 / 0.04 | — (all 10–100 nit) | 0.04 | 0.11 |

The synthetic control's EL is already at BL size, so neither picture
up-samples it; what remains of "filters" there is composing chroma after
up-sampling (Cr RMS 0.07 code), and the rounding dominates (ΔE_ITP 0.10).
The model of the renderer matches the helper on it to ΔE_ITP 0.03, as on
the titles.

**Clamp controls.** Today's helper refuses a bounded `vdr_in_max` before
rendering, so these compare the reference with itself on byte-identical
layers whose `vdr_in_max` is the 1048576 ceiling (the `valid` case).

| Control (frames 0, 5, 11) | `vdr_in_max` | Samples changed by the clamp | Y change (12-bit code) mean / max | ΔE_ITP mean / max |
|---|---|---|---|---|
| `binding-max` (generator case) | 524288 | none | 0 / 0 | 0 / 0 |
| `binding-tight` (measurement variant) | 4096 | all luma, no chroma | 5.4–10.3 / 7–14 | 1.1–2.1 / 1.4–2.9 |

`binding-max` is refused for a clamp that cannot bind on this fixture: its
largest residual is about 0.002 of range against a limit of 1/16. The
`binding-tight` variant (the generator with that one constant changed, not
committed) limits residuals to ±2 codes; the change follows
(|d| − ½) × 0.98 − 2 codes, as clause 5.4.3.2 predicts. Chroma is
unchanged because the fixture's chroma EL sits at its offset. This is the
size of error that admitting a binding stream without the clamp would
make, and the case milestone 5.4 must render.

**Decision: go.** Every title exceeds the mean (0.01) and maximum (0.1)
ΔE_ITP floors on the filters alone, and *Michael* and b2-29 also exceed
the 0.5-code band RMS. Milestone 5.3 should reproduce the specified
filters, compose before chroma up-sampling and round to `hdr_bit_depth`
as clause 5.4.3.3 does.

**Also found.**

- The GPU's bilinear weights are not exact: 4–11 % of pixels round the
  resampled EL one code away from exact arithmetic (one 12-bit code after
  LINEAR_DZ). Today's output therefore differs by a code between GPUs; an
  integer composition stage removes that.
- All four Profile 7 windows on hand were FEL and admitted (their
  `vdr_in_max` cannot bind); two could not be captured by
  `capture_layers` (packet timing) and are not measured.
- Limits: measured on macOS (MoltenVK, Homebrew libplacebo 7.387), not
  the worker's pinned build; two frames per title, three per control.
