//! Actual acquired-member fixtures, COMPILED NOT RUN before final review.
#![cfg(unix)]
use super::*;
use crate::adaptive_resources::{FamilyRoleDemand, WorkloadDemand};
use crate::resource_custody::InstallationContext;
use std::future::Future;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn cost() -> ResourceVector {
    ResourceVector {
        peak_memory_bytes: 65536,
        handles: 8,
        processes: 1,
        ..ResourceVector::default()
    }
}
async fn fixture(path: &Path, with_drain: bool) -> (NodeResourceRuntime, OwnedPreparation) {
    let runtime = NodeResourceRuntime::open(super::super::super::tests::options(
        path,
        daemon_owner(
            "live-settlement-node".into(),
            "live-settlement-cluster".into(),
        ),
        InstallationContext::FirstInstallation,
    ))
    .expect("actual local journal");
    runtime
        .update_capacity(1, Metric::Known(cost()))
        .expect("cap");
    let root = Uuid::new_v4();
    let mut members = vec![PlannedMember {
        member_id: root,
        kind: MemberKind::Root,
        adapter: Adapter::ProcessRoot,
        adapter_version: 1,
        domain: "live-settlement-fixture".into(),
        locator: "root-slot".into(),
        recovery: RecoveryMethod::LiveOnly {
            reason: "live only".into(),
        },
        owned_by: None,
        depends_on: Vec::new(),
        maximum_cardinality: 1,
        protected_paths: Vec::new(),
    }];
    let task = Uuid::new_v4();
    if with_drain {
        members.push(PlannedMember {
            member_id: task,
            kind: MemberKind::Task,
            adapter: Adapter::RootPipeTask,
            adapter_version: 1,
            domain: "live-settlement-fixture".into(),
            locator: pipe_drains::LOCATOR.into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "owned task".into(),
            },
            owned_by: Some(root),
            depends_on: vec![root],
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        });
    }
    let owner = runtime
        .prepare_families(
            &[FamilyDemand {
                identity: runtime.new_family().expect("family"),
                roles: &[FamilyRoleDemand {
                    role: FamilyRole::Video,
                    demand: WorkloadDemand {
                        resources: Metric::Known(cost()),
                        limit_epoch: 1,
                        trial: false,
                    },
                    retained: None,
                }],
            }],
            vec![GraphPlan {
                attempt_id: None,
                members,
            }],
            deadline(),
        )
        .expect("owned prepare");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("durable prepare");
    let mut command = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", "process::tests::child_main", "--nocapture"])
        .env("PLURX_CHILD_MODE", "sleep")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    runtime
        .spawn_root(
            &owner,
            0,
            root,
            &mut command,
            crate::process::ChildWork::background("live member settlement fixture"),
            deadline(),
        )
        .await
        .expect("actual root");
    if with_drain {
        runtime
            .spawn_pipe_drain(&owner, 0, task, deadline())
            .await
            .expect("actual audited task");
    }
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("durable seal");
    (runtime, owner)
}
async fn cleanup(runtime: NodeResourceRuntime, owner: &OwnedPreparation) {
    owner.state.lock().await.roles[0]
        .sealed
        .as_ref()
        .expect("sealed")
        .close()
        .await;
    assert_eq!(
        runtime
            .accounting()
            .expect("no fabricated proof/refund")
            .used,
        cost()
    );
    runtime.drain().expect("journal stopped");
}

#[tokio::test]
async fn live_inventory_captures_exact_acquired_root_and_task_before_owner_disposal() {
    let path = tempfile::tempdir().expect("durable TMPDIR");
    let (runtime, owner) = fixture(path.path(), true).await;
    {
        let mut bundle = owner.state.lock().await;
        let role = &mut bundle.roles[0];
        let facts = live_member_facts(role).expect("actual acquired inventory");
        assert_eq!(facts.len(), 2);
        for fact in &facts {
            assert_eq!(fact.domain, "live-settlement-fixture");
            assert_eq!(fact.adapter_version, 1);
            match fact.adapter {
                Adapter::ProcessRoot => {
                    let Metric::Known(process) = role.roots[&fact.member_id].process else {
                        panic!("known root");
                    };
                    assert_eq!(
                        fact.object_identity,
                        format!("{}:{}", process.pid, process.start_marker)
                    );
                }
                Adapter::RootPipeTask => assert_eq!(
                    fact.object_identity,
                    role.drains[&fact.member_id].object_identity()
                ),
                _ => panic!("unsupported inventory"),
            }
        }
        role.members.reverse();
        let reversed = live_member_facts(role).expect("selection by member, not order");
        assert_eq!(reversed, facts.into_iter().rev().collect::<Vec<_>>());
        assert!(role.receipt.is_none() && role.transition.is_none());
    }
    cleanup(runtime, &owner).await;
}

#[tokio::test]
async fn missing_or_duplicate_actual_selection_refuses_before_receipt() {
    let path = tempfile::tempdir().expect("fixture");
    let (runtime, owner) = fixture(path.path(), true).await;
    {
        let mut bundle = owner.state.lock().await;
        let role = &mut bundle.roles[0];
        let member = role.members[0].member_id;
        role.invoked.remove(&member);
        assert!(live_member_facts(role).is_err());
        role.invoked.insert(member);
        let (id, drain) = role.drains.pop_first().expect("actual selection");
        assert!(live_member_facts(role).is_err());
        role.drains.insert(id, drain);
        role.members.push(role.members[0].clone());
        assert!(live_member_facts(role).is_err());
        role.members.pop();
        assert!(live_member_facts(role).is_ok());
    }
    cleanup(runtime, &owner).await;
}

#[tokio::test]
async fn unsupported_or_unaudited_member_cannot_be_relabelled_into_live_facts() {
    let path = tempfile::tempdir().expect("fixture");
    let (runtime, owner) = fixture(path.path(), true).await;
    {
        let mut bundle = owner.state.lock().await;
        let role = &mut bundle.roles[0];
        let original = role.members[1].clone();
        role.members[1].locator = "arbitrary-writer".into();
        assert!(live_member_facts(role).is_err());
        role.members[1] = original.clone();
        role.members[1].adapter = Adapter::Scratch;
        role.members[1].kind = MemberKind::TypedResource;
        assert!(live_member_facts(role).is_err());
        role.members[1] = original.clone();
        role.members[1].maximum_cardinality = 2;
        assert!(live_member_facts(role).is_err());
        role.members[1] = original;
    }
    cleanup(runtime, &owner).await;
}

#[tokio::test]
async fn retained_user_and_conflicting_transition_refuse_before_physical_cleanup() {
    let path = tempfile::tempdir().expect("fixture");
    let (runtime, owner) = fixture(path.path(), false).await;
    let binding = owner.state.lock().await.roles[0].binding;
    runtime
        .operations
        .lock()
        .expect("book")
        .retained_users
        .insert(binding, [Uuid::new_v4()].into());
    assert!(matches!(
        runtime.queue_live_settlement(&owner, 0, deadline()).await,
        Err(OperationError::State)
    ));
    runtime
        .operations
        .lock()
        .expect("book")
        .retained_users
        .remove(&binding);
    {
        let mut bundle = owner.state.lock().await;
        start_transition(
            &runtime.journal,
            &mut bundle.roles[0],
            Mutation::Hold {
                reason: "retained test transition".into(),
            },
            Effect::Hold,
            deadline(),
        )
        .expect("actual queued transition");
    }
    assert!(matches!(
        runtime.queue_live_settlement(&owner, 0, deadline()).await,
        Err(OperationError::State)
    ));
    runtime
        .resume_role(&owner, 0, None)
        .await
        .expect("resume original hold");
    cleanup(runtime, &owner).await;
}

#[tokio::test]
async fn unsupported_live_platform_retains_charge_and_no_settlement_transition() {
    let path = tempfile::tempdir().expect("fixture");
    let (runtime, owner) = fixture(path.path(), true).await;
    assert!(
        matches!(runtime.queue_live_settlement(&owner, 0, deadline()).await,
        Err(OperationError::Physical(error)) if error.kind() == std::io::ErrorKind::Unsupported)
    );
    let bundle = owner.state.lock().await;
    assert!(bundle.roles[0].transition.is_none() && bundle.roles[0].receipt.is_none());
    assert!(!bundle.roles[0].refunded);
    drop(bundle);
    cleanup(runtime, &owner).await;
}

#[tokio::test]
async fn cancelled_live_settlement_wait_keeps_original_bundle_and_acquisitions() {
    let path = tempfile::tempdir().expect("fixture");
    let (runtime, owner) = fixture(path.path(), true).await;
    let guard = owner.state.lock().await;
    let mut wait = Box::pin(runtime.queue_live_settlement(&owner, 0, deadline()));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(wait.as_mut().poll(&mut context).is_pending());
    drop(wait);
    assert_eq!(guard.roles[0].roots.len(), 1);
    assert_eq!(guard.roles[0].drains.len(), 1);
    assert!(guard.roles[0].transition.is_none());
    drop(guard);
    assert!(live_member_facts(&owner.state.lock().await.roles[0]).is_ok());
    cleanup(runtime, &owner).await;
}

#[tokio::test]
async fn foreign_sealed_graph_with_same_counts_refuses_before_disposing_either_root() {
    let first = tempfile::tempdir().expect("first fixture");
    let second = tempfile::tempdir().expect("second fixture");
    let (runtime, owner) = fixture(first.path(), false).await;
    let (other_runtime, other_owner) = fixture(second.path(), false).await;
    {
        let mut bundle = owner.state.lock().await;
        let mut other = other_owner.state.lock().await;
        std::mem::swap(&mut bundle.roles[0].sealed, &mut other.roles[0].sealed);
    }
    assert!(matches!(
        runtime.queue_live_settlement(&owner, 0, deadline()).await,
        Err(OperationError::State)
    ));
    {
        let mut bundle = owner.state.lock().await;
        let mut other = other_owner.state.lock().await;
        for role in [&bundle.roles[0], &other.roles[0]] {
            let snapshot = role
                .sealed
                .as_ref()
                .expect("retained graph")
                .snapshot()
                .await;
            assert_eq!(snapshot.roots, 1);
            assert_eq!(snapshot.waited_roots, 0);
            assert!(role.transition.is_none() && !role.refunded);
        }
        std::mem::swap(&mut bundle.roles[0].sealed, &mut other.roles[0].sealed);
    }
    cleanup(runtime, &owner).await;
    cleanup(other_runtime, &other_owner).await;
}
