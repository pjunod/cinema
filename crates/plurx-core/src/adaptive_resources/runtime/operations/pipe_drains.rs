//! Journal composition for the concrete core-owned discard-root-pipes adapter.
//! No arbitrary future/callback enters this path. Closed task facts are data;
//! only a future whole-graph backend may combine them with other positive proof.
use super::*;
use crate::resource_custody::{ClosedMemberFact, MemberKind, RecoveryMethod};

pub(super) const LOCATOR: &str = "discard-root-pipes-v1";

impl NodeResourceRuntime {
    pub(crate) async fn spawn_pipe_drain(
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
            .members
            .iter()
            .find(|plan| plan.member_id == member)
            .cloned()
            .ok_or(OperationError::State)?;
        // This codec can describe OTHER pipe writers. Only this named concrete
        // implementation has the no-detach/no-export audit used below.
        let root_id = plan.owned_by.ok_or(OperationError::State)?;
        if plan.kind != MemberKind::Task
            || plan.adapter != Adapter::RootPipeTask
            || plan.adapter_version != 1
            || plan.locator != LOCATOR
            || plan.depends_on != [root_id]
            || !plan.protected_paths.is_empty()
            || !matches!(plan.recovery, RecoveryMethod::LiveOnly { .. })
            || role.refunded
            || role.graph.is_none()
            || role.invoked.contains(&member)
            || role.armed.contains(&member)
            || role
                .transition
                .as_ref()
                .is_some_and(|t| t.effect != Effect::Arm(member))
        {
            return Err(OperationError::State);
        }
        let root = *role.roots.get(&root_id).ok_or(OperationError::State)?;
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
        }
        self.finish_transition(id, role).await?;
        if Instant::now() >= deadline {
            seal_and_hold(
                &self.journal,
                role,
                "pipe drain deadline elapsed after arm",
                deadline,
            )?;
            return Err(OperationError::Deadline);
        }
        // A cancellation at the graph lock cannot replay the slot. If actual
        // task creation occurred, its JoinHandle is already in graph custody.
        role.invoked.insert(member);
        let graph = role.graph.as_mut().ok_or(OperationError::State)?;
        let arm = DurableArm {
            binding: role.binding,
            generation: graph.identity().generation,
        };
        let task = match graph.spawn_durable_pipe_drain(root, member, arm).await {
            Ok(task) => task,
            Err(error) => {
                seal_and_hold(
                    &self.journal,
                    role,
                    "pipe drain acquisition incomplete",
                    deadline,
                )?;
                return Err(error.into());
            }
        };
        role.drains.insert(member, task);
        let mutation = acquired_mutation(
            role,
            ObservedIdentity {
                version: 1,
                member_id: member,
                adapter: Adapter::RootPipeTask,
                adapter_version: 1,
                domain: plan.domain,
                object_identity: task.object_identity(),
                process: None,
            },
        )?;
        start_transition(&self.journal, role, mutation, Effect::Enrich, deadline)?;
        if let Err(error) = self.finish_transition(id, role).await {
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
            return Err(error);
        }
        Ok(())
    }

    /// Existing supervision seals the complete role first. This closes only an
    /// audited pipe task; it does NOT mint or submit a physical receipt/refund.
    pub(crate) async fn close_pipe_drain(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
    ) -> Result<ClosedMemberFact, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.graph.is_some() || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld) {
            return Err(OperationError::State);
        }
        if let Some(fact) = role.closed_drains.get(&member) {
            return Ok(fact.clone());
        }
        let task = *role.drains.get(&member).ok_or(OperationError::State)?;
        let plan = role
            .members
            .iter()
            .find(|plan| plan.member_id == member)
            .ok_or(OperationError::State)?;
        role.sealed
            .as_ref()
            .ok_or(OperationError::State)?
            .close_pipe_drain(task)
            .await?;
        let fact = ClosedMemberFact {
            member_id: member,
            adapter: Adapter::RootPipeTask,
            adapter_version: 1,
            domain: plan.domain.clone(),
            object_identity: task.object_identity(),
            evidence:
                "sealed original core pipe-drain future abort requested and joined; pipes dropped"
                    .into(),
        };
        role.closed_drains.insert(member, fact.clone());
        Ok(fact)
    }
}

fn seal_and_hold(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    reason: &str,
    deadline: Instant,
) -> Result<(), OperationError> {
    if let Some(graph) = role.graph.take() {
        role.sealed = Some(graph.seal());
    }
    start_transition(
        journal,
        role,
        Mutation::Hold {
            reason: reason.into(),
        },
        Effect::Hold,
        deadline,
    )
}

#[cfg(test)]
mod tests;
