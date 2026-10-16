//! Actual installed three-voter Source reservation; no worker is dispatched.
use super::*;
use plurx_core::{
    cluster::membership::{observe_source_admission_members_for_contract, SourceAdmissionMembers},
    playback_principal::PlaybackPrincipal,
    secrets::CredentialKey,
    sharing::{InvitationRecord, ShareClaim, SourceId},
    sharing_catalogue_details::CatalogueRevisionKey,
    sharing_source_sessions::*,
    store::{
        sharing_catalogue_details::SourceDetailsRead,
        sharing_source_ingress_custody::SourceCustodyWrite, SharingSourceDetailsStore,
        SharingSourceIngressCustodyStore, SharingSourceSessionStore, SharingStore,
    },
};
use uuid::Uuid;
async fn exec(client: &Client, sql: &str) {
    client
        .execute(sql.to_owned(), hiqlite::params!())
        .await
        .expect("replicated fixture mutation");
}
async fn observation(
    client: &Client,
    credential: &CredentialKey,
    raft: u64,
) -> SourceAdmissionMembers {
    observe_source_admission_members_for_contract(client, raft, credential)
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
    super::sharing_receiver_sessions::with_installed_source_three_voters(|store, client, members| async move {
    let credential = members.credential;
    let local_raft = members.local_raft;
    let local_node = members.local_node;
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
    let envelope = store.source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await.expect("installed revision key").expect("actual Source installer key");
    let key = CatalogueRevisionKey::open(&credential, identity.clone(), &envelope)
        .expect("selected credential opens installed revision key");
    client.execute("INSERT INTO items(id,library_id,kind,title,sort_title,added_at,updated_at) VALUES(1,$1,'movie','Movie','movie',1000,1000)",hiqlite::params!(library)).await.expect("Source candidate fixture operation");
    exec(&client,"INSERT INTO files(id,item_id,path,size,mtime,scanned_at) VALUES(1,1,'/private/synthetic.mkv',20,1000,1000)").await;
    client.execute("INSERT INTO cluster_node_capabilities(node_id,capability,last_seen_at) SELECT node_id,$1,last_seen_at FROM cluster_nodes WHERE removed_at IS NULL ON CONFLICT(node_id,capability) DO UPDATE SET last_seen_at=excluded.last_seen_at",hiqlite::params!("sharing_ingress_boot_v1:01a1128a-944d-4f11-9bf2-c7599d355213")).await.expect("routing metadata only; no physical registry proof");
    let first = intent(&store, grant, &credential, &key, "first").await;
    let second = intent(&store, grant, &credential, &key, "second").await;
    let old = observation(&client, &credential, local_raft).await;
    client.execute("UPDATE cluster_nodes SET role='learner' WHERE node_id=$1",hiqlite::params!(local_node.clone())).await.expect("temporarily fenced real member");
    let transition_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    client
        .execute(
            "INSERT INTO cluster_node_promotions(node_id,attempt_id,started_at) VALUES($1,'source-generation-case',$2)",
            hiqlite::params!(local_node.clone(), transition_at),
        )
        .await
        .expect("guarded compatible promotion fixture");
    client.execute("INSERT INTO cluster_sharing_membership_intents(raft_id,node_id,attempt_id,operation,api_address,raft_address,claimed_at) SELECT raft_id,node_id,'source-generation-case','voter',api_address,raft_address,$1 FROM cluster_nodes WHERE node_id=$2",hiqlite::params!(transition_at, local_node.clone())).await.expect("valid production-guarded membership intent");
    // The actual committed configuration already proves this member is a voter.
    // Resolve the guarded SQL promotion intent, then restore the visible floor.
    assert!(client
        .metrics_db()
        .await
        .expect("actual membership outcome")
        .membership_config
        .voter_ids()
        .any(|id| id == local_raft));
    client.execute("DELETE FROM cluster_sharing_membership_intents WHERE raft_id=$1",hiqlite::params!(local_raft as i64)).await.expect("restore intent");
    client.execute("UPDATE cluster_nodes SET role='voter' WHERE node_id=$1",hiqlite::params!(local_node.clone())).await.expect("restore member role");
    client.execute("DELETE FROM cluster_node_promotions WHERE node_id=$1",hiqlite::params!(local_node.clone())).await.expect("restore promotion");
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
    let proof = observation(&client, &credential, local_raft).await;
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
            .claim_source_media_session(&second, &observation(&client, &credential, local_raft).await)
            .await
            .expect("second distinct request"),
        SourceClaimOutcome::Acquired(_)
    ));
    let third = intent(&store, grant, &credential, &key, "third").await;
    assert!(matches!(
        store
            .claim_source_media_session(&third, &observation(&client, &credential, local_raft).await)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Capacity(SourceCapacity::PendingStarts)
    ));
    let retry = intent(&store, grant, &credential, &key, "first").await;
    match store
        .claim_source_media_session(&retry, &observation(&client, &credential, local_raft).await)
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
    let before = observation(&client, &credential, local_raft).await;
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
            .claim_source_media_session(&retry, &observation(&client, &credential, local_raft).await)
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
            .claim_source_media_session(&third, &observation(&client, &credential, local_raft).await)
            .await
            .expect("Source candidate fixture operation"),
        SourceClaimOutcome::Unavailable
    ));
    exec(
        &client,
        "UPDATE files SET path='/private/synthetic.mkv' WHERE id=1",
    )
    .await;
    let SourceClaimOutcome::Acquired(third_binding) = store
        .claim_source_media_session(&third, &observation(&client, &credential, local_raft).await)
        .await
        .expect("third actual claim")
    else {
        panic!("acquired third")
    };
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
            let proof = observation(&client, &credential, local_raft).await;
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
                    .claim_source_media_session(&ninth, &observation(&client, &credential, local_raft).await)
                    .await
                    .expect("Source candidate fixture operation"),
                SourceClaimOutcome::Capacity(SourceCapacity::SourceSlots)
            ));
        }
    }
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("Source candidate fixture operation");
    assert_eq!(rows[0].value, "8");
    let second_binding = match store
        .claim_source_media_session(&second, &observation(&client, &credential, local_raft).await)
        .await
        .expect("Source candidate fixture operation")
    {
        SourceClaimOutcome::InFlight(binding) => binding,
        _ => panic!("exact replay precedes full Source cap"),
    };
    // Assignment consumes no additional Source slot and uses the current
    // stored credential after a benign rotation, rather than the old token.
    client
        .execute(
            "UPDATE sharing_exports SET token_hash=$1 WHERE id=$2",
            hiqlite::params!("f".repeat(64), grant.to_string()),
        )
        .await
        .expect("rotate current fixture credential");
    let local = observation(&client, &credential, local_raft).await;
    let (first_assignment, retry_assignment) = tokio::join!(
        store.assign_source_dispatch(&second_binding, &credential, &local),
        store.assign_source_dispatch(&second_binding, &credential, &local)
    );
    for assignment in [first_assignment, retry_assignment] {
        let assignment = assignment
            .expect("atomic assignment")
            .expect("same actual local worker");
        assert_eq!(assignment.owner_node_id(), local_node.as_str());
        assert_eq!(assignment.dispatch_generation(), 1);
        assert_eq!(
            assignment.binding().incarnation_id(),
            second_binding.incarnation_id()
        );
    }
    let other_worker = observe_source_admission_members_for_contract(&client, members.other_raft, &credential)
        .await
        .expect("second worker full floor")
        .expect("second actual voter");
    assert!(store
        .assign_source_dispatch(&second_binding, &credential, &other_worker)
        .await
        .expect("foreign worker refusal")
        .is_none());
    assert_eq!(
        store
            .release_source_never_dispatched(&second_binding)
            .await
            .expect("assigned release refusal"),
        SourceReleaseOutcome::Refused
    );
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("capacity retained after assignment");
    assert_eq!(rows[0].value, "8");
    let assignment = store
        .assign_source_dispatch(
            &second_binding,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("activation assignment")
        .expect("same owned worker");
    let SourceWriteAuthorityRead::Ready(authority) = store
        .prepare_source_activation_authority(
            &assignment,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("fresh activation witness")
    else {
        panic!("current activation authority")
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    let mut activation = MediaSessionActivation {
        incarnation_id: second_binding.incarnation_id().to_string(),
        session_id: Uuid::new_v4().to_string(),
        principal: second_binding.principal().clone(),
        playback_id: "p-second".into(),
        recovery_epoch: String::new(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: true,
        request_id: Some("second".into()),
        request_fingerprint: "d".repeat(64),
        owner_node_id: local_node.as_str().into(),
        recipe_json: "{}".into(),
        response_json: "{}".into(),
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 60000,
        expected_desired_revision: None,
    };
    let foreign_lease = format!("session:{}", second_binding.incarnation_id());
    client.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES($1,'foreign-worker',1,1,$2,$3)",hiqlite::params!(foreign_lease.clone(),now+90000,now)).await.expect("foreign retained lease");
    client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign','foreign','foreign','media_session',$1,1,1)",hiqlite::params!(second_binding.incarnation_id().to_string())).await.expect("foreign retained pin");
    assert!(store
        .activate_source_media_session(&authority, &activation)
        .await
        .expect("foreign resources refuse activation")
        .is_none());
    assert!(matches!(
        store
            .prepare_source_activation_authority(
                &assignment,
                &credential,
                &observation(&client, &credential, local_raft).await
            )
            .await
            .expect("foreign actor lineage refuses mint"),
        SourceWriteAuthorityRead::Unavailable
    ));
    let rows = client
        .query_consistent_map::<SchemaText, _>(
            "SELECT CAST(expires_at_ms AS TEXT) AS value FROM job_leases WHERE resource=$1",
            hiqlite::params!(foreign_lease.clone()),
        )
        .await
        .expect("foreign lease preserved");
    assert_eq!(rows[0].value, (now + 90000).to_string());
    let rows = client
        .query_consistent_map::<SchemaText, _>(
            "SELECT CAST(count(*) AS TEXT) AS value FROM cache_consumer_pins WHERE consumer_id=$1",
            hiqlite::params!(second_binding.incarnation_id().to_string()),
        )
        .await
        .expect("foreign pin preserved");
    assert_eq!(rows[0].value, "1");
    client
        .execute(
            "DELETE FROM job_leases WHERE resource=$1",
            hiqlite::params!(foreign_lease),
        )
        .await
        .expect("remove only injected fixture lease");
    client
        .execute(
            "DELETE FROM cache_consumer_pins WHERE consumer_id=$1",
            hiqlite::params!(second_binding.incarnation_id().to_string()),
        )
        .await
        .expect("remove only injected fixture pin");
    let (reached, release) =
        HiqliteAuthStore::validation_pause_next_activation_after_pointer_read();
    let refused = {
        let pending = store.activate_source_media_session(&authority, &activation);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("activation completed before race: {}",result.is_ok()),
            result = reached => result.expect("actual pointer read pause"),
        }
        exec(
            &client,
            "UPDATE settings SET value='false' WHERE key='sharing_enabled'",
        )
        .await;
        release.send(()).expect("release activation race");
        pending.await.expect("typed authority refusal")
    };
    assert!(refused.is_none());
    for table in ["media_sessions", "media_playback_pointers", "job_leases"] {
        let rows = client
            .query_consistent_map::<SchemaText, _>(
                format!("SELECT CAST(count(*) AS TEXT) AS value FROM {table}"),
                hiqlite::params!(),
            )
            .await
            .expect("rollback resource census");
        assert_eq!(rows[0].value, "0", "no authority extension in {table}");
    }
    exec(
        &client,
        "UPDATE settings SET value='true' WHERE key='sharing_enabled'",
    )
    .await;
    let SourceWriteAuthorityRead::Ready(authority) = store
        .prepare_source_activation_authority(
            &assignment,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("fresh retry authority")
    else {
        panic!("retry authority")
    };
    activation.now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    activation.lease_expires_at_ms = activation.now_ms + 60000;
    let mut foreign = activation.clone();
    foreign.principal =
        PlaybackPrincipal::sharing(grant, &"e".repeat(64)).expect("different viewer");
    assert!(store
        .activate_source_media_session(&authority, &foreign)
        .await
        .expect("foreign proof refusal")
        .is_none());
    let route = store
        .activate_source_media_session(&authority, &activation)
        .await
        .expect("guarded first Source activation")
        .expect("first Source start")
        .route;
    assert_eq!(route.principal, activation.principal);
    assert_eq!(route.session_id, activation.session_id);
    assert_eq!(route.owner_node_id, local_node.as_str());
    assert_eq!(
        route.publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    let replay = store
        .activate_source_media_session(&authority, &activation)
        .await
        .expect("exact activation replay")
        .expect("same retained route");
    assert_eq!(replay.route.session_id, route.session_id);
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("physical obligations retained");
    assert_eq!(rows[0].value, "8");
    assert!(matches!(
        store
            .prepare_source_owned_route_authority(
                &assignment,
                &credential,
                &observation(&client, &credential, local_raft).await
            )
            .await
            .expect("unresolved owned refusal"),
        SourceOwnedRouteAuthorityRead::Unavailable
    ));
    // Actual coupled Store publication; this candidate SQL receipt does not
    // allocate a producer or qualify physical readiness.
    let SourcePublicationAuthorityRead::Ready(publication) = store
        .prepare_source_publication_authority(
            &assignment,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("actual voter publication permission")
    else {
        panic!("pending permission")
    };
    exec(
        &client,
        "UPDATE settings SET value='false' WHERE key='sharing_enabled'",
    )
    .await;
    assert!(store
        .complete_source_media_session_publication(&publication)
        .await
        .expect("same-write off refusal")
        .is_none());
    exec(
        &client,
        "UPDATE settings SET value='true' WHERE key='sharing_enabled'",
    )
    .await;
    exec(&client,"CREATE TRIGGER source_publication_ignore BEFORE UPDATE OF start_resolved_at_ms ON sharing_source_session_bindings BEGIN SELECT RAISE(IGNORE); END").await;
    assert!(store
        .complete_source_media_session_publication(&publication)
        .await
        .expect("postcondition rollback")
        .is_none());
    assert_eq!(
        store
            .media_session_route_by_incarnation(&activation.incarnation_id)
            .await
            .expect("rollback route")
            .expect("route")
            .publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    exec(&client, "DROP TRIGGER source_publication_ignore").await;
    let published = store
        .complete_source_media_session_publication(&publication)
        .await
        .expect("coupled publication")
        .expect("published route");
    assert_eq!(published.publication_ready_at_ms, 0);
    assert_eq!(published.session_id, activation.session_id);
    assert!(matches!(
        store
            .claim_source_media_session(&second, &observation(&client, &credential, local_raft).await)
            .await
            .expect("old rotated peer credential refuses"),
        SourceClaimOutcome::Unavailable
    ));
    let current_replay =
        intent_hash(&store, grant, &credential, &key, "second", "f".repeat(64)).await;
    assert!(
        matches!(store.claim_source_media_session(&current_replay,&observation(&client, &credential, local_raft).await).await.expect("ready-zero replay before full Source cap"),SourceClaimOutcome::Resolved{binding,..} if binding.same_identity(&second_binding))
    );
    let SourcePublicationAuthorityRead::Ready(publication_replay) = store
        .prepare_source_publication_authority(
            &assignment,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("exact publication permission")
    else {
        panic!("published permission")
    };
    assert_eq!(
        store
            .complete_source_media_session_publication(&publication_replay)
            .await
            .expect("exact published replay")
            .expect("same route")
            .session_id,
        published.session_id
    );
    let SourceOwnedRouteAuthorityRead::Ready(owned) = store
        .prepare_source_owned_route_authority(
            &assignment,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("actual voter owned witness")
    else {
        panic!("resolved owned witness")
    };
    let renewal = MediaSessionRenewal {
        incarnation_id: activation.incarnation_id.clone(),
        owner_epoch: 1,
        produced_playable_through_ms: 1000,
        fetched_through_ms: 500,
        media_sequence: 1,
    };
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64;
    let expiry = route.lease_expires_at_ms + 60000;
    assert!(store
        .renew_media_sessions(local_node.as_str(), std::slice::from_ref(&renewal), at, expiry)
        .await
        .expect("ordinary Shared refuses")
        .is_empty());
    exec(
        &client,
        "UPDATE settings SET value='false' WHERE key='sharing_enabled'",
    )
    .await;
    assert!(store
        .renew_source_media_session(&owned, &renewal, at, expiry)
        .await
        .expect("same-write off race")
        .is_none());
    assert_eq!(
        store
            .media_session_route_by_incarnation(&activation.incarnation_id)
            .await
            .expect("route")
            .expect("retained")
            .lease_expires_at_ms,
        route.lease_expires_at_ms
    );
    exec(
        &client,
        "UPDATE settings SET value='true' WHERE key='sharing_enabled'",
    )
    .await;
    let renewed = store
        .renew_source_media_session(&owned, &renewal, at, expiry)
        .await
        .expect("actual voter guarded renewal")
        .expect("same route");
    assert_eq!(renewed.lease_expires_at_ms, expiry);
    assert_eq!(renewed.produced_playable_through_ms, 1000);
    assert!(store
        .renew_source_media_session(&owned, &renewal, at, expiry + 60000)
        .await
        .expect("stale revision refused")
        .is_none());
    assert!(matches!(
        store
            .prepare_source_owned_route_authority(
                &assignment,
                &credential,
                &observation(&client, &credential, local_raft).await
            )
            .await
            .expect("refreshed owned witness"),
        SourceOwnedRouteAuthorityRead::Ready(_)
    ));
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("renewal retains physical obligations");
    assert_eq!(rows[0].value, "8");
    assert_eq!(
        store
            .settle_source_assigned_without_activation(&assignment)
            .await
            .expect("live route cannot settle"),
        SourceReleaseOutcome::Refused
    );
    let no_spawn = store
        .assign_source_dispatch(
            &third_binding,
            &credential,
            &observation(&client, &credential, local_raft).await,
        )
        .await
        .expect("actual assigned no-spawn worker")
        .expect("assigned");
    exec(
        &client,
        "UPDATE settings SET value='false' WHERE key='sharing_enabled'",
    )
    .await;
    let unsealed = store.source_ingress_custody(&no_spawn).await.expect("no-spawn custody").expect("claim retained custody");
    assert!(!unsealed.state.is_sealed());
    assert_eq!(unsealed.state.open().count(), 0);
    assert_eq!(store.settle_source_assigned_without_activation(&no_spawn).await.expect("unsealed no-spawn refuses"), SourceReleaseOutcome::Refused);
    assert_eq!(store.seal_source_ingress_custody(&no_spawn).await.expect("seal empty no-spawn custody"), SourceCustodyWrite::Applied);
    let sealed = store.source_ingress_custody(&no_spawn).await.expect("sealed no-spawn custody").expect("retained custody");
    assert!(sealed.state.is_sealed());
    assert_eq!(sealed.state.open().count(), 0, "no physical ingress registered");
    exec(&client,"CREATE TRIGGER source_no_spawn_ignore BEFORE UPDATE OF state ON media_session_requests WHEN NEW.state='failed' BEGIN SELECT RAISE(IGNORE); END").await;
    assert_eq!(
        store
            .settle_source_assigned_without_activation(&no_spawn)
            .await
            .expect("rollback missing request settlement"),
        SourceReleaseOutcome::Refused
    );
    exec(&client, "DROP TRIGGER source_no_spawn_ignore").await;
    assert_eq!(
        store
            .settle_source_assigned_without_activation(&no_spawn)
            .await
            .expect("owned SQL no-spawn accounting"),
        SourceReleaseOutcome::Released
    );
    assert_eq!(
        store
            .settle_source_assigned_without_activation(&no_spawn)
            .await
            .expect("exact no-spawn replay"),
        SourceReleaseOutcome::ExactReplay
    );
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("only settled obligation released");
    assert_eq!(rows[0].value, "7");
    assert_eq!(
        store
            .settle_source_terminal_worker(&assignment, &renewed)
            .await
            .expect("active worker SQL cannot settle"),
        SourceReleaseOutcome::Refused
    );
    let terminal = store
        .end_media_session_if_owner(&plurx_core::domain::MediaSessionEnd {
            incarnation_id: renewed.incarnation_id.clone(),
            session_id: renewed.session_id.clone(),
            expected_owner_node_id: renewed.owner_node_id.clone(),
            expected_owner_epoch: renewed.owner_epoch,
            expected_lease_expires_at_ms: renewed.lease_expires_at_ms,
            terminal_reason: "deleted".into(),
            now_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as i64,
        })
        .await
        .expect("exact existing lifecycle End")
        .expect("retained ended route");
    client
        .execute(
            "UPDATE sharing_exports SET state='revoked' WHERE id=$1",
            hiqlite::params!(grant.to_string()),
        )
        .await
        .expect("revoked cleanup");
    let unsealed = store.source_ingress_custody(&assignment).await.expect("terminal custody").expect("actual assigned custody");
    assert!(!unsealed.state.is_sealed());
    assert_eq!(unsealed.state.open().count(), 0);
    assert_eq!(store.settle_source_terminal_worker(&assignment, &terminal).await.expect("unsealed terminal refuses"), SourceReleaseOutcome::Refused);
    assert_eq!(store.seal_source_ingress_custody(&assignment).await.expect("seal empty terminal custody"), SourceCustodyWrite::Applied);
    let sealed = store.source_ingress_custody(&assignment).await.expect("sealed terminal custody").expect("retained custody");
    assert!(sealed.state.is_sealed());
    assert_eq!(sealed.state.open().count(), 0, "no physical ingress registered");
    client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('terminal-foreign','foreign','foreign','media_session',$1,2,0)",hiqlite::params!(terminal.incarnation_id.clone())).await.expect("foreign terminal pin");
    assert_eq!(
        store
            .settle_source_terminal_worker(&assignment, &terminal)
            .await
            .expect("foreign pin preserved"),
        SourceReleaseOutcome::Refused
    );
    exec(
        &client,
        "DELETE FROM cache_consumer_pins WHERE storage_id='terminal-foreign'",
    )
    .await;
    exec(&client,"CREATE TRIGGER source_terminal_ignore BEFORE UPDATE OF updated_at_ms ON media_session_requests BEGIN SELECT RAISE(IGNORE); END").await;
    assert_eq!(
        store
            .settle_source_terminal_worker(&assignment, &terminal)
            .await
            .expect("terminal accounting rollback"),
        SourceReleaseOutcome::Refused
    );
    let rows = client
        .query_consistent_map::<SchemaText, _>(
            "SELECT CAST(count(*) AS TEXT) AS value FROM job_leases WHERE resource='session:'||$1",
            hiqlite::params!(terminal.incarnation_id.clone()),
        )
        .await
        .expect("lease removal rolled back");
    assert_eq!(rows[0].value, "1");
    exec(&client, "DROP TRIGGER source_terminal_ignore").await;
    assert_eq!(
        store
            .settle_source_terminal_worker(&assignment, &terminal)
            .await
            .expect("owned SQL terminal accounting"),
        SourceReleaseOutcome::Released
    );
    assert_eq!(
        store
            .settle_source_terminal_worker(&assignment, &terminal)
            .await
            .expect("exact terminal accounting replay"),
        SourceReleaseOutcome::ExactReplay
    );
    let rows=client.query_consistent_map::<SchemaText,_>("SELECT CAST(count(*) AS TEXT) AS value FROM sharing_source_session_bindings WHERE reservation_state='held'",hiqlite::params!()).await.expect("other grants retained");
    assert_eq!(rows[0].value, "6");
    }).await;
}
