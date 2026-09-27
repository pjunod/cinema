// Included in vodserve::tests to exercise the real attachment/GET/producer
// seams with the same fixtures and publication commit as the copy tests.

fn encoded_plan(
    file: &MediaFile,
    options: &plurx_core::transcode::TranscodeOptions,
    encoder: plurx_core::transcode::Encoder,
) -> plurx_core::transcode::ResolvedTranscode {
    use plurx_core::transcode::{
        resolve_transcode, AttemptRestrictions, DecodeCapabilities,
        DecodeCapabilitySnapshotIdentity, DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot,
        DecodeSourceIdentity, SoftwareDecoder, TranscodeMediaOptions, TranscodeRequest,
    };

    let codec = file.video_codec.as_deref().unwrap_or("h264");
    let transfer = match plurx_core::transcode::routing_hdr(file) {
        Some("hdr10" | "hdr10plus" | "dolby_vision") => Some("smpte2084"),
        Some("hlg") => Some("arib-std-b67"),
        _ => Some("bt709"),
    };
    let facts = DecodeFacts::from_ffprobe_json(
        &serde_json::json!({
            "streams": [{
                "index": 0,
                "codec_type": "video",
                "codec_name": codec,
                "width": file.width.unwrap_or(320),
                "height": file.height.unwrap_or(180),
                "pix_fmt": if file.bit_depth.unwrap_or(8) >= 10 { "yuv420p10le" } else { "yuv420p" },
                "avg_frame_rate": "24000/1001",
                "r_frame_rate": "24000/1001",
                "color_transfer": transfer,
                "disposition": {"attached_pic": 0}
            }]
        }),
        DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("source identity"),
    )
    .expect("decode facts");
    let capabilities = DecodeCapabilities::new(
        DecodeCapabilitySnapshotIdentity::new(
            "b".repeat(64),
            "vod-test-node".to_owned(),
            None,
        )
        .expect("capability identity"),
        vec![],
        vec![SoftwareDecoder {
            codec: codec.to_owned(),
            implementation: Some(codec.to_owned()),
        }],
    )
    .expect("capabilities");
    resolve_transcode(
        &TranscodeRequest::new(encoder, TranscodeMediaOptions::from_options(file, options)),
        &facts,
        &capabilities,
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::none(),
    )
    .expect("resolved VOD fixture plan")
}

fn refresh_encoded_plan(file: &MediaFile, encoding: &mut crate::vodencode::Encoding) {
    let plan = encoded_plan(file, &encoding.options, encoding.plan.encoder());
    let resources = crate::admission::TranscodeResourceEstimate::of(
        &plan,
        &crate::admission::Workload::of(file, encoding.options.target_height),
    );
    encoding.plan = plan;
    encoding.resources = resources;
}

// Each test below owns a fresh `Admissions`, while production encoders on
// one daemon share a single admission budget. Running these restart campaigns
// concurrently can therefore launch more real FFmpeg processes than a daemon
// permits and make a healthy child lose its init or seek preroll under load.
// Serialize only this real-FFmpeg integration module so the fixture preserves the
// production resource boundary without weakening any product concurrency.
static ENCODED_INTEGRATION_CAMPAIGN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn ffmpeg_has_filters(names: &[&str]) -> bool {
    let Ok(output) = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-filters"])
        .kill_on_drop(true)
        .output()
        .await
    else {
        return false;
    };
    output.status.success()
        && crate::pipeprobe::declares_filters(&String::from_utf8_lossy(&output.stdout), names)
}

async fn encoded_fixture(base: &Path) -> (MediaFile, Arc<crate::vodencode::Encoding>) {
    use plurx_core::transcode::{Encoder, TranscodeOptions, VodFrameGrid};
    testfixtures::require_ffmpeg();
    let path = base.join("ntsc-source.mkv");
    let output = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=24000/1001,drawbox=x=0:y=120:w=iw:h=60:color=black:t=fill,drawbox=x=0:y=0:w=32:h=32:color=red:t=fill:enable='lt(t,2)',drawbox=x=0:y=0:w=32:h=32:color=green:t=fill:enable='gte(t,2)*lt(t,80)',drawbox=x=0:y=0:w=32:h=32:color=blue:t=fill:enable='gte(t,80)'", "-f", "lavfi", "-i", "aevalsrc=0.08*sin(2*PI*440*t)+if(eq(mod(n\\,96096)\\,95936)\\,0.4\\,0)+if(eq(mod(n\\,96096)\\,160)\\,0.6\\,0):s=48000", "-t", "96", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "30", "-threads", "2", "-g", "120", "-c:a", "aac"])
        .arg(&path).kill_on_drop(true).output().await.expect("generate source");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut file = media_file_at(path, 96_000);
    file.video_codec = Some("h264".into());
    file.width = Some(320);
    file.height = Some(180);
    file.audio_streams = vec![plurx_core::domain::AudioStream {
        index: 0,
        codec: "aac".into(),
        channels: Some(1),
        sample_rate: Some(48_000),
        language: Some("eng".into()),
        title: None,
        default: true,
    }];
    let metadata = std::fs::metadata(&file.path).expect("source metadata");
    file.size = metadata.len() as i64;
    file.mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    let options = TranscodeOptions {
        target_height: 144,
        video_bitrate_kbps: 300,
        software_threads: Some(2),
        ..Default::default()
    };
    let plan = encoded_plan(&file, &options, Encoder::Software);
    let resources = crate::admission::TranscodeResourceEstimate::of(
        &plan,
        &crate::admission::Workload::of(&file, options.target_height),
    );
    let encoding = Arc::new(crate::vodencode::Encoding {
        source_object_version: crate::fragment_index_cluster::open_source_fence(&file, None)
            .await
            .expect("source identity")
            .object_version()
            .to_owned(),
        plan,
        resources,
        options,
        grid: VodFrameGrid::new(24000, 1001).expect("NTSC grid"),
        subtitle: None,
        subtitle_digest: None,
        ffmpeg_build: crate::ffmpeg::ffmpeg_build().await,
        executable: crate::ffmpeg::EncodedExecutable::capture()
            .await
            .expect("frozen encoder"),
        engine: crate::ffmpeg::EncodedEngine::capture(None)
            .await
            .expect("frozen engine"),
        admissions: crate::admission::Admissions::new(),
        store: Arc::new(SqliteStore::open_in_memory().expect("capacity policy")),
        speculative: AtomicBool::new(false),
        queued: StdMutex::new(None),
        policy_retry: AtomicBool::new(false),
        handoff_wait: AtomicBool::new(false),
        last_refusal: StdMutex::new(None),
        handoff_claim: StdMutex::new(None),
        admission_pause: StdMutex::new(None),
    });
    (file, encoding)
}

async fn fetched_bytes(serve: &Arc<VodServe>, session: &str, name: &str) -> Vec<u8> {
    let mut ready = fetch(serve, session, name).await;
    let mut bytes = Vec::new();
    ready
        .file
        .read_to_end(&mut bytes)
        .await
        .expect("read served file");
    assert_eq!(bytes.len() as u64, ready.len);
    bytes
}

#[tokio::test]
async fn encoded_identity_never_shares_a_renderer_across_processes() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("process-isolated identity");
    let (file, mut encoding) = encoded_fixture(base.path()).await;
    let dependency = base.path().join("driver");
    tokio::fs::write(&dependency, b"same renderer inputs")
        .await
        .expect("driver identity");
    let mutable = Arc::get_mut(&mut encoding).expect("unique recipe");
    mutable.engine = crate::ffmpeg::EncodedEngine::capture_test_objects(
        std::slice::from_ref(&dependency),
        "node-a-process",
    )
    .await
    .expect("first process");
    let first = mutable.identity(&file, 96.0);
    mutable.engine = crate::ffmpeg::EncodedEngine::capture_test_objects(
        std::slice::from_ref(&dependency),
        "node-b-process",
    )
    .await
    .expect("other process");
    let other = mutable.identity(&file, 96.0);
    assert_ne!(first, other, "different nodes must never share encoded bytes");
}

#[tokio::test]
async fn encoded_vod_resurrection_cannot_adopt_same_size_mtime_replacement() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("source replacement");
    let (mut file, mut encoding) = encoded_fixture(base.path()).await;
    for color in ["red", "blue"] {
        let generated = tokio::process::Command::new(ffmpeg_bin())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={color}:s=64x64:r=24"),
                "-t",
                "6",
                "-c:v",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-threads",
                "1",
                "-f",
                "nut",
            ])
            .arg(base.path().join(format!("{color}.nut")))
            .kill_on_drop(true)
            .output()
            .await
            .expect("valid replacement video");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
    }
    file.path = base.path().join("red.nut");
    let metadata = std::fs::metadata(&file.path).expect("source metadata");
    let modified = metadata.modified().expect("original nanosecond mtime");
    file.size = metadata.len() as i64;
    file.mtime = modified
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    file.duration_ms = Some(6_000);
    file.width = Some(64);
    file.height = Some(64);
    file.video_codec = Some("rawvideo".into());
    file.audio_streams.clear();
    assert_eq!(
        std::fs::metadata(base.path().join("blue.nut"))
            .expect("replacement")
            .len(),
        metadata.len(),
        "different pixels but identical source length"
    );
    let mutable = Arc::get_mut(&mut encoding).expect("unique recipe");
    mutable.source_object_version = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .expect("original fence")
        .object_version()
        .to_owned();
    mutable.options.target_height = 64;
    mutable.plan = encoded_plan(
        &file,
        &mutable.options,
        plurx_core::transcode::Encoder::Software,
    );
    mutable.resources = crate::admission::TranscodeResourceEstimate::of(
        &mutable.plan,
        &crate::admission::Workload::of(&file, mutable.options.target_height),
    );
    mutable.grid = plurx_core::transcode::VodFrameGrid::new(24, 1).expect("grid");
    let cache = base.path().join("renditions");
    let old = bare_serve(&cache);
    let mut req = request("source-replacement", 0.0);
    req.kind = SessionKind::Transcode { height: 64 };
    old.try_create(
        VodRecipeRequest {
            request: &req,
            encoding: Some(Arc::clone(&encoding)),
        },
        &file,
        &settings(),
        VodAttribution {
            user_name: "test",
            item_title: "source fence",
            supersession_user: "test",
        },
        "old".into(),
    )
    .await
    .expect("old source session");
    assert_eq!(
        old.shared.sessions.lock().await["old"].delivery.method(),
        "transcode",
        "an encoded VOD session's bytes are transcode bytes"
    );
    let _ = fetched_bytes(&old, "old", "seg00000.m4s").await;
    let old_rendition = rendition_of(&old, "old").await;
    let old_key = old_rendition.key.clone();
    old.end("old", Terminal::Deleted).await;
    old_rendition.closed.store(true, Relaxed);
    old_rendition.kick();
    let _ = old_rendition
        .slot
        .perform(
            Step::Terminate {
                why: Termination::Idle,
            },
            || {},
        )
        .await;
    // Keep the old cache intact: resurrection must reject it by identity,
    // not pass because the test deleted all evidence of the wrong film.
    std::fs::rename(base.path().join("blue.nut"), &file.path).expect("atomic replacement");
    std::fs::File::options()
        .write(true)
        .open(&file.path)
        .expect("replacement file")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("preserve exact old mtime");
    let new = bare_serve(&cache);
    let refused = new
        .try_create(
            VodRecipeRequest {
                request: &req,
                encoding: Some(Arc::clone(&encoding)),
            },
            &file,
            &settings(),
            VodAttribution {
                user_name: "test",
                item_title: "source fence",
                supersession_user: "test",
            },
            "stale".into(),
        )
        .await
        .expect_err("a captured old source cannot attach a new object");
    assert!(refused.contains("source changed"), "{refused}");
    let fresh = Arc::new(crate::vodencode::Encoding {
        source_object_version: crate::fragment_index_cluster::open_source_fence(&file, None)
            .await
            .expect("new source fence")
            .object_version()
            .to_owned(),
        plan: encoding.plan.clone(),
        resources: encoding.resources,
        options: encoding.options.clone(),
        grid: encoding.grid,
        subtitle: None,
        subtitle_digest: None,
        ffmpeg_build: encoding.ffmpeg_build.clone(),
        executable: crate::ffmpeg::EncodedExecutable::capture()
            .await
            .expect("encoder"),
        engine: crate::ffmpeg::EncodedEngine::capture(None)
            .await
            .expect("engine"),
        admissions: encoding.admissions.clone(),
        store: Arc::clone(&encoding.store),
        speculative: AtomicBool::new(false),
        queued: StdMutex::new(None),
        policy_retry: AtomicBool::new(false),
        handoff_wait: AtomicBool::new(false),
        last_refusal: StdMutex::new(None),
        handoff_claim: StdMutex::new(None),
        admission_pause: StdMutex::new(None),
    });
    new.try_create(
        VodRecipeRequest {
            request: &req,
            encoding: Some(fresh),
        },
        &file,
        &settings(),
        VodAttribution {
            user_name: "test",
            item_title: "source fence",
            supersession_user: "test",
        },
        "new".into(),
    )
    .await
    .expect("fresh source session");
    let rendition = rendition_of(&new, "new").await;
    assert_ne!(old_key, rendition.key);
    let media = fetched_bytes(&new, "new", "seg00000.m4s").await;
    let init = fetched_bytes(&new, "new", "init.mp4").await;
    let output = base.path().join("new-source-get.mp4");
    tokio::fs::write(&output, [init, media].concat())
        .await
        .expect("actual GETs");
    let decoded = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
        .arg(output)
        .args([
            "-vf",
            "crop=2:2:8:8,format=rgb24",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .expect("new pixels");
    assert!(decoded.status.success());
    assert!(
        decoded.stdout[2] > 150 && decoded.stdout[0] < 40,
        "new GET must show blue, never cached red: {:?}",
        decoded.stdout
    );
    new.end("new", Terminal::Deleted).await;
}

#[tokio::test]
async fn encoded_vod_restarts_obey_current_capacity_policy() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("capacity policy");
    let (_, encoding) = encoded_fixture(base.path()).await;
    encoding
        .store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
        .await
        .expect("pool enabled");
    let first = encoding.try_permit().await.expect("initial permit");
    encoding
        .store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "0")
        .await
        .expect("pool disabled");
    assert!(encoding.try_permit().await.is_none());
    assert_eq!(
        encoding.admissions.software_in_use(),
        2,
        "existing child retains admission"
    );
    drop(first);
    assert!(
        encoding.try_permit().await.is_none(),
        "old rendition cannot resurrect old policy"
    );
    encoding
        .store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
        .await
        .expect("pool restored");
    assert!(encoding.try_permit().await.is_some());
    assert!(!encoding.is_waiting());
}

#[tokio::test]
async fn encoded_vod_capacity_read_has_one_deadline_and_no_orphaned_wait() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("held admission policy");
    let (_, encoding) = encoded_fixture(base.path()).await;
    tokio::time::pause();
    let before = tokio::time::Instant::now();
    assert!(encoding
        .try_permit_after(std::future::pending())
        .await
        .is_none());
    assert!(before.elapsed() >= Duration::from_secs(1));
    assert!(
        before.elapsed() <= Duration::from_millis(1010),
        "the timer wheel may round up one tick, not restart admission"
    );
    assert_eq!(encoding.admissions.software_in_use(), 0);
    assert!(
        !encoding.admissions.live_is_waiting(),
        "backend failure must not strand a foreground permit waiter"
    );
    assert!(
        encoding.is_waiting(),
        "the sole driver still owns a bounded retry"
    );
    let permit = encoding
        .try_permit_after(std::future::ready(Ok((Some("0".into()), Some("2".into())))))
        .await;
    assert!(
        permit.is_some(),
        "fresh readable policy may admit after the old read timed out"
    );
    assert!(!encoding.is_waiting());
    drop(permit);
    assert_eq!(encoding.admissions.software_in_use(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn encoded_vod_held_capacity_keeps_cached_gets_open_and_rechecks_seek_after_reap() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("driver admission ownership");
    let (file, encoding) = encoded_fixture(base.path()).await;
    encoding
        .store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "2")
        .await
        .expect("one recipe exactly fills the pool");
    let serve = bare_serve(&base.path().join("renditions"));
    let plan = encoding.grid.plan(96_000, 428_000);
    let rendition = serve
        .shared
        .build_rendition(
            "held-admission",
            None,
            Recipe {
                file,
                audio_index: None,
                aac: true,
                video: CopyVideoOptions::new(false, false),
                source_object_version: Some(encoding.source_object_version.clone()),
                cluster_cache_key: None,
                encoding: Some(Arc::clone(&encoding)),
            },
            plan,
            &settings(),
        )
        .await
        .expect("real encoded rendition without an automatic driver");
    rendition.attach_reader("viewer", 45).await;
    rendition
        .readers
        .lock()
        .await
        .get_mut("viewer")
        .expect("reader")
        .accept_control(1, 45);
    rendition
        .dir
        .materialize(
            &mut *rendition.manifest.lock().await,
            0,
            b"ready cached media",
            now_ms(),
        )
        .await
        .expect("already materialized response");
    let old_permit = encoding.try_permit().await.expect("old generation permit");
    let old_child = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .expect("old still-running generation");
    let old_pid = old_child.id().expect("old PID");
    rendition
        .slot
        .attach_owned(old_child, 0, Some(Box::new(old_permit)))
        .await;
    let old_wait = serve
        .shared
        .pool
        .register(
            WaitKey {
                rendition: rendition.key.clone(),
                index: 45,
            },
            "viewer",
        )
        .expect("admitted forward seek");
    let pause = Arc::new(tokio::sync::Barrier::new(2));
    *encoding.admission_pause.lock().expect("admission seam") = Some(Arc::clone(&pause));
    let pass = {
        let shared = Arc::clone(&serve.shared);
        let rendition = Arc::clone(&rendition);
        tokio::spawn(async move { driver_pass(&shared, &rendition).await })
    };
    tokio::time::timeout(Duration::from_secs(5), pause.wait())
        .await
        .expect("driver reaches policy await");
    assert_eq!(
        encoding.admissions.software_in_use(),
        0,
        "restart reaps its own sole permit before admission"
    );
    assert_eq!(
        unsafe { libc::kill(old_pid as libc::pid_t, 0) },
        -1,
        "old process really reaped"
    );
    let ready = tokio::time::timeout(
        Duration::from_secs(5),
        serve.serve_segment(
            &rendition,
            "viewer",
            0,
            Duration::from_secs(5),
            Arc::new(crate::meter::Meter::new()),
        ),
    )
    .await
    .expect("cached GET must not wait for the held admission read")
    .expect("cached response");
    assert_eq!(read_ready(ready).await, b"ready cached media");
    drop(old_wait);
    rendition
        .readers
        .lock()
        .await
        .get_mut("viewer")
        .expect("reader")
        .accept_control(2, 1);
    let latest_wait = serve
        .shared
        .pool
        .register(
            WaitKey {
                rendition: rendition.key.clone(),
                index: 1,
            },
            "viewer",
        )
        .expect("admitted backward seek");
    pause.wait().await;
    tokio::time::timeout(Duration::from_secs(5), pass)
        .await
        .expect("one permit cannot self-deadlock")
        .expect("driver task");
    assert_eq!(
        rendition.slot.belief().await.positioned_at(),
        Some(1),
        "spawn must use the latest seek, not the pre-admission decision at45"
    );
    assert_eq!(encoding.admissions.software_in_use(), 2);
    drop(latest_wait);
    rendition.gen_epoch.fetch_add(1, Relaxed);
    rendition
        .slot
        .perform(
            Step::Terminate {
                why: Termination::Idle,
            },
            || {},
        )
        .await
        .expect("final child reap");
    assert_eq!(encoding.admissions.software_in_use(), 0);
}

#[tokio::test]
async fn encoded_vod_burn_sidecar_cannot_reuse_replaced_source_captions() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("burn identity");
    let video = testfixtures::source("h264");
    for caption in ["ALPHA", "BRAVO"] {
        let text = base.path().join(format!("{caption}.srt"));
        tokio::fs::write(
            &text,
            format!("1\n00:00:00,000 --> 00:00:04,000\n{caption}\n\n"),
        )
        .await
        .expect("source caption");
        let muxed = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&video)
            .arg("-i")
            .arg(text)
            .args(["-map", "0", "-map", "1", "-c", "copy"])
            .arg(base.path().join(format!("{caption}.mkv")))
            .kill_on_drop(true)
            .output()
            .await
            .expect("source mux");
        assert!(
            muxed.status.success(),
            "{}",
            String::from_utf8_lossy(&muxed.stderr)
        );
    }
    let mut file = media_file_at(base.path().join("ALPHA.mkv"), 6_000);
    let metadata = std::fs::metadata(&file.path).expect("source metadata");
    let modified = metadata.modified().expect("mtime");
    file.size = metadata.len() as i64;
    file.mtime = modified
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    assert_eq!(
        metadata.len(),
        std::fs::metadata(base.path().join("BRAVO.mkv"))
            .expect("replacement")
            .len()
    );
    let cache = base.path().join("subtitles");
    let mut old = crate::subtitles::ensure_burn_file(
        &cache,
        &file,
        0,
        None,
        crate::subtitles::SIDECAR_JOIN_UNBOUNDED,
    )
        .await
        .expect("first burn extraction");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut old, &mut bytes).expect("first sidecar");
    assert!(bytes.windows(5).any(|window| window == b"ALPHA"));
    std::fs::rename(base.path().join("BRAVO.mkv"), &file.path).expect("replace captions");
    std::fs::File::options()
        .write(true)
        .open(&file.path)
        .expect("replacement handle")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("preserve exact mtime");
    let mut new = crate::subtitles::ensure_burn_file(
        &cache,
        &file,
        0,
        None,
        crate::subtitles::SIDECAR_JOIN_UNBOUNDED,
    )
        .await
        .expect("new source burn extraction");
    bytes.clear();
    std::io::Read::read_to_end(&mut new, &mut bytes).expect("new sidecar");
    assert!(bytes.windows(5).any(|window| window == b"BRAVO"));
    assert!(!bytes.windows(5).any(|window| window == b"ALPHA"));
}

#[cfg(unix)]
#[tokio::test]
async fn burn_extractor_physically_caps_oversized_matroska_attachment() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    const CAP: u64 = 64 * 1024 * 1024;
    let base = crate::test_tempdir().expect("oversized burn attachment");
    let attachment = base.path().join("oversized-font.bin");
    std::fs::File::create(&attachment)
        .expect("attachment")
        .set_len(CAP + 1024 * 1024)
        .expect("sparse attachment");
    let captions = base.path().join("caption.srt");
    tokio::fs::write(
        &captions,
        "1\n00:00:00,000 --> 00:00:04,000\nCAP TEST\n\n",
    )
    .await
    .expect("caption");
    let source = base.path().join("oversized.mkv");
    let output = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(testfixtures::source("h264"))
        .arg("-i")
        .arg(&captions)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:s:0",
            "-c",
            "copy",
            "-attach",
        ])
        .arg(&attachment)
        .args([
            "-metadata:s:t",
            "mimetype=application/octet-stream",
            "-metadata:s:t",
            "filename=oversized-font.bin",
        ])
        .arg(&source)
        .kill_on_drop(true)
        .output()
        .await
        .expect("mux oversized attachment");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut file = media_file_at(source, 6_000);
    let metadata = std::fs::metadata(&file.path).expect("source metadata");
    file.size = metadata.len() as i64;
    file.mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    let cache = base.path().join("subtitles");
    let error = crate::subtitles::ensure_burn_file(
        &cache,
        &file,
        0,
        None,
        crate::subtitles::SIDECAR_JOIN_UNBOUNDED,
    )
        .await
        .expect_err("oversized attachment must be stopped before publication");
    assert!(error.contains("disk bound"), "{error}");
    for entry in std::fs::read_dir(&cache).expect("cache directory") {
        let metadata = entry.expect("cache entry").metadata().expect("metadata");
        assert!(metadata.len() <= CAP, "temporary output crossed the cap");
    }
}

#[tokio::test]
async fn encoded_vod_hdr10_gets_keep_main10_and_pq_across_restarts() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    testfixtures::require_ffmpeg();
    if !ffmpeg_has_filters(&["zscale"]).await {
        eprintln!(
            "skipping encoded_vod_hdr10_gets_keep_main10_and_pq_across_restarts: \
             `{}` has no zscale filter",
            ffmpeg_bin()
        );
        return;
    }
    let base = crate::test_tempdir().expect("HDR10 grid");
    let (mut file, mut encoding) = encoded_fixture(base.path()).await;
    let path = base.path().join("pq-source.mkv");
    let generated = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file.path)
        .args([
            "-vf",
            "zscale=matrixin=bt709:transferin=bt709:primariesin=bt709:matrix=bt2020nc:transfer=smpte2084:primaries=bt2020,format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-x265-params",
            "pools=2:frame-threads=2:log-level=error",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "smpte2084",
            "-colorspace",
            "bt2020nc",
            "-c:a",
            "copy",
        ])
        .arg(&path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("synthetic PQ source");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    file.path = path;
    file.video_codec = Some("hevc".into());
    file.hdr = Some("hdr10".into());
    file.bit_depth = Some(10);
    let metadata = std::fs::metadata(&file.path).expect("PQ metadata");
    file.size = metadata.len() as i64;
    file.mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    let mutable = Arc::get_mut(&mut encoding).expect("unique recipe");
    mutable.options.pipeline = plurx_core::transcode::Pipeline::Hdr10Passthrough;
    mutable.source_object_version = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .expect("PQ source fence")
        .object_version()
        .to_owned();
    refresh_encoded_plan(&file, mutable);
    let serve = bare_serve(&base.path().join("renditions"));
    let mut first_init = None;
    for entry in [0, 45, 1] {
        let (init, first, next) = encoded_pair(&serve, &file, &encoding, entry).await;
        if let Some(expected) = &first_init {
            assert_eq!(&init, expected);
        } else {
            first_init = Some(init.clone());
        }
        let output = base.path().join(format!("pq-get-{entry}.mp4"));
        tokio::fs::write(&output, [init, first, next].concat())
            .await
            .expect("actual HDR GETs");
        let probe = plurx_core::scan::probe::probe(&output)
            .await
            .expect("actual output probe");
        let json: serde_json::Value =
            serde_json::from_str(probe.raw_json.as_deref().expect("probe JSON"))
                .expect("probe fields");
        let video = json["streams"]
            .as_array()
            .expect("streams")
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .expect("video");
        assert_eq!(video["codec_name"], "hevc");
        assert_eq!(video["pix_fmt"], "yuv420p10le");
        assert_eq!(video["color_transfer"], "smpte2084");
        let decoded = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
            .arg(output)
            .args(["-map", "0:v", "-map", "0:a", "-f", "null", "-"])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode actual Main10 GETs");
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
    }
}

async fn encoded_pair(
    serve: &Arc<VodServe>,
    file: &MediaFile,
    encoding: &Arc<crate::vodencode::Encoding>,
    entry: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let session = uuid::Uuid::new_v4().to_string();
    let start = f64::from(entry) * encoding.grid.segment_ticks() as f64
        / f64::from(encoding.grid.numerator);
    let mut req = request(&session, start);
    req.kind = SessionKind::Transcode { height: 144 };
    serve
        .try_create(
            VodRecipeRequest {
                request: &req,
                encoding: Some(Arc::clone(encoding)),
            },
            file,
            &settings(),
            VodAttribution {
                user_name: "test",
                item_title: "AAC join",
                supersession_user: "test",
            },
            session.clone(),
        )
        .await
        .expect("pair session");
    let first = fetched_bytes(serve, &session, &segment_name(u64::from(entry))).await;
    let next = fetched_bytes(serve, &session, &segment_name(u64::from(entry + 1))).await;
    let init = fetched_bytes(serve, &session, "init.mp4").await;
    let rendition = rendition_of(serve, &session).await;
    assert!(rendition.failure().is_none(), "{:?}", rendition.failure());
    serve.end(&session, Terminal::Deleted).await;
    rendition.gen_epoch.fetch_add(1, Relaxed);
    let _ = rendition
        .slot
        .perform(
            Step::Terminate {
                why: Termination::Idle,
            },
            || {},
        )
        .await;
    let mut manifest = rendition.manifest.lock().await;
    let freed = rendition
        .dir
        .make_room(&mut manifest, &[], u64::MAX)
        .await
        .expect("force fresh generation");
    sub_saturating(&serve.shared.working_set, freed.bytes);
    (init, first, next)
}

fn audio_interval(init: &[u8], media: &[u8]) -> (u64, u64) {
    let mut reader = FragmentReader::new();
    reader.push(init);
    reader.push(media);
    let mut audio = None;
    let mut interval = None;
    while let Some(unit) = reader.next_unit().expect("parse served AAC timeline") {
        match unit {
            Unit::Init(init) => {
                audio = init
                    .tracks
                    .iter()
                    .find(|track| track.kind == plurx_core::fmp4::TrackKind::Audio)
                    .map(|track| track.id)
            }
            Unit::Fragment(fragment) => {
                if let Some(track) = audio.and_then(|id| fragment.track(id)) {
                    assert!(interval.is_none(), "one muxed audio interval per segment");
                    interval = Some((
                        track.base_decode_time,
                        track.base_decode_time + track.duration(),
                    ));
                }
            }
            _ => {}
        }
    }
    interval.expect("AAC must not disappear at a restarted video boundary")
}

async fn decoded_audio(path: &Path, init: &[u8], first: &[u8], next: &[u8]) -> Vec<f32> {
    tokio::fs::write(path, [init, first, next].concat())
        .await
        .expect("independently assembled GETs");
    let output = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-ac", "1", "-f", "f32le", "-"])
        .kill_on_drop(true)
        .output()
        .await
        .expect("decode joined AAC");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
        .stdout
        .chunks_exact(4)
        .map(|sample| f32::from_le_bytes(sample.try_into().expect("float sample")))
        .collect()
}

#[tokio::test]
async fn encoded_vod_aac_is_continuous_across_independently_regenerated_neighbors() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("AAC continuity fixture");
    let (file, encoding) = encoded_fixture(base.path()).await;
    let serve = bare_serve(&base.path().join("renditions"));
    for entry in [0, 44] {
        let (init, old_first, old_next) = encoded_pair(&serve, &file, &encoding, entry).await;
        let (new_init, new_next, following) =
            encoded_pair(&serve, &file, &encoding, entry + 1).await;
        let (reverse_init, regenerated_first, _) =
            encoded_pair(&serve, &file, &encoding, entry).await;
        assert_eq!(init, new_init);
        assert_eq!(init, reverse_init);
        let first_interval = audio_interval(&init, &old_first);
        let next_interval = audio_interval(&init, &old_next);
        assert_eq!(
            first_interval.1, next_interval.0,
            "continuous producer audio boundary"
        );
        assert_eq!(
            audio_interval(&init, &new_next),
            next_interval,
            "regeneration must not reset AAC phase or packet assignment"
        );
        assert_eq!(audio_interval(&init, &regenerated_first), first_interval);
        assert_eq!(next_interval.1, audio_interval(&init, &following).0);
        let baseline = decoded_audio(
            &base.path().join("audio-baseline.mp4"),
            &init,
            &old_first,
            &old_next,
        )
        .await;
        for (label, left, right) in [
            ("forward", &old_first, &new_next),
            ("reverse", &regenerated_first, &old_next),
        ] {
            let joined = decoded_audio(
                &base.path().join(format!("audio-{label}.mp4")),
                &init,
                left,
                right,
            )
            .await;
            assert_eq!(
                joined.len(),
                baseline.len(),
                "no duplicated or missing decoded samples at {entry}/{label}"
            );
            let join = (first_interval.1 - first_interval.0) as usize;
            let low = join.saturating_sub(2048);
            let high = (join + 4096).min(joined.len());
            let square_error = baseline[low..high]
                .iter()
                .zip(&joined[low..high])
                .map(|(left, right)| f64::from(left - right).powi(2))
                .sum::<f64>();
            let rms = (square_error / (high - low) as f64).sqrt();
            let alignment_start = join + 2048;
            let alignment_end = (join + 4096).min(joined.len() - 64);
            let (sample_lag, aligned_square_error) = (-64isize..=64)
                .map(|lag| {
                    let error = (alignment_start..alignment_end)
                        .map(|index| {
                            f64::from(
                                baseline[index]
                                    - joined
                                        [index.checked_add_signed(lag).expect("positive window")],
                            )
                            .powi(2)
                        })
                        .sum::<f64>();
                    (lag, error)
                })
                .min_by(|left, right| left.1.total_cmp(&right.1))
                .expect("bounded AAC alignment search");
            let aligned_rms =
                (aligned_square_error / (alignment_end - alignment_start) as f64).sqrt();
            // Independently restarted AAC encoders can expose a small,
            // decoder-version-specific priming displacement even though the
            // fMP4 decode timeline and decoded sample count are identical.
            // Those exact interval and length assertions above own continuity;
            // this comparison proves the neighboring fragment still carries
            // the same audio rather than demanding one decoder's priming.
            assert!(
                sample_lag.unsigned_abs() <= 64 && aligned_rms < 0.005,
                "AAC content changed at {entry}/{label}: unaligned RMS {rms}, best sample lag {sample_lag}, aligned RMS {aligned_rms}"
            );
        }
    }
}

#[tokio::test]
async fn encoded_vod_ntsc_gets_decode_after_forward_and_backward_restarts() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("encoded fixture");
    let (file, encoding) = encoded_fixture(base.path()).await;
    assert_encoded_restarts(base.path(), file, encoding).await;
}

/// Explicit qualification benchmark, not part of the fast unit lane. The
/// seed is loop-remuxed into a real two-hour indexed Matroska file: seeking
/// near its end must decode two hours of AAC, not a ninety-second proxy.
#[tokio::test]
#[ignore = "explicit two-hour audio preparation benchmark"]
async fn encoded_vod_two_hour_audio_restart_budget() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("long audio benchmark");
    let (mut file, mut encoding) = encoded_fixture(base.path()).await;
    let surround = base.path().join("surround-seed.mkv");
    let seed = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"]).arg(&file.path)
        .args(["-f", "lavfi", "-i", "aevalsrc=0.08*sin(2*PI*311*t)|0.08*sin(2*PI*440*t)|0.08*sin(2*PI*523*t)|0.08*sin(2*PI*61*t)|0.08*sin(2*PI*659*t)|0.08*sin(2*PI*733*t):s=48000", "-map", "0:v:0", "-map", "1:a:0", "-t", "96", "-c:v", "copy", "-c:a", "aac", "-b:a", "384k"])
        .arg(&surround).kill_on_drop(true).output().await.expect("surround seed");
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let long = base.path().join("two-hour.mkv");
    let extended = tokio::process::Command::new(ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-stream_loop",
            "-1",
            "-i",
        ])
        .arg(&surround)
        .args(["-t", "7200", "-map", "0", "-c", "copy"])
        .arg(&long)
        .kill_on_drop(true)
        .output()
        .await
        .expect("real two-hour source");
    assert!(
        extended.status.success(),
        "{}",
        String::from_utf8_lossy(&extended.stderr)
    );
    file.path = long;
    file.duration_ms = Some(7_200_000);
    file.audio_streams[0].channels = Some(6);
    let metadata = std::fs::metadata(&file.path).expect("source metadata");
    file.size = metadata.len() as i64;
    file.mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    let serve = bare_serve(&base.path().join("renditions"));
    let mutable = Arc::get_mut(&mut encoding).expect("unique recipe");
    mutable.source_object_version = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .expect("long source identity")
        .object_version()
        .to_owned();
    refresh_encoded_plan(&file, mutable);
    let target = 3594.0 * 2.002;
    let args = encoding.args(&file, target, 7200.0);
    let inputs = args
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == "-i").then_some(index))
        .collect::<Vec<_>>();
    let audio_input = *inputs.get(1).expect("separate audio input");
    assert_eq!(args[audio_input - 2], "-ss");
    let audio_seek = args[audio_input - 1]
        .parse::<f64>()
        .expect("numeric audio seek");
    assert!(
        audio_seek > target - 5.0,
        "the audio input must seek near the requested film position, not decode from zero: {audio_seek}"
    );
    #[cfg(unix)]
    let cpu_before = child_cpu_seconds();
    let started = std::time::Instant::now();
    let (init, media, _) = encoded_pair(&serve, &file, &encoding, 3594).await;
    let elapsed = started.elapsed();
    #[cfg(unix)]
    eprintln!(
        "two-hour production child CPU: {:.3} seconds",
        child_cpu_seconds() - cpu_before
    );
    let decoded = decoded_audio(&base.path().join("long-get.mp4"), &init, &media, &[]).await;
    assert!(!decoded.is_empty());
    eprintln!("two-hour / 5.1 AAC / local warm file / seek {target:.3}s / {} source bytes / first two GETs + reap {:?}", file.size, elapsed);
    assert!(
        elapsed < Duration::from_secs(25),
        "preparation needs headroom under the 30-second materialization deadline: {elapsed:?}"
    );
}

#[cfg(unix)]
fn child_cpu_seconds() -> f64 {
    // SAFETY: getrusage writes the complete C rusage structure supplied here.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) },
        0
    );
    (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0
}

async fn assert_encoded_restarts(
    base: &Path,
    file: MediaFile,
    mut encoding: Arc<crate::vodencode::Encoding>,
) {
    let digest = if let Some(subtitle) = &encoding.subtitle {
        Some(
            crate::vodencode::digest_subtitle(subtitle)
                .await
                .expect("held burn identity"),
        )
    } else {
        None
    };
    Arc::get_mut(&mut encoding)
        .expect("new frozen fixture recipe")
        .subtitle_digest = digest;
    let serve = bare_serve(&base.join("renditions"));
    let mut init = None;
    let mut key = None;
    for (ordinal, target) in [0.0, 90.09, 3.0].into_iter().enumerate() {
        let session = format!("encoded-{ordinal}");
        let mut req = request(&session, target);
        req.kind = SessionKind::Transcode { height: 144 };
        req.subtitle_burn = encoding
            .options
            .subtitle_burn
            .as_ref()
            .map(|burn| burn.subtitle_index);
        req.audio_offset_ms = file.audio_offset_ms;
        let started = serve
            .try_create(
                VodRecipeRequest {
                    request: &req,
                    encoding: Some(Arc::clone(&encoding)),
                },
                &file,
                &settings(),
                VodAttribution {
                    user_name: "test",
                    item_title: "NTSC",
                    supersession_user: "test",
                },
                session.clone(),
            )
            .await
            .expect("encoded VOD attaches without a copy index");
        assert_eq!(started.session_id, session);
        let rendition = rendition_of(&serve, &session).await;
        if let Some(key) = &key {
            assert_eq!(&rendition.key, key);
        } else {
            key = Some(rendition.key.clone());
        }
        let entry = entry_containing(&rendition.plan, target);
        let media = fetched_bytes(&serve, &session, &segment_name(u64::from(entry))).await;
        let head = fetched_bytes(&serve, &session, "init.mp4").await;
        if let Some(init) = &init {
            assert_eq!(&head, init, "restart init must be byte-identical");
        } else {
            init = Some(head.clone());
        }
        let output = base.join(format!("served-{ordinal}.mp4"));
        tokio::fs::write(&output, [head, media].concat())
            .await
            .expect("persist actual GET bytes");
        let decoded = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
            .arg(&output)
            .args(["-map", "0:v", "-map", "0:a", "-f", "null", "-"])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode fetched init + segment");
        assert!(
            decoded.status.success(),
            "GET decode at {target}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let mut reader = FragmentReader::new();
        reader.push(&tokio::fs::read(&output).await.expect("served media"));
        let mut video_id = None;
        let mut audio_id = None;
        while let Some(unit) = reader.next_unit().expect("parse actual GET bytes") {
            match unit {
                Unit::Init(init) => {
                    video_id = init.video().map(|video| video.id);
                    audio_id = init
                        .tracks
                        .iter()
                        .find(|track| track.kind == plurx_core::fmp4::TrackKind::Audio)
                        .map(|track| track.id);
                }
                Unit::Fragment(fragment) => {
                    if let Some(video) = video_id.and_then(|id| fragment.track(id)) {
                        let expected = rendition.plan.entry(entry).expect("entry");
                        assert_eq!(video.base_decode_time, expected.start_ticks);
                        assert_eq!(video.duration(), expected.duration_ticks);
                        assert_eq!(
                            video.sample_count(),
                            encoding.grid.frames_per_segment as usize
                        );
                    }
                    if let Some(audio) = audio_id.and_then(|id| fragment.track(id)) {
                        let expected = rendition.plan.entry(entry).expect("entry");
                        let boundary =
                            expected.start_ticks * 48000 / u64::from(rendition.plan.timescale);
                        assert_eq!(
                            audio.base_decode_time,
                            boundary.div_ceil(1024) * 1024,
                            "AAC must retain its global sample phase"
                        );
                    }
                }
                Unit::Trailer => {}
            }
        }
        // A seek attaches at the containing segment boundary, which can
        // precede the requested film time. With VFR input, the fps filter may
        // legitimately choose a preceding source frame for that boundary.
        // Inspect the frame at the actual seek target so this assertion tests
        // destination content rather than the segment's leading preroll.
        let entry_start_seconds = rendition.plan.entry(entry).expect("entry").start_ticks as f64
            / f64::from(rendition.plan.timescale);
        let marker_offset = (target - entry_start_seconds).max(0.0);
        let marker_filter = format!(
            "select='gte(t,{marker_offset:.9})',crop=2:2:8:8,format=rgb24"
        );
        let pixel = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&output)
            .args([
                "-vf",
                &marker_filter,
                "-frames:v",
                "1",
                "-an",
                "-f",
                "rawvideo",
                "-",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode destination marker");
        assert!(pixel.status.success());
        assert_eq!(pixel.stdout.len(), 12, "one 2x2 RGB marker frame");
        let channel = [0, 2, 1][ordinal];
        assert!(
            pixel.stdout[channel] > 70
                && pixel.stdout[(channel + 1) % 3] < 35
                && pixel.stdout[(channel + 2) % 3] < 35,
            "seek must show destination content, not merely relabel old frame timestamps: {:?}",
            pixel.stdout
        );
        let frames = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&output)
            .args(["-map", "0:v", "-an", "-f", "framemd5", "-"])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode every output frame");
        assert!(frames.status.success());
        let checksums = String::from_utf8(frames.stdout).expect("frame checksums");
        let distinct = checksums
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.rsplit(',').next())
            .collect::<std::collections::HashSet<_>>();
        assert!(
            distinct.len() >= encoding.grid.frames_per_segment as usize / 3,
            "encoder preroll must not appear as duplicated/frozen video: {checksums}"
        );
        assert!(rendition.failure().is_none(), "{:?}", rendition.failure());
        // Cancel/reap the in-flight producer, then evict materialized media so
        // the same immutable URI must be regenerated on the following seek.
        serve.end(&session, Terminal::Deleted).await;
        rendition.gen_epoch.fetch_add(1, Relaxed);
        let _ = rendition
            .slot
            .perform(
                Step::Terminate {
                    why: Termination::Idle,
                },
                || {},
            )
            .await;
        let mut manifest = rendition.manifest.lock().await;
        let freed = rendition
            .dir
            .make_room(&mut manifest, &[], u64::MAX)
            .await
            .expect("evict after reader detach");
        sub_saturating(&serve.shared.working_set, freed.bytes);
    }
    assert_eq!(
        encoding.admissions.software_in_use(),
        0,
        "every cancelled encoder was reaped before releasing capacity"
    );
}

#[tokio::test]
#[ignore = "run serially by the Rust gate; real FFmpeg restart fixture"]
async fn encoded_vod_vfr_input_is_sampled_on_the_declared_rational_grid() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("VFR fixture");
    let (mut file, mut encoding) = encoded_fixture(base.path()).await;
    let path = base.path().join("vfr.mkv");
    let output = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file.path)
        .args([
            "-vf",
            "fps=30000/1001,select='if(lt(t,80),not(mod(n,2)),1)'",
            "-fps_mode:v",
            "vfr",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-threads",
            "2",
            "-c:a",
            "copy",
        ])
        .arg(&path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("generate variable frame intervals");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    file.path = path;
    let metadata = std::fs::metadata(&file.path).expect("VFR metadata");
    file.size = metadata.len() as i64;
    file.mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    let probe = plurx_core::scan::probe::probe(&file.path)
        .await
        .expect("probe VFR");
    Arc::get_mut(&mut encoding).expect("unique recipe").grid =
        crate::vodencode::frame_grid(probe.raw_json.as_deref()).expect("declared output cadence");
    let mutable = Arc::get_mut(&mut encoding).expect("unique recipe");
    mutable.source_object_version = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .expect("VFR source identity")
        .object_version()
        .to_owned();
    refresh_encoded_plan(&file, mutable);
    let packets = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(&file.path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("source packet intervals");
    assert!(packets.status.success());
    let times = String::from_utf8(packets.stdout)
        .expect("times")
        .lines()
        .filter_map(|line| line.trim().parse::<f64>().ok())
        .collect::<Vec<_>>();
    let gaps = times
        .windows(2)
        .map(|pair| ((pair[1] - pair[0]) * 100.0).round() as i64)
        .collect::<std::collections::HashSet<_>>();
    assert!(gaps.len() > 1, "fixture must really be VFR: {gaps:?}");
    assert_encoded_restarts(base.path(), file, encoding).await;
}

#[tokio::test]
#[ignore = "run serially by the Rust gate; real FFmpeg restart fixture"]
async fn encoded_vod_bitmap_burn_restores_cues_that_predate_video_seek_landing() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    let base = crate::test_tempdir().expect("bitmap fixture");
    let (mut file, mut encoding) = encoded_fixture(base.path()).await;
    let fixture = include_bytes!("../../../fuzz/corpus/inspect_sup/mkpgs-1920x1080.sup");
    let mut sup = Vec::new();
    for (start, end) in [(1000u32, 2000u32), (2000, 6000), (80000, 95000)] {
        let mut cursor = 0;
        while cursor + 13 <= fixture.len() {
            assert_eq!(&fixture[cursor..cursor + 2], b"PG");
            let pts = u32::from_be_bytes(fixture[cursor + 2..cursor + 6].try_into().expect("PTS"));
            let len = usize::from(u16::from_be_bytes(
                fixture[cursor + 11..cursor + 13]
                    .try_into()
                    .expect("PGS length"),
            )) + 13;
            if pts == 90_000 || pts == 630_000 {
                let mut segment = fixture[cursor..cursor + len].to_vec();
                segment[2..6]
                    .copy_from_slice(&(if pts == 90_000 { start } else { end } * 90).to_be_bytes());
                sup.extend(segment);
            }
            cursor += len;
        }
    }
    let sup_path = base.path().join("long-cue.sup");
    tokio::fs::write(&sup_path, sup)
        .await
        .expect("PGS display sets");
    let path = base.path().join("bitmap.mkv");
    let output = tokio::process::Command::new(ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-copyts",
            "-start_at_zero",
            "-i",
        ])
        .arg(&file.path)
        .args(["-f", "sup", "-i"])
        .arg(&sup_path)
        .args(["-map", "0", "-map", "1:s", "-c", "copy"])
        .arg(&path)
        .kill_on_drop(true)
        .output()
        .await
        .expect("mux actual PGS source");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    file.path = path;
    let metadata = std::fs::metadata(&file.path).expect("bitmap metadata");
    file.size = metadata.len() as i64;
    file.mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix mtime")
        .as_secs() as i64;
    file.subtitle_streams = vec![plurx_core::domain::SubtitleStream {
        index: 0,
        codec: "hdmv_pgs_subtitle".into(),
        ..Default::default()
    }];
    let subtitle =
        crate::subtitles::ensure_burn_file(
            &base.path().join("subtitles"),
            &file,
            0,
            None,
            crate::subtitles::SIDECAR_JOIN_UNBOUNDED,
        )
            .await
            .expect("production bitmap extraction");
    let frozen = Arc::get_mut(&mut encoding).expect("unique recipe");
    frozen.source_object_version = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .expect("PGS source identity")
        .object_version()
        .to_owned();
    frozen.options.subtitle_burn = Some(plurx_core::transcode::SubtitleBurn {
        subtitle_index: 0,
        bitmap: true,
    });
    frozen.options.subtitle_file = Some("/dev/fd/5".into());
    frozen.subtitle = Some(Arc::new(subtitle));
    refresh_encoded_plan(&file, frozen);
    assert_encoded_restarts(base.path(), file, encoding).await;
    for ordinal in 0..3 {
        let decoded = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(base.path().join(format!("served-{ordinal}.mp4")))
            .args([
                "-vf",
                "crop=iw:40:0:ih-40,format=gray",
                "-frames:v",
                "1",
                "-an",
                "-f",
                "rawvideo",
                "-",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode PGS pixels");
        assert!(decoded.status.success());
        let bright = decoded.stdout.iter().filter(|pixel| **pixel > 180).count();
        if ordinal == 0 {
            assert!(
                bright < 10,
                "the sidecar's first cue at one second must not be shifted to zero: {bright}"
            );
        } else {
            assert!(
                bright > 100,
                "long PGS cue must survive seek {ordinal}: {bright}"
            );
        }
    }
}

#[tokio::test]
async fn encoded_vod_manual_audio_correction_keeps_restart_init_stable() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    for offset in [-250, 250] {
        let base = crate::test_tempdir().expect("audio offset fixture");
        let (mut file, mut encoding) = encoded_fixture(base.path()).await;
        file.audio_offset_ms = offset;
        refresh_encoded_plan(
            &file,
            Arc::get_mut(&mut encoding).expect("unique recipe"),
        );
        assert_encoded_restarts(base.path(), file, encoding).await;
    }
}

#[tokio::test]
async fn encoded_vod_text_burn_gets_keep_absolute_cue_time_after_seek() {
    let _campaign = ENCODED_INTEGRATION_CAMPAIGN.lock().await;
    testfixtures::require_ffmpeg();
    if !ffmpeg_has_filters(&["subtitles"]).await {
        eprintln!(
            "skipping encoded_vod_text_burn_gets_keep_absolute_cue_time_after_seek: \
             `{}` has no subtitles filter",
            ffmpeg_bin()
        );
        return;
    }
    let base = crate::test_tempdir().expect("burn fixture");
    let (mut file, mut encoding) = encoded_fixture(base.path()).await;
    let path = base.path().join("burn.vtt");
    tokio::fs::write(&path, "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nFIRST CUE\n\n00:00:02.000 --> 00:00:06.000\nBACKWARD CUE\n\n00:01:20.000 --> 00:01:35.000\nLONG FORWARD CUE\n").await.expect("subtitle sidecar");
    file.subtitle_streams = vec![plurx_core::domain::SubtitleStream {
        index: 0,
        codec: "webvtt".into(),
        ..Default::default()
    }];
    let encoding_mut = Arc::get_mut(&mut encoding).expect("unique recipe");
    encoding_mut.options.subtitle_burn = Some(plurx_core::transcode::SubtitleBurn {
        subtitle_index: 0,
        bitmap: false,
    });
    encoding_mut.options.subtitle_file = Some("/dev/fd/5".into());
    encoding_mut.subtitle = Some(Arc::new(
        std::fs::File::open(path).expect("subtitle handle"),
    ));
    refresh_encoded_plan(&file, encoding_mut);
    assert_encoded_restarts(base.path(), file, encoding).await;
    // The source's entire bottom band is black. Cues began before the
    // decoder preroll; white pixels there prove the correct absolute-time
    // cue was actually rendered instead of merely named in the recipe.
    for ordinal in 0..3 {
        let output = base.path().join(format!("served-{ordinal}.mp4"));
        let decoded = tokio::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(output)
            .args([
                "-vf",
                "crop=iw:40:0:ih-40,format=gray",
                "-frames:v",
                "1",
                "-an",
                "-f",
                "rawvideo",
                "-",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .expect("decode burned pixels");
        assert!(decoded.status.success());
        let bright = decoded.stdout.iter().filter(|pixel| **pixel > 180).count();
        assert!(bright > 0, "the selected absolute-time cue must be visible on the otherwise black band after seek {ordinal}: {bright} bright pixels, maximum {:?}", decoded.stdout.iter().max());
    }
}
