//! Positive whole-role closure for the fixed rolling copy graph. The source,
//! parser, root and output proofs are retained together until the same typed
//! journal settlement has a known result.
use super::*;
use crate::adaptive_resources::physical_graph::copy_vod::{
    CopyProcessClosure, CopyRootOnlyClosure,
};
use crate::adaptive_resources::physical_graph::finite_vod::FiniteVodNeverRootClosure;
use crate::adaptive_resources::runtime::copy_vod::CopyCleanupHoldReason;
use crate::adaptive_resources::runtime::copy_vod::CopyCloseClaim;
use crate::adaptive_resources::runtime::operations::bootstrap::retirement::ProducerFiniteClosure;
use crate::resource_custody::{MemberKind, PhysicalTypedSettlement};

pub(in crate::adaptive_resources) struct CopyCancellationClosure {
    binding: ReservationBinding,
    _process: CopyCancelProcess,
    _output: Option<ProducerFiniteClosure>,
}
enum CopyCancelProcess {
    NeverRoot { _proof: FiniteVodNeverRootClosure },
    RootOnly { _proof: CopyRootOnlyClosure },
    Joined { _proof: CopyProcessClosure },
}
#[derive(Clone, Copy)]
enum CopyCloseEvidence<'a> {
    NeverRoot(&'a FiniteVodNeverRootClosure),
    RootOnly(&'a CopyRootOnlyClosure),
    Joined(&'a CopyProcessClosure),
}
impl CopyCloseEvidence<'_> {
    fn matches(self, graph: &SealedPhysicalGraph) -> bool {
        match self {
            Self::NeverRoot(p) => p.matches(graph),
            Self::RootOnly(p) => p.matches(graph),
            Self::Joined(p) => p.matches(graph),
        }
    }
    fn generation(self) -> Uuid {
        match self {
            Self::NeverRoot(p) => p.generation(),
            Self::RootOnly(p) => p.generation(),
            Self::Joined(p) => p.generation(),
        }
    }
    fn root_member(self) -> Uuid {
        match self {
            Self::NeverRoot(p) => p.root_member(),
            Self::RootOnly(p) => p.root_member(),
            Self::Joined(p) => p.root_member(),
        }
    }
    fn acquisition_id(self) -> Uuid {
        match self {
            Self::NeverRoot(p) => p.acquisition_id(),
            Self::RootOnly(p) => p.acquisition_id(),
            Self::Joined(p) => p.acquisition_id(),
        }
    }
    fn close_operation_id(self) -> Uuid {
        match self {
            Self::NeverRoot(p) => p.close_operation_id(),
            Self::RootOnly(p) => p.close_operation_id(),
            Self::Joined(p) => p.close_operation_id(),
        }
    }
    fn root(self) -> Option<GraphRoot> {
        match self {
            Self::NeverRoot(_) => None,
            Self::RootOnly(p) => Some(p.root()),
            Self::Joined(p) => Some(p.root()),
        }
    }
}
impl CopyCancellationClosure {
    pub(in crate::adaptive_resources) fn into_binding(self) -> ReservationBinding {
        self.binding
    }
}

impl NodeResourceRuntime {
    /// Existing daemon maintenance retries original failed constructors and
    /// abandoned close attempts. This projection creates no R or native owner;
    /// an unknown prepare/settlement leaves the exact book entry charged.
    pub(in crate::adaptive_resources::runtime) fn has_failed_copy_admission_inner(&self) -> bool {
        let Ok(book) = self.operations.lock() else {
            return true;
        };
        book.bundles.values().any(|state| {
            state.try_lock().map_or(true, |bundle| {
                bundle.roles.iter().any(|role| {
                    role.copy_cleanup
                        .as_ref()
                        .is_some_and(|custody| custody.repair_requested())
                        && !role.refunded
                        && role
                            .copy_vod
                            .as_ref()
                            .is_none_or(|copy| copy.permanent_hold.is_none())
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
            let cleanup_claim =
                {
                    let bundle =
                        match tokio::time::timeout_at(deadline.into(), preparation.state.lock())
                            .await
                        {
                            Ok(bundle) => bundle,
                            Err(_) => break,
                        };
                    if bundle.roles.len() != 1
                        || bundle.roles[0].refunded
                        || bundle.roles[0]
                            .copy_vod
                            .as_ref()
                            .is_some_and(|copy| copy.permanent_hold.is_some())
                    {
                        continue;
                    }
                    bundle.roles[0]
                        .copy_cleanup
                        .as_ref()
                        .and_then(|custody| custody.try_claim_repair())
                };
            let Some(cleanup_claim) = cleanup_claim else {
                continue;
            };
            let accepted = match tokio::time::timeout_at(
                deadline.into(),
                self.acknowledge_copy_preparation_for_cleanup(&preparation),
            )
            .await
            {
                Ok(Ok(accepted)) => accepted,
                _ => continue,
            };
            if !accepted {
                if !matches!(
                    tokio::time::timeout_at(
                        deadline.into(),
                        self.prepare_unaccepted_copy_close(&preparation, &cleanup_claim)
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    continue;
                }
                let mut selection = Some(preparation);
                if matches!(
                    tokio::time::timeout_at(
                        deadline.into(),
                        self.finish_finite_close(&mut selection, 0, deadline)
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    cleanup_claim.complete();
                    completed += 1;
                }
                continue;
            }
            let (pending_close, output_member) =
                {
                    let bundle =
                        match tokio::time::timeout_at(deadline.into(), preparation.state.lock())
                            .await
                        {
                            Ok(bundle) => bundle,
                            Err(_) => break,
                        };
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
                if !matches!(
                    tokio::time::timeout_at(
                        deadline.into(),
                        self.prepare_copy_close(
                            &preparation,
                            &cleanup_claim,
                            0,
                            output_member,
                            true,
                            deadline
                        )
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    continue;
                }
            }
            let mut selection = Some(preparation);
            if matches!(
                tokio::time::timeout_at(
                    deadline.into(),
                    self.finish_finite_close(&mut selection, 0, deadline)
                )
                .await,
                Ok(Ok(()))
            ) {
                cleanup_claim.complete();
                completed += 1;
            }
        }
        Ok(completed)
    }

    /// Keep a definitely unaccepted original copy prepare behind its own
    /// graph absence witness and application fence. This uses the existing
    /// CopyCancellationClosure, never a software FiniteLaunch or writer fact.
    pub(in crate::adaptive_resources::runtime) async fn prepare_unaccepted_copy_close(
        &self,
        owner: &OwnedPreparation,
        cleanup_claim: &CopyCloseClaim,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        let mut bundle = owner.state.lock().await;
        if !finite_prepare_definitely_unaccepted(&bundle) || bundle.roles.len() != 1 {
            return Err(OperationError::State);
        }
        let role = &mut bundle.roles[0];
        if !role
            .copy_cleanup
            .as_ref()
            .is_some_and(|custody| cleanup_claim.matches(custody))
        {
            return Err(OperationError::State);
        }
        if role.refunded
            || role.finite_vod.is_some()
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
        if role.receipt.is_some() && role.copy_vod.is_none() {
            return Ok(());
        }
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        if copy.entered
            || copy.task.is_some()
            || copy.task_observed_committed
            || copy.fixed_image.is_some()
            || copy.process_closure.is_some()
            || copy.terminal.is_some()
        {
            return Err(OperationError::State);
        }
        let (root_member, task_member, acquisition_id) =
            (copy.root_member, copy.task_member, copy.acquisition_id);
        self.close_retained_entry(role.binding)?;
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        if role
            .copy_vod
            .as_ref()
            .ok_or(OperationError::State)?
            .never_root_closure
            .is_none()
        {
            let absence = role
                .sealed
                .as_ref()
                .ok_or(OperationError::State)?
                .try_finite_vod_never_root_closure(root_member, task_member, acquisition_id)
                .await?;
            role.copy_vod
                .as_mut()
                .ok_or(OperationError::State)?
                .never_root_closure = Some(absence);
        }
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        let absent = copy
            .never_root_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        if role.receipt.is_some()
            || !absent.matches(role.sealed.as_ref().ok_or(OperationError::State)?)
            || absent.root_member() != copy.root_member
            || absent.task_member() != copy.task_member
            || absent.acquisition_id() != copy.acquisition_id
        {
            return Err(OperationError::State);
        }
        if let Some(budget) = role.response_budget.as_ref() {
            budget.begin_close_if_empty()?;
        }
        let receipt = PhysicalCloseReceipt::from_copy_cancel(CopyCancellationClosure {
            binding: role.binding,
            _process: CopyCancelProcess::NeverRoot {
                _proof: copy
                    .never_root_closure
                    .take()
                    .expect("original validated copy graph absence"),
            },
            _output: None,
        });
        role.receipt = Some(receipt);
        // Drop the same parser/sink/back-reference payload while R remains
        // charged. The actual external application aliases still block final
        // rich-bundle destruction; no journal phase is manufactured.
        role.copy_vod = None;
        role.response_budget = None;
        role.members.clear();
        role.typed_members = None;
        role.sealed = None;
        Ok(())
    }

    /// Positively close a copy role whose admitted constructor failed before
    /// the first source/open/root entry. The sealed original graph, not a
    /// caller's failed allocation, proves that no native member escaped.
    /// The classifier selects the original per-state adapter; it is not a
    /// nonentry witness. Every state still needs its sealed graph inventory.
    pub(in crate::adaptive_resources::runtime) async fn prepare_copy_close(
        &self,
        owner: &OwnedPreparation,
        cleanup_claim: &CopyCloseClaim,
        index: usize,
        output_member: Uuid,
        abort: bool,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.prepare_failed_copy_close(owner, cleanup_claim, index, output_member, abort, deadline)
            .await
    }

    async fn prepare_failed_copy_close(
        &self,
        owner: &OwnedPreparation,
        cleanup_claim: &CopyCloseClaim,
        index: usize,
        output_member: Uuid,
        abort: bool,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.validate_preparation(owner)?;
        {
            let mut bundle = owner.state.lock().await;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            if !role
                .copy_cleanup
                .as_ref()
                .is_some_and(|custody| cleanup_claim.matches(custody))
            {
                return Err(OperationError::State);
            }
            if role.refunded
                || role.pending_finite_close.is_some()
                || role
                    .transition
                    .as_ref()
                    .is_some_and(|t| t.effect == Effect::Settle)
            {
                return Ok(());
            }
            let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
            if copy.permanent_hold.is_some() {
                return Err(OperationError::State);
            }
            if copy.entered && role.roots.is_empty() {
                // The launcher may have forked before losing its handle. No
                // absence witness, kill receipt or restart authority exists.
                copy.permanent_hold = Some(CopyCleanupHoldReason::UncertainLaunch);
                return Err(OperationError::State);
            }
            if !role.finite_app.as_ref().is_some_and(|state| state.seal()) {
                return Err(OperationError::State);
            }
            copy.closing = true;
            copy.progress.notify_waiters();
            if let Some(graph) = role.graph.take() {
                role.sealed = Some(graph.seal());
            }
        }
        self.reconcile_copy_acquisition_for_cleanup(owner, index, output_member, deadline)
            .await?;
        #[cfg(all(test, feature = "fixtures"))]
        self.fixture_copy_stage(
            super::copy_vod::CopyStartFixtureStage::CleanupAcquisitionsResolved,
        )
        .await;
        let (never_root, root_only, initial_bootstrap) = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
            (
                !copy.entered && role.roots.is_empty() && copy.task.is_none(),
                copy.entered && role.roots.len() == 1 && copy.task.is_none(),
                role.bootstrap
                    .get(&output_member)
                    .is_some_and(|b| b.has_root_arm()),
            )
        };
        if initial_bootstrap {
            // Reconcile the ORIGINAL prefix arm; false-entry mode cannot send
            // a fresh open/mkdir latch and retains an actual NeverEntered job.
            self.observe_finite_bootstrap_root(owner, index, output_member, deadline)
                .await?;
        }
        if never_root {
            if initial_bootstrap {
                // A cancelled sink waiter may leave the ORIGINAL traversal or
                // mkdir job registered. Reconcile that arm before the strict
                // no-process-root classifier inspects the retained prefix.
                // This observer cannot release fresh native entry.
                self.observe_finite_bootstrap_arm(owner, index, output_member, deadline)
                    .await?;
            }
            self.close_copy_never_root(owner, index, output_member)
                .await?;
        } else if root_only {
            self.close_copy_root_only(owner, index).await?;
        } else {
            self.join_copy_producer(owner, index, abort).await?;
        }
        #[cfg(all(test, feature = "fixtures"))]
        self.fixture_copy_stage(super::copy_vod::CopyStartFixtureStage::CleanupNativeClosed)
            .await;
        let has_output = {
            let bundle = owner.state.lock().await;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            if role.bootstrap.len() > 1 || role.bootstrap.keys().any(|m| *m != output_member) {
                return Err(OperationError::State);
            }
            role.bootstrap.contains_key(&output_member)
        };
        if has_output {
            self.observe_finite_bootstrap_arm(owner, index, output_member, deadline)
                .await?;
            self.close_producer_finite_native(owner, index, output_member, deadline)
                .await?;
        }
        let needs_seal = {
            let bundle = owner.state.lock().await;
            !matches!(
                bundle.roles.get(index).ok_or(OperationError::State)?.phase,
                Phase::Sealed | Phase::UnknownHeld
            )
        };
        if needs_seal {
            self.seal_role(owner, index, deadline).await?;
        }
        self.prepare_copy_settlement(owner, index, output_member)
            .await?;
        #[cfg(all(test, feature = "fixtures"))]
        self.fixture_copy_stage(super::copy_vod::CopyStartFixtureStage::CleanupSettlementPrepared)
            .await;
        Ok(())
    }

    async fn close_copy_never_root(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        output_member: Uuid,
    ) -> Result<(), OperationError> {
        {
            let mut bundle = owner.state.lock().await;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
            // invoked records the attempted root launch even on a physical
            // pre-entry refusal. Allow only this original root here; the SAME
            // graph's positive no-acquisition constructor below remains owed.
            if role.refunded
                || copy.entered
                || copy.task.is_some()
                || !role.roots.is_empty()
                || role.retirement.is_some()
                || role.bootstrap.len() > 1
                || role.sources.len() > copy.source_members.len()
                || role
                    .sources
                    .keys()
                    .any(|m| !copy.source_members.contains(m))
                || role
                    .invoked
                    .iter()
                    .any(|m| !copy.source_members.contains(m) && *m != copy.root_member)
                || role.transition.is_some()
            {
                return Err(OperationError::State);
            }
            if let Some(bootstrap) = role.bootstrap.get(&output_member) {
                if matches!(
                    bootstrap.never_root_disposition(role.armed.contains(&output_member))?,
                    bootstrap::NeverRootBootstrap::UnarmedPlaceholder
                ) {
                    role.bootstrap.remove(&output_member);
                }
                // A retained acquired prefix is not NeverAcquired: native
                // output retirement below must close that same BootstrapMember.
            }
            self.close_retained_entry(role.binding)?;
        }
        // No parser/root has entered in this state. Keep the ORIGINAL
        // bundle guard while the sealed graph proves absence, and install
        // the one-shot proof synchronously before another cancellable await.
        // A contender can never leave a minted proof in a disposable waiter.
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        let graph = role
            .sealed
            .as_ref()
            .ok_or(OperationError::State)?
            .select_same();
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if copy.never_root_closure.is_none() {
            #[cfg(all(test, feature = "fixtures"))]
            self.fixture_copy_stage(
                super::copy_vod::CopyStartFixtureStage::NeverRootProofReadyToMint,
            )
            .await;
            let proof = graph
                .try_finite_vod_never_root_closure(
                    copy.root_member,
                    copy.task_member,
                    copy.acquisition_id,
                )
                .await?;
            copy.never_root_closure = Some(proof);
            #[cfg(all(test, feature = "fixtures"))]
            self.fixture_copy_stage(super::copy_vod::CopyStartFixtureStage::NeverRootProofStored)
                .await;
        }
        let proof = role
            .copy_vod
            .as_ref()
            .and_then(|c| c.never_root_closure.as_ref())
            .ok_or(OperationError::State)?;
        let mut first_error = None;
        for source in role.sources.values_mut() {
            if let Err(error) = source.close_parent_before_root(proof).await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            if role
                .sources
                .values()
                .any(|source| source.copy_cleanup_permanently_unknown())
            {
                super::copy_vod::set_copy_hold(
                    role.copy_vod.as_mut().ok_or(OperationError::State)?,
                    CopyCleanupHoldReason::FailedSourceClose,
                );
            }
            return Err(error);
        }
        Ok(())
    }

    async fn close_copy_root_only(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<(), OperationError> {
        let (graph, root) = {
            let mut bundle = owner.state.lock().await;
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
            if copy.task.is_none() && role.invoked.contains(&copy.task_member) {
                // Registration was recorded, but its original selector is
                // missing. Never adapt the live writer into a root-only proof.
                super::copy_vod::set_copy_hold(
                    copy,
                    CopyCleanupHoldReason::TerminalIdentityMismatch,
                );
                return Err(OperationError::State);
            }
            if !copy.entered
                || copy.task.is_some()
                || copy.task_observed_committed
                || copy.terminal.is_some()
                || role.roots.len() != 1
                || role.sources.len() != 2
            {
                return Err(OperationError::State);
            }
            if copy.root_only_closure.is_some() {
                return Ok(());
            }
            (
                role.sealed
                    .as_ref()
                    .ok_or(OperationError::State)?
                    .select_same(),
                *role
                    .roots
                    .get(&copy.root_member)
                    .ok_or(OperationError::State)?,
            )
        };
        self.request_copy_kill_or_observe_exit(owner, index, &graph, root)
            .await?;
        graph.wait_root(root).await?;
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        self.close_retained_entry(role.binding)?;
        let mut first_error = None;
        for bootstrap in role.bootstrap.values_mut() {
            if let Err(error) = bootstrap.join_registered_native().await {
                first_error.get_or_insert(error);
            }
        }
        for source in role.sources.values_mut() {
            if let Err(error) = source.join_registered().await {
                first_error.get_or_insert(error);
            }
        }
        for source in role.sources.values_mut() {
            if let Err(error) = source.close_parent().await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            if role
                .sources
                .values()
                .any(|source| source.copy_cleanup_permanently_unknown())
            {
                super::copy_vod::set_copy_hold(
                    role.copy_vod.as_mut().ok_or(OperationError::State)?,
                    CopyCleanupHoldReason::FailedSourceClose,
                );
            }
            return Err(error);
        }
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if copy.root_only_closure.is_none() {
            match graph
                .try_copy_root_only_closure(
                    root,
                    copy.root_member,
                    copy.task_member,
                    copy.acquisition_id,
                    &mut copy.fixed_image,
                )
                .await
            {
                Ok(proof) => copy.root_only_closure = Some(proof),
                Err(error) => {
                    if let Some(reason) = graph.copy_hold_reason().await {
                        super::copy_vod::set_copy_hold(
                            copy,
                            super::copy_vod::map_copy_task_hold(reason),
                        );
                    }
                    return Err(error.into());
                }
            }
        }
        let proof = copy
            .root_only_closure
            .as_ref()
            .ok_or(OperationError::State)?;
        if !proof.matches(&graph)
            || proof.root() != root
            || proof.root_member() != copy.root_member
            || proof.task_member() != copy.task_member
            || proof.acquisition_id() != copy.acquisition_id
            || proof.executable_identity() != copy.command.executable().source_identity()
            || proof.executable_sha256() != copy.command.executable().sha256()
        {
            copy.permanent_hold = Some(CopyCleanupHoldReason::TerminalIdentityMismatch);
            return Err(OperationError::State);
        }
        #[cfg(test)]
        if std::mem::take(&mut copy.post_mint_source_collision) {
            role.sources
                .values_mut()
                .next()
                .ok_or(OperationError::State)?
                .fixture_parent_close_operation_collision(proof.close_operation_id());
        }
        if let Some(error) = role
            .sources
            .values()
            .find_map(|source| source.copy_root_only_closed_fact(proof).err())
        {
            super::copy_vod::set_copy_hold(copy, CopyCleanupHoldReason::TerminalIdentityMismatch);
            return Err(error);
        }
        Ok(())
    }

    /// Validate all original state-specific facts while the witnesses remain
    /// retained, then move the one process witness and optional prefix proof
    /// into exactly one pending settlement with no intervening await.
    async fn prepare_copy_settlement(
        &self,
        owner: &OwnedPreparation,
        index: usize,
        output_member: Uuid,
    ) -> Result<(), OperationError> {
        let mut bundle = owner.state.lock().await;
        let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
        // A positive original closure may settle an existing hold directly.
        // Keep UnknownHeld until that exact settlement commits; generic Seal
        // deliberately cannot erase a hold classification.
        if !matches!(role.phase, Phase::Sealed | Phase::UnknownHeld)
            || role.transition.is_some()
            || role.graph.is_some()
            || role.retirement.is_some()
            || role.bootstrap.len() > 1
        {
            return Err(OperationError::State);
        }
        if let Some(budget) = &role.response_budget {
            budget.begin_close_if_empty()?;
        }
        #[cfg(test)]
        let omit_owner_once = std::mem::take(
            &mut role
                .copy_vod
                .as_mut()
                .ok_or(OperationError::State)?
                .preflight_omit_owner_once,
        );
        let handle = role.handle.clone().ok_or(OperationError::State)?;
        let graph = role.sealed.as_ref().ok_or(OperationError::State)?;
        let copy = role.copy_vod.as_ref().ok_or(OperationError::State)?;
        let evidence = match (
            &copy.never_root_closure,
            &copy.root_only_closure,
            &copy.process_closure,
        ) {
            (Some(proof), None, None)
                if !copy.entered && copy.task.is_none() && copy.terminal.is_none() =>
            {
                CopyCloseEvidence::NeverRoot(proof)
            }
            (None, Some(proof), None)
                if copy.entered
                    && copy.task.is_none()
                    && copy.terminal.is_none()
                    && !role.invoked.contains(&copy.task_member) =>
            {
                CopyCloseEvidence::RootOnly(proof)
            }
            (None, None, Some(proof))
                if copy.entered
                    && copy.task.is_some()
                    && copy.task_observed_committed
                    && copy.terminal.is_some() =>
            {
                CopyCloseEvidence::Joined(proof)
            }
            _ => return Err(OperationError::State),
        };
        if !evidence.matches(graph)
            || evidence.generation() != handle.graph_id()
            || evidence.root_member() != copy.root_member
            || evidence.acquisition_id() != copy.acquisition_id
            || evidence.close_operation_id().is_nil()
        {
            return Err(OperationError::State);
        }
        let task_matches = match evidence {
            CopyCloseEvidence::NeverRoot(proof) => proof.task_member() == copy.task_member,
            CopyCloseEvidence::RootOnly(proof) => proof.task_member() == copy.task_member,
            CopyCloseEvidence::Joined(proof) => Some(proof.task()) == copy.task,
        };
        if !task_matches {
            return Err(OperationError::State);
        }
        // U2: close the retained native owners, but an unbound process identity
        // can never create a LegacyV1 settlement fact or free the original R.
        if evidence.root().is_some()
            && (!copy.root_observed_committed
                || evidence
                    .root()
                    .is_some_and(|root| matches!(root.process, Metric::Unknown(_))))
        {
            role.copy_vod
                .as_mut()
                .expect("original copy")
                .permanent_hold = Some(CopyCleanupHoldReason::UnboundRootIdentity);
            return Err(OperationError::State);
        }
        let scratch_close = role
            .bootstrap
            .get(&output_member)
            .map(|b| b.producer_close_fact(role.binding, output_member))
            .transpose()?;
        let planned = role.typed_members.as_ref().ok_or(OperationError::State)?;
        if planned.len() != 5 {
            return Err(OperationError::State);
        }
        let close_id = v2::Id::new(evidence.close_operation_id())?;
        let mut facts = Vec::with_capacity(planned.len());
        for plan in planned {
            let member_id = v2::Id::new(plan.member_id())?;
            let fact = match plan {
                v2::MemberPlan::SourceV1 { .. }
                    if copy.source_members.contains(&plan.member_id()) =>
                {
                    match (role.sources.get(&plan.member_id()), evidence) {
                        (Some(source), CopyCloseEvidence::NeverRoot(proof)) => {
                            source.finite_never_root_closed_fact(proof)?
                        }
                        (Some(source), CopyCloseEvidence::RootOnly(proof)) => {
                            source.copy_root_only_closed_fact(proof)?
                        }
                        (Some(source), CopyCloseEvidence::Joined(proof)) => {
                            source.copy_closed_fact(proof)?
                        }
                        (None, CopyCloseEvidence::NeverRoot(proof))
                            if !role.armed.contains(&plan.member_id())
                                && !role.invoked.contains(&plan.member_id()) =>
                        {
                            source_files::finite_unregistered_closed_fact(plan, proof)?
                        }
                        _ => return Err(OperationError::State),
                    }
                }
                v2::MemberPlan::LegacyV1 { planned }
                    if planned.member_id == copy.root_member
                        && planned.kind == MemberKind::Root
                        && planned.adapter == Adapter::ProcessRoot =>
                {
                    match evidence.root() {
                        None => v2::ClosedFact::LegacyNeverAcquiredV1 {
                            planned: planned.clone(),
                            operation_id: close_id,
                        },
                        Some(root) => {
                            let Metric::Known(identity) = root.process else {
                                return Err(OperationError::State);
                            };
                            v2::ClosedFact::LegacyV1 { fact: crate::resource_custody::ClosedMemberFact {
                                member_id: planned.member_id, adapter: planned.adapter.clone(), adapter_version: planned.adapter_version,
                                domain: planned.domain.clone(), object_identity: copy_process_identity_text(identity.pid,identity.start_marker)?,
                                evidence: "original fixed copy root waited; original pipes, source parents and Child/ChildJob closed".into(),
                            }}
                        }
                    }
                }
                v2::MemberPlan::VodOutputTaskV1 { task, .. }
                    if task.actor == v2::VodActor::RollingCopyV1
                        && task.root_member_id.uuid() == copy.root_member
                        && task.member_id.uuid() == copy.task_member
                        && task.graph_id.uuid() == evidence.generation() =>
                {
                    if !matches!(evidence, CopyCloseEvidence::Joined(_)) {
                        if copy.task.is_some() || role.invoked.contains(&copy.task_member) {
                            return Err(OperationError::State);
                        }
                        v2::ClosedFact::VodOutputTaskNeverAcquiredV1 {
                            task: task.clone(),
                            operation_id: close_id,
                        }
                    } else {
                        if !task.inventory.pending_io.is_empty()
                            || task.inventory.buffers.len() != 1
                            || task.inventory.readers.len() != 2
                            || task.inventory.buffers[0].handles != 0
                            || task
                                .inventory
                                .readers
                                .iter()
                                .any(|r| r.handles != 1 || r.memory_bytes != 0)
                            || !task
                                .inventory
                                .readers
                                .iter()
                                .any(|r| r.owner_id == task.stdout_owner_id)
                            || !task
                                .inventory
                                .readers
                                .iter()
                                .any(|r| r.owner_id == task.diagnostic_owner_id)
                        {
                            return Err(OperationError::State);
                        }
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
                }
                v2::MemberPlan::ScratchAssetsV3 { plan, .. }
                    if member_id.uuid() == output_member =>
                {
                    match &scratch_close {
                        Some((closed, head)) if closed == plan => v2::ClosedFact::ScratchAssetsV3 {
                            plan: plan.clone(),
                            exact_head: head.clone(),
                            operation_id: head.operation_id,
                        },
                        None if matches!(evidence, CopyCloseEvidence::NeverRoot(_))
                            && role.bootstrap.is_empty()
                            && !role.armed.contains(&output_member)
                            && !role.invoked.contains(&output_member) =>
                        {
                            v2::ClosedFact::ScratchAssetsNeverAcquiredV3 {
                                plan: plan.clone(),
                                operation_id: close_id,
                            }
                        }
                        _ => return Err(OperationError::State),
                    }
                }
                _ => return Err(OperationError::State),
            };
            facts.push(v2::MemberDisposition::Closed { member_id, fact });
        }
        // Retire the prebuild scratch-plan clone before allocating the five
        // validation envelopes. The admitted workspace covers the largest
        // simultaneous pair of plan copies, not a hidden third clone.
        let has_scratch_close = scratch_close.is_some();
        drop(scratch_close);
        let operation_id = v2::Id::new(copy.settlement_operation_id)?;
        let settlement = v2::Settlement {
            version: 2,
            owner_id: v2::Id::new(self.owner.owner_id)?,
            graph_id: v2::Id::new(handle.graph_id())?,
            operation_id,
            members: facts,
        };
        #[cfg(test)]
        let settlement = {
            let mut settlement = settlement;
            if omit_owner_once {
                // Malform ONLY this locally assembled DTO, after actual native
                // closure. Original plans, observations and witnesses are intact.
                let owners = settlement
                    .members
                    .iter_mut()
                    .find_map(|member| match member {
                        v2::MemberDisposition::Closed {
                            fact: v2::ClosedFact::VodOutputTaskV1 { owners, .. },
                            ..
                        } => Some(owners),
                        _ => None,
                    })
                    .ok_or(OperationError::State)?;
                owners.pop().ok_or(OperationError::State)?;
            }
            settlement
        };
        let preflight = (|| -> Result<(), OperationError> {
            // Snapshot only the SAME retained acknowledged metadata, never a
            // Manifest reconstruction or native capability. Its exact known
            // requested workspace was admitted by known_copy_memory before R.
            let mut envelopes = Vec::with_capacity(planned.len());
            for plan in planned {
                let observed = match plan {
                    v2::MemberPlan::SourceV1 { .. } => role
                        .sources
                        .get(&plan.member_id())
                        .map(|source| source.copy_observed_for_settlement(plan))
                        .transpose()?
                        .flatten()
                        .map(|source| v2::Observed::SourceV1 {
                            source: source.clone(),
                        }),
                    v2::MemberPlan::LegacyV1 { planned } => match evidence.root() {
                        None => None,
                        Some(root) if copy.root_observed_committed => {
                            let Metric::Known(identity) = root.process else {
                                return Err(OperationError::State);
                            };
                            Some(v2::Observed::LegacyV1 {
                                identity: crate::resource_custody::ObservedIdentity {
                                    version: 1,
                                    member_id: planned.member_id,
                                    adapter: planned.adapter.clone(),
                                    adapter_version: planned.adapter_version,
                                    domain: planned.domain.clone(),
                                    object_identity: copy_process_identity_text(
                                        identity.pid,
                                        identity.start_marker,
                                    )?,
                                    process: Some(crate::resource_custody::ProcessIdentity {
                                        version: 1,
                                        pid: identity.pid,
                                        creation_marker: Some(identity.start_marker),
                                        domain: planned.domain.clone(),
                                    }),
                                },
                            })
                        }
                        _ => return Err(OperationError::State),
                    },
                    v2::MemberPlan::VodOutputTaskV1 { task, .. }
                        if copy.task_observed_committed =>
                    {
                        Some(v2::Observed::VodOutputTaskV1 { task: task.clone() })
                    }
                    v2::MemberPlan::VodOutputTaskV1 { .. } => None,
                    v2::MemberPlan::ScratchAssetsV3 { .. } => role
                        .bootstrap
                        .get(&output_member)
                        .map(|member| {
                            member.fold().ok_or(OperationError::State).map(|fold| {
                                v2::Observed::ScratchAssetsV3 {
                                    state: fold.state().clone(),
                                }
                            })
                        })
                        .transpose()?,
                    _ => return Err(OperationError::State),
                };
                envelopes.push(v2::Envelope {
                    version: 2,
                    planned: plan.clone(),
                    observed,
                });
            }
            // Length-counting FIRST bounds each serializer Vec, including the
            // aggregate settlement. The writer allocates no buffer; UUID scalar
            // conversion storage is separately admitted. Run the UNCHANGED full
            // validators while every one-shot proof remains retained.
            for envelope in &envelopes {
                count_copy_validation_json(envelope)?;
            }
            count_copy_validation_json(&settlement)?;
            for envelope in &envelopes {
                v2::validate_envelope_members(planned, envelope)?;
            }
            v2::validate_settlement_scope(
                v2::Id::new(self.owner.owner_id)?,
                v2::Id::new(handle.graph_id())?,
                planned.len(),
                &envelopes,
                &settlement,
            )?;
            drop(envelopes);
            Ok(())
        })();
        if let Err(error) = preflight {
            // These are metadata refusals, not lost native witnesses. Preserve
            // the SAME proofs, full R and logical operation ID for retry. An
            // immutable-invalid production DTO can remain held/retried; this
            // is visible policy, not a new recovery or native-close authority.
            super::copy_vod::set_copy_hold(
                role.copy_vod.as_mut().ok_or(OperationError::State)?,
                CopyCleanupHoldReason::ValidationRejected,
            );
            return Err(error);
        }
        let copy = role.copy_vod.as_mut().ok_or(OperationError::State)?;
        if copy.retryable_hold == Some(CopyCleanupHoldReason::ValidationRejected) {
            copy.retryable_hold = None;
        }
        let key = TransitionKey {
            handle,
            operation_id: operation_id.uuid(),
            expected_sequence: role.sequence,
        };
        // Stable under the same guard: no await/fallible DTO construction after
        // consuming the original one-shot native and prefix witnesses.
        let output = if has_scratch_close {
            Some(
                role.bootstrap
                    .get_mut(&output_member)
                    .expect("validated original prefix")
                    .take_producer_close(role.binding, output_member)
                    .expect("stable validated original prefix close"),
            )
        } else {
            None
        };
        let copy = role.copy_vod.as_mut().expect("validated copy");
        let process = if let Some(proof) = copy.never_root_closure.take() {
            CopyCancelProcess::NeverRoot { _proof: proof }
        } else if let Some(proof) = copy.root_only_closure.take() {
            CopyCancelProcess::RootOnly { _proof: proof }
        } else {
            CopyCancelProcess::Joined {
                _proof: copy
                    .process_closure
                    .take()
                    .expect("validated original joined closure"),
            }
        };
        let receipt = PhysicalCloseReceipt::from_copy_cancel(CopyCancellationClosure {
            binding: role.binding,
            _process: process,
            _output: output,
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

fn copy_process_identity_text(pid: u32, start_marker: u64) -> Result<String, OperationError> {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(31);
    write!(&mut text, "{pid}:{start_marker}").map_err(|_| OperationError::State)?;
    Ok(text)
}
fn count_copy_validation_json(value: &impl serde::Serialize) -> Result<(), OperationError> {
    struct CountWriter {
        bytes: usize,
    }
    impl std::io::Write for CountWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let next = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
            if next>crate::adaptive_resources::runtime::copy_vod::cleanup_memory::MAX_VALIDATION_JSON_BYTES {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }
            self.bytes = next;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(CountWriter { bytes: 0 }, value).map_err(|_| OperationError::State)
}
