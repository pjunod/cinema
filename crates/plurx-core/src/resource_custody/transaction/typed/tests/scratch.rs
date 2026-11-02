//! Real journal tests, authored/compiled only until final candidate review.
use super::*;
use crate::resource_custody::codec_v2::scratch::*;
pub(super) fn growth() -> Fixture {
    let mut f = fixture(true);
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    let mut plan = v2::scratch::tests::plan();
    let v2::MemberPlan::ScratchAssetsV1 { slot, scratch } =
        &batch.obligations[0].manifest.members[2]
    else {
        unreachable!()
    };
    plan.filesystem_domain = scratch.directory_object.filesystem_domain.clone();
    plan.asset_id = scratch.asset_id;
    batch.obligations[0].manifest.members[2] = v2::MemberPlan::ScratchAssetsV2 {
        slot: slot.clone(),
        plan: plan.clone(),
    };
    batch.obligations[0].resources.scratch_bytes = plan.limits.reserved_storage_bytes;
    batch.obligations[0].resources.peak_memory_bytes = plan.limits.resident_memory_bytes;
    batch.obligations[0].resources.handles = u64::from(plan.limits.live_handles);
    f
}
fn arm(f: &mut Fixture) -> (ObligationHandle, v2::Id, ScratchPlanV2) {
    let manifest = typed_manifest(f);
    let v2::MemberPlan::ScratchAssetsV2 { slot, plan } = &manifest.members[2] else {
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
fn io_arm(
    f: &Fixture,
    handle: &ObligationHandle,
    member: v2::Id,
    plan: &ScratchPlanV2,
) -> QueuedOperation {
    let io = v2::Id::new(Uuid::new_v4()).unwrap();
    let mut command = change_command(
        handle,
        sequence(f),
        Mutation::AssetChangeV2 {
            change: AssetChangeV2 {
                scratch_member_id: member,
                asset_id: plan.asset_id,
                generation_epoch: plan.generation_epoch,
                previous: None,
                event: AssetEventV1::ArmIo {
                    io_id: io,
                    operation: AssetIoV1::CreateDirectory {
                        copy_id: v2::Id::new(Uuid::new_v4()).unwrap(),
                    },
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
#[test]
fn scratch_v2_directory_arm_is_refused_before_member_write_ahead() {
    let mut f = growth();
    let prepared = prepare(&mut f);
    let handle = &prepared.obligations[0];
    let manifest = typed_manifest(&f);
    let v2::MemberPlan::ScratchAssetsV2 { slot, plan } = &manifest.members[2] else {
        unreachable!()
    };
    let premature = io_arm(&f, handle, slot.member_id, plan);
    assert!(execute(&mut f.storage, &premature, f.owner).is_err());
    let member_arm = change_command(
        handle,
        sequence(&f),
        Mutation::Arm {
            members: vec![slot.member_id.uuid()],
        },
    );
    execute(&mut f.storage, &member_arm, f.owner).unwrap();
    let command = io_arm(&f, handle, slot.member_id, plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let snap = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    let ObligationInventory::TypedV2 { members, .. } = &snap.obligations[0].inventory else {
        unreachable!()
    };
    let state = members
        .iter()
        .find_map(|m| match &m.envelope.observed {
            Some(v2::Observed::ScratchAssetsV2 { state }) => Some(state),
            _ => None,
        })
        .unwrap();
    assert!(state.directory.is_none());
    assert_eq!(state.totals.pending_jobs, 1);
    assert_eq!(state.totals.handles_reserved, 1);
    assert_eq!(
        state.totals.charged_storage_bytes,
        plan.limits.directory_charge()
    );
    assert_eq!(snap.trial_groups.len(), 1);
}
#[test]
fn scratch_v2_exact_journal_retry_preserves_one_arm_and_sequence() {
    let mut f = growth();
    let (handle, member, plan) = arm(&mut f);
    let command = io_arm(&f, &handle, member, &plan);
    f.storage.fail_after_commit_once = true;
    assert_eq!(
        execute(&mut f.storage, &command, f.owner)
            .unwrap_err()
            .certainty,
        CommitCertainty::Unknown
    );
    let after = sequence(&f);
    execute(&mut f.storage, &command, f.owner).unwrap();
    assert_eq!(sequence(&f), after);
    let count: i64 = f
        .storage
        .connection
        .query_row(
            "SELECT count(*) FROM operations WHERE operation_id=?1",
            [command.id.as_bytes()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    // Journal retry is data-only; it neither invokes nor owns any native work.
    // The concrete native custodian must consume this durable arm exactly once.
}
#[test]
fn scratch_v2_missing_cross_asset_or_reordered_event_ref_refuses_restore() {
    let mut f = growth();
    let (handle, member, plan) = arm(&mut f);
    let command = io_arm(&f, &handle, member, &plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let json: String = f
        .storage
        .connection
        .query_row(
            "SELECT result_json FROM operations WHERE operation_id=?1",
            [command.id.as_bytes()],
            |r| r.get(0),
        )
        .unwrap();
    let mut result: serde_json::Value = serde_json::from_str(&json).unwrap();
    result["change"]["asset_id"] = serde_json::json!(Uuid::new_v4().to_string());
    f.storage
        .connection
        .execute(
            "UPDATE operations SET result_json=?2 WHERE operation_id=?1",
            params![command.id.as_bytes(), result.to_string()],
        )
        .unwrap();
    assert!(snapshot(&f.storage.connection, &f.storage.witness).is_err());
    let mut reordered: serde_json::Value = serde_json::from_str(&json).unwrap();
    reordered["change"]["previous"] =
        serde_json::json!({"operation_id": command.id.to_string(), "revision": 1});
    f.storage
        .connection
        .execute(
            "UPDATE operations SET result_json=?2 WHERE operation_id=?1",
            params![command.id.as_bytes(), reordered.to_string()],
        )
        .unwrap();
    assert!(snapshot(&f.storage.connection, &f.storage.witness).is_err());
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
fn scratch_v2_outer_seal_keeps_only_existing_allocator_growth_and_hold_fences_entry() {
    let mut f = growth();
    let (handle, member, plan) = arm(&mut f);
    let seal = change_command(&handle, sequence(&f), Mutation::Seal);
    execute(&mut f.storage, &seal, f.owner).unwrap();
    let command = io_arm(&f, &handle, member, &plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let hold = change_command(
        &handle,
        sequence(&f),
        Mutation::Hold {
            reason: "uncertain native owner".into(),
        },
    );
    execute(&mut f.storage, &hold, f.owner).unwrap();
    let attempted = io_arm(&f, &handle, member, &plan);
    assert!(execute(&mut f.storage, &attempted, f.owner).is_err());
    snapshot(&f.storage.connection, &f.storage.witness).unwrap();
}
#[test]
fn scratch_v2_old_reader_rejects_new_operation_and_sql_shape_is_unchanged() {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)] // Frozen old decoder fixture; successful payloads are not consumed.
    #[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
    enum OldResult {
        Prepared {
            sequence: i64,
            obligations: Vec<Uuid>,
            trial: Option<Uuid>,
        },
        Changed(CommitAck),
        RetiredWithAssets(RetirementAck),
    }
    let mut f = growth();
    let version_before: i64 = f
        .storage
        .connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let catalog_before = f
        .storage
        .connection
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let (handle, member, plan) = arm(&mut f);
    let command = io_arm(&f, &handle, member, &plan);
    execute(&mut f.storage, &command, f.owner).unwrap();
    let json: String = f
        .storage
        .connection
        .query_row(
            "SELECT result_json FROM operations WHERE operation_id=?1",
            [command.id.as_bytes()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(serde_json::from_str::<OldResult>(&json).is_err());
    let version: i64 = f
        .storage
        .connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, version_before);
    let catalog_after = f
        .storage
        .connection
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(catalog_after, catalog_before);

    let adapter: (String, i64) = f
        .storage
        .connection
        .query_row(
            "SELECT adapter,adapter_version FROM members WHERE member_id=?1",
            [member.uuid().as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(adapter, ("scratch".into(), 1));
}
