
    /// A converted copy cannot answer an idempotency replay meant for an
    /// unconverted one.
    ///
    /// The fingerprint is what `claim_request` compares when a `request_id` is
    /// replayed, so every field that changes the output bytes has to be in it.
    /// A converted stream's RPUs are rewritten and its configuration record is
    /// different; answering a replay with the other one hands a client a
    /// stream it never asked for and that its decoder may refuse.
    ///
    /// The suffix is appended rather than folded into the existing digits so
    /// an unconverted request fingerprints to exactly the string it always
    /// did — a replay in flight across a deploy still recovers its session.
    #[test]
    fn a_converting_copy_fingerprints_apart_from_the_copy_it_replaces() {
        let copy = |convert: bool| SessionRequest {
            control_sequence: None,
            file_id: 1,
            playback_id: "p".to_owned(),
            request_id: Some("r".to_owned()),
            start_seconds: 0.0,
            audio_index: None,
            kind: SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: true,
                convert_dolby_vision: convert,
            },
            automatic: false,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
            previous_session_id: None,
            reopen_reason: None,
        };

        let plain = copy(false);
        let converting = copy(true);
        assert_ne!(
            plain.durable_intent_fingerprint(7),
            converting.durable_intent_fingerprint(7)
        );
        assert!(converting.intent_fingerprint("paul").contains("+p81"));
        assert!(
            plain.intent_fingerprint("paul").contains("c0d1"),
            "an unconverted copy keeps the exact string it always had: {}",
            plain.intent_fingerprint("paul")
        );
        assert!(!plain.intent_fingerprint("paul").contains("p81"));
    }

    #[test]
    fn ffmpeg_gets_the_app_owned_runtime_cache() {
        use plurx_core::store::SqliteStore;

        let root = crate::test_tempdir().expect("root");
        let work = root.path().join("transcode");
        let finished = root.path().join("cache").join("transcode");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let manager = TranscodeManager::new(store, work, EncoderCaps::default(), Pipeline::Cpu)
            .with_cache(finished, "test-ffmpeg".into(), "test-node".into());

        let expected = root.path().join("cache").join("runtime");
        assert_eq!(manager.runtime_cache, expected);
        assert!(expected.is_dir(), "the cache exists before ffmpeg starts");

        let mut command = tokio::process::Command::new("ffmpeg");
        crate::producer_spawn::configure_ffmpeg_runtime(&mut command, &manager.runtime_cache);
        let inherited = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == "XDG_CACHE_HOME")
            .and_then(|(_, value)| value)
            .expect("XDG_CACHE_HOME");
        assert_eq!(inherited, expected.as_os_str());
    }

    #[test]
    fn safe_segment_names() {
        assert!(is_safe_segment("seg00000.ts"));
        assert!(is_safe_segment("seg12345.ts"));
        assert!(is_safe_segment("seg00000.m4s")); // copy fMP4 segment
        assert!(is_safe_segment("init.mp4")); // copy fMP4 init
        assert!(!is_safe_segment("seg.ts"));
        assert!(!is_safe_segment("seg.m4s"));
        assert!(!is_safe_segment("../seg00000.ts"));
        assert!(!is_safe_segment("index.m3u8"));
        assert!(!is_safe_segment("other.mp4"));
        assert!(!is_safe_segment("seg0/../../etc.ts"));
    }

    /// A fenced successor's `EXT-X-MAP` names its own init object. If the
    /// serving allowlist does not know that shape, the very first thing a
    /// taken-over copy session advertises is unretrievable and no fMP4
    /// segment can be decoded for the rest of the session.
    #[test]
    fn every_generation_init_object_is_routable() {
        assert_eq!(init_object_name(None), "init.mp4");
        assert_eq!(init_object_name(Some(1)), "init.mp4");
        assert_eq!(init_object_name(Some(2)), "init-e2.mp4");
        assert_eq!(init_object_name(Some(37)), "init-e37.mp4");

        for epoch in [None, Some(1), Some(2), Some(37), Some(2_000)] {
            let name = init_object_name(epoch);
            assert!(is_init_object(&name), "{name} is an init object");
            assert!(is_safe_segment(&name), "{name} must be servable");
            assert_eq!(segment_index(&name), None, "{name} is not a segment index");
        }

        // The shape is still an allowlist, not a prefix match.
        assert!(!is_init_object("init-e.mp4"));
        assert!(!is_init_object("init-ex.mp4"));
        assert!(!is_init_object("init-e2.mp4.bak"));
        assert!(!is_init_object("init-e2/../../etc.mp4"));
        assert!(!is_safe_segment("init-e.mp4"));
        assert!(!is_safe_segment("init-ex.mp4"));
    }

    /// A remux begins at the keyframe at or before its requested start, so
    /// the offset a session records must come from the origin it *achieved*.
    /// Recording the requested one over-reports this generation's frontier by
    /// the pull-back, and the next successor resumes past a span no
    /// generation ever produced.
    #[tokio::test]
    async fn a_session_measures_its_offset_from_the_origin_it_reached() {
        let dir = crate::test_tempdir().expect("tempdir");
        let takeover = SessionTakeoverStart {
            provisional_session_id: "provisional-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            origin_base_ms: 120_000,
            frontier_offset_ms: 600_000,
            media_sequence: 2_000_000,
            discontinuity_sequence: 1,
            owner_epoch: 2,
        };

        // Asked to resume at 720.000s absolute — 600s past the row's origin —
        // and an accurate seek lands exactly there.
        let mut exact = test_session(dir.path().join("exact"));
        exact.takeover = Some(takeover.clone());
        exact.media_origin_seconds = 720.0;
        assert_eq!(exact.frontier_offset_ms(), 600_000);

        // The same request on a remux whose nearest keyframe is 9s earlier.
        let mut pulled_back = test_session(dir.path().join("pulled-back"));
        pulled_back.takeover = Some(takeover);
        pulled_back.media_origin_seconds = 711.0;
        assert_eq!(
            pulled_back.frontier_offset_ms(),
            591_000,
            "the offset follows the media this generation really starts at"
        );

        let ordinary = test_session(dir.path().join("ordinary"));
        assert_eq!(ordinary.frontier_offset_ms(), 0);
    }

    /// DELETE and the peer abort prove ownership from the process-local
    /// request record. A fenced successor has none — nothing on this node
    /// requested it — so without the incarnation carried on the session both
    /// return success while the replacement encoder keeps running.
    #[tokio::test]
    async fn delete_reaches_a_taken_over_session_with_no_request_record() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let mut session = test_session(dir.path().join("session"));
        session.takeover = Some(SessionTakeoverStart {
            provisional_session_id: "provisional-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            origin_base_ms: 0,
            frontier_offset_ms: 0,
            media_sequence: 2_000_000,
            discontinuity_sequence: 1,
            owner_epoch: 2,
        });
        mgr.sessions
            .lock()
            .await
            .insert("capability-a".into(), Arc::new(session));

        assert!(
            !mgr.stop_session_for_request("incarnation-b", "capability-a", "test")
                .await,
            "a different incarnation may not stop this worker"
        );
        assert!(
            mgr.sessions.lock().await.contains_key("capability-a"),
            "the refused abort left the worker alone"
        );
        assert!(
            mgr.stop_session_for_request("incarnation-a", "capability-a", "test")
                .await,
            "the incarnation that owns the successor stops it"
        );
        assert!(
            !mgr.sessions.lock().await.contains_key("capability-a"),
            "the worker is gone, not merely reported as gone"
        );
    }

    #[tokio::test]
    async fn delayed_epoch_one_start_abort_cannot_reap_an_epoch_two_successor() {
        use plurx_core::store::SqliteStore;

        let dir = crate::test_tempdir().expect("tempdir");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let mgr = Arc::new(TranscodeManager::new(
            store,
            dir.path().join("manager-work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let mut session = test_session(dir.path().join("session"));
        session.takeover = Some(SessionTakeoverStart {
            provisional_session_id: "provisional-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            origin_base_ms: 0,
            frontier_offset_ms: 0,
            media_sequence: 2_000_000,
            discontinuity_sequence: 1,
            owner_epoch: 2,
        });
        mgr.sessions
            .lock()
            .await
            .insert("capability-a".into(), Arc::new(session));

        assert!(
            !mgr.stop_session_for_owner("incarnation-a", "capability-a", 1, "delayed start abort",)
                .await,
            "an epoch-one cleanup cannot match the successor generation"
        );
        assert!(
            !mgr.stop_vod_session_for_request(
                "incarnation-a",
                "capability-a",
                "delayed start abort",
            )
            .await,
            "a missing exact request mapping cannot fall through to VOD cleanup"
        );
        assert!(
            mgr.sessions.lock().await.contains_key("capability-a"),
            "both delayed epoch-one paths leave the epoch-two worker alive"
        );
        assert!(
            mgr.stop_session_for_owner("incarnation-a", "capability-a", 2, "test cleanup",)
                .await,
            "the exact successor epoch remains independently stoppable"
        );
    }

    #[test]
    fn bitrate_ladder() {
        assert_eq!(bitrate_for_height(2160), 20_000);
        assert_eq!(bitrate_for_height(1080), 8_000);
        assert_eq!(bitrate_for_height(720), 4_000);
        assert_eq!(bitrate_for_height(240), 1_200);
    }

    #[test]
    fn durable_quality_distinguishes_unset_default_from_corruption() {
        assert_eq!(
            normalize_rate_control_request(None, None),
            (None, None, false)
        );
        assert_eq!(
            normalize_rate_control_request(Some("  "), None),
            (None, None, false)
        );
        assert_eq!(
            normalize_rate_control_request(Some("quality"), None),
            (Some(RateMode::Quality), None, false)
        );
        assert_eq!(
            normalize_rate_control_request(Some("quality"), Some("  ")),
            (Some(RateMode::Quality), None, false)
        );
        assert_eq!(
            normalize_rate_control_request(Some("quality"), Some("22")),
            (Some(RateMode::Quality), Some(22), false)
        );
        for corrupt in ["256", "garbage", "-1"] {
            assert_eq!(
                normalize_rate_control_request(Some("quality"), Some(corrupt)),
                (Some(RateMode::Bitrate), None, true),
                "{corrupt} must fail the pair closed rather than aliasing the family default"
            );
        }
        assert_eq!(
            normalize_rate_control_request(Some("cq"), Some("22")),
            (Some(RateMode::Bitrate), None, true)
        );
    }

    #[tokio::test]
    async fn boot_fails_a_corrupt_quality_pair_closed_to_legacy_vbr() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        store
            .put_settings(&[
                (keys::TRANSCODE_RATE_MODE, "quality"),
                (keys::TRANSCODE_QUALITY, "256"),
            ])
            .await
            .expect("corrupt durable pair");
        let (mgr, _work, _cache) = cached_manager(&store);

        mgr.initialize_rate_control().await.expect("boot fallback");
        assert_eq!(
            mgr.rate_control_snapshot().requested_mode,
            Some(RateMode::Bitrate)
        );
        assert_eq!(
            mgr.effective_rate_control(Encoder::Software),
            EffectiveRateControl::Vbr
        );
    }

    #[test]
    fn requested_quality_resolves_per_family_and_fails_closed() {
        let mut supported = QualityRc::default();
        supported.set_supported(Encoder::Qsv, true);
        let snapshot = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: Some(21),
            quality_rc: supported,
        };
        assert_eq!(
            snapshot.effective_for(Encoder::Qsv),
            EffectiveRateControl::Qvbr { quality: 21 }
        );
        assert_eq!(
            snapshot.effective_for(Encoder::Software),
            EffectiveRateControl::Vbr,
            "a requested value cannot outrun this family's probe verdict"
        );

        let defaults = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: None,
            quality_rc: supported,
        };
        assert_eq!(
            defaults.effective_for(Encoder::Qsv),
            EffectiveRateControl::Qvbr {
                quality: Encoder::Qsv.default_quality()
            }
        );

        let mut cross_family = supported;
        cross_family.set_supported(Encoder::Software, true);
        cross_family.set_supported(Encoder::VideoToolbox, true);
        let defaults = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: None,
            quality_rc: cross_family,
        };
        assert_eq!(
            defaults.effective_for(Encoder::VideoToolbox),
            EffectiveRateControl::Qvbr { quality: 65 }
        );
        assert_eq!(
            defaults.effective_for(Encoder::Software),
            EffectiveRateControl::Qvbr { quality: 23 },
            "a runtime hardware fallback must re-resolve the destination family's default"
        );
        assert_eq!(
            RateControlSnapshot::bitrate(supported).effective_for(Encoder::Qsv),
            EffectiveRateControl::Vbr
        );

        let mut every_family_supported = QualityRc::default();
        for encoder in [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ] {
            every_family_supported.set_supported(encoder, true);
        }
        let unset = RateControlSnapshot {
            requested_mode: None,
            requested_quality: None,
            quality_rc: every_family_supported,
        };
        let explicit_bitrate = RateControlSnapshot::bitrate(every_family_supported);
        for encoder in [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Qsv,
            Encoder::Vaapi,
            Encoder::VideoToolbox,
        ] {
            assert_eq!(unset.effective_for(encoder), EffectiveRateControl::Vbr);
            assert_eq!(
                explicit_bitrate.effective_for(encoder),
                EffectiveRateControl::Vbr
            );
        }
    }

    #[tokio::test]
    async fn effective_qvbr_identity_matches_live_speculative_and_offline_paths() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let encoder = Encoder::Software;
        let mut supported = QualityRc::default();
        supported.set_supported(encoder, true);
        let captured = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: Some(21),
            quality_rc: supported,
        };

        // Deliberately publish a different current value. Each path below
        // must apply its captured/durable q21 after the base builder reads q29;
        // otherwise the full-options equality fails before hashes are compared.
        *mgr.rate_control
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: Some(29),
            quality_rc: supported,
        };
        assert_eq!(
            mgr.effective_rate_control(encoder),
            EffectiveRateControl::Qvbr { quality: 29 }
        );

        let Tracks {
            audio_index,
            subtitle_burn,
        } = mgr.select_tracks(&file, None, None, false).await;
        let live = mgr.live_lookup_options(
            captured,
            encoder,
            &file,
            720,
            0.0,
            audio_index,
            subtitle_burn.clone(),
            None,
            OutputGrade::Sdr,
        );
        let speculative = mgr.speculative_producer_options(
            captured,
            encoder,
            &file,
            720,
            audio_index,
            subtitle_burn,
        );
        let offline_spec = OfflineSpec {
            target_height: 720,
            audio_index,
            subtitle: OfflineSubtitle::None,
            effective_rate_control: EffectiveRateControl::Qvbr { quality: 21 },
        };
        let offline = mgr.offline_package_options(encoder, &file, &offline_spec, None);

        assert_eq!(
            live.effective_rate_control,
            EffectiveRateControl::Qvbr { quality: 21 }
        );
        assert_eq!(
            live, speculative,
            "producer normalization drifted from live"
        );
        assert_eq!(live, offline, "offline normalization drifted from live");

        let expected = recipe_hash_for_options(&mgr, &file, &live, encoder).await;
        assert_eq!(
            recipe_hash_for_options(&mgr, &file, &speculative, encoder).await,
            expected
        );
        assert_eq!(
            recipe_hash_for_options(&mgr, &file, &offline, encoder).await,
            expected
        );

        // Mutation sentinels: each path's rate-control input independently
        // changes both its normalized options and its content identity. These
        // make a forgotten overwrite or a hard-coded shared value observable.
        let live_vbr = mgr.live_lookup_options(
            RateControlSnapshot::bitrate(supported),
            encoder,
            &file,
            720,
            0.0,
            audio_index,
            None,
            None,
            OutputGrade::Sdr,
        );
        assert_ne!(live_vbr, live);
        assert_ne!(
            recipe_hash_for_options(&mgr, &file, &live_vbr, encoder).await,
            expected
        );

        let speculative_q22 = mgr.speculative_producer_options(
            RateControlSnapshot {
                requested_mode: Some(RateMode::Quality),
                requested_quality: Some(22),
                quality_rc: supported,
            },
            encoder,
            &file,
            720,
            audio_index,
            None,
        );
        assert_ne!(speculative_q22, speculative);
        assert_ne!(
            recipe_hash_for_options(&mgr, &file, &speculative_q22, encoder).await,
            expected
        );

        let offline_q23 = OfflineSpec {
            effective_rate_control: EffectiveRateControl::Qvbr { quality: 23 },
            ..offline_spec
        };
        let offline_q23 = mgr.offline_package_options(encoder, &file, &offline_q23, None);
        assert_ne!(offline_q23, offline);
        assert_ne!(
            recipe_hash_for_options(&mgr, &file, &offline_q23, encoder).await,
            expected
        );
    }

    #[tokio::test]
    async fn offline_identity_uses_durable_snapshot_across_hot_change() {
        use plurx_core::domain::{NewOfflinePackage, OfflineCreateOutcome};
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let user = store.create_user("paul", "hash", true).await.expect("user");
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let mut supported = QualityRc::default();
        supported.set_supported(Encoder::Software, true);
        let package_id = "offline-vbr-snapshot";
        let requested = NewOfflinePackage {
            id: package_id.to_owned(),
            request_id: "offline-vbr-snapshot-request".to_owned(),
            user_id: user.id,
            file_id,
            node_id: NODE.to_owned(),
            source_path: file.path.to_string_lossy().into_owned(),
            source_size: file.size,
            source_mtime: file.mtime,
            effective_rate_control: "qvbr:21".to_owned(),
            target_height: 720,
            output_width: Some(1280),
            output_height: Some(720),
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
            .expect("claim")
            .expect("queued package");

        let set_quality = |quality| {
            *mgr.rate_control
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = RateControlSnapshot {
                requested_mode: Some(RateMode::Quality),
                requested_quality: Some(quality),
                quality_rc: supported,
            };
        };
        let spec = OfflineSpec {
            target_height: 720,
            audio_index: None,
            subtitle: OfflineSubtitle::None,
            effective_rate_control: EffectiveRateControl::Qvbr { quality: 21 },
        };
        set_quality(21);
        assert!(matches!(
            mgr.ensure_offline(
                &claimed,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("first offline pass"),
            OfflineProduceOutcome::Yielded
        ));
        let first_hash = store
            .offline_package_for_user(package_id, user.id)
            .await
            .expect("read package")
            .expect("package")
            .recipe_hash
            .expect("pinned recipe");

        assert!(store
            .requeue_offline_package(package_id, NODE, claimed.claim_generation)
            .await
            .expect("requeue"));
        let resumed = store
            .claim_next_offline_package(NODE)
            .await
            .expect("claim again")
            .expect("queued package");
        set_quality(29);
        assert!(matches!(
            mgr.ensure_offline(
                &resumed,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("resumed offline pass after hot setting change"),
            OfflineProduceOutcome::Yielded
        ));
        assert_eq!(
            store
                .offline_package_for_user(package_id, user.id)
                .await
                .expect("read resumed package")
                .expect("package")
                .recipe_hash
                .as_deref(),
            Some(first_hash.as_str()),
            "ensure_offline must keep the package's legacy identity across a hot quality change"
        );
    }

    #[tokio::test]
    async fn an_offline_decode_fault_persists_one_alternate_across_restart() {
        use plurx_core::domain::{NewOfflinePackage, OfflineCreateOutcome};
        use plurx_core::store::{keys, SqliteStore};
        use plurx_core::transcode::ArtifactQualification;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let user = store.create_user("paul", "hash", true).await.expect("user");
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        store
            .put_setting(keys::HWACCEL, "videotoolbox")
            .await
            .expect("select the hardware encoder");
        let (mut mgr, _work, _cache) = cached_manager(&store);
        mgr.caps.videotoolbox = true;
        mgr.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
        mgr.set_automatic_decoder_recovery(true);
        mgr.test_script_offline_production([
            OfflineProduceOutcome::HealthRefused,
            OfflineProduceOutcome::Yielded,
            OfflineProduceOutcome::HealthRefused,
        ]);

        let package_id = "offline-one-shot-decode-recovery";
        let requested = NewOfflinePackage {
            id: package_id.to_owned(),
            request_id: "offline-one-shot-decode-recovery-request".to_owned(),
            user_id: user.id,
            file_id,
            node_id: NODE.to_owned(),
            source_path: file.path.to_string_lossy().into_owned(),
            source_size: file.size,
            source_mtime: file.mtime,
            effective_rate_control: "vbr".to_owned(),
            target_height: 720,
            output_width: Some(1280),
            output_height: Some(720),
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
            .expect("claim")
            .expect("queued package");
        let spec = OfflineSpec {
            target_height: 720,
            audio_index: None,
            subtitle: OfflineSubtitle::None,
            effective_rate_control: EffectiveRateControl::Vbr,
        };

        assert!(matches!(
            mgr.ensure_offline(
                &claimed,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("first offline pass"),
            OfflineProduceOutcome::Yielded
        ));
        let attempts = mgr.test_offline_produced_recipes();
        assert_eq!(attempts.len(), 2, "one refused primary gets one alternate");
        assert_ne!(
            attempts[0], attempts[1],
            "the software-decode recovery must publish under a distinct artifact identity"
        );
        let primary_hash = attempts[0].clone();
        let alternate_hash = store
            .offline_package_for_user(package_id, user.id)
            .await
            .expect("read recovered package")
            .expect("recovered package")
            .recipe_hash
            .expect("durable alternate result reference");
        assert_eq!(alternate_hash, attempts[1]);

        assert_eq!(
            store
                .reset_interrupted_offline_packages(NODE)
                .await
                .expect("reset interrupted package"),
            1
        );
        let resumed = store
            .claim_next_offline_package(NODE)
            .await
            .expect("reclaim")
            .expect("recovered package");
        assert_eq!(
            resumed.recipe_hash.as_deref(),
            Some(alternate_hash.as_str())
        );
        assert!(matches!(
            mgr.ensure_offline(
                &resumed,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("resumed offline pass"),
            OfflineProduceOutcome::HealthRefused
        ));
        assert_eq!(
            mgr.test_offline_produced_recipes(),
            vec![
                primary_hash.clone(),
                alternate_hash.clone(),
                alternate_hash.clone()
            ],
            "restart must retry the consumed alternate once and must not mint another recovery"
        );
        assert_eq!(
            store
                .offline_package_for_user(package_id, user.id)
                .await
                .expect("read resumed package")
                .expect("resumed package")
                .recipe_hash
                .as_deref(),
            Some(alternate_hash.as_str()),
            "a failed alternate cannot move the durable result reference again"
        );
        assert!(store
            .fail_offline_package(
                package_id,
                NODE,
                resumed.claim_generation,
                "transcoding",
                "decode_unhealthy",
                "settle the first fixture",
            )
            .await
            .expect("settle first fixture"));

        // A store outage at the fault boundary cannot authorize either a
        // primary retry or an alternate. The coordinator receives a terminal
        // refusal and will settle the package as decode_unhealthy.
        let begin_error_id = "offline-recovery-begin-store-error";
        let mut begin_error_request = requested.clone();
        begin_error_request.id = begin_error_id.to_owned();
        begin_error_request.request_id = "offline-recovery-begin-store-error-request".to_owned();
        assert!(matches!(
            store
                .create_offline_package(&begin_error_request, 10, 10_000_000, 20_000_000)
                .await
                .expect("create begin-error fixture"),
            OfflineCreateOutcome::Created(_)
        ));
        let begin_error_claim = store
            .claim_next_offline_package(NODE)
            .await
            .expect("claim begin-error fixture")
            .expect("begin-error fixture");
        mgr.test_script_offline_production([OfflineProduceOutcome::HealthRefused]);
        mgr.test_fail_next_offline_recovery_begin();
        assert!(matches!(
            mgr.ensure_offline(
                &begin_error_claim,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("recovery-begin store failure"),
            OfflineProduceOutcome::HealthRefused
        ));
        let begin_error_stored = store
            .offline_package_for_user(begin_error_id, user.id)
            .await
            .expect("read begin-error package")
            .expect("begin-error package");
        assert_eq!(begin_error_stored.decoder_recovery_state, "primary");
        assert_eq!(begin_error_stored.alternate_recipe_hash, None);
        assert!(store
            .fail_offline_package(
                begin_error_id,
                NODE,
                begin_error_claim.claim_generation,
                "transcoding",
                "decode_unhealthy",
                "settle recovery-begin store error fixture",
            )
            .await
            .expect("settle begin-error fixture"));

        let pending_id = "offline-recovery-pending-crash";
        let mut pending_request = requested.clone();
        pending_request.id = pending_id.to_owned();
        pending_request.request_id = "offline-recovery-pending-crash-request".to_owned();
        assert!(matches!(
            store
                .create_offline_package(&pending_request, 10, 10_000_000, 20_000_000)
                .await
                .expect("create pending fixture"),
            OfflineCreateOutcome::Created(_)
        ));
        let pending_claim = store
            .claim_next_offline_package(NODE)
            .await
            .expect("claim pending fixture")
            .expect("pending fixture");
        assert!(store
            .set_offline_package_recipe(
                pending_id,
                NODE,
                pending_claim.claim_generation,
                &primary_hash,
            )
            .await
            .expect("bind primary before terminal fault"));
        assert!(store
            .begin_offline_decode_recovery(
                pending_id,
                NODE,
                pending_claim.claim_generation,
                &primary_hash,
            )
            .await
            .expect("consume recovery before simulated crash"));
        assert_eq!(
            store
                .reset_interrupted_offline_packages(NODE)
                .await
                .expect("restart pending fixture"),
            1
        );
        let pending_resumed = store
            .claim_next_offline_package(NODE)
            .await
            .expect("reclaim pending fixture")
            .expect("pending fixture after restart");
        assert_eq!(pending_resumed.decoder_recovery_state, "recovery_pending");
        assert_eq!(pending_resumed.recipe_hash, None);
        mgr.test_script_offline_production([OfflineProduceOutcome::Yielded]);
        assert!(matches!(
            mgr.ensure_offline(
                &pending_resumed,
                &file,
                &spec,
                Instant::now(),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("resume from pending recovery"),
            OfflineProduceOutcome::Yielded
        ));
        assert_eq!(
            mgr.test_offline_produced_recipes().last(),
            Some(&alternate_hash),
            "restart at the fault-before-planning boundary must run only the frozen alternate"
        );
        let pending_installed = store
            .offline_package_for_user(pending_id, user.id)
            .await
            .expect("read pending recovery")
            .expect("pending recovery package");
        assert_eq!(pending_installed.decoder_recovery_state, "alternate");
        assert_eq!(
            pending_installed.alternate_recipe_hash.as_deref(),
            Some(alternate_hash.as_str())
        );

        // The real three-voter membership harness proves the durable
        // alternate -> rehome_pending transition. Exercise the other half
        // here with the production coordinator: the new owner has no hardware
        // decoder, so its ordinary qualified plan is already software and the
        // generic restricted-plan comparison would otherwise return None.
        assert!(store
            .fail_offline_package(
                pending_id,
                NODE,
                pending_resumed.claim_generation,
                "transcoding",
                "fixture_complete",
                "settle the pending-crash fixture",
            )
            .await
            .expect("settle pending-crash fixture"));
        let survivor_node = "software-survivor";
        let rehome_id = "offline-recovery-software-survivor";
        let mut rehome_request = requested.clone();
        rehome_request.id = rehome_id.to_owned();
        rehome_request.request_id = "offline-recovery-software-survivor-request".to_owned();
        rehome_request.node_id = survivor_node.to_owned();
        assert!(matches!(
            store
                .create_offline_package(&rehome_request, 10, 10_000_000, 20_000_000)
                .await
                .expect("create survivor fixture"),
            OfflineCreateOutcome::Created(_)
        ));
        let survivor_claim = store
            .claim_next_offline_package(survivor_node)
            .await
            .expect("claim survivor fixture")
            .expect("survivor fixture");
        assert!(store
            .set_offline_package_recipe(
                rehome_id,
                survivor_node,
                survivor_claim.claim_generation,
                &primary_hash,
            )
            .await
            .expect("bind departed owner's primary identity"));
        assert!(store
            .begin_offline_decode_recovery(
                rehome_id,
                survivor_node,
                survivor_claim.claim_generation,
                &primary_hash,
            )
            .await
            .expect("preserve consumed origin budget"));
        let mut rehomed = store
            .offline_package_for_user(rehome_id, user.id)
            .await
            .expect("read rehomed package")
            .expect("rehomed package");
        rehomed.decoder_recovery_state = "rehome_pending".to_owned();

        let survivor_work = crate::test_tempdir().expect("survivor work");
        let survivor_cache = crate::test_tempdir().expect("survivor cache");
        let survivor = TranscodeManager::new(
            Arc::clone(&store),
            survivor_work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_cache(
            survivor_cache.path().to_path_buf(),
            "ffmpeg version software-survivor".into(),
            survivor_node.into(),
        );
        survivor.test_publish_artifact_qualification(ArtifactQualification::HealthQualified);
        survivor.test_script_offline_production([
            OfflineProduceOutcome::Yielded,
            OfflineProduceOutcome::HealthRefused,
            OfflineProduceOutcome::HealthRefused,
        ]);
        assert!(matches!(
            survivor
                .ensure_offline(
                    &rehomed,
                    &file,
                    &spec,
                    Instant::now(),
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
                .expect("software survivor recovery"),
            OfflineProduceOutcome::Yielded
        ));
        let survivor_attempts = survivor.test_offline_produced_recipes();
        assert_eq!(survivor_attempts.len(), 1);
        assert_ne!(survivor_attempts[0], primary_hash);
        assert_ne!(survivor_attempts[0], alternate_hash);
        assert_eq!(
            store
                .reset_interrupted_offline_packages(survivor_node)
                .await
                .expect("restart survivor"),
            1
        );
        let survivor_resumed = store
            .claim_next_offline_package(survivor_node)
            .await
            .expect("reclaim survivor package")
            .expect("survivor package after restart");
        assert_eq!(survivor_resumed.decoder_recovery_state, "alternate");
        assert!(matches!(
            survivor
                .ensure_offline(
                    &survivor_resumed,
                    &file,
                    &spec,
                    Instant::now(),
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
                .expect("resume survivor alternate"),
            OfflineProduceOutcome::HealthRefused
        ));
        assert_eq!(
            survivor.test_offline_produced_recipes(),
            vec![survivor_attempts[0].clone(), survivor_attempts[0].clone()],
            "the survivor must retry only its one bound software alternate"
        );
        assert!(store
            .fail_offline_package(
                rehome_id,
                survivor_node,
                survivor_resumed.claim_generation,
                "transcoding",
                "fixture_complete",
                "settle survivor fixture",
            )
            .await
            .expect("settle survivor fixture"));

        // A genuinely fresh software-primary package has no failed hardware
        // attempt to recover from. Its terminal refusal must therefore remain
        // primary rather than minting a budget that rehome could reinterpret.
        let software_primary_id = "offline-recovery-software-primary";
        let mut software_primary_request = requested.clone();
        software_primary_request.id = software_primary_id.to_owned();
        software_primary_request.request_id =
            "offline-recovery-software-primary-request".to_owned();
        software_primary_request.node_id = survivor_node.to_owned();
        assert!(matches!(
            store
                .create_offline_package(&software_primary_request, 10, 10_000_000, 20_000_000,)
                .await
                .expect("create software primary fixture"),
            OfflineCreateOutcome::Created(_)
        ));
        let software_primary = store
            .claim_next_offline_package(survivor_node)
            .await
            .expect("claim software primary fixture")
            .expect("software primary fixture");
        assert!(matches!(
            survivor
                .ensure_offline(
                    &software_primary,
                    &file,
                    &spec,
                    Instant::now(),
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
                .expect("run software primary fixture"),
            OfflineProduceOutcome::HealthRefused
        ));
        let stored_software_primary = store
            .offline_package_for_user(software_primary_id, user.id)
            .await
            .expect("read software primary fixture")
            .expect("software primary package");
        assert_eq!(stored_software_primary.decoder_recovery_state, "primary");
        assert_eq!(stored_software_primary.alternate_recipe_hash, None);
    }

    #[test]
    fn offline_recovery_state_parsing_is_closed_and_monotone() {
        assert_eq!(
            OfflineRecoveryState::parse("primary").expect("primary"),
            OfflineRecoveryState::Primary
        );
        assert_eq!(
            OfflineRecoveryState::parse("recovery_pending").expect("pending"),
            OfflineRecoveryState::Pending
        );
        assert_eq!(
            OfflineRecoveryState::parse("rehome_pending").expect("rehome pending"),
            OfflineRecoveryState::RehomePending
        );
        assert_eq!(
            OfflineRecoveryState::parse("alternate").expect("alternate"),
            OfflineRecoveryState::Alternate
        );
        assert!(
            OfflineRecoveryState::parse("unknown").is_err(),
            "unknown durable state must fail closed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_refresh_loop_updates_the_hot_path_snapshot() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let (writer, _writer_work, _writer_cache) = cached_manager(&store);
        let (peer, _peer_work, _peer_cache) = cached_manager(&store);
        let peer = Arc::new(peer);
        let mut supported = QualityRc::default();
        supported.set_supported(Encoder::Software, true);
        peer.publish_rate_control(
            RateControlSnapshot {
                requested_mode: Some(RateMode::Quality),
                requested_quality: Some(21),
                quality_rc: supported,
            },
            Encoder::Software,
        );
        assert_eq!(
            peer.effective_rate_control(Encoder::Software),
            EffectiveRateControl::Qvbr { quality: 21 }
        );

        writer
            .apply_rate_control_settings(RateMode::Bitrate, None)
            .await
            .expect("replicated write");
        assert_eq!(
            RATE_CONTROL_REFRESH,
            Duration::from_secs(2),
            "the replicated hot-path snapshot contract is a literal two seconds"
        );
        let refresh = tokio::spawn(Arc::clone(&peer).rate_control_refresh_once());
        // Sleeping yields until the worker has constructed its interval and
        // both tasks reach the real first deadline. Awaiting the one-cycle
        // harness observes the blocking store read rather than guessing how
        // many scheduler yields it needs to finish.
        let started = tokio::time::Instant::now();
        tokio::time::sleep(RATE_CONTROL_REFRESH).await;
        refresh.await.expect("rate-control refresh task");
        assert_eq!(
            tokio::time::Instant::now() - started,
            RATE_CONTROL_REFRESH,
            "the peer snapshot must refresh within the advertised two-second period"
        );
        assert_eq!(
            peer.effective_rate_control(Encoder::Software),
            EffectiveRateControl::Vbr,
            "the two-second refresher must replace the peer's stale snapshot"
        );
    }

    #[tokio::test]
    async fn a_peer_refresh_defers_quality_validation_while_a_viewer_waits() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let (peer, _work, _cache) = cached_manager(&store);
        store
            .put_settings(&[
                (keys::TRANSCODE_RATE_MODE, "quality"),
                (keys::TRANSCODE_QUALITY, "22"),
            ])
            .await
            .expect("replicated quality request");
        let _viewer = peer.admissions.wait_for_slot();

        assert!(
            peer.refresh_rate_control()
                .await
                .expect("refresh")
                .is_none(),
            "the runtime probe must be deferred, not treated as a driver refusal"
        );
        assert_eq!(
            peer.effective_rate_control(Encoder::Software),
            EffectiveRateControl::Vbr,
            "the last validated snapshot remains published while the viewer has priority"
        );
    }

    #[tokio::test]
    async fn an_admin_quality_change_does_not_mutate_settings_while_a_viewer_waits() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let (mgr, _work, _cache) = cached_manager(&store);
        let _viewer = mgr.admissions.wait_for_slot();

        assert!(matches!(
            mgr.apply_rate_control_settings(RateMode::Quality, Some(22))
                .await,
            Err(ApplyRateControlError::Busy)
        ));
        assert_eq!(
            mgr.requested_rate_control().await.expect("requested pair"),
            (None, None),
            "a deferred validation must not durably publish an unvalidated request"
        );
        assert_eq!(
            mgr.effective_rate_control(Encoder::Software),
            EffectiveRateControl::Vbr
        );
    }

    #[tokio::test]
    async fn runtime_quality_validation_never_overlaps_the_background_encoder_lane() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let (mgr, _work, _cache) = cached_manager(&store);
        let _producer = mgr.background_producer.lock().await;

        assert!(matches!(
            mgr.apply_rate_control_settings(RateMode::Quality, Some(22))
                .await,
            Err(ApplyRateControlError::Busy)
        ));
        assert_eq!(
            mgr.requested_rate_control().await.expect("requested pair"),
            (None, None),
            "validation must defer before writing while offline/speculative encoding owns the lane"
        );
    }

    #[tokio::test]
    async fn a_fallback_uses_the_session_generation_not_the_latest_setting() {
        use plurx_core::store::SqliteStore;

        super::require_ffmpeg();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store.get_file(file_id).await.expect("get").expect("file");
        let (mgr, _work, _cache) = cached_manager(&store);
        let mut supported = QualityRc::default();
        supported.set_supported(Encoder::VideoToolbox, true);
        supported.set_supported(Encoder::Software, true);
        let captured = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: None,
            quality_rc: supported,
        };
        let later = RateControlSnapshot {
            requested_mode: Some(RateMode::Quality),
            requested_quality: Some(31),
            quality_rc: supported,
        };
        *mgr.rate_control
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = later;

        let dir = crate::test_tempdir().expect("session dir");
        let session = test_session(dir.path().to_path_buf());
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
        opts.effective_rate_control = captured.effective_for(Encoder::VideoToolbox);
        // The rung is chosen when the retry recipe is frozen, at session
        // start, and that recipe is what the actor later authorizes. So the
        // generation question is asked of the frozen recipe, not of a
        // downgrade helper that production no longer runs.
        let retry = build_test_transcode_retry(
            &mgr,
            &file,
            &opts,
            Encoder::VideoToolbox,
            captured.effective_for(Encoder::Software),
            Pacing::unpaced(),
            dir.path(),
            "generation-test",
        )
        .await
        .expect("a CPU-pipeline hardware attempt has a software rung");
        drop(session);

        assert_eq!(
            retry.encoder,
            Encoder::Software,
            "a CPU-pipeline hardware attempt steps down to software"
        );
        let fallback = EffectiveRateControl::Qvbr { quality: 23 };
        assert!(
            retry.args.iter().any(|arg| arg.contains("23")),
            "the frozen retry must carry the session's captured generation, \
             not the global setting; args were {:?}",
            retry.args
        );
        assert_ne!(
            fallback,
            mgr.effective_rate_control(Encoder::Software),
            "the global q31 setting is reserved for a later session"
        );
    }

    /// The snap normalises menu strays onto the ladder — nearest rung, ties
    /// DOWN, because bandwidth is the scarce thing — and refuses to touch
    /// what is not a stray: anything above the ladder is an Original-class
    /// request carrying a source's own height.
    #[test]
    fn stray_heights_snap_to_the_ladder_and_promises_pass_through() {
        assert_eq!(snap_height(1080), 1080, "a rung is already home");
        assert_eq!(snap_height(540), 480, "nearest");
        assert_eq!(snap_height(600), 480, "equidistant resolves DOWN");
        assert_eq!(snap_height(900), 720, "equidistant resolves DOWN");
        assert_eq!(
            snap_height(144),
            360,
            "below the ladder climbs to its floor"
        );
        assert_eq!(
            snap_height(1440),
            1440,
            "above the ladder is a promise, not a stray"
        );
        assert_eq!(snap_height(2160), 2160);
    }

    /// The advertised ladder follows the source down and never offers an
    /// upscale; its totals are the wire cost the controller reasons about,
    /// and its peaks are what the rate-control window may actually spend.
    #[test]
    fn the_ladder_is_filtered_by_the_source_and_priced_both_ways() {
        let full = ladder(Some(2160));
        assert_eq!(
            full.iter().map(|r| r.height).collect::<Vec<_>>(),
            vec![1080, 720, 480, 360],
            "the shared lower ladder does not promise unresolved 4K output"
        );
        let top = full[0];
        assert_eq!(top.total_kbps, 8_160, "8 Mb/s video + 160 kb/s audio");
        assert_eq!(top.peak_kbps, 12_160, "the -maxrate window bound + audio");

        assert_eq!(
            ladder(Some(720))
                .iter()
                .map(|r| r.height)
                .collect::<Vec<_>>(),
            vec![720, 480, 360],
            "a 720p file offers 720p and below — never an upscale"
        );
        assert!(
            ladder(Some(240)).is_empty(),
            "nothing to offer below the floor"
        );
        assert_eq!(
            ladder(None).len(),
            4,
            "an unprobed source has nothing to filter by"
        );
        assert_eq!(ladder(Some(0)).len(), 4, "0 is not a height");

        let live_four_k = advertised_ladder(Some(2160), MAX_HEIGHT);
        assert_eq!(
            live_four_k
                .iter()
                .map(|rung| rung.height)
                .collect::<Vec<_>>(),
            vec![2160, 1080, 720, 480, 360],
            "a resolved live hardware session may retain 4K"
        );
        assert_eq!(live_four_k[0].total_kbps, 20_160);
        assert_eq!(live_four_k[0].peak_kbps, 30_160);
        assert_eq!(
            advertised_ladder(Some(1440), MAX_HEIGHT)
                .first()
                .map(|rung| rung.height),
            Some(1440),
            "a nonstandard source-preserving height must match Auto too"
        );
        assert_eq!(
            advertised_ladder(Some(2160), AUTO_HARDWARE_PROBED_HEIGHT)
                .iter()
                .map(|rung| rung.height)
                .collect::<Vec<_>>(),
            vec![1080, 720, 480, 360],
            "an unresolved or HDR ceiling must not advertise 4K"
        );
    }

    /// An advertised rung is a promise. The web ABR controller upgrades into
    /// any rung the ladder lists, so a ladder built from the source height
    /// while Auto was capped by the pipeline produced a self-sustaining
    /// oscillation: start at the 720p Auto answer, measure a JIT delivery
    /// estimate far above the 1080p bar, upgrade on schedule, fail with
    /// `session_failed`, take the emergency downgrade, repeat every ~2
    /// minutes. Observed on a Dolby Vision Profile 5 episode.
    #[tokio::test]
    async fn the_advertised_ladder_never_offers_a_rung_the_pipeline_cannot_serve() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let hardware = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps {
                nvenc: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        );
        let software = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let ordinary = {
            let mut file = profile5_file();
            file.hdr = None;
            file.hdr_format = None;
            file
        };
        let profile5 = profile5_file();

        // A Profile 5 reshape follows whichever encoder it can actually
        // reach. With no ffmpeg here the hardware pairing cannot be proved,
        // so it falls back to software — which is the contract: an unproved
        // pairing is never attempted, and the ceiling tells the truth about
        // the fallback rather than advertising a rung that would fail.
        assert_eq!(
            hardware.capability_height_ceiling(Some(&profile5)).await,
            AUTO_SOFTWARE_HEIGHT,
            "an unproved hardware pairing falls back to the software rung"
        );
        assert_eq!(
            software.capability_height_ceiling(Some(&profile5)).await,
            AUTO_SOFTWARE_HEIGHT
        );
        // A software-only node caps every file the same way.
        assert_eq!(
            software.capability_height_ceiling(Some(&ordinary)).await,
            AUTO_SOFTWARE_HEIGHT
        );
        // Hardware on an ordinary 4K source keeps the full ladder.
        assert_eq!(
            hardware.capability_height_ceiling(Some(&ordinary)).await,
            MAX_HEIGHT
        );
        let mut hdr10 = ordinary.clone();
        hdr10.hdr = Some("hdr10".into());
        assert_eq!(
            hardware.capability_height_ceiling(Some(&hdr10)).await,
            AUTO_HARDWARE_PROBED_HEIGHT,
            "the production probe proves a 1080p HDR output, not 4K"
        );
        assert_eq!(
            hardware.capability_height_ceiling(None).await,
            AUTO_HARDWARE_PROBED_HEIGHT
        );

        // The ceiling and Auto must agree, or the ladder is lying again.
        for (manager, file) in [
            (&hardware, &profile5),
            (&software, &profile5),
            (&software, &ordinary),
            (&hardware, &ordinary),
        ] {
            let ceiling = manager.capability_height_ceiling(Some(file)).await;
            let auto = manager.auto_height_for_file(Some(file), None).await;
            assert!(
                auto <= ceiling,
                "Auto {auto} exceeds the advertised ceiling {ceiling}"
            );
            let rungs = advertised_ladder(file.height, ceiling);
            assert!(
                rungs.iter().all(|rung| rung.height <= ceiling),
                "ladder {rungs:?} offers a rung above {ceiling}"
            );
            assert!(
                rungs.iter().any(|rung| rung.height == auto),
                "the rung Auto actually picked ({auto}) must be on the ladder: {rungs:?}"
            );
        }

        // The prior must not narrow the menu: a link that has recovered has to
        // be able to reach the cap again.
        let starved = plurx_core::domain::NetworkPrior {
            worst_rung_height: Some(720),
            starved_at_ms: Some(unix_ms()),
            ..Default::default()
        };
        assert_eq!(
            hardware.capability_height_ceiling(Some(&ordinary)).await,
            MAX_HEIGHT,
            "the capability ceiling ignores network priors by construction"
        );
        assert!(
            hardware
                .auto_height_for_file(Some(&ordinary), Some(&starved))
                .await
                < MAX_HEIGHT,
            "…while Auto still honours them"
        );
    }

    /// Auto is a policy about the encoder, not about the file.
    #[tokio::test]
    async fn auto_follows_the_source_only_when_hardware_can_carry_it() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let hardware = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps {
                nvenc: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        );
        let software = TranscodeManager::new(
            Arc::clone(&store),
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );

        // Software keeps the conservative ceiling whatever the source is: a
        // 1080p x264 session on a NUC cannot hold realtime, and a stream that
        // stutters at 1080p is worse than one that plays at 720p.
        for source in [Some(2160), Some(1080), None] {
            assert_eq!(
                software.auto_height(source, None).await,
                720,
                "source {source:?}"
            );
        }
        // …and never advertises a rung above the source: the filter already
        // refused to upscale, but the bitrate and the response metadata
        // described 720p for a 480p stream (§3.1).
        assert_eq!(
            software.auto_height(Some(480), None).await,
            480,
            "never upscales"
        );

        // Hardware follows a known SDR source. No network prior means there is
        // no evidence for a resolution downgrade.
        assert_eq!(
            hardware.auto_height(Some(2160), None).await,
            2160,
            "codec incompatibility alone must not discard 4K"
        );
        assert_eq!(hardware.auto_height(Some(1080), None).await, 1080);
        assert_eq!(
            hardware.auto_height(Some(480), None).await,
            480,
            "never upscales"
        );
        // An unprobed file has no height to follow; 1080 is the useful guess,
        // and the encoder being asked is the one that can carry it.
        assert_eq!(hardware.auto_height(None, None).await, 1080);
        assert_eq!(
            hardware.auto_height(Some(0), None).await,
            AUTO_HARDWARE_PROBED_HEIGHT,
            "0 is not a height"
        );
    }

    #[tokio::test]
    async fn hdr10_auto_starts_at_the_highest_qsv_rung_proved_at_boot() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let manager = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps {
                qsv: true,
                ..Default::default()
            },
            Pipeline::VppQsv,
        )
        .with_dovi_passthrough(true)
        .with_dovi_passthrough_qsv(true);
        let file = profile5_file();

        assert_eq!(
            manager
                .capability_height_ceiling_for_request(Some(&file), true)
                .await,
            HDR10_4K_HEIGHT
        );
        assert_eq!(
            manager
                .auto_height_for_request(Some(&file), None, true)
                .await,
            HDR10_4K_HEIGHT
        );
        assert_eq!(
            manager
                .capability_height_ceiling_for_request(Some(&file), false)
                .await,
            AUTO_SOFTWARE_HEIGHT,
            "without an HDR10 request the unproved SDR reshape remains conservative"
        );
    }

    /// Breaking Bad S01E01's production shape: AV1 forces a video re-encode
    /// on a client that does not claim AV1, but its SDR picture and resolution
    /// are independent facts. QuickSync should retain 2160p until measured
    /// link history says otherwise; HDR stays at the separately-probed output.
    #[tokio::test]
    async fn hardware_auto_preserves_a_four_k_av1_sdr_source() {
        use plurx_core::store::SqliteStore;
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work");
        let hardware = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps {
                qsv: true,
                ..Default::default()
            },
            Pipeline::VppQsv,
        );
        let mut file = profile5_file();
        file.video_codec = Some("av1".into());
        file.hdr = None;
        file.hdr_format = None;

        assert_eq!(
            hardware.auto_height_for_file(Some(&file), None).await,
            2160,
            "no link evidence means preserve the known SDR source geometry"
        );
        assert_eq!(
            advertised_ladder(
                file.height,
                hardware.capability_height_ceiling(Some(&file)).await,
            )
            .first()
            .map(|rung| rung.height),
            Some(2160),
            "the session must advertise the rung Auto actually selected"
        );

        let prior = plurx_core::domain::NetworkPrior {
            sustained_kbps: Some(20_000),
            ..Default::default()
        };
        assert_eq!(
            hardware
                .auto_height_for_file(Some(&file), Some(&prior))
                .await,
            1080,
            "the 4K peak is 30.16 Mb/s, so real link evidence may step down"
        );

        file.hdr = Some("hdr10".into());
        assert_eq!(
            hardware.auto_height_for_file(Some(&file), None).await,
            1080,
            "the boot probe has not proved a 4K HDR output"
        );
    }

    #[test]
    fn network_prior_adjusts_only_the_auto_starting_choice() {
        use plurx_core::domain::{NetworkPrior, NETWORK_PRIOR_STARVED_TTL_MS};

        // A fixed "now" well past the TTL, so a case can place its starvation
        // either inside or outside the horizon without touching a real clock.
        const NOW_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
        let fresh_ms = NOW_MS - NETWORK_PRIOR_STARVED_TTL_MS + 1;
        let expired_ms = NOW_MS - NETWORK_PRIOR_STARVED_TTL_MS - 1;

        let prior = |sustained_kbps, worst_rung_height: Option<i64>| NetworkPrior {
            sustained_kbps,
            worst_rung_height,
            starved_at_ms: worst_rung_height.map(|_| fresh_ms),
            ..NetworkPrior::default()
        };
        let aged = |sustained_kbps, worst_rung_height: Option<i64>| NetworkPrior {
            starved_at_ms: worst_rung_height.map(|_| expired_ms),
            ..prior(sustained_kbps, worst_rung_height)
        };
        let cases = [
            (1080, None, 1080, "absent prior is today's exact choice"),
            (
                1080,
                Some(prior(Some(20_000), None)),
                1080,
                "proven-fast link starts at the cap",
            ),
            (
                1080,
                Some(prior(Some(7_000), None)),
                720,
                "EWMA must cover the selected rung's peak",
            ),
            (
                1080,
                Some(prior(Some(20_000), Some(1080))),
                720,
                "a starved cap starts one rung below it",
            ),
            (
                1080,
                Some(prior(Some(20_000), Some(720))),
                480,
                "the lowest starved rung is the binding verdict",
            ),
            (
                480,
                Some(prior(Some(20_000), Some(1080))),
                480,
                "history never upscales a smaller source",
            ),
            // The two halves of the recovery rule (review finding 2). Without
            // them one transient stall pins a tuple one rung down for the row's
            // whole life, and the EWMA is dead code in both directions.
            (
                1080,
                Some(aged(Some(20_000), Some(1080))),
                1080,
                "an aged-out starvation stops binding, so a proven-fast link reaches the cap",
            ),
            (
                1080,
                Some(prior(Some(2_000), Some(1080))),
                360,
                "a fresh starved cap still lets a collapsed EWMA correct further down",
            ),
            (
                1080,
                Some(aged(Some(2_000), Some(1080))),
                360,
                "an aged-out verdict leaves the EWMA deciding alone",
            ),
            (
                1080,
                Some(NetworkPrior {
                    worst_rung_height: Some(1080),
                    starved_at_ms: None,
                    ..NetworkPrior::default()
                }),
                1080,
                "a verdict with no stamp is treated as expired, not permanent",
            ),
        ];
        for (current, prior, expected, reason) in cases {
            assert_eq!(
                auto_height_from_prior(current, Some(current), prior.as_ref(), NOW_MS),
                expected,
                "{reason}"
            );
        }
    }

    /// The progress stream is the only thing that can tell slow from stuck, so
    /// what it refuses to believe matters as much as what it parses.
    #[test]
    fn progress_lines_parse_and_reject() {
        let p = Progress::new();
        let gen = p.begin_attempt();
        assert_eq!(
            p.out_time_ms(),
            None,
            "nothing known before the first block"
        );
        assert_eq!(p.speed(), None);

        // `N/A` arrives before the first frame lands, and for `speed` on a
        // session too young to estimate one. It is not zero: zero out_time
        // reads as "produced nothing" and zero speed reads as "stalled".
        for line in ["out_time_us=N/A", "speed=N/A", "bitrate=N/A", "frame=0"] {
            apply_progress_line(&p, gen, line);
        }
        assert_eq!(p.out_time_ms(), None);
        assert_eq!(p.speed(), None);

        // Microseconds in, milliseconds out — `out_time_ms` is misnamed by
        // ffmpeg and also carries microseconds, so both spellings mean the same.
        apply_progress_line(&p, gen, "out_time_us=5960000");
        assert_eq!(p.out_time_ms(), Some(5960));
        apply_progress_line(&p, gen, "out_time_ms=8000000");
        assert_eq!(p.out_time_ms(), Some(8000));

        apply_progress_line(&p, gen, "speed=46.2x");
        assert_eq!(p.speed(), Some(46.2));
        apply_progress_line(&p, gen, "speed=0.71x");
        assert_eq!(p.speed(), Some(0.71));

        // Junk and unknown keys change nothing.
        for line in ["", "no-equals-sign", "speed=", "out_time_us=abc", "fps=25"] {
            apply_progress_line(&p, gen, line);
        }
        assert_eq!(p.out_time_ms(), Some(8000));
        assert_eq!(p.speed(), Some(0.71));

        // A respawn (the hardware→software fallback) starts a fresh timeline;
        // carrying the dead process's numbers would read as an instant stall.
        p.begin_attempt();
        assert_eq!(p.out_time_ms(), None);
        assert_eq!(p.speed(), None);
    }

    /// Staleness must measure the last time output *moved*, not the last block
    /// received: a wedged ffmpeg keeps emitting progress with a frozen time.
    #[test]
    fn stall_is_measured_from_movement_not_from_chatter() {
        let p = Progress::new();
        let gen = p.begin_attempt();
        apply_progress_line(&p, gen, "out_time_us=1000000");
        let after_move = p.stalled_for();
        // Re-reporting the same timestamp is not progress.
        for _ in 0..5 {
            apply_progress_line(&p, gen, "out_time_us=1000000");
            apply_progress_line(&p, gen, "speed=0.9x");
        }
        assert!(
            p.stalled_for() >= after_move,
            "repeating a timestamp must not reset the stall clock"
        );
        // Moving forward does reset it.
        std::thread::sleep(Duration::from_millis(5));
        let before = p.stalled_for();
        apply_progress_line(&p, gen, "out_time_us=2000000");
        assert!(p.stalled_for() <= before);
    }

    /// The race this closes: a killed attempt's stdout reader is still holding
    /// buffered lines when the replacement starts, and those lines describe a
    /// process that got further along. Applied to the new attempt they read as
    /// "produced a lot, then froze" -- a healthy encoder declared dead by its
    /// predecessor.
    #[test]
    fn a_dead_attempt_cannot_speak_for_its_replacement() {
        let p = Progress::new();
        let first = p.begin_attempt();
        apply_progress_line(&p, first, "out_time_us=30000000");
        assert_eq!(p.out_time_ms(), Some(30_000));

        // The fallback fires: new attempt, clean slate.
        let second = p.begin_attempt();
        assert_ne!(first, second);
        assert_eq!(p.out_time_ms(), None, "the replacement starts unmeasured");

        // The dead reader drains its buffer. None of it lands.
        for line in ["out_time_us=30000000", "speed=4.0x", "out_time_us=31000000"] {
            apply_progress_line(&p, first, line);
        }
        assert_eq!(p.out_time_ms(), None, "stale attempt was ignored");
        assert_eq!(p.speed(), None);

        // The live attempt is believed.
        apply_progress_line(&p, second, "out_time_us=2000000");
        assert_eq!(p.out_time_ms(), Some(2_000));
    }

    #[tokio::test]
    async fn process_supervisor_reports_current_exit_and_fences_predecessor_exit() {
        let control = crate::playback_control::RollingControlHandle::spawn("supervisor-test");
        let current_attempt = control
            .begin_producer_attempt()
            .await
            .expect("current attempt");
        let child = tokio::process::Command::new("false")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn exiting producer");
        let mut current = AttemptChild::new(current_attempt, child, control.clone(), None);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if current
                    .try_wait()
                    .expect("current producer status")
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("supervisor observes current exit without a watchdog poll");
        let current_delivery = control.snapshot().await.expect("actor snapshot").delivery;
        assert_eq!(current_delivery.producer_attempt, current_attempt);
        assert_eq!(
            current_delivery
                .producer_exit
                .as_ref()
                .map(|exit| (exit.success, exit.code)),
            Some((false, Some(1)))
        );

        let predecessor_attempt = control
            .begin_producer_attempt()
            .await
            .expect("predecessor attempt");
        let child = tokio::process::Command::new("sh")
            .args(["-c", "sleep 0.05; exit 7"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn delayed predecessor");
        let mut predecessor = AttemptChild::new(predecessor_attempt, child, control.clone(), None);
        let successor_attempt = control
            .begin_producer_attempt()
            .await
            .expect("successor attempt");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if predecessor
                    .try_wait()
                    .expect("predecessor status")
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("supervisor observes delayed predecessor exit");
        let successor_delivery = control.snapshot().await.expect("actor snapshot").delivery;
        assert_eq!(successor_delivery.producer_attempt, successor_attempt);
        assert_eq!(
            successor_delivery.producer_exit, None,
            "a reaped predecessor cannot terminate its already-admitted successor"
        );
    }

    #[tokio::test]
    async fn process_supervisor_owns_signals_termination_and_reaping() {
        let control = crate::playback_control::RollingControlHandle::spawn("supervisor-signal");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        let mut child = AttemptChild::new(attempt, long_running_child(), control.clone(), None);
        #[cfg(unix)]
        let pid = child.id().expect("running producer pid");

        assert_eq!(
            control.producer_flow_applied_for_test(),
            None,
            "a newly admitted producer has no physical-flow acknowledgement"
        );
        assert!(child
            .signal(crate::process_control::ProcessSignal::Suspend)
            .await
            .expect("supervised stop"));
        assert_eq!(
            control.producer_flow_applied_for_test(),
            Some((attempt, true)),
            "SIGSTOP is acknowledged for this attempt only after the supervisor's successful syscall"
        );
        assert!(child
            .signal(crate::process_control::ProcessSignal::Resume)
            .await
            .expect("supervised continue"));
        assert_eq!(
            control.producer_flow_applied_for_test(),
            Some((attempt, false)),
            "SIGCONT replaces the held acknowledgement with a running acknowledgement"
        );
        child.kill().await.expect("supervised kill waits for reap");
        let status = child
            .try_wait()
            .expect("reaped status")
            .expect("terminal status");
        assert_eq!(child.id(), None, "a reaped owner never exposes its old pid");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(status.signal(), Some(libc::SIGKILL));
            assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        #[cfg(windows)]
        assert!(!status.success(), "TerminateProcess must report failure");
        let exit = control
            .snapshot()
            .await
            .expect("actor snapshot")
            .delivery
            .producer_exit
            .expect("exact attempt exit");
        assert!(!exit.success);
        #[cfg(unix)]
        assert_eq!(exit.signal, Some(libc::SIGKILL));
        #[cfg(windows)]
        assert_eq!(exit.signal, None);
    }

    #[tokio::test]
    async fn deferred_flow_signal_reauthorizes_attempt_before_touching_pid() {
        let control = crate::playback_control::RollingControlHandle::spawn("signal-capacity");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        let mut child = AttemptChild::new(attempt, long_running_child(), control.clone(), None);
        assert!(control.reserve_producer_flow_capacity_for_test());
        assert!(control.reserve_producer_flow_capacity_for_test());

        {
            let signal = child.signal(crate::process_control::ProcessSignal::Suspend);
            tokio::pin!(signal);
            tokio::select! {
                result = signal.as_mut() => panic!("full-capacity signal completed early: {result:?}"),
                _ = control.wait_for_producer_flow_deferral_for_test() => {}
            }
            let successor = control
                .begin_producer_attempt()
                .await
                .expect("successor attempt");
            assert!(successor > attempt);
            control.release_producer_flow_capacity_for_test();
            assert!(
                !signal.await.expect("deferred signal verdict"),
                "the predecessor signal must fail after exact-attempt re-authorization"
            );
            assert_eq!(
                control.producer_flow_applied_for_test(),
                None,
                "a deferred predecessor cannot fabricate a physical acknowledgement"
            );
        }
        control.release_producer_flow_capacity_for_test();
        child.kill().await.expect("reap predecessor child");
    }

    #[tokio::test]
    async fn deferred_flow_signal_wakes_fail_closed_when_control_is_fenced() {
        let control = crate::playback_control::RollingControlHandle::spawn("signal-unavailable");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        let mut child = AttemptChild::new(attempt, long_running_child(), control.clone(), None);
        assert!(control.reserve_producer_flow_capacity_for_test());
        assert!(control.reserve_producer_flow_capacity_for_test());

        {
            let signal = child.signal(crate::process_control::ProcessSignal::Suspend);
            tokio::pin!(signal);
            tokio::select! {
                result = signal.as_mut() => panic!("full-capacity signal completed early: {result:?}"),
                _ = control.wait_for_producer_flow_deferral_for_test() => {}
            }
            control.fence_unavailable();
            assert!(
                !signal.await.expect("fenced deferred signal verdict"),
                "retirement must wake a capacity waiter before PID access"
            );
        }
        control.release_producer_flow_capacity_for_test();
        control.release_producer_flow_capacity_for_test();
        child
            .kill()
            .await
            .expect("cleanup command reaches supervisor");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn actor_task_exit_fences_a_reserved_signal_and_cleanup_still_progresses() {
        let control = crate::playback_control::RollingControlHandle::spawn("signal-actor-exit");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        let child = Arc::new(AttemptChild::new(
            attempt,
            long_running_child(),
            control.clone(),
            None,
        ));
        let pause = Arc::new(std::sync::Barrier::new(2));
        child.pause_signal_after_flow_reservation(Arc::clone(&pause));
        let signal = {
            let child = Arc::clone(&child);
            tokio::spawn(async move {
                child
                    .signal(crate::process_control::ProcessSignal::Suspend)
                    .await
            })
        };

        pause.wait();
        assert!(
            control.producer_transition_guard_is_held_for_test(),
            "the reserved signal owns the transition fence before its syscall"
        );
        control.abort_actor_for_test();
        control.wait_for_actor_exit_fence_for_test().await;
        assert!(
            !control.is_retired(),
            "actor-exit retirement waits behind the already-authorized signal"
        );
        pause.wait();
        assert!(signal
            .await
            .expect("reserved signal task")
            .expect("pre-fence signal verdict"));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !control.is_retired() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor-exit fence publishes retirement");
        let sent = child
            .signal(crate::process_control::ProcessSignal::Resume)
            .await
            .expect("signal verdict");
        assert!(
            !sent,
            "no process signal can linearize after actor-task retirement"
        );

        let mut child = Arc::try_unwrap(child).unwrap_or_else(|_| panic!("sole child owner"));
        child
            .kill()
            .await
            .expect("cleanup command reaches exited actor's process supervisor");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_signal_and_retirement_share_one_authorization_linearization() {
        let control = crate::playback_control::RollingControlHandle::spawn("signal-fence");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        let child = Arc::new(AttemptChild::new(
            attempt,
            long_running_child(),
            control.clone(),
            None,
        ));
        let pause = Arc::new(std::sync::Barrier::new(2));
        child.pause_signal_after_authorization(Arc::clone(&pause));
        let signal = {
            let child = Arc::clone(&child);
            tokio::spawn(async move {
                child
                    .signal(crate::process_control::ProcessSignal::Suspend)
                    .await
            })
        };

        // The supervisor has authorized the exact attempt and still owns the
        // transition guard immediately before the syscall.
        pause.wait();
        assert!(
            control.producer_transition_guard_is_held_for_test(),
            "the paused supervisor must own the transition guard"
        );
        let (started, contender_started) = std::sync::mpsc::channel();
        let (fenced, observed) = std::sync::mpsc::channel();
        let fence = {
            let control = control.clone();
            std::thread::spawn(move || {
                started.send(()).expect("contender started");
                control.fence_unavailable();
                fenced.send(()).expect("fence observation");
            })
        };
        contender_started
            .recv_timeout(Duration::from_secs(1))
            .expect("fence contender reached the guarded operation");
        assert!(
            observed.recv_timeout(Duration::from_millis(50)).is_err(),
            "retirement cannot linearize between authorization and signal"
        );
        pause.wait();
        assert!(signal
            .await
            .expect("signal task")
            .expect("supervised signal"));
        assert_eq!(
            control.producer_flow_applied_for_test(),
            Some((attempt, true)),
            "the successful SIGSTOP publishes the held flow for the authorized attempt"
        );
        observed
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement follows the physical signal");
        fence.join().expect("fence thread");
        assert!(control.is_retired());
        assert_eq!(
            control.producer_flow_applied_for_test(),
            Some((attempt, true)),
            "retirement does not fabricate a flow acknowledgement"
        );
        assert!(
            !child
                .signal(crate::process_control::ProcessSignal::Resume)
                .await
                .expect("retired signal verdict"),
            "no signal can land after the newer retirement fence"
        );
        assert_eq!(
            control.producer_flow_applied_for_test(),
            Some((attempt, true)),
            "a stale SIGCONT cannot publish a running acknowledgement"
        );

        let mut child = Arc::try_unwrap(child).unwrap_or_else(|_| panic!("sole child owner"));
        child.kill().await.expect("reap stopped child");
    }

    #[tokio::test]
    async fn dropping_process_owner_terminates_and_reaps_without_a_pid_side_channel() {
        let control = crate::playback_control::RollingControlHandle::spawn("supervisor-drop");
        let attempt = control
            .begin_producer_attempt()
            .await
            .expect("producer attempt");
        let child = AttemptChild::new(attempt, long_running_child(), control.clone(), None);
        #[cfg(unix)]
        let pid = child.id().expect("running producer pid");
        drop(child);

        let exit = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(exit) = control
                    .snapshot()
                    .await
                    .and_then(|lease| lease.delivery.producer_exit)
                {
                    break exit;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drop-triggered supervisor reap");
        assert!(!exit.success);
        #[cfg(unix)]
        assert_eq!(exit.signal, Some(libc::SIGKILL));
        #[cfg(windows)]
        assert_eq!(exit.signal, None);
        #[cfg(unix)]
        {
            assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }

    #[tokio::test]
    async fn status_falls_back_to_exact_process_facts_when_actor_is_unavailable() {
        let dir = crate::test_tempdir().expect("status scratch");
        let mut session = test_session(dir.path().to_owned());
        if let Some(child) = session.child.get_mut().take() {
            let mut child = child;
            child.kill().await.expect("remove fixture child");
        }
        session.control = crate::playback_control::RollingControlHandle::unavailable_for_test();
        session.progress.note_out_time(2_500);
        session.progress.speed_milli.store(1_250, Relaxed);
        session.progress.recent_milli.store(1_100, Relaxed);
        session.suspended.store(true, Relaxed);
        let child = tokio::process::Command::new("false")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn terminal producer");
        *session.child.get_mut() = Some(AttemptChild::new(0, child, session.control.clone(), None));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if session
                    .child
                    .get_mut()
                    .as_mut()
                    .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("supervisor terminal observation");

        let status = session_info(
            "actor-unavailable",
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
        assert_eq!(status.lease_state, "unavailable");
        assert_eq!(status.producer_state, "exited", "terminal precedes held");
        assert_eq!(status.producer_attempt, Some(0));
        assert_eq!(status.producer_exit_success, Some(false));
        assert_eq!(status.producer_exit_code, Some(1));
        assert_eq!(status.speed, None);
        assert_eq!(status.recent_speed, None);
        assert_eq!(status.out_time_ms, None);
        assert_eq!(status.progress_idle_ms, -1);
    }

    #[tokio::test]
    async fn status_snapshot_never_combines_successor_process_facts_with_predecessor_actor_state() {
        let dir = crate::test_tempdir().expect("status scratch");
        let session = Arc::new(test_session(dir.path().to_owned()));
        let predecessor = session
            .control
            .begin_producer_attempt()
            .await
            .expect("predecessor attempt");
        session.progress.begin_fenced_attempt(predecessor);
        session.progress.note_out_time(1_000);
        session.progress.speed_milli.store(900, Relaxed);
        session.progress.recent_milli.store(800, Relaxed);
        session
            .control
            .observe_producer_progress(predecessor, Some(1_000), Some(900), Some(800));
        assert_eq!(
            session
                .control
                .snapshot()
                .await
                .expect("predecessor snapshot")
                .delivery
                .producer_out_time_ms,
            Some(1_000)
        );
        let old_child = session.child.lock().await.take();
        if let Some(mut old_child) = old_child {
            old_child.kill().await.expect("remove fixture child");
        }
        *session.child.lock().await = Some(AttemptChild::new(
            predecessor,
            long_running_child(),
            session.control.clone(),
            None,
        ));

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .activity_detail_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let reader = {
            let session = Arc::clone(&session);
            tokio::spawn(async move {
                session_info(
                    "attempt-coherence",
                    &session,
                    AheadLimits {
                        max_secs: 0,
                        max_bytes: 0,
                        global_max_bytes: 0,
                    },
                    0,
                    0,
                )
                .await
            })
        };
        pause.wait().await;

        let successor = session
            .control
            .begin_producer_attempt()
            .await
            .expect("successor attempt");
        session.progress.begin_fenced_attempt(successor);
        session.progress.note_out_time(99_000);
        session.progress.speed_milli.store(9_000, Relaxed);
        session.progress.recent_milli.store(8_000, Relaxed);
        let old_child = session.child.lock().await.take();
        if let Some(mut old_child) = old_child {
            old_child.kill().await.expect("reap predecessor fixture");
        }
        let child = tokio::process::Command::new("false")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn successor terminal producer");
        *session.child.lock().await = Some(AttemptChild::new(
            successor,
            child,
            session.control.clone(),
            None,
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let terminal = session
                    .child
                    .lock()
                    .await
                    .as_mut()
                    .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_some()));
                if terminal {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("successor terminal observation");
        pause.wait().await;

        let status = reader.await.expect("status reader");
        assert_eq!(status.producer_attempt, Some(predecessor));
        assert_eq!(status.out_time_ms, Some(1_000));
        assert_eq!(status.speed, Some(0.9));
        assert_eq!(status.recent_speed, Some(0.8));
        assert_eq!(status.producer_exit_success, None);
        assert_eq!(status.producer_exit_code, None);
        assert_eq!(status.producer_state, "waiting");
    }
