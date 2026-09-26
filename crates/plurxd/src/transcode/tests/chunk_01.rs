    use super::*;

    #[test]
    fn metrics_encoder_inventory_has_closed_labels_and_records_successful_starts() {
        let metrics = CodecQualificationMetrics::default();
        metrics.record_encoder(Encoder::Qsv, OutputGrade::Hdr10);
        metrics.record_encoder(Encoder::Qsv, OutputGrade::Hdr10);
        metrics.record_pipeline(Pipeline::DoviPassthrough);
        let rendered = metrics.prometheus(&EncoderCaps {
            qsv: true,
            vaapi: true,
            ..EncoderCaps::default()
        });

        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("plurx_encoder_available{"))
                .count(),
            QUALIFICATION_ENCODERS.len()
        );
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("plurx_encoder_sessions_total{"))
                .count(),
            QUALIFICATION_ENCODERS.len() * QUALIFICATION_GRADES.len()
        );
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("plurx_tone_map_pipeline_sessions_total{"))
                .count(),
            QUALIFICATION_PIPELINES.len()
        );
        assert!(rendered.contains("plurx_encoder_available{family=\"software\"} 1\n"));
        assert!(rendered.contains("plurx_encoder_available{family=\"qsv\"} 1\n"));
        assert!(rendered.contains("plurx_encoder_available{family=\"nvenc\"} 0\n"));
        assert!(
            rendered.contains("plurx_encoder_sessions_total{family=\"qsv\",grade=\"hdr10\"} 2\n")
        );
        assert!(rendered
            .contains("plurx_tone_map_pipeline_sessions_total{pipeline=\"dovi_passthrough\"} 1\n"));
    }

    #[tokio::test]
    async fn held_plan_fallback_reasons() {
        use crate::decode_facts::{DecodeFactError, DecodePlanFallbackReason};
        use plurx_core::store::SqliteStore;
        use plurx_core::transcode::PlanSourceBinding;

        let cases = vec![
            (
                DecodeFactError::Deadline,
                DecodePlanFallbackReason::Deadline,
            ),
            (
                DecodeFactError::Cancelled,
                DecodePlanFallbackReason::Cancelled,
            ),
            (
                DecodeFactError::ProbeChanged,
                DecodePlanFallbackReason::ProbeChanged,
            ),
            (
                DecodeFactError::Spawn("spawn".into()),
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::MissingPipe,
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::Read("read".into()),
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::OversizedOutput,
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::Failed(Some(1), "failed".into()),
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::InvalidJson("json".into()),
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::InvalidFacts("facts".into()),
                DecodePlanFallbackReason::ProbeFailed,
            ),
            (
                DecodeFactError::ProbeIdentity("identity".into()),
                DecodePlanFallbackReason::IdentityIo,
            ),
            (
                DecodeFactError::SourceMetadata("metadata".into()),
                DecodePlanFallbackReason::IdentityIo,
            ),
            (
                DecodeFactError::CacheInvariant,
                DecodePlanFallbackReason::Invariant,
            ),
        ];
        #[cfg(not(target_os = "linux"))]
        let cases = cases
            .into_iter()
            .chain(std::iter::once((
                DecodeFactError::UnsupportedPlatform,
                DecodePlanFallbackReason::Invariant,
            )))
            .collect::<Vec<_>>();

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id = seed_file(&store).await;
        let file = store
            .get_file(file_id)
            .await
            .expect("get file")
            .expect("media file");
        for (error, expected) in cases {
            assert_eq!(error.fallback_reason(), expected, "{error}");
            let (manager, _work, _cache) = cached_manager(&store);
            let options = manager.options_for_tone_map(
                Encoder::Software,
                &file,
                720,
                0.0,
                None,
                None,
                Some(1),
                ToneMap::None,
                OutputGrade::Sdr,
            );
            let plan = manager
                .resolve_held_movie_plan_facts(&file, &options, Encoder::Software, Err(error))
                .await
                .expect("every non-source-change failure keeps the catalog fallback");
            assert_eq!(plan.source_binding(), PlanSourceBinding::CatalogRow);
            assert!(
                manager
                    .decode_facts
                    .metrics()
                    .prometheus()
                    .contains(&format!(
                        "plurx_decode_plan_fallbacks_total{{reason=\"{}\"}} 1",
                        expected.label()
                    )),
                "the real fallback disposition records {}",
                expected.label()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_change_refuses_bound_plan() {
        use plurx_core::store::SqliteStore;
        use std::os::unix::fs::PermissionsExt as _;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source_path = media.path().join("source-change.mkv");
        std::fs::write(&source_path, b"source before probe").expect("source fixture");
        let file_id = seed_real_file(&store, &source_path).await;
        let file = store
            .get_file(file_id)
            .await
            .expect("get file")
            .expect("media file");

        let probe_path = media.path().join("ffprobe-source-change");
        std::fs::write(
            &probe_path,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then printf '%s\\n' 'ffprobe version source-change'; exit 0; fi\nprintf '%s\\n' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"profile\":\"High\",\"pix_fmt\":\"yuv420p\",\"width\":160,\"height\":120,\"avg_frame_rate\":\"24/1\",\"r_frame_rate\":\"24/1\",\"color_transfer\":\"bt709\",\"disposition\":{\"attached_pic\":0}}]}'\n",
        )
        .expect("probe fixture");
        let mut permissions = std::fs::metadata(&probe_path)
            .expect("probe metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&probe_path, permissions).expect("executable probe");
        let probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
            probe_path.to_str().expect("probe path"),
        )
        .await
        .expect("probe identity");
        let (manager, _work, _cache) = cached_manager(&store);
        let manager = manager
            .with_decode_probe(Some(probe))
            .with_decode_source_final_identity_delay(Duration::from_millis(250));
        let options = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            120,
            0.0,
            None,
            None,
            Some(1),
            ToneMap::None,
            OutputGrade::Sdr,
        );
        let mutation_path = source_path.clone();
        let mutation = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            std::fs::write(mutation_path, b"source changed after collection")
                .expect("mutate source");
        });
        let result = manager
            .resolve_held_movie_plan(
                &file,
                &options,
                Encoder::Software,
                BoundPlanCaller::Vod.decode_fact_source(
                    Arc::new(std::fs::File::open(&source_path).expect("open held source")),
                    Arc::new(tokio::sync::Semaphore::new(1)),
                ),
                Instant::now() + Duration::from_secs(2),
                None,
            )
            .await;
        mutation.await.expect("source mutation");
        assert_eq!(
            result,
            Err("the held source changed during decoder probing; rescan before playback".into())
        );
        let reason = "the held source changed during decoder probing; rescan before playback";
        let vod_error = BoundPlanCaller::Vod
            .finish(Err(reason.to_owned()))
            .expect_err("VOD must preserve the typed decoder-plan refusal");
        assert_eq!(
            vod_refusal(&vod_error),
            Some(("vod_decoder_plan_refused", reason))
        );
        assert_eq!(
            BoundPlanCaller::Pretranscode.finish(Err(reason.to_owned())),
            Err(reason.to_owned()),
            "the pretranscode job must receive the actionable failure for retry/settlement"
        );
        assert!(manager
            .decode_facts
            .metrics()
            .prometheus()
            .contains("plurx_decode_plan_fallbacks_total{reason=\"refused_source_changed\"} 1"));
    }

    /// A decode-fact probe takes the class of the caller waiting on it (plan
    /// P-02 §3.2.2, review of #518 finding 1). The VOD start waits for it for
    /// up to `DECODE_PLAN_PROBE_BUDGET` with a viewer in front of it, so its
    /// probes are realtime; the pre-transcode pass has nobody waiting, so its
    /// probes are background. Before the fix one constant made both
    /// background.
    #[cfg(unix)]
    #[tokio::test]
    async fn decode_fact_probes_take_the_class_of_the_caller_waiting_on_them() {
        use crate::process_control::{priority::spawns_of, ChildClass};
        use plurx_core::store::SqliteStore;
        use std::os::unix::fs::PermissionsExt as _;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source_path = media.path().join("caller-class.mkv");
        std::fs::write(&source_path, b"held source for the class test").expect("source fixture");
        let file_id = seed_real_file(&store, &source_path).await;
        let mut file = store
            .get_file(file_id)
            .await
            .expect("get file")
            .expect("media file");
        let source_metadata = std::fs::metadata(&source_path).expect("source metadata");
        file.size = source_metadata.len() as i64;
        file.mtime = source_metadata
            .modified()
            .expect("source modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("source after epoch")
            .as_secs() as i64;

        let probe_path = media.path().join("ffprobe-caller-class");
        std::fs::write(
            &probe_path,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then printf '%s\\n' 'ffprobe version caller-class'; exit 0; fi\nprintf '%s\\n' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\",\"profile\":\"High\",\"pix_fmt\":\"yuv420p\",\"width\":160,\"height\":120,\"avg_frame_rate\":\"24/1\",\"r_frame_rate\":\"24/1\",\"color_transfer\":\"bt709\",\"disposition\":{\"attached_pic\":0}}]}'\n",
        )
        .expect("probe fixture");
        let mut permissions = std::fs::metadata(&probe_path)
            .expect("probe metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&probe_path, permissions).expect("executable probe");
        let discover = || {
            crate::decode_facts::DecodeProbeIdentity::discover_fixture(
                probe_path.to_str().expect("probe path"),
            )
        };

        let vod = BoundPlanCaller::Vod.decode_fact_work();
        let pretranscode = BoundPlanCaller::Pretranscode.decode_fact_work();
        assert_eq!(vod.class, ChildClass::Realtime);
        assert_eq!(pretranscode.class, ChildClass::Background);

        // The VOD start's seam, through its own manager so no cached fact
        // stands in for a probe.
        let (manager, _work, _cache) = cached_manager(&store);
        let manager = manager.with_decode_probe(Some(discover().await.expect("probe identity")));
        let options = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            120,
            0.0,
            None,
            None,
            Some(1),
            ToneMap::None,
            OutputGrade::Sdr,
        );
        let before = spawns_of(vod);
        let _ = manager
            .resolve_vod_movie_plan(
                &file,
                &options,
                Encoder::Software,
                Arc::new(std::fs::File::open(&source_path).expect("open held source")),
            )
            .await;
        assert!(
            spawns_of(vod) > before,
            "the VOD start's decode-fact probe did not run at the realtime class"
        );

        // The pre-transcode pass's seam.
        let (manager, _work, _cache) = cached_manager(&store);
        let manager = manager.with_decode_probe(Some(discover().await.expect("probe identity")));
        let bound_source = pretranscode_source_snapshot(&file, &[media.path().to_path_buf()])
            .await
            .expect("bound source");
        let before = spawns_of(pretranscode);
        let _ = manager
            .resolve_bound_movie_plan(
                &file,
                &options,
                Encoder::Software,
                &Arc::new(bound_source),
                Instant::now() + Duration::from_secs(5),
                None,
            )
            .await;
        assert!(
            spawns_of(pretranscode) > before,
            "the pre-transcode pass's decode-fact probe did not run at the background class"
        );
    }

    #[tokio::test]
    async fn channel_playback_repair_installs_normalized_native_hls_retry() {
        let file = execution_file_for_retry();
        let video_options =
            transcode::CopyVideoOptions::new(false, false).with_parameter_set_promotion(true);
        let args = transcode::hls_copy_args_with_dolby_vision(
            &file,
            6275.560,
            None,
            true,
            Pacing::unpaced(),
            video_options,
            "/tmp/channel-repair",
        );
        let retry = build_prepublication_copy_retry(
            true,
            video_options,
            args,
            "frozen-presentation",
            PathBuf::from("/tmp/runtime-cache"),
        )
        .expect("parameter-set promotion is supported by the native HLS retry");
        let joined = retry.args.join(" ");
        assert!(
            joined.contains("-bsf:v hevc_mp4toannexb,extract_extradata"),
            "production retry lost required normalization: {joined}"
        );
        assert!(!joined.contains("remove_types=32-34"), "{joined}");

        let (control, mut registration) =
            crate::playback_control::RollingControlHandle::spawn_prepublication_producer(
                "channel-repair-test",
            );
        registration
            .register()
            .await
            .expect("register production executor");
        control
            .bind_response_publication_contract(
                "frozen-presentation".into(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .expect("bind frozen presentation");
        let attempt = control
            .begin_initial_producer_attempt(crate::playback_control::InitialProducerPolicy::copy(
                "frozen-presentation".into(),
                PROGRESS_STALL,
                Some(retry.actor_recipe.clone()),
            ))
            .await
            .expect("install initial production policy");
        control
            .classify_copy_producer_exit_before(
                attempt,
                crate::playback_control::CopyProducerExitClassification::Unsupported,
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("classify realistic unsupported copy shape");
        let decision = registration.next_decision().await;
        let crate::playback_control::RollingProducerExecutorPoll::Decision(decision) = decision
        else {
            panic!("unsupported production copy did not select the frozen retry");
        };
        let crate::playback_control::ProducerDecision::Retry {
            failed_attempt,
            recipe,
            reason,
            ..
        } = decision.as_ref()
        else {
            panic!("unsupported production copy became terminal");
        };
        assert_eq!(*failed_attempt, attempt);
        assert_eq!(
            *reason,
            crate::playback_control::ProducerDecisionReason::Unsupported
        );
        assert_eq!(recipe, &retry.actor_recipe);

        let conversion = video_options.with_dolby_vision_conversion(true);
        assert!(
            build_prepublication_copy_retry(
                true,
                conversion,
                Vec::new(),
                "frozen-presentation",
                PathBuf::from("/tmp/runtime-cache"),
            )
            .is_none(),
            "the native muxer cannot replace Plurx's in-process RPU conversion"
        );
    }

    #[tokio::test]
    async fn channel_playback_repair_validates_distinct_init_before_publication() {
        let dir = crate::test_tempdir().expect("copy init directory");
        let feed = plurx_core::testfixtures::pipe_with_distinct_hevc_sample_entries("closed-gop");
        let mut reader = plurx_core::fmp4::FragmentReader::new();
        reader.push(&feed);
        let Some(plurx_core::fmp4::Unit::Init(init)) = reader
            .next_unit()
            .expect("parse distinct-description fixture")
        else {
            panic!("fixture must begin with init");
        };
        let Some(plurx_core::fmp4::Unit::Fragment(first)) =
            reader.next_unit().expect("parse first fixture fragment")
        else {
            panic!("fixture init must be followed by media");
        };
        // FFmpeg opens init.mp4 before it has enough packets to write moov.
        // Only the first segment rename establishes that the init is complete.
        tokio::fs::write(dir.path().join("init.mp4"), b"")
            .await
            .expect("open still-empty emitted init");
        let output = dir.path().to_path_buf();
        let producer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            tokio::fs::write(output.join("init.mp4"), &init.bytes)
                .await
                .expect("finish emitted init");
            let temporary = output.join("seg00000.m4s.tmp");
            tokio::fs::write(&temporary, &first.bytes)
                .await
                .expect("write first media privately");
            tokio::fs::rename(&temporary, output.join(plurx_core::fmp4::segment_name(0)))
                .await
                .expect("publish completed first media");
        });

        let mut session = test_session(dir.path().to_path_buf());
        session.kind = SessionKind::Copy {
            aac: true,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        session.frozen_presentation = Some(FrozenHlsPresentation::new(
            execution_file_for_retry(),
            HlsContext {
                file_id: 91,
                start_seconds: 6275.560,
                media_origin_seconds: 6275.560,
                codecs: "hvc1.2.4.L153.B0,mp4a.40.2".into(),
                supplemental_codecs: None,
                frame_rate: Some(24.0),
            },
            &session.kind,
        ));
        session.actor_prepublication_producer.store(true, Release);
        let producer_attempt = session.control.current_producer_attempt();
        let layout = validate_copy_init_before_publication(
            &session,
            producer_attempt,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("wait for complete media before validating the distinct configurations");
        producer.await.expect("producer finished");
        assert_eq!(
            layout,
            plurx_core::fmp4::HevcSampleEntryLayout::Multiple { count: 2 }
        );

        tokio::fs::write(dir.path().join("init.mp4"), b"not an init")
            .await
            .expect("replace init with malformed bytes");
        let error = validate_copy_init_before_publication(
            &session,
            producer_attempt,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect_err("malformed init cannot reach the first-media handoff");
        assert!(matches!(error, CopyInitValidationError::Invalid(_)));
    }

    #[tokio::test]
    async fn copy_init_wait_without_completed_media_respects_the_request_deadline() {
        let dir = crate::test_tempdir().expect("copy init directory");
        tokio::fs::write(dir.path().join("init.mp4"), b"")
            .await
            .expect("muxer opened an incomplete init");
        let session = test_session(dir.path().to_path_buf());
        session.actor_prepublication_producer.store(true, Release);
        let attempt = session.control.current_producer_attempt();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            validate_copy_init_before_publication(
                &session,
                attempt,
                Instant::now() + Duration::from_millis(30),
            ),
        )
        .await
        .expect("an unpublished copy cannot hold the HTTP request indefinitely");
        assert!(matches!(result, Err(CopyInitValidationError::StateChanged)));
        assert!(copy_init_attempt_is_current(&session, attempt));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepared_plan_is_not_reprobed_during_producer_execution() {
        use plurx_core::store::SqliteStore;
        use std::os::unix::fs::PermissionsExt as _;

        super::require_ffmpeg();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source_path = media.path().join("neutral-probe-timeout.mkv");
        write_real_video(&source_path, 2);
        let file_id = seed_real_file(&store, &source_path).await;
        let mut file = store
            .get_file(file_id)
            .await
            .expect("get file")
            .expect("media file");
        let source_metadata = std::fs::metadata(&source_path).expect("source metadata");
        file.size = source_metadata.len() as i64;
        file.mtime = source_metadata
            .modified()
            .expect("source modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("source after epoch")
            .as_secs() as i64;

        let probe = media.path().join("slow-ffprobe");
        std::fs::write(
            &probe,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then\n  printf '%s\\n' 'ffprobe version neutral-timeout'\n  exit 0\nfi\nsleep 5\nprintf '%s\\n' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\"}]}'\n",
        )
        .expect("write slow probe");
        let mut permissions = std::fs::metadata(&probe)
            .expect("probe metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&probe, permissions).expect("executable probe");
        let probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
            probe.to_str().expect("probe path"),
        )
        .await
        .expect("fixture probe identity");

        let (manager, _work, _cache) = cached_manager(&store);
        let manager = manager.with_decode_probe(Some(probe));
        let output = crate::test_tempdir().expect("output");
        let output = plurx_core::fs_secure::SecureDirectory::open(output.path())
            .await
            .expect("secure output");
        let bound_source = pretranscode_source_snapshot(&file, &[media.path().to_path_buf()])
            .await
            .expect("bound source");
        let options = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            120,
            0.0,
            None,
            None,
            Some(1),
            ToneMap::None,
            OutputGrade::Sdr,
        );
        let production_budget = Duration::from_secs(2);
        let started = Instant::now();
        let plan = manager
            .resolve_movie_plan(&file, &options, Encoder::Software)
            .await
            .expect("resolve producer plan");
        let produced = manager
            .produce_into(
                &output,
                "neutral-probe-timeout",
                &PortableProduction {
                    file: &file,
                    opts: &options,
                    plan: &plan,
                    deadline: started + production_budget,
                    yield_to_offline: false,
                    cancelled: None,
                    offline_package_id: None,
                    offline_claim_generation: None,
                    publication_fence: None,
                    pretranscode_fence: None,
                    expected_policy_generation: None,
                    expected_source_snapshot: None,
                    bound_source: Some(Arc::new(bound_source)),
                },
                None,
            )
            .await
            .expect("legacy producer after neutral observation")
            .expect("legacy FFmpeg completed inside its retained budget");
        assert!(produced.segments > 0, "the legacy FFmpeg produced no media");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepared_plan_keeps_producer_execution_out_of_the_probe_lane() {
        use plurx_core::store::SqliteStore;
        use std::os::unix::fs::PermissionsExt as _;

        super::require_ffmpeg();
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let media = crate::test_tempdir().expect("media");
        let source_path = media.path().join("final-probe-observation-timeout.mkv");
        write_real_video(&source_path, 2);
        let file_id = seed_real_file(&store, &source_path).await;
        let mut file = store
            .get_file(file_id)
            .await
            .expect("get file")
            .expect("media file");
        let source_metadata = std::fs::metadata(&source_path).expect("source metadata");
        file.size = source_metadata.len() as i64;
        file.mtime = source_metadata
            .modified()
            .expect("source modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("source after epoch")
            .as_secs() as i64;

        let probe = media.path().join("fast-ffprobe");
        std::fs::write(
            &probe,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then\n  printf '%s\\n' 'ffprobe version final-observation-timeout'\n  exit 0\nfi\nprintf '%s\\n' '{\"streams\":[{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\"}]}'\n",
        )
        .expect("write fast probe");
        let mut permissions = std::fs::metadata(&probe)
            .expect("probe metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&probe, permissions).expect("executable probe");
        let probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
            probe.to_str().expect("probe path"),
        )
        .await
        .expect("fixture probe identity");

        let (manager, _work, _cache) = cached_manager(&store);
        let manager = manager
            .with_decode_probe(Some(probe))
            .with_decode_source_final_identity_delay(Duration::from_secs(5));
        let output = crate::test_tempdir().expect("output");
        let output = plurx_core::fs_secure::SecureDirectory::open(output.path())
            .await
            .expect("secure output");
        let bound_source = pretranscode_source_snapshot(&file, &[media.path().to_path_buf()])
            .await
            .expect("bound source");
        let options = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            120,
            0.0,
            None,
            None,
            Some(1),
            ToneMap::None,
            OutputGrade::Sdr,
        );
        let production_budget = Duration::from_secs(2);
        let started = Instant::now();
        let plan = manager
            .resolve_movie_plan(&file, &options, Encoder::Software)
            .await
            .expect("resolve producer plan");
        let produced = manager
            .produce_into(
                &output,
                "final-probe-observation-timeout",
                &PortableProduction {
                    file: &file,
                    opts: &options,
                    plan: &plan,
                    deadline: started + production_budget,
                    yield_to_offline: false,
                    cancelled: None,
                    offline_package_id: None,
                    offline_claim_generation: None,
                    publication_fence: None,
                    pretranscode_fence: None,
                    expected_policy_generation: None,
                    expected_source_snapshot: None,
                    bound_source: Some(Arc::new(bound_source)),
                },
                None,
            )
            .await
            .expect("legacy producer after final observation timeout")
            .expect("legacy FFmpeg completed after the probe released its source offset lane");
        assert!(produced.segments > 0, "the legacy FFmpeg produced no media");
    }

    #[tokio::test]
    async fn orphan_sweep_never_enters_the_live_tv_owned_namespace() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let work = crate::test_tempdir().expect("work root");
        let live_session = work
            .path()
            .join(LIVE_TV_WORK_DIR_NAME)
            .join("live-tv-active");
        tokio::fs::create_dir_all(&live_session)
            .await
            .expect("active Live TV scratch");
        tokio::fs::write(live_session.join("index.m3u8"), b"active")
            .await
            .expect("active playlist");
        let ordinary_orphan = work.path().join("orphan-vod");
        tokio::fs::create_dir(&ordinary_orphan)
            .await
            .expect("ordinary orphan");
        let manager = TranscodeManager::new(
            store,
            work.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );

        assert!(manager.sweep_orphan_dirs().await >= 1);
        assert!(live_session.join("index.m3u8").is_file());
        assert!(!ordinary_orphan.exists());
    }

    #[test]
    fn retired_rolling_marker_prewarm_ambiguity_survives_plan_method_drift() {
        let scope = "[\"user_id\",1]";
        let key = (scope.to_owned(), 7, "transcode");
        let recent = std::sync::Mutex::new(RecentMarkerAmbiguityLedger {
            entries: HashMap::from([(key.clone(), Instant::now() - Duration::from_secs(1))]),
            overflow_ambiguous_until: None,
        });
        assert!(!recent_rolling_marker_ambiguity(&recent, scope, 7));
        remember_rolling_marker_ambiguity_key(&recent, scope, 7, "transcode");
        assert!(recent_rolling_marker_ambiguity(&recent, scope, 7,));
        assert!(recent.lock().expect("recent lock").entries[&key] > Instant::now());
        assert!(!recent_rolling_marker_ambiguity(
            &recent,
            "[\"user_id\",2]",
            7,
        ));
        assert!(!recent_rolling_marker_ambiguity(
            &recent,
            "[\"user_id\",1]",
            8,
        ));
    }

    #[test]
    fn rolling_marker_prewarm_ambiguity_saturation_fails_closed() {
        let deadline = Instant::now() + MARKER_AMBIGUITY_TTL;
        let recent = std::sync::Mutex::new(RecentMarkerAmbiguityLedger {
            entries: (0..MAX_MARKER_AMBIGUITIES)
                .map(|id| ((format!("user-{id}"), id as i64, "remux"), deadline))
                .collect(),
            overflow_ambiguous_until: None,
        });

        remember_rolling_marker_ambiguity_key(&recent, "overflow", i64::MAX, "transcode");

        let ledger = recent.lock().expect("recent lock");
        assert_eq!(ledger.entries.len(), MAX_MARKER_AMBIGUITIES);
        assert!(ledger.overflow_ambiguous_until.is_some());
        drop(ledger);
        assert!(
            recent_rolling_marker_ambiguity(&recent, "unrepresented", -1),
            "an unrepresented retirement makes every placeholder ambiguous"
        );
    }

    #[tokio::test]
    async fn rolling_etag_changes_with_incarnation_and_producer_attempt() {
        let first_dir = crate::test_tempdir().expect("first rolling ETag session");
        let second_dir = crate::test_tempdir().expect("second rolling ETag session");
        let first = Arc::new(test_session(first_dir.path().to_path_buf()));
        let second = Arc::new(test_session(second_dir.path().to_path_buf()));
        let first_attempt = MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            session: Arc::clone(&first),
            producer_attempt: 0,
        });
        let successor_attempt = MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            session: Arc::clone(&first),
            producer_attempt: 1,
        });
        let replacement_incarnation = MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            session: second,
            producer_attempt: 0,
        });

        let first_etag = first_attempt
            .rolling_etag("reusable-session", "seg00001.m4s", 4096)
            .expect("rolling ETag");
        assert_ne!(
            first_etag,
            successor_attempt
                .rolling_etag("reusable-session", "seg00001.m4s", 4096)
                .expect("successor-attempt ETag")
        );
        assert_ne!(
            first_etag,
            replacement_incarnation
                .rolling_etag("reusable-session", "seg00001.m4s", 4096)
                .expect("replacement-incarnation ETag")
        );
    }

    /// Install the durable route a control exchange is verified against.
    ///
    /// Reachable from the HLS boundary tests as well: the seek-coalescing
    /// acceptance drives real control exchanges through the manager, and a
    /// second copy of this setup would be a second thing to keep true.
    pub(crate) async fn activate_control_route(
        store: &dyn Store,
        session_id: &str,
        generation: &str,
        owner_node_id: &str,
    ) {
        let now_ms = crate::media_sessions::unix_ms();
        let fingerprint = "a".repeat(64);
        store
            .claim_media_session_request(
                7,
                generation,
                &fingerprint,
                "player-control",
                generation,
                now_ms,
                now_ms + 60_000,
            )
            .await
            .expect("claim route");
        assert!(store
            .assign_media_session_request_owner(7, generation, generation, owner_node_id, now_ms,)
            .await
            .expect("assign route owner"));
        let activation = plurx_core::domain::MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: generation.to_owned(),
            session_id: session_id.to_owned(),
            user_id: 7,
            playback_id: "player-control".to_owned(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: Some(generation.to_owned()),
            request_fingerprint: fingerprint,
            owner_node_id: owner_node_id.to_owned(),
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
    }

    #[tokio::test]
    async fn live_control_renews_only_fresh_sequences_and_cannot_cross_a_fence() {
        let dir = crate::test_tempdir().expect("session dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        activate_control_route(
            fixture.store.as_ref(),
            &session_id,
            &generation,
            "test-node",
        )
        .await;
        fixture
            .session
            .control
            .set_renewal_for_test(Instant::now() - Duration::from_secs(10), "before-control")
            .await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence, owner_epoch| crate::playback_control::LocalControlRequest {
            session_id: &session_id,
            generation: &generation,
            owner_node_id: "test-node",
            owner_epoch,
            client_instance_id: &client,
            sequence,
            snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Web,
            ),
            prepared_successor: crate::playback_control::PreparedSuccessorObservation::NotRequested,
        };

        let accepted = fixture
            .state
            .transcode
            .hls_session_control(request(1, 1))
            .await
            .expect("local worker")
            .expect("accepted control");
        assert_eq!(
            accepted.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        assert_eq!(
            accepted.lease_timeout_ms,
            crate::playback_control::ROLLING_EXPLICIT_LEASE_TIMEOUT_MS
        );
        let assert_explicit_status = |status: &HlsSessionInfo| {
            let HlsSessionInfo::Live(status) = status else {
                panic!("rolling control returned VOD status");
            };
            assert_eq!(status.lease_mode, "explicit");
            assert_eq!(status.lease_state, "active");
            assert_eq!(
                status.lease_timeout_ms,
                Some(crate::playback_control::ROLLING_EXPLICIT_LEASE_TIMEOUT_MS)
            );
            assert_eq!(status.control_demand, Some("active"));
            assert_eq!(status.reported_position_ms, Some(10_000));
            assert_eq!(status.client_runway_ms, Some(15_000));
            assert_eq!(status.render_state, Some("rendering"));
            assert_eq!(status.production_policy, "publication_clock_explicit");
            let producer_control = status
                .producer_control
                .as_ref()
                .expect("rolling status carries actor projection");
            assert!(producer_control.observation_only);
            assert!(producer_control.last_applied_sequence > 0);
        };
        assert_explicit_status(&accepted.status);
        let live_status = fixture
            .state
            .transcode
            .hls_session_status(&session_id)
            .await
            .expect("live status");
        assert_explicit_status(&live_status);
        let accepted_lease = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("lease snapshot");
        assert_eq!(accepted_lease.last_renewal_kind, "control");
        assert_eq!(
            accepted_lease.mode,
            crate::playback_control::RollingLeaseMode::Explicit
        );
        assert_eq!(
            accepted_lease
                .demand
                .as_ref()
                .map(|demand| demand.position_ms),
            Some(10_000)
        );

        fixture
            .session
            .control
            .set_renewal_for_test(
                Instant::now() - Duration::from_secs(10),
                "accepted-sequence",
            )
            .await;

        let replay = fixture
            .state
            .transcode
            .hls_session_control(request(1, 1))
            .await
            .expect("local worker")
            .expect("replay control");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        let replay_lease = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("replay lease");
        assert_eq!(replay_lease.last_renewal_kind, "accepted-sequence");
        assert!(replay_lease.idle_for >= Duration::from_secs(10));

        assert!(matches!(
            fixture
                .state
                .transcode
                .hls_session_control(request(0, 1))
                .await,
            Some(Err(
                crate::playback_control::ControlStateError::StaleSequence
            ))
        ));
        assert!(matches!(
            fixture
                .state
                .transcode
                .hls_session_control(request(2, 2))
                .await,
            Some(Err(
                crate::playback_control::ControlStateError::OwnerChanged
            ))
        ));
        let rejected_lease = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("rejected lease");
        assert_eq!(rejected_lease.last_renewal_kind, "accepted-sequence");
        assert!(rejected_lease.idle_for >= Duration::from_secs(10));

        tokio::time::sleep(Duration::from_millis(300)).await;
        let second_sequence = fixture
            .state
            .transcode
            .hls_session_control(request(2, 1))
            .await
            .expect("local worker")
            .expect("next fresh sequence remains admissible");
        assert_eq!(
            second_sequence.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        let renewed_lease = fixture
            .session
            .control
            .snapshot()
            .await
            .expect("renewed lease");
        assert_eq!(renewed_lease.last_renewal_kind, "control");
        assert!(renewed_lease.idle_for < Duration::from_secs(1));

        // Retirement is another command on the same mailbox. Once it wins,
        // neither a later control nor a later media request can renew.
        fixture.session.end_activity().await;
        assert!(matches!(
            fixture
                .state
                .transcode
                .hls_session_control(request(3, 1))
                .await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));
        assert!(
            !fixture
                .session
                .touch_attempt_if_active(
                    "segment",
                    fixture.session.control.current_producer_attempt(),
                )
                .await
        );
        assert!(fixture
            .session
            .control
            .snapshot()
            .await
            .is_some_and(|lease| lease.retired));
    }

    #[tokio::test]
    async fn accepted_rolling_control_end_returns_and_replays_terminal_status() {
        let root = crate::test_tempdir().expect("fixture root");
        let session_dir = root.path().join("session");
        tokio::fs::create_dir(&session_dir)
            .await
            .expect("create empty session scratch");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish_with_separate_state_root(
            &session_dir,
            &root.path().join("state"),
            &session_id,
        )
        .await;
        activate_control_route(
            fixture.store.as_ref(),
            &session_id,
            &generation,
            "test-node",
        )
        .await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence| {
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Web,
            );
            snapshot.demand = crate::playback_control::PlaybackDemand::End;
            snapshot.playback_rate = 0.0;
            snapshot.render_state = crate::playback_control::RenderState::Ended;
            crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "test-node",
                owner_epoch: 1,
                client_instance_id: &client,
                sequence,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            }
        };
        let retry_pause = Arc::new(tokio::sync::Barrier::new(2));
        let committer = crate::playback_control::RecoveringTerminalCommitter::with_retry_pause(
            Arc::clone(&retry_pause),
        );

        let accepted = fixture
            .state
            .transcode
            .hls_session_control_with_terminal(request(1), i64::MAX, Some(committer.clone()), None)
            .await
            .expect("local worker")
            .expect("end accepted");
        assert_eq!(
            accepted.disposition,
            crate::playback_control::ControlDisposition::Accepted
        );
        assert_eq!(accepted.lease_state, "ended");
        let HlsSessionInfo::Live(status) = &accepted.status else {
            panic!("rolling end returned VOD status");
        };
        assert_eq!(status.lease_state, "ended");
        assert_eq!(status.control_demand, Some("end"));
        assert_eq!(
            fixture
                .session
                .control
                .snapshot()
                .await
                .expect("terminal lease")
                .terminal,
            Some(crate::playback_control::RollingTerminalCause::End)
        );
        assert!(matches!(
            accepted
                .terminal_commit
                .as_ref()
                .expect("rolling terminal receipt")
                .wait()
                .await,
            Err(())
        ));
        assert_eq!(committer.attempts(), 1);
        assert!(
            fixture
                .state
                .transcode
                .retire_session(&session_id, &fixture.session)
                .await,
            "rolling cleanup may remove the retired worker after a bounded failed attempt"
        );
        assert!(
            !fixture
                .state
                .transcode
                .sessions
                .lock()
                .await
                .contains_key(&session_id),
            "the retry proof must use the manager terminal tombstone, not the live session"
        );

        let (replay_a, replay_b, ()) = tokio::join!(
            fixture.state.transcode.hls_session_control_with_terminal(
                request(1),
                i64::MAX,
                Some(committer.clone()),
                None,
            ),
            fixture.state.transcode.hls_session_control_with_terminal(
                request(1),
                i64::MAX,
                Some(committer.clone()),
                None,
            ),
            async {
                // The first replay has marked the one shared receipt running
                // and entered the second durable attempt. The other replay
                // now has a deterministic interval in which to contend.
                retry_pause.wait().await;
                tokio::task::yield_now().await;
                assert_eq!(
                    committer.attempts(),
                    2,
                    "both waiters must share one in-flight retry attempt"
                );
                retry_pause.wait().await;
            }
        );
        let replay_a = replay_a
            .expect("first local terminal worker")
            .expect("first exact end replay");
        let replay_b = replay_b
            .expect("second local terminal worker")
            .expect("second exact end replay");
        assert_eq!(
            replay_a.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(
            replay_b.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert_eq!(replay_a.lease_state, "ended");
        assert_eq!(replay_b.lease_state, "ended");
        assert_eq!(committer.attempts(), 2);
        let committed_a = replay_a
            .terminal_commit
            .as_ref()
            .expect("first retried rolling terminal receipt")
            .wait()
            .await
            .expect("first recovered terminal response");
        let committed_b = replay_b
            .terminal_commit
            .as_ref()
            .expect("second retried rolling terminal receipt")
            .wait()
            .await
            .expect("second recovered terminal response");
        assert_eq!(committed_a, committed_b);
        assert_eq!(committed_a.server_time_unix_ms, 1);
        assert!(matches!(
            fixture
                .state
                .transcode
                .hls_session_control(request(2))
                .await,
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
            ))
        ));
        // Keep the filesystem-backed retirement and concurrent retry on real
        // time. A start-paused runtime may auto-advance while async scratch
        // cleanup is pending; under hosted load that can expire the receipt
        // before the retry reaches `retry_pause`, leaving this test's barrier
        // waiting for an attempt that correctly never starts. Freeze time only
        // after the durable retry has completed, where advancing it exercises
        // terminal tombstone expiry without mixing virtual time with real I/O.
        tokio::time::pause();
        let terminal_deadline = replay_a
            .terminal_commit
            .as_ref()
            .expect("rolling terminal receipt")
            .deadline_for_test();
        tokio::time::advance(
            terminal_deadline
                .duration_since(tokio::time::Instant::now())
                .saturating_sub(Duration::from_millis(1)),
        )
        .await;
        assert_eq!(
            fixture
                .state
                .transcode
                .hls_session_control(request(1))
                .await
                .expect("rolling operation retained before expiry")
                .expect("exact rolling replay before expiry")
                .disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        assert!(fixture
            .state
            .transcode
            .terminal_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session_id));

        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            fixture
                .state
                .transcode
                .hls_session_control(request(1))
                .await
                .is_none(),
            "the rolling manager must drop the exact retained operation at acknowledgement expiry"
        );
        assert!(!fixture
            .state
            .transcode
            .terminal_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session_id));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(fixture
            .state
            .transcode
            .hls_session_control(request(1))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn accepted_hold_and_active_control_signal_the_real_producer_immediately() {
        let dir = crate::test_tempdir().expect("session dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        // Steady state, not startup: see `mark_started`.
        fixture.mark_started().await;
        activate_control_route(
            fixture.store.as_ref(),
            &session_id,
            &generation,
            "test-node",
        )
        .await;
        let client = uuid::Uuid::new_v4().to_string();
        let request = |sequence, demand, position_ms: i64| {
            let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                crate::playback_control::ClientPlatform::Web,
            );
            snapshot.demand = demand;
            // The started fixture has produced 64 s and published 48 s, one
            // whole batch staged. Held on time additionally requires the
            // producer to have reached what the next publication will ask
            // for: the clock's desired end (position + 48 s initial runway)
            // plus the 10 s guard, capped at its allowed end (+64 s). From
            // position 0 that is 58 s, which 64 s of produced media covers;
            // from position 10 s it is 68 s, which it does not.
            snapshot.position_ms = position_ms;
            snapshot.buffered_from_ms = Some(position_ms);
            if demand != crate::playback_control::PlaybackDemand::Active {
                snapshot.playback_rate = 0.0;
                snapshot.render_state = crate::playback_control::RenderState::Waiting;
            }
            crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "test-node",
                owner_epoch: 1,
                client_instance_id: &client,
                sequence,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            }
        };

        let hold = fixture
            .state
            .transcode
            .hls_session_control(request(
                1,
                crate::playback_control::PlaybackDemand::Hold,
                0,
            ))
            .await
            .expect("local worker")
            .expect("hold accepted");
        assert_eq!(
            hold.lease_timeout_ms,
            crate::playback_control::ROLLING_EXPLICIT_LEASE_TIMEOUT_MS
        );
        let HlsSessionInfo::Live(hold_status) = hold.status else {
            panic!("rolling hold returned VOD status");
        };
        assert!(hold_status.suspended);
        assert_eq!(hold_status.control_demand, Some("hold"));
        assert_eq!(hold_status.hold_reason, Some(AheadHoldReason::Demand));
        assert_eq!(hold_status.production_target_seconds, Some(0));

        tokio::time::sleep(Duration::from_millis(300)).await;
        let active = fixture
            .state
            .transcode
            .hls_session_control(request(
                2,
                crate::playback_control::PlaybackDemand::Active,
                0,
            ))
            .await
            .expect("local worker")
            .expect("active accepted");
        let HlsSessionInfo::Live(active_status) = active.status else {
            panic!("rolling active returned VOD status");
        };
        assert!(active_status.suspended);
        assert_eq!(active_status.control_demand, Some("active"));
        assert_eq!(active_status.hold_reason, Some(AheadHoldReason::Time));
        assert_eq!(
            active_status.production_policy,
            "publication_clock_explicit"
        );

        let events = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = fixture
                    .store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        event: None,
                        limit: 20,
                        ..plurx_core::domain::PlaybackEventQuery::default()
                    })
                    .await
                    .expect("flow events");
                if events.iter().any(|event| event.event == "suspend") {
                    return events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("demand hold event persisted");
        assert!(
            events.iter().all(|event| event.event != "resume"),
            "active demand cannot bypass an already-staged publication batch"
        );
        let suspend = events
            .iter()
            .find(|event| event.event == "suspend")
            .expect("demand suspend event");
        assert_eq!(suspend.hold_reason.as_deref(), Some("demand"));
        let extra: serde_json::Value = serde_json::from_str(
            suspend
                .extra
                .as_deref()
                .expect("demand suspend policy snapshot"),
        )
        .expect("valid demand suspend policy snapshot");
        assert_eq!(extra["lease_mode"], "explicit");
        assert_eq!(extra["lease_state"], "active");
        assert_eq!(extra["lease_timeout_ms"], 30_000);
        assert_eq!(extra["control_demand"], "hold");
        assert_eq!(extra["production_policy"], "explicit_demand");
        assert_eq!(extra["production_target_seconds"], 0);
        assert_eq!(extra["producer_control"]["observation_only"], true);

        // The same staged batch no longer holds a viewer ten seconds further
        // on: the next publication needs media the producer has not made yet,
        // so active demand resumes it instead of trapping publication at one
        // segment per cycle.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let behind = fixture
            .state
            .transcode
            .hls_session_control(request(
                3,
                crate::playback_control::PlaybackDemand::Active,
                10_000,
            ))
            .await
            .expect("local worker")
            .expect("active accepted");
        let HlsSessionInfo::Live(behind_status) = behind.status else {
            panic!("rolling active returned VOD status");
        };
        assert!(
            !behind_status.suspended,
            "a producer short of the next publication must resume"
        );
        assert_eq!(behind_status.hold_reason, None);
        assert_eq!(behind_status.control_demand, Some("active"));
        let events = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = fixture
                    .store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        event: None,
                        limit: 20,
                        ..plurx_core::domain::PlaybackEventQuery::default()
                    })
                    .await
                    .expect("flow events");
                if events.iter().any(|event| event.event == "resume") {
                    return events;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("resume event persisted");
        let resume = events
            .iter()
            .find(|event| event.event == "resume")
            .expect("resume event");
        assert_eq!(resume.hold_reason.as_deref(), Some("time"));
    }

    #[tokio::test]
    async fn cancelled_control_response_cannot_cancel_an_accepted_producer_transition() {
        let dir = crate::test_tempdir().expect("session dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        // Steady state, not startup: see `mark_started`.
        fixture.mark_started().await;
        activate_control_route(
            fixture.store.as_ref(),
            &session_id,
            &generation,
            "test-node",
        )
        .await;

        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *fixture
            .session
            .control_applied_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let client = uuid::Uuid::new_v4().to_string();
        let request = {
            let manager = Arc::clone(&fixture.state.transcode);
            let session_id = session_id.clone();
            let generation = generation.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
                    crate::playback_control::ClientPlatform::Web,
                );
                snapshot.demand = crate::playback_control::PlaybackDemand::Hold;
                snapshot.playback_rate = 0.0;
                snapshot.render_state = crate::playback_control::RenderState::Waiting;
                manager
                    .hls_session_control(crate::playback_control::LocalControlRequest {
                        session_id: &session_id,
                        generation: &generation,
                        owner_node_id: "test-node",
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

        // The actor has accepted, renewed, retained demand, and issued its
        // flow ticket, but the request still owns the child transition gate.
        pause.wait().await;
        request.abort();
        let request_error = match request.await {
            Ok(_) => panic!("request unexpectedly completed"),
            Err(error) => error,
        };
        assert!(request_error.is_cancelled());

        tokio::time::timeout(Duration::from_secs(1), async {
            while !fixture.session.suspended.load(Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached flow worker applied accepted hold after cancellation");

        let mut snapshot = crate::playback_control::PlaybackDemandSnapshot::test_default(
            crate::playback_control::ClientPlatform::Web,
        );
        snapshot.demand = crate::playback_control::PlaybackDemand::Hold;
        snapshot.playback_rate = 0.0;
        snapshot.render_state = crate::playback_control::RenderState::Waiting;
        let replay = fixture
            .state
            .transcode
            .hls_session_control(crate::playback_control::LocalControlRequest {
                session_id: &session_id,
                generation: &generation,
                owner_node_id: "test-node",
                owner_epoch: 1,
                client_instance_id: &client,
                sequence: 1,
                snapshot,
                prepared_successor:
                    crate::playback_control::PreparedSuccessorObservation::NotRequested,
            })
            .await
            .expect("live session")
            .expect("replayed control");
        assert_eq!(
            replay.disposition,
            crate::playback_control::ControlDisposition::Replay
        );
        let HlsSessionInfo::Live(status) = replay.status else {
            panic!("rolling replay returned VOD status");
        };
        assert!(status.suspended);
        assert_eq!(status.hold_reason, Some(AheadHoldReason::Demand));
    }

    #[tokio::test]
    async fn retirement_before_flow_completion_cannot_escape_as_active_control() {
        let dir = crate::test_tempdir().expect("session dir");
        let session_id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish(dir.path(), &session_id).await;
        activate_control_route(
            fixture.store.as_ref(),
            &session_id,
            &generation,
            "test-node",
        )
        .await;
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *fixture
            .session
            .flow_completion_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let client = uuid::Uuid::new_v4().to_string();
        let control = {
            let manager = Arc::clone(&fixture.state.transcode);
            let session_id = session_id.clone();
            let generation = generation.clone();
            tokio::spawn(async move {
                manager
                    .hls_session_control(crate::playback_control::LocalControlRequest {
                        session_id: &session_id,
                        generation: &generation,
                        owner_node_id: "test-node",
                        owner_epoch: 1,
                        client_instance_id: &client,
                        sequence: 1,
                        snapshot: crate::playback_control::PlaybackDemandSnapshot::test_default(
                            crate::playback_control::ClientPlatform::Web,
                        ),
                        prepared_successor:
                            crate::playback_control::PreparedSuccessorObservation::NotRequested,
                    })
                    .await
            })
        };

        // The actor accepted and producer policy ran, but the ticket has not
        // yet released the HTTP response. Retirement wins in that interval.
        pause.wait().await;
        assert!(
            fixture
                .state
                .transcode
                .stop_session(&session_id, "test-retirement")
                .await
        );
        pause.wait().await;
        assert!(matches!(
            control.await.expect("control task"),
            Some(Err(
                crate::playback_control::ControlStateError::SessionEnded
                    | crate::playback_control::ControlStateError::OwnerTransition
            ))
        ));
    }

    #[tokio::test]
    async fn reap_loop_finishes_a_dropped_retirement_with_truthful_cause_and_cleanup() {
        let root = crate::test_tempdir().expect("fixture root");
        let session_dir = root.path().join("session");
        tokio::fs::create_dir(&session_dir)
            .await
            .expect("create empty session scratch");
        let session_id = uuid::Uuid::new_v4().to_string();
        let fixture = HlsDeliveryFixture::publish_with_separate_state_root(
            &session_dir,
            &root.path().join("state"),
            &session_id,
        )
        .await;
        // Model a caller disappearing after the actor committed explicit
        // retirement but before it entered manager teardown. The actor-level
        // regression drops the actual oneshot reply; this exercises the real
        // repair loop that must recover the committed fence.
        fixture.session.control.end().await.expect("end verdict");
        assert!(fixture.session.control.is_retired());
        assert!(!fixture
            .state
            .transcode
            .renewable_session_ids()
            .await
            .contains(&session_id));

        let reaper = tokio::spawn(Arc::clone(&fixture.state.transcode).reap_loop());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let map_gone = !fixture
                    .state
                    .transcode
                    .sessions
                    .lock()
                    .await
                    .contains_key(&session_id);
                let child_exited = {
                    let mut child = fixture.session.child.lock().await;
                    child.as_mut().is_none_or(|child| {
                        child
                            .try_wait()
                            .expect("placeholder child status")
                            .is_some()
                    })
                };
                let scratch_gone = tokio::fs::metadata(&fixture.session.dir).await.is_err();
                if map_gone && child_exited && scratch_gone {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("repair loop must finish map, child, and scratch teardown");

        let event = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(event) = fixture
                    .store
                    .playback_events(&plurx_core::domain::PlaybackEventQuery {
                        event: Some("session_end".to_owned()),
                        limit: 20,
                        ..plurx_core::domain::PlaybackEventQuery::default()
                    })
                    .await
                    .expect("playback events")
                    .into_iter()
                    .find(|event| event.reason.as_deref() == Some("retired_recovery"))
                {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("retired recovery event persisted");
        assert_eq!(
            event.session_id.as_deref(),
            Some(session_log_id(&session_id).as_str())
        );
        reaper.abort();
        assert!(matches!(reaper.await, Err(error) if error.is_cancelled()));
    }

    #[test]
    fn bearer_session_ids_are_stably_redacted_for_observability() {
        let raw = "00000000-0000-4000-8000-0000000000d1";
        let redacted = session_log_id(raw);
        assert_eq!(redacted, session_log_id(raw));
        assert_ne!(
            redacted,
            session_log_id("00000000-0000-4000-8000-0000000000d2")
        );
        assert_eq!(redacted.len(), 66);
        assert!(redacted.starts_with("s-"));
        assert!(!redacted.contains(raw));
    }

    #[test]
    fn captured_ffmpeg_command_and_stderr_logs_redact_bearer_capabilities() {
        use tracing_subscriber::prelude::*;

        let raw = "00000000-0000-4000-8000-0000000000d1";
        let capability_url = format!("/api/v1/hls/{raw}/index.m3u8");
        let args = vec![
            "-i".to_owned(),
            capability_url.clone(),
            format!("/var/lib/plurx/transcode/{raw}/index.m3u8"),
        ];
        let logs = Arc::new(crate::logbuf::LogBuffer::new(8));
        let subscriber =
            tracing_subscriber::registry().with(crate::logbuf::BufferLayer(Arc::clone(&logs)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                "{}",
                ffmpeg_args_log_message("transcode ffmpeg args", &args, raw)
            );
            log_ffmpeg_stderr(
                raw,
                "qsv",
                &format!("could not write {capability_url}: permission denied"),
            );
        });

        let captured = logs.tail("trace", 8);
        assert_eq!(captured.len(), 2, "{captured:?}");
        for entry in captured {
            assert!(!entry.message.contains(raw), "{}", entry.message);
            assert!(
                !entry.message.contains(&capability_url),
                "{}",
                entry.message
            );
            assert!(
                entry.message.contains(&session_log_id(raw)),
                "{}",
                entry.message
            );
        }
    }

    #[test]
    fn scratch_capacity_fails_closed_when_the_background_sample_expires() {
        let sampled_at = 10_000;
        let max_age = i64::try_from(SCRATCH_SAMPLE_MAX_AGE.as_millis()).expect("age fits i64");
        assert_eq!(fresh_scratch_bytes(4096, sampled_at, sampled_at), 4096);
        assert_eq!(
            fresh_scratch_bytes(4096, sampled_at, sampled_at + max_age),
            4096
        );
        assert_eq!(
            fresh_scratch_bytes(4096, sampled_at, sampled_at + max_age + 1),
            0
        );
        assert_eq!(fresh_scratch_bytes(4096, 0, sampled_at), 0);
        assert_eq!(fresh_scratch_bytes(4096, sampled_at, sampled_at - 1), 0);
    }

    #[test]
    fn scratch_capacity_rejects_an_in_progress_seqlock_publication() {
        let generation = AtomicU64::new(2);
        let bytes = AtomicI64::new(4096);
        let sampled_at = AtomicI64::new(10_000);
        assert_eq!(
            read_scratch_sample(&generation, &bytes, &sampled_at),
            (4096, 10_000)
        );

        generation.store(3, Release);
        bytes.store(0, Relaxed);
        assert_eq!(
            read_scratch_sample(&generation, &bytes, &sampled_at),
            (0, 0)
        );
    }

    /// P2-10. The stored key reaches `VodSettings`, and an absent or unusable
    /// value keeps the built-in default.
    ///
    /// This is the layer the first version of the change never tested: its
    /// only test hand-built a `VodSettings` in Rust, so the entire
    /// store → reader → pool wire could be deleted with the whole suite green.
    /// A setting nothing reads is not a setting.
    #[tokio::test]
    async fn the_blocked_get_cap_setting_is_read_and_bounded() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let dir = crate::test_tempdir().expect("work");
        let manager = Arc::new(TranscodeManager::new(
            Arc::clone(&store),
            dir.path().to_owned(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let req = SessionRequest {
            control_sequence: None,
            file_id: 1,
            playback_id: "cap-probe".to_owned(),
            request_id: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
        };
        let cap = |manager: &Arc<TranscodeManager>, req: &SessionRequest| {
            let manager = Arc::clone(manager);
            let req = req.clone();
            async move {
                manager
                    .vod_settings(&req)
                    .await
                    .expect("settings read")
                    .expect("VOD presentation is on")
                    .blocked_get_cap
            }
        };

        assert_eq!(
            cap(&manager, &req).await,
            64,
            "an unconfigured node still bounds its parked work"
        );

        for (stored, expected, why) in [
            ("512", 512, "the operator's number"),
            ("", 64, "cleared means the default, not zero"),
            ("0", 64, "zero would refuse every blocked GET"),
            ("banana", 64, "unparseable is not a budget"),
            ("999999", 4_096, "clamped to the sanity bound"),
            ("1", 1, "the floor is usable"),
        ] {
            store
                .put_setting(plurx_core::store::keys::VOD_BLOCKED_GET_CAP, stored)
                .await
                .expect("store the setting");
            assert_eq!(cap(&manager, &req).await, expected, "{stored:?}: {why}");
        }
    }

    #[tokio::test]
    async fn metrics_session_snapshot_does_not_wait_for_the_session_map() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let dir = crate::test_tempdir().expect("work");
        let manager = Arc::new(TranscodeManager::new(
            store,
            dir.path().to_owned(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        manager.active_session_count.store(2, Relaxed);
        let sessions = manager.sessions.lock().await;
        let snapshot = manager.metrics_handle();
        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || sent.send(snapshot.snapshot().0));
        assert_eq!(
            received
                .recv_timeout(Duration::from_millis(100))
                .expect("atomic session snapshot must not wait for the map"),
            2
        );
        drop(sessions);
        handle.join().expect("join session reader").expect("send");
    }

    #[tokio::test]
    async fn activity_telemetry_wait_never_holds_the_session_map() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let dir = crate::test_tempdir().expect("work");
        let manager = Arc::new(TranscodeManager::new(
            store,
            dir.path().to_owned(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let session = Arc::new(test_session(dir.path().join("session")));
        manager
            .sessions
            .lock()
            .await
            .insert("selected".to_owned(), Arc::clone(&session));
        let pause = Arc::new(tokio::sync::Barrier::new(2));
        *session
            .activity_detail_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&pause));
        let reader = Arc::clone(&manager);
        let detail = tokio::spawn(async move {
            reader
                .delivery_details_bounded(&["selected".to_owned()], 1)
                .await
        });
        pause.wait().await;

        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), manager.active_sessions())
                .await
                .expect("another map operation must not wait on selected-session telemetry"),
            1
        );
        pause.wait().await;
        assert_eq!(detail.await.expect("activity reader").len(), 1);
    }

    fn profile5_file() -> plurx_core::domain::MediaFile {
        plurx_core::domain::MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 5,
            item_id: 1,
            path: PathBuf::from("/media/profile5.mkv"),
            size: 1,
            mtime: 1,
            duration_ms: Some(1_000),
            container: Some("matroska".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: Some("Main 10".into()),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            hdr: Some("dolby_vision".into()),
            hdr_format: Some("Dolby Vision · Profile 5".into()),
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

    #[test]
    fn frozen_hls_presentation_fingerprint_covers_master_affecting_facts() {
        let file = profile5_file();
        let context = HlsContext {
            file_id: file.id,
            start_seconds: 12.5,
            media_origin_seconds: 12.5,
            codecs: "hvc1.2.4.L150.B0,mp4a.40.2".into(),
            supplemental_codecs: Some("dvh1.08.09".into()),
            frame_rate: Some(23_976.0 / 1_000.0),
        };
        let first = FrozenHlsPresentation::new(
            file.clone(),
            context.clone(),
            &SessionKind::Transcode { height: 1080 },
        );
        let identical = FrozenHlsPresentation::new(
            file.clone(),
            context.clone(),
            &SessionKind::Transcode { height: 1080 },
        );
        let changed = FrozenHlsPresentation::new(
            file,
            HlsContext {
                codecs: "avc1.640034,mp4a.40.2".into(),
                ..context
            },
            &SessionKind::Transcode { height: 1080 },
        );

        assert_eq!(first.contract_fingerprint, identical.contract_fingerprint);
        assert_ne!(first.contract_fingerprint, changed.contract_fingerprint);
    }

    #[test]
    fn an_fmp4_avc_master_is_attempt_media_not_generation_metadata() {
        let file = profile5_file();
        let context = HlsContext {
            file_id: file.id,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs: "avc1.640034,mp4a.40.2".into(),
            supplemental_codecs: None,
            frame_rate: None,
        };
        let fmp4 = FrozenHlsPresentation::new(
            file.clone(),
            context.clone(),
            &SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
        );
        let mpeg_ts =
            FrozenHlsPresentation::new(file, context, &SessionKind::Transcode { height: 1080 });

        assert!(fmp4.sealed_stable_master_contract.is_none());
        assert!(mpeg_ts.sealed_stable_master_contract.is_some());
    }

    #[test]
    fn a_rolling_transcode_freezes_the_rung_geometry() {
        let file = profile5_file();
        let presentation = FrozenHlsPresentation::new(
            file,
            HlsContext {
                file_id: 5,
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                codecs: "avc1.640034,mp4a.40.2".into(),
                supplemental_codecs: None,
                frame_rate: None,
            },
            &SessionKind::Transcode { height: 720 },
        );

        assert_eq!(presentation.file.width, Some(1280));
        assert_eq!(presentation.file.height, Some(720));
    }

    #[test]
    fn a_rolling_transcode_of_an_unprobed_source_freezes_no_geometry() {
        let mut file = profile5_file();
        file.width = None;
        file.height = None;
        let presentation = FrozenHlsPresentation::new(
            file,
            HlsContext {
                file_id: 5,
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                codecs: "avc1.640034,mp4a.40.2".into(),
                supplemental_codecs: None,
                frame_rate: None,
            },
            &SessionKind::Transcode { height: 720 },
        );

        assert_eq!(presentation.file.width, None);
        assert_eq!(presentation.file.height, None);
    }

    #[test]
    fn a_rolling_transcode_never_upscales_its_declaration() {
        let mut file = profile5_file();
        file.width = Some(640);
        file.height = Some(360);
        let presentation = FrozenHlsPresentation::new(
            file,
            HlsContext {
                file_id: 5,
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                codecs: "avc1.640034,mp4a.40.2".into(),
                supplemental_codecs: None,
                frame_rate: None,
            },
            &SessionKind::Transcode { height: 1080 },
        );

        assert_eq!(presentation.file.width, Some(640));
        assert_eq!(presentation.file.height, Some(360));
    }

    #[test]
    fn encoded_vod_presentation_never_mixes_source_width_with_requested_height() {
        let mut file = profile5_file();
        file.width = Some(640);
        file.height = Some(360);

        let presented = encoded_vod_presentation_file(file, 1080, OutputGrade::Sdr);

        assert_eq!(presented.width, Some(640));
        assert_eq!(presented.height, Some(360));
    }

    #[test]
    fn encoded_vod_presentation_omits_both_unprobed_dimensions() {
        let mut file = profile5_file();
        file.width = None;
        file.height = None;

        let presented = encoded_vod_presentation_file(file, 1080, OutputGrade::Sdr);

        assert_eq!(presented.width, None);
        assert_eq!(presented.height, None);
    }

    #[test]
    fn a_copy_session_keeps_the_source_geometry() {
        let file = profile5_file();
        let presentation = FrozenHlsPresentation::new(
            file,
            HlsContext {
                file_id: 5,
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                codecs: "hvc1.2.4.H150.90,mp4a.40.2".into(),
                supplemental_codecs: None,
                frame_rate: None,
            },
            &SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
        );

        assert_eq!(presentation.file.width, Some(3840));
        assert_eq!(presentation.file.height, Some(2160));
    }

    #[test]
    fn frozen_frame_rate_prefers_average_and_rejects_zero_denominator() {
        let probe = serde_json::json!({
            "streams": [{
                "codec_type": "video",
                "avg_frame_rate": "24000/1001",
                "r_frame_rate": "60/1"
            }]
        })
        .to_string();
        let invalid_average = serde_json::json!({
            "streams": [{
                "codec_type": "video",
                "avg_frame_rate": "24000/0",
                "r_frame_rate": "30000/1001"
            }]
        })
        .to_string();

        assert_eq!(
            frozen_video_frame_rate(Some(&probe)),
            Some(24_000.0 / 1_001.0)
        );
        assert_eq!(
            frozen_video_frame_rate(Some(&invalid_average)),
            Some(30_000.0 / 1_001.0)
        );
    }

    #[test]
    fn media_response_publication_maps_attempt_objects_without_url_inference() {
        use crate::playback_control::RollingResponseObject;

        assert_eq!(
            MediaResponsePublication::attempt_media("playlist", Some("video.m3u8"))
                .rolling_object(),
            RollingResponseObject::VideoMediaPlaylist
        );
        assert_eq!(
            MediaResponsePublication::attempt_media(
                "segment-range-not-satisfiable",
                Some("segment-12.m4s")
            )
            .rolling_object(),
            RollingResponseObject::RangeNotSatisfiable
        );
        assert_eq!(
            MediaResponsePublication::generation_metadata("master-playlist").rolling_object(),
            RollingResponseObject::MasterPlaylist
        );
    }

    #[tokio::test]
    async fn actor_first_media_hook_is_idempotent() {
        let dir = crate::test_tempdir().expect("session scratch");
        let session = Arc::new(test_session(dir.path().to_path_buf()));
        session.actor_prepublication_producer.store(true, Release);
        let (handoff, applied) =
            begin_first_media_publication_handoff(&session, "first-media-hook")
                .await
                .expect("first-media settlement capacity");
        handoff.settle_for_test(true);
        assert!(applied.await.expect("handoff application"));
        assert!(!session.actor_prepublication_producer.load(Acquire));
        assert!(session.first_media_handoff_applied.load(Acquire));

        let (duplicate, duplicate_applied) =
            begin_first_media_publication_handoff(&session, "first-media-hook")
                .await
                .expect("duplicate first-media settlement capacity");
        duplicate.settle_for_test(true);
        assert!(duplicate_applied
            .await
            .expect("duplicate handoff application"));
        assert!(session.first_media_handoff_applied.load(Acquire));
    }

    #[test]
    fn successful_exit_probe_requires_exact_terminal_endlist_and_indexed_frontier() {
        let complete = completion_playlist_evidence(
            7,
            3,
            Some(
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\nseg00000.ts\n\
                   #EXTINF:1.5,\nseg00001.ts\n#EXT-X-ENDLIST\n",
            ),
        );
        assert_eq!(complete.probe_sequence, 7);
        assert_eq!(complete.producer_attempt, 3);
        assert!(complete.end_list);
        assert_eq!(complete.final_segment, Some(1));
        assert_eq!(complete.final_end_ms, Some(3_500));

        for partial in [
            b"#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n".as_slice(),
            b"#EXTM3U\n#EXT-X-ENDLIST\n#EXTINF:2.0,\nseg00000.ts\n".as_slice(),
            b"not utf8: \xff\n".as_slice(),
        ] {
            let evidence = completion_playlist_evidence(8, 4, Some(partial));
            assert!(!evidence.end_list);
        }
    }

    #[test]
    fn only_failed_closed_executor_exits_request_monitor_cleanup() {
        assert_eq!(
            PrepublicationExecutorExit::FailedClosed {
                producer_attempt: 7,
            }
            .monitor_cleanup_attempt(),
            Some(7)
        );
        for settled in [
            PrepublicationExecutorExit::ActorTerminal,
            PrepublicationExecutorExit::SessionGone,
            PrepublicationExecutorExit::ActorFailureApplied,
            PrepublicationExecutorExit::ActorCompletionApplied,
        ] {
            assert_eq!(settled.monitor_cleanup_attempt(), None, "{settled:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn specifically_configured_overlapping_source_root_wins_without_trusting_nested_links() {
        use std::os::unix::fs::symlink;

        let broad = crate::test_tempdir().expect("broad library root");
        let relocated = crate::test_tempdir().expect("relocated library root");
        let source = relocated.path().join("movie.mkv");
        tokio::fs::write(&source, b"bound source")
            .await
            .expect("source bytes");
        symlink(relocated.path(), broad.path().join("nas")).expect("configured relocation");
        let metadata = tokio::fs::metadata(&source).await.expect("source metadata");
        let mut file = profile5_file();
        file.path = broad.path().join("nas/movie.mkv");
        file.size = metadata.len() as i64;
        file.mtime = metadata
            .modified()
            .expect("modified time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix modified time")
            .as_secs() as i64;

        assert!(
            pretranscode_source_snapshot(&file, &[broad.path().to_path_buf()])
                .await
                .is_none(),
            "an unconfigured nested symlink remains outside the broad root's authority"
        );
        assert!(
            pretranscode_source_snapshot(
                &file,
                &[broad.path().to_path_buf(), broad.path().join("nas"),],
            )
            .await
            .is_some(),
            "the longest explicitly configured root authorizes its canonical relocation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bound_source_rejects_same_size_same_mtime_path_replacement() {
        let root = crate::test_tempdir().expect("library root");
        let source = root.path().join("movie.mkv");
        tokio::fs::write(&source, b"original media")
            .await
            .expect("source bytes");
        let metadata = std::fs::metadata(&source).expect("source metadata");
        let modified = metadata.modified().expect("source modified time");
        let mut file = profile5_file();
        file.path = source.clone();
        file.size = metadata.len() as i64;
        file.mtime = LocalSourceSnapshot::from_metadata(&metadata).modified_secs;
        let bound = pretranscode_source_snapshot(&file, &[root.path().to_path_buf()])
            .await
            .expect("bound source");

        let replacement = root.path().join("replacement.mkv");
        std::fs::write(&replacement, b"replaced media").expect("replacement bytes");
        let replacement_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .expect("replacement handle");
        replacement_file
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .expect("preserve mtime");
        std::fs::rename(&replacement, &source).expect("replace pathname");

        let replacement_metadata = std::fs::metadata(&source).expect("replacement metadata");
        assert_eq!(replacement_metadata.len(), metadata.len());
        assert_eq!(
            LocalSourceSnapshot::from_metadata(&replacement_metadata).modified_secs,
            file.mtime
        );
        assert_eq!(
            bound_source_snapshot(Some(&bound)).await,
            None,
            "the old descriptor cannot authorize bytes at a replaced pathname"
        );
    }

    #[test]
    fn noncompatible_dolby_vision_is_never_guessed_through_zscale() {
        let profile5 = profile5_file();
        assert_eq!(TranscodeManager::needs_dovi_reshape(&profile5), Ok(true));

        let mut compatible = profile5.clone();
        compatible.hdr_format = Some("Dolby Vision · Profile 8 (HDR10-compatible)".into());
        assert_eq!(TranscodeManager::needs_dovi_reshape(&compatible), Ok(false));

        let mut unknown = profile5.clone();
        unknown.hdr_format = Some("Dolby Vision".into());
        assert!(TranscodeManager::needs_dovi_reshape(&unknown)
            .expect_err("unknown profile must be refused")
            .contains("unknown"));

        let mut unsupported = profile5;
        unsupported.hdr_format = Some("Dolby Vision · Profile 4".into());
        assert!(TranscodeManager::needs_dovi_reshape(&unsupported)
            .expect_err("unproven non-compatible profile must be refused")
            .contains("Profile 4"));
    }

    #[tokio::test]
    async fn profile5_requires_the_probed_software_renderer_pair() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let dir = crate::test_tempdir().expect("work");
        let caps = EncoderCaps {
            qsv: true,
            ..EncoderCaps::default()
        };
        let manager = TranscodeManager::new(
            Arc::clone(&store),
            dir.path().to_path_buf(),
            caps.clone(),
            Pipeline::VppQsv,
        );
        let file = profile5_file();
        assert!(manager
            .encoder_for_file(&file, crate::process_control::ChildClass::Background)
            .await
            .expect_err("an unproved renderer must refuse before spawn")
            .contains("did not prove"));

        let manager =
            TranscodeManager::new(store, dir.path().join("proved"), caps, Pipeline::VppQsv)
                .with_dovi_reshape(true);
        manager
            .dovi_proofs
            .lock()
            .expect("proof cache")
            .insert(TranscodeManager::dovi_proof_key(&file), true);
        let encoder = manager
            .encoder_for_file(&file, crate::process_control::ChildClass::Background)
            .await
            .expect("proved route");
        assert_eq!(encoder, Encoder::Software);
        let opts = manager.options_for_tone_map(
            encoder,
            &file,
            1080,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        assert_eq!(opts.pipeline, Pipeline::DoviTonemapx);
        assert_eq!(opts.tone_map, ToneMap::Tonemapx);
        assert_eq!(manager.auto_height_for_file(Some(&file), None).await, 720);
    }

    /// The grade predicate and the renderer predicate answer the same
    /// question and must never drift.
    ///
    /// `playback::dolby_vision_needs_rpu_render` decides whether `/decision`
    /// may promise HDR10; `TranscodeManager::needs_dovi_reshape` decides
    /// which renderer this daemon actually builds. If they disagree, a client
    /// is promised a grade the encoder does not produce — an HDR10 badge over
    /// a tone-mapped SDR picture, which plays, so nobody reports it.
    #[test]
    fn the_grade_predicate_and_the_renderer_predicate_agree() {
        let base = profile5_file();
        let variant = |label: Option<&str>, hdr: Option<&str>| {
            let mut file = base.clone();
            file.hdr = hdr.map(str::to_owned);
            file.hdr_format = label.map(str::to_owned);
            file
        };
        let cases = [
            (Some("Dolby Vision · Profile 5"), Some("dolby_vision")),
            (Some("Dolby Vision · Profile 4"), Some("dolby_vision")),
            (Some("Dolby Vision · Profile 7"), Some("dolby_vision")),
            (
                Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
                Some("dolby_vision"),
            ),
            (
                Some("Dolby Vision · Profile 7 (HLG-compatible)"),
                Some("dolby_vision"),
            ),
            (Some("Dolby Vision"), Some("dolby_vision")),
            (None, Some("dolby_vision")),
            (None, Some("hdr10")),
            (None, Some("hlg")),
            (None, None),
        ];
        for (label, hdr) in cases {
            let file = variant(label, hdr);
            assert_eq!(
                plurx_core::playback::dolby_vision_needs_rpu_render(&file),
                TranscodeManager::needs_dovi_reshape(&file) == Ok(true),
                "the two predicates disagree about {label:?} / {hdr:?}"
            );
        }
    }

    /// The renderer a source's grade names and the pipeline the session
    /// actually builds are the same choice, read from one function.
    ///
    /// Both mistakes here are silent, and both are worse than a failure. The
    /// Dolby graph handed ordinary PQ frames emits crushed shadows and
    /// everything above roughly 70% clipped to white, at exit 0, with
    /// correct-looking tags. The plain graph handed a Profile 5 base layer
    /// puts a picture that is not an HDR10 grade in any sense on the wire
    /// tagged as one. Nothing downstream can tell either from the real thing.
    #[test]
    fn the_hdr10_rung_builds_the_pipeline_its_route_names() {
        let base = profile5_file();
        let variant = |label: Option<&str>, hdr: Option<&str>| {
            let mut file = base.clone();
            file.hdr = hdr.map(str::to_owned);
            file.hdr_format = label.map(str::to_owned);
            file
        };
        let cases = [
            (
                Some("Dolby Vision · Profile 5"),
                Some("dolby_vision"),
                Some(Pipeline::DoviPassthrough),
            ),
            (
                Some("Dolby Vision · Profile 8 (HDR10-compatible)"),
                Some("dolby_vision"),
                Some(Pipeline::Hdr10Passthrough),
            ),
            (
                Some("Dolby Vision · Profile 7 (HDR10-compatible)"),
                Some("dolby_vision"),
                Some(Pipeline::Hdr10Passthrough),
            ),
            (None, Some("hdr10"), Some(Pipeline::Hdr10Passthrough)),
            (None, Some("hdr10plus"), Some(Pipeline::Hdr10Passthrough)),
            // No HDR10 rung exists for these, so no pipeline should be named.
            (None, Some("hlg"), None),
            (Some("Dolby Vision · Profile 7"), Some("dolby_vision"), None),
            (Some("Dolby Vision"), Some("dolby_vision"), None),
            (None, None, None),
        ];
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
        let dir = crate::test_tempdir().expect("work");
        let manager = TranscodeManager::new(
            store,
            dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        for (label, hdr, expected) in cases {
            let file = variant(label, hdr);
            // Through the production selector, not a re-implementation of it:
            // a test that maps the route itself and asserts its own mapping
            // cannot notice `options_for_tone_map` drifting.
            let opts = manager.options_for_tone_map(
                Encoder::Software,
                &file,
                HDR10_HEIGHT,
                0.0,
                None,
                None,
                None,
                ToneMap::Zscale,
                OutputGrade::Hdr10,
            );
            let expected = expected.unwrap_or(Pipeline::Cpu);
            assert_eq!(opts.pipeline, expected, "{label:?} / {hdr:?}");
            // And whichever pipeline is named must admit the source it was
            // named for -- `handles` is the guard that keeps the Dolby graph
            // away from plain PQ and the plain graph away from an RPU.
            if expected != Pipeline::Cpu {
                assert!(
                    expected.handles(transcode::routing_hdr(&file)),
                    "{expected:?} refuses the source it was chosen for: {label:?} / {hdr:?}"
                );
                assert_eq!(expected.output_grade(), OutputGrade::Hdr10);
            }
        }
    }

    /// The HDR10 rung runs only at measured encoder/geometry points, and
    /// everything else falls back to the SDR ladder rather than to a guess.
    #[test]
    fn the_hdr10_rungs_only_admit_the_geometries_they_were_measured_at() {
        let mut file = profile5_file();
        file.width = Some(3840);
        file.height = Some(2160);
        assert!(hdr10_rung_fits(&file, HDR10_HEIGHT, Encoder::Software));
        assert!(hdr10_rung_fits(&file, HDR10_HEIGHT, Encoder::Qsv));
        assert!(hdr10_rung_fits(&file, HDR10_4K_HEIGHT, Encoder::Qsv));
        assert!(!hdr10_rung_fits(&file, HDR10_4K_HEIGHT, Encoder::Software));
        for lower in [360, 480, 720, 900] {
            assert!(!hdr10_rung_fits(&file, lower, Encoder::Qsv), "{lower}");
        }

        // Never upscaled into: a 720p source at the 1080 rung produces a 720p
        // frame, which is not the geometry the codec string describes.
        let mut small = file.clone();
        small.width = Some(1280);
        small.height = Some(720);
        assert!(!hdr10_rung_fits(&small, HDR10_HEIGHT, Encoder::Software));

        // A 2.39:1 frame at 1080 high is past HEVC level 4.0's luma bound, so
        // x265 would encode it at level 5 and `HDR10_HLS_CODEC` would be
        // describing a stream that does not exist.
        let mut scope = file.clone();
        scope.width = Some(5760);
        scope.height = Some(2400);
        let (w, h) = plurx_core::transcode::output_size(&scope, HDR10_HEIGHT).expect("size");
        assert!(w * h > HDR10_MAX_LUMA_SAMPLES, "{w}x{h}");
        assert!(!hdr10_rung_fits(&scope, HDR10_HEIGHT, Encoder::Software));

        // An unprobed source has no geometry to check.
        let mut unprobed = file;
        unprobed.width = None;
        assert!(!hdr10_rung_fits(&unprobed, HDR10_HEIGHT, Encoder::Software));
    }

    #[test]
    fn the_measured_qsv_sdr_reshape_keeps_profile5_at_four_k() {
        let file = profile5_file();
        assert_eq!(
            capability_height_for_encoder(Some(&file), Encoder::Qsv),
            MAX_HEIGHT
        );
        assert_eq!(
            capability_height_for_encoder(Some(&file), Encoder::Nvenc),
            AUTO_HARDWARE_PROBED_HEIGHT,
            "the QSV measurement must not widen an unmeasured family"
        );
        assert_eq!(
            capability_height_for_encoder(Some(&file), Encoder::Software),
            AUTO_SOFTWARE_HEIGHT
        );
    }

    /// The session's `CODECS` follows the grade, and its shape is what makes
    /// the master emit `VIDEO-RANGE=PQ` (see `http::hls`, which is not
    /// duplicated here).
    #[test]
    fn a_transcode_advertises_the_codec_its_grade_produces() {
        assert_eq!(
            transcoded_hls_codecs(OutputGrade::Sdr, 2160),
            "avc1.640034,mp4a.40.2"
        );
        let hdr10 = transcoded_hls_codecs(OutputGrade::Hdr10, HDR10_HEIGHT);
        assert_eq!(hdr10, "hvc1.2.4.H120.90,mp4a.40.2");
        assert!(
            hdr10.starts_with("hvc1"),
            "the HDR attribute logic keys on this prefix: {hdr10}"
        );
        assert_eq!(
            transcoded_hls_codecs(OutputGrade::Hdr10, HDR10_4K_HEIGHT),
            "hvc1.2.4.H150.90,mp4a.40.2"
        );
    }

    /// The whole gate, exercised through the manager rather than asserted
    /// about in pieces: what it takes to actually get the HDR10 rung, and the
    /// four ways to lose it.
    #[tokio::test]
    async fn the_hdr10_grade_is_refused_until_every_precondition_is_proved() {
        use plurx_core::store::SqliteStore;

        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let dir = crate::test_tempdir().expect("work");
        let file = profile5_file();
        let manager = TranscodeManager::new(
            Arc::clone(&store),
            dir.path().to_path_buf(),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_dovi_reshape(true)
        .with_dovi_passthrough(true);
        manager
            .dovi_proofs
            .lock()
            .expect("proof cache")
            .insert(TranscodeManager::dovi_proof_key(&file), true);

        // Everything proved, and asked for.
        assert_eq!(
            manager
                .hdr10_grade_for(&file, true, HDR10_HEIGHT, Encoder::Software, false)
                .await
                .expect("proved"),
            OutputGrade::Hdr10
        );
        // Not asked for: absent means not proven, and not proven tone-maps.
        assert_eq!(
            manager
                .hdr10_grade_for(&file, false, HDR10_HEIGHT, Encoder::Software, false)
                .await
                .expect("no request"),
            OutputGrade::Sdr
        );
        // Not the measured rung.
        assert_eq!(
            manager
                .hdr10_grade_for(&file, true, 2160, Encoder::Software, false)
                .await
                .expect("wrong rung"),
            OutputGrade::Sdr
        );
        // A plain HDR10 source on a node that proved only the Dolby renderer.
        // It must not borrow that proof: the Dolby graph applies an RPU this
        // file does not have, and emits a broken picture at exit 0 rather than
        // failing.
        let mut hdr10_source = file.clone();
        hdr10_source.hdr = Some("hdr10".into());
        hdr10_source.hdr_format = None;
        assert_eq!(
            manager
                .hdr10_grade_for(&hdr10_source, true, HDR10_HEIGHT, Encoder::Software, false)
                .await
                .expect("non-dv source"),
            OutputGrade::Sdr
        );
        // With the plain encode proved, the same file takes the plain rung —
        // and the pipeline the session builds for it is the plain graph, not
        // the Dolby one.
        let plain = TranscodeManager::new(
            Arc::clone(&store),
            dir.path().join("plain"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_hdr10_passthrough(true);
        assert_eq!(
            plain
                .hdr10_grade_for(&hdr10_source, true, HDR10_HEIGHT, Encoder::Software, false)
                .await
                .expect("plain rung"),
            OutputGrade::Hdr10
        );
        let plain_opts = plain.options_for_tone_map(
            Encoder::Software,
            &hdr10_source,
            HDR10_HEIGHT,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Hdr10,
        );
        assert_eq!(plain_opts.pipeline, Pipeline::Hdr10Passthrough);
        assert_eq!(plain_opts.pipeline.output_grade(), OutputGrade::Hdr10);
        // A burn tone-maps. Subtitle white is a code value, and on a PQ output
        // it lands at the top of the curve — nominally 10,000 nits.
        assert_eq!(
            plain
                .hdr10_grade_for(&hdr10_source, true, HDR10_HEIGHT, Encoder::Software, true)
                .await
                .expect("burned"),
            OutputGrade::Sdr
        );
        // And the Dolby proof alone does not unlock the plain rung's QSV half:
        // `dovi_passthrough_qsv` is gated behind a filter this route never
        // uses, so reusing it would take the 4K rung off a stock-ffmpeg node.
        assert_eq!(
            plain
                .hdr10_grade_for(&hdr10_source, true, HDR10_HEIGHT, Encoder::Qsv, false)
                .await
                .expect("qsv unproved"),
            OutputGrade::Sdr
        );

        // Not proved by this build.
        let unproved = TranscodeManager::new(
            store,
            dir.path().join("unproved"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_dovi_reshape(true);
        assert_eq!(
            unproved
                .hdr10_grade_for(&file, true, HDR10_HEIGHT, Encoder::Software, false)
                .await
                .expect("unproved build"),
            OutputGrade::Sdr
        );

        // And the options the proved case actually builds.
        let opts = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            HDR10_HEIGHT,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Hdr10,
        );
        assert_eq!(opts.pipeline, Pipeline::DoviPassthrough);
        assert_eq!(opts.pipeline.output_grade(), OutputGrade::Hdr10);
        assert_eq!(opts.tone_map, ToneMap::None);
        assert_eq!(
            opts.effective_rate_control,
            EffectiveRateControl::Vbr,
            "x265 has no measured quality-mode setting"
        );
        // …and the SDR grade for the same file is the pre-M5 reshape rung,
        // untouched.
        let sdr = manager.options_for_tone_map(
            Encoder::Software,
            &file,
            HDR10_HEIGHT,
            0.0,
            None,
            None,
            None,
            ToneMap::Zscale,
            OutputGrade::Sdr,
        );
        assert_eq!(sdr.pipeline, Pipeline::DoviTonemapx);
        assert_eq!(sdr.tone_map, ToneMap::Tonemapx);
    }

    /// An HDR10 request is a different stream, so it cannot recover another
    /// request's session through the idempotency key — and an SDR request
    /// still fingerprints to exactly the string it always did.
    #[test]
    fn the_grade_is_part_of_a_request_identity() {
        let request = SessionRequest {
            control_sequence: None,
            file_id: 5,
            playback_id: "player".into(),
            request_id: Some("attempt".into()),
            automatic: false,
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
        let hdr10 = SessionRequest {
            hdr10: true,
            ..request.clone()
        };
        assert_ne!(
            request.intent_fingerprint("paul"),
            hdr10.intent_fingerprint("paul")
        );
        assert!(hdr10.intent_fingerprint("paul").contains("t1080+hdr10"));
        assert!(request.intent_fingerprint("paul").contains("\"t1080\""));
        assert_ne!(
            request.intent_fingerprint("old-name"),
            request.intent_fingerprint("new-name"),
            "the legacy process-local key keeps its global username scope"
        );
        assert_eq!(
            request.durable_intent_fingerprint(7),
            request.clone().durable_intent_fingerprint(7),
            "the replicated key is scoped by immutable user id, not username"
        );
        assert_ne!(
            request.durable_intent_fingerprint(7),
            request.durable_intent_fingerprint(8),
            "different durable users remain distinct"
        );
    }
