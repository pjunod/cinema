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
