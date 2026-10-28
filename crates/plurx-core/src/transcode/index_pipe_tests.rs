use super::*;

use crate::domain::MediaFile;

fn hevc_dv() -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: "/library/film.mkv".into(),
        size: 1,
        mtime: 1,
        duration_ms: Some(600_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main 10".into()),
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: Some("dolby_vision".into()),
        hdr_format: Some("Profile 5".into()),
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(60_000_000),
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: crate::domain::DolbyVisionFacts::default(),
    }
}

#[test]
fn retained_parameter_sets_keep_hls_sample_entries_and_preserve_needed_updates() {
    for (hdr, label, compatibility, preserve, sample_entry) in [
        (None, None, None, false, "hvc1"),
        (
            Some("dolby_vision"),
            Some("Profile 5"),
            Some(0),
            true,
            "dvh1",
        ),
        (
            Some("dolby_vision"),
            Some("Profile 5"),
            Some(0),
            false,
            "hvc1",
        ),
        (
            Some("dolby_vision"),
            Some("Dolby Vision · Profile 8.1 (HDR10-compatible)"),
            Some(1),
            true,
            "hvc1",
        ),
    ] {
        let mut file = hevc_dv();
        file.hdr = hdr.map(str::to_owned);
        file.hdr_format = label.map(str::to_owned);
        file.dolby_vision.bl_compat_id = compatibility;
        file.dolby_vision.profile = compatibility.map(|compat| if compat == 0 { 5 } else { 8 });
        let ordinary = CopyVideoOptions::new(false, preserve);
        let retained = ordinary.with_parameter_set_retention(true);
        let ordinary_args = copy_video_args(&file, ordinary);
        assert!(
            contains_run(&ordinary_args, &["-tag:v".to_owned(), sample_entry.to_owned()]),
            "ordinary sample entry follows the source's authoritative compatibility {compatibility:?}: {ordinary_args:?}"
        );
        for args in [
            copy_video_args(&file, retained),
            copy_index_pipe_args(&file, retained),
            copy_pipe_args_with_dolby_vision(&file, 0.0, None, true, Pacing::unpaced(), retained),
        ] {
            assert!(
                contains_run(&args, &["-tag:v".to_owned(), sample_entry.to_owned()]),
                "retention cannot silently change the HLS sample-entry promise: {args:?}"
            );
            assert!(
                !args.iter().any(|arg| arg.contains("remove_types=32-34")),
                "retention must preserve the updates required to decode the original pixels: {args:?}"
            );
        }
        assert_ne!(
            crate::segplan::argv_fingerprint(&ordinary_args),
            crate::segplan::argv_fingerprint(&copy_video_args(&file, retained)),
            "a retaining recipe cannot alias destructive header stripping"
        );
    }
}

/// The index and the production pipe must agree about the video, because
/// the index is a list of output byte counts and the landing matcher
/// compares them. A drift here is not a stale index, it is a wrong one.
#[test]
fn the_index_pipe_carries_the_production_pipes_video_arguments() {
    for preserve in [false, true] {
        for dovi in [false, true] {
            let file = hevc_dv();
            let production = copy_pipe_args_with_dolby_vision(
                &file,
                0.0,
                None,
                true,
                Pacing::unpaced(),
                CopyVideoOptions::new(dovi, preserve),
            );
            let video_options = CopyVideoOptions::new(dovi, preserve);
            let index = copy_index_pipe_args(&file, video_options);
            let video = copy_video_args(&file, video_options);
            assert!(
                contains_run(&production, &video),
                "production pipe lost the video arguments: {production:?}"
            );
            // Header tracing is observational and must precede the
            // exact production filters. Compare after removing only
            // that explicitly asserted prefix, not the filter itself.
            let mut observed = index.clone();
            let filter = observed
                .iter()
                .position(|arg| arg == "-bsf:v")
                .expect("HEVC analysis filter")
                + 1;
            if observed[filter] == "trace_headers" {
                // Preserved DV may need no production bitstream filter.
                observed.drain(filter - 1..=filter);
            } else {
                observed[filter] = observed[filter]
                    .strip_prefix("trace_headers,")
                    .expect("observe original headers before destructive filtering")
                    .to_owned();
            }
            assert!(
                contains_run(&observed, &video),
                "index pipe changed production video arguments: {index:?}"
            );
        }
    }
}

#[test]
fn the_index_pipe_maps_no_audio_and_never_seeks() {
    let file = hevc_dv();
    let args = copy_index_pipe_args(&file, CopyVideoOptions::new(false, false));
    assert!(args.contains(&"-an".to_owned()));
    assert!(!args.iter().any(|a| a.starts_with("0:a:")));
    assert!(!args.contains(&"-ss".to_owned()));
    assert!(!args.contains(&"-noaccurate_seek".to_owned()));
    assert!(!args.contains(&"-readrate".to_owned()));
    assert!(args.ends_with(&["-f".to_owned(), "mp4".to_owned(), "pipe:1".to_owned()]));
}

/// The fingerprint must move when the video branch moves and stay put when
/// the file merely moves, which is the whole reason it is not a digest of
/// the entire argv.
#[test]
fn the_video_fingerprint_tracks_the_branch_not_the_path() {
    let file = hevc_dv();
    let mut moved = hevc_dv();
    moved.path = "/other/place/film.mkv".into();
    let stripped = crate::segplan::argv_fingerprint(&copy_video_args(
        &file,
        CopyVideoOptions::new(false, false),
    ));
    let preserved = crate::segplan::argv_fingerprint(&copy_video_args(
        &file,
        CopyVideoOptions::new(false, true),
    ));
    let relocated = crate::segplan::argv_fingerprint(&copy_video_args(
        &moved,
        CopyVideoOptions::new(false, false),
    ));
    assert_ne!(
        stripped, preserved,
        "preserving Dolby Vision changes the copied NAL stream, so it must \
             change the fingerprint"
    );
    assert_eq!(stripped, relocated, "a moved file is not a new pipeline");
}

#[test]
fn minimal_hevc_configuration_rebuilds_complete_muxer_extradata() {
    let mut file = hevc_dv();
    file.hdr = Some("hdr10".into());
    file.hdr_format = Some("HDR10".into());
    let probe = r#"{
            "streams": [
                {"codec_type":"video","codec_name":"mjpeg","extradata_size":23,
                 "disposition":{"attached_pic":1}},
                {"codec_type":"video","codec_name":"hevc","extradata_size":23,
                 "disposition":{"attached_pic":0}}
            ]
        }"#;
    assert!(hevc_parameter_set_promotion_required(&file, Some(probe)));

    let ordinary = copy_video_args(&file, CopyVideoOptions::new(false, false));
    let promoted = copy_video_args(
        &file,
        CopyVideoOptions::new(false, false).with_parameter_set_promotion(true),
    );
    assert!(
        ordinary.join(" ").contains("remove_types=32-34"),
        "ordinary HEVC must retain the hvc1 boundary-stutter fix"
    );
    assert!(
        promoted
            .join(" ")
            .contains("-bsf:v hevc_mp4toannexb,extract_extradata"),
        "the in-band VPS/SPS/PPS must become muxer extradata: {promoted:?}"
    );
    assert!(!promoted.join(" ").contains("32-34"), "{promoted:?}");
    assert_ne!(
        crate::segplan::argv_fingerprint(&ordinary),
        crate::segplan::argv_fingerprint(&promoted),
        "old failed indexes must not alias the repaired pipeline"
    );

    file.hdr = Some("dolby_vision".into());
    let stripped_dv = copy_video_args(
        &file,
        CopyVideoOptions::new(true, false).with_parameter_set_promotion(true),
    )
    .join(" ");
    assert!(
        stripped_dv.contains(
            "dovi_rpu=strip=1,hevc_mp4toannexb,extract_extradata,\
                 filter_units=remove_types=62-63"
        ),
        "DV metadata is still stripped while hvcC is rebuilt: {stripped_dv}"
    );
    assert!(!stripped_dv.contains("32-34"), "{stripped_dv}");
}

#[test]
fn channel_playback_repair_native_hls_keeps_parameter_set_promotion() {
    let file = hevc_dv();
    let args = hls_copy_args_with_dolby_vision(
        &file,
        6275.560,
        None,
        true,
        Pacing::unpaced(),
        CopyVideoOptions::new(false, false).with_parameter_set_promotion(true),
        "/tmp/channel-repair",
    );
    let joined = args.join(" ");

    assert!(
        joined.contains("-bsf:v hevc_mp4toannexb,extract_extradata"),
        "native HLS retry lost the normalization that rebuilds hvcC: {joined}"
    );
    assert!(joined.contains("-hls_segment_type fmp4"), "{joined}");
    assert!(!joined.contains("remove_types=32-34"), "{joined}");
}

#[test]
fn populated_or_non_hevc_configuration_does_not_request_promotion() {
    let file = hevc_dv();
    let populated = r#"{"streams":[{"codec_type":"video","extradata_size":97}]}"#;
    assert!(!hevc_parameter_set_promotion_required(
        &file,
        Some(populated)
    ));

    let mut h264 = file;
    h264.video_codec = Some("h264".into());
    let minimal = r#"{"streams":[{"codec_type":"video","extradata_size":23}]}"#;
    assert!(!hevc_parameter_set_promotion_required(&h264, Some(minimal)));
}

/// No argv that reaches an exec carries a plurx marker.
///
/// `DV_CONVERT_MARKER` exists to be fingerprinted, and it is deliberately
/// not a valid ffmpeg option — which means the day it reaches a child
/// process, every copy session and every index build for a converted title
/// dies at startup with "Unrecognized option". The marker is added in
/// `copy_video_args` and removed in exactly two places; this is the test
/// that says so, over every public builder in this module that produces an
/// argv a caller execs. They all funnel through `copy_input_args` or
/// `copy_index_pipe_args_with_input`, which is why two strip sites suffice
/// — but the funnel is an implementation detail, and a refactor that
/// widened it would be caught here rather than on a node.
#[test]
fn no_executed_argv_carries_a_plurx_marker() {
    let file = hevc_dv();
    let convert = CopyVideoOptions::new(true, false).with_dolby_vision_conversion(true);

    // The recipe carries it — that is the point of it existing.
    let recipe = copy_video_args(&file, convert);
    assert!(
        recipe.iter().any(|arg| arg == DV_CONVERT_MARKER),
        "the fingerprinted recipe must carry the marker: {recipe:?}"
    );

    let executed: Vec<(&str, Vec<String>)> = vec![
        (
            "copy pipe",
            copy_pipe_args_with_dolby_vision(&file, 0.0, None, true, Pacing::unpaced(), convert),
        ),
        ("index pipe", copy_index_pipe_args(&file, convert)),
        (
            "index pipe with input",
            copy_index_pipe_args_with_input(&file, "pipe:3", convert),
        ),
        (
            "hls copy",
            hls_copy_args_with_dolby_vision(
                &file,
                0.0,
                None,
                true,
                Pacing::unpaced(),
                convert,
                "/tmp/out",
            ),
        ),
        (
            "hls copy with sequence",
            hls_copy_args_with_sequence(
                &file,
                0.0,
                None,
                true,
                Pacing::unpaced(),
                convert,
                7,
                "init.mp4",
                "/tmp/out",
            ),
        ),
    ];
    for (name, args) in executed {
        assert!(
            !args.iter().any(|arg| arg.starts_with("--plurx-")),
            "{name} would hand ffmpeg a plurx marker: {args:?}"
        );
    }

    // And a stripped argv is otherwise the recipe, so the marker is the
    // only thing the two ever disagree about.
    assert_eq!(
        strip_plurx_markers(&recipe),
        copy_video_args(&file, CopyVideoOptions::new(true, true)),
        "stripping the marker must leave the preserving copy's arguments"
    );
}

/// The marker follows the source, not the flag.
///
/// It reserves a fragment-index identity, so the rule is that two argvs
/// differ exactly when the bytes they produce differ. Two shapes get that
/// wrong in opposite directions, and neither is reachable through today's
/// decider — which is the reason to hold the invariant in the builder
/// rather than rely on the decider to keep holding it.
#[test]
fn the_conversion_marker_tracks_the_source_and_not_the_flag() {
    let convert = CopyVideoOptions::new(true, false).with_dolby_vision_conversion(true);
    let carries = |file: &MediaFile, options| {
        copy_video_args(file, options)
            .iter()
            .any(|arg| arg == DV_CONVERT_MARKER)
    };

    // Plain HDR10 HEVC: there is no Dolby Vision to convert, so a set flag
    // must not earn a third index identity for a pipeline byte-identical
    // to the second.
    let mut hdr10 = hevc_dv();
    hdr10.hdr = Some("hdr10".into());
    hdr10.hdr_format = Some("HDR10".into());
    assert!(!carries(&hdr10, convert));
    assert_eq!(
        crate::segplan::argv_fingerprint(&copy_video_args(&hdr10, convert)),
        crate::segplan::argv_fingerprint(&copy_video_args(
            &hdr10,
            CopyVideoOptions::new(true, true)
        )),
        "an unconverted pipeline must not fork an index"
    );

    // A non-HEVC source: the marker lives outside the HEVC branch, so a
    // converted identity can never collide with an unconverted one. If it
    // did, an index built for one would be served for the other.
    let mut h264 = hevc_dv();
    h264.video_codec = Some("h264".into());
    assert!(carries(&h264, convert), "the marker is not HEVC-only");
    assert_ne!(
        crate::segplan::argv_fingerprint(&copy_video_args(&h264, convert)),
        crate::segplan::argv_fingerprint(&copy_video_args(
            &h264,
            CopyVideoOptions::new(true, true)
        )),
        "a converting copy must never share an identity with a plain one"
    );
}

/// The sample entry and the bitstream filter come from the stored columns,
/// not only from the label.
///
/// The two facts answer one question — is this Dolby Vision's base layer
/// watchable — and `playback` has read the column first since M2. When
/// this side read only the label, a row with the columns populated and no
/// label diverged: the decider called the base HDR10-compatible and routed
/// a preserving copy, while this builder took the Profile 5 branch and
/// rendered `-tag:v dvh1` with **no `-bsf:v` at all**, so NAL types 32-34
/// went unfiltered and the `hvc1` boundary-stutter fix was silently off.
#[test]
fn a_column_only_source_row_renders_the_compatible_base_arguments() {
    let row = |compat: Option<i64>, label: Option<&str>| {
        let mut file = hevc_dv();
        file.hdr_format = label.map(str::to_owned);
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.bl_compat_id = compat;
        file
    };
    let preserving = CopyVideoOptions::new(true, true);

    // 1 and 6 are HDR10 bases and 4 is HLG; all three are watchable
    // without a Dolby Vision decoder, which is what the tag and the filter
    // turn on. No label at all, so only the column can answer.
    for compat in [1, 4, 6] {
        let rendered = copy_video_args(&row(Some(compat), None), preserving).join(" ");
        assert!(
            rendered.contains("-tag:v hvc1"),
            "compatibility id {compat} is a watchable base: {rendered}"
        );
        assert!(
            rendered.contains("-bsf:v filter_units=remove_types=32-34"),
            "…so the parameter-set filtering must still run: {rendered}"
        );
    }

    // 2 is an SDR base and 0 is none: neither is watchable, so preserved
    // Dolby Vision keeps the `dvh1` entry and skips the filter, exactly as
    // Profile 5 does.
    for compat in [0, 2] {
        let rendered = copy_video_args(&row(Some(compat), None), preserving).join(" ");
        assert!(
            rendered.contains("-tag:v dvh1"),
            "compatibility id {compat} has no watchable base: {rendered}"
        );
        assert!(!rendered.contains("-bsf:v"), "{rendered}");
    }

    // The column outranks the label, in both directions. A row where they
    // disagree cannot be produced by today's scanner — the label is
    // derived from these same columns — but the precedence has to match
    // `playback`'s or the decider and the builder describe two streams.
    let contradicted = copy_video_args(&row(Some(2), Some("HDR10-compatible")), preserving);
    assert!(contradicted.join(" ").contains("-tag:v dvh1"));
    let rescued = copy_video_args(&row(Some(1), Some("Dolby Vision · Profile 5")), preserving);
    assert!(rescued.join(" ").contains("-tag:v hvc1"));
}

/// The stale-record predicate agrees with the filter it compensates for.
///
/// The two are one decision, and this is the assertion that keeps them so.
/// The predicate must be true for exactly the branches whose filter removes
/// the Dolby Vision NAL units *without* `dovi_rpu` — the only ones where
/// the DOVI side data survives into the muxer and becomes a record over a
/// stream that has neither layer. Derived from the rendered filter rather
/// than restated, so a future branch cannot be added to one and forgotten
/// in the other.
#[test]
fn the_stale_record_predicate_matches_every_filter_that_leaves_one() {
    for hdr in [Some("dolby_vision"), Some("hdr10"), None] {
        for have_dovi_bsf in [false, true] {
            for preserve in [false, true] {
                for convert in [false, true] {
                    let filter = hevc_copy_bsf_for_copy(hdr, have_dovi_bsf, preserve, convert);
                    // Removes the RPU units (62) by type, and does not ask
                    // ffmpeg to drop the side data they were described by.
                    let strips_the_layers_only =
                        filter.contains("62-63") && !filter.contains("dovi_rpu");
                    assert_eq!(
                        copy_leaves_a_stale_dolby_vision_record(
                            hdr,
                            have_dovi_bsf,
                            preserve,
                            convert
                        ),
                        strips_the_layers_only,
                        "hdr={hdr:?} dovi_bsf={have_dovi_bsf} preserve={preserve} \
                             convert={convert} rendered {filter}"
                    );
                }
            }
        }
    }
    // And it is not vacuous: the branch exists.
    assert!(copy_leaves_a_stale_dolby_vision_record(
        Some("dolby_vision"),
        false,
        false,
        false
    ));

    // The same equality against the OTHER filter builder. `copy_video_args`
    // hand-rolls its filter list on the parameter-set-promotion branch
    // instead of calling `hevc_copy_bsf_for_copy`, so the predicate's
    // agreement with it is a separate fact and held only by luck until
    // this asserted it.
    for hdr in [Some("dolby_vision"), Some("hdr10"), None] {
        for have_dovi_bsf in [false, true] {
            for preserve in [false, true] {
                for convert in [false, true] {
                    for promote in [false, true] {
                        let mut file = hevc_dv();
                        file.hdr = hdr.map(str::to_owned);
                        let options = CopyVideoOptions::new(have_dovi_bsf, preserve)
                            .with_parameter_set_promotion(promote)
                            .with_dolby_vision_conversion(convert);
                        let args = copy_video_args(&file, options);
                        let filter = args
                            .windows(2)
                            .find(|pair| pair[0] == "-bsf:v")
                            .map(|pair| pair[1].clone())
                            .unwrap_or_default();
                        let strips_the_layers_only = (filter.contains("62-63")
                            || filter.contains("remove_types=62-63"))
                            && !filter.contains("dovi_rpu");
                        assert_eq!(
                            options.leaves_a_stale_dolby_vision_record(&file),
                            strips_the_layers_only,
                            "hdr={hdr:?} dovi_bsf={have_dovi_bsf} preserve={preserve} \
                                 convert={convert} promote={promote} rendered {filter}"
                        );
                    }
                }
            }
        }
    }
}

/// A converting copy preserves the RPUs it converts.
///
/// The trap this closes: the decider reaches the conversion through a
/// client that refuses Profile 7, which is the same client it answers
/// `preserve = false` for. That renders `dovi_rpu=strip=1,…|62-63`, which
/// deletes the type-62 units — the conversion would find none, report
/// success, and serve plain HDR10 under a Profile 8.1 label.
#[test]
fn converting_dolby_vision_never_renders_a_stripping_bitstream_filter() {
    // The only source shape the conversion is ever reached for: Profile 7
    // over an HDR10-compatible base. The Profile 5 fixture would take the
    // parameter-set promotion branch instead and render no `-bsf:v` at
    // all, which would let this test pass for the wrong reason.
    let mut file = hevc_dv();
    file.hdr_format = Some(
        "Dolby Vision, Version 1.0, dvhe.07.06, BL+EL+RPU, \
                                HDR10-compatible / SMPTE ST 2086, HDR10"
            .into(),
    );

    for asked_preserve in [false, true] {
        let options =
            CopyVideoOptions::new(true, asked_preserve).with_dolby_vision_conversion(true);
        assert!(
            options.preserves_dolby_vision(),
            "converting implies preserving, whatever the caller asked for"
        );

        let rendered = copy_video_args(&file, options).join(" ");
        assert!(
            !rendered.contains("dovi_rpu=strip=1"),
            "the RPUs the conversion reads must survive the bsf: {rendered}"
        );
        assert!(
            !rendered.contains("62-63"),
            "…and so must their NAL type: {rendered}"
        );
        assert!(
            rendered.contains("filter_units=remove_types=32-34|63"),
            "the parameter-set filtering still applies, and the enhancement \
                 layer goes with it: {rendered}"
        );
        assert!(
            !rendered.contains("62"),
            "type 62 is the RPU — removing it would leave nothing for the \
                 rewrite after the muxer to convert: {rendered}"
        );
        // A compatible base keeps the `hvc1` sample entry — the Profile
        // 8.1 the conversion produces is an enhancement of HDR10, and the
        // profile is advertised by SUPPLEMENTAL-CODECS, not by the tag.
        assert!(
            rendered.contains("-tag:v hvc1"),
            "a converted stream is HDR10-compatible: {rendered}"
        );
    }

    // Turning the conversion off again leaves the caller's answer alone —
    // the coercion is one-way, so it cannot silently promote an ordinary
    // stripping copy into a preserving one.
    let off = CopyVideoOptions::new(true, false).with_dolby_vision_conversion(false);
    assert!(!off.preserves_dolby_vision());
    assert!(!off.converts_dolby_vision());
}

/// The parameter-set promotion branch drops the enhancement layer too.
///
/// That branch rebuilds the hvcC for a source whose VPS/SPS/PPS live only
/// in band, and a Profile 7 title with a 23-byte hvcC reaches it like any
/// other. It was the one path where a converting copy rendered an argv
/// byte-identical to a preserving one — so the enhancement layer survived
/// into a stream whose configuration record says it has none, and the two
/// pipelines were told apart only by the marker.
#[test]
fn a_converting_copy_drops_the_enhancement_layer_on_the_promotion_branch_too() {
    let mut file = hevc_dv();
    file.hdr_format =
        Some("Dolby Vision, Version 1.0, dvhe.07.06, BL+EL+RPU, HDR10-compatible".into());
    file.dolby_vision.profile = Some(7);
    file.dolby_vision.level = Some(6);
    file.dolby_vision.bl_compat_id = Some(1);

    let convert = CopyVideoOptions::new(true, false)
        .with_dolby_vision_conversion(true)
        .with_parameter_set_promotion(true);
    let preserve = CopyVideoOptions::new(true, true).with_parameter_set_promotion(true);

    let converting = copy_video_args(&file, convert).join(" ");
    assert!(
        converting.contains("filter_units=remove_types=63"),
        "the enhancement layer must go: {converting}"
    );
    assert!(
        !converting.contains("62"),
        "the RPUs must stay — they are what the rewrite converts: {converting}"
    );
    assert!(converting.contains("extract_extradata"), "{converting}");

    // …and the two pipelines no longer render the same command line, so
    // the marker is no longer the only thing separating their indexes.
    assert_ne!(
        strip_plurx_markers(&copy_video_args(&file, convert)),
        copy_video_args(&file, preserve),
    );
}

/// A converted stream gets its own fragment index.
///
/// Its bytes differ from both the stripped and the preserved copy, so its
/// segment boundaries do too. Sharing an identity with either would hand a
/// client a playlist whose cut points describe different media.
#[test]
fn a_converted_copy_has_its_own_fragment_index_identity() {
    let file = hevc_dv();
    let fingerprint = |options| crate::segplan::argv_fingerprint(&copy_video_args(&file, options));

    let stripped = fingerprint(CopyVideoOptions::new(true, false));
    let preserved = fingerprint(CopyVideoOptions::new(true, true));
    let converted =
        fingerprint(CopyVideoOptions::new(true, false).with_dolby_vision_conversion(true));

    assert_ne!(converted, stripped);
    assert_ne!(
        converted, preserved,
        "the conversion rewrites every RPU, so it is not the preserved copy"
    );
}

fn contains_run(haystack: &[String], needle: &[String]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    (0..=haystack.len() - needle.len()).any(|i| &haystack[i..i + needle.len()] == needle)
}

/// The one-identity argv as it stood before the shared builder existed,
/// frozen. `pipeline_digest_for_transform` hashes the live builder's
/// output into every cluster cache key, so a drift here would re-key
/// every index on the fleet.
fn frozen_copy_index_pipe_args_with_input(
    source: &MediaFile,
    input: &str,
    video: CopyVideoOptions,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    args.push("-i".into());
    args.push(input.to_owned());
    args.push("-map_chapters".into());
    args.push("-1".into());
    args.push("-map".into());
    args.push("0:v:0?".into());
    args.push("-an".into());
    args.push("-sn".into());
    args.extend(strip_plurx_markers(&copy_video_args(source, video)));
    args.extend(
        [
            "-avoid_negative_ts",
            "make_zero",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            "-use_editlist",
            "0",
            "-f",
            "mp4",
            "pipe:1",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if matches!(source.video_codec.as_deref(), Some("hevc" | "h265")) {
        args[2] = "info".to_owned();
        args.insert(3, "-xerror".to_owned());
        if let Some(index) = args.iter().position(|arg| arg == "-bsf:v") {
            args[index + 1] = format!("trace_headers,{}", args[index + 1]);
        } else {
            let at = args.len() - 1;
            args.splice(at..at, ["-bsf:v".to_owned(), "trace_headers".to_owned()]);
        }
    }
    args
}

fn every_copy_option() -> Vec<CopyVideoOptions> {
    let mut options = Vec::new();
    for bits in 0..32_u8 {
        options.push(
            CopyVideoOptions::new(bits & 1 != 0, bits & 2 != 0)
                .with_parameter_set_promotion(bits & 4 != 0)
                .with_dolby_vision_conversion(bits & 8 != 0)
                .with_parameter_set_retention(bits & 16 != 0),
        );
    }
    options
}

fn index_fixture_files() -> Vec<MediaFile> {
    let mut files = Vec::new();
    for (hdr, profile, codec) in [
        (Some("dolby_vision"), Some(5), "hevc"),
        (Some("dolby_vision"), Some(7), "hevc"),
        (Some("dolby_vision"), Some(8), "hevc"),
        (Some("hdr10"), None, "hevc"),
        (None, None, "hevc"),
        (None, None, "h264"),
    ] {
        let mut file = hevc_dv();
        file.hdr = hdr.map(str::to_owned);
        file.hdr_format = profile.map(|profile| format!("Profile {profile}"));
        file.dolby_vision.profile = profile;
        file.video_codec = Some(codec.to_owned());
        files.push(file);
    }
    files
}

#[test]
fn the_one_identity_index_argv_is_unchanged_by_the_shared_builder() {
    for file in index_fixture_files() {
        for video in every_copy_option() {
            assert_eq!(
                copy_index_pipe_args_with_input(&file, "/dev/fd/3", video),
                frozen_copy_index_pipe_args_with_input(&file, "/dev/fd/3", video),
                "{:?} {video:?}",
                file.hdr
            );
            assert_eq!(
                copy_index_pipe_args(&file, video),
                frozen_copy_index_pipe_args_with_input(&file, &file.path.to_string_lossy(), video),
            );
        }
    }
}

#[test]
fn a_shared_index_pass_is_one_input_then_each_identitys_own_block() {
    for file in index_fixture_files() {
        let options = every_copy_option();
        for pair in options.windows(3) {
            let shared = copy_index_shared_args_with_input(&file, "/dev/fd/3", pair);
            let lead = frozen_copy_index_pipe_args_with_input(&file, "/dev/fd/3", pair[0]);
            // The lead is today's whole argv, byte for byte.
            assert_eq!(shared[..lead.len()], lead[..]);
            // Each sibling is its own one-identity block after `-i`, with
            // its own pipe and no header trace.
            let mut at = lead.len();
            for (n, video) in pair.iter().enumerate().skip(1) {
                let alone = frozen_copy_index_pipe_args_with_input(&file, "/dev/fd/3", *video);
                let input = alone.iter().position(|arg| arg == "-i").expect("-i") + 2;
                let mut block: Vec<String> = alone[input..].to_vec();
                *block.last_mut().expect("url") = format!("pipe:{}", 3 + n);
                if let Some(bsf) = block.iter().position(|arg| arg == "-bsf:v") {
                    if block[bsf + 1] == "trace_headers" {
                        block.drain(bsf..bsf + 2);
                    } else if let Some(rest) = block[bsf + 1].strip_prefix("trace_headers,") {
                        block[bsf + 1] = rest.to_owned();
                    }
                }
                assert_eq!(shared[at..at + block.len()], block[..], "{video:?}");
                at += block.len();
            }
            assert_eq!(at, shared.len());
            assert_eq!(
                shared
                    .iter()
                    .filter(|arg| arg.contains("trace_headers"))
                    .count(),
                usize::from(matches!(file.video_codec.as_deref(), Some("hevc"))),
                "one trace, on the lead"
            );
            assert_eq!(shared.iter().filter(|arg| *arg == "-i").count(), 1);
        }
    }
}
