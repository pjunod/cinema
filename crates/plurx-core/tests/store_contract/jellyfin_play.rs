use super::*;
#[cfg(feature = "hiqlite-contract-tests")]
use plurx_core::store::JellyfinPlayStore;
use plurx_core::store::{
    JellyfinClientFamily as Family, JellyfinEntityKind as Kind, JellyfinLoginWrite,
    JellyfinPlayActivation as Activation, JellyfinPlayScope as Scope, NewJellyfinPlay,
    JELLYFIN_PENDING_PLAY_TTL_MS,
};
fn digest(label: &str) -> String {
    plurx_core::auth::hash_token(label)
}
async fn fixture(store: &Arc<dyn Store>) -> NewJellyfinPlay {
    let (uid, file_id) = seed_file(store, "jellyfin-play").await;
    let file = store
        .get_file(file_id)
        .await
        .expect("file query")
        .expect("file row");
    let item_wire = store
        .jellyfin_entity_ids(Kind::Item, &[file.item_id])
        .await
        .expect("item ID");
    let file_wire = store
        .jellyfin_entity_ids(Kind::File, &[file_id])
        .await
        .expect("file ID");
    let scope = Scope {
        user_id: uid,
        token_digest: digest("play-login"),
        device_digest: digest("play-device"),
        client_family: Family::Infuse,
    };
    assert!(store
        .replace_jellyfin_login(
            JellyfinLoginWrite {
                token_hash: scope.token_digest.clone(),
                user_id: uid,
                device_digest: scope.device_digest.clone(),
                client_family: scope.client_family,
                device_label: None,
                expected_password_hash: "hash".into(),
                created_at: unix_seconds()
            },
            None
        )
        .await
        .expect("scoped login"));
    NewJellyfinPlay {
        play_id: uuid::Uuid::new_v4().simple().to_string(),
        scope,
        playback_id: "jellyfin-player".into(),
        item_id: file.item_id,
        file_id,
        item_wire_id: item_wire[0].wire_id.clone(),
        file_wire_id: file_wire[0].wire_id.clone(),
        source_fingerprint: digest("source"),
        profile_fingerprint: digest("profile"),
        native_request_fingerprint: "a".repeat(64),
        selection_json: "{\"audio\":0,\"subtitle\":null}".into(),
        source_origin_ms: 120_000,
        created_at_ms: 1_000,
    }
}
#[tokio::test]
async fn jellyfin_pending_admission_expiry_and_terminal_fences_preserve_active_play() {
    for_each_backend(|store, backend| async move {
        let active = fixture(&store).await;
        assert!(store
            .create_jellyfin_play(active.clone())
            .await
            .expect("active negotiation"));
        let grant = "jellyfin-direct-grant";
        store
            .create_file_grant(plurx_core::store::NewFileGrant {
                id: grant.into(),
                token_hash: digest("direct-file-token"),
                file_id: active.file_id,
                user_id: active.scope.user_id,
                source_token_hash: active.scope.token_digest.clone(),
                created_at: unix_seconds(),
                expires_at: i64::MAX,
            })
            .await
            .expect("direct grant");
        assert!(store
            .activate_jellyfin_play(
                &active.play_id,
                &active.scope,
                Activation::DirectGrant(grant.into()),
                1_001
            )
            .await
            .expect("direct activation"));
        for _ in 0..64 {
            let mut pending = active.clone();
            pending.play_id = uuid::Uuid::new_v4().simple().to_string();
            assert!(
                store
                    .create_jellyfin_play(pending)
                    .await
                    .expect("pending admission"),
                "{backend}"
            );
        }
        let mut overflow = active.clone();
        overflow.play_id = uuid::Uuid::new_v4().simple().to_string();
        assert!(!store
            .create_jellyfin_play(overflow.clone())
            .await
            .expect("pending capacity"));
        let retained = store
            .jellyfin_play(&active.play_id, &active.scope)
            .await
            .expect("active read")
            .expect("active retained");
        assert_eq!(retained.state, "active");
        assert_eq!(retained.negotiation.source_origin_ms, 120_000);
        assert_eq!(retained.direct_grant_id.as_deref(), Some(grant));
        overflow.created_at_ms += JELLYFIN_PENDING_PLAY_TTL_MS + 1;
        assert!(store
            .create_jellyfin_play(overflow.clone())
            .await
            .expect("expired pending cleanup"));
        assert_eq!(
            store
                .jellyfin_play(&active.play_id, &active.scope)
                .await
                .expect("active read")
                .expect("active row")
                .state,
            "active"
        );
        assert!(store
            .end_jellyfin_play(&active.play_id, &active.scope, overflow.created_at_ms)
            .await
            .expect("exact terminal"));
        assert!(!store
            .activate_jellyfin_play(
                &active.play_id,
                &active.scope,
                Activation::DirectGrant(grant.into()),
                overflow.created_at_ms
            )
            .await
            .expect("late activation refused"));
        let terminal = store
            .jellyfin_play(&active.play_id, &active.scope)
            .await
            .expect("terminal read")
            .expect("terminal tombstone");
        assert_eq!(terminal.state, "ended");
        assert_eq!(terminal.direct_grant_id.as_deref(), Some(grant));
        let mut other_scope = active.scope.clone();
        other_scope.device_digest = digest("other-device");
        assert!(store
            .jellyfin_play(&active.play_id, &other_scope)
            .await
            .expect("other device read")
            .is_none());
        assert!(!store
            .end_jellyfin_play(&overflow.play_id, &other_scope, overflow.created_at_ms)
            .await
            .expect("other device stop"));
        // Expired pending bindings cannot activate even before another create
        // has physically removed their rows.
        assert!(!store
            .activate_jellyfin_play(
                &overflow.play_id,
                &overflow.scope,
                Activation::DirectGrant(grant.into()),
                overflow.created_at_ms + JELLYFIN_PENDING_PLAY_TTL_MS
            )
            .await
            .expect("pending expiry fence"));
    })
    .await;
}
#[tokio::test]
async fn jellyfin_media_binding_resolves_transferred_owner_and_never_retargets_late_stop() {
    for_each_backend(|store, backend| async move {
        let mut first = fixture(&store).await;
        first.source_origin_ms = 0;
        assert!(store
            .create_jellyfin_play(first.clone())
            .await
            .expect("first negotiation"));
        let native = current_media_session(
            store.as_ref(),
            first.scope.user_id,
            &first.playback_id,
            "11111111-1111-4111-8111-111111111101",
            "11111111-1111-4111-8111-111111111102",
            backend,
        )
        .await;
        assert!(store
            .activate_jellyfin_play(
                &first.play_id,
                &first.scope,
                Activation::MediaIncarnation(native.incarnation_id.clone()),
                1_001
            )
            .await
            .expect("bind native route"));
        let mut wrong_request = first.clone();
        wrong_request.play_id = uuid::Uuid::new_v4().simple().to_string();
        wrong_request.native_request_fingerprint = digest("another native request");
        assert!(store
            .create_jellyfin_play(wrong_request.clone())
            .await
            .expect("different request negotiation"));
        assert!(!store
            .activate_jellyfin_play(
                &wrong_request.play_id,
                &wrong_request.scope,
                Activation::MediaIncarnation(native.incarnation_id.clone()),
                1_001
            )
            .await
            .expect("native request fingerprint fence"));
        let mut wrong_origin = first.clone();
        wrong_origin.play_id = uuid::Uuid::new_v4().simple().to_string();
        wrong_origin.source_origin_ms = 1_000;
        assert!(store
            .create_jellyfin_play(wrong_origin.clone())
            .await
            .expect("different origin negotiation"));
        assert!(!store
            .activate_jellyfin_play(
                &wrong_origin.play_id,
                &wrong_origin.scope,
                Activation::MediaIncarnation(native.incarnation_id.clone()),
                1_001
            )
            .await
            .expect("native source origin fence"));
        let mut alias = first.clone();
        alias.play_id = uuid::Uuid::new_v4().simple().to_string();
        assert!(store
            .create_jellyfin_play(alias.clone())
            .await
            .expect("alias negotiation"));
        assert!(
            store
                .activate_jellyfin_play(
                    &alias.play_id,
                    &alias.scope,
                    Activation::MediaIncarnation(native.incarnation_id.clone()),
                    1_001
                )
                .await
                .is_err(),
            "a native incarnation cannot acquire a second play UUID"
        );
        let route = store
            .media_session_route_by_incarnation(&native.incarnation_id)
            .await
            .expect("native read")
            .expect("native route");
        let moved = store
            .claim_media_session_takeover(&MediaSessionTakeover {
                incarnation_id: route.incarnation_id.clone(),
                expected_owner_node_id: route.owner_node_id,
                expected_owner_epoch: route.owner_epoch,
                next_owner_node_id: "next-node".into(),
                now_ms: 900_001,
                lease_expires_at_ms: 1_800_000,
            })
            .await
            .expect("native owner transfer")
            .expect("transferred route");
        let bound = store
            .jellyfin_play(&first.play_id, &first.scope)
            .await
            .expect("binding after transfer")
            .expect("binding row");
        assert_eq!(
            bound.native_incarnation_id.as_deref(),
            Some(native.incarnation_id.as_str())
        );
        assert_eq!(
            store
                .media_session_route_by_incarnation(
                    bound
                        .native_incarnation_id
                        .as_deref()
                        .expect("native reference")
                )
                .await
                .expect("resolve current owner")
                .expect("current route")
                .owner_node_id,
            "next-node"
        );
        assert!(store
            .end_jellyfin_play(&first.play_id, &first.scope, 900_002)
            .await
            .expect("old play terminal"));
        let mut next = first.clone();
        next.play_id = uuid::Uuid::new_v4().simple().to_string();
        next.created_at_ms = 900_003;
        assert!(store
            .create_jellyfin_play(next.clone())
            .await
            .expect("successor negotiation"));
        let mut successor = native.clone();
        successor.incarnation_id = "11111111-1111-4111-8111-111111111103".into();
        successor.session_id = "11111111-1111-4111-8111-111111111104".into();
        successor.expected_predecessor_incarnation_id = Some(moved.incarnation_id.clone());
        successor.fence_predecessor = true;
        successor.now_ms = 900_003;
        successor.lease_expires_at_ms = 1_800_000;
        assert!(store
            .activate_media_session(&successor)
            .await
            .expect("native successor")
            .is_some());
        let blocked = confirm_media_activation(
            store.as_ref(),
            &successor,
            successor.now_ms + MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
            backend,
        )
        .await;
        assert!(!store
            .activate_jellyfin_play(
                &next.play_id,
                &next.scope,
                Activation::MediaIncarnation(successor.incarnation_id.clone()),
                900_004
            )
            .await
            .expect("unpublished native route fence"));
        store
            .complete_media_session_handoff(
                &blocked.incarnation_id,
                &blocked.owner_node_id,
                blocked.owner_epoch,
                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                900_004,
            )
            .await
            .expect("native predecessor handoff")
            .expect("published successor");
        assert!(store
            .activate_jellyfin_play(
                &next.play_id,
                &next.scope,
                Activation::MediaIncarnation(successor.incarnation_id.clone()),
                900_004
            )
            .await
            .expect("successor binding"));
        assert!(!store
            .end_jellyfin_play(&first.play_id, &first.scope, 900_005)
            .await
            .expect("late old stop"));
        assert_eq!(
            store
                .media_session_route_by_incarnation(&successor.incarnation_id)
                .await
                .expect("successor route")
                .expect("successor retained")
                .state,
            "active"
        );
        assert_eq!(
            store
                .jellyfin_play(&first.play_id, &first.scope)
                .await
                .expect("old tombstone")
                .expect("old mapping")
                .native_incarnation_id
                .as_deref(),
            Some(native.incarnation_id.as_str())
        );
        assert_eq!(
            store
                .jellyfin_play(&next.play_id, &next.scope)
                .await
                .expect("successor mapping")
                .expect("new mapping")
                .native_incarnation_id
                .as_deref(),
            Some(successor.incarnation_id.as_str())
        );
    })
    .await;
}

#[tokio::test]
async fn jellyfin_binding_refuses_retired_file_incarnation_after_integer_reuse() {
    for_each_backend(|store, backend| async move {
        let mut old = fixture(&store).await;
        old.source_origin_ms = 0;
        assert!(store
            .create_jellyfin_play(old.clone())
            .await
            .expect("old negotiation"));
        assert_eq!(
            store
                .delete_files(&[old.file_id])
                .await
                .expect("delete source"),
            1
        );
        let replacement = store
            .upsert_file(
                old.item_id,
                "/jellyfin-play/replacement.mkv",
                20_000,
                2,
                &ProbeResult {
                    duration_ms: Some(7_200_000),
                    container: Some("mkv".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("replacement source");
        assert_eq!(
            replacement, old.file_id,
            "{backend}: fixture must exercise integer reuse"
        );
        let ids = store
            .jellyfin_entity_ids(Kind::File, &[replacement])
            .await
            .expect("new source mapping");
        assert_ne!(ids[0].wire_id, old.file_wire_id);
        let native = current_media_session(
            store.as_ref(),
            old.scope.user_id,
            &old.playback_id,
            "11111111-1111-4111-8111-111111111105",
            "11111111-1111-4111-8111-111111111106",
            backend,
        )
        .await;
        assert!(!store
            .activate_jellyfin_play(
                &old.play_id,
                &old.scope,
                Activation::MediaIncarnation(native.incarnation_id.clone()),
                1_001
            )
            .await
            .expect("old incarnation fence"));
        let mut current = old.clone();
        current.play_id = uuid::Uuid::new_v4().simple().to_string();
        current.file_wire_id = ids[0].wire_id.clone();
        current.source_fingerprint = digest("replacement-source");
        assert!(store
            .create_jellyfin_play(current.clone())
            .await
            .expect("current negotiation"));
        assert!(store
            .activate_jellyfin_play(
                &current.play_id,
                &current.scope,
                Activation::MediaIncarnation(native.incarnation_id),
                1_001
            )
            .await
            .expect("current incarnation binding"));
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_play_import_preserves_terminal_and_active_references_on_another_node() {
    let _case = HIQLITE_CASE.lock().await;
    let source = tempfile::tempdir().expect("play import source");
    let path = source
        .path()
        .join(plurx_core::cluster::migration::SQLITE_FILENAME);
    let local: Arc<dyn Store> = Arc::new(SqliteStore::open(&path).expect("local source"));
    local
        .put_setting(plurx_core::store::keys::INSTANCE_ID, CONTRACT_INSTANCE_ID)
        .await
        .expect("instance ID");
    let mut terminal = fixture(&local).await;
    terminal.source_origin_ms = 0;
    assert!(local
        .create_jellyfin_play(terminal.clone())
        .await
        .expect("terminal negotiation"));
    let native = current_media_session(
        local.as_ref(),
        terminal.scope.user_id,
        &terminal.playback_id,
        "11111111-1111-4111-8111-111111111107",
        "11111111-1111-4111-8111-111111111108",
        "import",
    )
    .await;
    assert!(local
        .activate_jellyfin_play(
            &terminal.play_id,
            &terminal.scope,
            Activation::MediaIncarnation(native.incarnation_id.clone()),
            1_001
        )
        .await
        .expect("terminal native binding"));
    assert!(local
        .end_jellyfin_play(&terminal.play_id, &terminal.scope, 1_002)
        .await
        .expect("terminal mapping"));
    let mut active = terminal.clone();
    active.play_id = uuid::Uuid::new_v4().simple().to_string();
    active.source_origin_ms = 120_000;
    assert!(local
        .create_jellyfin_play(active.clone())
        .await
        .expect("active negotiation"));
    local
        .create_file_grant(plurx_core::store::NewFileGrant {
            id: "imported-play-grant".into(),
            token_hash: digest("imported-play-token"),
            file_id: active.file_id,
            user_id: active.scope.user_id,
            source_token_hash: active.scope.token_digest.clone(),
            created_at: unix_seconds(),
            expires_at: i64::MAX,
        })
        .await
        .expect("active direct grant");
    assert!(local
        .activate_jellyfin_play(
            &active.play_id,
            &active.scope,
            Activation::DirectGrant("imported-play-grant".into()),
            1_003
        )
        .await
        .expect("active direct binding"));
    drop(local);
    let prepared = prepare_sqlite_import(source.path()).expect("prepare play import");
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import play mappings");
    drop(store);
    let mut addresses = cluster.addresses.clone();
    addresses.rotate_left(1);
    let client = Client::remote(
        addresses,
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("alternate client");
    let store = HiqliteAuthStore::open(
        client,
        &cluster._root.path().join("play-import-telemetry.db"),
    )
    .await
    .expect("alternate store");
    let restored_terminal = store
        .jellyfin_play(&terminal.play_id, &terminal.scope)
        .await
        .expect("terminal lookup")
        .expect("terminal preserved");
    assert_eq!(restored_terminal.state, "ended");
    assert_eq!(
        restored_terminal.native_incarnation_id.as_deref(),
        Some(native.incarnation_id.as_str())
    );
    assert_eq!(
        store
            .media_session_route_by_incarnation(&native.incarnation_id)
            .await
            .expect("native imported reference")
            .expect("native route")
            .user_id,
        terminal.scope.user_id
    );
    let restored_active = store
        .jellyfin_play(&active.play_id, &active.scope)
        .await
        .expect("active lookup")
        .expect("active preserved");
    assert_eq!(restored_active.state, "active");
    assert_eq!(
        restored_active.direct_grant_id.as_deref(),
        Some("imported-play-grant")
    );
    assert_eq!(restored_active.negotiation.source_origin_ms, 120_000);
    assert_eq!(
        restored_active.negotiation.selection_json,
        active.selection_json
    );
    assert!(!store
        .activate_jellyfin_play(
            &terminal.play_id,
            &terminal.scope,
            Activation::MediaIncarnation(native.incarnation_id),
            1_004
        )
        .await
        .expect("imported terminal cannot revive"));
}

#[tokio::test]
async fn jellyfin_server_pending_limit_is_atomic_across_login_scopes() {
    for_each_backend(|store, backend| async move {
        let active = fixture(&store).await;
        assert!(store
            .create_jellyfin_play(active.clone())
            .await
            .expect("active negotiation"));
        store
            .create_file_grant(plurx_core::store::NewFileGrant {
                id: "global-limit-active-grant".into(),
                token_hash: digest("global-limit-file-token"),
                file_id: active.file_id,
                user_id: active.scope.user_id,
                source_token_hash: active.scope.token_digest.clone(),
                created_at: unix_seconds(),
                expires_at: i64::MAX,
            })
            .await
            .expect("active grant");
        assert!(store
            .activate_jellyfin_play(
                &active.play_id,
                &active.scope,
                Activation::DirectGrant("global-limit-active-grant".into()),
                1_001
            )
            .await
            .expect("active reference"));
        let mut contenders = Vec::new();
        for device in 0..66 {
            let mut template = active.clone();
            template.scope.token_digest = digest(&format!("server-limit-token-{device}"));
            template.scope.device_digest = digest(&format!("server-limit-device-{device}"));
            template.playback_id = format!("server-limit-player-{device}");
            assert!(store
                .replace_jellyfin_login(
                    JellyfinLoginWrite {
                        token_hash: template.scope.token_digest.clone(),
                        user_id: template.scope.user_id,
                        device_digest: template.scope.device_digest.clone(),
                        client_family: template.scope.client_family,
                        device_label: None,
                        expected_password_hash: "hash".into(),
                        created_at: unix_seconds(),
                    },
                    None
                )
                .await
                .expect("distinct device login"));
            if device >= 64 {
                template.play_id = uuid::Uuid::new_v4().simple().to_string();
                contenders.push(template);
                continue;
            }
            let pending = if device == 63 { 63 } else { 64 };
            for _ in 0..pending {
                let mut play = template.clone();
                play.play_id = uuid::Uuid::new_v4().simple().to_string();
                assert!(
                    store
                        .create_jellyfin_play(play)
                        .await
                        .expect("fill server pending budget"),
                    "{backend}"
                );
            }
        }
        let (first, second) = tokio::join!(
            store.create_jellyfin_play(contenders[0].clone()),
            store.create_jellyfin_play(contenders[1].clone()),
        );
        assert_eq!(
            usize::from(first.expect("first concurrent admission"))
                + usize::from(second.expect("second concurrent admission")),
            1,
            "{backend}: one remaining server slot has exactly one winner"
        );
        let mut refused = contenders[0].clone();
        refused.play_id = uuid::Uuid::new_v4().simple().to_string();
        assert!(!store
            .create_jellyfin_play(refused)
            .await
            .expect("full server refusal"));
        assert_eq!(
            store
                .jellyfin_play(&active.play_id, &active.scope)
                .await
                .expect("active lookup")
                .expect("active survives server pressure")
                .state,
            "active",
            "{backend}"
        );
    })
    .await;
}
