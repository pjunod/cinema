# Display-targeted Dolby Vision rendering — build plan

**Status:** PR A P8 cells passed; scanner controls partially passed, fixture repair pending review · **Updated:** 2026-10-22 · **Owner:** Sol 6.1 (source), coordinator (native/media/publication) ·
**Extends:** item 1 of [DV-ENHANCEMENTS-NOT-YET-BUILT.md](DV-ENHANCEMENTS-NOT-YET-BUILT.md)

## PR B implementation checkpoint

**Authored, not qualified (2026-10-22).** Explicit targeted HDR10 windows now
resolve output mastering metadata before encoding and author it after successful
private helper/encoder completion, before encoded-window validation and
publication. The writer is independent of the encoder family. The same payloads
replace stale encoder declarations in hvcC, init `mdcv`/`clli` and prefix SEI on
every IRAP; unrelated SEI, coded picture NALs, audio and sample timing survive.
The completed trailer is rebuilt because init/sample growth moves offsets.

Consistent title L9 supplies mastering primaries. Its preset coordinates come
from the pinned Dolby Vision dependency; custom coordinates convert from
1/32767 into ST 2086's 1/50000 units. Missing L9 may use MDCV from the exact
selected held source stream's already-attested probe side data. Contradictory
L9, reserved or unrepresentable primaries, malformed/conflicting source MDCV,
and missing source primaries refuse the explicit target with a named reason.
No BT.2020 container colour tag or assumed P3 gamut supplies missing mastering
facts. Title L6 supplies the minimum; missing L6 may use the source MDCV minimum,
and contradictory/missing minima refuse. Maximum luminance is the bucket,
MaxCLL is min(bucket, scanned title L1 peak), and MaxFALL is zero (unknown).
Default/master and elided policies retain their original encoded bytes.

Authored regressions cover preset/custom units and unknown/conflict handling,
selected-stream rational MDCV parsing, stale prefix/suffix SEI replacement for
IRAP types and NAL length sizes, identical init/in-band payloads, unchanged
coded pictures/audio/timing, and an independent decoded-frame ffprobe check.
No regression has run: completed-code independent review precedes functional
validation. Pinned compilation, independent review, once-only regressions,
actual encoder-family/device acceptance and publication remain open. No targeting
control or later C–F feature is introduced by B.

## PR A implementation checkpoint

- **Current qualification (2026-10-22; supersedes historical states below):**
  the independently approved base-receipt repair compiled and its exact new
  regression passed once. Both real P8 mapped cells, 1000 and 1600 nits, passed
  with complete 17,983-frame L1 observation and target-operation receipts.
  Retain these and the prior 99 Rust / 141 shader / 70 helper passes unchanged.
  P7's corrected copied interval passed all payload/timing/audio/FEL/raw-L1
  preparation checks, but its decoded scanner exited 1 with no aggregate:
  `complete selected video duration tag`. The selected video starts at 0.125 s,
  ends at 750.165 s and declares `DURATION=00:12:30.165000000`; subtracting its
  origin compares the wrong domain. The sibling renderer already uses the
  selected-track tag as an endpoint. Only the scanner's two comparisons change;
  relative `AVStream.duration`, independent selected-video coverage, decoder
  drains and the one-tick allowance remain intact. No endpoint-or-span fallback,
  audio/container substitution or real-input retiming/tag rewrite is introduced.
  Scanner-only linking and independent delta review passed. The manual control
  `tools/dv_processing/controls/test_scan_duration_tag.py` passed nine P7 scanner
  observations, then failed before its truncation scan: a time cutoff produced
  14 rather than 17 coded packets. The control-only repair copies exactly 17 AUs,
  muxes full unrelated audio separately, verifies original payload/PTS/durations,
  and preserves the 18-picture endpoint declaration. Hash-bound continuation
  retains the nine passes without remux/probe/scan replay. New control-delta review,
  repaired P7 truncation and ten unexecuted P8 observations precede failed P7
  decoded/policy and mapped cells. No scanner rebuild or unchanged-pass replay.
  Separate FEL integration, one fast lane and API merge remain open.
  No display/deployment/client-DYN/general Dolby accuracy claim; output-metadata
  PR B must land before any user-exposed targeting setting.

- The corrected real P8 excerpt reached complete 17,983-frame L1 observation and
  master preparation, then its single 1000-nit test failed with exit 101: the
  completed base-RPU receipt omitted `DisplayTargetMapping` (no count, expected
  48). The helper target rows and rendered operation had already been validated;
  source-RPU operations alone cannot describe post-reconstruction targeting.
  The narrow repair retains conditional source operations and adds only the
  completed graph's display-target operation for base receipts. A new focused
  regression covers master/1000/1600 accounting and guard refusals, not helper
  qualification. Compilation, independent targeted review and new/failed-only
  validation remain. The 1600-nit cell was not run, no mapped cell passed, and
  the interrupted standalone scanner's unknown exit is not reconstructed.
  Retain prior 99 Rust / 141 shader / 70 helper passes and successful unchanged
  policy evaluations; do not repeat them for the new receipt regression.
- The first full-helper attempt refused the synthetic scan: the control muxer
  omitted the encoder's declared frame rate, losing Matroska DefaultDuration and
  demuxed packet durations. The control-only repair preserves matching positive
  BL/EL rates; native mux compilation and independent targeted review passed.
  The coordinator reports the corrected Stage2 control passed 97 commands and
  70 results, worst independent-oracle error one code. This is synthetic helper
  acceptance, not real-title or device accuracy; the original refusal is retained.
- Rust qualification retains 99 distinct passes: the original 98 selected cases
  plus one partial-schema control. Initial four fixture failures and intermediate
  retries remain distinguishable from passing evidence. Final fixture repairs at
  `c93b3de50814` use a production-attested live fragment reader and two valid
  producer leases, respecting background-reader reservations and the 128-probe
  per-coordinator cap. Independent Astra targeted closure found no blockers;
  exact-source native formatting, all-target Clippy and contract/daemon compilation
  passed, followed by only the two remaining failed cases, each exactly matched
  and nonignored. Compiler, binary, list, log and true-exit receipts are verified.
  No retained Rust, shader or helper passes are repeated.
- The actual380a B/interface integration is independently approved and compiled
  natively with350. Production Rust/helper inputs are unchanged by fixture-only
  follow-ups; the coordinator owns the four final-daemon real-source cells.
- Full independent Astra review of `0d47bc6ac` is retained. Targeted independent
  review APPROVED the sole F1 repair at `43cce364a9e8`; its pinned native Clippy
  and Hiqlite-contract compilation passed. Post-review Rust qualification runs
  against that immutable source; no Rust pass is inferred from compilation.
- Actual source-bound Intel GPU shader qualification passed: 141 cases, 866,304
  Y/U/V samples, worst independent-oracle error one code (required at most two),
  all six buckets, black/peak/monotonic/edge checks and four exact L1 witnesses.
  This is synthetic post-reconstruction acceptance, not full-helper, real-title
  or display accuracy. The initial output-directory permission refusal occurred
  before any case; corrected execution changed no shader/oracle source.
- Actual FEL-catalog PR #1211 landed as `9753284ef7dd`. Integration preserves its
  typed cancellation/supervised-helper lifecycle while forwarding C's explicit
  target through the completed private window. The ignored physical harness
  unwraps `WindowOutcome::Completed` and still checks every target/L1/operation.
  Ordinary production entrypoints remain master-only. Main still ends at schema
  113, so this additive title migration remains 114 without renumbering.
- Actual first 750-second real-source matrix failed input prerequisites, with
  zero mapped-window passes. Both P7 target cells reached eligible complete stats
  but selected `TitleFits` (observed peak at most 1000 nits). P8 returned a scanner
  failure; its redundant second cell was stopped, not passed. The scanner's exact
  stderr is being captured once as failed-prerequisite diagnosis. No production
  defect is established, and neither buckets nor completeness rules are weakened.
- Cheap original/excerpt headers and one-second decoded witnesses were inspected.
  The current ffprobe exposes source PQ endpoints, not the L1 min/average/max
  triplet; absent fields do not establish a dark title. Short existing-tool RPU
  inspection must establish actual L1 and P7 FEL before selecting new excerpts.
- Current main `52e66778e7ce` is integrated at `1d2ec2967532`, resolving only
  changelog prose and preserving both topics. Core/helper/C-runtime and regression
  bodies are byte-identical to c93. Main's separate `--source-commit` CLI early exit
  does not run in the physical-probe entrypoint; no passing C case is invalidated.
  Exact integrated-source compilation remains coordinator-owned.
- Remaining: eligible genuine P7 FEL/P8 excerpts with observed L1 above 1600 nits
  and complete timing, the four 1000/1600 mapped runtime cells, exact-source
  integration compilation, fast lane and API merge. Preserve unchanged passing
  shader, policy/store/helper and prior B evidence; check only genuinely changed
  surfaces, not whole suites or reviews for base motion. No display/deploy claim.

### Historical source checkpoints (not current acceptance)

- Independent Astra reviewed the complete `0d47bc6ac` candidate: one blocking F1,
  unrelated chapters/HEVC/content-encoding grafts invalidating title generations.
  The full source pass is retained; only F1 and its repair delta need independent
  closure. That candidate's pinned native all-target check, Clippy and replicated
  store-contract compilation all exited zero; these were not functional tests.
- **F1 repair authored:** all four direct/fenced ordinary upserts and admitted
  `ApplyProbeJob` explicitly write `files.scanned_at`. Its `UPDATE OF` is the
  ordinary-publication intent, even with identical facts and timestamp. Document
  decorations and metadata backfills never write it and preserve title state.
  Source changes delete old demand in BEFORE without modifying the target file
  row; AFTER source cleanup only removes the aggregate. AFTER ordinary publication
  resets/reseeds demand. This is independent of AFTER trigger order when a single
  write carries fresh source and fresh facts, including recursive triggers.
  No JSON inequality, decoration whitelist, new wire command or feature gate.
- New authored controls parameterize the real stores over all three grafts and
  queued/running/published/manually cancelled states; stale-source grafts, direct
  and fenced identical/fresh-source upserts, admitted command publication and
  refused-coordinator/worker replay. A raw-schema control reverses AFTER trigger
  creation order in both recursive modes. None have run. Actual pinned local
  all-target Clippy with the replicated-contract feature exited zero. Native
  repair compilation, independent targeted closure and once-only functional
  qualification remain; B landing is not a review or test prerequisite.
- Draft PR #1216 is published at `59d23e4674d8`; the ignored physical harness
  continuation is being prepared separately. It reads the test-only
  `PLURX_DV_WINDOW_TARGET_NITS` strictly as 1000 or 1600, requires an actual
  complete-title scan, excludes native Profile 8.1, and explicitly targets
  windows 0/1/2/360 through the admitted runtime. Initial master preparation
  is labelled separately. Helper method, bucket, fixed title endpoints, raw
  L1 and per-frame target-operation receipts are asserted, not yet exercised.
  Production wrappers still select master. The unchanged helper bundle has
  static packaging/ABI proof; no numerical or physical qualification is claimed.
- **Durable acquisition correction authored, not qualified:** ordinary
  facts again use production `leaf-probe-v2`; comparison with pre-C source found
  no changed ordinary conclusions warranting a library-wide reprobe. A shared
  SQLite/Hiqlite demand outbox seeds current DV facts. `DvTitleScan` v1 contains
  only file identity, size/mtime, monotonic probe revision and scanner digest,
  never caller paths/tools. The existing scheduler owns the cancellable title
  future with one-CPU admission and common durable source-I/O reservation.
  Locally readable compatible higher-priority probes with live coordinators
  cause cancellation, child reap and release before ordinary admission. No new
  detached loop, watchdog or feature gate. Yield restarts the whole scan;
  sustained ordinary work can starve completion, with no partial eligibility.
- Manual cancellation suppresses the same generation immediately on cancelling,
  unlike scheduler Yield. Rebinding the same scanner cannot clear suppression.
  Scanner A→B→A rotates the existing monotonic demand revision, so the final A
  cannot reuse an old terminal request identity. Initial migration seeding runs
  only before the extension marker is committed; replay cannot recreate demand
  after a source-only update awaiting fresh ordinary facts.
- Priority observation retains its bounded keyset cursor across scheduler ticks,
  including beyond 128 locally unreadable probes. New title admission waits for
  the ordinary walk to finish. Preemption awaits the child and releases admission
  before restarting ordinary priority order. No separate polling mechanism.
- Source/probe/scanner changes invalidate old work.
  Aggregate attachment and
  settlement share a live-attempt/source-fenced transaction; attachment does not
  self-enqueue. Old unfiltered list/detail and UUID cancellation were inspected
  in source only; explicit new-kind filters hit a closed old enum, while retry
  and execution reject unsupported kinds. No mixed-version execution claim.
  Migration 114 is provisional behind this snapshot's existing 113; actual main/B
  schema numbering must be reconciled before landing.
- Authored, unexecuted regressions cover publication/replay/no self-enqueue,
  cancel/publication/Yield ordering, source/scanner/reprobe ABA and delete/reinsert,
  restart suppression, backend priority/offline/resource blockers, nested triggers
  in both recursive modes, one-shot seeding, candidate pagination, and real child
  reap with exact one-thread admission. Store contracts share the SQLite/Hiqlite
  backend harness; native execution remains post-review.
- Platform gates now match the Linux-only runtime; yield observation borrows the
  owned task mutably rather than requiring a Sync future or detached task.
  Explicit-path local Rust 1.97.1 all-target checking, core Clippy and
  Hiqlite-contract-feature compilation passed. These macOS checks exclude the
  Linux-only runtime. Earlier native checkpoint failure remains a failure; a new
  exact-source controller Linux receipt for the F1 repair is still required.
  The later `0d47bc6ac` checkpoint passed native compilation. Early local checks
  that selected Homebrew subcommands are diagnostic only, not pinned evidence.
- Remaining: pinned controller compilation of the F1 repair, independent targeted
  finding closure and the once-only qualification batch. Actual B/main/schema
  integration later needs only review/checks invalidated by its real delta.
- Baseline Rust 1.97.1 all-target checking and helper compilation against the
  pinned libplacebo API 374 succeeded; these are baseline, not candidate receipts.
- Complete-title scan groundwork is being implemented: one software base decoder,
  no GPU or render, exact decoded-frame accounting and clean EOF/drain. Typed
  aggregates live in `probe_json.plurx_dv_title_v1` under the dedicated title job's
  existing source/publication fence. Unknown versions, partial scans and old
  documents cannot establish eligibility. L6/L9 are constant, unknown or
  inconsistent; a missing observation is not silently replaced by the last one.
- Checkpoint `883b40fe` passed pinned Rust all-target checking and API 374 helper
  compilation. Checkpoint `37112e73` compiled the control driver and fixture muxer;
  its Rust check failed E0624 (example calling a private identity method), corrected
  by removing the unnecessary call, not widening production visibility. Original
  E0433 remains preserved as a failure, corrected by `01d21106`. Later source
  changes need fresh affected compilation. None are numerical qualification.
- Explicit title-qualified policies, semantic bucket/endpoints/title identity,
  helper arguments, report operation and exact request-bound runtime trace checks
  are implemented. Production constructors still select master. Real synthetic
  scan/window/elision controls and an independent shader oracle are authored.
  No units or controls have run: complete independent review/fixes precede their
  single affected batch. Final compilation/static checks, actual FEL-catalog B
  landing reconciliation and draft paperwork remain.
- The shader controls also include independently derived, public-standard
  constant-pixel witnesses: L1 average changes at 400 nits and L1 maximum changes
  at 4000 nits must move in the predicted direction and match their own expected
  10-bit codes within two. Hash inequality alone is insufficient. These are
  authored controls, not observed numerical passes.
- The scanner requires contiguous observed display-frame timing, clean decoder
  drain and an independent selected-video count/duration. Unknown endpoints or
  truncated containers returning clean EOF cannot claim completeness. Negative
  or unknown origins remain unqualified; no container/audio-duration proxy.
- Actual upstream API 374 inspection found a design conflict with the helper
  recipe below. `hybrid_mix` is removed, and its ST 2094-10 function uses a modified
  PQ-space knee selector. The shader also applies an adaptive virtual target and
  LMS/film response independently of peak detection. Setting contrast recovery
  and hybrid mix to zero therefore does not establish Annex B.2 equivalence.
  The required independent two-code bound is retained. The coordinator accepted
  the explicit post-reconstruction shader as a documented implementation-plan
  deviation, with separately named engineering extensions for degenerate anchors.
  It has not yet been numerically qualified. See the actual method below.
- The numerical reference is the published
  [SMPTE ST 2094-10:2021 Annex B.2](https://pub.smpte.org/latest/st2094-10/st2094-10-2021.pdf),
  not a translation of libplacebo's implementation. No private LUT research or
  proprietary output is part of this work. Metadata writing and viewer selection
  remain later PRs; targeting is not exposed to playback here.

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

| PR | Contents | Deploy |
|---|---|---|
| A | Title L1 scan (stored with the DV facts) + helper target policy + L1 → CIE_Y + ST 2094-10 + `DvExportPolicy` identity + report operation + control scripts | server image |
| B | MDCV/CLL writer for processed output (post-encode, reusing `hdr10_index.rs`) | server image |
| C | Caps `min_nits`/`source`, buckets, elision, Developer setting + enable section, indicator, Activity text | server image |
| D | Android HdrCapabilities + web display-peak setting (+ Apple value) | server image (web) + Android build |
| E | Phase 2 trims (after acceptance) | server image |
| F | Phase 3 SDR target | server image |

Each PR follows the usual lifecycle: draft, one adversarial review, fast
lane once, merge with `Regression-Test:` lines. STATUS.html is updated as
each one lands.

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
