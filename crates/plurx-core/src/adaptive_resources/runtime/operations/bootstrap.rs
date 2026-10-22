//! V3 pre-entry state in the existing retained runtime bundle.
//! No audit DTO is accepted as native acquisition/closure authority. The actual
//! immutable configured-root/native adapter must consume this known arm in place.
use super::*;
use v2::scratch::bootstrap::*;
#[cfg(unix)]
mod native;

pub(super) struct BootstrapMember {
    plan: ScratchPlanV3,
    fold: Option<BootstrapFold>,
    change: Option<(v2::Id, AssetChangeV3)>,
    root_arm: Option<(v2::Id, v2::Id)>,
    #[cfg(unix)]
    native_root: Option<native::RootCell>,
}

impl NodeResourceRuntime {
    pub(crate) async fn arm_scratch_bootstrap(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        self.eligible(&bundle)?;
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let plan = role
            .typed_members
            .as_ref()
            .and_then(|members| {
                members.iter().find_map(|m| match m {
                    v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                        if slot.member_id.uuid() == member =>
                    {
                        Some(plan.clone())
                    }
                    _ => None,
                })
            })
            .ok_or(OperationError::State)?;
        if plan.bootstrap().authority.installation_id.uuid() != self.recovered.installation_id
            || self.rendition_root.as_deref()
                != Some(Path::new(
                    &plan.bootstrap().authority.resolved_absolute_root,
                ))
            || role.refunded
            || role.scratch.contains_key(&member)
            || !matches!(
                role.phase,
                Phase::Prepared | Phase::AcquisitionPossible | Phase::Sealed
            )
        {
            return Err(OperationError::State);
        }
        if !role.armed.contains(&member) {
            if role.transition.is_none() {
                start_transition(
                    &self.journal,
                    role,
                    Mutation::Arm {
                        members: vec![member],
                    },
                    Effect::Arm(member),
                    deadline,
                )?;
            } else if role
                .transition
                .as_ref()
                .is_none_or(|t| t.effect != Effect::Arm(member))
            {
                return Err(OperationError::State);
            }
            self.finish_transition(id, role).await?;
        }
        if Instant::now() >= deadline {
            return Err(OperationError::Deadline);
        }
        if role
            .transition
            .as_ref()
            .is_some_and(|t| t.effect != Effect::Bootstrap(member))
        {
            return Err(OperationError::State);
        }
        if let Some(bootstrap) = role.bootstrap.get(&member) {
            if bootstrap.plan != plan || bootstrap.fold.is_none() {
                return Err(OperationError::State);
            }
        } else {
            let obligation = v2::Id::new(
                role.handle
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .obligation_id(),
            )?;
            role.bootstrap.insert(
                member,
                BootstrapMember {
                    fold: Some(BootstrapFold::empty(
                        &plan,
                        obligation,
                        v2::Id::new(member)?,
                    )?),
                    plan,
                    change: None,
                    root_arm: None,
                    #[cfg(unix)]
                    native_root: None,
                },
            );
        }
        queue_root(&self.journal, role, member, deadline)?;
        if role.transition.is_some() {
            self.finish_transition(id, role).await?;
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let state = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state();
        if bootstrap.change.is_some() || state.asset.totals.pending_jobs != 1 {
            return Err(OperationError::State);
        }
        Ok(())
    }
}

fn queue_root(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    deadline: Instant,
) -> Result<(), OperationError> {
    if role.refunded
        || role.handle.is_none()
        || !role.armed.contains(&member)
        || !matches!(role.phase, Phase::AcquisitionPossible | Phase::Sealed)
        || role
            .transition
            .as_ref()
            .is_some_and(|t| t.effect != Effect::Bootstrap(member))
    {
        return Err(OperationError::State);
    }
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    if bootstrap.root_arm.is_some() {
        return Ok(());
    }
    if role.transition.is_some() {
        return Err(OperationError::State);
    }
    let io_id = v2::Id::new(Uuid::new_v4())?;
    let copy_id = v2::Id::new(Uuid::new_v4())?;
    let change = AssetChangeV3 {
        scratch_member_id: v2::Id::new(member)?,
        asset_id: bootstrap.plan.asset_id,
        generation_epoch: bootstrap.plan.generation_epoch,
        previous: bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .head,
        event: AssetEventV3::ArmBootstrapV1 {
            io_id,
            operation: BootstrapIoV1::OpenNamespaceRoot { copy_id },
        },
    };
    bootstrap.change = Some((io_id, change.clone()));
    bootstrap.root_arm = Some((io_id, copy_id));
    start_named_transition(
        journal,
        role,
        io_id.uuid(),
        Mutation::AssetChangeV3 { change },
        Effect::Bootstrap(member),
        deadline,
    )
}

pub(super) fn apply_committed(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    let (id, change) = bootstrap.change.as_ref().ok_or(OperationError::State)?;
    if id.uuid() != operation {
        return Err(OperationError::State);
    }
    if bootstrap
        .fold
        .as_ref()
        .is_some_and(|f| f.state().asset.head.is_some_and(|h| h.operation_id == *id))
    {
        return Ok(());
    }
    let fold = bootstrap.fold.take().ok_or(OperationError::State)?;
    bootstrap.fold = Some(fold.apply(&bootstrap.plan, *id, change)?);
    Ok(())
}

pub(super) fn acknowledge(role: &mut OwnedRole, member: Uuid) -> Result<(), OperationError> {
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    bootstrap.change.take().ok_or(OperationError::State)?;
    Ok(())
}
