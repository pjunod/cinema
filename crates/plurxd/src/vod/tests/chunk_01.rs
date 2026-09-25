    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::AcqRel};

    use plurx_core::store::{
        ClusterFragmentIndexStore as _, FragmentIndexStore, LibraryStore as _,
        MediaSessionStore as _, MediaStore as _, PlaybackTelemetryStore as _,
        RenditionPlanStore as _, SqliteStore, TimelineAnnotationStore as _,
    };
    use plurx_core::testfixtures;

    use crate::fragindex::IndexOutcome;

    struct CleanupObservingCommitter {
        rendition: Arc<Rendition>,
        session_id: String,
        started: AtomicBool,
        reader_detached: AtomicBool,
        attempts: Arc<AtomicUsize>,
        expires_at_unix_ms: i64,
    }

    #[derive(Default)]
    struct RecordingPreparationAdmission {
        outcome: std::sync::Mutex<Option<crate::playback_control::PreparationControlOutcome>>,
    }

    impl crate::playback_control::PreparationSettlementAdmission for RecordingPreparationAdmission {
        fn accepted(&self, outcome: crate::playback_control::PreparationControlOutcome) {
            *self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        }
    }

    impl crate::playback_control::TerminalControlCommitter for CleanupObservingCommitter {
        fn start(
            &self,
            result: &crate::playback_control::LocalControlResult,
        ) -> crate::playback_control::TerminalCommitReceipt {
            self.started.store(true, Release);
            let detached = self
                .rendition
                .readers
                .try_lock()
                .ok()
                .is_some_and(|readers| !readers.contains_key(&self.session_id));
            self.reader_detached.store(detached, Release);
            let response = crate::playback_control::terminal_response_for_test(result);
            let attempts = Arc::clone(&self.attempts);
            crate::playback_control::TerminalCommitReceipt::retryable_until(
                self.expires_at_unix_ms,
                move |attempt| {
                    let index = attempts.fetch_add(1, AcqRel);
                    attempt.complete(if index == 0 {
                        Err(())
                    } else {
                        Ok(response.clone())
                    });
                },
            )
        }
    }

    #[tokio::test]
    async fn maintenance_converges_beyond_one_bounded_encoded_generation_batch() {
        let base = crate::test_tempdir().expect("encoded generation cache");
        let copy = base.path().join("copy-rendition");
        let current = base.path().join("current-encoded");
        for path in [&copy, &current] {
            std::fs::create_dir_all(path).expect("rendition directory");
        }
        let current_process = crate::ffmpeg::encoded_process_identity();
        std::fs::write(
            current.join(ENCODED_PROCESS_NAME),
            format!("{current_process}\n"),
        )
        .expect("current marker");
        let stale_keys = (0..=ENCODED_RECONCILE_BATCH)
            .map(|index| format!("stale-encoded-{index:03}"))
            .collect::<Vec<_>>();
        for key in &stale_keys {
            let path = base.path().join(key);
            std::fs::create_dir_all(&path).expect("stale rendition directory");
            std::fs::write(path.join(ENCODED_PROCESS_NAME), "process-old\n").expect("stale marker");
        }

        let sqlite = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let index = synthetic_index(16);
        let duration_ms = index_video_ms(&index);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &shipped_policy(index.timescale),
            &TrackDurations {
                video_ms: duration_ms,
                audio_ms: duration_ms,
                audio_bits_per_second: 256_000,
            },
        );
        let identity = SourceIdentity::new(1, 1, "generation-plan");
        for key in stale_keys
            .iter()
            .map(String::as_str)
            .chain(["copy-rendition", "current-encoded"])
        {
            sqlite
                .put_rendition_plan(key, 7, &plan, &identity)
                .await
                .expect("store rendition plan");
        }
        let store: Arc<dyn Store> = sqlite.clone();
        let serve = VodServe::new(base.path().to_path_buf(), store);

        serve.maintain().await;
        assert_eq!(
            stale_keys
                .iter()
                .filter(|key| base.path().join(key).exists())
                .count(),
            1,
            "one later batch remains after the bounded first pass"
        );
        serve.maintain().await;
        assert!(stale_keys.iter().all(|key| !base.path().join(key).exists()));
        assert!(copy.exists(), "an unmarked copy rendition is preserved");
        assert!(
            current.exists(),
            "the current encoded generation is preserved"
        );
        for key in &stale_keys {
            assert!(sqlite
                .rendition_plan(key, &identity)
                .await
                .expect("read stale plan")
                .is_none());
        }
        for key in ["copy-rendition", "current-encoded"] {
            assert!(sqlite
                .rendition_plan(key, &identity)
                .await
                .expect("read preserved plan")
                .is_some());
        }
    }

    /// The conversion reaches the pipeline, and therefore the identity.
    ///
    /// This is the join between "the plan review said convert" and everything
    /// downstream: the generation, the playlist facts and the index pass all
    /// read these options rather than carrying their own copy of the answer,
    /// so the flag arriving here is what makes them agree — and the argv
    /// fingerprint is what stops a converting session ever landing on the
    /// unconverted stream's index.
    #[test]
    fn a_converting_session_gets_a_converting_pipeline_and_its_own_identity() {
        let mut file = fixture_file();
        file.hdr = Some("dolby_vision".into());
        file.dolby_vision.profile = Some(7);
        file.dolby_vision.level = Some(6);
        file.dolby_vision.bl_compat_id = Some(1);

        let converting = copy_video_pipeline(&file, None, true, true, true);
        assert!(converting.converts_dolby_vision());
        assert!(
            converting.preserves_dolby_vision(),
            "there is nothing to convert in a stream the filter removed"
        );

        let preserving = copy_video_pipeline(&file, None, true, true, false);
        assert!(!preserving.converts_dolby_vision());
        assert!(preserving.preserves_dolby_vision());

        let stripping = copy_video_pipeline(&file, None, true, false, false);
        assert!(!stripping.preserves_dolby_vision());

        // Three pipelines, three identities. Two of them sharing one would
        // hand a session a playlist whose cut points describe media it never
        // produces — the failure the whole third-identity design exists to
        // prevent.
        let fingerprints: std::collections::HashSet<_> = [converting, preserving, stripping]
            .into_iter()
            .map(|video| crate::fragindex::identity_for(&file, video).argv_fingerprint)
            .collect();
        assert_eq!(fingerprints.len(), 3, "{fingerprints:?}");
    }

    /// A shared artifact for the selected converting recipe is sufficient to
    /// open copy VOD even when this node has no legacy file-keyed index.  This
    /// reaches the same hydrate path used for a blob fetched from a peer; the
    /// local cache is preinstalled only to keep transport/authentication out
    /// of a recipe-identity regression.
    #[tokio::test]
    async fn requeue_through_the_no_holder_arm_after_converting_artifact_hydrates() {
        testfixtures::require_ffmpeg();
        let source_dir = crate::test_tempdir().expect("source dir");
        let source_path = source_dir.path().join("profile-seven.mkv");
        std::fs::write(&source_path, b"attested source bytes").expect("source");
        let metadata = std::fs::metadata(&source_path).expect("source metadata");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix mtime")
            .as_secs() as i64;

        let sqlite = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = sqlite
            .create_library(&plurx_core::domain::NewLibrary {
                name: "Shared index".to_owned(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: Vec::new(),
                anime: false,
            })
            .await
            .expect("library");
        let item = sqlite
            .insert_item(&plurx_core::domain::NewItem {
                library_id: library.id,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "Profile seven".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = sqlite
            .upsert_file(
                item,
                &source_path.to_string_lossy(),
                metadata.len() as i64,
                mtime,
                &plurx_core::domain::ProbeResult {
                    duration_ms: Some(12_000),
                    container: Some("mkv".to_owned()),
                    video_codec: Some("hevc".to_owned()),
                    video_profile: Some("Main 10".to_owned()),
                    width: Some(3840),
                    height: Some(2160),
                    bit_depth: Some(10),
                    hdr: Some("dolby_vision".to_owned()),
                    hdr_format: Some("Dolby Vision · Profile 7 (HDR10-compatible)".to_owned()),
                    dolby_vision: plurx_core::domain::DolbyVisionFacts {
                        profile: Some(7),
                        level: Some(6),
                        bl_compat_id: Some(1),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let file = sqlite
            .get_file(file_id)
            .await
            .expect("read file")
            .expect("stored file");
        let video = copy_video_pipeline(&file, None, true, true, true);
        assert!(video.converts_dolby_vision());

        let object_version = crate::fragment_index_cluster::inspect_source(&file)
            .await
            .expect("source identity");
        let source_sha256 = "a".repeat(64);
        sqlite
            .record_fragment_index_source(&plurx_core::store::FragmentIndexSourceObservation {
                node_id: "node-a".to_owned(),
                file_id,
                object_version: object_version.clone(),
                source_size: file.size,
                source_mtime: file.mtime,
                source_sha256: source_sha256.clone(),
                observed_at_ms: 1,
            })
            .await
            .expect("source observation");

        let engine = crate::ffmpeg::fragment_index_engine_digest().await;
        let pipeline_sha256 = crate::fragment_index_cluster::pipeline_digest(&file, &engine, video);
        let cache_key = plurx_core::store::cluster_fragment_index_key(
            file.id,
            file.size,
            file.mtime,
            &source_sha256,
            &pipeline_sha256,
        )
        .expect("cache key");
        let now = crate::fragment_index_cluster::unix_ms();
        let queued = plurx_core::store::NewClusterFragmentIndexJob {
            cache_key: cache_key.clone(),
            file_id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256: source_sha256.clone(),
            pipeline_sha256: pipeline_sha256.clone(),
            priority: "foreground".to_owned(),
            trigger: "foreground".to_owned(),
            target_node_id: "node-a".to_owned(),
            not_before_ms: now,
            created_at_ms: now,
        };
        assert!(sqlite
            .enqueue_cluster_fragment_index(&queued)
            .await
            .expect("enqueue"));
        let claimed = sqlite
            .claim_cluster_fragment_index("node-a", &[], now, now + 60_000)
            .await
            .expect("claim")
            .expect("claimed job");

        let record = plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1)
            .expect("converted record");
        let mut index = synthetic_index(3);
        index.promotion.dolby_vision = Some(record.clone());
        let blob = plurx_core::store::encode_cluster_fragment_index_blob(
            &index,
            &source_sha256,
            &pipeline_sha256,
        )
        .expect("encode artifact");
        let artifact = plurx_core::store::ClusterFragmentIndexArtifact {
            cache_key: cache_key.clone(),
            file_id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_sha256,
            pipeline_sha256,
            blob_sha256: plurx_core::store::cluster_fragment_index_blob_sha256(&blob),
            bytes: blob.len() as i64,
            built_by_node_id: "node-a".to_owned(),
            built_at_ms: now + 1,
        };
        let location = plurx_core::store::ClusterFragmentIndexLocation {
            cache_key: cache_key.clone(),
            node_id: "node-a".to_owned(),
            bytes: artifact.bytes,
            verified_at_ms: now + 1,
            last_seen_at_ms: now + 1,
        };
        assert!(sqlite
            .complete_cluster_fragment_index(&claimed, &artifact, &location, now + 1)
            .await
            .expect("publish artifact"));
        let cache = crate::test_tempdir().expect("cluster index cache");
        crate::fragment_index_cluster::install_local_blob(cache.path(), &artifact, &blob)
            .await
            .expect("install cluster blob");
        assert!(sqlite
            .fragment_index(file.id, &crate::fragindex::identity_for(&file, video))
            .await
            .expect("read v1 index")
            .is_none());

        let base = crate::test_tempdir().expect("rendition root");
        let store: Arc<dyn Store> = sqlite.clone();
        let serve = VodServe::new_cluster(
            base.path().to_path_buf(),
            store,
            "node-a".to_owned(),
            cache.path().to_path_buf(),
            None,
        );
        let (hydrated, hydrated_version, hydrated_key) = serve
            .try_cluster_fragment_index(&file, video)
            .await
            .expect("selected converting artifact hydrates")
            .expect("source is already attested");
        assert_eq!(hydrated_key, cache_key);
        assert_eq!(hydrated_version, object_version);
        assert_eq!(hydrated.promotion.dolby_vision, Some(record));
        assert_eq!(hydrated.rows, index.rows);

        crate::fragment_index_cluster::remove_local_blob(cache.path(), &cache_key).await;
        let unavailable = serve
            .try_cluster_fragment_index(&file, video)
            .await
            .expect_err("a catalogue row without any verified holder is not playable");
        assert!(
            unavailable.contains("no verified holder could supply"),
            "{unavailable}"
        );
        let repair = sqlite
            .cluster_fragment_index_job(&cache_key, "node-a")
            .await
            .expect("read repair job")
            .expect("the no-holder arm retains a repair job");
        assert_eq!(repair.state, "queued");
        assert_eq!(repair.priority, "foreground");
        assert_eq!(repair.trigger, "foreground");
    }

    fn fixture_file() -> MediaFile {
        media_file_at(testfixtures::source("clean-cra"), 12_000)
    }

    include!("../../vodencode_tests.rs");

    fn media_file_at(path: PathBuf, duration_ms: i64) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 1,
            item_id: 1,
            path,
            size: 1,
            mtime: 1,
            duration_ms: Some(duration_ms),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main".into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: Some(1_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn request(playback_id: &str, start_seconds: f64) -> SessionRequest {
        SessionRequest {
            control_sequence: None,
            file_id: 1,
            playback_id: playback_id.to_string(),
            request_id: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        }
    }

    fn settings() -> VodSettings {
        VodSettings {
            working_set_bytes: 8 << 30,
            completed_cache_bytes: 50 << 30,
            block_budget: Duration::from_secs(30),
            materialize_budget: Duration::from_secs(30),
            blocked_get_cap: DEFAULT_GLOBAL_WAIT_CAP,
        }
    }

    #[test]
    fn forced_generation_holder_repair_targets_the_resolved_artifact() {
        let logical_key = "a".repeat(64);
        let generation_key = "b".repeat(64);
        let repair = plurx_core::store::NewClusterFragmentIndexJob {
            cache_key: logical_key,
            file_id: 1,
            source_size: 10,
            source_mtime: 20,
            source_sha256: "c".repeat(64),
            pipeline_sha256: "d".repeat(64),
            priority: "foreground".to_owned(),
            trigger: "foreground".to_owned(),
            target_node_id: "node-a".to_owned(),
            not_before_ms: 30,
            created_at_ms: 30,
        };
        let artifact = plurx_core::store::ClusterFragmentIndexArtifact {
            cache_key: generation_key.clone(),
            file_id: 1,
            source_size: 10,
            source_mtime: 20,
            source_sha256: repair.source_sha256.clone(),
            pipeline_sha256: repair.pipeline_sha256.clone(),
            blob_sha256: "e".repeat(64),
            bytes: 40,
            built_by_node_id: "node-a".to_owned(),
            built_at_ms: 30,
        };

        let resolved = repair_job_for_artifact(repair, &artifact);

        assert_eq!(resolved.cache_key, generation_key);
        assert_eq!(resolved.source_sha256, artifact.source_sha256);
        assert_eq!(resolved.pipeline_sha256, artifact.pipeline_sha256);
    }

    /// A store holding the fixture's real fragment index, built by the real
    /// index pipe — the whole attach sequence starts from what a scan would
    /// have persisted.
    async fn store_with_index(file: &MediaFile) -> (Arc<dyn Store>, FragmentIndex) {
        testfixtures::require_ffmpeg();
        let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
        let runtime = crate::test_tempdir().expect("runtime cache dir");
        let outcome = crate::fragindex::build(
            file,
            CopyVideoOptions::new(have_dovi, false),
            runtime.path(),
            Duration::from_secs(120),
        )
        .await;
        let IndexOutcome::Built(index) = outcome else {
            panic!("the fixture must index: {outcome:?}");
        };
        let store = SqliteStore::open_in_memory().expect("store");
        store
            .put_fragment_index(file.id, &index)
            .await
            .expect("store the index");
        (Arc::new(store) as Arc<dyn Store>, *index)
    }

    async fn serve_on(base: &Path) -> (Arc<VodServe>, MediaFile) {
        serve_on_file(base, fixture_file()).await
    }

    async fn serve_on_file(base: &Path, file: MediaFile) -> (Arc<VodServe>, MediaFile) {
        let (store, _) = store_with_index(&file).await;
        (VodServe::new(base.to_path_buf(), store), file)
    }

    /// A `VodServe` with an empty store, for tests that drive internals
    /// directly against a hand-built rendition.
    fn bare_serve(base: &Path) -> Arc<VodServe> {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        VodServe::new(base.to_path_buf(), store)
    }

    async fn activate_control_route(store: &SqliteStore, session_id: &str, generation: &str) {
        let now_ms = crate::media_sessions::unix_ms();
        let fingerprint = "a".repeat(64);
        let playback_id = format!("vod-control-{session_id}");
        store
            .claim_media_session_request(
                7,
                generation,
                &fingerprint,
                &playback_id,
                generation,
                now_ms,
                now_ms + 60_000,
            )
            .await
            .expect("claim route");
        assert!(store
            .assign_media_session_request_owner(7, generation, generation, "node-a", now_ms,)
            .await
            .expect("assign route owner"));
        let activation = plurx_core::domain::MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: generation.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id,
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: Some(generation.to_owned()),
            request_fingerprint: fingerprint,
            owner_node_id: "node-a".to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms,
            lease_expires_at_ms: now_ms + 60_000,
        };
        store
            .activate_media_session(&activation)
            .await
            .expect("activate route")
            .expect("route accepted");
        store
            .settle_media_session_activation(
                &activation,
                plurx_core::domain::MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now_ms,
            )
            .await
            .expect("confirm route")
            .expect("route confirmed");
        store
            .publish_media_session_activation(7, generation, generation, now_ms)
            .await
            .expect("publish route")
            .expect("route published");
    }

    async fn activate_ended_route(store: &SqliteStore, session_id: &str, generation: &str) {
        activate_control_route(store, session_id, generation).await;
        let ended = store
            .end_media_session(session_id, "deleted", crate::media_sessions::unix_ms())
            .await
            .expect("end durable VOD route")
            .expect("durable VOD route exists");
        assert_eq!(ended.state, "ended");
        assert_eq!(ended.incarnation_id, generation);
    }

    async fn insert_control_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
        touched: Instant,
    ) {
        rendition.attach_reader(session_id, 0).await;
        let rendition_key = rendition.key.clone();
        let file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition: Some(rendition),
                rendition_key,
                file,
                playback_id: "vod-control".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("vod-control", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle(session_id),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(touched),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: None,
                tombstone: None,
            },
        );
    }

    async fn insert_finished_terminal_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
    ) {
        let cleanup = Arc::new(TerminalCleanup::new());
        cleanup.complete();
        insert_terminal_session(serve, session_id, rendition, cleanup).await;
    }

    async fn insert_terminal_session(
        serve: &VodServe,
        session_id: &str,
        rendition: Arc<Rendition>,
        cleanup: Arc<TerminalCleanup>,
    ) {
        let rendition_key = rendition.key.clone();
        let file = Arc::new(rendition.recipe.file.clone());
        serve.shared.sessions.lock().await.insert(
            session_id.to_owned(),
            Session {
                rendition: None,
                rendition_key,
                file,
                playback_id: "vod-terminal".into(),
                user_name: "paul".into(),
                item_title: "Fixture".into(),
                started_unix: 1,
                target_height: 360,
                kind: request("vod-terminal", 0.0).kind,
                supersession_user: "[\"user_id\",1]".into(),
                block_budget: Duration::from_secs(8),
                lifecycle: serve.shared.session_lifecycle(session_id),
                incarnation: Arc::new(()),
                last_touch: StdMutex::new(Instant::now()),
                delivery: Arc::new(crate::meter::Meter::new()),
                control: StdMutex::new(crate::playback_control::ControlState::default()),
                marker_destinations: Vec::new(),
                last_control_snapshot: None,
                control_end: None,
                control_end_snapshot: None,
                terminal_cleanup: Some(cleanup),
                tombstone: Some(Terminal::Deleted),
            },
        );
    }

    /// A synthetic ~7 s-per-segment index, prodsched's own fixture shape.
    fn synthetic_index(fragments: usize) -> FragmentIndex {
        use plurx_core::fmp4::CutClass;
        use plurx_core::segplan::IndexRow;
        let mut rows = Vec::new();
        let mut dts = 0u64;
        for i in 0..fragments {
            let duration = if i % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes: 100_000,
                video_bytes: 99_400,
                class: CutClass::CleanIdr,
            });
            dts += duration;
        }
        FragmentIndex::new(
            16_000,
            rows,
            "sha",
            SourceIdentity::new(1, 1, "fingerprint"),
        )
    }

    /// A rendition built by hand — no driver, no store, no producer — for
    /// tests that exercise one internal mechanism deterministically.
    async fn synthetic_rendition(base: &Path) -> Arc<Rendition> {
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
        let dir = RenditionDir::new(base.join("synthetic"));
        dir.create().await.expect("create rendition dir");
        let plan_len = plan.len();
        Arc::new(Rendition {
            key: "synthetic-rendition".to_string(),
            dir,
            recipe: Recipe {
                file: media_file_at(PathBuf::from("unused.mkv"), ms),
                audio_index: None,
                aac: true,
                video: CopyVideoOptions::new(false, false),
                source_object_version: None,
                cluster_cache_key: None,
                encoding: None,
            },
            source: None,
            playlist: plan.playlist().into_bytes(),
            timescale: plan.timescale,
            seconds_per_segment: plan.duration_ticks() as f64
                / f64::from(plan.timescale)
                / plan.len() as f64,
            index: Some(index),
            policy,
            working_set_budget: 8 << 30,
            completed_cache_budget: 50 << 30,
            materialize_budget: Duration::from_secs(30),
            manifest: Mutex::new(Manifest::new(plan.clone())),
            plan,
            identity: Mutex::new(IdentityState::default()),
            slot: ProducerSlot::new(),
            readers: Mutex::new(HashMap::new()),
            publication_serial: AtomicU64::new(0),
            publication_versions: StdMutex::new(vec![None; plan_len]),
            marker_prewarm_dispatch: StdMutex::new(None),
            active_marker_prewarms: AtomicU32::new(0),
            marker_prewarm_generation: AtomicU64::new(0),
            failed: StdMutex::new(None),
            capacity_hold: StdMutex::new(None),
            ahead_hold: AtomicBool::new(false),
            init_notify: Notify::new(),
            wake: Notify::new(),
            #[cfg(test)]
            stopped_poll_armed: Notify::new(),
            #[cfg(test)]
            stopped_poll_fired: Notify::new(),
            gen_epoch: AtomicU64::new(0),
            last_child_pid: AtomicU32::new(0),
            dormant_since: StdMutex::new(None),
            closed: AtomicBool::new(false),
            warned_admission: AtomicBool::new(false),
            demand_since: StdMutex::new(HashMap::new()),
        })
    }

    #[tokio::test]
    async fn stored_marker_prewarm_is_subordinate_and_hits_only_its_own_production() {
        use plurx_core::segplan::{
            AnnotationProvenance, TimelineAnnotation, TimelineAnnotationSet,
        };

        let base = crate::test_tempdir().expect("base");
        let rendition = synthetic_rendition(base.path()).await;
        let file = &rendition.recipe.file;
        let duration_ms = file.duration_ms.expect("synthetic duration");
        assert!(
            duration_ms > 400_000,
            "fixture must contain the credits marker"
        );
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&plurx_core::domain::NewLibrary {
                name: "Markers".to_owned(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: Vec::new(),
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&plurx_core::domain::NewItem {
                library_id: library.id,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "Marker fixture".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let stored_file_id = store
            .upsert_file(
                item,
                &file.path.to_string_lossy(),
                file.size,
                file.mtime,
                &plurx_core::domain::ProbeResult {
                    duration_ms: Some(duration_ms),
                    ..plurx_core::domain::ProbeResult::default()
                },
            )
            .await
            .expect("file");
        assert_eq!(stored_file_id, file.id);
        let annotations = TimelineAnnotationSet {
            source_identity: crate::http::stream::annotation_source_identity(file),
            generation_id: uuid::Uuid::new_v4().to_string(),
            version: 1,
            annotations: vec![TimelineAnnotation {
                kind: AnnotationKind::Credits,
                start_ticks: 200_000,
                end_ticks: 400_000,
                timescale: 1_000,
                start_ms: 200_000,
                end_ms: 400_000,
                provenance: AnnotationProvenance::Authored,
                confidence_millis: 1_000,
                detector_version: "stored-marker-fixture".to_owned(),
                manual_override_revision: None,
            }],
        };
        store
            .put_timeline_annotation_set(file.id, duration_ms, &annotations)
            .await
            .expect("store exact marker");
        let destinations = stored_marker_destinations(
            store.as_ref(),
            file,
            &rendition.plan,
            rendition.seconds_per_segment,
        )
        .await;
        assert_eq!(destinations.len(), 1);
        let destination = destinations[0];
        assert_eq!(destination.end_ms, 400_000);
        assert!(destination.eligible);

        let frontier = entry_containing(&rendition.plan, 150.0);
        assert!(destination.target_entry > frontier);
        rendition.attach_reader("prewarmed", frontier).await;
        let ledger = rendition.readers.lock().await["prewarmed"]
            .marker_prewarm
            .clone();
        let mut rendering = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        rendering.position_ms = 150_000;
        rendering.buffered_from_ms = Some(145_000);
        rendering.buffered_through_ms = 170_000;
        assert!(apply_marker_prewarm_control(
            &rendition,
            "prewarmed",
            7,
            &rendering,
            &destinations,
        )
        .await
        .is_none());
        assert_eq!(
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records
                .len(),
            1,
            "the approaching stored marker registers one bounded request"
        );

        let ledgers = vec![Arc::clone(&ledger)];
        let mut manifest = rendition.manifest.lock().await;
        let empty_position = Position {
            produced_through: None,
            positioned_at: None,
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: false,
            working_set: WorkingSet::default(),
        };
        assert_eq!(
            decide(&manifest, &[Demand::idle_at(frontier)], empty_position, &[]),
            Action::Reposition { to: frontier },
            "the foreground fixture itself starts at the playhead window"
        );
        let foreground = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            empty_position,
            &[],
            &ledgers,
        );
        assert_eq!(
            foreground.action,
            Action::Reposition { to: frontier },
            "ordinary playhead fill wins before speculative work"
        );
        assert!(foreground.owners.is_empty());
        assert!(
            !ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records[0]
                .active
        );

        let horizon = ((f64::from(AHEAD_HORIZON_SECONDS) / rendition.seconds_per_segment).ceil()
            as u32)
            .max(1);
        let foreground_end = frontier.saturating_add(horizon);
        assert!(foreground_end < destination.target_entry);
        for entry in 0..=foreground_end {
            assert!(manifest.materialize(entry, 1_000, i64::from(entry)));
        }
        let filled_position = Position {
            produced_through: Some(foreground_end),
            positioned_at: Some(0),
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: false,
            working_set: WorkingSet {
                used_bytes: 2,
                budget_bytes: 1,
                held: false,
            },
        };
        let pressured = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            filled_position,
            &[],
            &ledgers,
        );
        assert!(matches!(
            pressured.action,
            Action::Suspend {
                reason: crate::prodsched::Hold::Ahead { .. },
                ..
            }
        ));
        assert!(pressured.owners.is_empty());
        assert!(
            !ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records[0]
                .active,
            "prewarm never makes room under pressure"
        );

        let unpressured = Position {
            working_set: WorkingSet::default(),
            ..filled_position
        };
        let prewarm = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            unpressured,
            &[],
            &ledgers,
        );
        assert_eq!(
            prewarm.action,
            Action::Produce {
                next: foreground_end + 1
            },
            "the same bounded scheduler advances toward the target only after foreground is full"
        );
        assert_eq!(prewarm.owners.len(), 1);
        let producer = Producer::Running {
            produced_through: Some(foreground_end),
            positioned_at: 0,
        };
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, prewarm.action),
            &prewarm,
        );
        let mut prewarm_publication = None;
        for entry in destination.target_entry..=destination.window_end_entry {
            assert!(manifest.materialize(entry, 1_000, i64::from(entry)));
            let publication = credit_marker_prewarm_publication(
                &rendition,
                rendition.gen_epoch.load(Relaxed),
                entry,
            )
            .await;
            if entry == destination.target_entry {
                prewarm_publication = Some(publication);
            }
        }
        let prewarm_publication = prewarm_publication.expect("target publication");
        let completed = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            unpressured,
            &[],
            &ledgers,
        );
        assert!(
            completed.owners.is_empty(),
            "a complete prewarm window stops scheduling but remains correlatable"
        );
        let pending = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(
            pending.matched,
            "the marker beacon binds the unique approach request"
        );
        assert!(
            pending.outcome.is_none(),
            "the approach position is not a landing"
        );
        drop(manifest);

        let mut seeking = rendering.clone();
        seeking.position_ms = destination.end_ms;
        seeking.buffered_from_ms = Some(destination.end_ms);
        seeking.buffered_through_ms = destination.end_ms;
        seeking.render_state = crate::playback_control::RenderState::Seeking;
        seeking.seek_target_ms = Some(destination.end_ms);
        assert!(
            apply_marker_prewarm_control(&rendition, "prewarmed", 9, &seeking, &destinations,)
                .await
                .is_none(),
            "an arbitrary exact-target seek is not a marker skip"
        );

        let mut landed = rendering.clone();
        landed.position_ms = destination.end_ms;
        landed.buffered_from_ms = Some(destination.end_ms);
        landed.buffered_through_ms = destination.end_ms;
        let hit = apply_marker_prewarm_control(&rendition, "prewarmed", 10, &landed, &destinations)
            .await
            .expect("the first settled post-seek snapshot emits the result");
        assert!(hit.hit);
        assert_eq!(hit.requested_sequence, Some(7));
        assert!(hit
            .produced_range
            .is_some_and(|range| (range.first..=range.last).contains(&destination.target_entry)));
        let serve = VodServe::new(
            base.path().join("marker-telemetry"),
            Arc::clone(&store) as Arc<dyn Store>,
        );
        serve.emit_marker_prewarm(
            "prewarmed",
            file.id,
            SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            hit,
        );
        let expected_session = session_log_id("prewarmed");
        let mut emitted = None;
        for _ in 0..3_000 {
            let events = store
                .playback_events(&plurx_core::domain::PlaybackEventQuery {
                    event: Some("marker_prewarm".to_owned()),
                    limit: 10,
                    ..plurx_core::domain::PlaybackEventQuery::default()
                })
                .await
                .expect("read marker telemetry");
            emitted = events
                .into_iter()
                .find(|event| event.session_id.as_deref() == Some(expected_session.as_str()));
            if emitted.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let emitted = emitted.expect("server marker prewarm telemetry persisted");
        assert_eq!(emitted.detail.as_deref(), Some("hit"));
        let emitted_extra: serde_json::Value =
            serde_json::from_str(emitted.extra.as_deref().expect("prewarm proof"))
                .expect("valid prewarm proof JSON");
        assert_eq!(
            emitted_extra["destination_entry"].as_u64(),
            Some(u64::from(destination.target_entry))
        );
        assert_eq!(
            emitted_extra["produced_range"]["first_entry"].as_u64(),
            Some(u64::from(destination.target_entry))
        );
        let ratio = crate::telemetry::prometheus()
            .lines()
            .find_map(|line| {
                line.strip_prefix("plurx_playback_marker_prewarm_hit_ratio ")
                    .and_then(|value| value.parse::<f64>().ok())
            })
            .expect("marker prewarm ratio");
        assert!(ratio > 0.0, "a proven server hit moves the ratio off zero");
        let duplicate = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(duplicate.matched);
        assert!(duplicate.outcome.is_none(), "one skip emits only once");

        let mut inside_marker = rendering.clone();
        inside_marker.position_ms = 250_000;

        // The rendition now contains the landing segment, but it was produced
        // for another playback. This is the rejected ordinary-buffer
        // definition made adversarial: the second playback must still miss.
        rendition.attach_reader("ordinary-buffer", frontier).await;
        apply_marker_prewarm_control(&rendition, "ordinary-buffer", 1, &rendering, &destinations)
            .await;
        apply_marker_prewarm_control(
            &rendition,
            "ordinary-buffer",
            2,
            &inside_marker,
            &destinations,
        )
        .await;
        let ordinary_ledger = rendition.readers.lock().await["ordinary-buffer"]
            .marker_prewarm
            .clone();
        let miss = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut ledger = ordinary_ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let beacon = ledger.note_client_skip(&manifest, &publications);
            assert!(beacon.matched);
            assert!(beacon.outcome.is_none());
            ledger
                .settle_pending_at(destination.target_entry, &manifest, &publications)
                .expect("a later landing emits the uncredited miss")
        };
        assert!(!miss.hit);
        assert_eq!(miss.produced_range, None);
        let duplicate_inside = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ordinary_ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(duplicate_inside.matched);
        assert!(
            duplicate_inside.outcome.is_none(),
            "an inside-marker arm is consumed and cannot emit twice"
        );

        // Old credit cannot attach to a new ordinary publication of the same
        // entry after eviction.
        apply_marker_prewarm_control(&rendition, "prewarmed", 11, &inside_marker, &destinations)
            .await;
        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.evict(destination.target_entry));
        assert!(manifest.materialize(destination.target_entry, 2_000, 2));
        rendition.active_marker_prewarms.store(0, Release);
        let ordinary_publication = credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            destination.target_entry,
        )
        .await;
        assert_ne!(ordinary_publication, prewarm_publication);
        let rematerialized = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut ledger = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let beacon = ledger.note_client_skip(&manifest, &publications);
            assert!(beacon.matched);
            assert!(beacon.outcome.is_none());
            ledger
                .settle_pending_at(destination.target_entry, &manifest, &publications)
                .expect("the repeated explicit skip settles")
        };
        assert!(!rematerialized.hit, "ordinary regeneration is not prewarm");
        drop(manifest);

        // A disabled playback never receives another reader's shared
        // prewarm attribution.
        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.evict(destination.target_entry));
        drop(manifest);
        apply_marker_prewarm_control(&rendition, "prewarmed", 12, &rendering, &destinations).await;
        let mut held = rendering.clone();
        held.demand = crate::playback_control::PlaybackDemand::Hold;
        apply_marker_prewarm_control(&rendition, "prewarmed", 13, &held, &destinations).await;
        rendition.attach_reader("enabled", frontier).await;
        apply_marker_prewarm_control(&rendition, "enabled", 1, &rendering, &destinations).await;
        let enabled_ledger = rendition.readers.lock().await["enabled"]
            .marker_prewarm
            .clone();
        let mut manifest = rendition.manifest.lock().await;
        let enabled = decide_with_marker_prewarm(
            &manifest,
            &[Demand::idle_at(frontier)],
            unpressured,
            &[],
            &[Arc::clone(&ledger), Arc::clone(&enabled_ledger)],
        );
        assert_eq!(enabled.owners.len(), 1);
        assert!(!ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record.active));
        assert!(enabled_ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record.active));
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, enabled.action),
            &enabled,
        );
        assert!(manifest.materialize(destination.target_entry, 3_000, 3));
        let shared_publication = credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            destination.target_entry,
        )
        .await;
        let publications = rendition
            .publication_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            publications[destination.target_entry as usize],
            Some(shared_publication)
        );
        assert!(enabled_ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record
                .credited_publication(destination.target_entry, Some(shared_publication))));
        assert!(!ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .any(|record| record
                .credited_publication(destination.target_entry, Some(shared_publication))));
    }

    #[tokio::test]
    async fn marker_prewarm_skip_time_and_request_identity_are_immutable() {
        let base = crate::test_tempdir().expect("base");
        let rendition = synthetic_rendition(base.path()).await;
        let target_entry = entry_containing(&rendition.plan, 200.0);
        let destination = MarkerDestination {
            kind: AnnotationKind::Intro,
            start_ms: 100_000,
            end_ms: 200_000,
            target_entry,
            window_end_entry: target_entry.saturating_add(1),
            eligible: true,
        };
        let frontier = entry_containing(&rendition.plan, 50.0);
        let mut approach = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        approach.position_ms = 50_000;
        approach.buffered_from_ms = Some(45_000);
        approach.buffered_through_ms = 65_000;
        rendition.attach_reader("race", frontier).await;
        apply_marker_prewarm_control(&rendition, "race", 1, &approach, &[destination]).await;
        let ledger = rendition.readers.lock().await["race"]
            .marker_prewarm
            .clone();

        let manifest = rendition.manifest.lock().await;
        let candidate = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate(&manifest)
            .expect("approach candidate");
        let owners = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activate(candidate)
            .into_iter()
            .map(|record_nonce| MarkerPrewarmOwner {
                ledger: Arc::clone(&ledger),
                record_nonce,
            })
            .collect::<Vec<_>>();
        let decision = MarkerPrewarmDecision {
            action: Action::Reposition { to: target_entry },
            candidate: Some(candidate),
            owners,
        };
        let producer = Producer::Running {
            produced_through: None,
            positioned_at: target_entry,
        };
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, decision.action),
            &decision,
        );
        let before_publication = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(before_publication.matched);
        assert!(before_publication.outcome.is_none());
        drop(manifest);

        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.materialize(target_entry, 1_000, 1));
        credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            target_entry,
        )
        .await;
        let after_beacon = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settle_pending_at(target_entry, &manifest, &publications)
                .expect("landing settles the snapshotted miss")
        };
        assert!(
            !after_beacon.hit,
            "production after skip time cannot upgrade the miss"
        );

        // Reusing protocol sequence 1 after a settled request creates a new
        // ledger nonce. The old in-flight dispatch cannot credit that record.
        assert!(manifest.evict(target_entry));
        drop(manifest);
        apply_marker_prewarm_control(&rendition, "race", 1, &approach, &[destination]).await;
        let new_nonce = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .approach_skip
            .expect("replacement request")
            .nonce;
        assert_ne!(new_nonce, decision.owners[0].record_nonce);
        let mut manifest = rendition.manifest.lock().await;
        assert!(manifest.materialize(target_entry, 2_000, 2));
        credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            target_entry,
        )
        .await;
        let reused_sequence = {
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut state = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(state.note_client_skip(&manifest, &publications).matched);
            state
                .settle_pending_at(target_entry, &manifest, &publications)
                .expect("replacement landing")
        };
        assert!(
            !reused_sequence.hit,
            "an old dispatch cannot credit a nonce replacement"
        );
        assert!(manifest.evict(target_entry));
        drop(manifest);

        // The opposite network ordering is also deterministic: a Rendering
        // snapshot can prove landing before the fire-and-forget beacon arrives.
        rendition.attach_reader("late-beacon", frontier).await;
        apply_marker_prewarm_control(&rendition, "late-beacon", 1, &approach, &[destination]).await;
        let late = rendition.readers.lock().await["late-beacon"]
            .marker_prewarm
            .clone();
        let mut manifest = rendition.manifest.lock().await;
        let candidate = late
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .candidate(&manifest)
            .expect("late candidate");
        let owners = late
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activate(candidate)
            .into_iter()
            .map(|record_nonce| MarkerPrewarmOwner {
                ledger: Arc::clone(&late),
                record_nonce,
            })
            .collect();
        let decision = MarkerPrewarmDecision {
            action: Action::Reposition { to: target_entry },
            candidate: Some(candidate),
            owners,
        };
        update_marker_prewarm_dispatch(
            &rendition,
            producer,
            next_step(producer, decision.action),
            &decision,
        );
        assert!(manifest.materialize(target_entry, 3_000, 3));
        credit_marker_prewarm_publication(
            &rendition,
            rendition.gen_epoch.load(Relaxed),
            target_entry,
        )
        .await;
        drop(manifest);
        let mut landed = approach.clone();
        landed.position_ms = destination.end_ms;
        landed.buffered_from_ms = Some(destination.end_ms);
        landed.buffered_through_ms = destination.end_ms;
        assert!(
            apply_marker_prewarm_control(&rendition, "late-beacon", 2, &landed, &[destination],)
                .await
                .is_none(),
            "landing waits for explicit skip intent"
        );
        let late_hit = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            late.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
                .outcome
                .expect("delayed beacon settles the latched landing")
        };
        assert!(late_hit.hit);

        // A natural pass can leave an exact landing waiting for a beacon.
        // Re-entering the same marker makes that old landing stale; a new
        // beacon, even if Seeking overtakes it on the control channel, must
        // wait for the new seek's own landing.
        {
            let mut manifest = rendition.manifest.lock().await;
            assert!(manifest.evict(target_entry));
        }
        rendition.attach_reader("rewind", frontier).await;
        apply_marker_prewarm_control(&rendition, "rewind", 1, &approach, &[destination]).await;
        let rewind = rendition.readers.lock().await["rewind"]
            .marker_prewarm
            .clone();
        {
            let mut manifest = rendition.manifest.lock().await;
            let candidate = rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .candidate(&manifest)
                .expect("rewind candidate");
            let owners = rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .activate(candidate)
                .into_iter()
                .map(|record_nonce| MarkerPrewarmOwner {
                    ledger: Arc::clone(&rewind),
                    record_nonce,
                })
                .collect();
            let rewind_decision = MarkerPrewarmDecision {
                action: Action::Reposition { to: target_entry },
                candidate: Some(candidate),
                owners,
            };
            update_marker_prewarm_dispatch(
                &rendition,
                producer,
                next_step(producer, rewind_decision.action),
                &rewind_decision,
            );
            assert!(manifest.materialize(target_entry, 4_000, 4));
            credit_marker_prewarm_publication(
                &rendition,
                rendition.gen_epoch.load(Relaxed),
                target_entry,
            )
            .await;
        }
        assert!(
            apply_marker_prewarm_control(&rendition, "rewind", 2, &landed, &[destination])
                .await
                .is_none(),
            "a natural traversal waits for an explicit marker beacon"
        );
        assert!(rewind
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .awaiting_beacon
            .is_some());
        assert!(
            apply_marker_prewarm_control(&rendition, "rewind", 3, &landed, &[destination])
                .await
                .is_none()
        );
        assert!(
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .awaiting_beacon
                .is_none(),
            "a landing-first reorder latch expires at the next accepted control"
        );

        let mut reentered = approach.clone();
        reentered.position_ms = destination.start_ms;
        apply_marker_prewarm_control(&rendition, "rewind", 4, &reentered, &[destination]).await;
        {
            let state = rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                state.awaiting_beacon.is_none(),
                "re-entry invalidates the old traversal's landing"
            );
            assert!(state.armed_skip.is_some());
        }
        let mut held_before_beacon = reentered.clone();
        held_before_beacon.demand = crate::playback_control::PlaybackDemand::Hold;
        held_before_beacon.playback_rate = 0.0;
        apply_marker_prewarm_control(&rendition, "rewind", 5, &held_before_beacon, &[destination])
            .await;
        assert!(
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .armed_skip
                .is_some(),
            "a pause disables speculation without erasing skip correlation"
        );

        let mut seeking_first = held_before_beacon.clone();
        seeking_first.render_state = crate::playback_control::RenderState::Seeking;
        seeking_first.seek_target_ms = Some(destination.end_ms);
        apply_marker_prewarm_control(&rendition, "rewind", 6, &seeking_first, &[destination]).await;
        let reordered_beacon = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(reordered_beacon.matched);
        assert!(
            reordered_beacon.outcome.is_none(),
            "Seeking cannot turn the previous traversal into an immediate hit"
        );
        let reordered_hit =
            apply_marker_prewarm_control(&rendition, "rewind", 7, &landed, &[destination])
                .await
                .expect("the new skip settles only at its new landing");
        assert!(reordered_hit.hit);

        apply_marker_prewarm_control(&rendition, "rewind", 8, &held_before_beacon, &[destination])
            .await;
        let held_replay = {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rewind
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications)
        };
        assert!(held_replay.matched);
        assert!(held_replay.outcome.is_none());
        let held_replay_miss =
            apply_marker_prewarm_control(&rendition, "rewind", 9, &landed, &[destination])
                .await
                .expect("a held replay still emits one authoritative result");
        assert!(
            !held_replay_miss.hit,
            "a replay that could not schedule new prewarm is a miss"
        );

        let second = MarkerDestination {
            kind: AnnotationKind::Credits,
            start_ms: 250_000,
            end_ms: 300_000,
            target_entry: entry_containing(&rendition.plan, 300.0),
            window_end_entry: entry_containing(&rendition.plan, 300.0).saturating_add(1),
            eligible: true,
        };
        let mut second_approach = approach.clone();
        second_approach.position_ms = 220_000;
        apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            3,
            &second_approach,
            &[destination, second],
        )
        .await;
        {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let second_beacon = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications);
            assert!(second_beacon.matched, "a later marker is not a duplicate");
            assert!(second_beacon.outcome.is_none());
        }
        let mut second_landing = second_approach.clone();
        second_landing.position_ms = second.end_ms;
        second_landing.buffered_from_ms = Some(second.end_ms);
        second_landing.buffered_through_ms = second.end_ms;
        let second_miss = apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            4,
            &second_landing,
            &[destination, second],
        )
        .await
        .expect("the second marker emits independently");
        assert!(!second_miss.hit);
        assert_eq!(second_miss.requested_sequence, Some(3));

        // An abandoned first seek cannot reserve the one sessionless beacon
        // slot forever. Reaching a different exact marker opportunity cancels
        // that stale pending request and lets the later marker settle.
        apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            5,
            &approach,
            &[destination, second],
        )
        .await;
        {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first_abandoned = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications);
            assert!(first_abandoned.matched);
            assert!(first_abandoned.outcome.is_none());
        }
        apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            6,
            &second_approach,
            &[destination, second],
        )
        .await;
        {
            let manifest = rendition.manifest.lock().await;
            let publications = rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let later_beacon = late
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .note_client_skip(&manifest, &publications);
            assert!(later_beacon.matched);
            assert!(
                later_beacon.outcome.is_none(),
                "marker B replaces marker A's abandoned pending seek"
            );
        }
        let later_landing = apply_marker_prewarm_control(
            &rendition,
            "late-beacon",
            7,
            &second_landing,
            &[destination, second],
        )
        .await
        .expect("marker B settles after marker A was abandoned");
        assert!(!later_landing.hit);
        assert_eq!(later_landing.requested_sequence, Some(6));

        // Publishing the speculative window endpoint exhausts attribution,
        // but the producer-origin fence survives until the old generation is
        // physically retired.
        {
            let mut manifest = rendition.manifest.lock().await;
            if manifest
                .state(destination.window_end_entry)
                .is_some_and(SegState::is_materialized)
            {
                assert!(manifest.evict(destination.window_end_entry));
            }
            assert!(manifest.materialize(destination.window_end_entry, 5_000, 5));
        }
        let old_epoch = rendition.gen_epoch.load(Relaxed);
        credit_marker_prewarm_publication(&rendition, old_epoch, destination.window_end_entry)
            .await;
        assert!(rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());
        assert_eq!(rendition.active_marker_prewarms.load(Acquire), 0);
        assert_eq!(
            rendition.marker_prewarm_generation.load(Acquire),
            old_epoch.saturating_add(1)
        );
        assert!(fence_marker_prewarm_before_room(
            &rendition,
            producer,
            Step::MakeRoom { wanted: 1 },
        ));
        assert_eq!(rendition.gen_epoch.load(Relaxed), old_epoch + 1);
        assert_eq!(rendition.active_marker_prewarms.load(Acquire), 0);
        assert!(rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());

        // Once real playhead demand adopts that same process, it is no longer
        // speculative and a later capacity pass must not kill it.
        let current_epoch = rendition.gen_epoch.load(Relaxed);
        rendition
            .marker_prewarm_generation
            .store(current_epoch.saturating_add(1), Release);
        let stale_owner = Arc::new(StdMutex::new(MarkerPrewarmLedger::default()));
        {
            let mut state = stale_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.records.push(MarkerPrewarmRecord {
                destination,
                requested_sequence: 10,
                nonce: 1,
                schedulable: false,
                active: true,
                settled: false,
                produced: Vec::new(),
            });
        }
        *rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(MarkerPrewarmDispatch {
            producer_epoch: current_epoch,
            candidate: MarkerPrewarmCandidate {
                target_entry,
                window_end_entry: target_entry,
                target_materialized: false,
            },
            owners: vec![MarkerPrewarmOwner {
                ledger: Arc::clone(&stale_owner),
                record_nonce: 1,
            }],
        });
        rendition.active_marker_prewarms.store(1, Release);
        let stopped = Producer::Stopped {
            produced_through: Some(target_entry.saturating_sub(1)),
            positioned_at: target_entry,
            reason: crate::prodsched::Hold::Ahead { horizon: 1 },
        };
        let foreground = MarkerPrewarmDecision {
            action: Action::Produce { next: target_entry },
            candidate: None,
            owners: Vec::new(),
        };
        let resume = next_step(stopped, foreground.action);
        assert_eq!(resume, Step::Resume);
        update_marker_prewarm_dispatch(&rendition, stopped, resume, &foreground);
        assert_eq!(rendition.marker_prewarm_generation.load(Acquire), 0);
        assert_eq!(rendition.active_marker_prewarms.load(Acquire), 0);
        assert!(rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none());
        {
            let mut manifest = rendition.manifest.lock().await;
            if manifest
                .state(target_entry)
                .is_some_and(SegState::is_materialized)
            {
                assert!(manifest.evict(target_entry));
            }
            assert!(manifest.materialize(target_entry, 6_000, 6));
        }
        credit_marker_prewarm_publication(&rendition, current_epoch, target_entry).await;
        assert!(
            stale_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .records[0]
                .produced
                .is_empty(),
            "foreground publication cannot inherit stale prewarm owners"
        );
        assert!(!fence_marker_prewarm_before_room(
            &rendition,
            Producer::Running {
                produced_through: Some(target_entry),
                positioned_at: target_entry,
            },
            Step::MakeRoom { wanted: 1 },
        ));

        // With no foreground owner and no remaining attribution, the same
        // ahead decision retires speculative ffmpeg instead of parking it.
        rendition
            .marker_prewarm_generation
            .store(current_epoch.saturating_add(1), Release);
        let running = Producer::Running {
            produced_through: Some(target_entry),
            positioned_at: target_entry,
        };
        let ahead = MarkerPrewarmDecision {
            action: Action::Suspend {
                produced_through: target_entry,
                reason: crate::prodsched::Hold::Ahead { horizon: 1 },
            },
            candidate: None,
            owners: Vec::new(),
        };
        assert!(matches!(
            retire_completed_marker_prewarm(
                &rendition,
                running,
                &ahead,
                next_step(running, ahead.action),
            ),
            Step::Terminate {
                why: Termination::Idle
            }
        ));
    }

    #[tokio::test]
    async fn marker_prewarm_placeholder_correlates_without_a_client_session_id() {
        let base = crate::test_tempdir().expect("base");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("in-memory store"));
        let serve = VodServe::new(base.path().join("serve"), store);
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(
            &serve,
            "marker-client",
            Arc::clone(&rendition),
            Instant::now(),
        )
        .await;

        let target_entry = entry_containing(&rendition.plan, 200.0);
        let destination = MarkerDestination {
            kind: AnnotationKind::Intro,
            start_ms: 100_000,
            end_ms: 200_000,
            target_entry,
            window_end_entry: target_entry.saturating_add(1),
            eligible: true,
        };
        let mut inside = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Apple,
        );
        inside.position_ms = 50_000;
        inside.buffered_from_ms = Some(45_000);
        inside.buffered_through_ms = 65_000;
        apply_marker_prewarm_control(&rendition, "marker-client", 1, &inside, &[destination]).await;
        {
            let mut sessions = serve.shared.sessions.lock().await;
            let session = sessions.get_mut("marker-client").expect("session");
            session.marker_destinations = vec![destination];
            session.last_control_snapshot = Some(inside);
        }

        insert_control_session(
            &serve,
            "same-file-unarmed",
            Arc::clone(&rendition),
            Instant::now(),
        )
        .await;
        {
            let mut sessions = serve.shared.sessions.lock().await;
            let ended = sessions
                .get_mut("same-file-unarmed")
                .expect("second session");
            ended.kind = SessionKind::Transcode { height: 720 };
            ended.rendition = None;
            ended.tombstone = Some(Terminal::Deleted);
        }
        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "remux")
                .await,
            "method filtering cannot hide another same-file VOD candidate"
        );
        serve
            .shared
            .sessions
            .lock()
            .await
            .remove("same-file-unarmed");

        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "transcode",)
                .await,
            "a transcode beacon cannot consume a copy/remux ledger"
        );
        {
            let mut sessions = serve.shared.sessions.lock().await;
            sessions.get_mut("marker-client").expect("session").kind =
                SessionKind::Transcode { height: 720 };
        }
        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "remux")
                .await,
            "a remux beacon cannot consume a transcode ledger"
        );
        {
            let mut sessions = serve.shared.sessions.lock().await;
            sessions.get_mut("marker-client").expect("session").kind = SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            };
        }

        assert!(
            serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "remux")
                .await,
            "an early auto-skip beacon binds the unique approach request without a session id"
        );
        assert!(
            !serve
                .consume_marker_prewarm_placeholder(1, rendition.recipe.file.id, "direct_play")
                .await,
            "direct play never consumes its client-owned miss"
        );
    }

    async fn create(
        serve: &Arc<VodServe>,
        file: &MediaFile,
        session_id: &str,
        playback_id: &str,
        settings: &VodSettings,
    ) -> VodStart {
        serve
            .try_create(
                &request(playback_id, 0.0),
                file,
                settings,
                VodAttribution {
                    user_name: "paul",
                    item_title: "Fixture",
                    supersession_user: "[\"user_id\",1]",
                },
                session_id.to_string(),
            )
            .await
            .expect("the fixture is VOD-presentable")
    }

    async fn fetch(serve: &Arc<VodServe>, session: &str, name: &str) -> SegmentReady {
        // A blocking GET may answer a typed Pending under a slow generation;
        // that is the protocol working, so retry the way a client would.
        for _ in 0..20 {
            match serve.segment(session, name).await {
                Some(VodPublication {
                    result: Ok(Some(ready)),
                    owner,
                }) => {
                    let index = planned_index(name);
                    assert!(serve.commit_resolved_media(session, &owner, index).await);
                    return ready;
                }
                Some(VodPublication {
                    result: Err(VodError::Pending { .. }),
                    ..
                }) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                other => panic!("fetching {name}: unexpected answer {:?}", describe(other)),
            }
        }
        panic!("fetching {name} never materialized");
    }

    fn describe(answer: Option<VodPublication<Option<SegmentReady>>>) -> String {
        match answer {
            None => "None".into(),
            Some(VodPublication {
                result: Ok(None), ..
            }) => "Ok(None)".into(),
            Some(VodPublication {
                result: Ok(Some(ready)),
                ..
            }) => format!("Ok(Some(len {}))", ready.len),
            Some(VodPublication {
                result: Err(error), ..
            }) => format!("Err({error:?})"),
        }
    }

    async fn read_ready(ready: SegmentReady) -> Vec<u8> {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let mut file = ready.file;
        file.read_to_end(&mut bytes).await.expect("read segment");
        bytes
    }

    async fn plan_len(serve: &Arc<VodServe>, session: &str) -> usize {
        let sessions = serve.shared.sessions.lock().await;
        sessions
            .get(session)
            .expect("session")
            .live_rendition()
            .expect("live rendition")
            .plan
            .len()
    }

    async fn rendition_of(serve: &Arc<VodServe>, session: &str) -> Arc<Rendition> {
        let sessions = serve.shared.sessions.lock().await;
        Arc::clone(
            sessions
                .get(session)
                .expect("session")
                .live_rendition()
                .expect("live rendition"),
        )
    }

    async fn wait_until(what: &str, deadline: Duration, mut check: impl AsyncFnMut() -> bool) {
        let started = Instant::now();
        while started.elapsed() < deadline {
            if check().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{what} never happened within {deadline:?}");
    }

    #[tokio::test]
    async fn terminal_cleanup_completion_after_wait_registration_is_not_lost() {
        let cleanup = Arc::new(TerminalCleanup::new());
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *cleanup
            .wait_enabled_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let waiter = tokio::spawn({
            let cleanup = Arc::clone(&cleanup);
            async move { cleanup.wait().await }
        });

        // `wait` has enabled its Notified future but has not performed the
        // state re-check. `notify_waiters` in this exact gap used to vanish.
        pause.wait().await;
        cleanup.complete();
        pause.wait().await;
        tokio::time::timeout(Duration::from_millis(250), waiter)
            .await
            .expect("registered terminal waiter must observe completion")
            .expect("terminal waiter task");
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_retention_starts_when_cleanup_completes() {
        let cleanup = TerminalCleanup::new();
        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION + Duration::from_secs(1)).await;
        assert!(
            !cleanup.retention_expired(),
            "creation time must not consume the post-cleanup replay window"
        );

        cleanup.complete();
        assert!(!cleanup.retention_expired());
        tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION - Duration::from_millis(1)).await;
        assert!(!cleanup.retention_expired());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(cleanup.retention_expired());
    }

    /// M6's preparation slot exists on the engine that serves the sessions.
    ///
    /// This is the whole point of moving it off the rolling actor. `create`
    /// sets `Presentation::Vod` for every session, so a slot only the actor
    /// held would have staged successors on a path viewers do not take — and
    /// M6 §3.4's acceptance would have passed while the feature fired on
    /// nothing, which is the defect class the replacement seam already had
    /// once.
    #[tokio::test]
    async fn a_vod_session_holds_a_preparation_slot() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;

        let gate = serve
            .preparation_gate(&session_id)
            .await
            .expect("a live VOD session has a gate");
        let successor = uuid::Uuid::new_v4().to_string();
        let predecessor = uuid::Uuid::new_v4().to_string();
        assert!(
            gate.stage_preparation(successor.clone(), predecessor.clone(), i64::MAX, None)
                .await
        );
        assert!(
            !gate.may_commit_preparation(&successor).await,
            "staging alone is not commit authority; a client acknowledgement must reserve it"
        );
        // One per playback, and the second ask is refused rather than
        // replacing the first: the store's primary key would reject it, and an
        // engine that believed in two could commit the wrong one.
        assert!(
            !gate
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    predecessor,
                    i64::MAX,
                    None
                )
                .await
        );
        assert!(gate.settle_preparation(&successor, true).await);
        assert!(
            !gate.may_commit_preparation(&successor).await,
            "a settled successor is no longer committable",
        );
    }

    /// An abandoned successor frees the slot, on this engine too.
    ///
    /// `settle_preparation(_, false)` aborts before it clears, and it is the
    /// only VOD-specific logic in the whole implementation — the rolling side
    /// pins the same pair, and the two must not drift.
    #[tokio::test]
    async fn an_abandoned_successor_frees_the_vod_slot() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let gate = serve.preparation_gate(&session_id).await.expect("gate");

        let abandoned = uuid::Uuid::new_v4().to_string();
        assert!(
            gate.stage_preparation(
                abandoned.clone(),
                uuid::Uuid::new_v4().to_string(),
                i64::MAX,
                None,
            )
            .await
        );
        assert!(gate.settle_preparation(&abandoned, false).await);
        assert!(
            !gate.may_commit_preparation(&abandoned).await,
            "an aborted successor is not committable",
        );
        // And the slot is free for the next one, which is the property that
        // distinguishes an abandon from a leak.
        assert!(
            gate.stage_preparation(
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                i64::MAX,
                None,
            )
            .await
        );
    }

    /// A gate outlives its attachment, and a session id does not identify one.
    ///
    /// An idle reap removes a VOD session deliberately *without* a tombstone,
    /// and a reconnect resurrects the same durable id sharing the same
    /// lifecycle gate — that gate is stable across exactly this, on purpose.
    /// So a gate that trusted the id would take the replacement's slot for a
    /// successor whose predecessor no longer holds the pointer, and the
    /// store's CAS would be the only thing left. The incarnation pin is what
    /// stops that.
    #[tokio::test]
    async fn a_gate_does_not_follow_its_session_id_to_a_new_incarnation() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let stale = serve.preparation_gate(&session_id).await.expect("gate");

        // The reap, then the resurrection under the same id.
        serve.shared.sessions.lock().await.remove(&session_id);
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;

        assert!(
            !stale
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                    None,
                )
                .await,
            "the stale gate must not take the replacement's slot",
        );
        // And the new attachment's own gate is unobstructed, which is what
        // that refusal is protecting.
        let fresh = serve.preparation_gate(&session_id).await.expect("gate");
        assert!(
            fresh
                .stage_preparation(
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    i64::MAX,
                    None,
                )
                .await
        );
    }

    /// Ending a session ends its staged successor, under the same lock.
    ///
    /// This is what lets `may_commit_preparation` trust the slot rather than
    /// layering a second refusal on it: the rolling actor's `terminate` aborts
    /// the slot it holds, and every VOD path that writes a tombstone now does
    /// the same. Without it the gate would say no while `ControlState` still
    /// said the successor was committable — two authorities over one slot.
    #[tokio::test]
    async fn ending_a_vod_session_ends_its_staged_successor() {
        let base = crate::test_tempdir().expect("base");
        let serve = bare_serve(base.path());
        let session_id = uuid::Uuid::new_v4().to_string();
        let rendition = synthetic_rendition(base.path()).await;
        insert_control_session(&serve, &session_id, Arc::clone(&rendition), Instant::now()).await;
        let gate = serve.preparation_gate(&session_id).await.expect("gate");
        let staged = uuid::Uuid::new_v4().to_string();
        assert!(
            gate.stage_preparation(
                staged.clone(),
                uuid::Uuid::new_v4().to_string(),
                i64::MAX,
                None
            )
            .await
        );

        serve.begin_end(&session_id, Terminal::Deleted).await;

        assert!(
            !gate.may_commit_preparation(&staged).await,
            "the slot itself refuses, not a check layered over it",
        );
        let sessions = serve.shared.sessions.lock().await;
        let session = sessions.get(&session_id).expect("session");
        assert!(
            !session
                .control
                .lock()
                .expect("control lock")
                .may_commit_preparation(&staged),
            "and the state agrees with the gate rather than contradicting it",
        );
    }
