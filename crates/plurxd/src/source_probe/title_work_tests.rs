use super::*;
use plurx_core::{
    domain::{DolbyVisionFacts, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult},
    store::{
        background_jobs::ClaimOutcome, background_jobs_probe::ProbeCoordinator, SqliteStore, Store,
    },
};

#[tokio::test]
async fn title_priority_walk_reaches_runnable_probe_beyond_unreadable_first_page() {
    let dir = crate::test_tempdir().expect("scratch");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let state = AppState::new(
        "test".into(),
        Arc::clone(&store),
        crate::state::Dirs {
            artwork: dir.path().join("artwork"),
            transcode: dir.path().join("transcode"),
            cache: dir.path().join("cache"),
            subs: dir.path().join("subs"),
            runtime_cache: dir.path().join("runtime"),
            renditions: dir.path().join("renditions"),
        },
        "title-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(16)),
    );
    let library = store
        .create_library(&NewLibrary {
            name: "title-priority".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().into()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "title".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = dir.path().join("readable.mkv");
    tokio::fs::write(&path, b"fixture")
        .await
        .expect("readable source");
    let (size, mtime) = snapshot(&path).await.expect("snapshot");
    let file = store
        .upsert_file(
            item,
            path.to_str().expect("path"),
            size,
            mtime,
            &ProbeResult {
                video_codec: Some("hevc".into()),
                dolby_vision: DolbyVisionFacts {
                    profile: Some(7),
                    rpu_present: Some(true),
                    ..Default::default()
                },
                raw_json: Some("{}".into()),
                ..Default::default()
            },
        )
        .await
        .expect("DV file");
    // One demand per physical source: repeated requests for one offline file
    // correctly join its existing job instead of filling a candidate page.
    let mut unreadable = Vec::new();
    for index in 0..128 {
        unreadable.push(
            store
                .upsert_file(
                    item,
                    dir.path()
                        .join(format!("offline-{index}.mkv"))
                        .to_str()
                        .expect("path"),
                    size,
                    mtime,
                    &Default::default(),
                )
                .await
                .expect("distinct offline file"),
        );
    }
    let scanner = "a".repeat(64);
    let revision = store.dv_title_demands(&scanner, 0).await.expect("demand")[0].probe_revision;
    let revision = store
        .bind_dv_title_demand(revision, &scanner)
        .await
        .expect("bind")
        .expect("current");
    let now = now_ms();
    let title_id = uuid::Uuid::new_v4().to_string();
    let request = |id: &str| JobRequest {
        scope: "title-priority".into(),
        request_id: id.into(),
        request_digest: "b".repeat(64),
        consumer_kind: "probe".into(),
        consumer_ref: id.into(),
        target_node_id: None,
        deadline_ms: None,
        retain_identity: false,
    };
    assert!(matches!(
        store
            .enqueue_job(EnqueueJob {
                id: title_id.clone(),
                payload: JobPayload::DvTitleScan {
                    file_id: file,
                    source_size: size,
                    source_mtime: mtime,
                    probe_revision: revision,
                    scanner_digest: scanner
                },
                dedupe_key: "title-priority".into(),
                priority: 0,
                not_before_ms: now,
                now_ms: now,
                request: request(&title_id),
            })
            .await
            .expect("enqueue title"),
        EnqueueOutcome::Accepted { .. }
    ));
    assert!(matches!(
        store
            .claim_job(ClaimJob {
                job_id: title_id.clone(),
                expected_revision: 0,
                node_id: state.node_id.clone(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::DvTitleScan,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await
            .expect("claim title"),
        ClaimOutcome::Claimed { .. }
    ));
    let plurx_core::cluster::coordination::LeaseClaim::Acquired(lease) = store
        .acquire_lease("repair:probe", "coordinator", now, now + 60_000)
        .await
        .expect("coordinator")
    else {
        panic!("lease");
    };
    let coordinator = ProbeCoordinator::from_lease(&lease);
    // A coordinator may own at most 128 active probes. A separate, valid
    // library scan can supply the next page while the repair page is full.
    let plurx_core::cluster::coordination::LeaseClaim::Acquired(scan_lease) = store
        .acquire_lease(
            &format!("scan:library:{}", library.id),
            "scan-coordinator",
            now,
            now + 60_000,
        )
        .await
        .expect("library scan coordinator")
    else {
        panic!("scan lease");
    };
    let scan_coordinator = ProbeCoordinator::from_lease(&scan_lease);
    for (index, file_id) in unreadable
        .iter()
        .copied()
        .chain(std::iter::once(file))
        .enumerate()
    {
        let id = uuid::Uuid::new_v4().to_string();
        let pipeline = "a".repeat(64);
        let coordinator = if index < 128 {
            &coordinator
        } else {
            &scan_coordinator
        };
        assert!(matches!(
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::MediaProbe {
                        file_id,
                        source_generation: plurx_core::store::background_jobs_probe::generation(
                            file_id,
                            size,
                            mtime,
                            coordinator,
                            &pipeline
                        ),
                        source_size: size,
                        source_mtime: mtime,
                        probe_digest: pipeline,
                        coordinator: coordinator.clone(),
                    },
                    dedupe_key: format!("priority-probe:{index}"),
                    priority: if index < 128 { 3 } else { 1 },
                    not_before_ms: now,
                    now_ms: now,
                    request: request(&id),
                })
                .await
                .expect("enqueue probe"),
            EnqueueOutcome::Accepted { .. }
        ));
    }
    let mut task = TitleTask {
        work: Box::pin(std::future::pending()),
        cancel: CancellationToken::new(),
        job_id: title_id.clone(),
        probe_pipeline: "a".repeat(64),
        probe_after: None,
    };
    assert!(!task
        .should_yield(&state)
        .await
        .expect("unreadable first page cannot preempt"));
    assert!(
        task.probe_after.is_some(),
        "nonfinal priority page must retain cursor"
    );
    assert!(task
        .should_yield(&state)
        .await
        .expect("runnable next page preempts"));
    // Offline/expired coordinators are not runnable, even with readable media.
    store
        .release_lease(&lease, now_ms())
        .await
        .expect("coordinator offline");
    store
        .release_lease(&scan_lease, now_ms())
        .await
        .expect("scan coordinator offline");
    task.probe_after = None;
    assert!(!task.should_yield(&state).await.expect("offline first page"));
    assert!(!task.should_yield(&state).await.expect("offline next page"));
}

#[tokio::test]
async fn title_stop_awaits_owned_work_instead_of_dropping_it() {
    let cancel = CancellationToken::new();
    let child_cancel = cancel.clone();
    let reaped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = Arc::clone(&reaped);
    let task = TitleTask {
        work: Box::pin(async move {
            child_cancel.cancelled().await;
            tokio::task::yield_now().await;
            done.store(true, std::sync::atomic::Ordering::Release);
            false
        }),
        cancel,
        job_id: uuid::Uuid::new_v4().to_string(),
        probe_pipeline: "a".repeat(64),
        probe_after: None,
    };
    assert!(!task.stop().await);
    assert!(reaped.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn title_admission_accounts_one_thread_and_reaps_before_ordinary_admission() {
    use plurx_core::transcode::{EncoderCaps, Pipeline};
    use std::time::Duration;
    let dir = crate::test_tempdir().expect("scratch");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "4")
        .await
        .expect("budget");
    let manager = Arc::new(crate::transcode::TranscodeManager::new(
        store,
        dir.path().into(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let admission = manager.admit_dv_title().await.expect("title admission");
    assert_eq!(manager.test_software_threads_in_use(), 1);
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let held = Arc::clone(&manager);
    let marker = dir.path().join("title.pid");
    let child_marker = marker.clone();
    let work = tokio::spawn(async move {
        let result = super::super::run_admitted_probe(
            &held,
            &admission,
            &stop,
            &CancellationToken::new(),
            None,
            async {
                plurx_core::process::bounded::output(
                    "/bin/sh",
                    &[
                        "-c".into(),
                        "echo $$ > \"$1\"; exec sleep 60".into(),
                        "title".into(),
                        child_marker.into_os_string(),
                    ],
                    Duration::from_secs(65),
                    1024,
                    plurx_core::process::ChildWork::background("title admission regression"),
                )
                .await
            },
        )
        .await;
        drop(admission);
        result
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child started");
    assert!(
        manager.admit_fragment().await.is_none(),
        "title still owns physical lane"
    );
    cancel.cancel();
    assert!(tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .expect("child reaped")
        .expect("task")
        .is_err());
    assert_eq!(manager.test_software_threads_in_use(), 0);
    let pid = tokio::fs::read_to_string(marker).await.expect("pid");
    #[allow(clippy::disallowed_methods)]
    let status = std::process::Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("check reaped child");
    assert!(!status.success());
    assert!(
        manager.admit_fragment().await.is_some(),
        "ordinary admission after reap and release"
    );
}

/// The FEL qualification through the real title job: the durable claim, the
/// complete-title scan, the qualification under the job's own admission,
/// publication. `PLURX_FEL_QUALIFY_SOURCE=<P7 FEL title>` (optionally
/// `PLURX_FEL_QUALIFY_SCRATCH`); prints the stored verdict. The scan decodes
/// every picture (hours for a feature on a small worker), so
/// `PLURX_FEL_QUALIFY_WITHOUT_SCAN=1` runs only the job's qualification step
/// (`qualify_if_due` under `run_admitted_probe` with the job's admission)
/// for a whole title.
#[tokio::test]
#[ignore = "physical: the deployed processing helper, a radeonsi VA-API device and a P7 FEL title"]
async fn physical_title_job_qualifies_a_p7_fel_title_and_stores_its_verdict() {
    use plurx_core::transcode::dv_fel_qualification as q;
    let source = std::path::PathBuf::from(
        std::env::var("PLURX_FEL_QUALIFY_SOURCE").expect("PLURX_FEL_QUALIFY_SOURCE"),
    );
    let scratch = crate::test_tempdir().expect("scratch");
    let dir = std::env::var("PLURX_FEL_QUALIFY_SCRATCH")
        .map_or_else(|_| scratch.path().to_path_buf(), std::path::PathBuf::from);
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let caps = plurx_core::transcode::detect_encoders(&crate::ffmpeg::ffmpeg_bin()).await;
    assert!(
        caps.vaapi_driver.is_some(),
        "the VA driver is identified: {caps:?}"
    );
    let mut state = AppState::new(
        "test".into(),
        Arc::clone(&store),
        crate::state::Dirs {
            artwork: dir.join("artwork"),
            transcode: dir.join("transcode"),
            cache: dir.join("cache"),
            subs: dir.join("subs"),
            runtime_cache: dir.join("runtime"),
            renditions: dir.join("renditions"),
        },
        "title-node".into(),
        caps.clone(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(16)),
    );
    let passthrough = crate::ffmpeg::has_hdr10_passthrough_vaapi(caps.vaapi_hdr_policy).await;
    state.transcode = Arc::new(
        crate::transcode::TranscodeManager::new(
            Arc::clone(&store),
            dir.join("runtime"),
            caps,
            plurx_core::transcode::Pipeline::Cpu,
        )
        .with_hdr10_passthrough_vaapi(passthrough),
    );
    std::fs::create_dir_all(dir.join("runtime")).expect("runtime");
    assert!(state.transcode.fel_qualification_capable());
    for key in [
        plurx_core::store::keys::DV_HDR_PROCESSING,
        plurx_core::store::keys::DV_FEL_REENCODE,
    ] {
        store.put_setting(key, "1").await.expect("setting");
    }
    let library = store
        .create_library(&NewLibrary {
            name: "fel".into(),
            kind: LibraryKind::Movies,
            paths: vec![source.parent().expect("parent").into()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "fel".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("probe");
    let (size, mtime) = snapshot(&source).await.expect("snapshot");
    let id = store
        .upsert_file(item, source.to_str().expect("path"), size, mtime, &probe)
        .await
        .expect("file");
    // Each precondition of discovery, named, so a refusal says which.
    assert!(
        state
            .jobs
            .execution_authority()
            .may_execute_job(JobKind::DvTitleScan)
            .await,
        "title jobs may run here"
    );
    let tools = ToolSet::capture().await.expect("helper").expect("helper");
    assert!(
        plurx_core::scan::probe::pipeline_digest().await.is_some(),
        "probe pipeline"
    );
    assert!(
        store
            .dv_title_demands(&tools.title_scanner_digest(), 0)
            .await
            .expect("demands")
            .iter()
            .any(|demand| demand.file_id == id),
        "the probed title has a title demand: {:?}",
        store
            .get_file(id)
            .await
            .expect("file")
            .map(|file| file.dolby_vision)
    );
    if std::env::var_os("PLURX_FEL_QUALIFY_WITHOUT_SCAN").is_some() {
        let started = std::time::Instant::now();
        let file = store.get_file(id).await.expect("file").expect("file");
        let document = store
            .get_file_probe_json(id)
            .await
            .expect("probe")
            .expect("probe");
        let admission = state
            .transcode
            .admit_dv_title()
            .await
            .expect("title admission");
        let token = CancellationToken::new();
        let outcome = super::super::run_admitted_probe(
            &state.transcode,
            &admission,
            &token,
            &CancellationToken::new(),
            None,
            super::super::fel_qualification::qualify_if_due(
                &state, &file, &document, &tools, &admission, &token,
            ),
        )
        .await
        .expect("qualification");
        let stored = store
            .get_file_probe_json(id)
            .await
            .expect("probe")
            .expect("probe");
        let identity = state
            .transcode
            .fel_identity(&tools.backend_digest())
            .await
            .expect("identity");
        let q::VerdictState::Current(verdict) =
            q::verdict_state(Some(&stored), file.size, file.mtime, &identity)
        else {
            panic!("no FEL verdict stored: {outcome:?}");
        };
        println!(
            "FEL qualification under its job admission: {} seconds={} verdict={}",
            source.display(),
            started.elapsed().as_secs(),
            serde_json::to_string(&verdict).expect("JSON")
        );
        return;
    }
    // The durable job exactly as discovery enqueues and claims it, then the
    // job's own body (`execute`): complete-title scan, the qualification under
    // the job's admission, publication.
    let started = std::time::Instant::now();
    let scanner = tools.title_scanner_digest();
    let mut demand = store
        .dv_title_demands(&scanner, 0)
        .await
        .expect("demands")
        .into_iter()
        .find(|demand| demand.file_id == id)
        .expect("demand");
    demand.probe_revision = store
        .bind_dv_title_demand(demand.probe_revision, &scanner)
        .await
        .expect("bind")
        .expect("bound");
    use sha2::{Digest, Sha256};
    let request = hex::encode(Sha256::digest(
        serde_json::to_vec(&("dv-title-v1", id, demand.probe_revision, &scanner))
            .expect("identity"),
    ));
    let file = store.get_file(id).await.expect("file").expect("file");
    let now = now_ms();
    let outcome = store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload: JobPayload::DvTitleScan {
                file_id: id,
                source_size: file.size,
                source_mtime: file.mtime,
                probe_revision: demand.probe_revision,
                scanner_digest: scanner.clone(),
            },
            dedupe_key: format!("dv-title:{request}"),
            priority: 0,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "dv-title".into(),
                request_id: request.clone(),
                request_digest: request,
                consumer_kind: "dv_title_scan".into(),
                consumer_ref: id.to_string(),
                target_node_id: None,
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("enqueue");
    let EnqueueOutcome::Accepted { job_id, .. } = outcome else {
        panic!("title job not accepted: {outcome:?}");
    };
    let candidate = store
        .background_job(&job_id)
        .await
        .expect("job")
        .expect("job");
    let (job, deadline) = crate::background_jobs::claim_with_resolution(
        store.as_ref(),
        &candidate,
        ClaimJob {
            job_id: job_id.clone(),
            expected_revision: candidate.revision,
            node_id: state.node_id.clone(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::DvTitleScan,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        },
    )
    .await
    .expect("claim")
    .expect("claimed");
    let active = ActiveBackgroundJob::start(
        Arc::clone(&store),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        job.token.clone().expect("token"),
        deadline,
        JobKind::DvTitleScan,
    )
    .expect("active job");
    let admission = state
        .transcode
        .admit_dv_title()
        .await
        .expect("title admission");
    let published = execute(
        state.clone(),
        file,
        demand,
        None,
        tools,
        admission,
        job,
        active,
        CancellationToken::new(),
    )
    .await;
    assert!(published, "the title job published");
    let file = store.get_file(id).await.expect("file").expect("file");
    let stored = store
        .get_file_probe_json(id)
        .await
        .expect("probe")
        .expect("probe");
    let identity = state
        .transcode
        .fel_identity(&scanner)
        .await
        .expect("identity");
    let q::VerdictState::Current(verdict) =
        q::verdict_state(Some(&stored), file.size, file.mtime, &identity)
    else {
        panic!("no FEL verdict stored by the title job");
    };
    println!(
        "FEL qualification via the title job: {} seconds={} verdict={}",
        source.display(),
        started.elapsed().as_secs(),
        serde_json::to_string(&verdict).expect("JSON")
    );
}

fn stored_title() -> TitleDemand {
    let stats = serde_json::json!({
        "schema": 1, "complete": true, "decoded_frames": 3, "valid_l1_frames": 3,
        "missing_l1_frames": 0, "invalid_l1_frames": 0, "max_l1_pq": 3000,
        "mastering_luminance": {"state": "unknown"}, "mastering_primaries": {"state": "unknown"}
    });
    TitleDemand {
        file_id: 9,
        probe_revision: 7,
        source_size: 1,
        source_mtime: 1,
        probe_json: serde_json::json!({
            "streams": [],
            plurx_core::scan::dv_title::PROBE_FIELD: stats,
        })
        .to_string(),
    }
}

/// A job settles with a stored aggregate only when its bind left the
/// generation where the document was read; a moved generation reads the file.
#[test]
fn a_moved_generation_reuses_no_stored_aggregate() {
    let demand = stored_title();
    let reused = reusable_aggregate(&demand, 7).expect("same generation");
    assert_eq!(reused.decoded_frames, 3);
    assert_eq!(reused.max_l1_pq, Some(3000));
    assert_eq!(reusable_aggregate(&demand, 8), None);
    let unscanned = TitleDemand {
        probe_json: r#"{"streams":[]}"#.into(),
        ..stored_title()
    };
    assert_eq!(reusable_aggregate(&unscanned, 7), None);
}

/// Discovery waits for the local index pass on a fresh title, decides reuse
/// from the generation its own bind returned, and only then rebinds the
/// demand it carries to the job.
#[test]
fn title_discovery_waits_for_the_pass_then_reuses_by_generation() {
    let source = include_str!("title_work.rs");
    let wait = source
        .find("state.jobs.title_scan_awaits_index_pass(file.id)")
        .expect("discovery waits for the index pass");
    let bind = source
        .find(".bind_dv_title_demand(demand.probe_revision, &scanner)")
        .expect("bind");
    let reuse = source
        .find("let reused = reusable_aggregate(&demand, revision);")
        .expect("reuse");
    let moved = source
        .find("demand.probe_revision = revision;")
        .expect("rebind");
    assert!(wait < bind && bind < reuse && reuse < moved);
    assert!(source.contains("let result = match reused {\n        Some(stats) => Ok(stats),"));
}

#[test]
fn a_fresh_title_waits_only_for_a_local_index_pass() {
    let dir = crate::test_tempdir().expect("scratch");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let state = AppState::new(
        "test".into(),
        Arc::clone(&store),
        crate::state::Dirs {
            artwork: dir.path().join("artwork"),
            transcode: dir.path().join("transcode"),
            cache: dir.path().join("cache"),
            subs: dir.path().join("subs"),
            runtime_cache: dir.path().join("runtime"),
            renditions: dir.path().join("renditions"),
        },
        "title-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(16)),
    );
    state.jobs.note_fresh_index_files(vec![5]);
    // The cluster queue never drains the fresh list: nothing waits on it.
    assert!(!state.jobs.title_scan_awaits_index_pass(5));
    state.jobs.set_local_index_pass(true);
    assert!(state.jobs.title_scan_awaits_index_pass(5));
    assert!(!state.jobs.title_scan_awaits_index_pass(6));
    // Never forever: a pass that has not reached the file reads it no later.
    if let Some(long_ago) = std::time::Instant::now()
        .checked_sub(crate::state::TITLE_SCAN_INDEX_WAIT + std::time::Duration::from_secs(1))
    {
        state.jobs.note_fresh_index_files_at(vec![7], long_ago);
        assert!(!state.jobs.title_scan_awaits_index_pass(7));
    }
}
