# Display-targeted Dolby Vision rendering — build plan

**Status:** PR A remains draft and unmerged; A–F implementation and evidence are tracked on `effort/display-targeted-dv`, with the single main promotion still open · **Updated:** 2026-10-22 · **Owner:** direct GPT/root (PR #1216, integration and qualification) ·
**Extends:** item 1 of [DV-ENHANCEMENTS-NOT-YET-BUILT.md](DV-ENHANCEMENTS-NOT-YET-BUILT.md)

## PR A implementation checkpoint

User-confirmed delivery is one effort branch followed by one promotion PR to
`main`. PR #1216 is retargeted to `effort/display-targeted-dv`, remains a draft
and is unmerged. The current published source checkpoint is
`503619eb6744264b99d4ce7858f92a244911faf3`; it remains the source identity for
its compile and P7 evidence. This documentation-only update does not relabel
those results. A–F stage PRs target the effort branch. Before each stage merge,
run the manual Effort development gate with `compile_only=true`; it does not run
unit suites. Keep every focused unit and media result attached to its actual
source. After stages A–F are integrated, freeze task merges, reconcile the
effort with current `main`, and open one final promotion PR. Promotion requires
a passing Main promotion gate and a qualification receipt for that exact
candidate. The final PR and landing record must preserve A’s 36 `Regression-Test:` anchors
and the additional regression fields from stages B–F.

Two measured root causes drove the repairs. Bounded demux discovery returned
missing or transient origin fields, so the helper now consumes the exact
common origin already established by the held probe. The real FEL input then
exposed eight luma polynomial pieces and two chroma MMR curves, which the old
synthetic identity-only path refused. Production now uses the shared validated
public decoder-metadata projection, retaining coded-frame/RPU association,
NLQ constraints and source-derived operation counts. Original timestamps,
window bounds and the soundtrack offset stay intact.

A’s general-FEL source review is approved. Pinned formatting (#339), check
(#343), Clippy and core/daemon linkage (#334) passed; two core regressions
passed at #334 and the exact daemon regression passed at #352. Fixture-only
source repair `98cc1f4` did not change Rust production code. Retain #333 formatting
and #335/#337 compile failures as build failures with zero tests; they are not
unit outcomes. Helper CPU control #341 passed, bundle #342 contains 79 files and a 12-source renderer closure, and the zero control passed six frames. The real P7 helper run #345
exited 0 after 48 frames. Public ffprobe #350 confirms the original cadence:
source PTS 2085 plus duration 41 reaches 2126, followed by 2127. Checker-only
#351 passed against that exact output; it is not a GPU render or mapped-cell
pass. Full P7 driver #348 is running only the 1000-nit target; 1600 nits is
pending. No mapped P7 cell has passed. The recorded maximum `3441` is a PQ code,
not a nit measurement. No full mapped render or accuracy pass is inferred.

B–D and F have built source and retain their own checks without merge
qualification: B has seven Rust passes (#312); C has ten Rust passes (#316),
14 badge cases and 72 settings cases; D has 27 web cases, plus
integrated Android 30, Apple 13 and build checks. F code `6d48d4c` and docs
`67dbc9818` retain ten Rust passes (three at #324, seven at #331), two web
cases and ten pixel cases at #325: 61,440 samples, maximum error one code under
a two-code limit. These source-attributed results do not qualify a later
integrated candidate by themselves.

E’s reviewed adapter source `928407c` adds a separately acquired official
neutral-L3 reference (#349): 14 cases and 42 vendor commands, all exit 0. Its
narrow Python unit passed once at run354. The 14-case comparison mechanics at #353
completed against the neutral-L3 reference; baseline PQ error is 0.110077
(previous #332 was 0.146821), with field-0 positive/negative errors 0.44085 /
0.48964. Those observations are not creative-trim accuracy acceptance. The
public contract still lacks complete L8 support. The full FEL mapped path,
physical-device acceptance and deployment remain open.

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

**Bucket elision.** If the scanned title maximum is at or below the bucket,
the target would change nothing above black. In that case the server uses
the master recipe and reports "this title already fits a 1600-nit display".
L6 MaxCLL is not used: it is often 0, or copied from the base layer, so it
understates an FEL reconstruction. A test proves the elided render is
byte-identical to the master render.

**Eligibility per title, never per window.** A title whose scan shows
missing L1 on any frame is not targeted at all; it gets the master render
with the reason named. Valid L1 with max = 0 (black frames) is legitimate:
those frames use the title's mastering peak as their input max, by one
fixed rule. One title therefore never mixes master and targeted windows.

### Helper (phase 1)

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

### Phase 2: L2/L8 trims (approximation)

- For each shot, pick the L2 or L8 trim whose target display (L8 indexes the
  L10 target list) is nearest to the bucket, at or below it.
- Apply slope/offset/power as ST 2094-10 Annex B.3 describes, through a
  custom `pl_tone_map_function` whose LUT includes the trim. libplacebo
  caches the tone-map LUT keyed on its parameters, so a trim held only in
  helper state would let a shot change reuse the old curve. The trim must
  be part of the hashed parameters, for example through the function's
  constants or one function object per distinct trim. A test changes only
  the trim and asserts the output changes.
- Saturation and chroma-weight trims need a separate shader pass (in ICtCp).
- **Acceptance:**
  - Before claiming "trims applied", compare against a real DV display
    rendering of the same frames: the Razr or a DV TV, captured
    photometrically or with a capture device. Paul's devices, through a GPT
    prompt.
  - Until that comparison passes, the report keeps
    `creative_trims_applied: false` and the indicator says "L1 only".

### Phase 3: SDR target

- Bucket 100 with a BT.709 SDR export: BT.709 primaries, gamma 2.4, limited
  range.
- This is item 2 of the not-yet-built doc, and the TCL's case (it tone-maps
  to SDR today with a generic curve).
- It reuses phases 1 and 2. The new parts are the export matrix and the
  `ToneMappedSdr` → `ProcessedSdr` route in the indicator.

## Tests

### PR A executable qualification inventory (authored, not yet run)

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
- Run `python3 tools/dv_processing/controls/compare_display_target.py --shader
  "$CONTROL_BIN/display_target_shader" --output "$EVIDENCE/shader"`. Every Y/U/V
  code uses the independent Gaussian-anchor oracle (two-code bound): six buckets,
  L1 sensitivity, monotonic grayscale, zero black, peak, explicit edge extensions
  and saturated/out-of-range samples.
- Run `python3 tools/dv_processing/controls/run_display_target_windows.py
  --helper "$HELPER_BIN/segment_decode_render" --scanner "$HELPER_BIN/scan_title"
  --policy "$RUST_BIN/dv_target_policy" --fixtures "$RUST_BIN/dv_target_fixtures"
  --mux "$CONTROL_BIN/make_movie" --ffmpeg "$FFMPEG" --output "$EVIDENCE/windows"`.
  It encodes bounded P7/P8 sources; checks title accounting, final-frame missing
  L1, missing/corrupt RPU, inconsistent L6 and truncation; uses actual Rust policy
  for master elision; checks independent pixels, identical master/different L1
  targeted output and overlapping-window byte/hash identity. MaxCLL is misleading
  deliberately, never used to elide the high title.
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
| Core unit | bucket mapping, including elision; caps parsing of `min_nits`/`source` | mapping is total, rounds down, refuses junk |
| Core unit | `playback_enhancement` with `display_target` | DYN active only with a target, and the text names the nits |
| Daemon unit | two buckets give two `semantic_digest`s (via `DvExportPolicy`); elision gives the master digest | rendition identity |
| Daemon unit | MDCV/CLL writer: both SEI types and the `mdcv`/`clli` boxes carry the expected values (source primaries, MaxCLL = min(bucket, title max), MaxFALL 0) | metadata reaches both client paths |
| Daemon unit | `prepare_processed_hdr10` declines are named (no peak, elided, L1 missing) | Paul's "every decline is named" |
| Helper control | `compare_display_target.py`: render at master and at 1000. Check peak luma ≤ PQ(1000) + 1 code and a monotonic curve. **Identical pixels with two different L1 sets must give different outputs.** Every pixel must match an independent Python implementation of ST 2094-10 Annex B.2 (from the same L1) within 2 codes | the curve is the specified one and is driven by per-shot L1 (a static curve fails) |
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

| Stage | Contents | Recorded state |
|---|---|---|
| A | L1 scan, helper target policy, L1-to-CIE_Y mapping, ST 2094-10, recipe identity, processing report and controls | PR #1216 remains draft on `effort/display-targeted-dv`; source review is approved, general-FEL checks and exact regressions pass, while mapped P7 and accuracy qualification remain open. |
| B | Processed-output MDCV/CLL writer | Built; seven Rust cases passed at #312; not merged or promotion-qualified. |
| C | Peak capability, buckets, elision, Developer setting, indicator and Activity text | Built; source-attributed Rust, badge and settings cases pass; not merged or promotion-qualified. |
| D | Android / Android TV peak reporting and web setting | Built; source-attributed web, Android, Apple and build checks pass; not merged or promotion-qualified. |
| E | Phase 2 trim comparison | Neutral-L3 reference and 14-case numerical observations are retained; creative accuracy and complete L8 contract remain unaccepted. |
| F | Phase 3 SDR target | Built with source-attributed Rust, web and pixel checks; not merged or promotion-qualified. |

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
