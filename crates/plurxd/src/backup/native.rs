//! Native backup adapter; the shared scheduler knows no database client.
use super::{BackupArtifact, BackupProvider};
use plurx_core::error::StoreError;
use plurx_core::storage::LocalIdentity;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) struct NativeBackupProvider {
    data_dir: PathBuf,
    credential_key_path: PathBuf,
    identity: LocalIdentity,
    credential_key_id: String,
}
impl NativeBackupProvider {
    pub(crate) fn new(
        data_dir: PathBuf,
        credential_key_path: PathBuf,
        identity: LocalIdentity,
        credential_key_id: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            credential_key_path,
            identity,
            credential_key_id,
        })
    }
}
#[async_trait::async_trait]
impl BackupProvider for NativeBackupProvider {
    fn lease_resource(&self) -> &'static str {
        "backup:native"
    }
    fn artifact_prefix(&self) -> &'static str {
        "plurx-native-backup-"
    }
    async fn build(&self, destination: &Path) -> Result<BackupArtifact, StoreError> {
        if self.data_dir.as_os_str().is_empty() {
            return Err(StoreError::Task(
                "native backup requires an owned data root".to_owned(),
            ));
        }
        let source = self
            .data_dir
            .join(plurx_core::storage::native::DATABASE_FILENAME);
        let destination = destination.to_path_buf();
        let key_path = self.credential_key_path.clone();
        let identity = self.identity.clone();
        let key_id = self.credential_key_id.clone();
        let (path, manifest) = tokio::task::spawn_blocking(move || {
            plurx_core::storage::backup::build_native_backup_artifact(
                &source,
                &destination,
                &identity,
                &key_path,
                &key_id,
                crate::version::LONG,
            )
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining native backup builder: {error}")))??;
        let image_bytes = manifest.image_bytes;
        let manifest = serde_json::to_value(manifest).map_err(|error| {
            StoreError::Task(format!("encoding native backup response: {error}"))
        })?;
        Ok(BackupArtifact {
            path,
            manifest,
            image_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_provider_builds_a_native_archive_without_a_cluster_client() {
        let root = tempfile::tempdir().expect("native root");
        let destination = tempfile::tempdir().expect("backup destination");
        let key = root.path().join("credentials.key");
        let handle = plurx_core::storage::open_native_store_at(root.path(), &key)
            .await
            .expect("native boot");
        let provider = NativeBackupProvider::new(
            root.path().to_path_buf(),
            key,
            handle.identity.clone(),
            handle.credential_key.id().to_owned(),
        );
        let artifact = provider
            .build(destination.path())
            .await
            .expect("native backup");
        assert_eq!(artifact.manifest["storage_backend"], "sqlite");
        assert!(artifact.manifest.get("raft_cut").is_none());
        assert!(artifact.path.join("manifest.json").is_file());
    }
}
