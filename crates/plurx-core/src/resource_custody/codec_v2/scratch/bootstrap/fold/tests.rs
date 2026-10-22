//! Authored deterministic audit regressions; COMPILE ONLY, not native evidence.
use super::*;
mod unknown;
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
fn apply(
    fold: BootstrapFold,
    plan: &ScratchPlanV3,
    operation: Id,
    event: AssetEventV3,
) -> Result<BootstrapFold, CustodyError> {
    let change = AssetChangeV3 {
        scratch_member_id: fold.member,
        asset_id: plan.asset_id,
        generation_epoch: plan.generation_epoch,
        previous: fold.output.state.head.clone(),
        event,
    };
    fold.apply(plan, operation, &change)
}
fn directory(plan: &ScratchPlanV3, operation: Id, io_id: Id, copy: Id) -> DirectoryObservedV1 {
    DirectoryObservedV1 {
        directory_id: operation,
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
    acquired: DirectoryObservedV1,
    closed: Vec<Id>,
    child: ChildEffect,
) -> BootstrapFold {
    let operation = acquired.directory_id;
    apply(
        fold,
        plan,
        operation,
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(acquired),
                closed_copy_ids: closed,
                child_effect: child,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap()
}
fn parent_ready(plan: &ScratchPlanV3) -> BootstrapFold {
    let mut fold = BootstrapFold::empty(plan, id(), id()).unwrap();
    let io_id = id();
    let copy = id();
    fold = apply(
        fold,
        plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: copy },
        },
    )
    .unwrap();
    let op = id();
    fold = observe(
        fold,
        plan,
        io_id,
        directory(plan, op, io_id, copy),
        vec![],
        ChildEffect::None,
    );
    let parent = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation,
        member_id: fold.member,
        directory_id: op,
    };
    let next = id();
    let next_copy = id();
    fold = apply(
        fold,
        plan,
        next,
        AssetEventV3::ArmBootstrapV1 {
            io_id: next,
            operation: BootstrapIoV1::OpenNextComponent {
                index: 0,
                parent,
                copy_id: next_copy,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.state().asset.totals.handles_reserved, 2);
    observe(
        fold,
        plan,
        next,
        directory(plan, id(), next, next_copy),
        vec![copy],
        ChildEffect::None,
    )
}
#[test]
fn bootstrap_plan_has_no_parent_identity_and_rejects_noncanonical_root_authority() {
    let plan = plan();
    validate_plan(&plan).unwrap();
    let json = codec::encode(&plan).unwrap();
    assert!(!json.contains("parent_binding") && !json.contains("native_object"));
    assert_eq!(codec::decode::<ScratchPlanV3>(&json).unwrap(), plan);
    for name in [".", "..", "", "a/b", "a\\b", "a\0b"] {
        let mut bad = plan.clone();
        let DirectoryOriginV1::ConfiguredRootBootstrapV1 { bootstrap } = &mut bad.directory;
        bootstrap.components = vec![name.into()];
        assert!(validate_plan(&bad).is_err());
    }
    for path in ["/", "cache", "/cache/", "//cache", "/./cache", "/foreign"] {
        let mut bad = plan.clone();
        let DirectoryOriginV1::ConfiguredRootBootstrapV1 { bootstrap } = &mut bad.directory;
        bootstrap.authority.resolved_absolute_root = path.into();
        assert!(validate_plan(&bad).is_err());
    }
}
#[test]
fn bootstrap_path_and_combined_storage_handle_bounds_refuse_before_event_entry() {
    let plan = plan();
    for adjust in 0..5 {
        let mut bad = plan.clone();
        let DirectoryOriginV1::ConfiguredRootBootstrapV1 { bootstrap } = &mut bad.directory;
        match adjust {
            0 => bootstrap.components = vec!["x".into(); 65],
            1 => bootstrap.components = vec!["x".repeat(256)],
            2 => bootstrap.bounds.parent_entry_reserve_bytes = u64::MAX,
            3 => bad.limits.live_handles = 2,
            _ => bootstrap.bounds.resident_memory_bytes = bad.limits.resident_memory_bytes + 1,
        }
        assert!(BootstrapFold::empty(&bad, id(), id()).is_err());
    }
}
#[test]
fn bootstrap_requires_known_parent_before_exclusive_child_and_one_native_slot() {
    let plan = plan();
    let fold = BootstrapFold::empty(&plan, id(), id()).unwrap();
    let io_id = id();
    assert!(apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: OwnedDirectoryRefV1 {
                    owning_obligation: id(),
                    member_id: id(),
                    directory_id: id()
                },
                copy_id: id(),
            }
        }
    )
    .is_err());
    let fold = BootstrapFold::empty(&plan, id(), id()).unwrap();
    let fold = apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: id() },
        },
    )
    .unwrap();
    let next = id();
    assert!(apply(
        fold,
        &plan,
        next,
        AssetEventV3::ArmBootstrapV1 {
            io_id: next,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: id() }
        }
    )
    .is_err());
}
#[test]
fn bootstrap_success_keeps_base_and_child_and_single_output_history_without_fake_v2_parent() {
    let plan = plan();
    let mut fold = parent_ready(&plan);
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::ParentReady);
    assert!(fold.output.state.directory.is_none() && fold.bootstrap.current.is_none());
    let base = fold
        .bootstrap
        .parent
        .as_ref()
        .unwrap()
        .observed
        .directory_id;
    let io_id = id();
    let copy = id();
    let parent = OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation,
        member_id: fold.member,
        directory_id: base,
    };
    fold = apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent,
                copy_id: copy,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.state().asset.totals.handles_reserved, 2);
    assert_eq!(fold.state().asset.totals.charged_storage_bytes, 8192);
    fold = observe(
        fold,
        &plan,
        io_id,
        directory(&plan, id(), io_id, copy),
        vec![],
        ChildEffect::Created,
    );
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::SinkReady);
    assert_eq!(fold.state().asset.totals.handles_reserved, 2);
    assert_eq!(fold.state().asset.revision, 6);
    let io_id = id();
    assert!(
        apply(
            fold,
            &plan,
            io_id,
            AssetEventV3::OutputV2 {
                event: AssetEventV1::ArmIo {
                    io_id,
                    operation: AssetIoV1::CreateDirectory { copy_id: id() },
                }
            }
        )
        .is_err(),
        "V3 cannot smuggle a second child through V2 output events"
    );
}
#[test]
fn entered_partial_mkdir_is_held_and_never_replayed_as_never_entered() {
    let plan = plan();
    let mut fold = parent_ready(&plan);
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
    fold = apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent,
                copy_id: id(),
            },
        },
    )
    .unwrap();
    fold = apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Failed {
                    error_class: "child-open-after-mkdir".into(),
                },
                acquired: None,
                closed_copy_ids: vec![],
                child_effect: ChildEffect::Created,
                inventory: InventoryCertainty::Exact,
            },
        },
    )
    .unwrap();
    assert_eq!(fold.bootstrap.phase, BootstrapPhase::UnknownHeld);
    assert!(fold.bootstrap.possible_child.is_some());
    assert_eq!(fold.state().asset.totals.charged_storage_bytes, 8192);
    assert_eq!(fold.state().asset.totals.handles_reserved, 1);
    assert_eq!(fold.state().asset.totals.pending_jobs, 0);
}
#[test]
fn bootstrap_never_entered_cannot_erase_an_actual_acquired_descriptor() {
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
    assert!(apply(
        fold,
        &plan,
        id(),
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::NeverEntered,
                acquired: Some(directory(&plan, id(), io_id, copy)),
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            }
        }
    )
    .is_err());
}
#[test]
fn bootstrap_same_device_is_not_same_mount_and_foreign_refs_do_not_borrow_a_base() {
    let plan = plan();
    let fold = parent_ready(&plan);
    let io_id = id();
    assert!(apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: OwnedDirectoryRefV1 {
                    owning_obligation: id(),
                    member_id: id(),
                    directory_id: id()
                },
                copy_id: id(),
            }
        }
    )
    .is_err());
    let fold = parent_ready(&plan);
    let copy = id();
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
    let fold = apply(
        fold,
        &plan,
        io_id,
        AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent,
                copy_id: copy,
            },
        },
    )
    .unwrap();
    let op = id();
    let mut acquired = directory(&plan, op, io_id, copy);
    acquired.native_object.mount_id = NativeValue::Known { value: 18 };
    assert!(apply(
        fold,
        &plan,
        op,
        AssetEventV3::ObserveBootstrapV1 {
            io_id,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(acquired),
                closed_copy_ids: vec![],
                child_effect: ChildEffect::Created,
                inventory: InventoryCertainty::Exact,
            }
        }
    )
    .is_err());
}
