// Actual committed-member relay: the ingress has a different subtitle cache
// and no copy of the owner's session. No peer body or membership is mocked.
mod cached_owner_delivery_tests {
    use super::*;
    use futures_util::FutureExt;
    use plurx_core::cluster::migration::select_daemon_store_observing;
    use plurx_core::config::Config;
    fn config(root: &std::path::Path) -> Config {
        let held: Vec<_> = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned port"))
            .collect();
        let addresses: Vec<_> = held
            .iter()
            .map(|socket| socket.local_addr().expect("owned fixture invariant"))
            .collect();
        let mut config = Config::default();
        config.storage.data_dir = root.into();
        config.server.bind = addresses[0];
        config.cluster.raft_bind = addresses[1];
        config.cluster.api_bind = addresses[2];
        config.cluster.advertise_host = "localhost".into();
        config.cluster.join_url = format!("http://{}", addresses[0]);
        config.cluster.artwork_url = config.cluster.join_url.clone();
        config
    }

    #[test]
    fn cached_caption_revision_reaches_real_remote_owner_without_ingress_cache() {
        let worker = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("cached caption fixture")
                    .block_on(Box::pin(remote_owner_fixture()));
            })
            .expect("cached caption fixture");
        if let Err(panic) = worker.join() {
            std::panic::resume_unwind(panic);
        }
    }
    async fn remote_owner_fixture() {
        let source_root = crate::test_tempdir().expect("owned leader root");
        let source_config = config(source_root.path());
        drop(
            plurx_core::store::SqliteStore::open(&source_root.path().join("plurx.db"))
                .expect("schema"),
        );
        let startup = Arc::new(crate::StartupObservationHttp::new(
            source_config.server.bind,
        ));
        let selected = select_daemon_store_observing(&source_config, Some(startup.as_ref()))
            .await
            .expect("actual singleton activation within production phase");
        let leader = selected.membership_manager();
        let session = uuid::Uuid::new_v4().to_string();
        let owner_root = crate::test_tempdir().expect("cached caption fixture");
        let mut owner =
            HlsDeliveryFixture::publish_without_process(owner_root.path(), &session).await;
        add_http_text_subtitle(&mut owner, &session).await;
        owner.mark_started().await;
        owner.state.node_id = selected.identity.node_id.clone();
        owner.state.membership = leader.clone();
        owner.state.media_sessions = crate::media_sessions::MediaSessionCoordinator::new(
            leader.clone(),
            Arc::clone(&owner.store),
        );
        // Admit the ingress through the same learner protocol, clock proof,
        // activation and normal membership initialization used by the daemon.
        // Keep the source clock probe alive while the membership grows.
        leader
            .activate_learner_protocol()
            .await
            .expect("activate actual learner admission");
        {
            let mut routes = startup.router.write().expect("cached caption fixture");
            *routes = routes.clone().merge(
                axum::Router::new()
                    .route(
                        "/api/v1/cluster/learner/join/redeem",
                        axum::routing::post(super::super::super::cluster::redeem_learner_join),
                    )
                    .route(
                        "/api/v1/cluster/learner/join/finalize",
                        axum::routing::post(super::super::super::cluster::finalize_learner_join),
                    )
                    .with_state(owner.state.clone()),
            );
        }
        let token = leader
            .issue_learner_token(Duration::from_secs(120))
            .await
            .expect("real learner token");
        let learner_root = crate::test_tempdir().expect("owned learner root");
        let token_path = learner_root.path().join("join.token");
        tokio::fs::write(&token_path, format!("{}\n", token.token))
            .await
            .expect("owned learner token file");
        let mut learner_config = config(learner_root.path());
        learner_config.cluster.join_token_file = token_path.clone();
        let learner_startup = Arc::new(crate::StartupObservationHttp::new(
            learner_config.server.bind,
        ));
        let learner_selected = tokio::time::timeout(
            Duration::from_secs(45),
            Box::pin(select_daemon_store_observing(
                &learner_config,
                Some(learner_startup.as_ref()),
            )),
        )
        .await
        .expect("unchanged 45-second learner budget")
        .expect("actual finalized learner activation and clock admission");
        assert!(!token_path.exists(), "a finalized join consumes its token");
        assert_eq!(learner_selected.identity.raft_id, token.raft_id);
        let learner_id = learner_selected.identity.node_id.clone();
        let learner = learner_selected.membership_manager();
        let applied = learner_selected
            .local_client()
            .expect("actual learner client")
            .local_db_raft_metrics()
            .expect("real applied watch")
            .membership_snapshot();
        assert!(applied.committed && applied.members.contains(&token.raft_id));
        assert!(!applied.voters.contains(&token.raft_id));
        assert!(!learner.may_run_cluster_jobs().await);

        let user = owner
            .store
            .create_user("caption-owner", "hash", false)
            .await
            .expect("cached caption fixture");
        let now = unix_ms();
        let activation = MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: session.clone(),
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: user.id,
            },
            playback_id: "caption-owner".into(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: owner.state.node_id.clone(),
            lease_expires_at_ms: now + 60_000,
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
        };
        owner
            .store
            .activate_media_session(&activation)
            .await
            .expect("cached caption fixture")
            .expect("cached caption fixture");
        owner
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now,
            )
            .await
            .expect("cached caption fixture")
            .expect("cached caption fixture");
        let ingress_root = crate::test_tempdir().expect("cached caption fixture");
        let ingress_fixture = HlsDeliveryFixture::publish_without_process(
            ingress_root.path(),
            "unrelated-ingress-session",
        )
        .await;
        let mut ingress = ingress_fixture.state.clone();
        ingress.node_id = learner_id;
        ingress.membership = learner;
        ingress.store = Arc::clone(&owner.store);
        ingress.media_sessions = crate::media_sessions::MediaSessionCoordinator::new(
            ingress.membership.clone(),
            Arc::clone(&owner.store),
        );
        let file = owner
            .store
            .get_file(owner.file_id())
            .await
            .expect("cached caption fixture")
            .expect("cached caption fixture");
        tokio::fs::remove_file(crate::subtitles::vtt_path(&owner.state.subs_dir, &file, 0))
            .await
            .expect("cached caption fixture");
        let path = crate::subtitles::vtt_window_path(&owner.state.subs_dir, &file, 0, 0, 200);
        let revision = path
            .file_name()
            .expect("cached caption fixture")
            .to_str()
            .expect("cached caption fixture")
            .to_owned();
        let bytes = b"WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nRemote owner window\n";
        tokio::fs::write(&path, bytes)
            .await
            .expect("cached caption fixture");
        assert!(
            crate::subtitles::read_cached_revision(&ingress.subs_dir, &file, 0, &revision)
                .await
                .expect("cached caption fixture")
                .is_none()
        );
        // Retain the actual signed clock route and its original observer when
        // adding the relay to the startup-owned socket.
        {
            let mut routes = startup.router.write().expect("cached caption fixture");
            *routes = routes.clone().merge(
                axum::Router::new()
                    .route(
                        crate::media_sessions::RELAY_PATH,
                        axum::routing::post(super::super::super::internal_media_sessions::relay),
                    )
                    .with_state(owner.state.clone()),
            );
        }
        // Production starts this sampler before heartbeat publication. Without
        // it, an unsampled node truthfully stays out of the media directory.
        let sampler_stop = tokio_util::sync::CancellationToken::new();
        let sampler = tokio::spawn(
            selected
                .replication_monitor()
                .passive_metrics_loop(sampler_stop.clone().cancelled_owned()),
        );
        let checks = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    leader
                        .heartbeat()
                        .await
                        .expect("actual owner readiness heartbeat");
                    ingress
                        .membership
                        .heartbeat()
                        .await
                        .expect("actual finalized ingress heartbeat");
                    let peers = ingress
                        .membership
                        .media_peers()
                        .await
                        .expect("actual committed media directory");
                    if let Some(peer) = peers
                        .iter()
                        .find(|peer| peer.node_id == owner.state.node_id && peer.reachable)
                    {
                        assert_eq!(
                            peer.http_base.as_deref(),
                            Some(source_config.cluster.join_url.as_str()),
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            })
            .await
            .expect("actual sampled owner becomes eligible before relay");
            let clock = super::super::super::peer_transport::PeerTransport::new(
                ingress.membership.clone(),
            )
            .clock_request(&owner.state.node_id, &source_config.cluster.join_url)
            .await
            .expect("signed clock service survives relay installation");
            assert_eq!(clock.status, reqwest::StatusCode::OK);
            assert!(clock.clock_timing.is_some(), "actual four-stamp clock timing");
            let timing: super::super::super::internal_clock::ClockResponse =
                serde_json::from_slice(&clock.body).expect("actual signed clock response");
            assert_eq!(timing.node_id, owner.state.node_id);
            assert!(timing.sent_unix_ms >= timing.received_unix_ms);
            let response = subtitle_vtt(
                State(ingress.clone()),
                AxPath((session.clone(), 0, format!("cached-{revision}"))),
                HeaderMap::new(),
            )
            .await
            .expect("cached caption fixture");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["x-plurx-subtitle-complete"], "false");
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .expect("cached caption fixture")
                    .as_ref(),
                bytes
            );
            assert!(
                crate::subtitles::read_cached_revision(&ingress.subs_dir, &file, 0, &revision)
                    .await
                    .expect("cached caption fixture")
                    .is_none(),
                "delivery did not manufacture an ingress extraction"
            );
            let stale = subtitle_vtt(
                State(ingress.clone()),
                AxPath((session.clone(), 0, "cached-f0-s0-1-1-w0-200.vtt".into())),
                HeaderMap::new(),
            )
            .await
            .expect("cached caption fixture");
            assert_eq!(stale.status(), StatusCode::NO_CONTENT);
            let wrong_track = cached_subtitle_revision_local_before(
                &owner.state,
                &session,
                1,
                &revision,
                response_publication_deadline(),
            )
            .await
            .expect_err("stale caption identity refused");
            assert_eq!(wrong_track.into_response().status(), StatusCode::NOT_FOUND);
            let other_session = cached_subtitle_revision_local_before(
                &owner.state,
                &uuid::Uuid::new_v4().to_string(),
                0,
                &revision,
                response_publication_deadline(),
            )
            .await
            .expect_err("stale caption identity refused");
            assert_eq!(
                other_session.into_response().status(),
                StatusCode::NOT_FOUND
            );
        })
        .catch_unwind()
        .await;
        sampler_stop.cancel();
        sampler.await.expect("owned passive sampler drained");
        startup.stop_and_drain().await;
        learner_startup.stop_and_drain().await;
        learner_selected
            .shutdown()
            .await
            .expect("actual learner listeners and writers drained");
        selected.shutdown().await.expect("cached caption fixture");
        if let Err(panic) = checks {
            std::panic::resume_unwind(panic);
        }
    }
}
