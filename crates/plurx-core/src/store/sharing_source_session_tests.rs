//! Actual candidate writes through both SQLite Store modes.
use super::*;
use crate::{
    cluster::membership::{
        source_admission_members_for_unit_test, SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
        SHARING_SESSION_PRINCIPAL_CAPABILITY,
    },
    domain::{LibraryKind, NewLibrary},
    playback_principal::PlaybackPrincipal,
    secrets::CredentialKey,
    sharing::{InvitationRecord, ShareClaim, SourceId},
    sharing_catalogue_details::CatalogueRevisionKey,
    store::{LibraryStore, SharingStore, SqliteStore, MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA},
};
use std::{collections::BTreeSet, path::PathBuf};

async fn setup(store: &SqliteStore) -> (Uuid, CredentialKey) {
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
        MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
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
    store.sharing_txn(vec![("INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".into(),vec![identity.server_id.into(),identity.catalogue_epoch.into(),envelope.as_stored().to_owned().into()]),("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,$1,'movie','Movie','movie')".into(),vec![library.into()]),("INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/private/synthetic.mkv',20,1000)".into(),vec![]),("INSERT INTO cluster_nodes VALUES('voter',1,$1,NULL,NULL,'api','raft')".into(),vec![now.into()]),("INSERT INTO cluster_node_capabilities VALUES('voter',$1,$3),('voter',$2,$3)".into(),vec![SHARING_SESSION_PRINCIPAL_CAPABILITY.to_owned().into(),SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY.to_owned().into(),now.into()]),("INSERT INTO settings(key,value,updated_at) VALUES('sharing_enabled','true',1) ON CONFLICT(key) DO UPDATE SET value='true'".into(),vec![])]).await.expect("current Source fixture");
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
    let witness = match store
        .source_item_file_witness(
            &"b".repeat(64),
            grant,
            SourceId::parse("1").expect("Source candidate fixture operation"),
            SourceId::parse("1").expect("Source candidate fixture operation"),
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
                now_ms: now,
                claim_expires_at_ms: now + 60000,
                credential_hash: "b".repeat(64),
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
fn proof() -> SourceAdmissionMembers {
    source_admission_members_for_unit_test(
        1,
        &BTreeSet::from([1]),
        now_ms().expect("Source candidate fixture operation"),
    )
    .expect("Source candidate fixture operation")
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
