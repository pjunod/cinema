//! Pure static candidate/heap boundaries, and the actual no-follow helper's
//! refusal surface. These do not manufacture P/native closure receipts.
use super::*;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};

fn fixture() -> (
    SessionRequest,
    Arc<plurx_core::store::PlaybackPlanningSnapshot>,
    crate::media_sessions::CandidateCatalogContext,
) {
    let file = plurx_core::domain::MediaFile {
        id: 1,
        item_id: 2,
        path: "/configured/unopened.mkv".into(),
        size: 42,
        mtime: 10,
        duration_ms: Some(1000),
        container: Some("mkv".into()),
        video_codec: Some("h264".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(1280),
        height: Some(720),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        dolby_vision: Default::default(),
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![],
        downloaded_subtitles: vec![],
        scanned_at: 10,
        audio_offset_ms: 0,
        probed: true,
    };
    let settings = std::collections::BTreeMap::new();
    let snapshot = Arc::new(plurx_core::store::PlaybackPlanningSnapshot {
        file,
        probe_json: Some("{\"streams\":[]}".into()),
        capacity: plurx_core::store::PlanningCapacity::unverified(&settings),
        settings,
        generation: 0,
        item_title: Some("static pure fixture".into()),
    });
    let request = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        control_sequence: None,
        file_id: 1,
        playback_id: String::new(),
        request_id: None,
        automatic: false,
        previous_session_id: None,
        reopen_reason: None,
        kind: SessionKind::Transcode { height: 360 },
        start_seconds: 0.0,
        audio_index: None,
        audio_delivery: None,
        audio_claim: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        hdr10: false,
        presentation: Presentation::Vod,
        block_budget_secs: None,
        transport: None,
    };
    let recipe_digest = [7; 32];
    let candidate = QualityCandidate {
        id: CandidateId::for_recipe_digest(recipe_digest),
        recipe_digest,
        route: CandidateRoute::Encode,
        planned_codec: Some("h264".into()),
        processed_hdr10: false,
        normalized_geometry: true,
        width: 640,
        height: 360,
        target_height: 360,
        average_bps: None,
        peak_bps: None,
        grade: OutputGrade::Sdr,
        decoder_compatible: true,
        complete_cache: false,
        sustainable: true,
    };
    let caps = serde_json::from_value(serde_json::json!({
        "v":2, "video":[{"codec":"h264","profiles":["high"],"present":["sdr"]}],
        "audio":["aac"],"containers":["mp4"],"transports":["hls"]
    }))
    .unwrap();
    let catalog = crate::media_sessions::CandidateCatalogContext {
        caps,
        candidate,
        binding: crate::media_pool::PlanningBinding::from_snapshot(&snapshot),
    };
    (request, snapshot, catalog)
}

#[test]
fn static_candidate_binding_is_pure_and_shares_the_exact_fresh_snapshot() {
    let (request, snapshot, catalog) = fixture();
    let expected = (catalog.candidate.id, catalog.candidate.recipe_digest);
    let context =
        restore_selected_candidate(&request, catalog, Arc::clone(&snapshot), expected, "node")
            .unwrap();
    assert!(Arc::ptr_eq(
        context.planning_snapshot.as_ref().unwrap(),
        &snapshot
    ));
    assert_eq!(context.owner_node_id.as_deref(), Some("node"));
    assert_eq!(context.candidate_id, expected.0);
    assert_eq!(context.recipe_digest, expected.1);
}

#[test]
fn static_candidate_binding_refuses_changed_identity_snapshot_caps_and_unsupported_delivery() {
    let (request, snapshot, catalog) = fixture();
    let expected = (catalog.candidate.id, catalog.candidate.recipe_digest);
    let mut bad = catalog.clone();
    bad.binding.generation += 1;
    assert!(
        restore_selected_candidate(&request, bad, Arc::clone(&snapshot), expected, "node").is_err()
    );
    let mut bad = catalog.clone();
    bad.candidate.recipe_digest[0] ^= 1;
    assert!(
        restore_selected_candidate(&request, bad, Arc::clone(&snapshot), expected, "node").is_err()
    );
    let mut bad = catalog.clone();
    bad.caps.v = 3;
    assert!(
        restore_selected_candidate(&request, bad, Arc::clone(&snapshot), expected, "node").is_err()
    );
    let mut bad = catalog.clone();
    bad.candidate.planned_codec = Some("hevc".into());
    assert!(
        restore_selected_candidate(&request, bad, Arc::clone(&snapshot), expected, "node").is_err()
    );
    let mut bad = catalog.clone();
    bad.candidate.decoder_compatible = false;
    assert!(
        restore_selected_candidate(&request, bad, Arc::clone(&snapshot), expected, "node").is_err()
    );
    let mut changed = request.clone();
    changed.kind = SessionKind::Transcode { height: 720 };
    assert!(restore_selected_candidate(&changed, catalog, snapshot, expected, "node").is_err());
}

#[test]
fn static_context_capacity_counts_nested_spare_capacity_and_refuses_nested_admission() {
    let (mut request, snapshot, catalog) = fixture();
    let expected = (catalog.candidate.id, catalog.candidate.recipe_digest);
    request.candidate_context =
        Some(restore_selected_candidate(&request, catalog, snapshot, expected, "node").unwrap());
    let before = request.selected_finite_requested_heap_bytes().unwrap();
    let context = request.candidate_context.as_mut().unwrap();
    context.canonical_caps.as_mut().unwrap().video[0].profiles[0].reserve_exact(4096);
    let actual_capacity = context.canonical_caps.as_ref().unwrap().video[0].profiles[0].capacity();
    let after = request.selected_finite_requested_heap_bytes().unwrap();
    assert!(after > before);
    assert!(after >= actual_capacity);
    let nested = request.clone();
    request.candidate_context.as_mut().unwrap().ordinary_request = Some(Box::new(nested));
    assert!(request.selected_finite_requested_heap_bytes().is_none());
}

#[test]
fn static_raw_catalog_capacity_counts_arrays_strings_and_object_nodes() {
    let mut text = String::new();
    text.reserve_exact(8192);
    text.push('x');
    let capacity = text.capacity();
    let mut values = Vec::with_capacity(32);
    values.push(serde_json::Value::String(text));
    let array_bytes = values.capacity() * std::mem::size_of::<serde_json::Value>();
    let mut object = serde_json::Map::new();
    object.insert("catalog".into(), serde_json::Value::Array(values));
    let value = serde_json::Value::Object(object);
    assert!(
        crate::media_sessions::selected_finite_json_heap_bytes(&value).unwrap()
            >= capacity + array_bytes
    );
}

#[test]
fn static_configured_scope_rejects_parent_components_and_native_walk_refuses_intermediate_symlink()
{
    use std::os::unix::fs::symlink;
    let base = crate::test_tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("source"), b"outside").unwrap();
    symlink(&outside, root.join("escape")).unwrap();
    let escaped = root.join("escape/source");
    assert!(
        static_source_in_roots(&escaped, std::slice::from_ref(&root)),
        "lexical scope alone is not native containment"
    );
    assert!(
        plurx_core::fs_secure::open_read_nofollow_blocking(&escaped).is_err(),
        "the same admitted opener rejects every intermediate symlink"
    );
    assert!(!static_source_in_roots(
        &root.join("../outside/source"),
        std::slice::from_ref(&root)
    ));
    assert!(!static_source_in_roots(
        &outside.join("source"),
        std::slice::from_ref(&root)
    ));
}

#[tokio::test]
async fn static_original_registry_allows_claim_renewal_but_refuses_a_changed_saved_carrier() {
    let (_, _, catalog) = fixture();
    let mut carrier =
        serde_json::json!({ "catalog": catalog, "source_object_version": "s1:1:2:42:10:0:10:0" });
    let digest = static_payload_digest(&carrier).unwrap();
    let claim = plurx_core::playback::audio::canonical_producer_claim();
    let intent = EncodedOutputIntent {
        target_node_id: "node".into(),
        target_height: 360,
        requested_height: Some(360),
        copy_for_burn: None,
        audio_index: None,
        audio_offset_ms: 0,
        audio_claim: claim,
        audio_delivery: plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::None,
            downmix: None,
            reason: "no source audio".into(),
        },
        subtitle_burn: None,
        subtitle_digest: None,
        hdr10_requested: false,
        grade: OutputGrade::Sdr,
        normalized_geometry: true,
        profile: None,
        width: 640,
        height: 360,
        plan_digest: "a".repeat(64),
        executable_digest: "b".repeat(64),
        engine_digest: "c".repeat(64),
        candidate_id: Some(CandidateId::for_recipe_digest([7; 32])),
        candidate_digest: Some([7; 32]),
    };
    assert!(intent.valid());
    let token = JobToken {
        job_id: uuid::Uuid::new_v4().to_string(),
        node_id: "node".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        fence: 1,
        revision: 1,
        lease_expires_ms: crate::media_sessions::unix_ms() + 60_000,
    };
    let store: Arc<dyn Store> = Arc::new(plurx_core::store::SqliteStore::open_in_memory().unwrap());
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        store,
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        token.clone(),
        tokio::time::Instant::now() + Duration::from_secs(30),
        plurx_core::store::background_jobs::JobKind::EncodedOutputPrepare,
    )
    .unwrap();
    let controller = active.fence();
    let registry = StaticArtifactRuns::default();
    let original = registry
        .reserve(
            token.clone(),
            digest,
            controller.clone(),
            intent.clone(),
            "policy".into(),
            1,
            4096,
        )
        .unwrap();
    let mut renewed = token.clone();
    renewed.revision += 1;
    renewed.lease_expires_ms += 100;
    let resumed = registry
        .reserve(
            renewed.clone(),
            digest,
            controller.clone(),
            intent.clone(),
            "policy".into(),
            1,
            4096,
        )
        .unwrap();
    assert!(
        Arc::ptr_eq(&original, &resumed),
        "renewal resumes the original operation"
    );
    carrier["catalog"]["caps"]["video"][0]["max_height"] = serde_json::json!(180);
    assert!(
        registry
            .reserve(
                renewed,
                static_payload_digest(&carrier).unwrap(),
                controller.clone(),
                intent.clone(),
                "policy".into(),
                1,
                4096
            )
            .is_err(),
        "changed caps cannot borrow an already prepared operation"
    );
    let mut replacement = token;
    replacement.claim_id = "another-claim".into();
    assert!(registry
        .reserve(
            replacement,
            digest,
            controller,
            intent,
            "policy".into(),
            1,
            4096
        )
        .is_err());
    assert!(Arc::ptr_eq(&original, &registry.current().unwrap()));
    active.finish().await;
}
