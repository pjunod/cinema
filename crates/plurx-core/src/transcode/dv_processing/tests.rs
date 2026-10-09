use super::*;
use std::path::PathBuf;

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
            el_shape: el_shape(),
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
        DvSelection::KeepExisting(DvFallbackReason::ProductionUnqualified)
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
        DvSelection::KeepExisting(DvFallbackReason::ProductionUnqualified)
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
                        DvDestination::Hdr10 => hdr,
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
    plan.capability.subset = DvMetadataSubset::RuntimeP7MasterDomainLinearDzV1;
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
    let mapped = dv_map_source_to_output_grid(&source, (1, 1_000), grid, 0, 48)
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
    assert!(dv_map_source_to_output_grid(&source[..47], (1, 1_000), grid, 0, 48).is_err());
    let mut dropped = source.clone();
    dropped[24].pts = dropped[25].pts;
    assert!(dv_map_source_to_output_grid(&dropped, (1, 1_000), grid, 0, 48).is_err());
    let mut wrong = source.clone();
    wrong[47].pts = DvTimestamp::new(1_970, 1_000).expect("valid bounded clock fixture");
    assert!(dv_map_source_to_output_grid(&wrong, (1, 1_000), grid, 0, 48).is_err());
    assert!(dv_map_source_to_output_grid(&source, (1, 24), grid, 0, 48).is_err());
}
