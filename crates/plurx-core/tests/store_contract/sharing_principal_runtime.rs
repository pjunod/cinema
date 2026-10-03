//! Production writers against the candidate layout, before schema installation.
use super::*;
use plurx_core::playback_principal::PlaybackPrincipal;
use plurx_core::store::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA;

const REQUEST_INCARNATION: &str = "00000000-0000-4000-a000-000000000080";
const RETRY_INCARNATION: &str = "00000000-0000-4000-a000-000000000081";
const SHARED_ROUTE: &str = "00000000-0000-4000-a000-000000000001";

async fn request_rows(client: &Client, sql: &str) -> Vec<String> {
    client
        .query_consistent_map::<SchemaText, _>(sql.to_owned(), hiqlite::params!())
        .await
        .expect("quorum request projection")
        .into_iter()
        .map(|row| row.value)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_local_request_writes_preserve_owner_and_refuse_cross_principal_replay() {
    let _case = HIQLITE_CASE.lock().await;
    for rebuilt in [false, true] {
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
        .expect("fixture client");
        for result in client
            .batch(include_str!("../fixtures/session-principal-local.sql"))
            .await
            .expect("local retention fixture")
        {
            result.expect("seed local rows");
        }
        if rebuilt {
            let statements: Vec<(String, hiqlite::Params)> = MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
                .split("-- next statement\n")
                .map(|sql| {
                    (
                        sql.trim().trim_end_matches(';').to_owned(),
                        hiqlite::params!(),
                    )
                })
                .collect();
            for result in client.txn(statements).await.expect("candidate transaction") {
                result.expect("candidate rebuild");
            }
            for result in client
                .batch(include_str!("../fixtures/session-principal-sharing.sql"))
                .await
                .expect("independent shared owners")
            {
                result.expect("seed two shared principals");
            }
        }
        if rebuilt {
            let owned = store
                .owned_media_sessions("node", 100)
                .await
                .expect("shared owned inventory");
            assert_eq!(owned.len(), 2);
            for (index, lease) in owned.iter().enumerate() {
                let grant = format!("00000000-0000-4000-a000-{:012}", index + 1);
                let expected = PlaybackPrincipal::sharing(
                    uuid::Uuid::parse_str(&grant).expect("grant UUID"),
                    &"a".repeat(64),
                )
                .expect("shared principal");
                assert_eq!(lease.principal, expected);
            }
            let expired = store
                .expired_media_sessions(9001, None, 10)
                .await
                .expect("shared takeover inventory");
            assert_eq!(expired.len(), 2);
            assert_eq!(expired[0].principal, owned[0].principal);
            assert_eq!(expired[1].principal, owned[1].principal);
        }
        let local = PlaybackPrincipal::LocalUser { user_id: 1 };
        let first_desired = store
            .record_desired_selection(
                &local,
                "runtime-playback",
                &"d".repeat(64),
                "v1;quality=auto",
                100,
            )
            .await
            .expect("production desired insertion");
        assert_eq!(first_desired.principal, local);
        assert_eq!(first_desired.revision, 1);
        let changed_desired = store
            .record_desired_selection(
                &local,
                "runtime-playback",
                &"e".repeat(64),
                "v1;quality=high",
                110,
            )
            .await
            .expect("production desired update");
        assert_eq!(changed_desired.revision, 2);
        let unchanged_desired = store
            .record_desired_selection(
                &local,
                "runtime-playback",
                &"e".repeat(64),
                "v1;quality=high",
                120,
            )
            .await
            .expect("production desired replay");
        assert_eq!(unchanged_desired, changed_desired);
        let recovery = recovery_request("runtime-epoch");
        let reserved = store
            .reserve_producer_recovery(&recovery, 100)
            .await
            .expect("production recovery reservation")
            .expect("first recovery budget");
        assert_eq!(reserved.principal, local);
        assert_eq!(
            store
                .reserve_producer_recovery(&recovery, 110)
                .await
                .expect("recovery replay")
                .expect("replayed budget"),
            reserved
        );
        let settled = store
            .settle_producer_recovery(
                &local,
                &recovery.playback_id,
                &recovery.recovery_epoch,
                &recovery.failed_incarnation_id,
                plurx_core::domain::ProducerRecoveryState::Installed,
                120,
            )
            .await
            .expect("production recovery settlement")
            .expect("settled recovery");
        assert_eq!(settled.principal, local);
        assert!(store
            .reserve_producer_recovery(&recovery, 130)
            .await
            .expect("spent budget refusal")
            .is_none());
        let fingerprint = "c".repeat(64);
        let claim = store
            .claim_media_session_request(
                &local,
                "runtime-request",
                &fingerprint,
                "runtime-playback",
                REQUEST_INCARNATION,
                100,
                9000,
            )
            .await
            .expect("production claim");
        assert!(matches!(claim, MediaSessionRequestClaim::Acquired { .. }));
        assert!(store
            .record_library_channel_session_recipe(
                &local,
                "runtime-request",
                REQUEST_INCARNATION,
                "{}",
                110
            )
            .await
            .expect("production recipe write"));
        assert!(store
            .assign_media_session_request_owner(
                &local,
                "runtime-request",
                REQUEST_INCARNATION,
                "producer",
                120
            )
            .await
            .expect("production owner assignment"));
        assert!(store
            .fail_media_session_request(&local, "runtime-request", REQUEST_INCARNATION, 130)
            .await
            .expect("production failure settlement"));
        let retry = store
            .claim_media_session_request(
                &local,
                "runtime-request",
                &fingerprint,
                "runtime-playback",
                RETRY_INCARNATION,
                140,
                9000,
            )
            .await
            .expect("production reacquire");
        assert!(matches!(retry, MediaSessionRequestClaim::Acquired { .. }));
        assert!(!store
            .record_library_channel_session_recipe(
                &local,
                "runtime-request",
                RETRY_INCARNATION,
                "{}",
                150
            )
            .await
            .expect("recipe after retry"));
        if rebuilt {
            assert_eq!(request_rows(&client, "SELECT owner_key || ':' || principal_kind || ':' || user_id AS value FROM media_session_requests WHERE request_id='runtime-request'").await, ["local:1:local:1"]);
            assert_eq!(request_rows(&client, "SELECT owner_key || ':' || principal_kind || ':' || user_id AS value FROM library_channel_session_recipes WHERE request_id='runtime-request'").await, ["local:1:local:1"]);
            assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM media_session_requests WHERE request_id='runtime-request' AND share_grant_id IS NULL AND share_viewer_key IS NULL").await, ["1"]);
            let shared = PlaybackPrincipal::sharing(
                uuid::Uuid::parse_str(SHARED_ROUTE).expect("grant UUID"),
                &"a".repeat(64),
            )
            .expect("sharing principal");
            client.execute("INSERT INTO media_session_producer_recovery(owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,playback_id,recovery_epoch,failed_incarnation_id,failed_producer_attempt,decision_sequence,failed_plan_digest,alternate_plan_digest,decode_restriction,state,created_at_ms,updated_at_ms) SELECT owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,playback_id,'shared-epoch',incarnation_id,1,1,$1,$2,NULL,'reserved',100,100 FROM media_sessions WHERE principal_kind='sharing'", hiqlite::params!("b".repeat(64), "c".repeat(64))).await.expect("two independent shared recovery fixture rows");
            for grant_id in 1..=2 {
                let grant = format!("00000000-0000-4000-a000-{grant_id:012}");
                let principal = PlaybackPrincipal::sharing(
                    uuid::Uuid::parse_str(&grant).expect("grant UUID"),
                    &"a".repeat(64),
                )
                .expect("principal");
                let ledger = store
                    .producer_recovery_for_epoch(&principal, "playback", "shared-epoch")
                    .await
                    .expect("complete shared recovery reader")
                    .expect("shared ledger");
                assert_eq!(ledger.principal, principal);
                assert_eq!(ledger.failed_incarnation_id, grant);
                let mut refused = recovery.clone();
                refused.principal = principal.clone();
                assert!(store
                    .reserve_producer_recovery(&refused, 160)
                    .await
                    .is_err());
                assert!(store
                    .settle_producer_recovery(
                        &principal,
                        "playback",
                        "shared-epoch",
                        &grant,
                        plurx_core::domain::ProducerRecoveryState::Installed,
                        160
                    )
                    .await
                    .is_err());
            }
            assert!(
                store
                    .claim_media_session_request(
                        &shared,
                        "not-admitted",
                        &fingerprint,
                        "runtime-playback",
                        REQUEST_INCARNATION,
                        160,
                        9000
                    )
                    .await
                    .is_err(),
                "new schema shape is not authority to admit a shared worker"
            );
            client.execute("UPDATE media_session_requests SET state='resolved', incarnation_id=$1, response_json='{}' WHERE owner_key='local:1' AND request_id='runtime-request'",
                hiqlite::params!(SHARED_ROUTE)).await.expect("corrupt cross-principal route fixture");
            let replay = store
                .claim_media_session_request(
                    &local,
                    "runtime-request",
                    &fingerprint,
                    "runtime-playback",
                    RETRY_INCARNATION,
                    170,
                    9000,
                )
                .await
                .expect("closed replay check");
            assert!(
                matches!(replay, MediaSessionRequestClaim::Conflict),
                "a local request cannot replay another principal's valid route"
            );
            let missing = PlaybackPrincipal::LocalUser { user_id: 2 };
            assert!(store
                .record_desired_selection(
                    &missing,
                    "absent-playback",
                    &"d".repeat(64),
                    "v1;quality=auto",
                    180
                )
                .await
                .is_err());
            let mut absent_recovery = recovery_request("absent-epoch");
            absent_recovery.principal = missing.clone();
            assert!(store
                .reserve_producer_recovery(&absent_recovery, 180)
                .await
                .expect("absent recovery owner refusal")
                .is_none());
            let missing_claim = store
                .claim_media_session_request(
                    &missing,
                    "absent-owner",
                    &fingerprint,
                    "absent-playback",
                    REQUEST_INCARNATION,
                    180,
                    9000,
                )
                .await
                .expect("missing owner refusal");
            assert!(matches!(
                missing_claim,
                MediaSessionRequestClaim::Overloaded
            ));
            assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM media_session_requests WHERE request_id='absent-owner'").await, ["0"]);
            client.execute("UPDATE media_session_requests SET state='failed', incarnation_id=$1 WHERE owner_key='local:1' AND request_id='runtime-request'",
                hiqlite::params!(RETRY_INCARNATION)).await.expect("retryable original request");
            client
                .execute("DELETE FROM users WHERE id=1", hiqlite::params!())
                .await
                .expect("delete local authority");
            assert!(store
                .record_desired_selection(
                    &local,
                    "runtime-playback",
                    &"f".repeat(64),
                    "v1;quality=low",
                    190
                )
                .await
                .is_err());
            assert!(store
                .desired_selection(&local, "runtime-playback")
                .await
                .expect("deleted desired inventory")
                .is_none());
            assert!(store
                .reserve_producer_recovery(&recovery_request("deleted-epoch"), 190)
                .await
                .expect("deleted recovery owner refusal")
                .is_none());
            let deleted = store
                .claim_media_session_request(
                    &local,
                    "runtime-request",
                    &fingerprint,
                    "runtime-playback",
                    REQUEST_INCARNATION,
                    190,
                    9000,
                )
                .await
                .expect("deleted owner refusal");
            assert!(matches!(deleted, MediaSessionRequestClaim::Overloaded));
            assert!(!store
                .record_library_channel_session_recipe(
                    &local,
                    "runtime-request",
                    REQUEST_INCARNATION,
                    "{}",
                    200
                )
                .await
                .expect("deleted owner recipe refusal"));
            assert!(!store
                .assign_media_session_request_owner(
                    &local,
                    "runtime-request",
                    REQUEST_INCARNATION,
                    "producer",
                    200
                )
                .await
                .expect("deleted owner assignment refusal"));
            assert_eq!(request_rows(&client, "SELECT state AS value FROM media_session_requests WHERE owner_key='local:1' AND request_id='runtime-request'").await, ["failed"]);
        }
        drop(store);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_local_preparation_rejoin_abort_preserve_principal_fences() {
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
    .expect("fixture client");
    for result in client
        .batch(include_str!("../fixtures/session-principal-local.sql"))
        .await
        .expect("local fixture")
    {
        result.expect("seed local fixture");
    }
    let current = "00000000-0000-4000-a000-000000000090";
    current_media_session(
        &store,
        1,
        "prepared-runtime",
        current,
        "00000000-0000-4000-a000-000000000091",
        "candidate preparation",
    )
    .await;
    let statements: Vec<(String, hiqlite::Params)> = MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
        .split("-- next statement\n")
        .map(|sql| {
            (
                sql.trim().trim_end_matches(';').to_owned(),
                hiqlite::params!(),
            )
        })
        .collect();
    for result in client.txn(statements).await.expect("candidate transaction") {
        result.expect("candidate rebuild");
    }
    for result in client
        .batch(include_str!("../fixtures/session-principal-sharing.sql"))
        .await
        .expect("shared fixture")
    {
        result.expect("seed shared owners");
    }
    // The schema projection is fixed for one Store lifetime. Reopen after
    // the fixture rebuild, as the coordinated upgrade path requires.
    drop(store);
    let store = HiqliteAuthStore::open(
        client.clone(),
        &cluster._root.path().join("candidate-reopened-telemetry.db"),
    )
    .await
    .expect("reopen rebuilt fixture store");
    let local = PlaybackPrincipal::LocalUser { user_id: 1 };
    let first = staged_preparation(
        1,
        "prepared-runtime",
        "00000000-0000-4000-a000-000000000092",
        "00000000-0000-4000-a000-000000000093",
        current,
    );
    let staged = store
        .prepare_media_session(&first)
        .await
        .expect("rebuilt production prepare")
        .expect("staged successor");
    assert_eq!(staged.principal, local);
    assert_eq!(
        store
            .media_session_route_for_playback(&local, "prepared-runtime")
            .await
            .expect("unchanged current pointer")
            .expect("current route")
            .incarnation_id,
        current
    );
    assert_eq!(request_rows(&client, "SELECT owner_key || ':' || principal_kind || ':' || user_id AS value FROM media_session_preparations WHERE playback_id='prepared-runtime'").await, ["local:1:local:1"]);
    assert_eq!(
        store
            .prepare_media_session(&first)
            .await
            .expect("exact staged replay")
            .expect("replayed staged route")
            .incarnation_id,
        first.incarnation_id
    );
    let second = staged_preparation(
        1,
        "prepared-runtime",
        "00000000-0000-4000-a000-000000000094",
        "00000000-0000-4000-a000-000000000095",
        current,
    );
    let rejoined = store
        .rejoin_media_session_preparation(&first.incarnation_id, &second)
        .await
        .expect("rebuilt rejoin")
        .expect("replacement staged successor");
    assert_eq!(rejoined.principal, local);
    assert_eq!(rejoined.incarnation_id, second.incarnation_id);
    assert_eq!(
        store
            .staged_media_session_for_playback(&local, "prepared-runtime")
            .await
            .expect("canonical staged reader")
            .expect("replacement ledger")
            .staged_incarnation_id,
        second.incarnation_id
    );
    let ended = store
        .abort_media_session_preparation(
            &local,
            "prepared-runtime",
            &preparation_abort_request(&second.incarnation_id, 3000),
        )
        .await
        .expect("rebuilt abort")
        .expect("ended staged successor");
    assert_eq!(ended.principal, local);
    assert_eq!(ended.terminal_reason.as_deref(), Some("replaced"));
    assert!(store
        .staged_media_session_for_playback(&local, "prepared-runtime")
        .await
        .expect("released ledger")
        .is_none());
    assert_eq!(
        store
            .media_session_route_for_playback(&local, "prepared-runtime")
            .await
            .expect("current pointer after abort")
            .expect("current route")
            .incarnation_id,
        current
    );
    client.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES('session:' || $1,'node',1,3,9000,10)", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign job lease fixture");
    client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign-storage','foreign-recipe','foreign-generation','media_session',$1,1,9000)", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign cache pin fixture");
    client.execute("INSERT INTO media_session_preparations(owner_key,principal_kind,user_id,playback_id,staged_incarnation_id,expected_predecessor_incarnation_id,deadline_ms,created_at_ms,updated_at_ms) VALUES('local:1','local',1,'prepared-runtime',$1,$2,800000,3000,3000)", hiqlite::params!(SHARED_ROUTE, current)).await.expect("corrupted cross-principal ledger fixture");
    assert!(store
        .abort_media_session_preparation(
            &local,
            "prepared-runtime",
            &preparation_abort_request(SHARED_ROUTE, 3100)
        )
        .await
        .expect("closed foreign abort")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT state AS value FROM media_sessions WHERE incarnation_id='00000000-0000-4000-a000-000000000001'").await, ["active"]);
    assert_eq!(request_rows(&client, "SELECT CAST(lease_expires_at_ms AS TEXT) AS value FROM media_sessions WHERE incarnation_id='00000000-0000-4000-a000-000000000001'").await, ["9000"]);
    assert_eq!(request_rows(&client, "SELECT CAST(revision AS TEXT) || ':' || expires_at_ms || ':' || updated_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["3:9000:10"]);
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM cache_consumer_pins WHERE consumer_kind='media_session' AND consumer_id='00000000-0000-4000-a000-000000000001' AND consumer_epoch=1 AND expires_at_ms=9000").await, ["1"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_local_activation_preserves_owner_and_foreign_lease() {
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
    .expect("fixture client");
    for result in client
        .batch(include_str!("../fixtures/session-principal-local.sql"))
        .await
        .expect("local fixture")
    {
        result.expect("local seed");
    }
    let statements: Vec<(String, hiqlite::Params)> = MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
        .split("-- next statement\n")
        .map(|sql| {
            (
                sql.trim().trim_end_matches(';').to_owned(),
                hiqlite::params!(),
            )
        })
        .collect();
    for result in client.txn(statements).await.expect("candidate transaction") {
        result.expect("candidate rebuild");
    }
    for result in client
        .batch(include_str!("../fixtures/session-principal-sharing.sql"))
        .await
        .expect("shared fixture")
    {
        result.expect("shared seed");
    }
    let activation = current_media_session(
        &store,
        1,
        "activation-runtime",
        "00000000-0000-4000-a000-000000000100",
        "00000000-0000-4000-a000-000000000101",
        "rebuilt activation",
    )
    .await;
    let local = PlaybackPrincipal::LocalUser { user_id: 1 };
    let replay = store
        .activate_media_session(&activation)
        .await
        .expect("exact activation replay")
        .expect("owned activation outcome");
    assert_eq!(replay.route.principal, local);
    assert_eq!(replay.route.publication_ready_at_ms, 0);
    assert_eq!(request_rows(&client, "SELECT owner_key || ':' || principal_kind || ':' || user_id AS value FROM media_sessions WHERE playback_id='activation-runtime'").await, ["local:1:local:1"]);
    assert_eq!(request_rows(&client, "SELECT owner_key || ':' || principal_kind || ':' || user_id AS value FROM media_playback_pointers WHERE playback_id='activation-runtime'").await, ["local:1:local:1"]);
    client.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES('session:' || $1,'node',1,3,9000,10)", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign job lease");
    let mut collision = activation.clone();
    collision.incarnation_id = SHARED_ROUTE.to_owned();
    collision.session_id = "00000000-0000-4000-a000-000000000102".to_owned();
    collision.owner_node_id = "node".to_owned();
    collision.now_ms = 2000;
    assert!(store
        .activate_media_session(&collision)
        .await
        .expect("foreign incarnation refusal")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT CAST(revision AS TEXT) || ':' || expires_at_ms || ':' || updated_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["3:9000:10"]);
    assert_eq!(request_rows(&client, "SELECT state AS value FROM media_sessions WHERE incarnation_id='00000000-0000-4000-a000-000000000001'").await, ["active"]);
    let mut missing = activation.clone();
    missing.principal = PlaybackPrincipal::LocalUser { user_id: 2 };
    missing.incarnation_id = "00000000-0000-4000-a000-000000000103".to_owned();
    missing.session_id = "00000000-0000-4000-a000-000000000104".to_owned();
    assert!(store
        .activate_media_session(&missing)
        .await
        .expect("missing activation owner refusal")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000103'").await, ["0"]);
    missing.principal = PlaybackPrincipal::sharing(
        uuid::Uuid::parse_str(SHARED_ROUTE).expect("grant"),
        &"a".repeat(64),
    )
    .expect("shared principal");
    assert!(store.activate_media_session(&missing).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_local_commit_binds_receipt_to_actual_predecessor_session() {
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
    .expect("fixture client");
    for result in client
        .batch(include_str!("../fixtures/session-principal-local.sql"))
        .await
        .expect("local fixture")
    {
        result.expect("local seed");
    }
    let statements: Vec<(String, hiqlite::Params)> = MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
        .split("-- next statement\n")
        .map(|sql| {
            (
                sql.trim().trim_end_matches(';').to_owned(),
                hiqlite::params!(),
            )
        })
        .collect();
    for result in client.txn(statements).await.expect("candidate transaction") {
        result.expect("candidate rebuild");
    }
    for result in client
        .batch(include_str!("../fixtures/session-principal-sharing.sql"))
        .await
        .expect("shared fixture")
    {
        result.expect("shared seed");
    }
    let current = "00000000-0000-4000-a000-000000000120";
    let current_session = "00000000-0000-4000-a000-000000000121";
    let foreign_session = "00000000-0000-4000-a000-000000000127";
    client
        .execute(
            "UPDATE media_sessions SET session_id=$1 WHERE incarnation_id=$2",
            hiqlite::params!(foreign_session, SHARED_ROUTE),
        )
        .await
        .expect("actual foreign session UUID fixture");
    current_media_session(
        &store,
        1,
        "commit-runtime",
        current,
        current_session,
        "rebuilt commit",
    )
    .await;
    let local = PlaybackPrincipal::LocalUser { user_id: 1 };
    let refused = staged_preparation(
        1,
        "commit-runtime",
        "00000000-0000-4000-a000-000000000122",
        "00000000-0000-4000-a000-000000000123",
        current,
    );
    store
        .prepare_media_session(&refused)
        .await
        .expect("first candidate prepare")
        .expect("staged candidate");
    let receipt = MediaSessionTerminalAck {
        incarnation_id: current.to_owned(),
        session_id: current_session.to_owned(),
        owner_node_id: "staged-node".to_owned(),
        owner_epoch: 1,
        client_instance_id: "00000000-0000-4000-a000-000000000128".to_owned(),
        sequence: 1,
        request_fingerprint: "f".repeat(64),
        response_json: "{}".to_owned(),
        expires_at_ms: 900000,
        updated_at_ms: 3000,
    };
    let mut wrong = receipt.clone();
    wrong.session_id = foreign_session.to_owned();
    let mut refused_commit = preparation_commit_request(&refused.incarnation_id, 3000, 900000);
    refused_commit.control_receipt = Some(wrong);
    assert!(store
        .commit_media_session_preparation(&local, "commit-runtime", &refused_commit)
        .await
        .expect("wrong-session receipt refusal")
        .is_none());
    assert_eq!(
        store
            .media_session_route_for_playback(&local, "commit-runtime")
            .await
            .expect("unadvanced pointer")
            .expect("current route")
            .incarnation_id,
        current
    );
    assert!(store
        .media_session_terminal_ack(foreign_session, 3100)
        .await
        .expect("no foreign receipt")
        .is_none());
    let prepared = staged_preparation(
        1,
        "commit-runtime",
        "00000000-0000-4000-a000-000000000124",
        "00000000-0000-4000-a000-000000000125",
        current,
    );
    store
        .prepare_media_session(&prepared)
        .await
        .expect("second candidate prepare")
        .expect("replacement candidate");
    let mut commit = preparation_commit_request(&prepared.incarnation_id, 3000, 900000);
    commit.control_receipt = Some(receipt.clone());
    let committed = store
        .commit_media_session_preparation(&local, "commit-runtime", &commit)
        .await
        .expect("correct receipt commit")
        .expect("committed successor");
    assert_eq!(committed.route.principal, local);
    assert_eq!(committed.route.incarnation_id, prepared.incarnation_id);
    let predecessor = committed.predecessor.expect("same-principal predecessor");
    assert_eq!(predecessor.principal, local);
    assert_eq!(predecessor.state, "active");
    assert!(predecessor.drain_deadline_ms.is_some());
    assert_eq!(committed.control_receipt, Some(receipt.clone()));
    assert!(store
        .staged_media_session_for_playback(&local, "commit-runtime")
        .await
        .expect("consumed ledger")
        .is_none());
    let replay = store
        .commit_media_session_preparation(&local, "commit-runtime", &commit)
        .await
        .expect("exact commit replay")
        .expect("replayed committed successor");
    assert_eq!(replay.route.principal, local);
    assert_eq!(replay.route.incarnation_id, prepared.incarnation_id);
    assert_eq!(replay.control_receipt, Some(receipt));
    client.execute("INSERT INTO media_session_preparations(owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,playback_id,staged_incarnation_id,expected_predecessor_incarnation_id,deadline_ms,created_at_ms,updated_at_ms) SELECT owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,'shared-staged',incarnation_id,$1,9000,100,100 FROM media_sessions WHERE principal_kind='sharing'", hiqlite::params!("00000000-0000-4000-a000-000000000129")).await.expect("independent stored shared preparation projections");
    for grant_id in 1..=2 {
        let grant = format!("00000000-0000-4000-a000-{grant_id:012}");
        let principal = PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str(&grant).expect("grant"),
            &"a".repeat(64),
        )
        .expect("principal");
        let staged = store
            .staged_media_session_for_playback(&principal, "shared-staged")
            .await
            .expect("shared staged principal reader")
            .expect("shared staged row");
        assert_eq!(staged.principal, principal);
        assert_eq!(staged.staged_incarnation_id, grant);
    }
}
