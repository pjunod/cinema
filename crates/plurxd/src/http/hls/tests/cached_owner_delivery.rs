// Actual committed-member relay: the ingress has a different subtitle cache
// and no copy of the owner's session. No peer body or membership is mocked.
mod cached_owner_delivery_tests {
    use super::*;
    use futures_util::FutureExt;
    use plurx_core::cluster::membership::{
        ActivitySigningKey, ClusterPeer, ClusterRole, JoinSecrets, MembershipManager,
        RedeemJoinRequest, StartupMembershipAdmission, decode_join_token, join_token_digest,
    };
    use plurx_core::cluster::migration::select_daemon_store_observing;
    use plurx_core::config::Config;
    use std::borrow::Cow;
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
        startup.stop_probe().await;
        let leader = selected.membership_manager();
        let token = leader
            .issue_token(Duration::from_secs(120))
            .await
            .expect("real token");
        let payload = decode_join_token(&token.token).expect("owned token");
        let learner_root = crate::test_tempdir().expect("owned learner root");
        let learner_config = config(learner_root.path());
        let learner_id = uuid::Uuid::new_v4().to_string();
        let (protocol_min, protocol_max) = payload.declared_protocol_range();
        leader
            .redeem(&RedeemJoinRequest {
                token_digest: join_token_digest(&token.token),
                raft_id: token.raft_id,
                node_id: learner_id.clone(),
                hostname: "causal-clock-control".into(),
                raft_address: learner_config.cluster.raft_bind.to_string(),
                api_address: learner_config.cluster.api_bind.to_string(),
                http_base: learner_config.cluster.artwork_url.clone(),
                schema_version: payload.schema_version(),
                protocol_version: protocol_min,
                protocol_min,
                protocol_max,
                live_tv_v1: true,
                sharing: Default::default(),
            })
            .await
            .expect("real authorized staged identity");
        let local = ClusterPeer {
            raft_id: token.raft_id,
            raft_address: learner_config.cluster.raft_bind.to_string(),
            api_address: learner_config.cluster.api_bind.to_string(),
        };
        let mut nodes: Vec<_> = payload
            .bootstrap()
            .iter()
            .map(hiqlite::Node::from)
            .collect();
        nodes.push(hiqlite::Node::from(&local));
        let learner_client = tokio::time::timeout(
            Duration::from_secs(45),
            Box::pin(hiqlite::start_node_for_clock_observation(
                hiqlite::NodeConfig {
                    node_id: token.raft_id,
                    nodes,
                    listen_addr_api: Cow::Borrowed("127.0.0.1"),
                    listen_addr_raft: Cow::Borrowed("127.0.0.1"),
                    data_dir: Cow::Owned(learner_root.path().to_string_lossy().into_owned()),
                    filename_db: Cow::Borrowed("clock-causal.db"),
                    secret_raft: payload.secrets().raft.clone(),
                    secret_api: payload.secrets().api.clone(),
                    tls_raft: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                    tls_api: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                    learner_only: true,
                    health_check_delay_secs: 0,
                    ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
                },
                Arc::new(StartupMembershipAdmission::default()),
            )),
        )
        .await
        .expect("unchanged 45-second learner budget")
        .expect("actual committed learner");
        let mut identity = selected.identity.clone();
        identity.node_id.clone_from(&learner_id);
        identity.raft_id = token.raft_id;
        let learner = MembershipManager::clock_observation(
            learner_client.clone(),
            selected.replication_monitor(),
            Arc::clone(&selected.store),
            identity,
            local,
            source_config.cluster.join_url.clone(),
            learner_config.cluster.artwork_url.clone(),
            JoinSecrets {
                raft: payload.secrets().raft.clone(),
                api: payload.secrets().api.clone(),
                credential_key: payload.secrets().credential_key.clone(),
            },
            ActivitySigningKey::from_seed_hex(&"13".repeat(32)).expect("test-owned signing key"),
            payload.activation_marker().clone(),
            ClusterRole::Voter,
            learner_root.path().into(),
        )
        .await
        .expect("actual pending observation identity");
        let applied = learner_client
            .local_db_raft_metrics()
            .expect("real applied watch")
            .membership_snapshot();
        assert!(applied.committed && applied.members.contains(&token.raft_id));
        assert!(!applied.voters.contains(&token.raft_id));

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
            publication_ready_at_ms: 0,
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
        let stop = tokio_util::sync::CancellationToken::new();
        let server = {
            let stop = stop.clone();
            let app = axum::Router::new()
                .route(
                    crate::media_sessions::RELAY_PATH,
                    axum::routing::post(super::super::super::internal_media_sessions::relay),
                )
                .with_state(owner.state.clone());
            let listener = tokio::net::TcpListener::bind(source_config.server.bind)
                .await
                .expect("cached caption fixture");
            tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(stop.cancelled_owned())
                    .await
                    .expect("cached caption fixture");
            })
        };
        leader
            .heartbeat()
            .await
            .expect("actual owner readiness heartbeat");
        let checks = std::panic::AssertUnwindSafe(async {
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
        stop.cancel();
        server.await.expect("cached caption fixture");
        let drain = tokio::spawn(async move { learner_client.shutdown().await }).await;
        if let Ok(result) = drain {
            result.expect("cached caption fixture");
        }
        selected.shutdown().await.expect("cached caption fixture");
        if let Err(panic) = checks {
            std::panic::resume_unwind(panic);
        }
    }
}
