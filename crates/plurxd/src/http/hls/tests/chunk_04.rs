
    /// P1-6. The successor resumes **where the viewer is**, not at the
    /// predecessor's original start and not at its fetch frontier.
    ///
    /// The original start was always excluded — `candidate_request` clones the
    /// predecessor's request and overwrites only the selection, so committing
    /// its `start_seconds` restarts the film for a viewer forty minutes in.
    /// The frontier is what this test now also excludes: `fetched_through_ms`
    /// reaches the *end* of a segment the moment the client asks for it and
    /// survives prefetch, retry and backward seeks, so a successor staged
    /// there begins past film the viewer has never been shown. That is the
    /// forward jump a prepared handoff exists to prevent, which is why the
    /// frontier assertion this replaces was pinning the defect in place.
    #[tokio::test]
    async fn a_staged_successor_resumes_at_the_accepted_playhead_not_the_fetch_frontier() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, mut route) = staging_fixture(dir.path()).await;
        route.media_origin_ms = RESUME_ORIGIN_MS;
        route.fetched_through_ms = RESUME_FETCHED_THROUGH_MS;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: RESUME_ACCEPTED_PLAYHEAD_MS,
                desired_digest: None,
            },
        )
        .await;

        let (origin_ms, start_seconds, bootstrap_start_seconds, bootstrap_origin_ms) =
            staged_resume_ms(&fixture, &route).await;
        assert_ne!(
            origin_ms, RESUME_FETCH_FRONTIER_MS,
            "the fetch frontier is a high-water mark, never a presentation time"
        );
        assert_ne!(
            origin_ms, RESUME_DOUBLE_COUNTED_ORIGIN_MS,
            "the accepted playhead is already absolute — the origin is in it once"
        );
        assert_eq!(
            origin_ms, 0,
            "a VOD successor keeps film zero as its origin"
        );
        assert_eq!(
            bootstrap_origin_ms, 0,
            "the bootstrap must expose the same VOD origin as the durable route"
        );
        assert!(
            (start_seconds - RESUME_ACCEPTED_PLAYHEAD_MS as f64 / 1_000.0).abs() < 0.001,
            "the recipe must agree with the row, not restart at {}",
            staged_candidate_request().start_seconds,
        );
        assert!(
            (bootstrap_start_seconds - RESUME_ACCEPTED_PLAYHEAD_MS as f64 / 1_000.0).abs() < 0.001,
            "the bootstrap the client reads must agree with the row too"
        );
    }

    /// The same proof at the **production control-to-staging boundary**.
    ///
    /// The helper test above would still pass if the exchange handed it the
    /// wrong number, because nothing in it exercises the capture. This drives
    /// two real `control_local_inner` exchanges — the first establishing a
    /// preparation-capable client and a delivered selection, the second asking
    /// for a height that crosses both resolution and delivery method — and
    /// lets the detached candidate run exactly as production spawns it.
    #[tokio::test]
    async fn the_control_seam_stages_from_the_accepted_playhead_not_the_route_frontier() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, mut route) = staging_fixture(dir.path()).await;
        route.media_origin_ms = RESUME_ORIGIN_MS;
        route.fetched_through_ms = RESUME_FETCHED_THROUGH_MS;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut request = preparing_control_request(&route);
        // A viewer 30 seconds into this session whose client has already
        // fetched 150 seconds of it: the two numbers the resume must tell
        // apart, carried on the envelope the exchange accepts.
        request.position_ms = RESUME_ACCEPTED_PLAYHEAD_MS;
        request.buffered_through_ms = RESUME_FETCH_FRONTIER_MS;
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        pace_control_exchanges().await;
        request.sequence = 2;
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        await_preparation_candidate(&route).await;

        let (origin_ms, start_seconds, bootstrap_start_seconds, bootstrap_origin_ms) =
            staged_resume_ms(&fixture, &route).await;
        assert_eq!(
            origin_ms, 0,
            "the exchange cannot turn an absolute resume point into a second VOD origin"
        );
        assert_eq!(bootstrap_origin_ms, 0);
        let expected_seconds = RESUME_ACCEPTED_PLAYHEAD_MS as f64 / 1_000.0;
        assert!((start_seconds - expected_seconds).abs() < 0.001);
        assert!((bootstrap_start_seconds - expected_seconds).abs() < 0.001);
    }

    /// A backward seek is the case the fetch frontier cannot survive: the
    /// client has already fetched far past where the viewer just asked to go,
    /// so the frontier stays high while the intended position drops. Resuming
    /// at the frontier would drop the viewer back at the point they seeked
    /// *away from*.
    ///
    /// `seek_target_ms` wins over `position_ms` here for the reason
    /// `ControlRequestV1::validate` anchors the buffer on it: mid-seek the
    /// reported position is still the old one.
    #[tokio::test]
    async fn a_backward_seek_stages_from_the_seek_target_at_the_control_seam() {
        const SEEK_TARGET_MS: i64 = 105_000;
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, mut route) = staging_fixture(dir.path()).await;
        route.media_origin_ms = RESUME_ORIGIN_MS;
        route.fetched_through_ms = RESUME_FETCHED_THROUGH_MS;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut request = preparing_control_request(&route);
        request.position_ms = 200_000;
        request.buffered_through_ms = RESUME_FETCH_FRONTIER_MS;
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        pace_control_exchanges().await;
        request.sequence = 2;
        // Still reporting the old position, as a seeking client does.
        request.render_state = crate::playback_control::RenderState::Seeking;
        request.seek_target_ms = Some(SEEK_TARGET_MS);
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        await_preparation_candidate(&route).await;

        let (origin_ms, start_seconds, bootstrap_start_seconds, bootstrap_origin_ms) =
            staged_resume_ms(&fixture, &route).await;
        assert_eq!(
            origin_ms, 0,
            "a backward seek changes the requested start, not the VOD timeline origin"
        );
        assert_eq!(bootstrap_origin_ms, 0);
        let expected_seconds = SEEK_TARGET_MS as f64 / 1_000.0;
        assert!((start_seconds - expected_seconds).abs() < 0.001);
        assert!((bootstrap_start_seconds - expected_seconds).abs() < 0.001);
    }

    /// A resume is bounded by the film, not by what `validate` will accept.
    ///
    /// `validate` deliberately allows one target duration of slack past the
    /// end so a client reporting the final segment's *end* is not refused, and
    /// this value is client-reported rather than derived from what the server
    /// observed itself delivering. Staging past the end would hold this
    /// playback's one preparation slot, a durable row and an admission slot
    /// for a full `PREPARATION_DEADLINE_MS` on a resume nobody can reach.
    ///
    /// 2 000 ms of slack is the smallest the clamp in `validate` can be, so
    /// this position is accepted whatever the fixture's target duration.
    #[tokio::test]
    async fn a_resume_past_the_end_of_the_film_is_bounded_by_the_film() {
        const FIXTURE_DURATION_MS: i64 = 6_000_000;
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut request = preparing_control_request(&route);
        request.position_ms = FIXTURE_DURATION_MS + 2_000;
        request.buffered_through_ms = FIXTURE_DURATION_MS + 2_000;
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the envelope itself is valid — the bound under test is the resume, not the exchange"
        );

        pace_control_exchanges().await;
        request.sequence = 2;
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        await_preparation_candidate(&route).await;

        let (origin_ms, start_seconds, bootstrap_start_seconds, bootstrap_origin_ms) =
            staged_resume_ms(&fixture, &route).await;
        assert_eq!(
            origin_ms, 0,
            "bounding the requested start must not move the VOD timeline origin"
        );
        assert_eq!(bootstrap_origin_ms, 0);
        let expected_seconds = FIXTURE_DURATION_MS as f64 / 1_000.0;
        assert!((start_seconds - expected_seconds).abs() < 0.001);
        assert!((bootstrap_start_seconds - expected_seconds).abs() < 0.001);
    }

    // -----------------------------------------------------------------
    // M0 — `delivery.preparation` and supersession cancellation.
    // -----------------------------------------------------------------

    /// One `plurx_playback_preparation_cancelled_total` bucket.
    ///
    /// Read out of the rendered exposition rather than through a private
    /// accessor, because there is no snapshot for this counter and because
    /// reading the text also proves the block is emitted under the label a
    /// dashboard would query. The counter is process-global and these tests run
    /// in parallel with each other, so every assertion against it is "grew",
    /// never "grew by one".
    fn cancelled_total(reason: &str) -> u64 {
        let needle = format!("plurx_playback_preparation_cancelled_total{{reason=\"{reason}\"}} ");
        crate::playback_control::prometheus()
            .lines()
            .find_map(|line| line.strip_prefix(needle.as_str()))
            .expect("the cancelled counter names this reason")
            .trim()
            .parse()
            .expect("a counter value")
    }

    /// The tri-state this milestone added, as a client reads it off the wire.
    fn preparation_state(body: &serde_json::Value) -> String {
        body["delivery"]["preparation"]
            .as_str()
            .expect("a build that evaluated the slot must answer the field")
            .to_owned()
    }

    async fn accepted_exchange(
        fixture: &HlsDeliveryFixture,
        route: &MediaSessionRoute,
        request: &crate::playback_control::ControlRequestV1,
    ) -> serde_json::Value {
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
        assert_eq!(status, StatusCode::OK, "exchange refused: {body}");
        body
    }

    /// The executor `stage_prepared_successor_with_prime` builds, built the
    /// same way, so a registered entry settles through the real store calls.
    async fn preparation_executor(
        fixture: &HlsDeliveryFixture,
        session_id: &str,
        route: &MediaSessionRoute,
    ) -> crate::playback_control::PreparationExecutor {
        let gate = fixture
            .state
            .transcode
            .session_preparation_gate(session_id)
            .await
            .expect("the live predecessor owns a preparation gate");
        crate::playback_control::PreparationExecutor::new(
            Arc::clone(&fixture.state.store),
            gate,
            route.user_id,
            route.playback_id.clone(),
            route.owner_node_id.clone(),
            route.owner_epoch,
        )
    }

    /// A preparation row of the shape the real staging path writes.
    fn preparation_row(
        fixture: &HlsDeliveryFixture,
        route: &MediaSessionRoute,
        staged_incarnation_id: &str,
    ) -> plurx_core::domain::MediaSessionPreparation {
        let staged_session_id = uuid::Uuid::new_v4().to_string();
        let staged_request = crate::transcode::SessionRequest {
            file_id: fixture.file_id(),
            playback_id: route.playback_id.clone(),
            request_id: Some(staged_incarnation_id.to_owned()),
            ..staged_candidate_request()
        };
        let response = StartResponse {
            session_id: staged_session_id.clone(),
            playlist_url: format!("/api/v1/hls/{staged_session_id}/index.m3u8"),
            duration_ms: Some(6_000_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 1080,
            encoder: "staged".to_owned(),
            vod: true,
            ladder: Vec::new(),
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &staged_session_id,
                staged_incarnation_id,
                1,
                crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        plurx_core::domain::MediaSessionPreparation {
            expected_desired_revision: None,
            incarnation_id: staged_incarnation_id.to_owned(),
            session_id: staged_session_id,
            user_id: route.user_id,
            playback_id: route.playback_id.clone(),
            expected_predecessor_incarnation_id: route.incarnation_id.clone(),
            expected_predecessor_owner_node_id: route.owner_node_id.clone(),
            expected_predecessor_owner_epoch: route.owner_epoch,
            request_fingerprint: staged_request.durable_intent_fingerprint(route.user_id),
            owner_node_id: fixture.state.node_id.clone(),
            recipe_json: serde_json::to_string(&RemoteStartRequest {
                protocol_version: crate::media_pool::PROTOCOL_VERSION,
                incarnation_id: staged_incarnation_id.to_owned(),
                user_id: route.user_id,
                source_size: 1,
                source_mtime: 1,
                typeless_playlist: false,
                library_channel: None,
                request: staged_request,
            })
            .expect("recipe"),
            response_json: serde_json::to_string(&response).expect("response"),
            media_origin_ms: 0,
            now_ms,
            deadline_ms: now_ms.saturating_add(30_000),
        }
    }

    /// A reserved-and-registered successor: the durable row exists and the
    /// registry names it, which is the state a real candidate is in between
    /// `register_active_preparation` and its commit.
    ///
    /// `register_test_preparation` rather than `stage_prepared_successor`
    /// deliberately: the stage-only helper never registers, so a cancellation
    /// test built on it would be cancelling an empty registry and would pass
    /// with the edge under test deleted.
    async fn registered_preparation(
        fixture: &HlsDeliveryFixture,
        session_id: &str,
        route: &MediaSessionRoute,
    ) -> (
        plurx_core::domain::MediaSessionPreparation,
        crate::playback_control::PreparationExecutor,
        tokio_util::sync::CancellationToken,
    ) {
        let executor = preparation_executor(fixture, session_id, route).await;
        let preparation = preparation_row(fixture, route, &uuid::Uuid::new_v4().to_string());
        assert!(
            executor
                .reserve(&preparation)
                .await
                .expect("reserve the successor's durable row"),
            "the fixture must be able to reserve, or the settlement below has nothing to abort"
        );
        let cancelled = register_test_preparation(
            fixture.state.clone(),
            executor.clone(),
            preparation.clone(),
            PreparationPurpose::SelectionChange,
        );
        assert!(
            active_preparation(&preparation.incarnation_id).is_some(),
            "the registry must actually name this successor before anything cancels it"
        );
        (preparation, executor, cancelled)
    }

    /// The route a reopen activates: a second incarnation of the same playback,
    /// fenced on the one it replaces.
    async fn activate_superseding_route(
        fixture: &HlsDeliveryFixture,
        predecessor: &MediaSessionRoute,
    ) -> MediaSessionRoute {
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let request = crate::transcode::SessionRequest {
            file_id: fixture.file_id(),
            playback_id: predecessor.playback_id.clone(),
            request_id: Some(incarnation_id.clone()),
            previous_session_id: Some(predecessor.session_id.clone()),
            ..staged_candidate_request()
        };
        let recipe = RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation_id.clone(),
            user_id: predecessor.user_id,
            source_size: 1,
            source_mtime: 1,
            typeless_playlist: false,
            library_channel: None,
            request: request.clone(),
        };
        let start = StartResponse {
            session_id: session_id.clone(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(6_000_000),
            start_seconds: 0.0,
            media_origin_ms: Some(0),
            height: 1080,
            encoder: "test".to_owned(),
            vod: true,
            ladder: Vec::new(),
            prior_kbps: None,
            delivered_dynamic_range: Some("sdr".to_owned()),
            delivered_dolby_vision_profile: None,
            control: crate::playback_control::ControlBootstrap::new(
                &session_id,
                &incarnation_id,
                1,
                crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id,
                session_id,
                user_id: predecessor.user_id,
                playback_id: predecessor.playback_id.clone(),
                expected_predecessor_incarnation_id: Some(predecessor.incarnation_id.clone()),
                fence_predecessor: true,
                request_id: None,
                request_fingerprint: request.durable_intent_fingerprint(predecessor.user_id),
                owner_node_id: fixture.state.node_id.clone(),
                lease_expires_at_ms: now_ms.saturating_add(900_000),
                recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                response_json: serde_json::to_string(&start).expect("response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms,
            },
        )
        .await
    }

    /// Run the supersession cancel at its real call site.
    ///
    /// `settle_activation_predecessor` is the one place M0 calls it, and it
    /// calls it above the `predecessor_incarnation` early return, so a plain
    /// create reaches it too. Passing `None` here is a create that named no
    /// predecessor; the function then returns as soon as the cancel has run.
    async fn settle_activation(
        fixture: &HlsDeliveryFixture,
        predecessor_incarnation: Option<String>,
        successor: &MediaSessionRoute,
    ) -> Result<(), ApiError> {
        let authority = crate::serving_fence::ServingAuthority::always_ready();
        let admitted = authority.admit().expect("an admitted serving generation");
        settle_activation_predecessor(
            &fixture.state,
            predecessor_incarnation,
            successor.clone(),
            authority,
            admitted,
            None,
        )
        .await
    }

    /// Wait for a settlement to have taken the successor's durable row.
    async fn await_aborted_preparation_row(
        fixture: &HlsDeliveryFixture,
        staged_incarnation_id: &str,
    ) -> MediaSessionRoute {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(row) = fixture
                    .state
                    .store
                    .media_session_route_by_incarnation(staged_incarnation_id)
                    .await
                    .expect("staged route read")
                {
                    if row.state == "ended" {
                        break row;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the settlement aborted or discarded the durable row")
    }

    /// The dispatching exchange answers `staging` and `action: none` together.
    ///
    /// Both halves in the same response are the point. `action: none` on its
    /// own is what the client already saw before this milestone, and a client
    /// that is told only that has no reason not to reopen — which is the reopen
    /// the whole effort exists to retire. The first exchange is asserted too,
    /// so the test cannot pass on a build that answers `staging` always.
    #[tokio::test]
    async fn delivery_preparation_says_staging_on_the_exchange_that_dispatched() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-dispatch");
        let (fixture, _session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut request = preparing_control_request(&route);
        let opening = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(
            preparation_state(&opening),
            "none",
            "the session's own opening ask is not a change and builds nothing",
        );

        pace_control_exchanges().await;
        request.sequence = 2;
        request.capabilities = None;
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let dispatched = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(
            dispatched["action"]["type"], "none",
            "the exchange that dispatches announces nothing — the stage is detached",
        );
        assert_eq!(
            preparation_state(&dispatched),
            "staging",
            "and the same response has to say so, or the client reopens",
        );

        await_preparation_candidate(&route).await;
    }

    /// The window `PendingCandidateGuard` exists for.
    ///
    /// Between the dispatch and the registry entry a candidate does a settings
    /// read, a source read and its planning, and an exchange landing in there
    /// has neither a dispatch of its own nor an entry to read. This parks a
    /// candidate in that window for longer than two client polls and drives two
    /// exchanges through it; deleting the pending-candidate clause from the
    /// emit rule makes both of them answer `none`.
    ///
    /// Then two endings for the same window: a plan that fails, after which the
    /// client must be told to stop waiting, and a supersession, after which the
    /// task must exit through `record_preparation_staged(false)` having staged
    /// nothing and left no marker.
    #[tokio::test]
    async fn delivery_preparation_stays_staging_while_the_candidate_is_planning() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-planning");
        let (fixture, _session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        // Longer than the two 260 ms polls below, and it refuses at the end:
        // this half of the test is about the window, not about what a
        // successful plan produces.
        fault_preparation_planning(&playback_id, std::time::Duration::from_millis(1_500), true);

        let mut request = preparing_control_request(&route);
        let _ = accepted_exchange(&fixture, &route, &request).await;

        pace_control_exchanges().await;
        request.sequence = 2;
        request.capabilities = None;
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let dispatched = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(preparation_state(&dispatched), "staging");

        for sequence in 3..=4_u64 {
            pace_control_exchanges().await;
            request.sequence = sequence;
            let polled = accepted_exchange(&fixture, &route, &request).await;
            assert_eq!(
                polled["action"]["type"], "none",
                "exchange {sequence} has nothing to announce yet",
            );
            assert_eq!(
                preparation_state(&polled),
                "staging",
                "exchange {sequence} landed while the candidate was still planning: \
                 it dispatched nothing itself and no successor is registered, so only \
                 the pending-candidate marker can answer for it",
            );
        }

        await_preparation_candidate(&route).await;
        assert!(
            pending_candidate_for_playback(&playback_id).is_none(),
            "the guard drops with the task, so the marker cannot outlive it",
        );
        pace_control_exchanges().await;
        request.sequence = 5;
        let after_failure = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(
            preparation_state(&after_failure),
            "none",
            "a plan that failed leaves nothing being built, and a client waiting for a \
             handoff that will never come must be told to stop",
        );

        // Same window, superseded instead of failed. A different height, so the
        // ask is undispatched again and a second candidate is spawned.
        fault_preparation_planning(&playback_id, std::time::Duration::from_millis(1_500), false);
        pace_control_exchanges().await;
        request.sequence = 6;
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 720 };
        let redispatched = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(preparation_state(&redispatched), "staging");

        let refused_before = crate::playback_control::preparation_staged_snapshot().refused;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel_preparations_for_superseded_predecessor(&playback_id, None);
        await_preparation_candidate(&route).await;

        assert!(
            pending_candidate_for_playback(&playback_id).is_none(),
            "the superseded candidate's marker goes with its guard",
        );
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "a candidate superseded mid-flight must not reach the ledger",
        );
        assert!(
            crate::playback_control::preparation_staged_snapshot().refused > refused_before,
            "and it must exit through record_preparation_staged(false), not silently",
        );
    }

    /// `offered` is a restatement of the action, not a second opinion about the
    /// slot: the exchange that carries `Prepare` answers `offered`, and nothing
    /// else in the emit rule is true at that moment — no dispatch, no pending
    /// marker, no registered successor — so the answer can only come from the
    /// action.
    #[tokio::test]
    async fn delivery_preparation_says_offered_with_the_prepare_action() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-offered");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &crate::transcode::SessionRequest {
                playback_id: playback_id.clone(),
                ..staged_candidate_request()
            },
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        assert!(
            pending_candidate_for_playback(&playback_id).is_none()
                && !has_active_preparation_for_ask(
                    &playback_id,
                    &request.selection.desired().digest(),
                ),
            "the staging helper registers nothing, so `offered` cannot come from `staging`'s sources",
        );
        let announced = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(announced["action"]["type"], "prepare");
        assert_eq!(preparation_state(&announced), "offered");
    }

    /// `none` on an empty slot, and `none` on an `end` — even with a successor
    /// sitting in the ledger.
    ///
    /// The second half is the one worth having. `End`'s owner-local transaction
    /// aborts whatever the session held, so the staged row is not work being
    /// done for this ask however durable it looks; the identical state answers
    /// `offered` on a non-terminal exchange, which
    /// `delivery_preparation_says_offered_with_the_prepare_action` pins.
    #[tokio::test]
    async fn delivery_preparation_says_none_when_the_slot_is_empty() {
        let dir = crate::test_tempdir().expect("state dir");
        let empty_playback = unique_playback_id("preparation-empty");
        let (empty, _empty_session, empty_route) =
            staging_fixture_for_playback(dir.path(), &empty_playback).await;
        let idle = accepted_exchange(
            &empty,
            &empty_route,
            &preparing_control_request(&empty_route),
        )
        .await;
        assert_eq!(
            preparation_state(&idle),
            "none",
            "nothing has been asked for and nothing is being built",
        );

        // A second fixture rather than a second exchange on the first: the End
        // below is this session's only exchange, so nothing it is refused for
        // could be a fence left behind by an earlier one.
        let ending_playback = unique_playback_id("preparation-ending");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &ending_playback).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &crate::transcode::SessionRequest {
                playback_id: ending_playback.clone(),
                ..staged_candidate_request()
            },
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &ending_playback)
                .await
                .expect("ledger read")
                .is_some(),
            "the ledger has to hold a successor, or the End below proves nothing",
        );

        let mut ending = preparing_control_request(&route);
        ending.demand = crate::playback_control::PlaybackDemand::End;
        ending.render_state = crate::playback_control::RenderState::Ended;
        ending.playback_rate = 0.0;
        let ended = accepted_exchange(&fixture, &route, &ending).await;
        assert_eq!(
            preparation_state(&ended),
            "none",
            "End announces no successor and aborts the slot, so nothing is being built \
             for this ask however durable the ledger row looks",
        );
    }

    /// Register a successor for this playback with a recorded ask, and no
    /// durable row: these tests are about what an exchange *says*, not about
    /// what a settlement does. The caller takes the entry back out.
    async fn register_successor_for_ask(
        fixture: &HlsDeliveryFixture,
        session_id: &str,
        route: &MediaSessionRoute,
        asked: Option<&str>,
    ) -> String {
        let executor = preparation_executor(fixture, session_id, route)
            .await
            .asking(asked.map(str::to_owned));
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let preparation = preparation_row(fixture, route, &incarnation_id);
        let _cancelled = register_test_preparation(
            fixture.state.clone(),
            executor,
            preparation,
            PreparationPurpose::SelectionChange,
        );
        assert!(
            active_preparation(&incarnation_id).is_some(),
            "the registry must actually name this successor, or the clause under test \
             is being read against an empty map",
        );
        incarnation_id
    }

    /// The first of the three `staging` sources, on its own.
    ///
    /// The guard is claimed with this exchange's digest just before the spawn,
    /// so in the ordinary case the pending marker answers for the dispatching
    /// exchange too — which is why this clause looks redundant and why deleting
    /// it passed every other test. It is not redundant: the task is already
    /// spawned, and on the multi-threaded runtime production runs it can finish
    /// and drop its marker before the emit rule reads the map. The seam here
    /// holds the exchange until exactly that has happened, so what is asserted
    /// is the clause rather than its shadow.
    ///
    /// Deleting `preparation_purpose.is_some()` from the emit rule fails this.
    #[tokio::test]
    async fn delivery_preparation_says_staging_on_the_dispatch_after_its_candidate_is_gone() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-dispatch-race");
        let (fixture, _session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut request = preparing_control_request(&route);
        let opening = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(preparation_state(&opening), "none");

        pace_control_exchanges().await;
        request.sequence = 2;
        request.capabilities = None;
        request.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        wait_for_the_dispatched_candidate(&playback_id);
        let dispatched = accepted_exchange(&fixture, &route, &request).await;

        assert!(
            pending_candidate_for_playback(&playback_id).is_none(),
            "the seam is supposed to have held this exchange until the candidate it \
             dispatched had finished and dropped its marker",
        );
        assert_eq!(
            preparation_state(&dispatched),
            "staging",
            "this exchange started a successor, and that is a fact about this exchange \
             — not about a map another thread may already have emptied",
        );
        await_preparation_candidate(&route).await;
    }

    /// The third of the three `staging` sources, on its own: a successor that is
    /// registered and priming for the ask this exchange is making. No dispatch,
    /// no pending marker — the registry is the only thing that can answer.
    ///
    /// Deleting `has_active_preparation_for_ask(..)` from the emit rule fails
    /// this.
    #[tokio::test]
    async fn delivery_preparation_says_staging_for_a_registered_successor_for_this_ask() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-registered");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;

        let mut request = preparing_control_request(&route);
        let opening = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(
            preparation_state(&opening),
            "none",
            "the session's own ask is recorded as dispatched, so the exchange below \
             repeats it and dispatches nothing",
        );

        let asked = request.selection.desired().digest();
        let registered =
            register_successor_for_ask(&fixture, &session_id, &route, Some(&asked)).await;

        pace_control_exchanges().await;
        request.sequence = 2;
        request.capabilities = None;
        let polled = accepted_exchange(&fixture, &route, &request).await;
        assert!(
            pending_candidate_for_playback(&playback_id).is_none(),
            "and no pending marker exists to answer in the registry's place",
        );
        assert_eq!(preparation_state(&polled), "staging");

        let _ = take_active_preparation(&registered);
    }

    /// The registry clause is measured against the ask, exactly as the pending
    /// clause is.
    ///
    /// One leftover entry — a stale ask the viewer left, a planned relocation, a
    /// successor whose commit never comes — otherwise answers `staging` on every
    /// exchange for the rest of the playback. A client that believes it waits
    /// out its whole bound and reopens anyway, which is strictly worse than the
    /// `none` it would have been told if the entry had not been there: the same
    /// reopen, several seconds later, with a lease it did not renew.
    ///
    /// Dropping the digest comparison from `has_active_preparation_for_ask`
    /// fails this.
    #[tokio::test]
    async fn delivery_preparation_says_none_for_a_registered_successor_for_another_ask() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-left-behind");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;

        let mut request = preparing_control_request(&route);
        let opening = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(preparation_state(&opening), "none");

        let asked = request.selection.desired().digest();
        let left_behind = register_successor_for_ask(
            &fixture,
            &session_id,
            &route,
            Some("an-ask-the-viewer-has-already-left"),
        )
        .await;
        assert_ne!(asked, "an-ask-the-viewer-has-already-left");

        pace_control_exchanges().await;
        request.sequence = 2;
        request.capabilities = None;
        let polled = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(
            preparation_state(&polled),
            "none",
            "a successor being built for a selection the viewer has left is not work \
             being done for the ask they are waiting on",
        );

        // And the same entry, recorded against *this* ask, does answer — so the
        // `none` above is the comparison talking, not an empty registry.
        let _ = take_active_preparation(&left_behind);
        let matching =
            register_successor_for_ask(&fixture, &session_id, &route, Some(&asked)).await;
        pace_control_exchanges().await;
        request.sequence = 3;
        let again = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(preparation_state(&again), "staging");
        let _ = take_active_preparation(&matching);
    }

    /// A second quality tap supersedes the successor still being built for the
    /// first — through the claim, not the flag.
    ///
    /// This is the ordinary case and the one the flag cannot see. Tapping a
    /// different quality on a live session activates nothing, so
    /// `cancel_preparations_for_superseded_predecessor` never runs and nothing
    /// anywhere is marked cancelled; the next exchange simply dispatches again
    /// and `PendingCandidateGuard::begin` overwrites the entry, with `cancelled`
    /// false, for a different ask. A reader that looked only at the flag let the
    /// first task register, arm its watch, reserve and prime a full encoder for
    /// a selection the viewer had already left — and left two entries naming one
    /// playback, the stale one of which then answered `staging` forever.
    ///
    /// Reading only the `cancelled` flag after registration fails this.
    #[tokio::test]
    async fn a_second_ask_supersedes_a_preparation_in_the_registration_window() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-second-ask");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;

        let first = PendingCandidateGuard::begin(&playback_id, "the-first-ask");
        delay_preparation_registration(&playback_id, std::time::Duration::from_millis(800));

        let superseded_before = cancelled_total("predecessor_superseded");
        let ownership_before = cancelled_total("ownership_cancelled");
        let state = fixture.state.clone();
        let staging_session = session_id.clone();
        let staging_route = route.clone();
        let candidate = crate::transcode::SessionRequest {
            playback_id: playback_id.clone(),
            ..staged_candidate_request()
        };
        let predecessor_recipe = staged_predecessor_recipe(&route);
        let staging = tokio::spawn(async move {
            stage_prepared_successor_with_prime(
                &state,
                &staging_session,
                &staging_route,
                &predecessor_recipe,
                &candidate,
                Some(&staged_source_file()),
                &state.node_id,
                PreparationPurpose::SelectionChange,
                AcceptedAsk {
                    film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                    desired_digest: Some("the-first-ask".to_owned()),
                },
                true,
            )
            .await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        // The viewer taps a different quality. Nothing is cancelled anywhere:
        // the new exchange's guard simply takes the slot.
        let second = PendingCandidateGuard::begin(&playback_id, "the-second-ask");
        assert!(
            !pending_candidate_superseded(&playback_id, None),
            "no cancelled flag is set — this is the claim-replacement path, which is \
             the one a second quality tap on a live session actually takes",
        );
        assert!(
            first.cancelled(),
            "the first candidate's own guard already knows the slot is not its any more; \
             the reader after registration has to reach the same answer",
        );

        staging.await.expect("the staging task finished");
        drop(first);

        assert!(
            !has_active_preparation_for_ask(&playback_id, "the-first-ask"),
            "the superseded successor must not be left registered and priming",
        );
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "and it must never have been reserved, let alone staged",
        );
        let superseded_after = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let now = cancelled_total("predecessor_superseded");
                if now > superseded_before {
                    break now;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the read after registration settled the successor as a supersession");
        assert!(superseded_after > superseded_before);
        assert_eq!(
            cancelled_total("ownership_cancelled"),
            ownership_before,
            "the second ask must be what tore it down, not a reservation guard dropping \
             after it had already gone on to reserve and prime",
        );
        drop(second);
    }

    /// A predecessor that is superseded frees its successor.
    ///
    /// The edge M0 added: before it, a reopen cancelled nothing and the
    /// speculative encoder ran to the 330 s deadline for nobody, or was torn
    /// down by the reopen's own admission pressure and counted as a refusal.
    #[tokio::test]
    async fn superseding_the_predecessor_cancels_its_registered_preparation() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-superseded");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        let (preparation, _executor, cancelled) =
            registered_preparation(&fixture, &session_id, &route).await;

        let successor = activate_superseding_route(&fixture, &route).await;
        let cancels_before = cancelled_total("predecessor_superseded");

        // The activation names its predecessor, which is what a client that
        // reopened through `previous_session_id` produces.
        let _ = settle_activation(&fixture, Some(route.incarnation_id.clone()), &successor).await;

        assert!(
            active_preparation(&preparation.incarnation_id).is_none(),
            "the registry entry must be gone",
        );
        assert!(
            cancelled.is_cancelled(),
            "the settlement must take ownership of the successor's cancellation token",
        );
        let aborted = await_aborted_preparation_row(&fixture, &preparation.incarnation_id).await;
        assert_eq!(
            aborted.terminal_reason.as_deref(),
            Some("replaced"),
            "the durable row must be aborted or discarded, not merely forgotten",
        );
        assert!(
            cancelled_total("predecessor_superseded") > cancels_before,
            "and the teardown must be attributable to the policy that ordered it",
        );
    }

    /// The `keep` guard, run with an entry actually present.
    ///
    /// Every activation runs the supersession cancel, including the committed
    /// successor's own. `keep` is that route's incarnation id, and without it
    /// the one successor that must survive its predecessor's retirement is the
    /// one this edge would tear down. Commit removes the entry first today, so
    /// the guard is not load-bearing on the current ordering — which is exactly
    /// why it has to be asserted against a registry that is not empty, or the
    /// cancel would "succeed" because there was nothing there.
    #[tokio::test]
    async fn the_activating_successor_is_kept_by_the_supersession_cancel() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-kept");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        let successor = activate_superseding_route(&fixture, &route).await;

        // Registered under the activating route's own incarnation: this is the
        // committed successor, the single entry `keep` names.
        let executor = preparation_executor(&fixture, &session_id, &route).await;
        let preparation = preparation_row(&fixture, &route, &successor.incarnation_id);
        let cancelled = register_test_preparation(
            fixture.state.clone(),
            executor,
            preparation,
            PreparationPurpose::SelectionChange,
        );
        assert!(
            active_preparation(&successor.incarnation_id).is_some(),
            "the guard is being exercised against a registry that names this successor",
        );

        settle_activation(&fixture, None, &successor)
            .await
            .expect("a create that names no predecessor settles immediately");

        assert!(
            active_preparation(&successor.incarnation_id).is_some(),
            "the activating route's own successor must survive the cancel its activation ran",
        );
        assert!(
            !cancelled.is_cancelled(),
            "and no settlement may have been started for it",
        );
        let _ = take_active_preparation(&successor.incarnation_id);
    }

    /// A lifecycle check, not a guard mutation: nothing here is deleted to make
    /// it fail. It pins that the ordinary commit-then-retire sequence leaves the
    /// successor current and usable, which is the outcome every cancellation
    /// edge added by this milestone has to stay out of the way of.
    #[tokio::test]
    async fn a_committed_successor_survives_the_predecessors_retirement() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-committed");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &crate::transcode::SessionRequest {
                playback_id: playback_id.clone(),
                ..staged_candidate_request()
            },
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let offered = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(offered["action"]["type"], "prepare");
        let action_id = offered["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        pace_control_exchanges().await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            committed_media_origin_ms: offered["action"]["media_origin_ms"].as_i64(),
            first_frame_unix_ms: Some(unix_ms()),
        });
        let committed_exchange = accepted_exchange(&fixture, &route, &request).await;
        assert_eq!(committed_exchange["action"]["type"], "none");

        // The predecessor retires. Before this milestone nothing cancelled a
        // successor on a supersession; now several things can, and none of them
        // may reach the one the client is already watching.
        fixture
            .state
            .store
            .end_media_session(&session_id, "superseded", unix_ms())
            .await
            .expect("retire the predecessor");

        let current = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &playback_id)
            .await
            .expect("current route")
            .expect("the playback still has a pointer");
        assert_eq!(
            current.incarnation_id, staged.staged_incarnation_id,
            "the committed successor is still the current generation",
        );
        assert_eq!(current.state, "active");
        assert!(
            control_start_response(&current).is_some(),
            "and it is still usable: without a bootstrap its next exchange is a 404",
        );
    }

    /// A plain create is a supersession too.
    ///
    /// M0 has one call site, at the top of `settle_activation_predecessor` and
    /// above its `predecessor_incarnation` early return, rather than the two the
    /// plan named. This is what that placement buys: a client that reopened
    /// without naming its predecessor still frees the successor it abandoned.
    /// Both states the cancel has to cover are checked — a registered entry, and
    /// a candidate that has not reached the registry yet.
    #[tokio::test]
    async fn a_plain_create_for_the_playback_cancels_the_preparation() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-plain-create");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        let (preparation, _executor, cancelled) =
            registered_preparation(&fixture, &session_id, &route).await;
        let successor = activate_superseding_route(&fixture, &route).await;
        assert_ne!(
            successor.incarnation_id, preparation.incarnation_id,
            "`keep` must not be what removes this entry",
        );

        settle_activation(&fixture, None, &successor)
            .await
            .expect("a create that names no predecessor settles immediately");

        assert!(
            active_preparation(&preparation.incarnation_id).is_none(),
            "a create that named no predecessor still went around the handoff",
        );
        assert!(cancelled.is_cancelled());
        let aborted = await_aborted_preparation_row(&fixture, &preparation.incarnation_id).await;
        assert_eq!(aborted.terminal_reason.as_deref(), Some("replaced"));

        // And the same create against a candidate that is still doing its Store
        // reads, which has no registry entry to take.
        let pending = PendingCandidateGuard::begin(&playback_id, "a-pending-digest");
        assert_eq!(
            pending_candidate_for_playback(&playback_id).as_deref(),
            Some("a-pending-digest"),
        );
        settle_activation(&fixture, None, &successor)
            .await
            .expect("a create that names no predecessor settles immediately");
        assert!(
            pending.cancelled(),
            "the pending candidate must observe the supersession at its next await",
        );
        assert!(
            pending_candidate_for_playback(&playback_id).is_none(),
            "and must stop reading as `staging` at once, before its task has unwound",
        );
    }

    /// The window between a staging task's last await and its registration.
    ///
    /// The guard's own `cancelled()` checks cannot cover it: in production there
    /// is no suspension point there, so a supersession landing in it is
    /// invisible until the registry names the successor. The flag is read once
    /// more immediately after `register_active_preparation`, and that read is
    /// the only thing standing between a superseded successor and a full prime.
    ///
    /// The task is parked in that window by a `#[cfg(test)]` delay, because a
    /// window with no await in it cannot otherwise be landed in; the
    /// supersession itself goes through the ordinary cancel.
    #[tokio::test]
    async fn a_supersession_in_the_registration_window_still_frees_the_preparation() {
        let dir = crate::test_tempdir().expect("state dir");
        let playback_id = unique_playback_id("preparation-register-window");
        let (fixture, session_id, route) =
            staging_fixture_for_playback(dir.path(), &playback_id).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;

        // The marker a real candidate would be holding at this point.
        let pending = PendingCandidateGuard::begin(&playback_id, "register-window-digest");
        delay_preparation_registration(&playback_id, std::time::Duration::from_millis(800));

        let superseded_before = cancelled_total("predecessor_superseded");
        let ownership_before = cancelled_total("ownership_cancelled");
        let state = fixture.state.clone();
        let staging_session = session_id.clone();
        let staging_route = route.clone();
        let candidate = crate::transcode::SessionRequest {
            playback_id: playback_id.clone(),
            ..staged_candidate_request()
        };
        let predecessor_recipe = staged_predecessor_recipe(&route);
        let staging = tokio::spawn(async move {
            stage_prepared_successor_with_prime(
                &state,
                &staging_session,
                &staging_route,
                &predecessor_recipe,
                &candidate,
                Some(&staged_source_file()),
                &state.node_id,
                PreparationPurpose::SelectionChange,
                AcceptedAsk {
                    film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                    desired_digest: None,
                },
                true,
            )
            .await;
        });

        // Inside the parked window: after the task's last await, before it
        // registers. The real edge, not a hand-written cancellation.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cancel_preparations_for_superseded_predecessor(&playback_id, None);
        staging.await.expect("the staging task finished");
        drop(pending);

        assert!(
            !has_active_preparation_for_ask(&playback_id, "register-window-digest"),
            "the successor registered in that window must not be left running",
        );
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "and it must never have been reserved, let alone staged",
        );
        let superseded_after = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let now = cancelled_total("predecessor_superseded");
                if now > superseded_before {
                    break now;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the post-registration read settled the successor as a supersession");
        assert!(superseded_after > superseded_before);
        assert_eq!(
            cancelled_total("ownership_cancelled"),
            ownership_before,
            "the supersession must be what tore it down, not a reservation guard \
             dropping after it had already gone on to reserve and prime",
        );
    }

    /// A guard that drops late must not evict a newer claim.
    ///
    /// There is one preparation slot per playback and a newer ask deliberately
    /// overwrites an older one, so the two guards' lifetimes overlap by
    /// construction. Without the claim comparison the older guard's destructor
    /// would remove the newer candidate's entry, and every exchange for the ask
    /// the viewer is actually waiting on would answer `none`.
    #[test]
    fn a_late_pending_preparation_guard_drop_leaves_the_newer_claim_alone() {
        let playback_id = unique_playback_id("preparation-guard-drop");
        let older = PendingCandidateGuard::begin(&playback_id, "older-digest");
        let newer = PendingCandidateGuard::begin(&playback_id, "newer-digest");

        assert!(
            older.cancelled(),
            "a guard whose entry is no longer its own reads as cancelled",
        );
        assert!(!newer.cancelled());

        drop(older);
        assert_eq!(
            pending_candidate_for_playback(&playback_id).as_deref(),
            Some("newer-digest"),
            "the older guard's destructor must leave the newer claim in place",
        );

        drop(newer);
        assert!(pending_candidate_for_playback(&playback_id).is_none());
    }

    /// Every cancellation reason this file passes must be attributable.
    ///
    /// The list is scanned out of the source rather than written here, so a
    /// cancellation edge added later with a reason nobody labelled is caught by
    /// this test instead of quietly inflating `other`. `other` itself stays
    /// reachable on purpose — it is what keeps the counter's sum equal to the
    /// number of settlements — but a reason that reaches it from inside this
    /// file is an attribution gap, and the one that does today is named.
    #[test]
    fn every_preparation_cancellation_reason_in_this_file_is_attributable() {
        // Split rather than written out, so this test's own source does not
        // match the scan it performs.
        let marker = concat!("_cancelled", "_preparation(");
        let source = hls_product_source();

        /// The argument list of a call whose opening parenthesis has just been
        /// consumed: balanced, and blind to parentheses inside string literals,
        /// so a reason that happens to contain one cannot truncate the parse.
        fn argument_list(rest: &str) -> Option<&str> {
            let mut depth = 1_usize;
            let mut in_string = false;
            let mut escaped = false;
            for (index, byte) in rest.bytes().enumerate() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        in_string = false;
                    }
                    continue;
                }
                match byte {
                    b'"' => in_string = true,
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&rest[..index]);
                        }
                    }
                    _ => {}
                }
            }
            None
        }

        /// The first string literal in an argument list, terminated by an
        /// unescaped quote rather than by the first quote of any kind.
        fn first_literal(arguments: &str) -> Option<&str> {
            let open = arguments.find('"')?;
            let rest = &arguments[open + 1..];
            let mut escaped = false;
            for (index, byte) in rest.bytes().enumerate() {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    return Some(&rest[..index]);
                }
            }
            None
        }

        /// The two shapes that legitimately carry a `&'static str` binding
        /// instead of a literal: the two function signatures, and the wrapper
        /// that forwards its own parameter. Anything else passing a binding is
        /// a cancellation edge whose reason this scan cannot read, and that is
        /// a failure rather than a skip — a reason the scan cannot see is a
        /// reason nobody had to label.
        const FORWARDING: [&str; 2] = [
            "active: ActivePreparedSuccessor, reason: &'static str",
            "active, reason",
        ];

        let mut reasons = std::collections::BTreeSet::new();
        for call in source.split(marker).skip(1) {
            let arguments = argument_list(call).expect("a balanced call argument list");
            match first_literal(arguments) {
                Some(reason) => {
                    reasons.insert(reason.to_owned());
                }
                None => {
                    // rustfmt gives a wrapped signature a trailing comma; the
                    // shape is the same one either way.
                    let normalized = arguments.split_whitespace().collect::<Vec<_>>().join(" ");
                    let normalized = normalized.trim_end_matches(',');
                    assert!(
                        FORWARDING.contains(&normalized),
                        "a settlement is passed a reason this scan cannot read: {normalized:?}. \
                         Pass the reason as a literal at the call site, or add the new \
                         forwarding shape here deliberately.",
                    );
                }
            }
        }
        assert!(
            reasons.len() >= 9,
            "the scan found only {} reasons, which means it stopped matching the call \
             sites it is supposed to read: {reasons:?}",
            reasons.len(),
        );

        /// Reasons this file passes that no label maps, found by this test and
        /// reported rather than relabelled: choosing a bucket for a settlement
        /// changes what the metric means, and that is not a test's call.
        const UNLABELLED: [&str; 1] = ["planned relocation expired before commit"];
        for reason in &reasons {
            let label = crate::playback_control::preparation_cancelled_label(reason);
            assert!(
                crate::playback_control::PREPARATION_CANCELLED_REASONS.contains(&label),
                "{reason:?} maps to {label:?}, which is not a bucket the counter has",
            );
            if UNLABELLED.contains(&reason.as_str()) {
                assert_eq!(
                    label, "other",
                    "{reason:?} is recorded here as a known attribution gap; if it has \
                     been given a label, remove it from UNLABELLED",
                );
            } else {
                assert_ne!(
                    label, "other",
                    "{reason:?} is a cancellation edge whose teardowns are unattributable: \
                     give it a label, or add it to UNLABELLED with a reason",
                );
            }
        }
        for unlabelled in UNLABELLED {
            assert!(
                reasons.contains(unlabelled),
                "{unlabelled:?} is no longer passed anywhere in this file",
            );
        }

        assert_eq!(
            crate::playback_control::preparation_cancelled_label("a reason no build emits"),
            "other",
            "an unmapped reason must count as an unexplained teardown, never vanish",
        );
    }

    /// A control envelope that will be admitted for preparation: a client that
    /// explicitly offers two pipelines and names the action. The throughput
    /// value remains rollout evidence; it is not an admission gate.
    fn preparing_control_request(
        route: &MediaSessionRoute,
    ) -> crate::playback_control::ControlRequestV1 {
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        request.observed_download_bps = Some(100_000_000);
        request.capabilities = Some(crate::playback_control::DynamicCapabilities {
            platform: crate::playback_control::ClientPlatform::Apple,
            max_height: 2160,
            codecs: vec![crate::playback_control::CodecPolicy::H264],
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dual_player_preparation: true,
        });
        request
    }

    /// Sleep past the per-session control budget.
    ///
    /// A second exchange inside `playback_control::MIN_CONTROL_INTERVAL` is
    /// answered 429 and never reaches the selection engine, so a preparation
    /// test that skipped this would assert an empty ledger against a request
    /// the server declined to consider — and would read as "staging is broken".
    ///
    /// 260 ms rather than the constant itself, because that constant is
    /// private and the fifteen exchange-floor sleeps already in this module
    /// all spell it this way. Widening a production constant's visibility for
    /// one test helper, while leaving fifteen literals beside it, would buy a
    /// second idiom rather than remove one. Migrating all sixteen is worth
    /// doing — in a change that is not also correcting a resume.
    async fn pace_control_exchanges() {
        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
    }

    /// Wait for the detached candidate keyed to this exact predecessor.
    ///
    /// Keyed rather than counted: the process-global in-flight count may still
    /// be zero before the spawned future receives its first poll, so polling it
    /// would let an assertion run against a ledger nothing had written yet.
    async fn await_preparation_candidate(route: &MediaSessionRoute) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if take_preparation_candidate_completion(&route.incarnation_id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached preparation candidate completed");
    }

    /// A VOD session — which is what every public create actually is — has a
    /// gate. The first version of this change resolved only the rolling
    /// registry, so it staged nothing in production while its own tests
    /// passed against a registry viewers never enter.
    #[tokio::test]
    async fn a_vod_session_resolves_a_preparation_gate() {
        let dir = crate::test_tempdir().expect("state dir");
        let (_app, state) = super::super::tests::test_app_with_state();
        let session_id = uuid::Uuid::new_v4().to_string();
        assert!(
            state
                .transcode
                .session_preparation_gate(&session_id)
                .await
                .is_none(),
            "a session neither engine knows has no slot to take"
        );

        state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        assert!(
            state
                .transcode
                .session_preparation_gate(&session_id)
                .await
                .is_some(),
            "the VOD engine serves every public session and must answer here"
        );
    }

    /// A plausible accepted playhead for the staging tests that are not about
    /// *where* the successor resumes.
    ///
    /// Deliberately nonzero: the fixture route starts at `media_origin_ms` 0
    /// with nothing fetched, so a zero here would let a resume computed from
    /// entirely the wrong quantity agree with the right one by accident.
    const STAGED_ACCEPTED_FILM_TIME_MS: i64 = 30_000;

    /// The fixture's own source row. The seam reads it, and staging refuses
    /// without a snapshot — `0/0` is the sentinel that would refuse the
    /// successor a takeover forever.
    async fn staging_source(fixture: &HlsDeliveryFixture) -> plurx_core::domain::MediaFile {
        fixture
            .store
            .get_file(fixture.file_id())
            .await
            .expect("fixture file lookup")
            .expect("the fixture has a source")
    }

    fn staged_predecessor_recipe(route: &MediaSessionRoute) -> RemoteStartRequest {
        RemoteStartRequest {
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: route.incarnation_id.clone(),
            user_id: route.user_id,
            source_size: 4_096,
            source_mtime: 1_700_000_000,
            typeless_playlist: false,
            library_channel: None,
            request: staged_candidate_request(),
        }
    }

    /// The real source snapshot the seam reads. `0/0` is the codebase's
    /// "this placement never saw the file" sentinel and would refuse the
    /// successor a takeover forever, so the fixture carries a readable one.
    fn staged_source_file() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 11,
            item_id: 1,
            path: std::path::PathBuf::from("/library/staged.mkv"),
            size: 4_096,
            mtime: 1_700_000_000,
            duration_ms: Some(3_600_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(18_183_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn staged_candidate_request() -> crate::transcode::SessionRequest {
        crate::transcode::SessionRequest {
            control_sequence: None,
            file_id: 11,
            playback_id: "stage-player".to_owned(),
            request_id: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: crate::transcode::SessionKind::Transcode { height: 1080 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        }
    }

    #[tokio::test]
    async fn prepared_candidate_reuses_the_ordinary_plan_for_original_and_compound_changes() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "planning-unrelated").await;
        let route = MediaSessionRoute {
            response_json: "{}".to_owned(),
            ..crate::media_sessions::takeover_eligible_route(
                "planning-session",
                "00000000-0000-4000-8000-0000000000a1",
            )
        };
        let predecessor = staged_predecessor_recipe(&route);
        let source = staged_source_file();
        let caps = caps_v2(
            r#"{"v":2,"video":[{"codec":"h264","present":["sdr"],"max_height":2160}],"audio":["aac"],"containers":["mkv","mp4"]}"#,
        );
        let mut selection = crate::playback_control::ClientSelection {
            quality: crate::playback_control::QualitySelection::Original,
            audio_track: None,
            subtitle: crate::playback_control::SubtitleSelection {
                mode: crate::playback_control::SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: crate::playback_control::CodecPolicy::Auto,
            dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
        };
        let original = plan_preparation_candidate(
            &fixture.state,
            &predecessor,
            Some(&caps),
            None,
            &selection,
            &source,
            1080,
        )
        .await
        .expect("Original plan");
        assert!(matches!(
            original.kind,
            crate::transcode::SessionKind::Copy { .. }
        ));
        assert!(!original.automatic);

        let compound_caps = caps_v2(
            r#"{"v":2,"video":[{"codec":"h264","present":["sdr"],"max_height":2160},{"codec":"hevc","present":["hdr10"],"max_height":2160}],"audio":["aac"],"containers":["mkv","mp4"]}"#,
        );
        let mut hdr_source = source.clone();
        hdr_source.video_codec = Some("hevc".into());
        hdr_source.bit_depth = Some(10);
        hdr_source.hdr = Some("hdr10".into());
        selection.codec = crate::playback_control::CodecPolicy::H264;
        selection.dynamic_range = crate::playback_control::DynamicRangePolicy::Sdr;
        let converted_original = plan_preparation_candidate(
            &fixture.state,
            &predecessor,
            Some(&compound_caps),
            None,
            &selection,
            &hdr_source,
            1080,
        )
        .await
        .expect("Original plus explicit H.264/SDR policy");
        assert!(matches!(
            converted_original.kind,
            crate::transcode::SessionKind::Transcode { .. }
        ));
        assert!(!converted_original.hdr10);

        selection.codec = crate::playback_control::CodecPolicy::Hevc;
        selection.dynamic_range = crate::playback_control::DynamicRangePolicy::Hdr10;
        let matching_original = plan_preparation_candidate(
            &fixture.state,
            &predecessor,
            Some(&compound_caps),
            None,
            &selection,
            &hdr_source,
            1080,
        )
        .await
        .expect("matching Original policy");
        assert!(matches!(
            matching_original.kind,
            crate::transcode::SessionKind::Copy { .. }
        ));

        selection.codec = crate::playback_control::CodecPolicy::Hevc;
        selection.dynamic_range = crate::playback_control::DynamicRangePolicy::Auto;
        assert!(plan_preparation_candidate(
            &fixture.state,
            &predecessor,
            Some(&compound_caps),
            None,
            &selection,
            &source,
            1080,
        )
        .await
        .is_err());

        selection.quality = crate::playback_control::QualitySelection::Manual { height: 720 };
        selection.codec = crate::playback_control::CodecPolicy::Auto;
        selection.dynamic_range = crate::playback_control::DynamicRangePolicy::Auto;
        selection.audio_track = Some(1);
        selection.audio_offset_ms = 125;
        selection.subtitle = crate::playback_control::SubtitleSelection {
            mode: crate::playback_control::SubtitleMode::Burn,
            track: Some(2),
        };
        let compound = plan_preparation_candidate(
            &fixture.state,
            &predecessor,
            Some(&caps),
            None,
            &selection,
            &source,
            1080,
        )
        .await
        .expect("compound plan");
        assert_eq!(
            compound.kind,
            crate::transcode::SessionKind::Transcode { height: 720 }
        );
        assert_eq!(compound.audio_index, Some(1));
        assert_eq!(compound.audio_offset_ms, 125);
        assert_eq!(compound.subtitle_burn, Some(2));

        let mut rolling_predecessor = predecessor;
        rolling_predecessor.request.presentation = crate::transcode::Presentation::Live;
        let rolling = plan_preparation_candidate(
            &fixture.state,
            &rolling_predecessor,
            Some(&caps),
            None,
            &selection,
            &source,
            1080,
        )
        .await
        .expect("rolling plan");
        assert_eq!(rolling.presentation, crate::transcode::Presentation::Live);
    }

    /// Drive the **ingress** control gate, not the helper behind it.
    ///
    /// That gate returns before `verify_authority` ever runs, so a version of
    /// this test that called the classifier directly would pass with the gate
    /// reverted to its old unconditional 425 — which is the one fact this
    /// change turns on. An adversarial review found exactly that mutation
    /// surviving, so this posts a real exchange at a durable route instead.
    #[tokio::test]
    async fn the_ingress_control_gate_answers_a_lost_owner_gone() {
        let dir = crate::test_tempdir().expect("state dir");
        let fixture = HlsDeliveryFixture::publish(dir.path(), "control-loss-unrelated").await;
        let user = fixture
            .store
            .create_user("control-loss", "hash", false)
            .await
            .expect("control-loss user");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: incarnation_id.clone(),
                session_id: session_id.clone(),
                user_id: user.id,
                playback_id: "control-loss".to_owned(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: "former-owner".to_owned(),
                // Active, long outside its lease, and serving EVENT — a
                // readable recipe that no takeover can ever act on, which is
                // the dominant real case rather than a malformed one. The
                // response is a real one too, because control admission needs
                // a bootstrap before it reaches the gate under test.
                lease_expires_at_ms: 2,
                recipe_json: crate::media_sessions::takeover_eligible_route(
                    &session_id,
                    &incarnation_id,
                )
                .recipe_json
                .replace("\"typeless_playlist\":true", "\"typeless_playlist\":false")
                .replace("\"user_id\":7", &format!("\"user_id\":{}", user.id)),
                response_json: serde_json::to_string(&StartResponse {
                    session_id: session_id.clone(),
                    playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
                    duration_ms: Some(60_000),
                    start_seconds: 0.0,
                    media_origin_ms: Some(0),
                    height: 720,
                    encoder: "software".to_owned(),
                    vod: false,
                    ladder: vec![],
                    prior_kbps: None,
                    delivered_dynamic_range: Some("sdr".to_owned()),
                    delivered_dolby_vision_profile: None,
                    control: crate::playback_control::ControlBootstrap::new(
                        &session_id,
                        &incarnation_id,
                        1,
                        crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
                    ),
                    plan_notes: Vec::new(),
                })
                .expect("start response"),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms: 1,
            },
        )
        .await;

        let body = Bytes::from(
            serde_json::to_vec(&control_request(incarnation_id.clone())).expect("control request"),
        );
        let response = control_inner(
            fixture.state.clone(),
            session_id,
            body,
            unix_ms().saturating_add(4_000),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::GONE,
            "a control exchange against an owner nothing can replace must not ask for a retry"
        );
        let (_, body) = control_body(response).await;
        assert_eq!(body["code"], "owner_lost");
        assert!(
            body.get("retry_after_ms").is_none(),
            "a hint is what all three reporters read as an instruction to keep going"
        );
        assert_eq!(body["generation"], incarnation_id);
    }

    /// Its counterpart, through the same gate: a session a survivor can still
    /// claim keeps the retryable answer and its hint.
    #[tokio::test]
    async fn the_ingress_control_gate_keeps_a_claimable_owner_retryable() {
        let route = {
            let mut route = eligible_owner_loss_route();
            route.lease_expires_at_ms = 1;
            route
        };
        let (status, body) = control_body(control_owner_answer(&route, Some(3), NOW_MS)).await;
        assert_eq!(status, StatusCode::TOO_EARLY);
        assert_eq!(body["code"], "owner_transition");
        assert_eq!(body["retry_after_ms"], 500);
        assert_eq!(
            body["control_epoch"], 3,
            "the client still needs the epoch it was fenced against"
        );
    }

    /// And the loss answer carries it too, when the caller has one.
    #[tokio::test]
    async fn the_loss_answer_reports_the_epoch_its_caller_was_fenced_against() {
        let mut route = untakeoverable_owner_loss_route();
        route.lease_expires_at_ms = 1;

        let (status, body) = control_body(control_owner_answer(&route, Some(3), NOW_MS)).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["code"], "owner_lost");
        assert_eq!(body["control_epoch"], 3);
        assert_eq!(body["generation"], route.incarnation_id);
    }

    /// The relay passes `owner_epoch` as an `Option`, and a peer's answer is
    /// validated against a strict `(status, code)` allowlist before it reaches
    /// a client — so the owner-side shape has to be a legal control body too.
    #[tokio::test]
    async fn the_relay_shaped_loss_answer_is_a_legal_control_body() {
        let mut route = untakeoverable_owner_loss_route();
        route.lease_expires_at_ms = 1;

        let (status, body) = control_body(control_owner_answer(&route, None, NOW_MS)).await;
        assert_eq!(status, StatusCode::GONE);
        let parsed: crate::playback_control::ControlErrorBody =
            serde_json::from_value(body).expect("the relay deserializes this exact shape");
        assert!(
            parsed.is_valid_for_status(status.as_u16()),
            "a body the relay would refuse never reaches the client at all"
        );
        assert_eq!(parsed.code, "owner_lost");
        assert!(parsed.retry_after_ms.is_none());
        assert!(
            parsed.control_epoch.is_none(),
            "the relay has no epoch to report and must not invent one"
        );
    }

    /// A committed replacement is pending publication with a live lease, so it
    /// is a transition on this plane too — and its recipe, being a VOD
    /// handle's, is one no takeover would ever accept. Classifying on the
    /// recipe alone would end a session whose successor is alive.
    #[tokio::test]
    async fn a_publication_fence_with_a_live_lease_stays_retryable_on_the_control_plane() {
        let mut route = untakeoverable_owner_loss_route();
        route.lease_expires_at_ms = NOW_MS + 1;
        route.publication_ready_at_ms =
            NOW_MS + plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS;

        let (status, body) = control_body(control_owner_answer(&route, Some(3), NOW_MS)).await;
        assert_eq!(status, StatusCode::TOO_EARLY);
        assert_eq!(body["code"], "owner_transition");
    }

    /// The two blocked-GET caps answer a client differently, because they mean
    /// different things to it.
    ///
    /// Both used to answer `segment_wait_busy` with a message about "this
    /// session", which was wrong for half of them: a node at its own ceiling is
    /// not this player asking for too much, and a client told to slow its own
    /// requests does the wrong thing about it. Both still answer 503 — the
    /// retry ladder is unchanged — but the code and the sentence say which
    /// limit was reached.
    #[tokio::test]
    async fn the_two_blocked_get_caps_answer_a_client_apart() {
        use crate::waitpool::WaitRefused;

        let (viewer_status, viewer) = error_body(vod_error(
            "sess-a",
            crate::vodserve::VodError::Busy(WaitRefused::SessionBusy),
        ))
        .await;
        assert_eq!(viewer_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(viewer["code"], "segment_wait_busy");
        assert!(
            viewer["message"]
                .as_str()
                .expect("a message")
                .contains("this session"),
            "the per-viewer refusal says whose limit it was"
        );

        let (node_status, node) = error_body(vod_error(
            "sess-a",
            crate::vodserve::VodError::Busy(WaitRefused::PoolFull),
        ))
        .await;
        assert_eq!(
            node_status,
            StatusCode::SERVICE_UNAVAILABLE,
            "still retryable — the ladder does not change"
        );
        assert_eq!(node["code"], "node_wait_capacity");
        assert_ne!(
            node["code"], viewer["code"],
            "a client cannot act on a distinction it cannot see"
        );
        assert!(
            node["message"]
                .as_str()
                .expect("a message")
                .contains("server"),
            "and the node refusal does not blame the player for it"
        );
    }

    /// The detail object is data. A route that somehow carried a `code` field
    /// must not be able to rename the error a client dispatches on.
    #[tokio::test]
    async fn typed_detail_cannot_shadow_the_code_a_client_dispatches_on() {
        let error = ApiError::typed_detail(
            StatusCode::GONE,
            "media_owner_lost",
            "message",
            serde_json::json!({ "code": "not_found", "message": "spoofed", "extra": 1 }),
        );
        let (status, body) = error_body(error).await;

        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["code"], "media_owner_lost");
        assert_eq!(body["message"], "message");
        assert_eq!(body["extra"], 1);
    }

    #[test]
    fn delayed_resolved_replay_rechecks_the_current_lease_boundary() {
        let mut route = MediaSessionRoute {
            recovery_epoch: String::new(),
            incarnation_id: "00000000-0000-4000-8000-0000000000d1".to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000d2".to_owned(),
            user_id: 7,
            playback_id: "replay-player".to_owned(),
            request_fingerprint: "d".repeat(64),
            owner_node_id: "node-d".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: 2_001,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: 1,
            drain_deadline_ms: None,
        };
        assert!(resolved_replay_is_live(&route, 1_000));
        route.publication_ready_at_ms = 1_001;
        assert!(
            !resolved_replay_is_live(&route, 1_000),
            "a durable predecessor-handoff fence blocks idempotent replay"
        );
        route.publication_ready_at_ms = 0;
        assert!(!resolved_replay_is_live(&route, 1_001));
        route.lease_expires_at_ms = 1_000;
        assert!(
            !resolved_replay_is_live(&route, 1_000),
            "a read that returns after exact expiry must never answer a replay"
        );
    }

    #[test]
    fn activation_confirmation_rejects_a_takeover_epoch_with_reused_ids() {
        let incarnation_id = "00000000-0000-4000-8000-0000000000e1";
        let session_id = "00000000-0000-4000-8000-0000000000e2";
        let fingerprint = "e".repeat(64);
        let recipe_json = "{}".to_owned();
        let response_json = r#"{"session":"confirmed"}"#.to_owned();
        let activation = MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: incarnation_id.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id: "activation-confirmation".to_owned(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: fingerprint.clone(),
            owner_node_id: "node-e".to_owned(),
            recipe_json: recipe_json.clone(),
            response_json: response_json.clone(),
            publication_ready_at_ms: 0,
            media_origin_ms: 0,
            now_ms: unix_ms(),
            lease_expires_at_ms: unix_ms() + 60_000,
        };
        let mut route = MediaSessionRoute {
            recovery_epoch: String::new(),
            incarnation_id: incarnation_id.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id: "activation-confirmation".to_owned(),
            request_fingerprint: fingerprint,
            owner_node_id: "node-e".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: unix_ms() + 60_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json,
            response_json,
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: unix_ms(),
            drain_deadline_ms: None,
        };
        assert!(route_matches_activation(&route, &activation));
        route.owner_epoch = 2;
        assert!(
            !route_matches_activation(&route, &activation),
            "a delayed epoch-one confirmation cannot adopt a same-id takeover"
        );
    }

    #[test]
    fn idempotent_publication_replay_accepts_the_current_takeover_epoch() {
        let now_ms = unix_ms();
        let observed = MediaSessionRoute {
            recovery_epoch: String::new(),
            incarnation_id: "00000000-0000-4000-8000-0000000000e3".to_owned(),
            session_id: "00000000-0000-4000-8000-0000000000e4".to_owned(),
            user_id: 7,
            playback_id: "publication-replay".to_owned(),
            request_fingerprint: "f".repeat(64),
            owner_node_id: "departed-owner".to_owned(),
            owner_epoch: 1,
            lease_expires_at_ms: now_ms + 30_000,
            state: "active".to_owned(),
            terminal_reason: None,
            publication_ready_at_ms: 0,
            recipe_json: r#"{"recipe":1}"#.to_owned(),
            response_json: r#"{"session":"ready"}"#.to_owned(),
            produced_playable_through_ms: 0,
            fetched_through_ms: 0,
            media_origin_ms: 0,
            media_sequence: 0,
            discontinuity_sequence: 0,
            updated_at_ms: now_ms,
            drain_deadline_ms: None,
        };
        let mut published = observed.clone();
        published.owner_node_id = "surviving-owner".to_owned();
        published.owner_epoch = 2;
        published.lease_expires_at_ms = now_ms + 60_000;
        published.discontinuity_sequence = 1;
        published.updated_at_ms = now_ms + 1;
        assert!(
            replay_publication_matches(&published, &observed),
            "takeover may advance only mutable ownership/progress coordinates"
        );
        published.response_json = r#"{"session":"different"}"#.to_owned();
        assert!(
            !replay_publication_matches(&published, &observed),
            "replay still requires the exact persisted response identity"
        );
    }

    #[tokio::test]
    async fn delayed_start_abort_cannot_reap_takeover_or_unmapped_vod() {
        let dir = crate::test_tempdir().expect("state dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let fixture =
            HlsDeliveryFixture::publish_takeover(dir.path(), &session_id, &incarnation_id, 2).await;

        abort_started_session(
            &fixture.state,
            &fixture.state.node_id,
            &incarnation_id,
            &session_id,
        )
        .await;
        assert!(
            fixture.worker_is_registered(&session_id).await,
            "the epoch-one call-site composition must leave a same-id epoch-two worker alive"
        );
        assert!(
            fixture
                .state
                .transcode
                .stop_session_for_owner(&incarnation_id, &session_id, 2, "test cleanup",)
                .await
        );

        let vod_session = uuid::Uuid::new_v4().to_string();
        let unmapped_incarnation = uuid::Uuid::new_v4().to_string();
        let _vod_owner = install_vod_http_session(&fixture, dir.path(), &vod_session).await;
        assert!(
            fixture
                .state
                .transcode
                .vod_owns_or_preparing(&vod_session)
                .await
        );
        assert!(
            !fixture
                .state
                .transcode
                .stop_vod_session_for_request(
                    &unmapped_incarnation,
                    &vod_session,
                    "delayed start abort",
                )
                .await,
            "a real VOD capability without the exact request mapping cannot be reaped"
        );
        assert!(
            fixture
                .state
                .transcode
                .vod_owns_or_preparing(&vod_session)
                .await,
            "the rejected VOD cleanup leaves the capability intact"
        );
        fixture
            .state
            .transcode
            .begin_session_terminal(
                &vod_session,
                crate::vodserve::Terminal::Replaced,
                "test cleanup",
            )
            .await;
    }

    /// A split child logs under its parent's target. `playlist_error` moved to
    /// `http/hls/playlist.rs`, a `#[path]` child whose default target is
    /// `plurxd::http::hls::playlist`; the console, journald and the log view
    /// label the line with the target, and a move does not change a log line.
    #[test]
    fn a_line_logged_by_an_hls_child_keeps_the_hls_target() {
        use tracing_subscriber::prelude::*;
        let logs = std::sync::Arc::new(crate::logbuf::LogBuffer::new(8));
        let guard = crate::test_tracing_default(
            tracing_subscriber::registry()
                .with(crate::logbuf::BufferLayer(std::sync::Arc::clone(&logs))),
        );
        let _ = playlist_error("sess-target", PlaylistError::SessionGone);
        drop(guard);
        let refused = logs
            .tail("trace", 8)
            .into_iter()
            .find(|entry| entry.message.contains("HLS playlist request refused"))
            .expect("the refusal is logged");
        assert_eq!(refused.target, "plurxd::http::hls");
    }
