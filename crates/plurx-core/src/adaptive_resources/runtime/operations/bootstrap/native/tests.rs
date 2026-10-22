//! Registration/cancelled-wait fixtures. These NEVER release native entry and
//! are not a Linux filesystem/namespace/physical-cleanup acceptance receipt.
use super::*;
mod steps;
fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).unwrap()
}
fn domain() -> v2::FilesystemDomain {
    v2::FilesystemDomain {
        platform: v2::FilesystemPlatform::Linux,
        domain_id: id(),
        daemon_owner_id: id(),
        os_boot: BootIdentity::Known {
            value: Uuid::new_v4(),
            provider: "fixture".into(),
            version: 1,
        },
        mount_namespace: v2::NativeValue::Unknown {
            reason: "no native entry in this fixture".into(),
        },
    }
}
#[tokio::test]
async fn bootstrap_native_unreleased_entry_latch_proves_only_neverentered_job() {
    let (mut cell, enter) = RootCell::launch(domain(), id(), id()).unwrap();
    assert!(cell.pending.is_some() && cell.result.is_none());
    drop(enter);
    let result = cell.join().await.unwrap();
    assert_eq!(
        result.audit.outcome,
        v2::scratch::NativeOutcomeV1::NeverEntered
    );
    assert!(result.files.is_empty() && result.audit.acquired.is_none());
    assert_eq!(result.audit.inventory, InventoryCertainty::Exact);
}
#[tokio::test]
async fn bootstrap_native_cancelled_join_waiter_keeps_actual_original_handle() {
    let (mut cell, enter) = RootCell::launch(domain(), id(), id()).unwrap();
    let original = cell.pending.as_ref().unwrap().id();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), cell.join())
            .await
            .is_err()
    );
    assert_eq!(cell.pending.as_ref().unwrap().id(), original);
    assert!(cell.result.is_none());
    drop(enter);
    assert_eq!(
        cell.join().await.unwrap().audit.outcome,
        v2::scratch::NativeOutcomeV1::NeverEntered
    );
    assert!(cell.pending.is_none() && !cell.failed_join);
}
#[tokio::test]
async fn bootstrap_native_joined_result_is_retained_without_relaunch_or_new_ids() {
    let (mut cell, enter) = RootCell::launch(domain(), id(), id()).unwrap();
    let original = (cell.observe_id, cell.join_id);
    drop(enter);
    let first = cell.join().await.unwrap().audit.clone();
    let again = cell.join().await.unwrap().audit.clone();
    assert_eq!(first, again);
    assert_eq!((cell.observe_id, cell.join_id), original);
    assert!(cell.pending.is_none() && cell.result.is_some());
}
