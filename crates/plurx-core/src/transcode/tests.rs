use super::*;
use crate::domain::MediaFile;

#[test]
fn vaapi_hdr_policy_is_frozen_scoped_and_shared_by_rolling_and_vod() {
    let source = file(Some("hdr10"));
    let facts = DecodeFacts::from_ffprobe_json(
        &serde_json::json!({"streams":[{
            "index":0,"codec_type":"video","codec_name":"hevc","profile":"Main 10",
            "pix_fmt":"yuv420p10le","width":1920,"height":1080,"field_order":"progressive",
            "avg_frame_rate":"24/1","r_frame_rate":"24/1",
            "color_transfer":"smpte2084","color_primaries":"bt2020","color_space":"bt2020nc"
        }]}),
        DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("source identity"),
    )
    .expect("bound decode facts");
    let capabilities = DecodeCapabilities::new(
        DecodeCapabilitySnapshotIdentity::new("b".repeat(64), "test-node".into(), None)
            .expect("capability identity"),
        vec![],
        vec![SoftwareDecoder {
            codec: "hevc".into(),
            implementation: None,
        }],
    )
    .expect("software decoder inventory");
    let mut caps = EncoderCaps {
        vaapi: true,
        vaapi_hdr_policy: Some(VaapiHdrPolicy::MesaRadeonsiVbaq),
        ..Default::default()
    };
    for (encoder, pipeline, applies) in [
        (Encoder::Vaapi, Pipeline::Hdr10Passthrough, true),
        (Encoder::Qsv, Pipeline::Hdr10Passthrough, false),
        (Encoder::Vaapi, Pipeline::Cpu, false),
    ] {
        let options = TranscodeOptions {
            pipeline,
            target_height: 1080,
            video_bitrate_kbps: 2000,
            auto_quality_rate_profile: None,
            normalized_geometry: false,
            ..Default::default()
        };
        let plan = resolve_transcode(
            &TranscodeRequest::new(
                encoder,
                TranscodeMediaOptions::from_options_with_facts(&source, &options, &facts),
            ),
            &facts,
            &capabilities,
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        )
        .expect("resolved plan");
        let execution = TranscodeExecution::from_options(&source, &options, Pacing::unpaced(), ".")
            .expect("execution inputs");
        let bound = plan.clone().with_vaapi_hdr_policy(&caps);
        assert_eq!(
            bound.output_contract().vaapi_hdr_policy().is_some(),
            applies
        );
        assert_eq!(bound.plan_digest() != plan.plan_digest(), applies);
        let digest = PipelineDigest {
            ffmpeg_build: "fixture".into(),
        };
        assert_eq!(
            Recipe::new(&digest, &bound, false).hash() != Recipe::new(&digest, &plan, false).hash(),
            applies
        );
        for args in [
            hls_args_for_plan(&bound, &execution),
            vod_pipe_args(
                &source,
                &bound,
                &execution,
                VodFrameGrid::new(24, 1).expect("fixed frame grid"),
                12.0,
            ),
        ] {
            assert_eq!(
                args.windows(2).any(|p| p == ["-compression_level", "19"]),
                applies
            );
            assert!(!args.iter().any(|a| a == "-qmax"));
            if applies {
                for pair in [
                    ["-b:v", "2000k"],
                    ["-maxrate", "3000k"],
                    ["-bufsize", "4000k"],
                ] {
                    assert!(args.windows(2).any(|p| p == pair), "{args:?}");
                }
            }
        }
        caps.vaapi_hdr_policy = None;
        assert_eq!(
            bound.output_contract().vaapi_hdr_policy().is_some(),
            applies,
            "the plan is frozen"
        );
        let cleared = bound.with_vaapi_hdr_policy(&caps);
        assert_eq!(cleared.plan_digest(), plan.plan_digest());
        assert_eq!(
            hls_args_for_plan(&cleared, &execution),
            hls_args_for_plan(&plan, &execution)
        );
        caps.vaapi_hdr_policy = Some(VaapiHdrPolicy::MesaRadeonsiVbaq);
    }
}

/// The Profile 5 renderer string, byte for byte. Three call sites build
/// from this one function (the renderer graph, the CPU-pipeline arm and
/// the boot probe); the argv goldens pin each site, this pins the source.
#[test]
fn the_dolby_tonemapx_builder_is_the_shipped_string() {
    assert_eq!(
        dovi_tonemapx_sdr_filter(),
        "tonemapx=tonemap=bt2390:transfer=bt709:matrix=bt709:primaries=bt709:\
             range=tv:format=yuv420p:apply_dovi=1"
    );
    assert_eq!(
        Pipeline::DoviTonemapx
            .filters(Some(1920), 1080, Some("dolby_vision"))
            .as_deref(),
        Some(
            "tonemapx=tonemap=bt2390:transfer=bt709:matrix=bt709:primaries=bt709:\
                 range=tv:format=yuv420p:apply_dovi=1,scale=1920:1080,format=yuv420p"
        )
    );
}

pub(super) fn file(hdr: Option<&str>) -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: "/media/movie.mkv".into(),
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
        hdr: hdr.map(str::to_owned),
        hdr_format: None,
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
fn software_encoder_vulkan_graph_uses_cpu_frame_handoff_without_upload_context() {
    let source = file(Some("hdr10"));
    let options = TranscodeOptions {
        pipeline: Pipeline::LibplaceboSoftware,
        target_height: 720,
        ..Default::default()
    };
    let args = hls_args(
        &source,
        Encoder::Software,
        &options,
        Pacing::unpaced(),
        "/tmp/session",
    );
    let vf = &args[args
        .iter()
        .position(|arg| arg == "-vf")
        .expect("production video arguments include a filter graph")
        + 1];
    assert!(vf.contains("format=rgba64le,setparams=colorspace=gbr,libplacebo="));
    assert!(!vf.contains("hwupload"));
    assert!(!vf.contains("hwdownload"));
    assert!(!args
        .iter()
        .any(|arg| arg == "-init_hw_device" || arg == "-filter_hw_device"));
    assert!(args.iter().any(|arg| arg == "libx264"));
}

#[test]
fn vulkan_playback_returns_frames_to_the_vaapi_encoder() {
    let source = file(Some("hdr10"));
    let options = TranscodeOptions {
        pipeline: Pipeline::Libplacebo,
        target_height: 1080,
        ..Default::default()
    };
    let args = hls_args(
        &source,
        Encoder::Vaapi,
        &options,
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(joined.contains("-init_hw_device vaapi=hw:"), "{joined}");
    assert!(joined.contains("-init_hw_device vulkan=vk@hw"), "{joined}");
    assert!(joined.contains("-filter_hw_device vk"), "{joined}");
    let vf = &args[args
        .iter()
        .position(|arg| arg == "-vf")
        .expect("video filter graph")
        + 1];
    assert!(vf.contains("hwupload,libplacebo="), "{vf}");
    assert!(vf.ends_with("hwupload=derive_device=vaapi"), "{vf}");
    assert!(joined.contains("h264_vaapi"), "{joined}");

    // The encoder upload must stay after a software subtitle composite.
    let options = TranscodeOptions {
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 0,
            bitmap: true,
        }),
        ..options
    };
    let args = hls_args(
        &source,
        Encoder::Vaapi,
        &options,
        Pacing::unpaced(),
        "/tmp/s",
    );
    let graph = &args[args
        .iter()
        .position(|arg| arg == "-filter_complex")
        .expect("subtitle composite graph")
        + 1];
    assert!(
        graph.ends_with("overlay=eof_action=pass,format=nv12,hwupload=derive_device=vaapi[vout]"),
        "{graph}"
    );
}

#[test]
fn vulkan_vaapi_playback_transfers_only_for_subtitle_burns() {
    let source = file(Some("hdr10"));
    for burn in [None, Some(false), Some(true)] {
        let options = TranscodeOptions {
            pipeline: Pipeline::LibplaceboVaapi,
            target_height: 1080,
            subtitle_burn: burn.map(|bitmap| SubtitleBurn {
                subtitle_index: 0,
                bitmap,
            }),
            ..Default::default()
        };
        let args = hls_args(
            &source,
            Encoder::Vaapi,
            &options,
            Pacing::unpaced(),
            "/tmp/s",
        );
        assert!(args
            .windows(2)
            .any(|p| p == ["-hwaccel_output_format", "vaapi"]));
        let flag = if burn == Some(true) {
            "-filter_complex"
        } else {
            "-vf"
        };
        let graph = &args[args.iter().position(|a| a == flag).expect("video graph") + 1];
        assert!(
            graph.contains("hwmap=derive_device=vaapi,format=vaapi"),
            "{graph}"
        );
        assert_eq!(
            graph.matches("hwdownload").count(),
            usize::from(burn.is_some()),
            "{graph}"
        );
        assert_eq!(
            graph.matches("hwupload").count(),
            usize::from(burn.is_some()),
            "{graph}"
        );
        if burn.is_some() {
            assert!(graph.contains("hwupload=derive_device=vaapi"), "{graph}");
            assert!(graph.find("hwmap=") < graph.find("hwdownload"), "{graph}");
        }
    }
}

#[test]
fn the_cpu_tone_map_names_peak_provenance_in_its_recipe() {
    let options = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        output_codec: None,
        strict_dolby: None,
        target_height: 1080,
        pipeline: Pipeline::Cpu,
        tone_map: ToneMap::Zscale,
        ..TranscodeOptions::default()
    };
    let default = file(Some("hdr10"));
    let default_filter = video_filters(&default, &options, "/media/movie.mkv");
    assert!(default_filter.contains("peak=10"), "{default_filter}");

    let mut cll = default.clone();
    cll.max_cll = Some(4000);
    cll.luminance_source = Some("stream".to_owned());
    let cll_filter = video_filters(&cll, &options, "/media/movie.mkv");
    assert!(cll_filter.contains("peak=40"), "{cll_filter}");

    let mut mdcv = default;
    mdcv.mastering_max_luminance = Some(2000);
    mdcv.luminance_source = Some("frame".to_owned());
    let mdcv_filter = video_filters(&mdcv, &options, "/media/movie.mkv");
    assert!(mdcv_filter.contains("peak=20"), "{mdcv_filter}");
    // Gamut conversion rides on the linearising zscale, ahead of the
    // curve; there is no separate primaries-only pass.
    assert!(
        mdcv_filter.contains(":t=linear:p=bt709:npl=100,format=gbrpf32le,tonemap="),
        "{mdcv_filter}"
    );
    assert!(!mdcv_filter.contains("zscale=p=bt709,"), "{mdcv_filter}");
    assert!(mdcv_filter.contains("dither=error_diffusion"));

    let default_media = TranscodeMediaOptions::from_options(&file(Some("hdr10")), &options);
    let cll_media = TranscodeMediaOptions::from_options(&cll, &options);
    let mdcv_media = TranscodeMediaOptions::from_options(&mdcv, &options);
    assert_eq!(default_media.tone_map_peak_source.name(), "default");
    assert_eq!(cll_media.tone_map_peak_source.name(), "cll");
    assert_eq!(mdcv_media.tone_map_peak_source.name(), "mdcv");
}

fn normalized_m0_args(
    source: &MediaFile,
    encoder: Encoder,
    options: &TranscodeOptions,
    force_software_decode: bool,
) -> Vec<String> {
    let mut arguments = hls_args_with_compatibility(
        source,
        encoder,
        options,
        Pacing::unpaced(),
        "/tmp/m0-output",
        force_software_decode,
    );
    for index in 1..arguments.len() {
        if arguments[index - 1] == "-vaapi_device" {
            arguments[index] = "<vaapi-device>".to_owned();
        }
    }
    arguments
        .into_iter()
        .map(|argument| {
            argument
                .replace("/media/movie.mkv", "<source>")
                .replace("/tmp/m0-output", "<output>")
        })
        .collect()
}

/// M0 migration fixture: M1 and M2 may change only the cases whose policy
/// change is named in the decoder plan. Every other token stays stable.
///
/// Re-measured once on the promotion merge, for exactly one named policy
/// change: S-07's CPU tone-map correction. The three heavy HDR cases moved
/// from `tonemap=...:desat=0,zscale=p=bt709:t=bt709:m=bt709:r=tv` to
/// `zscale=p=bt709,tonemap=...:desat=0:peak=10,zscale=t=bt709:m=bt709:\
/// r=tv:dither=error_diffusion` — gamut conversion before the tone map,
/// the explicit peak in hundreds of nits, and error-diffusion dithering on
/// the way back to 8-bit. Every other token in every case is unchanged,
/// including S-08's field-order routing, whose sources here are
/// progressive and therefore name no deinterlace filter.
///
/// Then once more for S-07's M2 cost correction: the standalone
/// `zscale=p=bt709` pass folded into the linearising zscale as `p=bt709`.
/// The output is bit-identical (measured); only the spelling moved.
#[test]
fn decoder_selection_m0_argument_baseline_is_stable() {
    let mut light_h264 = file(None);
    light_h264.container = Some("mkv".into());
    light_h264.video_codec = Some("h264".into());
    light_h264.video_profile = Some("High".into());
    light_h264.width = Some(1920);
    light_h264.height = Some(1080);
    light_h264.bit_depth = Some(8);
    light_h264.hdr = None;

    let mut incident_mpeg4 = light_h264.clone();
    incident_mpeg4.container = Some("avi".into());
    incident_mpeg4.video_codec = Some("mpeg4".into());
    incident_mpeg4.video_profile = Some("Advanced Simple Profile".into());
    incident_mpeg4.width = Some(624);
    incident_mpeg4.height = Some(352);
    incident_mpeg4.bitrate = None;

    let heavy_hevc = file(Some("hdr10"));
    let dovi = file(Some("dolby_vision"));
    let options = |pipeline| TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        pipeline,
        ..TranscodeOptions::default()
    };

    let cases = [
        (
            "software-sdr-h264",
            &light_h264,
            Encoder::Software,
            TranscodeOptions::default(),
            false,
        ),
        (
            "qsv-light-h264",
            &light_h264,
            Encoder::Qsv,
            TranscodeOptions::default(),
            false,
        ),
        (
            "vaapi-light-h264",
            &light_h264,
            Encoder::Vaapi,
            TranscodeOptions::default(),
            false,
        ),
        (
            "qsv-heavy-hevc-hdr",
            &heavy_hevc,
            Encoder::Qsv,
            TranscodeOptions::default(),
            false,
        ),
        (
            "vaapi-heavy-hevc-hdr",
            &heavy_hevc,
            Encoder::Vaapi,
            TranscodeOptions::default(),
            false,
        ),
        (
            "qsv-vendor-renderer",
            &heavy_hevc,
            Encoder::Qsv,
            options(Pipeline::VppQsv),
            false,
        ),
        (
            "vaapi-vendor-renderer",
            &heavy_hevc,
            Encoder::Vaapi,
            options(Pipeline::TonemapVaapi),
            false,
        ),
        (
            "nvenc-libplacebo-renderer",
            &heavy_hevc,
            Encoder::Nvenc,
            options(Pipeline::Libplacebo),
            false,
        ),
        (
            "vaapi-opencl-renderer",
            &heavy_hevc,
            Encoder::Vaapi,
            options(Pipeline::TonemapOpencl),
            false,
        ),
        (
            "nvenc-light-h264",
            &light_h264,
            Encoder::Nvenc,
            TranscodeOptions::default(),
            false,
        ),
        (
            "videotoolbox-sdr-h264",
            &light_h264,
            Encoder::VideoToolbox,
            TranscodeOptions::default(),
            false,
        ),
        (
            "videotoolbox-avi-mpeg4",
            &incident_mpeg4,
            Encoder::VideoToolbox,
            TranscodeOptions::default(),
            false,
        ),
        (
            "qsv-heavy-hevc-hdr-forced-software",
            &heavy_hevc,
            Encoder::Qsv,
            TranscodeOptions::default(),
            true,
        ),
        (
            "dovi-tonemapx-videotoolbox",
            &dovi,
            Encoder::VideoToolbox,
            options(Pipeline::DoviTonemapx),
            false,
        ),
        (
            "dovi-passthrough-qsv",
            &dovi,
            Encoder::Qsv,
            options(Pipeline::DoviPassthrough),
            false,
        ),
        (
            "hdr10-passthrough-qsv",
            &heavy_hevc,
            Encoder::Qsv,
            options(Pipeline::Hdr10Passthrough),
            false,
        ),
    ];
    let actual = cases
        .into_iter()
        .map(|(name, source, encoder, options, force_software_decode)| {
            serde_json::json!({
                "name": name,
                "encoder": encoder.label(),
                "renderer": options.pipeline.name(),
                "compatibility_force_software_decode": force_software_decode,
                "args": normalized_m0_args(
                    source,
                    encoder,
                    &options,
                    force_software_decode,
                ),
            })
        })
        .collect::<Vec<_>>();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/playback/decoder-selection-m0-args.json"
    ))
    .expect("M0 argument fixture is JSON");
    assert_eq!(serde_json::Value::Array(actual), expected);
}

#[test]
fn decoder_compatibility_override_values_are_stable() {
    for value in ["off", "0", "false", "no"] {
        assert!(
            compatibility_value_forces_software_decode(Some(value)),
            "legacy override {value:?} must force software decode"
        );
    }
    for value in [None, Some(""), Some("1"), Some("on"), Some("OFF")] {
        assert!(
            !compatibility_value_forces_software_decode(value),
            "legacy override {value:?} must retain automatic decode selection"
        );
    }
}

#[test]
fn software_hls_args_are_well_formed() {
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        output_codec: None,
        strict_dolby: None,
        target_height: 1080,
        video_bitrate_kbps: 6000,
        ..Default::default()
    };
    let args = hls_args(
        &file(None),
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/sess",
    );
    let joined = args.join(" ");
    assert!(joined.contains("-i /media/movie.mkv"));
    assert!(joined.contains("libx264"));
    assert!(joined.contains("scale=-2:'min(1080,ih)'"));
    assert!(joined.contains("-f hls"));
    assert!(joined.contains("/tmp/sess/index.m3u8"));
    // The forced-keyframe grid and the muxer's segment length are the same
    // number by construction — a session whose keyframes don't land on its
    // segment boundaries produces segments that are not independently
    // decodable, which is the one thing HLS requires of them. Written
    // against the constant so the two can never be tuned apart.
    assert!(joined.contains(&format!("expr:gte(t,n_forced*{SEGMENT_SECONDS})")));
    assert!(joined.contains(&format!("-hls_time {SEGMENT_SECONDS}")));
    // SDR source → no tonemap, just pixel-format normalize.
    assert!(joined.contains("format=yuv420p"));
    assert!(!joined.contains("tonemap"));
}

/// The geometry that a naive overlay gets wrong, and gets wrong invisibly
/// to anything but an eye: a UHD Blu-ray very often carries a 1920×1080
/// PGS canvas over 3840×2160 video, and compositing that as-is puts every
/// subtitle at quarter size in the upper-left quadrant. Scaling the
/// subtitle plane to the *output* frame is what puts it back.
#[test]
fn a_bitmap_burn_scales_the_subtitle_to_the_output_frame() {
    let mut f = file(None);
    f.width = Some(3840);
    f.height = Some(2160);
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        output_codec: None,
        strict_dolby: None,
        target_height: 1080,
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 2,
            bitmap: true,
        }),
        ..Default::default()
    };
    let args = hls_args(&f, Encoder::Software, &opts, Pacing::unpaced(), "/tmp/s");
    let joined = args.join(" ");

    // Two streams into one filter needs -filter_complex and a mapped label;
    // -vf cannot express it at all.
    assert!(joined.contains("-filter_complex"), "{joined}");
    assert!(!joined.contains("-vf "), "a burn must not also use -vf");
    assert!(
        joined.contains("-map [vout]"),
        "the burned video is what plays"
    );
    assert!(
        !joined.contains("-map 0:v:0"),
        "mapping the raw video skips the burn"
    );

    // The subtitle is scaled to the frame the viewer will see, not to the
    // source and not to its own canvas.
    assert!(joined.contains("[0:s:2]scale=1920:1080[sburn]"), "{joined}");
    // `eof_action=pass` rather than the default `repeat`: a subtitle
    // stream ends before the film does, and repeating its last frame both
    // freezes a subtitle on screen for the rest of the runtime and makes
    // the encoder log an alarming duplication error about a gap that is
    // not a problem. Verified against a real PGS stream.
    assert!(
        joined.contains("[vburn][sburn]overlay=eof_action=pass[vout]"),
        "{joined}"
    );

    // A text burn is a filter in the chain, not a composite — it must not
    // take this path.
    let text = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 0,
            bitmap: false,
        }),
        ..opts.clone()
    };
    let joined = hls_args(&f, Encoder::Software, &text, Pacing::unpaced(), "/tmp/s").join(" ");
    assert!(joined.contains("-vf "), "text burn stays a simple chain");
    assert!(joined.contains("subtitles="));
    assert!(!joined.contains("overlay"));
}

/// A bitmap burn no longer costs the GPU tone-map (the 4.9× of PERF-PLAN
/// §5): the vendor graph runs scale + tone-map on the GPU pinned to the
/// overlay's exact frame, comes down once for the composite, and the
/// encoder's upload runs after the overlay — never before it.
#[test]
fn a_bitmap_burn_keeps_the_gpu_tonemap_and_downloads_once() {
    let mut f = file(Some("dolby_vision"));
    f.width = Some(3840);
    f.height = Some(2160);
    f.bit_depth = Some(10);
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        target_height: 2160,
        pipeline: Pipeline::VppQsv,
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 5,
            bitmap: true,
        }),
        ..Default::default()
    };
    let args = hls_args(&f, Encoder::Qsv, &opts, Pacing::unpaced(), "/tmp/s");
    let joined = args.join(" ");

    // The GPU graph is present, pinned to the overlay's frame, and comes
    // down to system memory exactly once, before the composite.
    assert!(
        joined
            .contains("vpp_qsv=w=3840:h=2160:tonemap=1:format=nv12,hwdownload,format=nv12[vburn]"),
        "{joined}"
    );
    // The subtitle plane lands on the same geometry.
    assert!(joined.contains("[0:s:5]scale=3840:2160[sburn]"), "{joined}");
    // The encoder's upload happens AFTER the overlay — a chain that
    // uploads first hands `overlay` a hardware surface it cannot read.
    assert!(
        joined.contains("overlay=eof_action=pass,hwupload=extra_hw_frames=64,format=qsv[vout]"),
        "{joined}"
    );
    let up = joined.find("hwupload").expect("upload present");
    let ov = joined.find("overlay=").expect("overlay present");
    assert!(ov < up, "upload must follow the composite: {joined}");
    // No float tone-map anywhere near this session.
    assert!(!joined.contains("zscale"), "{joined}");
    assert!(!joined.contains("tonemap=tonemap=hable"), "{joined}");
}

/// The same ordering rule on the CPU chain: a QSV encode of a burned
/// session uploads after the composite, not before it. (The old graph put
/// the encoder suffix on the [vburn] half — upload, then overlay — an
/// order only the suffix-less software encoder could survive, which is
/// exactly what the tests used to burn with.)
#[test]
fn the_upload_suffix_follows_the_composite_on_the_cpu_chain_too() {
    let mut f = file(Some("hdr10"));
    f.width = Some(3840);
    f.height = Some(2160);
    f.bit_depth = Some(10);
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        output_codec: None,
        strict_dolby: None,
        target_height: 1080,
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 0,
            bitmap: true,
        }),
        ..Default::default()
    };
    let args = hls_args(&f, Encoder::Qsv, &opts, Pacing::unpaced(), "/tmp/s");
    let joined = args.join(" ");
    assert!(
        joined.contains("overlay=eof_action=pass,hwupload=extra_hw_frames=64,format=qsv[vout]"),
        "{joined}"
    );
    assert!(
        !joined.contains("format=qsv[vburn]"),
        "the upload crept back in front of the overlay: {joined}"
    );
    // Without a burn the suffix stays at the end of the plain chain.
    let plain = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        output_codec: None,
        strict_dolby: None,
        target_height: 1080,
        ..Default::default()
    };
    let joined = hls_args(&f, Encoder::Qsv, &plain, Pacing::unpaced(), "/tmp/s").join(" ");
    assert!(joined.contains("-vf "), "{joined}");
    assert!(
        joined.contains("hwupload=extra_hw_frames=64,format=qsv"),
        "{joined}"
    );
}

/// A Dolby Vision source with an HDR10-compatible base layer routes like
/// the HDR10 stream its base layer is; one without keeps the DV routing
/// the vendor graphs decline. The file's real `hdr` column is untouched —
/// this is about graph selection, not about what the file is.
#[test]
fn dv_with_an_hdr10_base_routes_as_hdr10() {
    let mut f = file(Some("dolby_vision"));
    f.hdr_format = Some("Dolby Vision · Profile 7 (HDR10-compatible)".into());
    assert_eq!(routing_hdr(&f), Some("hdr10"));
    f.hdr_format = Some("Dolby Vision · Profile 5".into());
    assert_eq!(routing_hdr(&f), Some("dolby_vision"));
    f.hdr_format = None;
    assert_eq!(routing_hdr(&f), Some("dolby_vision"));
    let f = file(Some("hdr10"));
    assert_eq!(routing_hdr(&f), Some("hdr10"));
    let f = file(None);
    assert_eq!(routing_hdr(&f), None);
}

/// The output frame is what a bitmap subtitle is scaled against, so its
/// arithmetic has to be exactly the scaler's: never upscale, keep the
/// aspect, and land on even sides.
#[test]
fn the_output_frame_matches_what_the_scaler_would_produce() {
    let mut f = file(None);
    f.width = Some(3840);
    f.height = Some(2160);
    assert_eq!(output_size(&f, 1080), Some((1920, 1080)));
    assert_eq!(output_size(&f, 720), Some((1280, 720)));
    // Never upscales: asking for more than the source has yields the source.
    assert_eq!(output_size(&f, 2160), Some((3840, 2160)));
    assert_eq!(output_size(&f, 4320), Some((3840, 2160)));

    // Odd arithmetic rounds to even — yuv420p cannot represent an odd side,
    // and every encoder refuses one.
    f.width = Some(1919);
    f.height = Some(1079);
    let (w, h) = output_size(&f, 720).expect("size");
    assert_eq!((w % 2, h % 2), (0, 0), "got {w}x{h}");

    // A file the probe never read has no aspect to preserve. Guessing one
    // would misshape the picture, so nothing is claimed.
    f.width = None;
    assert_eq!(output_size(&f, 1080), None);
}

#[test]
fn hdr_source_inserts_tonemap() {
    let args = hls_args(
        &file(Some("hdr10")),
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(joined.contains("tonemap=tonemap=hable"));
    assert!(joined.contains("zscale"));
}

#[test]
fn interlaced_file_uses_send_frame_bwdif_before_scale() {
    let mut source = file(None);
    source.field_order = Some("tt".into());
    let joined = hls_args(
        &source,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    )
    .join(" ");
    let bwdif = joined
        .find("bwdif=mode=send_frame:parity=auto:deint=interlaced")
        .expect("bwdif filter");
    let scale = joined.find("scale=").expect("scale filter");
    assert!(bwdif < scale, "deinterlace must precede scale: {joined}");

    source.field_order = Some("progressive".into());
    let joined = hls_args(
        &source,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    )
    .join(" ");
    assert!(!joined.contains("bwdif="), "{joined}");
}

#[test]
fn interlaced_special_renderers_apply_bwdif_without_breaking_dolby_vision_order() {
    for (pipeline, hdr) in [
        (Pipeline::DoviTonemapx, "dolby_vision"),
        (Pipeline::DoviPassthrough, "dolby_vision"),
        (Pipeline::Hdr10Passthrough, "hdr10"),
    ] {
        let mut source = file(Some(hdr));
        source.field_order = Some("tt".into());
        let joined = hls_args(
            &source,
            Encoder::Software,
            &TranscodeOptions {
                auto_quality_rate_profile: None,
                normalized_geometry: false,
                pipeline,
                ..TranscodeOptions::default()
            },
            Pacing::unpaced(),
            "/tmp/s",
        )
        .join(" ");
        let bwdif = joined
            .find("bwdif=mode=send_frame:parity=auto:deint=interlaced")
            .unwrap_or_else(|| panic!("{pipeline:?} omitted bwdif: {joined}"));
        let scale = joined
            .find("scale=")
            .unwrap_or_else(|| panic!("{pipeline:?} omitted scale: {joined}"));
        assert!(bwdif < scale, "{pipeline:?} scaled before bwdif: {joined}");
        assert_eq!(
            joined.matches("bwdif=").count(),
            1,
            "{pipeline:?}: {joined}"
        );
        if matches!(pipeline, Pipeline::DoviTonemapx | Pipeline::DoviPassthrough) {
            let reshape = joined
                .find("tonemapx=")
                .unwrap_or_else(|| panic!("{pipeline:?} omitted reshape: {joined}"));
            assert!(
                reshape < bwdif,
                "{pipeline:?} must reshape Dolby Vision before deinterlacing: {joined}"
            );
        }
    }
}

#[test]
#[ignore = "needs the shipped ffmpeg; run with PLURX_FFMPEG set"]
fn shipped_ffmpeg_deinterlace_fixture_matrix() {
    let ffmpeg = std::env::var("PLURX_FFMPEG").expect("PLURX_FFMPEG is required");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let status = std::process::Command::new("bash")
        .arg(root.join("tests/playback/interlace-fixtures.sh"))
        .env("PLURX_FFMPEG", ffmpeg)
        .status()
        .expect("run interlace fixture matrix");
    assert!(status.success(), "interlace fixture matrix failed");
}

#[test]
fn profile5_tonemap_applies_dolby_vision_metadata_before_sdr_mapping() {
    let mut source = file(Some("dolby_vision"));
    source.hdr_format = Some("Dolby Vision · Profile 5".into());
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        tone_map: ToneMap::Tonemapx,
        pipeline: Pipeline::DoviTonemapx,
        ..Default::default()
    };
    let args = hls_args(
        &source,
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(!joined.contains("-init_hw_device vulkan"), "{joined}");
    assert!(!joined.contains("-filter_hw_device"), "{joined}");
    assert!(!joined.contains("hwupload"), "{joined}");
    assert!(joined.contains("tonemapx=tonemap=bt2390"), "{joined}");
    assert!(joined.contains("apply_dovi=1"), "{joined}");
    assert!(joined.contains("transfer=bt709"), "{joined}");
    assert!(joined.contains("matrix=bt709"), "{joined}");
    assert!(joined.contains("primaries=bt709"), "{joined}");
    assert!(joined.contains("range=tv"), "{joined}");
    let tonemap_at = joined.find("tonemapx=").expect("tonemapx filter");
    let scale_at = joined.find("scale=1920:1080").expect("scale filter");
    assert!(
        tonemap_at < scale_at,
        "RPU reshape must precede scale: {joined}"
    );
    assert!(!joined.contains("setparams="), "{joined}");
    assert!(!joined.contains("zscale="), "{joined}");
    assert!(!joined.contains("-hwaccel qsv"), "{joined}");

    let mut compatible = source;
    compatible.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".into());
    let opts = TranscodeOptions::default();
    let joined = hls_args(
        &compatible,
        Encoder::Qsv,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    )
    .join(" ");
    assert!(!joined.contains("apply_dovi=1"), "{joined}");
    assert!(joined.contains("zscale="), "{joined}");

    let mut hlg_compatible = compatible;
    hlg_compatible.hdr_format = Some("Dolby Vision · Profile 8 (HLG-compatible)".into());
    let joined = hls_args(
        &hlg_compatible,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    )
    .join(" ");
    assert!(joined.contains("tin=arib-std-b67"), "{joined}");
    assert!(!joined.contains("tin=smpte2084"), "{joined}");
}

/// The SDR Profile 5 argument list, pinned **verbatim**.
///
/// This is the hard requirement M5 was allowed to change nothing about:
/// an SDR client's Dolby Vision transcode has to be the same bytes it was
/// before an HDR rung existed beside it. A `contains` assertion cannot see
/// a reordered option, an added flag, or a changed preset — all three
/// change the picture or the cache identity — so this compares the whole
/// vector.
///
/// If this fails, the answer is almost never to update the literal.
#[test]
fn the_sdr_argument_list_for_a_profile5_source_is_unchanged() {
    let mut source = file(Some("dolby_vision"));
    source.hdr_format = Some("Dolby Vision · Profile 5".into());
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        tone_map: ToneMap::Tonemapx,
        pipeline: Pipeline::DoviTonemapx,
        target_height: 1080,
        video_bitrate_kbps: 8_000,
        ..Default::default()
    };
    let args = hls_args(
        &source,
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    );
    assert_eq!(
        args,
        [
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "/media/movie.mkv",
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-vf",
            "tonemapx=tonemap=bt2390:transfer=bt709:matrix=bt709:primaries=bt709:\
                 range=tv:format=yuv420p:apply_dovi=1,scale=1920:1080,format=yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-b:v",
            "8000k",
            "-maxrate",
            "12000k",
            "-bufsize",
            "16000k",
            "-profile:v",
            "high",
            "-force_key_frames",
            "expr:gte(t,n_forced*2)",
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-b:a",
            "160k",
            // S-09 M2 pins every lossy rolling audio output at 48 kHz.
            "-ar",
            "48000",
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-f",
            "hls",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "event",
            "-hls_flags",
            "independent_segments+temp_file",
            "-hls_segment_type",
            "mpegts",
            "-hls_segment_filename",
            "/tmp/s/seg%05d.ts",
            "-start_number",
            "0",
            "/tmp/s/index.m3u8",
        ]
        .map(str::to_owned)
    );
}

/// The HDR10 rung's whole argument list: HEVC Main10 out of a PQ chain,
/// and nothing H.264 anywhere in it.
#[test]
fn the_hdr10_rung_produces_hevc_main10_pq() {
    let mut source = file(Some("dolby_vision"));
    source.hdr_format = Some("Dolby Vision · Profile 5".into());
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        tone_map: ToneMap::None,
        pipeline: Pipeline::DoviPassthrough,
        target_height: 1080,
        video_bitrate_kbps: 8_000,
        ..Default::default()
    };
    let args = hls_args(
        &source,
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(
        joined.contains(
            "-vf tonemapx=tonemap=none:transfer=smpte2084:matrix=bt2020:\
                 primaries=bt2020:range=tv:format=yuv420p10le:apply_dovi=1,\
                 scale=1920:1080,format=yuv420p10le"
        ),
        "{joined}"
    );
    assert!(joined.contains("-c:v libx265"), "{joined}");
    assert!(
        joined.contains("-x265-params hdr10=1:repeat-headers=1"),
        "{joined}"
    );
    assert!(!joined.contains("libx264"), "{joined}");
    assert!(!joined.contains("-profile:v high"), "{joined}");
    assert!(!joined.contains("zscale"), "{joined}");
    assert!(joined.contains("apply_dovi=1"), "{joined}");
    // The rate control is the same bounded VBR every rung uses.
    assert!(joined.contains("-b:v 8000k -maxrate 12000k -bufsize 16000k"));
    // …and the muxer/segment policy is untouched.
    assert!(joined.contains("-force_key_frames expr:gte(t,n_forced*2)"));
    assert!(joined.contains("-hls_segment_type mpegts"));

    source.width = Some(3840);
    source.height = Some(2160);
    let qsv = hls_args(
        &source,
        Encoder::Qsv,
        &TranscodeOptions {
            auto_quality_rate_profile: None,
            normalized_geometry: false,
            tone_map: ToneMap::None,
            pipeline: Pipeline::DoviPassthrough,
            target_height: 2160,
            video_bitrate_kbps: 20_000,
            force_idr: true,
            ..Default::default()
        },
        Pacing::unpaced(),
        "/tmp/s",
    )
    .join(" ");
    assert!(
        qsv.starts_with("-hide_banner -loglevel error -init_hw_device qsv=hw -filter_hw_device hw"),
        "{qsv}"
    );
    assert!(
        qsv.contains(
            "scale=3840:2160,format=yuv420p10le,format=p010le,\
                 hwupload=extra_hw_frames=64,format=qsv"
        ),
        "{qsv}"
    );
    assert!(
        qsv.contains("-c:v hevc_qsv -preset veryfast -profile:v main10"),
        "{qsv}"
    );
    assert!(
        qsv.contains("-color_primaries bt2020 -color_trc smpte2084"),
        "{qsv}"
    );
    assert!(
        qsv.contains("-colorspace bt2020nc -color_range tv"),
        "{qsv}"
    );
    assert!(qsv.contains("-forced_idr 1"), "{qsv}");
    assert!(
        !qsv.contains("-hwaccel qsv"),
        "Dolby RPU side data requires software decode: {qsv}"
    );
}

/// PQ at 8 bits does not fail — it calls `abort()`. The guard therefore
/// has to fire before ffmpeg is ever handed the chain, and it has to
/// leave the chains that legitimately mention PQ alone.
#[test]
fn a_pq_output_transfer_is_never_paired_with_an_eight_bit_format() {
    // The SDR tone-map declares PQ as its INPUT and outputs 8-bit BT.709.
    // That is correct, and a naive substring guard condemns it.
    assert_no_pq_at_8_bit(
        "zscale=tin=smpte2084:min=bt2020nc:pin=bt2020:t=linear:p=bt709:npl=100,\
             format=gbrpf32le,tonemap=tonemap=hable:desat=0:peak=10,\
             zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p",
    );
    // A 10-bit PQ map followed by an unrelated 8-bit convert in a later
    // filter is swscale's business, not tonemapx's.
    assert_no_pq_at_8_bit(
        "tonemapx=transfer=smpte2084:format=yuv420p10le,scale=64:64,format=yuv420p",
    );
    // Every chain this crate can actually build is clean, at every rung
    // and for every source grade.
    for pipeline in PIPELINE_CANDIDATES.iter().copied().chain([
        Pipeline::DoviTonemapx,
        Pipeline::DoviPassthrough,
        Pipeline::Hdr10Passthrough,
    ]) {
        for hdr in [None, Some("hdr10"), Some("hlg"), Some("dolby_vision")] {
            if let Some(graph) = pipeline.filters(Some(1920), 1080, hdr) {
                assert_no_pq_at_8_bit(&graph);
            }
        }
    }
}

#[test]
#[should_panic(expected = "aborts ffmpeg")]
fn a_pq_transfer_beside_an_eight_bit_format_is_refused_before_ffmpeg_sees_it() {
    // The exact combination measured to SIGABRT: transfer=smpte2084 with
    // an 8-bit output format on the same filter.
    assert_no_pq_at_8_bit(
        "tonemapx=tonemap=none:transfer=smpte2084:matrix=bt2020:primaries=bt2020:\
             range=tv:format=yuv420p:apply_dovi=1",
    );
}

/// Rule 4f: produce a stream and MEASURE it.
///
/// Everything above this asserts about an argument list, and an argument
/// list is a hypothesis. This one takes the vector `hls_args` actually
/// builds, hands it to a real ffmpeg, and reads the colour properties of
/// the segment that comes out. It is the only test here that can tell the
/// difference between "the options are spelled correctly" and "the filter
/// does what the spelling implies" — which for `tonemapx`'s passthrough
/// mode is the entire question, because the mode is engaged by a value
/// (`transfer=smpte2084`) rather than by a switch.
///
/// **It needs a real Dolby Vision Profile 5 source, and there is no way
/// to synthesise one from ffmpeg alone** — the RPU has to be injected by
/// `dovi_tool`, which is not a dependency of this repo. So it runs where
/// `PLURX_DV_P5_FIXTURE` names such a file and is skipped where it does
/// not, which is a compromise this file otherwise refuses (see
/// `testfixtures::require_ffmpeg`, which fails rather than skips). The
/// difference is that the missing thing here is *media*, not a tool an
/// operator can install.
///
/// Measured on jellyfin-ffmpeg7 7.1.4-3-bookworm, 2 cores, against a
/// 1920x1080 Profile 5 sample — `profile=Main 10`,
/// `pix_fmt=yuv420p10le`, `color_range=tv`, `color_space=bt2020nc`,
/// `color_transfer=smpte2084`, `color_primaries=bt2020`, `level=120`, no
/// residual Dolby Vision RPU side data, 10.4 fps.
#[test]
fn the_hdr10_rung_really_produces_a_pq_stream() {
    let Some(source_path) = std::env::var("PLURX_DV_P5_FIXTURE")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        eprintln!(
            "skipping: set PLURX_DV_P5_FIXTURE to a real Dolby Vision Profile 5 \
                 source to measure the HDR10 rung's output"
        );
        return;
    };
    let ffmpeg = std::env::var("PLURX_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
    let ffprobe = std::env::var("PLURX_FFPROBE").unwrap_or_else(|_| "ffprobe".to_owned());

    let mut source = file(Some("dolby_vision"));
    source.path = source_path.clone().into();
    source.hdr_format = Some("Dolby Vision · Profile 5".into());
    source.width = Some(1920);
    source.height = Some(1080);
    let out = std::env::temp_dir().join(format!(
        "plurx-m5-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&out).expect("output dir");

    // The production builder. Not a hand-written command that resembles
    // it — the same function a session calls.
    let args = hls_args(
        &source,
        Encoder::Software,
        &TranscodeOptions {
            auto_quality_rate_profile: None,
            normalized_geometry: false,
            tone_map: ToneMap::None,
            pipeline: Pipeline::DoviPassthrough,
            target_height: 1080,
            video_bitrate_kbps: 8_000,
            ..Default::default()
        },
        Pacing::unpaced(),
        &out.to_string_lossy(),
    );
    let produced = std::process::Command::new(&ffmpeg)
        .args(&args)
        .output()
        .expect("running ffmpeg");
    assert!(
        produced.status.success(),
        "the production HDR10 arguments failed: {}\n{}",
        produced.status,
        String::from_utf8_lossy(&produced.stderr)
    );
    // A signal death is finding (3) — PQ at 8 bits aborts — and must
    // never be reported as an ordinary failure.
    assert!(
        produced.status.code().is_some(),
        "ffmpeg died by signal, which is what the 8-bit PQ abort looks like"
    );

    let segment = out.join("seg00000.ts");
    assert!(segment.is_file(), "no segment was produced");
    let probe = std::process::Command::new(&ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(
            "stream=codec_name,profile,width,height,pix_fmt,color_range,\
                 color_space,color_transfer,color_primaries",
        )
        .args(["-of", "default=nw=1"])
        .arg(&segment)
        .output()
        .expect("running ffprobe");
    let report = String::from_utf8_lossy(&probe.stdout);
    // Printed, not just asserted on: this is the evidence, and a run that
    // bothered to encode a real Profile 5 sample should say what came out.
    eprintln!("HDR10 rung, measured from the production argument list:\n{report}");
    for expected in [
        "codec_name=hevc",
        "profile=Main 10",
        "pix_fmt=yuv420p10le",
        "color_range=tv",
        "color_space=bt2020nc",
        "color_transfer=smpte2084",
        "color_primaries=bt2020",
    ] {
        assert!(
            report.contains(expected),
            "the produced stream is not {expected}: {report}"
        );
    }

    // The RPU was consumed, not passed on. A stream that still carried it
    // would be Dolby Vision advertised as HDR10 — the reverse of the
    // mislabel this rung exists to fix, and just as wrong.
    let side = std::process::Command::new(&ffprobe)
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-read_intervals", "%+#1", "-show_frames"])
        .args(["-show_entries", "frame=side_data_list", "-of", "default"])
        .arg(&segment)
        .output()
        .expect("running ffprobe");
    let side = String::from_utf8_lossy(&side.stdout);
    assert!(
        !side.contains("Dolby Vision"),
        "the reshape left Dolby Vision metadata behind: {side}"
    );

    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn tonemap_none_passes_hdr_through() {
    // PLURX_TONEMAP=off → no tone-map filter, HDR just normalized to yuv420p.
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        tone_map: ToneMap::None,
        ..Default::default()
    };
    let args = hls_args(
        &file(Some("dolby_vision")),
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(!joined.contains("tonemap"));
    assert!(!joined.contains("zscale"));
    assert!(joined.contains("format=yuv420p"));
}

#[test]
fn start_offset_seeks_input() {
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        start_seconds: 90.5,
        ..Default::default()
    };
    let args = hls_args(
        &file(None),
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    );
    // -ss must come before -i for fast input seeking.
    let ss = args.iter().position(|a| a == "-ss").expect("has -ss");
    let i = args.iter().position(|a| a == "-i").expect("has -i");
    assert!(ss < i);
    assert_eq!(args[ss + 1], "90.500");
}

#[test]
fn text_subtitle_burn_uses_libass() {
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 2,
            bitmap: false,
        }),
        ..Default::default()
    };
    let args = hls_args(
        &file(None),
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        "/tmp/s",
    );
    assert!(args.join(" ").contains("subtitles='/media/movie.mkv':si=2"));
}

#[test]
fn cached_text_burn_keeps_gpu_tonemap_and_round_trips_once() {
    let mut f = file(Some("dolby_vision"));
    f.width = Some(3840);
    f.height = Some(2160);
    f.bit_depth = Some(10);
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        output_codec: None,
        strict_dolby: None,
        target_height: 1080,
        pipeline: Pipeline::VppQsv,
        subtitle_burn: Some(SubtitleBurn {
            subtitle_index: 2,
            bitmap: false,
        }),
        subtitle_file: Some(PathBuf::from("/cache/scary-forced.vtt")),
        ..Default::default()
    };
    let args = hls_args(&f, Encoder::Qsv, &opts, Pacing::unpaced(), "/tmp/s");
    let joined = args.join(" ");
    let filter = args
        .iter()
        .position(|arg| arg == "-vf")
        .expect("video filter");
    let provider = if cfg!(target_os = "macos") {
        ":font_provider=fontconfig"
    } else {
        ""
    };
    assert_eq!(
            args[filter + 1],
            format!(
                "vpp_qsv=w=1920:h=1080:tonemap=1:format=nv12,hwdownload,format=nv12,subtitles='/cache/scary-forced.vtt'{provider},hwupload=extra_hw_frames=64,format=qsv"
            ),
            "{joined}"
        );
    assert_eq!(joined.matches("hwdownload").count(), 1, "{joined}");
    assert_eq!(joined.matches("hwupload").count(), 1, "{joined}");
    assert!(!joined.contains("zscale"), "{joined}");
}

#[test]
fn copy_args_keep_video_and_package_fmp4_hls() {
    // DTS → AAC, HEVC video copied, delivered as fMP4 HLS (the Safari path).
    let args = hls_copy_args(
        &file(Some("hdr10")),
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
        "/tmp/sess",
    );
    let joined = args.join(" ");
    assert!(joined.contains("-c:v copy"));
    assert!(joined.contains("-tag:v hvc1")); // HEVC needs hvc1 for Safari
    assert!(joined.contains("-c:a aac")); // audio transcoded when asked
    assert!(joined.contains("-hls_segment_type fmp4")); // not mpegts
    assert!(joined.contains("-hls_fmp4_init_filename init.mp4"));
    // An open-GOP copy cannot promise independently decodable segments,
    // and a player that believes the promise discards one leading picture
    // per boundary. The transcode path (closed GOP) keeps the tag.
    assert!(!joined.contains("independent_segments"));
    assert!(joined.contains("-hls_flags temp_file"));
    // Every boundary costs a leading picture, so the copy path asks for
    // long segments; the burst covers the first-segment wait. The
    // transcode path keeps SEGMENT_SECONDS — TTFF at 1x encode.
    assert!(joined.contains(&format!("-hls_time {COPY_SEGMENT_SECONDS}")));
    assert!(!joined.contains(&format!("-hls_time {SEGMENT_SECONDS} ")));
    assert!(joined.contains("/tmp/sess/seg%05d.m4s"));
    assert!(joined.contains("/tmp/sess/index.m3u8"));
    // No re-encode: none of the transcode machinery leaks in.
    assert!(!joined.contains("libx264"));
    assert!(!joined.contains("scale="));
    assert!(!joined.contains("tonemap"));
}

#[test]
fn audio_only_copy_can_convert_an_audiobook_without_a_video_stream() {
    let mut media = file(None);
    media.video_codec = None;
    media.video_profile = None;
    media.width = None;
    media.height = None;
    media.bit_depth = None;
    media.audio_streams = vec![crate::domain::AudioStream {
        index: 0,
        codec: "wmav2".into(),
        channels: Some(2),
        default: true,
        ..Default::default()
    }];

    let joined = hls_copy_args(
        &media,
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
        "/tmp/audiobook",
    )
    .join(" ");
    assert!(joined.contains("-map 0:v:0?"));
    assert!(joined.contains("-map 0:a:0?"));
    assert!(joined.contains("-c:a aac"));
}

#[test]
fn every_lossy_rolling_audio_output_is_fixed_at_forty_eight_khz() {
    let media = file(None);
    let encoded = hls_args(
        &media,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/encoded",
    )
    .join(" ");
    assert!(
        encoded.contains("-c:a aac -ac 2 -b:a 160k -ar 48000"),
        "{encoded}"
    );

    let converted_copy = hls_copy_args(
        &media,
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
        "/tmp/copy",
    )
    .join(" ");
    assert!(
        converted_copy.contains("-c:a aac -b:a 256k -ar 48000"),
        "{converted_copy}"
    );

    let copied = hls_copy_args(
        &media,
        0.0,
        None,
        false,
        Pacing::unpaced(),
        false,
        "/tmp/copy",
    )
    .join(" ");
    assert!(
        !copied.contains("-ar 48000"),
        "copy must not rewrite source audio: {copied}"
    );
}

#[test]
fn six_channel_copy_audio_uses_an_explicit_standard_aac_layout() {
    let mut media = file(Some("dolby_vision"));
    media.audio_streams = vec![
        crate::domain::AudioStream {
            index: 0,
            codec: "ac3".into(),
            channels: Some(2),
            ..Default::default()
        },
        crate::domain::AudioStream {
            index: 1,
            codec: "dts".into(),
            channels: Some(6),
            ..Default::default()
        },
    ];

    let surround = hls_copy_args(
        &media,
        0.0,
        Some(1),
        true,
        Pacing::unpaced(),
        true,
        "/tmp/s",
    )
    .join(" ");
    assert!(surround.contains("-c:a aac -b:a 320k -channel_layout:a 5.1"));

    let stereo = hls_copy_args(
        &media,
        0.0,
        Some(0),
        true,
        Pacing::unpaced(),
        true,
        "/tmp/s",
    )
    .join(" ");
    assert!(stereo.contains("-c:a aac -b:a 256k"));
    assert!(!stereo.contains("-channel_layout:a"));
}

#[test]
fn typed_audio_argv_preserves_legacy_six_and_propagates_explicit_surround() {
    use crate::playback::{
        audio::{resolve_audio, AudioAction, AudioDelivery, AudioRoute},
        default_profile,
    };
    let mut media = file(None);
    media.audio_streams = vec![crate::domain::AudioStream {
        codec: "dts".into(),
        channels: Some(6),
        index: 1,
        ..Default::default()
    }];
    let mut audio = resolve_audio(
        media.audio_streams.first(),
        default_profile(),
        AudioRoute::Progressive,
        0,
    );
    let video = CopyVideoOptions::new(false, false);
    assert_eq!(
        copy_pipe_args_with_audio_delivery(
            &media,
            0.0,
            Some(1),
            true,
            Pacing::unpaced(),
            video,
            Some(&audio)
        ),
        copy_pipe_args_with_dolby_vision(&media, 0.0, Some(1), true, Pacing::unpaced(), video)
    );
    audio.reason = "a refreshed explanation".into();
    assert_eq!(
        copy_pipe_args_with_audio_delivery(
            &media,
            0.0,
            Some(1),
            true,
            Pacing::unpaced(),
            video,
            Some(&audio)
        ),
        copy_pipe_args_with_dolby_vision(&media, 0.0, Some(1), true, Pacing::unpaced(), video)
    );
    let explicit = AudioDelivery {
        action: AudioAction::Encode {
            codec: "eac3".into(),
            channels: 6,
            layout: Some("5.1".into()),
            bitrate_kbps: 640,
            sample_rate: 48_000,
        },
        downmix: None,
        reason: "explicit route".into(),
    };
    let mut options = TranscodeOptions::default();
    options.set_audio_delivery(explicit);
    let args = hls_args(
        &media,
        Encoder::Software,
        &options,
        Pacing::unpaced(),
        "/tmp/typed",
    )
    .join(" ");
    assert!(
        args.contains("-c:a eac3 -ac 6 -channel_layout:a 5.1 -b:a 640k -ar 48000"),
        "{args}"
    );
    assert!(!args.contains("pan=") && !args.contains("alimiter="));
}

/// ffmpeg keeps only the last `-af`. The measured fold and the per-file
/// A/V correction therefore travel as one chain — fold first — on the
/// rolling transcode and on the copy-video conversion alike.
#[test]
fn a_stereo_fold_and_the_offset_share_one_audio_filter_chain() {
    use crate::playback::audio::{resolve_audio, AudioRoute, DownmixMatrix};
    let mut media = file(None);
    media.audio_offset_ms = 250;
    media.audio_streams = vec![crate::domain::AudioStream {
        codec: "dts".into(),
        channels: Some(6),
        channel_layout: Some("5.1(side)".into()),
        sample_rate: Some(48_000),
        index: 1,
        ..Default::default()
    }];
    let fold = DownmixMatrix::LoRo51.filter().expect("measured fold");
    let filters = |args: &[String]| -> Vec<String> {
        args.windows(2)
            .filter(|pair| pair[0] == "-af")
            .map(|pair| pair[1].clone())
            .collect()
    };

    let mut options = TranscodeOptions::default();
    options.set_audio_delivery(resolve_audio(
        media.audio_streams.first(),
        crate::playback::default_profile(),
        AudioRoute::RollingHls,
        media.audio_offset_ms,
    ));
    let args = hls_args(
        &media,
        Encoder::Software,
        &options,
        Pacing::unpaced(),
        "/tmp/f",
    );
    assert_eq!(filters(&args), vec![format!("{fold},adelay=250:all=1")]);
    assert!(args
        .join(" ")
        .contains("-c:a aac -ac 2 -b:a 160k -ar 48000"));

    // The copy-video path converting to stereo for a stereo sink.
    let mut stereo = crate::playback::default_profile().clone();
    stereo.audio_codecs = vec!["aac".into()];
    stereo.max_audio_channels = [("aac".to_owned(), 2)].into_iter().collect();
    stereo.claimed_audio_decoders = ["aac".to_owned()].into_iter().collect();
    stereo.audio_sink_claims = [(
        "aac".to_owned(),
        crate::playback::audio::AudioSink {
            codec: "aac".into(),
            max_channels: 2,
            passthrough: false,
            sample_rates_hz: vec![48_000],
        },
    )]
    .into_iter()
    .collect();
    let audio = resolve_audio(
        media.audio_streams.first(),
        &stereo,
        AudioRoute::Progressive,
        media.audio_offset_ms,
    );
    assert_eq!(audio.downmix, Some(DownmixMatrix::LoRo51));
    let args = copy_pipe_args_with_audio_delivery(
        &media,
        0.0,
        Some(1),
        true,
        Pacing::unpaced(),
        CopyVideoOptions::new(false, false),
        Some(&audio),
    );
    assert_eq!(filters(&args), vec![format!("{fold},adelay=250:all=1")]);

    // Without an offset the chain is the fold alone; a stereo source
    // carries no filter at all.
    media.audio_offset_ms = 0;
    let args = hls_args(
        &media,
        Encoder::Software,
        &options,
        Pacing::unpaced(),
        "/tmp/f",
    );
    assert_eq!(filters(&args), vec![fold]);
    media.audio_streams[0].channels = Some(2);
    media.audio_streams[0].channel_layout = Some("stereo".into());
    let mut plain = TranscodeOptions::default();
    plain.set_audio_delivery(resolve_audio(
        media.audio_streams.first(),
        crate::playback::default_profile(),
        AudioRoute::RollingHls,
        0,
    ));
    let args = hls_args(
        &media,
        Encoder::Software,
        &plain,
        Pacing::unpaced(),
        "/tmp/f",
    );
    assert!(filters(&args).is_empty());
}

/// The `hvc1` tag promises no in-band parameter sets, and a
/// Blu-ray-lineage copy breaks that promise at every IRAP — which on this
/// path is every segment boundary. The filter is what keeps the promise;
/// a Dolby Vision source additionally sheds the EL/RPU units nothing on
/// an hvc1 wire can use.
#[test]
fn copied_hevc_keeps_the_hvc1_promise() {
    let joined = hls_copy_args(
        &file(Some("hdr10")),
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
        "/tmp/s",
    )
    .join(" ");
    assert!(joined.contains("-bsf:v filter_units=remove_types=32-34"));
    assert!(!joined.contains("62-63"), "HDR10 keeps nothing to shed");

    let dv = hls_copy_args(
        &file(Some("dolby_vision")),
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
        "/tmp/s",
    )
    .join(" ");
    assert!(dv.contains("-bsf:v filter_units=remove_types=32-34|62-63"));

    // With a capable ffmpeg the DV strip also removes the container
    // signaling: dovi_rpu drops the DOVI side data, so the muxer writes
    // no dvcC and the stream stops claiming a profile nothing supports.
    let dv7 = hls_copy_args(
        &file(Some("dolby_vision")),
        0.0,
        None,
        true,
        Pacing::unpaced(),
        true,
        "/tmp/s",
    )
    .join(" ");
    assert!(dv7.contains("-bsf:v dovi_rpu=strip=1,filter_units=remove_types=32-34|62-63"));

    // A backward-compatible profile the client accepts keeps its hvc1
    // base sample entry and its RPU/EL units. Apple uses the base entry
    // together with supplemental codec signaling to engage Dolby Vision.
    let mut compatible_dv = file(Some("dolby_vision"));
    compatible_dv.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".into());
    let preserved = hls_copy_args_with_dolby_vision(
        &compatible_dv,
        0.0,
        None,
        true,
        Pacing::unpaced(),
        DolbyVisionCopyOptions::new(true, true),
        "/tmp/s",
    )
    .join(" ");
    assert!(preserved.contains("-tag:v hvc1"));
    assert!(preserved.contains("-strict unofficial"));
    assert!(preserved.contains("-bsf:v filter_units=remove_types=32-34"));
    assert!(!preserved.contains("62-63"));
    assert!(!preserved.contains("dovi_rpu=strip=1"));
    assert!(!dv7.contains("-strict unofficial"));

    let profile5 = hls_copy_args_with_dolby_vision(
        &file(Some("dolby_vision")),
        0.0,
        None,
        true,
        Pacing::unpaced(),
        DolbyVisionCopyOptions::new(true, true),
        "/tmp/s",
    )
    .join(" ");
    assert!(profile5.contains("-tag:v dvh1"));
    assert!(profile5.contains("-strict unofficial"));
    assert!(
        !profile5.contains("-bsf:v"),
        "Profile 5 parameter sets must reach the segmenter for hvcC promotion"
    );

    // H.264 copies are untouched: they are not on the stuttering path and
    // avc1 + in-band parameter sets plays everywhere today.
    let mut f = file(None);
    f.video_codec = Some("h264".into());
    let h264 = hls_copy_args(&f, 0.0, None, true, Pacing::unpaced(), true, "/tmp/s").join(" ");
    assert!(!h264.contains("-bsf:v"));
    assert!(!h264.contains("-tag:v"));
}

#[test]
fn takeover_copy_uses_generation_specific_object_names() {
    let args = hls_copy_args_with_sequence(
        &file(Some("hdr10")),
        42.0,
        None,
        false,
        Pacing::unpaced(),
        DolbyVisionCopyOptions::new(true, false),
        17,
        "init-e3.mp4",
        "/tmp/s",
    )
    .join(" ");
    assert!(args.contains("-start_number 17"), "{args}");
    assert!(
        args.contains("-hls_fmp4_init_filename init-e3.mp4"),
        "{args}"
    );
}

/// The gate on dovi_rpu is an ffmpeg version parse, and getting it wrong
/// in either direction has a cost: too eager is a hard session exit on an
/// older build ("Unknown bit stream filter"), too shy leaves the dvcC box
/// that made Safari software-decode 4K on an M3 Max.
#[test]
fn dovi_bsf_gate_reads_real_version_lines() {
    assert!(ffmpeg_has_dovi_bsf(
        "ffmpeg version 7.1.4-Jellyfin Copyright (c) 2000-2026"
    ));
    assert!(ffmpeg_has_dovi_bsf("ffmpeg version 7.1"));
    assert!(ffmpeg_has_dovi_bsf("ffmpeg version 8.0-static"));
    assert!(!ffmpeg_has_dovi_bsf(
        "ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2023"
    ));
    assert!(!ffmpeg_has_dovi_bsf("ffmpeg version 7.0.2"));
    assert!(!ffmpeg_has_dovi_bsf("ffmpeg version 7"));
    assert!(!ffmpeg_has_dovi_bsf(""));
    assert!(!ffmpeg_has_dovi_bsf("not a version line at all"));
    // The n-prefixed spelling some builds use.
    assert!(ffmpeg_has_dovi_bsf("ffmpeg version n7.1.2"));
}

#[test]
fn copy_args_copy_audio_when_supported() {
    let args = hls_copy_args(
        &file(None),
        30.0,
        Some(1),
        false,
        Pacing::unpaced(),
        false,
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(joined.contains("-c:a copy")); // transcode_audio = false
    assert!(joined.contains("0:a:1?") || joined.contains("a:1?")); // chosen track
                                                                   // -ss before -i for fast input seek.
    let ss = args.iter().position(|a| a == "-ss").expect("has -ss");
    let i = args.iter().position(|a| a == "-i").expect("has -i");
    assert!(ss < i);
}

/// A copy-video seek may start at the prior keyframe. Accurate input seek
/// would discard encoded audio until the requested instant but retain that
/// video preroll, which is an immediate A/V offset after every seek.
#[test]
fn copy_seek_preserves_audio_preroll() {
    let args = hls_copy_args(
        &file(None),
        32.125,
        None,
        true,
        Pacing::unpaced(),
        false,
        "/tmp/s",
    );
    let ss = args.iter().position(|a| a == "-ss").expect("has -ss");
    assert_eq!(args[ss - 1], "-noaccurate_seek");
    assert_eq!(args[ss + 1], "32.125");
    let input = args.iter().position(|a| a == "-i").expect("has input");
    assert!(ss < input, "seek flags must remain input options");

    assert!(copy_input_seek_args(0.0).is_empty());
    assert!(copy_input_seek_args(-1.0).is_empty());
}

#[test]
fn copy_pipe_uses_the_edit_free_apple_hls_timeline() {
    let args = copy_pipe_args(&file(None), 30.0, None, false, Pacing::unpaced(), false);
    let option = args
        .iter()
        .position(|arg| arg == "-use_editlist")
        .expect("Apple HLS edit-list policy");
    assert_eq!(args.get(option + 1).map(String::as_str), Some("0"));
}

/// Exercise the actual ffmpeg boundary that exposed the bug: the seek is
/// between video keyframes and audio is re-encoded. The first timestamps
/// must still land together rather than leaving silence until the exact
/// requested instant.
#[test]
fn copy_seek_emits_aligned_audio_and_video() {
    use crate::testfixtures::{ffmpeg, ffprobe, run, source};
    use std::process::Command;

    let mut media = file(None);
    media.path = source("h264");
    media.video_codec = Some("h264".into());
    let bytes = run(Command::new(ffmpeg()).args(copy_pipe_args(
        &media,
        3.0,
        None,
        true,
        Pacing::unpaced(),
        false,
    )));
    assert!(
        !bytes
            .windows(4)
            .any(|window| window == b"edts" || window == b"elst"),
        "Apple HLS copy output must not carry movie edit lists"
    );
    let output = tempfile::Builder::new()
        .suffix(".mp4")
        .tempfile()
        .expect("temporary remux");
    std::fs::write(output.path(), bytes).expect("write remux");

    let probe = run(Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,start_time",
            "-of",
            "json",
        ])
        .arg(output.path()));
    let value: serde_json::Value = serde_json::from_slice(&probe).expect("ffprobe json");
    let starts = value["streams"].as_array().expect("stream list");
    let start = |kind: &str| {
        starts
            .iter()
            .find(|stream| stream["codec_type"] == kind)
            .and_then(|stream| stream["start_time"].as_str())
            .and_then(|time| time.parse::<f64>().ok())
            .unwrap_or_else(|| panic!("{kind} start time in {value}"))
    };
    let skew = (start("video") - start("audio")).abs();
    assert!(skew < 0.150, "A/V starts differ by {skew:.3}s: {value}");
}

/// Pacing flags are *input* options: they must land before their own `-i`,
/// and a second input (the A/V-offset case) needs its own copy or the muxer
/// interleave drags the whole pipeline back to flat-out.
#[test]
fn pacing_precedes_every_input() {
    let pacing = Pacing {
        readrate: Some(2.0),
        initial_burst: Some(90.0),
        legacy_re: false,
    };
    let mut f = file(None);
    f.audio_offset_ms = 250;
    // Copied audio: the one case that still forces the second input —
    // and an offset only applies to a file that has audio at all.
    f.audio_streams = vec![crate::domain::AudioStream {
        index: 0,
        codec: "dts".into(),
        ..Default::default()
    }];
    let args = hls_copy_args(&f, 0.0, None, false, pacing, false, "/tmp/s");

    let inputs: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "-i")
        .map(|(i, _)| i)
        .collect();
    let rates: Vec<usize> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "-readrate")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(inputs.len(), 2, "offset file remuxes from two inputs");
    assert_eq!(rates.len(), 2, "every input is paced, not just the first");
    for (rate, input) in rates.iter().zip(&inputs) {
        assert!(rate < input, "pacing is an input option, so it leads -i");
    }
    assert!(args.join(" ").contains("-readrate_initial_burst 90.0"));
    // The old realtime pacing is gone: it is what starved the buffer.
    assert!(!args.contains(&"-re".to_owned()));
}

/// §3.4: where the audio is being encoded anyway, the A/V correction is
/// a filter on the ONE input — the second `-itsoffset` input read the
/// whole source again for nothing but its audio. Copy semantics (and
/// only copy semantics) keep the second input.
#[test]
fn an_encoded_audio_offset_needs_no_second_read() {
    let audio = |f: &mut MediaFile| {
        f.audio_streams = vec![crate::domain::AudioStream {
            index: 0,
            codec: "dts".into(),
            ..Default::default()
        }];
    };
    let inputs = |args: &[String]| args.iter().filter(|a| *a == "-i").count();

    // Transcode path: audio is always AAC, so always one input.
    let mut f = file(None);
    f.audio_offset_ms = 250;
    audio(&mut f);
    let args = hls_args(
        &f,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    assert_eq!(inputs(&args), 1, "no second read of the source");
    assert!(!args.contains(&"-itsoffset".to_owned()));
    let af = args.iter().position(|a| a == "-af").expect("-af");
    assert_eq!(args[af + 1], "adelay=250:all=1", "positive = audio later");
    assert!(
        args.contains(&"0:a:0?".to_owned()),
        "audio maps from input 0"
    );

    // Negative offset: pull audio earlier by trimming its head — what
    // -itsoffset dropped before the video's start anyway.
    f.audio_offset_ms = -500;
    let args = hls_args(
        &f,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    let af = args.iter().position(|a| a == "-af").expect("-af");
    assert_eq!(args[af + 1], "atrim=start=0.500,asetpts=PTS-STARTPTS");

    // Copy path, audio transcoded (DTS→AAC): same one-input treatment.
    let args = hls_copy_args(&f, 0.0, None, true, Pacing::unpaced(), false, "/tmp/s");
    assert_eq!(
        inputs(&args),
        1,
        "an encoded copy-session audio needs no second read"
    );
    assert!(args.iter().any(|a| a.starts_with("atrim=")));

    // Copy path, audio COPIED: packets cannot be filtered — the second
    // input is retained, exactly as before.
    let args = hls_copy_args(&f, 0.0, None, false, Pacing::unpaced(), false, "/tmp/s");
    assert_eq!(inputs(&args), 2, "copy semantics keep the -itsoffset input");
    assert!(args.contains(&"-itsoffset".to_owned()));
    assert!(!args.contains(&"-af".to_owned()));

    // A file with no audio track: an offset row is metadata about
    // nothing — no filter (a hard error to attach), no second input.
    let mut mute = file(None);
    mute.audio_offset_ms = 250;
    let args = hls_args(
        &mute,
        Encoder::Software,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    assert_eq!(inputs(&args), 1);
    assert!(!args.contains(&"-af".to_owned()));
}

#[test]
fn pacing_degrades_with_the_ffmpeg_build() {
    let out =
        |p: Pacing| hls_copy_args(&file(None), 0.0, None, false, p, false, "/tmp/s").join(" ");
    // Nothing configured (or pacing switched off): no flags at all.
    assert!(!out(Pacing::unpaced()).contains("-readrate"));
    assert!(!out(Pacing::unpaced()).contains("-re "));
    // 5.1–6.0: rate limiting, no burst clause.
    let rate_only = Pacing {
        readrate: Some(3.0),
        initial_burst: None,
        legacy_re: false,
    };
    assert!(out(rate_only).contains("-readrate 3.00"));
    assert!(!out(rate_only).contains("initial_burst"));
    // Pre-5.1: bare `-re`, and never both.
    let legacy = Pacing {
        readrate: Some(2.0),
        initial_burst: Some(90.0),
        legacy_re: true,
    };
    assert!(out(legacy).contains("-re "));
    assert!(!out(legacy).contains("-readrate"));
}

/// A transcode session is paced too — the same burst-then-hold shape. An
/// encoder faster than realtime otherwise races the playhead and writes
/// segments nobody fetches.
#[test]
fn transcode_input_is_paced_too() {
    let pacing = Pacing {
        readrate: Some(2.0),
        initial_burst: Some(90.0),
        legacy_re: false,
    };
    let args = hls_args(
        &file(None),
        Encoder::Software,
        &TranscodeOptions::default(),
        pacing,
        "/tmp/s",
    );
    let rate = args
        .iter()
        .position(|a| a == "-readrate")
        .expect("paced input");
    let input = args.iter().position(|a| a == "-i").expect("has input");
    assert!(rate < input);
}

#[test]
fn nvenc_swaps_encoder_and_adds_decode() {
    let args = hls_args(
        &file(None),
        Encoder::Nvenc,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(joined.contains("h264_nvenc"));
    assert!(joined.contains("cuda")); // hwaccel decode
}

#[test]
fn qsv_hardware_decodes_heavy_hevc() {
    // 4K 10-bit Dolby Vision HEVC (the file() helper) → GPU decode so it
    // isn't CPU-bound, with an hwdownload to feed the CPU tonemap filters.
    let args = hls_args(
        &file(Some("dolby_vision")),
        Encoder::Qsv,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(joined.contains("-hwaccel qsv"));
    assert!(joined.contains("-hwaccel_output_format qsv"));
    assert!(joined.contains("hwdownload,format=p010le")); // 10-bit surface
    assert!(joined.contains("h264_qsv")); // still hardware encode
}

#[test]
fn qsv_software_decodes_light_source() {
    // 1080p 8-bit H.264 doesn't need GPU decode — keep the compatible
    // software-decode path, but still hardware-encode.
    let mut f = file(None);
    f.video_codec = Some("h264".into());
    f.width = Some(1920);
    f.height = Some(1080);
    f.bit_depth = Some(8);
    f.hdr = None;
    let args = hls_args(
        &f,
        Encoder::Qsv,
        &TranscodeOptions::default(),
        Pacing::unpaced(),
        "/tmp/s",
    );
    let joined = args.join(" ");
    assert!(!joined.contains("-hwaccel qsv")); // software decode
    assert!(!joined.contains("hwdownload"));
    assert!(joined.contains("h264_qsv")); // hardware encode
}

// ---- The tomb for P0-2 and P0-3 ------------------------------------
//
// Both of those defects lived in the gap between what the argument-shape
// tests assert and what the muxers actually emit: the arguments were
// exactly as intended, and the bytes that came out did not start where
// every consumer of them assumed. Nothing short of producing a session
// and measuring it can close that gap, so these tests run ffmpeg.

fn ffmpeg() -> String {
    std::env::var("PLURX_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

fn ffprobe() -> String {
    std::env::var("PLURX_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

/// A real clip with a deliberately LONG GOP, because a short-GOP fixture
/// hides P0-2: when every second is a keyframe, the requested start and
/// the keyframe before it are never far enough apart to notice.
fn write_long_gop_clip(path: &std::path::Path, seconds: u32, gop_seconds: u32) {
    let fps = 25;
    let status = std::process::Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=192x108:rate={fps}:duration={seconds}"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:duration={seconds}"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            &(fps * gop_seconds).to_string(),
            "-keyint_min",
            &(fps * gop_seconds).to_string(),
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-shortest",
            "-y",
        ])
        .arg(path)
        .status();
    assert!(
        status.map(|s| s.success()).unwrap_or(false),
        "fixture encode failed — this test needs a working ffmpeg"
    );
}

/// First video packet timestamps of a produced artefact, `(pts, dts)`.
fn first_packet_times(path: &std::path::Path) -> (f64, f64) {
    let out = std::process::Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,dts_time",
            "-read_intervals",
            "%+#1",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe the produced segment");
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text.lines().next().unwrap_or_default();
    let mut fields = row.split(',');
    let pts = fields
        .next()
        .unwrap_or("")
        .trim()
        .parse()
        .unwrap_or(f64::NAN);
    let dts = fields
        .next()
        .unwrap_or("")
        .trim()
        .parse()
        .unwrap_or(f64::NAN);
    (pts, dts)
}

fn run_ffmpeg(args: &[String]) {
    let out = std::process::Command::new(ffmpeg())
        .args(args)
        .output()
        .expect("run the produced argument list");
    assert!(
        out.status.success(),
        "ffmpeg refused the produced arguments: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// P0-3's tomb. The subtitle slicer anchors segment 0 with
/// `X-TIMESTAMP-MAP=MPEGTS:0`, so segment 0 has to actually begin at PTS
/// 0. ffmpeg's mpegts defaults (`muxpreload` 0.5 + `muxdelay` 0.7) put it
/// at ~1.4 s instead, and every native cue on a transcode session
/// rendered that much early. Measured, not asserted from the flag list:
/// the flag list was never the thing that was wrong.
#[test]
fn a_transcode_session_starts_its_mpegts_timeline_at_zero() {
    let dir = tempfile::tempdir().expect("work dir");
    let src = dir.path().join("clip.mp4");
    write_long_gop_clip(&src, 20, 5);

    let mut source = file(None);
    source.path = src.clone();
    source.width = Some(192);
    source.height = Some(108);
    source.hdr = None;
    let out_dir = dir.path().join("tx");
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let opts = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        target_height: 108,
        start_seconds: 12.3,
        tone_map: ToneMap::None,
        ..Default::default()
    };
    run_ffmpeg(&hls_args(
        &source,
        Encoder::Software,
        &opts,
        Pacing::unpaced(),
        &out_dir.to_string_lossy(),
    ));

    let (pts, dts) = first_packet_times(&out_dir.join("seg00000.ts"));
    // One frame of slack at 25 fps, and not a millisecond more: the whole
    // point is that `MPEGTS:0` is a true statement about these bytes.
    assert!(
        dts.abs() <= 0.04 && pts.abs() <= 0.08,
        "segment 0 begins at pts {pts} / dts {dts}, so X-TIMESTAMP-MAP=MPEGTS:0 is a lie \
             and every native cue renders that much early"
    );
}

/// P0-2's tomb. A copy session seeks with `-noaccurate_seek`, so the
/// media it emits begins at the keyframe BEFORE the requested start, and
/// a cue shift computed from the request leads the picture by the
/// difference. The probe has to find that keyframe, and the produced
/// stream has to agree with it.
#[test]
fn a_copy_session_begins_at_the_keyframe_before_the_requested_start() {
    let dir = tempfile::tempdir().expect("work dir");
    let src = dir.path().join("clip.mp4");
    // 5 s GOPs, so a start of 12.3 s is 2.3 s past its keyframe.
    write_long_gop_clip(&src, 30, 5);
    let requested = 12.3;

    // What the probe says.
    let probe = std::process::Command::new(ffprobe())
        .args(keyframe_probe_args(&src.to_string_lossy(), requested))
        .output()
        .expect("probe the source");
    assert!(
        probe.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let origin = parse_keyframe_origin(&String::from_utf8_lossy(&probe.stdout))
        .expect("the probe found a keyframe");
    assert!(
        origin < requested - 1.0,
        "a 5 s-GOP fixture must put the keyframe well before a 12.3 s start, got {origin}"
    );

    // What the session actually produces. The honest measure is how much
    // media comes out: a copy that begins at the keyframe emits
    // `duration - origin` seconds, not `duration - requested`.
    let mut source = file(None);
    source.path = src.clone();
    source.width = Some(192);
    source.height = Some(108);
    source.video_codec = Some("h264".into());
    source.hdr = None;
    source.audio_streams = vec![crate::domain::AudioStream {
        index: 0,
        codec: "aac".into(),
        channels: Some(1),
        default: true,
        ..Default::default()
    }];
    let out_dir = dir.path().join("cp");
    std::fs::create_dir_all(&out_dir).expect("out dir");
    run_ffmpeg(&hls_copy_args(
        &source,
        requested,
        None,
        false,
        Pacing::unpaced(),
        false,
        &out_dir.to_string_lossy(),
    ));

    let playlist =
        std::fs::read_to_string(out_dir.join("index.m3u8")).expect("copy session playlist");
    let produced: f64 = playlist
        .lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .filter_map(|value| {
            value
                .split_once(',')
                .map_or(value, |(duration, _)| duration)
                .parse::<f64>()
                .ok()
        })
        .sum();
    let from_origin = 30.0 - origin;
    assert!(
        (produced - from_origin).abs() < 0.5,
        "the session produced {produced}s of media; beginning at the probed origin \
             ({origin}s) predicts {from_origin}s. If it instead matches {}s, the media \
             begins at the REQUEST and the probe is wrong; if neither, the seek changed.",
        30.0 - requested
    );
    // And the defect restated as the assertion that would have failed
    // before the fix: shifting cues by the request rather than the origin
    // moves every one of them this far off the picture.
    assert!(
        requested - origin > 1.0,
        "this fixture must actually exercise the lead, got {}s",
        requested - origin
    );
}
