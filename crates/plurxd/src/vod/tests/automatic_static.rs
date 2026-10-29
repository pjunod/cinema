// Authored qualification fixture: real selected store/runtime and FFmpeg.
// Missing native tools/capacity fail explicitly; compilation is not execution.
#[tokio::test]
async fn automatic_selected_static_carrier_uses_typed_custody_and_counted_retained_output() {
    use plurx_core::playback::candidate::CandidateRoute;
    use plurx_core::store::background_jobs::*;
    eprintln!("automatic static fixture: entered original fixture body");
    testfixtures::require_ffmpeg();
    let base = crate::test_tempdir().expect("automatic encoded preparation");
    let path = base.path().join("source.mkv");
    eprintln!("automatic static fixture: before actual media fixture construction");
    std::fs::copy(testfixtures::source_with_eac3_audio(), &path).expect("owned source");
    eprintln!("automatic static fixture: actual media fixture returned");
    let preliminary = media_file_at(path.clone(), 12_000);
    eprintln!("automatic static fixture: before original held-source open and probe");
    let source = crate::fragment_index_cluster::open_source_fence(&preliminary, None)
        .await
        .expect("held source");
    let original_source_version = source.object_version().to_owned();
    let raw = crate::ffmpeg::held_source_probe_json(
        &source.handle,
        crate::process_control::ChildWork::background("automatic carrier fixture probe"),
    )
    .await
    .expect("real probe");
    let probe =
        plurx_core::scan::probe::parse_probe_json(&serde_json::from_str(&raw).expect("probe JSON"));
    eprintln!("automatic static fixture: actual held-source probe returned");
    let mut config = plurx_core::config::Config::default();
    config.storage.data_dir = base.path().join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture API port");
    config.cluster.raft_bind = raft.local_addr().expect("fixture Raft address");
    config.cluster.api_bind = api.local_addr().expect("fixture API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    crate::canonicalize_storage_roots(&mut config)
        .expect("production non-destructive storage preflight before Store ownership");
    eprintln!("automatic static fixture: before original Store selector");
    // Only Store bootstrap uses the existing owned debug-frame allowance;
    // the actual producer and native workers stay on this original runtime.
    let mut selected_store =
        crate::http::shared_source_playback::source_http_fixture_store(config.clone(), None)
            .await
            .expect("actual selected singleton");
    eprintln!("automatic static fixture: original Store selector returned");
    selected_store
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
        .await
        .expect("save Source enablement before the real single-use schema startup");
    assert!(
        Box::pin(selected_store.prepare_source_schema_before_serving())
            .await
            .expect("source schema")
    );
    let runtime = crate::resource_runtime::open_before_cleanup(&mut selected_store, &config)
        .await
        .expect("actual startup custody")
        .expect("Linux runtime");
    let resource_baseline = runtime.accounting().expect("initial actual ledger").used;
    let dirs = crate::create_dirs_for_config_with_runtime(&config, Some(runtime.as_ref()))
        .expect("managed directories under actual startup custody");
    let store = Arc::clone(&selected_store.store);
    let playback_cache = crate::playback_cache::PlaybackCacheBudget::new(
        Arc::clone(&store),
        vec![dirs.renditions.clone()],
    );
    let node = selected_store.identity.node_id.clone();
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
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "0")
        .await
        .expect("serialized selected static compatibility policy");
    store
        .put_setting(plurx_core::store::keys::HWACCEL, "software")
        .await
        .expect("CPU");
    store
        .put_setting("playback.vod_reorder_frames", "0")
        .await
        .expect("frozen off choice");
    let serve = VodServe::new_cluster_with_runtime(
        dirs.renditions.clone(),
        dirs.runtime_cache.clone(),
        Arc::clone(&store),
        node.clone(),
        dirs.cache.join("cluster-index"),
        None,
        Some(Arc::clone(&runtime)),
    );
    let decode_probe = crate::decode_facts::DecodeProbeIdentity::discover_fixture(
        &crate::ffmpeg::bound_ffprobe_bin(),
    )
    .await
    .expect("actual bound fixture FFprobe identity");
    serve.register_playback_cache(&playback_cache);
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        base.path().join("work"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    )
    .with_cache_layout(
        dirs.cache.clone(),
        dirs.runtime_cache.clone(),
        dirs.subs.clone(),
        dirs.renditions.clone(),
        "automatic-carrier-test".into(),
        node.clone(),
        None,
        Some(Arc::clone(&runtime)),
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
    let planning = Arc::new(planning);
    context.planning_snapshot = Some(Arc::clone(&planning));
    context.owner_node_id = Some(node.clone());
    selected.candidate_context = Some(Box::new(context));
    let mut resolved_file = file.clone();
    resolved_file.audio_offset_ms = 250;
    let frame_bytes = {
        let original = manager.resolve_selected_static_output_test(
            &selected,
            &resolved_file,
            Arc::clone(&planning),
            original_source_version.clone(),
            0,
            Instant::now() + Duration::from_secs(40),
        );
        std::mem::size_of_val(&original)
    };
    let (encoding, mut catalog_cleanup, catalog_application) = match Box::pin(
        manager.resolve_selected_static_output_test(
            &selected,
            &resolved_file,
            Arc::clone(&planning),
            original_source_version.clone(),
            frame_bytes,
            Instant::now() + Duration::from_secs(40),
        ),
    )
    .await
    {
        Ok(original) => original,
        Err(error) => {
            let namespace = base.keep();
            panic!("actual admitted canonical resolver failed: {error:?}; original fixture retained at {}", namespace.display());
        }
    };
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
            node_id: node.clone(),
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
    let controller = crate::background_jobs::BackgroundJobController::new(*claimed, active);
    let fence = controller.fence();
    let heavy = manager
        .admit_encoded_preparation()
        .expect("existing heavy lane");
    let observation = manager.copy_preparation_attachment_observation();
    assert!(
        manager.uses_selected_static_intent(&file, &intent),
        "automatic claim uses the same selected predicate as state.rs"
    );
    let compatibility_pool = encoding.admissions.clone();
    let compatibility_baseline = compatibility_pool.snapshot();
    // Only this shared admissions service survives the catalog recipe. It has
    // no report/native ownership. Destroy Encoding before its original M lease.
    drop(encoding);
    drop(catalog_application);
    catalog_cleanup
        .finish_retirement(Instant::now() + Duration::from_secs(40))
        .await
        .expect("original catalog P/M positive cleanup");
    assert!(catalog_cleanup.known_closed());
    assert_eq!(
        runtime.accounting().expect("catalog fixture retired").used,
        resource_baseline
    );
    drop(catalog_cleanup);
    let legacy = compatibility_pool
        .try_admit_software(0, 1, crate::admission::Priority::Live)
        .expect("actual original legacy owner");
    let deadline = Instant::now() + Duration::from_secs(40);
    let error = manager
        .produce_encoded_output_job(
            &file,
            controller.job(),
            fence.clone(),
            deadline,
            observation,
        )
        .await
        .expect_err("selected background R must not bypass the live legacy owner");
    assert!(
        matches!(error, crate::background_jobs::PreparationError::Retry(ref message)
        if message.contains("compatibility pool")),
        "actual compatibility refusal: {error:?}"
    );
    assert_eq!(
        compatibility_pool.snapshot().software_used,
        compatibility_baseline.software_used + 1,
        "pre-R refusal leaves the same original legacy claim"
    );
    drop(legacy);
    assert_eq!(compatibility_pool.snapshot(), compatibility_baseline);
    assert!(manager
        .produce_encoded_output_job(
            &file,
            controller.job(),
            fence.clone(),
            deadline,
            observation
        )
        .await
        .expect("same original automatic owner resumes under zero policy"));
    assert_eq!(
        compatibility_pool.snapshot(),
        compatibility_baseline,
        "known original static R-to-S releases compatibility credit without counting retained S as CPU work"
    );
    drop(heavy);
    drop(fence);
    let _ = controller.finish().await;
    assert_eq!(
        runtime
            .accounting()
            .expect("post-producer ledger")
            .unknown_peak_producers,
        0,
        "original P and R producers settle before exposing dormant S"
    );
    assert_eq!(
        store
            .background_job(&job.id)
            .await
            .expect("job")
            .expect("row")
            .state,
        JobState::Succeeded
    );
    let frame_bytes = {
        let original = manager.resolve_selected_static_output_test(
            &selected,
            &resolved_file,
            Arc::clone(&planning),
            original_source_version.clone(),
            0,
            Instant::now() + Duration::from_secs(40),
        );
        std::mem::size_of_val(&original)
    };
    let (encoding, mut attachment_cleanup, attachment_application) = match Box::pin(
        manager.resolve_selected_static_output_test(
            &selected,
            &resolved_file,
            Arc::clone(&planning),
            original_source_version.clone(),
            frame_bytes,
            Instant::now() + Duration::from_secs(40),
        ),
    )
    .await
    {
        Ok(original) => original,
        Err(error) => {
            let namespace = base.keep();
            panic!("actual admitted attachment resolver failed: {error:?}; original fixture retained at {}", namespace.display());
        }
    };
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
                root_admission: None,
                soundtrack: None,
                companion: None,
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
    let output = artifact
        .finite_output
        .as_ref()
        .expect("automatic output came from typed finite R-to-S")
        .clone();
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
    let policy_facts = artifact.facts();
    let policy_identity = artifact.observation.rates.identity;
    let rendition = Arc::clone(session.live_rendition().expect("rendition"));
    let logical = rendition.recipe.retained_logical.clone();
    drop(sessions);
    let init = plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Init;
    let len = output
        .file_len(init)
        .await
        .expect("same retained typed init")
        .expect("init exists");
    let mut ready = ReadyFile::Finite {
        output: output.clone(),
        file: init,
        len,
    };
    let body = ready
        .read_all_bounded(len, Instant::now() + Duration::from_secs(5))
        .await
        .expect("counted retained init body");
    assert!(matches!(&body, ReadyBytes::Finite(_)));
    assert_eq!(body.len() as u64, len);
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
    let retained_charge = serve.shared.retained_artifacts.test_capacity_snapshot().0;
    assert!(retained_charge > 0);
    store
        .put_setting(plurx_core::store::keys::HWACCEL, "vaapi")
        .await
        .expect("current policy changes after exposure");
    serve
        .shared
        .retained_artifacts
        .reconcile_static_policy(store.as_ref())
        .await;
    assert!(!serve
        .shared
        .retained_artifacts
        .test_has_artifact(&policy_facts));
    assert!(
        serve
            .shared
            .retained_artifacts
            .acquire(&policy_identity)
            .is_none(),
        "policy withdrawal prevents a new attachment before any refund"
    );
    assert_eq!(
        serve.shared.retained_artifacts.test_capacity_snapshot().0,
        retained_charge,
        "withdrawal alone cannot release the original S charge"
    );
    let original_static_path = output.path().to_owned();
    drop(ready);
    drop(output);
    drop(rendition);
    drop(logical);
    assert!(serve.end("later-automatic", Terminal::Deleted).await);
    assert!(!super::cache_budget::pressure_evict_test(&serve.shared, &original_static_path)
        .await.expect("actual cache pressure callback"),
        "fresh typed S must not fall through to raw namespace deletion while its actual body is live");
    assert_eq!(
        serve.shared.retained_artifacts.test_capacity_snapshot().0,
        retained_charge,
        "pressure alone cannot refund the original live-body S"
    );
    serve.shared.retained_artifacts.collect(base.path()).await;
    assert_eq!(
        serve.shared.retained_artifacts.test_capacity_snapshot().0,
        retained_charge,
        "the counted HTTP body still prevents native S settlement"
    );
    drop(body);
    // Existing maintenance resumes the same queued original eviction; no
    // replacement native operation or daemon/path-based refund is minted.
    for _ in 0..16 {
        serve.shared.retained_artifacts.collect(base.path()).await;
        if serve.shared.retained_artifacts.test_capacity_snapshot().0 == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        serve.shared.retained_artifacts.test_capacity_snapshot().0,
        0,
        "only positive original native/journal completion releases static S"
    );
    drop(manager);
    drop(serve);
    // Serve has destroyed every actual rendition/Encoding alias. The separate
    // original application lease then permits the same M journal retirement.
    drop(attachment_application);
    attachment_cleanup
        .finish_retirement(Instant::now() + Duration::from_secs(40))
        .await
        .expect("original attachment P/M positive cleanup after Serve aliases");
    assert!(attachment_cleanup.known_closed());
    drop(attachment_cleanup);
    let until = Instant::now() + Duration::from_secs(40);
    while runtime
        .accounting()
        .expect("original static cleanup ledger")
        .used
        != resource_baseline
    {
        assert!(
            Instant::now() < until,
            "original background P/M cleanup did not finish"
        );
        runtime.maintain_planning_cleanup(until).await;
        tokio::task::yield_now().await;
    }
    drop(runtime);
    selected_store
        .shutdown()
        .await
        .expect("shutdown selected singleton");
}

#[tokio::test]
async fn static_zero_compatibility_claim_survives_original_no_entry_ticket_and_application_alias() {
    let fixture = finite_cancellation_fixture().await;
    fixture
        .serve
        .shared
        .store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "0")
        .await
        .expect("actual serialized static policy");
    let encoding = Arc::clone(
        fixture
            .recipe
            .encoding
            .as_ref()
            .expect("actual frozen request"),
    );
    let compatibility_baseline = encoding.admissions.snapshot();
    let baseline = fixture
        .runtime
        .accounting()
        .expect("actual node baseline")
        .used;
    let snapshot = fixture
        .serve
        .shared
        .store
        .playback_planning_snapshot(
            fixture.recipe.file.id,
            &crate::transcode::QUALITY_PLANNING_KEYS,
        )
        .await
        .expect("actual coherent snapshot")
        .expect("source row");
    let policy = background_artifact::StaticArtifactPolicy::from_snapshot(&snapshot)
        .expect("actual static policy identity");
    let mut selected = request("static-original-compatibility", 0.0);
    selected.file_id = fixture.recipe.file.id;
    selected.kind = SessionKind::Transcode { height: 360 };
    selected.audio_index = None;
    selected.audio_claim = None;
    selected.audio_delivery = encoding.options.audio.clone();
    let mut request_recipe = VodRecipeRequest {
        request: &selected,
        root_admission: None,
        soundtrack: None,
        companion: None,
        encoding: Some(Arc::clone(&encoding)),
        retained_capture: RetainedOutputCapture::New,
        measured_candidate: None,
    };
    let prepared = fixture
        .serve
        .resolve_adaptive_recipe(&mut request_recipe, &fixture.recipe.file, &fixture.settings)
        .await
        .expect("same actual selected recipe");
    let deadline = Instant::now() + Duration::from_secs(90);
    let planned = fixture
        .serve
        .plan_static_artifact(
            prepared,
            policy,
            std::mem::size_of::<background_artifact::StaticArtifactRun>(),
            crate::transcode::TranscodeManager::static_handoff_limit_for_test(),
            deadline,
        )
        .await
        .expect("actual static plan and zero-compatible original claim");
    let before_r = encoding.admissions.snapshot().software_used;
    assert!(before_r > compatibility_baseline.software_used);
    let mut run = match fixture.serve.admit_static_artifact(planned, deadline) {
        Ok(run) => run,
        Err(error) => panic!("actual static R must admit: {}", error.into_parts().0),
    };
    let output = run.original_output_for_test();
    let mut ticket = run
        .prepare_cancel(deadline)
        .await
        .expect("same original no-entry typed close");
    let compatibility = run
        .take_compatibility()
        .expect("same original worker claim");
    let quota = run
        .take_cache_grant()
        .expect("same original operator quota");
    drop(run);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(20))
            .await
            .is_err(),
        "actual retained output alias blocks the same original application barrier"
    );
    assert_eq!(encoding.admissions.snapshot().software_used, before_r);
    assert!(
        encoding
            .admissions
            .try_admit_software(0, 1, crate::admission::Priority::Live)
            .is_none(),
        "no legacy zero-budget bypass while original close is unresolved"
    );
    let path = output.path().to_owned();
    assert!(crate::playback_cache::is_pinned(&path));
    drop(output);
    ticket
        .finish_retirement(deadline)
        .await
        .expect("actual original native/app close");
    compatibility.known_retired();
    quota.known_closed();
    assert_eq!(encoding.admissions.snapshot(), compatibility_baseline);
    assert!(!crate::playback_cache::is_pinned(&path));
    assert_eq!(
        fixture
            .runtime
            .accounting()
            .expect("same original refund")
            .used,
        baseline
    );
}
