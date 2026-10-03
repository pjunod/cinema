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

fn sharing_join_request(
    token: char,
    raft_id: u64,
) -> plurx_core::cluster::membership::RedeemJoinRequest {
    plurx_core::cluster::membership::RedeemJoinRequest {
        token_digest: token.to_string().repeat(64),
        raft_id,
        node_id: uuid::Uuid::new_v4().to_string(),
        hostname: "floor-fixture".to_owned(),
        raft_address: "127.0.0.1:32401".to_owned(),
        api_address: "127.0.0.1:32402".to_owned(),
        http_base: String::new(),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: plurx_core::store::AUTH_PROTOCOL_VERSION,
        protocol_min: plurx_core::store::AUTH_PROTOCOL_MIN,
        protocol_max: plurx_core::store::AUTH_PROTOCOL_MAX,
        live_tv_v1: true,
        sharing: Default::default(),
    }
}

async fn seed_sharing_join_token(
    client: &Client,
    request: &plurx_core::cluster::membership::RedeemJoinRequest,
    now: i64,
) {
    client
        .execute(
            "INSERT INTO cluster_join_tokens VALUES ($1,'issued',NULL,$2,$3)",
            hiqlite::params!(
                request.token_digest.as_str(),
                request.raft_id as i64,
                now + 60000
            ),
        )
        .await
        .expect("seed issued join token");
}

async fn submit_sharing_join(
    client: &Client,
    request: &plurx_core::cluster::membership::RedeemJoinRequest,
    now: i64,
    write_intents: bool,
) -> Result<Vec<usize>, hiqlite::Error> {
    use plurx_core::cluster::membership::sharing_join_intent_statements;
    let mut statements = if write_intents {
        sharing_join_intent_statements(request, now)
    } else {
        Vec::new()
    };
    statements.extend([
        ("UPDATE cluster_join_tokens SET state='redeeming',node_id=$1 WHERE token_hash=$2 AND state='issued'".to_owned(),hiqlite::params!(request.node_id.as_str(),request.token_digest.as_str())),
        ("INSERT INTO cluster_node_join_staging VALUES ($1)".to_owned(),hiqlite::params!(request.node_id.as_str())),
        ("INSERT INTO cluster_nodes(node_id,raft_id,last_seen_at,removed_at,role) VALUES ($1,$2,$3,NULL,'learner')".to_owned(),hiqlite::params!(request.node_id.as_str(),request.raft_id as i64,now)),
        ("DELETE FROM cluster_node_heartbeat_intents WHERE node_id=$1".to_owned(),hiqlite::params!(request.node_id.as_str())),
        ("DELETE FROM cluster_sharing_join_intents WHERE token_hash=$1".to_owned(),hiqlite::params!(request.token_digest.as_str())),
    ]);
    client.txn(statements).await?.into_iter().collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_voter_candidate_admission_fences_legacy_rejoin_and_promotion_after_partial_install()
{
    use plurx_core::cluster::membership::{
        sharing_member_admission_guard_schema, SharingJoinCapabilities,
        SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
    };
    assert!(
        serde_json::from_str::<SharingJoinCapabilities>(r#"{"arbitrary_capability":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<SharingJoinCapabilities>(r#"{"session_principal":"true"}"#).is_err()
    );
    assert_eq!(
        serde_json::from_str::<SharingJoinCapabilities>("{}").expect("legacy missing declaration"),
        SharingJoinCapabilities::default()
    );
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
    .expect("candidate admission observer");
    assert_eq!(
        client
            .metrics_db()
            .await
            .expect("actual voter roster")
            .membership_config
            .voter_ids()
            .count(),
        3
    );
    for sql in [
        "CREATE TABLE cluster_nodes (node_id TEXT PRIMARY KEY, raft_id INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, removed_at INTEGER, role TEXT)",
        "CREATE TABLE cluster_node_capabilities (node_id TEXT NOT NULL, capability TEXT NOT NULL, last_seen_at INTEGER NOT NULL, PRIMARY KEY(node_id,capability))",
        "CREATE TABLE cluster_node_heartbeat_intents (node_id TEXT PRIMARY KEY,last_seen_at INTEGER NOT NULL)",
        "CREATE TABLE cluster_node_join_staging (node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_join_tokens (token_hash TEXT PRIMARY KEY,state TEXT,node_id TEXT,raft_id INTEGER,expires_at INTEGER)",
        "CREATE TABLE cluster_node_promotions (node_id TEXT PRIMARY KEY,attempt_id TEXT,barrier_index INTEGER,started_at INTEGER)",
    ] { write(&client,sql).await; }
    // Install only into this isolated test state machine. Every application
    // table is absent when the candidate trigger DDL replays.
    for sql in sharing_member_admission_guard_schema() {
        write(&client, &sql).await;
    }
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("bounded clock");
    let legacy = sharing_join_request('a', 4);
    seed_sharing_join_token(&client, &legacy, now).await;
    submit_sharing_join(&client, &legacy, now, false)
        .await
        .expect("no candidate marker preserves legacy join behavior");
    write(
        &client,
        "CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT)",
    )
    .await;
    write(
        &client,
        "INSERT INTO settings VALUES ('sharing.enabled','0')",
    )
    .await;
    write(
        &client,
        "CREATE TABLE item_identity_watermark (malformed TEXT)",
    )
    .await;
    let mut allocator = sharing_join_request('b', 5);
    allocator.sharing.session_principal = true;
    seed_sharing_join_token(&client, &allocator, now).await;
    assert!(
        submit_sharing_join(&client, &allocator, now, false)
            .await
            .is_err(),
        "old coordinator cannot admit after a partial allocator install"
    );
    assert!(
        submit_sharing_join(&client, &allocator, now, true)
            .await
            .is_err(),
        "principal declaration does not prove allocator writers"
    );

    // A stale proof left for this token, or a previous member's capabilities,
    // cannot authorize a new request. Fresh transaction heartbeat coupling is
    // required even when the intent table contains the right capability name.
    client
        .execute(
            "INSERT INTO cluster_sharing_join_intents VALUES ($1,$2,$3)",
            hiqlite::params!(
                allocator.token_digest.as_str(),
                SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
                now - 1
            ),
        )
        .await
        .expect("seed stale token proof");
    client
        .execute(
            "INSERT INTO cluster_node_heartbeat_intents VALUES ($1,$2)",
            hiqlite::params!(allocator.node_id.as_str(), now),
        )
        .await
        .expect("different transaction heartbeat");
    assert!(
        submit_sharing_join(&client, &allocator, now, false)
            .await
            .is_err(),
        "stale token proof cannot inherit a fresh heartbeat"
    );
    assert!(
        submit_sharing_join(&client, &allocator, now, true)
            .await
            .is_err(),
        "new request clears old catalogue declaration"
    );
    allocator.sharing = SharingJoinCapabilities {
        session_principal: false,
        catalogue_item_identity: true,
    };
    submit_sharing_join(&client, &allocator, now, true)
        .await
        .expect("compatible allocator-only binary admitted under allocator marker");
    let rows: Vec<I64Value> = client
        .query_consistent_map(
            "SELECT COUNT(*) AS value FROM cluster_sharing_join_intents WHERE token_hash=$1",
            hiqlite::params!(allocator.token_digest.as_str()),
        )
        .await
        .expect("read consumed proof");
    assert_eq!(
        rows[0].value, 0,
        "join proof does not outlive its admission transaction"
    );

    // Any one owner column in even a temporary/malformed candidate table
    // requires the independent principal capability, regardless of the switch.
    write(
        &client,
        "CREATE TABLE media_session_requests_principal_new(owner_key INTEGER)",
    )
    .await;
    let mut both = sharing_join_request('c', 6);
    both.sharing = allocator.sharing;
    seed_sharing_join_token(&client, &both, now).await;
    assert!(
        submit_sharing_join(&client, &both, now, true)
            .await
            .is_err(),
        "partial principal install refuses allocator-only admission"
    );
    both.sharing.session_principal = true;
    submit_sharing_join(&client, &both, now, true)
        .await
        .expect("both independent declarations admit compatible member");

    let promotion_sql = "INSERT INTO cluster_node_promotions VALUES ($1,'attempt',NULL,$2)";
    assert!(
        client
            .execute(promotion_sql, hiqlite::params!(both.node_id.as_str(), now))
            .await
            .is_err(),
        "join declaration cannot replace target heartbeat proofs for promotion"
    );
    client
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ($1,$2,$3)",
            hiqlite::params!(
                both.node_id.as_str(),
                SHARING_SESSION_PRINCIPAL_CAPABILITY,
                now
            ),
        )
        .await
        .expect("principal target heartbeat proof");
    assert!(
        client
            .execute(promotion_sql, hiqlite::params!(both.node_id.as_str(), now))
            .await
            .is_err(),
        "principal-only target cannot be promoted under allocator marker"
    );
    client
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES ($1,$2,$3)",
            hiqlite::params!(
                both.node_id.as_str(),
                SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
                now - 1
            ),
        )
        .await
        .expect("stale allocator target proof");
    assert!(
        client
            .execute(promotion_sql, hiqlite::params!(both.node_id.as_str(), now))
            .await
            .is_err(),
        "stale target capability cannot promote"
    );
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id=$2",
            hiqlite::params!(now, both.node_id.as_str()),
        )
        .await
        .expect("fresh exact target proofs");
    assert_eq!(
        client
            .execute(promotion_sql, hiqlite::params!(both.node_id.as_str(), now))
            .await
            .expect("compatible promotion intent"),
        1
    );
    client.execute("UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id=$2 AND capability=$3",hiqlite::params!(now-1,both.node_id.as_str(),SHARING_SESSION_PRINCIPAL_CAPABILITY)).await.expect("legacy downgrade after promotion preflight");
    assert!(
        client
            .execute(
                "UPDATE cluster_node_promotions SET barrier_index=1 WHERE node_id=$1",
                hiqlite::params!(both.node_id.as_str())
            )
            .await
            .is_err(),
        "promotion cannot reuse earlier readiness after legacy heartbeat"
    );
    assert!(
        client
            .execute(
                "UPDATE cluster_nodes SET role='voter' WHERE node_id=$1",
                hiqlite::params!(both.node_id.as_str())
            )
            .await
            .is_err(),
        "direct legacy role publication is also fenced"
    );
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id=$2",
            hiqlite::params!(now, both.node_id.as_str()),
        )
        .await
        .expect("restore both proofs");
    assert_eq!(
        client
            .execute(
                "UPDATE cluster_nodes SET role='voter' WHERE node_id=$1",
                hiqlite::params!(both.node_id.as_str())
            )
            .await
            .expect("compatible role publication"),
        1
    );
    client
        .execute(
            "UPDATE cluster_join_tokens SET state='redeemed' WHERE token_hash=$1",
            hiqlite::params!(both.token_digest.as_str()),
        )
        .await
        .expect("compatible finalization requires actual target proofs");
    client
        .execute(
            "UPDATE cluster_nodes SET removed_at=1 WHERE node_id=$1",
            hiqlite::params!(both.node_id.as_str()),
        )
        .await
        .expect("tombstone old member");
    assert!(client.execute("UPDATE cluster_nodes SET removed_at=NULL WHERE node_id=$1",hiqlite::params!(both.node_id.as_str())).await.is_err(),"old joined capabilities cannot reactivate a removed member without a fresh token-bound intent");
}

async fn raw_sharing_membership(
    client: &Client,
    operation: &str,
    request: &plurx_core::cluster::membership::RedeemJoinRequest,
) -> reqwest::Response {
    let metrics = client.metrics_db().await.expect("leader membership");
    let leader = metrics
        .membership_config
        .membership()
        .get_node(&metrics.current_leader.expect("leader"))
        .expect("leader endpoint");
    reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(15)).build().expect("fixture HTTP client")
        .post(format!("https://{}/cluster/{operation}/sqlite",leader.addr_api))
        .header("X-API-SECRET",CONTRACT_API_SECRET).header("Accept","application/json")
        .json(&serde_json::json!({"node_id":request.raft_id,"addr_api":request.api_address,"addr_raft":request.raft_address}))
        .send().await.expect("raw membership response")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_fourth_learner_raw_admission_and_promotion_freeze_capability_races() {
    use plurx_core::cluster::membership::{
        sharing_member_admission_guard_schema, sharing_member_transition_absence_predicate,
        SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
    };
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
    .expect("membership observer");
    for sql in [
        "CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER NOT NULL,last_seen_at INTEGER NOT NULL,removed_at INTEGER,role TEXT,api_address TEXT NOT NULL DEFAULT '',raft_address TEXT NOT NULL DEFAULT '')",
        "CREATE TABLE cluster_node_capabilities(node_id TEXT NOT NULL,capability TEXT NOT NULL,last_seen_at INTEGER NOT NULL,PRIMARY KEY(node_id,capability))",
        "CREATE TABLE cluster_node_heartbeat_intents(node_id TEXT PRIMARY KEY,last_seen_at INTEGER NOT NULL)",
        "CREATE TABLE cluster_node_join_staging(node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_join_tokens(token_hash TEXT PRIMARY KEY,state TEXT,node_id TEXT,raft_id INTEGER,expires_at INTEGER)",
        "CREATE TABLE cluster_node_promotions(node_id TEXT PRIMARY KEY,attempt_id TEXT,barrier_index INTEGER,started_at INTEGER)",
        "CREATE TABLE sharing_transition_receipts(id INTEGER PRIMARY KEY)",
    ] { write(&client,sql).await; }
    for sql in sharing_member_admission_guard_schema() {
        write(&client, &sql).await;
    }
    // Install guards before this deliberately partial allocator marker.
    write(
        &client,
        "CREATE TABLE item_identity_watermark(malformed TEXT)",
    )
    .await;
    let ports = contract_free_ports(2);
    let mut request = sharing_join_request('e', 4);
    request.api_address = format!("127.0.0.1:{}", ports[0]);
    request.raft_address = format!("127.0.0.1:{}", ports[1]);
    assert!(
        !raw_sharing_membership(&client, "add_learner", &request)
            .await
            .status()
            .is_success(),
        "API secret alone cannot admit a missing target proof"
    );
    request.sharing.catalogue_item_identity = true;
    let now = chrono::Utc::now().timestamp_millis();
    seed_sharing_join_token(&client, &request, now).await;
    submit_sharing_join(&client, &request, now, true)
        .await
        .expect("compatible reserved learner");
    client
        .execute(
            "UPDATE cluster_nodes SET api_address=$1,raft_address=$2 WHERE node_id=$3",
            hiqlite::params!(
                request.api_address.clone(),
                request.raft_address.clone(),
                request.node_id.clone()
            ),
        )
        .await
        .expect("bind exact server endpoints");
    // A partially installed freeze family refuses before any Raft mutation.
    write(
        &client,
        "DROP TRIGGER cluster_sharing_freeze_cluster_nodes_update",
    )
    .await;
    assert!(!raw_sharing_membership(&client, "add_learner", &request)
        .await
        .status()
        .is_success());
    for sql in sharing_member_admission_guard_schema() {
        write(&client, &sql).await;
    }
    client.execute("UPDATE cluster_sharing_join_declarations SET last_seen_at=last_seen_at-1 WHERE node_id=$1",hiqlite::params!(request.node_id.clone())).await.expect("stale token declaration");
    assert!(
        !raw_sharing_membership(&client, "add_learner", &request)
            .await
            .status()
            .is_success(),
        "stale reserved declaration cannot admit a new Raft target"
    );
    client
        .execute(
            "UPDATE cluster_sharing_join_declarations SET last_seen_at=$1 WHERE node_id=$2",
            hiqlite::params!(now, request.node_id.clone()),
        )
        .await
        .expect("restore exact declaration");
    let metrics = client.metrics_db().await.expect("bootstrap membership");
    let mut specs = metrics
        .membership_config
        .nodes()
        .map(|(id, node)| ContractNodeSpec {
            id: *id,
            api: node.addr_api.clone(),
            raft: node.addr_raft.clone(),
        })
        .collect::<Vec<_>>();
    specs.push(ContractNodeSpec {
        id: 4,
        api: request.api_address.clone(),
        raft: request.raft_address.clone(),
    });
    let launch = ContractNodeLaunch {
        node_id: 4,
        learner_only: true,
        root: cluster._root.path().to_path_buf(),
        nodes: specs,
    };
    let mut child = Command::new(std::env::current_exe().expect("fixture executable"))
        .arg("hiqlite_contract_node_process")
        .arg("--ignored")
        .arg("--exact")
        .arg("--nocapture")
        .env(
            "PLURX_CONTRACT_NODE_LAUNCH",
            serde_json::to_string(&launch).expect("node launch"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real fourth learner");
    let input = child.stdin.take().expect("learner stdin");
    let output = child.stdout.take().expect("learner stdout");
    let reader = tokio::task::spawn_blocking(move || {
        let mut reader = BufReader::new(output);
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).expect("learner readiness line") > 0,
                "learner exited before ready"
            );
            if line.trim() == "PLURX_CONTRACT_NODE_READY 4" {
                return reader.into_inner();
            }
            assert!(
                !line.starts_with("PLURX_CONTRACT_NODE_START_FAILED"),
                "{line}"
            );
        }
    });
    // Register before waiting so every panic reaps the fourth process.
    // The reader owns stdout until readiness; use a cloned OS handle only after
    // it returns, with explicit failure cleanup on timeout.
    let output = match tokio::time::timeout(Duration::from_secs(60), reader).await {
        Ok(result) => result.expect("learner readiness task"),
        Err(error) => {
            drop(input);
            let _ = child.kill();
            let _ = child.wait();
            panic!("fourth learner readiness: {error}");
        }
    };
    cluster._nodes.push(ContractNodeProcess {
        _child: child,
        _input: Some(input),
        _output: output,
    });
    let metrics = client.metrics_db().await.expect("real learner membership");
    assert_eq!(metrics.membership_config.voter_ids().count(), 3);
    assert!(metrics
        .membership_config
        .membership()
        .get_node(&4)
        .is_some());
    assert!(
        !raw_sharing_membership(&client, "become_member", &request)
            .await
            .status()
            .is_success(),
        "initial declaration cannot replace a learner's own capability heartbeat"
    );
    client
        .execute(
            "INSERT INTO cluster_node_capabilities VALUES($1,$2,$3)",
            hiqlite::params!(
                request.node_id.clone(),
                SHARING_CATALOGUE_ITEM_IDENTITY_CAPABILITY,
                now
            ),
        )
        .await
        .expect("actual target heartbeat proof");
    client
        .execute(
            "INSERT INTO cluster_node_promotions VALUES($1,'promotion',0,$2)",
            hiqlite::params!(request.node_id.clone(), now),
        )
        .await
        .expect("promotion coordinator intent");
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at-1 WHERE node_id=$1",
            hiqlite::params!(request.node_id.clone()),
        )
        .await
        .expect("change target capability generation");
    assert!(
        !raw_sharing_membership(&client, "become_member", &request)
            .await
            .status()
            .is_success(),
        "stale capability cannot promote real learner"
    );
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id=$2",
            hiqlite::params!(now, request.node_id.clone()),
        )
        .await
        .expect("restore exact capability");
    // Claim through the same candidate SQL guard used by the leader, then
    // exercise mutations while that durable membership operation is pending.
    client.execute("INSERT INTO cluster_sharing_membership_intents VALUES($1,$2,'qualified-promotion','voter',$3,$4,$5)",hiqlite::params!(4_i64,request.node_id.clone(),request.api_address.clone(),request.raft_address.clone(),chrono::Utc::now().timestamp_millis())).await.expect("atomic admission intent");
    for sql in [
        "DELETE FROM cluster_node_capabilities WHERE node_id=$1",
        "UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at+1 WHERE node_id=$1",
        "UPDATE cluster_nodes SET last_seen_at=last_seen_at+1 WHERE node_id=$1",
        "UPDATE cluster_nodes SET removed_at=1 WHERE node_id=$1",
        "UPDATE cluster_sharing_join_declarations SET last_seen_at=last_seen_at+1 WHERE node_id=$1",
        "UPDATE cluster_join_tokens SET state='redeemed' WHERE node_id=$1",
    ] {
        assert!(
            client
                .execute(sql, hiqlite::params!(request.node_id.clone()))
                .await
                .is_err(),
            "pending membership must freeze {sql}"
        );
    }
    assert_eq!(
        client
            .execute(
                format!(
                    "INSERT INTO sharing_transition_receipts SELECT 1 WHERE {}",
                    sharing_member_transition_absence_predicate()
                ),
                hiqlite::params!()
            )
            .await
            .expect("same-write transition guard"),
        0
    );
    let response = raw_sharing_membership(&client, "become_member", &request).await;
    assert!(
        response.status().is_success(),
        "compatible real learner promotion: {}",
        response.text().await.expect("promotion response")
    );
    let metrics = client.metrics_db().await.expect("promoted membership");
    assert_eq!(metrics.membership_config.voter_ids().count(), 4);
    assert!(metrics.membership_config.voter_ids().any(|id| id == 4));
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at+1 WHERE node_id=$1",
            hiqlite::params!(request.node_id.clone()),
        )
        .await
        .expect("quorum-confirmed outcome releases freeze");
    let rows: Vec<I64Value> = client
        .query_consistent_map(
            "SELECT COUNT(*) AS value FROM cluster_sharing_membership_intents",
            hiqlite::params!(),
        )
        .await
        .expect("completed intent inventory");
    assert_eq!(rows[0].value, 0);
    client
        .execute(
            "UPDATE cluster_node_capabilities SET last_seen_at=$1 WHERE node_id=$2",
            hiqlite::params!(now, request.node_id.clone()),
        )
        .await
        .expect("restore target proof for finalization");
    client
        .execute(
            "UPDATE cluster_nodes SET role='voter' WHERE node_id=$1",
            hiqlite::params!(request.node_id.clone()),
        )
        .await
        .expect("publish promoted SQL role after committed membership");
    client
        .execute(
            "UPDATE cluster_join_tokens SET state='redeemed' WHERE node_id=$1",
            hiqlite::params!(request.node_id.clone()),
        )
        .await
        .expect("consume reserved declaration");
    let metrics = client.metrics_db().await.expect("remove endpoint");
    let leader = metrics
        .membership_config
        .membership()
        .get_node(&metrics.current_leader.expect("leader"))
        .expect("leader endpoint");
    let removed = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(15))
        .build()
        .expect("remove fixture client")
        .delete(format!(
            "https://{}/cluster/membership/sqlite",
            leader.addr_api
        ))
        .header("X-API-SECRET", CONTRACT_API_SECRET)
        .json(&serde_json::json!({"node_id":4,"stay_as_learner":false}))
        .send()
        .await
        .expect("actual member removal");
    assert!(removed.status().is_success());
    assert_eq!(
        client
            .metrics_db()
            .await
            .expect("removed roster")
            .membership_config
            .voter_ids()
            .count(),
        3
    );
    assert!(
        !raw_sharing_membership(&client, "add_learner", &request)
            .await
            .status()
            .is_success(),
        "prior joined capability cannot authorize a fresh raw rejoin after token consumption"
    );
    assert!(client
        .metrics_db()
        .await
        .expect("refused rejoin roster")
        .membership_config
        .membership()
        .get_node(&4)
        .is_none());
}
