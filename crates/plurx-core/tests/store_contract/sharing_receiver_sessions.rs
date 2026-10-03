//! Actual three-voter B route/upstream admission; no Source worker is started.
use super::*;
use plurx_core::{
    playback_principal::PlaybackPrincipal,
    sharing::SourceId,
    sharing_catalogue::SharedReference,
    sharing_catalogue_details::FileRevision,
    sharing_receiver_sessions::{ReceiverProducerKind, ReceiverSessionIntent, RemoteSourceRecipe},
    store::{
        sharing_catalogue::ReceiverCatalogueScope, SharingReceiverSessionStore,
        MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA,
    },
};
use sha2::Digest;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_receiver_three_voters_atomic_admission_replay_scope_and_unresolved_retention() {
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
    .expect("actual voter observer");
    assert_eq!(
        client
            .metrics_db()
            .await
            .expect("membership")
            .membership_config
            .voter_ids()
            .count(),
        3
    );
    for statement in MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA.split("-- next statement\n") {
        client
            .execute(statement.trim().trim_end_matches(';'), hiqlite::params!())
            .await
            .expect("frozen principal fixture before first session query");
    }
    client
        .execute(
            "INSERT INTO settings(key,value,updated_at) VALUES('sharing_enabled','true',1000)",
            hiqlite::params!(),
        )
        .await
        .expect("saved choice");
    for refusal in ["none", "assignment", "login", "policy", "epoch"] {
        let user = store
            .create_user(&format!("B-{refusal}"), "fixture-password-hash", false)
            .await
            .expect("B user");
        let hash = format!("{:x}", sha2::Sha256::digest(refusal.as_bytes()));
        store
            .create_token(&hash, user.id, None)
            .await
            .expect("B login");
        let scope = ReceiverCatalogueScope {
            import_id: Uuid::new_v4(),
            source_server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            lifecycle_generation: 1,
            assignment_generation: 1,
            endpoint_generation: 1,
            claim_id: Uuid::new_v4(),
            remote_grant_id: Uuid::new_v4(),
            libraries: vec![SourceId::parse("0").expect("Source zero")],
        };
        client
            .execute(
                "INSERT INTO sharing_viewers VALUES($1,$2)",
                hiqlite::params!(user.id, Uuid::new_v4().to_string()),
            )
            .await
            .expect("viewer");
        client.execute("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,remote_grant_id,credential_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,state,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'Source',$4,$5,'fixture not opened','[]',1,1,1,'active',1000,1000)",hiqlite::params!(scope.import_id.to_string(),scope.source_server_id.to_string(),scope.catalogue_epoch.to_string(),scope.claim_id.to_string(),scope.remote_grant_id.to_string())).await.expect("import");
        client
            .execute(
                "INSERT INTO sharing_assignments VALUES($1,'0',$2,1)",
                hiqlite::params!(scope.import_id.to_string(), user.id),
            )
            .await
            .expect("assignment");
        let recipe = RemoteSourceRecipe {
            kind: ReceiverProducerKind::RemoteSource,
            version: 1,
            reference: SharedReference {
                import_id: scope.import_id,
                server_id: scope.source_server_id,
                catalogue_epoch: scope.catalogue_epoch,
                library_id: scope.libraries[0].clone(),
                item_id: SourceId::parse("9223372036854775807").expect("lossless item"),
            },
            lifecycle_generation: 1,
            file_id: SourceId::parse("0").expect("Source file zero"),
            file_revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
            source_request_id: Uuid::new_v4(),
            parent_login_hash: hash.clone(),
            request_json: "{\"caps_v2\":{}}".into(),
        };
        let intent = ReceiverSessionIntent {
            scope: scope.clone(),
            user_id: user.id,
            login_hash: hash.clone(),
            recipe: recipe.clone(),
            source_position_ms: 1234,
        };
        let other_hash = format!(
            "{:x}",
            sha2::Sha256::digest(format!("other-{refusal}").as_bytes())
        );
        store
            .create_token(&other_hash, user.id, None)
            .await
            .expect("other valid login");
        let mut other = intent.clone();
        other.login_hash = other_hash;
        assert!(
            store
                .prepare_receiver_session_authority(other)
                .await
                .is_err(),
            "same user cannot adopt parent login"
        );
        let authority = store
            .prepare_receiver_session_authority(intent.clone())
            .await
            .expect("proof")
            .expect("current proof");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        let activation = MediaSessionActivation {
            incarnation_id: recipe.source_request_id.to_string(),
            session_id: Uuid::new_v4().to_string(),
            principal: PlaybackPrincipal::LocalUser { user_id: user.id },
            playback_id: "B-playback".into(),
            recovery_epoch: Uuid::new_v4().to_string(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some("B-request".into()),
            request_fingerprint: recipe.request_fingerprint().expect("complete fingerprint"),
            owner_node_id: "B-node".into(),
            recipe_json: serde_json::to_string(&recipe).expect("recipe"),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 1234,
            now_ms: now,
            lease_expires_at_ms: now + 30_000,
            expected_desired_revision: None,
        };
        assert!(matches!(
            store
                .claim_media_session_request(
                    &activation.principal,
                    "B-request",
                    &activation.request_fingerprint,
                    "B-playback",
                    &activation.incarnation_id,
                    now,
                    now + 30_000
                )
                .await
                .expect("claim"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(store
            .assign_media_session_request_owner(
                &activation.principal,
                "B-request",
                &activation.incarnation_id,
                "B-node",
                now
            )
            .await
            .expect("owner"));
        assert!(store.activate_media_session(&activation).await.is_err());
        match refusal {
            "assignment" => {
                client
                    .execute(
                        "UPDATE sharing_assignments SET enabled=0 WHERE import_id=$1",
                        hiqlite::params!(scope.import_id.to_string()),
                    )
                    .await
                    .expect("lost assignment");
            }
            "login" => {
                client
                    .execute(
                        "DELETE FROM tokens WHERE user_id=$1",
                        hiqlite::params!(user.id),
                    )
                    .await
                    .expect("revoked login");
            }
            "policy" => {
                client
                    .execute(
                        "INSERT INTO settings(key,value,updated_at) VALUES('auth.token_idle_days','1',1000)",
                        hiqlite::params!(),
                    )
                    .await
                    .expect("changed policy");
            }
            "epoch" => {
                client
                    .execute(
                        "UPDATE sharing_imports SET catalogue_epoch=$1 WHERE id=$2",
                        hiqlite::params!(Uuid::new_v4().to_string(), scope.import_id.to_string()),
                    )
                    .await
                    .expect("changed Source identity");
            }
            _ => {}
        }
        if refusal != "none" {
            client.execute("CREATE TRIGGER receiver_ignored_old_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END",hiqlite::params!()).await.expect("ignored old assertion fixture");
        }
        let outcome = store
            .activate_receiver_media_session(&authority, &activation)
            .await
            .expect("atomic B activation");
        if refusal != "none" {
            client
                .execute(
                    "DROP TRIGGER receiver_ignored_old_assertion",
                    hiqlite::params!(),
                )
                .await
                .expect("remove old assertion fixture");
        }
        assert_eq!(outcome.is_some(), refusal == "none", "{refusal}");
        if refusal == "none" {
            assert!(store
                .activate_receiver_media_session(&authority, &activation)
                .await
                .expect("exact replay")
                .is_some());
            let route = store
                .media_session_route_by_incarnation(&activation.incarnation_id)
                .await
                .expect("route")
                .expect("blocked owner");
            assert!(store
                .complete_media_session_handoff(
                    &activation.incarnation_id,
                    &activation.owner_node_id,
                    route.owner_epoch,
                    plurx_core::domain::MediaSessionProjectionCompletion::PredecessorAcknowledged,
                    now
                )
                .await
                .expect("ordinary publication refusal")
                .is_none());
            let renewal_now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as i64;
            let renewal = plurx_core::sharing_receiver_sessions::ReceiverPendingRenewal {
                incarnation_id: activation.incarnation_id.clone(),
                owner_node_id: activation.owner_node_id.clone(),
                owner_epoch: route.owner_epoch,
                request_id: "B-request".into(),
                now_ms: renewal_now,
                lease_expires_at_ms: renewal_now + 30_000,
            };
            let fresh = store
                .prepare_receiver_session_authority(intent.clone())
                .await
                .expect("fresh proof")
                .expect("original login");
            assert!(store
                .renew_pending_receiver_session(&fresh, &renewal)
                .await
                .expect("actual replicated atomic pending renewal"));
            assert_eq!(
                store
                    .media_session_route_by_incarnation(&activation.incarnation_id)
                    .await
                    .expect("renewed route")
                    .expect("route")
                    .lease_expires_at_ms,
                renewal.lease_expires_at_ms
            );
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=0 WHERE import_id=$1",
                    hiqlite::params!(scope.import_id.to_string()),
                )
                .await
                .expect("renewal race");
            let before: SchemaText = client
                .query_consistent_map(
                    "SELECT CAST(revision AS TEXT) AS value FROM job_leases WHERE resource=$1",
                    hiqlite::params!(format!("session:{}", activation.incarnation_id)),
                )
                .await
                .expect("lease before refused renewal")
                .pop()
                .expect("lease");
            client.execute("CREATE TRIGGER receiver_ignored_pending_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END",hiqlite::params!()).await.expect("ignored pending assertion fixture");
            assert!(!store
                .renew_pending_receiver_session(&fresh, &renewal)
                .await
                .expect("lost assignment refuses renewal"));
            client
                .execute(
                    "DROP TRIGGER receiver_ignored_pending_assertion",
                    hiqlite::params!(),
                )
                .await
                .expect("remove pending assertion fixture");
            let after: SchemaText = client
                .query_consistent_map(
                    "SELECT CAST(revision AS TEXT) AS value FROM job_leases WHERE resource=$1",
                    hiqlite::params!(format!("session:{}", activation.incarnation_id)),
                )
                .await
                .expect("lease after refused renewal")
                .pop()
                .expect("lease");
            assert_eq!(
                before.value, after.value,
                "guard refusal rolls back all replicated writes"
            );
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=1 WHERE import_id=$1",
                    hiqlite::params!(scope.import_id.to_string()),
                )
                .await
                .expect("restore fixture");

            use plurx_core::sharing_receiver_sessions::{
                ReceiverSourceAttachment, ReceiverSourceBinding, ReceiverSourceOwner,
                ReceiverSourcePublication, ReceiverSourceRenewal, ReceiverSourceWrite,
            };
            let key = plurx_core::secrets::CredentialKey::from_bytes([41; 32]);
            let envelope = key
                .seal_sharing(
                    plurx_core::secrets::SharingSecretPurpose::Upstream,
                    Uuid::new_v4(),
                    scope.import_id,
                    "actual contract upstream capability",
                )
                .expect("sealed actor result fixture");
            let mut attachment = ReceiverSourceAttachment {
                owner: ReceiverSourceOwner {
                    incarnation_id: recipe.source_request_id,
                    session_id: Uuid::parse_str(&activation.session_id).expect("B UUID"),
                    owner_node_id: activation.owner_node_id.clone(),
                    owner_epoch: route.owner_epoch,
                    request_id: "B-request".into(),
                    lease_expires_at_ms: renewal.lease_expires_at_ms,
                    now_ms: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("clock")
                        .as_millis() as i64,
                },
                binding: ReceiverSourceBinding {
                    reference: recipe.reference.clone(),
                    file_id: recipe.file_id.clone(),
                    file_revision: recipe.file_revision.clone(),
                    source_request_id: recipe.source_request_id,
                    source_session_id: Uuid::new_v4(),
                    source_incarnation_id: Uuid::new_v4(),
                    capability_envelope: envelope,
                },
            };
            let fresh = store
                .prepare_receiver_session_authority(intent.clone())
                .await
                .expect("current binding proof")
                .expect("original login");
            assert!(store
                .receiver_source_binding(&fresh, &attachment.owner)
                .await
                .expect("unbound reader")
                .is_none());
            let census="SELECT json_array((SELECT json_group_array(json_array(owner_node_id,owner_epoch,publication_ready_at_ms,lease_expires_at_ms,response_json,updated_at_ms)) FROM media_sessions),(SELECT json_group_array(json_array(state,claim_expires_at_ms,response_json,updated_at_ms)) FROM media_session_requests),(SELECT json_group_array(json_array(owner_node_id,fence,revision,expires_at_ms,updated_at_ms)) FROM job_leases),(SELECT json_group_array(json_array(source_session_id,source_incarnation_id,capability_envelope)) FROM sharing_relay_upstream)) AS value";
            let read = || client.query_consistent_map::<SchemaText, _>(census, hiqlite::params!());
            for (change, restore) in [
                (
                    "UPDATE sharing_assignments SET enabled=0",
                    "UPDATE sharing_assignments SET enabled=1",
                ),
                (
                    "UPDATE media_sessions SET owner_epoch=owner_epoch+1",
                    "UPDATE media_sessions SET owner_epoch=owner_epoch-1",
                ),
                (
                    "UPDATE sharing_relay_upstream SET source_incarnation_id='partial'",
                    "UPDATE sharing_relay_upstream SET source_incarnation_id=NULL",
                ),
            ] {
                client
                    .execute(change, hiqlite::params!())
                    .await
                    .expect("binding race");
                let before = read()
                    .await
                    .expect("before refusal")
                    .pop()
                    .expect("census")
                    .value;
                assert_eq!(
                    store
                        .attach_receiver_source(&fresh, &attachment)
                        .await
                        .expect("same-write refusal"),
                    ReceiverSourceWrite::Refused,
                    "{change}"
                );
                assert_eq!(
                    read()
                        .await
                        .expect("after refusal")
                        .pop()
                        .expect("census")
                        .value,
                    before
                );
                client
                    .execute(restore, hiqlite::params!())
                    .await
                    .expect("restore fixture");
            }
            client.execute("CREATE TRIGGER receiver_ignored_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END",hiqlite::params!()).await.expect("actual ignored assertion fixture");
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=0",
                    hiqlite::params!(),
                )
                .await
                .expect("revoked original assignment");
            let before = read()
                .await
                .expect("before ignored assertion")
                .pop()
                .expect("row")
                .value;
            assert_eq!(
                store
                    .attach_receiver_source(&fresh, &attachment)
                    .await
                    .expect("actual pre-trigger assertion"),
                ReceiverSourceWrite::Refused
            );
            assert_eq!(
                read()
                    .await
                    .expect("refusal atomic")
                    .pop()
                    .expect("row")
                    .value,
                before
            );
            client
                .execute(
                    "DROP TRIGGER receiver_ignored_assertion",
                    hiqlite::params!(),
                )
                .await
                .expect("remove fixture trigger");
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=1",
                    hiqlite::params!(),
                )
                .await
                .expect("restore fixture");
            assert_eq!(
                store
                    .attach_receiver_source(&fresh, &attachment)
                    .await
                    .expect("actual attached commit"),
                ReceiverSourceWrite::Applied
            );
            let snapshot = store
                .receiver_source_binding(&fresh, &attachment.owner)
                .await
                .expect("actual reader")
                .expect("retained binding");
            assert_eq!(
                snapshot.binding.source_incarnation_id,
                attachment.binding.source_incarnation_id
            );
            assert_eq!(
                snapshot
                    .binding
                    .capability_envelope
                    .to_persist()
                    .expect("envelope"),
                attachment
                    .binding
                    .capability_envelope
                    .to_persist()
                    .expect("envelope")
            );
            assert!(snapshot.response_json.is_none());
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=0",
                    hiqlite::params!(),
                )
                .await
                .expect("reader scope loss");
            assert!(store
                .receiver_source_binding(&fresh, &attachment.owner)
                .await
                .expect("scope refusal")
                .is_none());
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=1",
                    hiqlite::params!(),
                )
                .await
                .expect("restore scope");
            let before = read()
                .await
                .expect("attached census")
                .pop()
                .expect("row")
                .value;
            assert_eq!(
                store
                    .attach_receiver_source(&fresh, &attachment)
                    .await
                    .expect("exact attachment replay"),
                ReceiverSourceWrite::Replay
            );
            assert_eq!(
                read()
                    .await
                    .expect("unchanged replay")
                    .pop()
                    .expect("row")
                    .value,
                before
            );
            let mut wrong = attachment.clone();
            wrong.binding.source_session_id = Uuid::new_v4();
            assert_eq!(
                store
                    .attach_receiver_source(&fresh, &wrong)
                    .await
                    .expect("no Source rebind"),
                ReceiverSourceWrite::Refused
            );
            let target = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as i64
                + 30_000;
            let bound_renewal = ReceiverSourceRenewal {
                attachment: attachment.clone(),
                lease_expires_at_ms: target,
            };
            assert_eq!(
                store
                    .renew_receiver_source_session(&fresh, &bound_renewal)
                    .await
                    .expect("attached blocked renewal"),
                ReceiverSourceWrite::Applied
            );
            assert_eq!(
                store
                    .renew_receiver_source_session(&fresh, &bound_renewal)
                    .await
                    .expect("renewal replay"),
                ReceiverSourceWrite::Replay
            );
            attachment.owner.lease_expires_at_ms = target;
            let publication=ReceiverSourcePublication {attachment:attachment.clone(),response_json:serde_json::json!({"session_id":attachment.owner.session_id.to_string(),"file_id":"0"}).to_string()};
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=0",
                    hiqlite::params!(),
                )
                .await
                .expect("publication revocation");
            let before = read()
                .await
                .expect("before publication refusal")
                .pop()
                .expect("row")
                .value;
            assert_eq!(
                store
                    .publish_receiver_source(&fresh, &publication)
                    .await
                    .expect("publication revoked"),
                ReceiverSourceWrite::Refused
            );
            assert_eq!(
                read()
                    .await
                    .expect("atomic publication refusal")
                    .pop()
                    .expect("row")
                    .value,
                before
            );
            client
                .execute(
                    "UPDATE sharing_assignments SET enabled=1",
                    hiqlite::params!(),
                )
                .await
                .expect("restore fixture");
            assert_eq!(
                store
                    .publish_receiver_source(&fresh, &publication)
                    .await
                    .expect("actual publication"),
                ReceiverSourceWrite::Applied
            );
            let snapshot = store
                .receiver_source_binding(&fresh, &attachment.owner)
                .await
                .expect("published reader")
                .expect("retained publication");
            assert_eq!(
                snapshot.response_json.as_deref(),
                Some(publication.response_json.as_str())
            );
            assert!(store
                .owned_media_sessions(&attachment.owner.owner_node_id, attachment.owner.now_ms)
                .await
                .expect("Local inventory excludes B")
                .is_empty());
            let before = read()
                .await
                .expect("published census")
                .pop()
                .expect("row")
                .value;
            assert_eq!(
                store
                    .publish_receiver_source(&fresh, &publication)
                    .await
                    .expect("exact publication replay"),
                ReceiverSourceWrite::Replay
            );
            assert_eq!(
                read()
                    .await
                    .expect("read-only publication replay")
                    .pop()
                    .expect("row")
                    .value,
                before
            );
            assert!(store
                .publish_media_session_activation(
                    &activation.principal,
                    "B-request",
                    &activation.incarnation_id,
                    attachment.owner.now_ms
                )
                .await
                .expect("generic publication excludes remote")
                .is_none());
            let request_before:SchemaText=client.query_consistent_map("SELECT json_array(state,response_json,claim_expires_at_ms,updated_at_ms) AS value FROM media_session_requests WHERE incarnation_id=$1",hiqlite::params!(activation.incarnation_id.clone())).await.expect("request before renewal").pop().expect("row");
            let target = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as i64
                + 30_000;
            let bound_renewal = ReceiverSourceRenewal {
                attachment: attachment.clone(),
                lease_expires_at_ms: target,
            };
            assert_eq!(
                store
                    .renew_receiver_source_session(&fresh, &bound_renewal)
                    .await
                    .expect("published attached renewal"),
                ReceiverSourceWrite::Applied
            );
            let request_after:SchemaText=client.query_consistent_map("SELECT json_array(state,response_json,claim_expires_at_ms,updated_at_ms) AS value FROM media_session_requests WHERE incarnation_id=$1",hiqlite::params!(activation.incarnation_id.clone())).await.expect("request after renewal").pop().expect("row");
            assert_eq!(
                request_before.value, request_after.value,
                "published request never rewritten by renewal"
            );
            let later = now + 7 * 24 * 60 * 60 * 1000;
            store
                .maintain_media_sessions(later)
                .await
                .expect("expiry plus retention sweep");
            assert!(store
                .media_session_route_by_incarnation(&activation.incarnation_id)
                .await
                .expect("retained unresolved route")
                .is_some());
            let claim = store
                .claim_media_session_request(
                    &activation.principal,
                    "B-request",
                    &activation.request_fingerprint,
                    "B-playback",
                    &Uuid::new_v4().to_string(),
                    later,
                    later + 30_000,
                )
                .await
                .expect("no replacement of unknown Source start");
            assert!(!matches!(claim, MediaSessionRequestClaim::Acquired { .. }));
            client
                .execute(
                    "DELETE FROM sharing_relay_upstream WHERE incarnation_id=$1",
                    hiqlite::params!(activation.incarnation_id.as_str()),
                )
                .await
                .expect("missing adjunct corruption fixture");
            assert!(store
                .activate_receiver_media_session(&authority, &activation)
                .await
                .expect("missing adjunct refuses")
                .is_none());
        } else {
            assert!(store
                .media_session_route_by_incarnation(&activation.incarnation_id)
                .await
                .expect("no partial route")
                .is_none());
            let rows = client
                .query_consistent_map::<SchemaText, _>(
                    "SELECT CAST(count(*) AS TEXT) AS value FROM job_leases WHERE resource=$1",
                    hiqlite::params!(format!("session:{}", activation.incarnation_id)),
                )
                .await;
            assert!(rows.is_ok(), "lease census available");
            assert_eq!(
                rows.expect("lease census").first().expect("count").value,
                "0"
            );
        }
    }
}
