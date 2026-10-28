//! Positive typed cancellation of a launched fixed finite producer. The
//! original runtime retains every native owner and journal operation across
//! cancellation; a root-only fact is never a joined writer.
use super::*;
use crate::adaptive_resources::physical_graph::finite_vod::{
    FiniteVodNeverRootClosure, FiniteVodProcessClosure, FiniteVodRootOnlyClosure,
};
use crate::adaptive_resources::runtime::operations::bootstrap::retirement::ProducerFiniteClosure;
use crate::resource_custody::{MemberKind, PhysicalTypedSettlement};

/// Consumed only by the core receipt constructor after every original source
/// and scratch owner has positively closed. No serialized fact can mint this.
enum FiniteCancelProcess {
    NeverRoot { _proof: FiniteVodNeverRootClosure },
    RootOnly { _proof: FiniteVodRootOnlyClosure },
    Joined { _proof: FiniteVodProcessClosure },
}
enum FiniteCancelOutput {
    NeverAcquired,
    Closed { _proof: ProducerFiniteClosure },
}

pub(in crate::adaptive_resources) struct FiniteCancellationClosure {
    binding: ReservationBinding,
    _process: FiniteCancelProcess,
    _output: FiniteCancelOutput,
}
impl FiniteCancellationClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

impl NodeResourceRuntime {
    /// A definitely unaccepted original prepare has no journal obligation to
    /// settle. Preserve its actual no-root receipt until rich app owners drop;
    /// the known prepare result alone never authorizes early ledger release.
    pub(in crate::adaptive_resources::runtime) async fn prepare_unaccepted_finite_close(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        {
            let mut bundle = owner.state.lock().await;
            if !finite_prepare_definitely_unaccepted(&bundle)
                || bundle.roles.len() != 1
                || index != 0
            {
                return Err(OperationError::State);
            }
            let role = &mut bundle.roles[0];
            if role.refunded
                || role.handle.is_some()
                || role.transition.is_some()
                || role.pending_finite_close.is_some()
                || !role.armed.is_empty()
                || !role.invoked.is_empty()
                || !role.sources.is_empty()
                || !role.bootstrap.is_empty()
                || !role.scratch.is_empty()
                || !role.roots.is_empty()
                || !role.drains.is_empty()
                || !role.closed_drains.is_empty()
                || role.retirement.is_some()
                || !role.finite_app.as_ref().is_some_and(|state| state.seal())
            {
                return Err(OperationError::State);
            }
            if role.receipt.is_some() && role.finite_vod.is_none() {
                return Ok(());
            }
            let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
            if !finite.prepared_payload_disposable() || !finite.registered_source_members.is_empty()
            {
                return Err(OperationError::State);
            }
        }
        if !self.close_finite_never_root(owner, index).await? {
            return Err(OperationError::State);
        }
        let mut bundle = owner.state.lock().await;
        if !finite_prepare_definitely_unaccepted(&bundle) {
            return Err(OperationError::State);
        }
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
        let absent = finite
            .never_root_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        if role.receipt.is_some()
            || !absent.matches(role.sealed.as_ref().ok_or(OperationError::State)?)
            || absent.root_member() != finite.root_member
            || absent.task_member() != finite.task_member
            || absent.acquisition_id() != finite.acquisition_id
            || !finite.prepared_payload_disposable()
        {
            return Err(OperationError::State);
        }
        role.response_budget
            .as_ref()
            .ok_or(OperationError::State)?
            .begin_close_if_empty()?;
        let closure = FiniteCancellationClosure {
            binding: role.binding,
            _process: FiniteCancelProcess::NeverRoot {
                _proof: finite
                    .never_root_closure
                    .take()
                    .expect("validated original no-root closure"),
            },
            _output: FiniteCancelOutput::NeverAcquired,
        };
        role.receipt = Some(PhysicalCloseReceipt::from_finite_cancel(closure));
        // Drop the never-started pump's output/preparation aliases while its
        // full R charge remains held. External app selectors still gate final
        // destruction; no fictitious settled phase or journal command exists.
        role.finite_vod = None;
        role.response_budget = None;
        role.members.clear();
        role.typed_members = None;
        role.sealed = None;
        Ok(())
    }

    /// A pre-root path retains the original source cells and at most one
    /// original bootstrap cell. The graph proves absence of root/task entry;
    /// the bootstrap arm separately proves whether its native open entered.
    async fn close_finite_never_root(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<bool, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let bundle_id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        if finite.never_root_closure.is_none() && (finite.entered || !role.roots.is_empty()) {
            return Ok(false);
        }
        if role.refunded
            || role.retirement.is_some()
            || role.bootstrap.len() > 1
            || role.sources.len() != finite.registered_source_members.len()
            || role
                .sources
                .keys()
                .any(|member| !finite.registered_source_members.contains(member))
            || finite
                .registered_source_members
                .iter()
                .any(|member| !finite.source_members.contains(member))
        {
            return Err(OperationError::State);
        }
        if let Some(transition) = &role.transition {
            if !matches!(transition.effect, Effect::Arm(_) | Effect::Source(_)) {
                return Err(OperationError::State);
            }
            // Resume only the original captured operation. Unknown remains
            // held, even when the source open physically returned no File.
            self.finish_transition(bundle_id, role).await?;
        }
        if let Some(member) = role.bootstrap.keys().next().copied() {
            let disposition = role
                .bootstrap
                .get(&member)
                .ok_or(OperationError::State)?
                .never_root_disposition(role.armed.contains(&member))?;
            if matches!(
                disposition,
                bootstrap::NeverRootBootstrap::UnarmedPlaceholder
            ) {
                // This exact empty in-memory placeholder has no durable arm
                // and no native owner; the planned slot remains unacquired.
                role.bootstrap.remove(&member);
            }
            // Advanced scratch state stays owned for actual native retirement;
            // producer-process non-entry never turns it into a placeholder.
        }
        self.close_retained_entry(role.binding)?;
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        if role
            .finite_vod
            .as_ref()
            .ok_or(OperationError::State)?
            .never_root_closure
            .is_none()
        {
            let graph = role
                .sealed
                .as_ref()
                .ok_or(OperationError::State)?
                .select_same();
            let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
            let proof = graph
                .try_finite_vod_never_root_closure(
                    finite.root_member,
                    finite.task_member,
                    finite.acquisition_id,
                )
                .await?;
            finite.never_root_closure = Some(proof);
        }
        let proof = role
            .finite_vod
            .as_ref()
            .and_then(|finite| finite.never_root_closure.as_ref())
            .ok_or(OperationError::State)?;
        for source in role.sources.values_mut() {
            source.close_parent_before_root(proof).await?;
        }
        for source in role.sources.values() {
            let _ = source.finite_never_root_closed_fact(proof)?;
        }
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        for member in &finite.source_members {
            if !finite.registered_source_members.contains(member) {
                if role.armed.contains(member)
                    || role.sources.contains_key(member)
                    || planned
                        .iter()
                        .filter(|plan| plan.member_id() == *member)
                        .count()
                        != 1
                {
                    return Err(OperationError::State);
                }
                let plan = planned
                    .iter()
                    .find(|plan| plan.member_id() == *member)
                    .ok_or(OperationError::State)?;
                let _ = source_files::finite_unregistered_closed_fact(plan, proof)?;
            }
        }
        role.finite_vod
            .as_mut()
            .ok_or(OperationError::State)?
            .retiring = true;
        Ok(true)
    }

    /// Resolve an interrupted fixed producer, close its original output, and
    /// settle the same typed R after one known journal commit. Earlier source
    /// or bootstrap failures without this exact native proof stay fully held.
    pub(in crate::adaptive_resources::runtime) async fn prepare_failed_finite_vod_close(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        {
            let mut bundle = owner.state.lock().await;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            if role.refunded {
                return Ok(());
            }
            if role.pending_finite_close.is_some() {
                return Ok(());
            }
            if role
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
        // A cancelled launch can leave its exact Arm/Observe result pending.
        // SourceRoot acknowledgment needs the same retained parent File, so
        // reconcile before process join and source close can consume it.
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.reconcile_finite_acquisition_for_cleanup(owner, index, deadline),
        )
        .await
        .map_err(|_| OperationError::Deadline)??;
        // Join a check abandoned by a cancelled prelaunch or publication
        // waiter before deciding that no native work remains. Unknown close
        // or panic retains the original full R and refuses typed settlement.
        self.reconcile_finite_engine_check(owner, index).await?;
        let initial_member = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
            if !finite.entered
                && role.roots.is_empty()
                && role.bootstrap.len() == 1
                && role
                    .bootstrap
                    .values()
                    .next()
                    .is_some_and(|bootstrap| bootstrap.has_root_arm())
            {
                Some(*role.bootstrap.keys().next().ok_or(OperationError::State)?)
            } else {
                None
            }
        };
        if let Some(initial_member) = initial_member {
            self.observe_finite_bootstrap_root(owner, index, initial_member, deadline)
                .await?;
        }
        let never_root = self.close_finite_never_root(owner, index).await?;
        let member = if never_root {
            let member = {
                let bundle = owner.state.lock().await;
                let role = bundle.roles.get(index).ok_or(OperationError::State)?;
                role.bootstrap.keys().next().copied()
            };
            if let Some(member) = member {
                self.close_producer_finite_native(owner, index, member, deadline)
                    .await?;
            }
            member
        } else {
            // A never-registered task retains a root-only proof; a registered
            // writer retains its distinct joined proof. Neither can be
            // invented from the other when this wait is interrupted.
            let joined = self.join_finite_producer(owner, index).await;
            let member = {
                let bundle = owner.state.lock().await;
                let role = bundle.roles.get(index).ok_or(OperationError::State)?;
                let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
                if finite.root_only_closure.is_some() == finite.process_closure.is_some() {
                    return Err(joined.err().unwrap_or(OperationError::State));
                }
                if role.bootstrap.len() != 1 || role.retirement.is_some() {
                    return Err(OperationError::State);
                }
                *role.bootstrap.keys().next().ok_or(OperationError::State)?
            };
            // Reconcile the original armed traversal/create result before
            // choosing a cleanup action. This never advances to a new child.
            self.observe_finite_bootstrap_arm(owner, index, member, deadline)
                .await?;
            self.close_producer_finite_native(owner, index, member, deadline)
                .await?;
            Some(member)
        };
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
            || role.retirement.is_some()
            || role.bootstrap.len() != usize::from(member.is_some())
        {
            return Err(OperationError::State);
        }
        // Even a never-acquired output can have a constructed daemon
        // rendition. Native non-entry is not closure of that heap owner.
        role.response_budget
            .as_ref()
            .ok_or(OperationError::State)?
            .begin_close_if_empty()?;
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let finite = role.finite_vod.as_ref().ok_or(OperationError::State)?;
        let never_root = finite.never_root_closure.as_ref();
        let root_only = finite.root_only_closure.as_ref();
        let process = finite.process_closure.as_ref();
        let sealed = role.sealed.as_ref().ok_or(OperationError::State)?;
        let (root_member, generation, root_selection, close_operation_id, native_matches) =
            match (never_root, root_only, process) {
                (Some(absent), None, None) if !finite.producer_joined => (
                    absent.root_member(),
                    absent.generation(),
                    None,
                    absent.close_operation_id(),
                    absent.matches(sealed),
                ),
                (None, Some(root), None) if member.is_some() && !finite.producer_joined => (
                    root.root_member(),
                    root.generation(),
                    Some(root.root()),
                    root.close_operation_id(),
                    root.matches(sealed),
                ),
                (None, None, Some(process)) if member.is_some() && finite.producer_joined => (
                    process.root_member(),
                    process.generation(),
                    Some(process.root()),
                    process.close_operation_id(),
                    process.matches(sealed),
                ),
                _ => return Err(OperationError::State),
            };
        if !native_matches || generation != handle.graph_id() {
            return Err(OperationError::State);
        }
        let scratch_close = member
            .map(|member| {
                role.bootstrap
                    .get(&member)
                    .ok_or(OperationError::State)?
                    .producer_close_fact(role.binding, member)
            })
            .transpose()?;
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if planned.len() != finite.source_members.len() + 3 {
            return Err(OperationError::State);
        }
        let close_id = v2::Id::new(close_operation_id)?;
        let mut facts = Vec::with_capacity(planned.len());
        for plan in planned {
            let member_id = v2::Id::new(plan.member_id())?;
            let fact = match plan {
                v2::MemberPlan::SourceV1 { .. } => match role.sources.get(&plan.member_id()) {
                    Some(source) => match (never_root, root_only, process) {
                        (Some(absent), None, None) => {
                            source.finite_never_root_closed_fact(absent)?
                        }
                        (None, Some(root), None) => source.finite_root_only_closed_fact(root)?,
                        (None, None, Some(process)) => source.finite_closed_fact(process)?,
                        _ => return Err(OperationError::State),
                    },
                    None if never_root.is_some()
                        && !finite.registered_source_members.contains(&plan.member_id())
                        && !role.armed.contains(&plan.member_id()) =>
                    {
                        source_files::finite_unregistered_closed_fact(
                            plan,
                            never_root.ok_or(OperationError::State)?,
                        )?
                    }
                    None => return Err(OperationError::State),
                },
                v2::MemberPlan::LegacyV1 { planned }
                    if never_root.is_some()
                        && planned.member_id == root_member
                        && planned.kind == MemberKind::Root
                        && planned.adapter == Adapter::ProcessRoot =>
                {
                    v2::ClosedFact::LegacyNeverAcquiredV1 {
                        planned: planned.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.member_id == root_member
                        && planned.kind == MemberKind::Root
                        && planned.adapter == Adapter::ProcessRoot =>
                {
                    let Metric::Known(identity) =
                        root_selection.ok_or(OperationError::State)?.process
                    else {
                        return Err(OperationError::State);
                    };
                    v2::ClosedFact::LegacyV1 {
                        fact: crate::resource_custody::ClosedMemberFact {
                            member_id: planned.member_id,
                            adapter: planned.adapter.clone(),
                            adapter_version: planned.adapter_version,
                            domain: planned.domain.clone(),
                            object_identity: format!("{}:{}", identity.pid, identity.start_marker),
                            evidence: if root_only.is_some() {
                                "original fixed software AVC root waited; no writer registered; both original pipes and Child/ChildJob closed"
                            } else {
                                "original fixed software AVC root waited; writer and pipes joined; original Child/ChildJob disposed"
                            }.into(),
                        },
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if never_root.is_some()
                        && task.root_member_id.uuid() == root_member
                        && task.graph_id.uuid() == generation
                        && never_root.is_some_and(|absent| {
                            task.member_id.uuid() == absent.task_member()
                        }) =>
                {
                    v2::ClosedFact::VodOutputTaskNeverAcquiredV1 {
                        task: task.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if root_only.is_some()
                        && task.root_member_id.uuid() == root_member
                        && task.graph_id.uuid() == generation
                        && root_only
                            .is_some_and(|root| task.member_id.uuid() == root.task_member()) =>
                {
                    v2::ClosedFact::VodOutputTaskNeverAcquiredV1 {
                        task: task.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if process.is_some()
                        && task.root_member_id.uuid() == root_member
                        && task.graph_id.uuid() == generation
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
                    if never_root.is_some() && member.is_none() =>
                {
                    v2::ClosedFact::ScratchAssetsNeverAcquiredV3 {
                        plan: plan.clone(),
                        operation_id: close_id,
                    }
                }
                v2::MemberPlan::ScratchAssetsV3 { plan, .. }
                    if scratch_close
                        .as_ref()
                        .is_some_and(|(closed_plan, _)| plan == closed_plan)
                        && member == Some(member_id.uuid()) =>
                {
                    let (_, scratch_head) = scratch_close.as_ref().ok_or(OperationError::State)?;
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
        let never_root_selected = never_root.is_some();
        let root_only_selected = root_only.is_some();
        let output = if let Some(member) = member {
            FiniteCancelOutput::Closed {
                _proof: role
                    .bootstrap
                    .get_mut(&member)
                    .expect("validated producer output")
                    .take_producer_close(role.binding, member)
                    .expect("validated producer close is stable under bundle lock"),
            }
        } else {
            FiniteCancelOutput::NeverAcquired
        };
        let finite = role.finite_vod.as_mut().expect("validated finite role");
        let process = if never_root_selected {
            FiniteCancelProcess::NeverRoot {
                _proof: finite
                    .never_root_closure
                    .take()
                    .expect("validated original no-root closure"),
            }
        } else if root_only_selected {
            FiniteCancelProcess::RootOnly {
                _proof: finite
                    .root_only_closure
                    .take()
                    .expect("validated original root-only closure"),
            }
        } else {
            FiniteCancelProcess::Joined {
                _proof: finite
                    .process_closure
                    .take()
                    .expect("validated original joined closure"),
            }
        };
        let closure = FiniteCancellationClosure {
            binding: role.binding,
            _process: process,
            _output: output,
        };
        let receipt = PhysicalCloseReceipt::from_finite_cancel(closure);
        // Original native closure is now in the one-shot receipt, and the
        // metadata/body lease fence above is closed. Dispose producer planning
        // payloads (including the never-started pump's back-reference) while
        // the complete R reservation is still charged.
        role.bootstrap.clear();
        role.sources.clear();
        role.response_budget = None;
        role.members.clear();
        role.typed_members = None;
        role.armed.clear();
        role.invoked.clear();
        role.roots.clear();
        role.sealed = None;
        role.finite_vod = None;
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
