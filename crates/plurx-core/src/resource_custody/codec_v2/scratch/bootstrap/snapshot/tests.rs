//! Deterministic snapshot regressions; no native ownership is inferred here.
use super::*;

fn id() -> Id {
    Id::new(Uuid::new_v4()).unwrap()
}
fn plan() -> ScratchPlanV3 {
    let old = super::super::super::tests::plan();
    ScratchPlanV3 {
        version: 3,
        asset_id: old.asset_id,
        rendition_id: old.rendition_id,
        generation_epoch: old.generation_epoch,
        filesystem_domain: old.filesystem_domain,
        directory: DirectoryOriginV1::ConfiguredRootBootstrapV1 {
            bootstrap: RootBootstrapPlanV1 {
                version: 1,
                authority: ConfiguredRootAuthorityV1 {
                    installation_id: id(),
                    purpose: RootPurpose::RenditionCacheV1,
                    resolved_absolute_root: "/cache".into(),
                    traversal: RootTraversal::NamespaceRootToConfiguredRootV1,
                    descendant_mounts: DescendantMounts::RefuseV1,
                },
                components: vec!["cache".into()],
                child_component: "film".into(),
                protected_child_path: "/cache/film".into(),
                bounds: BootstrapBoundsV1 {
                    max_components: 64,
                    max_path_bytes: 4096,
                    max_component_bytes: 255,
                    peak_directory_handles: 2,
                    pending_native_jobs: 1,
                    resident_memory_bytes: 4096,
                    child_directory_reserve_bytes: 4096,
                    parent_entry_reserve_bytes: 4096,
                },
            },
        },
        outputs: old.outputs,
        limits: ScratchLimitsV1 {
            resident_file_objects: 8,
            resident_logical_bytes: 65536,
            reserved_storage_bytes: 131072,
            one_file_logical_bytes: 16384,
            one_io_payload_bytes: 16384,
            pending_native_jobs: 1,
            live_handles: 8,
            reader_leases: 2,
            resident_memory_bytes: 131072,
            allocation: AllocationAccountingV1::LogicalAndReportedAllocationV1 {
                allocation_unit_bytes: 4096,
                per_object_reserve_bytes: 4096,
                directory_reserve_bytes: 4096,
            },
        },
    }
}
fn apply(fold: BootstrapFold, plan: &ScratchPlanV3, event: AssetEventV3) -> BootstrapFold {
    let operation_id = id();
    let change = AssetChangeV3 {
        scratch_member_id: fold.member_id(),
        asset_id: plan.asset_id,
        generation_epoch: plan.generation_epoch,
        previous: fold.state().asset.head.clone(),
        event,
    };
    fold.apply(plan, operation_id, &change).unwrap()
}
fn arm(
    fold: BootstrapFold,
    plan: &ScratchPlanV3,
    io_id: Id,
    operation: BootstrapIoV1,
) -> BootstrapFold {
    let change = AssetChangeV3 {
        scratch_member_id: fold.member_id(),
        asset_id: plan.asset_id,
        generation_epoch: plan.generation_epoch,
        previous: fold.state().asset.head.clone(),
        event: AssetEventV3::ArmBootstrapV1 { io_id, operation },
    };
    fold.apply(plan, io_id, &change).unwrap()
}
fn directory(plan: &ScratchPlanV3, operation_id: Id, io_id: Id, copy: Id) -> DirectoryObservedV1 {
    DirectoryObservedV1 {
        directory_id: operation_id,
        acquisition_io_id: io_id,
        held_copy_ids: vec![copy],
        native_object: NativeObject {
            file_type: FileType::Directory,
            device: NativeValue::Known { value: 1 },
            inode: NativeValue::Known { value: 42 },
            mount_id: NativeValue::Known { value: 17 },
            filesystem_domain: plan.filesystem_domain.clone(),
        },
    }
}
fn observe(
    fold: BootstrapFold,
    plan: &ScratchPlanV3,
    io_id: Id,
    copy: Id,
    closed: Vec<Id>,
    effect: ChildEffect,
) -> BootstrapFold {
    let operation_id = id();
    let change = AssetChangeV3 {
        scratch_member_id: fold.member_id(),
        asset_id: plan.asset_id,
        generation_epoch: plan.generation_epoch,
        previous: fold.state().asset.head.clone(),
        event: AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(directory(plan, operation_id, io_id, copy)),
                closed_copy_ids: closed,
                child_effect: effect,
                inventory: InventoryCertainty::Exact,
            },
        },
    };
    fold.apply(plan, operation_id, &change).unwrap()
}
fn sink_ready(plan: &ScratchPlanV3) -> BootstrapFold {
    let mut fold = BootstrapFold::empty(plan, id(), id()).unwrap();
    let root_io = id();
    let root_copy = id();
    fold = arm(
        fold,
        plan,
        root_io,
        BootstrapIoV1::OpenNamespaceRoot { copy_id: root_copy },
    );
    fold = observe(fold, plan, root_io, root_copy, vec![], ChildEffect::None);
    let parent_ref = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation_id(),
        member_id: fold.member_id(),
        directory_id: fold
            .state()
            .bootstrap
            .current
            .as_ref()
            .unwrap()
            .observed
            .directory_id,
    };
    let parent_io = id();
    let parent_copy = id();
    fold = arm(
        fold,
        plan,
        parent_io,
        BootstrapIoV1::OpenNextComponent {
            index: 0,
            parent: parent_ref,
            copy_id: parent_copy,
        },
    );
    fold = observe(
        fold,
        plan,
        parent_io,
        parent_copy,
        vec![root_copy],
        ChildEffect::None,
    );
    let parent_ref = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation_id(),
        member_id: fold.member_id(),
        directory_id: fold
            .state()
            .bootstrap
            .parent
            .as_ref()
            .unwrap()
            .observed
            .directory_id,
    };
    let child_io = id();
    let child_copy = id();
    fold = arm(
        fold,
        plan,
        child_io,
        BootstrapIoV1::CreateRenditionChild {
            parent: parent_ref,
            copy_id: child_copy,
        },
    );
    observe(
        fold,
        plan,
        child_io,
        child_copy,
        vec![],
        ChildEffect::Created,
    )
}
#[test]
fn frozen_v3_snapshot_refuses_pending_and_unfrozen_fold() {
    let plan = plan();
    let fold = BootstrapFold::empty(&plan, id(), id()).unwrap();
    assert!(fold.frozen_snapshot(&plan).is_err());
    let io_id = id();
    let pending = arm(
        fold,
        &plan,
        io_id,
        BootstrapIoV1::OpenNamespaceRoot { copy_id: id() },
    );
    assert!(pending.frozen_snapshot(&plan).is_err());
    let sink = sink_ready(&plan);
    assert!(sink.frozen_snapshot(&plan).is_err());
}
#[test]
fn frozen_v3_snapshot_carries_parent_child_and_recounted_charges() {
    let plan = plan();
    let mut fold = sink_ready(&plan);
    fold = apply(
        fold,
        &plan,
        AssetEventV3::OutputV2 {
            event: AssetEventV1::Fence {
                terminal_epoch: plan.generation_epoch,
            },
        },
    );
    let before = fold.state().asset.head.clone().unwrap();
    fold = apply(
        fold,
        &plan,
        AssetEventV3::OutputV2 {
            event: AssetEventV1::Freeze {
                exact_head_before_freeze: before,
            },
        },
    );
    let snapshot = fold.frozen_snapshot(&plan).unwrap();
    assert_eq!(snapshot.obligation_id, fold.obligation_id());
    assert_eq!(snapshot.member_id, fold.member_id());
    assert!(snapshot.state.bootstrap.parent.is_some());
    assert!(snapshot.state.asset.directory.is_some());
    assert_eq!(snapshot.state.asset.totals.handles_reserved, 2);
    assert_eq!(snapshot.state.asset.totals.charged_storage_bytes, 8192);
    assert_eq!(snapshot.state.asset.totals.memory_reserved_bytes, 4096);
    validate_frozen_snapshot(&plan, &snapshot).unwrap();
    let mut altered = snapshot;
    altered.state.asset.totals.charged_storage_bytes -= 1;
    assert!(validate_frozen_snapshot(&plan, &altered).is_err());
}

#[test]
fn frozen_v3_reference_streams_inventory_larger_than_sql_row() {
    use super::super::super::{FileObservedV1, FileSlotId, OutputKey};
    let mut plan = plan();
    let mut fold = sink_ready(&plan);
    fold = apply(
        fold,
        &plan,
        AssetEventV3::OutputV2 {
            event: AssetEventV1::Fence {
                terminal_epoch: plan.generation_epoch,
            },
        },
    );
    let before = fold.state().asset.head.clone().unwrap();
    fold = apply(
        fold,
        &plan,
        AssetEventV3::OutputV2 {
            event: AssetEventV1::Freeze {
                exact_head_before_freeze: before,
            },
        },
    );
    let source_inventory_bytes = fold.resident_inventory_bytes().unwrap();
    assert!(source_inventory_bytes > 0);
    let mut full = fold.frozen_snapshot(&plan).unwrap();
    let small_inventory_bytes = full.resident_inventory_bytes().unwrap();
    plan.outputs.first_segment = 0;
    plan.outputs.segment_end_exclusive = 400;
    plan.limits.resident_file_objects = 400;
    plan.limits.reserved_storage_bytes = 2_000_000;
    let mut native = full
        .state
        .asset
        .directory
        .as_ref()
        .unwrap()
        .native_object
        .clone();
    native.file_type = FileType::Regular;
    for index in 0..400 {
        full.files.push(FileObservedV1 {
            file_id: id(),
            slot_id: FileSlotId(id()),
            creation_io_id: id(),
            native_object: native.clone(),
            temporary: false,
            published: Some(OutputKey::Segment { index }),
            logical_len: 1,
            reported_allocated_bytes: NativeValue::Known { value: 1 },
            charged_bytes: 4096,
            held_copy_ids: vec![],
        });
    }
    let large_inventory_bytes = full.resident_inventory_bytes().unwrap();
    assert!(large_inventory_bytes > small_inventory_bytes);
    let mut long_identity = String::with_capacity(1024);
    long_identity.push('x');
    full.files[0].native_object.device = NativeValue::Unknown {
        reason: long_identity,
    };
    let long_identity_bytes = full.resident_inventory_bytes().unwrap();
    assert!(long_identity_bytes > large_inventory_bytes);
    full.state.asset.totals.resident_file_objects = 400;
    full.state.asset.totals.logical_bytes = 400;
    full.state.asset.totals.charged_storage_bytes = 8192 + 400 * 4096;
    assert!(serde_json::to_vec(&full).unwrap().len() > 65_536);
    let reference = frozen_snapshot_ref(&plan, &full).unwrap();
    assert!(serde_json::to_vec(&reference).unwrap().len() < 65_536);
    validate_frozen_snapshot_ref(&plan, &reference, &full).unwrap();
}
