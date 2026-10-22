use super::*;

fn metadata_plan(encoded_process: bool) -> ScratchPlanV2 {
    let mut plan = plan();
    plan.outputs.naming = OutputNamingV1::FiniteVodMetadataV2 { encoded_process };
    plan
}

fn arm_publication(
    plan: &ScratchPlanV2,
    fold: ScratchFold,
    output: OutputKey,
    metadata_operation: bool,
) -> Result<(ScratchFold, Id), crate::resource_custody::CustodyError> {
    let io = id();
    let copy_id = id();
    let operation = if metadata_operation {
        AssetIoV1::PublishMetadata {
            slot_id: FileSlotId(io),
            output,
            logical_end: 64,
            payload_capacity: 128,
            copy_id,
        }
    } else {
        AssetIoV1::PublishNew {
            slot_id: FileSlotId(io),
            output,
            logical_end: 64,
            payload_capacity: 128,
            copy_id,
        }
    };
    let event = change(
        plan,
        &fold,
        AssetEventV1::ArmIo {
            io_id: io,
            operation,
        },
    );
    fold.apply(plan, io, &event).map(|fold| (fold, io))
}

fn observe_completed_metadata(
    plan: &ScratchPlanV2,
    fold: ScratchFold,
    io: Id,
    published: OutputKey,
) -> Result<ScratchFold, crate::resource_custody::CustodyError> {
    let operation_id = id();
    let mut native = fold.state.directory.as_ref().unwrap().native_object.clone();
    native.file_type = FileType::Regular;
    let observed = FileObservedV1 {
        file_id: operation_id,
        slot_id: FileSlotId(io),
        creation_io_id: io,
        native_object: native,
        temporary: false,
        published: Some(published),
        logical_len: 64,
        reported_allocated_bytes: NativeValue::Known { value: 64 },
        charged_bytes: plan.limits.storage_for(64)?,
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
                content_binding: Some("fixture-verified-metadata-binding".into()),
            },
        },
    );
    fold.apply(plan, operation_id, &event)
}

#[test]
fn film_segment_profile_rejects_metadata_and_finite_profile_charges_metadata_before_entry() {
    let old_plan = plan();
    let old_fold = directory(&old_plan);
    assert!(arm_publication(&old_plan, old_fold, OutputKey::IdentityMetadata, true,).is_err());

    let finite_plan = metadata_plan(false);
    let finite_fold = directory(&finite_plan);
    let (pending, _) =
        arm_publication(&finite_plan, finite_fold, OutputKey::IdentityMetadata, true)
            .expect("opted-in identity metadata arm");
    assert_eq!(pending.state.totals.pending_jobs, 1);
    assert_eq!(pending.state.totals.resident_file_objects, 1);
    assert_eq!(pending.state.totals.logical_bytes, 64);
    assert_eq!(
        pending.state.totals.charged_storage_bytes,
        finite_plan.limits.directory_charge() + finite_plan.limits.storage_for(64).unwrap()
    );
    assert_eq!(pending.state.totals.memory_reserved_bytes, 128);
    assert_eq!(pending.state.totals.handles_reserved, 2);
}

#[test]
fn metadata_keys_require_publish_metadata_and_only_declared_profile_keys_arm() {
    let plan = metadata_plan(true);
    let fold = directory(&plan);
    assert!(arm_publication(&plan, fold, OutputKey::IdentityMetadata, false).is_err());

    let plan = metadata_plan(false);
    let fold = directory(&plan);
    assert!(arm_publication(&plan, fold, OutputKey::Init, true).is_err());
    let fold = directory(&plan);
    assert!(arm_publication(&plan, fold, OutputKey::Segment { index: 1 }, true,).is_err());

    let plan = metadata_plan(false);
    let fold = directory(&plan);
    assert!(arm_publication(&plan, fold, OutputKey::EncodedProcessMetadata, true).is_err());

    let plan = metadata_plan(true);
    let fold = directory(&plan);
    assert!(arm_publication(&plan, fold, OutputKey::EncodedProcessMetadata, true).is_ok());
}

#[test]
fn completed_metadata_observation_requires_the_exact_armed_key() {
    let plan = metadata_plan(false);
    let (pending, io) = arm_publication(&plan, directory(&plan), OutputKey::IdentityMetadata, true)
        .expect("metadata arm");
    let completed = observe_completed_metadata(&plan, pending, io, OutputKey::IdentityMetadata)
        .expect("exact metadata key observation");
    assert_eq!(completed.files.len(), 1);
    assert_eq!(
        completed.files[0].published,
        Some(OutputKey::IdentityMetadata)
    );
    assert_eq!(completed.state.totals.pending_jobs, 0);
    assert_eq!(completed.state.totals.resident_file_objects, 1);

    let encoded_plan = metadata_plan(true);
    let (pending, io) = arm_publication(
        &encoded_plan,
        directory(&encoded_plan),
        OutputKey::IdentityMetadata,
        true,
    )
    .expect("second metadata arm");
    assert!(observe_completed_metadata(
        &encoded_plan,
        pending,
        io,
        OutputKey::EncodedProcessMetadata,
    )
    .is_err());
}
