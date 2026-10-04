use super::*;

/// This fixture boots the production one-voter selector. Candidate installation
/// and explicit capability seeding are test setup, not rolling-upgrade proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_preadmission_owns_real_capacity_before_activation_and_queue() {
    Box::pin(source_copy_preadmission_fixture(0)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_attachment_registers_current_producer_and_reaps_owned_permit() {
    Box::pin(source_copy_preadmission_fixture(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_actor_survives_waiter_cancellation_and_holds_capacity_through_body_retirement()
{
    Box::pin(source_copy_preadmission_fixture(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_actor_failed_physical_admission_releases_only_owned_no_spawn_assignment() {
    Box::pin(source_copy_preadmission_fixture(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_actor_stops_owned_renewal_after_actual_viewer_idle_reap() {
    Box::pin(source_copy_preadmission_fixture(4)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_actor_builds_cold_index_under_actual_owned_admission() {
    Box::pin(source_copy_preadmission_fixture(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_cold_index_owned_child_survives_cancelled_waiter_and_revoke() {
    Box::pin(source_copy_preadmission_fixture(6)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_cold_index_refuses_sharing_off_before_actual_child() {
    Box::pin(source_copy_preadmission_fixture(7)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_cold_index_retains_actual_permit_after_injected_wait_failure() {
    Box::pin(source_copy_preadmission_fixture(8)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_copy_cold_index_refuses_expired_original_observation_before_child() {
    Box::pin(source_copy_preadmission_fixture(9)).await;
}

fn source_fixture_state() -> Arc<crate::state::AppState> {
    Arc::new(crate::http::source_actor_test_state())
}
fn source_fixture_store(
    config: &plurx_core::config::Config,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    plurx_core::cluster::migration::SelectedStore,
                    plurx_core::error::StoreError,
                >,
            > + Send
            + '_,
    >,
> {
    Box::pin(plurx_core::cluster::migration::select_daemon_store(config))
}

async fn source_copy_preadmission_fixture(mode: u8) {
    use crate::http::hls::{prepare_source_playback, CreateSession, SourcePlaybackTarget};
    use plurx_core::{
        cluster::membership::*,
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
    let selected = source_fixture_store(&config)
        .await
        .expect("actual standalone voter");
    let client = selected.local_client().expect("actual local client");
    let master = Arc::clone(&selected.credential_key);
    let membership = selected.membership_manager();
    membership
        .prepare_purpose_master(Arc::clone(&master))
        .await
        .expect("actual selected master");
    let mut state = source_fixture_state();
    let state_mut = Arc::get_mut(&mut state).expect("sole fixture state");
    eprintln!("Source fixture: state constructed without Router");
    state_mut.store = Arc::clone(&selected.store);
    state_mut.membership = membership.clone();
    state_mut.sharing = Arc::new(crate::sharing::SharingManager::new(
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
    let file = directory.path().join(if mode >= 21 {
        "source.mkv"
    } else {
        "source.mp4"
    });
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
    if mode >= 21 {
        let caption = directory.path().join("actual.srt");
        std::fs::write(
            &caption,
            "1\n00:00:00,200 --> 00:00:01,800\nActual Source caption\n\n",
        )
        .expect("actual embedded caption");
        let muxed = directory.path().join("captioned.mkv");
        let result = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .arg("-i")
            .arg(&file)
            .arg("-i")
            .arg(&caption)
            .args([
                "-map", "0:v:0", "-map", "1:s:0", "-c:v", "copy", "-c:s", "subrip", "-y",
            ])
            .arg(&muxed)
            .output()
            .await
            .expect("actual subtitle mux");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::rename(muxed, &file).expect("actual captioned source");
    }
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
    if mode >= 21 {
        client
            .execute(
                "UPDATE files SET container='matroska' WHERE id=1",
                hiqlite::params!(),
            )
            .await
            .expect("actual Source container");
        let tracks = serde_json::to_string(&vec![plurx_core::domain::SubtitleStream {
            index: 0,
            codec: "subrip".into(),
            language: None,
            title: None,
            default: true,
            forced: false,
            hearing_impaired: false,
        }])
        .expect("actual scanned track");
        client
            .execute(
                "UPDATE files SET subtitle_streams=$1 WHERE id=1",
                hiqlite::params!(tracks),
            )
            .await
            .expect("actual embedded subtitle facts");
    }
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
    let mut body:CreateSession = serde_json::from_value(serde_json::json!({"playback_id":"copy-source","request_id":"copy-source-request","copy":true,"height":72,"quality_auto":false,"presentation":"vod","caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}})).expect("actual Source request");
    if (12..=20).contains(&mode) || mode == 29 || mode == 31 {
        body.copy = Some(false);
        body.height = Some(if mode == 31 { 144 } else { 36 });
    }
    if mode >= 21 {
        body.native_subtitles = Some(true);
        body.subtitle = Some(0);
    }
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
    if (12..=20).contains(&mode) || mode == 29 || mode == 31 {
        assert!(
            matches!(prepared.request().kind, SessionKind::Transcode { .. }),
            "actual common preparation resolves an encoded recipe"
        );
    }
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
    if mode < 5 {
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
    } else {
        assert!(store
            .fragment_index(
                1,
                &crate::fragindex::identity_for(
                    &media,
                    plurx_core::transcode::CopyVideoOptions::new(false, false)
                )
            )
            .await
            .expect("actual cold index read")
            .is_none());
    }
    let manager = Arc::new(TranscodeManager::new(
        Arc::clone(store),
        directory.path().join("workers"),
        EncoderCaps::default(),
        Pipeline::Cpu,
    ));
    if mode >= 5 {
        source_actual_actor_boxed(state, manager, prepared, assignment, mode, client.clone()).await;
        selected.shutdown().await.expect("actual voter shutdown");
        return;
    }
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
    if mode == 1 {
        source_actual_copy_attachment_boxed(&state, &manager, admitted, &prepared, &assignment)
            .await;
        selected.shutdown().await.expect("actual voter shutdown");
        return;
    }
    drop(admitted);
    assert_eq!(manager.admissions.software_in_use(), 0);
    if (2..=4).contains(&mode) {
        source_actual_actor_boxed(state, manager, prepared, assignment, mode, client.clone()).await;
        selected.shutdown().await.expect("actual voter shutdown");
        return;
    }
    assert_eq!(
        store
            .settle_source_assigned_without_activation(&assignment)
            .await
            .expect("owned actual no-activation fixture cleanup"),
        SourceReleaseOutcome::Released
    );
    selected.shutdown().await.expect("actual voter shutdown");
}

fn source_actual_copy_attachment_boxed<'a>(
    state: &'a crate::state::AppState,
    manager: &'a TranscodeManager,
    admitted: crate::vodserve::AdmittedSourceVodRendition,
    prepared: &'a crate::http::hls::PreparedSourcePlayback,
    assignment: &'a plurx_core::sharing_source_sessions::SourceDispatchAssignment,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    let future = source_actual_copy_attachment(state, manager, admitted, prepared, assignment);
    eprintln!(
        "Source attachment future bytes: {}",
        std::mem::size_of_val(&future)
    );
    Box::pin(future)
}

async fn source_actual_copy_attachment(
    state: &crate::state::AppState,
    manager: &TranscodeManager,
    admitted: crate::vodserve::AdmittedSourceVodRendition,
    prepared: &crate::http::hls::PreparedSourcePlayback,
    assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
) {
    let store = &state.store;
    let membership = &state.membership;
    let master = &state.sharing.key;
    let binding = assignment.binding();
    use plurx_core::{
        domain::{MediaSessionActivation, MediaSessionEnd, MEDIA_SESSION_PUBLICATION_BLOCKED},
        sharing_source_sessions::{
            SourcePublicationAuthorityRead, SourceReleaseOutcome, SourceWriteAuthorityRead,
        },
    };
    let mut reserved = manager
        .vod
        .reserve_source_vod(admitted)
        .expect("owned preactivation association");
    let session_id = uuid::Uuid::new_v4().to_string();
    let facts = reserved
        .start_info(&session_id)
        .expect("actual admitted facts");
    let info = facts.start_info();
    assert_eq!(info.target_height, 72);
    assert_eq!(info.media_origin_seconds, 0.0);
    assert_eq!(info.encoder, "vod");
    let members = membership
        .observe_source_admission_members()
        .await
        .expect("fresh first observation")
        .expect("current floor");
    let SourceWriteAuthorityRead::Ready(authority) = store
        .prepare_source_activation_authority(assignment, master, &members)
        .await
        .expect("fresh activation")
    else {
        panic!("activation authority")
    };
    let response =
        Box::pin(prepared.start_response(state, info, &binding.incarnation_id().to_string(), 1))
            .await
            .expect("complete actual Source start response");
    let now = crate::fragment_index_cluster::unix_ms();
    let activation = MediaSessionActivation {
        incarnation_id: binding.incarnation_id().to_string(),
        session_id: session_id.clone(),
        principal: binding.principal().clone(),
        playback_id: prepared.request().playback_id.clone(),
        recovery_epoch: String::new(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: Some(binding.request_id().into()),
        request_fingerprint: prepared.fingerprint().into(),
        owner_node_id: assignment.owner_node_id().into(),
        recipe_json: serde_json::to_string(prepared.request()).expect("actual recipe"),
        response_json: serde_json::to_string(&response).expect("complete Source response"),
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 60000,
        expected_desired_revision: None,
    };
    let outcome = store
        .activate_source_media_session(&authority, &activation)
        .await
        .expect("blocked activation")
        .expect("actual active route");
    assert_eq!(
        outcome.route.publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    assert!(manager.vod.session_ids().await.is_empty());
    let gate = Arc::new(SourceProducerAuthority {
        store: Arc::clone(store),
        membership: membership.clone(),
        master: Arc::clone(master),
    });
    authority
        .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
        .expect("original postcommit clock");
    let start = Box::pin(manager.vod.commit_source_vod(
        &mut reserved,
        &session_id,
        &gate,
        &manager.admissions,
    ))
    .await
    .expect("actual admitted attachment");
    assert_eq!(start.session_id, session_id);
    let physical = Box::pin(reserved.wait_ready(Instant::now() + Duration::from_secs(10)))
        .await
        .expect("actual registered init readiness");
    assert!(physical.matches(assignment));
    let members = membership
        .observe_source_admission_members()
        .await
        .expect("fresh publication observation")
        .expect("current publication floor");
    let SourcePublicationAuthorityRead::Ready(publication) = store
        .prepare_source_publication_authority(assignment, master, &members)
        .await
        .expect("current publication")
    else {
        panic!("publication authority")
    };
    let ready = store
        .complete_source_media_session_publication(&publication)
        .await
        .expect("coupled readiness")
        .expect("published Source route");
    assert_eq!(ready.publication_ready_at_ms, 0);
    let ended = store
        .end_media_session_if_owner(&MediaSessionEnd {
            incarnation_id: ready.incarnation_id.clone(),
            session_id: ready.session_id.clone(),
            expected_owner_node_id: ready.owner_node_id.clone(),
            expected_owner_epoch: ready.owner_epoch,
            expected_lease_expires_at_ms: ready.lease_expires_at_ms,
            terminal_reason: "deleted".into(),
            now_ms: crate::fragment_index_cluster::unix_ms(),
        })
        .await
        .expect("exact terminal Store")
        .expect("ended Source route");
    let settled = Box::pin(reserved.retire(&manager.vod))
        .await
        .expect("actual producer and writers barrier");
    assert!(settled.matches(assignment));
    assert_eq!(manager.admissions.software_in_use(), 0);
    assert_eq!(
        store
            .settle_source_terminal_worker(assignment, &ended)
            .await
            .expect("postreap SQL permission"),
        SourceReleaseOutcome::Released
    );
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

fn source_actual_actor_boxed(
    state: Arc<crate::state::AppState>,
    manager: Arc<TranscodeManager>,
    prepared: crate::http::hls::PreparedSourcePlayback,
    assignment: SourceDispatchAssignment,
    mode: u8,
    client: hiqlite::Client,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(source_actual_actor(
        state, manager, prepared, assignment, mode, client,
    ))
}
async fn source_actual_actor(
    state: Arc<crate::state::AppState>,
    manager: Arc<TranscodeManager>,
    prepared: crate::http::hls::PreparedSourcePlayback,
    assignment: SourceDispatchAssignment,
    mode: u8,
    client: hiqlite::Client,
) {
    if matches!(mode, 3 | 19 | 28) {
        state
            .store
            .put_setting(keys::SW_POOL_THREADS, "3")
            .await
            .expect("actual insufficient saved CPU budget");
    }
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .expect("actual members")
        .expect("actual floor");
    let SourceWriteAuthorityRead::Ready(activation) = state
        .store
        .prepare_source_activation_authority(&assignment, &state.sharing.key, &members)
        .await
        .expect("actual initial observation")
    else {
        panic!("activation hint")
    };
    let index_pause = match mode {
        6 => Some(manager.source_workers.index_hooks.pause_after_spawn()),
        7 | 9 | 10 | 11 => Some(manager.source_workers.index_hooks.pause_before_spawn()),
        8 => Some(
            manager
                .source_workers
                .index_hooks
                .pause_after_wait_failure(),
        ),
        13 => Some(manager.source_workers.probe_hooks.pause_after_spawn()),
        14 | 16 | 17 | 18 => Some(manager.source_workers.probe_hooks.pause_before_spawn()),
        15 => Some(
            manager
                .source_workers
                .probe_hooks
                .pause_after_wait_failure(),
        ),
        20 => Some(manager.source_workers.probe_hooks.pause_after_evidence()),
        22 => Some(manager.source_workers.native_hooks.pause_after_spawn()),
        23 | 25 | 26 | 27 => Some(manager.source_workers.native_hooks.pause_before_spawn()),
        24 => Some(
            manager
                .source_workers
                .native_hooks
                .pause_after_wait_failure(),
        ),
        _ => None,
    };
    let actor = Box::pin(manager.start_source_worker(
        Arc::clone(&state),
        assignment.clone(),
        *activation,
        prepared,
        Instant::now() + Duration::from_secs(15),
    ))
    .await
    .expect("owned Source task");
    let joined = manager
        .lookup_source_worker(&assignment)
        .expect("full assignment lookup");
    assert!(Arc::ptr_eq(&actor.0, &joined.0));
    if let Some(pause) = index_pause {
        let held = pause.reached().await;
        assert_eq!(
            manager.admissions.software_in_use(),
            if mode == 20 { 0 } else { 4 }
        );
        assert!(manager.vod.session_ids().await.is_empty());
        assert!(state
            .store
            .media_session_route_by_incarnation(&assignment.binding().incarnation_id().to_string())
            .await
            .expect("no preactivation media route")
            .is_none());
        let pid = if mode >= 21 {
            manager.source_workers.native_hooks.spawned_pid()
        } else if (12..=20).contains(&mode) || mode == 29 || mode == 31 {
            manager.source_workers.probe_hooks.spawned_pid()
        } else {
            manager.source_workers.index_hooks.spawned_pid()
        };
        if matches!(mode, 6 | 8 | 13 | 15 | 20 | 22 | 24) {
            assert!(pid > 0, "actual Source index child started");
        } else {
            assert_eq!(pid, 0, "refusal point precedes any actual child");
        }
        assert!(matches!(
            actor.wait_ready(Instant::now()).await,
            Err(SourceWorkerError::Deadline)
        ));
        drop(joined); // A disconnected waiter cannot abandon the scan owner.
        if matches!(mode, 6 | 7 | 13 | 14 | 22 | 23) {
            state
                .store
                .put_setting(keys::SHARING_ENABLED, "false")
                .await
                .expect("actual saved sharing off");
        }
        if mode == 20 {
            state
                .store
                .put_setting(keys::SW_POOL_THREADS, "0")
                .await
                .expect("actual encoder capacity removed after confirmed probe settlement");
        }
        if matches!(mode, 10 | 17 | 26) {
            let file = state
                .store
                .get_file(1)
                .await
                .expect("file")
                .expect("actual file");
            std::fs::write(&file.path, b"changed physical Source before scan")
                .expect("actual held Source object drift");
        }
        if matches!(mode, 11 | 18 | 27) {
            client
                .execute(
                    "DELETE FROM cluster_node_capabilities WHERE capability=$1",
                    hiqlite::params!(
                        plurx_core::cluster::membership::SHARING_PURPOSE_KEYS_CAPABILITY
                    ),
                )
                .await
                .expect("actual purpose capability disappearance after observation");
        }
        if matches!(mode, 9 | 16 | 25) {
            tokio::time::sleep(Duration::from_millis(5100)).await;
        }
        assert_eq!(
            manager.admissions.software_in_use(),
            if mode == 20 { 0 } else { 4 },
            "probe capacity matches actual settlement stage"
        );
        assert_eq!(actor.settlement_status(), None);
        drop(held);
        assert!(actor
            .wait_ready(Instant::now() + Duration::from_secs(10))
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(10), actor.retire())
            .await
            .expect("actual index retirement budget")
            .expect("confirmed scan/no-media-producer cleanup");
        assert_eq!(manager.admissions.software_in_use(), 0);
        assert!(manager.lookup_source_worker(&assignment).is_none());
        let file = state
            .store
            .get_file(1)
            .await
            .expect("retained file")
            .expect("actual file");
        assert!(state
            .store
            .fragment_index(
                1,
                &crate::fragindex::identity_for(
                    &file,
                    plurx_core::transcode::CopyVideoOptions::new(false, false)
                )
            )
            .await
            .expect("refused index publication")
            .is_none());
        assert_eq!(
            state
                .store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("already physically settled exact replay"),
            SourceReleaseOutcome::ExactReplay
        );
        return;
    }
    if matches!(mode, 3 | 19 | 28) {
        assert!(actor
            .wait_ready(Instant::now() + Duration::from_secs(10))
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(10), actor.retire())
            .await
            .expect("owned no-spawn cleanup budget")
            .expect("actual admission failed without a child");
        assert_eq!(manager.admissions.software_in_use(), 0);
        assert!(state
            .store
            .media_session_route_by_incarnation(&assignment.binding().incarnation_id().to_string())
            .await
            .expect("no provisional route")
            .is_none());
        assert!(manager.vod.session_ids().await.is_empty());
        assert!(manager.lookup_source_worker(&assignment).is_none());
        assert_eq!(
            state
                .store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("already settled exact SQL replay"),
            SourceReleaseOutcome::ExactReplay
        );
        return;
    }
    // A cancelled response wait leaves the independently owned start intact.
    assert!(matches!(
        actor.wait_ready(Instant::now()).await,
        Err(SourceWorkerError::Deadline)
    ));
    let response = actor
        .wait_ready(Instant::now() + Duration::from_secs(15))
        .await
        .expect("actual published actor");
    assert!(response.control.is_some());
    if (12..=20).contains(&mode) || mode == 29 || mode == 31 {
        let value = serde_json::to_value(&response).expect("complete encoded response");
        assert_eq!(value["height"], 72);
        assert_eq!(
            value["encoder"],
            plurx_core::transcode::Encoder::Software.label()
        );
        let segment = actor
            .open_resource(
                &SharingHlsResource::parse("seg00000.m4s").expect("typed actual encoded segment"),
                Instant::now() + Duration::from_secs(10),
            )
            .await
            .expect("actual encoded segment");
        let (payload, guard) = segment.into_parts();
        let SourceResourcePayload::File(file) = payload else {
            panic!("encoded media file")
        };
        assert!(file.len > 0);
        drop(file);
        drop(guard);
    }

    #[cfg(unix)]
    match mode {
        5 => manager
            .source_workers
            .index_hooks
            .assert_closed_parent_settlements(),
        12 => manager
            .source_workers
            .probe_hooks
            .assert_closed_parent_settlements(),
        21 => manager
            .source_workers
            .native_hooks
            .assert_closed_parent_settlements(),
        _ => {}
    }

    if mode == 5 {
        let file = state
            .store
            .get_file(1)
            .await
            .expect("actual file")
            .expect("retained file");
        let index = state
            .store
            .fragment_index(
                1,
                &crate::fragindex::identity_for(
                    &file,
                    plurx_core::transcode::CopyVideoOptions::new(false, false),
                ),
            )
            .await
            .expect("actual Source scan result")
            .expect("complete cold index retained");
        assert!(!index.rows.is_empty());
    }
    let incarnation = assignment.binding().incarnation_id().to_string();
    let route = state
        .store
        .media_session_route_by_incarnation(&incarnation)
        .await
        .expect("published route")
        .expect("retained route");
    assert_eq!(route.publication_ready_at_ms, 0);
    assert_eq!(route.session_id, response.session_id);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&route.response_json)
            .expect("actual retained DTO"),
        serde_json::to_value(&response).expect("actual complete DTO")
    );
    if (36..=41).contains(&mode) {
        let baseline = manager
            .vod
            .source_control_observation_for_test(&response.session_id)
            .await
            .expect("actual activity");
        if mode == 36 {
            let opened = actor
                .open_status(Instant::now() + Duration::from_secs(10))
                .await
                .expect("actual guarded Source status");
            let (status, held) = opened.into_parts();
            let value = serde_json::to_value(&status).expect("closed telemetry");
            assert!(value.get("file_id").is_none());
            assert!(value.get("id").is_none());
            assert!(value.get("producer_failed").is_none());
            assert!(value.get("final").is_some());
            assert_eq!(value["playlist_shape"], "vod");
            assert_eq!(value["target_height"], response.height);
            assert_eq!(
                manager
                    .vod
                    .source_control_observation_for_test(&response.session_id)
                    .await
                    .expect("activity after status"),
                baseline
            );
            actor.request_retirement();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(
                actor.settlement_status().is_none(),
                "actual status Body holds retirement"
            );
            drop(held);
            actor.retire().await.expect("actual status retirement");
            return;
        }
        let pause = actor.0.status_hooks.pause(matches!(mode, 39 | 41));
        let owned = actor.clone();
        let call = tokio::spawn(Box::pin(async move {
            owned
                .open_status(Instant::now() + Duration::from_secs(14))
                .await
        }));
        let pause_guard = pause.reached().await;
        match mode {
            37 => {
                state
                    .store
                    .put_setting(keys::SHARING_ENABLED, "false")
                    .await
                    .expect("saved switch race");
            }
            38 => {
                client
                    .execute(
                        "DELETE FROM cluster_node_capabilities WHERE capability=$1",
                        hiqlite::params!(
                            plurx_core::cluster::membership::SHARING_PURPOSE_KEYS_CAPABILITY
                        ),
                    )
                    .await
                    .expect("actual purpose floor race");
            }
            39 => {
                call.abort();
                assert!(matches!(call.await, Err(error) if error.is_cancelled()));
                actor.request_retirement();
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert!(actor.settlement_status().is_none(), "owned status observation must retain its actual guard after waiter cancellation");
                drop(pause_guard);
                actor
                    .retire()
                    .await
                    .expect("actual status job joins retirement");
                return;
            }
            40 | 41 => tokio::time::sleep(Duration::from_millis(5100)).await,
            _ => unreachable!(),
        }
        drop(pause_guard);
        let outcome = call.await.expect("owned status waiter");
        if mode == 41 {
            let (status, guard) = outcome
                .expect("fresh observation after parked status read")
                .into_parts();
            assert_eq!(status.playlist_shape, "vod");
            drop(guard);
        } else {
            assert!(matches!(outcome, Err(SourceWorkerError::Unavailable)));
        }
        assert_eq!(
            manager
                .vod
                .source_control_observation_for_test(&response.session_id)
                .await
                .expect("status activity unchanged"),
            baseline
        );
        actor
            .retire()
            .await
            .expect("actual raced status retirement");
        return;
    }
    if (30..=35).contains(&mode) {
        use crate::playback_control::*;
        let bootstrap = response
            .control
            .as_ref()
            .expect("actual Source control tuple");
        let quality = if mode == 31 {
            serde_json::json!({"mode":"manual","height":144})
        } else {
            serde_json::json!({"mode":"original"})
        };
        let request:ControlRequestV1 = serde_json::from_value(serde_json::json!({
            "protocol":PROTOCOL_V1,"generation":bootstrap.generation,"control_epoch":bootstrap.control_epoch,
            "client_instance_id":uuid::Uuid::new_v4().to_string(),"sequence":1,"demand":"active",
            "position_ms":0,"buffered_from_ms":0,"buffered_through_ms":1000,"playback_rate":1.0,
            "render_state":"seeking","seek_target_ms":1000,
            "selection":{"quality":quality,"audio_track":null,"subtitle":{"mode":"native","track":0},"audio_offset_ms":0,"codec":"auto","dynamic_range":"auto"},
            "capabilities":{"platform":"web","max_height":2160,"codecs":["h264"],"dynamic_ranges":["sdr"],"dual_player_preparation":false},
            "supported_actions":[],"intent":null
        })).expect("actual legacy no-intent Source seek");
        let control_deadline = || Instant::now() + Duration::from_secs(10);
        if (32..=35).contains(&mode) {
            let baseline = manager
                .vod
                .source_control_observation_for_test(&response.session_id)
                .await
                .expect("actual before-control activity");
            let pause = actor.0.control_hooks.pause(matches!(mode, 32 | 33));
            let owned = actor.clone();
            let called = request.clone();
            let call = tokio::spawn(Box::pin(async move {
                owned
                    .control(called, Instant::now() + Duration::from_secs(12))
                    .await
            }));
            let held_pause = pause.reached().await;
            match mode {
                32 => {
                    client
                        .execute(
                            "DELETE FROM cluster_node_capabilities WHERE capability=$1",
                            hiqlite::params!(
                                plurx_core::cluster::membership::SHARING_PURPOSE_KEYS_CAPABILITY
                            ),
                        )
                        .await
                        .expect("actual floor loss after observation");
                }
                33 => {
                    state
                        .store
                        .put_setting(keys::SHARING_ENABLED, "false")
                        .await
                        .expect("actual saved switch race");
                }
                34 => tokio::time::sleep(Duration::from_millis(5100)).await,
                35 => {
                    let actual = state
                        .store
                        .get_file(1)
                        .await
                        .expect("actual Source file")
                        .expect("file");
                    std::fs::write(&actual.path, b"changed physical control Source")
                        .expect("actual physical drift");
                }
                _ => unreachable!(),
            }
            drop(held_pause);
            let result = call.await.expect("owned control task");
            if let Ok(opened) = result {
                let (result, guard) = opened.into_parts();
                assert!(matches!(result, Err(ControlStateError::Unavailable)));
                drop(guard);
            } else {
                assert!(matches!(result, Err(SourceWorkerError::Unavailable)));
            }
            assert_eq!(
                manager
                    .vod
                    .source_control_observation_for_test(&response.session_id)
                    .await
                    .expect("retained actual control/activity evidence"),
                baseline,
                "refused Source control must not accept sequence/activity"
            );
            actor
                .retire()
                .await
                .expect("actual refused-control retirement");
            return;
        }
        let (accepted, held) = actor
            .control(request.clone(), control_deadline())
            .await
            .expect("owned Source seek")
            .into_parts();
        let accepted = accepted.expect("accepted seek");
        assert_eq!(accepted.disposition, ControlDisposition::Accepted);
        assert_eq!(accepted.accepted_sequence, 1);
        assert!(accepted.preparation_directive.is_none());
        let (replay, replay_guard) = actor
            .control(request.clone(), control_deadline())
            .await
            .expect("exact Source replay")
            .into_parts();
        assert_eq!(
            replay.expect("replay").disposition,
            ControlDisposition::Replay
        );
        drop(replay_guard);
        let mut wrong = request.clone();
        wrong.control_epoch += 1;
        assert!(matches!(
            actor.control(wrong, control_deadline()).await,
            Err(SourceWorkerError::Conflict)
        ));
        let mut wrong = request.clone();
        wrong.generation = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            actor.control(wrong, control_deadline()).await,
            Err(SourceWorkerError::Conflict)
        ));
        let mut directed = request.clone();
        directed.selection.audio_track = Some(0);
        assert!(matches!(
            actor.control(directed, control_deadline()).await,
            Err(SourceWorkerError::Unsupported)
        ));
        let mut pause = request.clone();
        pause.sequence = 2;
        pause.demand = PlaybackDemand::Hold;
        pause.render_state = RenderState::Rendering;
        pause.seek_target_ms = None;
        let (result, pause_guard) = actor
            .control(pause.clone(), control_deadline())
            .await
            .expect("owned Source pause")
            .into_parts();
        let (result, pause_guard) = if let Err(ControlStateError::RateLimited(ms)) = result {
            drop(pause_guard);
            tokio::time::sleep(Duration::from_millis(u64::from(ms) + 1)).await;
            actor
                .control(pause, control_deadline())
                .await
                .expect("actual rate-limited Source retry")
                .into_parts()
        } else {
            (result, pause_guard)
        };
        assert_eq!(result.expect("pause").accepted_sequence, 2);
        drop(pause_guard);
        actor.request_retirement();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            actor.settlement_status().is_none(),
            "actual control response body retains physical owner"
        );
        drop(held);
        actor
            .retire()
            .await
            .expect("retire after held actual control body");
        return;
    }
    if mode == 4 {
        manager
            .vod
            .expire_source_viewer_for_actor_test(&response.session_id, &assignment)
            .await;
        assert!(manager
            .vod
            .recovered_start(&response.session_id)
            .await
            .is_none());
        // No End, actor.retire, body touch or private control is sent. The
        // actual owner supervisor must notice its real VOD reader disappeared.
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let notification = actor.0.changed.notified();
                tokio::pin!(notification);
                notification.as_mut().enable();
                if let Some(result) = actor.0.state.lock().expect("actual actor state").settled {
                    result.expect("actual post-idle settlement");
                    break;
                }
                notification.await;
            }
        })
        .await
        .expect("automatic actual inactivity cleanup");
        assert_eq!(manager.admissions.software_in_use(), 0);
        assert!(manager.lookup_source_worker(&assignment).is_none());
        let ended = state
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .expect("retained exact route")
            .expect("ended route");
        assert_eq!(ended.state, "ended");
        return;
    }
    let native_body = if mode >= 21 {
        assert!(response.playlist_url.contains("master.m3u8"));
        assert!(matches!(
            actor
                .open_resource(
                    &SharingHlsResource::parse("master.m3u8?subtitle=1")
                        .expect("typed foreign selection"),
                    Instant::now() + Duration::from_secs(5)
                )
                .await,
            Err(SourceWorkerError::Unsupported)
        ));
        let legacy = actor
            .open_resource(
                &SharingHlsResource::parse("index.m3u8?native=1&subtitle=0")
                    .expect("typed native alias"),
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("guarded native alias");
        let (payload, guard) = legacy.into_parts();
        let SourceResourcePayload::Playlist(bytes) = payload else {
            panic!("native alias")
        };
        assert!(String::from_utf8_lossy(&bytes).contains("#EXT-X-MEDIA:TYPE=SUBTITLES"));
        drop(guard);
        let master = actor
            .open_resource(
                &SharingHlsResource::parse("master.m3u8?subtitle=0").expect("typed master"),
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("actual native master");
        let (payload, guard) = master.into_parts();
        let SourceResourcePayload::Playlist(master) = payload else {
            panic!("master")
        };
        assert!(String::from_utf8_lossy(&master).contains("subs/0/index.m3u8"));
        drop(guard);
        for path in ["video.m3u8", "subs/0/index.m3u8"] {
            let resource = SharingHlsResource::parse(path).expect("typed native playlist");
            let opened = actor
                .open_resource(&resource, Instant::now() + Duration::from_secs(5))
                .await
                .expect("actual native playlist");
            let (payload, guard) = opened.into_parts();
            let SourceResourcePayload::Playlist(bytes) = payload else {
                panic!("native playlist")
            };
            plurx_core::sharing_resources::validate_sharing_playlist(&resource, &bytes)
                .expect("closed native playlist grammar");
            drop(guard);
        }
        assert!(matches!(
            actor
                .open_resource(
                    &SharingHlsResource::parse("subs/1/index.m3u8").expect("typed missing track"),
                    Instant::now() + Duration::from_secs(5)
                )
                .await,
            Err(SourceWorkerError::Unsupported)
        ));
        let opened = actor
            .open_resource(
                &SharingHlsResource::parse("subs/0/seg00000.vtt").expect("typed actual VTT"),
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("actual native VTT");
        let (payload, guard) = opened.into_parts();
        let SourceResourcePayload::SubtitleText(bytes) = payload else {
            panic!("VTT")
        };
        let text = String::from_utf8(bytes).expect("actual bounded UTF8");
        assert!(text.contains("Actual Source caption"));
        assert!(text.contains("X-TIMESTAMP-MAP"));
        Some(guard)
    } else {
        None
    };
    let (start_body, start_guard) = actor
        .open_start_response(Instant::now() + Duration::from_secs(5))
        .await
        .expect("fresh actual counted Start body");
    assert_eq!(
        serde_json::to_value(&start_body).expect("actual Start body"),
        serde_json::to_value(&response).expect("actual retained response")
    );
    let opened = actor
        .open_resource(
            &SharingHlsResource::parse("index.m3u8").expect("typed playlist"),
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("authorized actual playlist");
    let (payload, body_guard) = opened.into_parts();
    let SourceResourcePayload::Playlist(bytes) = payload else {
        panic!("playlist bytes")
    };
    assert!(bytes.starts_with(b"#EXTM3U"));
    let init = actor
        .open_resource(
            &SharingHlsResource::parse("init.mp4").expect("typed actual init"),
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("actual verified open init");
    let (init_payload, init_guard) = init.into_parts();
    let SourceResourcePayload::File(init_file) = init_payload else {
        panic!("actual opened file")
    };
    assert!(init_file.len > 0);
    let file_id = assignment
        .binding()
        .file_id()
        .as_str()
        .parse::<i64>()
        .expect("actual Source ID");
    let file = state
        .store
        .get_file(file_id)
        .await
        .expect("stored actual file")
        .expect("file");
    let original = file.path.with_extension("held-source-original");
    tokio::fs::rename(&file.path, &original)
        .await
        .expect("replace actual Source path");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&original, &file.path).expect("actual Source link replacement");
    #[cfg(windows)]
    tokio::fs::copy(&original, &file.path)
        .await
        .expect("actual Source replacement");
    assert!(
        actor.0.gate.current_owned(&assignment).await.is_ok(),
        "stored authority can lag the physical object"
    );
    assert!(matches!(actor.open_resource(&SharingHlsResource::parse("index.m3u8").expect("typed actual cached playlist"), Instant::now() + Duration::from_secs(5)).await, Err(SourceWorkerError::Unavailable)), "Source physical boundary must refuse a changed or linked path despite stored authorization");
    tokio::time::timeout(Duration::from_secs(1), init_guard.cancelled())
        .await
        .expect("actual retained Body fence refuses physical Source drift");
    state
        .store
        .put_setting(keys::SHARING_ENABLED, "false")
        .await
        .expect("actual saved disable");
    let retiring = tokio::spawn({
        let actor = actor.clone();
        async move { actor.retire().await }
    });
    body_guard.cancelled().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !retiring.is_finished(),
        "actual response Body must retain Source obligation"
    );
    assert!(manager.lookup_source_worker(&assignment).is_some());
    assert!(matches!(
        actor
            .open_resource(
                &SharingHlsResource::parse("index.m3u8").expect("typed resource"),
                Instant::now() + Duration::from_secs(1)
            )
            .await,
        Err(SourceWorkerError::Unavailable)
    ));
    drop(bytes);
    drop(body_guard);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !retiring.is_finished(),
        "opened file Body independently retains Source obligation"
    );
    drop(init_file);
    drop(init_guard);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !retiring.is_finished(),
        "actual Start response Body independently retains the Source obligation"
    );
    drop(start_body);
    drop(start_guard);
    if let Some(guard) = native_body {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !retiring.is_finished(),
            "actual VTT Body retains Source obligation"
        );
        drop(guard);
    }
    tokio::time::timeout(Duration::from_secs(10), retiring)
        .await
        .expect("actual retirement budget")
        .expect("owned task")
        .expect("postreap, postbody release");
    assert_eq!(manager.admissions.software_in_use(), 0);
    assert!(manager.lookup_source_worker(&assignment).is_none());
}

#[tokio::test]
async fn source_copy_cold_index_refuses_changed_actual_file_before_child() {
    source_copy_preadmission_fixture(10).await;
}

#[tokio::test]
async fn source_copy_cold_index_refuses_purpose_floor_loss_before_child() {
    source_copy_preadmission_fixture(11).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_actor_real_probe_recipe_media_and_body_retirement() {
    Box::pin(source_copy_preadmission_fixture(12)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_child_survives_cancelled_waiter_and_revoke() {
    Box::pin(source_copy_preadmission_fixture(13)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_refuses_sharing_off_before_child() {
    Box::pin(source_copy_preadmission_fixture(14)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_retains_actual_permit_after_injected_wait_failure() {
    Box::pin(source_copy_preadmission_fixture(15)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_refuses_expired_original_observation() {
    Box::pin(source_copy_preadmission_fixture(16)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_refuses_changed_physical_file() {
    Box::pin(source_copy_preadmission_fixture(17)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_refuses_lost_purpose_floor() {
    Box::pin(source_copy_preadmission_fixture(18)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_requires_actual_cpu_admission_before_child() {
    Box::pin(source_copy_preadmission_fixture(19)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_encoded_probe_settles_before_separate_encoder_capacity_wait() {
    Box::pin(source_copy_preadmission_fixture(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_embedded_text_media_and_counted_vtt_retirement() {
    Box::pin(source_copy_preadmission_fixture(21)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_child_survives_waiter_cancel_and_revoke() {
    Box::pin(source_copy_preadmission_fixture(22)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_refuses_disabled_before_child() {
    Box::pin(source_copy_preadmission_fixture(23)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_retains_permit_through_actual_reap_retry() {
    Box::pin(source_copy_preadmission_fixture(24)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_refuses_expired_original_observation() {
    Box::pin(source_copy_preadmission_fixture(25)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_refuses_changed_physical_source() {
    Box::pin(source_copy_preadmission_fixture(26)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_refuses_lost_purpose_floor() {
    Box::pin(source_copy_preadmission_fixture(27)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_requires_actual_cpu_admission() {
    Box::pin(source_copy_preadmission_fixture(28)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_native_encoded_embedded_text_actual_media_and_body_retirement() {
    Box::pin(source_copy_preadmission_fixture(29)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_preparation_closes_actual_parent_descriptors_before_settlement() {
    Box::pin(source_copy_preadmission_fixture(5)).await;
    Box::pin(source_copy_preadmission_fixture(12)).await;
    Box::pin(source_copy_preadmission_fixture(21)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_legacy_copy_seek_replay_pause_and_counted_body() {
    Box::pin(source_copy_preadmission_fixture(30)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_legacy_encoded_seek_replay_pause_and_counted_body() {
    Box::pin(source_copy_preadmission_fixture(31)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_refuses_same_write_purpose_floor_loss_without_activity() {
    Box::pin(source_copy_preadmission_fixture(32)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_refuses_same_write_saved_switch_loss_without_activity() {
    Box::pin(source_copy_preadmission_fixture(33)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_refuses_expired_original_clock_without_activity() {
    Box::pin(source_copy_preadmission_fixture(34)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_refuses_changed_held_file_without_activity() {
    Box::pin(source_copy_preadmission_fixture(35)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_actual_metrics_are_private_activity_free_and_counted() {
    Box::pin(source_copy_preadmission_fixture(36)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_saved_switch_race_refuses_without_activity() {
    Box::pin(source_copy_preadmission_fixture(37)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_purpose_floor_race_refuses_without_activity() {
    Box::pin(source_copy_preadmission_fixture(38)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_waiter_cancellation_retains_owned_observation() {
    Box::pin(source_copy_preadmission_fixture(39)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_original_observation_expiry_refuses_without_activity() {
    Box::pin(source_copy_preadmission_fixture(40)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_parked_read_reobserves_fresh_authority() {
    Box::pin(source_copy_preadmission_fixture(41)).await;
}
