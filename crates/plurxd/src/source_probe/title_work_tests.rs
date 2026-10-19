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
    let unreadable = store
        .upsert_file(
            item,
            dir.path().join("offline.mkv").to_str().expect("path"),
            size,
            mtime,
            &Default::default(),
        )
        .await
        .expect("offline file");
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
    for index in 0..129 {
        let id = uuid::Uuid::new_v4().to_string();
        let file_id = if index < 128 { unreadable } else { file };
        let pipeline = "a".repeat(64);
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
                            &coordinator,
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
    // An offline/expired coordinator is not runnable, even with readable media.
    store
        .release_lease(&lease, now_ms())
        .await
        .expect("coordinator offline");
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
