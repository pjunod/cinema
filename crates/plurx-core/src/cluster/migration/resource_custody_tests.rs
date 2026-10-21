//! Actual retained-lock seam; authored, not executed before final review.
use super::*;
use crate::resource_custody::InstallationContext;

async fn selected(root: &Path) -> SelectedStore {
    let lock = acquire_daemon_lock_within(root, Duration::ZERO)
        .await
        .expect("actual daemon lock");
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("fixture store"));
    SelectedStore {
        catalogue: CatalogueReader::authority(Arc::clone(&store)),
        store,
        identity: super::super::ClusterIdentity {
            node_id: "custody-fixture-node".into(),
            cluster_id: "custody-fixture-cluster".into(),
            raft_id: 1,
        },
        credential_key: Arc::new(CredentialKey::generate()),
        backend: SelectedBackend::SqliteRecovery,
        membership: MembershipManager::unavailable(),
        replication: status::ReplicationMonitor::sqlite(),
        local_client: None,
        source_schema_startup_consumed: false,
        resource_custody_startup_consumed: false,
        _daemon_lock: lock,
    }
}

#[tokio::test]
async fn custody_startup_is_single_use_and_retains_actual_lock_after_selected_drop() {
    let root = tempfile::tempdir().expect("durable TMPDIR fixture");
    let root = std::fs::canonicalize(root.path()).expect("canonical fixture");
    let mut selected = selected(&root).await;
    let options = selected
        .resource_custody_options(
            root.clone(),
            Vec::new(),
            InstallationContext::FirstInstallation,
        )
        .expect("held-lock-bound inputs");
    assert_eq!(options.owner.node_id, selected.identity.node_id);
    assert_eq!(options.owner.cluster_id, selected.identity.cluster_id);
    assert!(selected
        .resource_custody_options(
            root.clone(),
            Vec::new(),
            InstallationContext::FirstInstallation
        )
        .is_err());
    drop(selected);
    assert!(
        acquire_daemon_lock_within(&root, Duration::ZERO)
            .await
            .is_err(),
        "options retain the actual OS lock, not a pathname or boolean"
    );
    drop(options);
    assert!(acquire_daemon_lock_within(&root, Duration::ZERO)
        .await
        .is_ok());
}

#[tokio::test]
async fn different_root_lock_inode_refuses_and_cannot_retry_a_new_startup_owner() {
    let root = tempfile::tempdir().expect("own root");
    let other = tempfile::tempdir().expect("other root");
    let other_lock = acquire_daemon_lock_within(other.path(), Duration::ZERO)
        .await
        .expect("other lock");
    let mut selected = selected(root.path()).await;
    assert!(selected
        .resource_custody_options(
            std::fs::canonicalize(other.path()).expect("other canonical"),
            Vec::new(),
            InstallationContext::FirstInstallation,
        )
        .is_err());
    assert!(
        selected
            .resource_custody_options(
                std::fs::canonicalize(root.path()).expect("own canonical"),
                Vec::new(),
                InstallationContext::FirstInstallation,
            )
            .is_err(),
        "failed startup cannot mint a second owner/fresh fallback"
    );
    drop(other_lock);
}
