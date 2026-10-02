# Media compatibility catalog — the troublesome inputs playback must remember

**Status:** open — inventory complete; further work deferred at Paul's request
on 2026-10-02 while other features are building · **Written:** 2026-10-02 · **Source baseline:**
`4d725701901b50050d1425ba133f882d767ba40a`.

Companion to [PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) (running the
existing playback lab) and [PLAYBACK.md](../PLAYBACK.md) (delivery behavior).
This catalog answers which media conditions have required accommodation,
what evidence survives, and what a future compatibility suite must preserve.

There are **52 case families** below. A family can require several fixtures:
for example, an open GOP with leading pictures and a clean CRA are distinct
variants of the same cutting problem. These are not 52 damaged titles or
52 reproduced production incidents. Many sources are valid; some defects
were in Plurx's output, and some cases were discovered only by synthetic
tests or fuzzing.

| Area | Cases | Count |
|---|---|---:|
| [Time, duration and end-of-media](#1-time-duration-and-reaching-the-end) | MC-001–MC-009 | 9 |
| [Video structure and geometry](#2-video-structure-decoder-configuration-and-geometry) | MC-010–MC-022 | 13 |
| [Dolby Vision and HDR](#3-dolby-vision-hdr-and-malformed-metadata) | MC-023–MC-031 | 9 |
| [Audio and broadcast captions](#4-audio-broadcast-acquisition-and-captions) | MC-032–MC-038 | 7 |
| [Subtitles](#5-subtitle-structure-timing-and-extraction-completeness) | MC-039–MC-048 | 10 |
| [Container/parser boundaries](#6-container-and-elementary-stream-boundary-cases) | MC-049–MC-052 | 4 |

**Scope:** identification and coverage mapping. This document does not change
playback, add a harness, acquire private media, or claim fresh playback passes.
It preserves earlier incident conclusions as dated evidence, even where the
incident document's implementation status is older than the current code.

## How to read the evidence

| Mark | Meaning |
|---|---|
| **Incident** | A repository record describes affected media and observed failure. Its stated uncertainty still applies. |
| **Reproduction** | A record describes a controlled media experiment, without necessarily identifying a production incident. |
| **Synthetic** | Test construction or fuzzing establishes the case; no matching library incident was identified in this pass. |
| **External** | Another project's observation, recorded here as a candidate; not a demonstrated Plurx incident. |

The case text distinguishes **valid/compatibility**, **malformed**, and
**output defect**. An absent field alone is not proof of damaged media.
“Existing coverage” means source was located and inspected, not that it ran
in this inventory session. A generated bitstream, a JSON probe, a parser
mutation, an argument assertion and a physical player run prove different
things. Gaps below are the next evidence needed, not assertions that no
other test exists anywhere.

Stable `MC-001`–`MC-052` IDs should follow cases into fixture manifests and
regressions. Keep the IDs if titles or fixture filenames change.

## 1. Time, duration and reaching the end

Unless another source is linked, test names in this section refer to
[fragindex.rs][index] or [fmp4.rs][fmp4].

| ID | Condition and expected behavior | Evidence | Existing coverage and remaining gap |
|---|---|---|---|
| **MC-001** | **Audio outlasts video.** Valid unequal track lengths must not make a complete video index look truncated. Preserve the audio tail, divide it into bounded segments, and reach the actual end without a replay loop. | **Incident:** [content-analysis failures][duration-rca], reference film K/file 9; [4K investigation §5.7bis][stutter], reference episode I has about 45 s of trailing audio. Two consequences of the same input family. | Generated `audiotail-2397` in [VOD fixtures][vod-fixtures]; `content_analysis_ffmpeg_accepts_complete_video_with_a_longer_audio_tail`; `a_mixed_final_fragment_splits_its_trailing_audio_at_the_ceiling`. **Gap:** named end-of-film cases in the browser and native suites, including audio continuity and final watch position. |
| **MC-002** | **MKV video has no embedded duration.** Valid video lacks `duration_ts`, `duration`, and Matroska duration tags even though the container/subtitles have them. Obtain selected-video evidence rather than declaring damage or permanently withholding VOD. | **Incident:** [MKV duration RCA][mkv-duration], reference film F/file 6109. Packet measurements established an endpoint; rolling fallback exposed separate server bugs. | Packet-bound resolver and unit cases in [fragindex.rs][index]. **Gap:** a retained full MKV fixture that preserves the missing fields and exercises scan → indexing → play/seek/end. JSON packet tests do not establish this end to end. |
| **MC-003** | **Selected-video timing fields disagree.** Conflicting duration candidates must produce an explicit unverified/conflict outcome; do not choose whichever makes the index pass. | **Synthetic:** boundary cases added with the [MKV implementation][mkv-build]. No separate damaged title identified. | `content_analysis_probe_rejects_conflicting_selected_video_timing`; `mkv_hls_duration_conflict_compares_the_full_candidate_range`. **Gap:** encoded/container-mutated sample with independently measured ground truth. |
| **MC-004** | **Signed/reordered packet timestamps, missing DTS, or missing packet duration.** Negative PTS and presentation order need correct bounds; valid PTS need not have DTS. Missing required endpoint evidence must remain unverified. | **Synthetic** packet variants supporting [MC-002's repair][mkv-build]; not evidence that all variants occurred in file 6109. | `mkv_hls_duration_packet_bounds_use_signed_pts_and_maximum_packet_end`; `mkv_hls_duration_valid_pts_does_not_require_dts`; `mkv_hls_duration_tail_requires_every_selected_packet_duration`. **Gap:** media variants proving the same answers after real demuxing. |
| **MC-005** | **Fractional-frame GOP boundaries and seek rebasing.** Valid 23.976/29.97 fps media lands before rounded seek targets; audio selection changes mux timestamp offsets. Repeated seeks must preserve film time and synchronization. | **Reproduction:** [VOD M0 A1][vod-m0], 43 sampled boundaries; millisecond seek formatting and rebased DTS were not reliable landing identities. | Nine shapes in [VOD fixtures][vod-fixtures], including mixed clean/dirty GOPs; `two_generations_publish_one_continuous_film_timeline` and `joins_are_gapless_and_overlap_free`. **Gap:** expose these precise timing shapes in player seek cases with frame/audio alignment assertions. |
| **MC-006** | **Negative composition offsets from frame reordering.** Valid reordered pictures require signed version-1 `trun` offsets; unsigned conversion must not move frames into the future. | **Synthetic**, with the [B-frame timeline design][bframes] documenting the separate encoded-output qualification limit. | `a_negative_composition_offset_forces_a_version_1_trun`; [timeline cases][bframe-cases]. **Gap:** physical playback of copy-path variants; this row does not authorize B-frames in encoded VOD. |
| **MC-007** | **Audio precedes video, or a track joins later.** Preserve relative track timing and late tracks across fragment merging and seek rebasing. | **Synthetic:** constructed muxed fragments in [fmp4.rs][fmp4]. | `a_leading_audio_only_fragment_is_rebased_with_the_anchor_not_left_behind`; `a_track_that_joins_late_is_not_dropped_from_the_segment`. **Gap:** full source files and audible/visible synchronization after seeks. Delayed AC-3's distinct init failure is MC-033. |
| **MC-008** | **Truncated media or producer output.** A partial box and a cleanly terminated but too-short stream are separate variants. Reject incomplete indexes and bound early-end recovery; never infer a watched film from an early transport EOF. | **Synthetic** truncation tests; [early-end history][stutter] describes the player boundary. Source damage and producer failure remain different diagnoses. | `a_truncated_feed_yields_no_fragment_and_no_panic`; `a_truncated_pipe_is_not_an_index`; `a_pipe_that_stops_short_of_the_probed_duration_is_truncated`. **Gap:** actual truncated-file player cases, including near-end and mid-film stops, truthful errors and preserved progress. |
| **MC-009** | **A whole film is shorter than startup's buffer requirement.** Valid short media must publish at EOF and finish, without waiting for a buffer it can never fill. | **Synthetic:** [copy segmenter tests][copyseg]. | `a_film_shorter_than_the_gate_still_publishes_whole_at_eof`. **Gap:** very short audiovisual files driven through each actual client. |

---

## 2. Video structure, decoder configuration and geometry

The shared [Rust fixture generator][core-fixtures] owns open GOP,
closed GOP, clean CRA, alternate PPS and H.264 source shapes. These are
reusable assets, not yet one unified compatibility manifest.

| ID | Condition and expected behavior | Evidence | Existing coverage and remaining gap |
|---|---|---|---|
| **MC-010** | **Open GOPs with leading pictures.** A keyframe is not necessarily an independent start. Cut on proven clean boundaries where available and do not promise independence for dirty cuts. Include clean CRA and H.264 IDR controls. | **Incident + reproduction:** [4K boundary stutter][stutter] and [segmenter plan][segmenter]. | `every_cra_on_the_open_gop_fixture_is_dirty`, `a_cra_with_no_leading_pictures_is_clean`, `h264_only_idr_is_clean`, `merged_stream_decodes_bit_identical` in [fmp4.rs][fmp4]. The [browser corpus][cases] includes an HDR open-GOP source. **Gap:** per-case boundary-frame evidence on Apple and Android. |
| **MC-011** | **Dense high-bitrate media and long gaps between clean cuts.** Valid streams can create segments larger than expected or outlast client buffer limits. Preserve bytes and obey explicit admission/cutting limits. | **Reproduction:** [VOD M0 A2][vod-m0], segments up to 125.2 MB on approximately 143 Mb/s fixtures. | `dense-2397`, `dense-open-2397` in [VOD fixtures][vod-fixtures]; `a_source_with_no_clean_points_cuts_at_the_ceiling_and_counts_it` in [fmp4.rs][fmp4]. **Gap:** retain measured bitrate/segment-size assertions and player quota behavior, not merely a “4K” label. |
| **MC-012** | **Repeated in-band HEVC parameter sets equal the configuration record.** Valid repeated VPS/SPS/PPS contributed to the historical copy-path accommodation. Any removal must preserve decoded pixels and remain distinct from MC-013. | **Incident/reproduction history:** [4K investigation][stutter]; its early stripping account is qualified by the later [in-band RCA][inband]. | `hvc1_merge_promotes_then_removes_in_band_parameter_sets` in [fmp4.rs][fmp4]; original-header proof logic in [fragindex.rs][index]. **Gap:** named constant-header control paired with MC-013 through all delivery paths. |
| **MC-013** | **HEVC redefines parameter sets during the film.** Valid chunk-encoded source changes PPS values, sometimes reusing PPS 0. Deleting those definitions causes pink/green pictures. Preserve the changing definitions and correct pixels. | **Incident + pixel reproduction:** [in-band parameter sets][inband], four configurations and 301 transitions in the recorded film. | [hevc_census.rs][census] generates a two-pass x265 source: `a_redefined_pps_is_measured_retained_and_decodes_to_the_source_pixels`; `a_retained_film_still_indexes_as_vod_presentable`. **Gap:** durable original excerpt identity and real decoder acceptance under the delivered `hvc1`/`dvh1` labels; FFmpeg pixel equality alone does not settle that. |
| **MC-014** | **Minimal/empty `hvcC`, with needed HEVC parameter sets in samples.** Packaging may be valid for an in-band sample entry but cannot be emitted as complete out-of-band configuration without promotion. Refuse genuinely incomplete configuration. | **Incident population:** [HEVC admission RCA][hevc-admission] distinguishes 23-byte records from the incident film's complete 132-byte record. | `in_band_hevc_parameter_sets_fill_an_empty_dolby_vision_hvcc`; `incomplete_hevc_parameter_sets_are_refused_without_mutating_init`; [daemon][index] `emitted_hvc1_refuses_an_incomplete_decoder_configuration_without_a_probe_hint`. **Gap:** retained original minimal-config files and startup/seek checks. |
| **MC-015** | **Multiple HEVC sample descriptions or invalid description references.** A valid multi-entry stream can be unsupported by the current fast path; that differs from malformed or incomplete entries. Use typed fallback/refusal before publication. | **Synthetic:** [fixture constructors][core-fixtures] produce duplicate and distinct encoder-authored configurations. | [fmp4.rs][fmp4] `complete_multi_entry_hevc_is_a_typed_validated_structural_refusal`, `channel_playback_repair_validates_default_description_reference`; [copyseg.rs][copyseg] `validated_multi_entry_hevc_feed_requests_fallback_before_publication`. **Gap:** fixture integration through the final fallback player path. |
| **MC-016** | **`hev1`/`dvhe` packaging when the client only proved another sample entry.** Valid HEVC-in-MP4 needs packaging-specific admission. Copy-remux where supported rather than unnecessarily re-encoding video. | **Incident + native A/B:** [HEVC admission RCA][hevc-admission], reference film K/file 9. Identical video decoded with `hvc1` and failed with `hev1` on the investigation Mac; full Safari acceptance was not established by that experiment. | [HEVC qualification receipt][hevc-receipt]; source-tag parser tests in [scan/probe.rs][scan]. **Gap:** preserve the four A/B variants outside their historical temporary directory; exercise SDR, HDR10, DV P5 and P8 separately in real clients. |
| **MC-017** | **Attached cover art appears as a video stream.** Valid multi-stream media must select the playable video consistently for probe, timing, indexing and playback. | **Incident context:** attached MJPEG stream in [reference film K][hevc-admission]; first-stream ordering variants are synthetic. | [scan/probe.rs][scan] `skips_attached_cover_art`, `video_codec_tag_uses_first_non_attached_video`; [fragindex.rs][index] `content_analysis_probe_refuses_attached_picture_selected_by_ffmpeg`. **Gap:** a real generated container with cover art before the movie, tested end to end. |
| **MC-018** | **Legacy MPEG-4 Part 2 / XVID on VideoToolbox decode.** Codec/backend incompatibility can corrupt pictures or report missing decoded frames. Use an appropriate decoder while retaining supported hardware encoding. | **Incident, mechanism partly unproved:** [AVI review][avi], issue 913, Advanced Simple Profile/XVID, 624×352 at 25 fps. | [Decoder baseline][decoder-baseline] has a generated Simple Profile AVI control and explicitly says the original incident media was unavailable. [Captured diagnostics][legacy-stderr] survive. **Gap:** obtain the original or an ASP/XVID reproducer and compare decoded pictures/seeks; the Simple Profile control is not equivalent. |
| **MC-019** | **True interlaced video.** Valid 480i/576i/1080i, top/bottom field order and HDR renderer variants must be deinterlaced in the right order; output cadence and client limits must agree. | **Reproduction:** [interlace investigation][interlace], combed frames had been encoded as progressive. | [Interlace fixture script][interlace-fixtures]; [transcode tests][transcode] `interlaced_file_uses_send_frame_bwdif_before_scale`, `interlaced_special_renderers_apply_bwdif_without_breaking_dolby_vision_order`. **Gap:** actual hardware/client picture and cadence qualification for each admitted graph. |
| **MC-020** | **Progressive pictures incorrectly flagged as interlaced.** Wrong metadata must not force damaging deinterlacing when measured frames contradict it. | **Synthetic/reproduction:** [interlace plan][interlace]. | [decode_facts.rs][decode-facts] `descriptor_bound_idet_overrules_misflag_and_confirms_interlace`; generated misflagged file and image comparison in [interlace fixtures][interlace-fixtures]. **Gap:** retain the sample as a reusable corpus asset and add client quality checks. |
| **MC-021** | **Hard telecine or mixed cadence.** Valid pulldown is not interchangeable with ordinary interlace. Record the cadence and intended handling without claiming inverse-telecine support from a flag. | **Synthetic:** [interlace plan][interlace] and its generated 3:2 pulldown input. | [Interlace script][interlace-fixtures] creates `hard-telecine.mkv` but does not assert its output behavior; its temporary directory is removed on exit. **Gap:** explicit acceptance contract, durable generator entry, and frame/cadence assertions. |
| **MC-022** | **Non-square pixels, unusual aspect ratios and rotation.** Valid coded dimensions are not display dimensions. Preserve upright geometry through copy, scale and presentation. | **Incident report unresolved:** [Live TV aspect investigation][live-surround], 704×480, SAR 40:33, DAR 16:9; server output was correct, client squish was not localized. Rotation is a synthetic sibling, not part of that incident. | [Display geometry tests][geometry]; `scripts/live-tv-hardware` records SAR/DAR. **Gap:** identify/replay the affected client and use a geometry chart so correct metadata cannot hide a distorted displayed image. |

---

## 3. Dolby Vision, HDR and malformed metadata

Keep source facts separate from output claims. A bad output record does not
make the original disc or WEB-DL corrupt. The small RPU fixtures below test
the converter; injecting them after FFmpeg's muxer does not test FFmpeg's
own `dovi_rpu` filter.

| ID | Condition and expected behavior | Evidence | Existing coverage and remaining gap |
|---|---|---|---|
| **MC-023** | **Dolby payload stripped but its record/brand survives.** Output defect: plain HEVC still declares DV P7, or a stale Dolby brand survives without a record. Remove misleading signaling while retaining correct HDR10 metadata. | **Incident/reproduction:** [4K investigation §5.3][stutter] records hardware/software decoder selection changing when the false declaration disappeared. | [copyseg.rs][copyseg] `the_recovery_path_removes_a_stale_dolby_vision_record_and_its_brand`, `a_stripping_session_writes_an_init_with_no_dolby_vision_claim`; [fmp4.rs][fmp4] `a_stripped_stream_still_gets_its_hdr10_static_metadata`. **Gap:** named strip/preserve controls with actual client decoder and dynamic-range evidence. |
| **MC-024** | **Profile 7 dual-layer content delivered to a single-layer client.** Valid MEL/FEL sources need the supported conversion/base-layer route. A conversion with absent or unreadable RPUs must not claim converted DV. | **Incident family:** [DV delivery findings][dv-findings]; later conversion work in [M5B status][dv-conversion-status]. The old findings predate conversion and are not current policy. | [dvconvert.rs][dvconvert] `a_profile_7_rpu_converts_to_profile_8_and_stays_parseable`; [fragindex.rs][index] `a_converting_pass_over_a_stream_with_no_rpus_is_not_an_index`; injected RPU source helpers. **Gap:** real MEL/FEL files with declared conversion expectations and physical DV confirmation. |
| **MC-025** | **Dolby Vision Profile 5 needs a correct, qualified renderer.** Valid source has no ordinary HDR10 base and needs DV-aware reshaping. The historical Vulkan/libplacebo route produced neon-green frames on the production Intel driver and could leave Apple TV black. | **Incident recorded in commit `8a4fff75f`:** replacement with the Dolby-aware `tonemapx` route; [DV delivery findings][dv-findings] provide the compatibility context. This is a renderer/backend failure, not evidence of corrupt P5 media. | [transcode tests][transcode] `profile5_tonemap_applies_dolby_vision_metadata_before_sdr_mapping`; renderer probes in [ffmpeg.rs][ffmpeg]. **Gap:** retained P5 image reference and real decoded-output comparison; argument order is insufficient. |
| **MC-026** | **HDR static metadata is in-band, absent, or lost in filtering.** Valid HDR may lack MaxCLL/MDCV or place it on frames. Preserve known metadata and apply an explicit fallback when absent; do not silently change the tone-map reference. | **Reproduction:** [tone-map investigation][tonemap] locates metadata loss at `zscale` on the measured build, correcting an earlier hardware-download hypothesis. | [fmp4.rs][fmp4] `hdr10_static_metadata_is_promoted_into_hvcc_once`; [scan/probe.rs][scan] `parses_stream_luminance_metadata_in_source_units`, `frame_luminance_upgrades_an_hdr_stream_observed_without_metadata`. **Gap:** retained absent/1000/4000-nit image cases across admitted software/GPU routes, plus HLG as a distinct control. |
| **MC-027** | **Malformed prefix SEI rejected by FFmpeg's DV strip filter.** The SEI payload length can be bad even when the RPU is fine. Expected disposition needs reproduction; the proposed unit-type strip must not be counted as a proven repair. | **External:** [DV strip trial plan][strip-trial] records Silo's observation and a still-required Plurx M0 experiment. | No completed Plurx media acceptance is established by that plan. **Gap:** corrupted type-39 SEI source, healthy control, captured diagnostics and comparison of both strip routes. Keep separate from RPU-parser failures below. |
| **MC-028** | **Unreadable or oversized Dolby RPU.** Malformed metadata must return a bounded typed refusal, leave no partial conversion output, and preserve the server process. | **Synthetic:** [converter tests][dvconvert]. | `an_unreadable_rpu_refuses_rather_than_passing_through`; `an_rpu_larger_than_any_real_one_is_refused_by_size_before_parsing`; `a_refusal_leaves_the_output_buffer_empty`. **Gap:** source-container/HTTP/player error cases, including a bad RPU encountered after playback starts. |
| **MC-029** | **RPU claims an enormous extension-block count.** Malformed count previously requested about 25.8 GB and could abort the process. Refuse before allocation. | **Fuzz reproduction:** [dvconvert.rs][dvconvert] records the September 24 finding. | [Hostile count seed][rpu-count]; `an_rpu_claiming_billions_of_extension_blocks_is_refused_without_allocating`. **Gap:** bounded worker/session failure check around the existing parser regression. |
| **MC-030** | **RPU requests unsupported linear interpolation.** Unsupported syntax reached an upstream `unimplemented!()`; it must return a refusal rather than panic. Do not label every unsupported curve as corrupt. | **Fuzz reproduction:** [converter][dvconvert], September 24. | [Linear interpolation seed][rpu-curve]; `an_rpu_with_a_linear_interpolation_curve_is_refused_not_panicked_on`. **Gap:** source-container/session integration. |
| **MC-031** | **RPU extension block has an unrecognized length.** Invalid/unsupported level 8/9/10 layouts must be refused before the former validation panic. | **Fuzz reproduction:** [converter][dvconvert], September 25. | [Level-8 length seed][rpu-length]; `an_rpu_with_an_unknown_extension_block_length_is_refused_not_panicked_on`. **Gap:** source-container/session integration and representative level-9/10 variants. |

---

## 4. Audio, broadcast acquisition and captions

Broadcast recordings belong in the file corpus too. Starting from a finite
capture and joining an ongoing broadcast have different acceptance criteria.

| ID | Condition and expected behavior | Evidence | Existing coverage and remaining gap |
|---|---|---|---|
| **MC-032** | **Immersive AC-4 decodes to 12-channel 7.1.4.** Valid layout exceeds the selected AAC encoder/output contract. Select a supported channel layout without unnecessarily transcoding video. | **Incident:** [ATSC audio RCA][atsc], channel 133.1; supplied logs, no retained capture in that investigation. | [ATSC tests][atsc-tests] `atsc_aac_conversion_bounds_unknown_and_immersive_channels`, `atsc_encoded_channel_layouts_follow_the_ceiling`. **Gap:** a real immersive AC-4 capture and decoded channel-content assertions; channel-count JSON is not an audio fixture. |
| **MC-033** | **AC-3 starts about two seconds after video.** Valid delayed audio can be unknown to a short probe; short fMP4 muxing can exit successfully yet leave an unreadable init. Require decodable output and preserve compatible video. | **Incident + exact-version reproduction:** [ATSC audio RCA][atsc], channel 128.1; `Cannot write moov atom before AC3 packets`, then `invalid size 0 in stsd`. | [ATSC tests][atsc-tests] `atsc_delayed_ac3_produces_decodable_live_fmp4` generates delayed audio; `atsc_supplied_ac3_capture_produces_decodable_live_fmp4` is opt-in. **Gap:** locate the checksum-identified capture and add real client startup/sync evidence. |
| **MC-034** | **AC-4 capture begins between global random-access frames.** A finite valid capture may contain packets but no state from which a fresh decoder can produce audio. Unknown channels/rate must not become zero-valued output settings or a blanket unsupported-channel verdict. | **Incident + packet analysis:** [ATSC audio RCA][atsc], channel 103.1; no global random-access flag in 175 English or 171 Spanish packets. | `atsc_zero_probe_values_are_unknown_not_output_parameters`, `atsc_incomplete_audio_facts_cannot_select_copy` in [ATSC tests][atsc-tests]. **Gap:** retain both the non-initializing prefix and a longer continuous capture that reaches the first random-access frame. Live recovery remains unproved by prefix replay. |
| **MC-035** | **A/53 captions on MPEG-2 input to H.264 VideoToolbox.** Valid caption side data triggered encoder SEI insertion failure after some output. A caption-free control misses it. | **Incident + generated hardware reproduction:** [ATSC VideoToolbox RCA][vt-captions]. | [Hardware test][vt-tests] `live_tv_videotoolbox_atsc1_publishes_decodable_segments`, ignored by ordinary runs, covers 480i/720p/1080i shapes. **Gap:** actual Mac/client evidence per supported build, with explicit caption-preservation/loss assertions. |
| **MC-036** | **The same caption-bearing bytes in DVR/VOD files.** The Live TV accommodation does not automatically fix the file transcode route or its cached-output identity. | **Supplied reproduction/open follow-up:** [VideoToolbox file captions][vt-vod]. | The linked follow-up requires a production VOD/DVR regression; the live-only test cannot close it. **Gap:** preserved captioned file, file-route test and explicit caption policy. |
| **MC-037** | **Multiple audio tracks with different languages, descriptions, codecs and layouts.** Valid broadcast/media variants require semantic track selection, correct stream/PID mapping and supported surround delivery. | **Incident/compatibility:** [Live TV direct-play and surround investigation][live-surround]. | [ATSC tests][atsc-tests] `atsc_described_audio_never_beats_the_main_track`, `atsc_selected_track_is_mapped_by_pid`, `atsc_copyable_simulcast_track_beats_the_first_track_needing_conversion`; [browser corpus][cases] has two-track switching. **Gap:** distinctive channel tones/spoken track IDs to prove the correct audible track and mix on devices. |
| **MC-038** | **Identical media described differently by FFprobe versions.** Compatibility, not file damage: missing/present TrueHD or E-AC-3 Atmos profile, added defaults, MIME labels or chapter fields can falsely signal source replacement. Real source changes must still fail comparison. | **Incident + paired-report reproduction:** [TCL probe RCA][probe-rca], reference film G/file 120; an unselected E-AC-3 track blocked the whole film. | [ffmpeg.rs][ffmpeg] `held_probe_comparison_admits_the_legacy_eac3_atmos_profile_omission`, `held_probe_comparison_accepts_measured_ffprobe_defaults_but_not_nondefaults`, `held_probe_comparison_accepts_empty_chapters_but_detects_chapter_changes`; [replay tool][probe-replay]. **Gap:** checksum-bound source plus both real probe documents in a versioned fixture set. Missing stored probe is a catalog-state variant, not damaged media. |

---

## 5. Subtitle structure, timing and extraction completeness

Valid silence, unavailable preparation, malformed subtitles and a failed
extractor need different verdicts. A successfully decoded video does not
prove subtitle correctness.

| ID | Condition and expected behavior | Evidence | Existing coverage and remaining gap |
|---|---|---|---|
| **MC-039** | **PGS subtitles embedded in a very large remux.** Valid sparse tracks may require expensive demuxing. A start must not wait unboundedly for a whole-film subtitle extraction. | **Incident:** [PGS start-path RCA][pgs-start], 79.5 GB source read before the selected burn could start. | Stored-track and bounded-join work is mapped by the RCA; [subtitle_ride_along.rs][ride] has real generated multi-track extraction tests. **Gap:** corpus entry preserving sparse access/large-file behavior and a timed player start; a tiny SUP parser fixture cannot reproduce this cost. |
| **MC-040** | **PGS Epoch Continue (`0xC0`) and acquisition boundaries.** Valid multi-clip state must be parsed and prior epoch caches discarded correctly. | **Parser audit/reproduction:** [PGS feasibility][pgs-feasibility]; upstream candidate omitted this state. | [Parser prerequisites][pgs-prereqs] `epoch_continue_is_retained_by_the_owned_pcs_adapter`; [PGS tests][pgs] `epoch_continue_does_not_reuse_the_previous_epoch_cache`. **Gap:** authored multi-clip track played visibly across the transition. |
| **MC-041** | **PGS palette/object reuse, fragmented objects and cropping.** Valid compositions may update only one component, assemble objects across packets, and place multiple cropped objects. Preserve authored pixels, order and clear events. | **Synthetic/reference comparison:** [PGS feasibility][pgs-feasibility]. | [PGS tests][pgs] `palette_update_reuses_the_previous_object`, `fragmented_object_reassembles_before_strict_decode`, `cropped_composition_hashes_multiple_objects_in_authored_order`, `hd_colored_cue_matches_the_ffmpeg_bt709_rgba_fixture`. **Gap:** reusable complete tracks and screenshot/timing checks in all overlay clients. |
| **MC-042** | **PGS timestamp wrap, duplicate times and backward jumps.** Valid 32-bit 90 kHz wrap must unwrap; duplicate timestamps retain ordering; ordinary backwards time is rejected. | **Synthetic:** [PGS feasibility][pgs-feasibility]. | [PGS tests][pgs] `pts_wrap_is_unwrapped_without_accepting_an_ordinary_backwards_jump`, `duplicate_timestamps_are_measured_but_backwards_time_is_rejected`; [parser prerequisites][pgs-prereqs]. **Gap:** muxed file/overlay test near the wrap with seeks and cue visibility. |
| **MC-043** | **Malformed PGS RLE, missing fragments, invalid references or bounds.** Do not turn incomplete pictures into transparent padding or allocate from unchecked geometry. | **Parser audit + mutation reproduction:** [PGS feasibility][pgs-feasibility]. | [PGS tests][pgs] `permissive_candidate_rle_is_rejected_by_the_adapter`, `truncated_display_set_fails_preflight`, `deterministic_mutation_corpus_never_panics`; [SUP fuzz seeds][sup-corpus]. **Gap:** route/client refusal cases that leave the playable video and other subtitle tracks usable. |
| **MC-044** | **Extraction produces a structurally valid but incomplete subtitle track.** Packet-boundary truncation can end on a legal PGS END; a text decoder failure can leave matching output counts. Verify completeness independently and isolate the failing track. | **Reproduction:** [PGS start RCA §6][pgs-start] and [cluster extraction experiments][subtitle-extraction]. | [Ride-along tests][ride] `a_corrupted_track_is_malformed_and_the_index_is_untouched`, `corrupt_text_packet_decoder_error_vetoes_equal_output_counts_for_only_its_stream`. **Gap:** promote good/bad sibling-track sources to the player corpus and test selection of each. |
| **MC-045** | **Oversized PGS canvas, display set or accumulated objects.** Malformed/excessive inputs must stay within memory, byte and count limits without rejecting ordinary long tracks merely for duration. | **Audit + synthetic:** [PGS feasibility][pgs-feasibility]. | [PGS tests][pgs] `oversized_canvas_is_rejected_before_bitmap_decode`, `preflight_bounds_segments_before_candidate_assembly`, `streamed_output_bounds_each_composition_without_rejecting_a_long_track`. **Gap:** controlled end-to-end resource/refusal evidence. |
| **MC-046** | **Empty subtitle track, long silence, sparse first cue or cue spanning a window.** Valid absence must not be reported as failed extraction; later and spanning cues must still appear. | **Synthetic/reproduction:** [subtitle range status][subtitle-ranges-status] and [reliability assessment][subtitle-reliability]. | [Ride-along][ride] `a_track_with_no_cues_is_empty`; [subtitles.rs][subtitles] `empty_stored_text_publishes_webvtt_without_flight`; [range tests][ranges] `absolute_window_keeps_spanning_cue_and_sparse_first_cue`. **Gap:** player-visible cue/no-cue timeline, including a seek into silence and a later first cue. |
| **MC-047** | **Nonzero source origin, preceding-keyframe seeks and version-dependent subtitle timestamp rebasing.** Valid cues must stay on absolute film time; guessing from the first sparse cue is unreliable. | **Reproduction/source fixes:** [range status][subtitle-ranges-status]; commits `820de81dd` and `ad7dda806`. | [range tests][ranges] `indexed_late_window_matches_full_scan_with_nonzero_source_start` generates a source with a 7.5 s offset; [subtitles.rs][subtitles] `a_window_is_normalized_to_absolute_time_whatever_ffmpeg_emitted`; [media-origin logic][media-origin]. **Gap:** client cue synchronization after forward/backward seeks across engine versions. |
| **MC-048** | **Styled/positioned ASS and preservation through stored sidecars.** Valid text carries more than dialogue and timing. The burn path must preserve styling/position; a plain WebVTT conversion is not an equivalent visual reference. | **Generated reproduction:** [cluster extraction experiments][subtitle-extraction]. | [subtitles.rs][subtitles] `styled_ass_stored_matroska_burn_matches_source_cues_and_style`; [ride-along][ride] `ass_positioning_and_styling_burn_identically_from_stored_mks_and_source`. **Gap:** reusable sample plus image comparison on the actual chosen delivery route. |

---

## 6. Container and elementary-stream boundary cases

These cases mostly describe the bytes Plurx reads **after** a demuxer or
muxer. They are essential robustness tests, but should not be presented as
four additional reports of broken library files.

| ID | Condition and expected behavior | Evidence | Existing coverage and remaining gap |
|---|---|---|---|
| **MC-049** | **Malformed fMP4 box sizes, impossible sample counts or incomplete headers.** Refuse overflow/resource abuse and incomplete structure without panic or publishing partial media. | **Synthetic/fuzz corpus:** [fMP4 seeds][fmp4-corpus]. | [fmp4.rs][fmp4] `a_trun_cannot_declare_four_billion_samples`, `a_box_that_would_wrap_the_cursor_is_refused`, `an_oversized_nested_box_is_malformed_before_the_resource_ceiling`; [copyseg.rs][copyseg] `an_unparseable_moov_is_reader_failure_not_fallback`. **Gap:** parser outcome → session error/recovery integration. |
| **MC-050** | **Legal MP4 defaults and large-size box headers.** Valid `trex`/`tfhd` defaults and 64-bit headers must survive reading and metadata rewriting. Misreading defaults can yield zero durations; rewriting at the wrong offset corrupts box lengths. | **Synthetic/regression history:** [segmenter plan][segmenter] and [fMP4 tests][fmp4]. | `trex_defaults_are_the_bottom_rung_of_the_ladder`; `default_sized_truns_are_expanded_and_unrepresentable_shapes_are_refused`; `a_largesize_record_box_is_not_renamed_over_its_own_length`. **Gap:** persist representative muxed variants and pass them through the full delivery path. |
| **MC-051** | **Invalid length-prefixed HEVC samples.** A declared NAL length can overrun the sample; legal one-/two-/four-byte prefixes need correct rewriting. Reject bad structure without partial output. | **Synthetic/fuzz:** [RPU rewrite seeds][rpu-corpus]. | [dvconvert.rs][dvconvert] `every_legal_nal_length_width_is_read_and_an_illegal_one_is_refused`, `a_sample_whose_lengths_do_not_add_up_is_refused`. **Gap:** container-level sample corruption and session containment test. |
| **MC-052** | **Chapters become an unwanted QuickTime text track; a promised media track is missing.** Valid chapter-bearing sources must not acquire an extra delivery track that the player rejects. Separately, a promised track missing from malformed output is a failure. Distinguish safe fallback from terminal failure without silently discarding media. | **Incident + reproduction:** [Safari chapter-track failure][stutter] records FFmpeg’s MP4 muxer turning chapters into a third `text` track, causing `MEDIA_ERR_DECODE` and unnecessary 4K→1080p fallback; `-map_chapters -1` prevents the extra track. The missing-video variant is **synthetic**. | [copyseg.rs][copyseg] `a_track_this_path_never_asked_for_takes_the_fallback` uses `source_with_chapters()`; [fMP4][fmp4] `a_repositioned_generation_needs_a_video_track_to_anchor_against`. **Gap:** promote the chapter-bearing fixture into real Safari/player coverage and exercise safe fallback versus typed terminal failure. |

## The corpus infrastructure already exists in several places

| Existing asset | What it proves | What it does not prove |
|---|---|---|
| [Playback lab][lab] and [case manifest][cases] | Generated source → isolated server → actual web player, including codecs, transports, seeks and switches. | The 52 families above are not all selectable cases in that manifest; ordinary success thresholds are not sufficient for expected refusals or pixel corruption. |
| [Core generators][core-fixtures] and [VOD probe fixtures][vod-fixtures] | Real encoded GOP/timing/audio variants, reproducible on a supported FFmpeg build. | Every generated file is not necessarily probed for every defining property; generator availability is not a current passing result. |
| [HEVC census reproduction][census] | Changed-PPS source, production copy, decoded pixel comparison and index behavior. | Native/browser decoder handling of the delivered sample-entry labels. |
| [ATSC audio tests][atsc-tests] and [VideoToolbox test][vt-tests] | Delayed-audio generation, decodable server output, optional capture/hardware replay. | Ordinary tests do not run ignored capture/hardware checks; finite replay is not a live-tuner acceptance result. |
| [Interlace matrix][interlace-fixtures] | Generated TFF/BFF/misflagged inputs, cadence and image checks. | Script-local files are deleted; telecine is generated without an output verdict. |
| [PGS parser][pgs], [SUP corpus][sup-corpus], [RPU seeds][rpu-corpus], [fMP4 seeds][fmp4-corpus] | Deterministic malformed-input and state-transition coverage. | Most are elementary bytes, not complete playable media files. |
| [Subtitle extraction][ride] and [range tests][ranges] | Real extraction, style/timing comparisons, bad sibling-track isolation. | A visible cue in each shipping player at the intended time. |
| [Decoder baseline][decoder-baseline] | Historical generated-source hashes, output probes and frame hashes. | The original XVID incident, current hardware or current branch acceptance. |

Keep the ordinary H.264/AAC, VP9/Opus, HEVC HDR, multi-track MKV and
MPEG-4/MP3 fixtures as controls. They are valuable compatibility baselines,
but an unsupported codec is not automatically broken media.

## Samples to recover or promote first

| Order | Cases | Concrete next step |
|---|---|---|
| 1 | MC-013, MC-012 | Promote the generated changing-PPS reproduction and constant-header control into the playback lab; retain the real incident excerpt with a checksum; compare colors at a parameter-set transition on each client. |
| 2 | MC-001–MC-004 | Promote audio-tail and missing-duration variants into scan/index/play/end cases. Include a genuinely truncated control so accommodating valid metadata cannot conceal incomplete media. |
| 3 | MC-016, MC-014 | Recreate and retain the label-only HEVC A/B files, plus a separate minimal-`hvcC` source; preserve SDR/HDR/DV variants and verify frames rather than startup events alone. |
| 4 | MC-033–MC-036 | Promote delayed AC-3 and captioned MPEG-2 generators; recover the two checksum-identified ATSC captures. Obtain a longer AC-4 capture with a random-access frame and cover DVR/VOD separately. |
| 5 | MC-018 | Recover issue 913's original XVID/ASP file or make a reproducer that fails on the implicated decoder. Preserve the original diagnostics alongside it. |
| 6 | MC-039, MC-044, MC-046–MC-048 | Promote extraction fixtures into visible subtitle cases: large/sparse PGS, bad sibling track, long silence, nonzero origin and styled ASS. |
| 7 | MC-010–MC-011, MC-019–MC-022 | Connect the existing GOP, density and interlace generators to targeted player cases; obtain the client identity for the unresolved anamorphic report. |
| 8 | MC-027–MC-031, MC-049–MC-052 | Keep fast parser regressions and add bounded session-failure cases. MC-027 first needs its missing Plurx reproduction; do not call an external diagnosis a repaired local incident. |

The [ATSC RCA][atsc] supplies two durable content identities even though
this inventory did not locate the private files:

| Capture | SHA-256 | Cases |
|---|---|---|
| `cap-128.1.ts` | `4c04c11bf47cfe0c11ab9111c4ff6fb0c62d234a7697395bbba3f82aa083ba31` | MC-033 |
| `prefix-103.1.ts` | `d7cebb3f188098e533bf6a2718cd8527ddbef3b55876b4e362934ef662550b6f` | MC-034 |

Other historical excerpts and temporary paths are **leads**, not verified
available assets. Private originals should remain outside Git; the catalog
should retain a neutral ID, checksum, acquisition location under the user's
control, and the smallest reproducer that preserves the condition.

## What each future fixture must record

1. **Identity and provenance:** case ID, source/generator, checksum, engine
   version, original incident and any transformations used to minimize it.
2. **Condition check:** an independent probe/parser assertion that the
   defining defect or unusual property remains present. Remuxing can repair
   timestamps/configuration; cutting can remove the only failing transition.
3. **Action:** cold start, resume, seek across the problem, track switch,
   subtitle selection, or play through the actual end. Observe the defect's
   location; the lab's short steady window is insufficient for a late PPS
   change, sparse subtitle or trailing audio.
4. **Expected disposition:** direct/copy/encode, an explicitly allowed
   recovery, or bounded refusal. Record the actual route so a rescue
   transcode cannot silently pass a copy-compatibility test.
5. **Observable result:** presented frames, reference pixels where needed,
   audible track/channel identity, A/V and subtitle timing, correct end,
   bounded memory/process lifetime, and useful errors as applicable.
6. **Coverage level:** metadata/unit, real server bytes, browser playback,
   or physical native playback; include exact source/client/engine versions.
   Missing tools, missing media and unrun device cases remain unverified.

## Scope limits and adjacent failures

This pass read the documentation index, searched streaming/client/performance
and archive records, inspected source/tests and regression receipts, and
searched the locally available history. The checkout has a non-shallow
history of 8,875 reachable commits at the recorded baseline. This was a
targeted historical search, not a line-by-line review of every commit.
The catalog does not claim access to every past chat, external issue body,
private original, or deployed filesystem. Older plans are evidence of a
case, not proof that a proposed test exists or that a repair shipped.

The following belong in neighboring harness suites. They can be exercised
with this corpus but should not inflate the count of media conditions:

- Network cliffs, slow storage, missing mounts, resource exhaustion and
  quorum loss: transport/cluster fault injection.
- Playlist publication holds, sliding-window freshness, stale segment URLs,
  recovery ownership and decoder-capability catalog read budgets:
  application/protocol defects. MC-002 exposed some of these; it did not
  cause all of them.
- Missing or stale catalog probes, stale indexes, source replacement and
  damaged generated caches: source identity/preparation/storage tests. The
  paired-reporter problem in MC-038 is retained because the input reports
  themselves are the reproducer.
- Subtitle selection state, UI overlays, controller cancellation and
  prepared-handoff state: client lifecycle tests, with the subtitle media
  here available as inputs.

No concrete local incident was established in this pass for **missing MKV
cues, broken MP4 seek tables, variable-frame-rate drift, generic missing
PTS, or random transport packet loss** as additional independent families.
These remain discovery candidates, not invented historical failures. MC-004
and MC-008 cover specific timestamp/truncation contracts, not every member
of those broader categories.

## Inventory verification

This is a documentation change. On 2026-10-02, all four documentation-index
checks passed. A separate inventory check verified 52 unique sequential
case IDs, all 59 reference-link destinations, and the named Rust test
symbols against the checked-out source. `git diff --check` passed.
No Rust/player test was executed as part of the inventory, and no
production media or service was changed.

```bash
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

[index]: ../../crates/plurxd/src/fragindex.rs
[fmp4]: ../../crates/plurx-core/src/fmp4.rs
[copyseg]: ../../crates/plurxd/src/copyseg.rs
[duration-rca]: CONTENT-ANALYSIS-FAILURES-RCA-AND-FIX.md
[mkv-duration]: MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md
[mkv-build]: MKV-DURATION-AND-SLIDING-HLS-IMPLEMENTATION.md
[stutter]: STUTTER-4K.md
[vod-fixtures]: ../../scripts/vod-probe-fixtures.sh
[vod-m0]: VOD-M0-ISSUES.md
[bframes]: VOD-BFRAMES-TIMELINE-DESIGN.md
[bframe-cases]: ../../tests/playback/vod-bframes-timeline-cases.json
[segmenter]: SEGMENTER-PLAN.md
[core-fixtures]: ../../crates/plurx-core/src/testfixtures.rs
[cases]: ../../tests/playback/cases.json
[inband]: HEVC-IN-BAND-PARAMETER-SETS.md
[census]: ../../crates/plurxd/src/hevc_census.rs
[hevc-admission]: SAFARI-DIAGNOSIS-AND-FIX.md
[hevc-receipt]: ../evidence/hevc-sample-entry-qualification-2026-09-16.md
[scan]: ../../crates/plurx-core/src/scan/probe.rs
[avi]: AVI_VIDEOTOOLBOX_REVIEW_DECISION.md
[decoder-baseline]: ../../tests/playback/decoder-media-baseline-2026-09-05.toml
[legacy-stderr]: ../../tests/playback/decoder-health/issue-913-legacy.stderr
[interlace]: INTERLACE-IN-THE-MEDIA-CONTRACT.md
[interlace-fixtures]: ../../tests/playback/interlace-fixtures.sh
[transcode]: ../../crates/plurx-core/src/transcode/mod.rs
[decode-facts]: ../../crates/plurxd/src/decode_facts.rs
[live-surround]: LIVE-TV-DIRECT-PLAY-AND-SURROUND.md
[geometry]: ../../crates/plurx-core/src/playback/geometry.rs
[dv-findings]: DV-DELIVERY-FINDINGS.md
[dv-conversion-status]: M5B_STATUS.md
[dvconvert]: ../../crates/plurx-core/src/transcode/dvconvert.rs
[ffmpeg]: ../../crates/plurxd/src/ffmpeg.rs
[tonemap]: TONE-MAP-CHAIN-CORRECTIONS.md
[strip-trial]: DV-STRIP-TRIAL-PROBE.md
[rpu-count]: ../../tests/playback/dv-p7-rpu-hostile-ext-blocks.hex
[rpu-curve]: ../../tests/playback/dv-p7-rpu-hostile-linear-interp.hex
[rpu-length]: ../../tests/playback/dv-p7-rpu-hostile-level8-length.hex
[atsc]: ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md
[atsc-tests]: ../../crates/plurxd/src/live_tv/atsc_audio_tests.rs
[vt-captions]: LIVE-TV-VIDEOTOOLBOX-ATSC1-ROOT-CAUSE-AND-FIX.md
[vt-tests]: ../../crates/plurxd/src/live_tv/videotoolbox_tests.rs
[vt-vod]: VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md
[probe-rca]: TCL-PROBE-MISMATCH-RCA-AND-FIX.md
[probe-replay]: ../evidence/probe-compatibility-replay.py
[pgs-start]: ../clients/PGS-SUBTITLE-START-PATH-RCA-AND-PLAN.md
[pgs-feasibility]: ../clients/PGS-OVERLAY-M0-FEASIBILITY.md
[pgs-prereqs]: ../../crates/plurx-pgs/tests/parser_prerequisites.rs
[pgs]: ../../crates/plurx-pgs/src/lib.rs
[ride]: ../../crates/plurxd/src/subtitle_ride_along.rs
[sup-corpus]: ../../fuzz/corpus/inspect_sup/
[subtitle-extraction]: ../clients/SUBTITLE-CLUSTER-EXTRACTION-PLAN.md
[subtitle-ranges-status]: ../clients/PARALLEL-SUBTITLE-RANGES-STATUS.md
[subtitle-reliability]: ../clients/SUBTITLE-RELIABILITY-ASSESSMENT.md
[subtitles]: ../../crates/plurxd/src/subtitles.rs
[ranges]: ../../crates/plurxd/src/subtitle_ranges.rs
[media-origin]: ../../crates/plurxd/src/transcode/media_origin.rs
[fmp4-corpus]: ../../fuzz/parsers/corpus/fmp4_reader/
[rpu-corpus]: ../../fuzz/parsers/corpus/rpu_rewrite/
[lab]: ../../scripts/playback-lab
