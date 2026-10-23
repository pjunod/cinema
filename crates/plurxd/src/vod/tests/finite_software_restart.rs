// Qualification fixture: Linux, real FFmpeg/FFprobe, a selected local store,
// and enough physical capacity for the fixed software profile. Missing tools,
// custody import, or a live prior owner fail explicitly. The child process
// exits before the parent opens the SAME persisted installation, so startup's
// previous-owner-exit proof is real rather than a simulated runtime drop.
const FINITE_RESTART_PHASE: &str = "PLURX_FINITE_SOFTWARE_RESTART_CHILD_ROOT";
const FINITE_RESTART_TEST: &str =
    "vodserve::tests::ordinary_selected_finite_vod_reuses_dormant_s_after_process_restart";

#[tokio::test]
async fn ordinary_selected_finite_vod_reuses_dormant_s_after_process_restart() {
    testfixtures::require_ffmpeg();
    if let Some(root) = std::env::var_os(FINITE_RESTART_PHASE) {
        finite_restart_child(std::path::Path::new(&root)).await;
        return;
    }

    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("repo-local fixture root");
    let fixture = tempfile::Builder::new()
        .prefix("finite-software-restart-")
        .tempdir_in(&target)
        .expect("repo-local restart fixture");
    let source_path = fixture.path().join("source.mkv");
    testfixtures::run(
        std::process::Command::new(testfixtures::ffmpeg())
            .args(["-v", "error", "-y", "-i"])
            .arg(testfixtures::source_with_eac3_audio())
            .args(["-t", "2", "-an", "-c:v", "copy"])
            .arg(&source_path),
    );

    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
    child
        .arg("--exact")
        .arg(FINITE_RESTART_TEST)
        .arg("--nocapture")
        .env(FINITE_RESTART_PHASE, fixture.path())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(240), child.status())
        .await
        .expect("first daemon process timed out")
        .expect("first daemon process failed to start");
    assert!(
        status.success(),
        "first daemon process must exit successfully"
    );

    let (config, selected, runtime) = finite_restart_open(fixture.path()).await;
    let initial = runtime.accounting().expect("recovered accounting");
    assert_eq!(initial.unknown_peak_producers, 0);
    assert_eq!(initial.held, 1, "the child left one charged dormant S");
    let file_id: i64 = std::fs::read_to_string(fixture.path().join("media-id"))
        .expect("child persisted its actual Store file id")
        .parse()
        .expect("Store file id");
    let expected_key = std::fs::read_to_string(fixture.path().join("rendition-key"))
        .expect("child persisted its actual rendition key");
    let file = selected
        .store
        .get_file(file_id)
        .await
        .expect("persisted media lookup")
        .expect("persisted media row");
    assert_eq!(file.path, source_path);
    let (serve, rendition) = finite_restart_attach(&config, &selected, &runtime, &file).await;
    assert_eq!(rendition.key, expected_key, "same finite recipe key");
    assert!(rendition.dir.is_finite(), "ordinary route imports finite S");
    assert!(
        rendition.dir.finite_producer().is_none(),
        "cold reuse must not admit a fresh encoder"
    );
    let output = rendition
        .dir
        .finite_output()
        .expect("imported native S owner");
    let deadline = Instant::now() + Duration::from_secs(90);
    for file in [
        plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Init,
        plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Segment(0),
    ] {
        let len = output
            .file_len(file)
            .await
            .expect("recovered inventory lookup")
            .expect("recovered published file");
        let mut ready = ReadyFile::Finite {
            output: output.clone(),
            file,
            len,
        };
        let body = ready
            .read_all_bounded(len, deadline)
            .await
            .expect("counted recovered HTTP body");
        assert!(matches!(&body, ReadyBytes::Finite(_)));
        assert_eq!(body.len() as u64, len);
        let marker = if matches!(
            file,
            plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Init
        ) {
            b"moov"
        } else {
            b"moof"
        };
        assert!(body.windows(4).any(|window| window == marker));
        drop(body);
    }
    let after_read = runtime.accounting().expect("cold reuse accounting");
    assert_eq!(after_read.unknown_peak_producers, 0);
    assert_eq!(after_read.held, 1);

    assert!(
        serve
            .end("finite-software-restart", Terminal::Deleted)
            .await
    );
    output
        .evict(Instant::now() + Duration::from_secs(90))
        .await
        .expect("imported S native cleanup and exact refund");
    let final_snapshot = runtime.accounting().expect("final accounting");
    assert_eq!(final_snapshot.held, 0);
    assert_eq!(final_snapshot.used, Default::default());
    drop(output);
    drop(rendition);
    drop(serve);
    let runtime =
        Arc::try_unwrap(runtime).unwrap_or_else(|_| panic!("recovered runtime still owned"));
    runtime.drain().expect("drain recovered custody worker");
    selected.shutdown().await.expect("shutdown recovered voter");
    drop(selected);
}

async fn finite_restart_child(root: &std::path::Path) {
    let source_path = root.join("source.mkv");
    let preliminary = media_file_at(source_path.clone(), 2_000);
    let source = crate::fragment_index_cluster::open_source_fence(&preliminary, None)
        .await
        .expect("actual held source");
    let probe_json = crate::ffmpeg::held_source_probe_json(
        &source.handle,
        crate::process_control::ChildWork::background("restart fixture probe"),
    )
    .await
    .expect("actual source probe");
    let probe = plurx_core::scan::probe::parse_probe_json(
        &serde_json::from_str(&probe_json).expect("probe JSON"),
    );
    drop(source);

    let (config, selected, runtime) = finite_restart_open(root).await;
    let store = Arc::clone(&selected.store);
    let library = store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "finite restart".into(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec![root.to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&plurx_core::domain::NewItem {
            library_id: library.id,
            kind: plurx_core::domain::ItemKind::Movie,
            parent_id: None,
            title: "finite restart".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let id = store
        .upsert_file(
            item,
            &source_path.to_string_lossy(),
            preliminary.size,
            preliminary.mtime,
            &probe,
        )
        .await
        .expect("source file");
    let file = store
        .get_file(id)
        .await
        .expect("source lookup")
        .expect("source row");
    std::fs::write(root.join("media-id"), id.to_string()).expect("fixture source row identity");
    let (serve, rendition) = finite_restart_attach(&config, &selected, &runtime, &file).await;
    assert!(
        rendition.dir.is_finite(),
        "ordinary route selects finite custody"
    );
    assert!(
        rendition.dir.finite_producer().is_some(),
        "first process owns the actual encoder"
    );
    let output = rendition
        .dir
        .finite_output()
        .expect("original finite output");
    let deadline = Instant::now() + Duration::from_secs(90);
    // The normal rendition driver wakes on its ordinary kick. This avoids an
    // HTTP materialization watchdog outliving the first daemon in the test.
    rendition.kick();
    let len = loop {
        assert!(Instant::now() < deadline, "first finite segment timed out");
        match output
            .file_len(plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Segment(0))
            .await
            .expect("first native publication state")
        {
            Some(len) => break len,
            None => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    let mut ready = ReadyFile::Finite {
        output: output.clone(),
        file: plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Segment(0),
        len,
    };
    let body = ready
        .read_all_bounded(len, deadline)
        .await
        .expect("first counted response");
    assert!(matches!(&body, ReadyBytes::Finite(_)));
    drop(body);
    wait_until(
        "first process R-to-S retirement",
        Duration::from_secs(90),
        || async {
            let snapshot = runtime.accounting().expect("retirement accounting");
            snapshot.unknown_peak_producers == 0 && snapshot.held == 1
        },
    )
    .await;
    std::fs::write(root.join("rendition-key"), &rendition.key)
        .expect("fixture actual rendition key");
    assert!(
        serve
            .end("finite-software-restart", Terminal::Deleted)
            .await
    );
    drop(output);
    drop(rendition);
    drop(serve);
    drop(store);
    let runtime = Arc::try_unwrap(runtime).unwrap_or_else(|_| panic!("first runtime still owned"));
    runtime.drain().expect("drain first custody worker");
    selected.shutdown().await.expect("shutdown first voter");
    drop(selected);
}

fn finite_restart_config(root: &std::path::Path) -> plurx_core::config::Config {
    let mut config = plurx_core::config::Config::default();
    config.storage.data_dir = root.join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    config
}

async fn finite_restart_open(
    root: &std::path::Path,
) -> (
    plurx_core::config::Config,
    plurx_core::cluster::migration::SelectedStore,
    Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>,
) {
    let config = finite_restart_config(root);
    let mut selected = crate::sharing_fixture_clock::select_applied_singleton(&config)
        .await
        .expect("actual selected local voter");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("production pre-serving schema"));
    let runtime = crate::resource_runtime::open_before_cleanup(&mut selected, &config)
        .await
        .expect("production custody startup")
        .expect("Linux finite custody runtime");
    (config, selected, runtime)
}

async fn finite_restart_attach(
    config: &plurx_core::config::Config,
    selected: &plurx_core::cluster::migration::SelectedStore,
    runtime: &Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>,
    file: &plurx_core::domain::MediaFile,
) -> (Arc<VodServe>, Arc<Rendition>) {
    let dirs = crate::create_dirs_for_config(config).expect("production managed directories");
    let store = Arc::clone(&selected.store);
    let serve = VodServe::new_cluster_with_runtime(
        dirs.renditions.clone(),
        dirs.runtime_cache.clone(),
        Arc::clone(&store),
        selected.identity.node_id.clone(),
        dirs.cache.join("cluster-index"),
        None,
        Some(Arc::clone(runtime)),
    );
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        dirs.transcode.clone(),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    )
    .with_cache(
        dirs.cache.clone(),
        "finite-software-restart".into(),
        selected.identity.node_id.clone(),
    )
    .with_copy_test_vod(Arc::clone(&serve));
    let mut request_selected = request("finite-software-restart", 0.0);
    request_selected.file_id = file.id;
    request_selected.kind = SessionKind::Transcode { height: 360 };
    request_selected.audio_index = None;
    request_selected.audio_claim = None;
    let encoding = manager
        .resolve_encoded_output_test(&request_selected, file)
        .await
        .expect("selected software encoding");
    request_selected.audio_delivery = encoding.options.audio.clone();
    let mut vod_settings = settings();
    vod_settings.materialize_budget = Duration::from_secs(90);
    serve
        .try_create(
            VodRecipeRequest {
                root_admission: None,
                soundtrack: None,
                companion: None,
                request: &request_selected,
                encoding: Some(encoding),
                retained_capture: RetainedOutputCapture::New,
                measured_candidate: None,
            },
            file,
            &vod_settings,
            VodAttribution {
                user_name: "fixture",
                item_title: "finite restart",
                supersession_user: "fixture",
            },
            "finite-software-restart".into(),
        )
        .await
        .expect("ordinary selected VOD creation");
    let rendition = rendition_of(&serve, "finite-software-restart").await;
    (serve, rendition)
}
