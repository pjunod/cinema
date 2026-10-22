//! Unknown inventory keeps reserved logical copies until an exact close event.
use super::*;

#[test]
fn unknown_native_inventory_keeps_the_armed_copy_and_closes_it_only_by_exact_result() {
    let plan = plan();
    let fold = BootstrapFold::empty(&plan, id(), id()).unwrap();
    let io_id = id();
    let copy = id();
    let fold = apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: copy },
        },
    )
    .unwrap();
    let fold = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Failed {
                    error_class: "inventory-poisoned".into(),
                },
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Unknown,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::UnknownHeld);
    assert_eq!(fold.state().asset.totals.handles_reserved, 1);
    assert!(fold.uncertain_copies.contains(&copy));
    let close = id();
    let fold = apply(
        fold,
        &plan,
        close,
        AssetEventV3::ArmBootstrapV1 {
            io_id: close,
            operation: BootstrapIoV1::CloseBootstrapHandles {
                copy_ids: vec![copy],
            },
        },
    )
    .unwrap();
    let fold = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id: close,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: None,
                closed_copy_ids: vec![copy],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::Closed);
    assert_eq!(fold.state().asset.totals.handles_reserved, 0);
    assert_eq!(fold.state().asset.totals.pending_jobs, 0);
    assert!(fold.output.state.directory.is_none());
}

#[test]
fn unknown_child_copy_cannot_be_reopened_under_the_same_logical_slot() {
    let plan = plan();
    let fold = parent_ready(&plan);
    let parent = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation,
        member_id: fold.member,
        directory_id: fold
            .bootstrap
            .parent
            .as_ref()
            .unwrap()
            .observed
            .directory_id,
    };
    let io_id = id();
    let copy = id();
    let fold = apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: parent.clone(),
                copy_id: copy,
            },
        },
    )
    .unwrap();
    let fold = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Failed {
                    error_class: "child-result-unknown".into(),
                },
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::Unknown,
                inventory: InventoryCertainty::Unknown,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.state().asset.totals.handles_reserved, 2);
    let resolve = id();
    assert!(
        apply(
            fold,
            &plan,
            resolve,
            AssetEventV3::ArmBootstrapV1 {
                io_id: resolve,
                operation: BootstrapIoV1::ResolvePossibleChild {
                    create_io_id: io_id,
                    parent
                },
            }
        )
        .is_err(),
        "close the uncertain actual copy before opening another descriptor in its slot"
    );
}
