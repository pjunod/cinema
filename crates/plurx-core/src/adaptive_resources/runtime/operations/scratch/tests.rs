//! Real runtime/journal scratch write-ahead fixtures; COMPILED NOT RUN.
#![cfg(unix)]
use super::*;
use crate::adaptive_resources::{FamilyRoleDemand, WorkloadDemand};
use crate::resource_custody::InstallationContext;
use std::time::Duration;
use v2::scratch::*;
fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn cost() -> ResourceVector {
    ResourceVector {
        peak_memory_bytes: 65536,
        handles: 4,
        scratch_bytes: 65536,
        ..ResourceVector::default()
    }
}
async fn fixture(root: &Path) -> (NodeResourceRuntime, OwnedPreparation, Uuid) {
    let runtime = NodeResourceRuntime::open(super::super::super::tests::options(
        root,
        daemon_owner(
            "scratch-fixture-node".into(),
            "scratch-fixture-cluster".into(),
        ),
        InstallationContext::FirstInstallation,
    ))
    .unwrap();
    runtime.update_capacity(1, Metric::Known(cost())).unwrap();
    let member = id();
    let protected = root.join("owned-output").to_str().unwrap().to_owned();
    let plan = ScratchPlanV2 {
        version: 2,
        asset_id: id(),
        rendition_id: "fixture-video".into(),
        generation_epoch: 1,
        filesystem_domain: v2::FilesystemDomain {
            platform: v2::FilesystemPlatform::Linux,
            domain_id: id(),
            daemon_owner_id: v2::Id::new(runtime.owner.owner_id).unwrap(),
            os_boot: runtime.owner.os_boot.clone(),
            mount_namespace: v2::NativeValue::Unknown {
                reason: "fixture has no native acquisition".into(),
            },
        },
        directory: DirectoryPlanV2 {
            protected_absolute_path: protected.clone(),
            child_component: "owned-output".into(),
            parent_binding: OwnedDirectoryRefV1 {
                owning_obligation: id(),
                member_id: id(),
                directory_id: id(),
            },
            mode: DirectoryModeV1::CreateExclusive,
        },
        outputs: OutputNamesV1 {
            init: true,
            first_segment: 0,
            segment_end_exclusive: 10000,
            naming: OutputNamingV1::FilmSegmentV1,
        },
        limits: ScratchLimitsV1 {
            resident_file_objects: 16,
            resident_logical_bytes: 32768,
            reserved_storage_bytes: 65536,
            one_file_logical_bytes: 4096,
            one_io_payload_bytes: 4096,
            pending_native_jobs: 1,
            live_handles: 4,
            reader_leases: 0,
            resident_memory_bytes: 65536,
            allocation: AllocationAccountingV1::LogicalAndReportedAllocationV1 {
                allocation_unit_bytes: 4096,
                per_object_reserve_bytes: 4096,
                directory_reserve_bytes: 4096,
            },
        },
    };
    let owner = runtime
        .prepare_typed_families(
            &[FamilyDemand {
                identity: runtime.new_family().unwrap(),
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
            vec![TypedGraphPlan {
                attempt_id: None,
                members: vec![v2::MemberPlan::ScratchAssetsV2 {
                    slot: v2::MemberSlot {
                        member_id: member,
                        owned_by: None,
                        depends_on: vec![],
                        protected_paths: vec![protected],
                    },
                    plan,
                }],
            }],
            deadline(),
        )
        .unwrap();
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    (runtime, owner, member.uuid())
}

async fn queue_unconsumed_directory(
    runtime: &NodeResourceRuntime,
    owner: &OwnedPreparation,
    member: Uuid,
) -> Uuid {
    let mut bundle = owner.state.lock().await;
    let id = bundle.id;
    let role = &mut bundle.roles[0];
    start_transition(
        &runtime.journal,
        role,
        Mutation::Arm {
            members: vec![member],
        },
        Effect::Arm(member),
        deadline(),
    )
    .unwrap();
    runtime.finish_transition(id, role).await.unwrap();
    let plan = role
        .typed_members
        .as_ref()
        .unwrap()
        .iter()
        .find_map(|m| match m {
            v2::MemberPlan::ScratchAssetsV2 { plan, .. } => Some(plan.clone()),
            _ => None,
        })
        .unwrap();
    role.scratch.insert(
        member,
        ScratchMember {
            fold: Some(ScratchFold::empty(&plan).unwrap()),
            plan,
            change: None,
            directory_arm: None,
        },
    );
    queue_directory(&runtime.journal, role, member, deadline()).unwrap();
    assert_eq!(
        role.scratch[&member].fold.as_ref().unwrap().state.revision,
        0
    );
    assert!(role.scratch[&member]
        .fold
        .as_ref()
        .unwrap()
        .state
        .directory
        .is_none());
    role.transition.as_ref().unwrap().operation.id
}

#[tokio::test]
async fn scratch_runtime_abandoned_arm_waiter_keeps_exact_pending_command_and_unconsumed_result() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner, member) = fixture(root.path()).await;
    let operation = queue_unconsumed_directory(&runtime, &owner, member).await;
    drop(owner);
    let owner = runtime.retained_preparations().unwrap().pop().unwrap();
    {
        let bundle = owner.state.lock().await;
        assert_eq!(
            bundle.roles[0].transition.as_ref().unwrap().operation.id,
            operation
        );
        assert_eq!(
            bundle.roles[0].scratch[&member]
                .fold
                .as_ref()
                .unwrap()
                .state
                .revision,
            0
        );
    }
    runtime.resume_role(&owner, 0, None).await.unwrap();
    {
        let bundle = owner.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(
            role.scratch[&member]
                .fold
                .as_ref()
                .unwrap()
                .state
                .head
                .as_ref()
                .unwrap()
                .operation_id
                .uuid(),
            operation
        );
        assert!(role.transition.is_none() && role.scratch[&member].change.is_none());
    }
    assert!(!root.path().join("owned-output").exists());
    assert_eq!(runtime.accounting().unwrap().used, cost());
    runtime.drain().unwrap();
}

#[tokio::test]
async fn scratch_runtime_known_event_fold_and_delayed_book_ack_never_apply_delta_twice() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner, member) = fixture(root.path()).await;
    let operation = queue_unconsumed_directory(&runtime, &owner, member).await;
    {
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = &mut bundle.roles[0];
        let result = role
            .transition
            .as_mut()
            .unwrap()
            .operation
            .resolve()
            .await
            .unwrap();
        assert!(matches!(result, OperationResult::Changed(ack) if ack.operation_id == operation));
        apply_committed(role, member, operation).unwrap();
        apply_committed(role, member, operation).unwrap();
        assert_eq!(
            role.scratch[&member].fold.as_ref().unwrap().state.revision,
            1
        );
        runtime.finish_transition(bundle_id, role).await.unwrap();
        assert_eq!(
            role.scratch[&member].fold.as_ref().unwrap().state.revision,
            1
        );
    }
    assert_eq!(runtime.accounting().unwrap().used, cost());
    runtime.drain().unwrap();
}

#[tokio::test]
async fn scratch_runtime_directory_arm_is_exact_known_commit_not_native_acquisition() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner, member) = fixture(root.path()).await;
    runtime
        .arm_scratch_directory(&owner, 0, member, deadline())
        .await
        .unwrap();
    let (sequence, io) = {
        let bundle = owner.state.lock().await;
        let role = &bundle.roles[0];
        let scratch = role.scratch.get(&member).unwrap();
        let fold = scratch.fold.as_ref().unwrap();
        assert_eq!(fold.state.revision, 1);
        assert_eq!(fold.state.totals.pending_jobs, 1);
        assert!(fold.state.directory.is_none());
        assert!(scratch.change.is_none() && role.transition.is_none());
        assert!(role.invoked.is_empty());
        assert_eq!(
            fold.state.head.as_ref().unwrap().operation_id,
            scratch.directory_arm.unwrap()
        );
        (role.sequence, scratch.directory_arm)
    };
    runtime
        .arm_scratch_directory(&owner, 0, member, deadline())
        .await
        .unwrap();
    {
        let bundle = owner.state.lock().await;
        assert_eq!(bundle.roles[0].sequence, sequence);
        assert_eq!(bundle.roles[0].scratch[&member].directory_arm, io);
    }
    assert!(!root.path().join("owned-output").exists());
    assert_eq!(runtime.accounting().unwrap().used, cost());
    assert!(runtime
        .cancel_preparation(&owner, deadline())
        .await
        .is_err());
    runtime.drain().unwrap();
}

#[tokio::test]
async fn scratch_runtime_expired_or_foreign_slot_never_opens_a_directory_or_refunds_a_hold() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner, member) = fixture(root.path()).await;
    assert!(runtime
        .arm_scratch_directory(&owner, 0, Uuid::new_v4(), deadline())
        .await
        .is_err());
    assert!(runtime
        .arm_scratch_directory(&owner, 0, member, Instant::now())
        .await
        .is_err());
    assert!(!root.path().join("owned-output").exists());
    assert_eq!(runtime.accounting().unwrap().used, cost());
    runtime.drain().unwrap();
}

#[tokio::test]
async fn scratch_runtime_sealed_member_can_arm_local_io_but_never_reopens_outer_members() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner, member) = fixture(root.path()).await;
    // Arm the finite member before sealing the graph. No native work has entered.
    {
        let mut bundle = owner.state.lock().await;
        let id = bundle.id;
        let role = &mut bundle.roles[0];
        start_transition(
            &runtime.journal,
            role,
            Mutation::Arm {
                members: vec![member],
            },
            Effect::Arm(member),
            deadline(),
        )
        .unwrap();
        runtime.finish_transition(id, role).await.unwrap();
    }
    runtime.seal_role(&owner, 0, deadline()).await.unwrap();
    runtime
        .arm_scratch_directory(&owner, 0, member, deadline())
        .await
        .unwrap();
    {
        let bundle = owner.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(role.phase, Phase::Sealed);
        assert!(role.graph.is_none() && role.sealed.is_some());
        assert_eq!(
            role.scratch[&member]
                .fold
                .as_ref()
                .unwrap()
                .state
                .totals
                .pending_jobs,
            1
        );
    }
    assert!(!root.path().join("owned-output").exists());
    runtime.drain().unwrap();
}
