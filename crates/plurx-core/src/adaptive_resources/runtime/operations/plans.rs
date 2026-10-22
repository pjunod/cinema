//! Process-local plans, bound once to the actual graph before durable prepare.
//! These are not serialized manifests, native identities or acquisition permits.
use super::*;

pub(crate) struct TypedGraphPlan {
    pub attempt_id: Option<Uuid>,
    pub members: Vec<TypedMemberPlan>,
}

#[derive(Clone)]
pub(crate) enum TypedMemberPlan {
    Legacy(PlannedMember),
    Source {
        slot: v2::MemberSlot,
        source: SourcePlan,
    },
    VodOutputTask {
        slot: v2::MemberSlot,
        task: VodTaskPlan,
    },
    ScratchV1 {
        slot: v2::MemberSlot,
        scratch: v2::ScratchAssets,
    },
    ScratchV2 {
        slot: v2::MemberSlot,
        plan: v2::scratch::ScratchPlanV2,
    },
    ScratchV3 {
        slot: v2::MemberSlot,
        plan: v2::scratch::bootstrap::ScratchPlanV3,
    },
}

#[derive(Clone)]
pub(crate) struct SourcePlan {
    pub holder_id: v2::Id,
    pub source_association: v2::SourceAssociation,
    // Observed domain data is preserved, never rewritten to bless another owner.
    pub filesystem_domain: v2::FilesystemDomain,
    pub copies: Vec<SourceCopyPlan>,
}

#[derive(Clone)]
pub(crate) struct SourceCopyPlan {
    pub copy_id: v2::Id,
    pub owner: SourceCopyOwner,
    pub acquisition_operation_id: v2::Id,
    pub parent_copy_id: Option<v2::Id>,
}

#[derive(Clone)]
pub(crate) enum SourceCopyOwner {
    LocalSourceCell,
    LocalRoot {
        root_member_id: v2::Id,
    },
    RetainedAssets {
        asset_id: v2::Id,
    },
    RetainedRoleHolder {
        obligation_id: v2::Id,
        member_id: v2::Id,
        family_id: v2::Id,
        role: Role,
    },
}

#[derive(Clone)]
pub(crate) struct VodTaskPlan {
    pub root_member_id: v2::Id,
    pub generation_epoch: u64,
    pub source_association: v2::SourceAssociation,
    pub stdout_owner_id: v2::Id,
    pub diagnostic_owner_id: v2::Id,
    pub asset_id: v2::Id,
    pub sink_owner_id: v2::Id,
    pub inventory: v2::IoInventory,
}

impl TypedMemberPlan {
    pub(super) fn legacy(&self) -> Option<&PlannedMember> {
        match self {
            Self::Legacy(planned) => Some(planned),
            _ => None,
        }
    }

    pub(super) fn bind(self, owner_id: v2::Id, graph_id: v2::Id) -> v2::MemberPlan {
        match self {
            Self::Legacy(planned) => v2::MemberPlan::LegacyV1 { planned },
            Self::ScratchV1 { slot, scratch } => v2::MemberPlan::ScratchAssetsV1 { slot, scratch },
            Self::ScratchV2 { slot, plan } => v2::MemberPlan::ScratchAssetsV2 { slot, plan },
            Self::ScratchV3 { slot, plan } => v2::MemberPlan::ScratchAssetsV3 { slot, plan },
            Self::Source { slot, source } => {
                let member_id = slot.member_id;
                let copies = source
                    .copies
                    .into_iter()
                    .map(|copy| v2::SourceCopy {
                        copy_id: copy.copy_id,
                        owner: match copy.owner {
                            SourceCopyOwner::LocalSourceCell => v2::CopyOwner::DaemonSourceCell {
                                owner_id,
                                graph_id,
                                member_id,
                            },
                            SourceCopyOwner::LocalRoot { root_member_id } => {
                                v2::CopyOwner::OwnedRoot {
                                    graph_id,
                                    root_member_id,
                                }
                            }
                            SourceCopyOwner::RetainedAssets { asset_id } => {
                                v2::CopyOwner::RetainedAssets {
                                    asset_id,
                                    source_member_id: member_id,
                                }
                            }
                            SourceCopyOwner::RetainedRoleHolder {
                                obligation_id,
                                member_id,
                                family_id,
                                role,
                            } => v2::CopyOwner::RetainedRoleHolder {
                                obligation_id,
                                member_id,
                                family_id,
                                role,
                            },
                        },
                        acquisition_operation_id: copy.acquisition_operation_id,
                        parent_copy_id: copy.parent_copy_id,
                    })
                    .collect();
                v2::MemberPlan::SourceV1 {
                    slot,
                    source: v2::SourcePlan {
                        version: 1,
                        holder_id: source.holder_id,
                        member_id,
                        source_association: source.source_association,
                        access: v2::SourceAccess::ReadOnlyRegular,
                        recovery: v2::SourceRecovery::LocalSourceHandlesV1 {
                            owner_id,
                            graph_id,
                            filesystem_domain: source.filesystem_domain.clone(),
                        },
                        filesystem_domain: source.filesystem_domain,
                        planned_copies: copies,
                    },
                }
            }
            Self::VodOutputTask { slot, task } => v2::MemberPlan::VodOutputTaskV1 {
                task: v2::VodOutputTask {
                    version: 1,
                    actor: v2::VodActor::VodGenerationV1,
                    graph_id,
                    member_id: slot.member_id,
                    root_member_id: task.root_member_id,
                    generation_epoch: task.generation_epoch,
                    source_association: task.source_association,
                    stdout_owner_id: task.stdout_owner_id,
                    diagnostic_owner_id: task.diagnostic_owner_id,
                    asset_id: task.asset_id,
                    sink_owner_id: task.sink_owner_id,
                    inventory: task.inventory,
                },
                slot,
            },
        }
    }
}
