use super::*;

fn known(metric: &Metric<u64>) -> String {
    match metric {
        Metric::Known(value) => format!("known {value}"),
        Metric::Unknown(reason) => format!("unknown ({reason:?})"),
    }
}

/// A local direct play on a node opened the way the daemon opens it: the
/// real selected Store, its startup custody, and a fresh capacity refresh.
/// The request is the one `open_local_direct` makes. A refusal names every
/// precondition the direct source checks before reservation that this node
/// does not meet, beside the accounting snapshot it was decided against.
#[tokio::test]
async fn local_direct_play_prepares_on_the_real_node_runtime() {
    let node = crate::test_runtime::RealRuntime::open().await;
    let runtime = node.handle();
    let root = crate::test_tempdir().expect("direct play root");
    let path = root.path().join("direct-play.bin");
    std::fs::write(&path, vec![0x5a_u8; 256 * 1024]).expect("direct play source");
    let metadata = std::fs::metadata(&path).expect("direct play metadata");
    let file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path,
        size: metadata.len() as i64,
        mtime: metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix mtime")
            .as_secs() as i64,
        duration_ms: None,
        container: None,
        video_codec: None,
        video_codec_tag: None,
        field_order: None,
        video_profile: None,
        width: None,
        height: None,
        bit_depth: None,
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: None,
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: Default::default(),
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    let epoch = runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("this node observes its admission capacity");
    // The request `open_local_direct` makes; it must fit its bounds here.
    let _request = TranscodeManager::local_direct_request(
        DirectSourceBinding::Local {
            authenticated_user_id: 7,
            file_id: file.id,
        },
        &file,
        epoch,
    )
    .expect("the local direct request fits its bounds");

    let snapshot = runtime.accounting().expect("ledger accounting");
    let os_boot = &runtime.owner().os_boot;
    let mut unmet = Vec::new();
    if snapshot.limit_epoch != epoch {
        unmet.push(format!(
            "request epoch {epoch} is not the ledger epoch {}",
            snapshot.limit_epoch
        ));
    }
    if !matches!(snapshot.capacity.cpu_ns_per_second, Metric::Known(value) if value > 0) {
        unmet.push("the read domain needs known, positive node CPU capacity".to_owned());
    }
    if !matches!(
        os_boot,
        plurx_core::resource_custody::BootIdentity::Known { provider, .. } if provider.len() <= 128
    ) {
        unmet.push("the filesystem domain needs a known boot identity".to_owned());
    }
    if let Err(error) = std::fs::metadata("/proc/self/ns/mnt") {
        unmet.push(format!("the mount namespace is unreadable: {error}"));
    }
    let context = format!(
        "unmet preconditions: {unmet:?}; ledger epoch {}, cpu {}, memory {}, kernel tasks {}, \
         handles {}, observation age {} ms, os_boot {os_boot:?}",
        snapshot.limit_epoch,
        known(&snapshot.capacity.cpu_ns_per_second),
        known(&snapshot.capacity.memory_bytes),
        known(&snapshot.capacity.kernel_tasks),
        known(&snapshot.capacity.handles),
        known(&snapshot.capacity_observation_age_ms),
    );

    // Open it the way a direct-play route does: the daemon's state holds this
    // node's runtime, and `open_local_direct` wraps the lease, starts its
    // original worker and only then opens the source.
    let store: Arc<dyn plurx_core::store::Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("direct play store"));
    let state = crate::state::AppState::new_with_runtime(
        "test".into(),
        store,
        crate::state::Dirs {
            artwork: root.path().join("artwork"),
            transcode: root.path().join("transcode"),
            cache: root.path().join("cache"),
            subs: root.path().join("subs"),
            runtime_cache: root.path().join("runtime"),
            renditions: root.path().join("renditions"),
        },
        "test-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(64)),
        Some(Arc::clone(&runtime)),
    );
    let manager = &state.transcode;
    let opened = match manager
        .open_local_direct(
            &state,
            DirectSourceBinding::Local {
                authenticated_user_id: 7,
                file_id: file.id,
            },
            &file,
            deadline,
        )
        .await
    {
        Ok(opened) => opened,
        Err(error) => panic!("local direct open refused: {error:?}; {context}"),
    };
    assert_eq!(
        opened.length, file.size as u64,
        "the opened source is the scanned file"
    );
    assert_eq!(manager.fixture_local_direct_custody(), (1, 0));

    drop(opened);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            manager.fixture_drive_local_direct();
            if manager.fixture_local_direct_custody() == (0, 0) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the direct play settles on the node it was charged to");
    assert_eq!(
        runtime.accounting().expect("ledger accounting").used,
        ResourceVector::default(),
        "the settled play leaves no charge behind"
    );
}
