# FEL RPU re-fit (F+) — build plan

**Status:** in progress — milestones 1–2 are in main, 3 (the job and
store) and 4 (copy-path serving) are in review; 5–7 are open · **Written:** 2026-11-12 ·
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
can't flicker inside a shot. Scenes shorter than 24 frames are pooled:
offline, one fit per 48-frame window cost nothing against per-frame fits,
and short scenes give thin statistics. The pooling is bounded:
- a scene of 24 or more frames is a group of its own;
- consecutive shorter scenes collect into a run, which becomes a group of
  its own once it reaches 24 frames;
- a run still short when a long scene follows joins the group before it, or,
  at the very start, the group after it;
- a run still short at the end joins the last group.

Every group is at least 24 frames unless the whole title is shorter, and
short scenes never chain into one long group. A title whose RPUs flag every
frame (the summary's `degenerate`) gets 24-frame groups, not one scene.

Scenes are keyed by presentation time, never by frame count. Each scene
carries `first_pts` and `end_pts` (its last frame's pts plus duration) in the
source's time base, and serving finds a sample's scene by its pts: scene *i*
covers [first_ptsᵢ, first_ptsᵢ₊₁), and the last one runs to its `end_pts`.
`end_pts` need not equal the next scene's `first_pts` exactly (container
durations are rounded), so the next scene's start is the boundary.

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

As built (milestone 2; `tools/dv_processing/refit_mode.h`, arithmetic in
`refit_compose.h`):

```sh
segment_decode_render --refit-stats SOURCE VIDEO_INDEX [--frames FIRST LAST]
segment_decode_render --dump-composed SOURCE VIDEO_INDEX FIRST LAST
```

- **Output:** `--refit-stats` prints one `refit_scene` JSON line per pooled
  scene (`first_frame`, `frames`, `first_pts` and `end_pts` in `time_base`
  ticks, and `fit` and `held_out` in `RefitStats`' serde layout, whose
  `xtx_upper` is XᵀX's upper triangle), then a `refit_summary` line
  (`frames`, `scenes`, `rpu_scenes`, `median_scene_frames` of the RPU's own
  scenes before pooling, `degenerate` when that median is under 24,
  `fit_samples`, `held_out_samples`, `first_pts`, `end_pts`, `time_base`).
  Diagnostics go to stderr; exit 0 only after a complete read; any refusal is
  exit 1 and the output is void. plurx-core reads the lines as `RefitScene`
  and `RefitSummary` (each checked, 512 KB per line at most) and
  `check_refit_run` cross-checks a whole run.
  `--dump-composed` writes the lattice samples as binary records (format in
  `refit_mode.h`), which
  `diagnostics/fel_quality/compare_composed.py` checks against the frozen
  tools.
- **Composition:** each frame's own mapping plus the LINEAR_DZ residual
  sign(d)·((|d| − ½)·S + T)/2²³, rounded as floor(v·4096 + ½) and clipped,
  the way the helper's GPU renderer (libplacebo v7.387.0) composes the
  master: input code/2¹⁰ (UNORM16 of code ≪ 6 rescaled,
  `src/shaders/colorspace.c:349-352`); pivots code/2¹⁰
  (`utils/libav_internal.h:985`), so a piece is chosen by code ≥ pivot;
  every component clamped to its own pivot range before any piece runs,
  including where another component's MMR reads it (`colorspace.c:178`);
  each mapped value clamped to [0, 1] before the residual
  (`colorspace.c:326`) and the sum clamped again (`colorspace.c:147-150`).
  FFmpeg parses the mapping but does not reshape. Our code already matched
  this, so the frozen-tool checks are unchanged. The renderer evaluates in
  binary32; the helper in binary64 with fused multiply-adds off. RPUs go
  through the playback path's own admission (`map_fel_rpu`), FFmpeg's parse
  must agree with libdovi's on every pivot and coefficient, and the
  scene_refresh_flag comes from the frame's own DM data. Only a
  half-resolution EL is admitted (the frozen pairing covers no other).
- **Lattices:** stride 8 over the RPU's L5 active area (else the full
  frame); fit at phase 0, held out at phase 4 in x and y. The diagnostic
  `--area LEFT RIGHT TOP BOTTOM` replaces L5 to reproduce an offline region.
  The offline Michael5400 and Beekeeper fixtures used regions that are not
  their L5 areas (32 vs 42 rows; 272/275 vs 276/276), so their production
  statistics differ from the fixtures' by design; with `--area` they are the
  same samples.
- **Decoding:** software only, four frame threads per layer (the window
  path's reservation); at the end of the title both decoders are flushed and
  drained one frame each in turn. The source is read whole, as `scan_title`
  reads it, within four times its length plus 64 MiB, and at most 10⁶
  frames.
- **Media checks:** `diagnostics/fel_quality/compare_composed.py` (exact
  against the frozen tools) and `controls/test_refit_title_edits.py`, which
  edits a real title's RPUs with dovi_tool (an L5 change inside a scene, a
  scene cut on a reordered frame, an invalid RPU mid-title) and checks scene
  boundaries, lattice counts and the mid-title refusal.
  The playback window path is unchanged: the modes are dispatched before its
  arguments are parsed, and a window rendered before and after the change is
  byte-identical.

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

As built (milestone 3, `source_probe/refit_work.rs`,
`store/dv_refit_jobs.{rs,sql}`, `transcode/dv_refit_title.rs`):
- **FEL, not MEL.** No catalogue fact separates them: the title scan's
  aggregate does not record the EL type. Demand takes a Profile 7 title with
  an EL and a published complete title aggregate only when its probe
  document holds a FEL fact for the current source (a FEL recipe request, a
  recipe verdict or a legacy verdict). Only the FEL route writes those, and it
  plans only for a source whose RPUs parse as FEL. A helper refusal (nonzero
  exit, one stderr line) is stored as a `refused` title, so a title the
  helper will not fit is not decoded again until its identity changes.
- **Probe revision** is the title scan's demand generation
  (`dv_title_jobs_demand.revision`): it moves with a fresh probe, a source
  change and a scanner rebind, which also drop the aggregate the fit read.
- **Storage.** `background_dv_refit_titles` (one row per file) and
  `background_dv_refit_scenes`. A scene's two statistics sets are about
  42 KB of JSON (the recorded Michael5400 run), so a feature film of ~1,500
  scenes stores ~65 MB (an open question for review: see the milestone 3
  PR); one replicated write
  may not exceed the 16 MiB WAL entry. The attempt stages its scenes in
  pages of at most 4 MiB under its token, and publication adopts exactly
  those scenes, replaces the older fit and settles the job in one
  transaction. A trigger on the job deletes an attempt's staged scenes when
  it stops running or changes claim, so a cancel leaves no partial rows.
- **Parsing.** The job reads the helper's lines through `dv_refit`'s typed
  `RefitScene`/`RefitSummary` and `check_refit_run`, requires the run to
  start at frame 0, and stores each scene's `first_pts` and `end_pts`; it
  does not pool (the helper does). Output that breaks the contract fails the
  job with nothing stored; it is not retried, since the same bits through
  the same helper print the same thing.
- **Title verdict.** Held-out squared error is pooled over the scenes,
  counting identity for a refused scene; the gain is 1 − served/identity,
  and under 5% the title serves identity. A degenerate summary is identity
  for the whole title.

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

Decisions (2026-11-12, settled for milestone 4):
- **Pinned per session.** A session's curve digest is chosen once, at HLS
  create, from the reconciled plan's conversion and its platform's enable,
  and rides in its request. Every fragment it converts uses that table. A
  refit published mid-playback reaches only sessions started after it.
  A producer loads the table by the pinned digest. Milestone 3 keeps a
  superseded table for 7 days; when one is gone (only after a fit-version
  or helper change, since a source change already ends sessions) the
  session re-pins to the current fit at its next rendition.
- **v2 only for fitted titles.** A title without a fitted table keeps
  `dv-p7-to-p81-rpu-v1`, so its indexes and caches stay valid. A fitted
  title's copy is `dv-p7-to-p81-rpu-v2+<digest>`; the indexer builds it
  beside v1 whenever any platform's enable is on. Identity and bytes always
  agree: a v2 output whose table or anchor is missing is refused, never
  served with identity curves.
- **Enable, not gate.** `playback.dolby_vision_refit_{apple,web,android,linux}`,
  one per `playback_control::ClientPlatform`, default off (milestone 3
  registers them). At create the only platform signal is the capability
  document's `client.kind` (`web`, `ios`/`tvos`, `android`, `linux-player`);
  a session without one has no platform, so no enable applies. Off ⇒ v1.
- **Placed by presentation time, exactly.** A sample's source pts is its
  presentation time mapped through a clock anchored on the source's first
  video keyframe (ffprobe pts; a Matroska B-frame head has no dts), with
  rational arithmetic; a time that is not a whole source tick is refused.

As built (milestone 4):
- `fmp4::rewrite_timed_video_samples`, `transcode::dv_refit_serve`
  (`ServedCurveTable`, `SourceClock`, `SourceKeyframe`, `written_curves`),
  `dvconvert::convert_length_prefixed(…, curves, …)` and
  `dvpipe::Converter::{with_refit, with_curves, convert_placed,
  convert_anchored}`; `plurxd::dv_refit_serving` is the one adapter to the
  store and the settings.
- **Index pass** (`fragindex`): the v2 output anchors on the source's first
  keyframe and its first sample's presentation time.
- **VOD** (`vodgen`): a converting F+ generation does not convert before its
  landing. Its buffered fragments are converted once per candidate index
  row, placed by that row's `dts`, and matched on those bytes; each later
  fragment is converted at its own row. The rendition anchors its clock by
  re-running the index argv for one frame
  (`fragindex::index_first_presentation`). **Open:** the production A/V
  pipe's video timeline is not a shift of the video-only index's (with an
  audio lead the mov muxer shifts it and pins the first sample to zero;
  measured on the `clean-cra` fixture), so placement refuses and an F+
  rendition fails loudly. Exact VOD placement needs each sample's packet
  identity: the packet-clock engine (ruling A1) or per-row sample timing in
  the index.
- **Copy ranges** (`copy_normalization`, v3 analysis): each fragment is
  anchored by its own packet clock's first packet.
- **Identity curves (v1) stay** for the legacy live copy session
  (`copyseg`) and shared Source copies. Live copy gains F+ when it moves
  onto the one copy engine with the packet clock (Paul's 2026-11-07 ruling
  A1). A session moving from live copy to indexed VOD changes from identity
  to F+ curves at that switch.

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
2. **Helper `--refit-stats`:** built (branch `claude/fel-refit-helper`).
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
