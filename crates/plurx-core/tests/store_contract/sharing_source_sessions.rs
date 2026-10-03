//! Real three-voter candidate Source reservation; no worker is dispatched.
use super::*;
use plurx_core::{
    cluster::membership::{
        observe_source_admission_members_for_contract, SourceAdmissionMembers,
        SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY, SHARING_SESSION_PRINCIPAL_CAPABILITY,
    },
    playback_principal::PlaybackPrincipal,
    secrets::CredentialKey,
    sharing::{InvitationRecord, ShareClaim, SourceId},
    sharing_catalogue_details::CatalogueRevisionKey,
    sharing_source_sessions::*,
    store::{
        sharing_catalogue_details::SourceDetailsRead,
        sharing_source_sessions::candidate_statements, SharingSourceDetailsStore,
        SharingSourceSessionStore, SharingStore, MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA,
    },
};
use uuid::Uuid;
async fn exec(client: &Client, sql: &str) {
    client
        .execute(sql.to_owned(), hiqlite::params!())
        .await
        .expect("replicated fixture mutation");
}
async fn observation(client: &Client) -> SourceAdmissionMembers {
    observe_source_admission_members_for_contract(client, 1)
        .await
        .expect("actual quorum factory")
        .expect("both floors ready")
}
async fn intent_hash(
    store: &HiqliteAuthStore,
    grant: Uuid,
    credential: &CredentialKey,
    key: &CatalogueRevisionKey,
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Source candidate fixture operation")
        .as_millis() as i64;
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
async fn intent(
    store: &HiqliteAuthStore,
    grant: Uuid,
    credential: &CredentialKey,
    key: &CatalogueRevisionKey,
    name: &str,
) -> SourceSessionIntent {
    intent_hash(store, grant, credential, key, name, "b".repeat(64)).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_source_reservations_three_voters_atomic_claim_caps_replay_and_release() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("Source candidate fixture operation");
    assert_eq!(
        client
            .metrics_db()
            .await
            .expect("Source candidate fixture operation")
            .membership_config
            .voter_ids()
            .count(),
        3
    );
    let identity = store
        .sharing_identity(1000)
        .await
        .expect("Source candidate fixture operation");
    let library = store
        .create_library(&NewLibrary {
            name: "Source".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/synthetic")],
            anime: false,
        })
        .await
        .expect("Source candidate fixture operation")
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
        .expect("Source candidate fixture operation");
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
        .expect("Source candidate fixture operation");
    store
        .approve_share(grant, 1, 1002)
        .await
        .expect("Source candidate fixture operation");
    let credential = CredentialKey::from_bytes([17; 32]);
    let envelope = CatalogueRevisionKey::generate_sealed(&credential, identity.clone())
        .expect("Source candidate fixture operation");
    let key = CatalogueRevisionKey::open(&credential, identity.clone(), &envelope)
        .expect("Source candidate fixture operation");
    let mut ddl = plurx_core::store::sharing_catalogue_source::candidate_statements();
    ddl.extend(plurx_core::store::sharing_catalogue_source::candidate_item_identity_statements());
    ddl.push(plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA.into());
    ddl.extend(
        MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
            .split("-- next statement\n")
            .map(|s| s.trim().trim_end_matches(';').to_owned()),
    );
    ddl.extend(candidate_statements());
    ddl.extend(["CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER,last_seen_at INTEGER,removed_at INTEGER,role TEXT,api_address TEXT,raft_address TEXT)","CREATE TABLE cluster_node_capabilities(node_id TEXT,capability TEXT,last_seen_at INTEGER,PRIMARY KEY(node_id,capability))","CREATE TABLE cluster_node_join_staging(node_id TEXT)","CREATE TABLE cluster_node_removals(node_id TEXT)","CREATE TABLE cluster_node_removal_attempts(node_id TEXT,attempt_id TEXT)","CREATE TABLE cluster_join_tokens(token_hash TEXT,state TEXT,node_id TEXT,raft_id INTEGER)","CREATE TABLE cluster_node_promotions(node_id TEXT,started_at INTEGER)","CREATE TABLE cluster_node_heartbeat_intents(node_id TEXT,last_seen_at INTEGER)"].into_iter().map(str::to_owned));
    for result in client
        .txn(
            ddl.into_iter()
                .map(|sql| (sql, hiqlite::params!()))
                .collect::<Vec<_>>(),
        )
        .await
        .expect("Source candidate fixture operation")
    {
        result.expect("Source candidate fixture operation");
    }
    client
        .execute(
            "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)",
            hiqlite::params!(
                identity.server_id.to_string(),
                identity.catalogue_epoch.to_string(),
                envelope.as_stored()
            ),
        )
        .await
        .expect("Source candidate fixture operation");
    client.execute("INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) VALUES(1,$1,'movie','Movie','movie',1000,1000)",hiqlite::params!(library)).await.expect("Source candidate fixture operation");
    exec(&client,"INSERT INTO files(id,item_id,path,size,mtime,scanned_at) VALUES(1,1,'/private/synthetic.mkv',20,1000,1000)").await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Source candidate fixture operation")
        .as_millis() as i64;
    for id in 1_i64..=3 {
        client
            .execute(
                "INSERT INTO cluster_nodes VALUES($1,$2,$3,NULL,NULL,'api','raft')",
                hiqlite::params!(format!("node-{id}"), id, now),
            )
            .await
            .expect("Source candidate fixture operation");
        client
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES($1,$2,$3),($1,$4,$3)",
                hiqlite::params!(
                    format!("node-{id}"),
                    SHARING_SESSION_PRINCIPAL_CAPABILITY,
                    now,
                    SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY
                ),
            )
            .await
            .expect("Source candidate fixture operation");
    }
    exec(&client,"INSERT INTO settings(key,value,updated_at) VALUES('sharing_enabled','true',1) ON CONFLICT(key) DO UPDATE SET value='true'").await;
    for result in client
        .txn(
            plurx_core::cluster::membership::sharing_member_admission_guard_schema()
                .into_iter()
                .map(|sql| (sql, hiqlite::params!()))
                .collect::<Vec<_>>(),
        )
        .await
        .expect("Source candidate fixture operation")
    {
        result.expect("Source candidate fixture operation");
    }
    let first = intent(&store, grant, &credential, &key, "first").await;
    let second = intent(&store, grant, &credential, &key, "second").await;
    let old = observation(&client).await;
    exec(
        &client,
        "UPDATE cluster_nodes SET role='learner' WHERE node_id='node-1'",
    )
    .await;
    let transition_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    client
        .execute(
            "INSERT INTO cluster_node_promotions(node_id,started_at) VALUES('node-1',$1)",
            hiqlite::params!(transition_at),
        )
        .await
        .expect("guarded compatible promotion fixture");
    client.execute("INSERT INTO cluster_sharing_membership_intents(raft_id,node_id,attempt_id,operation,api_address,raft_address,claimed_at) VALUES(1,'node-1','source-generation-case','voter','api','raft',$1)",hiqlite::params!(transition_at)).await.expect("valid production-guarded membership intent");
    // The actual committed configuration already proves node 1 is a voter.
    // Resolve the guarded SQL promotion intent, then restore the visible floor.
    assert!(client
        .metrics_db()
        .await
        .expect("actual membership outcome")
        .membership_config
        .voter_ids()
        .any(|id| id == 1));
    exec(
        &client,
        "DELETE FROM cluster_sharing_membership_intents WHERE raft_id=1",
    )
    .await;
    exec(
        &client,
        "UPDATE cluster_nodes SET role='voter' WHERE node_id='node-1'",
    )
    .await;
    exec(
        &client,
        "DELETE FROM cluster_node_promotions WHERE node_id='node-1'",
    )
    .await;
    assert!(matches!(
        store
            .claim_source_media_session(&first, &old)
            .await
            .expect("completed-transition refusal"),
        SourceClaimOutcome::Unavailable
    ));
    let rows = client
        .query_consistent_map::<SchemaText, _>(
            "SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings",
            hiqlite::params!(),
        )
        .await
        .expect("zero Source rows after stale proof");
    assert_eq!(rows[0].value, "0");
    let duplicate = intent(&store, grant, &credential, &key, "first").await;
    let proof = observation(&client).await;
    let (a, b) = tokio::join!(
        store.claim_source_media_session(&first, &proof),
        store.claim_source_media_session(&duplicate, &proof)
    );
    let binding = match (a.expect("first proposal"), b.expect("duplicate proposal")) {
        (SourceClaimOutcome::Acquired(acquired), SourceClaimOutcome::InFlight(retry))
        | (SourceClaimOutcome::InFlight(retry), SourceClaimOutcome::Acquired(acquired)) => {
            assert_eq!(
                acquired.incarnation_id(),
                retry.incarnation_id(),
                "racing exact requests share the original binding"
            );
            acquired
        }
        _ => panic!("one acquire and one exact in-flight replay"),
    };
    assert!(matches!(
        store
            .claim_source_media_session(&second, &observation(&client).await)
            .await
            .expect("second distinct request"),
        SourceClaimOutcome::Acquired(_)
    ));
    let third = intent(&store, grant, &credential, &key, "third").await;
    assert!(matches!(
        store
            .claim_source_media_session(&third, &observation(&client).await)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Capacity(SourceCapacity::PendingStarts)
    ));
    let retry = intent(&store, grant, &credential, &key, "first").await;
    match store
        .claim_source_media_session(&retry, &observation(&client).await)
        .await
        .expect("Source candidate fixture operation")
    {
        SourceClaimOutcome::InFlight(b) => assert_eq!(b.incarnation_id(), binding.incarnation_id()),
        _ => panic!("replay precedes full cap"),
    };
    let rows = client
        .query_consistent_map::<SchemaText, _>(
            "SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings",
            hiqlite::params!(),
        )
        .await
        .expect("Source candidate fixture operation");
    assert_eq!(rows[0].value, "2", "lost proposal leaves no binding");
    client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign','foreign','foreign','media_session',$1,1,1)",hiqlite::params!(binding.incarnation_id().to_string())).await.expect("Source candidate fixture operation");
    assert_eq!(
        store
            .release_source_never_dispatched(&binding)
            .await
            .expect("Source candidate fixture operation"),
        SourceReleaseOutcome::Refused,
        "even expired pin cannot prove no resource"
    );
    exec(
        &client,
        "DELETE FROM cache_consumer_pins WHERE storage_id='foreign'",
    )
    .await;
    let before = observation(&client).await;
    exec(
        &client,
        "UPDATE settings SET value='false' WHERE key='sharing_enabled'",
    )
    .await;
    assert!(matches!(
        store
            .claim_source_media_session(&third, &before)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Unavailable
    ));
    assert_eq!(
        store
            .release_source_never_dispatched(&binding)
            .await
            .expect("Source candidate fixture operation"),
        SourceReleaseOutcome::Released
    );
    assert_eq!(
        store
            .release_source_never_dispatched(&binding)
            .await
            .expect("Source candidate fixture operation"),
        SourceReleaseOutcome::ExactReplay
    );
    exec(
        &client,
        "UPDATE settings SET value='true' WHERE key='sharing_enabled'",
    )
    .await;
    assert!(matches!(
        store
            .claim_source_media_session(&retry, &observation(&client).await)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Retired(_)
    ));
    exec(
        &client,
        "UPDATE files SET path='/private/replaced.mkv' WHERE id=1",
    )
    .await;
    assert!(matches!(
        store
            .claim_source_media_session(&third, &observation(&client).await)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Unavailable
    ));
    exec(
        &client,
        "UPDATE files SET path='/private/synthetic.mkv' WHERE id=1",
    )
    .await;
    assert!(matches!(
        store
            .claim_source_media_session(&third, &observation(&client).await)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Acquired(_)
    ));
    for index in 1..=4 {
        let other = Uuid::new_v4();
        let invitation = Uuid::new_v4();
        let hash = format!("{:064x}", index + 100);
        client.execute("INSERT INTO sharing_invitations SELECT $1,$2,library_ids_json,created_at_ms,expires_at_ms,'consumed',NULL,NULL FROM sharing_invitations LIMIT 1",hiqlite::params!(invitation.to_string(),format!("{index:064x}"))).await.expect("Source candidate fixture operation");
        client.execute("INSERT INTO sharing_exports SELECT $1,$2,recipient_server_id,recipient_name,$3,scope_generation,credential_generation,catalogue_generation,mutation_generation,state,pending_expires_at_ms,created_at_ms,updated_at_ms FROM sharing_exports WHERE id=$4",hiqlite::params!(other.to_string(),invitation.to_string(),hash.clone(),grant.to_string())).await.expect("Source candidate fixture operation");
        client.execute("INSERT INTO sharing_export_libraries SELECT $1,library_id FROM sharing_export_libraries WHERE grant_id=$2",hiqlite::params!(other.to_string(),grant.to_string())).await.expect("Source candidate fixture operation");
        if index < 4 {
            let x = intent_hash(
                &store,
                other,
                &credential,
                &key,
                &format!("g-{index}-a"),
                hash.clone(),
            )
            .await;
            let y = intent_hash(
                &store,
                other,
                &credential,
                &key,
                &format!("g-{index}-b"),
                hash,
            )
            .await;
            let proof = observation(&client).await;
            let (x, y) = tokio::join!(
                store.claim_source_media_session(&x, &proof),
                store.claim_source_media_session(&y, &proof)
            );
            assert!(matches!(
                x.expect("Source candidate fixture operation"),
                SourceClaimOutcome::Acquired(_)
            ));
            assert!(matches!(
                y.expect("Source candidate fixture operation"),
                SourceClaimOutcome::Acquired(_)
            ));
        } else {
            let ninth = intent_hash(&store, other, &credential, &key, "ninth", hash).await;
            assert!(matches!(
                store
                    .claim_source_media_session(&ninth, &observation(&client).await)
                    .await
                    .expect("Source candidate fixture operation"),
                SourceClaimOutcome::Capacity(SourceCapacity::SourceSlots)
            ));
        }
    }
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("Source candidate fixture operation");
    assert_eq!(rows[0].value, "8");
    match store
        .claim_source_media_session(&second, &observation(&client).await)
        .await
        .expect("Source candidate fixture operation")
    {
        SourceClaimOutcome::InFlight(_) => {}
        _ => panic!("exact replay precedes full Source cap"),
    };
}
