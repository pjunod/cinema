use super::*;

#[tokio::test]
async fn sharing_route_legacy_schema_refuses_a_shared_playback_pointer() {
    let store = SqliteStore::open_in_memory().expect("legacy store");
    let principal = crate::playback_principal::PlaybackPrincipal::sharing(
        uuid::Uuid::new_v4(),
        &"a".repeat(64),
    )
    .expect("principal");
    assert!(store
        .media_session_route_for_playback(&principal, "playback")
        .await
        .is_err());
}

#[tokio::test]
async fn sharing_route_reads_rebuilt_sqlite_rows_without_a_local_user() {
    let directory = tempfile::tempdir().expect("pooled store directory");
    for store in [
        SqliteStore::open_in_memory().expect("in-memory store"),
        SqliteStore::open(&directory.path().join("store.db")).expect("pooled store"),
    ] {
        store
            .with_conn(|conn| {
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-local.sql"
                ))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-sharing.sql"
                ))?;
                Ok(())
            })
            .await
            .expect("rebuild and populate");
        for id in [
            "00000000-0000-4000-a000-000000000001",
            "00000000-0000-4000-a000-000000000002",
        ] {
            let route = store
                .media_session_route_by_incarnation(id)
                .await
                .expect("typed route read")
                .expect("route");
            assert_eq!(
                route.principal,
                crate::playback_principal::PlaybackPrincipal::sharing(
                    uuid::Uuid::parse_str(id).expect("grant"),
                    &"a".repeat(64)
                )
                .expect("principal")
            );
            assert_eq!(route.principal.local_user_id(), None);
            let current = store
                .media_session_route_for_playback(&route.principal, "playback")
                .await
                .expect("principal pointer read")
                .expect("current route");
            assert_eq!(current.incarnation_id, id);
            let desired = store
                .desired_selection(&route.principal, "playback")
                .await
                .expect("typed desired read")
                .expect("desired row");
            assert_eq!(desired.principal, route.principal);
            assert_eq!(desired.revision, if id.ends_with('1') { 1 } else { 2 });

            let other_viewer = crate::playback_principal::PlaybackPrincipal::sharing(
                uuid::Uuid::parse_str(id).expect("grant"),
                &"b".repeat(64),
            )
            .expect("viewer");
            assert!(store
                .media_session_route_for_playback(&other_viewer, "playback")
                .await
                .expect("other viewer read")
                .is_none());
            assert!(store
                .desired_selection(&other_viewer, "playback")
                .await
                .expect("other viewer desired read")
                .is_none());
        }
        let local = crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 };
        let local_desired = store
            .record_desired_selection(
                &local,
                "new-local-playback",
                &"c".repeat(64),
                "v1;quality=auto",
                11,
            )
            .await
            .expect("local desired insert after rebuild");
        assert_eq!(local_desired.revision, 1);
        assert_eq!(local_desired.principal, local);
        let replay = store
            .record_desired_selection(
                &local,
                "new-local-playback",
                &"c".repeat(64),
                "v1;quality=auto",
                12,
            )
            .await
            .expect("local desired replay after rebuild");
        assert_eq!(replay.revision, 1);
        assert_eq!(replay.updated_at_ms, 11);
        let changed = store
            .record_desired_selection(
                &local,
                "new-local-playback",
                &"d".repeat(64),
                "v1;quality=auto",
                13,
            )
            .await
            .expect("local desired upsert after rebuild");
        assert_eq!(changed.revision, 2);
        assert_eq!(
            store
                .desired_selection(&local, "new-local-playback")
                .await
                .expect("complete local read")
                .expect("desired")
                .principal,
            local
        );
        assert_eq!(
            store
                .media_session_route_for_playback(&local, "playback")
                .await
                .expect("local route")
                .expect("local pointer")
                .incarnation_id,
            "live"
        );
        store.with_conn(|conn| {
                conn.execute("DELETE FROM media_playback_pointers WHERE share_grant_id='00000000-0000-4000-a000-000000000002'", [])?;
                conn.execute("UPDATE media_playback_pointers SET desired_revision=1, current_incarnation_id='00000000-0000-4000-a000-000000000002' WHERE share_grant_id='00000000-0000-4000-a000-000000000001'", [])?;
                Ok(())
            }).await.expect("corrupt cross-grant pointer fixture");
        let first = crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000001").expect("grant"),
            &"a".repeat(64),
        )
        .expect("principal");
        assert!(store
            .media_session_route_for_playback(&first, "playback")
            .await
            .expect("cross-grant read")
            .is_none());
        store
            .with_conn(|conn| {
                conn.execute("DELETE FROM users WHERE id=1", [])?;
                Ok(())
            })
            .await
            .expect("delete local owner");
        assert!(
            store
                .record_desired_selection(
                    &local,
                    "new-local-playback",
                    &"e".repeat(64),
                    "v1;quality=auto",
                    14
                )
                .await
                .is_err(),
            "deleted owner cannot recreate desired state"
        );
        assert!(store
            .desired_selection(&local, "new-local-playback")
            .await
            .expect("deleted desired read")
            .is_none());
        assert!(store
            .desired_selection(&first, "playback")
            .await
            .expect("sharing selection survives local deletion")
            .is_some());
    }
}

#[tokio::test]
async fn sharing_terminal_ack_cannot_delete_a_foreign_owner_pointer() {
    let directory = tempfile::tempdir().expect("ack directory");
    let incarnation = "00000000-0000-4000-a000-000000000001";
    for store in [
        SqliteStore::open_in_memory().expect("memory ack"),
        SqliteStore::open(&directory.path().join("ack.db")).expect("pooled ack"),
    ] {
        let session = uuid::Uuid::new_v4().to_string();
        let fixture_session = session.clone();
        store.with_conn(move |conn| {
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-local.sql"))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-sharing.sql"))?;
                conn.execute("UPDATE media_sessions SET session_id=?1 WHERE incarnation_id=?2",params![fixture_session,incarnation])?;
                conn.execute("DELETE FROM media_playback_pointers WHERE share_grant_id=?1",[incarnation])?;
                conn.execute("UPDATE media_playback_pointers SET current_incarnation_id=?1,desired_revision=2 WHERE share_grant_id='00000000-0000-4000-a000-000000000002'",[incarnation])?;
                Ok(())
            }).await.expect("corrupt foreign pointer fixture");
        let ack = MediaSessionTerminalAck {
            incarnation_id: incarnation.into(),
            session_id: session.clone(),
            owner_node_id: "node".into(),
            owner_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            request_fingerprint: "a".repeat(64),
            response_json: "{}".into(),
            expires_at_ms: 9000,
            updated_at_ms: 20,
        };
        let mut wrong = ack.clone();
        wrong.owner_epoch = 2;
        assert!(!store
            .record_media_session_terminal_ack(&wrong)
            .await
            .expect("wrong epoch refused"));
        assert!(store
            .record_media_session_terminal_ack(&ack)
            .await
            .expect("Shared ack accepted"));
        assert_eq!(
            store
                .media_session_route_by_incarnation(incarnation)
                .await
                .expect("ended Shared")
                .expect("route")
                .state,
            "ended"
        );
        store.with_read(move |conn| {
                let pointer:String=conn.query_row("SELECT current_incarnation_id FROM media_playback_pointers WHERE share_grant_id='00000000-0000-4000-a000-000000000002'",[],|r|r.get(0))?;assert_eq!(pointer,incarnation);Ok(())
            }).await.expect("foreign pointer preserved");
        assert!(store
            .record_media_session_terminal_ack(&ack)
            .await
            .expect("exact Shared ack replay"));
        store
            .end_media_session(&session, "revoked", 21)
            .await
            .expect("Shared terminal cleanup")
            .expect("ended");
        assert_eq!(
            store
                .media_session_terminal_ack(&session, 22)
                .await
                .expect("retained Shared ack"),
            Some(ack)
        );
    }
}

#[test]
fn sharing_desired_decoder_refuses_corrupt_retained_principal_metadata() {
    let conn = rusqlite::Connection::open_in_memory().expect("desired decoder");
    conn.execute_batch("CREATE TABLE media_sessions(owner_key TEXT,principal_kind TEXT,share_grant_id TEXT,share_viewer_key TEXT);
            CREATE TABLE media_playback_desired(owner_key TEXT,principal_kind TEXT,user_id INTEGER,share_grant_id TEXT,share_viewer_key TEXT,playback_id TEXT,revision INTEGER,digest TEXT,canonical_form TEXT,updated_at_ms INTEGER);
            INSERT INTO media_playback_desired VALUES('local:1','sharing',1,NULL,NULL,'playback',1,'digest','form',10);").expect("corrupt restored projection");
    assert!(desired_within(&conn, 1, "playback").is_err());
    conn.execute(
        "UPDATE media_playback_desired SET principal_kind='local'",
        [],
    )
    .expect("repair metadata");
    assert_eq!(
        desired_within(&conn, 1, "playback")
            .expect("local read")
            .expect("desired")
            .principal
            .local_user_id(),
        Some(1)
    );
}

#[tokio::test]
async fn sharing_terminal_cleanup_and_corrupt_ledger_retirement_preserve_owner_boundaries() {
    let directory = tempfile::tempdir().expect("cleanup directory");
    let foreign = "00000000-0000-4000-a000-000000000001";
    for store in [
        SqliteStore::open_in_memory().expect("memory cleanup"),
        SqliteStore::open(&directory.path().join("cleanup.db")).expect("pooled cleanup"),
    ] {
        let foreign_session = uuid::Uuid::new_v4().to_string();
        let fixture_session = foreign_session.clone();
        store.with_conn(move |conn| {
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-local.sql"))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-sharing.sql"))?;
                conn.execute("UPDATE media_sessions SET session_id=?1 WHERE incarnation_id=?2",params![fixture_session,foreign])?;
                conn.execute("UPDATE media_session_preparations SET staged_incarnation_id=?1,deadline_ms=20 WHERE owner_key='local:1'",[foreign])?;
                conn.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES(?1,'node',1,4,9000,10)",[format!("session:{foreign}")])?;
                Ok(())
            }).await.expect("cleanup fixture");
        let before = store
            .media_session_route_by_incarnation(foreign)
            .await
            .expect("foreign read")
            .expect("foreign");
        store
            .maintain_media_sessions(21)
            .await
            .expect("sweep corrupt ledger");
        assert_eq!(
            store
                .media_session_route_by_incarnation(foreign)
                .await
                .expect("foreign after sweep")
                .expect("foreign"),
            before
        );
        store
            .with_read(move |conn| {
                let lease: (i64, i64, i64) = conn.query_row(
                    "SELECT revision,expires_at_ms,updated_at_ms FROM job_leases WHERE resource=?1",
                    [format!("session:{foreign}")],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(lease, (4, 9000, 10));
                let ledger: i64 = conn.query_row(
                    "SELECT count(*) FROM media_session_preparations WHERE owner_key='local:1'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(ledger, 0);
                Ok(())
            })
            .await
            .expect("foreign lease preserved and corrupt ledger removed");
        let end = MediaSessionEnd {
            incarnation_id: foreign.into(),
            session_id: foreign_session.clone(),
            expected_owner_node_id: "node".into(),
            expected_owner_epoch: 1,
            expected_lease_expires_at_ms: 9000,
            terminal_reason: "revoked".into(),
            now_ms: 22,
        };
        let ended = store
            .end_media_session_if_owner(&end)
            .await
            .expect("Shared exact cleanup")
            .expect("ended");
        assert_eq!(ended.principal, before.principal);
        assert_eq!(ended.state, "ended");
        assert!(store
            .media_session_route_for_playback(&before.principal, "playback")
            .await
            .expect("retired Shared pointer")
            .is_none());
        assert_eq!(
            store
                .end_media_session(&foreign_session, "revoked", 23)
                .await
                .expect("Shared capability cleanup")
                .expect("ended")
                .principal,
            before.principal
        );
        // A second grant sharing the same viewer/playback remains active.
        let other = store
            .media_session_route_by_incarnation("00000000-0000-4000-a000-000000000002")
            .await
            .expect("other grant")
            .expect("other route");
        assert_eq!(other.state, "active");
        assert_ne!(other.principal, before.principal);
        assert!(store
            .media_session_route_for_playback(&other.principal, "playback")
            .await
            .expect("other pointer")
            .is_some());
    }
}

#[tokio::test]
async fn sharing_worker_authority_refuses_shared_and_deleted_local_owners() {
    use crate::playback_principal::PlaybackPrincipal;
    let directory = tempfile::tempdir().expect("authority directory");
    let foreign = "00000000-0000-4000-a000-000000000001";
    for store in [
        SqliteStore::open_in_memory().expect("memory authority"),
        SqliteStore::open(&directory.path().join("authority.db")).expect("pooled authority"),
    ] {
        store.with_conn(move |conn| {
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-local.sql"))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-sharing.sql"))?;
                conn.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES(?1,'node',1,4,9000,10)", [format!("session:{foreign}")])?;
                Ok(())
            }).await.expect("authority fixture");
        let original = store
            .media_session_route_by_incarnation(foreign)
            .await
            .expect("read foreign")
            .expect("foreign");
        let shared_renewal = MediaSessionRenewal {
            incarnation_id: foreign.into(),
            owner_epoch: 1,
            produced_playable_through_ms: 10,
            fetched_through_ms: 10,
            media_sequence: 1,
        };
        assert!(store
            .renew_media_sessions("node", std::slice::from_ref(&shared_renewal), 20, 10000)
            .await
            .expect("refused shared renewal")
            .is_empty());
        let shared_takeover = MediaSessionTakeover {
            incarnation_id: foreign.into(),
            expected_owner_node_id: "node".into(),
            expected_owner_epoch: 1,
            next_owner_node_id: "next".into(),
            now_ms: 9001,
            lease_expires_at_ms: 18000,
        };
        assert!(store
            .claim_media_session_takeover(&shared_takeover)
            .await
            .expect("refused Shared takeover")
            .is_none());
        // Even an already-published Shared row must not appear as successful
        // replay of an incarnation-only Local handoff operation.
        assert!(store
            .complete_media_session_handoff(
                foreign,
                "node",
                1,
                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                21
            )
            .await
            .expect("shared completion refusal")
            .is_none());
        store
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE media_sessions SET publication_ready_at_ms=?1 WHERE incarnation_id=?2",
                    params![MEDIA_SESSION_PUBLICATION_BLOCKED, foreign],
                )?;
                Ok(())
            })
            .await
            .expect("blocked foreign fixture");
        assert!(store
            .arm_media_session_handoff(
                foreign,
                "node",
                1,
                21 + MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
                21
            )
            .await
            .expect("shared arm refusal")
            .is_none());
        store
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE media_sessions SET publication_ready_at_ms=0 WHERE incarnation_id=?1",
                    [foreign],
                )?;
                Ok(())
            })
            .await
            .expect("restore foreign fixture");
        assert_eq!(
            store
                .media_session_route_by_incarnation(foreign)
                .await
                .expect("read unchanged foreign")
                .expect("foreign"),
            original
        );
        store.with_read(move |conn| { let lease:(String,i64,i64,i64,i64)=conn.query_row("SELECT owner_node_id,fence,revision,expires_at_ms,updated_at_ms FROM job_leases WHERE resource=?1", [format!("session:{foreign}")], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?; assert_eq!(lease,("node".into(),1,4,9000,10)); Ok(()) }).await.expect("foreign lease unchanged");
        let activation = MediaSessionActivation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: PlaybackPrincipal::LocalUser { user_id: 1 },
            playback_id: "renew-local".into(),
            recovery_epoch: uuid::Uuid::new_v4().to_string(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: None,
            request_fingerprint: "c".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: 30,
            lease_expires_at_ms: 8000,
            expected_desired_revision: None,
        };
        let started = store
            .activate_media_session(&activation)
            .await
            .expect("local start")
            .expect("local started");
        store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                31,
            )
            .await
            .expect("confirm local")
            .expect("confirmed");
        let renewal = MediaSessionRenewal {
            incarnation_id: activation.incarnation_id.clone(),
            owner_epoch: started.route.owner_epoch,
            produced_playable_through_ms: 20,
            fetched_through_ms: 10,
            media_sequence: 2,
        };
        assert_eq!(
            store
                .renew_media_sessions("node", std::slice::from_ref(&renewal), 40, 10000)
                .await
                .expect("local renewal"),
            vec![activation.incarnation_id.clone()]
        );
        let takeover = MediaSessionTakeover {
            incarnation_id: activation.incarnation_id.clone(),
            expected_owner_node_id: "node".into(),
            expected_owner_epoch: started.route.owner_epoch,
            next_owner_node_id: "next".into(),
            now_ms: 10001,
            lease_expires_at_ms: 18000,
        };
        let taken = store
            .claim_media_session_takeover(&takeover)
            .await
            .expect("local takeover")
            .expect("taken");
        assert_eq!(taken.principal, activation.principal);
        assert_eq!(taken.owner_node_id, "next");
        assert_eq!(taken.owner_epoch, started.route.owner_epoch + 1);
        let local_incarnation = activation.incarnation_id.clone();
        let local_epoch = taken.owner_epoch;
        store.with_conn(move |conn| {
                conn.execute("DELETE FROM users WHERE id=1",[])?;
                // Restore a corrupt active projection after deletion triggers:
                // a retired state alone must not be the reason authority fails.
                conn.execute("UPDATE media_sessions SET state='active',terminal_reason=NULL,lease_expires_at_ms=18000,publication_ready_at_ms=0 WHERE incarnation_id=?1",[&local_incarnation])?;
                conn.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES(?1,'next',?2,5,18000,10001) ON CONFLICT(resource) DO UPDATE SET owner_node_id='next',fence=?2,revision=5,expires_at_ms=18000,updated_at_ms=10001",params![format!("session:{local_incarnation}"),local_epoch])?;
                Ok(())
            }).await.expect("deleted user corrupt fixture");
        let deleted_renewal = MediaSessionRenewal {
            owner_epoch: local_epoch,
            ..renewal
        };
        assert!(store
            .renew_media_sessions("next", std::slice::from_ref(&deleted_renewal), 10002, 19000)
            .await
            .expect("deleted renewal refused")
            .is_empty());
        let deleted_takeover = MediaSessionTakeover {
            expected_owner_node_id: "next".into(),
            expected_owner_epoch: local_epoch,
            next_owner_node_id: "last".into(),
            now_ms: 18001,
            lease_expires_at_ms: 25000,
            ..takeover
        };
        assert!(store
            .claim_media_session_takeover(&deleted_takeover)
            .await
            .expect("deleted takeover refused")
            .is_none());
        assert!(store
            .complete_media_session_handoff(
                &activation.incarnation_id,
                "next",
                local_epoch,
                MediaSessionProjectionCompletion::PredecessorAcknowledged,
                10003
            )
            .await
            .expect("deleted handoff refused")
            .is_none());
    }
}

#[tokio::test]
async fn sharing_preparation_commit_preserves_principal_and_fences_receipt_session() {
    use crate::playback_principal::PlaybackPrincipal;
    let directory = tempfile::tempdir().expect("commit directory");
    let predecessor = "00000000-0000-4000-a000-000000000003";
    let foreign = "00000000-0000-4000-a000-000000000001";
    for store in [
        SqliteStore::open_in_memory().expect("memory commit"),
        SqliteStore::open(&directory.path().join("commit.db")).expect("pooled commit"),
    ] {
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO users(id,username,password_hash,is_admin,created_at)
                    VALUES(1,'owner','hash',0,1)",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO media_sessions
                    (incarnation_id,session_id,user_id,playback_id,request_fingerprint,
                     owner_node_id,owner_epoch,lease_expires_at_ms,state,recipe_json,response_json,
                     updated_at_ms,recovery_epoch)
                    VALUES(?1,?2,1,'playback',?3,'node',2,9000,'active','{}','{}',10,'epoch')",
                    params![
                        predecessor,
                        uuid::Uuid::new_v4().to_string(),
                        "a".repeat(64)
                    ],
                )?;
                conn.execute(
                    "INSERT INTO media_playback_pointers
                    (user_id,playback_id,current_incarnation_id,updated_at_ms)
                    VALUES(1,'playback',?1,10)",
                    [predecessor],
                )?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-sharing.sql"
                ))?;
                conn.execute(
                    "UPDATE media_sessions SET session_id=?1 WHERE incarnation_id=?2",
                    params![uuid::Uuid::new_v4().to_string(), foreign],
                )?;
                Ok(())
            })
            .await
            .expect("commit fixture");
        let local = PlaybackPrincipal::LocalUser { user_id: 1 };
        let predecessor_route = store
            .media_session_route_by_incarnation(predecessor)
            .await
            .expect("predecessor")
            .expect("route");
        let foreign_route = store
            .media_session_route_by_incarnation(foreign)
            .await
            .expect("foreign")
            .expect("route");
        let mut preparation = MediaSessionPreparation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: local.clone(),
            playback_id: "playback".into(),
            expected_predecessor_incarnation_id: predecessor.into(),
            expected_predecessor_owner_node_id: "node".into(),
            expected_predecessor_owner_epoch: 2,
            request_fingerprint: "b".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            media_origin_ms: 0,
            now_ms: 20,
            expected_desired_revision: None,
            quality_cancellation_key: None,
            deadline_ms: 8000,
        };
        assert!(store
            .prepare_media_session(&preparation)
            .await
            .expect("prepare")
            .is_some());
        let mut receipt = MediaSessionTerminalAck {
            incarnation_id: predecessor.into(),
            session_id: foreign_route.session_id.clone(),
            owner_node_id: "node".into(),
            owner_epoch: 2,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            request_fingerprint: "c".repeat(64),
            response_json: "{}".into(),
            expires_at_ms: 9000,
            updated_at_ms: 21,
        };
        let mut request = MediaSessionPreparationCommitRequest {
            staged_incarnation_id: preparation.incarnation_id.clone(),
            expected_predecessor_owner_node_id: "node".into(),
            expected_predecessor_owner_epoch: 2,
            now_ms: 21,
            lease_expires_at_ms: 8000,
            control_receipt: Some(receipt.clone()),
            expected_desired_revision: None,
        };
        assert!(store
            .commit_media_session_preparation(&local, "playback", &request)
            .await
            .expect("wrong-session refusal")
            .is_none());
        assert_eq!(
            store
                .media_session_route_for_playback(&local, "playback")
                .await
                .expect("unchanged pointer")
                .expect("route")
                .incarnation_id,
            predecessor
        );
        let foreign_session = foreign_route.session_id.clone();
        store
            .with_read(move |conn| {
                let count: i64 = conn.query_row(
                    "SELECT count(*) FROM media_session_terminal_acks WHERE session_id=?1",
                    [foreign_session],
                    |r| r.get(0),
                )?;
                assert_eq!(count, 0);
                Ok(())
            })
            .await
            .expect("no foreign ack");
        preparation.incarnation_id = uuid::Uuid::new_v4().to_string();
        preparation.session_id = uuid::Uuid::new_v4().to_string();
        preparation.now_ms = 22;
        assert!(store
            .prepare_media_session(&preparation)
            .await
            .expect("fresh prepare")
            .is_some());
        receipt.session_id = predecessor_route.session_id;
        receipt.updated_at_ms = 23;
        request.staged_incarnation_id = preparation.incarnation_id.clone();
        request.now_ms = 23;
        request.control_receipt = Some(receipt.clone());
        let committed = store
            .commit_media_session_preparation(&local, "playback", &request)
            .await
            .expect("commit")
            .expect("committed");
        assert_eq!(committed.route.principal, local);
        let prior = committed.predecessor.expect("same-owner predecessor");
        assert_eq!(prior.principal, local);
        assert_eq!(prior.state, "active");
        assert_eq!(prior.drain_deadline_ms, Some(23 + MEDIA_SESSION_DRAIN_MS));
        assert_eq!(committed.control_receipt, Some(receipt.clone()));
        let replay = store
            .commit_media_session_preparation(&local, "playback", &request)
            .await
            .expect("commit replay")
            .expect("replayed");
        assert_eq!(replay.route, committed.route);
        assert_eq!(replay.control_receipt, Some(receipt));
        assert_eq!(
            store
                .media_session_route_for_playback(&local, "playback")
                .await
                .expect("advanced pointer")
                .expect("route")
                .incarnation_id,
            preparation.incarnation_id
        );
        assert_eq!(
            store
                .media_session_route_by_incarnation(foreign)
                .await
                .expect("foreign survived")
                .expect("route"),
            foreign_route
        );
    }
}

#[tokio::test]
async fn sharing_activation_rebuilt_local_metadata_and_foreign_owner_refusal() {
    use crate::playback_principal::PlaybackPrincipal;
    let directory = tempfile::tempdir().expect("activation directory");
    let foreign = "00000000-0000-4000-a000-000000000001";
    for store in [
        SqliteStore::open_in_memory().expect("memory activation"),
        SqliteStore::open(&directory.path().join("activation.db")).expect("pooled activation"),
    ] {
        store
            .with_conn(move |conn| {
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-local.sql"
                ))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-sharing.sql"
                ))?;
                conn.execute(
                    "INSERT INTO job_leases
                    (resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms)
                    VALUES(?1,'node',1,4,9000,10)",
                    [format!("session:{foreign}")],
                )?;
                Ok(())
            })
            .await
            .expect("activation fixture");
        let local = PlaybackPrincipal::LocalUser { user_id: 1 };
        let activation = MediaSessionActivation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: local.clone(),
            playback_id: "new-activation".into(),
            recovery_epoch: uuid::Uuid::new_v4().to_string(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: None,
            request_fingerprint: "c".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: 20,
            lease_expires_at_ms: 8000,
            expected_desired_revision: None,
        };
        let started = store
            .activate_media_session(&activation)
            .await
            .expect("activate")
            .expect("started");
        assert_eq!(started.route.principal, local);
        assert_eq!(
            started.route.publication_ready_at_ms,
            MEDIA_SESSION_PUBLICATION_BLOCKED
        );
        let confirmed = store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                21,
            )
            .await
            .expect("confirm")
            .expect("confirmed");
        assert_eq!(confirmed.principal, local);
        assert_eq!(confirmed.publication_ready_at_ms, 0);
        assert_eq!(
            store
                .activate_media_session(&activation)
                .await
                .expect("activation replay")
                .expect("replayed")
                .route,
            confirmed
        );
        store
            .with_read(|conn| {
                let metadata: (String, String, i64, Option<String>, Option<String>) = conn
                    .query_row(
                        "SELECT owner_key,principal_kind,user_id,share_grant_id,share_viewer_key
                     FROM media_playback_pointers WHERE playback_id='new-activation'",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )?;
                assert_eq!(metadata, ("local:1".into(), "local".into(), 1, None, None));
                Ok(())
            })
            .await
            .expect("complete pointer metadata");
        let mut collision = activation.clone();
        collision.incarnation_id = foreign.into();
        collision.playback_id = "collision".into();
        assert!(store
            .activate_media_session(&collision)
            .await
            .expect("foreign collision")
            .is_none());
        store
            .with_read(move |conn| {
                let lease: (i64, i64, i64) = conn.query_row(
                    "SELECT revision,expires_at_ms,updated_at_ms
                    FROM job_leases WHERE resource=?1",
                    [format!("session:{foreign}")],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(lease, (4, 9000, 10));
                Ok(())
            })
            .await
            .expect("foreign lease intact");
        let mut requested = activation.clone();
        requested.incarnation_id = uuid::Uuid::new_v4().to_string();
        requested.session_id = uuid::Uuid::new_v4().to_string();
        requested.playback_id = "request-activation".into();
        requested.request_id = Some("activation-request".into());
        requested.now_ms = 25;
        assert!(matches!(
            store
                .claim_media_session_request(
                    &local,
                    "activation-request",
                    &requested.request_fingerprint,
                    &requested.playback_id,
                    &requested.incarnation_id,
                    23,
                    8000
                )
                .await
                .expect("request claim"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(store
            .assign_media_session_request_owner(
                &local,
                "activation-request",
                &requested.incarnation_id,
                "node",
                24
            )
            .await
            .expect("assign"));
        assert!(store
            .activate_media_session(&requested)
            .await
            .expect("requested activation")
            .is_some());
        assert!(store
            .settle_media_session_activation(
                &requested,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0
                },
                26
            )
            .await
            .expect("requested confirmation")
            .is_some());
        let published = store
            .publish_media_session_activation(
                &local,
                "activation-request",
                &requested.incarnation_id,
                27,
            )
            .await
            .expect("publication")
            .expect("published");
        assert_eq!(published.principal, local);
        assert_eq!(
            store
                .publish_media_session_activation(
                    &local,
                    "activation-request",
                    &requested.incarnation_id,
                    28
                )
                .await
                .expect("publication replay")
                .expect("replay"),
            published
        );
        store
            .with_read(|conn| {
                let row: (String, String, String) = conn.query_row(
                    "SELECT owner_key,principal_kind,state
                    FROM media_session_requests WHERE request_id='activation-request'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                assert_eq!(row, ("local:1".into(), "local".into(), "resolved".into()));
                Ok(())
            })
            .await
            .expect("canonical published request");
        let mut absent = activation.clone();
        absent.principal = PlaybackPrincipal::LocalUser { user_id: 2 };
        absent.incarnation_id = uuid::Uuid::new_v4().to_string();
        assert!(store
            .activate_media_session(&absent)
            .await
            .expect("absent owner refusal")
            .is_none());
        let mut shared = activation.clone();
        shared.principal = PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str(foreign).expect("grant"),
            &"a".repeat(64),
        )
        .expect("shared");
        assert!(store.activate_media_session(&shared).await.is_err());
        store
            .with_conn(move |conn| {
                conn.execute(
                    "DELETE FROM media_playback_pointers WHERE current_incarnation_id=?1",
                    [foreign],
                )?;
                conn.execute(
                    "UPDATE media_playback_pointers SET current_incarnation_id=?1
                    WHERE owner_key='local:1' AND playback_id='new-activation'",
                    [foreign],
                )?;
                Ok(())
            })
            .await
            .expect("corrupt cross-owner pointer");
        let mut replacement = activation.clone();
        replacement.incarnation_id = uuid::Uuid::new_v4().to_string();
        replacement.expected_predecessor_incarnation_id = Some(foreign.into());
        assert!(store
            .activate_media_session(&replacement)
            .await
            .expect("cross-owner pointer refusal")
            .is_none());
        assert_eq!(
            store
                .media_session_route_by_incarnation(foreign)
                .await
                .expect("foreign route")
                .expect("route")
                .state,
            "active"
        );
    }
}

#[tokio::test]
async fn sharing_preparation_rebuilt_local_writes_and_foreign_abort_isolation() {
    use crate::playback_principal::PlaybackPrincipal;
    let directory = tempfile::tempdir().expect("preparation directory");
    let predecessor = "00000000-0000-4000-a000-000000000003";
    let foreign = "00000000-0000-4000-a000-000000000001";
    for store in [
        SqliteStore::open_in_memory().expect("memory preparation"),
        SqliteStore::open(&directory.path().join("preparation.db")).expect("pooled preparation"),
    ] {
        store
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO users(id,username,password_hash,is_admin,created_at)
                    VALUES(1,'owner','hash',0,1)",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO media_sessions
                    (incarnation_id,session_id,user_id,playback_id,request_fingerprint,
                     owner_node_id,owner_epoch,lease_expires_at_ms,state,recipe_json,response_json,
                     updated_at_ms,recovery_epoch)
                    VALUES(?1,?2,1,'playback',?3,'node',2,9000,'active','{}','{}',10,'epoch')",
                    params![
                        predecessor,
                        uuid::Uuid::new_v4().to_string(),
                        "a".repeat(64)
                    ],
                )?;
                conn.execute(
                    "INSERT INTO media_playback_pointers
                    (user_id,playback_id,current_incarnation_id,updated_at_ms)
                    VALUES(1,'playback',?1,10)",
                    [predecessor],
                )?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-sharing.sql"
                ))?;
                Ok(())
            })
            .await
            .expect("preparation fixture");
        let local = PlaybackPrincipal::LocalUser { user_id: 1 };
        let mut preparation = MediaSessionPreparation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            principal: local.clone(),
            playback_id: "playback".into(),
            expected_predecessor_incarnation_id: predecessor.into(),
            expected_predecessor_owner_node_id: "node".into(),
            expected_predecessor_owner_epoch: 2,
            request_fingerprint: "b".repeat(64),
            owner_node_id: "node".into(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            media_origin_ms: 0,
            now_ms: 20,
            expected_desired_revision: None,
            quality_cancellation_key: None,
            deadline_ms: 8000,
        };
        let staged = store
            .prepare_media_session(&preparation)
            .await
            .expect("prepare")
            .expect("staged");
        assert_eq!(staged.principal, local);
        assert_eq!(staged.recovery_epoch, "epoch");
        assert_eq!(
            store
                .prepare_media_session(&preparation)
                .await
                .expect("replay")
                .expect("replay route"),
            staged
        );
        assert_eq!(
            store
                .media_session_route_for_playback(&local, "playback")
                .await
                .expect("pointer")
                .expect("route")
                .incarnation_id,
            predecessor
        );
        let previous = preparation.incarnation_id.clone();
        preparation.incarnation_id = uuid::Uuid::new_v4().to_string();
        preparation.session_id = uuid::Uuid::new_v4().to_string();
        preparation.now_ms = 21;
        let replacement = store
            .rejoin_media_session_preparation(&previous, &preparation)
            .await
            .expect("rejoin")
            .expect("replacement");
        assert_eq!(replacement.principal, local);
        assert_eq!(
            store
                .staged_media_session_for_playback(&local, "playback")
                .await
                .expect("ledger")
                .expect("ledger row")
                .staged_incarnation_id,
            preparation.incarnation_id
        );
        let aborted = store
            .abort_media_session_preparation(
                &local,
                "playback",
                &MediaSessionPreparationAbortRequest {
                    staged_incarnation_id: preparation.incarnation_id.clone(),
                    expected_predecessor_owner_node_id: "node".into(),
                    expected_predecessor_owner_epoch: 2,
                    now_ms: 22,
                },
            )
            .await
            .expect("abort")
            .expect("aborted route");
        assert_eq!(aborted.principal, local);
        assert_eq!(aborted.state, "ended");
        store.with_conn(move |conn| {
                conn.execute("INSERT INTO media_session_preparations
                    (owner_key,principal_kind,user_id,playback_id,staged_incarnation_id,
                     expected_predecessor_incarnation_id,deadline_ms,created_at_ms,updated_at_ms)
                    VALUES('local:1','local',1,'playback',?1,?2,8000,10,10)",params![foreign,predecessor])?;
                conn.execute("INSERT INTO cache_consumer_pins
                    (storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms)
                    VALUES('storage','recipe','generation','media_session',?1,1,9000)",[foreign])?;
                conn.execute("INSERT INTO job_leases
                    (resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms)
                    VALUES(?1,'node',1,4,9000,10)",[format!("session:{foreign}")])?;
                Ok(())
            }).await.expect("corrupt local ledger points to foreign owner");
        assert!(store
            .abort_media_session_preparation(
                &local,
                "playback",
                &MediaSessionPreparationAbortRequest {
                    staged_incarnation_id: foreign.into(),
                    expected_predecessor_owner_node_id: "node".into(),
                    expected_predecessor_owner_epoch: 2,
                    now_ms: 30,
                }
            )
            .await
            .expect("foreign abort refusal")
            .is_none());
        store.with_read(move |conn| {
                let state:(String,i64,i64)=conn.query_row("SELECT state,lease_expires_at_ms,updated_at_ms FROM media_sessions WHERE incarnation_id=?1",[foreign],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                assert_eq!(state,("active".into(),9000,10));
                let pin:i64=conn.query_row("SELECT expires_at_ms FROM cache_consumer_pins WHERE consumer_id=?1",[foreign],|r|r.get(0))?;
                assert_eq!(pin,9000);
                let lease:(i64,i64,i64)=conn.query_row("SELECT revision,expires_at_ms,updated_at_ms FROM job_leases WHERE resource=?1",[format!("session:{foreign}")],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                assert_eq!(lease,(4,9000,10));
                Ok(())
            }).await.expect("foreign route/pin/lease retained");
        store
            .with_conn(move |conn| {
                conn.execute("DELETE FROM users WHERE id=1", [])?;
                Ok(())
            })
            .await
            .expect("delete local owner");
        preparation.incarnation_id = uuid::Uuid::new_v4().to_string();
        assert!(store
            .prepare_media_session(&preparation)
            .await
            .expect("deleted owner")
            .is_none());
    }
}

#[tokio::test]
async fn sharing_staged_and_worker_inventory_preserve_distinct_owners() {
    use crate::playback_principal::PlaybackPrincipal;
    let directory = tempfile::tempdir().expect("inventory directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory inventory"),
        SqliteStore::open(&directory.path().join("inventory.db")).expect("pooled inventory"),
    ] {
        store
            .with_conn(|conn| {
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-local.sql"
                ))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-sharing.sql"
                ))?;
                // Both grants have the same viewer/playback. Their preparations
                // must still occupy independent slots. Use distinct staged IDs
                // so the serving routes remain available to their dedicated actor.
                conn.execute(
                    "INSERT INTO media_session_preparations
                    (owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,
                     playback_id,staged_incarnation_id,expected_predecessor_incarnation_id,
                     deadline_ms,created_at_ms,updated_at_ms)
                    SELECT owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,
                     playback_id,'staged-' || incarnation_id,incarnation_id,8000,10,10
                    FROM media_sessions WHERE principal_kind='sharing'",
                    [],
                )?;
                // A starting claim suppresses exactly its own serving owner;
                // NULL user IDs cannot join the unrelated grant's inventory.
                conn.execute(
                    "INSERT INTO media_session_requests
                    (owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,
                     request_id,request_fingerprint,playback_id,state,claim_expires_at_ms,
                     incarnation_id,owner_node_id,updated_at_ms)
                    SELECT owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,
                     'starting','fingerprint',playback_id,'starting',9000,incarnation_id,
                     owner_node_id,10 FROM media_sessions
                    WHERE incarnation_id='00000000-0000-4000-a000-000000000001'",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("inventory fixture");
        let owners = [1, 2].map(|n| {
            PlaybackPrincipal::sharing(
                uuid::Uuid::parse_str(&format!("00000000-0000-4000-a000-{n:012}")).expect("grant"),
                &"a".repeat(64),
            )
            .expect("owner")
        });
        for owner in &owners {
            let staged = store
                .staged_media_session_for_playback(owner, "playback")
                .await
                .expect("staged read")
                .expect("staged");
            assert_eq!(&staged.principal, owner);
            assert_eq!(
                staged.staged_incarnation_id,
                format!("staged-{}", staged.expected_predecessor_incarnation_id)
            );
        }
        let stranger = PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000001").expect("grant"),
            &"b".repeat(64),
        )
        .expect("stranger");
        assert!(store
            .staged_media_session_for_playback(&stranger, "playback")
            .await
            .expect("stranger staged read")
            .is_none());
        let owned = store
            .owned_media_sessions("node", 20)
            .await
            .expect("owned inventory");
        assert!(
            owned.is_empty(),
            "Shared actors are outside the Local lease loop"
        );
        crate::store::SettingsStore::put_settings(
            &store,
            &[
                (crate::store::keys::CLUSTER_MEDIA_POOL_ENABLED, "1"),
                (crate::store::keys::CLUSTER_SESSION_TAKEOVER_ENABLED, "1"),
            ],
        )
        .await
        .expect("enable session takeover");
        let expired = store
            .expired_media_sessions(9000, None, 64)
            .await
            .expect("expired inventory")
            .into_expired()
            .expect("both takeover switches are on");
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].principal, owners[1]);
        store
            .with_conn(|conn| {
                conn.execute(
                    "DELETE FROM media_session_preparations WHERE staged_incarnation_id='staged'",
                    [],
                )?;
                conn.execute(
                    "UPDATE media_session_requests SET state='resolved' WHERE request_id='request'",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("release genuine Local fixture from preparation");
        let local = store
            .owned_media_sessions("node", 20)
            .await
            .expect("Local inventory");
        assert_eq!(local.len(), 1);
        assert_eq!(
            local[0].principal,
            PlaybackPrincipal::LocalUser { user_id: 1 }
        );
        store.with_conn(|conn| { conn.execute("UPDATE media_sessions SET recipe_json='{\"kind\":\"remote_source\"}' WHERE incarnation_id='live'",[])?; Ok(()) }).await.expect("typed Remote B fixture");
        assert!(store
            .owned_media_sessions("node", 20)
            .await
            .expect("actor inventory")
            .is_empty());
    }
}

#[tokio::test]
async fn sharing_recovery_rebuilt_keys_preserve_grants_and_one_local_budget() {
    use crate::domain::{ProducerRecoveryRequest, ProducerRecoveryState};
    use crate::playback_principal::PlaybackPrincipal;
    let directory = tempfile::tempdir().expect("recovery directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory recovery"),
        SqliteStore::open(&directory.path().join("recovery.db")).expect("pooled recovery"),
    ] {
        store.with_conn(|conn| {
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-local.sql"))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!("../../../../tests/fixtures/session-principal-sharing.sql"))?;
                conn.execute("INSERT INTO media_session_producer_recovery
                    (owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,
                    playback_id,recovery_epoch,failed_incarnation_id,failed_producer_attempt,
                    decision_sequence,failed_plan_digest,alternate_plan_digest,state,created_at_ms,updated_at_ms)
                    SELECT owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,
                    playback_id,recovery_epoch,incarnation_id,owner_epoch,1,?1,?2,'reserved',10,10
                    FROM media_sessions WHERE principal_kind='sharing'", params!["a".repeat(64), "b".repeat(64)])?;
                Ok(())
            }).await.expect("recovery candidate fixture");
        let first = PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000001").expect("first grant"),
            &"a".repeat(64),
        )
        .expect("first viewer");
        let second = PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000002").expect("second grant"),
            &"a".repeat(64),
        )
        .expect("second viewer");
        let one = store
            .producer_recovery_for_epoch(&first, "playback", "epoch")
            .await
            .expect("first recovery read")
            .expect("first recovery");
        let two = store
            .producer_recovery_for_epoch(&second, "playback", "epoch")
            .await
            .expect("second recovery read")
            .expect("second recovery");
        assert_eq!(one.principal, first);
        assert_eq!(two.principal, second);
        assert_ne!(one.failed_incarnation_id, two.failed_incarnation_id);
        let outsider = PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000001").expect("outsider grant"),
            &"b".repeat(64),
        )
        .expect("outsider viewer");
        assert!(store
            .producer_recovery_for_epoch(&outsider, "playback", "epoch")
            .await
            .expect("outsider recovery")
            .is_none());
        let mut request = ProducerRecoveryRequest {
            principal: PlaybackPrincipal::LocalUser { user_id: 1 },
            playback_id: "new-recovery".to_owned(),
            recovery_epoch: "new-epoch".to_owned(),
            failed_incarnation_id: uuid::Uuid::new_v4().to_string(),
            failed_producer_attempt: 1,
            decision_sequence: 1,
            failed_plan_digest: "c".repeat(64),
            alternate_plan_digest: "d".repeat(64),
            decode_restriction: None,
        };
        let reserved = store
            .reserve_producer_recovery(&request, 1000)
            .await
            .expect("reserve local")
            .expect("local budget");
        assert_eq!(reserved.principal, request.principal);
        assert_eq!(
            store
                .reserve_producer_recovery(&request, 1001)
                .await
                .expect("replay local"),
            Some(reserved)
        );
        request.failed_incarnation_id = uuid::Uuid::new_v4().to_string();
        assert!(store
            .reserve_producer_recovery(&request, 1002)
            .await
            .expect("different failure")
            .is_none());
        let current = store
            .producer_recovery_for_epoch(
                &request.principal,
                &request.playback_id,
                &request.recovery_epoch,
            )
            .await
            .expect("local recovery")
            .expect("local reservation");
        assert!(store
            .settle_producer_recovery(
                &request.principal,
                &request.playback_id,
                &request.recovery_epoch,
                &current.failed_incarnation_id,
                ProducerRecoveryState::Installed,
                1003
            )
            .await
            .expect("settle local")
            .is_some());
        request.failed_incarnation_id = current.failed_incarnation_id;
        assert!(store
            .reserve_producer_recovery(&request, 1004)
            .await
            .expect("spent local budget")
            .is_none());
        let mut shared = request.clone();
        shared.principal = first.clone();
        assert!(store
            .reserve_producer_recovery(&shared, 1005)
            .await
            .is_err());
        store
            .with_conn(|conn| {
                conn.execute("DELETE FROM users WHERE id=1", [])?;
                Ok(())
            })
            .await
            .expect("delete local owner");
        request.recovery_epoch = "after-delete".to_owned();
        assert!(store
            .reserve_producer_recovery(&request, 1006)
            .await
            .expect("deleted owner refusal")
            .is_none());
        assert!(store
            .producer_recovery_for_epoch(&first, "playback", "epoch")
            .await
            .expect("retained shared budget")
            .is_some());
    }
}

#[tokio::test]
async fn sharing_request_rebuilt_local_keys_replay_and_deleted_owner_refusal() {
    let directory = tempfile::tempdir().expect("pooled request directory");
    for store in [
        SqliteStore::open_in_memory().expect("memory request store"),
        SqliteStore::open(&directory.path().join("request.db")).expect("pooled request store"),
    ] {
        store
            .with_conn(|conn| {
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-local.sql"
                ))?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
                conn.execute_batch(include_str!(
                    "../../../../tests/fixtures/session-principal-sharing.sql"
                ))?;
                Ok(())
            })
            .await
            .expect("request candidate fixture");
        let local = crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 };
        let incarnation = uuid::Uuid::new_v4().to_string();
        let fingerprint = "c".repeat(64);
        assert!(matches!(
            store
                .claim_media_session_request(
                    &local,
                    "new-request",
                    &fingerprint,
                    "new-playback",
                    &incarnation,
                    1000,
                    2000
                )
                .await
                .expect("canonical local request insert"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(matches!(
            store
                .claim_media_session_request(
                    &local,
                    "new-request",
                    &fingerprint,
                    "new-playback",
                    &incarnation,
                    1001,
                    2000
                )
                .await
                .expect("request replay"),
            MediaSessionRequestClaim::InFlight { .. }
        ));
        assert!(store
            .record_library_channel_session_recipe(&local, "new-request", &incarnation, "{}", 1001)
            .await
            .expect("canonical recipe insert"));
        assert!(store
            .record_library_channel_session_recipe(&local, "new-request", &incarnation, "{}", 1002)
            .await
            .expect("canonical recipe upsert"));
        assert!(store
            .assign_media_session_request_owner(
                &local,
                "new-request",
                &incarnation,
                "new-node",
                1001
            )
            .await
            .expect("canonical owner assignment"));
        assert!(!store
            .assign_media_session_request_owner(
                &local,
                "new-request",
                &incarnation,
                "other-node",
                1002
            )
            .await
            .expect("owner assignment remains fenced"));
        assert!(store
            .fail_media_session_request(&local, "new-request", &incarnation, 1002)
            .await
            .expect("canonical failure"));
        let successor = uuid::Uuid::new_v4().to_string();
        assert!(
            matches!(store.claim_media_session_request(&local, "new-request", &fingerprint, "new-playback", &successor, 1003, 2000).await.expect("canonical reacquire"), MediaSessionRequestClaim::Acquired { incarnation_id } if incarnation_id == successor)
        );
        store.with_conn(|conn| {
                let owner: String = conn.query_row("SELECT owner_key || ':' || principal_kind || ':' || user_id FROM media_session_requests WHERE request_id='new-request'", [], |row| row.get(0))?;
                assert_eq!(owner, "local:1:local:1");
                conn.execute("UPDATE media_session_requests SET state='resolved', incarnation_id='00000000-0000-4000-a000-000000000001' WHERE request_id='new-request'", [])?;
                Ok(())
            }).await.expect("cross-principal replay fixture");
        assert!(
            matches!(
                store
                    .claim_media_session_request(
                        &local,
                        "new-request",
                        &fingerprint,
                        "new-playback",
                        &successor,
                        1004,
                        2000
                    )
                    .await
                    .expect("cross-principal replay refusal"),
                MediaSessionRequestClaim::Conflict
            ),
            "a resolved request cannot replay another principal's route"
        );
        let shared = crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000001").expect("grant"),
            &"a".repeat(64),
        )
        .expect("principal");
        assert!(
            store
                .claim_media_session_request(
                    &shared,
                    "shared-request",
                    &fingerprint,
                    "new-playback",
                    &successor,
                    1005,
                    2000
                )
                .await
                .is_err(),
            "shared writers wait for authority and floor integration"
        );
        store
            .with_conn(|conn| {
                conn.execute("DELETE FROM users WHERE id=1", [])?;
                Ok(())
            })
            .await
            .expect("delete request owner");
        assert!(
            matches!(
                store
                    .claim_media_session_request(
                        &local,
                        "deleted-request",
                        &fingerprint,
                        "new-playback",
                        &successor,
                        1006,
                        2000
                    )
                    .await
                    .expect("deleted account refusal"),
                MediaSessionRequestClaim::Overloaded
            ),
            "deleted local account cannot create a request"
        );
        assert!(store
            .media_session_route_by_incarnation("00000000-0000-4000-a000-000000000002")
            .await
            .expect("other grant retained")
            .is_some());
    }
}

#[test]
fn sharing_route_projection_refuses_an_incomplete_rebuild() {
    let conn = rusqlite::Connection::open_in_memory().expect("connection");
    conn.execute_batch("CREATE TABLE media_sessions(user_id INTEGER, owner_key TEXT)")
        .expect("partial schema");
    assert!(route_projection(&conn).is_err());
}

#[test]
fn sharing_route_decoder_preserves_namespace_and_refuses_corrupt_projection() {
    let conn = rusqlite::Connection::open_in_memory().expect("fixture connection");
    let grant = uuid::Uuid::new_v4().to_string();
    let viewer = "a".repeat(64);
    let key = format!("share:{grant}:{viewer}");
    let decode = |kind: &str,
                  user: Option<i64>,
                  grant: Option<&str>,
                  viewer: Option<&str>,
                  key: &str| {
        conn.query_row("SELECT 'incarnation' AS incarnation_id, 'session' AS session_id, ?2 AS user_id, 'playback' AS playback_id, 'fingerprint' AS request_fingerprint, 'node' AS owner_node_id, 1 AS owner_epoch, 2000 AS lease_expires_at_ms, 'active' AS state, NULL AS terminal_reason, 1000 AS publication_ready_at_ms, '{}' AS recipe_json, '{}' AS response_json, 0 AS produced_playable_through_ms, 0 AS fetched_through_ms, 0 AS media_origin_ms, 0 AS media_sequence, 0 AS discontinuity_sequence, 1000 AS updated_at_ms, 'recovery' AS recovery_epoch, NULL AS drain_deadline_ms, ?5 AS owner_key, ?1 AS principal_kind, ?3 AS share_grant_id, ?4 AS share_viewer_key", rusqlite::params![kind, user, grant, viewer, key], route_from_row)
    };
    let local = decode("local", Some(7), None, None, "local:7").expect("local route");
    assert_eq!(local.principal.local_user_id(), Some(7));
    let shared = decode("sharing", None, Some(&grant), Some(&viewer), &key).expect("shared route");
    assert_eq!(
        shared.principal,
        crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str(&grant).expect("grant"),
            &viewer
        )
        .expect("principal")
    );
    assert_eq!(shared.principal.local_user_id(), None);
    assert!(decode("sharing", Some(7), Some(&grant), Some(&viewer), &key).is_err());
    assert!(decode("sharing", None, Some(&grant), None, &key).is_err());
    assert!(decode("sharing", None, Some(&grant), Some(&viewer), "local:7").is_err());
    assert!(decode("local", Some(0), None, None, "local:0").is_err());
    assert!(decode("local", Some(-1), None, None, "local:-1").is_err());
    assert!(decode("local", Some(7), None, None, "local:07").is_err());
}

#[tokio::test]
async fn idle_session_maintenance_skips_updates_but_still_retires_expired_sessions() {
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use std::sync::{LazyLock, Mutex};

    // Only this fixture's connection installs the callback, so other
    // concurrent tests cannot contribute statements to this log.
    static UPDATES: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    fn record(event: TraceEvent<'_>) {
        if let TraceEvent::Stmt(_, sql) = event {
            if sql.starts_with("UPDATE media_sessions") {
                UPDATES.lock().expect("trace log").push(sql.to_owned());
            }
        }
    }
    async fn measure(store: &SqliteStore, now_ms: i64) -> Vec<String> {
        UPDATES.lock().expect("trace log").clear();
        store
            .conn
            .lock()
            .expect("connection")
            .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record));
        store
            .maintain_media_sessions(now_ms)
            .await
            .expect("maintenance");
        store
            .conn
            .lock()
            .expect("connection")
            .trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, None);
        UPDATES.lock().expect("trace log").clone()
    }

    let store = SqliteStore::open_in_memory().expect("store");
    assert!(
        measure(&store, 1_000).await.is_empty(),
        "an empty store has no sessions to update"
    );
    store
        .conn
        .lock()
        .expect("connection")
        .execute(
            "INSERT INTO media_sessions
                (incarnation_id, session_id, user_id, playback_id, request_fingerprint,
                 owner_node_id, owner_epoch, lease_expires_at_ms, state,
                 recipe_json, response_json, updated_at_ms)
             VALUES ('incarnation', 'session', 1, 'player', 'fingerprint',
                     'owner', 1, 2000, 'active', '{}', '{}', 1000)",
            [],
        )
        .expect("live session");
    assert!(
        measure(&store, 1_000).await.is_empty(),
        "a current session needs no retirement update"
    );

    let updates = measure(&store, 2_000 + TAKEOVER_RECOVERY_MS).await;
    assert!(
        !updates.is_empty(),
        "the eligibility check must not suppress due retirement"
    );
    let state: String = store
        .conn
        .lock()
        .expect("connection")
        .query_row(
            "SELECT state FROM media_sessions WHERE incarnation_id = 'incarnation'",
            [],
            |row| row.get(0),
        )
        .expect("retained session");
    assert_eq!(state, "ended");
}
async fn first_output_seal_fixture(
    rebuilt: bool,
) -> (SqliteStore, crate::store::PreparedOutputSeal) {
    let store = SqliteStore::open_in_memory().expect("capture store");
    store.with_conn(move |conn| {
            conn.execute("INSERT INTO users(id,username,password_hash,is_admin,created_at) VALUES(1,'capture','hash',0,1)", [])?;
            conn.execute("INSERT INTO media_sessions(incarnation_id,session_id,user_id,playback_id,request_fingerprint,owner_node_id,owner_epoch,lease_expires_at_ms,state,recipe_json,response_json,updated_at_ms,recovery_epoch)
                VALUES('00000000-0000-4000-8000-000000000001','old-session',1,'capture-playback',?1,'node',2,9000,'active','{}','{}',10,'epoch')", ["a".repeat(64)])?;
            conn.execute("INSERT INTO media_playback_pointers(user_id,playback_id,current_incarnation_id,updated_at_ms) VALUES(1,'capture-playback','00000000-0000-4000-8000-000000000001',10)", [])?;
            if rebuilt {
                conn.execute_batch("BEGIN IMMEDIATE")?;
                conn.execute_batch(&crate::store::media_session_principal_rebuild_schema())?;
                conn.execute_batch("COMMIT")?;
            }
            Ok(())
        }).await.expect("capture predecessor");
    let preparation = MediaSessionPreparation {
        quality_cancellation_key: None,
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 },
        playback_id: "capture-playback".into(),
        expected_predecessor_incarnation_id: "00000000-0000-4000-8000-000000000001".into(),
        expected_predecessor_owner_node_id: "node".into(),
        expected_predecessor_owner_epoch: 2,
        request_fingerprint: "b".repeat(64),
        owner_node_id: "node".into(),
        recipe_json: r#"{"retained_output_receiver":1,"request":{"presentation":"vod"}}"#.into(),
        response_json: r#"{"prepared_output_capture_pending":true,"height":720}"#.into(),
        media_origin_ms: 0,
        now_ms: 20,
        expected_desired_revision: None,
        deadline_ms: 8000,
    };
    assert!(store
        .prepare_media_session(&preparation)
        .await
        .expect("reserved")
        .is_some());
    let seal = crate::store::PreparedOutputSeal {
        incarnation_id: preparation.incarnation_id,
        session_id: preparation.session_id,
        user_id: 1,
        playback_id: preparation.playback_id,
        owner_node_id: "node".into(),
        owner_epoch: 1,
        expected_recipe_json: preparation.recipe_json,
        expected_response_json: preparation.response_json,
        predecessor_incarnation_id: "00000000-0000-4000-8000-000000000001".into(),
        predecessor_owner_node_id: "node".into(),
        predecessor_owner_epoch: 2,
        deadline_ms: 8000,
        now_ms: 21,
        already_complete: false,
        retained_output: Some(
            serde_json::json!({"artifact_id":uuid::Uuid::new_v4().to_string(),
                "output_identity":"ab".repeat(32),"average_bps":8000,"peak_bps":12000}),
        ),
    };
    (store, seal)
}

#[tokio::test]
async fn first_preparation_output_seal_is_exact_and_idempotent_in_both_layouts() {
    for rebuilt in [false, true] {
        for absence in [false, true] {
            let (store, mut seal) = first_output_seal_fixture(rebuilt).await;
            if absence {
                seal.retained_output = None;
            }
            let predecessor = store
                .media_session_route_by_incarnation("00000000-0000-4000-8000-000000000001")
                .await
                .expect("read")
                .expect("old");
            assert!(store.seal_prepared_output(&seal).await.expect("first seal"));
            let first = store
                .media_session_route_by_incarnation(&seal.incarnation_id)
                .await
                .expect("read")
                .expect("sealed");
            assert!(store
                .seal_prepared_output(&seal)
                .await
                .expect("same seal replay"));
            let original = MediaSessionPreparation {
                quality_cancellation_key: None,
                incarnation_id: seal.incarnation_id.clone(),
                session_id: seal.session_id.clone(),
                principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 },
                playback_id: seal.playback_id.clone(),
                expected_predecessor_incarnation_id: seal.predecessor_incarnation_id.clone(),
                expected_predecessor_owner_node_id: seal.predecessor_owner_node_id.clone(),
                expected_predecessor_owner_epoch: seal.predecessor_owner_epoch,
                request_fingerprint: "b".repeat(64),
                owner_node_id: seal.owner_node_id.clone(),
                recipe_json: seal.expected_recipe_json.clone(),
                response_json: seal.expected_response_json.clone(),
                media_origin_ms: 0,
                now_ms: 20,
                expected_desired_revision: None,
                deadline_ms: 8000,
            };
            assert_eq!(
                store
                    .prepare_media_session(&original)
                    .await
                    .expect("original reservation replay"),
                Some(first.clone())
            );
            let mut altered = original.clone();
            altered.recipe_json =
                r#"{"retained_output_receiver":1,"request":{"presentation":"live"}}"#.into();
            assert!(store
                .prepare_media_session(&altered)
                .await
                .expect("changed original refused")
                .is_none());
            assert_eq!(
                store
                    .media_session_route_by_incarnation(&seal.incarnation_id)
                    .await
                    .expect("read")
                    .expect("replayed"),
                first
            );
            let recipe: serde_json::Value =
                serde_json::from_str(&first.recipe_json).expect("sealed recipe");
            assert_eq!(
                recipe["retained_output"],
                seal.retained_output
                    .clone()
                    .unwrap_or(serde_json::Value::Null)
            );
            let response: serde_json::Value =
                serde_json::from_str(&first.response_json).expect("sealed response");
            assert_eq!(response["prepared_output_capture_complete"], true);
            assert!(response.get("prepared_output_capture_pending").is_none());
            let mut changed = seal.clone();
            changed.retained_output = Some(
                serde_json::json!({"artifact_id":uuid::Uuid::new_v4().to_string(),
                    "output_identity":"cd".repeat(32),"average_bps":9000,"peak_bps":13000}),
            );
            assert!(!store
                .seal_prepared_output(&changed)
                .await
                .expect("changed receipt refused"));
            assert_eq!(
                store
                    .media_session_route_by_incarnation("00000000-0000-4000-8000-000000000001")
                    .await
                    .expect("read")
                    .expect("old"),
                predecessor
            );
            assert_eq!(
                store
                    .staged_media_session_for_playback(&predecessor.principal, "capture-playback")
                    .await
                    .expect("ledger")
                    .expect("still staged")
                    .staged_incarnation_id,
                seal.incarnation_id
            );
        }
    }
}

#[tokio::test]
async fn first_preparation_output_seal_refuses_lost_ownership_and_legacy_absence() {
    let mutations = [
            "UPDATE media_sessions SET owner_epoch=2 WHERE incarnation_id=?1",
            "UPDATE media_sessions SET session_id='changed' WHERE incarnation_id=?1",
            "UPDATE media_sessions SET publication_ready_at_ms=0 WHERE incarnation_id=?1",
            "UPDATE media_sessions SET lease_expires_at_ms=21 WHERE incarnation_id=?1",
            "UPDATE job_leases SET fence=2 WHERE resource='session:'||?1",
            "UPDATE job_leases SET expires_at_ms=21 WHERE resource='session:'||?1",
            "UPDATE media_sessions SET owner_node_id='other' WHERE incarnation_id=?1",
            "DELETE FROM media_session_preparations WHERE staged_incarnation_id=?1",
            "UPDATE media_playback_pointers SET current_incarnation_id=?1 WHERE playback_id='capture-playback'",
            "UPDATE media_sessions SET owner_epoch=3 WHERE incarnation_id='00000000-0000-4000-8000-000000000001' AND ?1 IS NOT NULL",
            "UPDATE media_sessions SET recipe_json='{}' WHERE incarnation_id=?1",
            "UPDATE media_sessions SET response_json='{}' WHERE incarnation_id=?1",
        ];
    for rebuilt in [false, true] {
        for mutation in mutations {
            let (store, seal) = first_output_seal_fixture(rebuilt).await;
            let id = seal.incarnation_id.clone();
            store
                .with_conn(move |conn| {
                    conn.execute(mutation, [id])?;
                    Ok(())
                })
                .await
                .expect("lost fence");
            let before = store
                .media_session_route_by_incarnation(&seal.incarnation_id)
                .await
                .expect("read");
            assert!(
                !store
                    .seal_prepared_output(&seal)
                    .await
                    .expect("CAS refusal"),
                "{mutation}"
            );
            assert_eq!(
                store
                    .media_session_route_by_incarnation(&seal.incarnation_id)
                    .await
                    .expect("read"),
                before
            );
        }
        let (store, mut legacy) = first_output_seal_fixture(rebuilt).await;
        legacy.expected_response_json = "{}".into();
        assert!(
            store.seal_prepared_output(&legacy).await.is_err(),
            "missing pending marker cannot authorize capture"
        );
        let (store, mut stale) = first_output_seal_fixture(rebuilt).await;
        stale.now_ms = stale.deadline_ms;
        assert!(store.seal_prepared_output(&stale).await.is_err());
    }
}
