use super::*;
use plurx_core::{
    secrets::{CredentialKey, SharingSecretPurpose},
    sharing::*,
    store::sharing_catalogue::*,
};
use uuid::Uuid;
fn source_id(id: &str) -> SourceId {
    SourceId::parse(id).expect("canonical fixture ID")
}
async fn assigned_import(s: &dyn Store, user: i64) -> Uuid {
    let import = Uuid::new_v4();
    let local = s.sharing_identity(1000).await.expect("local identity");
    let key = CredentialKey::from_bytes([7; 32]);
    assert_eq!(
        s.create_share_import(NewImport {
            id: import,
            source: SharingIdentity {
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                created_at_ms: 1000
            },
            source_name: "Synthetic source".into(),
            claim_id: Uuid::new_v4(),
            credential: key
                .seal_sharing(
                    SharingSecretPurpose::Credential,
                    local.server_id,
                    import,
                    "synthetic-credential"
                )
                .expect("seal"),
            claim_secret: key
                .seal_sharing(
                    SharingSecretPurpose::Claim,
                    local.server_id,
                    import,
                    "synthetic-invitation"
                )
                .expect("seal"),
            endpoints: vec![Endpoint {
                ipv4: "100.101.102.103".parse().expect("address"),
                ipv6: None,
                ts_fqdn: "source.example.ts.net".into(),
                port: 32443,
                spki_sha256: "a".repeat(64)
            }],
            now_ms: 1000,
        })
        .await
        .expect("import"),
        ImportOutcome::Created
    );
    s.settle_share_claim(import, Uuid::new_v4(), true, 1001)
        .await
        .expect("activate");
    s.assign_share_viewers(
        import,
        1,
        vec![Assignment {
            library_id: source_id("12"),
            user_id: user,
        }],
        1002,
    )
    .await
    .expect("assign");
    import
}
fn update(import: Uuid, user: i64, sequence: i64, position: i64) -> RemoteWatchUpdate {
    RemoteWatchUpdate {
        import_id: import,
        library_id: source_id("12"),
        item_id: source_id("9007199254740993"),
        user_id: user,
        lifecycle_generation: 1,
        assignment_generation: 2,
        progress: RemoteWatch {
            position_ms: position,
            duration_ms: Some(60000),
            watched: false,
            sequence,
            updated_at_ms: 1000 + sequence,
        },
    }
}
#[tokio::test]
async fn sharing_private_watch_orders_updates_and_isolates_sources_and_assignments() {
    for_each_backend(|s, b| async move {
        let user = s
            .create_user("sharing-watch-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let outsider = s
            .create_user("sharing-watch-outsider", "synthetic-hash", false)
            .await
            .expect("outsider");
        let a = assigned_import(s.as_ref(), user.id).await;
        let c = assigned_import(s.as_ref(), user.id).await;
        assert_eq!(
            s.assigned_catalogue_libraries(a, user.id, 1, 2)
                .await
                .expect("synthetic catalogue fixture"),
            vec![source_id("12")]
        );
        assert!(s
            .assigned_catalogue_libraries(a, outsider.id, 1, 2)
            .await
            .expect("synthetic catalogue fixture")
            .is_empty());
        assert!(s
            .assigned_catalogue_libraries(a, user.id, 1, 1)
            .await
            .expect("synthetic catalogue fixture")
            .is_empty());
        assert!(s.assigned_catalogue_libraries(a, 0, 1, 2).await.is_err());

        assert_eq!(
            s.save_remote_watch(update(a, outsider.id, 1, 1000))
                .await
                .expect("denied"),
            RemoteProgressOutcome::Unauthorized,
            "{b}"
        );
        let newest = update(a, user.id, 2, 2200);
        assert_eq!(
            s.save_remote_watch(newest.clone()).await.expect("write"),
            RemoteProgressOutcome::Applied,
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(newest.clone()).await.expect("replay"),
            RemoteProgressOutcome::Applied,
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 1, 1000))
                .await
                .expect("old"),
            RemoteProgressOutcome::Stale,
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 2, 9999))
                .await
                .expect("conflict"),
            RemoteProgressOutcome::Conflict,
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(c, user.id, 1, 7000))
                .await
                .expect("other source"),
            RemoteProgressOutcome::Applied,
            "{b}"
        );
        assert_eq!(
            s.remote_watch(a, source_id("12"), source_id("9007199254740993"), user.id)
                .await
                .expect("read"),
            Some(newest.progress.clone()),
            "{b}"
        );
        assert_eq!(
            s.remote_watch(c, source_id("12"), source_id("9007199254740993"), user.id)
                .await
                .expect("read")
                .expect("other source history")
                .position_ms,
            7000,
            "{b}"
        );
        assert!(
            s.remote_watch(
                a,
                source_id("12"),
                source_id("9007199254740993"),
                outsider.id
            )
            .await
            .expect("private")
            .is_none(),
            "{b}"
        );
        assert!(
            s.watch_state(user.id, 9007199254740993)
                .await
                .expect("local household history")
                .is_none(),
            "{b}: remote progress cannot write local watch state"
        );
        s.assign_share_viewers(a, 2, vec![], 1010)
            .await
            .expect("unassign");
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 3, 3300))
                .await
                .expect("late"),
            RemoteProgressOutcome::Unauthorized,
            "{b}"
        );
        assert!(
            s.remote_watch(a, source_id("12"), source_id("9007199254740993"), user.id)
                .await
                .expect("revoked read")
                .is_none(),
            "{b}"
        );
        s.assign_share_viewers(
            a,
            3,
            vec![Assignment {
                library_id: source_id("12"),
                user_id: user.id,
            }],
            1011,
        )
        .await
        .expect("reassign");
        assert_eq!(
            s.remote_watch(a, source_id("12"), source_id("9007199254740993"), user.id)
                .await
                .expect("preserved"),
            Some(newest.progress),
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 4, 4400))
                .await
                .expect("old assignment generation"),
            RemoteProgressOutcome::Unauthorized,
            "{b}"
        );
        s.disable_share_import(a, 1012).await.expect("disable");
        assert!(
            s.remote_watch(a, source_id("12"), source_id("9007199254740993"), user.id)
                .await
                .expect("disabled read")
                .is_none(),
            "{b}"
        );
    })
    .await;
}

#[tokio::test]
async fn sharing_manual_watch_override_takes_next_global_sequence_and_refuses_late_beats() {
    for_each_backend(|s, b| async move {
        let user = s
            .create_user("sharing-manual-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let outsider = s
            .create_user("sharing-manual-outsider", "synthetic-hash", false)
            .await
            .expect("outsider");
        let a = assigned_import(s.as_ref(), user.id).await;
        let c = assigned_import(s.as_ref(), user.id).await;
        let item = || source_id("9007199254740993");
        let manual = |import: Uuid, library: &str, viewer: i64, assignment: i64, watched: bool| {
            RemoteWatchedOverride {
                import_id: import,
                library_id: source_id(library),
                item_id: item(),
                user_id: viewer,
                lifecycle_generation: 1,
                assignment_generation: assignment,
                watched,
                updated_at_ms: 5000,
            }
        };
        let current = || s.remote_watch(a, source_id("12"), item(), user.id);

        // No history yet: the override takes sequence 1, so a first beat that
        // was already in flight (also sequence 1) cannot replace it.
        let (outcome, watch) = s
            .set_remote_watched(manual(a, "12", user.id, 2, true))
            .await
            .expect("first override");
        assert_eq!(outcome, RemoteProgressOutcome::Applied, "{b}");
        let watch = watch.expect("override row");
        assert_eq!(
            (watch.sequence, watch.position_ms, watch.watched),
            (1, 0, true),
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 1, 1500))
                .await
                .expect("late first beat"),
            RemoteProgressOutcome::Conflict,
            "{b}"
        );

        // Playback resumes after a resync and is listed for Continue Watching.
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 2, 2200))
                .await
                .expect("resumed beat"),
            RemoteProgressOutcome::Applied,
            "{b}"
        );
        assert_eq!(
            s.remote_continue_watch_groups(user.id, 200)
                .await
                .expect("resumable")
                .len(),
            1,
            "{b}"
        );

        // Manual unwatched while a beat issued before it (sequence 3) is in
        // flight: the override takes 3, the late beat conflicts and an older
        // one is stale. Position is cleared and the item leaves the list.
        let (outcome, watch) = s
            .set_remote_watched(manual(a, "12", user.id, 2, false))
            .await
            .expect("manual unwatched");
        assert_eq!(outcome, RemoteProgressOutcome::Applied, "{b}");
        let watch = watch.expect("unwatched row");
        assert_eq!(
            (watch.sequence, watch.position_ms, watch.watched),
            (3, 0, false),
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 3, 2800))
                .await
                .expect("late beat"),
            RemoteProgressOutcome::Conflict,
            "{b}"
        );
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 2, 2200))
                .await
                .expect("old beat"),
            RemoteProgressOutcome::Stale,
            "{b}"
        );
        let after = current().await.expect("read").expect("override persists");
        assert_eq!(
            (after.sequence, after.position_ms, after.watched),
            (3, 0, false),
            "{b}"
        );
        assert!(
            s.remote_continue_watch_groups(user.id, 200)
                .await
                .expect("cleared")
                .is_empty(),
            "{b}: manual unwatched must not leave a resumable row"
        );

        // Marking watched keeps the stored position, as Local history does.
        assert_eq!(
            s.save_remote_watch(update(a, user.id, 4, 4100))
                .await
                .expect("watching"),
            RemoteProgressOutcome::Applied,
            "{b}"
        );
        let (outcome, watch) = s
            .set_remote_watched(manual(a, "12", user.id, 2, true))
            .await
            .expect("manual watched");
        assert_eq!(outcome, RemoteProgressOutcome::Applied, "{b}");
        let watch = watch.expect("watched row");
        assert_eq!(
            (watch.sequence, watch.position_ms, watch.watched),
            (5, 4100, true),
            "{b}"
        );

        // Same numeric item on another Source, other users and Local history
        // are untouched; refused overrides write nothing.
        assert!(
            s.remote_watch(c, source_id("12"), item(), user.id)
                .await
                .expect("other")
                .is_none(),
            "{b}"
        );
        assert_eq!(
            s.set_remote_watched(manual(a, "12", outsider.id, 2, false))
                .await
                .expect("outsider")
                .0,
            RemoteProgressOutcome::Unauthorized,
            "{b}"
        );
        assert_eq!(
            s.set_remote_watched(manual(a, "12", user.id, 1, false))
                .await
                .expect("old assignment generation")
                .0,
            RemoteProgressOutcome::Unauthorized,
            "{b}"
        );
        assert!(
            s.watch_state(user.id, 9007199254740993)
                .await
                .expect("local")
                .is_none(),
            "{b}: remote override cannot write local watch state"
        );

        // Retained history for this durable item is bound to library 12; an
        // override naming another assigned library is not an implicit move.
        s.assign_share_viewers(
            a,
            2,
            vec![
                Assignment {
                    library_id: source_id("12"),
                    user_id: user.id,
                },
                Assignment {
                    library_id: source_id("13"),
                    user_id: user.id,
                },
            ],
            1010,
        )
        .await
        .expect("second assigned library");
        assert_eq!(
            s.set_remote_watched(manual(a, "13", user.id, 3, false))
                .await
                .expect("other library")
                .0,
            RemoteProgressOutcome::Conflict,
            "{b}"
        );
        s.assign_share_viewers(a, 3, vec![], 1011)
            .await
            .expect("unassign");
        assert_eq!(
            s.set_remote_watched(manual(a, "12", user.id, 4, false))
                .await
                .expect("unassigned")
                .0,
            RemoteProgressOutcome::Unauthorized,
            "{b}"
        );
        s.assign_share_viewers(
            a,
            4,
            vec![Assignment {
                library_id: source_id("12"),
                user_id: user.id,
            }],
            1012,
        )
        .await
        .expect("reassign");
        let retained = current().await.expect("read").expect("retained history");
        assert_eq!(
            (retained.sequence, retained.position_ms, retained.watched),
            (5, 4100, true),
            "{b}: refused overrides wrote nothing"
        );
    })
    .await;
}

#[tokio::test]
async fn sharing_receiver_content_authority_is_read_only_and_fences_current_login_and_import() {
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    for_each_backend(move |s, b| async move {
        let user = s
            .create_user("sharing-current-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let other = s
            .create_user("sharing-other-viewer", "synthetic-hash", false)
            .await
            .expect("other");
        let import = assigned_import(s.as_ref(), user.id).await;
        let summary = s
            .sharing_import(import)
            .await
            .expect("import")
            .expect("active")
            .summary;
        let scope = ReceiverCatalogueScope {
            import_id: import,
            source_server_id: summary.source_server_id,
            catalogue_epoch: summary.catalogue_epoch,
            lifecycle_generation: summary.lifecycle_generation,
            assignment_generation: summary.assignment_generation,
            endpoint_generation: summary.endpoint_generation,
            claim_id: summary.claim_id,
            remote_grant_id: summary.remote_grant_id.expect("settled grant"),
            libraries: vec![source_id("12")],
        };
        let hash = "c".repeat(64);
        s.create_token(&hash, user.id, None)
            .await
            .expect("local login");
        assert!(
            s.receiver_catalogue_authorized(&hash, user.id, std::slice::from_ref(&scope), now_s)
                .await
                .expect("current proof"),
            "{b}"
        );
        assert!(!s
            .receiver_catalogue_authorized(&hash, other.id, std::slice::from_ref(&scope), now_s)
            .await
            .expect("wrong user"));
        let before = s.list_tokens_for_user(user.id).await.expect("before");
        for _ in 0..3 {
            assert!(s
                .receiver_catalogue_authorized(&hash, user.id, std::slice::from_ref(&scope), now_s)
                .await
                .expect("read-only proof"));
        }
        let after = s.list_tokens_for_user(user.id).await.expect("after");
        assert_eq!(
            before[0].last_seen_at, after[0].last_seen_at,
            "monitor reads never renew idle expiry"
        );
        for changed in 0..6 {
            let mut stale = scope.clone();
            match changed {
                0 => stale.lifecycle_generation += 1,
                1 => stale.assignment_generation += 1,
                2 => stale.endpoint_generation += 1,
                3 => stale.source_server_id = Uuid::new_v4(),
                4 => stale.remote_grant_id = Uuid::new_v4(),
                _ => stale.libraries = vec![source_id("13")],
            };
            assert!(
                !s.receiver_catalogue_authorized(&hash, user.id, &[stale], now_s)
                    .await
                    .expect("captured fence"),
                "{b}/{changed}"
            );
        }
        assert!(s
            .receiver_catalogue_authorized(&hash, user.id, &vec![scope.clone(); 65], now_s)
            .await
            .is_err());
        s.assign_share_viewers(
            import,
            summary.assignment_generation,
            vec![
                Assignment {
                    library_id: source_id("12"),
                    user_id: user.id,
                },
                Assignment {
                    library_id: source_id("13"),
                    user_id: user.id,
                },
            ],
            1009,
        )
        .await
        .expect("benign scope expansion");
        assert!(s
            .receiver_catalogue_authorized(&hash, user.id, std::slice::from_ref(&scope), now_s)
            .await
            .expect("captured effective scope survives expansion"));
        s.assign_share_viewers(import, summary.assignment_generation + 1, vec![], 1010)
            .await
            .expect("revoke assignment");
        assert!(!s
            .receiver_catalogue_authorized(&hash, user.id, &[scope], now_s)
            .await
            .expect("current assignment revoked"));
        assert!(s
            .receiver_catalogue_authorized(&hash, user.id, &[], now_s)
            .await
            .expect("login only"));
        s.delete_token(&hash).await.expect("revoke login");
        assert!(!s
            .receiver_catalogue_authorized(&hash, user.id, &[], now_s)
            .await
            .expect("current token revoked"));
    })
    .await;
}

#[tokio::test]
async fn sharing_continue_groups_isolate_sources_filter_assignments_and_refuse_hidden_overflow() {
    for_each_backend(|s, b| async move {
        let user = s
            .create_user("sharing-continue-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let other = s
            .create_user("sharing-continue-other", "synthetic-hash", false)
            .await
            .expect("other viewer");
        let a = assigned_import(s.as_ref(), user.id).await;
        let c = assigned_import(s.as_ref(), user.id).await;
        s.save_remote_watch(update(a, user.id, 2, 2200))
            .await
            .expect("A history");
        s.save_remote_watch(update(c, user.id, 3, 3300))
            .await
            .expect("C same-ID history");
        let groups = s
            .remote_continue_watch_groups(user.id, 200)
            .await
            .expect("bounded current groups");
        assert_eq!(groups.len(), 2, "{b}");
        assert_eq!(groups[0].scope.import_id, c, "newest import first");
        assert_eq!(groups[0].items[0].item_id, groups[1].items[0].item_id);
        assert_ne!(
            groups[0].scope.source_server_id,
            groups[1].scope.source_server_id
        );
        assert_eq!(groups[0].items[0].watch.position_ms, 3300);
        assert_eq!(groups[1].items[0].watch.position_ms, 2200);
        let single = s
            .remote_continue_watch_groups(user.id, 1)
            .await
            .expect("requested newest row");
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].scope.import_id, c);
        assert!(s
            .remote_continue_watch_groups(other.id, 200)
            .await
            .expect("other user")
            .is_empty());
        assert!(s.remote_continue_watch_groups(0, 200).await.is_err());
        assert!(s.remote_continue_watch_groups(user.id, 201).await.is_err());
        s.assign_share_viewers(c, 2, Vec::new(), 1010)
            .await
            .expect("remove current assignment");
        let current = s
            .remote_continue_watch_groups(user.id, 200)
            .await
            .expect("current assignments");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].scope.import_id, a);
        let mut finished = update(a, user.id, 3, 60000);
        finished.progress.watched = true;
        s.save_remote_watch(finished).await.expect("finished item");
        assert!(s
            .remote_continue_watch_groups(user.id, 200)
            .await
            .expect("watched is not resumable")
            .is_empty());
        for n in 1..=200 {
            let mut row = update(a, user.id, 1, 1000);
            row.item_id = source_id(&(9007199254740993_i64 + n).to_string());
            assert_eq!(
                s.save_remote_watch(row)
                    .await
                    .expect("bounded progress fixture"),
                RemoteProgressOutcome::Applied
            );
        }
        let full = s
            .remote_continue_watch_groups(user.id, 200)
            .await
            .expect("exact bound");
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].items.len(), 200);
        let repeat = s
            .remote_continue_watch_groups(user.id, 200)
            .await
            .expect("deterministic recent order");
        assert_eq!(
            full[0]
                .items
                .iter()
                .map(|i| i.item_id.clone())
                .collect::<Vec<_>>(),
            repeat[0]
                .items
                .iter()
                .map(|i| i.item_id.clone())
                .collect::<Vec<_>>()
        );
        let mut excess = update(a, user.id, 1, 1000);
        excess.item_id = source_id("9223372036854775807");
        s.save_remote_watch(excess)
            .await
            .expect("overflow sentinel fixture");
        assert!(
            s.remote_continue_watch_groups(user.id, 200).await.is_err(),
            "{b}/201st row refuses whole result"
        );
        assert!(
            s.remote_continue_watch_groups(user.id, 1).await.is_err(),
            "small client limit cannot hide physical overflow"
        );
        s.disable_share_import(a, 2000)
            .await
            .expect("disable current import");
        assert!(s
            .remote_continue_watch_groups(user.id, 200)
            .await
            .expect("disabled import hidden")
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn sharing_admin_library_authority_bootstraps_without_viewer_assignment_and_fences_role() {
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    for_each_backend(move |s, b| async move {
        let admin = s
            .create_user("sharing-library-admin", "synthetic-hash", true)
            .await
            .expect("admin");
        let viewer = s
            .create_user("sharing-library-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let import = assigned_import(s.as_ref(), viewer.id).await;
        s.assign_share_viewers(import, 2, vec![], 1003)
            .await
            .expect("empty assignments");
        let summary = s
            .sharing_import(import)
            .await
            .expect("read")
            .expect("import")
            .summary;
        let scope = ReceiverCatalogueScope {
            import_id: import,
            source_server_id: summary.source_server_id,
            catalogue_epoch: summary.catalogue_epoch,
            lifecycle_generation: summary.lifecycle_generation,
            assignment_generation: summary.assignment_generation,
            endpoint_generation: summary.endpoint_generation,
            claim_id: summary.claim_id,
            remote_grant_id: summary.remote_grant_id.expect("grant"),
            libraries: vec![],
        };
        let hash = "d".repeat(64);
        let viewer_hash = "e".repeat(64);
        s.create_token(&hash, admin.id, None)
            .await
            .expect("admin login");
        s.create_token(&viewer_hash, viewer.id, None)
            .await
            .expect("viewer login");
        assert!(
            s.receiver_admin_catalogue_authorized(&hash, admin.id, &scope, now_s)
                .await
                .expect("admin bootstrap"),
            "{b}"
        );
        assert!(!s
            .receiver_admin_catalogue_authorized(&viewer_hash, viewer.id, &scope, now_s)
            .await
            .expect("viewer cannot administer"));
        assert!(!s
            .receiver_admin_catalogue_authorized(&hash, viewer.id, &scope, now_s)
            .await
            .expect("token identity"));
        let before = s.list_tokens_for_user(admin.id).await.expect("before")[0].last_seen_at;
        for _ in 0..3 {
            assert!(s
                .receiver_admin_catalogue_authorized(&hash, admin.id, &scope, now_s)
                .await
                .expect("read only"));
        }
        assert_eq!(
            s.list_tokens_for_user(admin.id).await.expect("after")[0].last_seen_at,
            before
        );
        let mut with_libraries = scope.clone();
        with_libraries.libraries = vec![source_id("9007199254740993")];
        assert!(s
            .receiver_admin_catalogue_authorized(&hash, admin.id, &with_libraries, now_s)
            .await
            .expect("Source scope is separately checked"));
        assert!(!s
            .receiver_catalogue_authorized(&hash, admin.id, &[with_libraries], now_s)
            .await
            .expect("admin gets no viewer bypass"));
        for changed in 0..7 {
            let mut stale = scope.clone();
            match changed {
                0 => stale.lifecycle_generation += 1,
                1 => stale.assignment_generation += 1,
                2 => stale.endpoint_generation += 1,
                3 => stale.source_server_id = Uuid::new_v4(),
                4 => stale.catalogue_epoch = Uuid::new_v4(),
                5 => stale.remote_grant_id = Uuid::new_v4(),
                _ => stale.claim_id = Uuid::new_v4(),
            }
            assert!(
                !s.receiver_admin_catalogue_authorized(&hash, admin.id, &stale, now_s)
                    .await
                    .expect("scope fence"),
                "{b}/{changed}"
            );
        }
        s.set_admin(admin.id, false).await.expect("demote");
        assert!(!s
            .receiver_admin_catalogue_authorized(&hash, admin.id, &scope, now_s)
            .await
            .expect("current role"));
        s.set_admin(admin.id, true).await.expect("restore role");
        s.disable_share_import(import, 1004)
            .await
            .expect("disable import");
        assert!(!s
            .receiver_admin_catalogue_authorized(&hash, admin.id, &scope, now_s)
            .await
            .expect("current import"));
        s.delete_token(&hash).await.expect("logout");
        assert!(!s
            .receiver_admin_catalogue_authorized(&hash, admin.id, &scope, now_s)
            .await
            .expect("current login"));
    })
    .await;
}
