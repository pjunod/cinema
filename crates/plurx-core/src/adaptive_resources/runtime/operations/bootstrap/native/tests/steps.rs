//! New step-owner fixtures. Authored only; no execution before final review.
//! Host cases never enter native step IO. Linux cases require actual Linux
//! compilation/execution and are NOT covered by a host all-target check.
use super::*;
use std::os::fd::AsRawFd;
use v2::scratch::{AllocationAccountingV1, OutputNamesV1, OutputNamingV1, ScratchLimitsV1};
fn plan(path: &Path) -> ScratchPlanV3 {
    ScratchPlanV3 {
        version: 3,
        asset_id: id(),
        rendition_id: "native-fixture".into(),
        generation_epoch: 1,
        filesystem_domain: domain(),
        directory: DirectoryOriginV1::ConfiguredRootBootstrapV1 {
            bootstrap: RootBootstrapPlanV1 {
                version: 1,
                authority: ConfiguredRootAuthorityV1 {
                    installation_id: id(),
                    purpose: RootPurpose::RenditionCacheV1,
                    resolved_absolute_root: path.to_str().unwrap().into(),
                    traversal: RootTraversal::NamespaceRootToConfiguredRootV1,
                    descendant_mounts: DescendantMounts::RefuseV1,
                },
                components: path
                    .components()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(n) => Some(n.to_str().unwrap().into()),
                        _ => None,
                    })
                    .collect(),
                child_component: "fresh-output".into(),
                protected_child_path: path.join("fresh-output").to_str().unwrap().into(),
                bounds: BootstrapBoundsV1 {
                    max_components: 64,
                    max_path_bytes: 4096,
                    max_component_bytes: 255,
                    peak_directory_handles: 2,
                    pending_native_jobs: 1,
                    resident_memory_bytes: 16384,
                    child_directory_reserve_bytes: 65536,
                    parent_entry_reserve_bytes: 65536,
                },
            },
        },
        outputs: OutputNamesV1 {
            init: true,
            first_segment: 0,
            segment_end_exclusive: 10,
            naming: OutputNamingV1::FilmSegmentV1,
        },
        limits: ScratchLimitsV1 {
            resident_file_objects: 16,
            resident_logical_bytes: 65536,
            reserved_storage_bytes: 1048576,
            one_file_logical_bytes: 16384,
            one_io_payload_bytes: 16384,
            pending_native_jobs: 1,
            live_handles: 8,
            reader_leases: 2,
            resident_memory_bytes: 1048576,
            allocation: AllocationAccountingV1::LogicalAndReportedAllocationV1 {
                allocation_unit_bytes: 4096,
                per_object_reserve_bytes: 4096,
                directory_reserve_bytes: 4096,
            },
        },
    }
}
fn reference(cell: &RootCell) -> v2::scratch::OwnedDirectoryRefV1 {
    v2::scratch::OwnedDirectoryRefV1 {
        owning_obligation: id(),
        member_id: id(),
        directory_id: cell
            .result
            .as_ref()
            .unwrap()
            .audit
            .acquired
            .as_ref()
            .unwrap()
            .directory_id,
    }
}
fn held_fixture(path: &Path, plan: &ScratchPlanV3) -> RootCell {
    let copy = id();
    let file = File::open(path).unwrap();
    // These are deliberately fixture-only audit values, NOT native proof.
    let observed = id();
    RootCell {
        io_id: id(),
        observe_id: observed,
        join_id: id(),
        pending: None,
        failed_join: false,
        imported_retained: false,
        result: Some(RootResult {
            files: BTreeMap::from([(copy, file)]),
            audit: BootstrapResultV1 {
                outcome: v2::scratch::NativeOutcomeV1::Completed,
                acquired: Some(v2::scratch::DirectoryObservedV1 {
                    directory_id: observed,
                    acquisition_io_id: id(),
                    held_copy_ids: vec![copy],
                    native_object: v2::NativeObject {
                        file_type: v2::FileType::Directory,
                        device: v2::NativeValue::Known { value: 1 },
                        inode: v2::NativeValue::Known { value: 2 },
                        mount_id: v2::NativeValue::Unknown {
                            reason: "fixture".into(),
                        },
                        filesystem_domain: plan.filesystem_domain.clone(),
                    },
                }),
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        }),
    }
}
#[tokio::test]
async fn bootstrap_step_unreleased_latch_retains_original_parent_without_acquisition() {
    let root = tempfile::tempdir().unwrap();
    let plan = plan(root.path());
    let mut cell = held_fixture(root.path(), &plan);
    let old_fd = cell
        .result
        .as_ref()
        .unwrap()
        .files
        .values()
        .next()
        .unwrap()
        .as_raw_fd();
    let operation = BootstrapIoV1::OpenNextComponent {
        index: 0,
        parent: reference(&cell),
        copy_id: id(),
    };
    let enter = cell.launch_next(&plan, id(), &operation).unwrap();
    let original = cell.pending.as_ref().unwrap().id();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), cell.join())
            .await
            .is_err()
    );
    assert_eq!(cell.pending.as_ref().unwrap().id(), original);
    drop(enter);
    let result = cell.join().await.unwrap();
    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files.values().next().unwrap().as_raw_fd(), old_fd);
    assert_eq!(
        result.audit.outcome,
        v2::scratch::NativeOutcomeV1::NeverEntered
    );
    assert!(result.audit.acquired.is_none() && result.audit.closed_copy_ids.is_empty());
}
#[tokio::test]
async fn bootstrap_step_foreign_directory_or_missing_actual_copy_never_launches() {
    let root = tempfile::tempdir().unwrap();
    let plan = plan(root.path());
    let mut cell = held_fixture(root.path(), &plan);
    let mut parent = reference(&cell);
    parent.directory_id = id();
    let wrong = BootstrapIoV1::OpenNextComponent {
        index: 0,
        parent,
        copy_id: id(),
    };
    assert!(cell.launch_next(&plan, id(), &wrong).is_err());
    let correct = BootstrapIoV1::OpenNextComponent {
        index: 0,
        parent: reference(&cell),
        copy_id: id(),
    };
    cell.result.as_mut().unwrap().files.clear();
    assert!(cell.launch_next(&plan, id(), &correct).is_err());
    assert!(cell.pending.is_none());
}
#[tokio::test]
async fn bootstrap_child_unknown_mount_never_enters_mkdir() {
    let root = tempfile::tempdir().unwrap();
    let plan = plan(root.path());
    let mut cell = held_fixture(root.path(), &plan);
    let operation = BootstrapIoV1::CreateRenditionChild {
        parent: reference(&cell),
        copy_id: id(),
    };
    assert!(cell.launch_next(&plan, id(), &operation).is_err());
    assert!(cell.pending.is_none() && !root.path().join("fresh-output").exists());
}

#[cfg(target_os = "linux")]
async fn native_parent(path: &Path) -> (RootCell, ScratchPlanV3) {
    let mut plan = plan(path);
    plan.filesystem_domain.mount_namespace = v2::NativeValue::Known {
        value: std::fs::metadata("/proc/self/ns/mnt").unwrap().ino(),
    };
    let (mut cell, enter) = RootCell::launch(plan.filesystem_domain.clone(), id(), id()).unwrap();
    enter.send(()).unwrap();
    assert_eq!(
        cell.join().await.unwrap().audit.outcome,
        v2::scratch::NativeOutcomeV1::Completed
    );
    for index in 0..plan.bootstrap().components.len() {
        let old = cell
            .result
            .as_ref()
            .unwrap()
            .audit
            .acquired
            .as_ref()
            .unwrap()
            .held_copy_ids
            .clone();
        let operation = BootstrapIoV1::OpenNextComponent {
            index: index as u16,
            parent: reference(&cell),
            copy_id: id(),
        };
        let enter = cell.launch_next(&plan, id(), &operation).unwrap();
        enter.send(()).unwrap();
        let result = cell.join().await.unwrap();
        assert_eq!(
            result.audit.outcome,
            v2::scratch::NativeOutcomeV1::Completed
        );
        assert_eq!(result.audit.closed_copy_ids, old);
        assert_eq!(result.files.len(), 1);
    }
    (cell, plan)
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn bootstrap_native_linux_actual_parent_child_and_mount_are_retained() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let (mut cell, plan) = native_parent(&path).await;
    let operation = BootstrapIoV1::CreateRenditionChild {
        parent: reference(&cell),
        copy_id: id(),
    };
    let enter = cell.launch_next(&plan, id(), &operation).unwrap();
    enter.send(()).unwrap();
    let result = cell.join().await.unwrap();
    assert_eq!(
        result.audit.outcome,
        v2::scratch::NativeOutcomeV1::Completed
    );
    assert_eq!(result.audit.child_effect, ChildEffect::Created);
    assert_eq!(result.files.len(), 2);
    let parent_mount = super::super::steps::mount_id(result.files.values().next().unwrap());
    for file in result.files.values() {
        assert_eq!(super::super::steps::mount_id(file), parent_mount);
    }
    let audit = result.audit.clone();
    assert_eq!(cell.join().await.unwrap().audit, audit);
    assert!(path.join("fresh-output").is_dir());
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn bootstrap_native_linux_existing_child_is_never_adopted() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    std::fs::create_dir(path.join("fresh-output")).unwrap();
    let (mut cell, plan) = native_parent(&path).await;
    let operation = BootstrapIoV1::CreateRenditionChild {
        parent: reference(&cell),
        copy_id: id(),
    };
    let enter = cell.launch_next(&plan, id(), &operation).unwrap();
    enter.send(()).unwrap();
    let result = cell.join().await.unwrap();
    assert!(matches!(
        result.audit.outcome,
        v2::scratch::NativeOutcomeV1::Failed { .. }
    ));
    assert_eq!(result.audit.child_effect, ChildEffect::Unchanged);
    assert!(result.audit.acquired.is_none());
    assert_eq!(result.files.len(), 1);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cleanup_directory_subset_preserves_original_unlent_files_and_rejects_incomplete_inventory()
{
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let (mut cell, plan) = native_parent(&path).await;
    let parent = cell
        .result
        .as_ref()
        .unwrap()
        .audit
        .acquired
        .clone()
        .unwrap();
    let operation = BootstrapIoV1::CreateRenditionChild {
        parent: reference(&cell),
        copy_id: id(),
    };
    cell.launch_next(&plan, id(), &operation)
        .unwrap()
        .send(())
        .unwrap();
    let result = cell.join().await.unwrap();
    assert_eq!(
        result.audit.outcome,
        v2::scratch::NativeOutcomeV1::Completed
    );
    let child = result.audit.acquired.clone().unwrap();
    let original_fds: BTreeMap<_, _> = result
        .files
        .iter()
        .map(|(id, file)| (*id, file.as_raw_fd()))
        .collect();
    let baseline = [None, Some(&parent), Some(&child)];
    assert!(cell
        .lend_observed_directories(std::slice::from_ref(&child), &[None, None, Some(&child)])
        .is_err());
    let mut foreign = child.clone();
    foreign.directory_id = id();
    assert!(cell
        .lend_observed_directories(&[foreign], &baseline)
        .is_err());
    assert!(cell
        .lend_observed_directories(&[child.clone(), child.clone()], &baseline)
        .is_err());
    assert_eq!(cell.result.as_ref().unwrap().files.len(), 2);
    for (selected, retained) in [(&child, &parent), (&parent, &child)] {
        let mut lent = cell
            .lend_observed_directories(std::slice::from_ref(selected), &baseline)
            .unwrap();
        assert_eq!(lent.len(), 1);
        assert_eq!(
            lent[&selected.held_copy_ids[0]].as_raw_fd(),
            original_fds[&selected.held_copy_ids[0]]
        );
        let held = &cell.result.as_ref().unwrap().files;
        assert_eq!(held.len(), 1);
        assert_eq!(
            held[&retained.held_copy_ids[0]].as_raw_fd(),
            original_fds[&retained.held_copy_ids[0]]
        );
        assert!(!cell.no_original_handles());
        cell.return_observed_directories(std::slice::from_ref(selected), &mut lent)
            .unwrap();
        assert!(lent.is_empty());
        assert_eq!(cell.result.as_ref().unwrap().files.len(), 2);
    }
}
