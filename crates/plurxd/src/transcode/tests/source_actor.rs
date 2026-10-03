use super::*;

/// This fixture boots the production one-voter selector. Candidate installation
/// and explicit capability seeding are test setup, not rolling-upgrade proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_preadmission_owns_real_capacity_before_activation_and_queue() {
    Box::pin(source_copy_preadmission_fixture()).await;
}

async fn source_copy_preadmission_fixture() {
    use crate::http::hls::{prepare_source_playback, CreateSession, SourcePlaybackTarget};
    use plurx_core::{
        cluster::{membership::*, migration::select_daemon_store},
        config::Config,
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::{new_secret, secret_hash, InvitationRecord, SecretDomain, ShareClaim, SourceId},
        sharing_catalogue_details::CatalogueRevisionKey,
        sharing_source_sessions::*,
        store::{
            sharing_catalogue_details::SourceDetailsRead, MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA,
        },
    };
    let directory = crate::test_tempdir().expect("actual Source root");
    eprintln!("Source fixture: actual standalone selection");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    let selected = Box::pin(select_daemon_store(&config))
        .await
        .expect("actual standalone voter");
    let client = selected.local_client().expect("actual local client");
    let master = Arc::clone(&selected.credential_key);
    let membership = selected.membership_manager();
    membership
        .prepare_purpose_master(Arc::clone(&master))
        .await
        .expect("actual selected master");
    let mut state = crate::http::source_actor_test_state();
    eprintln!("Source fixture: state constructed without Router");
    state.store = Arc::clone(&selected.store);
    state.membership = membership.clone();
    state.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&master),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    let store = &state.store;
    store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved choice");
    store
        .put_setting(keys::SW_POOL_THREADS, "4")
        .await
        .expect("actual budget");
    let identity = store.sharing_identity(1000).await.expect("identity");
    let library = store
        .create_library(&NewLibrary {
            name: "Source".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    let mut ddl = plurx_core::store::sharing_catalogue_source::candidate_statements();
    ddl.extend(plurx_core::store::sharing_catalogue_source::candidate_item_identity_statements());
    ddl.push(plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA.into());
    ddl.extend(
        MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
            .split("-- next statement\n")
            .map(|sql| sql.trim().trim_end_matches(';').to_owned()),
    );
    ddl.extend(plurx_core::store::sharing_source_sessions::candidate_statements());
    ddl.extend(sharing_member_admission_guard_schema());
    for sql in ddl {
        client
            .execute(sql, hiqlite::params!())
            .await
            .expect("exact candidate fixture installation");
    }
    let item = store
        .insert_item(&NewItem {
            library_id: library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Actual Source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let file = directory.path().join("source.mp4");
    let generated = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=s=128x72:r=24",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual fixture FFmpeg");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let metadata = std::fs::metadata(&file).expect("actual Source facts");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-of",
            "json",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual scan fixture probe");
    assert!(probe.status.success());
    let probe = String::from_utf8(probe.stdout).expect("actual scan JSON");
    client.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate,probe_json,scanned_at) VALUES(1,$1,$2,$3,$4,2000,'mp4','h264',128,72,8,100000,$5,$6)",hiqlite::params!(item,file.to_string_lossy().to_string(),metadata.len() as i64,mtime,probe,crate::fragment_index_cluster::unix_ms()/1000)).await.expect("actual file facts");
    let envelope =
        CatalogueRevisionKey::generate_sealed(&master, identity.clone()).expect("Source key");
    let key = CatalogueRevisionKey::open(&master, identity.clone(), &envelope).expect("open key");
    client
        .execute(
            "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)",
            hiqlite::params!(
                identity.server_id.to_string(),
                identity.catalogue_epoch.to_string(),
                envelope.as_stored().to_owned()
            ),
        )
        .await
        .expect("actual sealed key");
    let now = crate::fragment_index_cluster::unix_ms();
    client
        .execute(
            "UPDATE cluster_nodes SET last_seen_at=$1 WHERE node_id=$2",
            hiqlite::params!(now, selected.identity.node_id.clone()),
        )
        .await
        .expect("fixture heartbeat");
    for capability in [
        SHARING_SESSION_PRINCIPAL_CAPABILITY.to_owned(),
        SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY.to_owned(),
        SHARING_PURPOSE_KEYS_CAPABILITY.to_owned(),
        format!(
            "sharing_purpose_master_v1:{}",
            master.sharing_purpose_master_fingerprint()
        ),
    ] {
        client.execute("INSERT INTO cluster_node_capabilities(node_id,capability,last_seen_at) VALUES($1,$2,$3) ON CONFLICT(node_id,capability) DO UPDATE SET last_seen_at=excluded.last_seen_at",hiqlite::params!(selected.identity.node_id.clone(),capability,now)).await.expect("explicit actual fixture capability");
    }
    let grant = uuid::Uuid::new_v4();
    let invitation = uuid::Uuid::new_v4();
    let secret = new_secret().expect("fixture secret");
    let hash = secret_hash(SecretDomain::Grant, &secret);
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![library],
            created_at_ms: now,
            expires_at_ms: now + 60000,
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
        .expect("grant");
    store
        .approve_share(grant, 1, now + 2)
        .await
        .expect("scope approval");
    let SourceDetailsRead::Authorized(witness) = store
        .source_item_file_witness(
            &hash,
            grant,
            SourceId::parse(&item.to_string()).expect("item ID"),
            SourceId::parse("1").expect("file ID"),
        )
        .await
        .expect("witness")
    else {
        panic!("authorized witness")
    };
    let actual_envelope = store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .expect("actual stored key read")
        .expect("actual stored Source key");
    assert_eq!(
        actual_envelope.as_stored(),
        envelope.as_stored(),
        "fixture cannot replace a winning Source key"
    );
    let actual_key =
        CatalogueRevisionKey::open(&state.sharing.key, identity.clone(), &actual_envelope)
            .expect("actual SharingManager key open");
    let revision = actual_key
        .file_revision(&witness)
        .expect("actual stored-key revision");
    assert_eq!(
        key.file_revision(&witness).expect("generated-key revision"),
        revision
    );
    let target = SourcePlaybackTarget {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        library_id: SourceId::parse(&library.to_string()).expect("library ID"),
        item_id: SourceId::parse(&item.to_string()).expect("item ID"),
        file_id: SourceId::parse("1").expect("file ID"),
        revision: revision.clone(),
    };
    assert!(witness.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id
    ));
    assert!(crate::sharing::enabled(store.as_ref())
        .await
        .expect("saved switch read"));
    assert!(store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .expect("actual key read")
        .is_some());
    assert!(store
        .playback_planning_snapshot(1, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .expect("actual planning read")
        .is_some());
    let body:CreateSession = serde_json::from_value(serde_json::json!({"playback_id":"copy-source","request_id":"copy-source-request","copy":true,"height":72,"quality_auto":false,"presentation":"vod","caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}})).expect("actual Source request");
    assert_eq!(
        body.caps.as_ref().expect("v2 caps").v,
        plurx_core::playback::DeviceCaps::VERSION
    );
    assert!(!body.caps.as_ref().expect("v2 caps").is_empty());
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("CinemaShare {}", secret.expose())
            .parse()
            .expect("header"),
    );
    headers.insert(
        "cinemashare-viewer",
        "c".repeat(64).parse().expect("viewer"),
    );
    let preparation = Box::pin(prepare_source_playback(&state, &headers, target, body));
    eprintln!(
        "Source preparation future: {} bytes",
        std::mem::size_of_val(preparation.as_ref().get_ref())
    );
    let prepared = preparation.await.expect("actual Source engine preparation");
    let SourceIntentRead::Ready(intent) = store
        .prepare_source_session_intent(
            SourceSessionRequest {
                principal: prepared.principal().clone(),
                request_id: "copy-source-request".into(),
                request_fingerprint: prepared.fingerprint().into(),
                playback_id: prepared.request().playback_id.clone(),
                incarnation_id: uuid::Uuid::new_v4(),
                now_ms: now,
                claim_expires_at_ms: now + 60000,
                credential_hash: hash,
                item_id: SourceId::parse(&item.to_string()).expect("item ID"),
                file_id: SourceId::parse("1").expect("file ID"),
                file_revision: revision,
            },
            &master,
        )
        .await
        .expect("current Source intent")
    else {
        panic!("intent")
    };
    let members = membership
        .observe_source_admission_members()
        .await
        .expect("actual observation")
        .expect("actual floor");
    let SourceClaimOutcome::Acquired(binding) = store
        .claim_source_media_session(&intent, &members)
        .await
        .expect("claim")
    else {
        panic!("canonical claim")
    };
    let assignment = store
        .assign_source_dispatch(&binding, &master, &members)
        .await
        .expect("assignment")
        .expect("actual local assignment");
    let media = store.get_file(1).await.expect("file read").expect("file");
    let crate::fragindex::IndexOutcome::Built(index) = Box::pin(crate::fragindex::build(
        &media,
        plurx_core::transcode::CopyVideoOptions::new(false, false),
        directory.path(),
        Duration::from_secs(30),
    ))
    .await
    else {
        panic!("actual scan index")
    };
    store
        .put_fragment_index(1, &index)
        .await
        .expect("actual scan index retained");
    let manager = TranscodeManager::new(
        Arc::clone(store),
        directory.path().join("workers"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let settings = manager
        .vod_settings(prepared.request())
        .await
        .expect("settings")
        .expect("enabled VOD");
    let admission = Box::pin(manager.vod.prepare_admitted_source_copy(
        &prepared,
        &assignment,
        &settings,
        &manager.admissions,
        store.as_ref(),
        Instant::now() + Duration::from_secs(10),
    ));
    let bytes = std::mem::size_of_val(admission.as_ref().get_ref());
    eprintln!("Source copy admission future: {bytes} bytes");
    assert!(
        bytes <= 32 * 1024,
        "owned Source preadmission future must stay bounded"
    );
    let admitted = admission.await.expect("actual preadmission");
    admitted.assert_no_demand_or_child().await;
    assert_eq!(manager.admissions.software_in_use(), 4);
    assert!(manager.vod.session_ids().await.is_empty());
    assert!(store
        .media_session_route_by_incarnation(&binding.incarnation_id().to_string())
        .await
        .expect("route read")
        .is_none());
    drop(admitted);
    assert_eq!(manager.admissions.software_in_use(), 0);
    assert_eq!(
        store
            .settle_source_assigned_without_activation(&assignment)
            .await
            .expect("owned actual no-activation fixture cleanup"),
        SourceReleaseOutcome::Released
    );
    selected.shutdown().await.expect("actual voter shutdown");
}

#[tokio::test]
async fn source_start_budget_uses_actual_vod_settings_and_admission_policy() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("actual settings store"));
    let directory = crate::test_tempdir().expect("work directory");
    let manager = TranscodeManager::new(
        Arc::clone(&store),
        directory.path().to_owned(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let request = SessionRequest {
        quality_catalog: None,
        candidate_context: None,
        control_sequence: None,
        file_id: 1,
        playback_id: "source-start-budget".into(),
        request_id: None,
        automatic: false,
        previous_session_id: None,
        reopen_reason: None,
        kind: SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        },
        start_seconds: 0.0,
        audio_index: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        hdr10: false,
        presentation: Default::default(),
        block_budget_secs: Some(0.001),
        transport: None,
    };
    for (stored, seconds) in [
        (None, 30),
        (Some("42"), 42),
        (Some("999"), 300),
        (Some("NaN"), 30),
    ] {
        if let Some(value) = stored {
            store
                .put_setting(keys::VOD_MATERIALIZE_BUDGET_SECS, value)
                .await
                .expect("set materialization budget");
        }
        assert_eq!(
            manager
                .source_start_budget_for_request(&request)
                .await
                .expect("budget"),
            crate::admission::QUEUE_WAIT + Duration::from_secs(seconds)
        );
    }
    store
        .put_setting(keys::VOD_PRESENTATION, " 0 ")
        .await
        .expect("maintenance");
    assert!(manager
        .source_start_budget_for_request(&request)
        .await
        .is_err());
    assert!(
        manager.vod.session_ids().await.is_empty(),
        "budget observation allocates no session"
    );
    assert_eq!(manager.admissions.software_in_use(), 0);
}
