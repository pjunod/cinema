//! Selected runtime custody only. This does not grant adapter completeness.
use super::*;
use crate::adaptive_resources::physical_graph::NativeNeverAcquired;
use crate::resource_custody::CommittedRetirementReceipt;

/// Exhaustion retains an explicit selector under the existing ledger owner.
/// It cannot acquire a census row or erase the sticky overflow uncertainty.
pub(in crate::adaptive_resources) enum RuntimeInventoryOwner {
    Tracked(InventoryOwner),
    Overflow { binding: ReservationBinding },
}

/// Compact original custody survives destruction of the rich application bundle.
/// Only the same mutation guard can retire it after the ledger entry is absent.
pub(in crate::adaptive_resources) struct ValidatedRuntimeClose {
    binding: ReservationBinding,
}

pub(in crate::adaptive_resources) struct PendingRuntimeClose {
    owner: RuntimeInventoryOwner,
    binding: ReservationBinding,
}

impl InventoryAdmission<'_> {
    pub(in crate::adaptive_resources) fn register_selected_runtime(
        &mut self,
        binding: ReservationBinding,
        classification: Option<(OwnerFamily, OwnerKind)>,
        phase: OwnerPhase,
    ) -> Result<RuntimeInventoryOwner, InventoryError> {
        let (family, kind) =
            classification.unwrap_or((OwnerFamily::NativeCleanup, OwnerKind::Producer));
        if matches!(kind, OwnerKind::Reference { .. }) {
            return Err(InventoryError::InvalidKind);
        }
        let record = InventoryRecord {
            identity: InventoryIdentity::Reservation(binding),
            family,
            kind,
            phase: if classification.is_none() {
                OwnerPhase::UnknownHeld
            } else {
                phase
            },
            physical_binding: Some(binding),
        };
        match self.inventory.insert(self.state, record) {
            Ok(owner) => Ok(RuntimeInventoryOwner::Tracked(owner)),
            Err(InventoryError::Full) => Ok(RuntimeInventoryOwner::Overflow { binding }),
            Err(error) => Err(error),
        }
    }

    fn owner_slot(&self, owner: &RuntimeInventoryOwner) -> Result<Option<usize>, InventoryError> {
        match owner {
            RuntimeInventoryOwner::Overflow { .. } if self.state.overflowed => Ok(None),
            RuntimeInventoryOwner::Overflow { .. } => Err(InventoryError::MissingOwner),
            RuntimeInventoryOwner::Tracked(owner) => {
                if !Arc::ptr_eq(&owner.state, &self.inventory.state) {
                    return Err(InventoryError::WrongInventory);
                }
                self.state
                    .records
                    .get(owner.slot)
                    .and_then(Option::as_ref)
                    .filter(|record| owner.registered && record.identity == owner.identity)
                    .map(|_| Some(owner.slot))
                    .ok_or(InventoryError::MissingOwner)
            }
        }
    }

    fn validate_binding(
        &self,
        owner: &RuntimeInventoryOwner,
        binding: ReservationBinding,
    ) -> Result<Option<usize>, InventoryError> {
        let slot = self.owner_slot(owner)?;
        let current = match owner {
            RuntimeInventoryOwner::Tracked(_) => self.state.records[slot.expect("tracked slot")]
                .as_ref()
                .and_then(|r| r.physical_binding),
            RuntimeInventoryOwner::Overflow { binding } => Some(*binding),
        };
        if current != Some(binding) {
            return Err(InventoryError::ReceiptMismatch);
        }
        Ok(slot)
    }

    /// The operation book separately proves retention of an overflow selector.
    /// A dropped original token cannot authorize another entry.
    pub(in crate::adaptive_resources) fn activate_runtime(
        &mut self,
        binding: ReservationBinding,
        retained_overflow: bool,
    ) -> Result<(), InventoryError> {
        let found = self.state.records.iter().position(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.physical_binding == Some(binding))
        });
        let Some(slot) = found else {
            return if self.state.overflowed && retained_overflow {
                Ok(())
            } else {
                Err(InventoryError::MissingOwner)
            };
        };
        if self.inventory.state.dropped[slot].load(Ordering::Acquire) {
            return Err(InventoryError::MissingOwner);
        }
        let record = self.state.records[slot].as_mut().expect("selected row");
        match record.phase {
            OwnerPhase::Registered => {
                record.phase = OwnerPhase::Active;
                self.state.changed();
            }
            OwnerPhase::Active | OwnerPhase::UnknownHeld => {}
            OwnerPhase::Closing => return Err(InventoryError::InvalidPhase),
        }
        Ok(())
    }

    pub(in crate::adaptive_resources) fn closing_runtime(&mut self, binding: ReservationBinding) {
        if let Some(record) = self
            .state
            .records
            .iter_mut()
            .flatten()
            .find(|record| record.physical_binding == Some(binding))
        {
            if matches!(record.phase, OwnerPhase::Registered | OwnerPhase::Active) {
                record.phase = OwnerPhase::Closing;
                self.state.changed();
            }
        }
    }

    pub(in crate::adaptive_resources) fn hold_runtime(&mut self, binding: ReservationBinding) {
        if let Some(record) = self
            .state
            .records
            .iter_mut()
            .flatten()
            .find(|record| record.physical_binding == Some(binding))
        {
            if record.phase != OwnerPhase::UnknownHeld {
                record.phase = OwnerPhase::UnknownHeld;
                self.state.changed();
            }
        }
    }

    pub(in crate::adaptive_resources) fn validate_runtime_close(
        &self,
        owner: &RuntimeInventoryOwner,
        receipt: &PhysicalCloseReceipt,
        ledger: &ResourceLedger,
    ) -> Result<ValidatedRuntimeClose, InventoryError> {
        if ledger.id != receipt.custody_binding().ledger {
            return Err(InventoryError::ReceiptMismatch);
        }
        self.validate_close_binding(owner, receipt.custody_binding())?;
        Ok(ValidatedRuntimeClose {
            binding: receipt.custody_binding(),
        })
    }

    pub(in crate::adaptive_resources) fn validate_never_acquired_close(
        &self,
        owner: &RuntimeInventoryOwner,
        witness: &NativeNeverAcquired,
        ledger: &ResourceLedger,
    ) -> Result<ValidatedRuntimeClose, InventoryError> {
        if ledger.id != witness.binding().ledger {
            return Err(InventoryError::ReceiptMismatch);
        }
        self.validate_close_binding(owner, witness.binding())?;
        Ok(ValidatedRuntimeClose {
            binding: witness.binding(),
        })
    }

    fn validate_close_binding(
        &self,
        owner: &RuntimeInventoryOwner,
        binding: ReservationBinding,
    ) -> Result<(), InventoryError> {
        if let Some(slot) = self.validate_binding(owner, binding)? {
            let identity = self.state.records[slot]
                .as_ref()
                .expect("selected row")
                .identity;
            if self.state.records.iter().flatten().any(|other| matches!(other.kind, OwnerKind::Reference { producer } if producer == identity)) {
                return Err(InventoryError::ReferencesRemain);
            }
        }
        Ok(())
    }

    /// Call only after all fallible validation, including original close proof
    /// and ledger arithmetic. No error may discard this original handoff.
    pub(in crate::adaptive_resources) fn take_validated_close(
        &mut self,
        owner: &mut Option<RuntimeInventoryOwner>,
        proof: ValidatedRuntimeClose,
    ) -> PendingRuntimeClose {
        let binding = proof.binding;
        let owner = owner.take().expect("validated original runtime owner");
        self.validate_close_binding(&owner, binding)
            .expect("same locked validated owner");
        PendingRuntimeClose { owner, binding }
    }

    /// Validate stored original custody before destroying its rich bundle.
    pub(in crate::adaptive_resources) fn validate_pending_runtime_close(
        &self,
        pending: &PendingRuntimeClose,
        ledger: &ResourceLedger,
        state: &super::super::State,
    ) -> Result<(), InventoryError> {
        if ledger.id != pending.binding.ledger {
            return Err(InventoryError::ReceiptMismatch);
        }
        self.validate_close_binding(&pending.owner, pending.binding)?;
        if state.entries.contains_key(&pending.binding.reservation) {
            return Err(InventoryError::NotFinalized);
        }
        Ok(())
    }

    pub(in crate::adaptive_resources) fn finish_runtime_close(
        &mut self,
        pending: PendingRuntimeClose,
        ledger: &ResourceLedger,
        state: &super::super::State,
    ) {
        assert_eq!(
            ledger.id, pending.binding.ledger,
            "same original finalization ledger"
        );
        self.validate_pending_runtime_close(&pending, ledger, state)
            .expect("prevalidated original census and ledger finalization");
        if let RuntimeInventoryOwner::Tracked(mut owner) = pending.owner {
            assert!(
                Arc::ptr_eq(&owner.state, &self.inventory.state),
                "same original finalization census"
            );
            let record = self.state.records[owner.slot]
                .as_ref()
                .expect("same original pending row");
            assert_eq!(record.identity, owner.identity);
            assert_eq!(record.physical_binding, Some(pending.binding));
            owner.registered = false;
            self.inventory.state.dropped[owner.slot].store(false, Ordering::Release);
            self.state.records[owner.slot] = None;
            self.state.changed();
        }
    }

    pub(in crate::adaptive_resources) fn rebind_runtime(
        &mut self,
        owner: &mut RuntimeInventoryOwner,
        receipt: &CommittedRetirementReceipt,
        commit: impl FnOnce() -> Result<super::super::CleanupAuthority, super::super::AdmissionRefusal>,
    ) -> Result<super::super::CleanupAuthority, super::super::AdmissionRefusal> {
        let slot = self
            .validate_binding(owner, receipt.source_binding())
            .map_err(|_| super::super::AdmissionRefusal::ReceiptMismatch)?;
        let cleanup = commit()?;
        match owner {
            RuntimeInventoryOwner::Tracked(_) => {
                let record = self.state.records[slot.expect("tracked row")]
                    .as_mut()
                    .expect("same original row");
                record.physical_binding = Some(receipt.target_binding());
                record.family = OwnerFamily::RetainedResidency;
                record.kind = OwnerKind::Residency;
                if record.phase != OwnerPhase::UnknownHeld {
                    record.phase = OwnerPhase::Active;
                }
                self.state.changed();
            }
            RuntimeInventoryOwner::Overflow { binding } => *binding = receipt.target_binding(),
        }
        Ok(cleanup)
    }
}
