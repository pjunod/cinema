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
/// client's alignment refused it), and the successor is staged at that
/// window, never behind the viewer.
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn auto_fel_refusal_reports_when_peer_selection_never_started() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("fel-peer-not-checked");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (mut intent, run, _) =
        upgrade_after_playing_to(&fixture, &session_id, &route, 30_000).await;
    let mut create = bare_create();
    create.intent = Some(envelope(
        1,
        plurx_core::playback::DesiredQuality::Auto {
            height: None,
            candidate_id: None,
        },
    ));
    intent.auto_policy = create.candidate_auto_policy();
    assert!(intent.auto_policy);
    let before = fixture
        .state
        .store
        .media_session_route_by_incarnation(&route.incarnation_id)
        .await
        .expect("route read");
    let local_reason = crate::transcode::TranscodeManager::reconstruction_slot_busy();
    let refusal = crate::transcode::vod_refusal_error(
        crate::media_sessions::ADAPTIVE_PROCESSED_CAPACITY_REFUSED,
        &local_reason,
    );
    let outcome = super::processed_upgrade::run_upgrade_with(
        &fixture.state,
        &intent,
        &run,
        |_request, _recovery| async {
            tokio::time::pause();
            tokio::time::advance(crate::transcode::PROCESSED_UPGRADE_TRIAL_BUDGET).await;
            tokio::time::resume();
            Err(refusal)
        },
    )
    .await;
    assert_eq!(outcome, "declined");
    let skip = super::processed_upgrade::processing_skip(&session_id, &route.incarnation_id)
        .expect("the current ordinary generation has the viewer-safe reason");
    assert_eq!(
        skip.reason,
        format!(
            "{local_reason}. No same-quality peer was checked under the original preparation deadline."
        )
    );
    assert_eq!(
        fixture
            .state
            .store
            .media_session_route_by_incarnation(&route.incarnation_id)
            .await
            .expect("ordinary route read after the refusal"),
        before
    );
    assert!(fixture
        .state
        .store
        .staged_media_session_for_playback(&route.principal, &route.playback_id)
        .await
        .expect("prepared route read")
        .is_none());
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
        move |_,_| std::future::ready(Err(encoded.clone()))).await,"qualification_pending");
    drop(run);
    assert!(fixture.state.transcode.validate_candidate_planning_binding(&intent.request,None,None).await.is_err(),
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
            assert!(state.transcode.validate_candidate_planning_binding(&fresh,None,None).await.is_ok());
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
        move |_,_|std::future::ready(Err(encoded.clone()))).await,"qualification_pending");drop(run);
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
    // A 4K source: the projection only describes a source with a height.
    let unprobed = fixture
        .store
        .get_file(intent.request.file_id)
        .await
        .expect("file read")
        .expect("source");
    fixture
        .store
        .upsert_file(
            unprobed.item_id,
            unprobed.path.to_str().expect("fixture source path"),
            unprobed.size,
            unprobed.mtime,
            &plurx_core::domain::ProbeResult {
                duration_ms: unprobed.duration_ms,
                width: Some(3840),
                height: Some(2160),
                ..plurx_core::domain::ProbeResult::default()
            },
        )
        .await
        .expect("probe the fixture's source");
    let source = fixture
        .store
        .get_file(intent.request.file_id)
        .await
        .expect("file read")
        .expect("source");
    let height = 2160;
    assert_eq!(source.height, Some(2160));
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
        !crate::http::hls::processed_candidate_kept(
            std::slice::from_ref(&described),
            &described
        ).is_some_and(|row| row.playable()),
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
    assert!(
        super::processed_upgrade::projected_processed_candidate(&fixture.state, &intent, &source)
            .is_some(),
        "the admission before the trial sees the description"
    );
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

/// Scheduling a future prepared window happens before client overlap, and
/// remains owned by the predecessor's ordinary cancellation lifecycle.
#[tokio::test]
async fn a_scheduled_processed_handoff_is_cancelled_with_its_predecessor() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-scheduled-cancel");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, _) = upgrade_after_playing_to(&fixture, &session_id, &route, 600_000).await;
    let pending = super::preparation::begin_processed_upgrade(&playback, run.cancelled())
        .expect("the upgrade holds the preparation slot");
    let window = 620_000..622_000;
    let scheduling = super::processed_upgrade::await_successor_start(
        &fixture.state, &intent, &run, &pending, Some(&window), None,
    );
    tokio::pin!(scheduling);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut scheduling)
        .await.is_err(), "a future entry waits instead of staging earlier media");
    super::processed_upgrade::cancel_for_session(&session_id);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1), &mut scheduling)
        .await.expect("cancellation wakes the scheduler"), Err("cancelled"));
}

#[tokio::test]
async fn a_scheduled_processed_handoff_meets_the_actual_prepared_entry() {
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-scheduled-entry");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, mut request) =
        upgrade_after_playing_to(&fixture, &session_id, &route, 600_000).await;
    let pending = super::preparation::begin_processed_upgrade(&playback, run.cancelled())
        .expect("the upgrade holds the preparation slot");
    let window = 620_000..622_000;
    let scheduling = super::processed_upgrade::await_successor_start(
        &fixture.state, &intent, &run, &pending, Some(&window), None,
    );
    tokio::pin!(scheduling);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(20), &mut scheduling)
        .await.is_err());
    // Honour the real control exchange cadence, then let the ordinary
    // incumbent approach the prepared entry before opening client overlap.
    tokio::time::sleep(std::time::Duration::from_millis(260)).await;
    request.sequence = 2;
    request.position_ms = 616_000;
    request.buffered_from_ms = Some(614_000);
    request.buffered_through_ms = 640_000;
    let (status, body) = control_body(control_local_inner(
        &fixture.state, &route, request, unix_ms().saturating_add(4_000),
    ).await).await;
    assert_eq!(status, StatusCode::OK, "the incumbent keeps playing: {body}");
    let (position, waited) = tokio::time::timeout(std::time::Duration::from_secs(2), &mut scheduling)
        .await.expect("the incumbent reached the handoff lead").expect("the session is current");
    assert_eq!(position, window.start, "staging uses produced media, not an earlier clamp");
    assert!(waited);
}

/// Exercise the real priming arm after its final planning awaits. An accepted
/// incumbent wait cancels only the pending child; releasing the session cancels
/// the parent too. The stage must preserve that distinction through the same
/// completion path production uses, without reserving or priming a successor.
#[tokio::test]
async fn a_late_staging_cancellation_rearms_only_a_live_processed_upgrade() {
    use super::preparation::{
        AcceptedAsk, PreparationPurpose, PreparationStageOutcome, StagedSuccessorIds,
    };
    for release_parent in [false, true] {
        let dir = crate::test_tempdir().expect("state dir");
        let playback = unique_playback_id("upgrade-late-stage-cancel");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback).await;
        let (intent, run, mut control) =
            upgrade_after_playing_to(&fixture, &session_id, &route, 30_000).await;
        let pending = super::preparation::begin_processed_upgrade(&playback, run.cancelled())
            .expect("the trial owns the pending claim");
        let cancellation = pending.cancellation_token();
        let registration = pending.registration();
        let source = staging_source(&fixture).await;
        let predecessor: RemoteStartRequest =
            serde_json::from_str(&route.recipe_json).expect("predecessor recipe");
        let candidate = intent.request.clone();
        let ids = StagedSuccessorIds::mint();
        let staged_incarnation = ids.incarnation_id.clone();
        super::hooks::prime_prepared_successors(&fixture.state, true);
        let pause = super::hooks::pause_before_preparation_registered(&fixture.state, &playback);
        let state = fixture.state.clone();
        let staging_route = route.clone();
        let staging_session = session_id.clone();
        let staging = tokio::spawn(async move {
            super::preparation::stage_prepared_successor_as(
                &state,
                &staging_session,
                &staging_route,
                &predecessor,
                &candidate,
                Some(&source),
                &state.node_id,
                PreparationPurpose::SelectionChange,
                AcceptedAsk {
                    prepared_proof: None,
                    film_time_ms: 30_000,
                    desired_digest: None,
                    planning_cancellation: Some(cancellation),
                    planning_registration: Some(registration),
                    quality_intent: None,
                },
                true,
                ids,
            )
            .await
        });
        let held = tokio::time::timeout(Duration::from_secs(2), pause.reached())
            .await
            .expect("the real stage reached its registration seam");
        if release_parent {
            super::processed_upgrade::cancel_for_session(&session_id);
        } else {
            // Honour the real control cadence. This is an accepted wait from
            // the same generation, not a direct test-only token cancellation.
            tokio::time::sleep(Duration::from_millis(260)).await;
            control.sequence += 1;
            control.render_state = crate::playback_control::RenderState::Waiting;
            let (status, body) = control_body(
                control_local_inner(
                    &fixture.state,
                    &route,
                    control,
                    unix_ms().saturating_add(4_000),
                )
                .await,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "the incumbent wait was accepted: {body}"
            );
        }
        assert!(pending.cancelled(), "the pending claim lost its owner");
        assert_eq!(run.cancelled().is_cancelled(), release_parent);
        held.release();
        let staged = tokio::time::timeout(Duration::from_secs(2), staging)
            .await
            .expect("staging finishes")
            .expect("staging task");
        assert_eq!(staged, PreparationStageOutcome::Cancelled);
        assert!(super::preparation::active_preparation(&staged_incarnation).is_none());
        assert!(
            fixture
                .store
                .media_session_route_by_incarnation(&staged_incarnation)
                .await
                .expect("successor read")
                .is_none(),
            "cancelled before reservation"
        );
        assert!(fixture
            .store
            .staged_media_session_for_playback(&route.principal, &playback)
            .await
            .expect("ledger read")
            .is_none());
        let current = fixture
            .store
            .media_session_route_for_playback(&route.principal, &playback)
            .await
            .expect("pointer read")
            .expect("the incumbent remains");
        assert_eq!(current.incarnation_id, route.incarnation_id);
        assert_eq!(current.owner_epoch, route.owner_epoch);
        assert!(fixture
            .state
            .transcode
            .session_preparation_gate(&session_id)
            .await
            .is_some());
        let outcome = run.finish_staging(staged);
        assert_eq!(outcome, "cancelled");
        drop(pending);
        let completed = super::processed_upgrade::finish_upgrade_run(intent, run, outcome);
        assert_eq!(
            completed,
            if release_parent {
                "cancelled"
            } else {
                "displaced"
            }
        );
        assert!(
            super::processed_upgrade::processing_skip(&session_id, &route.incarnation_id).is_none()
        );
        assert_eq!(
            super::processed_upgrade::is_waiting(&session_id),
            !release_parent
        );
        if !release_parent {
            // This is the exact take hook invoked by the next accepted
            // Rendering exchange. It can take this generation once only.
            let user = route.principal.local_user_id().expect("local viewer");
            let (_, next_run) = super::processed_upgrade::take_on_rendering(
                user,
                &session_id,
                &route.incarnation_id,
            )
            .expect("the live generation may try again when rendering");
            assert!(
                super::processed_upgrade::take_on_rendering(
                    user,
                    &session_id,
                    &route.incarnation_id,
                )
                .is_none(),
                "no duplicate rendering run"
            );
            super::processed_upgrade::cancel_for_session(&session_id);
            drop(next_run);
        }
    }
}

/// Cleanup cancels the pending child after a real reservation refusal too.
/// That later cancellation must never turn the refused stage into a retry.
#[tokio::test]
async fn a_refused_upgrade_reservation_is_not_rearmed_by_its_cleanup() {
    use super::preparation::{
        AcceptedAsk, PreparationPurpose, PreparationStageOutcome, PreparationStageRefusal,
        StagedSuccessorIds,
    };
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-stage-refused");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, _) = upgrade_after_playing_to(&fixture, &session_id, &route, 30_000).await;
    let pending = super::preparation::begin_processed_upgrade(&playback, run.cancelled())
        .expect("the trial owns the pending claim");
    let source = staging_source(&fixture).await;
    let predecessor: RemoteStartRequest = serde_json::from_str(&route.recipe_json).expect("recipe");
    let mut stale_owner = route.clone();
    stale_owner.owner_epoch += 1;
    let ids = StagedSuccessorIds::mint();
    let staged_incarnation = ids.incarnation_id.clone();
    let staged = super::preparation::stage_prepared_successor_as(
        &fixture.state,
        &session_id,
        &stale_owner,
        &predecessor,
        &intent.request,
        Some(&source),
        &fixture.state.node_id,
        PreparationPurpose::SelectionChange,
        AcceptedAsk {
            prepared_proof: None,
            film_time_ms: 30_000,
            desired_digest: None,
            planning_cancellation: Some(pending.cancellation_token()),
            planning_registration: Some(pending.registration()),
            quality_intent: None,
        },
        true,
        ids,
    )
    .await;
    // The existing five-second phase includes cleanup of the refused owner.
    // Either its known refusal or its settlement deadline stays terminal; a
    // cleanup token must not turn either result into a rendering retry.
    assert!(matches!(
        staged,
        PreparationStageOutcome::Refused(
            PreparationStageRefusal::ReservationRefused
                | PreparationStageRefusal::ReservationSettlementDeadline
        )
    ));
    assert!(
        pending.cancelled(),
        "the detached refusal owner performs its cleanup"
    );
    assert!(
        !run.cancelled().is_cancelled(),
        "the predecessor did not end"
    );
    assert!(super::preparation::active_preparation(&staged_incarnation).is_none());
    assert!(fixture
        .store
        .media_session_route_by_incarnation(&staged_incarnation)
        .await
        .expect("successor read")
        .is_none());
    let current = fixture
        .store
        .media_session_route_for_playback(&route.principal, &playback)
        .await
        .expect("pointer read")
        .expect("the incumbent remains");
    assert_eq!(current.incarnation_id, route.incarnation_id);
    assert_eq!(
        current.owner_epoch, route.owner_epoch,
        "stale staging changed no owner fence"
    );
    let outcome = run.finish_staging(staged);
    assert_eq!(outcome, "not_staged");
    drop(pending);
    assert_eq!(
        super::processed_upgrade::finish_upgrade_run(intent, run, outcome),
        "not_staged"
    );
    assert!(!super::processed_upgrade::is_waiting(&session_id));
    assert!(
        super::processed_upgrade::processing_skip(&session_id, &route.incarnation_id).is_some()
    );
    assert!(
        super::processed_upgrade::take_on_rendering(
            route.principal.local_user_id().expect("local viewer"),
            &session_id,
            &route.incarnation_id,
        )
        .is_none(),
        "a real refusal has no rendering retry"
    );
    super::processed_upgrade::cancel_for_session(&session_id);
}

/// The Air (Profile 5) path end to end through the upgrade: the trial is
/// refused for capacity its own viewer's incumbent holds (6 threads + slot of
/// an 11-thread, 2-slot node, trial 9 + slot), the upgrade claims that permit,
/// and only with it reserved runs the trial again, which then admits through
/// the same `admit_processed_trial` the create path calls — with no pool
/// waiter at any point.
#[tokio::test]
async fn a_trial_refused_by_its_own_incumbent_is_retried_with_that_permit() {
    use crate::admission::{Admissions, Priority, TranscodeResourceEstimate};
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-incumbent-claim");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, _) = upgrade_after_playing_to(&fixture, &session_id, &route, 600_000).await;
    let admissions = Admissions::new();
    let trial = TranscodeResourceEstimate { hardware_slot: true, cpu_threads: 9, decoder_threads: Some(1) };
    let incumbent = std::sync::Arc::new(std::sync::Mutex::new(Some(
        admissions
            .try_admit_bundle(2, 11, &TranscodeResourceEstimate {
                hardware_slot: true, cpu_threads: 6, decoder_threads: None }, Priority::Live)
            .expect("the viewer's encoded incumbent"),
    )));
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let admitted = std::sync::Arc::new(std::sync::Mutex::new(None));
    let outcome = super::processed_upgrade::run_upgrade_with_incumbent(
        &fixture.state,
        &intent,
        &run,
        |_request, _recovery| {
            let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (admissions, admitted) = (admissions.clone(), std::sync::Arc::clone(&admitted));
            async move {
                let result = crate::vodencode::admit_processed_trial(&admissions, 2, 11, &trial,
                    std::time::Instant::now() + std::time::Duration::from_secs(1)).await;
                assert_eq!(admissions.snapshot().live_waiting, 0, "call {call}: no pool waiter");
                match result {
                    Ok(permit) => {
                        *admitted.lock().expect("admitted") = Some(permit);
                        Err("the test stops after admission".to_owned())
                    }
                    Err(_) => Err("DV processing needs a GPU slot and 9 CPU threads that other work holds".to_owned()),
                }
            }
        },
        |_incarnation, needed| {
            assert_eq!(needed, trial, "the claim asks for exactly the refused bundle");
            let (admissions, incumbent) = (admissions.clone(), std::sync::Arc::clone(&incumbent));
            async move {
                // The incumbent's driver yields and reserves its capacity.
                drop(incumbent.lock().expect("incumbent").take());
                let token = admissions.reserve_for_handoff(&needed, std::time::Duration::from_secs(5));
                Some(crate::vodserve::IncumbentPermitClaim::detached_for_test(
                    crate::vodserve::TrialClaim::reserved_for_test(token), admissions))
            }
        },
    )
    .await;
    assert_eq!(outcome, "declined", "the test's own stop after admission");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "refused, then retried once");
    assert!(admitted.lock().expect("admitted").is_some(), "the retry admitted with the claim");
    assert_eq!(admissions.snapshot().reservations, 0, "the reservation was consumed");
}

/// A claim that closes during the retried trial (a seek past the incumbent's
/// runway) drops the trial at once, so its capacity goes back now rather than
/// at the trial's own deadline.
#[tokio::test]
async fn a_claim_closed_mid_trial_drops_the_retried_trial() {
    use crate::admission::{Admissions, Priority, TranscodeResourceEstimate};
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("upgrade-incumbent-closed");
    let (fixture, session_id, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let (intent, run, _) = upgrade_after_playing_to(&fixture, &session_id, &route, 600_000).await;
    let admissions = Admissions::new();
    let trial = TranscodeResourceEstimate { hardware_slot: true, cpu_threads: 9, decoder_threads: Some(1) };
    let _incumbent = admissions
        .try_admit_bundle(2, 11, &TranscodeResourceEstimate {
            hardware_slot: true, cpu_threads: 6, decoder_threads: None }, Priority::Live)
        .expect("incumbent");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let claimed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let upgrade = super::processed_upgrade::run_upgrade_with_incumbent(
        &fixture.state,
        &intent,
        &run,
        |_request, _recovery| {
            let first = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let admissions = admissions.clone();
            async move {
                if first {
                    let _ = crate::vodencode::admit_processed_trial(&admissions, 2, 11, &trial,
                        std::time::Instant::now() + std::time::Duration::from_secs(1)).await;
                    Err("refused".to_owned())
                } else {
                    std::future::pending().await
                }
            }
        },
        |_incarnation, _needed| {
            let (admissions, claimed) = (admissions.clone(), std::sync::Arc::clone(&claimed));
            async move {
                let claim = crate::vodserve::TrialClaim::reserved_for_test(0);
                *claimed.lock().expect("claimed") = Some(std::sync::Arc::clone(&claim));
                Some(crate::vodserve::IncumbentPermitClaim::detached_for_test(claim, admissions))
            }
        },
    );
    let closer = async {
        loop {
            if calls.load(std::sync::atomic::Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        claimed.lock().expect("claimed").as_ref().expect("claim").close_for_test();
    };
    let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(10),
        async { tokio::join!(upgrade, closer) }).await.expect("the closed claim ends the trial");
    assert_eq!(outcome, "declined");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Exercise the actual staging/reservation and announcement boundary. The
/// optional processing facts are the root owner's input to this boundary;
/// descriptive candidates and requested destinations are deliberately identical
/// in the negative case and cannot replace that input.
async fn staged_processing_delivery_round_trip(profile81: bool, verified: bool) {
    use super::preparation::{
        AcceptedAsk, PreparationPurpose, PreparationStageOutcome, StagedProcessingDelivery,
        StagedSuccessorIds,
    };
    use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
    use plurx_core::transcode::{OutputGrade, dv_processing::DvDestination};
    let dir = crate::test_tempdir().expect("state dir");
    let playback = unique_playback_id("staged-processing-delivery");
    let (fixture, session, route) = staging_fixture_for_playback(dir.path(), &playback).await;
    let old = fixture
        .store
        .get_file(fixture.file_id())
        .await
        .expect("source read")
        .expect("fixture source");
    let mut dv = dolby_vision_p7_file().dolby_vision;
    dv.el_present = Some(true);
    dv.rpu_present = Some(true);
    fixture
        .store
        .upsert_file(
            old.item_id,
            old.path.to_str().expect("source path"),
            old.size,
            old.mtime,
            &plurx_core::domain::ProbeResult {
                duration_ms: old.duration_ms,
                container: Some("mkv".into()),
                video_codec: Some("hevc".into()),
                video_profile: Some("Main 10".into()),
                width: Some(3840),
                height: Some(2160),
                bit_depth: Some(10),
                hdr: Some("dolby_vision".into()),
                dolby_vision: dv,
                audio_streams: vec![plurx_core::domain::AudioStream {
                    index: 2,
                    codec: "eac3".into(),
                    channels: Some(6),
                    sample_rate: Some(48_000),
                    channel_layout: Some("5.1".into()),
                    language: None,
                    title: None,
                    default: true,
                }],
                ..Default::default()
            },
        )
        .await
        .expect("source DV metadata");
    fixture
        .store
        .put_setting(plurx_core::store::keys::DV_HDR_PROCESSING, "1")
        .await
        .expect("processing enabled");
    fixture
        .store
        .put_setting(plurx_core::store::keys::PLAYBACK_DISPLAY_AWARE_AUTO, "1")
        .await
        .expect("route-v1 owner enabled before planning snapshot");
    let snapshot = fixture
        .store
        .playback_planning_snapshot(fixture.file_id(), &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("planning read")
        .expect("planning snapshot");
    let source = &snapshot.file;
    assert_eq!(
        source.dolby_vision.profile,
        Some(7),
        "typed source DV facts survived the store round trip"
    );
    let caps = caps_v2(
        r#"{"v":2,"video":[{"codec":"hevc","profiles":["main10"],"dv_profiles":[8],"present":["sdr","pq"],"max_width":3840,"max_height":2160,"max_frame_rate":{"numerator":24,"denominator":1}}],
        "containers":["mkv","mp4"],"transports":["hls"],"dv_transport":"hls","hls_hevc_sample_entries":["hvc1"],"audio":["aac","eac3"],
        "audio_sinks":[{"codec":"aac","max_channels":2,"sample_rates_hz":[48000]},
            {"codec":"eac3","max_channels":6,"sample_rates_hz":[48000],"passthrough":true}]}"#,
    );
    let body = CreateSession {
        playback_id: playback.clone(),
        copy: Some(true),
        audio: Some(2),
        caps: Some(caps.clone()),
        ..bare_create()
    };
    let query = body.selection_catalog_request(source, &caps, body.reviewed_copy_scope(None));
    let candidate = QualityCandidate {
        id: CandidateId::for_recipe_digest([0x8a; 32]),
        recipe_digest: [0x8a; 32],
        route: CandidateRoute::Encode,
        planned_codec: Some("hevc".into()),
        processed_dv: true,
        normalized_geometry: true,
        width: 3840,
        height: 2160,
        target_height: 2160,
        average_bps: Some(12_000_000),
        peak_bps: Some(18_000_000),
        grade: OutputGrade::Hdr10,
        decoder_compatible: true,
        complete_cache: false,
        sustainable: true,
    };
    let mut context = crate::transcode::TranscodeManager::candidate_context(&candidate);
    context.catalog_query = Some(query);
    context.canonical_caps = Some(caps.clone());
    context.planning_binding = Some(crate::media_pool::PlanningBinding::from_snapshot(&snapshot));
    context.planning_snapshot = Some(Arc::new(snapshot.clone()));
    let claim = plurx_core::playback::audio::AudioClaim::from_caps(&caps)
        .expect("audio caps")
        .expect("audio claim");
    let audio = plurx_core::playback::audio::resolve_audio(
        source.audio_streams.first(),
        &claim.profile(),
        plurx_core::playback::audio::AudioRoute::EncodedVod,
        0,
    );
    let processed = crate::transcode::SessionRequest {
        file_id: source.id,
        playback_id: playback.clone(),
        kind: crate::transcode::SessionKind::Transcode { height: 2160 },
        audio_index: Some(2),
        audio_claim: Some(claim),
        audio_delivery: Some(audio),
        hdr10: true,
        dv_processing_destination: Some(if profile81 {
            DvDestination::Profile81
        } else {
            DvDestination::Hdr10
        }),
        candidate_context: Some(Box::new(context)),
        ..staged_candidate_request()
    };
    fixture
        .state
        .transcode
        .remember_processed_candidate(&processed);
    let decoder = crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1)
        .expect("decoder snapshot");
    let mut predecessor = staged_predecessor_recipe(&route);
    predecessor.decoder_caps = Some(decoder.clone());
    let ids = StagedSuccessorIds::mint();
    let incarnation = ids.incarnation_id.clone();
    let successor_session = ids.session_id.clone();
    let outcome = super::preparation::stage_prepared_successor_with_delivery_as(
        &fixture.state,
        &session,
        &route,
        &predecessor,
        &processed,
        Some(source),
        &fixture.state.node_id,
        PreparationPurpose::SelectionChange,
        AcceptedAsk {
            prepared_proof: None,
            film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
            desired_digest: None,
            planning_cancellation: None,
            planning_registration: None,
            quality_intent: None,
        },
        false,
        ids,
        verified.then_some(StagedProcessingDelivery {
            dolby_vision_profile: profile81.then_some(8),
            fel: Some(true),
        }),
        None,
    )
    .await;
    assert_eq!(outcome, PreparationStageOutcome::Staged);
    let successor = fixture
        .store
        .media_session_route_by_incarnation(&incarnation)
        .await
        .expect("route read")
        .expect("the exact minted successor was reserved");
    assert_eq!(successor.session_id, successor_session);
    let serialized: RemoteStartRequest =
        serde_json::from_str(&successor.recipe_json).expect("durable route-v1 recipe");
    assert!(
        serialized.is_valid(),
        "the actual announcement requires a valid negotiated recipe"
    );
    assert_eq!(serialized.request.file_id, fixture.file_id());
    assert!(matches!(
        serialized.request.kind,
        crate::transcode::SessionKind::Transcode { height: 2160 }
    ));
    let start: StartResponse =
        serde_json::from_str(&successor.response_json).expect("durable StartResponse");
    let delivered = if profile81 && verified {
        "dolby_vision"
    } else {
        "hdr10"
    };
    let expected_profile = (profile81 && verified).then_some(8);
    assert_eq!(start.delivered_dynamic_range.as_deref(), Some(delivered));
    assert_eq!(start.delivered_dolby_vision_profile, expected_profile);
    assert!(
        start.enhancement.is_none(),
        "general StartResponse replay must not deserialize persisted enhancement authority"
    );
    let stored: serde_json::Value =
        serde_json::from_str(&successor.response_json).expect("raw durable StartResponse");
    let enhancement: plurx_core::playback::enhancement::PlaybackEnhancement =
        serde_json::from_value(stored["enhancement"].clone())
            .expect("persisted DV enhancement metadata");
    assert_eq!(
        enhancement
            .active
            .contains(&plurx_core::playback::enhancement::EnhancementBadge::Fel),
        verified,
        "source FEL and candidate.processed_dv never invent an admitted processing plan"
    );
    assert_eq!(
        enhancement.requires_effective_processing,
        !verified || !profile81
    );
    let mut request = control_request(route.incarnation_id.clone());
    request.supported_actions = Some(vec![
        crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned(),
    ]);
    let capabilities = request.capabilities.as_mut().expect("dynamic capabilities");
    capabilities.decoder_caps = Some(decoder);
    capabilities.max_height = 2160;
    capabilities.codecs = vec![crate::playback_control::CodecPolicy::Hevc];
    capabilities.dynamic_ranges = vec![
        crate::playback_control::DynamicRangePolicy::Hdr10,
        crate::playback_control::DynamicRangePolicy::DolbyVision,
    ];
    capabilities.dual_player_preparation = true;
    let (status, offer) = control_body(
        control_local_inner(
            &fixture.state,
            &route,
            request,
            unix_ms().saturating_add(4_000),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{offer}");
    assert_eq!(offer["action"]["type"], "prepare", "{offer}");
    assert_eq!(offer["action"]["session_id"], successor_session);
    assert_eq!(
        offer["action"]["effective_selection"]["dynamic_range"],
        delivered
    );
    assert_eq!(
        offer["action"]
            .get("delivered_dolby_vision_profile")
            .and_then(serde_json::Value::as_u64),
        expected_profile.map(u64::from)
    );
    assert!(
        offer["action"]["enhancement"].is_null(),
        "unprimed staging has no attached successor owner and cannot revive persisted enhancement metadata"
    );

    // The served-window report must bind to the staged generation AND its
    // authoritative delivered carrier when public control projects the owner
    // header. Reconstructed P81 evidence must not disappear as HDR10-E.
    let report = serde_json::json!({"generation":incarnation,"hdr10_enhanced":!profile81,
        "profile81_reconstructed":profile81,"fel_contributed":true,
        "applied_operations":["RpuColorConversion","TargetMapping","LinearNlqResidual"]});
    let mut response = Json(serde_json::json!({"generation":incarnation,
        "effective_selection":{"dynamic_range":delivered}}))
    .into_response();
    response.headers_mut().insert(
        "x-plurx-dv-processing",
        axum::http::HeaderValue::from_str(&report.to_string()).expect("owner header"),
    );
    let projected = super::control::public_control_processing(response).await;
    let bytes = axum::body::to_bytes(projected.into_body(), 65536)
        .await
        .expect("public body");
    let projected: serde_json::Value = serde_json::from_slice(&bytes).expect("public control");
    assert_eq!(
        projected.get("effective_processing").is_some(),
        verified || !profile81,
        "a current HDR10 owner receipt is valid without an earlier root marker; a P81 receipt still needs its DV carrier"
    );
    if !verified {
        assert!(
            enhancement.details.is_empty(),
            "pending metadata cannot contradict a later owner receipt"
        );
        assert!(
            enhancement.active.is_empty(),
            "without a verified root, no processing badge is active before its owner receipt"
        );
        assert!(
            !enhancement
                .active
                .contains(&plurx_core::playback::enhancement::EnhancementBadge::Fel)
        );
    }
    if verified || !profile81 {
        assert_eq!(
            projected["effective_processing"]["profile81_reconstructed"]
                .as_bool()
                .unwrap_or(false),
            profile81
        );
        assert_eq!(projected["effective_processing"]["fel_contributed"], true);
    }
}

#[tokio::test]
async fn verified_profile81_staged_delivery_survives_durable_start_prepare_and_control_projection()
{
    staged_processing_delivery_round_trip(true, true).await;
}

#[tokio::test]
async fn verified_hdr10_staged_delivery_keeps_its_effective_processing_requirement() {
    staged_processing_delivery_round_trip(false, true).await;
}

#[tokio::test]
async fn a_processed_candidate_without_verified_root_does_not_claim_profile81_or_fel() {
    staged_processing_delivery_round_trip(true, false).await;
}

#[tokio::test]
async fn a_processed_hdr10_candidate_without_verified_root_accepts_its_current_owner_receipt() {
    staged_processing_delivery_round_trip(false, false).await;
}
