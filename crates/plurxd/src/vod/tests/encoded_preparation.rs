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
