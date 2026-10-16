//! Node-local public HTTPS intent. No value in this module enters the Store.
use super::Config;
use crate::error::ConfigError;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    net::IpAddr,
    path::{Path, PathBuf},
};

pub const DEFAULT_HTTPS_PORT: u16 = 32443;
pub const HTTPS_SETTINGS_FILE: &str = "https-settings.json";
pub const HTTPS_SETTINGS_LIMIT: u64 = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HttpsConfig {
    pub enabled: bool,
    pub port: u16,
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    pub sans: Vec<String>,
}
impl Default for HttpsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_HTTPS_PORT,
            cert_file: PathBuf::new(),
            key_file: PathBuf::new(),
            sans: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
        }
    }
}
fn invalid(message: &str) -> ConfigError {
    ConfigError::Value {
        key: "server.https".into(),
        message: message.into(),
    }
}
impl HttpsConfig {
    pub fn generated(&self) -> bool {
        self.cert_file.as_os_str().is_empty()
    }

    /// Normalize representation without rejecting values repaired by an env.
    /// Validation belongs to the effective configuration after all overrides.
    pub fn normalize_saved(mut self, bind: IpAddr) -> Self {
        for san in &mut self.sans {
            *san = san.trim().to_ascii_lowercase();
            if let Ok(ip) = san.parse::<IpAddr>() {
                *san = ip.to_string();
            }
        }
        if self.generated() {
            self.sans
                .extend(["localhost".into(), "127.0.0.1".into(), "::1".into()]);
            if !bind.is_unspecified() {
                self.sans.push(bind.to_string());
            }
        }
        self.sans.sort();
        self.sans.dedup();
        self
    }

    /// Normalize both saved and effective choices. DNS comparisons ignore case.
    pub fn normalize(mut self, bind: IpAddr) -> Result<Self, ConfigError> {
        self = self.normalize_saved(bind);
        if self.port == 0 {
            return Err(invalid("port must be nonzero"));
        }
        if self.cert_file.as_os_str().is_empty() != self.key_file.as_os_str().is_empty() {
            return Err(invalid(
                "supply both absolute certificate/key paths, or leave both empty",
            ));
        }
        if !self.generated() && (!self.cert_file.is_absolute() || !self.key_file.is_absolute()) {
            return Err(invalid("certificate and key paths must be absolute"));
        }
        if self.sans.len() > 32 {
            return Err(invalid("at most 32 SANs are allowed"));
        }
        for san in &mut self.sans {
            *san = san.trim().to_ascii_lowercase();
            if san.is_empty() || san.len() > 253 {
                return Err(invalid("SANs must contain 1 through 253 bytes"));
            }
            if let Ok(ip) = san.parse::<IpAddr>() {
                if ip.is_unspecified() || ip.is_multicast() {
                    return Err(invalid("SAN IPs must be concrete unicast addresses"));
                }
                *san = ip.to_string();
            } else if !san.is_ascii()
                || san.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                return Err(invalid(
                    "SANs must be DNS names or IP addresses, without URLs, ports or wildcards",
                ));
            }
        }
        Ok(self)
    }
    pub fn validate_ports(&self, config: &Config) -> Result<(), ConfigError> {
        if self.enabled
            && [
                config.server.bind.port(),
                config.server.jellyfin_port,
                config.sharing.bind.port(),
                config.cluster.raft_bind.port(),
                config.cluster.api_bind.port(),
            ]
            .contains(&self.port)
        {
            return Err(invalid(
                "HTTPS port collides with an HTTP, Jellyfin, sharing or cluster port",
            ));
        }
        Ok(())
    }
}

/// Exact versioned on-disk CAS envelope. Missing files start at revision zero.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpsSettings {
    pub version: u32,
    pub revision: u64,
    pub settings: HttpsConfig,
}
/// Rename is the publication point. A later sync failure cannot be rolled back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpsPersistence {
    pub durability_confirmed: bool,
}
impl HttpsSettings {
    pub fn initial(settings: HttpsConfig) -> Self {
        Self {
            version: 1,
            revision: 0,
            settings,
        }
    }
    /// Durable replacement used under the daemon's serialized revision guard.
    pub fn persist(&self, data_dir: &Path) -> Result<HttpsPersistence, ConfigError> {
        self.persist_with_directory_sync(data_dir, |root| {
            #[cfg(unix)]
            {
                std::fs::File::open(root)?.sync_all()
            }
            #[cfg(not(unix))]
            {
                let _ = root;
                Ok(())
            }
        })
    }
    fn persist_with_directory_sync(
        &self,
        data_dir: &Path,
        sync_directory: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<HttpsPersistence, ConfigError> {
        use std::io::Write;
        if self.version != 1 {
            return Err(invalid("unsupported HTTPS settings version"));
        }
        let bytes =
            serde_json::to_vec(self).map_err(|_| invalid("cannot encode HTTPS settings"))?;
        if bytes.len() as u64 > HTTPS_SETTINGS_LIMIT {
            return Err(invalid("HTTPS settings exceed 16 KiB"));
        }
        let root = std::fs::canonicalize(data_dir)
            .map_err(|_| invalid("cannot resolve HTTPS data directory"))?;
        let path = root.join(HTTPS_SETTINGS_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(invalid("HTTPS settings leaf must be a regular file"))
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(invalid("cannot inspect HTTPS settings destination"))
            }
            _ => {}
        }
        let mut file = tempfile::NamedTempFile::new_in(&root)
            .map_err(|_| invalid("cannot stage HTTPS settings"))?;
        #[cfg(windows)]
        crate::fs_secure::harden_private_path_blocking(file.path(), false)
            .map_err(|_| invalid("cannot protect HTTPS settings"))?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|_| invalid("cannot persist HTTPS settings"))?;
        file.persist(&path)
            .map_err(|_| invalid("cannot atomically replace HTTPS settings"))?;
        // The new revision is now authoritative. Report sync uncertainty as a
        // published outcome so the API updates memory/CAS before responding.
        Ok(HttpsPersistence {
            durability_confirmed: sync_directory(&root).is_ok(),
        })
    }

    pub fn read(data_dir: &Path, base: HttpsConfig) -> Result<Self, ConfigError> {
        // data_dir itself retains the repository's existing symlink semantics.
        // Resolve it once, then refuse symlink descendants and the settings leaf.
        let root = match std::fs::canonicalize(data_dir) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::initial(base))
            }
            Err(_) => return Err(invalid("cannot resolve HTTPS data directory")),
        };
        let path = root.join(HTTPS_SETTINGS_FILE);
        let file = match crate::fs_secure::open_read_nofollow_blocking(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::initial(base))
            }
            Err(_) => return Err(invalid("cannot safely open node-local https-settings.json")),
        };
        let metadata = file
            .metadata()
            .map_err(|_| invalid("cannot inspect https-settings.json"))?;
        if !metadata.is_file() || metadata.len() > HTTPS_SETTINGS_LIMIT {
            return Err(invalid(
                "https-settings.json must be a regular file no larger than 16 KiB",
            ));
        }
        let mut bytes = Vec::new();
        file.take(HTTPS_SETTINGS_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("cannot read https-settings.json"))?;
        if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > HTTPS_SETTINGS_LIMIT {
            return Err(invalid(
                "https-settings.json changed or exceeded its read limit",
            ));
        }
        let settings: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("malformed https-settings.json envelope"))?;
        if settings.version != 1 {
            return Err(invalid("unsupported https-settings.json version"));
        }
        Ok(settings)
    }
}

/// Captured at boot. Only field names, never environment values, are exposed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpsOverrides(std::collections::BTreeMap<String, String>);
impl HttpsOverrides {
    pub fn capture(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let mut values = std::collections::BTreeMap::new();
        for (field, name) in [
            ("enabled", "PLURX_HTTPS_ENABLED"),
            ("port", "PLURX_HTTPS_PORT"),
            ("cert_file", "PLURX_HTTPS_CERT_FILE"),
            ("key_file", "PLURX_HTTPS_KEY_FILE"),
            ("sans", "PLURX_HTTPS_SANS"),
        ] {
            if let Some(value) = lookup(name) {
                values.insert(field.into(), value);
            }
        }
        Self(values)
    }
    pub fn fields(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
    pub fn apply(&self, mut config: HttpsConfig, bind: IpAddr) -> Result<HttpsConfig, ConfigError> {
        for (field, value) in &self.0 {
            match field.as_str() {
                "enabled" => {
                    config.enabled = value
                        .parse()
                        .map_err(|_| invalid("PLURX_HTTPS_ENABLED must be true or false"))?
                }
                "port" => {
                    config.port = value
                        .parse()
                        .map_err(|_| invalid("PLURX_HTTPS_PORT must be a nonzero port"))?
                }
                "cert_file" => config.cert_file = value.into(),
                "key_file" => config.key_file = value.into(),
                "sans" => {
                    config.sans = if value.is_empty() {
                        vec![]
                    } else {
                        value.split(',').map(str::to_owned).collect()
                    }
                }
                _ => unreachable!("closed HTTPS environment fields"),
            }
        }
        config.normalize(bind)
    }
    /// Reject edits to env-owned fields and preserve their pre-env choices.
    pub fn editable_save(
        &self,
        submitted: &HttpsConfig,
        effective: &HttpsConfig,
        saved: &HttpsConfig,
    ) -> Result<HttpsConfig, ConfigError> {
        let mut next = serde_json::to_value(submitted).expect("HTTPS JSON object");
        let current = serde_json::to_value(effective).expect("HTTPS JSON object");
        let previous = serde_json::to_value(saved).expect("HTTPS JSON object");
        for field in self.0.keys() {
            if next[field] != current[field] {
                return Err(invalid(&format!(
                    "{field} is controlled by an environment override"
                )));
            }
            next[field] = previous[field].clone();
        }
        serde_json::from_value(next).map_err(|_| invalid("invalid HTTPS choices"))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsLocalConfig {
    pub saved: HttpsSettings,
    pub overrides: HttpsOverrides,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bind() -> IpAddr {
        "0.0.0.0".parse().expect("synthetic HTTPS fixture")
    }
    #[test]
    fn https_defaults_and_san_validation_preserve_legacy_http() {
        let config = Config::default();
        assert!(!config.server.https.enabled);
        assert_eq!(config.server.bind.port(), 32400);
        assert_eq!(config.server.https.port, 32443);
        for san in [
            "https://media1",
            "media1:32443",
            "*.example.com",
            "0.0.0.0",
            "::",
            "-host",
            "bad..host",
            "évil",
        ] {
            assert!(
                HttpsConfig {
                    sans: vec![san.into()],
                    ..Default::default()
                }
                .normalize(bind())
                .is_err(),
                "{san}"
            );
        }
        let normalized = HttpsConfig {
            sans: vec!["MEDIA1.lan".into(), "2001:db8::1".into()],
            ..Default::default()
        }
        .normalize("10.42.1.5".parse().expect("synthetic HTTPS fixture"))
        .expect("synthetic HTTPS fixture");
        assert!(normalized.sans.contains(&"10.42.1.5".into()));
        assert!(normalized.sans.contains(&"media1.lan".into()));
        assert!(normalized.sans.contains(&"2001:db8::1".into()));
        assert!(!normalized.sans.contains(&"0.0.0.0".into()));
    }
    #[test]
    fn https_env_preserves_masked_saved_fields_and_explicit_empty_paths() {
        let saved = HttpsConfig {
            cert_file: "/cert.pem".into(),
            key_file: "/key.pem".into(),
            ..Default::default()
        }
        .normalize(bind())
        .expect("synthetic HTTPS fixture");
        let env = HttpsOverrides::capture(|name| match name {
            "PLURX_HTTPS_ENABLED" => Some("true".into()),
            "PLURX_HTTPS_CERT_FILE" | "PLURX_HTTPS_KEY_FILE" => Some(String::new()),
            _ => None,
        });
        let effective = env
            .apply(saved.clone(), bind())
            .expect("synthetic HTTPS fixture");
        assert!(effective.enabled && effective.generated());
        let mut submitted = effective.clone();
        submitted.port = 32445;
        let next = env
            .editable_save(&submitted, &effective, &saved)
            .expect("synthetic HTTPS fixture");
        assert_eq!(next.cert_file, saved.cert_file);
        assert!(!next.enabled);
        assert_eq!(next.port, 32445);
        submitted.enabled = false;
        assert!(env.editable_save(&submitted, &effective, &saved).is_err());
        assert!(HttpsOverrides::capture(
            |name| (name == "PLURX_HTTPS_ENABLED").then(|| "yes".into())
        )
        .apply(saved, bind())
        .is_err());
    }
    #[test]
    fn https_versioned_settings_preserve_revision_and_reject_malformed_intent() {
        let dir = tempfile::tempdir().expect("synthetic HTTPS fixture");
        let base = HttpsConfig::default();
        assert_eq!(
            HttpsSettings::read(dir.path(), base.clone())
                .expect("synthetic HTTPS fixture")
                .revision,
            0
        );
        let mut envelope = HttpsSettings::initial(base.clone());
        envelope.revision = 81;
        std::fs::write(
            dir.path().join(HTTPS_SETTINGS_FILE),
            serde_json::to_vec(&envelope).expect("synthetic HTTPS fixture"),
        )
        .expect("synthetic HTTPS fixture");
        assert_eq!(
            HttpsSettings::read(dir.path(), base.clone()).expect("synthetic HTTPS fixture"),
            envelope
        );
        for body in [
            r#"{"version":2,"revision":81,"settings":{}}"#,
            r#"{"version":1,"revision":81,"settings":{},"secret":"x"}"#,
            "broken",
        ] {
            std::fs::write(dir.path().join(HTTPS_SETTINGS_FILE), body)
                .expect("synthetic HTTPS fixture");
            assert!(HttpsSettings::read(dir.path(), base.clone()).is_err());
        }
    }
    #[test]
    fn https_paths_ports_and_collisions_are_explicit() {
        let mut config = Config::default();
        config.server.https.enabled = true;
        for port in [32400, 8096, 32444, 32401, 32402] {
            config.server.https.port = port;
            assert!(config.server.https.validate_ports(&config).is_err());
        }
        config.server.https.enabled = false;
        assert!(config.server.https.validate_ports(&config).is_ok());
        assert!(HttpsConfig {
            port: 0,
            ..Default::default()
        }
        .normalize(bind())
        .is_err());
        assert!(HttpsConfig {
            cert_file: "/cert.pem".into(),
            ..Default::default()
        }
        .normalize(bind())
        .is_err());
        assert!(HttpsConfig {
            cert_file: "cert.pem".into(),
            key_file: "key.pem".into(),
            ..Default::default()
        }
        .normalize(bind())
        .is_err());
    }
    #[test]
    fn https_validates_after_env_repairs_underlying_values() {
        let mut config: Config = toml::from_str("[server.https]\ncert_file='/cert.pem'\nport=0\n")
            .expect("synthetic HTTPS fixture");
        let dir = tempfile::tempdir().expect("synthetic HTTPS fixture");
        config.storage.data_dir = dir.path().into();
        let env = HttpsOverrides::capture(|name| match name {
            "PLURX_HTTPS_KEY_FILE" => Some("/key.pem".into()),
            "PLURX_HTTPS_PORT" => Some("32443".into()),
            _ => None,
        });
        config.resolve_https(env).expect("synthetic HTTPS fixture");
        assert_eq!(config.server.https.key_file, PathBuf::from("/key.pem"));
        let local = config.https_local.expect("synthetic HTTPS fixture");
        assert_eq!(local.saved.settings.port, 0);
        assert!(local.saved.settings.key_file.as_os_str().is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn legacy_symlink_data_directory_preserves_startup_and_refuses_settings_leaf_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("synthetic HTTPS fixture");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("synthetic HTTPS fixture");
        let link = dir.path().join("data");
        symlink(&real, &link).expect("synthetic HTTPS fixture");
        let mut config = Config::default();
        config.storage.data_dir = link.clone();
        config
            .resolve_https(HttpsOverrides::default())
            .expect("synthetic HTTPS fixture");
        assert!(!config.server.https.enabled);
        let envelope = HttpsSettings::initial(HttpsConfig::default());
        let target = dir.path().join("settings.json");
        std::fs::write(
            &target,
            serde_json::to_vec(&envelope).expect("synthetic HTTPS fixture"),
        )
        .expect("synthetic HTTPS fixture");
        symlink(target, real.join(HTTPS_SETTINGS_FILE)).expect("synthetic HTTPS fixture");
        assert!(HttpsSettings::read(&link, HttpsConfig::default()).is_err());
    }
    #[test]
    fn https_settings_atomic_save_preserves_revision_and_private_mode() {
        let dir = tempfile::tempdir().expect("synthetic HTTPS fixture");
        let mut envelope = HttpsSettings::initial(HttpsConfig::default());
        envelope.revision = 4;
        envelope
            .persist(dir.path())
            .expect("synthetic HTTPS fixture");
        assert_eq!(
            HttpsSettings::read(dir.path(), HttpsConfig::default())
                .expect("synthetic HTTPS fixture"),
            envelope
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join(HTTPS_SETTINGS_FILE))
                    .expect("synthetic HTTPS fixture")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        envelope.revision = 5;
        envelope
            .persist(dir.path())
            .expect("synthetic HTTPS fixture");
        assert_eq!(
            HttpsSettings::read(dir.path(), HttpsConfig::default())
                .expect("synthetic HTTPS fixture")
                .revision,
            5
        );
        assert!(envelope.persist(&dir.path().join("missing")).is_err());
    }
    #[test]
    fn https_post_publication_sync_failure_reports_committed_revision() {
        let dir = tempfile::tempdir().expect("synthetic HTTPS fixture");
        let mut envelope = HttpsSettings::initial(HttpsConfig::default());
        envelope.revision = 7;
        let outcome = envelope
            .persist_with_directory_sync(dir.path(), |_| {
                Err(std::io::Error::other("injected directory sync failure"))
            })
            .expect("synthetic HTTPS fixture");
        assert!(!outcome.durability_confirmed);
        assert_eq!(
            HttpsSettings::read(dir.path(), HttpsConfig::default())
                .expect("synthetic HTTPS fixture")
                .revision,
            7
        );
        envelope.revision = 8;
        assert!(
            envelope
                .persist(dir.path())
                .expect("synthetic HTTPS fixture")
                .durability_confirmed
        );
    }
}
