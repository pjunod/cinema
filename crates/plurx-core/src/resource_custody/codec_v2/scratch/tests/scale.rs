//! Scale/transition codec fixtures, not native acquisition/behavior receipts.
mod readers;
use super::*;
fn publish(plan: &ScratchPlanV2, fold: ScratchFold, index: u32) -> ScratchFold {
    let io = id();
    let arm = change(
        plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Segment { index },
                logical_end: 4096,
                payload_capacity: 4096,
                copy_id: id(),
            },
        },
    );
    assert!(codec::encode(&arm).unwrap().len() <= 65536);
    let fold = fold.apply(plan, io, &arm).unwrap();
    let observed_id = id();
    let mut native = fold.state.directory.as_ref().unwrap().native_object.clone();
    native.file_type = FileType::Regular;
    native.device = NativeValue::Known { value: 1 };
    native.inode = NativeValue::Known {
        value: 1000 + u64::from(index),
    };
    let observed = FileObservedV1 {
        file_id: observed_id,
        slot_id: FileSlotId(io),
        creation_io_id: io,
        native_object: native,
        temporary: false,
        published: Some(OutputKey::Segment { index }),
        logical_len: 4096,
        reported_allocated_bytes: NativeValue::Known { value: 4096 },
        charged_bytes: 8192,
        held_copy_ids: vec![],
    };
    let event = change(
        plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Completed,
            observation: AssetObservationV1::File {
                observed,
                content_binding: Some("fixture-verified-output-binding".into()),
            },
        },
    );
    assert!(codec::encode(&event).unwrap().len() <= 65536);
    fold.apply(plan, observed_id, &event).unwrap()
}
#[test]
fn scratch_v2_thousands_of_segments_use_bounded_records_not_oversized_manifest() {
    let plan = plan();
    let plan_bytes = codec::encode(&plan).unwrap();
    let mut fold = directory(&plan);
    for index in 0..4096 {
        fold = publish(&plan, fold, index);
    }
    assert_eq!(fold.files.len(), 4096);
    assert_eq!(fold.state.totals.resident_file_objects, 4096);
    assert_eq!(
        codec::encode(&plan).unwrap(),
        plan_bytes,
        "immutable preparation never grows with file inventory"
    );
    assert!(codec::encode(&fold.state).unwrap().len() < 4096);
    let fence = change(
        &plan,
        &fold,
        AssetEventV1::Fence {
            terminal_epoch: plan.generation_epoch,
        },
    );
    let fold = fold.apply(&plan, id(), &fence).unwrap();
    let freeze = change(
        &plan,
        &fold,
        AssetEventV1::Freeze {
            exact_head_before_freeze: fold.state.head.clone().unwrap(),
        },
    );
    let fold = fold.apply(&plan, id(), &freeze).unwrap();
    assert_eq!(fold.state.lifecycle, ScratchLifecycleV1::Frozen);
    assert_eq!(
        fold.files.len(),
        4096,
        "freeze preserves useful files, not a deletion/refund"
    );
}
#[test]
fn scratch_v2_repeated_published_key_refuses_and_old_object_keeps_charge() {
    let plan = plan();
    let fold = publish(&plan, directory(&plan), 3);
    let io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Segment { index: 3 },
                logical_end: 4096,
                payload_capacity: 4096,
                copy_id: id(),
            },
        },
    );
    assert_eq!(
        fold.state.totals.charged_storage_bytes,
        8192 + plan.limits.directory_charge()
    );
    assert!(fold.apply(&plan, io, &arm).is_err());
}
#[test]
fn scratch_v2_stale_epoch_and_wrong_head_refuse_even_with_valid_native_shape() {
    let plan = plan();
    let fold = directory(&plan);
    let mut event = change(
        &plan,
        &fold,
        AssetEventV1::Fence {
            terminal_epoch: plan.generation_epoch,
        },
    );
    event.generation_epoch += 1;
    assert!(fold.apply(&plan, id(), &event).is_err());
    let fold = directory(&plan);
    let mut event = change(
        &plan,
        &fold,
        AssetEventV1::Fence {
            terminal_epoch: plan.generation_epoch,
        },
    );
    event.previous = Some(AssetEventRefV1 {
        operation_id: id(),
        revision: fold.state.revision,
    });
    assert!(fold.apply(&plan, id(), &event).is_err());
}
