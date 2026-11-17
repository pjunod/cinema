//! Root-scoped source capacity: configured roots are the unit of maintenance
//! capacity, with optional aggregate storage-domain limits.
use super::*;

async fn roots_store(roots: &[&str], files: &[(i64, &str)]) -> SqliteStore {
    let store = SqliteStore::open_in_memory().expect("store");
    let paths = serde_json::to_string(roots).expect("root paths");
    sql(
        &store,
        &format!(
            "INSERT INTO libraries(id,name,kind,paths) VALUES(1,'Root fixture','movies','{paths}')"
        ),
    )
    .await;
    for (file_id, path) in files {
        sql(
            &store,
            &format!(
                "INSERT INTO items(id,library_id,kind,title,sort_title) VALUES({file_id},1,'movie','Fixture','Fixture')"
            ),
        )
        .await;
        sql(
            &store,
            &format!(
                "INSERT INTO files(id,item_id,path,size,mtime) VALUES({file_id},{file_id},'{path}',100,1)"
            ),
        )
        .await;
    }
    store
}

async fn enqueue_file(
    store: &SqliteStore,
    index: usize,
    file_id: i64,
    live: bool,
    now_ms: i64,
) -> String {
    let request = job(index, live, file_id, now_ms);
    let id = request.id.clone();
    store.enqueue_job(request).await.expect("enqueue");
    id
}

async fn claim_file(
    store: &SqliteStore,
    index: usize,
    file_id: i64,
    live: bool,
    node: &str,
    now_ms: i64,
) -> Option<String> {
    let id = enqueue_file(store, index, file_id, live, now_ms).await;
    claimed(store, claim_on(&id, 0, node, now_ms))
        .await
        .map(|_| id)
}

async fn resource_keys(store: &SqliteStore, job_id: &str) -> Vec<String> {
    sql(
        store,
        &format!(
            "SELECT resource_key FROM background_job_required_resources WHERE job_id='{job_id}' ORDER BY resource_key"
        ),
    )
    .await
}

fn root_key(library_id: i64, root: &str) -> String {
    format!(
        "source_io:root/{library_id}/{}",
        super::super::source_leases::hex_upper(root)
    )
}

async fn map_domain(store: &SqliteStore, root: &str, domain: &str) {
    sql(
        store,
        &format!(
            "INSERT INTO background_storage_domains(library_id,root_path,domain_id) VALUES(1,'{root}','{domain}')"
        ),
    )
    .await;
}

/// Two roots in the same library and mapped disk each get two maintenance
/// slots. The limit is cluster-wide per root, while one additional live read
/// can occupy the reserved third slot.
#[tokio::test]
async fn maintenance_capacity_is_independent_per_root_clusterwide_with_one_live_slot() {
    let roots = ["/media/a", "/media/b"];
    let store = roots_store(
        &roots,
        &[
            (1, "/media/a/one.mkv"),
            (2, "/media/a/two.mkv"),
            (3, "/media/a/three.mkv"),
            (4, "/media/a/live.mkv"),
            (5, "/media/b/one.mkv"),
            (6, "/media/b/two.mkv"),
            (7, "/media/b/three.mkv"),
        ],
    )
    .await;
    map_domain(&store, roots[0], "shared-disk").await;
    map_domain(&store, roots[1], "shared-disk").await;

    assert!(claim_file(&store, 0, 1, false, "node-a", 1_001)
        .await
        .is_some());
    assert!(claim_file(&store, 1, 2, false, "node-a", 1_002)
        .await
        .is_some());
    assert!(
        claim_file(&store, 2, 3, false, "node-b", 1_003)
            .await
            .is_none(),
        "a third maintenance reader on another node still hits this root's cap"
    );

    assert!(claim_file(&store, 3, 5, false, "node-a", 1_004)
        .await
        .is_some());
    assert!(claim_file(&store, 4, 6, false, "node-a", 1_005)
        .await
        .is_some());
    assert!(claim_file(&store, 5, 7, false, "node-b", 1_006)
        .await
        .is_none());

    assert!(
        claim_file(&store, 6, 4, true, "node-a", 1_007)
            .await
            .is_some(),
        "the third root slot is available to live demand"
    );
    assert!(
        claim_file(&store, 7, 3, false, "node-b", 1_008)
            .await
            .is_none(),
        "live demand uses the spare slot without raising maintenance capacity"
    );
}

/// Only the most-specific component-boundary match contributes a root key.
/// A shared optional domain key appears only for the root that is mapped.
#[tokio::test]
async fn file_resource_keys_use_longest_component_root_and_optional_domain() {
    let store = roots_store(
        &["/media", "/media/movies"],
        &[
            (1, "/media/movies/title.mkv"),
            (2, "/media/movies-old/title.mkv"),
            (3, "/media/movies/second.mkv"),
            (4, "/media/outside/one.mkv"),
            (5, "/media/movies/third.mkv"),
            (6, "/media/outside/two.mkv"),
        ],
    )
    .await;
    map_domain(&store, "/media/movies", "disk").await;
    let nested = enqueue_file(&store, 0, 1, false, 1_000).await;
    let boundary = enqueue_file(&store, 1, 2, false, 1_000).await;

    assert_eq!(
        resource_keys(&store, &nested).await,
        vec!["source_io:disk".to_owned(), root_key(1, "/media/movies")],
        "the nested root wins and contributes only its own root plus its explicit domain"
    );
    assert_eq!(
        resource_keys(&store, &boundary).await,
        vec![root_key(1, "/media")],
        "a textual prefix without a path-component boundary does not match the nested root"
    );

    let nested_second = enqueue_file(&store, 2, 3, false, 1_000).await;
    let parent_first = enqueue_file(&store, 3, 4, false, 1_000).await;
    assert!(claimed(&store, claim_on(&nested, 0, "node-a", 1_001))
        .await
        .is_some());
    assert!(
        claimed(&store, claim_on(&nested_second, 0, "node-a", 1_002))
            .await
            .is_some()
    );
    assert!(claimed(&store, claim_on(&boundary, 0, "node-a", 1_003))
        .await
        .is_some());
    assert!(claimed(&store, claim_on(&parent_first, 0, "node-a", 1_004))
        .await
        .is_some());

    let nested_third = enqueue_file(&store, 4, 5, false, 1_000).await;
    let parent_third = enqueue_file(&store, 5, 6, false, 1_000).await;
    assert!(claimed(&store, claim_on(&nested_third, 0, "node-b", 1_005))
        .await
        .is_none());
    assert!(
        claimed(&store, claim_on(&parent_third, 0, "node-b", 1_006))
            .await
            .is_none(),
        "parent and nested roots each keep their own two-reader budget"
    );
}

/// A root-specific override can lower a root while existing leases remain
/// charged; it does not alter the default capacity of a sibling root.
#[tokio::test]
async fn root_override_keeps_existing_holders_and_does_not_lower_sibling_root() {
    let roots = ["/media/a", "/media/b"];
    let store = roots_store(
        &roots,
        &[
            (1, "/media/a/one.mkv"),
            (2, "/media/a/two.mkv"),
            (3, "/media/a/three.mkv"),
            (4, "/media/b/one.mkv"),
            (5, "/media/b/two.mkv"),
            (6, "/media/b/three.mkv"),
        ],
    )
    .await;
    let first_id = enqueue_file(&store, 0, 1, false, 1_001).await;
    let first = claimed(&store, claim_on(&first_id, 0, "node-a", 1_001))
        .await
        .expect("first root reader");
    let second_id = enqueue_file(&store, 1, 2, false, 1_002).await;
    assert!(claimed(&store, claim_on(&second_id, 0, "node-a", 1_002))
        .await
        .is_some());

    let override_key = format!(
        "jobs.root_source_readers.1/{}",
        super::super::source_leases::hex_upper(roots[0])
    );
    store
        .put_setting(&override_key, "1")
        .await
        .expect("root override");
    assert_eq!(
        sql(
            &store,
            "SELECT count(*) FROM source_leases WHERE owner_kind='job' AND phase!='paused'"
        )
        .await,
        vec!["2"],
        "lowering a limit does not evict already-held source leases"
    );
    assert!(super::super::background_jobs_resources::pause_background(
        &store,
        first.token.expect("first token"),
        1_003
    )
    .await
    .expect("pause one existing reader"));
    assert!(
        claim_file(&store, 2, 3, false, "node-b", 1_004)
            .await
            .is_none(),
        "with one held reader, the root-specific limit of one must refuse another"
    );

    assert!(claim_file(&store, 3, 4, false, "node-a", 1_005)
        .await
        .is_some());
    assert!(claim_file(&store, 4, 5, false, "node-a", 1_006)
        .await
        .is_some());
    assert!(
        claim_file(&store, 5, 6, false, "node-a", 1_007)
            .await
            .is_none(),
        "the sibling keeps the default two maintenance readers"
    );
}

/// A mapped domain has no aggregate cap until its explicit setting exists.
/// Once set to one, it aggregates readers across distinct root keys.
#[tokio::test]
async fn explicit_domain_limit_aggregates_distinct_roots() {
    let roots = ["/media/a", "/media/b"];
    let store = roots_store(&roots, &[(1, "/media/a/one.mkv"), (2, "/media/b/one.mkv")]).await;
    map_domain(&store, roots[0], "shared-disk").await;
    map_domain(&store, roots[1], "shared-disk").await;
    store
        .put_setting("jobs.domain_source_readers.shared-disk", "1")
        .await
        .expect("explicit domain limit");

    assert!(claim_file(&store, 0, 1, false, "node-a", 1_001)
        .await
        .is_some());
    assert!(
        claim_file(&store, 1, 2, false, "node-b", 1_002)
            .await
            .is_none(),
        "the explicit shared-domain limit joins otherwise independent roots"
    );
}

/// Resume admission keeps the maintenance limit at two even when its total
/// source allowance has a third slot; the paused job may use that spare slot
/// only after it has live playback demand.
#[tokio::test]
async fn paused_resume_uses_live_spare_but_never_a_third_maintenance_slot() {
    let store = roots_store(
        &["/media/a"],
        &[
            (1, "/media/a/paused.mkv"),
            (2, "/media/a/other-one.mkv"),
            (3, "/media/a/other-two.mkv"),
        ],
    )
    .await;

    let paused_id = enqueue_file(&store, 0, 1, false, 1_000).await;
    let paused_job = claimed(&store, claim_on(&paused_id, 0, "node-a", 1_001))
        .await
        .expect("initial maintenance claim");
    let token = paused_job.token.expect("claim token");
    assert!(super::super::background_jobs_resources::pause_background(
        &store,
        token.clone(),
        1_002
    )
    .await
    .expect("pause"));

    assert!(claim_file(&store, 1, 2, false, "node-a", 1_003)
        .await
        .is_some());
    assert!(claim_file(&store, 2, 3, false, "node-b", 1_004)
        .await
        .is_some());
    assert!(
        !super::super::background_jobs_resources::resume_background(&store, token.clone(), 1_005)
            .await
            .expect("maintenance resume"),
        "the third total slot cannot raise the two-reader maintenance cap"
    );

    sql(
        &store,
        &format!(
            "UPDATE background_job_waiters SET consumer_kind='playback_fragment',state='pending',deadline_ms=5000 WHERE job_id='{paused_id}'"
        ),
    )
    .await;
    assert!(
        super::super::background_jobs_resources::resume_background(&store, token, 1_006)
            .await
            .expect("live resume"),
        "live playback demand can use the reserved third slot"
    );
}

/// An old binary's source-attestation write is checked against the shared
/// cluster-wide root budget, then mirrored with the canonical file-root key.
#[test]
fn legacy_attestation_insert_obeys_root_cap_and_rekeys_old_generic_key() {
    let conn = rusqlite::Connection::open_in_memory().expect("v133");
    SqliteStore::apply_migrations_for_test(
        &conn,
        super::super::source_leases::PRE_LEASE_VERSION as i64,
    )
    .expect("pre-lease catalogue");
    conn.execute_batch(
        r#"INSERT INTO libraries(id,name,kind,paths)
             VALUES(1,'movies','movies','["/media"]');
           INSERT INTO items(id,library_id,kind,title,sort_title)
             VALUES(1,1,'movie','Movie','Movie');
           INSERT INTO files(id,item_id,path,size,mtime)
             VALUES(1,1,'/media/movie.mkv',100,1);
           INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms)
             VALUES('subtitle-read-attest:node-c:1','node-c',1,1,30000,1000);"#,
    )
    .expect("v1 owner state");

    conn.execute_batch(super::super::source_leases::EXPAND)
        .expect("lease expand");

    let root_key = format!(
        "source_io:root/1/{}",
        super::super::source_leases::hex_upper("/media")
    );
    conn.execute_batch(&format!(
        r#"INSERT INTO source_leases(
             lease_id,owner_kind,work_id,fence,node_id,owner_json,resource_keys_json,phase,
             projection_json,started_at_ms,renewed_at_ms,expires_at_ms)
           VALUES
             ('job:held-a:1','job','held-a',1,'node-a','{{}}','["{root_key}"]',
               'active',NULL,1000,1000,30000),
             ('job:held-b:1','job','held-b',1,'node-b','{{}}','["{root_key}"]',
               'active',NULL,1000,1000,30000);"#
    ))
    .expect("two maintenance leases on one root across nodes");

    let legacy_insert = r#"INSERT INTO background_source_custody(
        custody_id,owner_key,owner_json,resource_keys_json,owner_kind,work_id,fence,phase,
        started_at_ms,updated_at_ms,lease_expires_at_ms)
      VALUES('legacy-attestation','lease:subtitle-read-attest:node-c:1:1',
        '{"lease":{"owner_node_id":"node-c"},"file_id":1,"source_size":100,"source_mtime":1}',
        '["source_io"]','lease','subtitle-read-attest:node-c:1',1,'active',1000,1000,30000);"#;

    conn.execute(legacy_insert, [])
        .expect("legacy third-reader proposal is safely ignored");
    let rejected: i64 = conn
        .query_row(
            "SELECT count(*) FROM background_source_custody WHERE custody_id='legacy-attestation'",
            [],
            |row| row.get(0),
        )
        .expect("rejected legacy custody count");
    assert_eq!(
        rejected, 0,
        "two rooted maintenance holders across nodes leave no third maintenance slot"
    );

    conn.execute(
        "DELETE FROM source_leases WHERE lease_id='job:held-a:1'",
        [],
    )
    .expect("one root reader stopped");
    conn.execute(legacy_insert, [])
        .expect("legacy reader admitted after a root slot frees");

    let mirrored: String = conn
        .query_row(
            "SELECT resource_keys_json FROM source_leases WHERE lease_id='lease:subtitle-read-attest:node-c:1:1'",
            [],
            |row| row.get(0),
        )
        .expect("mirrored canonical root lease");
    assert_eq!(
        mirrored,
        format!("[\"{root_key}\"]"),
        "the v1 generic source_io key is replaced by the canonical file-root key"
    );
    let custody_keys: String = conn
        .query_row(
            "SELECT resource_keys_json FROM background_source_custody WHERE custody_id='legacy-attestation'",
            [],
            |row| row.get(0),
        )
        .expect("legacy custody refreshed from its canonical mirror");
    assert_eq!(custody_keys, format!("[\"{root_key}\"]"));
}

/// A root edit cannot strand already-running work on the old parent key. Its
/// current files must continue to charge the newly selected longest root.
#[tokio::test]
async fn adding_nested_root_reclassifies_existing_holders_before_admission() {
    let store = roots_store(
        &["/media"],
        &[
            (1, "/media/movies/one.mkv"),
            (2, "/media/movies/two.mkv"),
            (3, "/media/movies/three.mkv"),
        ],
    )
    .await;

    assert!(claim_file(&store, 0, 1, false, "node-a", 1_001)
        .await
        .is_some());
    assert!(claim_file(&store, 1, 2, false, "node-b", 1_002)
        .await
        .is_some());
    sql(
        &store,
        "UPDATE libraries SET paths='[\"/media\",\"/media/movies\"]' WHERE id=1",
    )
    .await;

    assert!(
        claim_file(&store, 2, 3, false, "node-c", 1_003)
            .await
            .is_none(),
        "the two active files now resolve to the nested root and still fill its budget"
    );
}

/// Playback registered against a parent root remains a blocker for the same
/// file after a nested root is added to its library.
#[tokio::test]
async fn foreground_startup_reprojects_after_nested_root_is_added() {
    use super::super::background_jobs_resources::{
        register_foreground, ForegroundSourceOwner, ForegroundSourcePolicy,
    };

    let store = roots_store(&["/media"], &[(1, "/media/movies/title.mkv")]).await;
    let owner = ForegroundSourceOwner {
        owner_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "root-edit-playback".into(),
        session_incarnation: uuid::Uuid::new_v4().to_string(),
        producer_attempt: "producer-1".into(),
        node_id: "node-a".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        file_id: 1,
        source_size: 100,
        source_mtime: 1,
        source_generation: "source:1".into(),
        policy: ForegroundSourcePolicy::Rolling,
    };
    register_foreground(&store, owner, 1_000, 2_000)
        .await
        .expect("register playback")
        .expect("foreground lease");

    sql(
        &store,
        "UPDATE libraries SET paths='[\"/media\",\"/media/movies\"]' WHERE id=1",
    )
    .await;
    assert!(
        claim_file(&store, 0, 1, false, "node-b", 1_002)
            .await
            .is_none(),
        "the active foreground lease must be reprojected to the file's current longest root"
    );
}

/// The raw v1 resume command is subject to the same root maintenance cap as
/// the new Rust API. A paused live job may use the third, live-only slot.
#[tokio::test]
async fn legacy_resume_command_enforces_root_maintenance_and_live_spare() {
    let store = roots_store(
        &["/media"],
        &[(1, "/media/paused.mkv"), (2, "/media/active.mkv")],
    )
    .await;
    store
        .put_setting("jobs.source_readers_per_root", "1")
        .await
        .expect("one maintenance reader per root");

    let paused_id = enqueue_file(&store, 0, 1, false, 1_000).await;
    let paused_job = claimed(&store, claim_on(&paused_id, 0, "node-a", 1_001))
        .await
        .expect("initial claim");
    let token = paused_job.token.expect("ownership token");
    assert!(super::super::background_jobs_resources::pause_background(
        &store,
        token.clone(),
        1_002
    )
    .await
    .expect("pause"));
    assert!(
        claim_file(&store, 1, 2, false, "node-b", 1_003)
            .await
            .is_some(),
        "the other maintenance reader fills this root"
    );

    let command_id = uuid::Uuid::new_v4().to_string();
    let request_json = serde_json::json!({
        "id": command_id,
        "token": token,
        "now_ms": 1_004
    })
    .to_string();
    let escaped_request = request_json.replace('\'', "''");
    let old_resume_proposal = format!(
        "INSERT INTO background_job_commands(id,operation,request_json,result_json) VALUES('{command_id}','source_resume','{escaped_request}','true')"
    );

    sql(&store, &old_resume_proposal).await;
    assert_eq!(
        sql(
            &store,
            &format!("SELECT phase FROM background_source_pauses WHERE job_id='{paused_id}'")
        )
        .await,
        vec!["paused".to_owned()],
        "a raw true result cannot resume a third maintenance reader"
    );
    assert_eq!(
        sql(
            &store,
            &format!("SELECT phase FROM source_leases WHERE lease_id='job:{paused_id}:1'")
        )
        .await,
        vec!["paused".to_owned()]
    );

    sql(
        &store,
        &format!(
            "UPDATE background_job_waiters SET consumer_kind='playback_fragment',state='pending',deadline_ms=600000 WHERE job_id='{paused_id}'"
        ),
    )
    .await;
    // The first proposal was ignored, so retry the identical request body and
    // id after the paused job gains live demand.
    sql(&store, &old_resume_proposal).await;
    assert_eq!(
        sql(
            &store,
            &format!("SELECT phase FROM background_source_pauses WHERE job_id='{paused_id}'")
        )
        .await,
        vec!["active".to_owned()],
        "live demand can use the reserved third slot"
    );
    assert_eq!(
        sql(
            &store,
            &format!("SELECT phase FROM source_leases WHERE lease_id='job:{paused_id}:1'")
        )
        .await,
        vec!["active".to_owned()]
    );
    assert!(
        super::super::background_jobs_resources::resume_background(&store, token, 1_005)
            .await
            .expect("acknowledge already active phase"),
        "a lost acknowledgement must not make the already-admitted live phase wait on itself"
    );
}

/// A new foreground window on the newly selected nested root blocks both
/// current typed service requests and old-writer proposals carrying the
/// admitted parent-root snapshot.
#[tokio::test]
async fn nested_root_edit_protects_old_job_service_writers() {
    use super::super::background_jobs_resources::{
        claim_background_service, register_foreground, BackgroundSourceOwner,
        ForegroundSourceOwner, ForegroundSourcePolicy,
    };

    let store = roots_store(&["/media"], &[(1, "/media/movies/title.mkv")]).await;
    let job_id = enqueue_file(&store, 0, 1, false, 1_000).await;
    let job = claimed(&store, claim_on(&job_id, 0, "node-a", 1_001))
        .await
        .expect("admitted source job");
    let token = job.token.expect("physical job token");

    sql(
        &store,
        "UPDATE libraries SET paths='[\"/media\",\"/media/movies\"]' WHERE id=1",
    )
    .await;
    let foreground = ForegroundSourceOwner {
        owner_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "nested-root-service-playback".into(),
        session_incarnation: uuid::Uuid::new_v4().to_string(),
        producer_attempt: "producer-1".into(),
        node_id: "node-b".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        file_id: 1,
        source_size: 100,
        source_mtime: 1,
        source_generation: "source:1".into(),
        policy: ForegroundSourcePolicy::Transfer,
    };
    register_foreground(&store, foreground, 1_002, 2_000)
        .await
        .expect("foreground startup")
        .expect("foreground lease");

    assert!(
        claim_background_service(
            &store,
            BackgroundSourceOwner::Job {
                token: token.clone()
            },
            1_003
        )
        .await
        .expect("typed service claim")
        .is_none(),
        "typed service must see the current nested-root foreground window"
    );

    let command_id = uuid::Uuid::new_v4().to_string();
    let request = serde_json::json!({
        "kind": "job",
        "token": token,
        "id": command_id,
        "owner_key": format!("job:{job_id}:1"),
        "node_id": "node-a",
        "now_ms": 1_003,
        "epoch_id": 0,
        "charge_bytes": 0
    })
    .to_string()
    .replace('\'', "''");
    let old_parent_key = root_key(1, "/media");
    let old_writer = format!(
        "INSERT INTO background_job_commands(id,operation,request_json,result_json) VALUES('{command_id}','source_service','{request}','{{\"receipt_id\":\"legacy-root-writer\",\"epoch_id\":0,\"expires_at_ms\":5000,\"byte_allowance\":null,\"resource_keys\":[\"{old_parent_key}\"]}}') RETURNING result_json"
    );
    assert!(
        sql(&store, &old_writer).await.is_empty(),
        "the shared writer must ignore a stale parent-root grant proposal"
    );
}

/// A receipt's finite epoch allocation remains charged to its physical lease
/// when a library edit moves that file from a parent root to a nested root.
#[tokio::test]
async fn root_edit_keeps_existing_receipt_bytes_in_the_new_scope() {
    use super::super::background_jobs_resources::{
        claim_background_service, project_foreground, register_foreground, BackgroundSourceOwner,
        ForegroundSourceOwner, ForegroundSourcePhase, ForegroundSourcePolicy,
        ForegroundSourceProjection, SourceServiceClass, SourceServiceGrant,
    };

    for release_first_owner in [false, true] {
        let store = roots_store(
            &["/media"],
            &[
                (1, "/media/movies/first.mkv"),
                (2, "/media/movies/second.mkv"),
            ],
        )
        .await;
        let first_id = enqueue_file(&store, 0, 1, false, 1_000).await;
        let first_job = claimed(&store, claim_on(&first_id, 0, "node-a", 1_001))
            .await
            .expect("first maintenance job");
        let first_token = first_job.token.expect("first job token");

        let foreground = ForegroundSourceOwner {
            owner_id: uuid::Uuid::new_v4().to_string(),
            lifetime_id: "finite-root-service-playback".into(),
            session_incarnation: uuid::Uuid::new_v4().to_string(),
            producer_attempt: "producer-1".into(),
            node_id: "node-c".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            file_id: 1,
            source_size: 100,
            source_mtime: 1,
            source_generation: "source:1".into(),
            policy: ForegroundSourcePolicy::Transfer,
        };
        let foreground = register_foreground(&store, foreground, 1_002, 2_000)
            .await
            .expect("register foreground")
            .expect("foreground lease");
        let mut projection = ForegroundSourceProjection::startup(1_003);
        projection.phase = ForegroundSourcePhase::TransferService;
        projection.observation_version = 1;
        projection.observation_expires_at_ms = 5_000;
        projection.transfer_grant = Some(SourceServiceGrant {
            epoch_id: 0,
            starts_at_ms: 0,
            expires_at_ms: 5_000,
            foreground_bytes: 16 * 1_048_576,
            background_bytes: 8 * 1_048_576,
            foreground_floor_bps: None,
            supply_floor_bps: None,
            service_class: SourceServiceClass::WeightedUnqualified,
        });
        let _foreground = project_foreground(&store, foreground, projection, 1_003, 2_000)
            .await
            .expect("grant finite service")
            .expect("foreground service lease");
        let first_grant = claim_background_service(
            &store,
            BackgroundSourceOwner::Job {
                token: first_token.clone(),
            },
            1_004,
        )
        .await
        .expect("first service claim")
        .expect("first finite grant");
        assert_eq!(first_grant.byte_allowance, Some(8 * 1_048_576));

        sql(
            &store,
            "UPDATE libraries SET paths='[\"/media\",\"/media/movies\"]' WHERE id=1",
        )
        .await;
        if release_first_owner {
            sql(
                &store,
                &format!("DELETE FROM source_leases WHERE lease_id='job:{first_id}:1'"),
            )
            .await;
        }

        let second_id = enqueue_file(&store, 1, 2, false, 1_005).await;
        let second_job = claimed(&store, claim_on(&second_id, 0, "node-b", 1_006))
            .await
            .expect("second maintenance reader fits the root budget");
        let second_token = second_job.token.expect("second job token");
        assert_eq!(
            resource_keys(&store, &second_id).await,
            vec![root_key(1, "/media/movies")]
        );

        assert!(
            claim_background_service(
                &store,
                BackgroundSourceOwner::Job {
                    token: second_token,
                },
                1_007,
            )
            .await
            .expect("second service claim")
            .is_none(),
            "the first receipt's allocation remains charged whether its physical owner is open or released"
        );
        assert_eq!(
            sql(
                &store,
                &format!(
                    "SELECT allocated_bytes FROM background_source_service_grants WHERE receipt_id='{}'",
                    first_grant.receipt_id
                )
            )
            .await,
            vec![(8 * 1_048_576).to_string()]
        );
    }
}

/// Measured foreground supply feasibility follows a playback lease's current
/// root after a nested library root is introduced.
#[tokio::test]
async fn measured_transfer_supply_stays_shared_after_nested_root_edit() {
    use super::super::background_jobs_resources::{
        project_foreground, register_foreground, ForegroundSourceOwner, ForegroundSourcePhase,
        ForegroundSourcePolicy, ForegroundSourceProjection, SourceServiceClass, SourceServiceGrant,
    };

    let store = roots_store(&["/media"], &[(1, "/media/movies/title.mkv")]).await;
    let first_owner = ForegroundSourceOwner {
        owner_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "measured-transfer-first".into(),
        session_incarnation: uuid::Uuid::new_v4().to_string(),
        producer_attempt: "producer-1".into(),
        node_id: "node-a".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        file_id: 1,
        source_size: 100,
        source_mtime: 1,
        source_generation: "source:1".into(),
        policy: ForegroundSourcePolicy::Transfer,
    };
    let first = register_foreground(&store, first_owner, 1_000, 2_000)
        .await
        .expect("first foreground startup")
        .expect("first foreground lease");
    let mut first_projection = ForegroundSourceProjection::startup(1_001);
    first_projection.phase = ForegroundSourcePhase::TransferService;
    first_projection.observation_version = 1;
    first_projection.observation_expires_at_ms = 5_000;
    first_projection.transfer_grant = Some(SourceServiceGrant {
        epoch_id: 0,
        starts_at_ms: 0,
        expires_at_ms: 5_000,
        foreground_bytes: 6 * 1_048_576,
        background_bytes: 2 * 1_048_576,
        foreground_floor_bps: Some(8 * 1_048_576),
        supply_floor_bps: Some(16 * 1_048_576),
        service_class: SourceServiceClass::Measured,
    });
    let first = project_foreground(&store, first, first_projection, 1_001, 2_000)
        .await
        .expect("first measured grant")
        .expect("first grant fits 10 MiB supply");

    sql(
        &store,
        "UPDATE libraries SET paths='[\"/media\",\"/media/movies\"]' WHERE id=1",
    )
    .await;
    let second_owner = ForegroundSourceOwner {
        owner_id: uuid::Uuid::new_v4().to_string(),
        lifetime_id: "measured-transfer-second".into(),
        session_incarnation: uuid::Uuid::new_v4().to_string(),
        producer_attempt: "producer-2".into(),
        node_id: "node-b".into(),
        boot_id: uuid::Uuid::new_v4().to_string(),
        file_id: 1,
        source_size: 100,
        source_mtime: 1,
        source_generation: "source:1".into(),
        policy: ForegroundSourcePolicy::Transfer,
    };
    let second = register_foreground(&store, second_owner, 1_002, 2_000)
        .await
        .expect("second foreground startup")
        .expect("second foreground lease");
    let mut second_projection = ForegroundSourceProjection::startup(1_003);
    second_projection.phase = ForegroundSourcePhase::TransferService;
    second_projection.observation_version = 1;
    second_projection.observation_expires_at_ms = 5_000;
    second_projection.transfer_grant = Some(SourceServiceGrant {
        epoch_id: 0,
        starts_at_ms: 0,
        expires_at_ms: 5_000,
        foreground_bytes: 6 * 1_048_576,
        background_bytes: 2 * 1_048_576,
        foreground_floor_bps: Some(8 * 1_048_576),
        supply_floor_bps: Some(16 * 1_048_576),
        service_class: SourceServiceClass::Measured,
    });

    assert!(
        project_foreground(&store, second, second_projection, 1_003, 2_000)
            .await
            .expect("second measured grant")
            .is_none(),
        "the first lease's 6 MiB foreground share remains in the same 10 MiB scope"
    );
    assert_eq!(
        sql(
            &store,
            &format!(
                "SELECT phase FROM source_leases WHERE lease_id='{}'",
                first.owner.owner_id
            )
        )
        .await,
        vec!["transfer_service".to_owned()]
    );
}
