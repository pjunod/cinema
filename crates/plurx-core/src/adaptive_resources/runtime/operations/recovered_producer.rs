//! Cleanup-only owner for one positively exited fixed software R. The same
//! reopened parent/child descriptors move from import into this role.
use super::super::response_memory::ResponseBudget;
use super::*;

impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) fn install_recovered_r_preparation(
        &self,
        handle: ObligationHandle,
        sequence: i64,
    ) -> Result<OwnedPreparation, OperationError> {
        let authority = handle
            .producer_recovery_authority()
            .ok_or(OperationError::State)?;
        let binding = authority.binding();
        let imported = self
            .recovered_r
            .lock()
            .map_err(|_| OperationError::State)?
            .get(&handle.obligation_id())
            .cloned()
            .ok_or(OperationError::State)?;
        let claim = imported.claim();
        let manifest = claim.manifest().clone();
        let member = claim.scratch_member_id().uuid();
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
                || !matches!(
                    entry.state,
                    ReservationState::UnknownHeld | ReservationState::Reaping
                )
            {
                return Err(OperationError::State);
            }
            entry.resources
        };
        // This cleanup-only budget has no live response slots. Its ceiling is
        // the original full R memory charge and grants no serving capability.
        let budget = ResponseBudget::new(
            usize::try_from(resources.peak_memory_bytes).map_err(|_| OperationError::State)?,
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
            retirement: None,
            response_budget: Some(budget),
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
            phase: Phase::UnknownHeld,
            transition: None,
            receipt: None,
            refunded: false,
        };
        let bundle = Bundle {
            id,
            family_ids: Vec::new(),
            retained: BTreeSet::new(),
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
                    || bound.producer_recovery_authority().is_none_or(|saved| {
                        saved.binding() != binding
                            || saved.adoption_operation_id() != authority.adoption_operation_id()
                    })
                {
                    return Err(OperationError::State);
                }
                existing.clone()
            } else {
                if book.draining || book.bindings.contains_key(&binding) {
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
                held.producer_recovery_authority().is_none_or(|saved| {
                    saved.binding() != binding
                        || saved.adoption_operation_id() != authority.adoption_operation_id()
                })
            })
            || role.sequence != sequence
            || role.refunded
            || role.transition.is_some()
        {
            return Err(OperationError::State);
        }
        if !installed {
            if !role.bootstrap.is_empty() {
                return Err(OperationError::State);
            }
            // Role and cleanup authority exist before consuming the sole
            // native File pair. A validation failure leaves them imported.
            let parts = authority
                .take_native(&imported)
                .map_err(|_| OperationError::State)?;
            role.bootstrap.insert(
                member,
                bootstrap::BootstrapMember::from_recovered_producer(parts),
            );
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
