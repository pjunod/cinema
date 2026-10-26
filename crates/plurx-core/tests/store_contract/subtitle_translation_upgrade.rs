//! Exact progressive trigger upgrades run through the replicated migration transaction.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicated_progressive_retarget_upgrade_preserves_owned_data_and_refuses_unknown_shapes() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("remote fixture");
    let path = cluster._root.path().join("progressive-upgrade.db");
    let current = HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &path)
        .await
        .expect("current");
    current
        .validation_reset_contract_state()
        .await
        .expect("owned fixture reset");
    let legacy = include_str!("../../src/store/subtitle_translation.sql")
        .split("-- next statement")
        .find(|sql| {
            sql.trim_start()
                .starts_with("CREATE TRIGGER subtitle_translation_mutate ")
        })
        .expect("published trigger");
    client.txn([
        ("DROP TRIGGER subtitle_translation_mutate".to_owned(),hiqlite::params!()),
        ("DROP TRIGGER subtitle_translation_capture_continuity".to_owned(),hiqlite::params!()),
        ("DROP TRIGGER subtitle_translation_retire_continuity".to_owned(),hiqlite::params!()),
        ("DROP TRIGGER subtitle_translation_revoke_continuity".to_owned(),hiqlite::params!()),
        ("DROP TABLE subtitle_translation_range_continuities".to_owned(),hiqlite::params!()),
        ("ALTER TABLE subtitle_translation_demands DROP COLUMN continuity_id".to_owned(),hiqlite::params!()),
        (legacy.to_owned(),hiqlite::params!()),
        ("INSERT INTO subtitle_translation_capabilities VALUES('fixture-node','fixture-model',printf('%064d',0),'{\"preserve\":true}')".to_owned(),hiqlite::params!()),
    ]).await.expect("published shape fixture").into_iter().collect::<Result<Vec<_>,_>>().expect("atomic legacy fixture");
    let marker = replicated_schema_marker(&client).await;
    HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &path)
        .await
        .expect("recognized upgrade");
    assert_eq!(replicated_schema_marker(&client).await, marker);
    let mut receipt=client.query_consistent("SELECT receipt_json FROM subtitle_translation_capabilities WHERE node_id='fixture-node'",hiqlite::params!()).await.expect("retained data");
    assert_eq!(
        receipt[0].get::<String>("receipt_json"),
        "{\"preserve\":true}"
    );
    let mut trigger = client
        .query_consistent(
            "SELECT sql FROM sqlite_master WHERE name='subtitle_translation_mutate'",
            hiqlite::params!(),
        )
        .await
        .expect("current trigger");
    assert!(trigger[0]
        .get::<String>("sql")
        .contains("producer_retargeted"));
    client.txn([
        ("DROP TRIGGER subtitle_translation_mutate".to_owned(),hiqlite::params!()),
        ("CREATE TRIGGER subtitle_translation_mutate AFTER INSERT ON subtitle_translation_commands BEGIN SELECT 1; END".to_owned(),hiqlite::params!()),
    ]).await.expect("unknown fixture").into_iter().collect::<Result<Vec<_>,_>>().expect("atomic unknown shape");
    assert!(
        HiqliteAuthStore::bootstrap(client.clone(), CONTRACT_INSTANCE_ID, &path)
            .await
            .is_err()
    );
    let mut untouched = client
        .query_consistent(
            "SELECT sql FROM sqlite_master WHERE name='subtitle_translation_mutate'",
            hiqlite::params!(),
        )
        .await
        .expect("refusal preserves shape");
    assert!(untouched[0].get::<String>("sql").contains("SELECT 1"));
}
