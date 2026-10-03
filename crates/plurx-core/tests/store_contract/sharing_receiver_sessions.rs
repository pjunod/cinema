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
        let outcome = store
            .activate_receiver_media_session(&authority, &activation)
            .await
            .expect("atomic B activation");
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
            assert!(!store
                .renew_pending_receiver_session(&fresh, &renewal)
                .await
                .expect("lost assignment refuses renewal"));
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
