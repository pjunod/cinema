//! NEW synthetic source-bound lineage qualification, not capture replay.
#![cfg(all(feature = "hiqlite-store", feature = "hiqlite-contract-tests"))]

use futures_util::FutureExt;
use hiqlite::{params, Client, Node, NodeConfig, Row};
use plurx_core::store::{HiqliteAuthStore, SqliteStore};
use rusqlite::Connection;
use std::{
    borrow::Cow,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const SCHEMA: &str = "SELECT json_group_array(json_array(type,name,sql)) AS value FROM (SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name)";
const SENTINEL: &str = "INSERT INTO settings(key,value,updated_at) VALUES('lineage.sentinel','synthetic opaque payload',1)";
const PAYLOAD: &str = "SELECT value FROM settings WHERE key='lineage.sentinel'";
const HISTORY: &str = "INSERT INTO candidate_recovery(scope,recipe,cause,event_id,user_id,playback_id,recovery_epoch,created_ms,quality_step) VALUES('synthetic-scope','synthetic-recipe','decode','synthetic-event',7,'synthetic-playback','synthetic-epoch',123,1)";
const HISTORY_VALUE: &str = "SELECT json_array(scope,recipe,cause,event_id,user_id,playback_id,recovery_epoch,created_ms,quality_step) AS value FROM candidate_recovery";
const USER: &str = "INSERT INTO users(id,username,password_hash,is_admin,created_at) VALUES(7,'synthetic-lineage-user','not-a-real-auth-secret',0,1)";
const PACKAGE: &str = "INSERT INTO offline_packages(id,request_id,user_id,file_id,node_id,source_path,source_size,source_mtime,target_height,audio_offset_ms,subtitle_mode,state,phase,progress_millis,estimated_bytes,reserved_bytes,created_at,updated_at,last_access_at,expires_at) VALUES('synthetic-package','synthetic-request',7,1,'synthetic-node','/synthetic/source.mkv',4096,123,720,0,'none','queued','queued',0,4096,4096,1,1,1,10000)";
const AUDIO: &str = "UPDATE offline_packages SET audio_recipe='synthetic-resolved-audio' WHERE id='synthetic-package'";
const PACKAGE_VALUE: &str = "SELECT json_array(id,request_id,user_id,file_id,node_id,source_path,source_size,source_mtime,target_height,audio_offset_ms,subtitle_mode,state,phase,progress_millis,estimated_bytes,reserved_bytes,created_at,updated_at,last_access_at,expires_at,audio_recipe) AS value FROM offline_packages WHERE id='synthetic-package'";
const JOB: &str = "INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms) VALUES('synthetic-job','copy_output_prepare',1,'{\"file_id\":1,\"source_size\":4096,\"source_mtime\":123,\"intent\":{\"target_node_id\":\"synthetic-node\"}}','synthetic-dedupe',1,'queued',1,1,1)";
const JOB_VALUE: &str = "SELECT json_array(id,kind,payload_version,payload_json,dedupe_key,priority,state,target_node_id,revision,created_at_ms,updated_at_ms) AS value FROM background_jobs WHERE id='synthetic-job'";

struct Value(String);
impl From<&mut Row<'_>> for Value {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("value"))
    }
}
async fn value(client: &Client, sql: &'static str) -> String {
    let rows = client
        .query_consistent_map::<Value, _>(sql, params!())
        .await
        .expect("consistent qualification read");
    assert_eq!(rows.len(), 1);
    rows[0].0.clone()
}
async fn write(client: &Client, sql: &'static str) {
    client
        .execute(sql, params!())
        .await
        .expect("synthetic fixture write");
}

async fn coherent_advance(
    store: &HiqliteAuthStore,
    client: &Client,
    telemetry: &Path,
    private: bool,
) {
    let predecessor = store
        .validation_coherent_lineage_snapshot()
        .await
        .expect("recognized coherent predecessor");
    assert_eq!(predecessor.0, 69);
    let predecessor_complete: Vec<[String; 3]> = serde_json::from_str(&value(client, SCHEMA).await)
        .expect("complete predecessor raw schema");
    let predecessor_objects: Vec<[String; 3]> =
        serde_json::from_str(&predecessor.1).expect("raw predecessor fingerprint");
    for object in predecessor_objects {
        assert!(
            predecessor_complete.contains(&object),
            "predecessor raw object bytes are exact"
        );
    }
    assert_eq!(
        store
            .validation_coherent_lineage_snapshot()
            .await
            .expect("stable predecessor raw bytes"),
        predecessor
    );
    // Capture BEFORE releasing the actual writer(s). This is deterministic
    // snapshot-boundary coverage, not a probabilistic old-race reproduction.
    if private {
        let barrier = tokio::sync::Barrier::new(3);
        let migrate = || async {
            barrier.wait().await;
            let migrated = Box::pin(HiqliteAuthStore::open_or_migrate(client.clone(), telemetry))
                .await
                .expect("concurrent private bridge caller");
            migrated
                .validation_lineage_union_fingerprint()
                .await
                .expect("concurrent exact final fingerprint")
        };
        let (left, right, _) =
            tokio::join!(Box::pin(migrate()), Box::pin(migrate()), barrier.wait());
        assert_eq!(
            left, right,
            "both real migration callers settle identically"
        );
    } else {
        let barrier = tokio::sync::Barrier::new(2);
        let writer = async {
            barrier.wait().await;
            // Exact existing published 69->70 migration statements, atomic.
            let results = client.txn(vec![
                ("ALTER TABLE offline_packages ADD COLUMN audio_recipe TEXT".to_owned(), params!()),
                ("UPDATE cluster_meta SET schema_version=$1,migrated_at=$2 WHERE singleton=1 AND schema_version=$3".to_owned(), params!(70_i64, 123_i64, 69_i64)),
            ]).await.expect("published audio migration transaction");
            results
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .expect("all published migration statements");
        };
        tokio::join!(writer, barrier.wait());
    }
    let successor = store
        .validation_coherent_lineage_snapshot()
        .await
        .expect("recognized coherent successor");
    // A private caller bridges to the canonical end (73) and then runs the
    // ordinary continuous-quality steps (74, 75) in the same open.
    assert_eq!(successor.0, if private { 75 } else { 70 });
    assert_ne!(
        successor.1, predecessor.1,
        "committed schema actually advanced"
    );
    assert_eq!(
        store
            .validation_coherent_lineage_snapshot()
            .await
            .expect("stable successor raw bytes"),
        successor
    );
    // Raw object bytes must match an independent complete sqlite_schema read.
    let complete: Vec<[String; 3]> =
        serde_json::from_str(&value(client, SCHEMA).await).expect("complete raw schema");
    let observed: Vec<[String; 3]> =
        serde_json::from_str(&successor.1).expect("raw coherent fingerprint");
    for object in observed {
        assert!(
            complete.contains(&object),
            "snapshot raw object bytes are exact"
        );
    }
    assert_eq!(
        predecessor.0, 69,
        "captured predecessor was not rewritten by the writer"
    );
}

fn sqlite_cases(root: &Path) {
    for (private, markers) in [(false, 88..=91), (true, 88..=93)] {
        for marker in markers {
            let path = root.join(format!("sqlite-{private}-{marker}.db"));
            SqliteStore::validation_seed_schema_lineage(&path, marker, private)
                .expect("bound SQLite source fixture");
            let conn = Connection::open(&path).expect("fixture connection");
            conn.pragma_update(None, "foreign_keys", "ON")
                .expect("FK admission");
            conn.execute_batch(SENTINEL).expect("opaque payload");
            conn.execute_batch(USER).expect("synthetic user");
            conn.execute_batch(PACKAGE).expect("durable package");
            if private {
                conn.execute_batch(AUDIO).expect("resolved audio snapshot");
            }
            let expected_package: Option<String> = if private {
                Some(
                    conn.query_row(PACKAGE_VALUE, [], |row| row.get(0))
                        .expect("complete package oracle"),
                )
            } else {
                None
            };
            conn.execute_batch(JOB).expect("opaque copy job");
            let expected_job: String = conn
                .query_row(JOB_VALUE, [], |row| row.get(0))
                .expect("job oracle");
            let history = private && marker >= 92;
            let expected_history: Option<String> = if history {
                conn.execute_batch(HISTORY).expect("candidate history");
                Some(
                    conn.query_row(HISTORY_VALUE, [], |row| row.get(0))
                        .expect("history oracle"),
                )
            } else {
                None
            };
            if private && marker == 93 {
                let before: String = conn
                    .query_row(SCHEMA, [], |row| row.get(0))
                    .expect("pre-error schema");
                assert!(plurx_core::store::validation_sqlite_bridge_rollback(&conn).is_err());
                assert_eq!(
                    conn.query_row(SCHEMA, [], |row| row.get::<_, String>(0))
                        .expect("rollback schema"),
                    before
                );
                assert_eq!(
                    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                        .expect("rollback marker"),
                    marker
                );
                assert_eq!(
                    conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                        .expect("unchanged FK mode"),
                    1
                );
                // A known effort prefix plus only one published addition is
                // not an approved intermediate. Refuse without further DDL.
                conn.execute_batch("CREATE INDEX analysis_requests_result_target_force ON analysis_requests(result_cache_key,target_node_id,force_rebuild)").expect("mixed source fixture");
                let mixed: String = conn
                    .query_row(SCHEMA, [], |row| row.get(0))
                    .expect("mixed snapshot");
                assert!(SqliteStore::open(&path).is_err());
                assert_eq!(
                    conn.query_row(SCHEMA, [], |row| row.get::<_, String>(0))
                        .expect("refused snapshot"),
                    mixed
                );
                assert_eq!(
                    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                        .expect("refused marker"),
                    marker
                );
                conn.execute_batch("DROP INDEX analysis_requests_result_target_force")
                    .expect("remove only injected fixture object");
            }
            drop(conn);
            let store = SqliteStore::open(&path).expect("actual SQLite migration/bridge");
            drop(store);
            let conn = Connection::open(&path).expect("persistent qualified database");
            assert_eq!(
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .expect("canonical marker"),
                // The bridge stamps the canonical end (v97); the ordinary
                // continuous-quality steps then reach v99.
                99
            );
            assert_eq!(
                conn.query_row(PAYLOAD, [], |row| row.get::<_, String>(0))
                    .expect("payload retained"),
                "synthetic opaque payload"
            );
            assert_eq!(
                conn.query_row(JOB_VALUE, [], |row| row.get::<_, String>(0))
                    .expect("job retained"),
                expected_job
            );
            if let Some(expected) = expected_package {
                assert_eq!(
                    conn.query_row(PACKAGE_VALUE, [], |row| row.get::<_, String>(0))
                        .expect("package/audio retained"),
                    expected
                );
            } else {
                assert_eq!(
                    conn.query_row(
                        "SELECT audio_recipe FROM offline_packages WHERE id='synthetic-package'",
                        [],
                        |row| row.get::<_, Option<String>>(0)
                    )
                    .expect("legacy audio default"),
                    None
                );
            }
            if let Some(expected) = expected_history {
                assert_eq!(
                    conn.query_row(HISTORY_VALUE, [], |row| row.get::<_, String>(0))
                        .expect("all history columns retained"),
                    expected
                );
            }
            let fingerprint = plurx_core::store::validation_sqlite_union_fingerprint(&conn)
                .expect("exact canonical objects/columns");
            drop(conn);
            drop(SqliteStore::open(&path).expect("qualified reopen"));
            let conn = Connection::open(&path).expect("reopen oracle");
            assert_eq!(
                plurx_core::store::validation_sqlite_union_fingerprint(&conn)
                    .expect("reopen exact schema"),
                fingerprint
            );
        }
    }
}

async fn hiqlite_case(
    root: &Path,
    marker: i64,
    private: bool,
    omission: bool,
    active: &Arc<Mutex<Option<Client>>>,
) {
    let listeners: Vec<_> = (0..2)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned ephemeral port"))
        .collect();
    let addresses: Vec<_> = listeners
        .iter()
        .map(|listener| listener.local_addr().expect("address"))
        .collect();
    drop(listeners);
    let client = Box::pin(hiqlite::start_node(NodeConfig {
        node_id: 1,
        nodes: vec![Node {
            id: 1,
            addr_raft: addresses[0].to_string(),
            addr_api: addresses[1].to_string(),
        }],
        listen_addr_api: Cow::Borrowed("127.0.0.1"),
        listen_addr_raft: Cow::Borrowed("127.0.0.1"),
        data_dir: Cow::Owned(root.to_string_lossy().into_owned()),
        filename_db: Cow::Borrowed("lineage.db"),
        secret_api: "synthetic-lineage-api".into(),
        secret_raft: "synthetic-lineage-raft".into(),
        tls_api: None,
        tls_raft: None,
        ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
    }))
    .await
    .expect("owned singleton");
    *active.lock().expect("owned node census") = Some(client.clone());
    let scenario = std::panic::AssertUnwindSafe(async {
        tokio::time::timeout(Duration::from_secs(60), Box::pin(async {
        let telemetry = root.join("telemetry.db");
        let store = Box::pin(HiqliteAuthStore::bootstrap(client.clone(), "11111111-1111-4111-8111-111111111073", &telemetry)).await.expect("new synthetic source bootstrap");
        store.validation_set_schema_lineage(marker, private, omission).await.expect("explicit old source shape");
        write(&client, SENTINEL).await;
        write(&client, USER).await;
        write(&client, PACKAGE).await;
        if private { write(&client, AUDIO).await; }
        let expected_package = if private { Some(value(&client, PACKAGE_VALUE).await) } else { None };
        write(&client, JOB).await;
        let expected_job = value(&client, JOB_VALUE).await;
        let expected_history = if private && marker >= 68 { write(&client, HISTORY).await; Some(value(&client, HISTORY_VALUE).await) } else { None };
        if private && marker == 69 && !omission {
            let before = value(&client, SCHEMA).await;
            assert!(store.validation_lineage_bridge_rollback().await.is_err());
            assert_eq!(value(&client, SCHEMA).await, before, "all transactional DDL rolled back");
            assert_eq!(value(&client, "SELECT CAST(schema_version AS TEXT) AS value FROM cluster_meta").await, "69");
            write(&client, "CREATE INDEX analysis_requests_result_target_force ON analysis_requests(result_cache_key,target_node_id,force_rebuild)").await;
            let mixed = value(&client, SCHEMA).await;
            assert!(Box::pin(HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)).await.is_err());
            assert_eq!(value(&client, SCHEMA).await, mixed, "unknown mixed lineage is nonmutating");
            assert_eq!(value(&client, "SELECT CAST(schema_version AS TEXT) AS value FROM cluster_meta").await, "69");
            write(&client, "DROP INDEX analysis_requests_result_target_force").await;
        }
        if marker == 69 && !omission {
            Box::pin(coherent_advance(&store, &client, &telemetry, private)).await;
        }
        drop(store);
        let store = Box::pin(HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)).await.expect("actual replicated lineage upgrade");
        // Bridged to the canonical end (73), then the ordinary steps to 75.
        assert_eq!(value(&client, "SELECT CAST(schema_version AS TEXT) AS value FROM cluster_meta").await, "75");
        assert_eq!(value(&client, PAYLOAD).await, "synthetic opaque payload");
        assert_eq!(value(&client, JOB_VALUE).await, expected_job);
        if let Some(expected) = expected_package { assert_eq!(value(&client, PACKAGE_VALUE).await, expected); }
        else { assert_eq!(value(&client, "SELECT CASE WHEN audio_recipe IS NULL THEN 'null' ELSE audio_recipe END AS value FROM offline_packages WHERE id='synthetic-package'").await, "null"); }
        if let Some(expected) = expected_history { assert_eq!(value(&client, HISTORY_VALUE).await, expected); }
        let fingerprint = store.validation_lineage_union_fingerprint().await.expect("canonical named objects/columns");
        drop(store);
        let reopened = Box::pin(HiqliteAuthStore::open_or_migrate(client.clone(), &telemetry)).await.expect("reopen persisted schema");
        assert_eq!(reopened.validation_lineage_union_fingerprint().await.expect("reopened schema"), fingerprint);
        })).await.expect("bounded synthetic lineage scenario");
    }).catch_unwind().await;
    tokio::time::timeout(Duration::from_secs(10), client.shutdown())
        .await
        .expect("bounded singleton drain")
        .expect("singleton shutdown");
    drop(client);
    for address in addresses {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            match std::net::TcpListener::bind(address) {
                Ok(listener) => {
                    drop(listener);
                    break;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::AddrInUse
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("owned listener not reusable after drain: {error}"),
            }
        }
    }
    active.lock().expect("owned node census").take();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn narrow_schema_lineage_bridge_preserves_and_refuses() {
    // Match the existing full-Hiqlite debug fixture convention, locally to
    // this one test. The migration poll alone reserves about 1.78 MiB;
    // heap-pinning its callers does not shrink that polling stack frame.
    let worker = std::thread::Builder::new()
        .name("schema-lineage-qualification".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("owned lineage runtime")
                .block_on(Box::pin(qualify_schema_lineage()));
        })
        .expect("owned lineage thread");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn qualify_schema_lineage() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = tempfile::TempDir::new().expect("owned synthetic fixture root");
    let path = root.path().to_path_buf();
    let active = Arc::new(Mutex::new(None));
    // Nineteen distinct source fixtures share one global 480s qualification
    // bound. It leaves the external 600s command guard time for owned drain
    // and retained failure evidence; no independent child or suite is run.
    let result = std::panic::AssertUnwindSafe(tokio::time::timeout(
        Duration::from_secs(480),
        Box::pin(async {
            sqlite_cases(root.path());
            for private in [false, true] {
                for marker in 66..=69 {
                    let path = root.path().join(format!("hiqlite-{private}-{marker}"));
                    Box::pin(hiqlite_case(&path, marker, private, false, &active)).await;
                }
            }
            Box::pin(hiqlite_case(
                &root.path().join("hiqlite-private-fresh-omission"),
                69,
                true,
                true,
                &active,
            ))
            .await;
        }),
    ))
    .catch_unwind()
    .await;
    let remaining = active.lock().expect("owned node census").take();
    let drain_error = if let Some(client) = remaining {
        match tokio::time::timeout(Duration::from_secs(10), client.shutdown()).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("owned terminal shutdown failed: {error}")),
            Err(error) => Some(format!("owned terminal shutdown timed out: {error}")),
        }
    } else {
        None
    };
    if let Some(error) = drain_error {
        let retained = root.keep();
        panic!("{error}; retained fixture evidence {}", retained.display());
    }
    match result {
        Err(panic) => {
            let _retained = root.keep();
            std::panic::resume_unwind(panic);
        }
        Ok(Err(error)) => {
            let retained = root.keep();
            panic!(
                "global lineage qualification timeout: {error}; retained {}",
                retained.display()
            );
        }
        Ok(Ok(())) => {}
    }
    drop(root);
    assert!(
        !path.exists(),
        "only remove fixtures after proved shutdown/listener drain"
    );
}
