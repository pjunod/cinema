# FEL RPU re-fit (F+) — build plan

**Status:** open — milestones 1–2 merged; 3–6 implemented in PR #1565,
final review repaired, qualification pending; 7 remains a follow-up after served
F+ scores · **Updated:** 2026-11-13 · **Written:** 2026-11-12 ·
**Owner:** Codex (continuing Claude) · **Decision:** Paul approved building F+
and per-title re-encode trials against it, 2026-11-12

Companion to the offline result,
[RPU-REFIT.md](../../tools/dv_processing/diagnostics/fel_quality/RPU-REFIT.md),
and the investigation ledger,
[FEL-PICTURE-QUALITY-INVESTIGATION.md](FEL-PICTURE-QUALITY-INVESTIGATION.md).
This file says how F+ gets into the product. Work milestone by milestone
(§6). If a step seems to need a change to the processed FEL→8.1 route or to
a decoder or client protocol signal, stop and flag it: F+ changes RPU bytes
on the server. Developer controls and playback-info presentation are part of
§4 and §6, without changing how clients decode or identify their platform.

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
inside the product). Request identity hashes the source, the selected
video stream, the helper's refit identity and `dv_refit::FIT_VERSION` (the
probe revision was dropped from it in review; see below).

As built (milestone 3, `source_probe/{refit_work,dv_refit}.rs`,
`store/dv_refit_jobs.{rs,sql}`, `transcode/dv_refit_title.rs`; reworked after
review of PR #1565):
- **Switches.** Four Developer switches, one per
  `playback_control::ClientPlatform`: `playback.dolby_vision_refit_web`,
  `_apple`, `_android`, `_linux`, default off. Demand runs only while one is
  on; copy-path serving (milestone 4) reads each. The card's rows are
  advisory: whether this node's helper supports `--refit-stats`, N of M
  Profile 7 FEL titles fitted, the spool's size, and the measured clients
  line ("Apple app on macOS applies the 8.1 mapping (AVPlayerLayer); Lenovo
  TB322FC (Android) ignores it; iOS, tvOS, Google TV, web unobservable").
- **FEL, not MEL.** No catalogue fact separates them: the title scan's
  aggregate does not record the EL type. Demand takes a Profile 7 title with
  an EL and a published complete title aggregate only when its probe
  document holds a FEL fact for the current source (a FEL recipe request, a
  recipe verdict or a legacy verdict). Only the FEL route writes those, so a
  FEL title the FEL route never planned for is not fitted. That coverage gap
  is a follow-up: milestone 5 records the EL kind with the title facts.
- **Identity** (the job's dedupe key) is the source (file, size, mtime), the
  selected video stream, the helper's refit identity and `FIT_VERSION`. The
  refit identity is `REFIT_MODE_VERSION` (bumped when `refit_mode.h` or
  `refit_compose.h` change what a run prints; a test pins their sha256) and
  the decoder ABI (the bundle's avcodec/avformat/avutil majors), not the
  whole backend digest, so rebuilding the bundle for an unrelated change does
  not decode every FEL title again. The complete title aggregate is a demand
  precondition, not part of the identity. The title-scan generation
  (`dv_title_jobs_demand.revision`) is recorded; when a newer generation
  selects the same stream of the same source, the stored table (or terminal
  outcome) is rebound to it instead of fitted again.
- **Storage (one replicated write).** Statistics are never replicated.
  `background_dv_refit_titles` (keyed by file and curve digest) holds the
  title row; `background_dv_refit_scenes` each scene's first frame, frames,
  `first_pts`/`end_pts`/time base, the served coefficients as the numerators
  over 2^23 `apply_to_rpu` writes (identity: NULL), why a scene keeps
  identity, and its held-out identity error, fitted error and gain. Error
  numbers and the title's gain are stored as Rust's shortest round-trip
  decimal text (`{:e}`), so they read back bit for bit; serde_json's
  `float_roundtrip` is no longer used anywhere. A scene is about 1.2 KB with
  curves; a publication is bounded at 12 MiB (some 10,000 fitted scenes), and
  a title over it fails rather than splitting. The publication checks its
  own rows (contiguity, the digest, every served table loading back through
  `ServableCurves::from_quantised`, finite numbers, the bound) before it is
  sent; a failure there settles Fail.
- **Tables by digest.** Publication makes the new table current and the
  file's previous one superseded, in the same transaction; a superseded table
  stays addressable by `dv_refit_curves_by_digest(file_id, digest)` for seven
  days after supersession, then publication or the idle pass's prune drops
  it. `dv_refit_curves(file_id)` reads the current one. Both read with the
  authority's consistency (a follower's local copy may still hold a table
  publication has superseded) and require the catalogue row to still name the
  table's source. A changed source (size or mtime) drops every table and
  outcome of the file and supersedes its jobs (`background_dv_refit_source_change`).
- **The spool.** The helper's stdout is written into a node-local spool
  (`<runtime cache>/dv-refit`), keyed by the source, the stream and the refit
  identity: `<key>.jsonl.gz` once a run completed, `<key>.partial` for the
  whole scene lines of interrupted runs. A `FIT_VERSION` bump re-solves from
  a complete spool without decoding; without one it decodes again. The
  Developer card lists it (`GET /api/v1/developer/dv-refit/spool`) and deletes
  it (`DELETE` on the same path).
- **Resume and coexistence.** The helper resumes at a pooled scene's start
  (`--resume FIRST FIRST_PTS`: it seeks to the key frame before it and
  composes from there, its summary covering only that run). The job resumes
  at the start of the last whole scene line the spool kept and joins the
  resumed scenes to the ones before them; `dv_refit::summarise` rebuilds the
  whole title's summary from the scenes' `rpu_scene_frames`. The helper's own
  control (`controls/test_refit_resume.py`) shows the resumed scenes equal an
  uninterrupted run's at every boundary of the real clip (resuming at frame
  286 of 311 takes 1.1 s against 13.1 s). The helper runs with `--threads 2`
  and the job reserves exactly 3 CPU threads (measured on lab3: 1 thread 39 s
  at 1.0 core, 2 threads 21.9 s at 2.2 cores, 3 threads 16 s at 3.2 cores, 4
  threads no faster). It is admitted beside live sessions when that fits the
  software budget and no viewer waits, and yields only when a viewer or an
  offline download waits for capacity, or to a media probe. A yield keeps the
  spool and requeues the same identity; a yield that kept no new scene counts
  toward a bound of 8 in a row, after which it settles as a retry.
- **Classification.** The helper exits 3 for a content refusal (not FEL, a
  full-resolution or half-resolution EL that is not FEL-composable, the active
  area, the stream) and 1 for any other refusal (I/O, allocation, decoder,
  arguments); libav logging is quiet. The job reads only the exit status:
  exit 3 stores a `refused` title with the last stderr line as its reason;
  any other exit, a signal or a spawn error retries, resuming from the spool.
- **Terminal outcomes.** A failed (after its retries) or cancelled identity
  is recorded in `background_dv_refit_outcomes`, which outlives the job row,
  so retention pruning job history does not demand the title again. A new
  job for the identity (Retry on the jobs page) lifts it. Deterministic
  failures (malformed output, a solve failure, rows that fail their check)
  settle Fail; publication store errors retry with backoff while the lease
  holds; a fenced publication settles nothing.
- **Parsing and solving** run off the async runtime. The title verdict is as
  before: held-out squared error pooled over the scenes, counting identity for
  a refused scene; gain 1 − served/identity; under 5% the title serves
  identity; a degenerate summary is identity for the whole title. The curve
  digest is sha256 over each scene's span and served integers.
- **Acceptance.** Two real helper runs over the Michael5400 window print the
  same bytes and the same digest
  (`2a8d78f7cf05fff325afe70c4f5ce2d1972317027cd0bdb045202d16310cb75b`,
  pinned by `dv_refit_title` tests against the recorded run).

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

The job also records each scene's held-out squared code-error reduction
for F+ relative to identity on its lattice, and the title total is shown
in the UI. A title whose total gain is below 5% keeps identity curves
everywhere, because a negligible gain is not worth new bytes in every cache.

### 3.5 Serving: the copy-path converter

- `dvconvert::convert_length_prefixed` gains a curve argument. After `To81`
  it calls `apply_to_rpu`.
- `dvpipe::Converter` resolves each sample's scene from its presentation
  time (the fragment's `tfdt` and `trun` cts) against the stored scene table,
  which keys scenes by their first presentation time. It never uses a
  running count, because a seek starts mid-title.
- A fitted output uses `dv-p7-to-p81-rpu-v2` and its curve digest in the
  fragment-index identity. The v1 output stays alongside it for disabled
  platforms and titles without a fit. Live `copyseg` stays on v1 until it
  shares the indexed copy engine; no fitted scene is selected from a live
  session's floating-point clock.
- The indexed copy path (`fragindex.rs`, `vodgen.rs`) carries the fitted table.
  Its source packet DTS/PTS and stream time base anchor the index clock.
  Matroska may omit the first DTS. In that case a bounded, held-source
  first-packet SHA-256 probe uses the selected output's exact parameter-set
  retention filter. The first emitted sample must match those bytes and the
  independently measured PTS/time base. Its composition offset then resolves
  the decode origin with exact integer arithmetic; a mismatch or nonintegral
  clock refuses the fitted index. PTS is never substituted for DTS.
  A seek's measured clock bounds candidate index rows. The buffered fragments
  are converted once per candidate with that row's exact rational clock and
  compared against converted index byte counts. A unique match is required;
  an unmeasured or inconsistent landing refuses the fitted generation.

Decisions (2026-11-12, settled for milestone 4):
- **Pinned per session.** The digest in the server-owned copy intent is fixed
  at a session boundary. Its producers load the table by file ID and digest.
  If retention has removed the table, a new rendition generation may pin the
  current digest under a new identity before publishing. A refit published
  mid-generation cannot change that generation's bytes.
- **v2 only for fitted titles.** A title without a fitted table keeps
  `dv-p7-to-p81-rpu-v1`, so its indexes and caches stay valid. A fitted
  title's identity is `dv-p7-to-p81-rpu-v2` plus the curve digest. Identity
  and bytes always agree: if curves cannot be applied, the conversion is
  refused, never served as identity bytes under v2.
- **Enable, not gate.** Per-platform settings
  `playback.dolby_vision_refit_{apple,web,android,linux}`, default off, are
  read against the accepted control snapshot's typed `ClientPlatform`.
  Initial playback has no accepted control snapshot yet and serves v1; the
  next session boundary can pin a fit. The diagnostic `client.kind` never
  selects curves. Off ⇒ v1 bytes and identity.

As built (milestone 4; acceptance remains open):
- `fmp4::rewrite_timed_video_samples` hands the closure each sample's decode
  time (`tfdt` plus earlier durations) and composition offset.
- `transcode::dv_refit_serve`: `ServedCurveTable` (scenes by `first_pts`,
  servable re-check, identity outside every scene) and `SourceClock`
  (track ticks to source pts, exact or refused).
- `dvconvert::convert_length_prefixed` takes the sample's curves and writes
  them after `To81`; `dvpipe::Converter::with_refit` holds the pinned table
  and clock and resolves each sample's scene.
- The index pass records a source packet clock origin and converts every
  fragment under the digest-named output. VOD generation loads the pinned
  table and converts its candidate landing before comparing video byte
  counts. Its resolved digest is reported in session and start information.
- Real-source validation of a nonzero start with B frames, seek-window
  readback, and the full milestone 4 acceptance checks remain open.

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

The complete-title scanner now records a source-fenced `el_kind` aggregate:
consistent FEL · consistent MEL · unknown · inconsistent. An old schema-1
probe without this optional field reads as unknown, and the changed scanner
binary causes a fresh title-scan generation. Only a complete, consistent FEL
fact enters F+ demand; a legacy recipe verdict cannot infer FEL. The fit's
identity remains source size/mtime, selected video stream, helper mode/ABI and
`FIT_VERSION`. A pending disk conversion requests fitting even while all
client F+ switches are off, because the operator's conversion request requires
that prerequisite. The disk worker leaves the conversion queued with a visible
waiting reason until a current `Fitted`, `Identity` or content `Refused` outcome
exists; it does not occupy a conversion slot for the duration of the fit.

For a fitted title the existing scanner makes an optional sidecar from the same
held source: selected-stream time base, then each displayed RPU's exact packet
PTS and SHA-256 in decode order. The raw HEVC extraction has no presentation
clock. dovi_tool 2.3.3 extracts presentation-ordered RPU payloads without
HEVC NAL headers. The converter sorts sidecar rows by unique PTS, restores
headers for parsing/hash comparison, and matches every extracted RPU to its
sidecar hash and the stored title frame count. It selects each stored scene by
PTS, converts that RPU with `To81`, applies its stored curve when present, and
injects the resulting RPU stream. After remuxing, it extracts RPUs from the
replacement and compares every one to the authored bytes. Any mismatch or
incomplete mapping refuses the candidate while the original source remains.
MEL and recorded identity/refusal outcomes retain the existing `-m 2 convert`
path. The source and replacement still pass the held-inode binding, content
hash, owned-scratch and recovery-guard checks of `dv_disk`.

**Qualification boundary:** the original 311-packet Michael window has an
84 ms uncovered tail interval and is correctly refused by the complete-title
scanner. Acceptance preserves it as a negative control and uses its contiguous
310-packet prefix. Disk authoring/readback uses a timestamp-normalized copy of
that prefix; indexed-copy acceptance keeps the nonzero-origin prefix.

The existing raw-HEVC disk remux does not preserve a positive video start
offset. The nonzero-origin prefix is safely refused by the unchanged duration
guard (124 ms drift versus one 41.708 ms frame), with the original retained.
Preserving that origin is a separate open disk-conversion finding, not covered
or repaired by F+. Zero-origin acceptance must not be presented as nonzero
origin disk qualification.

## 4. Developer tab and visibility

Developer gets an enable section, "F+ curves for converted Dolby Vision"
(settings `playback.dolby_vision_refit_{apple,web,android,linux}`). Its
requirements are advisory, per the house rule, and never block the enable:
- the helper supports `--refit-stats` on this node;
- *N of M* P7 FEL titles have been fitted;
- clients measured to apply 8.1 mapping: macOS yes, Lenovo TB322FC no, the
  others unobservable.

Playback info for a converted P7 title gains a line: "F+ curves: fitted,
−N%" or "identity (why)". The percentage is held-out squared code error
removed versus identity mapping, not measured display improvement. It is
captured with the resolved digest and read from the attached rendition;
status polls perform no curve-store query.

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

Milestones 1–2 landed separately. At Paul's request, milestones 3–6 are
batched in PR #1565 with proper commits, one final adversarial review, then
the acceptance and fast-lane checks. Rerun only failed checks; preserve
completed applicable results.

1. **The arithmetic** (`plurx-core/src/transcode/dv_refit.rs`):
   - **Contents:** stats, safe solve, diagnostics, composer domain,
     `reshape`, and `apply_to_rpu`.
   - **Acceptance:** `cargo test -p plurx-core --lib dv_refit` passes, which
     includes:
     - coefficients within 2·2⁻²³ of `rpu_refit_eval.py` on all three
       fixtures;
     - the composer domain within 1e-6;
     - a real P7 RPU round trip exact to 2⁻²³.
2. **Helper `--refit-stats`:** merged (#1542).
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
6. **Developer section and playback info:** built, acceptance pending.
   - The report comes from the attached rendition's pinned digest and
     captured gain. Status polls do not read the curve store. Identity is
     reported only for actual To81 conversion, with the generation's reason.
   - **Acceptance:** each state renders on the web and Apple Settings, and
     a dv_patch_clips run on macOS against a served fragment shows A == B.
7. **Re-encode trials (§5):** plan first, then build.

## 7. Non-goals

- **No decoder or client protocol change.** Clients that ignore the mapping
  keep their current picture. The Apple Developer and playback-info UI in
  milestones 3 and 6 reports existing server facts; it adds no platform
  signal or decoder behaviour.
- **No change to the processed route.** It already composes the EL.
- **No per-frame curves.** They gained under 0.002 offline and would invite
  flicker.
- **No MEL titles.** A MEL carries no picture detail, and `To81` already
  keeps its mapping.
- **No gating.** The Developer section reports readiness; it does not block.
