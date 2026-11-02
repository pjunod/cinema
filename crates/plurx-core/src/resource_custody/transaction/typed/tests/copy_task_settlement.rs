//! U3: real SQL Prepare/Arm/Seal and its durable task envelopes. DTO
//! validation alone grants no native witness, physical settlement or refund.
use super::*;

#[test]
fn armed_unobserved_task_keeps_never_acquired_dto_eligible_until_acquisition_is_recorded() {
    let mut f = fixture(false);
    let original = typed_manifest(&f);
    let (manifest, _, settlement) = v2::tests::never_acquired_v3_fixture();
    let rebind = |json: String| {
        json.replace(
            &manifest.owner_id.uuid().to_string(),
            &original.owner_id.uuid().to_string(),
        )
        .replace(
            &manifest.graph_id.uuid().to_string(),
            &original.graph_id.uuid().to_string(),
        )
    };
    let manifest: v2::Manifest = codec::decode(&rebind(codec::encode(&manifest).unwrap())).unwrap();
    let settlement: v2::Settlement =
        codec::decode(&rebind(codec::encode(&settlement).unwrap())).unwrap();
    let v2::MemberPlan::VodOutputTaskV1 { task, .. } = &manifest.members[3] else {
        unreachable!()
    };
    let task = task.clone();
    let v2::MemberPlan::ScratchAssetsV3 { plan, .. } = &manifest.members[2] else {
        unreachable!()
    };
    let Command::PrepareTyped { batch, .. } = &mut f.command.command else {
        unreachable!()
    };
    // Exact existing fixture plan requirements; no invented native headroom.
    batch.obligations[0].resources.scratch_bytes = plan.limits.reserved_storage_bytes;
    batch.obligations[0].resources.peak_memory_bytes = plan.limits.resident_memory_bytes;
    batch.obligations[0].resources.handles = u64::from(plan.limits.live_handles);
    batch.obligations[0].manifest = manifest.clone();
    let prepared = prepare(&mut f);
    let handle = &prepared.obligations[0];
    let arm = change_command(
        handle,
        sequence(&f),
        Mutation::Arm {
            members: vec![task.member_id.uuid()],
        },
    );
    execute(&mut f.storage, &arm, f.owner).unwrap();
    let seal = change_command(handle, sequence(&f), Mutation::Seal);
    execute(&mut f.storage, &seal, f.owner).unwrap();
    let sealed = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    assert_eq!(sealed.obligations[0].phase, Phase::Sealed);
    let ObligationInventory::TypedV2 { members, .. } = &sealed.obligations[0].inventory else {
        unreachable!()
    };
    let armed = members
        .iter()
        .find(|member| member.envelope.planned.member_id() == task.member_id.uuid())
        .unwrap();
    assert_eq!(armed.phase, "acquisition_possible");
    assert!(armed.envelope.observed.is_none());
    let envelopes = manifest
        .members
        .iter()
        .map(|planned| {
            members
                .iter()
                .find(|member| member.envelope.planned == *planned)
                .unwrap()
                .envelope
                .clone()
        })
        .collect::<Vec<_>>();
    v2::validate_settlement(&manifest, &envelopes, &settlement).unwrap();
    assert_eq!(
        snapshot(&f.storage.connection, &f.storage.witness)
            .unwrap()
            .total,
        sealed.total,
        "DTO validation cannot refund the real held journal obligation"
    );

    let acquired = change_command(
        handle,
        sequence(&f),
        Mutation::RecordTypedAcquired {
            envelope: v2::Envelope {
                version: 2,
                planned: manifest.members[3].clone(),
                observed: Some(v2::Observed::VodOutputTaskV1 { task }),
            },
        },
    );
    execute(&mut f.storage, &acquired, f.owner).unwrap();
    let acquired = snapshot(&f.storage.connection, &f.storage.witness).unwrap();
    let ObligationInventory::TypedV2 { members, .. } = &acquired.obligations[0].inventory else {
        unreachable!()
    };
    let envelopes = manifest
        .members
        .iter()
        .map(|planned| {
            members
                .iter()
                .find(|member| member.envelope.planned == *planned)
                .unwrap()
                .envelope
                .clone()
        })
        .collect::<Vec<_>>();
    assert!(v2::validate_settlement(&manifest, &envelopes, &settlement).is_err());
    assert_eq!(acquired.total, sealed.total);
    assert_eq!(acquired.obligations[0].phase, Phase::Sealed);
    // Deliberately no PhysicalTypedSettlement and no SettleTyped command.
}
