//! Ephemeral support owner, not a physical reservation or coverage witness.
use super::*;
use crate::adaptive_resources::finalization_support::SupportIdentity;

pub(in crate::adaptive_resources) struct SupportInventoryOwner(InventoryOwner);

#[derive(Clone)]
pub(in crate::adaptive_resources) struct SupportInventoryMarker {
    state: Arc<SharedInventory>,
    slot: usize,
}
impl SupportInventoryMarker {
    pub(in crate::adaptive_resources) fn mark_unknown(&self) {
        self.state.dropped[self.slot].store(true, Ordering::Release);
    }
}
impl SupportInventoryOwner {
    pub(in crate::adaptive_resources) fn marker(&self) -> SupportInventoryMarker {
        SupportInventoryMarker {
            state: Arc::clone(&self.0.state),
            slot: self.0.slot,
        }
    }
}

impl InventoryAdmission<'_> {
    #[cfg(test)]
    pub(in crate::adaptive_resources) fn exhaust_epoch_for_support_fixture(&mut self) {
        self.state.epoch = u64::MAX;
        self.state.epoch_exhausted = true;
    }

    pub(in crate::adaptive_resources) fn preflight_support_owner(
        &self,
        identity: Uuid,
    ) -> Result<(), InventoryError> {
        if identity.is_nil()
            || self
                .state
                .records
                .iter()
                .flatten()
                .any(|row| row.identity == InventoryIdentity::Original(identity))
        {
            return Err(InventoryError::DuplicateIdentity);
        }
        if self.state.epoch.checked_add(1).is_none() || self.state.epoch_exhausted {
            return Err(InventoryError::Unavailable);
        }
        if !self.state.records.iter().any(Option::is_none) {
            return Err(InventoryError::Full);
        }
        Ok(())
    }

    pub(in crate::adaptive_resources) fn register_support_owner(
        &mut self,
        identity: SupportIdentity,
    ) -> Result<SupportInventoryOwner, InventoryError> {
        self.preflight_support_owner(identity.original)?;
        // with_runtime_mutation reconciled deferred markers on entry. Do not
        // call generic insert here: its second reconciliation could consume
        // the checked census epoch after this transaction's preflight.
        let slot = self
            .state
            .records
            .iter()
            .position(Option::is_none)
            .ok_or(InventoryError::Full)?;
        let record = InventoryRecord {
            identity: InventoryIdentity::Original(identity.original),
            family: OwnerFamily::NativeCleanup,
            kind: OwnerKind::Residency,
            phase: OwnerPhase::Registered,
            physical_binding: None,
        };
        self.inventory.state.dropped[slot].store(false, Ordering::Release);
        self.state.records[slot] = Some(record);
        self.state.changed();
        Ok(SupportInventoryOwner(InventoryOwner {
            state: Arc::clone(&self.inventory.state),
            identity: record.identity,
            slot,
            registered: true,
        }))
    }

    pub(in crate::adaptive_resources) fn support_phase(
        &mut self,
        owner: &SupportInventoryOwner,
        identity: SupportIdentity,
        phase: OwnerPhase,
    ) -> Result<(), InventoryError> {
        let slot = self.support_slot(owner, identity)?;
        if self.state.epoch_exhausted || self.state.epoch.checked_add(1).is_none() {
            return Err(InventoryError::Unavailable);
        }
        let old = self.state.records[slot]
            .as_mut()
            .ok_or(InventoryError::MissingOwner)?;
        if !matches!(phase, OwnerPhase::Active | OwnerPhase::Closing) {
            return Err(InventoryError::InvalidPhase);
        }
        if old.phase == OwnerPhase::UnknownHeld {
            return Err(InventoryError::InvalidPhase);
        }
        old.phase = phase;
        self.state.changed();
        Ok(())
    }

    fn support_slot(
        &self,
        owner: &SupportInventoryOwner,
        identity: SupportIdentity,
    ) -> Result<usize, InventoryError> {
        let token = &owner.0;
        if !Arc::ptr_eq(&token.state, &self.inventory.state) {
            return Err(InventoryError::WrongInventory);
        }
        let record = self.state.records[token.slot].ok_or(InventoryError::MissingOwner)?;
        if !token.registered
            || token.identity != InventoryIdentity::Original(identity.original)
            || record.identity != token.identity
            || record.kind != OwnerKind::Residency
            || record.family != OwnerFamily::NativeCleanup
            || record.physical_binding.is_some()
            || token.state.dropped[token.slot].load(Ordering::Acquire)
        {
            return Err(InventoryError::ReceiptMismatch);
        }
        Ok(token.slot)
    }

    pub(in crate::adaptive_resources) fn preflight_support_close(
        &self,
        owner: &SupportInventoryOwner,
        identity: SupportIdentity,
    ) -> Result<(), InventoryError> {
        self.support_slot(owner, identity)?;
        if self.state.epoch_exhausted || self.state.epoch.checked_add(1).is_none() {
            return Err(InventoryError::Unavailable);
        }
        if self.state.records.iter().flatten().any(|record| {
            matches!(record.kind, OwnerKind::Reference { producer }
                if producer == InventoryIdentity::Original(identity.original))
        }) {
            return Err(InventoryError::ReferencesRemain);
        }
        Ok(())
    }

    pub(in crate::adaptive_resources) fn finish_support_close(
        &mut self,
        owner: &mut SupportInventoryOwner,
        closed: &crate::adaptive_resources::runtime::finalization_support::FinalizationSupportClosed,
    ) -> Result<(), InventoryError> {
        let identity = closed.identity();
        self.preflight_support_close(owner, identity)?;
        let token = &mut owner.0;
        token.registered = false;
        token.state.dropped[token.slot].store(false, Ordering::Release);
        self.state.records[token.slot] = None;
        self.state.changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive_resources::finalization_support::FinalizationSupportProfile;
    use crate::adaptive_resources::{ResourceLedger, ResourceVector};
    use crate::process::observation::Metric;

    #[test]
    fn support_registration_does_not_reconcile_again_after_epoch_preflight() {
        let inventory = OwnerInventory::new();
        let prior = inventory
            .register_original(
                Uuid::new_v4(),
                OwnerFamily::NativeCleanup,
                OwnerKind::Residency,
            )
            .expect("prior original");
        let ledger = ResourceLedger::new(
            1,
            Metric::Known(ResourceVector {
                peak_memory_bytes: 1_000_000,
                ..ResourceVector::default()
            }),
        )
        .expect("ledger");
        ledger
            .observe_capacity(
                Metric::Known(ResourceVector {
                    peak_memory_bytes: 1_000_000,
                    ..ResourceVector::default()
                })
                .into(),
                std::time::Instant::now(),
            )
            .expect("fresh RAM");
        let owner = inventory
            .with_runtime_mutation(|context| {
                context.state.epoch = u64::MAX - 1;
                drop(prior); // Arrives after this context's entry reconciliation.
                let (_, owner) = ledger
                    .reserve_finalization_support(
                        FinalizationSupportProfile::fixture(),
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        1,
                        |identity| {
                            context.register_support_owner(identity).map_err(|_| {
                                crate::adaptive_resources::AdmissionRefusal::Unavailable
                            })
                        },
                    )
                    .expect("one preflighted mutation");
                assert_eq!(context.state.epoch, u64::MAX);
                assert!(
                    !context.state.epoch_exhausted,
                    "no hidden second mutation inside registration"
                );
                assert_eq!(context.state.records.iter().flatten().count(), 2);
                owner
            })
            .expect("context");
        assert_eq!(
            ledger.snapshot().expect("hold").used.peak_memory_bytes,
            64 * 1024
        );
        let snapshot = inventory.snapshot().expect("later deferred reconciliation");
        assert!(snapshot
            .records()
            .any(|record| record.phase == OwnerPhase::UnknownHeld));
        drop(owner);
    }
}
