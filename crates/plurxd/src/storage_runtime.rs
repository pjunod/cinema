//! Daemon composition of native storage and the optional cluster adapter.
//!
//! Native storage, identity, backup and handoff code live in core::storage and
//! contain no consensus implementation. This is the only startup branch that
//! chooses to construct the clustered runtime.

#[cfg(feature = "cluster")]
use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "cluster")]
use plurx_core::cluster::membership::MembershipManager;
#[cfg(feature = "cluster")]
use plurx_core::cluster::migration::status::ReplicationMonitor;
use plurx_core::config::Config;
use plurx_core::error::StoreError;
use plurx_core::secrets::CredentialKey;
use plurx_core::sharing_source_sessions::LocalSourceAuthority;
use plurx_core::storage::mode::{StorageManifest, StorageMode};
use plurx_core::storage::{LocalIdentity, NativeDirectoryLock, NativeStoreHandle};
use plurx_core::store::Store;

pub(crate) struct ServiceAdapters {
    #[cfg(feature = "cluster")]
    pub replication: Option<ReplicationMonitor>,
    #[cfg(feature = "cluster")]
    pub membership: Option<MembershipManager>,
    pub settings_changes: crate::settings_changes::SettingsChangeSource,
    pub backup: Arc<dyn crate::backup::BackupProvider>,
}

pub(crate) fn local_services(native: &NativeStoreHandle, config: &Config) -> ServiceAdapters {
    ServiceAdapters {
        #[cfg(feature = "cluster")]
        replication: None,
        #[cfg(feature = "cluster")]
        membership: None,
        settings_changes: crate::settings_changes::SettingsChangeSource::LocalOnly,
        backup: crate::backup::native::NativeBackupProvider::for_store(
            Arc::clone(&native.sqlite),
            native.data_dir.clone(),
            config.credential_key_path(),
            native.identity.clone(),
            native.credential_key.id().to_owned(),
        ),
    }
}

pub(crate) enum RuntimeStore {
    Native {
        handle: NativeStoreHandle,
        source: Option<LocalSourceAuthority>,
    },
    #[cfg(feature = "cluster")]
    Cluster(plurx_core::cluster::migration::PendingSelectedStore),
}

impl RuntimeStore {
    pub fn mode(&self) -> StorageMode {
        match self {
            Self::Native { .. } => StorageMode::Sqlite,
            #[cfg(feature = "cluster")]
            Self::Cluster(_) => StorageMode::Cluster,
        }
    }
    pub fn store(&self) -> Arc<dyn Store> {
        match self {
            Self::Native { handle, .. } => Arc::clone(&handle.store),
            #[cfg(feature = "cluster")]
            Self::Cluster(handle) => Arc::clone(&handle.store),
        }
    }
    pub fn identity(&self) -> LocalIdentity {
        match self {
            Self::Native { handle, .. } => handle.identity.clone(),
            #[cfg(feature = "cluster")]
            Self::Cluster(handle) => crate::cluster_runtime::local_identity(handle),
        }
    }
    pub fn credential_key(&self) -> Arc<CredentialKey> {
        match self {
            Self::Native { handle, .. } => Arc::clone(&handle.credential_key),
            #[cfg(feature = "cluster")]
            Self::Cluster(handle) => Arc::clone(&handle.credential_key),
        }
    }
    pub fn local_source(&self) -> Option<LocalSourceAuthority> {
        match self {
            Self::Native { source, .. } => source.clone(),
            #[cfg(feature = "cluster")]
            Self::Cluster(_) => None,
        }
    }
    pub fn services(&self, config: &Config) -> Result<ServiceAdapters, StoreError> {
        match self {
            Self::Native { handle, .. } => Ok(local_services(handle, config)),
            #[cfg(feature = "cluster")]
            Self::Cluster(handle) => crate::cluster_runtime::services(handle, config),
        }
    }
    pub async fn prepare_source_before_serving(&mut self) -> Result<bool, StoreError> {
        match self {
            Self::Native { handle, source } => {
                if source.is_some() {
                    return Err(StoreError::Task(
                        "native Source startup was already consumed".to_owned(),
                    ));
                }
                *source = Some(
                    handle
                        .sqlite
                        .prepare_native_source_before_serving(
                            &handle.identity.node_id,
                            &handle.credential_key,
                        )
                        .await?,
                );
                Ok(true)
            }
            #[cfg(feature = "cluster")]
            Self::Cluster(handle) => handle.prepare_source_schema_before_serving().await,
        }
    }
    pub async fn shutdown(&self) -> Result<(), StoreError> {
        match self {
            Self::Native { handle, source } => {
                if let Some(source) = source {
                    source.revoke();
                }
                handle.sqlite.close().await
            }
            #[cfg(feature = "cluster")]
            Self::Cluster(handle) => handle.shutdown().await,
        }
    }
}

#[cfg(feature = "cluster")]
pub(crate) async fn select(
    config: &Config,
    observer: Option<&dyn plurx_core::cluster::migration::StartupClockObserver>,
) -> Result<RuntimeStore, StoreError> {
    let root = &config.storage.data_dir;
    let lock = NativeDirectoryLock::acquire(root).await?;
    let lock = crate::storage_helper::settle(config, lock).await?;
    // Recovery owns all filesystem publication before either backend opens.
    plurx_core::storage::transition::recover_native_handoff(root)?;
    if let Some(manifest) = StorageManifest::read(root)? {
        match manifest.mode {
            StorageMode::Sqlite => {
                manifest.validate_sqlite(root)?;
                if crate::cluster_runtime::legacy_cluster_was_requested(config)? {
                    return Err(StoreError::Migration("native storage is already authoritative; use the managed cluster-create operation instead of changing its mode preference".to_owned()));
                }
                return native(config, lock).await;
            }
            StorageMode::Cluster => {
                crate::cluster_runtime::validate_manifest(config, &manifest)?;
                if config.storage.mode == Some(StorageMode::Sqlite) {
                    return Err(StoreError::Migration("a committed cluster cannot silently downgrade; export/decommission it explicitly".to_owned()));
                }
                let selected = crate::cluster_runtime::PendingSelection::new(
                    crate::cluster_runtime::open(config, observer, lock).await?,
                );
                if selected.identity.cluster_id != manifest.instance_id
                    || selected.identity.node_id != manifest.node_id
                {
                    return Err(selected
                        .refuse(StoreError::Migration(
                            "cluster manifest identity does not match the active store".to_owned(),
                        ))
                        .await);
                }
                return Ok(RuntimeStore::Cluster(selected));
            }
        }
    }
    let explicit_cluster = crate::cluster_runtime::legacy_cluster_was_requested(config)?;
    if plurx_core::storage::mode::has_native_selection_evidence(root)? {
        if explicit_cluster || crate::cluster_runtime::active_database(root).exists() {
            return Err(StoreError::Migration("native selection evidence has no committed manifest; restore or complete its native authority record before managed cluster creation".to_owned()));
        }
        return native(config, lock).await;
    }
    if explicit_cluster {
        if config.storage.mode == Some(StorageMode::Sqlite) {
            return Err(StoreError::Migration("existing explicit cluster admission requires decommission, not a standalone preference".to_owned()));
        }
        let selected = crate::cluster_runtime::PendingSelection::new(
            crate::cluster_runtime::open(config, observer, lock).await?,
        );
        let manifest = match committed_cluster_manifest(
            config,
            &selected.identity.cluster_id,
            &selected.identity.node_id,
        ) {
            Ok(manifest) => manifest,
            Err(error) => return Err(selected.refuse(error).await),
        };
        if let Err(error) = manifest.publish(root) {
            return Err(selected.refuse(error).await);
        }
        return Ok(RuntimeStore::Cluster(selected));
    }
    if crate::cluster_runtime::active_database(root).exists() {
        // Old releases silently bootstrapped a private one-voter store. It is
        // opened only long enough to settle/export its actual authoritative cut.
        let selected = crate::cluster_runtime::PendingSelection::new(
            crate::cluster_runtime::open(config, observer, lock).await?,
        );
        return convert_private(config, selected).await;
    }
    native(config, lock).await
}

/// Standalone selection never opens the old SDK or accepts a cluster preference.
#[cfg(not(feature = "cluster"))]
pub(crate) async fn select(config: &Config) -> Result<RuntimeStore, StoreError> {
    let root = &config.storage.data_dir;
    let lock = NativeDirectoryLock::acquire(root).await?;
    let lock = crate::storage_helper::settle(config, lock).await?;
    if config.storage.mode == Some(StorageMode::Cluster)
        || !config.cluster.join_token_file.as_os_str().is_empty()
        || !config.cluster.advertise_host.trim().is_empty()
    {
        return Err(StoreError::Migration(
            "explicit cluster configuration requires the cluster distribution".into(),
        ));
    }
    // A prepared native publication is backend-neutral and is recovered before
    // validating its old path. Unknown/interrupted operations fail closed.
    plurx_core::storage::transition::recover_native_handoff(root)?;
    if let Some(manifest) = StorageManifest::read(root)? {
        if manifest.mode != StorageMode::Sqlite {
            return Err(StoreError::Migration("cluster storage requires the cluster distribution or its version-matched migration helper".into()));
        }
        manifest.validate_sqlite(root)?;
        return native(config, lock).await;
    }
    for name in [
        "hiqlite",
        "hiqlite.incoming",
        "hiqlite-activated.json",
        "membership.json",
    ] {
        match std::fs::symlink_metadata(root.join(name)) {
            Ok(_) => return Err(StoreError::Migration("legacy or clustered storage requires the version-matched migration helper; retained SQLite is not current authority".into())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(StoreError::Migration(format!("inspecting storage authority: {error}"))),
        }
    }
    native(config, lock).await
}

/// Source preparation happens after selection. Bind only the marker's already
/// committed schema so a crash in between can reopen the predecessor layout.
#[cfg(feature = "cluster")]
fn committed_cluster_manifest(
    config: &Config,
    instance_id: &str,
    node_id: &str,
) -> Result<StorageManifest, StoreError> {
    let marker_path = plurx_core::storage::mode::owned_path(
        &config.storage.data_dir,
        Path::new("hiqlite/activation.json"),
    )?;
    let marker: plurx_core::cluster::migration::ActivationMarker =
        plurx_core::storage::mode::read_record(&marker_path)?.ok_or_else(|| {
            StoreError::Migration("opened cluster lost its committed activation marker".to_owned())
        })?;
    if marker.cluster_id != instance_id
        || !(1..=plurx_core::store::AUTH_SCHEMA_VERSION).contains(&marker.replicated_schema_version)
    {
        return Err(StoreError::Migration(
            "opened cluster activation identity or committed schema changed".to_owned(),
        ));
    }
    let manifest = StorageManifest {
        version: 1,
        mode: StorageMode::Cluster,
        database: "hiqlite".into(),
        instance_id: instance_id.to_owned(),
        node_id: node_id.to_owned(),
        generation: uuid::Uuid::new_v4().to_string(),
        schema_version: marker.replicated_schema_version,
    };
    crate::cluster_runtime::validate_manifest(config, &manifest)?;
    Ok(manifest)
}

async fn native(config: &Config, lock: NativeDirectoryLock) -> Result<RuntimeStore, StoreError> {
    let handle = plurx_core::storage::open_native_store_locked(config, lock).await?;
    tracing::info!(
        storage_backend = "sqlite",
        consensus = false,
        "native standalone storage selected"
    );
    Ok(RuntimeStore::Native {
        handle,
        source: None,
    })
}

#[cfg(feature = "cluster")]
async fn convert_private(
    config: &Config,
    selected: crate::cluster_runtime::PendingSelection,
) -> Result<RuntimeStore, StoreError> {
    let handle = plurx_core::storage::helper::convert_private_selected(config, selected).await?;
    Ok(RuntimeStore::Native {
        handle,
        source: None,
    })
}

#[cfg(test)]
mod tests;
