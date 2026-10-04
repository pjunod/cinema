#[tokio::test]
async fn encoded_exact_resolver_claimed_worker_and_new_attachment_hold_complete_background_output() {
    use plurx_core::store::background_jobs::*;
    testfixtures::require_ffmpeg();
    let base = crate::test_tempdir().expect("encoded preparation");
    let path = base.path().join("source.mkv");
    std::fs::copy(testfixtures::source_with_eac3_audio(), &path).expect("owned source");
    let preliminary = media_file_at(path.clone(), 12_000);
    let source = crate::fragment_index_cluster::open_source_fence(&preliminary, None).await.expect("held source");
    let raw = crate::ffmpeg::held_source_probe_json(&source.handle, crate::process_control::ChildWork::background("encoded preparation fixture probe")).await.expect("real probe");
    let probe = plurx_core::scan::probe::parse_probe_json(&serde_json::from_str(&raw).expect("probe JSON"));
    let database = base.path().join("store.sqlite");
    let sqlite = SqliteStore::open(&database).expect("store");
    rusqlite::Connection::open(&database).expect("guard connection")
        .execute_batch(include_str!("../../../../plurx-core/src/store/background_jobs_encoded_output.sql"))
        .expect("exact encoded guards, recovery migration remains separately owned");
    let store: Arc<dyn Store> = Arc::new(sqlite);
    let library = store.create_library(&plurx_core::domain::NewLibrary { name: "encoded".into(),
        kind: plurx_core::domain::LibraryKind::Movies, paths: vec![base.path().to_owned()], anime: false }).await.expect("library");
    let item = store.insert_item(&plurx_core::domain::NewItem { library_id: library.id,
        kind: plurx_core::domain::ItemKind::Movie, parent_id: None, title: "encoded".into(),
        year: None, season_number: None, episode_number: None }).await.expect("item");
    let id = store.upsert_file(item, &path.to_string_lossy(), preliminary.size, preliminary.mtime, &probe).await.expect("file");
    let file = store.get_file(id).await.expect("file").expect("row");
    drop(source);
    store.put_setting(plurx_core::store::keys::CACHE_MAX_GB, "1").await.expect("retention cap");
    let vod_base = base.path().join("vod");
    std::fs::create_dir(&vod_base).expect("owned VOD namespace");
    let serve = local_serve(vod_base, Arc::clone(&store));
    let manager = crate::transcode::TranscodeManager::new(Arc::clone(&store), base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(), plurx_core::transcode::Pipeline::Cpu)
        .with_cache(base.path().join("cache"), "encoded-test".into(), "test-local".into())
        .with_copy_test_vod(Arc::clone(&serve));
    let mut selected = request("encoded", 0.0);
    selected.file_id = id;
    selected.kind = SessionKind::Transcode { height: 360 };
    selected.audio_index = file.audio_streams.first().map(|stream| stream.index);
    selected.audio_claim = Some(plurx_core::playback::audio::AudioClaim { decoders: vec!["aac".into()], sinks: vec![] });
    selected.audio_offset_ms = 250;
    let mut resolved_file = file.clone(); resolved_file.audio_offset_ms = 250;
    let encoding = manager.resolve_encoded_output_test(&selected, &resolved_file).await.expect("actual resolver");
    selected.audio_delivery = encoding.options.audio.clone();
    assert!(selected.candidate_context.is_none());
    manager.enqueue_encoded_output(&selected, &resolved_file, &settings(), &encoding).await.expect("queue");
    manager.enqueue_encoded_output(&selected, &resolved_file, &settings(), &encoding).await.expect("dedup");
    let jobs = store.list_jobs(JobQuery { node_id: None, state: None, kind: Some(JobKind::EncodedOutputPrepare),
        after_id: None, limit: 10 }).await.expect("jobs").jobs;
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    let now = crate::media_sessions::unix_ms();
    let claimed = match store.claim_job(ClaimJob { job_id: job.id.clone(), expected_revision: job.revision,
        node_id: "test-local".into(), boot_id: uuid::Uuid::new_v4().to_string(), claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::EncodedOutputPrepare, payload_version: 1, now_ms: now, dispatched_at_ms: now }).await.expect("claim") {
        ClaimOutcome::Claimed { job } => job, other => panic!("claim {other:?}"),
    };
    let active = crate::background_jobs::ActiveBackgroundJob::start(Arc::clone(&store), Arc::new(CopyPreparationAuthority),
        claimed.token.clone().expect("token"), tokio::time::Instant::now() + Duration::from_secs(45),
        JobKind::EncodedOutputPrepare).expect("owner");
    let heavy = manager.admit_encoded_preparation().expect("existing heavy lane");
    let observation = manager.copy_preparation_attachment_observation();
    assert!(manager.produce_encoded_output_job(&file, &claimed, active.fence(),
        Instant::now() + Duration::from_secs(40), observation).await.expect("actual encoded worker"));
    drop(heavy); active.finish().await;
    assert_eq!(store.background_job(&job.id).await.expect("job").expect("row").state, JobState::Succeeded);
    let encoding = manager.resolve_encoded_output_test(&selected, &resolved_file).await.expect("fresh resolver");
    serve.try_create(VodRecipeRequest { request: &selected, encoding: Some(encoding),
        retained_capture: RetainedOutputCapture::New, measured_candidate: None }, &resolved_file, &settings(),
        VodAttribution { user_name: "user", item_title: "encoded", supersession_user: "user" }, "later-encoded".into())
        .await.expect("compatible new attachment");
    let artifact = serve.shared.sessions.lock().await.get("later-encoded").expect("session")
        .retained_output.clone().expect("settled local complete body");
    assert!(artifact.candidate.is_none());
    assert!(artifact.facts().peak_bps >= artifact.facts().average_bps);
    assert_eq!(artifact.observation.preimage.playlist, serve.shared.sessions.lock().await.get("later-encoded")
        .expect("session").live_rendition().expect("rendition").playlist);
    assert!(artifact.observation.rates.wire_bytes > 0);
}

/// Distinct automatic evidence: the manual None-context case above is not
/// evidence for persistence or reconstruction of a selected catalog authority.
#[tokio::test]
async fn automatic_encoded_canonical_carrier_survives_claim_completion_and_new_attachment() {
    use plurx_core::playback::candidate::CandidateRoute;
    use plurx_core::store::background_jobs::*;
    testfixtures::require_ffmpeg();
    let base = crate::test_tempdir().expect("automatic encoded preparation");
    let path = base.path().join("source.mkv");
    std::fs::copy(testfixtures::source_with_eac3_audio(), &path).expect("owned source");
    let preliminary = media_file_at(path.clone(), 12_000);
    let source = crate::fragment_index_cluster::open_source_fence(&preliminary, None)
        .await
        .expect("held source");
    let raw = crate::ffmpeg::held_source_probe_json(
        &source.handle,
        crate::process_control::ChildWork::background("automatic carrier fixture probe"),
    )
    .await
    .expect("real probe");
    let probe =
        plurx_core::scan::probe::parse_probe_json(&serde_json::from_str(&raw).expect("probe JSON"));
    let database = base.path().join("store.sqlite");
    let sqlite = SqliteStore::open(&database).expect("store");
    rusqlite::Connection::open(&database)
        .expect("guard connection")
        .execute_batch(include_str!(
            "../../../../plurx-core/src/store/background_jobs_encoded_output.sql"
        ))
        .expect("exact encoded guards");
    let store: Arc<dyn Store> = Arc::new(sqlite);
    let library = store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "automatic-carrier".into(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec![base.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "automatic-carrier".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let id = store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            preliminary.size,
            preliminary.mtime,
            &probe,
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("file").expect("row");
    drop(source);
    store
        .put_setting(plurx_core::store::keys::CACHE_MAX_GB, "1")
        .await
        .expect("cap");
    store
        .put_setting(plurx_core::store::keys::HWACCEL, "software")
        .await
        .expect("CPU");
    store
        .put_setting("playback.vod_reorder_frames", "0")
        .await
        .expect("frozen off choice");
    let vod_base = base.path().join("vod");
    std::fs::create_dir(&vod_base).expect("owned VOD namespace");
    let serve = local_serve(vod_base, Arc::clone(&store));
    let decode_probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
        &crate::ffmpeg::bound_ffprobe_bin(),
    )
    .await
    .expect("actual bound fixture FFprobe identity");
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    )
    .with_cache(
        base.path().join("cache"),
        "automatic-carrier-test".into(),
        "test-local".into(),
    )
    .with_decode_probe(Some(decode_probe))
    .with_copy_test_vod(Arc::clone(&serve));
    let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
        "v":2,"video":[{"codec":"h264","decode":true,"present":["sdr"]}],
        "audio":["aac"],
        "audio_sinks":[{"codec":"aac","max_channels":2,"sample_rates_hz":[48000]}],
        "transports":["hls"]
    }))
    .expect("real caller document");
    let planning = store
        .playback_planning_snapshot(id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("atomic snapshot")
        .expect("source");
    let audio_index = file.audio_streams.first().map(|stream| stream.index);
    let claim = plurx_core::playback::audio::AudioClaim::from_caps(&caps)
        .expect("claim")
        .expect("AAC claim");
    let rows = manager
        .quality_catalog_from_snapshot_progress(
            &planning,
            &caps,
            audio_index,
            250,
            None,
            crate::transcode::Presentation::Vod,
            None,
            None,
            Some(&claim),
            None,
            None,
        )
        .await
        .candidates;
    let candidate = rows
        .into_iter()
        .filter(|row| {
            row.route == CandidateRoute::Encode
                && row.decoder_compatible
                && row.grade == plurx_core::transcode::OutputGrade::Sdr
        })
        .min_by_key(|row| row.target_height)
        .expect("actual automatic encoded row");
    let mut selected = request("automatic-carrier", 0.0);
    selected.file_id = id;
    selected.automatic = true;
    selected.presentation = crate::transcode::Presentation::Vod;
    selected.kind = SessionKind::Transcode {
        height: i64::from(candidate.target_height),
    };
    selected.audio_index = audio_index;
    selected.audio_claim = Some(claim);
    selected.audio_offset_ms = 250;
    let mut context = crate::transcode::TranscodeManager::candidate_context(&candidate);
    context.canonical_caps = Some(caps);
    context.planning_binding = Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
    context.planning_snapshot = Some(Arc::new(planning));
    context.owner_node_id = Some("test-local".into());
    selected.candidate_context = Some(Box::new(context));
    let mut resolved_file = file.clone();
    resolved_file.audio_offset_ms = 250;
    let encoding = manager
        .resolve_encoded_output_test(&selected, &resolved_file)
        .await
        .expect("actual canonical resolver");
    selected.audio_delivery = encoding.options.audio.clone();
    manager
        .enqueue_encoded_output(&selected, &resolved_file, &settings(), &encoding)
        .await
        .expect("automatic queue");
    manager
        .enqueue_encoded_output(&selected, &resolved_file, &settings(), &encoding)
        .await
        .expect("exact carrier dedup");
    let jobs = store
        .list_jobs(JobQuery {
            node_id: None,
            state: None,
            kind: Some(JobKind::EncodedOutputPrepare),
            after_id: None,
            limit: 10,
        })
        .await
        .expect("jobs")
        .jobs;
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    let JobPayload::EncodedOutputPrepare {
        candidate_catalog,
        intent,
        ..
    } = job.supported_payload().expect("closed payload")
    else {
        panic!("encoded payload");
    };
    let persisted: crate::media_sessions::CandidateCatalogContext =
        serde_json::from_value(candidate_catalog.expect("persisted canonical authority"))
            .expect("existing strict carrier parser");
    assert_eq!(persisted.candidate.id, candidate.id);
    assert_eq!(intent.candidate_digest, Some(candidate.recipe_digest));
    let now = crate::media_sessions::unix_ms();
    let claimed = match store
        .claim_job(ClaimJob {
            job_id: job.id.clone(),
            expected_revision: job.revision,
            node_id: "test-local".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::EncodedOutputPrepare,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        })
        .await
        .expect("claim")
    {
        ClaimOutcome::Claimed { job } => job,
        other => panic!("claim {other:?}"),
    };
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        Arc::clone(&store),
        Arc::new(CopyPreparationAuthority),
        claimed.token.clone().expect("token"),
        tokio::time::Instant::now() + Duration::from_secs(45),
        JobKind::EncodedOutputPrepare,
    )
    .expect("owner");
    let heavy = manager
        .admit_encoded_preparation()
        .expect("existing heavy lane");
    let observation = manager.copy_preparation_attachment_observation();
    assert!(manager
        .produce_encoded_output_job(
            &file,
            &claimed,
            active.fence(),
            Instant::now() + Duration::from_secs(40),
            observation
        )
        .await
        .expect("actual automatic worker"));
    drop(heavy);
    active.finish().await;
    assert_eq!(
        store
            .background_job(&job.id)
            .await
            .expect("job")
            .expect("row")
            .state,
        JobState::Succeeded
    );
    let encoding = manager
        .resolve_encoded_output_test(&selected, &resolved_file)
        .await
        .expect("new attachment same canonical resolver");
    let measured_candidate = crate::vodserve::RetainedCandidateBinding {
        kind: selected.kind,
        normalized_geometry: candidate.normalized_geometry,
        profile: selected
            .candidate_context
            .as_ref()
            .expect("authority")
            .profile,
        candidate_id: candidate.id,
        recipe_digest: candidate.recipe_digest,
        file_id: id,
        audio_index: selected.audio_index,
        audio_offset_ms: 250,
        subtitle_burn: None,
        grade: candidate.grade,
        route: CandidateRoute::Encode,
    };
    serve
        .try_create(
            VodRecipeRequest {
                request: &selected,
                encoding: Some(encoding),
                retained_capture: RetainedOutputCapture::New,
                measured_candidate: Some(measured_candidate),
            },
            &resolved_file,
            &settings(),
            VodAttribution {
                user_name: "user",
                item_title: "automatic-carrier",
                supersession_user: "user",
            },
            "later-automatic".into(),
        )
        .await
        .expect("compatible new automatic attachment");
    let sessions = serve.shared.sessions.lock().await;
    let session = sessions.get("later-automatic").expect("new session");
    let artifact = session
        .retained_output
        .as_ref()
        .expect("actual settled complete output");
    assert_eq!(
        artifact
            .candidate
            .as_ref()
            .expect("candidate origin")
            .candidate_id,
        candidate.id
    );
    assert_eq!(
        artifact
            .candidate
            .as_ref()
            .expect("candidate origin")
            .recipe_digest,
        candidate.recipe_digest
    );
    assert_eq!(
        artifact.observation.preimage.playlist,
        session.live_rendition().expect("rendition").playlist
    );
    assert!(artifact.observation.rates.wire_bytes > 0);
    assert!(artifact.facts().peak_bps >= artifact.facts().average_bps);
    let rendition = Arc::clone(session.live_rendition().expect("rendition"));
    let logical = rendition.recipe.retained_logical.clone();
    drop(sessions);
    let mut wrong_offset = selected.clone();
    wrong_offset.audio_offset_ms += 1;
    assert!(
        serve
            .shared
            .retained_artifacts
            .acquire_prepared_candidate(&rendition, &logical, &resolved_file, &wrong_offset)
            .await
            .is_none(),
        "a different audio offset cannot borrow the settled origin"
    );
    let mut wrong_geometry = selected.clone();
    wrong_geometry
        .candidate_context
        .as_mut()
        .expect("authority")
        .normalized_geometry = !candidate.normalized_geometry;
    assert!(
        serve
            .shared
            .retained_artifacts
            .acquire_prepared_candidate(&rendition, &logical, &resolved_file, &wrong_geometry)
            .await
            .is_none(),
        "a different candidate geometry cannot borrow the settled origin"
    );
}
