use super::*;

#[test]
fn prepared_bootstrap_purpose_refuses_voluntary_upward_foreign_and_missing_decoder() {
    use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
    let route = crate::media_sessions::takeover_eligible_route(
        "bootstrap",
        "00000000-0000-4000-8000-0000000000a1",
    );
    let mut manual: crate::media_sessions::RemoteStartRequest =
        serde_json::from_str(&route.recipe_json).expect("durable fixture");
    manual.request.presentation = Presentation::Vod;
    manual.request.automatic = false;
    manual.request.kind = SessionKind::Transcode { height: 720 };
    manual.candidate_id = None;
    let caps = serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls"]})).expect("actual caps shape");
    manual.decoder_caps = Some(
        crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1)
            .expect("usable decoder snapshot"),
    );
    let digest = [42; 32];
    let mut automatic = manual.clone();
    automatic.request.automatic = true;
    automatic.candidate_id = Some(CandidateId::for_recipe_digest(digest));
    automatic.candidate_catalog = Some(crate::media_sessions::CandidateCatalogContext {
        caps,
        candidate: QualityCandidate {
            processed_dv: false,
            id: CandidateId::for_recipe_digest(digest),
            recipe_digest: digest,
            route: CandidateRoute::Encode,
            normalized_geometry: false,
            width: 1280,
            height: 720,
            target_height: 720,
            planned_codec: None,
            average_bps: None,
            peak_bps: None,
            grade: OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: false,
            sustainable: true,
        },
        binding: crate::media_pool::PlanningBinding {
            generation: 1,
            source_digest: "0".repeat(64),
        },
    });
    assert!(
        prepared_manual_auto_bootstrap(&manual, &automatic),
        "bounded canonical transition is a bootstrap"
    );
    assert!(
        !prepared_manual_auto_bootstrap(&automatic, &automatic),
        "an already automatic incumbent cannot grant voluntary handoff"
    );
    let mut changed = automatic.clone();
    changed.request.kind = SessionKind::Transcode { height: 1080 };
    changed
        .candidate_catalog
        .as_mut()
        .expect("catalog")
        .candidate
        .height = 1080;
    assert!(
        !prepared_manual_auto_bootstrap(&manual, &changed),
        "upward proposal needs its own capacity"
    );
    changed = automatic.clone();
    changed.principal =
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 987 };
    assert!(
        !prepared_manual_auto_bootstrap(&manual, &changed),
        "foreign principal cannot borrow an incumbent permit"
    );
    changed = automatic.clone();
    changed.request.playback_id.push_str("-other");
    assert!(
        !prepared_manual_auto_bootstrap(&manual, &changed),
        "another playback is unrelated"
    );
    changed = automatic.clone();
    changed.request.file_id += 1;
    assert!(
        !prepared_manual_auto_bootstrap(&manual, &changed),
        "different source is unrelated"
    );
    let mut no_decoder = manual.clone();
    no_decoder.decoder_caps = None;
    assert!(
        !prepared_manual_auto_bootstrap(&no_decoder, &automatic),
        "legacy metadata is not decoder authority"
    );
    changed = automatic.clone();
    changed.candidate_id = None;
    assert!(
        !prepared_manual_auto_bootstrap(&manual, &changed),
        "catalog presence alone cannot bind canonical output"
    );
    changed = automatic;
    changed
        .candidate_catalog
        .as_mut()
        .expect("catalog")
        .candidate
        .recipe_digest = [24; 32];
    assert!(
        !prepared_manual_auto_bootstrap(&manual, &changed),
        "wrong canonical identity never grants handoff"
    );
}

#[test]
fn cached_recovery_purpose_requires_downward_auto_identity_grade_and_decoder() {
    use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
    let route = crate::media_sessions::takeover_eligible_route(
        "bootstrap",
        "00000000-0000-4000-8000-0000000000a1",
    );
    let mut manual: crate::media_sessions::RemoteStartRequest =
        serde_json::from_str(&route.recipe_json).expect("durable fixture");
    manual.request.presentation = Presentation::Vod;
    manual.request.automatic = false;
    manual.request.kind = SessionKind::Transcode { height: 720 };
    manual.candidate_id = None;
    let caps = serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls"]})).expect("actual caps shape");
    manual.decoder_caps = Some(
        crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1)
            .expect("usable decoder snapshot"),
    );
    let digest = [42; 32];
    let mut automatic = manual.clone();
    automatic.request.automatic = true;
    automatic.candidate_id = Some(CandidateId::for_recipe_digest(digest));
    automatic.candidate_catalog = Some(crate::media_sessions::CandidateCatalogContext {
        caps,
        candidate: QualityCandidate {
            processed_dv: false,
            id: CandidateId::for_recipe_digest(digest),
            recipe_digest: digest,
            route: CandidateRoute::Encode,
            normalized_geometry: false,
            width: 1280,
            height: 720,
            target_height: 720,
            planned_codec: None,
            average_bps: None,
            peak_bps: None,
            grade: OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: false,
            sustainable: true,
        },
        binding: crate::media_pool::PlanningBinding {
            generation: 1,
            source_digest: "0".repeat(64),
        },
    });
    let mut incumbent = automatic.clone();
    incumbent.request.kind = SessionKind::Transcode { height: 1080 };
    incumbent
        .candidate_catalog
        .as_mut()
        .expect("catalog")
        .candidate
        .height = 1080;
    assert!(
        prepared_cached_recovery_recipe_matches(&incumbent, &automatic),
        "an already Auto incumbent may recover to its exact lower candidate"
    );
    assert!(
        prepared_cached_recovery_recipe_matches(&manual, &automatic),
        "the separately validated manual bootstrap keeps its existing purpose"
    );
    assert!(
        !prepared_cached_recovery_recipe_matches(&automatic, &automatic),
        "a same-rung voluntary retry is not pressure recovery"
    );
    assert!(!prepared_cached_recovery_recipe_matches(
        &automatic, &incumbent
    ));
    let mut changed = automatic.clone();
    changed.request.hdr10 = !incumbent.request.hdr10;
    assert!(!prepared_cached_recovery_recipe_matches(
        &incumbent, &changed
    ));
    changed = automatic.clone();
    changed.decoder_caps = None;
    assert!(!prepared_cached_recovery_recipe_matches(
        &incumbent, &changed
    ));
    changed = automatic.clone();
    changed
        .candidate_catalog
        .as_mut()
        .expect("catalog")
        .candidate
        .decoder_compatible = false;
    assert!(!prepared_cached_recovery_recipe_matches(
        &incumbent, &changed
    ));
    changed = automatic.clone();
    changed
        .candidate_catalog
        .as_mut()
        .expect("catalog")
        .candidate
        .recipe_digest = [24; 32];
    assert!(!prepared_cached_recovery_recipe_matches(
        &incumbent, &changed
    ));
    changed = automatic.clone();
    changed.request.playback_id = "foreign-playback".into();
    assert!(!prepared_cached_recovery_recipe_matches(
        &incumbent, &changed
    ));
    changed = automatic.clone();
    changed.principal =
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 987 };
    assert!(!prepared_cached_recovery_recipe_matches(
        &incumbent, &changed
    ));
}

#[tokio::test]
async fn cached_output_attachment_skips_preparation_but_cold_candidate_enqueues() {
    use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
    let (_temp, serve, facts, _) =
        crate::vodserve::retained::test_post_attachment_output_facts().await;
    assert!(facts.response_owner.retained_output_facts().is_some());
    let work_root = crate::test_tempdir().expect("enqueue work root");
    let manager = TranscodeManager::new(
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store")),
        work_root.path().to_path_buf(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    )
    .with_cache(
        work_root.path().join("cache"),
        "fixture-build".into(),
        "fixture".into(),
    );
    let mut receiver = manager
        .output_enqueue
        .receiver
        .lock()
        .expect("queue owner")
        .take()
        .expect("actual bounded enqueue receiver");
    let digest = [42; 32];
    let candidate = QualityCandidate {
        processed_dv: false,
        id: CandidateId::for_recipe_digest(digest),
        recipe_digest: digest,
        route: CandidateRoute::Remux,
        normalized_geometry: false,
        width: 1280,
        height: 720,
        target_height: 720,
        planned_codec: None,
        average_bps: None,
        peak_bps: None,
        grade: OutputGrade::Sdr,
        decoder_compatible: true,
        complete_cache: true,
        sustainable: true,
    };
    let mut request = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: Some(Box::new(TranscodeManager::candidate_context(&candidate))),
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        control_sequence: None,
        file_id: facts.file.id,
        playback_id: "enqueue-fixture".into(),
        request_id: None,
        automatic: true,
        previous_session_id: None,
        reopen_reason: None,
        kind: SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
            dual_layer_as_copied: false,
        },
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
    let context = request
        .candidate_context
        .as_mut()
        .expect("canonical context");
    context.canonical_caps = Some(serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls"]})).expect("caps"));
    context.planning_binding = Some(crate::media_pool::PlanningBinding::from_snapshot(
        &plurx_core::store::PlaybackPlanningSnapshot {
            file: facts.file.clone(),
            probe_json: None,
            settings: Default::default(),
            generation: 1,
            item_title: None,
        },
    ));
    let make_work = |session_id: &str| OutputEnqueue {
        request: request.clone(),
        file: facts.file.clone(),
        settings: crate::vodserve::VodSettings {
            working_set_bytes: 8 << 30,
            completed_cache_bytes: 4 << 30,
            block_budget: Duration::from_secs(30),
            materialize_budget: Duration::from_secs(30),
            blocked_get_cap: 100,
            sdr_master_codecs: false,
            index_cluster_cache: false,
            hevc_unverified_copy: false,
            live_recovery: true,
            output_preparation: crate::vodserve::OutputPreparation::CopyAndEncoded,
            output_budget_bytes: 4 << 30,
        },
        encoding: None,
        session_id: session_id.into(),
        queued_at: Instant::now(),
    };
    manager.hand_off_output_enqueue(make_work("cached"), Some(&facts.response_owner));
    assert!(
        matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "a verified actual cached response publishes no duplicate preparation"
    );
    // The same eligible candidate without actual retained attachment still
    // reaches the real owned worker hand-off, regardless of catalog hints.
    manager.hand_off_output_enqueue(make_work("cold"), None);
    let cold = receiver
        .try_recv()
        .expect("cold candidate offered to worker");
    assert_eq!(cold.session_id, "cold");
    assert_eq!(
        cold.request
            .candidate_context
            .as_ref()
            .expect("candidate")
            .candidate_id,
        candidate.id
    );
    assert_eq!(cold.file.id, facts.file.id);
    assert!(cold.settings.output_preparation.admits(false));
    manager.hand_off_output_enqueue(make_work("rolling-no-facts"), None);
    assert_eq!(
        receiver
            .try_recv()
            .expect("no-artifact fallback still offered")
            .session_id,
        "rolling-no-facts"
    );
    let source_authority = Arc::new(crate::state::AppState::new(
        "fixture".into(),
        Arc::clone(&manager.store),
        crate::state::Dirs {
            artwork: work_root.path().join("artwork"),
            transcode: work_root.path().join("transcode"),
            cache: work_root.path().join("cache"),
            subs: work_root.path().join("subs"),
            runtime_cache: work_root.path().join("runtime"),
            renditions: work_root.path().join("renditions"),
        },
        "fixture".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(64)),
    ));
    let owner = PreparedVodOwner {
        node_id: "fixture".into(),
        source_authority: Some(source_authority),
    };
    manager.hand_off_prepared_output_enqueue(
        make_work("prepared-cached"),
        &owner,
        &facts.response_owner,
    );
    assert!(
        receiver.try_recv().is_err(),
        "actual retained output skips duplicate prepared publication"
    );
    crate::vodserve::retained::test_make_attachment_uncaptured(
        &serve,
        "00000000-0000-4000-8000-00000000cace",
    )
    .await;
    let cold_facts = serve
        .hls_facts("00000000-0000-4000-8000-00000000cace")
        .await
        .expect("cold actual attachment");
    assert!(cold_facts.response_owner.retained_output_facts().is_none());
    let mut cold_work = make_work("prepared-cold");
    let cold_candidate = &mut cold_work
        .request
        .candidate_context
        .as_mut()
        .expect("canonical context")
        .selected_candidate;
    cold_candidate.complete_cache = false;
    cold_candidate.sustainable = false;
    assert!(
        !cold_candidate.playable(),
        "unknown producer capacity is not negative source/recipe authority"
    );
    manager.hand_off_prepared_output_enqueue(cold_work, &owner, &cold_facts.response_owner);
    let prepared = receiver
        .try_recv()
        .expect("local first canonical preparation reaches owned publication worker");
    assert_eq!(prepared.session_id, "prepared-cold");
    assert_eq!(
        prepared
            .request
            .candidate_context
            .as_ref()
            .expect("binding")
            .owner_node_id
            .as_deref(),
        Some("fixture")
    );
    assert!(
        !prepared
            .request
            .candidate_context
            .as_ref()
            .expect("queued candidate")
            .selected_candidate
            .playable(),
        "cold output is queued without inventing measured production"
    );
    assert!(
        request
            .candidate_context
            .as_ref()
            .expect("incumbent binding")
            .owner_node_id
            .is_none(),
        "queue-only binding cannot change the attachment's retained reuse authority"
    );
    let mut invalid_decoder = make_work("invalid-decoder");
    invalid_decoder
        .request
        .candidate_context
        .as_mut()
        .expect("context")
        .selected_candidate
        .decoder_compatible = false;
    manager.hand_off_prepared_output_enqueue(invalid_decoder, &owner, &cold_facts.response_owner);
    let mut invalid_identity = make_work("invalid-identity");
    invalid_identity
        .request
        .candidate_context
        .as_mut()
        .expect("context")
        .selected_candidate
        .recipe_digest = [99; 32];
    manager.hand_off_prepared_output_enqueue(invalid_identity, &owner, &cold_facts.response_owner);
    let foreign_owner = PreparedVodOwner {
        node_id: "foreign".into(),
        source_authority: owner.source_authority.clone(),
    };
    manager.hand_off_prepared_output_enqueue(
        make_work("foreign"),
        &foreign_owner,
        &cold_facts.response_owner,
    );
    let missing_authority = PreparedVodOwner {
        node_id: "fixture".into(),
        source_authority: None,
    };
    manager.hand_off_prepared_output_enqueue(
        make_work("unproven"),
        &missing_authority,
        &cold_facts.response_owner,
    );
    let mut disabled = make_work("disabled");
    disabled.settings.output_preparation = crate::vodserve::OutputPreparation::Off;
    manager.hand_off_prepared_output_enqueue(disabled, &owner, &cold_facts.response_owner);
    let mut split = make_work("split");
    split.request.candidate_context = None;
    manager.hand_off_prepared_output_enqueue(split, &owner, &cold_facts.response_owner);
    assert!(
        receiver.try_recv().is_err(),
        "foreign/unproven/disabled/unattributed work never publishes"
    );
}

/// The peer fences its seal on the predecessor epoch the preparation was
/// admitted under. Binding it to whatever epoch the predecessor holds when
/// the peer reads it let a peer seal (and attach) for a successor whose
/// predecessor had been taken over since staging; only the origin's second
/// seal and the commit CAS caught it, after the work was done.
#[tokio::test]
async fn first_preparation_fences_the_admitted_predecessor_epoch_not_its_current_one() {
    use plurx_core::store::{MediaSessionStore, UserStore};
    let (_temp, serve, _, actual_request) =
        crate::vodserve::retained::test_post_attachment_output_facts().await;
    let id = "00000000-0000-4000-8000-00000000cace";
    let initial_facts = serve.hls_facts(id).await.expect("actual current choice");
    let store = Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("capture store"));
    let user = store
        .create_user("capture", "hash", false)
        .await
        .expect("user");
    let principal =
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: user.id };
    let now = crate::media_sessions::unix_ms();
    let predecessor = uuid::Uuid::new_v4().to_string();
    let activation = plurx_core::domain::MediaSessionActivation {
        incarnation_id: predecessor.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: principal.clone(),
        playback_id: "first-capture".into(),
        recovery_epoch: "capture-epoch".into(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: None,
        request_fingerprint: "a".repeat(64),
        owner_node_id: "node".into(),
        recipe_json: "{}".into(),
        response_json: "{}".into(),
        publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 60_000,
        expected_desired_revision: None,
    };
    assert!(store
        .activate_media_session(&activation)
        .await
        .expect("predecessor")
        .is_some());
    // Published, as a playing predecessor is: only then can it be taken over.
    store
        .settle_media_session_activation(
            &activation,
            plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now,
        )
        .await
        .expect("confirm predecessor")
        .expect("predecessor confirmed");
    let incarnation = uuid::Uuid::new_v4().to_string();
    let fixture = crate::media_sessions::takeover_eligible_route(id, &incarnation);
    let mut recipe: crate::media_sessions::RemoteStartRequest =
        serde_json::from_str(&fixture.recipe_json).expect("typed recipe");
    recipe.principal = principal.clone();
    recipe.retained_output_receiver = Some(1);
    recipe.retained_output = None;
    recipe.request = actual_request;
    recipe.request.request_id = Some(incarnation.clone());
    assert_eq!(recipe.request.start_seconds, 70.841);
    serve.mark_prepared_incarnation(id, &incarnation).await;
    let recipe_json = serde_json::to_string(&recipe).expect("recipe");
    let preparation = plurx_core::domain::MediaSessionPreparation {
        quality_cancellation_key: None,
        incarnation_id: incarnation.clone(),
        session_id: id.into(),
        principal: principal.clone(),
        playback_id: "first-capture".into(),
        expected_predecessor_incarnation_id: predecessor.clone(),
        expected_predecessor_owner_node_id: "node".into(),
        expected_predecessor_owner_epoch: 1,
        request_fingerprint: "b".repeat(64),
        owner_node_id: "node".into(),
        recipe_json: recipe_json.clone(),
        response_json: r#"{"prepared_output_capture_pending":true}"#.into(),
        media_origin_ms: 0,
        now_ms: now,
        expected_desired_revision: None,
        deadline_ms: now + 30_000,
    };
    assert!(store
        .prepare_media_session(&preparation)
        .await
        .expect("reserve")
        .is_some());
    let work = crate::test_tempdir().expect("capture work");
    let mut manager = TranscodeManager::new(
        store.clone(),
        work.path().to_path_buf(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    manager.vod = Arc::clone(&serve);
    let manager = Arc::new(manager);
    // The predecessor's lease lapses and a survivor takes it over: same
    // incarnation, owner epoch 2. The staged successor is still reserved.
    let taken_at = now + 60_001;
    let adopted = store
        .claim_media_session_takeover(&plurx_core::domain::MediaSessionTakeover {
            incarnation_id: predecessor.clone(),
            expected_owner_node_id: "node".into(),
            expected_owner_epoch: 1,
            next_owner_node_id: "survivor".into(),
            now_ms: taken_at,
            lease_expires_at_ms: taken_at + 120_000,
        })
        .await
        .expect("takeover CAS")
        .expect("survivor owns the predecessor");
    assert_eq!(adopted.owner_epoch, 2);
    assert!(store
        .staged_media_session_for_playback(&principal, "first-capture")
        .await
        .expect("ledger")
        .is_some());
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(
        !manager
            .vod_prepare_first_before(
                &recipe_json,
                id,
                user.id,
                PreparedVodOwner {
                    node_id: "node".into(),
                    source_authority: None
                },
                manager.session_adoption_token(id).expect("adoption"),
                Some(preparation.expected_predecessor_owner_epoch),
                deadline
            )
            .await,
        "a successor admitted under predecessor epoch 1 cannot be sealed under epoch 2"
    );
    let route = store
        .media_session_route_by_incarnation(&incarnation)
        .await
        .expect("read")
        .expect("reserved");
    assert_eq!(
        route.response_json, preparation.response_json,
        "the reservation is still pending: nothing was sealed"
    );
    assert!(
        serve
            .response_status_owner_is_current(id, &initial_facts.response_owner)
            .await,
        "the existing attachment is untouched"
    );
    // An older origin sends no epoch; the peer keeps its previous fence.
    assert!(
        manager
            .vod_prepare_first_before(
                &recipe_json,
                id,
                user.id,
                PreparedVodOwner {
                    node_id: "node".into(),
                    source_authority: None
                },
                manager.session_adoption_token(id).expect("legacy adoption"),
                None,
                deadline
            )
            .await,
        "without the field the seal binds the predecessor's current epoch"
    );
}

#[tokio::test]
async fn first_preparation_seals_actual_choice_and_replay_keeps_owner() {
    use plurx_core::store::{MediaSessionStore, UserStore};
    for absence in [false, true] {
        let (_temp, serve, _, actual_request) =
            crate::vodserve::retained::test_post_attachment_output_facts().await;
        let id = "00000000-0000-4000-8000-00000000cace";
        if absence {
            crate::vodserve::retained::test_make_attachment_uncaptured(&serve, id).await;
        }
        let initial_facts = serve.hls_facts(id).await.expect("actual current choice");
        let store =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("capture store"));
        let user = store
            .create_user("capture", "hash", false)
            .await
            .expect("user");
        let principal =
            plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: user.id };
        let now = crate::media_sessions::unix_ms();
        let predecessor = uuid::Uuid::new_v4().to_string();
        let activation = plurx_core::domain::MediaSessionActivation {
            incarnation_id: predecessor.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: principal.clone(),
            playback_id: "first-capture".into(),
            recovery_epoch: "capture-epoch".into(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60_000,
            expected_desired_revision: None,
        };
        assert!(store
            .activate_media_session(&activation)
            .await
            .expect("predecessor")
            .is_some());
        let incarnation = uuid::Uuid::new_v4().to_string();
        let fixture = crate::media_sessions::takeover_eligible_route(id, &incarnation);
        let mut recipe: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&fixture.recipe_json).expect("typed recipe");
        recipe.principal = principal.clone();
        recipe.retained_output_receiver = Some(1);
        recipe.retained_output = None;
        recipe.request = actual_request;
        recipe.request.request_id = Some(incarnation.clone());
        assert_eq!(recipe.request.start_seconds, 70.841);
        serve.mark_prepared_incarnation(id, &incarnation).await;
        let recipe_json = serde_json::to_string(&recipe).expect("recipe");
        let preparation = plurx_core::domain::MediaSessionPreparation {
            quality_cancellation_key: None,
            incarnation_id: incarnation.clone(),
            session_id: id.into(),
            principal: principal.clone(),
            playback_id: "first-capture".into(),
            expected_predecessor_incarnation_id: predecessor.clone(),
            expected_predecessor_owner_node_id: "node".into(),
            expected_predecessor_owner_epoch: 1,
            request_fingerprint: "b".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: recipe_json.clone(),
            response_json: r#"{"prepared_output_capture_pending":true}"#.into(),
            media_origin_ms: 0,
            now_ms: now,
            expected_desired_revision: None,
            deadline_ms: now + 30_000,
        };
        assert!(store
            .prepare_media_session(&preparation)
            .await
            .expect("reserve")
            .is_some());
        let work = crate::test_tempdir().expect("capture work");
        let mut manager = TranscodeManager::new(
            store.clone(),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        manager.vod = Arc::clone(&serve);
        let manager = Arc::new(manager);
        let mut remote_state = crate::http::internal_media_sessions::output_capture_test_state();
        remote_state.store = store.clone();
        remote_state.transcode = Arc::clone(&manager);
        remote_state.node_id = "node".into();
        let deadline = Instant::now() + Duration::from_secs(5);
        // An old peer can return 204 without changing this actual reserved
        // row. That reply must never expose the pending successor.
        let stub_peer_reply = axum::http::StatusCode::NO_CONTENT;
        assert!(
            !manager
                .prepared_prime_is_ready(
                    &preparation,
                    stub_peer_reply == axum::http::StatusCode::NO_CONTENT
                )
                .await,
            "204 with durable pending capture is refused"
        );
        let prepare_request = crate::media_sessions::RemotePrepareRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation.clone(),
            session_id: id.into(),
            principal: principal.clone(),
            expected_owner_epoch: 1,
            expected_predecessor_owner_epoch: None,
        };
        let response = crate::http::internal_media_sessions::prepare_authorized(
            remote_state,
            axum::body::Bytes::from(
                serde_json::to_vec(&prepare_request).expect("remote preparation"),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::NO_CONTENT,
            "remote success waits for actual durable capture"
        );
        assert!(
            manager.prepared_prime_is_ready(&preparation, true).await,
            "local and remote activation wait for the exact durable seal"
        );
        assert!(!manager.prepared_prime_is_ready(&preparation, false).await);
        let sealed = store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .expect("read")
            .expect("sealed");
        let durable: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&sealed.recipe_json).expect("sealed typed recipe");
        assert_eq!(
            durable.retained_output,
            initial_facts.response_owner.retained_output_facts()
        );
        assert_eq!(
            durable.retained_output.is_none(),
            absence,
            "actual private choice, never advisory promotion"
        );
        assert!(
            manager
                .vod_prepare_first_before(
                    &recipe_json,
                    id,
                    user.id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    manager.session_adoption_token(id).expect("repeat adoption"),
                    None,
                    deadline
                )
                .await
        );
        assert!(
            serve
                .response_status_owner_is_current(id, &initial_facts.response_owner)
                .await,
            "repeat leaves actual private attachment unchanged"
        );
        assert_eq!(
            store
                .media_session_route_by_incarnation(&incarnation)
                .await
                .expect("read")
                .expect("same"),
            sealed
        );
        let mut changed_request = recipe.clone();
        changed_request.request.start_seconds += 1.0;
        let changed_recipe = serde_json::to_string(&changed_request).expect("changed request");
        assert!(
            !manager
                .vod_prepare_first_before(
                    &changed_recipe,
                    id,
                    user.id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    manager
                        .session_adoption_token(id)
                        .expect("changed request adoption"),
                    None,
                    deadline
                )
                .await,
            "same incarnation cannot replay a different nonretained recipe"
        );
        let mut changed_receipt = durable.clone();
        changed_receipt.retained_output = Some(RetainedOutputFacts {
            artifact_id: uuid::Uuid::new_v4().to_string(),
            output_identity: "ab".repeat(32),
            average_bps: 8000,
            peak_bps: 12000,
        });
        let changed_receipt = serde_json::to_string(&changed_receipt).expect("changed receipt");
        assert!(
            !manager
                .vod_prepare_first_before(
                    &changed_receipt,
                    id,
                    user.id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    manager
                        .session_adoption_token(id)
                        .expect("changed receipt adoption"),
                    None,
                    deadline
                )
                .await
        );
        // This is the same owner wrapper invoked by local and authenticated remote
        // PREPARE. A foreign owner cannot return a successful preparation receipt.
        assert!(
            !manager
                .vod_prepare_first_before(
                    &recipe_json,
                    id,
                    user.id,
                    PreparedVodOwner {
                        node_id: "foreign".into(),
                        source_authority: None
                    },
                    manager
                        .session_adoption_token(id)
                        .expect("foreign adoption"),
                    None,
                    deadline
                )
                .await
        );
        assert!(
            serve
                .response_status_owner_is_current(id, &initial_facts.response_owner)
                .await
        );
        if let Some(expected) = durable.retained_output.as_ref() {
            crate::vodserve::retained::test_assert_sealed_cached_delivery(&serve, id, expected)
                .await;
        }
        let committed = store
            .commit_media_session_preparation(
                &principal,
                "first-capture",
                &plurx_core::domain::MediaSessionPreparationCommitRequest {
                    staged_incarnation_id: incarnation.clone(),
                    expected_predecessor_owner_node_id: "node".into(),
                    expected_predecessor_owner_epoch: 1,
                    now_ms: crate::media_sessions::unix_ms(),
                    lease_expires_at_ms: now + 60_000,
                    control_receipt: None,
                    expected_desired_revision: None,
                },
            )
            .await
            .expect("commit sealed preparation")
            .expect("committed");
        assert_eq!(
            committed.route.recipe_json, sealed.recipe_json,
            "handoff keeps captured recipe"
        );
        if let Some(expected) = durable.retained_output.as_ref() {
            crate::vodserve::retained::test_assert_sealed_cached_delivery(&serve, id, expected)
                .await;
        }
        assert!(
            !manager
                .vod_prepare_first_before(
                    &recipe_json,
                    id,
                    user.id,
                    PreparedVodOwner {
                        node_id: "node".into(),
                        source_authority: None
                    },
                    manager
                        .session_adoption_token(id)
                        .expect("published adoption"),
                    None,
                    deadline
                )
                .await,
            "published route cannot perform first capture"
        );
        assert!(
            serve
                .response_status_owner_is_current(id, &initial_facts.response_owner)
                .await,
            "refused published repeat cannot kill serving owner"
        );
        serve.end_for_owner(id, &initial_facts.response_owner).await;
    }
}

#[test]
fn idempotent_recovery_preserves_captured_absence_and_exact_artifact_identity() {
    use crate::vodserve::RetainedOutputCapture;
    let original = RetainedOutputFacts {
        artifact_id: uuid::Uuid::new_v4().to_string(),
        output_identity: "ab".repeat(32),
        average_bps: 8000,
        peak_bps: 12000,
    };
    let mut other = original.clone();
    other.artifact_id = uuid::Uuid::new_v4().to_string();
    assert!(recovered_retained_output_matches(
        &RetainedOutputCapture::Restore(None),
        None,
        None
    ));
    assert!(!recovered_retained_output_matches(
        &RetainedOutputCapture::Restore(None),
        None,
        Some(&original)
    ));
    assert!(!recovered_retained_output_matches(
        &RetainedOutputCapture::Restore(Some(original.clone())),
        None,
        None
    ));
    assert!(recovered_retained_output_matches(
        &RetainedOutputCapture::Restore(Some(original.clone())),
        None,
        Some(&original)
    ));
    assert!(!recovered_retained_output_matches(
        &RetainedOutputCapture::Restore(Some(original.clone())),
        None,
        Some(&other)
    ));
    assert!(!recovered_retained_output_matches(
        &RetainedOutputCapture::ReceiverUnavailable,
        None,
        Some(&original)
    ));
    assert!(
        recovered_retained_output_matches(&RetainedOutputCapture::New, None, Some(&original)),
        "new capable replay reads the incumbent without changing it"
    );
    assert!(
        !recovered_retained_output_matches(&RetainedOutputCapture::New, Some(&original), None),
        "a measured selection cannot silently downgrade to old captured None"
    );
    assert!(!recovered_retained_output_matches(
        &RetainedOutputCapture::New,
        Some(&original),
        Some(&other)
    ));
    assert!(recovered_retained_output_matches(
        &RetainedOutputCapture::New,
        Some(&original),
        Some(&original)
    ));
}
