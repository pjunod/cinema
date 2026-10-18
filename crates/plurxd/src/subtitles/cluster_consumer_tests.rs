use super::*;
use plurx_core::domain::{ItemKind, NewItem, NewLibrary, ProbeResult, SubtitleStream};
use plurx_core::store::{
    keys, ClusterFragmentIndexStore, LibraryStore, MediaStore, SettingsStore, SqliteStore,
    SubtitleSourcePublication, SubtitleSourceStamp,
};

/// Plan P-02 §3.2.2, review of #518 finding 1: a whole-track extraction
/// runs at the class its caller passes. The `/subs` request a viewer's
/// player waits on is realtime; the native-HLS warm-up nobody waits on is
/// background. Before the fix every whole-track extraction was background.
#[tokio::test]
async fn a_whole_track_extraction_runs_at_the_class_its_caller_passes() {
    use crate::process_control::{priority::spawns_of, ChildClass};
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(97_002, &[Sub::Srt]);
    let store = SqliteStore::open_in_memory().expect("SQLite");
    let file = catalogued_text_source(&store, &fixture.source).await;
    let off = crate::subtitle_source::StoreAccess::off();

    let viewer = crate::http::stream::SUBTITLE_TRACK_FOR_A_VIEWER;
    assert_eq!(viewer.class, ChildClass::Realtime);
    let before = spawns_of(viewer);
    let bytes =
        ensure_vtt_bytes_with_store(&fixture.dir.path().join("viewer"), &file, 0, &off, viewer)
            .await
            .expect("the viewer's track");
    assert!(std::str::from_utf8(&bytes).expect("VTT").contains("hello"));
    assert!(
        spawns_of(viewer) > before,
        "the viewer's extraction was not started at the realtime class"
    );

    assert_eq!(SUBTITLE_WARM_UP.class, ChildClass::Background);
    let before = spawns_of(SUBTITLE_WARM_UP);
    let warm = fixture.dir.path().join("warm");
    warm_vtt_with_store(&warm, &file, 0, &off).await;
    let published = vtt_path(&warm, &file, 0);
    tokio::time::timeout(Duration::from_secs(30), async {
        while !valid_sidecar(&published, MAX_SIDECAR_BYTES).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the detached warm-up published its sidecar");
    assert!(
        spawns_of(SUBTITLE_WARM_UP) > before,
        "the warm-up's extraction was not started at the background class"
    );
}

/// Lab N7 (2026-10-11): deleting a library left every extracted
/// subtitle of its files in the cache for good. The sweep removes the
/// sidecars of files that are gone, a page of file ids at a time, and
/// leaves a live file's sidecars and anything it did not write alone.
#[tokio::test]
async fn the_sidecars_of_deleted_files_are_swept_and_a_live_files_kept() {
    let dir = crate::test_tempdir().expect("dir");
    let source = dir.path().join("live.mkv");
    std::fs::write(&source, b"a catalogued source").expect("source");
    let store = SqliteStore::open_in_memory().expect("SQLite");
    let live = catalogued_text_source(&store, &source).await;
    let (gone, also_gone) = (live.id + 1, live.id + 2);
    let subs = dir.path().join("subs");
    std::fs::create_dir_all(subs.join("pgs")).expect("subs");
    let kept = [
        vtt_name(live.id, 0, live.size, live.mtime),
        "notes.txt".to_owned(),
        format!("f{gone}-something-else.vtt"),
    ];
    let swept = [
        vtt_name(gone, 0, 3, 4),
        format!("f{gone}-s1-3-4-w0-60.vtt"),
        format!("f{also_gone}-s0-abc-burn-v2.mks"),
    ];
    for name in kept.iter().chain(&swept) {
        std::fs::write(subs.join(name), b"sidecar").expect("sidecar");
    }

    let first = sweep_orphaned_sidecars(&store, &subs, None, 2)
        .await
        .expect("first page");
    assert_eq!(
        first,
        SidecarSweep {
            removed: 2,
            next: Some(gone)
        }
    );
    let second = sweep_orphaned_sidecars(&store, &subs, first.next, 2)
        .await
        .expect("second page");
    assert_eq!(
        second,
        SidecarSweep {
            removed: 1,
            next: None
        }
    );
    for name in &kept {
        assert!(subs.join(name).exists(), "{name} must be kept");
    }
    for name in &swept {
        assert!(!subs.join(name).exists(), "{name} must be swept");
    }
    assert!(subs.join("pgs").is_dir());
}

async fn catalogued_text_source(store: &SqliteStore, source: &Path) -> MediaFile {
    let library = store
        .create_library(&NewLibrary {
            name: "Cluster subtitle consumer".to_owned(),
            kind: plurx_core::domain::LibraryKind::Movies,
            paths: vec![source.parent().expect("parent").to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Subtitle fixture".to_owned(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let metadata = std::fs::metadata(source).expect("source metadata");
    let stamp = crate::fragment_index_cluster::source_stamp(&metadata);
    let probe = ProbeResult {
        container: Some("mkv".to_owned()),
        subtitle_streams: vec![SubtitleStream {
            index: 0,
            codec: "subrip".to_owned(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let id = store
        .upsert_file(
            item,
            source.to_str().expect("source path"),
            metadata.len() as i64,
            stamp.mtime,
            &probe,
        )
        .await
        .expect("catalogue source");
    MediaFile {
        id,
        item_id: item,
        path: source.to_owned(),
        size: metadata.len() as i64,
        mtime: stamp.mtime,
        duration_ms: Some(16_000),
        container: probe.container,
        video_codec: Some("h264".to_owned()),
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: Some(64),
        height: Some(64),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        dolby_vision: Default::default(),
        bitrate: None,
        audio_streams: Vec::new(),
        subtitle_streams: probe.subtitle_streams,
        downloaded_subtitles: Vec::new(),
        scanned_at: 0,
        audio_offset_ms: 0,
        probed: true,
    }
}

async fn access(store: Arc<SqliteStore>, base: &Path) -> crate::subtitle_source::StoreAccess {
    for key in [
        keys::VOD_INDEX_CLUSTER_CACHE,
        keys::SUBTITLE_CLUSTER_SOURCES,
        keys::SUBTITLE_STORED_SOURCES,
    ] {
        store.put_setting(key, "1").await.expect("enable fixture");
    }
    let jobs = crate::state::JobManager::test_new(store.clone(), base.join("artwork"));
    crate::subtitle_source::StoreAccess::from_setting(store, &base.join("runtime"))
        .on_node(Some("test-node"))
        .with_jobs(jobs)
}

async fn source_and_access(
    base: &Path,
) -> (
    Arc<SqliteStore>,
    MediaFile,
    crate::subtitle_source::StoreAccess,
) {
    let source = base.join("source.mkv");
    std::fs::write(&source, b"not needed by queue-state tests").expect("source");
    let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
    let file = catalogued_text_source(&store, &source).await;
    let access = access(Arc::clone(&store), base).await;
    (store, file, access)
}

async fn stamp(file: &MediaFile) -> SubtitleSourceStamp {
    SubtitleSourceStamp {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        pipeline_version: crate::state::subtitle_source_pipeline_version().await,
    }
}

async fn source_digest(file: &MediaFile) -> String {
    crate::fragment_index_cluster::attest_source("test-node", file, None, &|_| {})
        .await
        .expect("attest fixture")
        .observation
        .source_sha256
}

#[tokio::test]
async fn cancelled_job_memos_for_negative_ttl_without_reenqueue() {
    let base = crate::test_tempdir().expect("fixture");
    let (store, file, access) = source_and_access(base.path()).await;
    let request = store
        .enqueue_or_promote_subtitle_source(&stamp(&file).await, "foreground", subtitle_clock_ms())
        .await
        .expect("enqueue")
        .expect("request");
    store
        .cancel_analysis_request_admin(&request.request_id, subtitle_clock_ms())
        .await
        .expect("cancel request");
    let cache = base.path().join("subs");
    let first = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
        .await
        .expect_err("operator cancellation is terminal for this flight");
    assert!(first.contains("cancelled"));
    let cached = vtt_path(&cache, &file, 0);
    assert_eq!(
        remembered_failure(&cached).await.as_deref(),
        Some(first.as_str())
    );
    let second = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
        .await
        .expect_err("negative memo refuses immediate retry");
    assert_eq!(second, first);
    let rows = store.analysis_requests(10).await.expect("requests");
    assert_eq!(rows.len(), 1, "memo cannot fork a second producer");
    assert_eq!(rows[0].state, "cancelled");
    assert!(!rows[0].force_rebuild);
}

#[tokio::test(start_paused = true)]
async fn unreachable_holders_release_inline_after_claim_wait_without_second_producer() {
    let base = crate::test_tempdir().expect("fixture");
    let (store, file, access) = source_and_access(base.path()).await;
    store
        .upsert_subtitle_source_publication(&SubtitleSourcePublication {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_attestation: source_digest(&file).await,
            node_id: "unreachable-node".to_owned(),
            ordinal: 0,
            kind: "text".to_owned(),
            format: "webvtt".to_owned(),
            verdict: "kept".to_owned(),
            attempts: 1,
            origin: "extracted".to_owned(),
            sha256: "b".repeat(64),
            bytes: 25,
            published_at_ms: subtitle_clock_ms(),
        })
        .await
        .expect("holder publication");
    let tmp = base.path().join("candidate.vtt");
    assert!(!cluster_vtt_into(&tmp, &file, 0, &access)
        .await
        .expect("release to inline"));
    assert!(store
        .analysis_requests(10)
        .await
        .expect("requests")
        .is_empty());
    assert!(
        !tmp.exists(),
        "cluster preference did not fabricate a sidecar"
    );
}

#[tokio::test]
async fn terminal_failed_row_releases_inline_path() {
    let base = crate::test_tempdir().expect("fixture");
    let (store, file, access) = source_and_access(base.path()).await;
    let now = subtitle_clock_ms();
    let request = store
        .enqueue_or_promote_subtitle_source(&stamp(&file).await, "foreground", now)
        .await
        .expect("enqueue")
        .expect("request");
    let claimed = store
        .claim_subtitle_fixture(&request.request_id, "test-node", now, now + 120_000)
        .await
        .expect("claim")
        .expect("running request");
    assert!(store
        .fail_subtitle_fixture(&claimed, "stored_probe_invalid", now)
        .await
        .expect("fail request"));
    let tmp = base.path().join("candidate.vtt");
    assert!(!cluster_vtt_into(&tmp, &file, 0, &access)
        .await
        .expect("terminal failure releases inline"));
    let rows = store.analysis_requests(10).await.expect("requests");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "failed");
    assert!(!rows[0].force_rebuild);
}

#[tokio::test]
async fn source_empty_publication_completes_vtt_without_inline_extraction() {
    let base = crate::test_tempdir().expect("fixture");
    let (store, file, access) = source_and_access(base.path()).await;
    store
        .upsert_subtitle_source_publication(&SubtitleSourcePublication {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_attestation: source_digest(&file).await,
            node_id: "producer".to_owned(),
            ordinal: 0,
            kind: "text".to_owned(),
            format: "webvtt".to_owned(),
            verdict: "empty".to_owned(),
            attempts: 1,
            origin: "extracted".to_owned(),
            sha256: String::new(),
            bytes: 0,
            published_at_ms: subtitle_clock_ms(),
        })
        .await
        .expect("empty publication");
    let cache = base.path().join("subs");
    let path = ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
        .await
        .expect("empty representation");
    assert_eq!(std::fs::read(path).expect("sidecar"), b"WEBVTT\n\n");
    assert!(store
        .analysis_requests(10)
        .await
        .expect("requests")
        .is_empty());
}

#[tokio::test]
async fn direct_text_and_offline_cold_queued_requests_complete_without_500() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(97_000, &[Sub::Srt]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
    let file = catalogued_text_source(&store, &fixture.source).await;
    let access = access(Arc::clone(&store), fixture.dir.path()).await;
    let jobs = Arc::clone(access.jobs().expect("queue worker"));
    let cache = fixture.dir.path().join("subs");

    let direct = {
        let access = access.clone();
        let file = file.clone();
        let cache = cache.clone();
        tokio::spawn(async move {
            ensure_vtt_bytes_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
                .await
        })
    };
    let offline = {
        let access = access.clone();
        let file = file.clone();
        let cache = cache.clone();
        tokio::spawn(async move {
            ensure_vtt_file_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK)
                .await
        })
    };
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(request) = store
                .analysis_requests(10)
                .await
                .expect("queued requests")
                .into_iter()
                .next()
            {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("consumer queued foreground work");
    assert_eq!(request.priority, "foreground");
    assert!(!request.force_rebuild);
    assert!(
        jobs.self_claim_subtitle_source(&request.request_id, &fixture.dir.path().join("runtime"))
            .await
    );
    let direct = direct
        .await
        .expect("direct task")
        .expect("direct VTT bytes");
    let mut offline = offline
        .await
        .expect("offline task")
        .expect("offline VTT file");
    let mut offline_bytes = Vec::new();
    std::io::Read::read_to_end(&mut offline, &mut offline_bytes).expect("offline VTT bytes");
    assert_eq!(direct, offline_bytes);
    assert!(std::str::from_utf8(&direct).expect("VTT").contains("hello"));
    let rows = store.analysis_requests(10).await.expect("requests");
    assert_eq!(rows.len(), 1, "both callers joined one producer");
    assert_eq!(rows[0].state, "ready");
}

#[tokio::test]
async fn cold_hls_rendition_warm_enqueues_and_completes_queued_source() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(97_001, &[Sub::Srt]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
    let file = catalogued_text_source(&store, &fixture.source).await;
    let access = access(Arc::clone(&store), fixture.dir.path()).await;
    let jobs = Arc::clone(access.jobs().expect("queue worker"));
    let cache = fixture.dir.path().join("hls-subs");

    // The HLS segment returns while its detached warmer owns the flight.
    warm_vtt_with_store(&cache, &file, 0, &access).await;
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(request) = store
                .analysis_requests(10)
                .await
                .expect("queued requests")
                .into_iter()
                .next()
            {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached HLS warm enqueued its source");
    assert_eq!(request.priority, "foreground");
    assert!(!request.force_rebuild);
    assert!(
        jobs.self_claim_subtitle_source(&request.request_id, &fixture.dir.path().join("runtime"))
            .await
    );
    let sidecar = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sidecar = vtt_path(&cache, &file, 0);
            if valid_sidecar(&sidecar, MAX_SIDECAR_BYTES).await {
                break sidecar;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("queue result reached the detached HLS warmer");
    assert!(std::fs::read_to_string(sidecar)
        .expect("VTT")
        .contains("hello"));
    assert_eq!(
        store.analysis_requests(10).await.expect("requests").len(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn remote_running_beyond_wait_bound_never_starts_inline_producer() {
    let base = crate::test_tempdir().expect("fixture");
    let (store, file, access) = source_and_access(base.path()).await;
    let now = subtitle_clock_ms();
    let request = store
        .enqueue_or_promote_subtitle_source(&stamp(&file).await, "foreground", now)
        .await
        .expect("enqueue")
        .expect("request");
    store
        .claim_subtitle_fixture(&request.request_id, "remote-worker", now, now + 1_800_000)
        .await
        .expect("claim")
        .expect("running row");

    let cache = base.path().join("vtt-cache");
    let error = tokio::time::timeout(
        Duration::from_secs(670),
        ensure_vtt_with_store(&cache, &file, 0, &access, crate::subtitles::TEST_TRACK),
    )
    .await
    .expect("bounded VTT wait")
    .expect_err("active worker cannot release inline");
    assert!(error.contains("still running"), "{error}");
    assert_eq!(
        remembered_failure(&vtt_path(&cache, &file, 0)).await,
        Some(error)
    );

    let mut pgs = file.clone();
    pgs.subtitle_streams[0].codec = "hdmv_pgs_subtitle".to_owned();
    let source = crate::fragment_index_cluster::open_source_fence(&pgs, None)
        .await
        .expect("held source");
    let error = tokio::time::timeout(
        Duration::from_secs(610),
        cluster_wait_burn(&access, &pgs, 0, &source),
    )
    .await
    .expect("bounded burn wait")
    .err()
    .expect("active worker cannot release burn inline");
    assert!(error.contains("still running"), "{error}");
    let rows = store.analysis_requests(10).await.expect("requests");
    assert_eq!(rows.len(), 1, "neither consumer forked a producer");
    assert_eq!(rows[0].state, "running");
    assert!(!rows[0].force_rebuild);
}
