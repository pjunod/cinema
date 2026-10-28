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

fn producer_open(plan: &ScratchPlanV3, with_reader: bool) -> (BootstrapFold, Id, NativeObject) {
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

    (fold, file_id, native)
}

fn retained(plan: &ScratchPlanV3, with_reader: bool) -> (BootstrapFold, Id, NativeObject) {
    let (mut fold, file_id, native) = producer_open(plan, with_reader);
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

#[test]
fn producer_cancel_is_only_for_open_output_with_no_pending_io_or_readers() {
    let plan = plan();
    let (open, _, _) = producer_open(&plan, false);
    let head = open.output.state.head.clone().unwrap();
    let cancelled = output_event(
        open,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .unwrap();
    assert_eq!(
        cancelled.output.state.lifecycle,
        ScratchLifecycleV1::Retiring
    );

    let (open, _, _) = producer_open(&plan, false);
    let head = open.output.state.head.clone().unwrap();
    assert!(output_event(
        open,
        &plan,
        id(),
        AssetEventV1::BeginRetirement {
            exact_head_before_retirement: head,
        },
    )
    .is_err());

    let (read_only, _, _) = retained(&plan, false);
    let head = read_only.output.state.head.clone().unwrap();
    assert!(output_event(
        read_only,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .is_err());

    let (with_reader, _, _) = producer_open(&plan, true);
    let head = with_reader.output.state.head.clone().unwrap();
    assert!(output_event(
        with_reader,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .is_err());

    let (open, file_id, _) = producer_open(&plan, false);
    let io_id = id();
    let pending = output_event(
        open,
        &plan,
        io_id,
        AssetEventV1::ArmIo {
            io_id,
            operation: AssetIoV1::SyncFiles {
                file_ids: vec![file_id],
            },
        },
    )
    .unwrap();
    let head = pending.output.state.head.clone().unwrap();
    assert!(output_event(
        pending,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .is_err());
}

#[test]
fn unstarted_root_requires_observed_never_entry_before_terminal_cancel() {
    let plan = plan();
    let fold = BootstrapFold::empty(&plan, id(), id()).unwrap();
    let root_io = id();
    let fold = apply(
        fold,
        &plan,
        root_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: root_io,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: id() },
        },
    )
    .unwrap();
    let other_io = id();
    let still_armed = apply(
        BootstrapFold::empty(&plan, id(), id()).unwrap(),
        &plan,
        other_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: other_io,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: id() },
        },
    )
    .unwrap();
    let armed_head = still_armed.state().asset.head.unwrap();
    assert!(output_event(
        still_armed,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: armed_head,
        },
    )
    .is_err());
    let fold = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id: root_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::NeverEntered,
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.state().bootstrap.phase, BootstrapPhase::Unstarted);
    let head = fold.state().asset.head.unwrap();
    let closed = output_event(
        fold,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .unwrap();
    assert_eq!(
        closed.state().asset.lifecycle,
        ScratchLifecycleV1::RetiredClosed
    );
    assert_eq!(closed.state().bootstrap.phase, BootstrapPhase::Closed);
    assert!(closed.files().is_empty());
}

#[test]
fn partial_producer_cancel_closes_exact_parent_and_refuses_possible_child() {
    let plan = plan();
    let open = parent_ready(&plan);
    let parent = open.state().bootstrap.parent.unwrap().observed;
    let head = open.state().asset.head.unwrap();
    let retiring = output_event(
        open,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .unwrap();
    assert_eq!(
        retiring.state().asset.lifecycle,
        ScratchLifecycleV1::Retiring
    );
    let close_io = id();
    let copy_ids = parent.held_copy_ids.clone();
    let closing = apply(
        retiring,
        &plan,
        close_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: close_io,
            operation: BootstrapIoV1::CloseBootstrapHandles {
                copy_ids: copy_ids.clone(),
            },
        },
    )
    .unwrap();
    let closed = apply(
        closing,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id: close_io,
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
    assert_eq!(
        closed.state().asset.lifecycle,
        ScratchLifecycleV1::RetiredClosed
    );
    assert_eq!(closed.state().bootstrap.phase, BootstrapPhase::Closed);
    assert_eq!(closed.state().asset.totals.handles_reserved, 0);
    let mut forged_partial = closed.state();
    forged_partial.asset.lifecycle = ScratchLifecycleV1::Retiring;
    forged_partial.bootstrap.phase = BootstrapPhase::ParentReady;
    assert!(validate_projection(&plan, &forged_partial).is_err());

    let old_plan = crate::resource_custody::codec_v2::scratch::tests::plan();
    let old_retiring = crate::resource_custody::codec_v2::scratch::ScratchStateV2 {
        version: 2,
        asset_id: old_plan.asset_id,
        directory: None,
        head: None,
        revision: 0,
        lifecycle: ScratchLifecycleV1::Retiring,
        totals: AssetTotalsV1::default(),
        directory_synced: false,
    };
    assert!(
        crate::resource_custody::codec_v2::scratch::validate_projection(&old_plan, &old_retiring,)
            .is_err()
    );

    let open = parent_ready(&plan);
    let parent = open.state().bootstrap.parent.unwrap().observed;
    let obligation = open.obligation;
    let member = open.member;
    let create_io = id();
    let possible = apply(
        open,
        &plan,
        create_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: create_io,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: OwnedDirectoryRefV1 {
                    owning_obligation: obligation,
                    member_id: member,
                    directory_id: parent.directory_id,
                },
                copy_id: id(),
            },
        },
    )
    .unwrap();
    let head = possible.state().asset.head.unwrap();
    assert!(output_event(
        possible,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .is_err());
}

#[test]
fn traversing_producer_cancel_closes_only_the_original_anchor() {
    let plan = plan();
    let fold = BootstrapFold::empty(&plan, id(), id()).unwrap();
    let root_io = id();
    let anchor_copy = id();
    let fold = apply(
        fold,
        &plan,
        root_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: root_io,
            operation: BootstrapIoV1::OpenNamespaceRoot {
                copy_id: anchor_copy,
            },
        },
    )
    .unwrap();
    let fold = observe(
        fold,
        &plan,
        root_io,
        directory(&plan, id(), root_io, anchor_copy),
        vec![],
        ChildEffect::None,
    );
    assert_eq!(fold.state().bootstrap.phase, BootstrapPhase::Traversing);
    let head = fold.state().asset.head.unwrap();
    let fold = output_event(
        fold,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: head,
        },
    )
    .unwrap();
    let close_io = id();
    let fold = apply(
        fold,
        &plan,
        close_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: close_io,
            operation: BootstrapIoV1::CloseBootstrapHandles {
                copy_ids: vec![anchor_copy],
            },
        },
    )
    .unwrap();
    let closed = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id: close_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: None,
                closed_copy_ids: vec![anchor_copy],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    assert_eq!(
        closed.state().asset.lifecycle,
        ScratchLifecycleV1::RetiredClosed
    );
    assert_eq!(closed.state().bootstrap.phase, BootstrapPhase::Closed);
}

#[test]
fn possible_child_never_entered_must_be_observed_before_partial_cancel() {
    let plan = plan();
    let fold = parent_ready(&plan);
    let parent = fold.state().bootstrap.parent.unwrap().observed;
    let obligation = fold.obligation;
    let member = fold.member;
    let create_io = id();
    let fold = apply(
        fold,
        &plan,
        create_io,
        AssetEventV3::ArmBootstrapV1 {
            io_id: create_io,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: OwnedDirectoryRefV1 {
                    owning_obligation: obligation,
                    member_id: member,
                    directory_id: parent.directory_id,
                },
                copy_id: id(),
            },
        },
    )
    .unwrap();
    let head = fold.state().asset.head.unwrap();
    assert_eq!(fold.state().bootstrap.phase, BootstrapPhase::ChildPossible);
    let fold = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id: create_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::NeverEntered,
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.state().bootstrap.phase, BootstrapPhase::ParentReady);
    let observed_head = fold.state().asset.head.unwrap();
    assert_ne!(head, observed_head);
    output_event(
        fold,
        &plan,
        id(),
        AssetEventV1::BeginProducerCancellation {
            exact_head_before_cancellation: observed_head,
        },
    )
    .unwrap();
}

#[test]
fn retained_child_removal_requires_prior_sync_and_consumes_quiescent_sync_marker() {
    let plan = plan();
    for synced in [false, true] {
        let (fold, file_id, native) = retained(&plan, false);
        let fold = begin_retirement(fold, &plan);
        let mut fold = remove_file(fold, &plan, file_id, native);
        if synced {
            fold = sync_child_directory(fold, &plan);
        }
        let parent = fold.bootstrap.parent.as_ref().unwrap();
        let parent_ref = OwnedDirectoryRefV1 {
            owning_obligation: fold.obligation,
            member_id: fold.member,
            directory_id: parent.observed.directory_id,
        };
        let child_directory_id = fold.output.state.directory.as_ref().unwrap().directory_id;
        let io_id = id();
        let armed = apply(
            fold,
            &plan,
            io_id,
            AssetEventV3::ArmBootstrapV1 {
                io_id,
                operation: BootstrapIoV1::RemoveRenditionChild {
                    parent: parent_ref,
                    child_directory_id,
                    child_component: plan.bootstrap().child_component.clone(),
                },
            },
        );
        if synced {
            let armed = armed.expect("the same lineage proves the preceding sync");
            assert_eq!(armed.bootstrap.phase, BootstrapPhase::RemovingRetainedChild);
            assert_eq!(armed.output.state.totals.pending_jobs, 1);
            assert!(!armed.output.state.directory_synced);
            validate_projection(&plan, &armed.state()).unwrap();
        } else {
            assert!(
                armed.is_err(),
                "pending removal must not invent preceding sync"
            );
        }
    }
}
