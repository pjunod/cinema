//! Finite-only terminal boundary: actual application aliases and rich bundle
//! allocations leave before the one matching ledger refund. The journal keeps
//! the original receipt and operation across an uncertain completion.
use super::*;

/// One original ordinary finite role has one final census-close token.
/// The collector owns this inline storage independently through the R/S refund.
#[derive(Default)]
pub struct FiniteCloseWorkspace {
    pending: Option<PendingRuntimeClose>,
}

impl FiniteCloseWorkspace {
    pub const fn new() -> Self {
        Self { pending: None }
    }
}

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) fn finite_app_state(
        &self,
        owner: &OwnedPreparation,
        index: usize,
    ) -> Result<Arc<super::super::finite_lifetime::FiniteAppState>, OperationError> {
        self.validate_preparation(owner)?;
        let bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
        let role = bundle.roles.get(index).ok_or(OperationError::State)?;
        role.finite_app.clone().ok_or(OperationError::State)
    }

    /// This ticket is the sole application-side bundle selector after every
    /// output, producer and control alias dropped. The original command and
    /// physical receipt remain in the runtime/journal until a known result.
    pub(in crate::adaptive_resources::runtime) async fn finish_finite_close(
        &self,
        selection: &mut Option<OwnedPreparation>,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.finish_application_close(
            selection,
            index,
            FinalApplicationRole::Finite,
            None,
            deadline,
        )
        .await
    }

    pub(in crate::adaptive_resources::runtime) async fn finish_finite_close_in(
        &self,
        selection: &mut Option<OwnedPreparation>,
        index: usize,
        workspace: &mut FiniteCloseWorkspace,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.finish_application_close(
            selection,
            index,
            FinalApplicationRole::Finite,
            Some(workspace),
            deadline,
        )
        .await
    }

    #[cfg(target_os = "linux")]
    pub(in crate::adaptive_resources::runtime) async fn finish_planning_metadata_close(
        &self,
        selection: &mut Option<OwnedPreparation>,
        index: usize,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        self.finish_application_close(
            selection,
            index,
            FinalApplicationRole::PlanningMetadata,
            None,
            deadline,
        )
        .await
    }

    async fn finish_application_close(
        &self,
        selection: &mut Option<OwnedPreparation>,
        index: usize,
        kind: FinalApplicationRole,
        mut workspace: Option<&mut FiniteCloseWorkspace>,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        if workspace
            .as_ref()
            .is_some_and(|fixed| fixed.pending.is_some())
        {
            return Err(OperationError::State);
        }
        let owner = selection.as_ref().ok_or(OperationError::State)?;
        self.validate_preparation(owner)?;
        let unaccepted = {
            let bundle = owner.state.lock().await;
            if workspace.is_some()
                && (!matches!(kind, FinalApplicationRole::Finite)
                    || index != 0
                    || bundle.roles.len() != 1
                    || bundle.roles[0].pending_inventory_close.is_some())
            {
                return Err(OperationError::State);
            }
            matches!(kind, FinalApplicationRole::Finite)
                && finite_prepare_definitely_unaccepted(&bundle)
        };
        if !unaccepted {
            let mut bundle = owner.state.lock().await;
            if !bundle.installed || !kind.matches(&bundle, index) {
                return Err(OperationError::State);
            }
            let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
            if !role.finite_app.as_ref().is_some_and(|state| state.seal()) {
                return Err(OperationError::State);
            }
            if role.transition.is_none() {
                let pending = role
                    .pending_finite_close
                    .take()
                    .ok_or(OperationError::State)?;
                if !matches!(pending.effect, Effect::Settle | Effect::Cancel) {
                    return Err(OperationError::State);
                }
                role.transition = Some(Transition {
                    operation: JournalOperation::transition(self.journal.change(
                        pending.key,
                        pending.mutation,
                        deadline,
                    )),
                    effect: pending.effect,
                });
            }
            let transition = role.transition.as_mut().ok_or(OperationError::State)?;
            if transition.operation.pending.is_none()
                && !matches!(transition.operation.result, Some(Ok(_)))
            {
                transition.operation.retry(&self.journal, deadline)?;
            }
            let OperationResult::Changed(ack) = transition.operation.resolve().await? else {
                return Err(OperationError::State);
            };
            let handle = role.handle.as_ref().ok_or(OperationError::State)?;
            if ack.operation_id != transition.operation.id
                || ack.obligation_id != handle.obligation_id()
                || ack.graph_id != handle.graph_id()
                || (ack.sequence < role.sequence)
                || (ack.sequence == role.sequence && role.phase != Phase::Settled)
                || ack.phase != Phase::Settled
            {
                return Err(OperationError::State);
            }
            if transition.effect == Effect::Settle && role.receipt.is_none() {
                role.receipt = Some(
                    self.journal
                        .take_settled_receipt(transition.operation.id, handle)?,
                );
            }
            if transition.effect == Effect::Cancel {
                transition.operation.acknowledge(&self.journal)?;
            }
            role.sequence = ack.sequence;
            role.phase = ack.phase;
        }

        // An adopted S has one extra internal preparation alias, never an
        // application lease. Release it only after known terminal COMMIT.
        #[cfg(target_os = "linux")]
        if !unaccepted {
            self.release_settled_recovered_preparation(owner.id)?;
        }

        self.affected_owners
            .with_runtime_mutation(|inventory| {
                // All validation and arithmetic precede the one-shot drop of the
                // rich original bundle. Hold census→book→ledger through finalization.
                let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
                let owner = selection.as_ref().ok_or(OperationError::State)?;
                let selected = book.bundles.get(&owner.id).ok_or(OperationError::State)?;
                if !Arc::ptr_eq(selected, &owner.state) {
                    return Err(OperationError::State);
                }
                let (binding, durable, _cancel_without_physical_receipt) = {
                    let bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
                    let role = bundle.roles.get(index).ok_or(OperationError::State)?;
                    if (workspace.is_some()
                        && (index != 0
                            || bundle.roles.len() != 1
                            || role.pending_inventory_close.is_some()))
                        || bundle.id != owner.id
                        || !bundle.prepare.acknowledged
                        || !kind.matches(&bundle, index)
                        || role.refunded
                        || role.pending_finite_close.is_some()
                        || role
                            .finite_app
                            .as_ref()
                            .is_none_or(|state| !state.released())
                    {
                        return Err(OperationError::State);
                    }
                    let cancel_without_physical_receipt = if unaccepted {
                        if !finite_prepare_definitely_unaccepted(&bundle)
                            || !finite_unaccepted_payload_closed(role)
                        {
                            return Err(OperationError::State);
                        }
                        // The original actual absence receipt remains here after the
                        // sealed graph's conservative Drop marks its entry held. That
                        // mark is not new native uncertainty. Only this proven branch
                        // may use the receipt after every application owner is gone;
                        // a generic CancelPrepared still has no physical receipt.
                        false
                    } else {
                        let transition = role.transition.as_ref().ok_or(OperationError::State)?;
                        if !bundle.installed
                            || role.phase != Phase::Settled
                            || !matches!(transition.effect, Effect::Settle | Effect::Cancel)
                            || (transition.effect == Effect::Settle
                                && role.receipt.as_ref().is_none_or(|receipt| {
                                    receipt.custody_binding() != role.binding
                                }))
                        {
                            return Err(OperationError::State);
                        }
                        transition.effect == Effect::Cancel
                    };
                    (
                        role.binding,
                        role.durable_tag,
                        cancel_without_physical_receipt,
                    )
                };
                let proof = {
                    let mut bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
                    if bundle.roles.iter().enumerate().any(|(other, role)| {
                        other != index
                            && (role.inventory.is_some() || role.pending_inventory_close.is_none())
                    }) {
                        return Err(OperationError::State);
                    }
                    let role = bundle.roles.get_mut(index).ok_or(OperationError::State)?;
                    let original = role.inventory.as_ref().ok_or(OperationError::State)?;
                    if let Some(receipt) = role.receipt.as_ref() {
                        inventory.validate_runtime_close(original, receipt, &self.ledger)
                    } else {
                        // The final application fence above proves alias closure.
                        // Native absence still comes from the original sealed graph.
                        if role.native_never_acquired.is_none() {
                            if let Some(graph) = role.graph.take() {
                                role.sealed = Some(graph.seal());
                            }
                            role.native_never_acquired = Some(
                                role.sealed
                                    .as_ref()
                                    .ok_or(OperationError::State)?
                                    .try_never_acquired()
                                    .map_err(|_| OperationError::State)?,
                            );
                        }
                        let proof = inventory.validate_never_acquired_close(
                            original,
                            role.native_never_acquired
                                .as_ref()
                                .ok_or(OperationError::State)?,
                            &self.ledger,
                        );
                        // Original sealed graph Drop runs before the ledger
                        // guard, with the same compact absence witness held.
                        role.sealed = None;
                        role.graph = None;
                        proof
                    }
                    .map_err(|_| OperationError::State)?
                };
                let mut ledger = self
                    .ledger
                    .inner
                    .lock()
                    .map_err(|_| OperationError::State)?;
                let entry = ledger
                    .entries
                    .get(&binding.reservation)
                    .ok_or(OperationError::State)?;
                if entry.binding != binding
                    || entry.durable != Some(durable)
                    || ledger.used.checked_sub(entry.resources).is_none()
                {
                    return Err(OperationError::State);
                }
                {
                    let bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
                    for role in &bundle.roles {
                        if let Some(close) = role.pending_inventory_close.as_ref() {
                            inventory
                                .validate_pending_runtime_close(close, &self.ledger, &ledger)
                                .map_err(|_| OperationError::State)?;
                        }
                    }
                }
                let owner = selection.take().ok_or(OperationError::State)?;
                let stored = book
                    .bundles
                    .remove(&owner.id)
                    .expect("validated finite bundle");
                drop(stored);
                let OwnedPreparation {
                    ledger: owner_ledger,
                    id: owner_id,
                    state: owner_state,
                } = owner;
                let mut bundle = match Arc::try_unwrap(owner_state) {
                    Ok(mutex) => mutex.into_inner(),
                    Err(state) => {
                        book.bundles.insert(owner_id, Arc::clone(&state));
                        *selection = Some(OwnedPreparation {
                            ledger: owner_ledger,
                            id: owner_id,
                            state,
                        });
                        return Err(OperationError::State);
                    }
                };
                for role in &bundle.roles {
                    book.bindings.remove(&role.binding);
                    book.closing.remove(&role.binding);
                    book.inventory_overflow.remove(&role.binding);
                }
                for held in &bundle.retained {
                    if let Some(users) = book.retained_users.get_mut(held) {
                        users.remove(&bundle.id);
                        if users.is_empty() {
                            book.retained_users.remove(held);
                        }
                    }
                }
                for family in &bundle.family_ids {
                    if let Some(users) = book.family_users.get_mut(family) {
                        users.remove(&bundle.id);
                        if users.is_empty() {
                            book.family_users.remove(family);
                            book.families.remove(family);
                        }
                    }
                }
                // This destroys the original receipt, manifest, Bundle/OwnedRole Vec
                // capacities, native closures and the final core app-state owner.
                let close =
                    inventory.take_validated_close(&mut bundle.roles[index].inventory, proof);
                // The fixed ordinary path allocates no pending-close vector.
                // Multi-role legacy/planning callers retain their separate scope.
                let mut pending = Vec::new();
                if let Some(fixed) = workspace.as_deref_mut() {
                    fixed.pending = Some(close);
                } else {
                    pending = bundle
                        .roles
                        .iter_mut()
                        .filter_map(|role| role.pending_inventory_close.take())
                        .collect();
                    pending.push(close);
                }
                drop(bundle);
                ledger
                    .entries
                    .get_mut(&binding.reservation)
                    .ok_or(OperationError::State)?
                    .durable = None;
                ledger
                    .release(binding.reservation)
                    .expect("prevalidated locked finite refund");
                if let Some(fixed) = workspace {
                    let close = fixed
                        .pending
                        .take()
                        .expect("prevalidated single finite close");
                    inventory.finish_runtime_close(close, &self.ledger, &ledger);
                } else {
                    for close in pending {
                        inventory.finish_runtime_close(close, &self.ledger, &ledger);
                    }
                }
                Ok(())
            })
            .map_err(|_| OperationError::State)?
    }
}

/// The existing no-root receipt is kept, not inferred from these empty
/// containers. Its original known-unaccepted preparation is checked separately.
fn finite_unaccepted_payload_closed(role: &OwnedRole) -> bool {
    #[cfg(target_os = "linux")]
    if role.copy_vod.is_some() || role.planning_tasks.is_some() || role.planning_metadata.is_some()
    {
        return false;
    }
    role.phase == Phase::Prepared
        && role.handle.is_none()
        && role.transition.is_none()
        && role.finite_vod.is_none()
        && role.response_budget.is_none()
        && role.graph.is_none()
        && role.sealed.is_none()
        && role.sources.is_empty()
        && role.roots.is_empty()
        && role.bootstrap.is_empty()
        && role.scratch.is_empty()
        && role.drains.is_empty()
        && role.closed_drains.is_empty()
        && role.armed.is_empty()
        && role.invoked.is_empty()
        && role.retirement.is_none()
        && role.members.is_empty()
        && role.typed_members.is_none()
        && role
            .receipt
            .as_ref()
            .is_some_and(|receipt| receipt.custody_binding() == role.binding)
}

#[derive(Clone, Copy)]
enum FinalApplicationRole {
    Finite,
    #[cfg(target_os = "linux")]
    PlanningMetadata,
}
impl FinalApplicationRole {
    fn matches(self, bundle: &Bundle, index: usize) -> bool {
        match self {
            Self::Finite => bundle.roles.len() == 1 && index == 0,
            #[cfg(target_os = "linux")]
            Self::PlanningMetadata => {
                bundle.roles.len() == 2
                    && index == 1
                    && bundle.roles[0].refunded
                    && bundle.roles[0].transition.is_none()
                    && bundle.roles[0].pending_finite_close.is_none()
                    && bundle.roles[1].planning_metadata.is_some()
            }
        }
    }
}
