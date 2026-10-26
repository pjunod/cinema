//! Audit DTO validator regressions only. These values cannot construct a
//! physical settlement, native closure or refund. Parser entry is not encoded.
use super::*;

#[test]
fn registered_vod_task_close_requires_non_nil_join_and_exact_observed_owner_inventory() {
    let (mut manifest, mut envelopes, mut settlement) = never_acquired_v3_fixture();
    let MemberPlan::VodOutputTaskV1 { task, .. } = &mut manifest.members[3] else {
        unreachable!()
    };
    task.inventory.readers = vec![
        OwnerAllocation {
            owner_id: task.stdout_owner_id,
            memory_bytes: 0,
            handles: 1,
        },
        OwnerAllocation {
            owner_id: task.diagnostic_owner_id,
            memory_bytes: 0,
            handles: 1,
        },
    ];
    task.inventory.buffers = vec![OwnerAllocation {
        owner_id: task.sink_owner_id,
        memory_bytes: 128,
        handles: 0,
    }];
    let task = task.clone();
    envelopes[3].planned = manifest.members[3].clone();
    envelopes[3].observed = Some(Observed::VodOutputTaskV1 { task: task.clone() });
    let owners = task
        .inventory
        .readers
        .iter()
        .chain(&task.inventory.buffers)
        .map(|owner| OwnerClosed {
            owner_id: owner.owner_id,
            operation_id: id(),
        })
        .collect();
    settlement.members[3] = MemberDisposition::Closed {
        member_id: task.member_id,
        fact: ClosedFact::VodOutputTaskV1 {
            task: task.clone(),
            join_operation_id: id(),
            owners,
        },
    };
    validate_manifest(&manifest).unwrap();
    validate_settlement(&manifest, &envelopes, &settlement).unwrap();

    let mut nil_join = settlement.clone();
    let MemberDisposition::Closed {
        fact: ClosedFact::VodOutputTaskV1 {
            join_operation_id, ..
        },
        ..
    } = &mut nil_join.members[3]
    else {
        unreachable!()
    };
    // Deliberately malformed in-process DTO: normal Id construction/decoding
    // already rejects nil. The settlement validator must reject it as well.
    *join_operation_id = Id(Uuid::nil());
    assert!(validate_settlement(&manifest, &envelopes, &nil_join).is_err());

    let mut missing_owner = settlement.clone();
    let MemberDisposition::Closed {
        fact: ClosedFact::VodOutputTaskV1 { owners, .. },
        ..
    } = &mut missing_owner.members[3]
    else {
        unreachable!()
    };
    owners.pop();
    assert!(validate_settlement(&manifest, &envelopes, &missing_owner).is_err());
    let mut unobserved = envelopes.clone();
    unobserved[3].observed = None;
    assert!(validate_settlement(&manifest, &unobserved, &settlement).is_err());
    let Some(Observed::VodOutputTaskV1 { task: observed }) = &mut envelopes[3].observed else {
        unreachable!()
    };
    observed.generation_epoch -= 1;
    assert!(validate_settlement(&manifest, &envelopes, &settlement).is_err());
}

#[test]
fn borrowed_plan_scope_validators_preserve_identity_and_malformed_dto_refusals() {
    let (manifest, envelopes, settlement) = never_acquired_v3_fixture();
    for envelope in &envelopes {
        validate_envelope(&manifest, envelope).unwrap();
        validate_envelope_members(&manifest.members, envelope).unwrap();
    }
    validate_settlement_scope(
        manifest.owner_id,
        manifest.graph_id,
        manifest.members.len(),
        &envelopes,
        &settlement,
    )
    .unwrap();
    for (owner, graph, count) in [
        (id(), manifest.graph_id, manifest.members.len()),
        (manifest.owner_id, id(), manifest.members.len()),
        (
            manifest.owner_id,
            manifest.graph_id,
            manifest.members.len() + 1,
        ),
    ] {
        assert!(validate_settlement_scope(owner, graph, count, &envelopes, &settlement).is_err());
    }
    let mut wrong_version = envelopes[0].clone();
    wrong_version.version = 3;
    assert!(validate_envelope_members(&manifest.members, &wrong_version).is_err());
    let mut foreign_plan = envelopes[0].clone();
    let MemberPlan::LegacyV1 { planned } = &mut foreign_plan.planned else {
        unreachable!()
    };
    planned.member_id = Uuid::new_v4();
    assert!(validate_envelope_members(&manifest.members, &foreign_plan).is_err());
    let mut duplicate = settlement.clone();
    duplicate.members[1] = duplicate.members[0].clone();
    assert!(validate_settlement_scope(
        manifest.owner_id,
        manifest.graph_id,
        manifest.members.len(),
        &envelopes,
        &duplicate
    )
    .is_err());
    // These are strict data checks only; no physical receipt is constructed.
}
