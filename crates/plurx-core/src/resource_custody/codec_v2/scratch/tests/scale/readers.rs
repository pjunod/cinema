//! Data-fold reader fixtures only; real HTTP/native lease adapter remains owed.
use super::*;
fn opened() -> (ScratchPlanV2, ScratchFold, ReaderLeaseV1) {
    let mut plan = plan();
    plan.limits.reader_leases = 2;
    let fold = publish(&plan, directory(&plan), 0);
    let file = fold.files[0].clone();
    let io = id();
    let lease = ReaderLeaseV1 {
        lease_id: io,
        asset_id: plan.asset_id,
        file_id: file.file_id,
        copy_id: id(),
        owner_id: id(),
        memory_cap: 1024,
        read_range: ByteRangeV1 {
            start: 0,
            end_exclusive: 4096,
        },
        cursor: 0,
        acquisition_io_id: io,
    };
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::OpenReader {
                lease: lease.clone(),
            },
        },
    );
    let fold = fold.apply(&plan, io, &arm).unwrap();
    assert_eq!(fold.state.totals.readers, 1, "reserve before open");
    assert_eq!(fold.state.totals.memory_reserved_bytes, 1024);
    let observe = change(
        &plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Completed,
            observation: AssetObservationV1::ReaderOpened {
                lease: lease.clone(),
                native_object: file.native_object,
            },
        },
    );
    let fold = fold.apply(&plan, id(), &observe).unwrap();
    (plan, fold, lease)
}
#[test]
fn scratch_v2_read_completion_keeps_lease_buffer_and_handle_until_joined_close_record() {
    let (plan, fold, lease) = opened();
    let read_io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: read_io,
            operation: AssetIoV1::ReadRange {
                lease_id: lease.lease_id,
                range: ByteRangeV1 {
                    start: 0,
                    end_exclusive: 1024,
                },
            },
        },
    );
    let fold = fold.apply(&plan, read_io, &arm).unwrap();
    let observe = change(
        &plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: read_io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Completed,
            observation: AssetObservationV1::ReaderRead {
                lease_id: lease.lease_id,
                bytes_read: 0,
            },
        },
    );
    let fold = fold.apply(&plan, id(), &observe).unwrap();
    assert_eq!(fold.state.totals.readers, 1, "EOF is not a close");
    assert_eq!(fold.state.totals.handles_reserved, 2);
    assert_eq!(fold.state.totals.memory_reserved_bytes, 1024);
    let close_io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: close_io,
            operation: AssetIoV1::CloseReader {
                lease_id: lease.lease_id,
            },
        },
    );
    let fold = fold.apply(&plan, close_io, &arm).unwrap();
    assert_eq!(
        fold.state.totals.handles_reserved, 2,
        "prearmed close is not completion"
    );
    let observe = change(
        &plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: close_io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Completed,
            observation: AssetObservationV1::ReaderClosed {
                lease_id: lease.lease_id,
            },
        },
    );
    let fold = fold.apply(&plan, id(), &observe).unwrap();
    assert_eq!(fold.state.totals.readers, 0);
    assert_eq!(fold.state.totals.handles_reserved, 1);
    assert_eq!(fold.state.totals.memory_reserved_bytes, 0);
    assert_eq!(
        fold.state.totals.resident_file_objects, 1,
        "reader closure does not erase file storage"
    );
}
#[test]
fn scratch_v2_freeze_preserves_read_only_lease_without_waiting_for_response_eof() {
    let (plan, fold, lease) = opened();
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
    assert_eq!(fold.state.totals.readers, 1);
    assert_eq!(fold.state.totals.memory_reserved_bytes, lease.memory_cap);
    let io = id();
    let new_lease = ReaderLeaseV1 {
        lease_id: io,
        acquisition_io_id: io,
        copy_id: id(),
        ..lease
    };
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::OpenReader { lease: new_lease },
        },
    );
    assert!(
        fold.apply(&plan, io, &arm).is_err(),
        "frozen producer cannot admit a new reader; retained S needs explicit transfer"
    );
}
#[test]
fn scratch_v2_reader_range_and_additional_payload_reservations_share_one_memory_ceiling() {
    let (plan, fold, lease) = opened();
    let io = id();
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::ReadRange {
                lease_id: lease.lease_id,
                range: ByteRangeV1 {
                    start: 0,
                    end_exclusive: 1025,
                },
            },
        },
    );
    assert!(fold.apply(&plan, io, &arm).is_err());
    let (mut plan, fold, _) = opened();
    plan.limits.resident_memory_bytes = 4096;
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
    assert!(
        fold.apply(&plan, io, &arm).is_err(),
        "a new payload cannot borrow an active reader's reserved buffer"
    );
}
