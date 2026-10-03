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
