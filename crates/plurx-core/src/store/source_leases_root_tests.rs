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
