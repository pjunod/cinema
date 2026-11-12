// Review 3 S9: the processed upgrade's own decisions around its trial,
// driven through `run_upgrade_with` on a real route and a live local worker
// (`staging_fixture_for_playback`), with the trial supplied by the test so no
// renderer is needed.

/// A taken upgrade for the fixture's session, as a first frame would take it,
/// after one accepted control exchange reporting the viewer at `position_ms`.
async fn upgrade_after_playing_to(
    fixture: &HlsDeliveryFixture,
    session_id: &str,
    route: &MediaSessionRoute,
    position_ms: i64,
) -> (
    Box<super::processed_upgrade::UpgradeIntent>,
    super::processed_upgrade::UpgradeRun,
    crate::playback_control::ControlRequestV1,
) {
    let mut request = control_request(route.incarnation_id.clone());
    request.position_ms = position_ms;
    request.buffered_from_ms = Some(position_ms.saturating_sub(2_000));
    request.buffered_through_ms = position_ms.saturating_add(20_000);
    let (status, body) = control_body(
        control_local_inner(
            &fixture.state,
            route,
            request.clone(),
            unix_ms().saturating_add(4_000),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the viewer's exchange: {body}");
    let recipe: RemoteStartRequest =
        serde_json::from_str(&route.recipe_json).expect("the route's recipe");
    let user_id = route.principal.local_user_id().expect("a local viewer");
    super::processed_upgrade::register(super::processed_upgrade::UpgradeIntent {
        user_id,
        playback_id: route.playback_id.clone(),
        session_id: session_id.to_owned(),
        incarnation_id: route.incarnation_id.clone(),
        request: recipe.request,
        hdr10_intent: false,
        auto_policy: false,
        network: None,
        network_prior: None,
        incumbent_receipt: None,
    });
    let (intent, run) = super::processed_upgrade::take_on_first_frame(user_id, session_id)
        .expect("the session's first frame takes its upgrade");
    (intent, run, request)
}

/// B1: the create started the film at 0 s and the viewer is ten minutes in.
/// The trial verifies its window at the viewer, never at the create's start
/// (the old code handed it the create's request unchanged, so a successor
/// began fifteen to thirty seconds behind a viewer of a fresh start and the
/// client's alignment refused it), and the staging position is the viewer's.
#[tokio::test]
async fn the_upgrade_positions_its_trial_and_stage_at_the_viewer_not_the_start() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-position");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, _) = upgrade_after_playing_to(&fixture, &session_id, &route, 600_000).await;
    assert_eq!(intent.request.start_seconds, 0.0, "the create started at 0 s");
    let asked = std::sync::Mutex::new(None);
    let outcome = super::processed_upgrade::run_upgrade_with(
        &fixture.state,
        &intent,
        &run,
        |request, _recovery| {
            *asked.lock().expect("asked") = Some(request.start_seconds);
            std::future::ready(Err("test trial".to_owned()))
        },
    )
    .await;
    assert_eq!(outcome, "declined");
    let asked = asked
        .lock()
        .expect("asked")
        .expect("the trial was reached");
    assert!(
        (600.0..=640.0).contains(&asked),
        "the trial's window starts at the viewer (600 s) plus at most its budget, not at \
         the create's start: {asked} s"
    );
    let staged_at = super::processed_upgrade::viewer_film_time_ms(&fixture.state, &intent, &run).await;
    assert!(
        (600_000..630_000).contains(&staged_at),
        "the successor is staged where the viewer is: {staged_at} ms"
    );
}

/// A trial that fails leaves the ordinary session exactly as it was: the same
/// durable route, its worker's preparation gate, no staged successor, the
/// playback's one preparation slot free again, and the session still
/// accepting the viewer's exchanges.
#[tokio::test]
async fn a_failed_trial_leaves_the_predecessors_route_and_actor_untouched() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-trial-failure");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, mut request) =
        upgrade_after_playing_to(&fixture, &session_id, &route, 30_000).await;
    let before = fixture
        .state
        .store
        .media_session_route_by_incarnation(&route.incarnation_id)
        .await
        .expect("route read")
        .expect("the predecessor's route");
    let outcome = super::processed_upgrade::run_upgrade_with(
        &fixture.state,
        &intent,
        &run,
        |_request, _recovery| std::future::ready(Err("the renderer refused the window".to_owned())),
    )
    .await;
    assert_eq!(outcome, "declined");
    drop(run);
    let after = fixture
        .state
        .store
        .media_session_route_by_incarnation(&route.incarnation_id)
        .await
        .expect("route read")
        .expect("the predecessor's route");
    assert_eq!(
        (
            &after.session_id,
            &after.state,
            after.owner_epoch,
            &after.owner_node_id,
            &after.recipe_json,
            &after.response_json,
        ),
        (
            &before.session_id,
            &before.state,
            before.owner_epoch,
            &before.owner_node_id,
            &before.recipe_json,
            &before.response_json,
        ),
        "the predecessor's route is untouched"
    );
    assert!(
        fixture
            .state
            .transcode
            .session_preparation_gate(&session_id)
            .await
            .is_some(),
        "the predecessor's worker still holds its gate"
    );
    assert!(
        fixture
            .state
            .store
            .staged_media_session_for_playback(&route.principal, &route.playback_id)
            .await
            .expect("ledger read")
            .is_none(),
        "nothing was staged"
    );
    assert!(
        super::preparation::begin_processed_upgrade(
            &route.playback_id,
            &tokio_util::sync::CancellationToken::new()
        )
        .is_some(),
        "the playback's preparation slot was released"
    );
    assert!(!super::processed_upgrade::is_waiting(&session_id));
    assert!(!super::processed_upgrade::is_running(&session_id));
    // The real exchange cadence, as the control tests honour it.
    tokio::time::sleep(std::time::Duration::from_millis(260)).await;
    request.sequence = 2;
    request.position_ms = 31_000;
    request.buffered_from_ms = Some(29_000);
    request.buffered_through_ms = 50_000;
    let (status, body) = control_body(
        control_local_inner(
            &fixture.state,
            &route,
            request,
            unix_ms().saturating_add(4_000),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the session still serves its viewer: {body}");
    let response = super::control::public_control_processing(
        super::control::with_current_processing_header(
            &fixture.state,
            &route,
            Json(body).into_response(),
        )
        .await,
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .expect("control body");
    let public: serde_json::Value = serde_json::from_slice(&bytes).expect("public control");
    assert_eq!(
        public["processing_skip"]["generation"],
        route.incarnation_id
    );
    assert_eq!(
        public["processing_skip"]["reason"],
        "the renderer refused the window"
    );
}

/// A pending first-frame trial leaves only a bounded continuation. A later
/// accepted exchange may resume it with a newly planned full binding, not the
/// startup context whose probe metadata the completed receipt changed.
#[tokio::test]
async fn a_late_qualification_resumes_once_with_fresh_authority_and_keeps_its_reason() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("late-fel-receipt");
    let (fixture, session, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (mut intent, run, control) = upgrade_after_playing_to(&fixture, &session, &route, 40_000).await;
    let snapshot = fixture.state.store.playback_planning_snapshot(intent.request.file_id,
        &crate::transcode::QUALITY_PLANNING_KEYS).await.expect("snapshot").expect("source");
    let mut before_receipt = snapshot.clone();
    before_receipt.probe_json = Some("{\"qualification_fixture\":\"before\"}".into());
    let old_binding = crate::media_pool::PlanningBinding::from_snapshot(&before_receipt);
    let fresh_binding = crate::media_pool::PlanningBinding::from_snapshot(&snapshot);
    assert_ne!(old_binding, fresh_binding, "metadata changes invalidate the full startup binding");
    let mut context = crate::transcode::continuous_test_candidate_context(
        plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x74;32]),[0x74;32],1080);
    context.planning_binding=Some(old_binding);
    intent.request.candidate_context=Some(Box::new(context));
    let pending = super::processed_upgrade::pending_qualification_fixture(intent.request.file_id,
        snapshot.file.size,snapshot.file.mtime,snapshot.generation,"metadata:fixture");
    let encoded = crate::transcode::pending_fel_qualification_error(pending);
    assert_eq!(super::processed_upgrade::run_upgrade_with(&fixture.state,&intent,&run,
        |_,_| std::future::ready(Err(encoded))).await,"qualification_pending");
    drop(run);
    assert!(fixture.state.transcode.validate_candidate_planning_binding(&intent.request,None).await.is_err(),
        "reusing the old intent would fail normal binding validation");
    let recipe: RemoteStartRequest=serde_json::from_str(&route.recipe_json).expect("recipe");
    let delivered=crate::playback_control::EffectiveSelection::from_recipe(&recipe,1080,Some("dolby_vision".into()));
    let observed=std::sync::Arc::new(std::sync::Mutex::new(None));
    let resumed=observed.clone();
    let base=recipe.request.clone();
    let expected=fresh_binding.clone();
    super::processed_upgrade::exercise_check(&fixture.state,&route,&recipe,&control.selection,&delivered,
        move |state,wait| async move {
            let current=state.store.playback_planning_snapshot(wait.file_id,&crate::transcode::QUALITY_PLANNING_KEYS)
                .await.expect("fresh authority").expect("source");
            let mut fresh=base;
            let mut context=crate::transcode::continuous_test_candidate_context(
                plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x75;32]),[0x75;32],1080);
            context.planning_binding=Some(crate::media_pool::PlanningBinding::from_snapshot(&current));
            fresh.candidate_context=Some(Box::new(context));
            assert!(state.transcode.validate_candidate_planning_binding(&fresh,None).await.is_ok());
            Ok(Some(fresh))
        },
        move |intent,run,pending| {
            let context=intent.request.candidate_context.as_ref().expect("fresh context");
            assert_eq!(context.planning_binding,Some(expected));
            assert!(context.required_fel_qualification.is_some(),"the resumed trial is restricted to its exact receipt");
            assert!(super::processed_upgrade::processing_skip(&intent.session_id,&intent.incarnation_id)
                .expect("explanation until replacement").reason.contains("until replacement"));
            *resumed.lock().expect("resumed")=Some(intent.request);
            drop(pending);drop(run);
        }).await;
    assert!(observed.lock().expect("observed").is_some());
    // Another accepted exchange cannot run the same completed continuation.
    super::processed_upgrade::exercise_check(&fixture.state,&route,&recipe,&control.selection,&delivered,
        |_,_| async { panic!("duplicate control must not resolve or measure again") },
        |_,_,_| panic!("duplicate continuation")).await;
    super::processed_upgrade::cancel_for_session(&session);
}

#[tokio::test]
async fn a_viewer_change_cancels_a_pending_receipt_check_before_it_can_resume() {
    let dir=crate::test_tempdir().expect("state dir");
    let playback=unique_playback_id("late-fel-cancel");
    let (fixture,session,route)=staging_fixture_for_playback(dir.path(),&playback).await;
    let (intent,run,control)=upgrade_after_playing_to(&fixture,&session,&route,10_000).await;
    let snapshot=fixture.state.store.playback_planning_snapshot(intent.request.file_id,&crate::transcode::QUALITY_PLANNING_KEYS)
        .await.expect("snapshot").expect("source");
    let encoded=crate::transcode::pending_fel_qualification_error(super::processed_upgrade::pending_qualification_fixture(
        intent.request.file_id,snapshot.file.size,snapshot.file.mtime,snapshot.generation,"metadata:fixture"));
    assert_eq!(super::processed_upgrade::run_upgrade_with(&fixture.state,&intent,&run,
        |_,_|std::future::ready(Err(encoded))).await,"qualification_pending");drop(run);
    let recipe:RemoteStartRequest=serde_json::from_str(&route.recipe_json).expect("recipe");
    let delivered=crate::playback_control::EffectiveSelection::from_recipe(&recipe,1080,Some("dolby_vision".into()));
    let (started_tx,started_rx)=tokio::sync::oneshot::channel();
    let state=fixture.state.clone();let route_for_check=route.clone();let selection=control.selection.clone();
    let check=tokio::spawn(async move {
        super::processed_upgrade::exercise_check(&state,&route_for_check,&recipe,&selection,&delivered,
            |_,_| async move {
                let _=started_tx.send(());
                std::future::pending::<Result<Option<crate::transcode::SessionRequest>,String>>().await
            },|_,_,_|panic!("a viewer-cancelled check cannot stage an enhancement")).await;
    });
    started_rx.await.expect("check claimed its preparation slot");
    let viewer=super::preparation::PendingCandidateGuard::begin(&playback,"new-viewer-selection");
    tokio::time::timeout(std::time::Duration::from_secs(2),check).await.expect("bounded cancellation").expect("check task");
    assert!(!viewer.cancelled(),"the upgrade never cancels the viewer's request");
    assert!(super::processed_upgrade::processing_skip(&session,&route.incarnation_id).is_some(),
        "cancellation never clears the truthful ordinary-delivery explanation");
    drop(viewer);super::processed_upgrade::cancel_for_session(&session);
}


#[tokio::test]
async fn an_ordinary_fallback_reports_the_reason_without_private_qualification_data() {
    let dir=crate::test_tempdir().expect("state dir");
    let playback=unique_playback_id("private-fel-token");
    let (fixture,_,_)=staging_fixture_for_playback(dir.path(),&playback).await;
    let mut file=fixture.store.get_file(fixture.file_id()).await.expect("file read").expect("source");
    file.hdr=Some("dolby_vision".into());
    file.dolby_vision.profile=Some(7);file.dolby_vision.el_present=Some(true);
    let wait=super::processed_upgrade::pending_qualification_fixture(file.id,file.size,file.mtime,0,"private-held-object");
    let private=crate::transcode::pending_fel_qualification_error(wait);
    let public=crate::transcode::ordinary_processing_decline(private);
    let kind=crate::transcode::SessionKind::Copy {aac:false,preserve_dolby_vision:true,
        convert_dolby_vision:true,dual_layer_as_copied:false};
    let enhancement=session_enhancement(Some(&file),&kind,plurx_core::transcode::OutputGrade::Hdr10,
        None,None,None,None,Some(&public)).expect("DV explanation");
    let wire=serde_json::to_string(&enhancement).expect("public enhancement");
    assert!(wire.contains("quality measurement (queued or running)"));
    for private in ["private-held-object","source_object_version","settings_generation","backend_identity","fel_qualification_pending"] {
        assert!(!wire.contains(private),"private continuation data must not enter Playback Info: {wire}");
    }
    assert_eq!(crate::transcode::ordinary_processing_decline("the decoder refused the window".into()),"the decoder refused the window");
}

/// A repeat upgrade of the same playback: this node described the processed
/// recipe on an earlier trial, and nothing is producing it now, so the
/// description carries no production proof. That is what the trial measures,
/// so it never refuses the trial; Auto's link evidence alone decides before
/// it (Avatar on the Google TV: every resume after the first upgrade was
/// refused as "link evidence" before any trial ran).
#[tokio::test]
async fn a_description_without_production_proof_still_reaches_the_trial() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-repeat-description");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (mut intent, run, _) = upgrade_after_playing_to(&fixture, &session_id, &route, 60_000).await;
    let source = fixture
        .store
        .get_file(intent.request.file_id)
        .await
        .expect("file read")
        .expect("source");
    let height = u32::try_from(source.height.expect("the fixture's source height") & !1)
        .expect("height");
    intent.auto_policy = true;
    intent.request.candidate_context = Some(Box::new(
        crate::transcode::continuous_test_candidate_context(
            plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x31; 32]),
            [0x31; 32],
            height,
        ),
    ));
    let mut described = crate::transcode::continuous_test_candidate_context(
        plurx_core::playback::candidate::CandidateId::for_recipe_digest([0x32; 32]),
        [0x32; 32],
        height,
    )
    .selected_candidate;
    described.processed_dv = true;
    described.planned_codec = Some("hevc".into());
    described.complete_cache = false;
    described.sustainable = false;
    assert!(
        !crate::http::hls::processed_candidate_is_admitted(
            std::slice::from_ref(&described),
            &described
        ),
        "the description has no current production proof"
    );
    intent.request.quality_catalog = Some(std::sync::Arc::new(
        crate::media_pool::QualityCatalogResult {
            candidates: vec![crate::media_pool::WorkerQualityCandidate {
                node_id: fixture.state.node_id.clone(),
                candidate: described,
                binding: None,
                partial: false,
                dispatch_supported: true,
            }],
            authority_refused: false,
            complete: true,
            causes: Vec::new(),
        },
    ));
    let reached = std::sync::atomic::AtomicBool::new(false);
    let outcome = super::processed_upgrade::run_upgrade_with(
        &fixture.state,
        &intent,
        &run,
        |_request, _recovery| {
            reached.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::ready(Err("test trial".to_owned()))
        },
    )
    .await;
    assert_eq!(outcome, "declined", "the trial decides, not the missing proof");
    assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
}
