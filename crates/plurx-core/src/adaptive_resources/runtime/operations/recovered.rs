//! Rebind a positively imported, committed S to the existing response and
//! cleanup owner. No new reservation, historical owner rewrite or refund.
use super::super::response_memory::{FiniteResponseBuffer, ResponseBudget};
use super::*;

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) fn install_recovered_finite_preparation(
        &self,
        handle: ObligationHandle,
        sequence: i64,
    ) -> Result<OwnedPreparation, OperationError> {
        let authority = handle.recovery_authority().ok_or(OperationError::State)?;
        let binding = authority.binding();
        let imported = self
            .recovered_s
            .lock()
            .map_err(|_| OperationError::State)?
            .get(&handle.obligation_id())
            .cloned()
            .ok_or(OperationError::State)?;
        let claim = imported.claim();
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
        let (resources, ledger_state) = {
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
                || !matches!(
                    entry.state,
                    ReservationState::UnknownHeld | ReservationState::Reaping
                )
            {
                return Err(OperationError::State);
            }
            (entry.resources, entry.state)
        };
        // Reconstruct only the finite profile's original two-body response
        // partition. The rest cannot become fresh buffer credit: it also owns
        // the daemon rendition metadata and imported inventory.
        let native_memory = claim
            .plan()
            .limits
            .resident_memory_bytes
            .checked_add(claim.plan().bootstrap().bounds.resident_memory_bytes)
            .and_then(|n| n.checked_add(64 * 1024))
            .and_then(|n| n.checked_add(claim.full().resident_inventory_bytes().ok()?))
            .ok_or(OperationError::State)?;
        let payload_capacity = usize::try_from(claim.plan().limits.one_file_logical_bytes)
            .map_err(|_| OperationError::State)?;
        let response_memory = FiniteResponseBuffer::required_partition_bytes(payload_capacity, 2)
            .ok_or(OperationError::State)?;
        let metadata_memory = resources
            .peak_memory_bytes
            .checked_sub(native_memory)
            .and_then(|n| n.checked_sub(response_memory))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or(OperationError::State)?;
        let app_memory =
            crate::adaptive_resources::runtime::finite_lifetime::requested_heap_bytes()
                .ok_or(OperationError::State)?;
        let metadata_memory = metadata_memory
            .checked_sub(app_memory)
            .filter(|remaining| *remaining > 0)
            .ok_or(OperationError::State)?;
        let response_budget = ResponseBudget::with_metadata(
            usize::try_from(response_memory).map_err(|_| OperationError::State)?,
            metadata_memory,
        )?;
        let role = OwnedRole {
            binding,
            durable_tag: id,
            members: Vec::new(),
            typed_members: Some(manifest.members),
            scratch: BTreeMap::new(),
            bootstrap: BTreeMap::new(),
            sources: BTreeMap::new(),
            finite_vod: None,
            finite_app: Some(
                crate::adaptive_resources::runtime::finite_lifetime::FiniteAppState::new(),
            ),
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
            pending_finite_close: None,
            receipt: None,
            refunded: false,
        };
        let bundle = Bundle {
            id,
            typed_obligation_ids: vec![handle.obligation_id()],
            family_ids: Vec::new(),
            retained: BTreeSet::new(),
            // Recovery has a known adoption, not a fabricated prepare result.
            // installed stays false until the actual Files have moved into
            // this already-registered runtime owner.
            prepare: JournalOperation {
                id: authority.adoption_operation_id(),
                pending: None,
                result: None,
                rejected: None,
                acknowledged: true,
            },
            installed: false,
            roles: vec![role],
        };
        let state = {
            let mut book = self.operations.lock().map_err(|_| OperationError::State)?;
            if let Some(existing) = book.bundles.get(&id) {
                let bound = book.bindings.get(&binding).ok_or(OperationError::State)?;
                if bound.obligation_id() != id
                    || bound.graph_id() != handle.graph_id()
                    || bound.recovery_authority().is_none_or(|saved| {
                        saved.binding() != binding
                            || saved.adoption_operation_id() != authority.adoption_operation_id()
                            || saved.current_owner_id() != authority.current_owner_id()
                    })
                {
                    return Err(OperationError::State);
                }
                existing.clone()
            } else {
                if book.draining
                    || book.bindings.contains_key(&binding)
                    || ledger_state != ReservationState::UnknownHeld
                    || resources.peak_memory_bytes == 0
                {
                    return Err(OperationError::State);
                }
                let state = Arc::new(AsyncMutex::new(bundle));
                book.bindings.insert(binding, handle.clone());
                book.bundles.insert(id, state.clone());
                state
            }
        };
        let owner = OwnedPreparation {
            ledger: self.ledger.id,
            id,
            state: state.clone(),
        };
        let mut bundle = state.try_lock().map_err(|_| OperationError::State)?;
        if bundle.id != id || bundle.roles.len() != 1 {
            return Err(OperationError::State);
        }
        let installed = bundle.installed;
        let role = bundle.roles.get_mut(0).ok_or(OperationError::State)?;
        if role.binding != binding
            || role.durable_tag != id
            || role.handle.as_ref().is_none_or(|held| {
                held.obligation_id() != id
                    || held.graph_id() != handle.graph_id()
                    || held.recovery_authority().is_none_or(|saved| {
                        saved.binding() != binding
                            || saved.adoption_operation_id() != authority.adoption_operation_id()
                            || saved.current_owner_id() != authority.current_owner_id()
                    })
            })
            || role.sequence != sequence
            || role.phase != Phase::Sealed
            || role.refunded
            || role.transition.is_some()
        {
            return Err(OperationError::State);
        }
        if !installed {
            if !role.bootstrap.is_empty() {
                return Err(OperationError::State);
            }
            // The role and its cleanup authority are registered before this
            // sole-file take. A native validation failure keeps both Files in
            // the imported owner, and retries select this exact placeholder.
            let parts = authority
                .take_native(&imported)
                .map_err(|_| OperationError::State)?;
            let bootstrap = bootstrap::BootstrapMember::from_recovered(parts);
            role.bootstrap.insert(member, bootstrap);
        } else if role.bootstrap.len() != 1 || !role.bootstrap.contains_key(&member) {
            return Err(OperationError::State);
        }
        bundle.installed = true;
        bundle.roles[0]
            ._cleanup
            .mark_reaping()
            .map_err(|_| OperationError::State)?;
        Ok(owner)
    }
}
