//! Actual native Source HTTP/producer lifetime; no replication fixture is linked.
use super::*;
use plurx_core::{
    config::Config,
    domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
    sharing::{new_secret, secret_hash, InvitationRecord, SecretDomain, ShareClaim, SourceId},
    sharing_catalogue_details::CatalogueRevisionKey,
    storage::mode::StorageMode,
    store::{keys, sharing_catalogue_details::SourceDetailsRead},
};
use serde_json::{json, Value};

struct HttpOwner {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>>,
}
impl Drop for HttpOwner {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl HttpOwner {
    async fn finish(mut self) {
        self.stop
            .take()
            .expect("owned stop")
            .send(())
            .expect("running accept owner");
        let task = self.task.as_mut().expect("owned accept task");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("bounded accept drain")
                .expect("accept task joined")
                .expect("accept drained"),
            crate::HttpDrain::Complete
        );
        self.task.take();
    }
}

struct NativeOwner {
    source: plurx_core::sharing_source_sessions::LocalSourceAuthority,
    shutdown: tokio_util::sync::CancellationToken,
}
impl Drop for NativeOwner {
    fn drop(&mut self) {
        self.source.revoke();
        self.shutdown.cancel();
    }
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    secret: &str,
    request: &Value,
) -> reqwest::Response {
    client
        .post(url)
        .header("authorization", format!("CinemaShare {secret}"))
        .header("cinemashare-viewer", "c".repeat(64))
        .header("connection", "close")
        .json(request)
        .send()
        .await
        .expect("actual native HTTP response")
}
async fn successful_bytes(response: reqwest::Response) -> bytes::Bytes {
    let status = response.status();
    let body = response.bytes().await.expect("actual response body closed");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(body.len() <= 4 * 1024 * 1024);
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_source_http_produces_bytes_and_revocation_closes_actual_owner() {
    let _budget = TranscodeManager::source_fixture_body_budget().await;
    let directory = crate::test_tempdir().expect("owned native Source root");
    let mut config = Config::default();
    config.storage.mode = Some(StorageMode::Sqlite);
    config.storage.data_dir = directory.path().join("data");
    config.server.bind = "127.0.0.1:0".parse().expect("loopback HTTP");
    // Ports remain occupied for the entire producer/HTTP lifetime. No cluster
    // startup, private API or transport can secretly supply this authority.
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied cluster API port");
    config.cluster.raft_bind = raft.local_addr().expect("occupied address");
    config.cluster.api_bind = api.local_addr().expect("occupied address");
    let native = plurx_core::storage::native::open_native_store(&config)
        .await
        .expect("actual exclusive native Store");
    native
        .store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved Source choice");
    native
        .store
        .put_setting(keys::SW_POOL_THREADS, "4")
        .await
        .expect("actual local capacity");
    let source = native
        .sqlite
        .prepare_native_source_before_serving(&native.identity.node_id, &native.credential_key)
        .await
        .expect("actual pre-serving native Source factory");
    let services = crate::storage_runtime::local_services(&native, &config);
    let state = crate::build_state(
        &config,
        native.identity.node_id.clone(),
        native.identity.instance_id.clone(),
        Arc::clone(&native.credential_key),
        StorageMode::Sqlite,
        Some(source.clone()),
        services.settings_changes,
        services.backup,
        #[cfg(feature = "cluster")]
        None,
        #[cfg(feature = "cluster")]
        None,
        plurx_core::store::CatalogueReader::authority(Arc::clone(&native.store)),
        Arc::clone(&native.store),
        crate::create_dirs_for_config(&config).expect("actual native directories"),
        Default::default(),
        Default::default(),
        crate::logbuf::LogBuffers {
            general: Arc::new(crate::logbuf::LogBuffer::new(32)),
            cluster: Arc::new(crate::logbuf::LogBuffer::new(32)),
            sharing: Arc::new(crate::logbuf::LogBuffer::new(32)),
        },
    );
    let _native_owner = NativeOwner {
        source: source.clone(),
        shutdown: state.shutdown.clone(),
    };
    assert!(!state.is_replicated());
    assert!(!state.serving.is_quorum_managed());
    let store = &state.store;
    let library = store
        .create_library(&NewLibrary {
            name: "Native Source".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("actual library")
        .id;
    let item = store
        .insert_item(&NewItem {
            library_id: library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Actual native Source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("actual item");
    let file = directory.path().join("source.mp4");
    let generated = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=s=128x72:r=24",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-t",
            "2",
            "-y",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual FFmpeg fixture process");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual FFprobe process");
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let document: Value = serde_json::from_slice(&probe.stdout).expect("actual probe JSON");
    let probe = plurx_core::scan::probe::parse_probe_json(&document);
    let metadata = std::fs::metadata(&file).expect("actual source file");
    let mtime = metadata
        .modified()
        .expect("actual mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let file_id = store
        .upsert_file(
            item,
            file.to_str().expect("fixture path"),
            metadata.len() as i64,
            mtime,
            &probe,
        )
        .await
        .expect("actual scanned file Store boundary");
    let media = store
        .get_file(file_id)
        .await
        .expect("actual file read")
        .expect("file");
    let crate::fragindex::IndexOutcome::Built(index) = Box::pin(crate::fragindex::build(
        &media,
        plurx_core::transcode::CopyVideoOptions::new(false, false),
        directory.path(),
        Duration::from_secs(30),
    ))
    .await
    else {
        panic!("actual fragment index");
    };
    store
        .put_fragment_index(file_id, &index)
        .await
        .expect("actual scan index");
    let now = crate::fragment_index_cluster::unix_ms();
    let identity = store
        .sharing_identity(now)
        .await
        .expect("actual Source identity");
    let grant = uuid::Uuid::new_v4();
    let invitation = uuid::Uuid::new_v4();
    let secret = new_secret().expect("actual grant secret");
    let hash = secret_hash(SecretDomain::Grant, &secret);
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![library],
            created_at_ms: now,
            expires_at_ms: now + 60_000,
        })
        .await
        .expect("invitation");
    store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: uuid::Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: uuid::Uuid::new_v4(),
            recipient_name: "Actual receiver".into(),
            credential_hash: hash.clone(),
            now_ms: now + 1,
        })
        .await
        .expect("actual grant claim");
    store
        .approve_share(grant, 1, now + 2)
        .await
        .expect("approved actual scope");
    let SourceDetailsRead::Authorized(witness) = store
        .source_item_file_witness(
            &hash,
            grant,
            SourceId::parse(&item.to_string()).expect("item ID"),
            SourceId::parse(&file_id.to_string()).expect("file ID"),
        )
        .await
        .expect("actual witness")
    else {
        panic!("authorized actual file");
    };
    let envelope = store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .expect("actual native key read")
        .expect("startup-owned Source key");
    let key = CatalogueRevisionKey::open(&native.credential_key, identity.clone(), &envelope)
        .expect("open actual Source key");
    let reference = crate::http::hls::SourcePlaybackTarget {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        library_id: SourceId::parse(&library.to_string()).expect("library ID"),
        item_id: SourceId::parse(&item.to_string()).expect("item ID"),
        file_id: SourceId::parse(&file_id.to_string()).expect("file ID"),
        revision: key.file_revision(&witness).expect("actual revision"),
    };
    let request_id = uuid::Uuid::new_v4();
    let mut recipe = json!({"reference": reference, "session": {"playback_id":"native-source", "request_id":request_id,
        "copy":true,"height":72,"quality_auto":false,"presentation":"vod","caps":{"v":2,
        "video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],
        "containers":["mp4"],"transports":["hls","progressive"]}}});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("actual HTTP listener");
    let address = listener.local_addr().expect("actual HTTP address");
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let owner = HttpOwner {
        stop: Some(stop),
        task: Some(tokio::spawn(crate::serve_http(
            listener,
            crate::http::sharing::peer_router(state.clone()),
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ))),
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .timeout(Duration::from_secs(45))
        .build()
        .expect("actual client");
    let path = format!("http://{address}/sharing/v1/items/{item}/files/{file_id}/sessions");
    let bytes = successful_bytes(post(&client, &path, secret.expose(), &recipe).await).await;
    let _started = crate::http::decode_source_start_response(&bytes, &reference)
        .expect("actual strict Start response");
    let envelope: serde_json::Value =
        serde_json::from_slice(&bytes).expect("actual Start envelope");
    let incarnation = uuid::Uuid::parse_str(
        envelope["incarnation_id"]
            .as_str()
            .expect("actual incarnation"),
    )
    .expect("UUID");
    let actor = {
        let entries = state
            .transcode
            .source_workers
            .entries
            .lock()
            .expect("actual worker registry");
        assert_eq!(entries.len(), 1, "actual producer attachment");
        SourceViewerActor(Arc::clone(&entries[0]))
    };
    assert!(actor.settlement_status().is_none());
    assert_eq!(actor.0.assignment.binding().incarnation_id(), incarnation);
    recipe["incarnation_id"] = json!(incarnation);
    recipe["session_id"] = envelope["response"]["session_id"].clone();
    recipe["control_epoch"] = envelope["response"]["control"]["control_epoch"].clone();
    let resources = format!("{path}/{request_id}/resources");
    recipe["resource"] = json!("index.m3u8");
    let playlist =
        successful_bytes(post(&client, &resources, secret.expose(), &recipe).await).await;
    let playlist = std::str::from_utf8(&playlist).expect("actual playlist text");
    let segment = playlist
        .lines()
        .find(|line| !line.starts_with('#') && !line.is_empty())
        .expect("actual media URI")
        .to_owned();
    for resource in ["init.mp4", segment.as_str()] {
        recipe["resource"] = json!(resource);
        let media =
            successful_bytes(post(&client, &resources, secret.expose(), &recipe).await).await;
        assert!(!media.is_empty(), "actual first media bytes for {resource}");
    }
    // Revoke the actual native factory capability, then wait for physical,
    // owned-driver and SQL settlement through the production actor. No ACK or
    // expired application lease substitutes for closure.
    source.revoke();
    assert!(
        state.source_admission_members().await.is_err(),
        "revoked native capability cannot acquire a replacement admission"
    );
    tokio::time::timeout(Duration::from_secs(30), actor.retire())
        .await
        .expect("bounded actual retirement")
        .expect("actual producer/driver/SQL closure");
    assert_eq!(actor.settlement_status(), Some(Ok(())));
    assert!(state
        .transcode
        .source_workers
        .entries
        .lock()
        .expect("registry")
        .is_empty());
    assert_eq!(state.transcode.admissions.software_in_use(), 0);
    let row = store
        .media_session_route_by_incarnation(&incarnation.to_string())
        .await
        .expect("actual terminal route")
        .expect("retained route");
    assert_ne!(row.state, "active");
    assert!(!post(&client, &resources, secret.expose(), &recipe)
        .await
        .status()
        .is_success());
    state.shutdown.cancel();
    owner.finish().await;
    native
        .sqlite
        .close()
        .await
        .expect("actual native database closure after producer/driver settlement");
    assert!(
        state.store.get_file(file_id).await.is_err(),
        "retained native HTTP State cannot reopen SQL after shutdown"
    );
    for name in ["hiqlite", "membership.json", "secret_raft", "secret_api"] {
        assert!(
            !config.storage.data_dir.join(name).exists(),
            "native producer created {name}"
        );
    }
}
