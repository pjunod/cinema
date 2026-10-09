

    #[tokio::test]
    async fn a05_real_vod_staged_observation_is_not_commit_authority_and_cannot_follow_replacement() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = local_serve(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;

        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(crate::playback_control::ClientPlatform::Web);
        snapshot.request_fingerprint = Some("origin".into());
        let desired = snapshot.selection.desired().digest();
        let client = uuid::Uuid::new_v4().to_string();

        let accepted = serve.control(crate::playback_control::LocalControlRequest {
            session_id: &session_id, generation: &generation, owner_node_id: "node-a", owner_epoch: 1,
            client_instance_id: &client, sequence: 1, snapshot,
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        }).await.expect("VOD worker").expect("accepted");
        assert_eq!(accepted.disposition, crate::playback_control::ControlDisposition::Accepted);

        let gate = serve.preparation_gate(&session_id).await.expect("real VOD gate");
        let identity = crate::playback_control::AcceptedControlIdentity {
            generation: generation.clone(), owner_epoch: 1, client_instance_id: client.clone(),
            sequence: 1, fingerprint: "origin".into(), desired_digest: desired.clone(),
        };
        let proof = gate.accepted_observation(identity).await.expect("real accepted actor origin");
        let staged = uuid::Uuid::new_v4().to_string();
        let deadline = crate::media_sessions::unix_ms() + 60_000;
        assert!(gate.staged_observation_is_current(proof.clone(), staged.clone(), deadline).await.is_none(), "reservation absent");
        assert!(gate.stage_preparation_for_owner(staged.clone(), generation.clone(), deadline, 1, Some(desired)).await);
        assert!(!gate.may_commit_preparation_for_owner(&staged, 1).await, "no commit reservation");
        assert!(gate.staged_observation_is_current(proof.clone(), staged.clone(), deadline).await.is_some(), "real Staged is observable before commit");
        assert!(gate.begin_abort_preparation_for_owner(&staged, 1).await);
        assert!(gate.staged_observation_is_current(proof.clone(), staged.clone(), deadline).await.is_none(), "aborting is not observation authority");
        serve.shared.sessions.lock().await.remove(&session_id);
        insert_control_session(&serve, &session_id, rendition, Instant::now()).await;
        assert!(!gate.observation_is_current(proof).await, "retired exact attachment cannot be followed");

    }

    #[tokio::test]
    async fn resolved_vod_owner_cannot_commit_after_tombstone_or_reattachment() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("sess-a", 0).await;
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                children: Vec::new(),
                passive_grant: None,
                rendition: Some(Arc::clone(&rendition)),
                retained_output: None,
                rendition_key: rendition.key.clone(),
                file: Arc::new(rendition.recipe.file.clone()),
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                sdr_master_codecs: false,
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                control_observed_at: None,
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                prepared_incarnation: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );

        let (_, stale_owner) = serve
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("playlist");
        assert!(serve.response_owner_is_live("sess-a", &stale_owner).await);
        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(!serve.response_owner_is_live("sess-a", &stale_owner).await);
        assert!(
            !serve
                .commit_resolved_media("sess-a", &stale_owner, Some(0))
                .await
        );

        let replacement_touch = Instant::now() - Duration::from_secs(5);
        let replacement_key = rendition.key.clone();
        let replacement_file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                children: Vec::new(),
                passive_grant: None,
                rendition: Some(rendition),
                rendition_key: replacement_key,
                retained_output: None,
                file: replacement_file,
                playback_id: "play-b".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 2,
                target_height: 360,
                kind: request("play-b", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                sdr_master_codecs: false,
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(replacement_touch),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                control_observed_at: None,
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                prepared_incarnation: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
        assert!(!serve.response_owner_is_live("sess-a", &stale_owner).await);
        assert!(
            !serve
                .commit_resolved_media("sess-a", &stale_owner, Some(0))
                .await
        );
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            replacement_touch,
            "an old incarnation cannot renew a new attachment under the same id",
        );
    }

    #[tokio::test]
    async fn idle_reap_and_same_id_resurrection_are_one_reader_transition() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;
        let (_, stale_owner) = serve
            .playlist("sess-a")
            .await
            .expect("ours")
            .expect("playlist");
        *serve.shared.sessions.lock().await["sess-a"]
            .last_touch
            .lock()
            .expect("touch lock") = Instant::now() - SESSION_IDLE_TTL - Duration::from_secs(1);

        // Stop the old reap exactly after registry removal but before reader
        // detach. Resurrection must wait on the stable per-id lifecycle gate,
        // not attach a successor that the old reap can then remove.
        let readers = rendition.readers.lock().await;
        let maintain = tokio::spawn({
            let serve = Arc::clone(&serve);
            async move { serve.maintain().await }
        });
        wait_until("idle registry removal", Duration::from_secs(2), || {
            let serve = Arc::clone(&serve);
            async move { !serve.shared.sessions.lock().await.contains_key("sess-a") }
        })
        .await;
        let resurrect = tokio::spawn({
            let serve = Arc::clone(&serve);
            let file = file.clone();
            async move { create(&serve, &file, "sess-a", "play-b", &settings()).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !resurrect.is_finished(),
            "resurrection waits until the old reader detach is complete"
        );

        drop(readers);
        maintain.await.expect("maintenance task");
        resurrect.await.expect("resurrection task");
        assert!(serve.shared.sessions.lock().await.contains_key("sess-a"));
        assert_eq!(
            rendition
                .readers
                .lock()
                .await
                .get("sess-a")
                .cloned()
                .expect("replacement reader")
                .last_served,
            None,
            "the replacement reader survives the predecessor's reap"
        );
        assert!(
            !serve
                .commit_resolved_media("sess-a", &stale_owner, Some(1))
                .await,
            "the old response incarnation cannot commit into the replacement"
        );
        assert_eq!(
            rendition.readers.lock().await["sess-a"].last_served,
            None,
            "the stale completion did not move the replacement frontier"
        );
    }

    #[tokio::test]
    async fn vod_control_renews_only_fresh_sequences_and_lifecycle_is_per_session() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = local_serve(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        let before = Instant::now() - Duration::from_secs(10);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), before).await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence, owner_epoch| crate::playback_control::LocalControlRequest {
            session_id: &session_id,
            generation: &generation,
            owner_node_id: "node-a",
            owner_epoch,
            client_instance_id: &client,
            sequence,
            snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Apple,
            ),
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };

        let accepted = serve
            .control(request(1, 1))
            .await
            .expect("VOD registry owner")
            .expect("accepted control");
        assert_eq!(
            accepted.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        let accepted_touch = {
            let sessions = serve.shared.sessions.lock().await;
            let last = sessions[&session_id].last_touch.lock().expect("touch lock");
            assert!(*last > before);
            *last
        };
        let replay = serve
            .control(request(1, 1))
            .await
            .expect("VOD registry owner")
            .expect("replay control");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(
            *serve.shared.sessions.lock().await[&session_id]
                .last_touch
                .lock()
                .expect("touch lock"),
            accepted_touch
        );
        assert!(matches!(
            serve.control(request(0, 1)).await,
            Some(Err(
                crate::playback_control::ControlStateError::StaleSequence
            ))
        ));
        assert!(matches!(
            serve.control(request(2, 2)).await,
            Some(Err(
                crate::playback_control::ControlStateError::OwnerChanged
            ))
        ));
        assert_eq!(
            *serve.shared.sessions.lock().await[&session_id]
                .last_touch
                .lock()
                .expect("touch lock"),
            accepted_touch
        );

        // Holding A's lifecycle cannot head-of-line block an unrelated VOD
        // terminal transition. A node-wide gate makes this timeout.
        let other_id = uuid::Uuid::new_v4().to_string();
        insert_control_session(&serve, &other_id, rendition, Instant::now()).await;
        let lifecycle = Arc::clone(&serve.shared.sessions.lock().await[&session_id].lifecycle);
        let lifecycle_guard = lifecycle.lock().await;
        assert!(tokio::time::timeout(
            Duration::from_millis(250),
            serve.end(&other_id, Terminal::Deleted),
        )
        .await
        .expect("unrelated lifecycle must not queue"));
        drop(lifecycle_guard);
    }

    #[tokio::test(start_paused = true)]
    async fn accepted_vod_control_end_tombstones_detaches_and_replays_exactly() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = local_serve(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        let before = Instant::now() - Duration::from_secs(10);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), before).await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence| {
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Apple,
            );
            snapshot.demand = crate::playback_control::PlaybackDemand::End;
            snapshot.playback_rate = 0.0;
            snapshot.render_state = crate::playback_control::RenderState::Ended;
            crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: &client,
                sequence,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            }
        };

        let committer = Arc::new(CleanupObservingCommitter {
            rendition: Arc::clone(&rendition),
            session_id: session_id.clone(),
            started: AtomicBool::new(false),
            reader_detached: AtomicBool::new(false),
            attempts: Arc::new(AtomicUsize::new(0)),
            expires_at_unix_ms: crate::media_sessions::unix_ms().saturating_add(
                i64::try_from((TERMINAL_TOMBSTONE_RETENTION * 2).as_millis()).unwrap_or(i64::MAX),
            ),
        });
        let committer_weak = Arc::downgrade(&committer);
        let accepted = serve
            .control_with_terminal(request(1), i64::MAX, Some(committer.clone()), None)
            .await
            .expect("VOD registry owner")
            .expect("end accepted");
        assert_eq!(
            accepted.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        assert_eq!(accepted.lease_state, "ended");
        assert_eq!(
            *serve.shared.sessions.lock().await[&session_id]
                .last_touch
                .lock()
                .expect("touch lock"),
            before,
            "terminal control is not a five-minute lease renewal"
        );
        assert_eq!(
            serve.shared.sessions.lock().await[&session_id].tombstone,
            Some(Terminal::Deleted)
        );
        assert!(!rendition.readers.lock().await.contains_key(&session_id));
        assert!(serve.status(&session_id).await.is_none());
        assert!(committer.started.load(Acquire));
        assert!(
            committer.reader_detached.load(Acquire),
            "the durable terminal commit cannot start before VOD reader detach"
        );
        assert!(matches!(
            accepted
                .terminal_commit
                .as_ref()
                .expect("deferred terminal receipt")
                .wait()
                .await,
            Err(())
        ));
        assert_eq!(committer.attempts.load(Acquire), 1);

        let replay = serve
            .control(request(1))
            .await
            .expect("terminal VOD registry owner")
            .expect("exact terminal replay");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(replay.accepted_sequence, accepted.accepted_sequence);
        assert_eq!(replay.lease_state, "ended");
        let committed = replay
            .terminal_commit
            .as_ref()
            .expect("retried VOD terminal receipt")
            .wait()
            .await
            .expect("recovered VOD terminal response");
        assert_eq!(committed.server_time_unix_ms, 1);
        assert_eq!(committer.attempts.load(Acquire), 2);

        let active_same_sequence = crate::playback_control::LocalControlRequest {
            session_id: &session_id,
            generation: &generation,
            owner_node_id: "node-a",
            owner_epoch: 1,
            client_instance_id: &client,
            sequence: 1,
            snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Apple,
            ),
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };
        assert!(matches!(
            serve.control(active_same_sequence).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));
        assert!(matches!(
            serve.control(request(2)).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));

        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION).await;
        {
            let sessions = serve.shared.sessions.lock().await;
            let terminal = &sessions[&session_id];
            assert!(
                terminal
                    .terminal_cleanup
                    .as_ref()
                    .is_some_and(|cleanup| cleanup.retention_expired()),
                "cleanup retention has elapsed for this interleave"
            );
            assert!(
                !terminal.terminal_replay_expired(),
                "an unexpired terminal commit receipt independently fences eviction"
            );
        }

        let terminal_deadline = accepted
            .terminal_commit
            .as_ref()
            .expect("accepted terminal receipt")
            .deadline_for_test();
        tokio::time::advance(
            terminal_deadline
                .duration_since(tokio::time::Instant::now())
                .saturating_sub(Duration::from_millis(1)),
        )
        .await;
        assert_eq!(
            serve
                .control(request(1))
                .await
                .expect("VOD tombstone before expiry")
                .expect("exact response retained before expiry")
                .disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert!(serve.shared.sessions.lock().await[&session_id]
            .control_end
            .is_some());

        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(
            serve.control(request(1)).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));
        let sessions = serve.shared.sessions.lock().await;
        assert_eq!(sessions[&session_id].tombstone, Some(Terminal::Deleted));
        assert!(
            sessions[&session_id].control_end.is_none(),
            "expired VOD recovery drops the retained operation but keeps its ownership tombstone"
        );
        let cleanup_before_repeat = sessions[&session_id]
            .terminal_cleanup
            .as_ref()
            .map(Arc::clone)
            .expect("completed terminal cleanup marker survives expiry");
        assert!(cleanup_before_repeat.is_finished());
        drop(sessions);
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(
            serve.control(request(1)).await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));

        assert!(
            serve.end(&session_id, Terminal::AdminStop).await,
            "a repeated non-control end remains idempotent after response expiry"
        );
        let cleanup_after_repeat = serve.shared.sessions.lock().await[&session_id]
            .terminal_cleanup
            .as_ref()
            .map(Arc::clone)
            .expect("idempotent terminal cleanup marker");
        assert!(
            Arc::ptr_eq(&cleanup_before_repeat, &cleanup_after_repeat),
            "expiry plus repeated end must not create a second cleanup or lifecycle-emission task"
        );

        serve.shared.sessions.lock().await.remove(&session_id);
        drop(accepted);
        drop(replay);
        drop(committer);
        assert!(
            committer_weak.upgrade().is_none(),
            "removing the VOD tombstone must release its deferred operation graph"
        );
    }

    #[tokio::test]
    async fn vod_end_transfers_a_rollover_abort_before_tombstoning() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = local_serve(base.path().to_path_buf(), store.clone());
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, rendition, Instant::now()).await;

        let epoch_one_client = uuid::Uuid::new_v4().to_string();
        serve
            .control(crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: &epoch_one_client,
                sequence: 1,
                snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Apple,
                ),
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            })
            .await
            .expect("VOD registry owner")
            .expect("epoch one control accepted");

        let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
        {
            let sessions = serve.shared.sessions.lock().await;
            assert!(sessions[&session_id]
                .control
                .lock()
                .expect("control lock")
                .stage_preparation_for_owner(
                    staged_incarnation_id.clone(),
                    generation.clone(),
                    i64::MAX,
                    1,
                    None,
                ));
        }

        let route = store
            .media_session_route(&session_id)
            .await
            .expect("route read")
            .expect("active route");
        let takeover_at = route.lease_expires_at_ms.saturating_add(1);
        let transferred = store
            .claim_media_session_takeover(&plurx_core::domain::MediaSessionTakeover {
                incarnation_id: generation.clone(),
                expected_owner_node_id: "node-a".to_owned(),
                expected_owner_epoch: 1,
                next_owner_node_id: "node-b".to_owned(),
                now_ms: takeover_at,
                lease_expires_at_ms: takeover_at.saturating_add(60_000),
            })
            .await
            .expect("takeover")
            .expect("epoch two route");
        assert_eq!(transferred.owner_epoch, 2);

        let admission = Arc::new(RecordingPreparationAdmission::default());
        let epoch_two_client = uuid::Uuid::new_v4().to_string();
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        snapshot.demand = crate::playback_control::PlaybackDemand::End;
        snapshot.playback_rate = 0.0;
        snapshot.render_state = crate::playback_control::RenderState::Ended;
        let ended = serve
            .control_with_terminal(
                crate::playback_control::LocalControlRequest {
                    session_id: &session_id,
                    generation: &generation,
                    owner_node_id: "node-b",
                    owner_epoch: 2,
                    client_instance_id: &epoch_two_client,
                    sequence: 1,
                    snapshot,
                    prepared_successor:
                        crate::playback_control::PreparedSuccessorObservation::NotRequested,
                },
                i64::MAX,
                None,
                Some(admission.clone()),
            )
            .await
            .expect("VOD registry owner")
            .expect("epoch two End accepted");
        assert_eq!(ended.lease_state, "ended");
        assert_eq!(
            admission
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(|outcome| &outcome.preparation_directive),
            Some(&crate::playback_control::PreparationDirective::Abort {
                staged_incarnation_id,
                acknowledgement_rejected: false,
            }),
            "the durable abort owner must receive the inherited slot before End clears it"
        );
        assert_eq!(
            serve.shared.sessions.lock().await[&session_id].tombstone,
            Some(Terminal::Deleted)
        );
    }

    #[tokio::test]
    async fn cancelled_vod_end_response_cannot_cancel_terminal_reader_cleanup() {
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        activate_control_route(store.as_ref(), &session_id, &generation).await;
        let serve = local_serve(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let client = uuid::Uuid::new_v4().to_string();
        let committer = Arc::new(CleanupObservingCommitter {
            rendition: Arc::clone(&rendition),
            session_id: session_id.clone(),
            started: AtomicBool::new(false),
            reader_detached: AtomicBool::new(false),
            attempts: Arc::new(AtomicUsize::new(0)),
            expires_at_unix_ms: crate::media_sessions::unix_ms().saturating_add(60_000),
        });

        // Pause the session-owned task at the exact pre-detach point. Status
        // preparation is therefore free to inspect the reader registry before
        // End publishes its cleanup marker.
        let terminal_detach_pause = serve.arm_terminal_detach_pause_for_test();
        let control_applied = serve
            .shared
            .test_hooks()
            .control_applied
            .arm("vod control applied");
        let pending = {
            let serve = Arc::clone(&serve);
            let session_id = session_id.clone();
            let generation = generation.clone();
            let client = client.clone();
            let committer = committer.clone();
            tokio::spawn(async move {
                let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Apple,
                );
                snapshot.demand = crate::playback_control::PlaybackDemand::End;
                snapshot.playback_rate = 0.0;
                snapshot.render_state = crate::playback_control::RenderState::Ended;
                serve
                    .control_with_terminal(
                        crate::playback_control::LocalControlRequest {
                            session_id: &session_id,
                            generation: &generation,
                            owner_node_id: "node-a",
                            owner_epoch: 1,
                            client_instance_id: &client,
                            sequence: 1,
                            snapshot,
                            prepared_successor:
                                crate::playback_control::PreparedSuccessorObservation::NotRequested,
                        },
                        i64::MAX,
                        Some(committer),
                        None,
                    )
                    .await
            })
        };
        // The applied point sits after the End commit published the
        // session-owned cleanup and before that cleanup is spawned.
        let applied = control_applied.reached().await;
        assert!(
            serve.shared.sessions.lock().await[&session_id]
                .terminal_cleanup
                .is_some(),
            "the End commit publishes its cleanup before the applied point"
        );
        assert!(
            !terminal_detach_pause.was_reached(),
            "the terminal cleanup is spawned after the applied point"
        );
        assert!(rendition.readers.lock().await.contains_key(&session_id));
        applied.release();
        let cleanup = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let cleanup = serve.shared.sessions.lock().await[&session_id]
                    .terminal_cleanup
                    .as_ref()
                    .map(Arc::clone);
                if let Some(cleanup) = cleanup {
                    return cleanup;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal commit publishes session-owned cleanup");
        let detach_held = terminal_detach_pause.reached().await;
        assert!(!cleanup.is_finished());
        pending.abort();
        let cancellation = match pending.await {
            Err(error) => error,
            Ok(_) => panic!("request unexpectedly completed"),
        };
        assert!(cancellation.is_cancelled());

        let terminal_replay_pause = serve
            .shared
            .test_hooks()
            .terminal_replay
            .arm("vod terminal replay before the detach fence");
        let replay = {
            let serve = Arc::clone(&serve);
            let session_id = session_id.clone();
            let generation = generation.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Apple,
                );
                snapshot.demand = crate::playback_control::PlaybackDemand::End;
                snapshot.playback_rate = 0.0;
                snapshot.render_state = crate::playback_control::RenderState::Ended;
                serve
                    .control(crate::playback_control::LocalControlRequest {
                        session_id: &session_id,
                        generation: &generation,
                        owner_node_id: "node-a",
                        owner_epoch: 1,
                        client_instance_id: &client,
                        sequence: 1,
                        snapshot,
                        prepared_successor:
                            crate::playback_control::PreparedSuccessorObservation::NotRequested,
                    })
                    .await
            })
        };
        let replay_held = terminal_replay_pause.reached().await;
        assert!(
            !committer.started.load(Acquire),
            "a replacement waiter at the cleanup fence cannot expose terminal settlement while detach is pinned"
        );
        replay_held.release();
        assert!(rendition.readers.lock().await.contains_key(&session_id));
        detach_held.release();
        tokio::time::timeout(Duration::from_secs(1), cleanup.wait())
            .await
            .expect("detached cleanup survives request cancellation");
        assert!(!rendition.readers.lock().await.contains_key(&session_id));
        assert!(committer.started.load(Acquire));
        assert!(committer.reader_detached.load(Acquire));

        let replay = replay
            .await
            .expect("replacement replay task")
            .expect("terminal VOD session")
            .expect("terminal replay after cleanup");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert!(replay
            .terminal_commit
            .as_ref()
            .expect("replacement receipt")
            .wait()
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn accepted_seek_intent_controls_demand_and_retention_in_both_directions() {
        const VIEWER: &str = "00000000-0000-4000-8000-000000000001";
        const GENERATION: &str = "00000000-0000-4000-8000-000000000002";
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        activate_control_route(store.as_ref(), VIEWER, GENERATION).await;
        let serve = local_serve(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, VIEWER, Arc::clone(&rendition), Instant::now()).await;
        let mut soundtrack = synthetic_rendition(&base.path().join("soundtrack")).await;
        let audio = Arc::get_mut(&mut soundtrack).expect("exclusive soundtrack");
        audio.key = "parent-soundtrack".into();
        audio.plan = plurx_core::transcode::vod_shared_audio_plan(plan_duration_ms(&rendition.plan), 128);
        audio.timescale = audio.plan.timescale;
        audio.manifest = Mutex::new(Manifest::new(audio.plan.clone()));
        soundtrack.attach_reader("private-audio", 0).await;
        rendition.attach_reader("private-root", 0).await;
        {
            let mut sessions = serve.shared.sessions.lock().await;
            let session = sessions.get_mut(VIEWER).expect("parent");
            session.children.push(ParentMediaReader { controlled: false, candidate_id: None, reader_id: "private-audio".into(),
                rendition: Arc::clone(&soundtrack), _reservation: None });
            session.children.push(ParentMediaReader { controlled: false, candidate_id: None, reader_id: "private-root".into(),
                rendition: Arc::clone(&rendition), _reservation: None });
        }
        let control = |sequence, snapshot| crate::playback_control::LocalControlRequest {
            session_id: VIEWER,
            generation: GENERATION,
            owner_node_id: "node-a",
            owner_epoch: 1,
            client_instance_id: "00000000-0000-4000-8000-000000000005",
            sequence,
            snapshot,
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };
        let key = |index| WaitKey {
            rendition: rendition.key.clone(),
            index,
        };
        let old = serve
            .shared
            .pool
            .register(key(3), VIEWER)
            .expect("old GET admitted");
        let current = serve
            .shared
            .pool
            .register(key(45), VIEWER)
            .expect("seek GET admitted");
        let prefetch = serve
            .shared
            .pool
            .register(key(58), VIEWER)
            .expect("prefetch admitted");
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        snapshot.render_state = crate::playback_control::RenderState::Seeking;
        snapshot.seek_target_ms = Some(
            (rendition.plan.entry(45).expect("target").start_ticks * 1000
                / u64::from(rendition.timescale)) as i64
                + 1,
        );
        serve
            .control(control(1, snapshot.clone()))
            .await
            .expect("VOD session")
            .expect("accepted seek");
        let forward_anchor = snapshot.buffer_anchor_ms();
        let expected_audio = media_entry_containing_ms(&soundtrack.plan, forward_anchor);
        assert!(expected_audio > 0, "AAC-only entries must advance beyond zero");
        assert_eq!(soundtrack.readers.lock().await["private-audio"].frontier, expected_audio);
        assert_eq!(soundtrack.readers.lock().await["private-audio"].control_sequence, Some(1));
        assert_eq!(rendition.readers.lock().await["private-root"].frontier, 45);
        let position = Position {
            produced_through: None,
            positioned_at: None,
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: false,
            working_set: WorkingSet::default(),
        };
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        assert_eq!(
            decide(&*rendition.manifest.lock().await, &demands, position, &[]),
            Action::Reposition { to: 45 }
        );
        assert!(
            !reader_window(&readers[VIEWER], rendition.seconds_per_segment).covers(3),
            "a forward seek does not protect the entire intervening film"
        );
        drop(readers);

        snapshot.seek_target_ms = Some(
            (rendition.plan.entry(3).expect("rewind").start_ticks * 1000
                / u64::from(rendition.timescale)) as i64
                + 1,
        );
        let mut rewind = serve
            .control(control(2, snapshot.clone()))
            .await
            .expect("VOD session");
        if let Err(crate::playback_control::ControlStateError::RateLimited(ms)) = rewind {
            tokio::time::sleep(Duration::from_millis(u64::from(ms) + 1)).await;
            rewind = serve
                .control(control(2, snapshot))
                .await
                .expect("VOD session");
        }
        rewind.expect("accepted rewind");
        let rewind_anchor = (rendition.plan.entry(3).expect("rewind").start_ticks * 1000
            / u64::from(rendition.timescale)) as i64 + 1;
        let expected_audio = media_entry_containing_ms(&soundtrack.plan, rewind_anchor);
        let mut audio_readers = soundtrack.readers.lock().await;
        audio_readers.get_mut("private-audio").expect("private reader").served(70);
        assert_eq!(audio_readers["private-audio"].frontier, expected_audio,
            "late soundtrack delivery cannot undo the parent seek");
        assert_eq!(audio_readers["private-audio"].control_sequence, Some(2));
        drop(audio_readers);
        assert_eq!(rendition.readers.lock().await["private-root"].frontier, 3);
        drop(prefetch);
        let near_prefetch = serve
            .shared
            .pool
            .register(key(10), VIEWER)
            .expect("nearby prefetch admitted");
        let mut readers = rendition.readers.lock().await;
        // An old high GET completing after the backward seek is delivery
        // evidence, not a new playback command.
        readers.get_mut(VIEWER).expect("reader").served(45);
        assert_eq!(readers[VIEWER].frontier, 3);
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        assert_eq!(
            decide(
                &*rendition.manifest.lock().await,
                &demands,
                Position {
                    positioned_at: Some(10),
                    ..position
                },
                &[]
            ),
            Action::Reposition { to: 3 }
        );
        assert!(!reader_window(&readers[VIEWER], rendition.seconds_per_segment).covers(45));
        drop(readers);
        assert_eq!(
            serve.shared.pool.retained(&rendition.key),
            vec![3, 10, 45],
            "every admitted request still has a narrow pin"
        );
        drop((current, near_prefetch));
        assert_eq!(serve.shared.pool.retained(&rendition.key), vec![3]);
        drop(old);
        assert!(serve.shared.pool.is_empty());
    }

    #[tokio::test]
    async fn cancelling_after_control_acceptance_keeps_the_anchor_and_exact_replay() {
        const VIEWER: &str = "00000000-0000-4000-8000-000000000003";
        const GENERATION: &str = "00000000-0000-4000-8000-000000000004";
        let base = crate::test_tempdir().expect("base");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        activate_control_route(store.as_ref(), VIEWER, GENERATION).await;
        let serve = local_serve(base.path().to_path_buf(), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, VIEWER, Arc::clone(&rendition), Instant::now()).await;
        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        snapshot.render_state = crate::playback_control::RenderState::Seeking;
        snapshot.seek_target_ms = Some(300_000);
        let target = entry_containing(&rendition.plan, 300.0);
        let ledger = Arc::clone(&rendition.readers.lock().await[VIEWER].marker_prewarm);
        ledger.lock().expect("ledger").enabled = true;
        let pause = serve
            .shared
            .test_hooks()
            .control_applied
            .arm("vod control applied");
        // Armed across the whole exchange: only a terminal replay reaches it.
        let terminal_replay = serve
            .shared
            .test_hooks()
            .terminal_replay
            .arm("vod terminal replay before the detach fence");
        let accepted = {
            let serve = Arc::clone(&serve);
            let snapshot = snapshot.clone();
            tokio::spawn(async move {
                serve
                    .control(crate::playback_control::LocalControlRequest {
                        session_id: VIEWER,
                        generation: GENERATION,
                        owner_node_id: "node-a",
                        owner_epoch: 1,
                        client_instance_id: "00000000-0000-4000-8000-000000000006",
                        sequence: 1,
                        snapshot,
                        prepared_successor:
                            crate::playback_control::PreparedSuccessorObservation::NotRequested,
                    })
                    .await
            })
        };
        let held = pause.reached().await;
        assert_eq!(
            rendition.readers.lock().await[VIEWER].frontier,
            target,
            "accepted sequence and playback anchor commit before any cancellable side effects"
        );
        assert!(!ledger.lock().expect("ledger").enabled, "accepted intent invalidates old speculation even when the later marker update is cancelled");
        accepted.abort();
        let _ = accepted.await;
        held.release();
        let replay = serve
            .control(crate::playback_control::LocalControlRequest {
                session_id: VIEWER,
                generation: GENERATION,
                owner_node_id: "node-a",
                owner_epoch: 1,
                client_instance_id: "00000000-0000-4000-8000-000000000006",
                sequence: 1,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            })
            .await
            .expect("VOD session")
            .expect("exact replay");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(rendition.readers.lock().await[VIEWER].frontier, target);
        assert!(
            !terminal_replay.was_reached(),
            "the terminal-replay point follows the lookup that found a terminal replay"
        );
        terminal_replay.release();
    }

    #[tokio::test]
    async fn a_published_blocked_get_keeps_its_pin_through_the_response_file_open() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("viewer", 3).await;
        let registered = serve
            .shared
            .test_hooks()
            .segment_wait_registered
            .arm("vod segment wait registered");
        let ready = serve
            .shared
            .test_hooks()
            .segment_ready
            .arm("vod segment ready before open");
        let get = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        45,
                        Duration::from_secs(5),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        registered.reached().await.release();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(
            !ready.was_reached(),
            "the ready point follows the wait's answer, not its registration"
        );
        // Publication follows the production GET's post-registration check;
        // the oneshot remembers Ready even if it precedes the next poll.
        rendition
            .dir
            .materialize(
                &mut *rendition.manifest.lock().await,
                45,
                b"target bytes",
                now_ms(),
            )
            .await
            .expect("publish");
        serve.shared.pool.satisfy(&rendition.key, 45);
        let ready_held = ready.reached().await;
        let windows = eviction_windows(&serve.shared, &rendition).await;
        let mut manifest = rendition.manifest.lock().await;
        rendition
            .dir
            .make_room(&mut manifest, &windows, u64::MAX)
            .await
            .expect("pressure sweep");
        assert!(manifest.state(45).expect("target").is_materialized());
        drop(manifest);
        ready_held.release();
        assert_eq!(get.await.expect("GET task").expect("response file").len, 12);
        assert!(serve.shared.pool.retained(&rendition.key).is_empty());
        let windows = eviction_windows(&serve.shared, &rendition).await;
        let mut manifest = rendition.manifest.lock().await;
        rendition
            .dir
            .make_room(&mut manifest, &windows, u64::MAX)
            .await
            .expect("later sweep");
        assert!(
            !manifest.state(45).expect("target").is_materialized(),
            "opening releases the narrow pin; old destinations do not become permanent retention"
        );
    }

    /// The ready point sits before the blocked GET opens its file: a file
    /// unlinked while the GET is held there is not served, and the GET answers
    /// pending instead.
    #[tokio::test]
    async fn segment_ready_point_precedes_the_file_open() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("viewer", 3).await;
        let registered = serve
            .shared
            .test_hooks()
            .segment_wait_registered
            .arm("vod segment wait registered");
        let ready = serve
            .shared
            .test_hooks()
            .segment_ready
            .arm("vod segment ready before open");
        let get = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        45,
                        Duration::from_secs(5),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        registered.reached().await.release();
        rendition
            .dir
            .materialize(
                &mut *rendition.manifest.lock().await,
                45,
                b"target bytes",
                now_ms(),
            )
            .await
            .expect("publish");
        serve.shared.pool.satisfy(&rendition.key, 45);
        let held = ready.reached().await;
        tokio::fs::remove_file(rendition.dir.path().join(segment_name(45)))
            .await
            .expect("unlink the published file behind the manifest");
        held.release();
        let answer = tokio::time::timeout(Duration::from_secs(5), get)
            .await
            .expect("the GET answers")
            .expect("GET task");
        assert!(
            matches!(answer, Err(VodError::Pending { .. })),
            "the GET opens its file after the ready point, so it finds the file gone"
        );
    }

    /// The VOD registry built by the production constructor reads the no-op
    /// hooks: every pause point is ready at its first poll, the route is read
    /// from the Store, and a blocked GET crosses both of its points to a
    /// response.
    #[tokio::test]
    async fn vod_shared_shipped_shape() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let hooks = serve.shared.hooks.get();
        let installed: &dyn std::any::Any = hooks;
        assert!(
            installed.is::<NoopVodSharedHooks>(),
            "the production constructor leaves the registry on the no-op hooks"
        );
        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        for (point, mut hook) in [
            ("before_terminal_replay_join", hooks.before_terminal_replay_join()),
            ("after_control_applied", hooks.after_control_applied()),
            ("after_segment_wait_registered", hooks.after_segment_wait_registered()),
            ("before_segment_ready_open", hooks.before_segment_ready_open()),
            ("before_terminal_detach", hooks.before_terminal_detach()),
            ("after_rendition_installed", hooks.after_rendition_installed()),
            ("after_dormant_purge_removed", hooks.after_dormant_purge_removed()),
        ] {
            assert!(
                hook.as_mut().poll(&mut context).is_ready(),
                "the production {point} point is ready at its first poll"
            );
        }
        assert_eq!(hooks.terminal_route_outcome("no-route"), None);
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                serve.shared.terminal_route_durably_non_live("no-route"),
            )
            .await
            .expect("the route is read from the Store"),
            "a session with no durable route is not live"
        );

        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("viewer", 3).await;
        let get = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        45,
                        Duration::from_secs(5),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while serve.shared.pool.blocked_on(&rendition.key) != Some(45) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the GET registers its wait");
        rendition
            .dir
            .materialize(
                &mut *rendition.manifest.lock().await,
                45,
                b"target bytes",
                now_ms(),
            )
            .await
            .expect("publish");
        serve.shared.pool.satisfy(&rendition.key, 45);
        let ready = tokio::time::timeout(Duration::from_secs(5), get)
            .await
            .expect("the production segment points do not hold the GET")
            .expect("GET task")
            .expect("response file");
        assert_eq!(ready.len, 12);
    }

    #[test]
    fn controlled_future_loading_retires_work_without_disposing_incumbent_bytes() {
        use plurx_core::playback::continuous_quality::{QualityAttachment, QualityLedger, QualityState, QualityTransaction};
        let identity = || uuid::Uuid::new_v4().to_string();
        let mut ledger = QualityLedger::new(identity(),1,QualityAttachment {
            client_instance_id: identity(), lifetime_id: "film".into(), attachment_id: identity(), family_id: "c".repeat(64),
        }).expect("ledger");
        let transaction = |intent_revision,target_rendition_id:String,state,first_presented_tick:Option<u64>| QualityTransaction {
            transaction_id: identity(),intent_revision,target_rendition_id,state,
            intent_superseded:false,cancel_requested:false,preparation:None,ready:vec![],reserved:vec![],appended:vec![],
            ever_appended:first_presented_tick.is_some(),disposed:vec![],first_presented_tick,
            first_presented_at_ms:first_presented_tick.map(|_| 1),
        };
        ledger.transactions.push(transaction(1,"a".repeat(64),QualityState::Presented,Some(0)));
        ledger.transactions.push(transaction(2,"b".repeat(64),QualityState::Ready,None));
        ledger.latest_intent_revision=2;
        let active = |ledger:&QualityLedger| VodServe::controlled_video_demand(ledger).expect("active")
            .into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(active(&ledger),vec!["a".repeat(64),"b".repeat(64)]);
        ledger.transactions[1].state=QualityState::Scheduled;
        assert_eq!(active(&ledger),vec!["b".repeat(64)]);
        assert_eq!(ledger.transactions[0].first_presented_tick,Some(0));
        assert!(!ledger.transactions[0].cancel_requested);
        ledger.transactions[1].cancel_requested=true;
        assert_eq!(active(&ledger),vec!["a".repeat(64)]);
        ledger.transactions[1].cancel_requested=false;
        ledger.transactions[1].state=QualityState::Presented;
        ledger.transactions[1].first_presented_tick=Some(48);
        assert_eq!(active(&ledger),vec!["b".repeat(64)]);
        ledger.transactions[1].state=QualityState::Disposed;
        assert_eq!(active(&ledger),vec!["b".repeat(64)],"ordinary eviction does not retire wanted loading");
    }

    #[tokio::test]
    async fn cold_controlled_readers_keep_authority_without_driving_shared_work() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        for (id,frontier) in [("parent",0),("cold",45),("other",3)] {
            rendition.attach_reader(id,frontier).await;
        }
        {
            let mut readers = rendition.readers.lock().await;
            for id in ["parent","cold"] {
                let reader = readers.get_mut(id).expect("authority");
                reader.authority_only = true;
                reader.accept_control(1,77);
            }
        }
        let cold = serve.shared.pool.register(WaitKey {
            rendition: rendition.key.clone(), index: 45,
        }, "cold").expect("bounded cold request");
        let active = serve.shared.pool.register(WaitKey {
            rendition: rendition.key.clone(), index: 3,
        }, "other").expect("other viewer");
        let windows = rendition.reader_windows().await;
        assert_eq!(windows.len(),1);
        assert_eq!(windows[0].playhead,3);
        {
            let readers = rendition.readers.lock().await;
            let manifest = rendition.manifest.lock().await;
            let demands = playback_demands(&serve.shared.pool,&rendition,&readers,&manifest);
            assert!(!demands.iter().any(|demand| demand.blocked_on == Some(45)));
            assert!(demands.iter().any(|demand| demand.blocked_on == Some(3) && demand.foreground));
            assert_eq!(readers["cold"].control_sequence,Some(1));
            assert_eq!(readers["cold"].frontier,77);
        }
        {
            let mut readers = rendition.readers.lock().await;
            readers.get_mut("cold").expect("same cold identity").authority_only = false;
            let manifest = rendition.manifest.lock().await;
            let demands = playback_demands(&serve.shared.pool,&rendition,&readers,&manifest);
            assert!(demands.iter().any(|demand| demand.blocked_on == Some(45)));
            assert!(readers["parent"].authority_only);
        }
        assert_eq!(rendition.reader_windows().await.len(),2);
        drop(cold);
        drop(active);
    }

    #[tokio::test]
    async fn admitted_controlled_target_moves_preparation_frontier_without_duplicate_credit() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        let parent = "already-admitted-parent";
        insert_control_session(&serve, parent, Arc::clone(&rendition), Instant::now()).await;
        rendition.attach_reader("target", 3).await;
        let admissions = crate::admission::Admissions::new();
        let estimate = crate::admission::TranscodeResourceEstimate {
            hardware_slot: false, cpu_threads: 1, decoder_threads: Some(1),
        };
        let permit = admissions.try_admit_bundle(0, 1, &estimate, crate::admission::Priority::Speculative)
            .expect("one retained credit");
        serve.shared.sessions.lock().await.get_mut(parent).expect("parent").children.push(ParentMediaReader {
            controlled: true, candidate_id: Some(plurx_core::playback::candidate::CandidateId([1;16])),
            reader_id: "target".into(), rendition: Arc::clone(&rendition), _reservation: Some(permit.into()),
        });
        for frontier_ms in [4_000, 72_000] {
            serve.admit_controlled_video_before(parent, &rendition.key, frontier_ms,
                Instant::now() + Duration::from_secs(1)).await.expect("reuse admitted target");
            let mut readers = rendition.readers.lock().await;
            let target = readers.get_mut("target").expect("target");
            target.accept_control(7, 11);
            assert_eq!(target.preparation_frontier, Some(media_entry_containing_ms(&rendition.plan, frontier_ms)));
            assert_eq!(target.frontier, 11, "the ordinary heartbeat remains independent");
            assert!(!target.authority_only);
            let manifest = rendition.manifest.lock().await;
            assert!(playback_demands(&serve.shared.pool, &rendition, &readers, &manifest)
                .iter().any(|demand| demand.frontier == media_entry_containing_ms(&rendition.plan, frontier_ms)));
            assert_eq!(admissions.software_in_use(), 1, "reuse never reserves another permit");
        }
        serve.shared.sessions.lock().await.remove(parent);
        assert_eq!(admissions.software_in_use(), 0, "the parent still owns and releases the credit");
    }

    #[tokio::test]
    async fn controlled_preparation_frontier_survives_incumbent_control_before_wait_registration() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("target", 3).await;
        let mut readers = rendition.readers.lock().await;
        let target = readers.get_mut("target").expect("target");
        target.preparation_frontier = Some(36);
        target.accept_control(7, 11);
        let manifest = rendition.manifest.lock().await;
        let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
        assert!(demands.iter().any(|demand| demand.frontier == 36 && demand.blocked_on == Some(36) && demand.foreground));
        assert_eq!(readers["target"].frontier, 11, "ordinary control still owns playback position");
        assert_eq!(readers["target"].control_sequence, Some(7));
        readers.get_mut("target").expect("target").preparation_frontier = None;
        let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
        assert!(demands.iter().any(|demand| demand.frontier == 11));
        assert!(!demands.iter().any(|demand| demand.frontier == 36));
    }

    #[tokio::test]
    async fn admitted_preparation_outranks_its_stale_get_before_target_wait_registration() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("target", 7).await;
        {
            let mut readers = rendition.readers.lock().await;
            let target = readers.get_mut("target").expect("target");
            target.accept_control(1, 7);
            target.preparation_frontier = Some(24);
        }
        let old = serve.shared.pool.register(WaitKey {
            rendition: rendition.key.clone(), index: 7,
        }, "target").expect("old GET admitted");
        let position = Position {
            produced_through: None, positioned_at: None,
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: false, working_set: WorkingSet::default(),
        };
        {
            let readers = rendition.readers.lock().await;
            let manifest = rendition.manifest.lock().await;
            let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
            assert_eq!(decide(&manifest, &demands, position, &[]), Action::Reposition { to: 24 });
            assert!(demands.iter().any(|demand| demand.blocked_on == Some(24) && demand.bounded_preparation));
            let running = Position { positioned_at: Some(21), ..position };
            assert_eq!(decide(&manifest, &demands, running, &[]), Action::Reposition { to: 24 },
                "an already-running target cannot spend the copy horizon on old gaps");
            assert!(demands.iter().any(|demand| demand.blocked_on == Some(7) && !demand.foreground), "the old obligation is retained");
            assert_eq!(readers["target"].frontier, 7, "ordinary playback reporting does not move");
        }
        rendition.attach_reader("other-viewer", 3).await;
        let other = serve.shared.pool.register(WaitKey {
            rendition: rendition.key.clone(), index: 3,
        }, "other-viewer").expect("foreground viewer admitted");
        {
            let readers = rendition.readers.lock().await;
            let manifest = rendition.manifest.lock().await;
            let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
            assert_eq!(decide(&manifest, &demands, position, &[]), Action::Reposition { to: 3 }, "preparation does not displace another viewer's ordered GET");
        }
        drop(other);
        rendition.readers.lock().await.get_mut("target").expect("target").preparation_frontier = None;
        let readers = rendition.readers.lock().await;
        let manifest = rendition.manifest.lock().await;
        let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
        assert_eq!(decide(&manifest, &demands, position, &[]), Action::Reposition { to: 7 });
        drop(old);
    }

    #[tokio::test]
    async fn legacy_and_controlled_readers_both_receive_fair_current_demand() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("legacy", 0).await;
        rendition.attach_reader("controlled", 45).await;
        rendition
            .readers
            .lock()
            .await
            .get_mut("controlled")
            .expect("reader")
            .accept_control(1, 45);
        let old = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 3,
                },
                "legacy",
            )
            .expect("legacy admitted");
        let newer = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 45,
                },
                "controlled",
            )
            .expect("controlled admitted");
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        let position = Position {
            produced_through: None,
            positioned_at: Some(45),
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: false,
            working_set: WorkingSet::default(),
        };
        assert_eq!(
            decide(&*rendition.manifest.lock().await, &demands, position, &[]),
            Action::Reposition { to: 3 }
        );
        drop(readers);
        drop(old);
        let far = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 55,
                },
                "legacy",
            )
            .expect("far legacy seek admitted");
        drop(newer);
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(
            &serve.shared.pool,
            &rendition,
            &readers,
            &*rendition.manifest.lock().await,
        );
        assert!(
            demands
                .iter()
                .any(|demand| demand.blocked_on == Some(55) && demand.foreground),
            "a legacy seek outside the last delivered window remains a fair candidate"
        );
        drop(far);
    }

    #[test]
    fn an_accepted_new_owner_sequence_one_replaces_the_old_owner_anchor() {
        let mut reader = Reader::new(3);
        reader.accept_control(90, 45);
        reader.accept_control(1, 3);
        assert_eq!(reader.frontier, 3);
        assert_eq!(reader.control_sequence, Some(1));
    }

    #[tokio::test]
    async fn a_published_nearest_request_cannot_hide_the_next_current_get() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        rendition.attach_reader("viewer", 45).await;
        rendition
            .readers
            .lock()
            .await
            .get_mut("viewer")
            .expect("reader")
            .accept_control(1, 45);
        let mut waits = Vec::new();
        for index in [3, 45, 46] {
            waits.push(
                serve
                    .shared
                    .pool
                    .register(
                        WaitKey {
                            rendition: rendition.key.clone(),
                            index,
                        },
                        "viewer",
                    )
                    .expect("admitted"),
            );
        }
        let mut manifest = rendition.manifest.lock().await;
        rendition
            .dir
            .materialize(&mut manifest, 45, b"published", now_ms())
            .await
            .expect("publish before satisfy");
        let readers = rendition.readers.lock().await;
        let demands = playback_demands(&serve.shared.pool, &rendition, &readers, &manifest);
        let position = Position {
            produced_through: Some(45),
            positioned_at: Some(45),
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: false,
            working_set: WorkingSet::default(),
        };
        assert_eq!(
            decide(&manifest, &demands, position, &[]),
            Action::Produce { next: 46 },
            "old request3 must not win the publication-to-satisfy interval"
        );
        drop(waits);
    }

    #[tokio::test]
    async fn a_zero_progress_eviction_does_not_self_schedule_a_hot_loop() {
        use futures_util::FutureExt;
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .working_set_budget = 1;
        rendition.attach_reader("viewer", 0).await;
        rendition
            .dir
            .materialize(
                &mut *rendition.manifest.lock().await,
                0,
                b"protected bytes",
                now_ms(),
            )
            .await
            .expect("publish");
        serve.shared.working_set.store(15, Relaxed);
        let pending = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 1,
                },
                "viewer",
            )
            .expect("admitted");
        driver_pass(&serve.shared, &rendition).await;
        assert!(rendition.wake.notified().now_or_never().is_none(), "zero-byte sweep waits for demand/capacity/maintenance change instead of kicking itself forever");
        assert_eq!(serve.shared.working_set.load(Relaxed), 15);
        #[cfg(unix)]
        {
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .expect("fake running producer");
            rendition.slot.attach(child, 0).await;
            driver_pass(&serve.shared, &rendition).await;
            assert!(
                matches!(rendition.slot.belief().await, Producer::Absent { .. }),
                "zero-progress capacity hold releases a running producer"
            );
            assert_eq!(
                rendition.gen_epoch.load(Relaxed),
                1,
                "queued producer writes are fenced"
            );
            assert!(rendition.wake.notified().now_or_never().is_none());
        }
        drop(pending);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_ahead_producer_is_stopped_not_killed() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake producer");
        rendition.slot.attach(child, 0).await;

        perform_driver_step(&serve.shared, &rendition, Step::Stop)
            .await
            .expect("ahead stop");

        assert!(matches!(
            rendition.slot.belief().await,
            Producer::Stopped {
                reason: crate::prodsched::Hold::Ahead { .. },
                ..
            }
        ));
        assert!(rendition.ahead_hold.load(Acquire));
        perform_driver_step(
            &serve.shared,
            &rendition,
            Step::Terminate {
                why: Termination::Idle,
            },
        )
        .await
        .expect("cleanup fake producer");
    }

    #[cfg(unix)]
    async fn encoded_driver_fixture(
        base: &Path,
    ) -> (
        Arc<VodServe>,
        Arc<Rendition>,
        Arc<crate::vodencode::Encoding>,
        Arc<crate::vodencode::Encoding>,
    ) {
        let serve = bare_serve(base);
        let (file, encoding_a) = encoded_fixture(base).await;
        let encoding_b = encoding_a
            .clone_with_admissions_for_test(encoding_a.admissions.clone())
            .await;
        encoding_a
            .store
            .put_setting(
                plurx_core::store::keys::SW_POOL_THREADS,
                &encoding_a.resources.cpu_threads.max(1).to_string(),
            )
            .await
            .expect("one-encoder software budget");
        let permit = encoding_a.try_permit().await.expect("encoder permit");
        let mut rendition = synthetic_rendition(base).await;
        let mutable = Arc::get_mut(&mut rendition).expect("unshared rendition");
        mutable.recipe.file = file;
        mutable.recipe.encoding = Some(Arc::clone(&encoding_a));
        park_stopped_at_horizon(&serve, &rendition, permit).await;
        (serve, rendition, encoding_a, encoding_b)
    }

    /// Materialize a rendition through its ahead horizon, give it a fake
    /// running producer holding `permit`, and register it, so the driver's
    /// first pass stops the producer at the horizon.
    #[cfg(unix)]
    async fn park_stopped_at_horizon(
        serve: &VodServe,
        rendition: &Arc<Rendition>,
        permit: crate::vodencode::EncodePermit,
    ) {
        let horizon = ((f64::from(AHEAD_HORIZON_SECONDS) / rendition.seconds_per_segment).ceil()
            as u32)
            .max(1);
        {
            let mut manifest = rendition.manifest.lock().await;
            for index in 0..=horizon {
                manifest.materialize(index, 1_000, 0);
            }
        }
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake encoded producer");
        rendition
            .slot
            .attach_owned(child, 0, Some(Box::new(permit)))
            .await;
        rendition.slot.produced(horizon).await;
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(rendition.key.clone(), Arc::clone(rendition));
    }

    #[cfg(unix)]
    async fn wait_for_belief(
        rendition: &Rendition,
        expected: impl Fn(Producer) -> bool,
        description: &str,
    ) {
        for _ in 0..200 {
            if expected(rendition.slot.belief().await) {
                return;
            }
            // Tokio time is paused in the poll/TTL tests. Give the real OS
            // child reap a small wall-clock scheduling window without moving
            // the deterministic runtime clock a second time.
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(5)))
                .await
                .expect("wall-clock belief wait");
        }
        panic!(
            "timed out waiting for {description}; belief is {:?}",
            rendition.slot.belief().await
        );
    }

    #[cfg(unix)]
    async fn close_test_driver(rendition: &Rendition, driver: tokio::task::JoinHandle<()>) {
        rendition.closed.store(true, Release);
        rendition.kick();
        driver.await.expect("test driver exits");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_encoded_producer_past_the_horizon_is_stopped_not_killed() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let (_, encoding) = encoded_fixture(base.path()).await;
        let permit = encoding.try_permit().await.expect("encoder permit");
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared rendition")
            .recipe
            .encoding = Some(Arc::clone(&encoding));
        rendition.attach_reader("viewer", 0).await;
        let horizon = ((f64::from(AHEAD_HORIZON_SECONDS) / rendition.seconds_per_segment).ceil()
            as u32)
            .max(1);
        {
            let mut manifest = rendition.manifest.lock().await;
            for index in 0..=horizon {
                manifest.materialize(index, 1_000, 0);
            }
        }
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake encoded producer");
        rendition
            .slot
            .attach_owned(child, 0, Some(Box::new(permit)))
            .await;
        rendition.slot.produced(horizon).await;

        driver_pass(&serve.shared, &rendition).await;

        assert!(matches!(
            rendition.slot.belief().await,
            Producer::Stopped { .. }
        ));
        assert!(rendition.ahead_hold.load(Acquire));
        perform_driver_step(
            &serve.shared,
            &rendition,
            Step::Terminate {
                why: Termination::Idle,
            },
        )
        .await
        .expect("cleanup fake producer");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_rolling_live_start_also_releases_a_stopped_encoder() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let (serve, rendition, encoding, contender) = encoded_driver_fixture(base.path()).await;
        rendition.attach_reader("viewer", 0).await;
        tokio::time::pause();
        let driver = spawn_driver(Arc::clone(&serve.shared), Arc::clone(&rendition));
        rendition.kick();
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Stopped { .. }),
            "the real driver to stop at the ahead horizon",
        )
        .await;

        let waiting = encoding.admissions.wait_for_slot();
        assert!(encoding.admissions.live_is_waiting());
        // Wait until the real driver has armed its stopped-encoder timer
        // before moving the paused Tokio clock. There is deliberately no
        // rendition kick here: a rolling-live waiter is outside the VOD
        // registry, so the poll is the only wake source.
        RenditionTestHooks::of(&rendition).stopped_poll_armed.notified().await;
        tokio::time::advance(STOPPED_ENCODER_POLL - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            matches!(rendition.slot.belief().await, Producer::Stopped { .. }),
            "a rolling-live waiter sends no VOD registry kick"
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        RenditionTestHooks::of(&rendition).stopped_poll_fired.notified().await;
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Absent { .. }),
            "the stopped-encoder poll to yield the child",
        )
        .await;

        assert!(rendition.ahead_hold.load(Acquire));
        let contender_permit = contender
            .try_permit()
            .await
            .expect("the rolling-live contender takes the reaped child's permit");
        assert!(serve
            .shared
            .pool
            .metrics_handle()
            .prometheus()
            .contains("plurx_vod_producer_terminations_total{why=\"yield_to_waiter\"} 1"));
        drop(contender_permit);
        drop(waiting);
        close_test_driver(&rendition, driver).await;
    }

    /// The stopped-encoder poll on a rendition built by the production
    /// `build_rendition`, which installs the no-op hooks. With no observation
    /// point to wait on, the test moves the paused clock one poll at a time,
    /// and the real driver still yields its stopped child to a waiting live
    /// start.
    #[cfg(unix)]
    #[tokio::test]
    async fn rendition_shipped_shape() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let (file, encoding) = encoded_fixture(base.path()).await;
        let contender = encoding
            .clone_with_admissions_for_test(encoding.admissions.clone())
            .await;
        encoding
            .store
            .put_setting(
                plurx_core::store::keys::SW_POOL_THREADS,
                &encoding.resources.cpu_threads.max(1).to_string(),
            )
            .await
            .expect("one-encoder software budget");
        let permit = encoding.try_permit().await.expect("encoder permit");
        let index = synthetic_index(240);
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        );
        let rendition = serve
            .shared
            .build_rendition(
                "shipped-shape",
                None,
                Recipe {
                    retained_logical: None,
                    measured_candidate: None,
                    file,
                    audio_index: None,
                   aac: true,
                    audio_delivery: None,
                   video: CopyVideoOptions::new(false, false),
                    source_object_version: Some(encoding.source_object_version.clone()),
                    cluster_cache_key: None,
                    encoding: Some(Arc::clone(&encoding)),
                },
                plan,
                &settings(),
            )
            .await
            .expect("a production-built encoded rendition");
        let hooks: &dyn std::any::Any = &*rendition.hooks;
        assert!(
            hooks.is::<NoopRenditionHooks>(),
            "build_rendition installs the no-op hooks"
        );
        park_stopped_at_horizon(&serve, &rendition, permit).await;
        rendition.attach_reader("viewer", 0).await;
        tokio::time::pause();
        let driver = spawn_driver(Arc::clone(&serve.shared), Arc::clone(&rendition));
        rendition.kick();
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Stopped { .. }),
            "the real driver to stop at the ahead horizon",
        )
        .await;
        let waiting = encoding.admissions.wait_for_slot();
        assert!(encoding.admissions.live_is_waiting());
        let mut polls = 0;
        while !matches!(rendition.slot.belief().await, Producer::Absent { .. }) {
            assert!(
                polls < 4,
                "the stopped-encoder poll never yielded the child; belief is {:?}",
                rendition.slot.belief().await
            );
            polls += 1;
            tokio::time::advance(STOPPED_ENCODER_POLL).await;
            for _ in 0..40 {
                if matches!(rendition.slot.belief().await, Producer::Absent { .. }) {
                    break;
                }
                tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(5)))
                    .await
                    .expect("wall-clock belief wait");
            }
        }
        assert!(rendition.ahead_hold.load(Acquire));
        let contender_permit = contender
            .try_permit()
            .await
            .expect("the live contender takes the yielded child's permit");
        drop(contender_permit);
        drop(waiting);
        close_test_driver(&rendition, driver).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_yielded_encoder_does_not_take_the_permit_back_while_still_ahead() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let (serve, rendition, encoding, _) = encoded_driver_fixture(base.path()).await;
        rendition.attach_reader("viewer", 0).await;
        tokio::time::pause();
        let driver = spawn_driver(Arc::clone(&serve.shared), Arc::clone(&rendition));
        rendition.kick();
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Stopped { .. }),
            "the real driver to stop at the ahead horizon",
        )
        .await;
        let waiting = encoding.admissions.wait_for_slot();
        assert!(encoding.admissions.live_is_waiting());
        RenditionTestHooks::of(&rendition).stopped_poll_armed.notified().await;
        tokio::time::advance(STOPPED_ENCODER_POLL).await;
        RenditionTestHooks::of(&rendition).stopped_poll_fired.notified().await;
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Absent { .. }),
            "the real driver to yield the stopped child",
        )
        .await;
        drop(waiting);

        let through = rendition
            .slot
            .belief()
            .await
            .produced_through()
            .expect("yield preserves progress");
        let resume = ((f64::from(crate::prodsched::AHEAD_RESUME_SECONDS)
            / rendition.seconds_per_segment)
            .ceil() as u32)
            .max(1);
        let before_low_water = through.saturating_sub(resume).saturating_sub(1);
        rendition
            .readers
            .lock()
            .await
            .get_mut("viewer")
            .expect("reader")
            .frontier = before_low_water;

        // If this pass incorrectly asks for admission, the encoding's
        // admission hook holds `try_permit` at its pause and makes the defect
        // observable before a real ffmpeg can spawn.
        let admission = encoding.pause_next_admission();
        rendition.kick();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(
            !admission.was_reached(),
            "the rendition must not re-admit above the low-water line"
        );
        assert!(matches!(
            rendition.slot.belief().await,
            Producer::Absent { .. }
        ));

        rendition
            .readers
            .lock()
            .await
            .get_mut("viewer")
            .expect("reader")
            .frontier = through.saturating_sub(resume);
        rendition.kick();
        // The low-water crossing reaches admission.
        let held = admission.reached().await;

        // Close before releasing the pause: the assertion is the attempted
        // re-admission at the exact boundary, not a real encoder generation
        // beyond this lifecycle test's scope.
        rendition.closed.store(true, Release);
        held.release();
        rendition.kick();
        driver.await.expect("test driver exits");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_idle_stopped_encoder_is_reclaimed_after_the_session_ttl() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let (serve, rendition, encoding, _) = encoded_driver_fixture(base.path()).await;
        tokio::time::pause();
        insert_control_session(
            &serve,
            "idle-encoded",
            Arc::clone(&rendition),
            Instant::now() - SESSION_IDLE_TTL - Duration::from_secs(1),
        )
        .await;
        let driver = spawn_driver(Arc::clone(&serve.shared), Arc::clone(&rendition));
        rendition.kick();
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Stopped { .. }),
            "the real driver to stop at the ahead horizon",
        )
        .await;
        let budget = encoding.resources.cpu_threads.max(1);
        assert!(
            encoding
                .admissions
                .try_admit_bundle(
                    crate::admission::DEFAULT_MAX_HW_SESSIONS,
                    budget,
                    &encoding.resources,
                    crate::admission::Priority::Live,
                )
                .is_none(),
            "the stopped child retains its full permit before TTL reap"
        );

        RenditionTestHooks::of(&rendition).stopped_poll_armed.notified().await;
        // Maintenance observes the real wall-clock session TTL, detaches the
        // reader and wakes the already-armed stopped-encoder driver wait.
        serve.maintain().await;
        RenditionTestHooks::of(&rendition).stopped_poll_fired.notified().await;
        assert!(!serve
            .shared
            .sessions
            .lock()
            .await
            .contains_key("idle-encoded"));
        assert!(rendition.readers.lock().await.is_empty());
        wait_for_belief(
            &rendition,
            |belief| matches!(belief, Producer::Absent { .. }),
            "TTL detach to wake the driver and reap the stopped child",
        )
        .await;

        let reclaimed = encoding
            .admissions
            .try_admit_bundle(
                crate::admission::DEFAULT_MAX_HW_SESSIONS,
                budget,
                &encoding.resources,
                crate::admission::Priority::Live,
            )
            .expect("TTL reap releases the complete encoder permit");
        drop(reclaimed);
        assert!(serve
            .shared
            .pool
            .metrics_handle()
            .prometheus()
            .contains("plurx_vod_producer_terminations_total{why=\"idle\"} 1"));
        close_test_driver(&rendition, driver).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_rendition_arriving_takes_the_stopped_producers_permit() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let (_, encoding_a) = encoded_fixture(base.path()).await;
        let encoding_b = encoding_a
            .clone_with_admissions_for_test(encoding_a.admissions.clone())
            .await;
        encoding_a
            .store
            .put_setting(
                plurx_core::store::keys::SW_POOL_THREADS,
                &encoding_a.resources.cpu_threads.max(1).to_string(),
            )
            .await
            .expect("one-encoder software budget");
        let permit_a = encoding_a.try_permit().await.expect("first permit");
        let mut rendition_a = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition_a)
            .expect("unshared rendition")
            .recipe
            .encoding = Some(Arc::clone(&encoding_a));
        rendition_a.attach_reader("viewer-a", 0).await;
        let horizon = ((f64::from(AHEAD_HORIZON_SECONDS) / rendition_a.seconds_per_segment).ceil()
            as u32)
            .max(1);
        {
            let mut manifest = rendition_a.manifest.lock().await;
            for index in 0..=horizon {
                manifest.materialize(index, 1_000, 0);
            }
        }
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake encoded producer");
        rendition_a
            .slot
            .attach_owned(child, 0, Some(Box::new(permit_a)))
            .await;
        rendition_a.slot.produced(horizon).await;
        driver_pass(&serve.shared, &rendition_a).await;
        assert!(matches!(
            rendition_a.slot.belief().await,
            Producer::Stopped { .. }
        ));
        serve
            .shared
            .renditions
            .lock()
            .await
            .insert(rendition_a.key.clone(), Arc::clone(&rendition_a));

        let was_waiting = encoding_b.has_live_wait();
        let refused = encoding_b.try_permit().await;
        assert!(refused.is_none());
        notify_new_vod_live_wait(&serve.shared, &encoding_b, was_waiting, false);
        tokio::time::timeout(Duration::from_secs(1), rendition_a.wake.notified())
            .await
            .expect("VOD registration kicks stopped renditions");
        driver_pass(&serve.shared, &rendition_a).await;

        assert!(matches!(
            rendition_a.slot.belief().await,
            Producer::Absent { .. }
        ));
        let permit_b = encoding_b
            .try_permit()
            .await
            .expect("second rendition takes yielded permit");
        drop(permit_b);
    }

    /// One viewer, one encoder permit on the node: a predecessor encoding the
    /// rendition the viewer is watching (running, well inside its ahead
    /// window), and the prepared successor a quality change just attached for
    /// the same playback. The successor is speculative until the client has
    /// seen its first frame, so it must be able to start before commit.
    #[cfg(unix)]
    struct HandoffFixture {
        serve: Arc<VodServe>,
        predecessor: Arc<Rendition>,
        successor: Arc<Rendition>,
        successor_encoding: Arc<crate::vodencode::Encoding>,
        /// A test-held live permit that fills the software pool, when the
        /// predecessor is a hardware encode.
        _filler: Option<crate::admission::TranscodePermit>,
    }

    #[cfg(unix)]
    const STAGED_SUCCESSOR: &str = "4b1e0a52-58a4-4d3e-9d0f-0d6c5c1a7a01";

    #[cfg(unix)]
    async fn prepared_handoff_fixture(base: &Path, predecessor_hardware: bool) -> HandoffFixture {
        let serve = bare_serve(base);
        let (file, source_encoding) = encoded_fixture(base).await;
        let successor_encoding = source_encoding
            .clone_with_admissions_for_test(source_encoding.admissions.clone())
            .await;
        let mut predecessor_encoding = source_encoding
            .clone_with_admissions_for_test(source_encoding.admissions.clone())
            .await;
        if predecessor_hardware {
            Arc::get_mut(&mut predecessor_encoding)
                .expect("unshared predecessor encoding")
                .resources = crate::admission::TranscodeResourceEstimate {
                hardware_slot: true,
                cpu_threads: 0,
                decoder_threads: None,
            };
        }
        let budget = successor_encoding.resources.cpu_threads.max(1);
        source_encoding
            .store
            .put_setting(plurx_core::store::keys::SW_POOL_THREADS, &budget.to_string())
            .await
            .expect("one-encoder software budget");
        source_encoding
            .store
            .put_setting(plurx_core::store::keys::MAX_HW_SESSIONS, "1")
            .await
            .expect("one-encoder hardware budget");
        let permit = predecessor_encoding
            .try_permit()
            .await
            .expect("the predecessor owns the node's only encoder permit");
        // With a hardware predecessor the software pool is filled by an
        // unrelated, non-waiting live encode, so releasing the predecessor's
        // hardware slot would not admit the software successor.
        let filler = predecessor_hardware.then(|| {
            source_encoding
                .admissions
                .try_admit_bundle(
                    1,
                    budget,
                    &successor_encoding.resources,
                    crate::admission::Priority::Live,
                )
                .expect("unrelated live software encode")
        });
        let mut renditions = Vec::new();
        for (name, encoding) in [
            ("predecessor", Arc::clone(&predecessor_encoding)),
            ("successor", Arc::clone(&successor_encoding)),
        ] {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).expect("rendition base");
            let mut rendition = synthetic_rendition(&dir).await;
            let mutable = Arc::get_mut(&mut rendition).expect("unshared rendition");
            mutable.key = format!("{name}-rendition");
            mutable.recipe.file = file.clone();
            mutable.recipe.encoding = Some(encoding);
            serve
                .shared
                .renditions
                .lock()
                .await
                .insert(rendition.key.clone(), Arc::clone(&rendition));
            renditions.push(rendition);
        }
        let successor = renditions.pop().expect("successor");
        let predecessor = renditions.pop().expect("predecessor");
        successor_encoding.mark_speculative();
        {
            let mut manifest = predecessor.manifest.lock().await;
            for index in 0..=3 {
                manifest.materialize(index, 1_000, 0);
            }
        }
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake encoded predecessor");
        predecessor
            .slot
            .attach_owned(child, 0, Some(Box::new(permit)))
            .await;
        predecessor.slot.produced(3).await;
        insert_control_session(&serve, "old-session", Arc::clone(&predecessor), Instant::now())
            .await;
        insert_control_session(&serve, "new-session", Arc::clone(&successor), Instant::now())
            .await;
        serve
            .mark_prepared_incarnation("new-session", STAGED_SUCCESSOR)
            .await;
        driver_pass(&serve.shared, &predecessor).await;
        assert!(
            matches!(predecessor.slot.belief().await, Producer::Running { .. }),
            "a predecessor inside its ahead window is still producing"
        );
        HandoffFixture {
            serve,
            predecessor,
            successor,
            successor_encoding,
            _filler: filler,
        }
    }

    /// Stage `incarnation` in the predecessor's own preparation slot.
    #[cfg(unix)]
    async fn stage_successor(fixture: &HandoffFixture, incarnation: &str) {
        let gate = fixture
            .serve
            .preparation_gate("old-session")
            .await
            .expect("predecessor gate");
        assert!(
            gate.stage_preparation(
                incarnation.to_owned(),
                uuid::Uuid::new_v4().to_string(),
                i64::MAX,
                None,
            )
            .await
        );
    }

    #[cfg(unix)]
    fn drain_kicks(rendition: &Rendition) {
        let _ = futures_util::FutureExt::now_or_never(rendition.wake.notified());
    }

    /// Refuse the successor, then run the predecessor's pass; `true` when the
    /// predecessor gave its process up.
    #[cfg(unix)]
    async fn successor_then_predecessor(fixture: &HandoffFixture) -> bool {
        drain_kicks(&fixture.predecessor);
        driver_pass(&fixture.serve.shared, &fixture.successor).await;
        assert!(matches!(
            fixture.successor.slot.belief().await,
            Producer::Absent { .. }
        ));
        driver_pass(&fixture.serve.shared, &fixture.predecessor).await;
        for _ in 0..40 {
            if matches!(
                fixture.predecessor.slot.belief().await,
                Producer::Absent { .. }
            ) {
                return true;
            }
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(5)))
                .await
                .expect("wall-clock belief wait");
        }
        false
    }

    #[cfg(unix)]
    async fn retire_predecessor(fixture: &HandoffFixture) {
        let _ = perform_driver_step(
            &fixture.serve.shared,
            &fixture.predecessor,
            Step::Terminate {
                why: Termination::Idle,
            },
        )
        .await;
    }

    /// Production (lab6, f600d282): every quality change on a full encoder pool
    /// logged `segment_pending` for the successor's init.mp4 and then
    /// `producer_failed`, and no successor ever spawned. The successor asks as
    /// `Speculative`, which registers no waiter, and a running predecessor
    /// only ever yields to a registered live waiter — so the viewer's own
    /// predecessor sat on the only permit until its 180 s ahead window filled.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_prepared_successor_takes_its_own_viewers_running_predecessors_permit() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        stage_successor(&fixture, STAGED_SUCCESSOR).await;
        drain_kicks(&fixture.successor);

        assert!(
            successor_then_predecessor(&fixture).await,
            "the running predecessor gives its permit back"
        );
        assert!(
            fixture.successor_encoding.is_waiting(),
            "the refused successor keeps retrying from its own driver instead of \
             waiting for the next GET"
        );
        tokio::time::timeout(Duration::from_secs(1), fixture.successor.wake.notified())
            .await
            .expect("the release wakes the successor at once");
        assert!(fixture
            .serve
            .shared
            .pool
            .metrics_handle()
            .prometheus()
            .contains("plurx_vod_producer_terminations_total{why=\"yield_to_waiter\"} 1"));

        // Its viewer is still attached and still wants media, but it must not
        // take the permit back while its own successor is waiting for it.
        let predecessor_encoding = fixture
            .predecessor
            .recipe
            .encoding
            .as_ref()
            .expect("encoded predecessor");
        let admission = predecessor_encoding.pause_next_admission();
        driver_pass(&fixture.serve.shared, &fixture.predecessor).await;
        assert!(!admission.was_reached());
        // Disarm: a released pause passes its next admission straight through.
        admission.release();

        let permit = fixture
            .successor_encoding
            .try_permit()
            .await
            .expect("the prepared successor is admitted with the yielded permit");
        assert!(!fixture.successor_encoding.is_waiting());
        assert_eq!(fixture.successor_encoding.admissions.snapshot().reservations, 0);
        drop(permit);
    }

    /// Between the predecessor's reap and the successor's retry the yielded
    /// capacity is reserved: another live start cannot take it, and leaving
    /// both streams without an encoder.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_handoff_reserves_the_yielded_permit_for_its_successor() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        stage_successor(&fixture, STAGED_SUCCESSOR).await;
        assert!(successor_then_predecessor(&fixture).await);

        let intruder = fixture
            .successor_encoding
            .clone_with_admissions_for_test(fixture.successor_encoding.admissions.clone())
            .await;
        intruder.promote();
        assert!(
            intruder.try_permit().await.is_none(),
            "another live start cannot take capacity reserved for the successor"
        );
        intruder.cancel_wait();
        let permit = fixture
            .successor_encoding
            .try_permit()
            .await
            .expect("the successor claims its reservation");
        drop(permit);
    }

    /// The invariants the handoff must not bend: a speculative successor never
    /// preempts anything while another viewer is waiting for capacity, and
    /// never touches a predecessor another viewer is also reading.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_prepared_successor_never_preempts_past_a_waiting_viewer_or_a_shared_rendition() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        stage_successor(&fixture, STAGED_SUCCESSOR).await;

        let other_viewer = fixture.successor_encoding.admissions.wait_for_slot();
        assert!(
            !successor_then_predecessor(&fixture).await,
            "a waiting viewer is served first; the successor does not clear its way"
        );
        assert!(!fixture.predecessor.handoff_requested());
        assert!(!fixture.successor_encoding.is_waiting());
        drop(other_viewer);

        insert_control_session(
            &fixture.serve,
            "someone-else",
            Arc::clone(&fixture.predecessor),
            Instant::now(),
        )
        .await;
        fixture
            .serve
            .shared
            .sessions
            .lock()
            .await
            .get_mut("someone-else")
            .expect("second viewer")
            .playback_id = "another-playback".into();
        assert!(
            !successor_then_predecessor(&fixture).await,
            "a rendition another viewer is reading is never preempted for a handoff"
        );
        assert!(!fixture.predecessor.handoff_requested());
        assert!(fixture.successor_encoding.try_permit().await.is_none());
        retire_predecessor(&fixture).await;
    }

    /// No live preparation naming this successor — never staged, or aborted —
    /// means no handoff.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_successor_without_a_live_preparation_preempts_nothing() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        assert!(
            !successor_then_predecessor(&fixture).await,
            "an unstaged successor has no predecessor to take from"
        );
        assert!(!fixture.successor_encoding.is_waiting());

        stage_successor(&fixture, STAGED_SUCCESSOR).await;
        let gate = fixture
            .serve
            .preparation_gate("old-session")
            .await
            .expect("predecessor gate");
        assert!(
            gate.begin_abort_preparation_for_owner(STAGED_SUCCESSOR, 1)
                .await
        );
        assert!(
            !successor_then_predecessor(&fixture).await,
            "an aborted preparation owes its successor nothing"
        );
        assert!(!fixture.predecessor.handoff_requested());
        retire_predecessor(&fixture).await;
    }

    /// Q1 -> Q2 -> Q3: the predecessor's slot names Q3, so the stale Q2
    /// rendition cannot take the permit Q3 will need.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_prepared_successor_cannot_take_the_permit_staged_for_a_newer_one() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        stage_successor(&fixture, "9f8d6c2e-1d64-4a63-8f0f-3b3a3c5e2b03").await;
        assert!(
            !successor_then_predecessor(&fixture).await,
            "a successor the predecessor did not stage preempts nothing"
        );
        assert!(!fixture.predecessor.handoff_requested());
        assert!(!fixture.successor_encoding.is_waiting());
        retire_predecessor(&fixture).await;
    }

    /// A predecessor is never killed for a release that would not admit the
    /// successor: here it holds a hardware slot while the software successor
    /// is blocked by an unrelated software encode.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_handoff_that_would_not_admit_the_successor_preempts_nothing() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), true).await;
        stage_successor(&fixture, STAGED_SUCCESSOR).await;
        assert!(
            !successor_then_predecessor(&fixture).await,
            "releasing a hardware slot cannot admit a software successor"
        );
        assert!(!fixture.predecessor.handoff_requested());
        assert!(!fixture.successor_encoding.is_waiting());
        retire_predecessor(&fixture).await;
    }

    /// An abort ends the predecessor's parking immediately and wakes it, so its
    /// viewer is produced for without waiting on a GET retry or the expiry.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_aborted_preparation_releases_its_parked_predecessor_at_once() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        stage_successor(&fixture, STAGED_SUCCESSOR).await;
        assert!(successor_then_predecessor(&fixture).await);
        assert!(fixture.predecessor.handoff_requested());
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain_kicks(&fixture.predecessor);

        let gate = fixture
            .serve
            .preparation_gate("old-session")
            .await
            .expect("predecessor gate");
        assert!(
            gate.begin_abort_preparation_for_owner(STAGED_SUCCESSOR, 1)
                .await
        );
        assert!(!fixture.predecessor.handoff_requested());
        tokio::time::timeout(Duration::from_millis(100), fixture.predecessor.wake.notified())
            .await
            .expect("the abort wakes the parked predecessor");

        // The successor still holds its claim; release it as its driver would.
        fixture.successor_encoding.cancel_wait();
        let predecessor_encoding = fixture
            .predecessor
            .recipe
            .encoding
            .as_ref()
            .expect("encoded predecessor");
        let admission = predecessor_encoding.pause_next_admission();
        let pass = tokio::spawn({
            let shared = Arc::clone(&fixture.serve.shared);
            let predecessor = Arc::clone(&fixture.predecessor);
            async move { driver_pass(&shared, &predecessor).await }
        });
        // The released predecessor reaches admission on its next pass.
        let held = admission.reached().await;
        // Stop before a real generation: after admission the pass re-reads
        // `closed` and returns, dropping whatever permit it took.
        fixture.predecessor.closed.store(true, Release);
        held.release();
        pass.await.expect("predecessor pass");
    }

    /// A parked predecessor whose handoff simply lapses (the successor stopped
    /// asking) wakes itself instead of waiting for its viewer's next GET.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_parked_predecessor_wakes_when_its_handoff_lapses() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        let base = crate::test_tempdir().expect("base");
        let fixture = prepared_handoff_fixture(base.path(), false).await;
        stage_successor(&fixture, STAGED_SUCCESSOR).await;
        assert!(successor_then_predecessor(&fixture).await);
        // The parked pass: its viewer wants media, the handoff still stands.
        driver_pass(&fixture.serve.shared, &fixture.predecessor).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        drain_kicks(&fixture.predecessor);
        assert!(fixture.predecessor.handoff_requested());
        tokio::time::timeout(
            HANDOFF_REQUEST_TTL + Duration::from_secs(2),
            fixture.predecessor.wake.notified(),
        )
        .await
        .expect("the lapse wakes the parked predecessor");
        assert!(!fixture.predecessor.handoff_requested());
    }

    /// A successor that closes or fails stops polling for a handoff and gives
    /// back any reservation made for it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_closed_or_failed_successor_stops_waiting_for_a_handoff() {
        let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
        for failed in [false, true] {
            let base = crate::test_tempdir().expect("base");
            let fixture = prepared_handoff_fixture(base.path(), false).await;
            stage_successor(&fixture, STAGED_SUCCESSOR).await;
            assert!(successor_then_predecessor(&fixture).await);
            assert!(fixture.successor_encoding.is_waiting());
            assert_eq!(fixture.successor_encoding.admissions.snapshot().reservations, 1);
            if failed {
                *fixture.successor.failed.lock().expect("failed lock") = Some(RenditionFailure {
                    decision: crate::playback_control::ProducerDecisionReason::ProcessExit,
                    cause: "fixture failure".into(),
                });
            } else {
                fixture.successor.closed.store(true, Release);
            }
            driver_pass(&fixture.serve.shared, &fixture.successor).await;
            assert!(
                !fixture.successor_encoding.is_waiting(),
                "failed={failed}: no handoff poll outlives the successor"
            );
            assert_eq!(
                fixture.successor_encoding.admissions.snapshot().reservations,
                0,
                "failed={failed}: its reservation is returned"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capacity_hold_status_stays_none_across_a_yield() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake producer");
        rendition.slot.attach(child, 0).await;
        perform_driver_step(&serve.shared, &rendition, Step::Stop)
            .await
            .expect("ahead stop");
        perform_driver_step(
            &serve.shared,
            &rendition,
            Step::Terminate {
                why: Termination::YieldToWaiter,
            },
        )
        .await
        .expect("yield");
        assert!(rendition
            .capacity_hold
            .lock()
            .expect("capacity hold")
            .is_none());
        assert!(rendition.ahead_hold.load(Acquire));
        assert!(matches!(
            rendition.slot.belief().await,
            Producer::Absent { .. }
        ));
        let metrics = serve.shared.pool.metrics_handle().prometheus();
        assert!(
            metrics.contains("plurx_vod_producer_terminations_total{why=\"yield_to_waiter\"} 1")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_ahead_latch_follows_the_performed_step() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;

        rendition.ahead_hold.store(true, Release);
        assert!(perform_driver_step(&serve.shared, &rendition, Step::Resume)
            .await
            .is_err());
        assert!(
            rendition.ahead_hold.load(Acquire),
            "a failed signal does not move the latch"
        );

        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("fake producer");
        rendition.slot.attach(child, 0).await;
        perform_driver_step(&serve.shared, &rendition, Step::Stop)
            .await
            .expect("stop");
        assert!(rendition.ahead_hold.load(Acquire));
        perform_driver_step(&serve.shared, &rendition, Step::Resume)
            .await
            .expect("resume");
        assert!(!rendition.ahead_hold.load(Acquire));
        perform_driver_step(&serve.shared, &rendition, Step::Restart { at: 12 })
            .await
            .expect("restart termination");
        assert!(!rendition.ahead_hold.load(Acquire));
    }

    #[test]
    fn stopped_encoder_poll_fits_every_live_admission_deadline() {
        assert!(STOPPED_ENCODER_POLL < crate::admission::QUEUE_WAIT);
        assert_eq!(crate::admission::QUEUE_WAIT, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancelled_and_refused_gets_leave_no_watchdog_or_reader_frontier() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .materialize_budget = Duration::from_secs(5);
        rendition.attach_reader("viewer", 3).await;
        let mut waits = Vec::new();
        for index in 10..10 + PER_SESSION_WAIT_CAP as u32 {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            waits.push(tokio::spawn(async move {
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        index,
                        Duration::from_secs(1),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            }));
        }
        wait_until("admitted requests", Duration::from_secs(1), async || {
            serve.shared.pool.len() == PER_SESSION_WAIT_CAP
        })
        .await;
        assert!(matches!(
            serve
                .serve_segment(
                    &rendition,
                    "viewer",
                    50,
                    Duration::from_secs(1),
                    Arc::new(crate::meter::Meter::new())
                )
                .await,
            // One viewer at its own ceiling, on a node with room: the refusal
            // names the per-session cap, which is the answer that tells this
            // client to slow down rather than telling it the node is out.
            Err(VodError::Busy(crate::waitpool::WaitRefused::SessionBusy))
        ));
        assert!(!rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .contains_key(&50));
        assert_eq!(rendition.readers.lock().await["viewer"].frontier, 3);
        let watchdogs = rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .values()
            .filter_map(|clock| clock.watchdog.clone())
            .collect::<Vec<_>>();
        for wait in waits {
            wait.abort();
            let _ = wait.await;
        }
        assert!(serve.shared.pool.is_empty());
        assert!(rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .is_empty());
        wait_until(
            "cancelled watchdog tasks",
            Duration::from_secs(1),
            async || watchdogs.iter().all(tokio::task::AbortHandle::is_finished),
        )
        .await;
        assert!(
            rendition.failure().is_none(),
            "orphaned deadlines cannot poison a shared rendition"
        );
        assert!(
            matches!(
                serve
                    .serve_segment(
                        &rendition,
                        "viewer",
                        50,
                        Duration::from_millis(1),
                        Arc::new(crate::meter::Meter::new())
                    )
                    .await,
                Err(VodError::Pending { .. })
            ),
            "a later destination receives its own production budget"
        );
    }

    #[tokio::test]
    async fn a_cancelled_episode_cannot_expire_a_later_wait_for_the_same_entry() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .materialize_budget = Duration::from_secs(5);
        let old = serve.shared.arm_materialize_watchdog(&rendition, 10);
        let old_started = old.started;
        let old_task = rendition.demand_since.lock().expect("demand lock")[&10]
            .watchdog
            .as_ref()
            .expect("watchdog")
            .clone();
        drop(old);
        wait_until(
            "old watchdog cancelled",
            Duration::from_secs(1),
            async || old_task.is_finished(),
        )
        .await;
        let new = serve.shared.arm_materialize_watchdog(&rendition, 10);
        assert!(new.started > old_started);
        let request = serve
            .shared
            .pool
            .register(
                WaitKey {
                    rendition: rendition.key.clone(),
                    index: 10,
                },
                "viewer",
            )
            .expect("new request admitted");
        assert!(old_task.is_finished());
        assert_eq!(
            rendition
                .demand_since
                .lock()
                .expect("demand lock")
                .get(&10)
                .expect("new episode retained")
                .started,
            new.started
        );
        assert_eq!(
            serve.shared.pool.demands(&rendition.key).len(),
            1,
            "old watchdog cannot settle the new request"
        );
        drop((request, new));
    }

    #[tokio::test]
    async fn a_manifest_recheck_cannot_restart_the_http_block_budget() {
        // This production seam uses an absolute Tokio deadline; drive the
        // clock explicitly rather than depend on wall-clock scheduling.
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let rendition = synthetic_rendition(base.path()).await;
        tokio::time::pause();
        let manifest = rendition.manifest.lock().await;
        let get = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .blocked_wait(
                        &rendition,
                        "viewer",
                        10,
                        Duration::from_secs(5),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(serve.shared.pool.len(), 1);
        tokio::time::advance(Duration::from_secs(6)).await;
        drop(manifest);
        let result = tokio::time::timeout(Duration::from_millis(100), get)
            .await
            .expect("expired GET does not receive a fresh five-second wait")
            .expect("GET task");
        assert!(matches!(result, Err(VodError::Pending { .. })));
    }

    #[tokio::test]
    async fn cancelled_init_request_does_not_leave_a_rendition_failure() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared")
            .materialize_budget = Duration::from_secs(5);
        let pending = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_init(
                        &rendition,
                        Duration::from_secs(1),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        wait_until("init deadline owner", Duration::from_secs(1), async || {
            rendition
                .demand_since
                .lock()
                .expect("demand lock")
                .contains_key(&INIT_DEMAND_INDEX)
        })
        .await;
        let watchdog = rendition.demand_since.lock().expect("demand lock")[&INIT_DEMAND_INDEX]
            .watchdog
            .as_ref()
            .expect("init watchdog")
            .clone();
        pending.abort();
        let _ = pending.await;
        assert!(rendition
            .demand_since
            .lock()
            .expect("demand lock")
            .is_empty());
        wait_until(
            "cancelled init watchdog",
            Duration::from_secs(1),
            async || watchdog.is_finished(),
        )
        .await;
        assert!(rendition.failure().is_none());
    }

    #[tokio::test]
    async fn materialize_watchdog_spans_http_retries_and_fails_typed() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition)
            .expect("unshared rendition")
            .materialize_budget = Duration::from_millis(25);
        rendition.attach_reader("sess-a", 0).await;
        let touched = Instant::now() - Duration::from_secs(5);
        let rendition_key = rendition.key.clone();
        let file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            "sess-a".into(),
            Session {
                children: Vec::new(),
                passive_grant: None,
                rendition: Some(rendition),
                rendition_key,
                retained_output: None,
                file,
                playback_id: "play-a".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("play-a", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_millis(1),
                sdr_master_codecs: false,
                lifecycle: serve.shared.session_lifecycle("sess-a"),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(touched),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                control_observed_at: None,
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                prepared_incarnation: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );

        assert!(matches!(
            serve.segment("sess-a", "seg00001.m4s").await,
            Some(VodPublication {
                result: Err(VodError::Pending { .. }),
                ..
            })
        ));
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "a pending response does not prove playback demand",
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        match serve.segment("sess-a", "seg00001.m4s").await {
            Some(VodPublication {
                result: Err(VodError::ProducerFailed(cause)),
                ..
            }) => {
                assert!(cause.contains("producer deadline"), "{cause}");
            }
            other => panic!("watchdog must settle retries typed: {}", describe(other)),
        }
        assert_eq!(
            *serve.shared.sessions.lock().await["sess-a"]
                .last_touch
                .lock()
                .expect("touch lock"),
            touched,
            "a producer failure does not renew the attachment",
        );
    }

    /// Pure bookkeeping, faked materialization on purpose: the property under
    /// test is the driver's window construction feeding `make_room`, not the
    /// generation path (the 12 s fixture's plan is too small to leave
    /// anything outside a 180 s ahead window).
    #[tokio::test]
    async fn make_room_never_evicts_inside_a_live_readers_window() {
        let index = synthetic_index(240);
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        );
        let mut manifest = Manifest::new(plan.clone());
        let temp = crate::test_tempdir().expect("dir");
        let dir = RenditionDir::new(temp.path().join("r"));
        dir.create().await.expect("create");
        let count = manifest.len() as u32;
        assert!(count >= 40, "the synthetic plan must be big: {count}");
        for segment in 0..count {
            dir.materialize(&mut manifest, segment, b"0123456789", i64::from(segment))
                .await
                .expect("materialize");
        }

        // The reader just fetched segment 5 — exactly the window the driver
        // would build for it.
        let seconds_per_segment = plan.duration_ticks() as f64 / 16_000.0 / plan.len() as f64;
        let mut reader = Reader::new(6);
        reader.last_served = Some(5);
        let window = reader_window(&reader, seconds_per_segment);
        let freed = dir
            .make_room(&mut manifest, &[window], u64::MAX)
            .await
            .expect("make_room");
        assert!(freed.is_complete());
        assert!(freed.bytes > 0, "something outside the window was evicted");
        for segment in 0..count {
            let materialized = manifest.state(segment).expect("planned").is_materialized();
            if window.covers(segment) {
                assert!(
                    materialized,
                    "segment {segment} is inside the live reader's window"
                );
                assert!(dir.path().join(segment_name(u64::from(segment))).exists());
            }
        }
        assert!(
            manifest.state(5).expect("planned").is_materialized(),
            "the segment the reader just fetched must survive"
        );

        // A forward seek's blocked GET moves the frontier past the playhead;
        // the window's high edge must follow it, or `make_room` evicts the
        // just-materialized seek target before the waiter opens it.
        let mut seeked_reader = Reader::new(30);
        seeked_reader.last_served = Some(5);
        let seeked = reader_window(&seeked_reader, seconds_per_segment);
        assert!(
            seeked.covers(30),
            "the blocked seek target sits inside its own reader's window"
        );
    }

    /// Fix 1's derivation, as a unit: `video_ms` from the fragment index,
    /// `audio_ms` from the container — the split that makes the audio tail
    /// plannable at all. A container-for-both derivation makes the tail
    /// identically zero on every file.
    #[test]
    fn the_plan_derives_video_from_the_index_and_audio_from_the_container() {
        let index = synthetic_index(24);
        let recipe = Recipe {
            retained_logical: None,
            measured_candidate: None,
            file: media_file_at(PathBuf::from("unused.mkv"), 0),
            audio_index: None,
           aac: true,
            audio_delivery: None,
           video: CopyVideoOptions::new(false, false),
            source_object_version: None,
            cluster_cache_key: None,
            encoding: None,
        };
        let video_ms = index_video_ms(&index);
        assert!(video_ms > 0);

        // Audio outruns video by three seconds: the tail must be planned.
        let tracks = track_durations(&index, &recipe, video_ms + 3_000);
        assert_eq!(tracks.video_ms, video_ms, "video honestly from the index");
        assert_eq!(
            tracks.audio_ms,
            video_ms + 3_000,
            "audio from the container"
        );
        let plan = plurx_core::segplan::plan_copy(&index, &shipped_policy(16_000), &tracks);
        let last = plan.entries.last().expect("a planned entry");
        assert_eq!(
            last.kind,
            PlanEntryKind::AudioTail,
            "a 3 s overrun plans an audio tail"
        );

        // Tracks of equal length plan no tail.
        let flat = track_durations(&index, &recipe, video_ms);
        let plan = plurx_core::segplan::plan_copy(&index, &shipped_policy(16_000), &flat);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.kind == PlanEntryKind::Video),
            "no overrun, no tail"
        );
    }

    /// Fix 3, end to end on a real audiotail-shaped source: a seek into the
    /// audio tail positions the session (and every spawn) at the last VIDEO
    /// entry, and a blocked GET on the tail index materializes through that
    /// generation's own `finish` — never through a generation started inside
    /// the tail, which vodgen categorically refuses.
    #[tokio::test]
    async fn a_seek_into_the_audio_tail_spawns_at_the_last_video_entry_and_serves_the_tail() {
        testfixtures::require_ffmpeg();
        let temp = crate::test_tempdir().expect("temp");
        // Video 9 s, audio 12 s, no -shortest: an honest 3 s audio tail —
        // the c58a4307 shape the plan's tail entries exist for.
        let source = temp.path().join("audiotail.mkv");
        let mut cmd = std::process::Command::new(testfixtures::ffmpeg());
        cmd.args(["-y", "-v", "error"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=24:duration=9",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=12",
            ])
            .args(["-c:v", "libx265", "-preset", "ultrafast"])
            .args([
                "-x265-params",
                "keyint=42:min-keyint=42:open-gop=1:bframes=0:scenecut=0:\
                 repeat-headers=1:log-level=none",
            ])
            .args([
                "-c:a", "aac", "-pix_fmt", "yuv420p", "-ac", "2", "-f", "matroska",
            ])
            .arg(&source);
        testfixtures::run(&mut cmd);
        // The index is built against the VIDEO duration (the video-only index
        // pipe can never cover the audio's extra three seconds — its coverage
        // check reads the duration it is handed); the CREATE carries the
        // container duration, which is what the probe reports and what the
        // audio tail is derived from.
        let (store, _) = store_with_index(&media_file_at(source.clone(), 9_000)).await;
        let file = media_file_at(source, 12_000);
        let serve = local_serve(temp.path().join("renditions"), store);

        // Seek into the tail (video ends ~9 s).
        serve
            .try_create(
                &request("play-a", 10.5),
                &file,
                &settings(),
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                "sess-a".to_string(),
            )
            .await
            .expect("the audiotail source is VOD-presentable");
        let rendition = rendition_of(&serve, "sess-a").await;
        let tail = rendition
            .plan
            .entries
            .iter()
            .find(|entry| entry.kind == PlanEntryKind::AudioTail)
            .expect("the plan must carry an audio tail")
            .index;
        // The clamp itself: a start inside the tail is a video position.
        let positioned = entry_containing(&rendition.plan, 10.5);
        assert!(
            rendition.plan.entry(positioned).expect("entry").kind == PlanEntryKind::Video,
            "a seek into the tail positions at a video entry, got {positioned}"
        );
        assert!(positioned < tail);

        // The blocked GET on the tail index itself materializes and serves.
        let ready = fetch(&serve, "sess-a", &segment_name(u64::from(tail))).await;
        assert!(ready.len > 0);
        assert!(
            rendition.failure().is_none(),
            "the tail must be produced, not refused as an in-tail spawn"
        );
    }

    /// Fix 2: a resurrected rendition whose `init.mp4` is gone but whose
    /// manifest adopted every segment has no gap for the scheduler to fill —
    /// the head regeneration at attach is what puts the init back.
    #[tokio::test]
    async fn a_resurrection_missing_only_its_init_regenerates_the_head_and_keeps_the_segments() {
        let base = crate::test_tempdir().expect("base");
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        let first = local_serve(base.path().to_path_buf(), Arc::clone(&store));
        create(&first, &file, "sess-a", "play-a", &settings()).await;
        let len = plan_len(&first, "sess-a").await;
        for segment in 0..len {
            fetch(&first, "sess-a", &segment_name(segment as u64)).await;
        }
        let rendition = rendition_of(&first, "sess-a").await;
        wait_until("the first producer ended", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;
        let dir_path = rendition.dir.path().to_path_buf();
        drop(first);
        tokio::fs::remove_file(dir_path.join(INIT_NAME))
            .await
            .expect("take the init away");

        let second = local_serve(base.path().to_path_buf(), store);
        create(&second, &file, "sess-b", "play-b", &settings()).await;
        let adopted = rendition_of(&second, "sess-b").await;
        assert_eq!(
            adopted.manifest.lock().await.materialized_count(),
            len,
            "a verified head keeps every adopted segment"
        );
        assert!(
            adopted.dir.has_init().await,
            "the head regeneration re-derived init.mp4 at attach"
        );
        // And the init is servable without any producer having spawned.
        match second
            .segment("sess-b", INIT_NAME)
            .await
            .expect("ours")
            .result
        {
            Ok(Some(ready)) => assert!(ready.len > 0),
            other => panic!("init.mp4 must serve: {other:?}"),
        }
        assert!(
            matches!(adopted.slot.belief().await, Producer::Absent { .. }),
            "no generation was needed beyond the head"
        );
    }

    /// Fix 2's mismatch arm: an adopted identity the pipeline cannot
    /// reproduce purges to planned-only at attach and establishes fresh on
    /// the next real generation.
    #[tokio::test]
    async fn a_resurrection_whose_identity_cannot_be_verified_purges_and_reproduces() {
        let base = crate::test_tempdir().expect("base");
        let file = fixture_file();
        let (store, _) = store_with_index(&file).await;
        let first = local_serve(base.path().to_path_buf(), Arc::clone(&store));
        create(&first, &file, "sess-a", "play-a", &settings()).await;
        fetch(&first, "sess-a", "seg00000.m4s").await;
        let rendition = rendition_of(&first, "sess-a").await;
        let dir_path = rendition.dir.path().to_path_buf();
        wait_until("the first producer ended", Duration::from_secs(30), || {
            let rendition = Arc::clone(&rendition);
            async move { matches!(rendition.slot.belief().await, Producer::Absent { .. }) }
        })
        .await;
        drop(first);
        // A stored identity this pipeline never produced, and no init to
        // trust: the head regeneration must refuse and purge.
        let bogus = InitIdentity {
            muxer_init: "not-a-real-digest".to_string(),
            served_init: "not-a-real-digest-either".to_string(),
            promotion: plurx_core::fmp4::PromotionInputs::default(),
        };
        store_identity(&dir_path.join(IDENTITY_NAME), &bogus)
            .await
            .expect("plant the bogus identity");
        tokio::fs::remove_file(dir_path.join(INIT_NAME))
            .await
            .expect("take the init away");

        let second = local_serve(base.path().to_path_buf(), store);
        create(&second, &file, "sess-b", "play-b", &settings()).await;
        let adopted = rendition_of(&second, "sess-b").await;
        assert_eq!(
            adopted.manifest.lock().await.materialized_count(),
            0,
            "an unverifiable adoption is purged to planned-only"
        );
        // And the rendition is healthy: a fresh generation establishes a new
        // identity and serves.
        let ready = fetch(&second, "sess-b", "seg00000.m4s").await;
        assert!(ready.len > 0);
    }

    #[tokio::test]
    async fn full_output_sink_observes_only_committed_current_epoch_bytes_and_complete_tail() {
        use crate::vodgen::Sink;
        let temp = crate::test_tempdir().expect("measurement fixture");
        let serve = bare_serve(temp.path());
        let mut rendition = synthetic_rendition(temp.path()).await;
        let path = temp.path().join("source.bin");
        tokio::fs::write(&path, b"held source version").await.expect("source fixture");
        let file = media_file_at(path, 10_000);
        let source = crate::fragment_index_cluster::open_source_fence(&file, None)
            .await.expect("held source fence");
        let owned = Arc::get_mut(&mut rendition).expect("unshared fixture");
        owned.source = Some(source);
        *owned.identity.get_mut() = IdentityState {
            identity: Some(InitIdentity {
                muxer_init: "fixture-muxer".to_owned(),
                served_init: "fixture-served".to_owned(),
                promotion: Default::default(),
            }),
            from_disk: false,
        };
        let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(),
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: 0,
        };
        let stale = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(),
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: 1,
        };
        assert_eq!(stale.materialize(0, vec![7; 999]).await
            .expect_err("stale write").kind(), io::ErrorKind::NotFound);
        for entry in 0..rendition.plan.len() {
            sink.materialize(entry as u32, vec![7; 1000 + entry]).await
                .expect("successful actual directory commit");
        }
        assert!(rendition.output_measurement.lock().expect("observer")
            .complete_rates().is_none(), "entries alone are not a completed trailer");
        sink.completed_output().await;
        let rates = rendition.output_measurement.lock().expect("observer")
            .complete_rates().expect("complete full-output observation");
        assert_eq!(rates.wire_bytes, (0..rendition.plan.len()).map(|i| 1000 + i as u64).sum::<u64>());
        // A published URI is immutable: a repeated write, even with other
        // bytes, traverses the original publication rather than replacing it,
        // so the measured bytes are still exactly the served bytes.
        sink.materialize(0, vec![8; 1000]).await.expect("repeat traverses the publication");
        let served = tokio::fs::read(rendition.dir.path().join(segment_name(0))).await
            .expect("published segment");
        assert_eq!(served, vec![7; 1000], "a repeated write cannot replace published bytes");
        let repeated = rendition.output_measurement.lock().expect("observer")
            .complete_rates().expect("an unchanged publication keeps measurement authority");
        assert_eq!(repeated.identity, rates.identity);
        assert_eq!(repeated.wire_bytes, rates.wire_bytes);
    }

    #[tokio::test]
    async fn retained_complete_output_pins_exact_init_media_and_refuses_conflicting_repair() {
        use crate::vodgen::Sink;
        let temp = crate::test_tempdir().expect("retained fixture");
        let serve = bare_serve(temp.path());
        let mut rendition = synthetic_rendition(temp.path()).await;
        let source_path = temp.path().join("held-source.bin");
        tokio::fs::write(&source_path, b"original source").await.expect("source");
        let file = media_file_at(source_path, 10_000);
        let source = crate::fragment_index_cluster::open_source_fence(&file, None).await.expect("source fence");
        let init = b"original immutable init";
        let served_init = hex::encode(Sha256::digest(init));
        let owned = Arc::get_mut(&mut rendition).expect("unshared fixture");
        owned.source = Some(source);
        owned.recipe.file = file;
        *owned.identity.get_mut() = IdentityState {
            identity: Some(InitIdentity { muxer_init: served_init.clone(), served_init, promotion: Default::default() }),
            from_disk: false,
        };
        tokio::fs::write(rendition.dir.path().join(INIT_NAME), init).await.expect("init");
        serve.shared.retained_artifacts.collect(temp.path()).await;
        let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared), rendition: Arc::clone(&rendition), epoch: 0 };
        for entry in 0..rendition.plan.len() {
            sink.materialize(entry as u32, vec![7; 1000 + entry]).await.expect("actual publication");
        }
        sink.completed_output().await;
        let identity = rendition.output_measurement.lock().expect("observer").complete_rates().expect("complete").identity;
        wait_until("retained assembly", Duration::from_secs(5), || {
            let serve = Arc::clone(&serve);
            async move { serve.shared.retained_artifacts.acquire(&identity).is_some() }
        }).await;
        let artifact = serve.shared.retained_artifacts.acquire(&identity).expect("retained artifact");
        let meter = Arc::new(crate::meter::Meter::default());
        let mut init_ready = artifact.open(None, &meter, &serve.shared, &rendition, Duration::from_secs(1)).await.expect("retained init");
        let mut init_bytes = Vec::new();
        init_ready.file.read_to_end(&mut init_bytes).await.expect("read init");
        assert_eq!(init_bytes, init);
        assert!(init_ready.observed_media_duration_ms.is_none());
        let mut ready = artifact.open(Some(0), &meter, &serve.shared, &rendition, Duration::from_secs(1)).await.expect("retained segment");
        assert!(ready.retained_lease.is_some());
        assert_eq!(ready.observed_media_duration_ms, plan_media_duration_ms(&rendition, 0));
        // Rematerialization traverses the published URI instead of replacing
        // it, and already-issued open body leases name the original bytes.
        sink.materialize(0, vec![9; 1000]).await.expect("ordinary rematerialization");
        let mut original = Vec::new();
        ready.file.read_to_end(&mut original).await.expect("open body remains readable");
        assert_eq!(original, vec![7; 1000]);
        let live = rendition.dir.path().join(segment_name(0));
        assert_eq!(tokio::fs::read(&live).await.expect("live segment"), vec![7; 1000]);
        let private = temp.path().join(".retained").join(artifact.facts().artifact_id);
        tokio::fs::remove_file(private.join(segment_name(0))).await.expect("missing-object fixture");
        // Out-of-band damage (a new inode, as a rename would leave) is the
        // only way the live name can now hold different bytes.
        let replace_live = |bytes: Vec<u8>| {
            let live = live.clone();
            async move {
                let staged = live.with_extension("fixture");
                tokio::fs::write(&staged, bytes).await.expect("staged live bytes");
                tokio::fs::rename(&staged, &live).await.expect("replace live name");
            }
        };
        replace_live(vec![9; 1000]).await;
        assert!(matches!(artifact.open(Some(0), &meter, &serve.shared, &rendition, Duration::from_secs(1)).await,
            Err(VodError::ProducerFailed(_))), "different live bytes cannot repair an issued proof");
        assert!(rendition.failed.lock().expect("failure lock").is_none(), "artifact-local refusal cannot poison the rendition");
        replace_live(vec![7; 1000]).await;
        let repaired = artifact.open(Some(0), &meter, &serve.shared, &rendition, Duration::from_secs(1)).await.expect("exact repair");
        assert_eq!(repaired.len, 1000);
        assert!(serve.shared.retained_artifacts.acquire_expected(&artifact.facts(), &rendition).is_some());
        let mut wrong = artifact.facts();
        wrong.average_bps += 1;
        assert!(serve.shared.retained_artifacts.acquire_expected(&wrong, &rendition).is_none());
    }

    /// Fix 4: a stale generation's queued materialize — landing after the
    /// driver restarted the producer — is refused under the manifest lock and
    /// touches neither the manifest nor the counters.
    #[tokio::test]
    async fn a_stale_generations_write_is_refused() {
        use crate::vodgen::Sink;
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(),
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: rendition.gen_epoch.load(Relaxed),
        };
        // The driver kills and replaces the generation this sink belongs to.
        rendition.gen_epoch.fetch_add(1, Relaxed);
        let error = sink
            .materialize(0, b"stale bytes".to_vec())
            .await
            .expect_err("a stale epoch must be refused");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            !rendition
                .manifest
                .lock()
                .await
                .state(0)
                .expect("planned")
                .is_materialized(),
            "a refused write must not touch the manifest"
        );
        assert_eq!(serve.shared.working_set.load(Relaxed), 0);

        // The replacement's own sink — current epoch — writes normally.
        let fresh = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(),
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: rendition.gen_epoch.load(Relaxed),
        };
        fresh
            .materialize(0, b"fresh bytes".to_vec())
            .await
            .expect("the live generation writes");
        assert!(rendition
            .manifest
            .lock()
            .await
            .state(0)
            .expect("planned")
            .is_materialized());
    }

    /// Fix 5: replacing a failed rendition subtracts what its manifest still
    /// claimed, so the rebuild's adoption counts the same bytes exactly once.
    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_a_failed_rendition_keeps_the_working_set_honest() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;
        // Freeze the producer before it materializes anything, so the fake
        // member below is the rendition's whole claim.
        wait_until("the producer spawned", Duration::from_secs(10), || {
            let rendition = Arc::clone(&rendition);
            async move { rendition.last_child_pid.load(Relaxed) != 0 }
        })
        .await;
        let pid = rendition.last_child_pid.load(Relaxed);
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);
        assert_eq!(
            rendition.manifest.lock().await.materialized_count(),
            0,
            "frozen before its first segment"
        );
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"0123456789", now_ms())
                .await
                .expect("inject a member");
        }
        serve.shared.working_set.fetch_add(10, Relaxed);
        assert_eq!(serve.shared.working_set.load(Relaxed), 10);
        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "boom".to_string(),
        );
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);

        create(&serve, &file, "sess-b", "play-b", &settings()).await;
        let renewed = rendition_of(&serve, "sess-b").await;
        assert!(
            !Arc::ptr_eq(&rendition, &renewed),
            "a failed rendition is replaced, not reattached"
        );
        let claimed = renewed.manifest.lock().await.materialized_bytes();
        assert_eq!(
            serve.shared.working_set.load(Relaxed),
            claimed,
            "the stale rendition's bytes are subtracted exactly once"
        );
    }

    /// Fix 6: the purge re-checks dormancy while owning the exact build key,
    /// so a create that attached between maintain's scan and the purge
    /// committing keeps its rendition.
    #[tokio::test]
    async fn a_purge_never_takes_a_rendition_a_create_just_attached() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;
        let rendition = rendition_of(&serve, "sess-a").await;
        let key = rendition.key.clone();

        // The interleave: maintain's scan saw the rendition dormant (simulate
        // by planting a dormant mark), but a session attached before the
        // purge could commit. The re-check must skip it.
        *rendition.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
        tokio::time::sleep(Duration::from_millis(5)).await;
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(
            serve.shared.renditions.lock().await.contains_key(&key),
            "a rendition with a live reader survives the purge"
        );
        assert!(!rendition.closed.load(Relaxed));
        assert!(
            serve.playlist("sess-a").await.expect("ours").is_ok(),
            "the attached session still serves"
        );

        // Genuinely dormant — no readers, past TTL — the purge commits.
        assert!(serve.end("sess-a", Terminal::Deleted).await);
        tokio::time::sleep(Duration::from_millis(5)).await;
        serve.shared.purge_if_dormant(&key, Duration::ZERO).await;
        assert!(
            !serve.shared.renditions.lock().await.contains_key(&key),
            "a dormant rendition purges"
        );
        assert!(rendition.closed.load(Relaxed));
    }

    /// Fix 8: a satisfy that fired between the materialized-miss and the wait
    /// registration woke nobody — the post-registration re-check serves the
    /// bytes instead of sleeping a whole budget on them.
    #[tokio::test]
    async fn a_satisfy_that_raced_registration_is_not_a_lost_wakeup() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        // The raced state: the bytes landed (their satisfy hit an empty
        // pool), and nothing will ever satisfy this index again.
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 5, b"already here", now_ms())
                .await
                .expect("materialize");
        }
        let served = tokio::time::timeout(
            Duration::from_secs(5),
            serve.blocked_wait(
                &rendition,
                "sess-x",
                5,
                Duration::from_secs(60),
                Arc::new(crate::meter::Meter::new()),
            ),
        )
        .await
        .expect("the re-check must answer without sleeping the budget")
        .expect("the materialized segment serves");
        assert!(served.len > 0);

        // Same window, failure flavor: a failure recorded in the gap answers
        // typed instead of sleeping.
        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "boom".to_string(),
        );
        match tokio::time::timeout(
            Duration::from_secs(5),
            serve.blocked_wait(
                &rendition,
                "sess-x",
                6,
                Duration::from_secs(60),
                Arc::new(crate::meter::Meter::new()),
            ),
        )
        .await
        .expect("the re-check must answer without sleeping the budget")
        {
            Err(VodError::ProducerFailed(cause)) => assert_eq!(cause, "boom"),
            other => panic!("expected ProducerFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cached_init_read_does_not_wake_an_idle_controlled_producer() {
        let temp = crate::test_tempdir().expect("cached init");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;
        rendition.attach_reader("controlled", 22).await;
        rendition.readers.lock().await.get_mut("controlled").expect("reader").authority_only = true;
        rendition.dir.write_init(b"moov").await.expect("immutable cached init");
        let ready = serve.serve_init(&rendition, Duration::from_secs(1), Arc::new(crate::meter::Meter::new()))
            .await.expect("cached init is immediately readable");
        assert_eq!(ready.len, 4);
        assert!(rendition.demand_since.lock().expect("materialization ownership").is_empty());
        assert!(tokio::time::timeout(Duration::from_millis(1), rendition.wake.notified()).await.is_err(),
            "reading an existing init must not start work at the ordinary frontier");
    }

    /// Fix 9: both wake paths for a blocked init GET — the init landing, and
    /// a producer failure — answer promptly instead of sleeping the budget.
    #[tokio::test]
    async fn an_init_write_wakes_a_blocked_init_get() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        let waiter = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_init(
                        &rendition,
                        Duration::from_secs(30),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        rendition.dir.write_init(b"moov").await.expect("init");
        rendition.init_notify.notify_waiters();
        let ready = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the write must wake the waiter")
            .expect("waiter task")
            .expect("the init serves");
        assert!(ready.etag.contains("-init-"));
    }

    #[tokio::test]
    async fn a_failure_wakes_a_blocked_init_get() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        let waiter = {
            let serve = Arc::clone(&serve);
            let rendition = Arc::clone(&rendition);
            tokio::spawn(async move {
                serve
                    .serve_init(
                        &rendition,
                        Duration::from_secs(30),
                        Arc::new(crate::meter::Meter::new()),
                    )
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        record_failure(
            &serve.shared,
            &rendition,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "boom".to_string(),
        );
        match tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the failure must wake the waiter")
            .expect("waiter task")
        {
            Err(VodError::ProducerFailed(cause)) => assert_eq!(cause, "boom"),
            other => panic!("expected ProducerFailed, got {other:?}"),
        }
    }

    /// Fix 10: the etag folds the materialization instant in, so an
    /// evict-and-regenerate whose length happens to match still changes it.
    #[tokio::test]
    async fn the_etag_changes_across_an_evict_and_regenerate() {
        let temp = crate::test_tempdir().expect("temp");
        let serve = bare_serve(temp.path());
        let rendition = synthetic_rendition(temp.path()).await;

        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"first bytes!", 1_000)
                .await
                .expect("materialize");
        }
        let first = serve
            .open_materialized(&rendition, 0, &Arc::new(crate::meter::Meter::new()))
            .await
            .expect("open")
            .expect("materialized");
        {
            let mut manifest = rendition.manifest.lock().await;
            rendition
                .dir
                .materialize(&mut manifest, 0, b"other bytes!", 2_000)
                .await
                .expect("re-materialize");
        }
        let second = serve
            .open_materialized(&rendition, 0, &Arc::new(crate::meter::Meter::new()))
            .await
            .expect("open")
            .expect("materialized");
        assert_eq!(first.len, second.len, "the collision the instant breaks");
        assert_ne!(
            first.etag, second.etag,
            "same key, index and length must still not collide across a \
             regeneration"
        );
    }

    /// The three surfaces the integrator wires this round: the lease loop's
    /// live ids and frontier, and the stall-reopen's predecessor facts.
    #[tokio::test]
    async fn lease_and_reopen_surfaces_report_live_sessions_only() {
        let base = crate::test_tempdir().expect("base");
        let (serve, file) = serve_on(base.path()).await;
        create(&serve, &file, "sess-a", "play-a", &settings()).await;

        assert_eq!(serve.live_session_ids().await, vec!["sess-a".to_string()]);
        assert_eq!(
            serve.frontier_ms("sess-a").await,
            Some(0),
            "0 before the first served segment"
        );
        let facts = serve.reopen_facts("sess-a").await.expect("live facts");
        assert_eq!(facts.playback_id, "play-a");
        assert_eq!(facts.supersession_user, "[\"user_id\",1]");
        assert_eq!(facts.file_id, file.id);

        fetch(&serve, "sess-a", "seg00000.m4s").await;
        let rendition = rendition_of(&serve, "sess-a").await;
        let expected = ticks_to_ms(
            rendition.plan.entry(0).expect("entry 0").end_ticks(),
            rendition.timescale,
        );
        assert_eq!(
            serve.frontier_ms("sess-a").await,
            Some(expected),
            "the film-time end of the last served segment"
        );

        assert!(serve.end("sess-a", Terminal::Deleted).await);
        assert!(serve.live_session_ids().await.is_empty());
        assert_eq!(serve.frontier_ms("sess-a").await, None);
        assert!(serve.reopen_facts("sess-a").await.is_none());
        assert!(serve.owns("sess-a").await, "tombstoned is still addressed");
        assert!(serve.frontier_ms("sess-x").await.is_none());
    }

    #[tokio::test]
    async fn restarted_sink_keeps_published_bytes_before_quality_reservation() {
        use crate::vodgen::Sink;
        let base = crate::test_tempdir().expect("unreserved traversal");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = local_serve(base.path().to_path_buf(), store.clone());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition).expect("private rendition").key = "b".repeat(64);
        let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition), epoch: rendition.gen_epoch.load(Relaxed) };
        let original = b"already-delivered-before-scheduled-ack";
        sink.materialize(0, original.to_vec()).await.expect("first publication");
        assert!(store.quality_reserved_intervals(&rendition.key).await.expect("no pins").is_empty());
        let charged = serve.shared.working_set.load(Relaxed);
        let publication = rendition.publication_serial.load(Relaxed);
        rendition.gen_epoch.fetch_add(1, Relaxed);
        let restarted = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition), epoch: rendition.gen_epoch.load(Relaxed) };
        restarted.materialize(0, b"different-encoder-history-before-pin".to_vec())
            .await.expect("traverse existing publication");
        assert_eq!(tokio::fs::read(rendition.dir.path().join(segment_name(0)))
            .await.expect("original publication"), original);
        assert_eq!(serve.shared.working_set.load(Relaxed), charged);
        assert_eq!(rendition.publication_serial.load(Relaxed), publication,
            "traversal must not mint a second publication");
    }

    #[tokio::test]
    async fn restarted_sink_keeps_verified_reserved_bytes_and_refuses_corruption() {
        use crate::vodgen::Sink;
        use plurx_core::playback::continuous_quality::{QualityAttachment, QualityLedger,
            QualityInterval, QualityOperation, QualityTransitionRequest};
        let base = crate::test_tempdir().expect("reserved traversal");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let generation = uuid::Uuid::new_v4().to_string();
        // A route's session id is a capability UUID; the store refuses others.
        let session = uuid::Uuid::new_v4().to_string();
        activate_control_route(&store, &session, &generation).await;
        let serve = local_serve(base.path().to_path_buf(), store.clone());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition).expect("private rendition").key = "b".repeat(64);
        let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition), epoch: rendition.gen_epoch.load(Relaxed) };
        let original = b"original-immutable-media";
        sink.materialize(0, original.to_vec()).await.expect("first publication");
        let entry = rendition.plan.entry(0).expect("entry");
        let interval = QualityInterval { artifact_id: hex::encode(Sha256::digest(original)),
            rendition_id: rendition.key.clone(), timescale: rendition.timescale,
            from_tick: entry.start_ticks, through_tick: entry.end_ticks(), byte_length: original.len() as u64 };
        let attachment = QualityAttachment { client_instance_id: uuid::Uuid::new_v4().to_string(),
            lifetime_id: session.clone(), attachment_id: uuid::Uuid::new_v4().to_string(),
            family_id: "c".repeat(64) };
        let mut ledger = QualityLedger::new(generation.clone(), 1, attachment.clone()).expect("ledger");
        let transaction = uuid::Uuid::new_v4().to_string();
        let mut request = QualityTransitionRequest { version: 1, generation, control_epoch: 1,
            sequence: 1, attachment, transaction_id: transaction.clone(),
            operation: QualityOperation::Prepare { intent_revision: 1, target_rendition_id: rendition.key.clone() } };
        ledger.apply(&request, now_ms()).expect("prepare");
        ledger.ready(&transaction, vec![interval.clone()]).expect("ready");
        request.sequence = 2; request.operation = QualityOperation::Scheduled { intervals: vec![interval.clone()] };
        ledger.apply(&request, now_ms()).expect("scheduled");
        assert!(store.write_quality_ledger(&ledger, "node-a", 0, now_ms()).await.expect("reserve"));
        assert_eq!(store.quality_reserved_intervals(&rendition.key).await.expect("dependencies"), vec![interval]);
        let charged = serve.shared.working_set.load(Relaxed);
        let publication = rendition.publication_serial.load(Relaxed);
        rendition.gen_epoch.fetch_add(1, Relaxed);
        let restarted = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition), epoch: rendition.gen_epoch.load(Relaxed) };
        restarted.materialize(0, b"different-regenerated-media".to_vec()).await.expect("traverse cached reserved interval");
        let path = rendition.dir.path().join(segment_name(0));
        assert_eq!(tokio::fs::read(&path).await.expect("original bytes"), original);
        assert_eq!(serve.shared.working_set.load(Relaxed), charged);
        assert_eq!(rendition.publication_serial.load(Relaxed), publication, "no false publication credit");
        // The sink consults reservations only for continuous recipes, so the
        // retained-publication check is exercised directly against this pin.
        let dependencies = store.quality_reserved_intervals(&rendition.key).await.expect("dependencies");
        verify_retained_publication(&path, &rendition.plan, 0, original.len() as u64, &dependencies)
            .await.expect("exact reserved bytes");
        tokio::fs::write(&path, vec![0; original.len()]).await.expect("corrupt cached bytes");
        assert!(verify_retained_publication(&path, &rendition.plan, 0, original.len() as u64, &dependencies)
            .await.is_err(), "corruption cannot be repaired under a live reservation");
        tokio::fs::remove_file(&path).await.expect("remove cached bytes");
        assert!(verify_retained_publication(&path, &rendition.plan, 0, original.len() as u64, &dependencies)
            .await.is_err(), "a missing reserved artifact cannot be silently replaced");
    }

    #[tokio::test]
    async fn ordinary_renditions_publish_without_consulting_continuous_reservations() {
        use crate::vodgen::Sink;
        use plurx_core::playback::continuous_quality::{QualityAttachment, QualityLedger,
            QualityInterval, QualityOperation, QualityTransitionRequest};
        let base = crate::test_tempdir().expect("ordinary publication");
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let serve = local_serve(base.path().to_path_buf(), store.clone());
        let mut rendition = synthetic_rendition(base.path()).await;
        Arc::get_mut(&mut rendition).expect("private rendition").key = "b".repeat(64);
        assert!(!quality_reservations_possible(&rendition.recipe), "a copy remux carries no pins");
        // Make the reservation lookup for this key fail: two ledgers name one
        // immutable artifact with different facts.
        let entry = rendition.plan.entry(0).expect("entry");
        for byte_length in [10_u64, 11] {
            let session = uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            activate_control_route(&store, &session, &generation).await;
            let interval = QualityInterval { artifact_id: "d".repeat(64),
                rendition_id: rendition.key.clone(), timescale: rendition.timescale,
                from_tick: entry.start_ticks, through_tick: entry.end_ticks(), byte_length };
            let attachment = QualityAttachment { client_instance_id: uuid::Uuid::new_v4().to_string(),
                lifetime_id: session.clone(), attachment_id: uuid::Uuid::new_v4().to_string(),
                family_id: "c".repeat(64) };
            let mut ledger = QualityLedger::new(generation.clone(), 1, attachment.clone()).expect("ledger");
            let transaction = uuid::Uuid::new_v4().to_string();
            let mut request = QualityTransitionRequest { version: 1, generation, control_epoch: 1,
                sequence: 1, attachment, transaction_id: transaction.clone(),
                operation: QualityOperation::Prepare { intent_revision: 1, target_rendition_id: rendition.key.clone() } };
            ledger.apply(&request, now_ms()).expect("prepare");
            ledger.ready(&transaction, vec![interval.clone()]).expect("ready");
            request.sequence = 2; request.operation = QualityOperation::Scheduled { intervals: vec![interval] };
            ledger.apply(&request, now_ms()).expect("scheduled");
            assert!(store.write_quality_ledger(&ledger, "node-a", 0, now_ms()).await.expect("reserve"));
        }
        assert!(store.quality_reserved_intervals(&rendition.key).await.is_err(), "lookup is unavailable");
        let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition), epoch: rendition.gen_epoch.load(Relaxed) };
        sink.materialize(0, b"ordinary-media".to_vec()).await
            .expect("ordinary playback does not depend on the quality ledger");
        assert!(rendition.failure().is_none());
        assert_eq!(tokio::fs::read(rendition.dir.path().join(segment_name(0))).await.expect("published"),
            b"ordinary-media");
    }

    #[test]
    fn unknown_continuous_reservations_are_a_hold_not_a_write_fault() {
        let unknown = quality_reservations_unknown_error("store timed out".into());
        assert!(quality_reservations_unknown(&unknown));
        assert!(unknown.to_string().contains("store timed out"));
        for fault in [io::Error::other("disk full"), io::Error::from(io::ErrorKind::TimedOut),
            io::Error::new(io::ErrorKind::InvalidData, "regenerated media differs")] {
            assert!(!quality_reservations_unknown(&fault), "{fault}");
        }
    }

    #[test]
    fn reserved_publication_refuses_changed_bytes_before_materialization() {
        let grid = plurx_core::transcode::VodFrameGrid::new(24_000, 1_001).expect("grid");
        let plan = grid.plan(6_000, 1_000_000);
        let entry = plan.entry(1).expect("middle");
        let bytes = b"immutable-media";
        let interval = plurx_core::playback::continuous_quality::QualityInterval {
            artifact_id: hex::encode(Sha256::digest(bytes)), rendition_id: "b".repeat(64),
            timescale: plan.timescale, from_tick: entry.start_ticks,
            through_tick: entry.end_ticks(), byte_length: bytes.len() as u64,
        };
        verify_reserved_publication(&plan, 1, bytes, std::slice::from_ref(&interval))
            .expect("identical regeneration is safe");
        assert!(verify_reserved_publication(&plan, 1, b"different-media",
            std::slice::from_ref(&interval)).is_err());
        assert!(verify_reserved_publication(&plan, 1, b"short",
            std::slice::from_ref(&interval)).is_err());
        verify_reserved_publication(&plan, 0, b"unreserved-neighbor",
            std::slice::from_ref(&interval)).expect("neighbor remains independent");
        let mut wrong = interval.clone();
        wrong.timescale = 48_000;
        assert!(verify_reserved_publication(&plan, 1, bytes, &[wrong]).is_err());
        let mut partial = interval.clone();
        partial.from_tick += 1;
        assert!(verify_reserved_publication(&plan, 1, bytes, &[partial]).is_err());
        verify_reserved_publication(&plan, 1, b"new-after-disposal", &[])
            .expect("exact disposal releases the immutable dependency");
    }

    #[test]
    fn durable_dependency_windows_protect_exact_intervals_from_eviction() {
        let grid = plurx_core::transcode::VodFrameGrid::new(24_000, 1_001).expect("grid");
        let plan = grid.plan(6_000, 1_000_000);
        let entry = plan.entry(1).expect("middle");
        let interval = plurx_core::playback::continuous_quality::QualityInterval {
            artifact_id: "a".repeat(64), rendition_id: "b".repeat(64),
            timescale: plan.timescale, from_tick: entry.start_ticks,
            through_tick: entry.end_ticks(), byte_length: 100,
        };
        let windows = continuous_dependency_windows(&plan, std::slice::from_ref(&interval))
            .expect("exact dependency");
        let mut manifest = Manifest::new(plan.clone());
        for index in 0..3 { manifest.materialize(index, 100, i64::from(index)); }
        assert_eq!(manifest.eviction_candidates(&windows, u64::MAX), vec![0, 2]);
        let mut wrong_clock = interval.clone();
        wrong_clock.timescale = 48_000;
        assert!(continuous_dependency_windows(&plan, &[wrong_clock]).is_err());
        let mut partial = interval.clone();
        partial.from_tick += 1;
        assert!(continuous_dependency_windows(&plan, &[partial]).is_err());
        assert_eq!(manifest.eviction_candidates(&[], u64::MAX), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn passive_vod_idle_detaches_reader_resurrects_exact_grant_and_fences_stop() {
        let base = crate::test_tempdir().expect("passive VOD base");
        let (serve, file) = serve_on(base.path()).await;
        let mut req = request("passive-player", 0.0);
        req.presentation = crate::transcode::Presentation::Vod;
        req.request_id = Some("passive-request".to_owned());
        req.vod_only = true;
        req.passive_vod = true;
        let attribution = VodAttribution { user_name: "user", item_title: "fixture", supersession_user: "user" };
        serve.try_create(&req, &file, &settings(), attribution, "passive-session".into()).await.expect("service create");
        let (_, stale_owner) = serve.playlist("passive-session").await.expect("owned").expect("playlist");
        assert!(serve.commit_resolved_media("passive-session", &stale_owner, Some(0)).await);
        let frontier = serve.frontier_ms("passive-session").await.expect("real frontier");
        let rendition = {
            let sessions = serve.shared.sessions.lock().await;
            Arc::clone(sessions.get("passive-session").expect("passive test fixture").live_rendition().expect("passive test fixture"))
        };
        serve.force_reader_idle_for_test("passive-session").await;
        serve.maintain().await;
        assert_eq!(serve.active_sessions().await, 0);
        assert!(rendition.readers.lock().await.is_empty());
        assert!(serve.playlist("passive-session").await.is_none());
        assert_eq!(serve.frontier_ms("passive-session").await, Some(frontier));
        assert!(serve.live_or_preparing_session_ids().await.contains(&"passive-session".to_owned()));
        assert!(!serve.commit_resolved_media("passive-session", &stale_owner, Some(1)).await);
        assert!(!serve.passive_presence("passive-session", "user", "passive-player", "old-request").await);
        assert!(serve.passive_presence("passive-session", "user", "passive-player", "passive-request").await);
        assert_eq!(serve.active_sessions().await, 0, "presence cannot attach a reader");
        assert_eq!(serve.frontier_ms("passive-session").await, Some(frontier));
        let released = AtomicBool::new(false);
        let transition = Arc::new(Mutex::new(()));
        serve.try_create_before_release(&req, &file, &settings(), attribution, "passive-session".into(),
            VodReleaseFence::new(Arc::clone(&transition), &released)).await.expect("exact grant resurrection");
        assert_eq!(serve.active_sessions().await, 1);
        assert_eq!(rendition.readers.lock().await.len(), 1);
        assert!(!serve.response_owner_is_live("passive-session", &stale_owner).await);
        assert!(serve.end("passive-session", Terminal::Deleted).await);
        assert!(!serve.passive_presence("passive-session", "user", "passive-player", "passive-request").await);
        let error = serve.try_create_before_release(&req, &file, &settings(), attribution, "passive-session".into(),
            VodReleaseFence::new(transition, &released)).await.expect_err("terminal recipe cannot mint grant");
        assert_eq!(crate::transcode::vod_refusal(&error).expect("passive test fixture").0, "vod_passive_route_expired");
    }

    #[tokio::test]
    async fn passive_vod_dormant_replacement_and_expiry_retire_presence() {
        let base = crate::test_tempdir().expect("passive VOD base");
        let (serve, file) = serve_on(base.path()).await;
        let mut req = request("passive-player", 0.0);
        req.presentation = crate::transcode::Presentation::Vod;
        req.request_id = Some("passive-request".to_owned()); req.vod_only = true; req.passive_vod = true;
        let attribution = VodAttribution { user_name: "user", item_title: "fixture", supersession_user: "user" };
        serve.try_create(&req, &file, &settings(), attribution, "predecessor".into()).await.expect("create");
        serve.force_reader_idle_for_test("predecessor").await; serve.maintain().await;
        assert_eq!(serve.supersede_before("user", "passive-player", "successor", None).await.expect("replace dormant grant"), 1);
        assert!(!serve.passive_presence("predecessor", "user", "passive-player", "passive-request").await);
        assert!(!serve.live_or_preparing_session_ids().await.contains(&"predecessor".to_owned()));
        req.request_id = Some("successor-request".to_owned());
        serve.try_create(&req, &file, &settings(), attribution, "successor".into()).await.expect("successor");
        serve.force_reader_idle_for_test("successor").await; serve.maintain().await;
        serve.expire_passive_grant_for_test("successor").await;
        assert!(!serve.passive_presence("successor", "user", "passive-player", "successor-request").await);
        assert_eq!(serve.frontier_ms("successor").await, None);
        assert!(!serve.live_or_preparing_session_ids().await.contains(&"successor".to_owned()));
        serve.maintain().await;
        assert!(matches!(serve.playlist("successor").await.map(|publication| publication.result), Some(Err(VodError::Gone(Terminal::PauseExpired)))));
    }

    #[tokio::test]
    async fn passive_vod_cancelled_admission_releases_quota_before_any_reader_attachment() {
        let base = crate::test_tempdir().expect("passive cancellation base");
        let (serve, file) = serve_on(base.path()).await;
        let mut retained = Vec::new();
        for n in 0..63 {
            retained.push(serve.shared.passive_grants.reserve(
                &format!("retained-{n}"), "user", "retained-player", &format!("retained-request-{n}"), false,
            ).expect("existing grant"));
        }
        let mut req = request("cancelled-player", 0.0);
        req.presentation = crate::transcode::Presentation::Vod;
        req.vod_only = true;
        req.passive_vod = true;
        req.request_id = Some("cancelled-request".to_owned());
        // Hold the real attachment commit. Admission has to reserve its quota
        // before it reaches this gate, and cancellation must drop that exact
        // reservation even though source/rendition preparation already began.
        let sessions = serve.shared.sessions.lock().await;
        let pending = tokio::spawn({
            let serve = Arc::clone(&serve);
            let req = req.clone();
            let file = file.clone();
            async move {
                serve.try_create(&req, &file, &settings(), VodAttribution {
                    user_name: "user", item_title: "Fixture", supersession_user: "user",
                }, "pending-cancel".to_owned()).await
            }
        });
        wait_until("real create quota reservation", Duration::from_secs(2), || {
            let serve = Arc::clone(&serve);
            async move {
                matches!(serve.shared.passive_grants.reserve("overflow", "user", "player", "request", false), Err(passive_grant::Refusal::Capacity))
            }
        }).await;
        let error = serve.try_create(&req, &file, &settings(), VodAttribution {
            user_name: "user", item_title: "Fixture", supersession_user: "user",
        }, "overflow-create".to_owned()).await.expect_err("quota refusal precedes reader attachment gate");
        assert_eq!(crate::transcode::vod_refusal(&error).expect("typed quota refusal").0, "vod_passive_capacity");
        assert!(retained.iter().all(|grant| grant.live()), "admission must never evict another grant");
        pending.abort();
        assert!(pending.await.expect_err("cancelled create").is_cancelled());
        assert!(sessions.is_empty(), "cancelled creation never attached a session");
        drop(sessions);
        serve.try_create(&req, &file, &settings(), VodAttribution {
            user_name: "user", item_title: "Fixture", supersession_user: "user",
        }, "pending-cancel".to_owned()).await.expect("cancelled quota is available to the retry");
        assert_eq!(serve.active_sessions().await, 1);
        assert!(serve.end("pending-cancel", Terminal::Deleted).await);
        assert!(retained.iter().all(|grant| grant.live()));
    }

    #[tokio::test]
    async fn a05_real_vod_terminal_or_idle_removal_during_route_result_await_invalidates_observation() {
        for terminal in [true, false] {
            let base = crate::test_tempdir().expect("base");
            let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let session_id = uuid::Uuid::new_v4().to_string();
            let generation = uuid::Uuid::new_v4().to_string();
            activate_control_route(store.as_ref(), &session_id, &generation).await;
            let serve = local_serve(base.path().to_path_buf(), store.clone());
            let rendition = synthetic_rendition(base.path()).await;
            insert_control_session(&serve, &session_id, rendition, Instant::now()).await;
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(crate::playback_control::ClientPlatform::Web);
            snapshot.request_fingerprint = Some("vod-origin".into());
            let desired = snapshot.selection.desired().digest();
            let client = uuid::Uuid::new_v4().to_string();
            let accepted = serve.control(crate::playback_control::LocalControlRequest {
                session_id: &session_id, generation: &generation, owner_node_id: "node-a", owner_epoch: 1,
                client_instance_id: &client, sequence: 1, snapshot,
                prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
            }).await.expect("VOD worker").expect("accepted");
            assert_eq!(accepted.disposition, crate::playback_control::ControlDisposition::Accepted);
            let gate = serve.preparation_gate(&session_id).await.expect("real VOD gate");
            let origin = gate.accepted_observation(crate::playback_control::AcceptedControlIdentity {
                generation: generation.clone(), owner_epoch: 1, client_instance_id: client,
                sequence: 1, fingerprint: "vod-origin".into(), desired_digest: desired.clone(),
            }).await.expect("accepted origin");
            let staged = uuid::Uuid::new_v4().to_string();
            let deadline = crate::media_sessions::unix_ms() + 60_000;
            assert!(gate.stage_preparation_for_owner(staged.clone(), generation, deadline, 1, Some(desired)).await);
            let stage = gate.staged_observation_is_current(origin.clone(), staged, deadline).await.expect("actual stage token");
            let held_incarnation = Arc::clone(&serve.shared.sessions.lock().await.get(&session_id).expect("session").incarnation);
            let read_ready = tokio::sync::Notify::new();
            let release = tokio::sync::Notify::new();
            let route_result = async {
                let route = store.media_session_route(&session_id).await.expect("real route query").expect("route");
                read_ready.notify_one();
                release.notified().await;
                route
            };
            tokio::pin!(route_result);
            tokio::select! {
                () = read_ready.notified() => {},
                _ = &mut route_result => panic!("final route result must remain awaited"),
            }
            if terminal {
                assert!(serve.begin_end_detached(&session_id, Terminal::Deleted).await);
            } else {
                *serve.shared.sessions.lock().await.get(&session_id).expect("live session").last_touch.lock().expect("touch") =
                    Instant::now() - SESSION_IDLE_TTL - Duration::from_secs(1);
                serve.maintain().await;
                assert!(!serve.shared.sessions.lock().await.contains_key(&session_id));
            }
            release.notify_one();
            let old_route = route_result.await;
            assert_eq!(old_route.state, "active", "the already-read durable response still looks active");
            assert!(Arc::strong_count(&held_incarnation) >= 1, "old weak identity can still upgrade");
            assert!(!stage.still_live(), "actual VOD terminal/removal invalidates stage before claim");
            assert!(!origin.still_live(), "actual VOD attachment retirement invalidates desired origin");
            assert!(!gate.observation_is_current(origin).await);
        }
    }


    /// A retiring owner can hold either gate while joining its output writer.
    /// The writer must leave the lock queue, settle, and release real admission
    /// without requiring the owner to unlock or bypass the admission policy.
    #[cfg(unix)]
    #[tokio::test]
    async fn retirement_releases_background_admission_with_publication_locks_held() {
        use crate::vodgen::Sink;
        for hold_build in [true, false] {
            let temp = crate::test_tempdir().expect("retirement fixture");
            let serve = bare_serve(temp.path());
            let rendition = synthetic_rendition(temp.path()).await;
            let admissions = crate::admission::Admissions::new();
            let permit = admissions.try_acquire(1, crate::admission::Priority::Background)
                .expect("background slot");
            let child = tokio::process::Command::new("sleep").arg("60")
                .kill_on_drop(true).spawn().expect("real child");
            let job = crate::process_control::ChildJob::attach(&child).expect("child job");
            let (registration, writers) = rendition.slot
                .attach_registered_job_owned(child, job, 0, Some(Box::new(permit))).await;
            let sink = RenditionSink {
                shared: Arc::clone(&serve.shared), rendition: Arc::clone(&rendition),
                epoch: 0, retirement: registration.retirement(),
            };
            let gate = serve.shared.rendition_build_gate(&rendition.key);
            let build = if hold_build { Some(gate.lock().await) } else { None };
            let manifest = if hold_build { None } else { Some(rendition.manifest.lock().await) };
            let publication = sink.materialize(0, vec![7; 128]);
            tokio::pin!(publication);
            assert!(futures_util::poll!(&mut publication).is_pending(), "writer is queued behind the owner");
            rendition.slot.request_registered_retirement(&registration).await.expect("retire");
            let error = tokio::time::timeout(Duration::from_secs(2), publication).await
                .expect("retirement wakes the lock waiter").expect_err("retired publication");
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            assert!(admissions.background_is_active(), "writer still owns its settlement barrier");
            writers.settled();
            let receipt = tokio::time::timeout(Duration::from_secs(2), registration.wait_confirmed_reap())
                .await.expect("actual child and writer reap while both owner locks stay held");
            assert_eq!(receipt.writers(), crate::prodrun::WriterSettlement::Settled);
            assert!(!admissions.background_is_active());
            assert!(admissions.try_acquire(1, crate::admission::Priority::Live).is_some(),
                "the next tablet transcode can start without overriding background priority");
            drop(manifest); drop(build);
            assert!(!rendition.dir.path().join(segment_name(0)).exists(), "no retired bytes published");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_output_survives_retirement_and_manifest_contention() {
        use crate::vodgen::Sink;
        let temp = crate::test_tempdir().expect("completion fixture");
        let serve = bare_serve(temp.path());
        let mut rendition = synthetic_rendition(temp.path()).await;
        let path = temp.path().join("source.bin");
        tokio::fs::write(&path, b"held source version").await.expect("source");
        let source = crate::fragment_index_cluster::open_source_fence(&media_file_at(path, 10_000), None)
            .await.expect("held source");
        let owned = Arc::get_mut(&mut rendition).expect("unshared fixture");
        owned.source = Some(source);
        *owned.identity.get_mut() = IdentityState {
            identity: Some(InitIdentity { muxer_init: "muxer".into(), served_init: "served".into(), promotion: Default::default() }),
            from_disk: false,
        };
        let admissions = crate::admission::Admissions::new();
        let permit = admissions.try_acquire(1, crate::admission::Priority::Background).expect("background permit");
        let child = tokio::process::Command::new("sleep").arg("60").kill_on_drop(true).spawn().expect("child");
        let job = crate::process_control::ChildJob::attach(&child).expect("job");
        let (registration, writers) = rendition.slot.attach_registered_job_owned(child, job, 0, Some(Box::new(permit))).await;
        let sink = RenditionSink { shared: Arc::clone(&serve.shared), rendition: Arc::clone(&rendition), epoch: 0, retirement: registration.retirement() };
        let completion = DeferredCompletionSink::new(&sink);
        for entry in 0..rendition.plan.len() {
            completion.materialize(entry as u32, vec![7; 1000 + entry]).await.expect("committed output");
        }
        let manifest = rendition.manifest.lock().await;
        rendition.slot.request_registered_retirement(&registration).await.expect("retirement");
        tokio::time::timeout(Duration::from_secs(2), completion.completed_output()).await.expect("trailer does not wait on manifest");
        assert!(completion.completed.load(Acquire));
        writers.settled();
        tokio::time::timeout(Duration::from_secs(2), registration.wait_confirmed_reap()).await.expect("writer and child settle before completion metadata");
        assert!(!admissions.background_is_active());
        let proof = sink.completed_output();
        tokio::pin!(proof);
        assert!(futures_util::poll!(&mut proof).is_pending(), "proof waits for the temporary reader instead of disappearing");
        drop(manifest);
        tokio::time::timeout(Duration::from_secs(2), proof).await.expect("completion resumes");
        assert!(rendition.output_measurement.lock().expect("measurement").complete_rates().is_some(), "retirement did not discard a verified complete trailer");
    }
