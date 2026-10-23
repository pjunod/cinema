//! Private write-ahead bridge. The runtime book, not a request future, owns all
//! provisional graphs, receivers and rejected proof-bearing commands. There is
//! no detached acquisition callback or launch authority in a serialized result.
use super::*;
use crate::adaptive_resources::physical_graph::{
    GraphRoot, OwnedPhysicalGraph, SealedPhysicalGraph,
};
use crate::adaptive_resources::{
    CleanupAuthority, FamilyDemand, FamilyIdentity, FamilyRole, PhysicalCloseReceipt,
};
use crate::resource_custody::{codec_v2 as v2, TypedNewObligation, TypedPrepareBatch};
use crate::resource_custody::{
    Adapter, CommitCertainty, FamilyHandle, FamilyRequest, Manifest, Mutation, NewObligation,
    ObligationHandle, ObservedIdentity, OperationResult, PendingOperation, PhysicalSettlement,
    PlannedMember, PrepareBatch, RejectedTransition, RoleTarget, TransitionKey,
};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

mod bootstrap;
#[cfg(unix)]
pub(super) mod finite_retirement;
#[cfg(unix)]
mod finite_vod;
mod live_settlement;
mod pipe_drains;
pub(super) mod plans;
mod scratch;
#[cfg(unix)]
pub(super) mod source_files;
pub(crate) use plans::{TypedGraphPlan, TypedMemberPlan};

#[derive(Default)]
pub(super) struct Operations {
    draining: bool,
    families: BTreeMap<FamilyIdentity, FamilyHandle>,
    family_users: BTreeMap<FamilyIdentity, BTreeSet<Uuid>>,
    bindings: BTreeMap<ReservationBinding, ObligationHandle>,
    retained_users: BTreeMap<ReservationBinding, BTreeSet<Uuid>>,
    closing: BTreeSet<ReservationBinding>,
    bundles: BTreeMap<Uuid, Arc<AsyncMutex<Bundle>>>,
}

/// Data describing missing roles, in reserve_families' family/role order. The
/// runtime supplies the ACTUAL graph generation and daemon identity itself.
pub(crate) struct GraphPlan {
    pub attempt_id: Option<Uuid>,
    pub members: Vec<PlannedMember>,
}

struct PreparationPlan {
    attempt_id: Option<Uuid>,
    legacy: Vec<PlannedMember>,
    typed: Option<Vec<TypedMemberPlan>>,
}

/// Private process-local selection of runtime-owned state. Not a wire token or
/// a replacement for a journal-minted retained-role handle. Drop cannot refund.
#[derive(Clone)]
pub(crate) struct OwnedPreparation {
    ledger: u64,
    id: Uuid,
    state: Arc<AsyncMutex<Bundle>>,
}

struct Bundle {
    id: Uuid,
    family_ids: Vec<FamilyIdentity>,
    retained: BTreeSet<ReservationBinding>,
    prepare: JournalOperation,
    installed: bool,
    roles: Vec<OwnedRole>,
}

struct OwnedRole {
    binding: ReservationBinding,
    members: Vec<PlannedMember>,
    typed_members: Option<Vec<v2::MemberPlan>>,
    scratch: BTreeMap<Uuid, scratch::ScratchMember>,
    // Same admitted member/ledger; V3 adds bootstrap state, not a second charge.
    bootstrap: BTreeMap<Uuid, bootstrap::BootstrapMember>,
    #[cfg(unix)]
    sources: BTreeMap<Uuid, source_files::SourceCell>,
    #[cfg(unix)]
    finite_vod: Option<finite_vod::FiniteLaunch>,
    #[cfg(unix)]
    retirement: Option<finite_retirement::FiniteRetirement>,
    armed: BTreeSet<Uuid>,
    invoked: BTreeSet<Uuid>,
    roots: BTreeMap<Uuid, GraphRoot>,
    drains: BTreeMap<Uuid, physical_graph::GraphPipeDrain>,
    closed_drains: BTreeMap<Uuid, crate::resource_custody::ClosedMemberFact>,
    graph: Option<OwnedPhysicalGraph>,
    sealed: Option<SealedPhysicalGraph>,
    // Retain the SAME cleanup owner; never reconstruct one from a UUID.
    _cleanup: CleanupAuthority,
    handle: Option<ObligationHandle>,
    sequence: i64,
    phase: Phase,
    transition: Option<Transition>,
    receipt: Option<PhysicalCloseReceipt>,
    refunded: bool,
}

struct Transition {
    operation: JournalOperation,
    effect: Effect,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Arm(Uuid),
    Scratch(Uuid),
    Bootstrap(Uuid),
    #[cfg(unix)]
    Source(Uuid),
    #[cfg(unix)]
    SourceRoot(Uuid),
    #[cfg(unix)]
    FiniteTask(Uuid),
    Enrich,
    Seal,
    Hold,
    Cancel,
    Settle,
}

struct JournalOperation {
    id: Uuid,
    pending: Option<PendingOperation>,
    result: Option<Result<OperationResult, CustodyError>>,
    rejected: Option<RejectedTransition>,
    acknowledged: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OperationError {
    #[error("resource admission refused: {0:?}")]
    Admission(AdmissionRefusal),
    #[error("{0}")]
    Custody(#[from] CustodyError),
    #[error("physical operation failed")]
    Physical(#[from] std::io::Error),
    #[error("resource runtime draining")]
    Draining,
    #[error("resource operation deadline elapsed")]
    Deadline,
    #[error("resource runtime operation state unavailable or mismatched")]
    State,
}
impl From<AdmissionRefusal> for OperationError {
    fn from(error: AdmissionRefusal) -> Self {
        Self::Admission(error)
    }
}

impl JournalOperation {
    fn queued(pending: PendingOperation) -> Self {
        Self {
            id: pending.operation_id(),
            pending: Some(pending),
            result: None,
            rejected: None,
            acknowledged: false,
        }
    }
    fn transition(result: Result<PendingOperation, RejectedTransition>) -> Self {
        match result {
            Ok(pending) => Self::queued(pending),
            Err(rejected) => Self {
                id: rejected.key.operation_id,
                pending: None,
                result: Some(Err(rejected.error.clone())),
                rejected: Some(rejected),
                acknowledged: false,
            },
        }
    }
    async fn resolve(&mut self) -> Result<OperationResult, OperationError> {
        if let Some(pending) = self.pending.as_mut() {
            // Await IN PLACE: cancellation leaves receiver and exact command in
            // owned state. No repeat submit, lost result or racing fresh UUID.
            let result = pending.wait_retained().await;
            self.pending = None;
            self.result = Some(result);
        }
        self.result
            .clone()
            .ok_or(OperationError::State)?
            .map_err(Into::into)
    }
    fn retry(&mut self, journal: &CustodyJournal, deadline: Instant) -> Result<(), OperationError> {
        if self.pending.is_some() || self.acknowledged {
            return Err(OperationError::State);
        }
        if let Some(rejected) = self.rejected.take() {
            match journal.change(rejected.key, rejected.mutation, deadline) {
                Ok(pending) => {
                    self.pending = Some(pending);
                    self.result = None;
                }
                Err(rejected) => {
                    let error = rejected.error.clone();
                    self.result = Some(Err(error.clone()));
                    self.rejected = Some(rejected);
                    return Err(error.into());
                }
            }
        } else {
            self.pending = Some(journal.retry(self.id, deadline)?);
            self.result = None;
        }
        Ok(())
    }
    fn acknowledge(&mut self, journal: &CustodyJournal) -> Result<(), OperationError> {
        if !self.acknowledged {
            journal.acknowledge(self.id)?;
            self.acknowledged = true;
        }
        Ok(())
    }
}

impl NodeResourceRuntime {
    /// Only authoritative observations/settings composition may update this;
    /// the value remains Known/Unknown and does not rewrite explicit settings.
    pub(crate) fn update_capacity(
        &self,
        epoch: u64,
        cap: Metric<ResourceVector>,
    ) -> Result<(), OperationError> {
        self.ledger.update_hard_cap(epoch, cap)?;
        Ok(())
    }

    pub(crate) fn new_family(&self) -> Result<FamilyIdentity, OperationError> {
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        if book.draining {
            return Err(OperationError::Draining);
        }
        if book.families.len() >= MAX_RESERVATIONS * 2 {
            return Err(AdmissionRefusal::LedgerFull.into());
        }
        let identity = self.ledger.new_family()?;
        book.families.insert(identity, self.journal.new_family());
        Ok(identity)
    }

    /// Drop only an unused selector after a definitely rejected preparation.
    /// No obligation, charge or native owner is removed by this operation.
    pub(crate) fn remove_unused_family(
        &self,
        family: FamilyIdentity,
    ) -> Result<(), OperationError> {
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        if book.family_users.contains_key(&family) {
            return Err(OperationError::State);
        }
        book.families.remove(&family).ok_or(OperationError::State)?;
        Ok(())
    }

    /// Linearize fresh native entry against epoch changes, drain and overage.
    /// The original job is already registered. Refusal drops only its unopened
    /// latch; its retained owner still joins and records the exact disposition.
    pub(crate) fn release_native_entry(
        &self,
        binding: ReservationBinding,
        enter: tokio::sync::oneshot::Sender<()>,
    ) -> Result<(), OperationError> {
        let state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        let entry = state
            .entries
            .get(&binding.reservation)
            .ok_or(OperationError::State)?;
        if binding.ledger != self.ledger.id
            || entry.binding != binding
            || state.invalid
            || state.native_overage_fenced
            || state.draining
            || state.epoch != binding.allocation_epoch
            || entry.durable.is_none()
            || !matches!(
                entry.state,
                ReservationState::Reserved | ReservationState::SpawnPossible
            )
        {
            return Err(OperationError::State);
        }
        // Sending is synchronous and performs no filesystem IO under this lock.
        // An overage fence installed afterward cannot retroactively undo entry.
        enter.send(()).map_err(|_| OperationError::State)
    }

    /// Synchronous OWNERSHIP transfer before the first await. All missing roles
    /// reserve atomically; retained roles must already have live journal handles.
    /// No mutable physical graph or public acquisition authority escapes here.
    pub(crate) fn prepare_families(
        &self,
        families: &[FamilyDemand<'_>],
        plans: Vec<GraphPlan>,
        deadline: Instant,
    ) -> Result<OwnedPreparation, OperationError> {
        self.prepare_plan_families(
            families,
            plans
                .into_iter()
                .map(|plan| PreparationPlan {
                    attempt_id: plan.attempt_id,
                    legacy: plan.members,
                    typed: None,
                })
                .collect(),
            false,
            deadline,
        )
    }

    pub(crate) fn prepare_typed_families(
        &self,
        families: &[FamilyDemand<'_>],
        plans: Vec<TypedGraphPlan>,
        deadline: Instant,
    ) -> Result<OwnedPreparation, OperationError> {
        self.prepare_plan_families(
            families,
            plans
                .into_iter()
                .map(|plan| PreparationPlan {
                    attempt_id: plan.attempt_id,
                    legacy: plan
                        .members
                        .iter()
                        .filter_map(|member| member.legacy().cloned())
                        .collect(),
                    typed: Some(plan.members),
                })
                .collect(),
            true,
            deadline,
        )
    }

    // Same reservation/family/cancellation/refund owner for both codec versions.
    fn prepare_plan_families(
        &self,
        families: &[FamilyDemand<'_>],
        plans: Vec<PreparationPlan>,
        typed: bool,
        deadline: Instant,
    ) -> Result<OwnedPreparation, OperationError> {
        if families.is_empty()
            || families.len() > 2
            || plans.is_empty()
            || plans.len() > 6
            || families
                .iter()
                .any(|family| family.roles.is_empty() || family.roles.len() > 3)
        {
            return Err(AdmissionRefusal::InvalidBundle.into());
        }
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        if book.draining {
            return Err(OperationError::Draining);
        }
        if book.bundles.len() >= MAX_RESERVATIONS {
            return Err(AdmissionRefusal::LedgerFull.into());
        }
        let mut requests = Vec::new();
        let mut missing = 0;
        let mut trial = None;
        let mut retained_bindings = BTreeSet::new();
        for family in families {
            let durable_family = book
                .families
                .get(&family.identity)
                .ok_or(OperationError::State)?;
            let mut roles = Vec::new();
            for role in family.roles {
                let target = if let Some(retained) = role.retained {
                    if book.closing.contains(&retained.binding) {
                        return Err(OperationError::State);
                    }
                    retained_bindings.insert(retained.binding);
                    RoleTarget::Retained(
                        book.bindings
                            .get(&retained.binding)
                            .ok_or(OperationError::State)?
                            .clone(),
                    )
                } else {
                    trial.get_or_insert(role.demand.trial);
                    let index = missing;
                    missing += 1;
                    RoleTarget::New(index)
                };
                roles.push((
                    match role.role {
                        FamilyRole::Video => Role::Video,
                        FamilyRole::Audio => Role::Audio,
                        FamilyRole::Companion => Role::Companion,
                    },
                    target,
                ));
            }
            requests.push(FamilyRequest {
                family: durable_family.clone(),
                roles,
            });
        }
        if plans.len() != missing || missing == 0 {
            return Err(AdmissionRefusal::InvalidBundle.into());
        }
        let owned = self.ledger.reserve_families(families)?;
        let id = Uuid::new_v4();
        let mut obligations = Vec::new();
        let mut typed_obligations = Vec::new();
        let mut roles = Vec::new();
        for ((reservation, cleanup), plan) in owned.into_iter().zip(plans) {
            let binding = reservation.custody_binding();
            // Graph construction creates only private memory custody, no kernel
            // object, child, task, file or other counted member.
            let graph = reservation.into_physical_graph()?;
            let identity = graph.identity();
            if identity.process_instance != self.owner.owner_id {
                return Err(OperationError::State);
            }
            // Resolve process-local references exactly once, BEFORE any journal
            // command/fingerprint exists. No supplied or guessed graph ID is
            // accepted for source-copy owners, recovery or the actual VOD task.
            let typed_members = plan
                .typed
                .map(|members| {
                    let owner_id = v2::Id::new(identity.process_instance)?;
                    let graph_id = v2::Id::new(identity.generation)?;
                    Ok::<_, CustodyError>(
                        members
                            .into_iter()
                            .map(|member| member.bind(owner_id, graph_id))
                            .collect::<Vec<_>>(),
                    )
                })
                .transpose()?;
            let resources = self
                .ledger
                .inner
                .lock()
                .map_err(|_| OperationError::State)?
                .entries
                .get(&binding.reservation)
                .ok_or(OperationError::State)?
                .resources;
            if let Some(members) = &typed_members {
                typed_obligations.push(TypedNewObligation {
                    runtime_binding: binding,
                    allocation_epoch: binding.allocation_epoch,
                    resources,
                    manifest: v2::Manifest {
                        version: 2,
                        owner_id: v2::Id::new(identity.process_instance)?,
                        graph_id: v2::Id::new(identity.generation)?,
                        attempt_id: plan.attempt_id.map(v2::Id::new).transpose()?,
                        members: members.clone(),
                    },
                });
            } else {
                obligations.push(NewObligation {
                    runtime_binding: binding,
                    allocation_epoch: binding.allocation_epoch,
                    resources,
                    manifest: Manifest {
                        version: 1,
                        owner_id: identity.process_instance,
                        graph_id: identity.generation,
                        attempt_id: plan.attempt_id,
                        members: plan.legacy.clone(),
                    },
                });
            }
            roles.push(OwnedRole {
                binding,
                members: plan.legacy,
                typed_members,
                scratch: BTreeMap::new(),
                bootstrap: BTreeMap::new(),
                #[cfg(unix)]
                sources: BTreeMap::new(),
                #[cfg(unix)]
                finite_vod: None,
                #[cfg(unix)]
                retirement: None,
                armed: BTreeSet::new(),
                invoked: BTreeSet::new(),
                roots: BTreeMap::new(),
                drains: BTreeMap::new(),
                closed_drains: BTreeMap::new(),
                graph: Some(graph),
                sealed: None,
                _cleanup: cleanup,
                handle: None,
                sequence: 0,
                phase: Phase::Prepared,
                transition: None,
                receipt: None,
                refunded: false,
            });
        }
        // All fallible local construction precedes the write-ahead binding. No
        // DB operation, randomness or OS call occurs under this accounting lock.
        {
            let mut state = self
                .ledger
                .inner
                .lock()
                .map_err(|_| OperationError::State)?;
            for role in &roles {
                let entry = state
                    .entries
                    .get(&role.binding.reservation)
                    .ok_or(OperationError::State)?;
                if entry.binding != role.binding
                    || entry.durable.is_some()
                    || entry.state != ReservationState::Reserved
                {
                    return Err(OperationError::State);
                }
            }
            for role in &roles {
                state
                    .entries
                    .get_mut(&role.binding.reservation)
                    .ok_or(OperationError::State)?
                    .durable = Some(id);
            }
        }
        // Queue acceptance transfers the command to the DB owner. Even a
        // prepare timeout after acceptance cannot make these charges disappear.
        let queued = if typed {
            self.journal.prepare_typed(
                id,
                TypedPrepareBatch {
                    obligations: typed_obligations,
                    families: requests,
                    trial: trial.unwrap_or(false),
                },
                deadline,
            )
        } else {
            self.journal.prepare(
                id,
                PrepareBatch {
                    obligations,
                    families: requests,
                    trial: trial.unwrap_or(false),
                },
                deadline,
            )
        };
        let pending = match queued {
            Ok(pending) => pending,
            Err(error) => {
                // enqueue failure is definite NOT accepted, before acquisition.
                // Keep any failed refund held; never turn it into capacity.
                for role in &mut roles {
                    refund(&self.ledger, id, role)?;
                }
                return Err(error.into());
            }
        };
        let state = Arc::new(AsyncMutex::new(Bundle {
            id,
            family_ids: families.iter().map(|f| f.identity).collect(),
            retained: retained_bindings.clone(),
            prepare: JournalOperation::queued(pending),
            installed: false,
            roles,
        }));
        book.bundles.insert(id, Arc::clone(&state));
        for family in families {
            book.family_users
                .entry(family.identity)
                .or_default()
                .insert(id);
        }
        for binding in retained_bindings {
            book.retained_users.entry(binding).or_default().insert(id);
        }
        Ok(OwnedPreparation {
            ledger: self.ledger.id,
            id,
            state,
        })
    }

    fn validate_preparation(&self, owner: &OwnedPreparation) -> Result<(), OperationError> {
        let book = self.operations.lock().map_err(|_| OperationError::State)?;
        if owner.ledger != self.ledger.id
            || !book
                .bundles
                .get(&owner.id)
                .is_some_and(|state| Arc::ptr_eq(state, &owner.state))
        {
            return Err(OperationError::State);
        }
        Ok(())
    }

    fn eligible(&self, bundle: &Bundle) -> Result<(), OperationError> {
        if self
            .operations
            .lock()
            .map_err(|_| OperationError::State)?
            .draining
        {
            return Err(OperationError::Draining);
        }
        let state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        if state.invalid
            || state.native_overage_fenced
            || bundle
                .roles
                .iter()
                .any(|role| role.binding.allocation_epoch != state.epoch)
        {
            return Err(AdmissionRefusal::StaleEpoch.into());
        }
        Ok(())
    }

    /// This borrowed wait may be dropped at ANY await. Runtime retains the
    /// bundle, receiver, graph and exact operation; existing supervision resumes.
    pub(crate) async fn resolve_preparation(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            let prepared = match bundle.prepare.resolve().await {
                Ok(OperationResult::Prepared(prepared)) => prepared,
                Ok(_) => return Err(OperationError::State),
                Err(OperationError::Custody(error))
                    if error.certainty != CommitCertainty::Unknown =>
                {
                    let id = bundle.id;
                    for role in &mut bundle.roles {
                        refund(&self.ledger, id, role)?;
                    }
                    bundle.prepare.acknowledge(&self.journal)?;
                    return Err(error.into());
                }
                Err(error) => return Err(error),
            };
            if prepared.obligations.len() != bundle.roles.len() || prepared.sequence < 1 {
                return Err(OperationError::State);
            }
            for (role, handle) in bundle.roles.iter().zip(&prepared.obligations) {
                if role
                    .graph
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .identity()
                    .generation
                    != handle.graph_id()
                {
                    return Err(OperationError::State);
                }
            }
            {
                let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
                for (role, handle) in bundle.roles.iter_mut().zip(prepared.obligations) {
                    book.bindings.insert(role.binding, handle.clone());
                    role.handle = Some(handle);
                    role.sequence = prepared.sequence;
                }
            }
            bundle.installed = true;
        }
        bundle.prepare.acknowledge(&self.journal)?;
        let eligibility = if Instant::now() >= deadline {
            Err(OperationError::Deadline)
        } else {
            self.eligible(&bundle)
        };
        if let Err(error) = eligibility {
            // A late commit never bypasses current cap epoch/drain. Cancel each
            // intact Prepared member durably; cancellation midway retains the
            // exact pending transition and every unrefunded role.
            self.cancel_bundle(&mut bundle, deadline).await?;
            return Err(error);
        }
        if bundle.roles.iter().any(|role| role.refunded) {
            return Err(OperationError::State);
        }
        Ok(())
    }

    pub(crate) async fn retry_preparation(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        owner
            .state
            .lock()
            .await
            .prepare
            .retry(&self.journal, deadline)
    }

    /// The actual owned launcher is reached only AFTER arm commit. A named root
    /// can be invoked once; ambiguous/cancelled invocation is never replayed.
    /// LiveOnly roots remain unsupported for production restart qualification.
    pub(crate) async fn spawn_root(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        command: &mut tokio::process::Command,
        work: crate::process::ChildWork,
        deadline: Instant,
    ) -> Result<GraphRoot, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        self.eligible(&bundle)?;
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        self.launch_root_role(id, role, member, command, work, deadline, None)
            .await
    }

    // Callers are private concrete adapters or the existing legacy entry above.
    // The role lock retains actual source descriptors throughout native spawn.
    #[allow(clippy::too_many_arguments)]
    async fn launch_root_role(
        &self,
        id: Uuid,
        role: &mut OwnedRole,
        member: Uuid,
        command: &mut tokio::process::Command,
        work: crate::process::ChildWork,
        deadline: Instant,
        named_arm: Option<Uuid>,
    ) -> Result<GraphRoot, OperationError> {
        let plan = role
            .members
            .iter()
            .find(|plan| plan.member_id == member && plan.adapter == Adapter::ProcessRoot)
            .cloned()
            .ok_or(OperationError::State)?;
        if role.refunded
            || role.graph.is_none()
            || role.invoked.contains(&member)
            || (role.armed.contains(&member) && named_arm.is_none())
            || role
                .transition
                .as_ref()
                .is_some_and(|transition| transition.effect != Effect::Arm(member))
        {
            return Err(OperationError::State);
        }
        if !role.armed.contains(&member) {
            if role.transition.is_none() {
                start_named_transition(
                    &self.journal,
                    role,
                    named_arm.unwrap_or_else(Uuid::new_v4),
                    Mutation::Arm {
                        members: vec![member],
                    },
                    Effect::Arm(member),
                    deadline,
                )?;
            } else if role.transition.as_ref().is_none_or(|t| {
                t.effect != Effect::Arm(member) || named_arm.is_some_and(|id| t.operation.id != id)
            }) {
                return Err(OperationError::State);
            }
            self.finish_transition(id, role).await?;
        }
        if Instant::now() >= deadline {
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
            start_transition(
                &self.journal,
                role,
                Mutation::Hold {
                    reason: "deadline elapsed after arm".into(),
                },
                Effect::Hold,
                deadline,
            )?;
            return Err(OperationError::Deadline);
        }
        // The fixed adapter records entry under the SAME graph lock as the
        // actual spawn. Its original source cells remain borrowed in this role.
        if named_arm.is_none() {
            role.invoked.insert(member);
        }
        let graph = role.graph.as_mut().ok_or(OperationError::State)?;
        let arm = DurableArm {
            binding: role.binding,
            generation: graph.identity().generation,
        };
        let launched = if named_arm.is_some() {
            #[cfg(unix)]
            {
                let finite = role.finite_vod.as_mut().ok_or(OperationError::State)?;
                if finite.root_member != member || Some(finite.acquisition_id) != named_arm {
                    return Err(OperationError::State);
                }
                graph
                    .spawn_selected_durable_root(command, work, arm, &mut finite.entered)
                    .await
            }
            #[cfg(not(unix))]
            {
                return Err(OperationError::State);
            }
        } else {
            graph.spawn_durable_root(command, work, arm).await
        };
        role.invoked.insert(member);
        let root = match launched {
            Ok(root) => root,
            Err(error) => {
                if let Some(graph) = role.graph.take() {
                    role.sealed = Some(graph.seal());
                }
                start_transition(
                    &self.journal,
                    role,
                    Mutation::Hold {
                        reason: "root acquisition incomplete".into(),
                    },
                    Effect::Hold,
                    deadline,
                )?;
                // Keep the pending hold even if its waiter is never resumed.
                return Err(error.into());
            }
        };
        role.roots.insert(member, root);
        #[cfg(unix)]
        if named_arm.is_some() {
            let control = root.registered_control().ok_or(OperationError::State)?;
            role.finite_vod
                .as_ref()
                .ok_or(OperationError::State)?
                .control
                .install(control)?;
        }
        let Metric::Known(process) = root.process else {
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
            start_transition(
                &self.journal,
                role,
                Mutation::Hold {
                    reason: "root identity unavailable".into(),
                },
                Effect::Hold,
                deadline,
            )?;
            return Err(AdmissionRefusal::InsufficientEvidence.into());
        };
        let identity = ObservedIdentity {
            version: 1,
            member_id: member,
            adapter: Adapter::ProcessRoot,
            adapter_version: plan.adapter_version,
            domain: plan.domain.clone(),
            object_identity: format!("{}:{}", process.pid, process.start_marker),
            process: Some(crate::resource_custody::ProcessIdentity {
                version: 1,
                pid: process.pid,
                creation_marker: Some(process.start_marker),
                domain: plan.domain,
            }),
        };
        let mutation = acquired_mutation(role, identity)?;
        start_transition(&self.journal, role, mutation, Effect::Enrich, deadline)?;
        if let Err(error) = self.finish_transition(id, role).await {
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
            return Err(error);
        }
        Ok(root)
    }

    pub(crate) async fn cancel_preparation(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !bundle.installed {
            return Err(OperationError::State);
        }
        self.cancel_bundle(&mut bundle, deadline).await
    }

    async fn cancel_bundle(
        &self,
        bundle: &mut Bundle,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        for role in &mut bundle.roles {
            if role.refunded {
                continue;
            }
            if role.transition.is_none() {
                if role.phase != Phase::Prepared {
                    return Err(OperationError::State);
                }
                // Remove all live acquisition capability BEFORE enqueue.
                self.close_retained_entry(role.binding)?;
                if let Some(graph) = role.graph.take() {
                    role.sealed = Some(graph.seal());
                }
                start_transition(
                    &self.journal,
                    role,
                    Mutation::CancelPrepared,
                    Effect::Cancel,
                    deadline,
                )?;
            } else if role
                .transition
                .as_ref()
                .is_none_or(|t| t.effect != Effect::Cancel)
            {
                return Err(OperationError::State);
            }
            self.finish_transition(bundle.id, role).await?;
        }
        Ok(())
    }

    /// Entry is sealed in memory BEFORE the first DB/await boundary.
    pub(crate) async fn seal_role(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        self.close_retained_entry(role.binding)?;
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        if role.transition.is_none() {
            start_transition(&self.journal, role, Mutation::Seal, Effect::Seal, deadline)?;
        } else if role
            .transition
            .as_ref()
            .is_none_or(|t| t.effect != Effect::Seal)
        {
            return Err(OperationError::State);
        }
        self.finish_transition(id, role).await
    }

    /// Physical proof is transferred into owned state/queue before any wait.
    /// This does not manufacture proof; actual positive backend is still owed.
    pub(crate) fn settle_role(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        proof: PhysicalSettlement,
        deadline: Instant,
    ) -> Result<(), (OperationError, Option<PhysicalSettlement>)> {
        if let Err(error) = self.validate_preparation(owner) {
            return Err((error, Some(proof)));
        }
        // Proof transfers synchronously BEFORE ANY await. A busy owner returns
        // the actual proof; caller cancellation cannot drop it inside a future.
        let Ok(mut bundle) = owner.state.try_lock() else {
            return Err((OperationError::State, Some(proof)));
        };
        let Some(role) = bundle.roles.get_mut(index) else {
            return Err((OperationError::State, Some(proof)));
        };
        if role.transition.is_some()
            || role.refunded
            || role.handle.is_none()
            || !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
            || role.graph.is_some()
            || proof.receipt.custody_binding() != role.binding
        {
            return Err((OperationError::State, Some(proof)));
        }
        // start_transition cannot refuse after these checks; enqueue rejection
        // retains the exact returned command/proof in the runtime operation.
        if let Err(error) = self.close_retained_entry(role.binding) {
            return Err((error, Some(proof)));
        }
        if let Err(error) = start_transition(
            &self.journal,
            role,
            Mutation::Settle(proof),
            Effect::Settle,
            deadline,
        ) {
            return Err((error, None));
        }
        Ok(())
    }

    /// Correct data-only facts only after DEFINITE failure. The original proof
    /// returns from the journal owner; unknown commits cannot change payload/ID.
    pub(crate) fn correct_settlement_facts(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        facts: Vec<crate::resource_custody::ClosedMemberFact>,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let transition = role.transition.as_mut().ok_or(OperationError::State)?;
        if transition.effect != Effect::Settle
            || transition.operation.pending.is_some()
            || !matches!(&transition.operation.result, Some(Err(error)) if error.certainty != CommitCertainty::Unknown)
        {
            return Err(OperationError::State);
        }
        if transition.operation.rejected.is_none() {
            transition.operation.rejected = Some(
                self.journal
                    .take_failed_transition(transition.operation.id)?,
            );
        }
        let rejected = transition
            .operation
            .rejected
            .as_mut()
            .ok_or(OperationError::State)?;
        let Mutation::Settle(proof) = &mut rejected.mutation else {
            return Err(OperationError::State);
        };
        proof.facts = facts;
        // A new payload is legal only because the exact old op DEFINITELY did
        // not commit. Keep that returned proof in owned state before retry.
        rejected.key.operation_id = Uuid::new_v4();
        transition.operation.id = rejected.key.operation_id;
        transition.operation.retry(&self.journal, deadline)
    }

    pub(crate) async fn resume_role(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        retry_deadline: Option<Instant>,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        let id = bundle.id;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        if let Some(deadline) = retry_deadline {
            role.transition
                .as_mut()
                .ok_or(OperationError::State)?
                .operation
                .retry(&self.journal, deadline)?;
        }
        self.finish_transition(id, role).await
    }

    async fn finish_transition(
        &self,
        bundle_id: Uuid,
        role: &mut OwnedRole,
    ) -> Result<(), OperationError> {
        let transition = role.transition.as_mut().ok_or(OperationError::State)?;
        let OperationResult::Changed(ack) = transition.operation.resolve().await? else {
            return Err(OperationError::State);
        };
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        if ack.operation_id != transition.operation.id
            || ack.obligation_id != handle.obligation_id()
            || ack.graph_id != handle.graph_id()
            || ack.sequence <= role.sequence
        {
            return Err(OperationError::State);
        }
        match transition.effect {
            Effect::Scratch(member) => scratch::apply_committed(role, member, ack.operation_id)?,
            Effect::Bootstrap(member) => {
                bootstrap::apply_committed(role, member, ack.operation_id)?
            }
            _ => {}
        }
        let transition = role.transition.as_mut().ok_or(OperationError::State)?;
        if matches!(transition.effect, Effect::Cancel | Effect::Settle) {
            if ack.phase != Phase::Settled {
                return Err(OperationError::State);
            }
            if transition.effect == Effect::Settle && !role.refunded && role.receipt.is_none() {
                role.receipt = Some(
                    self.journal
                        .take_settled_receipt(transition.operation.id, &handle)?,
                );
            }
            // Durable acknowledgement from THIS retained operation (never a
            // disk/DTO ack) precedes the sole private matching refund.
            refund(&self.ledger, bundle_id, role)?;
        }
        let transition = role.transition.as_mut().ok_or(OperationError::State)?;
        if transition.effect != Effect::Settle {
            transition.operation.acknowledge(&self.journal)?;
        }
        if let Effect::Arm(member) = transition.effect {
            role.armed.insert(member);
        }
        match transition.effect {
            Effect::Scratch(member) => scratch::acknowledge(role, member)?,
            Effect::Bootstrap(member) => bootstrap::acknowledge(role, member)?,
            #[cfg(unix)]
            Effect::Source(member) => source_files::acknowledge(role, member, ack.operation_id)?,
            #[cfg(unix)]
            Effect::SourceRoot(member) => {
                source_files::acknowledge_root(role, member, ack.operation_id)?
            }
            #[cfg(unix)]
            Effect::FiniteTask(member) => {
                finite_vod::acknowledge_task(role, member, ack.operation_id)?
            }
            _ => {}
        }
        role.sequence = ack.sequence;
        role.phase = ack.phase;
        role.receipt = None;
        role.transition = None;
        Ok(())
    }

    /// Explicit existing-supervisor entry, not a timer/TTL task. Handles remain
    /// process-local and permit cleanup only; requests cannot deserialize them.
    pub(crate) fn retained_preparations(&self) -> Result<Vec<OwnedPreparation>, OperationError> {
        let book = self.operations.lock().map_err(|_| OperationError::State)?;
        Ok(book
            .bundles
            .iter()
            .map(|(id, state)| OwnedPreparation {
                ledger: self.ledger.id,
                id: *id,
                state: Arc::clone(state),
            })
            .collect())
    }

    pub(crate) fn begin_drain(&self) -> Result<(), OperationError> {
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        book.draining = true;
        state.draining = true;
        Ok(())
    }

    /// Bounded in-memory operation retention is reclaimed only after ALL exact
    /// roles have refunded and all proof-bearing operations have completed.
    fn close_retained_entry(&self, binding: ReservationBinding) -> Result<(), OperationError> {
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        if book
            .retained_users
            .get(&binding)
            .is_some_and(|users| !users.is_empty())
        {
            return Err(OperationError::State);
        }
        // Serialized with registration: no successor can pin this role after
        // retirement starts; existing users must close or transfer first.
        book.closing.insert(binding);
        Ok(())
    }

    /// Bounded in-memory operation retention is reclaimed only after ALL exact
    /// roles have refunded and all proof-bearing operations have completed.
    pub(crate) async fn forget_preparation(
        &self,
        owner: OwnedPreparation,
    ) -> Result<(), OperationError> {
        self.validate_preparation(&owner)?;
        let bundle = owner.state.lock().await;
        if bundle
            .roles
            .iter()
            .any(|role| !role.refunded || role.transition.is_some())
            || !bundle.prepare.acknowledged
        {
            return Err(OperationError::State);
        }
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        book.bundles.remove(&owner.id);
        for role in &bundle.roles {
            book.bindings.remove(&role.binding);
            book.closing.remove(&role.binding);
        }
        for binding in &bundle.retained {
            if let Some(users) = book.retained_users.get_mut(binding) {
                users.remove(&bundle.id);
                if users.is_empty() {
                    book.retained_users.remove(binding);
                }
            }
        }
        // Family IDs cannot be reused. Removing this live lookup preserves the
        // on-disk history while allowing a long-running daemon to mint new ones.
        for id in &bundle.family_ids {
            if let Some(users) = book.family_users.get_mut(id) {
                users.remove(&bundle.id);
                if users.is_empty() {
                    book.family_users.remove(id);
                    book.families.remove(id);
                }
            }
        }
        Ok(())
    }
}

/// Only an actual legacy adapter can enrich its matching slot inside a typed
/// envelope. Source/output/scratch observations need their concrete adapters;
/// this helper cannot coerce a legacy observation into one of those variants.
fn acquired_mutation(
    role: &OwnedRole,
    identity: ObservedIdentity,
) -> Result<Mutation, OperationError> {
    let Some(members) = &role.typed_members else {
        return Ok(Mutation::RecordAcquired { identity });
    };
    let planned = members.iter().find(|member| {
        matches!(member, v2::MemberPlan::LegacyV1 { planned } if planned.member_id == identity.member_id)
    }).cloned().ok_or(OperationError::State)?;
    Ok(Mutation::RecordTypedAcquired {
        envelope: v2::Envelope {
            version: 2,
            planned,
            observed: Some(v2::Observed::LegacyV1 { identity }),
        },
    })
}

fn start_transition(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    mutation: Mutation,
    effect: Effect,
    deadline: Instant,
) -> Result<(), OperationError> {
    start_named_transition(journal, role, Uuid::new_v4(), mutation, effect, deadline)
}

fn start_named_transition(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    operation_id: Uuid,
    mutation: Mutation,
    effect: Effect,
    deadline: Instant,
) -> Result<(), OperationError> {
    if role.transition.is_some() || role.refunded {
        return Err(OperationError::State);
    }
    let handle = role.handle.clone().ok_or(OperationError::State)?;
    let key = TransitionKey {
        handle,
        operation_id,
        expected_sequence: role.sequence,
    };
    role.transition = Some(Transition {
        operation: JournalOperation::transition(journal.change(key, mutation, deadline)),
        effect,
    });
    Ok(())
}

/// Only callers owning definite unaccepted prepare, committed never-armed
/// cancellation, or the ORIGINAL receipt from exact committed settlement enter.
/// This function is private; public CleanupAuthority/Drop cannot clear the tag.
fn refund(
    ledger: &ResourceLedger,
    bundle_id: Uuid,
    role: &mut OwnedRole,
) -> Result<(), OperationError> {
    if role.refunded {
        return Ok(());
    }
    if role
        .receipt
        .as_ref()
        .is_some_and(|receipt| receipt.custody_binding() != role.binding)
    {
        return Err(OperationError::State);
    }
    let mut state = ledger.inner.lock().map_err(|_| OperationError::State)?;
    let entry = state
        .entries
        .get(&role.binding.reservation)
        .ok_or(OperationError::State)?;
    if entry.binding != role.binding || entry.durable != Some(bundle_id) {
        return Err(OperationError::State);
    }
    // Prevalidate arithmetic so failure cannot clear the write-ahead guard.
    if state.used.checked_sub(entry.resources).is_none() {
        return Err(OperationError::State);
    }
    state
        .entries
        .get_mut(&role.binding.reservation)
        .ok_or(OperationError::State)?
        .durable = None;
    state.release(role.binding.reservation)?;
    role.refunded = true;
    Ok(())
}

#[cfg(test)]
mod tests;
