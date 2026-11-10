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
