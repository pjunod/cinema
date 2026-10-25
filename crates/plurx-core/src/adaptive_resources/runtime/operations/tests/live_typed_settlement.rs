//! Journal/accounting fixtures only: synthetic close receipts below do not
//! establish native closure. Behavioral execution waits for final review.
#![cfg(unix)]
use super::*;
use crate::resource_custody::{ClosedMemberFact, PhysicalTypedSettlement};

async fn armed_roots(runtime: &NodeResourceRuntime) -> (OwnedPreparation, Vec<PlannedMember>) {
    let roots = vec![plan().members.remove(0), plan().members.remove(0)];
    let owner = typed::prepare_typed(
        runtime,
        vec![TypedGraphPlan {
            attempt_id: None,
            members: roots.iter().cloned().map(TypedMemberPlan::Legacy).collect(),
        }],
        false,
    );
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    {
        let mut bundle = owner.state.lock().await;
        let id = bundle.id;
        let role = &mut bundle.roles[0];
        start_transition(
            &runtime.journal,
            role,
            Mutation::Arm {
                members: roots.iter().map(|root| root.member_id).collect(),
            },
            Effect::Arm(roots[0].member_id),
            deadline(),
        )
        .unwrap();
        runtime.finish_transition(id, role).await.unwrap();
    }
    runtime.seal_role(&owner, 0, deadline()).await.unwrap();
    (owner, roots)
}

fn accounting_proof(
    runtime: &NodeResourceRuntime,
    role: &OwnedRole,
    roots: &[PlannedMember],
    operation_id: Uuid,
) -> PhysicalTypedSettlement {
    PhysicalTypedSettlement {
        // Private synthetic accounting fixture, never an OS-close assertion.
        receipt: PhysicalCloseReceipt {
            binding: role.binding,
            #[cfg(target_os = "linux")]
            _recovered_r: None,
        },
        settlement: v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(runtime.owner.owner_id).unwrap(),
            graph_id: v2::Id::new(role.handle.as_ref().unwrap().graph_id()).unwrap(),
            operation_id: v2::Id::new(operation_id).unwrap(),
            members: roots
                .iter()
                .map(|root| v2::MemberDisposition::Closed {
                    member_id: v2::Id::new(root.member_id).unwrap(),
                    fact: v2::ClosedFact::LegacyV1 {
                        fact: ClosedMemberFact {
                            member_id: root.member_id,
                            adapter: root.adapter.clone(),
                            adapter_version: root.adapter_version,
                            domain: root.domain.clone(),
                            object_identity: "unexecuted accounting fixture".into(),
                            evidence: "synthetic test receipt; no native acceptance".into(),
                        },
                    },
                })
                .collect(),
        },
    }
}

#[tokio::test]
async fn live_multi_member_settlement_closes_armed_roots_and_refunds_once() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let (owner, roots) = armed_roots(&runtime).await;
    let mut bundle = owner.state.lock().await;
    let id = bundle.id;
    let role = &mut bundle.roles[0];
    let operation_id = Uuid::new_v4();
    let proof = accounting_proof(&runtime, role, &roots, operation_id);
    start_named_transition(
        &runtime.journal,
        role,
        operation_id,
        Mutation::SettleTyped(Box::new(proof)),
        Effect::Settle,
        deadline(),
    )
    .unwrap();
    runtime.finish_transition(id, role).await.unwrap();
    assert_eq!(role.phase, Phase::Settled);
    assert!(role.refunded);
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    let settled_sequence = role.sequence;
    let operation_id = Uuid::new_v4();
    let proof = accounting_proof(&runtime, role, &roots, operation_id);
    start_named_transition(
        &runtime.journal,
        role,
        operation_id,
        Mutation::SettleTyped(Box::new(proof)),
        Effect::Settle,
        deadline(),
    )
    .unwrap();
    assert!(runtime.finish_transition(id, role).await.is_err());
    assert_eq!(role.sequence, settled_sequence);
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    drop(bundle);
    runtime.drain().unwrap();
}

#[tokio::test]
async fn incomplete_live_settlement_keeps_all_charge_and_original_command() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let (owner, roots) = armed_roots(&runtime).await;
    let mut bundle = owner.state.lock().await;
    let id = bundle.id;
    let role = &mut bundle.roles[0];
    let before = role.sequence;
    let operation_id = Uuid::new_v4();
    let mut proof = accounting_proof(&runtime, role, &roots, operation_id);
    proof.settlement.members.pop();
    start_named_transition(
        &runtime.journal,
        role,
        operation_id,
        Mutation::SettleTyped(Box::new(proof)),
        Effect::Settle,
        deadline(),
    )
    .unwrap();
    assert!(runtime.finish_transition(id, role).await.is_err());
    assert_eq!(role.sequence, before);
    assert!(!role.refunded);
    assert_eq!(role.transition.as_ref().unwrap().operation.id, operation_id);
    assert_eq!(runtime.accounting().unwrap().used, vector(2));
    drop(bundle);
    runtime.drain().unwrap();
}
