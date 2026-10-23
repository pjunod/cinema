//! Actual runtime prepare binding, without any native acquisition. NOT RUN.
#![cfg(unix)]
use super::super::plans::{SourceCopyOwner, SourceCopyPlan, SourcePlan, VodTaskPlan};
use super::*;

fn id() -> v2::Id {
    v2::Id::new(Uuid::new_v4()).unwrap()
}
fn slot(member_id: v2::Id) -> v2::MemberSlot {
    v2::MemberSlot {
        member_id,
        owned_by: None,
        depends_on: vec![],
        protected_paths: vec![],
    }
}
fn inventory() -> v2::IoInventory {
    v2::IoInventory {
        pending_io: vec![],
        buffers: vec![],
        readers: vec![],
    }
}
fn local_plan(runtime: &NodeResourceRuntime) -> TypedGraphPlan {
    let root = plan().members.remove(0);
    let root_member_id = v2::Id::new(root.member_id).unwrap();
    let source_member = id();
    let task_member = id();
    let scratch_member = id();
    let asset_id = id();
    let domain = v2::FilesystemDomain {
        platform: v2::FilesystemPlatform::Linux,
        domain_id: id(),
        daemon_owner_id: v2::Id::new(runtime.owner.owner_id).unwrap(),
        os_boot: runtime.owner.os_boot.clone(),
        mount_namespace: v2::NativeValue::Unknown {
            reason: "unacquired fixture".into(),
        },
    };
    let association = v2::SourceAssociation {
        media_file_id: "fixture-media".into(),
        object_version: "fixture-version".into(),
        recipe_source_binding: "fixture-software".into(),
    };
    let copy = id();
    let copies = vec![
        SourceCopyPlan {
            copy_id: copy,
            owner: SourceCopyOwner::LocalSourceCell,
            acquisition_operation_id: id(),
            parent_copy_id: None,
        },
        SourceCopyPlan {
            copy_id: id(),
            owner: SourceCopyOwner::LocalRoot { root_member_id },
            acquisition_operation_id: id(),
            parent_copy_id: Some(copy),
        },
    ];
    let mut scratch_slot = slot(scratch_member);
    scratch_slot.protected_paths.push("/fixture/output".into());
    TypedGraphPlan {
        attempt_id: Some(Uuid::new_v4()),
        members: vec![
            TypedMemberPlan::Legacy(root),
            TypedMemberPlan::Source {
                slot: slot(source_member),
                source: SourcePlan {
                    holder_id: id(),
                    source_association: association.clone(),
                    filesystem_domain: domain.clone(),
                    copies,
                },
            },
            // Pure unacquired codec fixture; no claim this native DTO is an owner.
            TypedMemberPlan::ScratchV1 {
                slot: scratch_slot,
                scratch: v2::ScratchAssets {
                    version: 1,
                    asset_id,
                    directory_path: "/fixture/output".into(),
                    directory_object: v2::NativeObject {
                        file_type: v2::FileType::Directory,
                        device: v2::NativeValue::Unknown {
                            reason: "not acquired".into(),
                        },
                        inode: v2::NativeValue::Unknown {
                            reason: "not acquired".into(),
                        },
                        mount_id: v2::NativeValue::Unknown {
                            reason: "not acquired".into(),
                        },
                        filesystem_domain: domain,
                    },
                    files: vec![],
                    inventory: inventory(),
                },
            },
            TypedMemberPlan::VodOutputTask {
                slot: slot(task_member),
                task: VodTaskPlan {
                    root_member_id,
                    generation_epoch: 17,
                    source_association: association,
                    stdout_owner_id: id(),
                    diagnostic_owner_id: id(),
                    asset_id,
                    sink_owner_id: id(),
                    inventory: inventory(),
                },
            },
        ],
    }
}

#[tokio::test]
async fn source_copy_and_vod_task_plans_bind_once_to_the_actual_prepared_graph() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let owner = typed::prepare_typed(&runtime, vec![local_plan(&runtime)], false);
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    let members;
    {
        let bundle = owner.state.lock().await;
        let role = &bundle.roles[0];
        let graph = role.graph.as_ref().unwrap().identity();
        assert_eq!(graph.generation, role.handle.as_ref().unwrap().graph_id());
        members = role.typed_members.clone().unwrap();
        let manifest = v2::Manifest {
            version: 2,
            owner_id: v2::Id::new(graph.process_instance).unwrap(),
            graph_id: v2::Id::new(graph.generation).unwrap(),
            attempt_id: None,
            members: members.clone(),
        };
        v2::validate_manifest(&manifest).unwrap();
        for member in &members {
            match member {
                v2::MemberPlan::SourceV1 { source, slot } => {
                    assert_eq!(source.member_id, slot.member_id);
                    let v2::SourceRecovery::LocalSourceHandlesV1 {
                        owner_id,
                        graph_id,
                        filesystem_domain,
                    } = &source.recovery;
                    assert_eq!(*owner_id, manifest.owner_id);
                    assert_eq!(*graph_id, manifest.graph_id);
                    assert_eq!(filesystem_domain, &source.filesystem_domain);
                    assert!(matches!(source.planned_copies[0].owner,
                        v2::CopyOwner::DaemonSourceCell { owner_id, graph_id, member_id }
                        if owner_id == manifest.owner_id && graph_id == manifest.graph_id && member_id == source.member_id));
                    assert!(matches!(source.planned_copies[1].owner,
                        v2::CopyOwner::OwnedRoot { graph_id, .. } if graph_id == manifest.graph_id));
                }
                v2::MemberPlan::VodOutputTaskV1 { task, slot } => {
                    assert_eq!(task.graph_id, manifest.graph_id);
                    assert_eq!(task.member_id, slot.member_id);
                    assert_eq!(task.generation_epoch, 17);
                }
                _ => {}
            }
        }
        assert!(role.armed.is_empty() && role.invoked.is_empty());
    }
    // A second resolution cannot reconstruct identities or rewrite the manifest.
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .unwrap();
    assert_eq!(
        owner.state.lock().await.roles[0].typed_members.as_ref(),
        Some(&members)
    );
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .unwrap();
    assert_eq!(runtime.accounting().unwrap().held, 0);
    runtime.forget_preparation(owner).await.unwrap();
    runtime.drain().unwrap();
}

#[tokio::test]
async fn stale_source_domain_is_not_rewritten_by_local_graph_binding() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let mut plan = local_plan(&runtime);
    let foreign_owner = id();
    let TypedMemberPlan::Source { source, .. } = &mut plan.members[1] else {
        panic!()
    };
    source.filesystem_domain.daemon_owner_id = foreign_owner;
    let owner = typed::prepare_typed(&runtime, vec![plan], false);
    {
        let bundle = owner.state.lock().await;
        let v2::MemberPlan::SourceV1 { source, .. } =
            &bundle.roles[0].typed_members.as_ref().unwrap()[1]
        else {
            panic!()
        };
        assert_eq!(source.filesystem_domain.daemon_owner_id, foreign_owner);
    }
    assert!(
        matches!(runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Custody(error)) if error.certainty == CommitCertainty::RolledBack)
    );
    assert_eq!(runtime.accounting().unwrap().held, 0);
    runtime.forget_preparation(owner).await.unwrap();
    runtime.drain().unwrap();
}

#[test]
fn local_copy_binding_preserves_external_retained_owner_and_copy_lineage() {
    let member_id = id();
    let owner_id = id();
    let graph_id = id();
    let foreign_obligation = id();
    let foreign_member = id();
    let family_id = id();
    let asset_id = id();
    let parent = id();
    let operation = id();
    let bound = TypedMemberPlan::Source {
        slot: slot(member_id),
        source: SourcePlan {
            holder_id: id(),
            source_association: v2::SourceAssociation {
                media_file_id: "fixture".into(),
                object_version: "v1".into(),
                recipe_source_binding: "recipe".into(),
            },
            filesystem_domain: v2::FilesystemDomain {
                platform: v2::FilesystemPlatform::Linux,
                domain_id: id(),
                daemon_owner_id: owner_id,
                os_boot: BootIdentity::Unknown {
                    reason: "fixture".into(),
                },
                mount_namespace: v2::NativeValue::Unknown {
                    reason: "fixture".into(),
                },
            },
            copies: vec![
                SourceCopyPlan {
                    copy_id: parent,
                    owner: SourceCopyOwner::RetainedAssets { asset_id },
                    acquisition_operation_id: operation,
                    parent_copy_id: None,
                },
                SourceCopyPlan {
                    copy_id: id(),
                    owner: SourceCopyOwner::RetainedRoleHolder {
                        obligation_id: foreign_obligation,
                        member_id: foreign_member,
                        family_id,
                        role: Role::Video,
                    },
                    acquisition_operation_id: operation,
                    parent_copy_id: Some(parent),
                },
            ],
        },
    }
    .bind(owner_id, graph_id, &[])
    .expect("source copy binding");
    let v2::MemberPlan::SourceV1 { source, .. } = bound else {
        panic!()
    };
    assert_eq!(
        source.planned_copies[0].owner,
        v2::CopyOwner::RetainedAssets {
            asset_id,
            source_member_id: member_id
        }
    );
    assert_eq!(
        source.planned_copies[1].owner,
        v2::CopyOwner::RetainedRoleHolder {
            obligation_id: foreign_obligation,
            member_id: foreign_member,
            family_id,
            role: Role::Video,
        }
    );
    assert_eq!(source.planned_copies[1].parent_copy_id, Some(parent));
    assert!(source
        .planned_copies
        .iter()
        .all(|c| c.acquisition_operation_id == operation));
}
