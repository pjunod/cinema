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
