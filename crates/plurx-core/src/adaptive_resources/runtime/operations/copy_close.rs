//! Positive whole-role closure for the fixed rolling copy graph. The source,
//! parser, root and output proofs are retained together until the same typed
//! journal settlement has a known result.
use super::*;
use crate::adaptive_resources::physical_graph::copy_vod::CopyProcessClosure;
use crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure;
use crate::adaptive_resources::runtime::operations::bootstrap::retirement::ProducerFiniteClosure;
use crate::resource_custody::{MemberKind, PhysicalTypedSettlement};

pub(in crate::adaptive_resources) struct CopyCancellationClosure {
    binding: ReservationBinding,
    _process: CopyCancelProcess,
    _output: Option<ProducerFiniteClosure>,
}
enum CopyCancelProcess {
    NeverRoot { _proof: FiniteVodNeverRootClosure },
    Joined { _proof: CopyProcessClosure },
}
impl CopyCancellationClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

impl NodeResourceRuntime {
    /// Existing daemon maintenance may retry only the original failed copy
    /// admissions. This projection never creates another R or native owner;
    /// an unknown prepare/settlement leaves the exact book entry charged.
    pub(in crate::adaptive_resources::runtime) fn has_failed_copy_admission_inner(&self) -> bool {
        let Ok(book) = self.operations.lock() else {
            return true;
        };
        book.bundles.values().any(|state| {
            state.try_lock().map_or(true, |bundle| {
                bundle.roles.iter().any(|role| {
                    (role.copy_admission_failed
                        || role.copy_vod.as_ref().is_some_and(|copy| {
                            copy.cleanup_requested
                                .load(std::sync::atomic::Ordering::Acquire)
                        }))
                        && !role.refunded
                })
            })
        })
    }

    pub(in crate::adaptive_resources::runtime) async fn maintain_failed_copy_admissions_inner(
        &self,
        deadline: Instant,
    ) -> Result<usize, OperationError> {
        let selections = self.retained_preparations()?;
        let mut completed = 0usize;
        for preparation in selections {
            if Instant::now() >= deadline {
                break;
            }
            let selected = {
                let bundle = preparation.state.lock().await;
                bundle.roles.len() == 1
                    && !bundle.roles[0].refunded
                    && (bundle.roles[0].copy_admission_failed
                        || bundle.roles[0].copy_vod.as_ref().is_some_and(|copy| {
                            copy.cleanup_requested
                                .load(std::sync::atomic::Ordering::Acquire)
                        }))
            };
            if !selected {
                continue;
            }
            if self
                .resolve_preparation(&preparation, deadline)
                .await
                .is_err()
            {
                continue;
            }
            let (pending_close, output_member) = {
                let bundle = preparation.state.lock().await;
                let role = &bundle.roles[0];
                (
                    role.pending_finite_close.is_some()
                        || role
                            .transition
                            .as_ref()
                            .is_some_and(|t| t.effect == Effect::Settle),
                    role.copy_vod.as_ref().and_then(|_| {
                        role.typed_members.as_ref().and_then(|members| {
                            members.iter().find_map(|member| match member {
                                v2::MemberPlan::ScratchAssetsV3 { slot, .. } => {
                                    Some(slot.member_id.uuid())
                                }
                                _ => None,
                            })
                        })
                    }),
                )
            };
            if !pending_close {
                let Some(output_member) = output_member else {
                    continue;
                };
                if self
                    .prepare_copy_close(&preparation, 0, output_member, true, deadline)
                    .await
                    .is_err()
                {
                    continue;
                }
            }
            let mut selection = Some(preparation);
            if self
                .finish_finite_close(&mut selection, 0, deadline)
                .await
                .is_ok()
            {
                completed += 1;
            }
        }
        Ok(completed)
    }

    /// Positively close a copy role whose admitted constructor failed before
    /// the first source/open/root entry. The sealed original graph, not a
    /// caller's failed allocation, proves that no native member escaped.
    async fn prepare_copy_never_root_close(
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
            let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
            if role.refunded
                || copy.entered
                || copy.task.is_some()
                || !role.roots.is_empty()
                || !role.bootstrap.is_empty()
                || role.sources.len() > copy.source_members.len()
                || role
                    .sources
                    .keys()
                    .any(|member| !copy.source_members.contains(member))
                || role
                    .armed
                    .iter()
                    .any(|member| !copy.source_members.contains(member))
                || role
                    .invoked
                    .iter()
                    .any(|member| !copy.source_members.contains(member))
                || role.retirement.is_some()
            {
                return Err(OperationError::State);
            }
            if role.transition.is_some() {
                if !role
                    .transition
                    .as_ref()
                    .is_some_and(|transition| matches!(transition.effect, Effect::Source(_)))
                {
                    return Err(OperationError::State);
                }
                // An uncertain original source observation stays held. A
                // known result is replayed under its original operation ID.
                self.finish_transition(bundle_id, role).await?;
            }
            if !role.finite_app.as_ref().is_some_and(|state| state.seal()) {
                return Err(OperationError::State);
            }
        }
        let needs_seal = {
            let bundle = owner.state.lock().await;
            bundle.roles.get(index).ok_or(OperationError::State)?.phase != Phase::Sealed
        };
        if needs_seal {
            self.seal_role(owner, index, deadline).await?;
        }
        // The original graph's absence check waits on its native custody
        // mutex. Release the bundle guard first; the sealed application gate
        // prevents a new entry while this one-shot witness is being minted.
        let absent_candidate = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
            if copy.never_root_closure.is_none() {
                Some((
                    role.sealed
                        .as_ref()
                        .ok_or(OperationError::State)?
                        .select_same(),
                    copy.root_member,
                    copy.task_member,
                    copy.acquisition_id,
                ))
            } else {
                None
            }
        };
        if let Some((graph, root_member, task_member, acquisition_id)) = absent_candidate {
            let witness = graph
                .try_finite_vod_never_root_closure(root_member, task_member, acquisition_id)
                .await?;
            let mut bundle = owner.state.lock().await;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
            if copy.never_root_closure.is_some() {
                return Err(OperationError::State);
            }
            copy.never_root_closure = Some(witness);
        }
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.phase != Phase::Sealed
            || role.transition.is_some()
            || !role.roots.is_empty()
            || !role.bootstrap.is_empty()
        {
            return Err(OperationError::State);
        }
        let proof = role
            .copy_vod
            .as_ref()
            .and_then(|copy| copy.never_root_closure.as_ref())
            .ok_or(OperationError::State)?;
        for source in role.sources.values_mut() {
            source.close_parent_before_root(proof).await?;
        }
        if let Some(budget) = &role.response_budget {
            budget.begin_close_if_empty()?;
        }
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let sealed = role.sealed.as_ref().ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        let absent = copy
            .never_root_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        if !absent.matches(sealed)
            || absent.generation() != handle.graph_id()
            || absent.root_member() != copy.root_member
            || absent.task_member() != copy.task_member
            || absent.acquisition_id() != copy.acquisition_id
        {
            return Err(OperationError::State);
        }
        let close_id = v2::Id::new(absent.close_operation_id())?;
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if planned.len() != 5 {
            return Err(OperationError::State);
        }
        let mut facts = Vec::with_capacity(planned.len());
        for plan in planned {
            let member_id = v2::Id::new(plan.member_id())?;
            let fact = match plan {
                v2::MemberPlan::SourceV1 { .. }
                    if copy.source_members.contains(&plan.member_id()) =>
                {
                    match role.sources.get(&plan.member_id()) {
                        Some(source) => source.finite_never_root_closed_fact(absent)?,
                        None if !role.armed.contains(&plan.member_id())
                            && !role.invoked.contains(&plan.member_id()) =>
                        {
                            source_files::finite_unregistered_closed_fact(plan, absent)?
                        }
                        None => return Err(OperationError::State),
                    }
                }
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.member_id == copy.root_member
                        && planned.kind == MemberKind::Root
                        && planned.adapter == Adapter::ProcessRoot =>
                {
                    v2::ClosedFact::LegacyNeverAcquiredV1 {
                        planned: planned.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if task.actor == v2::VodActor::RollingCopyV1
                        && task.root_member_id.uuid() == copy.root_member
                        && task.member_id.uuid() == copy.task_member
                        && task.graph_id.uuid() == absent.generation() =>
                {
                    v2::ClosedFact::VodOutputTaskNeverAcquiredV1 {
                        task: task.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::ScratchAssetsV3 { plan, .. } => {
                    v2::ClosedFact::ScratchAssetsNeverAcquiredV3 {
                        plan: plan.clone(),
                        operation_id: close_id,
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
        // No fallible validation or await after moving the one-shot witness.
        let process = role
            .copy_vod
            .as_mut()
            .expect("validated copy role")
            .never_root_closure
            .take()
            .expect("validated graph non-entry");
        let receipt = PhysicalCloseReceipt::from_copy_cancel(CopyCancellationClosure {
            binding: role.binding,
            _process: CopyCancelProcess::NeverRoot { _proof: process },
            _output: None,
        });
        role.response_budget = None;
        role.sources.clear();
        role.armed.clear();
        role.invoked.clear();
        role.members.clear();
        role.typed_members = None;
        role.sealed = None;
        role.copy_vod = None;
        role.pending_finite_close = Some(PendingFiniteClose {
            key,
            mutation: Mutation::SettleTyped(Box::new(PhysicalTypedSettlement {
                receipt,
                settlement,
            })),
            effect: Effect::Settle,
        });
        Ok(())
    }

    /// Stop and close the complete joined copy graph under its original full
    /// R charge. The settlement stays pending until the separate application
    /// alias fence has drained and the same journal operation commits.
    pub(in crate::adaptive_resources::runtime) async fn prepare_copy_close(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        output_member: Uuid,
        abort: bool,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let never_root = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            role.copy_vod.as_ref().is_some_and(|copy| !copy.entered) && role.roots.is_empty()
        };
        if never_root {
            return self
                .prepare_copy_never_root_close(owner, index, deadline)
                .await;
        }
        {
            let mut bundle = owner.state.lock().await;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            if role.refunded
                || role.pending_finite_close.is_some()
                || role
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.effect == Effect::Settle)
            {
                return Ok(());
            }
            if !role.finite_app.as_ref().is_some_and(|state| state.seal()) {
                return Err(OperationError::State);
            }
        }
        self.join_copy_producer(owner, index, abort).await?;
        // Only an already-entered bootstrap arm can be reconciled here. A
        // never-started root/output has a different physical state and stays
        // charged until its explicit no-root close path exists.
        self.observe_finite_bootstrap_arm(owner, index, output_member, deadline)
            .await?;
        self.close_producer_finite_native(owner, index, output_member, deadline)
            .await?;
        let needs_seal = {
            let bundle = owner.state.lock().await;
            bundle.roles.get(index).ok_or(OperationError::State)?.phase != Phase::Sealed
        };
        if needs_seal {
            self.seal_role(owner, index, deadline).await?;
        }
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if role.refunded {
            return Ok(());
        }
        if role.phase != Phase::Sealed
            || role.transition.is_some()
            || role.graph.is_some()
            || role.bootstrap.len() != 1
            || !role.bootstrap.contains_key(&output_member)
            || role.sources.len() != 2
        {
            return Err(OperationError::State);
        }
        role.response_budget
            .as_ref()
            .ok_or(OperationError::State)?
            .begin_close_if_empty()?;
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        let closure = copy.process_closure.as_ref().ok_or(OperationError::State)?;
        let graph = role.sealed.as_ref().ok_or(OperationError::State)?;
        if !closure.matches(graph)
            || closure.generation() != handle.graph_id()
            || closure.root_member() != copy.root_member
            || closure.task() != copy.task.ok_or(OperationError::State)?
            || closure.acquisition_id() != copy.acquisition_id
            || closure.close_operation_id().is_nil()
        {
            return Err(OperationError::State);
        }
        let scratch_close = role
            .bootstrap
            .get(&output_member)
            .ok_or(OperationError::State)?
            .producer_close_fact(role.binding, output_member)?;
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if planned.len() != 5 {
            return Err(OperationError::State);
        }
        let close_id = v2::Id::new(closure.close_operation_id())?;
        let mut facts = Vec::with_capacity(planned.len());
        for plan in planned {
            let member_id = v2::Id::new(plan.member_id())?;
            let fact = match plan {
                v2::MemberPlan::SourceV1 { .. } => role
                    .sources
                    .get(&plan.member_id())
                    .ok_or(OperationError::State)?
                    .copy_closed_fact(closure)?,
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.member_id == copy.root_member
                        && planned.kind == MemberKind::Root
                        && planned.adapter == Adapter::ProcessRoot =>
                {
                    let Metric::Known(identity) = closure.root().process else {
                        return Err(OperationError::State);
                    };
                    v2::ClosedFact::LegacyV1 {
                        fact: crate::resource_custody::ClosedMemberFact {
                            member_id: planned.member_id,
                            adapter: planned.adapter.clone(),
                            adapter_version: planned.adapter_version,
                            domain: planned.domain.clone(),
                            object_identity: format!("{}:{}", identity.pid, identity.start_marker),
                            evidence: "original fixed rolling-copy root waited; parser, pipes, source parents and Child/ChildJob closed".into(),
                        },
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if task.actor == v2::VodActor::RollingCopyV1
                        && task.root_member_id.uuid() == copy.root_member
                        && task.graph_id.uuid() == closure.generation()
                        && task.member_id.uuid() == copy.task_member
                        && task.inventory.pending_io.is_empty()
                        && task.inventory.buffers.len() == 1
                        && task.inventory.readers.len() == 2
                        && task.inventory.buffers[0].handles == 0
                        && task
                            .inventory
                            .readers
                            .iter()
                            .all(|r| r.handles == 1 && r.memory_bytes == 0)
                        && task
                            .inventory
                            .readers
                            .iter()
                            .any(|r| r.owner_id == task.stdout_owner_id)
                        && task
                            .inventory
                            .readers
                            .iter()
                            .any(|r| r.owner_id == task.diagnostic_owner_id) =>
                {
                    let owners = task
                        .inventory
                        .buffers
                        .iter()
                        .chain(&task.inventory.readers)
                        .map(|owner| v2::OwnerClosed {
                            owner_id: owner.owner_id,
                            operation_id: close_id,
                        })
                        .collect();
                    v2::ClosedFact::VodOutputTaskV1 {
                        task: task.clone(),
                        join_operation_id: close_id,
                        owners,
                    }
                }
                v2::MemberPlan::ScratchAssetsV3 { plan, .. }
                    if plan == &scratch_close.0 && member_id.uuid() == output_member =>
                {
                    v2::ClosedFact::ScratchAssetsV3 {
                        plan: plan.clone(),
                        exact_head: scratch_close.1.clone(),
                        operation_id: scratch_close.1.operation_id,
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
        // No await, allocation or fallible data validation after these
        // one-shot takes. Unknown commit keeps this exact command in the book.
        let output = role
            .bootstrap
            .get_mut(&output_member)
            .expect("validated copy output")
            .take_producer_close(role.binding, output_member)
            .expect("stable copy output close under bundle lock");
        let process = role
            .copy_vod
            .as_mut()
            .expect("validated copy role")
            .process_closure
            .take()
            .expect("validated original copy closure");
        let receipt = PhysicalCloseReceipt::from_copy_cancel(CopyCancellationClosure {
            binding: role.binding,
            _process: CopyCancelProcess::Joined { _proof: process },
            _output: Some(output),
        });
        role.bootstrap.clear();
        role.sources.clear();
        role.response_budget = None;
        role.members.clear();
        role.typed_members = None;
        role.armed.clear();
        role.invoked.clear();
        role.roots.clear();
        role.sealed = None;
        role.copy_vod = None;
        role.pending_finite_close = Some(PendingFiniteClose {
            key,
            mutation: Mutation::SettleTyped(Box::new(PhysicalTypedSettlement {
                receipt,
                settlement,
            })),
            effect: Effect::Settle,
        });
        Ok(())
    }
}
