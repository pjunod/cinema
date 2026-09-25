
    /// Creating a session spawns a process and kills its predecessor, so a
    /// replayed create is not harmless — it orphans an encoder. The
    /// idempotency key makes the retry return what the first call built.
    #[tokio::test]
    async fn a_repeated_create_returns_the_same_session() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let request = SessionRequest {
            control_sequence: None,
            file_id,
            playback_id: "pb-1".into(),
            request_id: Some("req-1".into()),
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        };
        // The idempotency identity is `intent_fingerprint`, so that is what
        // these guards have to name. Asserting against anything else lets a
        // field silently leave the real key while the test stays green.
        let shifted = SessionRequest {
            audio_offset_ms: 250,
            ..request.clone()
        };
        assert_ne!(
            shifted.intent_fingerprint("paul"),
            request.intent_fingerprint("paul"),
            "session-scoped audio sync must identify different output bytes"
        );
        // `user_name` and `playback_id` are in the key too: one `request_id`
        // reused by two viewers, or by two players on one account, must not
        // recover each other's session.
        assert_ne!(
            request.intent_fingerprint("someone-else"),
            request.intent_fingerprint("paul"),
            "one request id reused by two users must not collide"
        );
        let other_player = SessionRequest {
            playback_id: "pb-2".into(),
            ..request.clone()
        };
        assert_ne!(
            other_player.intent_fingerprint("paul"),
            request.intent_fingerprint("paul"),
            "one request id reused by two players must not collide"
        );

        let supersession_user = serde_json::json!(["username", "paul"]).to_string();
        let recovery = SessionRecoveryIdentity {
            user_id: 0,
            incarnation_id: String::new(),
            recovery_epoch: String::new(),
        };
        let first_creation = mgr
            .create_session_inner(
                &request,
                "paul",
                &supersession_user,
                &recovery,
                None,
                None,
                None,
                Priority::Live,
            )
            .await
            .expect("create");
        assert!(first_creation.created);
        let again_creation = mgr
            .create_session_inner(
                &request,
                "paul",
                &supersession_user,
                &recovery,
                None,
                None,
                None,
                Priority::Live,
            )
            .await
            .expect("replay");
        assert!(!again_creation.created);
        let first = first_creation.info;
        let again = again_creation.info;
        assert_eq!(
            first.session_id, again.session_id,
            "a replayed create must not spawn a second encoder"
        );
        assert_eq!(mgr.active_sessions().await, 1);
        assert_eq!(again.start_seconds, first.start_seconds);

        // The same key asking for something else is a mistake worth naming,
        // not a quiet second stream.
        let different = SessionRequest {
            start_seconds: 600.0,
            ..request.clone()
        };
        assert!(mgr
            .create_session(&different, "paul")
            .await
            .is_err_and(|e| e.contains("already used")));
        assert_eq!(mgr.active_sessions().await, 1);

        // A fresh key from the same player supersedes, as any restart does.
        let next = SessionRequest {
            request_id: Some("req-2".into()),
            start_seconds: 600.0,
            ..request.clone()
        };
        let moved = mgr.create_session(&next, "paul").await.expect("seek");
        assert_ne!(moved.session_id, first.session_id);
        assert_eq!(mgr.active_sessions().await, 1);

        // And once a session is gone, its key is stale rather than binding —
        // otherwise a client that reused an id would be told "conflict"
        // forever.
        assert!(mgr.stop_session(&moved.session_id, "test").await);
        let revived = mgr.create_session(&next, "paul").await.expect("recreate");
        assert_ne!(revived.session_id, moved.session_id);
        assert!(mgr.stop_session(&revived.session_id, "test").await);
    }

    /// The check-then-act race the reservation exists to close: a follower
    /// carrying the same request id while the owner is still in flight must
    /// recover the one live session the owner records. Exercise that claim
    /// transition directly so a deliberately prompt producer failure cannot
    /// turn this into the separate stale-ready-key recovery contract.
    #[tokio::test]
    async fn concurrent_creates_with_one_request_id_share_one_session() {
        use std::future::Future as _;
        use std::task::{Context, Poll, Waker};

        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let request = SessionRequest {
            control_sequence: None,
            file_id,
            playback_id: "pb-race".into(),
            request_id: Some("req-race".into()),
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        };
        let supersession_user = serde_json::json!(["username", "paul"]).to_string();

        let first = mgr
            .claim_request("req-race", &request, &supersession_user)
            .await
            .expect("first claim");
        let Claimed::Mine(claim, _) = first else {
            panic!("the first request owns the reservation")
        };

        let mut follower = Box::pin(mgr.claim_request("req-race", &request, &supersession_user));
        let mut context = Context::from_waker(Waker::noop());
        assert!(
            matches!(follower.as_mut().poll(&mut context), Poll::Pending),
            "the identical follower waits on the in-flight reservation"
        );

        let session_id = "coalesced-request-session";
        let mut session = test_session(work.path().join("coalesced-session"));
        session.file_id = file_id;
        mgr.sessions
            .lock()
            .await
            .insert(session_id.to_owned(), Arc::new(session));
        claim.complete(
            session_id,
            &std::collections::HashSet::from([session_id.to_owned()]),
        );

        let recovered = match follower.await.expect("follower claim") {
            Claimed::Recovered(info) => info,
            Claimed::Mine(_, _) => panic!("the follower must not reserve a second create"),
        };
        assert_eq!(recovered.session_id, session_id);
        assert_eq!(mgr.active_sessions().await, 1, "and exactly one exists");
        assert!(mgr.stop_session(session_id, "test").await);
    }

    /// A create that fails must clear its reservation on the way out. The
    /// second call reuses the id for a *different* stream on purpose: were the
    /// failed reservation left behind, this would be refused as a conflict —
    /// a client whose first attempt died would find its id poisoned and could
    /// never retry.
    #[tokio::test]
    async fn a_failed_create_does_not_poison_its_request_id() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let request = SessionRequest {
            control_sequence: None,
            file_id: 999_999, // nothing has this id, so the create fails
            playback_id: "pb-fail".into(),
            request_id: Some("req-fail".into()),
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        };
        assert!(mgr.create_session(&request, "paul").await.is_err());

        let retry = SessionRequest {
            file_id,
            ..request.clone()
        };
        let info = mgr
            .create_session(&retry, "paul")
            .await
            .expect("a failed attempt must not bind its request id");
        assert!(mgr.stop_session(&info.session_id, "test").await);
    }

    /// N4 server acceptance: the first stall reopen resolves one rung from the
    /// predecessor, persists that answer in the idempotency claim, and a wire
    /// replay cannot resolve again after the predecessor has been superseded.
    #[tokio::test]
    async fn transport_replay_of_a_stall_reopen_returns_one_session_and_one_target() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        store
            .put_setting(keys::SW_POOL_THREADS, "64")
            .await
            .expect("pool headroom");
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let original = SessionRequest {
            control_sequence: None,
            file_id,
            playback_id: "native-replay".into(),
            request_id: Some("native-original".into()),
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 1080 },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        };
        let previous = mgr
            .create_session(&original, "paul")
            .await
            .expect("original Auto session");

        let reopen = reopen_request(
            file_id,
            "native-replay",
            "native-stall-attempt",
            &previous.session_id,
        );
        let first = mgr
            .create_session(&reopen, "paul")
            .await
            .expect("stall reopen");
        assert_eq!(first.target_height, 720);
        assert_eq!(first.kind, SessionKind::Transcode { height: 720 });

        // Simulate a transport replay after mutable Auto inputs changed. The
        // body is still Auto, so its initial pre-claim numeric answer is not
        // allowed to create a conflict or become a second ladder step.
        let replay = SessionRequest {
            kind: SessionKind::Transcode { height: 360 },
            ..reopen
        };
        let again = mgr
            .create_session(&replay, "paul")
            .await
            .expect("transport replay");
        assert_eq!(again.session_id, first.session_id);
        assert_eq!(again.target_height, 720);
        assert_eq!(mgr.active_sessions().await, 1);
        assert!(mgr.stop_session(&first.session_id, "test").await);
    }

    /// A queued user seek may replace the playback while the stall request is
    /// being claimed. The stall's answer remains a function of its named
    /// predecessor, never whichever same-playback session is newest later.
    #[tokio::test]
    async fn a_queued_seek_racing_a_stall_does_not_step_from_the_seek() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let old_dir = crate::test_tempdir().expect("old session");
        let seek_dir = crate::test_tempdir().expect("seek session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "stalled-session",
                dir: old_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "shared-player",
                file_id: 41,
                target_height: 1080,
                automatic: true,
                kind: SessionKind::Transcode { height: 1080 },
            },
        )
        .await;
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "queued-seek-session",
                dir: seek_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "shared-player",
                file_id: 41,
                target_height: 480,
                automatic: true,
                kind: SessionKind::Transcode { height: 480 },
            },
        )
        .await;

        let request = reopen_request(41, "shared-player", "stall-vs-seek", "stalled-session");
        let Claimed::Mine(claim, normalized) = mgr
            .claim_request("stall-vs-seek", &request, r#"["username","paul"]"#)
            .await
            .expect("bound claim")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(normalized.kind, SessionKind::Transcode { height: 720 });
        assert_ne!(normalized.kind, SessionKind::Transcode { height: 360 });
        assert_eq!(
            mgr.requests
                .lock()
                .expect("requests")
                .get("stall-vs-seek")
                .and_then(|entry| entry.target_height),
            Some(720),
            "the target is stored before either session can be superseded"
        );
        drop(claim);
    }

    /// The browser that observed the cliff may have a fresher transfer sample
    /// than the server's predecessor record. It may ask to descend farther,
    /// but never use that hint to avoid the server-owned one-rung minimum.
    #[tokio::test]
    async fn a_bound_auto_reopen_honors_a_safer_client_rung() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let previous_dir = crate::test_tempdir().expect("previous session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "measured-cliff-session",
                dir: previous_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "measured-cliff-player",
                file_id: 51,
                target_height: 720,
                automatic: true,
                kind: SessionKind::Transcode { height: 720 },
            },
        )
        .await;

        let lower = SessionRequest {
            kind: SessionKind::Transcode { height: 360 },
            ..reopen_request(
                51,
                "measured-cliff-player",
                "measured-cliff-reopen",
                "measured-cliff-session",
            )
        };
        let Claimed::Mine(claim, normalized) = mgr
            .claim_request("measured-cliff-reopen", &lower, r#"["username","paul"]"#)
            .await
            .expect("lower measured rung is valid")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(normalized.kind, SessionKind::Transcode { height: 360 });
        assert_eq!(
            mgr.requests
                .lock()
                .expect("requests")
                .get("measured-cliff-reopen")
                .and_then(|entry| entry.target_height),
            Some(360),
        );
        drop(claim);

        let higher_dir = crate::test_tempdir().expect("higher previous session");
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "stale-high-session",
                dir: higher_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "stale-high-player",
                file_id: 52,
                target_height: 720,
                automatic: true,
                kind: SessionKind::Transcode { height: 720 },
            },
        )
        .await;
        let higher = reopen_request(
            52,
            "stale-high-player",
            "stale-high-reopen",
            "stale-high-session",
        );
        let Claimed::Mine(higher_claim, higher_normalized) = mgr
            .claim_request("stale-high-reopen", &higher, r#"["username","paul"]"#)
            .await
            .expect("higher hint is bounded")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(
            higher_normalized.kind,
            SessionKind::Transcode { height: 480 },
            "the client cannot avoid the server-owned one-rung descent",
        );
        drop(higher_claim);
    }

    /// A predecessor with the server-side wedge signature on it: no completed
    /// delivery for `idle_ms`, and `published_end_ms` of media the client has
    /// never fetched. The lease is the frontier authority, so the publication
    /// goes through the control actor rather than the in-memory index.
    async fn wedge_predecessor(previous: &Arc<Session>, idle_ms: i64, published_end_ms: i64) {
        previous.delivery.idle_for_test(idle_ms);
        assert!(
            previous
                .control
                .observe_publication(crate::playback_control::RollingPublicationObservation {
                    producer_attempt: 0,
                    publication_commit: true,
                    demand_sequence: None,
                    produced_segment: Some(0),
                    produced_end_ms: Some(published_end_ms),
                    playlist_ready: true,
                    published_segment: Some(0),
                    published_end_ms: Some(published_end_ms),
                    published_first_segment: Some(0),
                    published_start_ms: Some(0),
                    media_origin_ms: 0,
                    next_media_sequence: 1,
                    resolved_fetched_segment: None,
                    resolved_fetched_end_ms: None,
                })
                .await
        );
    }

    #[tokio::test]
    async fn a_wedged_predecessor_reopens_on_its_own_rung() {
        use plurx_core::store::SqliteStore;

        // The rung step is a verdict about a link that could not keep up. A
        // session that stopped completing deliveries while media it had never
        // fetched was published did not prove that — nothing was flowing to be
        // too slow — and stepping it down costs a viewer the picture they were
        // already being served, on a fault the link never had. A 2160p copy
        // must come back a 2160p copy.
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let previous_dir = crate::test_tempdir().expect("previous session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let previous = insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "wedged-session",
                dir: previous_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "wedged-player",
                file_id: 61,
                target_height: 2160,
                automatic: true,
                kind: SessionKind::Copy {
                    aac: false,
                    preserve_dolby_vision: false,
                    convert_dolby_vision: false,
                },
            },
        )
        .await;
        wedge_predecessor(&previous, WEDGE_IDLE_MS, WEDGE_GAP_MS).await;

        let request = reopen_request(61, "wedged-player", "wedged-reopen", "wedged-session");
        let Claimed::Mine(claim, normalized) = mgr
            .claim_request("wedged-reopen", &request, r#"["username","paul"]"#)
            .await
            .expect("a wedged predecessor is still a valid predecessor")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(
            normalized.kind,
            SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            "a copy that wedged comes back a copy",
        );
        assert_eq!(
            mgr.requests
                .lock()
                .expect("requests")
                .get("wedged-reopen")
                .and_then(|entry| entry.target_height),
            Some(2160),
            "on the rung the viewer was already watching",
        );
        drop(claim);

        // The same request id is the same answer: the decision is made before
        // the create is persisted, so a transport replay cannot see a
        // different rung than the one already handed out.
        let Claimed::Mine(replay_claim, replayed) = mgr
            .claim_request("wedged-reopen", &request, r#"["username","paul"]"#)
            .await
            .expect("replay is valid")
        else {
            panic!("an unfinished claim is replayable by its owner")
        };
        assert_eq!(replayed.kind, normalized.kind);
        drop(replay_claim);
    }

    #[tokio::test]
    async fn a_slow_link_still_steps_down_because_it_is_not_wedged() {
        use plurx_core::store::SqliteStore;

        // Both halves of the signature are load-bearing. A link that is merely
        // slow is still completing deliveries, and a session with nothing
        // published beyond what the client already has is not being starved by
        // a wedge. Either way the one-rung descent is the right answer, and a
        // backstop that swallowed those would leave real starvation at a rung
        // the link has already failed.
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        for (name, idle_ms, published_end_ms, why) in [
            (
                "still-delivering",
                WEDGE_IDLE_MS / 2,
                WEDGE_GAP_MS,
                "a delivery completed a moment ago is a slow link, not a wedge",
            ),
            (
                "nothing-waiting",
                WEDGE_IDLE_MS,
                WEDGE_GAP_MS - 1,
                "an idle client with nothing published to fetch proves nothing",
            ),
        ] {
            let dir = crate::test_tempdir().expect("previous session");
            let previous = insert_reopen_fixture(
                &mgr,
                ReopenFixture {
                    session_id: name,
                    dir: dir.path().to_path_buf(),
                    user_name: "paul",
                    playback_id: name,
                    file_id: 62,
                    target_height: 1080,
                    automatic: true,
                    kind: SessionKind::Transcode { height: 1080 },
                },
            )
            .await;
            wedge_predecessor(&previous, idle_ms, published_end_ms).await;

            let request = reopen_request(62, name, name, name);
            let Claimed::Mine(claim, normalized) = mgr
                .claim_request(name, &request, r#"["username","paul"]"#)
                .await
                .expect("valid reopen")
            else {
                panic!("new request owns its claim")
            };
            assert_eq!(
                normalized.kind,
                SessionKind::Transcode { height: 720 },
                "{why}",
            );
            drop(claim);
        }
    }

    #[test]
    fn delivery_wedge_thresholds_are_exact() {
        assert!(!delivery_wedge(WEDGE_IDLE_MS - 1, Some(WEDGE_GAP_MS), 0));
        assert!(!delivery_wedge(WEDGE_IDLE_MS, Some(WEDGE_GAP_MS - 1), 0));
        assert!(!delivery_wedge(WEDGE_IDLE_MS, None, 0));
        assert!(delivery_wedge(WEDGE_IDLE_MS, Some(WEDGE_GAP_MS), 0));
    }

    #[tokio::test]
    async fn a_manual_reopen_is_unchanged_by_the_wedge_backstop() {
        use plurx_core::store::SqliteStore;

        // A viewer who picked a rung owns it. The backstop only removes a
        // server-owned descent, and there is none to remove here.
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        for (name, idle_ms, published_end_ms) in [
            ("manual-wedged", WEDGE_IDLE_MS, WEDGE_GAP_MS),
            ("manual-healthy", 0, 0),
        ] {
            let dir = crate::test_tempdir().expect("previous session");
            let previous = insert_reopen_fixture(
                &mgr,
                ReopenFixture {
                    session_id: name,
                    dir: dir.path().to_path_buf(),
                    user_name: "paul",
                    playback_id: name,
                    file_id: 63,
                    target_height: 1080,
                    automatic: false,
                    kind: SessionKind::Transcode { height: 1080 },
                },
            )
            .await;
            wedge_predecessor(&previous, idle_ms, published_end_ms).await;

            let request = reopen_request(63, name, name, name);
            let Claimed::Mine(claim, normalized) = mgr
                .claim_request(name, &request, r#"["username","paul"]"#)
                .await
                .expect("valid reopen")
            else {
                panic!("new request owns its claim")
            };
            assert_eq!(
                normalized.kind,
                SessionKind::Transcode { height: 1080 },
                "a manual predecessor keeps its own rung either way",
            );
            drop(claim);
        }
    }

    #[tokio::test]
    async fn clustered_stall_reopen_survives_username_rename() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let previous_dir = crate::test_tempdir().expect("previous session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let immutable_scope = serde_json::json!(["user_id", 42]).to_string();
        let mut previous = test_session(previous_dir.path().to_path_buf());
        previous.user_name = "name-before-rename".into();
        previous.supersession_user = immutable_scope.clone();
        previous.playback_id = "renamed-player".into();
        previous.file_id = 42;
        previous.target_height = 1080;
        previous.automatic = true;
        previous.kind = SessionKind::Transcode { height: 1080 };
        mgr.sessions
            .lock()
            .await
            .insert("renamed-session".into(), Arc::new(previous));

        let request = reopen_request(42, "renamed-player", "renamed-reopen", "renamed-session");
        let Claimed::Mine(claim, normalized) = mgr
            .claim_request("renamed-reopen", &request, &immutable_scope)
            .await
            .expect("immutable user id still owns the renamed session")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(normalized.kind, SessionKind::Transcode { height: 720 });
        drop(claim);

        let foreign = SessionRequest {
            request_id: Some("renamed-foreign".into()),
            ..request
        };
        assert!(mgr
            .claim_request(
                "renamed-foreign",
                &foreign,
                &serde_json::json!(["user_id", 43]).to_string(),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn clustered_replacement_gate_refuses_a_wait_past_its_deadline() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let key = r#"[["user_id",42],"deadline-player"]"#.to_owned();
        let held = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("first replacement owns its gate");
        let error = match mgr
            .acquire_cluster_replacement_gate(key.clone(), None, tokio::time::Instant::now())
            .await
        {
            Ok(_) => panic!("a timed-out replacement must never begin provisional creation"),
            Err(error) => error,
        };
        assert!(is_retryable_capacity_error(&error), "{error}");

        drop(held);
        let reacquired = mgr
            .acquire_cluster_replacement_gate(
                key,
                None,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("a live retry acquires the released gate");
        drop(reacquired);
    }

    fn replacement_gate_fixture() -> (TranscodeManager, tempfile::TempDir) {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        (mgr, work)
    }

    /// A replacement that is abandoned — its request long since answered, its
    /// guard living on inside a detached cleanup nothing can reach — used to
    /// own its player's key for the life of the process. Every later open then
    /// waited the cooperative window and was refused, so a viewer could not
    /// restart the title they had been watching.
    ///
    /// Reproduced from m6, 2026-09-21 22:55 UTC, file 5208: a start blocked
    /// under the gate on a 402-second subtitle sidecar extraction, answered
    /// 503 at its own deadline, and its cleanup still held the key four
    /// seconds later when the viewer pressed Retry.
    #[tokio::test(start_paused = true)]
    async fn an_abandoned_replacement_cannot_hold_its_player_against_the_next_open() {
        let (mgr, _work) = replacement_gate_fixture();
        let key = r#"[["user_id",42],"abandoned-player"]"#.to_owned();

        let cleanup = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(600),
            )
            .await
            .expect("the first replacement owns its gate");
        // What `Drop for StartedSessionGuard` does before it spawns: name the
        // session this teardown is tearing down, then say the hold is no
        // longer a start anyone is waiting on.
        cleanup.publish_fenceable("abandoned-provisional-session");
        cleanup.mark_abandoned();

        let opened = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await
            .expect("a later open reclaims the player's key from an abandoned hold");

        // The key really moved rather than being shared: a third open must now
        // serialize behind the OPEN replacement, and must NOT be able to
        // reclaim it, because that hold is neither abandoned nor past its
        // ceiling.
        match mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await
        {
            Ok(_) => panic!("a healthy replacement must keep its player"),
            Err(error) => assert!(
                is_replacement_wait_error(&error),
                "the refusal a client can wait out, got: {error}"
            ),
        }

        // And the reclaimed hold letting go afterwards must not hand out the
        // live owner's key.
        drop(cleanup);
        assert!(
            mgr.acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await
            .is_err(),
            "the reclaimed hold's release must not reopen the live owner's key"
        );

        drop(opened);
        let reacquired = mgr
            .acquire_cluster_replacement_gate(
                key,
                None,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("a released gate is acquirable again");
        drop(reacquired);
    }

    /// The property the first version of this fix broke, and the reason it is
    /// pinned on its own: the work under this gate routinely takes tens of
    /// seconds — the incident had two 50-second starts — and the client's own
    /// retry ladder re-posts into it. A waiter must never be able to destroy a
    /// start that is simply still building.
    #[tokio::test(start_paused = true)]
    async fn a_healthy_replacement_is_never_reclaimed_by_a_waiter() {
        let (mgr, _work) = replacement_gate_fixture();
        let key = r#"[["user_id",42],"slow-but-healthy-player"]"#.to_owned();
        let building = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(600),
            )
            .await
            .expect("the start owns its gate");
        building.publish_fenceable("healthy-session");

        // Three arrivals, the shape of the 1 s / 2 s / 4 s ladder, each waiting
        // its full cooperative window and each refused rather than served.
        for attempt in 0..3 {
            match mgr
                .acquire_cluster_replacement_gate(
                    key.clone(),
                    None,
                    tokio::time::Instant::now() + Duration::from_secs(60),
                )
                .await
            {
                Ok(_) => panic!("attempt {attempt} reclaimed a healthy start's player"),
                Err(error) => assert!(
                    is_replacement_wait_error(&error),
                    "attempt {attempt}: {error}"
                ),
            }
        }
        drop(building);
    }

    /// A hold that has outlived every budget it declared is wedged by
    /// definition, and the next open takes its player back.
    #[tokio::test(start_paused = true)]
    async fn a_hold_past_its_ceiling_loses_its_player() {
        let (mgr, _work) = replacement_gate_fixture();
        let key = r#"[["user_id",42],"wedged-player"]"#.to_owned();
        let wedged = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(6_000),
            )
            .await
            .expect("the wedged start owns its gate");

        tokio::time::advance(CLUSTER_REPLACEMENT_HOLD_CEILING + Duration::from_secs(1)).await;
        let opened = mgr
            .acquire_cluster_replacement_gate(
                key,
                None,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await
            .expect("a hold past its ceiling is reclaimable");
        drop(opened);
        drop(wedged);
    }

    /// The refusal a viewer waits out has to stay inside the narrow class the
    /// clients retry. Widening it to the whole capacity class would put a
    /// refusal that tells the client to ask for a smaller height, and one that
    /// reports a misconfigured scratch ceiling, on a ladder neither can win.
    #[tokio::test(start_paused = true)]
    async fn the_replacement_wait_is_the_only_capacity_refusal_a_client_retries() {
        let (mgr, _work) = replacement_gate_fixture();
        let key = r#"[["user_id",42],"class-player"]"#.to_owned();
        let held = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(600),
            )
            .await
            .expect("the first replacement owns its gate");
        let error = mgr
            .acquire_cluster_replacement_gate(
                key,
                None,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await
            .err()
            .expect("a healthy hold refuses");
        assert!(is_replacement_wait_error(&error), "{error}");
        assert!(
            is_retryable_capacity_error(&error),
            "the wait class stays a subset of the capacity class, so every \
             existing server-side consumer keeps seeing it: {error}"
        );
        assert!(
            !is_replacement_wait_error(&capacity_error(
                "the proved 4K HDR10 QuickSync slot is busy; retry at 1080p"
            )),
            "a refusal that asks the client to change the request is not a wait"
        );
        drop(held);
    }

    /// A start with no budget left is out of time, not behind a wedged hold.
    #[tokio::test]
    async fn a_replacement_out_of_budget_refuses_instead_of_taking_the_key() {
        let (mgr, _work) = replacement_gate_fixture();
        let key = r#"[["user_id",42],"out-of-budget-player"]"#.to_owned();
        let held = mgr
            .acquire_cluster_replacement_gate(
                key.clone(),
                None,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .await
            .expect("the first replacement owns its gate");
        let error = match mgr
            .acquire_cluster_replacement_gate(key, None, tokio::time::Instant::now())
            .await
        {
            Ok(_) => panic!("an expired start must not evict a live replacement"),
            Err(error) => error,
        };
        assert!(is_retryable_capacity_error(&error), "{error}");
        drop(held);
    }

    /// The cluster replacement guard already spans provisional worker creation
    /// through the ingress activation verdict. A typed reopen must bind its
    /// predecessor to that same lifetime so the lease loop cannot terminalize
    /// it while the successor has not yet won the activation CAS.
    #[tokio::test]
    async fn clustered_replacement_gate_protects_its_exact_predecessor_route() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let predecessor = "replacement-activation-predecessor";
        let guard = mgr
            .acquire_cluster_replacement_gate(
                "replacement-activation-player".to_owned(),
                Some(predecessor),
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("replacement owns its gate");
        assert!(
            crate::media_sessions::settlement_protected_ids().contains(predecessor),
            "the predecessor is protected throughout provisional successor creation"
        );

        drop(guard);
        assert!(
            !crate::media_sessions::settlement_protected_ids().contains(predecessor),
            "a definitive activation or abort releases stale-settlement protection"
        );
    }

    #[tokio::test]
    async fn legacy_supersession_rechecks_deadline_before_predecessor_reap() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let predecessor_dir = crate::test_tempdir().expect("predecessor");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let immutable_scope = serde_json::json!(["user_id", 42]).to_string();
        let mut predecessor = test_session(predecessor_dir.path().to_path_buf());
        predecessor.supersession_user = immutable_scope.clone();
        predecessor.playback_id = "deadline-player".into();
        let predecessor = Arc::new(predecessor);
        mgr.sessions
            .lock()
            .await
            .insert("still-playable".into(), Arc::clone(&predecessor));

        let sessions_guard = mgr.sessions.lock().await;
        let sessions_error = mgr
            .reap_superseded_before(
                Some(tokio::time::Instant::now() + Duration::from_millis(20)),
                &immutable_scope,
                "deadline-player",
            )
            .await
            .expect_err("session-map contention must not outlive the replacement deadline");
        assert!(
            is_retryable_capacity_error(&sessions_error),
            "{sessions_error}"
        );
        drop(sessions_guard);

        let transition_guard = predecessor.child_transition.lock().await;
        let transition_error = mgr
            .reap_superseded_before(
                Some(tokio::time::Instant::now() + Duration::from_millis(20)),
                &immutable_scope,
                "deadline-player",
            )
            .await
            .expect_err("child-transition contention must not outlive the replacement deadline");
        assert!(
            is_retryable_capacity_error(&transition_error),
            "{transition_error}"
        );
        drop(transition_guard);

        assert!(
            mgr.sessions.lock().await.contains_key("still-playable"),
            "both expired lock acquisitions must leave the predecessor live"
        );
    }

    /// Track intent is orthogonal to height normalization. A stall claim keeps
    /// the chosen audio/subtitle fields, while a later user track action that
    /// omits the stall cause follows the ordinary (unstepped) create path.
    #[tokio::test]
    async fn an_audio_or_subtitle_change_during_a_stall_keeps_its_own_request() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let previous_dir = crate::test_tempdir().expect("previous session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "track-stall",
                dir: previous_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "track-player",
                file_id: 52,
                target_height: 1080,
                automatic: true,
                kind: SessionKind::Transcode { height: 1080 },
            },
        )
        .await;

        let request = SessionRequest {
            audio_index: Some(2),
            subtitle_burn: Some(5),
            ..reopen_request(52, "track-player", "stall-track", "track-stall")
        };
        let Claimed::Mine(stall_claim, normalized) = mgr
            .claim_request("stall-track", &request, r#"["username","paul"]"#)
            .await
            .expect("stall claim")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(normalized.kind, SessionKind::Transcode { height: 720 });
        assert_eq!(normalized.audio_index, Some(2));
        assert_eq!(normalized.subtitle_burn, Some(5));
        drop(stall_claim);

        let track_change = SessionRequest {
            request_id: Some("user-track-change".into()),
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 1080 },
            ..request
        };
        let Claimed::Mine(track_claim, ordinary) = mgr
            .claim_request("user-track-change", &track_change, r#"["username","paul"]"#)
            .await
            .expect("ordinary track claim")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(ordinary.kind, SessionKind::Transcode { height: 1080 });
        assert_eq!(ordinary.audio_index, Some(2));
        assert_eq!(ordinary.subtitle_burn, Some(5));
        drop(track_claim);
    }

    /// The ladder floor is a same-rung result, not a fifth hidden rung and not
    /// a new server terminal error. The server repeats the rung every time and
    /// bounds nothing: the client's own recovery budget owns the retry count
    /// and the eventual visible failure. Nothing here asserts "once", because
    /// no code in this repo implements a "once" — the reopens below prove the
    /// repeat is unconditional.
    #[tokio::test]
    async fn a_stall_at_the_floor_repeats_the_same_rung() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let previous_dir = crate::test_tempdir().expect("previous session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "floor-session",
                dir: previous_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "floor-player",
                file_id: 63,
                target_height: 360,
                automatic: true,
                kind: SessionKind::Transcode { height: 360 },
            },
        )
        .await;
        // Three separate attempts, each with its own request id: the server
        // answers 360 every time and never produces a terminal refusal of its
        // own. That is the whole server half of "one retry, then the existing
        // terminal surface" — the count lives in the client.
        for attempt in ["floor-retry-1", "floor-retry-2", "floor-retry-3"] {
            let request = reopen_request(63, "floor-player", attempt, "floor-session");
            let Claimed::Mine(claim, normalized) = mgr
                .claim_request(attempt, &request, r#"["username","paul"]"#)
                .await
                .expect("floor claim")
            else {
                panic!("new request owns its claim")
            };
            assert_eq!(normalized.kind, SessionKind::Transcode { height: 360 });
            assert_eq!(
                mgr.requests
                    .lock()
                    .expect("requests")
                    .get(attempt)
                    .and_then(|entry| entry.target_height),
                Some(360)
            );
            drop(claim);
        }
    }

    /// A very small source or an unprobed predecessor can resolve below the
    /// advertised ladder floor. The way down must never hand such a session a
    /// HIGHER rung. Reverting `one_rung_below`'s `unwrap_or(current)` to
    /// `unwrap_or(LADDER_HEIGHTS[0])` fails these cases.
    #[tokio::test]
    async fn a_stall_below_the_ladder_floor_never_steps_up() {
        use plurx_core::store::SqliteStore;

        // The unit the defect lived in, across the whole sub-floor range.
        assert_eq!(one_rung_below(MIN_HEIGHT), MIN_HEIGHT);
        assert_eq!(one_rung_below(240), MIN_HEIGHT);
        assert_eq!(one_rung_below(288), 240);
        assert_eq!(one_rung_below(360), 240);
        assert_eq!(one_rung_below(480), 360, "an on-ladder step still steps");
        assert_eq!(
            one_rung_below(2160),
            1080,
            "a 4K Auto stall enters the established lower ladder"
        );
        // A remux of an unprobed source records target_height 0; normalizing
        // that must not produce a zero-height transcode.
        assert_eq!(one_rung_below(0), MIN_HEIGHT);

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );

        // 144 is where a starved-rung prior puts a client; 240 is a 240p
        // source's honest Auto answer. Neither may normalize upwards, and
        // a stalled 240p session can now step down to the new 144p rung.
        for (index, height) in [MIN_HEIGHT, 240].into_iter().enumerate() {
            let session_id = format!("sub-floor-{height}");
            let playback_id = format!("sub-floor-player-{height}");
            let dir = crate::test_tempdir().expect("previous session");
            insert_reopen_fixture(
                &mgr,
                ReopenFixture {
                    session_id: &session_id,
                    dir: dir.path().to_path_buf(),
                    user_name: "paul",
                    playback_id: &playback_id,
                    file_id: 64 + index as i64,
                    target_height: height,
                    automatic: true,
                    kind: SessionKind::Transcode { height },
                },
            )
            .await;
            let attempt = format!("sub-floor-retry-{height}");
            let request = reopen_request(64 + index as i64, &playback_id, &attempt, &session_id);
            let Claimed::Mine(claim, normalized) = mgr
                .claim_request(&attempt, &request, r#"["username","paul"]"#)
                .await
                .expect("sub-floor claim")
            else {
                panic!("new request owns its claim")
            };
            let expected = one_rung_below(height);
            assert_eq!(
                normalized.kind,
                SessionKind::Transcode { height: expected },
                "a {height}p Auto stall reopen must step down when possible and never step up"
            );
            assert_eq!(
                mgr.requests
                    .lock()
                    .expect("requests")
                    .get(&attempt)
                    .and_then(|entry| entry.target_height),
                Some(expected),
                "the persisted target must match the answer the client gets"
            );
            drop(claim);
        }
    }

    /// `playback_id` is a supersession key, not sufficient identity for a
    /// recovery. The exact session id plus user and file decide which device's
    /// rung is eligible even if two clients accidentally reuse that key.
    #[tokio::test]
    async fn two_devices_sharing_a_playback_id_bind_reopens_to_the_named_session() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let a_dir = crate::test_tempdir().expect("device a");
        let b_dir = crate::test_tempdir().expect("device b");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "device-a-session",
                dir: a_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "duplicated-player-id",
                file_id: 74,
                target_height: 1080,
                automatic: true,
                kind: SessionKind::Transcode { height: 1080 },
            },
        )
        .await;
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "device-b-session",
                dir: b_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "duplicated-player-id",
                file_id: 74,
                target_height: 480,
                automatic: true,
                kind: SessionKind::Transcode { height: 480 },
            },
        )
        .await;

        let request = reopen_request(
            74,
            "duplicated-player-id",
            "device-a-reopen",
            "device-a-session",
        );
        let Claimed::Mine(claim, normalized) = mgr
            .claim_request("device-a-reopen", &request, r#"["username","paul"]"#)
            .await
            .expect("device A claim")
        else {
            panic!("new request owns its claim")
        };
        assert_eq!(normalized.kind, SessionKind::Transcode { height: 720 });
        drop(claim);

        let device_b = SessionRequest {
            request_id: Some("device-b-reopen".into()),
            previous_session_id: Some("device-b-session".into()),
            ..request.clone()
        };
        let Claimed::Mine(device_b_claim, device_b_normalized) = mgr
            .claim_request("device-b-reopen", &device_b, r#"["username","paul"]"#)
            .await
            .expect("device B claim")
        else {
            panic!("the other device owns its claim")
        };
        assert_eq!(
            device_b_normalized.kind,
            SessionKind::Transcode { height: 360 },
            "the exact predecessor, not the shared playback id, chooses the rung"
        );
        drop(device_b_claim);

        let foreign_user = SessionRequest {
            request_id: Some("foreign-user-reopen".into()),
            ..request
        };
        assert!(mgr
            .claim_request(
                "foreign-user-reopen",
                &foreign_user,
                r#"["username","other-user"]"#,
            )
            .await
            .is_err_and(|error| error.contains("does not belong")));
    }

    /// The previous session's normalized mode is authoritative. A client that
    /// accidentally labels a manual-height retry as Auto still receives the
    /// viewer's chosen rung rather than one below it.
    #[tokio::test]
    async fn a_manual_height_session_is_not_stepped() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let previous_dir = crate::test_tempdir().expect("previous session");
        let mgr = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        insert_reopen_fixture(
            &mgr,
            ReopenFixture {
                session_id: "manual-session",
                dir: previous_dir.path().to_path_buf(),
                user_name: "paul",
                playback_id: "manual-player",
                file_id: 85,
                target_height: 720,
                automatic: false,
                kind: SessionKind::Transcode { height: 720 },
            },
        )
        .await;
        let request = reopen_request(85, "manual-player", "manual-retry", "manual-session");
        let Claimed::Mine(claim, normalized) = mgr
            .claim_request("manual-retry", &request, r#"["username","paul"]"#)
            .await
            .expect("manual claim")
        else {
            panic!("new request owns its claim")
        };
        assert!(!normalized.automatic);
        assert_eq!(normalized.kind, SessionKind::Transcode { height: 720 });
        assert_eq!(
            mgr.requests
                .lock()
                .expect("requests")
                .get("manual-retry")
                .and_then(|entry| entry.target_height),
            Some(720)
        );
        drop(claim);
    }

    /// Which decoder the command will *name* is the only thing that may be
    /// matched to a contract.
    ///
    /// A grammar built on a guessed decoder name matches nothing, and a
    /// grammar that matches nothing certifies every stream as clean — the
    /// exact substitution this milestone exists to remove. So the answer is no
    /// unless the plan says the decoder out loud.
    #[test]
    fn only_a_plan_that_names_its_decoder_gets_a_grammar() {
        use plurx_core::transcode::DecodeBackend;

        let contract = crate::decoder_health::DiagnosticContract {
            id: "fixture".to_owned(),
            host: "test".to_owned(),
            ffmpeg_version: "8.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
            stderr_mode: crate::decoder_health::QUALIFIED_STDERR_MODE.to_owned(),
            input_codec: "h264".to_owned(),
            decoder: "h264".to_owned(),
            decode_backend: plurx_core::transcode::DecodeBackend::Software
                .name()
                .to_owned(),
            require_context_addresses: true,
            primary_message: "Error submitting packet to decoder:".to_owned(),
            subordinate_message: Some("No frame decoded?".to_owned()),
            attributes_every_failure: true,
            error_detail: "corrupt input packet".to_owned(),
            fixture: "f".to_owned(),
            fixture_sha256: "d".repeat(64),
            scope: "test".to_owned(),
            backend_fault_detail: None,
        };
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let resolve = |backend, decoder| {
            DiagnosticObservation::resolve(
                &policy,
                "digest".to_owned(),
                Some("h264"),
                backend,
                decoder,
                0,
                false,
            )
        };

        let named = resolve(DecodeBackend::Software, Some("h264"));
        assert!(
            named.grammar.is_some(),
            "the command names h264, and h264 is qualified"
        );
        assert!(named.qualified_logging());
        assert_eq!(named.contract_id.as_deref(), Some("fixture"));

        for (label, observation) in [
            // The daemon's startup inventory names no implementation, so this
            // is every software plan in production today: the command emits no
            // `-c:v` and FFmpeg picks its own default.
            (
                "software, no implementation measured",
                resolve(DecodeBackend::Software, None),
            ),
            // A hardware backend, which is no longer refused for being one.
            // It is refused here because the only contract in this policy was
            // qualified on software, and a decode path nobody qualified gets
            // no grammar — which is the same rule as every other line in this
            // list rather than a special case for hardware.
            (
                "a backend no contract covers",
                resolve(DecodeBackend::Qsv, Some("h264")),
            ),
            // A named implementation no contract covers.
            (
                "an implementation nobody qualified",
                resolve(DecodeBackend::Software, Some("libdav1d_h264")),
            ),
        ] {
            assert!(observation.grammar.is_none(), "{label}");
            assert!(
                !observation.qualified_logging(),
                "{label}: and it does not ask for flags it has no grammar for"
            );
            assert!(observation.contract_id.is_none(), "{label}");
            assert_eq!(observation.plan_digest, "digest", "{label}");
        }
    }

    /// Retained contracts report confidence; the direct Developer switch is
    /// what authorizes recovery. An uncovered path therefore receives an
    /// advisory grammar only while that switch is on.
    #[test]
    fn automatic_recovery_switch_admits_an_advisory_grammar_without_a_contract() {
        use plurx_core::transcode::DecodeBackend;

        let policy = crate::decoder_health::DiagnosticPolicy::default();
        let resolve = |enabled| {
            DiagnosticObservation::resolve(
                &policy,
                "digest".to_owned(),
                Some("h264"),
                DecodeBackend::VideoToolbox,
                None,
                0,
                enabled,
            )
        };
        let off = resolve(false);
        assert!(off.grammar.is_none());
        assert!(!off.qualified_logging());

        let on = resolve(true);
        assert!(on.grammar.is_some());
        assert!(on.qualified_logging());
        assert_eq!(
            on.contract_id, None,
            "advisory action is never a receipt contract"
        );
    }

    /// One contract, on the backend asked for. `h264` on both, because that is
    /// the measured fact the tests below are about: the accelerated and the
    /// software decode of h264 print the same decoder name.
    fn decode_contract_fixture(
        backend: plurx_core::transcode::DecodeBackend,
    ) -> crate::decoder_health::DiagnosticContract {
        crate::decoder_health::DiagnosticContract {
            id: "fixture".to_owned(),
            host: "test".to_owned(),
            ffmpeg_version: "9.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
            stderr_mode: crate::decoder_health::QUALIFIED_STDERR_MODE.to_owned(),
            input_codec: "h264".to_owned(),
            decoder: "h264".to_owned(),
            decode_backend: backend.name().to_owned(),
            require_context_addresses: true,
            primary_message: "Decoding error:".to_owned(),
            subordinate_message: None,
            attributes_every_failure: false,
            error_detail: "corrupt input packet".to_owned(),
            fixture: "f".to_owned(),
            fixture_sha256: "d".repeat(64),
            scope: "test".to_owned(),
            backend_fault_detail: None,
        }
    }

    /// A contract is qualified on a decode *path*, and the decoder name does
    /// not carry the path.
    ///
    /// Measured on the local Apple toolchain, FFmpeg 9.0.1: a VideoToolbox
    /// decode of h264 and a software decode of h264 print the identical
    /// context, `[vist#0:0/h264 @ …] [dec:h264 @ …]`. That backend attaches an
    /// accelerator to the same decoder rather than selecting a
    /// differently-named one, and the only line that says so at all is
    /// `Selecting decoder 'h264' because of requested hwaccel method
    /// videotoolbox` — which is not in the context grammar.
    ///
    /// So a contract keyed on the name alone would answer for a decode path
    /// nobody qualified. This is the assertion that it does not, and it is the
    /// reason M7b exists rather than simply deleting a refusal.
    #[test]
    fn a_contract_qualified_on_one_backend_does_not_answer_for_another() {
        use plurx_core::transcode::DecodeBackend;

        let software = decode_contract_fixture(DecodeBackend::Software);
        let accelerated = decode_contract_fixture(DecodeBackend::VideoToolbox);
        assert_eq!(
            software.decoder, accelerated.decoder,
            "the premise: both decode paths print the same decoder name"
        );

        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: software.ffmpeg_version.clone(),
                binary_sha256: software.binary_sha256.clone(),
                buildconf_sha256: software.buildconf_sha256.clone(),
            }),
            vec![software.clone()],
        );
        assert!(
            policy
                .contract_for(
                    "h264",
                    "h264",
                    DecodeBackend::Software.name(),
                    crate::decoder_health::QUALIFIED_STDERR_MODE,
                )
                .is_some(),
            "the software contract covers the path it was qualified on"
        );
        assert!(
            policy
                .contract_for(
                    "h264",
                    "h264",
                    DecodeBackend::VideoToolbox.name(),
                    crate::decoder_health::QUALIFIED_STDERR_MODE,
                )
                .is_none(),
            "and answers for no other, even though the decoder name matches"
        );

        // And the reverse, so this is a key rather than a one-way refusal.
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: accelerated.ffmpeg_version.clone(),
                binary_sha256: accelerated.binary_sha256.clone(),
                buildconf_sha256: accelerated.buildconf_sha256.clone(),
            }),
            vec![accelerated],
        );
        assert!(policy
            .contract_for(
                "h264",
                "h264",
                DecodeBackend::VideoToolbox.name(),
                crate::decoder_health::QUALIFIED_STDERR_MODE,
            )
            .is_some());
        assert!(
            policy
                .contract_for(
                    "h264",
                    "h264",
                    DecodeBackend::Software.name(),
                    crate::decoder_health::QUALIFIED_STDERR_MODE,
                )
                .is_none(),
            "a hardware contract is not a licence to read a software decode"
        );
    }

    /// A hardware plan that names its decoder and is covered gets a grammar.
    ///
    /// This is the refusal M7a found and the whole reason the recovery could
    /// not fire: the backend used to be rejected before the contract was ever
    /// consulted, so a hardware decode could not produce the evidence that
    /// triggers the software-decode alternate — while a software decode, which
    /// can produce it, has no alternate to be given.
    #[test]
    fn a_covered_hardware_decode_is_no_longer_refused_for_being_hardware() {
        use plurx_core::transcode::DecodeBackend;

        let contract = decode_contract_fixture(DecodeBackend::VideoToolbox);
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let observation = DiagnosticObservation::resolve(
            &policy,
            "digest".to_owned(),
            Some("h264"),
            DecodeBackend::VideoToolbox,
            Some("h264"),
            0,
            true,
        );
        assert!(observation.grammar.is_some());
        assert!(observation.qualified_logging());
        assert_eq!(observation.contract_id.as_deref(), Some("fixture"));

        // The refusals that remain are the ones that were always right: a
        // plan that names no decoder still gets nothing, whatever its backend.
        assert!(
            DiagnosticObservation::resolve(
                &policy,
                "digest".to_owned(),
                Some("h264"),
                DecodeBackend::VideoToolbox,
                None,
                0,
                false,
            )
            .grammar
            .is_none(),
            "a plan that does not say the decoder out loud is still unqualified"
        );
    }

    #[tokio::test]
    async fn a_qualified_hardware_plan_reads_its_name_from_the_backend_inventory() {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::{ArtifactQualification, DecodeBackend};

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let measured =
            MeasuredDecoders::from_measured(&[("hevc", DecodeBackend::VideoToolbox, "hevc")]);
        let (mgr, _work, _cache) = cached_manager(&store);
        let mgr = mgr.with_measured_decoders(measured.clone());
        mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
        let opts = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(&file, &opts, Encoder::VideoToolbox)
            .await
            .expect("hardware plan");
        assert_eq!(plan.decode().backend(), DecodeBackend::VideoToolbox);
        assert_eq!(
            plan.decode().software_decoder(),
            None,
            "the measured hardware name does not enter the plan or its digest"
        );

        let mut contract = decode_contract_fixture(DecodeBackend::VideoToolbox);
        contract.input_codec = "hevc".to_owned();
        contract.decoder = "hevc".to_owned();
        let policy = crate::decoder_health::DiagnosticPolicy::new(
            Some(crate::decoder_health::MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        );
        let observation = DiagnosticObservation::for_plan_against(&policy, &plan, &measured, true);
        assert!(observation.grammar.is_some());
        assert_eq!(observation.contract_id.as_deref(), Some("fixture"));

        let only_software =
            MeasuredDecoders::from_measured(&[("hevc", DecodeBackend::Software, "hevc")]);
        assert!(
            DiagnosticObservation::for_plan_against(&policy, &plan, &only_software, false)
                .grammar
                .is_none(),
            "a software measurement with the same name says nothing about VideoToolbox"
        );
    }

    /// A contract table written before the backend key parses as software.
    ///
    /// The retained tables on disk have no `decode_backend`, and they describe
    /// software decodes. A default that widened them to every backend would
    /// hand a software grammar to a hardware decode on the first node that
    /// upgraded — the exact confusion this key exists to prevent, arriving
    /// through the door marked backwards compatibility.
    #[test]
    fn a_contract_written_before_the_backend_key_means_software() {
        let table = r#"
version = 2

[[contracts]]
id = "legacy"
host = "test"
ffmpeg_version = "8.0.1"
binary_sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
buildconf_sha256 = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
stderr_mode = "repeat+level+error"
input_codec = "h264"
decoder = "h264"
require_context_addresses = true
primary_message = "Error submitting packet to decoder:"
attributes_every_failure = true
error_detail = "corrupt input packet"
fixture = "f"
fixture_sha256 = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
scope = "test"
"#;
        let contracts = crate::decoder_health::DiagnosticContract::load(table).expect("load");
        assert_eq!(contracts.len(), 1);
        assert_eq!(
            contracts[0].decode_backend,
            plurx_core::transcode::DecodeBackend::Software.name(),
        );
    }

    /// The flags a child is launched with are the flags the contract was
    /// selected under. One decision, read twice, and nothing that can drift.
    #[test]
    fn the_flags_asked_for_are_the_flags_the_contract_was_matched_under() {
        assert_eq!(
            crate::decoder_health::QUALIFIED_STDERR_MODE,
            plurx_core::transcode::DiagnosticLogging::Qualified.flags(),
            "the mode a contract is matched under and the token the command \
             emits are one string, or a qualified node reads a stream that \
             carries no severity labels"
        );
        assert_eq!(
            crate::decoder_health::LEGACY_STDERR_MODE,
            plurx_core::transcode::DiagnosticLogging::Legacy.flags()
        );
        let unqualified = DiagnosticObservation::default();
        assert!(!unqualified.qualified_logging());
        assert!(DiagnosticObservation::copy("session-abc")
            .plan_digest
            .starts_with("copy:"));
    }

    /// A part that ran to the end, one that yielded, and one that failed are
    /// three different receipts. Collapsing them loses the distinction §6.2
    /// keeps a yielded part's finished segments on.
    #[test]
    fn a_parts_ending_maps_to_its_own_disposition() {
        use crate::decoder_health::ExitDisposition;
        assert_eq!(
            part_exit_disposition(&PartEnd::Finished),
            ExitDisposition::CleanEnd
        );
        assert_eq!(
            part_exit_disposition(&PartEnd::Preempted),
            ExitDisposition::IntentionalYield
        );
        assert_eq!(
            part_exit_disposition(&PartEnd::Deadline),
            ExitDisposition::IntentionalYield
        );
        assert_eq!(
            part_exit_disposition(&PartEnd::Failed("boom".to_owned())),
            ExitDisposition::FailedTermination
        );
    }

    /// The reporting path end to end: a grammar latches, the sink reports, the
    /// handle publishes, the actor stores it.
    ///
    /// Without this, `DecodeFaultSink::report` could be replaced with an empty
    /// body and every other test in the workspace would still pass — the
    /// feature would be dead in production and the suite silent about it.
    #[tokio::test]
    async fn a_latched_fault_travels_from_the_reader_to_the_actor() {
        let control = crate::playback_control::RollingControlHandle::spawn("session-start");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("initial producer attempt");

        let contract = crate::decoder_health::DiagnosticContract {
            id: "fixture".to_owned(),
            host: "test".to_owned(),
            ffmpeg_version: "8.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
            stderr_mode: crate::decoder_health::QUALIFIED_STDERR_MODE.to_owned(),
            input_codec: "h264".to_owned(),
            decoder: "h264".to_owned(),
            decode_backend: plurx_core::transcode::DecodeBackend::Software
                .name()
                .to_owned(),
            require_context_addresses: true,
            primary_message: "Error submitting packet to decoder:".to_owned(),
            subordinate_message: Some("No frame decoded?".to_owned()),
            attributes_every_failure: true,
            error_detail: "corrupt input packet".to_owned(),
            fixture: "f".to_owned(),
            fixture_sha256: "d".repeat(64),
            scope: "test".to_owned(),
            backend_fault_detail: None,
        };
        let observation = DiagnosticObservation {
            plan_digest: "plan-digest".to_owned(),
            contract_id: Some(contract.id.clone()),
            grammar: Some(crate::decoder_health::DiagnosticGrammar::new(
                contract, 0, 0,
            )),
        };
        let observer =
            FfmpegProgressObserver::rolling(Arc::new(Progress::new()), attempt, control.clone());
        let sink = observation
            .fault_sink(&observer)
            .expect("a grammar and an actor is a sink");

        // Through the reader, not around it: the callback fires exactly on the
        // line that latches, which is what makes one barrier per attempt a
        // property of the accumulator rather than of this test.
        let (mut writer, reader) = tokio::io::duplex(8192);
        let line = "[vist#0:0/h264 @ 0x1] [dec:h264 @ 0x2] [error] Error submitting packet to decoder: corrupt input packet\n";
        let grammar = observation.grammar.clone();
        let reads = tokio::spawn(async move {
            crate::decoder_health::read_diagnostics_reporting(
                reader,
                grammar,
                |_| {},
                |_| false,
                move |fault, records, action_qualified| {
                    sink.report(fault, records, action_qualified);
                },
            )
            .await
        });
        {
            use tokio::io::AsyncWriteExt;
            for _ in 0..crate::decoder_health::VIDEO_DECODE_ERROR_LIMIT {
                writer
                    .write_all(line.as_bytes())
                    .await
                    .expect("write a primary record");
            }
        }
        drop(writer);
        let accumulator = reads.await.expect("join the reader");
        assert_eq!(
            accumulator.fault(),
            Some(crate::decoder_health::DecodeFaultKind::VideoDecodeFailure)
        );

        let snapshot = control
            .snapshot()
            .await
            .expect("the session is live")
            .producer_control;
        assert_eq!(
            snapshot.decode_fault,
            Some("video_decode_failure"),
            "the fault the reader latched reached the actor"
        );
        assert_eq!(
            snapshot.decode_error_records,
            Some(crate::decoder_health::VIDEO_DECODE_ERROR_LIMIT as u64)
        );
    }

    /// A sink exists only when there is both a grammar that can latch a fault
    /// and an actor to receive it. An offline part has neither.
    #[test]
    fn an_attempt_with_no_grammar_or_no_actor_has_no_sink() {
        let observer = FfmpegProgressObserver::offline(Arc::new(Progress::new()), 1);
        assert!(
            DiagnosticObservation::default()
                .fault_sink(&observer)
                .is_none(),
            "no grammar, no sink"
        );
    }

    fn health_plan_digest() -> String {
        "9".repeat(64)
    }

    fn health_receipt(
        qualification: crate::decoder_health::Qualification,
    ) -> crate::decoder_health::ProducerHealthReceipt {
        crate::decoder_health::ProducerHealthReceipt {
            receipt_version: crate::decoder_health::PRODUCER_HEALTH_RECEIPT_VERSION,
            plan_digest: health_plan_digest(),
            diagnostic_contract: Some("ffmpeg-test-v1".to_owned()),
            observation_complete: true,
            video_decode_error_records: 0,
            contract_qualified_error_records: 0,
            terminal_fault: None,
            exit_disposition: crate::decoder_health::ExitDisposition::CleanEnd,
            qualification,
        }
    }

    /// One part directory with `count` one-second segments, plus the playlist
    /// `assemble` reads to place them.
    async fn write_health_part(
        temp: &plurx_core::fs_secure::SecureDirectory,
        index: usize,
        count: usize,
    ) -> crate::produce::Part {
        let name = crate::produce::part_dir(index);
        let part = temp
            .create_child_directory(&name)
            .await
            .expect("part directory");
        let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:2\n");
        let mut segments = Vec::new();
        for segment in 0..count {
            let file = format!("seg{segment:05}.ts");
            part.atomic_write_child(&file, format!("part {index} segment {segment}").as_bytes())
                .await
                .expect("segment");
            playlist.push_str("#EXTINF:1.000,\n");
            playlist.push_str(&file);
            playlist.push('\n');
            segments.push(file);
        }
        playlist.push_str("#EXT-X-ENDLIST\n");
        part.atomic_write_child("index.m3u8", playlist.as_bytes())
            .await
            .expect("part playlist");
        crate::produce::Part {
            segments,
            durations_ms: vec![1_000; count],
        }
    }

    #[test]
    fn a_pass_that_attempted_nothing_settles_to_no_receipt() {
        let observation =
            GenerationObservation::inheriting(Vec::new(), None, &health_plan_digest());
        assert_eq!(observation.settle(), None);
        // And a pass whose only attempt was clean certifies its own work, so
        // the test above is about inheritance rather than about nothing ever
        // qualifying.
        let mut clean = GenerationObservation::inheriting(Vec::new(), None, &health_plan_digest());
        clean.record(
            health_receipt(crate::decoder_health::Qualification::Qualified),
            true,
        );
        assert!(clean.settle().expect("settled receipt").permits_reuse());
    }

    #[test]
    fn an_attempt_that_produced_no_bytes_still_refuses_the_generation() {
        // The defect this type exists to make structurally impossible. An
        // attempt whose decode failed writes no segment and exits zero, so a
        // record keyed on "did it produce bytes" would drop precisely the
        // receipt saying the film is truncated at a corrupt region — and the
        // producer's progress observer carries no control handle, so nothing
        // else in the process ever sees that fault.
        let digest = health_plan_digest();
        let mut observation = GenerationObservation::inheriting(Vec::new(), None, &digest);
        observation.record(
            health_receipt(crate::decoder_health::Qualification::Qualified),
            true,
        );
        let mut failed = health_receipt(crate::decoder_health::Qualification::Rejected);
        failed.terminal_fault = Some(crate::decoder_health::DecodeFaultKind::VideoDecodeFailure);
        failed.video_decode_error_records = 5;
        failed.contract_qualified_error_records = 5;
        // No part accompanies this one: the attempt left nothing on disk. It
        // is recorded regardless.
        observation.record(failed, true);
        observation.record(
            health_receipt(crate::decoder_health::Qualification::Qualified),
            true,
        );
        let settled = observation.settle().expect("settled receipt");
        assert_eq!(
            settled.qualification,
            crate::decoder_health::Qualification::Rejected
        );
        assert_eq!(
            settled.terminal_fault,
            Some(crate::decoder_health::DecodeFaultKind::VideoDecodeFailure)
        );
        assert!(!settled.permits_reuse());
    }

    #[tokio::test]
    async fn a_fresh_assembly_carries_the_receipt_its_parts_earned() {
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        let parts = vec![
            write_health_part(&temp, 0, 2).await,
            write_health_part(&temp, 1, 1).await,
        ];
        let receipt = health_receipt(crate::decoder_health::Qualification::Qualified);
        let published = publish_from(&temp, &parts, Some(receipt.clone()))
            .await
            .expect("publish")
            .expect("assembled generation");
        assert_eq!(published.segments, 3);
        assert_eq!(published.health, Some(receipt));
    }

    #[tokio::test]
    async fn adopting_this_pass_parts_own_assembly_keeps_their_receipt() {
        // The case that makes long films certifiable at all. Assembling a film
        // and then hashing it for its manifest is preemptible, so a pass can
        // leave a finished assembly behind and yield. If the next pass adopted
        // it without a receipt, every film long enough to be interrupted there
        // would be refused permanently under the qualified identity — the exact
        // failure the per-part records exist to prevent, reached one step
        // later.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        let parts = vec![
            write_health_part(&temp, 0, 2).await,
            write_health_part(&temp, 1, 1).await,
        ];
        let receipt = health_receipt(crate::decoder_health::Qualification::Qualified);
        publish_from(&temp, &parts, Some(receipt.clone()))
            .await
            .expect("first publish")
            .expect("assembled generation");

        let assembled = temp
            .open_child_directory(ASSEMBLED_DIR)
            .await
            .expect("assembled directory");
        let adopted = assembled_publication(&assembled, &parts, Some(receipt.clone()))
            .await
            .expect("adopted generation");
        assert_eq!(adopted.segments, 3);
        assert_eq!(adopted.health, Some(receipt));
    }

    #[tokio::test]
    async fn an_assembly_these_parts_did_not_produce_carries_no_receipt() {
        // The tie is the playlist, because `publish_from` writes exactly the
        // bytes `assemble` produces. A generation assembled from something
        // else is still adopted — the bytes may be perfectly good — but
        // nothing this pass observed describes them.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        let parts = vec![write_health_part(&temp, 0, 2).await];
        publish_from(&temp, &parts, None)
            .await
            .expect("first publish")
            .expect("assembled generation");

        let assembled = temp
            .open_child_directory(ASSEMBLED_DIR)
            .await
            .expect("assembled directory");
        // The same segments, described by a part list that is not the one that
        // made them.
        let mistaken = vec![crate::produce::Part {
            segments: vec!["seg00000.ts".to_owned(), "seg00001.ts".to_owned()],
            durations_ms: vec![1_000, 2_500],
        }];
        let adopted = assembled_publication(
            &assembled,
            &mistaken,
            Some(health_receipt(
                crate::decoder_health::Qualification::Qualified,
            )),
        )
        .await
        .expect("adopted generation");
        assert_eq!(
            adopted.health, None,
            "a receipt describes the parts it was settled over, not whatever is on disk"
        );
    }

    /// The shape `read_validated_part` would measure for a part directory.
    async fn measured_shape(
        temp: &plurx_core::fs_secure::SecureDirectory,
        index: usize,
    ) -> Vec<(String, u64, i64)> {
        let dir = temp
            .open_child_directory(&crate::produce::part_dir(index))
            .await
            .expect("part directory");
        read_validated_part(&dir, MAX_PRETRANSCODE_PART_PLAYLIST_BYTES)
            .await
            .expect("validated part")
            .shape
    }

    async fn seal_part_health(
        temp: &plurx_core::fs_secure::SecureDirectory,
        index: usize,
        receipt: &crate::decoder_health::ProducerHealthReceipt,
    ) {
        let shape = measured_shape(temp, index).await;
        let dir = temp
            .open_child_directory(&crate::produce::part_dir(index))
            .await
            .expect("part directory");
        retain_part_health(&dir, &shape, receipt).await;
    }

    async fn resumed_health(
        temp: &plurx_core::fs_secure::SecureDirectory,
        plan_digest: &str,
    ) -> Vec<crate::decoder_health::ProducerHealthReceipt> {
        resume_parts(temp, plan_digest)
            .await
            .expect("resume parts")
            .receipts
    }

    #[tokio::test]
    async fn a_sealed_part_record_lets_a_resumed_film_still_be_certified() {
        // Without this, every film long enough to need a second pass is
        // permanently uncertifiable, and the qualified artifact namespace could
        // never hold a long title at all.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        write_health_part(&temp, 0, 2).await;
        write_health_part(&temp, 1, 1).await;
        let clean = health_receipt(crate::decoder_health::Qualification::Qualified);
        seal_part_health(&temp, 0, &clean).await;
        seal_part_health(&temp, 1, &clean).await;

        let digest = health_plan_digest();
        let inherited = resumed_health(&temp, &digest).await;
        assert_eq!(inherited.len(), 2);
        let mut observation = GenerationObservation::inheriting(inherited, None, &digest);
        observation.record(clean, true);
        let settled = observation.settle().expect("settled receipt");
        assert!(
            settled.permits_reuse(),
            "a resumed film whose every part carries a clean record is certifiable"
        );
    }

    #[tokio::test]
    async fn a_part_with_no_record_resumes_unobserved() {
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        write_health_part(&temp, 0, 1).await;
        let inherited = resumed_health(&temp, &health_plan_digest()).await;
        assert_eq!(inherited.len(), 1);
        assert!(!inherited[0].permits_reuse());
        assert!(!inherited[0].observation_complete);
    }

    #[tokio::test]
    async fn a_record_no_longer_bound_to_the_bytes_beside_it_is_refused() {
        // Why the record is sealed over a shape at all. Re-encoding a part and
        // leaving the old record behind would carry a clean certificate onto
        // bytes nobody watched being made.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        write_health_part(&temp, 0, 1).await;
        seal_part_health(
            &temp,
            0,
            &health_receipt(crate::decoder_health::Qualification::Qualified),
        )
        .await;
        assert!(resumed_health(&temp, &health_plan_digest()).await[0].permits_reuse());

        // Same name, same playlist, different bytes.
        let part = temp
            .open_child_directory(&crate::produce::part_dir(0))
            .await
            .expect("part directory");
        part.atomic_write_child("seg00000.ts", b"a differently sized segment")
            .await
            .expect("rewrite segment");
        let inherited = resumed_health(&temp, &health_plan_digest()).await;
        assert!(
            !inherited[0].permits_reuse(),
            "a record sealed over other bytes must not certify these"
        );
    }

    #[tokio::test]
    async fn a_record_sealed_for_another_plan_is_refused() {
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        write_health_part(&temp, 0, 1).await;
        seal_part_health(
            &temp,
            0,
            &health_receipt(crate::decoder_health::Qualification::Qualified),
        )
        .await;
        let other_plan = "7".repeat(64);
        let inherited = resumed_health(&temp, &other_plan).await;
        assert!(
            !inherited[0].permits_reuse(),
            "a record can only be opened by the plan it was sealed for"
        );
        assert_eq!(inherited[0].plan_digest, other_plan);
    }

    #[tokio::test]
    async fn an_edited_record_is_refused_rather_than_read() {
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        write_health_part(&temp, 0, 1).await;
        seal_part_health(
            &temp,
            0,
            &health_receipt(crate::decoder_health::Qualification::Unqualified),
        )
        .await;
        let part = temp
            .open_child_directory(&crate::produce::part_dir(0))
            .await
            .expect("part directory");
        let encoded = part
            .read_bounded_child(PART_HEALTH_FILE, MAX_PART_HEALTH_BYTES)
            .await
            .expect("read record");
        let text = String::from_utf8(encoded).expect("utf8 record");
        let promoted = text.replace("\"unqualified\"", "\"qualified\"");
        assert_ne!(
            promoted, text,
            "the qualification must appear in the record"
        );
        part.atomic_write_child(PART_HEALTH_FILE, promoted.as_bytes())
            .await
            .expect("write promoted record");
        let inherited = resumed_health(&temp, &health_plan_digest()).await;
        assert!(
            !inherited[0].permits_reuse(),
            "a record whose own digest does not check out is not read"
        );
    }

    #[tokio::test]
    async fn a_record_is_never_placed_into_the_assembled_generation() {
        // It is staging-local evidence about how a part was made, not one of
        // the objects the manifest inventories.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        let parts = vec![write_health_part(&temp, 0, 2).await];
        seal_part_health(
            &temp,
            0,
            &health_receipt(crate::decoder_health::Qualification::Qualified),
        )
        .await;
        publish_from(&temp, &parts, None)
            .await
            .expect("publish")
            .expect("assembled generation");
        let mut placed = Vec::new();
        let mut entries = tokio::fs::read_dir(directory.path().join(ASSEMBLED_DIR))
            .await
            .expect("read assembled directory");
        while let Some(entry) = entries.next_entry().await.expect("assembled entry") {
            placed.push(entry.file_name().to_string_lossy().into_owned());
        }
        placed.sort();
        // The whole listing, not one probe for one name: a generation holds
        // its playlist and its segments and nothing else, whatever the record
        // is called.
        assert_eq!(placed, ["index.m3u8", "seg00000.ts", "seg00001.ts"]);
    }

    #[tokio::test]
    async fn an_attempt_that_left_no_part_is_carried_across_a_pass_boundary() {
        // Whether a film certifies must not depend on where preemption fell.
        // Within one pass a failed non-producing attempt is recorded and
        // refuses the generation; the ledger is what makes the same sequence
        // refuse it when a pass boundary lands in the middle.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        let digest = health_plan_digest();

        // Pass one: an attempt that decoded nothing and wrote no segment,
        // then a clean retry that produced the part.
        let mut first = GenerationObservation::inheriting(Vec::new(), None, &digest);
        let mut failed = health_receipt(crate::decoder_health::Qualification::Rejected);
        failed.terminal_fault = Some(crate::decoder_health::DecodeFaultKind::VideoDecodeFailure);
        first.record(failed, false);
        retain_generation_health(&temp, first.unproductive().expect("unproductive")).await;
        write_health_part(&temp, 0, 1).await;
        let clean = health_receipt(crate::decoder_health::Qualification::Qualified);
        first.record(clean.clone(), true);
        seal_part_health(&temp, 0, &clean).await;
        // …and then the pass is preempted, publishing nothing.

        // Pass two resumes, and must reach the same conclusion.
        let second = GenerationObservation::inheriting(
            resumed_health(&temp, &digest).await,
            carried_generation_health(&temp, &digest).await,
            &digest,
        );
        let settled = second.settle().expect("settled receipt");
        assert_eq!(
            settled.qualification,
            crate::decoder_health::Qualification::Rejected
        );
        assert_eq!(
            settled.terminal_fault,
            Some(crate::decoder_health::DecodeFaultKind::VideoDecodeFailure)
        );
        assert!(!settled.permits_reuse());
    }

    #[tokio::test]
    async fn a_ledger_that_is_there_and_will_not_open_is_unobserved() {
        // Absent means no pass claimed an unproductive attempt. Present and
        // unreadable means one did and it cannot be read, which is a different
        // statement and refuses reuse.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        let digest = health_plan_digest();
        assert_eq!(carried_generation_health(&temp, &digest).await, None);

        temp.atomic_write_child(GENERATION_HEALTH_FILE, b"{\"not\":\"a ledger\"}")
            .await
            .expect("write a ledger nobody can read");
        let carried = carried_generation_health(&temp, &digest)
            .await
            .expect("an unreadable ledger is still a statement");
        assert!(!carried.permits_reuse());
        assert!(!carried.observation_complete);
    }

    #[tokio::test]
    async fn a_ledger_sealed_for_another_plan_does_not_certify_this_one() {
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        retain_generation_health(
            &temp,
            &health_receipt(crate::decoder_health::Qualification::Qualified),
        )
        .await;
        let carried = carried_generation_health(&temp, &"7".repeat(64))
            .await
            .expect("a ledger for another plan is still a statement");
        assert!(!carried.permits_reuse());
    }

    #[tokio::test]
    async fn a_record_too_large_for_its_bound_is_not_written() {
        // `retain_part_health` is best effort, and this is the branch that
        // makes "best effort" mean something rather than being unreachable
        // prose. A plan digest is a digest everywhere it is produced, but the
        // type does not say so.
        let directory = crate::test_tempdir().expect("staging");
        let temp = plurx_core::fs_secure::SecureDirectory::open(directory.path())
            .await
            .expect("staging capability");
        write_health_part(&temp, 0, 1).await;
        let shape = measured_shape(&temp, 0).await;
        let part = temp
            .open_child_directory(&crate::produce::part_dir(0))
            .await
            .expect("part directory");
        let mut oversized = health_receipt(crate::decoder_health::Qualification::Qualified);
        oversized.plan_digest = "f".repeat(MAX_PART_HEALTH_BYTES as usize * 2);
        retain_part_health(&part, &shape, &oversized).await;
        assert!(part
            .child_metadata(PART_HEALTH_FILE)
            .await
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound));
        // And the part reads back as unobserved rather than as anything else.
        assert!(!resumed_health(&temp, &health_plan_digest()).await[0].permits_reuse());
    }

    // ---- the qualified artifact identity's receipt contract -----------------

    fn generation_manifest_with(
        health: Option<crate::decoder_health::ProducerHealthReceipt>,
    ) -> plurx_core::transcode::manifest::GenerationManifest {
        plurx_core::transcode::manifest::GenerationManifest {
            format_version: 1,
            generation_id: "generation-under-test".to_owned(),
            object_count: 1,
            objects: vec![plurx_core::transcode::manifest::GenerationObject {
                name: "index.m3u8".to_owned(),
                bytes: 32,
                sha256: "a".repeat(64),
            }],
            manifest_digest: "b".repeat(64),
            producer_health: health,
        }
    }

    async fn plan_under(
        qualification: plurx_core::transcode::ArtifactQualification,
    ) -> (
        ResolvedTranscode,
        TranscodeManager,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, work, cache) = cached_manager(&store);
        mgr.test_publish_artifact_qualification(qualification);
        let opts = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(&file, &opts, Encoder::Software)
            .await
            .expect("resolve plan");
        (plan, mgr, work, cache)
    }
