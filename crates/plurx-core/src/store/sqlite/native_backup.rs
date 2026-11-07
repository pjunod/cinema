//! Native online backups share the running Store's operation ownership.
use super::*;

impl SqliteStore {
    /// Capture this Store's database while retaining its real lifetime guard.
    /// The separate SQLite online-backup reader is drained by `close`, even
    /// after cancellation of the async backup waiter. A closed Store cannot
    /// reopen its old database path through the backup provider.
    pub async fn build_native_backup(
        &self,
        destination: PathBuf,
        identity: crate::storage::LocalIdentity,
        credential_key_path: PathBuf,
        credential_key_id: String,
        binary_version: String,
    ) -> Result<(PathBuf, crate::storage::backup::NativeBackupManifest), StoreError> {
        let source = self.path.clone().ok_or_else(|| {
            StoreError::Task("native backup requires a file-backed SQLite Store".into())
        })?;
        let operation = self.lifecycle.admit()?;
        let owner = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let _owner = owner;
            let _operation = operation;
            crate::storage::backup::build_native_backup_artifact(
                &source,
                &destination,
                &identity,
                &credential_key_path,
                &credential_key_id,
                &binary_version,
            )
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining native backup owner: {error}")))?
    }
}
