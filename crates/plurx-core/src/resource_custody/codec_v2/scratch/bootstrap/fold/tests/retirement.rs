//! Retained cleanup fold vocabulary; data-only regression fixtures.
use super::*;

fn output_event(
    fold: BootstrapFold,
    plan: &ScratchPlanV3,
    operation_id: Id,
    event: AssetEventV1,
) -> Result<BootstrapFold, CustodyError> {
    apply(fold, plan, operation_id, AssetEventV3::OutputV2 { event })
}

fn observe_asset(
    fold: BootstrapFold,
    plan: &ScratchPlanV3,
    io_id: Id,
    outcome: NativeOutcomeV1,
    observation: AssetObservationV1,
) -> BootstrapFold {
    output_event(
        fold,
        plan,
        id(),
        AssetEventV1::ObserveIo {
            io_id,
            native_join_operation_id: id(),
            outcome,
            observation,
        },
    )
    .unwrap()
}

fn retained(plan: &ScratchPlanV3, with_reader: bool) -> (BootstrapFold, Id, NativeObject) {
    let mut fold = parent_ready(plan);
    let parent_id = fold
        .bootstrap
        .parent
        .as_ref()
        .unwrap()
        .observed
        .directory_id;
    let parent_ref = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation,
        member_id: fold.member,
        directory_id: parent_id,
    };
    let child_io = id();
    let child_copy = id();
    fold = apply(
        fold,
        plan,
        child_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: child_io,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: parent_ref,
                copy_id: child_copy,
            },
        },
    )
    .unwrap();
    let child = directory(plan, id(), child_io, child_copy);
    fold = observe(
        fold,
        plan,
        child_io,
        child.clone(),
        vec![],
        ChildEffect::Created,
    );

    let publish_io = id();
    let file_copy = id();
    fold = output_event(
        fold,
        plan,
        publish_io,
        AssetEventV1::ArmIo {
            io_id: publish_io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(publish_io),
                output: OutputKey::Segment { index: 0 },
                logical_end: 4096,
                payload_capacity: 4096,
                copy_id: file_copy,
            },
        },
    )
    .unwrap();
    let file_id = id();
    let mut native = child.native_object.clone();
    native.file_type = FileType::Regular;
    native.inode = NativeValue::Known { value: 9001 };
    let observed_file = FileObservedV1 {
        file_id,
        slot_id: FileSlotId(publish_io),
        creation_io_id: publish_io,
        native_object: native.clone(),
        temporary: false,
        published: Some(OutputKey::Segment { index: 0 }),
        logical_len: 4096,
        reported_allocated_bytes: NativeValue::Known { value: 4096 },
        charged_bytes: 8192,
        held_copy_ids: vec![],
    };
    fold = observe_asset(
        fold,
        plan,
        publish_io,
        NativeOutcomeV1::Completed,
        AssetObservationV1::File {
            observed: observed_file,
            content_binding: Some("test-binding".into()),
        },
    );

    if with_reader {
        let open_io = id();
        let lease = ReaderLeaseV1 {
            lease_id: open_io,
            asset_id: plan.asset_id,
            file_id,
            copy_id: id(),
            owner_id: id(),
            memory_cap: 4096,
            read_range: ByteRangeV1 {
                start: 0,
                end_exclusive: 4096,
            },
            cursor: 0,
            acquisition_io_id: open_io,
        };
        fold = output_event(
            fold,
            plan,
            open_io,
            AssetEventV1::ArmIo {
                io_id: open_io,
                operation: AssetIoV1::OpenReader {
                    lease: lease.clone(),
                },
            },
        )
        .unwrap();
        fold = observe_asset(
            fold,
            plan,
            open_io,
            NativeOutcomeV1::Completed,
            AssetObservationV1::ReaderOpened {
                lease,
                native_object: native.clone(),
            },
        );
    }

    let fence_id = id();
    fold = output_event(
        fold,
        plan,
        fence_id,
        AssetEventV1::Fence {
            terminal_epoch: plan.generation_epoch,
        },
    )
    .unwrap();
    let freeze_id = id();
    let exact_head_before_freeze = fold.output.state.head.clone().unwrap();
    fold = output_event(
        fold,
        plan,
        freeze_id,
        AssetEventV1::Freeze {
            exact_head_before_freeze,
        },
    )
    .unwrap();
    let full = fold.frozen_snapshot(plan).unwrap();
    let reference = frozen_snapshot_ref(plan, &full).unwrap();
    let adopted = fold.adopt_frozen(plan, &reference, id(), id()).unwrap();
    (adopted, file_id, native)
}

fn begin_retirement(fold: BootstrapFold, plan: &ScratchPlanV3) -> BootstrapFold {
    let exact_head_before_retirement = fold.output.state.head.clone().unwrap();
    output_event(
        fold,
        plan,
        id(),
        AssetEventV1::BeginRetirement {
            exact_head_before_retirement,
        },
    )
    .unwrap()
}

fn remove_file(
    fold: BootstrapFold,
    plan: &ScratchPlanV3,
    file_id: Id,
    native: NativeObject,
) -> BootstrapFold {
    let io_id = id();
    let mut fold = output_event(
        fold,
        plan,
        io_id,
        AssetEventV1::ArmIo {
            io_id,
            operation: AssetIoV1::RemoveOwned {
                file_id,
                expected_native_object: Some(native),
            },
        },
    )
    .unwrap();
    fold = observe_asset(
        fold,
        plan,
        io_id,
        NativeOutcomeV1::Completed,
        AssetObservationV1::Removed { file_id },
    );
    fold
}

fn sync_child_directory(fold: BootstrapFold, plan: &ScratchPlanV3) -> BootstrapFold {
    let io_id = id();
    let mut fold = output_event(
        fold,
        plan,
        io_id,
        AssetEventV1::ArmIo {
            io_id,
            operation: AssetIoV1::SyncFiles { file_ids: vec![] },
        },
    )
    .unwrap();
    fold = observe_asset(
        fold,
        plan,
        io_id,
        NativeOutcomeV1::Completed,
        AssetObservationV1::Synced,
    );
    fold
}

fn remove_child(fold: BootstrapFold, plan: &ScratchPlanV3) -> BootstrapFold {
    let parent = fold.bootstrap.parent.as_ref().unwrap();
    let parent_ref = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation,
        member_id: fold.member,
        directory_id: parent.observed.directory_id,
    };
    let directory = fold.output.state.directory.as_ref().unwrap();
    let child_directory_id = directory.directory_id;
    let child_component = plan.bootstrap().child_component.clone();
    let io_id = id();
    let mut fold = apply(
        fold,
        plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::RemoveRenditionChild {
                parent: parent_ref,
                child_directory_id,
                child_component,
            },
        },
    )
    .unwrap();
    fold = apply(
        fold,
        plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::Removed,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    fold
}

fn sync_parent(fold: BootstrapFold, plan: &ScratchPlanV3) -> BootstrapFold {
    let parent = fold.bootstrap.parent.as_ref().unwrap();
    let parent_ref = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation,
        member_id: fold.member,
        directory_id: parent.observed.directory_id,
    };
    let io_id = id();
    let mut fold = apply(
        fold,
        plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::SyncRetainedParent { parent: parent_ref },
        },
    )
    .unwrap();
    fold = apply(
        fold,
        plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    fold
}

fn close_retained(fold: BootstrapFold, plan: &ScratchPlanV3) -> BootstrapFold {
    let mut exact = fold.held_copies();
    exact.extend(
        fold.output
            .state
            .directory
            .as_ref()
            .unwrap()
            .held_copy_ids
            .iter()
            .copied(),
    );
    let copy_ids: Vec<_> = exact.into_iter().collect();
    let io_id = id();
    let mut fold = apply(
        fold,
        plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CloseRetainedHandles {
                copy_ids: copy_ids.clone(),
            },
        },
    )
    .unwrap();
    fold = apply(
        fold,
        plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: None,
                closed_copy_ids: copy_ids,
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    fold
}

#[test]
fn retained_cleanup_requires_durable_unlink_order_then_closes_exact_folded_copy_set() {
    let plan = plan();
    let (fold, file_id, native) = retained(&plan, false);
    let fold = begin_retirement(fold, &plan);
    let fold = remove_file(fold, &plan, file_id, native);
    assert!(fold.output.files.is_empty());
    let fold = sync_child_directory(fold, &plan);
    assert!(fold.output.state.directory_synced);
    let fold = remove_child(fold, &plan);
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::RetainedChildRemoved);
    let fold = sync_parent(fold, &plan);
    assert!(fold.bootstrap.retained_parent_synced);
    let fold = close_retained(fold, &plan);
    assert_eq!(
        fold.state().asset.lifecycle,
        ScratchLifecycleV1::RetiredClosed
    );
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::Closed);
    assert!(fold.output.state.directory.is_none());
    assert!(fold.output.files.is_empty() && fold.output.readers().is_empty());
    assert_eq!(fold.state().asset.totals.pending_jobs, 0);
}

#[test]
fn retained_close_refuses_live_reader_and_refuses_missing_parent_sync() {
    let plan = plan();
    let (with_reader, _, _) = retained(&plan, true);
    let head = with_reader.output.state.head.clone().unwrap();
    assert!(output_event(
        with_reader,
        &plan,
        id(),
        AssetEventV1::BeginRetirement {
            exact_head_before_retirement: head,
        },
    )
    .is_err());

    let (fold, file_id, native) = retained(&plan, false);
    let fold = begin_retirement(fold, &plan);
    let fold = remove_file(fold, &plan, file_id, native);
    let fold = sync_child_directory(fold, &plan);
    let fold = remove_child(fold, &plan);
    let mut exact = fold.held_copies();
    exact.extend(
        fold.output
            .state
            .directory
            .as_ref()
            .unwrap()
            .held_copy_ids
            .iter()
            .copied(),
    );
    let io_id = id();
    assert!(apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CloseRetainedHandles {
                copy_ids: exact.into_iter().collect(),
            },
        },
    )
    .is_err());
}

#[test]
fn unresolved_file_removal_keeps_its_charge_and_cannot_be_retried() {
    let plan = plan();
    let (fold, file_id, native) = retained(&plan, false);
    let before = fold.state().asset.totals.charged_storage_bytes;
    let fold = begin_retirement(fold, &plan);
    let io_id = id();
    let fold = output_event(
        fold,
        &plan,
        io_id,
        AssetEventV1::ArmIo {
            io_id,
            operation: AssetIoV1::RemoveOwned {
                file_id,
                expected_native_object: Some(native.clone()),
            },
        },
    )
    .unwrap();
    let fold = observe_asset(
        fold,
        &plan,
        io_id,
        NativeOutcomeV1::Failed {
            error_class: "unlink-unresolved".into(),
        },
        AssetObservationV1::RemovalUnresolved { file_id },
    );
    assert_eq!(
        fold.state().asset.lifecycle,
        ScratchLifecycleV1::RetirementUncertain
    );
    assert_eq!(fold.state().asset.totals.charged_storage_bytes, before);
    assert_eq!(fold.output.files.len(), 1);
    let retry = id();
    assert!(output_event(
        fold,
        &plan,
        retry,
        AssetEventV1::ArmIo {
            io_id: retry,
            operation: AssetIoV1::RemoveOwned {
                file_id,
                expected_native_object: Some(native),
            },
        },
    )
    .is_err());
}
