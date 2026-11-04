//! Original application reference custody; never a native close receipt.
use super::*;
use crate::adaptive_resources::runtime::direct_source::application::DirectApplicationClosed;

pub(in crate::adaptive_resources) enum ApplicationReference {
    Tracked(InventoryOwner),
    Overflow { reference: Uuid },
}
impl ApplicationReference {
    pub(in crate::adaptive_resources) fn hold_unknown(&self) {
        if let Self::Tracked(owner) = self {
            // Reconciled under the next census mutation/snapshot. No lock or
            // allocation is permitted while an application is unwinding.
            owner.state.dropped[owner.slot].store(true, Ordering::Release);
        }
    }

    pub(in crate::adaptive_resources) fn identity(&self) -> Uuid {
        match self {
            Self::Tracked(owner) => match owner.identity {
                InventoryIdentity::Original(id) => id,
                _ => unreachable!("application references have original identities"),
            },
            Self::Overflow { reference } => *reference,
        }
    }
}
impl InventoryAdmission<'_> {
    pub(in crate::adaptive_resources) fn register_application_reference(
        &mut self,
        parent: &RuntimeInventoryOwner,
        reference: Uuid,
    ) -> Result<ApplicationReference, InventoryError> {
        let Some(slot) = self.owner_slot(parent)? else {
            return Ok(ApplicationReference::Overflow { reference });
        };
        let parent = self.state.records[slot].ok_or(InventoryError::MissingOwner)?;
        if !matches!(parent.kind, OwnerKind::Producer | OwnerKind::Residency)
            || !matches!(parent.phase, OwnerPhase::Registered | OwnerPhase::Active)
        {
            return Err(InventoryError::InvalidPhase);
        }
        let record = InventoryRecord {
            identity: InventoryIdentity::Original(reference),
            family: parent.family,
            kind: OwnerKind::Reference {
                producer: parent.identity,
            },
            phase: OwnerPhase::Registered,
            physical_binding: None,
        };
        match self.inventory.insert(self.state, record) {
            Ok(owner) => Ok(ApplicationReference::Tracked(owner)),
            Err(InventoryError::Full) => Ok(ApplicationReference::Overflow { reference }),
            Err(error) => Err(error),
        }
    }

    pub(in crate::adaptive_resources) fn finish_application_close(
        &mut self,
        owner: &mut ApplicationReference,
        closed: DirectApplicationClosed,
    ) -> Result<(), InventoryError> {
        if owner.identity() != closed.reference() {
            return Err(InventoryError::ReceiptMismatch);
        }
        match owner {
            ApplicationReference::Overflow { .. } if self.state.overflowed => Ok(()),
            ApplicationReference::Overflow { .. } => Err(InventoryError::MissingOwner),
            ApplicationReference::Tracked(owner) => {
                if !Arc::ptr_eq(&owner.state, &self.inventory.state) {
                    return Err(InventoryError::WrongInventory);
                }
                let record = self.state.records[owner.slot].ok_or(InventoryError::MissingOwner)?;
                if !owner.registered
                    || record.identity != owner.identity
                    || !matches!(record.kind, OwnerKind::Reference { .. })
                    || self.inventory.state.dropped[owner.slot].load(Ordering::Acquire)
                {
                    return Err(InventoryError::MissingOwner);
                }
                owner.registered = false;
                self.inventory.state.dropped[owner.slot].store(false, Ordering::Release);
                self.state.records[owner.slot] = None;
                self.state.changed();
                Ok(())
            }
        }
    }
}
