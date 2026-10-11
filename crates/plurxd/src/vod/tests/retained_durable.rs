#[tokio::test]
async fn durable_restore_checks_incoming_tuple_when_healthy_same_key_rendition_is_reused() {
    let base = crate::test_tempdir().expect("incoming provenance consumer");
    let (serve, file) = serve_on(base.path()).await;
    create(&serve, &file, "incumbent", "original", &settings()).await;
    let rendition = serve.shared.sessions.lock().await.get("incumbent").expect("incumbent")
        .rendition.clone().expect("rendition");
    for index in 0..rendition.plan.len() {
        drop(fetch(&serve, "incumbent", &segment_name(index as u64)).await);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let artifact = loop {
        let rates = rendition.output_measurement.lock().expect("observer").complete_rates();
        if let Some(artifact) = rates.and_then(|rates| serve.shared.retained_artifacts.acquire(&rates.identity)) {
            break artifact;
        }
        assert!(Instant::now() < deadline, "real complete artifact unavailable");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let facts = artifact.facts();
    let original = request("incumbent", 0.0);
    serve.try_create(VodRecipeRequest { soundtrack: None, companion: None,
        measured_candidate: None, retained_capture: RetainedOutputCapture::Restore(Some(facts.clone())),
        request: &original, encoding: None,
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "Fixture", supersession_user: "user" }, "incumbent".into())
        .await.expect("same tuple positive control");
    let incumbent = {
        let sessions = serve.shared.sessions.lock().await;
        let session = sessions.get("incumbent").expect("issued incumbent");
        assert!(Arc::ptr_eq(session.rendition.as_ref().expect("shared rendition"), &rendition));
        Arc::clone(&session.incarnation)
    };
    let mut different = original.clone();
    different.audio_claim = Some(plurx_core::playback::audio::AudioClaim { decoders: vec!["aac".into()], sinks: vec![] });
    let refused = serve.try_create(VodRecipeRequest { soundtrack: None, companion: None,
        measured_candidate: None, retained_capture: RetainedOutputCapture::Restore(Some(facts.clone())),
        request: &different, encoding: None,
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "Fixture", supersession_user: "user" }, "incumbent".into())
        .await.expect_err("different incoming provenance must not borrow old tuple");
    assert_eq!(crate::transcode::vod_refusal(&refused).expect("typed refusal").0, "retained_artifact_unavailable");
    {
        let sessions = serve.shared.sessions.lock().await;
        let preserved = sessions.get("incumbent").expect("preserved incumbent");
        assert!(Arc::ptr_eq(&incumbent, &preserved.incarnation));
        assert_eq!(preserved.retained_output.as_ref().expect("issued facts").facts(), facts);
    }
    let init = serve.segment("incumbent", INIT_NAME).await.expect("incumbent remains served");
    assert!(init.result.expect("private init").expect("init").retained_lease.is_some());
    serve.try_create(VodRecipeRequest { soundtrack: None, companion: None,
        measured_candidate: None, retained_capture: RetainedOutputCapture::New,
        request: &different, encoding: None,
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "Fixture", supersession_user: "user" }, "uncaptured".into())
        .await.expect("ordinary different tuple remains playable without borrowed proof");
    assert!(serve.shared.sessions.lock().await.get("uncaptured").expect("ordinary session").retained_output.is_none());
    serve.end("incumbent", Terminal::Deleted).await;
    serve.end("uncaptured", Terminal::Deleted).await;
}

#[tokio::test]
async fn durable_restore_preserves_none_incumbent_and_original_execution_identity() {
    let _ = tracing_subscriber::fmt().with_env_filter("plurxd::vodserve=debug").with_test_writer().try_init();
    let base = crate::test_tempdir().expect("durable real consumer");
    let (old, file) = serve_on(base.path()).await;
    let store = Arc::clone(&old.shared.store);
    create(&old, &file, "initial", "original", &settings()).await;
    let rendition = old.shared.sessions.lock().await.get("initial").expect("session")
        .rendition.clone().expect("rendition");
    for index in 0..rendition.plan.len() {
        drop(fetch(&old, "initial", &segment_name(index as u64)).await);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rates = rendition.output_measurement.lock().expect("measurement").complete_rates();
        if rates.is_some_and(|r| old.shared.retained_artifacts.acquire(&r.identity).is_some()) { break; }
        assert!(Instant::now() < deadline, "real completed producer did not retain artifact: rates={rates:?}, epoch={}", rendition.gen_epoch.load(Acquire));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    create(&old, &file, "issued", "issued", &settings()).await;
    let facts = old.shared.sessions.lock().await.get("issued").expect("issued")
        .retained_output.as_ref().expect("captured proof").facts();
    let weak = Arc::downgrade(&old.shared);
    // Model process shutdown, not a new owner restoring from mutable recipe
    // files. Settle the old driver before opening the leased namespace anew.
    rendition.closed.store(true, Release);
    rendition.kick();
    drop(rendition);
    drop(old);
    while weak.upgrade().is_some() {
        assert!(Instant::now() < deadline, "old driver did not settle");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let fresh = local_serve(base.path().to_owned(), store);
    let restored_request = request("issued", 0.0);
    fresh.try_create(VodRecipeRequest { soundtrack: None, companion: None,
        measured_candidate: None, retained_capture: RetainedOutputCapture::Restore(Some(facts.clone())),
        request: &restored_request, encoding: None,
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "Fixture", supersession_user: "user" }, "restored".into())
        .await.expect("actual typed Restore consumer");
    let publication = fresh.segment("restored", INIT_NAME).await.expect("issued init GET");
    assert_eq!(publication.owner.retained_output_facts(), Some(facts.clone()));
    assert!(publication.result.expect("private init").expect("init file").retained_lease.is_some());
    let none_request = request("none", 0.0);
    fresh.try_create(VodRecipeRequest { soundtrack: None, companion: None,
        measured_candidate: None, retained_capture: RetainedOutputCapture::Restore(None),
        request: &none_request, encoding: None,
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "Fixture", supersession_user: "user" }, "none".into())
        .await.expect("legacy None remains playable");
    assert!(fresh.shared.sessions.lock().await.get("none").expect("None session").retained_output.is_none());
    let mut contradictory = facts;
    contradictory.output_identity = "f".repeat(64);
    let failed = fresh.try_create(VodRecipeRequest { soundtrack: None, companion: None,
        measured_candidate: None, retained_capture: RetainedOutputCapture::Restore(Some(contradictory)),
        request: &restored_request, encoding: None,
    }, &file, &settings(), VodAttribution { user_name: "paul", item_title: "Fixture", supersession_user: "user" }, "refused".into())
        .await.expect_err("contradictory origin never replaces a session");
    assert_eq!(crate::transcode::vod_refusal(&failed).expect("typed refusal").0, "retained_artifact_unavailable");
    assert!(fresh.owns("restored").await);
    assert!(!fresh.owns("refused").await);
    fresh.end("restored", Terminal::Deleted).await;
    fresh.end("none", Terminal::Deleted).await;
}

#[tokio::test]
async fn durable_completion_requires_actual_trailer_and_refuses_mixed_or_repeated_publication() {
    let base = crate::test_tempdir().expect("real trailer negative control");
    let file = fixture_file();
    let (store, index) = store_with_index(&file).await;
    let serve = local_serve(base.path().to_owned(), store);
    let video = CopyVideoOptions::new(crate::ffmpeg::has_dovi_rpu().await, false);
    let recipe = Recipe {
        retained_logical: None, measured_candidate: None, file: file.clone(),
        audio_index: None, aac: true, audio_delivery: None, video,
        source_object_version: None, cluster_cache_key: None, encoding: None,
    };
    let policy = shipped_policy(index.timescale);
    let plan = plurx_core::segplan::plan_copy(&index, &policy,
        &track_durations(&index, &recipe, file.duration_ms.expect("duration")));
    let key = rendition_key(&recipe, &crate::fragindex::identity_for(&file, video));
    let rendition = serve.shared.build_rendition(&key, Some(index.clone()), recipe, plan, &settings())
        .await.expect("actual indexed copy rendition");
    let output = std::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args(recipe_pipe_args(&rendition.recipe, 0.0, false)).output().expect("bounded fixture pipe");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let mut reader = FragmentReader::new();
    reader.push(&output.stdout);
    let muxer = loop {
        match reader.next_unit().expect("parse fixture pipe") {
            Some(Unit::Init(init)) => break init,
            Some(_) => {},
            None => panic!("fixture lacks init"),
        }
    };
    let identity = InitIdentity::establish(&muxer, index.promotion.clone()).expect("actual init");
    rendition.dir.write_init(&identity.served_init_for(&muxer).expect("served init").bytes).await.expect("init commit");
    rendition.identity.lock().await.identity = Some(identity.clone());
    let mut at = 0usize;
    let mut trailer = None;
    while at.checked_add(8).is_some_and(|end| end <= output.stdout.len()) {
        let size = u32::from_be_bytes(output.stdout[at..at + 4].try_into().expect("box size")) as usize;
        assert!(size >= 8 && at.checked_add(size).is_some_and(|end| end <= output.stdout.len()));
        if &output.stdout[at + 4..at + 8] == b"mfra" { trailer = Some(at); break; }
        at += size;
    }
    let cut = trailer.expect("real FFmpeg normal trailer");
    let sink = RenditionSink { retirement: tokio_util::sync::CancellationToken::new(), shared: Arc::clone(&serve.shared), rendition: Arc::clone(&rendition), epoch: 0 };
    let outcome = crate::vodgen::run(&output.stdout[..cut], crate::vodgen::Generation {
        plan: rendition.plan.clone(), index: Some(index), encoded_audio_anchor: None,
        encoded_frame_ticks: None,
        encoded_video_origin: None,
        identity: identity.clone(), convert_dolby_vision: false,
        retain_hevc_parameter_sets: false, start_entry: 0, policy,
    }, &sink, "durable-trailer-negative").await;
    assert!(matches!(outcome, Outcome::Ran { .. }), "trailerless EOF is not normal completion: {outcome:?}");
    assert!(rendition.output_measurement.lock().expect("observer").complete_observation().is_none(), "no trailer never completes proof");
    for duplicate in [false, true] {
        let mut observer = PublishedOutputMeasurement::default();
        let member = output_measurement::ObservedOutputMember { bytes: 1000, digest: [9; 32], publication: 1 };
        for entry in 0..rendition.plan.len() { observer.observe(&rendition, &identity, 0, entry as u32, member.clone()); }
        observer.complete(0, true);
        assert!(observer.complete_rates().is_some(), "the control is complete before poisoning");
        observer.observe(&rendition, &identity, u64::from(!duplicate), 0, member.clone());
        observer.complete(0, true);
        assert!(observer.complete_observation().is_none(), "new-epoch and repeated publication cannot inherit proof");
    }
}
