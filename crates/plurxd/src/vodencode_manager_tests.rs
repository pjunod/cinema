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
