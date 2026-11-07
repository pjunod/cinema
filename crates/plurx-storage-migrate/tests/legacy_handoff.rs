//! Real SDK source -> separately built CLI -> durable native SQLite handoff.
//! Signed archive staging is covered by the controller's independent fixtures.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::cluster::migration::{select_daemon_store_observing, StartupClockObserver};
use plurx_core::config::Config;
use plurx_core::domain::{PlaybackEvent, PlaybackEventQuery, TraktAuth};
use plurx_core::error::StoreError;
use plurx_core::storage::helper::{
    self, HelperBinding, HelperBuild, HelperDescription, HelperResult,
};
use plurx_core::storage::{open_native_store_locked, NativeDirectoryLock};
use plurx_core::store::SqliteStore;
use sha2::{Digest, Sha256};

// Reuse the production bounded child owner, including real terminal reap.
// Its existing cancellation/bounds tests remain compiled in this test target.
#[path = "../../plurxd/src/storage_helper/process.rs"]
mod process;

struct AppliedSingletonObserver;

impl StartupClockObserver for AppliedSingletonObserver {
    fn start(
        &self,
        manager: MembershipManager,
        _node: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>> {
        Box::pin(async move {
            let roster = manager
                .clock_peers()
                .await
                .map_err(|error| StoreError::Database(error.to_string()))?;
            let applied = roster.membership.as_ref().ok_or_else(|| {
                StoreError::Database("helper fixture has no actual applied membership".into())
            })?;
            let sole = BTreeSet::from([applied.local_node]);
            if applied.members != sole || applied.voters != sole || !roster.peers.is_empty() {
                return Err(StoreError::Database(
                    "helper fixture refuses remote or unapplied singleton membership".into(),
                ));
            }
            let clock = manager.clock_guard();
            let ticket = clock
                .roster_for_peer_directory(&roster)
                .map_err(|error| StoreError::Database(error.to_string()))?;
            if !clock.publish(ticket, BTreeMap::new()) {
                return Err(StoreError::Database(
                    "actual singleton roster changed before clock observation".into(),
                ));
            }
            Ok(())
        })
    }
}

fn executable_hash(path: &Path) -> String {
    let mut source = std::fs::File::open(path).expect("actual helper executable");
    let mut hash = Sha256::new();
    let mut block = [0_u8; 65_536];
    loop {
        let count = source.read(&mut block).expect("read actual artifact bytes");
        if count == 0 {
            break;
        }
        hash.update(&block[..count]);
    }
    format!("{:x}", hash.finalize())
}

#[test]
fn actual_helper_cli_hands_off_auth81_and_auth82_to_native_sqlite() {
    // The real SDK's finite debug-frame allowance matches the Core fixtures.
    let worker = std::thread::Builder::new()
        .name("actual-helper-cli-legacy-handoff".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("owned helper integration runtime")
                .block_on(Box::pin(async {
                    for cohort in [81, 82] {
                        handoff(cohort).await;
                    }
                }));
        })
        .expect("owned SDK fixture worker");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn handoff(cohort: i64) {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_plurx-storage-migrate"));
    assert!(executable.is_absolute());
    let expected_build = HelperBuild {
        commit: env!("PLURX_HELPER_COMMIT").to_owned(),
        tree: env!("PLURX_HELPER_TREE").to_owned(),
        target: env!("PLURX_HELPER_TARGET").to_owned(),
    };
    for object in [&expected_build.commit, &expected_build.tree] {
        assert!(
            object.len() == 40 && object.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "this dedicated integration lane requires real PLURX_BUILD_SHA and PLURX_BUILD_TREE"
        );
    }
    let artifact_hash = executable_hash(&executable);
    let directory = tempfile::tempdir().expect("owned real legacy root");
    let root = directory.path();
    let mut config = Config::default();
    config.storage.data_dir = root.to_owned();
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve Raft endpoint");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve API endpoint");
    config.cluster.raft_bind = raft.local_addr().expect("actual Raft address");
    config.cluster.api_bind = api.local_addr().expect("actual API address");
    drop((raft, api));
    let original = SqliteStore::open(&root.join("plurx.db")).expect("original SQLite source");
    original
        .close()
        .await
        .expect("close original SQLite connections");
    drop(original);
    let mut selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(&AppliedSingletonObserver),
    ))
    .await
    .expect("real applied private legacy singleton");
    let server_id = selected.identity.cluster_id.clone();
    let node_id = selected.identity.node_id.clone();
    let original_key = selected.credential_key.id().to_owned();
    let user_id = if cohort == 82 {
        selected
            .store
            .put_setting("sharing_enabled", "true")
            .await
            .expect("saved Source choice");
        assert!(selected
            .prepare_source_schema_before_serving()
            .await
            .expect("real AUTH82 activation"));
        let user = selected
            .store
            .create_user("cli-helper-account", "fixture-hash", false)
            .await
            .expect("durable account");
        selected
            .store
            .put_trakt_auth(&TraktAuth {
                user_id: user.id,
                access_token: selected
                    .credential_key
                    .seal_trakt(user.id, "cli-access")
                    .expect("wrap access"),
                refresh_token: selected
                    .credential_key
                    .seal_trakt(user.id, "cli-refresh")
                    .expect("wrap refresh"),
                expires_at: 40,
                trakt_username: None,
                connected_at: 20,
                last_sync_at: 0,
                last_activities: None,
            })
            .await
            .expect("durable wrapped credential");
        Some(user.id)
    } else {
        assert!(selected
            .store
            .list_trakt_auth()
            .await
            .expect("actual Trakt census")
            .is_empty());
        assert_eq!(
            selected
                .store
                .sharing_sealed_census()
                .await
                .expect("actual sharing census")
                .sealed_rows(),
            0
        );
        None
    };
    let client = selected.local_client().expect("actual SDK source");
    let mut committed = client
        .query_consistent(
            "SELECT schema_version FROM cluster_meta WHERE singleton=1",
            Vec::new(),
        )
        .await
        .expect("actual committed layout");
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].get::<i64>("schema_version"), cohort);
    drop(client);
    selected
        .store
        .put_setting("cli.continuity", "actual replicated value")
        .await
        .expect("durable setting");
    selected
        .store
        .record_playback_event(&PlaybackEvent {
            event: "cli-handoff".into(),
            at_unix_ms: 12345,
            detail: Some("actual node-local value".into()),
            ..Default::default()
        })
        .await
        .expect("actual local telemetry");
    selected
        .shutdown()
        .await
        .expect("drain actual original SDK");
    drop(selected);
    let key = config.credential_key_path();
    if cohort == 81 {
        std::fs::remove_file(&key).expect("remove key only after actual empty inventory census");
    }

    let lease = NativeDirectoryLock::acquire(root)
        .await
        .expect("parent owns root");
    let operation = helper::lock_supervisor(root).expect("parent owns outer operation");
    let inherited = operation
        .try_clone()
        .expect("description child operation lease");
    let (operation, output) = process::run_owned_inheriting(
        operation,
        executable.clone(),
        vec!["describe".into()],
        Duration::from_secs(30),
        vec![inherited],
    )
    .await
    .expect("actual helper description child was reaped");
    let description: HelperDescription =
        serde_json::from_slice(&output.stdout).expect("complete real Describe JSON");
    assert_eq!(
        description,
        helper::describe(expected_build.clone())
            .expect("same compiled converter and cohort contract")
    );
    assert_eq!(executable_hash(&executable), artifact_hash);
    let prepared = helper::prepare(
        &config,
        &lease,
        HelperBinding {
            daemon: expected_build,
            helper: description,
            executable_sha256: artifact_hash.clone(),
            // This CLI boundary fixture uses its actual one-file payload digest.
            // It does not claim an authenticated release archive or staged generation.
            bundle_sha256: artifact_hash,
        },
    )
    .expect("actual parent request under root ownership");
    assert_eq!(
        helper::pending(root).expect("durable pending operation"),
        Some(prepared.clone())
    );
    assert!(
        NativeDirectoryLock::try_acquire(root).is_err(),
        "root remains held before handoff"
    );
    let inherited = operation.try_clone().expect("run child operation lease");
    let args: Vec<OsString> = vec![
        "run".into(),
        "--root".into(),
        root.as_os_str().to_owned(),
        "--key".into(),
        key.as_os_str().to_owned(),
        "--request".into(),
        prepared.id.clone().into(),
        "--request-sha256".into(),
        prepared.sha256.clone().into(),
    ];
    drop(lease);
    let outcome = process::run_owned_inheriting(
        operation,
        executable,
        args,
        Duration::from_secs(600),
        vec![inherited],
    )
    .await;
    // Acquire afresh even on child failure; stdout alone never confers ownership.
    let lease = NativeDirectoryLock::acquire(root)
        .await
        .expect("parent reacquires after real reap");
    let (operation, output) =
        outcome.expect("actual separately built helper Run succeeded and reaped");
    let result: HelperResult =
        serde_json::from_slice(&output.stdout).expect("complete real Run JSON");
    let durable: HelperResult = serde_json::from_reader(
        std::fs::File::open(
            root.join("storage-migrations")
                .join(&prepared.id)
                .join("result.json"),
        )
        .expect("actual durable result"),
    )
    .expect("strict durable result JSON");
    assert_eq!(result, durable);
    assert_eq!(result.request, prepared);
    assert_eq!(
        helper::validate_completed(&lease, &prepared)
            .expect("revalidate real manifest/receipt/root"),
        result
    );
    assert!(helper::pending(root).expect("settled marker").is_none());
    assert_eq!(
        helper::validate_completed(&lease, &prepared).expect("idempotent validation"),
        result
    );
    let receipt_path = root.join(&result.receipt_path);
    assert_eq!(executable_hash(&receipt_path), result.receipt_sha256);
    let receipt: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(receipt_path).expect("actual conversion receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(receipt["replicated"]["source_schema_version"], cohort);
    assert_eq!(receipt["replicated"]["cluster_id"], server_id);
    assert!(receipt["node_local"]["tables"]
        .as_array()
        .expect("local table receipt")
        .iter()
        .any(|table| table["table"] == "playback_events"
            && table["rows"].as_u64().is_some_and(|rows| rows >= 1)));
    let native = open_native_store_locked(&config, lease)
        .await
        .expect("real ordinary native reopen");
    assert_eq!(native.identity.instance_id, server_id);
    assert_eq!(native.identity.node_id, node_id);
    assert_eq!(
        native
            .store
            .get_setting("cli.continuity")
            .await
            .expect("native replicated value")
            .as_deref(),
        Some("actual replicated value")
    );
    let events = native
        .store
        .playback_events(&PlaybackEventQuery {
            event: Some("cli-handoff".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("native node-local value");
    assert!(events.iter().any(|event| event.at_unix_ms == 12345
        && event.detail.as_deref() == Some("actual node-local value")));
    if let Some(user_id) = user_id {
        assert_eq!(native.credential_key.id(), original_key);
        let auth = native
            .store
            .get_trakt_auth(user_id)
            .await
            .expect("native credential query")
            .expect("retained wrapped row");
        assert!(auth.is_wrapped());
        assert_eq!(
            auth.reveal_access_token(&native.credential_key)
                .expect("actual access decryption")
                .expose(),
            "cli-access"
        );
        assert_eq!(
            auth.reveal_refresh_token(&native.credential_key)
                .expect("actual refresh decryption")
                .expose(),
            "cli-refresh"
        );
    } else {
        assert_ne!(native.credential_key.id(), original_key);
        let intent: serde_json::Value = serde_json::from_reader(
            std::fs::File::open(
                root.join("storage-migrations")
                    .join(&prepared.id)
                    .join("key.json"),
            )
            .expect("actual key publication ledger"),
        )
        .expect("key publication JSON");
        assert_eq!(intent["request_sha256"], prepared.sha256);
        assert_eq!(intent["binding"]["sha256"], executable_hash(&key));
    }
    native
        .sqlite
        .close()
        .await
        .expect("close actual native connections");
    drop(native);
    drop(operation);
    drop(NativeDirectoryLock::try_acquire(root).expect("all native/child root owners released"));
}
