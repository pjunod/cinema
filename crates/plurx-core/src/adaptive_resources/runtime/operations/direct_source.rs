//! Rootless direct Source closure from the original retained native cell.
//! Unknown journal commits retain the same proof and operation in this bundle.

use super::*;
use crate::resource_custody::PhysicalTypedSettlement;

pub(in crate::adaptive_resources) struct DirectSourceClosure {
    binding: ReservationBinding,
}
pub(in crate::adaptive_resources) struct DirectReadDomainClosure {
    binding: ReservationBinding,
}
impl DirectReadDomainClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

/// Exact inline runtime frames for one prepared, acquired, then settling
/// rootless role. Dynamic plan text, body owners and read jobs are separate.
pub(in crate::adaptive_resources::runtime) const fn direct_source_role_frame_bytes() -> usize {
    std::mem::size_of::<Bundle>()
        + std::mem::size_of::<OwnedRole>()
        + std::mem::size_of::<JournalOperation>()
        + std::mem::size_of::<Transition>()
        + std::mem::size_of::<OwnedPhysicalGraph>()
        + std::mem::size_of::<SealedPhysicalGraph>()
        + std::mem::size_of::<v2::Manifest>()
        + std::mem::size_of::<v2::Envelope>()
        + std::mem::size_of::<v2::Settlement>()
        + super::source_files::direct_source_frame_bytes()
}
impl DirectSourceClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

impl NodeResourceRuntime {
    /// Join and close the original registered SourceCell while its full role
    /// charge remains held. The cell retains the native close fact for the
    /// later exact typed settlement.
    pub(in crate::adaptive_resources::runtime) async fn close_direct_source_native(
        &self,
        owner: &OwnedPreparation,
        role_index: usize,
        member: Uuid,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let role = bundle
            .roles
            .get_mut(role_index)
            .ok_or(OperationError::State)?;
        if role.refunded {
            return Err(OperationError::State);
        }
        let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
        source.close_direct_parent().await
    }

    /// The caller has sealed new entry under the registry lock and consumed
    /// every exact source member only after its known native/journal closure.
    /// The proof token is private to that path; a serialized count cannot call
    /// this settlement operation.
    pub(in crate::adaptive_resources::runtime) async fn settle_direct_read_domain(
        &self,
        owner: &OwnedPreparation,
        member: Uuid,
        domain_id: Uuid,
        ready: super::super::direct_source::DirectReadDomainReady,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let needs_seal = {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed {
                return Err(OperationError::State);
            }
            let bundle_id = bundle.id;
            let role = bundle.roles.get_mut(0).ok_or(OperationError::State)?;
            if role.refunded {
                return Ok(());
            }
            if let Some(transition) = role.transition.as_mut() {
                if transition.effect == Effect::Settle {
                    if transition.operation.pending.is_none()
                        && !matches!(transition.operation.result, Some(Ok(_)))
                    {
                        transition.operation.retry(&self.journal, deadline)?;
                    }
                    return self.finish_transition(bundle_id, role).await;
                }
            }
            matches!(role.phase, Phase::Prepared | Phase::AcquisitionPossible)
                || role
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.effect == Effect::Seal)
        };
        if needs_seal {
            self.seal_role(owner, 0, deadline).await?;
        }
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(0).ok_or(OperationError::State)?;
        if role.refunded {
            return Ok(());
        }
        if role.transition.is_some()
            || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
            || role.graph.is_some()
            || !role.sources.is_empty()
            || !role.roots.is_empty()
            || !role.drains.is_empty()
            || !role.bootstrap.is_empty()
            || !role.scratch.is_empty()
        {
            return Err(OperationError::State);
        }
        let manifest = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if !matches!(manifest.as_slice(),
            [v2::MemberPlan::DirectReadDomainV1 { slot, domain }]
                if slot.member_id.uuid() == member && domain.domain_id.uuid() == domain_id)
        {
            return Err(OperationError::State);
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        if !ready.matches(handle.obligation_id(), member, domain_id) {
            return Err(OperationError::State);
        }
        let sealed = role.sealed.as_ref().ok_or(OperationError::State)?;
        let snapshot = sealed.snapshot().await;
        if snapshot.identity.generation != handle.graph_id()
            || !snapshot.sealed
            || snapshot.roots != 0
            || snapshot.writers != 0
            || snapshot.source_files != 0
        {
            return Err(OperationError::State);
        }
        let operation_id = Uuid::new_v4();
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id: v2::Id::new(operation_id)?,
            members: vec![v2::MemberDisposition::Closed {
                member_id: v2::Id::new(member)?,
                fact: v2::ClosedFact::DirectReadDomainV1 {
                    domain_id: v2::Id::new(domain_id)?,
                    close_operation_id: v2::Id::new(operation_id)?,
                },
            }],
        };
        self.close_retained_entry(role.binding)?;
        let key = TransitionKey {
            handle,
            operation_id,
            expected_sequence: role.sequence,
        };
        let receipt = PhysicalCloseReceipt::from_direct_read_domain(DirectReadDomainClosure {
            binding: role.binding,
        });
        role.typed_members = None;
        role.sealed = None;
        role.armed.clear();
        role.invoked.clear();
        role.transition = Some(Transition {
            operation: JournalOperation::transition(self.journal.change(
                key,
                Mutation::SettleTyped(Box::new(PhysicalTypedSettlement {
                    receipt,
                    settlement,
                })),
                deadline,
            )),
            effect: Effect::Settle,
        });
        self.finish_transition(bundle_id, role).await
    }

    pub(in crate::adaptive_resources::runtime) async fn settle_direct_source(
        &self,
        owner: &OwnedPreparation,
        role_index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let needs_seal = {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed {
                return Err(OperationError::State);
            }
            let bundle_id = bundle.id;
            let role = bundle
                .roles
                .get_mut(role_index)
                .ok_or(OperationError::State)?;
            if role.refunded {
                return Ok(());
            }
            if let Some(transition) = role.transition.as_mut() {
                if transition.effect == Effect::Settle {
                    if transition.operation.pending.is_none()
                        && !matches!(transition.operation.result, Some(Ok(_)))
                    {
                        transition.operation.retry(&self.journal, deadline)?;
                    }
                    return self.finish_transition(bundle_id, role).await;
                }
            }
            matches!(role.phase, Phase::Prepared | Phase::AcquisitionPossible)
                || role
                    .transition
                    .as_ref()
                    .is_some_and(|transition| transition.effect == Effect::Seal)
        };
        // The original graph contains no root, producer, or output member.
        // Seal still commits the exact source head before native closure.
        if needs_seal {
            self.seal_role(owner, role_index, deadline).await?;
        }
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle
            .roles
            .get_mut(role_index)
            .ok_or(OperationError::State)?;
        if role.refunded {
            return Ok(());
        }
        if role.transition.is_some()
            || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
            || role.graph.is_some()
        {
            return Err(OperationError::State);
        }
        let source = role.sources.get_mut(&member).ok_or(OperationError::State)?;
        source.close_direct_parent().await?;
        let operation_id = Uuid::new_v4();
        let fact = source.direct_closed_fact(operation_id)?;
        let manifest = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if !matches!(manifest.as_slice(),
            [v2::MemberPlan::DirectSourceV1 { slot, .. }] if slot.member_id.uuid() == member)
            || !role
                .response_budget
                .as_ref()
                .is_some_and(|budget| budget.closed_and_empty())
        {
            return Err(OperationError::State);
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let sealed = role.sealed.as_ref().ok_or(OperationError::State)?;
        let snapshot = sealed.snapshot().await;
        if snapshot.identity.generation != handle.graph_id()
            || !snapshot.sealed
            || snapshot.roots != 0
            || snapshot.writers != 0
            || snapshot.source_files != 0
            || !role.roots.is_empty()
            || !role.drains.is_empty()
            || !role.bootstrap.is_empty()
            || !role.scratch.is_empty()
        {
            return Err(OperationError::State);
        }
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id: v2::Id::new(operation_id)?,
            members: vec![v2::MemberDisposition::Closed {
                member_id: v2::Id::new(member)?,
                fact,
            }],
        };
        self.close_retained_entry(role.binding)?;
        let key = TransitionKey {
            handle,
            operation_id,
            expected_sequence: role.sequence,
        };
        // Only the original SourceCell's joined close can mint this private
        // capability. Drop bulky native/plan ownership before any refund.
        let receipt = PhysicalCloseReceipt::from_direct_source(DirectSourceClosure {
            binding: role.binding,
        });
        role.sources.clear();
        role.typed_members = None;
        role.response_budget = None;
        role.sealed = None;
        role.armed.clear();
        role.invoked.clear();
        role.transition = Some(Transition {
            operation: JournalOperation::transition(self.journal.change(
                key,
                Mutation::SettleTyped(Box::new(PhysicalTypedSettlement {
                    receipt,
                    settlement,
                })),
                deadline,
            )),
            effect: Effect::Settle,
        });
        self.finish_transition(bundle_id, role).await
    }
}
