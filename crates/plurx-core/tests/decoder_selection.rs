use plurx_core::domain::DolbyVisionFacts;
use plurx_core::transcode::{
    resolve_transcode, AttemptRestrictions, CapabilityStatus, DecodeBackend, DecodeCapabilities,
    DecodeCapability, DecodeCapabilitySnapshotIdentity, DecodeCatalogMetadata, DecodeEvidence,
    DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot, DecodeReason, DecodeSourceIdentity,
    DecodeSurfaceContract, EffectiveRateControl, Encoder, FrameDomain, FrameRateProvenance,
    OutputGrade, Pipeline, PlanError, SoftwareDecoder, StreamSelectionProvenance,
    SubtitleRendering, ToneMap, TranscodeMediaOptions, TranscodeRequest,
};
use serde_json::{json, Value};

fn identity(byte: char) -> DecodeSourceIdentity {
    DecodeSourceIdentity::from_sha256(byte.to_string().repeat(64)).expect("valid digest")
}

fn video(
    index: u32,
    codec: Option<&str>,
    profile: Option<&str>,
    width: i64,
    height: i64,
    pixel_format: Option<&str>,
    average_rate: &str,
    nominal_rate: &str,
    transfer: Option<&str>,
) -> Value {
    json!({
        "index": index,
        "codec_type": "video",
        "codec_name": codec,
        "profile": profile,
        "width": width,
        "height": height,
        "pix_fmt": pixel_format,
        "avg_frame_rate": average_rate,
        "r_frame_rate": nominal_rate,
        "color_transfer": transfer,
        "disposition": {"attached_pic": 0}
    })
}

fn facts(stream: Value) -> DecodeFacts {
    DecodeFacts::from_ffprobe_json(&json!({"streams": [stream]}), identity('a'))
        .expect("valid decode facts")
}

fn options(pipeline: Pipeline) -> TranscodeMediaOptions {
    TranscodeMediaOptions {
        target_height: 1080,
        video_bitrate_kbps: 8_000,
        effective_rate_control: EffectiveRateControl::Vbr,
        audio_channels: 2,
        audio_bitrate_kbps: 160,
        audio_index: None,
        audio_offset_ms: 0,
        tone_map: ToneMap::Zscale,
        pipeline,
        subtitle_burn: None,
    }
}

fn capability(
    backend: DecodeBackend,
    codec: &str,
    profile: Option<&str>,
    pixel_format: Option<&str>,
    status: CapabilityStatus,
) -> DecodeCapability {
    DecodeCapability {
        backend,
        codec: codec.to_owned(),
        profile: profile.map(str::to_owned),
        pixel_format: pixel_format.map(str::to_owned),
        bit_depth: None,
        dynamic_range: None,
        max_width: None,
        max_height: None,
        max_pixel_rate: None,
        surface: None,
        status,
    }
}

fn snapshot_identity() -> DecodeCapabilitySnapshotIdentity {
    DecodeCapabilitySnapshotIdentity::new(
        "f".repeat(64),
        "test-node".to_owned(),
        Some("e".repeat(64)),
    )
    .expect("valid snapshot identity")
}

fn exact_capability(
    backend: DecodeBackend,
    input: &DecodeFacts,
    pipeline: Pipeline,
    encoder: Encoder,
    subtitle_rendering: SubtitleRendering,
    status: CapabilityStatus,
) -> DecodeCapability {
    let rate = input.frame_rate().value().expect("known fixture rate");
    let pixels = u128::from(input.width().expect("known fixture width"))
        * u128::from(input.height().expect("known fixture height"))
        * u128::from(rate.numerator());
    let pixel_rate = pixels.div_ceil(u128::from(rate.denominator()));
    DecodeCapability {
        backend,
        codec: input.codec().expect("known fixture codec").to_owned(),
        profile: Some(input.profile().expect("known fixture profile").to_owned()),
        pixel_format: Some(
            input
                .pixel_format()
                .expect("known fixture pixel format")
                .to_owned(),
        ),
        bit_depth: Some(input.bit_depth().expect("known fixture bit depth")),
        dynamic_range: Some(
            input
                .dynamic_range_class()
                .expect("known fixture dynamic range"),
        ),
        max_width: input.width(),
        max_height: input.height(),
        max_pixel_rate: Some(u64::try_from(pixel_rate).expect("fixture pixel rate")),
        surface: Some(DecodeSurfaceContract::for_plan(
            backend,
            pipeline,
            input,
            encoder,
            subtitle_rendering,
        )),
        status,
    }
}

fn capabilities(rows: Vec<DecodeCapability>) -> DecodeCapabilities {
    let mut rows = rows;
    rows.extend(["h264", "hevc", "mpeg4"].into_iter().map(|codec| {
        capability(
            DecodeBackend::Software,
            codec,
            None,
            None,
            CapabilityStatus::Advertised,
        )
    }));
    DecodeCapabilities::new(
        snapshot_identity(),
        rows,
        ["h264", "hevc", "mpeg4"]
            .into_iter()
            .map(|codec| SoftwareDecoder {
                codec: codec.to_owned(),
                implementation: codec.to_owned(),
            })
            .collect(),
    )
    .expect("valid capability snapshot")
}

fn capabilities_with_qualified_software(
    input: &DecodeFacts,
    pipeline: Pipeline,
    encoder: Encoder,
    subtitle_rendering: SubtitleRendering,
    mut rows: Vec<DecodeCapability>,
) -> DecodeCapabilities {
    rows.push(exact_capability(
        DecodeBackend::Software,
        input,
        pipeline,
        encoder,
        subtitle_rendering,
        CapabilityStatus::Qualified,
    ));
    capabilities(rows)
}

fn unqualified_software_capabilities(codec: &str) -> DecodeCapabilities {
    DecodeCapabilities::new(
        snapshot_identity(),
        vec![],
        vec![SoftwareDecoder {
            codec: codec.to_owned(),
            implementation: codec.to_owned(),
        }],
    )
    .expect("valid inventory without qualification")
}

fn resolve(
    encoder: Encoder,
    pipeline: Pipeline,
    facts: &DecodeFacts,
    capabilities: &DecodeCapabilities,
    policy: DecodePolicySnapshot,
) -> Result<plurx_core::transcode::ResolvedTranscode, PlanError> {
    resolve_transcode(
        &TranscodeRequest::new(encoder, options(pipeline)),
        facts,
        capabilities,
        &policy,
        &AttemptRestrictions::none(),
    )
}

#[test]
fn mpeg4_videotoolbox_exclusion_is_container_and_profile_independent() {
    let caps = capabilities(vec![]);
    for (container_only_for_case_name, profile, width, height) in [
        ("avi", "advanced simple profile", 624, 352),
        ("mkv", "simple profile", 1920, 1080),
        ("mp4", "main profile", 3840, 2160),
    ] {
        let _ = container_only_for_case_name;
        let facts = facts(video(
            0,
            Some("mpeg4"),
            Some(profile),
            width,
            height,
            Some("yuv420p"),
            "25/1",
            "25/1",
            None,
        ));
        let plan = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &facts,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("MPEG-4 has an inventoried software decoder");
        assert_eq!(plan.decode().backend(), DecodeBackend::Software);
        assert_eq!(plan.decode().software_decoder(), Some("mpeg4"));
        assert_eq!(plan.decode().reason(), DecodeReason::CompatibilityExclusion);
        assert_eq!(plan.encoder(), Encoder::VideoToolbox);
    }
}

#[test]
fn legacy_h264_preferences_match_existing_encoder_routes() {
    let caps = capabilities(vec![]);
    for (encoder, backend) in [
        (Encoder::Software, DecodeBackend::Software),
        (Encoder::VideoToolbox, DecodeBackend::VideoToolbox),
        (Encoder::Nvenc, DecodeBackend::Cuda),
        (Encoder::Qsv, DecodeBackend::Software),
        (Encoder::Vaapi, DecodeBackend::Software),
    ] {
        let input = facts(video(
            0,
            Some("h264"),
            Some("high"),
            1920,
            1080,
            Some("yuv420p"),
            "24000/1001",
            "24000/1001",
            None,
        ));
        let plan = resolve(
            encoder,
            Pipeline::Cpu,
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("legacy route resolves");
        assert_eq!(plan.decode().backend(), backend);
        assert_eq!(
            plan.decode().evidence(),
            if backend == DecodeBackend::Software {
                DecodeEvidence::LegacyUnverified
            } else {
                DecodeEvidence::LegacyUnverified
            }
        );
    }
}

#[test]
fn heavy_legacy_qsv_and_vaapi_inputs_preserve_hardware_decode() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24000/1001",
        "24000/1001",
        Some("smpte2084"),
    ));
    let caps = capabilities(vec![]);
    for (encoder, backend) in [
        (Encoder::Qsv, DecodeBackend::Qsv),
        (Encoder::Vaapi, DecodeBackend::Vaapi),
    ] {
        let plan = resolve(
            encoder,
            Pipeline::Cpu,
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("heavy legacy route resolves");
        assert_eq!(plan.decode().backend(), backend);
        assert_eq!(plan.decode().evidence(), DecodeEvidence::LegacyUnverified);
        assert_eq!(
            plan.decode().surface().decoder_download_format(),
            Some("p010le")
        );
    }
}

#[test]
fn cropped_sdr_hevc_does_not_claim_a_measured_hardware_preference() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main"),
        3840,
        1600,
        Some("yuv420p"),
        "24/1",
        "24/1",
        None,
    ));
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::Cpu,
        &input,
        &capabilities(vec![capability(
            DecodeBackend::Qsv,
            "hevc",
            None,
            None,
            CapabilityStatus::Advertised,
        )]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("legacy extraction remains neutral");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(plan.decode().reason(), DecodeReason::LegacyPreference);
}

#[test]
fn enforce_requires_the_qualified_profile_and_pixel_class() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv422p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    let supported = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    let caps = capabilities_with_qualified_software(
        &input,
        Pipeline::Cpu,
        Encoder::Qsv,
        SubtitleRendering::None,
        vec![exact_capability(
            DecodeBackend::Qsv,
            &supported,
            Pipeline::Cpu,
            Encoder::Qsv,
            SubtitleRendering::None,
            CapabilityStatus::Qualified,
        )],
    );
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::Cpu,
        &input,
        &caps,
        DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
    )
    .expect("a qualified software decoder is the conservative outcome");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(plan.decode().reason(), DecodeReason::CapabilityFallback);
}

#[test]
fn a_qualified_hardware_class_is_distinct_from_an_advertised_one() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    for (status, expected) in [
        (CapabilityStatus::Advertised, DecodeBackend::Software),
        (CapabilityStatus::Qualified, DecodeBackend::Qsv),
        (CapabilityStatus::Rejected, DecodeBackend::Software),
    ] {
        let caps = capabilities_with_qualified_software(
            &input,
            Pipeline::Cpu,
            Encoder::Qsv,
            SubtitleRendering::None,
            vec![exact_capability(
                DecodeBackend::Qsv,
                &input,
                Pipeline::Cpu,
                Encoder::Qsv,
                SubtitleRendering::None,
                status,
            )],
        );
        let plan = resolve(
            Encoder::Qsv,
            Pipeline::Cpu,
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        )
        .expect("software fallback remains available");
        assert_eq!(plan.decode().backend(), expected);
        if status == CapabilityStatus::Qualified {
            assert_eq!(plan.decode().reason(), DecodeReason::LegacyPreference);
        }
    }
}

#[test]
fn software_inventory_is_not_silently_promoted_to_qualification() {
    let input = facts(video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    let caps = unqualified_software_capabilities("h264");
    let legacy = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &caps,
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("legacy mode records inherited evidence");
    assert_eq!(legacy.decode().evidence(), DecodeEvidence::LegacyUnverified);
    assert_eq!(
        resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        ),
        Err(PlanError::CapabilityUnavailable(DecodeBackend::Software))
    );
}

#[test]
fn operator_software_requirement_replaces_a_vendor_surface_graph() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::VppQsv,
        &input,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some("off")),
    )
    .expect("CPU renderer preserves the SDR contract");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(
        plan.decode().reason(),
        DecodeReason::OperatorSoftwareOverride
    );
    assert_eq!(plan.options().pipeline, Pipeline::Cpu);
    assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
}

#[test]
fn dolby_rendering_requires_software_decode_and_retains_side_data() {
    let mut stream = video(
        2,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    );
    stream["side_data_list"] = json!([{"side_data_type": "DOVI configuration record"}]);
    let input = facts(stream);
    let plan = resolve(
        Encoder::VideoToolbox,
        Pipeline::DoviTonemapx,
        &input,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("Dolby software route resolves");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(plan.decode().reason(), DecodeReason::RendererRequirement);
    assert!(plan.decode().surface().requires_side_data());
    assert_eq!(plan.encoder(), Encoder::VideoToolbox);
}

#[test]
fn attached_artwork_and_multiple_video_streams_keep_absolute_attribution() {
    let json = json!({"streams": [
        {
            "index": 0,
            "codec_type": "video",
            "codec_name": "mjpeg",
            "disposition": {"attached_pic": 1}
        },
        video(3, Some("h264"), Some("high"), 1920, 1080, Some("yuv420p"), "24/1", "24/1", None),
        video(7, Some("hevc"), Some("main 10"), 3840, 2160, Some("yuv420p10le"), "24/1", "24/1", Some("smpte2084"))
    ]});
    let first = DecodeFacts::from_ffprobe_json(&json, identity('b')).expect("first video");
    assert_eq!(first.input_video_stream(), 3);
    assert_eq!(
        first.selection_provenance(),
        StreamSelectionProvenance::FirstPlayableVideo
    );
    let explicit = DecodeFacts::from_ffprobe_json_at(&json, identity('b'), 7)
        .expect("explicit selected video");
    assert_eq!(explicit.input_video_stream(), 7);
    assert_eq!(explicit.codec(), Some("hevc"));
    assert_eq!(
        explicit.selection_provenance(),
        StreamSelectionProvenance::ExplicitAbsoluteIndex
    );
}

#[test]
fn invalid_and_variable_rates_remain_unknown_instead_of_becoming_24fps() {
    let variable = facts(video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "24000/1001",
        "60/1",
        None,
    ));
    assert_eq!(variable.frame_rate().value(), None);
    assert_eq!(
        variable.frame_rate().provenance(),
        FrameRateProvenance::Variable
    );
    let invalid = facts(video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "0/0",
        "0/0",
        None,
    ));
    assert_eq!(invalid.frame_rate().value(), None);
    assert_eq!(
        invalid.frame_rate().provenance(),
        FrameRateProvenance::Unknown
    );
}

#[test]
fn continuation_restriction_cannot_be_bypassed_by_a_new_hardware_preference() {
    let input = facts(video(
        0,
        Some("h264"),
        Some("high"),
        3840,
        2160,
        Some("yuv420p"),
        "60/1",
        "60/1",
        Some("bt709"),
    ));
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Nvenc, options(Pipeline::Cpu)),
        &input,
        &capabilities_with_qualified_software(
            &input,
            Pipeline::Cpu,
            Encoder::Nvenc,
            SubtitleRendering::None,
            vec![],
        ),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        &AttemptRestrictions::requiring(DecodeBackend::Software),
    )
    .expect("the software continuation remains representable");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(
        plan.decode().reason(),
        DecodeReason::ContinuationRestriction
    );
    assert_eq!(plan.encoder(), Encoder::Nvenc);
}

#[test]
fn source_identity_and_selected_stream_are_bound_into_the_facts_digest() {
    let json = json!({"streams": [
        video(2, Some("h264"), Some("high"), 1920, 1080, Some("yuv420p"), "24/1", "24/1", None),
        video(5, Some("h264"), Some("high"), 1920, 1080, Some("yuv420p"), "24/1", "24/1", None)
    ]});
    let first = DecodeFacts::from_ffprobe_json_at(&json, identity('c'), 2).expect("first");
    let other_stream = DecodeFacts::from_ffprobe_json_at(&json, identity('c'), 5).expect("other");
    let other_source =
        DecodeFacts::from_ffprobe_json_at(&json, identity('d'), 2).expect("changed source");
    assert_ne!(first.facts_digest(), other_stream.facts_digest());
    assert_ne!(first.facts_digest(), other_source.facts_digest());
}

#[test]
fn missing_codec_or_uninventoried_software_decoder_is_a_typed_refusal() {
    let unknown = facts(video(
        0,
        None,
        None,
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        None,
    ));
    assert_eq!(
        resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &unknown,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        ),
        Err(PlanError::MissingCodec)
    );
    let av1 = facts(video(
        0,
        Some("av1"),
        Some("main"),
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        None,
    ));
    assert_eq!(
        resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &av1,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        ),
        Err(PlanError::SoftwareDecoderUnavailable)
    );
}

#[test]
fn policy_snapshot_is_immutable_and_preserves_exact_legacy_false_spellings() {
    for spelling in ["off", "0", "false", "no"] {
        let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some(spelling));
        assert!(policy.force_software_decode(), "{spelling}");
    }
    for automatic in ["OFF", " False ", "auto", "on", "1"] {
        let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some(automatic));
        assert!(
            !policy.force_software_decode(),
            "legacy parsing must not broaden for {automatic:?}"
        );
    }
    let automatic = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some("auto"));
    assert!(!automatic.force_software_decode());
    assert_eq!(automatic.compatibility_value(), Some("auto"));
}

#[test]
fn rational_values_are_reduced_and_positive_geometry_is_conservative() {
    let input = facts(video(
        0,
        Some("h264"),
        Some("high"),
        -1,
        0,
        Some("yuv420p"),
        "48000/2002",
        "48000/2002",
        None,
    ));
    assert_eq!(input.width(), None);
    assert_eq!(input.height(), None);
    let rate = input.frame_rate().value().expect("valid reduced rate");
    assert_eq!((rate.numerator(), rate.denominator()), (24_000, 1_001));
}

#[test]
fn pixel_layout_parsing_never_downgrades_unrecognized_high_bit_depth() {
    for (pixel_format, expected_depth, expected_chroma) in [
        ("yuv420p16le", Some(16), Some("420")),
        ("yuv444p14be", Some(14), Some("444")),
        ("yuv420p9le", Some(9), Some("420")),
        ("p010le", Some(10), Some("420")),
        ("p012be", Some(12), Some("420")),
        ("nv12", Some(8), Some("420")),
    ] {
        let input = facts(video(
            0,
            Some("hevc"),
            Some("main"),
            1920,
            1080,
            Some(pixel_format),
            "24/1",
            "24/1",
            None,
        ));
        assert_eq!(input.bit_depth(), expected_depth, "{pixel_format}");
        assert_eq!(input.chroma_sampling(), expected_chroma, "{pixel_format}");
    }
}

#[test]
fn contradictory_capability_rows_are_rejected() {
    let row = capability(
        DecodeBackend::Qsv,
        "hevc",
        None,
        None,
        CapabilityStatus::Advertised,
    );
    assert!(matches!(
        DecodeCapabilities::new(snapshot_identity(), vec![row.clone(), row], vec![]),
        Err(PlanError::DuplicateCapability)
    ));
}

#[test]
fn overlapping_equal_specificity_capabilities_are_rejected() {
    assert!(matches!(
        DecodeCapabilities::new(
            snapshot_identity(),
            vec![
                capability(
                    DecodeBackend::Qsv,
                    "hevc",
                    Some("main 10"),
                    None,
                    CapabilityStatus::Advertised,
                ),
                capability(
                    DecodeBackend::Qsv,
                    "hevc",
                    None,
                    Some("yuv420p10le"),
                    CapabilityStatus::Advertised,
                ),
            ],
            vec![],
        ),
        Err(PlanError::AmbiguousCapability)
    ));
}

#[test]
fn continuation_can_require_the_already_compatible_hardware_backend() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Qsv, options(Pipeline::VppQsv)),
        &input,
        &capabilities(vec![exact_capability(
            DecodeBackend::Qsv,
            &input,
            Pipeline::VppQsv,
            Encoder::Qsv,
            SubtitleRendering::None,
            CapabilityStatus::Qualified,
        )]),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        &AttemptRestrictions::requiring(DecodeBackend::Qsv),
    )
    .expect("the already-required QSV graph remains valid");
    assert_eq!(plan.decode().backend(), DecodeBackend::Qsv);
    assert_eq!(
        plan.decode().reason(),
        DecodeReason::ContinuationRestriction
    );
}

#[test]
fn decoder_renderer_surface_mismatch_is_a_typed_refusal() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    assert_eq!(
        resolve_transcode(
            &TranscodeRequest::new(Encoder::Qsv, options(Pipeline::VppQsv)),
            &input,
            &capabilities(vec![capability(
                DecodeBackend::Cuda,
                "hevc",
                None,
                None,
                CapabilityStatus::Advertised,
            )]),
            &DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
            &AttemptRestrictions::requiring(DecodeBackend::Cuda),
        ),
        Err(PlanError::IncompatibleRenderer)
    );
}

#[test]
fn presentation_contract_records_color_tracks_subtitle_and_av_correction() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    let mut media = options(Pipeline::Hdr10Passthrough);
    media.audio_index = Some(2);
    media.audio_offset_ms = -250;
    media.subtitle_burn = Some(plurx_core::transcode::SubtitleBurn {
        subtitle_index: 3,
        bitmap: true,
    });
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Qsv, media),
        &input,
        &capabilities(vec![exact_capability(
            DecodeBackend::Qsv,
            &input,
            Pipeline::Hdr10Passthrough,
            Encoder::Qsv,
            SubtitleRendering::BitmapBurn,
            CapabilityStatus::Qualified,
        )]),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        &AttemptRestrictions::none(),
    )
    .expect("complete HDR presentation contract");
    let contract = plan.output_contract();
    assert_eq!(contract.output_codec(), "hevc");
    assert_eq!(contract.output_encoder(), "hevc_qsv");
    assert_eq!(contract.output_profile(), Some("main10"));
    assert_eq!(contract.output_pixel_format(), "yuv420p10le");
    assert_eq!(contract.output_dynamic_range(), "hdr10");
    assert_eq!(contract.output_transfer(), "smpte2084");
    assert_eq!(contract.output_matrix(), "bt2020");
    assert_eq!(contract.output_primaries(), "bt2020");
    assert_eq!(contract.audio_offset_ms(), -250);
    assert_eq!(
        contract.subtitle_rendering(),
        plurx_core::transcode::SubtitleRendering::BitmapBurn
    );
    assert_eq!(
        plan.decode().surface().decoder_download_format(),
        Some("p010le")
    );
    assert_eq!(
        plan.decode().surface().encoder_upload_domain(),
        Some(FrameDomain::Qsv)
    );
    assert_eq!(
        plan.decode().surface().encoder_upload_format(),
        Some("p010le")
    );
}

#[test]
fn vendor_graphs_expose_their_hardware_surface_family() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    for (pipeline, encoder, backend, domain) in [
        (
            Pipeline::VppQsv,
            Encoder::Qsv,
            DecodeBackend::Qsv,
            FrameDomain::Qsv,
        ),
        (
            Pipeline::TonemapVaapi,
            Encoder::Vaapi,
            DecodeBackend::Vaapi,
            FrameDomain::Vaapi,
        ),
    ] {
        let plan = resolve(
            encoder,
            pipeline,
            &input,
            &capabilities(vec![exact_capability(
                backend,
                &input,
                pipeline,
                encoder,
                SubtitleRendering::None,
                CapabilityStatus::Qualified,
            )]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        )
        .expect("qualified vendor graph");
        assert_eq!(plan.decode().surface().decode_domain(), domain);
        assert_eq!(plan.decode().surface().decoder_download_format(), None);
    }
}

#[test]
fn unknown_layout_cannot_match_a_qualified_envelope() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main"),
        3840,
        2160,
        Some("mystery-layout"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    let row = DecodeCapability {
        backend: DecodeBackend::Qsv,
        codec: "hevc".to_owned(),
        profile: Some("main".to_owned()),
        pixel_format: Some("mystery-layout".to_owned()),
        bit_depth: Some(8),
        dynamic_range: input.dynamic_range_class(),
        max_width: Some(3840),
        max_height: Some(2160),
        max_pixel_rate: Some(3840 * 2160 * 24),
        surface: Some(DecodeSurfaceContract::for_plan(
            DecodeBackend::Qsv,
            Pipeline::Cpu,
            &input,
            Encoder::Qsv,
            SubtitleRendering::None,
        )),
        status: CapabilityStatus::Qualified,
    };
    assert_eq!(
        resolve(
            Encoder::Qsv,
            Pipeline::Cpu,
            &input,
            &capabilities(vec![row]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        ),
        Err(PlanError::CapabilityUnavailable(DecodeBackend::Software))
    );
}

#[test]
fn qualified_envelopes_do_not_cover_larger_geometry_or_a_different_surface() {
    let small = facts(video(
        0,
        Some("hevc"),
        Some("main"),
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    let large = facts(video(
        0,
        Some("hevc"),
        Some("main"),
        3840,
        2160,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    for qualified in [
        exact_capability(
            DecodeBackend::Qsv,
            &small,
            Pipeline::Cpu,
            Encoder::Qsv,
            SubtitleRendering::None,
            CapabilityStatus::Qualified,
        ),
        exact_capability(
            DecodeBackend::Qsv,
            &large,
            Pipeline::VppQsv,
            Encoder::Qsv,
            SubtitleRendering::None,
            CapabilityStatus::Qualified,
        ),
    ] {
        let plan = resolve(
            Encoder::Qsv,
            Pipeline::Cpu,
            &large,
            &capabilities_with_qualified_software(
                &large,
                Pipeline::Cpu,
                Encoder::Qsv,
                SubtitleRendering::None,
                vec![qualified],
            ),
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        )
        .expect("precisely qualified software fallback");
        assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    }
}

#[test]
fn capability_snapshot_identity_rejects_unbound_builds_and_drivers() {
    assert!(matches!(
        DecodeCapabilitySnapshotIdentity::new(
            "not-a-digest".to_owned(),
            "qsv-gen12".to_owned(),
            None,
        ),
        Err(PlanError::InvalidCapabilityIdentity("ffmpeg build digest"))
    ));
    assert!(matches!(
        DecodeCapabilitySnapshotIdentity::new(
            "a".repeat(64),
            "qsv-gen12".to_owned(),
            Some("bad-driver".to_owned()),
        ),
        Err(PlanError::InvalidCapabilityIdentity(
            "driver environment digest"
        ))
    ));
}

#[test]
fn catalog_hdr_and_dolby_facts_are_merged_and_contradictions_refused() {
    let stream = video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        None,
    );
    let dolby = DolbyVisionFacts {
        profile: Some(8),
        level: Some(6),
        bl_compat_id: Some(1),
        el_present: Some(false),
        rpu_present: Some(true),
    };
    let catalog = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8"),
        dolby,
    )
    .expect("typed catalog metadata");
    let merged = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [stream.clone()]}),
        identity('e'),
        &catalog,
    )
    .expect("catalog fills selective-probe omissions");
    assert_eq!(merged.dynamic_range(), Some("dolby_vision"));
    assert_eq!(merged.dolby_vision(), dolby);

    let hdr10 = DecodeCatalogMetadata::new(Some("hdr10"), Some("HDR10"), Default::default())
        .expect("HDR10 catalog");
    let mut hlg_stream = stream;
    hlg_stream["color_transfer"] = json!("arib-std-b67");
    assert_eq!(
        DecodeFacts::from_ffprobe_json_with_catalog(
            &json!({"streams": [hlg_stream]}),
            identity('f'),
            &hdr10,
        ),
        Err(PlanError::ConflictingMetadata("dynamic range"))
    );
}

#[test]
fn presentation_geometry_records_the_no_upscale_even_output() {
    let input = facts(video(
        0,
        Some("h264"),
        Some("high"),
        1280,
        720,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    let plan = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("legacy presentation");
    assert_eq!(plan.output_contract().requested_max_height(), 1080);
    assert_eq!(plan.output_contract().effective_width(), Some(1280));
    assert_eq!(plan.output_contract().effective_height(), Some(720));
}

#[test]
fn surface_contracts_name_vendor_subtitle_vulkan_and_opencl_transitions() {
    let input = facts(video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    for (pipeline, encoder, backend, native) in [
        (
            Pipeline::VppQsv,
            Encoder::Qsv,
            DecodeBackend::Qsv,
            FrameDomain::Qsv,
        ),
        (
            Pipeline::TonemapVaapi,
            Encoder::Vaapi,
            DecodeBackend::Vaapi,
            FrameDomain::Vaapi,
        ),
    ] {
        let surface = DecodeSurfaceContract::for_plan(
            backend,
            pipeline,
            &input,
            encoder,
            SubtitleRendering::BitmapBurn,
        );
        assert_eq!(surface.decode_domain(), native);
        assert_eq!(surface.renderer_download_format(), Some("nv12"));
        assert_eq!(surface.encoder_upload_domain(), Some(native));
        assert_eq!(surface.encoder_upload_format(), Some("nv12"));
    }

    let vulkan = DecodeSurfaceContract::for_plan(
        DecodeBackend::Cuda,
        Pipeline::Libplacebo,
        &input,
        Encoder::Nvenc,
        SubtitleRendering::None,
    );
    assert_eq!(vulkan.renderer_domain(), FrameDomain::Vulkan);
    assert_eq!(vulkan.renderer_upload_format(), Some("yuv420p10le"));
    assert_eq!(vulkan.renderer_download_format(), Some("nv12"));

    let opencl = DecodeSurfaceContract::for_plan(
        DecodeBackend::Cuda,
        Pipeline::TonemapOpencl,
        &input,
        Encoder::Nvenc,
        SubtitleRendering::None,
    );
    assert_eq!(opencl.renderer_domain(), FrameDomain::OpenCl);
    assert_eq!(opencl.renderer_upload_format(), Some("p010le"));
    assert_eq!(opencl.renderer_download_format(), Some("nv12"));
}
