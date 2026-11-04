//! Optional replication composition. Local storage never imports this module.
//!
//! This adapter supplies providers only when replication is selected. Native
//! service construction belongs to storage_runtime; no local factory lives here.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::storage_runtime::ServiceAdapters;
use plurx_core::cluster::membership::{LocalMembership, MembershipManager};
use plurx_core::cluster::migration::{
    select_daemon_store_observing_locked, SelectedBackend, SelectedStore, StartupClockObserver,
};
use plurx_core::config::Config;
use plurx_core::error::StoreError;
use plurx_core::storage::mode::{read_record, StorageMode};
use plurx_core::storage::{LocalIdentity, NativeDirectoryLock};

mod background;
pub(crate) mod create;
mod settings_changes;
pub(crate) mod source_admission;
pub(crate) use background::spawn_background_loops;

pub(crate) use plurx_core::cluster::migration::PendingSelectedStore as PendingSelection;

/// Historical explicit admission/configuration, never peer reachability.
pub(crate) fn legacy_cluster_was_requested(config: &Config) -> Result<bool, StoreError> {
    if config.storage.mode == Some(StorageMode::Cluster)
        || !config.cluster.join_token_file.as_os_str().is_empty()
        || !config.cluster.advertise_host.trim().is_empty()
    {
        return Ok(true);
    }
    let marker_path = plurx_core::storage::mode::owned_path(
        &config.storage.data_dir,
        Path::new("hiqlite/activation.json"),
    )?;
    let activation: Option<plurx_core::cluster::migration::ActivationMarker> =
        read_record(&marker_path)?;
    if activation.is_some_and(|marker| marker.admitted_role.is_some()) {
        // Joined voters/learners and restored first voters were deliberately
        // admitted even when their remaining bootstrap address is loopback.
        return Ok(true);
    }
    let membership: Option<LocalMembership> =
        read_record(&config.storage.data_dir.join("membership.json"))?;
    Ok(membership.is_some_and(|membership| {
        !membership.local.is_loopback()
            || membership.bootstrap.len() > 1
            || membership.bootstrap.iter().any(|peer| !peer.is_loopback())
    }))
}

pub(crate) async fn open(
    config: &Config,
    observer: Option<&dyn StartupClockObserver>,
    lock: NativeDirectoryLock,
) -> Result<SelectedStore, StoreError> {
    for relative in ["hiqlite", "hiqlite.incoming"] {
        plurx_core::storage::mode::owned_path(&config.storage.data_dir, Path::new(relative))?;
    }
    let selected =
        select_daemon_store_observing_locked(config, observer, lock.into_locked_file()?).await?;
    if selected.backend != SelectedBackend::Replicated {
        return Err(StoreError::Migration(
            "explicit cluster activation was interrupted; restart to retry the verified import; refusing to serve a cluster on unreplicated recovery storage".to_owned(),
        ));
    }
    Ok(selected)
}

/// Check committed directory identity without constructing a cluster client
/// or allowing its startup migrations to write anything.
pub(crate) fn validate_manifest(
    config: &Config,
    manifest: &plurx_core::storage::mode::StorageManifest,
) -> Result<(), StoreError> {
    use std::io::Read;
    let marker: plurx_core::cluster::migration::ActivationMarker = read_record(
        &config.storage.data_dir.join("hiqlite/activation.json"),
    )?
    .ok_or_else(|| {
        StoreError::Migration("cluster authority is missing its activation marker".to_owned())
    })?;
    let mut node = String::new();
    plurx_core::fs_secure::open_read_nofollow_blocking(&config.storage.data_dir.join("node.id"))
        .and_then(|file| file.take(4097).read_to_string(&mut node))
        .map_err(|error| {
            StoreError::Migration(format!("reading cluster node identity: {error}"))
        })?;
    if node.len() > 4096
        || node.trim() != manifest.node_id
        || marker.cluster_id != manifest.instance_id
        || marker.replicated_schema_version < manifest.schema_version
    {
        return Err(StoreError::Migration(
            "cluster manifest does not match its committed directory identity".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn services(
    selected: &SelectedStore,
    config: &Config,
) -> Result<ServiceAdapters, StoreError> {
    let client = selected
        .local_client()
        .ok_or_else(|| StoreError::Task("cluster runtime has no local client".to_owned()))?;
    let settings_changes = settings_changes::from_client(&client);
    let snapshot_timeout = Duration::from_secs(config.cluster.snapshot_transfer_timeout_secs)
        .saturating_add(Duration::from_secs(45));
    Ok(ServiceAdapters {
        replication: selected.replication_monitor(),
        membership: selected.membership_manager(),
        settings_changes,
        backup: crate::backup::cluster::ClusterBackupProvider::new(
            client,
            config.storage.data_dir.clone(),
            config.cluster.credential_key_path(&config.storage.data_dir),
            selected.identity.node_id.clone(),
            selected.credential_key.id().to_owned(),
            snapshot_timeout,
        ),
    })
}

pub(crate) fn local_identity(selected: &SelectedStore) -> LocalIdentity {
    LocalIdentity {
        instance_id: selected.identity.cluster_id.clone(),
        node_id: selected.identity.node_id.clone(),
    }
}

pub(crate) fn active_database(root: &Path) -> std::path::PathBuf {
    root.join("hiqlite")
}

pub(crate) fn settings_changes_for_membership(
    membership: &MembershipManager,
) -> crate::settings_changes::SettingsChangeSource {
    match membership.local_db_raft_metrics() {
        Some(metrics) => settings_changes::from_metrics(metrics),
        None => crate::settings_changes::SettingsChangeSource::LocalOnly,
    }
}

/// Cluster Source coordination is absent from native startup.
pub(crate) async fn prepare_source_authority(
    state: &crate::state::AppState,
    purpose_master: Arc<plurx_core::secrets::CredentialKey>,
) -> anyhow::Result<()> {
    use anyhow::Context;
    if state.storage_mode == plurx_core::storage::mode::StorageMode::Cluster {
        state
            .membership
            .set_ingress_custody_boot(Some(state.sharing.accepted_drivers.boot_id()));
        state
            .membership
            .publish_ingress_custody_boot()
            .await
            .context("publishing actual accepted-driver custody boot")?;
        state
            .membership
            .prepare_purpose_master(purpose_master)
            .await
            .context("qualifying coordinated sharing purpose master")?;
        match tokio::time::timeout(
            Duration::from_secs(5),
            state.membership.coordinate_purpose_keys(),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(code = error.code(), "sharing purpose factory is pending")
            }
            Err(_) => tracing::warn!(code = "deadline", "sharing purpose factory is pending"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn managed_created_loopback_cluster_remains_explicit_without_its_mode_manifest() {
        use plurx_core::cluster::membership::{ClusterPeer, ClusterRole, LocalMembership};
        use plurx_core::cluster::migration::ActivationMarker;
        let root = tempfile::tempdir().expect("managed admission metadata fixture");
        std::fs::create_dir(root.path().join("hiqlite")).expect("metadata directory");
        let mut config = plurx_core::config::Config::default();
        config.storage.data_dir = root.path().to_path_buf();
        let instance = uuid::Uuid::new_v4().to_string();
        let node = uuid::Uuid::new_v4().to_string();
        let local = ClusterPeer {
            raft_id: 1,
            raft_address: "127.0.0.1:5439".into(),
            api_address: "127.0.0.1:5440".into(),
        };
        let membership = LocalMembership {
            version: 1,
            cluster_id: instance.clone(),
            node_id: node,
            raft_id: 1,
            local: local.clone(),
            bootstrap: vec![local],
            join_token_digest: None,
            role: ClusterRole::Voter,
        };
        plurx_core::storage::mode::publish_record(
            &root.path().join("membership.json"),
            &membership,
        )
        .expect("retained loopback membership metadata");
        let mut marker = ActivationMarker {
            marker_version: 1,
            cluster_id: instance,
            source_backup_sha256: "a".repeat(64),
            source_schema_version: plurx_core::store::SQLITE_SCHEMA_VERSION,
            replicated_schema_version: plurx_core::store::AUTH_SCHEMA_VERSION,
            imported_rows: 1,
            table_hashes: vec![],
            admitted_role: Some(ClusterRole::Voter),
        };
        let path = root.path().join("hiqlite/activation.json");
        plurx_core::storage::mode::publish_record(&path, &marker)
            .expect("managed admission provenance");
        assert!(config.storage.mode.is_none());
        assert!(config.cluster.advertise_host.is_empty());
        assert!(!root.path().join("storage-mode.json").exists());
        assert!(
            super::legacy_cluster_was_requested(&config).expect("managed cluster remains explicit")
        );
        // Only the durable explicit-admission role distinguishes this cohort
        // from the old unconditional private singleton. No voter is created.
        marker.admitted_role = None;
        plurx_core::storage::mode::publish_record(&path, &marker)
            .expect("old private-cohort comparison");
        assert!(!super::legacy_cluster_was_requested(&config).expect("legacy comparison"));
    }

    #[test]
    fn restored_and_joined_loopback_nodes_remain_explicit_clusters() {
        use plurx_core::cluster::membership::ClusterRole;
        use plurx_core::cluster::migration::ActivationMarker;
        let root = tempfile::tempdir().expect("admission fixture");
        std::fs::create_dir(root.path().join("hiqlite")).expect("legacy target");
        let mut config = plurx_core::config::Config::default();
        config.storage.data_dir = root.path().to_path_buf();
        let mut marker = ActivationMarker {
            marker_version: 1,
            cluster_id: uuid::Uuid::new_v4().to_string(),
            source_backup_sha256: "a".repeat(64),
            source_schema_version: 123,
            replicated_schema_version: plurx_core::store::AUTH_SCHEMA_BASELINE_VERSION,
            imported_rows: 0,
            table_hashes: vec![],
            admitted_role: None,
        };
        for role in [None, Some(ClusterRole::Voter), Some(ClusterRole::Learner)] {
            let admitted = role.is_some();
            marker.admitted_role = role;
            plurx_core::storage::mode::publish_record(
                &root.path().join("hiqlite/activation.json"),
                &marker,
            )
            .expect("saved admission record");
            assert_eq!(
                super::legacy_cluster_was_requested(&config).expect("readonly admission inference"),
                admitted
            );
        }
        assert!(!root.path().join("secret_raft").exists());
        assert!(!root.path().join("storage-mode.json").exists());
    }
}
