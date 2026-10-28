//! V3 SAME-journal regressions, authored/compiled; no native proof is minted.
use super::*;
use crate::resource_custody::codec_v2::scratch::bootstrap::*;
use crate::resource_custody::codec_v2::scratch::*;
fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).unwrap()
}
fn bootstrap() -> Fixture {
    let mut f = super::scratch::growth();
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    let v2::MemberPlan::ScratchAssetsV2 { slot, plan } =
        batch.obligations[0].manifest.members[2].clone()
    else {
        unreachable!()
    };
    let mut limits = plan.limits;
    limits.live_handles = 8;
    limits.reserved_storage_bytes = 131072;
    limits.resident_memory_bytes = 131072;
    let plan = ScratchPlanV3 {
        version: 3,
        asset_id: plan.asset_id,
        rendition_id: plan.rendition_id,
        generation_epoch: plan.generation_epoch,
        filesystem_domain: plan.filesystem_domain,
        outputs: plan.outputs,
        limits,
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
    };
    let resources = &mut batch.obligations[0].resources;
    resources.scratch_bytes = plan.limits.reserved_storage_bytes;
    resources.peak_memory_bytes = plan.limits.resident_memory_bytes;
    resources.handles = u64::from(plan.limits.live_handles);
    batch.obligations[0].manifest.members[2] = v2::MemberPlan::ScratchAssetsV3 {
        slot: v2::MemberSlot {
            protected_paths: vec!["/cache/film".into()],
            ..slot
        },
        plan,
    };
    f
}
fn arm(f: &mut Fixture) -> (ObligationHandle, v2::Id, ScratchPlanV3) {
    let manifest = typed_manifest(f);
    let v2::MemberPlan::ScratchAssetsV3 { slot, plan } = &manifest.members[2] else {
        unreachable!()
    };
    let prepared = prepare(f);
    let handle = prepared.obligations[0].clone();
    let command = change_command(
        &handle,
        sequence(f),
        Mutation::Arm {
            members: vec![slot.member_id.uuid()],
        },
    );
    execute(&mut f.storage, &command, f.owner).unwrap();
    (handle, slot.member_id, plan.clone())
}
fn root_arm(
    f: &Fixture,
    handle: &ObligationHandle,
    member: v2::Id,
    plan: &ScratchPlanV3,
) -> QueuedOperation {
    let io = id();
    let mut command = change_command(
        handle,
        sequence(f),
        Mutation::AssetChangeV3 {
            change: AssetChangeV3 {
                scratch_member_id: member,
                asset_id: plan.asset_id,
                generation_epoch: plan.generation_epoch,
                previous: None,
                event: AssetEventV3::ArmBootstrapV1 {
                    io_id: io,
                    operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: id() },
                },
            },
        },
    );
    command.id = io.uuid();
    let Command::Change { key, .. } = &mut command.command else {
        unreachable!()
    };
    key.operation_id = io.uuid();
    command
}
fn result_json(f: &Fixture, operation: Uuid) -> String {
    f.storage
        .connection
        .query_row(
            "SELECT result_json FROM operations WHERE operation_id=?1",
            [operation.as_bytes()],
            |r| r.get(0),
        )
        .unwrap()
}

/// Actual SQLite/journal lineage for retirement tests. The native observations
/// are synthetic fixture data and make no physical-closure claim.
pub(in crate::resource_custody) struct FrozenSourceFixture {
    pub(in crate::resource_custody) _root: tempfile::TempDir,
    pub(in crate::resource_custody) storage: storage::Storage,
    pub(in crate::resource_custody) owner: Uuid,
    pub(in crate::resource_custody) handle: ObligationHandle,
    pub(in crate::resource_custody) member: v2::Id,
    pub(in crate::resource_custody) plan: ScratchPlanV3,
    pub(in crate::resource_custody) manifest: v2::Manifest,
    pub(in crate::resource_custody) full: FrozenScratchV3,
}

fn apply_event(
    f: &mut Fixture,
    fold: BootstrapFold,
    handle: &ObligationHandle,
    member: v2::Id,
    plan: &ScratchPlanV3,
    operation: Uuid,
    event: AssetEventV3,
) -> BootstrapFold {
    let change = AssetChangeV3 {
        scratch_member_id: member,
        asset_id: plan.asset_id,
        generation_epoch: plan.generation_epoch,
        previous: fold.state().asset.head,
        event,
    };
    let mut command = change_command(
        handle,
        sequence(f),
        Mutation::AssetChangeV3 {
            change: change.clone(),
        },
    );
    command.id = operation;
    let Command::Change { key, .. } = &mut command.command else {
        unreachable!()
    };
    key.operation_id = operation;
    execute(&mut f.storage, &command, f.owner).expect("fixture journal event");
    fold.apply(plan, v2::Id::new(operation).unwrap(), &change)
        .expect("fixture fold event")
}

fn observed_directory(
    plan: &ScratchPlanV3,
    operation: Uuid,
    io_id: v2::Id,
    copy_id: v2::Id,
) -> DirectoryObservedV1 {
    DirectoryObservedV1 {
        directory_id: v2::Id::new(operation).unwrap(),
        acquisition_io_id: io_id,
        held_copy_ids: vec![copy_id],
        native_object: crate::resource_custody::codec_v2::NativeObject {
            file_type: crate::resource_custody::codec_v2::FileType::Directory,
            device: crate::resource_custody::codec_v2::NativeValue::Known { value: 1 },
            inode: crate::resource_custody::codec_v2::NativeValue::Known { value: 42 },
            mount_id: crate::resource_custody::codec_v2::NativeValue::Known { value: 17 },
            filesystem_domain: plan.filesystem_domain.clone(),
        },
    }
}

pub(in crate::resource_custody) fn frozen_source_fixture() -> FrozenSourceFixture {
    frozen_source_fixture_for_schema(false)
}

pub(in crate::resource_custody) fn frozen_source_fixture_v6() -> FrozenSourceFixture {
    frozen_source_fixture_for_schema(true)
}

fn frozen_source_fixture_for_schema(v6: bool) -> FrozenSourceFixture {
    let mut f = bootstrap();
    if v6 {
        super::install_empty_v6_fixture(&mut f.storage);
    }
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    let v2::MemberPlan::ScratchAssetsV3 { slot, plan } =
        batch.obligations[0].manifest.members[2].clone()
    else {
        unreachable!()
    };
    let member = slot.member_id;
    let slot = v2::MemberSlot {
        member_id: member,
        owned_by: None,
        depends_on: Vec::new(),
        protected_paths: vec![plan.bootstrap().protected_child_path.clone()],
    };
    batch.obligations[0].manifest.members = vec![v2::MemberPlan::ScratchAssetsV3 {
        slot,
        plan: plan.clone(),
    }];
    batch.obligations[0].resources.handles = u64::from(plan.limits.live_handles);
    batch.obligations[0].resources.scratch_bytes = plan.limits.reserved_storage_bytes;
    batch.obligations[0].resources.peak_memory_bytes = plan.limits.resident_memory_bytes;

    let prepared = prepare(&mut f);
    let handle = prepared.obligations[0].clone();
    let manifest = typed_manifest(&f);
    let arm_member = change_command(
        &handle,
        sequence(&f),
        Mutation::Arm {
            members: vec![member.uuid()],
        },
    );
    execute(&mut f.storage, &arm_member, f.owner).expect("arm source member");
    let mut fold =
        BootstrapFold::empty(&plan, v2::Id::new(handle.obligation_id()).unwrap(), member)
            .expect("empty original fold");

    let root_io = id();
    let root_copy = id();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        root_io.uuid(),
        AssetEventV3::ArmBootstrapV1 {
            io_id: root_io,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id: root_copy },
        },
    );
    let root_observe = Uuid::new_v4();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        root_observe,
        AssetEventV3::ObserveBootstrapV1 {
            io_id: root_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(observed_directory(&plan, root_observe, root_io, root_copy)),
                closed_copy_ids: vec![],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    );

    let current_state = fold.state();
    let current = current_state.bootstrap.current.as_ref().unwrap();
    let namespace = OwnedDirectoryRefV1 {
        owning_obligation: v2::Id::new(handle.obligation_id()).unwrap(),
        member_id: member,
        directory_id: current.observed.directory_id,
    };
    let parent_io = id();
    let parent_copy = id();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        parent_io.uuid(),
        AssetEventV3::ArmBootstrapV1 {
            io_id: parent_io,
            operation: BootstrapIoV1::OpenNextComponent {
                index: 0,
                parent: namespace,
                copy_id: parent_copy,
            },
        },
    );
    let parent_observe = Uuid::new_v4();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        parent_observe,
        AssetEventV3::ObserveBootstrapV1 {
            io_id: parent_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(observed_directory(
                    &plan,
                    parent_observe,
                    parent_io,
                    parent_copy,
                )),
                closed_copy_ids: vec![root_copy],
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    );

    let configured_parent = OwnedDirectoryRefV1 {
        owning_obligation: v2::Id::new(handle.obligation_id()).unwrap(),
        member_id: member,
        directory_id: fold
            .state()
            .bootstrap
            .parent
            .as_ref()
            .unwrap()
            .observed
            .directory_id,
    };
    let child_io = id();
    let child_copy = id();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        child_io.uuid(),
        AssetEventV3::ArmBootstrapV1 {
            io_id: child_io,
            operation: BootstrapIoV1::CreateRenditionChild {
                parent: configured_parent,
                copy_id: child_copy,
            },
        },
    );
    let child_observe = Uuid::new_v4();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        child_observe,
        AssetEventV3::ObserveBootstrapV1 {
            io_id: child_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: Some(observed_directory(
                    &plan,
                    child_observe,
                    child_io,
                    child_copy,
                )),
                closed_copy_ids: vec![],
                child_effect: ChildEffect::Created,
                inventory: InventoryCertainty::Exact,
            },
        },
    );
    let fence = Uuid::new_v4();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        fence,
        AssetEventV3::OutputV2 {
            event: AssetEventV1::Fence {
                terminal_epoch: plan.generation_epoch,
            },
        },
    );
    let freeze = Uuid::new_v4();
    let exact_head_before_freeze = fold.state().asset.head.clone().unwrap();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        freeze,
        AssetEventV3::OutputV2 {
            event: AssetEventV1::Freeze {
                exact_head_before_freeze,
            },
        },
    );

    let parent_copies = fold
        .state()
        .bootstrap
        .parent
        .as_ref()
        .unwrap()
        .observed
        .held_copy_ids
        .clone();
    let close_io = id();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        close_io.uuid(),
        AssetEventV3::ArmBootstrapV1 {
            io_id: close_io,
            operation: BootstrapIoV1::CloseBootstrapHandles {
                copy_ids: parent_copies.clone(),
            },
        },
    );
    let close_observe = Uuid::new_v4();
    fold = apply_event(
        &mut f,
        fold,
        &handle,
        member,
        &plan,
        close_observe,
        AssetEventV3::ObserveBootstrapV1 {
            io_id: close_io,
            native_join_operation_id: id(),
            result: BootstrapResultV1 {
                outcome: NativeOutcomeV1::Completed,
                acquired: None,
                closed_copy_ids: parent_copies,
                child_effect: ChildEffect::None,
                inventory: InventoryCertainty::Exact,
            },
        },
    );

    let seal = change_command(&handle, sequence(&f), Mutation::Seal);
    execute(&mut f.storage, &seal, f.owner).expect("seal source");
    f.storage
        .connection
        .execute(
            "UPDATE members SET phase='acquired' WHERE obligation_id=?1 AND member_id=?2",
            rusqlite::params![handle.obligation_id().as_bytes(), member.uuid().as_bytes()],
        )
        .expect("fully acquired fixture member");
    let full = fold
        .frozen_snapshot(&plan)
        .expect("frozen source inventory");
    FrozenSourceFixture {
        _root: f._root,
        storage: f.storage,
        owner: f.owner,
        handle,
        member,
        plan,
        manifest,
        full,
    }
}
#[test]
fn scratch_v3_bootstrap_requires_member_arm_and_restores_possible_slot() {
    let mut f = bootstrap();
    let manifest = typed_manifest(&f);
    let v2::MemberPlan::ScratchAssetsV3 { slot, plan } = &manifest.members[2] else {
        unreachable!()
    };
    let prepared = prepare(&mut f);
    let handle = &prepared.obligations[0];
    let premature = root_arm(&f, handle, slot.member_id, plan);
    assert!(execute(&mut f.storage, &premature, f.owner).is_err());
    let member_arm = change_command(
        handle,
        sequence(&f),
        Mutation::Arm {
            members: vec![slot.member_id.uuid()],
        },
    );
    execute(&mut f.storage, &member_arm, f.owner).unwrap();
    let command = root_arm(&f, handle, slot.member_id, plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    let ObligationInventory::TypedV2 { members, .. } = &snapshot.obligations[0].inventory else {
        unreachable!()
    };
    let state = members
        .iter()
        .find_map(|m| match &m.envelope.observed {
            Some(v2::Observed::ScratchAssetsV3 { state }) => Some(state),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.asset.revision, 1);
    assert_eq!(state.asset.totals.pending_jobs, 1);
    assert_eq!(state.asset.totals.handles_reserved, 1);
    assert!(state.bootstrap.current.is_none());
    assert!(state.asset.directory.is_none());
}
#[test]
fn scratch_v3_lost_ack_retry_preserves_exact_event_and_original_fingerprint() {
    let mut f = bootstrap();
    let (handle, member, plan) = arm(&mut f);
    let command = root_arm(&f, &handle, member, &plan);
    let original = fingerprint(&command.command).unwrap();
    f.storage.fail_after_commit_once = true;
    assert_eq!(
        execute(&mut f.storage, &command, f.owner)
            .unwrap_err()
            .certainty,
        CommitCertainty::Unknown
    );
    let sequence_after = sequence(&f);
    let json = result_json(&f, command.id);
    execute(&mut f.storage, &command, f.owner).unwrap();
    assert_eq!(sequence(&f), sequence_after);
    assert_eq!(result_json(&f, command.id), json);
    assert_eq!(fingerprint(&command.command).unwrap(), original);
    snapshot(&f.storage.connection, &f.storage.witness).unwrap();
}
#[test]
fn scratch_v3_projection_tamper_and_missing_event_are_not_recovery_evidence() {
    let mut f = bootstrap();
    let (handle, member, plan) = arm(&mut f);
    let command = root_arm(&f, &handle, member, &plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let json = result_json(&f, command.id);
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["after"]["bootstrap"]["next_component"] = serde_json::json!(1);
    f.storage
        .connection
        .execute(
            "UPDATE operations SET result_json=?2 WHERE operation_id=?1",
            params![command.id.as_bytes(), value.to_string()],
        )
        .unwrap();
    assert!(snapshot(&f.storage.connection, &f.storage.witness).is_err());
    f.storage
        .connection
        .execute(
            "UPDATE operations SET result_json=?2 WHERE operation_id=?1",
            params![command.id.as_bytes(), json],
        )
        .unwrap();
    snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    f.storage
        .connection
        .execute(
            "DELETE FROM operations WHERE operation_id=?1",
            [command.id.as_bytes()],
        )
        .unwrap();
    assert!(snapshot(&f.storage.connection, &f.storage.witness).is_err());
}
#[test]
fn scratch_v3_unknownheld_blocks_new_root_but_allows_exact_neverentered_result() {
    let mut f = bootstrap();
    let (handle, member, plan) = arm(&mut f);
    let command = root_arm(&f, &handle, member, &plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let hold = change_command(
        &handle,
        sequence(&f),
        Mutation::Hold {
            reason: "original native outcome pending".into(),
        },
    );
    execute(&mut f.storage, &hold, f.owner).unwrap();
    let another = root_arm(&f, &handle, member, &plan);
    assert!(execute(&mut f.storage, &another, f.owner).is_err());
    let observed = change_command(
        &handle,
        sequence(&f),
        Mutation::AssetChangeV3 {
            change: AssetChangeV3 {
                scratch_member_id: member,
                asset_id: plan.asset_id,
                generation_epoch: plan.generation_epoch,
                previous: Some(AssetEventRefV1 {
                    operation_id: v2::Id::new(command.id).unwrap(),
                    revision: 1,
                }),
                event: AssetEventV3::ObserveBootstrapV1 {
                    io_id: v2::Id::new(command.id).unwrap(),
                    native_join_operation_id: id(),
                    result: BootstrapResultV1 {
                        outcome: NativeOutcomeV1::NeverEntered,
                        acquired: None,
                        closed_copy_ids: vec![],
                        child_effect: ChildEffect::None,
                        inventory: InventoryCertainty::Exact,
                    },
                },
            },
        },
    );
    execute(&mut f.storage, &observed, f.owner).unwrap();
    let snapshot = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(snapshot.obligations[0].phase, Phase::UnknownHeld);
    assert_ne!(snapshot.total, ResourceVector::default());
}
#[test]
fn scratch_v3_generic_acquired_cannot_inject_a_bootstrap_head() {
    let mut f = bootstrap();
    let (handle, member, plan) = arm(&mut f);
    let command = change_command(
        &handle,
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: v2::Envelope {
                version: 2,
                planned: typed_manifest(&f).members[2].clone(),
                observed: Some(v2::Observed::ScratchAssetsV3 {
                    state: BootstrapFold::empty(
                        &plan,
                        v2::Id::new(handle.obligation_id).unwrap(),
                        member,
                    )
                    .unwrap()
                    .state(),
                }),
            },
        },
    );
    assert!(execute(&mut f.storage, &command, f.owner).is_err());
}
#[test]
fn scratch_v3_bootstrap_reserve_and_protected_child_are_mandatory() {
    let mut f = bootstrap();
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    batch.obligations[0].resources.scratch_bytes = 1;
    assert!(execute(&mut f.storage, &f.command, f.owner).is_err());
    let mut f = bootstrap();
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    let v2::MemberPlan::ScratchAssetsV3 { slot, .. } =
        &mut batch.obligations[0].manifest.members[2]
    else {
        unreachable!()
    };
    slot.protected_paths.clear();
    assert!(execute(&mut f.storage, &f.command, f.owner).is_err());
}

#[test]
fn scratch_v3_old_decoder_rejects_new_results_without_sql_schema_change() {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)] // Frozen previous result decoder, not an execution API.
    #[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
    #[expect(
        clippy::large_enum_variant,
        reason = "This inline frozen old decoder is the compatibility fixture under test; boxing would change its historical Rust layout and add an allocation to the fixture."
    )]
    enum OldResult {
        Prepared {
            sequence: i64,
            obligations: Vec<Uuid>,
            trial: Option<Uuid>,
        },
        Changed(CommitAck),
        AssetChangedV2 {
            version: u32,
            ack: CommitAck,
            change: AssetChangeV2,
            after: ScratchStateV2,
        },
        RetiredWithAssets(RetirementAck),
    }
    let mut f = bootstrap();
    let (handle, member, plan) = arm(&mut f);
    let command = root_arm(&f, &handle, member, &plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    assert!(serde_json::from_str::<OldResult>(&result_json(&f, command.id)).is_err());
    let version: i64 = f
        .storage
        .connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    snapshot(&f.storage.connection, &f.storage.witness).unwrap();
}
