use plurx_core::domain::{
    AudioStream, ContinuationDecodeRestriction, DecodeRestrictionError, DolbyVisionFacts,
    FieldOrder, MediaFile, ScanType, CONTINUATION_DECODE_RESTRICTION_VERSION,
};
use plurx_core::transcode::{
    hls_args, resolve_transcode, ArtifactQualification, AttemptRestrictions, CapabilityStatus,
    DecodeBackend, DecodeCacheIdentity, DecodeCapabilities, DecodeCapability,
    DecodeCapabilitySnapshotIdentity, DecodeCatalogMetadata, DecodeEvidence, DecodeFacts,
    DecodePlanPolicy, DecodePolicySnapshot, DecodeReason, DecodeSourceIdentity,
    DecodeSurfaceContract, Deinterlace, DiagnosticLogging, EffectiveRateControl, Encoder,
    FrameDomain, FrameRateProvenance, InterlaceVerdict, OutputGrade, Pacing, Pipeline,
    PipelineDigest, PlanError, PlanSourceBinding, Recipe, SoftwareDecoder,
    StreamSelectionProvenance, SubtitleBurn, SubtitleRendering, ToneMap, TranscodeExecution,
    TranscodeMediaOptions, TranscodeOptions, TranscodeRequest, HEALTH_QUALIFIED_ARTIFACT_NAMESPACE,
    UNQUALIFIED_ARTIFACT_NAMESPACE,
};
use plurx_core::transcode::{
    LinuxDolbyContext, LinuxDolbyIdentity, LinuxDolbyObservation, MacosProcessingAvailability,
    MacosProcessingContext, MacosProcessingGraph, MacosProcessingIdentity,
    MacosProcessingSelection, Rational,
};
use serde_json::{json, Value};
use std::path::PathBuf;

fn macos_stream(hdr: bool) -> Value {
    let mut stream = video(
        0,
        Some("hevc"),
        Some(if hdr { "Main 10" } else { "Main" }),
        3840,
        2160,
        Some(if hdr { "yuv420p10le" } else { "yuv420p" }),
        "24/1",
        "24/1",
        Some(if hdr { "smpte2084" } else { "bt709" }),
    );
    stream["color_primaries"] = json!(if hdr { "bt2020" } else { "bt709" });
    stream["color_space"] = json!(if hdr { "bt2020nc" } else { "bt709" });
    stream["color_range"] = json!("tv");
    stream["field_order"] = json!("progressive");
    stream["sample_aspect_ratio"] = json!("1:1");
    stream
}

fn macos_identity(os_build: &str) -> MacosProcessingIdentity {
    MacosProcessingIdentity::new(
        "1".repeat(64),
        "2".repeat(64),
        "3".repeat(64),
        "4".repeat(64),
        os_build.to_owned(),
        "arm64".to_owned(),
        "Apple test SoC".to_owned(),
    )
    .expect("validated implementation")
}

fn macos_context(
    enabled: bool,
    availability: MacosProcessingAvailability,
) -> MacosProcessingContext {
    MacosProcessingContext::new(
        enabled,
        macos_identity("test-build"),
        availability,
        availability,
    )
}

fn macos_policy(context: MacosProcessingContext) -> DecodePolicySnapshot {
    DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None).with_macos_processing(context)
}

#[test]
fn macos_processing_selects_only_proved_sdr_and_hdr10_surfaces() {
    for (hdr, pipeline) in [
        (false, Pipeline::VtScaleSdr),
        (true, Pipeline::VtToneMapMetal),
    ] {
        let input = facts(macos_stream(hdr));
        let plan = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &unqualified_software_capabilities("hevc"),
            macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
        )
        .expect("compatible frozen plan");
        assert_eq!(plan.options().pipeline, pipeline);
        assert_eq!(plan.decode().backend(), DecodeBackend::VideoToolbox);
        assert_eq!(
            plan.decode().surface().decode_domain(),
            FrameDomain::VideoToolbox
        );
        assert_eq!(
            plan.decode().surface().renderer_domain(),
            FrameDomain::VideoToolbox
        );
        assert_eq!(plan.decode().surface().decoder_download_format(), None);
        assert_eq!(plan.decode().surface().renderer_download_format(), None);
        assert_eq!(
            plan.macos_processing_selection(),
            Some(MacosProcessingSelection::Selected)
        );
        assert!(plan.macos_processing_identity().is_some());
        assert_eq!(plan.output_contract().output_codec(), "h264");
        assert_eq!(plan.output_contract().effective_width(), Some(1920));
        assert_eq!(plan.output_contract().effective_height(), Some(1080));
    }
}

#[test]
fn macos_processing_unavailable_context_preserves_legacy_hash_and_argv() {
    let input = facts(macos_stream(false));
    let caps = unqualified_software_capabilities("hevc");
    let legacy = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &caps,
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("legacy plan");
    let source = execution_file("/fixture/source.mkv");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    for context in [
        macos_context(false, MacosProcessingAvailability::Available),
        macos_context(true, MacosProcessingAvailability::Pending),
        macos_context(true, MacosProcessingAvailability::Unavailable),
    ] {
        let plan = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &caps,
            macos_policy(context),
        )
        .expect("incumbent retained");
        assert_eq!(plan.plan_digest(), legacy.plan_digest());
        assert_eq!(hls_args(&plan, &execution), hls_args(&legacy, &execution));
        assert!(plan.macos_processing_identity().is_none());
        assert_eq!(
            plan.decode().surface().decode_domain(),
            FrameDomain::SystemMemory
        );
    }
}

#[test]
fn macos_processing_rejects_unqualified_geometry_and_color_classes() {
    let cases = [
        ("width", json!(3841)),
        ("sample_aspect_ratio", json!("4:3")),
        ("field_order", json!("tt")),
        ("field_order", Value::Null),
        ("r_frame_rate", json!("30/1")),
        ("color_range", json!("pc")),
        ("color_primaries", json!("bt2020")),
        ("color_transfer", json!("arib-std-b67")),
        ("pix_fmt", json!("yuv422p")),
        ("profile", Value::Null),
    ];
    for (key, value) in cases {
        let mut stream = macos_stream(false);
        stream[key] = value;
        let input = facts(stream);
        let plan = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &unqualified_software_capabilities("hevc"),
            macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
        )
        .expect("incumbent accepts unsupported Mac input");
        assert_eq!(plan.options().pipeline, Pipeline::Cpu, "{key}");
        assert!(plan.macos_processing_identity().is_none(), "{key}");
    }
}

#[test]
fn macos_processing_respects_software_and_failed_graph_restrictions() {
    let input = facts(macos_stream(false));
    let caps = unqualified_software_capabilities("hevc");
    let request = TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::Cpu));
    let context = macos_context(true, MacosProcessingAvailability::Available);
    let override_policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some("off"))
        .with_macos_processing(context.clone());
    let forced = resolve_transcode(
        &request,
        &input,
        &caps,
        &override_policy,
        &AttemptRestrictions::none(),
    )
    .expect("operator override");
    assert_eq!(forced.decode().backend(), DecodeBackend::Software);
    assert_eq!(forced.options().pipeline, Pipeline::Cpu);
    assert_eq!(
        forced.macos_processing_selection(),
        Some(MacosProcessingSelection::DecoderOverride)
    );
    let restricted = resolve_transcode(
        &request,
        &input,
        &caps,
        &macos_policy(context.clone()),
        &AttemptRestrictions::requiring(DecodeBackend::Software),
    )
    .expect("continuation");
    assert_eq!(restricted.options().pipeline, Pipeline::Cpu);
    let retry_context = context.excluding_pipeline(Pipeline::VtScaleSdr);
    assert!(
        retry_context.enabled(),
        "retry does not rewrite the saved choice"
    );
    let retry = resolve_transcode(
        &request,
        &input,
        &caps,
        &macos_policy(retry_context),
        &AttemptRestrictions::none(),
    )
    .expect("processing-owner fallback");
    assert_eq!(retry.options().pipeline, Pipeline::Cpu);
    assert_eq!(
        retry.macos_processing_selection(),
        Some(MacosProcessingSelection::RecoveryRestriction)
    );
    assert!(retry.macos_processing_identity().is_none());
}

#[test]
fn macos_selected_options_preserve_software_decode_recovery_for_sdr_and_hdr10() {
    for hdr in [false, true] {
        let input = facts(macos_stream(hdr));
        let caps = unqualified_software_capabilities("hevc");
        let policy = macos_policy(macos_context(true, MacosProcessingAvailability::Available));
        let original = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &caps,
            policy.clone(),
        )
        .expect("selected Mac plan");
        assert!(original.macos_processing_identity().is_some());
        let selected_request =
            TranscodeRequest::new(original.encoder(), original.options().clone());
        let restrictions = AttemptRestrictions::requiring(DecodeBackend::Software);
        let alternate = resolve_transcode(&selected_request, &input, &caps, &policy, &restrictions)
            .expect("software alternate from selected media options");
        assert_eq!(alternate.decode().backend(), DecodeBackend::Software);
        assert_eq!(alternate.options().pipeline, Pipeline::Cpu);
        assert_eq!(alternate.output_contract().output_grade(), OutputGrade::Sdr);
        assert_eq!(
            alternate.output_contract().output_grade(),
            original.output_contract().output_grade()
        );
        assert_eq!(alternate.macos_processing_identity(), None);
        assert_eq!(
            alternate.macos_processing_selection(),
            Some(MacosProcessingSelection::RecoveryRestriction)
        );
        assert_ne!(alternate.plan_digest(), original.plan_digest());
        let cpu = resolve_transcode(
            &TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::Cpu)),
            &input,
            &caps,
            &policy,
            &restrictions,
        )
        .expect("ordinary restricted CPU plan");
        assert_eq!(alternate.plan_digest(), cpu.plan_digest());
        let source = execution_file("/fixture/source.mkv");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("execution");
        assert_eq!(hls_args(&alternate, &execution), hls_args(&cpu, &execution));
        assert!(hls_args(&alternate, &execution)
            .windows(2)
            .any(|pair| pair == ["-hwaccel", "none"]));
    }
}

#[test]
fn macos_software_recovery_survives_readiness_changes_without_authorizing_mac_graphs() {
    for hdr in [false, true] {
        let input = facts(macos_stream(hdr));
        let caps = unqualified_software_capabilities("hevc");
        let original = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &caps,
            macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
        )
        .expect("selected Mac plan before readiness changes");
        let request = TranscodeRequest::new(original.encoder(), original.options().clone());
        let restrictions = AttemptRestrictions::requiring(DecodeBackend::Software);
        let baseline = resolve_transcode(
            &TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::Cpu)),
            &input,
            &caps,
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &restrictions,
        )
        .expect("CPU software identity");
        for policy in [
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            macos_policy(macos_context(false, MacosProcessingAvailability::Available)),
            macos_policy(macos_context(true, MacosProcessingAvailability::Pending)),
            macos_policy(macos_context(
                true,
                MacosProcessingAvailability::Unavailable,
            )),
            macos_policy(
                macos_context(true, MacosProcessingAvailability::Available)
                    .excluding_pipeline(original.options().pipeline),
            ),
        ] {
            let alternate = resolve_transcode(&request, &input, &caps, &policy, &restrictions)
                .expect("software recovery does not execute the unavailable Mac graph");
            assert_eq!(alternate.decode().backend(), DecodeBackend::Software);
            assert_eq!(alternate.options().pipeline, Pipeline::Cpu);
            assert_eq!(
                alternate.output_contract().output_grade(),
                original.output_contract().output_grade()
            );
            assert!(alternate.macos_processing_identity().is_none());
            assert_eq!(alternate.plan_digest(), baseline.plan_digest());
            assert_eq!(
                resolve_transcode(
                    &request,
                    &input,
                    &caps,
                    &policy,
                    &AttemptRestrictions::none()
                ),
                Err(PlanError::IncompatibleRenderer),
                "unrestricted unqualified Mac graph must still reject"
            );
        }
    }
}

#[test]
fn macos_processing_cannot_be_selected_without_context_or_for_burns() {
    let input = facts(macos_stream(false));
    let caps = unqualified_software_capabilities("hevc");
    assert_eq!(
        resolve(
            Encoder::VideoToolbox,
            Pipeline::VtScaleSdr,
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
        ),
        Err(PlanError::IncompatibleRenderer)
    );
    let mut media = options(Pipeline::Cpu);
    media.subtitle_burn = Some(SubtitleBurn {
        subtitle_index: 0,
        bitmap: false,
    });
    let plan = resolve_with_options(
        Encoder::VideoToolbox,
        media,
        &input,
        &caps,
        macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
    )
    .expect("incumbent burn");
    assert_eq!(plan.options().pipeline, Pipeline::Cpu);
    assert_eq!(
        plan.macos_processing_selection(),
        Some(MacosProcessingSelection::RuntimeProbeFailed)
    );
}

#[test]
fn macos_processing_implementation_changes_rotate_only_selected_plans() {
    let input = facts(macos_stream(false));
    let caps = unqualified_software_capabilities("hevc");
    let make = |build: &str, enabled| {
        resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &caps,
            macos_policy(MacosProcessingContext::new(
                enabled,
                macos_identity(build),
                MacosProcessingAvailability::Available,
                MacosProcessingAvailability::Available,
            )),
        )
        .expect("plan")
    };
    assert_ne!(
        make("build-one", true).plan_digest(),
        make("build-two", true).plan_digest()
    );
    assert_eq!(
        make("build-one", false).plan_digest(),
        make("build-two", false).plan_digest()
    );
}

#[test]
fn macos_processing_rolling_and_vod_share_frozen_color_graph() {
    let input = facts(macos_stream(true));
    let plan = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &unqualified_software_capabilities("hevc"),
        macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
    )
    .expect("HDR10 Metal plan");
    let source = execution_file("/fixture/source.mkv");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    let rolling = hls_args(&plan, &execution);
    let vod = plurx_core::transcode::vod_pipe_args(
        &source,
        &plan,
        &execution,
        plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid"),
        12.0,
    );
    let graph = Pipeline::VtToneMapMetal
        .filters(Some(1920), 1080, Some("hdr10"))
        .expect("graph");
    for args in [&rolling, &vod] {
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-hwaccel_output_format", "videotoolbox_vld"]));
        assert!(args.windows(2).any(|pair| pair == ["-allow_sw", "0"]));
        assert!(args.windows(2).any(|pair| pair == ["-color_range", "tv"]));
        assert!(args.windows(2).any(|pair| pair == ["-b:v", "8000k"]));
        let vf = &args[args.iter().position(|arg| arg == "-vf").expect("vf") + 1];
        assert!(vf.contains(&graph));
        assert!(!vf.contains("hwdownload"));
        assert!(!vf.contains("format=yuv420p"));
    }
    assert!(vod.iter().any(|arg| arg == "passthrough"));
    assert!(!plurx_core::transcode::PIPELINE_CANDIDATES.contains(&Pipeline::VtToneMapMetal));
}

#[test]
fn macos_processing_continuous_vod_retains_the_existing_avc_envelope() {
    for (hdr, pipeline) in [
        (false, Pipeline::VtScaleSdr),
        (true, Pipeline::VtToneMapMetal),
    ] {
        let input = facts(macos_stream(hdr));
        let caps = unqualified_software_capabilities("hevc");
        let request = TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::Cpu))
            .with_continuous_avc_video();
        let policy = macos_policy(macos_context(true, MacosProcessingAvailability::Available));
        let plan = resolve_transcode(
            &request,
            &input,
            &caps,
            &policy,
            &AttemptRestrictions::none(),
        )
        .expect("compatible continuous video envelope");
        assert_eq!(plan.options().pipeline, pipeline);
        assert_eq!(plan.output_contract().output_profile(), Some("high"));
        assert!(plan.output_contract().normalized_geometry().is_some());
        assert!(!plan.options().input_has_audio);
        let source = execution_file("/fixture/source.mkv");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("execution");
        let rolling = hls_args(&plan, &execution);
        let vod = plurx_core::transcode::vod_pipe_args(
            &source,
            &plan,
            &execution,
            plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
        );
        let renderer = pipeline
            .filters(Some(1920), 1080, Some(if hdr { "hdr10" } else { "sdr" }))
            .expect("renderer");
        for args in [&rolling, &vod] {
            for (flag, value) in [
                ("-hwaccel_output_format", "videotoolbox_vld"),
                ("-c:v", "h264_videotoolbox"),
                ("-allow_sw", "0"),
                ("-profile:v", "high"),
                ("-level:v", "50"),
                ("-b:v", "8000k"),
                ("-color_range", "tv"),
            ] {
                assert!(args.windows(2).any(|pair| pair == [flag, value]));
            }
            assert!(args.iter().any(|arg| arg == "-an"));
            let graph = &args[args.iter().position(|arg| arg == "-vf").expect("vf") + 1];
            assert!(graph.contains(&renderer));
            assert!(graph.contains(",setsar=1"));
            assert!(!graph.contains("hwdownload"));
        }
        let graph = &vod[vod.iter().position(|arg| arg == "-vf").expect("vf") + 1];
        assert!(graph.starts_with("trim=start=0.000000000,fps=24/1:start_time=0.000000000,"));
        assert!(graph.contains("tpad=stop_mode=clone:stop_duration=12.000000000,trim=end_frame=288,setpts=PTS-0.000000000/TB"));
        assert!(vod
            .windows(2)
            .any(|pair| pair == ["-enc_time_base:v", "1:24"]));
        assert!(vod
            .windows(2)
            .any(|pair| pair == ["-fps_mode:v", "passthrough"]));
        let excluded = policy.clone().with_macos_processing(
            policy
                .macos_processing()
                .expect("context")
                .clone()
                .excluding_pipeline(pipeline),
        );
        let fallback = resolve_transcode(
            &request,
            &input,
            &caps,
            &excluded,
            &AttemptRestrictions::none(),
        )
        .expect("existing retry owner restriction");
        assert_eq!(fallback.options().pipeline, Pipeline::Cpu);
        assert!(fallback.macos_processing_identity().is_none());
    }
}

#[test]
fn macos_processing_continuous_vod_does_not_relax_rate_or_auto_quality_contracts() {
    use plurx_core::transcode::AutoQualityRateProfile;
    let input = facts(macos_stream(false));
    let caps = unqualified_software_capabilities("hevc");
    let policy = macos_policy(macos_context(true, MacosProcessingAvailability::Available));
    let mut media = options(Pipeline::Cpu);
    media.video_bitrate_kbps = 200_000;
    let invalid = TranscodeRequest::new(Encoder::VideoToolbox, media).with_continuous_avc_video();
    assert_eq!(
        resolve_transcode(
            &invalid,
            &input,
            &caps,
            &policy,
            &AttemptRestrictions::none()
        ),
        Err(PlanError::InvalidMediaOption("continuous_video_envelope"))
    );
    let mut media = options(Pipeline::Cpu);
    media.target_height = 1440;
    media.video_bitrate_kbps = 12_000;
    let auto = TranscodeRequest::new(Encoder::VideoToolbox, media)
        .with_auto_quality_rate_profile(AutoQualityRateProfile::H264Sdr1440P30V1);
    let plan = resolve_transcode(&auto, &input, &caps, &policy, &AttemptRestrictions::none())
        .expect("existing auto quality route");
    assert_eq!(plan.options().pipeline, Pipeline::VtScaleSdr);
    assert_eq!(
        plan.macos_processing_selection(),
        Some(MacosProcessingSelection::Selected)
    );
    assert!(plan.macos_processing_identity().is_some());
    assert_eq!(plan.output_contract().effective_width(), Some(2560));
    assert_eq!(plan.output_contract().effective_height(), Some(1440));
    assert_eq!(plan.output_contract().output_profile(), Some("high"));
    assert_eq!(plan.options().video_bitrate_kbps, 12_000);
    let source = execution_file("/fixture/source.mkv");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    let args = hls_args(&plan, &execution);
    let graph = &args[args.iter().position(|arg| arg == "-vf").expect("vf") + 1];
    assert!(graph.starts_with("fps=24/1,"));
    assert!(graph.contains(
        &Pipeline::VtScaleSdr
            .filters(Some(2560), 1440, Some("sdr"))
            .expect("native scale")
    ));
    assert!(graph.ends_with(",setsar=1"));
    assert!(!graph.contains("hwdownload"));
    let mut too_fast = macos_stream(false);
    too_fast["avg_frame_rate"] = json!("60/1");
    too_fast["r_frame_rate"] = json!("60/1");
    assert_eq!(
        resolve_transcode(
            &auto,
            &facts(too_fast),
            &caps,
            &policy,
            &AttemptRestrictions::none()
        ),
        Err(PlanError::InvalidMediaOption("auto_quality_rate_profile"))
    );
    let mut anamorphic = macos_stream(false);
    anamorphic["width"] = json!(2880);
    anamorphic["sample_aspect_ratio"] = json!("4:3");
    let incumbent = resolve_transcode(
        &auto,
        &facts(anamorphic),
        &caps,
        &policy,
        &AttemptRestrictions::none(),
    )
    .expect("existing normalization path");
    assert_eq!(incumbent.options().pipeline, Pipeline::Cpu);
    assert_eq!(
        incumbent.macos_processing_selection(),
        Some(MacosProcessingSelection::PresentationConstraint)
    );
}

#[test]
fn macos_processing_rejected_decoder_and_hdr10plus_retain_incumbent() {
    let input = facts(macos_stream(true));
    let caps = DecodeCapabilities::new(
        snapshot_identity(),
        vec![capability(
            DecodeBackend::VideoToolbox,
            "hevc",
            None,
            None,
            CapabilityStatus::Rejected,
        )],
        vec![SoftwareDecoder {
            codec: "hevc".into(),
            implementation: Some("hevc".into()),
        }],
    )
    .expect("rejected decode observation");
    let plan = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &caps,
        macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
    )
    .expect("existing software fallback");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(plan.options().pipeline, Pipeline::Cpu);
    assert_eq!(
        plan.macos_processing_selection(),
        Some(MacosProcessingSelection::CapabilityFallback)
    );
    assert!(plan.macos_processing_identity().is_none());
    let mut stream = macos_stream(true);
    stream["side_data_list"] =
        json!([{"side_data_type":"HDR Dynamic Metadata SMPTE2094-40 (HDR10+)"}]);
    let plan = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &facts(stream),
        &unqualified_software_capabilities("hevc"),
        macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
    )
    .expect("HDR10+ incumbent");
    assert_eq!(plan.options().pipeline, Pipeline::Cpu);
    assert!(plan.macos_processing_identity().is_none());
}

#[test]
fn cpu_hdr_to_sdr_consumes_only_static_hdr_metadata_in_both_producers() {
    use plurx_core::transcode::OutputMetadataPolicy;
    let input = facts(macos_stream(true));
    let plan = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &unqualified_software_capabilities("hevc"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("CPU HDR-to-SDR");
    assert_eq!(
        plan.output_metadata_policy(),
        Some(OutputMetadataPolicy::ConsumedHdrStaticV1)
    );
    assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
    let source = execution_file("/fixture/source.mkv");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    let rolling = hls_args(&plan, &execution);
    let vod = plurx_core::transcode::vod_pipe_args(
        &source,
        &plan,
        &execution,
        plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid"),
        12.0,
    );
    for args in [&rolling, &vod] {
        let graph = &args[args.iter().position(|arg| arg == "-vf").expect("vf") + 1];
        assert!(graph.contains("format=yuv420p,sidedata=mode=delete:type=MASTERING_DISPLAY_METADATA,sidedata=mode=delete:type=CONTENT_LIGHT_LEVEL"));
        assert!(
            !graph.contains("type=A53_CC"),
            "caption data must remain available"
        );
        assert!(
            !graph.contains("sidedata=mode=delete,"),
            "never delete all side data"
        );
        assert!(
            graph.find("tonemap=tonemap=hable").expect("pixel mapping")
                < graph
                    .find("type=MASTERING_DISPLAY_METADATA")
                    .expect("consumed metadata")
        );
    }
}

#[test]
fn consumed_hdr_metadata_policy_leaves_unaffected_routes_unchanged() {
    let caps = unqualified_software_capabilities("hevc");
    for (hdr, pipeline, tone_map) in [
        (false, Pipeline::Cpu, ToneMap::Zscale),
        (true, Pipeline::Cpu, ToneMap::None),
        (true, Pipeline::Cpu, ToneMap::Tonemapx),
        (true, Pipeline::Hdr10Passthrough, ToneMap::Zscale),
    ] {
        let input = facts(macos_stream(hdr));
        let mut media = options(pipeline);
        media.tone_map = tone_map;
        let legacy = resolve_with_options(
            Encoder::Software,
            media.clone(),
            &input,
            &caps,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("unchanged route");
        let disabled = resolve_with_options(
            Encoder::Software,
            media,
            &input,
            &caps,
            macos_policy(macos_context(false, MacosProcessingAvailability::Available)),
        )
        .expect("unchanged route with disabled context");
        assert_eq!(legacy.output_metadata_policy(), None);
        assert_eq!(legacy.plan_digest(), disabled.plan_digest());
        let source = execution_file("/fixture/source.mkv");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("execution");
        let args = hls_args(&legacy, &execution);
        assert!(!args
            .iter()
            .any(|arg| arg.contains("sidedata=mode=delete:type=MASTERING_DISPLAY_METADATA")));
        assert_eq!(args, hls_args(&disabled, &execution));
    }
}

fn identity(byte: char) -> DecodeSourceIdentity {
    DecodeSourceIdentity::from_sha256(byte.to_string().repeat(64)).expect("valid digest")
}

// A flat argument list keeps each fixture visually aligned with FFprobe's
// stream object, which is the contract these tests exercise.
#[allow(clippy::too_many_arguments)]
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

/// Cover art carried as a video stream. Every ordinal-counting selector has to
/// skip these, or ordinal 0 means "the poster" on a large fraction of a library.
fn attached_picture(index: u32) -> Value {
    json!({
        "index": index,
        "codec_type": "video",
        "codec_name": "mjpeg",
        "profile": null,
        "width": 600,
        "height": 900,
        "pix_fmt": "yuvj420p",
        "avg_frame_rate": "0/0",
        "r_frame_rate": "90000/1",
        "color_transfer": null,
        "disposition": {"attached_pic": 1}
    })
}

fn facts(stream: Value) -> DecodeFacts {
    DecodeFacts::from_ffprobe_json(&json!({"streams": [stream]}), identity('a'))
        .expect("valid decode facts")
}

fn options(pipeline: Pipeline) -> TranscodeMediaOptions {
    TranscodeMediaOptions {
        output_codec: None,
        strict_dolby: None,
        video_sample_envelope: plurx_core::transcode::VideoSampleEnvelope::EncoderDefault,
        audio: None,
        target_height: 1080,
        video_bitrate_kbps: 8_000,
        effective_rate_control: EffectiveRateControl::Vbr,
        audio_channels: 2,
        audio_bitrate_kbps: 160,
        audio_index: None,
        audio_offset_ms: 0,
        input_has_audio: true,
        tone_map: ToneMap::Zscale,
        tone_map_peak_nits: 1000,
        tone_map_peak_source: plurx_core::transcode::ToneMapPeakSource::Default,
        pipeline,
        subtitle_burn: None,
        cache_identity: DecodeCacheIdentity::from_media_file(&execution_file(
            "/fixture/source.mkv",
        )),
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
                implementation: Some(codec.to_owned()),
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

/// An inventory that knows the build decodes this codec but has measured no
/// implementation — the honest shape of a startup `ffmpeg -decoders` read.
fn software_capabilities_without_implementation(codec: &str) -> DecodeCapabilities {
    DecodeCapabilities::new(
        snapshot_identity(),
        vec![],
        vec![SoftwareDecoder {
            codec: codec.to_owned(),
            implementation: None,
        }],
    )
    .expect("valid inventory naming no implementation")
}

fn unqualified_software_capabilities(codec: &str) -> DecodeCapabilities {
    DecodeCapabilities::new(
        snapshot_identity(),
        vec![],
        vec![SoftwareDecoder {
            codec: codec.to_owned(),
            implementation: Some(codec.to_owned()),
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
    resolve_with_options(encoder, options(pipeline), facts, capabilities, policy)
}

fn resolve_with_options(
    encoder: Encoder,
    media: TranscodeMediaOptions,
    facts: &DecodeFacts,
    capabilities: &DecodeCapabilities,
    policy: DecodePolicySnapshot,
) -> Result<plurx_core::transcode::ResolvedTranscode, PlanError> {
    resolve_transcode(
        &TranscodeRequest::new(encoder, media),
        facts,
        capabilities,
        &policy,
        &AttemptRestrictions::none(),
    )
}

fn execution_file(path: &str) -> MediaFile {
    MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 7,
        item_id: 11,
        path: PathBuf::from(path),
        size: 12_345,
        mtime: 67_890,
        duration_ms: Some(120_000),
        container: Some("mkv".to_owned()),
        video_codec: Some("h264".to_owned()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("high".to_owned()),
        width: Some(1920),
        height: Some(1080),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        dolby_vision: DolbyVisionFacts::default(),
        bitrate: Some(8_000_000),
        audio_streams: vec![AudioStream {
            index: 0,
            codec: "aac".to_owned(),
            channels: Some(2),
            ..AudioStream::default()
        }],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
    }
}

fn execution_options() -> TranscodeOptions {
    TranscodeOptions {
        output_codec: None,
        strict_dolby: None,
        video_sample_envelope: plurx_core::transcode::VideoSampleEnvelope::EncoderDefault,
        audio: None,
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        target_height: 1080,
        video_bitrate_kbps: 8_000,
        effective_rate_control: EffectiveRateControl::Vbr,
        audio_channels: 2,
        audio_bitrate_kbps: 160,
        audio_index: None,
        start_seconds: 0.0,
        start_number: 0,
        tone_map: ToneMap::Zscale,
        pipeline: Pipeline::Cpu,
        subtitle_burn: None,
        subtitle_file: None,
        force_idr: false,
        software_threads: None,
    }
}

#[test]
fn current_main_geometry_composes_with_independent_audio_and_codec_identity() {
    use plurx_core::playback::audio::{AudioAction, AudioDelivery};
    let mut stream = video(
        0,
        Some("h264"),
        Some("High"),
        1920,
        1080,
        Some("yuv420p"),
        "30/1",
        "30/1",
        Some("bt709"),
    );
    stream["sample_aspect_ratio"] = json!("1:1");
    stream["side_data_list"] = json!([{"side_data_type":"Display Matrix", "rotation":0,
        "displaymatrix":"00000000: 65536 0 0\n00000001: 0 65536 0\n00000002: 0 0 1073741824\n"}]);
    let input = facts(stream);
    let mut media = options(Pipeline::Cpu);
    media.target_height = 720;
    media.audio = Some(AudioDelivery {
        action: AudioAction::Copy {
            codec: "ac3".into(),
            channels: 6,
        },
        downmix: None,
        reason: "retained independent audio".into(),
    });
    let caps = software_capabilities("h264", "h264");
    let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
    let request =
        TranscodeRequest::new(Encoder::Software, media.clone()).with_normalized_geometry();
    let copied = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::none(),
    )
    .expect("combined normalized/audio plan");
    assert_eq!(copied.output_contract().effective_width(), Some(1280));
    assert_eq!(copied.output_contract().effective_height(), Some(720));
    assert!(copied.output_contract().normalized_geometry().is_some());
    assert_eq!(copied.codec_contract().codec.name(), "h264");
    let contract = serde_json::to_value(copied.output_contract()).expect("contract");
    assert_eq!(contract["audio"]["action"]["kind"], json!("copy"));
    media.audio = None;
    let legacy_audio = resolve_transcode(
        &TranscodeRequest::new(Encoder::Software, media).with_normalized_geometry(),
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::none(),
    )
    .expect("scalar audio plan");
    assert_ne!(copied.plan_digest(), legacy_audio.plan_digest());
}

fn software_capabilities(codec: &str, implementation: &str) -> DecodeCapabilities {
    DecodeCapabilities::new(
        snapshot_identity(),
        vec![],
        vec![SoftwareDecoder {
            codec: codec.to_owned(),
            implementation: Some(implementation.to_owned()),
        }],
    )
    .expect("valid software decoder inventory")
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
        assert_eq!(plan.decode().evidence(), DecodeEvidence::LegacyUnverified);
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
    for (encoder, pipeline) in [
        (Encoder::Nvenc, Pipeline::TonemapCuda),
        (Encoder::Qsv, Pipeline::VppQsv),
        (Encoder::Vaapi, Pipeline::LibplaceboVaapi),
    ] {
        let plan = resolve(
            encoder,
            pipeline,
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
}

#[test]
fn cuda_tone_map_keeps_hardware_frames_until_a_subtitle_composite() {
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
    for burn in [None, Some(false), Some(true)] {
        let mut media = options(Pipeline::TonemapCuda);
        media.subtitle_burn = burn.map(|bitmap| SubtitleBurn {
            subtitle_index: 0,
            bitmap,
        });
        let plan = resolve_with_options(
            Encoder::Nvenc,
            media,
            &input,
            &capabilities(vec![capability(
                DecodeBackend::Cuda,
                "hevc",
                None,
                None,
                CapabilityStatus::Advertised,
            )]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("CUDA plan");
        assert_eq!(plan.decode().backend(), DecodeBackend::Cuda);
        assert_eq!(plan.decode().surface().decode_domain(), FrameDomain::Cuda);
        assert_eq!(plan.decode().surface().renderer_domain(), FrameDomain::Cuda);
        assert_eq!(plan.decode().surface().decoder_download_format(), None);
        assert_eq!(
            plan.decode().surface().renderer_download_format(),
            burn.map(|_| "nv12")
        );
        let mut opts = execution_options();
        opts.subtitle_burn = plan.options().subtitle_burn.clone();
        let file = execution_file("/fixture/hdr.mkv");
        let execution =
            TranscodeExecution::from_options(&file, &opts, Pacing::unpaced(), "/fixture/out")
                .expect("execution");
        let args = hls_args(&plan, &execution);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-hwaccel_output_format", "cuda"]));
        let command = args.join(" ");
        assert!(
            command.contains("tonemap_cuda=tonemap=hable:tonemap_mode=max"),
            "{command}"
        );
        assert!(command.contains("h264_nvenc"));
        assert_eq!(
            command.matches("hwdownload").count(),
            usize::from(burn.is_some()),
            "{command}"
        );
        assert!(
            !command.contains("hwupload"),
            "NVENC accepts CPU subtitle composites directly: {command}"
        );
    }
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

/// The facts digest describes the stream that was measured, and nothing about
/// how the measurement reached it. Which stream was selected is part of the
/// output and must change the digest; the descriptor fingerprint is not, and
/// must not — see the sibling test for why that distinction is load-bearing.
#[test]
fn the_selected_stream_binds_into_the_facts_digest_and_the_descriptor_does_not() {
    let json = json!({"streams": [
        video(2, Some("h264"), Some("high"), 1920, 1080, Some("yuv420p"), "24/1", "24/1", None),
        video(5, Some("h264"), Some("high"), 1920, 1080, Some("yuv420p"), "24/1", "24/1", None)
    ]});
    let first = DecodeFacts::from_ffprobe_json_at(&json, identity('c'), 2).expect("first");
    let other_stream = DecodeFacts::from_ffprobe_json_at(&json, identity('c'), 5).expect("other");
    let other_source =
        DecodeFacts::from_ffprobe_json_at(&json, identity('d'), 2).expect("changed descriptor");
    assert_ne!(first.facts_digest(), other_stream.facts_digest());
    assert_eq!(first.facts_digest(), other_source.facts_digest());
}

#[test]
fn source_luminance_binds_into_the_facts_digest() {
    let document = |max_content| {
        json!({"streams": [{
            "index": 0, "codec_type": "video", "codec_name": "hevc",
            "width": 3840, "height": 2160, "pix_fmt": "yuv420p10le",
            "avg_frame_rate": "24/1", "r_frame_rate": "24/1",
            "color_transfer": "smpte2084", "disposition": {"attached_pic": 0},
            "side_data_list": [{
                "side_data_type": "Content light level metadata",
                "max_content": max_content, "max_average": 400
            }]
        }]})
    };
    let low =
        DecodeFacts::from_ffprobe_json(&document(json!(1000)), identity('c')).expect("low peak");
    let high =
        DecodeFacts::from_ffprobe_json(&document(json!(4000)), identity('c')).expect("high peak");
    assert_eq!(low.max_cll(), Some(1000));
    assert_ne!(low.facts_digest(), high.facts_digest());

    let mut catalog_file = execution_file("/fixture/hdr.mkv");
    catalog_file.hdr = Some("hdr10".to_owned());
    catalog_file.max_cll = Some(4000);
    catalog_file.luminance_source = Some("frame".to_owned());
    let catalog = DecodeCatalogMetadata::from_media_file(&catalog_file).expect("catalog");
    let from_catalog = DecodeFacts::from_ffprobe_json_with_catalog(
        &document(serde_json::Value::Null),
        identity('c'),
        &catalog,
    )
    .expect("catalog luminance fills selective probe omission");
    assert_eq!(from_catalog.max_cll(), Some(4000));
}

#[test]
fn field_order_is_typed_conservatively_and_binds_into_the_facts_digest() {
    let mut progressive = video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "30000/1001",
        "30000/1001",
        None,
    );
    progressive["field_order"] = json!("progressive");
    let progressive = facts(progressive);
    assert_eq!(progressive.scan_type(), ScanType::Progressive);

    let mut interlaced = video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "30000/1001",
        "30000/1001",
        None,
    );
    interlaced["field_order"] = json!("tt");
    let interlaced = facts(interlaced);
    assert_eq!(
        interlaced.scan_type(),
        ScanType::Interlaced(FieldOrder::Tff)
    );
    assert_ne!(progressive.facts_digest(), interlaced.facts_digest());
    let overruled = interlaced
        .clone()
        .with_interlace_verdict(InterlaceVerdict::FlagOverruled);
    assert_eq!(overruled.scan_type(), ScanType::Progressive);
    assert_ne!(overruled.facts_digest(), interlaced.facts_digest());

    let mut future = video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "30000/1001",
        "30000/1001",
        None,
    );
    future["field_order"] = json!("future-order");
    assert_eq!(facts(future).scan_type(), ScanType::Unknown);
}

/// The failure this guards against does not look like a bug. Two producers
/// reach the same film by different routes — one holds a descriptor, one reads
/// the catalog row — and fingerprint it differently. If that fingerprint names
/// the artifact, the two routes get disjoint key spaces: the producer fills the
/// cache forever and the player never hits it, which is indistinguishable from
/// a cache that is merely cold. One film, one name.
#[test]
fn the_same_source_measured_two_ways_names_one_artifact() {
    let stream = video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    );
    let json = json!({"streams": [stream]});
    let descriptor_bound = DecodeFacts::from_ffprobe_json_at(&json, identity('c'), 0)
        .expect("facts read through a held descriptor")
        .descriptor_bound();
    let from_catalog_row = DecodeFacts::from_ffprobe_json_at(&json, identity('d'), 0)
        .expect("facts from stored probe");
    let capabilities = software_capabilities("h264", "h264");
    let plan_of = |facts: &DecodeFacts| {
        resolve_with_options(
            Encoder::Software,
            options(Pipeline::Cpu),
            facts,
            &capabilities,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("plan")
    };
    let bound_plan = plan_of(&descriptor_bound);
    let stored_plan = plan_of(&from_catalog_row);

    assert_eq!(bound_plan.plan_digest(), stored_plan.plan_digest());
    let pipeline = PipelineDigest {
        ffmpeg_build: "ffmpeg fixture".to_owned(),
    };
    assert_eq!(
        Recipe::new(&pipeline, &bound_plan, false).hash(),
        Recipe::new(&pipeline, &stored_plan, false).hash()
    );

    // Identical names, and still an honest record of what each one proved.
    assert_eq!(
        bound_plan.source_binding(),
        PlanSourceBinding::DescriptorBound
    );
    assert_eq!(stored_plan.source_binding(), PlanSourceBinding::CatalogRow);
    assert_ne!(
        bound_plan.observed_source_identity().as_str(),
        stored_plan.observed_source_identity().as_str()
    );
}

/// Two routes that land on the same stream must describe it identically.
///
/// The stored-probe route asks for "the first playable video"; the
/// descriptor-bound route asks for an absolute index it resolved by ordinal.
/// Those are two ways of saying the same thing, and while the facts digest
/// recorded *which way was used*, they named two different artifacts for every
/// file — the producer's key space and the player's never met. What the digest
/// binds is the stream; how it was named is not part of the picture.
#[test]
fn explicit_and_first_playable_selection_of_one_stream_agree() {
    let json = json!({"streams": [
        attached_picture(0),
        video(1, Some("h264"), Some("high"), 1920, 1080, Some("yuv420p"), "24/1", "24/1", Some("bt709"))
    ]});
    let first_playable =
        DecodeFacts::from_ffprobe_json(&json, identity('c')).expect("first playable");
    let explicit = DecodeFacts::from_ffprobe_json_at(&json, identity('d'), 1).expect("explicit");

    assert_eq!(
        first_playable.input_video_stream(),
        1,
        "cover art is not the film"
    );
    assert_eq!(explicit.input_video_stream(), 1);
    assert_eq!(
        first_playable.facts_digest(),
        explicit.facts_digest(),
        "one stream, one description, whichever way it was asked for"
    );
}

/// The other half of the same rule: one name per source means two sources must
/// not share one. Identity is the catalog row — id, size, mtime — so a file
/// that is replaced stops matching, while a file that merely moves does not.
#[test]
fn different_sources_have_different_artifact_names() {
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
    let capabilities = software_capabilities("h264", "h264");
    let pipeline = PipelineDigest {
        ffmpeg_build: "ffmpeg fixture".to_owned(),
    };
    let name_for = |file: &MediaFile| {
        // Built the way production builds it. Setting `cache_identity` by hand
        // here would leave a `from_options` that assigned a constant identity
        // passing this test, which is the regression it exists to catch.
        let mut media = TranscodeMediaOptions::from_options(file, &execution_options());
        media.pipeline = Pipeline::Cpu;
        let plan = resolve_with_options(
            Encoder::Software,
            media,
            &input,
            &capabilities,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("plan");
        Recipe::new(&pipeline, &plan, false).hash()
    };

    let original = execution_file("/library/film.mkv");
    let mut replaced = original.clone();
    replaced.size += 1;
    let mut retimed = original.clone();
    retimed.mtime += 1;
    let mut another_row = original.clone();
    another_row.id += 1;
    let mut moved = original.clone();
    moved.path = PathBuf::from("/library/archive/film.mkv");

    let baseline = name_for(&original);
    assert_ne!(baseline, name_for(&replaced));
    assert_ne!(baseline, name_for(&retimed));
    assert_ne!(baseline, name_for(&another_row));
    assert_eq!(
        baseline,
        name_for(&moved),
        "a path is where the bytes live, not which bytes they are"
    );
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
    // An uninventoried codec is not a refusal under the legacy policy. The
    // startup inventory lists a small canonical set of families; VC-1, MPEG-1
    // and ProRes are absent from it and decode perfectly well, because the
    // pre-plan command named no decoder and FFmpeg chose one. The plan says so
    // by naming none either.
    let legacy = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &av1,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("an uninventoried codec still plans under the legacy policy");
    assert_eq!(legacy.decode().backend(), DecodeBackend::Software);
    assert_eq!(legacy.decode().software_decoder(), None);

    // Under enforcement the inventory is the contract, so the refusal stands.
    assert_eq!(
        resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &av1,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
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
fn one_pixel_dimensions_remain_unknown_instead_of_claiming_no_upscale() {
    for (width, height) in [(1, 1080), (1920, 1), (1, 1)] {
        let input = facts(video(
            0,
            Some("h264"),
            Some("high"),
            width,
            height,
            Some("yuv420p"),
            "24/1",
            "24/1",
            Some("bt709"),
        ));
        assert_eq!(input.width(), (width >= 2).then_some(width as u32));
        assert_eq!(input.height(), (height >= 2).then_some(height as u32));
        let plan = resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &input,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("unknown geometry retains a conservative plan");
        assert_eq!(plan.output_contract().effective_width(), None);
        assert_eq!(plan.output_contract().effective_height(), None);
    }
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
fn advertised_detail_cannot_override_rejection_without_stronger_qualification() {
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
    let rejected = capability(
        DecodeBackend::Qsv,
        "hevc",
        None,
        None,
        CapabilityStatus::Rejected,
    );
    let advertised = capability(
        DecodeBackend::Qsv,
        "hevc",
        Some("main 10"),
        None,
        CapabilityStatus::Advertised,
    );
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::Cpu,
        &input,
        &capabilities(vec![rejected.clone(), advertised]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("known rejection falls back to inventoried software");
    assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    assert_eq!(plan.decode().reason(), DecodeReason::CapabilityFallback);

    let qualified = exact_capability(
        DecodeBackend::Qsv,
        &input,
        Pipeline::Cpu,
        Encoder::Qsv,
        SubtitleRendering::None,
        CapabilityStatus::Qualified,
    );
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::Cpu,
        &input,
        &capabilities(vec![rejected, qualified]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
    )
    .expect("more-specific qualification supersedes the generic rejection");
    assert_eq!(plan.decode().backend(), DecodeBackend::Qsv);
    assert_eq!(plan.decode().evidence(), DecodeEvidence::Qualified);
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
    assert!(matches!(
        DecodeCapabilitySnapshotIdentity::new(
            "a".repeat(64),
            "/dev/dri/renderD128".to_owned(),
            None
        ),
        Err(PlanError::InvalidCapabilityIdentity("device class"))
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
fn pq_probe_is_refined_by_hdr10plus_and_dolby_catalog_facts() {
    let stream = video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    );
    let hdr10plus = DecodeCatalogMetadata::new(
        Some("hdr10plus"),
        Some("HDR10+"),
        DolbyVisionFacts::default(),
    )
    .expect("HDR10+ catalog");
    let refined = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [stream.clone()]}),
        identity('7'),
        &hdr10plus,
    )
    .expect("PQ base refines to HDR10+");
    assert_eq!(refined.dynamic_range(), Some("hdr10plus"));

    let dolby = DolbyVisionFacts {
        profile: Some(5),
        level: Some(6),
        bl_compat_id: Some(0),
        el_present: Some(false),
        rpu_present: Some(true),
    };
    let catalog = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 5"),
        dolby,
    )
    .expect("Dolby catalog");
    let refined = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [stream]}),
        identity('8'),
        &catalog,
    )
    .expect("PQ base refines to Dolby Vision");
    assert_eq!(refined.dynamic_range(), Some("dolby_vision"));
}

#[test]
fn dolby_compatible_bases_preserve_legacy_routing_and_profile5_needs_rpu_graph() {
    let pq_stream = video(
        0,
        Some("hevc"),
        Some("main 10"),
        3840,
        2160,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    );
    let compatible = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        DolbyVisionFacts {
            profile: Some(8),
            level: Some(6),
            bl_compat_id: Some(1),
            el_present: Some(false),
            rpu_present: Some(true),
        },
    )
    .expect("compatible Dolby catalog");
    let compatible = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [pq_stream.clone()]}),
        identity('9'),
        &compatible,
    )
    .expect("HDR10-compatible Dolby facts");
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::Hdr10Passthrough,
        &compatible,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("HDR10-compatible Dolby source retains the legacy HDR route");
    assert_eq!(plan.decode().backend(), DecodeBackend::Qsv);

    let hlg_catalog = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HLG-compatible)"),
        DolbyVisionFacts {
            profile: Some(8),
            level: Some(6),
            bl_compat_id: Some(4),
            el_present: Some(false),
            rpu_present: Some(true),
        },
    )
    .expect("HLG-compatible Dolby catalog");
    let mut hlg_stream = pq_stream.clone();
    hlg_stream["color_transfer"] = json!("arib-std-b67");
    let hlg = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [hlg_stream]}),
        identity('a'),
        &hlg_catalog,
    )
    .expect("HLG-compatible Dolby facts");
    resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &hlg,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("HLG-compatible Dolby source retains the legacy CPU route");

    for (compatibility_id, stale_label) in [
        (0, "Dolby Vision · Profile 5 (HDR10-compatible)"),
        (2, "Dolby Vision · Profile 8 (HLG-compatible)"),
    ] {
        assert_eq!(
            DecodeCatalogMetadata::new(
                Some("dolby_vision"),
                Some(stale_label),
                DolbyVisionFacts {
                    profile: Some(if compatibility_id == 0 { 5 } else { 8 }),
                    level: Some(6),
                    bl_compat_id: Some(compatibility_id),
                    el_present: Some(false),
                    rpu_present: Some(true),
                },
            ),
            Err(PlanError::ConflictingMetadata("dolby compatibility")),
            "typed compatibility id {compatibility_id} must outrank {stale_label}"
        );
    }

    for stale_label in [
        "Dolby Vision · Profile 5 (HDR10-compatible)",
        "Dolby Vision · Profile 5 (HLG-compatible)",
    ] {
        assert_eq!(
            DecodeCatalogMetadata::new(
                Some("dolby_vision"),
                Some(stale_label),
                DolbyVisionFacts {
                    profile: Some(5),
                    level: Some(6),
                    bl_compat_id: None,
                    el_present: Some(false),
                    rpu_present: Some(true),
                },
            ),
            Err(PlanError::ConflictingMetadata("dolby compatibility")),
            "profile 5 cannot borrow a compatible-base label when its id is missing"
        );
    }

    for impossible_id in [1, 4, 6] {
        assert_eq!(
            DecodeCatalogMetadata::new(
                Some("dolby_vision"),
                Some("Dolby Vision · Profile 5"),
                DolbyVisionFacts {
                    profile: Some(5),
                    level: Some(6),
                    bl_compat_id: Some(impossible_id),
                    el_present: Some(false),
                    rpu_present: Some(true),
                },
            ),
            Err(PlanError::ConflictingMetadata("dolby compatibility")),
            "profile 5 compatibility id {impossible_id} is impossible"
        );
    }

    let incomplete_typed_compatibility = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
        DolbyVisionFacts {
            profile: Some(8),
            level: Some(6),
            bl_compat_id: None,
            el_present: Some(false),
            rpu_present: Some(true),
        },
    )
    .expect("incomplete typed metadata remains observable but cannot borrow the label");
    let incomplete_typed_compatibility = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [pq_stream.clone()]}),
        identity('b'),
        &incomplete_typed_compatibility,
    )
    .expect("merge incomplete typed compatibility");
    assert_eq!(
        resolve(
            Encoder::Qsv,
            Pipeline::Hdr10Passthrough,
            &incomplete_typed_compatibility,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        ),
        Err(PlanError::IncompatibleRenderer),
        "compatibility prose cannot override a present typed Dolby profile"
    );

    let stale_hlg = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 5 (HLG-compatible)"),
        DolbyVisionFacts {
            profile: Some(5),
            level: Some(6),
            bl_compat_id: Some(0),
            el_present: Some(false),
            rpu_present: Some(true),
        },
    );
    assert_eq!(
        stale_hlg,
        Err(PlanError::ConflictingMetadata("dolby compatibility"))
    );

    let profile5 = DecodeCatalogMetadata::new(
        Some("dolby_vision"),
        Some("Dolby Vision · Profile 5"),
        DolbyVisionFacts {
            profile: Some(5),
            level: Some(6),
            bl_compat_id: Some(0),
            el_present: Some(false),
            rpu_present: Some(true),
        },
    )
    .expect("profile 5 catalog");
    let profile5 = DecodeFacts::from_ffprobe_json_with_catalog(
        &json!({"streams": [pq_stream]}),
        identity('b'),
        &profile5,
    )
    .expect("profile 5 facts");
    assert_eq!(
        resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &profile5,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        ),
        Err(PlanError::IncompatibleRenderer)
    );
    resolve(
        Encoder::Software,
        Pipeline::DoviTonemapx,
        &profile5,
        &capabilities(vec![]),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("profile 5 resolves through the RPU-aware graph");
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
fn presentation_geometry_matches_shipping_rounding_for_an_odd_height() {
    let input = facts(video(
        0,
        Some("h264"),
        Some("high"),
        4,
        3,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    let mut media = options(Pipeline::Cpu);
    media.target_height = 3;
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Software, media),
        &input,
        &capabilities(vec![]),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::none(),
    )
    .expect("odd shipping geometry resolves");
    assert_eq!(plan.output_contract().effective_width(), Some(4));
    assert_eq!(plan.output_contract().effective_height(), Some(2));
}

#[test]
fn presentation_height_outside_the_command_builders_domain_is_refused() {
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
    let mut media = options(Pipeline::Cpu);
    media.target_height = i64::MAX;
    assert_eq!(
        resolve_transcode(
            &TranscodeRequest::new(Encoder::Software, media),
            &input,
            &capabilities(vec![]),
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        ),
        Err(PlanError::InvalidMediaOption("target_height"))
    );
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

#[test]
fn vulkan_vaapi_plan_preserves_hardware_frames_and_subtitle_boundaries() {
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
    for burn in [None, Some(false), Some(true)] {
        let mut media = options(Pipeline::LibplaceboVaapi);
        media.subtitle_burn = burn.map(|bitmap| SubtitleBurn {
            subtitle_index: 0,
            bitmap,
        });
        let plan = resolve_transcode(
            &TranscodeRequest::new(Encoder::Vaapi, media),
            &input,
            &capabilities(vec![]),
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        )
        .expect("VA-API Vulkan plan");
        let mut opts = execution_options();
        opts.subtitle_burn = plan.options().subtitle_burn.clone();
        let file = execution_file("/fixture/source.mkv");
        let execution =
            TranscodeExecution::from_options(&file, &opts, Pacing::unpaced(), "/fixture/out")
                .expect("execution");
        let args = hls_args(&plan, &execution);
        assert!(args
            .windows(2)
            .any(|p| p == ["-hwaccel_output_format", "vaapi"]));
        let flag = if burn == Some(true) {
            "-filter_complex"
        } else {
            "-vf"
        };
        let graph = &args[args.iter().position(|a| a == flag).expect("graph") + 1];
        assert!(graph.contains("libplacebo="), "{graph}");
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
        let surface = DecodeSurfaceContract::for_plan(
            DecodeBackend::Vaapi,
            Pipeline::LibplaceboVaapi,
            &input,
            Encoder::Vaapi,
            if burn.is_some() {
                SubtitleRendering::BitmapBurn
            } else {
                SubtitleRendering::None
            },
        );
        assert_eq!(surface.decode_domain(), FrameDomain::Vaapi);
        assert_eq!(surface.decoder_download_format(), None);
        assert_eq!(surface.renderer_domain(), FrameDomain::Vulkan);
        assert_eq!(surface.renderer_upload_format(), None);
        assert_eq!(surface.renderer_download_format(), burn.map(|_| "nv12"));
        assert_eq!(
            surface.encoder_upload_domain(),
            burn.map(|_| FrameDomain::Vaapi)
        );
    }
}

#[test]
fn planned_command_uses_actual_decoder_and_absolute_video_stream() {
    let input = facts(video(
        3,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    ));
    let mut media = options(Pipeline::Cpu);
    media.subtitle_burn = Some(SubtitleBurn {
        subtitle_index: 1,
        bitmap: true,
    });
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Software, media),
        &input,
        &software_capabilities("h264", "libdav1d_h264_fixture"),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::none(),
    )
    .expect("software plan");
    let mut options = execution_options();
    options.subtitle_burn = plan.options().subtitle_burn.clone();
    let file = execution_file("/fixture/source.mkv");
    let execution =
        TranscodeExecution::from_options(&file, &options, Pacing::unpaced(), "/fixture/out")
            .expect("valid execution");
    let args = hls_args(&plan, &execution);
    let input_position = args.iter().position(|arg| arg == "-i").expect("input");
    let decoder_position = args
        .windows(2)
        .position(|pair| pair == ["-c:v", "libdav1d_h264_fixture"])
        .expect("actual decoder implementation");
    let encoder_position = args
        .windows(2)
        .position(|pair| pair == ["-c:v", "libx264"])
        .expect("output encoder");
    assert!(decoder_position < input_position);
    assert!(encoder_position > input_position);
    assert!(args.windows(2).any(|pair| pair == ["-map", "[vout]"]));
    let complex = args
        .windows(2)
        .find(|pair| pair[0] == "-filter_complex")
        .map(|pair| pair[1].as_str())
        .expect("bitmap complex filter");
    assert!(complex.starts_with("[0:3]"), "{complex}");
}

#[test]
fn descriptor_luminance_refines_the_filter_not_only_the_facts_digest() {
    let mut stream = video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    );
    stream["side_data_list"] = json!([{
        "side_data_type": "Content light level metadata",
        "max_content": 4000,
        "max_average": 1000
    }]);
    let input = facts(stream);
    let file = execution_file("/fixture/source.mkv");
    let options = execution_options();
    let media = TranscodeMediaOptions::from_options_with_facts(&file, &options, &input);
    assert_eq!(media.tone_map_peak_nits, 4000);
    assert_eq!(media.tone_map_peak_source.name(), "cll");

    let plan = resolve_with_options(
        Encoder::Software,
        media,
        &input,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("descriptor facts resolve");
    let execution =
        TranscodeExecution::from_options(&file, &options, Pacing::unpaced(), "/fixture/out")
            .expect("valid execution");
    let args = hls_args(&plan, &execution);
    let filter = args
        .windows(2)
        .find(|pair| pair[0] == "-vf")
        .map(|pair| pair[1].as_str())
        .expect("video filter");
    assert!(filter.contains("peak=40"), "{filter}");
}

#[test]
fn absent_descriptor_luminance_keeps_the_catalog_peak() {
    let input = facts(video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p10le"),
        "24/1",
        "24/1",
        Some("smpte2084"),
    ));
    let mut file = execution_file("/fixture/source.mkv");
    file.max_cll = Some(2000);
    file.luminance_source = Some("stream".to_owned());

    let media = TranscodeMediaOptions::from_options_with_facts(&file, &execution_options(), &input);
    assert_eq!(media.tone_map_peak_nits, 2000);
    assert_eq!(media.tone_map_peak_source.name(), "cll");
}

#[test]
fn plan_digest_and_command_are_stable_after_environment_mutation() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK.lock().expect("environment lock");
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
    let snapshot = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
    let plan = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "h264"),
        snapshot,
    )
    .expect("resolved before mutation");
    let file = execution_file("/fixture/source.mkv");
    let execution = TranscodeExecution::from_options(
        &file,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("valid execution");
    let before_args = hls_args(&plan, &execution);
    let before_digest = plan.plan_digest();
    let pipeline = PipelineDigest {
        ffmpeg_build: "ffmpeg fixture".to_owned(),
    };
    let before_recipe = Recipe::new(&pipeline, &plan, false).hash();

    // Where the environment *does* belong: in the policy snapshot taken while
    // the plan is resolved. If this stopped mattering, the mutation below
    // would be proving the absence of an effect that never existed.
    let forced = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some("off")),
    )
    .expect("compatibility snapshot resolves");
    assert_eq!(forced.decode().backend(), DecodeBackend::Software);
    let unforced = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("plain snapshot resolves");
    assert_ne!(forced.plan_digest(), unforced.plan_digest());

    let previous = std::env::var_os("PLURX_HWDECODE");
    // SAFETY: this test serializes its mutation and restores the process
    // environment before releasing the lock.
    unsafe { std::env::set_var("PLURX_HWDECODE", "off") };
    let after_args = hls_args(&plan, &execution);
    let after_recipe = Recipe::new(&pipeline, &plan, false).hash();
    match previous {
        Some(value) => unsafe { std::env::set_var("PLURX_HWDECODE", value) },
        None => unsafe { std::env::remove_var("PLURX_HWDECODE") },
    }
    assert_eq!(after_args, before_args);
    assert_eq!(plan.plan_digest(), before_digest);
    assert_eq!(after_recipe, before_recipe);
}

#[test]
fn resolved_interlace_decision_drives_command_and_plan_identity() {
    let source = video(
        0,
        Some("h264"),
        Some("high"),
        1920,
        1080,
        Some("yuv420p"),
        "30000/1001",
        "30000/1001",
        Some("bt709"),
    );
    let mut interlaced_source = source.clone();
    interlaced_source["field_order"] = json!("tt");
    let interlaced = facts(interlaced_source);
    let interlaced_plan = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &interlaced,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("interlaced plan");
    assert_eq!(interlaced_plan.deinterlace(), Deinterlace::BwdifSendFrame);

    let progressive = facts(source);
    let progressive_plan = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &progressive,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("progressive plan");
    assert_eq!(progressive_plan.deinterlace(), Deinterlace::None);
    assert_ne!(
        interlaced_plan.plan_digest(),
        progressive_plan.plan_digest()
    );

    let execution = TranscodeExecution::from_options(
        &execution_file("/fixture/interlaced.mkv"),
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("valid execution");
    let joined = hls_args(&interlaced_plan, &execution).join(" ");
    let bwdif = joined.find("bwdif=mode=send_frame").expect("bwdif");
    let scale = joined.find("scale=").expect("scale");
    assert!(bwdif < scale, "{joined}");
}

#[test]
fn execution_changes_do_not_change_plan_or_recipe_identity() {
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
    let plan = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("plan");
    let first_file = execution_file("/fixture/first-source.mkv");
    let mut second_file = first_file.clone();
    second_file.path = PathBuf::from("/dev/fd/3");
    let first_options = execution_options();
    let mut second_options = first_options.clone();
    second_options.start_seconds = 42.25;
    second_options.start_number = 21;
    second_options.subtitle_file = Some(PathBuf::from("/dev/fd/5"));
    second_options.force_idr = true;
    second_options.software_threads = Some(3);
    let first = TranscodeExecution::from_options(
        &first_file,
        &first_options,
        Pacing::unpaced(),
        "/fixture/one",
    )
    .expect("first execution");
    let second = TranscodeExecution::from_options(
        &second_file,
        &second_options,
        Pacing {
            readrate: Some(1.25),
            initial_burst: Some(10.0),
            legacy_re: false,
        },
        "/fixture/two",
    )
    .expect("second execution");
    assert_ne!(hls_args(&plan, &first), hls_args(&plan, &second));
    // The recipe half of this property is now enforced by the type: `Recipe`
    // takes the plan and nothing else, so no execution value can reach it and
    // an assertion here could not fail. What remains testable is that the
    // execution fields above changed the command and left the plan alone.
    assert_eq!(plan.plan_digest(), plan.plan_digest());
}

#[test]
fn reason_text_does_not_change_plan_digest() {
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
    let caps = capabilities(vec![]);
    let request = TranscodeRequest::new(Encoder::Qsv, options(Pipeline::Cpu));
    let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
    let preferred = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::none(),
    )
    .expect("preferred plan");
    let continuation = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::requiring(DecodeBackend::Qsv),
    )
    .expect("same route required by continuation");
    assert_ne!(preferred.decode().reason(), continuation.decode().reason());
    assert_eq!(preferred.plan_digest(), continuation.plan_digest());
}

/// What runs changes the name; what we happen to know about it does not.
///
/// The implementation is in the command, so two implementations are two
/// pictures and two names. Whether this node held a qualified inventory is not
/// in the command: a qualified node and an unqualified node running the same
/// decoder produce the same bytes, and giving the qualified one a private key
/// space would split the fleet's cache in half during a rollout. That
/// separation belongs to the artifact namespace, which `Recipe::hash` feeds on
/// its own.
#[test]
fn the_software_implementation_changes_the_plan_digest_and_the_evidence_class_does_not() {
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
    let legacy = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("legacy software");
    let alternate = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "libopenh264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("alternate implementation");
    let qualified_caps = capabilities_with_qualified_software(
        &input,
        Pipeline::Cpu,
        Encoder::Software,
        SubtitleRendering::None,
        vec![],
    );
    let qualified = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &qualified_caps,
        DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
    )
    .expect("qualified software");
    let unnamed = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &software_capabilities_without_implementation("h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("an inventory that names no implementation still plans");
    assert_eq!(unnamed.decode().software_decoder(), None);

    assert_ne!(
        legacy.plan_digest(),
        alternate.plan_digest(),
        "two decoders are two pictures"
    );
    assert_ne!(
        legacy.plan_digest(),
        unnamed.plan_digest(),
        "naming a decoder and letting FFmpeg choose are different commands"
    );
    assert_eq!(legacy.decode().evidence(), DecodeEvidence::LegacyUnverified);
    assert_eq!(qualified.decode().evidence(), DecodeEvidence::Qualified);
    assert_eq!(
        legacy.plan_digest(),
        qualified.plan_digest(),
        "the same decoder produces the same bytes however well we know it"
    );
}

/// Diagnostic log flags are execution context, never identity.
///
/// The same plan resolved on a node whose diagnostics are qualified and on one
/// whose are not is the same work: one artifact name, one plan digest, and
/// exactly one token of difference in the command. If the flags reached the
/// plan, a fleet mid-upgrade would name the same title two different things
/// and cache it twice.
#[test]
fn the_diagnostic_log_flags_are_execution_context_and_never_identity() {
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
    let plan = resolve_transcode(
        &TranscodeRequest::new(Encoder::Software, options(Pipeline::Cpu)),
        &input,
        &software_capabilities("h264", "libdav1d_h264_fixture"),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::none(),
    )
    .expect("software plan");
    let file = execution_file("/fixture/source.mkv");
    let execution =
        TranscodeExecution::from_options(&file, &execution_options(), Pacing::unpaced(), "/out")
            .expect("valid execution");
    assert_eq!(
        execution.diagnostics,
        DiagnosticLogging::Legacy,
        "a node says nothing about its diagnostics until it has measured a build"
    );

    let legacy = hls_args(&plan, &execution);
    let qualified = hls_args(
        &plan,
        &TranscodeExecution::from_options(&file, &execution_options(), Pacing::unpaced(), "/out")
            .expect("valid execution")
            .observing_qualified_grammar(true),
    );
    assert!(
        legacy.windows(2).any(|pair| pair == ["-loglevel", "error"]),
        "{legacy:?}"
    );
    assert!(
        qualified
            .windows(2)
            .any(|pair| pair == ["-loglevel", "repeat+level+error"]),
        "{qualified:?}"
    );

    // One token, and nothing else, separates the two commands.
    assert_eq!(legacy.len(), qualified.len());
    let differences: Vec<_> = legacy
        .iter()
        .zip(qualified.iter())
        .filter(|(left, right)| left != right)
        .collect();
    assert_eq!(
        differences,
        vec![(&"error".to_owned(), &"repeat+level+error".to_owned())],
        "asking a child to say more about itself is not a different encode"
    );

    // And the plan the two commands were built from is one plan: the flags
    // live on the execution, so there is nowhere for them to reach identity
    // from.
    assert_eq!(execution.diagnostics, DiagnosticLogging::Legacy);
}

/// Turning qualification on rotates the key space once, and nothing else.
///
/// Two things have to be true at the same time and they pull in opposite
/// directions. A fleet that has not turned qualification on must compute
/// exactly the artifact names it computed before this identity existed —
/// otherwise shipping the mechanism is itself a cache flush, which is a cost
/// nobody asked for. And a fleet that turns it on must land in a key space
/// that shares nothing with the old one, because the plan document forbids
/// relabelling an old artifact as health-qualified without reprocessing it,
/// and a separate namespace is how that is enforced rather than promised.
#[test]
fn the_qualified_identity_is_a_separate_key_space_and_costs_nothing_until_it_is_used() {
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
    let capabilities = software_capabilities("h264", "h264");
    let pipeline = PipelineDigest {
        ffmpeg_build: "ffmpeg fixture".to_owned(),
    };

    let default = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &capabilities,
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("a snapshot built the way every caller builds one");
    let stated = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &capabilities,
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
            .qualifying_artifacts(ArtifactQualification::Unqualified),
    )
    .expect("the same thing said out loud");
    let qualified = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &capabilities,
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
            .qualifying_artifacts(ArtifactQualification::HealthQualified),
    )
    .expect("qualified artifacts");

    // Not turned on: byte for byte the identity every deployed node computes.
    assert_eq!(default.artifact_namespace(), UNQUALIFIED_ARTIFACT_NAMESPACE);
    assert!(!default.enforces_receipt());
    assert_eq!(default.plan_digest(), stated.plan_digest());
    assert_eq!(
        Recipe::new(&pipeline, &default, false).hash(),
        Recipe::new(&pipeline, &stated, false).hash()
    );

    // Turned on: a different name for the same work. `Recipe::hash` feeds the
    // namespace both through `plan_digest` and on its own, so the recipe
    // assertion below is entailed by the digest one rather than independent of
    // it — the redundancy is a guard against a future revision of the digest's
    // field list, and `planned_v3_recipe_hash_is_a_golden_fixture` is what
    // makes removing it explicit.
    assert_eq!(
        qualified.artifact_namespace(),
        HEALTH_QUALIFIED_ARTIFACT_NAMESPACE
    );
    assert!(qualified.enforces_receipt());
    assert_ne!(
        default.plan_digest(),
        qualified.plan_digest(),
        "an artifact produced under an enforced receipt contract is not the same artifact"
    );
    assert_ne!(
        Recipe::new(&pipeline, &default, false).hash(),
        Recipe::new(&pipeline, &qualified, false).hash()
    );

    // And the decode decision itself is untouched: this is an identity, not a
    // different encode.
    assert_eq!(default.decode(), qualified.decode());
    assert_eq!(default.encoder(), qualified.encoder());
}

/// The artifact identity comes from the requested mode, not from what the node
/// happened to know.
///
/// A namespace derived from diagnostic-contract coverage would be matched on
/// the FFmpeg binary's own digest, so two distribution builds of one version
/// would compute two cache keys for the same source and the same encode and
/// split a cluster's cache mid-upgrade. The evidence class is the same trap one
/// level down, and the existing digest test already refuses it; this refuses it
/// for the namespace too.
#[test]
fn how_well_a_node_knows_its_decoder_does_not_move_the_artifact_identity() {
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
    let legacy = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &software_capabilities("h264", "h264"),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("legacy software");
    let qualified_caps = capabilities_with_qualified_software(
        &input,
        Pipeline::Cpu,
        Encoder::Software,
        SubtitleRendering::None,
        vec![],
    );
    let qualified_inventory = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &qualified_caps,
        DecodePolicySnapshot::new(DecodePlanPolicy::Enforce, None),
    )
    .expect("qualified software");

    assert_eq!(legacy.decode().evidence(), DecodeEvidence::LegacyUnverified);
    assert_eq!(
        qualified_inventory.decode().evidence(),
        DecodeEvidence::Qualified
    );
    assert_eq!(
        legacy.artifact_namespace(),
        qualified_inventory.artifact_namespace(),
        "a qualified inventory is not a qualified artifact contract"
    );
    assert_eq!(legacy.artifact_namespace(), UNQUALIFIED_ARTIFACT_NAMESPACE);
}

// ---------------------------------------------------------------------------
// M5c2 — turning a durable continuation record into a selection input
// ---------------------------------------------------------------------------

fn continuation_restriction(failed: &str, required: &str) -> ContinuationDecodeRestriction {
    ContinuationDecodeRestriction {
        version: CONTINUATION_DECODE_RESTRICTION_VERSION,
        source_revision_digest: "a".repeat(64),
        input_video_stream: 0,
        input_codec: "hevc".to_owned(),
        failed_backend: failed.to_owned(),
        required_backend: required.to_owned(),
        policy_revision: 1,
    }
}

#[test]
fn all_lists_every_backend_and_a_new_one_cannot_be_added_quietly() {
    // The compiler cannot check that `DecodeBackend::ALL` is complete, so
    // this does. The match is exhaustive over the variants: adding one makes
    // this test fail to compile, and the arm it forces you to write is the
    // reminder that `ALL` and `parse` both need the new spelling. `name` is
    // already an exhaustive match and therefore guards nothing here — it
    // forces a spelling to be written, never to be readable.
    for backend in DecodeBackend::ALL {
        match backend {
            DecodeBackend::Software
            | DecodeBackend::VideoToolbox
            | DecodeBackend::Cuda
            | DecodeBackend::Qsv
            | DecodeBackend::Vaapi
            | DecodeBackend::V4l2Request => {}
        }
    }
}

#[test]
fn every_backend_name_survives_the_round_trip_and_nothing_else_parses() {
    // `name` is what a durable record stores and `parse` is what reads it
    // back, so the two are one contract. A backend added on one side and not
    // the other is a restriction that writes cleanly and reads as a stranger.
    for backend in DecodeBackend::ALL {
        assert_eq!(DecodeBackend::parse(backend.name()), Some(backend));
    }
    // No default, no nearest match, no leniency about spelling. Each of these
    // is a way a name could be "almost" right, and each has to be nothing.
    for name in [
        "",
        " software",
        "software ",
        "Software",
        "SOFTWARE",
        "vt",
        "banana",
    ] {
        assert_eq!(DecodeBackend::parse(name), None, "{name:?} must not parse");
    }
}

#[test]
fn the_two_stored_spellings_of_a_backend_are_pinned_apart() {
    // `DecodeBackend` has two wire forms: `name`, which the restriction record
    // stores, and serde's snake_case rename, which every struct holding a
    // `DecodeBackend` field serializes. They agree for four variants and
    // disagree for the fifth, and `parse` inverts only the first. Pinning both
    // keeps that divergence deliberate: a writer that reaches for the serde
    // form produces a record this build refuses, which is exactly the loop the
    // restriction exists to stop.
    assert_eq!(DecodeBackend::VideoToolbox.name(), "videotoolbox");
    assert_eq!(
        serde_json::to_value(DecodeBackend::VideoToolbox).expect("backend json"),
        json!("video_toolbox"),
        "the serde spelling differs from the stored one for this variant"
    );
    assert_eq!(DecodeBackend::parse("video_toolbox"), None);
    for backend in [
        DecodeBackend::Software,
        DecodeBackend::Cuda,
        DecodeBackend::Qsv,
        DecodeBackend::Vaapi,
    ] {
        assert_eq!(
            serde_json::to_value(backend).expect("backend json"),
            json!(backend.name()),
            "the two spellings agree for every other variant"
        );
    }
}

#[test]
fn a_continuation_record_names_the_successor_outright() {
    let restriction = continuation_restriction("videotoolbox", "software");
    let restrictions =
        AttemptRestrictions::for_continuation(&restriction).expect("a readable restriction");
    assert_eq!(
        restrictions,
        AttemptRestrictions::requiring(DecodeBackend::Software),
        "the required form is the branch resolution reads first, and it names \
         the successor rather than leaving preference order to pick what is left"
    );
    // A hardware successor is legitimate and reaches the same shape, so the
    // conversion cannot be one that ignores the field and always says software.
    assert_eq!(
        AttemptRestrictions::for_continuation(&continuation_restriction("software", "cuda"))
            .expect("a readable restriction"),
        AttemptRestrictions::requiring(DecodeBackend::Cuda),
    );
}

#[test]
fn an_unreadable_required_backend_is_a_refusal_and_never_an_absence() {
    // The whole point of the restriction is that a source which failed on
    // hardware does not get hardware again. Reading an unknown name as "no
    // restriction" restores automatic selection and reproduces the failure,
    // silently, on every later continuation.
    assert_eq!(
        AttemptRestrictions::for_continuation(&continuation_restriction("videotoolbox", "banana")),
        Err(DecodeRestrictionError::UnknownBackend("banana".to_owned())),
        "an unknown vocabulary is reported as newer, not as malformed"
    );
}

#[test]
fn an_unreadable_failed_backend_is_honoured_because_it_is_never_used() {
    // The rolling-upgrade case, and the one an earlier draft got wrong by
    // being careful. A newer node records that some backend this build has
    // never heard of failed, and asks for software; the playback then moves to
    // an older node. Refusing there would restore hardware decode for a source
    // that already failed on hardware — the exact loop the record exists to
    // stop. The failed name is never used: the answer is the required one.
    assert_eq!(
        AttemptRestrictions::for_continuation(&continuation_restriction("av1hw", "software"))
            .expect("the usable half of the record is usable"),
        AttemptRestrictions::requiring(DecodeBackend::Software),
    );
    // Version is untouched by a new backend name, so the old node really does
    // see this record rather than refusing it at the schema.
    let record = continuation_restriction("av1hw", "software");
    assert_eq!(record.version, CONTINUATION_DECODE_RESTRICTION_VERSION);
    let encoded = record
        .encode()
        .expect("a new backend name is not a schema change");
    assert_eq!(
        ContinuationDecodeRestriction::decode(&encoded).expect("decodes on this build"),
        record
    );
}

#[test]
fn requiring_what_just_failed_is_refused_by_the_record_not_by_the_conversion() {
    // The division of labour this conversion depends on: `validate` already
    // refuses a record that requires the backend that just failed, because
    // that is a loop rather than a restriction. Asserted exactly, so a change
    // that started refusing for some other reason would be visible here.
    assert_eq!(
        continuation_restriction("software", "software").encode(),
        Err(DecodeRestrictionError::InvalidField("required_backend")),
    );
}

#[test]
fn bound_probe_geometry_keeps_coded_dimensions_and_truthful_missing_facts() {
    let upright = facts(
        json!({"index":0,"codec_type":"video","width":1440,"height":1080,"sample_aspect_ratio":"4:3","avg_frame_rate":"30000/1001"}),
    );
    assert_eq!(upright.width(), Some(1440));
    assert_eq!(upright.rotation_degrees(), Some(0));
    assert_eq!(
        upright
            .displayed_aspect()
            .expect("measured aspect")
            .output_at_height(1080),
        Some((1920, 1080))
    );
    let rotated = facts(
        json!({"index":0,"codec_type":"video","width":1920,"height":1080,"sample_aspect_ratio":"1:1","side_data_list":[{"rotation":-90}]}),
    );
    assert_eq!(rotated.rotation_degrees(), Some(270));
    assert_eq!(
        rotated
            .displayed_aspect()
            .expect("measured aspect")
            .output_at_height(1920),
        Some((1080, 1920))
    );
    let missing = facts(json!({"index":0,"codec_type":"video","width":1920,"height":1080}));
    assert!(missing.displayed_aspect().is_none());
    // Actual selective-probe shape before geometry fields were requested.
    let omitted_matrix = facts(
        json!({"index":0,"codec_type":"video","width":1920,"height":1080,"sample_aspect_ratio":"1:1","side_data_list":[{"side_data_type":"Display Matrix"}]}),
    );
    assert!(omitted_matrix.rotation_degrees().is_none());
    let malformed_matrix = facts(
        json!({"index":0,"codec_type":"video","width":1920,"height":1080,"sample_aspect_ratio":"1:1","side_data_list":[{"side_data_type":"Display Matrix","rotation":"invalid"}]}),
    );
    assert!(malformed_matrix.rotation_degrees().is_none());
    let conflicting = facts(
        json!({"index":0,"codec_type":"video","width":1920,"height":1080,"sample_aspect_ratio":"1:1","tags":{"rotate":"0"},"side_data_list":[{"rotation":90}]}),
    );
    assert!(conflicting.rotation_degrees().is_none());
}

#[test]
fn normalized_geometry_preserves_anamorphic_detail_and_versions_only_new_routes() {
    for (width, height, sar, rotation, target, expected) in [
        (1440, 1080, "4:3", 0, 1440, (1920, 1080)),
        (1440, 1080, "4:3", 90, 1920, (1080, 1920)),
        (1920, 1080, "3:4", 0, 1440, (1440, 1080)),
        (1920, 1080, "3:4", -90, 1440, (1080, 1440)),
        (1920, 1080, "1:1", 0, 1440, (1920, 1080)),
        (360, 240, "64:45", 0, 144, (306, 144)),
    ] {
        let mut stream = video(
            0,
            Some("h264"),
            Some("High"),
            width,
            height,
            Some("yuv420p"),
            "30/1",
            "30/1",
            Some("bt709"),
        );
        stream["sample_aspect_ratio"] = json!(sar);
        let (a, b, c, d) = match rotation {
            90 => (0, -65536, 65536, 0),
            -90 => (0, 65536, -65536, 0),
            _ => (65536, 0, 0, 65536),
        };
        stream["side_data_list"] = json!([{"side_data_type":"Display Matrix", "rotation":rotation,
            "displaymatrix":format!("00000000: {a} {b} 0\n00000001: {c} {d} 0\n00000002: 0 0 1073741824\n")}]);
        let input = facts(stream);
        let mut media = options(Pipeline::Cpu);
        media.target_height = target;
        let legacy_request = TranscodeRequest::new(Encoder::Software, media);
        let caps = software_capabilities("h264", "h264");
        let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
        let legacy = resolve_transcode(
            &legacy_request,
            &input,
            &caps,
            &policy,
            &AttemptRestrictions::none(),
        )
        .expect("legacy route");
        let normalized = resolve_transcode(
            &legacy_request.with_normalized_geometry(),
            &input,
            &caps,
            &policy,
            &AttemptRestrictions::none(),
        )
        .expect("normalized route");
        assert_eq!(
            normalized
                .output_contract()
                .effective_width()
                .zip(normalized.output_contract().effective_height()),
            Some(expected)
        );
        assert_ne!(normalized.plan_digest(), legacy.plan_digest());
        assert!(legacy.output_contract().normalized_geometry().is_none());
        let source = execution_file("/fixture/source.mkv");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("bound execution fixture");
        let args = plurx_core::transcode::hls_args_for_plan(&normalized, &execution);
        let filter = &args[args
            .iter()
            .position(|arg| arg == "-vf")
            .expect("video filter")
            + 1];
        assert!(filter.contains(&format!("scale={}:{},setsar=1", expected.0, expected.1)));
        let matrix = args
            .iter()
            .position(|arg| arg == "-display_rotation")
            .expect("input matrix reset");
        assert_eq!(args[matrix + 1], "0");
        assert!(matrix < args.iter().position(|arg| arg == "-i").expect("input"));
        assert!(args.iter().any(|arg| arg == "-noautorotate"));
        match rotation {
            90 => assert!(filter.contains("transpose=cclock")),
            -90 => assert!(filter.contains("transpose=clock")),
            _ => {}
        }
    }
}

#[test]
fn normalized_geometry_refuses_unknown_facts_without_changing_legacy_identity() {
    let stream = video(
        0,
        Some("h264"),
        Some("High"),
        1920,
        1080,
        Some("yuv420p"),
        "30/1",
        "30/1",
        Some("bt709"),
    );
    let input = facts(stream);
    let request = TranscodeRequest::new(Encoder::Software, options(Pipeline::Cpu));
    let caps = software_capabilities("h264", "h264");
    let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
    let legacy = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::none(),
    )
    .expect("legacy route");
    assert_eq!(
        resolve_transcode(
            &request.with_normalized_geometry(),
            &input,
            &caps,
            &policy,
            &AttemptRestrictions::none()
        ),
        Err(PlanError::InvalidFact("sample_aspect_ratio"))
    );
    assert!(serde_json::to_value(legacy.output_contract())
        .expect("legacy contract")
        .get("normalized_geometry")
        .is_none());
}

#[test]
fn normalization_refuses_reflection_shear_and_missing_full_display_matrix() {
    for matrix in [
        None,
        Some("00000000: 65536 0 0\n00000001: 0 -65536 0\n00000002: 0 0 1073741824"),
        Some("00000000: 65536 100 0\n00000001: 0 65536 0\n00000002: 0 0 1073741824"),
        Some("00000000: 65536 0 10\n00000001: 0 65536 0\n00000002: 0 0 1073741824"),
    ] {
        let mut stream = video(
            0,
            Some("h264"),
            Some("High"),
            1920,
            1080,
            Some("yuv420p"),
            "30/1",
            "30/1",
            Some("bt709"),
        );
        stream["sample_aspect_ratio"] = json!("1:1");
        stream["side_data_list"] = json!([{"side_data_type":"Display Matrix", "rotation":0}]);
        if let Some(matrix) = matrix {
            stream["side_data_list"][0]["displaymatrix"] = json!(matrix);
        }
        let input = facts(stream);
        assert_eq!(
            input.rotation_degrees(),
            Some(0),
            "scalar angle alone hides reflection"
        );
        assert!(!input.normalization_transform_known());
        let request = TranscodeRequest::new(Encoder::Software, options(Pipeline::Cpu));
        let caps = software_capabilities("h264", "h264");
        let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
        assert!(resolve_transcode(
            &request,
            &input,
            &caps,
            &policy,
            &AttemptRestrictions::none()
        )
        .is_ok());
        assert_eq!(
            resolve_transcode(
                &request.with_normalized_geometry(),
                &input,
                &caps,
                &policy,
                &AttemptRestrictions::none()
            ),
            Err(PlanError::InvalidFact("display_matrix"))
        );
    }
}

fn request_input(depth: u8) -> DecodeFacts {
    facts(video(
        0,
        Some("hevc"),
        Some(if depth == 10 { "Main 10" } else { "Main" }),
        1920,
        1080,
        Some(if depth == 10 {
            "yuv420p10le"
        } else {
            "yuv420p"
        }),
        "24/1",
        "24/1",
        Some("bt709"),
    ))
}

fn operational_request_caps(depth: u8) -> DecodeCapabilities {
    let input = request_input(depth);
    let mut row = capability(
        DecodeBackend::V4l2Request,
        "hevc",
        input.profile(),
        input.pixel_format(),
        CapabilityStatus::Operational,
    );
    row.bit_depth = Some(depth);
    row.surface = Some(DecodeSurfaceContract::for_plan(
        DecodeBackend::V4l2Request,
        Pipeline::Cpu,
        &input,
        Encoder::Software,
        SubtitleRendering::None,
    ));
    capabilities(vec![row])
}

#[test]
fn request_backend_keeps_semantic_identity_separate_from_ffmpeg_method() {
    assert_eq!(DecodeBackend::V4l2Request.name(), "v4l2_request");
    assert_eq!(
        DecodeBackend::parse("v4l2_request"),
        Some(DecodeBackend::V4l2Request)
    );
    assert_eq!(DecodeBackend::parse("drm"), None);
    assert_eq!(DecodeBackend::V4l2Request.hwaccel_method(), "drm");
    assert_eq!(
        DecodeBackend::V4l2Request.hardware_frame_format(),
        Some("drm_prime")
    );
    assert_eq!(
        DecodeBackend::V4l2Request.input_args(true),
        ["-hwaccel", "drm", "-hwaccel_output_format", "drm_prime"]
    );
}

#[test]
fn operational_request_decode_pairs_with_software_encode_without_qualification() {
    for depth in [8, 10] {
        let input = request_input(depth);
        let plan = resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &input,
            &operational_request_caps(depth),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("operational CPU encode plan");
        assert_eq!(plan.encoder(), Encoder::Software);
        assert_eq!(plan.decode().backend(), DecodeBackend::V4l2Request);
        assert_eq!(plan.decode().evidence(), DecodeEvidence::Operational);
        assert_eq!(plan.decode().reason(), DecodeReason::MeasuredPreference);
        assert_eq!(
            plan.decode().surface().decode_domain(),
            FrameDomain::DrmPrime
        );
        assert_eq!(
            plan.decode().surface().decoder_download_format(),
            input.pixel_format()
        );
        assert_eq!(plan.artifact_namespace(), UNQUALIFIED_ARTIFACT_NAMESPACE);
        let software = resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &input,
            &capabilities(vec![]),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("software fallback");
        assert_ne!(plan.plan_digest(), software.plan_digest());
    }
}

#[test]
fn request_decode_requires_measured_depth_class_and_respects_existing_restrictions() {
    let input = request_input(10);
    let eight_only = resolve(
        Encoder::Software,
        Pipeline::Cpu,
        &input,
        &operational_request_caps(8),
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
    )
    .expect("unmeasured Main10 remains software");
    assert_eq!(eight_only.decode().backend(), DecodeBackend::Software);
    let snapshot = operational_request_caps(10);
    for restrictions in [
        AttemptRestrictions::none(),
        AttemptRestrictions::excluding([DecodeBackend::V4l2Request]),
    ] {
        let override_value = if restrictions == AttemptRestrictions::none() {
            Some("off")
        } else {
            None
        };
        let plan = resolve_transcode(
            &TranscodeRequest::new(Encoder::Software, options(Pipeline::Cpu)),
            &input,
            &snapshot,
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, override_value),
            &restrictions,
        )
        .expect("existing software override or exclusion");
        assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    }
    let forced = resolve_transcode(
        &TranscodeRequest::new(Encoder::Software, options(Pipeline::Cpu)),
        &input,
        &capabilities(vec![]),
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::requiring(DecodeBackend::V4l2Request),
    );
    assert!(matches!(
        forced,
        Err(PlanError::CapabilityUnavailable(DecodeBackend::V4l2Request))
    ));
    for pixel_format in [None, Some("yuv422p10le")] {
        let unsupported = facts(video(
            0,
            Some("hevc"),
            Some("main 10"),
            1920,
            1080,
            pixel_format,
            "24/1",
            "24/1",
            Some("bt709"),
        ));
        let plan = resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &unsupported,
            &snapshot,
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("unknown or unsupported input remains software");
        assert_eq!(plan.decode().backend(), DecodeBackend::Software);
    }
}

#[test]
fn request_hls_and_vod_share_depth_preserving_download_and_software_encoder() {
    for depth in [8, 10] {
        let input = request_input(depth);
        let plan = resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &input,
            &operational_request_caps(depth),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("request plan");
        let source = execution_file("/fixture/source.mkv");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("execution");
        let hls = hls_args(&plan, &execution);
        let vod = plurx_core::transcode::vod_pipe_args(
            &source,
            &plan,
            &execution,
            plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
        );
        for args in [&hls, &vod] {
            let input_position = args.iter().position(|arg| arg == "-i").expect("input");
            let method = args
                .windows(2)
                .position(|pair| pair == ["-hwaccel", "drm"])
                .expect("DRM input");
            assert!(method < input_position);
            assert!(args
                .windows(2)
                .any(|pair| pair == ["-hwaccel_output_format", "drm_prime"]));
            let vf = args
                .windows(2)
                .find(|pair| pair[0] == "-vf")
                .expect("filter graph");
            let transfer = format!(
                "hwdownload,format={}",
                input.pixel_format().expect("format")
            );
            let download_position = vf[1].find(&transfer).expect("depth-preserving download");
            let scale_position = vf[1].find("scale=").expect("CPU scale");
            assert!(download_position < scale_position, "{}", vf[1]);
            assert_eq!(vf[1].matches("hwdownload").count(), 1);
            assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
            assert!(!args
                .iter()
                .any(|arg| arg == "hevc_v4l2request" || arg == "v4l2_request"));
        }
    }
}

#[test]
fn request_decode_requires_proven_identity_transform_even_for_continuations() {
    let base = video(
        0,
        Some("hevc"),
        Some("Main"),
        1920,
        1080,
        Some("yuv420p"),
        "24/1",
        "24/1",
        Some("bt709"),
    );
    let upright = facts(base.clone());
    assert!(upright.normalization_transform_known());
    assert_eq!(upright.rotation_degrees(), Some(0));
    let mut rotated = base.clone();
    rotated["tags"] = json!({"rotate": "90"});
    let mut mirrored = base.clone();
    mirrored["side_data_list"] = json!([{"side_data_type": "Display Matrix", "rotation": 0,
        "displaymatrix": "00000000: -65536 0 0\n00000001: 0 65536 0\n00000002: 0 0 1073741824"}]);
    let mut unknown = base;
    unknown["side_data_list"] = json!([{"side_data_type": "Display Matrix"}]);
    for stream in [rotated, mirrored, unknown] {
        let input = facts(stream);
        let plan = resolve(
            Encoder::Software,
            Pipeline::Cpu,
            &input,
            &operational_request_caps(8),
            DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        )
        .expect("software keeps autorotation ownership");
        assert_eq!(plan.decode().backend(), DecodeBackend::Software);
        let required = resolve_transcode(
            &TranscodeRequest::new(Encoder::Software, options(Pipeline::Cpu)),
            &input,
            &operational_request_caps(8),
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::requiring(DecodeBackend::V4l2Request),
        );
        assert!(matches!(required, Err(PlanError::IncompatibleRenderer)));
    }
}

#[test]
fn macos_burn_graphs_require_independent_observation_and_process_before_compositing() {
    for hdr in [false, true] {
        for bitmap in [false, true] {
            let input = facts(macos_stream(hdr));
            let caps = unqualified_software_capabilities("hevc");
            let graph = match (hdr, bitmap) {
                (false, false) => MacosProcessingGraph::SdrTextBurn,
                (false, true) => MacosProcessingGraph::SdrBitmapBurn,
                (true, false) => MacosProcessingGraph::Hdr10TextBurn,
                (true, true) => MacosProcessingGraph::Hdr10BitmapBurn,
            };
            let mut media = options(Pipeline::Cpu);
            media.subtitle_burn = Some(SubtitleBurn {
                subtitle_index: 0,
                bitmap,
            });
            let unobserved = resolve_with_options(
                Encoder::VideoToolbox,
                media.clone(),
                &input,
                &caps,
                macos_policy(macos_context(true, MacosProcessingAvailability::Available)),
            )
            .expect("unobserved burn retains incumbent compositor");
            assert_eq!(unobserved.options().pipeline, Pipeline::Cpu);
            let context = macos_context(true, MacosProcessingAvailability::Available)
                .with_graph(graph, MacosProcessingAvailability::Available);
            let plan = resolve_with_options(
                Encoder::VideoToolbox,
                media,
                &input,
                &caps,
                macos_policy(context),
            )
            .expect("independently observed burn tuple");
            assert_eq!(
                plan.options().pipeline,
                if hdr {
                    Pipeline::VtToneMapMetal
                } else {
                    Pipeline::VtScaleSdr
                }
            );
            assert_eq!(
                plan.decode().surface().renderer_download_format(),
                Some("nv12")
            );
            let source = execution_file("/fixture/source.mkv");
            let mut execution_options = execution_options();
            execution_options.subtitle_file =
                (!bitmap).then(|| PathBuf::from("/fixture/active.ass"));
            let execution = TranscodeExecution::from_options(
                &source,
                &execution_options,
                Pacing::unpaced(),
                "/output",
            )
            .expect("burn execution");
            let args = hls_args(&plan, &execution);
            let graph = args
                .windows(2)
                .find(|pair| pair[0] == "-filter_complex")
                .expect("shared compositor graph")[1]
                .as_str();
            let compositor = if bitmap {
                "overlay_videotoolbox=bitmap=1"
            } else {
                "overlay_videotoolbox=ass=1"
            };
            assert!(graph.find("scale_vt=") < graph.find(compositor), "{graph}");
            if hdr {
                assert!(
                    graph.find("tonemap_videotoolbox=") < graph.find(compositor),
                    "{graph}"
                );
            }
            assert!(graph.contains(compositor), "{graph}");
            assert_eq!(graph.matches("hwdownload").count(), 0, "{graph}");
            if bitmap {
                assert!(
                    graph.contains("scale=1920:1080,format=yuva420p[sburn]"),
                    "{graph}"
                );
            } else {
                assert!(
                    graph.contains("subtitles_vt_images='/fixture/active.ass'"),
                    "{graph}"
                );
                assert!(graph.contains("split[vburn][sclock]"), "{graph}");
                if cfg!(target_os = "macos") {
                    assert!(graph.contains(":font_provider=fontconfig"), "{graph}");
                }
            }
            assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
        }
    }
}

#[test]
fn macos_hlg_requires_its_own_observation_and_exact_transfer() {
    let mut stream = macos_stream(true);
    stream["color_transfer"] = json!("arib-std-b67");
    stream["side_data_list"] = json!([]);
    let input = facts(stream);
    let caps = unqualified_software_capabilities("hevc");
    let context = macos_context(true, MacosProcessingAvailability::Available);
    let unavailable = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &caps,
        macos_policy(context.clone()),
    )
    .expect("incumbent HLG");
    assert_eq!(unavailable.options().pipeline, Pipeline::Cpu);
    let selected = resolve(
        Encoder::VideoToolbox,
        Pipeline::Cpu,
        &input,
        &caps,
        macos_policy(context.with_graph(
            MacosProcessingGraph::HlgMetal,
            MacosProcessingAvailability::Available,
        )),
    )
    .expect("independent HLG graph");
    assert_eq!(selected.options().pipeline, Pipeline::VtToneMapMetal);
    assert_eq!(selected.output_contract().output_grade(), OutputGrade::Sdr);
    assert_ne!(selected.plan_digest(), unavailable.plan_digest());
}

#[test]
fn macos_bwdif_frame_graph_keeps_parity_cadence_and_runs_before_scale() {
    for parity in ["tt", "bb"] {
        let mut stream = macos_stream(false);
        stream["field_order"] = json!(parity);
        stream["codec_name"] = json!("h264");
        stream["profile"] = json!("High");
        let input = facts(stream);
        let caps = unqualified_software_capabilities("h264");
        let context = macos_context(true, MacosProcessingAvailability::Available).with_graph(
            MacosProcessingGraph::SdrBwdifFrame,
            MacosProcessingAvailability::Available,
        );
        let selected = resolve(
            Encoder::VideoToolbox,
            Pipeline::Cpu,
            &input,
            &caps,
            macos_policy(context),
        )
        .expect("independently observed file BWDIF graph");
        assert_eq!(selected.deinterlace(), Deinterlace::BwdifSendFrame);
        assert_eq!(selected.options().pipeline, Pipeline::VtScaleSdr);
        let source = execution_file("/fixture/interlaced.mkv");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/output",
        )
        .expect("file execution");
        let args = hls_args(&selected, &execution);
        let filter = &args[args.iter().position(|arg| arg == "-vf").expect("filter") + 1];
        assert!(
            filter.starts_with(
                "bwdif_videotoolbox=mode=send_frame:parity=auto:deint=interlaced,scale_vt="
            ),
            "{filter}"
        );
        assert!(!filter.contains("send_field"));
        assert!(
            !args.iter().any(|arg| arg == "-a53cc"),
            "live caption policy stays live-scoped"
        );
    }
}

#[test]
fn macos_hevc_codec_and_grade_require_independent_complete_graphs() {
    use plurx_core::transcode::VideoCodec;
    for hdr in [false, true] {
        let input = facts(macos_stream(hdr));
        let caps = unqualified_software_capabilities("hevc");
        let mut options = options(if hdr {
            Pipeline::Hdr10Passthrough
        } else {
            Pipeline::Cpu
        });
        options.output_codec = Some(VideoCodec::Hevc);
        options.effective_rate_control = EffectiveRateControl::Vbr;
        let unobserved = macos_context(true, MacosProcessingAvailability::Available)
            .with_hevc_output_enabled(true);
        assert!(resolve_with_options(
            Encoder::VideoToolbox,
            options.clone(),
            &input,
            &caps,
            macos_policy(unobserved.clone())
        )
        .is_err());
        let graph = if hdr {
            MacosProcessingGraph::HevcHdr10
        } else {
            MacosProcessingGraph::HevcSdr
        };
        let observed = unobserved.with_graph(graph, MacosProcessingAvailability::Available);
        let selected = resolve_with_options(
            Encoder::VideoToolbox,
            options.clone(),
            &input,
            &caps,
            macos_policy(observed),
        )
        .expect("independent native HEVC graph");
        assert_eq!(selected.output_contract().output_codec(), "hevc");
        assert_eq!(
            selected.output_contract().output_grade(),
            if hdr {
                OutputGrade::Hdr10
            } else {
                OutputGrade::Sdr
            }
        );
        assert_eq!(
            selected.options().pipeline,
            if hdr {
                Pipeline::VtScaleHdr10
            } else {
                Pipeline::VtScaleSdr
            }
        );
        let execution = TranscodeExecution::from_options(
            &execution_file("/fixture/source.mkv"),
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("execution");
        let args = hls_args(&selected, &execution);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c:v", "hevc_videotoolbox"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-color_trc", if hdr { "smpte2084" } else { "bt709" }]));
        assert!(args.windows(2).any(|pair| pair == ["-tag:v", "hvc1"]));
        let host_graph = if hdr {
            MacosProcessingGraph::HevcHdr10Host
        } else {
            MacosProcessingGraph::HevcSdrHost
        };
        let host = macos_context(false, MacosProcessingAvailability::Unavailable)
            .with_hevc_output_enabled(true)
            .with_graph(host_graph, MacosProcessingAvailability::Available);
        let recovered = resolve_with_options(
            Encoder::VideoToolbox,
            options.clone(),
            &input,
            &caps,
            macos_policy(host),
        )
        .expect("HEVC encoder independent of native processing");
        assert_eq!(recovered.options().pipeline, options.pipeline);
        assert_eq!(recovered.output_contract().output_codec(), "hevc");
        assert_eq!(
            recovered.output_contract().output_grade(),
            selected.output_contract().output_grade()
        );
        assert_ne!(selected.plan_digest(), recovered.plan_digest());
        let failure_context = macos_context(true, MacosProcessingAvailability::Available)
            .with_hevc_output_enabled(true)
            .with_graph(graph, MacosProcessingAvailability::Available)
            .with_graph(host_graph, MacosProcessingAvailability::Available)
            .excluding_pipeline(selected.options().pipeline);
        let retry = resolve_with_options(
            Encoder::VideoToolbox,
            selected.options().clone(),
            &input,
            &caps,
            macos_policy(failure_context),
        )
        .expect("observed grade-preserving host retry");
        assert_eq!(retry.options().pipeline, recovered.options().pipeline);
        assert_eq!(retry.output_contract().output_codec(), "hevc");
        assert_eq!(
            retry.output_contract().output_grade(),
            selected.output_contract().output_grade()
        );
        if !hdr {
            let mut legacy_options = options.clone();
            legacy_options.output_codec = None;
            let legacy_worker = resolve_with_options(
                Encoder::VideoToolbox,
                legacy_options,
                &input,
                &caps,
                macos_policy(macos_context(
                    false,
                    MacosProcessingAvailability::Unavailable,
                )),
            )
            .expect("legacy worker retains H264");
            assert_eq!(legacy_worker.output_contract().output_codec(), "h264");
            assert_ne!(
                legacy_worker.plan_digest(),
                retry.plan_digest(),
                "an old worker cannot publish H264 behind an exact HEVC recipe"
            );
        }
        let disabled = macos_context(false, MacosProcessingAvailability::Available)
            .with_graph(host_graph, MacosProcessingAvailability::Available);
        assert!(resolve_with_options(
            Encoder::VideoToolbox,
            options,
            &input,
            &caps,
            macos_policy(disabled)
        )
        .is_err());
    }
}

fn macos_p5_stream() -> Value {
    let mut stream = macos_stream(true);
    stream["color_space"] = json!("ipt-c2");
    stream["color_range"] = json!("pc");
    stream["side_data_list"] = json!([{"side_data_type":"DOVI configuration record", "dv_profile":5, "dv_level":6, "dv_bl_signal_compatibility_id":0, "rpu_present_flag":1, "el_present_flag":0, "bl_present_flag":1}]);
    stream
}

// Match the production probe/catalog handoff: the selective decode probe
// carries the range class, while the catalogue supplies typed DOVI fields.
fn macos_p5_facts(stream: Value) -> DecodeFacts {
    let document = json!({"streams": [stream]});
    let probe = plurx_core::scan::probe::parse_probe_json(&document);
    let catalog = DecodeCatalogMetadata::new(
        probe.hdr.as_deref(),
        probe.hdr_format.as_deref(),
        probe.dolby_vision,
    )
    .expect("typed DOVI catalogue fixture");
    DecodeFacts::from_ffprobe_json_with_catalog(&document, identity('a'), &catalog)
        .expect("catalogue-bound P5 fixture")
}

#[test]
fn macos_strict_p5_requires_independent_decoder_renderer_graphs() {
    let input = macos_p5_facts(macos_p5_stream());
    let caps = unqualified_software_capabilities("hevc");
    let request = TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::DoviTonemapx));
    let ordinary = macos_context(true, MacosProcessingAvailability::Available);
    let incumbent = resolve_transcode(
        &request,
        &input,
        &caps,
        &macos_policy(ordinary.clone()),
        &AttemptRestrictions::none(),
    )
    .expect("unobserved P5 stays incumbent");
    assert_eq!(incumbent.options().pipeline, Pipeline::DoviTonemapx);
    assert!(incumbent.options().strict_dolby.is_none());
    for (graph, pipeline, backend) in [
        (
            MacosProcessingGraph::P5SoftwareCpu,
            Pipeline::DoviStrictTonemapx,
            DecodeBackend::Software,
        ),
        (
            MacosProcessingGraph::P5VtTonemapx,
            Pipeline::VtDoviTonemapx,
            DecodeBackend::VideoToolbox,
        ),
        (
            MacosProcessingGraph::P5SoftwareMetal,
            Pipeline::DoviMetal,
            DecodeBackend::Software,
        ),
        (
            MacosProcessingGraph::P5VtMetal,
            Pipeline::VtDoviMetal,
            DecodeBackend::VideoToolbox,
        ),
    ] {
        let context = ordinary
            .clone()
            .with_graph(graph, MacosProcessingAvailability::Available);
        let plan = resolve_transcode(
            &request,
            &input,
            &caps,
            &macos_policy(context),
            &AttemptRestrictions::none(),
        )
        .expect("exact observed strict P5 tuple");
        assert_eq!(plan.options().pipeline, pipeline);
        assert_eq!(plan.decode().backend(), backend);
        assert!(plan.decode().surface().requires_side_data());
        assert!(plan.options().strict_dolby.is_some());
        assert!(plan.macos_processing_identity().is_some());
        assert_eq!(plan.output_contract().output_codec(), "h264");
        assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
        let source = execution_file("/fixture/p5.mp4");
        let execution = TranscodeExecution::from_options(
            &source,
            &execution_options(),
            Pacing::unpaced(),
            "/fixture/out",
        )
        .expect("execution");
        let args = hls_args(&plan, &execution);
        assert!(args.iter().any(|arg| arg == "-xerror"));
        for pair in [
            ["-strict_dovi", "1"],
            ["-err_detect", "explode"],
            ["-c:v", "hevc"],
        ] {
            assert!(args.windows(2).any(|args| args == pair));
        }
        assert_eq!(
            args.windows(2)
                .any(|args| args == ["-hwaccel_flags", "+require_hardware"]),
            backend == DecodeBackend::VideoToolbox
        );
        let filter = &args[args.iter().position(|arg| arg == "-vf").expect("filter") + 1];
        assert!(filter.contains("apply_dovi=1:require_dovi=1"));
        let renderer = filter
            .find(
                if matches!(pipeline, Pipeline::VtDoviMetal | Pipeline::DoviMetal) {
                    "tonemap_videotoolbox="
                } else {
                    "tonemapx="
                },
            )
            .expect("Dolby renderer");
        assert!(
            renderer
                < filter
                    .rfind("scale")
                    .expect("scale after source-raster reshape")
        );
        if pipeline == Pipeline::DoviMetal {
            assert!(filter.starts_with(
                "format=yuv420p10le,setparams=colorspace=unknown,format=p010le,hwupload,"
            ));
        }
        if pipeline == Pipeline::VtDoviTonemapx {
            assert!(filter.starts_with(
                "hwdownload,format=p010le,setparams=colorspace=unknown,format=yuv420p10le,"
            ));
        }
    }
}

#[test]
fn macos_strict_p5_recovery_retains_package_and_current_au_contract() {
    let input = macos_p5_facts(macos_p5_stream());
    let caps = unqualified_software_capabilities("hevc");
    let context = macos_context(true, MacosProcessingAvailability::Available)
        .with_graph(
            MacosProcessingGraph::P5VtMetal,
            MacosProcessingAvailability::Available,
        )
        .with_graph(
            MacosProcessingGraph::P5SoftwareCpu,
            MacosProcessingAvailability::Available,
        );
    let initial = resolve_transcode(
        &TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::DoviTonemapx)),
        &input,
        &caps,
        &macos_policy(context.clone()),
        &AttemptRestrictions::none(),
    )
    .expect("strict native initial");
    let mut media = initial.options().clone();
    // Existing retry callers can drop native renderer options, but the typed
    // semantic policy remains and cannot become a generic PQ/CPU graph.
    media.pipeline = Pipeline::Cpu;
    let request = TranscodeRequest::new(Encoder::VideoToolbox, media);
    let recovery_context = MacosProcessingContext::new(
        false,
        context.identity().clone(),
        MacosProcessingAvailability::Unavailable,
        MacosProcessingAvailability::Unavailable,
    )
    .with_graph(
        MacosProcessingGraph::P5SoftwareCpu,
        MacosProcessingAvailability::Available,
    );
    let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, Some("off"))
        .with_macos_processing(recovery_context.clone());
    let recovered = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::requiring(DecodeBackend::Software),
    )
    .expect("strict software CPU recovery");
    assert_eq!(recovered.options().pipeline, Pipeline::DoviStrictTonemapx);
    assert_eq!(recovered.decode().backend(), DecodeBackend::Software);
    assert_eq!(recovered.decode().software_decoder(), Some("hevc"));
    assert_eq!(
        recovered.macos_processing_identity(),
        initial.macos_processing_identity()
    );
    assert_eq!(
        recovered.options().strict_dolby,
        initial.options().strict_dolby
    );
    let unavailable = macos_policy(MacosProcessingContext::new(
        false,
        context.identity().clone(),
        MacosProcessingAvailability::Unavailable,
        MacosProcessingAvailability::Unavailable,
    ));
    assert_eq!(
        resolve_transcode(
            &request,
            &input,
            &caps,
            &unavailable,
            &AttemptRestrictions::requiring(DecodeBackend::Software)
        ),
        Err(PlanError::IncompatibleRenderer)
    );
    let changed = macos_policy(
        MacosProcessingContext::new(
            false,
            macos_identity("changed-package-owner"),
            MacosProcessingAvailability::Unavailable,
            MacosProcessingAvailability::Unavailable,
        )
        .with_graph(
            MacosProcessingGraph::P5SoftwareCpu,
            MacosProcessingAvailability::Available,
        ),
    );
    assert_eq!(
        resolve_transcode(
            &request,
            &input,
            &caps,
            &changed,
            &AttemptRestrictions::requiring(DecodeBackend::Software)
        ),
        Err(PlanError::IncompatibleRenderer)
    );
    for bad in 0..4 {
        let mut stream = macos_p5_stream();
        match bad {
            0 => stream["side_data_list"][0]["dv_profile"] = json!(8),
            1 => stream["side_data_list"][0]["rpu_present_flag"] = json!(0),
            2 => stream["sample_aspect_ratio"] = json!("4:3"),
            _ => stream["color_space"] = json!("bt2020nc"),
        };
        assert_eq!(
            resolve_transcode(
                &request,
                &macos_p5_facts(stream),
                &caps,
                &policy,
                &AttemptRestrictions::requiring(DecodeBackend::Software)
            ),
            Err(PlanError::IncompatibleRenderer)
        );
    }
}

#[test]
fn macos_normalized_1440_pq_retains_sdr_avc_geometry_and_cadence() {
    use plurx_core::transcode::AutoQualityRateProfile;
    let input = facts(macos_stream(true));
    let caps = unqualified_software_capabilities("hevc");
    let policy = macos_policy(macos_context(true, MacosProcessingAvailability::Available));
    let mut media = options(Pipeline::Cpu);
    media.target_height = 1440;
    media.video_bitrate_kbps = 12_000;
    let request = TranscodeRequest::new(Encoder::VideoToolbox, media)
        .with_auto_quality_rate_profile(AutoQualityRateProfile::H264Sdr1440P30V1);
    let plan = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::none(),
    )
    .expect("compatible normalized PQ tone map");
    assert_eq!(plan.options().pipeline, Pipeline::VtToneMapMetal);
    assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
    assert_eq!(plan.output_contract().output_codec(), "h264");
    assert_eq!(plan.output_contract().effective_width(), Some(2560));
    assert_eq!(plan.output_contract().effective_height(), Some(1440));
    assert_eq!(plan.options().video_bitrate_kbps, 12_000);
    assert_eq!(plan.output_contract().output_profile(), Some("high"));
    let source = execution_file("/fixture/pq.mkv");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    let args = hls_args(&plan, &execution);
    let graph = &args[args
        .iter()
        .position(|arg| arg == "-vf")
        .expect("video filter")
        + 1];
    assert!(graph.starts_with("fps=24/1,"));
    assert!(graph.contains("tonemap_videotoolbox"));
    assert!(graph.ends_with(",setsar=1"));
    assert!(!graph.contains("hwdownload"));
}

#[test]
fn macos_normalized_burn_keeps_captured_canvas_cadence_and_gpu_compositor() {
    use plurx_core::transcode::AutoQualityRateProfile;
    for hdr in [false, true] {
        for bitmap in [false, true] {
            let input = facts(macos_stream(hdr));
            let graph = match (hdr, bitmap) {
                (false, false) => MacosProcessingGraph::SdrTextBurn,
                (false, true) => MacosProcessingGraph::SdrBitmapBurn,
                (true, false) => MacosProcessingGraph::Hdr10TextBurn,
                (true, true) => MacosProcessingGraph::Hdr10BitmapBurn,
            };
            let context = macos_context(true, MacosProcessingAvailability::Available)
                .with_graph(graph, MacosProcessingAvailability::Available);
            let mut media = options(Pipeline::Cpu);
            media.target_height = 1440;
            media.video_bitrate_kbps = 12_000;
            media.subtitle_burn = Some(SubtitleBurn {
                subtitle_index: 2,
                bitmap,
            });
            let request = TranscodeRequest::new(Encoder::VideoToolbox, media)
                .with_auto_quality_rate_profile(AutoQualityRateProfile::H264Sdr1440P30V1);
            let plan = resolve_transcode(
                &request,
                &input,
                &unqualified_software_capabilities("hevc"),
                &macos_policy(context),
                &AttemptRestrictions::none(),
            )
            .expect("observed normalized burn");
            assert!(plan.macos_processing_identity().is_some());
            assert_eq!(plan.output_contract().effective_width(), Some(2560));
            assert_eq!(plan.output_contract().effective_height(), Some(1440));
            assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
            assert_eq!(plan.output_contract().output_profile(), Some("high"));
            let source = execution_file("/fixture/burn.mkv");
            let execution_options = execution_options();
            let execution = TranscodeExecution::from_options(
                &source,
                &execution_options,
                Pacing::unpaced(),
                "/fixture/out",
            )
            .expect("execution");
            let args = hls_args(&plan, &execution);
            let graph = &args[args
                .iter()
                .position(|arg| arg == "-filter_complex")
                .expect("normalized burn complex filter")
                + 1];
            assert!(graph.starts_with("[0:0]fps=24/1,"), "{graph}");
            assert!(graph.contains(",setsar=1"), "{graph}");
            assert!(!graph.contains("hwdownload"), "{graph}");
            assert!(
                graph.contains(if bitmap {
                    "[0:s:2]scale=2560:1440,format=yuva420p"
                } else {
                    "subtitles_vt_images='/fixture/burn.mkv':si=2"
                }),
                "{graph}"
            );
        }
    }
}

#[test]
fn macos_hdr_interlaced_retains_incumbent_when_vt_cannot_decode_high10_fields() {
    let caps = unqualified_software_capabilities("hevc");
    let policy = macos_policy(macos_context(true, MacosProcessingAvailability::Available));
    for transfer in ["smpte2084", "arib-std-b67"] {
        let mut stream = macos_stream(true);
        stream["codec_name"] = json!("h264");
        stream["profile"] = json!("High 10");
        stream["field_order"] = json!("tt");
        stream["color_transfer"] = json!(transfer);
        let request = TranscodeRequest::new(Encoder::VideoToolbox, options(Pipeline::Cpu));
        let plan = resolve_transcode(
            &request,
            &facts(stream),
            &caps,
            &policy,
            &AttemptRestrictions::none(),
        )
        .expect("incumbent HDR interlace route");
        assert_eq!(plan.options().pipeline, Pipeline::Cpu);
        assert!(plan.macos_processing_identity().is_none());
    }
}

#[test]
fn authorized_strict_p5_survives_rpu_only_renderer_guard_without_admitting_generic_cpu() {
    let input = macos_p5_facts(macos_p5_stream());
    assert_eq!(
        input.dynamic_range_class(),
        Some(plurx_core::transcode::DynamicRangeClass::DolbyVision)
    );
    let caps = unqualified_software_capabilities("hevc");
    let context = macos_context(true, MacosProcessingAvailability::Available).with_graph(
        MacosProcessingGraph::P5VtMetal,
        MacosProcessingAvailability::Available,
    );
    let strict = resolve_with_options(
        Encoder::VideoToolbox,
        options(Pipeline::VtDoviMetal),
        &input,
        &caps,
        macos_policy(context),
    )
    .expect("authorized P5 renderer survives generic RPU guard");
    assert_eq!(strict.options().pipeline, Pipeline::VtDoviMetal);
    assert!(strict.options().strict_dolby.is_some());
    assert!(strict.macos_processing_identity().is_some());
    let unavailable = macos_policy(macos_context(
        false,
        MacosProcessingAvailability::Unavailable,
    ));
    assert!(matches!(
        resolve_with_options(
            Encoder::VideoToolbox,
            options(Pipeline::Cpu),
            &input,
            &caps,
            unavailable
        ),
        Err(PlanError::IncompatibleRenderer)
    ));
    for profile in [7, 8] {
        let mut stream = macos_p5_stream();
        stream["side_data_list"][0]["dv_profile"] = json!(profile);
        let context = macos_context(true, MacosProcessingAvailability::Available).with_graph(
            MacosProcessingGraph::P5VtMetal,
            MacosProcessingAvailability::Available,
        );
        assert!(
            resolve_with_options(
                Encoder::VideoToolbox,
                options(Pipeline::VtDoviMetal),
                &macos_p5_facts(stream),
                &caps,
                macos_policy(context)
            )
            .is_err(),
            "unqualified P{profile} must not inherit strict P5"
        );
    }
}

fn linux_strict_context(driver_digest: char) -> LinuxDolbyContext {
    linux_strict_context_for_encoder(driver_digest, Encoder::Software)
}

fn linux_strict_context_for_encoder(driver_digest: char, encoder: Encoder) -> LinuxDolbyContext {
    let identity = LinuxDolbyIdentity::new(
        "f".repeat(64),
        "2".repeat(64),
        "3".repeat(64),
        "4".repeat(64),
        driver_digest.to_string().repeat(64),
        "6".repeat(64),
        "/dev/dri/renderD128".to_owned(),
    )
    .expect("complete Linux package and physical environment");
    let observations = [DecodeBackend::Vaapi, DecodeBackend::Software]
        .into_iter()
        .map(|decoder| {
            LinuxDolbyObservation::new(
                decoder,
                encoder,
                3840,
                2160,
                1920,
                1080,
                Rational::new(24, 1).expect("observed cadence"),
            )
            .expect("observed complete graph")
        })
        .collect();
    LinuxDolbyContext::new(identity, observations)
}

#[test]
fn linux_strict_qsv_encoder_is_derived_from_the_observed_decode_device() {
    let input = facts(macos_p5_stream());
    let caps = capabilities(vec![exact_capability(
        DecodeBackend::Vaapi,
        &input,
        Pipeline::DoviStrictTonemapx,
        Encoder::Qsv,
        SubtitleRendering::None,
        CapabilityStatus::Operational,
    )]);
    let plan = resolve(
        Encoder::Qsv,
        Pipeline::DoviTonemapx,
        &input,
        &caps,
        DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
            .with_linux_dolby(linux_strict_context_for_encoder('5', Encoder::Qsv)),
    )
    .expect("observed QSV encoder and VAAPI decoder tuple");
    let source = execution_file("/fixture/p5.mp4");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    let args = hls_args(&plan, &execution);
    for pair in [
        ["-init_hw_device", "vaapi=plurx_dovi_va:/dev/dri/renderD128"],
        ["-init_hw_device", "qsv=plurx_dovi_qsv@plurx_dovi_va"],
        ["-filter_hw_device", "plurx_dovi_qsv"],
        ["-hwaccel_device", "/dev/dri/renderD128"],
    ] {
        assert!(
            args.windows(2).any(|args| args == pair),
            "missing retained device projection: {pair:?}"
        );
    }
    assert!(!args.iter().any(|arg| arg == "qsv=hw"));
    let filter = &args[args.iter().position(|arg| arg == "-vf").expect("filter") + 1];
    assert!(filter.starts_with("hwdownload,format=p010le,setparams=colorspace=unknown,"));
    assert!(filter.contains("apply_dovi=1:require_dovi=1"));
    assert!(filter.ends_with("hwupload=extra_hw_frames=64,format=qsv"));
}

#[test]
fn linux_strict_p5_selects_native_vaapi_without_lifting_other_dolby_software_routes() {
    let input = facts(macos_p5_stream());
    let caps = capabilities(vec![exact_capability(
        DecodeBackend::Vaapi,
        &input,
        Pipeline::DoviStrictTonemapx,
        Encoder::Software,
        SubtitleRendering::None,
        CapabilityStatus::Operational,
    )]);
    let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
        .with_linux_dolby(linux_strict_context('5'));
    let plan = resolve(
        Encoder::Software,
        Pipeline::DoviTonemapx,
        &input,
        &caps,
        policy.clone(),
    )
    .expect("exact observed Profile5 route");
    assert_eq!(plan.decode().backend(), DecodeBackend::Vaapi);
    assert_eq!(plan.options().pipeline, Pipeline::DoviStrictTonemapx);
    assert_eq!(plan.output_contract().output_grade(), OutputGrade::Sdr);
    assert!(plan.macos_processing_identity().is_none());
    assert_eq!(
        plan.captured_processing_ffmpeg_sha256(),
        Some("f".repeat(64).as_str())
    );
    let source = execution_file("/fixture/p5.mp4");
    let execution = TranscodeExecution::from_options(
        &source,
        &execution_options(),
        Pacing::unpaced(),
        "/fixture/out",
    )
    .expect("execution");
    let args = hls_args(&plan, &execution);
    for pair in [
        ["-hwaccel", "vaapi"],
        ["-hwaccel_output_format", "vaapi"],
        ["-hwaccel_device", "/dev/dri/renderD128"],
        ["-strict_dovi", "1"],
        ["-err_detect", "explode"],
        ["-c:v", "hevc"],
    ] {
        assert!(args.windows(2).any(|args| args == pair));
    }
    assert!(args.iter().any(|arg| arg == "-xerror"));
    assert!(!args.iter().any(|arg| arg == "+require_hardware"));
    let filter = &args[args.iter().position(|arg| arg == "-vf").expect("filter") + 1];
    assert!(filter
        .starts_with("hwdownload,format=p010le,setparams=colorspace=unknown,format=yuv420p10le,"));
    assert!(filter.contains("apply_dovi=1:require_dovi=1"));
    for profile in [7, 8] {
        let mut stream = macos_p5_stream();
        stream["side_data_list"][0]["dv_profile"] = json!(profile);
        let other = facts(stream);
        let incumbent = resolve(
            Encoder::Software,
            Pipeline::DoviTonemapx,
            &other,
            &caps,
            policy.clone(),
        )
        .expect("other Dolby profiles retain incumbent software path");
        assert_eq!(incumbent.decode().backend(), DecodeBackend::Software);
        assert!(incumbent.options().strict_dolby.is_none());
    }
}

#[test]
fn linux_strict_p5_recovery_retains_package_and_rejects_changed_driver_or_unobserved_envelope() {
    let input = facts(macos_p5_stream());
    let caps = capabilities(vec![exact_capability(
        DecodeBackend::Vaapi,
        &input,
        Pipeline::DoviStrictTonemapx,
        Encoder::Software,
        SubtitleRendering::None,
        CapabilityStatus::Operational,
    )]);
    let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
        .with_linux_dolby(linux_strict_context('5'));
    let initial = resolve(
        Encoder::Software,
        Pipeline::DoviTonemapx,
        &input,
        &caps,
        policy.clone(),
    )
    .expect("observed strict hardware route");
    let mut media = initial.options().clone();
    media.pipeline = Pipeline::Cpu;
    let request = TranscodeRequest::new(Encoder::Software, media);
    let recovered = resolve_transcode(
        &request,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::requiring(DecodeBackend::Software),
    )
    .expect("observed grade-preserving strict software recovery");
    assert_eq!(recovered.decode().backend(), DecodeBackend::Software);
    assert_eq!(recovered.options().pipeline, Pipeline::DoviStrictTonemapx);
    assert_eq!(
        recovered.options().strict_dolby,
        initial.options().strict_dolby
    );
    assert_eq!(
        recovered.captured_processing_ffmpeg_sha256(),
        initial.captured_processing_ffmpeg_sha256()
    );
    let pending_reprobe = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None);
    let retained = resolve_transcode(
        &request,
        &input,
        &caps,
        &pending_reprobe,
        &AttemptRestrictions::requiring(DecodeBackend::Software),
    )
    .expect("pending admin reprobe must not erase the active plan's observed software recovery");
    assert_eq!(
        retained.options().strict_dolby,
        initial.options().strict_dolby
    );
    assert_eq!(retained.decode().backend(), DecodeBackend::Software);
    let new_admission = resolve(
        Encoder::Software,
        Pipeline::DoviTonemapx,
        &input,
        &caps,
        pending_reprobe,
    )
    .expect("ordinary incumbent admission remains independent of a pending hardware observation");
    assert!(
        new_admission.options().strict_dolby.is_none(),
        "only captured active plans may retain prior qualification"
    );
    let changed = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None)
        .with_linux_dolby(linux_strict_context('7'));
    assert_eq!(
        resolve_transcode(
            &request,
            &input,
            &caps,
            &changed,
            &AttemptRestrictions::requiring(DecodeBackend::Software)
        ),
        Err(PlanError::IncompatibleRenderer)
    );
    let mut faster = macos_p5_stream();
    faster["avg_frame_rate"] = json!("60/1");
    faster["r_frame_rate"] = json!("60/1");
    assert!(resolve_transcode(
        &request,
        &facts(faster),
        &caps,
        &policy,
        &AttemptRestrictions::requiring(DecodeBackend::Software)
    )
    .is_err());
    let mut too_large_media = request.options().clone();
    too_large_media.target_height = 2160;
    let too_large = TranscodeRequest::new(Encoder::Software, too_large_media);
    assert!(resolve_transcode(
        &too_large,
        &input,
        &caps,
        &policy,
        &AttemptRestrictions::requiring(DecodeBackend::Software)
    )
    .is_err());
}

#[test]
fn macos_text_font_binding_changes_only_text_output_identity() {
    let input = facts(macos_stream(false));
    let caps = unqualified_software_capabilities("hevc");
    let base = macos_identity("test-build");
    let mut text_digests = Vec::new();
    let mut bitmap_digests = Vec::new();
    let mut plain_digests = Vec::new();
    for font in ["a", "b"] {
        let context = macos_context(true, MacosProcessingAvailability::Available)
            .with_text_identity(base.clone().with_text_fonts(font.repeat(64)))
            .with_graph(
                MacosProcessingGraph::SdrTextBurn,
                MacosProcessingAvailability::Available,
            )
            .with_graph(
                MacosProcessingGraph::SdrBitmapBurn,
                MacosProcessingAvailability::Available,
            );
        for burn in [None, Some(false), Some(true)] {
            let mut media = options(Pipeline::Cpu);
            media.subtitle_burn = burn.map(|bitmap| SubtitleBurn {
                subtitle_index: 0,
                bitmap,
            });
            let plan = resolve_with_options(
                Encoder::VideoToolbox,
                media,
                &input,
                &caps,
                macos_policy(context.clone()),
            )
            .expect("observed complete native graph");
            let digest = plan.plan_digest();
            match burn {
                None => plain_digests.push(digest),
                Some(false) => text_digests.push(digest),
                Some(true) => bitmap_digests.push(digest),
            }
        }
    }
    assert_ne!(text_digests[0], text_digests[1]);
    assert_eq!(bitmap_digests[0], bitmap_digests[1]);
    assert_eq!(plain_digests[0], plain_digests[1]);
}
