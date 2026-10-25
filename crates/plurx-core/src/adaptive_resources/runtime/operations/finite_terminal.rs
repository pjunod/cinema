//! Finite-only terminal boundary: actual application aliases and rich bundle
//! allocations leave before the one matching ledger refund. The journal keeps
//! the original receipt and operation across an uncertain completion.
use super::*;

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
        self.finish_application_close(selection, index, FinalApplicationRole::Finite, deadline)
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
            deadline,
        )
        .await
    }

    async fn finish_application_close(
        &self,
        selection: &mut Option<OwnedPreparation>,
        index: usize,
        kind: FinalApplicationRole,
        deadline: Instant,
    ) -> Result<(), OperationError> {
        let owner = selection.as_ref().ok_or(OperationError::State)?;
        self.validate_preparation(owner)?;
        {
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
        self.release_settled_recovered_preparation(owner.id)?;

        // All validation and arithmetic precede the one-shot drop of the
        // rich original bundle. Hold book→ledger locks in their usual order.
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        let mut ledger = self
            .ledger
            .inner
            .lock()
            .map_err(|_| OperationError::State)?;
        let owner = selection.as_ref().ok_or(OperationError::State)?;
        let selected = book.bundles.get(&owner.id).ok_or(OperationError::State)?;
        if !Arc::ptr_eq(selected, &owner.state) {
            return Err(OperationError::State);
        }
        let (binding, durable, effect) = {
            let bundle = owner.state.try_lock().map_err(|_| OperationError::State)?;
            let role = bundle.roles.get(index).ok_or(OperationError::State)?;
            let transition = role.transition.as_ref().ok_or(OperationError::State)?;
            if bundle.id != owner.id
                || !bundle.prepare.acknowledged
                || !kind.matches(&bundle, index)
                || role.refunded
                || role.phase != Phase::Settled
                || role.pending_finite_close.is_some()
                || role
                    .finite_app
                    .as_ref()
                    .is_none_or(|state| !state.released())
                || !matches!(transition.effect, Effect::Settle | Effect::Cancel)
                || (transition.effect == Effect::Settle
                    && role
                        .receipt
                        .as_ref()
                        .is_none_or(|receipt| receipt.custody_binding() != role.binding))
            {
                return Err(OperationError::State);
            }
            (role.binding, role.durable_tag, transition.effect)
        };
        let entry = ledger
            .entries
            .get(&binding.reservation)
            .ok_or(OperationError::State)?;
        if entry.binding != binding
            || entry.durable != Some(durable)
            || ledger.used.checked_sub(entry.resources).is_none()
            || (effect == Effect::Cancel && entry.state == ReservationState::UnknownHeld)
        {
            return Err(OperationError::State);
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
        let bundle = match Arc::try_unwrap(owner_state) {
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
        drop(bundle);
        ledger
            .entries
            .get_mut(&binding.reservation)
            .ok_or(OperationError::State)?
            .durable = None;
        ledger.release(binding.reservation)?;
        Ok(())
    }
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
