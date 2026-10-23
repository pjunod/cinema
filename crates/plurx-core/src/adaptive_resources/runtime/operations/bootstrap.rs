//! V3 pre-entry state in the existing retained runtime bundle.
//! No audit DTO is accepted as native acquisition/closure authority. The actual
//! immutable configured-root/native adapter must consume this known arm in place.
use super::*;
use v2::scratch::bootstrap::*;
#[cfg(unix)]
use v2::scratch::{AssetEventV1, ScratchLifecycleV1};
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

/// Fully validated target fold while the actual native cells remain owned by
/// the source member. Construction and consumption must occur under the same
/// retained-bundle lock, with no await or source mutation between them.
#[cfg(unix)]
pub(super) struct PreparedBootstrapTransfer {
    target_plan: ScratchPlanV3,
    target_fold: BootstrapFold,
}

#[cfg(unix)]
impl PreparedBootstrapTransfer {
    /// Infallibly moves the original RootCell/publication/sync owners into the
    /// adopted member after every validation has completed. The caller must
    /// use the exact source member that produced this token under its held
    /// bundle lock, without an await or source mutation between calls.
    pub(super) fn move_to_retained(self, source: &mut BootstrapMember) -> BootstrapMember {
        BootstrapMember {
            plan: self.target_plan,
            fold: Some(self.target_fold),
            change: None,
            root_arm: None,
            root_ready: true,
            next_arm: None,
            native_root: source.native_root.take(),
            publication: source.publication.take(),
            sync: source.sync.take(),
        }
    }
}

impl BootstrapMember {
    pub(super) fn frozen_inventory(&self) -> Result<FrozenScratchV3, OperationError> {
        self.fold
            .as_ref()
            .ok_or(OperationError::State)?
            .frozen_snapshot(&self.plan)
            .map_err(Into::into)
    }

    #[cfg(unix)]
    pub(super) fn prepare_retained_transfer(
        &self,
        restored: &FrozenScratchV3,
        reference: &FrozenScratchRefV1,
        target_plan: &ScratchPlanV3,
        target_obligation: v2::Id,
        target_member: v2::Id,
    ) -> Result<PreparedBootstrapTransfer, OperationError> {
        let fold = self.fold.as_ref().ok_or(OperationError::State)?;
        let state = fold.state();
        if target_plan != &self.plan
            || !self.root_ready
            || self.root_arm.is_none()
            || self.change.is_some()
            || self.next_arm.is_some()
            || state.asset.lifecycle != ScratchLifecycleV1::Frozen
            || state.asset.totals.pending_jobs != 0
            || state.bootstrap.phase != BootstrapPhase::SinkReady
            || state.bootstrap.current.is_some()
            || state.bootstrap.possible_child.is_some()
        {
            return Err(OperationError::State);
        }
        let full = fold.frozen_snapshot(&self.plan)?;
        if &full != restored {
            return Err(OperationError::State);
        }
        validate_frozen_snapshot_ref(&self.plan, reference, &full)?;
        let directory = state
            .asset
            .directory
            .as_ref()
            .ok_or(OperationError::State)?;
        let parent = state
            .bootstrap
            .parent
            .as_ref()
            .ok_or(OperationError::State)?;
        let expected_directories = [parent.observed.clone(), directory.clone()];
        if !self
            .native_root
            .as_ref()
            .is_some_and(|root| root.retained_child_ready(&expected_directories))
            || !self
                .publication
                .as_ref()
                .is_some_and(|publication| publication.ready_for_transfer())
            || !self
                .sync
                .as_ref()
                .is_some_and(|sync| sync.ready_for_transfer())
        {
            return Err(OperationError::State);
        }
        let target_fold =
            fold.adopt_frozen(&self.plan, reference, target_obligation, target_member)?;
        Ok(PreparedBootstrapTransfer {
            target_plan: target_plan.clone(),
            target_fold,
        })
    }

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

    /// Advance only the admitted output's existing durable fence/freeze fold.
    /// This requires the original finite producer to be joined and retirement
    /// to have begun; it creates no native IO or close/refund authority.
    #[cfg(unix)]
    pub(in crate::adaptive_resources::runtime) async fn freeze_finite_output(
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
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        if role.refunded || !finite.producer_joined || !finite.retiring {
            return Err(OperationError::State);
        }
        let planned = role.typed_members.as_ref().is_some_and(|members| {
            members.iter().any(|planned| {
                matches!(planned,
                    v2::MemberPlan::ScratchAssetsV3 { slot, plan }
                        if slot.member_id.uuid() == member
                            && role.bootstrap.get(&member)
                                .is_some_and(|bootstrap| bootstrap.plan == *plan))
            })
        });
        if !planned {
            return Err(OperationError::State);
        }

        // A prior operation belongs to this exact bootstrap member or this
        // retirement cannot advance it. Resume the retained operation ID and
        // journal transition; never synthesize an acknowledgement.
        if role.transition.is_some() {
            if role
                .transition
                .as_ref()
                .is_none_or(|transition| transition.effect != Effect::Bootstrap(member))
            {
                return Err(OperationError::State);
            }
            self.finish_transition(id, role).await?;
        } else if role
            .bootstrap
            .get(&member)
            .ok_or(OperationError::State)?
            .change
            .is_some()
        {
            resume_change(&self.journal, role, member, deadline)?;
            self.finish_transition(id, role).await?;
        }

        // Each successful acknowledgment is followed by a fresh fold read.
        // At most Fence and Freeze can be newly queued in this call.
        for _ in 0..2 {
            let (lifecycle, exact_head, terminal_epoch, can_fence) = {
                let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
                if bootstrap.change.is_some() || role.transition.is_some() {
                    return Err(OperationError::State);
                }
                let state = bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state();
                (
                    state.asset.lifecycle,
                    state.asset.head,
                    bootstrap.plan.generation_epoch,
                    state.asset.totals.pending_jobs == 0
                        && state.bootstrap.phase == BootstrapPhase::SinkReady
                        && state.bootstrap.possible_child.is_none(),
                )
            };
            match lifecycle {
                ScratchLifecycleV1::Open => {
                    if !can_fence {
                        return Err(OperationError::State);
                    }
                    native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        Uuid::new_v4(),
                        AssetEventV1::Fence { terminal_epoch },
                        deadline,
                    )?;
                    self.finish_transition(id, role).await?;
                }
                ScratchLifecycleV1::Freezing => {
                    let exact_head_before_freeze = exact_head.ok_or(OperationError::State)?;
                    native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        Uuid::new_v4(),
                        AssetEventV1::Freeze {
                            exact_head_before_freeze,
                        },
                        deadline,
                    )?;
                    self.finish_transition(id, role).await?;
                }
                ScratchLifecycleV1::Frozen => return Ok(()),
                _ => return Err(OperationError::State),
            }
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        if bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .lifecycle
            == ScratchLifecycleV1::Frozen
        {
            Ok(())
        } else {
            Err(OperationError::State)
        }
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
