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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_factory_refusal_returns_exact_invocation_receipt_before_admission() {
    Box::pin(source_copy_preadmission_fixture(48)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_factory_refuses_unbound_normalized_preparation_before_admission() {
    Box::pin(source_copy_preadmission_fixture(49)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_factory_refuses_permission_from_another_actual_registry_boot() {
    Box::pin(source_copy_preadmission_fixture(50)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_ingress_permission_refuses_empty_ledger_before_factory() {
    Box::pin(source_copy_preadmission_fixture(51)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_producer_preparation_requires_fresh_unsealed_ingress_permission() {
    Box::pin(source_copy_preadmission_fixture(52)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_retained_permission_survives_zero_open_connections_and_reconnect() {
    Box::pin(source_copy_preadmission_fixture(54)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_retired_predecessor_cannot_invalidate_surviving_owner_generation_permission() {
    Box::pin(source_copy_preadmission_fixture(53)).await;
}

fn source_fixture_state() -> Arc<crate::state::AppState> {
    Arc::new(crate::http::source_actor_test_state())
}

fn source_fixture_addresses() -> (std::net::SocketAddr, std::net::SocketAddr) {
    static USED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeSet<std::net::SocketAddr>>,
    > = std::sync::OnceLock::new();

    // The voter binds after asynchronous store initialization. Keep every
    // probed address claimed by this fixture family even after its listener
    // is dropped, so another fixture cannot reuse a pending boot's address.
    let mut used = USED
        .get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .expect("Source fixture address registry");
    let mut reserve = || loop {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("Source fixture listener");
        let address = listener.local_addr().expect("Source fixture address");
        if used.insert(address) {
            break (listener, address);
        }
    };
    let (raft, raft_address) = reserve();
    let (api, api_address) = reserve();
    drop((raft, api));
    (raft_address, api_address)
}

#[test]
fn source_fixture_boots_do_not_reuse_probe_addresses() {
    let mut addresses = std::collections::BTreeSet::new();
    for _ in 0..64 {
        let (raft, api) = source_fixture_addresses();
        assert!(addresses.insert(raft));
        assert!(addresses.insert(api));
    }
    assert_eq!(addresses.len(), 128);
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
    Box::pin(crate::sharing_fixture_clock::select_applied_singleton(
        config,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_unknown_local_register_actual_closure_and_exact_ack_clear_pending_before_new_driver(
) {
    Box::pin(source_copy_preadmission_fixture(55)).await;
}
async fn source_copy_preadmission_fixture(mode: u8) {
    use crate::http::hls::{prepare_source_playback, CreateSession, SourcePlaybackTarget};
    use plurx_core::{
        config::Config,
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::{new_secret, secret_hash, InvitationRecord, SecretDomain, ShareClaim, SourceId},
        sharing_catalogue_details::CatalogueRevisionKey,
        sharing_source_sessions::*,
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let directory = crate::test_tempdir().expect("actual Source root");
    eprintln!("Source fixture: actual standalone selection");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    (config.cluster.raft_bind, config.cluster.api_bind) = source_fixture_addresses();
    config.cluster.advertise_host = "localhost".into();
    let mut selected = source_fixture_store(&config)
        .await
        .expect("actual standalone voter");
    selected
        .store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved Source choice before startup");
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual Source startup coordinator"));
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
    state_mut.media_sessions = crate::media_sessions::MediaSessionCoordinator::new(
        membership.clone(),
        Arc::clone(&state_mut.store),
    );
    state_mut.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&master),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    state_mut.node_id = selected.identity.node_id.clone();
    state_mut
        .membership
        .set_ingress_custody_boot(Some(state_mut.sharing.accepted_drivers.boot_id()));
    state_mut
        .membership
        .publish_ingress_custody_boot()
        .await
        .expect("actual registry boot publication before admission");

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
    let envelope = store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .expect("installed Source key read")
        .expect("startup-owned Source key");
    let key = CatalogueRevisionKey::open(&master, identity.clone(), &envelope)
        .expect("open startup-owned Source key");
    // A raw legacy heartbeat withdraws every capability, including this
    // process's actual ingress registry boot. Publish the canonical heartbeat
    // after Source startup instead of reconstructing a partial capability set.
    membership
        .publish_ingress_custody_boot()
        .await
        .expect("actual complete ingress boot heartbeat after candidate installation");
    let now = crate::fragment_index_cluster::unix_ms();
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
    let second_prepared = if mode == 53 {
        let mut second: CreateSession =
            serde_json::from_value(serde_json::to_value(&body).expect("second actual ask"))
                .expect("second complete ask");
        second.playback_id = "copy-source-survivor".into();
        second.request_id = Some("copy-source-survivor-request".into());
        Some(
            Box::pin(prepare_source_playback(
                &state,
                &headers,
                target.clone(),
                second,
            ))
            .await
            .expect("actual second prepared Source invocation"),
        )
    } else {
        None
    };
    let preparation = Box::pin(prepare_source_playback(&state, &headers, target, body));
    eprintln!(
        "Source preparation future: {} bytes",
        std::mem::size_of_val(preparation.as_ref().get_ref())
    );
    let mut prepared = preparation.await.expect("actual Source engine preparation");
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
                ingress_registry_boot_id: state.sharing.accepted_drivers.boot_id(),
                now_ms: now,
                claim_expires_at_ms: now + 60000,
                credential_hash: hash.clone(),
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
    if mode != 49 {
        prepared
            .bind_source_invocation(&binding)
            .expect("bind actual fresh fixture invocation before factory admission");
    }
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
    if let Some(mut second_prepared) = second_prepared {
        let now = crate::fragment_index_cluster::unix_ms();
        let SourceIntentRead::Ready(second_intent) = store
            .prepare_source_session_intent(
                SourceSessionRequest {
                    principal: second_prepared.principal().clone(),
                    request_id: "copy-source-survivor-request".into(),
                    request_fingerprint: second_prepared.fingerprint().into(),
                    playback_id: second_prepared.request().playback_id.clone(),
                    incarnation_id: uuid::Uuid::new_v4(),
                    ingress_registry_boot_id: state.sharing.accepted_drivers.boot_id(),
                    now_ms: now,
                    claim_expires_at_ms: now + 60000,
                    credential_hash: hash,
                    item_id: assignment.binding().item_id().clone(),
                    file_id: assignment.binding().file_id().clone(),
                    file_revision: assignment.binding().file_revision().clone(),
                },
                &master,
            )
            .await
            .expect("actual second Source intent")
        else {
            panic!("second intent");
        };
        let fresh_members = membership
            .observe_source_admission_members()
            .await
            .expect("actual second members")
            .expect("actual second floor");
        let SourceClaimOutcome::Acquired(second_binding) = store
            .claim_source_media_session(&second_intent, &fresh_members)
            .await
            .expect("actual second claim")
        else {
            panic!("second claim");
        };
        second_prepared
            .bind_source_invocation(&second_binding)
            .expect("bind actual second invocation");
        let second_assignment = store
            .assign_source_dispatch(&second_binding, &master, &fresh_members)
            .await
            .expect("actual second assignment")
            .expect("second assigned owner");
        Box::pin(source_two_owner_generation_fixture(
            state,
            manager,
            prepared,
            assignment,
            second_prepared,
            second_assignment,
        ))
        .await;
        selected.shutdown().await.expect("actual voter shutdown");
        return;
    }
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
    let seal = store
        .seal_source_ingress_custody(&assignment)
        .await
        .expect("exact empty fixture custody seal");
    assert!(matches!(
        seal,
        plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::Applied
            | plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::ExactReplay
    ));
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
    let ingress_fixture = Box::pin(SourceFactoryIngressFixture::new(
        Arc::new(state.clone()),
        assignment,
    ))
    .await;
    let gate = Arc::new(SourceProducerAuthority {
        store: Arc::clone(store),
        membership: membership.clone(),
        master: Arc::clone(master),
        registry_boot_id: state.sharing.accepted_drivers.boot_id(),
        ingress: ingress_fixture.permission().await,
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
    let physical =
        Box::pin(reserved.wait_ready(&manager.vod, Instant::now() + Duration::from_secs(10)))
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
    ingress_fixture.close().await;
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
        continuous_media: None,
        sdr_master_codecs: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        audio_delivery: None,
        audio_claim: None,
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
    if mode == 55 {
        source_actual_unknown_register_reconciliation(state, manager, assignment).await;
        return;
    }
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
    if mode == 51 {
        assert!(state
            .store
            .prepare_source_ingress_admission(
                &assignment,
                state.sharing.accepted_drivers.boot_id(),
                &members
            )
            .await
            .expect("guarded empty ledger read")
            .is_none());
        state
            .store
            .seal_source_ingress_custody(&assignment)
            .await
            .expect("empty exact custody seal");
        assert_eq!(
            state
                .store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("sealed empty assignment cleanup"),
            SourceReleaseOutcome::Released
        );
        assert!(manager.lookup_source_worker(&assignment).is_none());
        assert_eq!(manager.admissions.software_in_use(), 0);
        return;
    }
    let ingress_fixture = Box::pin(SourceFactoryIngressFixture::new(
        Arc::clone(&state),
        &assignment,
    ))
    .await;
    let ingress_permission = ingress_fixture.permission().await;
    if mode == 52 {
        let gate = SourceProducerAuthority {
            store: Arc::clone(&state.store),
            membership: state.membership.clone(),
            master: Arc::clone(&state.sharing.key),
            registry_boot_id: state.sharing.accepted_drivers.boot_id(),
            ingress: ingress_permission.clone(),
        };
        assert!(gate.current_preparation(&assignment).await.is_ok());
        ingress_fixture.close().await;
        assert!(gate.current_preparation(&assignment).await.is_err());
        assert_eq!(
            state
                .store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("actual closure and sealed assignment cleanup"),
            SourceReleaseOutcome::Released
        );
        assert_eq!(manager.admissions.software_in_use(), 0);
        assert!(manager.lookup_source_worker(&assignment).is_none());
        return;
    }
    if matches!(mode, 48..=50) {
        // A real factory validation refusal, with a genuine acquired/assigned
        // Source claim and activation authority. No registry flag or SQL
        // absence is used to manufacture its no-admission receipt.
        let other = if mode == 49 {
            Arc::clone(&state)
        } else if mode == 50 {
            let mut other = source_fixture_state();
            let owned = Arc::get_mut(&mut other).expect("sole replacement registry fixture");
            owned.store = Arc::clone(&state.store);
            owned.membership = state.membership.clone();
            owned.node_id = state.node_id.clone();
            assert_ne!(
                owned.sharing.accepted_drivers.boot_id(),
                ingress_permission.registry_boot_id()
            );
            assert!(prepared.matches_assignment(&assignment));
            other
        } else {
            source_fixture_state()
        };
        if mode == 48 {
            assert!(!Arc::ptr_eq(&state.store, &other.store));
        }
        let refusal = manager
            .start_source_worker(
                other,
                assignment.clone(),
                *activation,
                ingress_permission,
                prepared,
                Instant::now() + Duration::from_secs(15),
            )
            .await
            .err()
            .expect("actual factory refusal");
        assert_eq!(
            refusal.reason(),
            crate::transcode::source_actor::SourceWorkerError::Conflict
        );
        assert!(refusal.assignment().same_identity(&assignment));
        ingress_fixture.close().await;
        assert_eq!(
            state
                .store
                .settle_source_assigned_without_activation(refusal.assignment())
                .await
                .expect("exact g1 accounting cleanup after real factory receipt"),
            SourceReleaseOutcome::Released
        );
        assert!(manager.lookup_source_worker(&assignment).is_none());
        assert_eq!(manager.admissions.software_in_use(), 0);
        return;
    }
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
        ingress_permission,
        prepared,
        Instant::now() + Duration::from_secs(15),
    ))
    .await
    .expect("owned Source task");
    if mode == 54 {
        actor
            .wait_ready(Instant::now() + Duration::from_secs(15))
            .await
            .expect("actual admitted owner");
        ingress_fixture.close_transport(false, None).await;
        let fresh = state
            .membership
            .observe_source_admission_members()
            .await
            .expect("gap members")
            .expect("gap floor");
        assert!(state
            .store
            .prepare_source_ingress_admission(
                &assignment,
                state.sharing.accepted_drivers.boot_id(),
                &fresh
            )
            .await
            .expect("initial admission requires a current driver")
            .is_none());
        let retained = actor
            .0
            .gate
            .retained_ingress(&assignment)
            .expect("actual owner's initial permission");
        assert!(
            actor.0.gate.authorize_generation(&[retained]).await.is_ok(),
            "ordinary closed connection cannot end a still-admitted producer"
        );
        let reconnected = Box::pin(SourceFactoryIngressFixture::new(
            Arc::clone(&state),
            &assignment,
        ))
        .await;
        let monitor = reconnected.monitor_actor(actor.clone());
        let opened = actor
            .open_resource(
                &SharingHlsResource::parse("index.m3u8").expect("typed reconnect resource"),
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("reconnected owner still serves media");
        let (payload, guard) = opened.into_parts();
        assert!(
            matches!(payload, SourceResourcePayload::Playlist(bytes) if bytes.starts_with(b"#EXTM3U"))
        );
        drop(guard);
        tokio::time::timeout(Duration::from_secs(10), actor.retire())
            .await
            .expect("actual reconnect retirement budget")
            .expect("actual reconnect retirement");
        monitor.join().await;
        return;
    }
    let ingress_monitor = ingress_fixture.monitor_actor(actor.clone());
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
        ingress_monitor.join().await;
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
        ingress_monitor.join().await;
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
    if matches!(mode, 42..=44) {
        let resource = SharingHlsResource::parse(if mode != 43 {
            "init.mp4"
        } else {
            "seg00000.m4s"
        })
        .expect("actual closed Source media resource");
        let activity = manager
            .vod
            .source_control_observation_for_test(&response.session_id)
            .await
            .expect("actual resource activity");
        let pause = actor.0.resource_hooks.pause();
        let deadline = Instant::now() + Duration::from_secs(if mode == 44 { 2 } else { 20 });
        let owned = actor.clone();
        let call = tokio::spawn(Box::pin(async move {
            owned.open_resource(&resource, deadline).await
        }));
        let paused = pause.reached().await;
        // This exact actual cache descriptor is owned by the parked filesystem
        // job, not a simulated producer/closed flag.
        assert_descriptor(pause, true, "parked job owns its descriptor");
        if mode == 44 {
            let called = call.await.expect("actual timed resource waiter");
            assert!(matches!(called, Err(SourceWorkerError::Deadline)));
            assert!(
                actor.0.state.lock().expect("actual Source state").bodies > 0,
                "timed waiter cannot release actual unfinished filesystem job"
            );
            assert_descriptor(pause, true, "timed waiter leaves the job its descriptor");
            drop(paused);
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let changed = actor.0.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if actor.0.state.lock().expect("actual Source state").bodies == 0 {
                        break;
                    }
                    changed.await;
                }
            })
            .await
            .expect("actual timed read job joins");
            assert_eq!(
                manager
                    .vod
                    .source_control_observation_for_test(&response.session_id)
                    .await
                    .expect("actual postdeadline resource activity"),
                activity,
                "late observation cannot touch viewer inactivity/demand"
            );
            assert_descriptor(
                pause,
                false,
                "timed actual read job descriptor closed before final guard drop",
            );
            actor.retire().await.expect("actual timed read retirement");
            ingress_monitor.join().await;
            return;
        }
        call.abort();
        assert!(matches!(call.await, Err(error) if error.is_cancelled()));
        actor.request_retirement();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            actor.settlement_status().is_none(),
            "actual opened Source job retains retirement through cancelled waiter"
        );
        assert_descriptor(
            pause,
            true,
            "cancelled waiter leaves the job its descriptor",
        );
        drop(paused);
        tokio::time::timeout(Duration::from_secs(10), actor.retire())
            .await
            .expect("actual read job settlement deadline")
            .expect("actual joined read job and physical retirement");
        assert_eq!(actor.settlement_status(), Some(Ok(())));
        assert_descriptor(
            pause,
            false,
            "actual read descriptor closes before settled actor is visible",
        );
        ingress_monitor.join().await;
        return;
    }
    if (36..=41).contains(&mode) || mode == 45 {
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
            ingress_monitor.join().await;
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
                ingress_monitor.join().await;
                return;
            }
            40 | 41 => tokio::time::sleep(Duration::from_millis(5100)).await,
            45 => {
                // Another renewal by this same owner wins the optimistic
                // lease race while the observation is parked between its
                // proof read and its renewal.
                let (_, concurrent) = actor
                    .open_start_response(Instant::now() + Duration::from_secs(5))
                    .await
                    .expect("concurrent same-owner renewal");
                drop(concurrent);
            }
            _ => unreachable!(),
        }
        drop(pause_guard);
        let outcome = call.await.expect("owned status waiter");
        if matches!(mode, 41 | 45) {
            let (status, guard) = outcome
                .expect("fresh observation after parked status read or lost lease race")
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
        ingress_monitor.join().await;
        return;
    }
    if (30..=35).contains(&mode) || matches!(mode, 46 | 47) {
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
            ingress_monitor.join().await;
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
        // A changed ask cannot reuse an accepted sequence: that is a stale
        // fence, never a second answer for sequence 1.
        let mut directed = request.clone();
        directed.selection.audio_track = Some(0);
        let (result, stale_guard) = actor
            .control(directed, control_deadline())
            .await
            .expect("owned stale directed exchange")
            .into_parts();
        assert!(matches!(result, Err(ControlStateError::StaleSequence)));
        drop(stale_guard);
        if mode == 46 {
            // A directed change on a new sequence: accepted and recorded like
            // any other ask, answered `preparation: none` with the current
            // rendition unchanged, so the client reopens with a fresh Start.
            let mut directed = request.clone();
            directed.sequence = 2;
            directed.render_state = RenderState::Rendering;
            directed.seek_target_ms = None;
            directed.selection.quality = QualitySelection::Manual { height: 144 };
            directed.supported_actions = Some(vec![PREPARE_REPLACEMENT_ACTION.to_owned()]);
            let (answer, guard) = loop {
                let (answer, guard) = actor
                    .control(directed.clone(), control_deadline())
                    .await
                    .expect("owned Source directed change")
                    .into_response(&directed);
                match answer {
                    Err(ControlStateError::RateLimited(ms)) => {
                        drop(guard);
                        tokio::time::sleep(Duration::from_millis(u64::from(ms) + 1)).await;
                    }
                    answer => break (answer, guard),
                }
            };
            let answer = answer.expect("accepted directed change");
            assert_eq!(answer.accepted_sequence, 2);
            assert_eq!(answer.delivery.preparation.as_deref(), Some("none"));
            assert_eq!(answer.action, ControlAction::None);
            assert_ne!(
                answer.effective_selection.height, 144,
                "the current rendition is not replaced in place"
            );
            drop(guard);
            let (snapshot, _) = manager
                .vod
                .source_control_observation_for_test(&response.session_id)
                .await
                .expect("recorded directed ask");
            assert_eq!(
                snapshot.expect("accepted snapshot").selection.desired(),
                directed.selection.desired(),
                "the Source recorded the viewer's new ask"
            );
            drop(held);
            actor.retire().await.expect("retire after directed change");
            ingress_monitor.join().await;
            return;
        }
        if mode == 47 {
            // The Source never offers a successor, so an acknowledgement can
            // name no slot here. Refused before any sequence or activity.
            let baseline = manager
                .vod
                .source_control_observation_for_test(&response.session_id)
                .await
                .expect("before acknowledgement");
            let mut acknowledged = request.clone();
            acknowledged.sequence = 2;
            acknowledged.render_state = RenderState::Rendering;
            acknowledged.seek_target_ms = None;
            acknowledged.acknowledgement = Some(ActionAcknowledgement {
                action_id: uuid::Uuid::new_v4().to_string(),
                state: AcknowledgementState::MetadataReady,
                buffered_through_ms: None,
                committed_media_origin_ms: None,
                first_frame_unix_ms: None,
            });
            assert!(matches!(
                actor.control(acknowledged, control_deadline()).await,
                Err(SourceWorkerError::Unsupported)
            ));
            assert_eq!(
                manager
                    .vod
                    .source_control_observation_for_test(&response.session_id)
                    .await
                    .expect("after refused acknowledgement"),
                baseline,
                "a refused acknowledgement accepts no sequence or activity"
            );
            drop(held);
            actor
                .retire()
                .await
                .expect("retire after refused acknowledgement");
            ingress_monitor.join().await;
            return;
        }
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
        ingress_monitor.join().await;
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
        ingress_monitor.join().await;
        return;
    }
    if mode == 56 {
        let bootstrap = response.control.as_ref().expect("cached caption fixture");
        let request: crate::playback_control::ControlRequestV1 = serde_json::from_value(serde_json::json!({
            "protocol":crate::playback_control::PROTOCOL_V1,"generation":bootstrap.generation,"control_epoch":bootstrap.control_epoch,
            "client_instance_id":uuid::Uuid::new_v4().to_string(),"sequence":1,"demand":"active",
            "position_ms":0,"buffered_from_ms":0,"buffered_through_ms":1000,"playback_rate":1.0,
            "render_state":"seeking","seek_target_ms":1000,
            "selection":{"quality":{"mode":"original"},"audio_track":null,"subtitle":{"mode":"native","track":0},"audio_offset_ms":0,"codec":"auto","dynamic_range":"auto"},
            "capabilities":{"platform":"web","max_height":2160,"codecs":["h264"],"dynamic_ranges":["sdr"],"dual_player_preparation":false},
            "supported_actions":[],"intent":null
        })).expect("cached caption fixture");
        let opened = actor
            .control(request.clone(), Instant::now() + Duration::from_secs(10))
            .await
            .expect("cached caption fixture");
        let (control, guard) = opened.into_response(&request);
        let control = control.expect("cached caption fixture");
        assert_eq!(
            control.delivery.subtitle_readiness.as_deref(),
            Some("ready")
        );
        let reported = control
            .delivery
            .subtitle_revision
            .expect("cached caption fixture");
        drop(guard);
        let revision = actor
            .0
            .state
            .lock()
            .expect("cached caption fixture")
            .native
            .as_ref()
            .expect("cached caption fixture")
            .revision(0)
            .expect("cached caption fixture");
        assert_eq!(reported, revision);
        for (requested, present) in [(revision, true), ("source-stale.vtt".to_owned(), false)] {
            let opened = actor
                .open_resource(
                    &SharingHlsResource::parse(&format!("subs/0/cached-{requested}"))
                        .expect("cached caption fixture"),
                    Instant::now() + Duration::from_secs(5),
                )
                .await
                .expect("the assigned Source owner reads retained captions");
            let (payload, guard) = opened.into_parts();
            let SourceResourcePayload::CachedSubtitle {
                bytes,
                complete,
                absent,
            } = payload
            else {
                panic!("cached caption payload");
            };
            assert_eq!(complete, present);
            assert_eq!(absent, !present);
            assert_eq!(bytes.is_empty(), !present);
            if present {
                assert!(String::from_utf8_lossy(&bytes).contains("Actual Source caption"));
            }
            drop(guard);
        }
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
    ingress_monitor.join().await;
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
async fn source_control_changed_selection_declines_preparation() {
    Box::pin(source_copy_preadmission_fixture(46)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_control_ack_refused_without_slot() {
    Box::pin(source_copy_preadmission_fixture(47)).await;
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_status_survives_a_concurrent_same_owner_lease_renewal() {
    Box::pin(source_copy_preadmission_fixture(45)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_resource_init_open_job_retains_actual_fd_and_guard_after_waiter_cancellation() {
    Box::pin(source_copy_preadmission_fixture(42)).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_resource_media_open_job_retains_actual_fd_and_guard_after_waiter_cancellation() {
    Box::pin(source_copy_preadmission_fixture(43)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_resource_expired_waiter_joins_actual_job_without_late_viewer_activity() {
    Box::pin(source_copy_preadmission_fixture(44)).await;
}

/// Whether the exact descriptor a parked Source read job opened is still open.
/// The census follows the job's own file, not a descriptor number another test
/// may reuse once the job closes it. Only Unix exposes a side-effect-free
/// census of one descriptor; elsewhere the custody and settlement assertions
/// around these calls still run.
#[cfg(unix)]
fn assert_descriptor(pause: &resource::SourceResourceJobPause, open: bool, why: &str) {
    assert_eq!(pause.job_descriptor_open(), open, "{why}");
}
#[cfg(not(unix))]
fn assert_descriptor(_pause: &resource::SourceResourceJobPause, _open: bool, _why: &str) {}

/// Real accepted Hyper custody for factory fixtures. This transport setup is
/// not distributed playback qualification; it prevents a fabricated permission
/// or closure token from standing in for an accepted writer.
struct SourceFactoryIngressFixture {
    state: Arc<crate::state::AppState>,
    assignment: SourceDispatchAssignment,
    obligation: crate::sharing_connection_custody::CapturedIngressObligation,
    registration: plurx_core::sharing_ingress_custody::IngressRegistration,
    client: Option<hyper::client::conn::http2::SendRequest<axum::body::Body>>,
    response: Option<hyper::body::Incoming>,
    driver: Option<tokio::task::JoinHandle<Result<(), hyper::Error>>>,
    server: Option<tokio::task::JoinHandle<anyhow::Result<crate::HttpDrain>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Drop for SourceFactoryIngressFixture {
    fn drop(&mut self) {
        // Panic/cancellation owns both real drivers through abort; it does not
        // write an ACK or claim settlement from an aborted observer.
        if let Some(driver) = &self.driver {
            driver.abort();
        }
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}
struct SourceFactoryIngressMonitor(Option<tokio::task::JoinHandle<()>>);
impl SourceFactoryIngressMonitor {
    async fn join(mut self) {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.0.as_mut().expect("retained monitor"),
        )
        .await
        .expect("fixture ingress monitor deadline")
        .expect("fixture ingress monitor panicked");
        self.0.take();
    }
}
impl Drop for SourceFactoryIngressMonitor {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

// A lost custody CAS is acceptable only after the actual owner finishes and
// its exact ledger retains the very receipt this fixture independently joined.
fn source_fixture_ack_race_committed(
    error: &plurx_core::error::StoreError,
    mut ledger: plurx_core::sharing_ingress_custody::IngressCustodyState,
    registration: &plurx_core::sharing_ingress_custody::IngressRegistration,
    confirmation: &str,
) -> bool {
    error
        .to_string()
        .contains("NOT NULL constraint failed: sharing_source_session_bindings.incarnation_id")
        && ledger.is_sealed()
        && ledger.settled()
        && ledger.acknowledge(registration, confirmation)
            == plurx_core::sharing_ingress_custody::CustodyMutation::Replay
}
impl SourceFactoryIngressFixture {
    async fn new(
        state: Arc<crate::state::AppState>,
        assignment: &SourceDispatchAssignment,
    ) -> Self {
        Self::new_registration_outcome(state, assignment, false).await
    }
    async fn new_registration_outcome(
        state: Arc<crate::state::AppState>,
        assignment: &SourceDispatchAssignment,
        lose_register_reply: bool,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("factory ingress bind");
        let address = listener.local_addr().expect("factory ingress address");
        let (accepted, mut observed) = tokio::sync::mpsc::channel(1);
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(
                move |axum::Extension(connection): axum::Extension<
                    crate::SharingConnectionCancellation,
                >| {
                    let accepted = accepted.clone();
                    async move {
                        accepted
                            .send(connection)
                            .await
                            .expect("actual accepted connection observer");
                        axum::body::Body::empty()
                    }
                },
            ),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("factory ingress connect");
        let (mut client, driver) = hyper::client::conn::http2::handshake::<_, _, axum::body::Body>(
            hyper_util::rt::TokioExecutor::new(),
            hyper_util::rt::TokioIo::new(socket),
        )
        .await
        .expect("factory ingress handshake");
        let driver = tokio::spawn(driver);
        let response = client
            .send_request(
                axum::http::Request::builder()
                    .uri(format!("http://{address}/"))
                    .body(axum::body::Body::empty())
                    .expect("factory ingress request"),
            )
            .await
            .expect("factory ingress response")
            .into_body();
        let connection = observed.recv().await.expect("actual Hyper connection");
        let captured = state
            .sharing
            .accepted_drivers
            .capture(&connection, assignment.owner_node_id())
            .expect("actual registry capture");
        let mut permit = state
            .sharing
            .accepted_drivers
            .registration_guard()
            .await
            .expect("actual registration permit");
        let obligation = captured
            .prepare_obligation(
                &mut permit,
                "source",
                assignment.binding().incarnation_id(),
                &assignment.custody_identity(),
            )
            .expect("actual Source obligation");
        let registration = plurx_core::sharing_ingress_custody::IngressRegistration {
            node_id: state.node_id.clone(),
            boot_id: captured.id().boot_id,
            connection_id: captured.id().connection_id,
            driver_sequence: captured.id().driver_sequence,
            registration_sequence: obligation.registration_sequence(),
            closed_confirmation: None,
        };
        let members = state
            .membership
            .observe_source_admission_members()
            .await
            .expect("fresh registration members")
            .expect("fresh registration floor");
        let registered = state
            .store
            .register_source_ingress_custody(assignment, &registration, &members)
            .await
            .expect("guarded actual driver registration");
        assert!(matches!(registered, plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::Applied | plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::ExactReplay));
        if lose_register_reply {
            // The real durable Register succeeded; cancel its waiter before it
            // records definitive success. Dropping the permit retains the exact
            // principal reservation instead of pretending the row was absent.
            drop(permit);
        } else {
            permit.complete();
        }
        Self {
            state,
            assignment: assignment.clone(),
            obligation,
            registration,
            client: Some(client),
            response: Some(response),
            driver: Some(driver),
            server: Some(server),
            stop: Some(stop),
        }
    }
    async fn permission(&self) -> SourceIngressAdmissionPermission {
        let members = self
            .state
            .membership
            .observe_source_admission_members()
            .await
            .expect("actual ingress members")
            .expect("actual ingress floor");
        *self
            .state
            .store
            .prepare_source_ingress_admission(
                &self.assignment,
                self.state.sharing.accepted_drivers.boot_id(),
                &members,
            )
            .await
            .expect("guarded permission issuer")
            .expect("registered actual ingress permission")
    }
    async fn close(self) {
        self.close_transport(true, None).await;
    }
    async fn close_transport(mut self, seal: bool, retirement_owner: Option<SourceViewerActor>) {
        if seal {
            let sealed = self
                .state
                .store
                .seal_source_ingress_custody(&self.assignment)
                .await
                .expect("exact Source custody seal");
            assert!(matches!(sealed, plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::Applied | plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::ExactReplay));
        }
        drop(self.response.take());
        drop(self.client.take());
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            let drained = tokio::time::timeout(Duration::from_secs(10), server)
                .await
                .expect("actual server driver deadline")
                .expect("actual server driver join")
                .expect("actual server stopped");
            assert_eq!(drained, crate::HttpDrain::Complete);
        }
        if let Some(driver) = self.driver.take() {
            let _ = tokio::time::timeout(Duration::from_secs(10), driver)
                .await
                .expect("actual client driver deadline")
                .expect("actual client driver join");
        }
        let receipt = tokio::time::timeout(Duration::from_secs(10), self.obligation.joined())
            .await
            .expect("actual accepted Hyper driver closure");
        let ack = self
            .state
            .store
            .acknowledge_source_ingress_custody(
                &self.assignment,
                &self.registration,
                receipt.confirmation(),
            )
            .await;
        match ack {
            Ok(
                plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::Applied
                | plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite::ExactReplay,
            ) => {}
            Err(error) if retirement_owner.is_some() => {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    retirement_owner.expect("retirement owner").retire(),
                )
                .await
                .expect("actual owner settlement deadline")
                .expect("actual owner settlement");
                let ledger = self
                    .state
                    .store
                    .source_ingress_custody(&self.assignment)
                    .await
                    .expect("reread exact custody after competing ACK")
                    .expect("retained exact custody");
                assert!(
                    source_fixture_ack_race_committed(
                        &error,
                        ledger.state,
                        &self.registration,
                        receipt.confirmation()
                    ),
                    "receipt ACK failed without an exact committed accounting race: {error}"
                );
            }
            result => panic!("exact actual receipt ACK refused: {result:?}"),
        }
        self.obligation
            .release_after_ack(&receipt)
            .expect("actual closure after durable ACK");
    }
    fn monitor_actor(self, actor: SourceViewerActor) -> SourceFactoryIngressMonitor {
        SourceFactoryIngressMonitor(Some(tokio::spawn(async move {
            loop {
                let changed = actor.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if actor
                    .0
                    .state
                    .lock()
                    .expect("actual Source state")
                    .retirement_requested
                {
                    break;
                }
                changed.await;
            }
            // The production actor owns seal/retirement. This fixture owns
            // only its actual accepted transport and joined receipt/ACK; a
            // second seal CAS would race that actor's retained cleanup owner.
            self.close_transport(false, Some(actor)).await;
        })))
    }
}

async fn source_two_owner_generation_fixture(
    state: Arc<crate::state::AppState>,
    manager: Arc<TranscodeManager>,
    prepared_a: crate::http::hls::PreparedSourcePlayback,
    assignment_a: SourceDispatchAssignment,
    prepared_b: crate::http::hls::PreparedSourcePlayback,
    assignment_b: SourceDispatchAssignment,
) {
    let ingress_a = Box::pin(SourceFactoryIngressFixture::new(
        Arc::clone(&state),
        &assignment_a,
    ))
    .await;
    let initial_a = ingress_a.permission().await;
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .expect("first members")
        .expect("first floor");
    let SourceWriteAuthorityRead::Ready(activation_a) = state
        .store
        .prepare_source_activation_authority(&assignment_a, &state.sharing.key, &members)
        .await
        .expect("first activation")
    else {
        panic!("first activation");
    };
    let actor_a = Box::pin(manager.start_source_worker(
        Arc::clone(&state),
        assignment_a.clone(),
        *activation_a,
        initial_a.clone(),
        prepared_a,
        Instant::now() + Duration::from_secs(15),
    ))
    .await
    .expect("first actual owner");
    let monitor_a = ingress_a.monitor_actor(actor_a.clone());
    actor_a
        .wait_ready(Instant::now() + Duration::from_secs(15))
        .await
        .expect("first owner ready");
    let ingress_b = Box::pin(SourceFactoryIngressFixture::new(
        Arc::clone(&state),
        &assignment_b,
    ))
    .await;
    let initial_b = ingress_b.permission().await;
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .expect("second members")
        .expect("second floor");
    let SourceWriteAuthorityRead::Ready(activation_b) = state
        .store
        .prepare_source_activation_authority(&assignment_b, &state.sharing.key, &members)
        .await
        .expect("second activation")
    else {
        panic!("second activation");
    };
    let actor_b = Box::pin(manager.start_source_worker(
        Arc::clone(&state),
        assignment_b.clone(),
        *activation_b,
        initial_b.clone(),
        prepared_b,
        Instant::now() + Duration::from_secs(15),
    ))
    .await
    .expect("second actual owner");
    let monitor_b = ingress_b.monitor_actor(actor_b.clone());
    actor_b
        .wait_ready(Instant::now() + Duration::from_secs(15))
        .await
        .expect("second owner ready");
    tokio::time::timeout(Duration::from_secs(10), actor_a.retire())
        .await
        .expect("first retirement deadline")
        .expect("first actual retirement");
    monitor_a.join().await;
    assert!(manager.lookup_source_worker(&assignment_a).is_none());
    assert!(manager.lookup_source_worker(&assignment_b).is_some());
    // Exercise the same private per-owner collection used by rendition dispatch;
    // one released permission cannot replace its surviving owner's authority.
    assert!(actor_b
        .0
        .gate
        .authorize_generation(&[initial_a, initial_b])
        .await
        .is_ok());
    let opened = actor_b
        .open_resource(
            &SharingHlsResource::parse("index.m3u8").expect("typed survivor resource"),
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect("surviving owner still serves media");
    let (payload, guard) = opened.into_parts();
    assert!(
        matches!(payload, SourceResourcePayload::Playlist(bytes) if bytes.starts_with(b"#EXTM3U"))
    );
    drop(guard);
    tokio::time::timeout(Duration::from_secs(10), actor_b.retire())
        .await
        .expect("second retirement deadline")
        .expect("second actual retirement");
    monitor_b.join().await;
}

async fn source_actual_unknown_register_reconciliation(
    state: Arc<crate::state::AppState>,
    manager: Arc<TranscodeManager>,
    assignment: SourceDispatchAssignment,
) {
    use plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite;
    let fixture = Box::pin(SourceFactoryIngressFixture::new_registration_outcome(
        Arc::clone(&state),
        &assignment,
        true,
    ))
    .await;
    let obligation = fixture.obligation.clone();
    let registration = fixture.registration.clone();
    // Exact pending metadata remains visible through the common gate after a
    // canceled reply, despite the positive durable Source registration.
    drop(
        state
            .sharing
            .accepted_drivers
            .reconcile_guard(&obligation)
            .await
            .expect("unknown Register retained pending fence"),
    );
    let snapshot = state
        .store
        .source_ingress_custody(&assignment)
        .await
        .expect("Source guarded snapshot")
        .expect("Source ledger");
    assert!(snapshot
        .state
        .open()
        .any(|slot| slot.same_driver(&registration)));
    assert_eq!(snapshot.owner_identity, assignment.custody_identity());
    fixture.close_transport(false, None).await;
    let receipt = obligation.joined().await;
    let snapshot = state
        .store
        .source_ingress_custody(&assignment)
        .await
        .expect("actual closure Source snapshot")
        .expect("retained ledger");
    assert!(snapshot.state.open().next().is_none());
    let encoded: serde_json::Value =
        serde_json::from_str(&snapshot.state.encode().expect("exact Source state"))
            .expect("Source JSON");
    assert!(encoded["slots"]
        .as_array()
        .expect("Source slots")
        .iter()
        .any(
            |slot| slot["closed_confirmation"].as_str() == Some(receipt.confirmation())
                && slot["registration_sequence"].as_u64()
                    == Some(registration.registration_sequence)
        ));
    let permit = state
        .sharing
        .accepted_drivers
        .reconcile_guard(&obligation)
        .await
        .expect("ACK waiter resumes exact canceled registration");
    let ack = state
        .store
        .acknowledge_source_ingress_custody(&assignment, &registration, receipt.confirmation())
        .await
        .expect("same-boot exact Source ACK fence");
    assert!(matches!(
        ack,
        SourceCustodyWrite::ExactReplay | SourceCustodyWrite::ReconciledClosed
    ));
    // This is deliberately after actual joined closure and positive same-write
    // Source ACK, never after row absence, lease expiry or canceled Register.
    permit.complete();
    assert!(
        state
            .sharing
            .accepted_drivers
            .reconcile_guard(&obligation)
            .await
            .is_err(),
        "definitive Source ACK cleared local unknown reservation"
    );
    let next = Box::pin(SourceFactoryIngressFixture::new(
        Arc::clone(&state),
        &assignment,
    ))
    .await;
    assert_ne!(next.registration.connection_id, registration.connection_id);
    assert!(next.registration.registration_sequence > registration.registration_sequence);
    assert_ne!(
        next.registration.driver_sequence,
        registration.driver_sequence
    );
    assert_eq!(
        state
            .store
            .acknowledge_source_ingress_custody(&assignment, &registration, receipt.confirmation())
            .await
            .expect("actual old receipt reconciles against retained Source highwater"),
        SourceCustodyWrite::ReconciledClosed
    );
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .expect("current members")
        .expect("actual floor");
    assert!(state
        .store
        .prepare_source_ingress_admission(
            &assignment,
            state.sharing.accepted_drivers.boot_id(),
            &members
        )
        .await
        .expect("positive guarded next driver permission")
        .is_some());
    next.close().await;
    assert_eq!(
        state
            .store
            .settle_source_assigned_without_activation(&assignment)
            .await
            .expect("exact sealed actual driver closure"),
        SourceReleaseOutcome::Released
    );
    assert!(manager.lookup_source_worker(&assignment).is_none());
}

#[test]
fn source_fixture_ack_race_requires_exact_sealed_settled_receipt() {
    use plurx_core::error::StoreError;
    use plurx_core::sharing_ingress_custody::{IngressCustodyState, IngressRegistration};
    let registration = IngressRegistration {
        node_id: "fixture".into(),
        boot_id: uuid::Uuid::new_v4(),
        connection_id: uuid::Uuid::new_v4(),
        driver_sequence: 1,
        registration_sequence: 1,
        closed_confirmation: None,
    };
    let confirmation = "a".repeat(64);
    let race = StoreError::Database(
        "NOT NULL constraint failed: sharing_source_session_bindings.incarnation_id".into(),
    );
    let mut ledger = IngressCustodyState::default();
    ledger.register(registration.clone());
    assert!(!source_fixture_ack_race_committed(
        &race,
        ledger.clone(),
        &registration,
        &confirmation
    ));
    ledger.acknowledge(&registration, &confirmation);
    assert!(!source_fixture_ack_race_committed(
        &race,
        ledger.clone(),
        &registration,
        &confirmation
    ));
    ledger.seal();
    assert!(source_fixture_ack_race_committed(
        &race,
        ledger.clone(),
        &registration,
        &confirmation
    ));
    assert!(!source_fixture_ack_race_committed(
        &StoreError::Database("disk I/O error".into()),
        ledger.clone(),
        &registration,
        &confirmation
    ));
    assert!(!source_fixture_ack_race_committed(
        &race,
        ledger.clone(),
        &registration,
        &"b".repeat(64)
    ));
    let mut other = registration.clone();
    other.registration_sequence += 1;
    assert!(!source_fixture_ack_race_committed(
        &race,
        ledger.clone(),
        &other,
        &confirmation
    ));
    ledger.compact_settled();
    assert!(!source_fixture_ack_race_committed(
        &race,
        ledger,
        &registration,
        &confirmation
    ));
}

#[tokio::test]
async fn source_fixture_monitor_join_propagates_task_failure() {
    let monitor = SourceFactoryIngressMonitor(Some(tokio::spawn(async {
        panic!("injected fixture monitor failure")
    })));
    let joined = tokio::spawn(monitor.join()).await;
    assert!(joined
        .expect_err("monitor panic must reach the fixture")
        .is_panic());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_assigned_owner_cached_caption_revision_reads_retained_bytes_and_refuses_stale_identity(
) {
    Box::pin(source_copy_preadmission_fixture(56)).await;
}
