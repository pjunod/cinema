//! Positive settlement of the planning role P (roles[0]) after its original
//! probe and engine tasks have joined, or their original inputs prove neither
//! task entered and the original source parent is closed/never acquired.
//! Thinker decision t_054582a2#497.
//!
//! Every closed fact is minted by a method on the consumed native closure,
//! from IDs bound when each owner was opened. Roots and task observations
//! are enriched durably first, the planning source is closed, and one typed
//! settlement covers all seven P members. An uncertain or failed native task
//! is held durably and never refunded. M (roles[1]) stays charged; what
//! survives P's refund is bounded by `planning_retained_structural_bytes`.
use super::planning_tasks::{
    JoinedPlanningTaskNeverEntered, PlanningNativeState, PlanningObservation, PlanningTasks,
};
use super::*;
use crate::adaptive_resources::physical_graph::{
    fixed_engine::FixedEngineNativeClosure,
    fixed_probe::{FixedPlanningNoTaskGraph, FixedProbeNativeClosure},
};
use crate::resource_custody::{ClosedMemberFact, MemberKind, PhysicalTypedSettlement};
use bootstrap::btree_requested_upper_bound;
use std::collections::BTreeSet;

const UNCERTAIN_HOLD: &str = "planning native task uncertain";
const ROOT_EVIDENCE: &str = "original fixed planning root waited; session absent, \
    bounded pipes and pidfd closed under the joined P task";

/// Consumes the joined native closures or the original no-task graph witness.
/// Only `finish_planning_native` constructs it after matching source closure
/// and seven typed facts have been checked under the original bundle lock.
pub(in crate::adaptive_resources) struct PlanningNativeClosure {
    binding: ReservationBinding,
    _evidence: PlanningNativeEvidence,
}
// These accounted original payloads are consumed immediately by the receipt
// constructor; keep them inline instead of adding an uncharged Box allocation.
#[allow(clippy::large_enum_variant)]
enum PlanningNativeEvidence {
    Joined {
        _probe: FixedProbeNativeClosure,
        _engine: FixedEngineNativeClosure,
    },
    NeverEntered {
        _graph: FixedPlanningNoTaskGraph,
    },
    RegisteredNeverEntered {
        _graph: FixedPlanningNoTaskGraph,
        _probe: JoinedPlanningTaskNeverEntered,
        _engine: JoinedPlanningTaskNeverEntered,
    },
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
    // Arc's two counters impose word alignment even on a one-byte flag;
    // include trailing layout padding in the actual requested allocation.
    let flag_arc = std::alloc::Layout::new::<[usize; 2]>()
        .extend(std::alloc::Layout::new::<std::sync::atomic::AtomicBool>())
        .ok()?
        .0
        .pad_to_align()
        .size();
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
        .checked_add(2usize.checked_mul(flag_arc)?)?
        .checked_add(super::super::finite_lifetime::requested_heap_bytes()?)?
        .checked_add(size_of::<PhysicalTypedSettlement>())?
        .checked_add(size_of::<v2::MemberDisposition>())?
        .checked_add(crate::resource_custody::owned_operation_requested_bytes()?)?;
    u64::try_from(bytes).ok()
}
/// Requested allocation/frame bound for the borrowed seven-member preflight.
/// Plans, source strings and copy vectors are lent from their original owners;
/// only the existing validators' seen/copy sets and compact hash are allocated.
pub(in crate::adaptive_resources::runtime) fn registered_planning_preflight_bytes() -> Option<u64> {
    let bytes = std::mem::size_of::<[v2::EnvelopeRef<'static>; 7]>()
        // Scope retains its original iterator; lookup and probe source-child
        // validation may each retain one clone plus two current audit views.
        .checked_add(
            std::mem::size_of::<
                std::iter::Copied<std::slice::Iter<'static, v2::EnvelopeRef<'static>>>,
            >()
            .checked_mul(3)?,
        )?
        .checked_add(std::mem::size_of::<v2::EnvelopeRef<'static>>().checked_mul(2)?)?
        // The loan wrapper plus the local unobserved-envelope value.
        .checked_add(std::mem::size_of::<Option<v2::Observed>>().checked_mul(2)?)?
        .checked_add(std::mem::size_of::<&mut Option<v2::SourceObserved>>())? // restoration slot
        .checked_add(std::mem::size_of::<usize>())? // bounded writer counter
        .checked_add(btree_requested_upper_bound::<v2::Id, ()>(7)?)?
        .checked_add(btree_requested_upper_bound::<v2::Id, ()>(2)?.checked_mul(3)?)?
        .checked_add(64)? // engine_task_plan_sha256's exact hex allocation
        .checked_add(36)?; // Id serialization's one canonical UUID String
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
    /// Bound only this borrowed wait. JournalOperation::resolve awaits its
    /// PendingOperation receiver in place and clears it only after receipt;
    /// finish_transition has no further await after resolve. A timeout thus
    /// leaves the exact command, receiver, proof and original role retained,
    /// without interrupting synchronous acknowledgment/refund application.
    async fn finish_planning_transition(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        tokio::time::timeout_at(deadline.into(), async {
            #[cfg(test)]
            {
                let operation = role
                    .transition
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .operation
                    .id;
                super::planning_tasks::tests::stages::before_enrichment_ack(operation).await;
            }
            self.finish_transition(bundle_id, role).await
        })
        .await
        .map_err(lock_error)?
    }

    pub(in crate::adaptive_resources::runtime) async fn finish_planning_native(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let installed = tokio::time::timeout_at(deadline.into(), owner.state.lock())
            .await
            .map_err(lock_error)?
            .installed;
        if !installed {
            // A dropped owner may never have polled prepare_inputs. Resolve
            // the SAME accepted command; no fresh admission or source open.
            tokio::time::timeout_at(
                deadline.into(),
                self.acknowledge_planning_preparation_for_cleanup(owner),
            )
            .await
            .map_err(lock_error)??;
        }
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
                PlanningNativeState::RegisteredNeverEntered => {
                    self.close_registered_unentered_planning_members(bundle_id, role, deadline)
                        .await?;
                }
                PlanningNativeState::Uncertain => {
                    self.hold_uncertain_planning(bundle_id, role, deadline)
                        .await?;
                    return Err(OperationError::State);
                }
                PlanningNativeState::NotEntered => {
                    if let Err(error) = self
                        .close_never_entered_planning_members(bundle_id, role, deadline)
                        .await
                    {
                        if matches!(error, OperationError::Deadline) {
                            return Err(error);
                        }
                        self.hold_uncertain_planning(bundle_id, role, deadline)
                            .await?;
                        return Err(error);
                    }
                }
                PlanningNativeState::Pending | PlanningNativeState::Incomplete => {
                    return Err(OperationError::State)
                }
            }
            if state == PlanningNativeState::Joined {
                self.observe_planning_members(bundle_id, role, deadline)
                    .await?;
            }
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
            tokio::time::timeout_at(deadline.into(), self.seal_role(owner, 0, deadline))
                .await
                .map_err(lock_error)??;
        }
        let mut bundle = tokio::time::timeout_at(deadline.into(), owner.state.lock())
            .await
            .map_err(lock_error)?;
        self.settle_planning_role(&mut bundle, deadline).await
    }

    /// Phase B retains the original pure inputs until the original graph
    /// positively fences all task/root entry. Its SourceCell is separate from
    /// graph-native inventory: join/close only that already registered owner.
    async fn close_never_entered_planning_members(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
        let members = tasks.never_entered_members()?;
        let source_member = tasks.source_member;
        if !role.invoked.is_empty()
            || role.sources.len() > 1
            || role.sources.keys().any(|member| *member != source_member)
            || !role.roots.is_empty()
            || !role.drains.is_empty()
            || !role.closed_drains.is_empty()
            || !role.scratch.is_empty()
            || !role.bootstrap.is_empty()
            || role.finite_vod.is_some()
            || role.copy_vod.is_some()
            || role.retirement.is_some()
            || role.response_budget.is_some()
            || role.finite_app.is_some()
            || role.planning_metadata.is_some()
            || role.pending_finite_close.is_some()
            || role
                .armed
                .iter()
                .any(|member| *member != source_member && !members.contains(member))
            || (role.sources.contains_key(&source_member) == tasks.source_spec_retained())
            || (!role.sources.contains_key(&source_member) && role.armed.contains(&source_member))
        {
            return Err(OperationError::State);
        }
        if let Some(transition) = role.transition.as_mut() {
            if !matches!(transition.effect, Effect::Arm(member) if member == source_member || members.contains(&member))
                && !matches!(transition.effect, Effect::Source(member) if member == source_member)
                && transition.effect != Effect::Seal
            {
                return Err(OperationError::State);
            }
            if transition.operation.pending.is_none()
                && !matches!(transition.operation.result, Some(Ok(_)))
            {
                transition.operation.retry(&self.journal, deadline)?;
            }
            self.finish_planning_transition(bundle_id, role, deadline)
                .await?;
        }
        self.close_retained_entry(role.binding)?;
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
        if tasks.never_entered.is_none() {
            let source = role
                .typed_members
                .as_ref()
                .and_then(|plans| {
                    plans.iter().find_map(|plan| match plan {
                        v2::MemberPlan::FixedProbeDiscoverSourceV1 { slot, source }
                            if slot.member_id.uuid() == source_member =>
                        {
                            Some(source)
                        }
                        _ => None,
                    })
                })
                .ok_or(OperationError::State)?;
            let graph = role.sealed.as_ref().ok_or(OperationError::State)?;
            let proof = tokio::time::timeout_at(
                deadline.into(),
                graph.try_fixed_planning_no_task_graph(
                    members,
                    source,
                    tasks.settlement_operation_id,
                ),
            )
            .await
            .map_err(lock_error)??;
            if proof.source_member() != source_member
                || !proof.matches(
                    role.binding,
                    role.handle
                        .as_ref()
                        .ok_or(OperationError::State)?
                        .graph_id(),
                    members,
                )
            {
                return Err(OperationError::State);
            }
            let tasks = role.planning_tasks.as_mut().ok_or(OperationError::State)?;
            tasks.never_entered = Some(proof);
            tasks.consume_never_entered_inputs();
        }
        let proof = role
            .planning_tasks
            .as_ref()
            .and_then(|tasks| tasks.never_entered.as_ref())
            .ok_or(OperationError::State)?;
        if let Some(source) = role.sources.get_mut(&source_member) {
            tokio::time::timeout_at(
                deadline.into(),
                source.close_fixed_probe_parent_before_task(proof),
            )
            .await
            .map_err(lock_error)??;
            source.fixed_probe_no_task_closed_fact(proof)?;
        }
        Ok(())
    }

    async fn close_registered_unentered_planning_members(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
        let members = tasks.registered_unentered_members(role.binding)?;
        let source_member = tasks.source_member;
        if !role.invoked.is_empty()
            || role.sources.len() != 1
            || role.sources.keys().any(|member| *member != source_member)
            || !role.roots.is_empty()
            || !role.drains.is_empty()
            || !role.closed_drains.is_empty()
            || !role.scratch.is_empty()
            || !role.bootstrap.is_empty()
            || role.finite_vod.is_some()
            || role.copy_vod.is_some()
            || role.retirement.is_some()
            || role.response_budget.is_some()
            || role.finite_app.is_some()
            || role.planning_metadata.is_some()
            || role.pending_finite_close.is_some()
            || role
                .armed
                .iter()
                .any(|member| *member != source_member && !members.contains(member))
            || tasks.source_spec_retained()
            || (!role.sources.contains_key(&source_member) && role.armed.contains(&source_member))
        {
            return Err(OperationError::State);
        }
        if let Some(transition) = role.transition.as_mut() {
            if !matches!(transition.effect, Effect::Arm(member) if member == source_member || members.contains(&member))
                && !matches!(transition.effect, Effect::Source(member) if member == source_member)
                && transition.effect != Effect::Seal
            {
                return Err(OperationError::State);
            }
            if transition.operation.pending.is_none()
                && !matches!(transition.operation.result, Some(Ok(_)))
            {
                transition.operation.retry(&self.journal, deadline)?;
            }
            self.finish_planning_transition(bundle_id, role, deadline)
                .await?;
        }
        self.close_retained_entry(role.binding)?;
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
        if tasks.registered_unentered_graph.is_none() {
            let source = role
                .typed_members
                .as_ref()
                .and_then(|plans| {
                    plans.iter().find_map(|plan| match plan {
                        v2::MemberPlan::FixedProbeDiscoverSourceV1 { slot, source }
                            if slot.member_id.uuid() == source_member =>
                        {
                            Some(source)
                        }
                        _ => None,
                    })
                })
                .ok_or(OperationError::State)?;
            let graph = role.sealed.as_ref().ok_or(OperationError::State)?;
            let proof = tokio::time::timeout_at(
                deadline.into(),
                graph.try_fixed_planning_no_task_graph(
                    members,
                    source,
                    tasks.settlement_operation_id,
                ),
            )
            .await
            .map_err(lock_error)??;
            if proof.source_member() != source_member
                || !proof.matches(
                    role.binding,
                    role.handle
                        .as_ref()
                        .ok_or(OperationError::State)?
                        .graph_id(),
                    members,
                )
            {
                return Err(OperationError::State);
            }
            let tasks = role.planning_tasks.as_mut().ok_or(OperationError::State)?;
            tasks.registered_unentered_graph = Some(proof);
            // Original worker inputs were already destroyed before their joined signals.
        }
        let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
        let proof = tasks
            .registered_unentered_graph
            .as_ref()
            .ok_or(OperationError::State)?;
        let joined = tasks
            .probe_never_entered
            .as_ref()
            .ok_or(OperationError::State)?;
        let join = tasks.probe_join_operation.ok_or(OperationError::State)?;
        let source = role
            .sources
            .get_mut(&source_member)
            .ok_or(OperationError::State)?;
        tokio::time::timeout_at(
            deadline.into(),
            source.close_fixed_probe_parent_after_registered_nonentry(
                proof,
                joined,
                role.binding,
                join,
            ),
        )
        .await
        .map_err(lock_error)??;
        source.fixed_probe_registered_nonentry_closed_fact(proof, joined, role.binding, join)?;
        Ok(())
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
        self.finish_planning_transition(bundle_id, role, deadline)
            .await?;
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
        // Uncertainty can arise before either collector enters, including
        // retained source closure. Keep ledger and durable Hold consistent.
        self.retain_planning_uncertainty(role.binding)?;
        if role.transition.is_some() {
            if role
                .transition
                .as_ref()
                .is_some_and(|t| t.effect == Effect::Settle)
            {
                return Err(OperationError::State);
            }
            self.finish_planning_transition(bundle_id, role, deadline)
                .await?;
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
        self.finish_planning_transition(bundle_id, role, deadline)
            .await
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
            self.finish_planning_transition(bundle_id, role, deadline)
                .await?;
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
                self.finish_planning_transition(bundle_id, role, deadline)
                    .await?;
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
            self.finish_planning_transition(bundle_id, role, deadline)
                .await?;
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
                        v2::Observed::FixedPlanningEngineTaskV2 {
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
            self.finish_planning_transition(bundle_id, role, deadline)
                .await?;
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
    /// survivor set, build all facts, then move original native evidence into
    /// the one typed settlement and refund P on its committed acknowledgement.
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
        // j. Every P member is either durably observed and closed, or its
        // never-acquired disposition follows from the original input/graph
        // witness. Source parents have their own actual close/absence facts.
        {
            let role = &bundle.roles[0];
            let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
            let never_entered = tasks.never_entered.as_ref();
            let source = role.sources.get(&tasks.source_member);
            let native_ok = if let Some(proof) = tasks.registered_unentered_graph.as_ref() {
                let state = self
                    .ledger
                    .inner
                    .lock()
                    .map_err(|_| OperationError::State)?;
                let no_overage = !state.native_overages.values().any(|overage| {
                    overage.binding == role.binding
                        && overage.observed_extra != ResourceVector::default()
                });
                drop(state);
                no_overage
                    && proof.matches(
                        role.binding,
                        role.handle
                            .as_ref()
                            .ok_or(OperationError::State)?
                            .graph_id(),
                        tasks.registered_unentered_members(role.binding)?,
                    )
                    && source.is_some_and(source_files::SourceCell::parent_closed)
            } else if let Some(proof) = never_entered {
                proof.matches(
                    role.binding,
                    role.handle
                        .as_ref()
                        .ok_or(OperationError::State)?
                        .graph_id(),
                    tasks.never_entered_members()?,
                ) && role.invoked.is_empty()
                    && (source.is_some() != tasks.source_spec_retained())
                    && (source.is_some() || !role.armed.contains(&tasks.source_member))
            } else {
                tasks.native_state() == PlanningNativeState::Joined
                    && tasks.root_observations.iter().all(|o| o.committed)
                    && tasks.probe_task_observation.committed
                    && tasks.engine_task_observation.committed
                    && source.is_some_and(source_files::SourceCell::parent_closed)
            };
            if role.transition.is_some()
                || role.pending_finite_close.is_some()
                || role.graph.is_some()
                || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
                || role.handle.is_none()
                || !native_ok
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
                || role.sources.len() != usize::from(source.is_some())
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

        #[cfg(test)]
        if let Some(tasks) = bundle.roles[0]
            .planning_tasks
            .as_ref()
            .filter(|tasks| tasks.registered_unentered_graph.is_some())
        {
            // Native graph absence and original acquired-parent close already
            // cached; cancelling this borrow cannot consume their witnesses.
            super::planning_tasks::tests::stages::before_registered_preflight(
                tasks.settlement_operation_id,
            )
            .await;
        }

        // l. All facts, in manifest order, before anything is consumed.
        let settlement = {
            let role = &bundle.roles[0];
            let tasks = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
            let never_entered = tasks.never_entered.as_ref();
            let probe = tasks.probe_closed.as_ref();
            let engine = tasks.engine_closed.as_ref();
            let operation_id = v2::Id::new(tasks.settlement_operation_id)?;
            let source_plan = role
                .typed_members
                .as_ref()
                .and_then(|plans| {
                    plans
                        .iter()
                        .find(|plan| plan.member_id() == tasks.source_member)
                })
                .ok_or(OperationError::State)?;
            let source_fact = if let Some(proof) = tasks.registered_unentered_graph.as_ref() {
                role.sources
                    .get(&tasks.source_member)
                    .ok_or(OperationError::State)?
                    .fixed_probe_registered_nonentry_closed_fact(
                        proof,
                        tasks
                            .probe_never_entered
                            .as_ref()
                            .ok_or(OperationError::State)?,
                        role.binding,
                        tasks.probe_join_operation.ok_or(OperationError::State)?,
                    )?
            } else if let Some(proof) = never_entered {
                match role.sources.get(&tasks.source_member) {
                    Some(source) => source.fixed_probe_no_task_closed_fact(proof)?,
                    None => source_files::fixed_probe_unregistered_no_task_closed_fact(
                        source_plan,
                        proof,
                    )?,
                }
            } else {
                role.sources
                    .get(&tasks.source_member)
                    .ok_or(OperationError::State)?
                    .fixed_probe_closed_fact(probe.ok_or(OperationError::State)?)?
            };
            let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
            if planned.len() != 7 {
                return Err(OperationError::State);
            }
            let mut source_fact = Some(source_fact);
            let mut members = Vec::with_capacity(planned.len());
            for plan in planned {
                let fact = match plan {
                    v2::MemberPlan::LegacyV1 { planned }
                        if (never_entered.is_some()
                            || tasks.registered_unentered_graph.is_some())
                            && planned.kind == MemberKind::Root
                            && planned.adapter == Adapter::ProcessRoot =>
                    {
                        let members = if tasks.registered_unentered_graph.is_some() {
                            tasks.registered_unentered_members(role.binding)?
                        } else {
                            tasks.never_entered_members()?
                        };
                        if ![members[0], members[1], members[3], members[4]]
                            .contains(&planned.member_id)
                        {
                            return Err(OperationError::State);
                        }
                        v2::ClosedFact::LegacyNeverAcquiredV1 {
                            planned: planned.clone(),
                            operation_id,
                        }
                    }
                    v2::MemberPlan::FixedProbeDiscoverTaskV1 { task, .. }
                        if tasks.registered_unentered_graph.is_some()
                            && *task == tasks.probe_plan =>
                    {
                        v2::ClosedFact::FixedProbeDiscoverTaskNeverEnteredV1 {
                            task: task.clone(),
                            join_operation_id: v2::Id::new(
                                tasks
                                    .probe_never_entered
                                    .as_ref()
                                    .ok_or(OperationError::State)?
                                    .join_operation(),
                            )?,
                        }
                    }
                    v2::MemberPlan::FixedPlanningEngineTaskV1 { task, .. }
                        if tasks.registered_unentered_graph.is_some()
                            && *task == tasks.engine_plan =>
                    {
                        v2::ClosedFact::FixedPlanningEngineTaskNeverEnteredV1 {
                            member_id: task.member_id,
                            plan_sha256: v2::engine_task_plan_sha256(task)?,
                            join_operation_id: v2::Id::new(
                                tasks
                                    .engine_never_entered
                                    .as_ref()
                                    .ok_or(OperationError::State)?
                                    .join_operation(),
                            )?,
                        }
                    }
                    v2::MemberPlan::FixedProbeDiscoverTaskV1 { task, .. }
                        if never_entered.is_some() && *task == tasks.probe_plan =>
                    {
                        v2::ClosedFact::FixedProbeDiscoverTaskNeverAcquiredV1 {
                            task: task.clone(),
                            operation_id,
                        }
                    }
                    v2::MemberPlan::FixedPlanningEngineTaskV1 { task, .. }
                        if never_entered.is_some() && *task == tasks.engine_plan =>
                    {
                        v2::ClosedFact::FixedPlanningEngineTaskNeverAcquiredV1 {
                            task: task.clone(),
                            operation_id,
                        }
                    }
                    v2::MemberPlan::LegacyV1 { planned }
                        if planned.kind == MemberKind::Root
                            && planned.adapter == Adapter::ProcessRoot =>
                    {
                        let probe = probe.ok_or(OperationError::State)?;
                        let engine = engine.ok_or(OperationError::State)?;
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
                        probe
                            .ok_or(OperationError::State)?
                            .closed_fact(task, operation_id)?
                    }
                    v2::MemberPlan::FixedPlanningEngineTaskV1 { task, .. }
                        if engine.is_some_and(|engine| task == engine.task())
                            && *task == tasks.engine_plan =>
                    {
                        engine
                            .ok_or(OperationError::State)?
                            .closed_fact(operation_id)?
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

        // Check the complete emitted row while the SAME native closures,
        // source owner and manifest are still retained by P. A size refusal
        // cannot consume the one-shot proof or refund the original charge.
        v2::validate_planning_settlement_size(&settlement)?;

        if bundle.roles[0]
            .planning_tasks
            .as_ref()
            .is_some_and(|tasks| tasks.registered_unentered_graph.is_some())
        {
            let role = &mut bundle.roles[0];
            let source_member = role
                .planning_tasks
                .as_ref()
                .ok_or(OperationError::State)?
                .source_member;
            let plans = role.typed_members.as_ref().ok_or(OperationError::State)?;
            if plans.len() != 7 {
                return Err(OperationError::State);
            }
            role.sources
                .get_mut(&source_member)
                .ok_or(OperationError::State)?
                .with_fixed_probe_unentered_observation(|source_observed| {
                    let none = None;
                    let envelopes: [v2::EnvelopeRef<'_>; 7] = std::array::from_fn(|index| {
                        let planned = &plans[index];
                        v2::EnvelopeRef {
                            version: 2,
                            planned,
                            observed: if planned.member_id() == source_member {
                                source_observed
                            } else {
                                &none
                            },
                        }
                    });
                    for envelope in envelopes.iter().copied() {
                        v2::validate_envelope_members_ref(plans, envelope)?;
                    }
                    v2::validate_settlement_scope_refs(
                        v2::Id::new(self.owner.owner_id)?,
                        v2::Id::new(handle.graph_id())?,
                        plans.len(),
                        envelopes.iter().copied(),
                        &settlement,
                    )?;
                    Ok(())
                })?;
        }

        // m. No await or fallible work between consuming native evidence and
        // retaining the exact command in this role's transition.
        bundle.prepare.result = None;
        let role = &mut bundle.roles[0];
        let operation_id = settlement.operation_id.uuid();
        let tasks = role.planning_tasks.as_mut().expect("validated P tasks");
        let evidence = if let Some(graph) = tasks.registered_unentered_graph.take() {
            PlanningNativeEvidence::RegisteredNeverEntered {
                _graph: graph,
                _probe: tasks
                    .probe_never_entered
                    .take()
                    .expect("validated original probe join"),
                _engine: tasks
                    .engine_never_entered
                    .take()
                    .expect("validated original engine join"),
            }
        } else if let Some(graph) = tasks.never_entered.take() {
            PlanningNativeEvidence::NeverEntered { _graph: graph }
        } else {
            PlanningNativeEvidence::Joined {
                _probe: tasks.probe_closed.take().expect("validated probe closure"),
                _engine: tasks
                    .engine_closed
                    .take()
                    .expect("validated engine closure"),
            }
        };
        let receipt = PhysicalCloseReceipt::from_planning_native(PlanningNativeClosure {
            binding: role.binding,
            _evidence: evidence,
        });
        role.planning_tasks = None;
        role.sources.clear();
        // M's survivor sum excludes P's dynamic manifest backing. clear()
        // would retain that allocation after either joined or no-task refund.
        role.members = Vec::new();
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
        self.finish_planning_transition(bundle_id, role, deadline)
            .await?;
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
