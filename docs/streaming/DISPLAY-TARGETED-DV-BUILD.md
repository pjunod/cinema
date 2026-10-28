# Display-targeted Dolby Vision rendering — build plan

**Status:** A–D and F promoted to `main` through #1271 by Paul's decision of 2026-10-24. E (creative trims): closed-form Dolby display management merged through #1275; the served CM v4 L8 and CM v2.9 L2 trims (#1276, merged as 072d0f65 on 2026-10-25, not deployed) are accepted by pixel comparison against the reference on real titles, and playback reports `creative_trims_applied` per session; CM v2.9 near-black chroma follows the reference since #1278 (deca1e1e, not deployed); at the title's target black since #1281 (b9eb0466, not deployed) · **Updated:** 2026-10-25 · **Owner:** Claude (promotion #1271 and E; took over from direct GPT/root) ·
**Extends:** item 1 of [DV-ENHANCEMENTS-NOT-YET-BUILT.md](DV-ENHANCEMENTS-NOT-YET-BUILT.md)

Evidence remains bound to its recorded source. CM29 analytic gamut reconstruction and method v5 are built by Codex; adversarial review approved and native parity passed. [PR #1289](http://forge.lan:3000/noirr/plurx/pulls/1289) records final qualification and landing. The selected-frame and synthetic evidence is described below.

## Current qualification — source-bound evidence through F

| Phase | Retained evidence | Open for promotion |
|---|---|---|
| A · title scan/HDR10-E | #1216/#1269 integrated. Original P7 driver348 passed1000/1600; scanner405 passed40 gap cases plus controls at its original source. | Native459 P7/P8 HDR1000 PASS on91cbe; earlier evidence keeps its source. Current-candidate promotion remains. |
| B–D · metadata, controls, clients | Review398 closed production and harness findings. Focused Rust429 passed3 cases on4499; web425 passed221 checks. Apple434 and Android436 each passed their new identity case once. | Eight new Rust460 cases PASS on91cbe; physical-device acceptance remains separate. |
| E · trims | Reference349 completed42 vendor commands. Comparison353 fails baseline PQ.110077 and trim deltas.44085/.48964. GPU456/461: 4K mapping steady10.32fps. FIFO491: one continuous `cm_offline` process mapped96 frames at2560×1280, pixel-exact to TIFF488, second48 at27.62fps. Encode493 added P010+NVENC fragmented HEVC: both native exits0, two complete fragments, 0.963× real time end to end; the controller failed only on missing primaries/transfer tags (ffmpeg8.1 frame properties override encoder options for untagged RGB input; `setparams` fixes it). | Not part of #1271. 2026-10-25: the helper's mapping is now our own closed-form CM v4 / CM v2.9 display management (see “Helper (phase 2)”), measured privately against the vendor tool as a QC referee. Open: the CM v2.9 gamut stage and trim interpolation, and the acceptance decision. |
| F · SDR | P7/P8 native427 SDR100 passed on4499:17983 valid L1/zero holes each, four48-frame windows, FEL48/EL0 respectively. Earlier pixel325 retains max1code/limit2. | Valid-L6 SDR passes retained; unknown-L6 source-minimum focused460 cases PASS. |

Effort3cc264068 integrated#1216/#1269:78 regression fields, gate5889 8/8.
On4499, compiler428 and fast lane5904 (9/9) passed, retaining2044 Python
positives. Helper404 keeps its original a764 build attribution and13-input
adoption. Successful unchanged cases are not replayed.

Native418 rejected freshly published title facts because normalization omitted
the app-owned enrichment. Repair4499 excludes precisely that typed key while
retaining strict media comparison and title fences; both SDR retries passed.
Native427 then failed P7 HDR1000 before encoding because stream-only probing
missed full mastering metadata carried on decoded frames. P8 HDR was unstarted.
Diagnosis444 found full frame MDCV in both sources. The reviewed repair uses a
bounded selected-stream frame sidecar, sealed to the held descriptor, source and
probe identities. It leaves raw probe documents intact and resolves one target
before policy freeze. Unknown-L6 SDR consumes only the mastering minimum;
valid-L6 SDR and master remain unsampled. The same review398 closed both repair
findings445. Source91cbe passed Rust1.97.1 fmt/check/Clippy/link/list457 and all
eight focused460 cases once. Native459 P7/P8 HDR1000 each pass once, with
true queue/outer0,17983 valid L1/zero holes and four48-frame720p windows.
Only failed/unrun HDR cells ran; earlier SDR passes remain attributed to4499.

Paul decided on 2026-10-24 to ship what is done: A–D and F promote with E
unfinished, so playback reports `creative_trims_applied:false` / “L1 only”, and
E continues as a separate effort. Promotion runs the main fast lane once on the
candidate; only failed jobs rerun (Paul, 2026-10-24), and passes stay attributed
to their sources. Physical-display and full-app FEL EOF acceptance remain
outside bounded excerpt qualification.

## Stage implementation checkpoints

The PR B, PR C and PR A implementation checkpoints moved to
[qualification history](DISPLAY-TARGETED-DV-QUALIFICATION-HISTORY.md#stage-implementation-checkpoints-moved-from-the-build-plan)
on 2026-10-24; they are records, not current status.

### Historical qualification records

Earlier source checkpoints and qualification records are retained verbatim in
[qualification history](DISPLAY-TARGETED-DV-QUALIFICATION-HISTORY.md).

## Goal

A display that cannot decode Dolby Vision gets Dolby Vision–enhanced HDR10
(HDR10-E). Today HDR10-E includes the FEL and the RPU reconstruction, but it
**clips to the master grade**: the helper renders to a 10,000-nit PQ
container and leaves all tone mapping to the display. The display has only
static HDR10 metadata for that. One curve covers the whole film, and a dark
shot is mapped exactly like a bright one.

A Dolby Vision display does better because it reads the per-shot metadata
in the RPU:
- **L1:** the min, average and max PQ of each shot.
- **L2/L8:** the colourist's trims for target displays of known peak.

This plan does that mapping on the server, for the display the viewer
actually has. The server receives the display's peak brightness and renders
each shot with its own L1 (and, in phase 2, its trims) for that peak. That
**reduces** the mapping left to the display, though it does not remove it:
macOS always maps to the current EDR headroom, and TVs with dynamic tone
mapping ignore static metadata. The DYN pill becomes true for HDR10-E,
because the shot's dynamic metadata shaped the picture.

The first beneficiary is Chrome on a MacBook Pro / Studio Display XDR
(about 1600 nits peak, no DV decoder in Chrome). Others:
- non-DV Android phones (Pixel);
- HDR10 TVs;
- in phase 3, SDR displays, with a 100-nit target.

## Non-goals

- **No change for DV displays.** Native DV and P8.1 routes keep sending the
  RPU to the display, which applies it better than an approximation can.
- **No per-session one-off renders.** Targets are bucketed (below), so a
  render is shared by every viewer whose display falls in the same bucket.
- **Not Dolby's proprietary display management.** Phase 1 uses SMPTE ST
  2094-10 Annex B.2, the published L1-based curve. Phase 2's trims are an
  approximation, and the indicator says so.

## What exists today (main after #1202)

| Piece | Where | Today |
|---|---|---|
| Display peak in caps | `plurx-core/src/playback/caps.rs` `DisplayCaps.max_nits: Option<i64>` | Field exists; **no client sends it**, nothing reads it |
| Helper output policy | helper argv `bt2020-pq-master-clip`; `fel_renderer.h` / `base_dv_renderer.h` use `pl_tone_map_clip`, `PL_HDR_METADATA_NONE`, source and target `max_luma = 10000` | Master clip only |
| RPU parsing | helper reads `DoviVdrDmData` per frame (L1, L2, L5, L6, L8, L9, L11 are validated) | L1/L2/L8 are parsed and then **unused** |
| libplacebo dynamic metadata | `pl_hdr_metadata.max_pq_y` / `avg_pq_y` (`PL_HDR_METADATA_CIE_Y`, which is DV L1); `pl_tone_map_st2094_10` (Annex B.2; the B.3 gain/offset/gamma controls are explicitly not implemented) | Available in the bundled libplacebo (API 374), unused |
| Recipe identity | `DvExportPolicy::target_parameters()` (`dv_processing.rs`) hardcodes `bt2020-pq-master-clip-v1` / `-export-v3` and feeds `plan.semantic_digest()`; the helper argv is hardcoded in `dv_segment.rs`; `dv_runtime.rs` checks the graph name and `output_policy == "bt2020-pq-master-clip"` | The target must be **added** to the policy and digest, and both runtime checks widened. `argv_fingerprint` only hashes the ffmpeg argv, so nothing changes by itself |
| Effective-processing report | `DvOperation::TargetMapping`, `CreativeTrimApplication` | `TargetMapping` is reported for the master mapping; `creative_trims_applied: false` |
| Indicator | `plurx-core/src/playback/enhancement.rs` `EnhancementRoute::ProcessedHdr10` | DYN is always inactive: "HDR10 has no Dolby Vision dynamic metadata" |
| HDR10 static metadata in the output | `encoder.rs` notes MDCV/CLL is **not** synthesised; NUT raw frames carry no side data | Output has no MDCV/CLL SEI |
| HDR10 metadata writer | `plurx-core/src/fmp4/hdr10_index.rs`: `encode_sei` and the `mdcv`/`clli` init-box declarations used for copy segments | Reusable for processed output |
| Title-wide L1 statistics | none; nothing stores a per-file L1 maximum | Must be added (an RPU scan) |

## Design

### Target buckets

The server maps a reported peak to a bucket. The bucket, not the raw
number, becomes part of the recipe.

| Reported peak (nits) | Bucket |
|---|---|
| none / unknown / ≤ 0 | **master** (today's behaviour) |
| 1–399 | phase 3 SDR (100); until then, master |
| 400–599 | 400 |
| 600–999 | 600 |
| 1000–1599 | 1000 |
| 1600–1999 | 1600 |
| 2000–3999 | 2000 |
| 4000–4999 | 4000 |
| ≥ 5000 | master |

**Every range starts at its bucket value.** Rendering above the display's
peak brings back the display-side clipping this work exists to remove, so
the bucket never exceeds the reported peak.

**Title L1 scan.** A new per-file RPU scan (run with the existing DV scan,
stored next to the DV facts) records:
- the title maximum of L1 max;
- the frames whose L1 is missing or invalid;
- the L6/L9 mastering minimum and primaries.

**How the title scan runs.** [DV-TITLE-SCAN.md](DV-TITLE-SCAN.md) describes
the metadata-only parser, its concurrent read beside content analysis,
scanner-keyed demand and renderer-keyed FEL requalification.

**Bucket elision.** If the scanned title maximum is at or below the bucket,
the target would change nothing above black. In that case the server uses
the master recipe and reports "this title already fits a 1600-nit display".
L6 MaxCLL is not used: it is often 0, or copied from the base layer, so it
understates an FEL reconstruction. A test proves the elided render is
byte-identical to the master render.

**Eligibility per title, never per window.** A title whose scan shows
missing L1 on any frame is not targeted at all; it gets the master render
with the reason named. Valid L1 with max = 0 (black frames) is legitimate.
Phase 1 gave those frames the title's mastering peak as their input max;
phase 2 holds black, flat and clipped anchors one 12-bit code apart, so every
valid L1 maps. One title therefore never mixes master and targeted windows.

### Helper (phase 1, superseded on 2026-10-25 by phase 2 below)

The phase 1 curve below shipped in #1271 and is kept as the record of what
that release did. Its shader, Python oracle and controls were removed when
phase 2 replaced the curve.

- **New argument.** A new output policy value `bt2020-pq-target-<nits>`
  sits alongside `bt2020-pq-master-clip`. `<nits>` is one of the bucket
  values. Any other value is refused.
- **Actual method (accepted deviation, 2026-10-19):** preserve the existing
  native master reconstruction, including its 10,000-nit PQ container and all
  original DV operations. An explicit stateless custom shader then reads that
  native BT.2020/PQ RGB float texture, decodes PQ to linear RGB nits, applies the
  published B.2 rational curve per RGB component as in B.3 equation (7), clips
  to the target range, and encodes PQ. Existing bilinear/YUV export follows.
  There is no built-in `pl_tone_map_st2094_10`, CIE_Y renderer metadata execution,
  adaptive exposure, film/LMS blend, library patch or private LUT in this pass.
  Method identity: `annex-b2-rgb-fixed-floor-no-expansion-linear-edge-v1`.
  - Source peak and average are PQ EOTFs of L1 max/avg divided by 4095. A valid
    max0 uses the fixed title mastering peak (consistent L6, otherwise 4000).
  - Source and target floors are the same fixed title minimum (consistent L6,
    otherwise 0.005); actual samples at/below that floor are preserved, including
    exact zero black. Per-shot L1 min is not used as a rendering endpoint.
  - B.2 target average is `sqrt(source_average * sqrt(floor * target_peak))`,
    capped at `0.8 * target_peak` as suggested by the public standard.
  - Fit the three anchors using an affine normalization: `t=(x-floor)/(peak-floor)`,
    `u=(y-floor)/(target-floor)`, `u=t/(k*(1-t)+t)` and
    `k=t_avg*(1-u_avg)/(u_avg*(1-t_avg))`. Finite positive `k`, including its
    float uniform representation, proves no pole and nonnegative derivative on
    the actual interval. Dynamic public shader uniforms avoid per-frame pipelines.
  - **Versioned engineering extensions, not exact B.2 claims:** no inverse
    expansion when frame peak already fits; a degenerate average/adaptation or
    nonfinite/nonmonotone fit uses deterministic endpoint-linear compression.
    Each frame reports its actual curve/fallback reason and max0 provenance.
    Missing/invalid L1 is never an engineering-curve fallback.
  - Independent Python controls solve the defining unnormalized anchor equations
    by Gaussian elimination, not this coefficient expression or libplacebo code.
    Nondegenerate controls retain the two-code bound; edge controls separately
    disclose the extension, zero-black, peak and monotonicity requirements.
  - RGB-component mapping can change saturated-highlight hue. The coordinator's
    accepted RGB method supersedes the old clip-versus-perceptual choice for PR A:
    use clip, with saturated-highlight, negative and out-of-range controls. No
    inactive perceptual flag or second color-volume implementation is added.
    The old fire/neon comparison has NOT been performed; a perceptual experiment
    is separately specified future work. No hue-optimal, perceptual-equivalence
    or calibrated-accuracy claim.
- **Invalid L1 on a frame of an eligible title** cannot happen (the scan
  excluded such titles). If it does anyway, the window refuses and the
  daemon falls back to the master recipe for the **whole session**, with
  the reason named. Never per window.
- **Peak detection stays off.** The L1 values are the measured shot
  statistics, and peak detection would make the output depend on window
  boundaries, which breaks segment determinism.
- **Report.** Add `DvOperation::DisplayTargetMapping { nits }` to the
  effective-processing report. `TargetMapping` keeps meaning the master
  mapping.
- **Identity.**
  - `DvExportPolicy` gains the target, with a new versioned
    `target_parameters()` string (`bt2020-pq-target-v1` plus the nits), so
    `semantic_digest()` differs per bucket.
  - `dv_segment.rs` passes the policy instead of the hardcoded string.
  - `dv_runtime.rs` accepts the new graph and `output_policy` values.

### Output HDR10 metadata (phase 1, required)

A render for 1600 nits that carries no mastering metadata may be tone-mapped
**again** by a display that assumes a 4000- or 10,000-nit master. The
metadata has to reach clients in two places:
- the `mdcv`/`clli` boxes in the init segment, which Chrome's MSE path and
  AVPlayer read;
- prefix SEI on each IRAP, which TVs and hardware decoders read.

**Path: post-encode, in the daemon.** Reuse `fmp4/hdr10_index.rs`
(`encode_sei` and the `mdcv`/`clli` declarations already written for copy
segments). This works with every encoder. The other options are ruled out:
- NUT stream side data is very likely not written by the NUT muxer.
- `hevc_vaapi` writes HDR SEI only on frames that carry side data, which
  raw NUT frames never do.
- A Matroska intermediate is plausible but couples the fix to one ffmpeg
  behaviour.

**Values:**
- **Mastering primaries:** the source's (L9, usually P3-D65), not BT.2020.
  Some TVs gamut-compress a BT.2020 mastering declaration.
- **Mastering luminance:** max = bucket; min = the title's mastering
  minimum.
- **MaxCLL:** min(bucket, scanned title L1 max).
- **MaxFALL:** 0 (unknown). A PQ average is not FALL, and inventing one is
  worse than declaring none.

**Verification:**
- `ffprobe -show_frames -read_intervals %+#1` shows both SEI types on a
  segment.
- The init segment's `mdcv`/`clli` boxes decode to the same values.
- A Rust test covers both.

### Server (phase 1)

- **Caps:** `DisplayCaps.max_nits` gains a `peak_source` enum: `platform`
  (OS-reported) or `viewer` (chosen in settings). Both feed the bucket the
  same way; the source changes only the indicator and Activity text.
  `min_nits` is deliberately **not** collected in phase 1: the black level
  is the title's, and a per-display minimum would split the cache for no
  visible gain.
- **Plan:**
  - `prepare_processed_hdr10` picks the bucket from
    `original.canonical_caps.display`, applies bucket elision, and passes
    the policy to the trial.
  - Every decline is named: "your display did not report its peak, so the
    master grade is sent", or "this title already fits a 1600-nit display".
- **Developer setting:** "Display-targeted HDR10-E" (off by default), with
  its own enable section. Each row is advisory and never blocks the toggle
  (no code gates, per Paul's rule):
  - the processing helper is installed and HDR10-E is on;
  - at least one client reports a peak;
  - the title L1 scan has run, and how many titles are eligible.

  The MDCV/CLL writer lands in PR B, **before** the setting exists (PR C).
  So no build ever offers targeting without it, and no gate is needed.
- **Indicator:** `EnhancementRoute::ProcessedHdr10` gains
  `display_target: Option<u32>`. With a target, DYN is active:
  "DYN: per-shot L1 metadata mapped for a 1600-nit display by the server".
  Without one, the existing text stays.
- **Cache and rendition identity:** handled by `argv_fingerprint`. Add a
  test that two buckets give two digests and that bucket elision gives the
  master digest.
- **Hardware attributability:** the session's Activity row shows the
  reported peak, its source and the chosen bucket, for example "HDR10-E,
  mapped for 1600 nits (display reported 1600, from Android)". Settings →
  Developer shows cache bytes per bucket (Paul's rule: GPU work must be
  attributable in the product).
- **Prepared successors and Auto rungs:** a successor inherits its
  session's bucket. Pre-encodes and VOD caches stay master-only, so the
  first targeted view of a title renders live, and that is said in Activity.
  At most 2 buckets per title are cached (least recently used is evicted),
  so Auto rungs × buckets cannot multiply without limit.

### Clients (phase 1)

| Client | Peak source | Notes |
|---|---|---|
| Android / Android TV | `Display.getHdrCapabilities().desiredMaxLuminance` | This is the EDID "desired content max": often 0, sometimes a marketing number. Treat ≤ 0 or ≥ 5000 as unknown. The viewer can override it in settings. Re-send on display change (HDMI hot-plug on TV boxes). |
| Web | Settings → Playback → "HDR display peak": Auto (unknown) / 400 / 600 / 1000 / 1600 / 2000 / 4000 | Browsers expose `(dynamic-range: high)` but no nits. The setting includes a PQ step-wedge pattern ("pick the highest step you can still tell apart"), because nobody knows their display's nits, and picking too high quietly brings clipping back. Stored per browser (localStorage is fine for this per-viewer convenience) and sent as `peak_source: viewer`. For a Pro/Studio Display XDR the suggested value is **1000** (sustained full-field), not the 1600 peak. |
| Apple | not in phase 1 | EDR headroom × reference white has no fixed answer: reference white moves with the brightness slider. Apple clients take the native DV route anyway. |

### Phase 2: closed-form Dolby display management (L1, L3, L2, L8)

Phase 2 replaces the phase 1 curve with closed-form equations of Dolby's own
display management, derived and checked privately against the licensed
reference renderer (`cm_offline`, used only as a QC referee). Derivations and
evidence stay on the private `rog_hdr_dv` branch (its `CM4_CLOSED_FORM.md` and
`CM29_CLOSED_FORM.md`); this repository carries only the equations as code.

- **Family per frame, from the RPU.** CM v4 when the RPU carries CM v4.0
  metadata (L254) or L8 trims; CM v2.9 otherwise. Rust re-derives the family
  from the raw RPU and refuses a trace row that names another
  (`DvDisplayTarget::display_management`).
- **CM v4.** Effective L1 = L1 + L3 offsets, anchors held one 12-bit code
  apart. Two-stage tone curve, SOP in target-normalised PQ, colour in ICtCp
  (primary saturation, mesopic chroma, six-vector), MSWeight detail stage at
  weight 0, clip in the target primaries. L8 trims (preset target displays,
  or L10) are interpolated to the bucket peak: SOP linearly in PQ, colour
  trims linearly in a per-gamut display size V = K_G + A_G·|c_G − PQ|^2.027
  (so a BT.709 trim weighs less on a P3 display of the same peak), held for
  displays smaller than the smallest trim target and faded to neutral at the
  mastering display (L9 gamut, title peak). An L8 whose target
  display can be resolved neither from the preset table nor from L10 is not
  used.
- **CM v2.9.** Rational three-anchor curve, L2 SOP in absolute PQ, IPT-PQ
  colour, MSWeight detail on intensity (and on chroma when the chroma weight
  is non-zero). L2 trims: the trim authored for the bucket's peak; otherwise
  each decoded trim field (lift, gain, gamma, chroma weight, saturation gain,
  MS weight) linear in target PQ between the bracketing L2 targets, with
  neutral trims at PQ 0 and at the mastering display. At or above the
  mastering peak both families map untrimmed. The frame report names the
  selection (`cm29-l2-exact`, `cm29-l2-interpolated`, `cm29-l1-only`).
- **Targets.** HDR buckets: a P3-D65 display of the bucket peak inside the
  BT.2020/PQ container. SDR: a 100-nit BT.709 display. Target black is the
  fixed title minimum; trims fade at the fixed title mastering peak.
- **Code.** `tools/dv_processing/dm_closed_form.h` is the double-precision
  reference of the equations. `dv_dm.h` prepares each frame's state on the CPU
  from the RPU and runs three GPU passes (pointwise map; horizontal and
  vertical Gaussian / derivative-of-Gaussian detail; finish and gamut clip).
  `display_target.h` calls it and prints one `display_target_frame` row per
  frame (`curve`, `trim_targets`, `detail_weight`). Method identity:
  `dolby-closed-form-cm4-cm29-v5` (CM4 unchanged; CM29 family v4). Earlier
  versions served no trims (v1), added a fitted CM29 near-black gain (v3),
  and applied it at absolute output intensity (v4). The helper graph remains
  `dv-native-master-or-closed-form-dm-v1`.
- **CM4 accuracy (retained private evidence, 2026-10-25).** The unchanged
  model reproduces 30/30 random30 shots (median mean ΔE_ITP 0.000, maximum
  0.055) and real P8 frames with original L8 metadata (mean ≤0.001 at
  300–1600 nits, ≤0.007 at 100 nits). This evidence retains its original source.
- **CM29 analytic gamut reconstruction (Codex, 2026-10-25).** The former
  intensity-only gain could not represent the sign, hue and chroma dependence
  of interpolation near black. The model now uses standard IPT D65 in both
  directions, colorfulness compensation
  `S = clamp((Ib/I)*(2+I)/(2+Ib), 0.5, 2)` before the creative saturation trim,
  and the eight analytic corners of a trilinear IPT gamut grid. The intervals
  are target PQ peak divided by 64 for intensity and by 32 for each opponent
  axis. A corner is computed from inverse IPT, PQ EOTF, the target matrix and
  target cube, then encoded in the target transfer function. No measured
  output tables or per-title corrections are used; the old gain tables are
  removed. The colorfulness equation agrees with
  [US20160005349A1 equations 11–14](https://patents.google.com/patent/US20160005349A1/en).
- **Native black and encoding.** The HDR gamut cube has a zero-intensity
  black plane; title black shapes its tone curve. The stock 100-nit BT.1886
  cube has nominal black 0.005 nits, including its intensity origin and vertex
  encoding. The resolved title minimum shapes the tone curve and the final
  BT.1886 decoding independently. Confusing those two blacks causes a neutral
  intensity error before any hue correction. The CPU reference and shader
  share the prepared matrices and these parameters.
- **Evidence and limits.** The frozen analytic candidate improves every
  available selected-frame comparison across four title clips, including
  Smile's 0.01–0.1-nit source band. Same-peak target renders refused by the
  reference tool are explicit unavailable cells. Independent controls cover
  two L1 sets, three HDR peaks, BT.2020, and SDR black changes, with original
  title metadata preserved. Raw references, aggregate metrics, hashes and
  reproduction sources stay in the private research ledger
  `CM29_CLOSED_FORM.md`, branch `private/cm29-cause-20261025`.
  Gamut-corner differences and isolated spatial maxima remain; these are
  selected-frame pixel comparisons, not full-movie or physical-display
  qualification. Final review, shader parity and merge qualification are
  recorded with the landing PR. Paul deploys; this change is not deployed.
- **Served trims are on.** `DV_TARGET_TRIMS_CM4` / `DV_TARGET_TRIMS_CM29`
  (helper) and `DISPLAY_TARGET_CREATIVE_TRIMS_CM4` / `_CM29` (Rust) are
  true. A frame whose mapping used a trim reports `CreativeTrimApplication`.
  The effective report's `creative_trims_applied` is true when every served
  frame did and `creative_trims_partial` when some did; Playback info says
  “creative trims applied”, “creative trims on some shots” or “L1 only”.
- **Acceptance (Paul, 2026-10-25): pixel comparison against the reference
  on real frames.** The whole helper (decode, RPU, GPU passes, 16-bit
  export) on 4-second clips, four frames per bucket, against `cm_offline`
  with the same RPU; mean ΔE_ITP per frame:

  | Title | Family | 400 | 600 | 1000 | 1600 | 2000 |
  |---|---|---|---|---|---|---|
  | Little Brother | CM v4, P8 | 0.011 | | | 0.005 | |
  | Last Breath | CM v4, P8 | 0.007 | | 0.009 | 0.009 | |
  | Companion | CM v4, P8 | 0.009 | | master | 0.005 | |
  | The Beekeeper | CM v4, P7 | 0.008 | | master | 0.007 | |
  | Dumb Money | CM v2.9, P8 | | 0.27 | master | | 0.46 |
  | Smile | CM v2.9, P7 | | 0.32 | master | | 0.34 |
  | The Godfather Part II | CM v2.9, P7 | | 0.14–0.22 | 0.17–0.25 | | 0.19–0.28 |
  | The Sound of Music | CM v2.9, P7 | | 0.38–0.41 | 0.34–0.36 | | 0.26–0.27 |

  “master”: the bucket is the mastering peak, which the reference refuses
  and plurx elides. 2000 nits is interpolated for the 4000-nit titles and
  above the peak (untrimmed) for the 1000-nit ones. CM v4 frames stay ≤ 0.08 everywhere; CM v2.9 p95 is
  ≤ 3.0 with isolated pixels up to 13 (the approximate gamut stage). The
  untrimmed mapping served before (v1) is 4.9–14 from the reference on the
  same CM v2.9 frames, and “nearest lower” L2 selection was 5.4 at 2000 nits
  on Godfather II.
- **CM v2.9 interpolation, measured.** `cm_offline` renders CM v2.9 only to
  its stock targets (100, 600, 1000, 2000 nits), so 400 and 1600 can't be
  referenced directly. The rule was measured by removing a trim and finding
  the explicit trim at that target that reproduces the reference: linear in
  target PQ reproduces it (2000 between the 1000 trim and the 4000-nit
  master: 0.004; 600 between the 100 and 1000 trims: 0.003; 1000 between
  the 600 trim and the master: 0.022); linear in nits does not (1.7).
  Below the lowest target the trim fades to neutral at PQ 0 (0.019; holding
  it is 0.74). 400 and 1600 use the same rule.

### Phase 3: SDR target

- Bucket 100 with a BT.709 SDR export: BT.709 primaries, gamma 2.4, limited
  range.
- This is item 2 of the not-yet-built doc, and the TCL's case (it tone-maps
  to SDR today with a generic curve).
- It reuses phases 1 and 2. The new parts are the export matrix and the
  `ToneMappedSdr` → `ProcessedSdr` route in the indicator.

## Tests

### PR A qualification inventory (definitions; outcomes above)

The coordinator runs these only after complete independent review and fixes.
Use native Linux, pinned Rust 1.97.1 and the verified API 374 dependency image.
Do not use historical ARM scripts or reuse an output directory. Every receipt
binds source archive, binary/dependency hashes, exit status and nonzero matched
cases/samples. A compile is not a run.

- Core units: title eligibility/old JSON, exact bucket/elision, and
  `display_target_semantic_digest_separates_all_buckets_and_title_constants`.
- Daemon units: `display_target_trace_binds_exact_bucket_endpoints_and_raw_l1`
  and `title_scan_selects_the_catalogue_video_not_an_attached_cover`.
- Build qualification adapters with `cargo build --locked -p plurx-core --example
  dv_target_policy --example dv_target_fixtures`. They use the real pure policy
  and public RPU writer, not a playback route or feature toggle.
- Build controls with `sh tools/dv_processing/controls/build_target_controls.sh`
  and the same four explicit dependency/output arguments as the helper build.
- Phase 2 (2026-10-25) replaced the phase 1 controls. Run
  `"$CONTROL_BIN/test_dv_dm" tools/dv_processing/controls/tags`: the retained
  P7 fixture and two committed trim fixtures (`cm4-l8-trims.nal`,
  `cm29-l2-trims.nal`, written by the Rust selection test) go through the real
  parser and must give the selections the Rust tests pin, with trims on and as
  served (C and Rust agree on the same bytes); family and trim selection
  from the RPU, black/flat/clipped L1 and nonsensical trim codes, every
  bucket finite and inside the target, monotone neutral grey axis, L1
  sensitivity, trims that act at their target and fade out at the mastering
  peak, pinned golden values of the model. Then run
  `python3 tools/dv_processing/controls/run_dm_parity.py
  "$CONTROL_BIN/dm_shader_control" "$EVIDENCE/parity" "$FRAMES_F32"`: the GPU
  shader against the CPU reference on real-frame crops (ΔE_ITP max 0.05,
  2e-4 PQ) and a saturated synthetic lattice (mean 0.02, max 0.5).
- Run `python3 tools/dv_processing/controls/run_display_target_windows.py
  --helper "$HELPER_BIN/segment_decode_render" --scanner "$HELPER_BIN/scan_title"
  --policy "$RUST_BIN/dv_target_policy" --fixtures "$RUST_BIN/dv_target_fixtures"
  --mux "$CONTROL_BIN/make_movie" --ffmpeg "$FFMPEG" --output "$EVIDENCE/windows"`.
  It encodes bounded P7/P8 sources; checks title accounting, final-frame missing
  L1, missing/corrupt RPU, inconsistent L6 and truncation; uses actual Rust policy
  for master elision; checks the per-frame trace (raw L1, method, family), the
  target peak, different output for identical master pixels with different L1,
  and overlapping-window byte/hash identity. Phase 2 removed only its Annex B.2
  pixel oracle. MaxCLL is misleading deliberately, never used to elide the high
  title.
- Source changes during either probe invalidate publication under the existing
  admitted probe/source-revision fence. Fresh scans replace rather than merge
  title objects. Existing source-fence/probe-publication regressions remain
  affected final-batch coverage.

The programme-wide table below also includes later metadata/routing/client
phases. PR A does not write MDCV/CLL, offer a setting or establish real-title/device
acceptance. Runtime-invalid L1 refuses the private window with a whole-session
master reason; later session selection owns the whole-session transition, never
a per-window target/master mix.

| Level | Test | Proves |
|---|---|---|
| Core unit | bucket mapping, including elision; caps parsing of `max_nits`/`peak_source` | mapping is total, rounds down, refuses junk |
| Core unit | `playback_enhancement` with `display_target` | DYN active only with a target, and the text names the nits |
| Daemon unit | two buckets give two `semantic_digest`s (via `DvExportPolicy`); elision gives the master digest | rendition identity |
| Daemon unit | MDCV/CLL writer: both SEI types and the `mdcv`/`clli` boxes carry the expected values (source primaries, MaxCLL = min(bucket, title max), MaxFALL 0) | metadata reaches both client paths |
| Daemon unit | `prepare_processed_hdr10` declines are named (no peak, elided, L1 missing) | Paul's "every decline is named" |
| Helper control | `test_dv_dm` (phase 2; replaced phase 1's `compare_display_target.py`): every bucket stays ≤ PQ(target) + 1 code with a monotonic grey axis. **Identical pixels with two different L1 sets must give different outputs.** Trims change the output at their target and vanish at the mastering peak; golden values pin the model | the mapping is driven by per-shot L1 and trims (a static curve fails) |
| Helper control | `run_dm_parity.py`: GPU shader against the double-precision reference | the shader computes the equations |
| Helper control | render two overlapping windows; the shared frames' `yuv10_sha256` must be identical | segment determinism across window boundaries |
| Helper control | an elided title rendered "targeted" is byte-identical to its master render | elision is truthful |
| Helper unit | `contrast_recovery` and `hybrid_mix` are 0 in the targeted params | no hidden libplacebo defaults |
| Physical (ignored) | `actual_processed_hdr_windows…` with `PLURX_DV_WINDOW_TARGET_NITS=1000` and 1600, P7 FEL and P8 | real titles, real GPU, MDCV/CLL present in output |
| Device (GPT prompt) | Chrome on the M3 + XDR with the web setting at 1600; Pixel with automatic peak | DYN pill, no double tone mapping, by eye on a bright highlight scene |

**Proving the tests catch regressions:** each new unit test fails against
the old code (no `display_target` field, master digest for every bucket).
The control fails if the helper ignores the target (peak above PQ(1000)).

## Cost and performance

- **GPU:** an additional native float shader pass (PQ decode, rational RGB curve,
  PQ encode) precedes existing export. The old one-LUT/under-1-ms expectation is
  superseded, not measured. Source-bound `DVT` evidence is needed for throughput.
- **Cache:** one rendition per bucket in use. Elision covers titles whose
  content already fits.
- **First-window qualification:** unchanged, one trial per start.

## Delivery

#1216/#1269 are integrated into the effort at3cc264068, preserving all78
original regression fields. The [current matrix](#current-qualification--source-bound-evidence-through-f)
owns source-bound evidence. Paul's 2026-10-24 decision promotes A–D and F
with E unfinished; E continues as its own effort.

Review398 closed all four production findings and the qualification-fixture
follow-up at a7642100: actual SDR grade/Main10 facts, L6/L9 contradictions across
gaps, native SDR identity and activity100 bounds. Pinned Rust403 fmt/check/Clippy
pass; new scanner regression405 passes40 gap cases plus controls. Other changed
cases and current native HDR/SDR adoption remain unrun. The bounded native harness
uses real title policy, claimed-job publication, fresh durable admission and
normal lease/source checks; it does not prove HTTP/full-app EOF. The old two-case
reuse test remains; a separate SDR-only case avoids repeating valid cases.
Both scanner digests include the accumulator; binding rotation clears old facts.
Two formal producer hashes refresh only reviewed source identity; fences unchanged.
Promotion qualification is the main fast lane, once, on the exact candidate.

### Stage PRs target one effort branch

Open or retarget each A–F stage PR to `effort/display-targeted-dv`. Before
merging a stage into the effort, dispatch the manual Effort development gate
with `compile_only=true`. That gate checks policy, formatting, static web
contracts and affected compilation; it runs no Rust, Apple or Android unit
suites. Preserve focused regressions and native/media receipts with their exact
source identities, and do not treat the compile-only result as unit or device
qualification.

### One reviewed promotion moves the effort to main

When all stage merges are frozen, merge current `main` into the effort, resolve
and review the resulting candidate, and open one promotion PR from the effort
to `main`. Run the Main promotion gate and create the required qualification
receipt against that exact candidate. If either branch moves, qualify the new
tree again. Carry A’s 36 `Regression-Test:` anchors and the additional B–F regression
fields through the promotion PR description and landing message. Do not publish or deploy from a task-stage PR.

## Risks

- **Double tone mapping where metadata is ignored or the display maps
  anyway** (macOS EDR, TVs with dynamic tone mapping). The MDCV/CLL writer
  (PR B) lands before the setting (PR C), so every targeted render carries
  it. What remains is measured on the device check and stated in the
  indicator text, not hidden.
- **Wrong peak reports** (Android TVs that report 0 or 10,000). Values
  ≤ 0 or ≥ 5000 count as unknown (master), and the viewer can override in
  settings.
- **ST 2094-10 is not Dolby's display management.** Expect a close match on
  L1-driven brightness and a looser one on highlights with heavy trims until
  phase 2. The indicator says "L1" to make that clear.

## Review

An adversarial review on 2026-10-19 found 14 issues, and the plan above
incorporates all of them:
- identity does not follow from `argv_fingerprint`;
- bucket ranges rounded up;
- elision trusted L6;
- black-level pumping;
- per-window L1 refusal broke consistency;
- hidden libplacebo defaults;
- the wrong MDCV/CLL path, primaries, MaxCLL and FALL values;
- the trim LUT cache;
- tests a static curve would pass;
- a code gate;
- what reported peaks really mean;
- cache and successor interaction;
- web UX;
- gamut clipping.
