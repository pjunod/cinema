// Qualification fixture: Linux, a real selected local store, and a real
// FFmpeg/FFprobe installation with libx264 and physical headroom for one
// retained S plus one additional unknown-peak producer. Missing tools or
// capacity fail explicitly.
// This exercises the ordinary encoded request and HTTP body reader, rather
// than constructing a test-only native receipt or opening published files.
#[tokio::test]
async fn ordinary_selected_finite_vod_retains_s_reuses_producer_claim_and_refunds_on_evict() {
    use plurx_core::adaptive_resources::runtime::finite_vod::FiniteFile;
    use plurx_core::config::Config;

    testfixtures::require_ffmpeg();
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("repo-local fixture root");
    let base = tempfile::Builder::new()
        .prefix("finite-software-vod-")
        .tempdir_in(&target)
        .expect("repo-local finite fixture");
    let source_path = base.path().join("source.mkv");
    // A short, actual media source keeps the native writer and test bounded.
    testfixtures::run(
        std::process::Command::new(testfixtures::ffmpeg())
            .args(["-v", "error", "-y", "-i"])
            .arg(testfixtures::source_with_eac3_audio())
            .args(["-t", "2", "-an", "-c:v", "copy"])
            .arg(&source_path),
    );
    let preliminary = media_file_at(source_path.clone(), 2_000);
    let source = crate::fragment_index_cluster::open_source_fence(&preliminary, None)
        .await
        .expect("actual held source");
    let probe_json = crate::ffmpeg::held_source_probe_json(
        &source.handle,
        crate::process_control::ChildWork::background("finite software fixture probe"),
    )
    .await
    .expect("actual source probe");
    let probe = plurx_core::scan::probe::parse_probe_json(
        &serde_json::from_str(&probe_json).expect("probe JSON"),
    );
    drop(source);

    let mut config = Config::default();
    config.storage.data_dir = base.path().join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    let mut selected_store = crate::sharing_fixture_clock::select_applied_singleton(&config)
        .await
        .expect("actual selected local voter");
    assert!(
        Box::pin(selected_store.prepare_source_schema_before_serving())
            .await
            .expect("production pre-serving schema")
    );
    let runtime = crate::resource_runtime::open_before_cleanup(&mut selected_store, &config)
        .await
        .expect("production custody startup")
        .expect("Linux finite custody runtime");
    let dirs = crate::create_dirs_for_config(&config).expect("production managed directories");
    let store = Arc::clone(&selected_store.store);
    let library = store
        .create_library(&plurx_core::domain::NewLibrary {
            name: "finite software".into(),
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
            title: "finite software".into(),
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
        .expect("file lookup")
        .expect("file row");
    let serve = VodServe::new_cluster_with_runtime(
        dirs.renditions.clone(),
        dirs.runtime_cache.clone(),
        Arc::clone(&store),
        selected_store.identity.node_id.clone(),
        dirs.cache.join("cluster-index"),
        None,
        Some(Arc::clone(&runtime)),
    );
    let manager = crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        dirs.transcode.clone(),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    )
    .with_cache(
        dirs.cache.clone(),
        "finite-software-test".into(),
        selected_store.identity.node_id.clone(),
    )
    .with_copy_test_vod(Arc::clone(&serve));
    let mut request_selected = request("finite-software", 0.0);
    request_selected.file_id = id;
    request_selected.kind = SessionKind::Transcode { height: 360 };
    request_selected.audio_index = None;
    request_selected.audio_claim = None;
    let encoding = manager
        .resolve_encoded_output_test(&request_selected, &file)
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
            &file,
            &vod_settings,
            VodAttribution {
                user_name: "fixture",
                item_title: "finite software",
                supersession_user: "fixture",
            },
            "finite-software-client".into(),
        )
        .await
        .expect("ordinary selected VOD creation");
    let rendition = rendition_of(&serve, "finite-software-client").await;
    assert!(
        rendition.dir.is_finite(),
        "ordinary route must select finite custody"
    );
    let output = rendition
        .dir
        .finite_output()
        .expect("finite output selector");
    let initial = runtime.accounting().expect("active producer accounting");
    assert_eq!(initial.unknown_peak_producers, 1);

    let deadline = Instant::now() + Duration::from_secs(90);
    for entry in 0..rendition.plan.len() {
        let name = segment_name(entry as u64);
        let ready = loop {
            assert!(
                Instant::now() < deadline,
                "finite segment {entry} timed out"
            );
            match serve.segment("finite-software-client", &name).await {
                Some(VodPublication {
                    result: Ok(Some(ready)),
                    owner,
                }) => {
                    assert!(
                        serve
                            .commit_resolved_media("finite-software-client", &owner, entry as u32)
                            .await
                    );
                    break ready;
                }
                Some(VodPublication {
                    result: Err(VodError::Pending { .. }),
                    ..
                }) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                other => panic!("finite segment {entry}: {}", describe(other)),
            }
        };
        assert!(matches!(&ready.file, ReadyFile::Finite { .. }));
        let mut ready = ready;
        let body = ready
            .file
            .read_all_bounded(ready.len, deadline)
            .await
            .expect("counted HTTP body reader");
        assert!(matches!(&body, ReadyBytes::Finite(_)));
        assert_eq!(body.len() as u64, ready.len);
        assert!(body.windows(4).any(|box_type| box_type == b"moof"));
        let response = body.into_bytes();
        assert_eq!(response.len() as u64, ready.len);
        drop(response);
    }
    let init_len = output
        .file_len(FiniteFile::Init)
        .await
        .expect("durable init lookup")
        .expect("durable init file");
    assert!(init_len > 0);
    let mut init = ReadyFile::Finite {
        output: output.clone(),
        file: FiniteFile::Init,
        len: init_len,
    };
    let init_body = init
        .read_all_bounded(init_len, deadline)
        .await
        .expect("counted init body");
    assert!(matches!(&init_body, ReadyBytes::Finite(_)));
    assert_eq!(init_body.len() as u64, init_len);
    assert!(init_body.windows(4).any(|box_type| box_type == b"moov"));
    drop(init_body);

    wait_until(
        "normal R-to-S retirement",
        Duration::from_secs(90),
        || async {
            let snapshot = runtime.accounting().expect("retirement accounting");
            snapshot.unknown_peak_producers == 0 && snapshot.held == 1
        },
    )
    .await;
    // A second ordinary selected finite builder really admits another R
    // while the first S still owns every retained file and reader budget.
    let second_key = "b".repeat(64);
    assert_ne!(second_key, rendition.key);
    let second = finite_software::build(
        &serve.shared,
        &second_key,
        None,
        rendition.recipe.clone(),
        rendition.plan.clone(),
        &vod_settings,
    )
    .await
    .expect("new finite R admission while S is retained");
    let second_producer = second
        .dir
        .finite_producer()
        .expect("second original producer");
    assert_eq!(
        runtime
            .accounting()
            .expect("second claim")
            .unknown_peak_producers,
        1
    );
    second_producer
        .perform(Step::Terminate {
            why: Termination::Idle,
        })
        .await
        .expect("cancel unstarted second R through original owner");
    assert_eq!(
        runtime
            .accounting()
            .expect("second refund")
            .unknown_peak_producers,
        0
    );
    assert_eq!(runtime.accounting().expect("S retained").held, 1);

    serve.end("finite-software-client", Terminal::Deleted).await;
    output
        .evict(Instant::now() + Duration::from_secs(90))
        .await
        .expect("native S cleanup and exact refund");
    let final_snapshot = runtime.accounting().expect("final accounting");
    assert_eq!(final_snapshot.held, 0);
    assert_eq!(final_snapshot.unknown_peak_producers, 0);
    assert_eq!(final_snapshot.used, Default::default());
}
