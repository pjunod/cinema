    // Playback-start E1c review (S1, S2, S4): the route view through the
    // HTTP paths, over the replica double of `media_sessions::route_view_tests`.

    /// A fixture whose coordinator reads routes through a replica double
    /// holding nothing, so every view answer is `Absent` unless applied.
    async fn e1c_fixture(
        dir: &std::path::Path,
    ) -> (
        HlsDeliveryFixture,
        Arc<crate::media_sessions::route_view_tests::ReplicaRoutes>,
    ) {
        let mut fixture =
            HlsDeliveryFixture::publish_without_process(dir, &uuid::Uuid::new_v4().to_string())
                .await;
        let replica = crate::media_sessions::route_view_tests::ReplicaRoutes::new(true);
        let applied: Arc<dyn crate::media_sessions::RouteView> = replica.clone();
        fixture.state.media_sessions =
            crate::media_sessions::MediaSessionCoordinator::with_applied_view(
                fixture.state.membership.clone(),
                Arc::clone(&fixture.store),
                applied,
            );
        (fixture, replica)
    }

    async fn e1c_route(
        fixture: &HlsDeliveryFixture,
        session_id: &str,
        owner: &str,
    ) -> MediaSessionRoute {
        let user = fixture
            .store
            .create_user(&format!("e1c-{session_id}"), "hash", false)
            .await
            .expect("route user");
        let now_ms = unix_ms();
        activate_ready(
            &fixture.store,
            MediaSessionActivation {
                recovery_epoch: String::new(),
                expected_desired_revision: None,
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.to_owned(),
                principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                    user_id: user.id,
                },
                playback_id: format!("e1c-{session_id}"),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: owner.to_owned(),
                lease_expires_at_ms: now_ms.saturating_add(60_000),
                recipe_json: "{}".to_owned(),
                response_json: "{}".to_owned(),
                publication_ready_at_ms: 0,
                media_origin_ms: 0,
                now_ms,
            },
        )
        .await
    }

    /// Review S4: the first status resolution is view first, and a verdict
    /// that is not active is confirmed by a quorum read before it answers.
    /// Here the replica has not applied the session at all (`Absent`), while
    /// the cluster has already ended it: the request answers 410, not 404,
    /// and the confirmed verdict replaces the cached view answer.
    #[tokio::test]
    async fn e1c_status_confirms_a_non_active_view_verdict_by_quorum() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, replica) = e1c_fixture(dir.path()).await;
        let session_id = uuid::Uuid::new_v4().to_string();
        e1c_route(&fixture, &session_id, &fixture.state.node_id.clone()).await;
        fixture
            .store
            .end_media_session(&session_id, "deleted", unix_ms())
            .await
            .expect("end")
            .expect("ended row");
        let result = status_local_before_with_relay(
            &fixture.state,
            &session_id,
            Instant::now() + Duration::from_secs(1),
            false,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(ApiError::Typed {
                    status: StatusCode::GONE,
                    code: "media_session_ended",
                    ..
                })
            ),
            "the view's absence was confirmed as the durable end"
        );
        assert_eq!(
            replica.view_reads.load(std::sync::atomic::Ordering::Acquire),
            1,
            "status asked the view first"
        );
        assert!(
            fixture
                .state
                .media_sessions
                .cached_route_is_a_fact(&session_id)
                .await,
            "the confirmed verdict replaced the view answer"
        );
    }

    /// Review S1: `Absent` from the view is confirmed by quorum before media
    /// routing answers it locally. The replica has not applied a session a
    /// peer activated a moment ago; the request is relayed to that owner
    /// (unreachable here, so 503 from the relay), never answered from the
    /// local path.
    #[tokio::test]
    async fn e1c_relay_confirms_a_view_absence_before_answering_locally() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, replica) = e1c_fixture(dir.path()).await;
        let session_id = uuid::Uuid::new_v4().to_string();
        e1c_route(&fixture, &session_id, "e1c-peer-owner").await;
        let result = relay_if_remote(
            &fixture.state,
            &session_id,
            RelayResource::Status,
            RelayHeaders::default(),
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        assert!(
            matches!(result, Err(ApiError::ServiceUnavailable(_))),
            "the session a peer owns is relayed, not answered locally"
        );
        assert_eq!(
            replica.view_reads.load(std::sync::atomic::Ordering::Acquire),
            1
        );
        assert!(
            fixture
                .state
                .media_sessions
                .cached_route_is_a_fact(&session_id)
                .await
        );

        // An absence the cluster confirms is answered by the local path, once
        // per lease: the confirmed fact is kept and not confirmed again.
        let absent = uuid::Uuid::new_v4().to_string();
        for _ in 0..3 {
            assert!(matches!(
                relay_if_remote(
                    &fixture.state,
                    &absent,
                    RelayResource::Status,
                    RelayHeaders::default(),
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
                Ok(None)
            ));
        }
        assert_eq!(
            replica.view_reads.load(std::sync::atomic::Ordering::Acquire),
            2,
            "one view read for the absent id, then its confirmed fact"
        );
    }

    /// Review S2: the candidate-recovery receipt fence reads the Store, never
    /// the view, with a disagreeing view answer cached for the same session.
    #[tokio::test]
    async fn e1c_the_receipt_fence_never_reads_the_view() {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, replica) = e1c_fixture(dir.path()).await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let durable = e1c_route(&fixture, &session_id, &fixture.state.node_id.clone()).await;
        let mut stale = durable;
        stale.owner_node_id = "node-stale".to_owned();
        replica.apply(stale);
        let _ = relay_if_remote(
            &fixture.state,
            &session_id,
            RelayResource::Status,
            RelayHeaders::default(),
            Instant::now() + Duration::from_secs(1),
        )
        .await;
        let calls = replica.calls.load(std::sync::atomic::Ordering::Acquire);
        let sample = super::candidate_recovery::ClientRecoverySample {
            cause: plurx_core::store::CandidateRecoveryCause::Decode,
            event_id: uuid::Uuid::new_v4().to_string(),
            candidate_id: plurx_core::playback::candidate::CandidateId([0; 16]),
            recipe_digest: [0; 32],
            age_ms: 0,
            decoder_failed: true,
            rendered_elapsed_ms: 0,
            position_progress_ms: 0,
            dropped_frames: 0,
            runway_ms: 0,
        };
        let network = crate::telemetry::NetworkIdentity {
            client_class: "web".to_owned(),
            network_fingerprint: "e1c".to_owned(),
            credential_generation: None,
            user_id: Some(i64::MAX),
        };
        assert!(super::candidate_recovery::accept_sample(
            &fixture.state,
            &network,
            Some(&session_id),
            &sample,
        )
        .await
        .is_none());
        assert_eq!(
            replica.calls.load(std::sync::atomic::Ordering::Acquire),
            calls,
            "the receipt fence asked the applied view"
        );
    }

    /// A release may arrive either before the route read starts or while its
    /// answer is outstanding. Both fences must remain fail closed and report
    /// which side of the authoritative read refused this exact exchange.
    async fn control_release_failure_diagnostics(release_before_read: bool) {
        let dir = crate::test_tempdir().expect("state dir");
        let (fixture, _) = e1c_fixture(dir.path()).await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let durable = e1c_route(&fixture, &session_id, &fixture.state.node_id).await;
        let buffer = Arc::new(crate::logbuf::LogBuffer::new(16));
        let _capture = crate::logbuf::capture_for_test(&buffer);
        let gate = (!release_before_read)
            .then(|| fixture.state.media_sessions.hold_route_store_for_test());
        if release_before_read {
            assert!(matches!(
                fixture.state.media_sessions.begin_release_reconciliation(&session_id).await,
                ReleaseAdmission::Won(_)
            ));
        }
        let request = control_request(durable.incarnation_id.clone());
        let body = Bytes::from(serde_json::to_vec(&request).expect("control JSON"));
        let route = super::fence::hls_session_route("POST", "/api/v1/hls/{session}/control")
            .expect("control route");
        let path = format!("/api/v1/hls/{session_id}/control");
        let exchange = super::fence::observe_hls_session_response(
            &fixture.state.serving,
            route,
            &path,
            control_inner(fixture.state.clone(), session_id.clone(), body, unix_ms() + 4_000),
        );
        let release = async {
            if let Some((entered, gate)) = gate {
                entered.notified().await;
                assert!(matches!(
                    fixture.state.media_sessions.begin_release_reconciliation(&session_id).await,
                    ReleaseAdmission::Won(_)
                ));
                gate.close();
            }
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(exchange, release)
        }).await.expect("deterministic route/release interleave");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.extensions().get::<crate::http::error::ApiErrorCode>()
            .expect("typed control error").0, "control_unavailable");
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await.expect("body");
        assert_eq!(bytes.as_ref(), br#"{"code":"control_unavailable","message":"the durable media route is temporarily unavailable","retry_after_ms":500}"#);
        assert_eq!(fixture.store.media_session_route(&session_id).await
            .expect("durable route").expect("active row").state, "active",
            "diagnostics never bypass the release fence or mutate the row");
        let lines = buffer.tail("trace", 16);
        let failures = lines.iter().filter(|line| line.message.contains("playback response failed"))
            .collect::<Vec<_>>();
        assert_eq!(failures.len(), 1, "one existing diagnostic event: {lines:?}");
        let message = &failures[0].message;
        let expected_stage = if release_before_read {
            "control_route_failure=\"release_pending_before_read\""
        } else {
            "control_route_failure=\"release_pending_after_read\""
        };
        for field in [
            "status=503", "code=\"control_unavailable\"", expected_stage,
            "control_error_file=\"http/hls/control.rs\"", "control_error_line=",
            &format!("session=\"{}\"", crate::transcode::session_log_id(&session_id)),
        ] {
            assert!(message.contains(field), "{field} missing from {message}");
        }
        assert!(!message.contains(&session_id), "never log the bearer capability");
        assert!(!message.contains("control_store_error_class="),
            "a release fence is not a backend error: {message}");
    }

    #[tokio::test]
    async fn control_release_before_lookup_logs_typed_failure_without_serving_active_route() {
        control_release_failure_diagnostics(true).await;
    }

    #[tokio::test]
    async fn control_release_during_lookup_logs_typed_failure_without_serving_stale_answer() {
        control_release_failure_diagnostics(false).await;
    }
