use plurx_core::transcode::{
    resolve_transcode, AttemptRestrictions, CapabilityStatus, DecodeBackend, DecodeCapabilities,
    DecodeCapability, DecodeEvidence, DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot,
    DecodeReason, DecodeSourceIdentity, EffectiveRateControl, Encoder, FrameDomain,
    FrameRateProvenance, OutputGrade, Pipeline, PlanError, SoftwareDecoder,
    StreamSelectionProvenance, ToneMap, TranscodeMediaOptions, TranscodeRequest,
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
            CapabilityStatus::Qualified,
        )
    }));
    DecodeCapabilities::new(
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

fn unqualified_software_capabilities(codec: &str) -> DecodeCapabilities {
    DecodeCapabilities::new(
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
                DecodeEvidence::Qualified
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
        assert_eq!(plan.decode().surface().download_format(), Some("p010le"));
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
            CapabilityStatus::Qualified,
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
    let caps = capabilities(vec![capability(
        DecodeBackend::Qsv,
        "hevc",
        Some("main 10"),
        Some("yuv420p10le"),
        CapabilityStatus::Qualified,
    )]);
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
        let caps = capabilities(vec![capability(
            DecodeBackend::Qsv,
            "hevc",
            Some("main 10"),
            Some("yuv420p10le"),
            status,
        )]);
        let plan = resolve(
            Encoder::Qsv,
            Pipeline::Cpu,
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        )
        .expect("software fallback remains available");
        assert_eq!(plan.decode().backend(), expected);
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
        None,
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
        None,
    ));
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Nvenc, options(Pipeline::Cpu)),
        &input,
        &capabilities(vec![capability(
            DecodeBackend::Cuda,
            "h264",
            None,
            None,
            CapabilityStatus::Qualified,
        )]),
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
fn policy_snapshot_is_immutable_and_preserves_all_legacy_false_spellings() {
    for spelling in ["off", "0", "false", "no", "OFF", " False "] {
        let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some(spelling));
        assert!(policy.force_software_decode(), "{spelling}");
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
fn contradictory_capability_rows_are_rejected() {
    let row = capability(
        DecodeBackend::Qsv,
        "hevc",
        None,
        None,
        CapabilityStatus::Qualified,
    );
    assert!(matches!(
        DecodeCapabilities::new(vec![row.clone(), row], vec![]),
        Err(PlanError::DuplicateCapability)
    ));
}

#[test]
fn overlapping_equal_specificity_capabilities_are_rejected() {
    assert!(matches!(
        DecodeCapabilities::new(
            vec![
                capability(
                    DecodeBackend::Qsv,
                    "hevc",
                    Some("main 10"),
                    None,
                    CapabilityStatus::Qualified,
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
        &capabilities(vec![capability(
            DecodeBackend::Qsv,
            "hevc",
            None,
            None,
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
                CapabilityStatus::Qualified,
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
        &capabilities(vec![capability(
            DecodeBackend::Qsv,
            "hevc",
            None,
            None,
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
    assert_eq!(plan.decode().surface().download_format(), Some("p010le"));
    assert_eq!(
        plan.decode().surface().upload_domain(),
        Some(FrameDomain::Qsv)
    );
    assert_eq!(plan.decode().surface().upload_format(), Some("p010le"));
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
            &capabilities(vec![capability(
                backend,
                "hevc",
                None,
                None,
                CapabilityStatus::Qualified,
            )]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
        )
        .expect("qualified vendor graph");
        assert_eq!(plan.decode().surface().frame_domain(), domain);
        assert_eq!(plan.decode().surface().download_format(), None);
    }
}
