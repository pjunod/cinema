//! Native SQLite bootstrap. No consensus client, membership or vote is created.

use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::config::Config;
use crate::error::StoreError;
use crate::secrets::{self, CredentialKey};
use crate::store::{SettingsStore, SharingPurposeKeyStore, SharingStore, SqliteStore, Store};

use super::identity::{initialize_local_identity, LocalIdentity};
use super::mode::{StorageManifest, StorageMode};

pub const DATABASE_FILENAME: &str = "plurx.db";
pub const DIRECTORY_LOCK_FILENAME: &str = ".plurxd.lock";
const LOCK_WINDOW: Duration = Duration::from_secs(5);
const LOCK_RETRY: Duration = Duration::from_millis(50);

/// One directory owner, shared by handles belonging to that same runtime.
/// The lock name is also understood by existing clustered releases.
#[derive(Clone)]
pub struct NativeDirectoryLock {
    _file: Arc<File>,
    data_dir: PathBuf,
}

impl NativeDirectoryLock {
    pub async fn acquire(data_dir: &Path) -> Result<Self, StoreError> {
        let started = tokio::time::Instant::now();
        loop {
            match Self::try_acquire(data_dir) {
                Ok(lock) => return Ok(lock),
                Err(error) if is_lock_busy(&error) && started.elapsed() < LOCK_WINDOW => {
                    tokio::time::sleep(
                        LOCK_RETRY.min(LOCK_WINDOW.saturating_sub(started.elapsed())),
                    )
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn try_acquire(data_dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(data_dir)
            .map_err(|error| io("creating data directory", data_dir, error))?;
        let data_dir = std::fs::canonicalize(data_dir)
            .map_err(|error| io("resolving data directory", data_dir, error))?;
        let path = data_dir.join(DIRECTORY_LOCK_FILENAME);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let mut file = options
            .open(&path)
            .map_err(|error| io("opening lock", &path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| io("reading lock", &path, error))?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return Err(StoreError::Identity(
                    "storage lock refuses reparse points".to_owned(),
                ));
            }
        }
        if !metadata.is_file() {
            return Err(StoreError::Identity(
                "storage lock must be a regular file".to_owned(),
            ));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(StoreError::Identity(format!(
                    "native storage directory is busy: {}",
                    data_dir.display()
                )));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(io("locking", &path, error)),
        }
        // Diagnostic only; the held descriptor is the authority.
        if let Err(error) = file
            .set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| writeln!(file, "{}", std::process::id()))
            .and_then(|()| file.flush())
        {
            tracing::debug!(%error, "native storage lock holder diagnostic could not be recorded");
        }
        Ok(Self {
            _file: Arc::new(file),
            data_dir,
        })
    }

    /// Transfer the one owned descriptor to the existing cluster adapter.
    /// Refuse if another runtime handle still owns this directory lease.
    pub fn into_locked_file(self) -> Result<File, StoreError> {
        Arc::try_unwrap(self._file).map_err(|_| {
            StoreError::Identity("storage directory lock still has another live owner".to_owned())
        })
    }

    /// A startup cleanup owner retains the same underlying lock while a
    /// pending cluster client is cancelled and drained.
    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn duplicate_locked_file(&self) -> Result<File, StoreError> {
        self._file
            .try_clone()
            .map_err(|error| io("duplicating storage lock", &self.data_dir, error))
    }

    /// Adopt the descriptor retained by the previous backend after it has
    /// stopped all work. The lock stays held across this ownership transfer.
    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn from_locked_file(file: File, data_dir: PathBuf) -> Result<Self, StoreError> {
        let data_dir = std::fs::canonicalize(&data_dir)
            .map_err(|error| io("resolving locked directory", &data_dir, error))?;
        let descriptor = crate::fs_secure::std_file_identity(&file)
            .map_err(|error| io("reading lock descriptor", &data_dir, error))?;
        let path = crate::fs_secure::regular_file_identity_nofollow_blocking(
            &data_dir.join(DIRECTORY_LOCK_FILENAME),
        )
        .map_err(|error| io("reading lock path", &data_dir, error))?;
        if !descriptor.same_inode(path) {
            return Err(StoreError::Identity(
                "foreign storage lock descriptor".to_owned(),
            ));
        }
        Ok(Self {
            _file: Arc::new(file),
            data_dir,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

fn is_lock_busy(error: &StoreError) -> bool {
    matches!(error, StoreError::Identity(message) if message.starts_with("native storage directory is busy:"))
}

/// Native application services and their lifetime directory lease.
pub struct NativeStoreHandle {
    pub sqlite: Arc<SqliteStore>,
    pub store: Arc<dyn Store>,
    pub identity: LocalIdentity,
    pub credential_key: Arc<CredentialKey>,
    pub data_dir: PathBuf,
    pub directory_lock: NativeDirectoryLock,
}

pub(crate) struct NativeStoreParts {
    pub sqlite: Arc<SqliteStore>,
    pub store: Arc<dyn Store>,
    pub identity: LocalIdentity,
    pub credential_key: Arc<CredentialKey>,
}

/// Convenience adapter for the present configuration layout. The actual
/// native opener accepts explicit native paths, independent of cluster config.
pub async fn open_native_store(config: &Config) -> Result<NativeStoreHandle, StoreError> {
    open_native_store_at(&config.storage.data_dir, &config.credential_key_path()).await
}

pub async fn open_native_store_locked(
    config: &Config,
    directory_lock: NativeDirectoryLock,
) -> Result<NativeStoreHandle, StoreError> {
    open_native_store_at_locked(
        &config.storage.data_dir,
        &config.credential_key_path(),
        directory_lock,
    )
    .await
}

pub async fn open_native_store_at(
    data_dir: &Path,
    credential_key_path: &Path,
) -> Result<NativeStoreHandle, StoreError> {
    let directory_lock = NativeDirectoryLock::acquire(data_dir).await?;
    open_native_store_at_locked(data_dir, credential_key_path, directory_lock).await
}

pub async fn open_native_store_at_locked(
    data_dir: &Path,
    credential_key_path: &Path,
    directory_lock: NativeDirectoryLock,
) -> Result<NativeStoreHandle, StoreError> {
    let canonical = std::fs::canonicalize(data_dir)
        .map_err(|error| io("resolving data directory", data_dir, error))?;
    if canonical != directory_lock.data_dir {
        return Err(StoreError::Identity(
            "native store received a different directory's lock".to_owned(),
        ));
    }
    let data_dir = canonical.as_path();
    validate_native_authority(data_dir)?;
    let generation = super::mode::begin_native_adoption(data_dir)?;
    let parts = open_parts_at_retaining(
        data_dir,
        credential_key_path,
        Some(Arc::new(directory_lock.clone())),
    )
    .await?;
    super::mode::finish_native_adoption(data_dir, &parts.identity, &generation)?;
    Ok(NativeStoreHandle {
        sqlite: parts.sqlite,
        store: parts.store,
        identity: parts.identity,
        credential_key: parts.credential_key,
        data_dir: canonical,
        directory_lock,
    })
}

/// Refuse the retained pre-activation SQLite database. Only a manifest that
/// binds the current native database and its generation permits conversion.
fn validate_native_authority(data_dir: &Path) -> Result<(), StoreError> {
    match std::fs::symlink_metadata(data_dir.join(super::mode::TRANSITION_FILENAME)) {
        Ok(_) => {
            return Err(StoreError::Migration(
                "storage transition is incomplete; native startup cannot bypass it".to_owned(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io("inspecting storage transition", data_dir, error)),
    }
    match StorageManifest::read(data_dir)? {
        Some(manifest) if manifest.mode == StorageMode::Sqlite => {
            manifest.validate_sqlite(data_dir)?;
            let path = data_dir.join(super::identity::NODE_ID_FILENAME);
            let node = super::identity::read_node_id(&path)
                .map_err(|error| io("validating committed node identity", &path, error))?;
            if node != manifest.node_id {
                return Err(StoreError::Identity(
                    "committed native node identity does not match its manifest".to_owned(),
                ));
            }
            Ok(())
        }
        Some(_) => Err(StoreError::Migration(
            "this directory is owned by clustered storage".to_owned(),
        )),
        None => {
            for name in ["hiqlite", "hiqlite-activated.json"] {
                match std::fs::symlink_metadata(data_dir.join(name)) {
                    Ok(_) => return Err(StoreError::Migration(
                        "native storage refuses retained pre-activation data; perform the explicit verified conversion first".to_owned())),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io("inspecting storage authority", &data_dir.join(name), error)),
                }
            }
            Ok(())
        }
    }
}

/// Existing cluster import/recovery owns its lock and its authority checks.
/// This shares native opening without constructing any cluster identity.
pub(crate) async fn open_store_parts(config: &Config) -> Result<NativeStoreParts, StoreError> {
    open_parts_at(&config.storage.data_dir, &config.credential_key_path()).await
}

pub(crate) async fn open_parts_at(
    data_dir: &Path,
    credential_key_path: &Path,
) -> Result<NativeStoreParts, StoreError> {
    open_parts_at_retaining(data_dir, credential_key_path, None).await
}

async fn open_parts_at_retaining(
    data_dir: &Path,
    credential_key_path: &Path,
    lifetime_guard: Option<Arc<dyn Send + Sync>>,
) -> Result<NativeStoreParts, StoreError> {
    std::fs::create_dir_all(data_dir)
        .map_err(|error| io("creating data directory", data_dir, error))?;
    let sqlite = match lifetime_guard {
        Some(guard) => {
            SqliteStore::open_with_lifetime_guard(&data_dir.join(DATABASE_FILENAME), guard)?
        }
        None => SqliteStore::open(&data_dir.join(DATABASE_FILENAME))?,
    };
    let instance_id = sqlite.instance_id().await?;
    let identity = initialize_local_identity(data_dir, &instance_id)?;
    if identity.node_id != instance_id {
        let legacy_rows = sqlite.local_ownership_rows(&instance_id).await?;
        if legacy_rows > 0 {
            return Err(StoreError::Identity(format!(
                "node.id {} differs from instance.id {instance_id}, but {legacy_rows} cache or offline row(s) still use instance.id; refusing to strand owned bytes", identity.node_id)));
        }
        tracing::warn!(node_id = %identity.node_id, instance_id,
            "using a node-local identity distinct from instance.id");
    }
    let credential_key = open_credential_key_for(credential_key_path, &sqlite, &identity).await?;
    sqlite.migrate_trakt_credentials(&credential_key).await?;
    let sqlite = Arc::new(sqlite);
    Ok(NativeStoreParts {
        store: sqlite.clone(),
        sqlite,
        identity,
        credential_key: Arc::new(credential_key),
    })
}

/// Called only under the native directory owner's startup/census boundary.
/// Lost or mismatched keys fail before any replacement key can be minted.
pub(crate) async fn open_credential_key_for(
    path: &Path,
    sqlite: &SqliteStore,
    identity: &LocalIdentity,
) -> Result<CredentialKey, StoreError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .ok_or_else(|| {
            StoreError::Identity("native credential census clock is invalid".to_owned())
        })?;
    let claim =
        crate::store::sharing_purpose_keys::begin_native_census(sqlite, &identity.node_id, now_ms)
            .await?;
    sqlite.inspect_sharing_purpose_material().await?;
    let mut census = sqlite.sealed_trakt_row_census().await?;
    census.merge(sqlite.sharing_sealed_census().await?);
    let key = secrets::open_credential_key(path, &census)
        .map_err(|error| StoreError::Identity(error.to_string()))?;
    sqlite.verify_sharing_purpose_material(&key).await?;
    crate::store::sharing_purpose_keys::finish_census(sqlite, claim).await?;
    tracing::debug!(key_id = %key.id(), wrapped_rows = census.sealed_rows(),
        "opened the node-local credential key");
    Ok(key)
}

fn io(action: &str, path: &Path, error: impl std::fmt::Display) -> StoreError {
    StoreError::Identity(format!("{action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn directory_lock_refuses_symlinks_without_truncating_the_target() {
        let directory = tempfile::tempdir().expect("native directory");
        let target = directory.path().join("must-stay");
        std::fs::write(&target, b"preserve").expect("sentinel");
        std::os::unix::fs::symlink(&target, directory.path().join(DIRECTORY_LOCK_FILENAME))
            .expect("lock symlink");
        assert!(NativeDirectoryLock::try_acquire(directory.path()).is_err());
        assert_eq!(
            std::fs::read(&target).expect("preserved sentinel"),
            b"preserve"
        );
    }

    #[tokio::test]
    async fn native_store_restarts_with_the_same_identity_and_excludes_other_daemons() {
        let directory = tempfile::tempdir().expect("native directory");
        let key = directory.path().join("credentials.key");
        let first = open_native_store_at(directory.path(), &key)
            .await
            .expect("native boot");
        let identity = first.identity.clone();
        assert!(NativeDirectoryLock::try_acquire(directory.path()).is_err());
        assert!(!directory.path().join("hiqlite").exists());
        drop(first);
        let restarted = open_native_store_at(directory.path(), &key)
            .await
            .expect("native restart");
        assert_eq!(restarted.identity, identity);
    }

    #[tokio::test]
    async fn cloned_native_writers_retain_the_directory_lease_after_handle_drop() {
        let directory = tempfile::tempdir().expect("native directory");
        let handle =
            open_native_store_at(directory.path(), &directory.path().join("credentials.key"))
                .await
                .expect("native startup");
        let sqlite = Arc::clone(&handle.sqlite);
        let store = Arc::clone(&handle.store);
        drop(handle);
        assert!(NativeDirectoryLock::try_acquire(directory.path()).is_err());
        store
            .put_setting("lease.test", "retained writer")
            .await
            .expect("original writer remains owned");
        drop(store);
        assert!(
            NativeDirectoryLock::try_acquire(directory.path()).is_err(),
            "raw SQLite owner also pins authority"
        );
        drop(sqlite);
        let next = NativeDirectoryLock::try_acquire(directory.path())
            .expect("last writer drop releases ownership");
        drop(next);
    }

    #[tokio::test]
    async fn committed_native_identity_loss_or_mismatch_refuses_without_rewriting_data() {
        for missing in [true, false] {
            let root = tempfile::tempdir().expect("identity fixture");
            let key = root.path().join("credentials.key");
            let handle = open_native_store_at(root.path(), &key)
                .await
                .expect("native authority");
            drop(handle);
            let database =
                std::fs::read(root.path().join(DATABASE_FILENAME)).expect("committed image");
            let master = std::fs::read(&key).expect("master key");
            let node = root.path().join(super::super::identity::NODE_ID_FILENAME);
            if missing {
                std::fs::remove_file(&node).expect("missing node fixture");
            } else {
                std::fs::write(&node, uuid::Uuid::new_v4().to_string())
                    .expect("wrong node fixture");
            }
            assert!(open_native_store_at(root.path(), &key).await.is_err());
            assert_eq!(
                std::fs::read(root.path().join(DATABASE_FILENAME)).expect("unchanged image"),
                database
            );
            assert_eq!(std::fs::read(&key).expect("unchanged master"), master);
            assert_eq!(node.exists(), !missing);
        }
    }

    #[tokio::test]
    async fn retained_activation_source_is_refused_before_native_open() {
        let directory = tempfile::tempdir().expect("native directory");
        std::fs::write(directory.path().join("hiqlite-activated.json"), b"retained")
            .expect("marker");
        let result =
            open_native_store_at(directory.path(), &directory.path().join("credentials.key")).await;
        assert!(result.is_err());
        assert!(!directory.path().join(DATABASE_FILENAME).exists());
        assert!(!directory.path().join("credentials.key").exists());
    }

    #[tokio::test]
    async fn a_different_directory_lock_cannot_open_native_storage() {
        let first = tempfile::tempdir().expect("first directory");
        let second = tempfile::tempdir().expect("second directory");
        let lock = NativeDirectoryLock::acquire(first.path())
            .await
            .expect("lock");
        let result = open_native_store_at_locked(
            second.path(),
            &second.path().join("credentials.key"),
            lock,
        )
        .await;
        assert!(result.is_err());
        assert!(!second.path().join(DATABASE_FILENAME).exists());
    }
}
