
    /// The derivation this whole milestone rests on, exercised through a real
    /// resolved plan rather than a hand-built estimate.
    ///
    /// Without this, `TranscodeResourceEstimate::of` has no coverage at all:
    /// every other test constructs the estimate by hand, so `hardware_slot`,
    /// `cpu_threads` and the filter-chain rule are free to change and stay
    /// green.
    #[tokio::test]
    async fn the_estimate_reads_the_cost_off_the_plan_and_not_off_the_encoders_name() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let work = Workload::of(&file, 1080);

        let mut software = mgr.options_for_tone_map(
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
        software.pipeline = Pipeline::Cpu;
        let software_plan = mgr
            .resolve_movie_plan(&file, &software, Encoder::Software)
            .await
            .expect("software plan");
        let estimate = crate::admission::TranscodeResourceEstimate::of(&software_plan, &work);
        assert!(!estimate.hardware_slot, "software encoding needs no slot");
        assert_eq!(estimate.cpu_threads, work.software_threads());
        assert_eq!(
            estimate.decoder_threads, None,
            "no implementation was measured, so no cap is claimed"
        );

        // A hardware encoder whose decode is in software still spends the
        // cores the decode uses. This is the case that reserved nothing at all
        // before this milestone.
        let mut mixed = software.clone();
        mixed.pipeline = Pipeline::Cpu;
        let mixed_plan = mgr
            .resolve_movie_plan(&file, &mixed, Encoder::VideoToolbox)
            .await
            .expect("mixed plan");
        let estimate = crate::admission::TranscodeResourceEstimate::of(&mixed_plan, &work);
        assert!(estimate.hardware_slot, "the encode holds a slot");
        assert!(
            estimate.cpu_threads > 0,
            "and the CPU the rest of the pipeline spends is reserved: {estimate:?}"
        );

        // The two vendor graphs are the only ones that keep every frame off the
        // CPU, and a subtitle burn takes even them back to system memory.
        assert!(Pipeline::VppQsv.keeps_frames_off_the_cpu());
        assert!(Pipeline::TonemapVaapi.keeps_frames_off_the_cpu());
        for cpu_touching in [
            Pipeline::Cpu,
            Pipeline::Libplacebo,
            Pipeline::TonemapOpencl,
            Pipeline::Hdr10Passthrough,
        ] {
            assert!(
                !cpu_touching.keeps_frames_off_the_cpu(),
                "{cpu_touching:?} runs real filter work on the CPU"
            );
        }
    }

    /// A GPU pipeline's one-step retry keeps the encoder and moves to the CPU
    /// chain, so it owes a CPU delta and not a demotion. A CPU pipeline's
    /// retry replaces the encoder, so it owes the demotion and no delta. The
    /// two must never both be set: that would reserve the pipeline twice.
    #[test]
    fn a_retry_owes_either_a_demotion_or_a_cpu_delta_and_never_both() {
        let file = execution_file_for_retry();
        let mut gpu = execution_options_for_retry();
        gpu.pipeline = Pipeline::VppQsv;
        let retained = PrepublicationTranscodeRetry::prepare(
            &file,
            &gpu,
            Encoder::Qsv,
            EffectiveRateControl::Vbr,
        )
        .expect("a GPU pipeline has a color-safe CPU retry");
        assert_eq!(retained.encoder, Encoder::Qsv, "the encoder is retained");
        assert_eq!(retained.software_threads, None, "so this is not a demotion");

        let mut cpu = execution_options_for_retry();
        cpu.pipeline = Pipeline::Cpu;
        let demoted = PrepublicationTranscodeRetry::prepare(
            &file,
            &cpu,
            Encoder::Qsv,
            EffectiveRateControl::Vbr,
        )
        .expect("a CPU pipeline retries in software");
        assert_eq!(demoted.encoder, Encoder::Software);
        assert_eq!(
            demoted.software_threads,
            Some(Workload::of(&file, cpu.target_height).software_threads())
        );
    }

    /// The hardware→software fallback must return its slot at the transition,
    /// not at teardown: with a cap of one, the next hardware start would
    /// otherwise queue behind a software session for as long as it lives —
    /// potentially a whole film. And the admission record must flip to the
    /// software class, or every speed measured from the replacement encoder
    /// is filed as evidence about hardware.
    #[tokio::test]
    async fn a_fallback_to_software_frees_the_hardware_slot_at_once() {
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

        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-fallback")
            .await
            .expect("hardware start");
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
        let permit = mgr
            .admissions
            .software_pool()
            .take_forced(work_class.software_threads());
        session.demote_to_software(work_class, permit);

        assert_eq!(
            mgr.admissions.in_use(),
            0,
            "the slot came back at the transition"
        );
        assert_eq!(
            *session.class.lock().expect("class"),
            Workload::of(&file, session.target_height).software_class(),
            "speeds measured from here on are software evidence"
        );
        assert_eq!(
            mgr.admissions.software_in_use(),
            work_class.software_threads(),
            "the fallback owns its software weight"
        );
        // The whole point, stated as the viewer experiences it: with the cap
        // at one and the demoted session still alive, the next hardware start
        // is admitted immediately instead of queuing for this session's life.
        match mgr
            .admissions
            .admit(1, Workload::of(&file, session.target_height))
        {
            Admission::Hardware(_slot) => {}
            other => panic!("the freed slot must be grantable now, got {other:?}"),
        }
        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert_eq!(mgr.admissions.in_use(), 0);
        assert_eq!(
            mgr.admissions.software_in_use(),
            0,
            "fallback teardown returned its replacement permit"
        );
    }

    // ---- the software CPU pool, wired through start() (review §2.4) ---------

    /// A software-only box. Sessions used to bypass admission entirely here;
    /// now a start reserves its thread weight and every ending returns it.
    #[tokio::test]
    async fn software_starts_reserve_the_cpu_pool_and_stops_return_it() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(), // no hardware: software from the start
            Pipeline::Cpu,
        );

        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-sw")
            .await
            .expect("software start");
        // The 4K fixture at a 1080 rung: base 4 threads + the 4K decode
        // surcharge — the weight is the workload's, not the machine's.
        assert_eq!(
            mgr.admissions.software_in_use(),
            6,
            "the start reserved its weight"
        );
        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert_eq!(
            mgr.admissions.software_in_use(),
            0,
            "and the stop returned it"
        );
    }

    /// The budget is a bound on *joining*, not on existing: a second session
    /// that does not fit is refused with the reason, and space freed by a
    /// permit release is grantable again. Exercise the live-admission boundary
    /// directly: the metadata-only fixture has no source file, so starting
    /// ffmpeg would make process-exit cleanup race the capacity assertion.
    #[tokio::test]
    async fn the_software_pool_refuses_what_it_cannot_fit() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store
            .get_file(file_id)
            .await
            .expect("file lookup")
            .expect("seeded file");
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        // Policy under test, not the build machine's core count.
        store
            .put_setting(keys::SW_POOL_THREADS, "8")
            .await
            .expect("budget");

        let first = mgr
            .admit_live(
                Encoder::Software,
                None,
                Workload::of(&file, 1080),
                Duration::ZERO,
                Priority::Live,
            )
            .await
            .expect("first fits an empty pool");
        assert_eq!(mgr.admissions.software_in_use(), 6);
        let refused = match mgr
            .admit_live(
                Encoder::Software,
                None,
                Workload::of(&file, 1080),
                Duration::ZERO,
                Priority::Live,
            )
            .await
        {
            Err(why) => why,
            Ok(_) => panic!("6 + 6 exceeds a budget of 8 and must be refused"),
        };
        assert!(refused.contains("software CPU pool"), "{refused}");

        drop(first);
        assert_eq!(mgr.admissions.software_in_use(), 0);
        let second = mgr
            .admit_live(
                Encoder::Software,
                None,
                Workload::of(&file, 1080),
                Duration::ZERO,
                Priority::Live,
            )
            .await
            .expect("freed weight is grantable again");
        assert_eq!(mgr.admissions.software_in_use(), 6);
        drop(second);
        assert_eq!(mgr.admissions.software_in_use(), 0);
    }

    /// On a tiny box every session is over budget; the empty-pool exception
    /// is what keeps the budget from being a ban. One saturating session is
    /// the best that box can do.
    #[tokio::test]
    async fn a_lone_software_session_may_exceed_a_tiny_budget() {
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
        store
            .put_setting(keys::SW_POOL_THREADS, "2")
            .await
            .expect("budget");

        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-lone")
            .await
            .expect("the only session may saturate the box");
        assert_eq!(
            mgr.admissions.software_in_use(),
            6,
            "overcommit on the books"
        );
        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert_eq!(mgr.admissions.software_in_use(), 0);
    }

    // ---- rolling session fixtures and lifecycle ----------------------------

    /// `moved_at_ms` freezes with the SIGSTOP — correctly — so without a
    /// resume re-baseline, the first check after SIGCONT reads the whole
    /// suspension as a stall and kills a session that was healthy on both
    /// sides of it.
    #[test]
    fn resume_restarts_the_motion_clock() {
        let p = Progress::new();
        p.begin_attempt();
        // A long-suspended session: last movement far in the past.
        p.moved_at_ms.store(
            p.started.elapsed().as_millis() as i64 - 10 * PROGRESS_STALL.as_millis() as i64,
            Relaxed,
        );
        assert!(
            p.stalled_for() >= PROGRESS_STALL,
            "the setup must look stalled"
        );
        p.touch();
        assert!(
            p.stalled_for() < PROGRESS_STALL,
            "after resume the clock counts from the resume, not the suspension"
        );
    }

    /// Generic rolling-session fixture: an optional real child, test-owned
    /// scratch, and the actor/lifecycle state shared by unrelated tests below.
    /// The historical helper name remains to keep this mechanical cut small.
    fn watchdog_session_with_control(
        dir: &std::path::Path,
        child: Option<Child>,
        cached: bool,
        actor_managed_prepublication: bool,
        control: crate::playback_control::RollingControlHandle,
        producer_attempt: u64,
    ) -> Arc<Session> {
        Arc::new(Session {
            dir: dir.to_path_buf(),
            recovery: None,
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: None,
            actor_managed_response_publication: actor_managed_prepublication,
            actor_managed_prepublication_process: actor_managed_prepublication,
            actor_prepublication_producer: Arc::new(AtomicBool::new(actor_managed_prepublication)),
            response_publication_transition: Mutex::new(()),
            first_media_handoff_applied: AtomicBool::new(false),
            first_media_handoff_notify: tokio::sync::Notify::new(),
            prepublication_cleanup_active: AtomicBool::new(false),
            retirement_cleanup_started: AtomicBool::new(false),
            retirement_cleanup_finished: AtomicBool::new(false),
            retirement_settlement: std::sync::Mutex::new(None),
            scratch_cleanup_started: AtomicBool::new(false),
            retirement_context: None,
            cache_integrity_cleanup_started: AtomicBool::new(false),
            child: Mutex::new(
                child
                    .map(|child| AttemptChild::new(producer_attempt, child, control.clone(), None)),
            ),
            child_transition: Mutex::new(()),
            replacing_child: AtomicBool::new(false),
            replacement_pause: std::sync::Mutex::new(None),
            activity_detail_pause: std::sync::Mutex::new(None),
            control_applied_pause: std::sync::Mutex::new(None),
            terminal_response_pending: Arc::new(AtomicBool::new(false)),
            terminal_control: std::sync::Mutex::new(None),
            flow_completion_pause: std::sync::Mutex::new(None),
            playlist_publication_pause: std::sync::Mutex::new(None),
            producer_install_pause: std::sync::Mutex::new(None),
            refresh_after_read_pause: std::sync::Mutex::new(None),
            path_owner_sample_pause: std::sync::Mutex::new(None),
            retention_delete_pause: std::sync::Mutex::new(None),
            response_projection_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            first_media_owner_claim_pause: std::sync::Mutex::new(None),
            retirement_started: AtomicBool::new(false),
            #[cfg(test)]
            retirement_cleanup_handoff_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            scratch_cleanup_pause: std::sync::Mutex::new(None),
            cached,
            _cache_reader: None,
            subtitle_handle: None,
            #[cfg(windows)]
            source_handle: None,
            #[cfg(windows)]
            output_handle: None,
            cache_manifest: None,
            cache_location: None,
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id: 1,
            item_id: 1,
            item_title: "Watchdog Fixture".into(),
            user_name: "paul".into(),
            supersession_user: serde_json::json!(["username", "paul"]).to_string(),
            playback_id: "pb-watchdog".into(),
            automatic: true,
            kind: SessionKind::Transcode { height: 1080 },
            method: crate::delivery::Method::Transcode,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            grade: OutputGrade::Sdr,
            target_height: 1080,
            tone_map_peak_nits: None,
            tone_map_peak_source: None,
            encoder_label: Mutex::new("test"),
            started_unix: 0,
            failed: Arc::new(AtomicBool::new(false)),
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(false),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: None,
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope: 0,
            upload: None,
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::new()),
            class: std::sync::Mutex::new(String::new()),
            hw_slot: std::sync::Mutex::new(None),
            sw_permit: std::sync::Mutex::new(None),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::new(),
            http_waits: HttpWaitLedger::default(),
            readrate: 0.0,
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover: None,
            first_slide_logged: AtomicBool::new(false),
        })
    }

    fn watchdog_session_with_publication(
        dir: &std::path::Path,
        child: Option<Child>,
        cached: bool,
        actor_managed_prepublication: bool,
    ) -> Arc<Session> {
        watchdog_session_with_control(
            dir,
            child,
            cached,
            actor_managed_prepublication,
            crate::playback_control::RollingControlHandle::spawn("test-start"),
            0,
        )
    }

    fn watchdog_session(dir: &std::path::Path, child: Option<Child>, cached: bool) -> Arc<Session> {
        watchdog_session_with_publication(dir, child, cached, false)
    }

    fn reserve_test_admissions(session: &Session, admissions: &Admissions) {
        *session.hw_slot.lock().expect("hw slot mutex") = Some(
            admissions
                .try_acquire(1, Priority::Live)
                .expect("test hardware slot"),
        );
        *session.sw_permit.lock().expect("sw permit mutex") =
            Some(admissions.software_pool().take_forced(2));
    }

    async fn await_prepublication_cleanup(session: &Session) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while session.prepublication_cleanup_active.load(Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prepublication cleanup must settle");
    }

    async fn await_lifecycle_pause(pause: &LifecycleTestPause) {
        tokio::time::timeout(Duration::from_secs(2), pause.reached.notified())
            .await
            .expect("lifecycle pause must be reached");
    }

    async fn await_scratch_removed(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached scratch cleanup must settle");
    }

    #[tokio::test]
    async fn published_failure_cleanup_survives_waiter_cancellation_and_retains_media() {
        let root = crate::test_tempdir().expect("published cleanup root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create published scratch");
        let playlist = scratch.join("index.m3u8");
        let segment = scratch.join("seg00000.ts");
        tokio::fs::write(&playlist, b"#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n")
            .await
            .expect("seed published playlist");
        tokio::fs::write(&segment, b"published media")
            .await
            .expect("seed published segment");

        let admissions = Admissions::new();
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        session.actor_prepublication_producer.store(false, Release);
        session.first_media_handoff_applied.store(true, Release);
        reserve_test_admissions(&session, &admissions);
        let reap_pause = Arc::new(LifecycleTestPause::new());
        session
            .child
            .lock()
            .await
            .as_ref()
            .expect("attempt child")
            .pause_terminate_before_reap(Arc::clone(&reap_pause));

        let settlement = spawn_published_failure_cleanup_owner(
            &session,
            0,
            11,
            crate::playback_control::ProducerDecisionReason::ProgressDeadline,
            "published-cleanup",
        );
        await_lifecycle_pause(&reap_pause).await;
        drop(settlement);
        assert_eq!(admissions.in_use(), 1);
        assert_eq!(admissions.software_in_use(), 2);
        assert!(playlist.exists());
        assert!(segment.exists());
        assert!(!session.failed.load(Acquire));
        assert!(!session.control.is_retired());

        reap_pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while session.child.lock().await.is_some()
                || admissions.in_use() != 0
                || admissions.software_in_use() != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached published cleanup must confirm reap and release capacity");
        assert!(playlist.exists(), "published playlist must be retained");
        assert!(segment.exists(), "published segment must be retained");
        assert!(!session.failed.load(Acquire));
        assert!(!session.control.is_retired());
    }

    #[tokio::test]
    async fn successful_published_completion_reaps_child_and_returns_capacity_without_deleting_media(
    ) {
        let root = crate::test_tempdir().expect("published completion root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create completed scratch");
        let playlist = scratch.join("index.m3u8");
        let segment = scratch.join("seg00000.ts");
        tokio::fs::write(
            &playlist,
            b"#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n",
        )
        .await
        .expect("seed completed playlist");
        tokio::fs::write(&segment, b"completed media")
            .await
            .expect("seed completed segment");

        let admissions = Admissions::new();
        let session =
            watchdog_session_with_publication(&scratch, Some(successful_child()), false, true);
        session.actor_prepublication_producer.store(false, Release);
        session.first_media_handoff_applied.store(true, Release);
        reserve_test_admissions(&session, &admissions);

        assert_eq!(
            spawn_published_completion_cleanup_owner(
                &session,
                0,
                crate::playback_control::RollingProducerCompletionDisposition::CompleteUnverifiedDuration,
                "published-completion",
            )
            .await
            .expect("completion cleanup owner"),
            PublishedFailureCleanupOutcome::Reaped
        );
        assert!(session.child.lock().await.is_none());
        assert_eq!(admissions.in_use(), 0);
        assert_eq!(admissions.software_in_use(), 0);
        assert!(playlist.exists(), "completed playlist must be retained");
        assert!(segment.exists(), "completed segment must be retained");
        assert!(!session.failed.load(Acquire));
        assert!(!session.control.is_retired());
    }

    #[tokio::test]
    async fn present_segment_resolved_before_published_failure_is_rechecked_against_actor_frontier()
    {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("published frontier root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create frontier scratch");
        tokio::fs::write(scratch.join("seg00000.ts"), b"published")
            .await
            .expect("seed published segment");
        tokio::fs::write(scratch.join("seg00001.ts"), b"present but unpublished")
            .await
            .expect("seed beyond-frontier segment");
        tokio::fs::write(scratch.join("init.mp4"), b"init")
            .await
            .expect("seed init segment");

        let (control, mut registration) =
            crate::playback_control::RollingControlHandle::spawn_prepublication_transcode(
                "published-frontier-test",
            );
        registration.register().await.expect("register executor");
        let producer_attempt = control
            .begin_initial_producer_attempt(
                crate::playback_control::InitialProducerPolicy::software(
                    "published-frontier-contract".to_owned(),
                    PROGRESS_STALL,
                ),
            )
            .await
            .expect("begin producer attempt");
        assert!(
            control
                .observe_publication(crate::playback_control::RollingPublicationObservation {
                    producer_attempt,
                    publication_commit: true,
                    demand_sequence: None,
                    produced_segment: Some(0),
                    produced_end_ms: Some(2_000),
                    playlist_ready: true,
                    published_segment: Some(0),
                    published_end_ms: Some(2_000),
                    published_first_segment: Some(0),
                    published_start_ms: Some(0),
                    media_origin_ms: 0,
                    next_media_sequence: 1,
                    resolved_fetched_segment: None,
                    resolved_fetched_end_ms: None,
                })
                .await
        );
        let handoff = crate::playback_control::RollingFirstMediaPublicationHandoff::new();
        assert_eq!(
            control
                .authorize_response_publication(
                    crate::playback_control::RollingResponsePublication::attempt_media(
                        crate::playback_control::RollingResponseObject::VideoMediaPlaylist,
                        producer_attempt,
                        None,
                    ),
                    Some(handoff),
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Ok(crate::playback_control::RollingResponseAuthorization {
                first_producer_media_publication: true,
            })
        );

        let session = watchdog_session_with_control(
            &scratch,
            None,
            false,
            true,
            control.clone(),
            producer_attempt,
        );
        session.actor_prepublication_producer.store(false, Release);
        session.first_media_handoff_applied.store(true, Release);
        *session
            .compatibility_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = producer_attempt;
        *session.segments.lock().await = SegmentIndex {
            segs: vec![
                SegmentMeta {
                    index: 0,
                    name: "seg00000.ts".into(),
                    start_ms: 0,
                    end_ms: 2_000,
                    bytes: 9,
                    visibility: SegmentVisibility::Advertised,
                },
                SegmentMeta {
                    index: 1,
                    name: "seg00001.ts".into(),
                    start_ms: 2_000,
                    end_ms: 4_000,
                    bytes: 23,
                    visibility: SegmentVisibility::Advertised,
                },
            ],
            revision: 1,
        };
        session.publication.lock().await.served = Some(ServedPlaylistSnapshot {
            raw: Arc::from(&b"#EXTM3U\n"[..]),
            producer_attempt,
            revision: 1,
            last_segment: 0,
            first_segment: 0,
            end_ms: 2_000,
            duration_ms: 2_000,
            end_list: false,
            available_at: Instant::now(),
        });
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        manager
            .sessions
            .lock()
            .await
            .insert("published-frontier".to_owned(), Arc::clone(&session));

        // Resolve/open both paths before the failure wins. This is the exact
        // request race: path existence alone used to authorize segment 1
        // after the actor had retained only the frontier through segment 0.
        let beyond = match manager
            .segment_for_publication("published-frontier", "seg00001.ts")
            .await
            .expect("resolve beyond-frontier segment")
        {
            SegmentPublication::Ready(file) => file,
            _ => panic!("present segment must resolve before failure"),
        };
        let retained = match manager
            .segment_for_publication("published-frontier", "seg00000.ts")
            .await
            .expect("resolve retained segment")
        {
            SegmentPublication::Ready(file) => file,
            _ => panic!("published segment must resolve before failure"),
        };
        let init = match manager
            .segment_for_publication("published-frontier", "init.mp4")
            .await
            .expect("resolve init segment")
        {
            SegmentPublication::Ready(file) => file,
            _ => panic!("init segment must resolve before failure"),
        };

        control.observe_producer_exit(producer_attempt, false, Some(1), None);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if control
                    .snapshot()
                    .await
                    .is_some_and(|snapshot| snapshot.producer_control.producer_ended_with_proposal)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor must commit the retained published failure");

        assert!(matches!(
            manager
                .authorize_response_publication(
                    "published-frontier",
                    &beyond.response_owner(),
                    MediaResponsePublication::attempt_media(
                        "media-segment",
                        Some("seg00001.ts"),
                    ),
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(MediaResponsePublicationRejection::ProducerEnded(reason))
                if reason == "process_exit"
        ));
        for response_kind in ["segment-range", "segment-not-modified"] {
            assert!(
                matches!(
                    manager
                        .authorize_response_publication(
                            "published-frontier",
                            &beyond.response_owner(),
                            MediaResponsePublication::attempt_media(
                                response_kind,
                                Some("seg00001.ts"),
                            ),
                            Instant::now() + Duration::from_secs(1),
                        )
                        .await,
                    Err(MediaResponsePublicationRejection::ProducerEnded(reason))
                        if reason == "process_exit"
                ),
                "{response_kind} must preserve the numeric segment coordinate"
            );
        }
        assert!(manager
            .authorize_response_publication(
                "published-frontier",
                &retained.response_owner(),
                MediaResponsePublication::attempt_media("media-segment", Some("seg00000.ts")),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_ok());
        assert!(manager
            .authorize_response_publication(
                "published-frontier",
                &init.response_owner(),
                MediaResponsePublication::attempt_media("segment-range", Some("init.mp4")),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .is_ok());
    }

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Release);
        }
    }

    /// The pause `retirement_settlement_registers_notify_before_the_wait_gap`
    /// has always used, as a hook: the first wait to reach the point takes it
    /// and meets the test at the barrier twice.
    struct PausingRetirementSettlementHooks(std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>);

    impl RetirementSettlementHooks for PausingRetirementSettlementHooks {
        fn before_await_settled(&self) -> HookFuture<'_> {
            let pause = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Box::pin(async move {
                if let Some(pause) = pause {
                    pause.wait().await;
                    pause.wait().await;
                }
            })
        }
    }

    #[tokio::test]
    async fn retirement_settlement_registers_notify_before_the_wait_gap() {
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        let settlement = Arc::new(RollingRetirementSettlement::with_hooks(
            "ended",
            Box::new(PausingRetirementSettlementHooks(std::sync::Mutex::new(
                Some(Arc::clone(&pause)),
            ))),
        ));
        let waiter = tokio::spawn({
            let settlement = Arc::clone(&settlement);
            async move { settlement.wait().await }
        });

        pause.wait().await;
        settlement.complete(Ok(true));
        pause.wait().await;
        assert!(tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("registered Notify waiter must not lose completion")
            .expect("settlement waiter task")
            .expect("settlement result"));
    }

    /// M8's shipped-shape test for the retirement settlement: the production
    /// constructor (no-op hooks) runs the race test's scenario to completion.
    /// Acceptance runs it in the release profile
    /// (`cargo test --release -p plurxd rolling_retirement_settlement_shipped_shape`),
    /// where the settlement has the layout and await points the daemon ships.
    /// The waiter is polled by hand, so the test needs no task and no timer.
    #[tokio::test]
    async fn rolling_retirement_settlement_shipped_shape() {
        use futures_util::FutureExt;

        let settlement = RollingRetirementSettlement::new("ended");
        let mut waiter = Box::pin(settlement.wait());
        assert!(
            waiter.as_mut().now_or_never().is_none(),
            "nothing has settled, so the waiter parks on its registered Notify"
        );
        settlement.complete(Ok(true));
        assert_eq!(
            waiter.as_mut().now_or_never(),
            Some(Ok(true)),
            "the parked waiter observes completion on its next poll"
        );
    }

    #[tokio::test]
    async fn response_publication_observes_authority_loss_before_watch_delivery() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("direct serving authority root");
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(
            TranscodeManager::new(
                store,
                root.path().join("manager"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            .with_serving_authority(fence.authority()),
        );
        let session = watchdog_session(&root.path().join("rolling"), None, true);
        manager
            .sessions
            .lock()
            .await
            .insert("direct-authority".to_owned(), Arc::clone(&session));
        let owner = MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt: 0,
        });

        fence.validation_set_ready(false).await;
        assert!(
            manager.serving_ready.load(Acquire),
            "the teardown watch mirror has deliberately not consumed this loss"
        );
        assert_eq!(manager.serving_loss_generation.load(Acquire), 0);
        assert!(matches!(
            manager
                .authorize_response_publication(
                    "direct-authority",
                    &owner,
                    MediaResponsePublication::attempt_media("segment", Some("seg00000.ts")),
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(MediaResponsePublicationRejection::StateChanged)
        ));
    }

    #[tokio::test]
    async fn registration_observes_authority_loss_before_watch_delivery() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("direct registration authority root");
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(
            TranscodeManager::new(
                store,
                root.path().join("manager"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            .with_serving_authority(fence.authority()),
        );
        let session = watchdog_session(
            &root.path().join("unpublished"),
            Some(long_running_child()),
            false,
        );

        fence.validation_set_ready(false).await;
        assert!(manager.serving_ready.load(Acquire));
        assert_eq!(manager.serving_loss_generation.load(Acquire), 0);
        assert_eq!(
            manager
                .register_session("direct-registration", Arc::clone(&session), 0)
                .await,
            Err(SessionRegistrationRejection::ServingFence)
        );
        assert!(manager.sessions.lock().await.is_empty());
        assert!(session.child.lock().await.is_none());
        assert!(session.control.is_retired());
    }

    #[tokio::test]
    async fn in_flight_vod_publication_rechecks_direct_serving_authority() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("in-flight VOD authority root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let manager = Arc::new(
            TranscodeManager::new(
                Arc::clone(&store),
                root.path().join("manager"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            .with_serving_authority(fence.authority()),
        );
        manager
            .install_vod_http_test_session("vod-authority", file_id, root.path())
            .await;
        let publication = manager
            .vod_playlist("vod-authority")
            .await
            .expect("VOD publication fixture");
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *manager
            .vod_publication_admission_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));

        let authorization = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager
                    .authorize_response_publication(
                        "vod-authority",
                        &publication.owner,
                        MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
                        Instant::now() + Duration::from_secs(1),
                    )
                    .await
            }
        });
        pause.wait().await;
        fence.validation_set_ready(false).await;
        pause.wait().await;

        assert!(matches!(
            authorization.await.expect("VOD authorization task"),
            Err(MediaResponsePublicationRejection::StateChanged)
        ));
        assert!(
            manager.serving_ready.load(Acquire),
            "the async teardown mirror is not needed for in-flight rejection"
        );
    }

    #[tokio::test]
    async fn authorized_vod_eof_cannot_commit_after_phase_one_release() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("VOD EOF release root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        manager
            .install_vod_http_test_session("vod-eof-release", file_id, root.path())
            .await;
        let publication = manager
            .vod_playlist("vod-eof-release")
            .await
            .expect("VOD publication fixture");
        let authorization = manager
            .authorize_response_publication(
                "vod-eof-release",
                &publication.owner,
                MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("response admitted before release");

        manager
            .begin_session_publication_fence("vod-eof-release")
            .await;
        assert!(matches!(
            manager
                .commit_authorized_media(
                    authorization,
                    true,
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(MediaResponsePublicationRejection::StateChanged)
        ));
    }

    #[tokio::test]
    async fn authorized_vod_eof_cannot_commit_after_direct_authority_loss() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("VOD EOF authority root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let manager = Arc::new(
            TranscodeManager::new(
                store,
                root.path().join("manager"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            .with_serving_authority(fence.authority()),
        );
        manager
            .install_vod_http_test_session("vod-eof-authority", file_id, root.path())
            .await;
        let publication = manager
            .vod_playlist("vod-eof-authority")
            .await
            .expect("VOD publication fixture");
        let authorization = manager
            .authorize_response_publication(
                "vod-eof-authority",
                &publication.owner,
                MediaResponsePublication::attempt_media("playlist", Some("index.m3u8")),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("response admitted before authority loss");

        fence.validation_set_ready(false).await;
        assert!(matches!(
            manager
                .commit_authorized_media(
                    authorization,
                    true,
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(MediaResponsePublicationRejection::StateChanged)
        ));
    }

    #[tokio::test]
    async fn authority_fence_projects_the_entire_snapshot_before_actor_settlement() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("authority snapshot root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let blocked = watchdog_session(&root.path().join("blocked"), None, true);
        let follower = watchdog_session(&root.path().join("follower"), None, true);
        {
            let mut sessions = manager.sessions.lock().await;
            sessions.insert("a-blocked".to_owned(), Arc::clone(&blocked));
            sessions.insert("b-follower".to_owned(), Arc::clone(&follower));
        }
        let actor_pause = Arc::new(tokio::sync::Barrier::new(2));
        blocked
            .control
            .pause_producer_attempt_reply(Arc::clone(&actor_pause));
        let actor_command = tokio::spawn({
            let control = blocked.control.clone();
            async move { control.begin_producer_attempt().await }
        });
        actor_pause.wait().await;

        let fence = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager
                    .fence_sessions(&["a-blocked".to_owned(), "b-follower".to_owned()])
                    .await;
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !blocked.control.is_retired() || !follower.control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the complete snapshot must fail closed synchronously");
        assert!(
            !fence.is_finished(),
            "the blocked first actor must still hold settlement open"
        );
        let follower_owner = MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            session: Arc::clone(&follower),
            producer_attempt: 0,
        });
        assert!(
            matches!(
                manager
                    .authorize_response_publication(
                        "b-follower",
                        &follower_owner,
                        MediaResponsePublication::attempt_media("segment", Some("seg00000.ts")),
                        Instant::now() + Duration::from_secs(1),
                    )
                    .await,
                Err(MediaResponsePublicationRejection::StateChanged)
            ),
            "a later snapshot member cannot publish while an earlier actor is stalled"
        );

        actor_pause.wait().await;
        let _ = actor_command.await.expect("blocked actor command");
        fence.await.expect("authority fence task");
        assert_eq!(
            blocked
                .control
                .snapshot()
                .await
                .expect("blocked actor snapshot")
                .terminal,
            Some(crate::playback_control::RollingTerminalCause::AuthorityFence)
        );
        assert_eq!(
            follower
                .control
                .snapshot()
                .await
                .expect("follower actor snapshot")
                .terminal,
            Some(crate::playback_control::RollingTerminalCause::AuthorityFence)
        );
    }

    #[tokio::test]
    async fn bounded_stop_transfers_hold_before_stalled_actor_settlement() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("bounded stop root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session = watchdog_session(
            &root.path().join("session"),
            Some(long_running_child()),
            false,
        );
        manager
            .sessions
            .lock()
            .await
            .insert("bounded-stop".to_owned(), Arc::clone(&session));
        let actor_pause = Arc::new(tokio::sync::Barrier::new(2));
        session
            .control
            .pause_producer_attempt_reply(Arc::clone(&actor_pause));
        let actor_command = tokio::spawn({
            let control = session.control.clone();
            async move { control.begin_producer_attempt().await }
        });
        actor_pause.wait().await;
        let dropped = Arc::new(AtomicBool::new(false));

        assert!(
            !manager
                .stop_session_until(
                    "bounded-stop",
                    "replaced",
                    tokio::time::Instant::now() + Duration::from_millis(20),
                    DropProbe(Arc::clone(&dropped)),
                )
                .await,
            "the caller deadline expires while actor settlement is stalled"
        );
        assert!(session.control.is_retired());
        assert!(
            !dropped.load(Acquire),
            "the replacement hold belongs to detached teardown, not the timed-out caller"
        );
        assert!(
            manager.sessions.try_lock().is_ok(),
            "actor settlement must not retain the global registry lock"
        );

        actor_pause.wait().await;
        let _ = actor_command.await.expect("blocked actor command");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dropped.load(Acquire)
                || manager.sessions.lock().await.contains_key("bounded-stop")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached teardown must settle and release its hold");
        assert!(session.child.lock().await.is_none());
    }

    #[tokio::test]
    async fn first_media_settlement_gap_keeps_confirmed_reap_ownership() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("first-media retirement root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let admissions = Admissions::new();
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        reserve_test_admissions(&session, &admissions);
        let owner_pause = Arc::new(LifecycleTestPause::new());
        *session
            .first_media_owner_claim_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&owner_pause));
        let reap_pause = Arc::new(LifecycleTestPause::new());
        session
            .child
            .lock()
            .await
            .as_ref()
            .expect("attempt child")
            .pause_terminate_before_reap(Arc::clone(&reap_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("first-media-gap".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        let (handoff, applied) = begin_first_media_publication_handoff(&session, "first-media-gap")
            .await
            .expect("first-media settlement capacity");
        handoff.settle_for_test(true);
        await_lifecycle_pause(&owner_pause).await;
        assert!(!session.actor_prepublication_producer.load(Acquire));
        assert!(!session.first_media_handoff_applied.load(Acquire));
        assert!(
            session.prepublication_process_cleanup_required(),
            "retirement must retain confirmed-reap ownership until actor lifetime handoff publishes"
        );

        let retirement = tokio::spawn({
            let session = Arc::clone(&session);
            async move { manager.retire_session("first-media-gap", &session).await }
        });
        await_lifecycle_pause(&reap_pause).await;
        assert!(session.prepublication_cleanup_active.load(Acquire));
        assert_eq!(admissions.in_use(), 1);
        assert_eq!(admissions.software_in_use(), 2);

        owner_pause.release.notify_one();
        assert!(applied.await.expect("first-media handoff application"));
        reap_pause.release.notify_one();
        assert!(retirement.await.expect("retirement task"));
        await_prepublication_cleanup(&session).await;
        assert_eq!(admissions.in_use(), 0);
        assert_eq!(admissions.software_in_use(), 0);
        await_scratch_removed(&scratch).await;
        assert!(!scratch.exists());
        session.fail(PlaylistError::SessionFailed("test complete".into()));
    }

    #[tokio::test]
    async fn prepublication_retirement_holds_admissions_until_confirmed_reap() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("prepublication retirement root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        tokio::fs::write(scratch.join("index.m3u8"), b"prepublication")
            .await
            .expect("seed scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let admissions = Admissions::new();
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        reserve_test_admissions(&session, &admissions);
        let reap_pause = Arc::new(LifecycleTestPause::new());
        session
            .child
            .lock()
            .await
            .as_ref()
            .expect("attempt child")
            .pause_terminate_before_reap(Arc::clone(&reap_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("prepublication".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        let retirement = tokio::spawn({
            let session = Arc::clone(&session);
            async move { manager.retire_session("prepublication", &session).await }
        });
        await_lifecycle_pause(&reap_pause).await;
        assert!(session.prepublication_cleanup_active.load(Acquire));
        assert_eq!(admissions.in_use(), 1, "hardware remains owned before reap");
        assert_eq!(
            admissions.software_in_use(),
            2,
            "software remains owned before reap"
        );
        assert!(scratch.exists(), "scratch remains owned before reap");
        assert!(
            !session.failed.load(Acquire),
            "routine retirement is not a producer failure"
        );

        reap_pause.release.notify_one();
        assert!(
            retirement.await.expect("retirement task"),
            "routine retirement must transfer cleanup ownership"
        );
        await_prepublication_cleanup(&session).await;
        assert_eq!(admissions.in_use(), 0);
        assert_eq!(admissions.software_in_use(), 0);
        assert!(session.child.lock().await.is_none());
        await_scratch_removed(&scratch).await;
        assert!(!scratch.exists());
        assert!(!session.failed.load(Acquire));
    }

    #[tokio::test]
    async fn bounded_retirement_cleanup_survives_caller_cancellation() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("cancelled retirement root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let admissions = Admissions::new();
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        reserve_test_admissions(&session, &admissions);
        let handoff_pause = Arc::new(LifecycleTestPause::new());
        *session
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&handoff_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("cancelled".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        let retirement = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move {
                manager
                    .retire_session_until(
                        "cancelled",
                        &session,
                        Some(tokio::time::Instant::now() + Duration::from_secs(2)),
                    )
                    .await
            }
        });
        await_lifecycle_pause(&handoff_pause).await;
        assert!(session.prepublication_cleanup_active.load(Acquire));
        assert_eq!(admissions.in_use(), 1);
        assert_eq!(admissions.software_in_use(), 2);
        assert!(
            manager.sessions.try_lock().is_ok(),
            "retirement must not hold the global registry while actor/cleanup handoff waits"
        );
        assert!(
            manager.sessions.lock().await.contains_key("cancelled"),
            "the retired Arc stays discoverable until its child is reaped"
        );
        let follower = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move {
                manager
                    .retire_session_until_with_cause("cancelled", &session, None, "killed")
                    .await
            }
        });
        tokio::task::yield_now().await;
        assert!(
            !follower.is_finished(),
            "prepublication followers must join the paused physical winner"
        );

        // Cancelling the bounded caller drops only its settlement wait. The
        // already-spawned owner has the exact child, and a competing caller
        // remains joined to that same physical terminal proof.
        retirement.abort();
        assert!(retirement
            .await
            .expect_err("retirement task must be cancelled")
            .is_cancelled());
        handoff_pause.release.notify_one();
        let joined = follower
            .await
            .expect("prepublication follower task")
            .expect("prepublication follower settlement");
        assert_eq!(joined.participation, RollingRetirementParticipation::Joined);
        assert_eq!(joined.cause.as_ref(), "ended");
        assert!(joined.removed);
        await_prepublication_cleanup(&session).await;

        assert_eq!(admissions.in_use(), 0);
        assert_eq!(admissions.software_in_use(), 0);
        assert!(session.child.lock().await.is_none());
        await_scratch_removed(&scratch).await;
        assert!(!scratch.exists());
        assert!(!session.failed.load(Acquire));
    }

    #[tokio::test]
    async fn post_media_retirement_keeps_exact_resources_after_caller_cancellation() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("post-media retirement root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let admissions = Admissions::new();
        let session = watchdog_session(&scratch, Some(long_running_child()), false);
        reserve_test_admissions(&session, &admissions);
        let handoff_pause = Arc::new(LifecycleTestPause::new());
        *session
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&handoff_pause));
        let scratch_pause = Arc::new(LifecycleTestPause::new());
        *session
            .scratch_cleanup_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&scratch_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("post-media".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        let retirement = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move { manager.retire_session("post-media", &session).await }
        });
        await_lifecycle_pause(&handoff_pause).await;
        assert!(session.retirement_cleanup_started.load(Acquire));
        assert!(!session.prepublication_cleanup_active.load(Acquire));
        assert_eq!(admissions.in_use(), 1);
        assert_eq!(admissions.software_in_use(), 2);
        assert!(manager.sessions.lock().await.contains_key("post-media"));
        let follower = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move {
                manager
                    .retire_session_until_with_cause("post-media", &session, None, "killed")
                    .await
            }
        });
        tokio::task::yield_now().await;
        assert!(
            !follower.is_finished(),
            "a competing retirement must join the paused physical winner"
        );

        retirement.abort();
        assert!(retirement
            .await
            .expect_err("retirement waiter must cancel")
            .is_cancelled());
        handoff_pause.release.notify_one();
        let joined = follower
            .await
            .expect("retirement follower task")
            .expect("retirement follower settlement");
        assert_eq!(joined.participation, RollingRetirementParticipation::Joined);
        assert_eq!(joined.cause.as_ref(), "ended");
        assert!(joined.removed);
        await_lifecycle_pause(&scratch_pause).await;
        assert!(!manager.sessions.lock().await.contains_key("post-media"));
        assert!(session.child.lock().await.is_none());
        assert_eq!(admissions.in_use(), 0);
        assert_eq!(admissions.software_in_use(), 0);
        assert!(
            scratch.exists(),
            "physical settlement must not wait for unique scratch I/O"
        );
        spawn_rolling_scratch_cleanup_owner("post-media-duplicate".to_owned(), &session, None);
        tokio::task::yield_now().await;
        assert!(session.scratch_cleanup_started.load(Acquire));
        assert!(
            scratch.exists(),
            "prepublication/universal followers cannot spawn a second scratch owner"
        );

        let events = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let events = store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        since_ms: None,
                        event: Some("session_end".to_owned()),
                        limit: 50,
                    })
                    .await
                    .expect("retirement event query")
                    .into_iter()
                    .filter(|event| {
                        event.session_id.as_deref() == Some(session_log_id("post-media").as_str())
                    })
                    .collect::<Vec<_>>();
                if !events.is_empty() {
                    break events;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the winning retirement owner must retain event ownership");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason.as_deref(), Some("ended"));

        scratch_pause.release.notify_one();
        await_scratch_removed(&scratch).await;
        assert!(!scratch.exists());
    }

    #[tokio::test]
    async fn cached_retirement_survives_cancellation_without_deleting_cache() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("cached retirement root");
        let cached_dir = root.path().join("cache-entry");
        tokio::fs::create_dir_all(&cached_dir)
            .await
            .expect("create cache entry");
        tokio::fs::write(cached_dir.join("index.m3u8"), b"cached")
            .await
            .expect("seed cache entry");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session = watchdog_session(&cached_dir, None, true);
        let handoff_pause = Arc::new(LifecycleTestPause::new());
        *session
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&handoff_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("cached".to_owned(), Arc::clone(&session));

        let retirement = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move { manager.retire_session("cached", &session).await }
        });
        await_lifecycle_pause(&handoff_pause).await;
        retirement.abort();
        assert!(retirement
            .await
            .expect_err("cached retirement waiter must cancel")
            .is_cancelled());
        handoff_pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.sessions.lock().await.contains_key("cached") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached cached retirement must remove its exact Arc");
        assert!(
            cached_dir.join("index.m3u8").exists(),
            "session retirement must never delete a reusable cache generation"
        );
    }

    #[tokio::test]
    async fn cache_integrity_retirement_followers_join_the_exact_winner() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("cache integrity follower root");
        let cached_dir = root.path().join("cache-entry");
        tokio::fs::create_dir_all(&cached_dir)
            .await
            .expect("create cache entry");
        tokio::fs::write(cached_dir.join("index.m3u8"), b"cached")
            .await
            .expect("seed cache entry");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session = watchdog_session(&cached_dir, None, true);
        let handoff_pause = Arc::new(LifecycleTestPause::new());
        *session
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&handoff_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("cache-integrity".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        manager.fail_cached_session_integrity("cache-integrity", &session, "test");
        await_lifecycle_pause(&handoff_pause).await;
        assert!(manager
            .sessions
            .lock()
            .await
            .contains_key("cache-integrity"));

        let follower = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move {
                manager
                    .retire_session_until_with_cause("cache-integrity", &session, None, "killed")
                    .await
            }
        });
        tokio::task::yield_now().await;
        assert!(
            !follower.is_finished(),
            "admin retirement must join the paused cache-integrity winner"
        );

        handoff_pause.release.notify_one();
        let joined = follower
            .await
            .expect("cache integrity follower task")
            .expect("cache integrity follower settlement");
        assert_eq!(joined.participation, RollingRetirementParticipation::Joined);
        assert_eq!(joined.cause.as_ref(), "failed");
        assert!(joined.removed);
        assert!(session.retirement_cleanup_finished.load(Acquire));
        assert!(
            cached_dir.join("index.m3u8").exists(),
            "retiring a failed reader must preserve reusable cached bytes"
        );
    }

    #[tokio::test]
    async fn supersession_cancellation_finishes_vod_and_every_rolling_victim() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let root = crate::test_tempdir().expect("supersession transaction root");
        let manager = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        manager
            .install_vod_http_test_session("vod-victim", file_id, root.path())
            .await;
        let mut first = watchdog_session(
            &root.path().join("rolling-a"),
            Some(long_running_child()),
            false,
        );
        let mut second = watchdog_session(
            &root.path().join("rolling-b"),
            Some(long_running_child()),
            false,
        );
        for session in [&mut first, &mut second] {
            // Match the fixed HTTP VOD fixture's immutable supersession key
            // before either Arc is shared with the registry.
            let session = Arc::get_mut(session).expect("unshared rolling fixture");
            session.supersession_user = "[\"user_id\",1]".to_owned();
            session.playback_id = "http-vod-test".to_owned();
        }
        let first_pause = Arc::new(LifecycleTestPause::new());
        *first
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&first_pause));
        {
            let mut sessions = manager.sessions.lock().await;
            sessions.insert("rolling-a".to_owned(), Arc::clone(&first));
            sessions.insert("rolling-b".to_owned(), Arc::clone(&second));
            manager.active_session_count.store(sessions.len(), Relaxed);
        }

        let supersession = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                manager
                    .reap_superseded_before(None, "[\"user_id\",1]", "http-vod-test")
                    .await
            }
        });
        await_lifecycle_pause(&first_pause).await;
        assert!(
            !manager
                .vod
                .live_session_ids()
                .await
                .contains(&"vod-victim".to_owned()),
            "VOD mutation precedes the first rolling Terminal boundary"
        );
        supersession.abort();
        assert!(supersession
            .await
            .expect_err("supersession waiter must cancel")
            .is_cancelled());
        first_pause.release.notify_one();

        tokio::time::timeout(Duration::from_secs(2), async {
            while !manager.sessions.lock().await.is_empty()
                || !first.retirement_cleanup_finished.load(Acquire)
                || !second.retirement_cleanup_finished.load(Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached transaction must finish every rolling victim");
        assert!(first.child.lock().await.is_none());
        assert!(second.child.lock().await.is_none());
        assert!(first.retirement_cleanup_started.load(Acquire));
        assert!(second.retirement_cleanup_started.load(Acquire));
    }

    #[tokio::test]
    async fn retirement_removes_the_exact_arc_after_durable_id_adoption() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("adoption retirement root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        let handoff_pause = Arc::new(LifecycleTestPause::new());
        *session
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&handoff_pause));
        manager
            .sessions
            .lock()
            .await
            .insert("provisional".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        let retirement = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move { manager.retire_session("provisional", &session).await }
        });
        await_lifecycle_pause(&handoff_pause).await;
        assert!(
            manager.adopt_session_id("provisional", "durable").await,
            "durable activation may rename the exact Arc while End is in flight"
        );
        assert!(manager.sessions.lock().await.contains_key("durable"));

        handoff_pause.release.notify_one();
        assert!(retirement.await.expect("retirement task"));
        assert!(
            !manager.sessions.lock().await.contains_key("durable"),
            "settlement removes the exact Arc at its adopted key"
        );
        await_prepublication_cleanup(&session).await;
    }

    #[tokio::test]
    async fn retirement_finds_an_exact_arc_adopted_before_registry_entry() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("pre-adopted retirement root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        manager
            .sessions
            .lock()
            .await
            .insert("provisional-before".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);
        assert!(
            manager
                .adopt_session_id("provisional-before", "durable-before")
                .await
        );

        assert!(
            manager.retire_session("provisional-before", &session).await,
            "retirement owns the exact Arc even when its requested key was already adopted"
        );
        assert!(!manager.sessions.lock().await.contains_key("durable-before"));
        await_prepublication_cleanup(&session).await;
    }

    #[tokio::test]
    async fn adoption_panic_drops_cleanup_owner_at_the_moved_identity() {
        use plurx_core::store::SqliteStore;

        struct PanicAfterMoveOwner {
            identity: String,
            events: Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl SessionAdoptionOwner for PanicAfterMoveOwner {
            fn adopted_session_id(&mut self, durable_session_id: &str) {
                self.identity = durable_session_id.to_owned();
                self.events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("adopted:{durable_session_id}"));
                panic!("scripted panic after registry move");
            }
        }

        impl Drop for PanicAfterMoveOwner {
            fn drop(&mut self) {
                self.events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("drop:{}", self.identity));
            }
        }

        let root = crate::test_tempdir().expect("panic adoption root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        manager
            .sessions
            .lock()
            .await
            .insert("panic-provisional".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);
        let adoption = manager
            .session_adoption_token("panic-durable")
            .expect("adoption admission");
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));

        let task = tokio::spawn({
            let manager = Arc::clone(&manager);
            let events = Arc::clone(&events);
            async move {
                let owner = PanicAfterMoveOwner {
                    identity: "panic-provisional".to_owned(),
                    events,
                };
                let _ = manager
                    .adopt_session_id_with_owner(
                        "panic-provisional",
                        "panic-durable",
                        adoption,
                        owner,
                    )
                    .await;
            }
        });
        assert!(task
            .await
            .expect_err("adoption callback must panic")
            .is_panic());
        assert!(manager.sessions.lock().await.contains_key("panic-durable"));
        assert_eq!(
            *events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["adopted:panic-durable", "drop:panic-durable"],
            "the future owns cleanup and updates it synchronously with the map move"
        );

        manager
            .stop_session("panic-durable", "panic adoption test cleanup")
            .await;
    }

    #[tokio::test]
    async fn adoption_cancellation_drops_cleanup_owner_at_the_provisional_identity() {
        use plurx_core::store::SqliteStore;

        struct RecordingOwner {
            identity: String,
            dropped: Arc<std::sync::Mutex<Vec<String>>>,
        }

        impl SessionAdoptionOwner for RecordingOwner {
            fn adopted_session_id(&mut self, durable_session_id: &str) {
                self.identity = durable_session_id.to_owned();
            }
        }

        impl Drop for RecordingOwner {
            fn drop(&mut self) {
                self.dropped
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(self.identity.clone());
            }
        }

        let root = crate::test_tempdir().expect("cancelled adoption root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        manager
            .sessions
            .lock()
            .await
            .insert("cancel-provisional".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);
        let adoption = manager
            .session_adoption_token("cancel-durable")
            .expect("adoption admission");
        let transition = Arc::clone(&adoption.gate.transition).lock_owned().await;
        let dropped = Arc::new(std::sync::Mutex::new(Vec::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn({
            let manager = Arc::clone(&manager);
            let dropped = Arc::clone(&dropped);
            let entered = Arc::clone(&entered);
            async move {
                let owner = RecordingOwner {
                    identity: "cancel-provisional".to_owned(),
                    dropped,
                };
                entered.notify_one();
                let _ = manager
                    .adopt_session_id_with_owner(
                        "cancel-provisional",
                        "cancel-durable",
                        adoption,
                        owner,
                    )
                    .await;
            }
        });
        entered.notified().await;
        task.abort();
        assert!(task
            .await
            .expect_err("adoption waiter must cancel")
            .is_cancelled());
        drop(transition);

        assert!(manager
            .sessions
            .lock()
            .await
            .contains_key("cancel-provisional"));
        assert!(!manager.sessions.lock().await.contains_key("cancel-durable"));
        assert_eq!(
            *dropped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["cancel-provisional"],
            "cancellation before the registry move drops the move-owned cleanup capability at its provisional id"
        );
        manager
            .stop_session("cancel-provisional", "cancelled adoption test cleanup")
            .await;
    }

    #[tokio::test]
    async fn durable_adoption_cannot_publish_after_release_generation() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("release adoption root");
        let scratch = root.path().join("scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session =
            watchdog_session_with_publication(&scratch, Some(long_running_child()), false, true);
        manager
            .sessions
            .lock()
            .await
            .insert("release-provisional".to_owned(), Arc::clone(&session));
        manager.active_session_count.store(1, Relaxed);

        let before_end = manager
            .session_adoption_token("release-durable")
            .expect("release gate admission");
        manager.begin_session_release("release-durable").await;
        manager.complete_session_release("release-durable");
        assert!(
            !manager
                .adopt_session_id_with_token("release-provisional", "release-durable", before_end,)
                .await,
            "an operation admitted before End observes the monotone release generation"
        );

        let after_end = manager
            .session_adoption_token("release-durable")
            .expect("release gate admission");
        assert!(
            !manager
                .adopt_session_id_with_token("release-provisional", "release-durable", after_end,)
                .await,
            "the retained terminal gate closes the post-completion ABA window"
        );
        assert!(manager
            .sessions
            .lock()
            .await
            .contains_key("release-provisional"));
        assert!(
            manager
                .stop_session("release-provisional", "test cleanup")
                .await
        );
    }

    #[tokio::test]
    async fn untrusted_adoption_generations_have_a_hard_live_bound() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("adoption capacity root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let mut tokens = Vec::with_capacity(MAX_IN_FLIGHT_SESSION_ADOPTION_GATES);
        for index in 0..MAX_IN_FLIGHT_SESSION_ADOPTION_GATES {
            tokens.push(
                manager
                    .session_adoption_token(&format!("probe-{index}"))
                    .expect("capacity admits the documented number of distinct probes"),
            );
        }
        assert!(manager.session_adoption_token("probe-overflow").is_none());
        tokens.pop();
        assert!(
            manager.session_adoption_token("probe-recovered").is_some(),
            "dropping the final token frees its exact admission"
        );
    }

    #[tokio::test]
    async fn serving_fence_kills_existing_and_transition_racing_children() {
        use plurx_core::cluster::migration::status::ReplicationMonitor;
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("serving-fence root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let manager = Arc::new(
            TranscodeManager::new(
                store,
                root.path().join("manager"),
                EncoderCaps::default(),
                Pipeline::Cpu,
            )
            .with_serving_authority(fence.authority()),
        );
        let existing = watchdog_session(
            &root.path().join("existing"),
            Some(long_running_child()),
            false,
        );
        manager
            .sessions
            .lock()
            .await
            .insert("existing".to_owned(), Arc::clone(&existing));

        let fence_loop = tokio::spawn(Arc::clone(&manager).serving_fence_loop(fence.subscribe()));
        // Publish loss and recovery without yielding. A boolean watch could
        // coalesce this to `true` and preserve the old child; the generation
        // makes the lost authority permanent for generation zero.
        fence.validation_set_ready(false).await;
        fence.validation_set_ready(true).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let stopped = existing.retirement_cleanup_finished.load(Acquire);
                if existing.control.is_retired()
                    && stopped
                    && manager.sessions.lock().await.is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("existing child must be retired promptly");

        fence.validation_set_ready(false).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.serving_ready.load(Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("manager gate must close");

        // This insertion linearizes after the fence loop's false store and
        // after its first snapshot. `register_session` is the other half of
        // the race proof: the late child must retire itself.
        let late = watchdog_session(&root.path().join("late"), Some(long_running_child()), false);
        assert_eq!(
            manager.register_session("late", Arc::clone(&late), 0).await,
            Err(SessionRegistrationRejection::ServingFence),
            "a transition-racing session must not publish"
        );
        assert!(late.control.is_retired());
        assert!(manager.sessions.lock().await.is_empty());
        assert!(
            late.child.lock().await.is_none(),
            "the rejected late child must already be reaped and released"
        );

        drop(fence);
        fence_loop.await.expect("serving fence loop");
    }

    #[tokio::test]
    async fn initial_registration_reauthorizes_after_waiting_for_the_registry() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("registration root");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session = watchdog_session(
            &root.path().join("delayed"),
            Some(long_running_child()),
            false,
        );

        let registry = manager.sessions.lock().await;
        let registration = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move { manager.register_session("delayed", session, 0).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !registration.is_finished(),
            "the fixture must actually wait behind the registry lock"
        );
        session.control.end().await.expect("end verdict");
        drop(registry);

        assert_eq!(
            registration.await.expect("registration task"),
            Err(SessionRegistrationRejection::Producer(
                crate::playback_control::ProducerAttemptRejection::SessionEnded
            ))
        );
        assert!(manager.sessions.lock().await.is_empty());
        assert!(
            session.child.lock().await.is_none(),
            "the unregistered child is synchronously reaped and released"
        );
    }
