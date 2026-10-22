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
    assert_eq!(version, 2);
    snapshot(&f.storage.connection, &f.storage.witness).unwrap();
}
