//! Exercise encoded preparation's fresh Store routing and source guards.
#![cfg(feature = "hiqlite-store")]

use futures_util::FutureExt;
use hiqlite::{params, Client, Node, NodeConfig, Row};
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
use plurx_core::store::{HiqliteAuthStore, LibraryStore, MediaStore};
use std::{borrow::Cow, path::Path, time::Duration};

struct Count(i64);
impl From<&mut Row<'_>> for Count {
    fn from(row: &mut Row<'_>) -> Self {
        Self(row.get("value"))
    }
}
async fn count(client: &Client, sql: &'static str) -> i64 {
    let rows = client
        .query_consistent_map::<Count, _>(sql, params!())
        .await
        .expect("committed bootstrap oracle");
    assert_eq!(rows.len(), 1);
    rows[0].0
}
async fn scenario(root: &Path) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listeners: Vec<_> = (0..2)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned bootstrap port"))
        .collect();
    let addresses: Vec<_> = listeners
        .iter()
        .map(|listener| listener.local_addr().expect("owned listener address"))
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
        data_dir: Cow::Owned(root.join("node").to_string_lossy().into_owned()),
        filename_db: Cow::Borrowed("encoded-bootstrap.db"),
        secret_api: "owned-encoded-bootstrap-api".into(),
        secret_raft: "owned-encoded-bootstrap-raft".into(),
        tls_api: None,
        tls_raft: None,
        ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
    }))
    .await
    .expect("start actual owned singleton");
    let telemetry = root.join("telemetry.db");
    let store = Box::pin(HiqliteAuthStore::bootstrap(
        client.clone(),
        "11111111-1111-4111-8111-111111111169",
        &telemetry,
    ))
    .await
    .expect("actual fresh Store bootstrap");
    let library = store
        .create_library(&NewLibrary {
            name: "Owned encoded bootstrap".into(),
            kind: LibraryKind::Movies,
            paths: vec![root.join("media")],
            anime: false,
        })
        .await
        .expect("create actual library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Owned encoded bootstrap".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("create actual item");
    let file = store
        .upsert_file(
            item,
            "/owned-encoded-bootstrap/movie.mkv",
            4096,
            7,
            &ProbeResult::default(),
        )
        .await
        .expect("create actual source identity");
    for (id, size) in [("encoded-change", 4096), ("encoded-delete", 4097)] {
        let payload = serde_json::json!({"file_id": file, "source_size": size,
            "source_mtime": 7, "intent": {"target_node_id": "owned-encoded-target"}})
        .to_string();
        client.execute(
            "INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,not_before_ms,created_at_ms,updated_at_ms) VALUES($1,'encoded_output_prepare',1,$2,$1,0,'queued',10,10,10)",
            params!(id, payload),
        ).await.expect("seed one encoded preparation row");
        assert_eq!(count(&client, "SELECT count(*) AS value FROM background_jobs WHERE kind='encoded_output_prepare' AND target_node_id='owned-encoded-target'").await,
            if id == "encoded-change" { 1 } else { 2 }, "fresh bootstrap routes the exact encoded target");
        if id == "encoded-change" {
            client
                .execute("UPDATE files SET size=4097 WHERE id=$1", params!(file))
                .await
                .expect("change committed source revision");
            assert_eq!(count(&client, "SELECT count(*) AS value FROM background_jobs WHERE id='encoded-change' AND state='cancelled' AND last_error_code='source_changed' AND revision=1").await, 1,
                "source mutation cancels queued encoded preparation exactly once");
        } else {
            client
                .execute("DELETE FROM files WHERE id=$1", params!(file))
                .await
                .expect("delete committed source");
            assert_eq!(count(&client, "SELECT count(*) AS value FROM background_jobs WHERE kind='encoded_output_prepare' AND state='cancelled' AND last_error_code='source_changed' AND revision=1").await, 2,
                "source deletion cancels the new row without revising the earlier terminal row");
        }
    }
    drop(store);
    client
        .shutdown()
        .await
        .expect("actual singleton terminal drain");
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
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) => panic!("owned listener not reusable after drain: {error}"),
            }
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_bootstrap_routes_and_cancels_encoded_preparation() {
    let root = tempfile::tempdir().expect("owned bootstrap fixture directory");
    let path = root.path().to_path_buf();
    let result = std::panic::AssertUnwindSafe(async {
        tokio::time::timeout(Duration::from_secs(45), Box::pin(scenario(root.path())))
            .await
            .expect("bounded actual bootstrap and terminal drain");
    })
    .catch_unwind()
    .await;
    if let Err(panic) = result {
        let _retained = root.keep();
        std::panic::resume_unwind(panic);
    }
    drop(root);
    assert!(
        !path.exists(),
        "fixture removal follows proved singleton/listener drain"
    );
}
