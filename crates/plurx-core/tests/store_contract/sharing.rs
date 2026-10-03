use super::*;
use plurx_core::{secrets::SharingSecretPurpose, sharing::*};
use uuid::Uuid;
fn hash(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}
async fn library(store: &dyn Store, name: &str) -> i64 {
    store
        .create_library(&NewLibrary {
            name: name.into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/synthetic")],
            anime: false,
        })
        .await
        .expect("synthetic sharing fixture")
        .id
}
async fn invitation(store: &dyn Store, libs: Vec<i64>, expires: i64) -> InvitationRecord {
    let i = InvitationRecord {
        id: Uuid::new_v4(),
        token_hash: hash(1),
        library_ids: libs,
        created_at_ms: 1000,
        expires_at_ms: expires,
    };
    assert_eq!(
        store
            .create_share_invitation(i.clone())
            .await
            .expect("synthetic sharing fixture"),
        MutationOutcome::Applied
    );
    i
}
fn claim(i: &InvitationRecord) -> ShareClaim {
    ShareClaim {
        invitation_id: i.id,
        invitation_hash: i.token_hash.clone(),
        claim_id: Uuid::new_v4(),
        grant_id: Uuid::new_v4(),
        recipient_server_id: Uuid::new_v4(),
        recipient_name: "Synthetic Cinema".into(),
        credential_hash: hash(2),
        now_ms: 1001,
    }
}
#[tokio::test]
async fn sharing_claim_race_replay_expiry_and_pending_authority() {
    for_each_backend(|s, b| async move {
        let l = library(s.as_ref(), "Sharing movies").await;
        let i = invitation(s.as_ref(), vec![l], 2000).await;
        let first = claim(&i);
        let mut second = first.clone();
        second.claim_id = Uuid::new_v4();
        second.grant_id = Uuid::new_v4();
        second.credential_hash = hash(3);
        let (a, c) = tokio::join!(s.claim_share(first.clone()), s.claim_share(second.clone()));
        let (winner, loser, created) = match (
            a.expect("synthetic sharing fixture"),
            c.expect("synthetic sharing fixture"),
        ) {
            (ClaimOutcome::Created(g), ClaimOutcome::Consumed) => (first, second, g),
            (ClaimOutcome::Consumed, ClaimOutcome::Created(g)) => (second, first, g),
            other => panic!("{b}: {other:?}"),
        };
        assert_eq!(
            s.authorize_share(&winner.credential_hash, None, 1002)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Pending
        );
        assert_eq!(
            s.authorize_share(&winner.credential_hash, Some(l), 1002)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Denied
        );
        let mut retry = winner.clone();
        retry.now_ms = 3000;
        assert_eq!(
            s.claim_share(retry)
                .await
                .expect("synthetic sharing fixture"),
            ClaimOutcome::Replay(created.clone()),
            "{b}"
        );
        assert_eq!(
            s.claim_share(loser)
                .await
                .expect("synthetic sharing fixture"),
            ClaimOutcome::Consumed
        );
        assert_eq!(
            s.approve_share(created.id, 1, 3000)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        assert!(matches!(
            s.authorize_share(&winner.credential_hash, Some(l), 3001)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Allowed(_)
        ));
        assert_eq!(
            s.authorize_share(&winner.credential_hash, Some(l + 999), 3001)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Denied
        );
    })
    .await;
}
#[tokio::test]
async fn sharing_rotation_loss_scope_and_library_deletion_preserve_authority() {
    for_each_backend(|s, b| async move {
        let l = library(s.as_ref(), "Sharing movies").await;
        let next = library(s.as_ref(), "Sharing more").await;
        let i = invitation(s.as_ref(), vec![l], 2000).await;
        let c = claim(&i);
        let g = match s
            .claim_share(c.clone())
            .await
            .expect("synthetic sharing fixture")
        {
            ClaimOutcome::Created(g) => g,
            _ => panic!("claim"),
        };
        assert_eq!(
            s.approve_share(g.id, 1, 1002)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        let request = Uuid::new_v4();
        let new = hash(9);
        assert_eq!(
            s.rotate_share(g.id, request, &c.credential_hash, &new, 1003)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.rotate_share(g.id, request, &c.credential_hash, &new, 1004)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied,
            "{b}: exact response-loss replay"
        );
        assert!(s
            .share_rotation_status(g.id, request, &c.credential_hash, 1004)
            .await
            .expect("synthetic sharing fixture"));
        assert_eq!(
            s.authorize_share(&c.credential_hash, Some(l), 1004)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Denied
        );
        assert_eq!(
            s.rotate_share(g.id, Uuid::new_v4(), &new, &hash(10), 1004)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.share_scope(g.id, 3, vec![l, next], 1005)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        let expanded = match s
            .authorize_share(&new, Some(l), 1006)
            .await
            .expect("synthetic sharing fixture")
        {
            Authorization::Allowed(g) => g,
            _ => panic!("expanded"),
        };
        assert_eq!(
            expanded.scope_generation, 1,
            "{b}: rotation/expansion must not retire playback"
        );
        assert_eq!(
            s.share_scope(g.id, 3, vec![next], 1006)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.share_scope(g.id, 4, vec![next], 1007)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.authorize_share(&new, Some(l), 1008)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Denied
        );
        let mut replay = c.clone();
        replay.now_ms = 1008;
        assert!(matches!(
            s.claim_share(replay)
                .await
                .expect("post-approval invitation replay"),
            ClaimOutcome::Replay(_)
        ));
        assert_eq!(
            s.authorize_share(&new, Some(l), 1008)
                .await
                .expect("narrowed scope remains denied"),
            Authorization::Denied,
            "{b}: claim replay must not reintroduce invitation libraries after approval"
        );
        assert_eq!(
            s.authorize_share(&new, Some(-1), 1008)
                .await
                .expect("negative library rejected"),
            Authorization::Denied
        );
        assert!(
            s.delete_library(next)
                .await
                .expect("synthetic sharing fixture"),
            "{b}: authorized deletion must advance authority before removing links"
        );
        assert_eq!(
            s.authorize_share(&new, Some(next), 1009)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Denied
        );
        let current = match s
            .authorize_share(&new, None, 1009)
            .await
            .expect("synthetic sharing fixture")
        {
            Authorization::Allowed(g) => g,
            _ => panic!("state"),
        };
        assert_eq!(current.scope_generation, 3);
        assert_eq!(current.mutation_generation, 6);
        assert_eq!(
            s.share_scope(g.id, 6, vec![], 1010)
                .await
                .expect("unchanged empty scope"),
            MutationOutcome::Applied
        );
        let unchanged = match s
            .authorize_share(&new, None, 1010)
            .await
            .expect("status after unchanged scope")
        {
            Authorization::Allowed(g) => g,
            _ => panic!("unchanged active grant"),
        };
        assert_eq!(unchanged.scope_generation, current.scope_generation);
        assert_eq!(
            unchanged.catalogue_generation, current.catalogue_generation,
            "{b}: unchanged scope must not invalidate catalogue cursors"
        );
        assert_eq!(unchanged.mutation_generation, 7);
        s.revoke_share(g.id, 1010)
            .await
            .expect("synthetic sharing fixture");
        s.revoke_share(g.id, 1011)
            .await
            .expect("synthetic sharing fixture");
        assert_eq!(
            s.authorize_share(&new, None, 1012)
                .await
                .expect("synthetic sharing fixture"),
            Authorization::Denied
        );
        assert!(!s
            .share_rotation_status(g.id, request, &new, 1012)
            .await
            .expect("synthetic sharing fixture"));
    })
    .await;
}
#[tokio::test]
async fn sharing_import_assignment_cas_and_user_lifetime() {
    for_each_backend(|s, b| async move {
        let identity = s
            .sharing_identity(1000)
            .await
            .expect("synthetic sharing fixture");
        assert_eq!(
            s.sharing_identity(2000)
                .await
                .expect("synthetic sharing fixture"),
            identity
        );
        let dir = tempfile::tempdir().expect("synthetic sharing fixture");
        let key =
            plurx_core::secrets::open_credential_key(&dir.path().join("key"), &Default::default())
                .expect("synthetic sharing fixture");
        let import = Uuid::new_v4();
        let source = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let i = NewImport {
            id: import,
            source,
            source_name: "Synthetic source".into(),
            claim_id: Uuid::new_v4(),
            credential: key
                .seal_sharing(
                    SharingSecretPurpose::Credential,
                    identity.server_id,
                    import,
                    "synthetic-credential",
                )
                .expect("synthetic sharing fixture"),
            claim_secret: key
                .seal_sharing(
                    SharingSecretPurpose::Claim,
                    identity.server_id,
                    import,
                    "synthetic-invitation",
                )
                .expect("synthetic sharing fixture"),
            endpoints: vec![Endpoint {
                ipv4: "100.101.102.103"
                    .parse()
                    .expect("synthetic sharing fixture"),
                ipv6: None,
                ts_fqdn: "source.example.ts.net".into(),
                port: 32443,
                spki_sha256: hash(7),
            }],
            now_ms: 1000,
        };
        assert_eq!(
            s.create_share_import(i.clone())
                .await
                .expect("synthetic sharing fixture"),
            ImportOutcome::Created
        );
        let mut duplicate = i;
        duplicate.id = Uuid::new_v4();
        duplicate.claim_id = Uuid::new_v4();
        assert_eq!(
            s.create_share_import(duplicate)
                .await
                .expect("synthetic sharing fixture"),
            ImportOutcome::AlreadyImported(import)
        );
        assert_eq!(
            s.sharing_sealed_census()
                .await
                .expect("synthetic sharing fixture")
                .sealed_rows(),
            1,
            "{b}"
        );
        let user = s
            .create_user("shared-viewer", "synthetic-password-hash", false)
            .await
            .expect("synthetic sharing fixture");
        let library = SourceId::parse("9007199254740993").expect("synthetic sharing fixture");
        assert!(s
            .authorize_share_viewer(import, library.clone(), user.id)
            .await
            .expect("synthetic sharing fixture")
            .is_none());
        s.settle_share_claim(import, Uuid::new_v4(), true, 1002)
            .await
            .expect("synthetic sharing fixture");
        let rotation = ImportRotation {
            import_id: import,
            request_id: Uuid::new_v4(),
            credential: key
                .seal_sharing(
                    SharingSecretPurpose::Rotation,
                    identity.server_id,
                    import,
                    "synthetic-rotated",
                )
                .expect("seal rotation"),
            lifecycle_generation: 1,
            now_ms: 1002,
        };
        assert_eq!(
            s.begin_share_import_rotation(rotation.clone())
                .await
                .expect("begin"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.begin_share_import_rotation(rotation.clone())
                .await
                .expect("retry"),
            MutationOutcome::Applied
        );
        let mut competing = rotation.clone();
        competing.request_id = Uuid::new_v4();
        assert_eq!(
            s.begin_share_import_rotation(competing)
                .await
                .expect("competing rotation"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.sharing_sealed_census()
                .await
                .expect("pending census")
                .sealed_rows(),
            2
        );
        assert_eq!(
            s.commit_share_import_rotation(
                import,
                rotation.request_id,
                1,
                2,
                key.seal_sharing(
                    SharingSecretPurpose::Credential,
                    identity.server_id,
                    import,
                    "synthetic-rotated"
                )
                .expect("seal confirmed credential"),
                1003
            )
            .await
            .expect("confirm"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.commit_share_import_rotation(
                import,
                rotation.request_id,
                1,
                2,
                key.seal_sharing(
                    SharingSecretPurpose::Credential,
                    identity.server_id,
                    import,
                    "synthetic-rotated"
                )
                .expect("seal confirmed credential"),
                1004
            )
            .await
            .expect("confirm retry"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.sharing_sealed_census()
                .await
                .expect("committed census")
                .sealed_rows(),
            1
        );
        let assignments = vec![Assignment {
            library_id: library.clone(),
            user_id: user.id,
        }];
        assert_eq!(
            s.assign_share_viewers(import, 1, assignments.clone(), 1003)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        let original = s
            .authorize_share_viewer(import, library.clone(), user.id)
            .await
            .expect("synthetic sharing fixture")
            .expect("synthetic sharing fixture");
        assert_eq!(
            s.assign_share_viewers(import, 1, vec![], 1004)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.assign_share_viewers(import, 2, vec![], 1004)
                .await
                .expect("synthetic sharing fixture"),
            MutationOutcome::Applied
        );
        assert!(s
            .authorize_share_viewer(import, library.clone(), user.id)
            .await
            .expect("synthetic sharing fixture")
            .is_none());
        s.assign_share_viewers(import, 3, assignments, 1005)
            .await
            .expect("synthetic sharing fixture");
        assert_eq!(
            s.authorize_share_viewer(import, library.clone(), user.id)
                .await
                .expect("synthetic sharing fixture"),
            Some(original)
        );
        s.delete_user(user.id)
            .await
            .expect("synthetic sharing fixture");
        assert!(s
            .authorize_share_viewer(import, library.clone(), user.id)
            .await
            .expect("synthetic sharing fixture")
            .is_none());
        let recreated = s
            .create_user("shared-viewer", "synthetic-password-hash", false)
            .await
            .expect("synthetic sharing fixture");
        assert!(s
            .authorize_share_viewer(import, library.clone(), recreated.id)
            .await
            .expect("synthetic sharing fixture")
            .is_none());
        s.disable_share_import(import, 1007)
            .await
            .expect("synthetic sharing fixture");
        assert_eq!(
            s.assign_share_viewers(
                import,
                4,
                vec![Assignment {
                    library_id: library,
                    user_id: recreated.id
                }],
                1008
            )
            .await
            .expect("synthetic sharing fixture"),
            MutationOutcome::Conflict
        );
    })
    .await;
}

#[tokio::test]
async fn sharing_expired_cancelled_and_wrong_invites_cannot_claim() {
    for_each_backend(|s, b| async move {
        let l = library(s.as_ref(), "Sharing movies").await;
        let i = invitation(s.as_ref(), vec![l], 2000).await;
        let mut c = claim(&i);
        c.now_ms = 2000;
        assert_eq!(
            s.claim_share(c.clone())
                .await
                .expect("synthetic sharing fixture"),
            ClaimOutcome::Expired,
            "{b}"
        );
        c.invitation_hash = hash(5);
        assert_eq!(
            s.claim_share(c.clone())
                .await
                .expect("synthetic sharing fixture"),
            ClaimOutcome::NotFound
        );
        s.cancel_share_invitation(i.id)
            .await
            .expect("synthetic sharing fixture");
        s.cancel_share_invitation(i.id)
            .await
            .expect("synthetic sharing fixture");
        c.invitation_hash = i.token_hash;
        c.now_ms = 1002;
        assert_eq!(
            s.claim_share(c).await.expect("synthetic sharing fixture"),
            ClaimOutcome::Cancelled
        );
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test]
async fn sharing_populated_sqlite_import_preserves_sealed_credentials_and_viewer_authority() {
    use plurx_core::store::SharingStore;
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let target = open_contract_hiqlite_store(&cluster).await;
    target
        .validation_reset_contract_state()
        .await
        .expect("clean import target");
    let dir = tempfile::tempdir().expect("synthetic import source");
    let source = SqliteStore::open(&dir.path().join("plurx.db")).expect("synthetic source store");
    rusqlite::Connection::open(dir.path().join("plurx.db"))
        .expect("synthetic instance seed")
        .execute(
            "UPDATE settings SET value=?1 WHERE key='instance.id'",
            [CONTRACT_INSTANCE_ID],
        )
        .expect("same logical instance for backend migration");
    let identity = source
        .sharing_identity(1000)
        .await
        .expect("source sharing identity");
    let key =
        plurx_core::secrets::open_credential_key(&dir.path().join("key"), &Default::default())
            .expect("synthetic wrapping key");
    let import = Uuid::new_v4();
    source
        .create_share_import(NewImport {
            id: import,
            source: SharingIdentity {
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                created_at_ms: 1000,
            },
            source_name: "Synthetic upstream".into(),
            claim_id: Uuid::new_v4(),
            credential: key
                .seal_sharing(
                    SharingSecretPurpose::Credential,
                    identity.server_id,
                    import,
                    "synthetic-bearer",
                )
                .expect("seal credential"),
            claim_secret: key
                .seal_sharing(
                    SharingSecretPurpose::Claim,
                    identity.server_id,
                    import,
                    "synthetic-bootstrap",
                )
                .expect("seal bootstrap"),
            endpoints: vec![Endpoint {
                ipv4: "100.101.102.103"
                    .parse()
                    .expect("synthetic tailnet address"),
                ipv6: None,
                ts_fqdn: "source.example.ts.net".into(),
                port: 32443,
                spki_sha256: hash(7),
            }],
            now_ms: 1000,
        })
        .await
        .expect("persist import");
    source
        .settle_share_claim(import, Uuid::new_v4(), true, 1001)
        .await
        .expect("consume nullable bootstrap");
    let user = source
        .create_user("synthetic-import-viewer", "synthetic-password-hash", false)
        .await
        .expect("local viewer");
    let remote = SourceId::parse("9007199254740993").expect("large wire identity");
    source
        .assign_share_viewers(
            import,
            1,
            vec![Assignment {
                library_id: remote.clone(),
                user_id: user.id,
            }],
            1002,
        )
        .await
        .expect("assign viewer");
    let before = source
        .authorize_share_viewer(import, remote.clone(), user.id)
        .await
        .expect("source authority");
    let prepared = prepare_sqlite_import(dir.path()).expect("prepare sharing import");
    let report = target
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import populated sharing rows");
    for name in [
        "sharing_identity",
        "sharing_imports",
        "sharing_viewers",
        "sharing_assignments",
    ] {
        assert_eq!(
            report
                .tables
                .iter()
                .find(|t| t.table == name)
                .expect("sharing parity digest")
                .row_count,
            1,
            "{name}"
        );
    }
    assert_eq!(
        target
            .sharing_identity(1003)
            .await
            .expect("imported identity"),
        identity
    );
    assert_eq!(
        target
            .authorize_share_viewer(import, remote, user.id)
            .await
            .expect("imported viewer authority"),
        before
    );
    let census = target
        .sharing_sealed_census()
        .await
        .expect("imported sealed credential census");
    assert_eq!(census.sealed_rows(), 1);
    // Key IDs and exact ciphertext survive parity, without rewriting credentials.
    assert_eq!(
        census,
        source.sharing_sealed_census().await.expect("source census")
    );
}
