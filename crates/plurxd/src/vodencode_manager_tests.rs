// Included in transcode::tests: production normalization, public create,
// immutable serving, and the native HLS wrapper must agree about one recipe.

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires an actual HDR source, reviewed helper bundle and proved VAAPI device"]
async fn actual_processed_hdr_windows_verify_audio_cadence_native_shapes_and_seek() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    use plurx_core::store::SqliteStore;
    use plurx_core::transcode::dv_processing::{
        DvDisplayTarget, DvDisplayTargetChoice, DvOperation, DvProcessingMode, DvSelection,
        DISPLAY_TARGET_METHOD,
    };
    // Test-only policy injection. Malformed/unsupported requests must not
    // silently run master and then be reported as target qualification.
    let target_nits = match std::env::var("PLURX_DV_WINDOW_TARGET_NITS") {
        Ok(value) => {
            let nits = value.parse::<u32>().expect("integer target nits");
            assert!(
                matches!(nits, 1000 | 1600),
                "physical target matrix is 1000/1600"
            );
            Some(nits)
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("invalid target environment: {error}"),
    };
    let destination = match std::env::var("PLURX_DV_WINDOW_DESTINATION") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "hdr10".into(),
        Err(error) => panic!("invalid destination environment: {error}"),
    };
    assert!(matches!(
        destination.as_str(),
        "hdr10" | "profile81" | "sdr"
    ));
    let profile81 = destination == "profile81";
    let sdr = destination == "sdr";
    assert!(
        !sdr || target_nits.is_none(),
        "SDR always uses its frozen 100-nit policy"
    );
    assert!(
        !(profile81 && target_nits.is_some()),
        "native Profile 8.1 is never display targeted"
    );
    let source = PathBuf::from(std::env::var("PLURX_DV_WINDOW_SOURCE").expect("actual source"));
    let base = crate::test_tempdir().expect("isolated runtime");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("actual source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("isolated store"));
    let id = seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let file = store.get_file(id).await.expect("file").expect("seeded");
    store
        .upsert_file(
            file.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("held source identity");
    for (key, value) in [
        (keys::DV_HDR_PROCESSING, "1"),
        (keys::DV_FEL_REENCODE, "1"),
        (keys::HWACCEL, "vaapi"),
    ] {
        store
            .put_setting(key, value)
            .await
            .expect("isolated preference");
    }
    let file = store.get_file(id).await.expect("file").expect("attested");
    let mut manager = TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("runtime"),
        EncoderCaps {
            vaapi: true,
            // The radeonsi worker's detected policy: the one encoder the
            // reconstructed 8.1's recipe and verdicts are qualified on.
            vaapi_hdr_policy: profile81
                .then_some(plurx_core::transcode::VaapiHdrPolicy::MesaRadeonsiVbaq),
            vaapi_driver: profile81.then(|| "lab radeonsi VA driver".to_owned()),
            ..Default::default()
        },
        Pipeline::Cpu,
    )
    .with_decode_probe(Some(
        crate::decode_facts::DecodeProbeIdentity::discover(&crate::ffmpeg::bound_ffprobe_bin())
            .await
            .expect("actual sealed source probe"),
    ));
    if sdr {
        manager = manager.with_cache(
            base.path().join("cache"),
            crate::ffmpeg::ffmpeg_build().await,
            "node".into(),
        );
        store
            .put_setting(keys::DV_DISPLAY_TARGET, "1")
            .await
            .expect("SDR targeting enabled");
    }
    manager.dovi_reshape = crate::ffmpeg::has_dovi_reshape().await;
    manager.decoders =
        plurx_core::transcode::detect_video_decoders(&crate::ffmpeg::ffmpeg_bin()).await;
    // As at daemon start: the renderer's CPU debit follows the device's
    // proved HEVC Main10 decode.
    println!(
        "{}",
        serde_json::json!({"hevc_main10_vaapi_decode": crate::ffmpeg::has_hevc_main10_vaapi_decode().await})
    );
    manager.dovi_passthrough = crate::ffmpeg::has_dovi_passthrough().await;
    manager.hdr10_passthrough = crate::ffmpeg::has_hdr10_passthrough().await;
    manager.hdr10_passthrough_vaapi =
        crate::ffmpeg::has_hdr10_passthrough_vaapi(manager.caps.vaapi_hdr_policy).await;
    assert!(
        manager.hdr10_passthrough_vaapi,
        "actual P010/Main10 upload proof"
    );
    let display_target = if target_nits.is_some() || sdr {
        let stats = crate::source_probe::publish_title_for_control(&manager, &store, &file)
            .await
            .expect("admitted complete-title job publication for the held source");
        assert_eq!(
            stats.eligibility(),
            plurx_core::scan::dv_title::TitleEligibility::Eligible
        );
        let DvDisplayTargetChoice::Target(target) = (if sdr {
            DvDisplayTarget::for_sdr_title(Some(stats))
        } else {
            DvDisplayTarget::for_title(target_nits.expect("HDR bucket"), Some(stats))
        })
        .expect("supported physical bucket") else {
            panic!("physical target fixture must exceed its bucket; elision is a separate control");
        };
        println!(
            "{}",
            serde_json::json!({"title_stats":stats,"requested_target":target})
        );
        Some(target)
    } else {
        None
    };
    let file = store
        .get_file(id)
        .await
        .expect("current file")
        .expect("stored probe");
    // PLURX_DV_WINDOW_DESTINATION=profile81 takes the Dolby Vision client's
    // route instead: the P7 conversion copy, which FEL re-encode turns into a
    // reconstructed Profile 8.1 rendition at the source raster.
    let request = SessionRequest {
        request_id: Some("actual-processed-hdr-window".into()),
        automatic: false,
        // Any resolution: PLURX_DV_WINDOW_HEIGHT selects the rung (720 by default).
        kind: if profile81 {
            SessionKind::Copy {
                convert_dolby_vision: true,
                dual_layer_as_copied: false,
                aac: true,
                preserve_dolby_vision: false,
            }
        } else {
            SessionKind::Transcode {
                height: std::env::var("PLURX_DV_WINDOW_HEIGHT")
                    .ok()
                    .and_then(|height| height.parse().ok())
                    .unwrap_or(720),
            }
        },
        hdr10: !profile81,
        audio_index: Some(0),
        start_seconds: 0.0,
        presentation: Presentation::Vod,
        block_budget_secs: Some(10.0),
        ..reopen_request(id, "actual-hdr-probe", "unused", "unused")
    };
    if profile81 {
        // No verdict yet: the play keeps the plain conversion while the title
        // is measured. Then an admitting verdict for this source, renderer and
        // recipe (what the title job stores) lets the same start reconstruct.
        let unmeasured = SessionRequest {
            request_id: Some("actual-processed-hdr-window-unmeasured".into()),
            ..request.clone()
        };
        assert!(
            manager
                .prepare_vod_encoding(&unmeasured, &file)
                .await
                .expect("plain conversion start")
                .is_none(),
            "an unmeasured FEL title is not reconstructed"
        );
        let tools = crate::vodencode::dv_runtime::ToolSet::capture()
            .await
            .expect("helper")
            .expect("helper");
        let verdict = plurx_core::transcode::dv_fel_qualification::FelVerdict {
            schema: plurx_core::transcode::dv_fel_qualification::SCHEMA,
            identity: manager
                .fel_identity(&tools.backend_digest())
                .await
                .expect("this node's FEL verdict identity"),
            source_size: file.size,
            source_mtime: file.mtime,
            admitted: true,
            served_mean: 1.0,
            dropped_mean: 2.0,
            served_p99: 1.0,
            dropped_p99: 2.0,
            p99_tolerance: 0.0,
            measured_at_ms: 0,
            windows: Vec::new(),
        };
        assert!(store
            .put_dv_fel_verdict(
                file.id,
                file.size,
                file.mtime,
                &serde_json::to_value(&verdict).expect("verdict"),
            )
            .await
            .expect("verdict stored"));
    }
    let file = store
        .get_file(id)
        .await
        .expect("current file")
        .expect("stored verdict");
    let started = Instant::now();
    let (encoding, prepared_root) = if sdr {
        // Fresh durable admission, canonical catalog restoration and normal
        // one-shot lease/source validation precede genuine SDR rendering.
        let root = admit_native_sdr_window_control(&manager, &store, request.clone(), &file).await;
        (Arc::clone(&root.encoding), Some(root))
    } else {
        (
            manager
                .prepare_vod_encoding(&request, &file)
                .await
                .expect("actual native preparation")
                .expect("encoded route"),
            None,
        )
    };
    let DvSelection::Selected(selected) = &encoding.dv_processing else {
        panic!(
            "actual processing preparation refused: {:?}",
            encoding.dv_processing
        );
    };
    assert_eq!(
        selected.destination(),
        if profile81 {
            plurx_core::transcode::dv_processing::DvDestination::Profile81
        } else if sdr {
            plurx_core::transcode::dv_processing::DvDestination::Sdr
        } else {
            plurx_core::transcode::dv_processing::DvDestination::Hdr10
        }
    );
    assert_eq!(
        encoding.delivered_output_grade(),
        if sdr {
            OutputGrade::Sdr
        } else {
            OutputGrade::Hdr10
        }
    );
    assert_eq!(encoding.preserves_processed_dv(), profile81);
    let runtime = encoding
        .dv_runtime
        .as_ref()
        .expect("owned processing runtime");
    let display_target = if sdr {
        let frozen = runtime.display_target.expect("admitted SDR policy");
        assert!(frozen.is_sdr());
        assert_eq!(frozen.nits(), 100);
        frozen
            .resolved_sdr_floor()
            .expect("admission froze the source floor");
        Some(frozen)
    } else if let Some(target) = display_target {
        Some(
            runtime
                .resolve_display_target(target, None)
                .await
                .expect("single effective held source/mastering policy before targeted windows"),
        )
    } else {
        None
    };
    assert_eq!(runtime.encoder, Encoder::Vaapi);
    // The plan is resolved with the software source decoder; the windows
    // are encoded by the graph's admitted encoder, and every report says so.
    assert_eq!(encoding.plan.output_contract().output_encoder(), "libx265");
    assert_eq!(encoding.processed_window_encoder(), Some(Encoder::Vaapi));
    assert_eq!(encoding.delivered_output_encoder(), "hevc_vaapi");
    assert_eq!(encoding.delivered_encoder_label(), Encoder::Vaapi.label());
    let (index, verified) = runtime
        .prepared
        .lock()
        .expect("first-window owner")
        .take()
        .expect("actual prepared window");
    assert_eq!(index, 0);
    assert!(verified.receipt.is_some());
    assert_eq!(
        verified.observed.pictures.len(),
        usize::try_from(encoding.grid.frames_per_segment).expect("bounded frame count")
    );
    let raster = (
        encoding
            .plan
            .output_contract()
            .effective_width()
            .expect("raster width"),
        encoding
            .plan
            .output_contract()
            .effective_height()
            .expect("raster height"),
    );
    let elapsed = started.elapsed();
    if sdr {
        assert_native_sdr_window(&verified.bytes, raster, encoding.grid).await;
    }
    // PLURX_DV_WINDOW_DUMP=<dir> keeps the served first window (and its source
    // interval) for the offline composition proof and ffprobe checks.
    if let Ok(dir) = std::env::var("PLURX_DV_WINDOW_DUMP") {
        let dir = PathBuf::from(dir);
        std::fs::write(dir.join("window-000.mp4"), &verified.bytes).expect("dump served window");
        let first = &encoding
            .grid
            .plan(file.duration_ms.expect("duration"), 0)
            .entries[0];
        std::fs::write(
            dir.join("window-000.json"),
            serde_json::json!({"start_ticks":first.start_ticks,"duration_ticks":first.duration_ticks,
                "numerator":encoding.grid.numerator,"denominator":encoding.grid.denominator,
                "decoder":format!("{:?}", verified.observed.decoder),
                "renderer_decode":format!("{:?}", runtime.decode),
                "graph_cpu_threads":encoding.resources.cpu_threads})
            .to_string(),
        )
        .expect("dump window interval");
    }
    if profile81 {
        // One live reconstruction per worker: this graph holds the slot, and
        // a second play is not given another graph (it keeps its ordinary
        // converted copy, or an unprocessed recipe), never a second slot.
        assert!(runtime.reconstruction_slot.is_some());
        assert_eq!(manager.processed_reconstruction_slots.available_permits(), 0);
        let second = manager
            .prepare_vod_encoding(
                &SessionRequest {
                    request_id: Some("second-live-reconstruction".into()),
                    playback_id: "second-live-reconstruction".into(),
                    ..request.clone()
                },
                &file,
            )
            .await
            .expect("second play keeps an ordinary route");
        assert!(
            second.as_ref().is_none_or(|second| !matches!(
                second.dv_processing,
                DvSelection::Selected(_)
            )),
            "a second live reconstruction must not be admitted on this worker"
        );
        drop(second);
        assert_eq!(manager.processed_reconstruction_slots.available_permits(), 0);
    }
    let luma = processed_window_luma(&verified.bytes, raster, false);
    println!(
        "{}",
        serde_json::json!({"entry":0,"policy":if sdr { "fresh-owner-sdr100" } else { "master-preparation" },"elapsed_ms":elapsed.as_millis(),
        "decoder":format!("{:?}", verified.observed.decoder),"luma":luma,
        "bytes":verified.bytes.len(),"plan":selected.semantic_digest(),"receipt_frames":verified.receipt.as_ref().map(|receipt| receipt.served_frames().len()),
        "fel_frames":verified.receipt.as_ref().map(|receipt| receipt.counts().el_accepted)})
    );
    // HDR controls prepare master and then inject their explicit test policy;
    // SDR prepares its actual frozen 100-nit destination through fresh admission.
    // Release the first physical owner before independently admitting each window.
    drop(verified);
    drop(prepared_root);
    let segment_plan = encoding.grid.plan(file.duration_ms.expect("duration"), 0);
    let entries: &[usize] = if display_target.is_some() {
        &[0, 1, 2, 360]
    } else {
        &[1, 2, 360]
    };
    // A short excerpt has no twelve-minute window; it checks the ones it has.
    let entries: Vec<usize> = entries
        .iter()
        .copied()
        .filter(|at| *at < segment_plan.entries.len())
        .collect();
    let entries = entries.as_slice();
    let mut target_digest = None;
    for &at in entries {
        let entry = &segment_plan.entries[at];
        let bundle = crate::vodencode::admit_frozen_preparation(
            &manager.admissions,
            manager.max_hw_sessions().await,
            manager.software_budget().await,
            &encoding.resources,
            Some(1),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .expect("actual graph resources");
        let started = Instant::now();
        let (verified, plan) = match runtime
            .execute_policy_until(
                &file,
                &encoding.plan,
                &encoding.options,
                encoding.grid,
                entry,
                &encoding.executable.path,
                crate::dv_segment::SegmentAdmission::Owned(bundle),
                Arc::new(crate::prodrun::ProducerSlot::new()),
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                display_target,
            )
            .await
            .expect("actual verified window")
        {
            crate::vodencode::dv_runtime::WindowOutcome::Completed(window) => window,
            crate::vodencode::dv_runtime::WindowOutcome::Cancelled => {
                panic!("actual graph was cancelled instead of verifying its window")
            }
        };
        if let Some(target) = display_target {
            if sdr {
                assert_eq!(
                    plan.semantic_digest(),
                    selected.semantic_digest(),
                    "frozen SDR admission identity"
                );
                assert_native_sdr_window(&verified.bytes, raster, encoding.grid).await;
            } else {
                assert_ne!(
                    plan.semantic_digest(),
                    selected.semantic_digest(),
                    "target is not master identity"
                );
            }
            let digest = plan.semantic_digest();
            assert_eq!(
                target_digest.get_or_insert_with(|| digest.clone()),
                &digest,
                "target identity is stable across adjacent/seek windows"
            );
            assert_eq!(
                plan.destination(),
                if sdr {
                    plurx_core::transcode::dv_processing::DvDestination::Sdr
                } else {
                    plurx_core::transcode::dv_processing::DvDestination::Hdr10
                }
            );
            let rows = &verified.observed.display_target_rows;
            assert_eq!(rows.len(), verified.observed.pictures.len());
            for (index, (row, picture)) in rows.iter().zip(&verified.observed.pictures).enumerate()
            {
                assert_eq!(row["frame"], index);
                assert_eq!(row["nits"], target.nits());
                assert_eq!(row["method"], DISPLAY_TARGET_METHOD);
                assert_eq!(row["title_min_1e4"], target.mastering().min_1e4_nits);
                assert_eq!(row["title_peak"], target.mastering().max_nits);
                let (max, avg) = target.validate_rpu(&picture.rpu).expect("raw picture L1");
                assert_eq!(row["max_pq"], max);
                assert_eq!(row["avg_pq"], avg);
                assert_eq!(row["max0_fallback"], max == 0);
                assert!(
                    matches!(picture.profile, 7 | 8),
                    "P7 FEL/P8 physical matrix"
                );
                if picture.profile == 7 {
                    assert_eq!(runtime.mode, DvProcessingMode::Fel);
                }
            }
            let receipt = verified.receipt.as_ref().expect("target receipt");
            assert_eq!(
                receipt
                    .counts()
                    .operations_applied
                    .get(&DvOperation::DisplayTargetMapping {
                        nits: target.nits()
                    }),
                Some(&(verified.observed.pictures.len() as u64))
            );
            if sdr {
                assert_eq!(
                    receipt
                        .counts()
                        .operations_applied
                        .get(&DvOperation::Bt709Gamma24Conversion),
                    Some(&(verified.observed.pictures.len() as u64))
                );
            }
        } else {
            assert_eq!(
                plan.semantic_digest(),
                selected.semantic_digest(),
                "adjacent/seek recipe identity"
            );
            assert!(verified.observed.display_target_rows.is_empty());
        }

        assert_eq!(
            verified.observed.pictures.len(),
            usize::try_from(encoding.grid.frames_per_segment).expect("bounded frame count")
        );
        assert!(verified.receipt.is_some());
        assert!(verified
            .observed
            .pictures
            .iter()
            .all(|picture| picture.bl_shape == runtime.native_bl));
        if runtime.mode == DvProcessingMode::Fel {
            assert!(verified
                .observed
                .pictures
                .iter()
                .all(|picture| picture.el_shape.0 > 0));
        }
        // Twelve minutes in is picture, not an opening fade.
        let elapsed = started.elapsed();
        let luma = processed_window_luma(&verified.bytes, raster, at == 360);
        println!(
            "{}",
            serde_json::json!({"entry":at,"target_nits":display_target.map(|target| target.nits()),
            "target_rows":verified.observed.display_target_rows,"elapsed_ms":elapsed.as_millis(),
            "decoder":format!("{:?}", verified.observed.decoder),"luma":luma,
            "bytes":verified.bytes.len(),"plan":plan.semantic_digest(),"receipt_frames":verified.receipt.as_ref().map(|receipt| receipt.served_frames().len()),
        "fel_frames":verified.receipt.as_ref().map(|receipt| receipt.counts().el_accepted)})
        );
    }
}

/// Bounded physical coverage, not HTTP playback or full-app EOF: the shared
/// processed-root regression separately verifies registry, HLS and replay facts.
#[cfg(target_os = "linux")]
async fn admit_native_sdr_window_control(
    manager: &TranscodeManager,
    store: &Arc<dyn Store>,
    mut request: SessionRequest,
    file: &plurx_core::domain::MediaFile,
) -> crate::vodserve::PreparedRootAdmission {
    use plurx_core::playback::{
        audio::{resolve_audio, AudioClaim, AudioRoute},
        candidate::CandidateRoute,
    };
    let planning = store
        .playback_planning_snapshot(file.id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("atomic planning snapshot")
        .expect("source");
    let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
        "v":2,"video":[
            {"codec":"h264","profiles":["high"],"present":["sdr"]},
            {"codec":"hevc","profiles":["main10"],"present":["sdr"]}],
        "display":{"hdr":false,"dolby_vision":false},
        "containers":["mp4"],"transports":["hls"],"hls_hevc_sample_entries":["hvc1"],
        "audio":["aac"],"audio_sinks":[{"codec":"aac","max_channels":2,"sample_rates_hz":[48000]}]
    }))
    .expect("explicit SDR display and Main10 decoder");
    request.hdr10 = false;
    request.previous_session_id = None;
    request.reopen_reason = None;
    request.audio_index = TranscodeManager::candidate_audio_from_snapshot(&planning, None);
    request.audio_claim = AudioClaim::from_caps(&caps).expect("canonical audio claim");
    let track = request.audio_index.and_then(|index| {
        planning
            .file
            .audio_streams
            .iter()
            .find(|track| track.index == index)
    });
    request.audio_delivery = Some(resolve_audio(
        track,
        &request.audio_claim.as_ref().expect("claim").profile(),
        AudioRoute::EncodedVod,
        request.audio_offset_ms,
    ));
    let query = crate::media_pool::QualityCatalogRequest {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        caps: caps.clone(),
        audio_index: request.audio_index,
        audio_offset_ms: request.audio_offset_ms,
        audio_claim: request.audio_claim.clone(),
        audio_delivery: request.audio_delivery.clone(),
        subtitle_burn: None,
        presentation: Presentation::Vod,
        copy_contract: None,
    };
    let catalog = manager
        .quality_catalog_from_snapshot_progress(
            &planning,
            &caps,
            request.audio_index,
            request.audio_offset_ms,
            None,
            Presentation::Vod,
            None,
            request.audio_delivery.as_ref(),
            request.audio_claim.as_ref(),
            None,
            None,
        )
        .await;
    let SessionKind::Transcode { height } = request.kind else {
        panic!("SDR encoded height")
    };
    let candidate = catalog
        .candidates
        .iter()
        .find(|candidate| {
            !candidate.processed_dv
                && candidate.decoder_compatible
                && candidate.route == CandidateRoute::Encode
                && candidate.grade == OutputGrade::Sdr
                && i64::from(candidate.target_height) == height
        })
        .unwrap_or_else(|| {
            panic!(
                "ordinary SDR candidate at {height}: {:?}",
                catalog.candidates
            )
        })
        .clone();
    let bound = crate::media_sessions::CandidateCatalogContext {
        caps,
        candidate: candidate.clone(),
        binding: crate::media_pool::PlanningBinding::from_snapshot(&planning),
    };
    let mut context = manager
        .restore_catalog_context(
            &request,
            &bound,
            candidate.id,
            file.size,
            file.mtime,
            None,
            tokio::time::Instant::now() + Duration::from_secs(30),
        )
        .await
        .expect("canonical candidate restoration");
    context.catalog_query = Some(query);
    request.candidate_context = Some(Box::new(context));
    let user = store
        .create_user("native-sdr-control", "hash", false)
        .await
        .expect("real principal");
    let principal =
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: user.id };
    let incarnation_id = uuid::Uuid::new_v4().to_string();
    request.request_id = Some(incarnation_id.clone());
    let now = crate::media_sessions::unix_ms();
    let claim = store
        .claim_media_session_request(
            &principal,
            &incarnation_id,
            &request.durable_intent_fingerprint(&principal),
            &request.playback_id,
            &incarnation_id,
            now,
            now + 60_000,
        )
        .await
        .expect("durable create admission");
    assert!(matches!(
        claim,
        plurx_core::domain::MediaSessionRequestClaim::Acquired { .. }
    ));
    let recovery = SessionRecoveryIdentity {
        principal,
        incarnation_id,
        recovery_epoch: uuid::Uuid::new_v4().to_string(),
    };
    let (processed, candidate) = manager
        .prepare_processed_hdr10(&request, &recovery, false)
        .await
        .expect("fresh processed owner qualification")
        .expect("SDR processed candidate");
    assert!(candidate.processed_dv);
    assert_eq!(candidate.grade, OutputGrade::Sdr);
    let root = manager
        .admit_processed_window_control(&processed, &recovery, file)
        .await
        .expect("normal lease and source/recipe validation");
    assert!(
        manager
            .admit_processed_window_control(&processed, &recovery, file)
            .await
            .is_err(),
        "same prepared owner lease cannot be consumed twice"
    );
    root
}

#[cfg(target_os = "linux")]
async fn assert_native_sdr_window(
    bytes: &[u8],
    raster: (u32, u32),
    grid: plurx_core::transcode::VodFrameGrid,
) {
    let temporary = crate::test_tempdir().expect("SDR verification");
    let path = temporary.path().join("window.mp4");
    std::fs::write(&path, bytes).expect("actual completed SDR window");
    let output = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("probe actual SDR window");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).expect("SDR probe");
    let streams = document["streams"].as_array().expect("streams");
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .expect("video");
    assert_eq!(video["codec_name"], "hevc");
    assert_eq!(video["profile"], "Main 10");
    assert_eq!(video["width"], raster.0);
    assert_eq!(video["height"], raster.1);
    assert_eq!(video["pix_fmt"], "yuv420p10le");
    assert_eq!(video["color_range"], "tv");
    for field in ["color_transfer", "color_primaries", "color_space"] {
        assert_eq!(video[field], "bt709");
    }
    let (num, den) = video["r_frame_rate"]
        .as_str()
        .expect("cadence")
        .split_once('/')
        .expect("rational cadence");
    assert_eq!(
        num.parse::<u64>().expect("numerator") * u64::from(grid.denominator),
        den.parse::<u64>().expect("denominator") * u64::from(grid.numerator)
    );
    for side in video["side_data_list"].as_array().into_iter().flatten() {
        let kind = side["side_data_type"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            !kind.contains("dovi")
                && !kind.contains("mastering")
                && !kind.contains("content light"),
            "{side}"
        );
    }
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .expect("canonical encoded audio");
    assert_eq!(audio["codec_name"], "aac");
    assert_eq!(audio["channels"], 2);
    assert_eq!(audio["sample_rate"], "48000");
}

/// Decode a processed window and check its luma is a limited-range picture:
/// legal codes only, nominally 64..=940, and, where the film has picture, not
/// flat. A wrong range or bit shift in the export fails these.
#[cfg(target_os = "linux")]
fn processed_window_luma(
    window: &[u8],
    (width, height): (u32, u32),
    expect_picture: bool,
) -> serde_json::Value {
    let directory = crate::test_tempdir().expect("window decode directory");
    let path = directory.path().join("window.mp4");
    std::fs::write(&path, window).expect("window bytes");
    if let Some(dump) = std::env::var_os("PLURX_DV_WINDOW_DUMP") {
        let _ = std::fs::copy(
            &path,
            std::path::Path::new(&dump).join(format!("{expect_picture}-{}.mp4", window.len())),
        );
    }
    // The decoder's own yuv420p10le, so no conversion touches the codes.
    let output = std::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&path)
        .args(["-pix_fmt", "yuv420p10le", "-f", "rawvideo", "pipe:1"])
        .output()
        .expect("decode the processed window");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let luma_bytes = width as usize * height as usize * 2;
    let frame_bytes = luma_bytes * 3 / 2;
    assert!(
        !output.stdout.is_empty() && output.stdout.len().is_multiple_of(frame_bytes),
        "{} decoded bytes for a {width}x{height} window",
        output.stdout.len()
    );
    let codes: Vec<u16> = output
        .stdout
        .chunks_exact(frame_bytes)
        .flat_map(|frame| frame[..luma_bytes].chunks_exact(2))
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    assert!(!codes.is_empty(), "the window decodes to a picture");
    let (low, high) = codes.iter().fold((u16::MAX, 0), |(low, high), code| {
        (low.min(*code), high.max(*code))
    });
    let mean = codes.iter().map(|code| u64::from(*code)).sum::<u64>() / codes.len() as u64;
    // Encoded pictures may ring into the footroom near black, so the hard
    // bound is the legal 10-bit range; the picture itself must sit in the
    // nominal 64..=940 range. A full-range or bit-shifted export fails both.
    let nominal = codes
        .iter()
        .filter(|code| (64..=940).contains(*code))
        .count();
    assert!(
        (4..=1019).contains(&low) && (4..=1019).contains(&high),
        "luma {low}..{high} uses reserved codes"
    );
    assert!(
        nominal * 100 >= codes.len() * 98,
        "only {nominal} of {} luma codes are in the nominal range ({low}..{high})",
        codes.len()
    );
    assert!(
        !expect_picture || high - low >= 64,
        "luma {low}..{high} is a flat picture"
    );
    serde_json::json!({"min":low,"max":high,"mean":mean})
}

#[tokio::test]
async fn encoded_vod_manager_create_resolves_real_recipe_and_served_codecs() {
    use plurx_core::store::SqliteStore;
    use tokio::io::AsyncReadExt;
    let base = crate::test_tempdir().expect("manager fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            file.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("attested source metadata");
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let req = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: Some("qualification-vod".into()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        ..reopen_request(file_id, "encoded-manager", "unused", "unused")
    };
    let missing = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        file_id: i64::MAX,
        request_id: Some("qualification-vod-missing".into()),
        ..req.clone()
    };
    assert!(create_local_vod(&manager, &missing, "test").await.is_err());
    assert_eq!(
        manager.codec_qualification_encoder_count(Encoder::Software, OutputGrade::Sdr),
        0,
        "a VOD create that failed before reader attachment was counted"
    );
    assert_eq!(manager.codec_qualification_pipeline_count(Pipeline::Cpu), 0);
    let start = create_local_vod(&manager, &req, "test")
        .await
        .expect("public encoded create");
    assert_eq!(
        manager.codec_qualification_encoder_count(Encoder::Software, OutputGrade::Sdr),
        1,
        "one attached encoded-VOD reader must count once"
    );
    assert_eq!(manager.codec_qualification_pipeline_count(Pipeline::Cpu), 1);
    let replay = create_local_vod(&manager, &req, "test")
        .await
        .expect("idempotent encoded create");
    assert_eq!(replay.session_id, start.session_id);
    assert_eq!(replay.encoder, start.encoder);
    assert_eq!(replay.grade, start.grade);
    assert_eq!(replay.processed_dv_profile, start.processed_dv_profile);
    assert_eq!(
        manager.codec_qualification_encoder_count(Encoder::Software, OutputGrade::Sdr),
        1,
        "an idempotent VOD replay must not count a second start"
    );
    assert_eq!(manager.codec_qualification_pipeline_count(Pipeline::Cpu), 1);
    assert_eq!(start.target_height, 240);
    assert_eq!(start.kind, SessionKind::Transcode { height: 240 });
    assert_eq!(start.encoder, Encoder::Software.label());
    assert!(start.vod);
    let mut bytes = Vec::new();
    let mut init_len = None;
    for name in ["init.mp4", "seg00000.m4s"] {
        let response = manager
            .vod_segment(&start.session_id, name)
            .await
            .expect("VOD route");
        let mut ready = response
            .result
            .expect("materialized")
            .expect("planned resource");
        ready
            .file
            .read_to_end(&mut bytes)
            .await
            .expect("actual GET file");
        if init_len.is_none() {
            init_len = Some(bytes.len());
        }
    }
    let served_init = {
        let mut reader = plurx_core::fmp4::FragmentReader::new();
        reader.push(&bytes[..init_len.expect("init served")]);
        match reader.next_unit() {
            Ok(Some(plurx_core::fmp4::Unit::Init(init))) => init,
            other => panic!("the served init.mp4 is one init unit: {other:?}"),
        }
    };
    let served_video = plurx_core::fmp4::avc_rfc6381_codec(&served_init)
        .expect("valid served AVC sample entry")
        .expect("the served init carries AVC");
    let output = base.path().join("manager-get.mp4");
    tokio::fs::write(&output, bytes)
        .await
        .expect("served bytes");
    let decoded = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
        .arg(output)
        .args(["-f", "null", "-"])
        .kill_on_drop(true)
        .output()
        .await
        .expect("decode actual GET");
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let HlsPresentationResolution::Ready(context, file, _) = manager
        .hls_presentation_before(&start.session_id, Instant::now() + Duration::from_secs(5))
        .await
    else {
        panic!("frozen HLS presentation");
    };
    // No node-local SDR AVC experiment qualifies this manager (default caps,
    // and no qualification cell is 240p), so the frozen context defers the
    // profile and level to the init it serves: it names the bare sample
    // entry, which the HLS wrapper replaces with the served init's
    // `avc1.PPCCLL` (`http::hls` reads the same init object). The served
    // half is checked on the bytes this manager actually returned.
    assert_eq!(
        context.codecs.split(',').next(),
        Some("avc1"),
        "an unqualified SDR encode defers its AVC profile to the served init: {}",
        context.codecs
    );
    assert!(
        served_video.starts_with("avc1."),
        "the served init names its RFC 6381 AVC profile: {served_video}"
    );
    assert_eq!(file.height, Some(240));
    assert_eq!(file.hdr, None);
    assert_eq!(context.frame_rate, Some(24.0));
    manager.stop_session(&start.session_id, "test").await;
}

/// A catalog row that reports the derived E-AC-3 Atmos profile, read on a node
/// whose FFprobe does not report it, must still start an encoded session. The
/// comparator regression proves the verdict; this proves the session path that
/// turned the verdict into an HTTP 409 on the tablet.
///
/// The fixture carries real E-AC-3 audio rather than an edited JSON document,
/// because the whole class exists where one reporter derives a property from a
/// bitstream flag and another does not.
#[tokio::test]
async fn encoded_vod_manager_admits_a_reported_eac3_atmos_profile_the_node_omits() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("manager fixture");
    let source = plurx_core::testfixtures::source_with_eac3_audio();

    let held = plurx_core::scan::probe::probe(&source)
        .await
        .expect("held source probe");
    let mut document: serde_json::Value =
        serde_json::from_str(held.raw_json.as_deref().expect("raw probe")).expect("probe JSON");
    {
        let stream = document["streams"]
            .as_array_mut()
            .expect("streams")
            .iter_mut()
            .find(|stream| stream["codec_name"] == "eac3")
            .expect("E-AC-3 stream");
        assert!(
            stream.get("profile").is_none(),
            "this FFprobe already derives the profile, so the fixture cannot pose as a legacy \
             report: {stream}"
        );
        stream["profile"] = serde_json::json!("Dolby Digital Plus + Dolby Atmos");
    }
    let reported = plurx_core::domain::ProbeResult {
        raw_json: Some(document.to_string()),
        ..held.clone()
    };
    let changed_channels = {
        let mut document: serde_json::Value =
            serde_json::from_str(held.raw_json.as_deref().expect("raw probe")).expect("probe JSON");
        let stream = document["streams"]
            .as_array_mut()
            .expect("streams")
            .iter_mut()
            .find(|stream| stream["codec_name"] == "eac3")
            .expect("E-AC-3 stream");
        stream["channels"] = serde_json::json!(8);
        plurx_core::domain::ProbeResult {
            raw_json: Some(document.to_string()),
            ..held.clone()
        }
    };

    let metadata = std::fs::metadata(&source).expect("source metadata");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    for (probe, admitted) in [(reported, true), (changed_channels, false)] {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let file_id =
            seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
        let file = store
            .get_file(file_id)
            .await
            .expect("file")
            .expect("seeded");
        store
            .upsert_file(
                file.item_id,
                source.to_str().expect("path"),
                metadata.len() as i64,
                mtime,
                &probe,
            )
            .await
            .expect("stored catalog probe");
        let manager = TranscodeManager::new(
            store,
            base.path().join(if admitted { "admit" } else { "refuse" }),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let request = SessionRequest {
            dv_processing_destination: None,
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            request_id: None,
            previous_session_id: None,
            reopen_reason: None,
            presentation: Presentation::Vod,
            automatic: false,
            start_seconds: 0.0,
            kind: SessionKind::Transcode { height: 240 },
            ..reopen_request(file_id, "eac3-atmos", "unused", "unused")
        };
        match (create_local_vod(&manager, &request, "test").await, admitted) {
            (Ok(start), true) => {
                assert_eq!(start.kind, SessionKind::Transcode { height: 240 });
                manager.stop_session(&start.session_id, "test").await;
            }
            (Err(error), true) => {
                panic!("a reporting-only profile difference must not refuse playback: {error}")
            }
            (Ok(_), false) => panic!("a changed channel count must still refuse playback"),
            (Err(error), false) => {
                assert!(error.contains("vod_source_rescan_required"), "{error}");
                // The refusal has to say what disagreed and what repairs it.
                // "rescan" was the old advice and could not work: the scanner
                // skips a file whose size and mtime are unchanged.
                assert!(error.contains("/channels"), "{error}");
                assert!(error.contains("reanalyze this item"), "{error}");
                assert!(!error.contains(source.to_str().expect("path")), "{error}");
            }
        }
    }
}

#[tokio::test]
async fn encoded_vod_manager_refuses_replaced_source_with_stale_probe() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("stale source fixture");
    let source = base.path().join("source.nut");
    let replacement = base.path().join("replacement.nut");
    for (path, geometry) in [(&source, "64x64"), (&replacement, "128x32")] {
        let output = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=black:s={geometry}:r=24"),
                "-t",
                "2",
                "-c:v",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-threads",
                "1",
                "-f",
                "nut",
            ])
            .arg(path)
            .kill_on_drop(true)
            .output()
            .await
            .expect("generate source");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let source_len = std::fs::metadata(&source).expect("source size").len();
    let replacement_len = std::fs::metadata(&replacement)
        .expect("replacement size")
        .len();
    if source_len != replacement_len {
        use std::io::Write;
        let (shorter, difference) = if source_len < replacement_len {
            (&source, replacement_len - source_len)
        } else {
            (&replacement, source_len - replacement_len)
        };
        std::fs::OpenOptions::new()
            .append(true)
            .open(shorter)
            .expect("shorter fixture")
            .write_all(&vec![0; difference as usize])
            .expect("equalize fixture length");
    }
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("original probe");
    let metadata = std::fs::metadata(&source).expect("original metadata");
    assert_eq!(
        metadata.len(),
        std::fs::metadata(&replacement)
            .expect("replacement metadata")
            .len(),
        "fixture must evade the scanner's size field"
    );
    let modified = metadata.modified().expect("original mtime");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            file.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("stored original probe");
    std::fs::rename(replacement, &source).expect("replace source");
    std::fs::File::options()
        .write(true)
        .open(&source)
        .expect("replacement handle")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("preserve scanner mtime");

    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let request = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: None,
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 32 },
        ..reopen_request(file_id, "stale-probe", "unused", "unused")
    };
    let error = match create_local_vod(&manager, &request, "test").await {
        Ok(_) => panic!("stale scan facts must not create a recipe"),
        Err(error) => error,
    };
    assert!(error.contains("vod_source_rescan_required"), "{error}");
    assert!(manager.vod.session_ids().await.is_empty());
}

/// A stored track that is `empty` means there is nothing to burn: the session
/// start builds its pipeline without the subtitle overlay, and the recipe is
/// the one a session that never asked for a burn gets. With no store the same
/// request reads the source and burns, which is the control.
#[cfg(unix)]
#[tokio::test]
async fn an_empty_stored_track_starts_an_encoded_session_without_the_overlay() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("manager fixture");
    let video = plurx_core::testfixtures::source("h264");
    let authored = base.path().join("authored.sup");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/mkpgs");
    let status = std::process::Command::new(script)
        .args(["1920", "1080"])
        .arg(&authored)
        .output()
        .expect("author the PGS track");
    assert!(status.status.success());
    let source = base.path().join("with-pgs.mkv");
    let muxed = std::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(&video)
        .args(["-itsoffset", "1", "-i"])
        .arg(&authored)
        .args(["-map", "0", "-map", "1:s:0", "-c", "copy"])
        .arg(&source)
        .output()
        .expect("mux the fixture");
    assert!(
        muxed.status.success(),
        "{}",
        String::from_utf8_lossy(&muxed.stderr)
    );
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let seeded = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            seeded.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            crate::fragment_index_cluster::source_stamp(&metadata).mtime,
            &probe,
        )
        .await
        .expect("attested source metadata");
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("attested");
    assert!(
        file.subtitle_streams
            .first()
            .is_some_and(|stream| plurx_core::tracks::is_bitmap_subtitle(&stream.codec)),
        "the fixture carries a PGS track: {:?}",
        file.subtitle_streams
    );
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let req = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: Some("stored-empty-burn".into()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        subtitle_burn: Some(0),
        ..reopen_request(file_id, "stored-empty", "unused", "unused")
    };

    // First, while the burn cache is cold: a published sidecar is served as
    // it always was, so the store is only asked when there is a read to save.
    let root = crate::subtitle_source::store_root(manager.runtime_cache_dir());
    let dir = crate::subtitle_source::testing::write_manifest(
        &root,
        file_id,
        crate::subtitle_source::testing::stamp_of(&source),
        vec![crate::subtitle_source::testing::settled(
            0,
            crate::subtitle_source::Verdict::Empty,
        )],
    );
    let bare = manager
        .prepare_vod_encoding(&req, &file)
        .await
        .expect("an empty track starts")
        .expect("still an encoded recipe");
    assert!(bare.options.subtitle_burn.is_none(), "nothing is burned");
    assert!(bare.options.subtitle_file.is_none());
    assert!(bare.subtitle.is_none() && bare.subtitle_digest.is_none());
    let bare_args = bare.args(&file, 0.0, 2.0);
    assert!(
        !bare_args
            .iter()
            .any(|arg| arg == "/dev/fd/5" || arg.contains("overlay")),
        "no subtitle input and no overlay filter: {bare_args:?}"
    );
    let plain = manager
        .prepare_vod_encoding(
            &SessionRequest {
                dv_processing_destination: None,
                sdr_master_codecs: None,
                continuous_media: None,
                quality_catalog: None,
                candidate_context: None,
                vod_only: false,
                passive_vod: false,
                finite_bitrate_limit_bps: None,
                request_id: Some("stored-empty-plain".into()),
                subtitle_burn: None,
                ..req.clone()
            },
            &file,
        )
        .await
        .expect("a plain start")
        .expect("an encoded recipe");
    assert_eq!(
        bare_args,
        plain.args(&file, 0.0, 2.0),
        "nothing to burn is exactly a session without a burn"
    );

    // The control: the store taken away, the same request burns.
    std::fs::remove_dir_all(&dir).expect("empty the store");
    // Every child this start waits on runs at the realtime class (plan P-02
    // §3.2.2, review of #518 finding 1): the held source probe under its
    // five-second bound, and the burn extraction it joins for
    // `SIDECAR_JOIN_BUDGET`.
    let spawns_of = crate::process_control::priority::spawns_of;
    let realtime_burn =
        crate::process_control::ChildWork::realtime("burned-subtitle track extraction");
    let held_before = spawns_of(super::VOD_START_HELD_PROBE);
    let burn_before = spawns_of(realtime_burn);
    assert_eq!(
        super::VOD_START_HELD_PROBE.class,
        crate::process_control::ChildClass::Realtime
    );
    let burned = manager
        .prepare_vod_encoding(
            &SessionRequest {
                dv_processing_destination: None,
                sdr_master_codecs: None,
                continuous_media: None,
                quality_catalog: None,
                candidate_context: None,
                vod_only: false,
                passive_vod: false,
                finite_bitrate_limit_bps: None,
                request_id: Some("stored-empty-control".into()),
                ..req.clone()
            },
            &file,
        )
        .await
        .expect("the control burns")
        .expect("an encoded recipe");
    assert!(burned.options.subtitle_burn.is_some());
    assert!(burned.subtitle.is_some());
    assert!(
        spawns_of(super::VOD_START_HELD_PROBE) > held_before,
        "the start's held source probe was not started at the realtime class"
    );
    assert!(
        spawns_of(realtime_burn) > burn_before,
        "the burn extraction the start joined was not started at the realtime class"
    );
    let burned_args = burned.args(&file, 0.0, 2.0);
    assert!(
        burned_args.iter().any(|arg| arg == "/dev/fd/5"),
        "the control reads its sidecar: {burned_args:?}"
    );
    assert!(
        burned_args.iter().any(|arg| arg.contains("overlay")),
        "and overlays it: {burned_args:?}"
    );
}

/// The burn sidecar is fetched only after the encoder and grade are chosen,
/// because that choice can refuse the source outright. A refusal must be
/// immediate and final — not a detached full-source extraction answered
/// "pending" for as long as it takes, followed by the same refusal. Here the
/// source is labelled Dolby Vision with no profile, which encoder selection
/// refuses; the subtitle cache must still be untouched afterwards.
#[cfg(unix)]
#[tokio::test]
async fn a_source_encoder_selection_refuses_is_refused_before_any_burn_extraction() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("manager fixture");
    let video = plurx_core::testfixtures::source("h264");
    let authored = base.path().join("authored.sup");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/mkpgs");
    let status = std::process::Command::new(script)
        .args(["1920", "1080"])
        .arg(&authored)
        .output()
        .expect("author the PGS track");
    assert!(status.status.success());
    let source = base.path().join("with-pgs.mkv");
    let muxed = std::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(&video)
        .args(["-itsoffset", "1", "-i"])
        .arg(&authored)
        .args(["-map", "0", "-map", "1:s:0", "-c", "copy"])
        .arg(&source)
        .output()
        .expect("mux the fixture");
    assert!(
        muxed.status.success(),
        "{}",
        String::from_utf8_lossy(&muxed.stderr)
    );
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let seeded = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            seeded.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            crate::fragment_index_cluster::source_stamp(&metadata).mtime,
            &probe,
        )
        .await
        .expect("attested source metadata");
    let mut file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("attested");
    // What encoder selection refuses: Dolby Vision whose profile is unknown.
    file.hdr = Some("dolby_vision".into());
    file.hdr_format = None;
    file.dolby_vision = Default::default();
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let req = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: Some("refused-before-burn".into()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        subtitle_burn: Some(0),
        ..reopen_request(file_id, "refused-before-burn", "unused", "unused")
    };
    let refusal = match manager.prepare_vod_encoding(&req, &file).await {
        Err(refusal) => refusal,
        Ok(_) => panic!("encoder selection refuses an unknown Dolby Vision profile"),
    };
    assert!(refusal.contains("Dolby Vision"), "{refusal}");
    // Had the fetch run first, the fixture's tiny track would have been
    // extracted and published inside the call, before the refusal returned.
    let cache = manager.subtitle_cache_dir();
    let touched = std::fs::read_dir(cache)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        touched,
        0,
        "a refused start must not have started a burn extraction in {}",
        cache.display()
    );
}

/// A Profile 5 VOD start waits on the per-source Dolby Vision pixel proof
/// (two FFmpeg probes, 30 s each, whose answer is memoized), so the proof runs
/// at the realtime class; the same proof asked for by an offline package,
/// which nobody watches start, runs at the background class (plan P-02
/// §3.2.2, review of #518 finding 1). The fixture is not Dolby Vision, so the
/// proof fails and the start is refused; the class is read from the spawns.
#[cfg(unix)]
#[tokio::test]
async fn the_profile5_pixel_proof_takes_the_class_of_the_caller_waiting_on_it() {
    use crate::process_control::{priority::spawns_of, ChildClass, ChildWork};
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("manager fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let seeded = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            seeded.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            crate::fragment_index_cluster::source_stamp(&metadata).mtime,
            &probe,
        )
        .await
        .expect("attested source metadata");
    let mut file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("attested");
    file.hdr = Some("dolby_vision".into());
    file.hdr_format = Some("Dolby Vision · Profile 5".into());
    file.dolby_vision = Default::default();

    let realtime = ChildWork::realtime("Dolby Vision pixel probe");
    let background = ChildWork::background("Dolby Vision pixel probe");
    let manager = TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("start"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    )
    .with_dovi_reshape(true);
    let req = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: Some("profile5-proof-class".into()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        subtitle_burn: None,
        ..reopen_request(file_id, "profile5-proof-class", "unused", "unused")
    };
    let before = spawns_of(realtime);
    let refusal = match manager.prepare_vod_encoding(&req, &file).await {
        Err(refusal) => refusal,
        Ok(_) => panic!("a source that is not Dolby Vision cannot prove the Profile 5 renderer"),
    };
    assert!(refusal.contains("Dolby Vision"), "{refusal}");
    assert!(
        spawns_of(realtime) >= before + 2,
        "the start's pixel proof did not run at the realtime class"
    );
    assert_eq!(realtime.class, ChildClass::Realtime);

    let offline = TranscodeManager::new(
        store,
        base.path().join("offline"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    )
    .with_dovi_reshape(true);
    let before = spawns_of(background);
    assert!(offline
        .effective_rate_control_for_new_offline_package(&file, 1080, false)
        .await
        .is_err());
    assert!(
        spawns_of(background) >= before + 2,
        "the offline package's pixel proof did not run at the background class"
    );
}

/// `Encoding`'s admission through its production constructor
/// (`TranscodeManager::prepare_vod_encoding`, TRANSCODE-DECOMPOSITION-PLAN
/// §5.9): the no-op hook is ready at once, so asking for a permit reads the
/// pool policy and admits. The race tests that pause this point
/// (`encoded_vod_held_capacity_keeps_cached_gets_open_and_rechecks_seek_after_reap`
/// and the `vod::tests` handoff tests) build their own encodings.
#[tokio::test]
async fn encoding_shipped_shape() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("manager fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let seeded = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            seeded.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("attested source metadata");
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("attested");
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let req = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: Some("encoding-shipped-shape".into()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        ..reopen_request(file_id, "encoding-shipped-shape", "unused", "unused")
    };
    let encoding = manager
        .prepare_vod_encoding(&req, &file)
        .await
        .expect("the production recipe freezes")
        .expect("a transcode freezes an encoding");
    let permit = tokio::time::timeout(Duration::from_secs(5), encoding.try_permit())
        .await
        .expect("the production hook does not hold admission")
        .expect("an idle pool admits the encoding");
    assert!(
        encoding.admissions.software_in_use() > 0,
        "the permit holds software capacity"
    );
    drop(permit);
    assert_eq!(encoding.admissions.software_in_use(), 0);
}

/// S-02: a frozen plan this node's software budget can never admit is
/// refused at create, typed, instead of parking a viewer on the 30 s
/// materialize deadline for a permit no release can grant.
#[tokio::test]
async fn an_encode_the_software_budget_can_never_admit_is_refused_at_create() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("manager fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let seeded = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            seeded.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("attested source metadata");
    // No software encoder budget at all: every software encode needs at
    // least one thread, so no release can ever admit one.
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "0")
        .await
        .expect("software budget");
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("attested");
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let req = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: None,
        quality_catalog: None,
        candidate_context: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        request_id: Some("unadmittable-encode".into()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        ..reopen_request(file_id, "unadmittable-encode", "unused", "unused")
    };
    let refused = manager
        .prepare_vod_encoding(&req, &file)
        .await
        .expect_err("an unadmittable plan is refused before any rendition exists");
    let (code, reason) = crate::transcode::vod_refusal(&refused).expect("typed VOD refusal");
    assert_eq!(code, "vod_transcode_unavailable", "{refused}");
    assert!(reason.contains("software encoder budget is 0"), "{reason}");
    // The public create answers that refusal the way it answers every
    // `vod_transcode_unavailable`: a VOD-only request is refused typed, and an
    // ordinary one is served by the retained live engine at once. Neither
    // parks a VOD rendition on a permit that can never be granted.
    let vod_only = SessionRequest {
        vod_only: true,
        request_id: Some("unadmittable-encode-vod-only".into()),
        ..req.clone()
    };
    let Err(refused) = create_local_vod(&manager, &vod_only, "test").await else {
        panic!("a VOD-only create cannot fall back");
    };
    assert_eq!(
        crate::transcode::vod_refusal(&refused).map(|(code, _)| code),
        Some("vod_transcode_unavailable"),
        "{refused}"
    );
    assert!(manager.vod.session_ids().await.is_empty());
    let recovered = create_local_vod(&manager, &req, "test")
        .await
        .expect("an ordinary create uses the live-HLS recovery");
    assert!(!recovered.vod, "no VOD session was parked");
    assert!(manager.vod.session_ids().await.is_empty());
    manager.stop_session(&recovered.session_id, "test").await;
}

#[tokio::test]
async fn continuous_worker_roles_resolve_video_only_and_one_cpu_soundtrack() {
    // An unoptimized test frame keeps a stack slot for every future it awaits.
    // Build each production entry point in its own short frame and await it
    // boxed, so this walk of every role fits the default 2 MiB test stack.
    type Boxed<'a, T> =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + 'a>>;
    fn create<'a>(
        manager: &'a TranscodeManager,
        request: &'a SessionRequest,
    ) -> Boxed<'a, StartInfo> {
        Box::pin(create_local_vod(manager, request, "test"))
    }
    fn prepare<'a>(
        manager: &'a TranscodeManager,
        request: &'a SessionRequest,
        file: &'a plurx_core::domain::MediaFile,
    ) -> Boxed<'a, Option<Arc<crate::vodencode::Encoding>>> {
        Box::pin(manager.prepare_vod_encoding(request, file))
    }
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("continuous worker fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("source probe");
    let metadata = std::fs::metadata(&source).expect("metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id = seed_file_with_probe_at(
        &store,
        source.to_str().expect("fixture path"),
        probe.clone(),
    )
    .await;
    store
        .upsert_file(
            store
                .get_file(file_id)
                .await
                .expect("read file")
                .expect("seeded file")
                .item_id,
            source.to_str().expect("fixture path"),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("fixture mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("attested file");
    let file = store
        .get_file(file_id)
        .await
        .expect("read file")
        .expect("seeded file");
    // Admission compares each frozen role with the node budget, which
    // otherwise derives from this host's available parallelism: a 3-CPU unit
    // runner derives 2, under the soundtrack's frozen 3-thread floor. Pin a
    // budget that admits both roles so the walk proves role resolution only.
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "8")
        .await
        .expect("software budget");
    // Production planning always has a cache identity and the startup-bound
    // FFprobe; normalized continuous geometry is refused without either.
    let decode_probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
        &crate::ffmpeg::bound_ffprobe_bin(),
    )
    .await
    .expect("bound FFprobe identity");
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    )
    .with_cache(
        base.path().join("cache"),
        "test-ffmpeg".into(),
        "test-node".into(),
    )
    .with_decode_probe(Some(decode_probe));
    let mut request = SessionRequest {
        dv_processing_destination: None,
        sdr_master_codecs: None,
        continuous_media: Some(Box::new(ContinuousMediaRequest {
            controlled: false,
            autonomous_companion: None,
            companion_catalog: None,
            family_descriptor: None,
            companion_context: None,
            version: 1,
            family_generation: uuid::Uuid::new_v4().to_string(),
            role: ContinuousMediaRole::Video,
        })),
        candidate_context: None,
        request_id: Some(uuid::Uuid::new_v4().to_string()),
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        kind: SessionKind::Transcode { height: 240 },
        ..reopen_request(file_id, "continuous-worker", "unused", "unused")
    };
    let planning = manager
        .store
        .playback_planning_snapshot(file.id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("planning snapshot")
        .expect("source");
    // HLS create resolves the concrete soundtrack before it asks for the
    // catalog and carries that same index in the session request.
    request.audio_index =
        TranscodeManager::candidate_audio_from_snapshot(&planning, request.audio_index);
    // A browser declares an audio sink, so the catalog and HLS create both
    // carry an audio claim; the muxed catalog recipe includes its delivery.
    let caps = plurx_core::playback::DeviceCaps {
        v: 2,
        audio: vec!["aac".into()],
        audio_sinks: vec![plurx_core::playback::AudioSink {
            codec: "aac".into(),
            max_channels: 2,
            passthrough: false,
            sample_rates_hz: vec![48_000],
        }],
        ..Default::default()
    };
    request.audio_claim =
        plurx_core::playback::audio::AudioClaim::from_caps(&caps).expect("valid audio sinks");
    assert!(request.audio_claim.is_some());
    let catalog = manager
        .quality_candidates_from_snapshot_progress(
            &planning,
            &caps,
            request.audio_index,
            0,
            None,
            Presentation::Vod,
            None,
            None,
            None,
        )
        .await;
    // A real create binds every selected row to the planning inputs it was
    // built from (`PlanningBinding`); the worker refuses an unbound row.
    let bound = |candidate: &plurx_core::playback::candidate::QualityCandidate| {
        let mut context = TranscodeManager::candidate_context(candidate);
        context.planning_binding =
            Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
        context
    };
    let candidate = catalog
        .iter()
        .find(|candidate| {
            candidate.normalized_geometry
                && candidate.grade == OutputGrade::Sdr
                && candidate.route == plurx_core::playback::candidate::CandidateRoute::Encode
        })
        .expect("normalized SDR catalog candidate");
    request.kind = SessionKind::Transcode {
        height: i64::from(candidate.target_height),
    };
    request.candidate_context = Some(Box::new(bound(candidate)));
    let video = prepare(&manager, &request, &file)
        .await
        .expect("video planning from catalog")
        .expect("video recipe");
    assert!(
        video.candidate_recipe.is_none(),
        "video-only work is not a muxed candidate speed proof"
    );
    assert!(!video.plan.options().input_has_audio);
    assert!(video.shared_audio.is_none());
    assert_eq!(
        video.plan.options().video_sample_envelope,
        plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
    );
    let video_args = video.args(
        &file,
        0.0,
        file.duration_ms.expect("fixture duration") as f64 / 1000.0,
    );
    assert!(video_args.iter().any(|arg| arg == "-an"));
    assert!(!video_args.iter().any(|arg| arg == "aac"));
    request
        .continuous_media
        .as_mut()
        .expect("continuous role")
        .role = ContinuousMediaRole::SharedAudio;
    let audio = prepare(&manager, &request, &file)
        .await
        .expect("audio planning")
        .expect("audio recipe");
    assert!(audio.shared_audio.is_some());
    assert!(
        audio.candidate_recipe.is_none(),
        "shared AAC is not a muxed candidate speed proof"
    );
    assert!(!audio.resources().hardware_slot);
    assert_eq!(audio.resources().cpu_threads, 3);
    assert_eq!(
        audio
            .media_plan(file.duration_ms.expect("fixture duration"))
            .timescale,
        48_000
    );
    let audio_args = audio.args(
        &file,
        0.0,
        file.duration_ms.expect("fixture duration") as f64 / 1000.0,
    );
    assert!(audio_args.iter().any(|arg| arg == "-vn"));
    assert!(audio_args.iter().any(|arg| arg == "aac"));
    for encoding in [&video, &audio] {
        let plan = encoding.media_plan(file.duration_ms.expect("source duration"));
        let peak = encoding
            .continuous_peak_bps(&plan)
            .expect("container-inclusive ceiling");
        assert!(peak > 0);
        let sustained = encoding
            .continuous_sustained_bps()
            .expect("the rate a link must carry");
        assert!(
            sustained > 0 && sustained < peak,
            "a declared link rate never exceeds the publication ceiling"
        );
        for (index, entry) in plan.entries.iter().enumerate() {
            let bound = u64::try_from(
                u128::from(peak) * u128::from(entry.duration_ticks)
                    / (8 * u128::from(plan.timescale)),
            )
            .expect("entry bytes");
            assert_eq!(
                encoding.continuous_object_fits(&plan, index as u32, bound),
                Some(true)
            );
            assert_eq!(
                encoding.continuous_object_fits(&plan, index as u32, bound + 1),
                Some(false)
            );
        }
    }
    assert_eq!(
        video.continuous_sustained_bps(),
        Some(plurx_core::transcode::vbv_maxrate_bps(
            video.options.video_bitrate_kbps
        )),
        "video costs its VBV cap, as the catalog advertises the encode route"
    );
    assert_eq!(
        audio.continuous_sustained_bps(),
        Some(u64::from(audio.options.audio_bitrate_kbps) * 1000)
    );
    request
        .continuous_media
        .as_mut()
        .expect("continuous role")
        .role = ContinuousMediaRole::Video;
    request.candidate_context = None;
    let mut incumbent_request = request.clone();
    incumbent_request.continuous_media = None;
    incumbent_request.request_id = Some(uuid::Uuid::new_v4().to_string());
    let incumbent = create(&manager, &incumbent_request)
        .await
        .expect("healthy incumbent");
    manager
        .store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "3")
        .await
        .expect("audio-only capacity");
    // An oversized family may own an idle pool exclusively. Make this
    // refusal phase genuinely non-idle without changing the family budget.
    let unrelated_live = video
        .admissions
        .try_admit_software(3, 1, crate::admission::Priority::Live)
        .expect("unrelated live CPU credit");
    let refused = create(&manager, &request)
        .await
        .err()
        .expect("the whole video/audio group does not fit");
    assert!(refused.contains("vod_family_capacity"));
    assert!(
        manager
            .vod
            .playlist(&incumbent.session_id)
            .await
            .expect("incumbent remains registered")
            .result
            .is_ok(),
        "failed family admission must not run the legacy supersession sweep"
    );
    let companion = catalog
        .iter()
        .find(|row| {
            row.id != candidate.id
                && row.normalized_geometry
                && row.target_height != candidate.target_height
                && row.grade == OutputGrade::Sdr
                && row.route == plurx_core::playback::candidate::CandidateRoute::Encode
        })
        .expect("second canonical video recipe");
    // The capacity setting above is a planning input, so the family is
    // bound to the snapshot taken after it, as a real create would be.
    let planning = manager
        .store
        .playback_planning_snapshot(file.id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("planning snapshot")
        .expect("source");
    let bound = |candidate: &plurx_core::playback::candidate::QualityCandidate| {
        let mut context = TranscodeManager::candidate_context(candidate);
        context.planning_binding =
            Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
        context
    };
    let mut family = request.clone();
    family.request_id = Some(uuid::Uuid::new_v4().to_string());
    family.candidate_context = Some(Box::new(bound(candidate)));
    let media = family.continuous_media.as_mut().expect("family role");
    media.autonomous_companion = Some(companion.id);
    media.companion_catalog = Some(Box::new(companion.clone()));
    media.companion_context = Some(Box::new(ContinuousCompanionContext {
        height: i64::from(companion.target_height),
        candidate: bound(companion),
    }));
    let refused = create(&manager, &family)
        .await
        .err()
        .expect("three-role capacity denial");
    assert!(
        refused.contains("vod_family_capacity"),
        "both derived video and AAC roles validate before admission: {refused}"
    );
    assert!(manager
        .vod
        .playlist(&incumbent.session_id)
        .await
        .expect("incumbent survives family denial")
        .result
        .is_ok());
    drop(unrelated_live);
    manager
        .vod
        .end(&incumbent.session_id, crate::vodserve::Terminal::Deleted)
        .await;
    request
        .continuous_media
        .as_mut()
        .expect("continuous role")
        .version = 2;
    assert!(prepare(&manager, &request, &file).await.is_err());
}

#[tokio::test]
async fn service_vod_only_refuses_unindexed_copy_before_rolling_allocation() {
    use plurx_core::store::{keys, SqliteStore};
    let base = crate::test_tempdir().expect("policy fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("probe");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id = seed_file_with_probe_at(&store, source.to_str().expect("path"), probe).await;
    store
        .put_setting(keys::VOD_LIVE_RECOVERY, "true")
        .await
        .expect("recovery on");
    let manager = TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let mut request = reopen_request(file_id, "service-policy", "unused", "unused");
    request.request_id = Some("service-vod-only".into());
    request.previous_session_id = None;
    request.reopen_reason = None;
    request.presentation = Presentation::Vod;
    request.kind = SessionKind::Copy {
        aac: false,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
    };
    request.vod_only = true;
    let error = create_local_vod(&manager, &request, "test")
        .await
        .err()
        .expect("unindexed VOD refusal");
    assert_eq!(
        vod_refusal(&error).map(|(code, _)| code),
        Some("vod_index_pending"),
        "{error}"
    );
    assert!(
        manager.active_session_ids().await.is_empty(),
        "no rolling worker was allocated"
    );
    assert!(
        manager.vod.session_ids().await.is_empty(),
        "no VOD reader was attached"
    );
    assert!(
        manager.live_hls_recovery_enabled().await.expect("setting"),
        "service policy must not alter native recovery"
    );
    request.presentation = Presentation::Live;
    request.request_id = Some("service-forbidden-live".into());
    let error = create_local_vod(&manager, &request, "test")
        .await
        .err()
        .expect("explicit live also refused");
    assert_eq!(
        vod_refusal(&error).map(|(code, _)| code),
        Some("vod_source_unsupported")
    );
    assert!(manager.active_session_ids().await.is_empty());
}

/// The interval the dispatch revalidation does not cover (startup and
/// tone-map handoff §5.4, B-M2): the source is replaced after the worker's
/// VOD attempt has read it and refused (no index), and before the rolling
/// fallback starts. The fallback must start from the replaced row's facts.
/// On this legacy route the rolling start's own source read is the only
/// thing that sees the change, so a build that hands it the VOD attempt's
/// row fails here. (A candidate route re-validates its carried snapshot at
/// the fallback instead, and refuses a changed source.)
#[tokio::test]
async fn rolling_fallback_starts_from_a_source_replaced_after_the_vod_attempt() {
    use plurx_core::store::{keys, SqliteStore};
    use plurx_core::testfixtures::{ffmpeg, run};
    let base = crate::test_tempdir().expect("fallback fixture");
    let path = base.path().join("replaced.mkv");
    std::fs::copy(plurx_core::testfixtures::source("h264"), &path).expect("source copy");
    let path_text = path.to_str().expect("path").to_owned();
    let probe = plurx_core::scan::probe::probe(&path).await.expect("probe");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id = seed_file_with_probe_at(&store, &path_text, probe).await;
    store
        .put_setting(keys::VOD_LIVE_RECOVERY, "true")
        .await
        .expect("recovery on");
    let before = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    let manager = Arc::new(TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let mut request = reopen_request(file_id, "fallback-interval", "unused", "unused");
    request.request_id = Some("fallback-interval".into());
    request.previous_session_id = None;
    request.reopen_reason = None;
    request.start_seconds = 0.0;
    request.presentation = Presentation::Vod;
    request.kind = SessionKind::Copy {
        aac: false,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
    };
    let pause = manager
        .test_hooks()
        .vod_fallback_start
        .arm("before the rolling fallback start");
    let mut creating = {
        let manager = Arc::clone(&manager);
        let request = request.clone();
        tokio::spawn(async move { create_local_vod(&manager, &request, "test").await })
    };
    // A create that ends before the fallback (served as VOD, or refused
    // outright) never reaches the pause; say which instead of timing out.
    let held = tokio::select! {
        held = pause.reached() => held,
        ended = &mut creating => panic!(
            "the create ended before the rolling fallback: {:?}",
            ended.map(|result| result.map(|info| info.vod))
        ),
    };
    // The VOD attempt read the original row and refused for want of an
    // index. Replace the file on disk and rescan it, as the scanner would.
    let replacement = base.path().join("replacement.mkv");
    run(std::process::Command::new(ffmpeg())
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=320x180:rate=24:duration=6")
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000:duration=6",
        ])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-ac",
            "2",
            "-shortest",
            "-f",
            "matroska",
        ])
        .arg(&replacement));
    std::fs::rename(&replacement, &path).expect("replace the source");
    let metadata = std::fs::metadata(&path).expect("replaced metadata");
    let replaced_probe = plurx_core::scan::probe::probe(&path)
        .await
        .expect("replaced probe");
    store
        .upsert_file(
            before.item_id,
            &path_text,
            i64::try_from(metadata.len()).expect("size"),
            before.mtime + 60,
            &replaced_probe,
        )
        .await
        .expect("rescan");
    let after = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("rescanned");
    assert_ne!(
        after.size, before.size,
        "the replacement is a different file"
    );
    assert_ne!(after.duration_ms, before.duration_ms);
    assert_ne!(after.height, before.height);
    held.release();
    let started = creating
        .await
        .expect("create task")
        .expect("the index miss falls back to a rolling start");
    assert!(
        !started.vod,
        "the VOD attempt refused and the rolling engine served"
    );
    assert_eq!(
        started.duration_ms, after.duration_ms,
        "the rolling start took the replaced row's probe facts, not the ones the VOD attempt read"
    );
    assert_eq!(
        Some(started.target_height),
        after.height,
        "a copy reports the replaced source's own height"
    );
    assert!(manager.stop_session(&started.session_id, "test").await);
}

/// The same interval on a candidate route. Its rolling start reuses the
/// snapshot its plan was accepted from, so the fallback re-validates that
/// binding first: a source replaced after the VOD attempt refuses the
/// fallback as stale execution instead of starting it from the old facts.
#[tokio::test]
async fn a_candidate_rolling_fallback_refuses_a_source_replaced_after_the_vod_attempt() {
    use plurx_core::store::{keys, SqliteStore};
    use plurx_core::testfixtures::{ffmpeg, run};
    let base = crate::test_tempdir().expect("fallback fixture");
    let path = base.path().join("replaced.mkv");
    std::fs::copy(plurx_core::testfixtures::source("h264"), &path).expect("source copy");
    let path_text = path.to_str().expect("path").to_owned();
    let probe = plurx_core::scan::probe::probe(&path).await.expect("probe");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id = seed_file_with_probe_at(&store, &path_text, probe).await;
    store
        .put_setting(keys::VOD_LIVE_RECOVERY, "true")
        .await
        .expect("recovery on");
    let manager = Arc::new(TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let planning = store
        .playback_planning_snapshot(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("planning")
        .expect("seeded");
    let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
        "v": 2,
        "video": [{"codec": "h264", "decode": true, "present": ["sdr"]}],
        "audio": ["aac"],
        "transports": ["hls"]
    }))
    .expect("caps");
    let rows = manager
        .quality_candidates_from_snapshot_progress(
            &planning,
            &caps,
            None,
            0,
            None,
            Presentation::Vod,
            // The copy flags the request below carries, so the candidate's
            // recipe is the one its dispatch will rebuild.
            Some((false, false, false)),
            None,
            None,
        )
        .await;
    // The copy candidate (Remux for this h264 source), so the request is
    // the copy that has no VOD index and falls back.
    let copy = rows
        .iter()
        .find(|row| {
            matches!(
                row.route,
                plurx_core::playback::candidate::CandidateRoute::Remux
                    | plurx_core::playback::candidate::CandidateRoute::Original
            )
        })
        .unwrap_or_else(|| panic!("a copy candidate: {rows:?}"));
    let mut context = TranscodeManager::candidate_context(copy);
    // Bind the context to the exact copy recipe the dispatch rebuilds from
    // this source, executable and engine (the equality the VOD attempt
    // checks), so the attempt reaches its index refusal and falls back.
    let source = crate::fragment_index_cluster::open_source_fence(&planning.file, None)
        .await
        .expect("source fence");
    let executable = crate::ffmpeg::EncodedExecutable::capture()
        .await
        .expect("executable");
    let engine = crate::ffmpeg::EncodedEngine::capture(None)
        .await
        .expect("engine");
    let raster = (
        u32::try_from(planning.file.width.expect("width")).expect("width"),
        u32::try_from(planning.file.height.expect("height")).expect("height"),
    );
    context.recipe_digest = super::manager_candidates::copy_candidate_recipe_digest(
        &planning.file,
        None,
        0,
        None,
        (false, false, false),
        Some(source.object_version()),
        Some(&executable.digest),
        Some(&engine.digest),
        raster,
    );
    context.candidate_id =
        plurx_core::playback::candidate::CandidateId::for_recipe_digest(context.recipe_digest);
    context.canonical_caps = Some(caps);
    context.planning_binding = Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
    context.planning_snapshot = Some(Arc::new(planning.clone()));
    let mut request = reopen_request(file_id, "candidate-fallback", "unused", "unused");
    request.request_id = Some("candidate-fallback".into());
    request.previous_session_id = None;
    request.reopen_reason = None;
    request.start_seconds = 0.0;
    request.presentation = Presentation::Vod;
    request.kind = SessionKind::Copy {
        aac: false,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
    };
    request.candidate_context = Some(Box::new(context));
    let pause = manager
        .test_hooks()
        .vod_fallback_start
        .arm("before the rolling fallback start");
    let mut creating = {
        let manager = Arc::clone(&manager);
        let request = request.clone();
        tokio::spawn(async move { create_local_vod(&manager, &request, "test").await })
    };
    let held = tokio::select! {
        held = pause.reached() => held,
        ended = &mut creating => panic!(
            "the create ended before the rolling fallback: {:?}",
            ended.map(|result| result.map(|info| info.vod))
        ),
    };
    let replacement = base.path().join("replacement.mkv");
    run(std::process::Command::new(ffmpeg())
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=320x180:rate=24:duration=6")
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:sample_rate=48000:duration=6",
        ])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-ac",
            "2",
            "-shortest",
            "-f",
            "matroska",
        ])
        .arg(&replacement));
    std::fs::rename(&replacement, &path).expect("replace the source");
    let metadata = std::fs::metadata(&path).expect("replaced metadata");
    let replaced_probe = plurx_core::scan::probe::probe(&path)
        .await
        .expect("replaced probe");
    store
        .upsert_file(
            planning.file.item_id,
            &path_text,
            i64::try_from(metadata.len()).expect("size"),
            planning.file.mtime + 60,
            &replaced_probe,
        )
        .await
        .expect("rescan");
    held.release();
    let error = creating
        .await
        .expect("create task")
        .err()
        .expect("a changed source refuses the fallback");
    assert!(
        error.contains("candidate source or settings changed"),
        "the fallback refused as stale execution: {error}"
    );
    assert!(
        manager.active_session_ids().await.is_empty(),
        "no rolling producer started from the old facts"
    );
}

/// A processed Auto candidate is an enhancement of the ordinary request the
/// create already resolved. Dispatched without its prepared graph (here: none
/// was carried, as after an expired or consumed `PreparedCandidateOnce`), it
/// serves that ordinary request and says why, on both engines the create may
/// end on; only a processed candidate the client named itself is refused, and
/// the rolling engine refuses one whatever path hands it over.
#[tokio::test]
async fn a_processed_auto_candidate_that_cannot_be_consumed_degrades_to_its_ordinary_request() {
    use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
    use plurx_core::store::{keys, SqliteStore};
    let base = crate::test_tempdir().expect("degrade fixture");
    let path = base.path().join("ordinary.mkv");
    std::fs::copy(plurx_core::testfixtures::source("h264"), &path).expect("source copy");
    let path_text = path.to_str().expect("path").to_owned();
    let probe = plurx_core::scan::probe::probe(&path).await.expect("probe");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id = seed_file_with_probe_at(&store, &path_text, probe).await;
    store
        .put_setting(keys::VOD_LIVE_RECOVERY, "true")
        .await
        .expect("recovery on");
    let manager = Arc::new(TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let planning = store
        .playback_planning_snapshot(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("planning")
        .expect("seeded");
    let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
        "v": 2,
        "video": [{"codec": "h264", "decode": true, "present": ["sdr"]}],
        "audio": ["aac"],
        "transports": ["hls"]
    }))
    .expect("caps");
    let rows = manager
        .quality_candidates_from_snapshot_progress(
            &planning,
            &caps,
            None,
            0,
            None,
            Presentation::Vod,
            Some((false, false, false)),
            None,
            None,
        )
        .await;
    let copy = rows
        .iter()
        .find(|row| matches!(row.route, CandidateRoute::Remux | CandidateRoute::Original))
        .unwrap_or_else(|| panic!("a copy candidate: {rows:?}"));
    // The ordinary request: the bound copy candidate, exactly as the
    // candidate fallback test above dispatches it.
    let mut ordinary_context = TranscodeManager::candidate_context(copy);
    let source = crate::fragment_index_cluster::open_source_fence(&planning.file, None)
        .await
        .expect("source fence");
    let executable = crate::ffmpeg::EncodedExecutable::capture()
        .await
        .expect("executable");
    let engine = crate::ffmpeg::EncodedEngine::capture(None)
        .await
        .expect("engine");
    let raster = (
        u32::try_from(planning.file.width.expect("width")).expect("width"),
        u32::try_from(planning.file.height.expect("height")).expect("height"),
    );
    ordinary_context.recipe_digest = super::manager_candidates::copy_candidate_recipe_digest(
        &planning.file,
        None,
        0,
        None,
        (false, false, false),
        Some(source.object_version()),
        Some(&executable.digest),
        Some(&engine.digest),
        raster,
    );
    ordinary_context.candidate_id = CandidateId::for_recipe_digest(ordinary_context.recipe_digest);
    ordinary_context.canonical_caps = Some(caps.clone());
    ordinary_context.planning_binding =
        Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
    ordinary_context.planning_snapshot = Some(Arc::new(planning.clone()));
    let mut ordinary = reopen_request(file_id, "processed-degrade", "unused", "unused");
    ordinary.request_id = Some("processed-degrade".into());
    ordinary.previous_session_id = None;
    ordinary.reopen_reason = None;
    ordinary.start_seconds = 0.0;
    ordinary.automatic = true;
    ordinary.presentation = Presentation::Vod;
    ordinary.kind = SessionKind::Copy {
        aac: false,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
    };
    ordinary.candidate_context = Some(Box::new(ordinary_context));
    // The processed trial qualification would hand on: a Transcode HDR10
    // request bound to a processed candidate, carrying the ordinary request
    // it enhances but no prepared graph to consume.
    let processed_candidate = QualityCandidate {
        id: CandidateId::for_recipe_digest([7; 32]),
        recipe_digest: [7; 32],
        route: CandidateRoute::Encode,
        planned_codec: Some("hevc".into()),
        processed_dv: true,
        normalized_geometry: false,
        width: raster.0,
        height: raster.1,
        target_height: raster.1,
        average_bps: Some(4_000_000),
        peak_bps: Some(6_000_000),
        grade: OutputGrade::Hdr10,
        decoder_compatible: true,
        complete_cache: false,
        sustainable: true,
    };
    let mut processed_context = TranscodeManager::candidate_context(&processed_candidate);
    processed_context.canonical_caps = Some(caps);
    processed_context.planning_binding =
        Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
    processed_context.planning_snapshot = Some(Arc::new(planning.clone()));
    processed_context.owner_node_id = manager.cache_location().map(|(_, node)| node.to_owned());
    processed_context.ordinary_request = Some(Box::new(ordinary.clone()));
    let mut processed = ordinary.clone();
    processed.kind = SessionKind::Transcode {
        height: i64::from(raster.1),
    };
    processed.hdr10 = true;
    processed.dv_processing_destination =
        Some(plurx_core::transcode::dv_processing::DvDestination::Profile81);
    processed.candidate_context = Some(Box::new(processed_context.clone()));

    // The rolling engine never serves a processed candidate, whoever hands
    // it one: the same refusal the catalog restore path gives.
    let refused = manager
        .rolling_start_inputs(file_id, Some(&processed_context))
        .await
        .err()
        .expect("the rolling engine refuses a processed candidate context");
    assert_eq!(
        vod_refusal(&refused).map(|(code, _)| code),
        Some("candidate_recipe_unavailable"),
        "{refused}"
    );

    // Dispatch: the VOD attempt cannot consume the processed graph and
    // degrades to the ordinary copy, which has no index and falls back to
    // the rolling engine; that engine degrades the same way. The viewer gets
    // the ordinary start, with the reason, instead of a terminal refusal.
    let started = create_local_vod(&manager, &processed, "test")
        .await
        .expect("the ordinary request the processed candidate enhanced is served");
    assert!(
        !started.vod,
        "the copy has no VOD index; the rolling engine served it"
    );
    assert!(
        matches!(started.kind, SessionKind::Copy { .. }),
        "the ordinary copy, not the processed transcode: {:?}",
        started.kind
    );
    assert_eq!(started.grade, OutputGrade::Sdr);
    assert_eq!(
        started.degraded_processing.as_deref(),
        Some("the rolling engine cannot serve a processed candidate")
    );
    assert!(manager.stop_session(&started.session_id, "test").await);

    // A client that named the processed candidate itself has no ordinary
    // request to fall back to and is refused, as before.
    let mut named = processed.clone();
    named.request_id = Some("processed-named".into());
    named
        .candidate_context
        .as_mut()
        .expect("processed context")
        .ordinary_request = None;
    let error = create_local_vod(&manager, &named, "test")
        .await
        .err()
        .expect("a client-named processed candidate without its graph is refused");
    assert_eq!(
        vod_refusal(&error).map(|(code, _)| code),
        Some("candidate_recipe_unavailable"),
        "{error}"
    );
    assert!(
        manager.active_session_ids().await.is_empty(),
        "no engine served the refused processed candidate"
    );
}

#[tokio::test]
async fn finite_vod_bitrate_ceiling_is_shared_by_native_recipe_and_worker_identity() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
    let id = seed_file_with_probe_at(
        &store,
        "/finite-budget-fixture.mp4",
        plurx_core::domain::ProbeResult {
            audio_streams: vec![plurx_core::domain::AudioStream {
                index: 0,
                codec: "aac".into(),
                channel_layout: None,
                channels: Some(2),
                sample_rate: Some(48_000),
                language: None,
                title: None,
                default: true,
            }],
            ..Default::default()
        },
    )
    .await;
    let file = store.get_file(id).await.expect("file").expect("source");
    let mut request = reopen_request(id, "finite-player", "unused", "unused");
    request.kind = SessionKind::Transcode { height: 720 };
    request.previous_session_id = None;
    request.reopen_reason = None;
    request.request_id = Some("finite-attempt".into());
    request.presentation = Presentation::Vod;
    let mut options = plurx_core::transcode::TranscodeOptions::default();
    let native_rate = options.video_bitrate_kbps;
    super::manager_create::constrain_finite_vod_rate(&request, &file, &mut options)
        .expect("native unchanged");
    assert_eq!(options.video_bitrate_kbps, native_rate);
    let native_fingerprint = request.durable_intent_fingerprint(
        &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 },
    );
    request.finite_bitrate_limit_bps = Some(750_000);
    assert!(
        super::manager_create::constrain_finite_vod_rate(&request, &file, &mut options).is_err()
    );
    assert_eq!(options.video_bitrate_kbps, native_rate);
    request.vod_only = true;
    request.passive_vod = true;
    super::manager_create::constrain_finite_vod_rate(&request, &file, &mut options)
        .expect("bounded native recipe");
    assert!(
        u64::from(options.video_bitrate_kbps) * 1500 + u64::from(options.audio_bitrate_kbps) * 1000
            <= 750_000
    );
    assert_ne!(
        native_fingerprint,
        request.durable_intent_fingerprint(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
        )
    );
    let round_trip: SessionRequest =
        serde_json::from_str(&serde_json::to_string(&request).expect("worker serialization"))
            .expect("worker request");
    assert_eq!(round_trip.finite_bitrate_limit_bps, Some(750_000));
    assert_eq!(
        round_trip.durable_intent_fingerprint(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
        ),
        request.durable_intent_fingerprint(
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
        )
    );
    assert!(crate::media_sessions::worker_session_request_is_valid(
        &round_trip
    ));
    request.finite_bitrate_limit_bps = Some(64_000);
    let error = super::manager_create::constrain_finite_vod_rate(&request, &file, &mut options)
        .expect_err("audio alone exceeds this ceiling");
    assert_eq!(
        vod_refusal(&error).expect("typed").0,
        "vod_output_budget_refused"
    );
    let mut copy = request.clone();
    copy.kind = SessionKind::Copy {
        aac: true,
        preserve_dolby_vision: false,
        convert_dolby_vision: false,
        dual_layer_as_copied: false,
    };
    copy.finite_bitrate_limit_bps = Some(750_000);
    let mut copy_file = file.clone();
    copy_file.bitrate = Some(400_000);
    super::manager_create::validate_finite_copy_rate(&copy, &copy_file)
        .expect("source plus conservative audio fits");
    copy_file.bitrate = Some(500_000);
    assert!(super::manager_create::validate_finite_copy_rate(&copy, &copy_file).is_err());
    copy.subtitle_burn = Some(0);
    assert!(
        super::manager_create::validate_finite_copy_rate(&copy, &copy_file).is_err(),
        "a known empty burn track still needs the copy bitrate proof"
    );
    copy_file.bitrate = None;
    assert!(super::manager_create::validate_finite_copy_rate(&copy, &copy_file).is_err());
    request.passive_vod = false;
    assert!(!crate::media_sessions::worker_session_request_is_valid(
        &request
    ));
}

// Local VOD now requires a typed viewer demand even in process-local tests.
async fn create_local_vod(
    manager: &TranscodeManager,
    request: &SessionRequest,
    user_name: &str,
) -> Result<StartInfo, String> {
    let identity = SessionRecoveryIdentity {
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 },
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        recovery_epoch: String::new(),
    };
    let scope = serde_json::json!(["username", user_name]).to_string();
    Box::pin(manager.create_session_inner(
        request,
        user_name,
        &scope,
        Some(&identity),
        None,
        None,
        None,
        (Priority::Live, crate::vodserve::RetainedOutputCapture::New),
    ))
    .await
    .map(|creation| creation.info)
}
