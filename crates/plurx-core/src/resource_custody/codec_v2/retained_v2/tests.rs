//! Data-contract regressions; fixtures are not physical transfer receipts.
use super::*;
use scratch::bootstrap::*;
use scratch::{AssetTotalsV1, ScratchLifecycleV1, ScratchStateV2};

fn id() -> Id {
    Id::new(Uuid::new_v4()).unwrap()
}
fn directory(domain: &FilesystemDomain, copy: Id) -> scratch::DirectoryObservedV1 {
    scratch::DirectoryObservedV1 {
        directory_id: id(),
        acquisition_io_id: id(),
        held_copy_ids: vec![copy],
        native_object: NativeObject {
            file_type: FileType::Directory,
            device: NativeValue::Known { value: 1 },
            inode: NativeValue::Known { value: 42 },
            mount_id: NativeValue::Known { value: 17 },
            filesystem_domain: domain.clone(),
        },
    }
}
fn fixture() -> (Manifest, RetainedAssetsV2, FrozenScratchV3) {
    let owner_id = id();
    let old = scratch::tests::plan();
    let mut domain = old.filesystem_domain;
    domain.daemon_owner_id = owner_id;
    let plan = ScratchPlanV3 {
        version: 3,
        asset_id: old.asset_id,
        rendition_id: old.rendition_id,
        generation_epoch: old.generation_epoch,
        filesystem_domain: domain.clone(),
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
        limits: scratch::ScratchLimitsV1 {
            resident_file_objects: 8,
            resident_logical_bytes: 65536,
            reserved_storage_bytes: 131072,
            one_file_logical_bytes: 16384,
            one_io_payload_bytes: 16384,
            pending_native_jobs: 1,
            live_handles: 8,
            reader_leases: 2,
            resident_memory_bytes: 131072,
            allocation: scratch::AllocationAccountingV1::LogicalAndReportedAllocationV1 {
                allocation_unit_bytes: 4096,
                per_object_reserve_bytes: 4096,
                directory_reserve_bytes: 4096,
            },
        },
    };
    let child = directory(&domain, id());
    let parent = directory(&domain, id());
    let obligation_id = id();
    let source_member = id();
    let head = scratch::AssetEventRefV1 {
        operation_id: id(),
        revision: 1,
    };
    let full = FrozenScratchV3 {
        version: 1,
        obligation_id,
        member_id: source_member,
        state: ScratchStateV3 {
            version: 3,
            asset: ScratchStateV2 {
                version: 2,
                asset_id: plan.asset_id,
                directory: Some(child),
                head: Some(head),
                revision: 1,
                lifecycle: ScratchLifecycleV1::Frozen,
                totals: AssetTotalsV1 {
                    charged_storage_bytes: 8192,
                    memory_reserved_bytes: 4096,
                    handles_reserved: 2,
                    ..AssetTotalsV1::default()
                },
            },
            bootstrap: BootstrapStateV1 {
                phase: BootstrapPhase::SinkReady,
                next_component: 1,
                current: None,
                parent: Some(BootstrapDirectoryV1 {
                    observed: parent,
                    kind: BootstrapDirectoryKind::ConfiguredParentV1,
                    component_index: Some(0),
                }),
                possible_child: None,
            },
        },
        files: vec![],
        readers: vec![],
    };
    let reference = frozen_snapshot_ref(&plan, &full).unwrap();
    let target_member = id();
    let target_graph = id();
    let manifest = Manifest {
        version: 2,
        graph_id: target_graph,
        owner_id,
        attempt_id: None,
        members: vec![MemberPlan::ScratchAssetsV3 {
            slot: MemberSlot {
                member_id: target_member,
                owned_by: None,
                depends_on: vec![],
                protected_paths: vec![plan.bootstrap().protected_child_path.clone()],
            },
            plan: plan.clone(),
        }],
    };
    let custody = RetainedAssetsV2 {
        version: 2,
        asset_id: plan.asset_id,
        rendition_id: plan.rendition_id.clone(),
        generation_epoch: plan.generation_epoch,
        producer_obligation_id: obligation_id,
        producer_graph_id: id(),
        transfer_operation_id: id(),
        target_obligation_id: id(),
        target_graph_id: target_graph,
        target_owner_id: owner_id,
        transfer: MemberTransfer {
            source_member_id: source_member,
            target_member_id: target_member,
            identity: TransferIdentity::ScratchAssetsV3 {
                plan,
                snapshot: reference,
            },
        },
        reader_ids: vec![],
        reconciliation: Reconciliation::LiveOwned,
    };
    (manifest, custody, full)
}
#[test]
fn retained_v2_requires_original_parent_charge_and_exact_owner_ids() {
    let (manifest, custody, full) = fixture();
    let minimum = retained_minimum_resources_v2(&manifest, &custody, &full).unwrap();
    assert_eq!(minimum.scratch_bytes, 8192);
    assert_eq!(minimum.handles, 2);
    assert_eq!(minimum.peak_memory_bytes, 4096);
    let mut undercounted = custody.clone();
    let TransferIdentity::ScratchAssetsV3 { snapshot, .. } = &mut undercounted.transfer.identity
    else {
        unreachable!()
    };
    snapshot.state.asset.totals.charged_storage_bytes = 4096;
    assert!(retained_minimum_resources_v2(&manifest, &undercounted, &full).is_err());
    let mut foreign = custody.clone();
    foreign.producer_obligation_id = id();
    assert!(validate_retained_assets_v2(&manifest, &foreign).is_err());
}
#[test]
fn retained_v2_rejects_processing_tax_and_keeps_v1_transfer_tag() {
    let (manifest, custody, full) = fixture();
    let minimum = retained_minimum_resources_v2(&manifest, &custody, &full).unwrap();
    validate_retained_resources_v2(&manifest, &custody, &full, minimum).unwrap();
    let taxed = ResourceVector {
        cpu_ns_per_second: 1,
        ..minimum
    };
    assert!(validate_retained_resources_v2(&manifest, &custody, &full, taxed).is_err());
    let previous = super::super::tests::fixture();
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &previous.members[2] else {
        unreachable!()
    };
    let old = TransferIdentity::ScratchAssetsV1 {
        scratch: scratch.clone(),
    };
    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        format!(
            "{{\"kind\":\"scratch_assets_v1\",\"scratch\":{}}}",
            serde_json::to_string(scratch).unwrap()
        )
    );
}
