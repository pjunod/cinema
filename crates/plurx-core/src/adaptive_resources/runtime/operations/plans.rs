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
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    DirectSource {
        slot: v2::MemberSlot,
        source: DirectSourcePlan,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    FixedProbeDiscoverSource {
        slot: v2::MemberSlot,
        source: FixedProbeDiscoverPlan,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    DirectReadDomain {
        slot: v2::MemberSlot,
        domain_id: v2::Id,
    },
    VodOutputTask {
        slot: v2::MemberSlot,
        task: VodTaskPlan,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    CopyOutputTask {
        slot: v2::MemberSlot,
        task: VodTaskPlan,
    },
    #[expect(
        dead_code,
        reason = "The legacy known-probe process-local constructor is retained with its durable protocol; current production and fixtures construct cold FixedProbeDiscoverTask instead."
    )]
    FixedProbeTask {
        slot: v2::MemberSlot,
        task: v2::FixedProbeTaskV1,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    FixedProbeDiscoverTask {
        slot: v2::MemberSlot,
        task: v2::FixedProbeDiscoverTaskV1,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    FixedPlanningEngineTask {
        slot: v2::MemberSlot,
        task: v2::FixedPlanningEngineTaskV1,
    },
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy/fixed planning constructors are Linux-only; preserve the portable typed-plan match and ownership shape."
        )
    )]
    FixedProbeMetadata {
        slot: v2::MemberSlot,
        metadata: v2::FixedProbeMetadataV1,
    },
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Legacy process-local manifest constructor has fixture callers only; current production builds cold planning and V3 scratch. Durable decoding/recovery protocol is unchanged."
        )
    )]
    ScratchV1 {
        slot: v2::MemberSlot,
        scratch: v2::ScratchAssets,
    },
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Legacy process-local manifest constructor has fixture callers only; current production builds cold planning and V3 scratch. Durable decoding/recovery protocol is unchanged."
        )
    )]
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
pub(crate) struct DirectSourcePlan {
    pub holder_id: v2::Id,
    pub media_file_id: String,
    pub recipe_source_binding: String,
    pub provenance: v2::DirectSourceProvenanceV1,
    pub absolute_path: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
    pub expectation: v2::DirectSourceExpectationV1,
    pub filesystem_domain: v2::FilesystemDomain,
    pub copy_id: v2::Id,
    pub acquisition_operation_id: v2::Id,
    pub read_domain: DirectReadDomainRef,
}

#[derive(Clone)]
pub(crate) struct FixedProbeDiscoverPlan {
    pub holder_id: v2::Id,
    pub media_file_id: String,
    pub recipe_source_binding: String,
    pub absolute_path: String,
    pub scanner_size: i64,
    pub scanner_mtime: i64,
    pub filesystem_domain: v2::FilesystemDomain,
    pub copies: Vec<SourceCopyPlan>,
}

#[derive(Clone)]
pub(crate) enum DirectReadDomainRef {
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy domain selectors are constructed only on Linux; preserve cross-platform typed binding and refusal paths."
        )
    )]
    BatchRole(usize),
    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            dead_code,
            reason = "Current direct/copy domain selectors are constructed only on Linux; preserve cross-platform typed binding and refusal paths."
        )
    )]
    Existing(Uuid),
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained-source copy acquisition has binder fixtures but is not yet routed by production source adapters; preserve the graph-bound binder and serialized protocol."
        )
    )]
    RetainedAssets {
        asset_id: v2::Id,
    },
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained-source copy acquisition has binder fixtures but is not yet routed by production source adapters; preserve the graph-bound binder and serialized protocol."
        )
    )]
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

    pub(super) fn bind(
        self,
        owner_id: v2::Id,
        graph_id: v2::Id,
        obligation_ids: &[Uuid],
    ) -> Result<v2::MemberPlan, CustodyError> {
        Ok(match self {
            Self::Legacy(planned) => v2::MemberPlan::LegacyV1 { planned },
            Self::ScratchV1 { slot, scratch } => v2::MemberPlan::ScratchAssetsV1 { slot, scratch },
            Self::ScratchV2 { slot, plan } => v2::MemberPlan::ScratchAssetsV2 { slot, plan },
            Self::ScratchV3 { slot, plan } => v2::MemberPlan::ScratchAssetsV3 { slot, plan },
            Self::DirectReadDomain { slot, domain_id } => v2::MemberPlan::DirectReadDomainV1 {
                slot,
                domain: v2::DirectReadDomainPlanV1 {
                    version: 1,
                    domain_id,
                    owner_id,
                    graph_id,
                    maximum_parallel_reads: 1,
                },
            },
            Self::FixedProbeDiscoverSource { slot, source } => {
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
                v2::MemberPlan::FixedProbeDiscoverSourceV1 {
                    slot,
                    source: v2::FixedProbeDiscoverSourceV1 {
                        version: 1,
                        holder_id: source.holder_id,
                        member_id,
                        media_file_id: source.media_file_id,
                        recipe_source_binding: source.recipe_source_binding,
                        absolute_path: source.absolute_path,
                        scanner_size: source.scanner_size,
                        scanner_mtime: source.scanner_mtime,
                        filesystem_domain: source.filesystem_domain.clone(),
                        planned_copies: copies,
                        recovery: v2::SourceRecovery::LocalSourceHandlesV1 {
                            owner_id,
                            graph_id,
                            filesystem_domain: source.filesystem_domain,
                        },
                    },
                }
            }
            Self::DirectSource { slot, source } => v2::MemberPlan::DirectSourceV1 {
                source: v2::DirectSourcePlanV1 {
                    version: 1,
                    holder_id: source.holder_id,
                    member_id: slot.member_id,
                    media_file_id: source.media_file_id,
                    recipe_source_binding: source.recipe_source_binding,
                    provenance: source.provenance,
                    absolute_path: source.absolute_path,
                    scanner_size: source.scanner_size,
                    scanner_mtime: source.scanner_mtime,
                    expectation: source.expectation,
                    access: v2::SourceAccess::ReadOnlyRegular,
                    filesystem_domain: source.filesystem_domain.clone(),
                    planned_copies: vec![v2::SourceCopy {
                        copy_id: source.copy_id,
                        owner: v2::CopyOwner::DaemonSourceCell {
                            owner_id,
                            graph_id,
                            member_id: slot.member_id,
                        },
                        acquisition_operation_id: source.acquisition_operation_id,
                        parent_copy_id: None,
                    }],
                    recovery: v2::SourceRecovery::LocalSourceHandlesV1 {
                        owner_id,
                        graph_id,
                        filesystem_domain: source.filesystem_domain,
                    },
                    read_domain_obligation_id: v2::Id::new(match source.read_domain {
                        DirectReadDomainRef::BatchRole(index) => *obligation_ids
                            .get(index)
                            .ok_or_else(CustodyError::invalid_reference)?,
                        DirectReadDomainRef::Existing(id) => id,
                    })?,
                },
                slot,
            },
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
            Self::FixedProbeTask { slot, task } => v2::MemberPlan::FixedProbeTaskV1 { slot, task },
            Self::FixedProbeDiscoverTask { slot, task } => {
                v2::MemberPlan::FixedProbeDiscoverTaskV1 { slot, task }
            }
            Self::FixedPlanningEngineTask { slot, task } => {
                v2::MemberPlan::FixedPlanningEngineTaskV1 { slot, task }
            }
            Self::FixedProbeMetadata { slot, metadata } => {
                v2::MemberPlan::FixedProbeMetadataV1 { slot, metadata }
            }
            Self::CopyOutputTask { slot, task } => v2::MemberPlan::VodOutputTaskV1 {
                task: v2::VodOutputTask {
                    version: 1,
                    actor: v2::VodActor::RollingCopyV1,
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
        })
    }
}
