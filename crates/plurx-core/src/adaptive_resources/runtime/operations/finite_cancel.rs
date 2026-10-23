//! Positive typed cancellation of a launched fixed root whose writer never
//! acquired. The original runtime retains every native owner and journal
//! operation across cancellation; a root-only fact is never a joined writer.
use super::*;
use crate::adaptive_resources::physical_graph::finite_vod::FiniteVodRootOnlyClosure;
use crate::adaptive_resources::runtime::operations::bootstrap::retirement::ProducerFiniteClosure;
use crate::resource_custody::{MemberKind, PhysicalTypedSettlement};

/// Consumed only by the core receipt constructor after every original source
/// and scratch owner has positively closed. No serialized fact can mint this.
pub(in crate::adaptive_resources) struct FiniteCancellationClosure {
    binding: ReservationBinding,
    _root: FiniteVodRootOnlyClosure,
    _output: ProducerFiniteClosure,
}
impl FiniteCancellationClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

impl NodeResourceRuntime {
    /// Resolve an interrupted root-only start, close its original output, and
    /// settle the same typed R after one known journal commit. Earlier source
    /// or bootstrap failures without this exact native proof stay fully held.
    pub(in crate::adaptive_resources::runtime) async fn settle_failed_finite_vod(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        {
            let mut bundle = owner.state.lock().await;
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
        }
        // This may return Unsupported after it has retained the distinct
        // original root-only closure. Accept only that exact retained state.
        let joined = self.join_finite_producer(owner, index).await;
        let member = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            if role
                .finite_vod
                .as_ref()
                .is_none_or(|finite| finite.root_only_closure.is_none())
            {
                return Err(joined.err().unwrap_or(OperationError::State));
            }
            if role.bootstrap.len() != 1 || role.retirement.is_some() {
                return Err(OperationError::State);
            }
            *role.bootstrap.keys().next().ok_or(OperationError::State)?
        };
        self.close_producer_finite_native(owner, index, member, deadline)
            .await?;
        let needs_seal = {
            let bundle = owner.state.lock().await;
            bundle.roles.get(index).ok_or(OperationError::State)?.phase != Phase::Sealed
        };
        if needs_seal {
            self.seal_role(owner, index, deadline).await?;
        }
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded {
            return Ok(());
        }
        if role.phase != Phase::Sealed
            || role.transition.is_some()
            || role.graph.is_some()
            || role.retirement.is_some()
            || role.bootstrap.len() != 1
        {
            return Err(OperationError::State);
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let root = finite
            .root_only_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        let sealed = role.sealed.as_ref().ok_or(OperationError::State)?;
        if !root.matches(sealed)
            || root.generation() != handle.graph_id()
            || finite.process_closure.is_some()
            || finite.producer_joined
        {
            return Err(OperationError::State);
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let (scratch_plan, scratch_head) = bootstrap.producer_close_fact(role.binding, member)?;
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if planned.len() != role.sources.len() + 3 {
            return Err(OperationError::State);
        }
        let close_id = v2::Id::new(root.close_operation_id())?;
        let mut facts = Vec::with_capacity(planned.len());
        for plan in planned {
            let member_id = v2::Id::new(plan.member_id())?;
            let fact = match plan {
                v2::MemberPlan::SourceV1 { .. } => role
                    .sources
                    .get(&plan.member_id())
                    .ok_or(OperationError::State)?
                    .finite_root_only_closed_fact(root)?,
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.member_id == root.root_member()
                        && planned.kind == MemberKind::Root
                        && planned.adapter == Adapter::ProcessRoot =>
                {
                    let Metric::Known(identity) = root.root().process else {
                        return Err(OperationError::State);
                    };
                    v2::ClosedFact::LegacyV1 {
                        fact: crate::resource_custody::ClosedMemberFact {
                            member_id: planned.member_id,
                            adapter: planned.adapter.clone(),
                            adapter_version: planned.adapter_version,
                            domain: planned.domain.clone(),
                            object_identity: format!("{}:{}", identity.pid, identity.start_marker),
                            evidence: "original fixed software AVC root waited; no writer registered; both original pipes and Child/ChildJob closed".into(),
                        },
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if task.member_id.uuid() == root.task_member()
                        && task.root_member_id.uuid() == root.root_member()
                        && task.graph_id.uuid() == root.generation() =>
                {
                    v2::ClosedFact::VodOutputTaskNeverAcquiredV1 {
                        task: task.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::ScratchAssetsV3 { plan, .. }
                    if plan == &scratch_plan && member_id.uuid() == member =>
                {
                    v2::ClosedFact::ScratchAssetsV3 {
                        plan: plan.clone(),
                        exact_head: scratch_head.clone(),
                        operation_id: scratch_head.operation_id,
                    }
                }
                _ => return Err(OperationError::State),
            };
            facts.push(v2::MemberDisposition::Closed { member_id, fact });
        }
        let operation_id = v2::Id::new(Uuid::new_v4())?;
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id,
            members: facts,
        };
        let key = TransitionKey {
            handle,
            operation_id: operation_id.uuid(),
            expected_sequence: role.sequence,
        };
        // All fallible data construction precedes the one-shot native takes.
        let output = role
            .bootstrap
            .get_mut(&member)
            .expect("validated producer output")
            .take_producer_close(role.binding, member)
            .expect("validated producer close is stable under bundle lock");
        let root = role
            .finite_vod
            .as_mut()
            .expect("validated finite role")
            .root_only_closure
            .take()
            .expect("validated original root-only closure");
        let closure = FiniteCancellationClosure {
            binding: role.binding,
            _root: root,
            _output: output,
        };
        let receipt = PhysicalCloseReceipt::from_finite_cancel(closure);
        role.bootstrap.clear();
        role.sources.clear();
        role.response_budget = None;
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
