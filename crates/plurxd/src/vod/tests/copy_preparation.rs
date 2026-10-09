struct CopyPreparationAuthority;

#[tokio::test]
async fn manual_copy_queued_metadata_and_unsupported_versions_refuse_before_publication() {
    use plurx_core::store::background_jobs::*;
    let base = crate::test_tempdir().expect("manual metadata");
    let path = base.path().join("source.mkv");
    std::fs::copy(fixture_file().path, &path).expect("owned source");
    let file = media_file_at(path, 12_000);
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let library = store.create_library(&plurx_core::domain::NewLibrary {
        name: "metadata".into(), kind: plurx_core::domain::LibraryKind::Movies,
        paths: vec![base.path().to_owned()], anime: false,
    }).await.expect("library");
    let item = store.insert_item(&plurx_core::domain::NewItem {
        library_id: library.id, kind: plurx_core::domain::ItemKind::Movie,
        parent_id: None, title: "metadata".into(), year: None,
        season_number: None, episode_number: None,
    }).await.expect("item");
    let mut probe = plurx_core::domain::ProbeResult {
        duration_ms: file.duration_ms, container: file.container.clone(),
        video_codec: file.video_codec.clone(), width: file.width, height: file.height,
        bit_depth: file.bit_depth, video_profile: file.video_profile.clone(),
        ..Default::default()
    };
    assert_eq!(store.upsert_file(item, &file.path.to_string_lossy(), file.size, file.mtime,
        &probe).await.expect("file"), file.id);
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store), base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(), plurx_core::transcode::Pipeline::Cpu,
    ).with_cache(base.path().join("cache"), "metadata-test".into(), "test-local".into());
    let mut manual = request("metadata", 0.0);
    manual.audio_claim = Some(plurx_core::playback::audio::AudioClaim {
        decoders: vec!["aac".into()], sinks: vec![],
    });
    manual.audio_delivery = Some(plurx_core::playback::audio::resolve_audio(
        None, &manual.audio_claim.as_ref().expect("claim").profile(),
        plurx_core::playback::audio::AudioRoute::Progressive, 0,
    ));
    manager.enqueue_copy_output(&manual, &file, &settings()).await.expect("enqueue");
    let mut jobs = store.list_jobs(JobQuery { node_id: None, state: None,
        kind: Some(JobKind::CopyOutputPrepare), after_id: None, limit: 10,
    }).await.expect("jobs").jobs;
    assert_eq!(jobs.len(), 1);
    let job = jobs.remove(0);
    let original = job.supported_payload().expect("supported v2");
    for (version, geometry) in [(3, false), (1, false), (2, true)] {
        let mut changed = original.clone();
        if let JobPayload::CopyOutputPrepare { copy_output_version, intent, .. } = &mut changed {
            *copy_output_version = version; intent.normalized_geometry = geometry;
        }
        assert!(changed.validate().is_err(), "closed version/geometry contract");
    }
    let now = crate::media_sessions::unix_ms();
    let claimed = match store.claim_job(ClaimJob {
        job_id: job.id.clone(), expected_revision: job.revision,
        node_id: "test-local".into(), boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(), kind: JobKind::CopyOutputPrepare,
        payload_version: 1, now_ms: now, dispatched_at_ms: now,
    }).await.expect("claim") {
        ClaimOutcome::Claimed { job } => job,
        other => panic!("claim {other:?}"),
    };
    // Real store metadata replacement, not a changed local test struct; the
    // existing size/mtime trigger correctly leaves this job running.
    probe.container = Some("mp4".into());
    store.upsert_file(item, &file.path.to_string_lossy(), file.size, file.mtime, &probe)
        .await.expect("metadata replacement");
    assert_eq!(store.background_job(&job.id).await.expect("job").expect("row").state,
        JobState::Running);
    let current = store.get_file(file.id).await.expect("file").expect("row");
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        Arc::clone(&store), Arc::new(CopyPreparationAuthority), claimed.token.clone().expect("token"),
        tokio::time::Instant::now() + Duration::from_secs(30), JobKind::CopyOutputPrepare,
    ).expect("owner");
    let admission = manager.admit_fragment().await.expect("background admission");
    let error = manager.produce_copy_output_job(&current, &claimed, active.fence(),
        &admission, Instant::now() + Duration::from_secs(20)).await.expect_err("metadata refusal");
    assert_eq!(
        error,
        crate::background_jobs::PreparationError::Fail("manual_copy_source_metadata_changed")
    );
    assert_ne!(store.background_job(&job.id).await.expect("job").expect("row").state,
        JobState::Succeeded, "no historical completion or private publication");
    active.finish().await;
}

#[tokio::test]
async fn manual_copy_manager_queue_worker_and_matching_new_attachment_use_only_local_settled_proof() {
    use plurx_core::store::background_jobs::*;
    let base = crate::test_tempdir().expect("manual copy");
    let path = base.path().join("source.mkv");
    std::fs::copy(fixture_file().path, &path).expect("owned source");
    let (serve, file) = serve_on_file(base.path(), media_file_at(path, 12_000)).await;
    let store = Arc::clone(&serve.shared.store);
    let library = store.create_library(&plurx_core::domain::NewLibrary {
        name: "manual".into(), kind: plurx_core::domain::LibraryKind::Movies,
        paths: vec![base.path().to_owned()], anime: false,
    }).await.expect("library");
    let item = store.insert_item(&plurx_core::domain::NewItem {
        library_id: library.id, kind: plurx_core::domain::ItemKind::Movie,
        parent_id: None, title: "manual".into(), year: None,
        season_number: None, episode_number: None,
    }).await.expect("item");
    assert_eq!(store.upsert_file(item, &file.path.to_string_lossy(), file.size, file.mtime,
        &plurx_core::domain::ProbeResult {
            duration_ms: file.duration_ms, container: file.container.clone(),
            video_codec: file.video_codec.clone(), width: file.width, height: file.height,
            bit_depth: file.bit_depth, video_profile: file.video_profile.clone(),
            ..Default::default()
        }).await.expect("file"), file.id);
    store.put_setting(plurx_core::store::keys::CACHE_MAX_GB, "1").await.expect("retention cap");
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store), base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(), plurx_core::transcode::Pipeline::Cpu,
    ).with_cache(base.path().join("cache"), "manual-test".into(), "test-local".into())
        .with_copy_test_vod(Arc::clone(&serve));
    // A real cold mid-film attachment leaves a suffix, not a complete output.
    let mut manual = request("cold-manual", 8.0);
    manual.audio_claim = Some(plurx_core::playback::audio::AudioClaim {
        decoders: vec!["aac".into()], sinks: vec![],
    });
    manual.audio_delivery = Some(plurx_core::playback::audio::resolve_audio(
        None, &manual.audio_claim.as_ref().expect("claim").profile(),
        plurx_core::playback::audio::AudioRoute::Progressive, 0,
    ));
    assert!(manual.candidate_context.is_none());
    serve.try_create(&manual, &file, &settings(), VodAttribution {
        user_name: "user", item_title: "manual", supersession_user: "user",
    }, "cold-manual".into()).await.expect("ordinary unknown cold playback");
    assert!(serve.shared.sessions.lock().await.get("cold-manual").expect("cold")
        .retained_output.is_none());
    manager.enqueue_copy_output(&manual, &file, &settings()).await.expect("enqueue");
    manager.enqueue_copy_output(&manual, &file, &settings()).await.expect("dedup enqueue");
    let jobs = store.list_jobs(JobQuery { node_id: None, state: None,
        kind: Some(JobKind::CopyOutputPrepare), after_id: None, limit: 10,
    }).await.expect("jobs").jobs;
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert!(matches!(job.supported_payload().expect("manual payload"),
        JobPayload::CopyOutputPrepare { copy_output_version: 2, .. }));
    let now = crate::media_sessions::unix_ms();
    let claimed = match store.claim_job(ClaimJob {
        job_id: job.id.clone(), expected_revision: job.revision,
        node_id: "test-local".into(), boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(), kind: JobKind::CopyOutputPrepare,
        payload_version: 1, now_ms: now, dispatched_at_ms: now,
    }).await.expect("claim") {
        ClaimOutcome::Claimed { job } => job,
        other => panic!("claim {other:?}"),
    };
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        Arc::clone(&store), Arc::new(CopyPreparationAuthority),
        claimed.token.clone().expect("token"),
        tokio::time::Instant::now() + Duration::from_secs(30), JobKind::CopyOutputPrepare,
    ).expect("owner");
    let admission = manager.admit_fragment().await.expect("existing background admission");
    assert!(manager.produce_copy_output_job(&file, &claimed, active.fence(),
        &admission, Instant::now() + Duration::from_secs(20)).await.expect("real copy worker"));
    drop(admission);
    active.finish().await;
    manual.start_seconds = 0.0;
    serve.try_create(&manual, &file, &settings(), VodAttribution {
        user_name: "user", item_title: "manual", supersession_user: "user",
    }, "later-manual".into()).await.expect("compatible new attachment");
    let later = serve.shared.sessions.lock().await.get("later-manual").expect("later")
        .retained_output.clone().expect("locally minted complete proof");
    assert!(later.candidate.is_none(), "manual preparation never fabricates a candidate");
    assert!(later.private_preparation_origin.get().is_some());
    assert!(serve.shared.sessions.lock().await.get("cold-manual").expect("cold")
        .retained_output.is_none(), "initial unknown capture is immutable");
    drop(fetch(&serve, "later-manual", &segment_name(0)).await);
    let rendition = serve.shared.sessions.lock().await.get("later-manual").expect("later")
        .rendition.clone().expect("rendition");
    let video = rendition.recipe.video;
    let registry = &serve.shared.retained_artifacts;
    for (name, changed) in [
        ("offset", { let mut r = manual.clone(); r.audio_offset_ms = 1; r }),
        ("audio", { let mut r = manual.clone(); r.audio_index = Some(123); r }),
        ("delivery", { let mut r = manual.clone(); r.audio_delivery = None; r }),
        ("hdr", { let mut r = manual.clone(); r.hdr10 = true; r }),
    ] {
        let logical = Some(super::retained_manifest::LogicalOutput::resolve(&changed, None, &file, video));
        assert!(registry.acquire_prepared_manual(&rendition, &logical, &file).await.is_none(),
            "{name} cannot inherit private artifact facts");
    }
    let mut wrong_container = file.clone();
    wrong_container.container = Some("mp4".into());
    let logical = Some(super::retained_manifest::LogicalOutput::resolve(&manual, None, &file, video));
    assert!(registry.acquire_prepared_manual(&rendition, &logical, &wrong_container).await.is_none(),
        "incoming metadata, not discarded healthy Recipe, must match");
    let mut forged = later.facts();
    forged.output_identity = "f".repeat(64);
    assert!(registry.acquire_expected_for_request(&forged, &rendition, &logical).is_none(),
        "serialized facts do not mint private proof");
    let restarted = local_serve(base.path().to_owned(), Arc::clone(&store));
    restarted.try_create(&manual, &file, &settings(), VodAttribution {
        user_name: "user", item_title: "manual", supersession_user: "user",
    }, "restart-manual".into()).await.expect("uncaptured restart playback");
    assert!(restarted.shared.sessions.lock().await.get("restart-manual").expect("restart")
        .retained_output.is_none(), "restart discovery cannot mint manual origin");
    restarted.end("restart-manual", Terminal::Deleted).await;
    let mut changed_source = std::fs::OpenOptions::new().append(true).open(&file.path)
        .expect("owned source");
    std::io::Write::write_all(&mut changed_source, b"new physical incarnation").expect("source change");
    assert!(registry.acquire_prepared_manual(&rendition, &logical, &file).await.is_none(),
        "changed physical source refuses new attachment authority");
    serve.end("cold-manual", Terminal::Deleted).await;
    serve.end("later-manual", Terminal::Deleted).await;
}

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

#[tokio::test]
async fn completed_copy_preparation_does_not_spill_into_live_playback() {
    prepared_copy_consumer(4).await;
}

#[allow(clippy::disallowed_methods)]
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
    let mut playback_settings = settings();
    let mut owed = None;
    let viewer_pause = (control == 4).then(|| serve.shared.test_hooks().ordinary_materialize.arm("owed foreground media"));
    let mut viewer_held = None;
    if control == 4 {
        playback_settings.working_set_bytes = 1 << 20;
        let short_path = base.path().join("ordinary-short.mkv");
        testfixtures::run(std::process::Command::new(testfixtures::ffmpeg())
            .args(["-v", "error", "-y", "-i"]).arg(&file.path)
            .args(["-t", "4", "-c", "copy"]).arg(&short_path));
        let mut ordinary = media_file_at(short_path, 4000); ordinary.id = 42;
        let (_, index) = store_with_index(&ordinary).await;
        store.put_fragment_index(ordinary.id, &index).await.expect("ordinary title index");
        let pause = viewer_pause.as_ref().expect("foreground barrier owner");
        create(&serve, &ordinary, "incumbent", "original", &playback_settings).await;
        viewer_held = Some(pause.reached().await);
        owed = Some({ let serve = Arc::clone(&serve); tokio::spawn(async move { fetch(&serve, "incumbent", &segment_name(1)).await }) });
    } else {
        create(&serve, &file, "incumbent", "original", &playback_settings).await;
        drop(fetch(&serve, "incumbent", &segment_name(0)).await);
    }
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
            candidate_catalog: None,
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
    if (10..=15).contains(&control) {
        let hooks = serve.shared.test_hooks();
        let pause = match control {
            10 => hooks.private_marker.arm("constructor before marker"),
            11 => hooks.rendition_installed.arm("constructor installed"),
            12 => hooks.preparation_snapshot.arm("construction before fence snapshot"),
            13 | 15 => hooks.private_materialize.arm("owned publication before write"),
            _ => hooks.private_registration.arm("owned child before registration"),
        };
        let task = { let serve = Arc::clone(&serve); let incoming = incoming.clone(); let file = file.clone(); let object = object.clone(); let fence = active.fence();
            tokio::spawn(async move { serve.prepare_copy_output((&incoming).into(), &file, &settings(), &object, 64 << 20, fence, Instant::now() + Duration::from_secs(15), crate::admission::Admissions::new(),
                (Arc::new(crate::ffmpeg::EncodedExecutable::capture().await.expect("executable")), crate::ffmpeg::EncodedEngine::capture(None).await.expect("engine")), || true).await }) };
        let held = pause.reached().await;
        let private_source = serve.shared.renditions.lock().await.values().find(|rendition| rendition.private_storage.is_some()).cloned();
        let reap_fault = Arc::new(PrivateWaitFailure { fail: AtomicBool::new(true), attempts: AtomicU64::new(0) });
        if control == 15 {
            let source = private_source.as_ref().expect("registered private producer");
            assert!(!matches!(source.slot.belief().await, Producer::Absent { .. }));
            source.slot.set_reap_hooks(reap_fault.clone()).await;
            hooks.private_failures.lock().expect("unlink fault").insert(format!("unlink:{}", source.key));
        }
        task.abort(); assert!(task.await.is_err_and(|error| error.is_cancelled()));
        serve.shared.preparation_storage.maintain(&serve.shared).await;
        assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1, "constructor/writer/child still owns capacity");
        assert_eq!(serve.shared.working_set.load(Relaxed), 0, "private teardown never charges ordinary pressure");
        if control == 15 {
            let source = private_source.as_ref().expect("source");
            assert!(reap_fault.attempts.load(Acquire) >= 2, "real child.wait failures are retried by the owning reaper");
            assert!(source.dir.path().join(super::preparation_storage::MARKER).exists());
            let other_key = "9".repeat(64);
            let other = private_storage_fixture(&serve, &other_key, 4096, 50 << 30).await;
            other.release();
            for _ in 0..10 {
                serve.shared.preparation_storage.maintain(&serve.shared).await;
                if !base.path().join(&other_key).exists() { break; }
            }
            assert!(!base.path().join(&other_key).exists(), "another cleanup progresses while this reaper fails");
            reap_fault.fail.store(false, Release);
        }
        held.release();
        if control == 15 {
            serve.shared.preparation_storage.maintain(&serve.shared).await;
            assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1, "unlink fault remains owned after child reap clears");
            hooks.private_failures.lock().expect("clear unlink").clear();
        }
        drain_private_cleanup(&serve).await;
        assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
        assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 0);
        serve.end("incumbent", Terminal::Deleted).await; active.finish().await;
        return;
    }
    if control == 6 {
        let mut failures = serve.shared.test_hooks().private_failures.lock().expect("fault");
        failures.insert("link_after_init".into()); failures.insert("unlink".into());
    }
    let prepared_result = tokio::time::timeout(Duration::from_secs(15), serve.prepare_copy_output(
        (&incoming).into(), &file, &settings(), &object, 64 << 20,
        active.fence(), Instant::now() + Duration::from_secs(15), crate::admission::Admissions::new(),
        (Arc::new(crate::ffmpeg::EncodedExecutable::capture().await.expect("executable")),
         crate::ffmpeg::EncodedEngine::capture(None).await.expect("engine")), || true,
    )).await.expect("bounded preparation");
    if control == 6 {
        assert!(prepared_result.is_err(), "partial link failure is a refused preparation");
        serve.shared.preparation_storage.maintain(&serve.shared).await;
        let held_capacity = serve.shared.retained_artifacts.test_capacity_snapshot();
        assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
        assert!(held_capacity.0 > 0 && held_capacity.1 > 0);
        serve.shared.retained_artifacts.collect(base.path()).await;
        assert_eq!(serve.shared.retained_artifacts.test_capacity_snapshot(), held_capacity, "failed partial artifact remains pinned while scratch unlink fails");
        assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1, held_capacity.0 + held_capacity.1 + held_capacity.2).await.is_none(), "cleanup failure cannot open a capacity-reuse gap");
        serve.shared.test_hooks().private_failures.lock().expect("clear fault").clear();
        drain_private_cleanup(&serve).await;
        assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
        serve.shared.retained_artifacts.collect(base.path()).await;
        assert!(serve.shared.retained_artifacts.test_capacity_snapshot().0 < held_capacity.0, "the partial retained charge releases only after source settlement");
        serve.end("incumbent", Terminal::Deleted).await; active.finish().await;
        return;
    }
    let prepared = prepared_result.expect("actual completed copy body");
    let wire_bytes = prepared.private_wire_bytes();
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
        drain_private_cleanup(&serve).await;
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
        drain_private_cleanup(&serve).await;
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
    let ordinary_before = if control == 4 { 0 } else { let manifest = incumbent.manifest.lock().await;
        if manifest.is_admitted() { 0 } else { manifest.materialized_bytes() } };
    let private_key = serve.shared.renditions.lock().await.values().find(|rendition| rendition.private_storage.is_some()).expect("private source rendition").key.clone();
    if control == 5 || control == 7 { serve.shared.test_hooks().private_failures.lock().expect("fault").insert(if control == 7 { "store" } else { "unlink" }.into()); }
    assert!(prepared.settle_and_expose(&intent).await.expect("atomic settlement"));
    if control == 5 || control == 7 {
        serve.shared.preparation_storage.maintain(&serve.shared).await;
        let blocked = serve.shared.retained_artifacts.test_capacity_snapshot();
        assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
        assert!(blocked.0 > 0 && blocked.1 > 0, "retained body and residual scratch have distinct backed owners");
        serve.shared.retained_artifacts.test_expire_artifact(&facts);
        serve.shared.retained_artifacts.collect(base.path()).await;
        assert!(serve.shared.retained_artifacts.test_has_artifact(&facts), "a truly expired artifact stays pinned while source cleanup is blocked");
        assert_eq!(serve.shared.retained_artifacts.test_capacity_snapshot(), blocked, "GC cannot uncharge the cleanup pin");
        assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1, blocked.0 + blocked.1 + blocked.2).await.is_none(), "near-full retained budget cannot reuse pending cleanup capacity");
        serve.shared.test_hooks().private_failures.lock().expect("clear fault").clear();
    }
    if control == 4 {
        let ordinary_after = serve.shared.working_set.load(Relaxed);
        assert_eq!(ordinary_after, ordinary_before, "settlement cannot turn full-title scratch into viewer pressure");
        assert!(wire_bytes > playback_settings.working_set_bytes, "full-title preparation is larger than playback budget");
        assert!(!owed.as_ref().expect("owed foreground segment").is_finished());
        viewer_held.take().expect("foreground barrier").release();
        drop(tokio::time::timeout(Duration::from_secs(5), owed.take().expect("foreground GET")).await.expect("in-budget viewer progresses").expect("foreground task"));
        assert!(!matches!(*incumbent.capacity_hold.lock().expect("hold"), Some(crate::prodsched::Hold::WorkingSetFull { .. }) | Some(crate::prodsched::Hold::NoRoom { .. })));
    }
    assert_eq!(store.background_job(&id).await.expect("job").expect("job row").state, JobState::Succeeded);
    drain_private_cleanup(&serve).await;
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 0, "private media retired exactly once");
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 0);
    assert!(!store.forget_rendition_plan(&private_key).await.expect("private plan already retired"));
    if control == 5 || control == 7 {
        let before_gc = serve.shared.retained_artifacts.test_capacity_snapshot().0;
        serve.shared.retained_artifacts.test_expire_artifact(&facts);
        serve.shared.retained_artifacts.collect(base.path()).await;
        assert!(!serve.shared.retained_artifacts.test_has_artifact(&facts));
        assert!(serve.shared.retained_artifacts.test_capacity_snapshot().0 < before_gc);
        serve.end("incumbent", Terminal::Deleted).await; active.finish().await;
        return;
    }
    serve.try_create(VodRecipeRequest { soundtrack: None, companion: None,
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

// ---- D2 (main-merge defects 2026-10-04): an executor, a switch, a drain ----

/// A queued copy preparation row for `target`, enqueued directly: the drain
/// and the claim filter never look past the row.
async fn d2_queue_copy_row(store: &Arc<dyn Store>, file: &MediaFile, target: &str, tag: &str) -> String {
    use plurx_core::store::background_jobs::*;
    let intent = CopyOutputIntent {
        target_node_id: target.into(), audio_index: None, audio_offset_ms: 0, audio_claim: None,
        audio_delivery: plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::None, downmix: None, reason: "no_audio".into(),
        }, aac: true, preserve_dolby_vision: false, convert_dolby_vision: false,
        hdr10_requested: false, grade: plurx_core::transcode::OutputGrade::Sdr, normalized_geometry: true, profile: None,
        width: 640, height: 360, video_identity: "a".repeat(64), pipeline_identity: "b".repeat(64),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::media_sessions::unix_ms();
    let dedupe = format!("copy-{tag}");
    assert!(matches!(store.enqueue_job(EnqueueJob {
        id: id.clone(), payload: JobPayload::CopyOutputPrepare {
            copy_output_version: 1, file_id: file.id, source_generation: "g".repeat(16),
            source_size: file.size, source_mtime: file.mtime, source_object_version: "g".repeat(16),
            policy_generation: "copy:1".into(), intent, scratch_bytes: 64 << 20,
            candidate_catalog: None, reason: "recent_demand".into(),
        }, dedupe_key: dedupe.clone(), priority: 1, not_before_ms: now, now_ms: now,
        request: JobRequest {
            scope: "copy".into(), request_id: dedupe, request_digest: "c".repeat(64),
            consumer_kind: "copy_output".into(), consumer_ref: file.id.to_string(),
            target_node_id: Some(target.into()), deadline_ms: None, retain_identity: false,
        },
    }).await.expect("enqueue"), EnqueueOutcome::Accepted { .. }));
    id
}

async fn d2_store_with_file() -> (Arc<dyn Store>, MediaFile, tempfile::TempDir) {
    let base = crate::test_tempdir().expect("d2");
    let path = base.path().join("source.mkv");
    std::fs::copy(fixture_file().path, &path).expect("owned source");
    let file = media_file_at(path, 12_000);
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let library = store.create_library(&plurx_core::domain::NewLibrary {
        name: "d2".into(), kind: plurx_core::domain::LibraryKind::Movies,
        paths: vec![base.path().to_owned()], anime: false,
    }).await.expect("library");
    let item = store.insert_item(&plurx_core::domain::NewItem {
        library_id: library.id, kind: plurx_core::domain::ItemKind::Movie,
        parent_id: None, title: "d2".into(), year: None, season_number: None, episode_number: None,
    }).await.expect("item");
    let probe = plurx_core::domain::ProbeResult {
        duration_ms: file.duration_ms, container: file.container.clone(),
        video_codec: file.video_codec.clone(), width: file.width, height: file.height,
        ..Default::default()
    };
    assert_eq!(store.upsert_file(item, &file.path.to_string_lossy(), file.size, file.mtime, &probe)
        .await.expect("file"), file.id);
    (store, file, base)
}

async fn d2_job_state(store: &Arc<dyn Store>, id: &str) -> plurx_core::store::background_jobs::JobState {
    store.background_job(id).await.expect("job").expect("row").state
}

#[tokio::test]
async fn disabled_queued_rows_are_cancelled_without_a_claim() {
    use plurx_core::store::background_jobs::JobState;
    let (store, file, base) = d2_store_with_file().await;
    // Schedule off, no capacity, and a target node that does not exist: a
    // claim could never take this row. The drain still cancels it.
    let id = d2_queue_copy_row(&store, &file, "absent-node", "absent").await;
    let jobs = Arc::new(crate::state::JobManager::new(Arc::clone(&store), base.path().join("art")));
    assert_eq!(jobs.drain_disabled_output_preparation(crate::vodserve::OutputPreparation::Off).await, 1);
    assert_eq!(d2_job_state(&store, &id).await, JobState::Cancelled);
    // A second node (or tick) finds nothing left.
    assert_eq!(jobs.drain_disabled_output_preparation(crate::vodserve::OutputPreparation::Off).await, 0);
    // An admitted kind is left alone.
    let kept = d2_queue_copy_row(&store, &file, "absent-node", "kept").await;
    assert_eq!(jobs.drain_disabled_output_preparation(crate::vodserve::OutputPreparation::Copy).await, 0);
    assert_eq!(d2_job_state(&store, &kept).await, JobState::Queued);
}

#[tokio::test]
async fn a_row_claimed_during_the_drain_settles_and_never_stays_cancelling() {
    use plurx_core::store::background_jobs::*;
    let (store, file, base) = d2_store_with_file().await;
    let jobs = Arc::new(crate::state::JobManager::new(Arc::clone(&store), base.path().join("art")));
    // The drain lists the row as queued; a node claims it in the window
    // between that list and the drain's cancel. The hook is that window.
    let token = Arc::new(std::sync::Mutex::new(None));
    let claim_in_window = |job_id: String| {
        let store = Arc::clone(&store);
        let token = Arc::clone(&token);
        async move {
            let queued = store.background_job(&job_id).await.expect("job").expect("row");
            assert_eq!(queued.state, JobState::Queued, "the drain listed it as queued");
            let now = crate::media_sessions::unix_ms();
            match store.claim_job(ClaimJob {
                job_id, expected_revision: queued.revision, node_id: "test-local".into(),
                boot_id: uuid::Uuid::new_v4().to_string(), claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::CopyOutputPrepare, payload_version: 1, now_ms: now, dispatched_at_ms: now,
            }).await.expect("claim") {
                ClaimOutcome::Claimed { job } => {
                    *token.lock().expect("token slot") = job.token;
                }
                other => panic!("claim {other:?}"),
            }
        }
    };
    // Executor alive: the drain's cancel lands on a running row, which goes
    // to cancelling, and its owner settles the cancel.
    let live = d2_queue_copy_row(&store, &file, "test-local", "live").await;
    assert_eq!(
        jobs.drain_disabled_output_preparation_with(
            crate::vodserve::OutputPreparation::Off, &claim_in_window).await,
        1,
        "the row claimed between list and cancel is still cancelled"
    );
    assert_eq!(d2_job_state(&store, &live).await, JobState::Cancelling,
        "a row claimed during the drain goes to cancelling, never back to queued");
    let claimed = token.lock().expect("token slot").take().expect("claimed in the window");
    let active = crate::background_jobs::ActiveBackgroundJob::start(
        Arc::clone(&store), Arc::new(CopyPreparationAuthority), claimed,
        tokio::time::Instant::now() + Duration::from_secs(30), JobKind::CopyOutputPrepare,
    ).expect("owner");
    active.fence().settle(JobSettlement::Cancel).await.expect("owner settles the cancel");
    active.finish().await;
    assert_eq!(d2_job_state(&store, &live).await, JobState::Cancelled);
    // Executor gone: the same race, and store upkeep settles at lease expiry.
    let orphaned = d2_queue_copy_row(&store, &file, "test-local", "orphaned").await;
    let now = crate::media_sessions::unix_ms();
    assert_eq!(
        jobs.drain_disabled_output_preparation_with(
            crate::vodserve::OutputPreparation::Off, &claim_in_window).await,
        1
    );
    let _ = token.lock().expect("token slot").take().expect("claimed in the window");
    assert_eq!(d2_job_state(&store, &orphaned).await, JobState::Cancelling);
    store.maintain_jobs(now + 24 * 60 * 60 * 1_000).await.expect("upkeep");
    assert_ne!(d2_job_state(&store, &orphaned).await, JobState::Cancelling,
        "a cancelling row whose executor is gone settles at lease expiry");
}

#[tokio::test]
async fn claim_pretranscode_honors_allowed_kinds() {
    use plurx_core::store::background_jobs::JobKind;
    let (store, file, base) = d2_store_with_file().await;
    d2_queue_copy_row(&store, &file, "test-local", "allowed").await;
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store), base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(), plurx_core::transcode::Pipeline::Cpu,
    ).with_cache(base.path().join("cache"), "d2-test".into(), "test-local".into());
    let mut capabilities = manager.pretranscode_capabilities();
    capabilities.scratch_bytes = i64::MAX;
    let claim = |allowed: &'static [JobKind]| {
        let store = Arc::clone(&store);
        let manager = &manager;
        let capabilities = capabilities.clone();
        async move {
            crate::background_jobs::claim_pretranscode(
                store, Arc::new(CopyPreparationAuthority), manager, "test-local",
                allowed, &capabilities, i64::MAX, i64::MAX, &[],
            ).await.expect("claim pass")
        }
    };
    assert!(claim(&[JobKind::TranscodePrepare]).await.is_none(),
        "the speculative lane never takes a viewer-demand row");
    // An output lane whose retained registry is full does not claim either.
    let full = crate::background_jobs::claim_pretranscode(
        Arc::clone(&store), Arc::new(CopyPreparationAuthority), &manager, "test-local",
        &[JobKind::CopyOutputPrepare], &capabilities, i64::MAX, 0, &[],
    ).await.expect("claim pass");
    assert!(full.is_none(), "output rows are bounded by the retained registry's remainder");
    match claim(&[JobKind::CopyOutputPrepare]).await {
        Some(crate::background_jobs::PreparationClaim::Copy(_, active, _)) => active.finish().await,
        _ => panic!("the copy lane claims its row"),
    }
}

#[tokio::test]
async fn output_prepare_rows_carry_a_deadline() {
    use plurx_core::store::background_jobs::*;
    let (store, file, base) = d2_store_with_file().await;
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store), base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(), plurx_core::transcode::Pipeline::Cpu,
    ).with_cache(base.path().join("cache"), "d2-deadline".into(), "test-local".into());
    let mut manual = request("deadline", 0.0);
    manual.audio_claim = Some(plurx_core::playback::audio::AudioClaim {
        decoders: vec!["aac".into()], sinks: vec![],
    });
    manual.audio_delivery = Some(plurx_core::playback::audio::resolve_audio(
        None, &manual.audio_claim.as_ref().expect("claim").profile(),
        plurx_core::playback::audio::AudioRoute::Progressive, 0,
    ));
    manager.enqueue_copy_output(&manual, &file, &settings()).await.expect("enqueue");
    let job = store.list_jobs(JobQuery { node_id: None, state: Some(JobState::Queued),
        kind: Some(JobKind::CopyOutputPrepare), after_id: None, limit: 10,
    }).await.expect("jobs").jobs.pop().expect("queued row");
    // Past the request retention window, viewer demand has expired: upkeep
    // retires the row instead of leaving it to hold a slot forever.
    let now = crate::media_sessions::unix_ms();
    store.maintain_jobs(now + REQUEST_RETENTION_MS + 60_000).await.expect("upkeep");
    assert_ne!(d2_job_state(&store, &job.id).await, JobState::Queued,
        "an output preparation row nothing executes expires");
}

async fn drain_private_cleanup(serve: &Arc<VodServe>) {
    let cleanup_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < cleanup_deadline {
        serve.shared.preparation_storage.maintain(&serve.shared).await;
        if serve.shared.preparation_storage.diagnostics(&serve.shared).await.pending_cleanup_count == 0 { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("private cleanup did not settle");
}

#[tokio::test]
async fn preparation_constructor_cancellation_before_marker_has_a_cleanup_owner() { prepared_copy_consumer(10).await; }
#[tokio::test]
async fn preparation_constructor_cancellation_after_installation_has_a_cleanup_owner() { prepared_copy_consumer(11).await; }
#[tokio::test]
async fn preparation_cancellation_during_fence_snapshot_has_a_cleanup_owner() { prepared_copy_consumer(12).await; }
#[tokio::test]
async fn preparation_publication_and_release_barrier_conserves_private_bytes() { prepared_copy_consumer(13).await; }
#[tokio::test]
async fn preparation_child_registration_barrier_retains_storage_until_confirmed_reap() { prepared_copy_consumer(14).await; }

#[tokio::test]
async fn prepared_copy_cleanup_failure_pins_retained_capacity_through_collection() { prepared_copy_consumer(5).await; }
#[tokio::test]
async fn preparation_partial_retained_assembly_failure_keeps_source_cleanup_owned() { prepared_copy_consumer(6).await; }

#[tokio::test]
async fn prepared_copy_cleanup_store_failure_keeps_marker_and_capacity_for_retry() { prepared_copy_consumer(7).await; }

struct PrivateWaitFailure { fail: AtomicBool, attempts: AtomicU64 }
impl crate::prodrun::ProducerReapHooks for PrivateWaitFailure {
    fn wait<'a>(&'a self, child: &'a mut tokio::process::Child) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<std::process::ExitStatus>> + Send + 'a>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, AcqRel);
            if self.fail.load(Acquire) { Err(io::ErrorKind::Interrupted.into()) } else { child.wait().await }
        })
    }
}

#[tokio::test]
async fn preparation_real_wait_and_unlink_failure_retries_preserve_capacity_and_cleanup_fairness() { prepared_copy_consumer(15).await; }
