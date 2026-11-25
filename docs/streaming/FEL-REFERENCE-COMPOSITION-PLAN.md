# FEL reference composition — build plan

**Status:** proposed — not started; awaiting owner go-ahead for milestones 1–2
(measurement only) · **Written:** 2026-11-23 · **Owner:** Claude ·
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
