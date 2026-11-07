//! File-backed startup acceptance. These exercise the daemon composition seam,
//! rather than constructing a native Store in isolation.
use super::*;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
#[cfg(feature = "cluster")]
use plurx_core::storage::mode::publish_record;
use plurx_core::storage::mode::MANIFEST_FILENAME;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

struct OccupiedClusterPorts {
    _raft: TcpListener,
    _api: TcpListener,
}

fn configuration(root: &Path, preference: Option<StorageMode>) -> (Config, OccupiedClusterPorts) {
    let raft = TcpListener::bind("127.0.0.1:0").expect("reserve Raft port");
    let api = TcpListener::bind("127.0.0.1:0").expect("reserve private API port");
    let mut config = Config::default();
    config.storage.data_dir = root.to_path_buf();
    config.storage.mode = preference;
    config.server.bind = "127.0.0.1:0".parse().expect("public test address");
    config.cluster.raft_bind = raft.local_addr().expect("reserved Raft address");
    config.cluster.api_bind = api.local_addr().expect("reserved API address");
    (
        config,
        OccupiedClusterPorts {
            _raft: raft,
            _api: api,
        },
    )
}

async fn select_for_test(config: &Config) -> Result<RuntimeStore, StoreError> {
    #[cfg(feature = "cluster")]
    {
        select(config, None).await
    }
    #[cfg(not(feature = "cluster"))]
    {
        select(config).await
    }
}

async fn open_native(config: &Config) -> RuntimeStore {
    tokio::time::timeout(std::time::Duration::from_secs(30), select_for_test(config))
        .await
        .expect("native startup does not wait for cluster admission")
        .expect("native daemon storage")
}

fn assert_no_cluster_artifacts(root: &Path) {
    for name in [
        "hiqlite",
        "hiqlite.incoming",
        "hiqlite-activated.json",
        "membership.json",
        "secret_raft",
        "secret_api",
        "activity_http_signing_key",
    ] {
        assert!(
            !root.join(name).exists(),
            "standalone startup must not create {name}"
        );
    }
}

#[cfg(feature = "cluster")]
#[test]
fn cluster_restart_preflight_uses_committed_schema_before_source_preparation() {
    use plurx_core::cluster::migration::ActivationMarker;
    use plurx_core::store::{
        SqliteImportTableDigest, AUTH_SCHEMA_BASELINE_VERSION, AUTH_SCHEMA_VERSION,
    };
    let directory = tempfile::tempdir().expect("authority metadata fixture");
    let root = directory.path();
    std::fs::create_dir(root.join("hiqlite")).expect("cluster metadata directory");
    let instance = uuid::Uuid::new_v4().to_string();
    let node = uuid::Uuid::new_v4().to_string();
    std::fs::write(root.join("node.id"), format!("{node}\n")).expect("committed node identity");
    let mut config = Config::default();
    config.storage.data_dir = root.to_path_buf();
    config.storage.mode = Some(StorageMode::Cluster);
    // Metadata-only crash-boundary fixture: no voter or serving proof is
    // constructed. The ordinary restart preflight was the failing boundary.
    let mut marker = ActivationMarker {
        marker_version: 1,
        cluster_id: instance.clone(),
        source_backup_sha256: "a".repeat(64),
        source_schema_version: 123,
        replicated_schema_version: AUTH_SCHEMA_BASELINE_VERSION,
        imported_rows: 1,
        table_hashes: vec![SqliteImportTableDigest {
            table: "settings".to_owned(),
            row_count: 1,
            sha256: "b".repeat(64),
        }],
        admitted_role: None,
    };
    let marker_path = root.join("hiqlite/activation.json");
    publish_record(&marker_path, &marker).expect("committed predecessor marker");
    let manifest =
        committed_cluster_manifest(&config, &instance, &node).expect("bind committed schema");
    assert_eq!(manifest.schema_version, AUTH_SCHEMA_BASELINE_VERSION);
    assert!(
        manifest.schema_version < AUTH_SCHEMA_VERSION,
        "fixture exercises pending Source preparation"
    );
    manifest
        .publish(root)
        .expect("selection manifest before Source phase");
    let recovered = StorageManifest::read(root)
        .expect("restart manifest")
        .expect("committed authority");
    crate::cluster_runtime::validate_manifest(&config, &recovered)
        .expect("restart can reach actual cluster opening and Source recovery");
    marker.replicated_schema_version = AUTH_SCHEMA_VERSION;
    publish_record(&marker_path, &marker).expect("later committed Source phase");
    crate::cluster_runtime::validate_manifest(&config, &recovered)
        .expect("later schema advance retains valid authority binding");
}

#[tokio::test]
async fn standalone_default_and_explicit_sqlite_start_with_cluster_ports_unavailable() {
    for preference in [None, Some(StorageMode::Sqlite)] {
        let directory = tempfile::tempdir().expect("standalone root");
        let (mut config, _occupied) = configuration(directory.path(), preference);
        config.cluster.read_pool_size = 0;
        config.cluster.logs_until_snapshot = 0;
        config.cluster.snapshot_chunk_timeout_secs = 0;
        let mut runtime = open_native(&config).await;
        assert_eq!(runtime.mode(), StorageMode::Sqlite);
        let manifest = StorageManifest::read(directory.path())
            .expect("manifest read")
            .expect("durable native selection");
        assert_eq!(manifest.mode, StorageMode::Sqlite);
        assert_eq!(manifest.database, Path::new("plurx.db"));
        assert!(directory.path().join("plurx.db").is_file());
        let services = runtime.services(&config).expect("native service adapters");
        #[cfg(feature = "cluster")]
        {
            assert!(services.replication.is_none());
            assert!(services.membership.is_none());
        }
        let serving = crate::serving_fence::ServingFence::local();
        assert!(serving.is_ready());
        assert!(!serving.is_quorum_managed());
        assert!(matches!(
            services.settings_changes,
            crate::settings_changes::SettingsChangeSource::LocalOnly
        ));
        assert_eq!(services.backup.lease_resource(), "backup:native");
        assert!(runtime
            .prepare_source_before_serving()
            .await
            .expect("real native Source preparation"));
        assert!(
            runtime.prepare_source_before_serving().await.is_err(),
            "startup authority is consumed once"
        );
        let source = runtime.local_source().expect("local Source capability");
        let registry = uuid::Uuid::new_v4();
        let admitted = source
            .admission(registry)
            .expect("local registry-bound admission");
        admitted
            .write_guard(1000, 1, 2, 3)
            .expect("current local admission");
        let destination = tempfile::tempdir().expect("native backup output");
        let artifact = services
            .backup
            .build(destination.path())
            .await
            .expect("native backup from actual selected provider");
        assert!(artifact.path.join("manifest.json").is_file());
        assert_eq!(
            artifact
                .manifest
                .get("storage_backend")
                .and_then(serde_json::Value::as_str),
            Some("sqlite")
        );
        assert!(artifact.manifest.get("raft_cut").is_none());
        assert_no_cluster_artifacts(directory.path());
        let retained_store = runtime.store();
        runtime.shutdown().await.expect("native shutdown");
        assert!(
            retained_store.get_setting("shutdown.closed").await.is_err(),
            "runtime shutdown closes actual SQL access through retained handles"
        );
        assert!(retained_store
            .put_setting("shutdown.closed", "must refuse")
            .await
            .is_err());
        runtime
            .shutdown()
            .await
            .expect("repeated native close is idempotent");
        assert!(
            admitted.write_guard(1001, 1, 2, 3).is_err(),
            "shutdown revokes existing local admissions"
        );
    }
}

#[tokio::test]
async fn standalone_restart_preserves_identity_users_library_and_resume_data() {
    let directory = tempfile::tempdir().expect("standalone root");
    let (config, _occupied) = configuration(directory.path(), None);
    let mut first = open_native(&config).await;
    first
        .prepare_source_before_serving()
        .await
        .expect("native Source startup");
    let identity = first.identity();
    let store = first.store();
    let user = store
        .create_user("local-viewer", "test-hash", false)
        .await
        .expect("native viewer");
    let library = store
        .create_library(&NewLibrary {
            name: "Local films".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.path().join("media")],
            anime: false,
        })
        .await
        .expect("native library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Retained native resume".into(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("native catalogue item after Source initialization");
    store
        .put_setting("test.standalone.current", "after-native-selection")
        .await
        .expect("current native setting");
    let progress = store
        .put_progress_at(user.id, item, 42_000, Some(100_000), None)
        .await
        .expect("durable native progress");
    drop(store);
    first.shutdown().await.expect("first native shutdown");
    drop(first);
    let mut second = open_native(&config).await;
    assert_eq!(second.identity(), identity);
    second
        .prepare_source_before_serving()
        .await
        .expect("native Source restart");
    let store = second.store();
    assert_eq!(
        store
            .get_setting("test.standalone.current")
            .await
            .expect("retained setting")
            .as_deref(),
        Some("after-native-selection")
    );
    assert_eq!(
        store
            .get_user_by_username("local-viewer")
            .await
            .expect("retained viewer")
            .expect("viewer row")
            .id,
        user.id
    );
    assert_eq!(
        store
            .get_library(library.id)
            .await
            .expect("retained library")
            .expect("library row")
            .name,
        "Local films"
    );
    assert_eq!(
        store
            .watch_state(user.id, item)
            .await
            .expect("retained progress"),
        Some(progress)
    );
    assert_no_cluster_artifacts(directory.path());
    second.shutdown().await.expect("second native shutdown");
}

#[tokio::test]
async fn standalone_committed_mode_cannot_be_changed_by_a_cluster_preference() {
    let directory = tempfile::tempdir().expect("standalone root");
    let (mut config, _occupied) = configuration(directory.path(), None);
    let runtime = open_native(&config).await;
    let identity = runtime.identity();
    runtime
        .store()
        .put_setting("test.standalone.current", "keep-this-library")
        .await
        .expect("current data");
    runtime.shutdown().await.expect("native shutdown");
    drop(runtime);
    config.storage.mode = Some(StorageMode::Cluster);
    assert!(
        select_for_test(&config).await.is_err(),
        "editing a preference cannot replace committed native authority"
    );
    assert_no_cluster_artifacts(directory.path());
    config.storage.mode = None;
    config.cluster.join_token_file = directory.path().join("explicit-join-token");
    assert!(
        select_for_test(&config).await.is_err(),
        "join intent cannot overwrite native authority"
    );
    assert_no_cluster_artifacts(directory.path());
    config.cluster.join_token_file = PathBuf::new();
    config.storage.mode = Some(StorageMode::Sqlite);
    let reopened = open_native(&config).await;
    assert_eq!(reopened.identity(), identity);
    assert_eq!(
        reopened
            .store()
            .get_setting("test.standalone.current")
            .await
            .expect("current data intact")
            .as_deref(),
        Some("keep-this-library")
    );
    reopened.shutdown().await.expect("native shutdown");
}

#[tokio::test]
async fn standalone_missing_corrupt_or_mismatched_manifest_refuses_without_fallback() {
    let directory = tempfile::tempdir().expect("standalone root");
    let (config, _occupied) = configuration(directory.path(), None);
    let runtime = open_native(&config).await;
    runtime
        .store()
        .put_setting("test.standalone.current", "never-reinitialize")
        .await
        .expect("current data");
    runtime.shutdown().await.expect("native shutdown");
    drop(runtime);
    let path = directory.path().join(MANIFEST_FILENAME);
    let original = std::fs::read(&path).expect("original manifest");
    std::fs::remove_file(&path).expect("simulate lost manifest");
    assert!(
        select_for_test(&config).await.is_err(),
        "bound native data must not be treated as a fresh legacy directory"
    );
    std::fs::write(&path, b"{not-a-manifest").expect("simulate corrupt manifest");
    assert!(select_for_test(&config).await.is_err());
    let actual: serde_json::Value = serde_json::from_slice(&original).expect("actual manifest");
    for (field, value) in [
        ("version", serde_json::json!(999)),
        (
            "generation",
            serde_json::json!(uuid::Uuid::new_v4().to_string()),
        ),
        ("node_id", serde_json::json!("other-owner")),
        ("database", serde_json::json!("../outside.db")),
    ] {
        let mut modified = actual.clone();
        modified[field] = value;
        std::fs::write(
            &path,
            serde_json::to_vec(&modified).expect("fault manifest"),
        )
        .expect("publish simulated fault");
        assert!(
            select_for_test(&config).await.is_err(),
            "{field} mismatch cannot select another database"
        );
        assert_no_cluster_artifacts(directory.path());
    }
    std::fs::write(&path, original).expect("restore exact authority record");
    let reopened = open_native(&config).await;
    assert_eq!(
        reopened
            .store()
            .get_setting("test.standalone.current")
            .await
            .expect("original data intact")
            .as_deref(),
        Some("never-reinitialize")
    );
    reopened.shutdown().await.expect("native shutdown");
}

#[tokio::test]
async fn lost_native_manifest_cannot_be_bypassed_by_cluster_configuration() {
    let directory = tempfile::tempdir().expect("native authority fixture");
    let (mut config, _occupied) = configuration(directory.path(), None);
    let runtime = open_native(&config).await;
    runtime
        .store()
        .put_setting("test.bound-native", "preserve")
        .await
        .expect("accepted native write");
    runtime.shutdown().await.expect("native shutdown");
    drop(runtime);
    std::fs::remove_file(directory.path().join(MANIFEST_FILENAME)).expect("lost manifest fixture");
    config.storage.mode = Some(StorageMode::Cluster);
    assert!(select_for_test(&config).await.is_err());
    assert_no_cluster_artifacts(directory.path());
    let database = rusqlite::Connection::open_with_flags(
        directory.path().join("plurx.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("retained native authority");
    let value: String = database
        .query_row(
            "SELECT value FROM settings WHERE key='test.bound-native'",
            [],
            |row| row.get(0),
        )
        .expect("accepted data retained");
    assert_eq!(value, "preserve");
}

/// Missing optional migration tooling must never make the old SQLite copy
/// current authority. Check the exact on-disk bytes before any writer opens.
#[cfg(not(feature = "cluster"))]
#[tokio::test]
async fn standalone_legacy_markers_refuse_before_retained_sqlite_or_keys_are_opened() {
    for marker in [
        "hiqlite",
        "hiqlite.incoming",
        "hiqlite-activated.json",
        "membership.json",
    ] {
        let root = tempfile::tempdir().expect("legacy root");
        let retained = b"retained predecessor is not current authority";
        std::fs::write(root.path().join("plurx.db"), retained).expect("retained predecessor");
        if marker.starts_with("hiqlite") && !marker.ends_with(".json") {
            std::fs::create_dir(root.path().join(marker)).expect("legacy directory");
        } else {
            std::fs::write(root.path().join(marker), b"{}").expect("legacy marker");
        }
        let (config, _ports) = configuration(root.path(), None);
        assert!(select_for_test(&config).await.is_err());
        assert_eq!(
            std::fs::read(root.path().join("plurx.db")).expect("predecessor intact"),
            retained
        );
        assert!(!root.path().join(MANIFEST_FILENAME).exists());
        assert!(!config.credential_key_path().exists());
        assert!(!root.path().join("node.id").exists());
    }
}
