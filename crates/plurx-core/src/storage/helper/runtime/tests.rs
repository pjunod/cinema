use super::*;
use crate::storage::helper::tests::{binding, build, fixture};

fn key_context(root: &Path, config: &Config, request: &PreparedRequest) -> KeyPublication {
    KeyPublication {
        root: root.to_path_buf(),
        request: request.clone(),
        key_path: config.credential_key_path(),
        original: None,
    }
}

#[tokio::test]
async fn missing_empty_key_is_bound_before_publication_and_resumes_same_inode() {
    let (directory, config, lease) = fixture();
    let request = prepare(&config, &lease, binding()).expect("prepare");
    KEY_PUBLICATION
        .scope(key_context(directory.path(), &config, &request), async {
            let census = SealedRowCensus::default();
            let key = open_actual_census_key(&config.credential_key_path(), &census)
                .expect("first publication");
            let intent: KeyIntent = read_record(
                &operation_path(directory.path(), &request.id, "key.json").expect("path"),
            )
            .expect("read intent")
            .expect("durable key intent");
            let original = file_binding(&config.credential_key_path())
                .expect("key")
                .expect("present");
            assert_eq!(original, intent.binding);
            fs::remove_file(config.credential_key_path())
                .expect("simulate crash before final name");
            let resumed = open_actual_census_key(&config.credential_key_path(), &census)
                .expect("resume bound stage");
            assert_eq!(resumed.id(), key.id());
            assert_eq!(
                file_binding(&config.credential_key_path()).expect("resumed binding"),
                Some(original)
            );
        })
        .await;
}

#[tokio::test]
async fn missing_key_with_actual_sealed_inventory_is_never_created() {
    let (directory, config, lease) = fixture();
    let unrelated = tempfile::tempdir().expect("other key directory");
    let key = crate::secrets::open_credential_key(
        &unrelated.path().join("key"),
        &SealedRowCensus::default(),
    )
    .expect("original wrapping key");
    let sealed = key.seal_trakt(7, "durable bearer").expect("sealed row");
    let mut census = SealedRowCensus::default();
    census.observe_row(&sealed, &sealed);
    let request = prepare(&config, &lease, binding()).expect("prepare");
    KEY_PUBLICATION
        .scope(key_context(directory.path(), &config, &request), async {
            assert!(open_actual_census_key(&config.credential_key_path(), &census).is_err());
            assert!(!config.credential_key_path().exists());
            assert!(!operation_path(directory.path(), &request.id, "key.json")
                .expect("path")
                .exists());
        })
        .await;
}

#[tokio::test]
async fn appearing_key_and_replaced_bound_stage_are_preserved_and_refused() {
    let (directory, config, lease) = fixture();
    let request = prepare(&config, &lease, binding()).expect("prepare");
    KEY_PUBLICATION
        .scope(key_context(directory.path(), &config, &request), async {
            let census = SealedRowCensus::default();
            fs::write(config.credential_key_path(), b"foreign").expect("appearing key");
            assert!(open_actual_census_key(&config.credential_key_path(), &census).is_err());
            assert_eq!(
                fs::read(config.credential_key_path()).expect("preserved key"),
                b"foreign"
            );
            fs::remove_file(config.credential_key_path()).expect("remove test injection");
            drop(
                open_actual_census_key(&config.credential_key_path(), &census)
                    .expect("bound publication"),
            );
            let intent: KeyIntent = read_record(
                &operation_path(directory.path(), &request.id, "key.json").expect("path"),
            )
            .expect("read")
            .expect("intent");
            fs::remove_file(config.credential_key_path()).expect("simulate missing final key");
            let staged = config
                .credential_key_path()
                .parent()
                .expect("parent")
                .join(&intent.staged_name);
            fs::write(&staged, b"changed").expect("corrupt stage");
            assert!(open_actual_census_key(&config.credential_key_path(), &census).is_err());
            assert!(!config.credential_key_path().exists());
            assert_eq!(fs::read(staged).expect("preserved stage"), b"changed");
        })
        .await;
}

#[tokio::test]
async fn executable_mismatch_refuses_before_legacy_runtime_or_key_creation() {
    let (directory, config, lease) = fixture();
    let request = prepare(&config, &lease, binding()).expect("prepare");
    drop(lease);
    let error = run(
        directory.path(),
        &config.credential_key_path(),
        &request.id,
        &request.sha256,
        build(),
    )
    .await
    .expect_err("wrong executable");
    assert!(error.to_string().contains("actual helper executable"));
    assert!(!config.credential_key_path().exists());
    assert!(!directory.path().join("hiqlite/state_machine/db").exists());
    assert!(NativeDirectoryLock::try_acquire(directory.path()).is_ok());
}

fn conversion_fixture(root: &Path, request: &PreparedRequest) -> ConversionIntent {
    publish_record(
        &root.join("hiqlite-activated.json"),
        &serde_json::json!({
            "cluster_id":"server", "source_backup_sha256":"a".repeat(64),
        }),
    )
    .expect("retained original activation breadcrumb");
    let directory = root.join("native-import-test");
    fs::create_dir(&directory).expect("incoming");
    let manifest = StorageManifest {
        version: 1,
        mode: StorageMode::Sqlite,
        database: "plurx.db".into(),
        instance_id: "server".into(),
        node_id: "node".into(),
        generation: uuid::Uuid::new_v4().to_string(),
        schema_version: 124,
    };
    let database = rusqlite::Connection::open(directory.join("plurx.db")).expect("native target");
    database.execute_batch("PRAGMA user_version=124; CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT); INSERT INTO settings VALUES('instance.id','server'); CREATE TABLE storage_identity(singleton INTEGER PRIMARY KEY,instance_id TEXT,node_id TEXT,generation TEXT); CREATE TABLE user_changes(value TEXT);").expect("native identity schema");
    database
        .execute(
            "INSERT INTO storage_identity VALUES(1,?1,?2,?3)",
            rusqlite::params![manifest.instance_id, manifest.node_id, manifest.generation],
        )
        .expect("native identity");
    drop(database);
    fs::write(directory.join("receipt.json"), b"{\"fixture\":true}").expect("receipt");
    fs::write(directory.join("source.sqlite"), b"immutable source fixture").expect("source cut");
    ConversionIntent {
        request_sha256: request.sha256.clone(),
        manifest,
        incoming_database: "native-import-test/plurx.db".into(),
        retired_directory: "retired-storage/test".into(),
        receipt_path: "native-import-test/receipt.json".into(),
        receipt_sha256: hash_regular(&directory.join("receipt.json"), RECEIPT_LIMIT)
            .expect("receipt hash"),
        source_image: VerifiedArtifact {
            path: "native-import-test/source.sqlite".into(),
            sha256: hash_regular(&directory.join("source.sqlite"), u64::MAX).expect("source hash"),
        },
        target_sha256: hash_regular(&directory.join("plurx.db"), u64::MAX).expect("target hash"),
    }
}

#[test]
fn operation_recovery_finishes_interrupted_retirement_and_preserves_later_native_writes() {
    let (directory, config, lease) = fixture();
    let root = directory.path();
    let request = prepare(&config, &lease, binding()).expect("request");
    let intent = conversion_fixture(root, &request);
    publish_record(
        &operation_path(root, &request.id, "conversion.json").expect("path"),
        &intent,
    )
    .expect("durable conversion binding");
    let handoff = crate::storage::transition::prepare_native_handoff_with_source(
        root,
        intent.manifest.clone(),
        intent.incoming_database.clone(),
        intent.retired_directory.clone(),
        intent.receipt_path.clone(),
        intent.source_image.clone(),
    )
    .expect("native handoff");
    fs::create_dir_all(root.join(&intent.retired_directory)).expect("retired directory");
    fs::rename(
        root.join("hiqlite"),
        root.join(&intent.retired_directory).join("hiqlite"),
    )
    .expect("simulate crash after source rename");
    let result = recover_conversion(root, &request)
        .expect("recover")
        .expect("completed");
    assert_eq!(result.manifest, intent.manifest);
    assert_eq!(result.receipt_sha256, intent.receipt_sha256);
    let native =
        rusqlite::Connection::open(root.join("plurx.db")).expect("published native database");
    native
        .execute(
            "INSERT INTO user_changes VALUES('accepted native change')",
            [],
        )
        .expect("accepted write");
    drop(native);
    assert_ne!(
        hash_regular(&root.join("plurx.db"), u64::MAX).expect("live image hash"),
        handoff.target_sha256
    );
    fs::remove_file(operation_path(root, &request.id, "result.json").expect("path"))
        .expect("simulate lost result publication");
    assert_eq!(
        recover_conversion(root, &request).expect("repeat recovery"),
        Some(result.clone())
    );
    assert_eq!(
        validate_completed(&lease, &request).expect("parent validation"),
        result
    );
    assert!(pending(root).expect("cleared marker").is_none());
    assert!(root
        .join(&intent.retired_directory)
        .join("hiqlite/activation.json")
        .is_file());
}

#[test]
fn changed_conversion_image_or_wrong_request_cannot_publish_authority() {
    let (directory, config, lease) = fixture();
    let root = directory.path();
    let request = prepare(&config, &lease, binding()).expect("request");
    let mut intent = conversion_fixture(root, &request);
    intent.request_sha256 = "e".repeat(64);
    let path = operation_path(root, &request.id, "conversion.json").expect("path");
    publish_record(&path, &intent).expect("wrong request binding");
    assert!(recover_conversion(root, &request).is_err());
    intent.request_sha256 = request.sha256.clone();
    publish_record(&path, &intent).expect("correct request binding");
    fs::write(
        root.join(&intent.incoming_database),
        b"unexpected replacement",
    )
    .expect("corrupt target");
    assert!(recover_conversion(root, &request).is_err());
    assert!(root.join("hiqlite/activation.json").is_file());
    assert!(!root.join("storage-mode.json").exists());
}

#[test]
fn metadata_recovery_allows_only_bound_schema_advance_and_matching_breadcrumb() {
    let (directory, config, lease) = fixture();
    let root = directory.path();
    let activation = serde_json::json!({"cluster_id":"server", "source_backup_sha256":"a".repeat(64), "replicated_schema_version":81, "admitted_role":null, "retained_field":"exact"});
    publish_record(&root.join("hiqlite/activation.json"), &activation)
        .expect("original activation");
    let prepared = prepare(&config, &lease, binding()).expect("prepare");
    let request = read_request(root, &prepared).expect("original request");
    let mut advanced = activation.clone();
    advanced["replicated_schema_version"] = 82.into();
    publish_record(&root.join("hiqlite/activation.json"), &advanced).expect("advance");
    assert!(
        validate_source_metadata(root, &prepared, &request).is_err(),
        "mutation before owned opening refuses"
    );
    publish_record(
        &operation_path(root, &prepared.id, "opening.json").expect("path"),
        &Opening {
            request_sha256: prepared.sha256.clone(),
        },
    )
    .expect("owned opening");
    publish_record(
        &root.join("hiqlite-activated.json"),
        &serde_json::json!({"cluster_id":"server", "source_backup_sha256":"a".repeat(64)}),
    )
    .expect("matching breadcrumb");
    validate_source_metadata(root, &prepared, &request).expect("known opener mutations recover");
    advanced["retained_field"] = "unexpected".into();
    publish_record(&root.join("hiqlite/activation.json"), &advanced).expect("unknown mutation");
    assert!(validate_source_metadata(root, &prepared, &request).is_err());
    assert_eq!(
        read_record::<serde_json::Value>(&root.join("hiqlite/activation.json"))
            .expect("preserved record")
            .expect("present")["retained_field"],
        "unexpected"
    );
}

#[test]
fn selected_lock_descriptor_cannot_be_rebound_to_another_root() {
    let first = tempfile::tempdir().expect("first root");
    let second = tempfile::tempdir().expect("second root");
    let first_lease = NativeDirectoryLock::try_acquire(first.path()).expect("first lease");
    let _second_lease = NativeDirectoryLock::try_acquire(second.path()).expect("second lease");
    let descriptor = first_lease
        .duplicate_locked_file()
        .expect("owned descriptor");
    assert!(
        NativeDirectoryLock::from_locked_file(descriptor, second.path().to_path_buf()).is_err()
    );
}

use crate as observer_core;
#[allow(
    dead_code,
    reason = "the shared startup fixture also serves multi-peer migration tests"
)]
mod startup_observer {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/startup_observer.rs"
    ));
}

/// Full boot, SDK migration, online export, sidecar transfer, native publication
/// and native reopen. The first case exercises a missing credential file with a
/// real empty inventory. The second preserves real wrapped credentials under
/// the original key and the installed AUTH82 Source layout.
#[test]
fn actual_private_helper_converts_auth81_missing_key_and_auth82_wrapped_rows() {
    startup_observer::run_full_hiqlite_fixture("actual-private-storage-helper", || async {
        let executable = std::env::current_exe().expect("actual helper test executable");
        let executable_hash =
            hash_regular(&executable, 2 * 1024 * 1024 * 1024).expect("actual executable hash");
        for cohort in [81, 82] {
            actual_private_helper_case(cohort, &executable_hash).await;
        }
    });
}

async fn actual_private_helper_case(cohort: i64, executable_hash: &str) {
    use crate::domain::{PlaybackEvent, PlaybackEventQuery, TraktAuth};
    use crate::store::{HiqliteAuthStore, SqliteStore};
    let directory = tempfile::tempdir().expect("legacy data root");
    let root = directory.path();
    let mut config = Config::default();
    config.storage.data_dir = root.to_path_buf();
    // Reserve distinct loopback endpoints together before handing them to the
    // actual SDK. No advertised host or explicit cluster preference is set.
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve Raft endpoint");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve API endpoint");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    drop((raft, api));
    drop(SqliteStore::open(&root.join("plurx.db")).expect("original pre-cluster SQLite source"));
    let mut selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("real private legacy voter");
    let server_id = selected.identity.cluster_id.clone();
    let node_id = selected.identity.node_id.clone();
    let original_key = selected.credential_key.id().to_owned();
    let user_id = if cohort == 82 {
        selected
            .store
            .put_setting("sharing_enabled", "true")
            .await
            .expect("saved sharing choice");
        assert!(selected
            .prepare_source_schema_before_serving()
            .await
            .expect("actual Source activation"));
        let user = selected
            .store
            .create_user("helper-account", "fixture-hash", false)
            .await
            .expect("durable user");
        let auth = TraktAuth {
            user_id: user.id,
            access_token: selected
                .credential_key
                .seal_trakt(user.id, "access-fixture")
                .expect("wrap access"),
            refresh_token: selected
                .credential_key
                .seal_trakt(user.id, "refresh-fixture")
                .expect("wrap refresh"),
            expires_at: 40,
            trakt_username: None,
            connected_at: 20,
            last_sync_at: 0,
            last_activities: None,
        };
        selected
            .store
            .put_trakt_auth(&auth)
            .await
            .expect("persist wrapped credential");
        Some(user.id)
    } else {
        assert!(selected
            .store
            .list_trakt_auth()
            .await
            .expect("original Trakt census")
            .is_empty());
        assert_eq!(
            selected
                .store
                .sharing_sealed_census()
                .await
                .expect("original sharing census")
                .sealed_rows(),
            0
        );
        None
    };
    let client = selected
        .local_client()
        .expect("actual replicated source client");
    assert_eq!(
        HiqliteAuthStore::committed_schema_version_for_client(&client)
            .await
            .expect("committed cohort"),
        cohort
    );
    drop(client);
    selected
        .store
        .put_setting("helper.continuity", "durable replicated row")
        .await
        .expect("durable setting");
    selected
        .store
        .record_playback_event(&PlaybackEvent {
            event: "helper-fixture".into(),
            at_unix_ms: 12345,
            detail: Some("node-local observation".into()),
            ..Default::default()
        })
        .await
        .expect("real node-local playback event");
    selected
        .shutdown()
        .await
        .expect("drain the original SDK voter");
    drop(selected);
    let key = config.credential_key_path();
    if cohort == 81 {
        fs::remove_file(&key).expect("remove key after proving empty stored inventory");
    }
    let lease = NativeDirectoryLock::acquire(root)
        .await
        .expect("launcher independently acquires root");
    let mut expected = binding();
    expected.executable_sha256 = executable_hash.to_owned();
    let prepared = prepare(&config, &lease, expected).expect("durable helper request");
    drop(lease);
    let result = Box::pin(run(root, &key, &prepared.id, &prepared.sha256, build()))
        .await
        .expect("real helper conversion");
    let lease = NativeDirectoryLock::acquire(root)
        .await
        .expect("parent independently reacquires root");
    assert_eq!(
        validate_completed(&lease, &prepared).expect("durable validated completion"),
        result
    );
    let receipt: serde_json::Value = serde_json::from_reader(
        fs::File::open(root.join(&result.receipt_path)).expect("conversion receipt"),
    )
    .expect("cohort receipt");
    assert_eq!(receipt["replicated"]["source_schema_version"], cohort);
    assert_eq!(receipt["replicated"]["cluster_id"], server_id);
    let telemetry = receipt["node_local"]["tables"]
        .as_array()
        .expect("local telemetry receipt");
    assert!(telemetry
        .iter()
        .any(|table| table["table"] == "playback_events"
            && table["rows"].as_u64().is_some_and(|rows| rows >= 1)));
    let native = crate::storage::open_native_store_locked(&config, lease)
        .await
        .expect("ordinary native reopen");
    assert_eq!(native.identity.instance_id, server_id);
    assert_eq!(native.identity.node_id, node_id);
    assert_eq!(
        native
            .store
            .get_setting("helper.continuity")
            .await
            .expect("native setting")
            .as_deref(),
        Some("durable replicated row")
    );
    let events = native
        .store
        .playback_events(&PlaybackEventQuery {
            event: Some("helper-fixture".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("native telemetry");
    assert!(events.iter().any(|event| event.at_unix_ms == 12345
        && event.detail.as_deref() == Some("node-local observation")));
    if let Some(user_id) = user_id {
        assert_eq!(native.credential_key.id(), original_key);
        let auth = native
            .store
            .get_trakt_auth(user_id)
            .await
            .expect("native credentials")
            .expect("retained encrypted row");
        assert!(auth.is_wrapped());
        assert_eq!(
            auth.reveal_access_token(&native.credential_key)
                .expect("actual access decryption")
                .expose(),
            "access-fixture"
        );
        assert_eq!(
            auth.reveal_refresh_token(&native.credential_key)
                .expect("actual refresh decryption")
                .expose(),
            "refresh-fixture"
        );
    } else {
        assert_ne!(native.credential_key.id(), original_key);
        let intent: KeyIntent =
            read_record(&operation_path(root, &prepared.id, "key.json").expect("key ledger path"))
                .expect("durable key publication")
                .expect("actual publication intent");
        assert_eq!(
            file_binding(&key).expect("published key identity"),
            Some(intent.binding)
        );
    }
    native
        .sqlite
        .close()
        .await
        .expect("close actual native connections");
}
