use super::*;
mod metadata;
mod playlist;
mod scale;
use crate::resource_custody::codec_v2::tests::fixture;
fn id() -> Id {
    Id::new(Uuid::new_v4()).unwrap()
}
pub(in crate::resource_custody) fn plan() -> ScratchPlanV2 {
    let manifest = fixture();
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &manifest.members[2] else {
        unreachable!()
    };
    ScratchPlanV2 {
        version: 2,
        asset_id: scratch.asset_id,
        rendition_id: "video".into(),
        generation_epoch: 1,
        filesystem_domain: scratch.directory_object.filesystem_domain.clone(),
        directory: DirectoryPlanV2 {
            protected_absolute_path: "/fixture/output".into(),
            child_component: "output".into(),
            parent_binding: OwnedDirectoryRefV1 {
                owning_obligation: id(),
                member_id: id(),
                directory_id: id(),
            },
            mode: DirectoryModeV1::CreateExclusive,
        },
        outputs: OutputNamesV1 {
            init: true,
            first_segment: 0,
            segment_end_exclusive: 10000,
            naming: OutputNamingV1::FilmSegmentV1,
        },
        limits: ScratchLimitsV1 {
            resident_file_objects: 10000,
            resident_logical_bytes: 100_000_000,
            reserved_storage_bytes: 200_000_000,
            one_file_logical_bytes: 4096,
            one_io_payload_bytes: 4096,
            pending_native_jobs: 1,
            live_handles: 4,
            reader_leases: 0,
            resident_memory_bytes: 1_000_000,
            allocation: AllocationAccountingV1::LogicalAndReportedAllocationV1 {
                allocation_unit_bytes: 4096,
                per_object_reserve_bytes: 4096,
                directory_reserve_bytes: 4096,
            },
        },
    }
}
fn change(plan: &ScratchPlanV2, fold: &ScratchFold, event: AssetEventV1) -> AssetChangeV2 {
    AssetChangeV2 {
        scratch_member_id: id(),
        asset_id: plan.asset_id,
        generation_epoch: plan.generation_epoch,
        previous: fold.state.head.clone(),
        event,
    }
}
fn directory(plan: &ScratchPlanV2) -> ScratchFold {
    let fold = ScratchFold::empty(plan).unwrap();
    let io = id();
    let copy = id();
    let arm = change(
        plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::CreateDirectory { copy_id: copy },
        },
    );
    let fold = fold.apply(plan, io, &arm).unwrap();
    let observed_id = id();
    let manifest = fixture();
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &manifest.members[2] else {
        unreachable!()
    };
    let observe = change(
        plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Completed,
            observation: AssetObservationV1::Directory {
                observed: DirectoryObservedV1 {
                    directory_id: observed_id,
                    native_object: scratch.directory_object.clone(),
                    acquisition_io_id: io,
                    held_copy_ids: vec![copy],
                },
            },
        },
    );
    fold.apply(plan, observed_id, &observe).unwrap()
}
#[test]
fn scratch_v2_plan_has_no_future_native_ids_and_v1_equality_is_unchanged() {
    let plan = plan();
    validate_plan(&plan).unwrap();
    let encoded = codec::encode(&plan).unwrap();
    assert!(!encoded.contains("native_object"));
    let manifest = fixture();
    let member = manifest.members[2].clone();
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &member else {
        unreachable!()
    };
    let mut different = scratch.clone();
    different.inventory.buffers.push(OwnerAllocation {
        owner_id: id(),
        memory_bytes: 1,
        handles: 0,
    });
    assert!(validate_envelope(
        &manifest,
        &Envelope {
            version: 2,
            planned: member,
            observed: Some(Observed::ScratchAssetsV1 { scratch: different })
        }
    )
    .is_err());
}
#[test]
fn scratch_v2_write_range_overflow_and_file_memory_handle_limits_refuse_before_entry() {
    let plan = plan();
    let fold = directory(&plan);
    let io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Segment { index: 1 },
                logical_end: 4097,
                payload_capacity: 4097,
                copy_id: id(),
            },
        },
    );
    assert!(fold.apply(&plan, io, &arm).is_err());
    let fold = directory(&plan);
    let io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Segment { index: 1 },
                logical_end: 1,
                payload_capacity: u64::MAX,
                copy_id: id(),
            },
        },
    );
    assert!(fold.apply(&plan, io, &arm).is_err());
    let mut overflow = plan.clone();
    overflow.limits.one_file_logical_bytes = u64::MAX;
    overflow.limits.resident_logical_bytes = u64::MAX;
    assert!(validate_plan(&overflow).is_err());
}
#[test]
fn scratch_v2_freeze_requires_fence_and_consumed_native_operation() {
    let plan = plan();
    let fold = directory(&plan);
    let io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Segment { index: 1 },
                logical_end: 4096,
                payload_capacity: 4096,
                copy_id: id(),
            },
        },
    );
    let fold = fold.apply(&plan, io, &arm).unwrap();
    let fence = change(&plan, &fold, AssetEventV1::Fence { terminal_epoch: 1 });
    let fold = fold.apply(&plan, id(), &fence).unwrap();
    let freeze = change(
        &plan,
        &fold,
        AssetEventV1::Freeze {
            exact_head_before_freeze: fold.state.head.clone().unwrap(),
        },
    );
    assert!(fold.apply(&plan, id(), &freeze).is_err());
}
#[test]
fn scratch_v2_logical_length_is_not_allocated_bytes_and_unknown_does_not_shrink() {
    let plan = plan();
    let fold = directory(&plan);
    let io = id();
    let copy = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Segment { index: 1 },
                logical_end: 4096,
                payload_capacity: 4096,
                copy_id: copy,
            },
        },
    );
    let fold = fold.apply(&plan, io, &arm).unwrap();
    let observed_id = id();
    let manifest = fixture();
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &manifest.members[2] else {
        unreachable!()
    };
    let mut native = scratch.directory_object.clone();
    native.file_type = FileType::Regular;
    let observation = FileObservedV1 {
        file_id: observed_id,
        slot_id: FileSlotId(io),
        creation_io_id: io,
        native_object: native,
        temporary: true,
        published: None,
        logical_len: 17,
        reported_allocated_bytes: NativeValue::Unknown {
            reason: "unavailable".into(),
        },
        charged_bytes: 8192,
        held_copy_ids: vec![],
    };
    let event = change(
        &plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Failed {
                error_class: "partial_write".into(),
            },
            observation: AssetObservationV1::File {
                observed: observation,
                content_binding: None,
            },
        },
    );
    let fold = fold.apply(&plan, observed_id, &event).unwrap();
    assert_eq!(fold.state.totals.logical_bytes, 17);
    assert_eq!(
        fold.state.totals.charged_storage_bytes,
        8192 + plan.limits.directory_charge()
    );
}
