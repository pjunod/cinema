//! Original P task registration. Every native future is installed in the
//! charged bundle behind a start latch before it can first poll.
use super::super::planning_probe::{FixedPlanningProbeRequest, FixedPlanningSourceRequest};
use super::*;
use crate::adaptive_resources::physical_graph::{
    fixed_engine::{FixedEngineKnownClosed, FixedEngineNativeClosure, FixedEngineReportData},
    fixed_probe::{FixedProbeKnownClosed, FixedProbeNativeClosure, FixedProbeReportData},
};
use crate::process::fixed_planning_engine::{
    FixedEngineNativeCompletion, FixedEngineNativeFailure,
};
use crate::process::fixed_probe::{
    AdmittedProbeCompletion, AdmittedProbeNativeFailure, DecodeFactError,
};

pub(super) struct PlanningTasks {
    pub(super) probe_plan: v2::FixedProbeDiscoverTaskV1,
    pub(super) engine_plan: v2::FixedPlanningEngineTaskV1,
    pub(super) source_member: Uuid,
    source_spec: Option<source_files::SourceOpenSpec>,
    inputs: Option<FixedPlanningProbeRequest>,
    source: Option<FixedPlanningSourceRequest>,
    observed_source: Option<String>,
    probe: Option<
        tokio::task::JoinHandle<Result<AdmittedProbeCompletion, AdmittedProbeNativeFailure>>,
    >,
    engine: Option<
        tokio::task::JoinHandle<Result<FixedEngineNativeCompletion, FixedEngineNativeFailure>>,
    >,
    // A failed native task may still own its original child, job, pidfd or
    // pipe. Keep that owner in the charged P book rather than reducing it to
    // an error string and implicitly dropping the only cleanup authority.
    engine_failure: Option<FixedEngineNativeFailure>,
    probe_failure: Option<AdmittedProbeNativeFailure>,
    probe_report: Option<FixedProbeReportData>,
    engine_report: Option<FixedEngineReportData>,
    pub(super) probe_closed: Option<FixedProbeNativeClosure>,
    pub(super) engine_closed: Option<FixedEngineNativeClosure>,
    /// Bounded audit observations captured at join, before report data
    /// moves to M. They stay charged under P until its settlement.
    pub(super) probe_observed: Option<v2::FixedProbeDiscoverTaskObservedV1>,
    pub(super) engine_observed: Option<v2::FixedPlanningEngineTaskObservedV2>,
    /// Pre-minted, resumable enrichment operations. Order: probe version,
    /// probe document, engine version, engine dependency roots; then the
    /// probe and engine task observations.
    pub(super) root_observations: [PlanningObservation; 4],
    pub(super) probe_task_observation: PlanningObservation,
    pub(super) engine_task_observation: PlanningObservation,
    /// One durable settlement operation ID for the whole P role.
    pub(super) settlement_operation_id: Uuid,
    pub(super) unknown: bool,
    pub(super) entered: bool,
    pub(super) never_entered:
        Option<crate::adaptive_resources::physical_graph::fixed_probe::FixedPlanningNoTaskGraph>,
}

/// Settlement-relevant summary of the original P tasks. Joined native owners
/// or strict retained never-entered inputs may close; uncertainty stays held.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanningNativeState {
    NotEntered,
    /// A handle or unconsumed report remains; joining may still finish it.
    Pending,
    /// An unknown sibling or failed native owner; P stays charged.
    Uncertain,
    /// Both original closures and their audit observations are retained.
    Joined,
    /// Entered and nothing pending, but a closure is missing (bookkeeping
    /// defect). Refused without a durable hold.
    Incomplete,
}
impl PlanningTasks {
    /// The retained pure inputs, rather than a task status flag, prove that
    /// neither original collector was handed its source/image/native work.
    pub(super) fn never_entered_members(&self) -> Result<[Uuid; 6], OperationError> {
        if self.entered
            || self.unknown
            || self.probe.is_some()
            || self.engine.is_some()
            || self.probe_failure.is_some()
            || self.engine_failure.is_some()
            || self.probe_report.is_some()
            || self.engine_report.is_some()
            || self.probe_closed.is_some()
            || self.engine_closed.is_some()
            || self.probe_observed.is_some()
            || self.engine_observed.is_some()
            || self.source.is_some()
            || self.observed_source.is_some()
            || self
                .root_observations
                .iter()
                .any(|observation| observation.committed)
            || self.probe_task_observation.committed
            || self.engine_task_observation.committed
            || (self.never_entered.is_none() != self.inputs.is_some())
        {
            return Err(OperationError::State);
        }
        Ok([
            self.probe_plan.version_root_member_id.uuid(),
            self.probe_plan.document_root_member_id.uuid(),
            self.probe_plan.member_id.uuid(),
            self.engine_plan.version_root_member_id.uuid(),
            self.engine_plan.dependency_root_member_id.uuid(),
            self.engine_plan.member_id.uuid(),
        ])
    }
    pub(super) fn consume_never_entered_inputs(&mut self) {
        // The graph witness is already retained; this pure request cannot be
        // moved to an original task after its acquisition entry was sealed.
        self.inputs = None;
    }
    pub(super) fn source_spec_retained(&self) -> bool {
        self.source_spec.is_some()
    }
    pub(super) fn native_state(&self) -> PlanningNativeState {
        if self.unknown || self.probe_failure.is_some() || self.engine_failure.is_some() {
            PlanningNativeState::Uncertain
        } else if !self.entered {
            PlanningNativeState::NotEntered
        } else if self.probe.is_some()
            || self.engine.is_some()
            || self.probe_report.is_some()
            || self.engine_report.is_some()
            || self.source.is_some()
        {
            PlanningNativeState::Pending
        } else if self.probe_closed.is_some()
            && self.engine_closed.is_some()
            && self.probe_observed.is_some()
            && self.engine_observed.is_some()
        {
            PlanningNativeState::Joined
        } else {
            PlanningNativeState::Incomplete
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct PlanningObservation {
    pub(super) member: Uuid,
    pub(super) operation_id: Uuid,
    pub(super) committed: bool,
}
impl PlanningObservation {
    fn new(member: Uuid) -> Self {
        Self {
            member,
            operation_id: Uuid::new_v4(),
            committed: false,
        }
    }
}

struct ProbeTaskInput {
    source: source_files::RetainedSourceLease,
    observed: String,
    request: FixedPlanningProbeRequest,
    version_close: Uuid,
    document_close: Uuid,
    deadline: Instant,
}
async fn original_probe_task(
    input: Option<ProbeTaskInput>,
    start: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<AdmittedProbeCompletion, AdmittedProbeNativeFailure> {
    let input =
        input.ok_or_else(|| AdmittedProbeNativeFailure::before_entry(DecodeFactError::Deadline))?;
    start
        .ok_or_else(|| AdmittedProbeNativeFailure::before_entry(DecodeFactError::Deadline))?
        .await
        .map_err(|_| AdmittedProbeNativeFailure::before_entry(DecodeFactError::Deadline))?;
    let FixedPlanningProbeRequest {
        probe_program_locator,
        cache,
        catalog,
        selected_stream,
        work,
        ..
    } = input.request;
    planning_collector::run_fixed_probe(
        input.source,
        input.observed,
        probe_program_locator,
        cache,
        catalog,
        selected_stream,
        work,
        input.deadline,
        input.version_close,
        input.document_close,
    )
    .await
}
async fn original_engine_task(
    input: Option<(v2::FixedPlanningEngineTaskV1, Option<Uuid>, Instant)>,
    start: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<FixedEngineNativeCompletion, FixedEngineNativeFailure> {
    let (plan, boot, deadline) =
        input.ok_or_else(|| std::io::Error::other("missing original engine input"))?;
    start
        .ok_or_else(|| std::io::Error::other("missing original engine start"))?
        .await
        .map_err(|_| std::io::Error::other("original engine start dropped"))?;
    #[cfg(test)]
    tests::stages::before_engine_native(plan.member_id.uuid()).await;
    #[cfg(any(test, feature = "fixtures"))]
    let original_version_close = plan.version_close_operation_id.uuid();
    let result = planning_collector::run_fixed_engine(plan, boot, deadline).await;
    // A genuine JoinError/lost result AFTER the actual collector returns.
    // This does not inject a kernel wait failure or construct any close proof.
    #[cfg(any(test, feature = "fixtures"))]
    if result.is_ok() {
        crate::process::fixed_planning_engine::fixtures::panic_after_native_return(
            original_version_close,
        );
    }
    result
}

pub(in crate::adaptive_resources::runtime) fn planning_task_frame_bytes() -> Option<usize> {
    // Constructing an unpolled future does no native work or heap allocation.
    // Option::None has the SAME compiler-generated future layout as Some.
    std::mem::size_of::<PlanningTasks>()
        .checked_add(std::mem::size_of_val(&original_probe_task(None, None)))?
        .checked_add(std::mem::size_of_val(&original_engine_task(None, None)))
}

pub(in crate::adaptive_resources::runtime) type PlanningOwnerFences = (
    Arc<super::super::planning_probe::MetadataReleaseState>,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<std::sync::atomic::AtomicBool>,
);

impl NodeResourceRuntime {
    pub(super) fn retain_planning_uncertainty(
        &self,
        binding: ReservationBinding,
    ) -> Result<(), OperationError> {
        let mut ledger = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        let entry = ledger
            .entries
            .get_mut(&binding.reservation)
            .filter(|entry| entry.binding == binding)
            .ok_or(OperationError::State)?;
        // Preserve the full original charge and fence new graph acquisition.
        // Registered sibling tasks still retain their own prearmed custody.
        entry.state = ReservationState::UnknownHeld;
        Ok(())
    }

    pub(in crate::adaptive_resources::runtime) fn read_planning_ownership_diagnostics(
        &self,
    ) -> Option<super::super::PlanningOwnershipDiagnostics> {
        let book = self.operations.try_lock().ok()?;
        // Bound an operator HTTP read independently of playback. A partial
        // scan must never look like a complete inventory with zero owners.
        if book.bundles.len() > 256 {
            return None;
        }
        let mut counts = super::super::PlanningOwnershipDiagnostics::default();
        for state in book.bundles.values() {
            let bundle = state.try_lock().ok()?;
            let Some(metadata) = bundle
                .roles
                .get(1)
                .and_then(|role| role.planning_metadata.as_ref())
            else {
                continue;
            };
            counts.metadata_owners = counts.metadata_owners.checked_add(1)?;
            counts.cleanup_requested = counts.cleanup_requested.checked_add(usize::from(
                metadata
                    .release
                    .cleanup_requested
                    .load(std::sync::atomic::Ordering::Acquire),
            ))?;
            let native = bundle.roles.first()?;
            counts.native_roles_held = counts
                .native_roles_held
                .checked_add(usize::from(!native.refunded))?;
            if let Some(tasks) = native.planning_tasks.as_ref() {
                counts.original_tasks_pending = counts
                    .original_tasks_pending
                    .checked_add(usize::from(tasks.probe.is_some()))?
                    .checked_add(usize::from(tasks.engine.is_some()))?;
                counts.uncertain_native_owners =
                    counts.uncertain_native_owners.checked_add(usize::from(
                        native.phase == Phase::UnknownHeld
                            || tasks.unknown
                            || tasks.probe_failure.is_some()
                            || tasks.engine_failure.is_some(),
                    ))?;
            }
        }
        Some(counts)
    }
}

/// The same pure original inputs and pre-minted operations, moved into P
/// before exposure. This is not a task, native receipt or entry permission.
#[repr(transparent)]
pub(in crate::adaptive_resources::runtime) struct PreparedPlanningTasks(PlanningTasks);
impl PreparedPlanningTasks {
    pub(in crate::adaptive_resources::runtime) fn new(
        planned: super::super::planning_probe::plan::PlanningTaskPlan,
        request: FixedPlanningProbeRequest,
    ) -> Self {
        let probe = &planned.probe_plan;
        let engine = &planned.engine_plan;
        let root_observations = [
            PlanningObservation::new(probe.version_root_member_id.uuid()),
            PlanningObservation::new(probe.document_root_member_id.uuid()),
            PlanningObservation::new(engine.version_root_member_id.uuid()),
            PlanningObservation::new(engine.dependency_root_member_id.uuid()),
        ];
        let probe_task_observation = PlanningObservation::new(probe.member_id.uuid());
        let engine_task_observation = PlanningObservation::new(engine.member_id.uuid());
        Self(PlanningTasks {
            probe_observed: None,
            engine_observed: None,
            root_observations,
            probe_task_observation,
            engine_task_observation,
            settlement_operation_id: Uuid::new_v4(),
            probe_plan: planned.probe_plan,
            engine_plan: planned.engine_plan,
            source_member: planned.source_member,
            source_spec: Some(planned.source_spec),
            inputs: Some(request),
            source: None,
            observed_source: None,
            probe: None,
            engine: None,
            engine_failure: None,
            probe_failure: None,
            probe_report: None,
            engine_report: None,
            probe_closed: None,
            engine_closed: None,
            unknown: false,
            entered: false,
            never_entered: None,
        })
    }
    pub(super) fn install(self, role: &mut OwnedRole) {
        assert!(
            !role.refunded && role.planning_tasks.is_none(),
            "fresh private P slot"
        );
        role.planning_tasks = Some(self.0);
    }
}

impl NodeResourceRuntime {
    #[cfg(test)]
    pub(in crate::adaptive_resources::runtime) fn planning_constructor_checkpoint(
        &self,
        owner: &OwnedPreparation,
    ) {
        tests::construction::after_private_install(owner);
    }

    pub(in crate::adaptive_resources::runtime) async fn prepare_planning_inputs(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.resolve_preparation(owner, deadline).await?;
        let (member, mut spec) = {
            let mut bundle = owner.state.lock().await;
            let task = bundle
                .roles
                .get_mut(0)
                .and_then(|role| role.planning_tasks.as_mut())
                .ok_or(OperationError::State)?;
            if task.never_entered.is_some() {
                return Err(OperationError::State);
            }
            (task.source_member, task.source_spec.take())
        };
        // There is no cancellation point between removing the pending spec,
        // registering its original SourceCell, and restoring a rejected spec.
        if spec.is_some() {
            let result = self.queue_source_open(owner, 0, member, &mut spec);
            let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
            bundle.roles[0]
                .planning_tasks
                .as_mut()
                .ok_or(OperationError::State)?
                .source_spec = spec;
            result?;
        }
        self.acquire_prepared_source(owner, 0, member, deadline)
            .await
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub(in crate::adaptive_resources::runtime) async fn fixture_planning_native_gate(
        &self,
        owner: &OwnedPreparation,
        panic_after_native_return: bool,
    ) -> Result<
        crate::process::fixed_planning_engine::fixtures::FixedPlanningNativeGate,
        OperationError,
    > {
        self.validate_preparation(owner)?;
        let bundle = owner.state.lock().await;
        // Registration alone is sufficient for this diagnostic hook. Durable
        // preparation/source entry still occurs through the original APIs.
        if bundle.roles.len() != 2 || bundle.roles[0].refunded {
            return Err(OperationError::State);
        }
        let task = bundle.roles[0]
            .planning_tasks
            .as_ref()
            .ok_or(OperationError::State)?;
        if task.entered || task.unknown || task.never_entered.is_some() {
            return Err(OperationError::State);
        }
        crate::process::fixed_planning_engine::fixtures::FixedPlanningNativeGate::install(
            task.engine_plan.version_close_operation_id.uuid(),
            task.engine_plan.member_id.uuid(),
            panic_after_native_return,
        )
        .ok_or(OperationError::State)
    }

    pub(in crate::adaptive_resources::runtime) async fn collect_planning_tasks(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<super::super::planning_probe::FixedProbeMetadataLease, OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = tokio::time::timeout_at(deadline.into(), owner.state.lock())
            .await
            .map_err(|_| OperationError::Deadline)?;
        if !bundle.installed || bundle.roles.len() != 2 {
            return Err(OperationError::State);
        }
        // Epoch/drain/overage gates new entry, never the join of tasks that
        // this SAME original P already registered and may have released.
        let registered = bundle.roles[0]
            .planning_tasks
            .as_ref()
            .ok_or(OperationError::State)?
            .entered;
        if !registered {
            self.eligible(&bundle)?;
        }
        let mut hold_error = None;
        let bundle_id = bundle.id;
        let cleanup_requested = bundle.roles[1]
            .planning_metadata
            .as_ref()
            .ok_or(OperationError::State)?
            .release
            .cleanup_requested
            .load(std::sync::atomic::Ordering::Acquire);
        let role = &mut bundle.roles[0];
        let task = role.planning_tasks.as_ref().ok_or(OperationError::State)?;
        if task.never_entered.is_some() || (task.unknown || cleanup_requested) && !task.entered {
            return Err(OperationError::State);
        }
        let members = [
            task.probe_plan.version_root_member_id.uuid(),
            task.probe_plan.document_root_member_id.uuid(),
            task.probe_plan.member_id.uuid(),
            task.engine_plan.version_root_member_id.uuid(),
            task.engine_plan.dependency_root_member_id.uuid(),
            task.engine_plan.member_id.uuid(),
        ];
        for member in members {
            if !role.armed.contains(&member) {
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
                } else if role
                    .transition
                    .as_ref()
                    .is_none_or(|t| t.effect != Effect::Arm(member))
                {
                    return Err(OperationError::State);
                }
                self.finish_transition(bundle_id, role).await?;
            }
        }
        if Instant::now() >= deadline {
            return Err(OperationError::Deadline);
        }
        let binding = role.binding;
        let task = role.planning_tasks.as_mut().ok_or(OperationError::State)?;
        if !task.entered {
            let source = role
                .sources
                .get_mut(&task.source_member)
                .ok_or(OperationError::State)?;
            let (source_lease, _, observed) = source.take_probe_lease()?;
            let request = task.inputs.take().ok_or(OperationError::State)?;
            task.source = Some(FixedPlanningSourceRequest {
                path: request.source.path.clone(),
                media_file_id: request.source.media_file_id.clone(),
                scanner_size: request.source.scanner_size,
                scanner_mtime: request.source.scanner_mtime,
            });
            task.observed_source = Some(observed.clone());
            let (probe_start, probe_wait) = tokio::sync::oneshot::channel();
            let input = ProbeTaskInput {
                source: source_lease,
                observed,
                request,
                version_close: task.probe_plan.version_close_operation_id.uuid(),
                document_close: task.probe_plan.document_close_operation_id.uuid(),
                deadline,
            };
            task.probe = Some(tokio::spawn(original_probe_task(
                Some(input),
                Some(probe_wait),
            )));
            let boot = match &self.owner.os_boot {
                crate::resource_custody::BootIdentity::Known { value, .. } => Some(*value),
                _ => None,
            };
            let (engine_start, engine_wait) = tokio::sync::oneshot::channel();
            task.engine = Some(tokio::spawn(original_engine_task(
                Some((task.engine_plan.clone(), boot, deadline)),
                Some(engine_wait),
            )));
            task.entered = true;
            // Both handles are now original retained book members. Neither
            // source, process, image nor parser can enter before this point.
            #[cfg(test)]
            tests::stages::before_registered_entry(task.engine_plan.member_id.uuid()).await;
            let probe_released = self.release_native_entry(binding, probe_start).is_ok();
            #[cfg(test)]
            tests::stages::between_registered_entries(task.engine_plan.member_id.uuid()).await;
            let engine_released = self.release_native_entry(binding, engine_start).is_ok();
            if !probe_released || !engine_released {
                // Each failed release drops only its own unopened sender.
                // An earlier sibling may already own native work: retain and
                // join BOTH original handles below before reporting refusal.
                task.unknown = true;
                hold_error = self
                    .retain_planning_uncertainty(binding)
                    .err()
                    .or(hold_error);
            }
        }
        if let Some(original) = task.probe.as_mut() {
            let joined = tokio::time::timeout_at(deadline.into(), original)
                .await
                .map_err(|_| OperationError::Deadline)?;
            task.probe = None;
            let native = match joined {
                Ok(Ok(native)) => Some(native),
                Ok(Err(failure)) => {
                    #[cfg(all(test, feature = "fixtures"))]
                    eprintln!(
                        "original planning probe native failure: {:?}",
                        failure.error
                    );
                    tracing::warn!(reason = ?failure.error.fallback_reason(), "original planning probe failed; retaining native owners for cleanup");
                    task.probe_failure = Some(failure);
                    task.unknown = true;
                    hold_error = self
                        .retain_planning_uncertainty(binding)
                        .err()
                        .or(hold_error);
                    None
                }
                _ => {
                    task.unknown = true;
                    hold_error = self
                        .retain_planning_uncertainty(binding)
                        .err()
                        .or(hold_error);
                    None
                }
            };
            if let Some(native) = native {
                if task.probe_plan.source_member_id.uuid() != task.source_member {
                    return Err(OperationError::State);
                }
                let known = FixedProbeKnownClosed::from_joined_original(
                    native,
                    &task.probe_plan,
                    task.observed_source
                        .as_deref()
                        .ok_or(OperationError::State)?,
                )
                .inspect_err(|error| {
                    #[cfg(all(test, feature = "fixtures"))]
                    eprintln!("original planning probe completion proof refused: {error:?}");
                    #[cfg(not(all(test, feature = "fixtures")))]
                    let _ = error;
                })?;
                let (report, closure) = known.split();
                task.probe_observed = Some(v2::FixedProbeDiscoverTaskObservedV1 {
                    task: task.probe_plan.clone(),
                    executable_object_version: report.executable_object_version.clone(),
                    executable_sha256: report.executable_sha256.clone(),
                    build_digest: report.build_digest.clone(),
                });
                task.probe_report = Some(report);
                task.probe_closed = Some(closure);
                #[cfg(test)]
                tests::stages::probe_joined(task.engine_plan.member_id.uuid());
            }
        }
        if let Some(original) = task.engine.as_mut() {
            // Refuse an unencodable audit plan before awaiting/taking the
            // SAME original task. Even an impossible admitted-plan defect
            // must leave its JoinHandle and native custody installed.
            let plan_sha256 = v2::engine_task_plan_sha256(&task.engine_plan)?;
            let joined = tokio::time::timeout_at(deadline.into(), original)
                .await
                .map_err(|_| OperationError::Deadline)?;
            task.engine = None;
            let native = match joined {
                Ok(Ok(native)) => Some(native),
                Ok(Err(failure)) => {
                    #[cfg(all(test, feature = "fixtures"))]
                    eprintln!(
                        "original planning engine native failure: phase={:?}, error={:?}",
                        failure.phase, failure.error
                    );
                    task.engine_failure = Some(failure);
                    task.unknown = true;
                    hold_error = self
                        .retain_planning_uncertainty(binding)
                        .err()
                        .or(hold_error);
                    None
                }
                _ => {
                    task.unknown = true;
                    hold_error = self
                        .retain_planning_uncertainty(binding)
                        .err()
                        .or(hold_error);
                    None
                }
            };
            if let Some(native) = native {
                let known =
                    FixedEngineKnownClosed::from_joined_original(native, task.engine_plan.clone())?;
                let (report, closure) = known.split();
                task.engine_observed = Some(v2::FixedPlanningEngineTaskObservedV2 {
                    member_id: task.engine_plan.member_id,
                    plan_sha256,
                    used_file_slots: u16::try_from(closure.used_file_slots())
                        .map_err(|_| OperationError::State)?,
                    media_digest: closure.media_digest().to_owned(),
                });
                task.engine_report = Some(report);
                task.engine_closed = Some(closure);
            }
        }
        // An uncertain sibling cannot justify discarding or replacing the
        // other original task. Join any retained handles above, but never
        // turn partial completion into a successful planning report.
        if task.unknown {
            return Err(hold_error.unwrap_or(OperationError::State));
        }
        let probe = task.probe_report.take().ok_or(OperationError::State)?;
        let engine = task.engine_report.take().ok_or(OperationError::State)?;
        let source = task.source.take().ok_or(OperationError::State)?;
        let metadata = bundle.roles[1]
            .planning_metadata
            .as_mut()
            .ok_or(OperationError::State)?;
        metadata.install(probe, engine, source)
    }

    async fn retry_planning_native_failures(
        &self,
        owner: &OwnedPreparation,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = tokio::time::timeout_at(deadline.into(), owner.state.lock())
            .await
            .map_err(|_| OperationError::Deadline)?;
        let Some(tasks) = bundle
            .roles
            .first_mut()
            .and_then(|role| role.planning_tasks.as_mut())
        else {
            return Ok(());
        };
        // Both futures borrow the original stored owners. Joining them here
        // launches no new job and prevents one permanently uncertain sibling
        // from starving the other's actual cleanup on every maintenance pass.
        // Successful cleanup retains the charge and private close evidence.
        match (tasks.engine_failure.as_mut(), tasks.probe_failure.as_mut()) {
            (Some(engine), Some(probe)) => {
                let (engine, probe) = tokio::join!(
                    engine.retry_cleanup(deadline),
                    probe.retry_cleanup(deadline)
                );
                engine.and(probe)?;
            }
            (Some(engine), None) => engine.retry_cleanup(deadline).await?,
            (None, Some(probe)) => probe.retry_cleanup(deadline).await?,
            (None, None) => {}
        }
        Ok(())
    }

    /// Called from the existing transcode maintenance pass. This never enters
    /// new native work. The cursor avoids one unknown owner starving others.
    pub async fn maintain_planning_cleanup(&self, deadline: Instant) {
        let mut first = None;
        for _ in 0..8 {
            if Instant::now() >= deadline {
                break;
            }
            let selected = (|| {
                let mut book = self.operations.lock().ok()?;
                let eligible = |(_, state): &(&Uuid, &Arc<AsyncMutex<Bundle>>)| {
                    state.try_lock().ok().is_some_and(|bundle| {
                        bundle
                            .roles
                            .get(1)
                            .and_then(|role| role.planning_metadata.as_ref())
                            .is_some_and(|metadata| {
                                metadata
                                    .release
                                    .cleanup_requested
                                    .load(std::sync::atomic::Ordering::Acquire)
                            })
                    })
                };
                let selected = book
                    .bundles
                    .iter()
                    .filter(|(id, _)| book.planning_cursor.is_none_or(|cursor| **id > cursor))
                    .find(eligible)
                    .or_else(|| book.bundles.iter().find(eligible))
                    .map(|(id, state)| (*id, Arc::clone(state)));
                if let Some((id, _)) = selected.as_ref() {
                    book.planning_cursor = Some(*id);
                }
                selected
            })();
            let Some((id, state)) = selected else {
                break;
            };
            if first == Some(id) {
                break;
            }
            first.get_or_insert(id);
            let mut selection = Some(OwnedPreparation {
                ledger: self.ledger.id,
                id,
                state,
            });
            let owner = selection.as_ref().expect("selected original preparation");
            let known_closed = {
                let Ok(bundle) = tokio::time::timeout_at(deadline.into(), owner.state.lock()).await
                else {
                    continue;
                };
                let Some(metadata) = bundle
                    .roles
                    .get(1)
                    .and_then(|role| role.planning_metadata.as_ref())
                else {
                    continue;
                };
                Arc::clone(&metadata.release.known_closed)
            };
            // Original tasks keep their own native deadline. Joining them
            // here does not launch, abort or replace a job after cancellation.
            let _ = self.collect_planning_tasks(owner, deadline).await;
            let _ = self.retry_planning_native_failures(owner, deadline).await;
            if self.finish_planning_native(owner, deadline).await.is_err()
                || self
                    .queue_planning_metadata_close(owner, deadline)
                    .await
                    .is_err()
            {
                continue;
            }
            if self
                .finish_planning_metadata_close(&mut selection, 1, deadline)
                .await
                .is_ok()
            {
                known_closed.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
