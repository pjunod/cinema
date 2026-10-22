//! Compose the retained live graph's actual proof with its exact audit inventory.
//! This is not recovery, a source/output adapter, or a platform capability claim.
use super::*;
use crate::resource_custody::{ClosedMemberFact, MemberKind, RecoveryMethod};

impl NodeResourceRuntime {
    /// Queue, but do not yet refund, one exact live settlement. Resume the owned
    /// journal transition with `resume_role`; retries must never mint new proof.
    pub(crate) async fn queue_live_settlement(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let facts = live_member_facts(role)?;
        // Fence successor registration BEFORE cleanup, not after receipt issue.
        self.close_retained_entry(role.binding)?;
        if Instant::now() >= deadline {
            return Err(OperationError::Deadline);
        }
        let key = TransitionKey {
            handle: role.handle.clone().ok_or(OperationError::State)?,
            operation_id: Uuid::new_v4(),
            expected_sequence: role.sequence,
        };
        let sealed = role.sealed.as_ref().ok_or(OperationError::State)?;
        let inventory = sealed.snapshot().await;
        if inventory.identity.generation != key.handle.graph_id()
            || !inventory.sealed
            || !inventory.acquired
            || inventory.roots != role.roots.len()
            || inventory.writers != role.drains.len()
            || inventory.source_files != 0
        {
            // This path has no source-file/typed-resource manifest adapter.
            // A graph receipt can close such handles, but cannot invent their
            // omitted journal facts or authorize their retirement here.
            return Err(OperationError::State);
        }
        // The bundle remains locked: no member, sequence or competing transition
        // can change while graph-owned native cleanup awaits. Cancellation here
        // leaves the original sealed graph in the runtime book, without a proof.
        let receipt = sealed.try_live_close_receipt().await?;
        // NO await or fallible state lookup after the one-shot receipt. Even an
        // elapsed deadline/full queue returns the ORIGINAL command into owned
        // state. Unknown commits keep the same operation ID and payload through
        // JournalOperation::retry; facts alone cannot authorize any refund.
        role.transition = Some(Transition {
            operation: JournalOperation::transition(self.journal.change(
                key,
                Mutation::Settle(PhysicalSettlement { receipt, facts }),
                deadline,
            )),
            effect: Effect::Settle,
        });
        Ok(())
    }
}

/// Prebuild data before the graph disposes its native roots. Every named slot
/// must have its actual once-only acquisition selection; never invent a fact
/// for an uninvoked member or mislabel unsupported resources as local handles.
fn live_member_facts(role: &OwnedRole) -> Result<Vec<ClosedMemberFact>, OperationError> {
    if role.transition.is_some()
        // Typed retained assets require their complete private native inventory
        // and conserving transfer. Refuse BEFORE consuming any graph receipt.
        || role.typed_members.is_some()
        || role.refunded
        || role.receipt.is_some()
        || role.handle.is_none()
        || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
        || role.graph.is_some()
        || role.sealed.is_none()
        || role.roots.is_empty()
    {
        return Err(OperationError::State);
    }
    let ids: BTreeSet<_> = role.members.iter().map(|plan| plan.member_id).collect();
    let acquired: BTreeSet<_> = role
        .roots
        .keys()
        .chain(role.drains.keys())
        .copied()
        .collect();
    if ids.len() != role.members.len()
        || ids != role.armed
        || ids != role.invoked
        || ids != acquired
        || acquired.len() != role.roots.len() + role.drains.len()
    {
        return Err(OperationError::State);
    }
    let mut objects = BTreeSet::new();
    let mut facts = Vec::with_capacity(role.members.len());
    for plan in &role.members {
        if plan.adapter_version != 1
            || plan.maximum_cardinality != 1
            || !plan.protected_paths.is_empty()
            || !matches!(plan.recovery, RecoveryMethod::LiveOnly { .. })
        {
            return Err(OperationError::State);
        }
        let (object_identity, evidence) = match (&plan.kind, &plan.adapter) {
            (MemberKind::Root, Adapter::ProcessRoot)
                if plan.owned_by.is_none() && plan.depends_on.is_empty() =>
            {
                let root = role
                    .roots
                    .get(&plan.member_id)
                    .ok_or(OperationError::State)?;
                let Metric::Known(process) = root.process else {
                    return Err(AdmissionRefusal::InsufficientEvidence.into());
                };
                (
                    format!("{}:{}", process.pid, process.start_marker),
                    "original live graph root waited; original containment empty; native owner disposed",
                )
            }
            (MemberKind::Task, Adapter::RootPipeTask)
                if plan.locator == pipe_drains::LOCATOR
                    && plan.owned_by.is_some_and(|root| {
                        plan.depends_on == [root] && role.roots.contains_key(&root)
                    }) =>
            {
                let drain = role
                    .drains
                    .get(&plan.member_id)
                    .ok_or(OperationError::State)?;
                (
                    drain.object_identity(),
                    "original core discard-root-pipes task joined and its owned pipes dropped",
                )
            }
            _ => return Err(OperationError::State),
        };
        if !objects.insert(object_identity.clone()) {
            return Err(OperationError::State);
        }
        facts.push(ClosedMemberFact {
            member_id: plan.member_id,
            adapter: plan.adapter.clone(),
            adapter_version: plan.adapter_version,
            domain: plan.domain.clone(),
            object_identity,
            evidence: evidence.into(),
        });
    }
    Ok(facts)
}

#[cfg(test)]
mod tests;
