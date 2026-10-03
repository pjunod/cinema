//! Replicated application of the candidate rebuild, before runtime admission.
use super::*;
use plurx_core::store::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA;

const TABLES: [&str; 11] = [
    "media_session_requests",
    "media_playback_pointers",
    "media_sessions",
    "media_session_preparations",
    "media_playback_desired",
    "media_session_producer_recovery",
    "library_channel_session_recipes",
    "job_leases",
    "media_session_terminal_acks",
    "sharing_relay_upstream",
    "sharing_delivery_grants",
];

async fn text_rows(client: &Client, sql: String) -> Vec<String> {
    client
        .query_consistent_map::<SchemaText, _>(sql, hiqlite::params!())
        .await
        .expect("consistent migration probe")
        .into_iter()
        .map(|r| r.value)
        .collect()
}
async fn snapshot(client: &Client, table: &str, columns: &[String]) -> Vec<String> {
    let fields = columns
        .iter()
        .map(|c| format!("'{c}', {c}"))
        .collect::<Vec<_>>()
        .join(", ");
    text_rows(
        client,
        format!("SELECT json_object({fields}) AS value FROM {table} ORDER BY 1"),
    )
    .await
}
fn rebuild_statements() -> Vec<(String, hiqlite::Params)> {
    MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
        .split("-- next statement\n")
        .map(|sql| {
            (
                sql.trim().trim_end_matches(';').to_owned(),
                hiqlite::params!(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_principal_rebuild_is_atomic_and_preserves_rows_on_three_voters() {
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
    .expect("connect migration client");
    for result in client
        .batch(include_str!("../fixtures/session-principal-local.sql"))
        .await
        .expect("submit populated fixture")
    {
        result.expect("populate actual replicated tables");
    }
    let mut before = Vec::new();
    for table in TABLES {
        let cols = text_rows(
            &client,
            format!("SELECT name AS value FROM pragma_table_info('{table}') ORDER BY cid"),
        )
        .await;
        before.push((table, cols.clone(), snapshot(&client, table, &cols).await));
    }
    // A failing final statement must roll back all earlier DDL and backfills.
    let mut failing = rebuild_statements();
    failing.push((
        "INSERT INTO media_session_requests (user_id) VALUES (1)".to_owned(),
        hiqlite::params!(),
    ));
    let failed = client.txn(failing).await;
    assert!(
        failed.is_err()
            || failed
                .as_ref()
                .is_ok_and(|rows| rows.iter().any(Result::is_err)),
        "legacy omission must refuse the complete transaction"
    );
    for (table, cols, rows) in &before {
        assert_eq!(
            &snapshot(&client, table, cols).await,
            rows,
            "{table}: failed rebuild leaves original rows"
        );
    }
    assert!(text_rows(&client, "SELECT name AS value FROM pragma_table_info('media_sessions') WHERE name = 'owner_key'".to_owned()).await.is_empty(),
        "failed rebuild must not leave new columns installed");
    for result in client
        .txn(rebuild_statements())
        .await
        .expect("atomic replicated rebuild")
    {
        result.expect("rebuild statement");
    }
    for (table, cols, rows) in &before {
        assert_eq!(
            &snapshot(&client, table, cols).await,
            rows,
            "{table}: successful rebuild retains every old column"
        );
    }
    let local = text_rows(
        &client,
        "SELECT owner_key AS value FROM media_sessions ORDER BY incarnation_id".to_owned(),
    )
    .await;
    assert_eq!(local, ["local:1", "local:1"]);
    let owner = "share:00000000-0000-4000-a000-000000000001:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    let insert = "INSERT INTO media_session_requests
       (owner_key, principal_kind, share_grant_id, share_viewer_key, request_id,
        request_fingerprint, playback_id, state, claim_expires_at_ms, incarnation_id, updated_at_ms)
       VALUES ($1, 'sharing', $2, $3, 'request', 'fingerprint', 'playback', 'starting',
               9000, 'shared-live', 10)";
    client
        .execute(
            insert,
            hiqlite::params!(
                owner,
                "00000000-0000-4000-a000-000000000001",
                "d".repeat(64)
            ),
        )
        .await
        .expect("sharing request cannot collide with same local request id");
    assert_eq!(
        text_rows(
            &client,
            "SELECT owner_key AS value FROM media_session_requests ORDER BY owner_key".to_owned()
        )
        .await,
        ["local:1", owner]
    );
    let old_insert = "INSERT INTO media_session_requests
      (user_id, request_id, request_fingerprint, playback_id, state, claim_expires_at_ms,
       incarnation_id, updated_at_ms) VALUES (1, 'legacy', 'fp', 'legacy', 'starting', 9000, 'legacy', 10)";
    assert!(
        client
            .execute(old_insert, hiqlite::params!())
            .await
            .is_err(),
        "old writer omitting owner key is incompatible"
    );
    assert!(client.execute(format!("{old_insert} ON CONFLICT(user_id, request_id) DO UPDATE SET updated_at_ms = excluded.updated_at_ms"), hiqlite::params!()).await.is_err(), "old user conflict target is incompatible");
    client
        .execute("DELETE FROM users WHERE id = 1", hiqlite::params!())
        .await
        .expect("delete owner without FK retention block");
    assert_eq!(
        text_rows(
            &client,
            "SELECT incarnation_id AS value FROM media_sessions ORDER BY incarnation_id".to_owned()
        )
        .await,
        ["ended", "live"]
    );
    assert_eq!(
        text_rows(
            &client,
            "SELECT session_id AS value FROM media_session_terminal_acks".to_owned()
        )
        .await,
        ["ended-session"]
    );
    // No normal Store call is made after this candidate-only rebuild: its
    // current runtime intentionally still has the old ownership signatures.
    drop(store);
}
