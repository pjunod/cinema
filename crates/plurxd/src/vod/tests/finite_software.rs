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
                            .commit_resolved_media(
                                "finite-software-client",
                                &owner,
                                Some(entry as u32)
                            )
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
    let retained = runtime.accounting().expect("known retained S accounting");
    let status = serve
        .status("finite-software-client")
        .await
        .expect("ordinary status");
    let service = status
        .resource_service
        .expect("actual finite output diagnostics");
    assert_eq!(service.generation, rendition.gen_epoch.load(Acquire));
    assert_eq!(
        service.phase,
        plurx_core::resource_observation::WorkPhase::Retiring
    );
    assert_eq!(
        service.unit,
        plurx_core::resource_observation::ServiceUnit::MediaMilliseconds
    );
    let end = rendition
        .plan
        .entries
        .last()
        .expect("nonempty ordinary plan")
        .end_ticks();
    assert_eq!(
        service.completed,
        u64::try_from(ticks_to_ms(end, rendition.timescale))
            .expect("finite plan duration is nonnegative")
    );
    assert!(
        rendition
            .recipe
            .encoding
            .as_ref()
            .expect("selected software encoding")
            .active_production_evidence()
            .is_none(),
        "resource service observations must not grant legacy recipe compatibility proof"
    );
    assert!(
        retained.used.peak_memory_bytes < initial.used.peak_memory_bytes,
        "producer-only memory allowance must return after the known R-to-S commit"
    );
    assert!(
        retained.used.scratch_bytes < initial.used.scratch_bytes,
        "unused producer storage allowance must return after the known R-to-S commit"
    );
    assert!(retained.used.handles > 0);
    assert!(retained.used.peak_memory_bytes > 0);
    assert!(retained.used.scratch_bytes > 0);
    // Exercise finite map replacement/removal/refusal with its actual paid
    // allocation; sorted lookup must retain ordinary map semantics.
    {
        let lease = rendition
            .finite_client_lease
            .as_ref()
            .expect("finite client partition");
        let mut table = client_state::ClientTable::<u32, u32>::finite(3, lease)
            .expect("finite client table allocation");
        for (key, value) in [(9, 90), (1, 10), (5, 50)] {
            assert_eq!(table.try_insert(key, value), Ok(None));
        }
        assert_eq!(table.try_insert(5, 51), Ok(Some(50)));
        assert_eq!(table.try_insert(3, 30), Err((3, 30)));
        assert_eq!(table.get(&5), Some(&51));
        assert_eq!(table.remove(&1), Some(10));
        assert_eq!(table.try_insert(3, 30), Ok(None));
        assert_eq!(table.keys().copied().collect::<Vec<_>>(), vec![3, 5, 9]);
        assert_eq!(table.len(), 3);
    }
    // Removing a completed demand cancels the SAME obsolete deadline task.
    // Its future's slot, rather than an abort request, proves actual release.
    {
        let demand = serve
            .shared
            .arm_materialize_watchdog(&rendition, 0)
            .expect("finite demand task admission");
        let slot = {
            let clocks = rendition.demand_since.lock().expect("demand lock");
            Arc::downgrade(
                &clocks[&0]
                    ._finite_slot
                    .as_ref()
                    .expect("finite deadline task slot")
                    .0,
            )
        };
        assert!(slot.upgrade().is_some());
        rendition.clear_demand(0);
        drop(demand);
        wait_until(
            "obsolete finite deadline future drops its original slot",
            Duration::from_secs(5),
            || async { slot.upgrade().is_none() },
        )
        .await;
    }
    // A finite deadline wakes every waiter with the SAME charged payload.
    // Its fully escaped HTTP body must retain that slot after clock/outcome
    // owners have gone, and cancellation of the final body releases it.
    {
        use crate::waitpool::{
            FiniteFailureKind, FiniteWaitFailure, WaitKey, WaitOutcome, WaitPool,
        };
        let client_lease = rendition
            .finite_client_lease
            .as_ref()
            .expect("finite client partition");
        let slot = client_lease
            .reserve(
                FiniteWaitFailure::required_payload_bytes()
                    .expect("bounded finite failure payload"),
            )
            .expect("finite error payload admission");
        let weak_slot = Arc::downgrade(&slot);
        let failure = FiniteWaitFailure::new(
            FiniteFailureKind::Capacity,
            "\0".repeat(FiniteWaitFailure::MESSAGE_BYTES),
            MATERIALIZE_EXPIRY_RETAINED,
            Arc::clone(&slot),
        )
        .expect("bounded maximally escaped diagnostic");
        let pool = WaitPool::new(2, 1);
        let key = WaitKey {
            rendition: rendition.key.clone(),
            index: 0,
        };
        let mut first = pool
            .register(key.clone(), "finite-deadline-a")
            .expect("first finite waiter");
        let mut second = pool
            .register(key, "finite-deadline-b")
            .expect("second finite waiter");
        pool.expire_entry(
            &rendition.key,
            0,
            WaitOutcome::FiniteMaterialization(Arc::clone(&failure)),
        );
        let first_outcome = first.wait(Duration::from_secs(1)).await;
        let second_outcome = second.wait(Duration::from_secs(1)).await;
        let (
            WaitOutcome::FiniteMaterialization(first_failure),
            WaitOutcome::FiniteMaterialization(second_failure),
        ) = (first_outcome, second_outcome)
        else {
            panic!("finite wake lost its original failure owner")
        };
        assert!(Arc::ptr_eq(&first_failure, &second_failure));
        let body = failure.body();
        let wire: serde_json::Value =
            serde_json::from_slice(&body).expect("actual finite failure body JSON");
        assert_eq!(wire["code"], "segment_pending");
        assert_eq!(wire["cause"], "transcode_capacity_pending");
        assert!(wire["message"]
            .as_str()
            .expect("finite failure message")
            .starts_with(&"\0".repeat(512)));
        drop((slot, failure, first_failure, second_failure, first, second));
        assert!(
            weak_slot.upgrade().is_some(),
            "HTTP body still owns its admitted payload"
        );
        let last_body = body.clone();
        drop(body);
        assert!(weak_slot.upgrade().is_some());
        drop(last_body);
        assert!(
            weak_slot.upgrade().is_none(),
            "last actual body releases the in-role slot"
        );
    }
    let retained_len = output
        .file_len(FiniteFile::Segment(0))
        .await
        .expect("retained segment lookup")
        .expect("retained segment");
    let mut retained_file = ReadyFile::Finite {
        output: output.clone(),
        file: FiniteFile::Segment(0),
        len: retained_len,
    };
    let retained_body = retained_file
        .read_all_bounded(retained_len, deadline)
        .await
        .expect("retained S still owns its full response partition");
    assert!(matches!(&retained_body, ReadyBytes::Finite(_)));
    assert_eq!(retained_body.len() as u64, retained_len);
    drop(retained_body);
    drop(retained_file);
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
    // Closing before the first wake must let the actual registered driver
    // return while this test still holds the rendition's metadata. The driver
    // cannot wait for a lease it owns; the existing cleanup owner settles it
    // only after those real metadata references have dropped.
    second.closed.store(true, Release);
    let completion = second_producer.begin_driver();
    tokio::time::timeout(
        Duration::from_secs(2),
        finite_software::run(
            Arc::clone(&serve.shared),
            Arc::clone(&second),
            Arc::clone(&second_producer),
            completion,
        ),
    )
    .await
    .expect("closed-before-entry driver returns without waiting on its own metadata");
    assert_eq!(
        runtime
            .accounting()
            .expect("actual finite accounting")
            .unknown_peak_producers,
        1
    );
    // The detached closing owner keeps the exact fresh R while the test still
    // holds its original producer selector and rendition metadata.
    let second_output = second.dir.finite_output().expect("second finite output");
    let escaped_selector = second_output.clone();
    serve
        .shared
        .retain_unattached_finite(
            &second_key,
            Some(Arc::clone(&second)),
            second_output,
            Arc::clone(&second_producer),
        )
        .await;
    assert_eq!(
        runtime
            .accounting()
            .expect("actual finite accounting")
            .unknown_peak_producers,
        1
    );
    drop(second);
    drop(second_producer);
    assert_eq!(
        runtime
            .accounting()
            .expect("actual finite accounting")
            .unknown_peak_producers,
        1,
        "an escaped finite selector holds the original application heap fence"
    );
    drop(escaped_selector);
    wait_until(
        "actual detached finite close returns second R only after aliases drain",
        Duration::from_secs(5),
        || async {
            runtime
                .accounting()
                .expect("actual finite accounting")
                .unknown_peak_producers
                == 0
        },
    )
    .await;
    assert_eq!(runtime.accounting().expect("S retained").held, 1);

    serve.end("finite-software-client", Terminal::Deleted).await;
    let final_key = rendition.key.clone();
    drop(rendition);
    expire_finite_session_tombstone(&serve, "finite-software-client").await;
    // Exercise the production closing owner: it drains the actual rendition
    // metadata before native cleanup and the known journal refund.
    serve
        .shared
        .purge_if_dormant(&final_key, Duration::ZERO)
        .await;
    let final_snapshot = runtime.accounting().expect("final accounting");
    assert_eq!(final_snapshot.held, 0);
    assert_eq!(final_snapshot.unknown_peak_producers, 0);
    assert_eq!(final_snapshot.used, Default::default());
}

/// Advance only the existing terminal-response retention policy after actual
/// cleanup completed. Native exit/join/close proofs above use the real kernel;
/// no clock advance supplies resource extinction or settlement authority.
async fn expire_finite_session_tombstone(serve: &VodServe, session_id: &str) {
    let cleanup = {
        let sessions = serve.shared.sessions.lock().await;
        sessions
            .get(session_id)
            .expect("terminal session")
            .terminal_cleanup
            .as_ref()
            .cloned()
            .expect("original cleanup owner")
    };
    cleanup.wait().await;
    drop(cleanup);
    tokio::time::pause();
    tokio::time::advance(TERMINAL_TOMBSTONE_RETENTION + Duration::from_secs(1)).await;
    tokio::time::resume();
    // Ordinary maintenance must also obtain its fresh durable non-live check.
    serve.maintain().await;
    assert!(
        serve.shared.sessions.lock().await.get(session_id).is_none(),
        "expired terminal state must release its actual finite client owners"
    );
}

// Real accepted-R fixture for construction cancellation. No synthetic native
// receipt, path-based refund, or scheduler timeout can close its original role.
struct FiniteCancellationFixture {
    recipe: Recipe,
    plan: SegmentPlan,
    settings: VodSettings,
    serve: Arc<VodServe>,
    runtime: Arc<plurx_core::adaptive_resources::runtime::NodeResourceRuntime>,
    _selected_store: plurx_core::cluster::migration::SelectedStore,
    _base: tempfile::TempDir,
}

async fn finite_cancellation_fixture() -> FiniteCancellationFixture {
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
    let mut vod_settings = settings();
    vod_settings.materialize_budget = Duration::from_secs(90);

    let plan = encoding.media_plan(2_000);
    let recipe = Recipe {
        retained_logical: None,
        measured_candidate: None,
        file,
        audio_index: None,
        aac: false,
        audio_delivery: encoding.options.audio.clone(),
        video: CopyVideoOptions::new(false, false),
        source_object_version: Some(encoding.source_object_version.clone()),
        cluster_cache_key: None,
        encoding: Some(encoding),
    };
    FiniteCancellationFixture {
        recipe,
        plan,
        settings: vod_settings,
        serve,
        runtime,
        _selected_store: selected_store,
        _base: base,
    }
}

struct FiniteMetadataPause {
    key: String,
    pause: Arc<crate::seam_hooks::AsyncPause>,
}

macro_rules! finite_build_ready_hook {
    ($name:ident) => {
        fn $name(&self) -> crate::seam_hooks::HookFuture<'_> {
            Box::pin(crate::seam_hooks::HookReady)
        }
    };
}

impl VodSharedHooks for FiniteMetadataPause {
    fn before_finite_metadata_reservation(&self, key: &str) -> crate::seam_hooks::HookFuture<'_> {
        let selected = key == self.key;
        Box::pin(async move {
            if selected {
                let _released = self.pause.hold().await;
            }
        })
    }
    finite_build_ready_hook!(before_terminal_replay_join);
    finite_build_ready_hook!(after_control_applied);
    finite_build_ready_hook!(after_segment_wait_registered);
    finite_build_ready_hook!(before_segment_ready_open);
    finite_build_ready_hook!(before_terminal_detach);
    finite_build_ready_hook!(after_rendition_installed);
    finite_build_ready_hook!(after_dormant_purge_removed);
    fn terminal_route_outcome(&self, _: &str) -> Option<bool> {
        None
    }
}

#[tokio::test]
async fn cancelled_finite_metadata_wait_keeps_the_installed_original_cleanup_owner() {
    finite_metadata_wait_abandonment(false).await;
}

#[tokio::test]
async fn finite_metadata_deadline_arms_the_same_original_cleanup_owner() {
    finite_metadata_wait_abandonment(true).await;
}

async fn finite_metadata_wait_abandonment(exhaust_deadline: bool) {
    let mut fixture = finite_cancellation_fixture().await;
    if exhaust_deadline {
        fixture.settings.materialize_budget = Duration::from_secs(3);
    }
    let baseline = fixture
        .runtime
        .accounting()
        .expect("initial actual accounting");
    let key = "c".repeat(64);
    let build_gate = fixture
        .serve
        .shared
        .rendition_build_gate(&key)
        .lock_owned()
        .await;
    let pause = crate::seam_hooks::AsyncPause::new("accepted finite metadata reservation");
    let _hooks = fixture.serve.shared.hooks.get_or_install(|| {
        Box::new(FiniteMetadataPause {
            key: key.clone(),
            pause: Arc::clone(&pause),
        })
    });
    let build = {
        let shared = Arc::clone(&fixture.serve.shared);
        let key = key.clone();
        let recipe = fixture.recipe.clone();
        let plan = fixture.plan.clone();
        let settings = fixture.settings.clone();
        tokio::spawn(async move {
            finite_software::build(&shared, &key, None, recipe, plan, &settings).await
        })
    };
    let held = pause.reached().await;
    let original = fixture
        .serve
        .shared
        .finite_closing
        .lock()
        .await
        .get(&key)
        .cloned()
        .expect("original selector is installed before the metadata await");
    let cleanup = shared::PreparedAttemptCleanup::from_closing(Arc::clone(&original));
    let escaped = cleanup.original_output_for_test().await;
    assert_eq!(
        fixture
            .runtime
            .accounting()
            .expect("accepted R")
            .unknown_peak_producers,
        1
    );
    fixture
        .serve
        .shared
        .purge_if_dormant(&key, Duration::ZERO)
        .await;
    assert!(
        !cleanup.known_closed(),
        "active construction is not cleanup authority"
    );
    if exhaust_deadline {
        match build
            .await
            .expect("original builder returns its real deadline result")
        {
            Err(error) => assert!(error.contains("finite metadata reservation deadline elapsed")),
            Ok(_) => panic!("held original metadata reservation must reach its deadline"),
        }
    } else {
        build.abort();
        let cancelled = match build.await {
            Err(error) => error,
            Ok(_) => panic!("original builder must be cancelled before returning a rendition"),
        };
        assert!(cancelled.is_cancelled());
    }
    drop(held);
    drop(build_gate);
    assert!(
        fixture
            .serve
            .shared
            .finite_closing
            .lock()
            .await
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &original)),
        "same original cleanup selector"
    );
    assert!(
        !cleanup.known_closed(),
        "actual escaped output still holds the application fence"
    );
    assert_eq!(
        fixture
            .runtime
            .accounting()
            .expect("alias retained")
            .unknown_peak_producers,
        1
    );
    drop(escaped);
    wait_until(
        "original never-entered R known close",
        Duration::from_secs(30),
        || async { cleanup.known_closed() },
    )
    .await;
    let final_accounting = fixture.runtime.accounting().expect("known original refund");
    assert_eq!(final_accounting.unknown_peak_producers, 0);
    assert_eq!(final_accounting.used, baseline.used);
    assert!(!fixture
        .serve
        .shared
        .finite_closing
        .lock()
        .await
        .contains_key(&key));
}

#[tokio::test]
async fn dropped_finite_build_before_publication_keeps_its_original_cleanup_arm() {
    let fixture = finite_cancellation_fixture().await;
    let baseline = fixture
        .runtime
        .accounting()
        .expect("initial actual accounting");
    let key = "d".repeat(64);
    let build_gate = fixture
        .serve
        .shared
        .rendition_build_gate(&key)
        .lock_owned()
        .await;
    let rendition = finite_software::build(
        &fixture.serve.shared,
        &key,
        None,
        fixture.recipe.clone(),
        fixture.plan.clone(),
        &fixture.settings,
    )
    .await
    .expect("accepted dormant finite build");
    let original = fixture
        .serve
        .shared
        .finite_closing
        .lock()
        .await
        .get(&key)
        .cloned()
        .expect("post-build publication still retains the original selector");
    let cleanup = rendition
        .dir
        .finite_build_cleanup_selector()
        .expect("completed build still owns its dormant cleanup arm");
    let escaped = rendition
        .dir
        .finite_output()
        .expect("original output alias");
    assert!(!fixture
        .serve
        .shared
        .renditions
        .lock()
        .await
        .contains_key(&key));
    assert!(!cleanup.known_closed());
    drop(rendition);
    drop(build_gate);
    assert!(
        fixture
            .serve
            .shared
            .finite_closing
            .lock()
            .await
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &original)),
        "same original cleanup selector"
    );
    assert!(
        !cleanup.known_closed(),
        "escaped actual selector prevents final refund"
    );
    assert_eq!(
        fixture
            .runtime
            .accounting()
            .expect("alias retained")
            .unknown_peak_producers,
        1
    );
    drop(escaped);
    wait_until(
        "post-build original R known close",
        Duration::from_secs(30),
        || async { cleanup.known_closed() },
    )
    .await;
    let final_accounting = fixture.runtime.accounting().expect("known original refund");
    assert_eq!(final_accounting.unknown_peak_producers, 0);
    assert_eq!(final_accounting.used, baseline.used);
    assert!(!fixture
        .serve
        .shared
        .finite_closing
        .lock()
        .await
        .contains_key(&key));
}
