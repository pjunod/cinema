//! Configuration: sane defaults, one optional TOML file, env overrides.
//! (REQ-OPS-2: defaults + file + env; settings edited at runtime live in the
//! Store, not here — this file covers only what's needed before the Store opens.)

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ConfigError;

/// Default HTTP port. Deliberately near — but never colliding with — the
/// 32400-era ports ex-Plex users already have muscle memory for.
pub const DEFAULT_PORT: u16 = 32400;
/// Default Raft replication port, adjacent to the public HTTP API.
pub const DEFAULT_RAFT_PORT: u16 = 32401;
/// Default authenticated node-to-node API port.
pub const DEFAULT_CLUSTER_API_PORT: u16 = 32402;
/// Maximum share of a library that one complete scan may remove.
pub const DEFAULT_SCAN_PRUNE_PERCENT: u8 = 10;
/// Default bounded-replica apply backlog. The optimization remains disabled
/// until an operator explicitly enables it cluster-wide.
pub const DEFAULT_BOUNDED_REPLICA_MAX_LAG_ENTRIES: u64 = 64;
pub const MAX_BOUNDED_REPLICA_MAX_LAG_ENTRIES: u64 = 10_000;
/// Default deadline for sending and installing one Raft snapshot segment.
pub const DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS: u64 = 120;
pub const MIN_INSTALL_SNAPSHOT_TIMEOUT_SECS: u64 = 10;
pub const MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS: u64 = 3_600;

const DEFAULT_CONFIG_PATHS: &[&str] = &["plurx.toml", "/etc/plurx/plurx.toml"];

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    /// Forward-compatible cluster settings. M0 reads these without changing
    /// the production SQLite backend; later clustering releases activate them.
    pub cluster: ClusterConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Human-visible server name (cluster-wide identity comes later; REQ-HA-5).
    pub name: String,
    /// Address the HTTP API binds to.
    pub bind: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            name: "plurx".to_owned(),
            bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Authoritative database, identity, secrets, and compatibility root.
    pub data_dir: PathBuf,
    /// Optional root for persistent node-local artwork, transcode, subtitle,
    /// and offline bytes. Empty preserves the legacy layout under `data_dir`.
    pub cache_dir: PathBuf,
    /// Optional disposable live-transcode scratch directory. Empty preserves
    /// the legacy `<data_dir>/transcode` path.
    pub transcode_dir: PathBuf,
    /// Refuse vanished-file reconciliation above this percentage of the
    /// library's known files. `0` disables automatic deletion.
    pub scan_prune_percent: u8,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            data_dir: PathBuf::from("./data"),
            cache_dir: PathBuf::new(),
            transcode_dir: PathBuf::new(),
            scan_prune_percent: DEFAULT_SCAN_PRUNE_PERCENT,
        }
    }
}

/// Configuration reserved for the embedded cluster backend.
///
/// Unlike the surrounding config sections this intentionally tolerates
/// unknown keys. An M0 binary must be able to read a config written by a later
/// clustering release during rollback, while typos in the existing server and
/// storage sections must continue to fail closed.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClusterConfig {
    /// Raft replication listener. It is not part of the public HTTP API.
    pub raft_bind: SocketAddr,
    /// Authenticated node-to-node request listener.
    pub api_bind: SocketAddr,
    /// Reachable address advertised to peers; empty means derive it locally.
    pub advertise_host: String,
    /// Public plurxd base URL a joining node uses to redeem its one-time
    /// credential. Empty derives `http://<advertise_host>:<server port>`.
    pub join_url: String,
    /// Node-specific public plurxd base URL peers use for artwork recovery.
    /// Empty derives `http://<advertise_host>:<server port>` and deliberately
    /// does not inherit `join_url`, which may name a shared load balancer.
    pub artwork_url: String,
    /// Single-use join-token file; empty means bootstrap/reopen one voter.
    pub join_token_file: PathBuf,
    /// Required network boundary when inter-node transport is not using TLS.
    pub trusted_network: String,
    /// Node-local key that wraps durable credentials plurx must replay rather
    /// than verify — today only the Trakt bearer pair. Empty means
    /// `<data_dir>/credentials.key`.
    ///
    /// This lives in `[cluster]` rather than `[storage]` for the reason §3.2
    /// gives: the cluster section tolerates unknown keys, so an operator who
    /// sets it and then rolls back to an older binary still gets a config that
    /// loads. It applies to the single-node SQLite backend too — the key is
    /// what makes a replicated row safe to write, so it has to exist before
    /// replication is switched on, not with it.
    pub credential_key_file: PathBuf,
    /// Optional node-local mount point for a cache filesystem shared by
    /// multiple voters. A path is only a candidate; the daemon admits it
    /// after an authenticated two-way canary succeeds.
    pub shared_cache_dir: PathBuf,
    /// Operator-stable identity for the shared filesystem. Combined with the
    /// replicated cluster id so unrelated clusters cannot alias one mount.
    pub shared_cache_id: String,
    /// Opt-in/kill switch for lag-gated local catalogue reads. Keep identical
    /// on every voter during rollout and rollback.
    pub bounded_replica_reads: bool,
    /// Maximum quorum-commit to local-applied entry gap admitted for one
    /// bounded catalogue query.
    pub bounded_replica_max_lag_entries: u64,
    /// Local Hiqlite read-only connection pool. Four is the measured/default
    /// baseline; the bounded knob permits retained 4/8/16 comparison runs.
    pub read_pool_size: usize,
    /// Deadline for sending and installing one Raft snapshot segment. Hiqlite's
    /// zero non-final-segment timeout makes this the snapshot transfer deadline.
    pub install_snapshot_timeout_secs: u64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            raft_bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_RAFT_PORT)),
            api_bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_CLUSTER_API_PORT)),
            advertise_host: String::new(),
            join_url: String::new(),
            artwork_url: String::new(),
            join_token_file: PathBuf::new(),
            trusted_network: String::new(),
            credential_key_file: PathBuf::new(),
            shared_cache_dir: PathBuf::new(),
            shared_cache_id: String::new(),
            bounded_replica_reads: false,
            bounded_replica_max_lag_entries: DEFAULT_BOUNDED_REPLICA_MAX_LAG_ENTRIES,
            read_pool_size: 4,
            install_snapshot_timeout_secs: DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS,
        }
    }
}

impl ClusterConfig {
    /// Where this node's credential-wrapping key lives.
    pub fn credential_key_path(&self, data_dir: &Path) -> PathBuf {
        if self.credential_key_file.as_os_str().is_empty() {
            data_dir.join(crate::secrets::CREDENTIAL_KEY_FILENAME)
        } else {
            self.credential_key_file.clone()
        }
    }
}

impl Config {
    /// Load configuration.
    ///
    /// Precedence (lowest → highest): built-in defaults, TOML file, `PLURX_*`
    /// env vars. An explicitly given path must exist; the default locations
    /// (`./plurx.toml`, `/etc/plurx/plurx.toml`) are used only if present.
    pub fn load(explicit_path: Option<&Path>) -> Result<Config, ConfigError> {
        let mut config = match explicit_path {
            Some(path) => Self::from_file(path)?,
            None => match DEFAULT_CONFIG_PATHS
                .iter()
                .map(Path::new)
                .find(|p| p.is_file())
            {
                Some(path) => Self::from_file(path)?,
                None => Config::default(),
            },
        };
        config.apply_env()?;
        if config.storage.scan_prune_percent > 100 {
            return Err(ConfigError::Value {
                key: "storage.scan_prune_percent".to_owned(),
                message: "must be between 0 and 100".to_owned(),
            });
        }
        let shared_dir_set = !config.cluster.shared_cache_dir.as_os_str().is_empty();
        let shared_id_set = !config.cluster.shared_cache_id.is_empty();
        if shared_dir_set != shared_id_set {
            return Err(ConfigError::Value {
                key: "cluster.shared_cache_dir".to_owned(),
                message: "shared_cache_dir and shared_cache_id must be configured together"
                    .to_owned(),
            });
        }
        if shared_id_set
            && (config.cluster.shared_cache_id.len() > 64
                || !config
                    .cluster
                    .shared_cache_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
        {
            return Err(ConfigError::Value {
                key: "cluster.shared_cache_id".to_owned(),
                message: "must be 1-64 ASCII letters, digits, dots, dashes, or underscores"
                    .to_owned(),
            });
        }
        if config.cluster.bounded_replica_max_lag_entries > MAX_BOUNDED_REPLICA_MAX_LAG_ENTRIES {
            return Err(ConfigError::Value {
                key: "cluster.bounded_replica_max_lag_entries".to_owned(),
                message: format!(
                    "must be between 0 and {MAX_BOUNDED_REPLICA_MAX_LAG_ENTRIES} entries"
                ),
            });
        }
        if !(1..=16).contains(&config.cluster.read_pool_size) {
            return Err(ConfigError::Value {
                key: "cluster.read_pool_size".to_owned(),
                message: "must be between 1 and 16".to_owned(),
            });
        }
        if !(MIN_INSTALL_SNAPSHOT_TIMEOUT_SECS..=MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS)
            .contains(&config.cluster.install_snapshot_timeout_secs)
        {
            return Err(ConfigError::Value {
                key: "cluster.install_snapshot_timeout_secs".to_owned(),
                message: format!(
                    "must be between {MIN_INSTALL_SNAPSHOT_TIMEOUT_SECS} and \
                     {MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS} seconds"
                ),
            });
        }
        Ok(config)
    }

    fn from_file(path: &Path) -> Result<Config, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }

    fn apply_env(&mut self) -> Result<(), ConfigError> {
        if let Some(name) = env_var("PLURX_SERVER_NAME") {
            self.server.name = name;
        }
        if let Some(bind) = env_var("PLURX_BIND") {
            self.server.bind = bind.parse().map_err(|_| ConfigError::Env {
                var: "PLURX_BIND".to_owned(),
                message: format!("`{bind}` is not a socket address (e.g. 0.0.0.0:{DEFAULT_PORT})"),
            })?;
        }
        if let Some(dir) = env_var("PLURX_DATA_DIR") {
            self.storage.data_dir = PathBuf::from(dir);
        }
        if let Some(dir) = env_var("PLURX_CACHE_DIR") {
            self.storage.cache_dir = PathBuf::from(dir);
        }
        if let Some(dir) = env_var("PLURX_TRANSCODE_DIR") {
            self.storage.transcode_dir = PathBuf::from(dir);
        }
        if let Some(path) = env_var("PLURX_CREDENTIAL_KEY_FILE") {
            self.cluster.credential_key_file = PathBuf::from(path);
        }
        if let Some(path) = env_var("PLURX_SHARED_CACHE_DIR") {
            self.cluster.shared_cache_dir = PathBuf::from(path);
        }
        if let Some(id) = env_var("PLURX_SHARED_CACHE_ID") {
            self.cluster.shared_cache_id = id;
        }
        if let Some(value) = env_var("PLURX_CLUSTER_BOUNDED_REPLICA_READS") {
            self.cluster.bounded_replica_reads = value.parse().map_err(|_| ConfigError::Env {
                var: "PLURX_CLUSTER_BOUNDED_REPLICA_READS".to_owned(),
                message: "must be `true` or `false`".to_owned(),
            })?;
        }
        if let Some(value) = env_var("PLURX_CLUSTER_BOUNDED_REPLICA_MAX_LAG_ENTRIES") {
            self.cluster.bounded_replica_max_lag_entries =
                value.parse().map_err(|_| ConfigError::Env {
                    var: "PLURX_CLUSTER_BOUNDED_REPLICA_MAX_LAG_ENTRIES".to_owned(),
                    message: "must be an integer from 0 through 10000".to_owned(),
                })?;
        }
        if let Some(value) = env_var("PLURX_SCAN_PRUNE_PERCENT") {
            self.storage.scan_prune_percent = value.parse().map_err(|_| ConfigError::Env {
                var: "PLURX_SCAN_PRUNE_PERCENT".to_owned(),
                message: format!("`{value}` is not an integer from 0 through 100"),
            })?;
        }
        if let Some(value) = env_var("PLURX_CLUSTER_READ_POOL_SIZE") {
            self.cluster.read_pool_size = value.parse().map_err(|_| ConfigError::Env {
                var: "PLURX_CLUSTER_READ_POOL_SIZE".to_owned(),
                message: format!("`{value}` is not an integer from 1 through 16"),
            })?;
        }
        apply_install_snapshot_timeout_env(
            &mut self.cluster,
            env_var("PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS"),
        )?;
        Ok(())
    }
}

fn apply_install_snapshot_timeout_env(
    cluster: &mut ClusterConfig,
    value: Option<String>,
) -> Result<(), ConfigError> {
    if let Some(value) = value {
        cluster.install_snapshot_timeout_secs = value.parse().map_err(|_| ConfigError::Env {
            var: "PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS".to_owned(),
            message: format!(
                "`{value}` is not an integer from {MIN_INSTALL_SNAPSHOT_TIMEOUT_SECS} through \
                 {MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS}"
            ),
        })?;
    }
    Ok(())
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let config = Config::default();
        assert_eq!(config.server.bind.port(), DEFAULT_PORT);
        assert_eq!(config.server.name, "plurx");
        assert_eq!(config.storage.data_dir, PathBuf::from("./data"));
        assert!(config.storage.cache_dir.as_os_str().is_empty());
        assert!(config.storage.transcode_dir.as_os_str().is_empty());
        assert_eq!(
            config.storage.scan_prune_percent,
            DEFAULT_SCAN_PRUNE_PERCENT
        );
        assert_eq!(config.cluster.raft_bind.port(), DEFAULT_RAFT_PORT);
        assert_eq!(config.cluster.api_bind.port(), DEFAULT_CLUSTER_API_PORT);
        assert!(config.cluster.join_url.is_empty());
        assert!(config.cluster.artwork_url.is_empty());
        assert!(!config.cluster.bounded_replica_reads);
        assert_eq!(
            config.cluster.bounded_replica_max_lag_entries,
            DEFAULT_BOUNDED_REPLICA_MAX_LAG_ENTRIES
        );
        assert_eq!(config.cluster.read_pool_size, 4);
        assert_eq!(
            config.cluster.install_snapshot_timeout_secs,
            DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS
        );
    }

    #[test]
    fn file_overrides_defaults_and_rejects_unknown_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");

        std::fs::write(
            &path,
            "[server]\nname = \"den\"\nbind = \"127.0.0.1:9999\"\n",
        )
        .expect("write config");
        let config = Config::load(Some(&path)).expect("load");
        assert_eq!(config.server.name, "den");
        assert_eq!(config.server.bind.port(), 9999);
        // Unspecified sections keep defaults.
        assert_eq!(config.storage.data_dir, PathBuf::from("./data"));

        std::fs::write(&path, "[server]\nnmae = \"typo\"\n").expect("write config");
        assert!(matches!(
            Config::load(Some(&path)),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn cluster_section_tolerates_future_keys_without_weakening_existing_sections() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");

        std::fs::write(
            &path,
            "[cluster]\nraft_bind = \"127.0.0.1:42401\"\nsome_future_key = 1\n",
        )
        .expect("write future cluster config");
        let config = Config::load(Some(&path)).expect("future cluster key is tolerated");
        assert_eq!(config.cluster.raft_bind.port(), 42401);

        std::fs::write(
            &path,
            "[cluster]\nsome_future_key = 1\n[server]\nnmae = \"typo\"\n",
        )
        .expect("write strict server config");
        assert!(matches!(
            Config::load(Some(&path)),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn explicit_missing_path_errors() {
        assert!(matches!(
            Config::load(Some(Path::new("/nonexistent/plurx.toml"))),
            Err(ConfigError::Read { .. })
        ));
    }

    #[test]
    fn scan_prune_percentage_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");
        std::fs::write(&path, "[storage]\nscan_prune_percent = 101\n").expect("write config");
        assert!(matches!(
            Config::load(Some(&path)),
            Err(ConfigError::Value { key, .. }) if key == "storage.scan_prune_percent"
        ));
    }

    #[test]
    fn shared_cache_mount_and_identity_are_paired_and_rollback_safe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");
        std::fs::write(
            &path,
            "[cluster]\nshared_cache_dir = \"/srv/plurx-shared\"\nshared_cache_id = \"media-a\"\nfuture_shared_cache_knob = true\n",
        )
        .expect("write shared config");
        let config = Config::load(Some(&path)).expect("paired shared cache config");
        assert_eq!(
            config.cluster.shared_cache_dir,
            PathBuf::from("/srv/plurx-shared")
        );
        assert_eq!(config.cluster.shared_cache_id, "media-a");

        std::fs::write(
            &path,
            "[cluster]\nshared_cache_dir = \"/srv/plurx-shared\"\n",
        )
        .expect("write unpaired path");
        assert!(matches!(
            Config::load(Some(&path)),
            Err(ConfigError::Value { key, .. }) if key == "cluster.shared_cache_dir"
        ));

        std::fs::write(
            &path,
            "[cluster]\nshared_cache_dir = \"/srv/plurx-shared\"\nshared_cache_id = \"unsafe/id\"\n",
        )
        .expect("write invalid identity");
        assert!(matches!(
            Config::load(Some(&path)),
            Err(ConfigError::Value { key, .. }) if key == "cluster.shared_cache_id"
        ));
    }

    #[test]
    fn bounded_replica_lag_budget_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");
        std::fs::write(
            &path,
            "[cluster]\nbounded_replica_max_lag_entries = 10001\n",
        )
        .expect("write config");
        assert!(matches!(
            Config::load(Some(&path)),
            Err(ConfigError::Value { key, .. })
                if key == "cluster.bounded_replica_max_lag_entries"
        ));
    }

    #[test]
    fn storage_roots_and_read_pool_load_with_compatibility_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");
        for size in [1, 4, 8, 16] {
            std::fs::write(
                &path,
                format!(
                    "[storage]\ncache_dir = \"/cache\"\ntranscode_dir = \"/scratch\"\n\
                     [cluster]\nread_pool_size = {size}\n"
                ),
            )
            .expect("write split storage config");
            let config = Config::load(Some(&path)).expect("load split storage config");
            assert_eq!(config.storage.cache_dir, PathBuf::from("/cache"));
            assert_eq!(config.storage.transcode_dir, PathBuf::from("/scratch"));
            assert_eq!(config.cluster.read_pool_size, size);
        }

        for size in [0, 17] {
            std::fs::write(&path, format!("[cluster]\nread_pool_size = {size}\n"))
                .expect("write invalid read pool");
            assert!(matches!(
                Config::load(Some(&path)),
                Err(ConfigError::Value { key, .. }) if key == "cluster.read_pool_size"
            ));
        }
    }

    #[test]
    fn snapshot_install_timeout_is_bounded_and_env_values_are_parsed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("plurx.toml");

        for seconds in [
            MIN_INSTALL_SNAPSHOT_TIMEOUT_SECS,
            DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS,
            MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS,
        ] {
            std::fs::write(
                &path,
                format!("[cluster]\ninstall_snapshot_timeout_secs = {seconds}\n"),
            )
            .expect("write valid snapshot timeout");
            let config = Config::load(Some(&path)).expect("load valid snapshot timeout");
            assert_eq!(config.cluster.install_snapshot_timeout_secs, seconds);
        }

        for seconds in [
            MIN_INSTALL_SNAPSHOT_TIMEOUT_SECS - 1,
            MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS + 1,
        ] {
            std::fs::write(
                &path,
                format!("[cluster]\ninstall_snapshot_timeout_secs = {seconds}\n"),
            )
            .expect("write invalid snapshot timeout");
            assert!(matches!(
                Config::load(Some(&path)),
                Err(ConfigError::Value { key, .. })
                    if key == "cluster.install_snapshot_timeout_secs"
            ));
        }

        let mut cluster = ClusterConfig::default();
        apply_install_snapshot_timeout_env(&mut cluster, Some("300".to_owned()))
            .expect("parse environment override");
        assert_eq!(cluster.install_snapshot_timeout_secs, 300);
        assert!(matches!(
            apply_install_snapshot_timeout_env(&mut cluster, Some("fast".to_owned())),
            Err(ConfigError::Env { var, .. })
                if var == "PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS"
        ));
    }
}
