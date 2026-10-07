//! Actual candidate writes through both SQLite Store modes.
use super::*;
use crate::{
    cluster::membership::{
        source_admission_members_with_purpose_for_unit_test,
        SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY, SHARING_PURPOSE_KEYS_CAPABILITY,
        SHARING_SESSION_PRINCIPAL_CAPABILITY,
    },
    domain::{LibraryKind, NewLibrary},
    playback_principal::PlaybackPrincipal,
    secrets::CredentialKey,
    sharing::{InvitationRecord, ShareClaim, SourceId},
    sharing_catalogue_details::CatalogueRevisionKey,
    store::{
        media_session_principal_rebuild_schema, LibraryStore, MediaSessionStore,
        SharingSourceIngressCustodyStore, SharingStore, SqliteStore,
    },
};
use std::{collections::BTreeSet, path::PathBuf};

async fn setup(store: &SqliteStore) -> (Uuid, CredentialKey) {
    setup_with_media_id(store, 1).await
}

async fn setup_with_media_id(store: &SqliteStore, media_id: i64) -> (Uuid, CredentialKey) {
    let identity = store.sharing_identity(1000).await.expect("identity");
    let library = store
        .create_library(&NewLibrary {
            name: "Source".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/synthetic")],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    let library = if media_id == 0 {
        // Genuine retained zero IDs precede the monotonic allocator factory.
        store.sharing_txn(vec![("INSERT INTO libraries(id,name,kind,paths,anime) VALUES(0,'Legacy Source','movies','[]',0)".into(),vec![]),("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(0,0,'movie','Legacy Movie','legacy movie')".into(),vec![]),("INSERT INTO files(id,item_id,path,size,mtime) VALUES(0,0,'/private/legacy-zero.mkv',20,1000)".into(),vec![])]).await.expect("retained legacy media before factory");
        0
    } else {
        library
    };
    let grant = Uuid::new_v4();
    let invitation = Uuid::new_v4();
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![library],
            created_at_ms: 1000,
            expires_at_ms: 2000,
        })
        .await
        .expect("invite");
    store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: Uuid::new_v4(),
            recipient_name: "synthetic".into(),
            credential_hash: "b".repeat(64),
            now_ms: 1001,
        })
        .await
        .expect("claim");
    store.approve_share(grant, 1, 1002).await.expect("approve");
    let credential = CredentialKey::from_bytes([17; 32]);
    let envelope =
        CatalogueRevisionKey::generate_sealed(&credential, identity.clone()).expect("fixture key");
    let mut ddl = super::super::sharing_catalogue_source::candidate_statements();
    ddl.extend(super::super::sharing_catalogue_source::candidate_item_identity_statements());
    ddl.push(super::super::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA.into());
    ddl.extend(
        media_session_principal_rebuild_schema()
            .split("-- next statement\n")
            .map(|s| s.trim().trim_end_matches(';').to_owned()),
    );
    ddl.extend(candidate_statements());
    ddl.extend(["CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER,last_seen_at INTEGER,removed_at INTEGER,role TEXT,api_address TEXT,raft_address TEXT)","CREATE TABLE cluster_node_capabilities(node_id TEXT,capability TEXT,last_seen_at INTEGER,PRIMARY KEY(node_id,capability))","CREATE TABLE cluster_node_join_staging(node_id TEXT)","CREATE TABLE cluster_node_removals(node_id TEXT)","CREATE TABLE cluster_node_removal_attempts(node_id TEXT,attempt_id TEXT)","CREATE TABLE cluster_join_tokens(token_hash TEXT,state TEXT,node_id TEXT,raft_id INTEGER)"].into_iter().map(str::to_owned));
    ddl.extend(
        [
            "CREATE TABLE cluster_node_promotions(node_id TEXT,started_at INTEGER)",
            "CREATE TABLE cluster_node_heartbeat_intents(node_id TEXT,last_seen_at INTEGER)",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    store
        .sharing_txn(ddl.into_iter().map(|sql| (sql, vec![])).collect())
        .await
        .expect("all candidate objects");
    let now = now_ms().expect("clock");
    store.sharing_txn(vec![("INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".into(),vec![identity.server_id.into(),identity.catalogue_epoch.into(),envelope.as_stored().to_owned().into()]),("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,$1,'movie','Movie','movie')".into(),vec![library.into()]),("INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/private/synthetic.mkv',20,1000)".into(),vec![]),("INSERT INTO cluster_nodes VALUES('voter',1,$1,NULL,NULL,'api','raft')".into(),vec![now.into()]),("INSERT INTO cluster_node_capabilities VALUES('voter',$1,$3),('voter',$2,$3),('voter',$4,$3),('voter',$5,$3)".into(),vec![SHARING_SESSION_PRINCIPAL_CAPABILITY.to_owned().into(),SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY.to_owned().into(),now.into(),SHARING_PURPOSE_KEYS_CAPABILITY.to_owned().into(),format!("sharing_purpose_master_v1:{}",credential.sharing_purpose_master_fingerprint()).into()]),("INSERT INTO settings(key,value,updated_at) VALUES('sharing_enabled','true',1) ON CONFLICT(key) DO UPDATE SET value='true'".into(),vec![])]).await.expect("current Source fixture");
    // These unit rows exercise routing/admission SQL only, never physical
    // driver closure or a production registry owner.
    store
        .sharing_txn(vec![(
            "INSERT INTO cluster_node_capabilities VALUES('voter',$1,$3),('voter',$2,$3)".into(),
            vec![
                crate::cluster::membership::SHARING_INGRESS_CUSTODY_CAPABILITY
                    .to_owned()
                    .into(),
                "sharing_ingress_boot_v1:01a1128a-944d-4f11-9bf2-c7599d355213"
                    .to_owned()
                    .into(),
                now.into(),
            ],
        )])
        .await
        .expect("metadata-only current boot fixture");
    store
        .sharing_txn(
            crate::cluster::membership::sharing_member_admission_guard_schema()
                .into_iter()
                .map(|sql| (sql, vec![]))
                .collect(),
        )
        .await
        .expect("guard factory");
    (grant, credential)
}
async fn intent(
    store: &SqliteStore,
    grant: Uuid,
    credential: &CredentialKey,
    name: &str,
) -> SourceSessionIntent {
    intent_for_media(store, grant, credential, name, 1).await
}

async fn intent_for_media(
    store: &SqliteStore,
    grant: Uuid,
    credential: &CredentialKey,
    name: &str,
    media_id: i64,
) -> SourceSessionIntent {
    let media_id = media_id.to_string();
    let witness = match store
        .source_item_file_witness(
            &"b".repeat(64),
            grant,
            SourceId::parse(&media_id).expect("Source candidate fixture operation"),
            SourceId::parse(&media_id).expect("Source candidate fixture operation"),
        )
        .await
        .expect("Source candidate fixture operation")
    {
        SourceDetailsRead::Authorized(w) => w,
        _ => panic!("witness"),
    };
    let rows=store.sharing_read("SELECT json_object('server_id',server_id,'catalogue_epoch',catalogue_epoch,'created_at_ms',created_at_ms) AS payload FROM sharing_identity",vec![]).await.expect("Source candidate fixture operation");
    let identity = serde_json::from_str(&rows[0]).expect("Source candidate fixture operation");
    let envelope = store
        .source_catalogue_revision_key(witness.server, witness.epoch)
        .await
        .expect("Source candidate fixture operation")
        .expect("Source candidate fixture operation");
    let key = CatalogueRevisionKey::open(credential, identity, &envelope)
        .expect("Source candidate fixture operation");
    let now = now_ms().expect("Source candidate fixture operation");
    match store
        .prepare_source_session_intent(
            SourceSessionRequest {
                principal: PlaybackPrincipal::sharing(grant, &"c".repeat(64))
                    .expect("Source candidate fixture operation"),
                request_id: name.into(),
                request_fingerprint: "d".repeat(64),
                playback_id: format!("p-{name}"),
                incarnation_id: Uuid::new_v4(),
                ingress_registry_boot_id: Uuid::parse_str("01a1128a-944d-4f11-9bf2-c7599d355213")
                    .expect("routing metadata boot, not physical proof"),
                now_ms: now,
                claim_expires_at_ms: now + 60000,
                credential_hash: "b".repeat(64),
                item_id: SourceId::parse(&media_id).expect("Source candidate fixture operation"),
                file_id: SourceId::parse(&media_id).expect("Source candidate fixture operation"),
                file_revision: key
                    .file_revision(&witness)
                    .expect("Source candidate fixture operation"),
            },
            credential,
        )
        .await
        .expect("Source candidate fixture operation")
    {
        SourceIntentRead::Ready(i) => *i,
        _ => panic!("intent"),
    }
}
fn proof() -> SourceAdmissionMembers {
    source_admission_members_with_purpose_for_unit_test(
        1,
        &BTreeSet::from([1]),
        now_ms().expect("Source candidate fixture operation"),
        &CredentialKey::from_bytes([17; 32]),
    )
    .expect("Source candidate fixture operation")
}

#[derive(Clone, Copy, Debug)]
enum AdmissionInterleave {
    Register,
    Ack,
    Seal,
    AckAll,
    WrongOwner,
    WrongRoutingOwner,
    WrongBoot,
    WrongInitialHash,
    MalformedRouting,
    RemoveFloor,
}

/// This wrapper captures a real Store read, commits a real metadata operation,
/// then returns that captured read before admission's atomic assertion. No
/// sleeps or synthetic physical receipt are involved: this qualifies accounting
/// interleavings; the daemon separately authenticates actual driver closures.
struct AdmissionInterleavedStore<'a> {
    store: &'a SqliteStore,
    assignment: &'a SourceDispatchAssignment,
    action: std::sync::Mutex<Option<AdmissionInterleave>>,
    second: crate::sharing_ingress_custody::IngressRegistration,
}

#[async_trait::async_trait]
impl super::super::sharing::Backend for AdmissionInterleavedStore<'_> {
    fn sharing_is_replicated(&self) -> bool {
        self.store.sharing_is_replicated()
    }
    async fn sharing_read(
        &self,
        sql: &str,
        values: Vec<super::super::sharing::Value>,
    ) -> Result<Vec<String>, StoreError> {
        let rows = self.store.sharing_read(sql, values).await?;
        let action =
            if sql.starts_with("SELECT json_object('revision',revision,'custody',custody_json)") {
                self.action.lock().expect("one interleaving").take()
            } else {
                None
            };
        if let Some(action) = action {
            use crate::store::sharing_source_ingress_custody::SourceCustodyWrite;
            match action {
                AdmissionInterleave::Register => {
                    assert_eq!(
                        self.store
                            .register_source_ingress_custody(
                                self.assignment,
                                &self.second,
                                &proof()
                            )
                            .await?,
                        SourceCustodyWrite::Applied
                    );
                }
                AdmissionInterleave::Ack | AdmissionInterleave::AckAll => {
                    let current = self
                        .store
                        .source_ingress_custody(self.assignment)
                        .await?
                        .expect("current exact ledger");
                    let open = current.state.open().cloned().collect::<Vec<_>>();
                    let count = if matches!(action, AdmissionInterleave::Ack) {
                        1
                    } else {
                        open.len()
                    };
                    for slot in open.into_iter().take(count) {
                        assert_eq!(
                            self.store
                                .acknowledge_source_ingress_custody(
                                    self.assignment,
                                    &slot,
                                    &"e".repeat(64)
                                )
                                .await?,
                            SourceCustodyWrite::Applied
                        );
                    }
                }
                AdmissionInterleave::Seal => {
                    assert_eq!(
                        self.store
                            .seal_source_ingress_custody(self.assignment)
                            .await?,
                        SourceCustodyWrite::Applied
                    );
                }
                AdmissionInterleave::RemoveFloor => {
                    self.store
                        .sharing_txn(vec![(
                            "DELETE FROM cluster_node_capabilities WHERE capability=$1".into(),
                            vec![
                                crate::cluster::membership::SHARING_INGRESS_CUSTODY_CAPABILITY
                                    .to_owned()
                                    .into(),
                            ],
                        )])
                        .await?;
                }
                AdmissionInterleave::WrongOwner => {
                    self.store.sharing_txn(vec![("UPDATE sharing_ingress_custody SET owner_identity=$1,revision=revision+1 WHERE incarnation_id=$2".into(), vec!["f".repeat(64).into(), self.assignment.binding().incarnation_id().into()])]).await?;
                }
                other => {
                    let (path, value) = match other {
                        AdmissionInterleave::WrongRoutingOwner => {
                            ("$.source_routing.owner_node_id", "other".to_owned())
                        }
                        AdmissionInterleave::WrongBoot => (
                            "$.source_routing.registry_boot_id",
                            Uuid::new_v4().to_string(),
                        ),
                        AdmissionInterleave::WrongInitialHash => {
                            ("$.source_routing.initial_credential_hash", "f".repeat(64))
                        }
                        AdmissionInterleave::MalformedRouting => {
                            ("$.source_routing.unexpected", "extra".to_owned())
                        }
                        _ => unreachable!("handled metadata mutation"),
                    };
                    self.store.sharing_txn(vec![("UPDATE sharing_ingress_custody SET custody_json=json_set(custody_json,$1,$2),revision=revision+1 WHERE incarnation_id=$3".into(), vec![path.to_owned().into(), value.into(), self.assignment.binding().incarnation_id().into()])]).await?;
                }
            }
        }
        Ok(rows)
    }
    async fn sharing_txn(
        &self,
        statements: Vec<super::super::sharing::Statement>,
    ) -> Result<Vec<usize>, StoreError> {
        self.store.sharing_txn(statements).await
    }
    async fn sharing_revision_key_rows(&self) -> Result<Vec<String>, StoreError> {
        self.store.sharing_revision_key_rows().await
    }
    async fn sharing_file_locator_key_rows(&self) -> Result<Vec<String>, StoreError> {
        self.store.sharing_file_locator_key_rows().await
    }
    async fn sharing_purpose_archive_rows(&self) -> Result<Vec<String>, StoreError> {
        self.store.sharing_purpose_archive_rows().await
    }
}

#[tokio::test]
async fn sharing_source_ingress_admission_accepts_current_accounting_but_refuses_changed_authority()
{
    use crate::sharing_ingress_custody::IngressRegistration;
    use crate::store::sharing_source_ingress_custody::SourceCustodyWrite;
    for action in [
        AdmissionInterleave::Register,
        AdmissionInterleave::Ack,
        AdmissionInterleave::Seal,
        AdmissionInterleave::AckAll,
        AdmissionInterleave::WrongOwner,
        AdmissionInterleave::WrongRoutingOwner,
        AdmissionInterleave::WrongBoot,
        AdmissionInterleave::WrongInitialHash,
        AdmissionInterleave::MalformedRouting,
        AdmissionInterleave::RemoveFloor,
    ] {
        let store = SqliteStore::open_in_memory().expect("actual Store");
        let (grant, credential) = setup(&store).await;
        install_fixture_ingress_context(&store).await;
        let planned = intent(&store, grant, &credential, "admission-interleave").await;
        let binding = match store
            .claim_source_media_session(&planned, &proof())
            .await
            .expect("actual claim")
        {
            SourceClaimOutcome::Acquired(value) => value,
            _ => panic!("fresh claim"),
        };
        let assignment = store
            .assign_source_dispatch(&binding, &credential, &proof())
            .await
            .expect("actual assignment")
            .expect("exact assignment");
        let boot = planned.request.ingress_registry_boot_id;
        let first = IngressRegistration {
            node_id: "voter".into(),
            boot_id: boot,
            connection_id: Uuid::new_v4(),
            driver_sequence: 1,
            registration_sequence: 1,
            closed_confirmation: None,
        };
        let second = IngressRegistration {
            connection_id: Uuid::new_v4(),
            driver_sequence: 2,
            registration_sequence: 2,
            ..first.clone()
        };
        assert_eq!(
            store
                .register_source_ingress_custody(&assignment, &first, &proof())
                .await
                .expect("first metadata registration"),
            SourceCustodyWrite::Applied
        );
        if !matches!(action, AdmissionInterleave::Register) {
            assert_eq!(
                store
                    .register_source_ingress_custody(&assignment, &second, &proof())
                    .await
                    .expect("second metadata registration"),
                SourceCustodyWrite::Applied
            );
        }
        let retained = store
            .prepare_source_ingress_admission(&assignment, boot, &proof())
            .await
            .expect("initial admission")
            .expect("current open registration");
        let original_revision = store
            .source_ingress_custody(&assignment)
            .await
            .expect("read")
            .expect("exact ledger")
            .revision;
        let interleaved = AdmissionInterleavedStore {
            store: &store,
            assignment: &assignment,
            action: std::sync::Mutex::new(Some(action)),
            second,
        };
        let result = interleaved
            .prepare_source_ingress_admission(&assignment, boot, &proof())
            .await;
        if matches!(
            action,
            AdmissionInterleave::Register | AdmissionInterleave::Ack
        ) {
            let permission = result
                .expect("benign accounting cannot become a Store error")
                .expect("current authority still admits");
            assert!(permission.assignment().same_identity(&assignment));
            assert!(
                store
                    .source_ingress_custody(&assignment)
                    .await
                    .expect("read")
                    .expect("ledger")
                    .revision
                    > original_revision,
                "the actual accounting mutation committed between read and assertion"
            );
        } else {
            assert!(
                result.is_err() || result.expect("ordinary refusal").is_none(),
                "changed current authority must refuse: {action:?}"
            );
        }
        if matches!(action, AdmissionInterleave::AckAll) {
            assert!(
                store
                    .refresh_source_ingress_admission(&retained, &proof())
                    .await
                    .expect("retained refresh")
                    .is_some(),
                "an already admitted producer permits ordinary zero-open gaps while unsealed"
            );
        }
    }
}

/// These Store fixtures never admit an accepted HTTP driver or producer. They
/// qualify SQL accounting, not physical closure. Advance their exact retained
/// empty ledger through the production CAS; do not bypass the sealed predicate
/// or manufacture a joined-driver receipt from metadata.
async fn install_fixture_ingress_context(store: &SqliteStore) {
    // This SQL-only fixture installs the canonical private cleanup context.
    // It admitted no transport or producer; these rows are not physical proof.
    use crate::store::sharing_source_schema as layout;
    store.sharing_txn(vec![
        ("CREATE TABLE cluster_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1),schema_version INTEGER NOT NULL,protocol_min INTEGER NOT NULL,protocol_max INTEGER NOT NULL,migrated_at INTEGER NOT NULL) STRICT".into(), vec![]),
        (layout::BOOT_INTENTS_SCHEMA.into(), vec![]),
        (layout::INSTALLATION_SCHEMA.into(), vec![]),
        (layout::TRANSACTION_SCHEMA.into(), vec![]),
        ("INSERT INTO cluster_meta VALUES(1,$1,1,1,1)".into(), vec![layout::SOURCE_SCHEMA_VERSION.into()]),
        ("INSERT INTO sharing_source_schema_installation VALUES(1,$1,$2,1)".into(), vec![layout::SOURCE_LAYOUT_VERSION.into(), "a".repeat(64).into()]),
        ("INSERT INTO sharing_source_schema_transaction_guard VALUES(1,1)".into(), vec![]),
    ]).await.expect("actual private schema cleanup context");
}

async fn seal_fixture_owned_empty_ingress(
    store: &SqliteStore,
    assignment: &SourceDispatchAssignment,
) {
    install_fixture_ingress_context(store).await;
    let snapshot = store
        .source_ingress_custody(assignment)
        .await
        .expect("exact fixture custody read")
        .expect("retained fixture ledger");
    assert!(!snapshot.state.is_sealed());
    assert_eq!(
        snapshot.state.open().count(),
        0,
        "fixture construction registered no transport"
    );
    assert_eq!(
        store
            .seal_source_ingress_custody(assignment)
            .await
            .expect("guarded fixture seal"),
        crate::store::sharing_source_ingress_custody::SourceCustodyWrite::Applied
    );
    let sealed = store
        .source_ingress_custody(assignment)
        .await
        .expect("sealed fixture read")
        .expect("same retained ledger");
    assert!(sealed.state.settled());
}

#[tokio::test]
async fn sharing_source_retained_zero_media_ids_claim_assign_and_release() {
    let directory = tempfile::tempdir().expect("zero ID fixture");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&directory.path().join("zero.db")).expect("pool"),
    ] {
        let (grant, key) = setup_with_media_id(&store, 0).await;
        let request = intent_for_media(&store, grant, &key, "zero-media", 0).await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&request, &proof())
            .await
            .expect("legacy zero admission")
        else {
            panic!("zero media must claim")
        };
        assert_eq!(binding.library_id().as_str(), "0");
        assert_eq!(binding.item_id().as_str(), "0");
        assert_eq!(binding.file_id().as_str(), "0");
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("zero dispatch")
            .expect("assigned zero media");
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("unsealed custody refuses release"),
            SourceReleaseOutcome::Refused
        );
        seal_fixture_owned_empty_ingress(&store, &assignment).await;
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("never activated zero release"),
            SourceReleaseOutcome::Released
        );
    }
}
async fn count(store: &SqliteStore, table: &str) -> i64 {
    store
        .sharing_read(
            &format!("SELECT json_quote(count(*)) AS payload FROM {table}"),
            vec![],
        )
        .await
        .expect("Source candidate fixture operation")[0]
        .parse()
        .expect("Source candidate fixture operation")
}

#[tokio::test]
async fn sharing_source_dispatch_assignment_current_rotation_and_corruption_fences() {
    let dir = tempfile::tempdir().expect("fixture directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory store"),
        SqliteStore::open(&dir.path().join("dispatch.db")).expect("pooled store"),
    ] {
        let (grant, key) = setup(&store).await;
        let first = intent(&store, grant, &key, "dispatch-one").await;
        let second = intent(&store, grant, &key, "dispatch-two").await;
        let SourceClaimOutcome::Acquired(first) = store
            .claim_source_media_session(&first, &proof())
            .await
            .expect("first claim")
        else {
            panic!("acquired first")
        };
        let SourceClaimOutcome::Acquired(second) = store
            .claim_source_media_session(&second, &proof())
            .await
            .expect("second claim")
        else {
            panic!("acquired second")
        };
        // Current stored credential changes; the binding is not keyed by the
        // old peer token and the trusted assignment uses current authority.
        store
            .sharing_txn(vec![(
                "UPDATE sharing_exports SET token_hash=$1 WHERE id=$2".into(),
                vec!["f".repeat(64).into(), grant.into()],
            )])
            .await
            .expect("rotate fixture credential");
        let assigned = store
            .assign_source_dispatch(&first, &key, &proof())
            .await
            .expect("assignment write")
            .expect("actual local assignment");
        assert_eq!(assigned.owner_node_id(), "voter");
        assert_eq!(assigned.dispatch_generation(), 1);
        assert!(assigned
            .validate_observation_freshness(now_ms().expect("clock") + 6000)
            .is_err());
        assert_eq!(assigned.binding().incarnation_id(), first.incarnation_id());
        let replay = store
            .assign_source_dispatch(&first, &key, &proof())
            .await
            .expect("assignment replay")
            .expect("same committed assignment");
        assert_eq!(replay.binding().incarnation_id(), first.incarnation_id());
        assert_eq!(
            store
                .release_source_never_dispatched(&first)
                .await
                .expect("release refusal"),
            SourceReleaseOutcome::Refused
        );
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("saved switch off");
        assert!(store
            .assign_source_dispatch(&second, &key, &proof())
            .await
            .expect("disabled assignment")
            .is_none());
        let unchanged = binding_row(&store, &second.principal.owner_key(), &second.request_id)
            .await
            .expect("binding")
            .expect("retained second");
        assert_eq!(unchanged.dispatch, 0);
        assert!(unchanged.request_owner.is_none());
        store.sharing_txn(vec![("UPDATE settings SET value='true' WHERE key='sharing_enabled'".into(), vec![]),("UPDATE sharing_source_session_bindings SET dispatch_generation=1 WHERE incarnation_id=$1".into(), vec![second.incarnation_id.into()])]).await.expect("inject incomplete dispatch pair");
        assert!(store
            .assign_source_dispatch(&second, &key, &proof())
            .await
            .expect("corruption refusal")
            .is_none());
        let unchanged = binding_row(&store, &second.principal.owner_key(), &second.request_id)
            .await
            .expect("binding after refusal")
            .expect("retained corrupted pair");
        assert_eq!(unchanged.dispatch, 1);
        assert!(unchanged.request_owner.is_none());
        assert_eq!(count(&store, "media_sessions").await, 0);
        assert_eq!(count(&store, "job_leases").await, 0);
    }
}

#[tokio::test]
async fn sharing_source_activation_authority_assertion_rolls_back_and_preserves_faults() {
    let dir = tempfile::tempdir().expect("fixture directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&dir.path().join("activation-guard.db")).expect("pool"),
    ] {
        let (grant, key) = setup(&store).await;
        let intent = intent(&store, grant, &key, "guarded-start").await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&intent, &proof())
            .await
            .expect("claim")
        else {
            panic!("acquired")
        };
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("assignment")
            .expect("assigned local worker");
        let SourceWriteAuthorityRead::Ready(authority) = store
            .prepare_source_activation_authority(&assignment, &key, &proof())
            .await
            .expect("private authority")
        else {
            panic!("current witness")
        };
        let now = now_ms().expect("clock");
        let activation = crate::domain::MediaSessionActivation {
            incarnation_id: binding.incarnation_id.to_string(),
            session_id: Uuid::new_v4().to_string(),
            principal: binding.principal.clone(),
            playback_id: binding.playback_id.clone(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(binding.request_id.clone()),
            request_fingerprint: binding.request_fingerprint.clone(),
            owner_node_id: "voter".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("toggle race");
        let guard = source_activation_guard(&authority, &activation)
            .expect("closed assertion")
            .expect("matching tuple");
        let error=store.sharing_txn(vec![guard,("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES('guard-sentinel','voter',1,1,$1,$2)".into(),vec![(now+60000).into(),now.into()])]).await.expect_err("same transaction refuses");
        assert!(source_write_refused(&error));
        assert_eq!(count(&store, "job_leases").await, 0);
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='true' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("enable");
        let guard = source_activation_guard(&authority, &activation)
            .expect("valid assertion")
            .expect("matching authority");
        let error=store.sharing_txn(vec![guard,("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES(NULL,'voter',1,1,$1,$2)".into(),vec![(now+60000).into(),now.into()])]).await.expect_err("genuine storage fault");
        assert!(!source_write_refused(&error));
        assert_eq!(count(&store, "job_leases").await, 0);
        store
            .sharing_txn(vec![(
                "UPDATE files SET path='/private/replacement.mkv' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("file race");
        assert!(matches!(
            store
                .prepare_source_activation_authority(&assignment, &key, &proof())
                .await
                .expect("revalidate actual file"),
            SourceWriteAuthorityRead::Unavailable
        ));
        assert_eq!(count(&store, "media_sessions").await, 0);
    }
}

#[tokio::test]
async fn sharing_source_sqlite_first_activation_current_authority_and_lineage() {
    let dir = tempfile::tempdir().expect("fixture directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&dir.path().join("source-activation.db")).expect("pool"),
    ] {
        let (grant, key) = setup(&store).await;
        let request = intent(&store, grant, &key, "first-activation").await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&request, &proof())
            .await
            .expect("claim")
        else {
            panic!("Source reservation")
        };
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("assignment")
            .expect("actual local worker");
        let SourceWriteAuthorityRead::Ready(authority) = store
            .prepare_source_activation_authority(&assignment, &key, &proof())
            .await
            .expect("authority")
        else {
            panic!("current private witness")
        };
        let now = now_ms().expect("clock");
        let mut activation = crate::domain::MediaSessionActivation {
            incarnation_id: binding.incarnation_id.to_string(),
            session_id: Uuid::new_v4().to_string(),
            principal: binding.principal.clone(),
            playback_id: binding.playback_id.clone(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(binding.request_id.clone()),
            request_fingerprint: binding.request_fingerprint.clone(),
            owner_node_id: "voter".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        assert!(
            store.activate_media_session(&activation).await.is_err(),
            "ordinary Shared entry remains closed"
        );
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("switch race");
        assert!(store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("typed switch refusal")
            .is_none());
        for table in ["media_sessions", "media_playback_pointers", "job_leases"] {
            assert_eq!(count(&store, table).await, 0);
        }
        store.sharing_txn(vec![("UPDATE settings SET value='true' WHERE key='sharing_enabled'".into(),vec![]),("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES($1,'foreign',1,1,$2,$3)".into(),vec![format!("session:{}",binding.incarnation_id).into(),(now+90000).into(),now.into()]),("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign','foreign','foreign','media_session',$1,1,1)".into(),vec![binding.incarnation_id.into()])]).await.expect("foreign retained resources");
        assert!(store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("foreign lineage refusal")
            .is_none());
        assert!(matches!(
            store
                .prepare_source_activation_authority(&assignment, &key, &proof())
                .await
                .expect("foreign mint refusal"),
            SourceWriteAuthorityRead::Unavailable
        ));
        let rows = store
            .sharing_read(
                "SELECT json_quote(expires_at_ms) AS payload FROM job_leases WHERE resource=$1",
                vec![format!("session:{}", binding.incarnation_id).into()],
            )
            .await
            .expect("preserved expiry");
        assert_eq!(rows[0], (now + 90000).to_string());
        assert_eq!(count(&store, "cache_consumer_pins").await, 1);
        store
            .sharing_txn(vec![
                (
                    "DELETE FROM job_leases WHERE resource=$1".into(),
                    vec![format!("session:{}", binding.incarnation_id).into()],
                ),
                (
                    "DELETE FROM cache_consumer_pins WHERE consumer_id=$1".into(),
                    vec![binding.incarnation_id.into()],
                ),
            ])
            .await
            .expect("remove only fixture injections");
        store
            .sharing_txn(vec![(
                "UPDATE files SET path='/private/replacement.mkv' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("private file race");
        assert!(store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("same-write file refusal")
            .is_none());
        store
            .sharing_txn(vec![(
                "UPDATE files SET path='/private/synthetic.mkv' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("restore fixture file");
        let SourceWriteAuthorityRead::Ready(authority) = store
            .prepare_source_activation_authority(&assignment, &key, &proof())
            .await
            .expect("fresh authority")
        else {
            panic!("restored file authority")
        };
        activation.now_ms = now_ms().expect("clock");
        activation.lease_expires_at_ms = activation.now_ms + 60000;
        let mut foreign = activation.clone();
        foreign.principal =
            PlaybackPrincipal::sharing(grant, &"e".repeat(64)).expect("other viewer");
        assert!(store
            .activate_source_media_session(&authority, &foreign)
            .await
            .expect("foreign tuple refusal")
            .is_none());
        store.sharing_txn(vec![("CREATE TRIGGER source_activation_storage_fault BEFORE INSERT ON media_sessions BEGIN SELECT RAISE(ABORT,'source_activation_fixture_fault'); END".into(),vec![])]).await.expect("actual writer fault fixture");
        let error = store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect_err("genuine database fault propagates");
        assert!(!source_write_refused(&error));
        assert_eq!(count(&store, "job_leases").await, 0);
        assert_eq!(count(&store, "media_sessions").await, 0);
        store
            .sharing_txn(vec![(
                "DROP TRIGGER source_activation_storage_fault".into(),
                vec![],
            )])
            .await
            .expect("remove only fixture fault");
        let created = store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("guarded first activation")
            .expect("Source route");
        assert_eq!(created.route.principal, binding.principal);
        assert_eq!(created.route.session_id, activation.session_id);
        assert_eq!(
            created.route.publication_ready_at_ms,
            crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        let old_expiry = created.route.lease_expires_at_ms;
        activation.lease_expires_at_ms += 60000;
        let replay = store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("exact replay")
            .expect("same route");
        assert_eq!(
            replay.route.lease_expires_at_ms, old_expiry,
            "replay cannot renew"
        );
        assert_eq!(count(&store, "media_sessions").await, 1);
        assert_eq!(count(&store, "media_playback_pointers").await, 1);
        let retained = binding_row(&store, &binding.principal.owner_key(), &binding.request_id)
            .await
            .expect("binding")
            .expect("held obligation");
        assert_eq!(retained.reservation, "held");
        assert!(retained.start_resolved.is_none());
        assert_eq!(
            store
                .release_source_never_dispatched(&binding)
                .await
                .expect("release refusal"),
            SourceReleaseOutcome::Refused
        );
    }
}
#[tokio::test]
async fn sharing_source_claim_atomic_replay_caps_authority_and_never_dispatched_release() {
    let dir = tempfile::tempdir().expect("Source candidate fixture operation");
    for store in [
        SqliteStore::open_in_memory().expect("Source candidate fixture operation"),
        SqliteStore::open(&dir.path().join("source.db"))
            .expect("Source candidate fixture operation"),
    ] {
        let (grant, credential) = setup(&store).await;
        let first = intent(&store, grant, &credential, "first").await;
        let binding = match store
            .claim_source_media_session(&first, &proof())
            .await
            .expect("claim")
        {
            SourceClaimOutcome::Acquired(b) => b,
            _ => panic!("acquired"),
        };
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 1);
        assert_eq!(count(&store, "media_session_requests").await, 1);
        let retry = intent(&store, grant, &credential, "first").await;
        match store
            .claim_source_media_session(&retry, &proof())
            .await
            .expect("Source candidate fixture operation")
        {
            SourceClaimOutcome::InFlight(b) => {
                assert_eq!(b.incarnation_id(), binding.incarnation_id())
            }
            _ => panic!("exact replay"),
        };
        let second = intent(&store, grant, &credential, "second").await;
        assert!(matches!(
            store
                .claim_source_media_session(&second, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Acquired(_)
        ));
        let third = intent(&store, grant, &credential, "third").await;
        assert!(matches!(
            store
                .claim_source_media_session(&third, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Capacity(SourceCapacity::PendingStarts)
        ));
        assert_eq!(
            count(&store, "sharing_source_session_bindings").await,
            2,
            "failed proposal rolls back"
        );
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("Source candidate fixture operation");
        assert!(matches!(
            store
                .claim_source_media_session(&third, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Unavailable
        ));
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value=$1 WHERE key='sharing_enabled'".into(),
                vec![format!("{}true", " ".repeat(100000)).into()],
            )])
            .await
            .expect("bounded invalid saved switch fixture");
        assert!(matches!(
            store
                .claim_source_media_session(&third, &proof())
                .await
                .expect("bounded saved choice refusal"),
            SourceClaimOutcome::Unavailable
        ));
        assert_eq!(
            store
                .release_source_never_dispatched(&binding)
                .await
                .expect("Source candidate fixture operation"),
            SourceReleaseOutcome::Released,
            "disabled sharing permits proven retirement"
        );
        assert_eq!(
            store
                .release_source_never_dispatched(&binding)
                .await
                .expect("Source candidate fixture operation"),
            SourceReleaseOutcome::ExactReplay
        );
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='true' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("Source candidate fixture operation");
        assert!(matches!(
            store
                .claim_source_media_session(&retry, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Retired(_)
        ));
        assert!(matches!(
            store
                .claim_source_media_session(&third, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Acquired(_)
        ));
        let fourth = intent(&store, grant, &credential, "fourth").await;
        store
            .sharing_txn(vec![(
                "UPDATE files SET path='/private/replaced.mkv' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("Source candidate fixture operation");
        assert!(matches!(
            store
                .claim_source_media_session(&fourth, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Unavailable
        ));
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 3);
    }
}

async fn more_grant(store: &SqliteStore, template: Uuid, index: usize) -> Uuid {
    let grant = Uuid::new_v4();
    let invitation = Uuid::new_v4();
    // Fixture only: distinct approved grants preserve the same current credential
    // so the capacity scenario varies grant identity, not Source file identity.
    store.sharing_txn(vec![("INSERT INTO sharing_invitations SELECT $1,$2,library_ids_json,created_at_ms,expires_at_ms,'consumed',NULL,NULL FROM sharing_invitations LIMIT 1".into(),vec![invitation.into(),format!("{index:064x}").into()]),("INSERT INTO sharing_exports SELECT $1,$2,recipient_server_id,recipient_name,$3,scope_generation,credential_generation,catalogue_generation,mutation_generation,state,pending_expires_at_ms,created_at_ms,updated_at_ms FROM sharing_exports WHERE id=$4".into(),vec![grant.into(),invitation.into(),format!("{:064x}",index+100).into(),template.into()]),("INSERT INTO sharing_export_libraries SELECT $1,library_id FROM sharing_export_libraries WHERE grant_id=$2".into(),vec![grant.into(),template.into()])]).await.expect("Source candidate fixture operation");
    grant
}
async fn intent_hash(
    store: &SqliteStore,
    grant: Uuid,
    credential: &CredentialKey,
    name: &str,
    hash: String,
) -> SourceSessionIntent {
    let witness = match store
        .source_item_file_witness(
            &hash,
            grant,
            SourceId::parse("1").expect("Source candidate fixture operation"),
            SourceId::parse("1").expect("Source candidate fixture operation"),
        )
        .await
        .expect("Source candidate fixture operation")
    {
        SourceDetailsRead::Authorized(w) => w,
        _ => panic!("current witness"),
    };
    let rows=store.sharing_read("SELECT json_object('server_id',server_id,'catalogue_epoch',catalogue_epoch,'created_at_ms',created_at_ms) AS payload FROM sharing_identity",vec![]).await.expect("Source candidate fixture operation");
    let envelope = store
        .source_catalogue_revision_key(witness.server, witness.epoch)
        .await
        .expect("Source candidate fixture operation")
        .expect("Source candidate fixture operation");
    let key = CatalogueRevisionKey::open(
        credential,
        serde_json::from_str(&rows[0]).expect("Source candidate fixture operation"),
        &envelope,
    )
    .expect("Source candidate fixture operation");
    let now = now_ms().expect("Source candidate fixture operation");
    match store
        .prepare_source_session_intent(
            SourceSessionRequest {
                principal: PlaybackPrincipal::sharing(grant, &"c".repeat(64))
                    .expect("Source candidate fixture operation"),
                request_id: name.into(),
                request_fingerprint: "d".repeat(64),
                playback_id: format!("p-{name}"),
                incarnation_id: Uuid::new_v4(),
                ingress_registry_boot_id: Uuid::parse_str("01a1128a-944d-4f11-9bf2-c7599d355213")
                    .expect("routing metadata boot, not physical proof"),
                now_ms: now,
                claim_expires_at_ms: now + 60000,
                credential_hash: hash,
                item_id: SourceId::parse("1").expect("Source candidate fixture operation"),
                file_id: SourceId::parse("1").expect("Source candidate fixture operation"),
                file_revision: key
                    .file_revision(&witness)
                    .expect("Source candidate fixture operation"),
            },
            credential,
        )
        .await
        .expect("Source candidate fixture operation")
    {
        SourceIntentRead::Ready(i) => *i,
        _ => panic!("intent"),
    }
}
#[tokio::test]
async fn sharing_source_capacity_keeps_expired_and_resolved_starts_until_proven_release() {
    let dir = tempfile::tempdir().expect("Source candidate fixture operation");
    for store in [
        SqliteStore::open_in_memory().expect("Source candidate fixture operation"),
        SqliteStore::open(&dir.path().join("caps.db")).expect("Source candidate fixture operation"),
    ] {
        let (grant, credential) = setup(&store).await;
        let mut bindings = Vec::new();
        for n in 0..4 {
            let i = intent(&store, grant, &credential, &format!("grant-{n}")).await;
            let b = match store
                .claim_source_media_session(&i, &proof())
                .await
                .expect("Source candidate fixture operation")
            {
                SourceClaimOutcome::Acquired(b) => b,
                _ => panic!("held"),
            };
            bindings.push(b);
            store.sharing_txn(vec![("UPDATE sharing_source_session_bindings SET start_resolved_at_ms=created_at_ms WHERE incarnation_id=$1".into(),vec![bindings.last().expect("Source candidate fixture operation").incarnation_id().into()]),("UPDATE media_session_requests SET state='failed',claim_expires_at_ms=1 WHERE request_id=$1".into(),vec![format!("grant-{n}").into()])]).await.expect("Source candidate fixture operation");
        }
        let fifth = intent(&store, grant, &credential, "grant-5").await;
        assert!(
            matches!(
                store
                    .claim_source_media_session(&fifth, &proof())
                    .await
                    .expect("Source candidate fixture operation"),
                SourceClaimOutcome::Capacity(SourceCapacity::GrantSlots)
            ),
            "expired/failed request is not physical release"
        );
        for index in 1..=2 {
            let other = more_grant(&store, grant, index).await;
            for n in 0..2 {
                let i = intent_hash(
                    &store,
                    other,
                    &credential,
                    &format!("other-{index}-{n}"),
                    format!("{:064x}", index + 100),
                )
                .await;
                assert!(matches!(
                    store
                        .claim_source_media_session(&i, &proof())
                        .await
                        .expect("Source candidate fixture operation"),
                    SourceClaimOutcome::Acquired(_)
                ));
            }
        }
        let other = more_grant(&store, grant, 3).await;
        let ninth = intent_hash(&store, other, &credential, "ninth", format!("{:064x}", 103)).await;
        assert!(matches!(
            store
                .claim_source_media_session(&ninth, &proof())
                .await
                .expect("Source candidate fixture operation"),
            SourceClaimOutcome::Capacity(SourceCapacity::SourceSlots)
        ));
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 8);
        assert_eq!(
            store
                .release_source_never_dispatched(&bindings[0])
                .await
                .expect("Source candidate fixture operation"),
            SourceReleaseOutcome::Refused,
            "resolved start is not never-dispatched proof"
        );
    }
}
#[tokio::test]
async fn sharing_source_request_insert_failure_rolls_back_and_foreign_pin_blocks_release() {
    let store = SqliteStore::open_in_memory().expect("Source candidate fixture operation");
    let (grant, credential) = setup(&store).await;
    let i = intent(&store, grant, &credential, "injected").await;
    store.sharing_txn(vec![("CREATE TRIGGER source_test_request_failure BEFORE INSERT ON media_session_requests WHEN NEW.request_id='injected' BEGIN SELECT RAISE(ABORT,'injected request failure'); END".into(),vec![])]).await.expect("Source candidate fixture operation");
    assert!(store
        .claim_source_media_session(&i, &proof())
        .await
        .is_err());
    assert_eq!(count(&store, "sharing_source_session_bindings").await, 0);
    assert_eq!(count(&store, "media_session_requests").await, 0);
    store
        .sharing_txn(vec![(
            "DROP TRIGGER source_test_request_failure".into(),
            vec![],
        )])
        .await
        .expect("Source candidate fixture operation");
    let b = match store
        .claim_source_media_session(&i, &proof())
        .await
        .expect("Source candidate fixture operation")
    {
        SourceClaimOutcome::Acquired(b) => b,
        _ => panic!("acquired"),
    };
    store.sharing_txn(vec![("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign','foreign','foreign','media_session',$1,1,1)".into(),vec![b.incarnation_id().into()])]).await.expect("Source candidate fixture operation");
    assert_eq!(
        store
            .release_source_never_dispatched(&b)
            .await
            .expect("Source candidate fixture operation"),
        SourceReleaseOutcome::Refused
    );
    assert_eq!(count(&store, "cache_consumer_pins").await, 1);
    store
        .sharing_txn(vec![(
            "DELETE FROM cache_consumer_pins WHERE storage_id='foreign'".into(),
            vec![],
        )])
        .await
        .expect("Source candidate fixture operation");
    assert_eq!(
        store
            .release_source_never_dispatched(&b)
            .await
            .expect("Source candidate fixture operation"),
        SourceReleaseOutcome::Released
    );
}
#[tokio::test]
async fn sharing_source_schema_and_request_corruption_refuse_without_repair() {
    let dir = tempfile::tempdir().expect("Source candidate fixture operation");
    for store in [
        SqliteStore::open_in_memory().expect("Source candidate fixture operation"),
        SqliteStore::open(&dir.path().join("corrupt.db"))
            .expect("Source candidate fixture operation"),
    ] {
        let (grant, credential) = setup(&store).await;
        let first = intent(&store, grant, &credential, "first").await;
        let b = match store
            .claim_source_media_session(&first, &proof())
            .await
            .expect("Source candidate fixture operation")
        {
            SourceClaimOutcome::Acquired(b) => b,
            _ => panic!("acquired"),
        };
        let foreign = Uuid::new_v4();
        let foreign_owner = PlaybackPrincipal::sharing(foreign, &"c".repeat(64))
            .expect("Source candidate fixture operation")
            .owner_key();
        store.sharing_txn(vec![("UPDATE media_session_requests SET owner_key=$1,share_grant_id=$2 WHERE request_id='first'".into(),vec![foreign_owner.into(),foreign.into()])]).await.expect("Source candidate fixture operation");
        assert!(
            matches!(
                store
                    .claim_source_media_session(&first, &proof())
                    .await
                    .expect("Source candidate fixture operation"),
                SourceClaimOutcome::Unavailable
            ),
            "foreign canonical principal cannot borrow another binding"
        );
        store.sharing_txn(vec![("UPDATE media_session_requests SET owner_key=$1,share_grant_id=$2 WHERE request_id='first'".into(),vec![first.request.principal.owner_key().into(),grant.into()])]).await.expect("Source candidate fixture operation");
        store
            .sharing_txn(vec![(
                "DELETE FROM media_session_requests WHERE request_id='first'".into(),
                vec![],
            )])
            .await
            .expect("Source candidate fixture operation");
        assert!(
            matches!(
                store
                    .claim_source_media_session(&first, &proof())
                    .await
                    .expect("Source candidate fixture operation"),
                SourceClaimOutcome::Unavailable
            ),
            "dangling reservation is not a usable replay"
        );
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 1);
        assert!(
            store
                .sharing_txn(vec![(
                    "DELETE FROM sharing_source_session_bindings WHERE incarnation_id=$1".into(),
                    vec![b.incarnation_id().into()]
                )])
                .await
                .is_err(),
            "held obligations cannot be pruned"
        );
        let next = intent(&store, grant, &credential, "next").await;
        store
            .sharing_txn(vec![(
                "ALTER TABLE media_session_requests ADD COLUMN spoofed INTEGER".into(),
                vec![],
            )])
            .await
            .expect("Source candidate fixture operation");
        assert!(
            matches!(
                store
                    .claim_source_media_session(&next, &proof())
                    .await
                    .expect("Source candidate fixture operation"),
                SourceClaimOutcome::Unavailable
            ),
            "names and owner PK alone do not qualify canonical schema"
        );
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 1);
        assert_eq!(count(&store, "media_session_requests").await, 0);
    }
}
#[tokio::test]
async fn sharing_source_retained_bound_preserves_exact_replay_and_rejects_new_identity() {
    let store = SqliteStore::open_in_memory().expect("Source candidate fixture operation");
    let (grant, credential) = setup(&store).await;
    let first = intent(&store, grant, &credential, "first").await;
    assert!(matches!(
        store
            .claim_source_media_session(&first, &proof())
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Acquired(_)
    ));
    store.sharing_txn(vec![("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<4095) INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms,released_at_ms,release_fingerprint) SELECT printf('00000000-0000-4000-a000-%012x',x),owner_key,share_grant_id,share_viewer_key,'retained-'||x,request_fingerprint,'retained-'||x,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,'released',2,0,1,2,request_fingerprint FROM n,sharing_source_session_bindings WHERE request_id='first'".into(),vec![])]).await.expect("Source candidate fixture operation");
    assert_eq!(count(&store, "sharing_source_session_bindings").await, 4096);
    let retry = intent(&store, grant, &credential, "first").await;
    assert!(matches!(
        store
            .claim_source_media_session(&retry, &proof())
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::InFlight(_)
    ));
    let next = intent(&store, grant, &credential, "next").await;
    assert!(matches!(
        store
            .claim_source_media_session(&next, &proof())
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Capacity(SourceCapacity::RetainedBindings)
    ));
    assert_eq!(count(&store, "sharing_source_session_bindings").await, 4096);
}

#[tokio::test]
async fn sharing_source_replay_conflict_does_not_bypass_revoked_current_authority() {
    let store = SqliteStore::open_in_memory().expect("SQLite");
    let (grant, credential) = setup(&store).await;
    let first = intent(&store, grant, &credential, "first").await;
    assert!(matches!(
        store
            .claim_source_media_session(&first, &proof())
            .await
            .expect("initial claim"),
        SourceClaimOutcome::Acquired(_)
    ));
    let mut changed = intent(&store, grant, &credential, "first").await;
    changed.request.request_fingerprint = "e".repeat(64);
    assert!(matches!(
        store
            .claim_source_media_session(&changed, &proof())
            .await
            .expect("authorized changed identity"),
        SourceClaimOutcome::Conflict
    ));
    store
        .sharing_txn(vec![(
            "UPDATE sharing_exports SET state='disabled' WHERE id=$1".into(),
            vec![grant.into()],
        )])
        .await
        .expect("grant disabled between intent and replay");
    assert!(matches!(
        store
            .claim_source_media_session(&changed, &proof())
            .await
            .expect("current authority refusal"),
        SourceClaimOutcome::Unavailable
    ));
    assert_eq!(count(&store, "sharing_source_session_bindings").await, 1);
}

#[tokio::test]
async fn sharing_source_owned_renewal_current_authority_and_exact_lease() {
    let dir = tempfile::tempdir().expect("fixture directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&dir.path().join("source-activation.db")).expect("pool"),
    ] {
        let (grant, key) = setup(&store).await;
        let request = intent(&store, grant, &key, "first-activation").await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&request, &proof())
            .await
            .expect("claim")
        else {
            panic!("Source reservation")
        };
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("assignment")
            .expect("actual local worker");
        let SourceWriteAuthorityRead::Ready(authority) = store
            .prepare_source_activation_authority(&assignment, &key, &proof())
            .await
            .expect("authority")
        else {
            panic!("current private witness")
        };
        let now = now_ms().expect("clock");
        let activation = crate::domain::MediaSessionActivation {
            incarnation_id: binding.incarnation_id.to_string(),
            session_id: Uuid::new_v4().to_string(),
            principal: binding.principal.clone(),
            playback_id: binding.playback_id.clone(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(binding.request_id.clone()),
            request_fingerprint: binding.request_fingerprint.clone(),
            owner_node_id: "voter".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        let created = store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("guarded activation")
            .expect("Source route");
        assert!(matches!(
            store
                .prepare_source_owned_route_authority(&assignment, &key, &proof())
                .await
                .expect("unresolved mint"),
            SourceOwnedRouteAuthorityRead::Unavailable
        ));
        // Fixture-only published state: this test qualifies current-owner
        // renewal, not a producer or a production readiness transition.
        store.sharing_txn(vec![("UPDATE media_sessions SET publication_ready_at_ms=0 WHERE incarnation_id=$1".into(),vec![binding.incarnation_id.into()]),("UPDATE media_session_requests SET state='resolved',response_json='{}' WHERE incarnation_id=$1".into(),vec![binding.incarnation_id.into()]),("UPDATE sharing_source_session_bindings SET start_resolved_at_ms=$2 WHERE incarnation_id=$1".into(),vec![binding.incarnation_id.into(),now.into()])]).await.expect("fixture-only resolved route");
        let SourceOwnedRouteAuthorityRead::Ready(owned) = store
            .prepare_source_owned_route_authority(&assignment, &key, &proof())
            .await
            .expect("owned witness")
        else {
            panic!("current owned route")
        };
        let renewal = crate::domain::MediaSessionRenewal {
            incarnation_id: binding.incarnation_id.to_string(),
            owner_epoch: 1,
            produced_playable_through_ms: 1000,
            fetched_through_ms: 500,
            media_sequence: 1,
        };
        let at = now_ms().expect("clock");
        let expiry = created.route.lease_expires_at_ms + 60000;
        assert!(store
            .renew_media_sessions("voter", std::slice::from_ref(&renewal), at, expiry)
            .await
            .expect("ordinary refusal")
            .is_empty());
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("off race");
        assert!(store
            .renew_source_media_session(&owned, &renewal, at, expiry)
            .await
            .expect("typed refusal")
            .is_none());
        assert_eq!(
            store
                .media_session_route_by_incarnation(&renewal.incarnation_id)
                .await
                .expect("route")
                .expect("retained")
                .lease_expires_at_ms,
            created.route.lease_expires_at_ms
        );
        store
            .sharing_txn(vec![
                (
                    "UPDATE settings SET value='true' WHERE key='sharing_enabled'".into(),
                    vec![],
                ),
                (
                    "UPDATE files SET path='/private/changed.mkv' WHERE id=1".into(),
                    vec![],
                ),
            ])
            .await
            .expect("file race");
        assert!(store
            .renew_source_media_session(&owned, &renewal, at, expiry)
            .await
            .expect("file refusal")
            .is_none());
        store
            .sharing_txn(vec![(
                "UPDATE files SET path='/private/synthetic.mkv' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("restore fixture");
        let SourceOwnedRouteAuthorityRead::Ready(owned) = store
            .prepare_source_owned_route_authority(&assignment, &key, &proof())
            .await
            .expect("fresh witness")
        else {
            panic!("restored owned witness")
        };
        store.sharing_txn(vec![("UPDATE job_leases SET owner_node_id='foreign' WHERE resource=$1".into(),vec![format!("session:{}",binding.incarnation_id).into()]),("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign','foreign','foreign','media_session','unrelated-retained',2,1)".into(),vec![])]).await.expect("foreign lease and pin fixture");
        assert!(store
            .renew_source_media_session(&owned, &renewal, now_ms().expect("clock"), expiry)
            .await
            .expect("foreign lease refusal")
            .is_none());
        assert!(matches!(
            store
                .prepare_source_owned_route_authority(&assignment, &key, &proof())
                .await
                .expect("foreign lease mint refusal"),
            SourceOwnedRouteAuthorityRead::Unavailable
        ));
        let rows=store.sharing_read("SELECT json_object('node',owner_node_id,'expiry',expires_at_ms) AS payload FROM job_leases WHERE resource=$1",vec![format!("session:{}",binding.incarnation_id).into()]).await.expect("foreign unchanged");
        let row: serde_json::Value = serde_json::from_str(&rows[0]).expect("row");
        assert_eq!(row["node"], "foreign");
        assert_eq!(row["expiry"], created.route.lease_expires_at_ms);
        store
            .sharing_txn(vec![(
                "UPDATE job_leases SET owner_node_id='voter' WHERE resource=$1".into(),
                vec![format!("session:{}", binding.incarnation_id).into()],
            )])
            .await
            .expect("restore only injected lease owner");
        store.sharing_txn(vec![("CREATE TRIGGER source_renewal_fault BEFORE UPDATE OF lease_expires_at_ms ON media_sessions BEGIN SELECT RAISE(ABORT,'source_renewal_fixture_fault'); END".into(),vec![])]).await.expect("writer fault");
        let error = store
            .renew_source_media_session(&owned, &renewal, now_ms().expect("clock"), expiry)
            .await
            .expect_err("real fault");
        assert!(!source_write_refused(&error));
        let rows = store
            .sharing_read(
                "SELECT json_quote(expires_at_ms) AS payload FROM job_leases WHERE resource=$1",
                vec![format!("session:{}", binding.incarnation_id).into()],
            )
            .await
            .expect("lease rollback");
        assert_eq!(rows[0], created.route.lease_expires_at_ms.to_string());
        store
            .sharing_txn(vec![("DROP TRIGGER source_renewal_fault".into(), vec![])])
            .await
            .expect("remove fixture fault");
        let renewed = store
            .renew_source_media_session(&owned, &renewal, now_ms().expect("clock"), expiry)
            .await
            .expect("guarded renewal")
            .expect("same owner");
        assert_eq!(renewed.lease_expires_at_ms, expiry);
        assert_eq!(renewed.produced_playable_through_ms, 1000);
        assert!(store
            .renew_source_media_session(&owned, &renewal, now_ms().expect("clock"), expiry + 60000)
            .await
            .expect("stale lease proof")
            .is_none());
        let SourceOwnedRouteAuthorityRead::Ready(fresh) = store
            .prepare_source_owned_route_authority(&assignment, &key, &proof())
            .await
            .expect("fresh lease proof")
        else {
            panic!("renewed owned route")
        };
        assert_eq!(fresh.lease_revision, owned.lease_revision + 1);
        let rows=store.sharing_read("SELECT json_quote(expires_at_ms) AS payload FROM cache_consumer_pins WHERE consumer_id='unrelated-retained'",vec![]).await.expect("foreign pin retained");
        assert_eq!(rows[0], "1");
        tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
        assert!(store
            .renew_source_media_session(&fresh, &renewal, now_ms().expect("clock"), expiry + 60000)
            .await
            .expect("delayed current observation refused")
            .is_none());
        let SourceOwnedRouteAuthorityRead::Ready(current) = store
            .prepare_source_owned_route_authority(&assignment, &key, &proof())
            .await
            .expect("old assignment fresh observation")
        else {
            panic!("old assignment is lineage, not a perpetual freshness veto")
        };
        assert!(store
            .renew_source_media_session(
                &current,
                &renewal,
                now_ms().expect("clock"),
                expiry + 60000
            )
            .await
            .expect("fresh observation renews old assignment")
            .is_some());
        assert_eq!(
            binding_row(&store, &binding.principal.owner_key(), &binding.request_id)
                .await
                .expect("binding")
                .expect("retained")
                .reservation,
            "held"
        );
        assert_eq!(
            store
                .release_source_never_dispatched(&binding)
                .await
                .expect("release stays closed"),
            SourceReleaseOutcome::Refused
        );
    }
}

#[tokio::test]
async fn sharing_source_publication_atomic_ready_zero_and_exact_replay() {
    let dir = tempfile::tempdir().expect("directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&dir.path().join("source-publication.db")).expect("pool"),
    ] {
        let (grant, key) = setup(&store).await;
        let request = intent(&store, grant, &key, "publication").await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&request, &proof())
            .await
            .expect("claim")
        else {
            panic!("binding")
        };
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("assignment")
            .expect("worker");
        let SourceWriteAuthorityRead::Ready(authority) = store
            .prepare_source_activation_authority(&assignment, &key, &proof())
            .await
            .expect("activation authority")
        else {
            panic!("authority")
        };
        let now = now_ms().expect("clock");
        let activation = crate::domain::MediaSessionActivation {
            incarnation_id: binding.incarnation_id.to_string(),
            session_id: Uuid::new_v4().to_string(),
            principal: binding.principal.clone(),
            playback_id: binding.playback_id.clone(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(binding.request_id.clone()),
            request_fingerprint: binding.request_fingerprint.clone(),
            owner_node_id: "voter".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        store
            .activate_source_media_session(&authority, &activation)
            .await
            .expect("activation")
            .expect("blocked route");
        assert!(
            store
                .publish_media_session_activation(
                    &binding.principal,
                    &binding.request_id,
                    &binding.incarnation_id.to_string(),
                    now
                )
                .await
                .is_err(),
            "ordinary Shared publication stays closed"
        );
        let SourcePublicationAuthorityRead::Ready(publication) = store
            .prepare_source_publication_authority(&assignment, &key, &proof())
            .await
            .expect("publication permission")
        else {
            panic!("blocked permission")
        };
        store
            .sharing_txn(vec![(
                "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                vec![],
            )])
            .await
            .expect("switch off");
        assert!(store
            .complete_source_media_session_publication(&publication)
            .await
            .expect("switch refusal")
            .is_none());
        assert_eq!(
            store
                .media_session_route_by_incarnation(&binding.incarnation_id.to_string())
                .await
                .expect("route")
                .expect("held route")
                .publication_ready_at_ms,
            crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        store.sharing_txn(vec![("UPDATE settings SET value='true' WHERE key='sharing_enabled'".into(),vec![]),("CREATE TRIGGER source_publication_ignore BEFORE UPDATE OF start_resolved_at_ms ON sharing_source_session_bindings BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored accounting writer");
        assert!(store
            .complete_source_media_session_publication(&publication)
            .await
            .expect("coupled rollback refusal")
            .is_none());
        assert_eq!(
            store
                .media_session_route_by_incarnation(&binding.incarnation_id.to_string())
                .await
                .expect("route")
                .expect("rollback route")
                .publication_ready_at_ms,
            crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        let rows=store.sharing_read("SELECT json_quote(state) AS payload FROM media_session_requests WHERE incarnation_id=$1",vec![binding.incarnation_id.into()]).await.expect("request rollback");
        assert_eq!(rows, vec!["\"starting\"".to_owned()]);
        store
            .sharing_txn(vec![(
                "DROP TRIGGER source_publication_ignore".into(),
                vec![],
            )])
            .await
            .expect("fault removed");
        let route = store
            .complete_source_media_session_publication(&publication)
            .await
            .expect("publication")
            .expect("ready route");
        assert_eq!(route.publication_ready_at_ms, 0);
        assert_eq!(route.principal, binding.principal);
        assert_eq!(route.session_id, activation.session_id);
        assert!(
            matches!(store.claim_source_media_session(&request,&proof()).await.expect("ready-zero exact replay"),SourceClaimOutcome::Resolved{binding:replayed,..} if replayed.same_identity(&binding))
        );
        assert!(store
            .complete_source_media_session_publication(&publication)
            .await
            .expect("old pending phase refuses")
            .is_none());
        let SourcePublicationAuthorityRead::Ready(replay) = store
            .prepare_source_publication_authority(&assignment, &key, &proof())
            .await
            .expect("published exact permission")
        else {
            panic!("published permission")
        };
        assert_eq!(
            store
                .complete_source_media_session_publication(&replay)
                .await
                .expect("exact publication replay")
                .expect("same route")
                .session_id,
            route.session_id
        );
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 1);
        let held=store.sharing_read("SELECT json_quote(reservation_state) AS payload FROM sharing_source_session_bindings",vec![]).await.expect("held capacity");
        assert_eq!(held, vec!["\"held\"".to_owned()]);
    }
}

#[tokio::test]
async fn sharing_source_assigned_no_spawn_settlement_is_atomic_and_independent_of_grant() {
    let dir = tempfile::tempdir().expect("fixture directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&dir.path().join("no-spawn.db")).expect("pooled"),
    ] {
        let (grant, key) = setup(&store).await;
        let intent = intent(&store, grant, &key, "no-spawn").await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&intent, &proof())
            .await
            .expect("claim")
        else {
            panic!("acquired")
        };
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("assignment")
            .expect("actual worker");
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("unsealed custody retains capacity"),
            SourceReleaseOutcome::Refused
        );
        seal_fixture_owned_empty_ingress(&store, &assignment).await;
        assert_eq!(
            store
                .release_source_never_dispatched(&binding)
                .await
                .expect("ordinary release"),
            SourceReleaseOutcome::Refused
        );
        let resource = format!("session:{}", binding.incarnation_id());
        let now = now_ms().expect("clock");
        store.sharing_txn(vec![("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES($1,'foreign',1,1,$2,$3)".into(),vec![resource.clone().into(),(now+60000).into(),now.into()])]).await.expect("foreign physical obligation");
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("fenced"),
            SourceReleaseOutcome::Refused
        );
        assert_eq!(count(&store, "job_leases").await, 1);
        store.sharing_txn(vec![("DELETE FROM job_leases WHERE resource=$1".into(),vec![resource.into()]),("UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),vec![]),("UPDATE sharing_exports SET state='revoked' WHERE id=$1".into(),vec![grant.into()]),("CREATE TRIGGER refuse_no_spawn_request BEFORE UPDATE OF state ON media_session_requests WHEN NEW.state='failed' BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("disabled revoked accounting fault");
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("atomic rollback"),
            SourceReleaseOutcome::Refused
        );
        let state=store.sharing_read("SELECT reservation_state AS payload FROM sharing_source_session_bindings WHERE incarnation_id=$1",vec![binding.incarnation_id().into()]).await.expect("binding");
        assert_eq!(state, ["held"]);
        store
            .sharing_txn(vec![(
                "DROP TRIGGER refuse_no_spawn_request".into(),
                vec![],
            )])
            .await
            .expect("restore writer");
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("owned SQL settlement"),
            SourceReleaseOutcome::Released
        );
        assert_eq!(
            store
                .settle_source_assigned_without_activation(&assignment)
                .await
                .expect("exact replay"),
            SourceReleaseOutcome::ExactReplay
        );
        let state = store
            .sharing_read(
                "SELECT state AS payload FROM media_session_requests WHERE incarnation_id=$1",
                vec![binding.incarnation_id().into()],
            )
            .await
            .expect("request");
        assert_eq!(state, ["failed"]);
    }
}

#[tokio::test]
async fn sharing_source_terminal_settlement_fences_physical_rows_and_rolls_back() {
    let dir = tempfile::tempdir().expect("directory");
    for published in [false, true] {
        for store in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&dir.path().join(format!("terminal-{published}.db")))
                .expect("pooled"),
        ] {
            let (grant, key) = setup(&store).await;
            let intent = intent(&store, grant, &key, "terminal-worker").await;
            let SourceClaimOutcome::Acquired(binding) = store
                .claim_source_media_session(&intent, &proof())
                .await
                .expect("claim")
            else {
                panic!("acquired")
            };
            let assignment = store
                .assign_source_dispatch(&binding, &key, &proof())
                .await
                .expect("assign")
                .expect("owned");
            let SourceWriteAuthorityRead::Ready(authority) = store
                .prepare_source_activation_authority(&assignment, &key, &proof())
                .await
                .expect("authority")
            else {
                panic!("ready")
            };
            let now = now_ms().expect("clock");
            let activation = crate::domain::MediaSessionActivation {
                incarnation_id: binding.incarnation_id().to_string(),
                session_id: Uuid::new_v4().to_string(),
                principal: binding.principal().clone(),
                playback_id: binding.playback_id().to_owned(),
                recovery_epoch: String::new(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: true,
                request_id: Some(binding.request_id().to_owned()),
                request_fingerprint: binding.request_fingerprint().to_owned(),
                owner_node_id: "voter".into(),
                recipe_json: "{}".into(),
                response_json: "{}".into(),
                publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                media_origin_ms: 0,
                now_ms: now,
                lease_expires_at_ms: now + 60000,
                expected_desired_revision: None,
            };
            let mut route = store
                .activate_source_media_session(&authority, &activation)
                .await
                .expect("activate")
                .expect("route")
                .route;
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &route)
                    .await
                    .expect("live refusal"),
                SourceReleaseOutcome::Refused
            );
            if published {
                let SourcePublicationAuthorityRead::Ready(permission) = store
                    .prepare_source_publication_authority(&assignment, &key, &proof())
                    .await
                    .expect("publication")
                else {
                    panic!("publication permission")
                };
                route = store
                    .complete_source_media_session_publication(&permission)
                    .await
                    .expect("publish")
                    .expect("published route");
            }
            store
                .sharing_txn(vec![(
                    "UPDATE settings SET value='false' WHERE key='sharing_enabled'".into(),
                    vec![],
                )])
                .await
                .expect("off");
            if !published {
                // Canonical revoke makes route and job lease zero and fails
                // the request. Published disabled cleanup keeps replay data.
                store
                    .sharing_txn(vec![(
                        "UPDATE sharing_exports SET state='revoked' WHERE id=$1".into(),
                        vec![grant.into()],
                    )])
                    .await
                    .expect("revoked unpublished start");
            }
            let terminal = store
                .end_media_session_if_owner(&crate::domain::MediaSessionEnd {
                    incarnation_id: route.incarnation_id.clone(),
                    session_id: route.session_id.clone(),
                    expected_owner_node_id: route.owner_node_id.clone(),
                    expected_owner_epoch: route.owner_epoch,
                    expected_lease_expires_at_ms: route.lease_expires_at_ms,
                    terminal_reason: if published { "deleted" } else { "revoked" }.into(),
                    now_ms: now_ms().expect("clock"),
                })
                .await
                .expect("exact terminal")
                .expect("retained terminal");
            assert_eq!(terminal.lease_expires_at_ms == 0, !published);
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &terminal)
                    .await
                    .expect("terminal metadata alone cannot discharge unsealed custody"),
                SourceReleaseOutcome::Refused
            );
            seal_fixture_owned_empty_ingress(&store, &assignment).await;
            let mut foreign = terminal.clone();
            foreign.session_id = Uuid::new_v4().to_string();
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &foreign)
                    .await
                    .expect("foreign session refuses"),
                SourceReleaseOutcome::Refused
            );
            store
                .sharing_txn(vec![(
                    "UPDATE job_leases SET owner_node_id='foreign' WHERE resource='session:'||$1"
                        .into(),
                    vec![binding.incarnation_id().into()],
                )])
                .await
                .expect("foreign lease corruption");
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &terminal)
                    .await
                    .expect("foreign lease refuses"),
                SourceReleaseOutcome::Refused
            );
            assert_eq!(count(&store, "job_leases").await, 1);
            store.sharing_txn(vec![("UPDATE job_leases SET owner_node_id='voter' WHERE resource='session:'||$1".into(),vec![binding.incarnation_id().into()]),("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign','foreign','foreign','media_session',$1,2,0)".into(),vec![binding.incarnation_id().into()])]).await.expect("foreign pin");
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &terminal)
                    .await
                    .expect("foreign pin refuses"),
                SourceReleaseOutcome::Refused
            );
            assert_eq!(count(&store, "cache_consumer_pins").await, 1);
            store.sharing_txn(vec![("DELETE FROM cache_consumer_pins WHERE storage_id='foreign'".into(),vec![]),("CREATE TRIGGER ignore_terminal_request BEFORE UPDATE OF updated_at_ms ON media_session_requests BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("accounting fault");
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &terminal)
                    .await
                    .expect("rollback"),
                SourceReleaseOutcome::Refused
            );
            assert_eq!(
                count(&store, "job_leases").await,
                1,
                "lease deletion rolls back"
            );
            let held = store
                .sharing_read(
                    "SELECT reservation_state AS payload FROM sharing_source_session_bindings",
                    vec![],
                )
                .await
                .expect("held");
            assert_eq!(held, ["held"]);
            store
                .sharing_txn(vec![(
                    "DROP TRIGGER ignore_terminal_request".into(),
                    vec![],
                )])
                .await
                .expect("restore accounting");
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &terminal)
                    .await
                    .expect("SQL accounting permission"),
                SourceReleaseOutcome::Released
            );
            assert_eq!(
                store
                    .settle_source_terminal_worker(&assignment, &terminal)
                    .await
                    .expect("exact terminal replay"),
                SourceReleaseOutcome::ExactReplay
            );
            assert_eq!(count(&store, "job_leases").await, 0);
            let request_state = store
                .sharing_read(
                    "SELECT state AS payload FROM media_session_requests WHERE incarnation_id=$1",
                    vec![binding.incarnation_id().into()],
                )
                .await
                .expect("retained request");
            assert_eq!(
                request_state,
                [if published { "resolved" } else { "failed" }]
            );

            assert_eq!(
                count(&store, "media_sessions").await,
                1,
                "canonical terminal route retained"
            );
        }
    }
}

#[tokio::test]
async fn sharing_source_index_permission_and_bounded_evidence_preserve_lineage() {
    let dir = tempfile::tempdir().expect("directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory"),
        SqliteStore::open(&dir.path().join("source-index-permission.db")).expect("pool"),
    ] {
        let (grant, key) = setup(&store).await;
        store
            .sharing_txn(vec![(
                "UPDATE files SET probe_json='{}' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("stored bounded completion data");
        let request = intent(&store, grant, &key, "index-permission").await;
        let SourceClaimOutcome::Acquired(binding) = store
            .claim_source_media_session(&request, &proof())
            .await
            .expect("claim")
        else {
            panic!("binding")
        };
        let assignment = store
            .assign_source_dispatch(&binding, &key, &proof())
            .await
            .expect("assignment")
            .expect("worker");
        let SourceWriteAuthorityRead::Ready(authority) = store
            .prepare_source_activation_authority(&assignment, &key, &proof())
            .await
            .expect("authority")
        else {
            panic!("current authority")
        };
        assert!(store
            .authorize_source_index_preparation(&authority)
            .await
            .expect("current preactivation permission"));
        assert_eq!(
            store
                .source_index_probe_evidence(&authority)
                .await
                .expect("bounded evidence"),
            Some("{}".into())
        );
        store
            .sharing_txn(vec![(
                "UPDATE files SET probe_json=$1 WHERE id=1".into(),
                vec!["x".repeat(1048577).into()],
            )])
            .await
            .expect("concurrent oversized probe");
        assert_eq!(
            store
                .source_index_probe_evidence(&authority)
                .await
                .expect("bounded SQL refusal"),
            None
        );
        assert!(!store
            .authorize_source_index_preparation(&authority)
            .await
            .expect("changed witness refusal"));
        store
            .sharing_txn(vec![(
                "UPDATE files SET probe_json='{}' WHERE id=1".into(),
                vec![],
            )])
            .await
            .expect("restore exact file evidence");
        assert!(store
            .authorize_source_index_preparation(&authority)
            .await
            .expect("exact witness"));
        let resource = format!("session:{}", binding.incarnation_id);
        store.sharing_txn(vec![("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES($1,'foreign-node',7,9,1,1)".into(),vec![resource.clone().into()])])
            .await.expect("foreign retained physical lineage");
        assert!(!store
            .authorize_source_index_preparation(&authority)
            .await
            .expect("foreign lease refuses preparation"));
        assert_eq!(count(&store, "job_leases").await, 1);
        store.sharing_txn(vec![("DELETE FROM job_leases WHERE resource=$1".into(),vec![resource.into()]),("UPDATE media_session_requests SET request_fingerprint=$1 WHERE incarnation_id=$2".into(),vec!["e".repeat(64).into(),binding.incarnation_id.into()])])
            .await.expect("request lineage corruption");
        assert!(!store
            .authorize_source_index_preparation(&authority)
            .await
            .expect("foreign fingerprint refuses"));
        assert_eq!(count(&store, "media_sessions").await, 0);
        assert_eq!(count(&store, "sharing_source_session_bindings").await, 1);
        store
            .sharing_txn(vec![(
                "DROP TABLE cluster_node_capabilities".into(),
                vec![],
            )])
            .await
            .expect("actual schema fault");
        assert!(
            store
                .authorize_source_index_preparation(&authority)
                .await
                .is_err(),
            "genuine database failure must not become an authority refusal"
        );
    }
}

#[tokio::test]
async fn sharing_source_retained_planned_intent_fences_only_exact_boot_bound_uninvoked_g0() {
    let dir = tempfile::tempdir().expect("Source g0 fixture");
    for store in [
        SqliteStore::open_in_memory().expect("memory Source"),
        SqliteStore::open(&dir.path().join("uncertain-g0.db")).expect("file Source"),
    ] {
        let (grant, credential) = setup(&store).await;
        let planned = intent(&store, grant, &credential, "uncertain-g0").await;
        assert_eq!(
            store
                .release_source_uncertain_uninvoked_claim(&planned)
                .await
                .expect("missing exact claim"),
            SourceReleaseOutcome::Refused,
            "absence cannot discharge an uncertain invocation"
        );
        let acquired = match store
            .claim_source_media_session(&planned, &proof())
            .await
            .expect("actual claim mutation")
        {
            SourceClaimOutcome::Acquired(value) => value,
            _ => panic!("fresh fixture claim"),
        };
        // These definitions exercise the exact SQL fence. Only the daemon's
        // joined actual pre-factory invocation can retain its cleanup receipt.
        let adopted = intent(&store, grant, &credential, "uncertain-g0").await;
        assert_ne!(adopted.request.incarnation_id, acquired.incarnation_id());
        assert_eq!(
            store
                .release_source_uncertain_uninvoked_claim(&adopted)
                .await
                .expect("historical nonce refusal"),
            SourceReleaseOutcome::Refused
        );
        let mut wrong_boot = planned.clone();
        wrong_boot.request.ingress_registry_boot_id = Uuid::new_v4();
        assert_eq!(
            store
                .release_source_uncertain_uninvoked_claim(&wrong_boot)
                .await
                .expect("foreign boot refusal"),
            SourceReleaseOutcome::Refused
        );
        assert_eq!(
            store
                .release_source_uncertain_uninvoked_claim(&planned)
                .await
                .expect("exact retained planned fence"),
            SourceReleaseOutcome::Released
        );
        assert_eq!(
            store
                .release_source_uncertain_uninvoked_claim(&planned)
                .await
                .expect("exact retry"),
            SourceReleaseOutcome::ExactReplay
        );

        let assigned_intent = intent(&store, grant, &credential, "historical-g1").await;
        let binding = match store
            .claim_source_media_session(&assigned_intent, &proof())
            .await
            .expect("second claim")
        {
            SourceClaimOutcome::Acquired(value) => value,
            _ => panic!("fresh assigned fixture"),
        };
        store
            .assign_source_dispatch(&binding, &credential, &proof())
            .await
            .expect("actual dispatch assignment")
            .expect("assigned");
        assert_eq!(
            store
                .release_source_uncertain_uninvoked_claim(&assigned_intent)
                .await
                .expect("g1 remains owned"),
            SourceReleaseOutcome::Refused,
            "a joined pre-factory intent cannot settle an already assigned incarnation"
        );
    }
}
