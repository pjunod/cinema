//! Real-voter evidence for the sharing floor. The surrounding contract harness
//! starts three independent Hiqlite processes; no serving binary advertises the
//! capability here. SQL rows explicitly model each heartbeat and upgrade proof.
use super::*;
use plurx_core::cluster::membership::{
    sharing_session_principal_floor_ready, sharing_session_principal_guard_predicate,
    SHARING_SESSION_PRINCIPAL_CAPABILITY,
};

async fn write(client: &Client, sql: &str) {
    client
        .execute(sql.to_owned(), hiqlite::params!())
        .await
        .expect("commit floor fixture mutation");
}

async fn ready(client: &Client) -> bool {
    sharing_session_principal_floor_ready(client, 1)
        .await
        .expect("quorum floor observation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_voter_floor_and_atomic_admission_refuse_incomplete_member_proofs() {
    let mut cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("floor observer connects to three voters");
    assert_eq!(
        client
            .metrics_db()
            .await
            .expect("committed membership")
            .membership_config
            .voter_ids()
            .count(),
        3,
        "fixture must use three actual voters"
    );
    for sql in [
        "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, removed_at INTEGER, role TEXT)",
        "CREATE TABLE cluster_node_capabilities (node_id TEXT NOT NULL, capability TEXT NOT NULL, last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability))",
        "CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_node_removal_attempts (node_id TEXT NOT NULL, attempt_id TEXT)",
        "CREATE TABLE sharing_admission_receipts (id INTEGER PRIMARY KEY)",
    ] {
        write(&client, sql).await;
    }
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("fixture clock")
            .as_millis(),
    )
    .expect("bounded clock");
    for id in 1_i64..=3 {
        client
            .execute(
                "INSERT INTO cluster_nodes VALUES ($1,$2,$3,NULL,'voter')",
                hiqlite::params!(format!("voter-{id}"), id, now),
            )
            .await
            .expect("seed committed voter identity");
    }
    assert!(!ready(&client).await, "schema alone is not a binary floor");
    client.execute("INSERT INTO cluster_node_capabilities SELECT node_id,$1,last_seen_at FROM cluster_nodes", hiqlite::params!(SHARING_SESSION_PRINCIPAL_CAPABILITY)).await.expect("seed explicit binary proofs");
    assert!(ready(&client).await);
    assert!(!sharing_session_principal_floor_ready(&client, 99)
        .await
        .expect("unknown serving identity"));

    write(
        &client,
        "DELETE FROM cluster_node_capabilities WHERE node_id='voter-3'",
    )
    .await;
    assert!(!ready(&client).await, "one older voter refuses the floor");
    client
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('voter-3',$1,$2)",
            hiqlite::params!(SHARING_SESSION_PRINCIPAL_CAPABILITY, now),
        )
        .await
        .expect("restore voter proof");
    write(
        &client,
        "UPDATE cluster_nodes SET raft_id=99 WHERE node_id='voter-3'",
    )
    .await;
    assert!(
        !ready(&client).await,
        "missing SQL identity for a committed voter refuses"
    );
    write(
        &client,
        "UPDATE cluster_nodes SET raft_id=3 WHERE node_id='voter-3'",
    )
    .await;

    client
        .execute(
            "UPDATE cluster_nodes SET last_seen_at=$1 WHERE node_id='voter-3'",
            hiqlite::params!(now - 120001),
        )
        .await
        .expect("stale heartbeat");
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id='voter-3'",
            hiqlite::params!(now - 120001),
        )
        .await
        .expect("matching stale proof");
    assert!(!ready(&client).await, "coupled but stale heartbeat refuses");
    client
        .execute(
            "UPDATE cluster_nodes SET last_seen_at=$1 WHERE node_id='voter-3'",
            hiqlite::params!(now),
        )
        .await
        .expect("fresh heartbeat");
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id='voter-3'",
            hiqlite::params!(now),
        )
        .await
        .expect("fresh proof");
    assert!(ready(&client).await);

    // An ordinary heartbeat from a legacy binary invalidates an earlier
    // successful preflight. Its conditional admission runs in the same Raft
    // transaction as that heartbeat, proving SQL rechecks committed state.
    let guard = format!(
        "INSERT INTO sharing_admission_receipts SELECT 1 WHERE {}",
        sharing_session_principal_guard_predicate(1, 2, 3)
    );
    let outcomes = client
        .txn([
            (
                "UPDATE cluster_nodes SET last_seen_at=last_seen_at-1 WHERE node_id='voter-3'"
                    .to_owned(),
                hiqlite::params!(),
            ),
            (
                guard.clone(),
                hiqlite::params!("[1,2,3]", now - 120000, now),
            ),
        ])
        .await
        .expect("atomic legacy-heartbeat admission");
    assert_eq!(
        outcomes
            .into_iter()
            .map(|result| result.expect("transaction statement"))
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    assert!(!ready(&client).await);
    write(
        &client,
        "UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at-1 WHERE node_id='voter-3'",
    )
    .await;
    assert_eq!(
        client
            .execute(guard, hiqlite::params!("[1,2,3]", now - 120000, now))
            .await
            .expect("complete guarded admission"),
        1
    );

    // A SQL-projected learner is covered before it is promoted. This does not
    // claim a fourth live Raft learner: the serving cluster remains three voters.
    client
        .execute(
            "INSERT INTO cluster_nodes VALUES ('learner',4,$1,NULL,'learner')",
            hiqlite::params!(now),
        )
        .await
        .expect("project joining learner");
    assert!(
        !ready(&client).await,
        "learner cannot be ignored by voter readiness"
    );
    client
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ('learner',$1,$2)",
            hiqlite::params!(SHARING_SESSION_PRINCIPAL_CAPABILITY, now),
        )
        .await
        .expect("learner proof");
    assert!(ready(&client).await);
    for sql in [
        "INSERT INTO cluster_node_join_staging VALUES ('unknown-rejoin')",
        "INSERT INTO cluster_node_removals VALUES ('unknown-removal')",
        "INSERT INTO cluster_node_removal_attempts VALUES ('unknown-attempt','attempt')",
    ] {
        write(&client, sql).await;
        assert!(
            !ready(&client).await,
            "orphan membership transition refuses"
        );
        write(&client, "DELETE FROM cluster_node_join_staging").await;
        write(&client, "DELETE FROM cluster_node_removals").await;
        write(&client, "DELETE FROM cluster_node_removal_attempts").await;
        assert!(ready(&client).await);
    }
    write(
        &client,
        "UPDATE cluster_nodes SET removed_at=1 WHERE node_id='voter-3'",
    )
    .await;
    assert!(
        !ready(&client).await,
        "SQL tombstone cannot erase a still-committed voter"
    );
    write(
        &client,
        "UPDATE cluster_nodes SET removed_at=NULL WHERE node_id='voter-3'",
    )
    .await;
    assert!(ready(&client).await);
    // Losing two actual voters removes quorum even though all SQL proofs were
    // ready. A bounded caller must never receive a positive cached decision.
    for node in cluster._nodes.iter_mut().take(2) {
        node._child.kill().expect("stop fixture voter");
        node._child.wait().expect("reap stopped fixture voter");
    }
    let unavailable = tokio::time::timeout(
        Duration::from_secs(5),
        sharing_session_principal_floor_ready(&client, 1),
    )
    .await;
    assert!(
        !matches!(unavailable, Ok(Ok(true))),
        "lost quorum cannot authorize sharing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_voter_allocator_floor_requires_independent_fresh_capabilities_in_the_write() {
    use plurx_core::cluster::membership::{
        sharing_catalogue_item_identity_floor_ready, sharing_member_floor_ready,
        sharing_member_guard_predicate, SharingMemberFloor,
        SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
    };
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
    .expect("allocator floor three-voter observer");
    assert_eq!(
        client
            .metrics_db()
            .await
            .expect("actual roster")
            .membership_config
            .voter_ids()
            .count(),
        3
    );
    for sql in [
        "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, removed_at INTEGER, role TEXT)",
        "CREATE TABLE cluster_node_capabilities (node_id TEXT NOT NULL, capability TEXT NOT NULL, last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id, capability))",
        "CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_node_removals (node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_node_removal_attempts (node_id TEXT NOT NULL, attempt_id TEXT)",
        "CREATE TABLE allocator_admission_receipts (id INTEGER PRIMARY KEY)",
    ] { write(&client,sql).await; }
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("bounded clock");
    for id in 1_i64..=3 {
        client
            .execute(
                "INSERT INTO cluster_nodes VALUES ($1,$2,$3,NULL,'voter')",
                hiqlite::params!(format!("voter-{id}"), id, now),
            )
            .await
            .expect("seed voter");
    }
    client.execute("INSERT INTO cluster_node_capabilities SELECT node_id,$1,last_seen_at FROM cluster_nodes",hiqlite::params!(SHARING_SESSION_PRINCIPAL_CAPABILITY)).await.expect("principal-only capabilities");
    assert!(ready(&client).await);
    assert!(!sharing_catalogue_item_identity_floor_ready(&client, 1)
        .await
        .expect("independent allocator floor"));
    let guard = format!(
        "INSERT INTO allocator_admission_receipts SELECT 1 WHERE {}",
        sharing_member_guard_predicate(SharingMemberFloor::PrincipalAndCatalogue, 1, 2, 3)
    );
    assert_eq!(
        client
            .execute(
                guard.clone(),
                hiqlite::params!("[1,2,3]", now - 120000, now)
            )
            .await
            .expect("principal-only admission"),
        0
    );
    client.execute("INSERT INTO cluster_node_capabilities SELECT node_id,$1,last_seen_at FROM cluster_nodes",hiqlite::params!(SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY)).await.expect("allocator capabilities");
    assert!(
        sharing_member_floor_ready(&client, 1, SharingMemberFloor::PrincipalAndCatalogue)
            .await
            .expect("both independent floors")
    );
    assert_eq!(
        client
            .execute(
                guard.clone(),
                hiqlite::params!("[1,2,3]", now - 120000, now)
            )
            .await
            .expect("both-capability admission"),
        1
    );
    write(&client, "DELETE FROM allocator_admission_receipts").await;
    let outcomes=client.txn([
        ("UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at-1 WHERE node_id='voter-3' AND capability='sharing_catalogue_item_identity_v1'".to_owned(),hiqlite::params!()),
        (guard.clone(),hiqlite::params!("[1,2,3]",now-120000,now)),
    ]).await.expect("capability change and admission commit together");
    assert_eq!(
        outcomes
            .into_iter()
            .map(|result| result.expect("atomic statement"))
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    assert!(
        ready(&client).await,
        "allocator proof cannot weaken principal wrapper"
    );
    assert!(
        !sharing_member_floor_ready(&client, 1, SharingMemberFloor::PrincipalAndCatalogue)
            .await
            .expect("changed catalogue proof")
    );
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1",
            hiqlite::params!(now),
        )
        .await
        .expect("restore all fresh proofs");
    write(
        &client,
        "UPDATE cluster_nodes SET raft_id=99 WHERE node_id='voter-3'",
    )
    .await;
    assert!(
        !sharing_member_floor_ready(&client, 1, SharingMemberFloor::PrincipalAndCatalogue)
            .await
            .expect("exact committed roster")
    );
    assert_eq!(
        client
            .execute(
                guard.clone(),
                hiqlite::params!("[1,2,3]", now - 120000, now)
            )
            .await
            .expect("missing-roster guarded write"),
        0
    );
    write(
        &client,
        "UPDATE cluster_nodes SET raft_id=3 WHERE node_id='voter-3'",
    )
    .await;
    client
        .txn([
            (
                "UPDATE cluster_nodes SET last_seen_at=$1 WHERE node_id='voter-3'",
                hiqlite::params!(now - 120001),
            ),
            (
                "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id='voter-3'",
                hiqlite::params!(now - 120001),
            ),
        ])
        .await
        .expect("coupled stale proofs")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit stale proofs");
    assert!(
        !sharing_member_floor_ready(&client, 1, SharingMemberFloor::PrincipalAndCatalogue)
            .await
            .expect("coupled proofs must still be fresh")
    );
    assert_eq!(
        client
            .execute(guard, hiqlite::params!("[1,2,3]", now - 120000, now))
            .await
            .expect("stale-roster guarded write"),
        0
    );
}
