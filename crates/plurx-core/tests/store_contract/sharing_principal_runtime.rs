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
            assert!(
                owned.is_empty(),
                "Shared actors are outside the Local lease loop"
            );
            let expired = store
                .expired_media_sessions(9001, None, 10)
                .await
                .expect("shared takeover inventory");
            assert_eq!(expired.len(), 2);
            for lease in &expired {
                assert!(matches!(lease.principal, PlaybackPrincipal::Sharing { .. }));
            }
        }
        client
            .execute(
                "DELETE FROM media_session_preparations WHERE staged_incarnation_id='staged'",
                hiqlite::params!(),
            )
            .await
            .expect("release Local staged fixture");
        client
            .execute(
                "UPDATE media_session_requests SET state='resolved' WHERE request_id='request'",
                hiqlite::params!(),
            )
            .await
            .expect("resolve Local fixture");
        let owned = store
            .owned_media_sessions("node", 100)
            .await
            .expect("Local inventory");
        assert_eq!(owned.len(), 1);
        assert_eq!(
            owned[0].principal,
            PlaybackPrincipal::LocalUser { user_id: 1 }
        );
        client
            .execute(
                "UPDATE media_sessions SET recipe_json=$1 WHERE incarnation_id='live'",
                hiqlite::params!("{\"kind\":\"remote_source\"}"),
            )
            .await
            .expect("typed B fixture");
        assert!(store
            .owned_media_sessions("node", 100)
            .await
            .expect("actors excluded")
            .is_empty());
        client
            .execute(
                "UPDATE media_sessions SET recipe_json='{}' WHERE incarnation_id='live'",
                hiqlite::params!(),
            )
            .await
            .expect("restore Local recipe");
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
    // Terminal cleanup must agree with the activation principal, even when
    // a foreign incarnation is already ended at exactly this call's timestamp.
    client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign-storage','foreign-recipe','foreign-generation','media_session',$1,1,9000)", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign settlement pin");
    client.execute("UPDATE media_sessions SET state='ended',terminal_reason='replaced',updated_at_ms=4000 WHERE incarnation_id=$1", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign ended incarnation");
    assert!(store
        .settle_media_session_activation(
            &collision,
            MediaSessionActivationSettlement::Abandon,
            4000
        )
        .await
        .expect("foreign settlement refusal")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT CAST(revision AS TEXT) || ':' || expires_at_ms || ':' || updated_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["3:9000:10"]);
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM cache_consumer_pins WHERE consumer_id='00000000-0000-4000-a000-000000000001'").await, ["1"]);
    assert_eq!(
        store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0
                },
                1000
            )
            .await
            .expect("confirmation replay")
            .expect("confirmed local route")
            .principal,
        local
    );
    let mut pending = activation.clone();
    pending.incarnation_id = "00000000-0000-4000-a000-000000000150".to_owned();
    pending.session_id = "00000000-0000-4000-a000-000000000151".to_owned();
    pending.playback_id = "settlement-runtime".to_owned();
    store
        .activate_media_session(&pending)
        .await
        .expect("pending local activation")
        .expect("pending route");
    assert!(store
        .settle_media_session_activation(&pending, MediaSessionActivationSettlement::Abandon, 2000)
        .await
        .expect("local abandon")
        .is_none());
    assert_eq!(
        store
            .media_session_route(&pending.session_id)
            .await
            .expect("ended local route")
            .expect("retained ended route")
            .state,
        "ended"
    );
    assert!(store
        .media_session_route_for_playback(&local, &pending.playback_id)
        .await
        .expect("removed local pointer")
        .is_none());
    let mut published = pending.clone();
    published.incarnation_id = "00000000-0000-4000-a000-000000000152".to_owned();
    published.session_id = "00000000-0000-4000-a000-000000000153".to_owned();
    published.playback_id = "publication-runtime".to_owned();
    published.request_id = Some("publication-request".to_owned());
    assert!(matches!(
        store
            .claim_media_session_request(
                &local,
                "publication-request",
                &published.request_fingerprint,
                &published.playback_id,
                &published.incarnation_id,
                1000,
                900000
            )
            .await
            .expect("publication claim"),
        MediaSessionRequestClaim::Acquired { .. }
    ));
    store
        .assign_media_session_request_owner(
            &local,
            "publication-request",
            &published.incarnation_id,
            &published.owner_node_id,
            1000,
        )
        .await
        .expect("publication owner");
    store
        .activate_media_session(&published)
        .await
        .expect("publication activation")
        .expect("publication route");
    store
        .settle_media_session_activation(
            &published,
            MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            1000,
        )
        .await
        .expect("publication confirm")
        .expect("confirmed pending request");
    assert_eq!(
        store
            .publish_media_session_activation(
                &local,
                "publication-request",
                &published.incarnation_id,
                1001
            )
            .await
            .expect("publication write")
            .expect("published local route")
            .principal,
        local
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_local_renewal_takeover_refuse_shared_and_deleted_owners() {
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
    let current = "00000000-0000-4000-a000-000000000160";
    let session = "00000000-0000-4000-a000-000000000161";
    current_media_session(
        &store,
        1,
        "renewal-runtime",
        current,
        session,
        "rebuilt renewal",
    )
    .await;
    let renewal = MediaSessionRenewal {
        incarnation_id: current.to_owned(),
        owner_epoch: 1,
        produced_playable_through_ms: 10,
        fetched_through_ms: 10,
        media_sequence: 1,
    };
    assert_eq!(
        store
            .renew_media_sessions("staged-node", std::slice::from_ref(&renewal), 2000, 901000)
            .await
            .expect("local renewal"),
        [current]
    );
    client.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES('session:' || $1,'node',1,3,9000,10)", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign renewal lease");
    client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign-storage','foreign-recipe','foreign-generation','media_session',$1,1,9000)", hiqlite::params!(SHARED_ROUTE)).await.expect("foreign renewal pin");
    let shared_renewal = MediaSessionRenewal {
        incarnation_id: SHARED_ROUTE.to_owned(),
        ..renewal.clone()
    };
    assert!(store
        .renew_media_sessions("node", &[shared_renewal], 2000, 10000)
        .await
        .expect("closed shared renewal")
        .is_empty());
    let shared_takeover = MediaSessionTakeover {
        incarnation_id: SHARED_ROUTE.to_owned(),
        expected_owner_node_id: "node".to_owned(),
        expected_owner_epoch: 1,
        next_owner_node_id: "successor-owner".to_owned(),
        now_ms: 9001,
        lease_expires_at_ms: 10000,
    };
    assert!(store
        .claim_media_session_takeover(&shared_takeover)
        .await
        .expect("closed shared takeover")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT owner_node_id || ':' || fence || ':' || revision || ':' || expires_at_ms || ':' || updated_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["node:1:3:9000:10"]);
    assert_eq!(request_rows(&client, "SELECT CAST(expires_at_ms AS TEXT) AS value FROM cache_consumer_pins WHERE consumer_id='00000000-0000-4000-a000-000000000001'").await, ["9000"]);
    assert_eq!(
        store
            .media_session_route_by_incarnation(SHARED_ROUTE)
            .await
            .expect("shared inventory route")
            .expect("retained shared route")
            .owner_epoch,
        1
    );
    let takeover = MediaSessionTakeover {
        incarnation_id: current.to_owned(),
        expected_owner_node_id: "staged-node".to_owned(),
        expected_owner_epoch: 1,
        next_owner_node_id: "successor-owner".to_owned(),
        now_ms: 901001,
        lease_expires_at_ms: 902000,
    };
    let transferred = store
        .claim_media_session_takeover(&takeover)
        .await
        .expect("local takeover")
        .expect("claimed local route");
    assert_eq!(
        transferred.principal,
        PlaybackPrincipal::LocalUser { user_id: 1 }
    );
    assert_eq!(transferred.owner_epoch, 2);
    client
        .execute("DELETE FROM users WHERE id=1", hiqlite::params!())
        .await
        .expect("delete local owner");
    // Restore a corrupt orphan after the retirement trigger, so absence is
    // tested independently of state='ended' and the zero lease it writes.
    client.execute("UPDATE media_sessions SET state='active',terminal_reason=NULL,lease_expires_at_ms=9000,updated_at_ms=10 WHERE incarnation_id=$1", hiqlite::params!(current)).await.expect("orphan active row fixture");
    client.execute("UPDATE job_leases SET expires_at_ms=9000,revision=3,updated_at_ms=10 WHERE resource='session:' || $1", hiqlite::params!(current)).await.expect("orphan live lease fixture");
    let orphan_renewal = MediaSessionRenewal {
        owner_epoch: 2,
        ..renewal
    };
    assert!(store
        .renew_media_sessions("successor-owner", &[orphan_renewal], 2000, 10000)
        .await
        .expect("deleted local renewal refusal")
        .is_empty());
    let orphan_takeover = MediaSessionTakeover {
        expected_owner_node_id: "successor-owner".to_owned(),
        expected_owner_epoch: 2,
        next_owner_node_id: "last-owner".to_owned(),
        now_ms: 9001,
        lease_expires_at_ms: 10000,
        ..takeover
    };
    assert!(store
        .claim_media_session_takeover(&orphan_takeover)
        .await
        .expect("deleted local takeover refusal")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT owner_node_id || ':' || fence || ':' || revision || ':' || expires_at_ms || ':' || updated_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000160'").await, ["successor-owner:2:3:9000:10"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_cleanup_preserves_foreign_preparations_and_retires_shared_routes() {
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
    let foreign_session = "00000000-0000-4000-a000-000000000171";
    let second = "00000000-0000-4000-a000-000000000002";
    let second_session = "00000000-0000-4000-a000-000000000172";
    client
        .execute(
            "UPDATE media_sessions SET session_id=$1 WHERE incarnation_id=$2",
            hiqlite::params!(foreign_session, SHARED_ROUTE),
        )
        .await
        .expect("valid shared session identity");
    client
        .execute(
            "UPDATE media_sessions SET session_id=$1 WHERE incarnation_id=$2",
            hiqlite::params!(second_session, second),
        )
        .await
        .expect("second shared session identity");
    for incarnation in [SHARED_ROUTE, second] {
        client.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES('session:' || $1,'node',1,3,9000,10)", hiqlite::params!(incarnation)).await.expect("shared cleanup lease");
        client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign-storage','foreign-recipe','foreign-generation','media_session',$1,1,9000)", hiqlite::params!(incarnation)).await.expect("shared cleanup pin");
    }
    client.execute("UPDATE media_session_preparations SET staged_incarnation_id=$1,expected_predecessor_incarnation_id='00000000-0000-4000-a000-000000000170',deadline_ms=2000,updated_at_ms=100 WHERE owner_key='local:1' AND playback_id='playback'", hiqlite::params!(SHARED_ROUTE)).await.expect("corrupt foreign preparation ledger");
    store
        .maintain_media_sessions(3000)
        .await
        .expect("same-owner maintenance sweep");
    assert_eq!(
        store
            .media_session_route_by_incarnation(SHARED_ROUTE)
            .await
            .expect("foreign route after sweep")
            .expect("retained route")
            .state,
        "active"
    );
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM media_session_preparations WHERE owner_key='local:1'").await, ["0"]);
    assert_eq!(request_rows(&client, "SELECT CAST(revision AS TEXT) || ':' || expires_at_ms || ':' || updated_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["3:9000:10"]);
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM cache_consumer_pins WHERE consumer_id='00000000-0000-4000-a000-000000000001'").await, ["1"]);
    client
        .execute(
            "UPDATE media_sessions SET publication_ready_at_ms=$1 WHERE incarnation_id=$2",
            hiqlite::params!(MEDIA_SESSION_PUBLICATION_BLOCKED, SHARED_ROUTE),
        )
        .await
        .expect("unpublished shared fixture");
    assert!(store
        .arm_media_session_handoff(
            SHARED_ROUTE,
            "node",
            1,
            3000 + plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
            3000
        )
        .await
        .expect("closed shared handoff arm")
        .is_none());
    assert!(store
        .complete_media_session_handoff(
            SHARED_ROUTE,
            "node",
            1,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            3000
        )
        .await
        .expect("closed shared handoff completion")
        .is_none());
    assert_eq!(
        store
            .media_session_route_by_incarnation(SHARED_ROUTE)
            .await
            .expect("closed shared projection")
            .expect("retained route")
            .publication_ready_at_ms,
        MEDIA_SESSION_PUBLICATION_BLOCKED
    );
    let local_current = "00000000-0000-4000-a000-000000000174";
    current_media_session(
        &store,
        1,
        "handoff-runtime",
        local_current,
        "00000000-0000-4000-a000-000000000175",
        "local handoff authority",
    )
    .await;
    client
        .execute(
            "UPDATE media_sessions SET publication_ready_at_ms=$1 WHERE incarnation_id=$2",
            hiqlite::params!(MEDIA_SESSION_PUBLICATION_BLOCKED, local_current),
        )
        .await
        .expect("unpublished local fixture");
    assert!(store
        .arm_media_session_handoff(
            local_current,
            "staged-node",
            1,
            3000 + plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
            3000
        )
        .await
        .expect("local handoff arm")
        .is_some());
    assert!(store
        .complete_media_session_handoff(
            local_current,
            "staged-node",
            1,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            3000
        )
        .await
        .expect("local handoff complete")
        .is_some());
    let shared = PlaybackPrincipal::sharing(
        uuid::Uuid::parse_str(SHARED_ROUTE).expect("grant"),
        &"a".repeat(64),
    )
    .expect("shared owner");
    let ended = store
        .end_media_session_if_owner(&MediaSessionEnd {
            incarnation_id: SHARED_ROUTE.to_owned(),
            session_id: foreign_session.to_owned(),
            expected_owner_node_id: "node".to_owned(),
            expected_owner_epoch: 1,
            expected_lease_expires_at_ms: 9000,
            terminal_reason: "replaced".to_owned(),
            now_ms: 4000,
        })
        .await
        .expect("shared exact-owner terminal cleanup")
        .expect("ended shared route");
    assert_eq!(ended.principal, shared);
    assert_eq!(ended.state, "ended");
    assert!(store
        .media_session_route_for_playback(&shared, "playback")
        .await
        .expect("removed shared pointer")
        .is_none());
    assert_eq!(request_rows(&client, "SELECT CAST(expires_at_ms AS TEXT) AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["4000"]);
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM cache_consumer_pins WHERE consumer_id='00000000-0000-4000-a000-000000000001'").await, ["0"]);
    assert!(store
        .arm_media_session_terminal_projection(
            SHARED_ROUTE,
            "node",
            1,
            4000 + plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS,
            4000
        )
        .await
        .expect("shared terminal projection arm")
        .is_some());
    assert!(store
        .complete_media_session_terminal_projection(
            SHARED_ROUTE,
            "node",
            1,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            4000
        )
        .await
        .expect("shared terminal projection completion")
        .is_some());
    let second_ended = store
        .end_media_session(second_session, "replaced", 5000)
        .await
        .expect("shared capability terminal cleanup")
        .expect("second ended shared route");
    assert_eq!(second_ended.state, "ended");
    assert_eq!(
        second_ended.principal.owner_key(),
        format!("share:{second}:{}", "a".repeat(64))
    );
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM media_playback_pointers WHERE principal_kind='sharing'").await, ["0"]);
    client
        .execute("DELETE FROM users WHERE id=1", hiqlite::params!())
        .await
        .expect("remove handoff user");
    client.execute("UPDATE media_sessions SET state='active',terminal_reason=NULL,publication_ready_at_ms=0 WHERE incarnation_id=$1", hiqlite::params!(local_current)).await.expect("orphan already-published local fixture");
    assert!(store
        .complete_media_session_handoff(
            local_current,
            "staged-node",
            1,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            6000
        )
        .await
        .expect("deleted local handoff replay refusal")
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_rebuilt_terminal_ack_and_projection_retire_only_exact_shared_owner() {
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
    let second = "00000000-0000-4000-a000-000000000002";
    let first_session = "00000000-0000-4000-a000-000000000181";
    let second_session = "00000000-0000-4000-a000-000000000182";
    for (incarnation, session) in [(SHARED_ROUTE, first_session), (second, second_session)] {
        client
            .execute(
                "UPDATE media_sessions SET session_id=$1 WHERE incarnation_id=$2",
                hiqlite::params!(session, incarnation),
            )
            .await
            .expect("actual shared session identity");
        client.execute("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES('session:' || $1,'node',1,3,9000,10)", hiqlite::params!(incarnation)).await.expect("terminal lease fixture");
        client.execute("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('foreign-storage','foreign-recipe','foreign-generation','media_session',$1,1,9000)", hiqlite::params!(incarnation)).await.expect("terminal pin fixture");
    }
    let acknowledgement = MediaSessionTerminalAck {
        incarnation_id: SHARED_ROUTE.to_owned(),
        session_id: first_session.to_owned(),
        owner_node_id: "node".to_owned(),
        owner_epoch: 1,
        client_instance_id: "00000000-0000-4000-a000-000000000183".to_owned(),
        sequence: 1,
        request_fingerprint: "e".repeat(64),
        response_json: r#"{"terminal":true}"#.to_owned(),
        expires_at_ms: 1000000,
        updated_at_ms: 3000,
    };
    for invalid in [
        MediaSessionTerminalAck {
            session_id: second_session.to_owned(),
            ..acknowledgement.clone()
        },
        MediaSessionTerminalAck {
            owner_epoch: 2,
            ..acknowledgement.clone()
        },
        MediaSessionTerminalAck {
            owner_node_id: "foreign-node".to_owned(),
            ..acknowledgement.clone()
        },
    ] {
        assert!(!store
            .record_media_session_terminal_ack(&invalid)
            .await
            .expect("exact terminal identity refusal"));
    }
    assert!(store
        .media_session_terminal_ack(first_session, 3000)
        .await
        .expect("no refused ack")
        .is_none());
    assert!(store
        .media_session_terminal_ack(second_session, 3000)
        .await
        .expect("no foreign session ack")
        .is_none());
    assert_eq!(
        store
            .media_session_route_by_incarnation(SHARED_ROUTE)
            .await
            .expect("unmodified route")
            .expect("first shared route")
            .state,
        "active"
    );
    // Corrupt the second grant's pointer, not its canonical metadata. Ending
    // the first incarnation cannot authorize deleting that foreign row.
    client
        .execute(
            "DELETE FROM media_playback_pointers WHERE share_grant_id=$1",
            hiqlite::params!(SHARED_ROUTE),
        )
        .await
        .expect("remove first pointer before unique-incarnation corruption");
    client
        .execute(
            "UPDATE media_playback_pointers SET current_incarnation_id=$1, desired_revision=2 WHERE share_grant_id=$2",
            hiqlite::params!(SHARED_ROUTE, second),
        )
        .await
        .expect("foreign pointer corruption");
    assert!(store
        .record_media_session_terminal_ack(&acknowledgement)
        .await
        .expect("shared terminal ack"));
    let ended = store
        .media_session_route_by_incarnation(SHARED_ROUTE)
        .await
        .expect("terminal route")
        .expect("retained ended route");
    assert_eq!(ended.state, "ended");
    assert_eq!(ended.lease_expires_at_ms, 3000);
    assert_eq!(
        ended.principal.owner_key(),
        format!("share:{SHARED_ROUTE}:{}", "a".repeat(64))
    );
    assert_eq!(request_rows(&client, "SELECT CAST(count(*) AS TEXT) AS value FROM media_playback_pointers WHERE share_grant_id='00000000-0000-4000-a000-000000000001'").await, ["0"]);
    assert_eq!(request_rows(&client, "SELECT current_incarnation_id AS value FROM media_playback_pointers WHERE share_grant_id='00000000-0000-4000-a000-000000000002'").await, [SHARED_ROUTE]);
    assert_eq!(request_rows(&client, "SELECT CAST(revision AS TEXT) || ':' || expires_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000001'").await, ["4:3000"]);
    assert_eq!(request_rows(&client, "SELECT CAST(revision AS TEXT) || ':' || expires_at_ms AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000002'").await, ["3:9000"]);
    assert_eq!(request_rows(&client, "SELECT consumer_id AS value FROM cache_consumer_pins WHERE consumer_kind='media_session' ORDER BY consumer_id").await, [second]);
    assert_eq!(
        store
            .media_session_route_by_incarnation(second)
            .await
            .expect("foreign route isolation")
            .expect("second route")
            .state,
        "active"
    );
    client
        .execute(
            "UPDATE media_playback_pointers SET current_incarnation_id=$1 WHERE share_grant_id=$1",
            hiqlite::params!(second),
        )
        .await
        .expect("restore foreign pointer for independent cleanup");
    store
        .maintain_media_sessions(4000)
        .await
        .expect("terminal cleanup");
    assert_eq!(
        store
            .media_session_terminal_ack(first_session, 4000)
            .await
            .expect("retained terminal response"),
        Some(acknowledgement.clone())
    );
    assert!(store
        .record_media_session_terminal_ack(&acknowledgement)
        .await
        .expect("terminal ack replay after cleanup"));
    let conflicting = MediaSessionTerminalAck {
        sequence: 2,
        response_json: "{}".to_owned(),
        ..acknowledgement.clone()
    };
    assert!(!store
        .record_media_session_terminal_ack(&conflicting)
        .await
        .expect("terminal receipt conflict"));
    let second_ended = store
        .end_media_session(second_session, "replaced", 5000)
        .await
        .expect("second owner terminal retirement")
        .expect("second ended route");
    assert_eq!(second_ended.state, "ended");
    let safe_at = 5000 + plurx_core::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS;
    assert!(store
        .arm_media_session_terminal_projection(second, "foreign-node", 1, safe_at, 5000)
        .await
        .expect("wrong terminal projection node")
        .is_none());
    assert!(store
        .arm_media_session_terminal_projection(second, "node", 2, safe_at, 5000)
        .await
        .expect("wrong terminal projection epoch")
        .is_none());
    assert!(store
        .arm_media_session_terminal_projection(second, "node", 1, safe_at, 5000)
        .await
        .expect("shared terminal projection arm")
        .is_some());
    let wrong_epoch = store
        .complete_media_session_terminal_projection(
            second,
            "node",
            2,
            MediaSessionProjectionCompletion::PredecessorAcknowledged,
            5000,
        )
        .await
        .expect("wrong completion epoch");
    assert!(wrong_epoch.is_none());
    let completed = store
        .complete_media_session_terminal_projection(
            second,
            "node",
            1,
            MediaSessionProjectionCompletion::SafetyBoundaryElapsed {
                expected_not_before_ms: safe_at,
            },
            safe_at,
        )
        .await
        .expect("shared terminal projection completion")
        .expect("terminal projection completed");
    assert_eq!(completed.state, "ended");
    assert_eq!(completed.publication_ready_at_ms, 0);
    assert_eq!(completed.lease_expires_at_ms, 5000);
    assert_eq!(
        completed.principal.owner_key(),
        format!("share:{second}:{}", "a".repeat(64))
    );
    assert_eq!(request_rows(&client, "SELECT CAST(expires_at_ms AS TEXT) AS value FROM job_leases WHERE resource='session:00000000-0000-4000-a000-000000000002'").await, ["5000"]);
    assert_eq!(
        store
            .media_session_terminal_ack(first_session, safe_at)
            .await
            .expect("other grant retained ack"),
        Some(acknowledgement)
    );
}
