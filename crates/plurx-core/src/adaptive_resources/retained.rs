//! Single-lock accounting transfer from a retiring durable role to retained
//! storage. This module cannot mint the physical receipt required to commit.
#[cfg(test)]
use super::PhysicalRetirementReceipt;
#[cfg(test)]
use super::ResourceReservation;
use super::{
    AdmissionRefusal, CleanupAuthority, Entry, ReservationBinding, ReservationState,
    ResourceLedger, ResourceVector,
};
use crate::resource_custody::CommittedRetirementReceipt;
use std::collections::BTreeSet;
use uuid::Uuid;

/// Nonclone plan for one conservative R→S accounting replacement.
/// Preparation allocates only a fresh identity; it does not change charges.
pub(in crate::adaptive_resources) struct RetainedSwap {
    ledger: u64,
    source: ReservationBinding,
    source_durable: Uuid,
    source_resources: ResourceVector,
    target: ReservationBinding,
    target_durable: Uuid,
    retained: ResourceVector,
    committed: bool,
}

impl RetainedSwap {
    pub(in crate::adaptive_resources) fn target_binding(&self) -> ReservationBinding {
        self.target
    }

    pub(in crate::adaptive_resources) fn target_durable(&self) -> Uuid {
        self.target_durable
    }
}

impl ResourceLedger {
    /// Prepare a future retained-output replacement without changing used
    /// resources or removing the durable source entry.
    pub(in crate::adaptive_resources) fn prepare_retained_swap(
        &self,
        source: ReservationBinding,
        source_durable: Uuid,
        target_durable: Uuid,
        retained: ResourceVector,
    ) -> Result<RetainedSwap, AdmissionRefusal> {
        if source.ledger != self.id
            || source_durable.is_nil()
            || target_durable.is_nil()
            || source_durable == target_durable
            || retained == ResourceVector::default()
            || retained.cpu_ns_per_second != 0
            || retained.processes != 0
            || retained.hardware_slots != 0
            || retained.tuner_slots != 0
        {
            return Err(AdmissionRefusal::InvalidTransition);
        }

        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        let entry = state
            .entries
            .get(&source.reservation)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.binding != source || entry.durable != Some(source_durable) {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        if state.native_overages.values().any(|overage| {
            overage.binding == source && overage.observed_extra != ResourceVector::default()
        }) {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        if state
            .entries
            .values()
            .any(|entry| entry.durable == Some(target_durable))
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        let source_resources = entry.resources;
        if !componentwise_le(retained, source_resources) {
            return Err(AdmissionRefusal::InvalidTransition);
        }

        let target_id = state.next_reservation;
        let next_id = target_id
            .checked_add(1)
            .ok_or(AdmissionRefusal::IdentityExhausted)?;
        if target_id == 0 || state.entries.contains_key(&target_id) {
            return Err(AdmissionRefusal::IdentityExhausted);
        }
        let target = ReservationBinding {
            ledger: self.id,
            reservation: target_id,
            allocation_epoch: source.allocation_epoch,
        };
        state.next_reservation = next_id;
        Ok(RetainedSwap {
            ledger: self.id,
            source,
            source_durable,
            source_resources,
            target,
            target_durable,
            retained,
            committed: false,
        })
    }

    /// Atomically replace the durable source charge with a no-larger
    /// retained-output charge after the journal returns the original receipt
    /// wrapped in its nonclone known-COMMIT capability.
    pub(in crate::adaptive_resources) fn commit_retained_swap(
        &self,
        swap: &mut RetainedSwap,
        receipt: &CommittedRetirementReceipt,
    ) -> Result<CleanupAuthority, AdmissionRefusal> {
        if swap.ledger != self.id
            || receipt.source_binding() != swap.source
            || receipt.target_binding() != swap.target
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }

        let mut state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if swap.committed {
            let target = state
                .entries
                .get(&swap.target.reservation)
                .ok_or(AdmissionRefusal::UnknownReservation)?;
            if target.binding != swap.target
                || target.durable != Some(swap.target_durable)
                || target.resources != swap.retained
                || target.state != ReservationState::Reaping
                || state.entries.contains_key(&swap.source.reservation)
            {
                return Err(AdmissionRefusal::ReceiptMismatch);
            }
            return Ok(CleanupAuthority {
                ledger: self.clone(),
                binding: swap.target,
            });
        }

        let source = state
            .entries
            .get(&swap.source.reservation)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if source.binding != swap.source
            || source.durable != Some(swap.source_durable)
            || source.resources != swap.source_resources
            || state.entries.contains_key(&swap.target.reservation)
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        if state.native_overages.values().any(|overage| {
            overage.binding == swap.source && overage.observed_extra != ResourceVector::default()
        }) {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        let Some(next_used) = state
            .used
            .checked_sub(swap.source_resources)
            .and_then(|used| used.checked_add(swap.retained))
        else {
            state.invalid = true;
            return Err(AdmissionRefusal::Unavailable);
        };
        let mut next_trial = state.trial.clone();
        if let Some(members) = next_trial.as_mut() {
            if members.remove(&swap.source.reservation) {
                members.insert(swap.target.reservation);
            }
        }

        // All fallible checks are complete. The source removal, target insert,
        // used replacement and trial transfer form one locked state change.
        state.entries.remove(&swap.source.reservation);
        state.entries.insert(
            swap.target.reservation,
            Entry {
                binding: swap.target,
                family_role: None,
                resources: swap.retained,
                state: ReservationState::Reaping,
                custody: None,
                durable: Some(swap.target_durable),
            },
        );
        state
            .native_overages
            .retain(|_, overage| overage.binding != swap.source);
        state.used = next_used;
        state.trial = next_trial;
        swap.committed = true;
        Ok(CleanupAuthority {
            ledger: self.clone(),
            binding: swap.target,
        })
    }
}

fn componentwise_le(left: ResourceVector, right: ResourceVector) -> bool {
    left.cpu_ns_per_second <= right.cpu_ns_per_second
        && left.peak_memory_bytes <= right.peak_memory_bytes
        && left.handles <= right.handles
        && left.processes <= right.processes
        && left.hardware_slots <= right.hardware_slots
        && left.tuner_slots <= right.tuner_slots
        && left.scratch_bytes <= right.scratch_bytes
}

#[cfg(test)]
#[path = "retained/tests.rs"]
mod tests;
