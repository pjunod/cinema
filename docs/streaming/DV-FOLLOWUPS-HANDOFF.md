# Dolby Vision follow-ups: handoff for any agent (2026-10-19)

**Status:** open · **Updated:** 2026-10-19 · **Owner:** unassigned (from Claude session 018Q4JhsMWmMe7WWFAUpqNJz)

Repo: Forgejo `noirr/plurx`. Process:
- Paul's PR lifecycle: draft with a `WIP:` title → one adversarial review → fast lane once → merge via the API as a merge commit, with `Regression-Test: <path>::<name>` lines in the merge message.
- Paul deploys; agents never deploy.
- No code gates: features get an advisory Developer-tab section.
- Plans: [DISPLAY-TARGETED-DV-BUILD.md](DISPLAY-TARGETED-DV-BUILD.md) and [DV-ENHANCEMENTS-NOT-YET-BUILT.md](DV-ENHANCEMENTS-NOT-YET-BUILT.md).

Already merged and deployed: #1197–#1203 (HDR10-E at all resolutions, 4K throughput, FEL/RPU/DYN indicator, Animals frame-rate fix) and #1208 (display-targeted plan). Chrome on a MacBook Pro shows FEL+RPU for HDR10-E.

## A. Pixel fixes — branch `claude/pixel-hdr10e-and-animals` (head eed076a5e, no PR yet)

**What it fixes:**
1. **Avatar on a Pixel 11 never got HDR10-E.** The Pixel has no DV decoder, and the decision gave it a progressive remux of the HDR10 base. HDR10-E is applied only on an HLS session. With Developer → DV processing (`playback.dolby_vision_hdr_processing`) on, `routes_through_processed_hdr10` (`crates/plurxd/src/http/stream.rs`) now makes such a delivery a copy over HLS (`requires_hls: true`, reason named).
2. **Animals' fallback transcode answered 500.** Its 2160p continuous AVC primary is outside H.264 High@5.0 (`decoder plan refused: continuous_video_envelope is invalid`). `continuous_candidates_from_workers(workers, rate)` (`crates/plurxd/src/http/hls/create.rs`) now offers only rows inside `continuous_avc_envelope_accepts` (made `pub`) at the source's measured rate, and none when the rate is unmeasured.

**Status:** clippy passed on the first commit, and the catalog tests pass. `processed_hdr10_setting_routes_a_non_dv_client_over_hls` needs a valid run of the second commit.

**Finish:**
1. Build and run the filtered tests plus clippy (`-D warnings`) on a GPU test node.
2. Open the PR, review, and run the fast lane.
3. Merge with:
   - `Regression-Test: crates/plurxd/src/http/tests.rs::processed_hdr10_setting_routes_a_non_dv_client_over_hls`
   - `Regression-Test: crates/plurxd/src/http/hls/tests/chunk_06.rs::continuous_catalog_offers_only_rows_inside_the_avc_level_envelope`
4. After deploy, device check: Avatar on the Pixel shows the HDR10-E badge and FEL/RPU; Animals' fallback starts.

## B. FEL→Profile 8.1 for DV displays — branch `claude/fel-p81-catalog-starts` (head 1eeb0542d, no PR, **never validly built**)

**Symptom:** Safari on a Studio Display, Avatar (P7 FEL), FEL re-encode on, gets the plain P7→8.1 conversion with "FEL: not used for this play" and no reason. The reconstructed 8.1 (FEL rebuilt, re-encoded, new RPU) is never reached, although #1199's physical test proves that path.

**Cause:**
- `prepare_processed_hdr10` (`crates/plurxd/src/transcode/manager/create.rs`) returned early for every `Copy { preserve_dolby_vision: true }`.
- The DV backend is captured only when there is no `candidate_context` or this is a processed trial. Catalog starts always carry a context.

**Branch changes:**
- A converted copy (`Copy { convert_dolby_vision: true }`) is offered the reconstructed 8.1 under `fel_reencode`, and declines are named. The trial keeps the Copy kind, which is what selects `DvDestination::Profile81`.
- `processed_candidate_digest` (`crates/plurxd/src/vodencode.rs`) accepts both destinations with distinct tags.
- `session_delivered_processing_shape` and `session_enhancement` no longer require a Transcode kind.
- Enhancement text: the reconstructed 8.1 lights FEL+RPU+DYN, and the plain 8.1 explains its RPU.

**Tests:**
- `chunk_05.rs::a_reconstructed_profile81_copy_is_described_as_reconstructed`
- `dv_runtime.rs::a_converted_dv_copy_is_offered_the_reconstructed_profile81`
- `enhancement.rs::reconstructed_p81_carries_fel_and_dyn`

**Main risk:** an Encode-route candidate on a Copy-kind request. Check every place that assumes Encode ⇒ Transcode: `candidates.rs` restore (~1399, ~1447), successor predicates at the top of `manager/create.rs`, link receipts. Alternative if fragile: an explicit destination field on `SessionRequest` (note `deny_unknown_fields`), with a Transcode-kind trial. Also check `trial.audio_delivery = None` for copies and that `encoded_client_compatible` accepts the 8.1 output for Safari's caps.

**Finish:**
1. Build and test; prove both new daemon tests fail on main.
2. Draft PR, adversarial review, fast lane.
3. Merge with the two daemon `Regression-Test:` lines.
4. Device check after deploy: Safari plus Studio Display, Avatar from 10 minutes in, expecting FEL+RPU+DYN and "FEL: full enhancement layer reconstructed". Also check there is no stutter.

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

- **Pixel 11 direct play of Animals fails** with ExoPlayer 4003 `ERROR_CODE_DECODING_FAILED`. The file is HEVC Main10 L5.0 HDR10, 3840×2160, 15 Mb/s, 24/1 nominal. Get a device logcat (MediaCodec/c2 errors) and compare with a title that plays.
- **Apple TV UI test overruns:** `testLiveFullscreenRevealMoreReportAndBackRetainSession` goes past its 3 minutes. It predates #1197; rerun it on the Mac simulator.
- **A second processed session is refused** while the first holds the processing slot ("DV processing needs a GPU slot and 15 CPU threads that other work holds"). After an Auto replacement the successor may lose HDR10-E. Check whether successors should reuse the prepared processed root (`capture_processed_root`).
