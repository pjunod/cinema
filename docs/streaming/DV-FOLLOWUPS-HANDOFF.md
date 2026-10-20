# Dolby Vision follow-ups: handoff for any agent (2026-10-19)

**Status:** open · **Updated:** 2026-10-19 · **Owner:** unassigned (from Claude session 018Q4JhsMWmMe7WWFAUpqNJz)

Repo: Forgejo `noirr/plurx`. Process:
- Paul's PR lifecycle: draft with a `WIP:` title → one adversarial review → fast lane once → merge via the API as a merge commit, with `Regression-Test: <path>::<name>` lines in the merge message.
- Paul deploys; agents never deploy.
- No code gates: features get an advisory Developer-tab section.
- Plans: [DISPLAY-TARGETED-DV-BUILD.md](DISPLAY-TARGETED-DV-BUILD.md) and [DV-ENHANCEMENTS-NOT-YET-BUILT.md](DV-ENHANCEMENTS-NOT-YET-BUILT.md).

Already merged and deployed: #1197–#1203 (HDR10-E at all resolutions, 4K throughput, FEL/RPU/DYN indicator, Animals frame-rate fix) and #1208 (display-targeted plan). Chrome on a MacBook Pro shows FEL+RPU for HDR10-E.

## A. Pixel fixes — merged PR #1210 (source owner: Sol; publication: coordinator)

Original authored head `eed076a5e` on `claude/pixel-hdr10e-and-animals` is
preserved in the preparation branch `codex/cinema-pixel-hdr10e`, with current
main incorporated. PR #1210 landed as `e940b75b455e` after independent Astra
review, pinned compiler/static checks, five affected regressions and fast lane
5546 passed on reconciled head `44d31080e7fa`.

**What it fixes:**
1. **Avatar on a Pixel 11 never got HDR10-E.** The Pixel has no DV decoder, and the decision gave it a progressive remux of the HDR10 base. HDR10-E is applied only on an HLS session. With Developer → DV processing (`playback.dolby_vision_hdr_processing`) on, `routes_through_processed_hdr10` (`crates/plurxd/src/http/stream.rs`) now makes such a delivery a copy over HLS (`requires_hls: true`, reason named).
2. **Animals' fallback transcode answered 500.** Its 2160p continuous AVC primary is outside H.264 High@5.0 (`decoder plan refused: continuous_video_envelope is invalid`). `continuous_candidates_from_workers(workers, rate)` (`crates/plurxd/src/http/hls/create.rs`) now offers only rows inside `continuous_avc_envelope_accepts` (made `pub`) at the source's measured rate, and none when the rate is unmeasured.

**Status:** merged; deployment is not verified here. The coordinator's merge
receipt binds parents `f5323c20e359` and `44d31080e7fa` and both regression
anchors. Retained stale-binary runs are not evidence; source-bound receipts
and the passing reconciled-head lane are the qualification evidence.

**Finish:**
1. Source/review/test/fast-lane/API merge steps are complete. Landing contains:
   - `Regression-Test: crates/plurxd/src/http/tests.rs::processed_hdr10_setting_routes_a_non_dv_client_over_hls`
   - `Regression-Test: crates/plurxd/src/http/hls/tests/chunk_06.rs::continuous_catalog_offers_only_rows_inside_the_avc_level_envelope`
2. Paul deploys; after deployment, check Avatar on the Pixel shows the HDR10-E badge and FEL/RPU and Animals' fallback starts. No deployment or device result is claimed here.

## B. FEL→Profile 8.1 for DV displays — merged PR #1211 (source owner: Sol; publication: coordinator)

Original authored head `1eeb0542d` is preserved on preparation branch
`codex/cinema-fel-p81-catalog`. Actual Pixel/main landing `e940b75b455e` is
integrated, preserving unrelated main changes without changing B's implementation
or regression bodies. The sole merge conflict was changelog prose, resolved by
retaining both topics and removing duplicate Pixel bullets. Draft publication
`fdc6d3ed` passed pinned formatting/all-target Clippy, but the complete independent
Astra review required F1: real ingress saved no conversion provenance, and Encode
normalization lost P81 eligibility. The F1 repair at `418467db` compiled (pinned
formatting/all-target Clippy and linked daemon) and was published. Same-review
recheck found F2: catalog Copy audio retained the raw AAC echo, while actual
execution re-resolved canonical audio. Sol's narrow repair freezes only original
reviewed DV provenance, then constructs the query after canonical track selection
using the same Progressive track/offset resolver as actual Copy reconciliation.
P81 scope survives geometry/Encode normalization; original intent fingerprinting,
strict admission equality and ordinary fallback remain unchanged. Corrected F2
compilation/publication and the same independent reviewer's closure completed.
The Linux-only F3 fixture correction also passed; no production contract was
relaxed. PR #1211 merged as `9753284ef7ddf1ef4deb7c35a1116f56ace62068`,
with fourteen source-bound positives, two old-behavior sensitivity controls
and the complete required fast lane 5600 green. Deployment and Safari/Studio
Display physical acceptance remain Paul's work, not a software-pass claim.

**Retained evidence:** exact pre-integration `f931f603` formatting and all-target
Clippy passed after repairing the baseline helper visibility and one range-style
lint. The three section-B regressions each passed with exact source/archive,
Cargo JSON-selected binary, nonzero count and true-exit receipts. An initial
wrong-namespace zero-match failure is retained; only its two unexecuted cases
resumed. Test-only old-main sensitivity built successfully and each daemon case
failed with exactly one match. The runtime source assertion detects unconditional
Copy refusal; the HLS indicator assertion detects missing reconstructed RPU.
The raw aggregate sensitivity receipt remains FAIL because the prepared
first-HLS-shape-assertion expectation was wrong: early shape assertions passed,
then the indicator failed. Corrected classification is separate from raw output.
Unchanged positive and negative evidence is retained, not repeated or promoted
to physical playback evidence. Final review can invalidate changed closures only.

**Symptom:** Safari on a Studio Display, Avatar (P7 FEL), FEL re-encode on, gets the plain P7→8.1 conversion with "FEL: not used for this play" and no reason. The reconstructed 8.1 (FEL rebuilt, re-encoded, new RPU) is never reached, although #1199's physical test proves that path.

**Cause:**
- `prepare_processed_hdr10` (`crates/plurxd/src/transcode/manager/create.rs`) returned early for every `Copy { preserve_dolby_vision: true }`.
- The DV backend is captured only when there is no `candidate_context` or this is a processed trial. Catalog starts always carry a context.

**Branch changes:**
- A converted copy (`Copy { convert_dolby_vision: true }`) is offered reconstructed 8.1 under `fel_reencode` plus conversion policy, independently of `hdr_processing`, and declines are named. Both processed trials use a finite even-height `Transcode` with an explicit service-owned optional `dv_processing_destination`; preflight and runtime use one resolver. Native DV remains untouched.
- The original catalog query survives normalization. Its v2 description/reuse scope binds destination, full preferences and current encoded audio. Copy-route audio is discarded and re-resolved through the canonical EncodedVod AAC claim; actual geometry/Main10/P8 support must come from the same video entry.
- Ordinary payloads omit the optional destination and keep their intent bytes. Explicit new payloads fail closed on old strict workers; remote/durable serialization keeps semantics but loses local prepared authority. Neither restoration nor strict kind/audio equality is weakened.
- `processed_candidate_digest` (`crates/plurxd/src/vodencode.rs`) accepts both destinations with distinct tags.
- `session_delivered_processing_shape` and `session_enhancement` no longer require a Transcode kind.
- Enhancement text: the reconstructed 8.1 lights FEL+RPU+DYN, and the plain 8.1 explains its RPU.

**Tests:**
- `chunk_05.rs::a_reconstructed_profile81_copy_is_described_as_reconstructed`
- `dv_runtime.rs::a_converted_dv_copy_is_offered_the_reconstructed_profile81`
- `enhancement.rs::reconstructed_p81_carries_fel_and_dyn`
- `chunk_06.rs::reviewed_catalog_ingress_keeps_p81_scope_through_encode_and_reselection`
  drives the shared production ingress/normalization helpers into qualification
  inputs and remembered-description lookup, including FEL-only, HDR, native,
  numeric and disabled-policy cases. F1 qualification passed.
- `chunk_06.rs::ordinary_copy_catalog_audio_matches_reconciled_execution`
  compares production query and normalized/reconciled ordinary Copy tuples and
  recipe digests: both stale AAC directions, nondefault track, offset, native,
  DV-declined, failed optional preparation and legacy no-claim behavior. Written
  for F2; positive execution and compile-compatible old-418 sensitivity passed.

**Resolved design risk; still a review focus:** the original Encode-route/Copy-kind
trial violated retained-kind/audio invariants. The accepted explicit optional
`SessionRequest` destination uses a truthful Transcode trial instead. Review
strict `deny_unknown_fields` mixed-version refusal, local-only prepared authority,
restore/successor/recovery/link paths, canonical encoded AAC and same-entry Safari
Main10/Profile 8 raster/cadence/bitrate support; do not relax equality to make reuse
work. Ordinary producers refuse serialized explicit processed destinations.

**Reviewed implementation:** canonical Astra decision accepted the explicit
destination and truthful encoded carrier. Source repairs the retained-kind/audio
mismatch, FEL-only reuse scope and swallowed processing errors, and checks plan
destination/height again at prepared admission and retained binding. Source tests
cover wire/intent/binding and ordinary-producer refusal, preference/source/encoded
audio scope, same-entry completed client contract and original daemon indicators.
The complete independent review found F1 in production ingress, not in the
retained wire/lease safety contract. The repair retains original intent ordering,
ordinary fallback, query/destination equality and strict kind/audio binding.
Corrected compilation, same-reviewer closure and post-review execution are
complete for PR #1211. The separate reservation-slot
incident and general remote processed requalification remain out of B scope.

**Finish:**
1. Source, independent review, pinned compilation, affected qualification,
   required fast lane and API merge are complete. Retain passing evidence;
   history rebinding alone does not invalidate it. Ten regression anchors are
   retained in the PR and landing message.
2. Paul deploys. Device check after deploy: Safari plus Studio Display, Avatar
   from 10 minutes in, expecting FEL+RPU+DYN and "FEL: full enhancement layer
   reconstructed". Also check there is no stutter. This remains unverified here.

## C. Display-targeted HDR10-E — PR A of [DISPLAY-TARGETED-DV-BUILD.md](DISPLAY-TARGETED-DV-BUILD.md)

1. **Title L1 scan,** stored with the DV facts:
   - L1-max maximum;
   - whether every frame has valid L1;
   - L6 mastering min/max;
   - L9 primaries.
2. **Helper policy `bt2020-pq-target-<nits>`,** with nits ∈ {400, 600, 1000, 1600, 2000, 4000}, in `tools/dv_processing/fel_renderer.h` and `base_dv_renderer.h`, parsed in `segment_decode_render.c`. Per frame:
   - `max_pq_y = L1.max_pq/4095`, `avg_pq_y = L1.avg_pq/4095`;
   - source `max_luma` = title mastering peak (L6, else 4000);
   - constant min luma = title mastering min, on both sides;
   - target `max_luma = nits`;
   - `PL_HDR_METADATA_CIE_Y`, `pl_tone_map_st2094_10`;
   - **`contrast_recovery = 0`, `hybrid_mix = 0`**;
   - clip gamut; no peak detection;
   - report `DvOperation::DisplayTargetMapping { nits }`.
3. **Identity:**
   - `DvExportPolicy::target_parameters()` (`crates/plurx-core/src/transcode/dv_processing.rs` ~1275) gets a new versioned string, so `semantic_digest` differs per bucket.
   - Widen the `dv_runtime.rs` graph check (~203) and the `output_policy` check (~1438).
   - Replace the hardcoded helper argv in `dv_segment.rs` (~92, ~2156).
4. **Controls** (`tools/dv_processing/controls/compare_display_target.py`):
   - peak ≤ PQ(nits) + 1 code; monotonic curve;
   - identical pixels with different L1 differ;
   - within 2 codes of an independent Python ST 2094-10 Annex B.2;
   - overlapping windows give identical `yuv10_sha256`;
   - elided equals master byte for byte.
5. **Rust tests:** distinct digests per bucket; the physical test env `PLURX_DV_WINDOW_TARGET_NITS`.

PRs B–F (MDCV/CLL writer reusing `crates/plurx-core/src/fmp4/hdr10_index.rs`; caps/buckets/setting/indicator; Android and web peak; trims; SDR target) are specified in the plan.

## D. Small open items

- **Pixel diagnostic bounded closure:** on app 0.7.0/code 175 and Media3 1.10.1,
  the coordinator observed Air (2023) render and advance 0:34–0:51 without a
  PlaybackException, on actual HEVC+AAC processed HDR10-E/RPU delivery. Earlier
  Animals/Uprising failures identify a Dolby E-AC3 audio `queueInputBuffer`
  error (index 1), not a demonstrated HEVC decoder failure. Air's source is
  E-AC3/Profile 5, but its delivered AAC makes it neither a passing direct-E-AC3
  control nor proof of corrupt packets. No decoder source change is justified
  by this comparison; broader direct-audio diagnosis remains separate.
- **Apple TV UI test overruns:** `testLiveFullscreenRevealMoreReportAndBackRetainSession` goes past its 3 minutes. It predates #1197; rerun it on the Mac simulator.
- **A second processed session is refused** while the first holds the processing slot ("DV processing needs a GPU slot and 15 CPU threads that other work holds"). After an Auto replacement the successor may lose HDR10-E. Check whether successors should reuse the prepared processed root (`capture_processed_root`).

### Processed-root repair in progress (owner: Sol; native/publication: coordinator)

`codex/cinema-processed-successor-reuse` removes the leftover throughput-only
refresh/readers from exact-root reuse, retaining source/executable, full recipe,
principal/playback, actual rendition/reservation and final attachment fences.
The first pending staged successor can capture only its ledger-named local
predecessor under the caller's admitted epoch. A narrow, nonserialized session
seed preserves original Copy provenance; current planning/caps/decoder/audio
and scope are reconstructed and checked against the actual encoding. Ordinary
serialized restore still refuses processed descriptions. Changed recipes,
destinations, heights or independent plays do not borrow capacity.

Actual-owner cleanup is armed synchronously with registry attachment, before
marking/facts/seal awaits. The predecessor is never retired early; normal durable
seal/commit CAS still decides activation. Shared encoding priority is not changed
by a speculative successor. No new render, speed receipt or sustainable bit is
invented to admit an already-owned graph.

Current source includes lifecycle regression controls for both processed
destinations with a checked HEVC/PQ window, private-context loss on serialization,
the manager's mixed GPU/CPU saturation, missing admitted epoch, input/owner drift,
replay, two successive durable commit CAS/parent retirements and exclusive-worker
reap. Deterministic test-only pauses cover cancellation after attachment, facts,
before seal and after seal, plus predecessor epoch motion before the frozen seal.
Controlled completed-window publication binds reporting to the actual successor
owner: no HDR10-E is reported from descriptor selection alone; reconstructed P81
retains its distinct carrier/FEL semantics. These controls do not run the renderer
or establish Dolby pixel accuracy. Ordinary serialized recovery stays refused.

Candidate `03cbfe62c` passed pinned Rust 1.97.1 fmt/all-target Clippy and test
linking. Independent Astra complete review required F1 exact-owner race,
F2 cleanup-settlement and F3 real AAC controls; bounded closure approved the
repairs. The first fifteen-case affected batch has six genuine passes and nine
fixture setup failures, not nine demonstrated production failures. Eight staged
controls supplied an invalid coarse source tick; the other omitted its admitted
incarnation. Repairs probe actual source packet timing, retain the 24000/1001
cadence and 1/24000 source tick through the checked output, and supply real
incumbent/successor request incarnations. No production validator is relaxed.
Candidate `7cd6b015` passed pinned compilation/linking and bounded same-reviewer
closure, but its nine-case retry had no passes. Eight controls reached the next
sample-entry guard: SPS PQ flags do not supply the required MP4 nclx record.
Synthetic frame color properties plus explicit `write_colr` now generate that
record locally; a native standalone production-validator diagnostic is pending,
not a unit pass. Failure paths retain actual bytes, source probe and encoder argv.
The ninth reaches final predecessor revalidation; its external worker claim
conflicts with the automatic driver's empty slot. Coordinator-approved test-only
driver coordination now keys a scoped guard to the exact physical encoding before
the first wake. Captured tasks exit on cancellation, never resume into a generation;
normal owner cleanup still settles before explicit task exit and capacity assertions.
Assertions check absent slot, zero epoch/child pid and the real exclusive worker.
This isolates driver scheduling; it does not qualify normal driver execution/reap.
Production source/window/attachment validators are unchanged. The six original
passes remain; corrected compilation, bounded closure and nine failed-only retries are owed.
Actual B landing `9753284e` is incorporated. Required fast lane and API merge
remain. The historical live request path remains unknown; this is no deployment
or general Dolby rendering-accuracy claim.

Regression anchors for the eventual PR and merge message:
- `Regression-Test: crates/plurxd/src/vodencode_tests.rs::processed_root_reuse_needs_no_speed_or_missing_qualification_window`
- `Regression-Test: crates/plurxd/src/transcode/manager/processed_reuse_tests.rs::staged_processed_roots_reconstruct_both_destinations_without_serialized_authority_or_speed`
- `Regression-Test: crates/plurxd/src/transcode/manager/processed_reuse_tests.rs::staged_processed_root_refuses_current_input_and_owner_drift_without_spending_capacity`
- `Regression-Test: crates/plurxd/src/transcode/manager/processed_reuse_tests.rs::first_processed_attachment_arms_exact_owner_cleanup_before_returning`
- `Regression-Test: crates/plurxd/src/transcode/manager/processed_reuse_tests.rs::staged_processed_second_successor_survives_normal_commit_and_parent_retirement`
- `Regression-Test: crates/plurxd/src/transcode/manager/processed_reuse_tests.rs::staged_processed_cancellation_at_attachment_facts_and_seal_releases_only_successor`
- `Regression-Test: crates/plurxd/src/transcode/manager/processed_reuse_tests.rs::staged_processed_epoch_motion_after_attachment_refuses_frozen_seal`
