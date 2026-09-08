// Included in transcode::tests: production normalization, public create,
// immutable serving, and the native HLS wrapper must agree about one recipe.

#[tokio::test]
async fn encoded_vod_manager_create_resolves_real_recipe_and_served_codecs() {
    use plurx_core::store::SqliteStore;
    use tokio::io::AsyncReadExt;
    let base = crate::test_tempdir().expect("manager fixture");
    let source = plurx_core::testfixtures::source("h264");
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("real source probe");
    let metadata = std::fs::metadata(&source).expect("source metadata");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            file.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("attested source metadata");
    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let req = SessionRequest {
        request_id: None,
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 240 },
        ..reopen_request(file_id, "encoded-manager", "unused", "unused")
    };
    let start = manager
        .create_session(&req, "test")
        .await
        .expect("public encoded create");
    assert_eq!(start.target_height, 240);
    assert_eq!(start.kind, SessionKind::Transcode { height: 240 });
    assert_eq!(start.encoder, Encoder::Software.label());
    assert!(start.vod);
    let mut bytes = Vec::new();
    for name in ["init.mp4", "seg00000.m4s"] {
        let response = manager
            .vod_segment(&start.session_id, name)
            .await
            .expect("VOD route");
        let mut ready = response
            .result
            .expect("materialized")
            .expect("planned resource");
        ready
            .file
            .read_to_end(&mut bytes)
            .await
            .expect("actual GET file");
    }
    let output = base.path().join("manager-get.mp4");
    tokio::fs::write(&output, bytes)
        .await
        .expect("served bytes");
    let decoded = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
        .arg(output)
        .args(["-f", "null", "-"])
        .kill_on_drop(true)
        .output()
        .await
        .expect("decode actual GET");
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let HlsPresentationResolution::Ready(context, file, _) = manager
        .hls_presentation_before(&start.session_id, Instant::now() + Duration::from_secs(5))
        .await
    else {
        panic!("frozen HLS presentation");
    };
    assert!(context.codecs.starts_with("avc1."));
    assert_eq!(file.height, Some(240));
    assert_eq!(file.hdr, None);
    assert_eq!(context.frame_rate, Some(24.0));
    manager.stop_session(&start.session_id, "test").await;
}

#[tokio::test]
async fn encoded_vod_manager_refuses_replaced_source_with_stale_probe() {
    use plurx_core::store::SqliteStore;
    let base = crate::test_tempdir().expect("stale source fixture");
    let source = base.path().join("source.nut");
    let replacement = base.path().join("replacement.nut");
    for (path, geometry) in [(&source, "64x64"), (&replacement, "128x32")] {
        let output = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=black:s={geometry}:r=24"),
                "-t",
                "2",
                "-c:v",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-threads",
                "1",
                "-f",
                "nut",
            ])
            .arg(path)
            .kill_on_drop(true)
            .output()
            .await
            .expect("generate source");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let source_len = std::fs::metadata(&source).expect("source size").len();
    let replacement_len = std::fs::metadata(&replacement)
        .expect("replacement size")
        .len();
    if source_len != replacement_len {
        use std::io::Write;
        let (shorter, difference) = if source_len < replacement_len {
            (&source, replacement_len - source_len)
        } else {
            (&replacement, source_len - replacement_len)
        };
        std::fs::OpenOptions::new()
            .append(true)
            .open(shorter)
            .expect("shorter fixture")
            .write_all(&vec![0; difference as usize])
            .expect("equalize fixture length");
    }
    let probe = plurx_core::scan::probe::probe(&source)
        .await
        .expect("original probe");
    let metadata = std::fs::metadata(&source).expect("original metadata");
    assert_eq!(
        metadata.len(),
        std::fs::metadata(&replacement)
            .expect("replacement metadata")
            .len(),
        "fixture must evade the scanner's size field"
    );
    let modified = metadata.modified().expect("original mtime");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let file_id =
        seed_file_with_probe_at(&store, source.to_str().expect("path"), probe.clone()).await;
    let file = store
        .get_file(file_id)
        .await
        .expect("file")
        .expect("seeded");
    store
        .upsert_file(
            file.item_id,
            source.to_str().expect("path"),
            metadata.len() as i64,
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix mtime")
                .as_secs() as i64,
            &probe,
        )
        .await
        .expect("stored original probe");
    std::fs::rename(replacement, &source).expect("replace source");
    std::fs::File::options()
        .write(true)
        .open(&source)
        .expect("replacement handle")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("preserve scanner mtime");

    let manager = TranscodeManager::new(
        store,
        base.path().join("manager"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let request = SessionRequest {
        request_id: None,
        previous_session_id: None,
        reopen_reason: None,
        presentation: Presentation::Vod,
        automatic: false,
        start_seconds: 0.0,
        kind: SessionKind::Transcode { height: 32 },
        ..reopen_request(file_id, "stale-probe", "unused", "unused")
    };
    let error = match manager.create_session(&request, "test").await {
        Ok(_) => panic!("stale scan facts must not create a recipe"),
        Err(error) => error,
    };
    assert!(error.contains("vod_source_rescan_required"), "{error}");
    assert!(manager.vod.session_ids().await.is_empty());
}
