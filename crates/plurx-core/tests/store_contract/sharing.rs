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
            source: source.clone(),
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
        let snapshot = s
            .sharing_import_assignments(import)
            .await
            .expect("complete assignments")
            .expect("import");
        assert_eq!(snapshot.import_id, import);
        assert_eq!(snapshot.server_id, source.server_id);
        assert_eq!(snapshot.catalogue_epoch, source.catalogue_epoch);
        assert_eq!(snapshot.lifecycle_generation, 1);
        assert_eq!(snapshot.expected_assignment_generation, 2);
        assert_eq!(snapshot.state, "active");
        assert_eq!(
            snapshot.assignments,
            vec![ImportAssignmentGroup {
                library_id: library.clone(),
                user_ids: vec![user.id]
            }]
        );
        assert!(s
            .sharing_import_assignments(Uuid::new_v4())
            .await
            .expect("missing snapshot")
            .is_none());
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
        let empty = s
            .sharing_import_assignments(import)
            .await
            .expect("empty snapshot")
            .expect("existing import");
        assert_eq!(empty.expected_assignment_generation, 3);
        assert!(empty.assignments.is_empty());
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

#[tokio::test]
async fn sharing_endpoint_cas_and_re_pair_preserve_private_viewer_identity() {
    for_each_backend(|s, backend| async move {
        let local = s.sharing_identity(1000).await.expect("local identity");
        let lib = library(s.as_ref(), "Sharing status fixture").await;
        let invite = invitation(s.as_ref(), vec![lib], 2000).await;
        let claim = claim(&invite);
        let grant = match s.claim_share(claim.clone()).await.expect("claim") {
            ClaimOutcome::Created(grant) => grant,
            other => panic!("{backend}: {other:?}"),
        };
        let exports = s.sharing_exports(None).await.expect("bounded export page");
        assert_eq!(exports.len(), 1, "{backend}");
        assert_eq!(exports[0].grant.id, grant.id);
        assert_eq!(
            exports[0].library_ids,
            vec![SourceId::parse(&lib.to_string()).expect("library wire ID")]
        );
        assert_eq!(
            exports[0].pairing_code,
            pairing_code(
                local.server_id,
                claim.recipient_server_id,
                invite.id,
                claim.claim_id,
                &claim.credential_hash
            )
        );
        assert!(s
            .sharing_exports(Some(grant.id))
            .await
            .expect("keyset continuation")
            .is_empty());
        let status = s
            .sharing_grant_status(&claim.credential_hash)
            .await
            .expect("own grant status")
            .expect("pending status");
        assert_eq!(status.grant.id, grant.id);
        assert!(s
            .sharing_grant_status(&hash(9))
            .await
            .expect("wrong credential")
            .is_none());
        assert!(!serde_json::to_string(&status)
            .expect("public grant DTO")
            .contains(&claim.credential_hash));
        let endpoint = Endpoint {
            ipv4: "100.101.102.103".parse().expect("synthetic address"),
            ipv6: None,
            ts_fqdn: "source.example.ts.net".into(),
            port: 32443,
            spki_sha256: hash(7),
        };
        assert_eq!(
            s.set_sharing_endpoint_manifest(0, vec![endpoint.clone()])
                .await
                .expect("first approval"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.set_sharing_endpoint_manifest(0, vec![endpoint.clone()])
                .await
                .expect("stale initial approval"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.set_sharing_endpoint_manifest(1, vec![endpoint.clone()])
                .await
                .expect("next approval"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.sharing_endpoint_manifest()
                .await
                .expect("approved manifest")
                .expect("manifest")
                .revision,
            2
        );
        let mut invalid_endpoint = endpoint.clone();
        invalid_endpoint.ipv4 = "192.168.1.1".parse().expect("untrusted LAN address");
        assert!(s
            .set_sharing_endpoint_manifest(2, vec![invalid_endpoint])
            .await
            .is_err());
        let key = plurx_core::secrets::CredentialKey::generate();
        let source = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let id = Uuid::new_v4();
        let import = NewImport {
            id,
            source: source.clone(),
            source_name: "Synthetic source".into(),
            claim_id: Uuid::new_v4(),
            credential: key
                .seal_sharing(
                    SharingSecretPurpose::Credential,
                    local.server_id,
                    id,
                    "synthetic credential",
                )
                .expect("seal credential"),
            claim_secret: key
                .seal_sharing(
                    SharingSecretPurpose::Claim,
                    local.server_id,
                    id,
                    "synthetic invitation",
                )
                .expect("seal claim"),
            endpoints: vec![endpoint.clone()],
            now_ms: 1000,
        };
        assert_eq!(
            s.create_share_import(import.clone()).await.expect("import"),
            ImportOutcome::Created
        );
        let stored = s
            .sharing_import(id)
            .await
            .expect("read import")
            .expect("stored import");
        assert_eq!(
            key.open_sharing(
                SharingSecretPurpose::Credential,
                local.server_id,
                id,
                &stored.credential
            )
            .expect("open credential")
            .expose(),
            "synthetic credential"
        );
        assert!(stored.claim.is_some());
        assert_eq!(s.sharing_imports().await.expect("status list").len(), 1);
        assert_eq!(
            s.set_sharing_import_endpoints(id, 1, vec![endpoint.clone()], Some(2), 1001)
                .await
                .expect("new manifest"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.set_sharing_import_endpoints(id, 2, vec![endpoint.clone()], Some(1), 1001)
                .await
                .expect("old source revision"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.set_sharing_import_endpoints(id, 2, vec![endpoint.clone()], None, 1001)
                .await
                .expect("admin address edit"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.set_sharing_import_endpoints(id, 2, vec![endpoint.clone()], Some(3), 1001)
                .await
                .expect("stale local CAS"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.set_sharing_import_endpoints(id, 3, vec![endpoint], Some(3), 1001)
                .await
                .expect("fresh local and source generations"),
            MutationOutcome::Applied
        );
        let updated = s
            .sharing_import(id)
            .await
            .expect("updated import")
            .expect("import");
        assert_eq!(updated.summary.endpoint_generation, 4);
        assert_eq!(updated.summary.observed_endpoint_revision, Some(3));
        s.settle_share_claim(id, Uuid::new_v4(), true, 1002)
            .await
            .expect("activate");
        let user = s
            .create_user("sharing-status-viewer", "synthetic password hash", false)
            .await
            .expect("viewer");
        let remote_library = SourceId::parse("9007199254740993").expect("large source ID");
        s.assign_share_viewers(
            id,
            1,
            vec![Assignment {
                library_id: remote_library.clone(),
                user_id: user.id,
            }],
            1002,
        )
        .await
        .expect("assign");
        let original_viewer = s
            .authorize_share_viewer(id, remote_library.clone(), user.id)
            .await
            .expect("viewer authority")
            .expect("pseudonym");
        s.disable_share_import(id, 1003).await.expect("disable");
        let mut replacement = import;
        replacement.claim_id = Uuid::new_v4();
        replacement.now_ms = 1004;
        let mut other_source = replacement.clone();
        other_source.source.catalogue_epoch = Uuid::new_v4();
        assert_eq!(
            s.re_pair_share_import(other_source, 2)
                .await
                .expect("identity substitution"),
            MutationOutcome::Conflict
        );
        let (one, two) = tokio::join!(
            s.re_pair_share_import(replacement.clone(), 2),
            s.re_pair_share_import(replacement, 2)
        );
        let outcomes = [one.expect("first retry"), two.expect("second retry")];
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == MutationOutcome::Applied)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == MutationOutcome::Conflict)
                .count(),
            1
        );
        let pending = s
            .sharing_import(id)
            .await
            .expect("new lifecycle")
            .expect("retained import");
        assert_eq!(pending.summary.lifecycle_generation, 3);
        assert_eq!(pending.summary.state, "claiming");
        assert!(pending.summary.remote_grant_id.is_none());
        assert_eq!(pending.summary.assignment_generation, 3);
        assert!(s
            .authorize_share_viewer(id, remote_library.clone(), user.id)
            .await
            .expect("claiming cannot deliver")
            .is_none());
        let grant = Uuid::new_v4();
        assert_eq!(
            s.settle_current_share_claim(id, Uuid::new_v4(), 3, grant, true, 1005)
                .await
                .expect("old claim response"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.settle_current_share_claim(id, pending.summary.claim_id, 1, grant, true, 1005)
                .await
                .expect("old lifecycle response"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.fail_current_share_claim(id, pending.summary.claim_id, 1, 1005)
                .await
                .expect("old lifecycle failure"),
            MutationOutcome::Conflict
        );
        let confirmed = key
            .seal_sharing(
                SharingSecretPurpose::Credential,
                local.server_id,
                id,
                "credential with confirmed immutable pending expiry",
            )
            .expect("seal confirmed receipt");
        let receipt = ImportClaimReceipt {
            import_id: id,
            claim_id: pending.summary.claim_id,
            lifecycle_generation: 3,
            grant_id: grant,
            active: true,
            credential: confirmed.clone(),
            now_ms: 1005,
        };
        let mut stale = receipt.clone();
        stale.lifecycle_generation = 1;
        assert_eq!(
            s.settle_share_import_response(stale)
                .await
                .expect("stale receipt"),
            MutationOutcome::Conflict
        );
        let unchanged = s
            .sharing_import(id)
            .await
            .expect("unchanged import")
            .expect("import");
        assert_eq!(
            unchanged.credential.to_persist().expect("ciphertext"),
            pending
                .credential
                .to_persist()
                .expect("original ciphertext")
        );
        assert_eq!(
            s.settle_share_import_response(receipt.clone())
                .await
                .expect("activate repaired import"),
            MutationOutcome::Applied
        );
        assert_eq!(
            s.assign_share_viewers(id, 2, vec![], 1006)
                .await
                .expect("pre-repair stale matrix"),
            MutationOutcome::Conflict
        );
        let repaired = s
            .sharing_import_assignments(id)
            .await
            .expect("repaired matrix")
            .expect("import");
        assert_eq!(repaired.expected_assignment_generation, 3);
        assert_eq!(repaired.lifecycle_generation, 3);
        assert_eq!(
            repaired.assignments,
            vec![ImportAssignmentGroup {
                library_id: remote_library.clone(),
                user_ids: vec![user.id]
            }]
        );
        let settled = s
            .sharing_import(id)
            .await
            .expect("confirmed import")
            .expect("import");
        assert_eq!(
            settled.credential.to_persist().expect("ciphertext"),
            confirmed.to_persist().expect("confirmed ciphertext")
        );
        let mut delayed = receipt;
        delayed.active = false;
        assert_eq!(
            s.settle_share_import_response(delayed)
                .await
                .expect("late pending response"),
            MutationOutcome::Conflict
        );
        assert_eq!(
            s.sharing_import(id)
                .await
                .expect("active import")
                .expect("import")
                .summary
                .state,
            "active"
        );
        assert_eq!(
            s.authorize_share_viewer(id, remote_library, user.id)
                .await
                .expect("retained viewer identity")
                .as_deref(),
            Some(original_viewer.as_str())
        );
        assert!(s
            .sharing_import(id)
            .await
            .expect("settled import")
            .expect("import")
            .claim
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn sharing_capacity_refusals_preserve_existing_authority_and_reopen_expired_slots() {
    for_each_backend(|store, backend| async move {
        let lib = library(store.as_ref(), "Capacity fixture").await;
        let mut invitations = Vec::new();
        for n in 0..32 {
            let record = InvitationRecord {
                id: Uuid::new_v4(),
                token_hash: hash(n),
                library_ids: vec![lib],
                created_at_ms: 1000,
                expires_at_ms: 2000,
            };
            assert_eq!(
                store
                    .create_share_invitation(record.clone())
                    .await
                    .expect("fill invitation slots"),
                MutationOutcome::Applied,
                "{backend}"
            );
            invitations.push(record);
        }
        let overflow = InvitationRecord {
            id: Uuid::new_v4(),
            token_hash: hash(40),
            library_ids: vec![lib],
            created_at_ms: 1001,
            expires_at_ms: 3000,
        };
        assert_eq!(
            store
                .create_share_invitation(overflow.clone())
                .await
                .expect("bounded invitations"),
            MutationOutcome::Capacity,
            "{backend}"
        );
        assert_eq!(
            store
                .create_share_invitation(invitations[0].clone())
                .await
                .expect("duplicate remains conflict"),
            MutationOutcome::Conflict
        );
        store
            .cancel_share_invitation(invitations[0].id)
            .await
            .expect("cancel releases slot");
        assert_eq!(
            store
                .create_share_invitation(overflow)
                .await
                .expect("reuse canceled slot"),
            MutationOutcome::Applied
        );
        let after_expiry = InvitationRecord {
            id: Uuid::new_v4(),
            token_hash: hash(41),
            library_ids: vec![lib],
            created_at_ms: 2000,
            expires_at_ms: 3000,
        };
        assert_eq!(
            store
                .create_share_invitation(after_expiry)
                .await
                .expect("expired invitations release slots"),
            MutationOutcome::Applied
        );
        let local = store.sharing_identity(1000).await.expect("local identity");
        let directory = tempfile::tempdir().expect("disposable key directory");
        let key = plurx_core::secrets::open_credential_key(
            &directory.path().join("key"),
            &Default::default(),
        )
        .expect("fixture key");
        let make_import = || {
            let id = Uuid::new_v4();
            NewImport {
                id,
                source: SharingIdentity {
                    server_id: Uuid::new_v4(),
                    catalogue_epoch: Uuid::new_v4(),
                    created_at_ms: 1000,
                },
                source_name: "Capacity source".into(),
                claim_id: Uuid::new_v4(),
                credential: key
                    .seal_sharing(
                        SharingSecretPurpose::Credential,
                        local.server_id,
                        id,
                        "fixture credential",
                    )
                    .expect("seal credential"),
                claim_secret: key
                    .seal_sharing(
                        SharingSecretPurpose::Claim,
                        local.server_id,
                        id,
                        "fixture invitation",
                    )
                    .expect("seal invitation"),
                endpoints: vec![Endpoint {
                    ipv4: "100.101.102.103".parse().expect("fixture address"),
                    ipv6: None,
                    ts_fqdn: "source.example.ts.net".into(),
                    port: 32443,
                    spki_sha256: hash(7),
                }],
                now_ms: 1000,
            }
        };
        let first = make_import();
        assert_eq!(
            store
                .create_share_import(first.clone())
                .await
                .expect("first import"),
            ImportOutcome::Created
        );
        for _ in 1..32 {
            assert_eq!(
                store
                    .create_share_import(make_import())
                    .await
                    .expect("fill import slots"),
                ImportOutcome::Created
            );
        }
        assert_eq!(
            store
                .create_share_import(make_import())
                .await
                .expect("bounded imports"),
            ImportOutcome::Capacity,
            "{backend}"
        );
        let mut duplicate = first.clone();
        duplicate.id = Uuid::new_v4();
        duplicate.claim_id = Uuid::new_v4();
        assert_eq!(
            store
                .create_share_import(duplicate)
                .await
                .expect("duplicate at capacity"),
            ImportOutcome::AlreadyImported(first.id)
        );
        assert_eq!(
            store
                .sharing_imports()
                .await
                .expect("retained authority")
                .len(),
            32
        );
    })
    .await;
}
