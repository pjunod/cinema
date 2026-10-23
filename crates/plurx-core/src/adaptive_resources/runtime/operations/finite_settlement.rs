//! Retained S cleanup and its exact original proof-bearing settlement command.
//! A cancelled waiter loses no native owner, operation ID or physical receipt.
use super::*;
use crate::resource_custody::PhysicalTypedSettlement;

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) async fn settle_retained_finite_output(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        requested_member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let member = {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed {
                return Err(OperationError::State);
            }
            let bundle_id = bundle.id;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            if role.refunded {
                return Ok(());
            }
            if role
                .transition
                .as_ref()
                .is_some_and(|t| t.effect == Effect::Settle)
            {
                let transition = role.transition.as_mut().expect("selected settlement");
                if transition.operation.pending.is_none()
                    && !matches!(transition.operation.result, Some(Ok(_)))
                {
                    transition.operation.retry(&self.journal, deadline)?;
                }
                return self.finish_transition(bundle_id, role).await;
            }
            let member = retained_member(role, requested_member)?;
            self.close_retained_entry(role.binding)?;
            member
        };
        self.close_retained_finite_native(owner, index, member, deadline)
            .await?;
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded {
            return Ok(());
        }
        // A concurrent eviction may already have retained the one original
        // proof. Continue its command rather than attempting another take.
        if role
            .transition
            .as_ref()
            .is_some_and(|t| t.effect == Effect::Settle)
        {
            let transition = role.transition.as_mut().expect("selected settlement");
            if transition.operation.pending.is_none()
                && !matches!(transition.operation.result, Some(Ok(_)))
            {
                transition.operation.retry(&self.journal, deadline)?;
            }
            return self.finish_transition(bundle_id, role).await;
        }
        if role.transition.is_some() || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld) {
            return Err(OperationError::State);
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let (plan, exact_head) = bootstrap.retained_close_fact(role.binding, member)?;
        let manifest_members = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if !matches!(manifest_members.as_slice(),
            [v2::MemberPlan::ScratchAssetsV3 { slot, plan: expected }]
                if slot.member_id.uuid() == member && expected == &plan)
        {
            return Err(OperationError::State);
        }
        let operation_id = v2::Id::new(Uuid::new_v4())?;
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id,
            members: vec![v2::MemberDisposition::Closed {
                member_id: v2::Id::new(member)?,
                fact: v2::ClosedFact::ScratchAssetsV3 {
                    plan,
                    exact_head: exact_head.clone(),
                    operation_id: exact_head.operation_id,
                },
            }],
        };
        let key = TransitionKey {
            handle,
            operation_id: operation_id.uuid(),
            expected_sequence: role.sequence,
        };
        let closure = role
            .bootstrap
            .get_mut(&member)
            .expect("validated original retained owner")
            .take_retained_close(role.binding, member)?;
        // No await or fallible validation after consuming this one-shot native
        // proof. Queue refusal retains this exact command in JournalOperation.
        let receipt = PhysicalCloseReceipt::from_retained_finite(closure);
        // Native closure is proven. Dispose the original closed cells and
        // their retained inventories before the journal can authorize refund.
        // The only live authority is now the original command below.
        role.bootstrap.clear();
        role.retirement = None;
        role.typed_members = None;
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

fn retained_member(role: &OwnedRole, requested: Uuid) -> Result<Uuid, OperationError> {
    if role.retirement.is_none() {
        return Err(OperationError::State);
    }
    if role.bootstrap.contains_key(&requested) {
        return Ok(requested);
    }
    role.retirement
        .as_ref()
        .and_then(|retirement| retirement.resolve_source_member(requested))
        .ok_or(OperationError::State)
}
