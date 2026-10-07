//! Purpose factory uses the real current quorum observation and normal Store.
use super::*;
use plurx_core::{
    cluster::membership::{
        bootstrap_purpose_admission_schema_for_contract, observe_purpose_key_members_for_contract,
        SHARING_PURPOSE_KEYS_CAPABILITY,
    },
    secrets::CredentialKey,
    store::{sharing_purpose_keys::PurposeKeyInstallation, SharingPurposeKeyStore, SharingStore},
};
use uuid::Uuid;
struct Envelope(String);
impl From<&mut hiqlite::Row<'_>> for Envelope {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self(row.get("payload"))
    }
}
async fn envelopes(client: &Client, source: bool) -> Vec<String> {
    let sql = if source {
        "SELECT revision_envelope AS payload FROM sharing_catalogue_keys"
    } else {
        "SELECT locator_envelope AS payload FROM sharing_file_locator_keys"
    };
    client
        .query_consistent_map::<Envelope, _>(sql, hiqlite::params!())
        .await
        .expect("sealed snapshot")
        .into_iter()
        .map(|row| row.0)
        .collect()
}
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("bounded clock")
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_purpose_factory_three_voters_preserves_winner_and_refuses_stale_master_proof() {
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
    .expect("observer");
    let roster = client
        .metrics_db()
        .await
        .expect("metrics")
        .membership_config
        .voter_ids()
        .collect::<Vec<_>>();
    assert_eq!(roster.len(), 3);
    for sql in [
        "CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER NOT NULL,last_seen_at INTEGER NOT NULL,removed_at INTEGER,role TEXT,api_address TEXT,raft_address TEXT)",
        "CREATE TABLE cluster_node_capabilities(node_id TEXT NOT NULL,capability TEXT NOT NULL,last_seen_at INTEGER NOT NULL,PRIMARY KEY(node_id,capability))",
        "CREATE TABLE cluster_node_heartbeat_intents(node_id TEXT PRIMARY KEY,last_seen_at INTEGER NOT NULL)",
        "CREATE TABLE cluster_node_join_staging(node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_join_tokens(token_hash TEXT PRIMARY KEY,state TEXT,node_id TEXT,raft_id INTEGER,expires_at INTEGER)",
        "CREATE TABLE cluster_node_promotions(node_id TEXT PRIMARY KEY,attempt_id TEXT,barrier_index INTEGER,started_at INTEGER)",
        "CREATE TABLE cluster_node_removals(node_id TEXT PRIMARY KEY)",
        "CREATE TABLE cluster_node_removal_attempts(node_id TEXT NOT NULL,attempt_id TEXT)",
    ] {client.execute(sql,hiqlite::params!()).await.expect("floor schema");}
    let master = CredentialKey::from_bytes([41; 32]);
    let now = now_ms();
    for id in &roster {
        let id = i64::try_from(*id).expect("actual raft identity");
        let node = format!("voter-{id}");
        client
            .execute(
                "INSERT INTO cluster_nodes VALUES($1,$2,$3,NULL,'voter','api','raft')",
                hiqlite::params!(node.as_str(), id, now),
            )
            .await
            .expect("current node");
        if id == i64::try_from(roster[0]).expect("actual first voter") {
            let identity = plurx_core::cluster::ClusterIdentity {
                cluster_id: "contract-cluster".to_owned(),
                node_id: node.clone(),
                raft_id: roster[0],
            };
            plurx_core::store::sharing_purpose_keys::with_purpose_census_for_contract(
                &store,
                &identity,
                now,
                || async {},
            )
            .await
            .expect("actual startup census precedes capability proof");
        }
        for proof in [
            SHARING_PURPOSE_KEYS_CAPABILITY.to_owned(),
            format!(
                "sharing_purpose_master_v1:{}",
                master.sharing_purpose_master_fingerprint()
            ),
        ] {
            client
                .execute(
                    "INSERT INTO cluster_node_capabilities VALUES($1,$2,$3)",
                    hiqlite::params!(node.as_str(), proof, now),
                )
                .await
                .expect("selected master proof");
        }
    }
    client
        .execute(
            "CREATE TABLE cluster_sharing_join_intents(partial TEXT)",
            hiqlite::params!(),
        )
        .await
        .expect("partial existing bootstrap shape");
    assert!(
        bootstrap_purpose_admission_schema_for_contract(&client, roster[0], &master)
            .await
            .is_err(),
        "partial schema is refused without repair"
    );
    client
        .execute(
            "INSERT INTO cluster_sharing_join_intents VALUES('unchanged')",
            hiqlite::params!(),
        )
        .await
        .expect("original partial shape retained");
    client
        .execute(
            "DROP TABLE cluster_sharing_join_intents",
            hiqlite::params!(),
        )
        .await
        .expect("remove test-only partial fixture");
    let legacy_schema = plurx_core::cluster::membership::sharing_member_admission_guard_schema()
        .into_iter()
        .filter(|sql| !sql.contains("cluster_sharing_purpose_keys_"))
        .map(|sql| sql.replace(",'sharing_purpose_keys_v1'", ""))
        .collect::<Vec<_>>();
    for sql in &legacy_schema {
        client
            .execute(sql.clone(), hiqlite::params!())
            .await
            .expect("complete legacy two-capability guard schema");
    }
    let legacy_before = client.query_consistent_map::<Envelope,_>("SELECT sql AS payload FROM sqlite_master WHERE name GLOB 'cluster_sharing_*' ORDER BY name",hiqlite::params!()).await.expect("legacy schema snapshot").into_iter().map(|row|row.0).collect::<Vec<_>>();
    assert!(
        bootstrap_purpose_admission_schema_for_contract(&client, roster[0], &master)
            .await
            .is_err(),
        "complete legacy two-capability schema requires explicit migration, never repair"
    );
    assert_eq!(client.query_consistent_map::<Envelope,_>("SELECT sql AS payload FROM sqlite_master WHERE name GLOB 'cluster_sharing_*' ORDER BY name",hiqlite::params!()).await.expect("unchanged legacy schema").into_iter().map(|row|row.0).collect::<Vec<_>>(),legacy_before);
    for kind in ["TRIGGER", "TABLE"] {
        for sql in &legacy_schema {
            let canonical = sql.replace(" IF NOT EXISTS", "");
            let words = canonical.split_whitespace().collect::<Vec<_>>();
            if words.first() == Some(&"CREATE") && words.get(1) == Some(&kind) {
                let name = words[2].split('(').next().expect("fixed legacy DDL name");
                client
                    .execute(format!("DROP {kind} {name}"), hiqlite::params!())
                    .await
                    .expect("remove complete legacy test fixture");
            }
        }
    }
    let (first_bootstrap, second_bootstrap) = tokio::join!(
        bootstrap_purpose_admission_schema_for_contract(&client, roster[0], &master),
        bootstrap_purpose_admission_schema_for_contract(&client, roster[1], &master),
    );
    first_bootstrap.expect("actual first voter bootstrap");
    second_bootstrap.expect("competing voter adopts only complete winner");
    let local = roster[0];
    let witness = observe_purpose_key_members_for_contract(&client, local, &master)
        .await
        .expect("real floor")
        .expect("current complete proof");
    assert_eq!(
        store
            .verify_sharing_purpose_material(&master)
            .await
            .expect("read never initializes"),
        PurposeKeyInstallation::NotReady
    );
    let identity = plurx_core::cluster::ClusterIdentity {
        cluster_id: "contract-cluster".to_owned(),
        node_id: format!("voter-{local}"),
        raft_id: local,
    };
    let blocked = plurx_core::store::sharing_purpose_keys::with_purpose_census_for_contract(
        &store,
        &identity,
        now_ms(),
        || async {
            store
                .install_sharing_purpose_keys(&master, &witness, now_ms())
                .await
        },
    )
    .await
    .expect("actual census ownership")
    .expect("guarded refusal");
    assert_eq!(
        blocked,
        PurposeKeyInstallation::NotReady,
        "old floor observation cannot authorize a factory during master census"
    );
    for proof in [
        SHARING_PURPOSE_KEYS_CAPABILITY.to_owned(),
        format!(
            "sharing_purpose_master_v1:{}",
            master.sharing_purpose_master_fingerprint()
        ),
    ] {
        client
            .execute(
                "INSERT INTO cluster_node_capabilities VALUES($1,$2,$3)",
                hiqlite::params!(identity.node_id.as_str(), proof, now),
            )
            .await
            .expect("new boot proof after exact census release");
    }
    let witness = observe_purpose_key_members_for_contract(&client, local, &master)
        .await
        .expect("current after census")
        .expect("actual selected master floor");
    client
        .execute(
            "UPDATE cluster_node_capabilities SET capability=$1 WHERE node_id=$2 AND capability=$3",
            hiqlite::params!(
                format!("sharing_purpose_master_v1:{}", "a".repeat(64)),
                format!("voter-{}", roster[1]),
                format!(
                    "sharing_purpose_master_v1:{}",
                    master.sharing_purpose_master_fingerprint()
                )
            ),
        )
        .await
        .expect("wrong full selected-master fingerprint");
    assert!(
        observe_purpose_key_members_for_contract(&client, local, &master)
            .await
            .expect("actual wrong-fingerprint check")
            .is_none()
    );
    client
        .execute(
            "UPDATE cluster_node_capabilities SET capability=$1 WHERE node_id=$2 AND capability=$3",
            hiqlite::params!(
                format!(
                    "sharing_purpose_master_v1:{}",
                    master.sharing_purpose_master_fingerprint()
                ),
                format!("voter-{}", roster[1]),
                format!("sharing_purpose_master_v1:{}", "a".repeat(64))
            ),
        )
        .await
        .expect("restore current actual proof");
    let (first, second) = tokio::join!(
        store.install_sharing_purpose_keys(&master, &witness, now_ms()),
        store.install_sharing_purpose_keys(&master, &witness, now_ms()),
    );
    assert_eq!(
        first.expect("first concurrent factory"),
        PurposeKeyInstallation::Ready
    );
    assert_eq!(
        second.expect("second adopts valid winner"),
        PurposeKeyInstallation::Ready
    );
    let before = envelopes(&client, true).await;
    let before_b = envelopes(&client, false).await;
    assert_eq!(
        store
            .install_sharing_purpose_keys(&master, &witness, now_ms())
            .await
            .expect("idempotent reopen"),
        PurposeKeyInstallation::Ready
    );
    assert_eq!(before, envelopes(&client, true).await);
    assert_eq!(before_b, envelopes(&client, false).await);
    // Retired purpose material participates in the same real guarded rewrap.
    let retired = plurx_core::sharing::SharingIdentity {
        server_id: Uuid::new_v4(),
        catalogue_epoch: Uuid::new_v4(),
        created_at_ms: now,
    };
    let retired_source =
        plurx_core::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
            &master,
            retired.clone(),
        )
        .expect("retired Source key");
    let retired_receiver =
        plurx_core::sharing_file_locators::FileLocatorKey::generate_sealed(&master, &retired)
            .expect("retired B key");
    for (purpose, envelope) in [
        ("catalogue_revision", &retired_source),
        ("file_locator", &retired_receiver),
    ] {
        client
            .execute(
                "INSERT INTO sharing_purpose_key_archive VALUES($1,$2,$3,$4)",
                hiqlite::params!(
                    purpose,
                    retired.server_id.to_string(),
                    retired.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ),
            )
            .await
            .expect("retired complete pair");
    }
    let archive_before = client
        .query_consistent_map::<Envelope, _>(
            "SELECT envelope AS payload FROM sharing_purpose_key_archive ORDER BY purpose",
            hiqlite::params!(),
        )
        .await
        .expect("archive snapshot")
        .into_iter()
        .map(|row| row.0)
        .collect::<Vec<_>>();
    assert_eq!(
        store
            .sharing_sealed_census()
            .await
            .expect("active and archived census")
            .sealed_rows(),
        4
    );
    let replacement = CredentialKey::from_bytes([43; 32]);
    let fresh = observe_purpose_key_members_for_contract(&client, local, &master)
        .await
        .expect("fresh current floor")
        .expect("ready");
    client.execute("CREATE TRIGGER fixture_ignore_locator_rewrap BEFORE UPDATE ON sharing_file_locator_keys BEGIN SELECT RAISE(IGNORE); END",hiqlite::params!()).await.expect("adversarial ignored write");
    assert!(
        store
            .rewrap_sharing_purpose_keys(&master, &replacement, &fresh, now_ms())
            .await
            .is_err(),
        "final transaction assertion refuses ignored locator update"
    );
    assert_eq!(
        envelopes(&client, true).await,
        before,
        "Source ciphertext rollback"
    );
    assert_eq!(
        envelopes(&client, false).await,
        before_b,
        "B ciphertext rollback"
    );
    assert_eq!(
        store
            .verify_sharing_purpose_material(&master)
            .await
            .expect("marker and archive rollback open under original key"),
        PurposeKeyInstallation::Ready
    );
    client
        .execute(
            "DROP TRIGGER fixture_ignore_locator_rewrap",
            hiqlite::params!(),
        )
        .await
        .expect("remove adversarial fixture");
    let fresh = observe_purpose_key_members_for_contract(&client, local, &master)
        .await
        .expect("fresh floor after rollback")
        .expect("ready");
    store
        .rewrap_sharing_purpose_keys(&master, &replacement, &fresh, now_ms())
        .await
        .expect("one guarded complete rewrap");
    assert_eq!(
        store
            .verify_sharing_purpose_material(&replacement)
            .await
            .expect("complete replacement opens"),
        PurposeKeyInstallation::Ready
    );
    assert!(store
        .verify_sharing_purpose_material(&master)
        .await
        .is_err());
    let after = envelopes(&client, true).await;
    let after_b = envelopes(&client, false).await;
    let archive_after = client
        .query_consistent_map::<Envelope, _>(
            "SELECT envelope AS payload FROM sharing_purpose_key_archive ORDER BY purpose",
            hiqlite::params!(),
        )
        .await
        .expect("new archive snapshot")
        .into_iter()
        .map(|row| row.0)
        .collect::<Vec<_>>();
    assert_ne!(before, after);
    assert_ne!(before_b, after_b);
    assert_ne!(archive_before, archive_after);
    // A partially replaced archive must refuse complete rewrap without writes.
    client
        .execute(
            "UPDATE sharing_purpose_key_archive SET envelope=$1 WHERE purpose='catalogue_revision'",
            hiqlite::params!(archive_before[0].as_str()),
        )
        .await
        .expect("mixed-master fixture");
    assert!(store
        .verify_sharing_purpose_material(&replacement)
        .await
        .is_err());
    assert!(store
        .rewrap_sharing_purpose_keys(&master, &replacement, &fresh, now_ms())
        .await
        .is_err());
    assert_eq!(after, envelopes(&client, true).await);
    client
        .execute(
            "UPDATE sharing_purpose_key_archive SET envelope=$1 WHERE purpose='catalogue_revision'",
            hiqlite::params!(archive_after[0].as_str()),
        )
        .await
        .expect("restore fixture exact row");
    client.execute("UPDATE cluster_node_capabilities SET last_seen_at=last_seen_at-1 WHERE node_id=$1 AND capability=$2",hiqlite::params!(format!("voter-{}",roster[1]),format!("sharing_purpose_master_v1:{}",master.sharing_purpose_master_fingerprint()))).await.expect("stale selected master");
    assert!(
        observe_purpose_key_members_for_contract(&client, local, &master)
            .await
            .expect("fresh check")
            .is_none()
    );
    assert!(store
        .install_sharing_purpose_keys(&CredentialKey::from_bytes([42; 32]), &witness, now_ms())
        .await
        .is_err());
    assert_eq!(after, envelopes(&client, true).await);
}
