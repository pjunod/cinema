//! Daemon composition of native storage and the optional cluster adapter.
//!
//! Native storage, identity, backup and handoff code live in core::storage and
//! contain no consensus implementation. This is the only startup branch that
//! chooses to construct the clustered runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::cluster::migration::status::ReplicationMonitor;
use plurx_core::config::Config;
use plurx_core::error::StoreError;
use plurx_core::secrets::CredentialKey;
use plurx_core::sharing_source_sessions::LocalSourceAuthority;
use plurx_core::storage::mode::{publish_record, StorageManifest, StorageMode};
use plurx_core::storage::{LocalIdentity, NativeDirectoryLock, NativeStoreHandle};
use plurx_core::store::Store;

pub(crate) struct ServiceAdapters {
    pub replication: ReplicationMonitor,
    pub membership: MembershipManager,
    pub settings_changes: crate::settings_changes::SettingsChangeSource,
    pub backup: Arc<dyn crate::backup::BackupProvider>,
}

pub(crate) fn local_services(native: &NativeStoreHandle, config: &Config) -> ServiceAdapters {
    ServiceAdapters {
        replication: ReplicationMonitor::sqlite(),
        membership: MembershipManager::unavailable(),
        settings_changes: crate::settings_changes::SettingsChangeSource::LocalOnly,
        backup: crate::backup::native::NativeBackupProvider::new(
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
    Cluster(plurx_core::cluster::migration::PendingSelectedStore),
}

impl RuntimeStore {
    pub fn mode(&self) -> StorageMode {
        match self {
            Self::Native { .. } => StorageMode::Sqlite,
            Self::Cluster(_) => StorageMode::Cluster,
        }
    }
    pub fn store(&self) -> Arc<dyn Store> {
        match self {
            Self::Native { handle, .. } => Arc::clone(&handle.store),
            Self::Cluster(handle) => Arc::clone(&handle.store),
        }
    }
    pub fn identity(&self) -> LocalIdentity {
        match self {
            Self::Native { handle, .. } => handle.identity.clone(),
            Self::Cluster(handle) => crate::cluster_runtime::local_identity(handle),
        }
    }
    pub fn credential_key(&self) -> Arc<CredentialKey> {
        match self {
            Self::Native { handle, .. } => Arc::clone(&handle.credential_key),
            Self::Cluster(handle) => Arc::clone(&handle.credential_key),
        }
    }
    pub fn local_source(&self) -> Option<LocalSourceAuthority> {
        match self {
            Self::Native { source, .. } => source.clone(),
            Self::Cluster(_) => None,
        }
    }
    pub fn services(&self, config: &Config) -> Result<ServiceAdapters, StoreError> {
        match self {
            Self::Native { handle, .. } => Ok(local_services(handle, config)),
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
            Self::Cluster(handle) => handle.prepare_source_schema_before_serving().await,
        }
    }
    pub async fn shutdown(&self) -> Result<(), StoreError> {
        match self {
            Self::Native { source, .. } => {
                if let Some(source) = source {
                    source.revoke();
                }
                Ok(())
            }
            Self::Cluster(handle) => handle.shutdown().await,
        }
    }
}

pub(crate) async fn select(
    config: &Config,
    observer: Option<&dyn plurx_core::cluster::migration::StartupClockObserver>,
) -> Result<RuntimeStore, StoreError> {
    let root = &config.storage.data_dir;
    let lock = NativeDirectoryLock::acquire(root).await?;
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

/// Source preparation happens after selection. Bind only the marker's already
/// committed schema so a crash in between can reopen the predecessor layout.
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

async fn convert_private(
    config: &Config,
    selected: crate::cluster_runtime::PendingSelection,
) -> Result<RuntimeStore, StoreError> {
    let root = &config.storage.data_dir;
    let generation = uuid::Uuid::new_v4().to_string();
    let incoming_relative = PathBuf::from(format!("native-import-{generation}"));
    let incoming = plurx_core::storage::mode::owned_path(root, &incoming_relative)?;
    let target = incoming.join("plurx.db");
    let conversion = async {
        let client = selected.local_client().ok_or_else(|| {
            StoreError::Migration("legacy private storage has no local export client".to_owned())
        })?;
        std::fs::create_dir(&incoming).map_err(|error| {
            StoreError::Migration(format!("creating native conversion target: {error}"))
        })?;
        plurx_core::storage::mode::begin_native_adoption(&incoming)?;
        let receipt = plurx_core::cluster::migration::standalone_export::export_private_singleton(
            &client,
            root,
            &target,
            &selected.identity.cluster_id,
            &selected.identity.node_id,
        )
        .await?;
        let telemetry_source =
            plurx_core::storage::mode::owned_path(root, Path::new("hiqlite/telemetry.db"))?;
        let telemetry_target = target.clone();
        let node_local = tokio::task::spawn_blocking(move || {
            plurx_core::storage::telemetry::transfer_local_telemetry(
                &telemetry_source,
                &telemetry_target,
            )
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining local state conversion: {error}")))??;
        // Preserve the exact ownership identity before opening the imported
        // native store; a newly minted node ID would strand cache/offline data.
        let node_path = plurx_core::storage::mode::owned_path(root, Path::new("node.id"))?;
        std::fs::copy(node_path, incoming.join("node.id")).map_err(|error| {
            StoreError::Migration(format!("preserving native node identity: {error}"))
        })?;
        let imported =
            plurx_core::storage::open_native_store_at(&incoming, &config.credential_key_path())
                .await?;
        if imported.identity.instance_id != selected.identity.cluster_id
            || imported.identity.node_id != selected.identity.node_id
        {
            return Err(StoreError::Migration(
                "native conversion changed logical or local identity".to_owned(),
            ));
        }
        // This validates real local Source/key/custody state, not a readiness
        // flag. Its validation boot is revoked before the target is published.
        let source = imported
            .sqlite
            .prepare_native_source_before_serving(
                &imported.identity.node_id,
                &imported.credential_key,
            )
            .await?;
        source.revoke();
        drop(source);
        let manifest = StorageManifest::read(&incoming)?.ok_or_else(|| {
            StoreError::Migration("verified native import lost its mode binding".to_owned())
        })?;
        drop(imported);
        let checkpoint_target = target.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            plurx_core::storage::mode::checkpoint_native_image(&checkpoint_target)?;
            Ok(())
        })
        .await
        .map_err(|error| StoreError::Task(format!("joining native checkpoint: {error}")))??;
        let receipt_relative = incoming_relative.join("conversion-receipt.json");
        publish_record(
            &root.join(&receipt_relative),
            &serde_json::json!({
                "replicated": &receipt,
                "node_local": node_local,
            }),
        )?;
        Ok::<_, StoreError>((manifest, receipt, receipt_relative))
    }
    .await;
    // The old runtime is drained even when export/validation refuses; until a
    // verified handoff is prepared its database remains the only authority.
    selected.shutdown().await?;
    let lock = selected
        .into_selected()
        .into_native_directory_lock(root.clone())?;
    let (manifest, receipt, receipt_relative) = conversion?;
    let source_path = receipt
        .source_image
        .strip_prefix(root)
        .map_err(|_| StoreError::Migration("export cut escaped the owned root".to_owned()))?
        .to_path_buf();
    let handoff = plurx_core::storage::transition::prepare_native_handoff_with_source(
        root,
        manifest,
        incoming_relative.join("plurx.db"),
        PathBuf::from(format!("retired-storage/{generation}")),
        receipt_relative,
        plurx_core::storage::transition::VerifiedArtifact {
            path: source_path,
            sha256: receipt.source_sha256,
        },
    )?;
    plurx_core::storage::transition::finish_native_handoff(root, &handoff)?;
    native(config, lock).await
}

#[cfg(test)]
mod tests;
