//! Rebind a positively imported, committed S to the existing response and
//! cleanup owner. No new reservation, historical owner rewrite or refund.
use super::super::response_memory::ResponseBudget;
use super::*;

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) fn install_recovered_finite_preparation(
        &self,
        handle: ObligationHandle,
        sequence: i64,
    ) -> Result<OwnedPreparation, OperationError> {
        let authority = handle.recovery_authority().ok_or(OperationError::State)?;
        let binding = authority.binding();
        let claim = authority.claim();
        let manifest = claim.manifest().clone();
        let member = claim.member_id().uuid();
        let id = handle.obligation_id();
        if authority.current_owner_id() != self.owner.owner_id
            || claim.sequence() != sequence
            || claim.obligation_id() != id
            || claim.graph_id() != handle.graph_id()
            || self.recovered_bindings.get(&id) != Some(&binding)
        {
            return Err(OperationError::State);
        }
        let resources = {
            let state = self
                .ledger
                .inner
                .lock()
                .map_err(|_| OperationError::State)?;
            let entry = state
                .entries
                .get(&binding.reservation)
                .ok_or(OperationError::State)?;
            if entry.binding != binding
                || entry.durable != Some(id)
                || entry.resources != claim.resources()
                || entry.state != ReservationState::UnknownHeld
            {
                return Err(OperationError::State);
            }
            entry.resources
        };
        {
            let book = self.operations.lock().map_err(|_| OperationError::State)?;
            if book.draining
                || book.bundles.contains_key(&id)
                || book.bindings.contains_key(&binding)
            {
                return Err(OperationError::State);
            }
        }
        // The original S retains the whole nonproducer allowance. Native
        // metadata/bootstrap stay in their recorded partitions; body buffers
        // use only the remaining already-charged memory, never fresh credit.
        let native_memory = claim
            .plan()
            .limits
            .resident_memory_bytes
            .checked_add(claim.plan().bootstrap().bounds.resident_memory_bytes)
            .and_then(|n| n.checked_add(64 * 1024))
            .ok_or(OperationError::State)?;
        let response_memory = resources
            .peak_memory_bytes
            .checked_sub(native_memory)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(OperationError::State)?;
        let response_budget = ResponseBudget::new(response_memory)?;
        let parts = authority.take_native().map_err(|_| OperationError::State)?;
        // Infallible same-File move. The native importer/claim checked every
        // constructor invariant before either the fold or Files left its map.
        let bootstrap = bootstrap::BootstrapMember::from_recovered(parts);
        let role = OwnedRole {
            binding,
            durable_tag: id,
            members: Vec::new(),
            typed_members: Some(manifest.members),
            scratch: BTreeMap::new(),
            bootstrap: BTreeMap::from([(member, bootstrap)]),
            sources: BTreeMap::new(),
            finite_vod: None,
            retirement: None,
            response_budget: Some(response_budget),
            armed: BTreeSet::from([member]),
            invoked: BTreeSet::from([member]),
            roots: BTreeMap::new(),
            drains: BTreeMap::new(),
            closed_drains: BTreeMap::new(),
            graph: None,
            sealed: None,
            _cleanup: CleanupAuthority {
                ledger: self.ledger.clone(),
                binding,
            },
            handle: Some(handle.clone()),
            sequence,
            phase: Phase::Sealed,
            transition: None,
            receipt: None,
            refunded: false,
        };
        let bundle = Bundle {
            id,
            family_ids: Vec::new(),
            retained: BTreeSet::new(),
            // Recovery has a known adoption, not a fabricated prepare result.
            // installed=true means resolve_preparation never consumes this slot.
            prepare: JournalOperation {
                id: authority.adoption_operation_id(),
                pending: None,
                result: None,
                rejected: None,
                acknowledged: true,
            },
            installed: true,
            roles: vec![role],
        };
        let state = Arc::new(AsyncMutex::new(bundle));
        let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
        book.bindings.insert(binding, handle);
        book.bundles.insert(id, state.clone());
        Ok(OwnedPreparation {
            ledger: self.ledger.id,
            id,
            state,
        })
    }
}
