
    #[tokio::test]
    async fn cancelled_copy_registration_rejection_keeps_exact_cleanup_ownership() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("cancelled copy registration root");
        let scratch = root.path().join("copy-scratch");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("create copy scratch");
        tokio::fs::write(scratch.join("index.m3u8"), b"unpublished")
            .await
            .expect("seed copy scratch");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            root.path().join("manager"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let admissions = Admissions::new();
        let mut session = watchdog_session(&scratch, Some(long_running_child()), false);
        Arc::get_mut(&mut session)
            .expect("unshared copy fixture")
            .method = crate::delivery::Method::HlsCopy;
        reserve_test_admissions(&session, &admissions);
        let handoff_pause = Arc::new(LifecycleTestPause::new());
        *session
            .retirement_cleanup_handoff_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&handoff_pause));
        session
            .control
            .end()
            .await
            .expect("pre-registration terminal");

        let registration = tokio::spawn({
            let manager = Arc::clone(&manager);
            let session = Arc::clone(&session);
            async move {
                manager
                    .register_session("unregistered-copy", session, 0)
                    .await
            }
        });
        await_lifecycle_pause(&handoff_pause).await;
        assert!(manager.sessions.lock().await.is_empty());
        assert_eq!(admissions.in_use(), 1);
        assert_eq!(admissions.software_in_use(), 2);
        assert!(session.child.lock().await.is_some());

        registration.abort();
        assert!(registration
            .await
            .expect_err("registration waiter must cancel")
            .is_cancelled());
        handoff_pause.release.notify_one();

        tokio::time::timeout(Duration::from_secs(2), async {
            while !session.retirement_cleanup_finished.load(Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached unregistered copy cleanup must settle");
        assert!(session.child.lock().await.is_none());
        assert_eq!(admissions.in_use(), 0);
        assert_eq!(admissions.software_in_use(), 0);
        await_scratch_removed(&scratch).await;
    }

    /// A process that outlives the test unless lifecycle cleanup reaps it.
    fn long_running_child() -> Child {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("60").kill_on_drop(true);
        cmd.spawn().expect("spawn sleep")
    }

    fn successful_child() -> Child {
        let mut cmd = tokio::process::Command::new("true");
        cmd.kill_on_drop(true);
        cmd.spawn().expect("spawn successful child")
    }

    /// An actor-authorized replacement can begin immediately before a viewer
    /// stops the session. If replacement already owns `child_transition`,
    /// teardown must wait for successor publication and then kill that
    /// successor; removing the manager entry independently lets an untracked
    /// encoder survive the stop.
    #[tokio::test]
    async fn stop_waits_for_replacement_and_kills_its_successor() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("dir");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .replacement_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        manager
            .sessions
            .lock()
            .await
            .insert("retirement-race".into(), Arc::clone(&session));

        let replacement = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                let (replacement, producer_attempt) = session
                    .kill_child_for_replacement()
                    .await
                    .expect("the live session may begin actor-authorized replacement");
                *session.child.lock().await = Some(AttemptChild::new(
                    producer_attempt,
                    long_running_child(),
                    session.control.clone(),
                    None,
                ));
                replacement.complete();
            }
        });
        pause.wait().await;

        let stop = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move {
                assert!(manager.stop_session("retirement-race", "test").await);
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.retirement_started.load(Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stop must reach the shared transition");
        pause.wait().await;

        replacement.await.expect("copy fallback task");
        stop.await.expect("stop task");
        assert!(
            manager
                .sessions
                .lock()
                .await
                .get("retirement-race")
                .is_none(),
            "the stopped session must stay retired"
        );
        assert!(
            session.child.lock().await.is_none(),
            "no replacement producer may survive confirmed session retirement"
        );

        // Scratch is not the lifetime oracle. Even if a stale directory is
        // present again after teardown, the monotonic retirement verdict must
        // refuse an already-decided replacement.
        tokio::fs::create_dir_all(&session.dir)
            .await
            .expect("recreate stale scratch");
        assert_eq!(
            session.kill_child_for_replacement().await.err(),
            Some(crate::playback_control::ProducerAttemptRejection::SessionEnded),
            "a retired session must never publish another producer"
        );
    }

    #[tokio::test]
    async fn fenced_session_is_not_renewable_while_teardown_is_blocked() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("dir");
        let session = watchdog_session(dir.path(), Some(long_running_child()), false);
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        manager
            .sessions
            .lock()
            .await
            .insert("fenced-renewal".into(), Arc::clone(&session));
        let transition = session.child_transition.lock().await;

        manager.fence_sessions(&["fenced-renewal".to_owned()]).await;
        assert_eq!(
            session
                .control
                .snapshot()
                .await
                .expect("authority-fenced snapshot")
                .terminal,
            Some(crate::playback_control::RollingTerminalCause::AuthorityFence)
        );
        assert_eq!(
            manager.active_session_ids().await,
            vec!["fenced-renewal".to_owned()],
            "the teardown barrier keeps the worker discoverable for cleanup"
        );
        assert!(
            manager.renewable_session_ids().await.is_empty(),
            "the same fenced worker must never be submitted for renewal"
        );
        drop(transition);
        assert!(manager.stop_session("fenced-renewal", "test").await);
        assert_eq!(
            session
                .control
                .snapshot()
                .await
                .expect("cleanup preserves terminal snapshot")
                .terminal,
            Some(crate::playback_control::RollingTerminalCause::AuthorityFence),
            "later cleanup cannot relabel authority loss as an ordinary end"
        );
    }

    /// The inverse transition ordering matters too: a hardware fallback can
    /// decide to downgrade immediately before the serving fence retires its
    /// session, then arrive at `child_transition` only after teardown. The
    /// real downgrade entry point must treat that work as stale instead of
    /// installing a new ffmpeg process into an unregistered session.
    #[tokio::test]
    async fn retirement_wins_before_production_fallback_and_prevents_successor() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let dir = crate::test_tempdir().expect("session dir");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        let predecessor_pid = session
            .child
            .lock()
            .await
            .as_ref()
            .and_then(AttemptChild::id)
            .expect("placeholder process id");
        let predecessor_generation = session.progress.generation();
        mgr.sessions
            .lock()
            .await
            .insert("retirement-first".into(), Arc::clone(&session));

        assert!(mgr.stop_session("retirement-first", "test").await);
        // Scratch presence is not authority. Recreate it so this regression
        // proves the monotonic retirement verdict is what rejects fallback.
        tokio::fs::create_dir_all(&session.dir)
            .await
            .expect("recreate stale scratch");

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
        // Retirement has already won. The production retry executor is the
        // thing that must refuse to resurrect an encoder afterwards, so it is
        // what this regression drives — the retired ladder helper would prove
        // nothing about the path production takes.
        let retry = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            dir.path(),
            "retirement-first",
        )
        .await
        .expect("a CPU-pipeline hardware attempt has a software rung");
        let refused = execute_prepublication_transcode_retry(
            Arc::clone(&session),
            &retry,
            1,
            1,
            &retry.actor_recipe,
            crate::playback_control::ProducerDecisionReason::ProgressDeadline,
            "retirement-first",
        )
        .await;
        assert!(
            refused.is_err(),
            "a retired session must refuse the retry rather than install a successor"
        );

        assert!(session.control.is_retired());
        assert!(
            mgr.sessions.lock().await.get("retirement-first").is_none(),
            "the retired session must remain unregistered"
        );
        let child = session.child.lock().await;
        assert_eq!(
            session.progress.generation(),
            predecessor_generation,
            "fallback must not begin or publish a successor process after retirement (predecessor pid {predecessor_pid})"
        );
        assert!(child.is_none(), "the retired predecessor must be reaped");
    }

    // ---- the pre-transcode cache, serving side (PERF-PLAN §6.3) -------------

    const NODE: &str = "node-under-test";

    /// A manager with a cache root, and the cache root's temp dir (which has to
    /// outlive the manager or the directory goes out from under it).
    fn cached_manager(
        store: &Arc<dyn Store>,
    ) -> (TranscodeManager, tempfile::TempDir, tempfile::TempDir) {
        let work = crate::test_tempdir().expect("work");
        let cache = crate::test_tempdir().expect("cache");
        let mgr = TranscodeManager::new(
            Arc::clone(store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_cache(
            cache.path().to_path_buf(),
            "ffmpeg version 6.1.1-test".into(),
            NODE.into(),
        );
        (mgr, work, cache)
    }

    async fn recipe_hash_for_options(
        mgr: &TranscodeManager,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
    ) -> String {
        let plan = mgr
            .resolve_movie_plan(file, opts, encoder)
            .await
            .expect("resolve recipe plan");
        let digest = mgr.digest().expect("cache configured");
        mgr.effective_recipe(&digest, &plan, false).hash()
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_test_transcode_retry(
        mgr: &TranscodeManager,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
        software_rate_control: EffectiveRateControl,
        pacing: Pacing,
        dir: &std::path::Path,
        fingerprint: &str,
    ) -> Result<PrepublicationTranscodeRetry, String> {
        let prepared =
            PrepublicationTranscodeRetry::prepare(file, opts, encoder, software_rate_control)?;
        let plan = mgr
            .resolve_movie_plan(file, &prepared.opts, prepared.encoder)
            .await?;
        PrepublicationTranscodeRetry::build(
            file,
            prepared,
            &plan,
            pacing,
            &dir.to_string_lossy(),
            fingerprint,
            mgr.admissions.software_pool(),
            mgr.software_budget().await,
            mgr.runtime_cache.clone(),
            &mgr.measured_decoders,
            false,
            "one-step-color-safe",
        )
    }

    /// The name `start()` would look this session up under. Computed through
    /// the manager's own builders, because a test that spelled the recipe out
    /// by hand would keep passing after the two spellings diverged — which is
    /// the failure the single builder exists to prevent.
    async fn recipe_hash_for(
        mgr: &TranscodeManager,
        file: &plurx_core::domain::MediaFile,
        height: i64,
    ) -> String {
        let encoder = mgr.encoder().await;
        let opts = mgr.options_for_tone_map(
            encoder,
            file,
            height,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(file, &opts, encoder)
            .await
            .expect("resolve cache recipe plan");
        let digest = mgr.digest().expect("cache configured");
        mgr.effective_recipe(&digest, &plan, false).hash()
    }

    /// A retry that changes the decode route is a different production, and
    /// the two ways it could pretend otherwise are publishing under the failed
    /// plan's name and resuming the failed producer's prefix. Both are closed
    /// here: the alternative resolves to its own key, and the retry builder
    /// refuses to carry a plan that disagrees with the route it prepared.
    #[tokio::test]
    async fn an_alternative_plan_cannot_publish_or_resume_under_the_failed_plans_key() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let dir = crate::test_tempdir().expect("session dir");

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

        let digest = mgr.digest().expect("cache configured");
        let failed_plan = mgr
            .resolve_movie_plan(&file, &opts, Encoder::VideoToolbox)
            .await
            .expect("the failed hardware plan");
        let failed_hash = mgr.effective_recipe(&digest, &failed_plan, false).hash();

        let prepared = PrepublicationTranscodeRetry::prepare(
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
        )
        .expect("a CPU-pipeline hardware attempt has a software rung");
        let alternative_plan = mgr
            .resolve_movie_plan(&file, &prepared.opts, prepared.encoder)
            .await
            .expect("the alternative plan");
        let alternative_hash = mgr
            .effective_recipe(&digest, &alternative_plan, false)
            .hash();

        assert_ne!(
            failed_plan.plan_digest(),
            alternative_plan.plan_digest(),
            "a changed decode route is a changed plan"
        );
        assert_ne!(
            failed_hash, alternative_hash,
            "the alternative must not publish under the failed plan's key"
        );

        let retry = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
            Pacing::unpaced(),
            dir.path(),
            "alternative-plan-key",
        )
        .await
        .expect("build the alternative");
        // The retry's fingerprint and the recipe hash are digests over
        // different field lists, so asserting they differ proves nothing but
        // the absence of a SHA-256 collision. What is worth proving is below.
        let _ = &retry.actor_recipe.fingerprint;

        // The prefix protection itself: a retry prepared for the software
        // route cannot be constructed carrying the hardware plan it replaced,
        // so there is no assembled object that could resume it.
        let prepared_again = PrepublicationTranscodeRetry::prepare(
            &file,
            &opts,
            Encoder::VideoToolbox,
            EffectiveRateControl::Vbr,
        )
        .expect("prepare again");
        let mismatched = PrepublicationTranscodeRetry::build(
            &file,
            prepared_again,
            &failed_plan,
            Pacing::unpaced(),
            &dir.path().to_string_lossy(),
            "alternative-plan-key",
            mgr.admissions.software_pool(),
            mgr.software_budget().await,
            mgr.runtime_cache.clone(),
            &mgr.measured_decoders,
            false,
            "one-step-color-safe",
        );
        assert!(
            mismatched.is_err(),
            "a retry must refuse the plan of the route it is replacing"
        );
    }

    /// Staged parts are only worth resuming if they were produced under the
    /// same recipe. A prefix staged under a different plan carries a different
    /// v3 hash, so it must be quarantined rather than assembled into this
    /// encode — the one cache failure that is not an error, just the wrong
    /// film.
    ///
    /// Both hashes come from `effective_recipe`, not from two arbitrary
    /// strings: a test that hand-wrote them would pass on any recipe
    /// composition, including one that had stopped telling the two plans apart.
    #[tokio::test]
    async fn a_staged_prefix_from_another_plan_is_quarantined_by_the_v3_recipe_hash() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let staged_file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let stale_hash = recipe_hash_for(&mgr, &staged_file, 480).await;
        let planned_hash = recipe_hash_for(&mgr, &staged_file, 1080).await;
        assert_ne!(
            stale_hash, planned_hash,
            "two plans, two names — otherwise this test proves nothing"
        );

        let root = crate::test_tempdir().expect("staging root");
        let parent = plurx_core::fs_secure::SecureDirectory::open(root.path())
            .await
            .expect("open staging parent");
        let staging = parent
            .create_child_directory("job-staging")
            .await
            .expect("create staging");

        let source_path = root.path().join("source.mkv");
        tokio::fs::write(&source_path, b"source bytes")
            .await
            .expect("write source");
        let source = LocalSourceSnapshot::from_metadata(
            &tokio::fs::metadata(&source_path).await.expect("metadata"),
        );

        let job = pretranscode_job_fixture();
        let stale = PretranscodeStagingIdentity {
            job_id: job.id.clone(),
            file_id: job.file_id,
            source_size: job.source_size,
            source_mtime: job.source_mtime,
            policy_generation: job.policy_generation.clone(),
            // What a producer working under a different plan wrote.
            recipe_hash: stale_hash.clone(),
            source,
        };
        write_pretranscode_staging_identity(&staging, &stale)
            .await
            .expect("write the stale identity");
        staging
            .atomic_write_child("part-000.ts", b"bytes from the old encode")
            .await
            .expect("retain a part");
        assert!(staging
            .read_bounded_child("part-000.ts", 4_096)
            .await
            .is_ok());

        let rebound =
            bind_pretranscode_staging(&parent, "job-staging", staging, &job, &planned_hash, source)
                .await
                .expect("rebind the staging root");

        assert!(
            rebound
                .read_bounded_child("part-000.ts", 4_096)
                .await
                .is_err(),
            "a part produced under another recipe must not survive into this one"
        );
        let bound = read_pretranscode_staging_identity(&rebound)
            .await
            .expect("the replacement carries an identity");
        assert_eq!(bound.recipe_hash, planned_hash);
        assert_eq!(bound.job_id, job.id);
    }

    fn pretranscode_job_fixture() -> plurx_core::domain::PretranscodeJob {
        plurx_core::domain::PretranscodeJob {
            id: "job-under-test".to_owned(),
            dedupe_key: "dedupe-under-test".to_owned(),
            file_id: 7,
            source_size: 12_345,
            source_mtime: 67_890,
            target_height: 720,
            policy_generation: "policy-1".to_owned(),
            requirements_json: "{}".to_owned(),
            reason: "test".to_owned(),
            priority: 0,
            state: "running".to_owned(),
            owner_node_id: NODE.to_owned(),
            fence: 1,
            lease_expires_ms: 0,
            attempts: 1,
            not_before_ms: 0,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    /// Write what a finished transcode looks like on disk.
    async fn seed_cache_dir(root: &std::path::Path, rel: &str) -> PathBuf {
        let dir = root.join(rel);
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        seeded_session_dir(&dir, 3, 2.0).await;
        let playlist_path = dir.join("index.m3u8");
        let playlist = tokio::fs::read_to_string(&playlist_path)
            .await
            .expect("read seeded playlist")
            .replace("#EXT-X-PLAYLIST-TYPE:EVENT", "#EXT-X-PLAYLIST-TYPE:VOD")
            + "#EXT-X-ENDLIST\n";
        tokio::fs::write(&playlist_path, playlist)
            .await
            .expect("finish seeded VOD playlist");
        dir
    }

    async fn complete_manifest_cache(
        store: &Arc<dyn Store>,
        file_id: i64,
        recipe: &str,
        relative: &str,
        manifest_digest: &str,
    ) {
        use plurx_core::cluster::coordination::LeaseClaim;
        use plurx_core::domain::{
            NewPretranscodeJob, PretranscodeRequirements, PretranscodeWorkerCapabilities,
        };

        let job_id = "00000000-0000-4000-8000-000000000601";
        let lease_now = unix_ms();
        let lease = match store
            .acquire_lease(
                "transcode-manifest-session",
                "scheduler",
                lease_now,
                lease_now.saturating_add(90_000),
            )
            .await
            .expect("candidate lease")
        {
            LeaseClaim::Acquired(lease) => lease,
            other => panic!("candidate lease held: {other:?}"),
        };
        let requirements = serde_json::to_string(&PretranscodeRequirements {
            version: PretranscodeRequirements::VERSION,
            decoder: "hevc".to_owned(),
            acceptable_encoder_families: vec!["software".to_owned()],
            output_contract: "hls-mpegts-v1".to_owned(),
            tone_map: false,
            output_grade: "sdr".to_owned(),
            scratch_bytes: 1,
        })
        .expect("requirements");
        assert!(store
            .enqueue_pretranscode_job(
                &NewPretranscodeJob {
                    id: job_id.to_owned(),
                    dedupe_key: "transcode-manifest-session".to_owned(),
                    file_id,
                    source_size: 1,
                    source_mtime: 1,
                    target_height: 1080,
                    policy_generation: "session-integrity-v1".to_owned(),
                    requirements_json: requirements,
                    reason: "recent".to_owned(),
                    priority: 100,
                    not_before_ms: lease_now,
                    created_at_ms: lease_now,
                },
                &lease,
                &lease
                    .publication_successor()
                    .expect("publication successor"),
            )
            .await
            .expect("enqueue manifest session"));
        let claimed = store
            .claim_pretranscode_job(
                NODE,
                &PretranscodeWorkerCapabilities {
                    version: PretranscodeRequirements::VERSION,
                    decoders: vec!["hevc".to_owned()],
                    encoder_families: vec!["software".to_owned()],
                    max_target_height: 2_160,
                    output_contracts: vec!["hls-mpegts-v1".to_owned()],
                    tone_map: false,
                    output_grades: vec!["sdr".to_owned()],
                    scratch_bytes: 2,
                },
                &[],
                lease_now,
                lease_now.saturating_add(90_000),
            )
            .await
            .expect("claim manifest session")
            .expect("manifest session job");
        assert!(store
            .complete_pretranscode_job(
                &claimed,
                recipe,
                CACHE_RECIPE_VERSION,
                relative,
                1_234,
                None,
                manifest_digest,
                lease_now.saturating_add(1),
            )
            .await
            .expect("complete manifest session"));
    }

    #[tokio::test]
    async fn an_unlisted_safe_segment_cannot_evict_a_valid_cached_generation() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = Arc::new(mgr);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;
        let relative = "fa/manifest-session";
        let dir = seed_cache_dir(cache.path(), relative).await;
        let manifest = plurx_core::transcode::manifest::publish(
            &dir,
            "00000000-0000-4000-8000-000000000601:1",
            &[
                "index.m3u8".to_owned(),
                "seg00000.ts".to_owned(),
                "seg00001.ts".to_owned(),
                "seg00002.ts".to_owned(),
            ],
        )
        .await
        .expect("publish generation manifest");
        complete_manifest_cache(&store, file_id, &hash, relative, &manifest.manifest_digest).await;

        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-manifest")
            .await
            .expect("cached start");
        assert_eq!(info.encoder, "cached");
        assert!(mgr
            .segment(&info.session_id, "seg99999.ts")
            .await
            .expect("segment admission")
            .is_none());
        assert_eq!(
            mgr.active_sessions().await,
            1,
            "an unlisted probe retired the valid session"
        );
        assert!(
            store
                .cache_hit(&hash, NODE)
                .await
                .expect("cache lookup")
                .is_some(),
            "an unlisted probe invalidated the valid cache location"
        );
        assert!(mgr
            .segment(&info.session_id, "seg00000.ts")
            .await
            .expect("segment admission")
            .is_some());

        tokio::fs::write(dir.join("seg00001.ts"), b"corrupt listed object")
            .await
            .expect("corrupt listed segment");
        let corrupt_session = mgr
            .sessions
            .lock()
            .await
            .get(&info.session_id)
            .cloned()
            .expect("cached session before integrity failure");
        let retirement_gate = corrupt_session.child_transition.lock().await;
        let error = match mgr
            .segment_for_publication(&info.session_id, "seg00001.ts")
            .await
            .expect("segment admission")
        {
            SegmentPublication::Failed(error) => error,
            _ => panic!("corrupt listed segment did not publish a typed failure"),
        };
        assert!(corrupt_session
            .cache_integrity_cleanup_started
            .load(Acquire));
        assert_eq!(
            error.error,
            PlaylistError::SessionFailed(CACHED_MEDIA_INTEGRITY_FAILURE.to_owned()),
            "segment integrity publishes its exact failure before detached cleanup"
        );
        assert_eq!(
            mgr.active_sessions().await,
            1,
            "the detecting request does not own retirement while its gate is held"
        );
        drop(retirement_gate);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let retired = mgr.active_sessions().await == 0;
                let invalidated = store
                    .cache_hit(&hash, NODE)
                    .await
                    .expect("cache lookup")
                    .is_none();
                if retired && invalidated {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached segment-integrity cleanup");

        let owner = error.owner.as_ref().expect("exact cached failure owner");
        mgr.authorize_playlist_error_publication(
            &info.session_id,
            owner,
            &error.error,
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("retired exact segment failure remains publishable");
    }

    #[tokio::test]
    async fn cached_playlist_integrity_cleanup_survives_request_cancellation_and_keeps_owner() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = Arc::new(mgr);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;
        let relative = "fb/playlist-integrity-owner";
        let dir = seed_cache_dir(cache.path(), relative).await;
        let manifest = plurx_core::transcode::manifest::publish(
            &dir,
            "00000000-0000-4000-8000-000000000601:1",
            &[
                "index.m3u8".to_owned(),
                "seg00000.ts".to_owned(),
                "seg00001.ts".to_owned(),
                "seg00002.ts".to_owned(),
            ],
        )
        .await
        .expect("publish generation manifest");
        complete_manifest_cache(&store, file_id, &hash, relative, &manifest.manifest_digest).await;

        let info = mgr
            .start(
                file_id,
                1080,
                0.0,
                None,
                None,
                "paul",
                "pb-playlist-integrity",
            )
            .await
            .expect("cached start");
        let session = mgr
            .sessions
            .lock()
            .await
            .get(&info.session_id)
            .cloned()
            .expect("cached session before integrity failure");
        tokio::fs::write(dir.join("index.m3u8"), b"corrupt cached playlist")
            .await
            .expect("corrupt cached playlist");

        // Pin retirement after the detached owner has been installed. The
        // detecting request can then be cancelled without also owning the
        // registry removal that its exact failure response must survive.
        let retirement_gate = session.child_transition.lock().await;
        let (error_tx, error_rx) = tokio::sync::oneshot::channel();
        let request_manager = Arc::clone(&mgr);
        let request_session_id = info.session_id.clone();
        let request = tokio::spawn(async move {
            let error = match request_manager
                .playlist_with_owner(&request_session_id)
                .await
            {
                Err(error) => error,
                Ok(_) => panic!("corrupt cached playlist was published"),
            };
            let _ = error_tx.send(error);
            std::future::pending::<()>().await;
        });
        let error = tokio::time::timeout(Duration::from_secs(2), error_rx)
            .await
            .expect("synchronous cached-playlist failure")
            .expect("request published its failure owner");
        assert_eq!(
            error.error,
            PlaylistError::SessionFailed(CACHED_MEDIA_INTEGRITY_FAILURE.to_owned())
        );
        assert!(session.cache_integrity_cleanup_started.load(Acquire));
        request.abort();
        assert!(request
            .await
            .expect_err("cancelled detecting request")
            .is_cancelled());

        tokio::time::timeout(Duration::from_secs(2), async {
            while store
                .cache_hit(&hash, NODE)
                .await
                .expect("cache lookup")
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached invalidation survives request cancellation");
        assert_eq!(
            mgr.active_sessions().await,
            1,
            "the detached owner is still waiting on exact retirement"
        );

        drop(retirement_gate);
        tokio::time::timeout(Duration::from_secs(2), async {
            while mgr.active_sessions().await != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached retirement survives request cancellation");

        let owner = error.owner.as_ref().expect("exact cached failure owner");
        mgr.authorize_playlist_error_publication(
            &info.session_id,
            owner,
            &error.error,
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("retired exact cached failure remains publishable");
    }

    #[tokio::test]
    async fn media_offer_claims_only_a_byte_verified_complete_generation() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;
        let relative = "fb/media-offer";
        let dir = seed_cache_dir(cache.path(), relative).await;
        let manifest = plurx_core::transcode::manifest::publish(
            &dir,
            "00000000-0000-4000-8000-000000000601:1",
            &[
                "index.m3u8".to_owned(),
                "seg00000.ts".to_owned(),
                "seg00001.ts".to_owned(),
                "seg00002.ts".to_owned(),
            ],
        )
        .await
        .expect("publish generation manifest");
        complete_manifest_cache(&store, file_id, &hash, relative, &manifest.manifest_digest).await;
        let encoder = mgr.encoder().await;
        let opts = mgr.options_for_tone_map(
            encoder,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(&file, &opts, encoder)
            .await
            .expect("resolve offer-verification plan");

        assert!(
            !mgr.verified_cache_hit(&plan).await,
            "an offer fails closed while its single verifier is running"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if mgr.verified_cache_hit(&plan).await {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background cache-offer verdict");
        tokio::fs::write(dir.join("index.m3u8"), b"corrupt")
            .await
            .expect("corrupt playlist");
        mgr.cache_offer_verdicts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&hash);
        assert!(!mgr.verified_cache_hit(&plan).await);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if store
                    .cache_hit(&hash, NODE)
                    .await
                    .expect("cache lookup")
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background invalidation verdict");
    }

    #[tokio::test]
    async fn media_offer_never_opens_an_unproved_profile_five_source() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let manager = TranscodeManager::new(
            store,
            work.path().to_owned(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_dovi_reshape(true);
        let mut file = profile5_file();
        file.path = work.path().join("sleeping-nas/movie.mkv");

        assert!(matches!(
            manager
                .media_offer_probe(&file, 1080, 0.0, None, None, false)
                .await,
            Err("source_proof_unavailable")
        ));
        assert!(
            !file.path.exists(),
            "offer fanout created or materialized the absent source path"
        );
    }

    /// The three ways a lookup can go, and only one of them is a hit.
    ///
    /// The middle case is the one worth writing down: a row that says the bytes
    /// are there is not the bytes being there. A cache root on a mount that did
    /// not come back after a reboot leaves every row intact and every directory
    /// gone, and a lookup that trusted the row would hand out a playlist for an
    /// empty directory — an error the viewer sees as a film that will not play,
    /// with nothing in the log to say why.
    #[tokio::test]
    async fn a_lookup_hits_only_on_a_finished_entry_whose_bytes_are_really_there() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;
        let encoder = mgr.encoder().await;
        let opts = mgr.options_for_tone_map(
            encoder,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(&file, &opts, encoder)
            .await
            .expect("resolve lookup plan");
        let look = || {
            mgr.serve_cached(
                &file,
                &opts,
                &plan,
                "Heat",
                SessionOwner {
                    user_name: "paul",
                    supersession_user: r#"["username","paul"]"#,
                    playback_id: "pb-1",
                    automatic: true,
                },
            )
        };

        // Nothing claimed yet.
        assert!(look().await.is_none(), "an unknown recipe is a miss");

        // Claimed but unfinished: a directory a producer is still writing into.
        let dir = seed_cache_dir(cache.path(), "ab/entry").await;
        store
            .claim_cache_entry(&hash, file_id, 1, NODE, "ab/entry")
            .await
            .expect("claim");
        assert!(
            look().await.is_none(),
            "a claim is not a hit — that playlist stops in the middle of the film"
        );

        store
            .complete_cache_entry(&hash, NODE, 1_234, None)
            .await
            .expect("complete");
        let hit = look().await.expect("a finished entry serves");
        assert_eq!(
            mgr.sessions.lock().await[hit.session_id.as_str()]
                .delivery
                .method(),
            "transcode",
            "a cache hit serves an encoded rendition: transcode bytes"
        );
        assert!(
            hit.vod,
            "the whole stream exists; the player may seek freely"
        );
        assert_eq!(hit.encoder, "cached");
        assert_eq!(
            hit.start_seconds, 0.0,
            "a cached asset is the whole title — where a viewer joins is a seek"
        );
        assert_eq!(mgr.active_sessions().await, 1);
        assert!(mgr.stop_session(&hit.session_id, "test").await);

        // The row survives what the filesystem does not.
        tokio::fs::remove_file(dir.join("index.m3u8"))
            .await
            .expect("rm playlist");
        assert!(
            look().await.is_none(),
            "a row pointing at a directory with no playlist is a miss, not a 404 for the viewer"
        );
    }

    /// The whole point, end to end: a hit plays with no encoder, no hardware
    /// slot and no queue — and, the part that would destroy the cache, the
    /// bytes are still there afterwards.
    ///
    /// Every other way a session ends removes its directory, correctly. A
    /// cached session reaching one of those paths unguarded would delete the
    /// entry that served it, so each hit would cost the next viewer a full
    /// re-encode and the cache would sit permanently empty while looking like
    /// it was working.
    #[tokio::test]
    async fn a_hit_starts_no_encoder_and_outlives_the_session_that_played_it() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = Arc::new(mgr);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;
        let dir = seed_cache_dir(cache.path(), "cd/entry").await;
        store
            .claim_cache_entry(&hash, file_id, 1, NODE, "cd/entry")
            .await
            .expect("claim");
        store
            .complete_cache_entry(&hash, NODE, 1_234, None)
            .await
            .expect("complete");

        // No `require_ffmpeg` here on purpose: if this ever spawns one, the
        // test should fail rather than quietly start passing on a box that has
        // ffmpeg installed.
        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-1")
            .await
            .expect("start");
        assert!(info.vod);
        assert_eq!(info.encoder, "cached");
        let session = mgr
            .sessions
            .lock()
            .await
            .get(&info.session_id)
            .cloned()
            .expect("registered");
        assert!(
            session.child.lock().await.is_none(),
            "a hit has nothing to run"
        );
        assert_eq!(
            mgr.admissions.in_use(),
            0,
            "and nothing to wait behind — the work is already done"
        );
        assert!(mgr.playlist(&info.session_id).await.is_ok());

        // Retention must not treat a finished asset as this session's scratch.
        // Pretend the viewer has watched well past the window and let the
        // pruner have its pass.
        session.refresh_segments().await;
        session
            .fetched_end_ms
            .store((RETENTION_SECS + 600) * 1000, Relaxed);
        gc_expired_segments(&session).await;
        assert_eq!(
            session.ahead_bytes.load(Relaxed),
            0,
            "a cache entry is not scratch, and must not push live encoders over the global budget"
        );
        assert_eq!(
            session.live_bytes.load(Relaxed),
            0,
            "on the disk-budget figure exactly as on the pacing one"
        );

        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert!(
            tokio::fs::metadata(dir.join("index.m3u8")).await.is_ok(),
            "the cache entry outlives the session that played it"
        );
        assert!(
            tokio::fs::metadata(dir.join("seg00000.ts")).await.is_ok(),
            "and so do its segments — the pruner does not eat a cached asset"
        );
        assert!(
            store.cache_hit(&hash, NODE).await.expect("hit").is_some(),
            "so the next viewer hits it too"
        );
    }

    /// Cache housekeeping and serving share one ownership registry. A budget
    /// change may leave the cache temporarily over its ceiling, but it may
    /// never remove a VOD playlist or segment while a viewer can still ask for
    /// it. The next sweep reclaims the entry after that session ends.
    #[tokio::test]
    async fn active_cache_playback_is_not_evicted_under_its_reader() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;
        let dir = seed_cache_dir(cache.path(), "ef/entry").await;
        store
            .claim_cache_entry(&hash, file_id, 1, NODE, "ef/entry")
            .await
            .expect("claim");
        store
            .complete_cache_entry(&hash, NODE, 1_234, None)
            .await
            .expect("complete");
        store
            .put_setting(keys::CACHE_MAX_GB, "0")
            .await
            .expect("disable cache");

        let info = mgr
            .start(file_id, 1080, 0.0, None, None, "paul", "pb-reader")
            .await
            .expect("cached start");
        assert_eq!(info.encoder, "cached");

        let active = crate::cachekeep::sweep_with_readers(
            &store,
            cache.path(),
            NODE,
            mgr.cache_readers(),
            0,
        )
        .await;
        assert_eq!((active.evicted, active.protected), (0, 1));
        assert!(
            dir.join("index.m3u8").exists(),
            "the budget sweep removed an active viewer's playlist"
        );
        assert!(
            store.cache_hit(&hash, NODE).await.expect("hit").is_some(),
            "the active entry's row must stay with its bytes"
        );

        assert!(mgr.stop_session(&info.session_id, "test").await);
        let idle = crate::cachekeep::sweep_with_readers(
            &store,
            cache.path(),
            NODE,
            mgr.cache_readers(),
            1,
        )
        .await;
        assert_eq!((idle.evicted, idle.protected), (1, 0));
        assert!(!dir.exists(), "an idle over-budget entry was not reclaimed");
        assert!(
            store.cache_hit(&hash, NODE).await.expect("miss").is_none(),
            "eviction removed the bytes but left a serveable row"
        );
    }

    /// The producer and the serving path, against each other, with a real
    /// ffmpeg.
    ///
    /// This is the only test that can catch the two disagreeing, and the
    /// disagreement is silent: a producer that hashes its output one way and a
    /// playback that looks it up another produces a cache that fills forever
    /// and never hits, which from outside is indistinguishable from a cache
    /// that is simply cold. Every unit test above passes in that world.
    #[tokio::test]
    async fn what_the_producer_makes_is_what_a_playback_finds() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source = media.path().join("Heat.mkv");
        write_real_video(&source, 6);
        let file_id = seed_real_file(&store, &source).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = Arc::new(mgr);
        let file = store.get_file(file_id).await.expect("get").expect("file");

        let hash = mgr
            .produce(&file, 240, Instant::now() + Duration::from_secs(120))
            .await
            .expect("produce")
            .expect("something was produced")
            .recipe;

        // On disk as one continuous, finished asset — no part directories, no
        // gaps in the numbering, and an ENDLIST so a player treats it as VOD.
        let dir = cache.path().join(&hash[..2]).join(&hash);
        let playlist = tokio::fs::read_to_string(dir.join("index.m3u8"))
            .await
            .expect("playlist");
        assert!(playlist.contains("#EXT-X-ENDLIST"), "{playlist}");
        assert!(playlist.contains("#EXT-X-PLAYLIST-TYPE:VOD"), "{playlist}");
        let names: Vec<&str> = playlist.lines().filter(|l| l.ends_with(".ts")).collect();
        assert!(names.len() >= 2, "expected several segments: {playlist}");
        for (i, name) in names.iter().enumerate() {
            assert_eq!(*name, format!("seg{i:05}.ts"), "numbering has a gap");
            assert!(
                tokio::fs::metadata(dir.join(name)).await.is_ok(),
                "{name} is in the playlist but not on disk"
            );
        }
        let mut entries = tokio::fs::read_dir(&dir).await.expect("read dir");
        while let Ok(Some(e)) = entries.next_entry().await {
            assert!(
                !e.file_name().to_string_lossy().starts_with("part-"),
                "a part directory survived publication"
            );
        }
        // Nothing left in the staging area either.
        assert!(
            !cache.path().join("tmp").join(&hash).exists(),
            "the temp directory was published, not left behind"
        );

        // And the part that cannot be checked any other way: a real playback,
        // computing the recipe from its own inputs, finds it.
        let info = mgr
            .start(file_id, 240, 0.0, None, None, "paul", "pb-after-produce")
            .await
            .expect("start");
        assert!(
            info.vod,
            "the producer and the player disagree about what this transcode is called"
        );
        assert_eq!(info.encoder, "cached");
        assert!(mgr.playlist(&info.session_id).await.is_ok());
        assert!(mgr.stop_session(&info.session_id, "test").await);

        // Producing it again is a no-op rather than a second encode.
        assert!(
            mgr.produce(&file, 240, Instant::now() + Duration::from_secs(120))
                .await
                .expect("produce again")
                .is_none(),
            "an entry that already exists was produced a second time"
        );
    }

    /// An unfinished run keeps its claim and its bytes — and is never
    /// serveable.
    ///
    /// Those are the same fact from two sides, and the second is what makes the
    /// first safe. The claim is a *bookmark*: a two-hour 4K film on a contended
    /// box takes several passes, and a run that discarded its work each time it
    /// was interrupted would never finish one. Nothing can serve it in the
    /// meantime because a claim is not a hit, and if the node dies for good the
    /// stale-claim sweep takes the claim and its staging together.
    #[tokio::test]
    async fn an_unfinished_run_keeps_its_place_but_is_never_serveable() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        // The budget below is squeezed between two facts about the encoder,
        // and the two ends fail on opposite hardware:
        //
        //   too generous → a quick box finishes the whole film inside it, and
        //     "an unfinished run must not publish" fails on a run that finished
        //   too tight    → a busy box has not published a segment yet, and
        //     "the encoded part was thrown away" fails on a part never written
        //
        // Unpaced, both ends are facts about the CPU, and no constant satisfies
        // both: 700 ms failed the first way on a 16-core desktop, 100 ms failed
        // the second way on a 2-core runner, and 1200 ms failed the second way
        // again on the desktop once the suite around it got busier.
        //
        // Pacing the input fixes the upper end **arithmetically**: at READRATE,
        // a budget of B reaches B x READRATE seconds of source and no more, on
        // any hardware, so "it cannot have finished" stops being a hope. See
        // the pacing note in `a_preempted_producer_resumes_without_losing_picture`.
        //
        // The lower end cannot be closed the same way, because "ffmpeg starts
        // and publishes one segment" is genuinely a fact about the machine and
        // about what else is running on it. So it is not asserted on the first
        // try: a pass that produced nothing gets one more with twice the
        // budget, and only a second empty pass is a failure. That costs a slow
        // build a few seconds and costs a correct one nothing, where the
        // alternative — a bigger constant — costs every build every time and
        // still guesses.
        const SECONDS: u32 = 120;
        const READRATE: f64 = 5.0;
        // 25 s of a 120 s film on the first try and 51 s on the second, so
        // even the doubled budget is nowhere near the end; and 5 s for ffmpeg
        // to start against the 0.4 s of reading one segment needs.
        const PARTIAL_BUDGET: Duration = Duration::from_millis(5_000);

        if !crate::ffmpeg::pacing_caps().await.readrate {
            eprintln!(
                "SKIP: an_unfinished_run_keeps_its_place_but_is_never_serveable: \
                 `{}` has no -readrate (needs ffmpeg 5.1+), so the partial budget \
                 cannot be made independent of this machine's speed",
                crate::ffmpeg::ffmpeg_bin()
            );
            return;
        }

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source = media.path().join("Heat.mkv");
        write_real_video(&source, SECONDS);
        let file_id = seed_real_file(&store, &source).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = mgr.with_producer_tuning(ProducerTuning {
            pacing: Pacing {
                readrate: Some(READRATE),
                initial_burst: None,
                legacy_re: false,
            },
            ..ProducerTuning::default()
        });
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 240).await;

        // A short budget: it encodes some of the film and runs out of time.
        let staging = crate::cachekeep::staging_dir(cache.path(), &hash);
        let mut budget = PARTIAL_BUDGET;
        for attempt in 1..=2 {
            let started = Instant::now();
            let unfinished = mgr
                .produce(&file, 240, Instant::now() + budget)
                .await
                .expect("produce");
            assert!(
                unfinished.is_none(),
                "an unfinished run must not publish — but this box finished the \
                 whole {SECONDS}s fixture in {:?}, inside a {budget:?} budget \
                 that at {READRATE}x realtime reaches only {}s of it, so the \
                 pacing is not being applied rather than the cache being wrong",
                started.elapsed(),
                budget.as_secs_f64() * READRATE
            );
            if staging.join(crate::produce::part_dir(0)).exists() {
                break;
            }
            // Nothing was encoded, so there is nothing for the rest of this
            // test to be about. That is the lower end giving way — see above.
            assert!(
                attempt < 2,
                "two passes, {:?} and {:?}, and ffmpeg never published a \
                 segment: at {READRATE}x realtime one {}s segment is {}s of \
                 reading, so this box needed more than {budget:?} just to \
                 start an encoder",
                PARTIAL_BUDGET,
                budget,
                plurx_core::transcode::SEGMENT_SECONDS,
                f64::from(plurx_core::transcode::SEGMENT_SECONDS) / READRATE
            );
            budget *= 2;
        }

        assert!(
            store.cache_hit(&hash, NODE).await.expect("hit").is_none(),
            "an unfinished run must never be serveable"
        );
        assert!(
            !cache.path().join(&hash[..2]).join(&hash).exists(),
            "…and must not have a published directory"
        );
        // The bookmark, and the work it refers to.
        let claims = store
            .stale_cache_claims(NODE, i64::MAX)
            .await
            .expect("claims");
        assert_eq!(claims.len(), 1, "the claim that lets a later pass resume");
        assert!(
            staging.join(crate::produce::part_dir(0)).exists(),
            "the encoded part was thrown away, so the next pass starts from zero"
        );

        // And picking it up finishes the job from where it stopped rather than
        // from the beginning.
        //
        // Flat out, deliberately: the pacing above exists to bound what the
        // *budgeted* pass could reach, and this pass has no budget to bound.
        // Pacing it too would add SECONDS/READRATE seconds to every run of
        // this test in exchange for nothing.
        let mgr = mgr.with_producer_tuning(ProducerTuning::default());
        let made = mgr
            .produce(&file, 240, Instant::now() + Duration::from_secs(180))
            .await
            .expect("produce")
            .expect("the second pass finishes it");
        assert_eq!(made.recipe, hash);
        assert!(
            made.parts >= 2,
            "the second pass restarted from zero instead of resuming"
        );
        assert!(
            (made.duration_ms - (SECONDS as i64) * 1_000).abs() <= 1_000,
            "resuming across passes lost picture: {}ms of a {SECONDS}s source",
            made.duration_ms
        );
        assert!(
            store.cache_hit(&hash, NODE).await.expect("hit").is_some(),
            "and it is serveable now"
        );
        assert!(
            !staging.exists(),
            "the staging directory outlived the asset it built"
        );
    }

    /// Two producers, one recipe. The loser has to be told, because the
    /// alternative is not a wasted encode but a corrupted one: it would
    /// publish over the directory the winner is still writing into.
    #[tokio::test]
    async fn a_second_producer_stands_down_rather_than_racing() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, _cache) = cached_manager(&store);
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let hash = recipe_hash_for(&mgr, &file, 1080).await;

        // Somebody else is mid-encode.
        store
            .claim_cache_entry(&hash, file_id, 1, NODE, &format!("{}/{hash}", &hash[..2]))
            .await
            .expect("claim");

        assert!(
            mgr.produce(&file, 1080, Instant::now() + Duration::from_secs(30))
                .await
                .expect("produce")
                .is_none(),
            "the second producer started an encode against a claimed recipe"
        );
        // …and did not disturb the claim it lost to.
        let claims = store
            .stale_cache_claims(NODE, i64::MAX)
            .await
            .expect("claims");
        assert_eq!(
            claims.len(),
            1,
            "the winner's claim was removed by the loser"
        );
        assert_eq!(claims[0].relative_dir, format!("{}/{hash}", &hash[..2]));
    }

    #[tokio::test]
    async fn speculative_production_stands_down_while_offline_is_waiting() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let (mgr, _work, _cache) = cached_manager(&store);
        let file = store.get_file(file_id).await.expect("get").expect("file");

        mgr.offline_waiting
            .store(true, std::sync::atomic::Ordering::Release);
        let result = mgr
            .produce(&file, 720, Instant::now() + Duration::from_secs(30))
            .await
            .expect("producer result");
        mgr.offline_waiting
            .store(false, std::sync::atomic::Ordering::Release);

        assert!(
            result.is_none(),
            "offline preparation must own the producer lane"
        );
        assert!(
            store
                .stale_cache_claims(NODE, i64::MAX)
                .await
                .expect("claims")
                .is_empty(),
            "standing down must happen before a recipe is claimed"
        );
    }

    #[tokio::test]
    async fn offline_preempts_speculation_and_resumes_its_published_part() {
        super::require_ffmpeg();
        use plurx_core::domain::{NewOfflinePackage, OfflineCreateOutcome};
        use plurx_core::store::SqliteStore;

        const SECONDS: u32 = 30;
        const READRATE: f64 = 10.0;
        if !crate::ffmpeg::pacing_caps().await.readrate {
            eprintln!(
                "SKIP: offline_preempts_speculation_and_resumes_its_published_part: \
                 `{}` has no -readrate (needs ffmpeg 5.1+)",
                crate::ffmpeg::ffmpeg_bin()
            );
            return;
        }

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let user = store.create_user("paul", "hash", true).await.expect("user");
        let media = crate::test_tempdir().expect("media");
        let source = media.path().join("Heat.mkv");
        write_real_video(&source, SECONDS);
        let file_id = seed_real_file(&store, &source).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let package_id = "offline-preemption";
        let requested = NewOfflinePackage {
            id: package_id.to_owned(),
            request_id: "offline-preemption-request".to_owned(),
            user_id: user.id,
            file_id,
            node_id: NODE.to_owned(),
            source_path: file.path.to_string_lossy().into_owned(),
            source_size: file.size,
            source_mtime: file.mtime,
            effective_rate_control: "vbr".to_owned(),
            target_height: 240,
            output_width: Some(320),
            output_height: Some(240),
            audio_index: None,
            audio_offset_ms: 0,
            subtitle_index: None,
            subtitle_language: None,
            subtitle_mode: "none".to_owned(),
            estimated_bytes: 1_000_000,
            reserved_bytes: 1_100_000,
            expires_at: i64::MAX,
        };
        assert!(matches!(
            store
                .create_offline_package(&requested, 10, 10_000_000, 20_000_000)
                .await
                .expect("create package"),
            OfflineCreateOutcome::Created(_)
        ));
        let claimed = store
            .claim_next_offline_package(NODE)
            .await
            .expect("claim package")
            .expect("queued package");
        assert_eq!(claimed.id, package_id);

        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = Arc::new(mgr.with_producer_tuning(ProducerTuning {
            pacing: Pacing {
                readrate: Some(READRATE),
                initial_burst: None,
                legacy_re: false,
            },
            retry: Duration::from_millis(250),
        }));
        let speculative = {
            let mgr = Arc::clone(&mgr);
            let file = file.clone();
            tokio::spawn(async move {
                mgr.produce(&file, 240, Instant::now() + Duration::from_secs(180))
                    .await
            })
        };
        wait_for_part_segment(cache.path(), 0).await;

        let outcome = mgr
            .ensure_offline(
                &claimed,
                &file,
                &OfflineSpec {
                    target_height: 240,
                    audio_index: None,
                    subtitle: OfflineSubtitle::None,
                    effective_rate_control: EffectiveRateControl::Vbr,
                },
                Instant::now() + Duration::from_secs(180),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("offline preparation");
        assert!(
            speculative
                .await
                .expect("join")
                .expect("producer")
                .is_none(),
            "the speculative pass must yield rather than publish"
        );
        let made = match outcome {
            OfflineProduceOutcome::Ready(made) | OfflineProduceOutcome::Cached(made) => made,
            other => panic!("offline preparation did not finish: {other:?}"),
        };
        assert!(
            made.parts >= 2,
            "offline preparation restarted instead of resuming the preempted part"
        );
    }

    /// Exercise the production handoff rather than holding a waiter manually:
    /// a real foreground `start` must make a running software producer
    /// terminate before it spawns, keep that producer from making another
    /// part while the session owns its permit, then let it resume afterwards.
    #[tokio::test]
    async fn a_real_live_start_preempts_and_parks_a_software_producer() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;

        if !crate::ffmpeg::pacing_caps().await.readrate {
            eprintln!(
                "SKIP: a_real_live_start_preempts_and_parks_a_software_producer: \
                 `{}` has no -readrate (needs ffmpeg 5.1+)",
                crate::ffmpeg::ffmpeg_bin()
            );
            return;
        }

        const SECONDS: u32 = 8;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        store
            .put_setting(keys::SW_POOL_THREADS, "8")
            .await
            .expect("software budget");
        let media = crate::test_tempdir().expect("media");
        let source = media.path().join("Handoff.mkv");
        write_real_video(&source, SECONDS);
        let file_id = seed_real_file(&store, &source).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = Arc::new(mgr.with_producer_tuning(ProducerTuning {
            pacing: Pacing {
                readrate: Some(1.0),
                initial_burst: None,
                legacy_re: false,
            },
            retry: Duration::from_millis(25),
        }));

        let producer = {
            let mgr = Arc::clone(&mgr);
            tokio::spawn(async move {
                mgr.produce(&file, 240, Instant::now() + Duration::from_secs(60))
                    .await
            })
        };
        wait_for_part_segment(cache.path(), 0).await;
        assert_eq!(
            mgr.admissions.software_in_use(),
            2,
            "the running producer owns its background permit"
        );

        let live = mgr
            .start(file_id, 240, 0.0, None, None, "paul", "pb-live-handoff")
            .await
            .expect("foreground start after bounded preemption");
        assert_eq!(
            mgr.admissions.software_in_use(),
            2,
            "the producer released before the live encoder acquired; only the viewer remains"
        );

        assert!(
            tokio::time::timeout(PRODUCER_POLL * 2, wait_for_part_segment(cache.path(), 1))
                .await
                .is_err(),
            "the producer reacquired capacity while a live permit was held"
        );

        assert!(mgr.stop_session(&live.session_id, "test").await);
        wait_for_part_segment(cache.path(), 1).await;
        let made = producer
            .await
            .expect("producer join")
            .expect("producer result")
            .expect("producer resumed and published");
        assert!(
            made.parts >= 2,
            "preemption must leave a resumed second part"
        );
        assert_eq!(mgr.admissions.software_in_use(), 0, "all permits returned");
    }

    /// Preempted mid-encode, then resumed — and the film that comes out has no
    /// hole in it.
    ///
    /// This is the producer's whole reason for existing in parts, and the
    /// failure it guards against is the quietest one in the system: resuming a
    /// few hundred milliseconds late loses a moment of picture in the middle of
    /// a film, in a file nobody watches until next week, with every log line
    /// green. Only measuring the assembled timeline against the source catches
    /// it, so that is what this does.
    ///
    /// Everything about the timing here is derived from the producer's own
    /// output or from a rate this test sets — see the comment on the pacing
    /// below. A test of preemption that sleeps a fixed interval is really a
    /// test of how fast the machine is.
    #[tokio::test]
    async fn a_preempted_producer_resumes_without_losing_picture() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        // The producer's input is paced for this test, which is what makes it
        // a test rather than a race.
        //
        // Preemption is only observable while the encoder is still running, so
        // the previous shape — a 240-second fixture and a 400 ms sleep — was
        // an unwritten assertion that this machine needs more than 400 ms to
        // transcode 240 seconds of 160x120. A 2-core CI box needs about five
        // seconds and passed; a 16-core desktop needs about a third of one,
        // finished before the first interrupt, and failed with "nothing was
        // actually preempted". No fixture length satisfies both: whatever is
        // long enough for the fast box is minutes of CI time on the slow one.
        //
        // `-readrate N` removes the CPU from the equation. The encoder reads
        // its input at N times realtime, so a part's wall-clock duration is
        // SECONDS / READRATE on any box quick enough to keep up — and the
        // margin for "quick enough" is what the numbers below are chosen for:
        // the slowest machine seen here manages about 50x realtime on this
        // fixture, five times the rate asked for. A box slower than READRATE
        // just takes longer and still gets preempted, so the failure mode is
        // a slow test rather than a false one.
        const SECONDS: u32 = 30;
        const READRATE: f64 = 10.0;
        // Long enough that a running encoder cannot miss it (PRODUCER_POLL is
        // a quarter-second), short enough not to dominate the test.
        const HOLD: Duration = Duration::from_millis(750);

        // `-readrate` landed in ffmpeg 5.1 and is a hard exit on anything
        // older, so an ancient build gets a skip rather than a failure — the
        // same bargain `pipeprobe` strikes with a missing zscale. CI has a
        // modern ffmpeg, so the coverage is not lost.
        if !crate::ffmpeg::pacing_caps().await.readrate {
            eprintln!(
                "SKIP: a_preempted_producer_resumes_without_losing_picture: \
                 `{}` has no -readrate (needs ffmpeg 5.1+), so a producer part's \
                 duration cannot be made independent of this machine's speed",
                crate::ffmpeg::ffmpeg_bin()
            );
            return;
        }

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source = media.path().join("Heat.mkv");
        write_real_video(&source, SECONDS);
        let file_id = seed_real_file(&store, &source).await;
        let (mgr, _work, cache) = cached_manager(&store);
        let mgr = mgr.with_producer_tuning(ProducerTuning {
            pacing: Pacing {
                readrate: Some(READRATE),
                // No burst: a burst is exactly the "read the first N seconds
                // flat out" behaviour this test needs not to have.
                initial_burst: None,
                legacy_re: false,
            },
            // Production waits five seconds before asking for the hardware
            // again, and that is the right number for a box where a viewer
            // just took the slot. Here it is dead time the test pays once per
            // preemption, and the retry is not what is under test.
            retry: Duration::from_millis(250),
        });
        let mgr = Arc::new(mgr);
        let file = store.get_file(file_id).await.expect("get").expect("file");

        let run = {
            let mgr = Arc::clone(&mgr);
            let file = file.clone();
            tokio::spawn(async move {
                mgr.produce(&file, 240, Instant::now() + Duration::from_secs(180))
                    .await
            })
        };
        // Interrupt it twice, so the asset is assembled from three parts and a
        // per-join error compounds rather than cancels.
        //
        // Each interrupt waits for the part it is about to kill to have
        // published a segment, and then for the *next* part to publish one
        // before interrupting again. Both waits are observations of the
        // producer's own output, not sleeps: they are what stops an interrupt
        // landing before the encoder started (killing a part that produced
        // nothing, which the producer renumbers away) or during the retry
        // backoff (where it is simply lost — the reason the old loop's second
        // interrupt never landed, so this test only ever built two parts even
        // where it passed).
        for part in 0..2usize {
            wait_for_part_segment(cache.path(), part).await;
            let queued = mgr.admissions.wait_for_slot();
            tokio::time::sleep(HOLD).await;
            drop(queued);
            wait_for_part_segment(cache.path(), part + 1).await;
        }
        let made = run
            .await
            .expect("join")
            .expect("produce")
            .expect("something was produced");
        // The assertion that stops this test passing for the wrong reason: an
        // encode that finished before it was interrupted would satisfy every
        // check below while exercising none of the resume path.
        assert!(
            made.parts >= 3,
            "assembled from {} part(s) — two interrupts must leave three, and \
             fewer means the encode was not actually preempted, so this test \
             proved nothing about resuming",
            made.parts
        );
        let hash = made.recipe;

        let dir = cache.path().join(&hash[..2]).join(&hash);
        let playlist = tokio::fs::read_to_string(dir.join("index.m3u8"))
            .await
            .expect("playlist");
        let part = crate::produce::Part::from_playlist(&playlist);

        // Continuous numbering, every segment on disk and non-empty.
        for (i, name) in part.segments.iter().enumerate() {
            assert_eq!(*name, format!("seg{i:05}.ts"), "a gap at {i}: {playlist}");
            let meta = tokio::fs::metadata(dir.join(name)).await.expect(name);
            assert!(meta.len() > 0, "{name} is empty");
        }

        // And the timeline covers the source. A resume that restarted a beat
        // late would land short here, by exactly the picture it dropped.
        let produced_ms = part.duration_ms();
        let source_ms = (SECONDS as i64) * 1000;
        // Half a segment. The resume works in whole published segments, so a
        // mistake here shows up as a segment lost or repeated — two seconds —
        // and a tolerance loose enough to swallow that would hide the only
        // thing this test exists to find. Measured drift on this fixture is
        // zero.
        assert!(
            (produced_ms - source_ms).abs() <= 1_000,
            "assembled {produced_ms}ms from a {source_ms}ms source — \
             a resume lost or repeated picture:\n{playlist}"
        );
        // Belt and braces: ffprobe the assembled asset, because a playlist can
        // claim a duration its bytes do not have.
        let probed = std::process::Command::new(
            std::env::var("PLURX_FFPROBE").unwrap_or_else(|_| "ffprobe".into()),
        )
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
            "-allowed_extensions",
            "ALL",
        ])
        .arg(dir.join("index.m3u8"))
        .output()
        .expect("ffprobe");
        let probed_s: f64 = String::from_utf8_lossy(&probed.stdout)
            .trim()
            .parse()
            .unwrap_or(0.0);
        assert!(
            (probed_s - SECONDS as f64).abs() <= 2.0,
            "ffprobe reads {probed_s}s from a {SECONDS}s source"
        );

        // It serves, which is the only thing any of this was for.
        let info = mgr
            .start(file_id, 240, 0.0, None, None, "paul", "pb-resumed")
            .await
            .expect("start");
        assert!(info.vod, "a resumed asset is not findable");
        assert!(mgr.stop_session(&info.session_id, "test").await);
    }

    /// Wait until part `index` of the one staged pre-transcode has published a
    /// segment.
    ///
    /// The producer publishes each part's segments as it makes them, so this
    /// is the encoder saying "I am running and I got somewhere" in the only
    /// vocabulary it has. Polling for it replaces a sleep, and a sleep here is
    /// always a guess about CPU speed dressed up as a constant.
    async fn wait_for_part_segment(cache: &std::path::Path, index: usize) {
        let staging = cache.join(crate::cachekeep::STAGING);
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            // One recipe is in flight, but find it rather than recomputing the
            // hash: a hash spelled out twice is a hash that can drift.
            if let Ok(mut entries) = tokio::fs::read_dir(&staging).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let dir = entry.path().join(crate::produce::part_dir(index));
                    if let Ok(dir) = plurx_core::fs_secure::SecureDirectory::open(&dir).await {
                        if !read_part(&dir).await.part.is_empty() {
                            return;
                        }
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "part {index} never published a segment under {}",
                staging.display()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A file whose real details are on disk, so ffmpeg can actually read it.
    async fn seed_real_file(store: &Arc<dyn Store>, path: &std::path::Path) -> i64 {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
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
        let meta = std::fs::metadata(path).expect("fixture");
        store
            .upsert_file(
                movie,
                &path.to_string_lossy(),
                meta.len() as i64,
                1,
                &ProbeResult {
                    duration_ms: Some(6_000),
                    container: Some("mkv".into()),
                    video_codec: Some("h264".into()),
                    width: Some(160),
                    height: Some(120),
                    ..Default::default()
                },
            )
            .await
            .expect("file")
    }

    /// A node with no cache root is not a broken node — it is the ordinary
    /// case, and every path has to read as a plain miss.
    #[tokio::test]
    async fn a_manager_without_a_cache_root_simply_always_misses() {
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
        assert!(mgr.digest().is_none());
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let encoder = mgr.encoder().await;
        let opts = mgr.options_for_tone_map(
            encoder,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            tone_map_pref(),
            OutputGrade::Sdr,
        );
        let plan = mgr
            .resolve_movie_plan(&file, &opts, encoder)
            .await
            .expect("resolve cacheless plan");
        assert!(mgr
            .serve_cached(
                &file,
                &opts,
                &plan,
                "Heat",
                SessionOwner {
                    user_name: "paul",
                    supersession_user: r#"["username","paul"]"#,
                    playback_id: "pb-1",
                    automatic: true,
                },
            )
            .await
            .is_none());
    }

    /// The speculative dedupe key is durable: it is stored on every queue
    /// row (`domain.rs` `policy_generation`), compared before production,
    /// and a mismatch is turned into a hard `cancel_job(.., "policy_changed")`
    /// rather than a yield. An unset pair and an explicit `bitrate` resolve
    /// to the same effective policy on every family, so they must hash to the
    /// same generation. If the tri-state spelled "unset" differently, every
    /// queued speculative row would be cancelled and rediscovered at deploy,
    /// and the generation would flip again mid-boot on every restart — the
    /// manager's first snapshot is `RateControlSnapshot::bitrate`, and
    /// `initialize_rate_control` then republishes the absent pair as `None`.
    #[test]
    fn an_unset_rate_control_pair_keeps_the_explicit_bitrate_policy_generation() {
        let prefs = plurx_core::tracks::LangPrefs::default();
        let unset = RateControlSnapshot {
            requested_mode: None,
            requested_quality: None,
            quality_rc: QualityRc::default(),
        };
        let explicit = RateControlSnapshot {
            requested_mode: Some(RateMode::Bitrate),
            ..unset
        };
        let quality = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            ..unset
        };

        let generation =
            TranscodeManager::pretranscode_policy_generation_for(unset, "auto", &prefs);
        assert_eq!(
            generation,
            TranscodeManager::pretranscode_policy_generation_for(explicit, "auto", &prefs),
            "an unset pair and an explicit bitrate request are the same policy and must \
             not enqueue replacement work against each other"
        );
        assert_ne!(
            generation,
            TranscodeManager::pretranscode_policy_generation_for(quality, "auto", &prefs),
            "an explicit quality request is a different policy"
        );

        // The durable value itself, unchanged since before `requested_mode`
        // became an `Option`. A PR that flips a family default moves this
        // deliberately, with its artefact; nothing else may move it.
        assert_eq!(
            generation,
            "speculative-auto-v2:\
             08494ca183d08fdddf67791b5c47a324dbf4dd32544694dc2fd12589a6c67723",
            "the speculative dedupe key for an unset pair is durable state"
        );
    }

    #[tokio::test]
    async fn speculative_policy_generation_tracks_replicated_track_preferences() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let mgr = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );

        let base = mgr.pretranscode_policy_generation().await;
        store
            .put_setting(keys::AUDIO_LANG, "jpn")
            .await
            .expect("audio");
        let audio = mgr.pretranscode_policy_generation().await;
        assert_ne!(base, audio);
        store
            .put_setting(keys::AUDIO_LANG, "ja")
            .await
            .expect("alias");
        assert_eq!(
            audio,
            mgr.pretranscode_policy_generation().await,
            "equivalent language aliases should not enqueue replacement work"
        );
        store
            .put_setting(keys::SUB_LANG, "spa")
            .await
            .expect("subs");
        let subtitles = mgr.pretranscode_policy_generation().await;
        assert_ne!(audio, subtitles);
        store
            .put_setting(keys::SUB_MODE, "always")
            .await
            .expect("subtitle mode");
        let subtitle_mode = mgr.pretranscode_policy_generation().await;
        assert_ne!(subtitles, subtitle_mode);
        store
            .put_setting(keys::HWACCEL, "qsv")
            .await
            .expect("encoder");
        assert_ne!(subtitle_mode, mgr.pretranscode_policy_generation().await);
    }

    #[tokio::test]
    async fn manager_reads_prefs_and_runs_session_lifecycle() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));

        // Defaults with an empty store.
        let prefs = mgr.lang_prefs().await;
        assert_eq!(prefs.audio_lang, "eng");
        // No hardware caps → software encoder.
        assert_eq!(mgr.encoder().await, Encoder::Software);
        assert_eq!(mgr.active_sessions().await, 0);
        assert!(mgr.list_deliveries().await.is_empty());

        // Settings feed the language prefs.
        store.put_setting(keys::AUDIO_LANG, "jpn").await.expect("s");
        store.put_setting(keys::SUB_LANG, "eng").await.expect("s");
        store
            .put_setting(keys::SUB_MODE, "always")
            .await
            .expect("s");
        let prefs = mgr.lang_prefs().await;
        assert_eq!(prefs.audio_lang, "jpn");

        // Unknown session lookups fail fast (no waiting).
        assert_eq!(
            mgr.playlist("missing").await,
            Err(PlaylistError::SessionGone),
            "an id with no session is gone, not a stream that failed to build"
        );
        assert!(mgr
            .segment("missing", "seg00000.ts")
            .await
            .expect("segment admission")
            .is_none());
        assert!(mgr
            .segment("missing", "../evil")
            .await
            .expect("segment admission")
            .is_none());
        assert!(!mgr.stop_session("missing", "test").await);

        // A real start spawns ffmpeg (it fails async on the fake path, but the
        // session is created and tracked). Then the admin stop kills it.
        let info = mgr
            .start(file_id, 720, 0.0, None, None, "paul", "pb-paul")
            .await
            .expect("start");
        assert_eq!(info.encoder, "software (x264)");
        assert_eq!(mgr.active_sessions().await, 1);
        assert_eq!(
            mgr.sessions.lock().await[info.session_id.as_str()]
                .delivery
                .method(),
            "transcode",
            "a live transcode's bytes are transcode bytes"
        );
        let sessions = mgr.list_deliveries().await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].0.user_name, "paul");
        assert_eq!(sessions[0].1, crate::delivery::Method::Transcode);
        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert_eq!(mgr.active_sessions().await, 0);

        // The copy-video path likewise creates and tears down a session.
        // This fixture tests lifecycle after admission; unverified HEVC copy
        // requires the explicit Developer override.
        store
            .put_setting(keys::HEVC_UNVERIFIED_COPY, "1")
            .await
            .expect("allow unverified copy for lifecycle coverage");
        let info = mgr
            .start_copy(
                file_id,
                5.0,
                Some(1),
                CopySessionOptions {
                    convert_dolby_vision: false,
                    transcode_audio: true,
                    preserve_dolby_vision: false,
                },
                "paul",
                "pb-paul",
            )
            .await
            .expect("start_copy");
        assert_eq!(info.encoder, "copy");
        assert_eq!(
            mgr.sessions.lock().await[info.session_id.as_str()]
                .delivery
                .method(),
            "remux",
            "a live copy-video session's bytes are remux bytes"
        );
        // The two HLS kinds share a struct and are told apart structurally,
        // never by the encoder label: that label goes to "cached" on a cache
        // hit and is rewritten by the hardware→software fallback, either of
        // which would have a copy-remux reporting itself as a transcode.
        let copies = mgr.list_deliveries().await;
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].1, crate::delivery::Method::HlsCopy);
        assert!(mgr.stop_session(&info.session_id, "test").await);
        assert!(
            mgr.list_deliveries().await.is_empty(),
            "and the row goes with the session"
        );
    }

    /// Seeking must not leave the old session running. Before this, every seek
    /// stacked another ffmpeg for up to ~75s (idle timeout + reaper tick).
    #[tokio::test]
    async fn a_new_session_supersedes_the_same_players_old_one() {
        super::require_ffmpeg();
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let work = crate::test_tempdir().expect("work");
        let mgr = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        // Two players deliberately coexist below; this test is about the
        // supersession key, not the CPU pool, so give the pool room — on a
        // 2-core runner the machine-derived budget would refuse the second
        // player and fail the test for the wrong reason.
        store
            .put_setting(keys::SW_POOL_THREADS, "64")
            .await
            .expect("pool headroom");
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let live_weight = Workload::of(&file, 720).software_threads();

        // Play, then "seek" three times. Only the newest survives.
        let first = mgr
            .start(file_id, 720, 0.0, None, None, "paul", "pb-paul")
            .await
            .expect("start");
        assert_eq!(mgr.admissions.software_in_use(), live_weight);
        let second = mgr
            .start(file_id, 720, 600.0, None, None, "paul", "pb-paul")
            .await
            .expect("seek");
        assert_eq!(
            mgr.admissions.software_in_use(),
            live_weight,
            "supersession returns the predecessor before admitting its replacement"
        );
        let third = mgr
            .start(file_id, 720, 1200.0, None, None, "paul", "pb-paul")
            .await
            .expect("seek again");
        assert_eq!(mgr.admissions.software_in_use(), live_weight);
        assert_eq!(mgr.active_sessions().await, 1);
        assert_eq!(
            mgr.playlist(&first.session_id).await,
            Err(PlaylistError::SessionGone),
            "a superseded session is gone — the viewer's own seek replaced it"
        );
        assert!(!mgr.stop_session(&second.session_id, "test").await);
        assert!(mgr.stop_session(&third.session_id, "test").await);

        // The copy path supersedes too, and across paths: a transcode fallback
        // after a copy attempt must not leave the copy remux reading the disk.
        store
            .put_setting(keys::HEVC_UNVERIFIED_COPY, "1")
            .await
            .expect("allow unverified copy for supersession coverage");
        let copy = mgr
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
            .expect("copy");
        let fallback = mgr
            .start(file_id, 720, 0.0, None, None, "paul", "pb-paul")
            .await
            .expect("fallback");
        assert_eq!(mgr.active_sessions().await, 1);
        assert!(!mgr.stop_session(&copy.session_id, "test").await);

        // Another player is untouched — and this is the part that changed.
        // Supersession used to be keyed by (viewer, file), so one account
        // watching the same film in two places meant each device killed the
        // other's stream on every seek. Two player instances now coexist even
        // as the same person, on the same file, from the same account.
        let laptop = mgr
            .start(file_id, 720, 0.0, None, None, "paul", "pb-laptop")
            .await
            .expect("second device");
        assert_eq!(mgr.active_sessions().await, 2);
        assert_eq!(
            mgr.admissions.software_in_use(),
            live_weight * 2,
            "multiple viewers retain one permit each"
        );
        let reseek = mgr
            .start(file_id, 720, 30.0, None, None, "paul", "pb-paul")
            .await
            .expect("the first player seeks");
        assert_eq!(
            mgr.active_sessions().await,
            2,
            "one player's seek must not touch another player's stream"
        );
        assert_eq!(
            mgr.admissions.software_in_use(),
            live_weight * 2,
            "replacing one viewer preserves the aggregate live cap"
        );
        assert!(!mgr.stop_session(&fallback.session_id, "test").await);
        assert!(mgr.stop_session(&laptop.session_id, "test").await);
        assert!(mgr.stop_session(&reseek.session_id, "test").await);
        assert_eq!(mgr.active_sessions().await, 0);
        assert_eq!(
            mgr.admissions.software_in_use(),
            0,
            "every superseded and stopped live permit returned"
        );
    }
