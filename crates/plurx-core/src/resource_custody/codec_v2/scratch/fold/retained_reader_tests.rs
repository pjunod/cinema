//! Reader admission at the frozen producer / retained owner boundary.
use super::*;

fn id() -> Id {
    Id::new(Uuid::new_v4()).unwrap()
}

#[test]
fn frozen_rejects_new_reader_but_retained_read_only_accepts_it() {
    let mut plan = super::super::tests::plan();
    plan.limits.reader_leases = 1;
    let file_id = id();
    let mut native = NativeObject {
        file_type: FileType::Directory,
        device: NativeValue::Known { value: 1 },
        inode: NativeValue::Known { value: 2 },
        mount_id: NativeValue::Known { value: 3 },
        filesystem_domain: plan.filesystem_domain.clone(),
    };
    let directory = DirectoryObservedV1 {
        directory_id: id(),
        acquisition_io_id: id(),
        held_copy_ids: vec![id()],
        native_object: native.clone(),
    };
    native.file_type = FileType::Regular;
    let file = FileObservedV1 {
        file_id,
        slot_id: FileSlotId(id()),
        creation_io_id: id(),
        native_object: native,
        temporary: false,
        published: Some(OutputKey::Init),
        logical_len: 8,
        reported_allocated_bytes: NativeValue::Known { value: 4096 },
        charged_bytes: 4096,
        held_copy_ids: vec![],
    };
    let mut state = ScratchFold::empty_output(plan.asset_id).state;
    state.directory = Some(directory);
    state.lifecycle = ScratchLifecycleV1::Frozen;
    let mut frozen = ScratchFold::from_frozen_inventory(state.clone(), vec![file.clone()], vec![]);
    let io_id = id();
    let arm = AssetEventV1::ArmIo {
        io_id,
        operation: AssetIoV1::OpenReader {
            lease: ReaderLeaseV1 {
                lease_id: io_id,
                asset_id: plan.asset_id,
                file_id,
                copy_id: id(),
                owner_id: id(),
                memory_cap: 4096,
                read_range: ByteRangeV1 {
                    start: 0,
                    end_exclusive: 8,
                },
                cursor: 0,
                acquisition_io_id: io_id,
            },
        },
    };
    assert!(frozen.apply_event(plan.output_view(), io_id, &arm).is_err());
    state.lifecycle = ScratchLifecycleV1::RetainedReadOnly;
    let mut retained = ScratchFold::from_frozen_inventory(state, vec![file], vec![]);
    retained
        .apply_event(plan.output_view(), io_id, &arm)
        .unwrap();
    assert!(retained.has_pending());
}
