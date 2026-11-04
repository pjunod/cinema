//! Real operation-book ownership, independently of census completeness.
use super::*;
use crate::adaptive_resources::owner_inventory::{
    InventoryError, InventoryIdentity, OwnerFamily, OwnerKind, OwnerPhase, MAX_INVENTORY_OWNERS,
};

#[tokio::test]
async fn original_row_precedes_entry_and_dropped_selection_keeps_it_owned() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).unwrap();
    let binding = owner.state.lock().await.roles[0].binding;
    let snapshot = runtime.affected_owner_inventory().snapshot().unwrap();
    assert_eq!(snapshot.records().count(), 1);
    assert_eq!(
        snapshot.records().next().unwrap().identity,
        InventoryIdentity::Reservation(binding)
    );
    assert!(
        !snapshot.completeness().is_complete(),
        "selected registration cannot grant wider adapter coverage"
    );
    drop(owner);
    let owner = runtime.retained_preparations().unwrap().pop().unwrap();
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    let (enter, entered) = tokio::sync::oneshot::channel();
    runtime.release_native_entry(binding, None, enter).unwrap();
    entered.await.unwrap();
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        1
    );
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        1,
        "native and ledger closure do not destroy published application owners"
    );
    runtime.forget_preparation(owner).await.unwrap();
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        0
    );
    runtime.drain().unwrap();
}

#[tokio::test]
async fn final_bundle_alias_blocks_census_retirement_after_baseline_refund() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).unwrap();
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    let alias = owner.clone();
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    assert_eq!(
        runtime.accounting().unwrap().held,
        0,
        "existing never-entry ledger behavior"
    );
    let snapshot = runtime.affected_owner_inventory().snapshot().unwrap();
    assert_eq!(snapshot.records().count(), 1);
    assert_eq!(
        snapshot.records().next().unwrap().phase,
        OwnerPhase::UnknownHeld
    );
    assert!(
        runtime.forget_preparation(owner).await.is_err(),
        "the actual rich bundle is still aliased"
    );
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        1
    );
    drop(alias);
    let retained = runtime.retained_preparations().unwrap().pop().unwrap();
    runtime.forget_preparation(retained).await.unwrap();
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        0
    );
    runtime.drain().unwrap();
}

#[tokio::test]
async fn census_exhaustion_preserves_ordinary_entry_but_never_completeness() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let mut unrelated = Vec::new();
    for _ in 0..MAX_INVENTORY_OWNERS {
        unrelated.push(
            runtime
                .affected_owner_inventory()
                .register_original(
                    Uuid::new_v4(),
                    OwnerFamily::LiveTransport,
                    OwnerKind::Producer,
                )
                .unwrap(),
        );
    }
    let owner = prepare(&runtime, vec![plan()], deadline())
        .expect("baseline ledger admits beyond census slots");
    let binding = {
        let bundle = owner.state.lock().await;
        assert!(matches!(
            bundle.roles[0].inventory,
            Some(RuntimeInventoryOwner::Overflow { .. })
        ));
        bundle.roles[0].binding
    };
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    let (enter, entered) = tokio::sync::oneshot::channel();
    runtime
        .release_native_entry(binding, None, enter)
        .expect("ordinary entry retains original overflow selector");
    entered.await.unwrap();
    let snapshot = runtime.affected_owner_inventory().snapshot().unwrap();
    assert!(snapshot.completeness().overflowed);
    assert!(!snapshot.completeness().is_complete());
    assert!(matches!(
        runtime
            .affected_owner_inventory()
            .with_current_complete_snapshot(&snapshot, |_| panic!(
                "incomplete census must not execute a trial reservation"
            )),
        Err(InventoryError::Incomplete)
    ));
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    runtime.forget_preparation(owner).await.unwrap();
    assert!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .completeness()
            .overflowed,
        "closing an overflow owner never erases uncertainty"
    );
    drop(unrelated);
    runtime.drain().unwrap();
}

#[tokio::test]
async fn losing_original_token_refuses_latch_and_keeps_hidden_charge() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).unwrap();
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    let binding = {
        let mut bundle = owner.state.lock().await;
        drop(bundle.roles[0].inventory.take());
        bundle.roles[0].binding
    };
    let (enter, entered) = tokio::sync::oneshot::channel();
    assert!(runtime.release_native_entry(binding, None, enter).is_err());
    assert!(entered.await.is_err());
    let snapshot = runtime.affected_owner_inventory().snapshot().unwrap();
    assert_eq!(snapshot.records().count(), 1);
    assert_eq!(
        snapshot.records().next().unwrap().phase,
        OwnerPhase::UnknownHeld
    );
    assert_eq!(runtime.accounting().unwrap().held, 1);
    runtime.drain().unwrap();
}

#[tokio::test]
async fn dropped_preparation_waiter_preserves_original_census_and_operation() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).unwrap();
    let state = Arc::clone(&owner.state);
    let blocked = state.lock().await;
    let binding = blocked.roles[0].binding;
    let mut waiter = Box::pin(runtime.resolve_preparation(&owner, deadline()));
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut waiter)
        .await
        .is_err());
    drop(waiter);
    drop(blocked);
    drop(state);
    drop(owner);
    let census = runtime.affected_owner_inventory().snapshot().unwrap();
    assert_eq!(census.records().count(), 1);
    assert_eq!(
        census.records().next().unwrap().identity,
        InventoryIdentity::Reservation(binding)
    );
    assert_eq!(runtime.accounting().unwrap().held, 1);
    let owner = runtime.retained_preparations().unwrap().pop().unwrap();
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    runtime.forget_preparation(owner).await.unwrap();
    assert_eq!(
        runtime
            .affected_owner_inventory()
            .snapshot()
            .unwrap()
            .records()
            .count(),
        0
    );
    runtime.drain().unwrap();
}

#[tokio::test]
async fn later_typed_construction_failure_does_not_accumulate_overflow_lookups() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let mut originals = Vec::new();
    for _ in 0..MAX_INVENTORY_OWNERS {
        originals.push(
            runtime
                .affected_owner_inventory()
                .register_original(
                    Uuid::new_v4(),
                    OwnerFamily::LiveTransport,
                    OwnerKind::Producer,
                )
                .unwrap(),
        );
    }
    let family = runtime.new_family().unwrap();
    // The actual fixed P/M admission shape permits one unknown native
    // producer and one heap-only metadata survivor. Two process-bearing
    // fixture roles would correctly fail unknown-task overlap first.
    let claims = [
        MemoryClaim::UnknownProducerPeak,
        MemoryClaim::PlanningMetadata,
    ];
    let roles = [
        FamilyRoleDemand {
            role: FamilyRole::Video,
            demand: WorkloadDemand {
                resources: Metric::Known(vector(2)),
                limit_epoch: 1,
                trial: false,
            },
            retained: None,
        },
        FamilyRoleDemand {
            role: FamilyRole::Companion,
            demand: WorkloadDemand {
                resources: Metric::Known(ResourceVector {
                    peak_memory_bytes: 2,
                    ..ResourceVector::default()
                }),
                limit_epoch: 1,
                trial: false,
            },
            retained: None,
        },
    ];
    let before = runtime.operations.lock().unwrap().inventory_overflow.len();
    for _ in 0..3 {
        let member_id = v2::Id::new(Uuid::new_v4()).unwrap();
        let plans = vec![
            TypedGraphPlan {
                attempt_id: None,
                members: plan()
                    .members
                    .into_iter()
                    .map(TypedMemberPlan::Legacy)
                    .collect(),
            },
            TypedGraphPlan {
                attempt_id: Some(Uuid::nil()),
                members: vec![TypedMemberPlan::FixedProbeMetadata {
                    slot: v2::MemberSlot {
                        member_id,
                        owned_by: None,
                        depends_on: Vec::new(),
                        protected_paths: Vec::new(),
                    },
                    metadata: v2::FixedProbeMetadataV1 {
                        version: 1,
                        member_id,
                        metadata_id: v2::Id::new(Uuid::new_v4()).unwrap(),
                        retained_memory_bytes: 2,
                    },
                }],
            },
        ];
        // The first role has already obtained its overflow selector when the
        // second role rejects its actual invalid typed attempt identity. No
        // prepare is enqueued, and no replacement census record is fabricated.
        let result = runtime.prepare_typed_families_claiming(
            &[FamilyDemand {
                identity: family,
                roles: &roles,
            }],
            plans,
            Some(&claims),
            deadline(),
        );
        let error = result
            .err()
            .expect("invalid second typed attempt must refuse construction");
        assert!(
            matches!(error, OperationError::Custody(_)),
            "unexpected refusal before the second constructor: {error:?}"
        );
        assert_eq!(
            runtime.operations.lock().unwrap().inventory_overflow.len(),
            before
        );
        let accounting = runtime.accounting().unwrap();
        assert_eq!(accounting.held, 0);
        assert_eq!(accounting.used, ResourceVector::default());
        let census = runtime.affected_owner_inventory().snapshot().unwrap();
        assert_eq!(census.records().count(), MAX_INVENTORY_OWNERS);
        assert!(census.completeness().overflowed);
        assert!(!census.completeness().is_complete());
    }
    drop(originals);
    runtime.drain().unwrap();
}
