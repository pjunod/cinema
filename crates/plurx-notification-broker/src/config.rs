use crate::{wire, Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub generation: String,
    pub master_key_file: PathBuf,
    pub publishers: Vec<Publisher>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub publisher_id: String,
    pub server_instance_id: String,
    pub proof_hash: String,
    pub apple: Option<Apple>,
    pub android: Option<Android>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apple {
    pub team_id: String,
    pub key_id: String,
    pub topic: String,
    pub environment: AppleEnvironment,
    pub private_key_file: PathBuf,
    #[serde(skip)]
    pub key_der: zeroize::Zeroizing<Vec<u8>>,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppleEnvironment {
    Production,
    Sandbox,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Android {
    pub project_id: String,
    pub service_account_email: String,
    pub private_key_file: PathBuf,
    #[serde(skip)]
    pub key_der: zeroize::Zeroizing<Vec<u8>>,
}
impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = read(path, 128 * 1024)?;
        let mut value: Self = serde_json::from_slice(&bytes).map_err(|_| Error::invalid())?;
        value.validate()?;
        for publisher in &mut value.publishers {
            if let Some(apple) = publisher.apple.as_mut() {
                apple.key_der = read_secret(&apple.private_key_file, 16 * 1024)?;
            }
            if let Some(android) = publisher.android.as_mut() {
                android.key_der = read_secret(&android.private_key_file, 16 * 1024)?;
            }
        }
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != "cinema.broker.operator.v1" || self.publishers.len() > 64 {
            return Err(Error::invalid());
        }
        wire::uuid(&self.generation)?;
        let mut seen = HashSet::new();
        for publisher in &self.publishers {
            wire::uuid(&publisher.publisher_id)?;
            wire::label(&publisher.server_instance_id, 128)?;
            wire::digest(&publisher.proof_hash)?;
            if !seen.insert(&publisher.publisher_id) {
                return Err(Error::invalid());
            }
            if let Some(apple) = &publisher.apple {
                for value in [&apple.key_id, &apple.team_id] {
                    if value.len() != 10
                        || !value
                            .bytes()
                            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                    {
                        return Err(Error::invalid());
                    }
                }
                wire::label(&apple.topic, 256)?;
                if !apple
                    .topic
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
                {
                    return Err(Error::invalid());
                }
            }
            if let Some(android) = &publisher.android {
                wire::label(&android.project_id, 128)?;
                wire::label(&android.service_account_email, 254)?;
                if !android
                    .project_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    || !android.service_account_email.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'-' | b'_')
                    })
                {
                    return Err(Error::invalid());
                }
            }
        }
        Ok(())
    }
}
pub fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|_| Error::unavailable())?;
    if !file.metadata().map_err(|_| Error::unavailable())?.is_file() {
        return Err(Error::unavailable());
    }
    let mut bytes = Vec::new();
    file.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::unavailable())?;
    if bytes.len() > max {
        return Err(Error::invalid());
    }
    Ok(bytes)
}

pub fn read_secret(path: &Path, max: usize) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let file = std::fs::File::open(path).map_err(|_| Error::unavailable())?;
    let metadata = file.metadata().map_err(|_| Error::unavailable())?;
    if !metadata.is_file() {
        return Err(Error::unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::unavailable());
        }
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::unavailable())?;
    if bytes.len() > max {
        return Err(Error::invalid());
    }
    Ok(bytes)
}

impl Publisher {
    pub fn record(&self) -> crate::store::PublisherRecord {
        crate::store::PublisherRecord {
            credential: hex::encode(Sha256::digest(
                format!(
                    "{}:{}",
                    self.apple
                        .as_ref()
                        .map(|value| format!(
                            "{}:{}",
                            value.key_id,
                            hex::encode(Sha256::digest(value.key_der.as_slice()))
                        ))
                        .unwrap_or_default(),
                    self.android
                        .as_ref()
                        .map(|value| format!(
                            "{}:{}",
                            value.service_account_email,
                            hex::encode(Sha256::digest(value.key_der.as_slice()))
                        ))
                        .unwrap_or_default()
                )
                .as_bytes(),
            )),
            id: self.publisher_id.clone(),
            proof_hash: self.proof_hash.clone(),
            server: self.server_instance_id.clone(),
            apple: self.apple.as_ref().map(|value| {
                format!(
                    "{}:{}:{}",
                    value.team_id,
                    value.topic,
                    match value.environment {
                        AppleEnvironment::Production => "production",
                        AppleEnvironment::Sandbox => "sandbox",
                    }
                )
            }),
            android: self.android.as_ref().map(|value| value.project_id.clone()),
        }
    }
}
/// Stable schema realm. Mutable operator publishers and signing credentials are
/// reconciled separately, rather than forcing all users through a restore.
pub const STORAGE_REALM: &str = "cinema.broker.storage.v1";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_manifest_key_and_unknown_operator_fields_refuse(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("missing.json");
        assert!(Manifest::load(&path).is_err());
        assert!(read_secret(&path, 32).is_err());
        std::fs::write(&path,br#"{"version":"cinema.broker.operator.v1","generation":"a640bd62-1a69-4f06-8809-b725310bfad5","master_key_file":"missing","publishers":[],"unknown":true}"#)?;
        assert!(Manifest::load(&path).is_err());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn secret_permission_and_size_are_checked_on_open_descriptor(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("synthetic.key");
        std::fs::write(&path, [7u8; 33])?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        assert!(read_secret(&path, 64).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        assert!(read_secret(&path, 32).is_err());
        assert_eq!(read_secret(&path, 33)?.len(), 33);
        Ok(())
    }
}
