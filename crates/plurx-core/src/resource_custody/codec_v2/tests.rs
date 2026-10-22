//! Authored typed ownership regressions. No tests run before final review.
use super::*;
fn id() -> Id {
    Id::new(Uuid::new_v4()).unwrap()
}
fn inventory() -> IoInventory {
    IoInventory {
        pending_io: vec![],
        buffers: vec![],
        readers: vec![],
    }
}
pub(in crate::resource_custody) fn fixture() -> Manifest {
    let owner_id = id();
    let graph_id = id();
    let root = id();
    let source_member = id();
    let asset_id = id();
    let scratch_member = id();
    let task_member = id();
    let domain = FilesystemDomain {
        platform: FilesystemPlatform::Linux,
        domain_id: id(),
        daemon_owner_id: owner_id,
        os_boot: BootIdentity::Unknown {
            reason: "fixture has no restart authority".into(),
        },
        mount_namespace: NativeValue::Unknown {
            reason: "not acquired".into(),
        },
    };
    let source = SourcePlan {
        version: 1,
        holder_id: id(),
        member_id: source_member,
        source_association: SourceAssociation {
            media_file_id: "catalogue-file".into(),
            object_version: "source-version".into(),
            recipe_source_binding: "software-recipe-v1".into(),
        },
        access: SourceAccess::ReadOnlyRegular,
        filesystem_domain: domain.clone(),
        planned_copies: vec![SourceCopy {
            copy_id: id(),
            owner: CopyOwner::DaemonSourceCell {
                owner_id,
                graph_id,
                member_id: source_member,
            },
            acquisition_operation_id: id(),
            parent_copy_id: None,
        }],
        recovery: SourceRecovery::LocalSourceHandlesV1 {
            owner_id,
            graph_id,
            filesystem_domain: domain.clone(),
        },
    };
    let native = NativeObject {
        file_type: FileType::Directory,
        device: NativeValue::Known { value: u64::MAX },
        inode: NativeValue::Known { value: u64::MAX },
        mount_id: NativeValue::Unknown {
            reason: "not acquired".into(),
        },
        filesystem_domain: domain,
    };
    let slot = |member_id| MemberSlot {
        member_id,
        owned_by: None,
        depends_on: vec![],
        protected_paths: vec![],
    };
    Manifest {
        version: 2,
        graph_id,
        owner_id,
        attempt_id: None,
        members: vec![
            MemberPlan::LegacyV1 {
                planned: PlannedMember {
                    member_id: root.uuid(),
                    kind: MemberKind::Root,
                    adapter: super::super::Adapter::ProcessRoot,
                    adapter_version: 1,
                    domain: "trusted-software-fixture".into(),
                    locator: "fixed root slot".into(),
                    recovery: super::super::RecoveryMethod::LiveOnly {
                        reason: "not a hostile-descendant proof".into(),
                    },
                    owned_by: None,
                    depends_on: vec![],
                    maximum_cardinality: 1,
                    protected_paths: vec![],
                },
            },
            MemberPlan::SourceV1 {
                slot: slot(source_member),
                source: source.clone(),
            },
            MemberPlan::ScratchAssetsV1 {
                slot: MemberSlot {
                    protected_paths: vec!["/fixture/output".into()],
                    ..slot(scratch_member)
                },
                scratch: ScratchAssets {
                    version: 1,
                    asset_id,
                    directory_path: "/fixture/output".into(),
                    directory_object: native,
                    files: vec![],
                    inventory: inventory(),
                },
            },
            MemberPlan::VodOutputTaskV1 {
                slot: slot(task_member),
                task: VodOutputTask {
                    version: 1,
                    actor: VodActor::VodGenerationV1,
                    graph_id,
                    member_id: task_member,
                    root_member_id: root,
                    generation_epoch: u64::MAX,
                    source_association: source.source_association,
                    stdout_owner_id: id(),
                    diagnostic_owner_id: id(),
                    asset_id,
                    sink_owner_id: id(),
                    inventory: inventory(),
                },
            },
        ],
    }
}
#[test]
fn v2_source_and_task_roundtrip_preserves_u64_and_unknown_native_identity() {
    let manifest = fixture();
    validate_manifest(&manifest).unwrap();
    let json = codec::encode(&manifest).unwrap();
    assert_eq!(
        decode_manifest(&json).unwrap(),
        ManifestDocument::V2(manifest)
    );
    assert!(codec::decode::<super::super::Manifest>(&json).is_err());
}
#[test]
fn v2_canonical_uuid_unknown_fields_future_versions_and_oversize_refuse() {
    let manifest = fixture();
    let mut value = serde_json::to_value(&manifest).unwrap();
    value["owner_id"] =
        serde_json::Value::String(manifest.owner_id.uuid().to_string().to_uppercase());
    // Use a fixed UUID with alphabetic digits so the case test cannot be vacuous.
    value["owner_id"] = serde_json::json!("A0000000-0000-4000-8000-000000000001");
    assert!(decode_manifest(&value.to_string()).is_err());
    let mut value = serde_json::to_value(&manifest).unwrap();
    value["extra"] = serde_json::json!(1);
    assert!(decode_manifest(&value.to_string()).is_err());
    let mut future = manifest.clone();
    future.version = 3;
    assert!(decode_manifest(&codec::encode(&future).unwrap()).is_err());
    assert!(decode_manifest(&"x".repeat(codec::JSON_LIMIT + 1)).is_err());
    assert!(codec::decode::<Id>("\"00000000-0000-0000-0000-000000000000\"").is_err());
}
#[test]
fn v2_source_copy_cycle_or_wrong_owner_is_not_a_shared_holder() {
    let mut manifest = fixture();
    let MemberPlan::SourceV1 { source, .. } = &mut manifest.members[1] else {
        unreachable!()
    };
    source.planned_copies[0].parent_copy_id = Some(source.planned_copies[0].copy_id);
    assert!(validate_manifest(&manifest).is_err());
    let mut manifest = fixture();
    let MemberPlan::SourceV1 { source, .. } = &mut manifest.members[1] else {
        unreachable!()
    };
    source.filesystem_domain.daemon_owner_id = id();
    assert!(validate_manifest(&manifest).is_err());
}
#[test]
fn v2_observed_source_must_match_holder_association_domain_and_actual_copy_slots() {
    let manifest = fixture();
    let planned = manifest.members[1].clone();
    let MemberPlan::SourceV1 { source, .. } = &planned else {
        unreachable!()
    };
    let observed = SourceObserved {
        version: 1,
        holder_id: source.holder_id,
        member_id: source.member_id,
        copies: source.planned_copies.clone(),
        native_object: NativeObject {
            file_type: FileType::Regular,
            device: NativeValue::Known { value: u64::MAX },
            inode: NativeValue::Known { value: u64::MAX },
            mount_id: NativeValue::Unknown {
                reason: "fixture unknown mount".into(),
            },
            filesystem_domain: source.filesystem_domain.clone(),
        },
        access: source.access.clone(),
        source_association: source.source_association.clone(),
    };
    let envelope = Envelope {
        version: 2,
        planned,
        observed: Some(Observed::SourceV1 { source: observed }),
    };
    validate_envelope(&manifest, &envelope).unwrap();
    let mut bad = envelope.clone();
    let Some(Observed::SourceV1 { source }) = &mut bad.observed else {
        unreachable!()
    };
    source.holder_id = id();
    assert!(validate_envelope(&manifest, &bad).is_err());
    let mut bad = envelope.clone();
    let Some(Observed::SourceV1 { source }) = &mut bad.observed else {
        unreachable!()
    };
    source.copies[0].acquisition_operation_id = id();
    assert!(validate_envelope(&manifest, &bad).is_err());
}
#[test]
fn v2_task_cannot_name_another_source_or_unowned_asset() {
    let mut manifest = fixture();
    let MemberPlan::VodOutputTaskV1 { task, .. } = &mut manifest.members[3] else {
        unreachable!()
    };
    task.source_association.object_version = "different-object".into();
    assert!(validate_manifest(&manifest).is_err());
    let mut manifest = fixture();
    let MemberPlan::VodOutputTaskV1 { task, .. } = &mut manifest.members[3] else {
        unreachable!()
    };
    task.asset_id = id();
    assert!(validate_manifest(&manifest).is_err());
}
#[test]
fn v2_assets_are_cleanup_only_not_a_producer_family_alias() {
    let manifest = fixture();
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &manifest.members[2] else {
        unreachable!()
    };
    let custody = RetainedAssets {
        version: 1,
        asset_id: scratch.asset_id,
        rendition_id: "rendition".into(),
        generation_epoch: u64::MAX,
        producer_obligation_id: id(),
        producer_graph_id: id(),
        transfer_operation_id: id(),
        target_obligation_id: id(),
        target_graph_id: manifest.graph_id,
        target_owner_id: manifest.owner_id,
        transfers: vec![],
        directory_path: scratch.directory_path.clone(),
        directory_object: scratch.directory_object.clone(),
        reader_ids: vec![],
        reconciliation: Reconciliation::LiveOwned,
    };
    assert!(validate_retained_assets(&manifest, &custody).is_err());
}

#[test]
fn retained_cost_covers_real_directory_reader_buffer_and_allocated_file_owners() {
    use crate::adaptive_resources::ResourceVector;
    let mut manifest = fixture();
    manifest.members = vec![manifest.members[2].clone()];
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &mut manifest.members[0] else {
        unreachable!()
    };
    scratch.inventory.readers.push(OwnerAllocation {
        owner_id: id(),
        memory_bytes: 8192,
        handles: 3,
    });
    let mut native = scratch.directory_object.clone();
    native.file_type = FileType::Regular;
    scratch.files.push(AssetFile {
        file_id: id(),
        relative_path: "media/0.m4s".into(),
        native_object: native,
        allocated_bytes: 4096,
        charged_bytes: 4096,
    });
    let minimum = retained_minimum_resources(&manifest).unwrap();
    assert_eq!(
        minimum,
        ResourceVector {
            peak_memory_bytes: 8192,
            handles: 4,
            scratch_bytes: 4096,
            ..ResourceVector::default()
        }
    );
    validate_retained_resources(&manifest, minimum).unwrap();
    assert!(validate_retained_resources(
        &manifest,
        ResourceVector {
            handles: 3,
            ..minimum
        }
    )
    .is_err());
    assert!(validate_retained_resources(
        &manifest,
        ResourceVector {
            scratch_bytes: 0,
            ..minimum
        }
    )
    .is_err());
    assert!(validate_retained_resources(
        &manifest,
        ResourceVector {
            cpu_ns_per_second: 1,
            ..minimum
        }
    )
    .is_err());
}

#[test]
fn pending_output_io_cannot_be_reclassified_as_retired_assets() {
    let mut manifest = fixture();
    manifest.members = vec![manifest.members[2].clone()];
    let MemberPlan::ScratchAssetsV1 { scratch, .. } = &mut manifest.members[0] else {
        unreachable!()
    };
    scratch.inventory.pending_io.push(OwnerAllocation {
        owner_id: id(),
        memory_bytes: 1,
        handles: 1,
    });
    assert!(retained_minimum_resources(&manifest).is_err());
}
