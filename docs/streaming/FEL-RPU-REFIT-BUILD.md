# FEL RPU re-fit (F+) — build plan

**Status:** plan; PR 1 (the arithmetic) in review · **Written:** 2026-11-12 ·
**Owner:** Claude · **Decision:** Paul approved building F+ and per-title
re-encode trials against it, 2026-11-12

Companion to the offline result,
[RPU-REFIT.md](../../tools/dv_processing/diagnostics/fel_quality/RPU-REFIT.md),
and the investigation ledger,
[FEL-PICTURE-QUALITY-INVESTIGATION.md](FEL-PICTURE-QUALITY-INVESTIGATION.md).
This file says how F+ gets into the product. Work milestone by milestone
(§6). If a step seems to need a change to the processed FEL→8.1 route or to
a client, stop and flag it: F+ is a server-side change to RPU bytes only.

## 1. Objective

A Profile 7 FEL title converted to 8.1 today plays the disc's base layer
with identity mapping curves. F+ keeps those bits and writes per-scene
mapping curves fitted to the FEL composition into each 8.1 RPU instead.

- **What it buys:** offline, F+ removed 54–59% of the identity serve's
  pooled ΔITP on two Michael windows and 34% on Beekeeper. Every frame got
  better.
- **Which clients see it:** clients that apply the 8.1 mapping see it
  (measured on macOS). Clients that ignore the mapping show exactly what
  they show today (measured on the Lenovo TB322FC).
- **What it doesn't do:** it does not replace re-encoding where EL detail
  matters. The best encodes still win Michael frame 0, so per-title trials
  against F+ are part of this plan (§5).

## 2. How it fits together

```
 P7 FEL title (BL + EL + RPU)
        │
        ▼
 DvRefit job (per title, any node)          §3.1–3.3
   helper --refit-stats: decode BL+EL, compose at a stride-8 lattice,
   accumulate per scene (scene_refresh_flag) ──▶ stats per scene (JSON)
        │
        ▼
 plurx-core dv_refit: solve per scene, guard rails, composer domain  §3.4
        │
        ▼
 store: dv_refit_curves (source identity, fit version, scenes, digest)
        │
        ├──▶ copy-path converter (fragindex, copyseg, vodgen,
        │    copy_normalization): To81, then apply_to_rpu(scene curves)   §3.5
        ├──▶ dv_disk permanent conversion: curves written before the
        │    EL is discarded                                             §3.6
        └──▶ Developer tab + Activity: what was fitted, gain, clients  §4
```

The processed FEL→8.1 route (`dv_author_profile81_window`) is untouched.
Its picture already contains the EL, so identity mapping is correct there.

## 3. Design

### 3.1 Scenes come from the RPU

Curves change only at an RPU `scene_refresh_flag`. The colourist's scene
cuts are where the DM metadata already changes, so a mapping change there
can't flicker inside a shot. A scene with fewer than 24 frames pools with
its predecessor: offline, one fit per 48-frame window cost nothing against
per-frame fits, and short scenes give thin statistics.

### 3.2 Composition in the helper, on the CPU

`segment_decode_render.c` already decodes both layers. It gets a
`--refit-stats` mode that:
- reads the host planes it already builds (`native[2]`);
- composes each stride-8 lattice pixel on the CPU from the frame's RPU, as
  mapped BL plus the NLQ-dequantised EL residual, in float (the model A
  family, which libplacebo-style decoders implement);
- rounds the result to 12-bit;
- pairs chroma the way the frozen tools pair it;
- accumulates `RefitStats` per scene and prints one JSON object per scene.

The mode needs no GPU, so any node can fit. The NLQ arithmetic is ported from
`fel_renderer.h`'s shader, and the port is checked against the frozen tools
(acceptance in §6).

### 3.3 The job

`JobKind::DvRefit` copies `DvTitleScan`'s shape (`source_probe/title_work.rs`):
- `may_execute_job` and admission;
- a demand query of P7 FEL titles without current curves;
- a claim with lease, heartbeat and cancellation;
- `ChildWork::background("Dolby Vision F+ curve fit")` as the Activity
  label;
- a publish step.

It yields to media probes like the title scan does. It is attributable and
stoppable from the background-jobs page, because it decodes a whole title
on the CPU (Paul's rule: hardware use has to be visible and stoppable from
inside the product). Request identity hashes the file, the probe revision,
the helper identity and `dv_refit::FIT_VERSION`.

### 3.4 Solving and guard rails

The job solves each scene with `RefitStats::solve` (PR 1). A scene's curves
are served only when all of these hold; otherwise that scene keeps identity
curves:
- the luma curve rises;
- every coefficient is writable, in [−64, 63);
- the luma curve departs from identity by ≤ 256 12-bit codes;
- the chroma MMR departs by ≤ 512 codes on the 17³ grid.

Offline, the worst departures were 33 (luma) and 80 (chroma) codes. The
limits are 8× and 6× that: wide enough not to bite on real content, tight
enough to stop a pathological scene.

The job also records each scene's code-domain gain (mean |composed − served|
for identity against F+, on its own lattice), and the title total is shown
in the UI. A title whose total gain is below 5% keeps identity curves
everywhere, because a negligible gain is not worth new bytes in every cache.

### 3.5 Serving: the copy-path converter

- `dvconvert::convert_length_prefixed` gains a curve argument. After `To81`
  it calls `apply_to_rpu`.
- `dvpipe::Converter` resolves each sample's scene from its presentation
  time (the fragment's `tfdt` and `trun` cts) against the stored scene table,
  which keys scenes by their first presentation time. It never uses a
  running count, because a seek starts mid-title.
- `DV_CONVERSION_TRANSFORM_REVISION` becomes `dv-p7-to-p81-rpu-v2`, and the
  title's curve digest joins the fragment-index identity. Indexes built with
  identity curves stay valid for titles without curves, and are rebuilt for
  titles with them.
- Caller sites: `fragindex.rs`, `copyseg.rs`, `vodgen.rs` and
  `copy_normalization.rs` (survey of 2026-11-12; re-verify at build time).

### 3.6 Serving: permanent on-disk conversion

`dv_disk::build_and_verify` strips the EL first (`filter_units=remove_types=63`)
and converts with `dovi_tool -m 2`. With `keep_original` false, the source
and its EL are then deleted.

- **Ordering:** for a P7 FEL title, the disk conversion waits until the
  title has a current fit, or has a recorded refusal. Otherwise the EL would
  be gone before anything could fit it.
- **Writing the curves:** when curves exist, plurx writes the converted RPUs
  itself (`To81` + `apply_to_rpu`, ordered by presentation time) and injects
  them with `dovi_tool inject-rpu` instead of `convert`. The replacement's
  verification also reads the curves back.

## 4. Developer tab and visibility

Developer gets an enable section, "F+ curves for converted Dolby Vision"
(setting `playback.dolby_vision_refit`). Its requirements are advisory, per
the house rule, and never block the enable:
- the helper supports `--refit-stats` on this node;
- *N of M* P7 FEL titles have been fitted;
- clients measured to apply 8.1 mapping: macOS yes, Lenovo TB322FC no, the
  others unobservable.

Playback info for a converted P7 title gains a line: "F+ curves: fitted,
−N% code error" or "identity (why)".

## 5. Per-title re-encode trials against F+

Paul's decision (2026-11-12): a re-encode is worth trying per title, now
that the cluster can carry the work.

- **Recipe:** NP1 (psy on), decided over P1 because P1's ΔITP edge is
  0.02–0.03 and it costs about 1.5 points of grain.
- **Trial:** encode NP1 on sample windows across the cluster, score them
  against F+ (not F) in the four models with the helper's master as
  reference, and promote a full-title replacement only when the trial beats
  F+ by a margin.
- **Where it plugs in:** `dv_fel_qualification` already renders sample
  windows three ways for the processed route. The trial adds F+ as the
  comparison arm there.
- The design and the margin get their own plan once F+ serves, because they
  need F+ scores per title to set the bar.

## 6. Milestones

One PR each, in order. Each lands only after an adversarial review and one
green fast lane.

1. **The arithmetic** (`plurx-core/src/transcode/dv_refit.rs`):
   - **Contents:** stats, safe solve, diagnostics, composer domain,
     `reshape`, and `apply_to_rpu`.
   - **Acceptance:** `cargo test -p plurx-core --lib dv_refit` passes, which
     includes:
     - coefficients within 2·2⁻²³ of `rpu_refit_eval.py` on all three
       fixtures;
     - the composer domain within 1e-6;
     - a real P7 RPU round trip exact to 2⁻²³.
2. **Helper `--refit-stats`:**
   - **Contents:** CPU composition, the lattice, and per-scene JSON.
   - **Acceptance:**
     - on the Michael5400 window, curves solved from the helper's stats
       score within 0.01 pooled ΔITP of offline Fp_safe in all four models,
       through `rpu_refit_eval.py --readback`;
     - per-scene sample counts equal the lattice size × frames.
3. **`DvRefit` job and store:**
   - **Contents:** table, demand, claim, publish, Activity label and cancel.
   - **Acceptance:** a job on Michael5400's source writes curves whose
     digest is stable across two runs. Cancelling it mid-run leaves no rows.
4. **Copy-path serving:**
   - **Contents:** pts → scene, revision v2, and the identity digest.
   - **Acceptance:**
     - a fragment index built for a fitted title carries RPUs whose curves
       parse back equal to the stored ones;
     - a seek window's first RPU has its scene's curves;
     - an unfitted title's bytes are unchanged from v1.
5. **dv_disk:**
   - **Contents:** the ordering and the injection.
   - **Acceptance:** a disk conversion of a fitted title reads back the
     stored curves frame by frame, and an unfitted FEL title waits.
6. **Developer section and playback info:**
   - **Acceptance:** each state renders on the web and Apple Settings, and
     a dv_patch_clips run on macOS against a served fragment shows A == B.
7. **Re-encode trials (§5):** plan first, then build.

## 7. Non-goals

- **No client change.** Clients that ignore the mapping lose nothing, and
  making them apply it is not ours to do.
- **No change to the processed route.** It already composes the EL.
- **No per-frame curves.** They gained under 0.002 offline and would invite
  flicker.
- **No MEL titles.** A MEL carries no picture detail, and `To81` already
  keeps its mapping.
- **No gating.** The Developer section reports readiness; it does not block.
