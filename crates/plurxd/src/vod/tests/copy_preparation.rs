struct CopyPreparationAuthority;

#[tokio::test]
async fn copy_preparation_full_footprint_cap_and_drop_preserve_ordinary_working_set() {
    let base = crate::test_tempdir().expect("preparation accounting");
    let (serve, _) = serve_on_file(base.path(), fixture_file()).await;
    let registry = &serve.shared.retained_artifacts;
    assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1025, 1024).await.is_none());
    let allowance = super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1024, 1024)
        .await.expect("bounded reservation");
    assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1, 1024).await.is_none());
    assert!(allowance.begin(256).expect("metadata").commit(false));
    let pending = allowance.begin(768).expect("remaining footprint");
    assert!(allowance.begin(1).is_none(), "inflight bytes count before publication");
    drop(pending);
    assert_eq!(allowance.footprint(), Some(256));
    // Simulate the actual publisher's already-charged media. Exclusion never
    // edits that physical accounting or the pre-existing ordinary charge.
    serve.shared.working_set.store(1500, Relaxed);
    assert!(allowance.begin(500).expect("media").commit(true));
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 500);
    allowance.release();
    allowance.release();
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
    assert_eq!(serve.shared.working_set.load(Relaxed), 1500);
    assert_eq!(registry.test_preparation_count(), 0);
    assert!(allowance.begin(1).is_none());
    let dropped = super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1024, 1024)
        .await.expect("released cap reusable");
    drop(dropped);
    assert_eq!(registry.test_preparation_count(), 0);
}
#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl plurx_core::cluster::coordination::ClusterJobAuthority for CopyPreparationAuthority {
    async fn may_run_cluster_jobs(&self) -> bool { true }
}

#[tokio::test]
async fn prepared_copy_private_incarnation_settles_before_new_attachment_and_preserves_incumbent() {
    prepared_copy_consumer(0).await;
}

#[tokio::test]
async fn prepared_copy_foreground_attachment_yields_private_body_without_revoking_incumbent() {
    prepared_copy_consumer(1).await;
}

#[tokio::test]
async fn prepared_copy_source_change_refuses_settlement_and_releases_private_body() {
    prepared_copy_consumer(2).await;
}

#[tokio::test]
async fn prepared_copy_issued_body_survives_later_successful_foreground_attachment() {
    prepared_copy_consumer(3).await;
}

async fn prepared_copy_consumer(control: u8) {
    use plurx_core::store::background_jobs::*;
    let base = crate::test_tempdir().expect("prepared copy consumer");
    let original = fixture_file();
    let source_path = base.path().join("source.mkv");
    std::fs::copy(&original.path, &source_path).expect("owned immutable source fixture");
    let file = media_file_at(source_path, 12_000);
    let (serve, file) = serve_on_file(base.path(), file).await;
    let store = Arc::clone(&serve.shared.store);
    let library = store.create_library(&plurx_core::domain::NewLibrary {
        name: "copy consumer".into(), kind: plurx_core::domain::LibraryKind::Movies,
        paths: vec![base.path().to_path_buf()], anime: false,
    }).await.expect("library");
    let item = store.insert_item(&plurx_core::domain::NewItem {
        library_id: library.id, kind: plurx_core::domain::ItemKind::Movie, parent_id: None,
        title: "copy consumer".into(), year: None, season_number: None, episode_number: None,
    }).await.expect("item");
    assert_eq!(store.upsert_file(item, &file.path.to_string_lossy(), file.size, file.mtime,
        &plurx_core::domain::ProbeResult::default()).await.expect("source row"), file.id);
    create(&serve, &file, "incumbent", "original", &settings()).await;
    drop(fetch(&serve, "incumbent", &segment_name(0)).await);
    let incumbent = serve.shared.sessions.lock().await.get("incumbent").expect("incumbent")
        .rendition.clone().expect("partial rendition");
    let incoming = request("prepared", 0.0);
    let source = crate::fragment_index_cluster::open_source_fence(&file, None).await.expect("source proof");
    let object = source.object_version().to_owned();
    let intent = CopyOutputIntent {
        target_node_id: "test-local".into(), audio_index: None, audio_offset_ms: 0, audio_claim: None,
        audio_delivery: plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::None, downmix: None, reason: "no_audio".into(),
        }, aac: true, preserve_dolby_vision: false, convert_dolby_vision: false,
        hdr10_requested: false, grade: plurx_core::transcode::OutputGrade::Sdr, normalized_geometry: true, profile: None,
        width: 640, height: 360, video_identity: "a".repeat(64), pipeline_identity: "b".repeat(64),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::media_sessions::unix_ms();
    assert!(matches!(store.enqueue_job(EnqueueJob {
        id: id.clone(), payload: JobPayload::CopyOutputPrepare {
            copy_output_version: 1, file_id: file.id, source_generation: object.clone(),
            source_size: file.size, source_mtime: file.mtime, source_object_version: object.clone(),
            policy_generation: "copy:1".into(), intent: intent.clone(), scratch_bytes: 64 << 20,
            reason: "recent_demand".into(),
        }, dedupe_key: "copy:consumer".into(), priority: 1, not_before_ms: now, now_ms: now,
        request: JobRequest {
            scope: "copy".into(), request_id: "copy:consumer".into(), request_digest: "c".repeat(64),
            consumer_kind: "copy_output".into(), consumer_ref: file.id.to_string(),
            target_node_id: Some("test-local".into()), deadline_ms: None, retain_identity: false,
        },
    }).await.expect("enqueue"), EnqueueOutcome::Accepted { .. }));
    let job = match store.claim_job(ClaimJob {
        job_id: id.clone(), expected_revision: 0, node_id: "test-local".into(),
        boot_id: uuid::Uuid::new_v4().to_string(), claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::CopyOutputPrepare, payload_version: 1, now_ms: now + 1, dispatched_at_ms: now + 1,
    }).await.expect("claim") {
        ClaimOutcome::Claimed { job } => job,
        outcome => panic!("unexpected claim {outcome:?}"),
    };
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        Arc::clone(&store), Arc::new(CopyPreparationAuthority), job.token.expect("token"),
        tokio::time::Instant::now() + Duration::from_secs(30), JobKind::CopyOutputPrepare,
    ).expect("active owner");
    let prepared = tokio::time::timeout(Duration::from_secs(15), serve.prepare_copy_output(
        (&incoming).into(), &file, &settings(), &object, 64 << 20,
        active.fence(), Instant::now() + Duration::from_secs(15), crate::admission::Admissions::new(),
        (Arc::new(crate::ffmpeg::EncodedExecutable::capture().await.expect("executable")),
         crate::ffmpeg::EncodedEngine::capture(None).await.expect("engine")), || true,
    )).await.expect("bounded preparation").expect("actual completed copy body");
    let facts = prepared.private_facts();
    assert!(facts.valid());
    assert!(!serve.shared.retained_artifacts.test_has_artifact(&facts),
        "private assembly cannot expose candidate cost before settlement");
    if control == 2 {
        let mut changed = std::fs::OpenOptions::new().append(true).open(&file.path)
            .expect("owned source mutation");
        std::io::Write::write_all(&mut changed, b"changed source incarnation")
            .expect("source incarnation change");
        assert!(!prepared.settle_and_expose(&intent).await.expect("changed source refusal"));
        assert!(!serve.shared.retained_artifacts.test_has_artifact(&facts));
        assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
        assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 0);
        assert_ne!(store.background_job(&id).await.expect("job").expect("row").state, JobState::Succeeded);
        serve.end("incumbent", Terminal::Deleted).await;
        active.finish().await;
        return;
    }
    if control == 1 {
        create(&serve, &file, "new-foreground", "foreground", &settings()).await;
        assert!(!prepared.settle_and_expose(&intent).await.expect("yielded preparation"));
        assert!(!serve.shared.retained_artifacts.test_has_artifact(&facts));
        assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
        assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 0);
        assert!(Arc::ptr_eq(serve.shared.sessions.lock().await.get("incumbent")
            .expect("incumbent remains attached").rendition.as_ref().expect("rendition"), &incumbent));
        assert!(serve.segment("new-foreground", &segment_name(0)).await.expect("foreground answer")
            .result.expect("playable foreground").is_some());
        serve.end("new-foreground", Terminal::Deleted).await;
        serve.end("incumbent", Terminal::Deleted).await;
        active.finish().await;
        return;
    }
    assert!(prepared.settle_and_expose(&intent).await.expect("atomic settlement"));
    assert_eq!(store.background_job(&id).await.expect("job").expect("job row").state, JobState::Succeeded);
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 0, "live exclusion released exactly once");
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 0);
    serve.try_create(VodRecipeRequest {
        request: &incoming, encoding: None, measured_candidate: None,
        retained_capture: RetainedOutputCapture::Restore(Some(facts.clone())),
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "fixture", supersession_user: "user" },
        "prepared".into()).await.expect("compatible NEW presentation captures prepared body");
    if control == 3 {
        create(&serve, &file, "later-foreground", "foreground", &settings()).await;
    }
    let first = serve.segment("prepared", &segment_name(0)).await.expect("first wire answer");
    assert_eq!(first.owner.retained_output_facts(), Some(facts));
    assert!(first.result.expect("publication").expect("media").retained_lease.is_some());
    assert!(Arc::ptr_eq(serve.shared.sessions.lock().await.get("incumbent").expect("incumbent preserved")
        .rendition.as_ref().expect("incumbent rendition"), &incumbent));
    serve.end("prepared", Terminal::Deleted).await;
    if control == 3 {
        serve.end("later-foreground", Terminal::Deleted).await;
    }
    serve.end("incumbent", Terminal::Deleted).await;
    active.finish().await;
}
