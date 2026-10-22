//! Authored bridge contracts. COMPILE ONLY until complete-candidate review.
use super::*;
use crate::adaptive_resources::{FamilyRoleDemand, WorkloadDemand};
use crate::resource_custody::{InstallationContext, MemberKind, RecoveryMethod};
use std::time::Duration;
mod binding;
mod native_entry;
mod typed;

fn vector(value: u64) -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: value,
        peak_memory_bytes: value,
        // Three possible pipe slots per admitted root before native launch.
        handles: value * 3,
        processes: value,
        hardware_slots: value,
        tuner_slots: value,
        scratch_bytes: value,
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn runtime(root: &Path) -> NodeResourceRuntime {
    let owner = daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let runtime = NodeResourceRuntime::open(super::super::tests::options(
        root,
        owner,
        InstallationContext::FirstInstallation,
    ))
    .expect("actual journal");
    runtime
        .update_capacity(1, Metric::Known(vector(100)))
        .expect("observed bounds");
    runtime
}
fn plan() -> GraphPlan {
    GraphPlan {
        attempt_id: None,
        members: vec![PlannedMember {
            member_id: Uuid::new_v4(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "fixture-root".into(),
            locator: "unexecuted-root".into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "fixture has no physical backend".into(),
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        }],
    }
}
fn prepare(
    runtime: &NodeResourceRuntime,
    plans: Vec<GraphPlan>,
    until: Instant,
) -> Result<OwnedPreparation, OperationError> {
    let roles: Vec<_> = (0..plans.len())
        .map(|index| FamilyRoleDemand {
            role: [FamilyRole::Video, FamilyRole::Audio, FamilyRole::Companion][index],
            demand: WorkloadDemand {
                resources: Metric::Known(vector(2)),
                limit_epoch: 1,
                trial: false,
            },
            retained: None,
        })
        .collect();
    runtime.prepare_families(
        &[FamilyDemand {
            identity: runtime.new_family()?,
            roles: &roles,
        }],
        plans,
        until,
    )
}

#[cfg(unix)]
#[tokio::test]
async fn dropped_prepare_selection_retains_whole_charge_then_durable_cancel_refunds_once() {
    let root = tempfile::tempdir().expect("durable TMPDIR");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan(), plan()], deadline()).expect("queued together");
    assert_eq!(runtime.accounting().expect("provisional").used, vector(4));
    drop(owner);
    let mut retained = runtime.retained_preparations().expect("same owner book");
    assert_eq!(retained.len(), 1);
    let owner = retained.pop().expect("retained owner");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("exact owned result");
    {
        let bundle = owner.state.lock().await;
        assert_eq!(bundle.roles.len(), 2);
        assert!(bundle
            .roles
            .iter()
            .all(|role| role.phase == Phase::Prepared && role.handle.is_some() && !role.refunded));
        assert_eq!(bundle.roles[0].sequence, bundle.roles[1].sequence);
    }
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .expect("settle prepared");
    assert_eq!(
        runtime.accounting().expect("refunded").used,
        ResourceVector::default()
    );
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .expect("no double refund");
    runtime
        .forget_preparation(owner)
        .await
        .expect("reclaim bounded book");
    assert!(runtime
        .retained_preparations()
        .expect("empty live book")
        .is_empty());
    runtime.drain().expect("drain actual worker");
}

#[cfg(unix)]
#[tokio::test]
async fn late_prepare_commit_rechecks_epoch_and_cancels_without_returning_launch_owner() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan(), plan()], deadline()).expect("prepare accepted");
    runtime
        .update_capacity(2, Metric::Known(vector(1)))
        .expect("new epoch below held cost");
    assert_eq!(runtime.accounting().expect("not clamped").used, vector(4));
    assert!(matches!(
        runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Admission(AdmissionRefusal::StaleEpoch))
    ));
    assert_eq!(
        runtime
            .accounting()
            .expect("only after durable cancel")
            .held,
        0
    );
    assert!(owner
        .state
        .lock()
        .await
        .roles
        .iter()
        .all(|role| role.refunded && role.phase == Phase::Settled && role.graph.is_none()));
    runtime
        .forget_preparation(owner)
        .await
        .expect("forget settled bundle");
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn drain_after_prepare_acceptance_cancels_all_members_and_refuses_new_families() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).expect("accepted");
    runtime.begin_drain().expect("draining");
    assert!(matches!(
        runtime.new_family(),
        Err(OperationError::Draining)
    ));
    assert!(matches!(
        runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Draining)
    ));
    assert_eq!(runtime.accounting().expect("durable cancellation").held, 0);
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn definite_prepare_rejection_refunds_but_keeps_exact_diagnostic_until_acknowledged() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let owner = prepare(
        &runtime,
        vec![GraphPlan {
            attempt_id: None,
            members: Vec::new(),
        }],
        deadline(),
    )
    .expect("queue accepts invalid body for DB validation");
    assert!(
        matches!(runtime.resolve_preparation(&owner, deadline()).await, Err(OperationError::Custody(error)) if error.certainty == CommitCertainty::RolledBack)
    );
    assert_eq!(runtime.accounting().expect("definite rollback").held, 0);
    runtime
        .forget_preparation(owner)
        .await
        .expect("not leaking completed operation");
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[test]
fn expired_enqueue_never_leaves_a_durable_charge_or_owned_queue_command() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    assert!(prepare(&runtime, vec![plan()], Instant::now()).is_err());
    assert_eq!(runtime.accounting().expect("no accepted command").held, 0);
    assert!(runtime
        .retained_preparations()
        .expect("no owned bundle")
        .is_empty());
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn durable_graph_drop_and_public_cleanup_cannot_bypass_journal_settlement() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).expect("accepted");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("committed prepared");
    {
        let mut bundle = owner.state.lock().await;
        let role = &mut bundle.roles[0];
        drop(role.graph.take());
        // This synthetic test receipt is NOT physical backend/qualification evidence.
        let rejected = role
            ._cleanup
            .close(PhysicalCloseReceipt {
                binding: role.binding,
            })
            .expect_err("durable close must retain proof");
        assert_eq!(rejected.reason, AdmissionRefusal::InvalidTransition);
        assert_eq!(rejected.receipt.custody_binding(), role.binding);
    }
    let snapshot = runtime.accounting().expect("all costs retained");
    assert_eq!(snapshot.used, vector(2));
    assert_eq!(snapshot.unknown_held, 1);
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .expect("actual never-armed owner can cancel");
    assert_eq!(runtime.accounting().expect("settled first").held, 0);
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn proof_is_retained_synchronously_on_queue_rejection_and_busy_owner_returns_original() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).expect("accepted");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("prepared");
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("sealed");
    let binding = owner.state.lock().await.roles[0].binding;
    // Synthetic private test receipt checks ownership, not physical extinction.
    let proof = PhysicalSettlement {
        receipt: PhysicalCloseReceipt { binding },
        facts: Vec::new(),
    };
    let guard = owner.state.lock().await;
    let (_, Some(proof)) = runtime
        .settle_role(&owner, 0, proof, deadline())
        .expect_err("busy must return actual proof")
    else {
        panic!("lost actual proof");
    };
    assert_eq!(proof.receipt.custody_binding(), binding);
    drop(guard);
    runtime
        .settle_role(&owner, 0, proof, Instant::now())
        .expect("synchronous retained queue rejection");
    {
        let bundle = owner.state.lock().await;
        let operation = &bundle.roles[0]
            .transition
            .as_ref()
            .expect("owned settlement")
            .operation;
        assert!(
            matches!(&operation.rejected, Some(rejected) if matches!(&rejected.mutation, Mutation::Settle(proof) if proof.receipt.custody_binding() == binding))
        );
    }
    assert_eq!(runtime.accounting().expect("not refunded").used, vector(2));
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn retained_successor_pins_prevent_original_refund_until_candidate_is_retired() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let original = prepare(&runtime, vec![plan(), plan()], deadline()).expect("original family");
    runtime
        .resolve_preparation(&original, deadline())
        .await
        .expect("original prepared");
    let successor = {
        let bundle = original.state.lock().await;
        let held_video = bundle.roles[0]
            .graph
            .as_ref()
            .expect("actual video owner")
            .retained_role();
        let held_audio = bundle.roles[1]
            .graph
            .as_ref()
            .expect("actual audio owner")
            .retained_role();
        let demand = WorkloadDemand {
            resources: Metric::Known(vector(2)),
            limit_epoch: 1,
            trial: false,
        };
        let incumbent_roles = [
            FamilyRoleDemand {
                role: FamilyRole::Video,
                demand,
                retained: Some(held_video),
            },
            FamilyRoleDemand {
                role: FamilyRole::Audio,
                demand,
                retained: Some(held_audio),
            },
        ];
        let successor_roles = [
            FamilyRoleDemand {
                role: FamilyRole::Video,
                demand,
                retained: None,
            },
            FamilyRoleDemand {
                role: FamilyRole::Audio,
                demand,
                retained: Some(held_audio),
            },
        ];
        runtime
            .prepare_families(
                &[
                    FamilyDemand {
                        identity: bundle.family_ids[0],
                        roles: &incumbent_roles,
                    },
                    FamilyDemand {
                        identity: runtime.new_family().expect("new identity"),
                        roles: &successor_roles,
                    },
                ],
                vec![plan()],
                deadline(),
            )
            .expect("atomic shared overlap")
    };
    runtime
        .resolve_preparation(&successor, deadline())
        .await
        .expect("successor prepared");
    assert_eq!(
        runtime
            .accounting()
            .expect("distinct obligations only")
            .used,
        vector(6)
    );
    assert!(runtime
        .cancel_preparation(&original, deadline())
        .await
        .is_err());
    assert_eq!(
        runtime.accounting().expect("original pinned").used,
        vector(6)
    );
    runtime
        .cancel_preparation(&successor, deadline())
        .await
        .expect("retire candidate new video");
    runtime
        .forget_preparation(successor)
        .await
        .expect("release retained users, not original costs");
    assert_eq!(
        runtime.accounting().expect("incumbent unchanged").used,
        vector(4)
    );
    runtime
        .cancel_preparation(&original, deadline())
        .await
        .expect("now original may close");
    assert_eq!(runtime.accounting().expect("all settled").held, 0);
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn corrected_definite_settlement_retains_original_proof_and_refunds_after_exact_commit_only()
{
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).expect("accepted");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("prepared");
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("sealed first");
    let (binding, member) = {
        let bundle = owner.state.lock().await;
        (bundle.roles[0].binding, bundle.roles[0].members[0].clone())
    };
    // A synthetic test-only proof exercises the persistence protocol. It is NOT
    // a positive kernel/physical backend receipt or qualification measurement.
    runtime
        .settle_role(
            &owner,
            0,
            PhysicalSettlement {
                receipt: PhysicalCloseReceipt { binding },
                facts: Vec::new(),
            },
            deadline(),
        )
        .expect("actual proof transferred synchronously");
    assert_eq!(
        runtime.accounting().expect("no precommit refund").used,
        vector(2)
    );
    assert!(matches!(runtime.resume_role(&owner, 0, None).await,
        Err(OperationError::Custody(error)) if error.certainty == CommitCertainty::RolledBack));
    assert_eq!(
        runtime.accounting().expect("failed facts keep charge").used,
        vector(2)
    );
    runtime
        .correct_settlement_facts(
            &owner,
            0,
            vec![crate::resource_custody::ClosedMemberFact {
                member_id: member.member_id,
                adapter: member.adapter,
                adapter_version: 1,
                domain: member.domain,
                object_identity: "synthetic-never-acquired-fixture".into(),
                evidence: "test-only ownership protocol, not physical closure".into(),
            }],
            deadline(),
        )
        .expect("definite failure returns original proof for corrected data");
    runtime
        .resume_role(&owner, 0, None)
        .await
        .expect("exact committed settlement then memory refund");
    assert_eq!(runtime.accounting().expect("refunded once").held, 0);
    assert!(runtime.resume_role(&owner, 0, None).await.is_err());
    runtime
        .forget_preparation(owner)
        .await
        .expect("receipt and operation reclaimed");
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn ambiguous_launcher_error_keeps_armed_member_and_charge_and_cannot_reinvoke_slot() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let plan = plan();
    let member = plan.members[0].member_id;
    let owner = prepare(&runtime, vec![plan], deadline()).expect("accepted");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("prepared");
    let mut command = tokio::process::Command::new(root.path().join("missing-fixture-executable"));
    let work = crate::process::ChildWork::background("custody protocol fixture");
    assert!(runtime
        .spawn_root(&owner, 0, member, &mut command, work, deadline())
        .await
        .is_err());
    {
        let bundle = owner.state.lock().await;
        assert!(bundle.roles[0].armed.contains(&member));
        assert!(bundle.roles[0].invoked.contains(&member));
        assert!(bundle.roles[0].graph.is_none());
    }
    runtime
        .resume_role(&owner, 0, None)
        .await
        .expect("durable unknown hold");
    assert_eq!(owner.state.lock().await.roles[0].phase, Phase::UnknownHeld);
    assert_eq!(
        runtime.accounting().expect("full charge survives").used,
        vector(2)
    );
    assert!(runtime
        .spawn_root(&owner, 0, member, &mut command, work, deadline())
        .await
        .is_err());
    assert!(runtime
        .cancel_preparation(&owner, deadline())
        .await
        .is_err());
    runtime.drain().expect("drain");
}
