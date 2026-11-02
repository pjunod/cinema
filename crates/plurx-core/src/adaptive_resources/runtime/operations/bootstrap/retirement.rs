//! Durable V3 retirement over the original retained S native owners. Every
//! destructive step has one retained Arm/Observe identity and one native cell.
use super::*;
use native::cleanup::{CleanupCell, CleanupFact};
use v2::scratch::{
    AssetEventRefV1, AssetIoV1, AssetObservationV1, NativeOutcomeV1, OwnedDirectoryRefV1,
};

/// Consumed once, under the retained bundle lock, by the outer typed
/// settlement. Neither the folded DTO nor a pathname can construct this.
pub(in crate::adaptive_resources) struct RetainedFiniteClosure {
    binding: ReservationBinding,
    member: Uuid,
    head: AssetEventRefV1,
}
/// Producer-R output closure after its own Open→Retiring cancellation event.
/// This is native-prefix closure, not application/response destruction or
/// refund authority. Copy consumes it only after its separate final barriers.
/// This cannot be consumed as a retained-S close receipt.
pub(in crate::adaptive_resources) struct ProducerFiniteClosure {
    binding: ReservationBinding,
    member: Uuid,
    head: AssetEventRefV1,
}
impl ProducerFiniteClosure {
    pub(in crate::adaptive_resources) fn binding(&self) -> ReservationBinding {
        self.binding
    }
    pub(in crate::adaptive_resources) fn member(&self) -> Uuid {
        self.member
    }
    pub(in crate::adaptive_resources) fn head(&self) -> AssetEventRefV1 {
        self.head.clone()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupMode {
    Retained,
    Producer,
    // Selected only by the original Copy cleanup claim. Native prefix closure
    // precedes application/response destruction; final Copy preflight still
    // requires the original application and response budget to be empty.
    #[cfg(target_os = "linux")]
    CopyProducer,
}
impl CleanupMode {
    fn is_producer(self) -> bool {
        self != Self::Retained
    }
    fn requires_response_barrier(self) -> bool {
        #[cfg(target_os = "linux")]
        if self == Self::CopyProducer {
            return false;
        }
        true
    }
}
impl RetainedFiniteClosure {
    pub(in crate::adaptive_resources) fn binding(&self) -> ReservationBinding {
        self.binding
    }
    pub(in crate::adaptive_resources) fn head(&self) -> AssetEventRefV1 {
        self.head.clone()
    }
    pub(in crate::adaptive_resources) fn member(&self) -> Uuid {
        self.member
    }
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

#[derive(Clone)]
enum CleanupOperation {
    Output(AssetIoV1),
    Bootstrap(BootstrapIoV1),
}
struct CleanupIo {
    io: v2::Id,
    observe: v2::Id,
    join: v2::Id,
    operation: CleanupOperation,
    cell: CleanupCell,
    armed: bool,
    launched: bool,
    joined: bool,
}
#[allow(
    clippy::large_enum_variant,
    reason = "CleanupIo retains the exact original native cell and journal IDs inline in BootstrapMember; retained_bootstrap_node_bytes includes that layout, while boxing would introduce another allocation."
)]
enum CleanupPending {
    Begin { operation: v2::Id },
    Io(CleanupIo),
}
pub(super) struct CleanupRun {
    mode: CleanupMode,
    pending: Option<CleanupPending>,
    closed: Option<RetainedFiniteClosure>,
    producer_closed: Option<ProducerFiniteClosure>,
    issued: bool,
}
impl CleanupRun {
    #[cfg(all(
        test,
        feature = "fixtures",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(super) fn fixture_job_identity(
        &self,
    ) -> Option<(Uuid, Uuid, Uuid, Option<tokio::task::Id>, bool)> {
        let Some(CleanupPending::Io(io)) = self.pending.as_ref() else {
            return None;
        };
        Some((
            io.io.uuid(),
            io.observe.uuid(),
            io.join.uuid(),
            io.cell.fixture_job(),
            io.joined,
        ))
    }
    fn new(mode: CleanupMode) -> Self {
        Self {
            mode,
            pending: None,
            closed: None,
            producer_closed: None,
            issued: false,
        }
    }
    pub(super) fn pending_cell_mut(&mut self) -> Option<&mut CleanupCell> {
        match self.pending.as_mut() {
            Some(CleanupPending::Io(io)) => Some(&mut io.cell),
            _ => None,
        }
    }
}
fn id() -> Result<v2::Id, OperationError> {
    Ok(v2::Id::new(Uuid::new_v4())?)
}
fn io(operation: CleanupOperation, cell: CleanupCell) -> Result<CleanupPending, OperationError> {
    Ok(CleanupPending::Io(CleanupIo {
        io: id()?,
        observe: id()?,
        join: id()?,
        operation,
        cell,
        armed: false,
        launched: false,
        joined: false,
    }))
}
fn directory_ref(
    fold: &BootstrapFold,
    directory: &v2::scratch::DirectoryObservedV1,
) -> OwnedDirectoryRefV1 {
    OwnedDirectoryRefV1 {
        owning_obligation: fold.obligation_id(),
        member_id: fold.member_id(),
        directory_id: directory.directory_id,
    }
}
fn next_step(bootstrap: &BootstrapMember) -> Result<CleanupPending, OperationError> {
    let fold = bootstrap.fold.as_ref().ok_or(OperationError::State)?;
    let state = fold.state();
    let mode = bootstrap
        .retirement
        .as_ref()
        .ok_or(OperationError::State)?
        .mode;
    if (mode == CleanupMode::Retained
        && state.asset.lifecycle == ScratchLifecycleV1::RetainedReadOnly)
        || (mode.is_producer()
            && state.asset.lifecycle == ScratchLifecycleV1::Open
            && (matches!(
                state.bootstrap.phase,
                BootstrapPhase::Traversing
                    | BootstrapPhase::ParentReady
                    | BootstrapPhase::SinkReady
            ) || (state.bootstrap.phase == BootstrapPhase::Unstarted
                && bootstrap.root_arm.is_some()
                && bootstrap
                    .native_root
                    .as_ref()
                    .is_some_and(|root| root.initial_root_never_entered()))))
    {
        return Ok(CleanupPending::Begin { operation: id()? });
    }
    if state.asset.lifecycle != ScratchLifecycleV1::Retiring {
        return Err(OperationError::State);
    }
    if mode.is_producer()
        && matches!(
            state.bootstrap.phase,
            BootstrapPhase::Traversing | BootstrapPhase::ParentReady
        )
        && state.asset.directory.is_none()
        && fold.files().is_empty()
        && state.bootstrap.possible_child.is_none()
    {
        // The original traversal cell owns exactly this observed directory.
        // No child name was entered, so a descriptor-only close suffices.
        let observed = state
            .bootstrap
            .current
            .iter()
            .chain(state.bootstrap.parent.iter())
            .map(|directory| directory.observed.clone())
            .collect::<Vec<_>>();
        let mut copy_ids = observed
            .iter()
            .flat_map(|directory| directory.held_copy_ids.iter().copied())
            .collect::<Vec<_>>();
        copy_ids.sort_unstable();
        return io(
            CleanupOperation::Bootstrap(BootstrapIoV1::CloseBootstrapHandles {
                copy_ids: copy_ids.clone(),
            }),
            CleanupCell::close_retained(&observed, &copy_ids)?,
        );
    }
    let child = state
        .asset
        .directory
        .as_ref()
        .ok_or(OperationError::State)?;
    if let Some(file) = fold.files().first() {
        return io(
            CleanupOperation::Output(AssetIoV1::RemoveOwned {
                file_id: file.file_id,
                expected_native_object: Some(file.native_object.clone()),
            }),
            CleanupCell::remove_owned_file(file, child)?,
        );
    }
    if state.bootstrap.phase == BootstrapPhase::SinkReady && !state.asset.directory_synced {
        return io(
            CleanupOperation::Output(AssetIoV1::SyncFiles { file_ids: vec![] }),
            CleanupCell::sync_child(child)?,
        );
    }
    let parent = &state
        .bootstrap
        .parent
        .as_ref()
        .ok_or(OperationError::State)?
        .observed;
    let parent_ref = directory_ref(fold, parent);
    match state.bootstrap.phase {
        BootstrapPhase::SinkReady => io(
            CleanupOperation::Bootstrap(BootstrapIoV1::RemoveRenditionChild {
                parent: parent_ref,
                child_directory_id: child.directory_id,
                child_component: bootstrap.plan.bootstrap().child_component.clone(),
            }),
            CleanupCell::remove_child(&bootstrap.plan, parent, child)?,
        ),
        BootstrapPhase::RetainedChildRemoved if !state.bootstrap.retained_parent_synced => io(
            CleanupOperation::Bootstrap(BootstrapIoV1::SyncRetainedParent { parent: parent_ref }),
            CleanupCell::sync_parent(parent)?,
        ),
        BootstrapPhase::RetainedChildRemoved => {
            let observed = [parent.clone(), child.clone()];
            let mut copy_ids = observed
                .iter()
                .flat_map(|directory| directory.held_copy_ids.iter().copied())
                .collect::<Vec<_>>();
            copy_ids.sort_unstable();
            io(
                CleanupOperation::Bootstrap(BootstrapIoV1::CloseRetainedHandles {
                    copy_ids: copy_ids.clone(),
                }),
                CleanupCell::close_retained(&observed, &copy_ids)?,
            )
        }
        _ => Err(OperationError::State),
    }
}
fn queue_bootstrap_change(
    journal: &CustodyJournal,
    role: &mut OwnedRole,
    member: Uuid,
    operation_id: v2::Id,
    event: AssetEventV3,
    deadline: Instant,
) -> Result<(), OperationError> {
    let bootstrap = role
        .bootstrap
        .get_mut(&member)
        .ok_or(OperationError::State)?;
    if bootstrap.change.is_some() || role.transition.is_some() {
        return Err(OperationError::State);
    }
    let change = AssetChangeV3 {
        scratch_member_id: v2::Id::new(member)?,
        asset_id: bootstrap.plan.asset_id,
        generation_epoch: bootstrap.plan.generation_epoch,
        previous: bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .head,
        event,
    };
    bootstrap.change = Some((operation_id, change));
    resume_change(journal, role, member, deadline)
}
fn retry_transition(
    role: &mut OwnedRole,
    journal: &CustodyJournal,
    member: Uuid,
    deadline: Instant,
) -> Result<(), OperationError> {
    if let Some(transition) = role.transition.as_mut() {
        if transition.effect != Effect::Bootstrap(member) {
            return Err(OperationError::State);
        }
        if transition.operation.pending.is_none()
            && matches!(transition.operation.result, Some(Err(_)))
        {
            transition.operation.retry(journal, deadline)?;
        }
    }
    Ok(())
}

impl BootstrapMember {
    pub(in crate::adaptive_resources::runtime::operations) fn response_allocation_ready(
        &self,
    ) -> bool {
        self.change.is_none()
            && self.retirement.is_none()
            && self.fold.as_ref().is_some_and(|fold| {
                matches!(
                    fold.state().asset.lifecycle,
                    ScratchLifecycleV1::Open | ScratchLifecycleV1::RetainedReadOnly
                )
            })
    }

    /// Read-only prebuild fact. The one-shot native capability remains in this
    /// member until the outer settlement is completely validated.
    pub(in crate::adaptive_resources::runtime) fn retained_close_fact(
        &self,
        expected_binding: ReservationBinding,
        expected_member: Uuid,
    ) -> Result<(ScratchPlanV3, AssetEventRefV1), OperationError> {
        let run = self.retirement.as_ref().ok_or(OperationError::State)?;
        let closed = run.closed.as_ref().ok_or(OperationError::State)?;
        if closed.binding() != expected_binding
            || closed.member() != expected_member
            || run.issued
            || run.pending.is_some()
            || self.change.is_some()
            || self.next_arm.is_some()
            || !self.readers.is_empty()
        {
            return Err(OperationError::State);
        }
        let state = self.fold.as_ref().ok_or(OperationError::State)?.state();
        if state.asset.lifecycle != ScratchLifecycleV1::RetiredClosed
            || state.asset.head != Some(closed.head())
        {
            return Err(OperationError::State);
        }
        Ok((self.plan.clone(), closed.head()))
    }
    /// Synchronous one-shot move under the SAME role bundle lock. No journal
    /// or ledger fallible work may occur between this take and receipt custody.
    pub(in crate::adaptive_resources::runtime) fn take_retained_close(
        &mut self,
        expected_binding: ReservationBinding,
        expected_member: Uuid,
    ) -> Result<RetainedFiniteClosure, OperationError> {
        self.retained_close_fact(expected_binding, expected_member)?;
        let run = self.retirement.as_mut().ok_or(OperationError::State)?;
        run.issued = true;
        run.closed.take().ok_or(OperationError::State)
    }

    pub(in crate::adaptive_resources::runtime) fn producer_close_fact(
        &self,
        expected_binding: ReservationBinding,
        expected_member: Uuid,
    ) -> Result<(ScratchPlanV3, AssetEventRefV1), OperationError> {
        let run = self.retirement.as_ref().ok_or(OperationError::State)?;
        let closed = run.producer_closed.as_ref().ok_or(OperationError::State)?;
        if !run.mode.is_producer()
            || closed.binding() != expected_binding
            || closed.member() != expected_member
            || run.issued
            || run.pending.is_some()
            || self.change.is_some()
            || self.next_arm.is_some()
            || !self.readers.is_empty()
        {
            return Err(OperationError::State);
        }
        let state = self.fold.as_ref().ok_or(OperationError::State)?.state();
        if state.asset.lifecycle != ScratchLifecycleV1::RetiredClosed
            || state.asset.head != Some(closed.head())
        {
            return Err(OperationError::State);
        }
        Ok((self.plan.clone(), closed.head()))
    }
    pub(in crate::adaptive_resources::runtime) fn take_producer_close(
        &mut self,
        expected_binding: ReservationBinding,
        expected_member: Uuid,
    ) -> Result<ProducerFiniteClosure, OperationError> {
        self.producer_close_fact(expected_binding, expected_member)?;
        let run = self.retirement.as_mut().ok_or(OperationError::State)?;
        run.issued = true;
        run.producer_closed.take().ok_or(OperationError::State)
    }
}

#[cfg(target_os = "linux")]
fn validate_copy_cleanup_claim(
    role: &OwnedRole,
    mode: CleanupMode,
    claim: Option<&crate::adaptive_resources::runtime::copy_vod::CopyCloseClaim>,
) -> Result<(), OperationError> {
    if mode == CleanupMode::CopyProducer {
        if !claim.is_some_and(|claim| {
            role.copy_cleanup
                .as_ref()
                .is_some_and(|custody| claim.matches(custody))
        }) || role.copy_vod.as_ref().is_none_or(|copy| !copy.closing)
            || role.finite_vod.is_some()
            || role.retirement.is_some()
            || role.refunded
        {
            return Err(OperationError::State);
        }
    } else if claim.is_some() {
        return Err(OperationError::State);
    }
    Ok(())
}

impl NodeResourceRuntime {
    /// Run the exact V3 S cleanup lineage. The same member-owned run and native
    /// cell survive cancellation and uncertain journal waits.
    pub(in crate::adaptive_resources::runtime) async fn close_retained_finite_native(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.close_finite_native(
            owner,
            index,
            member,
            deadline,
            CleanupMode::Retained,
            #[cfg(target_os = "linux")]
            None,
        )
        .await
    }

    pub(in crate::adaptive_resources::runtime) async fn close_producer_finite_native(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.close_finite_native(
            owner,
            index,
            member,
            deadline,
            CleanupMode::Producer,
            #[cfg(target_os = "linux")]
            None,
        )
        .await
    }

    /// Native Copy prefix closure under the SAME exclusive cleanup claim.
    /// This grants no response destruction, typed settlement or refund fact.
    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) async fn close_copy_producer_finite_native(
        &self,
        owner: &OwnedPreparation,
        claim: &crate::adaptive_resources::runtime::copy_vod::CopyCloseClaim,
        index: usize,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.close_finite_native(
            owner,
            index,
            member,
            deadline,
            CleanupMode::CopyProducer,
            Some(claim),
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The Linux Copy path must borrow its exact original cleanup claim separately from the retained native member and deadline; no caller boolean or replacement owner can authorize this native-only stage."
    )]
    async fn close_finite_native(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        member: Uuid,
        deadline: Instant,
        mode: CleanupMode,
        #[cfg(target_os = "linux")] copy_claim: Option<
            &crate::adaptive_resources::runtime::copy_vod::CopyCloseClaim,
        >,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        #[cfg(target_os = "linux")]
        {
            let bundle = owner.state.lock().await;
            validate_copy_cleanup_claim(
                bundle.roles.get(index).ok_or(OperationError::State)?,
                mode,
                copy_claim,
            )?;
        }
        self.close_dropped_finite_readers(owner, index, deadline)
            .await?;
        loop {
            if Instant::now() >= deadline {
                return Err(OperationError::Deadline);
            }
            let mut bundle = owner.state.lock().await;
            let bundle_id = bundle.id;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            #[cfg(target_os = "linux")]
            validate_copy_cleanup_claim(role, mode, copy_claim)?;
            #[cfg(target_os = "linux")]
            let recovered_producer = role.finite_vod.is_none()
                && role
                    .handle
                    .as_ref()
                    .and_then(|handle| handle.producer_recovery_authority())
                    .is_some();
            #[cfg(not(target_os = "linux"))]
            let recovered_producer = false;
            #[cfg(target_os = "linux")]
            let closed_copy_producer = role.copy_vod.as_ref().is_some_and(|copy| {
                let never_root = copy.never_root_closure.is_some();
                let root_only = copy.root_only_closure.is_some();
                let joined = copy.process_closure.is_some();
                // Each branch requires its own original graph witness.
                // A missing task/terminal alone is never nonentry proof.
                usize::from(never_root) + usize::from(root_only) + usize::from(joined) == 1
                    && copy.entered != never_root
                    && copy.task.is_some() == joined
                    && copy.terminal.is_some() == joined
            });
            #[cfg(not(target_os = "linux"))]
            let closed_copy_producer = false;
            if role.refunded
                || (mode == CleanupMode::Retained && role.retirement.is_none())
                || (mode.is_producer()
                    && (role.retirement.is_some()
                        || (!recovered_producer
                            && !closed_copy_producer
                            && role.finite_vod.as_ref().is_none_or(|finite| {
                                let never_root = finite.never_root_closure.is_some();
                                let root_only = finite.root_only_closure.is_some();
                                let joined = finite.process_closure.is_some();
                                !finite.retiring
                                    || usize::from(never_root)
                                        + usize::from(root_only)
                                        + usize::from(joined)
                                        != 1
                                    || finite.producer_joined != joined
                            }))))
            {
                return Err(OperationError::State);
            }
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            // An issued owner cannot disappear. Refuse before Begin/native
            // cleanup so restoring that same owner can resume this attempt.
            if !bootstrap.native_io_slots_consistent()
                || bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state()
                    .asset
                    .totals
                    .readers
                    != 0
            {
                return Err(OperationError::State);
            }
            readers::clear_terminal_readers(bootstrap)?;
            if bootstrap.retirement.is_none()
                && bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state()
                    .asset
                    .totals
                    .pending_jobs
                    != 0
            {
                return Err(OperationError::State);
            }
            if mode.requires_response_barrier() {
                role.response_budget
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .begin_close_if_empty()?;
            }
            if bootstrap.retirement.is_none() {
                // Install the owner before the first await, journal arm or
                // destructive native entry.
                bootstrap.retirement = Some(CleanupRun::new(mode));
            }
            let run = bootstrap.retirement.as_ref().ok_or(OperationError::State)?;
            if run.mode != mode {
                return Err(OperationError::State);
            }
            if (mode == CleanupMode::Retained && run.closed.is_some())
                || (mode.is_producer() && run.producer_closed.is_some())
            {
                return Ok(());
            }
            self.drive_cleanup(role, bundle_id, member, deadline)
                .await?;
        }
    }

    async fn drive_cleanup(
        &self,
        role: &mut OwnedRole,
        bundle_id: Uuid,
        member: Uuid,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        if bootstrap
            .retirement
            .as_ref()
            .ok_or(OperationError::State)?
            .pending
            .is_none()
        {
            let step = next_step(bootstrap)?;
            role.bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?
                .retirement
                .as_mut()
                .ok_or(OperationError::State)?
                .pending = Some(step);
        }
        let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
        let pending = bootstrap
            .retirement
            .as_ref()
            .ok_or(OperationError::State)?
            .pending
            .as_ref()
            .ok_or(OperationError::State)?;
        let (arm, observe, begin) = match pending {
            CleanupPending::Begin { operation } => (*operation, None, true),
            CleanupPending::Io(io) => (io.io, Some(io.observe), false),
        };
        if let Some(transition) = &role.transition {
            if transition.effect != Effect::Bootstrap(member)
                || (transition.operation.id != arm.uuid()
                    && Some(transition.operation.id) != observe.map(v2::Id::uuid))
            {
                return Err(OperationError::State);
            }
        }
        let head = bootstrap
            .fold
            .as_ref()
            .ok_or(OperationError::State)?
            .state()
            .asset
            .head;
        if begin {
            let empty_cancel = {
                let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
                let state = bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state();
                bootstrap
                    .retirement
                    .as_ref()
                    .is_some_and(|run| run.mode.is_producer())
                    && state.bootstrap.phase == BootstrapPhase::Unstarted
                    && bootstrap.root_arm.is_some()
                    && bootstrap
                        .native_root
                        .as_ref()
                        .is_some_and(|root| root.initial_root_never_entered())
            };
            let mode = role
                .bootstrap
                .get(&member)
                .and_then(|bootstrap| bootstrap.retirement.as_ref())
                .ok_or(OperationError::State)?
                .mode;
            if head.as_ref().is_none_or(|head| head.operation_id != arm) {
                if role.transition.is_none()
                    && role
                        .bootstrap
                        .get(&member)
                        .ok_or(OperationError::State)?
                        .change
                        .is_none()
                {
                    native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        arm.uuid(),
                        match mode {
                            CleanupMode::Retained => AssetEventV1::BeginRetirement {
                                exact_head_before_retirement: head.ok_or(OperationError::State)?,
                            },
                            _ => AssetEventV1::BeginProducerCancellation {
                                exact_head_before_cancellation: head
                                    .ok_or(OperationError::State)?,
                            },
                        },
                        deadline,
                    )?;
                }
                resume_change(&self.journal, role, member, deadline)?;
                retry_transition(role, &self.journal, member, deadline)?;
                self.finish_transition(bundle_id, role).await?;
            }
            if empty_cancel {
                let budget_closed = role
                    .response_budget
                    .as_ref()
                    .is_some_and(|budget| budget.closed_and_empty());
                let bootstrap = role
                    .bootstrap
                    .get_mut(&member)
                    .ok_or(OperationError::State)?;
                let state = bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .state();
                if (mode.requires_response_barrier() && !budget_closed)
                    || bootstrap.change.is_some()
                    || bootstrap.next_arm.is_some()
                    || !bootstrap.native_io_dormant()
                    || !bootstrap.readers.is_empty()
                    || !bootstrap
                        .native_root
                        .as_ref()
                        .is_some_and(|root| root.initial_root_never_entered())
                    || state.asset.lifecycle != ScratchLifecycleV1::RetiredClosed
                    || state.bootstrap.phase != BootstrapPhase::Closed
                    || state.asset.totals.pending_jobs != 0
                    || state.asset.directory.is_some()
                    || !bootstrap
                        .fold
                        .as_ref()
                        .ok_or(OperationError::State)?
                        .files()
                        .is_empty()
                {
                    return Err(OperationError::State);
                }
                bootstrap
                    .retirement
                    .as_mut()
                    .ok_or(OperationError::State)?
                    .producer_closed = Some(ProducerFiniteClosure {
                    binding: role.binding,
                    member,
                    head: state.asset.head.ok_or(OperationError::State)?,
                });
            }
            role.bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?
                .retirement
                .as_mut()
                .ok_or(OperationError::State)?
                .pending = None;
            return Ok(());
        }
        let (io_id, observe_id, join_id, operation, armed, launched, joined) = {
            let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
            let Some(CleanupPending::Io(io)) = bootstrap
                .retirement
                .as_ref()
                .ok_or(OperationError::State)?
                .pending
                .as_ref()
            else {
                return Err(OperationError::State);
            };
            (
                io.io,
                io.observe,
                io.join,
                io.operation.clone(),
                io.armed,
                io.launched,
                io.joined,
            )
        };
        if !armed {
            if head.as_ref().is_none_or(|head| head.operation_id != io_id) {
                if role.transition.is_none()
                    && role
                        .bootstrap
                        .get(&member)
                        .ok_or(OperationError::State)?
                        .change
                        .is_none()
                {
                    match operation.clone() {
                        CleanupOperation::Output(operation) => {
                            native::publication::queue_output_change(
                                &self.journal,
                                role,
                                member,
                                io_id.uuid(),
                                AssetEventV1::ArmIo { io_id, operation },
                                deadline,
                            )?
                        }
                        CleanupOperation::Bootstrap(operation) => queue_bootstrap_change(
                            &self.journal,
                            role,
                            member,
                            io_id,
                            AssetEventV3::ArmBootstrapV1 { io_id, operation },
                            deadline,
                        )?,
                    }
                }
                resume_change(&self.journal, role, member, deadline)?;
                retry_transition(role, &self.journal, member, deadline)?;
                self.finish_transition(bundle_id, role).await?;
            }
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let Some(CleanupPending::Io(io)) = bootstrap
                .retirement
                .as_mut()
                .ok_or(OperationError::State)?
                .pending
                .as_mut()
            else {
                return Err(OperationError::State);
            };
            io.armed = true;
            return Ok(());
        }
        if !launched {
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let state = bootstrap
                .fold
                .as_ref()
                .ok_or(OperationError::State)?
                .state();
            let baseline = [
                state
                    .bootstrap
                    .current
                    .as_ref()
                    .map(|directory| &directory.observed),
                state
                    .bootstrap
                    .parent
                    .as_ref()
                    .map(|directory| &directory.observed),
                state.asset.directory.as_ref(),
            ];
            let Some(CleanupPending::Io(io)) = bootstrap
                .retirement
                .as_mut()
                .ok_or(OperationError::State)?
                .pending
                .as_mut()
            else {
                return Err(OperationError::State);
            };
            let enter = io.cell.launch(
                bootstrap
                    .native_root
                    .as_mut()
                    .ok_or(OperationError::State)?,
                &baseline,
                #[cfg(all(
                    test,
                    feature = "fixtures",
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                ))]
                (member, io.io.uuid()),
            )?;
            io.launched = true;
            let _ = enter.send(());
            return Ok(());
        }
        if !joined {
            let bootstrap = role
                .bootstrap
                .get_mut(&member)
                .ok_or(OperationError::State)?;
            let Some(CleanupPending::Io(io)) = bootstrap
                .retirement
                .as_mut()
                .ok_or(OperationError::State)?
                .pending
                .as_mut()
            else {
                return Err(OperationError::State);
            };
            io.cell.join_registered().await?;
            io.joined = true;
            return Ok(());
        }
        let fact = {
            let bootstrap = role.bootstrap.get(&member).ok_or(OperationError::State)?;
            let Some(CleanupPending::Io(io)) = bootstrap
                .retirement
                .as_ref()
                .ok_or(OperationError::State)?
                .pending
                .as_ref()
            else {
                return Err(OperationError::State);
            };
            io.cell.fact().ok_or(OperationError::State)?
        };
        let event = match (&operation, &fact) {
            (
                CleanupOperation::Output(_),
                CleanupFact::Asset {
                    outcome,
                    observation,
                },
            ) => AssetEventV3::OutputV2 {
                event: AssetEventV1::ObserveIo {
                    io_id,
                    native_join_operation_id: join_id,
                    outcome: outcome.clone(),
                    observation: observation.clone(),
                },
            },
            (CleanupOperation::Bootstrap(_), CleanupFact::Bootstrap(result)) => {
                AssetEventV3::ObserveBootstrapV1 {
                    io_id,
                    native_join_operation_id: join_id,
                    result: result.clone(),
                }
            }
            _ => return Err(OperationError::State),
        };
        let positive = match &fact {
            CleanupFact::Asset {
                outcome,
                observation,
            } => {
                *outcome == NativeOutcomeV1::Completed
                    && matches!(
                        observation,
                        AssetObservationV1::Removed { .. } | AssetObservationV1::Synced
                    )
            }
            CleanupFact::Bootstrap(result) => result.outcome == NativeOutcomeV1::Completed,
        };
        let partial_close = matches!(
            &operation,
            CleanupOperation::Bootstrap(BootstrapIoV1::CloseBootstrapHandles { .. })
        );
        let is_close = partial_close
            || matches!(
                &operation,
                CleanupOperation::Bootstrap(BootstrapIoV1::CloseRetainedHandles { .. })
            );
        if head
            .as_ref()
            .is_none_or(|head| head.operation_id != observe_id)
        {
            if role.transition.is_none()
                && role
                    .bootstrap
                    .get(&member)
                    .ok_or(OperationError::State)?
                    .change
                    .is_none()
            {
                match event {
                    AssetEventV3::OutputV2 { event } => native::publication::queue_output_change(
                        &self.journal,
                        role,
                        member,
                        observe_id.uuid(),
                        event,
                        deadline,
                    )?,
                    event => queue_bootstrap_change(
                        &self.journal,
                        role,
                        member,
                        observe_id,
                        event,
                        deadline,
                    )?,
                }
            }
            resume_change(&self.journal, role, member, deadline)?;
            retry_transition(role, &self.journal, member, deadline)?;
            self.finish_transition(bundle_id, role).await?;
        }
        let mode = role
            .bootstrap
            .get(&member)
            .and_then(|bootstrap| bootstrap.retirement.as_ref())
            .ok_or(OperationError::State)?
            .mode;
        let budget_closed = role
            .response_budget
            .as_ref()
            .is_some_and(|budget| budget.closed_and_empty());
        let bootstrap = role
            .bootstrap
            .get_mut(&member)
            .ok_or(OperationError::State)?;
        let Some(CleanupPending::Io(io)) = bootstrap
            .retirement
            .as_mut()
            .ok_or(OperationError::State)?
            .pending
            .as_mut()
        else {
            return Err(OperationError::State);
        };
        io.cell.acknowledge_after_observe(
            bootstrap
                .native_root
                .as_mut()
                .ok_or(OperationError::State)?,
        )?;
        if !positive {
            return Err(OperationError::State);
        }
        if is_close {
            let state = bootstrap
                .fold
                .as_ref()
                .ok_or(OperationError::State)?
                .state();
            if (mode.requires_response_barrier() && !budget_closed)
                || !io.cell.positive_close_acknowledged()
                || !bootstrap
                    .native_root
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .no_original_handles()
                || !bootstrap.readers.is_empty()
                || (partial_close && !bootstrap.native_io_dormant())
                || (!partial_close
                    && bootstrap
                        .native_root
                        .as_ref()
                        .is_some_and(|root| root.imported_retained())
                    && !bootstrap.native_io_dormant())
                || (!partial_close
                    && !bootstrap
                        .native_root
                        .as_ref()
                        .is_some_and(|root| root.imported_retained())
                    && !bootstrap.native_io_ready())
                || state.asset.lifecycle != ScratchLifecycleV1::RetiredClosed
                || state.asset.directory.is_some()
                || !bootstrap
                    .fold
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .files()
                    .is_empty()
                || state.bootstrap.phase != BootstrapPhase::Closed
                || state.asset.totals.pending_jobs != 0
            {
                return Err(OperationError::State);
            }
            let run = bootstrap.retirement.as_mut().ok_or(OperationError::State)?;
            let head = state.asset.head.ok_or(OperationError::State)?;
            match run.mode {
                CleanupMode::Retained => {
                    run.closed = Some(RetainedFiniteClosure {
                        binding: role.binding,
                        member,
                        head,
                    });
                }
                _ => {
                    run.producer_closed = Some(ProducerFiniteClosure {
                        binding: role.binding,
                        member,
                        head,
                    });
                }
            }
        }
        bootstrap
            .retirement
            .as_mut()
            .ok_or(OperationError::State)?
            .pending = None;
        Ok(())
    }
}
