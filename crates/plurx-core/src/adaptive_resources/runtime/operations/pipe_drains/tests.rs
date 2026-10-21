//! Real journal/owned-child/task contract bodies, COMPILED NOT RUN.
use super::*;
use crate::adaptive_resources::{FamilyRoleDemand, WorkloadDemand};
use crate::resource_custody::InstallationContext;
use std::future::Future;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn demand() -> ResourceVector {
    ResourceVector {
        peak_memory_bytes: 65536,
        handles: 8,
        processes: 1,
        ..ResourceVector::default()
    }
}
fn runtime(root: &Path) -> NodeResourceRuntime {
    let runtime = NodeResourceRuntime::open(super::super::super::tests::options(
        root,
        daemon_owner("drain-fixture-node".into(), "drain-fixture-cluster".into()),
        InstallationContext::FirstInstallation,
    ))
    .expect("real local journal");
    runtime
        .update_capacity(
            1,
            Metric::Known(ResourceVector {
                peak_memory_bytes: 1024 * 1024,
                handles: 100,
                processes: 100,
                ..ResourceVector::default()
            }),
        )
        .expect("fixture bound");
    runtime
}
fn plan() -> (GraphPlan, Uuid, Uuid) {
    let root = Uuid::new_v4();
    let task = Uuid::new_v4();
    let root_plan = PlannedMember {
        member_id: root,
        kind: MemberKind::Root,
        adapter: Adapter::ProcessRoot,
        adapter_version: 1,
        domain: "drain-fixture-domain".into(),
        locator: "root-slot".into(),
        recovery: RecoveryMethod::LiveOnly {
            reason: "no containment fixture".into(),
        },
        owned_by: None,
        depends_on: Vec::new(),
        maximum_cardinality: 1,
        protected_paths: Vec::new(),
    };
    let task_plan = PlannedMember {
        member_id: task,
        kind: MemberKind::Task,
        adapter: Adapter::RootPipeTask,
        adapter_version: 1,
        domain: "drain-fixture-domain".into(),
        locator: LOCATOR.into(),
        recovery: RecoveryMethod::LiveOnly {
            reason: "in-process pipe owner".into(),
        },
        owned_by: Some(root),
        depends_on: vec![root],
        maximum_cardinality: 1,
        protected_paths: Vec::new(),
    };
    (
        GraphPlan {
            attempt_id: None,
            members: vec![root_plan, task_plan],
        },
        root,
        task,
    )
}
fn prepare(
    runtime: &NodeResourceRuntime,
    plan: GraphPlan,
    resources: ResourceVector,
) -> OwnedPreparation {
    runtime
        .prepare_families(
            &[FamilyDemand {
                identity: runtime.new_family().expect("family"),
                roles: &[FamilyRoleDemand {
                    role: FamilyRole::Video,
                    demand: WorkloadDemand {
                        resources: Metric::Known(resources),
                        limit_epoch: 1,
                        trial: false,
                    },
                    retained: None,
                }],
            }],
            vec![plan],
            deadline(),
        )
        .expect("owned prepare")
}
fn command() -> tokio::process::Command {
    let mut command =
        tokio::process::Command::new(std::env::current_exe().expect("fixture binary"));
    command
        .args(["--exact", "process::tests::child_main", "--nocapture"])
        .env("PLURX_CHILD_MODE", "sleep")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}
async fn launch(runtime: &NodeResourceRuntime, owner: &OwnedPreparation, root: Uuid) {
    runtime
        .resolve_preparation(owner, deadline())
        .await
        .expect("prepared committed");
    runtime
        .spawn_root(
            owner,
            0,
            root,
            &mut command(),
            crate::process::ChildWork::background("audited pipe drain fixture"),
            deadline(),
        )
        .await
        .expect("real retained root");
}
async fn cleanup(runtime: &NodeResourceRuntime, owner: &OwnedPreparation) {
    let bundle = owner.state.lock().await;
    let sealed = bundle.roles[0].sealed.as_ref().expect("sealed graph");
    assert_eq!(
        sealed.close().await,
        physical_graph::GraphCloseOutcome::UnknownHeld(
            physical_graph::GraphHoldReason::ContainmentUnavailable
        )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn audited_task_arm_acquire_join_preserves_whole_graph_charge_and_exact_identity() {
    let root = tempfile::tempdir().expect("durable TMPDIR");
    let runtime = runtime(root.path());
    let (plan, root_id, task) = plan();
    let owner = prepare(&runtime, plan, demand());
    launch(&runtime, &owner, root_id).await;
    runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .expect("arm before actual task");
    assert!(runtime.close_pipe_drain(&owner, 0, task).await.is_err());
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("seal committed");
    let fact = runtime
        .close_pipe_drain(&owner, 0, task)
        .await
        .expect("abort and actual join");
    assert_eq!(fact.adapter, Adapter::RootPipeTask);
    assert!(fact.object_identity.starts_with("pipe-drain-v1:"));
    assert_eq!(
        runtime
            .close_pipe_drain(&owner, 0, task)
            .await
            .expect("same facts"),
        fact
    );
    assert_eq!(
        runtime.accounting().expect("never whole-graph refund").used,
        demand()
    );
    cleanup(&runtime, &owner).await;
    runtime.drain().expect("worker drained");
}

#[cfg(unix)]
#[tokio::test]
async fn duplicate_audited_task_cannot_steal_pipes_or_start_second_task() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let (plan, root_id, task) = plan();
    let owner = prepare(&runtime, plan, demand());
    launch(&runtime, &owner, root_id).await;
    runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .expect("first task");
    assert!(runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .is_err());
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("seal");
    cleanup(&runtime, &owner).await;
    runtime
        .close_pipe_drain(&owner, 0, task)
        .await
        .expect("join already cached by graph close");
    assert_eq!(
        owner.state.lock().await.roles[0]
            .sealed
            .as_ref()
            .expect("sealed")
            .snapshot()
            .await
            .writers,
        1
    );
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn unknown_pipe_writer_recipe_refuses_before_arm_or_actual_pipe_take() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let (mut plan, root_id, task) = plan();
    plan.members[1].locator = "arbitrary-output-callback".into();
    let owner = prepare(&runtime, plan, demand());
    launch(&runtime, &owner, root_id).await;
    assert!(runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .is_err());
    assert!(!owner.state.lock().await.roles[0].armed.contains(&task));
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("seal");
    cleanup(&runtime, &owner).await;
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn task_with_missing_owned_root_refuses_without_spawning_unbound_future() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let (plan, _, task) = plan();
    let owner = prepare(&runtime, plan, demand());
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("prepared");
    assert!(runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .is_err());
    assert!(!owner.state.lock().await.roles[0].armed.contains(&task));
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .expect("intact never-armed cancellation");
    assert_eq!(runtime.accounting().expect("never acquired").held, 0);
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn insufficient_task_buffer_budget_seals_and_holds_without_early_refund() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let (plan, root_id, task) = plan();
    let resources = ResourceVector {
        peak_memory_bytes: 1,
        ..demand()
    };
    let owner = prepare(&runtime, plan, resources);
    launch(&runtime, &owner, root_id).await;
    assert!(runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .is_err());
    runtime
        .resume_role(&owner, 0, None)
        .await
        .expect("durable hold acknowledged");
    assert!(owner.state.lock().await.roles[0].drains.is_empty());
    cleanup(&runtime, &owner).await;
    assert_eq!(
        runtime.accounting().expect("uncertainty retained").used,
        resources
    );
    runtime.drain().expect("drain");
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_task_start_waiter_retains_exact_arm_operation_then_cannot_replay_slot() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    let (plan, root_id, task) = plan();
    let owner = prepare(&runtime, plan, demand());
    launch(&runtime, &owner, root_id).await;
    // Force cancellation at the pending journal acknowledgement, not an OS sleep.
    let mut future = Box::pin(runtime.spawn_pipe_drain(&owner, 0, task, deadline()));
    let _ = std::future::poll_fn(|context| {
        let result = future.as_mut().poll(context);
        std::task::Poll::Ready(result)
    })
    .await;
    drop(future);
    // If the owned operation was still pending, explicit supervision resumes the
    // SAME command. A fully completed first poll is also a valid owned outcome.
    if owner.state.lock().await.roles[0].transition.is_some() {
        runtime
            .resume_role(&owner, 0, None)
            .await
            .expect("same retained transition");
    }
    assert!(runtime
        .spawn_pipe_drain(&owner, 0, task, deadline())
        .await
        .is_err());
    runtime
        .seal_role(&owner, 0, deadline())
        .await
        .expect("cleanup seals late arm");
    cleanup(&runtime, &owner).await;
    assert_eq!(
        runtime.accounting().expect("no cancel refund").used,
        demand()
    );
    runtime.drain().expect("drain");
}
