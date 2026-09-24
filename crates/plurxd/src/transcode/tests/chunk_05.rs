
    /// A recovery that was reserved and then failed to install has been
    /// attempted, and the row says so.
    ///
    /// This is the whole reason the budget is not refunded. The alternate is
    /// installed against a source whose decode already failed once; a refund
    /// on the failure path would hand the same playback another try at it, and
    /// the reopen loop this effort is named after is exactly a sequence of
    /// attempts that each looked like the first.
    ///
    /// The failure is a real one — an unclearable predecessor scratch, the
    /// same fixture the fencing test uses — rather than an injected error, so
    /// the settle under test is on the path a real transaction takes out.
    #[tokio::test]
    async fn a_recovery_that_fails_to_install_still_spends_the_budget() {
        use plurx_core::domain::ProducerRecoveryState;
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let dir = crate::test_tempdir().expect("session dir");
        seeded_session_dir(dir.path(), 1, 4.0).await;
        let served_name = dir.path().join("init.mp4");
        tokio::fs::create_dir(&served_name)
            .await
            .expect("undeletable served-name fixture");
        let session = Arc::new(test_session(dir.path().to_path_buf()));

        let mut opts = mgr.options_for_tone_map(
            Encoder::VideoToolbox,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        opts.pipeline = Pipeline::Cpu;
        let retry = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            dir.path(),
            "budget-spent-on-failure",
        )
        .await
        .expect("a CPU-pipeline hardware attempt has a software rung")
        .with_recovery(DecodeRecoveryReservation {
            ledger: crate::playback_control::ProducerRecoveryLedger::new(
                Arc::clone(&store),
                7,
                "playback-failing",
                "epoch-failing",
                "incarnation-1",
            )
            .expect("a complete identity"),
            failed_plan_digest: "a".repeat(64),
        });

        let failed_attempt = session
            .control
            .begin_producer_attempt()
            .await
            .expect("admit the predecessor attempt");
        *session.child.lock().await = Some(AttemptChild::new(
            failed_attempt,
            long_running_child(),
            session.control.clone(),
            None,
        ));

        let result = execute_prepublication_transcode_retry(
            Arc::clone(&session),
            &retry,
            1,
            failed_attempt,
            &retry.actor_recipe,
            crate::playback_control::ProducerDecisionReason::SourceDecodeRetry,
            "budget-spent-on-failure",
        )
        .await;
        assert!(
            matches!(&result, Err(error) if error.contains("clearing predecessor scratch")),
            "the fixture must fail inside the transaction, got {result:?}"
        );

        let row = store
            .producer_recovery_for_epoch(7, "playback-failing", "epoch-failing")
            .await
            .expect("read the row")
            .expect("the executor reserved before it installed");
        assert_eq!(
            row.state,
            ProducerRecoveryState::Exhausted,
            "a failed install settles the budget rather than leaving it held"
        );
        assert_eq!(row.failed_incarnation_id, "incarnation-1");
        assert_eq!(row.alternate_plan_digest, retry.observation.plan_digest);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("storage failure retires actor ownership");
    }

    #[tokio::test]
    async fn production_fallback_stays_fenced_when_predecessor_cleanup_fails() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let dir = crate::test_tempdir().expect("session dir");
        seeded_session_dir(dir.path(), 1, 4.0).await;
        let served_name = dir.path().join("init.mp4");
        tokio::fs::create_dir(&served_name)
            .await
            .expect("undeletable served-name fixture");
        tokio::fs::write(served_name.join("predecessor-bytes"), b"old")
            .await
            .expect("predecessor bytes");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        session.refresh_segments().await;
        let predecessor_live_bytes = session.live_bytes.load(Acquire);
        assert!(predecessor_live_bytes >= 1_024);

        let mut opts = mgr.options_for_tone_map(
            Encoder::VideoToolbox,
            &file,
            720,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        opts.pipeline = Pipeline::Cpu;
        // The production retry executor is what clears predecessor scratch,
        // and its transaction is what has to stay fenced when the clear
        // fails. Drive that, not the retired ladder helper.
        let retry = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            dir.path(),
            "fallback-clear-failure",
        )
        .await
        .expect("a CPU-pipeline hardware attempt has a software rung");
        let failed_attempt = session
            .control
            .begin_producer_attempt()
            .await
            .expect("admit the predecessor attempt");
        // The executor terminates the exact failed attempt before it clears
        // scratch, so the fixture needs a predecessor child bound to that
        // attempt. Without one the transaction fails at termination and never
        // reaches the clear this test is about.
        *session.child.lock().await = Some(AttemptChild::new(
            failed_attempt,
            long_running_child(),
            session.control.clone(),
            None,
        ));
        let result = execute_prepublication_transcode_retry(
            Arc::clone(&session),
            &retry,
            1,
            failed_attempt,
            &retry.actor_recipe,
            crate::playback_control::ProducerDecisionReason::ProgressDeadline,
            "fallback-clear-failure",
        )
        .await;

        assert!(
            result.is_err(),
            "an unclearable predecessor scratch must fail the retry transaction"
        );
        assert!(served_name.join("predecessor-bytes").exists());
        assert!(session.replacing_child.load(Acquire));
        // The retired helper left a stopped predecessor handle in the slot and
        // this test asserted on it. That was an artifact of the helper's
        // shape, not a property of the system: the production transaction
        // fails before it installs anything. "No successor" is what matters
        // and this is the assertion that says it.
        assert!(
            session.coherent_path_producer_attempt().await.is_none(),
            "a failed retry transaction installs no successor"
        );
        assert_eq!(
            session.live_bytes.load(Acquire),
            predecessor_live_bytes,
            "predecessor bytes remain charged until verified scratch clearing"
        );
        assert!(
            matches!(
                session.failure_reason(),
                PlaylistError::SessionFailed(reason)
                    if reason.contains("clearing predecessor scratch")
                        && reason.contains("init.mp4")
            ),
            "the failure names the scratch clear and the file that blocked it, \
             got {:?}",
            session.failure_reason()
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("storage failure retires actor ownership");
    }

    /// A playlist request that already resolved the session Arc still observes
    /// retirement promptly. The Arc keeps the object alive; it does not make a
    /// superseded session an addressable stream until the startup budget ends.
    #[tokio::test]
    async fn a_waiting_playlist_keeps_current_retirement_retryable_until_registry_removal() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let session = watchdog_session(dir.path(), None, false);
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.playlist_wait_override_ms.store(30_000, Relaxed);
        mgr.sessions
            .lock()
            .await
            .insert("retired-session".into(), Arc::clone(&session));

        let mut playlist = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.playlist("retired-session").await }
        });
        tokio::time::sleep(PLAYLIST_WAIT_POLL * 2).await;
        assert!(
            !playlist.is_finished(),
            "the playlist request acquired the session and is waiting for publication"
        );
        assert!(
            session
                .control
                .snapshot()
                .await
                .is_some_and(|lease| lease.last_renewal_kind == "test-start"),
            "waiting itself must not renew the lease"
        );
        assert!(
            mgr.retire_session("retired-session", &session).await,
            "the production retirement path must own this live session"
        );

        let refused = tokio::time::timeout(Duration::from_secs(1), &mut playlist)
            .await
            .expect("retirement must beat the 30-second startup budget")
            .expect("playlist task");
        assert!(matches!(refused, Err(PlaylistError::StartupTimedOut(_))));
        assert!(
            !session.failed.load(Relaxed),
            "routine retirement is gone/superseded, not a producer failure"
        );
    }

    /// Re-evaluating an unchanged running session must be a no-op. Without the
    /// state guard, a healthy playlist poll sends SIGCONT and touches the
    /// motion clock, which postpones the actor's producer-progress verdict.
    #[tokio::test]
    async fn unchanged_flow_control_state_does_not_touch_the_motion_clock() {
        use plurx_core::store::SqliteStore;
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 1, 4.0).await;
        let session = test_session(dir.path().to_path_buf());
        session.refresh_segments().await;
        session.progress.moved_at_ms.store(-10_000, Relaxed);
        let before = session.progress.stalled_for();

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        mgr.apply_ahead_window(
            &session,
            "unchanged-running",
            AheadLimits {
                max_secs: 180,
                max_bytes: 2_000_000_000,
                global_max_bytes: 8_000_000_000,
            },
            0,
            0,
            None,
        )
        .await;

        assert!(!session.suspended.load(Relaxed));
        assert!(
            session.progress.stalled_for() >= before,
            "an unchanged flow-control evaluation must not reset motion"
        );
    }

    #[tokio::test]
    async fn refresh_ignores_predecessor_scratch_during_admitted_cutover() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = test_session(dir.path().to_path_buf());

        session.replacing_child.store(true, Release);
        let successor = session
            .control
            .begin_producer_attempt()
            .await
            .expect("prepublication successor");
        session.reset_compatibility_delivery(successor).await;
        session.refresh_segments().await;

        assert!(session.segments.lock().await.segs.is_empty());
        assert_eq!(
            session
                .control
                .snapshot()
                .await
                .expect("rolling actor")
                .delivery
                .published_segment,
            None,
            "predecessor scratch cannot be attributed to the admitted successor"
        );
        session.replacing_child.store(false, Release);
    }

    #[tokio::test]
    async fn an_older_playlist_read_cannot_land_with_a_newer_catalog_revision() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .refresh_after_read_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));

        let older = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.refresh_segments().await }
        });
        pause.wait().await;

        // The first refresh already owns the two-second bytes but has not
        // merged them. A second refresh observes a rewritten three-second
        // timeline and must be the only one allowed to spend revision zero.
        seeded_session_dir(dir.path(), 2, 3.0).await;
        session.refresh_segments().await;
        pause.wait().await;
        older.await.expect("older refresh task");

        let index = session.segments.lock().await;
        assert_eq!(index.revision, 1);
        assert_eq!(index.end_ms_of(0), Some(3_000));
        assert_eq!(index.produced_playable_end_ms(), Some(6_000));
    }

    #[tokio::test]
    async fn playlist_owner_sampling_cannot_cross_a_complete_replacement_aba() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let manager_dir = crate::test_tempdir().expect("manager tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let owner_pause = Arc::new(tokio::sync::Barrier::new(2));
        let replacement_pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .path_owner_sample_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&owner_pause));
        *session
            .replacement_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&replacement_pause));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            manager_dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("playlist-aba".into(), Arc::clone(&session));

        let playlist = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.playlist_with_owner("playlist-aba").await }
        });
        owner_pause.wait().await;
        let successor = tokio::spawn(complete_seeded_replacement(
            Arc::clone(&session),
            dir.path().to_path_buf(),
            16,
            3.0,
            None,
        ));
        replacement_pause.wait().await;
        owner_pause.wait().await;
        replacement_pause.wait().await;
        let successor = successor.await.expect("replacement task");

        let (bytes, owner) = playlist
            .await
            .expect("playlist task")
            .unwrap_or_else(|_| panic!("successor playlist"));
        let text = String::from_utf8(bytes).expect("playlist text");
        assert!(text.contains("#EXTINF:3.000"), "{text}");
        assert!(!text.contains("#EXTINF:2.000"), "{text}");
        assert!(matches!(
            owner.0,
            MediaResponseOwnerKind::Rolling {
                producer_attempt,
                ..
            } if producer_attempt == successor
        ));
    }

    #[tokio::test]
    async fn segment_owner_sampling_cannot_cross_a_complete_replacement_aba() {
        use plurx_core::store::SqliteStore;
        use tokio::io::AsyncReadExt as _;

        let dir = crate::test_tempdir().expect("tempdir");
        let manager_dir = crate::test_tempdir().expect("manager tempdir");
        seeded_session_dir(dir.path(), 1, 2.0).await;
        tokio::fs::write(dir.path().join("seg00000.ts"), b"predecessor")
            .await
            .expect("predecessor segment");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let owner_pause = Arc::new(tokio::sync::Barrier::new(2));
        let replacement_pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .path_owner_sample_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&owner_pause));
        *session
            .replacement_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&replacement_pause));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            manager_dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("segment-aba".into(), Arc::clone(&session));

        let segment = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move { mgr.segment("segment-aba", "seg00000.ts").await }
        });
        owner_pause.wait().await;
        let successor = tokio::spawn(complete_seeded_replacement(
            Arc::clone(&session),
            dir.path().to_path_buf(),
            16,
            3.0,
            Some(b"successor"),
        ));
        replacement_pause.wait().await;
        owner_pause.wait().await;
        replacement_pause.wait().await;
        let successor = successor.await.expect("replacement task");

        let mut segment = segment
            .await
            .expect("segment task")
            .expect("segment open")
            .expect("successor segment");
        let mut bytes = Vec::new();
        segment
            .file
            .read_to_end(&mut bytes)
            .await
            .expect("read successor segment");
        assert_eq!(bytes, b"successor");
        assert!(matches!(
            segment.response_owner().0,
            MediaResponseOwnerKind::Rolling {
                producer_attempt,
                ..
            } if producer_attempt == successor
        ));
    }

    #[tokio::test]
    async fn http_wait_ledger_counts_parked_requests_until_their_guard_drops() {
        let dir = crate::test_tempdir().expect("tempdir");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        assert_eq!(session.http_waits.snapshot(), HttpWaitSnapshot::default());
        let first = HttpWaitGuard::enter(&session, Some(7));
        std::thread::sleep(Duration::from_millis(5));
        let second = HttpWaitGuard::enter(&session, Some(9));
        let parked = session.http_waits.snapshot();
        assert_eq!(parked.count, 2);
        assert_eq!(
            parked.oldest_segment,
            Some(7),
            "the oldest wait names its segment"
        );
        assert!(parked.oldest_ms.is_some_and(|ms| ms >= 5), "{parked:?}");
        drop(first);
        let after = session.http_waits.snapshot();
        assert_eq!((after.count, after.oldest_segment), (1, Some(9)));
        drop(second);
        assert_eq!(session.http_waits.snapshot(), HttpWaitSnapshot::default());
    }

    /// The status reading is only honest if a request that is really waiting
    /// shows up in it, and one the client abandons mid-wait leaves it again.
    #[tokio::test]
    async fn a_parked_segment_request_is_counted_until_the_client_drops_it() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let manager_dir = crate::test_tempdir().expect("manager tempdir");
        seeded_session_dir(dir.path(), 1, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            manager_dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("parked-wait".into(), Arc::clone(&session));

        let request = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move {
                mgr.segment_for_publication_before(
                    "parked-wait",
                    "seg00009.ts",
                    Instant::now() + Duration::from_secs(30),
                )
                .await
                .map(|_| ())
            }
        });
        let give_up = Instant::now() + Duration::from_secs(5);
        while session.http_waits.snapshot().count == 0 {
            assert!(
                Instant::now() < give_up,
                "a request for an unpublished segment never registered as a wait"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let parked = session.http_waits.snapshot();
        assert_eq!(parked.count, 1);
        assert_eq!(parked.oldest_segment, Some(9));
        // And the status clients poll carries it, rather than the zero it
        // used to report whatever was happening.
        let status = session_info(
            "parked-wait",
            &session,
            AheadLimits {
                max_secs: 0,
                max_bytes: 0,
                global_max_bytes: 0,
            },
            0,
            0,
        )
        .await;
        assert_eq!(status.http_wait_count, 1);
        assert_eq!(status.http_wait_segment, Some(9));
        assert!(status.http_wait_oldest_ms.is_some());

        request.abort();
        let _ = request.await;
        assert_eq!(
            session.http_waits.snapshot(),
            HttpWaitSnapshot::default(),
            "a request the client dropped mid-wait must not stay counted"
        );
    }

    /// A cached session skips the publication check and waits only at the
    /// loop's final sleep, for a file that is not on disk. That wait counts
    /// too.
    #[tokio::test]
    async fn a_parked_request_on_a_cached_session_is_counted() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let manager_dir = crate::test_tempdir().expect("manager tempdir");
        seeded_session_dir(dir.path(), 1, 2.0).await;
        let mut session = test_session(dir.path().to_path_buf());
        session.cached = true;
        let session = Arc::new(session);
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            manager_dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("parked-cached".into(), Arc::clone(&session));

        let request = tokio::spawn({
            let mgr = Arc::clone(&mgr);
            async move {
                mgr.segment_for_publication_before(
                    "parked-cached",
                    "seg00009.ts",
                    Instant::now() + Duration::from_secs(30),
                )
                .await
                .map(|_| ())
            }
        });
        let give_up = Instant::now() + Duration::from_secs(5);
        while session.http_waits.snapshot().count == 0 {
            assert!(
                !request.is_finished(),
                "the cached lookup returned instead of waiting: {:?}",
                request.await.map(|result| result.is_ok())
            );
            assert!(
                Instant::now() < give_up,
                "a cached request for a missing segment never registered as a wait"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(session.http_waits.snapshot().oldest_segment, Some(9));
        request.abort();
        let _ = request.await;
        assert_eq!(session.http_waits.snapshot(), HttpWaitSnapshot::default());
    }

    #[tokio::test]
    async fn refresh_owner_sampling_cannot_cross_a_complete_replacement_aba() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 2, 2.0).await;
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let owner_pause = Arc::new(tokio::sync::Barrier::new(2));
        let replacement_pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .path_owner_sample_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&owner_pause));
        *session
            .replacement_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&replacement_pause));

        let refresh = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.refresh_segments().await }
        });
        owner_pause.wait().await;
        let successor = tokio::spawn(complete_seeded_replacement(
            Arc::clone(&session),
            dir.path().to_path_buf(),
            16,
            3.0,
            None,
        ));
        replacement_pause.wait().await;
        owner_pause.wait().await;
        refresh.await.expect("stale refresh task");
        assert!(session.segments.lock().await.segs.is_empty());
        replacement_pause.wait().await;
        let successor = successor.await.expect("replacement task");

        session.refresh_segments().await;
        assert_eq!(
            session.segments.lock().await.produced_playable_end_ms(),
            Some(48_000)
        );
        let delivery = session
            .control
            .snapshot()
            .await
            .expect("rolling actor")
            .delivery;
        assert_eq!(delivery.producer_attempt, successor);
        assert_eq!(delivery.published_end_ms, Some(48_000));
    }

    #[tokio::test]
    async fn retention_finishes_predecessor_deletes_before_reused_successor_paths() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 100, 4.0).await;
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let raw = tokio::fs::read_to_string(dir.path().join("index.m3u8"))
            .await
            .expect("seed playlist");
        let mut seeded = SegmentIndex {
            segs: parse_playlist(&raw),
            revision: 0,
        };
        for segment in &mut seeded.segs {
            segment.bytes = 1_024;
        }
        *session.segments.lock().await = seeded;
        session.fetched_end_ms.store(300_000, Relaxed);
        expire_seeded_prefix(&session, 30).await;

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .retention_delete_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let gc = tokio::spawn({
            let session = Arc::clone(&session);
            async move { gc_expired_segments(&session).await }
        });
        pause.wait().await;
        let queued_during_pause = session
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        assert!(queued_during_pause > 0);
        assert!(session.retention_cleanup_active.load(Acquire));
        gc_expired_segments(&session).await;
        assert_eq!(
            session
                .retention_cleanup_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            queued_during_pause,
            "a repeated repair tick cannot enqueue a second cleanup batch"
        );

        let replacement = tokio::spawn({
            let session = Arc::clone(&session);
            let path = dir.path().to_path_buf();
            async move {
                let (replacement, attempt) = session
                    .kill_child_for_replacement()
                    .await
                    .expect("prepublication replacement");
                clear_session_dir(&path).await.expect("clear predecessor");
                session.confirm_predecessor_scratch_cleared();
                seeded_session_dir(&path, 2, 2.0).await;
                *session.child.lock().await = Some(AttemptChild::new(
                    session.control.current_producer_attempt(),
                    long_running_child(),
                    session.control.clone(),
                    None,
                ));
                replacement.complete();
                attempt
            }
        });
        gc.await.expect("retention task");
        let successor = tokio::time::timeout(Duration::from_secs(1), replacement)
            .await
            .expect("slow garbage deletion cannot hold the path transition")
            .expect("replacement task");
        pause.wait().await;
        pause.wait().await;
        assert_eq!(session.control.current_producer_attempt(), successor);
        assert!(dir.path().join("seg00000.ts").exists());
        assert!(dir.path().join("seg00001.ts").exists());
    }

    #[tokio::test]
    async fn failed_retention_garbage_unlink_cannot_reexpose_a_served_path() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 40, 4.0).await;
        // Built against the manager's own ledger, because the global cap is
        // the ledger now: a fixture with no admission contributes nothing,
        // and this test exists to prove failed physical deletion stays inside
        // the cap.
        let ledger_store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(
            ledger_store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let mut fixture = test_session(dir.path().to_path_buf());
        fixture.scratch = Some(
            manager
                .scratch_ledger
                .reserve(0, HLS_SCRATCH_MAX_BYTES_DEFAULT)
                .expect("fixture admission")
                .bound_to("failed-garbage", 0),
        );
        let session = Arc::new(fixture);
        session.refresh_segments().await;
        session.fetched_end_ms.store(300_000, Relaxed);
        expire_seeded_prefix(&session, 30).await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .retention_delete_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));

        gc_expired_segments(&session).await;
        pause.wait().await;
        let garbage = session
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .first()
            .map(|(path, _)| path.clone())
            .expect("handed-off garbage path");
        tokio::fs::remove_file(&garbage)
            .await
            .expect("replace garbage file");
        tokio::fs::create_dir(&garbage)
            .await
            .expect("force remove_file failure");
        pause.wait().await;
        pause.wait().await;

        assert!(!dir.path().join("seg00000.ts").exists());
        assert!(session
            .segments
            .lock()
            .await
            .segs
            .first()
            .is_some_and(|segment| {
                segment.visibility == SegmentVisibility::Deleted && segment.bytes == 0
            }));
        assert!(
            tokio::fs::metadata(&garbage)
                .await
                .is_ok_and(|metadata| metadata.is_dir()),
            "cleanup failure remains confined to an unservable garbage name"
        );
        let retained_bytes = 10 * 1_024;
        let charged_garbage = session.retention_garbage_bytes.load(Acquire);
        assert_eq!(
            charged_garbage, 1_024,
            "only the independently failed item remains charged"
        );
        assert_eq!(
            session.live_bytes.load(Acquire),
            retained_bytes + charged_garbage,
            "failed physical deletion remains included in the hard scratch cap"
        );
        manager
            .sessions
            .lock()
            .await
            .insert("failed-garbage".into(), Arc::clone(&session));
        assert_eq!(
            manager.global_flow_bytes().await.0,
            retained_bytes + charged_garbage,
            "the global hard cap includes failed detached cleanup"
        );
        assert!(!session.retention_cleanup_active.load(Acquire));
        assert_eq!(
            session
                .retention_cleanup_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1,
            "the failed item remains retryable without stranding the tail"
        );
        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("session dir");
        let mut hidden_garbage = 0;
        while let Some(entry) = entries.next_entry().await.expect("directory entry") {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".plurx-retention-")
            {
                hidden_garbage += 1;
            }
        }
        assert_eq!(
            hidden_garbage, 1,
            "the worker attempts and removes every independent tail entry"
        );

        tokio::fs::remove_dir(&garbage)
            .await
            .expect("release failed-cleanup fixture");
        gc_expired_segments(&session).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while session.retention_cleanup_active.load(Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued retention retry");
        assert!(session
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty());
        assert_eq!(session.retention_garbage_bytes.load(Acquire), 0);
        assert_eq!(session.live_bytes.load(Acquire), retained_bytes);
        assert_eq!(manager.global_flow_bytes().await.0, retained_bytes);
    }

    /// Retention is measured back from the DOWNLOAD frontier and must leave a
    /// reload margin behind the observed playhead. This asserts the segment set
    /// that actually survives pruning, rather than only restating the constants
    /// used to calculate the window.
    #[tokio::test]
    async fn retention_keeps_a_reload_margin_above_the_observed_fetch_lead() {
        let dir = crate::test_tempdir().expect("tempdir");
        let p = dir.path();
        // 100 segments of 4s = 400s of media.
        seeded_session_dir(p, 100, 4.0).await;
        tokio::fs::write(p.join("init.mp4"), b"i")
            .await
            .expect("write init");

        let session = Arc::new(test_session(p.to_path_buf()));
        session.refresh_segments().await;
        assert_eq!(
            session.segments.lock().await.produced_playable_end_ms(),
            Some(400_000)
        );

        // Physical-iPad regression: AVPlayer fetched through 300s while its
        // playhead was around 180s, despite a 60s preferred forward buffer.
        // The retained window must still leave 60s behind that playhead for
        // back-buffering and a retry.
        let observed_playhead_ms = 180_000;
        let observed_fetched_end_ms = 300_000;
        let required_reload_margin_ms = 60_000;
        session
            .fetched_end_ms
            .store(observed_fetched_end_ms, Relaxed);
        accept_rolling_publication_demand(
            &session,
            1,
            observed_playhead_ms,
            1.0,
            crate::playback_control::PlaybackDemand::Active,
            crate::playback_control::RenderState::Rendering,
        )
        .await;
        assert!(
            commit_rolling_publication_media(&session, 74, observed_fetched_end_ms).await,
            "download frontier must reach the publication actor"
        );
        expire_seeded_prefix(&session, 30).await;
        gc_expired_segments(&session).await;
        // This fixture expires files directly instead of advancing them
        // through the publication transition, so mirror the logical retention
        // boundary that production installs before selecting a new snapshot.
        session.publication.lock().await.retention_first_segment = Some(30);
        session
            .publication_cycle_at(
                "retained-window",
                Instant::now() + ROLLING_PUBLICATION_TARGET,
            )
            .await
            .expect("retention boundary reaches the served snapshot");

        // The retained/pruned segment boundary is the behavior under test.
        // Segment 30 begins at 120s, leaving 60s behind the observed 180s
        // playhead even though the download frontier had reached 300s.
        let (first_retained, retained_start_ms) = {
            let index = session.segments.lock().await;
            let first = index.first_retained_index().expect("retained segment");
            let (start, _) = index.window_ms_of(first).expect("retained window");
            (first, start)
        };
        assert_eq!(first_retained, 30);
        assert_eq!(
            observed_playhead_ms - retained_start_ms,
            required_reload_margin_ms,
            "the actual retained set leaves the required reload margin"
        );
        assert!(!p.join("seg00029.ts").exists(), "just before the boundary");
        assert!(p.join("seg00030.ts").exists(), "retention boundary");
        assert!(p.join("seg00074.ts").exists(), "just inside the window");
        // …and only what is older goes.
        assert!(!p.join("seg00000.ts").exists());
        assert!(!p.join("seg00010.ts").exists());
        assert!(p.join("init.mp4").exists(), "init is never a segment");

        // The writer's history remains complete for duration accounting, but
        // the manager's HTTP-facing view advances past every deleted URI.
        let raw = tokio::fs::read_to_string(p.join("index.m3u8"))
            .await
            .expect("raw playlist");
        assert!(raw.contains("#EXT-X-PLAYLIST-TYPE:EVENT"), "{raw}");
        assert!(raw.contains("seg00000.ts"), "{raw}");

        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            p.join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        mgr.sessions
            .lock()
            .await
            .insert("retained-window".into(), Arc::clone(&session));
        let (served, playlist_owner) = mgr
            .playlist_with_owner("retained-window")
            .await
            .map_err(|error| error.error)
            .expect("served playlist");
        let served = String::from_utf8(served).expect("playlist utf8");
        assert!(served.contains("#EXT-X-MEDIA-SEQUENCE:30"), "{served}");
        assert!(!served.contains("#EXT-X-PLAYLIST-TYPE:EVENT"), "{served}");
        assert!(!served.contains("seg00029.ts"), "{served}");
        assert!(served.contains("seg00030.ts"), "{served}");
        for name in served.lines().filter(|line| line.ends_with(".ts")) {
            assert!(p.join(name).exists(), "served segment must exist: {name}");
        }
        assert!(
            mgr.commit_resolved_media(
                "retained-window",
                &playlist_owner,
                "playlist",
                Some("index.m3u8"),
                true,
            )
            .await,
            "the successful playlist delivery commits its exact owner"
        );
        let before_miss = session.control.snapshot().await.expect("rolling actor");
        assert_eq!(before_miss.last_renewal_kind, "playlist");

        assert!(mgr
            .segment("retained-window", "seg00029.ts")
            .await
            .expect("pruned request")
            .is_none());
        let after_miss = session.control.snapshot().await.expect("rolling actor");
        assert_eq!(
            after_miss.last_renewal_kind, "playlist",
            "a pruned media miss must not change the playback lease source"
        );
        assert!(
            after_miss.idle_for >= before_miss.idle_for,
            "a pruned media miss must not renew the playback lease"
        );

        // Subtitle timing still sees the discarded prefix through the full
        // internal index: segment 30 begins at 120 seconds, not at zero.
        assert_eq!(
            mgr.segment_window("retained-window", 30).await,
            Some((120.0, 124.0))
        );

        // A frontier that has not yet passed the window prunes nothing.
        let fresh = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(fresh.path(), 10, 4.0).await;
        let young = test_session(fresh.path().to_path_buf());
        young.refresh_segments().await;
        young.fetched_end_ms.store(40_000, Relaxed);
        gc_expired_segments(&young).await;
        assert!(fresh.path().join("seg00000.ts").exists());
    }

    /// The reserve in bytes has to shrink when the bytes stop existing, or the
    /// budget would hold a session for scratch it already reclaimed.
    #[tokio::test]
    async fn pruned_bytes_stop_counting_toward_the_budget() {
        let dir = crate::test_tempdir().expect("tempdir");
        seeded_session_dir(dir.path(), 100, 4.0).await;
        let session = test_session(dir.path().to_path_buf());
        session.refresh_segments().await;
        session.fetched_end_ms.store(300_000, Relaxed);
        expire_seeded_prefix(&session, 30).await;

        let before = session.ahead().await.expect("published").bytes;
        gc_expired_segments(&session).await;
        let after = session.ahead().await.expect("published").bytes;
        assert_eq!(
            before, after,
            "pruning happens BEHIND the frontier, so the ahead figure is untouched"
        );
        // What did change is total scratch, which a refresh re-measures.
        session.refresh_segments().await;
        let total: i64 = session
            .segments
            .lock()
            .await
            .segs
            .iter()
            .map(|s| s.bytes)
            .sum();
        assert!(total < 100 * 1024, "pruned files no longer count: {total}");
    }

    /// Write a real (tiny) H.264 file, because the fixtures elsewhere are
    /// deliberately garbage bytes and ffmpeg refuses them — which is fine when
    /// the assertion is about bookkeeping, and useless when it is about
    /// telemetry that only exists while ffmpeg is genuinely working.
    fn write_real_video(path: &std::path::Path, seconds: u32) {
        let status = std::process::Command::new(
            std::env::var("PLURX_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()),
        )
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=160x120:rate=15:duration={seconds}"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "15",
            "-y",
        ])
        .arg(path)
        .status();
        assert!(
            status.map(|s| s.success()).unwrap_or(false),
            "fixture encode failed — this test needs a working ffmpeg"
        );
    }

    /// The same, in HEVC.
    ///
    /// The copy path's Dolby Vision branch is inside `copy_video_args`'s
    /// `hevc | h265` arm, so an H.264 fixture never reaches the argv this
    /// exists to read — and a probe row that merely *claims* `hevc` over
    /// H.264 bytes produces an ffmpeg that fails for the wrong reason. 10-bit,
    /// because every Dolby Vision base layer is.
    async fn write_real_hevc_video(path: &std::path::Path, seconds: u32) {
        let mut command = tokio::process::Command::new(
            std::env::var("PLURX_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()),
        );
        command
            .kill_on_drop(true)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=size=160x120:rate=15:duration={seconds}"),
                "-pix_fmt",
                "yuv420p10le",
                "-c:v",
                "libx265",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-x265-params",
                "log-level=none:keyint=15:min-keyint=15:pools=none:frame-threads=1",
                "-tag:v",
                "hvc1",
                "-y",
            ])
            .arg(path);
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
            .await
            .expect("HEVC fixture encode exceeded its 30-second deadline")
            .expect("failed to start HEVC fixture encode");
        assert!(
            output.status.success(),
            "HEVC fixture encode failed — this test needs an ffmpeg with libx265: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The whole point of the telemetry is that it comes from a running
    /// encoder, so this drives one: real input, real `-progress` stream, real
    /// signals. It is the only test that can catch the plumbing being wrong —
    /// a stdout that was never piped, a progress flag ffmpeg ignored, a
    /// SIGSTOP sent to the wrong pid — because every one of those looks
    /// perfectly healthy from the outside.
    #[tokio::test]
    async fn a_client_fetch_advances_publication_frontier_without_legacy_hold() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        use tokio::io::AsyncReadExt as _;

        let media = crate::test_tempdir().expect("media dir");
        let src = media.path().join("clip.mp4");
        write_real_video(&src, 60);

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file_with_probe_at(
            &store,
            &src.to_string_lossy(),
            plurx_core::domain::ProbeResult {
                duration_ms: Some(60_000),
                container: Some("mp4".into()),
                video_codec: Some("h264".into()),
                width: Some(160),
                height: Some(120),
                ..Default::default()
            },
        )
        .await;
        let seeded = store
            .get_file(file_id)
            .await
            .expect("read real fixture probe")
            .expect("real fixture file");
        assert_eq!(seeded.video_codec.as_deref(), Some("h264"));
        assert_eq!((seeded.width, seeded.height), (Some(160), Some(120)));
        let work = crate::test_tempdir().expect("work");
        let mgr = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        // Burst then crawl. The burst gives the session something to be ahead
        // *with* straight away; the realtime pace afterwards keeps it alive for
        // the rest of the test, because an unpaced copy of a short clip
        // finishes before there is anything left to suspend.
        store.put_setting(keys::HLS_READRATE, "1").await.expect("s");
        store
            .put_setting(keys::HLS_BURST_SECS, "20")
            .await
            .expect("s");
        store
            .put_setting(keys::HLS_AHEAD_MAX_SECS, "1")
            .await
            .expect("s");
        store
            .put_setting(keys::HLS_AHEAD_MAX_BYTES, "0")
            .await
            .expect("s");
        store
            .put_setting(keys::HLS_SCRATCH_MAX_BYTES, "0")
            .await
            .expect("s");

        let info = mgr
            .start_copy(
                file_id,
                0.0,
                None,
                CopySessionOptions {
                    convert_dolby_vision: false,
                    transcode_audio: false,
                    preserve_dolby_vision: false,
                },
                "paul",
                "pb-paul",
            )
            .await
            .expect("copy session");
        let session = mgr
            .sessions
            .lock()
            .await
            .get(&info.session_id)
            .cloned()
            .expect("session is tracked");

        // Telemetry arrives from the real progress stream, and advances. (The
        // first block can legitimately read zero — a frame at PTS 0 — so this
        // waits for movement, not merely for a number.)
        let produced = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match session.progress.out_time_ms() {
                    Some(ms) if ms >= 3_000 => return ms,
                    _ if session.failed.load(Acquire) => {
                        panic!(
                            "ffmpeg failed before its output timeline advanced: {:?}",
                            session.failure_reason()
                        );
                    }
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("ffmpeg reported an advancing output timeline");
        assert!(produced >= 3_000);
        assert!(
            session.progress.speed().is_some(),
            "the speed field parses too — it is what tells slow from stuck"
        );

        // Nothing has been fetched, so everything published is reserve. This
        // reads the index (what the playlist says exists) rather than the
        // encoder clock, so it also proves the parser sees real ffmpeg output.
        session.refresh_segments().await;

        // When the burst is not honoured (ffmpeg 8 declares it but ignores
        // it), the publish gate fills at the paced rate rather than at I/O
        // speed and the ahead window is empty after only 3 s of progress.
        // Skip the suspend-resume cycle that requires a populated reserve;
        // the progress and speed checks above still validate the encoder.
        let burst_honoured = pacing_caps().await.initial_burst;
        let ffmpeg_build = crate::ffmpeg::ffmpeg_build().await;
        if !burst_honoured {
            eprintln!(
                "INFO: {ffmpeg_build} does not honour -readrate_initial_burst: \
                 skipping ahead and suspend-resume assertions that require \
                 burst-published reserve segments"
            );
        }
        if burst_honoured {
            assert!(
                session
                    .ahead()
                    .await
                    .is_some_and(|a| a.seconds > 0 && a.bytes > 0),
                "published media with an unmoved frontier is reserve; measured {ffmpeg_build}"
            );

            // Publish the playlist through the same owner/authorization/EOF
            // transaction as HTTP. The first-media handoff must happen here;
            // neither a scratch-file refresh nor a later segment read may
            // infer that a player has observed this generation.
            let (playlist_bytes, playlist_owner) =
                match mgr.playlist_with_owner(&info.session_id).await {
                    Ok(publication) => publication,
                    Err(_) => panic!("playlist publication failed"),
                };
            let playlist = std::str::from_utf8(&playlist_bytes).expect("UTF-8 playlist");
            let newest = playlist
                .lines()
                .map(str::trim)
                .rfind(|line| is_safe_segment(line) && segment_index(line).is_some())
                .expect("advertised media segment")
                .to_owned();
            let playlist_deadline = Instant::now() + Duration::from_secs(5);
            let playlist_authorization = mgr
                .authorize_response_publication(
                    &info.session_id,
                    &playlist_owner,
                    MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
                    playlist_deadline,
                )
                .await
                .expect("playlist response admission");
            mgr.commit_authorized_media(playlist_authorization, true, playlist_deadline)
                .await
                .expect("playlist response EOF commit");
            let actor_delivery = session
                .control
                .snapshot()
                .await
                .expect("rolling actor")
                .delivery;
            assert!(actor_delivery.playlist_ready);
            assert!(session.first_media_handoff_applied.load(Acquire));
            assert!(!session.actor_prepublication_producer.load(Acquire));

            // Publication-clock pacing does not turn a freshly served window
            // into the old inferred-ahead hold. The encoder remains runnable
            // until a complete next batch is staged.
            mgr.flow_control(&session, &info.session_id).await;
            assert!(!session.suspended.load(Relaxed));
            let status = mgr.session_status(&info.session_id).await.expect("status");
            assert_eq!(status.readrate, 1.0, "HLS exposes its effective input pace");
            assert_eq!(status.production_policy, "publication_clock_legacy");
            assert_eq!(status.hold_reason, None);
            assert_eq!(status.resume_below_bytes, None);
            assert_eq!(status.suspend_count, 0);
            // Fetch the newest published segment through the real request path.
            // That advances the actor-owned download frontier without creating
            // a legacy inferred-ahead suspend/resume transition.
            let opened = match mgr
                .segment_for_publication(&info.session_id, &newest)
                .await
                .expect("segment resolution")
            {
                SegmentPublication::Ready(opened) => *opened,
                _ => panic!("advertised segment was not ready"),
            };
            let segment_owner = opened.response_owner();
            let segment_authorization_deadline = Instant::now() + Duration::from_secs(5);
            let segment_authorization = mgr
                .authorize_response_publication(
                    &info.session_id,
                    &segment_owner,
                    MediaResponsePublication::attempt_media("media-segment", Some(&newest)),
                    segment_authorization_deadline,
                )
                .await
                .expect("segment response admission");
            let SegmentFile {
                mut file,
                len,
                mut delivery,
            } = opened;
            let started = Instant::now();
            let mut bytes = Vec::with_capacity(usize::try_from(len).expect("segment length"));
            file.read_to_end(&mut bytes)
                .await
                .expect("read advertised segment");
            delivery.note_read(bytes.len() as u64, started.elapsed());
            assert_eq!(bytes.len() as u64, len, "read through advertised EOF");
            assert!(delivery.finish(), "segment delivery reached exact EOF");
            let segment_completion_deadline = Instant::now() + Duration::from_secs(5);
            mgr.commit_authorized_media(segment_authorization, true, segment_completion_deadline)
                .await
                .expect("segment response EOF commit");
            let fetched_index = segment_index(&newest).expect("numbered segment");
            let fetched_end_ms = session
                .segments
                .lock()
                .await
                .end_ms_of(fetched_index)
                .expect("published segment duration");
            let actor_delivery = session
                .control
                .snapshot()
                .await
                .expect("rolling actor")
                .delivery;
            assert_eq!(actor_delivery.fetched_segment, Some(fetched_index));
            assert_eq!(actor_delivery.fetched_end_ms, fetched_end_ms);
            mgr.flow_control(&session, &info.session_id).await;
            let actor_delivery = session
                .control
                .snapshot()
                .await
                .expect("rolling actor")
                .delivery;
            assert!(actor_delivery.playlist_ready);
            assert!(actor_delivery.published_segment >= Some(fetched_index));
            assert!(actor_delivery.published_end_ms >= Some(fetched_end_ms));
            assert!(!session.suspended.load(Relaxed), "session was released");
            assert_eq!(
                mgr.session_status(&info.session_id)
                    .await
                    .expect("status after fetch")
                    .suspend_count,
                0,
            );
        }
        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert_eq!(mgr.active_sessions().await, 0);
    }

    include!("../../vodencode_manager_tests.rs");

    async fn seed_file(store: &Arc<dyn Store>) -> i64 {
        seed_file_at(store, "/media/Heat.mkv").await
    }

    async fn seed_file_at(store: &Arc<dyn Store>, path: &str) -> i64 {
        seed_file_with_probe_at(
            store,
            path,
            plurx_core::domain::ProbeResult {
                duration_ms: Some(6_000_000),
                container: Some("mkv".into()),
                video_codec: Some("hevc".into()),
                width: Some(3840),
                height: Some(2160),
                ..Default::default()
            },
        )
        .await
    }

    async fn seed_file_with_probe_at(
        store: &Arc<dyn Store>,
        path: &str,
        probe: plurx_core::domain::ProbeResult,
    ) -> i64 {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
        let lib = store
            .create_library(&NewLibrary {
                name: "L".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("lib");
        let movie = store
            .insert_item(&NewItem {
                library_id: lib.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Heat".into(),
                year: Some(1995),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        store
            .upsert_file(movie, path, 1, 1, &probe)
            .await
            .expect("file")
    }

    /// A background process that ignores the yield signal cannot turn a Play
    /// request into an unbounded spinner. The production path uses five
    /// seconds; this focused test supplies a small window to prove the same
    /// deadline and error classification without sleeping for the full one.
    #[tokio::test]
    async fn live_admission_fails_retryably_when_background_does_not_release() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work_dir = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            store,
            work_dir.path().to_path_buf(),
            EncoderCaps {
                nvenc: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        );
        let producer = mgr
            .admissions
            .try_acquire(2, Priority::Background)
            .expect("background permit");
        let wait = Duration::from_millis(25);
        let began = Instant::now();
        let error = match mgr
            .admit_live(
                Encoder::Nvenc,
                None,
                Workload {
                    source_height: 1080,
                    codec: "h264",
                    hdr: None,
                    target_height: 720,
                },
                wait,
                Priority::Live,
            )
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("live admission overlapped a stuck background permit"),
        };
        let elapsed = began.elapsed();
        assert!(elapsed >= wait, "returned before the configured window");
        assert!(
            elapsed < Duration::from_millis(150),
            "polling overshot the bounded window: {elapsed:?}"
        );
        assert!(is_retryable_capacity_error(&error), "{error}");
        assert!(
            error.contains("background encoding did not yield"),
            "{error}"
        );
        assert!(
            !mgr.admissions.live_is_waiting(),
            "the failed request leaked its waiter"
        );
        drop(producer);
        assert_eq!(mgr.admissions.in_use(), 0, "the background guard returned");
    }

    /// Cancelling the HTTP request while it is waiting must withdraw the
    /// yield signal. Otherwise one abandoned Play request parks every future
    /// producer even though no live encoder owns capacity.
    #[tokio::test]
    async fn aborting_a_waiting_live_admission_releases_its_yield_signal() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work_dir = crate::test_tempdir().expect("work");
        let mgr = Arc::new(TranscodeManager::new(
            store,
            work_dir.path().to_path_buf(),
            EncoderCaps {
                nvenc: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        ));
        let producer = mgr
            .admissions
            .try_acquire(2, Priority::Background)
            .expect("background permit");
        let waiting = {
            let mgr = Arc::clone(&mgr);
            tokio::spawn(async move {
                mgr.admit_live(
                    Encoder::Nvenc,
                    None,
                    Workload {
                        source_height: 1080,
                        codec: "h264",
                        hdr: None,
                        target_height: 720,
                    },
                    Duration::from_secs(10),
                    Priority::Live,
                )
                .await
            })
        };

        let deadline = Instant::now() + Duration::from_secs(1);
        while !mgr.admissions.live_is_waiting() && Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
        assert!(
            mgr.admissions.live_is_waiting(),
            "the request never registered its yield signal"
        );
        waiting.abort();
        let cancelled = waiting.await;
        assert!(
            matches!(cancelled, Err(error) if error.is_cancelled()),
            "the admission task was not cancelled"
        );
        assert!(
            !mgr.admissions.live_is_waiting(),
            "the cancelled request leaked its yield signal"
        );
        assert_eq!(mgr.admissions.in_use(), 1, "only the producer remains");
        assert_eq!(mgr.admissions.software_in_use(), 0);
        drop(producer);
        assert_eq!(mgr.admissions.in_use(), 0, "all permits returned");
    }

    /// A start owns its permit before it creates scratch or spawns ffmpeg, so
    /// every error in that interval must return admission automatically. A
    /// file where the work directory belongs makes the first post-admission
    /// filesystem operation fail deterministically.
    #[tokio::test]
    async fn a_post_admission_start_failure_returns_every_permit() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let root = crate::test_tempdir().expect("root");
        let not_a_directory = root.path().join("not-a-directory");
        tokio::fs::write(&not_a_directory, b"occupied")
            .await
            .expect("sentinel file");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            not_a_directory,
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        assert_eq!(
            mgr.codec_qualification_encoder_count(Encoder::Software, OutputGrade::Sdr),
            0
        );

        let error = match mgr
            .start(file_id, 720, 0.0, None, None, "paul", "pb-failed-start")
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("the invalid work root must fail after admission"),
        };
        assert!(error.contains("creating session dir"), "{error}");
        assert_eq!(mgr.active_sessions().await, 0);
        assert!(!mgr.admissions.live_is_waiting());
        assert_eq!(mgr.admissions.in_use(), 0);
        assert_eq!(
            mgr.codec_qualification_encoder_count(Encoder::Software, OutputGrade::Sdr),
            0,
            "a start that failed before manager registration was counted"
        );
        assert_eq!(mgr.codec_qualification_pipeline_count(Pipeline::Cpu), 0);
        assert_eq!(
            mgr.admissions.software_in_use(),
            0,
            "the failed start leaked its software permit"
        );
    }

    /// The cap has to hold against *concurrent* starts, which is the only case
    /// that matters: a third 4K session admitted alongside two others does not
    /// run a third as fast, it drags all three under realtime. A count read and
    /// then written would let every racer through.
    #[tokio::test]
    async fn concurrent_starts_cannot_exceed_the_hardware_slot_cap() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        // A box that believes it has NVENC. Keep the returned permits alive
        // instead of spawning an encoder that immediately fails on a runner
        // without a GPU; the cap concerns simultaneous ownership, not how many
        // failed producers can pass through the same slots over five seconds.
        let mgr = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps {
                nvenc: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        ));
        store
            .put_setting(keys::MAX_HW_SESSIONS, "2")
            .await
            .expect("cap");

        let workload = Workload {
            source_height: 2160,
            codec: "hevc",
            hdr: None,
            target_height: 1080,
        };
        let mut starts = Vec::new();
        for _ in 0..5 {
            let mgr = Arc::clone(&mgr);
            starts.push(tokio::spawn(async move {
                mgr.admit_live(
                    Encoder::Nvenc,
                    None,
                    workload,
                    Duration::ZERO,
                    Priority::Live,
                )
                .await
            }));
        }
        let mut admitted = Vec::new();
        let mut refused = 0;
        for s in starts {
            match s.await.expect("join") {
                Ok(admission) => {
                    assert_eq!(admission.encoder, Encoder::Nvenc);
                    assert!(admission.hw_slot.is_some());
                    assert!(admission.sw_permit.is_none());
                    admitted.push(admission);
                }
                Err(why) => {
                    assert!(why.contains("hardware transcode slots"), "{why}");
                    refused += 1;
                }
            }
        }
        assert_eq!(admitted.len(), 2, "the cap is the cap");
        assert_eq!(refused, 3);
        assert_eq!(mgr.admissions.in_use(), 2, "and it is accounted for");

        drop(admitted);
        assert_eq!(mgr.admissions.in_use(), 0, "slots come back");
    }

    /// The other transition, and the one that had no accounting at all: a
    /// retry that keeps its hardware encoder and moves the rest of the chain
    /// onto the CPU.
    ///
    /// It must not go through the demotion path. Releasing the slot here would
    /// leave a live hardware encoder running against nothing, and the next
    /// hardware start would be admitted onto the same video block — one slot
    /// authorizing two encoders, which is the contention the cap exists to
    /// prevent. What it must do instead is reserve the CPU the pipeline has
    /// started spending, which before this milestone it did not do at all.
    #[tokio::test]
    async fn a_retry_that_keeps_its_encoder_keeps_its_slot_and_pays_for_its_decode() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps {
                nvenc: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        );
        store
            .put_setting(keys::MAX_HW_SESSIONS, "1")
            .await
            .expect("cap");
        assert_eq!(
            mgr.codec_qualification_encoder_count(Encoder::Nvenc, OutputGrade::Sdr),
            0
        );

        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-mixed")
            .await
            .expect("hardware start");
        assert_eq!(
            mgr.codec_qualification_encoder_count(Encoder::Nvenc, OutputGrade::Sdr),
            1,
            "one manager-registered rolling start must count once"
        );
        assert_eq!(mgr.codec_qualification_pipeline_count(Pipeline::Cpu), 1);
        assert_eq!(mgr.admissions.in_use(), 1, "the start holds the only slot");
        let session = mgr
            .sessions
            .lock()
            .await
            .get(&info.session_id)
            .cloned()
            .expect("session");
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let work_class = Workload::of(&file, session.target_height);

        // This start already reserved CPU at admission, because its renderer
        // is the CPU chain — which is the point of the estimate. A recovery
        // that moves more of the pipeline onto the CPU therefore owes the
        // *difference*, and here there is none.
        let admitted = session.software_threads_held();
        assert_eq!(
            admitted,
            work_class.software_threads(),
            "the CPU this pipeline spends was reserved when it was admitted"
        );
        assert_eq!(mgr.admissions.software_in_use(), admitted);

        // A retry whose total equals what is already held asks for nothing.
        assert_eq!(
            work_class.software_threads().checked_sub(admitted),
            Some(0),
            "no delta is owed, so the recovery reserves nothing further"
        );

        // And when it does owe one, the second reservation is additive rather
        // than a replacement — replacing would drop the first and leave the
        // session paying for both to own one.
        let permit = mgr
            .admissions
            .software_pool()
            .try_take_delta(mgr.software_budget().await + admitted, 2)
            .expect("an idle pool grants a two-thread delta");
        session.add_cpu_decode_reservation(permit);
        assert_eq!(
            session.software_threads_held(),
            admitted + 2,
            "the delta adds to the reservation instead of replacing it"
        );
        assert_eq!(mgr.admissions.software_in_use(), admitted + 2);

        assert_eq!(
            mgr.admissions.in_use(),
            1,
            "the encoder is still running, so its slot is still held"
        );
        // With the cap at one and this session still encoding on hardware, no
        // second hardware start may be admitted.
        assert!(
            !matches!(
                mgr.admissions
                    .admit(1, Workload::of(&file, session.target_height)),
                Admission::Hardware(_)
            ),
            "one slot authorized a second encoder while the first still runs"
        );
        assert!(mgr.stop_session(&info.session_id, "test").await);
    }

    fn execution_file_for_retry() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 91,
            item_id: 3,
            path: PathBuf::from("/media/retry.mkv"),
            size: 12_345,
            mtime: 67_890,
            duration_ms: Some(7_200_000),
            container: Some("matroska".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(20_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn execution_options_for_retry() -> TranscodeOptions {
        TranscodeOptions {
            target_height: 1080,
            video_bitrate_kbps: 8_000,
            effective_rate_control: EffectiveRateControl::Vbr,
            audio_channels: 2,
            audio_bitrate_kbps: 160,
            audio_index: None,
            tone_map: ToneMap::Zscale,
            pipeline: Pipeline::Cpu,
            ..TranscodeOptions::default()
        }
    }

    /// The identity reaches the session, and an absent budget is not an
    /// unspent one.
    ///
    /// This is the whole of M5c3's first slice, and the distinction in the
    /// second half is the one everything after it rests on. Three starts reach
    /// the daemon without a server-minted epoch — a legacy process-local
    /// start, a relayed worker start, and a cached serve — and the ledger
    /// refuses an empty epoch. A caller that read "no epoch" as "a fresh
    /// budget" would grant one automatic recovery per attempt on exactly the
    /// paths that have no durable bound at all, which is the failure the
    /// budget exists to prevent.
    #[tokio::test]
    async fn a_session_without_a_server_minted_epoch_reports_no_budget_rather_than_a_fresh_one() {
        let dir = crate::test_tempdir().expect("dir");
        let mut session = test_session(dir.path().to_path_buf());

        assert!(
            session.recovery_identity().is_none(),
            "a session with no identity at all has no budget"
        );

        session.recovery = Some(SessionRecoveryIdentity {
            user_id: 7,
            incarnation_id: "incarnation".to_owned(),
            recovery_epoch: String::new(),
        });
        assert!(
            session.recovery_identity().is_none(),
            "an empty epoch is what the legacy and relayed starts carry, and the \
             ledger refuses it — so it is no budget, not an unspent one"
        );

        // Each field the store refuses, refused here too. A `Some` returned on
        // the strength of the epoch alone is a reservation that fails at the
        // moment it is needed, and this method is the one place a caller is
        // entitled to trust.
        session.recovery = Some(SessionRecoveryIdentity {
            user_id: 0,
            incarnation_id: "incarnation".to_owned(),
            recovery_epoch: "epoch-1".to_owned(),
        });
        assert!(
            session.recovery_identity().is_none(),
            "the store refuses a non-positive user id"
        );
        session.recovery = Some(SessionRecoveryIdentity {
            user_id: 7,
            incarnation_id: String::new(),
            recovery_epoch: "epoch-1".to_owned(),
        });
        assert!(
            session.recovery_identity().is_none(),
            "the store refuses an empty failed incarnation"
        );

        session.recovery = Some(SessionRecoveryIdentity {
            user_id: 7,
            incarnation_id: "incarnation".to_owned(),
            recovery_epoch: "epoch-1".to_owned(),
        });
        let identity = session
            .recovery_identity()
            .expect("a server-minted epoch is a budget");
        assert_eq!(identity.user_id, 7);
        assert_eq!(identity.incarnation_id, "incarnation");
        assert_eq!(identity.recovery_epoch, "epoch-1");
    }

    /// The alternate M5c installs, frozen at session start like the one beside
    /// it, and different from it in exactly one way.
    ///
    /// A decode fault says nothing about the encoder — the picture was fine
    /// when it arrived, and what failed was reading the source. So the
    /// alternate keeps the encode route and changes only how the source is
    /// read, which is why it goes through `prepare_decode_restricted` rather
    /// than `prepare`, and why `build`'s guard passes on a plan whose decode
    /// backend moved: that guard compares the encode route, and the decode
    /// backend is not part of it.
    #[tokio::test]
    async fn the_software_decode_alternate_is_a_distinct_artifact_that_keeps_its_encoder() {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::{
            ArtifactQualification, AttemptRestrictions, DecodeBackend, DecodeReason,
        };

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        // This test isolates the mixed-resource and recipe shape. The
        // production-policy pairing rule has its own real-policy regression
        // below; force both plans qualified here so this test reaches the
        // shape it is responsible for.
        mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
        let work = Workload::of(&file, 1080);

        let mut opts = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        opts.pipeline = Pipeline::Cpu;

        let delivered = mgr
            .resolve_movie_plan(&file, &opts, Encoder::VideoToolbox)
            .await
            .expect("delivered plan");
        let alternate = mgr
            .resolve_restricted_movie_plan(
                &file,
                &opts,
                Encoder::VideoToolbox,
                &AttemptRestrictions::requiring(DecodeBackend::Software),
            )
            .await
            .expect("software-decode alternate");

        assert_eq!(alternate.decode().backend(), DecodeBackend::Software);
        assert_eq!(
            alternate.decode().reason(),
            DecodeReason::ContinuationRestriction,
            "the plan says why it moved, so a badge cannot report it as a preference"
        );
        assert_eq!(
            alternate.encoder(),
            delivered.encoder(),
            "a decode fault is not a reason to change the encoder"
        );
        assert_ne!(
            alternate.plan_digest(),
            delivered.plan_digest(),
            "the decode backend feeds the plan digest, so the alternate is a different artifact"
        );

        assert_ne!(
            delivered.decode().backend(),
            DecodeBackend::Software,
            "the fixture has to actually be moving the backend, or this test proves nothing"
        );

        // The mixed shape: whatever else changes, the encode keeps its
        // hardware slot. Releasing it would leave a live hardware encoder with
        // nothing reserved for it, and the next hardware start admitted onto
        // the same block.
        let delivered_cost = crate::admission::TranscodeResourceEstimate::of(&delivered, &work);
        let alternate_cost = crate::admission::TranscodeResourceEstimate::of(&alternate, &work);
        assert!(
            alternate_cost.hardware_slot,
            "the encode still holds a slot"
        );
        assert!(
            alternate_cost.cpu_threads >= delivered_cost.cpu_threads,
            "forcing software decode cannot cost less CPU: \
             {delivered_cost:?} then {alternate_cost:?}"
        );
        // And on this fixture it costs exactly the same, which is worth
        // asserting rather than glossing. `TranscodeResourceEstimate::of`
        // reserves the whole pipeline's software estimate whenever *any* stage
        // runs on the CPU, and `Pipeline::Cpu` already puts the filters there
        // — so the session was already paying for these cores before the
        // decode moved. That is why the mixed transition takes a *delta* from
        // `software_threads_held()` and not a fresh whole estimate: on a
        // CPU-filtered route the delta is zero and the retry must reserve
        // nothing rather than pay twice to end up owning once.
        assert_eq!(
            alternate_cost.cpu_threads, delivered_cost.cpu_threads,
            "a CPU-filtered route already reserved the cores the decode now uses"
        );

        // And the recipe built from it is the mixed transition rather than a
        // demotion: no software thread claim, so nothing hands the hardware
        // slot back, and a total for the CPU delta to be taken from.
        let prepared = PrepublicationTranscodeRetry::prepare_decode_restricted(
            &delivered,
            &alternate,
            &opts,
            Encoder::VideoToolbox,
        )
        .expect("a legal alternate")
        .expect("an alternate exists for a hardware-decoded delivery");
        assert_eq!(
            prepared.software_threads, None,
            "a Some here would demote and release the slot the encoder is using"
        );
        assert_eq!(
            prepared.opts.pipeline,
            alternate.options().pipeline,
            "the pipeline comes from the plan, because forcing software decode \
             can rewrite the renderer and the guard compares the two"
        );
        let dir = std::env::temp_dir().join(format!("plurx-m5c2b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let recipe = PrepublicationTranscodeRetry::build(
            &file,
            prepared,
            &alternate,
            Pacing::unpaced(),
            &dir,
            "presentation-m5c2b",
            mgr.admissions.software_pool(),
            mgr.software_budget().await,
            mgr.runtime_cache.clone(),
            &mgr.measured_decoders,
            false,
            "software-decode",
        )
        .expect("the alternate is a legal retry for this encode route");
        assert_eq!(
            recipe.actor_recipe.startup_kind,
            crate::playback_control::ProducerStartupKind::MixedSoftwareDecode,
        );
        assert!(
            recipe.cpu_total.is_some(),
            "the mixed transition needs a total to take its delta from"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recovery construction follows the direct switch rather than artifact
    /// qualification. Coverage can strengthen both plans without deciding
    /// whether the software-decode alternate exists.
    #[tokio::test]
    async fn automatic_decode_recovery_does_not_require_a_qualified_pair() {
        use plurx_core::store::keys::DECODER_HEALTH_QUALIFIED_ARTIFACTS;
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::decoder_inventory::MeasuredDecoders;
        use plurx_core::transcode::{ArtifactQualification, AttemptRestrictions, DecodeBackend};

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        store
            .put_setting(DECODER_HEALTH_QUALIFIED_ARTIFACTS, "1")
            .await
            .expect("enable path-scoped policy");
        let measured = MeasuredDecoders::from_measured(&[
            ("hevc", DecodeBackend::Software, "hevc"),
            ("hevc", DecodeBackend::VideoToolbox, "hevc"),
        ]);
        let contract = |id: &str, backend: DecodeBackend| {
            let mut contract = decode_contract_fixture(backend);
            contract.id = id.to_owned();
            contract.input_codec = "hevc".to_owned();
            contract.decoder = "hevc".to_owned();
            contract
        };
        let build = || crate::decoder_health::MeasuredBuild {
            ffmpeg_version: "9.0.1".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
        };
        let options = |mgr: &TranscodeManager| {
            let mut opts = mgr.options_for_tone_map(
                Encoder::Software,
                &file,
                1080,
                0.0,
                None,
                None,
                None,
                tone_map_pref(),
                OutputGrade::Sdr,
            );
            opts.pipeline = Pipeline::Cpu;
            opts
        };

        let (mut hardware_only, _work, _cache) = cached_manager(&store);
        hardware_only.caps.videotoolbox = true;
        let hardware_only = hardware_only
            .with_decoders(vec!["hevc".to_owned()])
            .with_measured_decoders(measured.clone())
            .with_diagnostic_policy(Arc::new(crate::decoder_health::DiagnosticPolicy::new(
                Some(build()),
                vec![contract("hardware", DecodeBackend::VideoToolbox)],
            )));
        let published = hardware_only.publish_artifact_qualification().await;
        assert!(published.requested, "the operator policy remains enabled");
        assert_eq!(
            published.effective,
            ArtifactQualification::Unqualified,
            "legacy whole-node state stays conservative for a partial pair"
        );
        let opts = options(&hardware_only);
        let delivered = hardware_only
            .resolve_movie_plan(&file, &opts, Encoder::VideoToolbox)
            .await
            .expect("covered hardware delivery");
        let uncovered_software = hardware_only
            .resolve_restricted_movie_plan(
                &file,
                &opts,
                Encoder::VideoToolbox,
                &AttemptRestrictions::requiring(DecodeBackend::Software),
            )
            .await
            .expect("uncovered software plan");
        assert!(delivered.enforces_receipt());
        assert!(!uncovered_software.enforces_receipt());
        assert!(
            PrepublicationTranscodeRetry::prepare_decode_restricted(
                &delivered,
                &uncovered_software,
                &opts,
                Encoder::VideoToolbox,
            )
            .expect("the unqualified successor remains structurally safe")
            .is_some(),
            "missing software-path qualification must not gate the enabled recovery"
        );

        let (mut paired, _paired_work, _paired_cache) = cached_manager(&store);
        paired.caps.videotoolbox = true;
        let paired = paired
            .with_decoders(vec!["hevc".to_owned()])
            .with_measured_decoders(measured)
            .with_diagnostic_policy(Arc::new(crate::decoder_health::DiagnosticPolicy::new(
                Some(build()),
                vec![
                    contract("hardware", DecodeBackend::VideoToolbox),
                    contract("software", DecodeBackend::Software),
                ],
            )));
        let published = paired.publish_artifact_qualification().await;
        assert_eq!(
            published.effective,
            ArtifactQualification::HealthQualified,
            "both selectable paths are now covered"
        );
        let opts = options(&paired);
        let delivered = paired
            .resolve_movie_plan(&file, &opts, Encoder::VideoToolbox)
            .await
            .expect("qualified hardware delivery");
        let software = paired
            .resolve_restricted_movie_plan(
                &file,
                &opts,
                Encoder::VideoToolbox,
                &AttemptRestrictions::requiring(DecodeBackend::Software),
            )
            .await
            .expect("qualified software plan");
        assert!(delivered.enforces_receipt() && software.enforces_receipt());
        let prepared = PrepublicationTranscodeRetry::prepare_decode_restricted(
            &delivered,
            &software,
            &opts,
            Encoder::VideoToolbox,
        )
        .expect("the covered pair is compatible")
        .expect("the covered pair has an automatic alternate");
        let dir = std::env::temp_dir().join(format!("plurx-m7b-pair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let alternate = PrepublicationTranscodeRetry::build(
            &file,
            prepared,
            &software,
            Pacing::unpaced(),
            &dir,
            "presentation-m7b-pair",
            paired.admissions.software_pool(),
            paired.software_budget().await,
            paired.runtime_cache.clone(),
            &paired.measured_decoders,
            false,
            "software-decode",
        )
        .expect("qualified alternate recipe");
        let colour_safe = build_test_transcode_retry(
            &paired,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            &dir,
            "presentation-m7b-pair",
        )
        .await
        .expect("colour-safe retry");
        assert!(
            colour_safe
                .with_decode_alternate(Some(alternate))
                .decode_alternate
                .is_some(),
            "paired coverage attaches the alternate the actor may select"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The executor installs the recipe the actor named, and only that one.
    ///
    /// Two recipes are frozen on the same structure and the actor names one of
    /// them in its decision. Assuming the colour-safe one — which is what the
    /// code did before it learned to resolve the name — would respawn the exact
    /// command the decode fault was about, under a reason that told the client
    /// a recovery was under way. Every later use in that function reads the
    /// selected recipe, so getting this wrong is not one wrong field but the
    /// whole attempt: the arguments, the reservation and the diagnostic
    /// grammar.
    #[tokio::test]
    async fn the_executor_installs_the_recipe_the_actor_named() {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::{ArtifactQualification, AttemptRestrictions, DecodeBackend};

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);

        let mut opts = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        opts.pipeline = Pipeline::Cpu;

        let delivered = mgr
            .resolve_movie_plan(&file, &opts, Encoder::VideoToolbox)
            .await
            .expect("delivered plan");
        let alternate_plan = mgr
            .resolve_restricted_movie_plan(
                &file,
                &opts,
                Encoder::VideoToolbox,
                &AttemptRestrictions::requiring(DecodeBackend::Software),
            )
            .await
            .expect("alternate plan");

        let dir = std::env::temp_dir().join(format!("plurx-m5c2d-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let colour_safe = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            &dir,
            "presentation-m5c2d-exec",
        )
        .await
        .expect("colour-safe recipe");
        let alternate = PrepublicationTranscodeRetry::build(
            &file,
            PrepublicationTranscodeRetry::prepare_decode_restricted(
                &delivered,
                &alternate_plan,
                &opts,
                Encoder::VideoToolbox,
            )
            .expect("a legal alternate")
            .expect("an alternate exists"),
            &alternate_plan,
            Pacing::unpaced(),
            &dir,
            "presentation-m5c2d-exec",
            mgr.admissions.software_pool(),
            mgr.software_budget().await,
            mgr.runtime_cache.clone(),
            &mgr.measured_decoders,
            false,
            "software-decode",
        )
        .expect("alternate recipe");

        // The two really are distinguishable, which is what makes the rest of
        // this test mean anything.
        assert_ne!(colour_safe.actor_recipe, alternate.actor_recipe);
        assert_ne!(colour_safe.args, alternate.args);
        assert!(
            alternate
                .actor_recipe
                .identity
                .starts_with("software-decode:"),
            "the identity names what the recipe is for: {}",
            alternate.actor_recipe.identity
        );

        let paired = colour_safe
            .clone()
            .with_decode_alternate(Some(alternate.clone()));
        assert_eq!(
            paired
                .decode_alternate
                .as_deref()
                .map(|held| held.actor_recipe.clone()),
            Some(alternate.actor_recipe.clone()),
            "the executor holds the material the policy advertises"
        );
        // And the pair does not lose the recipe it was built from.
        assert_eq!(paired.actor_recipe, colour_safe.actor_recipe);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// There is no alternate when the delivery already decodes in software,
    /// and saying so is the whole job.
    ///
    /// `prepare` structurally cannot produce a retry identical to the attempt
    /// that just failed — it always moves the pipeline or the encoder. This one
    /// moves neither, so nothing but an explicit refusal stops it from tearing
    /// down the failed child and respawning the identical command: same plan
    /// digest, same arguments, same recipe fingerprint. A recovery that is a
    /// no-op is worse than none, because it spends an attempt and reads in the
    /// ledger as though something was tried.
    #[tokio::test]
    async fn there_is_no_software_decode_alternate_for_a_delivery_already_decoding_in_software() {
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::{AttemptRestrictions, DecodeBackend};

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);

        let mut opts = mgr.options_for_tone_map(
            Encoder::Software,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        opts.pipeline = Pipeline::Cpu;

        // Resolve the delivered plan *under the restriction*, which is the
        // shape of a session whose delivery already decodes in software.
        let already_software = mgr
            .resolve_restricted_movie_plan(
                &file,
                &opts,
                Encoder::VideoToolbox,
                &AttemptRestrictions::requiring(DecodeBackend::Software),
            )
            .await
            .expect("software-decoded delivery");
        assert_eq!(already_software.decode().backend(), DecodeBackend::Software);
        assert!(
            PrepublicationTranscodeRetry::prepare_decode_restricted(
                &already_software,
                &already_software,
                &opts,
                Encoder::VideoToolbox,
            )
            .expect("a legal answer")
            .is_none(),
            "an alternate with the delivered plan's own digest is not an alternate"
        );

        // A software encoder has no hardware slot to keep, so there is no
        // mixed transition to make and every sentence the recipe's doc claims
        // about one would be false.
        let software_plan = mgr
            .resolve_movie_plan(&file, &opts, Encoder::Software)
            .await
            .expect("software plan");
        assert!(
            PrepublicationTranscodeRetry::prepare_decode_restricted(
                &software_plan,
                &already_software,
                &opts,
                Encoder::Software,
            )
            .expect("a legal answer")
            .is_none(),
            "a software encoder is not a mixed transition"
        );
    }
