use super::*;

fn rolling_plan() -> ScratchPlanV2 {
    let mut plan = plan();
    plan.outputs.naming = OutputNamingV1::CopyRollingV1;
    plan
}

fn first_playlist(plan: &ScratchPlanV2) -> ScratchFold {
    let fold = directory(plan);
    let io = id();
    let arm = change(
        plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: AssetIoV1::PublishNew {
                slot_id: FileSlotId(io),
                output: OutputKey::Playlist,
                logical_end: 64,
                payload_capacity: 64,
                copy_id: id(),
            },
        },
    );
    let fold = fold.apply(plan, io, &arm).expect("first playlist arm");
    let mut native = fold.state.directory.as_ref().unwrap().native_object.clone();
    native.file_type = FileType::Regular;
    native.device = NativeValue::Known { value: 1 };
    native.inode = NativeValue::Known { value: 1001 };
    native.mount_id = NativeValue::Known { value: 2 };
    let observed_id = id();
    let observe = change(
        plan,
        &fold,
        AssetEventV1::ObserveIo {
            io_id: io,
            native_join_operation_id: id(),
            outcome: NativeOutcomeV1::Completed,
            observation: AssetObservationV1::File {
                observed: FileObservedV1 {
                    file_id: observed_id,
                    slot_id: FileSlotId(io),
                    creation_io_id: io,
                    native_object: native,
                    temporary: false,
                    published: Some(OutputKey::Playlist),
                    logical_len: 64,
                    reported_allocated_bytes: NativeValue::Known { value: 64 },
                    charged_bytes: plan.limits.storage_for(64).unwrap(),
                    held_copy_ids: vec![],
                },
                content_binding: Some("fixture-first-playlist".into()),
            },
        },
    );
    fold.apply(plan, observed_id, &observe)
        .expect("first playlist observation")
}

#[test]
fn rolling_playlist_replacement_charges_old_and_new_and_requires_exact_prior_object() {
    let plan = rolling_plan();
    let fold = first_playlist(&plan);
    let old = fold.files[0].clone();
    let prior_charge = fold.state.totals.charged_storage_bytes;
    let io = id();
    let operation = AssetIoV1::ReplacePlaylist {
        prior_file_id: old.file_id,
        expected_native_object: old.native_object.clone(),
        slot_id: FileSlotId(io),
        logical_end: 80,
        payload_capacity: 128,
        copy_id: id(),
    };
    let arm = change(
        &plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation: operation.clone(),
        },
    );
    let pending = fold.apply(&plan, io, &arm).expect("exact prior arm");
    assert_eq!(
        pending.state.totals.charged_storage_bytes,
        prior_charge + plan.limits.storage_for(80).unwrap()
    );
    assert_eq!(pending.state.totals.resident_file_objects, 2);

    let changed = first_playlist(&plan);
    let wrong_old = changed.files[0].clone();
    let wrong_io = id();
    let wrong = change(
        &plan,
        &changed,
        AssetEventV1::ArmIo {
            io_id: wrong_io,
            operation: AssetIoV1::ReplacePlaylist {
                prior_file_id: id(),
                expected_native_object: wrong_old.native_object,
                slot_id: FileSlotId(wrong_io),
                logical_end: 80,
                payload_capacity: 80,
                copy_id: id(),
            },
        },
    );
    assert!(changed.apply(&plan, wrong_io, &wrong).is_err());
}
