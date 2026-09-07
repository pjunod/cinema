use plurx_core::domain::{AudioStream, DolbyVisionFacts, MediaFile};
use plurx_core::transcode::{
    hls_args, resolve_transcode, AttemptRestrictions, CapabilityStatus, DecodeBackend,
    DecodeCacheIdentity, DecodeCapabilities, DecodeCapability, DecodeCapabilitySnapshotIdentity,
    DecodeCatalogMetadata, DecodeEvidence, DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot,
    DecodeReason, DecodeSourceIdentity, DecodeSurfaceContract, EffectiveRateControl, Encoder,
    FrameDomain, FrameRateProvenance, OutputGrade, Pacing, Pipeline, PipelineDigest, PlanError,
    PlanSourceBinding, Recipe, SoftwareDecoder, StreamSelectionProvenance, SubtitleBurn,
    SubtitleRendering, ToneMap, TranscodeExecution, TranscodeMediaOptions, TranscodeOptions,
    TranscodeRequest,
};
use serde_json::{json, Value};
use std::path::PathBuf;

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
        target_height: 1080,
        video_bitrate_kbps: 8_000,
        effective_rate_control: EffectiveRateControl::Vbr,
        audio_channels: 2,
        audio_bitrate_kbps: 160,
        audio_index: None,
        audio_offset_ms: 0,
        input_has_audio: true,
        tone_map: ToneMap::Zscale,
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
        id: 7,
        item_id: 11,
        path: PathBuf::from(path),
        size: 12_345,
        mtime: 67_890,
        duration_ms: Some(120_000),
        container: Some("mkv".to_owned()),
        video_codec: Some("h264".to_owned()),
        video_profile: Some("high".to_owned()),
        width: Some(1920),
        height: Some(1080),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
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
