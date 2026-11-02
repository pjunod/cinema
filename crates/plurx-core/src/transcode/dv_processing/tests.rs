use super::*;
use std::path::PathBuf;

#[test]
fn completed_vaapi_policy_separates_backend_identity_without_leaking_to_other_encoders() {
    use crate::transcode::{Encoder, VaapiHdrPolicy};
    let plain = backend();
    let policy = Some(VaapiHdrPolicy::MesaRadeonsiVbaq);
    assert_ne!(
        plain.with_completed_encoder_policy(Encoder::Vaapi, policy),
        plain
    );
    assert_eq!(
        plain.with_completed_encoder_policy(Encoder::Vaapi, policy),
        plain.with_completed_encoder_policy(Encoder::Vaapi, policy)
    );
    assert_eq!(
        plain.with_completed_encoder_policy(Encoder::Vaapi, None),
        plain
    );
    for encoder in [Encoder::Software, Encoder::Nvenc] {
        assert_eq!(plain.with_completed_encoder_policy(encoder, policy), plain);
    }
}

#[test]
fn explicit_export_policy_keeps_native_shapes_and_separates_resampling_identity() {
    let native = (3840, 2160);
    let export = DvExportPolicy::PqBilinearYuv420p10V1 {
        raster: (1280, 720),
    };
    assert_eq!(export.raster(native).expect("bounded export"), (1280, 720));
    assert_eq!(
        DvExportPolicy::NativeRgb48V1
            .raster(native)
            .expect("native"),
        native
    );
    assert_eq!(
        DvExportPolicy::NativeRgb48V1.target_parameters().as_str(),
        hex::encode(Sha256::digest(b"bt2020-pq-master-clip-v1"))
    );
    assert_ne!(
        export.target_parameters(),
        DvExportPolicy::NativeRgb48V1.target_parameters()
    );
    assert_ne!(
        export.target_parameters(),
        DvExportPolicy::PqBilinearYuv420p10V1 {
            raster: (1920, 1080)
        }
        .target_parameters()
    );
    for raster in [
        (3842, 2160),
        (3840, 2162),
        (1281, 720),
        (1280, 721),
        (0, 0),
        (16, 16),
    ] {
        assert!(DvExportPolicy::PqBilinearYuv420p10V1 { raster }
            .raster(native)
            .is_err());
    }
}

#[test]
fn completed_packet_authoring_keeps_base_and_dynamic_association() {
    use crate::fmp4::{self, Unit};
    // Three Main10 pictures encoded at 24000/1001 by the retained x265
    // control, then authored by the independently built C libdovi mux tool.
    // Its absolute start is deliberately nonzero; this is no movie receipt.
    let encoded = include_bytes!("../../../tests/fixtures/dv-runtime/authored.mp4");
    let rpus = vec![
        include_bytes!("../../../tests/fixtures/dv-runtime/frame-000.nal").to_vec(),
        include_bytes!("../../../tests/fixtures/dv-runtime/frame-001.nal").to_vec(),
        include_bytes!("../../../tests/fixtures/dv-runtime/frame-002.nal").to_vec(),
    ];
    let independently_authored_payloads = dv_validate_encoded_window(
        encoded,
        (64, 64),
        24000,
        48048,
        1001,
        &rpus,
        DvDestination::Profile81,
    )
    .expect("independent C authored control");
    let mut reader = fmp4::FragmentReader::new();
    reader.push(encoded);
    let Some(Unit::Init(mut init)) = reader.next_unit().expect("control init") else {
        panic!("control init missing");
    };
    fmp4::remove_dolby_vision_record(&mut init).expect("remove control DV record");
    let video = init.video().expect("video").id;
    let mut base = init.bytes.clone();
    while let Some(unit) = reader.next_unit().expect("control fragment") {
        let Unit::Fragment(mut fragment) = unit else {
            continue;
        };
        fmp4::rewrite_video_samples(&mut fragment, &init.tracks, video, |sample| {
            let mut output = Vec::new();
            let mut at = 0;
            while at < sample.len() {
                let count = u32::from_be_bytes(
                    sample[at..at + 4].try_into().expect("validated NAL length"),
                ) as usize;
                let end = at + 4 + count;
                if (sample[at + 4] >> 1) & 63 != 62 {
                    output.extend_from_slice(&sample[at..end]);
                }
                at = end;
            }
            Ok(output)
        })
        .expect("strip RPUs while preserving base packets");
        base.extend_from_slice(&fragment.bytes);
    }
    assert!(
        reader.saw_trailer(),
        "the retained input actually completed"
    );
    base.extend_from_slice(&completed_window_trailer(&init));
    let base_payloads = dv_validate_encoded_window(
        &base,
        (64, 64),
        24000,
        48048,
        1001,
        &rpus,
        DvDestination::Hdr10,
    )
    .expect("HDR10 control");
    let authored = dv_author_profile81_window(&base, (64, 64), 24000, 48048, 1001, &rpus, 1)
        .expect("packet-aware author");
    let mut completed = fmp4::FragmentReader::new();
    completed.push(&authored);
    while completed
        .next_unit()
        .expect("complete authored stream")
        .is_some()
    {}
    assert!(completed.saw_trailer());
    let trailer_bytes = completed_window_trailer(&init).len();
    assert!(
        dv_author_profile81_window(
            &base[..base.len() - trailer_bytes],
            (64, 64),
            24000,
            48048,
            1001,
            &rpus,
            1
        )
        .is_err(),
        "absent completion cannot be authored"
    );
    assert!(
        dv_author_profile81_window(
            &base[..base.len() - 1],
            (64, 64),
            24000,
            48048,
            1001,
            &rpus,
            1
        )
        .is_err(),
        "truncated completion cannot be authored"
    );
    assert_eq!(base_payloads.len(), 3);
    assert_eq!(
        dv_validate_encoded_window(
            &authored,
            (64, 64),
            24000,
            48048,
            1001,
            &rpus,
            DvDestination::Profile81
        )
        .expect("exact associated adaptation"),
        independently_authored_payloads
    );
    let mut swapped = rpus.clone();
    swapped.swap(0, 1);
    assert!(dv_validate_encoded_window(
        &authored,
        (64, 64),
        24000,
        48048,
        1001,
        &swapped,
        DvDestination::Profile81
    )
    .is_err());
    assert!(dv_validate_encoded_window(
        &authored,
        (64, 64),
        24000,
        0,
        1001,
        &rpus,
        DvDestination::Profile81
    )
    .is_err());
    assert!(
        dv_author_profile81_window(&base, (64, 64), 24000, 48048, 1001, &rpus[..2], 1).is_err()
    );
    assert!(dv_validate_encoded_window(
        &authored[..authored.len() - 1],
        (64, 64),
        24000,
        48048,
        1001,
        &rpus,
        DvDestination::Profile81
    )
    .is_err());
}

fn digest(c: char) -> DvDigest {
    DvDigest::new(c.to_string().repeat(64)).expect("valid synthetic control")
}
fn key(ordinal: u64) -> DvFrameKey {
    DvFrameKey {
        absolute_video_index: 0,
        continuity_epoch: 7,
        pts: DvTimestamp::new(ordinal as i64, 24).expect("valid synthetic control"),
        display_ordinal: ordinal,
    }
}
fn duration() -> DvDuration {
    DvDuration::new(1, 24, DvDurationProvenance::DeclaredFixtureInterval)
        .expect("valid synthetic control")
}
fn backend() -> DvBackendIdentity {
    DvBackendIdentity::new(
        digest('a'),
        digest('b'),
        digest('c'),
        digest('d'),
        1,
        digest('e'),
    )
    .expect("valid synthetic control")
}
fn media() -> crate::domain::MediaFile {
    crate::domain::MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 42,
        item_id: 7,
        path: PathBuf::from("/media/Heat.mkv"),
        size: 12_345_678,
        mtime: 1_700_000_000,
        duration_ms: Some(9_000_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(3840),
        height: Some(2160),
        bit_depth: Some(10),
        hdr: Some("hdr10".into()),
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(60_000_000),
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: crate::domain::DolbyVisionFacts::default(),
    }
}

fn bl_shape() -> DvPlaneShape {
    DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7BaseYuv420P10Limited).expect("bounded BL")
}
fn el_shape() -> DvPlaneShape {
    DvPlaneShape::new(64, 64, DvPlaneRepresentation::P7ElYuv420P10Residual).expect("bounded EL")
}
fn source() -> DvSourceFacts {
    let file = media();
    DvSourceFacts::new(
        DolbyVisionFacts {
            profile: Some(7),
            level: Some(6),
            bl_compat_id: Some(1),
            el_present: Some(true),
            rpu_present: Some(true),
        },
        0,
        DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("valid synthetic control"),
        DecodeCacheIdentity::from_media_file(&file),
        PlanSourceBinding::DescriptorBound,
        digest('c'),
        DvRpuState::ValidatedFresh,
        DvElKind::Fel,
        Some(DvSourceCoverage::sampled(7, vec![key(0)]).expect("valid synthetic control")),
        Some(bl_shape()),
        Some(el_shape()),
    )
    .expect("valid synthetic control")
}
fn input(destination: DvDestination) -> DvResolutionInput {
    DvResolutionInput {
        existing_route: DvExistingRoute::Other,
        enhanced_encode_allowed: true,
        destination,
        preferences: DvPreferences {
            hdr_processing: true,
            fel_reencode: true,
            ..Default::default()
        },
        source: source(),
        selected_backend: backend(),
        capability: Some(DvCapabilityEvidence {
            completed_window_checked: false,
            scope: DvEvidenceScope::SyntheticControl,
            identity: backend(),
            schema: DV_CONTRACT_VERSION,
            subset: DvMetadataSubset::SyntheticP7AffineLumaLinearNlqV1,
            decoder: DecodeBackend::Software,
            encoder: Encoder::Software,
            raster: (64, 64),
            bl_shape: bl_shape(),
            el_shape: Some(el_shape()),
            max_frames: 64,
            target: DvTargetPolicy::new(1, 10_000_000, 5, digest('f'))
                .expect("valid synthetic control"),
            evidence_digest: digest('f'),
            validity: DvProofValidity::new(0, Some(100), 1, false)
                .expect("valid synthetic control"),
        }),
        now: 1,
    }
}
fn plan(destination: DvDestination) -> DvProcessingPlan {
    let DvSelection::Selected(plan) =
        resolve_inner(&input(destination), true).expect("valid synthetic control")
    else {
        panic!("synthetic plan refused")
    };
    *plan
}
fn parsed(p81: bool) -> DvParsedMetadata {
    DvParsedMetadata {
        profile: if p81 { 8 } else { 7 },
        depths: [10, 10, 12],
        luma_integer: [0, if p81 { 1 } else { 0 }],
        luma_fraction: if p81 { [0, 0] } else { [524_288, 6_291_456] },
        coefficient_denominator: 8_388_608,
        pivots: [0, 1023],
        curve_representation: DvCurveRepresentation::PolynomialAffine,
        polynomial_order_minus1: 0,
        chroma_integer: [[0, 1]; 2],
        chroma_fraction: [[0, 0]; 2],
        coefficient_data_type: 0,
        bl_full_range: false,
        source_el_kind: if p81 { DvElKind::None } else { DvElKind::Fel },
        nonlinear_matrix: [9574, 0, 13802, 9574, -1540, -5348, 9574, 17610, 0],
        linear_matrix: [7222, 8771, 390, 2654, 12430, 1300, 0, 422, 15962],
        nonlinear_offsets: [16_777_216, 134_217_728, 134_217_728],
        nlq_offset: [512; 3],
        nlq_slope: [8192; 3],
        nlq_slope_integer: [0; 3],
        nlq_threshold_integer: [0; 3],
        nlq_threshold: [4096; 3],
        nlq_max_integer: [1; 3],
        nlq_max_fraction: [0; 3],
        residual_disabled: p81,
        reused: false,
        creative_trim_levels: BTreeSet::new(),
        other_metadata_levels: BTreeSet::from([1, 6]),
    }
}
fn frame(plan: &DvProcessingPlan, ordinal: u64) -> DvFrameEvidence {
    let key = key(ordinal);
    DvFrameEvidence {
        key: key.clone(),
        duration: duration(),
        observed_source: plan.source.observed_source.clone(),
        backend: backend(),
        bl_payload: digest('a'),
        bl_shape: bl_shape(),
        el_shape: Some(el_shape()),
        el_payload: Some((key.clone(), digest('b'))),
        fresh_raw_rpu: Some(digest('c')),
        raw_rpu_frame: Some(key.clone()),
        parsed: parsed(false),
        reconstructed_base: Some(digest('d')),
        adapted: (plan.destination == DvDestination::Profile81).then(|| {
            DvMetadataAdaptationEvidence {
                source_rpu: digest('c'),
                reconstructed_base: digest('d'),
                destination_rpu: digest('e'),
                parsed: parsed(true),
            }
        }),
        applied_operations: plan.graph.required_operations(),
    }
}
fn make_observer(plan: DvProcessingPlan, first: u64, count: u64) -> DvIntervalObserver {
    let source_trace = (0..first + count)
        .map(|ordinal| (key(ordinal), duration()))
        .collect();
    let expected = DvIntervalExpectation::fixture(
        plan.source.observed_source.clone(),
        0,
        7,
        digest('f'),
        source_trace,
        DvTimestamp::new(first as i64, 24).expect("valid synthetic control"),
        DvTimestamp::new((first + count) as i64, 24).expect("valid synthetic control"),
    )
    .expect("valid synthetic control");
    DvIntervalObserver::new(plan, expected).expect("valid synthetic control")
}
fn settle(
    observer: &DvIntervalObserver,
    served: &[DvFrameKey],
) -> Result<DvProcessingReceipt, DvContractError> {
    observer.settle(
        served,
        1,
        digest('a'),
        digest('b'),
        digest('c'),
        true,
        &mut DvIntervalSettlementLedger::default(),
    )
}

#[test]
fn production_registry_is_empty_even_for_relabelled_synthetic_evidence() {
    let mut input = input(DvDestination::Hdr10);
    assert!(matches!(
        resolve_dv_processing(&input).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::AwaitingFirstWindow)
    ));
    let scope: DvEvidenceScope =
        serde_json::from_str("\"production_route\"").expect("valid synthetic control");
    assert_eq!(
        DvCapabilityEvidence::validate_claimed_scope(scope),
        Err(DvContractError::ProductionQualificationUnavailable)
    );
    input
        .capability
        .as_mut()
        .expect("valid synthetic control")
        .scope = scope;
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::AwaitingFirstWindow)
    ));
}
#[test]
fn every_preference_combination_preserves_conversion_and_native_constraints() {
    for hdr in [false, true] {
        for fel in [false, true] {
            for conversion in [false, true] {
                for destination in [DvDestination::Hdr10, DvDestination::Profile81] {
                    let mut input = input(destination);
                    input.preferences = DvPreferences {
                        hdr_processing: hdr,
                        fel_reencode: fel,
                        conversion_permitted: conversion,
                        planning_input_generation: 0,
                    };
                    let expected = match destination {
                        DvDestination::Hdr10 | DvDestination::Sdr => hdr,
                        DvDestination::Profile81 => fel && conversion,
                    };
                    assert_eq!(
                        matches!(
                            resolve_inner(&input, true).expect("valid synthetic control"),
                            DvSelection::Selected(_)
                        ),
                        expected
                    );
                    input.existing_route = DvExistingRoute::NativeDvCopy;
                    assert!(matches!(
                        resolve_inner(&input, true).expect("valid synthetic control"),
                        DvSelection::KeepExisting(DvFallbackReason::NativeCompatible)
                    ));
                    input.existing_route = DvExistingRoute::Other;
                    input.enhanced_encode_allowed = false;
                    assert!(matches!(
                        resolve_inner(&input, true).expect("valid synthetic control"),
                        DvSelection::KeepExisting(DvFallbackReason::ExistingDeliveryConstraint)
                    ));
                }
            }
        }
    }
    assert!(!DvPreferences::default().hdr_processing && !DvPreferences::default().fel_reencode);
}
#[test]
fn stale_wrong_worker_unknown_schema_and_catalog_binding_refuse() {
    let mut input = input(DvDestination::Hdr10);
    input.now = 100;
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::StaleBackendProof)
    ));
    input.now = 1;
    input.selected_backend.executable = digest('f');
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::StaleBackendProof)
    ));
    input.selected_backend = backend();
    input
        .capability
        .as_mut()
        .expect("valid synthetic control")
        .schema = 999;
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::UnsupportedReceiptSchema)
    ));
    input
        .capability
        .as_mut()
        .expect("valid synthetic control")
        .schema = 1;
    input.source.binding = PlanSourceBinding::CatalogRow;
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::UnqualifiedSourceBinding)
    ));
}
#[test]
fn unknown_el_reuse_and_noncompatible_profiles_refuse() {
    let mut input = input(DvDestination::Hdr10);
    input.source.el_kind = DvElKind::Unknown;
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::UnknownElKind)
    ));
    input.source.el_kind = DvElKind::Fel;
    input.source.rpu_state = DvRpuState::ReuseUnsupported;
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::RpuUnavailable)
    ));
    input.source.rpu_state = DvRpuState::ValidatedFresh;
    input.source.catalog.profile = Some(5);
    assert!(matches!(
        resolve_inner(&input, true).expect("valid synthetic control"),
        DvSelection::KeepExisting(DvFallbackReason::UnsupportedProfileOrBase)
    ));
}
#[test]
fn graph_refuses_duplicate_missing_wrong_domain_and_unproved_operations() {
    let strategy = DvStrategy::FelRpuToHdr10;
    let edges = DvGraph::expected(strategy);
    for operation in [
        DvOperation::PolynomialReshape,
        DvOperation::LinearNlqResidual,
        DvOperation::RpuColorConversion,
    ] {
        let mut changed = edges.clone();
        changed.retain(|edge| edge.operation != operation);
        assert!(DvGraph::new(strategy, changed, false).is_err());
        let mut changed = edges.clone();
        changed.push(
            edges
                .iter()
                .find(|edge| edge.operation == operation)
                .expect("valid synthetic control")
                .clone(),
        );
        assert!(DvGraph::new(strategy, changed, false).is_err());
    }
    for operation in [
        DvOperation::MmrReshape,
        DvOperation::TargetMapping,
        DvOperation::CreativeTrimApplication,
        DvOperation::BaseNormalization,
        DvOperation::P81ContainerSignaling,
    ] {
        let mut changed = edges.clone();
        changed[1].operation = operation;
        assert!(DvGraph::new(strategy, changed, false).is_err());
    }
    let mut changed = edges;
    changed[1].input = DvIntermediateDomain::ReconstructedBt2020PqRgb;
    assert!(DvGraph::new(strategy, changed, false).is_err());
}
#[test]
fn rational_duration_and_equal_pts_are_not_ambiguous_aliases() {
    assert_eq!(
        DvTimestamp::new(2, 48).expect("valid synthetic control"),
        DvTimestamp::new(1, 24).expect("valid synthetic control")
    );
    assert!(
        DvTimestamp::new(i64::MIN, 1).expect("valid synthetic control")
            < DvTimestamp::new(0, 1).expect("valid synthetic control")
    );
    assert!(DvTimestamp::new(1, 0).is_err());
    assert!(DvDuration::new(0, 1, DvDurationProvenance::StoredContainer).is_err());
    let mut observer = make_observer(plan(DvDestination::Hdr10), 0, 1);
    let original = key(0);
    observer
        .record_decoded(original.clone(), duration())
        .expect("valid synthetic control");
    let mut duplicate = original;
    duplicate.display_ordinal = 999;
    assert_eq!(
        observer.record_decoded(duplicate, duration()),
        Err(DvContractError::DuplicateFrame)
    );
    let inferred = DvDuration::new(1, 24, DvDurationProvenance::DecoderInferred)
        .expect("valid synthetic control");
    assert!(observer.record_decoded(key(1), inferred).is_err());
}
#[test]
fn sampled_metadata_cannot_certify_later_frame_or_curve_change() {
    let plan = plan(DvDestination::Hdr10);
    assert!(plan
        .source
        .sampled_coverage()
        .expect("valid synthetic control")
        .covers(&key(0)));
    assert!(!plan
        .source
        .sampled_coverage()
        .expect("valid synthetic control")
        .covers(&key(1)));
    let mut observer = make_observer(plan.clone(), 0, 2);
    observer
        .record_decoded(key(1), duration())
        .expect("valid synthetic control");
    let mut later = frame(&plan, 1);
    later.parsed.luma_fraction[1] = 1;
    assert!(observer.accept(later).is_err());
    assert_eq!(
        observer.emit(&key(1)),
        Err(DvContractError::IncompleteCoverage)
    );
}
#[test]
fn each_emitted_frame_requires_its_own_rpu_el_and_ordered_work() {
    let plan = plan(DvDestination::Hdr10);
    for mutation in 0..5 {
        let mut observer = make_observer(plan.clone(), 0, 1);
        observer
            .record_decoded(key(0), duration())
            .expect("valid synthetic control");
        let mut bad = frame(&plan, 0);
        match mutation {
            0 => bad.fresh_raw_rpu = None,
            1 => bad.el_payload = None,
            2 => bad.el_payload.as_mut().expect("valid synthetic control").0 = key(1),
            3 => {
                bad.applied_operations.remove(0);
            }
            _ => bad.parsed.reused = true,
        };
        assert!(observer.accept(bad).is_err());
        assert!(observer.emit(&key(0)).is_err());
        assert!(settle(&observer, &[key(0)]).is_err());
    }
}
#[test]
fn rejected_disconnected_stage_successes_do_not_sum_into_acceptance() {
    let plan = plan(DvDestination::Hdr10);
    let mut observer = make_observer(plan.clone(), 0, 1);
    for ordinal in 0..2 {
        observer
            .record_decoded(key(ordinal), duration())
            .expect("valid synthetic control");
        let mut bad = frame(&plan, ordinal);
        if ordinal == 0 {
            bad.el_payload = None;
        } else {
            bad.fresh_raw_rpu = None;
        }
        assert!(observer.accept(bad).is_err());
    }
    assert!(settle(&observer, &[key(0), key(1)]).is_err());
}
#[test]
fn duplicate_terminal_emit_empty_and_partial_coverage_refuse() {
    let plan = plan(DvDestination::Hdr10);
    let mut observer = make_observer(plan.clone(), 0, 1);
    assert!(settle(&observer, &[]).is_err());
    observer
        .record_decoded(key(0), duration())
        .expect("valid synthetic control");
    observer
        .accept(frame(&plan, 0))
        .expect("valid synthetic control");
    assert_eq!(
        observer.accept(frame(&plan, 0)),
        Err(DvContractError::DuplicateFrame)
    );
    observer.emit(&key(0)).expect("valid synthetic control");
    assert_eq!(observer.emit(&key(0)), Err(DvContractError::DuplicateFrame));
    assert!(settle(&observer, &[]).is_err());
    assert!(settle(&observer, &[key(0), key(0)]).is_err());
    assert!(observer
        .settle(
            &[key(0)],
            1,
            digest('a'),
            digest('b'),
            digest('c'),
            false,
            &mut DvIntervalSettlementLedger::default()
        )
        .is_err());
    observer
        .record_decoded(key(1), duration())
        .expect("valid synthetic control");
    assert!(settle(&observer, &[key(0)]).is_err());
}
#[test]
fn accepted_preroll_is_not_the_served_set_and_cannot_award_enhancement() {
    let plan = plan(DvDestination::Hdr10);
    let mut observer = make_observer(plan.clone(), 1, 2);
    for ordinal in 0..3 {
        observer
            .record_decoded(key(ordinal), duration())
            .expect("valid synthetic control");
        observer
            .accept(frame(&plan, ordinal))
            .expect("valid synthetic control");
    }
    observer.emit(&key(1)).expect("valid synthetic control");
    observer.emit(&key(2)).expect("valid synthetic control");
    let receipt = settle(&observer, &[key(1), key(2)]).expect("valid synthetic control");
    assert_eq!(receipt.counts().accepted, 3);
    assert_eq!(receipt.counts().emitted, 2);
    assert_eq!(receipt.served_frames().len(), 2);
    assert!(!receipt.reports_hdr10_enhanced(
        &DvProductionRegistry,
        &plan,
        &digest('a'),
        &digest('c')
    ));
    observer.fail(DvFallbackReason::BackendFailure);
    assert!(settle(&observer, &[key(1), key(2)]).is_err());
}
#[test]
fn zero_residual_identity_rpu_requires_acceptance_not_pixel_difference() {
    let mut plan = plan(DvDestination::Hdr10);
    plan.capability.subset = DvMetadataSubset::SyntheticP7IdentityLinearNlqV1;
    let mut observer = make_observer(plan.clone(), 0, 1);
    observer
        .record_decoded(key(0), duration())
        .expect("valid synthetic control");
    let mut frame = frame(&plan, 0);
    frame.parsed.luma_integer = [0, 1];
    frame.parsed.luma_fraction = [0, 0];
    observer.accept(frame).expect("valid synthetic control");
    observer.emit(&key(0)).expect("valid synthetic control");
    assert_eq!(
        settle(&observer, &[key(0)])
            .expect("valid synthetic control")
            .counts()
            .el_accepted,
        1
    );
}
#[test]
fn reconstructed_base_rejects_original_curve_trim_and_matrix() {
    let plan = plan(DvDestination::Profile81);
    for mutation in 0..3 {
        let mut observer = make_observer(plan.clone(), 0, 1);
        observer
            .record_decoded(key(0), duration())
            .expect("valid synthetic control");
        let mut frame = frame(&plan, 0);
        let adapted = &mut frame
            .adapted
            .as_mut()
            .expect("valid synthetic control")
            .parsed;
        match mutation {
            0 => {
                adapted.luma_integer = [0, 0];
                adapted.luma_fraction = [524_288, 6_291_456];
            }
            1 => {
                adapted.creative_trim_levels.insert(2);
            }
            _ => {
                adapted.linear_matrix[0] /= 2;
            }
        }
        assert!(observer.accept(frame).is_err());
        assert!(observer.emit(&key(0)).is_err());
    }
}
#[test]
fn semantic_identity_excludes_observations_revision_and_continuity() {
    let plan = plan(DvDestination::Hdr10);
    let original = plan.semantic_digest();
    let mut changed = plan.clone();
    changed.preferences.planning_input_generation = 900;
    changed.capability.validity.snapshot_revision = 900;
    changed.capability.validity.expires_at = Some(101);
    changed.source.observed_source =
        DecodeSourceIdentity::from_sha256("f".repeat(64)).expect("valid synthetic control");
    assert_eq!(original, changed.semantic_digest());
    let mut other_stream = plan.clone();
    other_stream.source.absolute_video_index = 1;
    assert_ne!(original, other_stream.semantic_digest());
    changed.capability.identity.executable = digest('f');
    assert_ne!(original, changed.semantic_digest());
    changed = plan.clone();
    changed.capability.target.parameters = digest('a');
    assert_ne!(original, changed.semantic_digest());
}
#[test]
fn display_target_semantic_digest_separates_all_buckets_and_title_constants() {
    use crate::scan::dv_title::*;
    let stats = DvTitleStats {
        schema: SCHEMA,
        complete: true,
        decoded_frames: 1,
        valid_l1_frames: 1,
        missing_l1_frames: 0,
        invalid_l1_frames: 0,
        max_l1_pq: Some(4095),
        mastering_luminance: TitleConstant::Unknown,
        mastering_primaries: TitleConstant::Unknown,
    };
    let mut master = plan(DvDestination::Hdr10);
    let master_export = DvExportPolicy::PqBilinearYuv420p10V1 { raster: (64, 64) };
    master.capability.target.parameters = master_export.target_parameters();
    let mut digests = std::collections::BTreeSet::new();
    for nits in DISPLAY_TARGET_BUCKETS {
        let (export, _) = DvExportPolicy::for_display_target((64, 64), nits, Some(stats))
            .expect("supported bucket and complete high-peak title");
        let mut target = master.clone();
        target.capability.target.parameters = export.target_parameters();
        assert_ne!(master.semantic_digest(), target.semantic_digest());
        assert!(digests.insert(target.semantic_digest().as_str().to_owned()));
        let (other, _) = DvExportPolicy::for_display_target(
            (64, 64),
            nits,
            Some(DvTitleStats {
                mastering_luminance: TitleConstant::Consistent(MasteringLuminance {
                    min_1e4_nits: 10,
                    max_nits: 1000,
                }),
                ..stats
            }),
        )
        .expect("supported bucket and consistent mastering luminance");
        let mut changed = target.clone();
        changed.capability.target.parameters = other.target_parameters();
        assert_ne!(target.semantic_digest(), changed.semantic_digest());
        let (elided, _) = DvExportPolicy::for_display_target(
            (64, 64),
            nits,
            Some(DvTitleStats {
                max_l1_pq: Some(0),
                ..stats
            }),
        )
        .expect("supported bucket and complete zero-peak title permit elision");
        let mut same = master.clone();
        same.capability.target.parameters = elided.target_parameters();
        assert_eq!(master.semantic_digest(), same.semantic_digest());
    }
}
#[test]
fn resolved_source_floor_is_the_completed_plan_black_level_and_semantic_identity() {
    use crate::scan::dv_title::*;
    let stats = DvTitleStats {
        schema: SCHEMA,
        complete: true,
        decoded_frames: 1,
        valid_l1_frames: 1,
        missing_l1_frames: 0,
        invalid_l1_frames: 0,
        max_l1_pq: Some(4095),
        mastering_luminance: TitleConstant::Unknown,
        mastering_primaries: TitleConstant::Consistent(MasteringPrimaries {
            index: 0,
            custom_xy: None,
        }),
    };
    let DvDisplayTargetChoice::Target(proposal) =
        DvDisplayTarget::for_title(1600, Some(stats)).expect("title")
    else {
        panic!("target");
    };
    let export = |target| DvExportPolicy::DisplayTargetYuv420p10V1 {
        raster: (64, 64),
        target,
    };
    assert!(
        completed_target_policy(export(proposal)).is_err(),
        "no unresolved completed plan"
    );
    let source = DvSourceMastering {
        primaries_rgbw: [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450],
        min_1e4_nits: 17,
    };
    let resolved = proposal.resolve_source(Some(source)).expect("source floor");
    let fallback = proposal.resolve_source(None).expect("renderer floor");
    let mut source_plan = plan(DvDestination::Hdr10);
    source_plan.capability.target =
        completed_target_policy(export(resolved)).expect("completed target");
    let mut fallback_plan = source_plan.clone();
    fallback_plan.capability.target =
        completed_target_policy(export(fallback)).expect("fallback target");
    assert_eq!(source_plan.capability.target.black_1e4_nits, Some(17));
    assert_eq!(source_plan.capability.target.black_millinit, 1);
    assert_eq!(fallback_plan.capability.target.black_1e4_nits, Some(50));
    assert_eq!(
        source_plan.capability.target.parameters,
        export(resolved).target_parameters()
    );
    assert_ne!(
        source_plan.semantic_digest(),
        fallback_plan.semantic_digest()
    );
    assert_eq!(
        serde_json::to_value(&source_plan.capability.target).expect("completed report target")
            ["black_1e4_nits"],
        17
    );
}

#[test]
fn successor_generations_do_not_reset_recovery_episode() {
    let mut episode = DvRecoveryEpisode::default();
    episode
        .attempt(DvStrategy::FelRpuToHdr10)
        .expect("valid synthetic control");
    episode.record_failure().expect("valid synthetic control");
    episode
        .attempt(DvStrategy::BaseRpuToHdr10)
        .expect("valid synthetic control");
    episode.record_failure().expect("valid synthetic control");
    assert!(episode.attempt(DvStrategy::FelRpuToHdr10).is_err());
    assert!(episode.attempt(DvStrategy::BaseRpuToHdr10).is_err());
}
#[test]
fn malformed_scalar_bounds_and_source_coverage_refuse() {
    assert!(DvDigest::new("A".repeat(64)).is_err());
    assert!(DvProofValidity::new(10, Some(10), 1, false).is_err());
    assert!(DvTargetPolicy::new(1, 10_000_001, 5, digest('a')).is_err());
    let mut other = key(0);
    other.continuity_epoch = 8;
    assert!(DvSourceCoverage::sampled(7, vec![other]).is_err());
    assert!(DvPreferences {
        planning_input_generation: -1,
        ..Default::default()
    }
    .validate()
    .is_err());
}

#[test]
fn omitted_interior_frame_and_overlapping_settlement_cannot_be_relabelled_preroll() {
    let plan = plan(DvDestination::Hdr10);
    let mut observer = make_observer(plan.clone(), 0, 3);
    for ordinal in 0..3 {
        observer
            .record_decoded(key(ordinal), duration())
            .expect("valid synthetic control");
        observer
            .accept(frame(&plan, ordinal))
            .expect("valid synthetic control");
    }
    observer.emit(&key(0)).expect("valid synthetic control");
    observer.emit(&key(2)).expect("valid synthetic control");
    assert!(settle(&observer, &[key(0), key(2)]).is_err());
    observer.emit(&key(1)).expect("valid synthetic control");
    let mut ledger = DvIntervalSettlementLedger::default();
    observer
        .settle(
            &[key(0), key(1), key(2)],
            1,
            digest('a'),
            digest('b'),
            digest('c'),
            true,
            &mut ledger,
        )
        .expect("valid synthetic control");
    assert!(observer
        .settle(
            &[key(0), key(1), key(2)],
            1,
            digest('d'),
            digest('e'),
            digest('f'),
            true,
            &mut ledger
        )
        .is_err());
}
#[test]
fn overlapping_source_durations_and_unqualified_seek_target_refuse() {
    let source = source();
    let trace = vec![
        (
            key(0),
            DvDuration::new(2, 24, DvDurationProvenance::DeclaredFixtureInterval)
                .expect("valid synthetic control"),
        ),
        (key(1), duration()),
    ];
    assert!(DvIntervalExpectation::fixture(
        source.observed_source.clone(),
        0,
        7,
        digest('f'),
        trace,
        DvTimestamp::new(0, 24).expect("valid synthetic control"),
        DvTimestamp::new(2, 24).expect("valid synthetic control")
    )
    .is_err());
    // Declared synthetic timing control; not qualification of an actual 200ms seek.
    let mut before = key(0);
    before.pts = DvTimestamp::new(140, 1000).expect("valid synthetic control");
    let mut at = key(1);
    at.pts = DvTimestamp::new(230, 1000).expect("valid synthetic control");
    let mut after = key(2);
    after.pts = DvTimestamp::new(300, 1000).expect("valid synthetic control");
    let trace = vec![
        (
            before,
            DvDuration::new(90, 1000, DvDurationProvenance::DeclaredFixtureInterval)
                .expect("valid synthetic control"),
        ),
        (
            at,
            DvDuration::new(70, 1000, DvDurationProvenance::DeclaredFixtureInterval)
                .expect("valid synthetic control"),
        ),
        (
            after,
            DvDuration::new(70, 1000, DvDurationProvenance::DeclaredFixtureInterval)
                .expect("valid synthetic control"),
        ),
    ];
    assert!(DvIntervalExpectation::fixture(
        source.observed_source,
        0,
        7,
        digest('f'),
        trace,
        DvTimestamp::new(200, 1000).expect("valid synthetic control"),
        DvTimestamp::new(370, 1000).expect("valid synthetic control")
    )
    .is_err());
}

#[test]
fn raw_predicate_rejects_mmr_chroma_and_unprocessed_metadata_instead_of_inference() {
    for mutation in 0..5 {
        let mut metadata = parsed(false);
        match mutation {
            0 => metadata.curve_representation = DvCurveRepresentation::Mmr,
            1 => metadata.chroma_integer[0][1] = 0,
            2 => {
                metadata.other_metadata_levels.insert(9);
            }
            3 => metadata.nlq_slope_integer[0] = 1,
            _ => metadata.bl_full_range = true,
        }
        assert!(!metadata.supported(DvMetadataSubset::SyntheticP7AffineLumaLinearNlqV1));
    }
}

#[test]
fn adapted_metadata_binds_the_actual_source_rpu_and_reconstructed_base() {
    let plan = plan(DvDestination::Profile81);
    for mutation in 0..2 {
        let mut observer = make_observer(plan.clone(), 0, 1);
        observer
            .record_decoded(key(0), duration())
            .expect("valid synthetic control");
        let mut evidence = frame(&plan, 0);
        if mutation == 0 {
            evidence
                .adapted
                .as_mut()
                .expect("valid synthetic control")
                .source_rpu = digest('f');
        } else {
            evidence
                .adapted
                .as_mut()
                .expect("valid synthetic control")
                .reconstructed_base = digest('f');
        }
        assert!(observer.accept(evidence).is_err());
    }
}

#[test]
fn fresh_raw_rpu_from_a_different_display_frame_is_rejected() {
    let plan = plan(DvDestination::Hdr10);
    let mut observer = make_observer(plan.clone(), 0, 1);
    observer
        .record_decoded(key(0), duration())
        .expect("valid synthetic control");
    let mut evidence = frame(&plan, 0);
    evidence.raw_rpu_frame = Some(key(1));
    assert!(observer.accept(evidence).is_err());
    assert!(observer.emit(&key(0)).is_err());
}

#[test]
fn a_new_continuity_epoch_cannot_reuse_the_old_sample_observation() {
    let plan = plan(DvDestination::Hdr10);
    let mut new_key = key(0);
    new_key.continuity_epoch = 8;
    let expectation = DvIntervalExpectation::fixture(
        plan.source.observed_source.clone(),
        0,
        8,
        digest('f'),
        vec![(new_key, duration())],
        DvTimestamp::new(0, 24).expect("valid synthetic control"),
        DvTimestamp::new(1, 24).expect("valid synthetic control"),
    )
    .expect("valid synthetic control");
    assert!(DvIntervalObserver::new(plan, expectation).is_err());
}

#[test]
fn source_and_each_decoded_layer_bind_actual_geometry_and_representation() {
    let mut input = input(DvDestination::Hdr10);
    input.source.bl_shape = Some(
        DvPlaneShape::new(128, 64, DvPlaneRepresentation::P7BaseYuv420P10Limited)
            .expect("known shape"),
    );
    assert!(matches!(
        resolve_inner(&input, true).expect("selection"),
        DvSelection::KeepExisting(DvFallbackReason::UnsupportedCodecOrGeometry)
    ));
    let plan = plan(DvDestination::Hdr10);
    for mutation in 0..3 {
        let mut observer = make_observer(plan.clone(), 0, 2);
        observer.record_decoded(key(0), duration()).expect("frame0");
        observer.accept(frame(&plan, 0)).expect("frame0 accepted");
        observer.record_decoded(key(1), duration()).expect("frame1");
        let mut later = frame(&plan, 1);
        match mutation {
            0 => {
                later.bl_shape =
                    DvPlaneShape::new(128, 64, DvPlaneRepresentation::P7BaseYuv420P10Limited)
                        .expect("BL shape")
            }
            1 => {
                later.el_shape = Some(
                    DvPlaneShape::new(64, 128, DvPlaneRepresentation::P7ElYuv420P10Residual)
                        .expect("EL shape"),
                )
            }
            _ => later.el_shape = Some(bl_shape()),
        }
        assert!(observer.accept(later).is_err());
        assert!(observer.emit(&key(1)).is_err());
    }
}
#[test]
fn initial_mel_and_no_el_predicates_are_refused_before_selecting_an_impossible_plan() {
    for kind in [DvElKind::Mel, DvElKind::None] {
        let mut input = input(DvDestination::Hdr10);
        input.source.el_kind = kind;
        assert!(matches!(
            resolve_inner(&input, true).expect("selection"),
            DvSelection::KeepExisting(DvFallbackReason::UnsupportedMetadata)
        ));
    }
}
#[test]
fn exhausted_episode_stays_exhausted_and_failure_requires_an_active_attempt() {
    let mut episode = DvRecoveryEpisode::new(1).expect("budget");
    assert!(episode.record_failure().is_err());
    assert_eq!(episode.failures, 0);
    episode
        .attempt(DvStrategy::FelRpuToHdr10)
        .expect("first candidate");
    assert!(episode.attempt(DvStrategy::BaseRpuToHdr10).is_err());
    assert_eq!(episode.record_failure(), Err(DvContractError::Capacity));
    for _ in 0..3 {
        assert_eq!(
            episode.attempt(DvStrategy::BaseRpuToHdr10),
            Err(DvContractError::Capacity)
        );
        assert_eq!(episode.record_failure(), Err(DvContractError::Capacity));
        assert_eq!(episode.failures, 1);
    }
}

#[test]
fn playback_generation_digest_is_domain_separated_and_validates_uuid() {
    let generation = "00000000-0000-0000-0000-000000000001";
    let digest = dv_playback_generation_digest(generation).expect("valid UUID");
    assert_eq!(
        digest.as_str(),
        "5b04dd287bde74e654bbe2dffbe1c5558891f277f10c9fabe215a2a9eb4eff68"
    );
    assert_ne!(
        digest,
        dv_playback_generation_digest("00000000-0000-0000-0000-000000000002").expect("valid UUID")
    );
    assert!(dv_playback_generation_digest("not-a-generation").is_err());
}

#[test]
fn effective_report_cannot_turn_synthetic_receipt_into_hdr10_enhancement() {
    let plan = plan(DvDestination::Hdr10);
    let mut observer = make_observer(plan.clone(), 0, 1);
    observer
        .record_decoded(key(0), duration())
        .expect("decoded");
    observer.accept(frame(&plan, 0)).expect("accepted");
    observer.emit(&key(0)).expect("emitted");
    let receipt = settle(&observer, &[key(0)]).expect("settled synthetic receipt");
    assert!(receipt
        .hdr10_effective_report(
            &DvProductionRegistry,
            &plan,
            "00000000-0000-0000-0000-000000000001",
            &digest('c')
        )
        .expect("valid playback identity")
        .is_none());
    assert!(receipt
        .hdr10_effective_report(&DvProductionRegistry, &plan, "invalid", &digest('c'))
        .is_err());
}

#[test]
fn base_piecewise_rpu_accepts_delta_pivots_and_counts_actual_segments() {
    let raw = include_bytes!("../../../tests/fixtures/dv-runtime/p8-piecewise.nal");
    let metadata = DvBaseMetadata::from_raw_rpu(raw, (64, 64)).expect("reviewed piecewise RPU");
    assert_eq!(metadata.profile, 8);
    assert_eq!(metadata.polynomial_segments, 4);
    assert_eq!(metadata.mmr_segments, 0);
    assert!(metadata
        .applied_operations
        .contains(&DvOperation::PolynomialReshape));
    assert!(!metadata
        .applied_operations
        .contains(&DvOperation::MmrReshape));
    assert!(!metadata
        .applied_operations
        .contains(&DvOperation::LinearNlqResidual));
}

#[test]
fn base_rpu_accepts_signalled_bounded_curve_extents_without_replacing_them() {
    use dolby_vision::rpu::dovi_rpu::DoviRpu;
    let fixture = include_bytes!("../../../tests/fixtures/dv-runtime/p8-piecewise.nal");
    for (first, last_delta) in [(0, 509), (2, 509)] {
        let mut rpu = DoviRpu::parse_unspec62_nalu(fixture).expect("fixture");
        let curve = &mut rpu.rpu_data_mapping.as_mut().expect("mapping").curves[0];
        assert_eq!(curve.pivots, [0, 512, 511]);
        curve.pivots = vec![first, 512, last_delta];
        rpu.modified = true;
        let raw = rpu
            .write_hevc_unspec62_nalu()
            .expect("bounded curve serializes");
        let parsed = DvBaseMetadata::from_raw_rpu(&raw, (64, 64))
            .expect("public mapper supports bounded extents");
        assert_eq!(parsed.polynomial_segments, 4);
        let preserved = DoviRpu::parse_unspec62_nalu(&raw).expect("fresh RPU");
        assert_eq!(
            preserved.rpu_data_mapping.expect("mapping").curves[0].pivots,
            [first, 512, last_delta]
        );
    }
    for pivots in [vec![0, 0, 511], vec![0, 512, 512], vec![513, 512, 1]] {
        let mut rpu = DoviRpu::parse_unspec62_nalu(fixture).expect("fixture");
        rpu.rpu_data_mapping.as_mut().expect("mapping").curves[0].pivots = pivots;
        rpu.modified = true;
        let raw = rpu
            .write_hevc_unspec62_nalu()
            .expect("malformed extent control serializes");
        assert!(DvBaseMetadata::from_raw_rpu(&raw, (64, 64)).is_err());
    }
}

/// The master reconstruction applies none of the display metadata (L2, L8,
/// L9, L10, L11, L254, L255), so every version of it that a real title carries
/// is accepted. The Uprising's first refused window carried an L8 trim of a
/// length the old subset refused.
#[test]
fn every_display_metadata_version_the_master_reconstruction_ignores_is_accepted() {
    use dolby_vision::rpu::dovi_rpu::DoviRpu;
    use dolby_vision::rpu::extension_metadata::blocks::{
        ExtMetadataBlock, ExtMetadataBlockLevel10, ExtMetadataBlockLevel11,
        ExtMetadataBlockLevel255, ExtMetadataBlockLevel8, ExtMetadataBlockLevel9,
    };
    use dolby_vision::rpu::extension_metadata::{CmV40DmData, DmData};
    fn widen(fixture: &[u8]) -> Vec<u8> {
        let mut rpu = DoviRpu::parse_unspec62_nalu(fixture).expect("fixture RPU");
        let dm = rpu.vdr_dm_data.as_mut().expect("display metadata");
        if dm.cmv40_metadata.is_none() {
            dm.cmv40_metadata = Some(DmData::V40(CmV40DmData::new_with_l254_402()));
        }
        for block in [
            ExtMetadataBlock::Level8(ExtMetadataBlockLevel8 {
                length: 13,
                target_display_index: 1,
                ..Default::default()
            }),
            ExtMetadataBlock::Level9(ExtMetadataBlockLevel9 {
                length: 1,
                source_primary_index: 2,
                ..Default::default()
            }),
            ExtMetadataBlock::Level10(ExtMetadataBlockLevel10 {
                length: 5,
                target_display_index: 2,
                target_max_pq: 2081,
                target_min_pq: 62,
                target_primary_index: 2,
                ..Default::default()
            }),
            ExtMetadataBlock::Level11(ExtMetadataBlockLevel11 {
                content_type: 1,
                reference_mode_flag: true,
                ..Default::default()
            }),
            ExtMetadataBlock::Level255(ExtMetadataBlockLevel255 {
                dm_run_mode: 0,
                dm_run_version: 1,
                ..Default::default()
            }),
        ] {
            dm.replace_metadata_block(block).expect("CM v4.0 block");
        }
        rpu.modified = true;
        let raw = rpu.write_hevc_unspec62_nalu().expect("widened RPU");
        let reread = DoviRpu::parse_unspec62_nalu(&raw).expect("widened RPU parses");
        let dm = reread.vdr_dm_data.expect("display metadata");
        match dm.get_block(8) {
            Some(ExtMetadataBlock::Level8(level8)) => assert_eq!(level8.length, 13),
            other => panic!("no L8 trim: {other:?}"),
        }
        assert!(
            matches!(dm.get_block(9), Some(ExtMetadataBlock::Level9(level9))
            if level9.source_primary_index == 2)
        );
        assert!(dm.get_block(11).is_some());
        raw
    }
    let fel = widen(include_bytes!(
        "../../../tests/fixtures/dv-runtime/frame-000.nal"
    ));
    let parsed = DvParsedMetadata::from_raw_rpu(&fel).expect("FEL RPU parses");
    let carried: BTreeSet<u8> = parsed
        .other_metadata_levels
        .union(&parsed.creative_trim_levels)
        .copied()
        .collect();
    for level in [8, 9, 10, 11, 255] {
        assert!(carried.contains(&level), "L{level} is carried: {carried:?}");
    }
    assert!(parsed.runtime_supported(), "{parsed:?}");
    let base = widen(include_bytes!(
        "../../../tests/fixtures/dv-runtime/p8-piecewise.nal"
    ));
    DvBaseMetadata::from_raw_rpu(&base, (64, 64)).expect("base RPU with every trim version");
}

#[test]
fn completed_window_report_binds_publication_without_pixel_checksum_authority() {
    let mut raw = dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(include_bytes!(
        "../../../tests/fixtures/dv-runtime/frame-000.nal"
    ))
    .expect("actual authored control source RPU");
    let dm = raw.vdr_dm_data.as_mut().expect("metadata");
    dm.remove_metadata_level(10);
    dm.remove_metadata_level(255);
    for curve in &mut raw.rpu_data_mapping.as_mut().expect("mapping").curves {
        let polynomial = curve.polynomial.as_mut().expect("affine control");
        polynomial.poly_coef_int[0][0] = 0;
        polynomial.poly_coef_int[0][1] = 1;
        polynomial.poly_coef[0][0] = 0;
        polynomial.poly_coef[0][1] = 0;
    }
    raw.modified = true;
    let raw = raw.write_hevc_unspec62_nalu().expect("control RPU rewrite");
    let parsed = DvParsedMetadata::from_raw_rpu(&raw).expect("independent source parse");
    assert!(!parsed.other_metadata_levels.contains(&10));
    assert!(!parsed.other_metadata_levels.contains(&255));
    assert!(parsed.runtime_supported(), "{parsed:?}");
    // Only this private test changes the completion flag. It exercises report
    // binding, and does not turn the control into daemon execution evidence.
    let mut plan = plan(DvDestination::Hdr10);
    plan.capability.scope = DvEvidenceScope::ProductionRoute;
    plan.capability.completed_window_checked = true;
    plan.capability.subset = DvMetadataSubset::RuntimeP7MasterDomainLinearDzV2;
    let mut receipt = dv_receipt_completed_window(&plan, &[key(0)], &[duration()], &[raw])
        .expect("completed receipt");
    let session = "00000000-0000-0000-0000-000000000001";
    let shape = digest('d');
    assert!(receipt
        .hdr10_effective_report(&DvProductionRegistry, &plan, session, &shape)
        .expect("valid focused control")
        .is_none());
    receipt
        .bind_publication(session, shape.clone())
        .expect("exact publication binding");
    let report = receipt
        .hdr10_effective_report(&DvProductionRegistry, &plan, session, &shape)
        .expect("valid focused control")
        .expect("completed published HDR10 window");
    assert!(
        report.fel_contributed,
        "actual paired-layer custody does not depend on diagnostic pixel SHA"
    );
    assert!(receipt
        .hdr10_effective_report(
            &DvProductionRegistry,
            &plan,
            "00000000-0000-0000-0000-000000000002",
            &shape
        )
        .expect("valid focused control")
        .is_none());
    assert!(receipt
        .hdr10_effective_report(&DvProductionRegistry, &plan, session, &digest('e'))
        .expect("valid focused control")
        .is_none());
    receipt.terminal_failure = Some(DvFallbackReason::BackendFailure);
    assert!(receipt
        .hdr10_effective_report(&DvProductionRegistry, &plan, session, &shape)
        .expect("valid focused control")
        .is_none());
}

#[test]
fn completed_base_window_receipt_keeps_display_mapping_and_source_operations() {
    let raw = include_bytes!("../../../tests/fixtures/dv-runtime/p8-piecewise.nal").to_vec();
    let parsed = DvBaseMetadata::from_raw_rpu(&raw, (64, 64)).expect("authored base RPU");
    assert!(parsed
        .applied_operations
        .contains(&DvOperation::PolynomialReshape));
    assert!(!parsed.applied_operations.contains(&DvOperation::MmrReshape));
    let keys = [key(0), key(1)];
    let durations = [duration(), duration()];
    let rpus = [raw.clone(), raw];
    for nits in [None, Some(1000), Some(1600)] {
        // As in the publication-binding control above, only this private test
        // sets the completion flag. This is receipt accounting, not helper or
        // encoded-output qualification and grants no production capability.
        let mut completed = plan(DvDestination::Hdr10);
        completed.source.catalog.profile = Some(i64::from(parsed.profile));
        completed.source.el_kind = parsed.source_el_kind;
        completed.source.coverage =
            Some(DvSourceCoverage::sampled(7, keys.to_vec()).expect("control coverage"));
        completed.source.bl_shape = Some(
            DvPlaneShape::new(64, 64, DvPlaneRepresentation::DoviBaseP10Limited)
                .expect("base shape"),
        );
        completed.source.el_shape = None;
        completed.capability.scope = DvEvidenceScope::ProductionRoute;
        completed.capability.completed_window_checked = true;
        completed.capability.subset = DvMetadataSubset::RuntimeBaseRpuMasterDomainV2;
        completed.capability.bl_shape = completed.source.bl_shape.expect("base shape");
        completed.capability.el_shape = None;
        completed.graph.strategy = DvStrategy::BaseRpuToHdr10;
        completed.graph.pixels = DvGraph::expected(DvStrategy::BaseRpuToHdr10);
        // The completed graph declares both conditional reshape capabilities;
        // this polynomial-only RPU must not acquire an MMR application count.
        completed.graph.pixels.insert(
            2,
            DvGraphEdge {
                operation: DvOperation::MmrReshape,
                input: DvIntermediateDomain::ReshapedBaseVdrComponents,
                output: DvIntermediateDomain::ReshapedBaseVdrComponents,
            },
        );
        if let Some(nits) = nits {
            completed.graph.pixels.insert(
                4,
                DvGraphEdge {
                    operation: DvOperation::DisplayTargetMapping { nits },
                    input: DvIntermediateDomain::TargetMappedBt2020PqRgb,
                    output: DvIntermediateDomain::TargetMappedBt2020PqRgb,
                },
            );
            // The receipt needs the window's frozen target (its title mastering peak decides
            // the CM v2.9 trim interpolation), and only one of the graph's bucket.
            use crate::scan::dv_title::*;
            let target = |nits| match DvDisplayTarget::for_title(
                nits,
                Some(DvTitleStats {
                    schema: SCHEMA,
                    complete: true,
                    decoded_frames: 2,
                    valid_l1_frames: 2,
                    missing_l1_frames: 0,
                    invalid_l1_frames: 0,
                    max_l1_pq: Some(4095),
                    mastering_luminance: TitleConstant::Unknown,
                    mastering_primaries: TitleConstant::Unknown,
                }),
            )
            .expect("title")
            {
                DvDisplayTargetChoice::Target(target) => target,
                DvDisplayTargetChoice::Master { .. } => panic!("target"),
            };
            assert!(dv_receipt_completed_window(&completed, &keys, &durations, &rpus).is_err());
            completed.display_target = Some(target(if nits == 1000 { 1600 } else { 1000 }));
            assert!(dv_receipt_completed_window(&completed, &keys, &durations, &rpus).is_err());
            completed.display_target = Some(target(nits));
        }
        let receipt = dv_receipt_completed_window(&completed, &keys, &durations, &rpus)
            .expect("completed base receipt");
        let mut expected = parsed.applied_operations.clone();
        if let Some(nits) = nits {
            expected.push(DvOperation::DisplayTargetMapping { nits });
        }
        for frame in receipt.served_frames() {
            assert_eq!(frame.applied_operations, expected);
        }
        let counts: BTreeMap<_, _> = expected.into_iter().map(|op| (op, 2)).collect();
        assert_eq!(receipt.counts().operations_applied, counts);
        assert_eq!(receipt.counts().el_accepted, 0);
        assert_eq!(receipt.counts().el_not_required, 2);
        assert!(
            dv_receipt_completed_window(&completed, &keys, &durations, &[vec![], vec![]]).is_err()
        );
        let mut wrong_profile = completed.clone();
        wrong_profile.source.catalog.profile = Some(7);
        assert!(dv_receipt_completed_window(&wrong_profile, &keys, &durations, &rpus).is_err());
        completed.capability.completed_window_checked = false;
        assert!(dv_receipt_completed_window(&completed, &keys, &durations, &rpus).is_err());
    }
}

#[test]
fn output_grid_mapping_preserves_rounded_source_keys_and_refuses_drops() {
    let grid = super::super::VodFrameGrid::new(24_000, 1_001).expect("NTSC cadence");
    let source: Vec<_> = (0..48)
        .map(|ordinal| DvFrameKey {
            absolute_video_index: 2,
            continuity_epoch: 9,
            pts: DvTimestamp::new((ordinal * 1_001 + 12) / 24, 1_000)
                .expect("valid bounded clock fixture"),
            display_ordinal: ordinal as u64,
        })
        .collect();
    let origin = DvSourceOrigin::observed_common(
        DvTimestamp::new(0, 1).expect("valid zero origin timestamp"),
        super::super::VodSourceClock::ReportedOrigin(0),
    )
    .expect("valid observed zero source origin");
    let mapped = dv_map_source_to_output_grid(&source, (1, 1_000), origin, grid, 0, 48)
        .expect("valid bounded clock fixture");
    assert_eq!(mapped.len(), 48);
    assert_eq!(
        mapped[1].source.pts,
        DvTimestamp::new(42, 1_000).expect("valid bounded clock fixture")
    );
    assert_eq!(
        mapped[1].output_pts,
        DvTimestamp::new(1_001, 24_000).expect("valid bounded clock fixture")
    );
    assert!(dv_map_source_to_output_grid(&source[..47], (1, 1_000), origin, grid, 0, 48).is_err());
    let mut dropped = source.clone();
    dropped[24].pts = dropped[25].pts;
    assert!(dv_map_source_to_output_grid(&dropped, (1, 1_000), origin, grid, 0, 48).is_err());
    let mut wrong = source.clone();
    wrong[47].pts = DvTimestamp::new(1_970, 1_000).expect("valid bounded clock fixture");
    assert!(dv_map_source_to_output_grid(&wrong, (1, 1_000), origin, grid, 0, 48).is_err());
    assert!(dv_map_source_to_output_grid(&source, (1, 24), origin, grid, 0, 48).is_err());
}

#[test]
fn common_source_origin_preserves_absolute_keys_across_film_windows() {
    let grid = super::super::VodFrameGrid::new(24_000, 1_001).expect("valid NTSC cadence");
    for millis in [0, 125] {
        let origin = DvSourceOrigin::observed_common(
            DvTimestamp::new(millis, 1000).expect("valid origin timestamp"),
            super::super::VodSourceClock::ReportedOrigin(millis * 1000),
        )
        .expect("valid observed source origin");
        for entry in [0, 1, 2, 360] {
            let first = entry * 48_u64;
            let keys: Vec<_> = (first..first + 48)
                .map(|frame| DvFrameKey {
                    absolute_video_index: 0,
                    continuity_epoch: 11,
                    pts: DvTimestamp::new(
                        millis + i64::try_from(frame * 1001 / 24).expect("frame PTS fits i64"),
                        1000,
                    )
                    .expect("valid source PTS"),
                    display_ordinal: frame,
                })
                .collect();
            let mapped = dv_map_source_to_output_grid(&keys, (1, 1000), origin, grid, first, 48)
                .expect("complete bounded source window");
            assert_eq!(
                mapped.iter().map(|m| m.source.clone()).collect::<Vec<_>>(),
                keys
            );
            assert_eq!(
                mapped[0].output_pts,
                DvTimestamp::new(
                    i64::try_from(first * 1001).expect("output frame index fits i64"),
                    24000,
                )
                .expect("valid output timestamp")
            );
            assert!(
                dv_map_source_to_output_grid(&keys[..47], (1, 1000), origin, grid, first, 48)
                    .is_err()
            );
            let mut duplicate = keys.clone();
            duplicate[24] = duplicate[23].clone();
            assert!(
                dv_map_source_to_output_grid(&duplicate, (1, 1000), origin, grid, first, 48)
                    .is_err()
            );
            let mut drift = keys.clone();
            drift[47].pts = DvTimestamp::new(
                millis
                    + i64::try_from((first + 47) * 1001 / 24).expect("drift frame PTS fits i64")
                    + 2,
                1000,
            )
            .expect("valid drift PTS");
            assert!(
                dv_map_source_to_output_grid(&drift, (1, 1000), origin, grid, first, 48).is_err()
            );
            let wrong = DvSourceOrigin::observed_common(
                DvTimestamp::new(millis + 2, 1000).expect("valid shifted origin timestamp"),
                super::super::VodSourceClock::ReportedOrigin((millis + 2) * 1000),
            )
            .expect("valid shifted source origin");
            assert!(
                dv_map_source_to_output_grid(&keys, (1, 1000), wrong, grid, first, 48).is_err()
            );
            assert!(
                dv_map_source_to_output_grid(&keys, (1, 1000), origin, grid, u64::MAX, 48).is_err()
            );
        }
    }
}

#[test]
fn common_source_origin_bounds_are_absolute_exact_and_seek_stable() {
    for millis in [0, 125] {
        let origin = DvSourceOrigin::observed_common(
            DvTimestamp::new(millis, 1000).expect("valid boundary origin timestamp"),
            super::super::VodSourceClock::ReportedOrigin(millis * 1000),
        )
        .expect("valid observed source origin");
        for entry in [0, 1, 2, 360] {
            let first = entry * 48 * 1001_u64;
            let start = origin
                .source_boundary(first, 24000, (1, 1000))
                .expect("bounded source start boundary");
            let end = origin
                .source_boundary(first + 48 * 1001, 24000, (1, 1000))
                .expect("bounded source end boundary");
            let expected = if entry == 0 {
                DvTimestamp::new(millis, 1000).expect("valid expected start timestamp")
            } else {
                DvTimestamp::new(
                    i64::try_from(first * 1000 / 24000).expect("expected boundary offset fits i64")
                        + millis
                        - 1,
                    1000,
                )
                .expect("valid expected boundary timestamp")
            };
            assert_eq!(
                DvTimestamp::new(start.0, start.1).expect("valid actual boundary timestamp"),
                expected
            );
            assert_eq!(
                end,
                origin
                    .source_boundary((entry + 1) * 48 * 1001, 24000, (1, 1000))
                    .expect("bounded expected next boundary")
            );
        }
        assert!(origin.source_boundary(u64::MAX, 24000, (1, 1000)).is_err());
        assert!(origin.source_boundary(1, 0, (1, 1000)).is_err());
    }
    assert!(DvSourceOrigin::observed_common(
        DvTimestamp::new(0, 1).expect("valid default clock timestamp"),
        super::super::VodSourceClock::ProducerDefaultZero
    )
    .is_err());
    assert!(DvSourceOrigin::observed_common(
        DvTimestamp::new(1, 8).expect("valid mismatched clock timestamp"),
        super::super::VodSourceClock::ReportedOrigin(0)
    )
    .is_err());
    assert!(DvSourceOrigin::observed_common(
        DvTimestamp::new(-1, 8).expect("valid negative origin timestamp"),
        super::super::VodSourceClock::ReportedOrigin(-125000)
    )
    .is_err());
}

#[test]
fn effective_target_report_rejects_ambiguous_or_unsupported_buckets() {
    let generation = "00000000-0000-4000-8000-000000000001";
    let report = |targets: &[u32]| {
        let mut operations = vec![DvOperation::RpuColorConversion, DvOperation::TargetMapping];
        operations.extend(
            targets
                .iter()
                .map(|nits| DvOperation::DisplayTargetMapping { nits: *nits }),
        );
        serde_json::to_vec(
            &serde_json::json!({"generation":generation,"hdr10_enhanced":true,
            "fel_contributed":false,"applied_operations":operations}),
        )
        .expect("report")
    };
    let master =
        DvEffectiveProcessingReport::from_owner_wire(&report(&[]), generation, Some("hdr10"))
            .expect("master report");
    assert_eq!(master.display_target_nits(), None);
    let targeted =
        DvEffectiveProcessingReport::from_owner_wire(&report(&[1600]), generation, Some("hdr10"))
            .expect("target report");
    assert_eq!(targeted.display_target_nits(), Some(1600));
    for buckets in [&[999][..], &[1000, 1600][..], &[1600, 1600][..]] {
        assert!(DvEffectiveProcessingReport::from_owner_wire(
            &report(buckets),
            generation,
            Some("hdr10")
        )
        .is_none());
    }
    assert!(DvEffectiveProcessingReport::from_owner_wire(
        &report(&[1600]),
        generation,
        Some("sdr")
    )
    .is_none());
    assert!(DvEffectiveProcessingReport::from_owner_wire(
        &report(&[1600]),
        "00000000-0000-4000-8000-000000000002",
        Some("hdr10")
    )
    .is_none());
}

#[test]
fn sdr_owner_report_requires_its_grade_target_and_conversion_receipt() {
    let generation = "00000000-0000-4000-8000-000000000001";
    let mut value = serde_json::json!({"generation":generation,"hdr10_enhanced":false,"sdr_processed":true,"fel_contributed":false,"applied_operations":["RpuColorConversion","TargetMapping","Bt709Gamma24Conversion",{"DisplayTargetMapping":{"nits":100}}]});
    let accept = |value: &serde_json::Value, delivered| {
        DvEffectiveProcessingReport::from_owner_wire(
            &serde_json::to_vec(value).expect("wire"),
            generation,
            delivered,
        )
    };
    let report = accept(&value, Some("sdr")).expect("typed SDR report");
    assert_eq!(
        serde_json::to_value(&report).expect("wire")["creative_trims_applied"],
        false
    );
    value["creative_trims_applied"] = true.into();
    assert!(accept(&value, Some("sdr")).is_none());
    value["creative_trims_applied"] = false.into();
    // all, some or none of the served frames applied trims
    use crate::playback::enhancement::CreativeTrims;
    assert_eq!(report.creative_trims(), CreativeTrims::None);
    value["creative_trims_partial"] = true.into();
    let partial = accept(&value, Some("sdr")).expect("partial trims");
    assert_eq!(partial.creative_trims(), CreativeTrims::Partial);
    let operations = value["applied_operations"].as_array_mut().expect("array");
    operations.push("CreativeTrimApplication".into());
    value["creative_trims_applied"] = true.into();
    assert!(accept(&value, Some("sdr")).is_none());
    value["creative_trims_partial"] = false.into();
    let trimmed = accept(&value, Some("sdr")).expect("trimmed");
    assert_eq!(trimmed.creative_trims(), CreativeTrims::All);
    value["applied_operations"]
        .as_array_mut()
        .expect("array")
        .pop();
    value["creative_trims_applied"] = false.into();
    assert!(report.sdr_processed());
    assert_eq!(report.display_target_nits(), Some(100));
    assert!(accept(&value, Some("hdr10")).is_none());
    value["hdr10_enhanced"] = true.into();
    assert!(accept(&value, Some("sdr")).is_none());
    value["hdr10_enhanced"] = false.into();
    value["applied_operations"][3]["DisplayTargetMapping"]["nits"] = 400.into();
    assert!(accept(&value, Some("sdr")).is_none());
    value["applied_operations"][3]["DisplayTargetMapping"]["nits"] = 100.into();
    value["applied_operations"]
        .as_array_mut()
        .expect("array")
        .remove(2);
    assert!(accept(&value, Some("sdr")).is_none());
}

// Numeric metadata shape from public inspection; encoded pictures remain the
// existing authored synthetic control, never a private movie fixture.
fn general_fel_control_rpu() -> dolby_vision::rpu::dovi_rpu::DoviRpu {
    use dolby_vision::rpu::{
        dovi_rpu::DoviRpu,
        rpu_data_mapping::{DoviMMRCurve, DoviMappingMethod},
    };
    let mut rpu = DoviRpu::parse_unspec62_nalu(include_bytes!(
        "../../../tests/fixtures/dv-runtime/frame-000.nal"
    ))
    .expect("authored FEL control");
    let mapping = rpu.rpu_data_mapping.as_mut().expect("control mapping");
    let curve = &mut mapping.curves[0];
    curve.num_pivots_minus2 = 7;
    curve.pivots = vec![0, 128, 128, 128, 128, 128, 128, 128, 127];
    let polynomial = curve.polynomial.as_mut().expect("control polynomial");
    polynomial.poly_order_minus1 = vec![0; 8];
    polynomial.linear_interp_flag = vec![false; 8];
    fn split(values: &[i64]) -> (Vec<i64>, Vec<u64>) {
        (
            values.iter().map(|v| v.div_euclid(8_388_608)).collect(),
            values
                .iter()
                .map(|v| u64::try_from(v.rem_euclid(8_388_608)).expect("nonnegative fraction"))
                .collect(),
        )
    }
    let luma: [[i64; 2]; 8] = [
        [3626, 8388608],
        [3626, 8388607],
        [26593, 8296738],
        [34580, 8275440],
        [-22003, 8388606],
        [-22004, 8388608],
        [-22004, 8388608],
        [-22004, 8388608],
    ];
    polynomial.poly_coef_int = luma
        .iter()
        .map(|v| split(v).0.into_iter().collect())
        .collect();
    polynomial.poly_coef = luma
        .iter()
        .map(|v| split(v).1.into_iter().collect())
        .collect();
    let chroma: [[i64; 21]; 2] = [
        [
            -230431, 11051188, 7235011, -5083283, 1418656, -9439323, 8448648, 2927811, -9479913,
            -11794478, -849379, -11216816, 14289784, 1876340, 862433, 8308635, 4327467, -587533,
            -6229824, 13894123, -2858171,
        ],
        [
            -2612310, -432811, 7716669, -5781593, 7848329, -3804433, 5194520, 4692332, 2594927,
            2650192, 2542064, -18029098, -1373643, 2228015, -564564, -205162, -2612863, 3987001,
            -7321630, 5786752, 802095,
        ],
    ];
    for (component, values) in chroma.iter().enumerate() {
        let curve = &mut mapping.curves[component + 1];
        curve.mapping_idc = DoviMappingMethod::MMR;
        curve.polynomial = None;
        curve.mmr = Some(DoviMMRCurve {
            mmr_order_minus1: vec![2],
            mmr_constant_int: vec![0],
            mmr_constant: vec![[26_163, 566_438][component]],
            mmr_coef_int: vec![values
                .chunks(7)
                .map(|v| split(v).0.into_iter().collect())
                .collect()],
            mmr_coef: vec![values
                .chunks(7)
                .map(|v| split(v).1.into_iter().collect())
                .collect()],
        });
    }
    let nlq = mapping.nlq.as_mut().expect("control NLQ");
    nlq.nlq_offset = [512; 3];
    nlq.linear_deadzone_slope_int = [0; 3];
    nlq.linear_deadzone_slope = [2048; 3];
    nlq.linear_deadzone_threshold_int = [0; 3];
    nlq.linear_deadzone_threshold = [0; 3];
    nlq.vdr_in_max_int = [0; 3];
    nlq.vdr_in_max = [1_048_576; 3];
    rpu.modified = true;
    rpu
}

#[test]
fn fresh_general_fel_metadata_keeps_piecewise_mmr_and_refuses_unsupported_nlq() {
    let raw = general_fel_control_rpu()
        .write_hevc_unspec62_nalu()
        .expect("valid general FEL control");
    let parsed = DvFelMetadata::from_raw_rpu(&raw, (64, 64)).expect("bounded general FEL");
    assert_eq!((parsed.polynomial_segments, parsed.mmr_segments), (8, 2));
    for op in [
        DvOperation::PolynomialReshape,
        DvOperation::MmrReshape,
        DvOperation::LinearNlqResidual,
    ] {
        assert!(parsed.applied_operations.contains(&op));
    }
    assert!(
        DvParsedMetadata::from_raw_rpu(&raw).is_err(),
        "synthetic projection remains narrow"
    );
    let mut clipped = general_fel_control_rpu();
    clipped
        .rpu_data_mapping
        .as_mut()
        .expect("mapping")
        .nlq
        .as_mut()
        .expect("NLQ")
        .vdr_in_max = [16_384; 3];
    let clipped = clipped
        .write_hevc_unspec62_nalu()
        .expect("valid binding NLQ wire control");
    assert!(DvFelMetadata::from_raw_rpu(&clipped, (64, 64)).is_err());
    let mut reused = general_fel_control_rpu();
    reused.header.use_prev_vdr_rpu_flag = true;
    let reused = reused
        .write_hevc_unspec62_nalu()
        .expect("valid reused-RPU wire control");
    assert!(DvFelMetadata::from_raw_rpu(&reused, (64, 64)).is_err());
    assert!(DvFelMetadata::from_raw_rpu(&[], (64, 64)).is_err());
    assert!(DvFelMetadata::from_raw_rpu(
        include_bytes!("../../../tests/fixtures/dv-runtime/p8-piecewise.nal"),
        (64, 64)
    )
    .is_err());
}

fn general_fel_hdr10_control_window() -> Vec<u8> {
    use crate::fmp4::{self, Unit};
    let mut reader = fmp4::FragmentReader::new();
    reader.push(include_bytes!(
        "../../../tests/fixtures/dv-runtime/authored.mp4"
    ));
    let Some(Unit::Init(mut init)) = reader.next_unit().expect("control init") else {
        panic!("control init missing");
    };
    fmp4::remove_dolby_vision_record(&mut init).expect("remove control DV record");
    let video = init.video().expect("video").id;
    let mut encoded = init.bytes.clone();
    while let Some(unit) = reader.next_unit().expect("control unit") {
        let Unit::Fragment(mut fragment) = unit else {
            continue;
        };
        fmp4::rewrite_video_samples(&mut fragment, &init.tracks, video, |sample| {
            let mut output = Vec::new();
            let mut at = 0;
            while at < sample.len() {
                let count =
                    u32::from_be_bytes(sample[at..at + 4].try_into().expect("NAL length")) as usize;
                let end = at + 4 + count;
                if (sample[at + 4] >> 1) & 63 != 62 {
                    output.extend_from_slice(&sample[at..end]);
                }
                at = end;
            }
            Ok(output)
        })
        .expect("strip synthetic RPUs while preserving Main10 packets");
        encoded.extend_from_slice(&fragment.bytes);
    }
    assert!(reader.saw_trailer());
    encoded.extend_from_slice(&completed_window_trailer(&init));
    encoded
}

#[test]
fn completed_general_fel_factory_and_receipt_keep_conditional_operations() {
    let raw = general_fel_control_rpu()
        .write_hevc_unspec62_nalu()
        .expect("general FEL wire control");
    let rpus = vec![raw; 3];
    let keys: Vec<_> = (48..51)
        .map(|ordinal| DvFrameKey {
            pts: DvTimestamp::new(ordinal * 1001, 24000).expect("control PTS"),
            ..key(u64::try_from(ordinal).expect("control ordinal"))
        })
        .collect();
    let duration = DvDuration::new(1001, 24000, DvDurationProvenance::CodedPacketObserved)
        .expect("control duration");
    let encoded = general_fel_hdr10_control_window();
    let mut facts = source();
    facts.coverage =
        Some(DvSourceCoverage::sampled(7, keys.clone()).expect("complete source keys"));
    let origin = DvSourceOrigin::observed_common(
        DvTimestamp::new(0, 1000).expect("control origin"),
        super::super::VodSourceClock::ReportedOrigin(0),
    )
    .expect("observed control origin");
    use crate::scan::dv_title::{DvTitleStats, TitleConstant, SCHEMA};
    let DvDisplayTargetChoice::Target(sdr_target) =
        DvDisplayTarget::for_sdr_title(Some(DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 3,
            valid_l1_frames: 3,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        }))
        .expect("SDR target")
    else {
        panic!("SDR conversion must not elide");
    };
    let sdr_target = sdr_target
        .resolve_sdr_floor(None)
        .expect("resolved SDR floor");
    let mut sdr_encoded = dv_author_sdr_window(&encoded).expect("synthetic SDR declarations");
    // Only the container contract is under test. This deliberately keeps the
    // synthetic coded pictures; it is not evidence of an SDR pixel conversion.
    let colors: Vec<_> = sdr_encoded
        .windows(4)
        .enumerate()
        .filter_map(|(at, bytes)| (bytes == b"nclx").then_some(at))
        .collect();
    assert_eq!(colors.len(), 1);
    sdr_encoded[colors[0] + 4..colors[0] + 10].copy_from_slice(&[0, 1, 0, 1, 0, 1]);
    let make = |destination, source_keys: &[DvFrameKey], source_rpus: &[Vec<u8>]| {
        dv_plan_completed_window(DvCompletedWindowInput {
            source: facts.clone(),
            preferences: input(destination).preferences,
            backend: backend(),
            destination,
            encoder: Encoder::Software,
            mode: DvProcessingMode::Fel,
            source_keys,
            source_rpus,
            source_tick: (1, 1000),
            source_origin: origin,
            grid: super::super::VodFrameGrid::new(24000, 1001).expect("control grid"),
            first_global_frame: 48,
            encoded: if destination == DvDestination::Sdr {
                &sdr_encoded
            } else {
                &encoded
            },
            raster: (64, 64),
            export_policy: if destination == DvDestination::Sdr {
                DvExportPolicy::DisplayTargetYuv420p10V1 {
                    raster: (64, 64),
                    target: sdr_target,
                }
            } else {
                DvExportPolicy::NativeRgb48V1
            },
            source_trace: digest('d'),
            observed_at: 1,
            decoder: DecodeBackend::Software,
        })
    };
    // This exercises the typed factory against retained synthetic encoded data.
    // It is neither a real helper receipt nor native/GPU acceptance evidence.
    for destination in [DvDestination::Hdr10, DvDestination::Sdr] {
        let completed = make(destination, &keys, &rpus).expect("bounded general FEL factory");
        assert!(completed
            .graph
            .required_operations()
            .contains(&DvOperation::MmrReshape));
        let receipt = dv_receipt_completed_window(&completed, &keys, &[duration; 3], &rpus)
            .expect("general FEL receipt");
        for frame in receipt.served_frames() {
            assert!(frame
                .applied_operations
                .contains(&DvOperation::PolynomialReshape));
            assert!(frame.applied_operations.contains(&DvOperation::MmrReshape));
            assert!(frame
                .applied_operations
                .contains(&DvOperation::LinearNlqResidual));
            assert_eq!(
                frame
                    .applied_operations
                    .contains(&DvOperation::Bt709Gamma24Conversion),
                destination == DvDestination::Sdr
            );
            assert_eq!(
                frame
                    .applied_operations
                    .contains(&DvOperation::Bt2020NclConversion),
                destination == DvDestination::Hdr10
            );
            if destination == DvDestination::Sdr {
                assert!(frame
                    .applied_operations
                    .contains(&DvOperation::DisplayTargetMapping { nits: 100 }));
            }
        }
        assert_eq!(
            receipt.counts().operations_applied[&DvOperation::MmrReshape],
            3
        );
        assert_eq!(receipt.counts().el_accepted, 3);
        assert!(make(destination, &keys[..2], &rpus).is_err());
        assert!(make(destination, &keys, &rpus[..2]).is_err());
    }
    assert!(
        dv_author_profile81_window(&encoded, (64, 64), 24000, 48048, 1001, &rpus, 1).is_err(),
        "P8.1 authoring remains narrow"
    );
}

/// A reconstructed Profile 8.1 report travels only with a Dolby Vision
/// carrier, is never HDR10-E or SDR, is never display targeted, and claims
/// FEL only with the NLQ residual it would have applied.
#[test]
fn reconstructed_profile81_owner_report_requires_its_dolby_vision_carrier() {
    let generation = "00000000-0000-4000-8000-000000000001";
    let base = serde_json::json!({"generation":generation,"hdr10_enhanced":false,
        "profile81_reconstructed":true,"fel_contributed":true,
        "applied_operations":["RpuColorConversion","TargetMapping","LinearNlqResidual"]});
    let accept = |value: &serde_json::Value, delivered| {
        DvEffectiveProcessingReport::from_owner_wire(
            &serde_json::to_vec(value).expect("wire"),
            generation,
            delivered,
        )
    };
    let report = accept(&base, Some("dolby_vision")).expect("reconstructed 8.1 report");
    assert!(report.profile81_reconstructed() && report.fel_contributed());
    assert!(!report.sdr_processed());
    let wire = serde_json::to_value(&report).expect("wire");
    assert_eq!(wire["profile81_reconstructed"], true);
    assert_eq!(wire["hdr10_enhanced"], false);
    for delivered in [Some("hdr10"), Some("sdr"), None] {
        assert!(accept(&base, delivered).is_none(), "{delivered:?}");
    }
    // A Dolby Vision carrier without the reconstructed flag is not processed.
    let mut plain = base.clone();
    plain["profile81_reconstructed"] = false.into();
    assert!(accept(&plain, Some("dolby_vision")).is_none());
    // HDR10-E cannot also be the reconstructed 8.1.
    let mut both = base.clone();
    both["hdr10_enhanced"] = true.into();
    assert!(accept(&both, Some("dolby_vision")).is_none());
    // FEL needs the residual.
    let mut no_nlq = base.clone();
    no_nlq["applied_operations"] = serde_json::json!(["RpuColorConversion", "TargetMapping"]);
    assert!(accept(&no_nlq, Some("dolby_vision")).is_none());
    no_nlq["fel_contributed"] = false.into();
    assert!(!accept(&no_nlq, Some("dolby_vision"))
        .expect("base-only window")
        .fel_contributed());
    // The display maps the 8.1 RPU itself; the server never targets it.
    let mut targeted = base.clone();
    targeted["applied_operations"]
        .as_array_mut()
        .expect("array")
        .push(serde_json::json!({"DisplayTargetMapping":{"nits":1000}}));
    assert!(accept(&targeted, Some("dolby_vision")).is_none());
}
