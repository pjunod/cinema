//! Shared runtime typed preparation; authored/compiled, not yet executed.
#![cfg(unix)]
use super::*;

fn typed_plan() -> TypedGraphPlan {
    TypedGraphPlan {
        attempt_id: Some(Uuid::new_v4()),
        members: plan()
            .members
            .into_iter()
            .map(|planned| v2::MemberPlan::LegacyV1 { planned })
            .collect(),
    }
}
fn prepare_typed(
    runtime: &NodeResourceRuntime,
    plans: Vec<TypedGraphPlan>,
    trial: bool,
) -> OwnedPreparation {
    let roles: Vec<_> = (0..plans.len())
        .map(|i| FamilyRoleDemand {
            role: [FamilyRole::Video, FamilyRole::Audio, FamilyRole::Companion][i],
            demand: WorkloadDemand {
                resources: Metric::Known(vector(2)),
                limit_epoch: 1,
                trial,
            },
            retained: None,
        })
        .collect();
    runtime
        .prepare_typed_families(
            &[FamilyDemand {
                identity: runtime.new_family().unwrap(),
                roles: &roles,
            }],
            plans,
            deadline(),
        )
        .unwrap()
}

#[tokio::test]
async fn typed_prepare_uses_actual_graph_identity_and_one_retained_runtime_charge() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let plan = typed_plan();
    let members = plan.members.clone();
    let owner = prepare_typed(&runtime, vec![plan], false);
    assert_eq!(runtime.accounting().unwrap().used, vector(2));
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    {
        let bundle = owner.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(role.typed_members.as_ref(), Some(&members));
        let identity = role.graph.as_ref().unwrap().identity();
        assert_eq!(identity.process_instance, runtime.owner.owner_id);
        assert_eq!(
            identity.generation,
            role.handle.as_ref().unwrap().graph_id()
        );
        assert!(role.armed.is_empty() && role.invoked.is_empty());
    }
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    runtime.forget_preparation(owner).await.unwrap();
    runtime.drain().unwrap();
}

#[tokio::test]
async fn dropped_typed_prepare_selection_preserves_every_role_until_cancel_commit() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    drop(prepare_typed(
        &runtime,
        vec![typed_plan(), typed_plan()],
        false,
    ));
    assert_eq!(runtime.accounting().unwrap().used, vector(4));
    let owner = runtime.retained_preparations().unwrap().pop().unwrap();
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    {
        let bundle = owner.state.lock().await;
        assert_eq!(bundle.roles.len(), 2);
        assert_eq!(bundle.roles[0].sequence, bundle.roles[1].sequence);
        assert_ne!(
            bundle.roles[0].handle.as_ref().unwrap().graph_id(),
            bundle.roles[1].handle.as_ref().unwrap().graph_id()
        );
    }
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    assert_eq!(runtime.accounting().unwrap().held, 0);
    runtime.forget_preparation(owner).await.unwrap();
    runtime.drain().unwrap();
}

#[tokio::test]
async fn typed_prepare_rollback_and_epoch_change_use_the_same_no_acquisition_refund_path() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = prepare_typed(
        &runtime,
        vec![TypedGraphPlan {
            attempt_id: None,
            members: vec![],
        }],
        false,
    );
    assert!(
        matches!(runtime.resolve_preparation(&owner, deadline()).await, Err(OperationError::Custody(error)) if error.certainty == CommitCertainty::RolledBack)
    );
    assert_eq!(runtime.accounting().unwrap().held, 0);
    runtime.forget_preparation(owner).await.unwrap();
    let owner = prepare_typed(&runtime, vec![typed_plan()], false);
    runtime
        .update_capacity(2, Metric::Known(vector(1)))
        .unwrap();
    assert!(matches!(
        runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Admission(AdmissionRefusal::StaleEpoch))
    ));
    assert_eq!(runtime.accounting().unwrap().held, 0);
    runtime.forget_preparation(owner).await.unwrap();
    runtime.drain().unwrap();
}

#[tokio::test]
async fn typed_acquisition_enrichment_cannot_use_legacy_dto_or_early_all_closed_settlement() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = prepare_typed(&runtime, vec![typed_plan()], false);
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    {
        let bundle = owner.state.lock().await;
        let role = &bundle.roles[0];
        let planned = &role.members[0];
        let mut identity = ObservedIdentity {
            version: 1,
            member_id: planned.member_id,
            adapter: planned.adapter.clone(),
            adapter_version: 1,
            domain: planned.domain.clone(),
            object_identity: "data-only".into(),
            process: None,
        };
        assert!(matches!(
            acquired_mutation(role, identity.clone()).unwrap(),
            Mutation::RecordTypedAcquired {
                envelope: v2::Envelope {
                    observed: Some(v2::Observed::LegacyV1 { .. }),
                    ..
                }
            }
        ));
        identity.member_id = Uuid::new_v4();
        assert!(acquired_mutation(role, identity).is_err());
        assert!(role.graph.is_some() && role.receipt.is_none());
    }
    assert!(runtime
        .queue_live_settlement(&owner, 0, deadline())
        .await
        .is_err());
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    runtime.drain().unwrap();
}
