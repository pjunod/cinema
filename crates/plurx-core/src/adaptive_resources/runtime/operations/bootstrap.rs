//! V3 pre-entry state in the existing retained runtime bundle.
//! No audit DTO is accepted as native acquisition/closure authority. The actual
//! immutable configured-root/native adapter must consume this known arm in place.
use super::*;
use v2::scratch::bootstrap::*;
#[cfg(unix)]
pub(super) mod native;

pub(super) struct BootstrapMember {
    plan: ScratchPlanV3,
    fold: Option<BootstrapFold>,
    change: Option<(v2::Id, AssetChangeV3)>,
    root_arm: Option<(v2::Id, v2::Id)>,
    root_ready: bool,
    next_arm: Option<(v2::Id, BootstrapIoV1)>,
    #[cfg(unix)]
    native_root: Option<native::RootCell>,
    #[cfg(unix)]
    publication: Option<native::publication::Publication>,
    #[cfg(unix)]
    sync: Option<native::sync::Sync>,
}

impl BootstrapMember {
    /// Join already-registered native workers in their original cells. This
    /// does not launch work or create journal Arm/Observe transitions.
    pub(super) async fn join_registered_native(&mut self) -> Result<(), OperationError> {
        #[cfg(unix)]
        {
            let mut first_error = None;
            if let Some(root) = self.native_root.as_mut() {
                if let Err(error) = root.join_registered().await {
                    first_error = Some(error);
                }
            }
            if let Some(publication) = self.publication.as_mut() {
                if let Err(error) = publication.join_registered().await {
                    first_error.get_or_insert(error);
                }
            }
            if let Some(sync) = self.sync.as_mut() {
                if let Err(error) = sync.join_registered().await {
                    first_error.get_or_insert(error);
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        Ok(())
    }
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
            || plan.filesystem_domain.daemon_owner_id.uuid() != self.owner.owner_id
            || plan.filesystem_domain.os_boot != self.owner.os_boot
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
                    root_ready: false,
                    next_arm: None,
                    #[cfg(unix)]
                    native_root: None,
                    #[cfg(unix)]
                    publication: None,
                    #[cfg(unix)]
                    sync: None,
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
        return resume_change(journal, role, member, deadline);
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
    resume_change(journal, role, member, deadline)
}

// A command may be retained before queue submission fails (e.g. its deadline).
// Retry that EXACT command/id, not a new native operation or an invented ack.
fn resume_change(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    deadline: Instant,
) -> Result<(), OperationError> {
    if let Some(transition) = &role.transition {
        return if transition.effect == Effect::Bootstrap(member) {
            Ok(())
        } else {
            Err(OperationError::State)
        };
    }
    if let Some((id, change)) = role
        .bootstrap
        .get(&member)
        .ok_or(OperationError::State)?
        .change
        .as_ref()
        .cloned()
    {
        start_named_transition(
            journal,
            role,
            id.uuid(),
            Mutation::AssetChangeV3 { change },
            Effect::Bootstrap(member),
            deadline,
        )?;
    }
    Ok(())
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
