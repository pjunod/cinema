// Qualification fixture: Linux, real FFmpeg/FFprobe, a selected local store,
// and enough physical capacity for the fixed software profile. Missing tools,
// custody import, or a live prior owner fail explicitly. The child process
// exits before the parent opens the SAME persisted installation, so startup's
// previous-owner-exit proof is real rather than a simulated runtime drop.
const FINITE_RESTART_PHASE: &str = "PLURX_FINITE_SOFTWARE_RESTART_CHILD_ROOT";
const FINITE_RESTART_UNSERVED: &str = "PLURX_FINITE_SOFTWARE_RESTART_UNSERVED";
const FINITE_RESTART_TEST: &str =
    "vodserve::tests::ordinary_selected_finite_vod_reuses_dormant_s_after_process_restart";

#[tokio::test]
async fn ordinary_selected_finite_vod_reuses_dormant_s_after_process_restart() {
    testfixtures::require_ffmpeg();
    if let Some(root) = std::env::var_os(FINITE_RESTART_PHASE) {
        if std::env::var_os(FINITE_RESTART_UNSERVED).is_some() {
            finite_restart_unserved_child(std::path::Path::new(&root)).await;
        } else {
            finite_restart_child(std::path::Path::new(&root), false).await;
        }
        return;
    }

    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("repo-local fixture root");
    let target = std::fs::canonicalize(target).expect("canonical native fixture ancestry");
    let fixture = FiniteLifecycleFixtureRoot(
        tempfile::Builder::new()
            .prefix("finite-software-restart-")
            .tempdir_in(&target)
            .expect("repo-local restart fixture"),
    );
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

    // A second real daemon owns imported Files before any serving-map owner.
    let status = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", FINITE_RESTART_TEST, "--nocapture"])
        .env(FINITE_RESTART_PHASE, fixture.path())
        .env(FINITE_RESTART_UNSERVED, "1")
        .kill_on_drop(true)
        .status()
        .await
        .expect("unserved import daemon");
    assert!(
        status.success(),
        "unserved imported S exits through the real collector"
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
    let (serve, rendition, playback_cache) =
        finite_restart_attach(&config, &selected, &runtime, &file).await;
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
    let final_key = rendition.key.clone();
    drop(output); // Eviction must wait for actual selector aliases to leave.
    drop(rendition);
    expire_finite_session_tombstone(&serve, "finite-software-restart").await;
    serve
        .shared
        .purge_if_dormant(&final_key, Duration::ZERO)
        .await;
    serve
        .shutdown_ordinary_finalizer(deadline)
        .await
        .expect("actual recovered collector shutdown");
    let final_snapshot = runtime.accounting().expect("final accounting");
    assert!(
        final_snapshot.finalization_support.is_none(),
        "actual supportReleased"
    );
    assert_eq!(final_snapshot.held, 0);
    assert_eq!(final_snapshot.used, Default::default());
    drop(serve);
    drop(playback_cache);
    let runtime =
        Arc::try_unwrap(runtime).unwrap_or_else(|_| panic!("recovered runtime still owned"));
    runtime.drain().expect("drain recovered custody worker");
    selected.shutdown().await.expect("shutdown recovered voter");
    drop(selected);
}

#[tokio::test]
async fn ordinary_collector_retained_shutdown_drains_dormant_closing_envelope() {
    testfixtures::require_ffmpeg();
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("repo-local fixture root");
    let target = std::fs::canonicalize(target).expect("canonical native fixture ancestry");
    let fixture = FiniteLifecycleFixtureRoot(
        tempfile::Builder::new()
            .prefix("finite-software-retained-closing-")
            .tempdir_in(&target)
            .expect("repo-local retained closing fixture"),
    );
    testfixtures::run(
        std::process::Command::new(testfixtures::ffmpeg())
            .args(["-v", "error", "-y", "-i"])
            .arg(testfixtures::source_with_eac3_audio())
            .args(["-t", "2", "-an", "-c:v", "copy"])
            .arg(fixture.path().join("source.mkv")),
    );
    finite_restart_child(fixture.path(), true).await;
}

async fn finite_restart_child(root: &std::path::Path, dormant_closing: bool) {
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
    let (serve, rendition, playback_cache) =
        finite_restart_attach(&config, &selected, &runtime, &file).await;
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
    loop {
        assert!(Instant::now() < deadline, "first finite segment timed out");
        assert!(
            rendition.failure().is_none(),
            "first finite producer failed"
        );
        if rendition
            .manifest
            .lock()
            .await
            .state(0)
            .is_some_and(|state| state.is_materialized() && state.bytes() > 0)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The real Materialized event follows durable publication. An output
    // selector created before bootstrap has no file-lookup authority yet.
    let len = output
        .file_len(plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Segment(0))
        .await
        .expect("published native inventory")
        .expect("durably published first segment");
    assert!(len > 0);
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
    drop(ready); // Release the response's cloned FiniteOutput before runtime drain.
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
    let init_len = output
        .file_len(plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Init)
        .await
        .expect("retained init inventory")
        .expect("retained init");
    let closed_reader = output
        .prepare_reader(
            plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile::Init,
            0,
            init_len,
            4096,
            deadline,
        )
        .await
        .expect("real retained reader");
    closed_reader
        .close(deadline)
        .await
        .expect("actual reader native close");
    let output_path = output.path().to_owned();
    let inventory = finite_restart_inventory(&output_path);
    std::fs::write(root.join("retained-inventory"), &inventory)
        .expect("persist original inode evidence");
    let before = runtime.accounting().expect("S plus actual support");
    let support_bytes = before
        .finalization_support
        .expect("real initialized collector")
        .requested_bytes;
    let gate = serve
        .shared
        .rendition_build_gate(&rendition.key)
        .expect("original S gate");
    let held_gate = gate.clone().lock_owned().await;
    let waiting = Arc::clone(&serve);
    let observer = tokio::spawn(async move {
        waiting
            .shutdown_ordinary_finalizer(Instant::now() + Duration::from_secs(90))
            .await
    });
    wait_until(
        "paid exit selector before serving removal",
        Duration::from_secs(10),
        || async {
            serve
                .shared
                .ordinary_collector
                .lock()
                .expect("collector publication")
                .as_ref()
                .is_some_and(|client| client.retained_shutdown_key(&rendition.key))
        },
    )
    .await;
    observer.abort();
    assert!(observer
        .await
        .expect_err("cancelled shutdown observer")
        .is_cancelled());
    assert_eq!(finite_restart_inventory(&output_path), inventory);
    assert_eq!(
        runtime
            .accounting()
            .expect("actual retained shutdown accounting")
            .used,
        before.used
    );
    assert!(runtime
        .accounting()
        .expect("actual retained shutdown accounting")
        .finalization_support
        .is_some());
    drop(held_gate);
    let retained_closing = if dormant_closing {
        let producer = rendition
            .dir
            .finite_producer()
            .expect("same retained producer");
        let original_driver = producer
            .original_driver_id_now_for_test()
            .expect("original installed driver identity");
        // The paid S selector is already captured. The actual pressure path
        // transfers this dormant owner under its original, now-unlocked gate.
        serve
            .shared
            .pressure_evict_finite_rendition(&rendition.key)
            .await;
        let closing = serve
            .shared
            .finite_closing
            .lock()
            .await
            .get(&rendition.key)
            .cloned()
            .expect("actual dormant purge registered its original closing envelope");
        assert!(!serve
            .shared
            .renditions
            .lock()
            .await
            .contains_key(&rendition.key));
        wait_until(
            "same retained service joins driver and drains actual closing payload",
            Duration::from_secs(10),
            || async {
                producer
                    .original_driver_joined_for_test(original_driver)
                    .await
                    && closing.retained_payload_dropped_for_test().await
                    && !serve
                        .shared
                        .finite_closing
                        .lock()
                        .await
                        .contains_key(&rendition.key)
            },
        )
        .await;
        assert_eq!(
            runtime
                .accounting()
                .expect("actual retained shutdown accounting")
                .used,
            before.used
        );
        assert_eq!(finite_restart_inventory(&output_path), inventory);
        drop(producer);
        Some(closing)
    } else {
        None
    };
    drop(output);
    drop(rendition);
    // Even a positively closed reader remains an application selector.
    assert!(serve
        .shutdown_ordinary_finalizer(Instant::now() + Duration::from_millis(100))
        .await
        .is_err());
    assert!(runtime
        .accounting()
        .expect("actual retained shutdown accounting")
        .finalization_support
        .is_some());
    assert!(
        serve.shared.sessions.lock().await.is_empty(),
        "actual terminal client payloads and slots dropped before S allocation wait"
    );
    assert_eq!(finite_restart_inventory(&output_path), inventory);
    drop(closed_reader);
    // Drop the actual empty envelope/header alias before isolating the final
    // storage barrier to the one original unlocked gate Arc below.
    drop(retained_closing);
    // This actual gate Arc survives actor joins and blocks storage destruction.
    let storage_alias_error = serve
        .shutdown_ordinary_finalizer(Instant::now() + Duration::from_secs(90))
        .await
        .expect_err("original unlocked gate alias must block final storage destruction");
    assert!(
        serve.shared.ordinary_collector.lock().expect("original collector publication lock").is_none(),
        "final publication removed; shutdown error: {storage_alias_error}; closing entries: {}; sessions: {}",
        serve.shared.finite_closing.lock().await.len(),
        serve.shared.sessions.lock().await.len()
    );
    assert!(runtime
        .accounting()
        .expect("actual retained shutdown accounting")
        .finalization_support
        .is_some());
    assert_eq!(finite_restart_inventory(&output_path), inventory);
    drop(gate);
    serve
        .shutdown_ordinary_finalizer(Instant::now() + Duration::from_secs(90))
        .await
        .expect("retry same final lifecycle root after original gate alias drops");
    let parked = runtime.accounting().expect("parked retained S");
    assert!(
        parked.finalization_support.is_none(),
        "actual supportReleased"
    );
    assert_eq!(parked.held, 1, "same durable S remains charged");
    let mut expected = before.used;
    expected.peak_memory_bytes -= support_bytes;
    assert_eq!(
        parked.used, expected,
        "only independently paid support was released"
    );
    assert_eq!(
        finite_restart_inventory(&output_path),
        inventory,
        "same files and inodes"
    );
    drop(serve);
    drop(playback_cache);
    drop(store);
    let runtime = Arc::try_unwrap(runtime).unwrap_or_else(|_| panic!("first runtime still owned"));
    runtime.drain().expect("drain first custody worker");
    selected.shutdown().await.expect("shutdown first voter");
    drop(selected);
}

fn finite_restart_inventory(path: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let mut entries = std::fs::read_dir(path)
        .expect("same retained directory")
        .map(|entry| {
            let entry = entry.expect("original retained directory entry");
            let m = entry.metadata().expect("original retained entry metadata");
            format!(
                "{}:{}:{}:{}",
                entry.file_name().to_string_lossy(),
                m.dev(),
                m.ino(),
                m.len()
            )
        })
        .collect::<Vec<_>>();
    let m = std::fs::metadata(path).expect("original retained directory metadata");
    entries.push(format!(".:{}:{}:{}", m.dev(), m.ino(), m.len()));
    entries.sort();
    entries.join("\n")
}

async fn finite_restart_unserved_child(root: &std::path::Path) {
    let (config, selected, runtime) = finite_restart_open(root).await;
    let key = std::fs::read_to_string(root.join("rendition-key"))
        .expect("persisted original rendition key");
    let dirs = crate::create_dirs_for_config_with_runtime(&config, Some(runtime.as_ref()))
        .expect("recovery-aware managed directories for unserved import");
    let serve = VodServe::new_cluster_with_runtime(
        dirs.renditions.clone(),
        dirs.runtime_cache,
        Arc::clone(&selected.store),
        selected.identity.node_id.clone(),
        dirs.cache.join("cluster-index"),
        None,
        Some(Arc::clone(&runtime)),
    );
    serve
        .initialize_ordinary_finalizer(Instant::now() + Duration::from_secs(30))
        .await
        .expect("actual unserved import collector startup");
    assert!(
        serve.shared.renditions.lock().await.is_empty(),
        "imported S has no serving owner"
    );
    let before = runtime
        .accounting()
        .expect("actual retained shutdown accounting");
    assert_eq!(before.held, 1);
    let gate = serve
        .shared
        .rendition_build_gate(&key)
        .expect("original unserved import gate");
    let held = gate.lock_owned().await;
    let waiting = Arc::clone(&serve);
    let observer = tokio::spawn(async move {
        waiting
            .shutdown_ordinary_finalizer(Instant::now() + Duration::from_secs(90))
            .await
    });
    wait_until(
        "unadopted import registered before map adoption",
        Duration::from_secs(10),
        || async {
            serve
                .shared
                .ordinary_collector
                .lock()
                .expect("original unserved collector publication lock")
                .as_ref()
                .is_some_and(|client| client.retained_shutdown_key(&key))
        },
    )
    .await;
    observer.abort();
    assert!(observer
        .await
        .expect_err("cancelled original unserved shutdown observer")
        .is_cancelled());
    assert_eq!(
        runtime
            .accounting()
            .expect("actual retained shutdown accounting")
            .held,
        1
    );
    assert!(runtime
        .accounting()
        .expect("actual retained shutdown accounting")
        .finalization_support
        .is_some());
    drop(held);
    serve
        .shutdown_ordinary_finalizer(Instant::now() + Duration::from_secs(90))
        .await
        .expect("same unadopted File pair and original pending adoption close in permanent SF");
    let after = runtime
        .accounting()
        .expect("actual retained shutdown accounting");
    assert!(after.finalization_support.is_none());
    assert_eq!(after.held, 1);
    assert_eq!(
        finite_restart_inventory(&dirs.renditions.join(key)),
        std::fs::read_to_string(root.join("retained-inventory"))
            .expect("persisted original retained inode inventory")
    );
    drop(serve);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("unserved aliases dropped")
        .drain()
        .expect("drain original unserved custody worker");
    selected
        .shutdown()
        .await
        .expect("shutdown unserved import voter");
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
    let mut config = finite_restart_config(root);
    crate::canonicalize_storage_roots(&mut config)
        .expect("production non-destructive storage preflight before Store ownership");
    let mut selected = crate::sharing_fixture_clock::select_applied_singleton(&config)
        .await
        .expect("actual selected local voter");
    selected
        .store
        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
        .await
        .expect("save Source enablement before this fixture's real schema startup");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("production pre-serving schema"));
    let runtime = if std::env::var_os(FINITE_RESTART_UNSERVED).is_some() {
        // Enter the actual startup boundary before reconciliation has installed
        // a book/serving owner. The original queued Adopt is already retained.
        let dirs = crate::configured_dirs(&config.storage);
        let options = selected
            .resource_custody_options(
                config.storage.data_dir.clone(),
                vec![
                    dirs.transcode,
                    dirs.artwork,
                    dirs.cache,
                    dirs.subs,
                    dirs.runtime_cache,
                    dirs.renditions.clone(),
                ],
                plurx_core::resource_custody::InstallationContext::Existing,
            )
            .expect("actual unserved custody startup options");
        Arc::new(tokio::task::spawn_blocking(move ||
            plurx_core::adaptive_resources::runtime::NodeResourceRuntime::open_with_rendition_root(options, dirs.renditions))
            .await.expect("original unserved runtime startup task joined").expect("actual imported Files and original queued adoption"))
    } else {
        crate::resource_runtime::open_before_cleanup(&mut selected, &config)
            .await
            .expect("production custody startup")
            .expect("Linux finite custody runtime")
    };
    (config, selected, runtime)
}

async fn finite_restart_attach(
    config: &plurx_core::config::Config,
    selected: &plurx_core::cluster::migration::SelectedStore,
    runtime: &Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>,
    file: &plurx_core::domain::MediaFile,
) -> (
    Arc<VodServe>,
    Arc<Rendition>,
    Arc<crate::playback_cache::PlaybackCacheBudget>,
) {
    let dirs = crate::create_dirs_for_config_with_runtime(config, Some(runtime.as_ref()))
        .expect("production recovery-aware managed directories");
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
    let playback_cache = crate::playback_cache::PlaybackCacheBudget::new(
        Arc::clone(&store),
        vec![dirs.renditions.clone()],
    );
    serve.register_playback_cache(&playback_cache);
    serve
        .initialize_ordinary_finalizer(Instant::now() + Duration::from_secs(30))
        .await
        .expect("real ordinary collector before finite admission");
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
    (serve, rendition, playback_cache)
}
