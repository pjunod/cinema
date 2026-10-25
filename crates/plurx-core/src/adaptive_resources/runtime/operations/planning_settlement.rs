//! Positive settlement of the planning role P (roles[0]) after its original
//! probe and engine tasks have joined. Thinker decision t_054582a2#497.
//!
//! Every closed fact is minted by a method on the consumed native closure,
//! from IDs bound when each owner was opened. Roots and task observations
//! are enriched durably first, the planning source is closed, and one typed
//! settlement covers all seven P members. An uncertain or failed native task
//! is held durably and never refunded. M (roles[1]) stays charged; what
//! survives P's refund is bounded by `planning_retained_structural_bytes`.
use super::planning_tasks::{PlanningNativeState, PlanningObservation, PlanningTasks};
use super::*;
use crate::adaptive_resources::physical_graph::{
    fixed_engine::FixedEngineNativeClosure, fixed_probe::FixedProbeNativeClosure,
};
use crate::resource_custody::{ClosedMemberFact, MemberKind, PhysicalTypedSettlement};
use bootstrap::btree_requested_upper_bound;
use std::collections::BTreeSet;

const UNCERTAIN_HOLD: &str = "planning native task uncertain";
const ROOT_EVIDENCE: &str = "original fixed planning root waited; session absent, \
    bounded pipes and pidfd closed under the joined P task";

/// Consumes both original native closures. Only `finish_planning_native`
/// constructs it, after the journal facts were derived from these closures.
pub(in crate::adaptive_resources) struct PlanningNativeClosure {
    binding: ReservationBinding,
    _probe: FixedProbeNativeClosure,
    _engine: FixedEngineNativeClosure,
}
impl PlanningNativeClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

/// Deterministic requested-capacity bound for every allocation that can
/// survive P's refund while M is live (decision 2c). It is charged in M's
/// admission and checked against actual capacities before P settles.
pub(in crate::adaptive_resources::runtime) fn planning_retained_structural_bytes() -> Option<u64> {
    use std::mem::size_of;
    let arc = 2 * size_of::<usize>();
    let bytes = size_of::<AsyncMutex<Bundle>>()
        .checked_add(arc)?
        .checked_add(MAX_PLANNING_BUNDLE_SLOTS.checked_mul(size_of::<OwnedRole>())?)?
        .checked_add(MAX_PLANNING_BUNDLE_SLOTS.checked_mul(size_of::<Uuid>())?)?
        .checked_add(MAX_PLANNING_BUNDLE_SLOTS.checked_mul(size_of::<FamilyIdentity>())?)?
        .checked_add(btree_requested_upper_bound::<
            ReservationBinding,
            ObligationHandle,
        >(2)?)?
        .checked_add(btree_requested_upper_bound::<ReservationBinding, ()>(2)?)?
        .checked_add(btree_requested_upper_bound::<Uuid, Arc<AsyncMutex<Bundle>>>(1)?)?
        .checked_add(btree_requested_upper_bound::<FamilyIdentity, FamilyHandle>(
            1,
        )?)?
        .checked_add(btree_requested_upper_bound::<FamilyIdentity, BTreeSet<Uuid>>(1)?)?
        .checked_add(btree_requested_upper_bound::<Uuid, ()>(1)?)?
        .checked_add(size_of::<v2::MemberPlan>())?
        .checked_add(physical_graph::empty_graph_structural_bytes()?)?
        .checked_add(size_of::<super::super::planning_probe::MetadataReleaseState>())?
        .checked_add(arc)?
        .checked_add(2usize.checked_mul(size_of::<std::sync::atomic::AtomicBool>() + arc)?)?
        .checked_add(super::super::finite_lifetime::requested_heap_bytes()?)?
        .checked_add(size_of::<PhysicalTypedSettlement>())?
        .checked_add(size_of::<v2::MemberDisposition>())?
        .checked_add(crate::resource_custody::owned_operation_requested_bytes()?)?;
    u64::try_from(bytes).ok()
}
/// Roles, typed-obligation and family vectors in the planning bundle hold at
/// most this many entries of capacity; asserted before P settles.
const MAX_PLANNING_BUNDLE_SLOTS: usize = 4;

fn lock_error(_: tokio::time::error::Elapsed) -> OperationError {
    OperationError::Deadline
}

fn mark_committed(tasks: &mut PlanningTasks, operation: Uuid) {
    let PlanningTasks {
        root_observations,
        probe_task_observation,
        engine_task_observation,
        ..
    } = tasks;
    for observation in root_observations
        .iter_mut()
        .chain([probe_task_observation, engine_task_observation])
    {
        if observation.operation_id == operation {
            observation.committed = true;
        }
    }
}

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) async fn finish_planning_native(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        {
            let mut bundle = tokio::time::timeout_at(deadline.into(), owner.state.lock())
                .await
                .map_err(lock_error)?;
            if !bundle.installed || bundle.roles.len() != 2 {
                return Err(OperationError::State);
            }
            let bundle_id = bundle.id;
            let role = &mut bundle.roles[0];
            if role.refunded {
                return Ok(());
            }
            // Resume the exact retained settlement command; never re-mint it.
            if role
                .transition
                .as_ref()
                .is_some_and(|transition| transition.effect == Effect::Settle)
            {
                return self
                    .resume_planning_settlement(bundle_id, role, deadline)
                    .await;
            }
            let state = role
                .planning_tasks
                .as_ref()
                .ok_or(OperationError::State)?
                .native_state();
            match state {
                PlanningNativeState::Joined => {}
                PlanningNativeState::Uncertain => {
                    self.hold_uncertain_planning(bundle_id, role, deadline)
                        .await?;
                    return Err(OperationError::State);
                }
                // Pending joins belong to collect_planning_tasks; a
                // never-entered P has no positive closure yet (Phase B).
                PlanningNativeState::NotEntered
                | PlanningNativeState::Pending
                | PlanningNativeState::Incomplete => return Err(OperationError::State),
            }
            self.observe_planning_members(bundle_id, role, deadline)
                .await?;
        }
        // seal_role takes the bundle lock itself.
        let needs_seal = {
            let mut bundle = tokio::time::timeout_at(deadline.into(), owner.state.lock())
                .await
                .map_err(lock_error)?;
            let role = &mut bundle.roles[0];
            match role.phase {
                Phase::Sealed => false,
                Phase::UnknownHeld => {
                    // A durable Seal is refused after a hold; fence entry only.
                    self.close_retained_entry(role.binding)?;
                    if let Some(graph) = role.graph.take() {
                        role.sealed = Some(graph.seal());
                    }
                    false
                }
                _ => true,
            }
        };
        if needs_seal {
            self.seal_role(owner, 0, deadline).await?;
        }
        let mut bundle = tokio::time::timeout_at(deadline.into(), owner.state.lock())
            .await
            .map_err(lock_error)?;
        self.settle_planning_role(&mut bundle, deadline).await
    }

    async fn resume_planning_settlement(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let transition = role.transition.as_mut().ok_or(OperationError::State)?;
        if transition.operation.pending.is_none()
            && !matches!(transition.operation.result, Some(Ok(_)))
        {
            // A deterministic rejection re-rejects; P stays charged.
            transition.operation.retry(&self.journal, deadline)?;
        }
        self.finish_transition(bundle_id, role).await?;
        if !role.refunded {
            return Err(OperationError::State);
        }
        Ok(())
    }

    /// One durable Hold so the journal phase matches the ledger's
    /// UnknownHeld. The failure owners stay in the book; nothing refunds.
    async fn hold_uncertain_planning(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        if role.transition.is_some() {
            if role
                .transition
                .as_ref()
                .is_some_and(|t| t.effect == Effect::Settle)
            {
                return Err(OperationError::State);
            }
            self.finish_transition(bundle_id, role).await?;
        }
        if role.phase == Phase::UnknownHeld {
            return Ok(());
        }
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        start_transition(
            &self.journal,
            role,
            Mutation::Hold {
                reason: UNCERTAIN_HOLD.into(),
            },
            Effect::Hold,
            deadline,
        )?;
        self.finish_transition(bundle_id, role).await
    }

    /// Steps d-h: resume any enrichment in flight, then durably observe the
    /// source child copy, the four roots and both tasks, then close the
    /// planning source parent. Every operation ID was minted at registration.
    async fn observe_planning_members(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let (source_member, probe_plan, engine_plan) = {
            let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
            (
                tasks.source_member,
                tasks.probe_plan.clone(),
                tasks.engine_plan.clone(),
            )
        };
        if let Some(transition) = role.transition.as_mut() {
            let effect = transition.effect;
            let operation = transition.operation.id;
            if !matches!(effect, Effect::Enrich | Effect::Seal)
                && !matches!(effect, Effect::SourceRoot(member) if member == source_member)
            {
                return Err(OperationError::State);
            }
            if transition.operation.pending.is_none()
                && !matches!(transition.operation.result, Some(Ok(_)))
            {
                transition.operation.retry(&self.journal, deadline)?;
            }
            self.finish_transition(bundle_id, role).await?;
            mark_committed(
                role.planning_tasks.as_mut().ok_or(OperationError::State)?,
                operation,
            );
        }

        // e. Source child copy, observed from the joined document root. The
        // parent must still be open: the observation re-reads its File.
        let parent_closed = role
            .sources
            .get(&source_member)
            .ok_or(OperationError::State)?
            .parent_closed();
        if !parent_closed {
            let observation = {
                let closure = role
                    .planning_tasks
                    .as_ref()
                    .and_then(|tasks| tasks.probe_closed.as_ref())
                    .ok_or(OperationError::State)?;
                role.sources
                    .get_mut(&source_member)
                    .ok_or(OperationError::State)?
                    .fixed_probe_child_observation(closure)?
            };
            if !observation.acknowledged {
                start_named_transition(
                    &self.journal,
                    role,
                    observation.observation_id,
                    Mutation::RecordTypedAcquired {
                        envelope: observation.envelope,
                    },
                    Effect::SourceRoot(source_member),
                    deadline,
                )?;
                self.finish_transition(bundle_id, role).await?;
            }
        }

        // f. The four original roots, from the identities their closures
        // sampled before waiting.
        for index in 0..4 {
            let (observation, identity) = {
                let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
                let observation: PlanningObservation = tasks.root_observations[index];
                if observation.committed {
                    continue;
                }
                let probe = tasks.probe_closed.as_ref().ok_or(OperationError::State)?;
                let engine = tasks.engine_closed.as_ref().ok_or(OperationError::State)?;
                let (member, identity) = match index {
                    0 => (probe.version_member(), probe.version_identity()),
                    1 => (probe.document_member(), probe.document_identity()),
                    2 => (engine.version_member(), engine.version_identity()),
                    _ => (engine.dependency_member(), engine.dependency_identity()),
                };
                if member != observation.member || identity.pid == 0 || identity.start_marker == 0 {
                    return Err(OperationError::State);
                }
                (observation, identity)
            };
            let plan = role
                .members
                .iter()
                .find(|plan| {
                    plan.member_id == observation.member
                        && plan.kind == MemberKind::Root
                        && plan.adapter == Adapter::ProcessRoot
                })
                .cloned()
                .ok_or(OperationError::State)?;
            let observed = ObservedIdentity {
                version: 1,
                member_id: observation.member,
                adapter: Adapter::ProcessRoot,
                adapter_version: plan.adapter_version,
                domain: plan.domain.clone(),
                object_identity: format!("{}:{}", identity.pid, identity.start_marker),
                process: Some(crate::resource_custody::ProcessIdentity {
                    version: 1,
                    pid: identity.pid,
                    creation_marker: Some(identity.start_marker),
                    domain: plan.domain,
                }),
            };
            let mutation = acquired_mutation(role, observed)?;
            start_named_transition(
                &self.journal,
                role,
                observation.operation_id,
                mutation,
                Effect::Enrich,
                deadline,
            )?;
            self.finish_transition(bundle_id, role).await?;
            role.planning_tasks
                .as_mut()
                .ok_or(OperationError::State)?
                .root_observations[index]
                .committed = true;
        }

        // g. Both task observations, captured at join.
        for probe_task in [true, false] {
            let (observation, member, observed) = {
                let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
                if probe_task {
                    (
                        tasks.probe_task_observation,
                        probe_plan.member_id.uuid(),
                        v2::Observed::FixedProbeDiscoverTaskV1 {
                            task: tasks.probe_observed.clone().ok_or(OperationError::State)?,
                        },
                    )
                } else {
                    (
                        tasks.engine_task_observation,
                        engine_plan.member_id.uuid(),
                        v2::Observed::FixedPlanningEngineTaskV1 {
                            task: tasks.engine_observed.clone().ok_or(OperationError::State)?,
                        },
                    )
                }
            };
            if observation.committed {
                continue;
            }
            if observation.member != member {
                return Err(OperationError::State);
            }
            let planned = role
                .typed_members
                .as_ref()
                .and_then(|members| members.iter().find(|plan| plan.member_id() == member))
                .cloned()
                .ok_or(OperationError::State)?;
            start_named_transition(
                &self.journal,
                role,
                observation.operation_id,
                Mutation::RecordTypedAcquired {
                    envelope: v2::Envelope {
                        version: 2,
                        planned,
                        observed: Some(observed),
                    },
                },
                Effect::Enrich,
                deadline,
            )?;
            self.finish_transition(bundle_id, role).await?;
            mark_committed(
                role.planning_tasks.as_mut().ok_or(OperationError::State)?,
                observation.operation_id,
            );
        }

        // h. Close the SAME original source File. Failed/unknown is terminal
        // for this attempt; P stays charged.
        if !parent_closed {
            let closure = role
                .planning_tasks
                .as_ref()
                .and_then(|tasks| tasks.probe_closed.as_ref())
                .ok_or(OperationError::State)?;
            role.sources
                .get_mut(&source_member)
                .ok_or(OperationError::State)?
                .close_fixed_probe_parent(closure)
                .await?;
        }
        Ok(())
    }

    /// Steps j-n under one bundle lock: recheck, assert the structural
    /// survivor set, build all facts, then move both closures into the one
    /// typed settlement and refund P on its committed acknowledgement.
    async fn settle_planning_role(
        &self,
        bundle: &mut Bundle,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let bundle_id = bundle.id;
        if bundle.roles.len() != 2 {
            return Err(OperationError::State);
        }
        if bundle.roles[0].refunded {
            return Ok(());
        }
        if bundle.roles[0]
            .transition
            .as_ref()
            .is_some_and(|transition| transition.effect == Effect::Settle)
        {
            return self
                .resume_planning_settlement(bundle_id, &mut bundle.roles[0], deadline)
                .await;
        }
        // j. Every P member is durably observed; nothing native remains
        // outside the two retained closures.
        {
            let role = &bundle.roles[0];
            let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
            let source = role
                .sources
                .get(&tasks.source_member)
                .ok_or(OperationError::State)?;
            if role.transition.is_some()
                || role.pending_finite_close.is_some()
                || role.graph.is_some()
                || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
                || role.handle.is_none()
                || tasks.native_state() != PlanningNativeState::Joined
                || !tasks.root_observations.iter().all(|o| o.committed)
                || !tasks.probe_task_observation.committed
                || !tasks.engine_task_observation.committed
                || !role.roots.is_empty()
                || !role.drains.is_empty()
                || !role.scratch.is_empty()
                || !role.bootstrap.is_empty()
                || role.finite_vod.is_some()
                || role.copy_vod.is_some()
                || role.retirement.is_some()
                || role.response_budget.is_some()
                || role.finite_app.is_some()
                || role.planning_metadata.is_some()
                || role.sources.len() != 1
                || !source.parent_closed()
            {
                return Err(OperationError::State);
            }
        }
        let handle = bundle.roles[0]
            .handle
            .clone()
            .ok_or(OperationError::State)?;
        let graph = bundle.roles[0]
            .sealed
            .as_ref()
            .ok_or(OperationError::State)?
            .snapshot()
            .await;
        if graph.identity.generation != handle.graph_id()
            || !graph.sealed
            || graph.acquired
            || graph.roots != 0
            || graph.writers != 0
            || graph.source_files != 0
        {
            return Err(OperationError::State);
        }
        // k. Actual capacities never exceed what M's admission assumed.
        check_planning_survivors(bundle)?;

        // l. All facts, in manifest order, before anything is consumed.
        let settlement = {
            let role = &bundle.roles[0];
            let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
            let probe = tasks.probe_closed.as_ref().ok_or(OperationError::State)?;
            let engine = tasks.engine_closed.as_ref().ok_or(OperationError::State)?;
            let operation_id = v2::Id::new(tasks.settlement_operation_id)?;
            let source_fact = role
                .sources
                .get(&tasks.source_member)
                .ok_or(OperationError::State)?
                .fixed_probe_closed_fact(probe)?;
            let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
            if planned.len() != 7 {
                return Err(OperationError::State);
            }
            let mut source_fact = Some(source_fact);
            let mut members = Vec::with_capacity(planned.len());
            for plan in planned {
                let fact = match plan {
                    v2::MemberPlan::LegacyV1 { planned }
                        if planned.kind == MemberKind::Root
                            && planned.adapter == Adapter::ProcessRoot =>
                    {
                        let identity = if planned.member_id == probe.version_member() {
                            probe.version_identity()
                        } else if planned.member_id == probe.document_member() {
                            probe.document_identity()
                        } else if planned.member_id == engine.version_member() {
                            engine.version_identity()
                        } else if planned.member_id == engine.dependency_member() {
                            engine.dependency_identity()
                        } else {
                            return Err(OperationError::State);
                        };
                        v2::ClosedFact::LegacyV1 {
                            fact: ClosedMemberFact {
                                member_id: planned.member_id,
                                adapter: planned.adapter.clone(),
                                adapter_version: planned.adapter_version,
                                domain: planned.domain.clone(),
                                object_identity: format!(
                                    "{}:{}",
                                    identity.pid, identity.start_marker
                                ),
                                evidence: ROOT_EVIDENCE.into(),
                            },
                        }
                    }
                    v2::MemberPlan::FixedProbeDiscoverSourceV1 { slot, .. }
                        if slot.member_id.uuid() == tasks.source_member =>
                    {
                        source_fact.take().ok_or(OperationError::State)?
                    }
                    v2::MemberPlan::FixedProbeDiscoverTaskV1 { task, .. }
                        if *task == tasks.probe_plan =>
                    {
                        probe.closed_fact(task, operation_id)?
                    }
                    v2::MemberPlan::FixedPlanningEngineTaskV1 { task, .. }
                        if task == engine.task() && *task == tasks.engine_plan =>
                    {
                        engine.closed_fact(operation_id)?
                    }
                    _ => return Err(OperationError::State),
                };
                members.push(v2::MemberDisposition::Closed {
                    member_id: v2::Id::new(plan.member_id())?,
                    fact,
                });
            }
            v2::Settlement {
                version: 2,
                owner_id: v2::Id::new(self.owner.owner_id)?,
                graph_id: v2::Id::new(handle.graph_id())?,
                operation_id,
                members,
            }
        };

        // m. No await or fallible work between consuming the closures and
        // retaining the exact command in this role's transition.
        bundle.prepare.result = None;
        let role = &mut bundle.roles[0];
        let operation_id = settlement.operation_id.uuid();
        let tasks = role.planning_tasks.as_mut().expect("validated P tasks");
        let probe = tasks.probe_closed.take().expect("validated probe closure");
        let engine = tasks
            .engine_closed
            .take()
            .expect("validated engine closure");
        let receipt = PhysicalCloseReceipt::from_planning_native(PlanningNativeClosure {
            binding: role.binding,
            _probe: probe,
            _engine: engine,
        });
        role.planning_tasks = None;
        role.sources.clear();
        role.members.clear();
        role.typed_members = None;
        role.armed.clear();
        role.invoked.clear();
        role.roots.clear();
        role.drains.clear();
        role.closed_drains.clear();
        role.sealed = None;
        start_named_transition(
            &self.journal,
            role,
            operation_id,
            Mutation::SettleTyped(Box::new(PhysicalTypedSettlement {
                receipt,
                settlement,
            })),
            Effect::Settle,
            deadline,
        )?;
        // n. The committed acknowledgement refunds P; M is untouched.
        self.finish_transition(bundle_id, role).await?;
        if !role.refunded {
            return Err(OperationError::State);
        }
        Ok(())
    }
}

/// Decision 2d: compare actual retained capacities with the structural bound
/// M's admission charged. Any excess keeps P charged.
fn check_planning_survivors(bundle: &Bundle) -> Result<(), OperationError> {
    let empty_graph =
        physical_graph::empty_graph_structural_bytes().ok_or(OperationError::State)?;
    let metadata = &bundle.roles[1];
    let typed_ok = match metadata.typed_members.as_deref() {
        Some([v2::MemberPlan::FixedProbeMetadataV1 { slot, .. }]) => {
            metadata
                .typed_members
                .as_ref()
                .is_some_and(|v| v.capacity() <= 1)
                && slot.depends_on.capacity() == 0
                && slot.protected_paths.capacity() == 0
        }
        _ => false,
    };
    let graph_ok = match (metadata.graph.as_ref(), metadata.sealed.as_ref()) {
        (Some(graph), None) => graph
            .retained_empty_heap_bytes()
            .is_some_and(|bytes| bytes <= empty_graph),
        (None, Some(graph)) => graph
            .retained_empty_heap_bytes()
            .is_some_and(|bytes| bytes <= empty_graph),
        _ => false,
    };
    if bundle.roles.capacity() > MAX_PLANNING_BUNDLE_SLOTS
        || bundle.typed_obligation_ids.capacity() > MAX_PLANNING_BUNDLE_SLOTS
        || bundle.family_ids.capacity() > MAX_PLANNING_BUNDLE_SLOTS
        || !bundle.retained.is_empty()
        || !bundle.prepare.acknowledged
        || !typed_ok
        || !graph_ok
        || metadata.members.capacity() != 0
        || !metadata.sources.is_empty()
        || !metadata.bootstrap.is_empty()
        || !metadata.scratch.is_empty()
        || !metadata.roots.is_empty()
        || !metadata.drains.is_empty()
        || !metadata.closed_drains.is_empty()
        || !metadata.armed.is_empty()
        || !metadata.invoked.is_empty()
        || metadata.finite_vod.is_some()
        || metadata.copy_vod.is_some()
        || metadata.retirement.is_some()
        || metadata.response_budget.is_some()
        || metadata.planning_tasks.is_some()
        || metadata.planning_metadata.is_none()
    {
        return Err(OperationError::State);
    }
    Ok(())
}
