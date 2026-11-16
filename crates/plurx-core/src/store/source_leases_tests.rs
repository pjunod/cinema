//! Source leases and per-node source capacity (source-lease plan steps 3
//! and 4): docs/server/SOURCE-LEASE-TABLE-AND-NODE-CAPACITY-BUILD.md §8.
//! Every test here fails against the code before the build; each names the
//! production change it pins.

use super::background_jobs::*;
use super::sqlite::SqliteStore;
use super::SettingsStore;

fn job(index: usize, live: bool, file_id: i64, now_ms: i64) -> EnqueueJob {
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::FragmentIndexBuild {
            file_id,
            source_generation: format!("source:{file_id}"),
            source_size: 100,
            source_mtime: 1,
            source_sha256: "d".repeat(64),
            cache_key: format!("{index:064}"),
            pipeline_digest: "a".repeat(64),
        },
        dedupe_key: format!("fragment:{index}"),
        priority: 1,
        not_before_ms: now_ms,
        now_ms,
        request: JobRequest {
            scope: if live { "playback-artifact" } else { "user:1" }.to_owned(),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: "b".repeat(64),
            consumer_kind: if live {
                "playback_fragment"
            } else {
                "analysis"
            }
            .to_owned(),
            consumer_ref: format!("analysis:{index}"),
            target_node_id: live.then(|| "node-a".to_owned()),
            deadline_ms: live.then_some(600_000),
            retain_identity: false,
        },
    }
}

fn claim_on(job_id: &str, revision: i64, node: &str, now_ms: i64) -> ClaimJob {
    ClaimJob {
        job_id: job_id.to_owned(),
        expected_revision: revision,
        node_id: node.to_owned(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::FragmentIndexBuild,
        payload_version: 1,
        now_ms,
        dispatched_at_ms: now_ms,
    }
}

async fn claimed(store: &SqliteStore, request: ClaimJob) -> Option<BackgroundJob> {
    match store.claim_job(request).await.expect("claim") {
        ClaimOutcome::Claimed { job } => Some(*job),
        ClaimOutcome::Contended => None,
        outcome => panic!("unexpected claim outcome {outcome:?}"),
    }
}

async fn enqueued(store: &SqliteStore, index: usize, live: bool, now_ms: i64) -> String {
    let request = job(index, live, 1, now_ms);
    let id = request.id.clone();
    store.enqueue_job(request).await.expect("enqueue");
    id
}

/// One fixture statement through the store's own SQL path. The path binds
/// one JSON body; a statement that does not read it binds it in a CTE.
async fn sql(store: &SqliteStore, statement: &str) -> Vec<String> {
    let statement = if statement.contains("$1") {
        statement.to_owned()
    } else {
        format!("WITH body AS (SELECT $1 AS json) {statement}")
    };
    QueueSql::queue_sql(store, statement, "{}".into(), true, true)
        .await
        .expect("fixture statement")
}

async fn readers_per_node(store: &SqliteStore, value: &str) {
    store
        .put_setting(super::keys::JOB_SOURCE_READERS_PER_NODE, value)
        .await
        .expect("readers per node");
}

async fn lease(
    store: &SqliteStore,
    id: &str,
    kind: &str,
    node: &str,
    phase: &str,
    projection: &str,
    expires: i64,
) {
    sql(store, &format!("INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
        VALUES('{id}','{kind}','{id}',1,'{node}','{{}}','[\"source_io\"]','{phase}',{projection},1000,1000,{expires})")).await;
}

/// Mutation: count a lease regardless of its expiry (`lease_counted!`).
#[tokio::test]
async fn capacity_frees_without_a_sweep() {
    let store = SqliteStore::open_in_memory().expect("store");
    let now = 200_000;
    // Two readers whose leases expired 61 s ago; no sweep has run.
    lease(
        &store,
        "job:dead-1:1",
        "job",
        "node-a",
        "active",
        "NULL",
        now - 61_000,
    )
    .await;
    lease(
        &store,
        "job:dead-2:1",
        "job",
        "node-a",
        "active",
        "NULL",
        now - 61_000,
    )
    .await;
    let id = enqueued(&store, 0, false, now).await;
    assert!(
        claimed(&store, claim_on(&id, 0, "node-a", now))
            .await
            .is_some(),
        "capacity frees at expiry plus the grace, sweep or not"
    );
    // Inside the grace the same two readers still hold the node.
    let store = SqliteStore::open_in_memory().expect("store");
    lease(
        &store,
        "job:dying-1:1",
        "job",
        "node-a",
        "active",
        "NULL",
        now - 59_000,
    )
    .await;
    lease(
        &store,
        "job:dying-2:1",
        "job",
        "node-a",
        "active",
        "NULL",
        now - 59_000,
    )
    .await;
    let id = enqueued(&store, 0, false, now).await;
    assert!(claimed(&store, claim_on(&id, 0, "node-a", now))
        .await
        .is_none());
}

/// Mutation: drop `held.node_id=N` from the holder counts.
#[tokio::test]
async fn node_cap_is_per_node() {
    let store = SqliteStore::open_in_memory().expect("store");
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(enqueued(&store, index, false, 1_000).await);
    }
    let live = enqueued(&store, 3, true, 1_000).await;
    for id in &ids[..2] {
        assert!(claimed(&store, claim_on(id, 0, "node-a", 1_001))
            .await
            .is_some());
    }
    assert!(
        claimed(&store, claim_on(&ids[2], 0, "node-a", 1_002))
            .await
            .is_none(),
        "node-a runs its two background readers"
    );
    assert!(
        claimed(&store, claim_on(&live, 0, "node-a", 1_002))
            .await
            .is_some(),
        "the third slot is kept for live demand"
    );
    assert!(
        claimed(&store, claim_on(&ids[2], 0, "node-b", 1_003))
            .await
            .is_some(),
        "node-b's readers are its own"
    );
}

/// Mutation: hard-code the node limit to 2 (`node_source_readers!`).
#[tokio::test]
async fn setting_changes_the_next_claim() {
    let store = SqliteStore::open_in_memory().expect("store");
    let ids = [
        enqueued(&store, 0, false, 1_000).await,
        enqueued(&store, 1, false, 1_000).await,
        enqueued(&store, 2, false, 1_000).await,
    ];
    readers_per_node(&store, "1").await;
    let first = claimed(&store, claim_on(&ids[0], 0, "node-a", 1_001))
        .await
        .expect("one reader");
    assert!(claimed(&store, claim_on(&ids[1], 0, "node-a", 1_002))
        .await
        .is_none());
    readers_per_node(&store, "3").await;
    let second = claimed(&store, claim_on(&ids[1], 0, "node-a", 1_003))
        .await
        .expect("raised");
    let third = claimed(&store, claim_on(&ids[2], 0, "node-a", 1_004))
        .await
        .expect("raised");
    // Lowering below what runs refuses nothing already running.
    readers_per_node(&store, "1").await;
    let renewed = store
        .renew_jobs(RenewJobs {
            tokens: [first, second, third]
                .into_iter()
                .map(|job| job.token.expect("token"))
                .collect(),
            now_ms: 2_000,
        })
        .await
        .expect("renew");
    assert!(renewed
        .iter()
        .all(|outcome| matches!(outcome, RenewOutcome::Renewed { .. })));
    let leases = sql(
        &store,
        "SELECT json_object('n',count(*)) AS result_json FROM source_leases WHERE owner_kind='job'",
    )
    .await;
    assert_eq!(
        leases,
        vec![r#"{"n":3}"#.to_owned()],
        "no reader is evicted"
    );
}

/// Mutations: scope the window to the claimant's node; count playback.
#[tokio::test]
async fn playback_window_blocks_every_node_and_is_never_counted() {
    let store = SqliteStore::open_in_memory().expect("store");
    let id = enqueued(&store, 0, false, 1_000).await;
    lease(
        &store,
        "playback-a",
        "playback",
        "node-a",
        "refill",
        "'{\"source_quiescent\":false}'",
        30_000,
    )
    .await;
    assert!(
        claimed(&store, claim_on(&id, 0, "node-b", 1_001))
            .await
            .is_none(),
        "a refilling playback on node-a blocks background reads on node-b (unmapped storage)"
    );
    sql(
        &store,
        "DELETE FROM source_leases WHERE lease_id='playback-a'",
    )
    .await;
    for index in 0..4 {
        lease(
            &store,
            &format!("playback-{index}"),
            "playback",
            "node-a",
            "source_idle",
            "'{\"source_quiescent\":true}'",
            30_000,
        )
        .await;
    }
    let other = enqueued(&store, 1, false, 1_000).await;
    assert!(claimed(&store, claim_on(&id, 0, "node-a", 1_002))
        .await
        .is_some());
    assert!(
        claimed(&store, claim_on(&other, 0, "node-a", 1_003))
            .await
            .is_some(),
        "four idle playbacks leave node-a's two background readers"
    );
}

/// Mutations: remove `source_lease_job_claim`, `_renew`, `_cancelling`, `_settled`.
#[tokio::test]
async fn job_lease_follows_the_job() {
    let store = SqliteStore::open_in_memory().expect("store");
    let id = enqueued(&store, 0, false, 1_000).await;
    let job = claimed(&store, claim_on(&id, 0, "node-a", 1_001))
        .await
        .expect("claim");
    let expiry = |rows: Vec<String>| rows.first().cloned();
    let row = |id: &str| {
        format!("SELECT json_object('expires',expires_at_ms,'node',node_id) AS result_json FROM source_leases WHERE lease_id='job:{id}:1'")
    };
    assert_eq!(
        expiry(sql(&store, &row(&id)).await),
        Some(format!(
            r#"{{"expires":{},"node":"node-a"}}"#,
            job.token.as_ref().expect("token").lease_expires_ms
        )),
        "the claim writes the lease, with the job's expiry"
    );
    let renewed = store
        .renew_jobs(RenewJobs {
            tokens: vec![job.token.clone().expect("token")],
            now_ms: 5_000,
        })
        .await
        .expect("renew");
    let RenewOutcome::Renewed { token } = &renewed[0] else {
        panic!("renewed")
    };
    assert_eq!(
        expiry(sql(&store, &row(&id)).await),
        Some(format!(
            r#"{{"expires":{},"node":"node-a"}}"#,
            token.lease_expires_ms
        )),
        "renewal moves it"
    );
    store
        .settle_job(SettleJob {
            token: token.clone(),
            settlement: JobSettlement::Fail {
                error_code: "test".into(),
            },
            now_ms: 6_000,
        })
        .await
        .expect("settle");
    assert!(
        sql(&store, &row(&id)).await.is_empty(),
        "the owner's settle releases it"
    );
    // Cancelling: the reader is being stopped; the grace still charges it.
    let stopping = enqueued(&store, 2, false, 7_000).await;
    claimed(&store, claim_on(&stopping, 0, "node-a", 7_001))
        .await
        .expect("claim");
    store
        .cancel_job(CancelJob {
            job_id: stopping.clone(),
            now_ms: 7_500,
        })
        .await
        .expect("cancel");
    assert_eq!(
        expiry(sql(&store, &row(&stopping)).await),
        Some(r#"{"expires":7500,"node":"node-a"}"#.to_owned()),
        "cancelling lapses it now"
    );
    // An abandoned job (its lease expired) keeps the reader charged until the grace.
    let other = enqueued(&store, 1, false, 10_000).await;
    claimed(&store, claim_on(&other, 0, "node-a", 10_001))
        .await
        .expect("claim");
    let again = claimed(&store, claim_on(&other, 1, "node-b", 10_001 + JOB_LEASE_MS))
        .await
        .expect("takeover");
    assert_eq!(again.fence, 2);
    assert_eq!(
        sql(&store, &row(&other)).await.len(),
        1,
        "the old reader is still charged"
    );
    store
        .reap_expired_source_leases(10_001 + JOB_LEASE_MS + 59_000)
        .await
        .expect("sweep");
    assert_eq!(sql(&store, &row(&other)).await.len(), 1, "inside the grace");
    store
        .reap_expired_source_leases(10_001 + JOB_LEASE_MS + 61_000)
        .await
        .expect("sweep");
    assert!(
        sql(&store, &row(&other)).await.is_empty(),
        "collected past the grace"
    );
}

/// Mutation: drop the claimant self-exclusion from `CLAIM_SQL`'s holder
/// and maintenance counts. A job taken over after its owner died does not
/// wait on its own older lease; every other claim still counts that lease.
#[tokio::test]
async fn a_successor_fence_does_not_wait_on_its_own_ghost() {
    let store = SqliteStore::open_in_memory().expect("store");
    readers_per_node(&store, "1").await;
    let ghosted = enqueued(&store, 0, false, 1_000).await;
    claimed(&store, claim_on(&ghosted, 0, "node-a", 1_001))
        .await
        .expect("claim");
    // The owner died: its lease lapsed but is inside the grace.
    let now = 1_001 + JOB_LEASE_MS + 1_000;
    let other = enqueued(&store, 1, false, now).await;
    assert!(
        claimed(&store, claim_on(&other, 0, "node-a", now))
            .await
            .is_none(),
        "the ghost still charges another job's claim"
    );
    let again = claimed(&store, claim_on(&ghosted, 1, "node-a", now))
        .await
        .expect("the same job takes itself over");
    assert_eq!(again.fence, 2);
}

/// Mutation: drop `AND OLD.lease_expires_ms>NEW.updated_at_ms` from
/// `source_lease_job_settled` (an abandoned job's reader is released the
/// moment maintenance fails it, instead of at expiry plus the grace).
#[tokio::test]
async fn an_abandoned_job_keeps_its_reader_charged_through_the_grace() {
    let store = SqliteStore::open_in_memory().expect("store");
    let id = enqueued(&store, 0, false, 1_000).await;
    let job = claimed(&store, claim_on(&id, 0, "node-a", 1_001))
        .await
        .expect("claim");
    let expires = job.token.as_ref().expect("token").lease_expires_ms;
    let row =
        format!("SELECT lease_id AS result_json FROM source_leases WHERE lease_id='job:{id}:1'");
    // The owner stopped renewing and its waiter has no attempt left:
    // maintenance fails the running attempt.
    sql(
        &store,
        &format!("UPDATE background_job_waiters SET failed_attempts=attempt_limit WHERE job_id='{id}' RETURNING 'true' AS result_json"),
    )
    .await;
    store
        .maintain_jobs(expires + 1_000)
        .await
        .expect("maintain");
    let state = sql(
        &store,
        &format!("SELECT state AS result_json FROM background_jobs WHERE id='{id}'"),
    )
    .await;
    assert_ne!(
        state.first().map(String::as_str),
        Some("running"),
        "maintenance ended the abandoned attempt"
    );
    assert_eq!(
        sql(&store, &row).await.len(),
        1,
        "the reader may still be running: its lease stays"
    );
    store
        .reap_expired_source_leases(expires + 59_000)
        .await
        .expect("sweep");
    assert_eq!(sql(&store, &row).await.len(), 1, "inside the grace");
    store
        .reap_expired_source_leases(expires + 61_000)
        .await
        .expect("sweep");
    assert!(
        sql(&store, &row).await.is_empty(),
        "collected past the grace"
    );
}

/// Mutation: drop the storage-domain branch of the holder counts.
#[tokio::test]
async fn domain_cap_still_binds() {
    let store = SqliteStore::open_in_memory().expect("store");
    sql(
        &store,
        "INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/nas\"]')",
    )
    .await;
    sql(&store, "INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie')").await;
    sql(
        &store,
        "INSERT INTO files(id,item_id,path,size,mtime) VALUES(1,1,'/nas/movie.mkv',100,1)",
    )
    .await;
    sql(&store, "INSERT INTO background_storage_domains(library_id,root_path,domain_id) VALUES(1,'/nas','nas')").await;
    let first = enqueued(&store, 0, false, 1_000).await;
    let second = enqueued(&store, 1, false, 1_000).await;
    assert!(claimed(&store, claim_on(&first, 0, "node-a", 1_001))
        .await
        .is_some());
    assert!(
        claimed(&store, claim_on(&second, 0, "node-b", 1_002))
            .await
            .is_none(),
        "the mapped domain's one background reader is taken, on any node"
    );
    store
        .put_setting(
            &format!("{}nas", super::keys::JOB_DOMAIN_SOURCE_READERS_PREFIX),
            "2",
        )
        .await
        .expect("domain readers");
    assert!(claimed(&store, claim_on(&second, 0, "node-b", 1_003))
        .await
        .is_some());
}

/// Mutation: carry every v1 custody row, with its own expiry.
#[tokio::test]
async fn expand_carries_live_holders_and_drops_ownerless_custody() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("v132.db");
    {
        let conn = rusqlite::Connection::open(&path).expect("v132");
        SqliteStore::apply_migrations_for_test(
            &conn,
            super::source_leases::PRE_LEASE_VERSION as i64,
        )
        .expect("v132 catalogue");
        let far = 10_i64.pow(13);
        conn.execute_batch(&format!("
          INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,owner_node_id,owner_boot_id,claim_id,fence,revision,lease_expires_ms,not_before_ms,created_at_ms,updated_at_ms)
            VALUES('live','fragment_index_build',1,'{{}}','live',1,'running','node-a','boot','claim',1,1,{far},0,1000,1000),
                  ('paused','fragment_index_build',1,'{{}}','paused',1,'running','node-a','boot','claim',1,1,{far},0,1000,1000),
                  ('done','fragment_index_build',1,'{{}}','done',1,'succeeded',NULL,NULL,NULL,1,1,NULL,0,1000,1000);
          INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) VALUES('source_io',0,'live',1,{far});
          INSERT INTO background_source_pauses(job_id,fence,node_id,boot_id,claim_id,resource_keys_json,phase,paused_at_ms)
            VALUES('paused',1,'node-a','boot','claim','[\"source_io\"]','paused',1000);
          INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms,settlement_proof)
            VALUES('viewer','{{\"node_id\":\"node-a\",\"lifetime_id\":\"life\"}}','[\"source_io\"]',1,'source_idle','{{\"source_quiescent\":true}}',1000,1000,{far},NULL);
          INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms,settlement_proof)
            VALUES('lapsed','lapsed','{{\"lease\":{{\"owner_node_id\":\"node-b\"}}}}','[\"source_io\"]','lease','subtitle-read-attest:node-b:1',1,'active',1000,1000,{far},NULL),
                  ('joined','joined','{{\"node_id\":\"node-b\"}}','[\"source_io\"]','offline','gone',1,'active',1000,1000,{far},'joined');
        ")).expect("v1 holders");
    }
    let store = SqliteStore::open(&path).expect("expand and contract");
    let leases = sql(&store, "SELECT json_group_array(lease_id) AS result_json FROM (SELECT lease_id FROM source_leases ORDER BY lease_id)").await;
    assert_eq!(
        leases,
        vec![r#"["job:live:1","job:paused:1","viewer"]"#.to_owned()],
        "live job, paused job and playback carry; ownerless and settled custody do not"
    );
    let paused = sql(
        &store,
        "SELECT phase AS result_json FROM source_leases WHERE lease_id='job:paused:1'",
    )
    .await;
    assert_eq!(paused, vec!["paused".to_owned()], "resume finds its row");
    let id = enqueued(&store, 0, false, 2_000).await;
    assert!(
        claimed(&store, claim_on(&id, 0, "node-b", 2_001))
            .await
            .is_some(),
        "node-b holds nothing after the expand"
    );
}

/// Mutation: remove the bridge triggers.
#[test]
fn old_owner_rows_bridge_until_contract() {
    let conn = rusqlite::Connection::open_in_memory().expect("v134");
    SqliteStore::apply_migrations_for_test(&conn, super::source_leases::EXPANDED_VERSION as i64)
        .expect("expanded catalogue");
    conn.execute_batch("INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms)
        VALUES('old-binary','{\"node_id\":\"node-a\",\"lifetime_id\":\"life\"}','[\"source_io\"]',1,'startup','{\"source_quiescent\":false}',1000,1000,30000);")
        .expect("a v1 binary registers playback");
    let blocks = |conn: &rusqlite::Connection| -> bool {
        conn.query_row(
            &format!(
                "SELECT {}",
                super::source_leases::foreground_window("'source_io'", "2000")
            ),
            [],
            |row| row.get(0),
        )
        .expect("window")
    };
    assert!(
        blocks(&conn),
        "the old binary's playback blocks upgraded claims"
    );
    conn.execute_batch("UPDATE foreground_source_owners SET phase='source_idle',projection_json='{\"source_quiescent\":true}',fence=2 WHERE owner_id='old-binary';")
        .expect("projection");
    assert!(!blocks(&conn), "its projection follows");
    conn.execute_batch("UPDATE foreground_source_owners SET phase='refill',projection_json='{\"source_quiescent\":false}',fence=3 WHERE owner_id='old-binary'; DELETE FROM foreground_source_owners;")
        .expect("release");
    assert!(!blocks(&conn), "its release follows");
}

/// Mutation: drop `source_lease_bridge_out_*` (an upgraded node's playback
/// is invisible to a node still on the v1 binary during the roll).
#[test]
fn new_playback_is_mirrored_to_v1_owners_until_contract() {
    for recursive in ["OFF", "ON"] {
        let conn = rusqlite::Connection::open_in_memory().expect("v134");
        SqliteStore::apply_migrations_for_test(
            &conn,
            super::source_leases::EXPANDED_VERSION as i64,
        )
        .expect("expanded catalogue");
        conn.pragma_update(None, "recursive_triggers", recursive)
            .expect("trigger recursion");
        let v1 = |conn: &rusqlite::Connection| -> Vec<(String, i64, String, i64)> {
            conn.prepare("SELECT owner_id,fence,phase,lease_expires_at_ms FROM foreground_source_owners ORDER BY owner_id")
                .expect("v1 owners")
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                .expect("v1 rows")
                .collect::<Result<_, _>>()
                .expect("v1 owners read")
        };
        let leases = |conn: &rusqlite::Connection| -> i64 {
            conn.query_row("SELECT count(*) FROM source_leases", [], |row| row.get(0))
                .expect("leases")
        };
        conn.execute_batch("INSERT INTO source_leases(lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
            VALUES('new-binary','playback','life',1,'node-b','{\"node_id\":\"node-b\"}','[\"source_io\"]','startup','{\"source_quiescent\":false}',1000,1000,30000);")
            .expect("an upgraded node registers playback");
        assert_eq!(
            v1(&conn),
            [("new-binary".to_owned(), 1, "startup".to_owned(), 30000)],
            "recursive_triggers={recursive}: a v1 node's claims see it"
        );
        assert_eq!(leases(&conn), 1, "recursive_triggers={recursive}: no echo");
        conn.execute_batch("UPDATE source_leases SET phase='refill',fence=2,expires_at_ms=40000,renewed_at_ms=2000 WHERE lease_id='new-binary';")
            .expect("renew");
        assert_eq!(
            v1(&conn),
            [("new-binary".to_owned(), 2, "refill".to_owned(), 40000)],
            "recursive_triggers={recursive}: renewal follows"
        );
        // The v1 binary's own playback still bridges the other way.
        conn.execute_batch("INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,started_at_ms,updated_at_ms,lease_expires_at_ms)
            VALUES('old-binary','{\"node_id\":\"node-a\"}','[\"source_io\"]',1,'startup','{}',1000,1000,30000);")
            .expect("a v1 node registers playback");
        assert_eq!(leases(&conn), 2, "recursive_triggers={recursive}");
        conn.execute_batch("DELETE FROM source_leases WHERE lease_id='new-binary';")
            .expect("release");
        assert_eq!(
            v1(&conn),
            [("old-binary".to_owned(), 1, "startup".to_owned(), 30000)],
            "recursive_triggers={recursive}: release follows"
        );
        conn.execute_batch(super::source_leases::CONTRACT)
            .expect("contract drops the bridge with the v1 owners");
    }
}

/// Mutation: drop the readiness guard from the contract.
#[test]
fn contract_waits_for_every_node() {
    let conn = rusqlite::Connection::open_in_memory().expect("v134");
    SqliteStore::apply_migrations_for_test(&conn, super::source_leases::EXPANDED_VERSION as i64)
        .expect("expanded catalogue");
    conn.execute_batch("CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,last_seen_at INTEGER,removed_at INTEGER);
        CREATE TABLE cluster_node_join_staging(node_id TEXT PRIMARY KEY);
        CREATE TABLE cluster_node_capabilities(node_id TEXT,capability TEXT,last_seen_at INTEGER,PRIMARY KEY(node_id,capability));
        INSERT INTO cluster_nodes VALUES('node-a',10,NULL),('node-b',20,NULL);
        INSERT INTO cluster_node_capabilities VALUES('node-a','source_leases_v2',10);")
        .expect("membership");
    let contract = |conn: &rusqlite::Connection| {
        let tx = conn.unchecked_transaction().expect("transaction");
        let guard = format!(
            "SELECT CASE WHEN {} THEN 1 ELSE json('source lease contract waits for every node') END",
            crate::cluster::membership::source_leases_ready_predicate()
        );
        let applied = tx
            .query_row(&guard, [], |_| Ok(()))
            .and_then(|()| tx.execute_batch(super::source_leases::CONTRACT));
        match applied {
            Ok(()) => tx.commit().map(|()| true).expect("commit"),
            Err(_) => false,
        }
    };
    assert!(!contract(&conn), "node-b has not run a lease-aware binary");
    let v1: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='foreground_source_owners'",
            [],
            |row| row.get(0),
        )
        .expect("v1");
    assert_eq!(v1, 1, "the v1 objects remain for node-b");
    conn.execute_batch(
        "INSERT INTO cluster_node_capabilities VALUES('node-b','source_leases_v2',20);",
    )
    .expect("node-b heartbeats the new build");
    assert!(contract(&conn));
    let version: i64 = conn
        .query_row("SELECT version FROM source_lease_schema", [], |row| {
            row.get(0)
        })
        .expect("marker");
    assert_eq!(version, 3);
}

/// Mutation: old source binaries continue writing custody after the lease expand.
#[test]
fn old_custody_phase_bridge_tracks_renewal_pause_offline_service_and_release() {
    for recursive in ["OFF", "ON"] {
        let conn = rusqlite::Connection::open_in_memory().expect("store");
        SqliteStore::apply_migrations_for_test(
            &conn,
            super::source_leases::EXPANDED_VERSION as i64,
        )
        .expect("expanded catalogue");
        conn.pragma_update(None, "recursive_triggers", recursive)
            .expect("recursive triggers");
        conn.execute_batch(
            r#"PRAGMA foreign_keys=OFF;
             INSERT INTO settings(key,value) VALUES('jobs.source_readers_per_node','16');
             INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[]');
             INSERT INTO items(id,library_id,kind,title,sort_title)
               VALUES(1,1,'movie','Movie','Movie');
             INSERT INTO files(id,item_id,path,size,mtime)
               VALUES(1,1,'/movie.mkv',100,7);
             INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms)
               VALUES('subtitle-read-attest:node-a:1','node-a',1,1,30000,1000);
             INSERT INTO offline_packages(
               id,request_id,user_id,file_id,node_id,source_path,source_size,source_mtime,
               target_height,subtitle_mode,state,phase,claim_generation,expires_at)
               VALUES('offline-package','request',1,1,'node-a','/movie.mkv',100,7,
                 720,'none','preparing','download',1,100);
             INSERT INTO analysis_requests(
               request_id,file_id,source_size,source_mtime,component,video_identity,
               pipeline_version,requested_generation,force_rebuild,target_node_id,state,
               owner_node_id,fence,lease_expires_ms,not_before_ms,created_at_ms,updated_at_ms)
               VALUES('coordinated',1,100,7,'fragment_index',
                 'coordination-video:1:identity','coordination-contract:1','generation',
                 0,'node-a','running','node-a',1,30000,0,1000,1000);
             INSERT INTO analysis_coordination_roles(
               request_id,file_id,source_size,source_mtime,origin_node_id,object_version,
               video_identity,contract_version,requested_generation,phase_sequence,
               active_custody_id,phase_registered,physical_node_id,physical_fence)
               VALUES('coordinated',1,100,7,'node-a','object','identity',1,'generation',
                 1,'phase-custody-uuid',1,'node-a',1);
             INSERT INTO background_source_custody(
               custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,
               phase,started_at_ms,updated_at_ms,lease_expires_at_ms)
               VALUES
               ('phase-custody-uuid','analysis-owner','{"node_id":"node-a"}',
                 '["source_io"]','analysis','coordinated',1,'active',1000,1000,30000),
               ('attestation-custody-uuid','lease:subtitle-read-attest:node-a:1:1',
                 '{"lease":{"owner_node_id":"node-a"},"file_id":1,"source_size":100,"source_mtime":7}',
                 '["source_io"]','lease','subtitle-read-attest:node-a:1',1,'active',1000,1000,30000),
               ('offline-custody-uuid','offline:offline-package:1',
                 '{"kind":"offline","node_id":"node-a"}',
                 '["source_io"]','offline','offline-package',1,'active',1000,1000,30000);"#,
        )
        .expect("v1 writes after expand");

        let leases: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM source_leases WHERE owner_kind IN ('analysis','lease','offline')"#,
                [],
                |row| row.get(0),
            )
            .expect("bridged holders");
        assert_eq!(leases, 3, "recursive_triggers={recursive}");
        let phase_id: String = conn
            .query_row(
                r#"SELECT lease_id FROM source_leases WHERE owner_kind='analysis'"#,
                [],
                |row| row.get(0),
            )
            .expect("physical phase id");
        assert_eq!(
            phase_id, "phase-custody-uuid",
            "preserve exact phase identity"
        );
        let attestation_expiry: i64 = conn
            .query_row(
                r#"SELECT expires_at_ms FROM source_leases WHERE owner_kind='lease'"#,
                [],
                |row| row.get(0),
            )
            .expect("attestation lease");
        assert_eq!(attestation_expiry, 30000);
        let offline_id: String = conn
            .query_row(
                r#"SELECT lease_id FROM source_leases WHERE owner_kind='offline'"#,
                [],
                |row| row.get(0),
            )
            .expect("offline lease");
        assert_eq!(offline_id, "offline:offline-package:1");

        conn.execute(
            r#"UPDATE analysis_requests SET lease_expires_ms=50000,updated_at_ms=2000 WHERE request_id='coordinated'"#,
            [],
        )
        .expect("analysis owner renewal");
        let analysis_expiry: i64 = conn
            .query_row(
                r#"SELECT expires_at_ms FROM source_leases WHERE lease_id='phase-custody-uuid'"#,
                [],
                |row| row.get(0),
            )
            .expect("renewed analysis phase");
        assert_eq!(analysis_expiry, 50000);
        conn.execute(
            r#"UPDATE background_source_custody SET phase='paused',updated_at_ms=2100 WHERE custody_id='phase-custody-uuid'"#,
            [],
        )
        .expect("old phase pauses");
        let phase_state: String = conn
            .query_row(
                r#"SELECT phase FROM source_leases WHERE lease_id='phase-custody-uuid'"#,
                [],
                |row| row.get(0),
            )
            .expect("paused phase");
        assert_eq!(phase_state, "paused");
        conn.execute(
            r#"UPDATE background_source_custody SET phase='active',updated_at_ms=2200 WHERE custody_id='phase-custody-uuid'"#,
            [],
        )
        .expect("old phase resumes");

        conn.execute(
            r#"UPDATE job_leases SET expires_at_ms=42000,updated_at_ms=2300 WHERE resource='subtitle-read-attest:node-a:1'"#,
            [],
        )
        .expect("attestation renewal");
        let renewed_attestation: i64 = conn
            .query_row(
                r#"SELECT expires_at_ms FROM source_leases WHERE owner_kind='lease'"#,
                [],
                |row| row.get(0),
            )
            .expect("renewed attestation");
        assert_eq!(renewed_attestation, 42000);

        // The pre-upsert binary sends flattened owner fields. Its accepted
        // grant must recreate the bounded offline lease and preserve the old
        // custody UUID when the two directions meet.
        conn.execute(
            r#"INSERT INTO background_job_commands(id,operation,request_json,result_json)
             VALUES('service','source_service',
               '{"kind":"offline","package_id":"offline-package","node_id":"node-a","claim_generation":1,"owner_key":"offline:offline-package:1","now_ms":2500,"epoch_id":0,"charge_bytes":0}',
               '{"receipt_id":"grant","expires_at_ms":7500,"resource_keys":["source_io"]}')"#,
            [],
        )
        .expect("accepted offline service");
        let offline: (i64, String, String) = conn
            .query_row(
                r#"SELECT expires_at_ms,owner_json,lease_id FROM source_leases WHERE owner_kind='offline'"#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("renewed offline lease");
        assert_eq!(offline.0, 32500);
        assert_eq!(offline.2, "offline:offline-package:1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&offline.1).expect("offline owner")["kind"],
            "offline"
        );
        let preserved: String = conn
            .query_row(
                r#"SELECT custody_id FROM background_source_custody WHERE owner_key='offline:offline-package:1'"#,
                [],
                |row| row.get(0),
            )
            .expect("preserved legacy UUID");
        assert_eq!(preserved, "offline-custody-uuid");

        conn.execute(
            r#"DELETE FROM background_source_custody WHERE custody_id='attestation-custody-uuid'"#,
            [],
        )
        .expect("legacy reader release");
        let after_release: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM source_leases WHERE owner_kind='lease'"#,
                [],
                |row| row.get(0),
            )
            .expect("lease released with legacy custody");
        assert_eq!(after_release, 0);
        conn.execute(
            r#"UPDATE offline_packages SET state='ready' WHERE id='offline-package'"#,
            [],
        )
        .expect("offline owner ends");
        let offline_after_end: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM source_leases WHERE owner_kind='offline'"#,
                [],
                |row| row.get(0),
            )
            .expect("offline lease end");
        assert_eq!(offline_after_end, 0);

        conn.execute_batch(super::source_leases::CONTRACT)
            .expect("contract drops temporary bridge");
        let bridges: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name IN (
                   'source_lease_custody_insert','source_lease_custody_update',
                   'source_lease_custody_delete','source_lease_custody_out_insert',
                   'source_lease_custody_out_update','source_lease_custody_out_delete',
                   'source_lease_offline_service_admission')"#,
                [],
                |row| row.get(0),
            )
            .expect("bridge inventory");
        assert_eq!(bridges, 0);
    }
}

/// Mutation: new independent leases are visible to v1 source admission and
/// their release removes only the canonical mirror, preserving an old UUID.
#[test]
fn source_lease_custody_bridge_round_trips_without_replacing_old_uuid() {
    for recursive in ["OFF", "ON"] {
        let conn = rusqlite::Connection::open_in_memory().expect("store");
        SqliteStore::apply_migrations_for_test(
            &conn,
            super::source_leases::EXPANDED_VERSION as i64,
        )
        .expect("expanded catalogue");
        conn.pragma_update(None, "recursive_triggers", recursive)
            .expect("recursive triggers");
        conn.execute_batch(
            r#"PRAGMA foreign_keys=OFF;
             INSERT INTO settings(key,value) VALUES('jobs.source_readers_per_node','16');
             INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[]');
             INSERT INTO items(id,library_id,kind,title,sort_title)
               VALUES(1,1,'movie','Movie','Movie');
             INSERT INTO files(id,item_id,path,size,mtime)
               VALUES(1,1,'/movie.mkv',100,7);
             INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms)
               VALUES('subtitle-read-attest:node-a:1','node-a',1,1,30000,1000);
             INSERT INTO offline_packages(
               id,request_id,user_id,file_id,node_id,source_path,source_size,source_mtime,
               target_height,subtitle_mode,state,phase,claim_generation,expires_at)
               VALUES('offline-package','request',1,1,'node-a','/movie.mkv',100,7,
                 720,'none','preparing','download',1,100);"#,
        )
        .expect("owner fixtures");

        conn.execute_batch(
            r#"INSERT INTO source_leases(
               lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,
               started_at_ms,renewed_at_ms,expires_at_ms)
             VALUES
               ('lease:subtitle-read-attest:node-a:1:1','lease','subtitle-read-attest:node-a:1',
                 1,'node-a','{"lease":{"owner_node_id":"node-a"},"file_id":1,"source_size":100,"source_mtime":7}',
                 '["source_io"]','active',1000,1000,30000),
               ('offline:offline-package:1','offline','offline-package',1,'node-a',
                 '{"kind":"offline","node_id":"node-a","package_id":"offline-package","claim_generation":1}',
                 '["source_io"]','active',1000,1000,31000);"#,
        )
        .expect("new binary leases");

        let legacy_occupancy: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM source_io_occupancy WHERE resource_key='source_io' AND owner_kind IN ('lease','offline') AND active_occupancy=1"#,
                [],
                |row| row.get(0),
            )
            .expect("v1 occupancy");
        assert_eq!(legacy_occupancy, 2, "recursive_triggers={recursive}");
        let projected: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM background_source_custody WHERE custody_id=owner_key"#,
                [],
                |row| row.get(0),
            )
            .expect("canonical rows");
        assert_eq!(projected, 2);

        conn.execute(
            r#"UPDATE job_leases SET expires_at_ms=40000,updated_at_ms=2000 WHERE resource='subtitle-read-attest:node-a:1'"#,
            [],
        )
        .expect("attestation owner renewal");
        let mirrored_expiry: i64 = conn
            .query_row(
                r#"SELECT lease_expires_at_ms FROM background_source_custody WHERE owner_key='lease:subtitle-read-attest:node-a:1:1'"#,
                [],
                |row| row.get(0),
            )
            .expect("mirrored owner renewal");
        assert_eq!(mirrored_expiry, 40000);

        conn.execute(
            r#"DELETE FROM source_leases WHERE lease_id='lease:subtitle-read-attest:node-a:1:1'"#,
            [],
        )
        .expect("new lease release");
        let after_new_release: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM background_source_custody WHERE owner_key='lease:subtitle-read-attest:node-a:1:1'"#,
                [],
                |row| row.get(0),
            )
            .expect("new custody release");
        assert_eq!(after_new_release, 0);
        conn.execute(
            r#"DELETE FROM source_leases WHERE lease_id='offline:offline-package:1'"#,
            [],
        )
        .expect("new offline release");
        let occupancy_after_release: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM source_io_occupancy WHERE resource_key='source_io' AND owner_kind IN ('lease','offline') AND active_occupancy=1"#,
                [],
                |row| row.get(0),
            )
            .expect("old admission release");
        assert_eq!(occupancy_after_release, 0);

        conn.execute(
            r#"INSERT INTO background_source_custody(
               custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,
               phase,started_at_ms,updated_at_ms,lease_expires_at_ms)
             VALUES('legacy-owner-uuid','lease:subtitle-read-attest:node-a:1:1',
               '{"lease":{"owner_node_id":"node-a"},"file_id":1,"source_size":100,"source_mtime":7}',
               '["source_io"]','lease','subtitle-read-attest:node-a:1',1,'active',1000,1000,30000)"#,
            [],
        )
        .expect("legacy UUID custody");
        let source_lease_id: String = conn
            .query_row(
                r#"SELECT lease_id FROM source_leases WHERE owner_kind='lease'"#,
                [],
                |row| row.get(0),
            )
            .expect("canonical new schema id");
        assert_eq!(source_lease_id, "lease:subtitle-read-attest:node-a:1:1");
        let custody_id: String = conn
            .query_row(
                r#"SELECT custody_id FROM background_source_custody WHERE owner_key='lease:subtitle-read-attest:node-a:1:1'"#,
                [],
                |row| row.get(0),
            )
            .expect("old uuid remains canonical owner");
        assert_eq!(custody_id, "legacy-owner-uuid");
        conn.execute(
            r#"DELETE FROM source_leases WHERE lease_id='lease:subtitle-read-attest:node-a:1:1'"#,
            [],
        )
        .expect("sweep-style lease removal");
        let old_uuid_survives: i64 = conn
            .query_row(
                r#"SELECT count(*) FROM background_source_custody WHERE custody_id='legacy-owner-uuid'"#,
                [],
                |row| row.get(0),
            )
            .expect("old binary custody remains");
        assert_eq!(old_uuid_survives, 1);

        conn.execute_batch(super::source_leases::CONTRACT)
            .expect("contract drops temporary bridge");
    }
}
