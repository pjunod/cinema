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
async fn jellyfin_direct_activation_fences_prior_play_without_negotiation_or_replay_eviction() {
    for_each_backend(|store, backend| async move {
        let old = manual_active(&store, fixture(&store).await).await;
        let mut next = old.clone();
        next.play_id = uuid::Uuid::new_v4().simple().to_string();
        next.created_at_ms = 1_002;
        assert!(store
            .create_jellyfin_play(next.clone())
            .await
            .expect("pending"));
        assert_eq!(
            store
                .jellyfin_play(&old.play_id, &old.scope)
                .await
                .expect("old")
                .expect("row")
                .state,
            "active"
        );
        let grant = uuid::Uuid::new_v4().to_string();
        store
            .create_file_grant(plurx_core::store::NewFileGrant {
                id: grant.clone(),
                token_hash: digest(&grant),
                file_id: next.file_id,
                user_id: next.scope.user_id,
                source_token_hash: next.scope.token_digest.clone(),
                created_at: unix_seconds(),
                expires_at: i64::MAX,
            })
            .await
            .expect("grant");
        assert!(store
            .activate_jellyfin_play(
                &next.play_id,
                &next.scope,
                Activation::DirectGrant(grant.clone()),
                1_003
            )
            .await
            .expect("activate"));
        assert_eq!(
            store
                .jellyfin_play(&old.play_id, &old.scope)
                .await
                .expect("old")
                .expect("row")
                .state,
            "ended",
            "{backend}"
        );
        let mut newer = next.clone();
        newer.play_id = uuid::Uuid::new_v4().simple().to_string();
        // Even an equal clock reading belongs to a fresh later negotiation.
        assert!(store
            .create_jellyfin_play(newer.clone())
            .await
            .expect("new pending"));
        assert!(!store
            .activate_jellyfin_play(
                &next.play_id,
                &next.scope,
                Activation::DirectGrant(grant),
                1_004
            )
            .await
            .expect("replay"));
        assert_eq!(
            store
                .jellyfin_play(&newer.play_id, &newer.scope)
                .await
                .expect("newer")
                .expect("row")
                .state,
            "pending",
            "{backend}: replay must not evict fresh metadata"
        );
        assert!(store
            .put_jellyfin_progress(progress_write(&old, 0, 9000, true), None)
            .await
            .expect("late final")
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn jellyfin_native_pointer_activation_atomically_fences_prior_events_and_keeps_newer_asks() {
    for_each_backend(|store, backend| async move {
        let mut old = fixture(&store).await;
        old.native_request_fingerprint = digest("old direct identity");
        let old = manual_active(&store, old).await;
        assert!(store
            .put_jellyfin_progress(progress_write(&old, 0, 1000, false), None)
            .await
            .expect("old progress")
            .is_some());
        let mut stale = old.clone();
        stale.play_id = uuid::Uuid::new_v4().simple().to_string();
        stale.created_at_ms = 1_000;
        assert!(store
            .create_jellyfin_play(stale.clone())
            .await
            .expect("stale ask"));
        let mut selected = old.clone();
        selected.play_id = uuid::Uuid::new_v4().simple().to_string();
        selected.native_request_fingerprint = "a".repeat(64);
        selected.created_at_ms = 1_000;
        assert!(store
            .create_jellyfin_play(selected.clone())
            .await
            .expect("selected ask"));
        let mut newer = stale.clone();
        newer.play_id = uuid::Uuid::new_v4().simple().to_string();
        newer.created_at_ms = 1_000;
        assert!(store
            .create_jellyfin_play(newer.clone())
            .await
            .expect("newer ask"));
        let mut other_player = old.clone();
        other_player.play_id = uuid::Uuid::new_v4().simple().to_string();
        other_player.playback_id = "other-player".into();
        let other_player = manual_active(&store, other_player).await;
        assert_eq!(
            store
                .jellyfin_play(&old.play_id, &old.scope)
                .await
                .expect("old")
                .expect("binding")
                .state,
            "active",
            "{backend}: pending asks must leave incumbent live"
        );
        current_media_session(
            store.as_ref(),
            selected.scope.user_id,
            &selected.playback_id,
            "11111111-1111-4111-8111-111111111301",
            "11111111-1111-4111-8111-111111111302",
            backend,
        )
        .await;
        for (play, expected) in [
            (&old, "ended"),
            (&stale, "ended"),
            (&selected, "pending"),
            (&newer, "pending"),
            (&other_player, "active"),
        ] {
            assert_eq!(
                store
                    .jellyfin_play(&play.play_id, &play.scope)
                    .await
                    .expect("read")
                    .expect("binding")
                    .state,
                expected,
                "{backend}: {}",
                play.play_id
            );
        }
        assert!(store
            .put_jellyfin_progress(progress_write(&old, 0, 9000, true), None)
            .await
            .expect("late old final")
            .is_none());
        assert_eq!(
            store
                .watch_state(old.scope.user_id, old.item_id)
                .await
                .expect("watch")
                .expect("prior watch")
                .position_ms,
            1000
        );
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

async fn manual_active(store: &Arc<dyn Store>, play: NewJellyfinPlay) -> NewJellyfinPlay {
    assert!(store
        .create_jellyfin_play(play.clone())
        .await
        .expect("negotiation"));
    let grant = uuid::Uuid::new_v4().to_string();
    store
        .create_file_grant(plurx_core::store::NewFileGrant {
            id: grant.clone(),
            token_hash: digest(&grant),
            file_id: play.file_id,
            user_id: play.scope.user_id,
            source_token_hash: play.scope.token_digest.clone(),
            created_at: unix_seconds(),
            expires_at: i64::MAX,
        })
        .await
        .expect("grant");
    assert!(store
        .activate_jellyfin_play(
            &play.play_id,
            &play.scope,
            Activation::DirectGrant(grant),
            1_001
        )
        .await
        .expect("activation"));
    play
}
#[tokio::test]
async fn jellyfin_manual_edits_advance_only_eligible_own_play_and_never_revive_an_external_fence() {
    for_each_backend(|store, backend| async move {
        let initial = fixture(&store).await;
        let play = manual_active(&store, initial).await;
        assert_eq!(
            store
                .set_watched_tree_with_origin(
                    play.scope.user_id,
                    play.item_id,
                    true,
                    Some(&play.scope)
                )
                .await
                .expect("own edit"),
            vec![play.item_id],
            "{backend}"
        );
        assert_eq!(
            store
                .jellyfin_play(&play.play_id, &play.scope)
                .await
                .expect("play")
                .expect("play")
                .manual_revision,
            1
        );
        let before = store
            .watch_state(play.scope.user_id, play.item_id)
            .await
            .expect("watch")
            .expect("watch");
        assert!(store
            .set_watched_tree_with_origin(play.scope.user_id, play.item_id, true, Some(&play.scope))
            .await
            .expect("own explicit no-op")
            .is_empty());
        assert_eq!(
            store
                .watch_state(play.scope.user_id, play.item_id)
                .await
                .expect("watch"),
            Some(before)
        );
        assert_eq!(
            store
                .jellyfin_play(&play.play_id, &play.scope)
                .await
                .expect("play")
                .expect("play")
                .manual_revision,
            2
        );
        store
            .set_watched(play.scope.user_id, play.item_id, false)
            .await
            .expect("external native edit");
        assert_eq!(
            store
                .jellyfin_play(&play.play_id, &play.scope)
                .await
                .expect("play")
                .expect("play")
                .manual_revision,
            2
        );
        assert!(store
            .set_watched_tree_with_origin(
                play.scope.user_id,
                play.item_id,
                false,
                Some(&play.scope)
            )
            .await
            .expect("fenced own edit")
            .is_empty());
        assert_eq!(
            store
                .jellyfin_play(&play.play_id, &play.scope)
                .await
                .expect("play")
                .expect("play")
                .manual_revision,
            2
        );
        let mut fresh = play.clone();
        fresh.play_id = uuid::Uuid::new_v4().simple().to_string();
        assert!(store
            .create_jellyfin_play(fresh.clone())
            .await
            .expect("fresh negotiation"));
        assert_eq!(
            store
                .jellyfin_play(&fresh.play_id, &fresh.scope)
                .await
                .expect("fresh play")
                .expect("fresh play")
                .manual_revision,
            4
        );
    })
    .await;
}
#[tokio::test]
async fn jellyfin_manual_own_edit_refuses_ambiguous_scope_and_terminal_reduction_cannot_revive_it()
{
    for_each_backend(|store, backend| async move {
        let initial = fixture(&store).await;
        let first = manual_active(&store, initial).await;
        let mut another = first.clone();
        another.play_id = uuid::Uuid::new_v4().simple().to_string();
        another.playback_id = "second-legacy-player".into();
        let second = manual_active(&store, another).await;
        store
            .set_watched_tree_with_origin(
                first.scope.user_id,
                first.item_id,
                true,
                Some(&first.scope),
            )
            .await
            .expect("ambiguous own edit");
        for play in [&first, &second] {
            assert_eq!(
                store
                    .jellyfin_play(&play.play_id, &play.scope)
                    .await
                    .expect("play")
                    .expect("play")
                    .manual_revision,
                0,
                "{backend}"
            );
        }
        assert!(store
            .end_jellyfin_play(&first.play_id, &first.scope, 1_002)
            .await
            .expect("stop first"));
        store
            .set_watched_tree_with_origin(
                second.scope.user_id,
                second.item_id,
                false,
                Some(&second.scope),
            )
            .await
            .expect("later own edit");
        assert_eq!(
            store
                .jellyfin_play(&second.play_id, &second.scope)
                .await
                .expect("second")
                .expect("second")
                .manual_revision,
            0
        );
    })
    .await;
}

fn progress_write(
    play: &NewJellyfinPlay,
    revision: i64,
    position: i64,
    final_commit: bool,
) -> plurx_core::store::JellyfinProgressWrite {
    plurx_core::store::JellyfinProgressWrite {
        provenance: plurx_core::store::JellyfinProgressProvenance {
            play_id: play.play_id.clone(),
            scope: play.scope.clone(),
            manual_revision: revision,
        },
        item_id: play.item_id,
        position_ms: position,
        duration_ms: Some(10_000),
        final_commit,
    }
}
#[tokio::test]
async fn jellyfin_manual_progress_retains_queued_revision_and_final_is_atomic_with_its_exact_tombstone(
) {
    for_each_backend(|store, backend| async move {
        let initial = fixture(&store).await;
        let play = manual_active(&store, initial).await;
        let old = store
            .put_jellyfin_progress(progress_write(&play, 0, 1000, false), None)
            .await
            .expect("leading beat")
            .expect("committed beat");
        store
            .set_watched_tree_with_origin(
                play.scope.user_id,
                play.item_id,
                false,
                Some(&play.scope),
            )
            .await
            .expect("own unwatch");
        assert!(
            store
                .put_jellyfin_progress(progress_write(&play, 0, 1500, false), Some(&old))
                .await
                .expect("queued old beat")
                .is_none(),
            "{backend}"
        );
        let resumed = store
            .put_jellyfin_progress(progress_write(&play, 1, 1800, false), None)
            .await
            .expect("new own observation")
            .expect("new revision commit");
        assert_eq!(resumed.position_ms, 1800);
        store
            .set_watched_tree(play.scope.user_id, play.item_id, true)
            .await
            .expect("external edit");
        let edited = store
            .watch_state(play.scope.user_id, play.item_id)
            .await
            .expect("edited state");
        assert!(store
            .put_jellyfin_progress(progress_write(&play, 1, 9000, true), None)
            .await
            .expect("fenced final")
            .is_none());
        assert_eq!(
            store
                .watch_state(play.scope.user_id, play.item_id)
                .await
                .expect("preserved edit"),
            edited
        );
        let mut fresh = play.clone();
        fresh.play_id = uuid::Uuid::new_v4().simple().to_string();
        fresh.playback_id = "independent-final-player".into();
        let fresh = manual_active(&store, fresh).await;
        let revision = store
            .jellyfin_play(&fresh.play_id, &fresh.scope)
            .await
            .expect("fresh play")
            .expect("fresh play")
            .manual_revision;
        assert_eq!(revision, 2);
        let final_state = store
            .put_jellyfin_progress(progress_write(&fresh, revision, 2500, true), None)
            .await
            .expect("durable final")
            .expect("final row");
        assert_eq!(final_state.position_ms, 2500);
        assert_eq!(
            store
                .jellyfin_play(&fresh.play_id, &fresh.scope)
                .await
                .expect("terminal play")
                .expect("terminal play")
                .state,
            "ended"
        );
        assert!(store
            .put_jellyfin_progress(progress_write(&fresh, revision, 9999, false), None)
            .await
            .expect("late beat")
            .is_none());
        assert!(store
            .put_jellyfin_progress(progress_write(&fresh, revision, 9999, true), None)
            .await
            .expect("duplicate final")
            .is_none());
        assert_eq!(
            store
                .watch_state(play.scope.user_id, play.item_id)
                .await
                .expect("final preserved"),
            Some(final_state)
        );
        assert_eq!(
            store
                .jellyfin_play(&play.play_id, &play.scope)
                .await
                .expect("other play")
                .expect("other play")
                .state,
            "active"
        );
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_manual_import_drops_edit_origin_without_reviving_an_ambiguous_play() {
    let _case = HIQLITE_CASE.lock().await;
    let source = tempfile::tempdir().expect("watch import source");
    let path = source
        .path()
        .join(plurx_core::cluster::migration::SQLITE_FILENAME);
    let local: Arc<dyn Store> = Arc::new(SqliteStore::open(&path).expect("source"));
    local
        .put_setting(plurx_core::store::keys::INSTANCE_ID, CONTRACT_INSTANCE_ID)
        .await
        .expect("instance");
    let initial = fixture(&local).await;
    let first = manual_active(&local, initial).await;
    let mut second = first.clone();
    second.play_id = uuid::Uuid::new_v4().simple().to_string();
    second.playback_id = "second-imported-player".into();
    let second = manual_active(&local, second).await;
    local
        .set_watched_tree_with_origin(first.scope.user_id, first.item_id, true, Some(&first.scope))
        .await
        .expect("ambiguous own edit");
    local
        .end_jellyfin_play(&first.play_id, &first.scope, 1002)
        .await
        .expect("end one play");
    let expected = local
        .watch_state(first.scope.user_id, first.item_id)
        .await
        .expect("watch");
    drop(local);
    let prepared = prepare_sqlite_import(source.path()).expect("prepare");
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import");
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
    let store: Arc<dyn Store> = Arc::new(
        HiqliteAuthStore::open(
            client,
            &cluster._root.path().join("manual-import-telemetry.db"),
        )
        .await
        .expect("alternate store"),
    );
    let restored = store
        .jellyfin_play(&second.play_id, &second.scope)
        .await
        .expect("binding")
        .expect("binding");
    assert_eq!(restored.state, "active");
    assert_eq!(
        restored.manual_revision, 0,
        "import must not replay a trusted edit context"
    );
    assert!(store
        .put_jellyfin_progress(progress_write(&second, 0, 5000, false), None)
        .await
        .expect("fenced progress")
        .is_none());
    assert_eq!(
        store
            .watch_state(first.scope.user_id, first.item_id)
            .await
            .expect("preserved"),
        expected
    );
    let mut fresh = second.clone();
    fresh.play_id = uuid::Uuid::new_v4().simple().to_string();
    let fresh = manual_active(&store, fresh).await;
    assert_eq!(
        store
            .jellyfin_play(&fresh.play_id, &fresh.scope)
            .await
            .expect("fresh")
            .expect("fresh")
            .manual_revision,
        1
    );
    assert!(store
        .put_jellyfin_progress(progress_write(&fresh, 1, 6000, false), None)
        .await
        .expect("fresh progress")
        .is_some());
}

#[tokio::test]
async fn jellyfin_manual_edit_cannot_advance_a_released_play_waiting_for_terminal_retry() {
    for_each_backend(|store, backend| async move {
        let initial = fixture(&store).await;
        let play = manual_active(&store, initial).await;
        store
            .put_jellyfin_progress(progress_write(&play, 0, 1000, false), None)
            .await
            .expect("initial")
            .expect("initial");
        let binding = store
            .jellyfin_play(&play.play_id, &play.scope)
            .await
            .expect("binding")
            .expect("binding");
        store
            .revoke_file_grant(
                binding.direct_grant_id.as_deref().expect("grant"),
                play.scope.user_id,
                unix_seconds(),
            )
            .await
            .expect("release after failed final");
        assert!(
            !store
                .jellyfin_progress_is_current(&progress_write(&play, 0, 2000, false))
                .await
                .expect("released admission"),
            "{backend}"
        );
        store
            .set_watched_tree_with_origin(
                play.scope.user_id,
                play.item_id,
                false,
                Some(&play.scope),
            )
            .await
            .expect("own edit after failed stop");
        assert_eq!(
            store
                .jellyfin_play(&play.play_id, &play.scope)
                .await
                .expect("binding")
                .expect("binding")
                .manual_revision,
            0,
            "released reconciliation is not an eligible active play: {backend}"
        );
        let edited = store
            .watch_state(play.scope.user_id, play.item_id)
            .await
            .expect("edit");
        assert!(store
            .put_jellyfin_progress(progress_write(&play, 0, 9000, true), None)
            .await
            .expect("terminal retry fenced by edit")
            .is_none());
        assert_eq!(
            store
                .watch_state(play.scope.user_id, play.item_id)
                .await
                .expect("preserved"),
            edited
        );
    })
    .await;
}

#[tokio::test]
async fn jellyfin_logout_terminalizes_only_exact_login_scope_and_retains_release_references() {
    for_each_backend(|store, backend| async move {
        let initial = fixture(&store).await;
        let first = manual_active(&store, initial.clone()).await;
        let mut sibling = initial;
        sibling.play_id = uuid::Uuid::new_v4().simple().to_string();
        sibling.playback_id = uuid::Uuid::new_v4().to_string();
        let second = manual_active(&store, sibling).await;
        let mut wrong = first.scope.clone();
        wrong.device_digest = digest("wrong-device");
        assert!(store
            .end_jellyfin_login_plays(&wrong, 10_000)
            .await
            .expect(backend)
            .is_empty());
        let ended = store
            .end_jellyfin_login_plays(&first.scope, 10_000)
            .await
            .expect(backend);
        assert_eq!(ended.len(), 2, "{backend}");
        assert!(ended
            .iter()
            .all(|p| p.state == "ended" && p.direct_grant_id.is_some()));
        for play in [first, second] {
            assert_eq!(
                store
                    .jellyfin_play(&play.play_id, &play.scope)
                    .await
                    .expect(backend)
                    .expect("binding")
                    .state,
                "ended"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn jellyfin_progress_rejects_a_changed_probe_even_when_source_size_and_mtime_match() {
    for_each_backend(|store, backend| async move {
        let mut initial = fixture(&store).await;
        let snapshot = store.playback_planning_snapshot(initial.file_id, &[]).await.expect(backend).expect("source");
        initial.selection_json = serde_json::json!({"source":{"size":snapshot.file.size,"mtime":snapshot.file.mtime,"probe":snapshot.probe_json}}).to_string();
        let play = manual_active(&store, initial).await;
        let leading = progress_write(&play, 0, 1000, false);
        assert!(store.put_jellyfin_progress(leading.clone(), None).await.expect(backend).is_some());
        let source = snapshot.file;
        store.upsert_file(source.item_id, source.path.to_str().expect("path"), source.size, source.mtime,
            &plurx_core::domain::ProbeResult {
                duration_ms: source.duration_ms, container: source.container, video_codec: source.video_codec,
                video_codec_tag: source.video_codec_tag, video_profile: source.video_profile,
                width: source.width, height: source.height, bit_depth: source.bit_depth,
                bitrate: source.bitrate, audio_streams: source.audio_streams, subtitle_streams: source.subtitle_streams,
                raw_json: Some("{\"streams\":[],\"jellyfin_test_probe_revision\":2}".into()),
                ..Default::default()
            }).await.expect(backend);
        assert!(!store.jellyfin_progress_is_current(&leading).await.expect(backend));
        assert!(store.put_jellyfin_progress(progress_write(&play, 0, 9000, true), None).await.expect(backend).is_none());
        assert_eq!(store.watch_state(play.scope.user_id, play.item_id).await.expect(backend).expect("watch").position_ms, 1000);
    }).await;
}
