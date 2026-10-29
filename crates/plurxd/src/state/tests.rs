use super::*;
use plurx_core::domain::{
    DolbyVisionFacts, ItemEdit, ItemKind, NewItem, NewLibrary, PlaybackEventQuery, ProbeResult,
};

#[test]
fn subtitle_source_job_argv_has_no_index_pipe_and_ends_in_null_sentinel() {
    let temp = tempfile::tempdir().expect("stage root");
    let source = temp.path().join("movie.mkv");
    std::fs::write(&source, b"source").expect("source");
    let stamp =
        crate::fragment_index_cluster::source_stamp(&std::fs::metadata(&source).expect("metadata"));
    let plan =
        crate::subtitle_ride_along::RideAlongPlan::standalone(temp.path(), 7, stamp, vec![0])
            .expect("plan");
    let argv = subtitle_source_argv("/dev/fd/3", &plan);
    assert!(argv.iter().all(|arg| arg != "pipe:1"));
    assert!(argv.last().is_some_and(|arg| arg.ends_with("[f=null]-")));
    assert!(argv.windows(2).any(|pair| pair == ["-i", "/dev/fd/3"]));
}

#[test]
fn subtitle_source_full_extracted_coverage_is_a_no_op_before_spawning() {
    use plurx_core::store::SubtitleSourcePublication;
    let tracks = [
        crate::subtitle_ride_along::ProbedTrack {
            ordinal: 0,
            kind: crate::subtitle_ride_along::ProbedKind::Text,
            stream_index: 1,
        },
        crate::subtitle_ride_along::ProbedTrack {
            ordinal: 1,
            kind: crate::subtitle_ride_along::ProbedKind::Pgs,
            stream_index: 2,
        },
    ];
    let mut row = SubtitleSourcePublication {
        file_id: 7,
        source_size: 10,
        source_mtime: 1,
        source_attestation: "a".repeat(64),
        node_id: "peer".to_owned(),
        ordinal: 0,
        kind: "text".to_owned(),
        format: "webvtt".to_owned(),
        verdict: "kept".to_owned(),
        attempts: 1,
        origin: "hydrated".to_owned(),
        sha256: "b".repeat(64),
        bytes: 5,
        published_at_ms: 1,
    };
    assert!(!subtitle_source_covered(&tracks, &[row.clone()]));
    row.origin = "extracted".to_owned();
    let text_row = row.clone();
    row.ordinal = 1;
    row.kind = "pgs".to_owned();
    row.format = "sup".to_owned();
    row.verdict = "transient".to_owned();
    row.attempts = i64::from(crate::subtitle_source::TRANSIENT_ATTEMPTS) - 1;
    assert!(!subtitle_source_covered(
        &tracks,
        &[text_row.clone(), row.clone()]
    ));
    row.attempts += 1;
    assert!(subtitle_source_covered(&tracks, &[text_row, row]));
}

#[test]
fn luminance_backfill_reads_stream_facts_without_opening_media() {
    let recovered = stored_luminance(
        r#"{"streams":[{"codec_type":"video","color_transfer":"smpte2084","side_data_list":[{"side_data_type":"Content light level metadata","max_content":4000,"max_average":1000},{"side_data_type":"Mastering display metadata","max_luminance":"40000000/10000"}]}]}"#,
    );
    assert_eq!(recovered.max_cll, Some(4000));
    assert_eq!(recovered.max_fall, Some(1000));
    assert_eq!(recovered.mastering_max_luminance, Some(4000));
    assert_eq!(recovered.luminance_source.as_deref(), Some("stream"));
    assert_eq!(stored_luminance("not json").luminance_source, None);
}

/// A file-backed catalogue with one library root and one movie item, for
/// driving the first-frame luminance backfill with a stubbed frame read.
struct LuminanceFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    database: PathBuf,
    store: Arc<SqliteStore>,
    item: i64,
    jobs: Arc<JobManager>,
}

impl LuminanceFixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("fixture");
        let root = dir.path().join("library");
        std::fs::create_dir(&root).expect("library root");
        let database = dir.path().join("catalogue.sqlite");
        let store = Arc::new(SqliteStore::open(&database).expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "HDR".to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![root.clone()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Fixture".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let artwork = dir.path().join("artwork");
        std::fs::create_dir(&artwork).expect("artwork");
        let store_handle: Arc<dyn Store> = store.clone();
        let jobs = Arc::new(JobManager::new(store_handle, artwork));
        Self {
            _dir: dir,
            root,
            database,
            store,
            item,
            jobs,
        }
    }

    async fn document_walk_done(&self) {
        self.store
            .put_setting(keys::JOB_LUMINANCE_BACKFILL_DONE, "1")
            .await
            .expect("document walk done");
    }

    /// Seed one catalogued file whose on-disk identity matches its row.
    async fn row(
        &self,
        name: &str,
        hdr: Option<&str>,
        luminance_source: Option<&str>,
        transfer: &str,
    ) -> (i64, PathBuf) {
        let path = self.root.join(name);
        std::fs::write(&path, path.to_string_lossy().as_bytes()).expect("media stand-in");
        let (size, mtime) = scan::file_stat(&path).await.expect("stat");
        let id = self
                .store
                .upsert_file(
                    self.item,
                    path.to_str().expect("utf8 path"),
                    size,
                    mtime,
                    &ProbeResult {
                        hdr: hdr.map(str::to_owned),
                        luminance_source: luminance_source.map(str::to_owned),
                        raw_json: Some(format!(
                            r#"{{"streams":[{{"codec_type":"video","color_transfer":"{transfer}"}}],"path":"{}"}}"#,
                            path.display()
                        )),
                        ..Default::default()
                    },
                )
                .await
                .expect("file");
        (id, path)
    }

    async fn run<F, Fut>(&self, read_frame: F)
    where
        F: Fn(PathBuf) -> Fut,
        Fut: std::future::Future<
            Output = Result<
                Option<plurx_core::scan::probe::FrameLuminance>,
                plurx_core::error::ProbeError,
            >,
        >,
    {
        self.jobs
            .backfill_frame_luminance_with(read_frame, || std::future::ready(true))
            .await;
    }

    async fn run_with_self_check<F, Fut>(&self, read_frame: F, self_check: bool)
    where
        F: Fn(PathBuf) -> Fut,
        Fut: std::future::Future<
            Output = Result<
                Option<plurx_core::scan::probe::FrameLuminance>,
                plurx_core::error::ProbeError,
            >,
        >,
    {
        self.jobs
            .backfill_frame_luminance_with(read_frame, || std::future::ready(self_check))
            .await;
    }

    async fn cursor(&self) -> Option<String> {
        let key = self
            .jobs
            .local_job_key(keys::JOB_LUMINANCE_FRAME_BACKFILL_CURSOR);
        self.store.get_setting(&key).await.expect("cursor")
    }

    async fn done(&self) -> bool {
        self.store
            .get_setting(keys::JOB_LUMINANCE_FRAME_BACKFILL_DONE)
            .await
            .expect("stamp")
            .is_some()
    }

    async fn file(&self, id: i64) -> MediaFile {
        self.store.get_file(id).await.expect("read").expect("row")
    }

    /// Seed one SDR-catalogued file whose stored stream document carries
    /// the given colour signalling.
    async fn stream_row(
        &self,
        name: &str,
        codec: &str,
        bit_depth: i64,
        transfer: Option<&str>,
        primaries: Option<&str>,
    ) -> (i64, PathBuf) {
        let path = self.root.join(name);
        std::fs::write(&path, path.to_string_lossy().as_bytes()).expect("media stand-in");
        let (size, mtime) = scan::file_stat(&path).await.expect("stat");
        let mut stream = serde_json::json!({
            "codec_type": "video",
            "codec_name": codec,
            "pix_fmt": if bit_depth >= 10 { "yuv420p10le" } else { "yuv420p" },
        });
        if let Some(transfer) = transfer {
            stream["color_transfer"] = serde_json::json!(transfer);
        }
        if let Some(primaries) = primaries {
            stream["color_primaries"] = serde_json::json!(primaries);
        }
        let document = serde_json::json!({"streams": [stream], "path": path.display().to_string()});
        let id = self
            .store
            .upsert_file(
                self.item,
                path.to_str().expect("utf8 path"),
                size,
                mtime,
                &ProbeResult {
                    video_codec: Some(codec.to_owned()),
                    bit_depth: Some(bit_depth),
                    raw_json: Some(document.to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        (id, path)
    }

    async fn run_unsignalled<F, Fut>(&self, digest: &str, read_frame: F)
    where
        F: Fn(PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, plurx_core::error::ProbeError>>,
    {
        self.jobs
            .backfill_unsignalled_transfer_with(digest, read_frame, || std::future::ready(true))
            .await;
    }

    async fn run_unsignalled_with_self_check<F, Fut>(
        &self,
        digest: &str,
        read_frame: F,
        self_check: bool,
    ) where
        F: Fn(PathBuf) -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, plurx_core::error::ProbeError>>,
    {
        self.jobs
            .backfill_unsignalled_transfer_with(digest, read_frame, || {
                std::future::ready(self_check)
            })
            .await;
    }

    async fn unsignalled_cursor(&self) -> Option<String> {
        let key = self
            .jobs
            .local_job_key(keys::JOB_UNSIGNALLED_TRANSFER_BACKFILL_CURSOR);
        self.store.get_setting(&key).await.expect("cursor")
    }

    async fn unsignalled_done(&self) -> Option<String> {
        self.store
            .get_setting(keys::JOB_UNSIGNALLED_TRANSFER_BACKFILL_DONE)
            .await
            .expect("stamp")
    }
}

fn frame_with_transfer(transfer: &str) -> serde_json::Value {
    serde_json::json!({"frames": [{
        "color_transfer": transfer,
        "color_primaries": "bt2020",
        "side_data_list": [{
            "side_data_type": "Content light level metadata",
            "max_content": 1000, "max_average": 400
        }]
    }]})
}

/// Lab F4, 2026-10-11: titles catalogued SDR before the scanner asked the
/// first frame are never rescanned, so they need this walk to become HDR.
#[tokio::test]
async fn unsignalled_transfer_backfill_recatalogues_hdr_rows_once_per_pipeline_digest() {
    let fixture = LuminanceFixture::new().await;
    let (pq_id, pq) = fixture
        .stream_row("pq.mkv", "hevc", 10, Some("unknown"), None)
        .await;
    let (sdr_id, sdr) = fixture
        .stream_row("sdr10.mkv", "hevc", 10, Some("unknown"), None)
        .await;
    let (hlg_id, hlg) = fixture
        .stream_row("hlg.mkv", "hevc", 10, None, Some("bt2020"))
        .await;
    // Listed by the store's coarse filter (10-bit), skipped by the
    // scanner's eligibility: the stream signals its transfer.
    let (signalled_id, _) = fixture
        .stream_row("signalled.mkv", "hevc", 10, Some("bt709"), None)
        .await;
    // Not a candidate at all: 8-bit H.264.
    let (h264_id, _) = fixture
        .stream_row("h264.mkv", "h264", 8, Some("unknown"), None)
        .await;
    let (refused_id, refused) = fixture
        .stream_row("refused.mkv", "av1", 10, Some("unspecified"), None)
        .await;

    let reads = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let read_frame = {
        let reads = Arc::clone(&reads);
        let (pq, hlg, refused) = (pq.clone(), hlg.clone(), refused.clone());
        move |path: PathBuf| {
            reads.lock().expect("reads").push(path.clone());
            let answer = if path == pq {
                Ok(frame_with_transfer("smpte2084"))
            } else if path == hlg {
                Ok(frame_with_transfer("arib-std-b67"))
            } else if path == refused {
                Err(refusal(&path))
            } else {
                Ok(frame_with_transfer("bt709"))
            };
            async move { answer }
        }
    };

    fixture.run_unsignalled("digest-one", &read_frame).await;
    assert_eq!(
        *reads.lock().expect("reads"),
        vec![pq.clone(), sdr.clone(), hlg.clone(), refused.clone()],
        "only the rows a fresh scan would frame-read, in id order"
    );
    let recatalogued = fixture.file(pq_id).await;
    assert_eq!(
        (
            recatalogued.hdr.as_deref(),
            recatalogued.hdr_format.as_deref(),
            recatalogued.max_cll,
            recatalogued.max_fall,
            recatalogued.luminance_source.as_deref()
        ),
        (
            Some("hdr10"),
            Some("HDR10"),
            Some(1000),
            Some(400),
            Some("frame")
        )
    );
    let hlg_row = fixture.file(hlg_id).await;
    assert_eq!(
        (hlg_row.hdr.as_deref(), hlg_row.hdr_format.as_deref()),
        (Some("hlg"), Some("HLG"))
    );
    for id in [sdr_id, signalled_id, h264_id, refused_id] {
        let row = fixture.file(id).await;
        assert_eq!(row.hdr, None, "row {id} stays SDR");
        assert_eq!(row.luminance_source, None, "row {id} stays SDR");
    }
    assert_eq!(
        fixture.unsignalled_cursor().await,
        Some(format!("digest-one:{refused_id}")),
        "a frame that says SDR, an ineligible row and one refusal beside \
             successful reads all advance the cursor"
    );
    assert_eq!(fixture.unsignalled_done().await, None);

    // The next tick finds nothing after the cursor and stamps the digest;
    // a walk completed for this digest never reads again.
    fixture.run_unsignalled("digest-one", &read_frame).await;
    assert_eq!(
        fixture.unsignalled_done().await.as_deref(),
        Some("digest-one")
    );
    fixture.run_unsignalled("digest-one", &read_frame).await;
    assert_eq!(reads.lock().expect("reads").len(), 4);

    // Another pipeline digest — a new classification version or another
    // FFprobe build — walks again from the start, and reads only the rows
    // that are still SDR.
    reads.lock().expect("reads").clear();
    fixture.run_unsignalled("digest-two", &read_frame).await;
    assert_eq!(
        *reads.lock().expect("reads"),
        vec![sdr.clone(), refused.clone()]
    );
    assert_eq!(
        fixture.unsignalled_cursor().await,
        Some(format!("digest-two:{refused_id}"))
    );
}

#[tokio::test]
async fn unsignalled_transfer_backfill_holds_its_cursor_when_this_node_cannot_read() {
    use plurx_core::error::ProbeError;
    let fixture = LuminanceFixture::new().await;
    fixture
        .stream_row("first.mkv", "hevc", 10, Some("unknown"), None)
        .await;
    fixture
        .stream_row("second.mkv", "hevc", 10, Some("unknown"), None)
        .await;
    let spawn =
        |_path: PathBuf| async { Err(ProbeError::Spawn("No such file or directory".to_owned())) };
    fixture.run_unsignalled("digest", spawn).await;
    assert_eq!(
        fixture.unsignalled_cursor().await,
        None,
        "Spawn stops the page"
    );
    let transient = |path: PathBuf| async move {
        Err(ProbeError::Transient {
            path: path.display().to_string(),
            reason: "ffprobe exceeded 30 s".to_owned(),
        })
    };
    fixture.run_unsignalled("digest", transient).await;
    assert_eq!(
        fixture.unsignalled_cursor().await,
        None,
        "Transient stops the page"
    );
    let refuse_all = |path: PathBuf| async move { Err(refusal(&path)) };
    fixture
        .run_unsignalled_with_self_check("digest", refuse_all, false)
        .await;
    assert_eq!(
        fixture.unsignalled_cursor().await,
        None,
        "an all-refused page holds while ffprobe fails its self-check"
    );
    // An unmounted share holds the cursor without a read.
    let parked = fixture.root.with_extension("parked");
    std::fs::rename(&fixture.root, &parked).expect("unmount");
    std::fs::create_dir(&fixture.root).expect("empty mountpoint");
    let reads = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let recorded = {
        let reads = Arc::clone(&reads);
        move |path: PathBuf| {
            reads.lock().expect("reads").push(path);
            async { Ok(frame_with_transfer("smpte2084")) }
        }
    };
    fixture.run_unsignalled("digest", &recorded).await;
    assert!(
        reads.lock().expect("reads").is_empty(),
        "nothing is read while the root is away"
    );
    assert_eq!(fixture.unsignalled_cursor().await, None);
    assert_eq!(fixture.unsignalled_done().await, None);
}

/// Review finding on the frame walks: a page whose only reads were
/// refusals of corrupt files reset the cursor every tick, so two corrupt
/// files at the end of a library were re-read every 60 s and the walk
/// never stamped done. With ffprobe passing its self-check the refusals
/// are verdicts on those files and the walk completes.
#[tokio::test]
async fn frame_luminance_backfill_completes_past_a_page_of_corrupt_files() {
    let fixture = LuminanceFixture::new().await;
    fixture.document_walk_done().await;
    let (_, sdr) = fixture.row("sdr.mkv", None, None, "bt709").await;
    let (_, first) = fixture
        .row("corrupt-a.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (second_id, second) = fixture
        .row("corrupt-b.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let reads = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let refuse_all = {
        let reads = Arc::clone(&reads);
        move |path: PathBuf| {
            reads.lock().expect("reads").push(path.clone());
            let answer = Err(refusal(&path));
            async move { answer }
        }
    };
    fixture.run(&refuse_all).await;
    assert_eq!(fixture.cursor().await, Some(second_id.to_string()));
    fixture.run(&refuse_all).await;
    assert!(fixture.done().await, "the walk completes");
    fixture.run(&refuse_all).await;
    assert_eq!(
        *reads.lock().expect("reads"),
        vec![first, second],
        "each corrupt file is read once, never again"
    );
    assert!(!reads.lock().expect("reads").contains(&sdr));
}

#[tokio::test]
async fn unsignalled_transfer_backfill_completes_past_a_page_of_corrupt_files() {
    let fixture = LuminanceFixture::new().await;
    let (ineligible_id, _) = fixture
        .stream_row("signalled.mkv", "hevc", 10, Some("bt709"), None)
        .await;
    let (_, first) = fixture
        .stream_row("corrupt-a.mkv", "hevc", 10, Some("unknown"), None)
        .await;
    let (second_id, second) = fixture
        .stream_row("corrupt-b.mkv", "hevc", 10, Some("unknown"), None)
        .await;
    let reads = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let refuse_all = {
        let reads = Arc::clone(&reads);
        move |path: PathBuf| {
            reads.lock().expect("reads").push(path.clone());
            let answer = Err(refusal(&path));
            async move { answer }
        }
    };
    // A node-wide refusal holds at the row before the first refused
    // read, keeping the ineligible row's advance.
    fixture
        .run_unsignalled_with_self_check("digest", &refuse_all, false)
        .await;
    assert_eq!(
        fixture.unsignalled_cursor().await,
        Some(format!("digest:{ineligible_id}"))
    );
    assert_eq!(fixture.unsignalled_done().await, None);
    // Once ffprobe passes its self-check the refusals are verdicts.
    reads.lock().expect("reads").clear();
    fixture.run_unsignalled("digest", &refuse_all).await;
    assert_eq!(
        fixture.unsignalled_cursor().await,
        Some(format!("digest:{second_id}"))
    );
    fixture.run_unsignalled("digest", &refuse_all).await;
    assert_eq!(fixture.unsignalled_done().await.as_deref(), Some("digest"));
    fixture.run_unsignalled("digest", &refuse_all).await;
    assert_eq!(*reads.lock().expect("reads"), vec![first, second]);
}

fn frame(max_cll: i64) -> plurx_core::scan::probe::FrameLuminance {
    plurx_core::scan::probe::FrameLuminance {
        max_cll: Some(max_cll),
        max_fall: Some(612),
        mastering_max_luminance: Some(4000),
    }
}

fn refusal(path: &Path) -> plurx_core::error::ProbeError {
    plurx_core::error::ProbeError::Failed {
        path: path.display().to_string(),
        code: Some(1),
        reason: "Invalid data found when processing input".to_owned(),
    }
}

#[tokio::test]
async fn frame_luminance_backfill_reads_none_rows_once_and_stamps_done() {
    let fixture = LuminanceFixture::new().await;
    let (sei_id, sei) = fixture
        .row("sei.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (bare_id, bare) = fixture
        .row("bare.mkv", Some("hlg"), Some("none"), "arib-std-b67")
        .await;
    let (broken_id, broken) = fixture
        .row("broken.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    // The stored-document walk stamps every unclassified HDR row `none`,
    // including Dolby Vision whose base layer is neither PQ nor HLG; the
    // scanner never frame-reads such a stream, so this walk does not.
    let (dv_id, _) = fixture
        .row("dv.mkv", Some("dolby_vision"), Some("none"), "bt709")
        .await;
    let (stream_id, _) = fixture
        .row("stream.mkv", Some("hdr10"), Some("stream"), "smpte2084")
        .await;
    let (sdr_id, _) = fixture.row("sdr.mkv", None, None, "bt709").await;

    let reads = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let read_frame = {
        let reads = Arc::clone(&reads);
        let (sei, broken) = (sei.clone(), broken.clone());
        move |path: PathBuf| {
            reads.lock().expect("reads").push(path.clone());
            let answer = if path == sei {
                Ok(Some(frame(2008)))
            } else if path == broken {
                Err(refusal(&path))
            } else {
                Ok(None)
            };
            async move { answer }
        }
    };

    // Nothing runs until the stored-document walk has finished: rows it
    // has not yet classified would otherwise fall behind this cursor.
    fixture.run(&read_frame).await;
    assert!(reads.lock().expect("reads").is_empty());
    fixture.document_walk_done().await;

    fixture.run(&read_frame).await;
    assert_eq!(
        *reads.lock().expect("reads"),
        vec![sei.clone(), bare.clone(), broken.clone()],
        "only PQ/HLG rows the document left none are read, in id order"
    );
    let read = fixture.file(sei_id).await;
    assert_eq!(
        (
            read.max_cll,
            read.max_fall,
            read.mastering_max_luminance,
            read.luminance_source.as_deref()
        ),
        (Some(2008), Some(612), Some(4000), Some("frame"))
    );
    for id in [bare_id, broken_id, dv_id] {
        let row = fixture.file(id).await;
        assert_eq!(row.luminance_source.as_deref(), Some("none"));
        assert_eq!(row.max_cll, None);
    }
    assert_eq!(
        fixture.file(stream_id).await.luminance_source.as_deref(),
        Some("stream")
    );
    assert_eq!(fixture.file(sdr_id).await.luminance_source, None);
    assert_eq!(
        fixture.cursor().await,
        Some(dv_id.to_string()),
        "a frame with nothing, a refusal beside successful reads and an \
             ineligible row all advance the cursor"
    );
    assert!(!fixture.done().await);

    // The next tick finds nothing after the cursor and stamps the walk
    // done; a stamped walk never reads again.
    fixture.run(&read_frame).await;
    assert!(fixture.done().await);
    let key = fixture
        .jobs
        .local_job_key(keys::JOB_LUMINANCE_FRAME_BACKFILL_CURSOR);
    fixture.store.put_setting(&key, "0").await.expect("rewind");
    fixture.run(&read_frame).await;
    assert_eq!(reads.lock().expect("reads").len(), 3);
}

#[tokio::test]
async fn frame_luminance_backfill_skips_stale_rows_but_waits_for_an_unavailable_root() {
    let fixture = LuminanceFixture::new().await;
    fixture.document_walk_done().await;
    let (changed_id, changed) = fixture
        .row("changed.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (gone_id, gone) = fixture
        .row("gone.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (dangling_id, dangling) = fixture
        .row("dangling.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (kept_id, kept) = fixture
        .row("kept.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    std::fs::write(&changed, b"a different, longer body than the catalogue saw")
        .expect("change on disk");
    std::fs::remove_file(&gone).expect("delete");
    std::fs::remove_file(&dangling).expect("replace with a link");
    // A dangling link where the platform has them; elsewhere the row is
    // simply another deleted file, with the same expected outcome.
    #[cfg(unix)]
    std::os::unix::fs::symlink(fixture.root.join("missing-target.mkv"), &dangling)
        .expect("dangling link");

    let reads = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let read_frame = {
        let reads = Arc::clone(&reads);
        move |path: PathBuf| {
            reads.lock().expect("reads").push(path);
            async { Ok(None) }
        }
    };

    // Unmounted share: the root is an empty directory (a mountpoint with
    // nothing on it). Nothing is read, nothing is skipped, the cursor
    // does not move.
    let parked = fixture.root.with_extension("parked");
    std::fs::rename(&fixture.root, &parked).expect("unmount");
    std::fs::create_dir(&fixture.root).expect("empty mountpoint");
    fixture.run(&read_frame).await;
    assert_eq!(
        fixture.cursor().await,
        None,
        "an empty mountpoint holds the cursor"
    );
    // Missing root entirely: the same.
    std::fs::remove_dir(&fixture.root).expect("remove mountpoint");
    fixture.run(&read_frame).await;
    assert_eq!(
        fixture.cursor().await,
        None,
        "a missing root holds the cursor"
    );
    assert!(reads.lock().expect("reads").is_empty());

    // Root back: a changed file is the rescan's, a deleted file and a
    // dangling link are stale rows the next scan reconciles; all three
    // advance without a read, and the intact file is read.
    std::fs::rename(&parked, &fixture.root).expect("remount");
    fixture.run(&read_frame).await;
    assert_eq!(*reads.lock().expect("reads"), vec![kept.clone()]);
    assert_eq!(fixture.cursor().await, Some(kept_id.to_string()));
    for id in [changed_id, gone_id, dangling_id, kept_id] {
        assert_eq!(
            fixture.file(id).await.luminance_source.as_deref(),
            Some("none")
        );
    }
    fixture.run(&read_frame).await;
    assert!(fixture.done().await);
}

#[tokio::test]
async fn frame_luminance_backfill_does_not_advance_when_the_reader_cannot_read() {
    use plurx_core::error::ProbeError;
    let fixture = LuminanceFixture::new().await;
    fixture.document_walk_done().await;
    let (first_id, _) = fixture
        .row("first.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (second_id, _) = fixture
        .row("second.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;

    // ffprobe could not run (missing, not executable, exit 126/127) and
    // ffprobe did not finish (timeout, signal) are not verdicts on a file.
    let spawn =
        |_path: PathBuf| async { Err(ProbeError::Spawn("No such file or directory".to_owned())) };
    fixture.run(spawn).await;
    assert_eq!(fixture.cursor().await, None, "Spawn stops the page");
    let transient = |path: PathBuf| async move {
        Err(ProbeError::Transient {
            path: path.display().to_string(),
            reason: "ffprobe exceeded 30 s".to_owned(),
        })
    };
    fixture.run(transient).await;
    assert_eq!(fixture.cursor().await, None, "Transient stops the page");

    // Every read in a page refused, with more than one attempted, is a
    // statement about the reader, not about those files.
    let refuse_all = |path: PathBuf| async move { Err(refusal(&path)) };
    fixture.run_with_self_check(refuse_all, false).await;
    assert_eq!(
        fixture.cursor().await,
        None,
        "an all-refused page holds while ffprobe fails its self-check"
    );
    assert!(!fixture.done().await);

    // One refusal beside a successful read is a statement about one file.
    let first = fixture.file(first_id).await.path;
    let refuse_first = move |path: PathBuf| {
        let answer = if path == first {
            Err(refusal(&path))
        } else {
            Ok(None)
        };
        async move { answer }
    };
    fixture.run(&refuse_first).await;
    assert_eq!(fixture.cursor().await, Some(second_id.to_string()));
    fixture.run(&refuse_first).await;
    assert!(fixture.done().await);
}

#[tokio::test]
async fn frame_luminance_backfill_stops_on_a_store_error_and_survives_a_fenced_write() {
    let fixture = LuminanceFixture::new().await;
    fixture.document_walk_done().await;
    let (raced_id, raced) = fixture
        .row("raced.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    let (refused_id, _) = fixture
        .row("refused.mkv", Some("hdr10"), Some("none"), "smpte2084")
        .await;
    // A store write that fails stops the page where it is.
    rusqlite::Connection::open(&fixture.database)
        .expect("trigger connection")
        .execute_batch(
            "CREATE TRIGGER refuse_frame BEFORE UPDATE OF luminance_source ON files
                   WHEN NEW.luminance_source = 'frame'
                 BEGIN SELECT RAISE(ABORT, 'refused by the fixture'); END;",
        )
        .expect("refusing trigger");

    // The first row is rescanned between the listing and the write (its
    // probe document changes), so its write matches no row: fenced, not
    // an error, and the trigger never fires for it.
    let read_frame = {
        let store = Arc::clone(&fixture.store);
        let item = fixture.item;
        let raced = raced.clone();
        move |path: PathBuf| {
            let store = Arc::clone(&store);
            let raced = raced.clone();
            async move {
                if path == raced {
                    let (size, mtime) = scan::file_stat(&path).await.expect("stat");
                    store
                            .upsert_file(
                                item,
                                path.to_str().expect("utf8"),
                                size,
                                mtime,
                                &ProbeResult {
                                    hdr: Some("hdr10".to_owned()),
                                    luminance_source: Some("none".to_owned()),
                                    raw_json: Some(
                                        r#"{"streams":[{"codec_type":"video","color_transfer":"smpte2084"}],"rescanned":true}"#
                                            .to_owned(),
                                    ),
                                    ..Default::default()
                                },
                            )
                            .await
                            .expect("concurrent rescan");
                }
                Ok(Some(frame(1000)))
            }
        }
    };
    fixture.run(&read_frame).await;
    let raced_row = fixture.file(raced_id).await;
    assert_eq!(raced_row.luminance_source.as_deref(), Some("none"));
    assert_eq!(raced_row.max_cll, None, "the fenced write changed nothing");
    assert_eq!(
        fixture.file(refused_id).await.luminance_source.as_deref(),
        Some("none")
    );
    assert_eq!(
        fixture.cursor().await,
        Some(raced_id.to_string()),
        "the fenced row advances; the failed write holds the cursor before it"
    );

    rusqlite::Connection::open(&fixture.database)
        .expect("trigger connection")
        .execute_batch("DROP TRIGGER refuse_frame;")
        .expect("drop trigger");
    fixture.run(&read_frame).await;
    let refused_row = fixture.file(refused_id).await;
    assert_eq!(refused_row.luminance_source.as_deref(), Some("frame"));
    assert_eq!(refused_row.max_cll, Some(1000));
    assert_eq!(fixture.cursor().await, Some(refused_id.to_string()));
}

#[tokio::test]
async fn playback_preparation_is_durable_exact_and_independent_of_discovery() {
    use plurx_core::store::{ClusterFragmentIndexStore as _, UserStore as _};
    use plurx_core::transcode::CopyVideoOptions;
    let store = SqliteStore::open_in_memory().expect("store");
    store
        .put_setting(keys::VOD_INDEX_MINS, "0")
        .await
        .expect("disable discovery");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".to_owned(),
            kind: LibraryKind::Movies,
            paths: Vec::new(),
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "First play".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    // Deliberately absent: enqueueing must not hash or open the media.
    let id = store
        .upsert_file(
            item,
            "/absent/first-play.mkv",
            100,
            1,
            &ProbeResult {
                video_codec: Some("hevc".to_owned()),
                hdr: Some("dolby_vision".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read").expect("file");
    let strip = CopyVideoOptions::new(true, false);
    let convert = strip.with_dolby_vision_conversion(true);
    for principal in [
        plurx_core::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::new_v4(),
            &"a".repeat(64),
        )
        .expect("shared demand"),
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 0 },
        plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: -1 },
    ] {
        let viewer = PlaybackViewerDemand {
            principal,
            playback_id: "refused-demand".to_owned(),
        };
        assert!(enqueue_copy_preparation_for_object_with_viewer(
            &store,
            "node-a",
            &file,
            convert,
            None,
            Some(&viewer)
        )
        .await
        .is_err());
        assert!(store
            .analysis_requests(10)
            .await
            .expect("no anonymous queue write")
            .is_empty());
    }
    let first = enqueue_copy_preparation(&store, "node-a", &file, convert)
        .await
        .expect("first request");
    let retry = enqueue_copy_preparation(&store, "node-a", &file, convert)
        .await
        .expect("joined request");
    let other = enqueue_copy_preparation(&store, "node-a", &file, strip)
        .await
        .expect("different recipe");
    let bound =
        enqueue_copy_preparation_for_object(&store, "node-a", &file, convert, Some("object-a"))
            .await
            .expect("bound request");
    let repeated =
        enqueue_copy_preparation_for_object(&store, "node-a", &file, convert, Some("object-a"))
            .await
            .expect("joined bound request");
    let moved =
        enqueue_copy_preparation_for_object(&store, "node-a", &file, convert, Some("object-b"))
            .await
            .expect("new object request");
    assert_eq!(bound.request_id, repeated.request_id);
    assert_ne!(
        bound.request_id, moved.request_id,
        "same catalog size/mtime cannot hide an object change"
    );
    assert_eq!(first.request_id, retry.request_id);
    assert_ne!(first.request_id, other.request_id);
    assert_eq!(first.state, "queued");
    assert_eq!(
        first.video_identity,
        crate::fragindex::identity_for(&file, convert).argv_fingerprint
    );
    assert_eq!(first.target_node_id, "node-a");
    assert_eq!(first.priority, "normal");
    assert!(!first.force_rebuild);
    let viewer = store
        .create_user("first-play-viewer", "hash", false)
        .await
        .expect("viewer");
    let joined = enqueue_copy_preparation_for_object_with_viewer(
        &store,
        "node-a",
        &file,
        convert,
        None,
        Some(&PlaybackViewerDemand {
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: viewer.id,
            },
            playback_id: "first-play".into(),
        }),
    )
    .await
    .expect("viewer joins exact preparation");
    assert_eq!(joined.request_id, first.request_id);
    let promoted = store
        .analysis_request(&first.request_id)
        .await
        .expect("read viewer demand")
        .expect("request");
    assert_eq!(promoted.priority, "foreground");
    assert_eq!(promoted.trigger, "playback");
    assert_eq!(promoted.requested_generation, first.requested_generation);
    assert_eq!(
        store.analysis_requests(10).await.expect("requests").len(),
        4
    );
    assert_eq!(
        store
            .get_setting(keys::VOD_INDEX_MINS)
            .await
            .expect("setting")
            .as_deref(),
        Some("0")
    );
}

#[tokio::test(start_paused = true)]
async fn playback_preparation_wakes_busy_analysis_for_any_live_request_waiter() {
    use plurx_core::store::{
        BackgroundJobStore as _, ClusterFragmentIndexStore as _, UserStore as _,
    };
    use plurx_core::transcode::CopyVideoOptions;

    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "1")
        .await
        .expect("shared indexing on");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".to_owned(),
            kind: LibraryKind::Movies,
            paths: Vec::new(),
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Viewer demand".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let id = store
        .upsert_file(
            item,
            "/absent/viewer-demand.mkv",
            100,
            1,
            &ProbeResult {
                video_codec: Some("h264".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read file").expect("file");
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let _playback = transcode.test_mark_live_waiting();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let consumer = tokio::spawn(
        Arc::clone(&jobs).background_work_loop_with_ready(Arc::clone(&transcode), Some(ready_tx)),
    );
    // Measure the wake path from a subscribed worker.
    ready_rx.await.expect("analysis worker subscribed");
    let request = enqueue_copy_preparation(
        store.as_ref(),
        "test-node",
        &file,
        CopyVideoOptions::new(false, false),
    )
    .await
    .expect("enqueue");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        store
            .analysis_request(&request.request_id)
            .await
            .expect("read")
            .expect("row")
            .attempts,
        0,
        "busy playback must leave maintenance unclaimed"
    );
    let user = store
        .create_user("busy-source-viewer", "hash", false)
        .await
        .expect("viewer");
    enqueue_copy_preparation_for_object_with_viewer(
        store.as_ref(),
        "test-node",
        &file,
        CopyVideoOptions::new(false, false),
        None,
        Some(&PlaybackViewerDemand {
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: user.id,
            },
            playback_id: "busy-source-playback".into(),
        }),
    )
    .await
    .expect("join requesting viewer");
    assert!(
        jobs.analysis_source_may_continue(&transcode, Some(&request))
            .await,
        "any live request waiter permits the source read on a busy node"
    );
    assert!(
        !jobs.analysis_source_may_continue(&transcode, None).await,
        "ordinary maintenance still yields"
    );
    let admitted = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = store
                .analysis_request(&request.request_id)
                .await
                .expect("request read")
                .expect("request kept");
            if current.fence > 0 {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    consumer.abort();
    let _ = consumer.await;
    assert!(
        admitted.is_ok(),
        "a busy worker must admit a source read with any live request waiter: {:?}",
        store.analysis_request(&request.request_id).await
    );
    let interest = plurx_core::store::AnalysisViewerInterest {
        analysis_request_id: request.request_id.clone(),
        requested_generation: request.requested_generation.clone(),
        pipeline_version: request.pipeline_version.clone(),
        video_identity: request.video_identity.clone(),
        target_node_id: request.target_node_id.clone(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
            user_id: user.id,
        },
        playback_id: "busy-source-playback".into(),
        now_ms: clock_ms(),
    };
    store
        .cancel_waiter(plurx_core::store::background_jobs::CancelWaiter {
            scope: "playback-analysis".into(),
            request_id: interest.consumer_id(),
            now_ms: clock_ms(),
        })
        .await
        .expect("retire viewer");
    let lost = tokio_util::sync::CancellationToken::new();
    tokio::time::timeout(
        Duration::from_secs(1),
        jobs.wait_for_cluster_fragment_index_stop(
            &transcode,
            Some(&request),
            &lost,
            &AnalysisAttestationBudget::default(),
        ),
    )
    .await
    .expect("departed viewer stops source attestation");
    assert!(!lost.is_cancelled(), "viewer departure is not claim loss");
}

#[tokio::test(start_paused = true)]
async fn playback_preparation_wakes_idle_analysis_within_two_seconds() {
    use plurx_core::store::ClusterFragmentIndexStore as _;
    use plurx_core::transcode::CopyVideoOptions;

    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "1")
        .await
        .expect("shared indexing on");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".to_owned(),
            kind: LibraryKind::Movies,
            paths: Vec::new(),
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Viewer demand".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let id = store
        .upsert_file(
            item,
            "/absent/viewer-demand.mkv",
            100,
            1,
            &ProbeResult {
                video_codec: Some("h264".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read file").expect("file");
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let consumer =
        tokio::spawn(Arc::clone(&jobs).background_work_loop_with_ready(transcode, Some(ready_tx)));
    // Measure the wake path from an idle subscribed worker. CI can spend
    // wall time scheduling this test among thousands of concurrent tests.
    ready_rx.await.expect("analysis worker subscribed");
    let request = enqueue_copy_preparation(
        store.as_ref(),
        "test-node",
        &file,
        CopyVideoOptions::new(false, false),
    )
    .await
    .expect("enqueue");
    let admitted = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = store
                .analysis_request(&request.request_id)
                .await
                .expect("request read")
                .expect("request kept");
            if current.attempts > 0 {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    consumer.abort();
    let _ = consumer.await;
    assert!(
        admitted.is_ok(),
        "an idle worker must admit the exact playback request before the ordinary polling delay"
    );
}

fn lease_heartbeat(
    renew_every_ms: u64,
    metrics: &Arc<AnalysisRuntimeMetrics>,
    stop: &tokio_util::sync::CancellationToken,
    lost: &tokio_util::sync::CancellationToken,
) -> LeaseHeartbeat {
    LeaseHeartbeat {
        queue: "fragment-index",
        row: "cache-key:node-a".to_owned(),
        fence: 7,
        attempts: 1,
        known_expiry_ms: clock_ms() + 60_000,
        lease_ms: 60_000,
        renew_every: Duration::from_millis(renew_every_ms),
        metrics: Arc::clone(metrics),
        stop: stop.clone(),
        lost: lost.clone(),
    }
}

/// A store error is not a lost lease.
///
/// `STORE_TIMEOUT` is three seconds against a sixty-second lease. Treating
/// the first timeout as a loss throws away a build this node still holds
/// the right to finish — and, when the error is permanent rather than
/// transient, abandons every job the queue ever claims.
#[tokio::test(start_paused = true)]
async fn a_renewal_error_is_retried_rather_than_treated_as_a_lost_lease() {
    let attempts = Arc::new(AtomicU64::new(0));
    let metrics = Arc::new(AnalysisRuntimeMetrics::default());
    let stop = tokio_util::sync::CancellationToken::new();
    let lost = tokio_util::sync::CancellationToken::new();
    let counter = Arc::clone(&attempts);
    let renew = move |_now: i64, _expires: i64| {
        let counter = Arc::clone(&counter);
        async move {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                Err(StoreError::Database("store timeout".to_owned()))
            } else {
                Ok(true)
            }
        }
    };
    let heartbeat = tokio::spawn(lease_heartbeat(15_000, &metrics, &stop, &lost).run(renew));
    // Three ticks inside one sixty-second lease: two errors at 15 s and
    // 30 s, and the renewal that succeeds at 45 s.
    tokio::time::sleep(Duration::from_millis(50_000)).await;
    assert!(
        !lost.is_cancelled(),
        "two store errors inside one lease must not abandon the claim"
    );
    assert!(attempts.load(Ordering::SeqCst) >= 3);
    assert_eq!(metrics.lease_count(LeaseEvent::RenewFailed), 2);
    assert!(metrics.lease_count(LeaseEvent::Renewed) >= 1);
    assert_eq!(metrics.lease_count(LeaseEvent::Expired), 0);
    stop.cancel();
    heartbeat.await.expect("heartbeat retires");
}

/// The first tick belongs to the claim that just happened.
///
/// `tokio::time::interval` completes its first tick immediately. With a
/// renewal that always fails, the old loop cancelled `lost` microseconds
/// after the claim — before the worker had read the catalog row — and the
/// worker then returned without writing anything at all.
#[tokio::test(start_paused = true)]
async fn the_immediate_first_tick_does_not_renew() {
    let attempts = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&attempts);
    let stop = tokio_util::sync::CancellationToken::new();
    let lost = tokio_util::sync::CancellationToken::new();
    let metrics = Arc::new(AnalysisRuntimeMetrics::default());
    let heartbeat = tokio::spawn(lease_heartbeat(20_000, &metrics, &stop, &lost).run(
        move |_now, _expires| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(false)
            }
        },
    ));
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "the heartbeat must not renew before its first interval elapses"
    );
    assert!(!lost.is_cancelled());
    tokio::time::sleep(Duration::from_millis(25_000)).await;
    assert!(lost.is_cancelled(), "a row that is no longer ours is lost");
    heartbeat.await.expect("heartbeat retires");
    stop.cancel();
}

/// A renewal that never succeeds self-fences when the lease runs out — and
/// not one tick before.
#[tokio::test(start_paused = true)]
async fn a_permanently_failing_renewal_self_fences_at_the_deadline() {
    let metrics = Arc::new(AnalysisRuntimeMetrics::default());
    let stop = tokio_util::sync::CancellationToken::new();
    let lost = tokio_util::sync::CancellationToken::new();
    let heartbeat = tokio::spawn(
        lease_heartbeat(20_000, &metrics, &stop, &lost)
            .run(|_now, _expires| async { Err(StoreError::Database("refused".to_owned())) }),
    );
    tokio::time::sleep(Duration::from_millis(55_000)).await;
    assert!(
        !lost.is_cancelled(),
        "the lease is still held while renewals are merely failing"
    );
    tokio::time::sleep(Duration::from_millis(10_000)).await;
    assert!(
        lost.is_cancelled(),
        "the lease must self-fence once it has actually expired"
    );
    assert_eq!(metrics.lease_count(LeaseEvent::Expired), 1);
    assert_eq!(metrics.lease_count(LeaseEvent::Lost), 0);
    heartbeat.await.expect("heartbeat retires");
    stop.cancel();
}

/// Retirement must not be reported as a loss.
#[tokio::test(start_paused = true)]
async fn stopping_the_heartbeat_is_not_a_lost_lease() {
    let metrics = Arc::new(AnalysisRuntimeMetrics::default());
    let stop = tokio_util::sync::CancellationToken::new();
    let lost = tokio_util::sync::CancellationToken::new();
    let heartbeat = tokio::spawn(
        lease_heartbeat(20_000, &metrics, &stop, &lost).run(|_now, _expires| async { Ok(true) }),
    );
    tokio::time::sleep(Duration::from_millis(45_000)).await;
    stop.cancel();
    heartbeat.await.expect("heartbeat retires");
    assert!(!lost.is_cancelled());
    assert_eq!(metrics.lease_count(LeaseEvent::Lost), 0);
    assert_eq!(metrics.lease_count(LeaseEvent::Expired), 0);
    assert_eq!(metrics.lease_count(LeaseEvent::Renewed), 2);
}

/// Retirement is not a lost lease.
///
/// The worker settles the row and *then* the heartbeat's interrupted
/// renewal resolves `Ok(false)`, because the row is no longer `running`.
/// Reporting that as a loss is how the signal this milestone exists to
/// create becomes noise nobody reads.
#[tokio::test(start_paused = true)]
async fn a_renewal_that_loses_to_retirement_is_not_reported_as_a_loss() {
    let metrics = Arc::new(AnalysisRuntimeMetrics::default());
    let stop = tokio_util::sync::CancellationToken::new();
    let lost = tokio_util::sync::CancellationToken::new();
    let settling = stop.clone();
    let heartbeat = tokio::spawn(lease_heartbeat(20_000, &metrics, &stop, &lost).run(
        move |_now, _expires| {
            let settling = settling.clone();
            async move {
                // The outcome write lands while this renewal is in flight.
                settling.cancel();
                Ok(false)
            }
        },
    ));
    tokio::time::sleep(Duration::from_millis(25_000)).await;
    heartbeat.await.expect("heartbeat retires");
    assert!(
        !lost.is_cancelled(),
        "a row settled by this worker is not a lease it lost"
    );
    assert_eq!(metrics.lease_count(LeaseEvent::Lost), 0);
}

fn outcome_job() -> plurx_core::store::ClusterFragmentIndexJob {
    plurx_core::store::ClusterFragmentIndexJob {
        cache_key: "cache-key".to_owned(),
        file_id: 42,
        source_size: 10_000,
        source_mtime: 1,
        source_sha256: "a".repeat(64),
        pipeline_sha256: "b".repeat(64),
        priority: "normal".to_owned(),
        trigger: "background".to_owned(),
        target_node_id: "node-a".to_owned(),
        state: "running".to_owned(),
        owner_node_id: "node-a".to_owned(),
        fence: 7,
        lease_expires_ms: 1_000,
        attempts: 3,
        not_before_ms: 0,
        created_at_ms: 0,
        updated_at_ms: 0,
        last_error_code: String::new(),
        attempt_errors: String::new(),
        index_retry_deadline_ms: 0,
        index_diagnostic_json: String::new(),
    }
}

/// An outcome write that matched no row, or that the store refused, is
/// counted rather than discarded. Eighteen call sites used to write
/// `let _ = …`, so a queue failing every job it claimed said nothing.
#[test]
fn an_outcome_write_that_does_not_land_is_counted() {
    let metrics = AnalysisRuntimeMetrics::default();
    let job = outcome_job();
    assert!(record_analysis_outcome(
        &metrics,
        &job,
        "fail",
        "source_unavailable",
        Ok(true)
    ));
    assert_eq!(metrics.lease_count(LeaseEvent::OutcomeWriteLost), 0);

    assert!(!record_analysis_outcome(
        &metrics,
        &job,
        "fail",
        "source_unavailable",
        Ok(false)
    ));
    assert!(!record_analysis_outcome(
        &metrics,
        &job,
        "complete",
        "",
        Err(StoreError::Database("refused".to_owned()))
    ));
    assert_eq!(metrics.lease_count(LeaseEvent::OutcomeWriteLost), 2);
}

#[test]
fn the_lease_metric_family_names_every_event() {
    let metrics = AnalysisRuntimeMetrics::default();
    metrics.lease_event(LeaseEvent::Renewed);
    metrics.lease_event(LeaseEvent::OutcomeWriteLost);
    let rendered = metrics.prometheus("node-a");
    for event in LEASE_EVENTS {
        assert!(
            rendered.contains(&format!("plurx_analysis_lease_total{{event=\"{event}\"}}")),
            "the {event} series must be rendered even at zero"
        );
    }
    assert!(rendered.contains("plurx_analysis_lease_total{event=\"renewed\"} 1"));
    assert!(rendered.contains("plurx_analysis_lease_total{event=\"outcome_write_lost\"} 1"));
    assert!(rendered.contains("plurx_analysis_lease_total{event=\"lost\"} 0"));
}

#[test]
fn playback_contention_timeouts_do_not_exhaust_analysis_attempts() {
    let budget = AnalysisAttestationBudget::default();
    assert_eq!(
        budget.deadline_failure(false),
        AnalysisResolutionError::Retry {
            code: "source_attestation_timeout",
            charge_attempt: true,
        }
    );
    budget.observe_busy(true);
    budget.observe_busy(false);
    for _ in 0..10 {
        assert_eq!(
            budget.deadline_failure(false),
            AnalysisResolutionError::Retry {
                code: "source_attestation_timeout",
                charge_attempt: false,
            },
            "contention remains uncharged even after the viewer stops"
        );
    }
    assert_eq!(
        AnalysisAttestationBudget::default().deadline_failure(true),
        AnalysisResolutionError::Retry {
            code: "source_attestation_timeout",
            charge_attempt: false,
        },
        "a newly busy node at the deadline also stays uncharged"
    );
}

#[test]
fn a_timed_out_attestation_is_reported_as_a_timeout_not_a_refusal() {
    // The queue row's `last_error_code` is what an operator reads, and
    // these two send them to different places: a refusal is the source
    // changing, a timeout is a mount that stopped answering a 64 MiB read.
    assert_eq!(
        attestation_failure_code(&Err(AttestationFailure::TimedOut)),
        "source_attestation_timeout"
    );
    assert_eq!(
        attestation_failure_code(&Err(AttestationFailure::Refused(
            "source no longer matches the scanner's size/mtime identity".to_owned()
        ))),
        "source_attestation_failed"
    );

    // The helper is only worth anything if the call site uses it. Pin
    // that the cluster path passes the helper's result to
    // `fail_cluster_fragment_index` rather than a literal, and that both
    // arms of the failure exist to be distinguished.
    let source = include_str!("../state.rs");
    let handler = source
        .split_once("let attestation = tokio::select! {")
        .expect("the cluster-job attestation")
        .1
        .split_once("record_fragment_index_source")
        .expect("the end of the attestation block")
        .0;
    assert!(
        handler.contains("Some(Err(AttestationFailure::TimedOut))"),
        "the timeout arm must carry its own failure value"
    );
    assert!(
        handler.contains("let code = attestation_failure_code(&outcome);"),
        "the failure code must come from the helper this test covers"
    );
    assert!(
        !handler.contains("\"source_attestation_failed\""),
        "a literal code at the call site would make the helper decorative"
    );
}

#[test]
fn a_non_forced_generation_carries_the_attestation_regime() {
    // `enqueue_analysis_request` refuses a generation that already exists
    // in any state, so a library stranded on terminal rows can only be
    // re-requested by moving this fingerprint. Pin the token literally:
    // renaming it re-requests every file on the fleet, which is a
    // decision and not a refactor.
    assert_eq!(
        ANALYSIS_ATTESTATION_GENERATION,
        "source-attestation/sampled-v1"
    );
    let without = plurx_core::segplan::argv_fingerprint(&[
        "analysis-request".to_owned(),
        "41".to_owned(),
        "10000".to_owned(),
        "1700000000".to_owned(),
        "fragment_index".to_owned(),
        "pipeline-v3".to_owned(),
    ]);
    let with = plurx_core::segplan::argv_fingerprint(&[
        "analysis-request".to_owned(),
        "41".to_owned(),
        "10000".to_owned(),
        "1700000000".to_owned(),
        "fragment_index".to_owned(),
        "pipeline-v3".to_owned(),
        ANALYSIS_ATTESTATION_GENERATION.to_owned(),
    ]);
    assert_ne!(
        without, with,
        "the regime has to change the generation or nothing is reopened"
    );
    // And it is still deterministic, so a restart does not re-request the
    // library a second time.
    assert_eq!(
        with,
        plurx_core::segplan::argv_fingerprint(&[
            "analysis-request".to_owned(),
            "41".to_owned(),
            "10000".to_owned(),
            "1700000000".to_owned(),
            "fragment_index".to_owned(),
            "pipeline-v3".to_owned(),
            ANALYSIS_ATTESTATION_GENERATION.to_owned(),
        ])
    );
}

#[test]
fn transient_fragment_source_reads_are_retryable_not_stale() {
    assert!(matches!(
        classify_fragment_source_read(
            Err(StoreError::Database("temporary authority read".to_owned())),
            10,
            20,
        ),
        Err(FragmentSourceReadFailure::Transient)
    ));
    assert!(matches!(
        classify_fragment_source_read(Ok(None), 10, 20),
        Err(FragmentSourceReadFailure::Stale)
    ));
}
use plurx_core::store::{
    ClusterFragmentIndexStore, DvConversionMode, DvConversionStore, LibraryStore, MediaStore,
    PlaybackTelemetryStore, SettingsStore, SqliteStore,
};
use plurx_core::transcode::Pipeline;
use serde_json::json;
use std::sync::atomic::AtomicUsize;

fn metrics_sample(value: i64) -> PrometheusStoreSnapshot {
    PrometheusStoreSnapshot {
        libraries: value,
        users: value,
        offline: OfflinePackageStats {
            queued: value,
            preparing: value,
            ready: value,
            failed: value,
            queued_bytes: value,
            preparing_bytes: value,
            ready_bytes: value,
            failed_bytes: value,
            active_leases: value,
            pinned_bytes: value,
        },
        watched_outbox: (value, value, value),
        background_jobs: plurx_core::store::background_jobs::BackgroundJobMetrics {
            counts: [value; plurx_core::store::background_jobs::JOB_METRIC_SLOTS],
            oldest_age_seconds: [value; plurx_core::store::background_jobs::JOB_METRIC_SLOTS],
            source_io_reservations: value,
            legacy_pending: value,
        },
        analysis: plurx_core::store::AnalysisStoreMetrics {
            queue_depth: [value; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
            queue_oldest_age_seconds: [value; plurx_core::store::ANALYSIS_QUEUE_METRIC_SLOTS],
            lifecycle_counts: [value; plurx_core::store::ANALYSIS_LIFECYCLE_METRIC_SLOTS],
            marker_counts: [value; plurx_core::store::ANALYSIS_MARKER_METRIC_SLOTS],
            health: plurx_core::store::AnalysisQueueHealth {
                ready_24h: value,
                attempt_limit_24h: value,
                claimed_24h: value,
                claimable: value,
                running_past_lease: value,
                last_ready_at_ms: value,
            },
        },
    }
}

#[test]
fn store_metrics_cache_distinguishes_absent_stale_and_failed_samples() {
    let cache = StoreMetricsCache::default();
    assert_eq!(
        cache.snapshot_at(0),
        StoreMetricsView {
            sample: None,
            age_seconds: None,
            valid: false,
            errors: 0,
            queue_health: None,
        }
    );

    cache.record_error();
    assert_eq!(cache.snapshot_at(30).sample, None);
    assert_eq!(cache.snapshot_at(30).errors, 1);

    let complete = metrics_sample(7);
    cache.publish_at(complete, 40);
    assert_eq!(
        cache.snapshot_at(41),
        StoreMetricsView {
            sample: Some(complete),
            age_seconds: Some(1),
            valid: true,
            errors: 1,
            // Every figure is the same number here, so as many jobs
            // exhausted their retry budget as were claimed at all: over
            // budget by any reading, and `degraded` says so without
            // needing a single claim watched from this process.
            queue_health: Some(AnalysisQueueHealthReport {
                health: complete.analysis.health,
                claims_since_start: 0,
                lease_losses_since_start: 0,
                verdict: plurx_core::store::AnalysisQueueVerdict::Degraded,
            }),
        }
    );

    cache.record_error();
    let stale = cache.snapshot_at(40 + STORE_METRICS_FRESHNESS_SECS + 1);
    assert_eq!(stale.sample, Some(complete));
    assert_eq!(stale.age_seconds, Some(STORE_METRICS_FRESHNESS_SECS + 1));
    assert!(!stale.valid);
    assert_eq!(stale.errors, 2);
}

#[test]
fn store_metrics_cache_never_exposes_a_torn_complete_sample() {
    let cache = StoreMetricsCache::default();
    cache.publish_at(metrics_sample(1), 0);
    let writer = cache.clone();
    let handle = std::thread::spawn(move || {
        for ordinal in 0..10_000 {
            writer.publish_at(metrics_sample(if ordinal % 2 == 0 { 2 } else { 3 }), 0);
        }
    });
    for _ in 0..10_000 {
        let sample = cache
            .snapshot_at(0)
            .sample
            .expect("the initial complete sample remains present");
        let expected = sample.libraries;
        assert_eq!(sample, metrics_sample(expected));
    }
    handle.join().expect("join metrics publisher");
}

#[test]
fn re_pairing_provider_art_waits_for_its_replacement_cover() {
    assert!(
        curator_pairing_can_advance(None, false, "edition:new", false),
        "an initial optional or failed cover may retain embedded EPUB art"
    );
    assert!(
        curator_pairing_can_advance(Some("edition:old"), false, "edition:new", false),
        "EPUB-backed Curator facts may advance without provider art"
    );
    assert!(
        !curator_pairing_can_advance(Some("edition:old"), true, "edition:new", false),
        "fetch failure or publication-slot contention must retain the coherent old pair"
    );
    assert!(curator_pairing_can_advance(
        Some("edition:old"),
        true,
        "edition:new",
        true
    ));
    assert!(
        curator_pairing_can_advance(Some("edition:same"), true, "edition:same", false),
        "refreshing facts for the same edition may retain its provider cover"
    );
}

async fn drain_library(jobs: &Arc<JobManager>, library_id: i64) {
    let scratch = crate::test_tempdir().expect("library worker scratch");
    let transcode = TranscodeManager::new(
        Arc::clone(&jobs.store),
        scratch.path().to_path_buf(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    assert!(
        jobs.work_library_queue(&transcode, Some(library_id)).await,
        "durable worker claimed library"
    );
}

async fn completed_scan(
    jobs: &Arc<JobManager>,
    request: ScanRequest,
) -> Result<Option<TargetedScan>, TargetError> {
    let result = jobs.request_scan(request.clone()).await?;
    if result.is_some() {
        return Ok(result);
    }
    drain_library(jobs, request.library_id).await;
    jobs.request_scan(request).await
}

fn manager(store: Arc<dyn Store>, artwork: &std::path::Path) -> Arc<JobManager> {
    Arc::new(JobManager::new(store, artwork.to_path_buf()))
}

/// Co-resolve (shared-read §5.5): once one identity's request has
/// attested the source, every other identity of the file is on the queue
/// beside it — keyed as its own request would key it, at background
/// priority, for the same target — and a second resolution adds nothing.
#[tokio::test]
async fn resolving_one_identity_queues_every_sibling_beside_it() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let jobs = manager(store.clone(), artwork.path());
    let library = store
        .create_library(&NewLibrary {
            name: "co-resolve".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![artwork.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "co-resolve".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let probe = ProbeResult {
        video_codec: Some("hevc".to_owned()),
        duration_ms: Some(1_000),
        hdr: Some("dolby_vision".to_owned()),
        dolby_vision: DolbyVisionFacts {
            profile: Some(7),
            level: Some(6),
            bl_compat_id: Some(6),
            el_present: Some(true),
            rpu_present: Some(true),
        },
        ..Default::default()
    };
    let id = store
        .upsert_file(
            item,
            &artwork.path().join("film.mkv").to_string_lossy(),
            4_096,
            100,
            &probe,
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read").expect("file");
    let options = fragment_index_video_identity_options(store.as_ref(), &file, true, true)
        .await
        .expect("identities");
    assert_eq!(options.len(), 3, "strip, preserve, convert");
    let source_sha256 = "ab".repeat(32);
    let engine = "cd".repeat(32);
    let key = |video| {
        cluster_fragment_index_key(
            file.id,
            file.size,
            file.mtime,
            &source_sha256,
            &crate::fragment_index_cluster::pipeline_digest(&file, &engine, video),
        )
        .expect("key")
    };
    for _ in 0..2 {
        jobs.co_resolve_sibling_identities(
            &crate::fragment_index_cluster::pipeline_digest(&file, &engine, options[0].0),
            "node-a",
            &file,
            "node-a",
            &engine,
            true,
            true,
            "object-v1",
            &source_sha256,
            1_000,
        )
        .await;
    }
    assert!(
        store
            .cluster_fragment_index_job(&key(options[0].0), "node-a")
            .await
            .expect("read")
            .is_none(),
        "the requested identity is its own request's to submit"
    );
    for (video, _) in &options[1..] {
        let job = store
            .cluster_fragment_index_job(&key(*video), "node-a")
            .await
            .expect("read")
            .expect("a sibling is on the queue");
        assert_eq!(job.state, "queued");
        assert_eq!(
            (job.priority.as_str(), job.trigger.as_str()),
            ("normal", "background")
        );
        assert_eq!(job.source_sha256, source_sha256);
    }
}

async fn seed_subtitle_backfill(store: &SqliteStore, media: &Path, count: usize) -> Vec<i64> {
    let library = store
        .create_library(&NewLibrary {
            name: "Subtitle backfill".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![media.to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let mut ids = Vec::new();
    for n in 0..count {
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Subtitle movie {n}"),
                year: Some(2026),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let source = media.join(format!("movie-{n}.mkv"));
        std::fs::write(&source, b"source").expect("source");
        let id = store
            .upsert_file(
                item,
                source.to_str().expect("path"),
                6,
                1,
                &ProbeResult {
                    container: Some("mkv".to_owned()),
                    subtitle_streams: vec![plurx_core::domain::SubtitleStream {
                        index: 0,
                        codec: "subrip".to_owned(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        ids.push(id);
    }
    ids
}

async fn enable_subtitle_backfill(store: &SqliteStore) {
    for key in [
        keys::VOD_INDEX_CLUSTER_CACHE,
        keys::SUBTITLE_CLUSTER_SOURCES,
        keys::SUBTITLE_BACKFILL,
    ] {
        store.put_setting(key, "1").await.expect("setting");
    }
}

#[tokio::test]
async fn busy_workers_allow_one_foreground_self_claim_and_one_subtitle_producer() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(7, &[Sub::Srt]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("work");
    let library = store
        .create_library(&NewLibrary {
            name: "Foreground subtitles".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![fixture.dir.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Cold subtitle".to_owned(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let file_id = store
        .upsert_file(
            item,
            fixture.source.to_str().expect("source path"),
            fixture.file.size,
            fixture.file.mtime,
            &ProbeResult {
                container: Some("mkv".to_owned()),
                subtitle_streams: vec![plurx_core::domain::SubtitleStream {
                    index: 0,
                    codec: "subrip".to_owned(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .expect("catalog file");
    for key in [
        keys::VOD_INDEX_CLUSTER_CACHE,
        keys::SUBTITLE_CLUSTER_SOURCES,
    ] {
        store.put_setting(key, "1").await.expect("setting");
    }
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        work.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    store
        .put_setting(keys::SW_POOL_THREADS, "2")
        .await
        .expect("bounded CPU pool");
    jobs.share_subtitle_admissions(transcode.admissions());
    let background = transcode
        .admit_fragment()
        .await
        .expect("real background CPU permit");
    assert!(transcode.fragment_worker_idle(&background));
    let background_manager = Arc::clone(&transcode);
    let yielding = tokio::spawn(async move {
        tokio::time::timeout(crate::admission::QUEUE_WAIT, async {
            while background_manager.fragment_worker_idle(&background) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background observes registered foreground pressure");
        drop(background);
    });
    let (request, _) = jobs
        .request_file_analysis(file_id, false, "subtitle_source", "playback")
        .await
        .expect("one durable foreground request");
    assert_eq!(request.priority, "foreground");
    let (first, second) = tokio::join!(
        jobs.self_claim_subtitle_source(&request.request_id, work.path()),
        jobs.self_claim_subtitle_source(&request.request_id, work.path())
    );
    yielding.await.expect("cooperative background yield");
    assert!(
        !transcode.admissions().live_is_waiting(),
        "successful admission releases waiter before extracting"
    );
    assert_eq!((first as u8) + (second as u8), 1, "one owner runs");
    let rows = store.analysis_requests(20).await.expect("requests");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "ready");
    let publications = store
        .list_subtitle_source_publications(file_id, fixture.file.size, fixture.file.mtime)
        .await
        .expect("publications");
    assert!(publications
        .iter()
        .any(|row| { row.origin == "extracted" && row.ordinal == 0 && row.format == "webvtt" }));
    let directory =
        crate::subtitle_source::file_dir(&crate::subtitle_source::store_root(work.path()), file_id);
    assert!(directory
        .join(crate::subtitle_source::MANIFEST_NAME)
        .is_file());
    let finished = jobs
        .finished_analysis_rows
        .lock()
        .expect("finished progress");
    let progress = finished
        .iter()
        .find(|row| row.job_id == request.request_id)
        .expect("subtitle progress");
    assert_eq!((progress.text_tracks, progress.pgs_tracks), (1, 0));
}

#[tokio::test]
async fn unready_learner_cannot_claim_a_foreground_subtitle_source_request() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = tempfile::tempdir().expect("media");
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("work");
    let file_id = seed_subtitle_backfill(store.as_ref(), media.path(), 1).await[0];
    for key in [
        keys::VOD_INDEX_CLUSTER_CACHE,
        keys::SUBTITLE_CLUSTER_SOURCES,
    ] {
        store.put_setting(key, "1").await.expect("setting");
    }
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        store.clone(),
        artwork.path().to_owned(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "learner".to_owned(),
        MovableJobAuthority::learner(),
    ));
    let (request, _) = jobs
        .request_file_analysis(file_id, false, "subtitle_source", "playback")
        .await
        .expect("request");
    assert!(
        !jobs
            .self_claim_subtitle_source(&request.request_id, work.path())
            .await
    );
    let rows = store.analysis_requests(10).await.expect("requests");
    assert_eq!(rows[0].state, "queued");
}

#[tokio::test]
async fn full_subtitle_coverage_settles_ready_without_spawning_a_pass() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    use plurx_core::store::SubtitleSourcePublication;

    let fixture = fixture(10, &[Sub::Srt]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("work");
    let attestation =
        crate::fragment_index_cluster::attest_source("test-node", &file, None, &|_| {})
            .await
            .expect("sampled source digest")
            .observation
            .source_sha256;
    for key in [
        keys::VOD_INDEX_CLUSTER_CACHE,
        keys::SUBTITLE_CLUSTER_SOURCES,
    ] {
        store.put_setting(key, "1").await.expect("setting");
    }
    store
        .upsert_subtitle_source_publication(&SubtitleSourcePublication {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_attestation: attestation,
            node_id: "peer".to_owned(),
            ordinal: 0,
            kind: "text".to_owned(),
            format: "webvtt".to_owned(),
            verdict: "empty".to_owned(),
            attempts: 1,
            origin: "extracted".to_owned(),
            sha256: String::new(),
            bytes: 0,
            published_at_ms: clock_ms(),
        })
        .await
        .expect("coverage");
    let jobs = manager(store.clone(), artwork.path());
    let (request, _) = jobs
        .request_file_analysis(file.id, false, "subtitle_source", "playback")
        .await
        .expect("request");
    assert!(
        jobs.self_claim_subtitle_source(&request.request_id, work.path())
            .await
    );
    assert_eq!(
        store.analysis_requests(10).await.expect("requests")[0].state,
        "ready"
    );
    assert!(
        !crate::subtitle_source::store_root(work.path()).exists(),
        "the no-op never opens a stage or ffmpeg child"
    );
}

#[tokio::test]
async fn remove_original_complete_publisher_and_request_third_ordinal_recovers_through_repair() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(9, &[Sub::Srt, Sub::Srt, Sub::Srt]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("work");
    for key in [
        keys::VOD_INDEX_CLUSTER_CACHE,
        keys::SUBTITLE_CLUSTER_SOURCES,
    ] {
        store.put_setting(key, "1").await.expect("setting");
    }
    let jobs = manager(store.clone(), artwork.path());
    let (initial, _) = jobs
        .request_file_analysis(file.id, false, "subtitle_source", "playback")
        .await
        .expect("initial request");
    assert!(
        jobs.self_claim_subtitle_source(&initial.request_id, work.path())
            .await
    );
    let stamp = SubtitleSourceStamp {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        pipeline_version: subtitle_source_pipeline_version().await,
    };
    let before = store
        .list_subtitle_source_publications(file.id, file.size, file.mtime)
        .await
        .expect("original publications");
    assert!(before
        .iter()
        .any(|row| row.ordinal == 2 && row.origin == "extracted"));
    store
        .delete_subtitle_source_publications(file.id, jobs.coordinator.node_id())
        .await
        .expect("publisher removed");
    let dir =
        crate::subtitle_source::file_dir(&crate::subtitle_source::store_root(work.path()), file.id);
    std::fs::remove_dir_all(dir).expect("publisher files removed");
    assert!(store
        .retire_subtitle_source_ready(&stamp, clock_ms())
        .await
        .expect("retire lost publication"));
    let (repair, _) = jobs
        .request_file_analysis(file.id, false, "subtitle_source", "playback")
        .await
        .expect("repair request for missing third ordinal");
    assert_ne!(repair.request_id, initial.request_id);
    assert!(
        jobs.self_claim_subtitle_source(&repair.request_id, work.path())
            .await
    );
    let after = store
        .list_subtitle_source_publications(file.id, file.size, file.mtime)
        .await
        .expect("repaired publications");
    assert!(after
        .iter()
        .any(|row| row.ordinal == 2 && row.origin == "extracted"));
}

#[tokio::test]
async fn normal_subtitle_job_yields_uncharged_to_playback_but_foreground_is_not_preempted() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(8, &[Sub::Pgs]);
    let source = std::fs::File::open(&fixture.source).expect("source");
    let stamp = crate::fragment_index_cluster::source_stamp(&source.metadata().expect("metadata"));
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let work = tempfile::tempdir().expect("work");
    let transcode = TranscodeManager::new(
        store,
        work.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let _busy = transcode.test_mark_live_waiting();
    let lost = CancellationToken::new();
    for _ in 0..3 {
        let plan =
            crate::subtitle_ride_along::RideAlongPlan::standalone(work.path(), 8, stamp, vec![0])
                .expect("normal plan");
        assert!(matches!(
            run_subtitle_source_pass(plan, &source, work.path(), &lost, Some(&transcode), &|| {},)
                .await,
            Err(AnalysisResolutionError::Retry {
                code: "foreground_preempted",
                charge_attempt: false,
            })
        ));
    }
    let plan =
        crate::subtitle_ride_along::RideAlongPlan::standalone(work.path(), 8, stamp, vec![0])
            .expect("foreground plan");
    assert!(
        run_subtitle_source_pass(plan, &source, work.path(), &lost, None, &|| {})
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn subtitle_backfill_enqueues_nothing_when_worker_is_busy() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = tempfile::tempdir().expect("media");
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("work");
    seed_subtitle_backfill(store.as_ref(), media.path(), 1).await;
    enable_subtitle_backfill(store.as_ref()).await;
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        work.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let _busy = transcode.test_mark_live_waiting();
    jobs.discover_subtitle_sources(transcode).await;
    assert!(store
        .analysis_requests(20)
        .await
        .expect("requests")
        .is_empty());
}

#[tokio::test]
async fn subtitle_backfill_lease_is_exclusive_and_pass_enqueues_at_most_eight() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = tempfile::tempdir().expect("media");
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("work");
    let ids = seed_subtitle_backfill(store.as_ref(), media.path(), 10).await;
    enable_subtitle_backfill(store.as_ref()).await;
    let first = Arc::new(JobManager::new_with_scan_prune_percent(
        store.clone(),
        artwork.path().to_owned(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "first".to_owned(),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
    ));
    let second = Arc::new(JobManager::new_with_scan_prune_percent(
        store.clone(),
        artwork.path().to_owned(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "second".to_owned(),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
    ));
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        work.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let lease = first
        .acquire_job("media:subtitle-source:backfill".to_owned())
        .await
        .expect("lease")
        .expect("first holder");
    second
        .discover_subtitle_sources(Arc::clone(&transcode))
        .await;
    assert!(store
        .analysis_requests(20)
        .await
        .expect("requests")
        .is_empty());
    lease.release().await.expect("release");
    second.discover_subtitle_sources(transcode).await;
    let requests = store.analysis_requests(20).await.expect("requests");
    assert_eq!(requests.len(), 8, "one pass admits at most eight sources");
    let newest: std::collections::HashSet<_> = ids.into_iter().rev().take(8).collect();
    assert!(requests
        .iter()
        .all(|request| newest.contains(&request.file_id)));
    assert!(requests.iter().all(|request| {
        request.component == "subtitle_source"
            && request.priority == "normal"
            && request.trigger == "background"
            && !request.force_rebuild
            && request.target_node_id.is_empty()
    }));
}

#[test]
fn analysis_backoff_is_stable_exponential_jitter_with_a_cap() {
    let policy = AnalysisRetryPolicy::from_settings(&BTreeMap::new());
    assert_eq!(
        policy.lease_ms,
        plurx_core::store::DEFAULT_ANALYSIS_LEASE_SECS * 1_000
    );
    let first = policy.backoff_ms("request-a", 1);
    assert_eq!(first, policy.backoff_ms("request-a", 1));
    assert!((3_750..=6_250).contains(&first));
    let later = policy.backoff_ms("request-a", 6);
    assert!(later >= first);
    assert!(later <= policy.backoff_max_ms);
    assert_eq!(
        policy.backoff_ms("request-a", 60),
        policy.backoff_max_ms,
        "a large attempt cannot overflow or exceed the configured ceiling"
    );
}

#[test]
fn durable_progress_attachment_cannot_relabel_a_replacement_execution() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store, artwork.path());
    let old =
        jobs.start_analysis_progress(("cache", "node"), 1, "fragment_index", "verifying", 100, 0);
    let current = jobs.start_analysis_progress(
        ("cache", "node"),
        1,
        "fragment_index",
        "fragment_index",
        100,
        0,
    );
    let token = plurx_core::store::background_jobs::JobToken {
        job_id: uuid::Uuid::new_v4().to_string(),
        node_id: "node".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        fence: 2,
        revision: 1,
        lease_expires_ms: clock_ms() + 30_000,
    };
    old.attach_attempt(&token);
    assert!(jobs.analysis_progress_snapshot()[0]
        .durable_job_id
        .is_none());
    current.attach_attempt(&token);
    drop(old);
    let row = &jobs.analysis_progress_snapshot()[0];
    assert_eq!(row.durable_job_id.as_deref(), Some(token.job_id.as_str()));
    assert_eq!(row.durable_fence, Some(2));
    drop(current);
    assert!(jobs.analysis_progress_snapshot().is_empty());
}

#[test]
fn analysis_progress_registry_is_bounded_and_removes_completed_work() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store, artwork.path());
    let mut guards = Vec::new();
    for index in 0..(MAX_ANALYSIS_PROGRESS + 8) {
        guards.push(jobs.start_analysis_progress(
            (&format!("job-{index:03}"), ""),
            index as i64,
            "fragment_index",
            "probing",
            1_000,
            2_000,
        ));
    }
    assert_eq!(
        jobs.analysis_progress_snapshot().len(),
        MAX_ANALYSIS_PROGRESS
    );
    let newest = format!("job-{:03}", MAX_ANALYSIS_PROGRESS + 7);
    jobs.update_analysis_progress(&newest, "", "fragment_index", 500, 1_000, 12);
    let row = jobs
        .analysis_progress_snapshot()
        .into_iter()
        .find(|row| row.job_id == newest)
        .expect("newest progress retained");
    assert_eq!(row.stage, "fragment_index");
    assert_eq!(row.fragments_indexed, 12);
    drop(guards);
    assert!(jobs.analysis_progress_snapshot().is_empty());
    assert!(jobs
        .analysis_metrics
        .current_by_stage
        .iter()
        .all(|value| value.load(Ordering::Relaxed) == 0));

    let old = jobs.start_analysis_progress(("same-job", ""), 1, "skip_markers", "probing", 1, 1);
    let current =
        jobs.start_analysis_progress(("same-job", ""), 1, "skip_markers", "persisting", 1, 1);
    drop(old);
    assert_eq!(jobs.analysis_progress_snapshot()[0].stage, "persisting");
    drop(current);
    assert!(jobs.analysis_progress_snapshot().is_empty());
    assert!(jobs
        .analysis_metrics
        .current_by_stage
        .iter()
        .all(|value| value.load(Ordering::Relaxed) == 0));

    let generic =
        jobs.start_analysis_progress(("shared-cache", ""), 1, "fragment_index", "verifying", 1, 1);
    let targeted = jobs.start_analysis_progress(
        ("shared-cache", "node-a"),
        1,
        "fragment_index",
        "fragment_index",
        1,
        1,
    );
    let snapshot = jobs.analysis_progress_snapshot();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.iter().any(|row| row.stage == "verifying"));
    assert!(snapshot.iter().any(|row| row.stage == "fragment_index"));
    drop(generic);
    assert_eq!(jobs.analysis_progress_snapshot().len(), 1);
    assert_eq!(jobs.analysis_progress_snapshot()[0].stage, "fragment_index");
    drop(targeted);
    assert!(jobs.analysis_progress_snapshot().is_empty());
    assert!(jobs
        .analysis_metrics
        .current_by_stage
        .iter()
        .all(|value| value.load(Ordering::Relaxed) == 0));
}

/// LAB2 C-02: with TMDB unreachable the scan finished with zero problems,
/// because the failures lived only in the in-memory enrichment report and
/// the status a finished library shows is read back from the durable work
/// row. The outage is now one line in the scan's own problems, and the
/// titles stay queued for the next scan.
#[tokio::test]
async fn a_scan_with_tmdb_unreachable_reports_it_once_in_its_problems() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    for n in 1..=3 {
        std::fs::write(
            media.path().join(format!("Blocked Title {n} (200{n}).mkv")),
            b"video",
        )
        .expect("movie file");
    }
    let library = store
        .create_library(&NewLibrary {
            name: "Blocked".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    store
        .put_setting(keys::TMDB_API_KEY, "c02-fixture-key")
        .await
        .expect("key");
    // Nothing listens on port 1: every connect is refused.
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), "http://127.0.0.1:1");

    assert!(jobs.trigger_scan(library.id).await, "scan admitted");
    drain_library(&jobs, library.id).await;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while jobs
            .all_statuses()
            .await
            .get(&library.id)
            .is_none_or(|status| status.running)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scan finished");

    let status = jobs
        .all_statuses()
        .await
        .remove(&library.id)
        .expect("scan status");
    let report = status.last_scan.expect("durable scan report");
    assert_eq!(report.added, 3, "{report:?}");
    let outage: Vec<&String> = report
        .problems
        .iter()
        .filter(|line| line.contains("TMDB could not be reached"))
        .collect();
    assert_eq!(outage.len(), 1, "{:?}", report.problems);
    assert!(outage[0].contains("3 titles stay queued"), "{}", outage[0]);
    assert!(!outage[0].contains("c02-fixture-key"), "{}", outage[0]);
    assert!(report.errors >= 1);
    assert_eq!(
        store
            .items_needing_metadata(Some(library.id), false, None)
            .await
            .expect("retry candidates")
            .len(),
        3,
        "every title is left for the next scan"
    );
}

fn manager_with_tmdb(
    store: Arc<dyn Store>,
    artwork: &std::path::Path,
    base: &str,
) -> Arc<JobManager> {
    let mut manager = JobManager::new(store, artwork.to_path_buf());
    manager.tmdb_base = Some((base.to_owned(), base.to_owned()));
    Arc::new(manager)
}

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn three_cluster_ticks_run_one_provider_pass() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("cluster tick store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    seeded_episode_backlog(&store, 1).await;
    let hits = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let base = serve(blocking_season_tmdb(
        Arc::clone(&hits),
        Arc::clone(&entered),
        Arc::clone(&release),
    ))
    .await;
    let manager = |node: &str| {
        let store: Arc<dyn Store> = store.clone();
        let mut manager = JobManager::new_with_scan_prune_percent(
            store,
            artwork.path().to_path_buf(),
            plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
            node.to_owned(),
            Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
        );
        manager.tmdb_base = Some((base.clone(), base.clone()));
        Arc::new(manager)
    };
    let a = manager("tick-a");
    let b = manager("tick-b");
    let c = manager("tick-c");
    let transcode_store: Arc<dyn Store> = store.clone();
    let transcode = Arc::new(TranscodeManager::new(
        transcode_store,
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let (a_tick, b_tick, c_tick) = tokio::join!(
        a.run_due_jobs(&transcode),
        b.run_due_jobs(&transcode),
        c.run_due_jobs(&transcode),
    );
    a_tick.expect("tick a");
    b_tick.expect("tick b");
    c_tick.expect("tick c");
    tokio::time::timeout(PROVIDER_LIVENESS, entered.notified())
        .await
        .expect("winning provider pass started");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "three real scheduler ticks must dispatch one provider pass"
    );
    release.notify_waiters();
    tokio::time::timeout(PROVIDER_LIVENESS, async {
        while a.retrying_artwork.load(Ordering::Relaxed)
            || b.retrying_artwork.load(Ordering::Relaxed)
            || c.retrying_artwork.load(Ordering::Relaxed)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider owner released its lease");
}

#[tokio::test]
async fn aborted_job_owner_stops_heartbeat_and_allows_ttl_takeover() {
    let store: Arc<dyn Store> =
        Arc::new(SqliteStore::open_in_memory().expect("aborted owner store"));
    let first =
        StoreCoordinator::new(Arc::clone(&store), "aborted-owner").expect("first coordinator");
    let successor =
        StoreCoordinator::new(Arc::clone(&store), "successor").expect("successor coordinator");
    let lease = match first
        .acquire("test:aborted-owner", std::time::Duration::from_secs(1))
        .await
        .expect("first acquire")
    {
        LeaseClaim::Acquired(lease) => lease,
        held => panic!("first owner must acquire, got {held:?}"),
    };
    let active = ActiveJobLease::start_with_policy(
        first,
        lease,
        std::time::Duration::from_secs(1),
        std::time::Duration::from_millis(50),
    )
    .expect("valid test lease policy");
    let owner = tokio::spawn(async move {
        let active = active;
        std::future::pending::<()>().await;
        drop(active);
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    owner.abort();
    assert!(owner
        .await
        .expect_err("owner task was aborted")
        .is_cancelled());

    tokio::time::sleep(std::time::Duration::from_millis(1_150)).await;
    assert!(matches!(
        successor
            .acquire("test:aborted-owner", std::time::Duration::from_secs(1))
            .await
            .expect("takeover acquire"),
        LeaseClaim::Acquired(_)
    ));
}

/// Provider work runs in the lease owner's task, while renewal runs in the
/// independent task owned by `ActiveJobLease`. A provider future that does
/// not wake therefore cannot silently let another node take the scan.
#[tokio::test]
async fn a_hung_provider_does_not_stop_scan_lease_renewal() {
    let store: Arc<dyn Store> =
        Arc::new(SqliteStore::open_in_memory().expect("hung provider store"));
    let first = StoreCoordinator::new(Arc::clone(&store), "provider-owner").expect("coordinator");
    let successor =
        StoreCoordinator::new(Arc::clone(&store), "provider-successor").expect("successor");
    let lease = match first
        .acquire("scan:library:1", Duration::from_secs(1))
        .await
        .expect("first acquire")
    {
        LeaseClaim::Acquired(lease) => lease,
        held => panic!("first owner must acquire, got {held:?}"),
    };
    let active = ActiveJobLease::start_with_policy(
        first,
        lease,
        Duration::from_secs(1),
        Duration::from_millis(100),
    )
    .expect("valid test lease policy");

    tokio::time::sleep(Duration::from_millis(1_150)).await;
    assert!(matches!(
        successor
            .acquire("scan:library:1", Duration::from_secs(1))
            .await
            .expect("successor acquire"),
        LeaseClaim::Held { .. }
    ));

    active.release().await.expect("release renewed lease");
}

#[tokio::test]
async fn empty_pretranscode_queue_rate_limits_local_cache_maintenance() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("empty queue store"));
    store
        .put_setting(keys::CACHE_MAX_GB, "50")
        .await
        .expect("enable cache");
    let artwork = crate::test_tempdir().expect("artwork");
    let cache = crate::test_tempdir().expect("cache");
    let cache_root = cache.path().join("transcode");
    let recipe = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    let orphan = cache_root
        .join("dd")
        .join(format!("{recipe}-j00000000-0000-4000-8000-000000000303-f1"));
    tokio::fs::create_dir_all(&orphan)
        .await
        .expect("queue-shaped orphan");
    tokio::fs::write(orphan.join("index.m3u8"), b"unbound")
        .await
        .expect("orphan bytes");

    let shared: Arc<dyn Store> = store;
    let jobs = manager(Arc::clone(&shared), artwork.path());
    let transcode = Arc::new(
        TranscodeManager::new(
            shared,
            cache.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_decoders(vec!["h264".to_owned()])
        .with_cache(cache_root, "test-ffmpeg".to_owned(), "test-node".to_owned()),
    );

    Arc::clone(&jobs)
        .work_pretranscode_queue(transcode, crate::background_jobs::PreparationLanes::ALL)
        .await;
    assert!(
        !orphan.exists(),
        "a producer-enabled node did not maintain its local cache on an empty queue"
    );
}

// ---- D2 (main-merge defects 2026-10-04): preparation lanes ----------

async fn d2_lanes(settings: &[(&str, &str)]) -> crate::background_jobs::PreparationLanes {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    for (key, value) in settings {
        store.put_setting(key, value).await.expect("setting");
    }
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store, artwork.path());
    let lanes = jobs.preparation_lanes().await;
    jobs.executable_lanes(lanes).await
}

#[tokio::test]
async fn output_preparation_runs_with_schedule_off_when_enabled() {
    use plurx_core::store::background_jobs::JobKind;
    let lanes = d2_lanes(&[(keys::VOD_OUTPUT_PREPARATION, "copy")]).await;
    assert!(!lanes.speculative, "discovery stays at its default, never");
    assert_eq!(
        lanes.kinds(),
        vec![JobKind::CopyOutputPrepare],
        "viewer-demand preparation has an executor with the schedule off"
    );
    let both = d2_lanes(&[(keys::VOD_OUTPUT_PREPARATION, "copy_and_encoded")]).await;
    assert_eq!(
        both.kinds(),
        vec![JobKind::CopyOutputPrepare, JobKind::EncodedOutputPrepare]
    );
}

#[tokio::test]
async fn viewer_demand_kinds_never_claim_transcode_prepare_with_schedule_off() {
    use plurx_core::store::background_jobs::JobKind;
    let lanes = d2_lanes(&[(keys::VOD_OUTPUT_PREPARATION, "copy_and_encoded")]).await;
    assert!(
        !lanes.kinds().contains(&JobKind::TranscodePrepare),
        "opening the output lanes must not open the speculative one"
    );
    let claim = include_str!("../background_jobs.rs");
    let transcode_branch = claim
        .split("let JobPayload::TranscodePrepare {")
        .nth(1)
        .expect("the speculative branch");
    assert!(
        transcode_branch.contains("if !kinds.contains(&JobKind::TranscodePrepare)"),
        "the claim filter refuses speculative rows when their lane is closed"
    );
}

#[tokio::test]
async fn output_preparation_off_neither_enqueues_nor_claims() {
    use plurx_core::store::background_jobs::JobKind;
    let lanes = d2_lanes(&[(keys::JOB_CACHE_PRODUCE_MINS, "60")]).await;
    assert!(lanes.speculative, "the schedule is on");
    assert_eq!(
        lanes.kinds(),
        vec![JobKind::TranscodePrepare],
        "with the switch absent, no output kind is claimed even with the schedule on"
    );
    let off = crate::vodserve::OutputPreparation::parse(None);
    assert_eq!(off, crate::vodserve::OutputPreparation::Off);
    assert!(
        !off.admits(false) && !off.admits(true),
        "and a VOD start enqueues nothing"
    );
    let copy = crate::vodserve::OutputPreparation::parse(Some("copy"));
    assert!(copy.admits(false) && !copy.admits(true));
    assert_eq!(
        crate::vodserve::OutputPreparation::parse(Some("bogus")),
        off
    );
}

#[tokio::test]
async fn copy_and_encoded_preparation_publish_producing_now() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store, artwork.path());
    let source = artwork.path().join("Heat.mkv");
    std::fs::write(&source, b"heat").expect("source");
    let file = crate::subtitle_ride_along::synthetic_media_file(&source, 1_000).expect("file");
    jobs.publish_preparation(&file, None, "recent_demand", 0)
        .await;
    let now = jobs.producing_now().await.expect("published");
    let title = source.display().to_string();
    assert_eq!(
        (now.title.as_str(), now.reason.as_str(), now.index),
        (title.as_str(), "recent_demand", 1)
    );
    // Both branches publish, so the banner and its Stop cover them.
    let source = include_str!("../state.rs");
    // Formatting may split the call from its await; both production paths
    // must still publish through this owner.
    let call = concat!(
        "self.publish_preparation(",
        "&file, item.as_ref(), &reason, index)"
    );
    assert_eq!(source.matches(call).count(), 2);
}

#[tokio::test]
async fn cachekeep_sweep_runs_only_with_the_schedule_on() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("empty queue store"));
    store
        .put_setting(keys::CACHE_MAX_GB, "50")
        .await
        .expect("enable cache");
    let artwork = crate::test_tempdir().expect("artwork");
    let cache = crate::test_tempdir().expect("cache");
    let cache_root = cache.path().join("transcode");
    let recipe = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let orphan = cache_root
        .join("ee")
        .join(format!("{recipe}-j00000000-0000-4000-8000-000000000304-f1"));
    tokio::fs::create_dir_all(&orphan).await.expect("orphan");
    tokio::fs::write(orphan.join("index.m3u8"), b"unbound")
        .await
        .expect("orphan bytes");
    let shared: Arc<dyn Store> = store;
    let jobs = manager(Arc::clone(&shared), artwork.path());
    let transcode = Arc::new(
        TranscodeManager::new(
            shared,
            cache.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_decoders(vec!["h264".to_owned()])
        .with_cache(cache_root, "test-ffmpeg".to_owned(), "test-node".to_owned()),
    );
    Arc::clone(&jobs)
        .work_pretranscode_queue(
            Arc::clone(&transcode),
            crate::background_jobs::PreparationLanes {
                speculative: false,
                output: crate::vodserve::OutputPreparation::CopyAndEncoded,
            },
        )
        .await;
    assert!(
        orphan.exists(),
        "output preparation alone does not sweep the pre-transcode cache"
    );
    Arc::clone(&jobs)
        .work_pretranscode_queue(transcode, crate::background_jobs::PreparationLanes::ALL)
        .await;
    assert!(!orphan.exists(), "the speculative lane maintains its cache");
}

fn targeted_show_tmdb(
    show_hits: Arc<AtomicUsize>,
    season_one_hits: Arc<AtomicUsize>,
    season_one_poster: Option<&'static str>,
) -> axum::Router {
    use axum::routing::get;
    use axum::Json;

    axum::Router::new()
        .route(
            "/tv/42",
            get(move || {
                let hits = Arc::clone(&show_hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "id": 42,
                        "name": "Severance",
                        "first_air_date": "2022-02-18",
                        "poster_path": "/show.jpg",
                        "backdrop_path": "/backdrop.jpg"
                    }))
                }
            }),
        )
        .route(
            "/tv/42/season/1",
            get(move || {
                let hits = Arc::clone(&season_one_hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "poster_path": season_one_poster,
                        "episodes": [{
                            "episode_number": 1,
                            "name": "Good News About Hell",
                            "still_path": "/episode-1.jpg"
                        }]
                    }))
                }
            }),
        )
        .route(
            "/tv/42/season/2",
            get(|| async {
                Json(json!({
                    "poster_path": "/season-2.jpg",
                    "episodes": [{
                        "episode_number": 1,
                        "name": "Hello, Ms. Cobel",
                        "still_path": "/episode-2.jpg"
                    }]
                }))
            }),
        )
        // Image paths are served by the same base in this test.
        .fallback(get(|| async { vec![0_u8, 1, 2, 3] }))
}

fn empty_season_tmdb(season_hits: Arc<AtomicUsize>) -> axum::Router {
    use axum::routing::get;
    use axum::Json;

    axum::Router::new().route(
        "/tv/42/season/1",
        get(move || {
            let hits = Arc::clone(&season_hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Json(json!({
                    "poster_path": "/season-1.jpg",
                    "episodes": []
                }))
            }
        }),
    )
}

/// How long the blocking-provider tests wait for something that must
/// happen. Each wait is either liveness (the pass reached the provider,
/// the owner let go) or proves the other side was not held by a provider
/// that never answers until the test releases it, so a longer bound
/// proves the same thing. Two seconds failed on loaded CI runners, where
/// the whole suite took 900 s, while the same tests pass locally in 0.3 s.
const PROVIDER_LIVENESS: std::time::Duration = std::time::Duration::from_secs(30);

fn blocking_season_tmdb(
    season_hits: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
) -> axum::Router {
    use axum::routing::get;
    use axum::Json;

    axum::Router::new().route(
        "/tv/42/season/1",
        get(move || {
            let hits = Arc::clone(&season_hits);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                entered.notify_one();
                release.notified().await;
                Json(json!({
                    "poster_path": "/season-1.jpg",
                    "episodes": []
                }))
            }
        }),
    )
}

async fn seeded_enriched_show(store: &SqliteStore) -> (i64, i64, i64) {
    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    store
        .put_setting(keys::TMDB_API_KEY, "test-key")
        .await
        .expect("key");
    let show = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Severance".into(),
            year: Some(2022),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");
    let season = store
        .insert_item(&NewItem {
            library_id: lib.id,
            kind: ItemKind::Season,
            parent_id: Some(show),
            title: "Season 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: None,
        })
        .await
        .expect("season");
    store
        .apply_metadata(
            show,
            &MetadataPatch {
                tmdb_id: Some(42),
                poster_path: Some("existing-show.jpg".into()),
                backdrop_path: Some("existing-backdrop.jpg".into()),
                enriched: true,
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("enrich show");
    (lib.id, show, season)
}

async fn seeded_episode_backlog(store: &SqliteStore, count: usize) -> Vec<i64> {
    let (library_id, _, season) = seeded_enriched_show(store).await;
    store
        .apply_metadata(
            season,
            &MetadataPatch {
                poster_path: Some("existing-season.jpg".into()),
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("healthy season");
    let mut episodes = Vec::with_capacity(count);
    for number in 1..=count {
        episodes.push(
            store
                .insert_item(&NewItem {
                    library_id,
                    kind: ItemKind::Episode,
                    parent_id: Some(season),
                    title: format!("Episode {number}"),
                    year: None,
                    season_number: Some(1),
                    episode_number: Some(number as i32),
                })
                .await
                .expect("episode"),
        );
    }
    episodes
}

fn scan_request_fixture(id: impl Into<String>, library_id: i64) -> ScanRequest {
    ScanRequest {
        id: id.into(),
        library_id,
        path: PathBuf::from("/missing/target"),
        ids: None,
        book: None,
        correlation_id: Some("correlation".into()),
        source: Some("test".into()),
    }
}

#[tokio::test]
async fn job_lifecycle_helpers_publish_progress_and_clear_every_guard() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(Arc::clone(&store), artwork.path());

    assert!(
        !jobs.stop_producing(),
        "an idle producer has nothing to stop"
    );
    jobs.producing.store(true, Ordering::Relaxed);
    jobs.set_producing(Some(ProducingNow {
        title: "Heat".into(),
        reason: crate::produce::REASON_RECENT.to_owned(),
        index: 1,
        total: 2,
    }))
    .await;
    assert!(jobs.stop_producing());
    assert_eq!(jobs.producing_now().await.expect("activity").title, "Heat");
    drop(ProducingGuard(Arc::clone(&jobs)));
    assert!(!jobs.producing.load(Ordering::Relaxed));
    assert!(!jobs.stop_producing.load(Ordering::Relaxed));
    assert!(jobs.producing_now().await.is_none());

    jobs.backfilling_genres.store(true, Ordering::Relaxed);
    drop(GenreBackfillGuard(Arc::clone(&jobs)));
    assert!(!jobs.backfilling_genres.load(Ordering::Relaxed));
    jobs.retrying_artwork.store(true, Ordering::Relaxed);
    drop(ArtworkRetryGuard(Arc::clone(&jobs)));
    assert!(!jobs.retrying_artwork.load(Ordering::Relaxed));

    let progress = Arc::new(ScanProgress::default());
    progress.found.store(5, Ordering::Relaxed);
    progress.processed.store(3, Ordering::Relaxed);
    progress.changed.store(2, Ordering::Relaxed);
    jobs.statuses.lock().await.insert(
        7,
        ScanStatus {
            running: true,
            ..Default::default()
        },
    );
    jobs.live.lock().await.insert(7, Arc::clone(&progress));
    let status = jobs.all_statuses().await.remove(&7).expect("live status");
    let snapshot = status.progress.expect("sampled progress");
    assert_eq!(
        (snapshot.found, snapshot.processed, snapshot.changed),
        (5, 3, 2)
    );

    jobs.finish(7, ScanStatus::default()).await;
    let finished = jobs
        .all_statuses()
        .await
        .remove(&7)
        .expect("finished status");
    assert!(!finished.running);
    assert!(finished.finished_at.is_some());
    assert!(finished.progress.is_none());
    let failed = error_status("disk unavailable");
    assert_eq!(failed.error.as_deref(), Some("disk unavailable"));
    assert!(failed.finished_at.is_some());
}

#[tokio::test]
async fn job_settings_are_bounded_and_stamps_round_trip() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(Arc::clone(&store), artwork.path());

    assert_eq!(jobs.job_interval("missing").await, 0);
    assert_eq!(jobs.job_interval_or("missing", 17).await, 17);
    store
        .put_setting("job.test", " -3 ")
        .await
        .expect("negative");
    assert_eq!(jobs.job_interval("job.test").await, 0);
    store
        .put_setting("job.test", " 42 ")
        .await
        .expect("interval");
    assert_eq!(jobs.job_interval("job.test").await, 42);
    assert_eq!(jobs.job_stamp("job.test").await, Some(42));
    store
        .put_setting("job.test", "invalid")
        .await
        .expect("invalid");
    assert_eq!(jobs.job_stamp("job.test").await, None);

    let lease = jobs
        .acquire_job("test:stamp".to_owned())
        .await
        .expect("stamp lease")
        .expect("stamp owner");
    let publisher = lease.publisher(store.as_ref());
    jobs.stamp("job.stamped", &publisher).await;
    drop(publisher);
    let _ = lease.release().await;
    let stamped = jobs.job_stamp("job.stamped").await.expect("stamp");
    assert!((now() - stamped).abs() <= 1);
}

/// A job authority whose answer can be moved while the manager holding it
/// keeps running, which is the whole point: committed membership moves
/// under a live daemon and the gate has to follow it without a restart.
struct MovableJobAuthority(std::sync::atomic::AtomicBool);

impl MovableJobAuthority {
    fn learner() -> Arc<Self> {
        Arc::new(Self(std::sync::atomic::AtomicBool::new(false)))
    }

    fn promote(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl ClusterJobAuthority for MovableJobAuthority {
    async fn may_run_cluster_jobs(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Every cluster-wide singleton resource, named individually.
///
/// Spelled out rather than derived, so adding a sixth job without deciding
/// whether a node with no vote may run it fails here.
const CLUSTER_SINGLETON_RESOURCES: &[&str] = &[
    "backup:cluster",
    "provider:artwork",
    "provider:genres",
    "scan:library:1",
    "repair:probe",
    "candidate:pretranscode",
    "watched:outbox",
    "metadata-classification",
];

/// Leases that are singletons but not *cluster* singletons, named so this
/// enumeration is honest about what it does and does not cover.
///
/// `shared-cache-gc:{storage_id}` is taken directly on the store by
/// `shared_cache::gc_once_inner` rather than through `acquire_cluster_job`,
/// and that is deliberate: it is a per-shared-volume singleton owned by
/// whichever node has the volume mounted. Its work is the same work
/// whoever runs it, it is fenced against a concurrent successor, and it
/// has to keep happening on a node with no vote — a learner that mounts
/// the volume is exactly as responsible for it as a voter is.
///
/// Listing it here rather than leaving the gap unstated is the point: an
/// enumeration that silently omits a resource cannot fail.
const SHARED_STORAGE_SINGLETON_RESOURCES: &[&str] = &["shared-cache-gc:{storage_id}"];

/// A node with no vote acquires none of the cluster's singleton leases —
/// and the store it is talking to is perfectly writable, so the refusal is
/// the gate rather than an incidental failure.
#[tokio::test]
async fn a_node_without_a_vote_acquires_no_cluster_job() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let authority = MovableJobAuthority::learner();
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        Arc::clone(&store),
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "learner-node".to_owned(),
        authority.clone(),
    ));

    assert!(!jobs.may_run_cluster_jobs().await);
    for resource in CLUSTER_SINGLETON_RESOURCES {
        let claimed = jobs
            .acquire_job((*resource).to_owned())
            .await
            .expect("lease acquisition must not error");
        assert!(
            claimed.is_none(),
            "a node with no vote acquired the {resource} lease"
        );
    }

    // Nothing took the leases, so the store still hands every one of them
    // to a node that is allowed to ask.
    let voter = Arc::new(JobManager::new_with_scan_prune_percent(
        Arc::clone(&store),
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "voter-node".to_owned(),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
    ));
    for resource in CLUSTER_SINGLETON_RESOURCES {
        let claimed = voter
            .acquire_job((*resource).to_owned())
            .await
            .expect("lease acquisition")
            .unwrap_or_else(|| panic!("an eligible node must acquire {resource}"));
        let _ = claimed.release().await;
    }
}

/// The lease that bypasses the membership gate is named, not denied.
///
/// `acquire_cluster_job` claimed to be "the only gate the whole
/// singleton-job surface passes through". It was not: the shared-cache GC
/// takes `shared-cache-gc:{storage_id}` directly on the store. Nothing is
/// corrupted by that — the GC is fenced and is a per-shared-volume
/// singleton any mounting node may own, including a learner — but while
/// the claim stood, the enumeration beside it could not fail, because a
/// resource nobody had listed could not be missing.
#[test]
fn the_lease_that_bypasses_the_membership_gate_is_named_rather_than_denied() {
    // Built at runtime so this test's own source does not count as a call
    // site of what it is looking for.
    let needle = format!(".{}(", "acquire_lease");

    let gate = include_str!("../job_lease.rs");
    assert!(
        !gate.contains("the only gate the whole singleton-job surface passes through"),
        "the false claim must not come back"
    );
    for resource in SHARED_STORAGE_SINGLETON_RESOURCES {
        assert!(
            gate.contains(resource),
            "the gate's contract must name the exception {resource}"
        );
    }

    // And the exception is exactly one call site, still outside the gate.
    let shared_cache = include_str!("../shared_cache.rs");
    assert_eq!(
        shared_cache.matches(needle.as_str()).count(),
        SHARED_STORAGE_SINGLETON_RESOURCES.len(),
        "a second direct lease means the enumeration above is stale again"
    );
    assert!(shared_cache.contains("shared-cache-gc:"));
    assert!(
        !shared_cache.contains("acquire_cluster_job"),
        "routing it through the gate is a fine choice, but then it is no longer an exception \
             and both enumerations have to say so"
    );
}

/// The scheduler tick itself is gated, not only the leases it dispatches.
///
/// Defence in depth, and it was untested: deleting the gate left every
/// plurxd test green, because the individual leases refuse a learner
/// anyway. The tick still matters — it is what keeps a node with no vote
/// from doing the scheduler's reads and writing its log lines every minute
/// — and the answer has to move the moment committed membership does,
/// without a restart.
#[tokio::test]
async fn a_node_without_a_vote_dispatches_no_scheduler_tick() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let work = tempfile::tempdir().expect("transcode work");
    let authority = MovableJobAuthority::learner();
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        Arc::clone(&store),
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "learner-node".to_owned(),
        authority.clone(),
    ));
    let transcode = Arc::new(TranscodeManager::new(
        Arc::clone(&store),
        work.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    assert!(
        !jobs.schedule_tick(&transcode).await,
        "a node with no vote must not dispatch a scheduler tick"
    );

    authority.promote();
    assert!(
        jobs.schedule_tick(&transcode).await,
        "and the same manager, never restarted, dispatches once it has a vote"
    );
}

#[tokio::test]
async fn scheduler_converges_guardless_committed_scratch_after_commit_crash() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let library = store
        .create_library(&NewLibrary {
            name: "Committed cleanup".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Committed cleanup movie".into(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let source = media.path().join("movie.mkv");
    tokio::fs::write(&source, b"published profile eight")
        .await
        .expect("published source");
    let file_id = store
        .upsert_file(
            item,
            source.to_str().expect("utf8 source"),
            80_000,
            7,
            &ProbeResult {
                container: Some("mkv".to_owned()),
                dolby_vision: DolbyVisionFacts {
                    profile: Some(7),
                    bl_compat_id: Some(6),
                    el_present: Some(true),
                    rpu_present: Some(true),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("P7 file");
    assert!(store
        .set_library_dv_conversion_mode(library.id, DvConversionMode::Manual)
        .await
        .expect("enable manual conversion"));
    assert!(matches!(
        store.queue_dv_conversion(file_id, 1).await.expect("queue"),
        QueueDvConversionOutcome::Queued(_)
    ));
    assert!(store
        .mark_dv_conversion_running(file_id, 80_000)
        .await
        .expect("running"));
    assert!(store
        .mark_dv_conversion_verified(file_id, Some("fel"), 60_000)
        .await
        .expect("verified"));
    assert!(store
        .mark_dv_conversion_committed(
            file_id,
            Some(
                source
                    .with_extension("mkv.p7.orig")
                    .to_str()
                    .expect("retained path")
            ),
            60_000,
            2,
        )
        .await
        .expect("commit before simulated crash"));

    let scratch = media.path().join(format!(".movie.mkv.plurx-dv-{file_id}"));
    tokio::fs::create_dir(&scratch).await.expect("scratch root");
    let owner = json!({
        "version": 5,
        "file_id": file_id,
        "source_path": source.to_str().expect("utf8 source"),
        "source": {
            "content": { "size": 80_000, "sha256": "test-source" },
            "local": {
                "node_id": "test-node",
                "identity": {
                    "device": 1,
                    "inode": 1,
                    "size": 80_000,
                    "modified_seconds": 1,
                    "modified_nanoseconds": 0,
                    "changed_seconds": 1,
                    "changed_nanoseconds": 0
                }
            }
        },
        "replacement": null,
        "expected_bytes": 60_000,
        "keep_original": true,
        "rollback_reason": null,
        "rollback_restore": null,
        "discard_pending": false,
        "recovery_guard_id": null
    });
    tokio::fs::write(
        scratch.join(".plurx-dv-owner.json"),
        serde_json::to_vec(&owner).expect("owner JSON"),
    )
    .await
    .expect("owner marker");
    tokio::fs::write(scratch.join("bounded-residue"), b"residue")
        .await
        .expect("residual scratch");

    let store_handle: Arc<dyn Store> = store.clone();
    let jobs = Arc::new(JobManager::new(store_handle, artwork.path().to_path_buf()));
    assert!(!jobs.dv_disk_capabilities.available);

    let slot = jobs
        .acquire_job("media:dv-disk-convert:0".to_owned())
        .await
        .expect("slot claim")
        .expect("hold cleanup slot");
    Arc::clone(&jobs).work_dv_disk_queue().await;
    assert!(scratch.exists(), "cleanup must respect the shared I/O slot");
    slot.release().await.expect("release cleanup slot");

    let file_lease = jobs
        .acquire_job(format!("media:dv-disk-convert:file:{file_id}"))
        .await
        .expect("file claim")
        .expect("hold cleanup file lease");
    Arc::clone(&jobs).work_dv_disk_queue().await;
    assert!(scratch.exists(), "cleanup must respect the per-file lease");
    file_lease.release().await.expect("release file lease");

    Arc::clone(&jobs).work_dv_disk_queue().await;
    assert!(
        !scratch.exists(),
        "the next tool-independent bounded pass converges post-commit scratch"
    );
}

#[tokio::test]
async fn safe_rollback_hands_guarded_scratch_to_tool_independent_orphan_retirement() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let work = crate::test_tempdir().expect("transcode work");
    let media_root = media.path().join("library");
    tokio::fs::create_dir(&media_root)
        .await
        .expect("media root");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![media_root.clone()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Guarded movie".into(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let source = media_root.join("movie.mkv");
    tokio::fs::write(&source, vec![b'7'; 80_000])
        .await
        .expect("real Profile 7 source");
    let source_metadata = tokio::fs::metadata(&source).await.expect("source metadata");
    #[cfg(unix)]
    let source_mtime = {
        use std::os::unix::fs::MetadataExt;
        source_metadata.mtime()
    };
    #[cfg(not(unix))]
    let source_mtime = source_metadata
        .modified()
        .expect("source modified time")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("source modified after epoch")
        .as_secs() as i64;
    let file_id = store
        .upsert_file(
            item,
            source.to_str().expect("utf8 source"),
            source_metadata.len() as i64,
            source_mtime,
            &ProbeResult {
                container: Some("mkv".to_owned()),
                dolby_vision: DolbyVisionFacts {
                    profile: Some(7),
                    bl_compat_id: Some(6),
                    el_present: Some(true),
                    rpu_present: Some(true),
                    ..Default::default()
                },
                raw_json: Some("{}".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("P7 file");
    assert!(store
        .set_library_dv_conversion_mode(library.id, DvConversionMode::Manual)
        .await
        .expect("enable manual conversion"));
    assert!(matches!(
        store.queue_dv_conversion(file_id, 1).await.expect("queue"),
        QueueDvConversionOutcome::Queued(_)
    ));
    assert!(store
        .mark_dv_conversion_running(file_id, 80_000)
        .await
        .expect("running"));
    assert!(store
        .mark_dv_conversion_verified(file_id, Some("fel"), 60_000)
        .await
        .expect("verified"));
    let file = store
        .get_file(file_id)
        .await
        .expect("read file")
        .expect("stored file");
    let intent = crate::dv_disk::prepare_test_published_recovery_guard(&file)
        .await
        .expect("real owned guarded scratch");
    assert!(store
        .begin_dv_recovery_guard(file_id, &intent.guard_id, &intent.recovery_path, 2,)
        .await
        .expect("guard intent"));
    let store_handle: Arc<dyn Store> = store.clone();
    let jobs = Arc::new(JobManager::new(
        Arc::clone(&store_handle),
        artwork.path().to_path_buf(),
    ));
    assert!(!jobs.dv_disk_capabilities.available);
    jobs.finish_dv_safe_rollback(
        &PublicationStore::unfenced(store.as_ref()),
        &file,
        &CancellationToken::new(),
        "simulate safe rollback after guard publication",
    )
    .await
    .expect("handoff guarded rollback to orphan cleanup");
    assert!(
        std::path::Path::new(&intent.recovery_path).exists(),
        "safe rollback must preserve guard-owned scratch until the leased orphan lifecycle runs"
    );
    let failed = store
        .dv_conversion(file_id)
        .await
        .expect("read failed conversion")
        .expect("failed conversion row");
    assert_eq!(failed.state, DvConversionState::Failed);
    assert!(failed.recovery_guard.is_none());
    assert!(store
        .dv_recovery_guard_by_id(&intent.guard_id)
        .await
        .expect("read guard")
        .is_some());

    // Model a voter whose configured absolute path still exists but whose
    // media filesystem disappeared. The empty underlying directory has no
    // owned scratch and, critically, no source-parent witness.
    let mounted_media = media.path().join("library-mounted");
    tokio::fs::rename(&media_root, &mounted_media)
        .await
        .expect("hide real media mount");
    tokio::fs::create_dir(&media_root)
        .await
        .expect("empty underlying mount point");
    Arc::clone(&jobs).work_dv_disk_queue().await;
    let stranded = store
        .dv_recovery_guard_by_id(&intent.guard_id)
        .await
        .expect("guard after missing mount")
        .expect("guard row remains");
    assert_eq!(
        stranded.state,
        DvRecoveryGuardState::Intent,
        "an empty underlying mount point must not advance any guard phase"
    );
    assert!(
        tokio::fs::read_dir(&media_root)
            .await
            .expect("underlying mount contents")
            .next_entry()
            .await
            .expect("underlying mount entry")
            .is_none(),
        "cleanup must not attest the empty underlying filesystem"
    );
    tokio::fs::remove_dir(&media_root)
        .await
        .expect("remove empty underlying mount point");
    tokio::fs::rename(&mounted_media, &media_root)
        .await
        .expect("restore real media mount");

    let cleanup_loss = CancellationToken::new();
    crate::dv_disk::remove_orphan_recovery_guard(
        file_id,
        source.to_str().expect("utf8 source"),
        &intent.recovery_path,
        &intent.guard_id,
        jobs.coordinator.node_id(),
        &cleanup_loss,
    )
    .await
    .expect("remove real guarded inode");
    assert!(store
        .advance_dv_recovery_guard(
            &intent.guard_id,
            DvRecoveryGuardState::Intent,
            DvRecoveryGuardState::GuardRemoved,
            4,
        )
        .await
        .expect("record guard removal"));

    tokio::fs::rename(&media_root, &mounted_media)
        .await
        .expect("hide media before scratch cleanup");
    tokio::fs::create_dir(&media_root)
        .await
        .expect("second empty underlying mount point");
    Arc::clone(&jobs).work_dv_disk_queue().await;
    assert_eq!(
        store
            .dv_recovery_guard_by_id(&intent.guard_id)
            .await
            .expect("guard after second missing mount")
            .expect("guard row remains before scratch cleanup")
            .state,
        DvRecoveryGuardState::GuardRemoved,
        "missing-mounted witness must block the scratch-removed transition"
    );
    tokio::fs::remove_dir(&media_root)
        .await
        .expect("remove second empty underlying mount point");
    tokio::fs::rename(&mounted_media, &media_root)
        .await
        .expect("restore media for scratch cleanup");

    crate::dv_disk::remove_orphan_recovery_scratch(
        file_id,
        source.to_str().expect("utf8 source"),
        &intent.guard_id,
        &cleanup_loss,
    )
    .await
    .expect("remove real recovery scratch and persist tombstone");
    assert!(store
        .advance_dv_recovery_guard(
            &intent.guard_id,
            DvRecoveryGuardState::GuardRemoved,
            DvRecoveryGuardState::ScratchRemoved,
            5,
        )
        .await
        .expect("record scratch removal"));

    tokio::fs::rename(&media_root, &mounted_media)
        .await
        .expect("hide media before ledger retirement");
    tokio::fs::create_dir(&media_root)
        .await
        .expect("third empty underlying mount point");
    Arc::clone(&jobs).work_dv_disk_queue().await;
    assert_eq!(
        store
            .dv_recovery_guard_by_id(&intent.guard_id)
            .await
            .expect("guard after third missing mount")
            .expect("terminal guard row remains")
            .state,
        DvRecoveryGuardState::ScratchRemoved,
        "missing terminal tombstone must block ledger-row deletion"
    );
    tokio::fs::remove_dir(&media_root)
        .await
        .expect("remove third empty underlying mount point");
    tokio::fs::rename(&mounted_media, &media_root)
        .await
        .expect("restore media for ledger retirement");

    let transcode = Arc::new(TranscodeManager::new(
        store_handle,
        work.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    assert!(jobs.schedule_tick(&transcode).await);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .dv_recovery_guard_by_id(&intent.guard_id)
                .await
                .expect("poll guard")
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("tool-independent cleanup completes");
}

/// The live property. The same manager, never restarted, starts acquiring
/// leases the moment its committed role changes — which is what makes this
/// a membership check rather than a boot-time flag.
#[tokio::test]
async fn a_promoted_node_starts_acquiring_without_a_restart() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let authority = MovableJobAuthority::learner();
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        Arc::clone(&store),
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "promoted-node".to_owned(),
        authority.clone(),
    ));

    assert!(jobs
        .acquire_job("provider:artwork".to_owned())
        .await
        .expect("lease acquisition")
        .is_none());

    authority.promote();

    assert!(jobs.may_run_cluster_jobs().await);
    let lease = jobs
        .acquire_job("provider:artwork".to_owned())
        .await
        .expect("lease acquisition")
        .expect("a promoted node acquires the lease it was refused a moment ago");
    let _ = lease.release().await;
}

/// The scheduler's startup arm asks the same question, and asks it after
/// its settle delay rather than before, so the answer is the one that
/// holds at the moment work would actually begin.
#[tokio::test(start_paused = true)]
async fn the_startup_scan_does_not_run_without_a_vote() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = tempfile::tempdir().expect("media");
    let artwork = tempfile::tempdir().expect("artwork");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    store
        .put_setting(keys::JOB_SCAN_ON_STARTUP, "1")
        .await
        .expect("enable startup scan");
    let authority = MovableJobAuthority::learner();
    let store_handle: Arc<dyn Store> = store.clone();
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        store_handle,
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "learner-node".to_owned(),
        authority.clone(),
    ));

    let settle = |jobs: Arc<JobManager>| async move {
        let startup = tokio::spawn(async move { jobs.scan_on_startup().await });
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        startup.await.expect("startup task");
    };

    settle(Arc::clone(&jobs)).await;
    assert!(
        jobs.all_statuses().await.is_empty(),
        "a node with no vote must not start the boot scan"
    );

    authority.promote();
    settle(Arc::clone(&jobs)).await;
    assert!(
        jobs.all_statuses().await.contains_key(&library.id),
        "and must start it once it carries one"
    );
}

#[tokio::test]
async fn successful_zero_row_cleanup_does_not_repeat_or_advance_busy_discovery() {
    let dir = tempfile::tempdir().expect("cleanup fixture");
    let path = dir.path().join("cleanup.db");
    let store = Arc::new(SqliteStore::open(&path).expect("store"));
    let jobs = manager(store.clone(), dir.path());
    let lost = CancellationToken::new();
    jobs.prune_fragment_index_catalog(dir.path(), &lost).await;
    let stamp = store
        .get_setting(keys::JOB_LAST_FRAGMENT_INDEX_CLEANUP)
        .await
        .expect("success stamp")
        .expect("zero-row success consumes interval");
    // Insert an eligible row AFTER success. If busy minute wakes repeat
    // prune, this row disappears even though discovery made no progress.
    rusqlite::Connection::open(&path).expect("seed connection").execute_batch(
            "INSERT INTO cluster_fragment_index_jobs(cache_key,target_node_id,file_id,source_size,source_mtime,source_sha256,pipeline_sha256,state,not_before_ms,created_at_ms,updated_at_ms)
             VALUES ('due-later','node',999,1,1,'source','pipeline','ready',1,1,1)")
             .expect("seed eligible history");
    for _ in 0..15 {
        jobs.prune_fragment_index_catalog(dir.path(), &lost).await;
    }
    assert!(store
        .cluster_fragment_index_job("due-later", "node")
        .await
        .expect("history")
        .is_some());
    assert_eq!(
        store
            .get_setting(keys::JOB_LAST_FRAGMENT_INDEX_CLEANUP)
            .await
            .expect("unchanged stamp"),
        Some(stamp)
    );
    assert_eq!(
        store
            .get_setting(&jobs.local_job_key(keys::JOB_LAST_VOD_INDEX))
            .await
            .expect("discovery stamp"),
        None
    );
    assert_eq!(
        store
            .get_setting(&jobs.local_job_key(keys::JOB_VOD_INDEX_CURSOR))
            .await
            .expect("discovery cursor"),
        None
    );
    // Both cluster-wide discovery permits still share this stamp.
    let authority = MovableJobAuthority::learner();
    authority.promote();
    let other = Arc::new(JobManager::new_with_scan_prune_percent(
        store.clone(),
        dir.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "other-node".to_owned(),
        authority,
    ));
    other.prune_fragment_index_catalog(dir.path(), &lost).await;
    assert!(store
        .cluster_fragment_index_job("due-later", "node")
        .await
        .expect("shared interval")
        .is_some());
}

#[tokio::test]
async fn an_empty_vod_pass_does_not_delay_the_first_useful_index() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = tempfile::tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    Arc::clone(&jobs).build_fragment_indexes(transcode).await;

    let stamp = jobs.local_job_key(keys::JOB_LAST_VOD_INDEX);
    assert_eq!(
        store.get_setting(&stamp).await.expect("read VOD stamp"),
        None,
        "a boot tick before library creation must stay due for the first scan"
    );
    assert!(!jobs.indexing.load(Ordering::Relaxed));
}

/// Catalogue a ride-along fixture: a library, an item and a file row
/// whose size and mtime are the fixture's own, so freshness checks pass.
async fn catalogued_fixture(
    store: &Arc<SqliteStore>,
    fixture: &crate::subtitle_ride_along::testing::Fixture,
) -> MediaFile {
    let library = store
        .create_library(&NewLibrary {
            name: format!("Ride {}", fixture.file.id),
            kind: LibraryKind::Movies,
            paths: vec![fixture.dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Ride along".into(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let probe = ProbeResult {
        duration_ms: Some(16_000),
        container: Some("matroska".into()),
        video_codec: Some("h264".into()),
        width: Some(64),
        height: Some(64),
        ..Default::default()
    };
    let file_id = store
        .upsert_file(
            item,
            &fixture.source.to_string_lossy(),
            fixture.file.size,
            fixture.file.mtime,
            &probe,
        )
        .await
        .expect("file");
    store.get_file(file_id).await.expect("read").expect("file")
}

/// Review finding 7: the decision both index paths make before a pass's
/// PGS tracks reach the store. A pass whose held source moved publishes
/// nothing; a pass whose catalog row moved publishes nothing; a current
/// pass publishes.
#[tokio::test]
async fn a_riding_pass_publishes_only_while_its_source_and_its_row_are_current() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(0, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let jobs = manager(store.clone(), artwork.path());
    let cache = crate::test_tempdir().expect("cache");
    let root = crate::subtitle_source::store_root(cache.path());
    let gate = crate::subtitle_ride_along::RideAlongGate::for_test(root.clone());
    let video = plurx_core::transcode::CopyVideoOptions::new(false, false);
    let dir = crate::subtitle_source::file_dir(&root, file.id);
    let pass = || async {
        let source = std::fs::File::open(&fixture.source).expect("held source");
        crate::fragindex::build_from_attested_file_with_progress(
            &file,
            &source,
            "fixture-object-v1",
            video,
            cache.path(),
            std::time::Duration::from_secs(60),
            |_: &crate::fragindex::PassProgress| {},
            Some(&gate),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .ride_along
        .expect("the pass rode along")
    };

    assert!(
        jobs.settle_ride_along(&file, pass().await, false)
            .await
            .is_none(),
        "the held source moved"
    );
    assert!(!dir.exists());

    let rescanned = ProbeResult {
        duration_ms: Some(16_000),
        container: Some("matroska".into()),
        video_codec: Some("h264".into()),
        width: Some(64),
        height: Some(64),
        ..Default::default()
    };
    store
        .upsert_file(
            file.item_id,
            &fixture.source.to_string_lossy(),
            file.size + 1,
            file.mtime,
            &rescanned,
        )
        .await
        .expect("a rescan moves the row");
    assert!(
        jobs.settle_ride_along(&file, pass().await, true)
            .await
            .is_none(),
        "the catalog row moved"
    );
    assert!(!dir.exists());

    store
        .upsert_file(
            file.item_id,
            &fixture.source.to_string_lossy(),
            file.size,
            file.mtime,
            &rescanned,
        )
        .await
        .expect("and back");
    let manifest = jobs
        .settle_ride_along(&file, pass().await, true)
        .await
        .expect("a current pass publishes");
    assert_eq!(manifest.ordinals, vec![0]);
    assert!(dir.join(crate::subtitle_source::MANIFEST_NAME).is_file());
}

/// Review finding 7, end to end on the non-cluster path: with the gate
/// open (on this test's thread only), the background index pass over a
/// file with a PGS track builds its index and publishes the track.
#[tokio::test]
async fn the_non_cluster_index_pass_rides_and_publishes() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    use plurx_core::store::FragmentIndexStore as _;

    let fixture = fixture(0, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let _open = crate::subtitle_ride_along::force_gate_open_on_this_thread();

    Arc::clone(&jobs)
        .build_fragment_indexes(Arc::clone(&transcode))
        .await;

    assert!(
        store
            .holds_fragment_index_for_source(file.id, file.size, file.mtime)
            .await
            .expect("read"),
        "the index was built"
    );
    let dir = crate::subtitle_source::file_dir(
        &crate::subtitle_source::store_root(transcode.runtime_cache_dir()),
        file.id,
    );
    let manifest = crate::subtitle_source::read_manifest(&dir)
        .await
        .expect("the pass published its PGS track");
    assert_eq!(manifest.ordinals, vec![0]);
    assert_eq!(
        manifest.track(0).map(|track| track.verdict),
        Some(crate::subtitle_source::Verdict::Kept)
    );
    // #463 review findings 1 and 6: the local pass — the default one —
    // has a progress row like the queue's, and the real pass wrote its
    // ride-along onto it.
    let row = finished_row_of(&jobs, file.id);
    assert!(
        row.job_id.starts_with(&format!("local-index:{}:", file.id)),
        "{row:?}"
    );
    assert_eq!(row.component, "fragment_index");
    assert!(row.fragments_indexed > 0, "{row:?}");
    assert_eq!(
        row.pgs_tracks, 1,
        "the row names the track it kept: {row:?}"
    );
}

/// A local pass over one catalogued fixture, with what it needs.
async fn local_index_fixture() -> (
    crate::subtitle_ride_along::testing::Fixture,
    Arc<SqliteStore>,
    MediaFile,
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<JobManager>,
    Arc<TranscodeManager>,
) {
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    let fixture = fixture(0, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    (
        fixture,
        store,
        file,
        artwork,
        transcode_dir,
        jobs,
        transcode,
    )
}

/// Lab5 (2026-10-26): the local pass quit whenever the pool was not idle,
/// and a Dolby Vision title scan holds a background admission for minutes at
/// a time, back to back, so titles waited days for an index. The pass now
/// queues behind that work for the heavy lane and builds once it is free.
#[tokio::test]
async fn the_local_index_pass_queues_behind_background_work_instead_of_quitting() {
    use plurx_core::store::FragmentIndexStore as _;
    let (_fixture, store, file, _artwork, _transcode_dir, jobs, transcode) =
        local_index_fixture().await;
    // What a title scan holds while it runs: the heavy lane and its threads.
    let scan = transcode
        .admit_fragment()
        .await
        .expect("the scan's background admission");
    let pass = tokio::spawn(Arc::clone(&jobs).build_fragment_indexes(Arc::clone(&transcode)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !pass.is_finished(),
        "the pass waits its turn rather than giving up"
    );
    drop(scan);
    tokio::time::timeout(Duration::from_secs(60), pass)
        .await
        .expect("the pass finishes once the lane is free")
        .expect("the pass");
    assert!(
        store
            .holds_fragment_index_for_source(file.id, file.size, file.mtime)
            .await
            .expect("read"),
        "built after the scan released the lane"
    );
}

/// The queued pass still yields to a viewer: when its turn on the lane comes
/// while playback waits for capacity, it leaves the file due and the pool to
/// the viewer, and the next pass builds it.
#[tokio::test]
async fn a_queued_local_index_pass_yields_to_a_waiting_viewer() {
    use plurx_core::store::FragmentIndexStore as _;
    let (_fixture, store, file, _artwork, _transcode_dir, jobs, transcode) =
        local_index_fixture().await;
    let scan = transcode
        .admit_fragment()
        .await
        .expect("the scan's background admission");
    let pass = tokio::spawn(Arc::clone(&jobs).build_fragment_indexes(Arc::clone(&transcode)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!pass.is_finished(), "queued behind the scan");
    let viewer = transcode.test_mark_live_waiting();
    drop(scan);
    tokio::time::timeout(Duration::from_secs(10), pass)
        .await
        .expect("the pass yields promptly")
        .expect("the pass");
    let built = || async {
        store
            .holds_fragment_index_for_source(file.id, file.size, file.mtime)
            .await
            .expect("read")
    };
    assert!(!built().await, "the waiting viewer keeps the pool");
    drop(viewer);
    Arc::clone(&jobs)
        .build_fragment_indexes(Arc::clone(&transcode))
        .await;
    assert!(built().await, "the next pass builds the file it left due");
}

/// Review of #1299: the heavy lane has one permit and everything else
/// try-acquires it, so a pass that waited for the lane before every file won
/// every turn and held the lane for the whole pass. It waits once; after its
/// file it releases the lane, and a speech pass queued meanwhile goes next
/// while the pass stops instead of re-taking the lane.
#[tokio::test]
async fn a_local_index_pass_waits_for_the_heavy_lane_once_and_lets_a_queued_worker_go_next() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    use plurx_core::store::FragmentIndexStore as _;
    let first = fixture(0, &[Sub::Pgs]);
    let second = fixture(1, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let files = [
        catalogued_fixture(&store, &first).await,
        catalogued_fixture(&store, &second).await,
    ];
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let scan = transcode
        .admit_fragment()
        .await
        .expect("the scan's background admission");
    let pass = tokio::spawn(Arc::clone(&jobs).build_fragment_indexes(Arc::clone(&transcode)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let speech = tokio::spawn({
        let transcode = Arc::clone(&transcode);
        async move {
            transcode
                .admit_speech_queued(&tokio_util::sync::CancellationToken::new())
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(scan);
    tokio::time::timeout(Duration::from_secs(60), pass)
        .await
        .expect("the pass ends")
        .expect("the pass");
    let speech = tokio::time::timeout(Duration::from_secs(10), speech)
        .await
        .expect("the queued speech pass gets its turn")
        .expect("speech task")
        .expect("a speech admission");
    let mut built = 0;
    for file in &files {
        if store
            .holds_fragment_index_for_source(file.id, file.size, file.mtime)
            .await
            .expect("read")
        {
            built += 1;
        }
    }
    assert_eq!(
        built, 1,
        "one file under the queued turn, then the lane went on"
    );
    drop(speech);
}

/// Lab N4 (2026-10-11): a changed title waited for the round-robin
/// cursor to wrap the whole library, 4 files a pass. A file a scan just
/// added or changed is now examined before the backlog, under the same
/// per-pass bound, and does not move the cursor: the backlog resumes
/// where it stood.
#[tokio::test]
async fn a_fresh_file_is_indexed_before_the_round_robin_backlog() {
    use plurx_core::store::FragmentIndexStore as _;
    let media = crate::test_tempdir().expect("media");
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "0")
        .await
        .expect("local indexing");
    let library = store
        .create_library(&NewLibrary {
            name: "Backlog".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let mut files = Vec::new();
    for n in 0..INDEX_MAX_PER_PASS + 2 {
        let source = media.path().join(format!("film-{n}.mkv"));
        std::fs::copy(plurx_core::testfixtures::source("h264"), &source)
            .expect("copy H.264 fixture");
        let metadata = std::fs::metadata(&source).expect("stat");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("Film {n}"),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let probe = ProbeResult {
            video_codec: Some("h264".into()),
            duration_ms: Some(12_000),
            ..Default::default()
        };
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let id = store
            .upsert_file(
                item,
                &source.to_string_lossy(),
                metadata.len() as i64,
                mtime,
                &probe,
            )
            .await
            .expect("file");
        files.push(id);
    }
    let work = crate::test_tempdir().expect("work");
    let jobs = manager(store.clone(), work.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        work.path().join("transcode"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let indexed = async |id: i64| {
        let file = store.get_file(id).await.expect("read").expect("file");
        let identity = crate::fragindex::identity_for(
            &file,
            plurx_core::transcode::CopyVideoOptions::new(false, false),
        );
        store
            .fragment_index(id, &identity)
            .await
            .expect("read")
            .is_some()
    };
    let fresh = *files.last().expect("files");
    jobs.note_fresh_index_files(vec![fresh]);
    jobs.clone().build_fragment_indexes(transcode.clone()).await;
    assert!(indexed(fresh).await, "the fresh file is indexed first");
    for &id in &files[..INDEX_MAX_PER_PASS - 1] {
        assert!(indexed(id).await, "the backlog fills the rest of the pass");
    }
    let next = files[INDEX_MAX_PER_PASS - 1];
    assert!(!indexed(next).await, "the per-pass bound still holds");
    assert!(
        jobs.fresh_index_files.lock().expect("fresh").is_empty(),
        "an examined fresh file goes back to the backlog's order"
    );

    jobs.clone().build_fragment_indexes(transcode.clone()).await;
    assert!(
        indexed(next).await,
        "the backlog resumes where it stood, not after the fresh file"
    );
}

#[tokio::test]
async fn local_hevc_index_refreshes_object_proof_without_cluster_queue() {
    use plurx_core::store::FragmentIndexStore as _;
    let media = crate::test_tempdir().expect("media");
    let source = media.path().join("source.mp4");
    std::fs::copy(plurx_core::testfixtures::source("closed-gop"), &source)
        .expect("copy HEVC fixture");
    let metadata = std::fs::metadata(&source).expect("stat");
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "0")
        .await
        .expect("local indexing");
    let library = store
        .create_library(&NewLibrary {
            name: "HEVC proof refresh".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "HEVC".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let probe = ProbeResult {
        video_codec: Some("hevc".into()),
        duration_ms: Some(12000),
        ..Default::default()
    };
    let id = store
        .upsert_file(
            item,
            &source.to_string_lossy(),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read").expect("file");
    let identity = crate::fragindex::identity_for(
        &file,
        plurx_core::transcode::CopyVideoOptions::new(false, false),
    );
    let work = crate::test_tempdir().expect("work");
    let jobs = manager(store.clone(), work.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        work.path().join("transcode"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    jobs.clone().build_fragment_indexes(transcode.clone()).await;
    let mut index = store
        .fragment_index(id, &identity)
        .await
        .expect("read")
        .expect("initial index");
    let object = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .expect("object");
    let proof = index
        .promotion
        .hevc_configuration
        .as_mut()
        .expect("original proof");
    assert!(proof.current_on_node(&object, jobs.coordinator.node_id()));
    // Model a prior object's proof with the same catalog size/mtime.
    proof.source_object_version = "stale-same-size-and-mtime".into();
    store
        .put_fragment_index(id, &index)
        .await
        .expect("stale row");
    jobs.clone().build_fragment_indexes(transcode.clone()).await;
    let mut rebuilt = store
        .fragment_index(id, &identity)
        .await
        .expect("read")
        .expect("rebuilt index");
    assert!(rebuilt
        .promotion
        .hevc_configuration
        .as_ref()
        .expect("refreshed proof")
        .current_on_node(&object, jobs.coordinator.node_id()));
    // A completed refusal on this exact object must remain untouched.
    rebuilt
        .promotion
        .hevc_configuration
        .as_mut()
        .expect("proof")
        .refusal = Some("completed unsupported configuration".into());
    store
        .put_fragment_index(id, &rebuilt)
        .await
        .expect("refused row");
    jobs.clone().build_fragment_indexes(transcode.clone()).await;
    assert_eq!(
        store.fragment_index(id, &identity).await.expect("read"),
        Some(rebuilt)
    );
}

/// The row a finished pass over `file_id` left behind, as it stood when
/// the pass ended.
fn finished_row_of(jobs: &JobManager, file_id: i64) -> AnalysisProgress {
    jobs.finished_analysis_rows
        .lock()
        .expect("rows")
        .iter()
        .rev()
        .find(|row| row.file_id == file_id && row.component == "fragment_index")
        .cloned()
        .unwrap_or_else(|| panic!("no progress row for file {file_id}"))
}

/// #463 review finding 6: the queue worker's pass writes its ride-along
/// onto its progress row through the real wiring — request, resolve,
/// claim, build — not only through the row's own update call.
#[tokio::test]
async fn the_queue_worker_row_carries_its_ride_along() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};

    let fixture = fixture(0, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "1")
        .await
        .expect("the queue on");
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let _open = crate::subtitle_ride_along::force_gate_open_on_this_thread();
    jobs.request_file_analysis(file.id, false, "fragment_index", "admin")
        .await
        .expect("requested");
    for _ in 0..40 {
        Arc::clone(&jobs)
            .work_cluster_fragment_index_queue(Arc::clone(&transcode))
            .await;
        let done = jobs
            .finished_analysis_rows
            .lock()
            .expect("rows")
            .iter()
            .any(|row| row.file_id == file.id && row.pgs_tracks > 0);
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let row = finished_row_of(&jobs, file.id);
    assert!(!row.job_id.starts_with("local-index:"), "{row:?}");
    assert_eq!(
        row.pgs_tracks, 1,
        "the queue's row names the track: {row:?}"
    );
    assert!(row.fragments_indexed > 0, "{row:?}");
}

/// A learner membership lets execute fragment-index work.
struct ArtifactLearnerAuthority;

#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl ClusterJobAuthority for ArtifactLearnerAuthority {
    async fn may_run_cluster_jobs(&self) -> bool {
        false
    }

    async fn may_execute_job(&self, kind: plurx_core::store::background_jobs::JobKind) -> bool {
        kind.permits_artifact_execution()
    }
}

/// Lab3 (2026-10-26): resolving analysis requests was gated on the voters'
/// singleton authority, so a learner allowed to execute fragment-index work
/// never resolved the requests targeted at it -- and nothing else claims a
/// targeted request. They sat queued for as long as it was a learner.
#[tokio::test]
async fn a_learner_resolves_the_analysis_request_targeted_at_it() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    use plurx_core::store::ClusterFragmentIndexStore as _;

    let fixture = fixture(0, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "1")
        .await
        .expect("the queue on");
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        store.clone(),
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "learner-node".to_owned(),
        Arc::new(ArtifactLearnerAuthority),
    ));
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let (request, _) = jobs
        .request_file_analysis(file.id, false, "fragment_index", "admin")
        .await
        .expect("requested");
    assert_eq!(request.target_node_id, "learner-node");
    let mut state = request.state.clone();
    for _ in 0..40 {
        Arc::clone(&jobs)
            .work_cluster_fragment_index_queue(Arc::clone(&transcode))
            .await;
        state = store
            .analysis_request(&request.request_id)
            .await
            .expect("read")
            .expect("request")
            .state;
        if state != "queued" && state != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        matches!(state.as_str(), "submitted" | "ready"),
        "the learner resolved its own request: {state}"
    );
}

/// Lab3 (2026-10-26): background discovery had filled the analysis queue to
/// its active ceiling, and a viewer's copy preparation -- queued as ordinary
/// background work -- was refused with the rest. A viewer's request is
/// foreground work with its own headroom above that ceiling.
#[tokio::test]
async fn a_viewer_preparation_is_queued_when_background_work_fills_the_ceiling() {
    use plurx_core::store::{ClusterFragmentIndexStore as _, UserStore as _};
    let dir = crate::test_tempdir().expect("db dir");
    let path = dir.path().join("ceiling.db");
    let store = SqliteStore::open(&path).expect("store");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".to_owned(),
            kind: LibraryKind::Movies,
            paths: Vec::new(),
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Ceiling".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let id = store
        .upsert_file(
            item,
            "/absent/ceiling.mkv",
            100,
            1,
            &ProbeResult {
                video_codec: Some("h264".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("file");
    let file = store.get_file(id).await.expect("read").expect("file");
    rusqlite::Connection::open(&path)
        .expect("seed connection")
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {ceiling})
             INSERT INTO analysis_requests (request_id, file_id, source_size, source_mtime,
                 component, pipeline_version, video_identity, requested_generation, priority,
                 trigger, force_rebuild, target_node_id, state, not_before_ms, created_at_ms,
                 updated_at_ms)
             SELECT 'filler-' || i, {id}, 100, 1, 'fragment_index', 'filler', '',
                    'filler-' || i, 'normal', 'background', 0, 'node-filler', 'queued', 0, 0, 0
               FROM n",
            ceiling = plurx_core::store::MAX_ACTIVE_ANALYSIS_REQUESTS,
        ))
        .expect("fill the active ceiling");
    let video = plurx_core::transcode::CopyVideoOptions::new(false, false);
    assert!(
        enqueue_copy_preparation_for_object(&store, "node-a", &file, video, None)
            .await
            .is_err(),
        "background preparation still stops at the ceiling"
    );
    let viewer = store
        .create_user("ceiling-viewer", "hash", false)
        .await
        .expect("viewer");
    let request = enqueue_copy_preparation_for_object_with_viewer(
        &store,
        "node-a",
        &file,
        video,
        None,
        Some(&PlaybackViewerDemand {
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: viewer.id,
            },
            playback_id: "ceiling-play".into(),
        }),
    )
    .await
    .expect("a viewer's preparation is admitted above the ceiling");
    assert_eq!(request.state, "queued");
    assert_eq!(request.priority, "foreground");
    assert_eq!(request.trigger, "playback");

    // A viewer's subtitle source shares the headroom; discovery does not.
    let stamp = SubtitleSourceStamp {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        pipeline_version: "ceiling-subtitles".to_owned(),
    };
    assert!(
        !matches!(
            store
                .enqueue_or_promote_subtitle_source(&stamp, "normal", 10)
                .await,
            Ok(Some(_))
        ),
        "background subtitle discovery still stops at the ceiling"
    );
    let subtitle = store
        .enqueue_or_promote_subtitle_source(&stamp, "foreground", 11)
        .await
        .expect("a viewer's subtitle source is admitted above the ceiling")
        .expect("request");
    assert_eq!(subtitle.priority, "foreground");
}

/// #463 review finding 3: the switch turned off while a riding pass runs
/// stops that pass's tracks reaching the store, and is counted.
#[tokio::test]
async fn a_riding_pass_finished_after_the_switch_went_off_publishes_nothing() {
    use crate::subtitle_ride_along::testing::{fixture, Sub};
    use crate::subtitle_ride_along::{discarded, Discard};

    let fixture = fixture(0, &[Sub::Pgs]);
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file = catalogued_fixture(&store, &fixture).await;
    let artwork = tempfile::tempdir().expect("artwork");
    let jobs = manager(store.clone(), artwork.path());
    let cache = crate::test_tempdir().expect("cache");
    let root = crate::subtitle_source::store_root(cache.path());
    let gate = crate::subtitle_ride_along::RideAlongGate::for_test(root.clone());
    let source = std::fs::File::open(&fixture.source).expect("held source");
    let pending = crate::fragindex::build_from_attested_file_with_progress(
        &file,
        &source,
        "fixture-object-v1",
        plurx_core::transcode::CopyVideoOptions::new(false, false),
        cache.path(),
        std::time::Duration::from_secs(60),
        |_: &crate::fragindex::PassProgress| {},
        Some(&gate),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .ride_along
    .expect("the pass rode along");
    // Turned off after the gate let the pass ride.
    store
        .put_setting(keys::SUBTITLE_STORED_SOURCES, "0")
        .await
        .expect("switch off");
    let before = discarded(Discard::SwitchOff);
    assert!(jobs.settle_ride_along(&file, pending, true).await.is_none());
    assert!(discarded(Discard::SwitchOff) > before, "counted");
    assert!(
        crate::subtitle_source::read_manifest(&crate::subtitle_source::file_dir(&root, file.id))
            .await
            .is_none(),
        "nothing was published"
    );
}

/// PR 3: the analysis row of a pass that rides along says how many PGS
/// tracks it keeps and what it has written.
#[tokio::test]
async fn an_analysis_row_carries_its_ride_along() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let jobs = manager(store.clone(), artwork.path());
    let _guard = jobs.start_analysis_progress(
        ("job-ride", "node-a"),
        7,
        "fragment_index",
        "fragment_index",
        1_000,
        60_000,
    );
    let row = |jobs: &JobManager| {
        jobs.analysis_progress_snapshot()
            .into_iter()
            .find(|row| row.job_id == "job-ride")
            .expect("row")
    };
    assert_eq!(
        (row(&jobs).pgs_tracks, row(&jobs).pgs_bytes_written),
        (0, 0)
    );
    jobs.update_analysis_ride_along("job-ride", "node-a", 2, 18_866);
    assert_eq!(
        (row(&jobs).pgs_tracks, row(&jobs).pgs_bytes_written),
        (2, 18_866)
    );
    // A peer that predates the fields still parses, as not riding.
    let mut value = serde_json::to_value(row(&jobs)).expect("json");
    value.as_object_mut().expect("object").remove("pgs_tracks");
    value.as_object_mut().expect("object").remove("text_tracks");
    value
        .as_object_mut()
        .expect("object")
        .remove("pgs_bytes_written");
    let parsed: AnalysisProgress = serde_json::from_value(value).expect("old row");
    assert_eq!(
        (
            parsed.pgs_tracks,
            parsed.text_tracks,
            parsed.pgs_bytes_written
        ),
        (0, 0, 0)
    );
}

/// A file the indexer cannot index is asked once, not once per pass.
///
/// Before this, `Truncated` and `Unsupported` were a `tracing::warn!` and
/// nothing else: the cursor moved on, the next wrap of the library found
/// the same file with no index, and spent the whole-file read again — up
/// to thirty minutes of one node's disk, forever, for a title `vodserve`
/// would go on answering `vod_index_pending` for. Nothing durable said the
/// question had already been asked.
///
/// The fixture is a file whose probe row says HEVC over bytes that are not
/// video, which is the cheapest reachable `Unsupported`: `copyseg::supports`
/// lets it through on the codec, and the index pipe then produces no
/// fragments. Deleting the outcome check in the identity loop makes the
/// second pass record a second attempt, which is what this fails on.
#[tokio::test]
async fn a_file_that_cannot_be_indexed_is_asked_once_per_identity() {
    use plurx_core::domain::{ItemKind, NewItem, ProbeResult};
    use plurx_core::store::FragmentIndexStore as _;

    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = tempfile::tempdir().expect("transcode");
    let media = tempfile::tempdir().expect("media");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Unindexable".into(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = media.path().join("unindexable.mkv");
    std::fs::write(&path, b"not video, but the probe row says otherwise").expect("fixture");
    let probe = ProbeResult {
        duration_ms: Some(60_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        width: Some(1920),
        height: Some(1080),
        ..Default::default()
    };
    let file_id = store
        .upsert_file(item, &path.to_string_lossy(), 43, 1, &probe)
        .await
        .expect("file");
    let identity = {
        let file = store.get_file(file_id).await.expect("read").expect("file");
        let videos = fragment_index_video_identities(store.as_ref(), &file, false, false)
            .await
            .expect("identities");
        crate::fragindex::identity_for(&file, videos[0])
    };

    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    Arc::clone(&jobs)
        .build_fragment_indexes(Arc::clone(&transcode))
        .await;
    let first = store
        .fragment_index_outcome(file_id, &identity)
        .await
        .expect("read the outcome")
        .expect("the pass records why it could not index this file");
    assert_eq!(first.attempts, 1);
    assert!(
        first.is_terminal(clock_ms()),
        "an unsupported source is terminal, not a wait: {first:?}"
    );
    assert!(!first.reason.is_empty(), "the builder's own words are kept");

    // The pass that follows must not spend the read again.
    Arc::clone(&jobs)
        .build_fragment_indexes(Arc::clone(&transcode))
        .await;
    assert_eq!(
        store
            .fragment_index_outcome(file_id, &identity)
            .await
            .expect("reread the outcome")
            .expect("the outcome survives the next pass")
            .attempts,
        1,
        "the second pass re-attempted a file that had already answered"
    );

    // …until the file itself changes, at which point it is a new question
    // and the old answer stops matching. Nothing has to notice: the
    // identity simply differs.
    store
        .upsert_file(item, &path.to_string_lossy(), 44, 2, &probe)
        .await
        .expect("replace the file");
    let replaced = {
        let file = store.get_file(file_id).await.expect("read").expect("file");
        let videos = fragment_index_video_identities(store.as_ref(), &file, false, false)
            .await
            .expect("identities");
        crate::fragindex::identity_for(&file, videos[0])
    };
    assert_ne!(replaced, identity, "the fixture must change the identity");
    assert_eq!(
        store
            .fragment_index_outcome(file_id, &replaced)
            .await
            .expect("read the replacement's outcome"),
        None,
        "a replaced file does not inherit its predecessor's refusal"
    );
}

#[tokio::test]
async fn analysis_hash_stop_signal_observes_foreground_playback() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_setting(keys::VOD_INDEX_CLUSTER_CACHE, "1")
        .await
        .expect("enable analysis");
    let artwork = tempfile::tempdir().expect("artwork");
    let transcode_dir = tempfile::tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store,
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    let lost = tokio_util::sync::CancellationToken::new();
    let waiter = {
        let jobs = Arc::clone(&jobs);
        let transcode = Arc::clone(&transcode);
        let lost = lost.clone();
        tokio::spawn(async move {
            jobs.wait_for_cluster_fragment_index_stop(
                &transcode,
                None,
                &lost,
                &AnalysisAttestationBudget::default(),
            )
            .await;
            lost.is_cancelled()
        })
    };
    tokio::task::yield_now().await;
    let _waiting_viewer = transcode.test_mark_live_waiting();
    let claim_was_lost = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("foreground demand cancels the hash selector")
        .expect("wait task");
    assert!(
        !claim_was_lost,
        "foreground cancellation is a no-charge retry, not claim loss"
    );
}

/// Lab N4: demand, then fresh files, then the backlog from its cursor —
/// each file once, and only files a library still lists.
#[test]
fn index_demand_and_fresh_files_go_before_the_backlog() {
    let paths = [1, 9, 17, 120, 5910]
        .into_iter()
        .map(|file_id| (file_id, PathBuf::from(format!("/{file_id}.mkv"))))
        .collect();
    let ordered: Vec<(i64, bool)> =
        prioritised_index_paths(paths, Some(17), &[9, 404], &[1, 9, 5910])
            .into_iter()
            .map(|(file_id, _, backlog)| (file_id, backlog))
            .collect();
    assert_eq!(
        ordered,
        [
            (9, false),
            (1, false),
            (5910, false),
            (120, true),
            (17, true)
        ]
    );
}

#[test]
fn the_vod_index_cursor_wraps_past_failed_low_ids() {
    let paths = [1, 9, 17, 120, 5910]
        .into_iter()
        .map(|file_id| (file_id, PathBuf::from(format!("/{file_id}.mkv"))))
        .collect();
    let ordered: Vec<i64> = ordered_index_paths(paths, Some(17))
        .into_iter()
        .map(|(file_id, _)| file_id)
        .collect();
    assert_eq!(ordered, [120, 5910, 1, 9, 17]);
}

#[test]
fn long_remuxes_receive_a_complete_pass_budget() {
    assert_eq!(
        index_file_budget(None, 0, None),
        Duration::from_secs(INDEX_FILE_BUDGET_FLOOR_SECS)
    );
    let steel = index_file_budget(Some(7_095_005), 0, None);
    assert!(steel >= Duration::from_secs(15 * 60));
    assert!(steel <= Duration::from_secs(INDEX_FILE_BUDGET_CEILING_SECS));
}

fn budget_exceeded(
    elapsed_ms: u64,
    covered_ms: i64,
    expected_ms: i64,
) -> plurx_core::segplan::FragmentIndexOutcome {
    plurx_core::segplan::FragmentIndexOutcome {
        source: plurx_core::segplan::SourceIdentity::new(1, 1, "argv"),
        refusal: plurx_core::segplan::IndexRefusal::Truncated { rows: 900 },
        reason: "exceeded the 1511s index budget".to_owned(),
        attempts: 1,
        next_attempt_at_ms: 0,
        updated_at_ms: 0,
        typed_code: Some(plurx_core::content_analysis::IndexFailureCode::IndexBudgetExceeded),
        typed_retryable: Some(true),
        retry_deadline_ms: 0,
        policy_revision: 1,
        terminal_reason: None,
        diagnostic: Some(plurx_core::content_analysis::IndexDiagnostic {
            version: 1,
            code: "index_budget_exceeded".to_owned(),
            retryable: true,
            claim_fence: 0,
            attempt: 1,
            selected_stream: None,
            expectation_provenance: None,
            covered_ms: Some(covered_ms),
            expected_ms: Some(expected_ms),
            container_ms: None,
            fragment_count: None,
            output_bytes: None,
            elapsed_ms: Some(elapsed_ms),
            budget_ms: Some(1_511_000),
            exit_category: Some("signal: 9 (SIGKILL)".to_owned()),
            stderr_tail: Vec::new(),
            recorded_at_ms: 0,
        }),
    }
}

/// a lab5 remux (79 GB, 197 min): killed by a 1511 s budget at 64% of
/// the film. Its retry is budgeted from that measured rate, past the old
/// 30-minute ceiling, instead of repeating the same kill.
#[test]
fn a_budget_failure_budgets_the_retry_from_the_rate_it_measured() {
    let film_ms = 11_820_000;
    let first = index_file_budget(Some(film_ms), 0, None);
    assert_eq!(first, Duration::from_secs(1_508));
    let previous = budget_exceeded(1_511_000, film_ms * 64 / 100, film_ms);
    let retry = index_file_budget(Some(film_ms), 0, Some(&previous));
    // 1511 s for 64% is 2361 s for the film; a quarter more, plus headroom.
    assert!(retry >= Duration::from_secs(2_966), "{retry:?}");
    assert!(retry > Duration::from_secs(INDEX_FILE_BUDGET_CEILING_SECS));
    // Only a budget failure says anything about the rate.
    let mut other = previous.clone();
    other.typed_code = Some(plurx_core::content_analysis::IndexFailureCode::IndexSourceIo);
    assert_eq!(index_file_budget(Some(film_ms), 0, Some(&other)), first);
    // A measurement that covered nothing gives no rate.
    let nothing = budget_exceeded(1_511_000, 0, film_ms);
    assert_eq!(index_file_budget(Some(film_ms), 0, Some(&nothing)), first);
    // And it is bounded, however slow the read was.
    let crawl = budget_exceeded(1_511_000, 1_000, film_ms);
    assert_eq!(
        index_file_budget(Some(film_ms), 0, Some(&crawl)),
        Duration::from_secs(INDEX_MEASURED_BUDGET_CEILING_SECS)
    );
}

/// A first attempt on a large, high-bitrate remux is budgeted from its size
/// too: 79 GB at the slowest planned read rate, not 197 minutes at 8x.
#[test]
fn a_first_attempt_on_a_large_remux_is_budgeted_from_its_size() {
    let remux = index_file_budget(Some(11_820_000), 79_000_000_000, None);
    assert!(remux >= Duration::from_secs(3_000), "{remux:?}");
    // A small file keeps the duration budget.
    assert_eq!(
        index_file_budget(Some(11_820_000), 4_000_000_000, None),
        Duration::from_secs(1_508)
    );
}

#[tokio::test]
async fn targeted_request_failures_queue_every_waiter_and_stay_bounded() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let media = crate::test_tempdir().expect("media");
    let jobs = manager(Arc::clone(&store), artwork.path());
    assert!(jobs
        .request_scan(scan_request_fixture("missing", 404))
        .await
        .is_err());
    assert!(
        jobs.scan_request("missing").await.is_none(),
        "rejected work has no acceptance receipt"
    );
    let library = store
        .create_library(&NewLibrary {
            name: "Durable".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    for id in ["queued-1", "queued-2"] {
        assert!(jobs
            .request_scan(scan_request_fixture(id, library.id))
            .await
            .expect("durable admission")
            .is_none());
    }
    // Reconstruct the manager with no process-local request ring.
    let restarted = manager(store, artwork.path());
    for id in ["queued-1", "queued-2"] {
        assert_eq!(
            restarted
                .scan_request(id)
                .await
                .expect("survived restart")
                .status,
            "queued"
        );
    }
    drain_library(&restarted, library.id).await;
    for id in ["queued-1", "queued-2"] {
        let record = restarted.scan_request(id).await.expect("durable result");
        assert_eq!(record.status, "failed");
        assert!(record
            .error
            .expect("path error")
            .contains("not under any root"));
    }
}

#[tokio::test]
async fn same_path_waiters_all_finish_and_apply_their_own_ids() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let folder = media.path().join("Queued");
    std::fs::create_dir_all(&folder).expect("queued folder");
    std::fs::write(folder.join("clip.mp4"), b"not really video").expect("queued clip");
    let library = store
        .create_library(&NewLibrary {
            name: "Queued Home".to_owned(),
            kind: LibraryKind::Home,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let jobs = manager(store.clone(), artwork.path());
    jobs.statuses.lock().await.insert(
        library.id,
        ScanStatus {
            running: true,
            ..Default::default()
        },
    );

    let first = ScanRequest {
        id: "same-path-first".to_owned(),
        library_id: library.id,
        path: folder.clone(),
        ids: None,
        book: None,
        correlation_id: None,
        source: Some("first".to_owned()),
    };
    let second = ScanRequest {
        id: "same-path-second".to_owned(),
        ids: Some(IdHints {
            tmdb: Some(4242),
            ..Default::default()
        }),
        source: Some("second".to_owned()),
        ..first.clone()
    };
    assert!(jobs
        .request_scan(first)
        .await
        .expect("queue first")
        .is_none());
    assert!(jobs
        .request_scan(second)
        .await
        .expect("queue second")
        .is_none());
    jobs.statuses.lock().await.remove(&library.id);
    drain_library(&jobs, library.id).await;

    for request_id in ["same-path-first", "same-path-second"] {
        assert_eq!(
            jobs.scan_request(request_id)
                .await
                .expect("terminal waiter")
                .status,
            "done",
            "every same-path waiter must reach a terminal state"
        );
    }
    let (counts, _) = jobs.metrics().snapshot();
    assert_eq!(
        counts
            .into_iter()
            .find(|(trigger, _)| *trigger == "targeted")
            .map(|(_, count)| count),
        Some(1),
        "same-path waiters share one physical scan/enrichment pass"
    );
    let second = jobs
        .scan_request("same-path-second")
        .await
        .expect("second waiter record");
    let item_id = second
        .items
        .expect("second waiter placed items")
        .first()
        .expect("placed item")
        .item_id;
    assert_eq!(
        store
            .get_item(item_id)
            .await
            .expect("item lookup")
            .expect("placed item")
            .tmdb_id,
        Some(4242),
        "the later waiter's caller-specific ids must not be discarded"
    );
}

#[tokio::test]
async fn scan_identity_queued_target_keeps_full_scan_directory_owner() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let season_one = media.path().join("Signal House/Season 1");
    std::fs::create_dir_all(&season_one).expect("season one");
    std::fs::write(season_one.join("Signal House S01E01.mkv"), b"video").expect("episode one");
    let library = store
        .create_library(&NewLibrary {
            name: "Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    scan::scan_library(store.as_ref(), &library)
        .await
        .expect("full scan");
    let original = store
        .find_show(library.id, "Signal House", None)
        .await
        .expect("show lookup")
        .expect("show");
    store
        .update_item_fields(
            original.id,
            &ItemEdit {
                title: Some("Signal House (North)".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("provider-style rename");

    let season_two = media.path().join("Signal House/Season 2");
    std::fs::create_dir_all(&season_two).expect("season two");
    std::fs::write(season_two.join("Signal House S02E01.mkv"), b"video").expect("episode two");
    let jobs = manager(store.clone(), artwork.path());
    jobs.statuses.lock().await.insert(
        library.id,
        ScanStatus {
            running: true,
            ..Default::default()
        },
    );
    assert!(jobs
        .request_scan(ScanRequest {
            id: "scan-identity-queued".into(),
            library_id: library.id,
            path: season_two,
            ids: None,
            book: None,
            correlation_id: Some("scan-identity".into()),
            source: Some("fixture".into()),
        })
        .await
        .expect("queue request")
        .is_none());
    jobs.statuses.lock().await.remove(&library.id);
    drain_library(&jobs, library.id).await;

    let request = jobs
        .scan_request("scan-identity-queued")
        .await
        .expect("request record");
    assert_eq!(request.status, "done");
    let episode_id = request
        .items
        .expect("placed items")
        .first()
        .expect("placed episode")
        .item_id;
    let episode = store
        .get_item(episode_id)
        .await
        .expect("episode lookup")
        .expect("episode");
    let season = store
        .get_item(episode.parent_id.expect("season id"))
        .await
        .expect("season lookup")
        .expect("season");
    assert_eq!(season.parent_id, Some(original.id));
    assert_eq!(
        store
            .get_item_children(original.id)
            .await
            .expect("seasons")
            .len(),
        2,
        "the queued target must extend the full-scan show rather than split it"
    );
}

#[tokio::test]
async fn scan_identity_hint_routes_only_series_id_and_surfaces_conflict() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let library = store
        .create_library(&NewLibrary {
            name: "Hint Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("library");
    let show = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Show,
            parent_id: None,
            title: "Hint Show".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("show");
    let season = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Season,
            parent_id: Some(show),
            title: "Season 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: None,
        })
        .await
        .expect("season");
    let episode = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: "Episode 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: Some(1),
        })
        .await
        .expect("episode");
    let jobs = manager(store.clone(), artwork.path());
    let publisher = PublicationStore::unfenced(store.as_ref());
    let placed = vec![
        PlacedFile {
            item_id: episode,
            file_id: 1,
            path: "/fixture/one.mkv".into(),
        },
        PlacedFile {
            item_id: episode,
            file_id: 2,
            path: "/fixture/two.mkv".into(),
        },
    ];
    let request = |series_tmdb, tmdb| ScanRequest {
        id: "hint-routing".into(),
        library_id: library.id,
        path: PathBuf::from("/fixture"),
        ids: Some(IdHints {
            tmdb,
            series_tmdb,
            episodeish: true,
            ..Default::default()
        }),
        book: None,
        correlation_id: Some("hint-routing".into()),
        source: Some("fixture".into()),
    };

    assert!(jobs
        .apply_ids(&request(None, Some(9001)), &placed, &publisher)
        .await
        .is_empty());
    assert_eq!(
        store
            .get_item(show)
            .await
            .expect("show")
            .expect("show")
            .tmdb_id,
        None,
        "an episode TMDB id is never reinterpreted as a series id"
    );
    assert!(jobs
        .apply_ids(&request(Some(42), Some(9001)), &placed, &publisher)
        .await
        .is_empty());
    assert_eq!(
        store
            .get_item(show)
            .await
            .expect("show")
            .expect("show")
            .tmdb_id,
        Some(42)
    );
    let problems = jobs
        .apply_ids(&request(Some(43), None), &placed, &publisher)
        .await;
    assert_eq!(
        problems.len(),
        1,
        "duplicate files report one show conflict"
    );
    assert!(problems[0].contains("43") && problems[0].contains("42"));
    assert_eq!(
        store
            .get_item(show)
            .await
            .expect("show")
            .expect("show")
            .tmdb_id,
        Some(42),
        "the known provider id wins"
    );
}

#[tokio::test]
async fn scan_identity_hint_conflict_keeps_targeted_scan_playable_and_visible() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let season_one = media.path().join("Beacon Field/Season 1");
    std::fs::create_dir_all(&season_one).expect("season one");
    std::fs::write(season_one.join("Beacon Field S01E01.mkv"), b"video").expect("episode one");
    let library = store
        .create_library(&NewLibrary {
            name: "Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    scan::scan_library(store.as_ref(), &library)
        .await
        .expect("initial scan");
    let show = store
        .find_show(library.id, "Beacon Field", None)
        .await
        .expect("show lookup")
        .expect("show");
    store
        .apply_series_tmdb_hint(library.id, show.id, 42)
        .await
        .expect("seed show id");
    let season_two = media.path().join("Beacon Field/Season 2");
    std::fs::create_dir_all(&season_two).expect("season two");
    std::fs::write(season_two.join("Beacon Field S02E01.mkv"), b"video").expect("episode two");
    let jobs = manager(store.clone(), artwork.path());
    let scan = completed_scan(
        &jobs,
        ScanRequest {
            id: "scan-identity-hint-conflict".into(),
            library_id: library.id,
            path: season_two,
            ids: Some(IdHints {
                series_tmdb: Some(43),
                episodeish: true,
                ..Default::default()
            }),
            book: None,
            correlation_id: Some("hint-conflict".into()),
            source: Some("fixture".into()),
        },
    )
    .await
    .expect("targeted scan")
    .expect("ran immediately");

    assert_eq!(
        scan.items.len(),
        1,
        "the conflicting import remains playable"
    );
    assert!(scan.report.problems.iter().any(|problem| {
        problem.contains("43") && problem.contains("42") && problem.contains(&show.id.to_string())
    }));
    assert_eq!(
        store
            .get_item(show.id)
            .await
            .expect("show")
            .expect("show")
            .tmdb_id,
        Some(42)
    );
}

#[tokio::test]
async fn scan_identity_hint_coalesced_order_keeps_first_series_id() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let season = media.path().join("Ordered Signals/Season 1");
    std::fs::create_dir_all(&season).expect("season");
    std::fs::write(season.join("Ordered Signals S01E01.mkv"), b"video").expect("episode");
    let library = store
        .create_library(&NewLibrary {
            name: "Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let jobs = manager(store.clone(), artwork.path());
    jobs.statuses.lock().await.insert(
        library.id,
        ScanStatus {
            running: true,
            ..Default::default()
        },
    );
    for (id, series_tmdb) in [("ordered-first", 42), ("ordered-second", 43)] {
        assert!(jobs
            .request_scan(ScanRequest {
                id: id.into(),
                library_id: library.id,
                path: season.clone(),
                ids: Some(IdHints {
                    series_tmdb: Some(series_tmdb),
                    episodeish: true,
                    ..Default::default()
                }),
                book: None,
                correlation_id: Some(id.into()),
                source: Some("fixture".into()),
            })
            .await
            .expect("queue waiter")
            .is_none());
    }
    jobs.statuses.lock().await.remove(&library.id);
    drain_library(&jobs, library.id).await;

    let second = jobs
        .scan_request("ordered-second")
        .await
        .expect("second record");
    assert_eq!(second.status, "done");
    assert!(second
        .report
        .as_ref()
        .expect("scan report")
        .problems
        .iter()
        .any(|problem| problem.contains("43") && problem.contains("42")));
    let episode_id = second
        .items
        .expect("placed items")
        .first()
        .expect("episode")
        .item_id;
    let episode = store
        .get_item(episode_id)
        .await
        .expect("episode")
        .expect("episode");
    let season_item = store
        .get_item(episode.parent_id.expect("season"))
        .await
        .expect("season")
        .expect("season");
    let show = store
        .get_item(season_item.parent_id.expect("show"))
        .await
        .expect("show")
        .expect("show");
    assert_eq!(show.tmdb_id, Some(42));
}

#[tokio::test]
async fn targeted_waiter_queue_is_bounded_and_overflow_is_terminal() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let media = crate::test_tempdir().expect("media");
    let library = store
        .create_library(&NewLibrary {
            name: "Bounded".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let jobs = manager(store.clone(), artwork.path());
    for index in 0..MAX_PENDING_PER_LIBRARY {
        assert!(jobs
            .request_scan(scan_request_fixture(format!("bounded-{index}"), library.id))
            .await
            .expect("within bound")
            .is_none());
    }
    let error = jobs
        .request_scan(scan_request_fixture("bounded-overflow", library.id))
        .await
        .expect_err("overflow explicit");
    assert!(error.to_string().contains("QueueFull"));
    assert!(
        matches!(error, TargetError::Refused(_)),
        "a full queue is a refusal the caller retries, not a storage failure: {error}"
    );
    assert!(
        jobs.scan_request("bounded-overflow").await.is_none(),
        "rejection must not claim durable acceptance"
    );
    let rows = store
        .library_work_requests(
            plurx_core::store::background_jobs_library::LibraryWorkQuery {
                limit: MAX_PENDING_PER_LIBRARY,
                pending_only: true,
                ..Default::default()
            },
        )
        .await
        .expect("durable pending requests");
    assert_eq!(rows.len(), MAX_PENDING_PER_LIBRARY);
}

#[tokio::test]
async fn refresh_of_a_missing_library_finishes_with_an_error() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store, artwork.path());
    assert!(!jobs.trigger_refresh_as(999, ScanTrigger::Scheduled).await);
    assert!(
        jobs.scan_requests().await.is_empty(),
        "missing library was never accepted"
    );
}

#[tokio::test(start_paused = true)]
async fn startup_scan_is_opt_in_and_runs_each_library_after_settling() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let jobs = manager(store.clone(), artwork.path());

    jobs.scan_on_startup().await;
    assert!(
        jobs.all_statuses().await.is_empty(),
        "startup scan is opt in"
    );

    store
        .put_setting(keys::JOB_SCAN_ON_STARTUP, "1")
        .await
        .expect("enable startup scan");
    let startup = tokio::spawn({
        let jobs = Arc::clone(&jobs);
        async move { jobs.scan_on_startup().await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    startup.await.expect("startup task");

    tokio::time::resume();
    drain_library(&jobs, library.id).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if jobs
                .all_statuses()
                .await
                .get(&library.id)
                .is_some_and(|status| !status.running)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup scan finished");
    let status = jobs
        .all_statuses()
        .await
        .remove(&library.id)
        .expect("startup scan status");
    assert!(!status.running);
    assert!(status.last_scan.is_some());
    let (counts, _) = jobs.metrics().snapshot();
    assert_eq!(
        counts
            .into_iter()
            .find(|(trigger, _)| *trigger == "startup")
            .map(|(_, count)| count),
        Some(1)
    );
}

#[tokio::test]
async fn schedule_loop_dispatches_a_due_library_scan() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let transcode = crate::test_tempdir().expect("transcode");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    store
        .set_library_schedule(library.id, 1, 0)
        .await
        .expect("schedule scan");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    let scheduler = tokio::spawn(Arc::clone(&jobs).schedule_loop(Arc::clone(&transcode)));
    tokio::time::timeout(Duration::from_secs(5), async {
        while jobs.scan_requests().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduler durably admitted scan");
    assert!(jobs.work_library_queue(&transcode, Some(library.id)).await);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if jobs
                .all_statuses()
                .await
                .get(&library.id)
                .is_some_and(|status| !status.running)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduled scan finished");
    scheduler.abort();
    assert!(scheduler
        .await
        .expect_err("scheduler keeps ticking")
        .is_cancelled());

    let status = jobs
        .all_statuses()
        .await
        .remove(&library.id)
        .expect("scheduled scan status");
    assert!(status.last_scan.is_some());
    assert!(status.error.is_none());
    let (counts, _) = jobs.metrics().snapshot();
    assert_eq!(
        counts
            .into_iter()
            .find(|(trigger, _)| *trigger == "scheduled")
            .map(|(_, count)| count),
        Some(1)
    );
}

#[tokio::test]
async fn due_jobs_dispatches_a_scheduled_metadata_refresh() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let transcode = crate::test_tempdir().expect("transcode");
    let library = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    store
        .set_library_schedule(library.id, 0, 1)
        .await
        .expect("schedule refresh");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    jobs.run_due_jobs(&transcode).await.expect("scheduler tick");
    assert!(jobs.work_library_queue(&transcode, Some(library.id)).await);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if jobs
                .all_statuses()
                .await
                .get(&library.id)
                .is_some_and(|status| !status.running)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduled refresh finished");

    let refreshed = store
        .get_library(library.id)
        .await
        .expect("read library")
        .expect("library remains");
    assert!(refreshed.last_refresh_at.is_some());
    let status = jobs
        .all_statuses()
        .await
        .remove(&library.id)
        .expect("scheduled refresh status");
    assert!(status.last_scan.is_some());
    assert!(status.error.is_none());
    let (counts, _) = jobs.metrics().snapshot();
    assert_eq!(
        counts
            .into_iter()
            .find(|(trigger, _)| *trigger == "scheduled")
            .map(|(_, count)| count),
        Some(1)
    );
}

#[tokio::test]
async fn refreshing_missing_item_artwork_is_an_empty_success() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store, artwork.path());

    let outcome = jobs
        .refresh_item_artwork(i64::MAX)
        .await
        .expect("missing item is not a store failure");
    assert!(outcome.enrich.is_none());
    assert!(outcome.local_art.is_none());
}

/// The bug this whole change exists for, in one test.
///
/// Curator POSTs `/api/v1/scan` the moment an import finishes and waits for
/// the answer. Before this, the handler placed the row and stopped — no
/// enrichment, no artwork — and the item sat with a blank card until
/// somebody pressed Scan on the whole library. A home library is the
/// provider-free way to prove it: its enrichment adopts the picture
/// already sitting next to the file, so the assertion is about *whether
/// enrichment ran at all*, not about anyone's network.
#[tokio::test]
async fn a_targeted_scan_enriches_what_it_placed() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    std::fs::create_dir_all(media.path().join("Holiday")).expect("mkdir");
    std::fs::write(media.path().join("Holiday/clip.mp4"), b"not really video").expect("clip");
    std::fs::write(media.path().join("Holiday/clip-thumb.jpg"), b"jpeg-ish").expect("thumb");

    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let jobs = manager(store.clone(), artwork.path());
    let scan = completed_scan(
        &jobs,
        ScanRequest {
            id: "req-1".into(),
            library_id: lib.id,
            path: media.path().join("Holiday"),
            ids: None,
            book: None,
            correlation_id: None,
            source: Some("monarr".into()),
        },
    )
    .await
    .expect("scan ran")
    .expect("not queued");
    assert_eq!(scan.items.len(), 1, "the clip was placed");

    let clip = store
        .get_item(scan.items[0].item_id)
        .await
        .expect("get")
        .expect("item");
    assert!(
        clip.poster_path.is_some(),
        "a peer-ingested item must come out of the targeted scan with \
             artwork; before the fix this was None and stayed None"
    );

    // And the folder above it inherited that poster — which only happens
    // if the ancestors were enriched too, not just the placed row.
    let folder = store
        .get_item(clip.parent_id.expect("parent"))
        .await
        .expect("get")
        .expect("folder");
    assert_eq!(folder.kind, ItemKind::Folder);
    assert!(folder.poster_path.is_some());
}

/// A show can be old while the episode is brand new. The show row is the
/// gateway to TMDB's season endpoint, so treating its earlier enrichment
/// stamp as a reason to skip it strands the new season and episode with
/// blank cards. This is the shape seen in the live library: movies from
/// the same import window had posters, episodes under known shows did not.
#[tokio::test]
async fn a_targeted_scan_enriches_new_children_of_an_existing_show() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let media = crate::test_tempdir().expect("media");
    let artwork = crate::test_tempdir().expect("artwork");
    let show = media.path().join("Severance (2022)");
    let season_one = show.join("Season 01");
    std::fs::create_dir_all(&season_one).expect("mkdir s1");
    std::fs::write(season_one.join("Severance.S01E01.mkv"), b"video").expect("episode 1");

    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![media.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    store
        .put_setting(keys::TMDB_API_KEY, "test-key")
        .await
        .expect("key");

    let show_hits = Arc::new(AtomicUsize::new(0));
    let season_one_hits = Arc::new(AtomicUsize::new(0));
    let base = serve(targeted_show_tmdb(
        Arc::clone(&show_hits),
        Arc::clone(&season_one_hits),
        Some("/season-1.jpg"),
    ))
    .await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
    let ids = Some(IdHints {
        series_tmdb: Some(42),
        episodeish: true,
        ..Default::default()
    });

    let first = completed_scan(
        &jobs,
        ScanRequest {
            id: "s1".into(),
            library_id: lib.id,
            path: season_one,
            ids: ids.clone(),
            book: None,
            correlation_id: None,
            source: Some("monarr".into()),
        },
    )
    .await
    .expect("first scan")
    .expect("first ran");
    let first_episode = store
        .get_item(first.items[0].item_id)
        .await
        .expect("get first")
        .expect("first episode");
    assert!(first_episode.poster_path.is_some(), "initial episode art");
    let first_season = store
        .get_item(first_episode.parent_id.expect("first season"))
        .await
        .expect("get first season")
        .expect("first season");
    let first_show = store
        .get_item(first_season.parent_id.expect("first show"))
        .await
        .expect("get first show")
        .expect("first show");
    assert!(first_show.poster_path.is_some(), "initial show poster");
    assert!(first_show.backdrop_path.is_some(), "initial show backdrop");

    // The show is now metadata-stamped. A later notification for a new
    // season must still walk through it to hydrate the newly placed rows.
    let season_two = show.join("Season 02");
    std::fs::create_dir_all(&season_two).expect("mkdir s2");
    std::fs::write(season_two.join("Severance.S02E01.mkv"), b"video").expect("episode 2");
    let second = completed_scan(
        &jobs,
        ScanRequest {
            id: "s2".into(),
            library_id: lib.id,
            path: season_two,
            ids,
            book: None,
            correlation_id: None,
            source: Some("monarr".into()),
        },
    )
    .await
    .expect("second scan")
    .expect("second ran");
    let second_episode = store
        .get_item(second.items[0].item_id)
        .await
        .expect("get second")
        .expect("second episode");
    assert!(
        second_episode.poster_path.is_some(),
        "a new episode under an existing show must leave the notification path with artwork"
    );
    let second_season = store
        .get_item(second_episode.parent_id.expect("season"))
        .await
        .expect("get season")
        .expect("second season");
    assert!(second_season.poster_path.is_some(), "new season poster");
    assert_eq!(
        season_one_hits.load(Ordering::SeqCst),
        1,
        "the targeted retry must not re-download every existing season"
    );
    assert_eq!(
        show_hits.load(Ordering::SeqCst),
        1,
        "the second import routes through the known show id without re-fetching the show"
    );
}

/// `scan_path` hands back the rows that own the *files* — episodes, not
/// shows. Enriching those ids alone would enrich nothing, because a show
/// is what the provider is asked about.
#[tokio::test]
async fn enrich_targets_walks_up_to_the_show() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let lib = store
        .create_library(&NewLibrary {
            name: "Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("lib");
    let new = |kind: ItemKind, parent: Option<i64>, title: &str| {
        let store = store.clone();
        let item = NewItem {
            library_id: lib.id,
            kind,
            parent_id: parent,
            title: title.into(),
            year: None,
            season_number: None,
            episode_number: None,
        };
        async move { store.insert_item(&item).await.expect("insert") }
    };
    let show = new(ItemKind::Show, None, "Severance").await;
    let season = new(ItemKind::Season, Some(show), "S1").await;
    let episode = new(ItemKind::Episode, Some(season), "E1").await;

    let jobs = manager(store.clone(), artwork.path());
    let targets = jobs.enrich_targets(&[episode]).await;
    assert!(targets.contains(&show), "the identity lives on the show");
    assert!(targets.contains(&season));
    assert!(targets.contains(&episode));

    // Two episodes of the same show contribute the show once, not twice —
    // a season import must not enrich the show ten times over.
    let episode2 = new(ItemKind::Episode, Some(season), "E2").await;
    let targets = jobs.enrich_targets(&[episode, episode2]).await;
    assert_eq!(targets.iter().filter(|id| **id == show).count(), 1);

    // A full 16-step walk that lands on a row seen by a later start still
    // has not proved that row's parent was visited. The later start must
    // continue walking rather than treating output deduplication as a
    // traversal cutoff.
    let mut chain = vec![show];
    for depth in 1..=16 {
        let parent = *chain.last().expect("parent");
        chain.push(new(ItemKind::Folder, Some(parent), &format!("F{depth}")).await);
    }
    let deepest = *chain.last().expect("deepest");
    let first_ancestor = chain[1];
    let targets = jobs.enrich_targets(&[deepest, first_ancestor]).await;
    assert!(
        targets.contains(&show),
        "a later start must finish a depth-capped ancestor walk"
    );
}

/// The sweep with nothing to sweep must not invent work — and must not
/// need a provider key to say so.
#[tokio::test]
async fn the_artwork_sweep_is_a_no_op_on_a_healthy_library() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let jobs = manager(store.clone(), artwork.path());
    assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 0);
}

/// The daemon owns the bound, not merely the store query. An inherited
/// backlog larger than one batch advances by exactly that batch and leaves
/// the tail due for the next scheduler tick.
#[tokio::test]
async fn the_artwork_sweep_claims_exactly_one_bounded_batch() {
    const EXTRA: usize = 7;
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let episodes = seeded_episode_backlog(&store, ARTWORK_RETRY_BATCH as usize + EXTRA).await;
    let season_hits = Arc::new(AtomicUsize::new(0));
    let base = serve(empty_season_tmdb(Arc::clone(&season_hits))).await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);

    assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 0);
    let mut stamped = 0;
    for id in episodes {
        if store
            .get_item(id)
            .await
            .expect("get episode")
            .expect("episode")
            .artwork_attempted_at
            .is_some()
        {
            stamped += 1;
        }
    }
    assert_eq!(stamped, ARTWORK_RETRY_BATCH as usize);
    let due = store
        .items_missing_artwork(
            None,
            keys::ARTWORK_RETRY_BACKOFF_SECS,
            ARTWORK_RETRY_BATCH + EXTRA as i64,
        )
        .await
        .expect("remaining due");
    assert_eq!(
        due.len(),
        EXTRA,
        "the unclaimed tail remains immediately due"
    );
    assert_eq!(season_hits.load(Ordering::SeqCst), 1);
}

/// A zero backoff is an adversarial but supported setting for the store
/// query. Never-attempted rows must still all get one turn before an old
/// row is reclaimed from the head of a larger-than-batch backlog.
#[tokio::test]
async fn artwork_retry_fairness_drains_fresh_rows_before_reclaiming_any() {
    const EXTRA: usize = 7;
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let episodes = seeded_episode_backlog(&store, ARTWORK_RETRY_BATCH as usize + EXTRA).await;
    let season_hits = Arc::new(AtomicUsize::new(0));
    let base = serve(empty_season_tmdb(Arc::clone(&season_hits))).await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);

    let first = jobs
        .sweep_artwork_with_backoff(0)
        .await
        .expect("first pass");
    let second = jobs
        .sweep_artwork_with_backoff(0)
        .await
        .expect("second pass");
    assert_eq!(first.claimed_ids.len(), ARTWORK_RETRY_BATCH as usize);
    assert_eq!(second.claimed_ids.len(), EXTRA);
    let first_ids: HashSet<i64> = first.claimed_ids.into_iter().collect();
    assert!(
        second.claimed_ids.iter().all(|id| !first_ids.contains(id)),
        "no row is reclaimed before every never-attempted row is claimed"
    );
    let mut all = first_ids;
    all.extend(second.claimed_ids);
    assert_eq!(all.len(), episodes.len());
    assert_eq!(season_hits.load(Ordering::SeqCst), 2);
}

/// A missing key or a transient provider failure made no artwork attempt.
/// Neither may be laundered into the 24-hour "provider has no image"
/// backoff; the normal half-hour scheduler should get another chance.
#[tokio::test]
async fn artwork_retry_does_not_back_off_work_that_never_reached_a_provider_result() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let episode = seeded_episode_backlog(&store, 1).await[0];
    store
        .put_setting(keys::TMDB_API_KEY, "")
        .await
        .expect("remove key");
    let jobs = manager(store.clone(), artwork.path());
    assert_eq!(jobs.sweep_artwork().await.expect("missing-key pass"), 0);
    assert!(
        store
            .get_item(episode)
            .await
            .expect("get")
            .expect("episode")
            .artwork_attempted_at
            .is_none(),
        "skipping for a missing key is not an artwork attempt"
    );

    store
        .put_setting(keys::TMDB_API_KEY, "test-key")
        .await
        .expect("restore key");
    use axum::http::StatusCode;
    use axum::routing::get;
    let base = serve(axum::Router::new().route(
        "/tv/42/season/1",
        get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
    ))
    .await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
    assert_eq!(jobs.sweep_artwork().await.expect("transient pass"), 0);
    let episode = store
        .get_item(episode)
        .await
        .expect("get")
        .expect("episode");
    assert!(episode.artwork_attempted_at.is_none());
    assert!(episode.artwork_error.is_none());
    assert_eq!(
        store
            .items_missing_artwork(None, keys::ARTWORK_RETRY_BACKOFF_SECS, 100)
            .await
            .expect("still due")
            .len(),
        1,
        "a transient failure remains eligible for the next scheduler pass"
    );
}

/// The spawned entry point itself is single-flight. A second tick while a
/// provider request is blocked must return without issuing another pass.
#[tokio::test]
async fn artwork_retry_pass_is_single_flight() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    seeded_episode_backlog(&store, 1).await;
    let hits = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let base = serve(blocking_season_tmdb(
        Arc::clone(&hits),
        Arc::clone(&entered),
        Arc::clone(&release),
    ))
    .await;
    let jobs = manager_with_tmdb(store, artwork.path(), &base);

    let first = tokio::spawn(Arc::clone(&jobs).artwork_retry_pass());
    tokio::time::timeout(PROVIDER_LIVENESS, entered.notified())
        .await
        .expect("first pass reached provider");
    tokio::time::timeout(PROVIDER_LIVENESS, Arc::clone(&jobs).artwork_retry_pass())
        .await
        .expect("second pass returned");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    release.notify_waiters();
    first.await.expect("first pass task");
    assert!(!jobs.retrying_artwork.load(Ordering::Relaxed));
}

/// Dispatching a slow artwork retry must not hold the scheduler loop. The
/// cleanup job comes after artwork in `due_jobs`, so its persisted stamp is
/// direct evidence that the tick continued while TMDB was still blocked.
#[tokio::test]
async fn due_jobs_continue_while_artwork_retry_is_in_flight() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    seeded_episode_backlog(&store, 1).await;
    store
        .put_setting(keys::JOB_TRANSCODE_CLEANUP_MINS, "15")
        .await
        .expect("schedule cleanup");
    let hits = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let base = serve(blocking_season_tmdb(
        Arc::clone(&hits),
        Arc::clone(&entered),
        Arc::clone(&release),
    ))
    .await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    tokio::time::timeout(PROVIDER_LIVENESS, jobs.run_due_jobs(&transcode))
        .await
        .expect("scheduler returned while artwork was blocked")
        .expect("scheduler tick");
    tokio::time::timeout(PROVIDER_LIVENESS, entered.notified())
        .await
        .expect("artwork reached provider");
    let cleanup_key = jobs.local_job_key(keys::JOB_LAST_TRANSCODE_CLEANUP);
    assert!(
        store
            .get_setting(&cleanup_key)
            .await
            .expect("cleanup stamp")
            .is_some(),
        "a job ordered after artwork completed in the same tick"
    );
    assert!(
        store
            .get_setting(keys::JOB_LAST_TRANSCODE_CLEANUP)
            .await
            .expect("legacy global cleanup stamp")
            .is_none(),
        "node-local cleanup must not recreate the cluster-wide clock"
    );
    let other_store: Arc<dyn Store> = store.clone();
    let other = JobManager::new_with_scan_prune_percent(
        other_store,
        artwork.path().to_path_buf(),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "other-cleanup-node".to_owned(),
        Arc::new(plurx_core::cluster::coordination::UnclusteredJobAuthority),
    );
    let other_key = other.local_job_key(keys::JOB_LAST_TRANSCODE_CLEANUP);
    assert_ne!(cleanup_key, other_key);
    assert_eq!(other.job_stamp(&other_key).await, None);
    other.stamp_local(keys::JOB_LAST_TRANSCODE_CLEANUP).await;
    assert!(jobs.job_stamp(&cleanup_key).await.is_some());
    assert!(other.job_stamp(&other_key).await.is_some());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    release.notify_waiters();
    tokio::time::timeout(PROVIDER_LIVENESS, async {
        while jobs.retrying_artwork.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("artwork pass finished");
}

#[tokio::test]
async fn scheduled_telemetry_prune_is_aged_bounded_and_stamped() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let now_secs = now();
    for (name, at) in [
        ("expired", (now_secs - 31 * 24 * 60 * 60) * 1_000),
        ("kept", (now_secs - 29 * 24 * 60 * 60) * 1_000),
    ] {
        store
            .record_playback_event(&PlaybackEvent {
                at_unix_ms: at,
                event: name.to_owned(),
                ..PlaybackEvent::default()
            })
            .await
            .expect("seed telemetry");
    }
    let artwork = crate::test_tempdir().expect("artwork");
    let transcode_dir = crate::test_tempdir().expect("transcode");
    let jobs = manager(store.clone(), artwork.path());
    let transcode = Arc::new(TranscodeManager::new(
        store.clone(),
        transcode_dir.path().join("work"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));

    jobs.run_due_jobs(&transcode).await.expect("scheduler tick");
    let events = store
        .playback_events(&PlaybackEventQuery {
            limit: 10,
            ..PlaybackEventQuery::default()
        })
        .await
        .expect("remaining telemetry");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "kept");
    assert!(
        store
            .get_setting(&jobs.local_job_key(keys::JOB_LAST_TELEMETRY_PRUNE))
            .await
            .expect("prune stamp")
            .is_some(),
        "the scheduler persists the prune clock"
    );
}

/// Production shape: the known show has a poster and an enrichment stamp,
/// while later-imported children have neither artwork nor an attempt
/// stamp. One valid episode is repaired; one local episode TMDB does not
/// list is stamped and backed off. The show is only a route and stays
/// byte-for-byte untouched.
#[tokio::test]
async fn the_artwork_sweep_repairs_and_converges_blank_tv_children() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let (library_id, show, season) = seeded_enriched_show(&store).await;
    let episode = store
        .insert_item(&NewItem {
            library_id,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: "Episode 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: Some(1),
        })
        .await
        .expect("episode");
    let ghost = store
        .insert_item(&NewItem {
            library_id,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: "Episode 99".into(),
            year: None,
            season_number: Some(1),
            episode_number: Some(99),
        })
        .await
        .expect("ghost episode");

    let show_hits = Arc::new(AtomicUsize::new(0));
    let season_hits = Arc::new(AtomicUsize::new(0));
    let base = serve(targeted_show_tmdb(
        Arc::clone(&show_hits),
        Arc::clone(&season_hits),
        Some("/season-1.jpg"),
    ))
    .await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
    assert_eq!(
        jobs.sweep_artwork().await.expect("sweep"),
        2,
        "the return value counts repaired candidate rows, not the routing show"
    );

    let show = store.get_item(show).await.expect("get show").expect("show");
    let season = store
        .get_item(season)
        .await
        .expect("get season")
        .expect("season");
    let episode = store
        .get_item(episode)
        .await
        .expect("get episode")
        .expect("episode");
    let ghost = store
        .get_item(ghost)
        .await
        .expect("get ghost")
        .expect("ghost");
    assert_eq!(show.poster_path.as_deref(), Some("existing-show.jpg"));
    assert!(season.poster_path.is_some(), "season card repaired");
    assert!(episode.poster_path.is_some(), "episode card repaired");
    assert!(ghost.poster_path.is_none(), "TMDB has no episode 99");
    assert!(
        ghost.artwork_attempted_at.is_some(),
        "an unmatched child is stamped so the daily backoff can engage"
    );
    assert!(store
        .items_missing_artwork(None, keys::ARTWORK_RETRY_BACKOFF_SECS, 100)
        .await
        .expect("due after sweep")
        .is_empty());
    assert_eq!(jobs.sweep_artwork().await.expect("second sweep"), 0);
    assert_eq!(show_hits.load(Ordering::SeqCst), 0);
    assert_eq!(
        season_hits.load(Ordering::SeqCst),
        1,
        "a converged child cannot pin its show to a half-hour retry loop"
    );
}

/// A blank season is independently repairable even when every episode
/// still is healthy. This is the empty-bucket path: no episode should be
/// downloaded merely to make the season endpoint run.
#[tokio::test]
async fn the_artwork_sweep_repairs_a_blank_season_without_touching_healthy_rows() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let (library_id, show, season) = seeded_enriched_show(&store).await;
    let episode = store
        .insert_item(&NewItem {
            library_id,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: "Episode 1".into(),
            year: None,
            season_number: Some(1),
            episode_number: Some(1),
        })
        .await
        .expect("episode");
    store
        .apply_metadata(
            episode,
            &MetadataPatch {
                poster_path: Some("healthy-episode.jpg".into()),
                artwork: Some(ArtworkAttempt::Stored),
                ..Default::default()
            },
        )
        .await
        .expect("healthy episode");

    let show_hits = Arc::new(AtomicUsize::new(0));
    let season_hits = Arc::new(AtomicUsize::new(0));
    let base = serve(targeted_show_tmdb(
        Arc::clone(&show_hits),
        Arc::clone(&season_hits),
        Some("/season-1.jpg"),
    ))
    .await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
    assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 1);

    let show = store.get_item(show).await.expect("show").expect("show");
    let season = store
        .get_item(season)
        .await
        .expect("season")
        .expect("season");
    let episode = store
        .get_item(episode)
        .await
        .expect("episode")
        .expect("episode");
    assert_eq!(show.poster_path.as_deref(), Some("existing-show.jpg"));
    assert!(season.poster_path.is_some());
    assert_eq!(episode.poster_path.as_deref(), Some("healthy-episode.jpg"));
    assert_eq!(show_hits.load(Ordering::SeqCst), 0);
    assert_eq!(season_hits.load(Ordering::SeqCst), 1);
}

/// TMDB can legitimately have no poster for a season. That is an attempt,
/// not silence: the row stays blank but leaves the immediate due set.
#[tokio::test]
async fn the_artwork_sweep_stamps_a_season_when_tmdb_has_no_poster() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let artwork = crate::test_tempdir().expect("artwork");
    let (_, show, season) = seeded_enriched_show(&store).await;
    let show_hits = Arc::new(AtomicUsize::new(0));
    let season_hits = Arc::new(AtomicUsize::new(0));
    let base = serve(targeted_show_tmdb(
        Arc::clone(&show_hits),
        Arc::clone(&season_hits),
        None,
    ))
    .await;
    let jobs = manager_with_tmdb(store.clone(), artwork.path(), &base);
    assert_eq!(jobs.sweep_artwork().await.expect("sweep"), 0);

    let show = store.get_item(show).await.expect("show").expect("show");
    let season = store
        .get_item(season)
        .await
        .expect("season")
        .expect("season");
    assert_eq!(show.poster_path.as_deref(), Some("existing-show.jpg"));
    assert!(season.poster_path.is_none());
    assert!(season.artwork_attempted_at.is_some());
    assert!(store
        .items_missing_artwork(None, keys::ARTWORK_RETRY_BACKOFF_SECS, 100)
        .await
        .expect("due after unavailable poster")
        .is_empty());
    assert_eq!(show_hits.load(Ordering::SeqCst), 0);
    assert_eq!(season_hits.load(Ordering::SeqCst), 1);
}
