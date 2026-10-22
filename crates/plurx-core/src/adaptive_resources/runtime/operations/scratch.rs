//! Concrete scratch-directory write-ahead state in the existing runtime owner.
//! This module does not yet acquire a bootstrap parent or export a native permit.
//! Durable audit DTOs cannot be supplied as a native observation/close receipt.
use super::*;
use v2::scratch::{AssetChangeV2, AssetEventV1, AssetIoV1, ScratchFold, ScratchPlanV2};

pub(super) struct ScratchMember {
    plan: ScratchPlanV2,
    // On an impossible disagreement with a known committed event, leave None:
    // refuse new work while retaining all outer charges and native ownership.
    fold: Option<ScratchFold>,
    change: Option<(v2::Id, AssetChangeV2)>,
    directory_arm: Option<v2::Id>,
}

impl NodeResourceRuntime {
    /// Bind a real admitted typed slot, then durably arm its one directory job.
    /// Borrowed cancellation retains the exact journal op/IDs in OwnedRole. No
    /// callback, fd, path-open or guessed native identity is accepted or returned.
    pub(crate) async fn arm_scratch_directory(
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
                    v2::MemberPlan::ScratchAssetsV2 { slot, plan }
                        if slot.member_id.uuid() == member =>
                    {
                        Some(plan.clone())
                    }
                    _ => None,
                })
            })
            .ok_or(OperationError::State)?;
        if role.refunded
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
            .is_some_and(|t| t.effect != Effect::Scratch(member))
        {
            return Err(OperationError::State);
        }
        if let Some(scratch) = role.scratch.get(&member) {
            if scratch.plan != plan || scratch.fold.is_none() {
                return Err(OperationError::State);
            }
        } else {
            role.scratch.insert(
                member,
                ScratchMember {
                    fold: Some(ScratchFold::empty(&plan)?),
                    plan,
                    change: None,
                    directory_arm: None,
                },
            );
        }
        queue_directory(&self.journal, role, member, deadline)?;
        if let Some(transition) = &role.transition {
            if transition.effect != Effect::Scratch(member) {
                return Err(OperationError::State);
            }
            self.finish_transition(id, role).await?;
        }
        let scratch = role.scratch.get(&member).ok_or(OperationError::State)?;
        if scratch.change.is_some()
            || scratch
                .fold
                .as_ref()
                .is_none_or(|fold| fold.state.totals.pending_jobs != 1)
        {
            return Err(OperationError::State);
        }
        Ok(())
    }
}

/// Queue acceptance and native entry are separate. This concrete command can
/// be driven by the same retained supervisor after its original waiter goes.
fn queue_directory(
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
            .is_some_and(|t| t.effect != Effect::Scratch(member))
    {
        return Err(OperationError::State);
    }
    let scratch = role.scratch.get_mut(&member).ok_or(OperationError::State)?;
    if scratch.directory_arm.is_some() {
        return Ok(());
    }
    if role.transition.is_some() {
        return Err(OperationError::State);
    }
    // Logical IDs are reserved before enqueue, not native inode observations.
    let io_id = v2::Id::new(Uuid::new_v4())?;
    let change = AssetChangeV2 {
        scratch_member_id: v2::Id::new(member)?,
        asset_id: scratch.plan.asset_id,
        generation_epoch: scratch.plan.generation_epoch,
        previous: scratch
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state
            .head
            .clone(),
        event: AssetEventV1::ArmIo {
            io_id,
            operation: AssetIoV1::CreateDirectory {
                copy_id: v2::Id::new(Uuid::new_v4())?,
            },
        },
    };
    scratch.change = Some((io_id, change.clone()));
    scratch.directory_arm = Some(io_id);
    start_named_transition(
        journal,
        role,
        io_id.uuid(),
        Mutation::AssetChangeV2 { change },
        Effect::Scratch(member),
        deadline,
    )
}

/// Invoked only on THIS retained operation's known DB acknowledgement, before
/// acknowledging its worker-book entry. Repeated finish after acknowledgement
/// failure compares the exact head, never applies the same delta twice.
pub(super) fn apply_committed(
    role: &mut OwnedRole,
    member: Uuid,
    operation: Uuid,
) -> Result<(), OperationError> {
    let scratch = role.scratch.get_mut(&member).ok_or(OperationError::State)?;
    let (id, change) = scratch.change.as_ref().ok_or(OperationError::State)?;
    if id.uuid() != operation {
        return Err(OperationError::State);
    }
    if scratch
        .fold
        .as_ref()
        .and_then(|f| f.state.head.as_ref())
        .is_some_and(|h| h.operation_id == *id)
    {
        return Ok(());
    }
    let fold = scratch.fold.take().ok_or(OperationError::State)?;
    scratch.fold = Some(fold.apply(&scratch.plan, *id, change)?);
    Ok(())
}

pub(super) fn acknowledge(role: &mut OwnedRole, member: Uuid) -> Result<(), OperationError> {
    let scratch = role.scratch.get_mut(&member).ok_or(OperationError::State)?;
    scratch.change.take().ok_or(OperationError::State)?;
    Ok(())
}

#[cfg(test)]
mod tests;
