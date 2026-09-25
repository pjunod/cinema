
    #[tokio::test]
    async fn repeated_seeks_with_old_buffers_remain_accepted_at_the_control_endpoint() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;
        let mut request = control_request(route.incarnation_id.clone());
        request.position_ms = 10_000;
        request.buffered_from_ms = Some(8_000);
        request.buffered_through_ms = 24_000;
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

        let seeks = [
            (10_000, Some(8_000), 24_000, 600_000),
            (600_000, Some(598_000), 620_000, 10_000),
            // Retained incident: the requested landing and media clock
            // disagreed; that evidence must still reach the control actor.
            (797_711, Some(797_711), 809_211, 729_643),
            (797_711, None, 797_711, 729_643),
        ];
        for (index, (position, from, through, target)) in
            seeks.into_iter().cycle().take(24).enumerate()
        {
            // Honor the real exchange cadence rather than bypassing the
            // handler/actor boundary that the previous seek-storm test missed.
            tokio::time::sleep(std::time::Duration::from_millis(260)).await;
            let sequence = index as u64 + 2;
            request.sequence = sequence;
            request.position_ms = position;
            request.buffered_from_ms = from;
            request.buffered_through_ms = through;
            request.render_state = crate::playback_control::RenderState::Seeking;
            request.seek_target_ms = Some(target);
            let (status, body) = control_body(
                control_local_inner(
                    &fixture.state,
                    &route,
                    request.clone(),
                    unix_ms().saturating_add(4_000),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "sequence {sequence}: {body}");
            assert_eq!(body["accepted_sequence"], sequence);
            assert_eq!(body["delivery"]["client_runway_ms"], 0);
        }
        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence += 1;
        request.position_ms = 729_643;
        request.buffered_from_ms = Some(729_643);
        request.buffered_through_ms = 749_643;
        request.render_state = crate::playback_control::RenderState::Rendering;
        request.seek_target_ms = None;
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
        assert_eq!(status, StatusCode::OK, "settled playback: {body}");
        assert_eq!(body["accepted_sequence"], 26);
        assert_eq!(body["delivery"]["client_runway_ms"], 20_000);
    }

    /// M6 §3.4's acceptance, stated in the handoff: *"with a test client
    /// reporting `dual_player_preparation: true` on a resolution-and-delivery-
    /// method change with headroom, the ledger holds a staged successor and
    /// the pointer still names the predecessor."*
    ///
    /// Both halves matter and the second is the one that would go wrong
    /// quietly. `activate_media_session` — the call every other durable
    /// successor in this codebase goes through — reaps the predecessor and
    /// advances the pointer unconditionally, so a preparation built on it
    /// would retire the stream the viewer is still watching. This asserts the
    /// pointer is untouched, which is what says `prepare_media_session` is the
    /// entry point actually being used.
    #[tokio::test]
    async fn a_prepared_transition_stages_a_successor_and_leaves_the_pointer_alone() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
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
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        assert_eq!(
            staged.expected_predecessor_incarnation_id, route.incarnation_id,
            "the commit CAS is fenced on the generation that was current when it staged"
        );
        assert_ne!(staged.staged_incarnation_id, route.incarnation_id);

        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("route while staged")
                .expect("the playback still has a pointer")
                .incarnation_id,
            route.incarnation_id,
            "staging must not advance the pointer — `activate_media_session` would have",
        );
    }

    /// A surviving prepared route describes the real engine behind it. The
    /// production path attaches that VOD worker before actor publication; the
    /// metadata-only test helper exercises the payload construction itself.
    #[tokio::test]
    async fn a_prepared_successor_publishes_the_vod_engine_identity() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
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
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        let published = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged.staged_incarnation_id)
            .await
            .expect("staged route read")
            .expect("the staged successor has a durable route");
        let response: serde_json::Value =
            serde_json::from_str(&published.response_json).expect("the published start response");

        assert_eq!(
            response["encoder"], "vod",
            "a successor that survived reserve-and-prime must not identify itself as metadata"
        );
    }

    /// M6 §3.4c's acceptance at the actual control seam. The stage happens
    /// outside the exchange that requested it; the next exchange reads the
    /// durable row, has the same owner-local slot authorize it, and returns a
    /// transaction whose identity survives an exact replay.
    #[tokio::test]
    async fn a_staged_successor_is_announced_without_moving_the_pointer_and_replays_exactly() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
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
            &staged_candidate_request(),
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
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        let staged_route = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged.staged_incarnation_id)
            .await
            .expect("staged route read")
            .expect("the ledger names a route");

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
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
        let first = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("control response");
        assert_eq!(
            first.delivery.presentation, "vod",
            "the acceptance must run through the engine public sessions use",
        );
        let crate::playback_control::ControlAction::Prepare {
            action_id,
            session_id: announced_session_id,
            playlist_url,
            control,
            media_origin_ms,
            effective_selection,
        } = &first.action
        else {
            panic!("the next exchange must announce the staged successor");
        };
        assert!(uuid::Uuid::parse_str(action_id).is_ok());
        assert_eq!(announced_session_id, &staged_route.session_id);
        assert_eq!(media_origin_ms, &staged_route.media_origin_ms);
        let start = control_start_response(&staged_route).expect("staged response");
        assert_eq!(control, &start.control,
            "the next reporter must use the staged successor's own control identity");
        let recipe = serde_json::from_str::<RemoteStartRequest>(&staged_route.recipe_json)
            .expect("staged recipe");
        assert_eq!(playlist_url, &start.playlist_url);
        assert_eq!(
            effective_selection,
            &crate::playback_control::EffectiveSelection::from_recipe(
                &recipe,
                start.height,
                start.delivered_dynamic_range,
            ),
        );
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("the predecessor remains current")
                .incarnation_id,
            route.incarnation_id,
            "announcing a preparation must not advance the pointer",
        );

        let (status, replay_body) = control_body(
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
        let replay =
            serde_json::from_value::<crate::playback_control::ControlResponseV1>(replay_body)
                .expect("exact replay response");
        assert_eq!(replay.action, first.action);
        assert_eq!(replay.accepted_sequence, first.accepted_sequence);

        let mut replay_request = request;
        replay_request.supported_actions = None;
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                replay_request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "stale_control");
    }

    #[tokio::test]
    async fn a_local_prepare_rejects_an_unsafe_durable_playlist_url() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        let gate = fixture
            .state
            .transcode
            .session_preparation_gate(&session_id)
            .await
            .expect("live predecessor gate");
        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        let staged_session_id = uuid::Uuid::new_v4().to_string();
        let mut staged_request = staged_candidate_request();
        staged_request.file_id = fixture.file_id();
        staged_request.request_id = Some(staged_incarnation_id.clone());
        let response = StartResponse {
            session_id: staged_session_id.clone(),
            playlist_url: format!("//attacker.invalid/{staged_session_id}/index.m3u8"),
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
                &staged_incarnation_id,
                1,
                crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            ),
            plan_notes: Vec::new(),
        };
        let now_ms = unix_ms();
        let preparation = plurx_core::domain::MediaSessionPreparation {
            expected_desired_revision: None,
            incarnation_id: staged_incarnation_id.clone(),
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
                incarnation_id: staged_incarnation_id,
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
        };
        let executor = crate::playback_control::PreparationExecutor::new(
            Arc::clone(&fixture.state.store),
            gate,
            route.user_id,
            route.playback_id.clone(),
            route.owner_node_id.clone(),
            route.owner_epoch,
        );
        assert!(executor
            .stage(&preparation)
            .await
            .expect("stage hostile row"));

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
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
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");
    }

    #[tokio::test]
    async fn a_transient_staged_read_failure_does_not_advance_or_rotate_the_action() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
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
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
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
        let first = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("first response");
        assert!(matches!(
            first.action,
            crate::playback_control::ControlAction::Prepare { .. }
        ));

        fail_next_staged_read(&route.incarnation_id);
        let (status, body) = control_body(
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
        let replay = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("cached exact replay");
        assert_eq!(replay.accepted_sequence, first.accepted_sequence);
        assert_eq!(replay.action, first.action);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        fail_next_staged_read(&route.incarnation_id);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");

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
        assert_eq!(status, StatusCode::OK);
        let recovered = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("recovered response");
        assert_eq!(recovered.accepted_sequence, 2);
        assert_eq!(recovered.action, first.action);
    }

    #[tokio::test]
    async fn a_client_without_prepare_vocabulary_skips_the_staged_store_read() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
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
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;

        let mut request = control_request(route.incarnation_id.clone());
        fail_next_staged_read(&route.incarnation_id);
        let (status, body) = control_body(
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
        let passive = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("passive response");
        assert_eq!(passive.action, crate::playback_control::ControlAction::None);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "control_unavailable");

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
        assert_eq!(status, StatusCode::OK);
        let prepared = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("prepared response");
        assert!(matches!(
            prepared.action,
            crate::playback_control::ControlAction::Prepare { .. }
        ));
    }

    /// The detached decision boundary stages an admitted candidate and leaves
    /// the predecessor pointer alone. The production control-to-boundary
    /// wiring is covered separately below.
    #[tokio::test]
    async fn an_admitted_preparation_candidate_reaches_the_ledger() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        // The seam reads the source, and staging refuses without a snapshot.
        // Its 2160 height is also load-bearing: it is what makes the viewer's
        // 1080 ask cross the delivery method as well as the resolution.
        let source = staging_source(&fixture).await;

        // A 2160p copy being delivered, and the viewer asks for 1080p: the
        // copy becomes a transcode, so height and delivery method move
        // together — the one pair with a hardware receipt.
        let mut recipe = staged_candidate_request();
        recipe.file_id = source.id;
        recipe.kind = crate::transcode::SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        // 2160 is what is being *delivered*; the source row's own height is
        // deliberately not required. `candidate_request` turns a copy into a
        // transcode whenever the asked height is not the source's, and an
        // unprobed source is `None`, so the copy → transcode arm fires either
        // way — which is the delivery-method half of the transition.
        let delivered =
            crate::playback_control::EffectiveSelection::from_request(&recipe, 2160, None);
        assert_eq!(delivered.codec, "source", "the predecessor direct-plays");
        let asked_height = 1080;

        process_preparation_candidate(
            fixture.state.clone(),
            PendingCandidateGuard::begin(&route.playback_id, "test-digest"),
            PreparationCandidateInputs {
                session_id: session_id.clone(),
                route: route.clone(),
                recipe: RemoteStartRequest {
                    protocol_version: crate::media_pool::PROTOCOL_VERSION,
                    incarnation_id: route.incarnation_id.clone(),
                    user_id: route.user_id,
                    source_size: 0,
                    source_mtime: 0,
                    typeless_playlist: false,
                    library_channel: None,
                    request: recipe,
                },
                planning_caps: None,
                planning_overrides: None,
                selection: crate::playback_control::ClientSelection {
                    quality: crate::playback_control::QualitySelection::Manual {
                        height: asked_height,
                    },
                    audio_track: None,
                    subtitle: crate::playback_control::SubtitleSelection {
                        mode: crate::playback_control::SubtitleMode::Off,
                        track: None,
                    },
                    audio_offset_ms: 0,
                    codec: crate::playback_control::CodecPolicy::Auto,
                    dynamic_range: crate::playback_control::DynamicRangePolicy::Auto,
                },
                // Ten times the delivered rate: well clear of the floor the
                // 2026-09-03 run had to be re-run to satisfy.
                observed_download_bps: Some(100_000_000),
                delivered: delivered.clone(),
                delivered_bps: Some(10_000_000),
                capabilities: Some(crate::playback_control::DynamicCapabilities {
                    platform: crate::playback_control::ClientPlatform::Apple,
                    max_height: 2160,
                    codecs: vec![crate::playback_control::CodecPolicy::H264],
                    dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
                    // The retained field, read rather than re-derived.
                    dual_player_preparation: true,
                }),
                platform: crate::playback_control::ClientPlatform::Apple,
                purpose: PreparationPurpose::SelectionChange,
                accepted_film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
            },
        )
        .await;

        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("the seam staged a successor");
        assert_eq!(
            staged.expected_predecessor_incarnation_id,
            route.incarnation_id
        );
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("route while staged")
                .expect("the playback still has a pointer")
                .incarnation_id,
            route.incarnation_id,
            "and the viewer's own generation is still the current one",
        );
    }

    /// The complete production seam: an accepted selection change schedules
    /// the detached stage, and the next accepted sequence announces the row.
    /// Capabilities are omitted after sequence one exactly as shipped clients
    /// do, proving the actor-retained document is the one used.
    #[tokio::test]
    async fn an_accepted_selection_change_is_staged_and_announced_on_the_next_exchange() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut first = control_request(route.incarnation_id.clone());
        first.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        first.observed_download_bps = Some(100_000_000);
        first.capabilities = Some(crate::playback_control::DynamicCapabilities {
            platform: crate::playback_control::ClientPlatform::Web,
            max_height: 2160,
            codecs: vec![crate::playback_control::CodecPolicy::H264],
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dual_player_preparation: true,
        });
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                first.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        let mut changed = first.clone();
        changed.sequence = 2;
        changed.capabilities = None;
        changed.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                changed.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let changed_response =
            serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
                .expect("selection-change response");
        assert_eq!(
            changed_response.action,
            crate::playback_control::ControlAction::None
        );

        let staged = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(staged) = fixture
                    .state
                    .store
                    .staged_media_session_for_playback(route.user_id, &route.playback_id)
                    .await
                    .expect("ledger read")
                {
                    break staged;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached production seam staged the successor");

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        changed.sequence = 3;
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                changed,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let announced = serde_json::from_value::<crate::playback_control::ControlResponseV1>(body)
            .expect("announcement response");
        let staged_route = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged.staged_incarnation_id)
            .await
            .expect("staged route read")
            .expect("staged route");
        assert!(matches!(
            announced.action,
            crate::playback_control::ControlAction::Prepare { ref session_id, .. }
                if staged_route.session_id == *session_id
        ));
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("predecessor remains current")
                .incarnation_id,
            route.incarnation_id,
        );
    }

    /// The production seam with a client that cannot safely hold two players.
    /// Nothing stages, which proves the retained capability reaches the
    /// detached decision rather than being re-derived from the platform.
    #[tokio::test]
    async fn a_client_that_cannot_prepare_stages_nothing_at_the_seam() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;
        fixture.set_delivered_bps_for_test(10_000_000);

        let mut first = control_request(route.incarnation_id.clone());
        first.supported_actions = Some(vec!["prepare_replacement".to_owned()]);
        first.observed_download_bps = Some(100_000_000);
        first.capabilities = Some(crate::playback_control::DynamicCapabilities {
            platform: crate::playback_control::ClientPlatform::Web,
            max_height: 2160,
            codecs: vec![crate::playback_control::CodecPolicy::H264],
            dynamic_ranges: vec![crate::playback_control::DynamicRangePolicy::Sdr],
            dual_player_preparation: false,
        });
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                first.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        first.sequence = 2;
        first.capabilities = None;
        first.selection.quality =
            crate::playback_control::QualitySelection::Manual { height: 1080 };
        let (status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                first,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // The candidate runs detached. Wait for the completion marker keyed
        // to this exact predecessor, rather than inferring completion from a
        // process-global in-flight count that may still be zero before the
        // spawned future receives its first poll.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if take_preparation_candidate_completion(&route.incarnation_id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached preparation decision completed");

        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "a client that cannot hold two pipelines is not given a successor"
        );
    }

    /// A second `Prepare` on the same playback finds the actor's one slot
    /// already taken. It is refused rather than queued, and refusing is
    /// counted separately from deciding, because the two answer different
    /// questions and only the second says whether the ledger moved.
    #[tokio::test]
    async fn a_second_prepared_transition_is_refused_by_the_one_successor_slot() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let first = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("the first staged")
            .staged_incarnation_id;

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        assert_eq!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read")
                .expect("still exactly one")
                .staged_incarnation_id,
            first,
            "the second is refused rather than replacing the first"
        );
    }

    /// A playback with no live local worker has no slot to take. Staging a row
    /// the actor never holds would leave it for the maintenance backstop while
    /// it counted against the user's admission cap the whole time.
    #[tokio::test]
    async fn a_playback_with_no_live_worker_stages_nothing() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _session_id, route) = staging_fixture(dir.path()).await;

        stage_prepared_successor(
            &fixture.state,
            &uuid::Uuid::new_v4().to_string(),
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
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
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read")
                .is_none(),
            "the ledger is untouched"
        );
    }

    async fn acknowledge_rolling_preparation(
        fixture: &HlsDeliveryFixture,
        session_id: &str,
        route: &MediaSessionRoute,
        state: crate::playback_control::AcknowledgementState,
        transient_failures: usize,
    ) -> (serde_json::Value, String) {
        stage_prepared_successor(
            &fixture.state,
            session_id,
            route,
            &staged_predecessor_recipe(route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let staged_incarnation_id = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged")
            .staged_incarnation_id;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (prepare_status, prepare_body) = control_body(
            control_local_inner(
                &fixture.state,
                route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(prepare_status, StatusCode::OK);
        assert_eq!(
            prepare_body["delivery"]["presentation"], "live-recovery",
            "without a VOD registration the public rolling engine must answer"
        );
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        tokio::time::sleep(Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state,
            buffered_through_ms: None,
            // Only a commit carries an origin, and it must be the offered one.
            committed_media_origin_ms: (state
                == crate::playback_control::AcknowledgementState::Committed)
                .then(|| {
                    prepare_body["action"]["media_origin_ms"]
                        .as_i64()
                        .expect("offer origin")
                }),
            first_frame_unix_ms: (state
                == crate::playback_control::AcknowledgementState::Committed)
                .then(unix_ms),
        });
        if transient_failures > 0 {
            fail_next_preparation_settlements(&route.incarnation_id, transient_failures);
        }
        let (status, body) = control_body(
            control_local_inner(
                &fixture.state,
                route,
                request,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["action"]["type"], "none");
        assert_eq!(
            body["delivery"]["presentation"], "live-recovery",
            "the terminal acknowledgement must propagate through the public rolling engine"
        );
        (body, staged_incarnation_id)
    }

    #[tokio::test]
    async fn a_rolling_preparation_commit_reaches_the_store() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        let (_, staged_incarnation_id) = acknowledge_rolling_preparation(
            &fixture,
            &session_id,
            &route,
            crate::playback_control::AcknowledgementState::Committed,
            0,
        )
        .await;

        let current = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("current route")
            .expect("the successor is current");
        assert_eq!(current.incarnation_id, staged_incarnation_id);
        assert!(matches!(
            fixture
                .state
                .media_sessions
                .authoritative_route_resolution_before(
                    &current.session_id,
                    &fixture.state.node_id,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("control-plane route while draining"),
            DurableRouteResolution::OwnerTransition(_)
        ));
        assert!(matches!(
            fixture
                .state
                .media_sessions
                .authoritative_media_route_resolution_before(
                    &current.session_id,
                    &fixture.state.node_id,
                    Instant::now() + Duration::from_secs(1),
                )
                .await
                .expect("prepared media route while draining"),
            DurableRouteResolution::ActiveLocal(_)
        ));
        let predecessor = fixture
            .state
            .store
            .media_session_route(&session_id)
            .await
            .expect("predecessor route")
            .expect("the predecessor remains readable");
        // Still serving, on purpose. The commit gives it a drain deadline
        // instead of retiring it, so a client that has not finished switching
        // keeps getting media until its owner's next tick past the deadline.
        assert_eq!(predecessor.state, "active");
        assert_eq!(
            predecessor.terminal_reason, None,
            "nothing has decided a terminal cause for the predecessor yet"
        );
        assert!(fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger after rolling commit")
            .is_none());
    }

    #[tokio::test]
    async fn a_rolling_preparation_abort_reaches_the_store() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        let (_, staged_incarnation_id) = acknowledge_rolling_preparation(
            &fixture,
            &session_id,
            &route,
            crate::playback_control::AcknowledgementState::Failed,
            0,
        )
        .await;

        let current = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("current route")
            .expect("the predecessor stays current");
        assert_eq!(current.incarnation_id, route.incarnation_id);
        let aborted = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged_incarnation_id)
            .await
            .expect("aborted successor route")
            .expect("the aborted successor remains readable");
        assert_eq!(aborted.state, "ended");
        assert!(fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger after rolling abort")
            .is_none());
    }

    #[tokio::test]
    async fn a_transient_store_failure_does_not_strand_a_rolling_commit() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        let (_, staged_incarnation_id) = acknowledge_rolling_preparation(
            &fixture,
            &session_id,
            &route,
            crate::playback_control::AcknowledgementState::Committed,
            1,
        )
        .await;
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("route after retried settlement")
                .expect("successor route")
                .incarnation_id,
            staged_incarnation_id,
        );
    }

    #[tokio::test]
    async fn concurrent_exact_acknowledgements_join_one_canonical_settlement() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let staged_incarnation_id = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged")
            .staged_incarnation_id;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (prepare_status, prepare_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(prepare_status, StatusCode::OK);
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();
        tokio::time::sleep(Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            // Echo the offer: a commit naming a different origin is refused.
            committed_media_origin_ms: prepare_body["action"]["media_origin_ms"].as_i64(),
            first_frame_unix_ms: Some(unix_ms()),
        });
        delay_next_preparation_settlement(&route.incarnation_id, Duration::from_millis(300));
        let first = control_local_inner(
            &fixture.state,
            &route,
            request.clone(),
            unix_ms().saturating_add(4_000),
        );
        let second = control_local_inner(
            &fixture.state,
            &route,
            request,
            unix_ms().saturating_add(4_000),
        );
        let (first, second) = tokio::join!(first, second);
        let ((first_status, first_body), (second_status, second_body)) =
            tokio::join!(control_body(first), control_body(second));
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(
            first_body, second_body,
            "both waiters receive the stored bytes"
        );
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("successor is current")
                .incarnation_id,
            staged_incarnation_id,
        );
    }

    #[tokio::test]
    async fn a_late_exact_acknowledgement_joins_the_completed_canonical_settlement() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let staged_incarnation_id = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged")
            .staged_incarnation_id;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (prepare_status, prepare_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(prepare_status, StatusCode::OK);
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();
        tokio::time::sleep(Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            // Echo the offer: a commit naming a different origin is refused.
            committed_media_origin_ms: prepare_body["action"]["media_origin_ms"].as_i64(),
            first_frame_unix_ms: Some(unix_ms()),
        });

        let (first_status, first_body) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(first_status, StatusCode::OK);

        // Model a second request that already crossed ingress with this
        // active-predecessor snapshot before the first Store call committed,
        // but reaches settlement after the first owner completed. It must
        // join the retained receipt instead of manufacturing new timestamps.
        let retry = DurablePreparationSettlementAdmission {
            state: fixture.state.clone(),
            gate: fixture
                .state
                .transcode
                .session_preparation_gate(&route.session_id)
                .await
                .expect("active preparation gate"),
            route: route.clone(),
            start: control_start_response(&route).expect("control start"),
            recipe: serde_json::from_str(&route.recipe_json).expect("control recipe"),
            request: request.clone(),
            status: fixture
                .state
                .transcode
                .hls_session_status_publication(&route.session_id)
                .await
                .and_then(|publication| publication.result.ok()),
            reservation: std::sync::Mutex::new(
                reserve_preparation_settlement(
                    &fixture.state,
                    unix_ms().saturating_add(4_000),
                    preparation_settlement_slots(),
                )
                .await,
            ),
            receipt: std::sync::Mutex::new(None),
        };
        crate::playback_control::PreparationSettlementAdmission::accepted(
            &retry,
            crate::playback_control::PreparationControlOutcome {
                disposition: crate::playback_control::ControlDisposition::Replay,
                accepted_sequence: request.sequence,
                action: crate::playback_control::ControlAction::None,
                action_suppressed: false,
                preparation_directive: crate::playback_control::PreparationDirective::Commit {
                    staged_incarnation_id,
                },
                platform: crate::playback_control::ClientPlatform::Web,
                lease_expires_at_unix_ms: unix_ms().saturating_add(10_000),
                lease_timeout_ms: 10_000,
                lease_state: "active",
                selection: crate::playback_control::SelectionObservation::default(),
            },
        );
        let replayed = retry
            .receipt()
            .expect("late retry joined retained receipt")
            .wait_before(unix_ms().saturating_add(4_000))
            .await
            .expect("retained settlement");
        let PreparationSettlement::Committed(retry_response) = replayed else {
            panic!("late exact retry must receive the committed response");
        };
        assert_eq!(
            serde_json::to_value(*retry_response).expect("retry response JSON"),
            first_body
        );
    }

    #[tokio::test]
    async fn a_detached_abort_settles_after_the_status_snapshot_disappears() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let staged_incarnation_id = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged")
            .staged_incarnation_id;
        let request = control_request(route.incarnation_id.clone());
        let admission = DurablePreparationSettlementAdmission {
            state: fixture.state.clone(),
            gate: fixture
                .state
                .transcode
                .session_preparation_gate(&route.session_id)
                .await
                .expect("the gate was captured before retirement"),
            route: route.clone(),
            start: control_start_response(&route).expect("control start"),
            recipe: serde_json::from_str(&route.recipe_json).expect("control recipe"),
            request: request.clone(),
            status: None,
            reservation: std::sync::Mutex::new(
                reserve_preparation_settlement(
                    &fixture.state,
                    unix_ms().saturating_add(4_000),
                    preparation_settlement_slots(),
                )
                .await,
            ),
            receipt: std::sync::Mutex::new(None),
        };
        crate::playback_control::PreparationSettlementAdmission::accepted(
            &admission,
            crate::playback_control::PreparationControlOutcome {
                disposition: crate::playback_control::ControlDisposition::Accepted,
                accepted_sequence: request.sequence,
                action: crate::playback_control::ControlAction::None,
                action_suppressed: false,
                preparation_directive: crate::playback_control::PreparationDirective::Abort {
                    staged_incarnation_id,
                    acknowledgement_rejected: false,
                },
                platform: crate::playback_control::ClientPlatform::Web,
                lease_expires_at_unix_ms: unix_ms(),
                lease_timeout_ms: 0,
                lease_state: "ended",
                selection: crate::playback_control::SelectionObservation::default(),
            },
        );
        assert!(matches!(
            admission
                .receipt()
                .expect("detached abort receipt")
                .wait_before(unix_ms().saturating_add(4_000))
                .await,
            Some(PreparationSettlement::Aborted)
        ));
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read after abort")
                .is_none(),
            "an Abort directive does not need a response-status snapshot"
        );
    }

    #[tokio::test]
    async fn a_fresh_takeover_engine_discards_the_departed_owners_preparation() {
        let dir = crate::test_tempdir().expect("old owner");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let staged = fixture
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger")
            .expect("old preparation");
        let now_ms = route.lease_expires_at_ms + 1;
        let adopted = fixture
            .store
            .claim_media_session_takeover(&plurx_core::domain::MediaSessionTakeover {
                incarnation_id: route.incarnation_id.clone(),
                expected_owner_node_id: route.owner_node_id.clone(),
                expected_owner_epoch: route.owner_epoch,
                next_owner_node_id: "replacement-node".into(),
                now_ms,
                lease_expires_at_ms: now_ms + 900_000,
            })
            .await
            .expect("takeover CAS")
            .expect("new owner");
        let next_dir = crate::test_tempdir().expect("new owner");
        let root = next_dir.path();
        let next = crate::state::AppState::new(
            "test".into(),
            Arc::clone(&fixture.store),
            crate::state::Dirs {
                artwork: root.join("artwork"),
                transcode: root.join("transcode"),
                cache: root.join("cache"),
                subs: root.join("subs"),
                runtime_cache: root.join("runtime"),
                renditions: root.join("renditions"),
            },
            "replacement-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        next.transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), root)
            .await;
        // This owner has never seen the old engine or its ControlState. Use
        // the same reconciliation invoked before takeover adoption publishes.
        crate::media_sessions::abort_inherited_preparation_for_test(&next, &adopted)
            .await
            .expect("cleanup before publication");
        assert!(fixture
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger")
            .is_none());
        assert_eq!(
            fixture
                .store
                .media_session_route_by_incarnation(&staged.staged_incarnation_id)
                .await
                .expect("successor")
                .expect("retained tombstone")
                .state,
            "ended"
        );
        let mut request = control_request(route.incarnation_id.clone());
        request.control_epoch = 2;
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.into()
        ]);
        let (status, _) =
            control_body(control_local_inner(&next, &adopted, request, unix_ms() + 4_000).await)
                .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the fresh owner is not blocked by departed staging"
        );
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        assert!(
            fixture
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger after stale worker")
                .is_none(),
            "an old detached worker cannot recreate the preparation after reconciliation"
        );
    }

    #[test]
    fn consecutive_owner_epochs_cannot_share_a_preparation_settlement() {
        let incarnation_id = uuid::Uuid::new_v4().to_string();
        let client_instance_id = uuid::Uuid::new_v4().to_string();
        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        assert_ne!(
            preparation_settlement_key(
                &incarnation_id,
                2,
                &client_instance_id,
                1,
                &staged_incarnation_id,
            ),
            preparation_settlement_key(
                &incarnation_id,
                3,
                &client_instance_id,
                1,
                &staged_incarnation_id,
            ),
            "each takeover owns an independent detached settlement"
        );
    }

    #[tokio::test]
    async fn preparation_capacity_and_serving_loss_refuse_before_acknowledgement_acceptance() {
        use crate::playback_control::{AcknowledgementState, ActionAcknowledgement};
        for committed in [false, true] {
            for saturated in [false, true] {
                let dir = crate::test_tempdir().expect("state dir");
                let (fixture, session_id, route) = staging_fixture(dir.path()).await;
                stage_prepared_successor(
                    &fixture.state,
                    &session_id,
                    &route,
                    &staged_predecessor_recipe(&route),
                    &staged_candidate_request(),
                    Some(&staged_source_file()),
                    AcceptedAsk {
                        film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                        desired_digest: None,
                    },
                )
                .await;
                let mut request = control_request(route.incarnation_id.clone());
                request.supported_actions = Some(vec![
                    crate::playback_control::PREPARE_REPLACEMENT_ACTION.into(),
                ]);
                let (status, body) = control_body(
                    control_local_inner(&fixture.state, &route, request.clone(), unix_ms() + 4_000)
                        .await,
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                tokio::time::sleep(Duration::from_millis(260)).await;
                request.sequence = 2;
                request.capabilities = None;
                request.acknowledgement = Some(ActionAcknowledgement {
                    action_id: body["action"]["action_id"]
                        .as_str()
                        .expect("Prepare id")
                        .into(),
                    state: if committed {
                        AcknowledgementState::Committed
                    } else {
                        AcknowledgementState::Failed
                    },
                    buffered_through_ms: None,
                    // The origin the offer named. A commit that does not
                    // echo it is refused, so the fixture has to answer the
                    // Prepare it was actually given.
                    committed_media_origin_ms: committed.then(|| {
                        body["action"]["media_origin_ms"]
                            .as_i64()
                            .expect("offer origin")
                    }),
                    first_frame_unix_ms: committed.then(unix_ms),
                });
                let slots = Arc::new(tokio::sync::Semaphore::new(1));
                let held = if saturated {
                    Some(
                        Arc::clone(&slots)
                            .acquire_owned()
                            .await
                            .expect("test permit"),
                    )
                } else {
                    fixture.state.serving.validation_set_ready(false).await;
                    None
                };
                let (status, _) = control_body(
                    control_local_with_settlement_capacity(
                        &fixture.state,
                        &route,
                        request.clone(),
                        unix_ms() + 100,
                        Arc::clone(&slots),
                    )
                    .await,
                )
                .await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert!(fixture
                    .store
                    .staged_media_session_for_playback(route.user_id, &route.playback_id)
                    .await
                    .expect("unchanged ledger")
                    .is_some());
                drop(held);
                if !saturated {
                    fixture.state.serving.validation_set_ready(true).await;
                }
                let (status, _) = control_body(
                    control_local_with_settlement_capacity(
                        &fixture.state,
                        &route,
                        request,
                        unix_ms() + 4_000,
                        slots,
                    )
                    .await,
                )
                .await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "the refused request did not consume its sequence"
                );
                assert!(fixture
                    .store
                    .staged_media_session_for_playback(route.user_id, &route.playback_id)
                    .await
                    .expect("settled ledger")
                    .is_none());
            }
        }
    }

    #[tokio::test]
    async fn ordinary_control_does_not_consume_preparation_settlement_capacity() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _, route) = staging_fixture(dir.path()).await;
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.into()
        ]);
        let (status, _) = control_body(
            control_local_with_settlement_capacity(
                &fixture.state,
                &route,
                request,
                unix_ms() + 100,
                Arc::new(tokio::sync::Semaphore::new(0)),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn admitted_preparation_settlement_holds_serving_fence_through_commit_and_abort() {
        use crate::playback_control::{AcknowledgementState, ActionAcknowledgement};
        for committed in [false, true] {
            let dir = crate::test_tempdir().expect("state dir");
            let (fixture, session_id, route) = staging_fixture(dir.path()).await;
            stage_prepared_successor(
                &fixture.state,
                &session_id,
                &route,
                &staged_predecessor_recipe(&route),
                &staged_candidate_request(),
                Some(&staged_source_file()),
                AcceptedAsk {
                    film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                    desired_digest: None,
                },
            )
            .await;
            let staged = fixture
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger")
                .expect("staged row");
            let mut request = control_request(route.incarnation_id.clone());
            request.supported_actions = Some(vec![
                crate::playback_control::PREPARE_REPLACEMENT_ACTION.into(),
            ]);
            let (status, body) = control_body(
                control_local_inner(&fixture.state, &route, request.clone(), unix_ms() + 4_000)
                    .await,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            tokio::time::sleep(Duration::from_millis(260)).await;
            request.sequence = 2;
            request.capabilities = None;
            request.acknowledgement = Some(ActionAcknowledgement {
                action_id: body["action"]["action_id"]
                    .as_str()
                    .expect("Prepare id")
                    .into(),
                state: if committed {
                    AcknowledgementState::Committed
                } else {
                    AcknowledgementState::Failed
                },
                buffered_through_ms: None,
                committed_media_origin_ms: committed.then(|| {
                    body["action"]["media_origin_ms"]
                        .as_i64()
                        .expect("offer origin")
                }),
                first_frame_unix_ms: committed.then(unix_ms),
            });
            let key = preparation_settlement_key(
                &route.incarnation_id,
                route.owner_epoch,
                &request.client_instance_id,
                2,
                &staged.staged_incarnation_id,
            );
            delay_next_preparation_settlement(&route.incarnation_id, Duration::from_millis(400));
            let task_state = fixture.state.clone();
            let task_route = route.clone();
            let waiter = tokio::spawn(async move {
                control_local_inner(&task_state, &task_route, request, unix_ms() + 4_000).await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if preparation_settlements()
                        .lock()
                        .expect("settlement map")
                        .contains_key(&key)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("actor admitted settlement");
            let fence = fixture.state.serving.clone();
            let loss = tokio::spawn(async move { fence.validation_set_ready(false).await });
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                !loss.is_finished(),
                "the loss transition waits for the admitted durable mutation"
            );
            waiter.abort();
            let _ = waiter.await;
            tokio::time::timeout(Duration::from_secs(3), loss)
                .await
                .expect("detached settlement releases serving fence")
                .expect("loss task");
            assert!(fixture
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("settled ledger")
                .is_none());
            let current = fixture
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("pointer")
                .expect("current stream");
            assert_eq!(
                current.incarnation_id,
                if committed {
                    staged.staged_incarnation_id
                } else {
                    route.incarnation_id
                }
            );
        }
    }

    /// The staged row has to be **usable**, not merely present. Every reader of
    /// a route's `response_json` parses it as a `StartResponse`, and
    /// `control_start_response` filters on the bootstrap being present — so a
    /// row without one answers 404 `session_gone` on the successor's first
    /// exchange the moment §3.5 commits it. A first version of this change
    /// wrote three loose fields and no test noticed, because the staged-row
    /// read carries no recipe or response at all. This commits the successor
    /// and reads what it became.
    #[tokio::test]
    async fn a_staged_successor_commits_into_a_usable_route() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
        fixture
            .state
            .transcode
            .vod_for_test()
            .install_http_test_session(&session_id, staged_source_file(), dir.path())
            .await;
        // The predecessor was itself opened as a stall reopen. `candidate` is
        // its request with only the selection overwritten, so without an
        // explicit clear the successor would stage claiming to be a reopen of
        // a session that is still playing.
        let candidate = crate::transcode::SessionRequest {
            previous_session_id: Some(uuid::Uuid::new_v4().to_string()),
            reopen_reason: Some(crate::transcode::ReopenReason::Stall),
            ..staged_candidate_request()
        };
        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &candidate,
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
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (status, prepare_body) = control_body(
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
        assert_eq!(prepare_body["action"]["type"], "prepare");
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            // Echo the offer: a commit naming a different origin is refused.
            committed_media_origin_ms: prepare_body["action"]["media_origin_ms"].as_i64(),
            first_frame_unix_ms: Some(unix_ms()),
        });
        let committed_request = request.clone();
        let (status, committed_body) = control_body(
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
        assert_eq!(committed_body["action"]["type"], "none");
        assert_eq!(
            committed_body["delivery"]["presentation"], "vod",
            "the commit acknowledgement must propagate through the public VOD engine"
        );
        let (replay_status, replay_body) = control_body(
            control_inner(
                fixture.state.clone(),
                session_id.clone(),
                Bytes::from(
                    serde_json::to_vec(&committed_request).expect("serialize committed replay"),
                ),
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(replay_status, StatusCode::OK);
        assert_eq!(
            replay_body, committed_body,
            "the durable receipt must replay the exact response after the predecessor retires"
        );

        let committed = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("committed route")
            .expect("the successor is now current");
        assert_eq!(committed.incarnation_id, staged.staged_incarnation_id);
        let draining_predecessor = fixture
            .state
            .store
            .media_session_route(&session_id)
            .await
            .expect("predecessor route after acknowledgement")
            .expect("the predecessor remains readable");
        assert_eq!(
            draining_predecessor.state, "active",
            "pointer advancement no longer retires the predecessor: it starts a drain, \
             and the predecessor keeps serving until its owner's tick passes the deadline"
        );
        assert!(
            fixture
                .state
                .store
                .staged_media_session_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("ledger read after acknowledgement")
                .is_none(),
            "commit removes the preparation ledger row"
        );

        let response = serde_json::from_str::<StartResponse>(&committed.response_json)
            .expect("the staged response must parse as the type every reader uses");
        assert!(
            response.control.is_some(),
            "without a bootstrap the successor's first control exchange is a 404"
        );
        assert_eq!(response.session_id, committed.session_id);

        let recipe = serde_json::from_str::<RemoteStartRequest>(&committed.recipe_json)
            .expect("the staged recipe must parse");
        let file = staged_source_file();
        assert_eq!(
            (recipe.source_size, recipe.source_mtime),
            (file.size, file.mtime),
            "`0/0` is the sentinel that refuses this successor a takeover forever"
        );
        assert_eq!(
            recipe.request.request_id.as_deref(),
            Some(recipe.incarnation_id.as_str()),
            "the envelope validator requires exactly this"
        );
        assert!(
            recipe.request.previous_session_id.is_none() && recipe.request.reopen_reason.is_none(),
            "a successor is its own generation, not a reopen of the one it replaces"
        );
    }

    #[tokio::test]
    async fn a_timed_out_settlement_finishes_detached_and_replays_durably() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
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
            &staged_candidate_request(),
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
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (status, prepare_body) = control_body(
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
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Committed,
            buffered_through_ms: None,
            // Echo the offer: a commit naming a different origin is refused.
            committed_media_origin_ms: prepare_body["action"]["media_origin_ms"].as_i64(),
            first_frame_unix_ms: Some(unix_ms()),
        });
        delay_next_preparation_settlement(&route.incarnation_id, Duration::from_millis(700));
        let request_body =
            Bytes::from(serde_json::to_vec(&request).expect("serialize timed-out acknowledgement"));
        let (failed_status, _) = control_body(
            control_local_inner(
                &fixture.state,
                &route,
                request.clone(),
                unix_ms().saturating_add(500),
            )
            .await,
        )
        .await;
        assert_eq!(failed_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("pointer after failed settlement")
                .expect("the predecessor stays current")
                .incarnation_id,
            route.incarnation_id
        );

        tokio::time::sleep(Duration::from_millis(400)).await;
        let committed_before_retry = fixture
            .state
            .store
            .media_session_route_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("pointer after detached settlement")
            .expect("the detached settlement committed the successor");
        assert_eq!(
            committed_before_retry.incarnation_id, staged.staged_incarnation_id,
            "the timed-out request's detached task, not the retry, must advance the pointer"
        );
        let draining_predecessor = fixture
            .state
            .store
            .media_session_route(&session_id)
            .await
            .expect("predecessor after detached settlement")
            .expect("the predecessor remains readable");
        assert_eq!(
            draining_predecessor.state, "active",
            "the detached settlement starts the predecessor's drain rather than retiring it"
        );
        let retained_receipt = fixture
            .state
            .store
            .media_session_terminal_ack(&session_id, unix_ms())
            .await
            .expect("receipt after detached settlement")
            .expect("commit and its exact response are retained atomically");
        let retained_envelope: serde_json::Value =
            serde_json::from_str(&retained_receipt.response_json).expect("retained response JSON");
        let retained_body = retained_envelope["response"].clone();

        let (retry_status, retry_body) = control_body(
            control_inner(
                fixture.state.clone(),
                session_id,
                request_body,
                unix_ms().saturating_add(4_000),
            )
            .await,
        )
        .await;
        assert_eq!(retry_status, StatusCode::OK);
        assert_eq!(
            retry_body, retained_body,
            "the retry must replay the response retained before it arrived"
        );
    }

    #[tokio::test]
    async fn a_failed_preparation_acknowledgement_aborts_and_frees_the_slot() {
        failed_preparation_acknowledgement_frees_slot(false).await;
    }

    #[tokio::test]
    async fn a_preparation_acknowledgement_settles_the_slot_after_durable_cleanup() {
        failed_preparation_acknowledgement_frees_slot(true).await;
    }

    async fn failed_preparation_acknowledgement_frees_slot(cleaned_up: bool) {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, session_id, route) = staging_fixture(dir.path()).await;
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
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let first_staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged")
            .staged_incarnation_id;

        let mut request = control_request(route.incarnation_id.clone());
        request.supported_actions = Some(vec![
            crate::playback_control::PREPARE_REPLACEMENT_ACTION.to_owned()
        ]);
        let (status, prepare_body) = control_body(
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
        let action_id = prepare_body["action"]["action_id"]
            .as_str()
            .expect("Prepare action id")
            .to_owned();

        if cleaned_up {
            // Model a deadline abort whose Store attempt failed, followed by
            // independent durable cleanup. No executor has settled the actor.
            let gate = fixture
                .state
                .transcode
                .session_preparation_gate(&session_id)
                .await
                .expect("preparation gate");
            assert!(
                gate.begin_abort_preparation_for_owner(&first_staged, route.owner_epoch)
                    .await
            );
            assert!(fixture
                .state
                .store
                .abort_media_session_preparation(
                    route.user_id,
                    &route.playback_id,
                    &plurx_core::domain::MediaSessionPreparationAbortRequest {
                        staged_incarnation_id: first_staged.clone(),
                        expected_predecessor_owner_node_id: route.owner_node_id.clone(),
                        expected_predecessor_owner_epoch: route.owner_epoch,
                        now_ms: unix_ms(),
                    },
                )
                .await
                .expect("independent cleanup")
                .is_some());
        }

        tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        request.sequence = 2;
        request.capabilities = None;
        request.acknowledgement = Some(crate::playback_control::ActionAcknowledgement {
            action_id,
            state: crate::playback_control::AcknowledgementState::Failed,
            buffered_through_ms: None,
            committed_media_origin_ms: None,
            first_frame_unix_ms: None,
        });
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
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["action"]["type"], "none");
        assert_eq!(
            body["delivery"]["presentation"], "vod",
            "the abort acknowledgement must propagate through the public VOD engine"
        );
        assert_eq!(
            fixture
                .state
                .store
                .media_session_route_for_playback(route.user_id, &route.playback_id)
                .await
                .expect("current route")
                .expect("the predecessor stays current")
                .incarnation_id,
            route.incarnation_id
        );
        assert!(fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger after abort")
            .is_none());

        stage_prepared_successor(
            &fixture.state,
            &session_id,
            &route,
            &staged_predecessor_recipe(&route),
            &staged_candidate_request(),
            Some(&staged_source_file()),
            AcceptedAsk {
                film_time_ms: STAGED_ACCEPTED_FILM_TIME_MS,
                desired_digest: None,
            },
        )
        .await;
        let replacement = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("replacement ledger read")
            .expect("the settled slot accepts another preparation");
        assert_ne!(replacement.staged_incarnation_id, first_staged);
    }

    /// The one arrangement that tells the three candidate resume points apart.
    ///
    /// A session whose film begins at 00:01:30, a viewer 30 seconds into it,
    /// and a client that has fetched 150 seconds of it:
    ///
    /// | quantity | route-relative | absolute film time |
    /// |---|---|---|
    /// | session origin | — | 90 000 |
    /// | accepted playhead | 30 000 | **120 000** |
    /// | fetch frontier | 150 000 | 240 000 |
    ///
    /// Each wrong answer lands on its own number, so no assertion here can
    /// pass for the wrong reason: the fetch frontier gives 240 000, adding the
    /// origin back to an already-absolute playhead gives 210 000, and the
    /// predecessor's untouched `start_seconds` gives 0.
    const RESUME_ORIGIN_MS: i64 = 90_000;
    const RESUME_FETCHED_THROUGH_MS: i64 = 150_000;
    const RESUME_ACCEPTED_PLAYHEAD_MS: i64 = 120_000;
    const RESUME_FETCH_FRONTIER_MS: i64 = RESUME_ORIGIN_MS + RESUME_FETCHED_THROUGH_MS;
    const RESUME_DOUBLE_COUNTED_ORIGIN_MS: i64 = RESUME_ORIGIN_MS + RESUME_ACCEPTED_PLAYHEAD_MS;

    /// Every place a staged successor's timeline is written: the VOD route's
    /// zero `media_origin_ms`, the recipe's absolute `start_seconds`, and the
    /// bootstrap response's start and zero origin.
    ///
    /// VOD media time already starts at film zero, so the resume point belongs
    /// only in the request/bootstrap start. A nonzero origin would be added to
    /// an already-absolute fetched frontier after owner loss. Staging is what
    /// establishes these values; commit needs a separately accepted client
    /// acknowledgement these tests deliberately do not supply.
    async fn staged_resume_ms(
        fixture: &HlsDeliveryFixture,
        route: &MediaSessionRoute,
    ) -> (i64, f64, f64, i64) {
        let staged = fixture
            .state
            .store
            .staged_media_session_for_playback(route.user_id, &route.playback_id)
            .await
            .expect("ledger read")
            .expect("a successor is staged");
        let successor = fixture
            .state
            .store
            .media_session_route_by_incarnation(&staged.staged_incarnation_id)
            .await
            .expect("staged route")
            .expect("successor");
        let recipe =
            serde_json::from_str::<RemoteStartRequest>(&successor.recipe_json).expect("recipe");
        let bootstrap = serde_json::from_str::<StartResponse>(&successor.response_json)
            .expect("the staged response parses as a StartResponse");
        (
            successor.media_origin_ms,
            recipe.request.start_seconds,
            bootstrap.start_seconds,
            bootstrap.media_origin_ms.unwrap_or_default(),
        )
    }
