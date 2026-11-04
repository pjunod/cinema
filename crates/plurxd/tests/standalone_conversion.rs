//! Shipping-daemon regression for an existing private one-voter installation.
#![cfg(feature = "cluster-integration-tests")]
// Test-owned children are intentionally launched without the production launcher.
#![allow(clippy::disallowed_methods)]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use plurx_core::auth::{hash_password, hash_token};
use plurx_core::cluster::migration::SelectedBackend;
use plurx_core::config::Config;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, WatchState};
use plurx_core::storage::mode::{StorageManifest, StorageMode};
use plurx_core::store::{
    LibraryStore, MediaStore, SettingsStore, SqliteStore, UserStore, WatchStore,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use plurx_core as observer_core;
#[allow(
    dead_code,
    reason = "shared fixture also provides plural-node observation owners"
)]
#[path = "../../plurx-core/tests/fixtures/startup_observer.rs"]
mod startup_observer;

struct OwnedDaemon {
    child: Child,
    capture: tempfile::NamedTempFile,
}

impl OwnedDaemon {
    fn start(config: &Path) -> Self {
        let capture = tempfile::NamedTempFile::new().expect("private owned child log");
        let stdout = std::fs::OpenOptions::new()
            .append(true)
            .open(capture.path())
            .expect("real daemon conversion fixture");
        let stderr = stdout.try_clone().expect("real daemon conversion fixture");
        let child = Command::new(env!("CARGO_BIN_EXE_plurxd"))
            .args(["--config", config.to_str().expect("config path"), "run"])
            .env("PLURX_LOG", "info")
            .env("PLURX_LOG_FORMAT", "json")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("start the shipping daemon");
        Self { child, capture }
    }

    fn log_tail(&self) -> String {
        let logs = std::fs::read(self.capture.path()).unwrap_or_default();
        String::from_utf8_lossy(&logs[logs.len().saturating_sub(16_384)..]).into_owned()
    }

    fn owns_http_listener(&mut self, address: SocketAddr) -> bool {
        assert!(
            self.child.try_wait().expect("owned child status").is_none(),
            "shipping daemon exited before readiness: {}",
            self.log_tail()
        );
        let logs = std::fs::read_to_string(self.capture.path()).expect("owned child capture");
        logs.lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|event| {
                event["level"].as_str() == Some("INFO")
                    && event["target"].as_str() == Some("plurxd")
                    && event["fields"]["message"].as_str() == Some("listening")
                    && event["fields"]["addr"]
                        .as_str()
                        .and_then(|value| value.parse::<SocketAddr>().ok())
                        == Some(address)
            })
        })
    }

    async fn ready(&mut self, client: &reqwest::Client, port: u16) {
        let deadline = Instant::now() + Duration::from_secs(180);
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        loop {
            if self.owns_http_listener(address) {
                if let Ok(response) = client.get(format!("http://{address}/readyz")).send().await {
                    if response.status().is_success() {
                        return;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "native conversion/readiness timed out: {}",
                self.log_tail()
            );
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    }

    async fn stop(&mut self) {
        if self.child.try_wait().expect("owned child status").is_none() {
            #[cfg(unix)]
            {
                // SAFETY: this PID belongs to the live child owned by this guard.
                assert_eq!(
                    unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) },
                    0
                );
            }
            #[cfg(not(unix))]
            self.child.kill().expect("stop owned child");
        }
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Some(status) = self.child.try_wait().expect("owned child shutdown status") {
                #[cfg(unix)]
                assert!(
                    status.success(),
                    "native daemon did not shut down cleanly: {status}; {}",
                    self.log_tail()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "native child failed to drain before its deadline: {}",
                self.log_tail()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "owned daemon {} log tail:\n{}",
                self.child.id(),
                self.log_tail()
            );
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn free_ports() -> [u16; 3] {
    let listeners: [TcpListener; 3] = std::array::from_fn(|_| {
        TcpListener::bind("127.0.0.1:0").expect("real daemon conversion fixture")
    });
    std::array::from_fn(|index| {
        listeners[index]
            .local_addr()
            .expect("real daemon conversion fixture")
            .port()
    })
}

fn quoted_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn assert_no_consensus_listeners(ports: [u16; 2]) {
    for port in ports {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err(),
            "native service unexpectedly has a configured Raft/private API listener at {address}"
        );
    }
}

fn only_retired_generation(data: &Path) -> PathBuf {
    let generations = std::fs::read_dir(data.join("retired-storage"))
        .expect("retired backend generations")
        .map(|entry| entry.expect("real daemon conversion fixture").path())
        .collect::<Vec<_>>();
    assert_eq!(
        generations.len(),
        1,
        "the conversion must retain one identifiable old backend"
    );
    generations
        .into_iter()
        .next()
        .expect("real daemon conversion fixture")
}

async fn assert_native_rows(
    data: &Path,
    user_id: i64,
    library_id: i64,
    item_id: i64,
    watch: WatchState,
    token: &str,
) {
    let store = SqliteStore::open(&data.join("plurx.db"))
        .expect("ordinary native Store opens current authority");
    assert_eq!(
        store
            .get_setting("conversion.after_activation")
            .await
            .expect("real daemon conversion fixture")
            .as_deref(),
        Some("current replicated value")
    );
    assert_eq!(
        store
            .get_user(user_id)
            .await
            .expect("real daemon conversion fixture")
            .expect("preserved owner")
            .username,
        "conversion-owner"
    );
    assert_eq!(
        store
            .get_library(library_id)
            .await
            .expect("real daemon conversion fixture")
            .expect("preserved library")
            .name,
        "Conversion library"
    );
    let item = store
        .get_item(item_id)
        .await
        .expect("real daemon conversion fixture")
        .expect("preserved catalogue item");
    assert_eq!(item.id, item_id);
    assert_eq!(item.library_id, library_id);
    assert_eq!(item.title, "Post-activation title");
    assert_eq!(
        store
            .watch_state(user_id, item_id)
            .await
            .expect("real daemon conversion fixture"),
        Some(watch)
    );
    assert_eq!(
        store
            .user_for_token(&hash_token(token))
            .await
            .expect("real daemon conversion fixture")
            .expect("native-issued login token")
            .id,
        user_id
    );
}

/// The legacy fixture uses the old activation API, while both subsequent
/// boots execute the shipping daemon's real RuntimeStore selection. Reading
/// the stale original SQLite file would lose every asserted post-activation
/// fact. The native-issued token additionally proves post-publication writes
/// survive the second boot instead of being rejected by an initial file hash.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_private_singleton_boot_converts_current_state_to_native_and_restarts_without_raft(
) {
    let _owner = CONVERSION_CASE.lock().await;
    convert_private_fixture(false).await;
}

static CONVERSION_CASE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_enabled_private_singleton_retains_its_keys_through_native_conversion() {
    let _owner = CONVERSION_CASE.lock().await;
    convert_private_fixture(true).await;
}

async fn convert_private_fixture(initialized_source: bool) {
    let temporary = std::fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
    let root = tempfile::tempdir_in(temporary).expect("isolated conversion fixture");
    let data = root.path().join("data");
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).expect("real daemon conversion fixture");
    let config_path = root.path().join("plurx.toml");
    let [http_port, raft_port, api_port] = free_ports();
    std::fs::write(&config_path,format!(
        "[server]\nbind=\"127.0.0.1:{http_port}\"\n[storage]\ndata_dir=\"{}\"\n[cluster]\nraft_bind=\"127.0.0.1:{raft_port}\"\napi_bind=\"127.0.0.1:{api_port}\"\n",
        quoted_path(&data),
    )).expect("real daemon conversion fixture");
    let config = Config::load(Some(&config_path)).expect("private singleton config");
    assert_eq!(
        config.storage.data_dir, data,
        "environment must not redirect the fixture to another installation"
    );
    assert_eq!(
        config.server.bind,
        SocketAddr::from(([127, 0, 0, 1], http_port))
    );
    assert_eq!(
        config.cluster.raft_bind,
        SocketAddr::from(([127, 0, 0, 1], raft_port))
    );
    assert_eq!(
        config.cluster.api_bind,
        SocketAddr::from(([127, 0, 0, 1], api_port))
    );
    assert_eq!(config.storage.mode, None);
    assert!(config.cluster.advertise_host.is_empty());
    assert!(config.cluster.join_token_file.as_os_str().is_empty());
    let mut selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("legacy one-voter activation fixture");
    assert_eq!(selected.backend, SelectedBackend::Replicated);
    if initialized_source {
        selected
            .store
            .put_setting("sharing_enabled", "1")
            .await
            .expect("explicit fixture Sharing enable");
        assert!(selected
            .prepare_source_schema_before_serving()
            .await
            .expect("current legacy Source/key layout"));
    }
    let instance_id = selected.identity.cluster_id.clone();
    let node_id = selected.identity.node_id.clone();
    let password = "conversion-test-password";
    let user = selected
        .store
        .create_user(
            "conversion-owner",
            &hash_password(password).expect("real daemon conversion fixture"),
            true,
        )
        .await
        .expect("real daemon conversion fixture");
    selected
        .store
        .put_setting("conversion.after_activation", "current replicated value")
        .await
        .expect("real daemon conversion fixture");
    let library = selected
        .store
        .create_library(&NewLibrary {
            name: "Conversion library".into(),
            kind: LibraryKind::Movies,
            paths: vec![media],
            anime: false,
        })
        .await
        .expect("real daemon conversion fixture");
    selected
        .store
        .set_library_schedule(library.id, 0, 0)
        .await
        .expect("real daemon conversion fixture");
    let item_id = selected
        .store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Post-activation title".into(),
            year: Some(2026),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("real daemon conversion fixture");
    let watch = selected
        .store
        .put_progress_at(user.id, item_id, 12_345, Some(120_000), None)
        .await
        .expect("real daemon conversion fixture");
    let credential_path = config.cluster.credential_key_path(&data);
    let credential = std::fs::read(&credential_path).expect("original credential key");
    selected
        .shutdown()
        .await
        .expect("close legacy fixture without losing its committed tail");
    drop(selected);
    let original_root =
        std::fs::read(data.join("plurx.db")).expect("retained pre-activation database");
    assert!(
        !data.join("storage-mode.json").exists(),
        "legacy fixture has no native mode handoff"
    );
    let stale =
        Connection::open_with_flags(data.join("plurx.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("real daemon conversion fixture");
    assert_eq!(
        stale
            .query_row(
                "SELECT value FROM settings WHERE key='conversion.after_activation'",
                [],
                |row| row.get::<_, String>(0)
            )
            .optional()
            .expect("real daemon conversion fixture"),
        None
    );
    drop(stale);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("real daemon conversion fixture");
    let mut first = OwnedDaemon::start(&config_path);
    first.ready(&client, http_port).await;
    let info = client
        .get(format!("http://127.0.0.1:{http_port}/api/v1/server"))
        .send()
        .await
        .expect("real daemon conversion fixture")
        .json::<serde_json::Value>()
        .await
        .expect("real daemon conversion fixture");
    assert_eq!(info["storage_mode"], "sqlite");
    assert_eq!(info["cluster_enabled"], false);
    let manifest = StorageManifest::read(&data)
        .expect("real daemon conversion fixture")
        .expect("published native mode");
    assert_eq!(manifest.mode, StorageMode::Sqlite);
    assert_eq!(manifest.instance_id, instance_id);
    assert_eq!(manifest.node_id, node_id);
    manifest
        .validate_sqlite(&data)
        .expect("real daemon conversion fixture");
    assert!(
        !data.join("hiqlite").exists(),
        "native authority must not retain an auto-detected active Raft directory"
    );
    assert!(
        data.join("hiqlite-activated.json").is_file(),
        "old binaries must retain their refusal breadcrumb"
    );
    assert_no_consensus_listeners([raft_port, api_port]);
    let retired = only_retired_generation(&data);
    assert!(retired.join("hiqlite/activation.json").is_file());
    assert_eq!(
        std::fs::read(retired.join("legacy-plurx.db")).expect("real daemon conversion fixture"),
        original_root,
        "the old root is retained, never selected as current data"
    );
    assert_eq!(
        std::fs::read(&credential_path).expect("real daemon conversion fixture"),
        credential
    );
    let response = client
        .post(format!("http://127.0.0.1:{http_port}/api/v1/auth/login"))
        .json(&serde_json::json!({"username":"conversion-owner","password":password}))
        .send()
        .await
        .expect("real daemon conversion fixture");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "{}",
        first.log_tail()
    );
    let token = response
        .json::<serde_json::Value>()
        .await
        .expect("real daemon conversion fixture")["token"]
        .as_str()
        .expect("real daemon conversion fixture")
        .to_owned();
    let backups = root.path().join("backups");
    std::fs::create_dir(&backups).expect("real daemon conversion fixture");
    let backup = client
        .post(format!("http://127.0.0.1:{http_port}/api/v1/backups"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"output":backups}))
        .send()
        .await
        .expect("real daemon conversion fixture");
    assert_eq!(
        backup.status(),
        reqwest::StatusCode::OK,
        "{}",
        first.log_tail()
    );
    let backup = backup
        .json::<serde_json::Value>()
        .await
        .expect("real daemon conversion fixture");
    assert_eq!(backup["status"], "ok");
    assert_eq!(backup["manifest"]["storage_backend"], "sqlite");
    let archive = PathBuf::from(
        backup["path"]
            .as_str()
            .expect("real daemon conversion fixture"),
    );
    plurx_core::storage::backup::verify_native_backup_artifact(&archive)
        .expect("real daemon conversion fixture");
    first.stop().await;
    assert_native_rows(&data, user.id, library.id, item_id, watch, &token).await;

    if !initialized_source {
        let restored = root.path().join("restored-native");
        let mut restore = tokio::process::Command::new(env!("CARGO_BIN_EXE_plurxd"));
        restore
            .args(["restore", "--archive"])
            .arg(&archive)
            .arg("--data-dir")
            .arg(&restored)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(90), restore.output())
            .await
            .expect("bounded native restore CLI")
            .expect("owned restore child");
        assert!(
            output.status.success(),
            "native restore CLI: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let restored_manifest = StorageManifest::read(&restored)
            .expect("restored manifest")
            .expect("native restore selected storage");
        assert_eq!(restored_manifest.mode, StorageMode::Sqlite);
        assert_eq!(restored_manifest.instance_id, instance_id);
        assert_ne!(restored_manifest.generation, manifest.generation);
        let restored_config = root.path().join("restored.toml");
        std::fs::write(&restored_config, format!("[server]\nbind=\"127.0.0.1:{http_port}\"\n[storage]\ndata_dir=\"{}\"\n[cluster]\nraft_bind=\"127.0.0.1:{raft_port}\"\napi_bind=\"127.0.0.1:{api_port}\"\n", quoted_path(&restored))).expect("restored native config");
        let mut restored_daemon = OwnedDaemon::start(&restored_config);
        restored_daemon.ready(&client, http_port).await;
        assert_no_consensus_listeners([raft_port, api_port]);
        let me = client
            .get(format!("http://127.0.0.1:{http_port}/api/v1/me"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("restored authentication");
        assert_eq!(
            me.status(),
            reqwest::StatusCode::OK,
            "{}",
            restored_daemon.log_tail()
        );
        restored_daemon.stop().await;
        assert_native_rows(&restored, user.id, library.id, item_id, watch, &token).await;
    }

    let mut restarted = OwnedDaemon::start(&config_path);
    restarted.ready(&client, http_port).await;
    assert_eq!(
        StorageManifest::read(&data).expect("real daemon conversion fixture"),
        Some(manifest)
    );
    assert_no_consensus_listeners([raft_port, api_port]);
    let me = client
        .get(format!("http://127.0.0.1:{http_port}/api/v1/me"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("real daemon conversion fixture");
    assert_eq!(
        me.status(),
        reqwest::StatusCode::OK,
        "{}",
        restarted.log_tail()
    );
    assert_eq!(
        me.json::<serde_json::Value>()
            .await
            .expect("real daemon conversion fixture")["id"]
            .as_i64(),
        Some(user.id)
    );
    restarted.stop().await;
    assert_native_rows(&data, user.id, library.id, item_id, watch, &token).await;
    assert_eq!(
        std::fs::read(only_retired_generation(&data).join("legacy-plurx.db"))
            .expect("real daemon conversion fixture"),
        original_root
    );
    assert_eq!(
        std::fs::read(&credential_path).expect("real daemon conversion fixture"),
        credential
    );
}
